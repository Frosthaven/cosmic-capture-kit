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

// DRAGON-541 dropped `icon_svg` from this re-export. Every tinted surface (the Linux trays,
// the Windows tray) now goes through `recording_ui::tray_icon_svg` instead, which recolours
// the template rather than handing back the raw black one, and macOS — the only caller that
// still wants the untinted template, because AppKit tints its menu-bar image itself — imports
// it from `recording_ui` directly. Re-exporting a name with one consumer left only hid where
// that consumer actually reads from.
pub use crate::recording_ui::IconState;

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

// `shows_recording_controls(state) -> bool` lived here from DRAGON-170 until DRAGON-574:
// every menu renderer branched on it to pick idle-vs-recording. The ONE portable
// `recording_ui::tray_menu(recording, paused)` model now takes the flag directly, so the
// branch (and the predicate) had no caller left. The rule it encoded survives unchanged:
// a PAUSED recording is still "in progress" (`RecordingState.recording` stays true while
// paused, so the controls stay and the Pause item just reads Resume), pinned by
// `controls_and_icon_follow_recording_flag` below.

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

// ── The recording child's OWN control inlet (DRAGON-583) ─────────────────────
//
// ## Why the relay grew a second ADDRESS (and not a second protocol)
//
// Everything above is the RESIDENT relay: the resident listens on the one well-known
// [`socket_path`], the recording child connects to it, and the resident's menu clicks
// travel back as [`Command`] words. That is the only cross-process way to drive a live
// recording, and it has two properties that make it unreachable from a THIRD process:
// the listener is the resident, and with no resident running there is no socket at all
// (the child raises its own tray instead, see `platform/linux/tray.rs`'s `OwnBacking`).
//
// DRAGON-583 needs exactly that third process. On Linux the three in-app recording
// shortcuts (toggle mic, toggle system audio, finish and save) cannot be delivered: the
// xdg-desktop-portal `GlobalShortcuts` interface does not exist on COSMIC (so
// `platform::global_shortcuts` flips `dead` immediately), and during a recording no
// surface of ours holds keyboard focus, because the capture overlays are torn down at
// record start. The answer is the same shape as every other Linux hotkey here: the user
// binds a key in their own desktop settings and it runs our binary with a flag. That
// flag's process then has to reach the recording.
//
// So the relay gains ONE address, not one protocol. The verbs are [`Command`] verbatim,
// the framing is the same newline-delimited plain words, and the received command is fed
// into the SAME channel a resident's command lands in, so the app side is byte-identical
// either way. What is new is that the RECORDING CHILD also listens, on its own per-pid
// socket, whether or not a resident exists. That sidesteps the resident entirely: its
// socket, its connection and its menu are untouched, which is what keeps macOS and
// Windows out of this change.
//
// The address is the per-pid marker namespace `instance.rs` already owns, in exactly the
// shape [`crate::preview_ipc`] established for reaching a live preview host:
//
// ```text
// {runtime_dir}/cosmic-capture-kit.<pid>.recording        <- the marker (discovery)
// {runtime_dir}/cosmic-capture-kit.<pid>.recording.sock   <- this socket (transport)
// ```
//
// ## Fire and forget, deliberately
//
// There is no reply word. A [`Command`] is the whole wire in this direction, exactly as
// when a resident sends one, and the resident's own send is `let _ = write_all(..)` too.
// The CLI's honest report is therefore "could I reach a live recording at all" (the
// connect), never "did the recording agree", and that is the same guarantee a menu click
// has had since DRAGON-170. Adding an application ACK would mean the `preview_ipc`
// machinery (an `Ack` owned by the update loop), which exists there because a LOST
// handoff loses the user's capture. A lost toggle costs one more keypress.
//
// Unix only: Windows has no unix sockets, and needs none here: its tray daemon binds
// process-wide hotkeys and its recordings are reachable from the tray without focus.

