use super::*;
// `handle_key`/`preview_modal_key` dispatch through `update` (a `cosmic::Application`
// trait method, not an inherent one) — bring the trait into scope, same as
// application.rs does for the same reason.
use cosmic::Application as _;

/// A directional key being held down (DRAGON-601).
///
/// The two instants are what [`crate::shortcuts::nudge_step_allowed`] needs, and nothing else
/// is stored: the DIRECTION is not kept, because a press in a new direction is a fresh tap
/// either way and re-arming on every press is simpler than comparing.
pub(super) struct NudgeHold {
    /// When the key went down. Auto-repeats are measured against this.
    pressed_at: std::time::Instant,
    /// When we last actually MOVED for this hold. Starts equal to `pressed_at`, because the
    /// press itself is the first step.
    last_step: std::time::Instant,
}

impl App {
    /// DRAGON-601: should this directional key event move anything, and record it if so.
    ///
    /// The effectful half of [`crate::shortcuts::nudge_step_allowed`]: it owns the clock and
    /// the hold state, asks the pure rule, and writes back only when the answer is yes. Both
    /// nudge consumers call it, so the picker's sample and the drawn region cannot end up with
    /// different ideas of what a tap costs.
    ///
    /// A press (or a repeat with no hold on record, which is what a focus change mid-hold
    /// looks like) RE-ARMS: the hold starts now, so the auto-repeat delay is measured from a
    /// press we actually saw rather than from one we missed.
    fn nudge_step_due(&mut self, repeat: bool) -> bool {
        let now = std::time::Instant::now();
        if crate::shortcuts::nudge_rearms(self.nudge_hold.is_some(), repeat) {
            self.nudge_hold = Some(NudgeHold { pressed_at: now, last_step: now });
            return true;
        }
        let Some(hold) = self.nudge_hold.as_mut() else {
            return true;
        };
        if !crate::shortcuts::nudge_step_allowed(
            true,
            now.saturating_duration_since(hold.pressed_at),
            now.saturating_duration_since(hold.last_step),
        ) {
            return false;
        }
        hold.last_step = now;
        true
    }

    /// DRAGON-612: what an accept key means in THIS session, right now.
    ///
    /// The effectful half of [`accept_verdict`]: it reads the state and asks the pure rule.
    /// Named at the call site because six positional arguments in a row is exactly the shape a
    /// later edit transposes two of, and the gate's own tests cannot catch a caller that
    /// passes them in the wrong order.
    fn accept_verdict(&self) -> AcceptOutcome {
        accept_verdict(
            /* on_overlay */ !self.outputs.is_empty(),
            /* busy */
            self.countdown.is_some() || self.recording.is_some() || self.capturing.is_some(),
            /* color_picking */ self.color_picking(),
            /* picker */
            picker_sample_state(
                self.color_picker.hover.is_some(),
                self.frozen_pending,
                self.color_picker.pointer.is_some(),
            ),
            /* mode */ self.mode,
            /* region_drawn */ self.normalized_region().is_some(),
        )
    }

    /// An accept has been ASKED FOR, by a key press or by the hold's own re-ask (DRAGON-612).
    ///
    /// The ONE place a request is resolved, so the answers cannot drift apart, and the one
    /// place the hold is armed and cleared. **Every branch is audible**: a deliberate keypress
    /// that does nothing and says nothing is a bug the user reports as "it does not work", and
    /// they are right. Each refusal reaches the debug log, and the two that can be SEEN also
    /// reach the overlay's banner.
    ///
    /// Neither firing branch invents a commit path. The region sends the message the toolbar's
    /// capture button sends and the pick sends the message a left click sends, so there is one
    /// way to finish a capture and one way to take a colour, whatever started it.
    pub(in crate::app) fn request_accept(&mut self) -> Task<cosmic::Action<Msg>> {
        match self.accept_verdict() {
            AcceptOutcome::Region => {
                self.accept_pending = None;
                log::debug!("DRAGON-612 accept: committing the drawn region from the keyboard");
                // The SAME message `toolbar`'s capture button sends. `capture` builds the
                // selection from the drawn region in region mode and ignores the output name
                // it is handed, which is why an empty one is honest here rather than a guess;
                // `CaptureMsg::CopySelection` passes `""` for the same reason.
                self.update(Msg::Capture(CaptureMsg::Capture { output: String::new() }))
            }
            AcceptOutcome::Picker => {
                self.accept_pending = None;
                let Some((output, point)) = self.color_picker.pointer.clone() else {
                    // Unreachable: `PickerSample::Ready` needs a hover, and a hover needs a
                    // recorded pointer. Written out rather than unwrapped so a later change to
                    // the classification degrades into a log line instead of a panic.
                    log::warn!(
                        "DRAGON-612 accept: the picker reported a ready sample with no pointer \
                         on record, so there is no point to take it from"
                    );
                    return Task::none();
                };
                log::debug!("DRAGON-612 accept: taking the picker's color from the keyboard");
                // The SAME message a left click on the picker overlay sends, carrying the same
                // RAW pointer point. `color_picker_pick` resolves the sample itself (the
                // keyboard nudge included), so the key and the click take the same pixel by
                // construction rather than by two call sites agreeing.
                self.update(Msg::ColorPicker(ColorPickerMsg::Pick(output, point)))
            }
            AcceptOutcome::Wait => {
                // Armed from the FIRST ask, never re-armed, so the budget measures the wait
                // the user has actually experienced rather than restarting on every re-ask
                // (which would be a wait with no bound at all, dressed as one that has one).
                let Some(since) = self.accept_pending else {
                    log::debug!(
                        "DRAGON-612 accept: holding the request, the picker has no pixel yet \
                         (snapshot in flight: {}, pointer known: {})",
                        self.frozen_pending,
                        self.color_picker.pointer.is_some()
                    );
                    self.accept_pending = Some(std::time::Instant::now());
                    return Task::none();
                };
                let waited_ms = since.elapsed().as_millis() as u64;
                if !accept_wait_expired(waited_ms) {
                    return Task::none();
                }
                // The bound. Reached only by a session that never settles, which in practice
                // means the frozen-flats grab thread died: `frozen_pending` then stays true
                // for good and no later tick could answer differently.
                self.accept_pending = None;
                log::warn!(
                    "DRAGON-612 accept: gave up after {waited_ms}ms, the picker never found a \
                     pixel (snapshot in flight: {}, pointer known: {})",
                    self.frozen_pending,
                    self.color_picker.pointer.is_some()
                );
                self.toast =
                    Some("The screen never finished loading, so no color could be taken".into());
                Task::none()
            }
            AcceptOutcome::Refuse(why) => {
                self.accept_pending = None;
                match why {
                    // VISIBLE. This is the case most likely to be read as the key being
                    // broken, and the copy names the FIX rather than the fault. It must never
                    // fall back to capturing the whole screen: `capture` would happily do
                    // nothing here, but a destructive surprise is worse than a refusal and a
                    // silent no is worse than both.
                    AcceptRefusal::NoRegion => {
                        log::debug!(
                            "DRAGON-612 accept: declined, region mode with nothing drawn yet"
                        );
                        self.toast = Some(
                            "Drag to draw a region first, then press Enter to capture it".into(),
                        );
                    }
                    // VISIBLE, and this is the one picker refusal that waiting cannot fix.
                    AcceptRefusal::NoPixelSource => {
                        log::warn!(
                            "DRAGON-612 accept: declined, the picker is over a display with no \
                             pixel source"
                        );
                        self.toast =
                            Some("This display cannot be read, so there is no color to take".into());
                    }
                    // LOG ONLY, and it is a decision rather than laziness. `countdown_view`
                    // and `recording_view` deliberately stack no banner, so the text would not
                    // be drawn while it was true, and it would still be SET, so cancelling a
                    // countdown (which
                    // returns to `overlay_view`, which DOES draw it) would tell the user a
                    // capture was under way about one they had just cancelled.
                    AcceptRefusal::Busy => {
                        log::debug!(
                            "DRAGON-612 accept: declined, the session is busy (countdown: {}, \
                             recording: {}, committed: {})",
                            self.countdown.is_some(),
                            self.recording.is_some(),
                            self.capturing.is_some()
                        );
                    }
                    // LOG ONLY, for the user: there is no banner on a window or monitor
                    // overlay worth nagging with, and nothing is ambiguous to resolve. The
                    // line carries the MODE so a later report of "Enter does nothing" is
                    // answerable from the debug log without a reproduction, which is the same
                    // reason the busy line above carries all three of its terms.
                    AcceptRefusal::NotHere => {
                        log::debug!(
                            "DRAGON-612 accept: declined, nothing accepts here (mode: {:?}, \
                             picking: {}, overlays: {})",
                            self.mode,
                            self.color_picking(),
                            self.outputs.len()
                        );
                    }
                }
                Task::none()
            }
        }
    }

