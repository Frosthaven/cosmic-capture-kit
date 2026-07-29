//! DRAGON-419: the opt-in debug log — ONE cross-platform file that can name a customer's
//! failure without asking them anything.
//!
//! # Why this exists
//!
//! A paying macOS customer could not capture anything: every mode failed, nothing was saved,
//! no preview appeared, and no message was shown. A full day of investigation could not name
//! the cause, because every non-delivery path in this app ends in a `log::warn!` and a
//! `finish_session()` — and on a packaged mac app (or a `setsid`-detached daemon child on any
//! OS) that warning goes to a stderr nobody has. The app had no way to explain itself after
//! the fact.
//!
//! # The mechanism, in one paragraph
//!
//! [`init`] installs a `log::Log` that wraps the process's normal `env_logger`. Stderr
//! behaviour is UNCHANGED (same `warn` default filter, same `RUST_LOG` handling). When the
//! log is ENABLED it additionally appends OUR OWN records at `debug` and above — plus any
//! dependency's `warn`/`error` (see [`DEP_FILE_LEVEL`]) — to one shared file, tagged with a
//! timestamp, the pid, and this process's [`Component`]. That is the whole
//! trick: every `log::warn!`/`info!`/`debug!` already in the codebase — including all nine
//! silent-exit paths — becomes a line in a file a customer can email, with no per-call-site
//! opt-in and no second logging system to keep in step.
//!
//! # The four hard requirements
//!
//! **Every launch path.** [`init`] is the first statement of `main`, before any subcommand
//! returns and before the resident-daemon branch. Every way of starting this program runs it:
//! a hotkey capture, a daemon-spawned child, a CLI run, Finder / Start Menu, the settings
//! window, each detached share helper. They all APPEND to the same file, so a multi-process
//! session reads as one story ordered by time, with `pid=` and the component tag to untangle
//! the strands. Shape borrowed wholesale from `platform/windows/diag.rs` (DRAGON-406), which
//! already proved it: one open-append-write-close per line, so processes interleave BETWEEN
//! lines and never split one, and no process holds a handle that would block a rotation.
//!
//! **All platforms.** The predecessor (`CCK_LOG_FILE`) was `#[cfg(windows)]`, which is exactly
//! why macOS had nothing to show. This is portable by construction: only [`log_dir`] has
//! per-OS arms.
//!
//! **Bounded.** [`MAX_LOG_BYTES`] rotates the live file to a single `.1` backup;
//! [`MAX_SESSION_BYTES`] caps what any one process can add; and because the daemon is
//! long-lived, rotation is re-checked every [`ROTATE_CHECK_BYTES`] this process writes rather
//! than only at start. Worst case on disk is stated at [`MAX_LOG_BYTES`].
//!
//! **Cheap when off.** Off is the default. When off, `log::max_level()` is whatever the
//! stderr logger asked for (`warn`), so every `log::debug!` in the codebase is one relaxed
//! atomic load and a comparison — the same cost it has today. No file is opened, no directory
//! is created, no line is formatted, and nothing is written. The only startup cost is reading
//! the one setting: a single `read_to_string` of the config plus a TOML parse into a
//! one-field struct (`main` already loads the full config a few lines later for the resident
//! branch).
//!
//! # THE PRIVACY RULE — read this before adding a line
//!
//! **Log the app's own BEHAVIOUR, not the user's CONTENT.**
//!
//! This file gets emailed to us by customers. It must be safe to send and safe to ask for.
//! Concretely, never write:
//!
//! * window titles, application names, or monitor model strings (a capture tool's window
//!   titles name the documents someone was screenshotting),
//! * capture / save / recording file paths, or any part of a filename,
//! * clipboard contents, OCR or QR-decoded text, audio, or pixels,
//! * usernames, hostnames, network names, or anything derived from `$HOME`.
//!
//! Where something is genuinely undiagnosable without a path or a title, log its SHAPE
//! instead of its value — [`path_shape`] gives you `path(ext=png, name_len=27, depth=4,
//! exists=true, dir_writable=true)`, which distinguishes "the folder is gone", "the disk
//! refused the write" and "we never had a name" without carrying a byte of the user's
//! filesystem. [`redact_args`] does the same job for argv: flags survive, values become
//! `<value>`. Both are unit-tested, so the rule is a property of the code and not a habit.
//!
//! The one deliberate exception is the app's OWN resolved log directory, which is shown in
//! Settings so a customer can find the file. It is never written INTO the log.
//!
//! # The file holds ONE session — the user's (DRAGON-424)
//!
//! A customer enables the log, reproduces the failure once, and mails the file. That only
//! works if the file contains their session and nothing else, so a build/test process never
//! writes to it: [`is_dev_process`] sends anything cargo runs — the test harness, `cargo run`,
//! and every child they spawn — to a temp sandbox instead, and an explicit [`PATH_ENV`] that
//! points inside the user's folder is refused there. The rule lives in [`resolve_path`], which
//! is pure and unit-tested, and `tests/log_isolation.rs` proves it end to end against a real
//! spawned child, which is the shape the leak actually had.
//!
//! # How to add a log line
//!
//! Just use `log::debug!` / `log::info!` / `log::warn!` where the interesting thing happens —
//! it lands in the file automatically. Two helpers exist for the cases that matter most:
//!
//! * [`note_failure`] at any path that ends a session without delivering something. It warns
//!   through the normal `log` facade (so stderr keeps working exactly as before) AND records
//!   the reason, so [`session_end`] can name it. Adding one of these to a silent-exit path is
//!   a ONE-LINE change; prefer that to restructuring the path.
//! * [`Failure`] is the closed vocabulary of those reasons. A new silent-exit path should add
//!   a variant rather than an ad-hoc string, so a support reply can be written against a
//!   stable code.
//!
//! # What this consolidated (DRAGON-419)
//!
//! * `CCK_LOG_FILE` — the `#[cfg(windows)]` file log this generalises. The variable still
//!   works, on every platform, and now means "enable the debug log and put it HERE"
//!   ([`PATH_ENV`]).
//! * `CCK_TIMING` / `util::timing_mark` — folded IN, not removed. The marks still print to
//!   stderr under `CCK_TIMING`, and they now also reach this file at `debug` under the
//!   `timing` target whenever the log is on. They blanket `main` → `App::init` →
//!   `acquire_scene` → `seed_overlays` → `configure_overlay`, so on a launch that HANGS the
//!   last mark in the file names the call that never returned. That is the whole answer for a
//!   launch-hang report, and it came for free.
//!
//! NOT consolidated, deliberately: the DRAGON-406 Windows-10 report
//! (`platform/windows/diag.rs`). It is a live instrument for the unresolved DRAGON-408 and
//! carries content this log does not reproduce (the swapchain/DirectComposition probe and the
//! per-window style read-backs). It writes into the SAME folder as this log, so "send me the
//! logs folder" gets both. See DRAGON-407 for the removal accounting.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::OnceLock;

// ── Configuration: names, sizes, environment ─────────────────────────────────

/// The live log's basename inside [`log_dir`].
const LOG_NAME: &str = "debug.log";
/// The single rotation backup.
const LOG_BACKUP: &str = "debug.1.log";

/// `CCK_DEBUG_LOG=1` forces the log ON for this run, `=0` forces it OFF, whatever the
/// persisted setting says. The support path for "run this once from a terminal" without
/// walking a customer through the settings window first.
pub const ENABLE_ENV: &str = "CCK_DEBUG_LOG";

/// `CCK_LOG_FILE=<path>` — the DRAGON-233 Windows-only variable, generalised. Setting it
/// enables the log for this run AND redirects it to that exact file (no rotation is applied
/// to a path the caller chose; that is the caller's business). Kept working because existing
/// support instructions and ticket comments still tell people to set it.
pub const PATH_ENV: &str = "CCK_LOG_FILE";

/// Rotate the live file to its single `.1` backup at 2 MiB.
///
/// **Worst case on disk.** Two files. Each is bounded by this cap plus the overshoot a
/// process can add between rotation checks, which is at most [`ROTATE_CHECK_BYTES`] per
/// concurrently-writing process (a rotation is only observed at a check). In resident mode
/// the concurrent set is the daemon plus its one-shot children, so with a generous eight
/// processes that is 2 MiB + 512 KiB per file: **under 5 MiB total**, and in the ordinary
/// single-process case under 4.1 MiB. Small enough to attach to an email, which is the whole
/// point of having a cap at all.
pub const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

