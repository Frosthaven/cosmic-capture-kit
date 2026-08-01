//! Keyboard Shortcuts settings page: one row per [`Action`], each showing its
//! current binding as a button. Pressing the button captures the next key as the new
//! binding (see `App::handle_key`); the per-row reset, the page "Reset to defaults",
//! and Factory reset all restore defaults through the usual `Persisted` path.

use super::super::*;
use super::super::row::{Item, SectionSpec};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use super::super::row::Severity;
use crate::shortcuts::{Action, Shortcut};

impl crate::app::App {
    pub(in crate::app::settings) fn keyboard_sections(&self) -> Vec<SectionSpec<'_>> {
        // One section per group; groups are contiguous in `Action::ALL`, so append to
        // the current section while the group matches, else start a new one.
        let mut secs: Vec<SectionSpec<'_>> = Vec::new();

        // macOS (DRAGON-130) / Windows (DRAGON-237) / DRAGON-295 / DRAGON-428: the resident
        // daemon's six global capture hotkeys sit FIRST, in their own section at the TOP.
        // Unlike the in-app bindings below (iced key-capture), these are process-wide OS
        // hotkeys owned by the tray/menu-bar daemon, so each is edited as a validated SPEC
        // string ("PrintScreen", "Cmd+Shift+2", …). All six default UNSET (opt-in). cfg-gated
        // so the Linux page stays byte-identical, AND because these rows do not apply there at
        // all: Linux's capture key is a COSMIC custom shortcut in the user's own desktop
        // config, and its only in-app global bindings are the RECORDING hotkeys, which go
        // through the xdg portal (`platform::global_shortcuts`) and are bound by the desktop.
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            use crate::app::CaptureHotkeySlot;
            // One row per slot, byte-for-byte the SAME anatomy as the in-app shortcut rows
            // below (label + chord button + "x" clear), just wired to a daemon global hotkey
            // slot instead of an in-app `Action`. The order and the labels come from
            // `CaptureHotkeySlot::ALL` / `label`, which the DRAGON-452 duplicate check and
            // both daemons' registration tables share, so the three can never drift.
            // DRAGON-428's "(no editor)" twins sit after all three originals (that is the
            // order `ALL` declares) so the pairs read as a group rather than interleaving.
            let items: Vec<Item<'_>> = CaptureHotkeySlot::ALL
            .into_iter()
            .map(|slot| {
                let label_text = slot.label();
                let spec = self.capture_hotkey_slot(slot);
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
                    crate::widgets::icons::handle("window-close-symbolic"),
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
                // DRAGON-452: what the last recorded chord did to THIS row, in the helper
                // line. A row that took a chord says where it came from; the row that lost
                // one says who took it. A duplicate is never accepted in silence.
                let (note, sev) = self.capture_hotkey_row_note(slot);
                // All six default UNSET, so the row reset slot re-clears WITHOUT recording
                // a keypress; shown only when the row currently holds a value.
                let mut item = Item::new(label_text, note, control).reset_to(
                    Msg::Settings(SettingsMsg::SetCaptureHotkey(slot, String::new())),
                    !spec.is_empty(),
                );
                if let Some(sev) = sev {
                    item = item.status(sev);
                }
                item
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
                crate::widgets::icons::handle("window-close-symbolic"),
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
            // Reset restores the FACTORY binding — or clears it, for an action that ships
            // unbound (DRAGON-369: the slot-member tools, reached through their cycle key).
            let reset = match action.default_shortcut() {
                Some(sc) => Msg::Settings(SettingsMsg::SetShortcut(action, sc)),
                None => Msg::Settings(SettingsMsg::UnbindShortcut(action)),
            };
            let item = Item::new(action.label(), action.description(), control)
                .reset_to(reset, !self.keymap.is_default(action));
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

    /// DRAGON-452: the helper line for ONE global capture-hotkey row, describing what the
    /// last recorded chord did to it, plus the severity that colours it. Empty (and `None`)
    /// when this row was not involved, which is the normal case.
    ///
    /// Three things can be said, and the winning row can say two of them at once:
    ///
    /// * the row TOOK a chord that another row held, and that row is now unbound;
    /// * the row LOST its chord to another row;
    /// * the OS refused to register the chord because another app already owns it globally
    ///   (Windows, where the settings process can ask; see the `SetCaptureHotkey` handler).
    ///
    /// The point is that a duplicate is never accepted in silence: whichever way the collision
    /// resolved, the rows involved say so in plain words.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn capture_hotkey_row_note(
        &self,
        slot: crate::app::CaptureHotkeySlot,
    ) -> (String, Option<Severity>) {
        let Some(notice) = self.settings.capture_hotkey_notice else {
            return (String::new(), None);
        };
        let mut parts: Vec<String> = Vec::new();
        let mut sev: Option<Severity> = None;
        if notice.slot == slot {
            if let Some(from) = notice.taken_from {
                parts.push(format!(
                    "This shortcut was taken from {}. That shortcut is now unbound.",
                    from.label()
                ));
                sev = Some(Severity::Warn);
            }
            if notice.os_refused {
                parts.push(
                    "Another app is already using this shortcut, so it will not work here."
                        .to_string(),
                );
                sev = Some(Severity::Error);
            }
        } else if notice.taken_from == Some(slot) {
            parts.push(format!(
                "Unbound. {} now uses this shortcut.",
                notice.slot.label()
            ));
            sev = Some(Severity::Warn);
        }
        (parts.join(" "), sev)
    }
}