    /// The message a BAKED editor action ([`crate::shortcuts::FixedEditorAction`]) performs on
    /// preview `id` (DRAGON-617).
    ///
    /// Deliberately the SAME messages the five retired `Action` arms dispatched. Baking
    /// changed where a chord is matched, never what it does, so every commit path behind them
    /// is untouched: the save flow with its destination picker, the upload FLYOUT (not an
    /// upload: that picks an account and cannot be undone, so a keystroke must not commit
    /// one), the window's own close-or-confirm decision, and the shared undo history.
    ///
    /// [`crate::shortcuts::FixedEditorAction::Close`] has a SECOND meaning on the capture
    /// overlay, where it dismisses the overlay instead. That is dispatched in `handle_key`
    /// rather than here, exactly as the retired `Action::PreviewCancel` was read from two
    /// places, because only this one has a preview document to address.
    fn fixed_editor_msg(action: crate::shortcuts::FixedEditorAction, id: window::Id) -> Msg {
        use crate::shortcuts::FixedEditorAction as Fixed;
        Msg::Preview(
            id,
            match action {
                Fixed::Save => PreviewMsg::Save,
                Fixed::Upload => PreviewMsg::UploadFlyoutToggle,
                Fixed::Close => PreviewMsg::Cancel,
                Fixed::Undo => PreviewMsg::Undo,
                Fixed::Redo => PreviewMsg::Redo,
            },
        )
    }

    /// The message a BAKED scanner action ([`crate::shortcuts::FixedScannerAction`]) performs
    /// (DRAGON-627).
    ///
    /// Deliberately the SAME messages the three retired `Action` arms dispatched, which are
    /// also the three the scanner overlay's context menu sends (`app::overlay::menus`).
    /// Baking changed where a chord is matched, never what it does.
    ///
    /// No preview id, unlike [`Self::fixed_editor_msg`]: these act on the capture overlay's
    /// recognized text, which belongs to the session rather than to a document.
    fn fixed_scanner_msg(action: crate::shortcuts::FixedScannerAction) -> Msg {
        use crate::shortcuts::FixedScannerAction as Scanner;
        Msg::Detect(match action {
            Scanner::Copy => DetectMsg::TextCopy,
            Scanner::SelectAll => DetectMsg::TextSelectAll,
            Scanner::Deselect => DetectMsg::TextDeselect,
        })
    }

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
            Action::PreviewCopy => return pv(PreviewMsg::Copy),
            Action::PreviewPlay => return pv(PreviewMsg::Play),
            Action::PreviewFramePrev => return pv(PreviewMsg::FrameStep(-1)),
            Action::PreviewFrameNext => return pv(PreviewMsg::FrameStep(1)),
            Action::PreviewCovermark => return pv(PreviewMsg::Covermark),
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
        repeat: bool,
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
                // **A chord DRAGON-617 baked is refused, out loud** (`is_fixed_editor_chord`).
                // Accepting one would store a binding that can never fire: the baked matcher
                // is asked before every keymap lane, so the row would show the user's key
                // while the key kept doing Undo. The capture stays ARMED, so the row still
                // reads "Press a key…" and the next press is an ordinary retry, and the row's
                // helper line says which key was refused and why.
                if crate::shortcuts::is_fixed_editor_chord(&sc) {
                    let label = sc.label();
                    log::debug!(
                        "DRAGON-617 rebind: refused {label} for {action:?}, it is a fixed \
                         shortcut"
                    );
                    self.settings.rebind_refused = Some((action, label));
                    return Task::none();
                }
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
        // that preview for every source window.
        //
        // **The fallback stops at a window of ours that owns its own keyboard.** Without
        // that term the fallback reached EVERY surface in the process, so with a preview
        // open a key typed into the settings search field, the permission checker or the
        // colour picker's hex row was routed into the preview's modal handler: Ctrl+Z in
        // the settings search undid a preview edit, and Escape there closed the document.
        // `sub_global_events` forwards presses with no `Status` filter, so the text field
        // having handled the key first is not something this can observe; the delivering
        // SURFACE is, and it is the honest signal.
        //
        // Skipping the fallback lets the press fall through to the lanes below, which is
        // exactly the path it already takes on the same window when NO preview is open.
        // So this removes a special case rather than adding one, and the two cases now
        // agree. A press delivered to a preview surface itself is unaffected, and so is
        // every capture overlay, which has no text input to protect.
        if let Some(target) = self.key_target_preview(window) {
            self.note_preview_focus(target);
            return self.preview_modal_key(target, modifiers, key, location, text);
        }
        // DRAGON-587: the colour picker's magnifier zoom, numpad `+` / `-`.
        //
        // Checked here, before every keymap lane, because the picker overlay owns the keyboard
        // for the seconds it is up and these keys mean nothing else while it is: a picker
        // launch mints no capture toolbar, has no region and no recording. Fixed rather than
        // bindable, for the reason `shortcuts::magnifier_zoom_step` documents. Gated on
        // `color_picking`, so no other launch shape sees any change at all.
        if self.color_picking()
            && let Some(steps) = crate::shortcuts::magnifier_zoom_step(&key, location)
        {
            return self.update(Msg::ColorPicker(ColorPickerMsg::Zoom(steps)));
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
        // Otherwise the capture overlay. The single fixed "Close" key (Escape, shared with
        // the preview) dismisses the overlay; the rest are the overlay's own OCR /
        // settings-search binds.
        //
        // DRAGON-617 baked Close, so this reads the matcher instead of the keymap. The key
        // and the message are unchanged; what went away is the ability to rebind it, and
        // with it the odd shape where the OVERLAY's close was configured through a row
        // labelled for the preview.
        if crate::shortcuts::fixed_editor_action(modifiers, &key)
            == Some(crate::shortcuts::FixedEditorAction::Close)
        {
            return self.update(Msg::WindowChrome(WindowChromeMsg::Close));
        }
        // **The region-copy chord** (DRAGON-479), checked BEFORE the scanner lane so that the
        // scanner-vs-region precedence for primary+C is ONE visible decision rather than two
        // scattered checks: `region_copy_fires` says when the region meaning wins, and its
        // `kind != Scanner` term is exactly what leaves the OCR copy
        // (`shortcuts::FixedScannerAction::Copy`, below) untouched in scanner kind.
        //
        // History: DRAGON-451 retired a CONFIGURABLE version of this (`Action::RegionCopy` and a
        // whole `Context::Region` keymap lane) on the grounds that the global "(no editor)"
        // hotkeys covered it. The owner wants the in-overlay chord back — it is the one that
        // works when you have already drawn the region — and DRAGON-479 restores the BEHAVIOUR
        // without the configurability that made the two meanings of Ctrl+C collide. See
        // `shortcuts::is_region_copy_chord` for why it is fixed rather than bindable.
        if crate::shortcuts::is_region_copy_chord(modifiers, &key)
            && region_copy_fires(
                self.mode,
                self.kind,
                self.normalized_region().is_some(),
                self.countdown.is_some() || self.recording.is_some(),
                // DRAGON-577: dead on the portal fallback (the copy cannot deliver
                // there); the Region tooltip hides the chord on the same predicate.
                self.overlay_fallback_active(),
            )
        {
            return self.update(Msg::Capture(CaptureMsg::CopySelection));
        }
        // "Search settings" is a FIXED, non-configurable shortcut (DRAGON-158):
        // Ctrl+F on Linux/Windows, Cmd+F on macOS. It is not part of the editable
        // keymap, so match it here directly (the handler no-ops unless the settings
        // window is open). The primary command modifier is logo on macOS, Ctrl
        // elsewhere — matching the app's other primary-chord defaults.
        if Self::is_search_shortcut(modifiers, &key) {
            return self.update(Msg::WindowChrome(WindowChromeMsg::ConfigSearchActivate));
        }
        // The scanner overlay's three RECOGNIZED-TEXT chords (DRAGON-627 baked them; they
        // were a `Context::Overlay` keymap lane read right here, and that context had no other
        // members, so it retired with them). Asked in the SAME position, so the precedence
        // above is untouched: Close, then region-copy, then settings-search, then these. The
        // messages are the ones the retired `Action` arms dispatched, and the ones the
        // overlay's own context menu sends.
        if let Some(action) = crate::shortcuts::fixed_scanner_action(modifiers, &key) {
            return self.update(Self::fixed_scanner_msg(action));
        }
        // **The directional keys** (DRAGON-599): the four arrows and the vim letters `h` `j`
        // `k` `l`. ONE key family, two consumers, chosen here by which one is on screen:
        // the colour picker's SAMPLE POINT during a pick, the drawn REGION otherwise. Both
        // take a `shortcuts::Direction` and neither can tell an arrow from a letter, which is
        // the whole reason that type exists.
        //
        // Placed in the FALL-THROUGH, after the Overlay keymap lane, which is the opposite of
        // where the region-copy chord above sits — and the difference is deliberate. That
        // chord had to be arbitrated because primary+C is a DEFAULT binding of another action
        // in this very context; these keys collide with nothing at all (every Overlay and
        // Recording default is a chord, pinned by
        // `shortcuts::nudge_direction_tests::no_overlay_or_recording_default_claims_a_bare_directional`).
        // With no collision to settle, the safe order is the one where a binding the user
        // configured always wins, and a bare arrow means "move" only when nothing else claimed
        // it. On a default install the two orders are indistinguishable.
        //
        // **Hold-to-move runs on OUR cadence, not the desktop's** (DRAGON-601). It used to run
        // on the desktop's: a held key arrives as repeated `KeyPressed` events, iced marks them
        // `repeat: true`, and this lane read every one of them as another pixel. The owner asked
        // for "1px per tap, but hold to move should be available too", and inheriting the
        // system's cadence cannot deliver the first half, because that cadence is tuned for TEXT
        // ENTRY: a desktop with a short repeat delay turns one deliberate tap into two or three
        // pixels, and the same build then nudges differently on two machines.
        //
        // So a PRESS always moves exactly once, and repeats are ignored until the key has been
        // held past our own `NUDGE_HOLD_DELAY`, then capped at our own `NUDGE_REPEAT_INTERVAL`.
        // The decision is pure and shared (`shortcuts::nudge_step_allowed`); `nudge_step_due`
        // owns the clock and the hold state. Asked ONCE here, before either consumer, so the
        // picker's sample and the drawn region can never disagree about what a tap costs.
        //
        // The gate is consulted only for keys that really are directional, so a non-directional
        // press can never disturb a hold in progress. This is the mirror image of what
        // push-to-talk does with `ptt_held`: there the repeats are noise to filter out, here
        // they are the feature, just re-timed.
        //
        // **The delivered-surface test is the same one the accept keys carry**, and this lane
        // needed it just as badly. These eight are BARE keys, and `h` `j` `k` `l` are ordinary
        // letters, so without it a letter typed into the settings search field moved the drawn
        // region. DRAGON-612 named the hazard exactly ("a press on a non-overlay surface of
        // ours was almost certainly typed INTO something, which handled it") and fixed it for
        // Enter and Space only; the same sentence was always true here.
        //
        // **The consequence, stated rather than left to be discovered:** nudging no longer
        // works while the colour picker's RESULT window holds focus, only while one of its
        // overlays does. That is the right answer on the same reasoning: the result window
        // carries the hex and RGB text rows, and a key typed into a window with text inputs
        // belongs to that window. The picker's own overlays ARE `self.outputs` entries (see
        // `mint_startup_pickers`, which mints one layer surface per registered output), so
        // picking and nudging on the overlay is untouched.
        //
        // Asked BEFORE `nudge_step_due`, which owns the hold state: a press we are not going
        // to answer must not re-arm or advance somebody else's hold, exactly as a
        // non-directional press does not.
        if let Some(dir) = crate::shortcuts::nudge_direction(modifiers, &key)
            && self.press_landed_on_overlay(window)
        {
            if !self.nudge_step_due(repeat) {
                // Too soon in a hold. A dropped step is a no-op, never a swallow of some other
                // meaning: on these surfaces a bare arrow means "move" or it means nothing.
                return Task::none();
            }
            // The picker first, because `color_picking` is also the term that keeps the region
            // lane out of a pick (`region_nudge_fires`): the two are exclusive by construction
            // rather than by ordering, and reading them in this order just says so out loud.
            if self.color_picking() {
                return self.update(Msg::ColorPicker(ColorPickerMsg::Nudge(dir)));
            }
            if region_nudge_fires(
                self.mode,
                self.color_picking(),
                self.normalized_region().is_some(),
                self.countdown.is_some() || self.recording.is_some(),
            ) {
                return self.update(Msg::Capture(CaptureMsg::NudgeRegion(dir)));
            }
        }
        // **The accept keys** (DRAGON-612): bare Enter and bare Space, which COMMIT what the
        // directional keys above were aiming. Without them the loop DRAGON-599 opened stays
        // half finished: the user nudges to an exact pixel and then has to reach for the
        // mouse, and moving the mouse is what undoes the precision they just achieved.
        //
        // Last in the FALL-THROUGH, for the reason the two lanes above give: bare Enter and
        // bare Space are the default of no action in any context that can reach here (the one
        // default naming Enter at all is `Action::RecordStop`, which is Shift+Alt+Enter, and
        // `Shortcut::matches` compares all four modifier flags exactly), so with no collision
        // to arbitrate the safe order is the one where a binding the user configured always
        // wins. Pinned by `shortcuts::accept_key_tests`.
        //
        // **The delivered-surface test is here, and it is the one thing these keys need that
        // no other fixed key does.** `sub_global_events` forwards every key press with NO
        // `Status` filter, on the stated principle that a key press means the same thing
        // whoever handled it. That principle held while every fixed binding was a modifier
        // chord. Enter and Space are not chords, and two TEXT INPUTS are reachable in this
        // process: the colour picker's RESULT window (its hex and RGB rows) and the settings
        // search field. Without this test a space typed into either would commit a capture or
        // take a colour.
        //
        // It is asked HERE rather than inside the gate on purpose. A press on a non-overlay
        // surface of ours was almost certainly typed INTO something, which handled it, so it
        // is not our press to answer and must not reach the log as a refusal; it falls through
        // to the same `Task::none()` it always did. The gate keeps its own weaker `on_overlay`
        // term for the re-ask, where the overlays can be gone by the time a held accept is
        // served.
        if crate::shortcuts::is_accept_key(modifiers, &key) && self.press_landed_on_overlay(window)
        {
            return self.request_accept();
        }
        Task::none()
    }

