//! Recording control IPC between the resident menu-bar DAEMON and a recording
//! capture CHILD (macOS, DRAGON-170).
//!
//! ## Why this exists
//!
//! The resident daemon (`crate::daemon`) owns the ONE menu-bar item and spawns the
//! full app as one-shot capture children. Before this, a recording child raised its
//! OWN second NSStatusItem (`crate::tray` = `platform/mac/tray.rs`) with the in-recording
//! controls, so a daemon-spawned recording showed TWO menu-bar items. DRAGON-170
//! consolidates: while a daemon is present, the in-recording actions (pause/resume,
//! mic toggle, system-audio toggle, stop) move INTO the daemon's menu, and the
//! daemon's single icon flips to a recording state. The child no longer raises its
//! own item; it RELAYS its recording state to the daemon and receives the daemon's
//! menu commands.
//!
//! ## The one-shot model is inviolable
//!
//! The child still OWNS the recording and `finish_session`. The daemon only relays:
//! a menu click becomes a command line the child turns into the exact same
//! `RecordingMsg` the in-frame toolbar / Linux tray already dispatch. If no daemon
//! is listening (a terminal/CLI recording, or the daemon is off), the child keeps
//! its own control surface (the `platform/mac/tray.rs` NSStatusItem / in-frame toolbar) exactly
//! as before, so non-daemon recordings never lose their controls.
//!
//! ## Transport
//!
//! A dependency-free Unix domain socket at
//! `{runtime_dir}/cosmic-capture-kit-recording.sock`. The daemon LISTENS; the child
//! CONNECTS at recording start. The protocol is newline-delimited text (mirroring
//! `instance.rs`'s pid-file simplicity):
//!
//! * child -> daemon: a full `RecordingState` snapshot on every change
//!   (`state <recording> <paused> <mic> <system>`), idempotent so a missed delta
//!   never desyncs the menu; and the connection CLOSING (EOF) means "recording
//!   ended / child gone" so the daemon reverts even on a child crash.
//! * daemon -> child: a single `Command` word per menu click (`pause` / `mic` /
//!   `system` / `stop`).
//!
//! The wire encode/decode live here and are unit-tested with no socket or AppKit. The
//! socket plumbing (daemon listener thread, child connect) is the thin platform layer in
//! `platform/mac/daemon.rs` / `platform/mac/tray.rs`. The pure icon-state / label / menu-model decisions moved
//! to the portable [`crate::recording_ui`] module (DRAGON-172) so every control surface —
//! including the Linux tray and the future Linux resident process — shares one source;
//! this module re-exports them for existing call sites.

pub use crate::recording_ui::{icon_svg, IconState};

/// The recording state a child reports to the daemon. A full snapshot (not deltas)
/// so any single lost line self-heals on the next update; `recording = false` is the
/// explicit "ended" state (the socket closing says the same for a crash).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct RecordingState {
    /// Whether a recording is in progress at all (drives the icon + menu visibility).
    pub recording: bool,
    /// Whether the in-progress recording is paused (menu label Pause vs Resume).
    pub paused: bool,
    /// Whether the microphone is armed (menu checkmark).
    pub mic: bool,
    /// Whether system audio is armed (menu checkmark).
    pub system: bool,
}

impl RecordingState {
    /// Encode as a single newline-terminated wire line the daemon parses.
    pub fn encode(&self) -> String {
        format!(
            "state {} {} {} {}\n",
            self.recording as u8, self.paused as u8, self.mic as u8, self.system as u8
        )
    }

    /// Parse one wire line back into a state. Returns `None` for any line that is not
    /// a well-formed `state <0|1> <0|1> <0|1> <0|1>` (a truncated / garbage read is
    /// ignored, never misapplied). Trailing newline / whitespace is tolerated.
    pub fn parse(line: &str) -> Option<Self> {
        let mut it = line.split_whitespace();
        if it.next()? != "state" {
            return None;
        }
        let mut bit = || match it.next()? {
            "0" => Some(false),
            "1" => Some(true),
            _ => None,
        };
        let recording = bit()?;
        let paused = bit()?;
        let mic = bit()?;
        let system = bit()?;
        // Reject extra tokens so a longer future line can't be half-read as this one.
        if it.next().is_some() {
            return None;
        }
        Some(RecordingState { recording, paused, mic, system })
    }
}

