//! Small cross-cutting helpers shared across the crate: the session runtime directory,
//! `~/` expansion, and `file://` URI building. Centralised so the fallback and format
//! live in one place instead of being re-derived at every call site.

use std::path::{Path, PathBuf};

/// Startup-latency instrumentation. Prints `[+<ms since process start>] <label>` to stderr
/// under `CCK_TIMING`, and — since DRAGON-419 — also records the same mark in the debug log
/// whenever that is enabled. The process-start `Instant` is captured lazily on first use.
pub fn timing_start() -> std::time::Instant {
    *TIMING_T0.get_or_init(std::time::Instant::now)
}
static TIMING_T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// `CCK_TIMING`, read once. It used to be a `var_os` call on EVERY mark — a getenv plus an
/// allocation on a path that runs dozens of times during startup, for a feature that is off.
static TIMING_STDERR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Record a startup-latency mark.
///
/// DRAGON-419 folded this INTO the debug log rather than removing it. The marks blanket
/// `main` → `App::init` → the permission preflight → `acquire_scene` → `seed_overlays` →
/// `configure_overlay`, so on a launch that HANGS — the DRAGON-414 report — the last mark in
/// the file names the call that never returned. That is the whole answer to a launch-hang
/// report, and it was already written; it was just going to a stderr nobody has.
///
/// Cheap when both are off: one `OnceLock` read and one relaxed atomic. The `label` is a
/// literal at nearly every call site, so nothing is formatted; the few marks that carry a
/// measured number ask [`timing_on`] first so they don't build a string for a disabled log.
#[inline]
pub fn timing_mark(label: &str) {
    let to_stderr = *TIMING_STDERR.get_or_init(|| std::env::var_os("CCK_TIMING").is_some());
    if !to_stderr && !crate::diag::is_enabled() {
        return;
    }
    let t0 = *TIMING_T0.get_or_init(std::time::Instant::now);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    if to_stderr {
        eprintln!("[+{ms:>7.1}ms] {label}");
    }
    // Its own target so a reader can isolate the launch timeline with one filter, and so it
    // never looks like an application warning.
    log::debug!(target: "timing", "[+{ms:.1}ms] {label}");
}

/// Whether [`timing_mark`] would record anything right now.
///
/// The gate for a mark whose LABEL costs something to build. Most marks pass a literal, but
/// the preview-open timeline (DRAGON-454) carries measured values — the decoded pixel count,
/// the view-build index — and formatting those on a launch with the debug log off would be
/// work for nobody. Same two reads `timing_mark` makes, so asking first is free.
#[inline]
pub fn timing_on() -> bool {
    *TIMING_STDERR.get_or_init(|| std::env::var_os("CCK_TIMING").is_some())
        || crate::diag::is_enabled()
}

// ── The dev/test sandbox key (DRAGON-424, shared by DRAGON-433) ──────────────

/// Cargo sets this for anything IT runs, and a child process inherits it. Absent from every
/// shipped launch — a `.desktop` entry, a Finder/Start Menu launch, a login item and the
/// user's PrintScreen shortcut all start from a session that never had it, and `cargo` does
/// not export anything into a login shell.
///
/// That inheritance is the whole reason this is the marker. See [`is_dev_process`].
const CARGO_ENV: &str = "CARGO_MANIFEST_DIR";

/// Whether this process is part of a build/test run rather than a real user session.
///
/// **Why it lives here.** Introduced by DRAGON-424 inside `diag` to keep test output out of
/// the owner's debug log; DRAGON-433 needed the identical question for the CONFIG directory,
/// and "is this process a dev/test run" is a fact about the process, not about logging. One
/// definition, so the two sandboxes can never disagree about what a test process is.
///
/// **Why not `cfg!(test)` alone.** The observed leak was NOT the test harness. It was
/// `tests/cli.rs` spawning the REAL binary as a subprocess — a child built without
/// `cfg(test)`, which resolves its own paths and knows nothing about the harness that started
/// it. Any check that lives only in test-compiled code misses the case that actually happened.
///
/// **Why not "the binary sits under `target/`".** The owner's PrintScreen shortcut launches
/// `target/release/cosmic-capture-kit` directly. Treating a target-dir binary as a test would
/// sandbox the person the app is for.
///
/// So the test is the inherited cargo environment, present for the harness AND its children
/// and absent for every shipped launch. `cfg!(test)` is kept as a cheap second condition for
/// the harness itself.
pub(crate) fn is_dev_process() -> bool {
    cfg!(test) || std::env::var_os(CARGO_ENV).is_some()
}

/// A short, deterministic key that separates one checkout's sandbox from another's.
///
/// Keyed by the cargo manifest dir so concurrent runs in different worktrees never share a
/// sandbox (and so two users on one machine never collide over a `/tmp` folder the other
/// created), but STABLE within a run — a test's spawned children inherit `CARGO_MANIFEST_DIR`
/// and so resolve to the same sandbox as the harness, which is what makes the isolation hold
/// across process boundaries.
pub(crate) fn sandbox_key() -> String {
    let seed = std::env::var_os(CARGO_ENV)
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cargo".to_string());
    fnv1a_hex(&seed)
}

