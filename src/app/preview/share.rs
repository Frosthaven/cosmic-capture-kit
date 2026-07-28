//! Share plumbing: the Save As dialog, the in-place reload after an export, the
//! background bake that commits pending edits, and the ONE completion seam every share
//! action lands on.
//!
//! # The DRAGON-353 model
//!
//! Every action here is expressed as a [`ShareIntent`], and every one of them completes in
//! [`App::finish_share_intent`] — whether a bake ran (asynchronously, via `BakeDone`) or
//! not (synchronously, when the document was clean). That single funnel is what keeps
//! "save writes the `-edited` sibling", "copy leaves the saved file untouched", "delete
//! copies first when the setting says so" and "only the settings-driven flavours close the
//! document" from drifting apart across four call sites.
//!
//! Two rules the funnel enforces that used to be scattered:
//!
//! * **A share NEVER closes the editor by itself.** The auto-close setting is gone; only
//!   [`ShareIntent::closes_document`] (the two settings-driven flavours) and an explicit
//!   `close_after_share` (the unsaved-changes dialog's action buttons) close anything.
//! * **Feedback is a per-document TOAST**, not a desktop notification. The processing
//!   notification is gone too — the editor stays up and shows its own spinner instead
//!   (see `PREVIEW_PROCESSING_MESSAGES`).

use super::*;

impl App {
    /// Post a toast on `id`'s document. A no-op for a document that has already closed, so
    /// a late async completion can never resurrect state.
    pub(super) fn preview_toast(
        &mut self,
        id: window::Id,
        kind: ToastKind,
        text: impl Into<String>,
    ) {
        if let Some(p) = self.preview_for_mut(id) {
            p.toasts.push(kind, text);
        }
    }

    /// [`Self::preview_toast`] carrying an explicit per-toast icon (DRAGON-357) — the outcome's
    /// own glyph (copied / saved / deleted, and their failures) instead of the severity default.
    pub(super) fn preview_toast_icon(
        &mut self,
        id: window::Id,
        kind: ToastKind,
        text: impl Into<String>,
        icon: &'static str,
    ) {
        if let Some(p) = self.preview_for_mut(id) {
            p.toasts.push_icon(kind, text, icon);
        }
    }

    /// The user-facing name of a path — what a toast says instead of a full path (which
    /// would wrap the card in a fullscreen overlay).
    fn display_name(path: &std::path::Path) -> String {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    }

    /// Put `path` on the clipboard and TOAST the outcome. Returns what was reported.
    ///
    /// See [`crate::share::copy_to_clipboard`]'s doc for what "success" can mean here: on
    /// Linux the selection is served by a detached worker, so a `true` is "handed to a
    /// worker", never a verified round-trip. The toast wording ("Copied to clipboard")
    /// matches what the app can honestly claim; a `false` is a real failure (no worker
    /// could be launched at all) and reads as one.
    pub(super) fn copy_to_clipboard_now(
        &mut self,
        id: window::Id,
        path: &std::path::Path,
        is_video: bool,
    ) -> bool {
        let ok = crate::platform::services::copy_to_clipboard(path, is_video);
        if ok {
            self.preview_toast_icon(id, ToastKind::Success, "Copied to clipboard", "clipboard-check-symbolic");
        } else {
            self.preview_toast_icon(id, ToastKind::Error, "Couldn't copy to clipboard", "clipboard-x-symbolic");
        }
        ok
    }

    /// Copy `src` to a throwaway temp beside the other runtime files and return it, so the
    /// clipboard can be served from a file that OUTLIVES `src`.
    ///
    /// This exists for the delete-with-copy path. The Linux clipboard worker is handed a
    /// PATH, not bytes: it reads the file (a still) or advertises its `file://` URI (a
    /// recording) from a detached child, so unlinking the original right after spawning it
    /// is a race at best and a dead URI at worst. Staging first removes the question —
    /// the same trick the edited-copy bake already uses.
    fn stage_clipboard_copy(src: &std::path::Path) -> Option<PathBuf> {
        let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("png");
        let temp = PathBuf::from(crate::util::runtime_dir()).join(format!("cck-copy.{ext}"));
        match std::fs::copy(src, &temp) {
            Ok(_) => Some(temp),
            Err(e) => {
                log::warn!("preview: could not stage a clipboard copy of {}: {e}", src.display());
                None
            }
        }
    }

