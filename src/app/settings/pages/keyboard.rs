//! Keyboard Shortcuts settings page: one row per [`Action`], each showing its
//! current binding as a button. Pressing the button captures the next key as the new
//! binding (see `App::handle_key`); the per-row reset, the page "Reset to defaults",
//! and Factory reset all restore defaults through the usual `Persisted` path.

use super::super::*;
use super::super::row::{Item, SectionSpec};
use crate::shortcuts::{Action, Shortcut};

impl crate::app::App {
    pub(in crate::app::settings) fn keyboard_sections(&self) -> Vec<SectionSpec<'_>> {
        // One section per group; groups are contiguous in `Action::ALL`, so append to
        // the current section while the group matches, else start a new one.
        let mut secs: Vec<SectionSpec<'_>> = Vec::new();

        // macOS (DRAGON-130): the resident daemon's global "Start Capture" hotkey sits
        // FIRST, in its own section at the TOP. Unlike the in-app bindings below (iced
        // key-capture), this is a process-wide OS hotkey owned by the menu-bar daemon,
        // so it is edited as a validated SPEC string ("PrintScreen", "Cmd+Shift+2", …).
        // cfg-gated so the Linux page stays byte-identical.
        #[cfg(target_os = "macos")]
        {
            // Byte-for-byte the SAME anatomy as the in-app shortcut rows below (label +
            // chord button + "x" clear), just wired to the daemon's global hotkey instead
            // of an in-app `Action`. No helper line (leanest rows have none): the
            // PrintScreen-swallow caveat is documented in `App::handle_key` /
            // `SetCaptureHotkey`, NOT in UI text.
            let d = crate::state::defaults();
            let capturing = self.settings.capture_hotkey_rebinding;
            // The current chord verbatim (the spec IS what the daemon parses), or
            // "Unbound" when cleared to empty — exactly like a neighbor with no binding.
            let label = if capturing {
                "Press a key…".to_string()
            } else if self.capture_hotkey.is_empty() {
                "Unbound".to_string()
            } else {
                self.capture_hotkey.clone()
            };
            let keybind = widget::button::standard(label)
                .on_press(Msg::Settings(SettingsMsg::BeginCaptureHotkeyRebind));
            // The same "x" clear widget/position/semantics as the neighbors: clearing sets
            // an EMPTY spec (no hotkey registered until set again). Disabled when already
            // empty, matching how a neighbor disables clear with nothing to unbind.
            let mut clear = widget::button::icon(
                widget::icon::from_name("window-close-symbolic").size(14),
            )
            .padding(6);
            if !self.capture_hotkey.is_empty() {
                clear = clear
                    .on_press(Msg::Settings(SettingsMsg::SetCaptureHotkey(String::new())));
            }
            let control = widget::row(vec![keybind.into(), clear.into()])
                .spacing(4.0)
                .align_y(Alignment::Center);
            // Restore-default (row reset slot, same style/position as every neighbor):
            // re-selects PrintScreen WITHOUT recording a keypress.
            let item = Item::new("Start Capture", "", control).reset_to(
                Msg::Settings(SettingsMsg::SetCaptureHotkey(d.capture_hotkey.clone())),
                self.capture_hotkey != d.capture_hotkey,
            );
            secs.push(SectionSpec {
                title: "Global",
                items: vec![item],
            });
        }

        for action in Action::ALL {
            let capturing = self.settings.rebinding == Some(action);
            let binding = self.keymap.get(action);
            // While capturing, the button prompts for a key; otherwise it shows the
            // current binding (e.g. "Ctrl+C"), or "Unbound" when it has none. Pressing
            // it toggles capture.
            let label = if capturing {
                "Press a key…".to_string()
            } else {
                binding.as_ref().map_or_else(|| "Unbound".to_string(), Shortcut::label)
            };
            let keybind = widget::button::standard(label)
                .on_press(Msg::Settings(SettingsMsg::BeginRebind(action)));
            // An "x" to clear the binding, sitting right next to it like a button group.
            // Disabled (no press) when there's nothing to unbind.
            let mut clear = widget::button::icon(
                widget::icon::from_name("window-close-symbolic").size(14),
            )
            .padding(6);
            if binding.is_some() {
                clear = clear.on_press(Msg::Settings(SettingsMsg::UnbindShortcut(action)));
            }
            let control = widget::row(vec![keybind.into(), clear.into()])
                .spacing(4.0)
                .align_y(Alignment::Center);
            let item = Item::new(action.label(), action.description(), control).reset_to(
                Msg::Settings(SettingsMsg::SetShortcut(action, action.default_shortcut())),
                !self.keymap.is_default(action),
            );
            match secs.last_mut() {
                Some(sec) if sec.title == action.group() => sec.items.push(item),
                _ => secs.push(SectionSpec {
                    title: action.group(),
                    items: vec![item],
                }),
            }
        }
        secs
    }
}
