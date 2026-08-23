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
//! TWO triggers hold this guard, each with its own setting and its own guard value. The preview
//! editor's is above. The second (DRAGON-694) holds it while a LIVE-ANNOTATION tool is armed
//! during a recording; the decision of when that one engages and releases is pure and lives in
//! `crate::app::ink_duck`, and nothing in this module changed for it. Both are safe to overlap:
//! a babysitter pauses only what it found PLAYING, so a second one started while the first holds
//! finds nothing to pause and holds nothing.
//!
//! **A D-Bus return code is not an outcome** (DRAGON-741). `Pause` and `Play` are ordinary
//! method calls: `Ok` means the message reached the player and was answered, and says nothing
//! about what the player then did. Both are verified against the player's own
//! `PlaybackStatus` now, bounded, and BOTH sides escalate once through `PlayPause`: the
//! resume side from a state measured as not playing, the pause side from a state measured as
//! Playing. Believing the return codes let this child log `resumed 1 player(s)` over a browser
//! that stayed silent for nine minutes, and the report arrived one preview later as "pause
//! other media isn't triggering", because by then the player really was not playing and the
//! detection line was honest. The full measurement is on [`run_duck`].
//!
//! The pause-side escalation is part 2 of that ticket and it landed last, after both other
//! backends. The report it answers is the owner's "its just the following attempt didnt
//! pause", and what made it actionable was the same shape turning up on Windows with a log
//! line under it (DRAGON-747, below). Neither platform's evidence proves the other's
//! mechanism, but the fix is the same one either way and it can only ever fire at a player
//! we have just watched keep playing.
//!
//! Windows (DRAGON-283) reuses the SAME babysitter + pipe-EOF contract too; its backend lives in
//! [`media_control`] and pauses/resumes every Playing GSMTC (Global System Media Transport Controls)
//! session via `TryPauseAsync`/`TryPlayAsync`. Same safety semantics: pause only what's playing,
//! resume only those still paused, crash-safe.
//!
//! **A GSMTC return code is not an outcome either, and this one is MEASURED** (DRAGON-747).
//! `TryPauseAsync` answering `Ok(true)` means the command was accepted, not that the app
//! obeyed it, which is the same shape of claim the D-Bus `Ok` above turned out not to
//! support. The ticket asked whether GSMTC's `Ok(true)` might be the stronger guarantee, and
//! said only a Windows box could settle it. It is settled: **it lies**. Owner QA on
//! 2026-08-21, with a browser playing YouTube as the only session, logged `paused 1 other
//! player(s) (found=1 playing=1 paused=1 unreadable=0)` while the audio KEPT PLAYING, and the
//! release then logged `resumed 0 player(s)`, because by then the honest status read said
//! Playing and there was nothing owed. An earlier duck the same morning worked end to end, so
//! it is per-session and not per-machine, the same "the first one works, the next one does
//! not" shape as the Linux defect.
//!
//! So the Windows backend verifies BOTH calls against the session's own `PlaybackStatus`,
//! over the SAME budgets and the SAME pure decisions this file already owns, and BOTH sides
//! escalate once through `TryTogglePlayPauseAsync`: the resume side from a state measured as
//! not playing ([`escalate_to_play_pause`]), the pause side from a state measured as Playing
//! ([`escalate_pause_to_play_pause`]). Each gate only ever fires the toggle from the one
//! state where a toggle does the thing being asked for.
//!
//! **macOS closed the same gap next** (DRAGON-752), so all three backends now count what the
//! player was OBSERVED to do and not one of them counts a call that returned successfully. Its
//! read is the odd one out, and the difference is the interesting part: MediaRemote answers
//! about whatever owns the system Now Playing slot, never about a player you name, and every
//! read costs a `/usr/bin/perl` launch rather than a property fetch. So the mac backend scopes
//! each read to the PID it paused ([`PlayerStatus::from_now_playing`]), and a snapshot about
//! anybody else is `Other`, which every decision below refuses to act on. Both escalations are
//! the shared gates, sent as MediaRemote's play/pause toggle. See [`duck_mac`].

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

/// The whole MPRIS name family, as a session-bus policy pattern: "any media player".
/// Players publish under [`MPRIS_PREFIX`] with their own suffix, so no single name can be
/// asked about, and reaching ONE of them is enough for this feature to do its job.
#[cfg(target_os = "linux")]
const MPRIS_FAMILY: &str = "org.mpris.MediaPlayer2.*";

/// Can this process pause other media players at all (DRAGON-555)?
///
/// Linux answers from the session-bus policy, which is a capability and not a sandbox test:
/// true on every ordinary session, and false only where the bus refuses us the MPRIS family.
///
/// That refusal is INVISIBLE from inside the calls this module makes, which is why the
/// question has to be asked separately. A filtered `ListNames` omits the players instead of
/// failing, so [`mpris_players`] comes back empty and every layer above reads that as "nothing
/// was playing". The feature was dead in the `lab/flatpak` build for exactly that reason and
/// reported success the whole time: the settings toggle sat there switched on, the babysitter
/// child spawned, found nobody, and exited immediately.
///
/// The answer heals itself if the sandbox is ever granted `--talk-name=org.mpris.MediaPlayer2.*`;
/// nothing here would need changing.
#[cfg(target_os = "linux")]
pub(crate) fn media_pause_available() -> bool {
    crate::util::bus_name_reachable(MPRIS_FAMILY)
}

/// macOS and Windows reach the system's Now Playing app through a native API
/// ([`duck_mac`] / [`media_control`]), not a bus name anyone can filter, so the capability is
/// always there.
#[cfg(any(target_os = "macos", windows))]
pub(crate) fn media_pause_available() -> bool {
    true
}

/// Other platforms: no public API to pause arbitrary apps, matching the inert
/// [`OtherAudioDuck::engage`] below.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) fn media_pause_available() -> bool {
    false
}

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
        // No route to any player (DRAGON-555): spawning the babysitter would cost a process
        // to discover that, every time a preview opens, and it would look like the feature
        // ran. Say so once, in the log, and hand back the inert guard. The settings row is
        // gated on the same answer, so nothing offered the user this in the first place.
        if !media_pause_available() {
            log::info!(
                "pause other media: the session bus will not let this process reach any \
                 org.mpris.MediaPlayer2 name, so other players are left alone"
            );
            return Self { child: None };
        }
        // `self_exe` (DRAGON-510): the babysitter is held for the whole recording and the
        // AppImage mount path is not a durable way to name ourselves.
        //
        // Both failure arms are LOUD (DRAGON-733). They used to be a silent `.ok()` pair, so
        // "the feature did nothing" and "we could not even start the process that does it"
        // looked identical from the log, and the second is a real thing to know: the AppImage
        // mount can be gone, and a fork can fail under fd pressure.
        let child = match crate::util::self_exe() {
            Ok(exe) => match Command::new(exe)
                .arg(DUCK_FLAG)
                // Our keepalive: the child blocks on this pipe and resumes when we close it.
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(c) => Some(c),
                Err(e) => {
                    log::warn!(
                        "pause other media: the babysitter child would not start ({e}), so \
                         other players are left alone"
                    );
                    None
                }
            },
            Err(e) => {
                log::warn!(
                    "pause other media: this process cannot name its own executable ({e}), so \
                     the babysitter cannot be launched and other players are left alone"
                );
                None
            }
        };
        Self { child }
    }
}