    /// The AUTOMATIC clipboard copy a preview editor performs as it opens (DRAGON-353 —
    /// the "Automatically copy to clipboard" setting became unconditional behaviour, and
    /// this is where it landed). Idempotent per document: the path can arrive after the
    /// surface (a pre-opened spinner), so several seams call this and only the first one
    /// with a path does anything.
    ///
    /// * A `--preview` file is NOT copied: it is the user's own file, opened as a viewer,
    ///   and silently hijacking their clipboard for it was never asked for.
    /// * Over the clipboard SIZE LIMIT ([`crate::share::AUTO_COPY_MAX_BYTES`], a fixed
    ///   constant since DRAGON-353 removed the setting) it is skipped with an error toast
    ///   naming the limit — that toast is why the knob was no longer needed.
    /// * It never saves and never closes, whatever the "Automatically save on copy" /
    ///   "Automatically close on copy" settings say: those are about the user's Copy ACTION.
    ///   An editor that shut itself the instant it opened would be unusable.
    pub(super) fn auto_copy_preview_on_open(&mut self, id: window::Id) {
        let Some(p) = self.preview_for(id) else { return };
        if p.copied_on_open || p.external {
            return;
        }
        let Some(path) = p.path.clone() else { return };
        let size = p.size.unwrap_or(0);
        let is_video = matches!(p.kind, PreviewKind::Video(_));
        if let Some(p) = self.preview_for_mut(id) {
            p.copied_on_open = true;
        }
        if size > crate::platform::services::AUTO_COPY_MAX_BYTES {
            // The limit is a fixed constant since DRAGON-353 (the "Clipboard size limit"
            // setting is gone), so this toast is the ONLY place it is visible — it names
            // the number rather than a vague "too large".
            let limit = crate::platform::services::auto_copy_limit_label();
            self.preview_toast_icon(
                id,
                ToastKind::Error,
                format!("Too large to copy automatically (over {limit})"),
                "clipboard-x-symbolic",
            );
            return;
        }
        self.copy_to_clipboard_now(id, &path, is_video);
    }

    /// Run one of the unsaved-changes dialog's ACTION buttons: dismiss the dialog, arm
    /// "close when this lands", and dispatch the ordinary action message.
    ///
    /// The close is armed as STATE rather than threaded through the action, so Save,
    /// Save As and Copy each keep exactly one implementation — including their
    /// asynchronous halves (`BakeDone`, `SaveAsBaked`), which read `close_after_share` at
    /// completion. A failed action deliberately leaves the document OPEN: closing after
    /// "save" that did not save would be the data loss the dialog exists to prevent.
    pub(super) fn share_then_close(
        &mut self,
        id: window::Id,
        action: PreviewMsg,
    ) -> Task<cosmic::Action<Msg>> {
        if let Some(p) = self.preview_for_mut(id) {
            // A retry after a failure starts clean: the stale reason goes, and `baking` /
            // `pending` / `pending_output` were already cleared by the completion that
            // reported the failure, so nothing here can wedge a second attempt.
            p.edit.begin_close_action();
        }
        self.update_preview(id, action)
    }

    /// A dialog-initiated action FAILED (DRAGON-353 follow-up): keep the document, re-raise
    /// the unsaved-changes dialog and give it the real reason.
    ///
    /// ONE path for all four dialog actions — a failed Save, Save As, Copy or Delete lands
    /// here and produces the same card, so the user always gets the same three ways out
    /// (retry, Exit anyway, Continue editing) whatever went wrong. The close intent is
    /// DISARMED: the whole point is that we are not closing, and leaving it armed would let
    /// an unrelated later completion close on the back of a failure.
    ///
    /// A no-op when the action did NOT come from the dialog — a toolbar action's failure is
    /// reported by its toast and the editor simply stays up, which is already the ruling.
    pub(super) fn fail_close_action(&mut self, id: window::Id, reason: impl Into<String>) {
        if let Some(p) = self.preview_for_mut(id) {
            p.edit.note_action_failure(reason);
        }
    }

