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

/// Whether this process is running inside a FLATPAK sandbox (`lab/flatpak`).
///
/// Cached: the answer cannot change during a process's life, and several hot-ish paths ask
/// (the overlay mint, the config readers, the update gate).
///
/// See [`flatpak_sandboxed_from`] for why the marker FILE is the test rather than the
/// environment variable everyone reaches for first.
pub fn flatpak_sandboxed() -> bool {
    static SANDBOXED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SANDBOXED.get_or_init(|| flatpak_sandboxed_from(std::path::Path::new("/.flatpak-info").exists()))
}

/// **Pure**, unit-tested: the sandbox decision, given whether `/.flatpak-info` exists.
///
/// **Why the file and not `$FLATPAK_ID`.** Both are set inside a sandbox, and the environment
/// variable is the more obvious test. It is the wrong one HERE specifically, because this
/// codebase spawns detached children constantly: every capture child, the upload child, and
/// the whole `share::reexec` family. A child inherits the parent's environment, so a
/// `$FLATPAK_ID` test keeps answering "sandboxed" for any process descended from a sandboxed
/// one even after it has left the sandbox, and it can be set by anyone. `/.flatpak-info` is
/// placed in the mount namespace by Flatpak itself, so it is present exactly when the calling
/// process really is inside the sandbox and absent the moment it is not. Flatpak's own
/// documentation names it as the reliable check for this reason.
///
/// Taking the existence check as an argument keeps the decision testable without a sandbox,
/// which is the only way this is provable on a developer machine or in CI.
pub fn flatpak_sandboxed_from(marker_exists: bool) -> bool {
    marker_exists
}

// ── Session-bus reachability (a CAPABILITY, not a sandbox test) ───────────────
//
// A sandboxed process talks to the session bus through a FILTERING PROXY, and the filter is
// not an error the caller can see: `ListNames` silently omits names the policy does not
// cover, and a method call to one of them comes back as an ordinary failure. So a feature
// built on a bus name can be completely dead while every call site reports "nothing was
// playing" or "the file manager declined". Two of ours were (DRAGON-555, DRAGON-556).
//
// The policy that decides this is written where the process can read it: the
// `[Session Bus Policy]` section of `/.flatpak-info`. Asking THAT is a capability question
// with the same shape as the Wayland protocol probe: it is true on a normal session (no
// policy applies), true in a sandbox that was granted the name, and false only where the
// call really cannot land. It is deliberately not a `flatpak_sandboxed()` test: a grant
// added to the manifest heals the feature with no code change, which a sandbox test could
// never do.

/// The contents of `/.flatpak-info`, or `None` when this process is not inside a sandbox
/// (no bus policy applies to it at all). Read once and cached: the file cannot change under
/// a running process.
///
/// Linux-only, like Flatpak itself and like both callers of the capability below.
#[cfg(target_os = "linux")]
fn flatpak_info() -> Option<&'static str> {
    static INFO: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    INFO.get_or_init(|| std::fs::read_to_string("/.flatpak-info").ok()).as_deref()
}

/// May this process CALL methods on the session-bus name `want`?
///
/// `want` may name one bus name (`org.freedesktop.FileManager1`) or a whole family with a
/// trailing `.*` (`org.mpris.MediaPlayer2.*`, "any media player"). The answer is the live
/// session-bus policy, so it is `true` on every unsandboxed session and the callers keep
/// their existing behaviour byte-identical there.
///
/// See [`session_bus_talk_allowed`] for the decision itself.
#[cfg(target_os = "linux")]
pub fn bus_name_reachable(want: &str) -> bool {
    session_bus_talk_allowed(flatpak_info(), want)
}

/// **Pure**, unit-tested: may a process whose `/.flatpak-info` is `info` call methods on the
/// session-bus name (or `.*` family) `want`?
///
/// `None` means no sandbox policy applies, which is every ordinary launch, and the answer is
/// unconditionally yes. Inside a sandbox the `[Session Bus Policy]` section lists one
/// `name=verb` per line, where the verb is `see`, `talk` or `own` and the name may end in
/// `.*` to cover a family. Only `talk` and `own` let a method call through; `see` allows a
/// process to observe that a name exists and nothing more, so it is not enough for any of our
/// callers (pausing a player, showing a folder).
///
/// A policy entry and the wanted name are BOTH patterns, so the test is whether the two
/// overlap: `org.mpris.MediaPlayer2.*=talk` answers the family question, and so does a
/// single-player grant like `org.mpris.MediaPlayer2.spotify=talk`, because one reachable
/// player is enough for the feature to do something.
// Compiled into the TEST build on every host so the parser is proven wherever the suite runs,
// while staying honestly absent from a mac or Windows release binary, which has no Flatpak,
// no `/.flatpak-info` and no caller.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn session_bus_talk_allowed(info: Option<&str>, want: &str) -> bool {
    let Some(info) = info else { return true };
    let mut in_section = false;
    for line in info.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == "[Session Bus Policy]";
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((name, verb)) = line.split_once('=') else { continue };
        if verb != "talk" && verb != "own" {
            continue;
        }
        if bus_patterns_overlap(name.trim(), want) {
            return true;
        }
    }
    false
}

/// **Pure**, unit-tested: do two session-bus name patterns name any bus name in common?
///
/// A pattern is either an exact bus name or a prefix with a trailing `.*`, which Flatpak
/// documents as matching the prefix itself and everything below it. Two exact names overlap
/// only when equal; a wildcard overlaps anything at or below its prefix, in either direction
/// (asking about `org.mpris.MediaPlayer2.*` must be answered by a grant of `org.mpris.*` and
/// by a grant of one single player alike).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn bus_patterns_overlap(a: &str, b: &str) -> bool {
    /// `(prefix, is_wildcard)` for one pattern.
    fn parts(p: &str) -> (&str, bool) {
        match p.strip_suffix(".*") {
            Some(prefix) => (prefix, true),
            None => (p, false),
        }
    }
    /// Whether `name` is at or below `prefix`.
    fn covers(prefix: &str, name: &str) -> bool {
        name == prefix
            || (name.len() > prefix.len()
                && name.starts_with(prefix)
                && name.as_bytes()[prefix.len()] == b'.')
    }
    let (pa, wa) = parts(a);
    let (pb, wb) = parts(b);
    match (wa, wb) {
        (false, false) => pa == pb,
        (true, false) => covers(pa, pb),
        (false, true) => covers(pb, pa),
        // Two families overlap when either prefix is at or below the other.
        (true, true) => covers(pa, pb) || covers(pb, pa),
    }
}

/// How this build was DELIVERED. One vocabulary, so the debug log and the About page can never
/// disagree about what the user is running.
///
/// The distinction is not cosmetic. Each kind answers different questions about where the app's
/// own files are, whether it can update itself, and which copy of ffmpeg or tesseract is really
/// being spawned, which is the first thing to establish when a report says a bundled tool went
/// missing.
/// **The two axes are read differently, and DRAGON-614 is where that becomes visible.** The
/// three Linux kinds are a RUNTIME fact: AppImage and the plain binary are the same compiled
/// code, so no `cfg` can tell them apart and only the running process knows, from `$APPIMAGE`.
/// macOS and Windows are a COMPILE-TIME fact: a build either targets them or it does not.
/// [`package_kind`] branches on the second and delegates the first to [`package_kind_from`],
/// which stays exactly the Linux artifact-kind decision it always was.
///
/// This is deliberately NOT the same question [`crate::update::platform_key`] answers, and the
/// two must not be folded together. That one picks which artifact an UPDATE downloads and reads
/// `cfg!(target_os)`, `$APPIMAGE` and `cfg!(target_arch)` directly; it has never consulted this
/// enum and adding cases here cannot change a manifest key.
/// **Every build constructs only its own subset, so each variant declares where it is dead.**
/// The vocabulary is deliberately cross-platform (one enum, so the About badge and the debug
/// log speak the same five names everywhere, and `cargo test` exercises all five on any host),
/// but a Linux binary never constructs [`Self::MacOs`] and a Mac never constructs
/// [`Self::Binary`]. The per-variant gates are deliberately NOT one blanket allow on the enum:
/// this way the compiler still catches a variant that goes dead on its OWN platform, which
/// would mean `package_kind`'s arm for it had been lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind {
    /// A plain Linux binary: a distro build, a ZIP unpacked by hand, a `cargo` build.
    #[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
    Binary,
    /// A self-mounting AppImage bundle, carrying its own sidecars.
    #[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
    AppImage,
    /// A Flatpak sandbox: read-only `/app`, private config, some tools from the runtime.
    #[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
    Flatpak,
    /// A macOS build: the signed `.app` from the dmg, or a `cargo` build on a Mac.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    MacOs,
    /// A Windows build: the MSI install, or a `cargo` build on Windows.
    #[cfg_attr(not(windows), allow(dead_code))]
    Windows,
}

impl PackageKind {
    /// The user-facing name, for the About page.
    ///
    /// **The three Linux ones name their platform, and that is a deliberate reversal**
    /// (DRAGON-614). They read "Binary Release" and so on until macOS and Windows joined
    /// them, which was right while every case was Linux and wrong the moment the badge
    /// stopped being Linux-only: an unprefixed "Binary Release" beside "macOS Release" reads
    /// as a THIRD platform rather than as the Linux plain-binary artifact. Do not simplify
    /// the prefixes back out.
    ///
    /// macOS and Windows take no artifact word because they have no second artifact to be
    /// distinguished from: each ships one kind, so "macOS Release" is already unambiguous
    /// where "Linux Release" would not be.
    pub fn label(self) -> &'static str {
        match self {
            PackageKind::Binary => "Linux Binary Release",
            PackageKind::AppImage => "Linux AppImage Release",
            PackageKind::Flatpak => "Linux Flatpak Release",
            PackageKind::MacOs => "macOS Release",
            PackageKind::Windows => "Windows Release",
        }
    }

    /// The glyph the About page's release-kind line shows for this package (DRAGON-591 and
    /// DRAGON-614, the owner's picks): lucide `binary` for a bare executable, `file-archive`
    /// for the AppImage (one file carrying a packed filesystem), the general `package` box for
    /// the Flatpak, `apple` for macOS and `grid-2x2` for Windows. Names are the
    /// freedesktop-ish keys `widgets::icons::handle` maps into the bundled lucide set, so this
    /// returns a NAME rather than bytes and the icon plumbing stays the single resolver.
    pub fn icon_name(self) -> &'static str {
        match self {
            PackageKind::Binary => "application-x-executable-symbolic",
            PackageKind::AppImage => "application-x-appimage-symbolic",
            PackageKind::Flatpak => "package-x-generic-symbolic",
            PackageKind::MacOs => "package-macos-symbolic",
            PackageKind::Windows => "package-windows-symbolic",
        }
    }

    /// The debug log's spelling. Deliberately NOT the same string as [`Self::label`]: the log
    /// says what the thing IS ("plain binary") while the UI names a release channel, and a log
    /// line reading "Binary Release" would imply a distribution we do not have.
    ///
    /// The two non-Linux spellings are COARSER than the Linux three, and that is honest rather
    /// than lazy: the Linux names distinguish three artifacts that behave differently from each
    /// other, and neither macOS nor Windows ships a second kind for these to be told apart
    /// from. There is nothing finer to say.
    pub fn diag_name(self) -> &'static str {
        match self {
            PackageKind::Binary => "plain binary",
            PackageKind::AppImage => "AppImage",
            PackageKind::Flatpak => "Flatpak",
            PackageKind::MacOs => "macOS build",
            PackageKind::Windows => "Windows build",
        }
    }
}

/// How THIS process was delivered.
///
/// macOS and Windows answer from a `cfg`, because on those platforms the question has a
/// compile-time answer. Only the Linux arm has anything to probe, and it asks
/// [`package_kind_from`], which is that arm's whole decision.
pub fn package_kind() -> PackageKind {
    #[cfg(target_os = "macos")]
    {
        PackageKind::MacOs
    }
    #[cfg(windows)]
    {
        PackageKind::Windows
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        package_kind_from(flatpak_sandboxed(), running_from_appimage())
    }
}

/// **Pure**, unit-tested: the LINUX artifact-kind decision, with both probes injected.
///
/// Three artifacts, one compiled binary, so this is a runtime question and not a `cfg` one.
/// It is called only from [`package_kind`]'s Linux arm; macOS and Windows never reach it,
/// because their kind is settled at compile time and modelling them the same way would invent
/// a probe for a fact that is already known.
///
/// Flatpak is asked FIRST, and the order is load-bearing rather than arbitrary. The two are not
/// mutually exclusive in principle (nothing stops someone putting an AppImage inside a Flatpak),
/// and if that ever happened the sandbox is the fact that governs everything a reader cares
/// about here: the read-only `/app`, the private config, the hidden Wayland globals. Reporting
/// "AppImage" for it would name the inner packaging and hide the thing actually shaping the
/// app's behaviour.
// Its only non-test caller is the Linux arm above, so off Linux it is honestly dead rather
// than hidden behind a blanket allow. It stays compiled everywhere because the decision is
// portable and `cargo test` proves it on any host, which is the whole point of the split.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
pub fn package_kind_from(flatpak: bool, appimage: bool) -> PackageKind {
    if flatpak {
        PackageKind::Flatpak
    } else if appimage {
        PackageKind::AppImage
    } else {
        PackageKind::Binary
    }
}