/// FNV-1a as 16 hex digits. Pure, so [`sandbox_key`]'s "two binaries agree" property is
/// testable without spawning anything.
///
/// Written out rather than taken from `DefaultHasher` because the parent harness and the
/// child app are two different binaries that must agree on the answer, and std's hasher only
/// promises stability within one build.
fn fnv1a_hex(seed: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in seed.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The config directory a dev/test process gets instead of the user's real one (DRAGON-433).
///
/// Under the temp dir, for the same reason the log sandbox is: a test run leaves nothing
/// behind in a place the user (or a support request) would later look. Starts EMPTY, which is
/// the point — a test sees schema defaults, not whatever the developer happens to have set.
fn sandbox_config_dir() -> PathBuf {
    std::env::temp_dir().join(format!("cosmic-capture-kit-dev-config-{}", sandbox_key()))
}

/// The app's OWN config directory — `~/.config/cosmic-capture-kit` on EVERY OS, so
/// the user-facing paths (the covermark drop folder, `config.toml`) read the same
/// everywhere. Linux resolves through `dirs::config_dir()` (XDG-respecting —
/// byte-identical to the historical location); macOS/Windows pin to `~/.config`
/// instead of their native config homes (`~/Library/Application Support` /
/// `%APPDATA%`), MIGRATING anything an older build left in the native location
/// (a one-time whole-directory rename, checked once per process).
///
/// **DRAGON-433: a dev/test process gets a sandbox instead** ([`sandbox_config_dir`]), and
/// the real directory is unreachable from one. THE choke point for that rule, deliberately:
/// every config-adjacent path in the app derives from this one function — `config.toml` load
/// AND save (`state::store::config_path`), `diag::init`'s early direct read of the same file,
/// the update manifest cache (`update.rs`), and the covermark drop folder
/// (`preview::edit`). Sandboxing here means a test can neither READ the developer's real
/// settings nor WRITE over them, with no second mechanism to keep in step.
///
/// See [`is_dev_process`] for why the marker is the inherited cargo environment rather than
/// `cfg!(test)` — the same reasoning, and the same predicate, as the DRAGON-424 log sandbox.
pub fn app_config_dir() -> Option<PathBuf> {
    if is_dev_process() {
        return Some(sandbox_config_dir());
    }
    #[cfg(target_os = "linux")]
    {
        Some(dirs::config_dir()?.join("cosmic-capture-kit"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let new = dirs::home_dir()?.join(".config").join("cosmic-capture-kit");
        static MIGRATED: std::sync::Once = std::sync::Once::new();
        MIGRATED.call_once(|| {
            if let Some(old) = dirs::config_dir().map(|d| d.join("cosmic-capture-kit")) {
                migrate_app_config_dir(&old, &new);
            }
        });
        Some(new)
    }
}

/// Move an older build's app config dir from the OS-native location into the
/// uniform `~/.config` one. Strictly one-shot and conservative: only when the old
/// dir exists and the new one does NOT (never merges, never overwrites — if both
/// exist the new location simply wins and the old is left untouched). Both live
/// under `$HOME`, so this is a same-volume rename.
#[cfg(not(target_os = "linux"))]
fn migrate_app_config_dir(old: &Path, new: &Path) {
    if !old.is_dir() || new.exists() {
        return;
    }
    if let Some(parent) = new.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::rename(old, new) {
        Ok(()) => log::info!(
            "migrated the app config dir {} -> {}",
            old.display(),
            new.display()
        ),
        Err(e) => log::warn!(
            "could not migrate the app config dir {} -> {}: {e}",
            old.display(),
            new.display()
        ),
    }
}

/// The session runtime directory (`$XDG_RUNTIME_DIR`), falling back to the OS temp
/// dir (`/tmp` on Linux — identical to the old fallback; a real path on mac/win,
/// where XDG vars don't exist). Home of the single-instance locks, the per-channel
/// level-meter files, and the clean-mic FIFO.
pub fn runtime_dir() -> String {
    std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned())
}

/// How long an unclaimed transient RECORDING is kept before the startup sweep removes it
/// (DRAGON-467 review, major 3).
///
/// Seven days is chosen to be forgiving rather than tidy: the file is the user's capture, and
/// the only thing that makes it disposable is that they closed the editor without saving it.
/// A week means "I meant to keep that" is still recoverable from the cache folder, while the
/// folder still cannot grow without bound.
pub const TRANSIENT_MAX_AGE: std::time::Duration =
    std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// The DISK-BACKED transient directory for recordings that the user has not saved
/// (`~/.cache/cosmic-capture-kit/transient`), created on demand. `None` when the OS has no
/// cache dir to offer.
///
/// # Why recordings do not use [`runtime_dir`] (DRAGON-467 review, major 3)
///
/// `$XDG_RUNTIME_DIR` is a **tmpfs**, so it is RAM, and it is small: the systemd default is
/// 10% of physical memory. A recording buffers its live `.recording` temp AND its finished
/// file there, so a long take with "Automatically save originals" off could fill it and
/// ENOSPC mid-capture, losing the take it was in the middle of. `/tmp` is no answer either:
/// on this project's own distro (Arch) it is tmpfs as well.
///
/// Stills stay in the runtime dir on purpose. They are a few MB, they are written once, and
/// the runtime dir's clear-at-logout lifetime is exactly right for them.
///
/// The Linux clipboard constraint is what stops this being a temp file we delete on close: a
/// copied recording is served to the desktop as a `file://` URI by a DETACHED worker, so the
/// file must outlive this process for a paste to work at all. Bounded accumulation is handled
/// by age instead, in [`sweep_transient_recordings`].
pub fn transient_recording_dir() -> Option<PathBuf> {
    let dir = if is_dev_process() {
        // The same sandbox rule as `app_config_dir`: nothing a cargo run does may litter the
        // developer's real cache folder.
        PathBuf::from(runtime_dir()).join("cck-dev-transient")
    } else {
        dirs::cache_dir()?.join("cosmic-capture-kit").join("transient")
    };
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Remove transient recordings older than [`TRANSIENT_MAX_AGE`] (DRAGON-467 review, major 3).
///
/// Called once per launch, off the UI thread, and deliberately forgiving: every step is
/// best-effort, an unreadable entry is skipped rather than reported, and a file whose age
/// cannot be determined is KEPT. Deleting a capture is the destructive direction, so anything
/// uncertain resolves toward keeping it.
///
/// Only plain files directly in the folder are considered, so a stray directory (or a
/// recording's sibling `.recording` temp, which shares the same age) is handled by the same
/// rule without special cases.
pub fn sweep_transient_recordings() {
    let Some(dir) = transient_recording_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let now = std::time::SystemTime::now();
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        // `modified` is unavailable on a few filesystems; an unknown age keeps the file.
        let Ok(modified) = meta.modified() else { continue };
        let Ok(age) = now.duration_since(modified) else { continue };
        if age > TRANSIENT_MAX_AGE && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        log::debug!("swept {removed} unsaved transient recording(s) older than a week");
    }
}

// `is_cosmic()` moved into the COSMIC desktop profile
// (`platform::linux::cosmic`) with the rest of the per-desktop COSMIC config
// knowledge (DRAGON-220).

/// Locate the `ffmpeg` binary: explicit override (`CCK_FFMPEG`) → on macOS, the `.app`
/// bundle's `Resources/` sidecar and the checked-out dev vendor dir → a bundled sidecar
/// next to our own executable (how the mac/win packages will ship it) → bare name
/// (the OS resolves it on PATH — the Linux packaging model).
pub fn ffmpeg_path() -> PathBuf {
    locate_tool("ffmpeg", "CCK_FFMPEG")
}

/// Locate the `ffprobe` binary (same resolution order as [`ffmpeg_path`]).
pub fn ffprobe_path() -> PathBuf {
    locate_tool("ffprobe", "CCK_FFPROBE")
}

/// Locate the `ffplay` binary (`CCK_FFPLAY` override → exe-adjacent sidecar → PATH, the
/// same resolution order as [`ffmpeg_path`]). Windows-only: the preview soundtrack renders
/// through ffplay (SDL2 → default output endpoint) because the bundled Windows ffmpeg has
/// no pulse muxer and no audio-output device at all, so it can't be the sink (DRAGON-285).
/// Linux/macOS use `ffmpeg -f pulse`/`-f audiotoolbox` and never call this.
#[cfg(windows)]
pub fn ffplay_path() -> PathBuf {
    locate_tool("ffplay", "CCK_FFPLAY")
}

/// Locate the `proton-drive` binary, Proton's own official Drive command-line tool
/// (DRAGON-485), with the same resolution order as [`ffmpeg_path`]: the `CCK_PROTON_DRIVE`
/// override, then a sidecar next to our executable, then `PATH`.
///
/// **Optional at runtime and never bundled**, exactly like `ffmpeg` and `tesseract`. It is a
/// ~118 MB standalone binary the user downloads from Proton, and shipping a copy of someone
/// else's signed application inside ours would be both enormous and wrong. The Proton provider
/// probes for it and, when it is absent, says so and offers the download page rather than
/// failing a connect halfway through. See `cloud::proton`.
pub fn proton_drive_path() -> PathBuf {
    locate_tool("proton-drive", "CCK_PROTON_DRIVE")
}

/// Build a [`std::process::Command`] that never pops a console window on Windows
/// (DRAGON-236). A GUI-subsystem process (DRAGON-233) that spawns a CONSOLE-subsystem
/// child — ffmpeg, ffprobe, tesseract, curl, netsh, cmd, … — makes Windows allocate a
/// console for that child; it flashes for a tick, and (when the machine's default
/// terminal is Windows Terminal) a child that is later KILLED leaves a permanent blank
/// pane behind. `CREATE_NO_WINDOW` (`0x0800_0000`) means no console is ever created for
/// the child — killed or not. Piped / redirected / null stdio is UNAFFECTED by the flag,
/// so every `.output()` / `.status()` / piped `.spawn()` keeps working unchanged. Off
/// Windows this is a plain `Command::new` — Linux/macOS behaviour is byte-identical
/// (portable glue, like `EXE_SUFFIX`). Route every runtime tool spawn through this (or
/// the [`ffmpeg_command`] / [`ffprobe_command`] wrappers); our own re-exec of this GUI
/// binary needs nothing (it is already GUI-subsystem).
pub fn quiet_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    #[allow(unused_mut)]
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW: the child gets no console, so it never flashes a window and
        // (being window-less) can never leave a Windows Terminal pane when killed.
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// Spawn `cmd` TETHERED to this process: the child cannot outlive us, however we die.
///
/// **Scope, because this has been read as more than it says (DRAGON-423):** the tether is
/// triggered by our DEATH and by nothing else. It says nothing about a child whose parent is
/// alive and stuck — while we are hung it is inert, and the child hangs on with us. Bounding
/// a session that stops making progress is a separate mechanism at a separate level
/// ([`crate::record::progress`], whose teardown is `record::recover::abandon_session`).
///
/// DRAGON-421. The recording muxer is the child this exists for. When our process dies
/// mid-recording, that ffmpeg is parked in `open()` on an audio FIFO whose only writer was
/// the process that just died — it never reaches the video pipe, so it never sees the EOF
/// our death delivered, and it sits there holding the recording temp until someone finds
/// and kills it. Every bound we have runs INSIDE the process that died, so no amount of
/// teardown code can help: the case that matters is exactly the case where our code does
/// not get to run. The kernel has to enforce it.
///
/// Per platform:
///
/// * **Linux** — `prctl(PR_SET_PDEATHSIG, SIGKILL)` in the child, between `fork` and
///   `exec`. The kernel kills it when its parent goes, whatever took the parent down
///   (SIGKILL, a panic, an OOM kill). The classic race — the parent dying in the window
///   before `prctl` runs, so the signal is missed — is closed by re-reading `getppid()`
///   immediately after and refusing to `exec` if it already changed.
/// * **Windows** — a per-process JOB OBJECT with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
///   (`crate::platform::windows::job`). Every tethered child is assigned to it; when our
///   last handle to the job closes — which process teardown does unconditionally, killed or
///   not — the kernel terminates everything still in it. Same guarantee as Linux's, and
///   without the thread caveat below.
/// * **macOS** — there is NO kernel equivalent (no `PDEATHSIG`, no job objects; `kqueue`'s
///   `NOTE_EXIT` needs a live watcher, which is the very thing we do not have). Rather than
///   ship a babysitter process for one child, macOS relies on the ledger + sweep in
///   [`crate::record::recover`]: the orphan is reaped at the start of the next recording,
///   so it never poisons a later take. That is genuinely weaker — the orphan exists in the
///   meantime — and it is recorded here rather than glossed over.
///
/// **Linux caveat, and the rule it implies:** `PR_SET_PDEATHSIG` fires when the parent
/// THREAD that forked exits, not when the parent process does. So only spawn through this
/// from a thread that owns the child for the child's whole life — which the recording
/// workers do by construction (the worker thread spawns its muxer, feeds it, and reaps it
/// in `run_video_stop_tail` before returning). Do not route a child through here if it is
/// handed off to another thread to outlive its spawner.
pub fn spawn_tethered(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt as _;
        let parent = std::process::id();
        // SAFETY: the closure runs in the forked child before `exec`, where only
        // async-signal-safe work is allowed. Both calls are bare syscalls through rustix
        // (`prctl` and `getppid`) — no allocation, no locks, no libc state.
        unsafe {
            cmd.pre_exec(move || {
                let _ = rustix::process::set_parent_process_death_signal(Some(
                    rustix::process::Signal::KILL,
                ));
                // If the parent died in the fork→prctl window the signal was already
                // missed; refuse to exec rather than become the orphan we are preventing.
                let still_ours = rustix::process::getppid()
                    .is_some_and(|p| p.as_raw_nonzero().get() as u32 == parent);
                if !still_ours {
                    return Err(std::io::Error::other(
                        "parent exited before the child could be tethered to it",
                    ));
                }
                Ok(())
            });
        }
    }
    let child = cmd.spawn()?;
    #[cfg(windows)]
    crate::platform::windows::job::tether(&child);
    Ok(child)
}

/// [`quiet_command`] for the resolved ffmpeg binary — the console-free spawn seam for
/// every runtime ffmpeg invocation (DRAGON-236).
pub fn ffmpeg_command() -> std::process::Command {
    quiet_command(ffmpeg_path())
}

/// [`quiet_command`] for the resolved ffprobe binary (DRAGON-236).
pub fn ffprobe_command() -> std::process::Command {
    quiet_command(ffprobe_path())
}

fn locate_tool(name: &str, env_override: &str) -> PathBuf {
    // An explicit override is taken verbatim (user intent, even if the file is
    // missing — a broken override should fail loudly, not silently fall back).
    if let Ok(p) = std::env::var(env_override)
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    // Build the ordered candidate list, then take the first that exists on disk (else
    // the bare name for a PATH lookup). Precedence lives in `first_existing_or_bare`
    // so it is unit-testable without a real executable or `.app` bundle.
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_deref().and_then(Path::parent);
    let mut candidates: Vec<PathBuf> = Vec::new();
    // macOS: prefer a bundled/vendored ffmpeg over PATH (the mac package has no
    // system ffmpeg to rely on), inserted AFTER the env override and BEFORE the
    // generic exe-adjacent sidecar.
    #[cfg(target_os = "macos")]
    {
        // The `.app` bundle's Resources sidecar: Contents/MacOS/../Resources/{name} —
        // where the packaged mac build ships ffmpeg.
        if let Some(dir) = exe_dir {
            candidates.push(dir.join("..").join("Resources").join(name));
        }
        // Dev vendor dir: the checked-out repo's vendored static arm64 ffmpeg.
        // `CARGO_MANIFEST_DIR` is baked at compile time — it points at the repo for a
        // dev build and simply doesn't exist in a shipped bundle (harmless fall-through).
        candidates.push(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("vendor/ffmpeg/macos-arm64")
                .join(name),
        );
    }
    // A sidecar shipped next to the app binary wins over PATH.
    if let Some(dir) = exe_dir {
        candidates.push(dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)));
    }
    first_existing_or_bare(name, &candidates)
}