    /// Which preview a key press delivered to `window` is addressed to, or `None` when it
    /// is not the preview's press to answer. The effectful half of [`key_target`]: it reads
    /// the three window slots and the focus pointer, and the RULE lives in the pure function.
    fn key_target_preview(&self, window: window::Id) -> Option<window::Id> {
        key_target(
            self.preview_for(window).map(|p| p.window),
            self.owns_its_own_keyboard(window),
            self.focused_preview_id(),
        )
    }

    /// Whether this press was delivered to one of our capture OVERLAYS.
    ///
    /// **The delivery test the two BARE-key lanes share**, the accept keys (DRAGON-612) and
    /// the directional keys. Both answer keys that are ordinary typing on any surface with a
    /// text input, so both have to know that the press really landed on an overlay rather
    /// than being typed into something that already handled it. `sub_global_events` forwards
    /// every press with no `Status` filter, so the delivering surface is the only signal
    /// available, and it is a good one.
    ///
    /// One method rather than the predicate written out at both lanes: the two were allowed
    /// to disagree once already, and the nudge lane was the one that lost.
    ///
    /// Only the BARE-key lanes need it. A modifier chord is not typing, which is why the
    /// region-copy chord, the five baked editor chords and the three baked scanner chords
    /// (DRAGON-627) do not consult this.
    fn press_landed_on_overlay(&self, window: window::Id) -> bool {
        press_on_overlay(self.outputs.iter().map(|o| o.id), window)
    }

