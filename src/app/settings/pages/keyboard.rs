//! Keyboard Shortcuts settings page: one row per [`Action`], each showing its
//! current binding as a button. Pressing the button captures the next key as the new
//! binding (see `App::handle_key`); the per-row reset, the page "Reset to defaults",
//! and Factory reset all restore defaults through the usual `Persisted` path.
//!
//! ## The Global tab (DRAGON-589)
//!
//! The three in-app tabs list bindings this app owns. The GLOBAL tab lists the hotkeys the
//! operating system owns, and its promise is that **every global action this build can
//! perform gets a row**: a chord editor where the app can register the key itself, otherwise
//! the exact command a user pastes into their own desktop's shortcut settings.
//!
//! That rests on TWO separate questions, and keeping them separate is the whole design:
//!
//! 1. **Is the action in this build at all?** If not there is no row and no mention. Under a
//!    Flatpak the compositor hides the protocols that say which window or monitor is active,
//!    so `--active-window` / `--active-monitor` cannot run there by any route; advertising a
//!    command for them would be advertising a shortcut that fails. Asked through
//!    `capture_flow::immediate_capture_available`, which is the same gate the LAUNCH consults
//!    before it fails a session, so the page and the launch cannot disagree.
//! 2. **Can the app bind the key itself?** If yes the row keeps its chord editor
//!    (`platform::app_registers_capture_hotkeys`); if no the row shows the command. This is
//!    never a reason to hide anything: the action still works, it is just the desktop's job
//!    to put a key on it.
//!
//! [`ShortcutsTab::GLOBAL_EMPTY_NOTE`] leads the tab, under an EMPTY group title, whenever any
//! row turned out to be a command row. It is the one sentence that explains them all, so it is
//! said once at the top rather than repeated per row.

use super::super::*;
use super::super::row::{command_line, Item, SectionSpec, Severity};
use crate::app::CaptureHotkeySlot;
use crate::shortcuts::{Action, Shortcut};

/// **Pure**, unit-tested: the helper line a row shows when the chord the user pressed is one
/// DRAGON-617 baked, so the rebind was refused.
///
/// It has three jobs, in order. NAME the key, so the user knows which of the two things they
/// just pressed was the problem. Say it is FIXED rather than taken, because "taken" would
/// invite them to go and free it and nothing here can be freed. Then say what to do next,
/// because the row is still armed and one more press finishes the job they started.
///
/// Deliberately does not list the five, or say which action owns the key. A row for "Copy to
/// clipboard" is a bad place to teach the whole fixed-shortcut table, and the DRAGON-588 /
/// DRAGON-589 tab is the right one: when it lands, this sentence should point at it.
fn fixed_chord_refusal(key: &str) -> String {
    format!("{key} is a fixed shortcut and cannot be reassigned. Press a different key.")
}

/// What the Global tab can offer for ONE global action.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::app::settings) enum GlobalRow {
    /// The app registers this hotkey itself, so the row is an editable chord.
    Chord,
    /// The app cannot register it, but the action IS in this build: show the command that
    /// runs it, for the user's own desktop shortcut.
    Command,
    /// The action is not in this build at all: no row, and no mention of it anywhere.
    Absent,
}

/// **Pure**, unit-tested: which shape a Global tab row takes, from the two questions the
/// module doc separates (DRAGON-589).
///
/// The order of the tests is the rule. Absence wins outright: an action this build cannot
/// perform must not appear even as a command, because the command would be a shortcut that
/// does nothing. Only then does it matter who binds the key, and neither answer hides
/// anything, they only choose between a chord editor and a copyable command.
pub(in crate::app::settings) fn global_row_kind(
    in_this_build: bool,
    app_binds_it: bool,
) -> GlobalRow {
    match (in_this_build, app_binds_it) {
        (false, _) => GlobalRow::Absent,
        (true, true) => GlobalRow::Chord,
        (true, false) => GlobalRow::Command,
    }
}

/// **Pure**, unit-tested: does the Global tab lead with
/// [`ShortcutsTab::GLOBAL_EMPTY_NOTE`]?
///
/// Yes as soon as ONE row is a command row, because that sentence is what explains why a
/// command is being shown at all, and saying it once above everything beats repeating it per
/// row. A machine where every global hotkey is bindable (macOS and Windows today) never sees
/// it, which keeps those pages exactly as they were.
///
/// The second clause is a floor rather than a case that ships: `ShortcutsTab::occupied`
/// promises the Global tab always has content, so a build that somehow offered no global
/// action at all must still say something rather than render a blank tab.
pub(in crate::app::settings) fn global_note_shown(command_rows: usize, total_rows: usize) -> bool {
    command_rows > 0 || total_rows == 0
}

