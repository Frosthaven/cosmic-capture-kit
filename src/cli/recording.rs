//! Recording CONTROL commands: drive a recording that is already running, from a second
//! process (DRAGON-583, respelled by DRAGON-695).
//!
//! ## Why these exist
//!
//! Cosmic Capture Kit has in-app recording shortcuts. On Linux none of them can be delivered
//! to a live recording, and that is structural rather than a bug in the keymap:
//!
//! * There is no focus-free binding to be had. The xdg-desktop-portal `GlobalShortcuts`
//!   interface does not exist on COSMIC (a `busctl introspect` of the portal returns an
//!   empty introspection for it), so `platform::global_shortcuts` fails its bind and flips
//!   `dead` the moment a recording starts, exactly as its module doc predicts.
//! * Nothing of ours holds the keyboard either. At record start a native session hands
//!   focus straight BACK to the window being recorded, on purpose, so the user can type
//!   into the app they are recording; the portal-fallback session destroys its one
//!   toplevel outright. The keystroke therefore goes to whatever the user is really using.
//!
//! So on Linux these become what every other Linux hotkey in this app already is: the user
//! binds a key in their own desktop's shortcut settings and it runs our binary with a
//! command. `record --sound mic` is to a running recording what `capture --region` is to a
//! new capture.
//!
//! ## The spelling changed, the mechanism did not
//!
//! DRAGON-583 shipped these as five flat flags, one verb each, with no command word in
//! front of them. DRAGON-695's subcommand grammar folds them into `record`, under the same three
//! options the help lists: `--action pause|save|cancel`, `--tool
//! pointer|marker|magic-marker|erase`, `--sound both|mic|system|none`. Two things changed with
//! them and both are improvements:
//!
//! * The two audio verbs were TOGGLES and are now ABSOLUTE. A key bound to "toggle the mic"
//!   does the wrong thing half the time, because the process pressing it cannot see the arm
//!   it is flipping. `--sound mic` says what the user meant, whatever the recording was doing.
//!   The wire word is [`Command::SetAudioArms`]; see its doc for why an absolute verb is the
//!   only correct spelling from a process with no snapshot.
//! * Controls COMBINE. One invocation may carry a sound, a tool and an action, and they apply
//!   in that order, so a single key can (say) unmute and finish.
//!
//! ## How a command reaches the recording
//!
//! Through the EXISTING resident relay, [`crate::daemon_ipc`]: the same newline-delimited
//! [`Command`] words the resident's menu already sends, landing in the same channel and mapped
//! to the same `RecordingMsg`s. What DRAGON-583 added to that relay is one ADDRESS, not one
//! protocol: the recording child also listens on its own per-pid socket, beside the recording
//! marker `instance.rs` already writes, so a process that is not the resident can reach it
//! whether or not a resident is running. `daemon_ipc`'s own module doc carries the full
//! reasoning.
//!
//! ## What these commands are NOT
//!
//! They are not capture sessions, so nothing here records a [`crate::diag::Failure`]. The
//! same call `cloud::child` makes and for the same reason: no capture is at stake, and a
//! control code in that closed vocabulary would be the second failure vocabulary CLAUDE.md
//! forbids. A failure is a stderr line plus a log line plus a non-zero exit, which is what
//! a CLI command owes its caller.

use crate::errln;

use crate::daemon_ipc::Command;
use crate::recording_ui::AudioArmState;

/// One recording-control command as the CLI offers it: the argv that runs it, the relay
/// command it sends, and the plain-words label every surface names it by.
///
/// `label` earns its place through DRAGON-589: the Settings Global tab lists these by name,
/// with the command that runs each, exactly as it lists the capture slots. Naming them any
/// other way would be a second vocabulary for verbs the tray menu and the in-app shortcuts
/// already name. Where a verb HAS an in-app [`crate::shortcuts::Action`], its label is
/// word-for-word that action's (pinned by a test below), so a verb reads the same wherever
/// the user meets it.
pub struct RecordingCommand {
    /// The exact argv, after the program name. Multi-token since DRAGON-695: the grammar is
    /// `record --action save`, not one flag.
    pub argv: &'static [&'static str],
    /// The relay word it becomes on the wire.
    pub command: Command,
    /// What it does, in the same words the tray menu and the settings rows use.
    pub label: &'static str,
}