    /// Whether `window` is a toplevel of OURS that owns its own keyboard: the settings
    /// window, the permission checker, or the colour picker's result window.
    ///
    /// These three are exactly the surfaces in this process that carry TEXT INPUTS (the
    /// settings search field, the picker's hex and RGB rows), which is what makes them the
    /// set worth naming. A capture overlay is deliberately NOT here: it has no text input,
    /// and the fall-through lanes below are the whole point of an overlay press.
    ///
    /// A slot that is `None` (the window is not open) matches nothing, because
    /// `Some(window) == None` is false. So this is `false` for every ordinary session, and
    /// the sessions it is true for are precisely the ones that opened one of these windows.
    fn owns_its_own_keyboard(&self, window: window::Id) -> bool {
        Some(window) == self.settings.window
            || Some(window) == self.permissions.window
            || Some(window) == self.color_picker.window
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
        //
        // DRAGON-468: the Esc half is the pure, unit-tested
        // `annotate::escape_stage` — the ONE statement of "what does this press give up first",
        // shared with the live-text-edit lane above (`text_edit_key`), where the numpad-Enter twin
        // ends a typing session and drops the selections with it. The two lanes consult it
        // separately because the modal lanes between them (unsaved-changes dialog, colour picker,
        // flyout) own Escape first and must keep doing so. A `Pass` here reaches the BAKED
        // Close key (DRAGON-617; it was `Action::PreviewCancel` in the keymap before that),
        // i.e. the window's own close-or-confirm decision; nothing about that decision is
        // restated here.
        //
        // "Selected" spans BOTH selections: the annotation set and the video timeline's segments.
        // A text edit is always settled by now (its lane returns above), so `text_editing` is
        // false at this call site — it is passed honestly rather than hard-coded.
        let stage = self.preview_for(id).map(|p| {
            preview::escape_stage(
                &key,
                location,
                p.edit.text_edit.is_some(),
                !p.edit.sel.is_empty(),
                p.timeline_selected(),
            )
        });
        if stage == Some(preview::EscapeStage::Deselect) {
            return self.deselect_everything(id);
        }
        // Delete/Backspace stays the ANNOTATION lane: it removes the selected annotations before
        // the keymap's plain-Delete binding (`PreviewDeleteSegment`) can see the press. With no
        // annotations selected it falls through, and after an Escape has cleared everything that
        // binding finds an empty timeline selection and does nothing (`Timeline::delete_selected`
        // refuses an empty set) — no surprise deletion.
        if self.preview_for(id).is_some_and(|p| !p.edit.sel.is_empty())
            && matches!(&key, Key::Named(Named::Delete) | Key::Named(Named::Backspace))
        {
            return self.update(Msg::Preview(id, PreviewMsg::DeleteSelected));
        }
        // **The five BAKED editor chords** (DRAGON-617): Save, Upload, Close, Undo, Redo.
        // Save / Upload / Undo / Redo are dispatched HERE and nowhere else, so a press on a
        // surface with no preview open can never reach them, which is what keeps Ctrl+Z in
        // the colour picker's hex row and the settings search field the FIELD's undo.
        //
        // Asked BEFORE the keymap lane, so baked wins: see `shortcuts::fixed_editor_action`
        // for why this sits on the region-copy side of that choice rather than the
        // accept-key side. Asked AFTER every modal lane above, so nothing about who owns
        // Escape first changed: the crop session, the live text edit, the unsaved-changes
        // dialog, the colour wheel, the flyout and the annotation deselect all still answer
        // it before this does. This only replaces what the BOTTOM of this function resolves.
        if let Some(fixed) = crate::shortcuts::fixed_editor_action(modifiers, &key) {
            return self.update(Self::fixed_editor_msg(fixed, id));
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

/// **Pure**, unit-tested: did this press land on one of our capture overlays?
///
/// The rule [`App::press_landed_on_overlay`] applies, split out from the `self.outputs` read
/// so it can be tested with no compositor and no live surfaces. Membership, and nothing more:
/// an overlay that is open but not the delivering surface does not count, which is the whole
/// point, since "some overlay exists" is exactly the weaker question that let a keystroke
/// typed elsewhere move the region.
///
/// A session with NO overlays answers `false` for every window, which is the right answer for
/// a preview-only or picker-window-only process: there is nothing on screen for a bare
/// directional key or an accept key to act on.
fn press_on_overlay(
    overlays: impl IntoIterator<Item = window::Id>,
    window: window::Id,
) -> bool {
    overlays.into_iter().any(|id| id == window)
}

/// **Pure**, unit-tested: which preview a key press is addressed to.
///
/// Three inputs, read in one order:
///
/// * `delivered` is the preview the press was delivered TO, when the delivering surface is
///   itself a preview. That is the unambiguous case and it always wins, whatever else is open.
/// * `owns_own_keyboard` says the press landed on a window of OURS that owns its keyboard
///   (see [`App::owns_its_own_keyboard`]). Then there is no preview target at all, and the
///   press falls through to the lanes after this one.
/// * `focused_preview` is the fallback: the last-focused preview, for a press that arrived
///   with no preview surface of its own and no other claimant.
///
/// **The middle term is the whole reason this is a function.** It used to be absent, so the
/// fallback reached every surface in the process and a key typed into the settings search
/// undid a preview edit. Stating the rule here, rather than as an `.or_else` chain at the
/// call site, is what lets it be tested without a compositor or a live window.
///
/// The ordering is load-bearing in one direction only: `delivered` outranks
/// `owns_own_keyboard`, so the two sets can never be read as overlapping. They cannot
/// actually overlap (a preview is not the settings window), and asking in this order means
/// nothing depends on that staying true.
fn key_target(
    delivered: Option<window::Id>,
    owns_own_keyboard: bool,
    focused_preview: Option<window::Id>,
) -> Option<window::Id> {
    match delivered {
        Some(id) => Some(id),
        None if owns_own_keyboard => None,
        None => focused_preview,
    }
}

/// **Pure**, unit-tested: whether the fixed region-copy chord ([`crate::shortcuts::is_region_copy_chord`])
/// FIRES in the capture overlay right now (DRAGON-479). The chord itself is matched separately;
/// this is the whole of "does it mean anything here", split out so the four-way precedence is
/// testable without a compositor, an overlay or a keypress.
///
/// All five terms, and why each is a term rather than an assumption:
///
/// * `mode == Region` — the action copies THE DRAWN REGION. In window or monitor mode there is
///   no rectangle for it to mean.
/// * `kind != Scanner`: the scanner OWNS this chord, because primary+C there is
///   `shortcuts::FixedScannerAction::Copy`, which copies the recognized text out of the scan.
///   That meaning is untouched, and this term is the only thing standing between the two (see
///   the lane in `handle_key`). The scanner pins `Mode::Region`, so without this term the
///   region meaning would ALWAYS win in scanner kind and the OCR copy would become
///   unreachable.
/// * `region_drawn` — with nothing drawn there is nothing to capture; the press falls through
///   to whatever handled it before (in practice the scanner lane's OCR copy).
/// * `!busy` — a countdown or a live recording owns the session. Committing a still capture
///   underneath either one would end a session the user is in the middle of.
/// * `!fallback` (DRAGON-577) — on a portal-FALLBACK session (the caller's
///   `overlay_fallback_active`; keyed on the session, never on "is Flatpak") the copy cannot
///   DELIVER: there is no data-control protocol, and the clipboard fallback rides the preview
///   window's focus, which the skip-editor path never creates. Firing would silently lose the
///   user's capture, so the chord is honestly dead there, and the Region tooltip
///   (`toolbar::region_capture_tip`) stops advertising it on the same predicate.
///
/// Every `false` here is a FALL-THROUGH, never a swallow: the caller only returns early when
/// this is true, so a chord that does not fire behaves exactly as it did before DRAGON-479.
fn region_copy_fires(
    mode: Mode,
    kind: Kind,
    region_drawn: bool,
    busy: bool,
    fallback: bool,
) -> bool {
    mode == Mode::Region && kind != Kind::Scanner && region_drawn && !busy && !fallback
}

/// **Pure**, unit-tested: whether a directional key ([`crate::shortcuts::nudge_direction`])
/// MOVES the drawn region right now (DRAGON-599). The key itself is matched separately; this
/// is the whole of "does it mean anything here".
///
/// Four terms, and why each is one:
///
/// * `mode == Region` — the action moves THE DRAWN RECTANGLE. Window and monitor mode have no
///   rectangle, and nothing on those overlays wants an arrow key.
/// * `!color_picking` — a picker launch mints the same per-output overlays and can be carrying
///   a REMEMBERED region it never shows (`self.region` is restored from the config at every
///   launch). Without this term the arrows would move an invisible rectangle AND nudge the
///   picker's sample at the same time. The picker's own lane, earlier in `handle_key`, is
///   where the directional keys mean something during a pick.
/// * `region_drawn` — with nothing drawn there is nothing to move, so the press falls through
///   exactly as it did before this existed.
/// * `!busy` — a countdown or a live recording owns the session. Moving the rectangle out from
///   under a running take, or out from under a shot that is a second away, would change what
///   gets captured after the user stopped choosing.
///
/// The KIND is deliberately not a term, unlike [`region_copy_fires`]'s: a scanner selection is
/// still a drawn region and moving it is exactly how you line a scan up. Its own bindings are
/// primary chords, which these keys never match.
///
/// Every `false` is a FALL-THROUGH, never a swallow: this lane is the last thing `handle_key`
/// tries, so a key that does not fire behaves exactly as it did before DRAGON-599.
fn region_nudge_fires(mode: Mode, color_picking: bool, region_drawn: bool, busy: bool) -> bool {
    mode == Mode::Region && !color_picking && region_drawn && !busy
}

/// DRAGON-599: whether a directional key RELEASE should persist the nudged region.
///
/// Split from [`region_nudge_fires`] by exactly one term, and the term is the point. A
/// release is not the press: by the time an arrow comes up the region has ALREADY moved, so
/// `region_drawn` is true by construction and re-testing it would only be a way to lose the
/// save. Everything else must still hold, so a release that arrives after a countdown started
/// (Escape, then let go of the arrow) does not write a region the session no longer owns.
///
/// Why a release at all: `save_state` writes the whole config file, and key auto-repeat
/// delivers a press per repeat, so saving on the PRESS would mean tens of writes a second for
/// as long as a key is held. The release is the end of the gesture, exactly as `RegionDone` is
/// the end of a drag, and it fires once per hold. **Pure**, unit-tested.
pub(in crate::app) fn region_nudge_settles(mode: Mode, color_picking: bool, busy: bool) -> bool {
    mode == Mode::Region && !color_picking && !busy
}

/// Whether the colour picker has a pixel it could hand over right now (DRAGON-612).
///
/// Three states rather than a bool, because "no colour yet" has two causes that mean opposite
/// things to a user pressing Enter, and a bool would collapse them into one wrong answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::app) enum PickerSample {
    /// A pixel is being sampled and shown in the loupe. An accept can take it.
    Ready,
    /// Not yet, but shortly: the launch's frozen snapshot is still in flight, or the pointer
    /// has not been located on an overlay yet. Both end on their own, in the thing the user
    /// asked for.
    Loading,
    /// The snapshot has landed and the pointer is known, and there is still no pixel. This
    /// display genuinely cannot be read, and waiting cannot change that.
    Unreadable,
}

/// **Pure**, unit-tested: which of the three [`PickerSample`] states the picker is in.
///
/// Its own function because the classification is the whole subtlety of the picker's accept.
/// The two Loading causes look nothing alike in the code and are identical to the user:
///
/// * `snapshot_pending` — the frozen-flats grab is deferred off the launch critical path so the
///   overlay maps immediately, so the first moments of EVERY picker launch have no pixels.
/// * `!has_pointer` — the sample is read from the last recorded pointer position, and until the
///   compositor delivers a `wl_pointer.enter` there is no position to read from. DRAGON-609
///   made that enter arrive promptly, but "promptly" is not "before the user pressed Enter".
///
/// Treating either as Unreadable would refuse a perfectly good display once per launch, which
/// is exactly the false alarm `color_picker_resample` already guards against for the toast.
///
/// **What resolves a held accept is `has_sample`, never `has_pointer` on its own**, and that
/// distinction is worth stating because a pointer can become known WITHOUT the user moving
/// anything: cosmic-comp's `wl_pointer.enter` is indistinguishable downstream from a real move,
/// so it arrives as an ordinary `ColorPickerMsg::Moved` and sets the position. That is the
/// correct thing to accept on, not a hazard to guard against: the enter reports where the
/// pointer TRULY is, and "the pixel under the cursor" is exactly what the user asked for when
/// they pressed Enter early. What it must not do is fire on the position alone, because the
/// enter can land before the snapshot does; requiring a real sample means the accept waits for
/// a pixel that was actually read rather than for the mere knowledge of where to read one.
/// `has_pointer` therefore only ever separates Loading from Unreadable, and never fires
/// anything by itself.
pub(in crate::app) fn picker_sample_state(
    has_sample: bool,
    snapshot_pending: bool,
    has_pointer: bool,
) -> PickerSample {
    if has_sample {
        return PickerSample::Ready;
    }
    if snapshot_pending || !has_pointer {
        return PickerSample::Loading;
    }
    PickerSample::Unreadable
}

/// What an accept key ([`crate::shortcuts::is_accept_key`]) should DO right now (DRAGON-612).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::app) enum AcceptOutcome {
    /// Commit the drawn region, through the ordinary capture path.
    Region,
    /// Take the picker's current colour, exactly as a click would.
    Picker,
    /// The picker cannot read a pixel YET. Hold the press and ask again shortly.
    Wait,
    /// Say why, out loud, and drop the press.
    Refuse(AcceptRefusal),
}

/// Why an accept key did nothing (DRAGON-612). Every variant reaches the debug log; the two
/// marked VISIBLE also reach the overlay's banner.
///
/// It is an enum rather than a message string because the pure gate must be able to assert
/// WHICH refusal a state produces, and because the choice of "banner or log only" is part of
/// the decision rather than a property of the copy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::app) enum AcceptRefusal {
    /// Region mode with nothing drawn. VISIBLE: the overlay stacks the banner here, and this
    /// is the case most likely to be read as the key being broken.
    NoRegion,
    /// The picker is over a display with no readable pixel source. VISIBLE.
    NoPixelSource,
    /// A countdown, a recording or a committed capture owns the session. LOG ONLY.
    Busy,
    /// Nothing on this surface accepts: window or monitor mode, or a press that did not land
    /// on one of our overlays at all. LOG ONLY.
    NotHere,
}