/// The user's REAL config directory, seeing past a Flatpak sandbox (`lab/flatpak`).
///
/// Outside a sandbox this is exactly `dirs::config_dir()` and nothing changes.
///
/// Inside one it must not be, and the reason is easy to get wrong. Flatpak leaves `$HOME`
/// pointing at the real home but redirects `$XDG_CONFIG_HOME` to
/// `~/.var/app/<app-id>/config`, which is the app's PRIVATE store. `dirs::config_dir()` reads
/// that variable, so every caller silently starts reading a directory the desktop has never
/// written to. Nothing errors: the reads just miss and fall through to defaults, so the
/// symptom is a capture with the wrong accent and square corners rather than a failure.
///
/// The host's own config is still reachable, because a `--filesystem=xdg-config/cosmic:ro`
/// grant bind-mounts it at its ordinary path under the real `$HOME`. So the fix is to stop
/// asking `$XDG_CONFIG_HOME` and ask the host instead. libcosmic's `cosmic-config` solved the
/// identical problem the same way, which is why the COSMIC readers here can follow it.
///
/// **The rule, rather than a caller list that goes stale.** Reading somebody ELSE'S config
/// (the desktop's, the compositor's, another app's) goes through here. Reading OUR OWN config
/// keeps using `dirs::config_dir()` via [`app_config_dir`], because inside a sandbox ours
/// SHOULD be private. So: if the file was written by something other than this app, ask this
/// function.
///
/// Callers today are the COSMIC theme reader (accent, corner radius), the COSMIC wallpaper
/// readers, the KDE applet reader, the sway/hyprland reader, and the XDG autostart entry,
/// which the SESSION reads even though we write it.
///
/// DRAGON-619 is what turned this from a list into a rule: three readers were added over time
/// using `dirs::config_dir()` directly, and every one of them silently read the sandbox and
/// missed. The failure is invisible by construction, since a miss is indistinguishable from a
/// machine that genuinely has no such config, so nothing logs and nothing fails. DRAGON-618
/// was the same mistake in the writing direction.
// Callers are the COSMIC theme and wallpaper readers, all `cfg(target_os = "linux")`, so
// this is honestly dead off Linux. Kept portable rather than Linux-gated so the pure decision
// below stays reachable from the tests on every host.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn host_config_dir() -> Option<std::path::PathBuf> {
    host_config_dir_from(
        flatpak_sandboxed(),
        std::env::var_os("HOST_XDG_CONFIG_HOME").map(std::path::PathBuf::from),
        dirs::home_dir(),
        dirs::config_dir(),
    )
}

/// **Pure**, unit-tested: [`host_config_dir`]'s decision, with every input injected.
///
/// The ladder inside a sandbox, in order, and each rung earns its place:
///
/// 1. `$HOST_XDG_CONFIG_HOME`, which Flatpak sets when the HOST's own `XDG_CONFIG_HOME` is
///    non-default. Rare, but when it is set it is the only correct answer, because the user's
///    config genuinely is not at `~/.config`.
/// 2. `$HOME/.config`, the ordinary case. `$HOME` is the real home inside the sandbox, so this
///    resolves to the bind-mounted host directory.
/// 3. `dirs::config_dir()`, which is the sandboxed path and therefore wrong, kept only so the
///    function is total. A caller reading it finds nothing and falls back to its own defaults,
///    which is the same outcome as a machine with no COSMIC config at all.
// Dead off Linux for the same reason as `host_config_dir`, but compiled everywhere on
// purpose: it is the pure half, and its tests are what prove the sandbox rule on a machine
// that has no sandbox.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn host_config_dir_from(
    sandboxed: bool,
    host_xdg_config_home: Option<std::path::PathBuf>,
    home: Option<std::path::PathBuf>,
    config_dir: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    if !sandboxed {
        return config_dir;
    }
    host_xdg_config_home
        .or_else(|| home.map(|h| h.join(".config")))
        .or(config_dir)
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

/// Locate the `tesseract` binary (same resolution order as [`ffmpeg_path`]:
/// `CCK_TESSERACT` override → mac `.app` `Resources/` → exe-adjacent sidecar → PATH).
///
/// It went through [`locate_tool`] late (DRAGON-527). The three OCR call sites used a
/// BARE NAME, so tesseract could only ever be found on `PATH`, fine for Linux, where
/// the distro packages it, and silently broken for a packaged mac/Windows build, which
/// has no Homebrew and nothing on `PATH`. OCR was therefore absent for exactly the
/// users who paid for the app. Routing it here lets the packaged builds ship a sidecar
/// beside the binary, the same way they already ship ffmpeg.
pub fn tesseract_path() -> PathBuf {
    locate_tool("tesseract", "CCK_TESSERACT")
}

/// Where tesseract should look for `*.traineddata`, given the binary that [`tesseract_path`]
/// actually resolved. **Pure**, unit-tested: the effectful half is [`tessdata_dir`].
///
/// The distinction is the whole point, and getting it backwards would be a regression the
/// user sees as "my language packs disappeared":
///
/// * A **bundled** tesseract (an absolute path: the mac `Resources/` sidecar, the
///   exe-adjacent Windows/AppImage sidecar, or a `CCK_TESSERACT` override) ships with only
///   `eng`. It has no system tessdata to fall back on, so we own the directory and hand it
///   a writable one the user can drop more languages into.
/// * A **PATH** tesseract (the bare name, i.e. the distro's own package) already knows
///   where its data lives, and the user's `tesseract-data-deu` sits there. Pointing it
///   somewhere else would HIDE every language pack they installed. So: leave it alone.
fn tessdata_dir_for(resolved: &Path, writable: Option<PathBuf>) -> Option<PathBuf> {
    // `locate_tool` returns the bare name when nothing on disk matched, which is the
    // signal that the OS will resolve it on PATH.
    if resolved.parent().is_none_or(|p| p.as_os_str().is_empty()) {
        return None;
    }
    writable
}

/// [`tessdata_dir_for`] with the real writable location filled in: `<app config dir>/tessdata`,
/// which is sandboxed away from a developer's real directory like every other config-adjacent
/// path (see [`app_config_dir`]). `None` means "say nothing to tesseract and let it use its
/// own data", which is what a distro install wants.
pub fn tessdata_dir() -> Option<PathBuf> {
    tessdata_dir_for(&tesseract_path(), app_config_dir().map(|d| d.join("tessdata")))
}

/// Seed the writable tessdata dir from the bundled `eng.traineddata` on first run, so a
/// packaged build has a language to work with and a folder the user can add to.
///
/// Best-effort and idempotent: an existing file is never overwritten (the user may have
/// replaced `eng` with `tessdata_best`), and every failure is a warning rather than an
/// error, because OCR is an optional feature and a read-only install must not break launch.
pub fn seed_tessdata() {
    let Some(dir) = tessdata_dir() else {
        return; // PATH tesseract: the distro owns the data.
    };
    let tool = tesseract_path();
    let Some(bundled) = tool.parent().map(|d| d.join("tessdata")) else {
        return;
    };
    if !bundled.is_dir() {
        return; // No sidecar data shipped (a dev build pointing at a system tesseract).
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("tessdata: could not create the language dir: {e}");
        return;
    }
    // What we last seeded, so a shipped update can replace OUR copy without ever
    // touching the user's. See `seed_verdict`.
    let ledger = dir.join(".seeded");
    let mut seeded = read_seed_ledger(&ledger);
    let mut ledger_dirty = false;

    // Mirror the WHOLE bundled tree, not just `*.traineddata`.
    //
    // This filtered on that extension once, and the result was that bundled OCR
    // returned nothing at all, silently. The scan runs `tesseract … tsv`, where `tsv`
    // is not an output flag: it is a CONFIG FILE (`configs/tsv`, containing
    // `tessedit_create_tsv 1`) that tesseract loads from the tessdata directory. With
    // the language pack present but no `configs/`, tesseract prints
    // `read_params_file: Can't open tsv` to stderr, quietly falls back to PLAIN TEXT,
    // and exits 0. `parse_tsv_words` then finds no columns and returns an empty list,
    // so the feature looks broken with no error anywhere. Verified both ways.
    for (rel, src, src_len) in collect_files(&bundled) {
        let dest = dir.join(&rel);
        let dest_len = std::fs::metadata(&dest).map(|m| m.len()).ok();
        match seed_verdict(dest_len, src_len, seeded.get(&rel).copied()) {
            SeedVerdict::UpToDate => continue,
            SeedVerdict::UserOwned => {
                log::debug!("tessdata: leaving the user's own {rel} alone");
                continue;
            }
            SeedVerdict::Write => {}
        }
        if let Some(parent) = dest.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            log::warn!("tessdata: could not create {}: {e}", parent.display());
            continue;
        }
        // Copy aside, then RENAME into place. A language pack is tens of megabytes and
        // this app is one-shot: a process that exits mid-copy would otherwise leave a
        // truncated file that the ownership rule then preserves forever, so OCR would
        // fail on every later run with no way back short of deleting the folder by hand.
        // The rename is atomic within the directory, so the name only ever appears
        // complete. A `.part` left by an interrupted run is overwritten by the next one.
        let part = dest.with_extension("cck-part");
        if let Err(e) = std::fs::copy(&src, &part) {
            log::warn!("tessdata: could not seed {rel}: {e}");
            continue;
        }
        if let Err(e) = std::fs::rename(&part, &dest) {
            log::warn!("tessdata: could not publish {rel}: {e}");
            let _ = std::fs::remove_file(&part);
            continue;
        }
        seeded.insert(rel, src_len);
        ledger_dirty = true;
    }
    if ledger_dirty {
        write_seed_ledger(&ledger, &seeded);
    }
}