    /// Open the native Save As file chooser, then route the pick to `SaveAsResult`.
    ///
    /// Only a fullscreen OVERLAY is torn down first: it's a layer-shell surface with an
    /// exclusive keyboard grab, so the file chooser would render behind it and be
    /// unusable. A cancelled dialog re-mints the overlay on the still-loaded capture
    /// ([`Self::reopen_preview_surface`], DRAGON-157). A normal WINDOW can show the
    /// chooser over itself, so it stays open.
    pub(super) fn save_as_dialog(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        self.stop_preview_playback(id);
        let name = self.preview_for(id)
            .and_then(|p| p.path.as_ref())
            .and_then(|path| path.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "capture".to_string());
        let hide = match self.preview_for(id) {
            Some(p) if !p.surface.is_window() => {
                let close = p.surface.close(p.window);
                // The document stays loaded but its surface is gone until the dialog
                // resolves — record that so a later close doesn't double-destroy it.
                if let Some(p) = self.preview_for_mut(id) {
                    p.surface_open = false;
                }
                close
            }
            _ => Task::none(),
        };
        let pick = Task::perform(super::pick_save_path(name), move |opt| {
            cosmic::Action::App(Msg::Preview(id, PreviewMsg::SaveAsResult(opt)))
        });
        Task::batch([hide, pick])
    }

    // DRAGON-353 follow-up: `reload_preview_in_place` lived here — after a Save (or a Save
    // As export) it re-decoded the file the bake had just written and RESET the edit state,
    // so the committed pixels became the new baseline and nothing double-applied. That is
    // also what threw the undo history away on every save, and it is gone.
    //
    // The editor is non-destructive instead: `path` stays pinned to the untouched media,
    // the scene stays live on top of it, and a save only moves the bookkeeping
    // (`saved_path` / `size` / `save_in_place` / `edit.mark_saved`). The displayed result
    // is identical — base + scene IS what the save wrote — but Ctrl+Z still works
    // afterwards. Do not reintroduce a reload-on-save: it would resurrect exactly this bug.

