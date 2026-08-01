use super::*;
// `handle_key`/`preview_modal_key` dispatch through `update` (a `cosmic::Application`
// trait method, not an inherent one) — bring the trait into scope, same as
// application.rs does for the same reason.
use cosmic::Application as _;

impl App {
    /// Map a keyboard [`crate::shortcuts::Action`] to the message that performs it.
    ///
    /// `preview` is the preview document a Preview-context action is ADDRESSED to (see
    /// [`Self::focused_preview_id`]). A Preview action with no open preview has no target,
    /// so this returns `None` — the press is simply swallowed, exactly as it was when the
    /// old singular `preview` field was `None`.
    fn action_msg(action: crate::shortcuts::Action, preview: Option<window::Id>) -> Option<Msg> {
        use crate::shortcuts::Action;
        // Every Preview-domain arm needs the target id; bail once instead of per arm.
        let pv = |m: PreviewMsg| preview.map(|id| Msg::Preview(id, m));
        Some(match action {
            Action::CopyText => Msg::Detect(DetectMsg::TextCopy),
            Action::SelectAllText => Msg::Detect(DetectMsg::TextSelectAll),
            Action::DeselectText => Msg::Detect(DetectMsg::TextDeselect),
            Action::PreviewSave => return pv(PreviewMsg::Save),
            Action::PreviewSaveAs => return pv(PreviewMsg::SaveAs),
            Action::PreviewCopy => return pv(PreviewMsg::Copy),
            Action::PreviewPlay => return pv(PreviewMsg::Play),
            Action::PreviewFramePrev => return pv(PreviewMsg::FrameStep(-1)),
            Action::PreviewFrameNext => return pv(PreviewMsg::FrameStep(1)),
            Action::PreviewDelete => return pv(PreviewMsg::Delete),
            Action::PreviewCancel => return pv(PreviewMsg::Cancel),
            Action::PreviewCovermark => return pv(PreviewMsg::Covermark),
            Action::PreviewUndo => return pv(PreviewMsg::Undo),
            Action::PreviewRedo => return pv(PreviewMsg::Redo),
            Action::PreviewDeleteSegment => return pv(PreviewMsg::TimelineDelete),
            Action::PreviewAnnotPointer => return pv(PreviewMsg::SelectTool(
                crate::widgets::annotation_canvas::Tool::Pointer,
            )),
            Action::PreviewSelectAll => return pv(PreviewMsg::SelectAllAnnotations),
            // Deselect-all (DRAGON-369) REPLACES the whole selection with nothing — the same
            // message the canvas's empty-click and Esc use, so there is one deselect path.
            Action::PreviewDeselectAll => return pv(PreviewMsg::SelectAnnotation(None)),
            Action::PreviewAnnotArrow => {
                return pv(PreviewMsg::SelectTool(crate::widgets::annotation_canvas::Tool::Arrow))
            }
            Action::PreviewAnnotBadge => {
                return pv(PreviewMsg::SelectTool(crate::widgets::annotation_canvas::Tool::Badge))
            }
            Action::PreviewAnnotBox => {
                return pv(PreviewMsg::SelectTool(crate::widgets::annotation_canvas::Tool::Rect))
            }
            Action::PreviewAnnotHighlight => return pv(PreviewMsg::SelectTool(
                crate::widgets::annotation_canvas::Tool::Highlight,
            )),
            Action::PreviewAnnotBoxHighlight => return pv(PreviewMsg::SelectTool(
                crate::widgets::annotation_canvas::Tool::BoxHighlight,
            )),
            Action::PreviewAnnotPixelate => return pv(PreviewMsg::SelectTool(
                crate::widgets::annotation_canvas::Tool::Pixelate,
            )),
            Action::PreviewAnnotBlur => return pv(PreviewMsg::SelectTool(
                crate::widgets::annotation_canvas::Tool::Blur,
            )),
            Action::PreviewAnnotSpotlight => return pv(PreviewMsg::SelectTool(
                crate::widgets::annotation_canvas::Tool::Spotlight,
            )),
            Action::PreviewAnnotPen => {
                return pv(PreviewMsg::SelectTool(crate::widgets::annotation_canvas::Tool::Pen))
            }
            Action::PreviewAnnotText => {
                return pv(PreviewMsg::SelectTool(crate::widgets::annotation_canvas::Tool::Text))
            }
            Action::PreviewAnnotEraser => return pv(PreviewMsg::SelectTool(
                crate::widgets::annotation_canvas::Tool::Eraser,
            )),
            // The one-key tool SLOTS (DRAGON-369): the action IS the slot's identity, so both
            // arms carry it through unchanged and `cycle_tool_slot` reads the tray for the rest.
            Action::PreviewAnnotRedactCycle | Action::PreviewAnnotShapeCycle => {
                return pv(PreviewMsg::CycleToolSlot(action))
            }
            Action::PreviewAnnotDuplicate => return pv(PreviewMsg::DuplicateSelected),
            Action::PreviewAnnotStrokeCycle => return pv(PreviewMsg::CycleAnnotStrokeW),
            // DRAGON-385: C starts/confirms a crop session exactly like the toolbar button
            // (`CropEnter` both opens and applies). A live session owns the keyboard, so a
            // mid-session press never reaches here (see `preview_modal_key`) — no double-start.
            Action::PreviewCrop => return pv(PreviewMsg::CropEnter),
            Action::PreviewColorFlyout => return pv(PreviewMsg::ToggleAnnotPalette),
            Action::PreviewColorCompanionSwap => return pv(PreviewMsg::AnnotColorCompanionSwap),
            // DRAGON-392: H arms the HAND TOOL through the ordinary tool path (it flipped a pan
            // MODE before), so the toolbar button and the key can't disagree about what is armed.
            Action::PreviewAnnotHand => {
                return pv(PreviewMsg::SelectTool(crate::widgets::annotation_canvas::Tool::Hand))
            }
            Action::RecordStop => Msg::Recording(RecordingMsg::StopRecording),
            Action::RecordToggleMic => Msg::Recording(RecordingMsg::ToggleMic),
            Action::RecordToggleSystemAudio => Msg::Recording(RecordingMsg::ToggleSystemAudio),
        })
    }