/// How long a held accept keeps hoping the picker will find a pixel (DRAGON-612).
///
/// Nothing in this app waits unboundedly, and a held keypress is a wait like any other. What
/// is being waited on is narrow: a pick reads the FROZEN SNAPSHOT, so it is completely
/// indifferent to how far DRAGON-606's dim fade has come, and what is left is the grab itself
/// and the pointer enter.
///
/// Four seconds because the worst honest path is the DRAGON-600 tray hold followed by that
/// grab, and a picker launched from the tray is an ordinary way to reach this. Short enough
/// that a session which never settles gives up while the user still remembers pressing the key.
pub(in crate::app) const ACCEPT_WAIT_BUDGET_MS: u64 = 4_000;

const _: () = assert!(
    ACCEPT_WAIT_BUDGET_MS > super::capture_flow::MENU_HOLD_BUDGET_MS,
    "DRAGON-612: a held accept must outlast the DRAGON-600 tray-dropdown hold, which is what \
     delays the frozen-flats grab the pick reads from. Shorter and an Enter pressed on a \
     tray-launched picker gives up before the snapshot it was waiting for has arrived, every \
     time, which reads as the key being dead on the tray path only"
);

/// **Pure**, unit-tested: has a held accept run out of patience?
///
/// Its own function, tiny as it is, because an inline comparison never gets a test and quietly
/// becomes `>` versus `>=` on someone's refactor. At exactly the budget the request is over.
pub(in crate::app) fn accept_wait_expired(waited_ms: u64) -> bool {
    waited_ms >= ACCEPT_WAIT_BUDGET_MS
}

/// **Pure**, unit-tested: what an accept key should do right now (DRAGON-612).
///
/// The key itself is matched separately ([`crate::shortcuts::is_accept_key`]); this is the
/// whole of "does it mean anything here". ONE function for BOTH surfaces, unlike the nudge,
/// which splits the same choice across an `if color_picking()` and [`region_nudge_fires`].
/// The difference is that a nudge has no refusal vocabulary and an accept does: with two
/// functions, the two surfaces could each decline for their own reason and the press would
/// end up saying nothing at all, which is the exact defect this ticket exists to remove.
///
/// The terms, in the order they are asked, because the order IS the precedence:
///
/// * `on_overlay` — this session still HAS overlays of ours. Its live case is the re-ask: a
///   held accept whose overlays were torn down while it waited (a sibling capture, a teardown)
///   must drop rather than fire into nothing. At press time it is implied by the stricter test
///   the dispatch lane applies, that the press was DELIVERED to one of those overlays
///   ([`App::press_landed_on_overlay`]). That delivery test is shared with the DIRECTIONAL-key
///   lane since DRAGON-617, and the two bare-key lanes are the only ones that need it: a
///   modifier chord is not typing, so the region-copy chord and the baked editor chords answer
///   wherever they are pressed.
/// * `busy` — a countdown, a live recording or an already-committed capture owns the session.
///   Same term and same reason as [`region_copy_fires`] and [`region_nudge_fires`]:
///   committing underneath one of those would end a session the user is in the middle of.
/// * `color_picking` — a picker launch mints the same per-output overlays as a capture, and it
///   can be carrying a REMEMBERED region it never draws. This is what keeps the two surfaces
///   exclusive by construction rather than by dispatch order, exactly as it does for the nudge.
/// * `mode` / `region_drawn` — the capture side. Only [`Mode::Region`] has a box to accept.
///
/// **Window and monitor mode DECLINE, and that was considered rather than overlooked.** The
/// owner's call, on this reasoning: in those modes the pointer IS the selection, so a click is
/// already a single gesture and there is no keyboard-nudged precision to preserve. DRAGON-599
/// nudges a DRAWN RECTANGLE and nothing else, so the loop this feature closes does not exist
/// there. If that is ever revisited, the thing to add is an accept for the HOVERED target, and
/// it should be a new arm here rather than a widened `Region` one, because "commit the box I
/// drew" and "commit the window I am pointing at" are different promises.
///
/// Nothing here is a swallow. Every `Refuse` still returns to the same `Task::none()` the press
/// reached before this existed; what changed is that it now says so.
pub(in crate::app) fn accept_verdict(
    on_overlay: bool,
    busy: bool,
    color_picking: bool,
    picker: PickerSample,
    mode: Mode,
    region_drawn: bool,
) -> AcceptOutcome {
    if !on_overlay {
        return AcceptOutcome::Refuse(AcceptRefusal::NotHere);
    }
    if busy {
        return AcceptOutcome::Refuse(AcceptRefusal::Busy);
    }
    if color_picking {
        return match picker {
            PickerSample::Ready => AcceptOutcome::Picker,
            PickerSample::Loading => AcceptOutcome::Wait,
            PickerSample::Unreadable => AcceptOutcome::Refuse(AcceptRefusal::NoPixelSource),
        };
    }
    if mode != Mode::Region {
        return AcceptOutcome::Refuse(AcceptRefusal::NotHere);
    }
    if !region_drawn {
        return AcceptOutcome::Refuse(AcceptRefusal::NoRegion);
    }
    AcceptOutcome::Region
}