impl Drop for OtherAudioDuck {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // DRAGON-733: the release is worth a line of its own. The child's own "resumed"
            // line is written by a DIFFERENT process, so without this one a reader cannot
            // tell "the guard was still held when the session ended" from "the guard was
            // dropped early and the child had already gone".
            log::debug!("pause other media: releasing the babysitter (it resumes what it paused)");
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

/// What one `--duck` child found on the bus and what it managed to do about it.
///
/// Counts ONLY. A bus name says which app the user has open and a track title says what they
/// were watching, and neither belongs in a log that ships to us; how many players there were
/// and what happened to each is behaviour, which is exactly what a support reader needs
/// (DRAGON-733, and the privacy rule in CLAUDE.md).
///
/// `paused` counts players we WATCHED STOP since DRAGON-741, not players whose `Pause` call
/// returned successfully. The two are different and the difference cost a user their music.
///
/// Compiled on every host so [`PauseTally::summary`] is proven by the suite wherever it runs.
/// All three backends fill one in (`run_duck` here, `duck_mac`, `media_control`), so it is
/// dead only on a target with no media backend at all.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos", windows)), allow(dead_code))]
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PauseTally {
    /// MPRIS names owned on the session bus when we looked.
    pub found: usize,
    /// Of those, the ones that answered "Playing".
    pub playing: usize,
    /// Of those, the ones we then OBSERVED leave the Playing state (DRAGON-741 on Linux,
    /// DRAGON-747 on Windows, DRAGON-752 on macOS). Every backend measures this now; not one
    /// of them counts a call that merely returned successfully.
    pub paused: usize,
    /// Players whose `PlaybackStatus` could not be read at all (a proxy or a property error).
    pub unreadable: usize,
}

#[cfg_attr(not(any(target_os = "linux", target_os = "macos", windows)), allow(dead_code))]
impl PauseTally {
    /// **Pure**, unit-tested: the one log sentence for what this child found.
    ///
    /// It names the OUTCOME first, because the reader arriving at this line is asking "did
    /// the feature do anything", and every count after it explains the answer. The three
    /// no-op shapes are spelled apart on purpose: "no player is running", "a player is
    /// running but nothing is playing" and "a player is playing and refused us" are three
    /// different bug reports and they used to produce the same silence.
    pub fn summary(&self) -> String {
        let head = if self.paused > 0 {
            format!("paused {} other player(s)", self.paused)
        } else if self.found == 0 {
            "no media player is on the session bus, so nothing was paused".to_string()
        } else if self.playing == 0 {
            "no player was playing, so nothing was paused".to_string()
        } else {
            "every playing player refused to pause".to_string()
        };
        format!(
            "pause other media: {head} (found={} playing={} paused={} unreadable={})",
            self.found, self.playing, self.paused, self.unreadable
        )
    }
}

/// What a player's `PlaybackStatus` property says, reduced to the states a decision here
/// turns on.
///
/// `Unreadable` is deliberately NOT folded into `Stopped`. "The player says it is not
/// playing" and "we could not ask the player" lead to opposite actions on the resume path:
/// the escalation below is a TOGGLE, so sending it to something that might be playing would
/// silence the very media we are trying to restore.
///
/// Compiled on every host so the decisions built on it are proven by the suite wherever it
/// runs. All three backends read a real status into it, from three different systems (MPRIS
/// `PlaybackStatus` strings, GSMTC `PlaybackStatus` integers, MediaRemote's Now Playing
/// snapshot), so the decisions below are written ONCE and cannot drift between them.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos", windows)), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlayerStatus {
    Playing,
    Paused,
    Stopped,
    /// A string the MPRIS spec does not define. Never acted on.
    Other,
    /// The property could not be read at all.
    Unreadable,
}

impl PlayerStatus {
    /// **Pure**, unit-tested: the `PlaybackStatus` property string, as a state.
    ///
    /// The spec defines exactly three values and they are case-sensitive. Anything else is
    /// `Other` rather than a guess, because every caller here either acts on a definite
    /// state or does nothing.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn from_mpris(s: &str) -> Self {
        match s {
            "Playing" => Self::Playing,
            "Paused" => Self::Paused,
            "Stopped" => Self::Stopped,
            _ => Self::Other,
        }
    }

    /// **Pure**, unit-tested: GSMTC's `PlaybackStatus` integer, as the same state (DRAGON-747).
    ///
    /// Takes the raw `i32` rather than the WinRT newtype so the whole table is provable on
    /// every host, the way the Windows build-number gates in `platform::mod` are. That matters
    /// more here than usual: Windows QA is one box, and the ARM64 build has no QA hardware at
    /// all, so a mapping only Windows could test would be a mapping nothing tests.
    ///
    /// `Windows.Media.Control` defines six values. Three of them name a definite playback
    /// state and map straight across. The other three do NOT, and they map to `Other`, which
    /// is the state every decision below refuses to act on:
    ///
    /// - `Closed` (0) is a session with no media left to command.
    /// - `Opened` (1) is a session that has media but has never started.
    /// - `Changing` (2) is mid-transition, so it is a state that is about to be wrong.
    ///
    /// Folding any of them into `Stopped` would be the tempting reading and it is the wrong
    /// one, because `Stopped` is a state [`escalate_to_play_pause`] fires a TOGGLE from. A
    /// toggle aimed at a session that is really about to start playing pauses it instead, and
    /// the whole point of this pass is to stop guessing at the user's media.
    ///
    /// An undefined integer is `Other` for the same reason `from_mpris` refuses an unknown
    /// string: a value we do not recognise is not evidence of anything.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn from_gsmtc(raw: i32) -> Self {
        match raw {
            4 => Self::Playing,
            5 => Self::Paused,
            3 => Self::Stopped,
            _ => Self::Other,
        }
    }

    /// **Pure**, unit-tested: macOS's Now Playing snapshot, as the state of ONE NAMED PLAYER
    /// (DRAGON-752).
    ///
    /// The other two readers answer about the player they were asked about. MediaRemote does
    /// not: it answers about whatever owns the system Now Playing slot at that instant, and
    /// that owner can change while we hold a pause. So this takes the PID we are asking ABOUT
    /// (`subject_pid`, the app we paused) beside the PID the snapshot is about (`owner_pid`),
    /// and a snapshot about anybody else is not evidence about us:
    ///
    /// - The same owner maps `playing` straight across, to `Playing` or `Paused`.
    /// - A different owner, no owner at all (`owner_pid` 0), or no subject to ask about is
    ///   `Other`, the state every decision below refuses to act on.
    ///
    /// **The scoping is a safety property, not tidiness.** Both escalations send a play/pause
    /// TOGGLE. Read unscoped, a snapshot saying "something is playing" moments after our pause
    /// would look exactly like a player ignoring us, and the toggle would then go to whoever
    /// holds the slot NOW: a player we never touched, that we will never resume, silenced for
    /// a user who had just started it. `Other` closes that off, because
    /// [`escalate_pause_to_play_pause`] fires only from `Playing` and
    /// [`escalate_to_play_pause`] only from `Paused` or `Stopped`.
    ///
    /// **A PID must be positive to be one.** The proxy's contract is that 0 means "nobody owns
    /// Now Playing", and a negative value is not a process at all, so neither can name a
    /// player it is safe to command. Anything but a real PID on either side is `Other`.
    ///
    /// **`Stopped` is unreachable here, and nothing is lost by that.** MediaRemote's read is a
    /// bool, so paused and stopped are one state to it, and `Paused` is the honest name for
    /// what it can actually say. It costs nothing because no decision in this file separates
    /// the two, which `now_playing_status_tests` pins rather than leaving to memory.
    ///
    /// A read that failed outright is [`PlayerStatus::Unreadable`], decided by the caller, the
    /// same way the Windows backend maps a status it could not fetch.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn from_now_playing(subject_pid: i32, owner_pid: i32, playing: bool) -> Self {
        if subject_pid <= 0 || owner_pid != subject_pid {
            Self::Other
        } else if playing {
            Self::Playing
        } else {
            Self::Paused
        }
    }
}

