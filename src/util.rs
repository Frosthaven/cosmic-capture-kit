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
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(tool).is_file()))
        .unwrap_or(false)
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