/// How long an accepted connection has to finish its (single, tiny) command line before
/// it is dropped. DRAGON-118's rule applied to this thread: a peer that connects and then
/// says nothing must never pin the accept loop and starve a real command.
#[cfg(target_os = "linux")]
const COMMAND_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a sender will spend connecting to / writing at a recording. Bounded for the
/// same reason: a wedged recorder must cost the CLI a moment, never a hang.
#[cfg(target_os = "linux")]
pub const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// The most of a command line worth reading. A real one is under 8 bytes; this is
/// generous headroom that still stops a garbage sender making us allocate without bound.
#[cfg(target_os = "linux")]
const MAX_COMMAND_LINE: u64 = 4096;

/// A bound recording-control listener plus the channel its accept thread feeds.
///
/// Held by the `App` for the life of a recording. `Drop` unlinks the socket, so a process
/// that has stopped recording stops being reachable at once (and
/// `instance::sweep_stale_markers` clears the file left by one that was killed outright).
/// The accept thread is detached and dies with the process, like every other listener here.
#[cfg(target_os = "linux")]
pub struct ControlInlet {
    path: String,
    /// Commands received from other processes, oldest first. Drained by the app's tray
    /// poll, alongside the tray's own events.
    commands: std::sync::mpsc::Receiver<Command>,
}

#[cfg(target_os = "linux")]
impl ControlInlet {
    /// Take every command that has arrived since the last call. Non-blocking.
    pub fn drain(&self) -> Vec<Command> {
        self.commands.try_iter().collect()
    }

    /// The socket path this inlet is listening on (tests + logs).
    #[cfg(test)]
    pub fn socket_path(&self) -> &str {
        &self.path
    }
}