    /// Re-mint the preview surface for the ALREADY-LOADED capture and re-point the
    /// existing [`PreviewState`] at it — the same re-pointing as
    /// `toggle_preview_appearance`, minus the close (the surface is already gone) and
    /// minus the appearance flip. Used when a fullscreen overlay had to close for the
    /// Save As dialog and the dialog was CANCELLED: the capture and every edit are
    /// still in memory, so the overlay comes back instead of the session exiting
    /// (DRAGON-157). Falls back to ending the session when no preview state exists.
    pub(super) fn reopen_preview_surface(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for(id) else {
            // The document vanished while its dialog was up: close it (which ends the
            // process only if nothing else is open — DRAGON-336 phase 2).
            return self.close_preview(id);
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
        let extra_h = self.preview_for(id)
            .map(|p| transport_h_for(&p.kind, PreviewSurface::Window))
            .unwrap_or(0.0);
        let (new_id, open_task, new_monitor, surface) =
            self.preview_surface_for(Some(id), output, monitor, None, extra_h);
        self.repoint_preview(id, new_id, surface, new_monitor);
        open_task
    }

    /// WHERE a Save writes: [`naming::save_target`] over the document's CURRENT save
    /// identity and its `save_in_place` bit. `None` when the document has no file at all.
    ///
    /// The identity is `saved_path` once the document has saved once, and the capture's own
    /// `path` before that (DRAGON-353 follow-up — `path` stays pinned to the media, so the
    /// naming rule reads the save side explicitly rather than assuming the two are the
    /// same). The behaviour is unchanged: first dirty save → the `-edited` sibling; every
    /// save after that → straight back to it.
    pub(super) fn preview_save_target(&self, id: window::Id) -> Option<PathBuf> {
        let p = self.preview_for(id)?;
        let current = p.saved_path.as_ref().or(p.path.as_ref())?;
        Some(naming::save_target(current, p.save_in_place, &naming::on_disk))
    }

    /// The file this document IS on disk right now — its last save if it has one, else the
    /// capture it opened with. What a Copy puts on the clipboard and what the toasts name.
    pub(super) fn preview_current_file(&self, id: window::Id) -> Option<PathBuf> {
        let p = self.preview_for(id)?;
        p.saved_path.clone().or_else(|| p.path.clone())
    }

    /// THE share entry point (DRAGON-353): run `intent` against `id`, baking first when
    /// there are edits to commit. Every action bar button, hotkey and unsaved-changes
    /// dialog button routes through here, so the bake-vs-no-bake fork exists once.
    pub(super) fn run_share(
        &mut self,
        id: window::Id,
        intent: ShareIntent,
    ) -> Task<cosmic::Action<Msg>> {
        match self.begin_bake(id, intent) {
            // A bake is in flight; `BakeDone` calls `finish_share_intent` with its output.
            Some(task) => task,
            // Nothing to commit — complete the intent right now against the file as it is.
            None => self.finish_share_intent(id, intent, None),
        }
    }

    /// Kick off a bake before running `intent`, or `None` when none is needed (no pending
    /// edits, no path, or a video without probed dims).
    ///
    /// DRAGON-353: the bake now runs with the EDITOR STILL UP. It used to vanish the
    /// surface and span the re-encode with a desktop "Processing capture" notification;
    /// instead the editor draws its own processing overlay (the same spinner the load
    /// state uses) while `edit.baking` holds every input. Copy flavours still bake to a
    /// throwaway TEMP so the saved file stays clean; saving flavours bake to the
    /// document's SAVE TARGET, which for a fresh capture is the `-edited` sibling rather
    /// than the capture itself.
    pub(super) fn begin_bake(&mut self, id: window::Id, intent: ShareIntent) -> Option<Task<cosmic::Action<Msg>>> {
        // The save target reads `self`, so resolve it before borrowing the preview.
        let save_target = self.preview_save_target(id);
        let processing_msg = random_processing_msg();
        let p = self.preview_for_mut(id)?;
        // WHEN a bake is owed. A copy flavour bakes to a throwaway temp that does not exist
        // yet, so any scene content needs one (this is what makes copy-on-delete put the
        // EDITED picture on the clipboard, DRAGON-352). A SAVING flavour writes the
        // document's save target — which, since the history now survives a save (DRAGON-353
        // follow-up), may ALREADY hold exactly this scene. Re-encoding identical pixels
        // would churn the file's mtime and (for a lossy format) its quality, so a save that
        // is standing on its own save point is the clean-save no-op instead, toast and all.
        // A plain Delete (no copy, no save) never bakes: baking output nobody reads only to
        // unlink the source would be pure waste — the user is discarding the file. The whole
        // decision is `ShareIntent::owes_bake` (pure, unit-tested there).
        let owed = intent.owes_bake(p.dirty(), p.unsaved());
        if !owed || p.edit.baking {
            return None;
        }
        // The bake ALWAYS reads the untouched media (`path`), never the last save: the
        // saved file already has the scene burned in, and baking it again would double
        // every annotation. This is the other half of the non-destructive model.
        let src = p.path.clone()?;
        let covermark = p.edit.covermark.clone();
        // Annotations are IMAGES only (a video preview never accumulates them).
        let annotations = p.edit.annotations.clone();
        let annot_curve = p.edit.curve_radius();
        let dim = p.edit.dim;
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
        // Copy flavours target a throwaway temp (leave the saved file clean); saving
        // flavours write the document's save target.
        let dst = if intent.bakes_to_temp() {
            let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("png");
            PathBuf::from(crate::util::runtime_dir()).join(format!("cck-copy.{ext}"))
        } else {
            save_target.clone().unwrap_or_else(|| src.clone())
        };
        p.edit.baking = true;
        p.edit.pending = Some(intent);
        p.edit.pending_output = Some(dst.clone());
        p.edit.processing_msg = processing_msg;
        let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            // DRAGON-353: no desktop "Processing capture" notification — the editor is
            // still on screen showing its own spinner over the picture.
            let result = match &video {
                Some(v) => edit::bake_video(&src, &dst, covermark.as_ref(), v),
                None => {
                    edit::bake_image(&src, &dst, covermark.as_ref(), &annotations, annot_curve, dim)
                }
            };
            // Log the real io::Error here — it's about to be discarded to an Option
            // (BakeDone's eventual log::warn! has no error left to report).
            if let Err(e) = &result {
                log::warn!("preview edit bake failed: {e}");
            }
            let _ = tx.send(result.ok());
        });
        Some(Task::perform(rx, move |res| {
            cosmic::Action::App(Msg::Preview(id, PreviewMsg::BakeDone(res.ok().flatten())))
        }))
    }

    /// THE completion seam every share action lands on — after a bake (`baked` = the file
    /// the bake wrote) or straight away when the document was clean (`baked` = `None`).
    ///
    /// Order matters and is deliberate: **save → copy → delete → close**. The copy reads
    /// whatever the save just produced (so "save & close on copy" puts the SAVED bytes on
    /// the clipboard, not a second re-encode), and the delete happens only after the
    /// clipboard has been served from a file that outlives it.
    pub(super) fn finish_share_intent(
        &mut self,
        id: window::Id,
        intent: ShareIntent,
        baked: Option<PathBuf>,
    ) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for(id) else { return Task::none() };
        let is_video = matches!(p.kind, PreviewKind::Video(_));
        let external = p.external;
        let current = self.preview_current_file(id);
        // The dialog's action buttons ask for a close once the action lands. Read, NOT
        // taken: a failure below hands the flag to `fail_close_action`, which needs to know
        // the request came from the dialog in order to re-raise it. It is cleared on the
        // way out of every path that does not fail.
        let from_dialog = self.preview_for(id).is_some_and(|p| p.edit.close_after_share);
        let close_after = from_dialog || intent.closes_document();

        let mut tasks: Vec<Task<cosmic::Action<Msg>>> = Vec::new();

        // ── 1. SAVE ───────────────────────────────────────────────────────────────────
        if intent.saves() {
            match &baked {
                // The bake wrote the save target: ADOPT it as the working document.
                Some(dest) => {
                    let size = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
                    self.stop_preview_playback(id);
                    crate::platform::services::notify(dest, false);
                    self.preview_toast_icon(
                        id,
                        ToastKind::Success,
                        format!("Saved {}", Self::display_name(dest)),
                        "save-check-symbolic",
                    );
                    // RETARGET, never reload (DRAGON-353 follow-up). The old flow re-decoded
                    // the baked file and reset the edit state so nothing double-applied —
                    // and took the whole undo history with it. The document now keeps
                    // rendering the untouched media plus its live scene (which looks
                    // identical to the file it just wrote), so every edit stays undoable
                    // across the save; only the save-side bookkeeping moves. `mark_saved`
                    // pins the history position the file corresponds to, which is what makes
                    // `unsaved()` go false now and true again if the user undoes past it.
                    if let Some(p) = self.preview_for_mut(id) {
                        p.saved_path = Some(dest.clone());
                        p.size = Some(size);
                        p.save_in_place = true;
                        p.note_written(dest);
                        p.edit.mark_saved();
                    }
                }
                // Nothing to commit. A clean Save is a NO-OP with a toast, not a rewrite:
                // re-encoding identical pixels would only churn the file's mtime and (for
                // a lossy format) its quality. DRAGON-353's clean-save ruling.
                None => {
                    let msg = if external {
                        "No changes to save".to_string()
                    } else {
                        match &current {
                            Some(path) => {
                                format!("Already saved as {}", Self::display_name(path))
                            }
                            None => "Nothing to save yet".to_string(),
                        }
                    };
                    self.preview_toast_icon(id, ToastKind::Success, msg, "save-check-symbolic");
                }
            }
        }

        // ── 2. COPY ───────────────────────────────────────────────────────────────────
        if intent.copies() {
            // The document's file as it stands NOW: a save in step 1 may have retargeted it.
            let path = self.preview_current_file(id);
            let source = match (&baked, intent) {
                // A save-flavoured copy puts the SAVED file on the clipboard.
                (_, i) if i.saves() => path.clone(),
                // A copy-flavoured bake wrote the throwaway temp — copy that.
                (Some(temp), _) => Some(temp.clone()),
                // Nothing baked. A plain Copy serves the file directly (it isn't going
                // anywhere); a DELETE-flavoured one must serve a STAGED temp, because the
                // clipboard worker is handed a path and the original is about to be
                // unlinked out from under it.
                (None, i) if i.deletes() => {
                    path.as_deref().and_then(Self::stage_clipboard_copy).or_else(|| path.clone())
                }
                (None, _) => path.clone(),
            };
            let copy_ok = match source {
                Some(src) => self.copy_to_clipboard_now(id, &src, is_video),
                None => {
                    self.preview_toast_icon(id, ToastKind::Error, "Nothing to copy yet", "clipboard-x-symbolic");
                    false
                }
            };
            // DRAGON-355: a failed COPY aborts the WHOLE action, including a copy-on-delete.
            // The DRAGON-353 carve-out that let a delete proceed on a "courtesy copy" miss is
            // gone: deleting (or closing) the capture when it never reached the clipboard
            // destroys the user's only copy over a failure they can SEE. `copy_to_clipboard_now`
            // (or the "Nothing to copy" arm above) already TOASTED the miss; `fail_close_action`
            // additionally re-raises the unsaved-changes dialog when the action came from it (a
            // no-op for a toolbar press, whose editor simply stays up). Nothing was baked and no
            // history moved, so retry / save / exit-anyway are all live. Where a platform cannot
            // detect a late failure (the detached Linux worker's own write, in another process)
            // `copy_ok` is `true` and the flow proceeds exactly as before — honest per platform,
            // never faked (see `share::clipboard::copy_to_clipboard`).
            if copy_failure_aborts(intent, copy_ok) {
                self.fail_close_action(id, copy_failure_reason(intent.deletes()));
                return Task::batch(tasks);
            }
        }

        // ── 3. DELETE ─────────────────────────────────────────────────────────────────
        // Reached only once step 2's copy (if any) SUCCEEDED — a copy-on-delete whose copy
        // failed aborted above, so the file survives the clipboard miss.
        if intent.deletes() {
            self.stop_preview_playback(id);
            // A delete that could NOT remove everything has not done what was asked. Say so
            // honestly and STAY: the document is untouched (nothing was baked, no history
            // moved), so retrying or exiting anyway are both live options. `copies()` tells it
            // whether a courtesy-copy toast is already speaking for this action, so a PLAIN
            // delete gets its own "Capture deleted" confirmation instead of closing silently.
            if let Err(reason) = self.delete_owned_files(id) {
                self.fail_close_action(id, reason);
                return Task::batch(tasks);
            }
        }

        // ── 4. CLOSE (guarded) ────────────────────────────────────────────────────────
        self.clear_close_intent(id);
        if !close_after {
            return Task::batch(tasks);
        }
        // A close that neither SAVED nor DELETED would silently drop unsaved edits — the
        // close-on-copy-WITHOUT-save case (DRAGON-355). Route it through the SAME
        // unsaved-changes guard the Esc / ✕ close uses, so the user gets Save / Discard /
        // Keep editing instead of losing work. EXCEPT when the action came from that very
        // dialog (`from_dialog`): it already asked, and re-raising would bounce it off
        // itself. A saving close committed the edits; a deleting close discarded the file on
        // purpose; neither needs the guard.
        if close_guards_unsaved(intent, from_dialog)
            && self
                .preview_for(id)
                .is_some_and(|p| close_needs_confirmation(p.unsaved(), p.edit.confirm_close))
        {
            if let Some(p) = self.preview_for_mut(id) {
                p.edit.confirm_close = true;
            }
            return Task::batch(tasks);
        }
        // Everything asked for has happened, so CLOSE NOW — no hold, whatever the flavour
        // (DRAGON-371). A close-after-copy / -delete used to sit here for a second
        // (`COPY_CLOSE_HOLD`, driven by a `PendingClose` hold state) purely so its SUCCESS
        // toast could be read; both are gone. The user's instruction was "make it as fast as
        // possible - just dont exit if the copy/delete/whatever failed", and the not-exiting
        // half is not the hold's job at all: every failing step above ALREADY returned early
        // with the editor still up (`copy_failure_aborts` for the clipboard,
        // `delete_owned_files`'s `Err` for the unlink, the unsaved-changes guard for edits).
        // The hold only ever protected the SUCCESS toasts, and on success the surface going
        // away IS the feedback — including the delete's "Capture deleted", which step 3 posts
        // after it has already unlinked, so nothing is lost by never showing it. Reading
        // matters only on the failure path, and that path does not reach here.
        tasks.push(self.close_preview(id));
        Task::batch(tasks)
    }

    /// Disarm the "and then close" the dialog armed, on a path that did NOT fail — the
    /// mirror of [`Self::fail_close_action`], which disarms it on one that did. Either way
    /// the flag never survives the action it belonged to, so it can't attach itself to an
    /// unrelated later completion.
    fn clear_close_intent(&mut self, id: window::Id) {
        if let Some(p) = self.preview_for_mut(id) {
            p.edit.close_after_share = false;
        }
    }

    /// Unlink every file this document owns, and toast the outcome.
    ///
    /// Deletes [`PreviewState::delete_paths`]: the capture plus every path the document
    /// WROTE (the `-edited` variants and any Save As destinations, wherever they landed).
    /// A path that is already gone is not a failure — it is the outcome we wanted — so only
    /// a real unlink error counts. A partial failure is reported as an `Err`, on which the
    /// caller does NOT close (DRAGON-355): the editor stays up over whatever survived, with
    /// the toast naming it, so retrying or exiting anyway are both live.
    ///
    /// A successful delete still posts ONE "Capture deleted" toast, though since DRAGON-371
    /// its close is immediate, so in practice only the FAILURE text is ever read (which is
    /// the text that matters — see `copy_failure_aborts`). The file COUNT is
    /// deliberately not named on success: the original and its `-edited` / Save As
    /// siblings are the SAME image to the user, and "Deleted 2 files" reads as a surprise
    /// rather than a receipt (user decision, 2026-07-27; only the failure toast still
    /// talks in files, because a partial failure is exactly where the distinction
    /// matters). On a copy-on-delete the confirmation STACKS beside the copy toast (the
    /// DRAGON-353 queue): the delete is its own action and deserves its own receipt.
    fn delete_owned_files(&mut self, id: window::Id) -> Result<(), String> {
        let paths = self.preview_for(id).map(|p| p.delete_paths()).unwrap_or_default();
        let mut gone = 0usize;
        let mut failures: Vec<String> = Vec::new();
        for path in &paths {
            match std::fs::remove_file(path) {
                Ok(()) => gone += 1,
                // Already absent: the desired end state, not an error.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    log::warn!("preview: could not delete {}: {e}", path.display());
                    failures.push(format!("{}: {e}", Self::display_name(path)));
                }
            }
        }
        log::info!(
            "preview delete: {gone} removed, {} failed, {} considered",
            failures.len(),
            paths.len()
        );
        if !failures.is_empty() {
            self.preview_toast(
                id,
                ToastKind::Error,
                format!("Deleted {gone} of {} files", gone + failures.len()),
            );
            return Err(format!("These files couldn't be deleted: {}", failures.join("; ")));
        }
        self.preview_toast_icon(id, ToastKind::Success, "Capture deleted", "edit-delete-symbolic");
        Ok(())
    }
}

