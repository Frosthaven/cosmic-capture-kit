//! Pause OTHER media players (Spotify, browsers, …) for as long as the preview overlay with a
//! soundtrack is open, then resume them — via the MPRIS D-Bus standard.
//!
//! All D-Bus work runs in a RE-EXEC'd "babysitter" child ([`run_duck`], wired into `main` behind
//! [`DUCK_FLAG`]) — NEVER from the GUI's own threads, where a blocking `zbus` call can stall
//! waiting for a reply the app's executor never pumps (that froze the preview while music played).
//! The child pauses every Playing player, then blocks reading its stdin; the GUI holds the write
//! end. When the preview closes (the guard drops) — OR the GUI exits for any reason, even a crash
//! (the OS closes the pipe) — the child reads EOF and resumes exactly what it paused. So nothing
//! is left paused, with no race and without ever blocking the GUI. Portable (just D-Bus + our own
//! binary), and it TRULY pauses the player (resuming where it left off), unlike muting its stream.
//!
//! macOS (DRAGON-171) reuses the SAME `--duck` babysitter + pipe-EOF contract; its backend lives in
//! [`duck_mac`] and pauses/resumes the system Now Playing app via a MediaRemote proxy loaded under
//! `/usr/bin/perl`. Same safety semantics: pause only what's playing, resume only that, crash-safe.
//!
//! Windows (DRAGON-283) reuses the SAME babysitter + pipe-EOF contract too; its backend lives in
//! [`media_control`] and pauses/resumes every Playing GSMTC (Global System Media Transport Controls)
//! session via `TryPauseAsync`/`TryPlayAsync`. Same safety semantics: pause only what's playing,
//! resume only those still paused, crash-safe.

#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use zbus::blocking::{Connection, Proxy};

#[cfg(target_os = "macos")]
// Physically under `platform/mac/` (the closed-platform split, DRAGON-226);
// module path stable via `#[path]`.
#[path = "../platform/mac/services/duck_mac/mod.rs"]
mod duck_mac;

#[cfg(windows)]
// Physically under `platform/windows/` (the closed-platform split, DRAGON-226);
// module path stable via `#[path]`, mirroring `duck_mac`.
#[path = "../platform/windows/media_control.rs"]
mod media_control;

pub(crate) const DUCK_FLAG: &str = "--duck";

#[cfg(target_os = "linux")]
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
#[cfg(target_os = "linux")]
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
#[cfg(target_os = "linux")]
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

/// RAII guard the preview holds while a soundtrack is on screen. [`engage`](Self::engage) just
/// spawns the babysitter child (fast, non-blocking — no D-Bus on our threads); the child does the
/// pausing and resumes when this guard drops (dropping closes the child's stdin → it reads EOF).
/// The process exiting would close that pipe too, so other media never stays paused. No-ops
/// gracefully if the child can't be spawned.
pub(crate) struct OtherAudioDuck {
    child: Option<std::process::Child>,
}

/// macOS (DRAGON-171): spawn the same `--duck` babysitter; its child pauses the
/// system Now Playing app via the MediaRemote proxy and resumes on our EOF. The
/// crash-safety is the identical pipe-EOF contract as Linux.
#[cfg(target_os = "macos")]
impl OtherAudioDuck {
    pub(crate) fn engage() -> Self {
        Self { child: duck_mac::spawn_babysitter() }
    }
}

/// Windows (DRAGON-283): spawn the same `--duck` babysitter; its child pauses every
/// Playing GSMTC session and resumes on our EOF. The crash-safety is the identical
/// pipe-EOF contract as Linux/macOS.
#[cfg(windows)]
impl OtherAudioDuck {
    pub(crate) fn engage() -> Self {
        Self { child: media_control::spawn_babysitter() }
    }
}

/// Other platforms: no public API to pause arbitrary apps, so media ducking is
/// capability-off. `engage` is an inert guard.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
impl OtherAudioDuck {
    pub(crate) fn engage() -> Self {
        Self { child: None }
    }
}

#[cfg(target_os = "linux")]
impl OtherAudioDuck {
    /// Spawn the babysitter. It pauses the other players and holds them until we let go.
    pub(crate) fn engage() -> Self {
        let child = std::env::current_exe().ok().and_then(|exe| {
            Command::new(exe)
                .arg(DUCK_FLAG)
                // Our keepalive: the child blocks on this pipe and resumes when we close it.
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok()
        });
        Self { child }
    }
}

impl Drop for OtherAudioDuck {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Close our end of its stdin → the child reads EOF → it resumes what it paused.
            drop(child.stdin.take());
            // It exits on its own right after; reap if already gone, but never block on it.
            let _ = child.try_wait();
        }
    }
}

// ── re-exec entry point: runs in a clean single-threaded child, where zbus::blocking is safe ──

/// macOS (DRAGON-171): the `--duck` child pauses the system Now Playing app via
/// the MediaRemote proxy, blocks on stdin, and resumes it on EOF. See [`duck_mac`].
#[cfg(target_os = "macos")]
pub(crate) fn run_duck() {
    duck_mac::run_duck();
}

/// Windows (DRAGON-283): the `--duck` child pauses every Playing GSMTC session, blocks
/// on stdin, and resumes those still paused on EOF. See [`media_control`].
#[cfg(windows)]
pub(crate) fn run_duck() {
    media_control::run_duck();
}

/// Other platforms: no media babysitter — nothing to pause. The `--duck` re-exec
/// is never spawned here, but the entry point must exist.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) fn run_duck() {}

/// `--duck`: pause every MPRIS player that's currently Playing, then wait for our parent to let go
/// (stdin EOF — an explicit release or the parent exiting) and resume exactly those players.
#[cfg(target_os = "linux")]
pub(crate) fn run_duck() {
    let Ok(conn) = Connection::session() else { return };
    let paused = pause_playing(&conn);
    if paused.is_empty() {
        return; // nothing held → no reason to linger
    }
    // Block until the parent closes our stdin (releases us) or dies (the OS closes the pipe).
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 64];
    while let Ok(n) = stdin.read(&mut buf) {
        if n == 0 {
            break; // EOF
        }
    }
    for name in &paused {
        if let Ok(p) = Proxy::new(&conn, name.as_str(), MPRIS_PATH, PLAYER_IFACE) {
            let _ = p.call_method("Play", &());
        }
    }
}

/// Pause every Playing MPRIS player; return the bus names we actually paused.
#[cfg(target_os = "linux")]
fn pause_playing(conn: &Connection) -> Vec<String> {
    let mut paused = Vec::new();
    for name in mpris_players(conn) {
        let Ok(p) = Proxy::new(conn, name.as_str(), MPRIS_PATH, PLAYER_IFACE) else { continue };
        let playing =
            p.get_property::<String>("PlaybackStatus").map(|s| s == "Playing").unwrap_or(false);
        let did_pause = playing && p.call_method("Pause", &()).is_ok();
        drop(p); // ends the borrow of `name` so we can keep it
        if did_pause {
            paused.push(name);
        }
    }
    paused
}

/// Session-bus names that are MPRIS media players.
#[cfg(target_os = "linux")]
fn mpris_players(conn: &Connection) -> Vec<String> {
    let Ok(reply) = conn.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus"),
        "ListNames",
        &(),
    ) else {
        return Vec::new();
    };
    reply
        .body()
        .deserialize::<Vec<String>>()
        .unwrap_or_default()
        .into_iter()
        .filter(|n| n.starts_with(MPRIS_PREFIX))
        .collect()
}