/// A control the daemon menu sends back to the child. Mirrors `tray::TrayEvent` so the
/// child maps each straight onto the existing recording action plumbing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Pause the recording, or resume it when paused.
    TogglePause,
    /// Toggle the microphone arm.
    ToggleMic,
    /// Toggle system-audio arm.
    ToggleSystemAudio,
    /// Finish (stop + save) the recording.
    Stop,
    /// Cancel the recording and discard its file (the toolbar trash action).
    Cancel,
}

impl Command {
    /// The single wire word for this command.
    pub fn as_str(self) -> &'static str {
        match self {
            Command::TogglePause => "pause",
            Command::ToggleMic => "mic",
            Command::ToggleSystemAudio => "system",
            Command::Stop => "stop",
            Command::Cancel => "cancel",
        }
    }

    /// Encode as a newline-terminated wire line the child reads.
    pub fn encode(self) -> String {
        format!("{}\n", self.as_str())
    }

    /// Parse one wire word/line back into a command; `None` for anything unknown.
    pub fn parse(line: &str) -> Option<Self> {
        match line.trim() {
            "pause" => Some(Command::TogglePause),
            "mic" => Some(Command::ToggleMic),
            "system" => Some(Command::ToggleSystemAudio),
            "stop" => Some(Command::Stop),
            "cancel" => Some(Command::Cancel),
            _ => None,
        }
    }
}

/// Whether the daemon should show the in-recording action group + flip the icon.
/// True exactly while a recording is in progress; a paused recording is still "in
/// progress" (the actions stay, the Pause item just reads Resume).
pub fn shows_recording_controls(state: &RecordingState) -> bool {
    state.recording
}

/// The icon state for a reported recording state (see [`IconState`]) — a thin adapter
/// over the portable [`crate::recording_ui::icon_state`].
pub fn icon_state(state: &RecordingState) -> IconState {
    crate::recording_ui::icon_state(state.recording, state.paused)
}

/// The socket the resident (macOS daemon / Linux resident) listens on and the recording
/// child connects to. One path per user runtime dir, matching `instance.rs`'s lock-file
/// convention. Portable (DRAGON-173): the Linux resident and its recording children speak
/// the exact same protocol over the exact same socket. `not(windows)`: Windows carries the
/// SAME wire protocol but over a NAMED PIPE (there is no std Unix socket on Windows), so it
/// uses [`pipe_name`] instead.
#[cfg(not(windows))]
pub fn socket_path() -> String {
    format!("{}/cosmic-capture-kit-recording.sock", crate::util::runtime_dir())
}