/// **Pure**, unit-tested: did our `Pause` actually take?
///
/// DRAGON-741. A `Pause` call returning `Ok` means the message was DELIVERED and answered,
/// never that the player obeyed it, and this code believed the return code from the day the
/// feature shipped. A pause we only THINK we made must not be "resumed" later: sending `Play`
/// to a player the USER had paused starts their music for them, which is the opposite of the
/// feature, and the pre-fix binary was measured doing exactly that against a player that
/// ignored our `Pause`.
///
/// `Unreadable` counts as landed ON PURPOSE. We did call `Pause` and it answered, so the
/// player probably is stopped; forgetting it would leave the user's media dead with nothing
/// in the system that knows it was ours. The resume path re-reads the state anyway and only
/// acts on a definite one, so the cost of being wrong here is one redundant `Play`.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos", windows)), allow(dead_code))]
fn pause_landed(after: PlayerStatus) -> bool {
    !matches!(after, PlayerStatus::Playing)
}

/// **Pure**, unit-tested: has the player actually come back?
#[cfg_attr(not(any(target_os = "linux", target_os = "macos", windows)), allow(dead_code))]
fn resume_landed(after: PlayerStatus) -> bool {
    matches!(after, PlayerStatus::Playing)
}

/// **Pure**, unit-tested: may a resume that `Play` did not deliver escalate to `PlayPause`?
///
/// Only from a state we have just MEASURED as not playing. `PlayPause` is a toggle, so on a
/// player that is already playing it PAUSES, and on one whose state we could not read it is
/// a coin flip against the user's own music. Both are worse than leaving the player alone
/// and saying so in the log.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos", windows)), allow(dead_code))]
fn escalate_to_play_pause(after: PlayerStatus) -> bool {
    matches!(after, PlayerStatus::Paused | PlayerStatus::Stopped)
}

/// **Pure**, unit-tested: may a PAUSE that the player did not honour escalate to the
/// play/pause toggle? (DRAGON-747, the field evidence.)
///
/// Only from a state we have just MEASURED as Playing. That is the mirror of
/// [`escalate_to_play_pause`]'s gate and it is safe for the mirrored reason: the toggle does
/// the thing we want from THIS state and the wrong thing from any other. A player that really
/// did pause would be STARTED by a toggle, and a player we could not read is a coin flip, so
/// both must decline. Being wrong here plays music at somebody who asked for quiet.
///
/// This is the exact complement of [`pause_landed`] today, and it is still its own function
/// on purpose. They answer two different questions, "did our pause take" and "may we command
/// this player again", and folding them into one name would let a later change to the first
/// silently change when we reach for the user's media. One counter or one predicate answering
/// two questions is the defect class this project has now paid for four times.
///
/// All three backends now. Windows added it on measured field evidence, macOS took it next
/// (DRAGON-752) rather than wait to be lied to, since that backend is one player deep and a
/// refusal there is the whole feature failing, and Linux joined them in DRAGON-741 part 2:
/// the "the first duck works, the following attempt does not pause" report that opened this
/// work has the same shape on both platforms, so the resume-side escalation Linux shipped in
/// part 1 was only ever half the answer.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos", windows)), allow(dead_code))]
fn escalate_pause_to_play_pause(after: PlayerStatus) -> bool {
    matches!(after, PlayerStatus::Playing)
}

// The three settle budgets are SHARED by every backend that verifies its calls (DRAGON-747).
// They were Linux-only while Linux was the only one doing the verifying. Two backends reading
// two sets of numbers would mean "how long does a player get to obey" had two answers that
// nothing compared, and the values are not platform facts: they are how long a human waits
// before calling their music broken. The values are UNCHANGED from DRAGON-741, so Linux reads
// exactly what it read before.

/// How long a `Pause` gets to visibly take effect before we stop believing it landed.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos", windows)), allow(dead_code))]
const PAUSE_SETTLE_MS: u64 = 600;
/// How long a `Play` gets. Longer than the pause budget on purpose: stopping is immediate,
/// while starting can mean a browser bringing a throttled background tab back first.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos", windows)), allow(dead_code))]
const RESUME_SETTLE_MS: u64 = 1500;
/// Gap between `PlaybackStatus` re-reads inside one settle.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos", windows)), allow(dead_code))]
const SETTLE_POLL_MS: u64 = 50;

// Un-gated with the constants, so the relation is now checked when compiling for ANY target
// rather than only for Linux. Same assertions, more places they have to hold.
const _: () = assert!(
    SETTLE_POLL_MS < PAUSE_SETTLE_MS,
    "a settle budget inside one poll gap re-reads the player exactly once, so a pause that \
     is merely slow would read as ignored and the player would never be resumed (DRAGON-741)"
);
const _: () = assert!(
    PAUSE_SETTLE_MS < RESUME_SETTLE_MS,
    "coming back is slower than stopping; a resume budget at the pause budget declares a \
     working player stranded and escalates a toggle at it (DRAGON-741)"
);