/// The per-PROCESS write budget. The resident daemon runs for days, so without this one
/// process could dominate the file between rotations. On reaching it the process writes one
/// final line saying so and then goes quiet; it never starts failing or slowing the app.
pub const MAX_SESSION_BYTES: u64 = 512 * 1024;

/// How much THIS process may write between rotation checks. Rotation at process start alone
/// is not enough for a daemon that outlives many rotations (the DRAGON-406 report has exactly
/// that hole); re-checking every 64 KiB closes it at the cost of one `metadata` call per
/// 64 KiB written.
pub const ROTATE_CHECK_BYTES: u64 = 64 * 1024;

/// The level at which OUR OWN records reach the FILE when the log is enabled. `Debug`, not
/// `Trace`: trace is reserved for per-frame firehoses that would bury the story, and nothing
/// in this app's diagnosis has ever needed it.
const FILE_LEVEL: log::Level = log::Level::Debug;

/// The log target prefix of this crate's own records. `log`'s default target is the module
/// path, so every line we write starts with this and no dependency's does.
const OWN_TARGET: &str = "cosmic_capture_kit";

/// The level a DEPENDENCY's record must reach to be written to the file (DRAGON-421).
///
/// **Why dependencies are not logged at `debug`.** Turning the file on raises
/// `log::max_level()` to `Debug` for the whole process, which enables `log::debug!` in every
/// crate we link — and this app links `naga`, `wgpu`, `zbus` and the wayland stack, which are
/// among the most verbose debug emitters in the ecosystem. Measured on the owner's own
/// machine with the log enabled: EVERY process burned its entire [`MAX_SESSION_BYTES`] budget
/// on `naga` shader-lowering dumps within a few MILLISECONDS of launch, then went quiet — so
/// the file contained thousands of lines of `Resolving [17] = Literal(AbstractFloat(0.0))`
/// and not one line about the capture, the recording, or the failure it was turned on to
/// diagnose. Two whole 2 MiB files, and the recording-health line DRAGON-411/419 exists to
/// surface was in neither.
///
/// A diagnostic that reliably spends its budget before reaching the subject is worse than no
/// diagnostic: it costs the write, it costs the reader's time, and it makes a real failure
/// look like an absence of evidence. Dependencies still matter when they COMPLAIN, so their
/// warnings and errors are kept — that is the part that has ever named a cause.
const DEP_FILE_LEVEL: log::Level = log::Level::Warn;

/// Our own records that do NOT use the default module-path target, and so cannot be
/// recognised by the [`OWN_TARGET`] prefix. Both are load-bearing for this log: `timing` is
/// DRAGON-419's launch-hang answer (the last mark in the file names the call that never
/// returned) and it is emitted at `debug`, which is exactly the level the dependency rule
/// drops. Anything that gives itself a custom target must be listed here.
const OWN_EXTRA_TARGETS: [&str; 2] = ["timing", "cck::failure"];

/// Whether `target` names a record this crate emitted (rather than a dependency's).
fn is_own_target(target: &str) -> bool {
    target == OWN_TARGET
        || target.strip_prefix(OWN_TARGET).is_some_and(|rest| rest.starts_with("::"))
        || OWN_EXTRA_TARGETS.contains(&target)
}

/// Whether a record with this `target`/`level` belongs in the file (see [`DEP_FILE_LEVEL`]).
fn wanted_in_file(target: &str, level: log::Level) -> bool {
    level <= if is_own_target(target) { FILE_LEVEL } else { DEP_FILE_LEVEL }
}

// ── Process-wide state ───────────────────────────────────────────────────────

/// Whether the file sink is live. Checked on every record that clears `log::max_level()`, so
/// it is a plain relaxed atomic and nothing more.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Bytes this process has written, against [`MAX_SESSION_BYTES`].
static SESSION_BYTES: AtomicU64 = AtomicU64::new(0);

/// Bytes written since this process last checked whether the file wants rotating.
static SINCE_ROTATE_CHECK: AtomicU64 = AtomicU64::new(0);

/// This process's [`Component`], as its `u8` discriminant.
static COMPONENT: AtomicU8 = AtomicU8::new(Component::App as u8);

/// The resolved log file path, computed once at [`init`].
static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// The last [`Failure`] this process noted, so [`session_end`] can name it.
static LAST_FAILURE: std::sync::Mutex<Option<(Failure, String)>> = std::sync::Mutex::new(None);

/// The FIRST [`Failure`] this process noted — the root cause (DRAGON-415).
///
/// Failures are noted in causal order: the ScreenCaptureKit call that never answered is
/// recorded before the grab that consequently returned nothing, and the TCC denial before
/// both. [`note_failure`] overwrites, so [`last_failure`] ends up holding the SYMPTOM and
/// the diagnosis is only recoverable by reading the file. That is fine for a log a human
/// reads top to bottom and wrong for anything that has to act on one code — the
/// user-facing alert, which must say "screen capture isn't responding", not "the capture
/// came back empty", when both are true of the same session.
static FIRST_FAILURE: std::sync::Mutex<Option<(Failure, String)>> = std::sync::Mutex::new(None);

/// The filter the stderr logger asked for, so [`set_enabled`] can restore `log::max_level()`
/// when the log is turned back off from the settings window.
static STDERR_FILTER: OnceLock<log::LevelFilter> = OnceLock::new();

// ── Component: which process is talking ──────────────────────────────────────

/// Which of this program's processes wrote a line.
///
/// The app is one binary that runs in several shapes at once — a resident daemon, the
/// one-shot capture children it spawns, a settings window, and small detached helpers for
/// clipboard/notify/reveal. They all append to one file, so without this tag a multi-process
/// session is unreadable. Derived from argv by [`component_from_args`] at [`init`], and
/// re-stamped by [`mark_component`] where argv cannot tell (the resident daemon is a *bare*
/// launch, decided later by a persisted setting).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Component {
    /// A GUI launch that is none of the below (bare launch, `--overlay`, unknown flags).
    App = 0,
    /// The resident tray / menu-bar daemon.
    Daemon = 1,
    /// A capture launch — a hotkey, a daemon child, or a CLI capture flag.
    Capture = 2,
    /// The settings window (`--settings`).
    Settings = 3,
    /// A preview-only launch (`--preview <file>`).
    Preview = 4,
    /// The macOS permission-checker window (`--permissions`).
    Permissions = 5,
    /// A small detached helper child: clipboard copy, notify, reveal, open-uri, ducking,
    /// the AeroSpace babysitter, the Windows update swap.
    Helper = 6,
    /// A non-GUI CLI subcommand (`--test`, `--inspect`, `--help`, the sync-calibration
    /// tools).
    Cli = 7,
}

impl Component {
    /// The tag written on every line. Fixed-width-ish and lowercase so a reader can scan a
    /// column of them.
    pub fn tag(self) -> &'static str {
        match self {
            Component::App => "app",
            Component::Daemon => "daemon",
            Component::Capture => "capture",
            Component::Settings => "settings",
            Component::Preview => "preview",
            Component::Permissions => "permissions",
            Component::Helper => "helper",
            Component::Cli => "cli",
        }
    }

    /// Inverse of the `u8` discriminant, for [`COMPONENT`]. An unknown byte cannot occur (we
    /// only ever store a real discriminant) but must not panic if it somehow does.
    fn from_u8(v: u8) -> Component {
        match v {
            1 => Component::Daemon,
            2 => Component::Capture,
            3 => Component::Settings,
            4 => Component::Preview,
            5 => Component::Permissions,
            6 => Component::Helper,
            7 => Component::Cli,
            _ => Component::App,
        }
    }
}

