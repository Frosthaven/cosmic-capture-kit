//! Recording CONTROL commands: drive a recording that is already running, from a second
//! process (DRAGON-583).
//!
//! ## Why these exist
//!
//! Cosmic Capture Kit has three in-app recording shortcuts: toggle the microphone, toggle
//! system audio, and finish and save. On Linux none of them can be delivered to a live
//! recording, and that is structural rather than a bug in the keymap:
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
//! binds a key in their own desktop's shortcut settings and it runs our binary with a flag.
//! `--toggle-mic` is to a running recording what `--region` is to a new capture.
//!
//! ## How a flag reaches the recording
//!
//! Through the EXISTING resident relay, [`crate::daemon_ipc`]: the same newline-delimited
//! [`Command`] words the resident's menu already sends (`pause` / `mic` / `system` / `stop`
//! / `cancel`), landing in the same channel and mapped to the same `RecordingMsg`s. What
//! DRAGON-583 added to that relay is one ADDRESS, not one protocol: the recording child
//! also listens on its own per-pid socket, beside the recording marker `instance.rs`
//! already writes, so a process that is not the resident can reach it whether or not a
//! resident is running. `daemon_ipc`'s own module doc carries the full reasoning.
//!
//! ## What these commands are NOT
//!
//! They are not capture sessions, so nothing here records a [`crate::diag::Failure`]. The
//! same call `cloud::child` makes and for the same reason: no capture is at stake, and a
//! control code in that closed vocabulary would be the second failure vocabulary CLAUDE.md
//! forbids. A failure is a stderr line plus a log line plus a non-zero exit, which is what
//! a CLI command owes its caller.

use crate::daemon_ipc::Command;

/// One recording-control command as the CLI offers it: the flag, the relay command it sends,
/// and the plain-words label every surface names it by.
///
/// `label` was deleted by DRAGON-588 as orphaned and is BACK, deliberately, because
/// DRAGON-589 gives it a reader again: the Global tab lists these five by name, with the
/// command that runs each, exactly as it lists the capture slots. Naming them any other way
/// would be a second vocabulary for five verbs the tray menu and the in-app shortcuts already
/// name, which is the thing DRAGON-588 was right to object to. Three of the five have an
/// in-app [`crate::shortcuts::Action`] too, and their labels are word-for-word that action's
/// (pinned by a test below), so a verb reads the same wherever the user meets it.
pub struct RecordingFlag {
    /// The exact argv token, matched by equality and nothing else.
    pub flag: &'static str,
    /// The relay word it becomes on the wire.
    pub command: Command,
    /// What it does, in the same words the tray menu and the settings rows use.
    pub label: &'static str,
}

/// Every recording-control flag. The list IS the CLI's parsing surface, and `--help`,
/// `CLI.md`, the README and the Settings Global tab all read these five.
///
/// It mirrors [`Command`] one-for-one (pinned by a test below), so the CLI can never offer
/// a verb the relay cannot carry, nor miss one it can.
pub const RECORDING_FLAGS: [RecordingFlag; 5] = [
    RecordingFlag {
        flag: "--pause-recording",
        command: Command::TogglePause,
        label: "Pause or resume recording",
    },
    RecordingFlag {
        flag: "--finish-recording",
        command: Command::Stop,
        label: "Stop and save recording",
    },
    RecordingFlag {
        flag: "--cancel-recording",
        command: Command::Cancel,
        label: "Cancel and delete recording",
    },
    RecordingFlag {
        flag: "--toggle-mic",
        command: Command::ToggleMic,
        label: "Toggle Microphone",
    },
    RecordingFlag {
        flag: "--toggle-system-audio",
        command: Command::ToggleSystemAudio,
        label: "Toggle system audio",
    },
];

/// **Pure**, unit-tested: does THIS BUILD carry the transport these five flags need?
///
/// Honestly a compile-time fact, and one of the few that genuinely is. [`run_recording_command`]
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