/// Every recording-control command, in the order the help and the Settings Global tab list
/// them: the three actions, then the four audio states, then the four annotation tools.
///
/// This is the ONE table `--help`, `CLI.md`, the README and the Settings Global tab read. It
/// is not what [`crate::cli::parse`] matches on (a parser that takes VALUES cannot be a table
/// lookup), but a test below runs every entry's argv through that parser and demands the
/// listed command back, so the two cannot drift: an entry that stops parsing, or parses to
/// something else, fails the suite.
pub const RECORDING_COMMANDS: [RecordingCommand; 11] = [
    RecordingCommand {
        argv: &["record", "--action", "pause"],
        command: Command::TogglePause,
        label: "Pause or resume recording",
    },
    RecordingCommand {
        argv: &["record", "--action", "save"],
        command: Command::Stop,
        label: "Stop and save recording",
    },
    RecordingCommand {
        argv: &["record", "--action", "cancel"],
        command: Command::Cancel,
        label: "Cancel and delete recording",
    },
    RecordingCommand {
        argv: &["record", "--sound", "both"],
        command: Command::SetAudioArms(AudioArmState::Both),
        label: "Record microphone and system audio",
    },
    RecordingCommand {
        argv: &["record", "--sound", "mic"],
        command: Command::SetAudioArms(AudioArmState::MicrophoneOnly),
        label: "Record the microphone only",
    },
    RecordingCommand {
        argv: &["record", "--sound", "system"],
        command: Command::SetAudioArms(AudioArmState::SystemOnly),
        label: "Record system audio only",
    },
    RecordingCommand {
        argv: &["record", "--sound", "none"],
        command: Command::SetAudioArms(AudioArmState::None),
        label: "Record no audio",
    },
    RecordingCommand {
        argv: &["record", "--tool", "pointer"],
        command: Command::InkTool(crate::ink::InkTool::Pointer),
        label: "Live annotations: Pointer",
    },
    RecordingCommand {
        argv: &["record", "--tool", "marker"],
        command: Command::InkTool(crate::ink::InkTool::Marker),
        label: "Live annotations: Marker",
    },
    RecordingCommand {
        argv: &["record", "--tool", "magic-marker"],
        command: Command::InkTool(crate::ink::InkTool::MagicMarker),
        label: "Live annotations: Magic Marker",
    },
    RecordingCommand {
        argv: &["record", "--tool", "erase"],
        command: Command::InkClear,
        label: "Live annotations: Clear Marks",
    },
];

/// The `scan --action refresh` argv and its label, listed beside the recording commands on the
/// Settings Global tab. Not a [`RecordingCommand`]: it reaches a scanner, not a recording, and
/// carries no [`Command`] at all.
pub const SCAN_REFRESH_COMMAND: (&[&str], &str) =
    (&["scan", "--action", "refresh"], "Rescan the screen");

/// **Pure**, unit-tested: does THIS BUILD carry the transport these commands need?
///
/// Honestly a compile-time fact, and one of the few that genuinely is. [`run_recording_commands`]
/// has a Linux body that talks to the relay and an off-Linux body that prints "Linux-only" and
/// exits non-zero; nothing about the running session can change which one was compiled in. So
/// this is a `cfg!`, named once here rather than repeated as a bare `cfg` at each caller.
///
/// Read by the Settings Global tab, which must not print a command that cannot work: an action
/// missing from the build gets no row at all, while one the app merely cannot BIND gets a row
/// with its command (DRAGON-589).
pub const fn recording_commands_supported() -> bool {
    cfg!(target_os = "linux")
}

/// How one batch of commands is spelled back to the user in a message: the argv the parser
/// would have to see to produce it. Total by construction over the table, with a fallback the
/// compiler cannot know is unreachable.
fn spelling(cmds: &[Command]) -> String {
    let mut out = String::from("record");
    for cmd in cmds {
        match RECORDING_COMMANDS.iter().find(|rc| rc.command == *cmd) {
            Some(rc) => {
                for token in rc.argv.iter().skip(1) {
                    out.push(' ');
                    out.push_str(token);
                }
            }
            None => out.push_str(" <command>"),
        }
    }
    out
}

/// Whether this batch asks for the audio arms, which is the one case whose failure message
/// gains a second line: a user whose recording is not running probably meant the persisted
/// default, and that default has its own command.
fn touches_sound(cmds: &[Command]) -> bool {
    cmds.iter().any(|c| matches!(c, Command::SetAudioArms(_)))
}