/// Classify this process from its argv.
///
/// Pure and unit-tested, because getting it wrong makes a multi-process file unreadable in
/// exactly the situation it exists for. The order matters and mirrors `main`'s own dispatch
/// order: helpers and CLI subcommands return before any GUI work, so they are checked first;
/// a capture-mode flag beats everything else because "capture NOW" is the intent that matters
/// to a reader. A bare launch is [`Component::App`] here and is re-stamped to
/// [`Component::Daemon`] by `main` if the resident branch takes it.
pub fn component_from_args<S: AsRef<str>>(args: &[S]) -> Component {
    let has = |flag: &str| args.iter().skip(1).any(|a| a.as_ref() == flag);
    // Detached helper children first: they are the shortest-lived and most numerous, and a
    // reader scanning for the capture story wants them labelled out of the way.
    const HELPER_FLAGS: &[&str] = &[
        "--copy-image",
        "--copy-file",
        "--copy-text",
        "--open-uri",
        "--reveal",
        "--notify-copied",
        "--notify-saved",
        "--duck",
        "--focus-settings",
        "--update-swap",
        "--restore-aerospace",
    ];
    if HELPER_FLAGS.iter().any(|f| has(f)) {
        return Component::Helper;
    }
    const CLI_FLAGS: &[&str] =
        &["--test", "--inspect", "--help", "-h", "--make-sync-clip", "--calibrate-sync"];
    if CLI_FLAGS.iter().any(|f| has(f)) {
        return Component::Cli;
    }
    const CAPTURE_FLAGS: &[&str] = &[
        "--region",
        "--window",
        "--monitor",
        "--all-in-one",
        "--active-window",
        "--active-monitor",
        "--image",
        "--video",
        "--scan",
        "--scanner",
        "--countdown",
    ];
    if CAPTURE_FLAGS.iter().any(|f| has(f)) {
        return Component::Capture;
    }
    if has("--settings") {
        return Component::Settings;
    }
    if has("--preview") {
        return Component::Preview;
    }
    if has("--permissions") {
        return Component::Permissions;
    }
    Component::App
}

/// This process's component tag.
pub fn component() -> Component {
    Component::from_u8(COMPONENT.load(Ordering::Relaxed))
}

/// Re-stamp this process's component, for the cases argv cannot decide.
///
/// The resident daemon is a *bare* launch that becomes the daemon only after a persisted
/// setting is read, so `main`'s daemon branch calls this just before the tray loop takes
/// over. From that line on, every entry from this pid reads `daemon` — which is what
/// distinguishes the long-lived tray process from the one-shot children it spawns.
pub fn mark_component(c: Component) {
    COMPONENT.store(c as u8, Ordering::Relaxed);
    if is_enabled() {
        write_line(&format!("component: this process is now `{}`", c.tag()));
    }
}

// ── Failure vocabulary ───────────────────────────────────────────────────────

/// The closed vocabulary of "this session is ending and the user did not get what they asked
/// for" reasons.
///
/// Every one of these corresponds to a real path that today ends in a `log::warn!` and a
/// `finish_session()` — silent from the user's seat, and (before this ticket) silent from
/// ours too on any launch without a terminal. The point of an enum rather than free text is
/// that a support answer, a test, and a `grep` can all be written against a STABLE code.
///
/// Codes are lowercase-hyphenated and must never change once shipped: a customer's log from
/// an old build is read against the same vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Failure {
    /// No display/output could be enumerated, so no overlay could be minted and no capture
    /// target exists. On macOS this is the shape of a ScreenCaptureKit that has stopped
    /// answering; on Linux, a compositor with no usable capture protocol.
    /// Constructed by `seed_overlays_mac`, which is `not(target_os = "linux")` — Linux
    /// receives its outputs through Wayland `OutputEvent`s and has no single "we ended up
    /// with none" moment to record.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    NoOutputs,
    /// A screen-recording / capture permission the platform requires is missing or was
    /// refused. macOS TCC — the only platform with a preflight that can answer "no" while
    /// the app keeps running.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    PermissionDenied,
    /// A platform capture call did not answer within its timeout. On macOS both
    /// ScreenCaptureKit entry points sit behind a flat 10s deadline, and a slow SCK and a
    /// failed one are otherwise indistinguishable.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    SceneGrabTimeout,
    /// Pixel capture returned nothing — the region/monitor/window grab produced no image.
    NoImage,
    /// Writing the capture to disk failed (a full disk, an unwritable capture folder, a
    /// vanished directory).
    SaveFailed,
    /// A capture/compose/save worker thread died before reporting a result. Distinguishable
    /// from an empty capture ONLY if it is recorded here — the two are otherwise laundered
    /// into the same boolean.
    WorkerPanic,
    /// Decoding a captured image for the preview editor failed or its worker died. The file
    /// is on disk; the editor is what was lost.
    DecodeFailed,
    /// A preview surface was destroyed out of band (a compositor/WM took the window away)
    /// and took the session with it.
    PreviewSurfaceLost,
    /// There was no output to open a preview on, so the capture was saved/copied/notified but
    /// no editor appeared.
    NoPreviewOutput,
    /// The capture was handed to an already-running preview host, which accepted it. NOT an
    /// error — recorded because from this process's seat it is indistinguishable from a
    /// silent exit, and a reader needs to be able to tell them apart.
    HandoffAccepted,
    /// Recording failed. The worker's own reason string is carried in the detail — it names
    /// the pre-flight stage (pulse client / FIFO / mic chain) or the muxer failure, and it is
    /// the whole diagnosis for a recording that produced nothing.
    RecordingFailed,
    /// A recording session stopped making progress and was ended by the session-level bound
    /// (DRAGON-423) rather than being left to hang.
    ///
    /// A member of the SAME vocabulary, not a second one — it earns its place because a hang
    /// and a failure need different things said. A failed recording produced nothing; a
    /// wedged one usually produced a take that was salvaged, and telling the user "nothing
    /// was saved" would be a lie. It also gives the shape its own stable code in the log,
    /// which is the whole reason that log exists. The detail (`progress::wedge_detail`) says
    /// what stalled, for how long, and where the take went.
    RecordingWedged,
    /// The user cancelled — the session ended as designed. Recorded so a reader can tell a
    /// cancel apart from a failure without guessing.
    Cancelled,
}

impl Failure {
    /// The stable code written to the log. Never change one of these.
    pub fn code(self) -> &'static str {
        match self {
            Failure::NoOutputs => "no-outputs",
            Failure::PermissionDenied => "permission-denied",
            Failure::SceneGrabTimeout => "scene-grab-timeout",
            Failure::NoImage => "no-image",
            Failure::SaveFailed => "save-failed",
            Failure::WorkerPanic => "worker-panic",
            Failure::DecodeFailed => "decode-failed",
            Failure::PreviewSurfaceLost => "preview-surface-lost",
            Failure::NoPreviewOutput => "no-preview-output",
            Failure::HandoffAccepted => "handoff-accepted",
            Failure::RecordingFailed => "recording-failed",
            Failure::RecordingWedged => "recording-wedged",
            Failure::Cancelled => "cancelled",
        }
    }

    /// Whether this outcome means the user lost work.
    ///
    /// A cancel and an accepted handoff are ordinary endings, not defects — and telling them
    /// apart from the rest is the first thing anyone reading a "nothing happened" report has
    /// to do.
    pub fn is_loss(self) -> bool {
        !matches!(self, Failure::Cancelled | Failure::HandoffAccepted)
    }

    /// Whether the captured bytes reached disk before the session ended.
    ///
    /// This is the single most useful split in a "nothing happened" report — it halves the
    /// search space in one line. "No file in the capture folder" points at the capture; "a
    /// file is there but no editor opened" points at delivery, and the fixes have nothing in
    /// common.
    pub fn reached_disk(self) -> bool {
        matches!(
            self,
            Failure::DecodeFailed
                | Failure::PreviewSurfaceLost
                | Failure::NoPreviewOutput
                | Failure::HandoffAccepted
        )
    }
}