/// DRAGON-355: does a failed clipboard copy ABORT the share before its delete/close steps?
///
/// Yes for EVERY copy flavour whose copy did not succeed. This REPLACES the DRAGON-353
/// carve-out that let a copy-on-delete proceed to delete on a failed courtesy copy: deleting
/// (or closing) the capture when it never reached the clipboard would destroy the user's only
/// copy over a failure they can see, so the destructive step is vetoed and the editor stays
/// open. A non-copying intent (a plain Save, a plain Delete) is never gated on the clipboard.
///
/// # This veto IS the "don't exit if it failed" rule (DRAGON-371)
///
/// It, and not any timed hold, is what keeps the editor up when something went wrong: the
/// share returns early with the surface still live, so retry / save / exit-anyway all stay
/// reachable. That is why dropping `COPY_CLOSE_HOLD` cost nothing — the hold only ever held
/// open on SUCCESS, where the closing surface is itself the confirmation.
///
/// # How much the veto can actually promise, per platform
///
/// `copy_ok` is honest per platform (see `share::clipboard::copy_to_clipboard`): macOS/Windows
/// report the real synchronous pasteboard result, so there the guarantee is exactly what it
/// sounds like. **On LINUX it is weaker**: `copy_ok` reports only whether the detached
/// selection worker could be SPAWNED. A worker that spawns and then fails inside its own
/// process reads as success here — it happens after we have let go, with no channel back, so
/// it is undetectable rather than ignored. So on Linux "we never destroy/close over a failed
/// copy" holds for every failure this side can SEE, which is not every failure there is.
/// Pre-existing and not introduced by DRAGON-371; recorded here because this is where the
/// promise is made.
///
/// The reassuring corollary of that same detachment: closing INSTANTLY is safe for the
/// clipboard on Linux. The worker is a separate process that owns the Wayland selection until
/// something else is copied, so the copied data outlives the editor (and this one-shot process)
/// by construction — there is nothing for a hold to protect.
pub(super) fn copy_failure_aborts(intent: ShareIntent, copy_ok: bool) -> bool {
    intent.copies() && !copy_ok
}