/// The extra line a `--sound` failure carries. Pure so the wording is pinned by a test rather
/// than by reading the source.
fn sound_hint() -> &'static str {
    "to change the default, use: sound <both|mic|system|none>"
}

/// Send `cmds` to the live recording, in order, and exit. Prints the reason and exits non-zero
/// when there is no recording to reach, matching the other one-shot subcommands here.
///
/// The batch stops at the FIRST failure: a `--sound mic --action save` against a recording
/// that has already ended has nothing to save either, and reporting one reason is clearer than
/// reporting the same one three times.
///
/// Linux carries the transport; see the module doc and the off-Linux arm below.
#[cfg(target_os = "linux")]
pub fn run_recording_commands(cmds: &[Command]) {
    for cmd in cmds {
        match crate::daemon_ipc::send_command(*cmd) {
            Ok(pid) => log::info!("{} delivered to the recording (pid {pid})", cmd.as_str()),
            Err(why) => {
                // Both channels, like every other detached helper: a terminal run reads it on
                // stderr, and a run from a desktop hotkey (which has no terminal at all) leaves
                // it in the debug log. The message names no path and no capture.
                let what = spelling(cmds);
                errln!("{what}: {why}");
                log::warn!("{what}: {why}");
                if touches_sound(cmds) {
                    errln!("{}", sound_hint());
                }
                std::process::exit(1);
            }
        }
    }
}

/// macOS and Windows are LINUX-only's other side, and deliberately untouched by
/// DRAGON-583. Both keep their capture overlay windows through a recording (record start
/// only makes them click-through) and neither takes the keyboard away, so the in-app
/// Shift+Alt chords can still land there; both also have a menu-bar / tray item carrying
/// the same controls. Nothing there was broken, so nothing there was changed, down to
/// the transport not being compiled.
///
/// The commands are still PARSED on every platform, and that is the point of this arm: a
/// `record --action save` typed on the wrong machine must end HERE, saying what to use
/// instead, rather than doing nothing at all.
#[cfg(not(target_os = "linux"))]
pub fn run_recording_commands(cmds: &[Command]) {
    let what = spelling(cmds);
    let why = "recording commands are Linux-only; on this platform use the in-app \
               recording shortcuts or the menu bar / tray recording controls";
    errln!("{what}: {why}");
    log::warn!("{what}: {why}");
    if touches_sound(cmds) {
        errln!("{}", sound_hint());
    }
    std::process::exit(1);
}

#[cfg(test)]
mod recording_command_table_tests {
    use super::*;
    use crate::cli::parse::{parse, Plan};

    fn argv(rest: &[&str]) -> Vec<String> {
        std::iter::once("cosmic-capture-kit")
            .chain(rest.iter().copied())
            .map(String::from)
            .collect()
    }

    /// THE single-source pin. Every advertised argv really parses, and really produces the
    /// command this table says it does. The table is what the help and Settings read; the
    /// parser is what the user's key actually runs. This test is the only thing that keeps
    /// them one source.
    #[test]
    fn every_listed_command_parses_to_exactly_itself() {
        for rc in RECORDING_COMMANDS {
            assert_eq!(
                parse(&argv(rc.argv)),
                Ok(Plan::RecordControl(vec![rc.command])),
                "{:?}",
                rc.argv
            );
            assert_eq!(rc.argv.first(), Some(&"record"), "{:?}", rc.argv);
            assert!(!rc.label.is_empty(), "{:?} needs a label every surface can show", rc.argv);
        }
    }

    /// The scan refresh row is advertised the same way and must parse the same way.
    #[test]
    fn the_scan_refresh_row_parses_too() {
        let (argv_tokens, label) = SCAN_REFRESH_COMMAND;
        assert_eq!(parse(&argv(argv_tokens)), Ok(Plan::ScanRefresh));
        assert!(!label.is_empty());
    }

    /// DRAGON-589: a verb that ALSO has an in-app shortcut is named word-for-word the way that
    /// shortcut is named. The Global tab and the Recording tab can show the same verb on one
    /// machine, so two spellings would read as two different features.
    ///
    /// Only ONE verb qualifies since DRAGON-695. The two audio rows that used to share labels
    /// with `RecordToggleMic` / `RecordToggleSystemAudio` were TOGGLES; the CLI's audio
    /// commands are absolute now, so "Toggle Microphone" would be an actively wrong name for
    /// `record --sound mic` and sharing it would be worse than not sharing it.
    #[test]
    fn a_verb_with_an_in_app_action_shares_its_label() {
        use crate::shortcuts::Action;
        let label = RECORDING_COMMANDS
            .iter()
            .find(|rc| rc.command == Command::Stop)
            .map(|rc| rc.label)
            .expect("finish and save is in the table");
        assert_eq!(label, Action::RecordStop.label());
    }