/// Record a failure and warn about it.
///
/// Deliberately shaped as a ONE-LINE addition at an existing failure site: it warns through
/// the normal `log` facade (so a terminal run behaves exactly as it did before) and stores the
/// reason for [`session_end`]. Nothing about the surrounding control flow needs to change.
///
/// `detail` must obey the privacy rule at the top of this module — describe the failure, not
/// the user's content. Use [`path_shape`] if a path is genuinely load-bearing.
pub fn note_failure(f: Failure, detail: &str) {
    if let Ok(mut slot) = LAST_FAILURE.lock() {
        *slot = Some((f, detail.to_string()));
    }
    // DRAGON-415: first-note-wins, so the ROOT cause survives being overwritten by the
    // symptom it produced (see [`FIRST_FAILURE`]).
    if let Ok(mut slot) = FIRST_FAILURE.lock()
        && slot.is_none()
    {
        *slot = Some((f, detail.to_string()));
    }
    log::warn!(target: "cck::failure", "[{}] {detail}", f.code());
}

/// The last failure this process noted, if any.
pub fn last_failure() -> Option<(Failure, String)> {
    LAST_FAILURE.lock().ok().and_then(|s| s.clone())
}

/// The FIRST failure this process noted — the root cause (see [`FIRST_FAILURE`]).
///
/// Prefer this wherever ONE code has to stand for the whole session; prefer
/// [`last_failure`] where "has anything gone wrong yet?" is the question (the guard
/// several sites use before noting an ordinary ending).
pub fn root_failure() -> Option<(Failure, String)> {
    FIRST_FAILURE.lock().ok().and_then(|s| s.clone())
}

/// One line at the lifecycle seam, naming how this session ended.
///
/// Called from `App::finish_session` — THE place every "we're done" path routes through — so
/// a reader always gets a verdict, and never has to infer one from the absence of lines.
/// `where_from` is the call site's own short description of the exit.
pub fn session_end(where_from: &str) {
    if !is_enabled() {
        return;
    }
    match last_failure() {
        Some((f, detail)) => {
            // DRAGON-415: name the ROOT cause too when it differs from the last note. A
            // stalled ScreenCaptureKit call is recorded before the empty grab it causes, and
            // `outcome=no-image` alone sends a reader looking at geometry when the answer was
            // three lines earlier.
            let root = match root_failure() {
                Some((r, _)) if r != f => format!(" root={}", r.code()),
                _ => String::new(),
            };
            write_line(&format!(
                "session end: outcome={} loss={} reached_disk={}{root} via={where_from} :: {detail}",
                f.code(),
                f.is_loss(),
                f.reached_disk(),
            ))
        }
        None => write_line(&format!("session end: outcome=delivered via={where_from}")),
    }
}

// ── Path resolution ──────────────────────────────────────────────────────────

/// The environment variable cargo sets for every binary it RUNS (DRAGON-424).
///
/// Cargo exports this into the environment of a `cargo test` harness, of `cargo run`, and —
/// because environments are inherited — of every child those processes spawn. A shipped build
/// never has it: a COSMIC shortcut, a `.desktop` launch, a packaged `.app`, a Start Menu
/// shortcut and the resident daemon all start with the user's own environment, and cargo does
/// not export anything into a login shell.
///
/// That inheritance is the whole reason this is the marker. See [`is_dev_process`].
const CARGO_ENV: &str = "CARGO_MANIFEST_DIR";

/// Whether this process is part of a build/test run rather than a real user session.
///
/// **The bug this exists for (DRAGON-424).** `cargo test` was appending to the owner's live
/// `debug.log` — a debug-profile 0.24.0 test binary interleaving its lines into the record of a
/// release 0.23.0 session, caught while that very log was being read to diagnose a recording
/// wedge. Three separate costs: it corrupts the evidence during exactly the investigation the
/// log exists for, it spends the [`MAX_SESSION_BYTES`] budget, and a suite that spawns many
/// short-lived processes can push a real session out through [`MAX_LOG_BYTES`] rotation.
///
/// **Why not `cfg!(test)` alone.** The observed leak was NOT the test harness writing. It was
/// `tests/cli.rs` spawning the REAL binary as a subprocess — a child built without `cfg(test)`,
/// which resolves its own path and knows nothing about the harness that started it. Any check
/// that lives only in test-compiled code misses the one case that actually happened.
///
/// **Why not "the binary sits under `target/`".** The owner's PrintScreen shortcut launches
/// `target/release/cosmic-capture-kit` directly. Treating a target-dir binary as a test would
/// silence the log for the person the log is for.
///
/// So the test is the inherited cargo environment, which is present for the harness AND its
/// children and absent for every shipped launch. `cfg!(test)` is kept as a cheap second
/// condition for the harness itself.
fn is_dev_process() -> bool {
    cfg!(test) || std::env::var_os(CARGO_ENV).is_some()
}

/// The sandbox folder a dev/test process writes to instead of the user's log folder.
///
/// Under the temp dir, not the user's data dir: a test run leaves nothing to clean up in a
/// place a customer might later be asked to send us. Keyed by the cargo manifest dir so
/// concurrent runs in different worktrees do not interleave into one file (and so two users
/// on one machine never collide over a `/tmp` folder the other created), but STABLE within a
/// run — a test's spawned children inherit `CARGO_MANIFEST_DIR` and so land in the same file
/// as the harness, which keeps the one-file multi-process story readable here too.
fn sandbox_log_dir() -> PathBuf {
    std::env::temp_dir().join(format!("cosmic-capture-kit-dev-logs-{}", sandbox_key()))
}

/// A short, deterministic key for [`sandbox_log_dir`].
///
/// FNV-1a, written out rather than taken from `DefaultHasher`, because the parent harness and
/// the child app are two different binaries that must agree on the answer, and std's hasher
/// only promises stability within one build.
fn sandbox_key() -> String {
    let seed = std::env::var_os(CARGO_ENV)
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cargo".to_string());
    fnv1a_hex(&seed)
}

/// FNV-1a as 16 hex digits. Pure, so [`sandbox_key`]'s "two binaries agree" property is
/// testable without spawning anything.
fn fnv1a_hex(seed: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in seed.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The caller's explicit [`PATH_ENV`] choice, if any.
fn explicit_override() -> Option<PathBuf> {
    std::env::var_os(PATH_ENV).map(PathBuf::from)
}

/// Decide the file this process writes to. Pure, so the rule is a tested fact.
///
/// The ordinary case is unchanged: an explicit [`PATH_ENV`] wins, else `debug.log` in the
/// user's log folder. A dev/test process gets ONE extra rule, and it is absolute — **the real
/// user log folder is unreachable**. It may still name an exact file (that is how a harness
/// redirects itself to a temp path, and how a controlled experiment works), but a path inside
/// the user's folder is refused and the sandbox is used instead. Without that last clause the
/// override would be a way to re-open the leak by accident.
///
/// `starts_with` is a component-wise prefix test on the paths as given; it is not proof against
/// a deliberately obfuscated `..` path, and does not need to be — the case it guards is a
/// harness pointing somewhere by mistake, not an attacker.
fn resolve_path(
    dev: bool,
    explicit: Option<PathBuf>,
    user_dir: Option<PathBuf>,
    sandbox_dir: &Path,
) -> Option<PathBuf> {
    if !dev {
        return explicit.or_else(|| Some(user_dir?.join(LOG_NAME)));
    }
    match explicit {
        Some(p) if !user_dir.is_some_and(|d| p.starts_with(&d)) => Some(p),
        _ => Some(sandbox_dir.join(LOG_NAME)),
    }
}

/// The folder the log lives in, per-platform conventional, whether or not it exists yet.
///
/// * **macOS** — `~/Library/Logs/cosmic-capture-kit`. The documented place for an app's logs,
///   visible in Console.app, and already home to `panic.log` (the crash hook in `main`), so
///   "send me that folder" gets the panic trace and the debug log together.
/// * **Windows** — `%LOCALAPPDATA%\cosmic-capture-kit\logs`. The same folder the DRAGON-406
///   report already uses, so the two files sit side by side and one instruction reaches both.
/// * **Linux** — `$XDG_STATE_HOME/cosmic-capture-kit/logs` (`~/.local/state/…`), which is
///   what the XDG basedir spec designates for logs, falling back to `$XDG_DATA_HOME`.
///
/// Never the capture folder (a user's pictures are not a place for our diagnostics) and never
/// next to the binary (read-only in every packaged form). Creates nothing — calling it is
/// always safe.
///
/// DRAGON-424: a dev/test process is sent to [`sandbox_log_dir`] instead. This is the folder
/// the settings row displays and its button opens, so "where it says it writes" and "where it
/// writes" stay the same statement under cargo as they are for a customer.
pub fn log_dir() -> Option<PathBuf> {
    if is_dev_process() {
        return Some(sandbox_log_dir());
    }
    user_log_dir()
}

/// The real user's log folder, ignoring the DRAGON-424 sandbox — the per-platform arms.
///
/// Split out from [`log_dir`] because the sandbox rule needs to know what it is protecting:
/// [`resolve_path`] refuses an explicit override that points inside this folder.
fn user_log_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(dirs::home_dir()?.join("Library").join("Logs").join("cosmic-capture-kit"))
    }
    #[cfg(windows)]
    {
        let base = dirs::data_local_dir()
            .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))?;
        Some(base.join("cosmic-capture-kit").join("logs"))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let base = dirs::state_dir().or_else(dirs::data_local_dir)?;
        Some(base.join("cosmic-capture-kit").join("logs"))
    }
}