/// **Pure**, unit-tested: which recording-control command, if any, this argv asks for.
///
/// Exact token equality, never a prefix match, so `--toggle-mic-later` is not a mic toggle
/// and `--cancel-recording` cannot be reached by `--cancel`. The FIRST recognised token in
/// argv order wins, which is the only ordering a user can predict from what they typed; a
/// keybinding passes exactly one and never sees the rule. `argv[0]` is skipped, because the
/// program's own path is not an argument.
///
/// `None` means "this launch is not a recording-control command", and the caller must then
/// fall through to the ordinary launch parsing untouched.
pub fn recording_command_from_args<S: AsRef<str>>(args: &[S]) -> Option<Command> {
    args.iter().skip(1).find_map(|a| {
        let a = a.as_ref();
        RECORDING_FLAGS
            .iter()
            .find_map(|rf| (rf.flag == a).then_some(rf.command))
    })
}

/// The flag that spells `cmd`, for the messages this module prints. Total by construction:
/// [`RECORDING_FLAGS`] covers every [`Command`] variant, and the fallback only exists
/// because the compiler cannot know that.
fn flag_for(cmd: Command) -> &'static str {
    RECORDING_FLAGS
        .iter()
        .find_map(|rf| (rf.command == cmd).then_some(rf.flag))
        .unwrap_or("--finish-recording")
}

/// Send `cmd` to the live recording and exit. Prints the reason and exits non-zero when
/// there is no recording to reach, matching the other one-shot subcommands here.
///
/// Linux carries the transport; see the module doc and the off-Linux arm below.
#[cfg(target_os = "linux")]
pub fn run_recording_command(cmd: Command) {
    match crate::daemon_ipc::send_command(cmd) {
        Ok(pid) => log::info!("{} delivered to the recording (pid {pid})", flag_for(cmd)),
        Err(why) => {
            // Both channels, like every other detached helper: a terminal run reads it on
            // stderr, and a run from a desktop hotkey (which has no terminal at all) leaves
            // it in the debug log. The message names no path and no capture.
            eprintln!("{}: {why}", flag_for(cmd));
            log::warn!("{}: {why}", flag_for(cmd));
            std::process::exit(1);
        }
    }
}

/// macOS and Windows are LINUX-only's other side, and deliberately untouched by
/// DRAGON-583. Both keep their capture overlay windows through a recording (record start
/// only makes them click-through) and neither takes the keyboard away, so the in-app
/// Shift+Alt chords can still land there; both also have a menu-bar / tray item carrying
/// the same five controls. Nothing there was broken, so nothing there was changed, down to
/// the transport not being compiled.
///
/// The flags are still PARSED on every platform, and that is the point of this arm: an
/// argument `main` does not recognise falls through to a normal launch and opens a capture
/// overlay, so a `--finish-recording` typed on the wrong machine must end HERE, saying what
/// to use instead, rather than starting a screenshot.
#[cfg(not(target_os = "linux"))]
pub fn run_recording_command(cmd: Command) {
    let flag = flag_for(cmd);
    let why = "recording commands are Linux-only; on this platform use the in-app \
               recording shortcuts or the menu bar / tray recording controls";
    eprintln!("{flag}: {why}");
    log::warn!("{flag}: {why}");
    std::process::exit(1);
}

#[cfg(test)]
mod recording_command_argv_tests {
    use super::*;

    fn argv(rest: &[&str]) -> Vec<String> {
        std::iter::once("cosmic-capture-kit")
            .chain(rest.iter().copied())
            .map(String::from)
            .collect()
    }

    /// Every advertised flag resolves to its relay command, and the pairing is the one
    /// `--help` / CLI.md / the README document.
    #[test]
    fn each_flag_sends_its_own_command() {
        for rf in RECORDING_FLAGS {
            assert_eq!(
                recording_command_from_args(&argv(&[rf.flag])),
                Some(rf.command),
                "{}",
                rf.flag
            );
            assert!(rf.flag.starts_with("--"), "{} must be a long flag", rf.flag);
            assert!(!rf.label.is_empty(), "{} needs a label every surface can show", rf.flag);
        }
    }

