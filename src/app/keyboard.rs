use super::*;
// `handle_key`/`preview_modal_key` dispatch through `update` (a `cosmic::Application`
// trait method, not an inherent one) — bring the trait into scope, same as
// application.rs does for the same reason.
use cosmic::Application as _;

impl App {
    /// Map a keyboard [`crate::shortcuts::Action`] to the message that performs it.
    fn action_msg(action: crate::shortcuts::Action) -> Msg {
        use crate::shortcuts::Action;
        match action {
            Action::CopyText => Msg::Detect(DetectMsg::TextCopy),
            Action::SelectAllText => Msg::Detect(DetectMsg::TextSelectAll),
            Action::DeselectText => Msg::Detect(DetectMsg::TextDeselect),
            Action::RegionCopy => Msg::Capture(CaptureMsg::CopySelection),
            Action::PreviewSave => Msg::Preview(PreviewMsg::Save),
            Action::PreviewSaveAs => Msg::Preview(PreviewMsg::SaveAs),
            Action::PreviewCopy => Msg::Preview(PreviewMsg::Copy),
            Action::PreviewPlay => Msg::Preview(PreviewMsg::Play),
            Action::PreviewFramePrev => Msg::Preview(PreviewMsg::FrameStep(-1)),
            Action::PreviewFrameNext => Msg::Preview(PreviewMsg::FrameStep(1)),
            Action::PreviewDelete => Msg::Preview(PreviewMsg::Delete),
            Action::PreviewCancel => Msg::Preview(PreviewMsg::Cancel),
            Action::PreviewCovermark => Msg::Preview(PreviewMsg::Covermark),
            Action::PreviewUndo => Msg::Preview(PreviewMsg::Undo),
            Action::PreviewRedo => Msg::Preview(PreviewMsg::Redo),
            Action::PreviewDeleteSegment => Msg::Preview(PreviewMsg::TimelineDelete),
            Action::PreviewAnnotPointer => Msg::Preview(PreviewMsg::SelectTool(
                crate::widgets::annotation_canvas::Tool::Pointer,
            )),
            Action::PreviewSelectAll => Msg::Preview(PreviewMsg::SelectAllAnnotations),
            Action::PreviewAnnotArrow => {
                Msg::Preview(PreviewMsg::SelectTool(crate::widgets::annotation_canvas::Tool::Arrow))
            }
            Action::PreviewAnnotBox => {
                Msg::Preview(PreviewMsg::SelectTool(crate::widgets::annotation_canvas::Tool::Rect))
            }
            Action::PreviewAnnotHighlight => Msg::Preview(PreviewMsg::SelectTool(
                crate::widgets::annotation_canvas::Tool::Highlight,
            )),
            Action::PreviewAnnotBoxHighlight => Msg::Preview(PreviewMsg::SelectTool(
                crate::widgets::annotation_canvas::Tool::BoxHighlight,
            )),
            Action::PreviewAnnotPixelate => Msg::Preview(PreviewMsg::SelectTool(
                crate::widgets::annotation_canvas::Tool::Pixelate,
            )),
            Action::PreviewAnnotBlur => Msg::Preview(PreviewMsg::SelectTool(
                crate::widgets::annotation_canvas::Tool::Blur,
            )),
            Action::PreviewAnnotSpotlight => Msg::Preview(PreviewMsg::SelectTool(
                crate::widgets::annotation_canvas::Tool::Spotlight,
            )),
            Action::PreviewAnnotPen => {
                Msg::Preview(PreviewMsg::SelectTool(crate::widgets::annotation_canvas::Tool::Pen))
            }
            Action::PreviewAnnotEraser => Msg::Preview(PreviewMsg::SelectTool(
                crate::widgets::annotation_canvas::Tool::Eraser,
            )),
            Action::PreviewAnnotDuplicate => Msg::Preview(PreviewMsg::DuplicateSelected),
            Action::PreviewAnnotStrokeCycle => Msg::Preview(PreviewMsg::CycleAnnotStrokeW),
            Action::PreviewColorFlyout => Msg::Preview(PreviewMsg::ToggleAnnotPalette),
            Action::PreviewTogglePan => Msg::Preview(PreviewMsg::TogglePanMode),
            Action::RecordStop => Msg::Recording(RecordingMsg::StopRecording),
            Action::RecordToggleMic => Msg::Recording(RecordingMsg::ToggleMic),
            Action::RecordToggleSystemAudio => Msg::Recording(RecordingMsg::ToggleSystemAudio),
        }
    }