/// Every file under `root`, as `(path relative to root, absolute path, length)`.
///
/// Hand-rolled rather than pulling in `walkdir`: the tree is a language directory a
/// handful of levels deep, and this is the only place in the crate that needs a walk.
/// Depth-limited so a symlink loop in a user-writable directory cannot hang a capture
/// launch.
fn collect_files(root: &Path) -> Vec<(String, PathBuf, u64)> {
    fn walk(dir: &Path, prefix: &str, depth: u32, out: &mut Vec<(String, PathBuf, u64)>) {
        if depth > 4 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            match entry.file_type() {
                Ok(t) if t.is_dir() => walk(&entry.path(), &rel, depth + 1, out),
                Ok(t) if t.is_file() => {
                    let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    out.push((rel, entry.path(), len));
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", 0, &mut out);
    out
}

/// What [`seed_tessdata`] should do with one language file.
#[derive(Debug, PartialEq, Eq)]
enum SeedVerdict {
    /// The user put this here, or replaced ours. Never touch it.
    UserOwned,
    /// Already ours and already current.
    UpToDate,
    /// Missing, or ours and stale.
    Write,
}

/// **Pure**, unit-tested: decide whether to write a bundled language pack over what is
/// already on disk.
///
/// The rule the user asked for: **a pack the user installed always wins**, but a pack WE
/// seeded must still be replaceable when a release ships a newer one. Before this, the
/// rule was simply "never overwrite", which protected the user's file correctly and also
/// meant an updated `eng.traineddata` could never reach anyone who had already launched
/// the old build.
///
/// Ownership is decided by SIZE against a ledger of what we last wrote, rather than by
/// hashing: these files are tens of megabytes and this runs on a one-shot launch path, so
/// reading them fully on every capture would be absurd. A size match is not proof of
/// identity, but the failure it risks is "we overwrite our own copy with an identical
/// one", which costs nothing. The case that MATTERS, a user dropping in `tessdata_best`
/// or another language's file, changes the size in practice.
fn seed_verdict(dest_len: Option<u64>, src_len: u64, seeded_len: Option<u64>) -> SeedVerdict {
    let Some(dest_len) = dest_len else {
        return SeedVerdict::Write; // nothing there yet
    };
    match seeded_len {
        // We have no record of writing this, so the user put it there.
        None => SeedVerdict::UserOwned,
        // On disk no longer matches what we wrote: the user replaced it.
        Some(recorded) if recorded != dest_len => SeedVerdict::UserOwned,
        // Still ours. Write only when the bundled pack has actually changed.
        Some(_) if dest_len == src_len => SeedVerdict::UpToDate,
        Some(_) => SeedVerdict::Write,
    }
}

/// Read the seed ledger: one `<size> <name>` line per file we wrote. A missing or
/// unreadable ledger reads as empty, which makes every existing file user-owned, the
/// safe direction.
fn read_seed_ledger(path: &Path) -> std::collections::HashMap<String, u64> {
    let mut out = std::collections::HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return out;
    };
    for line in text.lines() {
        if let Some((len, name)) = line.split_once(' ')
            && let Ok(len) = len.parse::<u64>()
        {
            out.insert(name.to_string(), len);
        }
    }
    out
}

/// Write the seed ledger. Best-effort: losing it only makes the next run treat our own
/// files as the user's, which is the conservative direction.
fn write_seed_ledger(path: &Path, seeded: &std::collections::HashMap<String, u64>) {
    let body: String = seeded.iter().map(|(n, l)| format!("{l} {n}\n")).collect();
    if let Err(e) = std::fs::write(path, body) {
        log::warn!("tessdata: could not record what was seeded: {e}");
    }
}

// `ffplay_path` is GONE, and the reason matters more than the deletion.
//
// It existed because the bundled Windows ffmpeg has no pulse muxer and no audio-output
// device at all, so ffmpeg could not be the preview soundtrack's sink there and an
// `ffplay` sidecar (SDL2 → default endpoint) was (DRAGON-285). That sidecar reported no
// position, which is why the Windows preview rode a bootstrap clock for the whole session
// instead of a real playout clock.
//
// The WASAPI render sink (`platform::windows::audio_out`) replaced it: we own the output,
// so `IAudioClock::GetPosition` gives the true audible position and Windows gets the same
// playout clock the other platforms have. Its last caller went with it.
//
// If you are re-adding an ffplay path, re-read that: the problem was never locating the
// binary, it was that a sidecar we do not own cannot tell us where playback actually is.
//
// NOTE FOR PACKAGING: `scripts/fetch-win-vendor.ps1` still fetches and asserts
// `ffplay.exe`, and the MSI still ships it, on the strength of a comment there calling it
// "NOT optional ... the preview editor's audio sink on Windows". That statement is now
// false. Dropping it is a packaging decision (it is ~2MB of the bundle), deliberately left
// out of the change that made it unused.

/// Locate the `proton-drive` binary, Proton's own official Drive command-line tool
/// (DRAGON-485), with the same resolution order as [`ffmpeg_path`]: the `CCK_PROTON_DRIVE`
/// override, then a sidecar next to our executable, then `PATH`.
///
/// **Optional at runtime and never bundled by the DIRECT builds**, exactly like `ffmpeg` and
/// `tesseract`. It is a ~118 MB standalone binary the user downloads from Proton, and a copy
/// inside our direct artifacts would be enormous and would go stale on OUR update cadence
/// rather than Proton's. The Proton provider probes for it and, when it is absent, says so
/// and (outside a Flatpak) offers the download page rather than failing a connect halfway
/// through. See `cloud::proton`.
///
/// **The FLATPAK bundles it since DRAGON-566**, at `/app/bin/proton-drive`, which is exactly
/// the exe-adjacent arm of this resolution: inside the sandbox a host install is invisible,
/// so "download it yourself" is advice that package makes false; the CLI is MIT-licensed, so
/// redistribution is clean; and the store is the Flatpak's update channel, so the bundled
/// copy is updated with the app. `cloud::proton`'s module doc carries the whole record.
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

/// Which `vendor/<dir>/macos-arm64/` directory a tool is vendored into.
///
/// **Pure**, unit-tested. Not simply `name`, because ffprobe ships alongside ffmpeg from
/// the same upstream archive and so shares its directory; everything else vendors under
/// its own name.
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn vendor_subdir(name: &str) -> &str {
    match name {
        "ffprobe" | "ffplay" => "ffmpeg",
        other => other,
    }
}

// ── Re-executing ourselves (DRAGON-510) ──────────────────────────────────────

/// Set by the AppImage runtime to the `.AppImage` FILE it is running, which is the only
/// path to this program that outlives the mount. Not a `CCK_` name because it is not ours:
/// the runtime defines it, and it is absent from every other kind of launch.
const APPIMAGE_ENV: &str = "APPIMAGE";

/// The path to spawn when this process re-executes ITSELF.
///
/// **Why this is not just `current_exe()`.** Inside an AppImage, `current_exe()` is
/// `/tmp/.mount_XXXXXX/usr/bin/cosmic-capture-kit`, and that mount exists only while the
/// process that created it is alive. This app deliberately spawns children that OUTLIVE
/// their parent (the clipboard / notify / upload helpers, [`crate::share::reexec`], and the
/// resident daemon's capture children), and `App::finish_session` always exits. A child
/// holding the mount path can therefore lose its own executable mid-run, and anything that
/// RECORDS the path for later (the autostart `.desktop` entry) records one that is
/// guaranteed to be gone by the next login. `$APPIMAGE` is the AppImage file itself, which
/// mounts afresh on every invocation.
///
/// Byte-identical off an AppImage: the variable is absent, so this is `current_exe()`.
///
/// **Not for [`locate_tool`]**, which keeps `current_exe()` on purpose. That one wants the
/// MOUNT path, because the bundled `ffmpeg` / `tesseract` sidecars live next to the
/// extracted binary and are not reachable through the `.AppImage` file.
pub fn self_exe() -> std::io::Result<PathBuf> {
    let current = std::env::current_exe()?;
    Ok(re_exec_path(std::env::var_os(APPIMAGE_ENV).as_deref(), &current))
}

/// **Pure**, unit-tested: [`self_exe`]'s decision, with the environment injected.
fn re_exec_path(appimage: Option<&std::ffi::OsStr>, current: &Path) -> PathBuf {
    appimage_from(appimage).unwrap_or_else(|| current.to_path_buf())
}

/// The `.AppImage` FILE this process is running from, or `None` on every other kind of
/// launch (a source build, the release binary, the `.app`, the MSI install).
///
/// The honest answer to "which file IS this program", which is a different question from
/// [`self_exe`]'s "what should I spawn": they agree inside an AppImage and diverge outside
/// it, where `self_exe` still has an answer and this has none. The in-place self-update
/// (DRAGON-532) needs the file, and needs `None` to mean "there is nothing here we own,
/// so do not offer to replace it".
///
/// Linux-only, and honestly dead elsewhere: AppImage is a Linux format, so no macOS or
/// Windows build has a caller. The pure [`appimage_from`] underneath stays ungated, because
/// [`re_exec_path`] uses it on every platform.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn appimage_path() -> Option<PathBuf> {
    appimage_from(std::env::var_os(APPIMAGE_ENV).as_deref())
}

/// **Pure**, unit-tested: [`appimage_path`]'s decision, with the environment injected, and
/// the ONE place the "what counts as running from an AppImage" rule is written.
///
/// An EMPTY `$APPIMAGE` is treated as absent rather than as a path of `""`. As a spawn
/// target that would fail with a bare `No such file or directory` and no hint of where the
/// path came from; as an update target it would be far worse, since the installer's
/// swap would aim at a file the user never named.
fn appimage_from(appimage: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    match appimage {
        Some(p) if !p.is_empty() => Some(PathBuf::from(p)),
        _ => None,
    }
}

/// Set by the AppImage runtime to the directory it MOUNTED the bundle at, the root the
/// bundle's own `usr/bin`, `usr/lib` and `usr/share` hang off. Like [`APPIMAGE_ENV`] it is
/// the runtime's name, not ours, and it is absent from every other kind of launch.
const APPDIR_ENV: &str = "APPDIR";

/// The directory this process's AppImage bundle is mounted at, or `None` on every other kind
/// of launch. The ONE place the app asks that question, so the Health page and the debug log
/// can never disagree about it.
///
/// The mount point is a fresh random path (`/tmp/.mount_CosmicXXXXXX`) on every launch, so it
/// is only ever useful as something to measure another path AGAINST: see [`bundle_label`].
///
/// Not gated to Linux even though AppImage is a Linux format. It is two environment reads
/// that no macOS or Windows launch can satisfy (nothing there sets the runtime's variables),
/// its `$APPIMAGE` half is already portable for [`re_exec_path`], and keeping it portable is
/// what lets [`tool_location_label`] stay free of platform branches.
pub fn appimage_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    appimage_dir_from(
        std::env::var_os(APPIMAGE_ENV).as_deref(),
        std::env::var_os(APPDIR_ENV).as_deref(),
        exe.as_deref(),
    )
}

/// **Pure**, unit-tested: [`appimage_dir`]'s decision, with the environment injected.
///
/// `$APPIMAGE` is REQUIRED and is the "this really is an AppImage launch" marker
/// ([`appimage_from`], the same rule the re-exec path uses). Empty values are treated as
/// absent, exactly as [`appimage_from`] treats them.
///
/// `$APPDIR` is preferred but NO LONGER REQUIRED, and that change is a bug fix rather than a
/// loosening (owner report: a Linux Mint user's AppImage showed plain paths where a CachyOS one
/// showed `AppImage://`).
///
/// The reason it can go missing is that our `AppRun` is a SYMLINK straight to the binary rather
/// than a shell script, deliberately, so nothing of ours exports anything. We inherit whatever
/// the runtime chose to set, and that is not identical everywhere: extract-and-run mode, older
/// runtimes and some launchers differ. Requiring both variables therefore made a purely
/// cosmetic label depend on an environment detail the user cannot see or influence, and its
/// absence looked like "the bundled ffmpeg is not being used" when the bundled ffmpeg was in
/// fact being used the whole time.
///
/// So when `$APPDIR` is absent, derive the root from where WE are, which is ground truth: the
/// binary lives at `<mount>/usr/bin/<name>`, so three parents up is the mount. `exe` is the
/// resolved `current_exe`.
///
/// The `usr/bin` shape is CHECKED rather than assumed, and that check is what keeps this
/// honest: without it, any `$APPIMAGE`-carrying launch would claim its grandparent directory as
/// a bundle root and start labelling ordinary paths. A dev build under `target/release` fails
/// the check and is left alone.
fn appimage_dir_from(
    appimage: Option<&std::ffi::OsStr>,
    appdir: Option<&std::ffi::OsStr>,
    exe: Option<&Path>,
) -> Option<PathBuf> {
    appimage_from(appimage)?;
    if let Some(d) = appdir
        && !d.is_empty()
    {
        return Some(PathBuf::from(d));
    }
    // `<mount>/usr/bin/<name>` → `<mount>`, but only when the tail really is `usr/bin`.
    let exe = exe?;
    let bin = exe.parent()?;
    let usr = bin.parent()?;
    if bin.file_name()? != "bin" || usr.file_name()? != "usr" {
        return None;
    }
    let root = usr.parent()?;
    // A bundle root of `/` is not a bundle root, it is a distro install at
    // `/usr/bin/cosmic-capture-kit` that happens to match the shape. Accepting it would make
    // `bundle_label` strip `/` off every system path and report `AppImage://usr/bin/ffmpeg`
    // for a file we did not ship. Caught by `a_wrong_shaped_exe_path_derives_nothing`.
    (root != Path::new("/") && root.file_name().is_some()).then(|| root.to_path_buf())
}

/// Whether this launch is running from a mounted AppImage bundle rather than a plain binary
/// (a distro build, a ZIP unpacked by hand, a `cargo` build). Always false off Linux.
///
/// Its one caller is [`package_kind`]'s Linux arm, so DRAGON-614 made it honestly dead on the
/// other two: those platforms answer their package kind from a `cfg` and have no artifact
/// kinds to tell apart, which is the whole distinction that commit drew. It stays portable
/// rather than becoming Linux-only because [`appimage_dir`] beneath it already is (it reads
/// `$APPIMAGE`, which simply never exists elsewhere), and a `cfg`-gated body here would be a
/// second statement of the same fact.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
pub fn running_from_appimage() -> bool {
    appimage_dir().is_some()
}

// ── Spelling our own launch, for a human to paste (DRAGON-589) ───────────────

/// The shell command line that starts THIS build with `flags`, for a user who has to bind it
/// as a shortcut in their own desktop.
///
/// [`self_exe`] answers "what path do I spawn"; this answers the neighbouring question "what
/// does a PERSON type to spawn me". Same subject, different audience, so the answer is a
/// string rather than a `PathBuf` and it has to survive a paste into a terminal.
///
/// The reader collects the two facts and [`launch_command_from`] decides. See that function
/// for why the three package kinds are spelled differently.
pub fn launch_command(flags: &[&str]) -> String {
    let exe = self_exe().ok();
    launch_command_from(package_kind(), exe.as_deref(), FLATPAK_APP_ID, flags)
}

/// The Flatpak application id this build ships under, which is also the only handle a Flatpak
/// launch has: `flatpak run <app-id>`. Inside the sandbox the binary's own path (`/app/bin/…`)
/// exists only for processes already in that sandbox, so it is useless to the user's desktop
/// shortcut, which runs on the HOST.
///
/// The same string the manifest declares (`scripts/flatpak/dev.thedragon.CosmicCaptureKit.yml`)
/// and the same one `cosmic::Application::APP_ID` carries.
pub const FLATPAK_APP_ID: &str = "dev.thedragon.CosmicCaptureKit";

/// **Pure**, unit-tested: [`launch_command`]'s decision, with both probes injected.
///
/// One action, three spellings, because how you start this program depends entirely on how it
/// was delivered:
///
/// * **Flatpak**: `flatpak run <app-id> --flag`. The executable path inside the sandbox is not
///   reachable from the host, and the app id is the only name the host knows us by.
/// * **AppImage**: the `.AppImage` FILE, which is what [`self_exe`] already resolves inside a
///   bundle (`$APPIMAGE`). Emphatically NOT `current_exe()`, which names a FUSE mount that
///   stops existing the moment this process does, so a shortcut recorded against it would
///   break at the next launch.
/// * **Plain binary**: the resolved executable path, which for this project is usually a
///   `target/release` build rather than anything on `PATH`.
///
/// `exe` is `None` only when the platform could not report our own path at all, which leaves
/// nothing honest to print; the command then falls back to the program NAME, which is right
/// for a distro install on `PATH` and at least tells the reader what to run.
///
/// The path is home-collapsed and shell-quoted; see [`shell_word`] for the quoting rule and
/// [`collapse_home`] for why `~` is a safe default rather than a trade-off.
pub fn launch_command_from(
    kind: PackageKind,
    exe: Option<&Path>,
    app_id: &str,
    flags: &[&str],
) -> String {
    let head = match kind {
        PackageKind::Flatpak => format!("flatpak run {app_id}"),
        // DRAGON-614: macOS and Windows join the executable-path arm, which is where they
        // already were as `PackageKind::Binary`, so the command they print is unchanged. The
        // Flatpak is still the only kind whose own path is unreachable from the host.
        PackageKind::Binary
        | PackageKind::AppImage
        | PackageKind::MacOs
        | PackageKind::Windows => match exe {
            Some(p) => shell_word(&collapse_home(&p.to_string_lossy())),
            None => BIN_NAME.to_string(),
        },
    };
    let mut out = head;
    for f in flags {
        out.push(' ');
        out.push_str(&shell_word(f));
    }
    out
}