/// Pure, unit-tested: may a "(no editor)" capture slot be offered in THIS session?
///
/// An editor-less capture saves, copies and notifies with no preview window ever opening.
/// That is only coherent where the clipboard write can STAND ON ITS OWN
/// ([`crate::share::CopyRoute::Standalone`]): macOS and Windows write inline, and Linux with
/// `data_control` hands a detached worker the selection. A [`crate::share::CopyRoute::ThisWindow`]
/// session (a sandbox, where the compositor hides `data_control`) can only serve a selection
/// through a focused window of ours, and an editor-less capture opens none, so the copy has
/// nowhere to go.
///
/// The ordinary slots are unaffected: they open the editor, which IS the window that route
/// needs. Keyed on the copy route rather than on "is Flatpak", so a sandboxed session on a
/// compositor that does expose `data_control` keeps its rows.
fn editor_less_slot_available(no_editor: bool, route: crate::share::CopyRoute) -> bool {
    !no_editor || matches!(route, crate::share::CopyRoute::Standalone)
}

impl crate::app::App {
    pub(in crate::app::settings) fn keyboard_sections(&self) -> Vec<SectionSpec<'_>> {
        // One section per group; groups are contiguous in `Action::ALL`, so append to
        // the current section while the group matches, else start a new one.
        let mut secs: Vec<SectionSpec<'_>> = Vec::new();

        // The GLOBAL tab's two groups, built first because they lead the page.
        let (capture_items, capture_commands) = self.global_capture_items();
        let (recording_items, recording_commands) = self.global_recording_items();
        let command_rows = capture_commands + recording_commands;
        let total_rows = capture_items.len() + recording_items.len();
        // The note sits ABOVE both groups, under an EMPTY group title that still takes a
        // heading's worth of space (`section_card` reserves it). It is not a group of
        // shortcuts, it is the answer to the question the groups below it raise, so giving it
        // a name would invent a category that does not exist.
        if global_note_shown(command_rows, total_rows) {
            secs.push(SectionSpec {
                title: crate::app::settings::ShortcutsTab::GLOBAL_NOTE_GROUP,
                items: vec![Item::note(widget::text::caption(
                    crate::app::settings::ShortcutsTab::GLOBAL_EMPTY_NOTE,
                ))],
            });
        }
        if !capture_items.is_empty() {
            secs.push(SectionSpec {
                title: crate::app::settings::ShortcutsTab::GLOBAL_CAPTURE_GROUP,
                items: capture_items,
            });
        }
        if !recording_items.is_empty() {
            secs.push(SectionSpec {
                title: crate::app::settings::ShortcutsTab::GLOBAL_RECORDING_GROUP,
                items: recording_items,
            });
        }

        // DRAGON-583: whether an in-app shortcut can reach a recording that is ALREADY
        // RUNNING, which is the only thing the three Recording rows below are for. On a
        // session that cannot deliver one, they are dead controls, and the settled rule for
        // a control that cannot apply is to hide it and show what DOES work instead (the
        // DRAGON-551 / 569 / 577 hide-where-dead line, DRAGON-577 being the closest: a chord
        // the session cannot honor stopped being advertised and stopped firing together).
        //
        // Keyed on the CAPABILITY, never on the platform and never on Flatpak: on Linux the
        // deciding term is a live probe of the portal's GlobalShortcuts interface, so a
        // desktop that ships it keeps the rows exactly as they are today. macOS and Windows
        // answer `true` from their own facts (their overlay windows survive record start and
        // nothing takes the keyboard away), so their page is byte-identical.
        let recording_shortcuts_live = crate::platform::in_app_recording_shortcuts_work();