    /// DRAGON-589: the three verbs that ALSO have an in-app shortcut are named word-for-word
    /// the way that shortcut is named. The Global tab and the Recording tab can show the same
    /// verb on one machine, so two spellings would read as two different features.
    #[test]
    fn a_verb_with_an_in_app_action_shares_its_label() {
        use crate::shortcuts::Action;
        for (cmd, action) in [
            (Command::Stop, Action::RecordStop),
            (Command::ToggleMic, Action::RecordToggleMic),
            (Command::ToggleSystemAudio, Action::RecordToggleSystemAudio),
        ] {
            let label = RECORDING_FLAGS
                .iter()
                .find(|rf| rf.command == cmd)
                .map(|rf| rf.label)
                .expect("every relay command has a flag");
            assert_eq!(label, action.label(), "{cmd:?}");
        }
    }

    /// The build gate is exactly "did the Linux transport get compiled in", and nothing else.
    /// Where it is false the flags still PARSE (so a stray one cannot open a capture overlay)
    /// but they cannot reach a recording, which is why Settings must not advertise them.
    #[test]
    fn the_build_gate_tracks_the_compiled_transport() {
        assert_eq!(recording_commands_supported(), cfg!(target_os = "linux"));
    }

    /// The flag list must stay a complete, one-to-one mirror of the relay vocabulary: a
    /// `Command` with no flag is a verb the CLI silently cannot reach, and two flags for one
    /// command is two spellings of the same key.
    #[test]
    fn the_flags_mirror_every_relay_command_exactly() {
        let all = [
            Command::TogglePause,
            Command::ToggleMic,
            Command::ToggleSystemAudio,
            Command::Stop,
            Command::Cancel,
        ];
        for cmd in all {
            let spellings = RECORDING_FLAGS.iter().filter(|rf| rf.command == cmd).count();
            assert_eq!(spellings, 1, "{cmd:?} must have exactly one flag");
            // And the reverse lookup the messages use agrees with the table.
            assert_eq!(
                RECORDING_FLAGS
                    .iter()
                    .find(|rf| rf.flag == flag_for(cmd))
                    .map(|rf| rf.command),
                Some(cmd)
            );
        }
        assert_eq!(RECORDING_FLAGS.len(), all.len(), "no flag may name a verb twice");
    }

    /// An ordinary launch must be untouched: nothing here may claim a capture flag, and a
    /// bare launch stays a bare launch.
    #[test]
    fn an_ordinary_launch_is_never_claimed() {
        assert_eq!(recording_command_from_args(&argv(&[])), None);
        for other in [
            "--region", "--window", "--monitor", "--video", "--image", "--scan", "--settings",
            "--no-editor", "--all-in-one", "--audio", "both", "--preview", "--countdown", "3",
        ] {
            assert_eq!(recording_command_from_args(&argv(&[other])), None, "{other}");
        }
    }

    /// Exact tokens only. A prefix, a suffix, an `=` form or a near miss must fall through
    /// to the ordinary launch parsing rather than silently controlling a recording.
    #[test]
    fn only_an_exact_token_counts() {
        for near in [
            "--toggle-mic-later",
            "-toggle-mic",
            "--toggle-mic=1",
            "toggle-mic",
            "--cancel",
            "--finish",
            "--pause",
            "--Toggle-Mic",
            "--toggle-system",
        ] {
            assert_eq!(recording_command_from_args(&argv(&[near])), None, "{near}");
        }
    }

    /// argv[0] is the program, not an argument: a binary that happens to be NAMED like a
    /// flag must not control a recording.
    #[test]
    fn the_program_name_is_not_an_argument() {
        let args = vec!["--finish-recording".to_string()];
        assert_eq!(recording_command_from_args(&args), None);
    }

    /// With several passed, the first in ARGV order wins, whatever order the table lists
    /// them in. Pinned in both directions so the rule cannot quietly become "the table's
    /// order".
    #[test]
    fn the_first_flag_in_argv_wins() {
        assert_eq!(
            recording_command_from_args(&argv(&["--toggle-mic", "--finish-recording"])),
            Some(Command::ToggleMic)
        );
        assert_eq!(
            recording_command_from_args(&argv(&["--finish-recording", "--toggle-mic"])),
            Some(Command::Stop)
        );
        // Unrelated arguments before ours do not shadow it.
        assert_eq!(
            recording_command_from_args(&argv(&["--video", "--cancel-recording"])),
            Some(Command::Cancel)
        );
    }
}