/// The log file's full path — [`PATH_ENV`] when set, else `debug.log` in [`log_dir`], and
/// never the user's file when this is a dev/test process (see [`resolve_path`]).
pub fn log_path() -> Option<PathBuf> {
    resolve_path(is_dev_process(), explicit_override(), user_log_dir(), &sandbox_log_dir())
}

/// The resolved log folder as a display string for the settings row, or a plain statement
/// that no such folder exists on this machine. The description of that row is JUST this
/// (owner's spec) — no prose.
pub fn log_dir_display() -> String {
    match log_dir() {
        Some(d) => d.display().to_string(),
        None => "(no log folder is available on this system)".to_string(),
    }
}

/// Open the log folder in the desktop's file manager (the settings row's folder button).
///
/// Creates the folder first, so a customer who has not reproduced anything yet lands
/// somewhere real instead of on a file-manager error. Routes through the app's existing
/// detached open-URI helper, which is already correct on all three platforms.
pub fn open_log_dir() {
    let Some(dir) = log_dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    crate::share::open_uri(&crate::util::path_to_file_uri(&dir));
}

// ── Enable / disable ─────────────────────────────────────────────────────────

/// Whether the file sink is live for this process.
#[inline]
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Turn the file sink on or off at runtime, from the settings toggle.
///
/// Immediate, with no restart: a customer can turn it on, reproduce the failure once, and
/// mail the file. Turning it on writes a fresh session header so the file explains what it is
/// looking at even when the toggle was flipped mid-session.
pub fn set_enabled(on: bool) {
    if on == is_enabled() {
        return;
    }
    ENABLED.store(on, Ordering::Relaxed);
    let stderr = *STDERR_FILTER.get_or_init(log::max_level);
    if on {
        log::set_max_level(stderr.max(FILE_LEVEL.to_level_filter()));
        prepare_file();
        session_header("enabled from settings");
    } else {
        write_line("debug logging turned OFF from settings — nothing further is recorded");
        log::set_max_level(stderr);
    }
}

/// Resolve whether the log should start enabled. Env wins over the persisted setting, so a
/// support instruction always works regardless of what is stored.
fn resolve_enabled() -> bool {
    match std::env::var_os(ENABLE_ENV).and_then(|v| v.into_string().ok()) {
        Some(v) if v == "0" || v.eq_ignore_ascii_case("false") => return false,
        Some(_) => return true,
        None => {}
    }
    // The historical Windows variable: naming a file is asking for a log.
    if std::env::var_os(PATH_ENV).is_some() {
        return true;
    }
    persisted_setting()
}

/// Read JUST the `debug_logging` key out of the persisted config.
///
/// Deliberately not `state::store::load()`: that runs migrations and can WRITE the config as
/// a side effect, and this runs as the first statement of `main`, before any subcommand or
/// the daemon branch. A one-field struct plus serde's ignore-unknown-fields default gets the
/// one bit for one `read_to_string` and one parse, with no side effects at all.
fn persisted_setting() -> bool {
    #[derive(serde::Deserialize, Default)]
    struct Flag {
        #[serde(default)]
        debug_logging: bool,
    }
    let Some(path) = crate::state::config_path() else { return false };
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    toml::from_str::<Flag>(&text).map(|f| f.debug_logging).unwrap_or(false)
}

// ── The logger ───────────────────────────────────────────────────────────────

/// The `log::Log` this crate installs: the normal `env_logger` for stderr, plus the file sink
/// when enabled.
///
/// Wrapping rather than replacing is the point — a terminal run, `RUST_LOG`, and every
/// existing warning behave EXACTLY as they did before this ticket, and the file is purely
/// additive.
struct DiagLogger {
    stderr: env_logger::Logger,
}

impl log::Log for DiagLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.stderr.enabled(metadata)
            || (is_enabled() && wanted_in_file(metadata.target(), metadata.level()))
    }

    fn log(&self, record: &log::Record) {
        // `env_logger::Logger::log` re-checks its own filter, so a record that is only
        // interesting to the file never reaches stderr.
        self.stderr.log(record);
        // The target/level test comes FIRST and costs a prefix compare: `log!` dispatches
        // every record that clears `log::max_level()` WITHOUT consulting `enabled`, and with
        // the file on that level is `Debug` process-wide. Formatting before deciding meant
        // allocating and rendering every `naga`/`wgpu`/`zbus` debug record for the life of
        // the process — including the many thousands discarded after the budget was spent.
        if is_enabled() && wanted_in_file(record.target(), record.level()) {
            write_line(&format!(
                "{:<5} {}: {}",
                record.level(),
                record.target(),
                record.args()
            ));
        }
    }

    fn flush(&self) {
        self.stderr.flush();
    }
}

/// Install the logger and, when enabled, start this process's file session.
///
/// MUST be the first logging-related statement of `main`, before any subcommand returns and
/// before the resident-daemon branch, so that every way of starting this program is covered.
/// Everything it does is best-effort and failure-tolerant: an unresolvable directory, an
/// unopenable file, or a config that will not parse each degrade to "no file log", never to a
/// panic and never to a slower or broken startup.
pub fn init() {
    let stderr =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).build();
    let stderr_filter = stderr.filter();
    let _ = STDERR_FILTER.set(stderr_filter);
    let on = resolve_enabled();
    ENABLED.store(on, Ordering::Relaxed);
    let level = if on { stderr_filter.max(FILE_LEVEL.to_level_filter()) } else { stderr_filter };
    log::set_max_level(level);
    // A second `init` (only reachable from a test binary) must not panic the process.
    let _ = log::set_boxed_logger(Box::new(DiagLogger { stderr }));

    let args: Vec<String> = std::env::args().collect();
    COMPONENT.store(component_from_args(&args) as u8, Ordering::Relaxed);
    if !on {
        return;
    }
    prepare_file();
    session_header("process start");
    install_panic_hook();
}

/// Chain a panic hook that records the panic in the debug log before delegating.
///
/// The existing macOS/Windows hooks write a full backtrace to their own `panic.log` and are
/// left exactly as they are; this only ensures that a panic also appears IN THE ONE FILE, in
/// time order next to whatever the app was doing. It matters because a panic on a capture
/// worker thread is laundered into an ordinary "failed" boolean, and a panic on the UI thread
/// ends at `libc::_exit(1)` — both of which look like a clean silent exit from outside. Linux
/// has no `panic.log` at all, so on Linux this is the only record there is.
///
/// The panic MESSAGE is written; a backtrace is not. Panic messages here are our own literals
/// and formatted values, and the backtrace is what the platform `panic.log` already carries.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>").to_string();
        write_line(&format!("PANIC on thread `{name}`: {info}"));
        previous(info);
    }));
}

/// Create the folder and rotate a full file, once per process.
fn prepare_file() {
    let path = PATH.get_or_init(log_path).clone();
    let Some(path) = path else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    rotate_if_needed(&path);
}