    /// Resolve a raw key press against the live keymap: feed a rebind capture if one
    /// is in progress, otherwise dispatch the matched action.
    pub(super) fn handle_key(
        &mut self,
        modifiers: cosmic::iced::keyboard::Modifiers,
        key: cosmic::iced::keyboard::Key,
    ) -> Task<cosmic::Action<Msg>> {
        use crate::shortcuts::Context;
        use cosmic::iced::keyboard::{key::Named, Key};
        // macOS (DRAGON-130) / Windows (DRAGON-237): the "Start Capture" global-hotkey row
        // records the next chord the same way an in-app rebind row does, but the captured
        // chord is serialized to the daemon's SPEC string and flows through `SetCaptureHotkey`
        // (persist + restart the daemon) rather than into the keymap. Takes priority
        // like the in-app rebind below; the two are never active at once (each row's
        // button cancels the other's mode by starting its own).
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(slot) = self.settings.capture_hotkey_rebinding {
            if key == Key::Named(Named::Escape) {
                self.settings.capture_hotkey_rebinding = None; // Esc cancels capture
                return Task::none();
            }
            if let Some(sc) = crate::shortcuts::Shortcut::from_event(modifiers, &key) {
                // Serialize to the daemon spec and route through the SAME message the
                // former text field used, so persist + daemon-restart are unchanged. The
                // captured chord targets the slot the armed row belongs to (DRAGON-295).
                self.settings.capture_hotkey_rebinding = None;
                let spec = sc.daemon_spec();
                return self.update(Msg::Settings(SettingsMsg::SetCaptureHotkey(slot, spec)));
            }
            // A bare modifier press: keep waiting for a real key.
            return Task::none();
        }
        // Rebinding capture (Keyboard Shortcuts page) takes priority.
        if let Some(action) = self.settings.rebinding {
            if key == Key::Named(Named::Escape) {
                self.settings.rebinding = None; // Esc cancels capture
            } else if let Some(sc) = crate::shortcuts::Shortcut::from_event(modifiers, &key) {
                self.keymap.set(action, sc);
                self.settings.rebinding = None;
                self.save_state();
            }
            // A bare modifier press: keep waiting for a real key.
            return Task::none();
        }
        // The post-capture preview is modal: only its (Preview-context) keybinds fire,
        // and everything else is swallowed while it's up.
        if self.preview.is_some() {
            return self.preview_modal_key(modifiers, key);
        }
        // The mic key does double duty: in push-to-talk mode, while recording, it's
        // HOLD-to-talk — the first press un-mutes the mic (auto-repeat presses ignored;
        // the release handler re-mutes). Otherwise it toggles the mic like the toolbar
        // button (handled by the Recording-context dispatch below).
        let is_mic_key = self
            .keymap
            .get(crate::shortcuts::Action::RecordToggleMic)
            .is_some_and(|sc| sc.matches(modifiers, &key));
        if is_mic_key && self.ptt_active() {
            // Push-to-talk: the mic key is HOLD-only, NEVER a toggle — even before a
            // recording starts, so holding it works as a live visual test (the mic
            // button lights while held). Mid-recording it also un-mutes the mic for the
            // held span. Auto-repeat presses are ignored via `ptt_held`.
            if !self.ptt_held {
                self.ptt_held = true;
                if self.recording.is_some() {
                    self.log_audio_toggle(crate::record::AudioChannel::Mic, true);
                }
            }
            return Task::none();
        }
        // The Recording controls (stop / toggle mic / toggle system audio) work in the
        // capture overlay whether or not a recording is running — so the mic / system
        // toggles are usable and reflected in the toolbar BEFORE you start, and work
        // the same with the in-frame toolbar or the system tray. Stop is a no-op when
        // idle.
        if let Some(action) = self.keymap.action_for(Context::Recording, modifiers, &key) {
            let msg = Self::action_msg(action);
            return self.update(msg);
        }
        // Otherwise the capture overlay. The single "Close" keybind (shared with the
        // preview, default Esc) dismisses the overlay; the rest are the overlay's own
        // OCR / settings-search binds.
        if self
            .keymap
            .get(crate::shortcuts::Action::PreviewCancel)
            .is_some_and(|sc| sc.matches(modifiers, &key))
        {
            return self.update(Msg::WindowChrome(WindowChromeMsg::Close));
        }
        // Region quick-actions (Copy selection) fire in region-draw mode. This lane is
        // checked BEFORE the Overlay lane so the shared primary+C prefers "copy the drawn
        // region" here over "copy recognized text" (Overlay/OCR) whenever a region is
        // actually drawn — the two live in SEPARATE contexts (Region vs Overlay) so
        // neither steals the other's bind, and only one is active at a time. With no
        // region drawn yet the lane is skipped and primary+C falls through to the OCR
        // copy (scanner) exactly as before, so nothing regresses.
        if self.mode == Mode::Region
            && self.normalized_region().is_some()
            && let Some(action) = self.keymap.action_for(Context::Region, modifiers, &key)
        {
            let msg = Self::action_msg(action);
            return self.update(msg);
        }
        // "Search settings" is a FIXED, non-configurable shortcut (DRAGON-158):
        // Ctrl+F on Linux/Windows, Cmd+F on macOS. It is not part of the editable
        // keymap, so match it here directly (the handler no-ops unless the settings
        // window is open). The primary command modifier is logo on macOS, Ctrl
        // elsewhere — matching the app's other primary-chord defaults.
        if Self::is_search_shortcut(modifiers, &key) {
            return self.update(Msg::WindowChrome(WindowChromeMsg::ConfigSearchActivate));
        }
        match self.keymap.action_for(Context::Overlay, modifiers, &key) {
            Some(action) => {
                let msg = Self::action_msg(action);
                self.update(msg)
            }
            None => Task::none(),
        }
    }