/// The bounded settle loop with the status READ INJECTED, so it can be driven by a scripted
/// closure in the suite on any host (DRAGON-747, the repo's `foo_with` seam).
///
/// Re-read the player's state until `done` accepts it or `budget_ms` runs out, and return
/// whatever it last said. This is the same shape as the Linux [`settle_status`] below, which
/// is left exactly as DRAGON-741 shipped it: that one is bound to a `zbus` `Proxy`, and this
/// change was made on a Windows box where the Linux arm does not compile, so folding the two
/// together is a job for a Linux box and not for this ticket.
///
/// Three properties are load-bearing and each has a test:
///
/// - **The read happens BEFORE the deadline test**, so even a budget of zero asks the player
///   once. A settle that could return without ever looking would report every call as ignored.
/// - **It returns the moment `done` accepts**, so a healthy player costs one round trip and no
///   sleep at all. The full budget only elapses when the call genuinely did not take.
/// - **It is BOUNDED, always.** This runs in a detached child with no window and no user, so
///   an unbounded wait here is a process that lives until the session ends.
///
/// The delay a failing call costs is a deliberate trade, and the Linux twin's doc carries the
/// full reasoning: a missed pause self-corrects on the next cycle, while a falsely claimed
/// resume leaves the user's media dead and nothing ever corrects it.
///
/// **`poll_ms` is a floor and not a cadence, which matters on macOS** (DRAGON-752). The two
/// other backends read a property, so their loop runs at roughly the poll gap. The mac reader
/// LAUNCHES A PROCESS per read (`/usr/bin/perl`, see [`duck_mac`]), so its loop runs at the
/// read's own cost and the gap barely shows. That is fine and it is why the gap can stay small
/// for everyone: this loop is bounded by `budget_ms` PLUS one read either way, since the read
/// happens before the deadline test. The mac read carries its own 2s watchdog inside the
/// proxy, so the worst case there is one budget plus that watchdog, and it is still bounded.
#[cfg(any(target_os = "macos", windows, test))]
fn settle_status_with(
    budget_ms: u64,
    poll_ms: u64,
    mut read: impl FnMut() -> PlayerStatus,
    done: impl Fn(PlayerStatus) -> bool,
) -> PlayerStatus {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(budget_ms);
    loop {
        let now = read();
        if done(now) || std::time::Instant::now() >= deadline {
            return now;
        }
        std::thread::sleep(std::time::Duration::from_millis(poll_ms));
    }
}

/// `--duck`: pause every MPRIS player that's currently Playing, then wait for our parent to let go
/// (stdin EOF — an explicit release or the parent exiting) and resume exactly those players.
///
/// DRAGON-733 made this child SAY what it did. It used to log nothing at all between its process
/// banner and its exit, so "the setting is on, the child spawned, the music kept playing" was an
/// unanswerable report: every one of "no session bus", "no player on the bus", "a player that is
/// not Playing" and "a player that refused Pause" produced the same silent early return, and an
/// hour went into guessing which. Counts only, never a bus name and never a track title.
///
/// DRAGON-741 made it stop believing D-Bus return codes. `Pause` and `Play` are ordinary
/// method calls: `Ok` says the message reached the player and was answered, and says nothing
/// at all about what the player did next. On 2026-08-20 this child logged `resumed 1
/// player(s)` and the owner's YouTube stayed dead for nine minutes. The recording taken 27
/// seconds after that "successful" resume measured its own system track at -120.0 dBFS,
/// digital silence, where the three takes around the two resumes that DID work each measured
/// -18 dBFS. Forty-four seconds later the next preview looked at the same player, correctly
/// read it as not playing, and printed `no player was playing`, which is what the report
/// arrived as. So the visible symptom was a detection line and the defect was one cycle
/// earlier, on the resume. Both calls are now VERIFIED against the player's own state.
///
/// Part 2 of that ticket is [`pause_playing`]'s toggle escalation, and it answers the half
/// of the report the resume fix did not: "its just the following attempt didnt pause". The
/// incident's own two accounts were never reconciled and this does not settle them, so it is
/// not written up as the cause. What it does is remove the one shape both platforms have now
/// been seen to take, a player that answers the pause and keeps playing, from the list of
/// things that can happen without a line in this log.
///
/// Dead end, recorded so nobody re-runs it: keying "is this player really making sound" on
/// the audio server instead of on MPRIS. The JOIN is not the problem. `pactl list
/// sink-inputs` carries the owning pid and it lines up exactly with the bus name
/// (`GetConnectionUnixProcessID` on `org.mpris.MediaPlayer2.firefox.instance_…` returned the
/// same pid as that sink input's `application.process.id`). The FLAG is the problem, and it
/// is wrong in BOTH directions. Sampled once a second for 860 seconds against a browser the
/// owner was really using: 1 sample had the player `Paused` while its sink input was
/// uncorked, and 9 samples had it `Playing` with no sink input on the server at all. So an
/// uncorked stream does not mean sound and a missing stream does not mean silence. Reading
/// actual sample levels would answer it honestly and costs a capture stream per check, which
/// is far too much for a guard that runs on every preview.
#[cfg(target_os = "linux")]
pub(crate) fn run_duck() {
    let conn = match Connection::session() {
        Ok(c) => c,
        Err(e) => {
            log::warn!(
                "pause other media: this process cannot reach the session bus ({e}), so other \
                 players are left alone"
            );
            return;
        }
    };
    let (paused, tally) = pause_playing(&conn);
    log::debug!("{}", tally.summary());
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
    // Two counts, not one. A player that LEFT the bus while we held it (the user closed
    // Spotify, the browser tab went away) is nothing to put back, and counting it as a
    // failure would print an alarming line for the most ordinary thing a user can do. Only
    // a player that is still there and would not take `Play` is actually stranded, and that
    // is the one worth an error, because nothing else in the system will ever resume it.
    let still_here = mpris_players(&conn);
    let (mut owed, mut resumed, mut escalated) = (0usize, 0usize, 0usize);
    for name in &paused {
        if !still_here.contains(name) {
            continue;
        }
        let Ok(p) = player_proxy(&conn, name.as_str()) else {
            // On the bus but unreachable through the player interface. Nothing we can send
            // it, so counting it owed would print a shortfall we could never clear.
            continue;
        };
        // A player the USER resumed while we held it owes us nothing (the Windows backend
        // has always checked this; Linux did not, and would send it a redundant `Play`).
        if read_status(&p) == PlayerStatus::Playing {
            continue;
        }
        owed += 1;
        if p.call_method("Play", &()).is_err() {
            continue;
        }
        let mut after = settle_status(&p, RESUME_SETTLE_MS, resume_landed);
        // DRAGON-741: `Play` answered and the player stayed put. Escalate ONCE, and only
        // from a state just measured as not playing, because `PlayPause` is a toggle. Some
        // players wire the two verbs to different handlers, and a player that has dropped
        // its media session while we held it can answer `Play` without having anything to
        // start; the toggle is the one other thing worth trying before giving up loudly.
        if !resume_landed(after)
            && escalate_to_play_pause(after)
            && p.call_method("PlayPause", &()).is_ok()
        {
            escalated += 1;
            after = settle_status(&p, RESUME_SETTLE_MS, resume_landed);
        }
        if resume_landed(after) {
            resumed += 1;
        }
    }
    if resumed < owed {
        // This line used to be unreachable in practice: it compared return codes, and a
        // player that answers `Play` and does nothing answers it successfully. It now
        // compares MEASURED states, so it fires exactly when the user's media is stranded.
        log::error!(
            "pause other media: resumed {resumed} of the {owed} player(s) still owed a \
             resume ({escalated} of them retried through PlayPause); the rest are still \
             paused and nothing else will resume them"
        );
    } else {
        log::debug!(
            "pause other media: resumed {resumed} player(s) (of {} paused, {escalated} \
             through PlayPause; the rest left the bus or the user resumed them)",
            paused.len()
        );
    }
}