        for action in Action::ALL {
            if !recording_shortcuts_live && action.context() == crate::shortcuts::Context::Recording
            {
                continue;
            }
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
            // DRAGON-617: a chord this build BAKED cannot be taken, and the row says so
            // instead of the press appearing to do nothing. Same shape the Global tab's
            // duplicate notice uses (DRAGON-452): the sentence replaces the helper line and
            // the row wears an error status while it stands.
            let refusal = self
                .settings
                .rebind_refused
                .as_ref()
                .filter(|(a, _)| *a == action)
                .map(|(_, key)| fixed_chord_refusal(key));
            let refused = refusal.is_some();
            let desc: std::borrow::Cow<'_, str> = refusal.map_or_else(
                || std::borrow::Cow::Borrowed(action.description()),
                std::borrow::Cow::Owned,
            );
            let mut item = Item::new(action.label(), desc, control)
                .reset_to(reset, !self.keymap.is_default(action));
            if refused {
                item = item.status(Severity::Error);
            }
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


    /// The Global Capture rows, and how many of them are COMMAND rows.
    ///
    /// One row per [`CaptureHotkeySlot`], in `ALL`'s order, which is also the order both
    /// resident daemons register them in and the order the DRAGON-452 duplicate check speaks
    /// in. DRAGON-428's "(no editor)" twins therefore sit after all three originals, so the
    /// pairs read as a group rather than interleaving.
    ///
    /// A slot that this build cannot perform at all is skipped outright, with nothing said
    /// about it (DRAGON-589): under a Flatpak the compositor hides the protocols an immediate
    /// capture needs, so the four active-window / active-monitor rows are simply not there.
    /// Everything else gets a row, whichever shape [`global_row_kind`] chooses.
    ///
    /// The "(no editor)" twins are skipped where the session has no STANDALONE clipboard
    /// route (owner's call). DRAGON-589 first kept them, reasoning that the missing piece is
    /// `data_control`, which only gates a CLIPBOARD delivery, so a file-save delivery would
    /// still work. The owner overruled that: "flatpak cann't do the no-editor options because
    /// of how copy and paste work. remove the command suggestions from there." The point of
    /// an editor-less capture is copy-and-go, and a `CopyRoute::ThisWindow` session cannot
    /// serve one, because that route needs a focused window of ours and an editor-less
    /// capture deliberately opens none. Offering the command anyway would be advertising a
    /// hotkey that half-works, which is the thing this tab exists to stop.
    fn global_capture_items(&self) -> (Vec<Item<'_>>, usize) {
        let mut items: Vec<Item<'_>> = Vec::new();
        let mut commands = 0usize;
        for slot in CaptureHotkeySlot::ALL {
            // Only the active-target slots ask the compositor what is active; the rest answer
            // `None` and are available wherever a capture is.
            let in_this_build = slot
                .immediate()
                .is_none_or(crate::app::capture_flow::immediate_capture_available)
                && editor_less_slot_available(slot.no_editor(), crate::share::copy_route());
            match global_row_kind(
                in_this_build,
                crate::platform::app_registers_capture_hotkeys(),
            ) {
                GlobalRow::Absent => continue,
                GlobalRow::Chord => {}
                GlobalRow::Command => commands += 1,
            }
            items.push(self.global_capture_item(slot));
        }
        (items, commands)
    }