/// The program's own basename, the last-resort spelling when the platform cannot report where
/// this executable lives. Matches the `[[bin]]` name in `Cargo.toml`, which is what a distro
/// package puts on `PATH`.
///
/// `pub(crate)` since DRAGON-618: the Flatpak autostart path hands this exact name to the
/// Background portal as the command to launch inside the sandbox, where `/app/bin` is on
/// `PATH`. It must be the SAME string the binary is actually installed as, so it defers here
/// rather than re-spelling it.
pub(crate) const BIN_NAME: &str = "cosmic-capture-kit";

/// **Pure**, unit-tested: `word` as ONE argument of a shell command line, quoted only if it
/// has to be.
///
/// Quoting only when needed is the point. A command a user is meant to read and paste should
/// look like the command they would have typed, and `'/usr/bin/cosmic-capture-kit' --region`
/// reads as a mistake. A path with a space in it, though, silently becomes two arguments, so
/// that case must be quoted or the pasted line does something else entirely.
///
/// The safe set is the characters no POSIX shell treats specially: letters, digits, and
/// `_ - . / : = + , @ %`. Anything else (a space, a quote, `$`, `&`, a newline, a non-ASCII
/// byte) sends the word through single quotes, where the shell interprets nothing at all. A
/// single quote inside is written `'\''`, the same escape `update::shell_single_quote` uses
/// for the inside of a script literal; that one always sits inside quotes its caller wrote,
/// while this decides whether there are quotes at all, which is why they are separate.
///
/// A LEADING `~/` is kept outside the quotes, because `~` only expands unquoted. `~/'my
/// folder'/app` is one word to the shell, expands the home directory, and runs.
pub fn shell_word(word: &str) -> String {
    fn safe(s: &str) -> bool {
        !s.is_empty()
            && s.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '=' | '+' | ',' | '@' | '%')
            })
    }
    fn quoted(s: &str) -> String {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
    if safe(word) {
        return word.to_string();
    }
    // `~` alone, or a home-collapsed path whose tail needs quoting: the tilde stays bare so the
    // shell still expands it, and only the rest goes inside quotes.
    if word == "~" {
        return word.to_string();
    }
    match word.strip_prefix("~/") {
        Some(rest) if safe(rest) => word.to_string(),
        Some(rest) => format!("~/{}", quoted(rest)),
        None => quoted(word),
    }
}

/// A resolved tool path as a human should read it, given the AppImage mount root (if any).
/// **Pure**, unit-tested.
///
/// A path INSIDE the bundle is reported relative to the bundle, written `AppImage://usr/…`.
/// Showing the real one would be worse than useless: the mount point is a different random
/// path on every launch, so the interesting half (`usr/bin/ffmpeg`, i.e. "the copy we
/// shipped") is buried under noise that changes each time the user looks. Everything else is
/// shown as it is, because for a plain install the full path IS the answer.
///
/// The `://` is deliberate, and reads as a scheme rather than a sentence. `AppImage: usr/…`
/// looked like a label followed by a path, which invites reading the two halves as separate
/// things and makes the result awkward to quote or paste. `AppImage://usr/bin/ffmpeg` is one
/// token that says "inside the bundle, at this path", the same way any URI does.
///
/// A FLATPAK gets the same treatment for the same reason, written `Flatpak://app/bin/tesseract`
/// (`lab/flatpak`). The two bundles differ in one detail that matters to the formatting: an
/// AppImage path is measured against a mount root, so what survives is already relative
/// (`usr/bin/ffmpeg`), while a Flatpak path is ABSOLUTE inside the sandbox (`/app/bin/…`).
/// Interpolating that directly would produce `Flatpak:///app/…` with three slashes, which
/// reads as a malformed URI, so the leading separator is dropped and the scheme keeps exactly
/// two.
///
/// Only paths from the bundle IMAGE are labelled, which on a Flatpak means `/app` (what we
/// ship) and `/usr` (the runtime we ship against). Anything else the sandbox can see is a real
/// host path the user recognises, and is shown as it is.
fn bundle_label(path: &Path, mount: Option<&Path>, flatpak: bool) -> String {
    if let Some(root) = mount
        && let Ok(rel) = path.strip_prefix(root)
        && !rel.as_os_str().is_empty()
    {
        return format!("AppImage://{}", rel.display());
    }
    if flatpak {
        let s = path.to_string_lossy();
        if s.starts_with("/app/") || s.starts_with("/usr/") {
            return format!("Flatpak://{}", s.trim_start_matches('/'));
        }
    }
    path.display().to_string()
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
        // Dev vendor dir: the checked-out repo's vendored arm64 sidecars.
        // `CARGO_MANIFEST_DIR` is baked at compile time, so it points at the repo for a
        // dev build and simply doesn't exist in a shipped bundle (harmless fall-through).
        //
        // The subdirectory is PER TOOL, and that is the fix for a real bug: this used to
        // be hardcoded to `vendor/ffmpeg/macos-arm64`, so `tesseract_path()` looked for
        // `vendor/ffmpeg/macos-arm64/tesseract`, never matched, and fell through to PATH.
        // A mac dev build therefore took ffmpeg from the pinned vendor dir but tesseract
        // from Homebrew, which is precisely the "different on the build machine" mismatch
        // the vendoring exists to remove, and it would have hidden a broken vendored
        // tesseract until someone opened a packaged build.
        candidates.push(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("vendor")
                .join(vendor_subdir(name))
                .join("macos-arm64")
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
    tool_location(tool).is_some()
}

/// WHERE a tool resolved by [`ffmpeg_path`]-style lookup actually sits on disk, or `None`
/// when it is nowhere: an absolute path is itself (when the file is there), a bare name is
/// the first `PATH` entry that has it. [`tool_available`] is written in terms of this, so
/// "it is available" and "here is the one we will run" can never disagree.
///
/// The result is deliberately NOT canonicalised. What the user needs to see is the binary
/// this app will actually spawn, symlinks and all: `/usr/bin/ffmpeg` is the honest answer
/// even when it points at `ffmpeg-7`. Resolving links would also break the AppImage prefix
/// match in [`bundle_label`] on any system that reaches its mount point through one.
pub fn tool_location(tool: &Path) -> Option<PathBuf> {
    if tool.is_absolute() {
        return tool.is_file().then(|| tool.to_path_buf());
    }
    tool_on_path(tool)
}

/// The full path to the bare tool `tool` in the first `PATH` directory that holds it, or
/// `None` when no entry does. The path-returning half of the `PATH` scan
/// [`tool_available`] has always done.
pub fn tool_on_path(tool: &Path) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|d| tool_in_dir(&d, tool))
}

/// The bare tool `tool` inside directory `d`, as the full path to it, if it is there. On
/// Windows the on-disk file carries `EXE_SUFFIX` (`ffmpeg.exe`) even though
/// `Command::new("ffmpeg")` resolves it without — so the bare `d/ffmpeg` never exists and a
/// PATH-only ffmpeg would wrongly read as "missing" (gating recording). Also probe
/// `d/ffmpeg{EXE_SUFFIX}` to match the runtime spawn (DRAGON-229). On Linux/macOS
/// `EXE_SUFFIX` is empty, so this is byte-identical to the plain `d.join(tool)`.
fn tool_in_dir(d: &Path, tool: &Path) -> Option<PathBuf> {
    let plain = d.join(tool);
    if plain.is_file() {
        return Some(plain);
    }
    let suffix = std::env::consts::EXE_SUFFIX;
    if !suffix.is_empty() {
        let mut name = tool.as_os_str().to_os_string();
        name.push(suffix);
        let suffixed = d.join(name);
        if suffixed.is_file() {
            return Some(suffixed);
        }
    }
    None
}

/// Where an external tool ACTUALLY resolved, written for a human to read, or `None` when the
/// tool is not on this machine at all. Takes whatever [`ffmpeg_path`] and friends returned
/// (an absolute path or a bare name) and reports the file that will really be spawned, with
/// a path inside a bundle named for that bundle (see [`bundle_label`]).
pub fn tool_location_label(tool: &Path) -> Option<String> {
    Some(bundle_label(
        &tool_location(tool)?,
        appimage_dir().as_deref(),
        flatpak_sandboxed(),
    ))
}

/// One external tool and where it resolved: `location` is `None` when it is not installed.
pub struct ToolLocation {
    /// The tool as the user knows it, which is also the name of its Health-page row.
    pub name: &'static str,
    /// Its [`tool_location_label`], or `None` when nothing on this machine matched.
    pub location: Option<String>,
}

/// Every external binary this app can run, with where each one resolved. ONE producer for
/// both surfaces that report this (the Health page's rows and the debug log's session
/// header), so the two can never tell a customer different stories.
///
/// Resolved once per process and cached: each entry costs a handful of `stat`s and, for a
/// tool that is not a bundled sidecar, a `PATH` scan, while the Health page re-derives its
/// rows on every frame that draws the nav health icon. Nothing here can change under a
/// running process anyway.
pub fn tool_locations() -> &'static [ToolLocation] {
    static CACHE: std::sync::OnceLock<Vec<ToolLocation>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        // `mut` only where something is pushed below: pactl is a Linux row, so mac and
        // Windows build this list once and never add to it.
        #[cfg_attr(any(target_os = "macos", windows), allow(unused_mut))]
        let mut tools = vec![
            ToolLocation { name: "ffmpeg", location: tool_location_label(&ffmpeg_path()) },
            ToolLocation { name: "ffprobe", location: tool_location_label(&ffprobe_path()) },
            ToolLocation { name: "tesseract", location: tool_location_label(&tesseract_path()) },
        ];
        // Linux only: there is no pactl on macOS or Windows, where audio devices come from
        // avfoundation / DirectShow through ffmpeg, which is already the first row here.
        #[cfg(not(any(target_os = "macos", windows)))]
        tools.push(ToolLocation {
            name: "pactl",
            location: crate::audio::devices::pactl_path()
                .as_deref()
                .and_then(tool_location_label),
        });
        tools
    })
}

/// One external tool and the version its binary REPORTS: `version` is `None` when the tool
/// is missing or its output carried no recognisable version. Same `name` vocabulary as
/// [`ToolLocation`], so the Health page can join the two by name.
#[derive(Debug, Clone)]
pub struct ToolVersion {
    /// The tool as the user knows it, matching its [`ToolLocation`] entry.
    pub name: &'static str,
    /// The bare version its `-version` / `--version` output reported ("8.1.2"), or `None`.
    pub version: Option<String>,
}

/// How long one version probe may run before its child is killed. These commands answer in
/// milliseconds; the bound exists for the wedged case (DRAGON-118: nothing waits unboundedly,
/// and a hung probe would otherwise leak its detached worker thread for the process lifetime).
const VERSION_PROBE_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// The version each external binary reports, for the Health page's rows (DRAGON-564). The
/// same tool set as [`tool_locations`], resolved through the same `*_path()` lookups, so the
/// version describes the binary the app actually spawns, env overrides included.
///
/// BLOCKING: spawns each tool once (bounded, [`VERSION_PROBE_BUDGET`] apiece). Call it off
/// the UI thread (`app::off_thread`); the settings window kicks it when it opens and the
/// result arrives as a message. Deliberately NOT cached here: the one caller already caches
/// the result for the process lifetime, and a second cache could only disagree with it.
pub fn probe_tool_versions() -> Vec<ToolVersion> {
    // `mut` only where something is pushed below: pactl is a Linux row, so mac and
    // Windows build this list once and never add to it (mirrors `tool_locations`).
    #[cfg_attr(any(target_os = "macos", windows), allow(unused_mut))]
    let mut tools = vec![
        ToolVersion {
            name: "ffmpeg",
            version: probe_version(&ffmpeg_path(), "-version", ffmpeg_version_line),
        },
        ToolVersion {
            name: "ffprobe",
            version: probe_version(&ffprobe_path(), "-version", ffmpeg_version_line),
        },
        ToolVersion {
            name: "tesseract",
            version: probe_version(&tesseract_path(), "--version", name_version_line),
        },
    ];
    #[cfg(not(any(target_os = "macos", windows)))]
    tools.push(ToolVersion {
        name: "pactl",
        version: crate::audio::devices::pactl_path()
            .and_then(|p| probe_version(&p, "--version", name_version_line)),
    });
    tools
}

/// Spawn `tool <flag>` and parse the version out of the FIRST non-empty line of its output,
/// stdout first with stderr as the fallback (tesseract releases before 5 print their version
/// banner to stderr). `None` on any failure: a missing tool, a killed child, a non-zero-exit
/// crash that printed nothing usable, or output no parser recognises.
///
/// BOUNDED, modelled on `update::run_bounded` (DRAGON-118). A separate copy rather than a
/// call, for the reason that one gives for not calling ffmpeg's: it carries its domain with
/// it (its kill warning names the update flow) and it DROPS stderr, which this probe needs.
/// Reader threads drain both pipes while the reap loop polls, so a chatty tool can never
/// fill a pipe buffer and stall itself into the deadline. Logs at debug only, and never a
/// path (the diag privacy rules).
fn probe_version(tool: &Path, flag: &str, parse: fn(&str) -> Option<String>) -> Option<String> {
    let mut child = quiet_command(tool)
        .arg(flag)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take().map(drain_thread);
    let stderr = child.stderr.take().map(drain_thread);
    let deadline = std::time::Instant::now() + VERSION_PROBE_BUDGET;
    let exited = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) => {}
            Err(_) => break false,
        }
        if std::time::Instant::now() >= deadline {
            log::debug!(
                "version probe: a tool outlived its {}s budget and was stopped",
                VERSION_PROBE_BUDGET.as_secs()
            );
            let _ = child.kill();
            let _ = child.wait();
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    // Joined AFTER the child is gone, so every read has hit EOF and no join can block.
    let stdout = stdout.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = stderr.and_then(|h| h.join().ok()).unwrap_or_default();
    if !exited {
        return None;
    }
    first_line(&stdout)
        .and_then(|l| parse(&l))
        .or_else(|| first_line(&stderr).and_then(|l| parse(&l)))
}