/// A player proxy that will actually ASK the player every time we read a property.
///
/// `Proxy::new` caches properties (`CacheProperties::Lazily`), so the first read is real and
/// every later one is served from a copy that only a `PropertiesChanged` signal refreshes.
/// That was invisible while this code read `PlaybackStatus` exactly once. The moment
/// DRAGON-741 started re-reading it to verify a `Pause`, the settle loop spent its whole
/// budget re-reading the value from BEFORE the pause and declared every player a refusal.
/// Caught by the fake-player harness on the first run, which is precisely the class of thing
/// that shipped silently the last time nobody could reproduce this feature.
#[cfg(target_os = "linux")]
fn player_proxy<'a>(conn: &Connection, name: &'a str) -> zbus::Result<Proxy<'a>> {
    zbus::blocking::proxy::Builder::<Proxy<'a>>::new(conn)
        .destination(name)?
        .path(MPRIS_PATH)?
        .interface(PLAYER_IFACE)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
}

/// This player's `PlaybackStatus`, or [`PlayerStatus::Unreadable`] if it cannot be read.
#[cfg(target_os = "linux")]
fn read_status(p: &Proxy<'_>) -> PlayerStatus {
    match p.get_property::<String>("PlaybackStatus") {
        Ok(s) => PlayerStatus::from_mpris(&s),
        Err(_) => PlayerStatus::Unreadable,
    }
}

/// Re-read `PlaybackStatus` until `done` accepts it or `budget_ms` runs out, and return
/// whatever it last said.
///
/// BOUNDED, always. This runs in a detached child with no window and no user, so an
/// unbounded wait here is a process that lives until the session ends. Each phase is bounded
/// by the players it touches times two of its budgets, since a player is settled at most
/// twice per phase: the `Pause` or `Play`, then the one `PlayPause` escalation.
///
/// **The delay this adds is a deliberate trade, and it goes this way round on purpose.** A
/// healthy player costs one extra round trip and no sleep at all, because the read happens
/// BEFORE the deadline test and returns the moment `done` accepts; the full budget only
/// elapses when the call genuinely did not take. But while a child sits in that failing case,
/// a second duck child starting up would read the same player as not-Playing and decline to
/// pause it, so a fast re-arm can miss one pause. That is accepted, because the two failures
/// are not the same size: **a missed pause self-corrects on the very next cycle and costs the
/// user a few seconds of ducking, while a falsely claimed resume leaves their media dead with
/// the log insisting everything is fine, and nothing in the system ever corrects it.** Trading
/// the recoverable failure for the unrecoverable one is the whole point of this function. Do
/// not "optimise" the budgets away to close the first gap; that reopens the second.
#[cfg(target_os = "linux")]
fn settle_status(p: &Proxy<'_>, budget_ms: u64, done: impl Fn(PlayerStatus) -> bool) -> PlayerStatus {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(budget_ms);
    loop {
        let now = read_status(p);
        if done(now) || std::time::Instant::now() >= deadline {
            return now;
        }
        std::thread::sleep(std::time::Duration::from_millis(SETTLE_POLL_MS));
    }
}