    /// ONE Global Capture row, on a platform whose resident daemon can BIND the key
    /// (macOS DRAGON-130 / Windows DRAGON-237, both DRAGON-295 / DRAGON-428).
    ///
    /// Unlike the in-app bindings further down the page (iced key-capture), these are
    /// process-wide OS hotkeys owned by the tray/menu-bar daemon, so each is edited as a
    /// validated SPEC string ("PrintScreen", "Cmd+Shift+2", …). All seven default UNSET
    /// (opt-in). Untouched by DRAGON-589, which only changed what happens on the platforms
    /// that have no such daemon.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn global_capture_item(&self, slot: CaptureHotkeySlot) -> Item<'_> {
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
        let mut clear =
            widget::button::icon(crate::widgets::icons::handle("window-close-symbolic"))
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
        // All seven default UNSET, so the row reset slot re-clears WITHOUT recording
        // a keypress; shown only when the row currently holds a value.
        let mut item = Item::new(label_text, note, control).reset_to(
            Msg::Settings(SettingsMsg::SetCaptureHotkey(slot, String::new())),
            !spec.is_empty(),
        );
        if let Some(sev) = sev {
            item = item.status(sev);
        }
        item
    }

    /// ONE Global Capture row on a platform with no resident daemon to bind the key
    /// (DRAGON-589). The same action under the same name, with the command that performs it.
    ///
    /// This is what Linux always had, unwritten: its capture keys are the DESKTOP's own
    /// custom shortcuts, each running this binary with a flag. The page now says so per
    /// action instead of leaving the user to find the flag in the README.
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    fn global_capture_item(&self, slot: CaptureHotkeySlot) -> Item<'_> {
        self.global_command_item(slot.label(), slot.flags())
    }

    /// The Global Recording rows, and how many of them are COMMAND rows.
    ///
    /// All of them, today: no platform registers a global RECORDING hotkey, so there is no
    /// chord to edit anywhere. The five are the whole recording-control vocabulary the tray
    /// menu carries, named the way the tray menu and the in-app shortcuts name them
    /// (`RecordingFlag::label`).
    ///
    /// Absent entirely on a build without the transport: macOS and Windows compile a
    /// `run_recording_command` that only prints "Linux-only", so a command row there would
    /// advertise a shortcut that cannot work. Those platforms keep the in-app Recording tab
    /// instead, which is where their chords really do reach a live recording.
    fn global_recording_items(&self) -> (Vec<Item<'_>>, usize) {
        // The second term is the one DRAGON-585 flips when a bindable global recording slot
        // lands beside the capture ones; until then nothing registers these.
        let kind = global_row_kind(crate::cli::recording_commands_supported(), false);
        if kind != GlobalRow::Command {
            return (Vec::new(), 0);
        }
        let items: Vec<Item<'_>> = crate::cli::RECORDING_FLAGS
            .iter()
            .map(|rf| self.global_command_item(rf.label, std::slice::from_ref(&rf.flag)))
            .collect();
        let n = items.len();
        (items, n)
    }

    /// One Global tab row for an action this build performs but cannot BIND: the action's
    /// name, and under it the exact command to run, led by a copy button (DRAGON-589).
    ///
    /// The copy-plus-text pairing is `row::command_line`, the twin of the `row::path_line`
    /// treatment every filesystem path in settings already gets, so the two read as the same
    /// kind of affordance. The command itself comes from `util::launch_command`, which spells
    /// this build the only way that actually starts it (`flatpak run …`, the `.AppImage` file,
    /// or the resolved executable path) and quotes what needs quoting.
    ///
    /// The command is also the row's plain `desc`, which is what the settings search reads, so
    /// a user searching for `--toggle-mic` finds the row that shows it.
    ///
    /// `flush` because the control slot is empty here: the whole row is a label and a helper
    /// line, so the reserved unit and reset slots would only add trailing air (DRAGON-495).
    fn global_command_item<'a>(&self, label: &'a str, flags: &[&str]) -> Item<'a> {
        let command = crate::util::launch_command(flags);
        let copied =
            super::super::row::path_line_copied(&command, &self.settings.health_copied);
        Item::new(label, command.clone(), widget::text(""))
            .desc_el(command_line(command, copied))
            .flush()
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

/// DRAGON-589: which shape a Global tab row takes. Two questions, asked in one order, and
/// getting the order wrong either hides a working action or advertises a broken one.
#[cfg(test)]
mod global_row_kind_tests {
    use super::*;

    /// An action missing from the build is absent whoever would have bound it. This is the
    /// exception the ticket names: `--active-window` under a Flatpak cannot work by ANY route,
    /// so a command row would be a shortcut that fails silently.
    #[test]
    fn an_action_missing_from_the_build_is_never_mentioned() {
        assert_eq!(global_row_kind(false, false), GlobalRow::Absent);
        assert_eq!(global_row_kind(false, true), GlobalRow::Absent);
    }

    /// An action that IS in the build always gets a row. Which shape depends only on whether
    /// the app can register the key itself, and neither answer hides it.
    #[test]
    fn an_action_in_the_build_always_gets_a_row() {
        assert_eq!(global_row_kind(true, true), GlobalRow::Chord);
        assert_eq!(global_row_kind(true, false), GlobalRow::Command);
    }

    /// The two questions are genuinely independent, and the matrix says so exhaustively, so a
    /// later edit cannot quietly make "we cannot bind it" mean "it does not exist".
    #[test]
    fn the_two_questions_stay_separate() {
        for in_build in [false, true] {
            for binds in [false, true] {
                let got = global_row_kind(in_build, binds);
                assert_eq!(got == GlobalRow::Absent, !in_build, "{in_build} {binds}");
                if in_build {
                    assert_eq!(got == GlobalRow::Chord, binds, "{in_build} {binds}");
                }
            }
        }
    }