/// Windows (DRAGON-237): the named pipe the resident tray daemon listens on and a recording
/// child connects to — the transport analog of [`socket_path`], carrying the identical
/// newline-delimited [`RecordingState`]/[`Command`] wire protocol. `Local\`-scoped by the
/// `\\.\pipe\` convention (the whole pipe namespace is machine-local); one name for the
/// whole login session, like the unix socket is one per runtime dir.
#[cfg(windows)]
pub fn pipe_name() -> &'static str {
    r"\\.\pipe\cosmic-capture-kit-recording"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_every_flag_combo() {
        for r in [false, true] {
            for p in [false, true] {
                for m in [false, true] {
                    for s in [false, true] {
                        let st = RecordingState { recording: r, paused: p, mic: m, system: s };
                        let line = st.encode();
                        assert!(line.ends_with('\n'), "must be newline-terminated: {line:?}");
                        assert_eq!(RecordingState::parse(&line), Some(st), "round trip {st:?}");
                        // Also parse without the trailing newline (line readers strip it).
                        assert_eq!(RecordingState::parse(line.trim_end()), Some(st));
                    }
                }
            }
        }
    }

    #[test]
    fn state_parse_rejects_malformed() {
        assert_eq!(RecordingState::parse(""), None);
        assert_eq!(RecordingState::parse("state"), None);
        assert_eq!(RecordingState::parse("state 1 0 1"), None); // too few
        assert_eq!(RecordingState::parse("state 1 0 1 0 1"), None); // too many
        assert_eq!(RecordingState::parse("state 2 0 0 0"), None); // non-bit
        assert_eq!(RecordingState::parse("nope 1 0 1 0"), None); // wrong tag
        assert_eq!(RecordingState::parse("state x y z w"), None);
    }

    #[test]
    fn command_round_trips() {
        for cmd in [
            Command::TogglePause,
            Command::ToggleMic,
            Command::ToggleSystemAudio,
            Command::Stop,
            Command::Cancel,
        ] {
            let line = cmd.encode();
            assert!(line.ends_with('\n'));
            assert_eq!(Command::parse(&line), Some(cmd), "round trip {cmd:?}");
            assert_eq!(Command::parse(cmd.as_str()), Some(cmd));
        }
        assert_eq!(Command::parse("bogus"), None);
        assert_eq!(Command::parse(""), None);
    }

    #[test]
    fn controls_and_icon_follow_recording_flag() {
        let idle = RecordingState::default();
        assert!(!shows_recording_controls(&idle));
        assert_eq!(icon_state(&idle), IconState::Idle);

        // Any recording state (incl. paused) shows controls; the icon distinguishes
        // live recording from paused.
        let rec = RecordingState { recording: true, paused: false, mic: true, system: false };
        assert!(shows_recording_controls(&rec));
        assert_eq!(icon_state(&rec), IconState::Recording);
        let paused = RecordingState { paused: true, ..rec };
        assert!(shows_recording_controls(&paused));
        assert_eq!(icon_state(&paused), IconState::Paused);
    }

    // The three-state icon geometry is pinned in `crate::recording_ui`'s tests now (the
    // SVGs + `icon_svg` moved there, DRAGON-172); this module keeps only the wire +
    // adapter tests.

    // End-to-end over a real Unix socket pair (no AppKit): the child's encoded state
    // lines arrive and parse on the resident side, and a resident command arrives + parses
    // on the child side. This pins the exact framing the resident/child threads rely on.
    // Portable (DRAGON-173): the Linux resident + its children speak this exact protocol,
    // so it runs on both OSes.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn wire_round_trips_over_a_unix_socket() {
        use std::io::{BufRead as _, BufReader, Write as _};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir();
        let path = dir.join(format!("cck-ipc-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind");

        // Child connects, sends two state snapshots, then reads one command.
        let child_path = path.clone();
        let child = std::thread::spawn(move || {
            let mut conn = std::os::unix::net::UnixStream::connect(&child_path).expect("connect");
            let s1 = RecordingState { recording: true, paused: false, mic: true, system: false };
            let s2 = RecordingState { recording: true, paused: true, mic: true, system: true };
            conn.write_all(s1.encode().as_bytes()).unwrap();
            conn.write_all(s2.encode().as_bytes()).unwrap();
            conn.flush().unwrap();
            // Now read a command line the daemon side sends back.
            let mut r = BufReader::new(conn);
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            Command::parse(&line)
        });

        // Daemon side: accept, read the two states, send a Stop command back.
        let (stream, _) = listener.accept().expect("accept");
        let mut writer = stream.try_clone().unwrap();
        let mut r = BufReader::new(stream);
        let mut l1 = String::new();
        let mut l2 = String::new();
        r.read_line(&mut l1).unwrap();
        r.read_line(&mut l2).unwrap();
        assert_eq!(
            RecordingState::parse(&l1),
            Some(RecordingState { recording: true, paused: false, mic: true, system: false })
        );
        assert_eq!(
            RecordingState::parse(&l2),
            Some(RecordingState { recording: true, paused: true, mic: true, system: true })
        );
        writer.write_all(Command::Stop.encode().as_bytes()).unwrap();
        writer.flush().unwrap();

        assert_eq!(child.join().unwrap(), Some(Command::Stop));
        let _ = std::fs::remove_file(&path);
    }

    // The Pause/Resume label is pinned in `crate::recording_ui`'s tests now (the ONE
    // portable source every surface, incl. the mac daemon + Linux resident, renders from);
    // this module keeps only the wire + adapter tests.
}