#[cfg(target_os = "linux")]
impl Drop for ControlInlet {
    fn drop(&mut self) {
        // Best-effort: a failed unlink only leaves a stale socket, which the next launch's
        // `instance::sweep_stale_markers` clears.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Begin accepting recording-control commands for THIS process: bind
/// `{runtime}/cosmic-capture-kit.<pid>.recording.sock` and start the accept thread.
///
/// A leftover socket at our OWN pid's path (a previous process with the same pid, killed
/// outright) is unlinked first: binding over it is the only way to reclaim the name, and
/// no live process can be holding our own pid's path.
///
/// Bind BEFORE `instance::set_recording_marker(true)` where possible, so a marker a
/// sender discovers always implies a socket that already exists. A bind failure is not an
/// error the user needs: the recording is unaffected, only the CLI commands cannot reach
/// it, so the caller logs and carries on.
#[cfg(target_os = "linux")]
pub fn start_control_inlet() -> std::io::Result<ControlInlet> {
    use std::os::unix::net::UnixListener;

    let path = crate::instance::recording_socket_path(std::process::id());
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    crate::preview_ipc::ensure_cloexec(&listener)?;

    let (tx, commands) = std::sync::mpsc::channel::<Command>();
    let spawned = std::thread::Builder::new()
        .name("cck-recording-control-ipc".into())
        .spawn(move || control_accept_loop(&listener, &tx));
    if let Err(e) = spawned {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(ControlInlet { path, commands })
}

/// The blocking accept loop, factored out so a test can drive it directly. Reads ONE
/// command line per connection and posts it; returns when the listener dies or the
/// receiver is dropped (the recording ended).
///
/// Every error is per-connection: a peer that speaks garbage, stalls or vanishes gets its
/// connection dropped and the loop keeps serving the next one.
#[cfg(target_os = "linux")]
fn control_accept_loop(
    listener: &std::os::unix::net::UnixListener,
    tx: &std::sync::mpsc::Sender<Command>,
) {
    use std::io::{BufRead as _, Read as _};
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if crate::preview_ipc::ensure_cloexec(&stream).is_err() {
            continue;
        }
        if stream.set_read_timeout(Some(COMMAND_READ_TIMEOUT)).is_err() {
            continue;
        }
        let mut line = String::new();
        let mut reader = std::io::BufReader::new(stream.take(MAX_COMMAND_LINE));
        if reader.read_line(&mut line).is_err() {
            continue;
        }
        let Some(cmd) = Command::parse(&line) else {
            // Not one of our words. Ignored rather than misapplied, exactly as
            // `RecordingState::parse` ignores a malformed state line.
            log::debug!("recording control: ignoring an unrecognised command line");
            continue;
        };
        log::info!("recording control: {} received", cmd.as_str());
        if tx.send(cmd).is_err() {
            // The recording ended and the App dropped its inlet: stop serving.
            return;
        }
    }
}

/// Send one [`Command`] to the live recording, if there is one. `Ok(pid)` names the
/// process it reached.
///
/// `Err` carries a sentence for a human at a terminal (and for the debug log). The two
/// cases the caller must be able to tell apart both live in the wording: nothing is
/// recording, versus a recording that could not be reached.
#[cfg(target_os = "linux")]
pub fn send_command(cmd: Command) -> Result<u32, String> {
    let hosts = crate::instance::live_recording_hosts();
    if hosts.is_empty() {
        return Err("no recording is in progress".to_string());
    }
    let mut last: Option<String> = None;
    for pid in hosts {
        let path = crate::instance::recording_socket_path(pid);
        // A marker with no socket is a recording that is not listening (an older build, or
        // a failed bind), so skip it without counting it as a refusal.
        if !std::path::Path::new(&path).exists() {
            continue;
        }
        match send_to_recording(&path, cmd) {
            Ok(()) => return Ok(pid),
            Err(why) => {
                log::info!("recording control: pid {pid} did not take {}: {why}", cmd.as_str());
                last = Some(why);
            }
        }
    }
    match last {
        Some(why) => Err(format!("the recording could not be reached ({why})")),
        None => Err("no recording is listening for commands".to_string()),
    }
}

/// One send against ONE socket path. Split out from [`send_command`] so the whole exchange
/// is testable against a socket in a temp dir, with no dependency on the real runtime dir.
#[cfg(target_os = "linux")]
fn send_to_recording(path: &str, cmd: Command) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;

    let mut conn = UnixStream::connect(path).map_err(|e| format!("connect: {e}"))?;
    crate::preview_ipc::ensure_cloexec(&conn).map_err(|e| format!("cloexec: {e}"))?;
    conn.set_write_timeout(Some(SEND_TIMEOUT)).map_err(|e| format!("timeout: {e}"))?;
    conn.write_all(cmd.encode().as_bytes()).map_err(|e| format!("write: {e}"))?;
    conn.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(())
}

/// Accumulate raw pipe bytes into complete newline-terminated lines. Pure; unit-tested
/// (DRAGON-538).
///
/// Both Windows ends of the recording pipe used `BufReader::lines()`, whose blocking read
/// PARKS in `ReadFile`, and a synchronous pipe serializes its I/O on the FILE OBJECT: a
/// parked read holds the object, and every later `WriteFile` on the same connection queues
/// behind it forever. That is how a stop's final `state 0 ...` line never left the child
/// (the tray stayed on the recording state), and how a daemon menu command could wedge the
/// daemon's own main thread. Both ends now PEEK for available bytes and only then read,
/// so no read ever parks; this buffer is the line assembly those bounded reads need,
/// portable and pure so the rule is tested on every host.
///
/// Bounded: a stream that never sends a newline cannot grow this without limit. Past
/// [`Self::MAX_PENDING`] the pending fragment is discarded (the wire only ever carries our
/// own short lines, so an overlong fragment is garbage, and dropping it mirrors how
/// `RecordingState::parse` ignores a malformed line rather than misapplying it).
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Default)]
pub struct LineBuf {
    pending: Vec<u8>,
}