/// Drain one child pipe to EOF on its own thread (so the reap loop never blocks on I/O and
/// the child never blocks on a full pipe), handing the bytes back through the join.
fn drain_thread<R: std::io::Read + Send + 'static>(mut pipe: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

/// Pure: the first non-empty line of a tool's output, trimmed, decoded lossily.
fn first_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// Pure: the bare version out of an `ffmpeg -version` / `ffprobe -version` first line
/// ("ffmpeg version 8.1.2 Copyright (c) ..."), or `None` when the line carries none.
/// The token after the literal `version` is the version; unit-tested
/// (`ffmpeg_version_line_tests`), including the git-build and garbage lines that must
/// answer `None` so the Health row shows the name alone.
fn ffmpeg_version_line(line: &str) -> Option<String> {
    let mut tokens = line.split_whitespace();
    tokens.by_ref().find(|t| *t == "version")?;
    bare_version(tokens.next()?)
}

/// Pure: the bare version out of a `tesseract --version` / `pactl --version` first line
/// ("tesseract 5.5.3", "pactl 17.0"): the token after the tool's own name; unit-tested
/// (`name_version_line_tests`), including garbage lines that must answer `None`.
fn name_version_line(line: &str) -> Option<String> {
    let mut tokens = line.split_whitespace();
    let _name = tokens.next()?;
    bare_version(tokens.next()?)
}