/// The once-per-session environment block.
///
/// Everything here is about the app and the machine, never the user: version, OS, this
/// process's role and pid, its parent, and its REDACTED argv. `reason` says why the header was
/// written (process start, or the settings toggle being flipped mid-session).
fn session_header(reason: &str) {
    write_line(&format!(
        "=== cosmic-capture-kit {} | {} | debug log ({reason}) ===",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
    ));
    let args: Vec<String> = std::env::args().collect();
    write_line(&format!(
        "launch: component={} args={} parent_pid={}",
        component().tag(),
        redact_args(&args),
        parent_pid_display(),
    ));
    write_line(&format!(
        "build: profile={} features(zero-copy)={} target={}",
        if cfg!(debug_assertions) { "debug" } else { "release" },
        cfg!(feature = "zero-copy"),
        std::env::consts::ARCH,
    ));
    announce_sandbox();
}

/// Say, once per session and in both places a human might look, that this is NOT a user's log.
///
/// In the file, so a sandbox copy that reaches us by accident cannot be mistaken for a
/// customer's session. On stderr, because the only way to be under cargo is to have a terminal
/// — a developer who turned the log on and then cannot find it deserves the path rather than a
/// silent redirect. Both are skipped entirely for a real launch, which is every launch a
/// customer ever makes.
fn announce_sandbox() {
    if !is_dev_process() {
        return;
    }
    write_line(
        "dev/test process: this log is isolated from the user's log folder (DRAGON-424)",
    );
    if let Some(p) = PATH.get_or_init(log_path).as_ref() {
        eprintln!("cosmic-capture-kit: debug log isolated to {} (DRAGON-424)", p.display());
    }
}

/// The parent pid, so a daemon-spawned child can be tied to the daemon that spawned it. A
/// number of ours, not the user's data.
fn parent_pid_display() -> String {
    #[cfg(unix)]
    {
        // SAFETY: `getppid` takes no arguments, touches no memory and cannot fail.
        unsafe { libc::getppid() }.to_string()
    }
    #[cfg(not(unix))]
    {
        "n/a".to_string()
    }
}

// ── The file sink ────────────────────────────────────────────────────────────

/// Append ONE line to the shared file.
///
/// Open-append-write-close per line, exactly as the DRAGON-406 report does and for the same
/// reasons: an `O_APPEND` write of a single line is atomic on every filesystem we ship on, so
/// concurrent processes interleave BETWEEN lines and never split one, and holding no handle is
/// what lets the next process rotate the file out from under us. Every failure is silent — a
/// diagnostic must never be able to break the thing it is diagnosing.
fn write_line(message: &str) {
    let written = SESSION_BYTES.load(Ordering::Relaxed);
    if written >= MAX_SESSION_BYTES {
        return;
    }
    let Some(path) = PATH.get_or_init(log_path).as_ref() else { return };
    let stamp = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string();
    let line = log_line(&stamp, std::process::id(), component().tag(), message);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
    let n = line.len() as u64;
    let total = SESSION_BYTES.fetch_add(n, Ordering::Relaxed) + n;
    // The budget's own exhaustion is worth exactly one line, written before the gate closes.
    if written < MAX_SESSION_BYTES && total >= MAX_SESSION_BYTES {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = f.write_all(
                log_line(
                    &stamp,
                    std::process::id(),
                    component().tag(),
                    "per-process log budget reached — this process records nothing further",
                )
                .as_bytes(),
            );
        }
        return;
    }
    // Re-check rotation periodically, not just at process start: the resident daemon outlives
    // many rotations, and a bound that only a fresh process can enforce is not a bound.
    if SINCE_ROTATE_CHECK.fetch_add(n, Ordering::Relaxed) + n >= ROTATE_CHECK_BYTES {
        SINCE_ROTATE_CHECK.store(0, Ordering::Relaxed);
        rotate_if_needed(path);
    }
}

/// Rotate the live file to its single `.1` backup once it reaches [`MAX_LOG_BYTES`].
///
/// Best-effort: a failed rename just means this process keeps appending, and the per-process
/// budget still bounds what it can add. Skipped entirely when the path came from [`PATH_ENV`]
/// — a caller who named an exact file is running a controlled experiment and does not want us
/// renaming it.
///
/// The test is "this path IS the caller's choice", not merely "the variable is set": a
/// DRAGON-424 sandbox that took over from a refused override is ours to bound, and a test
/// suite run repeatedly would otherwise grow one temp file forever.
fn rotate_if_needed(path: &Path) {
    if explicit_override().is_some_and(|p| p == path) {
        return;
    }
    let Ok(meta) = std::fs::metadata(path) else { return };
    if !should_rotate(meta.len(), MAX_LOG_BYTES) {
        return;
    }
    let backup = path.with_file_name(LOG_BACKUP);
    let _ = std::fs::remove_file(&backup);
    let _ = std::fs::rename(path, &backup);
}

// ── Pure helpers (unit-tested; this is where the rules live) ──────────────────

/// Pure rotation policy: rotate when the file has reached the cap.
///
/// Split out so the bound is a tested fact rather than an inline comparison — the failure it
/// guards (a log too big to email, which is a log that does not get sent) is invisible until a
/// customer hits it.
pub fn should_rotate(current_size: u64, max_bytes: u64) -> bool {
    current_size >= max_bytes
}

/// One log line: `[<iso8601> pid=<pid> <component>] <message>\n`.
///
/// The pid and component are not decoration: the app, the resident daemon and every capture
/// child append to this one file, and without them a multi-process session is an unreadable
/// interleaving. The trailing newline is part of the returned string because the writer issues
/// exactly ONE append per line.
pub fn log_line(timestamp: &str, pid: u32, component: &str, message: &str) -> String {
    // A message containing a newline would break the one-append-one-line invariant that makes
    // concurrent writes safe to read, so fold any embedded newlines into a visible marker.
    let flat = if message.contains('\n') { message.replace('\n', " ⏎ ") } else { message.into() };
    format!("[{timestamp} pid={pid} {component}] {flat}\n")
}

/// Whether an argv token is safe to log verbatim: a flag (`-x` / `--flag`), or a short bare
/// word with no path shape. Everything else is a value that could be a file path, a save
/// target, or a window title.
fn arg_is_safe(token: &str) -> bool {
    if token.starts_with('-') {
        // `--flag=<path>` is split by the caller, so a `-`-led token with no separator left in
        // it is a flag and nothing else.
        return !token.contains(['/', '\\', ':']);
    }
    token.len() <= 24
        && !token.is_empty()
        && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Redact a process's argv for the log: flags survive, path-shaped values become `<value>`.
/// `argv[0]` is dropped entirely — it carries the user's install location and tells us nothing
/// the version line does not.
///
/// This is what lets the log answer "which launch path was this?" (hotkey child, tray child,
/// direct run, CLI) without carrying a single byte of the user's filesystem.
pub fn redact_args<S: AsRef<str>>(args: &[S]) -> String {
    let mut out: Vec<String> = Vec::new();
    for a in args.iter().skip(1) {
        let a = a.as_ref();
        match a.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => {
                let shown = if arg_is_safe(value) { value } else { "<value>" };
                out.push(format!("{flag}={shown}"));
            }
            _ => out.push(if arg_is_safe(a) { a.to_string() } else { "<value>".into() }),
        }
    }
    if out.is_empty() { "(none)".into() } else { out.join(" ") }
}