/// The delivery test the two BARE-key lanes share: accept (DRAGON-612) and the directional
/// keys, which had it missing.
#[cfg(test)]
mod press_on_overlay_tests {
    use super::*;

    fn id() -> window::Id {
        window::Id::unique()
    }

    /// The press must land on THE overlay, not merely coexist with one. That distinction is
    /// the bug: `h` typed into the settings search moved the drawn region precisely because
    /// the lane asked "is there a region" and never "did this press arrive here".
    #[test]
    fn only_the_delivering_overlay_counts() {
        let (a, b, elsewhere) = (id(), id(), id());
        assert!(press_on_overlay([a, b], a));
        assert!(press_on_overlay([a, b], b));
        assert!(
            !press_on_overlay([a, b], elsewhere),
            "a press on another surface must not be answered just because overlays exist"
        );
    }

    /// A session with no overlays answers false for everything: a preview-only or
    /// picker-window-only process has nothing on screen for these keys to act on.
    #[test]
    fn no_overlays_means_no_bare_key_lane_fires() {
        assert!(!press_on_overlay([], id()));
    }

    /// The colour picker's RESULT window is a surface of ours that is NOT an overlay, which
    /// is the accepted behaviour change: nudging works on the picker's overlays and stops
    /// while the result window holds focus, because that window carries the hex and RGB text
    /// rows. Its overlays are ordinary `self.outputs` entries, so those keep working.
    #[test]
    fn the_picker_result_window_is_not_an_overlay() {
        let overlay = id();
        let result_window = id();
        assert!(press_on_overlay([overlay], overlay), "the picker overlay still nudges");
        assert!(
            !press_on_overlay([overlay], result_window),
            "the result window has text inputs, so its keys are its own"
        );
    }
}

/// Which preview a key press is addressed to, and the window that must never be answered
/// by one: a settings / permissions / colour-picker surface with a text input in it.
#[cfg(test)]
mod key_target_tests {
    use super::*;

    fn id() -> window::Id {
        window::Id::unique()
    }

    /// A press delivered to a preview goes to THAT preview, whatever else is open and
    /// whichever preview last held focus. The unambiguous case, and it must stay first.
    #[test]
    fn a_press_on_a_preview_goes_to_that_preview() {
        let (delivered, focused) = (id(), id());
        assert_eq!(key_target(Some(delivered), false, Some(focused)), Some(delivered));
        // Even if the delivering surface somehow also answered the own-keyboard test, the
        // press it actually received still wins. Nothing depends on the sets being disjoint.
        assert_eq!(key_target(Some(delivered), true, Some(focused)), Some(delivered));
    }

    /// THE BUG. A press on the settings window, the permission checker or the colour
    /// picker's result window is not the preview's to answer, even with a preview open and
    /// focused. Before this term, Ctrl+Z typed into the settings search undid a preview edit
    /// and Escape there closed the document.
    #[test]
    fn a_press_on_a_window_that_owns_its_keyboard_reaches_no_preview() {
        assert_eq!(key_target(None, true, Some(id())), None);
        // And with no preview open at all it is still None, which is the same answer it
        // always gave: this term can only ever REMOVE a target, never invent one.
        assert_eq!(key_target(None, true, None), None);
    }

    /// The fallback still works for every other surface, which is what keeps the
    /// single-preview behaviour the multi-document work was built on.
    #[test]
    fn any_other_surface_still_falls_back_to_the_focused_preview() {
        let focused = id();
        assert_eq!(key_target(None, false, Some(focused)), Some(focused));
        assert_eq!(key_target(None, false, None), None);
    }

    /// The own-keyboard term changes the answer in EXACTLY one situation: a non-preview
    /// surface of ours while a preview is open. Stated as a matrix so a later edit cannot
    /// quietly widen it into the delivered case or the no-preview case.
    #[test]
    fn the_new_term_bites_in_exactly_one_cell() {
        let (delivered, focused) = (id(), id());
        for delivered in [None, Some(delivered)] {
            for focused in [None, Some(focused)] {
                let without = key_target(delivered, false, focused);
                let with = key_target(delivered, true, focused);
                let differs = without != with;
                assert_eq!(
                    differs,
                    delivered.is_none() && focused.is_some(),
                    "{delivered:?} {focused:?}"
                );
            }
        }
    }
}

/// DRAGON-479: the fixed region-copy chord's gate.
#[cfg(test)]
mod region_copy_tests {
    use super::*;

    /// The happy path, and the one shape that produces it: a drawn region, in region mode, in a
    /// non-scanner kind, with nothing else running, on a native (non-fallback) session.
    #[test]
    fn it_fires_only_with_a_drawn_region_in_an_idle_region_capture() {
        assert!(region_copy_fires(Mode::Region, Kind::Image, true, false, false));
        assert!(region_copy_fires(Mode::Region, Kind::Video, true, false, false));
        // Nothing drawn yet.
        assert!(!region_copy_fires(Mode::Region, Kind::Image, false, false, false));
        // Busy: a countdown or a recording owns the session.
        assert!(!region_copy_fires(Mode::Region, Kind::Image, true, true, false));
    }

    /// The SCANNER keeps primary+C for its own "copy recognized text"
    /// (`shortcuts::FixedScannerAction::Copy`). The scanner pins `Mode::Region`, so this term
    /// is load-bearing rather than defensive: without it the region meaning would take the
    /// chord in every scan.
    #[test]
    fn the_scanner_never_loses_its_own_copy_chord() {
        for drawn in [true, false] {
            for busy in [true, false] {
                assert!(
                    !region_copy_fires(Mode::Region, Kind::Scanner, drawn, busy, false),
                    "scanner kind must fall through (drawn={drawn} busy={busy})"
                );
            }
        }
    }

    /// Window and monitor mode have no drawn rectangle, whatever else is true.
    #[test]
    fn only_region_mode_has_a_region_to_copy() {
        for mode in [Mode::Window, Mode::Monitor] {
            for kind in [Kind::Image, Kind::Video, Kind::Scanner] {
                assert!(!region_copy_fires(mode, kind, true, false, false), "{mode:?} / {kind:?}");
            }
        }
    }

    /// DRAGON-577: on a portal-FALLBACK session the chord NEVER fires, whatever the mode, kind
    /// or state — the copy cannot deliver there (no data-control; the clipboard fallback rides
    /// a preview window the skip-editor path never creates), so firing would silently lose the
    /// capture. The tooltip hides the chord on the same predicate.
    #[test]
    fn the_fallback_session_never_fires_the_chord() {
        for mode in [Mode::Region, Mode::Window, Mode::Monitor] {
            for kind in [Kind::Image, Kind::Video, Kind::Scanner] {
                for drawn in [true, false] {
                    for busy in [true, false] {
                        assert!(
                            !region_copy_fires(mode, kind, drawn, busy, true),
                            "{mode:?} / {kind:?} / drawn={drawn} / busy={busy}"
                        );
                    }
                }
            }
        }
    }

    /// EVERY combination that is not the happy shape falls through — stated exhaustively, so a
    /// term added or dropped later cannot quietly widen what the chord swallows. With
    /// `fallback` false this is the pre-DRAGON-577 native matrix, byte-identical.
    #[test]
    fn every_other_combination_falls_through() {
        for mode in [Mode::Region, Mode::Window, Mode::Monitor] {
            for kind in [Kind::Image, Kind::Video, Kind::Scanner] {
                for drawn in [true, false] {
                    for busy in [true, false] {
                        let want = mode == Mode::Region
                            && kind != Kind::Scanner
                            && drawn
                            && !busy;
                        assert_eq!(
                            region_copy_fires(mode, kind, drawn, busy, false),
                            want,
                            "{mode:?} / {kind:?} / drawn={drawn} / busy={busy}"
                        );
                    }
                }
            }
        }
    }
}