/// The first candidate that exists as a file, else the bare tool `name` (which the OS
/// then resolves on `PATH`). Pure over its inputs so the sidecar/bundle/vendor
/// precedence is unit-testable without a real executable or `.app` bundle.
fn first_existing_or_bare(name: &str, candidates: &[PathBuf]) -> PathBuf {
    for c in candidates {
        if c.is_file() {
            return c.clone();
        }
    }
    PathBuf::from(name)
}

/// Whether a tool resolved by [`ffmpeg_path`]-style lookup is actually runnable:
/// an absolute path must exist; a bare name is scanned for on PATH.
pub fn tool_available(tool: &Path) -> bool {
    if tool.is_absolute() {
        return tool.is_file();
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| dir_has_tool(&d, tool)))
        .unwrap_or(false)
}

/// Whether directory `d` contains the bare tool `tool` as a file. On Windows the on-disk
/// file carries `EXE_SUFFIX` (`ffmpeg.exe`) even though `Command::new("ffmpeg")` resolves it
/// without — so the bare `d/ffmpeg` never exists and a PATH-only ffmpeg would wrongly read as
/// "missing" (gating recording). Also probe `d/ffmpeg{EXE_SUFFIX}` to match the runtime spawn
/// (DRAGON-229). On Linux/macOS `EXE_SUFFIX` is empty, so this is byte-identical to the plain
/// `d.join(tool).is_file()`.
fn dir_has_tool(d: &Path, tool: &Path) -> bool {
    if d.join(tool).is_file() {
        return true;
    }
    let suffix = std::env::consts::EXE_SUFFIX;
    if !suffix.is_empty() {
        let mut name = tool.as_os_str().to_os_string();
        name.push(suffix);
        return d.join(name).is_file();
    }
    false
}

