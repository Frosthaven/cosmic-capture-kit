//! Small cross-cutting helpers shared across the crate: the session runtime directory,
//! `~/` expansion, and `file://` URI building. Centralised so the fallback and format
//! live in one place instead of being re-derived at every call site.

use std::path::{Path, PathBuf};

/// Startup-latency instrumentation (investigation only, gated on `CCK_TIMING`).
/// Prints `[+<ms since process start>] <label>` to stderr. Zero cost unless the
/// env var is set (the process-start `Instant` is captured lazily on first use).
pub fn timing_start() -> std::time::Instant {
    *TIMING_T0.get_or_init(std::time::Instant::now)
}
static TIMING_T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

#[inline]
pub fn timing_mark(label: &str) {
    if std::env::var_os("CCK_TIMING").is_some() {
        let t0 = *TIMING_T0.get_or_init(std::time::Instant::now);
        eprintln!("[+{:>7.1}ms] {label}", t0.elapsed().as_secs_f64() * 1000.0);
    }
}

/// The app's OWN config directory — `~/.config/cosmic-capture-kit` on EVERY OS, so
/// the user-facing paths (the covermark drop folder, `config.toml`) read the same
/// everywhere. Linux resolves through `dirs::config_dir()` (XDG-respecting —
/// byte-identical to the historical location); macOS/Windows pin to `~/.config`
/// instead of their native config homes (`~/Library/Application Support` /
/// `%APPDATA%`), MIGRATING anything an older build left in the native location
/// (a one-time whole-directory rename, checked once per process).
pub fn app_config_dir() -> Option<PathBuf> {
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

// `is_cosmic()` and the COSMIC preview-float tiling-exception writer
// (`set_cosmic_preview_float`) moved into the COSMIC desktop profile
// (`platform::linux::cosmic` + `platform::linux::cosmic::quirks`) with the rest
// of the per-desktop COSMIC config knowledge (DRAGON-220).

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
}
