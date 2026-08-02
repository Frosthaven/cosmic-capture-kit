//! Share plumbing: the save destination picker, the background bake that renders pending
//! edits, and the completion seam a copy lands on.
//!
//! # The model (DRAGON-353, reshaped by DRAGON-467)
//!
//! The editor has TWO ways of getting a document out, and they are deliberately different
//! shapes:
//!
//! * **Save** opens the destination picker ([`App::save_as_dialog`], pre-filled with
//!   [`App::preview_save_target`]) and the pick runs its own export in
//!   `PreviewMsg::SaveAsResult` → `SaveAsBaked`, which bakes straight to the chosen path and
//!   retargets the document onto it.
//! * **Copy** runs through [`App::run_copy`] → [`App::finish_copy`], whether a bake ran
//!   (asynchronously, via `BakeDone`), was reused, or was not needed at all. That funnel is
//!   what keeps "copy leaves what is on disk alone", "a failed copy never closes anything"
//!   and "who closes the document" from drifting across the three callers (the toolbar, the
//!   exit path, and the ask card's discard).
//!
//! * **Share** (DRAGON-474) runs through [`App::run_share_sheet`] →
//!   [`App::finish_share_sheet`] — the same refuse / reuse / bake fork as Copy, delivering
//!   to the system share sheet instead of the clipboard.
//!
//! There is no fourth. DELETE used to be here, with its own `ShareIntent` flavour, its
//! file-unlink step and a copy-first setting; the editor stopped deleting anything in
//! DRAGON-467, so the intent enum, the delete step and the tracked `written` set all went
//! (the share sheet brought back exactly one bit of it, `bake_for_share`).
//!
//! Rules this file enforces that used to be scattered:
//!
//! * **An action NEVER closes the editor by itself.** Only an explicit `close_after_share`
//!   closes anything, and exactly two callers arm it: the unsaved-changes card's Save button,
//!   and the exit path's "Automatically copy changes on exit".
//! * **A close never rides on a failed action.** Every failing step returns early with the
//!   surface still up, so the exit copy cannot close over a clipboard write that did not
//!   happen.
//! * **Feedback is a per-document TOAST**, not a desktop notification. The processing
//!   notification is gone too — the editor stays up and shows its own spinner instead
//!   (see `PREVIEW_PROCESSING_MESSAGES`).

use super::*;

