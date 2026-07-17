//! Share plumbing: the Save As dialog, the in-place reload after an export,
//! and the background bake that commits pending edits before Save/Copy.
//! Split from `preview/mod.rs` (DRAGON-115) — pure code motion.

use super::*;


impl App {
    /// Open the native Save As file chooser, then route the pick to `SaveAsResult`.
    ///
    /// Only a fullscreen OVERLAY is torn down first: it's a layer-shell surface with an
    /// exclusive keyboard grab, so the file chooser would render behind it and be
    /// unusable. A cancelled dialog re-mints the overlay on the still-loaded capture
    /// ([`Self::reopen_preview_surface`], DRAGON-157). A normal WINDOW can show the
    /// chooser over itself, so it stays open.
    pub(super) fn save_as_dialog(&mut self) -> Task<cosmic::Action<Msg>> {
        self.stop_preview_playback();
        let name = self
            .preview
            .as_ref()
            .and_then(|p| p.path.as_ref())
            .and_then(|path| path.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "capture".to_string());
        let hide = match self.preview.as_ref() {
            Some(p) if !p.surface.is_window() => p.surface.close(p.window),
            _ => Task::none(),
        };
        let pick = Task::perform(super::pick_save_path(name), |opt| {
            cosmic::Action::App(Msg::Preview(PreviewMsg::SaveAsResult(opt)))
        });
        Task::batch([hide, pick])
    }

    /// Reload the preview on `path` IN PLACE, keeping the current window/surface (used
    /// by a keep-open Save As that committed edits into the working file itself, where
    /// the stale edit state must not double-apply). Resets the edit state and
    /// re-decodes the new file as the working document.
    pub(super) fn reload_preview_in_place(
        &mut self,
        path: PathBuf,
        size: u64,
        is_video: bool,
    ) -> Task<cosmic::Action<Msg>> {
        let (kind, task) = if is_video {
            (PreviewKind::Video(VideoPreview::loading()), video::poster_task(path.clone()))
        } else {
            (PreviewKind::Image(ImagePreview::loading()), image::decode_task(path.clone()))
        };
        if let Some(p) = &mut self.preview {
            p.path = Some(path);
            p.size = Some(size);
            p.kind = kind;
            p.edit = EditState::default();
            p.view = Viewport::default();
            // The new file is its own thing — no captured footprint to target.
            p.display_dims = None;
        }
        task
    }

    /// Re-mint the preview surface for the ALREADY-LOADED capture and re-point the
    /// existing [`PreviewState`] at it — the same re-pointing as
    /// `toggle_preview_appearance`, minus the close (the surface is already gone) and
    /// minus the appearance flip. Used when a fullscreen overlay had to close for the
    /// Save As dialog and the dialog was CANCELLED: the capture and every edit are
    /// still in memory, so the overlay comes back instead of the session exiting
    /// (DRAGON-157). Falls back to ending the session when no preview state exists.
    pub(super) fn reopen_preview_surface(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview.as_ref() else {
            return self.finish_session();
        };
        let external = p.external;
        let fallback_monitor = p.monitor;
        // `--preview` files anchor to the active output (None); in-app captures to the
        // capture monitor — the same anchoring as `toggle_preview_appearance`.
        let (output, monitor) = if external {
            (None, fallback_monitor)
        } else {
            match self.preview_output.clone() {
                Some((o, m)) => (Some(o), m),
                None => (None, fallback_monitor),
            }
        };
        let extra_h = self
            .preview
            .as_ref()
            .map(|p| transport_h_for(&p.kind, PreviewSurface::Window))
            .unwrap_or(0.0);
        let (new_id, open_task, new_monitor, surface) =
            self.preview_surface_for(output, monitor, None, extra_h);
        if let Some(p) = self.preview.as_mut() {
            p.window = new_id;
            p.monitor = new_monitor;
            p.surface = surface;
            // A freshly-minted window carries the transient max_size hint again.
            p.max_hint_pending = surface.is_window();
        }
        open_task
    }

    /// After a Save / Copy: close the preview editor (the historical behaviour) when
    /// `auto_close_preview` is on, otherwise keep it open so the user can keep working
    /// on the capture. Always stops inline playback + resumes ducked media first.
    pub(super) fn finish_or_keep_preview(&mut self) -> Task<cosmic::Action<Msg>> {
        self.stop_preview_playback();
        if self.auto_close_preview {
            self.finish_session()
        } else {
            Task::none()
        }
    }

    /// Kick off a bake before running `intent`, or `None` when none is needed (no
    /// covermark and no deleted timeline content, no path, or a video without probed
    /// dims).
    ///
    /// The bake runs BEHIND THE SCENES: the preview surface closes immediately (so the
    /// user isn't stuck staring at a spinner) and a persistent "Processing capture"
    /// notification spans the re-encode. Save/Save As write the capture in place; Copy
    /// writes a throwaway TEMP and copies that, so copying never persists edits to the
    /// saved file.
    pub(super) fn begin_bake(&mut self, intent: ShareIntent) -> Option<Task<cosmic::Action<Msg>>> {
        // When the editor stays open after a share, DON'T tear the surface down for the
        // bake — it re-renders in place and BakeDone reloads the committed result.
        let keep_open = !self.auto_close_preview;
        let p = self.preview.as_mut()?;
        if !p.dirty() || p.edit.baking {
            return None;
        }
        let src = p.path.clone()?;
        let covermark = p.edit.covermark.clone();
        let video = match &p.kind {
            PreviewKind::Image(_) => None,
            // A video bake needs the probed metadata (overlay raster size, audio
            // presence for the cut graph); without it (ffprobe failed) there's
            // nothing sane to do, so share unedited.
            PreviewKind::Video(vid) => {
                let m = vid.meta?;
                Some(edit::VideoBake {
                    w: m.w,
                    h: m.h,
                    has_audio: m.has_audio,
                    // Kept spans only when content was DELETED — an uncut (or
                    // merely razor-split) timeline keeps the historical
                    // covermark/metadata-only ffmpeg invocations.
                    keep: vid
                        .timeline
                        .as_ref()
                        .filter(|t| t.edited())
                        .map(|t| t.spans.clone()),
                })
            }
        };
        // Copy targets a temp (leave the saved file clean); Save/Save As write in place.
        let dst = if intent == ShareIntent::Copy {
            let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("png");
            std::path::PathBuf::from(crate::util::runtime_dir()).join(format!("cck-copy.{ext}"))
        } else {
            src.clone()
        };
        p.edit.baking = true;
        p.edit.pending = Some(intent);
        p.edit.pending_output = Some(dst.clone());
        // Vanish the overlay right away — the work continues in the background — UNLESS
        // the editor is staying open, in which case it re-renders in place.
        let hide = if keep_open { Task::none() } else { p.surface.close(p.window) };
        let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let result = crate::share::with_processing_notification(|| match &video {
                Some(v) => edit::bake_video(&src, &dst, covermark.as_ref(), v),
                None => edit::bake_image(&src, &dst, covermark.as_ref()),
            });
            // Log the real io::Error here — it's about to be discarded to an Option
            // (BakeDone's eventual log::warn! has no error left to report).
            if let Err(e) = &result {
                log::warn!("preview edit bake failed: {e}");
            }
            let _ = tx.send(result.ok());
        });
        let bake = Task::perform(rx, |res| {
            cosmic::Action::App(Msg::Preview(PreviewMsg::BakeDone(res.ok().flatten())))
        });
        Some(Task::batch([hide, bake]))
    }
}
