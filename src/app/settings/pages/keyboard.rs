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

        // macOS (DRAGON-130) / Windows (DRAGON-237) / DRAGON-295: the resident daemon's three
        // global capture hotkeys sit FIRST, in their own section at the TOP. Unlike the in-app
        // bindings below (iced key-capture), these are process-wide OS hotkeys owned by the
        // tray/menu-bar daemon, so each is edited as a validated SPEC string ("PrintScreen",
        // "Cmd+Shift+2", …). All three default UNSET (opt-in). cfg-gated so the Linux page
        // stays byte-identical (Linux's capture key is a COSMIC custom shortcut, not owned here).
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            use crate::app::CaptureHotkeySlot;
            // One row per slot, byte-for-byte the SAME anatomy as the in-app shortcut rows
            // below (label + chord button + "x" clear), just wired to a daemon global hotkey
            // slot instead of an in-app `Action`.
            let items: Vec<Item<'_>> = [
                (CaptureHotkeySlot::AllInOne, "Capture All In One", self.capture_hotkey.as_str()),
                (
                    CaptureHotkeySlot::ActiveWindow,
                    "Capture Active Window",
                    self.capture_active_window_hotkey.as_str(),
                ),
                (
                    CaptureHotkeySlot::ActiveMonitor,
                    "Capture Active Monitor",
                    self.capture_active_monitor_hotkey.as_str(),
                ),
            ]
            .into_iter()
            .map(|(slot, label_text, spec)| {
                let capturing = self.settings.capture_hotkey_rebinding == Some(slot);
                // The current chord rendered as native modifier SYMBOLS on macOS (DRAGON-294:
                // ⌃⌥⇧⌘), or Windows-native modifier NAMES on Windows (Ctrl/Alt/Shift/Win — the
                // logo token reads "Win", not the serialized "Cmd"); "Unbound" when cleared.
                let label = if capturing {
                    "Press a key…".to_string()
                } else if spec.is_empty() {
                    "Unbound".to_string()
                } else {
                    #[cfg(target_os = "macos")]
                    {
                        crate::shortcuts::mac_symbolic_spec(spec)
                    }
                    #[cfg(windows)]
                    {
                        crate::shortcuts::win_readable_spec(spec)
                    }
                };
                let keybind = widget::button::standard(label)
                    .on_press(Msg::Settings(SettingsMsg::BeginCaptureHotkeyRebind(slot)));
                // The same "x" clear widget/position/semantics as the neighbors: clearing
                // sets an EMPTY spec (no hotkey registered until set again). Disabled when
                // already empty, matching how a neighbor disables clear with nothing to unbind.
                let mut clear = widget::button::icon(
                    widget::icon::from_name("window-close-symbolic").size(14),
                )
                .padding(6);
                if !spec.is_empty() {
                    clear = clear.on_press(Msg::Settings(SettingsMsg::SetCaptureHotkey(
                        slot,
                        String::new(),
                    )));
                }
                let control = widget::row(vec![
                    crate::widgets::arrow_cursor::arrow_cursor(keybind),
                    crate::widgets::arrow_cursor::arrow_cursor(clear),
                ])
                    .spacing(4.0)
                    .align_y(Alignment::Center);
                // All three default UNSET, so the row reset slot re-clears WITHOUT recording
                // a keypress; shown only when the row currently holds a value.
                Item::new(label_text, "", control).reset_to(
                    Msg::Settings(SettingsMsg::SetCaptureHotkey(slot, String::new())),
                    !spec.is_empty(),
                )
            })
            .collect();
            secs.push(SectionSpec {
                title: "Global",
                items,
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
            let control = widget::row(vec![
                crate::widgets::arrow_cursor::arrow_cursor(keybind),
                crate::widgets::arrow_cursor::arrow_cursor(clear),
            ])
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