    /// Whether a keypress is the FIXED "Search settings" shortcut (Ctrl+F, or Cmd+F
    /// on macOS). Not user-configurable (DRAGON-158), so it lives outside the keymap.
    fn is_search_shortcut(
        modifiers: cosmic::iced::keyboard::Modifiers,
        key: &cosmic::iced::keyboard::Key,
    ) -> bool {
        use cosmic::iced::keyboard::Key;
        let is_f = matches!(key, Key::Character(c) if c.eq_ignore_ascii_case("f"));
        if !is_f {
            return false;
        }
        // The primary command modifier alone: Cmd (logo) on macOS, Ctrl elsewhere.
        #[cfg(target_os = "macos")]
        {
            modifiers.logo() && !modifiers.control() && !modifiers.alt() && !modifiers.shift()
        }
        #[cfg(not(target_os = "macos"))]
        {
            modifiers.control() && !modifiers.logo() && !modifiers.alt() && !modifiers.shift()
        }
    }

    /// Key handling while the post-capture preview is open — modal, in priority
    /// order: a bake in progress holds every input; then the overwrite-confirm
    /// dialog; then the covermark picker; otherwise the preview's own keymap
    /// context. Only called from `handle_key` once `self.preview` is known `Some`.
    fn preview_modal_key(
        &mut self,
        modifiers: cosmic::iced::keyboard::Modifiers,
        key: cosmic::iced::keyboard::Key,
    ) -> Task<cosmic::Action<Msg>> {
        use crate::shortcuts::Context;
        use cosmic::iced::keyboard::{key::Named, Key};
        let Some(p) = &self.preview else {
            return Task::none();
        };
        // While a bake is committing edits, every input is held (the pending
        // share continues on its own when the bake lands).
        if p.edit.baking {
            return Task::none();
        }
        // The overwrite-confirmation dialog is modal: Enter overwrites, Esc
        // cancels, everything else is swallowed.
        if p.edit.confirm_overwrite {
            return match &key {
                Key::Named(Named::Enter) => {
                    self.update(Msg::Preview(PreviewMsg::ConfirmOverwrite))
                }
                Key::Named(Named::Escape) => {
                    self.update(Msg::Preview(PreviewMsg::CancelOverwrite))
                }
                _ => Task::none(),
            };
        }
        // The custom color-wheel picker is modal: Esc closes it — before the shared flyout /
        // keymap dispatch. Enter is left to the picker's own hex/RGB text input (its submit
        // commits the typed value); the Apply button applies.
        if p.edit.annot_picker.is_some()
            && let Key::Named(Named::Escape) = &key
        {
            return self.update(Msg::Preview(PreviewMsg::AnnotColorEditor(false)));
        }
        // The SHARED keyboard-navigable flyout dispatch (covermark picker OR color palette):
        // arrows move the highlight, Enter applies, Esc closes — before the keymap sees it.
        if p.edit.flyout.is_some() {
            let msg = match &key {
                Key::Named(Named::ArrowLeft) | Key::Named(Named::ArrowUp) => {
                    Some(PreviewMsg::FlyoutNav(-1))
                }
                Key::Named(Named::ArrowRight) | Key::Named(Named::ArrowDown) => {
                    Some(PreviewMsg::FlyoutNav(1))
                }
                Key::Named(Named::Enter) => Some(PreviewMsg::FlyoutApply),
                Key::Named(Named::Escape) => Some(PreviewMsg::FlyoutClose),
                _ => None,
            };
            if let Some(msg) = msg {
                return self.update(Msg::Preview(msg));
            }
        }
        // Annotation selection: when a shape is selected, Esc deselects (before falling
        // through to Close/Cancel) and Delete/Backspace removes it (before the timeline's
        // Delete). No selection → both fall through to their normal keymap actions.
        if self.preview.as_ref().is_some_and(|p| !p.edit.sel.is_empty()) {
            match &key {
                Key::Named(Named::Escape) => {
                    return self.update(Msg::Preview(PreviewMsg::SelectAnnotation(None)));
                }
                Key::Named(Named::Delete) | Key::Named(Named::Backspace) => {
                    return self.update(Msg::Preview(PreviewMsg::DeleteSelected));
                }
                _ => {}
            }
        }
        match self.keymap.action_for(Context::Preview, modifiers, &key) {
            Some(action) => {
                let msg = Self::action_msg(action);
                self.update(msg)
            }
            None => Task::none(),
        }
    }
}