    /// Resolve a raw key press against the live keymap: feed a rebind capture if one
    /// is in progress, otherwise dispatch the matched action.
    /// `window` is the surface the press was delivered to (forwarded from the event
    /// subscription), which is what picks the preview document a Preview-context binding
    /// acts on — see the focus note at the modal branch below.
    ///
    /// `location` (DRAGON-364) is the press's PHYSICAL key group, forwarded untouched to the
    /// live text-annotation editor — the one place that needs to tell numpad Enter from main
    /// Enter, which the logical `key` alone cannot. Nothing else consults it: the keymap binds
    /// LOGICAL keys, so every lane below is byte-identical to before.
    pub(super) fn handle_key(
        &mut self,
        window: window::Id,
        modifiers: cosmic::iced::keyboard::Modifiers,
        key: cosmic::iced::keyboard::Key,
        location: cosmic::iced::keyboard::Location,
        text: Option<String>,
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
        // A preview editor swallows the keyboard: only its (Preview-context) keybinds
        // fire while one is up. With SEVERAL previews open (DRAGON-336 phase 2) the old
        // "the preview is modal" framing becomes "the FOCUSED preview owns the keyboard":
        // the press goes to the preview the event was delivered TO when that surface is a
        // preview, and otherwise to the last-focused one. `focused_preview_id` falls back
        // to the most recently opened preview, so with exactly one open this resolves to
        // that preview for every source window — byte-identical to the old gate.
        if let Some(target) = self
            .preview_for(window)
            .map(|p| p.window)
            .or_else(|| self.focused_preview_id())
        {
            self.note_preview_focus(target);
            return self.preview_modal_key(target, modifiers, key, location, text);
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
        if let Some(action) = self.keymap.action_for(Context::Recording, modifiers, &key)
            && let Some(msg) = Self::action_msg(action, self.focused_preview_id())
        {
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
        // DRAGON-451: a Region lane sat here, checked BEFORE the Overlay lane so that with a
        // region drawn, primary+C meant "copy the drawn region and finish" instead of the
        // OCR "copy recognized text". That quick-action is retired — the global "(no editor)"
        // hotkeys cover it from outside the overlay — so primary+C now always reaches the
        // Overlay lane below, which is what it already did whenever no region was drawn.
        //
        // "Search settings" is a FIXED, non-configurable shortcut (DRAGON-158):
        // Ctrl+F on Linux/Windows, Cmd+F on macOS. It is not part of the editable
        // keymap, so match it here directly (the handler no-ops unless the settings
        // window is open). The primary command modifier is logo on macOS, Ctrl
        // elsewhere — matching the app's other primary-chord defaults.
        if Self::is_search_shortcut(modifiers, &key) {
            return self.update(Msg::WindowChrome(WindowChromeMsg::ConfigSearchActivate));
        }
        match self
            .keymap
            .action_for(Context::Overlay, modifiers, &key)
            .and_then(|action| Self::action_msg(action, self.focused_preview_id()))
        {
            Some(msg) => self.update(msg),
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

    /// macOS (DRAGON-361): open the Emoji & Symbols palette against preview `id`'s own native
    /// view. Routed by window IDENTITY through `window::run_with_handle(id, ..)` — the same
    /// route `focus_preview_for_text_edit` uses and for the same reason (DRAGON-336 allows
    /// several simultaneous previews, so a title scan could target the WRONG document) — whose
    /// callback runs on the winit event loop with that exact window's raw handle; the AppKit
    /// handle's `ns_view` IS the WinitView in our winit fork, i.e. the `NSTextInputClient` the
    /// palette must insert into. The AppKit work (and the diagnostics) live in the mac seam.
    #[cfg(target_os = "macos")]
    fn show_character_palette(id: window::Id) -> Task<cosmic::Action<Msg>> {
        window::run_with_handle(id, |handle| {
            use window::raw_window_handle::RawWindowHandle;
            if let RawWindowHandle::AppKit(h) = handle.as_raw() {
                // SAFETY: the callback runs synchronously on the winit event loop (the main
                // thread) while the handle borrow of the live window is held — exactly the
                // seam's documented contract.
                unsafe { crate::platform::mac::window::show_character_palette(h.ns_view) };
            }
        })
        .discard()
    }

    /// Key handling while the post-capture preview is open — modal, in priority
    /// order: a bake in progress holds every input; then the overwrite-confirm
    /// dialog; then the covermark picker; otherwise the preview's own keymap
    /// context. Only called from `handle_key` once `id` is known to be an OPEN preview
    /// (the focused one); every message it produces is addressed to that document.
    fn preview_modal_key(
        &mut self,
        id: window::Id,
        modifiers: cosmic::iced::keyboard::Modifiers,
        key: cosmic::iced::keyboard::Key,
        location: cosmic::iced::keyboard::Location,
        text: Option<String>,
    ) -> Task<cosmic::Action<Msg>> {
        use crate::shortcuts::Context;
        use cosmic::iced::keyboard::{key::Named, Key};
        let Some(p) = self.preview_for(id) else {
            return Task::none();
        };
        // While a bake is committing edits, every input is held (the pending
        // share continues on its own when the bake lands).
        if p.edit.baking {
            return Task::none();
        }
        // A live CROP session (DRAGON-382) OWNS the keyboard, like a text edit: Enter accepts,
        // Escape cancels, and every tool/action hotkey is SWALLOWED so it can't leak past the
        // modal crop UI. Highest priority after the bake hold.
        if p.edit.crop_session.is_some() {
            return match &key {
                Key::Named(Named::Enter) => self.update(Msg::Preview(id, PreviewMsg::CropAccept)),
                Key::Named(Named::Escape) => self.update(Msg::Preview(id, PreviewMsg::CropCancel)),
                _ => Task::none(),
            };
        }
        // macOS (DRAGON-361): while a text annotation is being edited, the Emoji & Symbols
        // chord (⌃⌘Space) is handled BY US — we open the Character Viewer directly rather
        // than waiting for the system to route its symbolic hotkey to an accessory-policy
        // app that owns no menu bar (see `platform::mac::window::show_character_palette` for
        // the summon-vs-delivery story; delivery is the winit fork's out-of-band
        // `insertText:` → `Ime::Commit` conversion, DRAGON-380). Scoped to a LIVE text edit,
        // so outside one the chord is never swallowed; the `is_character_palette_chord`
        // predicate is portable + unit-tested, only the AppKit call behind it is mac-only.
        // Non-mac builds are byte-identical (nothing compiled).
        #[cfg(target_os = "macos")]
        if p.edit.text_edit.is_some()
            && crate::shortcuts::is_character_palette_chord(modifiers, &key)
        {
            return Self::show_character_palette(id);
        }
        // A live TEXT edit (DRAGON-354) OWNS the keyboard: printable keys type into the box,
        // editing keys move the caret / delete, Escape (or NUMPAD Enter — DRAGON-364; main Enter
        // still inserts a newline) settles — and every tool/action hotkey
        // is SUSPENDED (swallowed) so typing "b" adds a letter, not a box. Highest priority
        // after the bake hold, before the confirm-close and keymap dispatch below.
        if p.edit.text_edit.is_some() {
            return self.text_edit_key(id, modifiers, key, location, text);
        }
        // The unsaved-changes dialog (DRAGON-353) is modal: Enter takes the SAFE default
        // (save, then close), Esc keeps editing, everything else is swallowed. Esc
        // deliberately does NOT close the document — Esc is the very key that raised this
        // dialog, so treating it as "discard" would make a double-tap lose the edits.
        if p.edit.confirm_close {
            return match &key {
                Key::Named(Named::Enter) => {
                    self.update(Msg::Preview(id, PreviewMsg::SaveAndClose))
                }
                Key::Named(Named::Escape) => {
                    self.update(Msg::Preview(id, PreviewMsg::KeepEditing))
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
            return self.update(Msg::Preview(id, PreviewMsg::AnnotColorEditor(false)));
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
                return self.update(Msg::Preview(id, msg));
            }
        }
        // Annotation selection: when a shape is selected, Esc deselects (before falling
        // through to Close/Cancel) and Delete/Backspace removes it (before the timeline's
        // Delete). No selection → both fall through to their normal keymap actions.
        if self.preview_for(id).is_some_and(|p| !p.edit.sel.is_empty()) {
            match &key {
                Key::Named(Named::Escape) => {
                    return self.update(Msg::Preview(id, PreviewMsg::SelectAnnotation(None)));
                }
                Key::Named(Named::Delete) | Key::Named(Named::Backspace) => {
                    return self.update(Msg::Preview(id, PreviewMsg::DeleteSelected));
                }
                _ => {}
            }
        }
        match self
            .keymap
            .action_for(Context::Preview, modifiers, &key)
            .and_then(|action| Self::action_msg(action, Some(id)))
        {
            Some(msg) => self.update(msg),
            None => Task::none(),
        }
    }
}