/// DRAGON-355: must a settings-driven CLOSE consult the unsaved-changes guard before it
/// runs, instead of closing straight away?
///
/// Yes exactly for a close that neither SAVED nor DELETED — the close-on-copy-WITHOUT-save
/// case — and only when it did NOT originate from the unsaved-changes dialog itself. Such a
/// close would otherwise silently drop the in-memory edits (the file on disk keeps its last
/// saved bytes; the clipboard has the edited ones), so it is routed through the same
/// Save / Discard / Keep-editing card the Esc / ✕ close uses. A saving close already committed
/// the edits; a deleting close discarded the file on purpose; a `from_dialog` close was chosen
/// AT that card and must not bounce off it. (The document's own `unsaved()` is the final term,
/// applied at the call site.)
pub(super) fn close_guards_unsaved(intent: ShareIntent, from_dialog: bool) -> bool {
    !from_dialog && !intent.saves() && !intent.deletes()
}

/// The failure reason handed to [`App::fail_close_action`] when a copy step aborts — spells
/// out that the destructive half did NOT run, so the dialog's re-raised card is truthful.
fn copy_failure_reason(deletes: bool) -> &'static str {
    if deletes {
        "The capture couldn't be put on the clipboard, so nothing was copied and the file was kept."
    } else {
        "The capture couldn't be put on the clipboard, so nothing was copied."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DRAGON-355: a failed copy aborts the destructive step for EVERY copy flavour — the
    /// copy-on-delete carve-out is gone. A successful copy never aborts, and a non-copying
    /// intent is never gated on the clipboard at all.
    #[test]
    fn a_failed_copy_aborts_every_copy_flavour_including_copy_on_delete() {
        use ShareIntent::*;
        // A successful copy proceeds, whatever the flavour.
        for i in [Copy, SaveCopy, CopyClose, SaveCopyClose, CopyThenDelete] {
            assert!(!copy_failure_aborts(i, true), "{i:?} with a good copy must proceed");
        }
        // A failed copy aborts the plain copies, the closing copies AND copy-on-delete
        // alike — the whole point of the ticket (the delete no longer proceeds on a miss).
        for i in [Copy, SaveCopy, CopyClose, SaveCopyClose, CopyThenDelete] {
            assert!(copy_failure_aborts(i, false), "{i:?} with a failed copy must abort");
        }
        // Non-copying intents never consult the clipboard, pass or fail.
        for i in [Save, Delete] {
            assert!(!copy_failure_aborts(i, false), "{i:?} is never gated on the clipboard");
            assert!(!copy_failure_aborts(i, true));
        }
    }

    /// DRAGON-355 copy-close gating: a close that neither saved nor deleted (the
    /// close-on-copy-WITHOUT-save case) must consult the unsaved-changes guard so it cannot
    /// silently drop edits — but a saving/deleting close, and a close that came from the
    /// dialog itself, must not.
    #[test]
    fn only_a_non_saving_non_deleting_toolbar_close_guards_unsaved_edits() {
        use ShareIntent::*;
        // The one intent that needs the guard: close-on-copy without save, from the toolbar.
        assert!(close_guards_unsaved(CopyClose, false), "close without save must ask");
        // A save committed the edits; a delete discarded the file on purpose — no guard.
        for i in [SaveCopyClose, SaveCopy, CopyThenDelete, Delete, Save] {
            assert!(!close_guards_unsaved(i, false), "{i:?} needs no unsaved guard");
        }
        // From the dialog, nothing re-guards (the card already asked) — including CopyClose.
        for i in [CopyClose, SaveCopyClose, Copy, Delete] {
            assert!(!close_guards_unsaved(i, true), "{i:?} from the dialog must not re-ask");
        }
    }

    /// The abort reason names the destructive half that did NOT run, so a re-raised dialog is
    /// truthful, and carries no em/en-dash (the runtime-string house rule).
    #[test]
    fn the_copy_failure_reason_is_truthful_and_dash_free() {
        assert!(copy_failure_reason(true).contains("file was kept"));
        assert!(!copy_failure_reason(false).contains("file was kept"));
        for deletes in [true, false] {
            let r = copy_failure_reason(deletes);
            assert!(!r.contains('—') && !r.contains('–'), "no em/en-dash in {r:?}");
        }
    }
}