impl App {
    /// Post a toast on `id`'s document, carrying the outcome's own glyph (copied / saved, and
    /// their failures) rather than a severity default. A no-op for a document that has already
    /// closed, so a late async completion can never resurrect state.
    ///
    /// DRAGON-467: there used to be an icon-LESS `preview_toast` beside this. Its only caller
    /// was the delete's partial-failure toast, so it went with the delete feature and every
    /// notice now names its glyph.
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
        self.toast_copy_outcome(id, ok);
        ok
    }

    /// Post the clipboard outcome's toast, and record the history position the clipboard now
    /// holds. Shared by the synchronous Copy action above and by the asynchronous open-time
    /// copy's completion ([`PreviewMsg::AutoCopied`], DRAGON-454), so the two can never word
    /// the same outcome differently, and so BOTH count as "the clipboard has this state"
    /// (DRAGON-467 review, major 4 — the auto-copy lands at depth 0, which is what makes an
    /// immediate Escape on an untouched capture copy nothing a second time).
    pub(super) fn toast_copy_outcome(&mut self, id: window::Id, ok: bool) {
        if ok && let Some(p) = self.preview_for_mut(id) {
            p.edit.mark_copied();
        }
        if ok {
            self.preview_toast_icon(id, ToastKind::Success, "Copied to clipboard", "clipboard-check-symbolic");
        } else {
            self.preview_toast_icon(id, ToastKind::Error, "Couldn't copy to clipboard", "clipboard-x-symbolic");
        }
    }

    // DRAGON-467 removed `stage_clipboard_copy` from here. It copied the document to a
    // throwaway runtime-dir file so the clipboard could be served from something that
    // OUTLIVED an imminent unlink — the copy-on-delete path, where the Linux worker is handed
    // a PATH (it reads a still's bytes, or advertises a recording's `file://` URI) from a
    // detached child, so unlinking the original right after spawning it was a race at best
    // and a dead URI at worst. With copy-on-delete gone (the capture reaches the clipboard at
    // capture time instead) no share both copies and deletes, so nothing needs staging. If a
    // copy-then-delete flavour ever comes back, it needs this again: serving the clipboard
    // from a file you are about to remove does not work.

    /// The AUTOMATIC clipboard copy a preview editor performs as it opens (DRAGON-353 —
    /// the "Automatically copy to clipboard" setting became unconditional behaviour, and
    /// this is where it landed). Idempotent per document: the path can arrive after the
    /// surface (a pre-opened spinner), so several seams call this and only the first one
    /// with a path does anything.
    ///
    /// * A `--preview` file is NOT copied: it is the user's own file, opened as a viewer,
    ///   and silently hijacking their clipboard for it was never asked for.
    /// * An IMAGE over the clipboard SIZE LIMIT ([`crate::share::AUTO_COPY_MAX_BYTES`], a
    ///   fixed constant since DRAGON-353 removed the setting) is skipped with an error toast
    ///   naming the limit — that toast is why the knob was no longer needed. A RECORDING is
    ///   never skipped for size (DRAGON-450): it copies as a path, not as bytes, so the
    ///   limit has nothing to bound and refusing a long recording only cost the user their
    ///   copy. See [`crate::share::copy_embeds_bytes`].
    /// * It never saves and never closes, whatever the "Automatically save on copy" /
    ///   "Automatically close on copy" settings say: those are about the user's Copy ACTION.
    ///   An editor that shut itself the instant it opened would be unusable.
    ///
    /// # Why this one is asynchronous and the Copy ACTION is not (DRAGON-454)
    ///
    /// The clipboard write itself is real work — on Windows it DECODES the capture's PNG a
    /// second time and re-encodes it for the clipboard, and `OpenClipboard` can be held by
    /// another app (a clipboard manager) for as long as it likes. Measured at ~55-75 ms for a
    /// 5120x1440 still on the dev box, and unbounded in the contended case.
    ///
    /// It used to run inline, inside `update`. On the routes that pre-open a spinner (a window
    /// grab, a freeze crop, a stopped recording) that put the whole cost in front of a user who
    /// was already looking at the editor: the surface was up and the toolbar simply did not
    /// answer. On the routes that open the editor here, it ran BEFORE the surface's own open
    /// task, so it delayed the editor appearing at all. Nothing in the editor reads the result
    /// — the only output is a toast — so it has no business on the UI thread.
    ///
    /// The user's explicit Copy stays SYNCHRONOUS on purpose: `finish_share_intent` gates the
    /// delete and the close on its outcome (DRAGON-355), so that one must answer before the
    /// action continues. This one answers to nobody.
    ///
    /// Returns the task carrying the outcome back as [`PreviewMsg::AutoCopied`]; the
    /// `copied_on_open` latch is set BEFORE the work starts, so the several seams that call
    /// this still copy exactly once even while a copy is in flight.
    pub(super) fn auto_copy_preview_on_open(
        &mut self,
        id: window::Id,
    ) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for(id) else { return Task::none() };
        if p.copied_on_open || p.external {
            return Task::none();
        }
        let Some(path) = p.path.clone() else { return Task::none() };
        let size = p.size.unwrap_or(0);
        let is_video = matches!(p.kind, PreviewKind::Video(_));
        if let Some(p) = self.preview_for_mut(id) {
            p.copied_on_open = true;
        }
        if crate::platform::services::copy_embeds_bytes(&path, is_video)
            && size > crate::platform::services::AUTO_COPY_MAX_BYTES
        {
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
            return Task::none();
        }
        // DRAGON-454: OFF the UI thread, bracketed on the launch timeline at both ends. A
        // plain OS thread rather than `spawn_blocking`, matching every other "this blocks, get
        // it off the loop" worker in the app (the image decode right beside it, the capture
        // worker): the executor's blocking pool is not something the one-shot process wants to
        // wait on at teardown.
        crate::util::timing_mark("preview: auto-copy on open (begin, worker thread)");
        let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let ok = crate::platform::services::copy_to_clipboard(&path, is_video);
            crate::util::timing_mark("preview: auto-copy on open (done, worker thread)");
            let _ = tx.send(ok);
        });
        Task::perform(rx, move |res| {
            // A dropped sender means the worker died mid-write. Nothing was put on the
            // clipboard, so it reads as the failure it is — the same toast a refused write
            // gets. The capture is on disk either way; only the courtesy copy is lost.
            cosmic::Action::App(Msg::Preview(id, PreviewMsg::AutoCopied(res.unwrap_or(false))))
        })
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

    /// An action that had a CLOSE armed behind it FAILED (DRAGON-353 follow-up): keep the
    /// document and say so.
    ///
    /// ONE path for every such action — a failed Save, Copy or Delete lands here, so the user
    /// always gets the same ways out whatever went wrong. The close intent is DISARMED: the
    /// whole point is that we are not closing, and leaving it armed would let an unrelated
    /// later completion close on the back of a failure.
    ///
    /// A no-op when nothing armed a close: a toolbar action's failure is reported by its toast
    /// and the editor simply stays up, which is already the ruling.
    ///
    /// # Why the card is not always the right answer (DRAGON-467 review, minor 6)
    ///
    /// The card is titled "unsaved changes", and it offers Save / Copy / Delete / Exit anyway.
    /// That is exactly right for a failure the user reached FROM it. It is wrong for a failed
    /// EXIT COPY on a document that has nothing unsaved, or whose owner turned "Ask to save"
    /// off: raising it there invents a claim ("you have unsaved changes") that is not true,
    /// and re-asks a question the setting already answered. Those cases get the honest thing
    /// instead, a toast, which the caller has already posted. `raise_card` is that decision.
    pub(super) fn fail_close_action(&mut self, id: window::Id, reason: impl Into<String>) {
        let raise_card = {
            let a = self.preview_automation(id);
            self.preview_for(id)
                .is_some_and(|p| card_answers_the_failure(a.ask_to_save, p.unsaved()))
        };
        if let Some(p) = self.preview_for_mut(id) {
            if raise_card {
                p.edit.note_action_failure(reason);
            } else {
                // Disarm the close without raising anything. The editor stays up over the
                // toast the caller posted, which is all a failure with nothing to lose owes.
                p.edit.close_after_share = false;
            }
        }
    }

    /// Open the native save file chooser PRE-FILLED with [`Self::preview_save_target`], then
    /// route the pick to `SaveAsResult`.
    ///
    /// DRAGON-467: this IS the Save button now. The prefill is a full PATH, not just a name —
    /// the user's configured save folder plus the filename a plain overwrite-save would have
    /// written — so the picker opens where the setting says captures live even when the bytes
    /// are still in the runtime directory ("Automatically save originals" off). The native
    /// dialog's own replace prompt is what protects an existing file.
    ///
    /// Only a fullscreen OVERLAY is torn down first: it's a layer-shell surface with an
    /// exclusive keyboard grab, so the file chooser would render behind it and be
    /// unusable. A cancelled dialog re-mints the overlay on the still-loaded capture
    /// ([`Self::reopen_preview_surface`], DRAGON-157). A normal WINDOW can show the
    /// chooser over itself, so it stays open.
    ///
    /// The teardown goes through [`Self::hide_preview_surface`], and that is not tidiness: it
    /// is what keeps this process ALIVE (DRAGON-469). Off Linux the overlay is a real winit
    /// window, so the `window::close` it issues echoes a `window::Event::Closed` straight back
    /// — the runtime cannot tell our own teardown from a window manager's. That echo used to
    /// close the document, which was the last one, which ran `finish_session`: the process
    /// exited with the chooser still on screen, so nothing was ever exported and the editor
    /// never came back. [`super::surface_closed`] tells the two apart by reading
    /// `surface_open`, which the seam clears BEFORE minting the destroy — an ordering that is
    /// structural there rather than a rule this function has to remember.
    pub(super) fn save_as_dialog(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        self.stop_preview_playback(id);
        // `preview_save_target` already forces `.png` for a still (DRAGON-455: a still is
        // what we are going to write, so suggesting `clip.JPG` for a file that comes out as a
        // PNG is a mismatch shown before the user has touched anything) and already places the
        // name in the configured folder. This is only the starting path; the PICK is forced
        // through the same png rule in `SaveAsResult`, so retyping cannot get around it.
        let suggested = self
            .preview_save_target(id)
            .unwrap_or_else(|| PathBuf::from("capture"));
        // DRAGON-467 review, minor 8: the folder has to EXIST before the portal is asked to
        // open in it. With "Automatically save originals" off nothing has created it yet, and
        // a missing `current_folder` is not an error the portal reports — it silently opens
        // somewhere else, so the pre-fill the ticket asked for would quietly not happen on
        // exactly the configuration that needs it most. Best-effort: a folder we cannot
        // create is the portal's own fallback, which is where we would have been anyway.
        if let Some(dir) = suggested.parent().filter(|d| !d.as_os_str().is_empty()) {
            let _ = std::fs::create_dir_all(dir);
        }
        // A fullscreen OVERLAY comes down for the chooser; a WINDOW keeps its surface and
        // shows the chooser over itself. The teardown goes through the ONE seam
        // ([`App::hide_preview_surface`], DRAGON-469), which clears the liveness flag and
        // mints the destroy TOGETHER — the document stays loaded, so a later close cannot
        // double-destroy it and our own echoed `Closed` cannot end the session.
        let hide = match self.preview_for(id) {
            Some(p) if !p.surface.is_window() => self.hide_preview_surface(id),
            _ => Task::none(),
        };
        let pick = Task::perform(super::pick_save_path(suggested), move |opt| {
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
    // (`saved_path` / `size` / `note_written` / `edit.mark_saved`). The displayed result
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

    /// WHERE a Save writes, i.e. what the destination picker opens PRE-FILLED with:
    /// [`naming::save_prefill`] over the document's save identity and the user's configured
    /// save folder. `None` when the document has no file at all.
    ///
    /// Three inputs, assembled here because only `App` knows two of them:
    ///
    /// * `saved_path` once the document has saved once, else the capture's own `path`
    ///   (DRAGON-353 follow-up — `path` stays pinned to the media, so the naming rule reads
    ///   the save side explicitly rather than assuming the two are the same).
    /// * the configured folder for this media kind (`screenshot_dir` / `record_dir`), which
    ///   DRAGON-467 keeps as THE basis for the Save action. An EXTERNAL `--preview` document
    ///   gets `None` instead: it is the user's own file, so it saves back to its own folder,
    ///   not into the capture folder.
    /// * DRAGON-455: a STILL is written as PNG, so its target NAMES png — forced BEFORE the
    ///   folder is applied, so the name placed in the save folder is the one that will
    ///   actually be written. A recording keeps whatever container it is in; `bake_video`
    ///   honours that.
    pub(super) fn preview_save_target(&self, id: window::Id) -> Option<PathBuf> {
        let p = self.preview_for(id)?;
        let is_video = matches!(p.kind, PreviewKind::Video(_));
        let current = p.path.as_ref().or(p.saved_path.as_ref())?;
        let current = if is_video { current.clone() } else { naming::png_name(current) };
        let dir = if p.external {
            None
        } else {
            Some(self.capture_save_dir(is_video))
        };
        Some(naming::save_prefill(p.saved_path.as_deref(), &current, dir.as_deref()))
    }

    /// The file this document IS on disk right now — its last save if it has one, else the
    /// capture it opened with. What a Copy puts on the clipboard and what the toasts name.
    pub(super) fn preview_current_file(&self, id: window::Id) -> Option<PathBuf> {
        let p = self.preview_for(id)?;
        p.saved_path.clone().or_else(|| p.path.clone())
    }

    /// Whether this document's media can be BAKED at all right now (DRAGON-398).
    ///
    /// An IMAGE always can — its pixels are already decoded in the editor. A VIDEO cannot
    /// until its ffprobe metadata has landed: [`edit::VideoBake`] needs the pixel size (for
    /// the covermark raster) and the audio-presence flag (for the cut filtergraph), and a
    /// probe that timed out or failed (DRAGON-106) leaves `meta` at `None` forever. A
    /// document with no file at all is likewise unbakeable.
    ///
    /// Split out because "the bake silently did not happen" used to be indistinguishable
    /// from "there was nothing to bake": both made [`Self::begin_bake`] return `None`, and
    /// the share then completed against the UNEDITED file wearing a success toast — a Save
    /// that reported "Already saved as …" without saving the edits, and a Copy that put the
    /// untouched recording on the clipboard. See [`bake_blocked`] for the rule this feeds.
    fn preview_media_bakeable(&self, id: window::Id) -> bool {
        let Some(p) = self.preview_for(id) else { return false };
        if p.path.is_none() {
            return false;
        }
        match &p.kind {
            PreviewKind::Image(_) => true,
            PreviewKind::Video(vid) => vid.meta.is_some(),
        }
    }

    /// Report — and REFUSE — a share whose bake cannot run (DRAGON-398). Returns `true` when
    /// the caller must abandon the action.
    ///
    /// `owed` is whether this action would have to bake (the intent's [`ShareIntent::owes_bake`],
    /// or a plain `dirty()` for Save As, which always writes a fresh file). When something is
    /// owed and [`Self::preview_media_bakeable`] says it cannot be produced, the whole action
    /// stops here: nothing is written, nothing is copied, nothing is deleted and nothing closes.
    /// The document keeps its edits and the editor stays up with the reason — the same shape as
    /// every other failure in this file (`copy_failure_aborts`, `delete_owned_files`'s `Err`), so
    /// retry / save / exit-anyway all stay reachable.
    fn refuse_unbakeable(&mut self, id: window::Id, owed: bool) -> bool {
        if !bake_blocked(owed, self.preview_media_bakeable(id)) {
            return false;
        }
        log::warn!("preview: refusing a share that owes a bake the media can't produce");
        // The wording names the media honestly: the real (and video-only) cause is a probe
        // that never landed, but the degenerate no-file case can reach here for either kind.
        let video = self.preview_for(id).is_some_and(|p| matches!(p.kind, PreviewKind::Video(_)));
        let (toast, reason) = if video {
            (
                "Couldn't read this recording, so the edits can't be applied",
                "This recording couldn't be read, so the edits couldn't be applied and nothing \
                 was written, copied or deleted.",
            )
        } else {
            (
                "Couldn't read this capture, so the edits can't be applied",
                "This capture couldn't be read, so the edits couldn't be applied and nothing \
                 was written, copied or deleted.",
            )
        };
        self.preview_toast_icon(id, ToastKind::Error, toast, "save-off-symbolic");
        self.fail_close_action(id, reason);
        true
    }

    /// THE copy entry point: put `id`'s current state on the clipboard, baking first when
    /// there are edits to render. The toolbar's Copy, the exit copy and the discard-with-copy
    /// all route through here, so the bake-vs-reuse-vs-nothing fork exists once.
    ///
    /// It used to take a `ShareIntent` naming which of save / copy / delete to run. Save moved
    /// to the destination picker (DRAGON-467) and Delete left the editor entirely, so there is
    /// one action left and the parameter said nothing.
    pub(super) fn run_copy(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        // DRAGON-398: a copy that OWES a bake it cannot produce must not quietly complete
        // against the unedited file. Refuse it loudly instead (the editor stays up).
        let owed = self.preview_for(id).is_some_and(|p| p.dirty());
        if self.refuse_unbakeable(id, owed) {
            return Task::none();
        }
        // DRAGON-467 review, major 4: a copy standing on its own last bake REUSES that
        // artifact. Without this, "Save then Escape" ran the scene through the encoder twice,
        // because `dirty()` stays true after a save by design. `Reuse` hands the artifact
        // straight to the completion seam, which copies it exactly as it would a fresh bake's.
        let reuse = self.preview_for(id).and_then(|p| {
            match edit::bake_need(
                p.dirty(),
                p.edit.undo_stack.len(),
                p.edit.baked.as_ref().map(|(d, _)| *d),
            ) {
                // The artifact has to still BE there — a runtime-dir temp can be swept, and a
                // saved file can be moved out from under us between actions.
                edit::BakeNeed::Reuse => {
                    p.edit.baked.as_ref().map(|(_, f)| f.clone()).filter(|f| f.exists())
                }
                _ => None,
            }
        });
        if let Some(artifact) = reuse {
            log::debug!("preview: reusing the last bake instead of rendering it again");
            return self.finish_copy(id, Some(artifact));
        }
        match self.begin_bake(id) {
            // A bake is in flight; `BakeDone` calls `finish_copy` with its output.
            Some(task) => task,
            // Nothing to render — copy the file as it stands.
            None => self.finish_copy(id, None),
        }
    }

    /// THE share-sheet entry point (DRAGON-474): hand `id`'s current state to the system
    /// share sheet, baking first when there are edits to render — the same refuse / reuse /
    /// bake fork as [`Self::run_copy`], so Share and Copy can never disagree about what
    /// "the current state" means.
    ///
    /// This funnel is the fix for the bug the first wiring shipped: it handed
    /// `preview_current_file` straight to the sheet, which is the last save (or the pristine
    /// capture), so every unsaved edit was silently absent from what the share target
    /// received. DRAGON-398's rule applies here exactly as it does to Copy: a share that
    /// OWES a bake it cannot produce is refused loudly with the editor up, never quietly
    /// served the unedited file.
    pub(super) fn run_share_sheet(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        let owed = self.preview_for(id).is_some_and(|p| p.dirty());
        if self.refuse_unbakeable(id, owed) {
            return Task::none();
        }
        // A share standing on the last bake's exact state serves that artifact, exactly as
        // a copy does (DRAGON-467 review, major 4) — and the artifact is SHARED between the
        // two actions: a Copy's bake serves the Share that follows it, and vice versa.
        let reuse = self.preview_for(id).and_then(|p| {
            match edit::bake_need(
                p.dirty(),
                p.edit.undo_stack.len(),
                p.edit.baked.as_ref().map(|(d, _)| *d),
            ) {
                edit::BakeNeed::Reuse => {
                    p.edit.baked.as_ref().map(|(_, f)| f.clone()).filter(|f| f.exists())
                }
                _ => None,
            }
        });
        if let Some(artifact) = reuse {
            log::debug!("preview: sharing the last bake instead of rendering it again");
            return self.finish_share_sheet(id, Some(artifact));
        }
        match self.begin_bake(id) {
            Some(task) => {
                // The bake is in flight — tell `BakeDone` its artifact is for the share
                // sheet, not the clipboard.
                if let Some(p) = self.preview_for_mut(id) {
                    p.edit.bake_for_share = true;
                }
                task
            }
            // Nothing to render — the file on disk IS the current state; share it.
            None => self.finish_share_sheet(id, None),
        }
    }

    /// THE completion seam a share lands on — after a bake (`baked` = the temp it wrote), on
    /// a reused artifact, or straight away for a clean document (`baked` = `None`).
    ///
    /// Unlike [`Self::finish_copy`] there is no close step: Share is a toolbar action only,
    /// no exit-card button arms a close behind it, and handing a file to another app is not
    /// an "I'm done here" signal the way a save is. The one deferred-close interaction (the
    /// surface dying mid-bake) is handled at `BakeDone`, which closes instead of anchoring
    /// a sheet to a window that no longer exists.
    pub(super) fn finish_share_sheet(
        &mut self,
        id: window::Id,
        baked: Option<PathBuf>,
    ) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for(id) else { return Task::none() };
        let is_video = matches!(p.kind, PreviewKind::Video(_));
        let Some(path) = baked.or_else(|| self.preview_current_file(id)) else {
            self.preview_toast_icon(id, ToastKind::Error, "Nothing to share yet", "share-symbolic");
            return Task::none();
        };

        // DRAGON-480: macOS's picker needs the exact preview VIEW, which only window
        // IDENTITY can give us (DRAGON-336 allows several simultaneous previews open at
        // once) — reached through `window::run_with_handle`, the same route
        // `app::keyboard::show_character_palette` uses for the identical reason. See
        // `platform::mac::share`'s module doc for why this bypasses the portable seam
        // function every other platform calls directly below.
        #[cfg(target_os = "macos")]
        {
            window::run_with_handle(id, move |handle| {
                use window::raw_window_handle::RawWindowHandle;
                match handle.as_raw() {
                    // SAFETY: `run_with_handle` runs synchronously on the main thread while
                    // this exact window's handle is held live, matching the caller contract
                    // `share_file_at_view` documents.
                    RawWindowHandle::AppKit(h) => unsafe {
                        crate::platform::mac::share::share_file_at_view(&path, is_video, h.ns_view)
                    },
                    // Belt-and-braces: `run_with_handle` not handing back an AppKit handle
                    // would be new winit/iced behavior, not something expected to happen.
                    // Fall back to the portable seam's best-effort (key-window) body rather
                    // than refuse outright.
                    _ => crate::platform::services::share_file(&path, is_video),
                }
            })
            .map(move |result| {
                cosmic::Action::App(Msg::Preview(id, PreviewMsg::ShareDone(result)))
            })
        }

        #[cfg(not(target_os = "macos"))]
        {
            if let Err(reason) = crate::platform::services::share_file(&path, is_video) {
                self.preview_toast_icon(id, ToastKind::Error, reason, "share-symbolic");
            }
            Task::none()
        }
    }

    /// The Save As guard (DRAGON-398): `true` when the destination picker must NOT open,
    /// because the edits this export would have to render cannot be produced. Refusing
    /// BEFORE the dialog is deliberate — making the user choose a destination for a save
    /// that is going to drop their edits is worse than refusing up front.
    pub(super) fn refuse_unbakeable_save_as(&mut self, id: window::Id) -> bool {
        // Save As always writes a NEW file, so it owes a bake whenever the scene is dirty
        // (there is no "standing on our own save point" no-op to fall back to).
        let owed = self.preview_for(id).is_some_and(|p| p.dirty());
        self.refuse_unbakeable(id, owed)
    }

    /// Kick off a bake before running `intent`, or `None` when none is needed (no pending
    /// edits, no path, or a video without probed dims).
    ///
    /// DRAGON-353: the bake runs with the EDITOR STILL UP. It used to vanish the surface and
    /// span the re-encode with a desktop "Processing capture" notification; instead the editor
    /// draws its own processing overlay (the same spinner the load state uses) while
    /// `edit.baking` holds every input. The output is always a throwaway TEMP, because the
    /// things that bake here — a COPY, or a SHARE (DRAGON-474) — must both leave whatever
    /// is on disk alone. (A SAVE bakes too, but straight to the destination the user
    /// picked, in `PreviewMsg::SaveAsResult`.)
    pub(super) fn begin_bake(&mut self, id: window::Id) -> Option<Task<cosmic::Action<Msg>>> {
        let processing_msg = random_processing_msg();
        let p = self.preview_for_mut(id)?;
        // WHEN a bake is owed: the throwaway temp does not exist yet, so ANY scene content
        // needs rendering onto it (DRAGON-352 — this is what puts the EDITED picture on the
        // clipboard, and what makes "copy changes on exit" carry the edits).
        if !p.dirty() || p.edit.baking {
            return None;
        }
        // The bake ALWAYS reads the document's PRISTINE media, never the last save: the
        // saved file already has the scene burned in, and baking it again would double every
        // annotation. That is `bake_source()`, not `path` — after a still has been saved over
        // its own capture the pristine bytes live in a runtime-dir snapshot instead
        // (DRAGON-467 review, blocker 1; see `edit::bake_prep`).
        let src = p.bake_source()?.to_path_buf();
        let covermark = p.edit.covermark.clone();
        // Annotations are IMAGES only (a video preview never accumulates them).
        let annotations = p.edit.annotations.clone();
        // The curve radius is a POINT preset baked into SOURCE-px geometry (DRAGON-383);
        // identity on an unscaled (1x) output.
        let annot_curve = super::annotate::points_to_source_px(p.edit.curve_radius(), p.source_scale);
        let dim = p.edit.dim;
        // The committed crop (DRAGON-382; IMAGES only) — applied as the bake's final step.
        let crop = p.edit.crop;
        let is_video = matches!(p.kind, PreviewKind::Video(_));
        let video = match &p.kind {
            PreviewKind::Image(_) => None,
            // A video bake needs the probed metadata (overlay raster size, audio
            // presence for the cut graph). `run_share` already REFUSED the action when it
            // is missing (DRAGON-398), so this `?` is now unreachable defence rather than
            // the silent share-unedited fallback it used to be.
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
        // Always a throwaway temp: Copy and Share (DRAGON-474) are the baking intents that
        // reach here, and both leave whatever is on disk alone. `clipboard_temp_name`'s
        // `-copy` marker is what stops this colliding with `src` when the capture itself
        // lives in the runtime directory ("Automatically save originals" off).
        let dst = PathBuf::from(crate::util::runtime_dir())
            .join(clipboard_temp_name(&src, is_video));
        p.edit.baking = true;
        p.edit.pending_output = Some(dst.clone());
        p.edit.processing_msg = processing_msg;
        let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            // DRAGON-353: no desktop "Processing capture" notification — the editor is
            // still on screen showing its own spinner over the picture.
            let result = match &video {
                Some(v) => edit::bake_video(&src, &dst, covermark.as_ref(), v),
                None => {
                    edit::bake_image(&src, &dst, covermark.as_ref(), &annotations, annot_curve, dim, crop)
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

    /// THE completion seam a copy lands on — after a bake (`baked` = the file the bake wrote)
    /// or straight away when the document was clean (`baked` = `None`).
    ///
    /// Two steps: **copy, then close if one was armed**. The close only runs once the
    /// clipboard write has actually succeeded, which is the whole point of the ordering.
    ///
    /// It used to run save / copy / delete / close and take a `ShareIntent` saying which.
    /// Save moved to the destination picker (its bookkeeping now lives in
    /// `PreviewMsg::SaveAsBaked`, one implementation rather than a lost one), and Delete left
    /// the editor with the whole delete feature, so what is left is the copy.
    pub(super) fn finish_copy(
        &mut self,
        id: window::Id,
        baked: Option<PathBuf>,
    ) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for(id) else { return Task::none() };
        let is_video = matches!(p.kind, PreviewKind::Video(_));
        // The dialog's Save button — and the exit path — ask for a close once the action
        // lands. Read, NOT taken: a failure below hands the flag to `fail_close_action`,
        // which needs to know a close was requested in order to re-raise the card. It is
        // cleared on the way out of every path that does not fail.
        let close_after = self.preview_for(id).is_some_and(|p| p.edit.close_after_share);

        let tasks: Vec<Task<cosmic::Action<Msg>>> = Vec::new();

        // ── 1. COPY ───────────────────────────────────────────────────────────────────
        // The document's file as it stands NOW, or the throwaway temp a dirty copy just
        // baked (or the artifact a previous bake left at this same state — see `run_copy`).
        let source = match &baked {
            Some(temp) => Some(temp.clone()),
            None => self.preview_current_file(id),
        };
        let copy_ok = match source {
            Some(src) => self.copy_to_clipboard_now(id, &src, is_video),
            None => {
                self.preview_toast_icon(id, ToastKind::Error, "Nothing to copy yet", "clipboard-x-symbolic");
                false
            }
        };
        // DRAGON-355: a failed COPY aborts the rest. Closing the editor when the state never
        // reached the clipboard destroys work over a failure the user can SEE.
        // `copy_to_clipboard_now` (or the "Nothing to copy" arm above) already TOASTED the
        // miss; `fail_close_action` additionally re-raises the unsaved-changes card when a
        // close was armed AND the card actually answers the failure (DRAGON-467 review, minor
        // 6), so "Automatically copy changes on exit" cannot close the editor over a copy that
        // did not happen. Nothing was baked and no history moved, so retrying is live. Where a
        // platform cannot detect a late failure (the detached Linux worker's own write, in
        // another process) `copy_ok` is `true` and the flow proceeds exactly as before —
        // honest per platform, never faked (see `share::clipboard::copy_to_clipboard`).
        if !copy_ok {
            self.fail_close_action(id, COPY_FAILURE_REASON);
            return Task::batch(tasks);
        }

        // ── 2. CLOSE ──────────────────────────────────────────────────────────────────
        self.clear_close_intent(id);
        if !close_after {
            return Task::batch(tasks);
        }
        // Nothing reaches this point unasked. `close_after_share` has exactly two setters: the
        // unsaved-changes card's Save button (the card already asked) and the exit path (which
        // ran the ask gate itself, and whose setting may legitimately say "do not ask"). An
        // earlier `close_guards_unsaved` re-ran the card here for the old
        // close-on-copy-WITHOUT-save setting; with that setting gone it had no live input, and
        // re-asking would bounce the card off itself in the first case and override the user's
        // setting in the second. Bring it back only if some future caller can arm a close
        // WITHOUT having asked first.
        //
        // The close is IMMEDIATE (DRAGON-371). A close-after-copy used to sit here for a
        // second purely so its success toast could be read; the user's instruction was "make
        // it as fast as possible - just dont exit if the copy/delete/whatever failed", and the
        // not-exiting half is not a hold's job at all — the failure branch above already
        // returned early with the editor still up. On success the surface going away IS the
        // feedback.
        let mut tasks = tasks;
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

    // DRAGON-467 removed `delete_owned_files` from here. It unlinked
    // `PreviewState::delete_paths` (the capture plus every path the document wrote) and
    // toasted the outcome, reporting a PARTIAL failure as an `Err` so the caller would keep
    // the editor up over whatever survived. It went with the delete feature itself.
}

/// The reason handed to [`App::fail_close_action`] when a copy step aborts.
///
/// A failed copy stops everything after it, and that veto IS the "don't exit if it failed"
/// rule (DRAGON-371): `finish_copy` returns early with the surface still live, so retrying
/// stays reachable. It is not a timed hold, and a hold would not have helped — the hold that
/// used to sit there only ever held open on SUCCESS.
///
/// # How much the veto can actually promise, per platform
///
/// The `copy_ok` it reads is honest per platform (see `share::clipboard::copy_to_clipboard`):
/// macOS/Windows report the real synchronous pasteboard result, so there the guarantee is
/// exactly what it sounds like. **On LINUX it is weaker**: `copy_ok` reports only whether the
/// detached selection worker could be SPAWNED. A worker that spawns and then fails inside its
/// own process reads as success here — it happens after we have let go, with no channel back,
/// so it is undetectable rather than ignored. So on Linux "we never close over a failed copy"
/// holds for every failure this side can SEE, which is not every failure there is.
/// Pre-existing and not introduced by DRAGON-371; recorded here because this is where the
/// promise is made.
///
/// The reassuring corollary of that same detachment: closing INSTANTLY is safe for the
/// clipboard on Linux. The worker is a separate process that owns the Wayland selection until
/// something else is copied, so the copied data outlives the editor (and this one-shot
/// process) by construction — there is nothing for a hold to protect.
pub(super) const COPY_FAILURE_REASON: &str =
    "The capture couldn't be put on the clipboard, so nothing was copied.";

/// Does the unsaved-changes CARD actually answer this failure (DRAGON-467 review, minor 6)?
///
/// Yes only when there is unsaved work AND the user wants to be asked about it. The card
/// says "your edits haven't been written to a file yet" and offers Save / Copy / Delete /
/// Exit anyway, so raising it over a document with nothing to lose states something false,
/// and raising it when "Ask to save edited …" is OFF re-asks a question the setting already
/// answered. In both cases the toast the action already posted is the honest report, and the
/// editor simply stays up.
///
/// Pure; unit-tested below.
pub(super) fn card_answers_the_failure(ask_to_save: bool, unsaved: bool) -> bool {
    ask_to_save && unsaved
}

/// DRAGON-398: must a share be REFUSED because the bake it owes cannot be produced?
///
/// Yes exactly when something is owed and the media cannot render it. The only real
/// instance is a VIDEO whose ffprobe metadata never landed (a probe timeout / failure —
/// DRAGON-106 — leaves `VideoPreview::meta` at `None`, and [`edit::VideoBake`] cannot be
/// built without it), plus the degenerate document that has no file at all.
///
/// # Why this needs to be its own rule
///
/// It closes a hole the image path never had, because an image's pixels are always
/// available. Before this, an unbakeable video made `begin_bake` return `None` — the SAME
/// answer as "there is nothing to bake" — and the share completed against the untouched
/// file wearing a SUCCESS toast: a Save that said "Already saved as …" while the cut was
/// dropped, and a Copy that put the unedited recording on the clipboard. That is precisely
/// the "a Copy that silently no-ops" the ticket rules out; the refusal says so instead.
///
/// It is deliberately gated on `owed` rather than on dirtiness: an UNCUT, un-covermarked
/// recording owes no bake, so a missing probe must not stop it being saved, copied or
/// deleted. Those paths never touch ffmpeg at all.
pub(super) fn bake_blocked(owed: bool, media_bakeable: bool) -> bool {
    owed && !media_bakeable
}

/// The runtime-dir FILENAME a clipboard copy is served from (DRAGON-398).
///
/// * **Images** get the fixed `cck-copy.png` — a still is written as PNG (DRAGON-455), so
///   the baked file says png rather than echoing whatever the source was named. Every
///   platform hands the clipboard IMAGE BYTES (Linux reads the file in the selection worker;
///   macOS writes `NSData`; Windows CF_DIBV5), so the name is never user-visible and a fixed
///   one keeps the runtime dir bounded no matter how many copies a session makes.
/// * **Videos** take the document's own stem plus `-copy`, because there the name IS
///   user-visible: every platform puts a recording on the clipboard as a FILE REFERENCE
///   (Linux a `text/uri-list` URI, macOS an `NSURL`, Windows CF_HDROP), so pasting into a
///   file manager or a chat client writes a file called whatever this returns. `cck-copy.mp4`
///   was a poor thing to hand someone; `Recording 2026-copy.mp4` says what it is.
///
/// **The `-copy` marker is load-bearing, not decoration** (DRAGON-467). The temp lives in the
/// session runtime directory, and since "Automatically save originals" can put the CAPTURE
/// there too, a temp named exactly like its source would BE its source: the bake would read
/// and write one file at once. The marker is what keeps them apart. (It replaced `-edited`,
/// which existed to separate the baked temp from a staged copy of the untouched original;
/// that staging retired with copy-on-delete.)
///
/// Always a single path component (`file_name`, never a directory), so the caller's
/// `runtime_dir().join(..)` can't be walked out of.
pub(super) fn clipboard_temp_name(src: &std::path::Path, is_video: bool) -> String {
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or(naming::PNG_EXT);
    if !is_video {
        return format!("cck-copy.{}", naming::PNG_EXT);
    }
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if stem.is_empty() {
        return format!("cck-copy.{ext}");
    }
    format!("{stem}-copy.{ext}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DRAGON-355: a failed copy aborts the rest of the action. A successful copy never
    /// aborts, and a non-copying intent is never gated on the clipboard at all.
    ///
    /// The case that matters most since DRAGON-467 is the EXIT copy: it arms a close around a
    /// plain `Copy`, so this veto is what stops "Automatically copy changes on exit" from
    /// closing the editor over a clipboard write that did not happen.
    #[test]
    fn the_copy_failure_reason_is_truthful_and_dash_free() {
        // It names what did NOT happen, so a re-raised card is truthful about the clipboard.
        assert!(COPY_FAILURE_REASON.contains("clipboard"));
        assert!(COPY_FAILURE_REASON.contains("nothing was copied"));
        // It must NOT claim anything about files: since DRAGON-467 a failed copy has no
        // destructive half to have spared, so mentioning one would be a leftover lie.
        assert!(!COPY_FAILURE_REASON.contains("file was kept"));
        // The runtime-string house rule: no em/en-dashes in user-facing copy.
        assert!(
            !COPY_FAILURE_REASON.contains('\u{2014}') && !COPY_FAILURE_REASON.contains('\u{2013}'),
            "no em/en-dash in {COPY_FAILURE_REASON:?}"
        );
    }

    /// DRAGON-467 review, minor 6: the unsaved-changes card is raised only when it is TRUE
    /// and WANTED. It claims "your edits haven't been written to a file yet" and offers the
    /// committing actions, so it answers a failure only where there is unsaved work and the
    /// user asked to be asked about it.
    ///
    /// The case that motivated the rule: a failed EXIT COPY. That path arms a close on a
    /// document that may be perfectly saved, or whose owner turned the ask off, and raising
    /// the card there both lied and re-asked a settled question.
    #[test]
    fn the_unsaved_card_is_raised_only_when_it_is_true_and_wanted() {
        assert!(card_answers_the_failure(true, true), "unsaved work the user wants asked about");
        assert!(
            !card_answers_the_failure(true, false),
            "a SAVED document has no unsaved changes to report"
        );
        assert!(
            !card_answers_the_failure(false, true),
            "ask-to-save off means the question is already settled"
        );
        assert!(!card_answers_the_failure(false, false));
    }

    /// DRAGON-398: a share that OWES a bake the media cannot produce is refused; one that owes
    /// nothing is never gated on it.
    ///
    /// The second half is the important one for video. An UNCUT, un-covermarked recording owes
    /// no bake, so a failed ffprobe must not stop it being saved or copied — those paths never
    /// invoke ffmpeg at all, which is the same reason they stay byte-identical.
    #[test]
    fn a_bake_that_cannot_run_blocks_only_the_actions_that_need_it() {
        // Owed + unbakeable (a recording whose probe never landed) => refuse.
        assert!(bake_blocked(true, false));
        // Owed + bakeable => proceed, which is every normal edited share.
        assert!(!bake_blocked(true, true));
        // Nothing owed => never blocked, bakeable or not. The uncut-video case.
        assert!(!bake_blocked(false, false));
        assert!(!bake_blocked(false, true));
    }

    /// DRAGON-398: a CLEAN document is never refused, however unreadable its media. It owes
    /// no render, so the copy path never invokes ffmpeg at all — which is what keeps a
    /// recording whose ffprobe failed still copyable as it stands.
    #[test]
    fn a_clean_document_is_never_refused_for_unreadable_media() {
        assert!(bake_blocked(true, false), "a dirty document with unrenderable media is refused");
        assert!(!bake_blocked(false, false), "a clean one is not, and copies the file as it is");
    }

    /// DRAGON-467: the Video Editor group's toggles cannot get PAST the DRAGON-398 refusal.
    ///
    /// The dangerous combination is the EXIT copy: with "Automatically copy changes on exit"
    /// on, closing a dirty recording whose probe never landed must not quietly close over the
    /// unedited bytes. It routes through a plain `Copy`, so this walks every one of the eight
    /// video-setting combinations and checks that whatever the settings say, the intent a
    /// close can reach is refused WHOLE for a dirty, unsaved, unbakeable recording —
    /// `run_share` returns before the bake, nothing is half-completed, and the editor stays up
    /// with the reason. Delete is the deliberate survivor: it owes no bake and must stay
    /// available or a corrupt recording is undeletable.
    #[test]
    fn the_video_toggles_cannot_bypass_the_unbakeable_refusal() {
        use super::super::{PreviewAutomation, preview_automation};
        for bits in 0..8u8 {
            let vid = PreviewAutomation {
                copy_on_exit: bits & 1 != 0,
                save_originals: bits & 2 != 0,
                ask_to_save: bits & 4 != 0,
            };
            // The image triple is deliberately the OPPOSITE throughout: a video document must
            // resolve from `vid` alone, so nothing here can be an image setting in disguise.
            let img = PreviewAutomation {
                copy_on_exit: !vid.copy_on_exit,
                save_originals: !vid.save_originals,
                ask_to_save: !vid.ask_to_save,
            };
            let a = preview_automation(true, img, vid);
            assert_eq!(a, vid);
            // The copy (the toolbar's, and the exit path's) is always refused for an
            // unrenderable dirty recording, whatever the settings say: nothing is copied and
            // nothing closes.
            assert!(
                bake_blocked(true, false),
                "a dirty copy must be refused, not silently completed against the unedited file"
            );
            // A clean one is never gated, so an unreadable recording stays copyable as it is.
            assert!(!bake_blocked(false, false));
        }
    }

    /// Clipboard temp naming (DRAGON-398, re-marked by DRAGON-467). Images keep the fixed
    /// `cck-copy.png` (their bytes go on the clipboard, so the name is never seen and a fixed
    /// one bounds the runtime dir); VIDEOS carry the document's own stem plus `-copy`, because
    /// every platform puts a recording on the clipboard as a FILE REFERENCE and that name is
    /// what a paste writes to disk.
    #[test]
    fn a_copied_recording_pastes_under_its_own_name_and_a_still_does_not() {
        use std::path::Path;
        // Images: one fixed name, and it SAYS png — DRAGON-455, because png is what the baked
        // file contains whatever the source was called. An external `--preview a.JPG` used to
        // echo its name into `cck-copy.JPG`.
        assert_eq!(clipboard_temp_name(Path::new("/shots/a.png"), false), "cck-copy.png");
        assert_eq!(clipboard_temp_name(Path::new("/shots/a.JPG"), false), "cck-copy.png");
        // Videos: the document's own name plus the `-copy` marker.
        let rec = Path::new("/rec/Recording 2026.mp4");
        assert_eq!(clipboard_temp_name(rec, true), "Recording 2026-copy.mp4");
        // The extension rides along whatever it is.
        assert_eq!(clipboard_temp_name(Path::new("/rec/t.mkv"), true), "t-copy.mkv");
        assert_eq!(clipboard_temp_name(Path::new("/rec/t.webm"), true), "t-copy.webm");
        // THE collision the marker exists for: a capture that lives in the runtime dir
        // because "Automatically save originals" is off. The temp must not be its source, or
        // the bake reads and writes one file.
        let transient = Path::new("/run/user/1000/Recording 2026.mp4");
        assert_ne!(
            clipboard_temp_name(transient, true),
            transient.file_name().unwrap().to_string_lossy(),
            "the bake temp must never be the file it bakes FROM"
        );
        // A path with no file name at all falls back rather than producing an empty name.
        assert_eq!(clipboard_temp_name(Path::new("/"), true), "cck-copy.png");
        // Always ONE path component: a `join` onto the runtime dir can't be walked out of.
        for (p, v) in [("/rec/a/b.mp4", true), ("/shots/a/b.png", false)] {
            let name = clipboard_temp_name(Path::new(p), v);
            assert_eq!(Path::new(&name).components().count(), 1, "{name}");
        }
    }

}