    /// The chord row is compiled exactly where the seam says the app binds these keys. The
    /// row builder is a `cfg` pair, so a drift between the two would render a command row on
    /// a platform whose decision said Chord (or fail to compile, which is the good case).
    #[test]
    fn the_row_builder_is_compiled_where_the_seam_says_chord() {
        assert_eq!(
            crate::platform::app_registers_capture_hotkeys(),
            cfg!(any(target_os = "macos", target_os = "windows"))
        );
    }
}

/// DRAGON-589: when the Global tab leads with the owner's one sentence.
#[cfg(test)]
mod global_note_tests {
    use super::*;

    /// One command row is enough: that sentence is what explains the command, so it is said
    /// once above everything rather than repeated per row. Arguments are (command rows,
    /// TOTAL rows), so a Linux page is (12, 12) and a mixed one (1, 8).
    #[test]
    fn any_command_row_brings_the_note() {
        assert!(global_note_shown(1, 1));
        assert!(global_note_shown(1, 8));
        assert!(global_note_shown(12, 12));
    }

    /// A page where every global hotkey is bindable says nothing, which is what keeps macOS
    /// and Windows exactly as they were before this ticket.
    #[test]
    fn an_all_bindable_page_stays_silent() {
        assert!(!global_note_shown(0, 7));
        assert!(!global_note_shown(0, 1));
    }

    /// The floor: `ShortcutsTab::occupied` promises the Global tab always has content, so a
    /// build offering no global action at all must still say something.
    #[test]
    fn a_tab_with_no_rows_at_all_still_says_something() {
        assert!(global_note_shown(0, 0));
    }
}

/// DRAGON-589, owner correction: an editor-less capture slot needs a clipboard route that
/// works with no window of ours, because that is the one thing it deliberately does not open.
#[cfg(test)]
mod editor_less_slot_tests {
    use super::editor_less_slot_available;
    use crate::share::CopyRoute;

    /// An ordinary slot opens the editor, which IS a focused window, so both routes serve it.
    #[test]
    fn an_ordinary_slot_is_available_on_either_route() {
        assert!(editor_less_slot_available(false, CopyRoute::Standalone));
        assert!(editor_less_slot_available(false, CopyRoute::ThisWindow));
    }

    /// The twins need a standalone route. A window route cannot serve a capture that opens
    /// no window, so the slot is not offered at all rather than offered and half-working.
    #[test]
    fn a_no_editor_slot_needs_a_standalone_route() {
        assert!(editor_less_slot_available(true, CopyRoute::Standalone));
        assert!(!editor_less_slot_available(true, CopyRoute::ThisWindow));
    }
}

/// DRAGON-617: the sentence a row shows when it refuses a baked chord.
#[cfg(test)]
mod fixed_chord_refusal_tests {
    use super::fixed_chord_refusal;

    /// It NAMES the key, calls it fixed rather than taken, and says what to do next. Each of
    /// the three is asserted separately, because dropping any one of them leaves a sentence
    /// that reads fine and answers nothing.
    #[test]
    fn it_names_the_key_and_says_what_to_do() {
        let note = fixed_chord_refusal("Ctrl+Z");
        assert!(note.starts_with("Ctrl+Z "), "the key must lead: {note}");
        assert!(note.contains("fixed"), "it must say fixed, not taken: {note}");
        assert!(!note.contains("taken"), "'taken' would invite freeing it: {note}");
        assert!(note.contains("Press a different key"), "it must say what to do: {note}");
    }

    /// The macOS glyph forms pass through untouched: the label is rendered upstream by the
    /// one in-app chord formatter and this only frames it.
    #[test]
    fn it_carries_whatever_the_formatter_produced() {
        assert!(fixed_chord_refusal("⌘S").starts_with("⌘S "));
        assert!(fixed_chord_refusal("Esc").starts_with("Esc "));
    }

    /// House copy rule: no em-dash anywhere in user-facing text.
    #[test]
    fn it_stays_em_dash_free() {
        assert!(!fixed_chord_refusal("Ctrl+Y").contains('\u{2014}'));
    }
}