/// DRAGON-599: when the fixed directional keys move the drawn region, and when letting one go
/// writes it down.
#[cfg(test)]
mod region_nudge_tests {
    use super::*;

    /// The happy path: a drawn region, in region mode, in an idle capture launch. Every KIND
    /// is included on purpose — a scanner selection is still a drawn region, and moving it is
    /// exactly how a scan gets lined up, which is where this gate differs from the copy chord's.
    #[test]
    fn it_fires_with_a_drawn_region_in_an_idle_region_capture() {
        assert!(region_nudge_fires(Mode::Region, false, true, false));
        // Nothing drawn yet: there is no rectangle to move.
        assert!(!region_nudge_fires(Mode::Region, false, false, false));
        // Busy: a countdown or a recording owns the session, and the target must stop moving.
        assert!(!region_nudge_fires(Mode::Region, false, true, true));
    }

    /// Window and monitor mode have no drawn rectangle at all.
    #[test]
    fn only_region_mode_has_a_region_to_move() {
        for mode in [Mode::Window, Mode::Monitor] {
            for drawn in [true, false] {
                assert!(!region_nudge_fires(mode, false, drawn, false), "{mode:?} drawn={drawn}");
            }
        }
    }

    /// A COLOUR PICKER launch never moves a region, whatever else is true. It mints the same
    /// per-output overlays and it can be carrying a remembered region it never draws, so
    /// without this term the arrows would shove an invisible rectangle around while also
    /// nudging the picker's sample.
    #[test]
    fn a_colour_picker_launch_never_moves_a_region() {
        for mode in [Mode::Region, Mode::Window, Mode::Monitor] {
            for drawn in [true, false] {
                for busy in [true, false] {
                    assert!(
                        !region_nudge_fires(mode, true, drawn, busy),
                        "{mode:?} / drawn={drawn} / busy={busy}"
                    );
                }
            }
        }
    }

    /// The full matrix, so a term added or dropped later cannot quietly widen what these keys
    /// swallow in the capture overlay.
    #[test]
    fn every_other_combination_falls_through() {
        for mode in [Mode::Region, Mode::Window, Mode::Monitor] {
            for picking in [true, false] {
                for drawn in [true, false] {
                    for busy in [true, false] {
                        let want = mode == Mode::Region && !picking && drawn && !busy;
                        assert_eq!(
                            region_nudge_fires(mode, picking, drawn, busy),
                            want,
                            "{mode:?} / picking={picking} / drawn={drawn} / busy={busy}"
                        );
                    }
                }
            }
        }
    }

    /// The SETTLE gate is the press gate minus `region_drawn`, and that difference is the
    /// whole design: by release time the region has already moved, so re-testing "is one
    /// drawn" could only ever throw the save away.
    #[test]
    fn a_release_settles_wherever_a_press_could_have_moved_something() {
        for mode in [Mode::Region, Mode::Window, Mode::Monitor] {
            for picking in [true, false] {
                for busy in [true, false] {
                    // Whenever the PRESS would have fired (drawn = true), the release settles.
                    if region_nudge_fires(mode, picking, true, busy) {
                        assert!(
                            region_nudge_settles(mode, picking, busy),
                            "{mode:?} / picking={picking} / busy={busy}"
                        );
                    }
                    assert_eq!(
                        region_nudge_settles(mode, picking, busy),
                        mode == Mode::Region && !picking && !busy,
                        "{mode:?} / picking={picking} / busy={busy}"
                    );
                }
            }
        }
    }

    /// A release that arrives after the session moved on writes nothing: press an arrow, hit
    /// Escape into a countdown, then let the arrow go. The region belongs to the countdown by
    /// then, and a save would be recording a decision the user has already backed out of.
    #[test]
    fn a_release_after_the_session_moved_on_writes_nothing() {
        assert!(!region_nudge_settles(Mode::Region, false, true));
        assert!(!region_nudge_settles(Mode::Region, true, false));
    }
}

/// DRAGON-612: how the picker's three sample states are told apart.
#[cfg(test)]
mod picker_sample_state_tests {
    use super::*;

    /// A sample in hand is Ready, whatever else is true. The other two inputs describe how a
    /// sample might be ARRIVING, and they cannot un-arrive one that already has.
    #[test]
    fn a_sample_in_hand_is_ready() {
        for pending in [true, false] {
            for pointer in [true, false] {
                assert_eq!(
                    picker_sample_state(true, pending, pointer),
                    PickerSample::Ready,
                    "pending={pending} pointer={pointer}"
                );
            }
        }
    }

    /// THE distinction this function exists for. Two different causes, both meaning "not yet",
    /// and collapsing either into Unreadable would refuse a perfectly good display once per
    /// launch: the grab is deferred off the launch path, and the pointer enter arrives after
    /// the overlay maps.
    #[test]
    fn both_not_yet_causes_are_loading_rather_than_unreadable() {
        // The snapshot is still in flight, pointer known.
        assert_eq!(picker_sample_state(false, true, true), PickerSample::Loading);
        // The snapshot has landed, but no pointer has been located yet.
        assert_eq!(picker_sample_state(false, false, false), PickerSample::Loading);
        // Both at once, which is what the first moments of every picker launch look like.
        assert_eq!(picker_sample_state(false, true, false), PickerSample::Loading);
    }

    /// The ONE shape that cannot be fixed by waiting: the grab is done, we know where the
    /// pointer is, and there is still no pixel. That display genuinely cannot be read.
    #[test]
    fn only_a_settled_session_with_a_known_pointer_is_unreadable() {
        assert_eq!(picker_sample_state(false, false, true), PickerSample::Unreadable);
    }

    /// The whole three-input space, so a term added or dropped later cannot quietly turn a
    /// wait into a refusal (or, worse, a refusal into a wait that never ends).
    #[test]
    fn every_combination_classifies() {
        for sample in [true, false] {
            for pending in [true, false] {
                for pointer in [true, false] {
                    let want = if sample {
                        PickerSample::Ready
                    } else if pending || !pointer {
                        PickerSample::Loading
                    } else {
                        PickerSample::Unreadable
                    };
                    assert_eq!(
                        picker_sample_state(sample, pending, pointer),
                        want,
                        "sample={sample} pending={pending} pointer={pointer}"
                    );
                }
            }
        }
    }
}

/// DRAGON-612: what an accept key does on every surface state, crossed with both keys.
///
/// This module is the half of the feature that stops a future state quietly becoming a DEAD
/// KEY. The key half, that Enter and Space are the accept and nothing else is, is
/// `shortcuts::accept_key_tests`.
#[cfg(test)]
mod accept_verdict_tests {
    use super::*;
    use cosmic::iced::keyboard::{key::Named, Key, Modifiers};

    /// Every mode a capture overlay can be in.
    const MODES: [Mode; 3] = [Mode::Region, Mode::Window, Mode::Monitor];
    /// Every state the picker's sample can be in.
    const SAMPLES: [PickerSample; 3] =
        [PickerSample::Ready, PickerSample::Loading, PickerSample::Unreadable];

    /// The two keys, as the dispatch lane sees them.
    fn accept_keys() -> [Key; 2] {
        [Key::Named(Named::Enter), Key::Character(" ".into())]
    }

    /// The two happy paths, and they are exactly the two the owner asked for.
    #[test]
    fn a_drawn_region_and_a_ready_picker_accept() {
        assert_eq!(
            accept_verdict(true, false, false, PickerSample::Loading, Mode::Region, true),
            AcceptOutcome::Region,
            "the picker state is irrelevant on a capture launch"
        );
        assert_eq!(
            accept_verdict(true, false, true, PickerSample::Ready, Mode::Region, false),
            AcceptOutcome::Picker,
            "a picker launch can carry a remembered region and must still pick"
        );
    }

    /// An empty region REFUSES, visibly, and must never fall back to capturing the screen. A
    /// destructive surprise is worse than a refusal, and a silent no is worse than both.
    #[test]
    fn an_empty_region_refuses_and_never_captures_anything() {
        let v = accept_verdict(true, false, false, PickerSample::Ready, Mode::Region, false);
        assert_eq!(v, AcceptOutcome::Refuse(AcceptRefusal::NoRegion));
        assert_ne!(v, AcceptOutcome::Region, "an empty region must not commit a capture");
    }

    /// An early Enter on the picker WAITS rather than refusing, which is the ticket's own
    /// reading of what the user meant: "accept as soon as you can".
    #[test]
    fn an_early_press_on_the_picker_waits() {
        assert_eq!(
            accept_verdict(true, false, true, PickerSample::Loading, Mode::Region, false),
            AcceptOutcome::Wait
        );
        // And the one picker state that waiting cannot fix says so instead.
        assert_eq!(
            accept_verdict(true, false, true, PickerSample::Unreadable, Mode::Region, false),
            AcceptOutcome::Refuse(AcceptRefusal::NoPixelSource)
        );
    }