/// Expand a leading `~/` to the user's home directory; every other path passes through.
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

/// A `file://` URI for a local path (clipboard file references + opening folders).
pub fn path_to_file_uri(path: impl AsRef<Path>) -> String {
    format!("file://{}", path.as_ref().display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_resolves_home_prefix() {
        let home = dirs::home_dir().expect("home dir available in the test env");
        assert_eq!(expand_tilde("~/Capture"), home.join("Capture"));
    }

    #[test]
    fn expand_tilde_passes_through_absolute_and_relative() {
        assert_eq!(expand_tilde("/etc/hosts"), PathBuf::from("/etc/hosts"));
        assert_eq!(expand_tilde("rel/path"), PathBuf::from("rel/path"));
        // Only the "~/" prefix expands; a bare "~" stays literal.
        assert_eq!(expand_tilde("~weird"), PathBuf::from("~weird"));
    }

    #[test]
    fn file_uri_prefixes_scheme() {
        assert_eq!(path_to_file_uri(Path::new("/a/b c")), "file:///a/b c");
    }

    // ── Quiet-spawn seam (DRAGON-236) ─────────────────────────────────────────

    #[test]
    fn quiet_command_sets_the_program() {
        // The seam is a thin constructor: same program, ready to chain args/stdio. The
        // Windows CREATE_NO_WINDOW flag is invisible to `get_program` but proven by the
        // Windows-only spawn test below.
        assert_eq!(
            quiet_command("cck-quiet-probe").get_program(),
            std::ffi::OsStr::new("cck-quiet-probe")
        );
    }

    #[test]
    fn ffmpeg_and_ffprobe_commands_target_the_resolved_binaries() {
        assert_eq!(ffmpeg_command().get_program(), ffmpeg_path().as_os_str());
        assert_eq!(ffprobe_command().get_program(), ffprobe_path().as_os_str());
    }

    // ── The child tether (DRAGON-421) ─────────────────────────────────────────
    //
    // The whole promise is "if the parent dies, the child dies", and the case that
    // matters is the one where none of our teardown code runs — so the proof has to
    // SIGKILL a real parent and watch a real child go with it. That needs three
    // processes, so this test re-invokes the test binary at the `#[ignore]`d helper
    // below as the middle one. Linux-only: it is the platform whose guarantee is
    // `PR_SET_PDEATHSIG`. Windows' job object is the same promise by a different
    // mechanism and is proven on Windows; macOS deliberately makes no such promise
    // (see `spawn_tethered`).

    /// The MIDDLE process: spawn a tethered child, publish its pid, then block forever.
    /// Never runs in an ordinary suite run (`#[ignore]`) — it is a fixture, not a test.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "fixture process for a_tethered_child_dies_when_its_parent_is_sigkilled"]
    fn tether_fixture_parent() {
        let pidfile = std::env::var("CCK_TETHER_PIDFILE").expect("fixture needs its pidfile");
        let mut cmd = quiet_command("tail");
        cmd.args(["-f", "/dev/null"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = spawn_tethered(&mut cmd).expect("tethered spawn");
        std::fs::write(&pidfile, child.id().to_string()).expect("publish the child pid");
        // Hold the process (and this thread — `PR_SET_PDEATHSIG` is thread-scoped) open
        // until the test SIGKILLs us. Far longer than the test's own budgets, so a
        // timeout can only ever mean the tether failed, never that the fixture gave up.
        std::mem::forget(child);
        std::thread::sleep(std::time::Duration::from_secs(120));
    }

    /// The MIDDLE process for the ffmpeg reproduction below: start a REAL recording muxer
    /// against FIFOs nobody will ever write to — the exact wedge from the field report —
    /// then block until the test SIGKILLs us.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "fixture process for a_wedged_ffmpeg_cannot_outlive_a_sigkilled_recorder"]
    fn tether_fixture_wedged_ffmpeg() {
        let pidfile = std::env::var("CCK_TETHER_PIDFILE").expect("fixture needs its pidfile");
        let mic = std::env::var("CCK_TETHER_MIC").expect("fixture needs its mic fifo");
        let sys = std::env::var("CCK_TETHER_SYS").expect("fixture needs its sys fifo");
        let temp = std::env::var("CCK_TETHER_TEMP").expect("fixture needs its temp");
        // The real recording command, built exactly as a worker builds it. Nothing will
        // ever open the write end of either FIFO, so ffmpeg parks in `open()` — where it
        // never reaches stdin and so never observes our death as an EOF. That is precisely
        // why the owner found one alive 78 seconds after its parent was gone.
        let child = crate::encode::spawn_ffmpeg_media_clock(
            64,
            64,
            64,
            64,
            30,
            &crate::encode::EncodePlan::for_backend(
                "sw",
                64,
                64,
                &crate::encode::Presets::default(),
            )
            .expect("the software plan is always available"),
            500,
            std::path::Path::new(&temp),
            std::path::Path::new(&mic),
            std::path::Path::new(&sys),
        )
        .expect("spawn the recording muxer");
        std::fs::write(&pidfile, child.id().to_string()).expect("publish the muxer pid");
        std::mem::forget(child);
        std::thread::sleep(std::time::Duration::from_secs(120));
    }

    /// The field report, reproduced end to end: a recorder dies without running a line of
    /// its own teardown, and its ffmpeg — wedged on FIFOs with no writer, holding the
    /// recording temp — must die with it.
    ///
    /// Loudly skips (never silently passes) without ffmpeg, per the repo convention for the
    /// tests that need a real one.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_wedged_ffmpeg_cannot_outlive_a_sigkilled_recorder() {
        use std::time::{Duration, Instant};
        let ffmpeg_ok = quiet_command(ffmpeg_path())
            .arg("-version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ffmpeg_ok {
            eprintln!(
                "SKIP a_wedged_ffmpeg_cannot_outlive_a_sigkilled_recorder: no runnable ffmpeg"
            );
            return;
        }
        let dir = std::env::temp_dir().join(format!("cck-d421-wedge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (mic, sys) = (dir.join("mic.pcm"), dir.join("sys.pcm"));
        for f in [&mic, &sys] {
            rustix::fs::mkfifoat(
                rustix::fs::CWD,
                f,
                rustix::fs::Mode::from_bits_truncate(0o600),
            )
            .unwrap();
        }
        let pidfile = dir.join("muxer.pid");
        let temp = dir.join(".wedge.recording.mkv");

        let mut recorder = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "util::tests::tether_fixture_wedged_ffmpeg",
                "--ignored",
                "--test-threads",
                "1",
            ])
            .env("CCK_TETHER_PIDFILE", &pidfile)
            .env("CCK_TETHER_MIC", &mic)
            .env("CCK_TETHER_SYS", &sys)
            .env("CCK_TETHER_TEMP", &temp)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(30);
        let muxer = loop {
            if let Some(pid) = std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "the fixture never spawned its muxer");
            std::thread::sleep(Duration::from_millis(50));
        };
        // Let it actually reach the FIFO open — i.e. become the wedged orphan-to-be.
        std::thread::sleep(Duration::from_millis(500));
        assert!(crate::instance::pid_is_live(muxer), "the muxer should be up and wedged");

        recorder.kill().unwrap(); // SIGKILL: no stop tail, no watchdog, no Drop
        recorder.wait().unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        while crate::instance::pid_is_live(muxer) {
            assert!(
                Instant::now() < deadline,
                "a wedged ffmpeg outlived its SIGKILLed recorder — this is the DRAGON-421 \
                 orphan, still holding the recording temp with nobody left to write its FIFOs"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_tethered_child_dies_when_its_parent_is_sigkilled() {
        use std::time::{Duration, Instant};
        let pidfile = std::env::temp_dir().join(format!("cck-d421-tether-{}", std::process::id()));
        let _ = std::fs::remove_file(&pidfile);
        let mut parent = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "util::tests::tether_fixture_parent",
                "--ignored",
                "--test-threads",
                "1",
            ])
            .env("CCK_TETHER_PIDFILE", &pidfile)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();

        // Wait for the fixture to publish its tethered child's pid.
        let deadline = Instant::now() + Duration::from_secs(30);
        let child_pid = loop {
            if let Some(pid) = std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "the fixture never spawned its tethered child");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(crate::instance::pid_is_live(child_pid), "the child should be running");

        // SIGKILL the parent: no destructor, no stop tail, no watchdog — nothing of ours
        // runs. This is exactly the crash that used to leave an ffmpeg holding a temp.
        parent.kill().unwrap();
        parent.wait().unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        while crate::instance::pid_is_live(child_pid) {
            assert!(
                Instant::now() < deadline,
                "the tethered child outlived its SIGKILLed parent — an orphaned muxer is \
                 exactly what DRAGON-421 makes impossible"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = std::fs::remove_file(&pidfile);
    }

    // On Windows, CREATE_NO_WINDOW must NOT break spawning or output capture — the flag
    // only suppresses the console window; inherited/piped stdio is unaffected. `cmd /C
    // echo` ships on every Windows install, so this proves the seam still runs a real
    // console tool and captures its stdout.
    #[cfg(windows)]
    #[test]
    fn quiet_command_still_spawns_and_captures_on_windows() {
        let out = quiet_command("cmd")
            .args(["/C", "echo", "cck-quiet-probe"])
            .output()
            .expect("cmd should spawn under CREATE_NO_WINDOW");
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("cck-quiet-probe"));
    }

    /// DRAGON-236 regression guard: every spawn of a CONSOLE-subsystem tool must go
    /// through the quiet-command seam ([`quiet_command`] / [`ffmpeg_command`] /
    /// [`ffprobe_command`]), never a bare `Command::new`. On Windows a GUI-subsystem
    /// process that spawns a bare console child pops a window and — when that child is
    /// killed — LEAVES a blank Windows Terminal pane; `CREATE_NO_WINDOW` prevents both.
    /// This scans the tree and fails if a bare routed-tool spawn creeps back in outside
    /// the seam. TEST code is scanned too: the original exemption ("test helpers … not
    /// a runtime console flash") was DISPROVEN 2026-07-22 — under a console-less parent
    /// (IDE runners, agent harnesses, CI wrappers) every console child a test spawns
    /// allocates a brand-new visible console (an EnumWindows watcher counted one
    /// Windows Terminal pane per ffmpeg/ffprobe spawn during `av_sync_tests`), so tests
    /// route through the seam like everything else. Excluded: the closed mac/linux
    /// platform bodies (cfg-gated OFF Windows, keeping their own byte-identical native
    /// spawns) and any hit whose ENCLOSING fn carries a `cfg(target_os =
    /// "macos"|"linux")` (a native body inside a shared file, not converted per the
    /// platform-isolation law).
    #[test]
    fn console_tool_spawns_go_through_the_quiet_seam() {
        // The exact bare-spawn spellings that must never appear outside the seam.
        const FORBIDDEN: &[&str] = &[
            "Command::new(crate::util::ffmpeg_path())",
            "Command::new(crate::util::ffprobe_path())",
            "Command::new(\"ffmpeg\")",
            "Command::new(\"ffprobe\")",
            "Command::new(\"curl\")",
            "Command::new(\"tesseract\")",
            "Command::new(\"netsh\")",
            "Command::new(\"cmd\")",
            // NOTE: `pactl` is deliberately NOT guarded — it is the Linux (`not(macos)`)
            // audio-device path; on Windows it simply FAILS TO START (no pactl.exe), so no
            // console is ever created and there is nothing to flash or leave behind.
        ];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut violations = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Whole-file mac/linux platform bodies are cfg-gated OFF Windows and
                    // keep their own native (byte-identical) spawns — not this class.
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if name != "mac" && name != "linux" {
                        stack.push(path);
                    }
                    continue;
                }
                let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if path.extension().and_then(|e| e.to_str()) != Some("rs")
                    || fname == "util.rs" // the seam + this guard's own literals
                {
                    continue;
                }
                // `#[cfg(test)]` blocks and `*_tests.rs` files are scanned like
                // everything else: a test-spawned console child flashes a console
                // under a console-less parent just as a runtime one does.
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                let lines: Vec<&str> = text.lines().collect();
                for (i, line) in lines.iter().enumerate() {
                    if !FORBIDDEN.iter().any(|p| line.contains(p)) {
                        continue;
                    }
                    // A cfg(macos)/cfg(linux) native body inside a shared file keeps its own
                    // spawn (byte-identical law — not converted). Attribute the hit to its
                    // ENCLOSING fn (walk up to the nearest fn declaration) and skip it if that
                    // fn carries a macos/linux cfg on its attribute lines — robust regardless
                    // of how far the spawn sits below the cfg (e.g. the mac install flow).
                    let is_fn_decl = |t: &str| {
                        let t = t.trim_start();
                        ["fn ", "pub fn ", "pub(crate) fn ", "pub(super) fn ", "async fn ",
                         "unsafe fn ", "pub async fn "]
                            .iter()
                            .any(|p| t.starts_with(p))
                    };
                    let gated = (0..i)
                        .rev()
                        .find(|&j| is_fn_decl(lines[j]))
                        .is_some_and(|fl| {
                            lines[fl.saturating_sub(4)..=fl].iter().any(|l| {
                                l.contains("cfg(target_os = \"macos\")")
                                    || l.contains("cfg(target_os = \"linux\")")
                            })
                        });
                    if !gated {
                        violations.push(format!("{}:{} {}", path.display(), i + 1, line.trim()));
                    }
                }
            }
        }
        assert!(
            violations.is_empty(),
            "bare console-tool Command::new outside the quiet-command seam — route via \
             crate::util::{{quiet_command, ffmpeg_command, ffprobe_command}} (DRAGON-236):\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn first_existing_or_bare_falls_back_to_bare_name_when_no_candidate_exists() {
        let missing = std::env::temp_dir().join(format!("cck-locate-missing-{}", std::process::id()));
        // Nothing on disk → bare name (a PATH lookup happens later at call sites).
        assert_eq!(first_existing_or_bare("ffmpeg", &[]), PathBuf::from("ffmpeg"));
        assert_eq!(
            first_existing_or_bare("ffmpeg", &[missing]),
            PathBuf::from("ffmpeg")
        );
    }

    #[test]
    fn first_existing_or_bare_returns_first_existing_candidate_in_order() {
        let base = std::env::temp_dir().join(format!("cck-locate-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&base);
        let missing = base.join("missing-dir").join("ffmpeg");
        let lower = base.join("lower");
        let higher = base.join("higher");
        let _ = std::fs::create_dir_all(&lower);
        let _ = std::fs::create_dir_all(&higher);
        let lower_tool = lower.join("ffmpeg");
        let higher_tool = higher.join("ffmpeg");
        std::fs::write(&lower_tool, b"x").expect("write lower tool");
        std::fs::write(&higher_tool, b"x").expect("write higher tool");
        // A non-existent earlier candidate is skipped; the FIRST existing one wins even
        // when a later candidate also exists (precedence is list order).
        assert_eq!(
            first_existing_or_bare("ffmpeg", &[missing, higher_tool.clone(), lower_tool.clone()]),
            higher_tool
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The one-shot native→`~/.config` migration moves the whole old dir when the
    /// new location is absent, and NEVER touches anything once the new one exists
    /// (no merge, no overwrite — the conservative contract `app_config_dir` relies on).
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn app_config_migration_moves_once_and_never_overwrites() {
        let base = std::env::temp_dir().join(format!("cck-cfg-migrate-{}", std::process::id()));
        let old = base.join("native/cosmic-capture-kit");
        let new = base.join("home/.config/cosmic-capture-kit");
        std::fs::create_dir_all(old.join("covermarks")).expect("seed old dir");
        std::fs::write(old.join("config.toml"), b"old").expect("seed old config");
        // New absent → the whole directory (config + covermarks) moves.
        migrate_app_config_dir(&old, &new);
        assert!(!old.exists(), "old dir should be gone after the move");
        assert_eq!(std::fs::read(new.join("config.toml")).expect("moved config"), b"old");
        assert!(new.join("covermarks").is_dir(), "subdirs ride along");
        // New present → a re-appearing old dir is left alone and the new one wins.
        std::fs::create_dir_all(&old).expect("recreate old dir");
        std::fs::write(old.join("config.toml"), b"stale").expect("seed stale config");
        migrate_app_config_dir(&old, &new);
        assert!(old.exists(), "existing new location must never absorb a merge");
        assert_eq!(std::fs::read(new.join("config.toml")).expect("kept config"), b"old");
        let _ = std::fs::remove_dir_all(&base);
    }

    // Windows (DRAGON-229): the bundled-ffmpeg story is the PORTABLE exe-adjacent
    // sidecar candidate (`<exe_dir>\ffmpeg.exe` via EXE_SUFFIX) — no Windows-specific
    // arm exists in `locate_tool` because none is needed. This pins that resolution
    // empirically against the REAL locate_tool: a sidecar next to the running (test)
    // executable wins; with it absent, the bare name (PATH lookup) is returned.
    #[cfg(target_os = "windows")]
    #[test]
    fn locate_tool_prefers_exe_adjacent_sidecar_then_path_on_windows() {
        let exe_dir = std::env::current_exe()
            .expect("test exe path")
            .parent()
            .expect("test exe dir")
            .to_path_buf();
        // A tool name no other test (and no PATH entry) uses, so parallel tests and a
        // real ffmpeg install can't interfere.
        let name = format!("cck-sidecar-probe-{}", std::process::id());
        let sidecar = exe_dir.join(format!("{name}.exe"));
        std::fs::write(&sidecar, b"x").expect("write sidecar next to the test exe");
        assert_eq!(
            locate_tool(&name, "CCK_SIDECAR_UNSET_FOR_TEST"),
            sidecar,
            "an exe-adjacent sidecar must win over the PATH fallback"
        );
        std::fs::remove_file(&sidecar).expect("remove sidecar");
        assert_eq!(
            locate_tool(&name, "CCK_SIDECAR_UNSET_FOR_TEST"),
            PathBuf::from(&name),
            "without the sidecar the bare name (PATH lookup) must come back"
        );
    }

    /// `dir_has_tool` must find a bare tool name whose on-disk file carries the platform
    /// `EXE_SUFFIX` — on Windows the file is `ffmpeg.exe` but `Command::new("ffmpeg")`
    /// resolves it without, so a bare-only check wrongly reports it missing and gates
    /// recording (DRAGON-229). On Linux/macOS (empty suffix) this is the plain bare match.
    #[test]
    fn dir_has_tool_matches_exe_suffixed_bare_name() {
        let dir = std::env::temp_dir().join(format!("cck-toolcheck-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mk temp dir");
        // The on-disk file as the OS ships it (bare on unix, `.exe` on windows).
        let disk = dir.join(format!("cck-fake-tool{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&disk, b"x").expect("write fake tool");
        assert!(
            dir_has_tool(&dir, Path::new("cck-fake-tool")),
            "the bare tool name must resolve to its EXE_SUFFIX file on disk"
        );
        assert!(!dir_has_tool(&dir, Path::new("cck-absent-tool")), "an absent tool is not found");
        let _ = std::fs::remove_file(&disk);
        let _ = std::fs::remove_dir(&dir);
    }

    // The mac locator must find the checked-out static ffmpeg/ffprobe under
    // vendor/ffmpeg/macos-arm64 in a dev build (CARGO_MANIFEST_DIR is baked at compile
    // time). Skips loudly if the vendored binaries aren't present in this checkout.
    #[cfg(target_os = "macos")]
    #[test]
    fn locate_tool_finds_dev_vendor_ffmpeg() {
        let vendor = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/ffmpeg/macos-arm64");
        if !vendor.join("ffmpeg").is_file() {
            eprintln!("skipping: no vendored ffmpeg at {}", vendor.display());
            return;
        }
        // No env override set → the vendor dir resolves (its file exists; the generic
        // exe-adjacent sidecar in a `cargo test` binary dir does not).
        let ff = locate_tool("ffmpeg", "CCK_FFMPEG_UNSET_FOR_TEST");
        assert_eq!(ff, vendor.join("ffmpeg"), "ffmpeg should resolve to the vendor dir");
        assert!(ff.is_file());
        let fp = locate_tool("ffprobe", "CCK_FFPROBE_UNSET_FOR_TEST");
        assert_eq!(fp, vendor.join("ffprobe"), "ffprobe should resolve to the vendor dir");
    }

    // ── The dev/test sandbox (DRAGON-424's key, DRAGON-433's config dir) ─────

    /// The harness and the child it spawns are two different binaries that have to agree on
    /// the sandbox folder, or a test's own child reads and writes somewhere the test cannot
    /// see. Moved here from `diag` by DRAGON-433, which made this key serve two sandboxes.
    #[test]
    fn the_sandbox_key_is_stable_and_per_worktree() {
        assert_eq!(fnv1a_hex("/home/jane/repo"), fnv1a_hex("/home/jane/repo"));
        assert_ne!(fnv1a_hex("/home/jane/repo"), fnv1a_hex("/home/jane/worktrees/other"));
        assert_eq!(fnv1a_hex("x").len(), 16);
        assert!(fnv1a_hex("x").chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The live assertion, made by the harness about ITSELF: this very process is a dev
    /// process, so the config it resolves is a sandbox and NOT the developer's real one.
    ///
    /// DRAGON-433. Before this, `state::load()` in a test read the `config.toml` of whoever
    /// ran the suite — so a test could pass or fail on the developer's settings, and a test
    /// that saved could overwrite them.
    #[test]
    fn this_test_binary_resolves_config_into_the_sandbox() {
        assert!(is_dev_process(), "a cargo test binary must classify itself as dev/test");
        let resolved = app_config_dir().expect("a dev process always has a sandbox");
        assert!(
            resolved.starts_with(std::env::temp_dir()),
            "config {resolved:?} must be under the temp dir, not the user's home"
        );
        // And specifically not the real location, whatever this OS calls it.
        if let Some(home) = dirs::home_dir() {
            let real = home.join(".config").join("cosmic-capture-kit");
            assert_ne!(resolved, real, "a test must never resolve the real config dir");
        }
    }

    /// The config FILE the app actually opens — asserted through `state::config_path` rather
    /// than re-derived here, so this covers the seam `state::load` uses.
    #[test]
    fn the_config_file_a_test_would_load_is_inside_the_sandbox() {
        let path = crate::state::config_path().expect("a dev process always has one");
        assert!(path.starts_with(std::env::temp_dir()), "config file at {path:?}");
        assert!(path.ends_with("config.toml"), "{path:?}");
    }

    /// The two sandboxes are SEPARATE folders sharing one key: a run's logs and its config
    /// never land in the same directory, but they always belong to the same run.
    #[test]
    fn the_log_and_config_sandboxes_are_distinct_but_share_a_key() {
        let cfg = sandbox_config_dir();
        let logs = crate::diag::log_dir().expect("a dev process always has a log sandbox");
        assert_ne!(cfg, logs);
        let key = sandbox_key();
        assert!(cfg.to_string_lossy().ends_with(&key), "{cfg:?}");
        assert!(logs.to_string_lossy().ends_with(&key), "{logs:?}");
    }
}