/// Pure: reduce one version token to its bare numeric form, or `None` when it has none.
/// Strips a single packaging prefix ("n8.1.2" on FFmpeg's own release builds, "v5.3.0"
/// on Windows tesseract packages), then keeps the leading digits-and-dots run, so a
/// distro suffix ("6.1.1-3ubuntu5") falls away. A token with no leading digit (a git
/// build's "N-109802-g7c2f9a6c4e", prose) is not a version. Unit-tested through the two
/// line parsers above.
fn bare_version(token: &str) -> Option<String> {
    let t = token
        .strip_prefix(['n', 'v'])
        .filter(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or(token);
    let end = t
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(t.len());
    let bare = t[..end].trim_end_matches('.');
    bare.starts_with(|c: char| c.is_ascii_digit())
        .then(|| bare.to_string())
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

/// [`expand_tilde`]'s inverse: collapse a leading home directory back to `~`, for a path we
/// are about to SHOW.
///
/// The reason is privacy rather than tidiness (owner call). Settings screens get screenshotted
/// into bug reports, pasted into chats, and shared on calls, and a full path leaks the account
/// name to everyone who sees it. `~/Capture` says everything the reader needs and nothing they
/// do not.
///
/// It stays a valid, pasteable path, which is why this is a safe default rather than a
/// trade-off: a shell expands `~` itself, so a copied string still works in a terminal, and the
/// [`expand_tilde`] above is what reads it back if it ever comes to us as input.
///
/// Only an exact home PREFIX collapses. A path that merely starts with the same characters
/// (`/home/frost-backup` beside `/home/frost`) is left alone, because it is a different
/// directory and rewriting it would be a lie rather than an abbreviation.
pub fn collapse_home(path: &str) -> String {
    collapse_home_from(path, dirs::home_dir().as_deref())
}

/// **Pure**, unit-tested: [`collapse_home`]'s decision, with the home directory injected.
pub fn collapse_home_from(path: &str, home: Option<&Path>) -> String {
    let Some(home) = home.map(|h| h.to_string_lossy().into_owned()) else {
        return path.to_string();
    };
    // An empty or root home would match everything; nothing sane to collapse against.
    if home.is_empty() || home == "/" {
        return path.to_string();
    }
    if path == home {
        return "~".to_string();
    }
    // The separator has to be part of the match, or `/home/frost-backup` would collapse
    // against `/home/frost` and name a directory the user does not have.
    let with_sep = format!("{home}/");
    match path.strip_prefix(&with_sep) {
        Some(rest) => format!("~/{rest}"),
        None => path.to_string(),
    }
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

    /// `tool_in_dir` must find a bare tool name whose on-disk file carries the platform
    /// `EXE_SUFFIX` — on Windows the file is `ffmpeg.exe` but `Command::new("ffmpeg")`
    /// resolves it without, so a bare-only check wrongly reports it missing and gates
    /// recording (DRAGON-229). On Linux/macOS (empty suffix) this is the plain bare match.
    /// It also returns the file it matched, which is what the Health page reports.
    #[test]
    fn tool_in_dir_matches_exe_suffixed_bare_name() {
        let dir = std::env::temp_dir().join(format!("cck-toolcheck-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mk temp dir");
        // The on-disk file as the OS ships it (bare on unix, `.exe` on windows).
        let disk = dir.join(format!("cck-fake-tool{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&disk, b"x").expect("write fake tool");
        assert_eq!(
            tool_in_dir(&dir, Path::new("cck-fake-tool")),
            Some(disk.clone()),
            "the bare tool name must resolve to its EXE_SUFFIX file on disk"
        );
        assert_eq!(
            tool_in_dir(&dir, Path::new("cck-absent-tool")),
            None,
            "an absent tool is not found"
        );
        let _ = std::fs::remove_file(&disk);
        let _ = std::fs::remove_dir(&dir);
    }

    /// An absolute tool path reports itself when the file is there, and nothing when it is
    /// not: the Health page must never claim a location for a binary that has been removed.
    #[test]
    fn tool_location_answers_for_an_absolute_path() {
        let dir = std::env::temp_dir().join(format!("cck-toolloc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mk temp dir");
        let disk = dir.join("cck-fake-tool");
        std::fs::write(&disk, b"x").expect("write fake tool");
        assert_eq!(tool_location(&disk), Some(disk.clone()));
        assert!(tool_available(&disk));
        let gone = dir.join("cck-absent-tool");
        assert_eq!(tool_location(&gone), None);
        assert!(!tool_available(&gone));
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

/// How a resolved tool path is reported to a human (the Health page's rows, the debug log's
/// session header). The AppImage arm can only ever run on Linux, but the rule is pure and is
/// pinned here on every platform, because it is the half that cannot be checked by running
/// the app on the machine you are developing on.
#[cfg(test)]
mod tool_location_label_tests {
    use super::*;
    use std::ffi::OsStr;

    /// Off an AppImage the full path IS the answer, and it is passed through untouched: no
    /// tilde, no shortening, no canonicalising away the symlink the user's distro installed.
    #[test]
    fn a_plain_path_is_shown_as_it_is() {
        for p in [
            "/usr/bin/ffmpeg",
            "/opt/homebrew/bin/tesseract",
            "/Applications/Cosmic Capture Kit.app/Contents/Resources/ffprobe",
        ] {
            assert_eq!(bundle_label(Path::new(p), None, false), p);
        }
    }

    /// A tool INSIDE the running bundle is named relative to the bundle. The mount root is a
    /// fresh random directory every launch, so the raw path would be different noise each
    /// time the user opened the page; `usr/bin/ffmpeg` is the durable fact.
    #[test]
    fn a_tool_inside_the_bundle_is_named_relative_to_it() {
        let mount = Path::new("/tmp/.mount_Cosmic7fA2xQ");
        assert_eq!(
            bundle_label(Path::new("/tmp/.mount_Cosmic7fA2xQ/usr/bin/ffmpeg"), Some(mount), false),
            "AppImage://usr/bin/ffmpeg"
        );
        assert_eq!(
            bundle_label(Path::new("/tmp/.mount_Cosmic7fA2xQ/usr/bin/tesseract"), Some(mount), false),
            "AppImage://usr/bin/tesseract"
        );
    }

    /// The label is written as a SCHEME, `AppImage://usr/…`, not as a sentence. Pinned
    /// because it is user-visible text that gets pasted into support threads, and because
    /// the earlier `AppImage: usr/…` form read as a label followed by a separate path.
    #[test]
    fn the_bundle_label_reads_as_a_scheme() {
        let mount = Path::new("/tmp/.mount_Cosmic7fA2xQ");
        let out = bundle_label(Path::new("/tmp/.mount_Cosmic7fA2xQ/usr/bin/ffmpeg"), Some(mount), false);
        assert!(out.starts_with("AppImage://"), "must carry the scheme prefix: {out}");
        assert!(!out.contains("AppImage: "), "the old label-and-path form must not come back");
        // One token, so a double-click or a shell paste takes the whole thing.
        assert!(!out.contains(' '), "the label must not contain a space: {out}");
    }

    /// A tool OUTSIDE the bundle keeps its real path even while running from an AppImage.
    /// That is the interesting case: it is how the user sees that the distro's ffmpeg is in
    /// use rather than the one we shipped.
    #[test]
    fn a_system_tool_keeps_its_path_inside_an_appimage() {
        let mount = Path::new("/tmp/.mount_Cosmic7fA2xQ");
        assert_eq!(bundle_label(Path::new("/usr/bin/ffmpeg"), Some(mount), false), "/usr/bin/ffmpeg");
        // The mount root itself has no relative part to show, so it stays a path rather than
        // becoming a bare "AppImage://".
        assert_eq!(bundle_label(mount, Some(mount), false), "/tmp/.mount_Cosmic7fA2xQ");
    }

    /// Both runtime variables are required, and empty counts as absent. `$APPDIR` alone is a
    /// generic enough name that something else could have exported it, and labelling a path
    /// "AppImage" over it would mislead rather than explain.
    #[test]
    fn the_mount_root_needs_both_runtime_variables() {
        let img = OsStr::new("/home/u/Apps/CosmicCaptureKit.AppImage");
        let dir = OsStr::new("/tmp/.mount_Cosmic7fA2xQ");
        assert_eq!(
            appimage_dir_from(Some(img), Some(dir), None),
            Some(PathBuf::from("/tmp/.mount_Cosmic7fA2xQ"))
        );
        assert_eq!(appimage_dir_from(None, Some(dir), None), None, "$APPDIR alone proves nothing");
        assert_eq!(
            appimage_dir_from(Some(img), None, None),
            None,
            "no $APPDIR and no exe to derive one from leaves nothing to measure against"
        );
        assert_eq!(
            appimage_dir_from(Some(OsStr::new("")), Some(dir), None),
            None,
            "empty is absent"
        );
        assert_eq!(
            appimage_dir_from(Some(img), Some(OsStr::new("")), None),
            None,
            "empty is absent"
        );
        assert_eq!(appimage_dir_from(None, None, None), None, "every other kind of launch");
    }

    /// The end-to-end label for a tool this machine really has. Nothing here can assert a
    /// specific path (it differs per machine), so it pins the contract: a tool that resolves
    /// gets a non-empty label, one that does not gets nothing at all.
    #[test]
    fn a_missing_tool_has_no_location_at_all() {
        assert_eq!(tool_location_label(Path::new("cck-tool-that-does-not-exist")), None);
        assert_eq!(tool_location_label(Path::new("/nonexistent/cck-tool")), None);
        let me = std::env::current_exe().expect("test exe path");
        assert_eq!(tool_location_label(&me), Some(me.display().to_string()));
    }

    /// The list both surfaces read is the same list, and every entry is named after the
    /// binary it describes (which is what lets the Health page look one up by row name).
    #[test]
    fn the_shared_tool_list_names_every_binary_we_report() {
        let names: Vec<&str> = tool_locations().iter().map(|t| t.name).collect();
        assert!(names.contains(&"ffmpeg"), "{names:?}");
        assert!(names.contains(&"ffprobe"), "{names:?}");
        assert!(names.contains(&"tesseract"), "{names:?}");
        #[cfg(not(any(target_os = "macos", windows)))]
        assert!(names.contains(&"pactl"), "{names:?}");
        // Cached: the same slice comes back, so a per-frame Health page pays once.
        assert_eq!(tool_locations().as_ptr(), tool_locations().as_ptr());
    }
}

/// DRAGON-564: the `ffmpeg -version` / `ffprobe -version` first-line parser, pinned
/// against the real formats those builds print. Anything unrecognisable must answer
/// `None`, because the Health row's fallback is the name alone, never a wrong version.
#[cfg(test)]
mod ffmpeg_version_line_tests {
    use super::*;

    #[test]
    fn a_release_ffmpeg_line_yields_the_bare_version() {
        assert_eq!(
            ffmpeg_version_line(
                "ffmpeg version 8.1.2 Copyright (c) 2000-2025 the FFmpeg developers"
            ),
            Some("8.1.2".to_string())
        );
    }

    #[test]
    fn an_ffprobe_line_parses_the_same_way() {
        assert_eq!(
            ffmpeg_version_line(
                "ffprobe version 8.1.2 Copyright (c) 2007-2025 the FFmpeg developers"
            ),
            Some("8.1.2".to_string())
        );
    }

    #[test]
    fn the_n_packaging_prefix_and_git_suffix_fall_away() {
        // FFmpeg's own release builds tag the version "n8.1.2"; a point build appends
        // the commit ("-2-g8e14e5cd6a"). Both are packaging, not version.
        assert_eq!(
            ffmpeg_version_line(
                "ffmpeg version n8.1.2-2-g8e14e5cd6a Copyright (c) 2000-2025 the FFmpeg developers"
            ),
            Some("8.1.2".to_string())
        );
    }

    #[test]
    fn a_distro_suffix_falls_away() {
        assert_eq!(
            ffmpeg_version_line(
                "ffmpeg version 6.1.1-3ubuntu5 Copyright (c) 2000-2024 the FFmpeg developers"
            ),
            Some("6.1.1".to_string())
        );
    }

    #[test]
    fn a_git_master_build_has_no_version_to_show() {
        // "N-<count>-g<hash>" carries no release number; the row shows the name alone.
        assert_eq!(
            ffmpeg_version_line(
                "ffmpeg version N-109802-g7c2f9a6c4e Copyright (c) 2000-2023 the FFmpeg developers"
            ),
            None
        );
    }

    #[test]
    fn garbage_lines_answer_none() {
        assert_eq!(ffmpeg_version_line("bash: ffmpeg: command not found"), None);
        assert_eq!(ffmpeg_version_line("ffmpeg version"), None);
        assert_eq!(ffmpeg_version_line(""), None);
    }
}

/// DRAGON-564: the `tesseract --version` / `pactl --version` first-line parser ("name
/// version"), pinned against the real formats, old-tesseract and Windows shapes included.
#[cfg(test)]
mod name_version_line_tests {
    use super::*;

    #[test]
    fn a_modern_tesseract_line_yields_the_bare_version() {
        assert_eq!(name_version_line("tesseract 5.5.3"), Some("5.5.3".to_string()));
    }

    #[test]
    fn an_old_tesseract_stderr_banner_parses_too() {
        // Before 5.0 the banner went to stderr; the probe falls back to it, same shape.
        assert_eq!(name_version_line("tesseract 4.1.1"), Some("4.1.1".to_string()));
    }

    #[test]
    fn the_windows_v_prefix_falls_away() {
        assert_eq!(
            name_version_line("tesseract v5.3.0.20221214"),
            Some("5.3.0.20221214".to_string())
        );
    }

    #[test]
    fn a_pactl_line_yields_the_bare_version() {
        assert_eq!(name_version_line("pactl 17.0"), Some("17.0".to_string()));
    }

    #[test]
    fn garbage_lines_answer_none() {
        assert_eq!(name_version_line("Usage: tesseract --help | --version"), None);
        assert_eq!(name_version_line("no such file or directory"), None);
        assert_eq!(name_version_line("tesseract"), None);
        assert_eq!(name_version_line(""), None);
    }
}

/// DRAGON-527: where OCR language data comes from, which depends on WHICH tesseract
/// we resolved. Getting this backwards is a regression the user experiences as "all my
/// language packs vanished", so both directions are pinned.
#[cfg(test)]
mod tessdata_dir_tests {
    use super::*;

    /// A bundled tesseract (an absolute path: the mac `Resources/` sidecar, the
    /// exe-adjacent Windows/AppImage sidecar) ships only `eng` and has no system data to
    /// fall back on, so it gets our writable directory.
    #[test]
    fn a_bundled_tesseract_gets_the_writable_dir() {
        let writable = PathBuf::from("/home/u/.config/cosmic-capture-kit/tessdata");
        for bundled in [
            "/Applications/Cosmic Capture Kit.app/Contents/Resources/tesseract",
            "/opt/cck/tesseract",
            "/tmp/.mount_abc/usr/bin/tesseract",
        ] {
            assert_eq!(
                tessdata_dir_for(Path::new(bundled), Some(writable.clone())),
                Some(writable.clone()),
                "{bundled} is a sidecar and should use our own language dir"
            );
        }
    }

    /// The bare name is `locate_tool`'s "nothing on disk matched, let the OS resolve it on
    /// PATH" signal, i.e. the distro's own tesseract. Its packs live in `/usr/share/tessdata`
    /// and `tesseract-data-deu` put them there, so we must say NOTHING and leave them visible.
    #[test]
    fn a_path_tesseract_is_left_alone() {
        let writable = PathBuf::from("/home/u/.config/cosmic-capture-kit/tessdata");
        assert_eq!(tessdata_dir_for(Path::new("tesseract"), Some(writable)), None);
        assert_eq!(tessdata_dir_for(Path::new("tesseract.exe"), None), None);
    }

    /// No writable location resolved (no home dir) degrades to "say nothing" rather than
    /// inventing a path. Tesseract's own default is a better answer than a broken flag.
    #[test]
    fn no_writable_dir_means_no_flag() {
        assert_eq!(tessdata_dir_for(Path::new("/opt/cck/tesseract"), None), None);
    }
}

/// DRAGON-531: seeding must be able to ship an updated language pack WITHOUT ever
/// overwriting one the user installed themselves.
#[cfg(test)]
mod seed_verdict_tests {
    use super::{SeedVerdict, seed_verdict};

    /// Nothing on disk yet: the first launch of a packaged build.
    #[test]
    fn a_missing_file_is_written() {
        assert_eq!(seed_verdict(None, 4_000_000, None), SeedVerdict::Write);
    }

    /// A file we have no record of writing belongs to the user. This is the case that
    /// protects someone who dropped a pack in before ever running a bundled build.
    #[test]
    fn an_unrecorded_file_belongs_to_the_user() {
        assert_eq!(
            seed_verdict(Some(15_000_000), 4_000_000, None),
            SeedVerdict::UserOwned
        );
    }

    /// The user swapped our tessdata_fast eng for tessdata_best. On disk no longer
    /// matches what we recorded writing, so it is theirs now and stays.
    #[test]
    fn a_replaced_file_belongs_to_the_user() {
        assert_eq!(
            seed_verdict(Some(15_000_000), 4_000_000, Some(4_000_000)),
            SeedVerdict::UserOwned
        );
    }

    /// Ours, and the bundled pack has not changed: the common path, and it must not
    /// rewrite tens of megabytes on every one-shot launch.
    #[test]
    fn our_own_current_file_is_left_alone() {
        assert_eq!(
            seed_verdict(Some(4_000_000), 4_000_000, Some(4_000_000)),
            SeedVerdict::UpToDate
        );
    }

    /// THE BUG THIS FIXES: still ours, but the release ships a newer pack. The old rule
    /// was "never overwrite", so an updated eng.traineddata could never reach anyone who
    /// had already launched the previous build.
    #[test]
    fn our_own_stale_file_is_updated() {
        assert_eq!(
            seed_verdict(Some(4_000_000), 4_200_000, Some(4_000_000)),
            SeedVerdict::Write
        );
    }
}

/// DRAGON-510: which path this process re-executes ITSELF from.
#[cfg(test)]
mod re_exec_path_tests {
    use super::re_exec_path;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    /// The ordinary launch: no AppImage, so the answer is exactly `current_exe()` and
    /// every existing spawn site behaves as it always has.
    #[test]
    fn without_an_appimage_it_is_the_running_binary() {
        assert_eq!(
            re_exec_path(None, Path::new("/home/u/repo/target/release/cosmic-capture-kit")),
            PathBuf::from("/home/u/repo/target/release/cosmic-capture-kit")
        );
    }

    /// THE CASE THIS EXISTS FOR: `current_exe()` is a FUSE mount that dies with the
    /// process holding it, and this app detaches children that outlive their parent. The
    /// `.AppImage` file is the only path that is still there afterwards.
    #[test]
    fn inside_an_appimage_it_is_the_appimage_file() {
        assert_eq!(
            re_exec_path(
                Some(OsStr::new("/home/u/Apps/CosmicCaptureKit-0.28.1-x86_64.AppImage")),
                Path::new("/tmp/.mount_Cosmic1a2b3c/usr/bin/cosmic-capture-kit"),
            ),
            PathBuf::from("/home/u/Apps/CosmicCaptureKit-0.28.1-x86_64.AppImage")
        );
    }

    /// An empty value is treated as absent. Spawning `""` would fail with a bare
    /// "No such file or directory" that names nothing and points nowhere.
    #[test]
    fn an_empty_appimage_variable_falls_back() {
        assert_eq!(
            re_exec_path(Some(OsStr::new("")), Path::new("/usr/bin/cosmic-capture-kit")),
            PathBuf::from("/usr/bin/cosmic-capture-kit")
        );
    }
}

/// DRAGON-532: whether this launch has an `.AppImage` file behind it, which is what decides
/// both the manifest key it reads and whether a one-click update is offered at all.
#[cfg(test)]
mod appimage_from_tests {
    use super::appimage_from;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    /// Every launch that is not an AppImage: a source build, the release binary, the `.app`,
    /// the MSI install. There is no file we own, so the updater must not offer to swap one.
    #[test]
    fn without_the_variable_there_is_no_appimage() {
        assert_eq!(appimage_from(None), None);
    }

    #[test]
    fn with_the_variable_it_is_the_appimage_file() {
        assert_eq!(
            appimage_from(Some(OsStr::new(
                "/home/u/Apps/CosmicCaptureKit-0.28.1-x86_64.AppImage"
            ))),
            Some(PathBuf::from(
                "/home/u/Apps/CosmicCaptureKit-0.28.1-x86_64.AppImage"
            ))
        );
    }

    /// The name is NOT parsed, anywhere. A user who renamed the download still gets updates,
    /// and the installer writes back to whatever they called it (DRAGON-532: the swap keeps
    /// the filename so autostart entries and capture shortcuts survive it).
    #[test]
    fn a_renamed_appimage_is_still_an_appimage() {
        assert_eq!(
            appimage_from(Some(OsStr::new("/home/u/bin/screenshot-tool"))),
            Some(PathBuf::from("/home/u/bin/screenshot-tool"))
        );
    }

    /// Empty means absent, and here that is load-bearing beyond the spawn case: an installer
    /// that took `""` as its target would swap a file the user never named.
    #[test]
    fn an_empty_variable_is_absent() {
        assert_eq!(appimage_from(Some(OsStr::new(""))), None);
    }
}

/// DRAGON-531: which vendor directory each macOS sidecar comes from.
#[cfg(test)]
mod vendor_subdir_tests {
    use super::vendor_subdir;

    /// ffprobe and ffplay ship inside the ffmpeg archive, so they share its directory.
    /// Giving them their own would send the lookup somewhere nothing is ever vendored.
    #[test]
    fn the_ffmpeg_family_shares_one_directory() {
        assert_eq!(vendor_subdir("ffmpeg"), "ffmpeg");
        assert_eq!(vendor_subdir("ffprobe"), "ffmpeg");
        assert_eq!(vendor_subdir("ffplay"), "ffmpeg");
    }

    /// THE BUG THIS FIXES: the directory was hardcoded to ffmpeg's, so a mac dev build
    /// looked for `vendor/ffmpeg/macos-arm64/tesseract`, never found it, and silently
    /// used Homebrew's tesseract while taking ffmpeg from the pinned vendor dir.
    #[test]
    fn tesseract_has_its_own_directory() {
        assert_eq!(vendor_subdir("tesseract"), "tesseract");
    }

    /// A tool nobody has vendored yet resolves to its own name rather than to ffmpeg's
    /// directory, so adding one needs no change here.
    #[test]
    fn an_unknown_tool_uses_its_own_name() {
        assert_eq!(vendor_subdir("magick"), "magick");
    }
}

/// DRAGON-531: seeding must mirror the WHOLE tessdata tree.
#[cfg(test)]
mod collect_files_tests {
    use super::collect_files;

    /// The case that was broken: `configs/tsv` is a real file tesseract loads, and a
    /// walk that only saw the top level would miss it, leaving OCR silently empty.
    #[test]
    fn it_finds_files_in_subdirectories() {
        let root = std::env::temp_dir().join(format!("cck-collect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("configs")).expect("mk configs");
        std::fs::write(root.join("eng.traineddata"), b"lang").expect("write lang");
        std::fs::write(root.join("configs/tsv"), b"tessedit_create_tsv 1").expect("write cfg");

        let mut got: Vec<String> = collect_files(&root).into_iter().map(|(r, _, _)| r).collect();
        got.sort();
        assert_eq!(got, vec!["configs/tsv".to_string(), "eng.traineddata".to_string()]);

        // The length travels with it, since the ownership rule compares on size.
        let cfg = collect_files(&root)
            .into_iter()
            .find(|(r, _, _)| r == "configs/tsv")
            .expect("configs/tsv present");
        assert_eq!(cfg.2, 21);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A missing directory is not an error: a dev build pointing at a system tesseract
    /// has no bundled tessdata at all, and that must not be noisy.
    #[test]
    fn a_missing_root_yields_nothing() {
        assert!(collect_files(std::path::Path::new("/nonexistent-cck-tessdata")).is_empty());
    }
}

/// The Flatpak sandbox seam (`lab/flatpak`).
#[cfg(test)]
mod flatpak_sandbox_tests {
    use super::{flatpak_sandboxed_from, host_config_dir_from};
    use std::path::PathBuf;

    #[test]
    fn the_marker_file_is_the_sandbox_test() {
        assert!(flatpak_sandboxed_from(true));
        assert!(!flatpak_sandboxed_from(false));
    }

    /// Outside a sandbox the config dir is untouched, whatever else is set. This is the case
    /// every existing user is in, so it must be exactly the old behaviour.
    #[test]
    fn outside_a_sandbox_the_config_dir_is_unchanged() {
        let got = host_config_dir_from(
            false,
            Some(PathBuf::from("/host/xdg")),
            Some(PathBuf::from("/home/u")),
            Some(PathBuf::from("/home/u/.config")),
        );
        assert_eq!(got, Some(PathBuf::from("/home/u/.config")));
    }

    /// Inside a sandbox `dirs::config_dir()` points at the app's PRIVATE store, which the
    /// desktop never writes to, so it must not win. `$HOME/.config` is where the host's config
    /// is bind-mounted.
    #[test]
    fn inside_a_sandbox_home_config_beats_the_private_store() {
        let got = host_config_dir_from(
            true,
            None,
            Some(PathBuf::from("/home/u")),
            Some(PathBuf::from("/home/u/.var/app/dev.thedragon.CosmicCaptureKit/config")),
        );
        assert_eq!(got, Some(PathBuf::from("/home/u/.config")));
    }

    /// When the HOST's own XDG_CONFIG_HOME is non-default, Flatpak passes it through and it is
    /// the only correct answer: the user's config genuinely is not at ~/.config.
    #[test]
    fn host_xdg_config_home_wins_when_flatpak_provides_it() {
        let got = host_config_dir_from(
            true,
            Some(PathBuf::from("/host/xdg")),
            Some(PathBuf::from("/home/u")),
            Some(PathBuf::from("/home/u/.var/app/x/config")),
        );
        assert_eq!(got, Some(PathBuf::from("/host/xdg")));
    }

    /// Total even with nothing to go on, so a caller falls through to its own defaults rather
    /// than failing. That is the same outcome as a machine with no COSMIC config at all.
    #[test]
    fn it_stays_total_when_nothing_resolves() {
        assert_eq!(host_config_dir_from(true, None, None, None), None);
    }
}

#[cfg(test)]
mod session_bus_policy_tests {
    use super::session_bus_talk_allowed;

    /// The real `/.flatpak-info` of the `lab/flatpak` build, trimmed to the sections that
    /// matter. Copied from a live sandbox, so the parser is proven against the actual format
    /// rather than an invented one.
    const LIVE: &str = "\
[Application]
name=dev.thedragon.CosmicCaptureKit

[Context]
shared=ipc;network;
sockets=pulseaudio;wayland;

[Session Bus Policy]
org.kde.StatusNotifierWatcher=talk
com.system76.CosmicStatusNotifierWatcher=talk
org.freedesktop.secrets=talk
com.system76.CosmicSettingsDaemon=talk

[Environment]
ALSA_CONFIG_DIR=/usr/share/alsa
";

    /// No policy file means no policy applies: an ordinary launch keeps every bus feature,
    /// which is what makes this safe to gate real features on.
    #[test]
    fn no_policy_means_everything_is_reachable() {
        assert!(session_bus_talk_allowed(None, "org.freedesktop.FileManager1"));
        assert!(session_bus_talk_allowed(None, "org.mpris.MediaPlayer2.*"));
    }

    /// The two names DRAGON-555 and DRAGON-556 turn on, against the policy that is really
    /// shipped: neither is granted, so both features must know they are dead.
    #[test]
    fn the_live_policy_grants_neither_mpris_nor_the_file_manager() {
        assert!(!session_bus_talk_allowed(Some(LIVE), "org.mpris.MediaPlayer2.*"));
        assert!(!session_bus_talk_allowed(Some(LIVE), "org.freedesktop.FileManager1"));
    }

    /// What the policy DOES grant still reads as granted, so the parser is not simply
    /// answering "no" to everything.
    #[test]
    fn the_live_policy_grants_the_names_the_manifest_asked_for() {
        assert!(session_bus_talk_allowed(Some(LIVE), "org.freedesktop.secrets"));
        assert!(session_bus_talk_allowed(Some(LIVE), "org.kde.StatusNotifierWatcher"));
    }

    /// A `--talk-name` added to the manifest heals the feature with no code change. This is
    /// the whole reason the guard reads the POLICY rather than testing for a sandbox.
    #[test]
    fn granting_the_family_makes_it_reachable() {
        let granted = "[Session Bus Policy]\norg.mpris.MediaPlayer2.*=talk\n";
        assert!(session_bus_talk_allowed(Some(granted), "org.mpris.MediaPlayer2.*"));
    }

    /// One player granted by name is enough to ask the family question: pausing whatever we
    /// can reach is the feature, and it can do that.
    #[test]
    fn one_named_player_answers_the_family_question() {
        let granted = "[Session Bus Policy]\norg.mpris.MediaPlayer2.spotify=talk\n";
        assert!(session_bus_talk_allowed(Some(granted), "org.mpris.MediaPlayer2.*"));
    }

    /// A wildcard ABOVE the family covers it too.
    #[test]
    fn a_broader_wildcard_covers_the_family() {
        let granted = "[Session Bus Policy]\norg.mpris.*=talk\n";
        assert!(session_bus_talk_allowed(Some(granted), "org.mpris.MediaPlayer2.*"));
    }

    /// `see` is not enough: it lets a process observe that a name exists and nothing more,
    /// while every caller here wants to CALL a method on it.
    #[test]
    fn see_is_not_talk() {
        let seen = "[Session Bus Policy]\norg.freedesktop.FileManager1=see\n";
        assert!(!session_bus_talk_allowed(Some(seen), "org.freedesktop.FileManager1"));
        let owned = "[Session Bus Policy]\norg.freedesktop.FileManager1=own\n";
        assert!(session_bus_talk_allowed(Some(owned), "org.freedesktop.FileManager1"));
    }

    /// A grant only covers what sits at or below it, so a near-miss prefix is still a no.
    /// Without the dot boundary `org.mpris.MediaPlayer2Fake` would answer for the real one.
    #[test]
    fn a_prefix_only_matches_on_a_dot_boundary() {
        let granted = "[Session Bus Policy]\norg.mpris.MediaPlayer2Fake.*=talk\n";
        assert!(!session_bus_talk_allowed(Some(granted), "org.mpris.MediaPlayer2.*"));
    }

    /// Entries outside the section are not policy, however much they look like it. The
    /// `[Environment]` block is full of `key=value` lines.
    #[test]
    fn only_the_session_bus_policy_section_counts() {
        let elsewhere = "\
[System Bus Policy]
org.freedesktop.FileManager1=talk

[Session Bus Policy]
org.freedesktop.secrets=talk
";
        assert!(!session_bus_talk_allowed(Some(elsewhere), "org.freedesktop.FileManager1"));
    }

    /// A sandbox with no policy section at all grants nothing, rather than falling back to
    /// the unsandboxed "yes".
    #[test]
    fn a_policy_free_sandbox_grants_nothing() {
        assert!(!session_bus_talk_allowed(
            Some("[Application]\nname=x\n"),
            "org.freedesktop.FileManager1"
        ));
    }
}

/// The FLATPAK half of [`bundle_label`] (`lab/flatpak`).
#[cfg(test)]
mod flatpak_label_tests {
    use super::bundle_label;
    use std::path::Path;

    /// THE formatting rule the owner asked for: exactly two slashes after the scheme. A Flatpak
    /// path is absolute inside the sandbox, so interpolating it raw would give `Flatpak:///app/…`
    /// and read as a malformed URI. AppImage paths never had this problem because they are
    /// already relative to a mount root by the time they are formatted.
    #[test]
    fn the_scheme_keeps_exactly_two_slashes() {
        let out = bundle_label(Path::new("/app/bin/tesseract"), None, true);
        assert_eq!(out, "Flatpak://app/bin/tesseract");
        assert!(!out.contains(":///"), "three slashes reads as a malformed URI: {out}");
        assert_eq!(out.matches('/').count(), 4, "scheme's two, plus the path's own two");
    }

    /// Both bundle trees are labelled: `/app` is what we ship, `/usr` is the runtime we ship
    /// against. Both are the Flatpak image rather than anything on the user's machine.
    #[test]
    fn both_app_and_the_runtime_are_bundle_paths() {
        assert_eq!(bundle_label(Path::new("/app/bin/tesseract"), None, true), "Flatpak://app/bin/tesseract");
        assert_eq!(bundle_label(Path::new("/usr/bin/ffmpeg"), None, true), "Flatpak://usr/bin/ffmpeg");
    }

    /// A HOST path the sandbox can see is the user's own file and is shown as it is. Labelling
    /// it would claim we shipped something we did not.
    #[test]
    fn a_host_path_inside_the_sandbox_is_not_relabelled() {
        for p in ["/home/u/.local/bin/tesseract", "/run/host/usr/bin/ffmpeg", "/var/tmp/ffmpeg"] {
            assert_eq!(bundle_label(Path::new(p), None, true), p);
        }
    }

    /// Not sandboxed: `/app` and `/usr` are ordinary directories and must not be dressed up.
    /// This is the case every non-Flatpak user is in, so it must be exactly the old behaviour.
    #[test]
    fn without_the_sandbox_nothing_is_relabelled() {
        assert_eq!(bundle_label(Path::new("/usr/bin/ffmpeg"), None, false), "/usr/bin/ffmpeg");
        assert_eq!(bundle_label(Path::new("/app/bin/tesseract"), None, false), "/app/bin/tesseract");
    }

    /// An AppImage mount wins if one is somehow present, so the two schemes can never both
    /// apply to one path and produce a doubled label.
    #[test]
    fn the_appimage_mount_takes_precedence() {
        let mount = Path::new("/tmp/.mount_Cosmic7fA2xQ");
        let out = bundle_label(Path::new("/tmp/.mount_Cosmic7fA2xQ/usr/bin/ffmpeg"), Some(mount), true);
        assert_eq!(out, "AppImage://usr/bin/ffmpeg");
        assert!(!out.contains("Flatpak"));
    }
}

/// The `$APPDIR`-less AppImage fallback (owner report: a Linux Mint AppImage showed plain
/// paths where a CachyOS one showed `AppImage://`).
#[cfg(test)]
mod appimage_dir_fallback_tests {
    use super::appimage_dir_from;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    const IMG: &str = "/home/u/Apps/CosmicCaptureKit-x86_64.AppImage";

    /// THE CASE THIS FIXES. `$APPIMAGE` is set, `$APPDIR` is not, and the binary is sitting at
    /// `<mount>/usr/bin/<name>` exactly as our AppImage lays it out. The mount is three parents
    /// up, so the label works without the runtime having exported anything.
    #[test]
    fn the_mount_is_derived_from_the_exe_when_appdir_is_missing() {
        let exe = Path::new("/tmp/.mount_Cosmic7fA2xQ/usr/bin/cosmic-capture-kit");
        assert_eq!(
            appimage_dir_from(Some(OsStr::new(IMG)), None, Some(exe)),
            Some(PathBuf::from("/tmp/.mount_Cosmic7fA2xQ"))
        );
    }

    /// Extract-and-run mounts somewhere else entirely, and the derivation does not care: it is
    /// keyed on the `usr/bin` SHAPE, not on the mount looking like `/tmp/.mount_*`.
    #[test]
    fn an_extracted_run_derives_just_as_well() {
        let exe = Path::new("/tmp/appimage_extracted_9f2/usr/bin/cosmic-capture-kit");
        assert_eq!(
            appimage_dir_from(Some(OsStr::new(IMG)), None, Some(exe)),
            Some(PathBuf::from("/tmp/appimage_extracted_9f2"))
        );
    }

    /// `$APPDIR` still WINS when present, so a runtime that exports it keeps deciding and this
    /// build behaves exactly as every previous one did on the machines where it already worked.
    #[test]
    fn appdir_still_takes_precedence() {
        let exe = Path::new("/tmp/.mount_Cosmic7fA2xQ/usr/bin/cosmic-capture-kit");
        assert_eq!(
            appimage_dir_from(Some(OsStr::new(IMG)), Some(OsStr::new("/tmp/.mount_REAL")), Some(exe)),
            Some(PathBuf::from("/tmp/.mount_REAL"))
        );
    }

    /// The `usr/bin` shape is what keeps the fallback honest. Without that check any
    /// `$APPIMAGE`-carrying launch would claim its grandparent as a bundle root and start
    /// labelling ordinary paths as though we had shipped them.
    #[test]
    fn a_wrong_shaped_exe_path_derives_nothing() {
        for exe in [
            "/home/u/repo/target/release/cosmic-capture-kit", // a dev build
            "/usr/bin/cosmic-capture-kit",                    // a distro install: no `usr/bin` ABOVE it
            "/opt/cck/bin/cosmic-capture-kit",                // bin, but not under usr
            "/cosmic-capture-kit",                            // no parents to speak of
        ] {
            assert_eq!(
                appimage_dir_from(Some(OsStr::new(IMG)), None, Some(Path::new(exe))),
                None,
                "{exe} must not be read as a bundle root"
            );
        }
    }

    /// `$APPIMAGE` remains the gate. Without it this is not an AppImage launch at all, and a
    /// `usr/bin`-shaped path (every distro install) must never be labelled.
    #[test]
    fn without_appimage_the_exe_shape_proves_nothing() {
        let exe = Path::new("/tmp/.mount_Cosmic7fA2xQ/usr/bin/cosmic-capture-kit");
        assert_eq!(appimage_dir_from(None, None, Some(exe)), None);
    }
}

/// The `~` abbreviation shown paths get (owner call: a full path leaks the account name into
/// every screenshot of a settings screen).
#[cfg(test)]
mod collapse_home_tests {
    use super::collapse_home_from;
    use std::path::Path;

    const HOME: &str = "/home/frosthaven";

    #[test]
    fn a_path_under_home_collapses() {
        assert_eq!(
            collapse_home_from("/home/frosthaven/Capture", Some(Path::new(HOME))),
            "~/Capture"
        );
        assert_eq!(
            collapse_home_from("/home/frosthaven/.config/cosmic-capture-kit/tessdata", Some(Path::new(HOME))),
            "~/.config/cosmic-capture-kit/tessdata"
        );
    }

    /// Home itself, with no trailing component, is just `~`.
    #[test]
    fn home_itself_is_a_bare_tilde() {
        assert_eq!(collapse_home_from(HOME, Some(Path::new(HOME))), "~");
    }

    /// THE case the separator exists for. `/home/frosthaven-backup` shares a prefix with
    /// `/home/frosthaven` but is a DIFFERENT directory, and rewriting it would name a place
    /// the user does not have. A plain `strip_prefix` on the string would get this wrong.
    #[test]
    fn a_sibling_that_merely_shares_the_prefix_is_left_alone() {
        for p in ["/home/frosthaven-backup/Capture", "/home/frosthaven2", "/home/frosthavenX/x"] {
            assert_eq!(collapse_home_from(p, Some(Path::new(HOME))), p);
        }
    }

    /// Paths outside home are untouched, including the bundle labels, which must keep their
    /// scheme rather than acquiring a tilde.
    #[test]
    fn paths_outside_home_pass_through() {
        for p in ["/usr/bin/ffmpeg", "/app/bin/tesseract", "Flatpak://app/bin/tesseract", "AppImage://usr/bin/ffmpeg"] {
            assert_eq!(collapse_home_from(p, Some(Path::new(HOME))), p);
        }
    }

    /// No home, or a degenerate one, changes nothing. A home of `/` would otherwise match
    /// every absolute path on the machine and turn `/usr/bin/ffmpeg` into `~/usr/bin/ffmpeg`.
    #[test]
    fn a_missing_or_degenerate_home_is_a_no_op() {
        assert_eq!(collapse_home_from("/home/frosthaven/Capture", None), "/home/frosthaven/Capture");
        assert_eq!(collapse_home_from("/usr/bin/ffmpeg", Some(Path::new("/"))), "/usr/bin/ffmpeg");
        assert_eq!(collapse_home_from("/usr/bin/ffmpeg", Some(Path::new(""))), "/usr/bin/ffmpeg");
    }

    /// It round-trips with `expand_tilde`, which is what makes the copied string usable: a
    /// shell expands `~`, and so do we if it ever comes back as input.
    #[test]
    fn it_round_trips_with_expand_tilde() {
        let collapsed = collapse_home_from("/home/frosthaven/Capture", Some(Path::new(HOME)));
        assert_eq!(collapsed, "~/Capture");
        // `expand_tilde` reads the real home, so assert the SHAPE rather than the literal.
        assert!(super::expand_tilde(&collapsed).ends_with("Capture"));
    }
}

/// The delivery-kind vocabulary shared by the About page and the debug log.
#[cfg(test)]
mod package_kind_tests {
    use super::{PackageKind, package_kind_from};

    #[test]
    fn each_probe_combination_names_one_kind() {
        assert_eq!(package_kind_from(false, false), PackageKind::Binary);
        assert_eq!(package_kind_from(false, true), PackageKind::AppImage);
        assert_eq!(package_kind_from(true, false), PackageKind::Flatpak);
    }

    /// Flatpak wins when both are somehow true. Nothing stops an AppImage inside a sandbox, and
    /// if that happened the SANDBOX is the fact that governs everything a reader cares about
    /// here: read-only /app, private config, hidden Wayland globals. Naming the inner packaging
    /// would hide the thing actually shaping behaviour.
    #[test]
    fn the_sandbox_wins_over_the_inner_packaging() {
        assert_eq!(package_kind_from(true, true), PackageKind::Flatpak);
    }

    /// Every kind, so a new variant fails the exhaustive match below and forces a decision
    /// here rather than falling into whatever a `_` arm happened to say. Mirrors
    /// `update::notes_source_tests::EVERY_KIND`, which does the same job for the channel and
    /// notes questions.
    const EVERY_KIND: [PackageKind; 5] = [
        PackageKind::Binary,
        PackageKind::AppImage,
        PackageKind::Flatpak,
        PackageKind::MacOs,
        PackageKind::Windows,
    ];

    /// The owner asked for these strings exactly, and they are what the About page shows.
    ///
    /// **The three Linux prefixes are the DRAGON-614 reversal**, and pinning them literally is
    /// the point: they were unprefixed while every kind was Linux, and that was right then.
    /// Once macOS and Windows sit beside them an unprefixed "Binary Release" reads as a third
    /// platform, so the prefixes must not be simplified back out.
    #[test]
    fn the_ui_labels_are_the_requested_wording() {
        assert_eq!(PackageKind::Binary.label(), "Linux Binary Release");
        assert_eq!(PackageKind::AppImage.label(), "Linux AppImage Release");
        assert_eq!(PackageKind::Flatpak.label(), "Linux Flatpak Release");
        assert_eq!(PackageKind::MacOs.label(), "macOS Release");
        assert_eq!(PackageKind::Windows.label(), "Windows Release");
        // Every Linux kind names its platform; neither of the other two borrows the word.
        for k in EVERY_KIND {
            let linux = matches!(
                k,
                PackageKind::Binary | PackageKind::AppImage | PackageKind::Flatpak
            );
            assert_eq!(k.label().starts_with("Linux "), linux, "{k:?}");
            assert!(k.label().ends_with(" Release"), "{k:?}");
        }
    }

    /// DRAGON-591 and DRAGON-614: one glyph per package kind on the About page's release-kind
    /// line, the owner's picks. Pinned by NAME because the names are what
    /// `widgets::icons::handle` maps into the bundled lucide set;
    /// `widgets::icons::tests::every_mapped_name_embeds` lists all five, so these two pins
    /// together prove the row can actually draw on every kind.
    #[test]
    fn each_package_kind_has_its_own_glyph() {
        assert_eq!(PackageKind::Binary.icon_name(), "application-x-executable-symbolic");
        assert_eq!(PackageKind::AppImage.icon_name(), "application-x-appimage-symbolic");
        // The Flatpak keeps the general package box, which the owner was happy with.
        assert_eq!(PackageKind::Flatpak.icon_name(), "package-x-generic-symbolic");
        assert_eq!(PackageKind::MacOs.icon_name(), "package-macos-symbolic");
        assert_eq!(PackageKind::Windows.icon_name(), "package-windows-symbolic");
        let names: Vec<&str> = EVERY_KIND.iter().map(|k| k.icon_name()).collect();
        let mut uniq = names.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), names.len(), "each kind is visually distinct");
    }

    /// The log spells it differently ON PURPOSE: it says what the thing IS, where the UI names a
    /// release channel. A log line reading "Binary Release" would imply a distribution we do not
    /// have. Pinned so a later tidy-up does not "unify" them.
    #[test]
    fn the_log_names_differ_from_the_ui_labels() {
        assert_eq!(PackageKind::Binary.diag_name(), "plain binary");
        assert_eq!(PackageKind::AppImage.diag_name(), "AppImage");
        assert_eq!(PackageKind::Flatpak.diag_name(), "Flatpak");
        assert_eq!(PackageKind::MacOs.diag_name(), "macOS build");
        assert_eq!(PackageKind::Windows.diag_name(), "Windows build");
        for k in EVERY_KIND {
            assert_ne!(k.diag_name(), k.label(), "{k:?}");
        }
    }

    /// DRAGON-614: the running build's kind agrees with the platform it was compiled for.
    ///
    /// The half worth having is the NEGATIVE one. `package_kind_from`'s three answers are
    /// Linux artifact kinds, and a macOS or Windows build must never reach them: a Mac
    /// reporting "Linux Binary Release" is exactly the bug the prefixes would otherwise
    /// introduce, and it is what this build did before the cfg arms existed.
    #[test]
    fn the_running_kind_matches_the_target_platform() {
        let kind = super::package_kind();
        if cfg!(target_os = "macos") {
            assert_eq!(kind, PackageKind::MacOs);
        } else if cfg!(windows) {
            assert_eq!(kind, PackageKind::Windows);
        } else {
            assert!(
                matches!(
                    kind,
                    PackageKind::Binary | PackageKind::AppImage | PackageKind::Flatpak
                ),
                "a Linux build must report a Linux artifact kind, got {kind:?}"
            );
        }
    }
}

/// DRAGON-589: the shell command line that starts THIS build, which the Global tab shows for
/// every hotkey the app cannot register itself. A user pastes it into their desktop's shortcut
/// settings, so a wrong spelling is a shortcut that silently does nothing.
#[cfg(test)]
mod launch_command_tests {
    use super::*;

    /// The three package kinds, each spelled the only way that actually starts that build.
    #[test]
    fn each_package_kind_gets_its_own_spelling() {
        let exe = PathBuf::from("/opt/cck/cosmic-capture-kit");
        assert_eq!(
            launch_command_from(PackageKind::Binary, Some(&exe), FLATPAK_APP_ID, &["--region"]),
            "/opt/cck/cosmic-capture-kit --region"
        );
        // An AppImage names the .AppImage FILE, which is what `self_exe` resolves there.
        let img = PathBuf::from("/opt/CosmicCaptureKit.AppImage");
        assert_eq!(
            launch_command_from(PackageKind::AppImage, Some(&img), FLATPAK_APP_ID, &["--region"]),
            "/opt/CosmicCaptureKit.AppImage --region"
        );
        // A Flatpak's own path is unreachable from the host, so the app id is the handle.
        assert_eq!(
            launch_command_from(PackageKind::Flatpak, Some(&exe), FLATPAK_APP_ID, &["--region"]),
            "flatpak run dev.thedragon.CosmicCaptureKit --region"
        );
    }

    /// A Flatpak ignores the executable path entirely, present or not: inside the sandbox it
    /// names a file the host cannot run.
    #[test]
    fn a_flatpak_never_prints_its_own_path() {
        let inside = PathBuf::from("/app/bin/cosmic-capture-kit");
        for exe in [Some(inside.as_path()), None] {
            let c = launch_command_from(PackageKind::Flatpak, exe, FLATPAK_APP_ID, &["--scan"]);
            assert_eq!(c, "flatpak run dev.thedragon.CosmicCaptureKit --scan");
            assert!(!c.contains("/app/bin"), "{c}");
        }
    }

    /// Several flags keep their order and are separated by single spaces, so the no-editor
    /// twins (`<capture flag> --no-editor`) read exactly as the daemons spawn them.
    #[test]
    fn every_flag_survives_in_order() {
        let exe = PathBuf::from("/opt/cck/cosmic-capture-kit");
        assert_eq!(
            launch_command_from(
                PackageKind::Binary,
                Some(&exe),
                FLATPAK_APP_ID,
                &["--active-window", "--no-editor"]
            ),
            "/opt/cck/cosmic-capture-kit --active-window --no-editor"
        );
        // No flags at all is a bare launch, and prints as one.
        assert_eq!(
            launch_command_from(PackageKind::Binary, Some(&exe), FLATPAK_APP_ID, &[]),
            "/opt/cck/cosmic-capture-kit"
        );
    }

    /// With no path to report, the command falls back to the program NAME rather than printing
    /// something empty or misleading. That is the right answer for a distro install on `PATH`.
    #[test]
    fn a_missing_exe_falls_back_to_the_program_name() {
        assert_eq!(
            launch_command_from(PackageKind::Binary, None, FLATPAK_APP_ID, &["--region"]),
            "cosmic-capture-kit --region"
        );
        assert_eq!(
            launch_command_from(PackageKind::AppImage, None, FLATPAK_APP_ID, &[]),
            "cosmic-capture-kit"
        );
    }

    /// The whole reason quoting exists here: a path with a SPACE must stay one argument, or the
    /// pasted line runs a different program with a stray argument.
    #[test]
    fn a_path_with_a_space_survives_a_paste() {
        let exe = PathBuf::from("/somewhere/My Apps/cosmic-capture-kit");
        let c = launch_command_from(PackageKind::Binary, Some(&exe), FLATPAK_APP_ID, &["--region"]);
        assert_eq!(c, "'/somewhere/My Apps/cosmic-capture-kit' --region");
        // And the flag stays OUTSIDE the quoted word, still its own argument.
        assert!(c.ends_with(" --region"), "{c}");
    }

    /// The quoting rule itself, both directions. An ordinary path must come back untouched, so
    /// the common case reads like something a person would type.
    #[test]
    fn only_a_word_that_needs_quoting_gets_quoted() {
        for plain in [
            "/opt/cck/cosmic-capture-kit",
            "cosmic-capture-kit",
            "--toggle-system-audio",
            "/home/u/.local/bin/cck-1.2",
            "~/apps/cck",
            "~",
        ] {
            assert_eq!(shell_word(plain), plain, "{plain} should not be quoted");
        }
        assert_eq!(shell_word("/a b/c"), "'/a b/c'");
        assert_eq!(shell_word("/a$b"), "'/a$b'");
        assert_eq!(shell_word("/a&b"), "'/a&b'");
        assert_eq!(shell_word("/a\"b"), "'/a\"b'");
        assert_eq!(shell_word("/a\nb"), "'/a\nb'");
        // An empty word still has to BE a word, or it vanishes from argv.
        assert_eq!(shell_word(""), "''");
    }

    /// A single quote inside the word is the one escape a single-quoted shell literal needs,
    /// and it is written the same way `update::shell_single_quote` writes it.
    #[test]
    fn a_single_quote_is_escaped_the_posix_way() {
        assert_eq!(shell_word("/u/it's here/cck"), r"'/u/it'\''s here/cck'");
    }

    /// A home-collapsed path keeps its `~` OUTSIDE the quotes, because a quoted tilde is a
    /// literal character and the pasted line would look for a folder actually named `~`.
    #[test]
    fn a_collapsed_home_keeps_its_tilde_expandable() {
        assert_eq!(shell_word("~/My Apps/cck"), "~/'My Apps/cck'");
        assert!(!shell_word("~/My Apps/cck").starts_with('\''));
    }

    /// The command SHOWS a home-collapsed path, like every other path this app prints
    /// (`collapse_home`'s privacy rule), and it still runs: the shell expands `~` itself.
    #[test]
    fn the_command_collapses_the_home_directory() {
        let Some(home) = dirs::home_dir() else {
            eprintln!("skipping: no home directory on this runner");
            return;
        };
        let exe = home.join("apps/cosmic-capture-kit");
        let c = launch_command_from(PackageKind::Binary, Some(&exe), FLATPAK_APP_ID, &["--region"]);
        assert_eq!(c, "~/apps/cosmic-capture-kit --region");
        assert!(!c.contains(&*home.to_string_lossy()), "{c}");
    }
}