    /// The build gate is exactly "did the Linux transport get compiled in", and nothing else.
    #[test]
    fn the_build_gate_tracks_the_compiled_transport() {
        assert_eq!(recording_commands_supported(), cfg!(target_os = "linux"));
    }

    /// The table must stay a complete, one-to-one mirror of the relay vocabulary the CLI can
    /// reach: a command with no argv is a verb the CLI silently cannot send, and two argvs for
    /// one command are two spellings of the same key.
    ///
    /// The two TOGGLE verbs are deliberately absent. They still exist on the wire, because the
    /// tray menu sends them (it can see the arms it is flipping), and they are deliberately not
    /// offered to a process that cannot.
    #[test]
    fn the_table_mirrors_every_reachable_relay_command_exactly() {
        let reachable = [
            Command::TogglePause,
            Command::Stop,
            Command::Cancel,
            Command::SetAudioArms(AudioArmState::Both),
            Command::SetAudioArms(AudioArmState::MicrophoneOnly),
            Command::SetAudioArms(AudioArmState::SystemOnly),
            Command::SetAudioArms(AudioArmState::None),
            Command::InkTool(crate::ink::InkTool::Pointer),
            Command::InkTool(crate::ink::InkTool::Marker),
            Command::InkTool(crate::ink::InkTool::MagicMarker),
            Command::InkClear,
        ];
        for cmd in reachable {
            let spellings = RECORDING_COMMANDS.iter().filter(|rc| rc.command == cmd).count();
            assert_eq!(spellings, 1, "{cmd:?} must have exactly one argv");
        }
        assert_eq!(RECORDING_COMMANDS.len(), reachable.len(), "no argv may name a verb twice");
        for toggle in [Command::ToggleMic, Command::ToggleSystemAudio] {
            assert!(
                !RECORDING_COMMANDS.iter().any(|rc| rc.command == toggle),
                "{toggle:?} is a tray-only verb: a CLI process cannot see the arm it flips"
            );
        }
    }

    /// The no-near-miss guarantee, carried over from DRAGON-583 in the shape the new grammar
    /// gives it. The old rule was "a bare `toggle-mic` must not parse"; under subcommands the
    /// same worry is that an ordinary launch could be mistaken for a control, or the reverse.
    #[test]
    fn an_ordinary_launch_is_never_mistaken_for_a_control() {
        for line in [
            vec!["capture", "--region"],
            vec!["record", "--region"],
            vec!["record", "--monitor", "--sound", "mic"],
            vec!["scan"],
            vec!["settings"],
            vec!["tray"],
        ] {
            assert!(
                !matches!(parse(&argv(&line)), Ok(Plan::RecordControl(_))),
                "{line:?} is a launch, not a control"
            );
        }
        // And the reverse: a control never turns into a launch.
        for line in [
            vec!["record", "--action", "save"],
            vec!["record", "--sound", "none"],
            vec!["record", "--tool", "erase"],
        ] {
            assert!(
                matches!(parse(&argv(&line)), Ok(Plan::RecordControl(_))),
                "{line:?} is a control, not a launch"
            );
        }
    }

    /// The spelling a failure message uses is the argv the user typed, so "no recording is in
    /// progress" is attributed to something they can retype.
    #[test]
    fn a_failure_names_the_command_it_was() {
        assert_eq!(spelling(&[Command::Stop]), "record --action save");
        assert_eq!(
            spelling(&[
                Command::SetAudioArms(AudioArmState::MicrophoneOnly),
                Command::InkClear,
            ]),
            "record --sound mic --tool erase"
        );
        assert!(touches_sound(&[Command::SetAudioArms(AudioArmState::None)]));
        assert!(!touches_sound(&[Command::Stop, Command::InkClear]));
        // The hint points at the command that DOES persist a default, so a user who reached
        // this by accident is one line from what they meant.
        assert!(sound_hint().contains("sound <both|mic|system|none>"));
    }
}