    /// WINDOW and MONITOR mode refuse, explicitly and on purpose, asserted as their own rows
    /// rather than left to the matrix: an omitted row is how a future change silently makes
    /// them fire. The owner's call, on the reasoning in `accept_verdict`'s doc: the pointer IS
    /// the selection in those modes, so there is no keyboard-nudged precision to preserve.
    #[test]
    fn window_and_monitor_mode_refuse_whatever_else_is_true() {
        for mode in [Mode::Window, Mode::Monitor] {
            for drawn in [true, false] {
                for sample in SAMPLES {
                    assert_eq!(
                        accept_verdict(true, false, false, sample, mode, drawn),
                        AcceptOutcome::Refuse(AcceptRefusal::NotHere),
                        "{mode:?} / drawn={drawn} / {sample:?}"
                    );
                }
            }
        }
    }

    /// A COLOUR PICKER launch never commits a region, whatever mode or rectangle it is
    /// carrying. Same term and same reason as `region_nudge_fires`: a picker mints the same
    /// per-output overlays and `App::region` is restored from the config at every launch, so
    /// without this the accept could capture an invisible rectangle.
    #[test]
    fn a_picker_launch_never_commits_a_region() {
        for mode in MODES {
            for drawn in [true, false] {
                for sample in SAMPLES {
                    let v = accept_verdict(true, false, true, sample, mode, drawn);
                    assert_ne!(v, AcceptOutcome::Region, "{mode:?} / drawn={drawn} / {sample:?}");
                }
            }
        }
    }

    /// A BUSY session refuses everywhere, and the reason outranks every surface reason: a
    /// countdown, a recording or a committed capture owns the session, and committing under
    /// one of those would end something the user is in the middle of.
    #[test]
    fn a_busy_session_refuses_on_both_surfaces() {
        for picking in [true, false] {
            for mode in MODES {
                for drawn in [true, false] {
                    for sample in SAMPLES {
                        assert_eq!(
                            accept_verdict(true, true, picking, sample, mode, drawn),
                            AcceptOutcome::Refuse(AcceptRefusal::Busy),
                            "picking={picking} / {mode:?} / drawn={drawn} / {sample:?}"
                        );
                    }
                }
            }
        }
    }

    /// With NO overlays there is nothing to accept on. Its live case is the re-ask: a held
    /// accept whose overlays were torn down while it waited must drop rather than fire into
    /// nothing. It outranks `busy` because "there is no surface" is the more basic answer.
    #[test]
    fn no_overlay_refuses_before_any_other_reason() {
        for busy in [true, false] {
            for picking in [true, false] {
                for mode in MODES {
                    for sample in SAMPLES {
                        assert_eq!(
                            accept_verdict(false, busy, picking, sample, mode, true),
                            AcceptOutcome::Refuse(AcceptRefusal::NotHere),
                            "busy={busy} / picking={picking} / {mode:?} / {sample:?}"
                        );
                    }
                }
            }
        }
    }

    /// **THE table.** Every surface state crossed with BOTH keys, asserting exactly which
    /// combinations accept, that the two keys are indistinguishable in every one of them, and
    /// that everything else lands on a named answer rather than on nothing.
    ///
    /// The key crossing is not decoration. Enter and Space reach this through one predicate,
    /// and the whole promise of the feature is that they are interchangeable; a change that
    /// made one of them mean less would otherwise pass every other test in this file.
    #[test]
    fn every_state_crossed_with_both_keys_gives_a_named_answer() {
        for key in accept_keys() {
            assert!(
                crate::shortcuts::is_accept_key(Modifiers::empty(), &key),
                "{key:?} must reach the accept lane at all"
            );
            for on_overlay in [true, false] {
                for busy in [true, false] {
                    for picking in [true, false] {
                        for sample in SAMPLES {
                            for mode in MODES {
                                for drawn in [true, false] {
                                    let got = accept_verdict(
                                        on_overlay, busy, picking, sample, mode, drawn,
                                    );
                                    let want = if !on_overlay {
                                        AcceptOutcome::Refuse(AcceptRefusal::NotHere)
                                    } else if busy {
                                        AcceptOutcome::Refuse(AcceptRefusal::Busy)
                                    } else if picking {
                                        match sample {
                                            PickerSample::Ready => AcceptOutcome::Picker,
                                            PickerSample::Loading => AcceptOutcome::Wait,
                                            PickerSample::Unreadable => AcceptOutcome::Refuse(
                                                AcceptRefusal::NoPixelSource,
                                            ),
                                        }
                                    } else if mode != Mode::Region {
                                        AcceptOutcome::Refuse(AcceptRefusal::NotHere)
                                    } else if !drawn {
                                        AcceptOutcome::Refuse(AcceptRefusal::NoRegion)
                                    } else {
                                        AcceptOutcome::Region
                                    };
                                    assert_eq!(
                                        got, want,
                                        "{key:?} / on_overlay={on_overlay} / busy={busy} / \
                                         picking={picking} / {sample:?} / {mode:?} / \
                                         drawn={drawn}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// No state is a DEAD KEY: every combination answers with something the caller must act
    /// on, and the two firing answers are reachable only from the states above. `Wait` counts
    /// as audible because it is a HELD request, not a dropped one, and it ends in either the
    /// accept the user asked for or the budget's loud give-up.
    #[test]
    fn no_state_answers_with_nothing() {
        let mut fires = 0usize;
        let mut waits = 0usize;
        let mut refusals = 0usize;
        for on_overlay in [true, false] {
            for busy in [true, false] {
                for picking in [true, false] {
                    for sample in SAMPLES {
                        for mode in MODES {
                            for drawn in [true, false] {
                                match accept_verdict(
                                    on_overlay, busy, picking, sample, mode, drawn,
                                ) {
                                    AcceptOutcome::Region | AcceptOutcome::Picker => fires += 1,
                                    AcceptOutcome::Wait => waits += 1,
                                    AcceptOutcome::Refuse(_) => refusals += 1,
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(fires > 0 && waits > 0 && refusals > 0);
        assert_eq!(fires + waits + refusals, 2 * 2 * 2 * 3 * 3 * 2);
    }

    /// Every refusal reason is REACHABLE, so none of the four is dead vocabulary. Each one is
    /// handled with its own log line (and, for the two visible ones, its own banner) in
    /// `App::request_accept`, so a reason that could never occur would be a promise the code
    /// makes and never keeps.
    #[test]
    fn every_refusal_reason_is_reachable() {
        let mut seen = std::collections::BTreeSet::new();
        for on_overlay in [true, false] {
            for busy in [true, false] {
                for picking in [true, false] {
                    for sample in SAMPLES {
                        for mode in MODES {
                            for drawn in [true, false] {
                                if let AcceptOutcome::Refuse(why) = accept_verdict(
                                    on_overlay, busy, picking, sample, mode, drawn,
                                ) {
                                    seen.insert(format!("{why:?}"));
                                }
                            }
                        }
                    }
                }
            }
        }
        for want in ["NoRegion", "NoPixelSource", "Busy", "NotHere"] {
            assert!(seen.contains(want), "{want} is unreachable, so it is dead vocabulary");
        }
    }

    /// The budget is inclusive at its edge. At exactly the budget the request is over: a
    /// wait that has used its whole allowance has used it.
    #[test]
    fn the_wait_budget_is_inclusive_at_its_edge() {
        assert!(!accept_wait_expired(0));
        assert!(!accept_wait_expired(ACCEPT_WAIT_BUDGET_MS - 1));
        assert!(accept_wait_expired(ACCEPT_WAIT_BUDGET_MS));
        assert!(accept_wait_expired(ACCEPT_WAIT_BUDGET_MS + 1));
    }

    /// The budget must clear the tray hold it exists to sit through, or an Enter pressed on a
    /// tray-launched picker gives up before the snapshot arrives EVERY time, which would read
    /// as the key being dead on the tray path only. The compile-time assert beside the
    /// constant is the real guard; this is its runtime twin, so a reader who changes the
    /// number sees a named test fail rather than a build error with no context.
    ///
    /// Asked THROUGH `accept_wait_expired` rather than by comparing the two constants, so that
    /// it exercises the real comparison instead of restating the relation the compile-time
    /// assert already owns.
    #[test]
    fn the_budget_outlasts_the_tray_hold() {
        let worst = crate::app::capture_flow::MENU_HOLD_BUDGET_MS;
        assert!(
            !accept_wait_expired(worst),
            "a request held through the whole tray hold must still be alive"
        );
    }
}