// The struct-level `allow(dead_code)` above doesn't reach these: rustc checks an inherent
// impl's associated items independently of the type they're on, so the same gate has to be
// repeated here or `MAX_PENDING`/`push` warn on their own on non-Windows builds.
#[cfg_attr(not(windows), allow(dead_code))]
impl LineBuf {
    /// The most of an unterminated fragment worth keeping. A real wire line is under 32
    /// bytes; this is generous headroom, not a protocol limit.
    pub const MAX_PENDING: usize = 4096;

    /// Feed freshly-read bytes; get back every line they complete, oldest first, with the
    /// terminator (and a `\r`, should one ever appear) stripped.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=pos).collect();
            out.push(String::from_utf8_lossy(&line[..pos]).trim_end_matches('\r').to_string());
        }
        if self.pending.len() > Self::MAX_PENDING {
            self.pending.clear();
        }
        out
    }
}

/// DRAGON-583: the recording child's own control inlet, over REAL unix sockets in a temp
/// dir (no compositor, no D-Bus, no GPU, no runtime-dir dependency).
#[cfg(all(test, target_os = "linux"))]
mod control_inlet_tests {
    use super::*;
    use std::time::Duration;

    fn temp_socket(tag: &str) -> String {
        let dir = std::env::temp_dir()
            .join(format!("cck-recording-ipc-{}-{tag}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir.join("control.sock").to_string_lossy().into_owned()
    }

    fn serve(path: &str) -> std::sync::mpsc::Receiver<Command> {
        use std::os::unix::net::UnixListener;
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path).expect("bind");
        let (tx, rx) = std::sync::mpsc::channel::<Command>();
        // Detached, like the real accept thread: it dies with the process.
        std::thread::spawn(move || control_accept_loop(&listener, &tx));
        rx
    }

    /// Every verb the relay carries survives the wire, one connection per command, in
    /// order. This is the whole contract a CLI command depends on.
    #[test]
    fn every_command_arrives_in_order() {
        let path = temp_socket("commands");
        let rx = serve(&path);
        let sent = [
            Command::ToggleMic,
            Command::ToggleSystemAudio,
            Command::TogglePause,
            Command::Cancel,
            Command::Stop,
        ];
        for cmd in sent {
            send_to_recording(&path, cmd).expect("a live listener takes it");
            assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(cmd), "{cmd:?}");
        }
        let _ = std::fs::remove_file(&path);
    }

    /// Garbage is ignored rather than misapplied, and the loop KEEPS SERVING: the next real
    /// command still gets through. A stray connection must never cost a recording a toggle.
    #[test]
    fn garbage_is_ignored_and_the_loop_keeps_serving() {
        use std::io::Write as _;
        use std::os::unix::net::UnixStream;

        let path = temp_socket("garbage");
        let rx = serve(&path);
        for junk in ["hello there\n", "state 1 0 1 1\n", "\n", "stop stop\n"] {
            let mut conn = UnixStream::connect(&path).expect("connect");
            conn.write_all(junk.as_bytes()).unwrap();
            conn.flush().unwrap();
            drop(conn);
        }
        send_to_recording(&path, Command::Stop).expect("the loop still serves");
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(Command::Stop));
        // …and nothing else arrived: the junk was dropped, not half-read into a command.
        assert!(rx.try_recv().is_err(), "garbage must produce no command at all");
        let _ = std::fs::remove_file(&path);
    }

    /// No socket at the path (no recording, or one that never bound): the send fails fast
    /// and says so, which is what turns into the CLI's "no recording is in progress".
    #[test]
    fn a_missing_socket_fails_fast() {
        let path = temp_socket("missing");
        let _ = std::fs::remove_file(&path);
        let err = send_to_recording(&path, Command::Stop).expect_err("nothing is listening");
        assert!(err.starts_with("connect:"), "{err}");
    }

    /// The real bind: a per-pid socket beside this pid's recording marker, an end-to-end
    /// command over it, and the socket gone the moment the inlet drops (so a command sent
    /// after the recording ends reports no recording instead of vanishing).
    #[test]
    fn the_inlet_binds_a_per_pid_socket_and_unlinks_on_drop() {
        let inlet = match start_control_inlet() {
            Ok(i) => i,
            // A runtime dir we cannot bind in (an exotic sandbox) is a legitimate "no
            // inlet"; don't fail the suite for it.
            Err(e) => {
                eprintln!("skipping: cannot bind in the runtime dir ({e})");
                return;
            }
        };
        let path = inlet.socket_path().to_string();
        assert!(
            path.ends_with(&format!("cosmic-capture-kit.{}.recording.sock", std::process::id())),
            "{path}"
        );
        assert!(std::path::Path::new(&path).exists(), "the socket must exist while recording");

        send_to_recording(&path, Command::ToggleMic).expect("our own inlet takes it");
        // The app drains on its tray tick; poll the same way, bounded.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        while got.is_empty() && std::time::Instant::now() < deadline {
            got = inlet.drain();
            if got.is_empty() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        assert_eq!(got, vec![Command::ToggleMic]);
        assert!(inlet.drain().is_empty(), "a drain takes each command exactly once");

        drop(inlet);
        assert!(
            !std::path::Path::new(&path).exists(),
            "dropping the inlet must unlink its socket"
        );
    }
}