/// The SHAPE of a path, for the cases where a failure genuinely cannot be diagnosed without
/// knowing something about it — a save that failed, a capture folder that vanished.
///
/// Carries the extension (which names the format we were writing), the LENGTH of the filename
/// (a 260-character path is a real Windows failure mode), the directory depth, whether the
/// path and its parent exist, and whether the parent is writable. That is enough to separate
/// "the folder is gone", "the folder is read-only", "the name is too long" and "we never had a
/// name" — without a single character of the user's filesystem.
///
/// Never log a `Path` directly. This function is the only sanctioned way a path informs a log
/// line.
pub fn path_shape(path: &Path) -> String {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("none");
    // The extension is ours (png/jpg/mp4/…), but a user-chosen name could carry one we do not
    // recognise — and a bare extension can still be identifying, so cap it hard.
    let ext = if ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()) { ext } else { "?" };
    let name_len = path.file_name().map(|n| n.to_string_lossy().chars().count()).unwrap_or(0);
    let depth = path.components().count();
    let exists = path.exists();
    let parent = path.parent();
    let parent_exists = parent.map(|p| p.is_dir()).unwrap_or(false);
    let parent_writable = parent
        .map(|p| p.metadata().map(|m| !m.permissions().readonly()).unwrap_or(false))
        .unwrap_or(false);
    format!(
        "path(ext={ext}, name_len={name_len}, depth={depth}, exists={exists}, \
         dir_exists={parent_exists}, dir_writable={parent_writable})"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DRAGON-421. The file sink takes THIS crate at `debug` and dependencies only when they
    /// complain. Enabling the log raises `log::max_level()` to `Debug` process-wide, which
    /// switches on `log::debug!` in `naga`, `wgpu`, `zbus` and the wayland stack; on the
    /// owner's machine that spent the whole per-process budget on shader-lowering dumps
    /// within milliseconds of launch, so the file held no line about the capture, the
    /// recording, or the failure it was turned on to diagnose.
    #[test]
    fn the_file_sink_takes_our_debug_but_only_a_dependency_s_complaints() {
        // Ours, at every level the file accepts.
        for t in ["cosmic_capture_kit", "cosmic_capture_kit::record::pump"] {
            assert!(wanted_in_file(t, log::Level::Debug), "{t} debug");
            assert!(wanted_in_file(t, log::Level::Info), "{t} info");
            assert!(wanted_in_file(t, log::Level::Error), "{t} error");
            // Trace is reserved for per-frame firehoses and is never written.
            assert!(!wanted_in_file(t, log::Level::Trace), "{t} trace");
        }
        // The exact regression: the three loudest dependency debug emitters are dropped...
        for t in ["naga::front", "wgpu_core::device", "zbus::object_server", "naga"] {
            assert!(!wanted_in_file(t, log::Level::Debug), "{t} debug must not reach the file");
            assert!(!wanted_in_file(t, log::Level::Info), "{t} info must not reach the file");
        }
        // ...while what a dependency says when something is actually WRONG is kept — that is
        // the part that has ever named a cause.
        assert!(wanted_in_file("naga::front", log::Level::Warn));
        assert!(wanted_in_file("wgpu_hal", log::Level::Error));
        // A crate whose name merely STARTS with ours is a dependency, not us.
        assert!(!wanted_in_file("cosmic_capture_kit_helper::x", log::Level::Debug));
        assert!(is_own_target("cosmic_capture_kit"));
        assert!(!is_own_target("cosmic_capture_kit_helper"));
        // Our own custom targets survive the dependency rule — `timing` is emitted at
        // `debug` and IS the launch-hang answer, so dropping it would silently remove the
        // one feature this log gives for free.
        assert!(wanted_in_file("timing", log::Level::Debug), "timing marks must be kept");
        assert!(wanted_in_file("cck::failure", log::Level::Warn));
    }

    #[test]
    fn a_log_line_is_exactly_one_append() {
        let line = log_line("2026-07-29T10:11:12.345Z", 4242, "capture", "hello");
        assert_eq!(line, "[2026-07-29T10:11:12.345Z pid=4242 capture] hello\n");
        // Exactly one newline, at the end: the writer issues one atomic append per line, so
        // concurrent processes interleave BETWEEN lines and can never split one.
        assert_eq!(line.matches('\n').count(), 1);
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn an_embedded_newline_cannot_break_the_one_line_invariant() {
        // A `log::warn!("{e}")` on a multi-line error would otherwise emit two lines, the
        // second with no pid, and a concurrent process could land between them.
        let line = log_line("t", 1, "app", "ffmpeg said:\nno such device\nand gave up");
        assert_eq!(line.matches('\n').count(), 1);
        assert!(line.ends_with('\n'));
        assert!(line.contains("ffmpeg said: ⏎ no such device"));
    }

    #[test]
    fn rotation_fires_at_the_cap_and_not_before() {
        assert!(!should_rotate(0, MAX_LOG_BYTES));
        assert!(!should_rotate(MAX_LOG_BYTES - 1, MAX_LOG_BYTES));
        assert!(should_rotate(MAX_LOG_BYTES, MAX_LOG_BYTES));
        assert!(should_rotate(MAX_LOG_BYTES * 4, MAX_LOG_BYTES));
    }

    #[test]
    fn the_bound_stays_small_enough_to_email() {
        // Two files (live + `.1`), each at the cap plus the overshoot a generous eight
        // concurrent processes can add between rotation checks. If any of these constants is
        // raised, this is the assertion that has to be argued with.
        const WORST_CASE: u64 = 2 * (MAX_LOG_BYTES + 8 * ROTATE_CHECK_BYTES);
        const { assert!(WORST_CASE <= 5 * 1024 * 1024) };
        // One process can never fill the file on its own, so rotation always gets a turn.
        const { assert!(MAX_SESSION_BYTES < MAX_LOG_BYTES) };
        // And a process must be able to write several times between rotation checks, or the
        // check becomes a per-line `stat`.
        const { assert!(ROTATE_CHECK_BYTES < MAX_SESSION_BYTES) };
    }

    #[test]
    fn redaction_keeps_flags_and_drops_paths() {
        // argv[0] (the exe path) is dropped outright; flags and short bare words survive,
        // because "which launch path was this?" is exactly what the log must answer.
        let args = ["/Applications/CCK.app/Contents/MacOS/cck".to_string(), "resident".to_string()];
        assert_eq!(redact_args(&args), "resident");
        let args = ["cck".to_string(), "--region".to_string(), "--image".to_string()];
        assert_eq!(redact_args(&args), "--region --image");
        assert_eq!(redact_args(&["cck".to_string()]), "(none)");
        assert_eq!(redact_args::<String>(&[]), "(none)");
    }

    #[test]
    fn redaction_replaces_every_path_shaped_value() {
        // THE PRIVACY TEST for argv. Absolute paths, UNC paths and the `--flag=<path>` form
        // all collapse, on every platform's path syntax.
        let args = [
            "cck".to_string(),
            "--reveal".to_string(),
            "/Users/jane/Pictures/Q4 layoffs.png".to_string(),
            "--out=C:\\Users\\jane\\secret.mp4".to_string(),
            "--preview".to_string(),
            "\\\\server\\share\\board minutes.png".to_string(),
        ];
        let out = redact_args(&args);
        assert_eq!(out, "--reveal <value> --out=<value> --preview <value>");
        assert!(!out.contains("jane"));
        assert!(!out.contains("layoffs"));
        assert!(!out.contains("secret"));
        assert!(!out.contains("minutes"));
        // A long bare word is redacted too — it could be a filename with no separator in it.
        assert_eq!(redact_args(&["x".to_string(), "a".repeat(64)]), "<value>");
    }

    #[test]
    fn path_shape_carries_the_shape_and_never_the_value() {
        // THE PRIVACY TEST for paths. Everything a save failure needs, nothing a customer
        // would mind us seeing.
        let p = Path::new("/home/jane/Pictures/Q4 layoffs - final.png");
        let s = path_shape(p);
        assert!(s.contains("ext=png"), "{s}");
        assert!(s.contains("name_len=22"), "{s}");
        assert!(s.contains("exists=false"), "{s}");
        assert!(!s.contains("jane"), "{s}");
        assert!(!s.contains("layoffs"), "{s}");
        assert!(!s.contains("Pictures"), "{s}");
        // A directory with no extension, and a path with no name at all, must not panic or
        // invent anything.
        assert!(path_shape(Path::new("/tmp")).contains("ext=none"));
        assert!(path_shape(Path::new("/")).contains("name_len=0"));
    }

    #[test]
    fn path_shape_refuses_an_extension_that_could_identify() {
        // An "extension" that is really the tail of a user's filename (`report.v2.private`)
        // must not pass through. Only short alphanumeric extensions are shown.
        assert!(path_shape(Path::new("/x/report.confidential-draft")).contains("ext=?"));
        assert!(path_shape(Path::new("/x/a.mp4")).contains("ext=mp4"));
    }

    #[test]
    fn path_shape_reports_a_real_directory_truthfully() {
        // The one case that has to be right for a save failure to be diagnosable: a file in a
        // directory that DOES exist and IS writable reads differently from one whose folder
        // has been deleted.
        let dir = std::env::temp_dir();
        let s = path_shape(&dir.join("cck-diag-test.png"));
        assert!(s.contains("exists=false"), "{s}");
        assert!(s.contains("dir_exists=true"), "{s}");
        let gone = path_shape(Path::new("/definitely/not/here/x.png"));
        assert!(gone.contains("dir_exists=false"), "{gone}");
    }

    #[test]
    fn every_failure_has_a_distinct_stable_code() {
        // Codes are the vocabulary a support answer is written against, so a duplicate would
        // silently merge two different diagnoses.
        const ALL: &[Failure] = &[
            Failure::NoOutputs,
            Failure::PermissionDenied,
            Failure::SceneGrabTimeout,
            Failure::NoImage,
            Failure::SaveFailed,
            Failure::WorkerPanic,
            Failure::DecodeFailed,
            Failure::PreviewSurfaceLost,
            Failure::NoPreviewOutput,
            Failure::HandoffAccepted,
            Failure::RecordingFailed,
            Failure::RecordingWedged,
            Failure::Cancelled,
        ];
        let mut seen: Vec<&str> = ALL.iter().map(|f| f.code()).collect();
        let total = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), total, "two failures share a code");
        for f in ALL {
            let c = f.code();
            assert!(!c.is_empty());
            assert!(
                c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '-'),
                "code `{c}` is not lowercase-hyphenated"
            );
        }
    }

    #[test]
    fn the_two_non_defect_endings_are_not_losses() {
        // A cancel and an accepted handoff look exactly like a silent failure from outside;
        // telling them apart is the first thing a reader of a "nothing happened" report does.
        assert!(!Failure::Cancelled.is_loss());
        assert!(!Failure::HandoffAccepted.is_loss());
        assert!(Failure::NoImage.is_loss());
        assert!(Failure::SaveFailed.is_loss());
        assert!(Failure::DecodeFailed.is_loss());
    }

    #[test]
    fn reached_disk_splits_capture_failures_from_delivery_failures() {
        // This is the split that halves the search space in a "nothing happened" report: no
        // file in the capture folder means the capture died; a file with no editor means
        // delivery did. The fixes have nothing in common.
        for f in [Failure::NoImage, Failure::SaveFailed, Failure::WorkerPanic, Failure::NoOutputs] {
            assert!(!f.reached_disk(), "{f:?} must not claim the bytes reached disk");
        }
        for f in [Failure::DecodeFailed, Failure::PreviewSurfaceLost, Failure::NoPreviewOutput] {
            assert!(f.reached_disk(), "{f:?} happens AFTER the file is written");
        }
    }

    #[test]
    fn components_are_classified_from_argv() {
        let c = |v: &[&str]| component_from_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        // A bare launch is `app` until the resident branch re-stamps it.
        assert_eq!(c(&["cck"]), Component::App);
        assert_eq!(c(&["cck", "resident"]), Component::App);
        assert_eq!(c(&["cck", "--region"]), Component::Capture);
        assert_eq!(c(&["cck", "--video", "--monitor"]), Component::Capture);
        assert_eq!(c(&["cck", "--settings"]), Component::Settings);
        assert_eq!(c(&["cck", "--preview", "/x/y.png"]), Component::Preview);
        assert_eq!(c(&["cck", "--permissions"]), Component::Permissions);
        assert_eq!(c(&["cck", "--copy-image", "/x/y.png"]), Component::Helper);
        assert_eq!(c(&["cck", "--test", "mac-shot"]), Component::Cli);
    }

    #[test]
    fn a_helper_child_is_never_mistaken_for_a_capture() {
        // The reveal/notify helpers are spawned BY a capture and carry a path argument. If
        // they were tagged `capture` a reader would see two capture sessions per capture, and
        // the one that matters would be ambiguous.
        let c = |v: &[&str]| component_from_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(c(&["cck", "--notify-saved", "/x/y.png"]), Component::Helper);
        assert_eq!(c(&["cck", "--reveal", "/x/y.png"]), Component::Helper);
        // argv[0] is never inspected: an exe path that happens to contain a flag-like word
        // must not classify the process.
        assert_eq!(c(&["/opt/--region/cck"]), Component::App);
    }

    /// DRAGON-424. The rule, in one table: a real launch is unchanged; a dev/test process can
    /// never resolve the user's file.
    #[test]
    fn a_dev_process_can_never_resolve_the_user_s_log_file() {
        let user = PathBuf::from("/home/jane/.local/state/cosmic-capture-kit/logs");
        let sandbox = Path::new("/tmp/cosmic-capture-kit-dev-logs-0123456789abcdef");
        let expected_sandbox = sandbox.join(LOG_NAME);
        let r = |dev, explicit: Option<&str>| {
            resolve_path(dev, explicit.map(PathBuf::from), Some(user.clone()), sandbox)
        };

        // A real launch: exactly what it was before this ticket.
        assert_eq!(r(false, None), Some(user.join(LOG_NAME)));
        assert_eq!(r(false, Some("/tmp/mine.log")), Some(PathBuf::from("/tmp/mine.log")));

        // A dev/test process with no override goes to the sandbox — THE regression. This is
        // the case that was appending a debug-profile test binary's lines into the owner's
        // live release log while that log was being read to diagnose a wedge.
        assert_eq!(r(true, None), Some(expected_sandbox.clone()));
        // It may still name an exact file: that is how a harness redirects itself.
        assert_eq!(r(true, Some("/tmp/harness.log")), Some(PathBuf::from("/tmp/harness.log")));
        // But not one inside the user's folder, however it is spelled — otherwise the
        // override is just a way to re-open the leak by accident.
        assert_eq!(
            r(true, Some("/home/jane/.local/state/cosmic-capture-kit/logs/debug.log")),
            Some(expected_sandbox.clone())
        );
        assert_eq!(
            r(true, Some("/home/jane/.local/state/cosmic-capture-kit/logs/sub/other.log")),
            Some(expected_sandbox.clone())
        );
        // And a machine with no resolvable user folder still gets a sandbox, never `None`:
        // "we could not find your folder" must not become "write wherever the caller said".
        assert_eq!(resolve_path(true, None, None, sandbox), Some(expected_sandbox));
    }

    /// The harness and the child it spawns are two different binaries that have to agree on
    /// the sandbox folder, or a test's own child writes somewhere the test cannot see.
    #[test]
    fn the_sandbox_key_is_stable_and_per_worktree() {
        assert_eq!(fnv1a_hex("/home/jane/repo"), fnv1a_hex("/home/jane/repo"));
        assert_ne!(fnv1a_hex("/home/jane/repo"), fnv1a_hex("/home/jane/worktrees/other"));
        assert_eq!(fnv1a_hex("x").len(), 16);
        assert!(fnv1a_hex("x").chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The live assertion, made by the harness about ITSELF: this very process is a dev
    /// process, and the path it would write to is not the user's.
    #[test]
    fn this_test_binary_resolves_away_from_the_user_s_log() {
        assert!(is_dev_process(), "a cargo test binary must classify itself as dev/test");
        // A run that named its own file is a controlled experiment; the invariant under test
        // is where we land when nobody said.
        if explicit_override().is_some() {
            return;
        }
        let resolved = log_path().expect("a dev process always has a sandbox to fall back on");
        if let Some(user) = user_log_dir() {
            assert!(!resolved.starts_with(&user), "resolved {resolved:?} inside {user:?}");
        }
        assert!(
            resolved.starts_with(std::env::temp_dir()),
            "the sandbox lives under the temp dir, not in the user's data: {resolved:?}"
        );
    }

    #[test]
    fn the_module_doc_states_the_privacy_rule() {
        // The rule has to be findable by the next person adding a line, which means it lives
        // in the doc comment and not in a ticket. Pin the phrasing so a refactor cannot
        // quietly drop it.
        let doc = include_str!("diag.rs");
        assert!(
            doc.contains("Log the app's own BEHAVIOUR, not the user's CONTENT"),
            "the privacy rule must stay stated in the module doc"
        );
    }
}