/// Pause every Playing MPRIS player; return the bus names we actually paused, and the
/// [`PauseTally`] describing what the bus looked like while we did it.
///
/// "Actually paused" means WATCHED TO STOP since DRAGON-741, not "answered our `Pause`".
/// A name only joins the returned list once the player's own `PlaybackStatus` has left
/// Playing, because that list is the exact set we will send `Play` to at release.
///
/// DRAGON-741 part 2 added the PAUSE-side toggle escalation, the mirror of the one the
/// resume path has carried since part 1. A player that answers `Pause` and keeps playing
/// gets one `PlayPause`, and only from a state just measured as Playing. The Windows twin
/// measured that shape in the field first (DRAGON-747) and macOS took it next
/// (DRAGON-752), so all three backends now reach for the toggle in the same one place.
#[cfg(target_os = "linux")]
fn pause_playing(conn: &Connection) -> (Vec<String>, PauseTally) {
    let mut paused = Vec::new();
    let mut tally = PauseTally::default();
    // Kept apart from the tally on purpose: `paused` is what the user's media did, and
    // these two are what WE had to do to get there. Folding them in would let a rescued
    // player read as an ordinary one, and the whole reason this line exists is that the
    // ordinary-looking sessions were the broken ones.
    let (mut escalated, mut rescued) = (0usize, 0usize);
    for name in mpris_players(conn) {
        tally.found += 1;
        let Ok(p) = player_proxy(conn, name.as_str()) else {
            tally.unreadable += 1;
            continue;
        };
        let status = read_status(&p);
        if status == PlayerStatus::Unreadable {
            tally.unreadable += 1;
        }
        if status != PlayerStatus::Playing {
            continue;
        }
        tally.playing += 1;
        // The command. Its return code is logged and NOT believed, which is the whole rule
        // this module is built on: `Ok` says the message reached the player and was
        // answered, never that the player obeyed. It used to gate the claim below, so a
        // player that errored here was skipped without anyone ever looking at it.
        if let Err(e) = p.call_method("Pause", &()) {
            log::debug!("pause other media: a player's Pause call failed ({e})");
        }
        // Watch the player's own status leave Playing before claiming it. This list is the
        // exact set we send `Play` to at release, so a player we only THINK we paused would
        // have its media started for a user who had stopped it themselves.
        let mut after = settle_status(&p, PAUSE_SETTLE_MS, pause_landed);
        // Still playing, so the command did nothing. Escalate ONCE through the toggle, and
        // only from a state just MEASURED as Playing, which is what makes a toggle safe:
        // from Playing it can only pause. A player that reads as stopped, or that we cannot
        // read at all, is left alone, because there the same toggle would START its media.
        if escalate_pause_to_play_pause(after) && p.call_method("PlayPause", &()).is_ok() {
            escalated += 1;
            after = settle_status(&p, PAUSE_SETTLE_MS, pause_landed);
            if pause_landed(after) {
                rescued += 1;
            }
        }
        let landed = pause_landed(after);
        drop(p); // ends the borrow of `name` so we can keep it
        // The tally stays honest either way. A player that never leaves Playing, even after
        // the toggle, is NOT counted as paused and never joins `paused`, so it is not owed a
        // resume at release and cannot produce a phantom shortfall there.
        if landed {
            tally.paused += 1;
            paused.push(name);
        }
    }
    if escalated > 0 {
        // Worth its own line: this is a player lying about a `Pause`, which is what part 2
        // of the ticket was looking for on this platform. Counts only, never which player.
        log::debug!(
            "pause other media: {escalated} player(s) answered the pause and kept playing, \
             so they were retried through PlayPause ({rescued} then stopped)"
        );
    }
    (paused, tally)
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

/// DRAGON-741: the three decisions that stopped this feature trusting a D-Bus return code.
/// Pure, so the whole truth table is provable on any host, including the two that never run
/// this backend.
#[cfg(test)]
mod player_status_tests {
    use super::{PlayerStatus, escalate_to_play_pause, pause_landed, resume_landed};

    /// MPRIS defines exactly three values and they are case-sensitive. Anything else must
    /// land in `Other` rather than being guessed at, because a guess here toggles a stranger's
    /// music.
    #[test]
    fn only_the_three_spec_values_parse() {
        assert_eq!(PlayerStatus::from_mpris("Playing"), PlayerStatus::Playing);
        assert_eq!(PlayerStatus::from_mpris("Paused"), PlayerStatus::Paused);
        assert_eq!(PlayerStatus::from_mpris("Stopped"), PlayerStatus::Stopped);
        for odd in ["playing", "PLAYING", "", "Buffering", "Playing "] {
            assert_eq!(PlayerStatus::from_mpris(odd), PlayerStatus::Other, "{odd:?}");
        }
    }

    /// The defect itself: a player still Playing after our `Pause` was never paused, however
    /// happily the call returned. Counting it would send it `Play` at release and start
    /// media the user had running all along.
    #[test]
    fn a_player_still_playing_was_not_paused() {
        assert!(!pause_landed(PlayerStatus::Playing));
    }

    /// Everything else counts, INCLUDING `Unreadable`. We did call `Pause` and it answered,
    /// so dropping the name would strand the user's media with nothing in the system that
    /// knows it was ours; the resume path re-reads before it acts, so the cost of being
    /// wrong is one redundant `Play`.
    #[test]
    fn every_other_state_counts_as_paused_including_unreadable() {
        for s in [
            PlayerStatus::Paused,
            PlayerStatus::Stopped,
            PlayerStatus::Other,
            PlayerStatus::Unreadable,
        ] {
            assert!(pause_landed(s), "{s:?}");
        }
    }

    /// A resume has landed only when the player says it is Playing. Not "the call worked",
    /// which is what this code used to accept and what let a nine minute silence be logged
    /// as `resumed 1 player(s)`.
    #[test]
    fn only_playing_is_a_resume() {
        assert!(resume_landed(PlayerStatus::Playing));
        for s in [
            PlayerStatus::Paused,
            PlayerStatus::Stopped,
            PlayerStatus::Other,
            PlayerStatus::Unreadable,
        ] {
            assert!(!resume_landed(s), "{s:?}");
        }
    }

    /// `PlayPause` is a TOGGLE. It may only be sent from a state we have just measured as
    /// definitely not playing; on `Playing` it would silence the user's media and on
    /// `Unreadable` or `Other` it is a coin flip against it.
    #[test]
    fn the_toggle_escalation_is_only_reachable_from_a_measured_stop() {
        assert!(escalate_to_play_pause(PlayerStatus::Paused));
        assert!(escalate_to_play_pause(PlayerStatus::Stopped));
        for s in [PlayerStatus::Playing, PlayerStatus::Other, PlayerStatus::Unreadable] {
            assert!(!escalate_to_play_pause(s), "{s:?}");
        }
    }

    /// The two sides must stay exact opposites on the one state both are asked about, or a
    /// player could be counted resumed and escalated at the same time.
    #[test]
    fn a_resumed_player_is_never_escalated() {
        for s in [
            PlayerStatus::Playing,
            PlayerStatus::Paused,
            PlayerStatus::Stopped,
            PlayerStatus::Other,
            PlayerStatus::Unreadable,
        ] {
            assert!(!(resume_landed(s) && escalate_to_play_pause(s)), "{s:?}");
        }
    }
}

#[cfg(test)]
mod pause_tally_tests {
    use super::PauseTally;

    /// The three no-op shapes must READ differently. They were one silence before
    /// DRAGON-733, and telling them apart is the whole reason this line exists.
    #[test]
    fn the_no_op_shapes_are_distinguishable() {
        let empty = PauseTally::default().summary();
        let idle = PauseTally { found: 2, playing: 0, paused: 0, unreadable: 0 }.summary();
        let refused = PauseTally { found: 2, playing: 1, paused: 0, unreadable: 0 }.summary();
        assert!(empty.contains("no media player is on the session bus"));
        assert!(idle.contains("no player was playing"));
        assert!(refused.contains("refused to pause"));
        assert_ne!(empty, idle);
        assert_ne!(idle, refused);
    }

    #[test]
    fn a_successful_pause_leads_with_the_count() {
        let t = PauseTally { found: 3, playing: 2, paused: 2, unreadable: 0 };
        assert!(t.summary().starts_with("pause other media: paused 2 other player(s)"));
    }

    /// Every count rides along even on the success line, because "we paused one of three
    /// playing players" is a different report from "we paused the only one".
    #[test]
    fn every_count_is_carried() {
        let t = PauseTally { found: 4, playing: 3, paused: 1, unreadable: 2 };
        let s = t.summary();
        assert!(s.contains("found=4"), "{s}");
        assert!(s.contains("playing=3"), "{s}");
        assert!(s.contains("paused=1"), "{s}");
        assert!(s.contains("unreadable=2"), "{s}");
    }

    /// The privacy rule (CLAUDE.md): this line records BEHAVIOUR. Nothing in it can carry a
    /// bus name, an app name or a track title, because the type it is built from holds
    /// nothing but counts. Pinned so a later "just add which player" cannot land quietly.
    #[test]
    fn the_summary_can_only_carry_counts() {
        let s = PauseTally { found: 1, playing: 1, paused: 1, unreadable: 0 }.summary();
        assert!(!s.contains("org.mpris"), "{s}");
        assert!(s.is_ascii(), "{s}");
    }
}

/// DRAGON-747: the GSMTC status mapping, proven on every host.
///
/// These integers are `Windows.Media.Control`'s, and this table is the only place the Windows
/// backend decides what a session's state MEANS. It is tested here, in the shared tree, for
/// the reason `platform::mod` keeps the Windows build-number gates out of the Windows plugin:
/// there is one Windows QA box and no ARM64 one at all, so a decision only Windows could test
/// is a decision nothing tests.
#[cfg(test)]
mod gsmtc_status_tests {
    use super::{PlayerStatus, escalate_to_play_pause, resume_landed};

    // `GlobalSystemMediaTransportControlsSessionPlaybackStatus`, verified against the
    // `windows` crate source rather than remembered.
    const CLOSED: i32 = 0;
    const OPENED: i32 = 1;
    const CHANGING: i32 = 2;
    const STOPPED: i32 = 3;
    const PLAYING: i32 = 4;
    const PAUSED: i32 = 5;

    /// The three values that name a definite playback state map straight across. Getting one
    /// of these wrong would invert the whole feature: reading Playing as Paused pauses nothing
    /// and then "resumes" media the user was already enjoying.
    #[test]
    fn the_three_playback_states_map_across() {
        assert_eq!(PlayerStatus::from_gsmtc(PLAYING), PlayerStatus::Playing);
        assert_eq!(PlayerStatus::from_gsmtc(PAUSED), PlayerStatus::Paused);
        assert_eq!(PlayerStatus::from_gsmtc(STOPPED), PlayerStatus::Stopped);
    }

    /// The other three do NOT name a playback state, so they must land in `Other`, which every
    /// decision refuses to act on. `Changing` is the dangerous one: it is mid-transition, so a
    /// session on its way INTO playing would, if this said `Stopped`, be handed a toggle and
    /// silenced.
    #[test]
    fn the_non_playback_states_are_other() {
        for raw in [CLOSED, OPENED, CHANGING] {
            assert_eq!(PlayerStatus::from_gsmtc(raw), PlayerStatus::Other, "{raw}");
        }
    }

    /// The safety property that mapping buys, stated directly: none of the three can ever
    /// reach the `TryTogglePlayPauseAsync` escalation, and none of them counts as a resume.
    #[test]
    fn a_session_in_a_non_playback_state_is_never_toggled() {
        for raw in [CLOSED, OPENED, CHANGING] {
            let s = PlayerStatus::from_gsmtc(raw);
            assert!(!escalate_to_play_pause(s), "{raw} escalated");
            assert!(!resume_landed(s), "{raw} counted as resumed");
        }
    }

    /// An integer the enum does not define is not evidence of anything, exactly as an
    /// unknown MPRIS string is not. Pinned across the edges so a sign or width mistake in a
    /// future WinRT bump cannot read as a real state.
    #[test]
    fn an_undefined_value_is_never_guessed_at() {
        for raw in [-1, 6, 7, 99, i32::MIN, i32::MAX] {
            assert_eq!(PlayerStatus::from_gsmtc(raw), PlayerStatus::Other, "{raw}");
        }
    }

    /// The two readers must agree, because ONE set of decisions is built on top of them. If
    /// they ever disagreed, Linux and Windows would be running different features under the
    /// same log lines.
    #[test]
    fn both_backends_produce_the_same_states() {
        assert_eq!(PlayerStatus::from_gsmtc(PLAYING), PlayerStatus::from_mpris("Playing"));
        assert_eq!(PlayerStatus::from_gsmtc(PAUSED), PlayerStatus::from_mpris("Paused"));
        assert_eq!(PlayerStatus::from_gsmtc(STOPPED), PlayerStatus::from_mpris("Stopped"));
        assert_eq!(PlayerStatus::from_gsmtc(CHANGING), PlayerStatus::from_mpris("Buffering"));
    }
}

/// DRAGON-747: the PAUSE-side toggle escalation, added once the field proved GSMTC's
/// `Ok(true)` lies. A browser answered the pause and kept playing, and the tally believed it.
#[cfg(test)]
mod pause_escalation_tests {
    use super::{
        PlayerStatus, escalate_pause_to_play_pause, escalate_to_play_pause, pause_landed,
    };

    const EVERY_STATE: [PlayerStatus; 5] = [
        PlayerStatus::Playing,
        PlayerStatus::Paused,
        PlayerStatus::Stopped,
        PlayerStatus::Other,
        PlayerStatus::Unreadable,
    ];

    /// The toggle may only be aimed at a player we have just watched keep PLAYING. From
    /// anywhere else it is the wrong command: on a player that really paused it starts the
    /// music again, and on one we could not read it is a coin flip against it.
    #[test]
    fn the_toggle_is_only_reachable_from_a_measured_play() {
        assert!(escalate_pause_to_play_pause(PlayerStatus::Playing));
        for s in EVERY_STATE {
            if s == PlayerStatus::Playing {
                continue;
            }
            assert!(!escalate_pause_to_play_pause(s), "{s:?}");
        }
    }

    /// A player already counted as paused must never also be a toggle candidate, or one
    /// settle could pause it and then start it again inside the same engage.
    #[test]
    fn a_paused_player_is_never_escalated() {
        for s in EVERY_STATE {
            assert!(!(pause_landed(s) && escalate_pause_to_play_pause(s)), "{s:?}");
        }
    }

    /// The gate and the landing test are exact complements today. Pinned so that a change to
    /// either one is a visible change to both, rather than a silent drift in when we command
    /// somebody's media.
    #[test]
    fn the_gate_and_the_landing_test_stay_exact_complements() {
        for s in EVERY_STATE {
            assert_eq!(pause_landed(s), !escalate_pause_to_play_pause(s), "{s:?}");
        }
    }

    /// The two escalation gates must never accept the same state. One of them wants the
    /// player stopped and the other wants it started, so a state both accepted would mean
    /// the same toggle was correct for two opposite goals.
    #[test]
    fn the_two_escalation_gates_are_disjoint() {
        for s in EVERY_STATE {
            assert!(
                !(escalate_to_play_pause(s) && escalate_pause_to_play_pause(s)),
                "{s:?}"
            );
        }
    }

    /// The field case in the terms the Windows backend actually sees: the browser answered
    /// `Ok(true)` and still reported GSMTC `Playing` (4). That session is exactly the one the
    /// toggle exists for, and it is what the old code counted as a successful pause.
    #[test]
    fn the_lying_player_from_the_field_is_a_toggle_candidate() {
        let after = PlayerStatus::from_gsmtc(4);
        assert!(escalate_pause_to_play_pause(after));
        assert!(!pause_landed(after), "it must not be counted as paused");
    }

    /// The same case in the terms the LINUX backend sees, since DRAGON-741 part 2 wired the
    /// escalation there too: an MPRIS player that answered `Pause` and still reports the
    /// string `"Playing"`. Both readers must reach the same verdict, or the two platforms
    /// would be running different features under one set of log lines.
    #[test]
    fn the_mpris_reading_of_that_player_agrees() {
        let after = PlayerStatus::from_mpris("Playing");
        assert!(escalate_pause_to_play_pause(after));
        assert!(!pause_landed(after), "it must not be counted as paused");
        assert_eq!(after, PlayerStatus::from_gsmtc(4));
    }

    /// The one state the toggle must never be sent from on either backend: a player we could
    /// not read. Sending it there is a coin flip that can start music at a user who asked for
    /// quiet, which is the failure this gate exists to make unreachable.
    #[test]
    fn an_unreadable_player_is_left_alone() {
        assert!(!escalate_pause_to_play_pause(PlayerStatus::Unreadable));
        assert!(
            pause_landed(PlayerStatus::Unreadable),
            "it is still counted as paused, so the resume path re-reads it and puts it back"
        );
    }
}

/// DRAGON-752: the Now Playing mapping, and the safety property it buys.
///
/// The mac backend reads through this on every settle, so what a snapshot MEANS is decided
/// here, in the shared tree, and proven on every host. It matters more than the sibling
/// tables: MediaRemote answers about the slot rather than about a player, so this is the only
/// thing standing between "our pause was ignored" and "somebody else started playing".
#[cfg(test)]
mod now_playing_status_tests {
    use super::{
        PlayerStatus, escalate_pause_to_play_pause, escalate_to_play_pause, pause_landed,
        resume_landed,
    };

    /// A plausible PID for the app we paused, and one for a different app.
    const OURS: i32 = 4711;
    const THEIRS: i32 = 90210;
    /// What the proxy prints when nothing owns the Now Playing slot.
    const NOBODY: i32 = 0;

    /// The player we actually paused maps straight across, both ways round. Getting this wrong
    /// would invert the feature: reading Playing as Paused claims a pause that never happened
    /// and then "resumes" media the user was enjoying all along.
    #[test]
    fn the_player_we_paused_maps_straight_across() {
        assert_eq!(PlayerStatus::from_now_playing(OURS, OURS, true), PlayerStatus::Playing);
        assert_eq!(PlayerStatus::from_now_playing(OURS, OURS, false), PlayerStatus::Paused);
    }

    /// A snapshot about a DIFFERENT app is not evidence about ours, whatever it says. This is
    /// the case the scoping exists for: without it, "something is playing" moments after our
    /// pause reads as our player ignoring us.
    #[test]
    fn a_snapshot_about_somebody_else_says_nothing_about_us() {
        for playing in [true, false] {
            assert_eq!(
                PlayerStatus::from_now_playing(OURS, THEIRS, playing),
                PlayerStatus::Other,
                "playing={playing}"
            );
        }
    }

    /// Nothing owning the slot, and no player of our own to ask about, are both `Other`. A PID
    /// has to be positive to be one, so a negative on either side is refused too rather than
    /// compared for equality and believed.
    #[test]
    fn only_a_real_pid_on_both_sides_is_a_playback_state() {
        for (subject, owner) in
            [(OURS, NOBODY), (NOBODY, OURS), (NOBODY, NOBODY), (-1, -1), (-1, OURS), (OURS, -1)]
        {
            assert_eq!(
                PlayerStatus::from_now_playing(subject, owner, true),
                PlayerStatus::Other,
                "subject={subject} owner={owner}"
            );
        }
    }

    /// The safety property, stated directly: a slot held by a stranger, or by nobody, can
    /// never reach EITHER toggle, and can never count as our resume. Both escalations send a
    /// play/pause toggle, so a state that slipped through here would command the media of a
    /// user who had just started playing something.
    #[test]
    fn a_stranger_at_the_slot_is_never_commanded() {
        for (subject, owner) in [(OURS, THEIRS), (OURS, NOBODY), (NOBODY, THEIRS)] {
            for playing in [true, false] {
                let s = PlayerStatus::from_now_playing(subject, owner, playing);
                assert!(!escalate_pause_to_play_pause(s), "{subject}/{owner} paused-side");
                assert!(!escalate_to_play_pause(s), "{subject}/{owner} resume-side");
                assert!(!resume_landed(s), "{subject}/{owner} counted as resumed");
            }
        }
    }

    /// `Stopped` is unreachable from this reader, because MediaRemote's read is a bool and
    /// cannot tell paused from stopped. That costs nothing only while no decision here
    /// separates the two, so pin exactly that rather than trusting it to stay true. If this
    /// fails, the mac reader needs a `Stopped` answer and cannot give one.
    #[test]
    fn the_distinction_mediaremote_cannot_make_is_one_no_decision_reads() {
        let (p, s) = (PlayerStatus::Paused, PlayerStatus::Stopped);
        assert_eq!(pause_landed(p), pause_landed(s));
        assert_eq!(resume_landed(p), resume_landed(s));
        assert_eq!(escalate_to_play_pause(p), escalate_to_play_pause(s));
        assert_eq!(escalate_pause_to_play_pause(p), escalate_pause_to_play_pause(s));
    }

    /// The field case in mac terms: we paused the app that owns the slot, and it is still that
    /// app, still playing. That is exactly the session the pause-side toggle exists for, and
    /// exactly what the old mac code counted as a successful pause.
    #[test]
    fn a_player_that_kept_playing_is_a_toggle_candidate() {
        let after = PlayerStatus::from_now_playing(OURS, OURS, true);
        assert!(escalate_pause_to_play_pause(after));
        assert!(!pause_landed(after), "it must not be counted as paused");
    }

    /// All three readers must agree on the states they share, because ONE set of decisions is
    /// built on top of them. If they drifted, the three platforms would be running different
    /// features under the same log lines.
    #[test]
    fn all_three_readers_produce_the_same_states() {
        assert_eq!(
            PlayerStatus::from_now_playing(OURS, OURS, true),
            PlayerStatus::from_mpris("Playing")
        );
        assert_eq!(PlayerStatus::from_now_playing(OURS, OURS, true), PlayerStatus::from_gsmtc(4));
        assert_eq!(
            PlayerStatus::from_now_playing(OURS, OURS, false),
            PlayerStatus::from_mpris("Paused")
        );
        assert_eq!(PlayerStatus::from_now_playing(OURS, OURS, false), PlayerStatus::from_gsmtc(5));
    }
}

/// DRAGON-747: the bounded settle, driven by a scripted reader instead of a real player.
///
/// Every one of these runs in microseconds: a poll gap of 0 costs no real sleeping, and the
/// bounded case uses a budget in the low tens of milliseconds.
#[cfg(test)]
mod settle_status_tests {
    use super::{PlayerStatus, pause_landed, resume_landed, settle_status_with};

    /// A player that obeys on the second look is settled on the second look, and the loop
    /// stops there. Not a third read, and not the whole budget.
    #[test]
    fn it_returns_as_soon_as_done_accepts() {
        let script = [PlayerStatus::Playing, PlayerStatus::Paused];
        let mut reads = 0usize;
        let got = settle_status_with(
            5_000,
            0,
            || {
                let s = script[reads.min(script.len() - 1)];
                reads += 1;
                s
            },
            pause_landed,
        );
        assert_eq!(got, PlayerStatus::Paused);
        assert_eq!(reads, 2);
    }

    /// A healthy player costs exactly ONE round trip and no sleep, which is what makes it
    /// affordable to verify every call.
    #[test]
    fn a_player_that_already_obeyed_costs_one_read() {
        let mut reads = 0usize;
        let got = settle_status_with(
            5_000,
            0,
            || {
                reads += 1;
                PlayerStatus::Playing
            },
            resume_landed,
        );
        assert_eq!(got, PlayerStatus::Playing);
        assert_eq!(reads, 1);
    }

    /// The read happens BEFORE the deadline test, so even a spent budget asks the player once.
    /// A settle that could return without looking would report every call as ignored, and the
    /// resume path would then fire a toggle at a player it never actually measured.
    #[test]
    fn it_reads_once_even_with_no_budget_left() {
        let mut reads = 0usize;
        let got = settle_status_with(
            0,
            0,
            || {
                reads += 1;
                PlayerStatus::Playing
            },
            pause_landed,
        );
        assert_eq!(got, PlayerStatus::Playing);
        assert_eq!(reads, 1);
    }

    /// A player that never obeys must end the loop anyway. This child is detached, with no
    /// window and no user, so an unbounded wait here is a process that outlives the session.
    #[test]
    fn a_player_that_never_obeys_is_bounded() {
        let mut reads = 0usize;
        let got = settle_status_with(
            20,
            1,
            || {
                reads += 1;
                PlayerStatus::Playing
            },
            pause_landed,
        );
        assert_eq!(got, PlayerStatus::Playing);
        assert!(reads >= 1, "the reader was never called");
    }

    /// It reports what it LAST saw, not a default and not the first thing it read. The caller
    /// re-tests that value to decide whether the call landed, so a stale return would be a
    /// wrong answer rather than a missing one.
    #[test]
    fn it_returns_the_last_state_it_read() {
        // Not one of these is `Playing`, so `resume_landed` never accepts and the budget is
        // what stops the loop. The value it hands back must be the reader's FINAL word.
        let script = [PlayerStatus::Stopped, PlayerStatus::Paused, PlayerStatus::Unreadable];
        let (mut reads, mut last) = (0usize, PlayerStatus::Playing);
        let got = settle_status_with(
            20,
            1,
            || {
                let s = script[reads.min(script.len() - 1)];
                reads += 1;
                last = s;
                s
            },
            resume_landed,
        );
        assert_eq!(got, last, "the settle returned something other than its final read");
        assert!(reads >= 1, "the reader was never called");
    }
}