#[cfg(test)]
mod line_buf_tests {
    use super::*;

    /// The shapes a pipe read actually delivers: whole lines, several at once, and split
    /// fragments. Every complete line comes out exactly once, oldest first, terminator
    /// stripped.
    #[test]
    fn fragments_reassemble_and_batches_split() {
        let mut b = LineBuf::default();
        assert_eq!(b.push(b"state 1 0 1 1\n"), vec!["state 1 0 1 1"]);
        assert_eq!(b.push(b"state 0"), Vec::<String>::new());
        assert_eq!(b.push(b" 0 1 1\n"), vec!["state 0 0 1 1"]);
        assert_eq!(b.push(b"pause\nstop\n"), vec!["pause", "stop"]);
        // A trailing fragment waits for its terminator without disturbing later lines.
        assert_eq!(b.push(b"can"), Vec::<String>::new());
        assert_eq!(b.push(b"cel\nmic"), vec!["cancel"]);
        assert_eq!(b.push(b"\n"), vec!["mic"]);
    }

    /// A `\r` is stripped like the `\n`, so a future writer that emits CRLF cannot make
    /// `Command::parse` miss.
    #[test]
    fn a_carriage_return_does_not_reach_the_parser() {
        let mut b = LineBuf::default();
        assert_eq!(b.push(b"stop\r\n"), vec!["stop"]);
    }

    /// The bound: an unterminated stream is discarded past MAX_PENDING rather than growing
    /// forever, and the buffer keeps working afterwards.
    #[test]
    fn garbage_without_a_newline_is_bounded() {
        let mut b = LineBuf::default();
        let junk = vec![b'x'; LineBuf::MAX_PENDING + 1];
        assert_eq!(b.push(&junk), Vec::<String>::new());
        // The over-limit fragment was dropped; a fresh real line still comes through.
        assert_eq!(b.push(b"stop\n"), vec!["stop"]);
    }
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
        assert!(!idle.recording, "idle renders the idle menu (tray_menu(false, ..))");
        assert_eq!(icon_state(&idle), IconState::Idle);

        // Any recording state (incl. paused) keeps `recording` true, which is what makes
        // every renderer show the in-recording controls; the icon distinguishes live
        // recording from paused.
        let rec = RecordingState { recording: true, paused: false, mic: true, system: false };
        assert_eq!(icon_state(&rec), IconState::Recording);
        let paused = RecordingState { paused: true, ..rec };
        assert!(paused.recording, "a paused recording is still in progress");
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
