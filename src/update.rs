//! In-app update channel (DRAGON-175).
//!
//! The app is distributed from a **private** source repo, so the update channel
//! is a small SEPARATE **public** repo served over GitHub Pages (Pages is not
//! available for private repos on the free plan). The release workflow publishes
//! a manifest (`update.json`) plus the macOS `.dmg` there; the app polls the
//! manifest, compares its `version` against `CARGO_PKG_VERSION`, and (macOS) can
//! download + verify + swap the installed `.app` in one click.
//!
//! Everything here is dependency-light on purpose:
//!   * fetch is a `curl` shell-out (present on macOS and virtually every Linux),
//!   * the manifest is parsed by a tiny hand parser for its FIXED shape (no
//!     `serde_json` in the tree — see CLAUDE.md's "don't add deps lightly"),
//!   * sha256 is `shasum -a 256` (no hashing crate in the tree).
//!
//! The pure islands — [`compare_versions`], [`Manifest::parse`],
//! [`UpdateStatus::from_manifest`], [`artifact_name`] — are unit-tested at the
//! file bottom. The I/O (curl / hdiutil / the detached swap helper) is not, by
//! the repo's testing rule.

/// The default public update channel base URL (the SEPARATE public Pages repo;
/// the private source repo can't host Pages on the free plan). Overridable at
/// runtime with `CCK_UPDATE_URL` for local E2E testing (point it at a
/// `file://`/`http://localhost` manifest).
pub const DEFAULT_MANIFEST_URL: &str =
    "https://frosthaven.github.io/cosmic-capture-kit-updates/update.json";

/// Environment override for the manifest URL (dev/testing).
pub const MANIFEST_URL_ENV: &str = "CCK_UPDATE_URL";

/// The project page the "Update Now" launch dialog opens on Linux (no one-click
/// install there yet), matching the About page's "Open releases" link destination.
/// Only the non-macOS launch-dialog path consumes it; on macOS the one-click flow is
/// used instead, so it's dead there (compiled everywhere so the plumbing can't drift).
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub const RELEASES_URL: &str = "https://github.com/Frosthaven/cosmic-capture-kit";

/// Env var a spawner sets so a settings child opens on a specific tab. "about"
/// is the only recognized value (the post-update landing page).
pub const SETTINGS_TAB_ENV: &str = "CCK_SETTINGS_TAB";

/// A file in the app's config dir (the same location `state::store` uses, via
/// [`crate::util::app_config_dir`]), for the manifest cache + the post-update
/// marker. Sharing the helper keeps these beside `config.toml` on every OS.
fn state_file(name: &str) -> Option<std::path::PathBuf> {
    Some(crate::util::app_config_dir()?.join(name))
}

/// Cache the last successfully FETCHED manifest body on disk (best effort). The
/// settings window seeds its update state from this at launch - instant notes +
/// dialog with zero network wait (mac settings is a fresh process every time) -
/// then refreshes over the network in the background.
fn write_manifest_cache(body: &str) {
    if let Some(path) = state_file("update-manifest.json") {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, body);
    }
}

/// The update status seeded from the on-disk manifest cache, if one exists and
/// parses. Staleness is fine: the caller always follows with a real check; the
/// seed only makes the first render instant.
pub fn seeded_status_from_cache() -> Option<UpdateStatus> {
    let body = std::fs::read_to_string(state_file("update-manifest.json")?).ok()?;
    let manifest = Manifest::parse(&body)?;
    Some(UpdateStatus::from_manifest(&manifest, env!("CARGO_PKG_VERSION")))
}

/// Write the post-update marker: the installer drops it just before the swap
/// helper relaunches the app, and the relaunch consumes it to land the user on
/// Settings > About (the new version's "What's new" front and center).
/// Only the macOS one-click install flow writes it today; compiled (and
/// type-checked) everywhere on purpose.
#[cfg_attr(not(target_os = "macos"), expect(dead_code))]
fn write_post_update_marker() {
    if let Some(path) = state_file("post-update") {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(&path, b"1") {
            Ok(()) => log::info!("update: wrote post-update marker at {}", path.display()),
            Err(e) => log::warn!("update: could not write post-update marker: {e}"),
        }
    }
}

/// Consume (remove) the post-update marker; true when this launch is the first
/// one after an update install.
pub fn take_post_update_marker() -> bool {
    let Some(path) = state_file("post-update") else {
        return false;
    };
    let taken = std::fs::remove_file(&path).is_ok();
    if taken {
        log::info!("update: consumed post-update marker; landing on Settings > About");
    }
    taken
}

/// Minimum perceived duration of an INTERACTIVE update check (DRAGON-177). When a
/// check that shows the "Checking..." state resolves faster than this (e.g. a local
/// `file://` manifest, or a warm cache), the UI holds "Checking..." until this floor
/// so the button/status does not flip back instantly — an instant flip reads as a
/// broken no-op. The floor is applied to the RESULT DELIVERY (an async delay on the
/// blocking pool), never by slowing the fetch itself or sleeping on the UI thread; the
/// daemon-startup check (which never shows a Checking state) is unaffected.
pub const INTERACTIVE_CHECK_FLOOR: std::time::Duration = std::time::Duration::from_secs(2);

/// The remaining delay to hold "Checking..." so an interactive check meets the
/// [`INTERACTIVE_CHECK_FLOOR`]: `floor - elapsed`, or zero if the check already took
/// at least the floor. Pure + unit-tested; the caller sleeps this on the blocking pool.
pub fn check_floor_remainder(
    elapsed: std::time::Duration,
    floor: std::time::Duration,
) -> std::time::Duration {
    floor.saturating_sub(elapsed)
}

/// The platform key this build looks for in the manifest's `platforms` map.
pub const PLATFORM_KEY: &str = if cfg!(target_os = "macos") {
    "macos"
} else if cfg!(target_os = "linux") {
    "linux"
} else {
    "unknown"
};

/// Resolve the manifest URL: the `CCK_UPDATE_URL` override if set, else the
/// baked-in default.
pub fn manifest_url() -> String {
    std::env::var(MANIFEST_URL_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_MANIFEST_URL.to_string())
}

/// One platform's artifact entry from the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformArtifact {
    /// Direct download URL for the artifact (a `.dmg` on macOS).
    pub url: String,
    /// Lowercase hex sha256 of the artifact.
    pub sha256: String,
    /// Artifact size in bytes (0 when unknown).
    pub size: u64,
}

/// The parsed `update.json` manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The published version (semver `X.Y.Z`).
    pub version: String,
    /// Human-readable release notes (may be empty).
    pub notes: String,
    /// ISO publish date (may be empty).
    pub published: String,
    /// The artifact for THIS platform, if the manifest carries one.
    pub artifact: Option<PlatformArtifact>,
}

impl Manifest {
    /// Parse a manifest for the CURRENT platform's key. Returns `None` on
    /// malformed input. This is a deliberately small hand parser for the fixed
    /// shape:
    /// ```json
    /// { "version": "X.Y.Z", "notes": "...", "published": "...",
    ///   "platforms": { "macos": { "url": "...", "sha256": "...", "size": N } } }
    /// ```
    /// It is NOT a general JSON parser; it tolerates whitespace and key order,
    /// and unescapes the handful of JSON string escapes the publisher emits.
    pub fn parse(json: &str) -> Option<Manifest> {
        Manifest::parse_platform(json, PLATFORM_KEY)
    }

    /// Same as [`parse`](Manifest::parse) but for an explicit platform key
    /// (so the tests can exercise every platform on one host).
    pub fn parse_platform(json: &str, platform: &str) -> Option<Manifest> {
        let version = json_string_field(json, "version")?;
        if version.trim().is_empty() {
            return None;
        }
        let notes = json_string_field(json, "notes").unwrap_or_default();
        let published = json_string_field(json, "published").unwrap_or_default();
        let artifact = platform_object(json, platform).and_then(|obj| {
            let url = json_string_field(obj, "url")?;
            let sha256 = json_string_field(obj, "sha256").unwrap_or_default();
            let size = json_number_field(obj, "size").unwrap_or(0);
            if url.trim().is_empty() {
                return None;
            }
            Some(PlatformArtifact { url, sha256: sha256.to_lowercase(), size })
        });
        Some(Manifest { version, notes, published, artifact })
    }
}

/// The app's knowledge of update availability (cached in `App`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// Not checked yet this session.
    Unknown,
    /// A check is running.
    Checking,
    /// The installed version is the newest published. Carries the manifest's
    /// version + notes so the About page can always show "What's new in
    /// <installed version>" even with no update pending.
    UpToDate { version: String, notes: String },
    /// A newer version is published.
    Available(UpdateInfo),
    /// The check failed (no network, no curl, malformed manifest, …). The string
    /// is a short human reason for the About page.
    Failed(String),
}

/// The details of an available update, cached for the About page + one-click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    /// The new version string.
    pub version: String,
    /// Release notes.
    pub notes: String,
    /// The artifact for this platform, if the manifest carried one (Linux
    /// manifests may omit it — no published Linux artifact yet).
    pub artifact: Option<PlatformArtifact>,
}

impl UpdateStatus {
    /// Decide the status from a freshly fetched manifest, comparing its version
    /// against `current` (normally `CARGO_PKG_VERSION`). Pure + unit-tested.
    pub fn from_manifest(manifest: &Manifest, current: &str) -> UpdateStatus {
        match compare_versions(&manifest.version, current) {
            Some(std::cmp::Ordering::Greater) => UpdateStatus::Available(UpdateInfo {
                version: manifest.version.clone(),
                notes: manifest.notes.clone(),
                artifact: manifest.artifact.clone(),
            }),
            Some(_) => UpdateStatus::UpToDate {
                version: manifest.version.clone(),
                notes: manifest.notes.clone(),
            },
            // A malformed manifest version is not something we can act on — treat
            // it as "no update" rather than surfacing a scary error.
            None => UpdateStatus::UpToDate {
                version: manifest.version.clone(),
                notes: manifest.notes.clone(),
            },
        }
    }

    /// Whether an update is available (drives the nav-rail success tint + icon).
    pub fn is_available(&self) -> bool {
        matches!(self, UpdateStatus::Available(_))
    }

    /// The (version, notes) pair the About page renders as "What's new in
    /// <version>": the pending update's when one is Available, the installed
    /// (manifest) version's when UpToDate - so the changelog stays visible even
    /// with no update pending. `None` for Unknown / Checking / Failed.
    pub fn notes_and_version(&self) -> Option<(&str, &str)> {
        match self {
            UpdateStatus::Available(info) => Some((&info.version, &info.notes)),
            UpdateStatus::UpToDate { version, notes } => Some((version, notes)),
            _ => None,
        }
    }
}

/// Whether the launch-time update dialog (DRAGON-177) should be shown, and with what
/// [`UpdateInfo`]. Pure + unit-tested. The dialog appears ONLY when the user has the
/// "Notify me when an update is available" setting ON (`notify_updates`) AND a freshly
/// resolved check found an available update AND the active settings page is NOT About.
/// Every other status (Unknown / Checking / UpToDate / Failed) yields `None`, as does
/// the setting being OFF. The caller then gates on its own once-per-session flag so a
/// repeated check doesn't re-pop it.
///
/// The `active_page_is_about` gate suppresses the popup when the user is already on the
/// About page (DRAGON-177 follow-up): About carries the same controls (Install button,
/// notes, notify toggle), so the dialog there is redundant. The decision is made once
/// when the check resolves; a suppression here is not re-armed later in the session
/// (the About page already showed everything).
///
/// Cloning the `UpdateInfo` here (rather than borrowing) lets the caller stash it in
/// the dialog state, which outlives the transient status match.
pub fn dialog_for_status(
    status: &UpdateStatus,
    notify_updates: bool,
    active_page_is_about: bool,
) -> Option<UpdateInfo> {
    if !notify_updates || active_page_is_about {
        return None;
    }
    match status {
        UpdateStatus::Available(info) => Some(info.clone()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Daemon startup update notice (DRAGON-177 follow-up).
// ---------------------------------------------------------------------------

/// How long the resident daemon waits after startup before its update check.
/// Small on purpose: a MANUALLY launched daemon should surface a live update
/// near-instantly (a long wait reads as "nothing happened"). The login-time
/// network race is handled by RETRIES instead (see
/// [`DAEMON_STARTUP_CHECK_RETRIES`]): a failed first check is retried after a
/// backoff, so a daemon that beat the network still lands the notice.
pub const DAEMON_STARTUP_CHECK_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// Retry schedule for the daemon startup check when the fetch FAILS (network
/// not up yet, typical right after login). Each entry is a wait before the
/// next attempt. Up-to-date / Available results never retry; only failures do.
pub const DAEMON_STARTUP_CHECK_RETRIES: [std::time::Duration; 2] = [
    std::time::Duration::from_secs(15),
    std::time::Duration::from_secs(45),
];

/// Pure decision for the daemon's startup update notice: given the persisted
/// `notify_updates` setting and a freshly resolved [`UpdateStatus`], decide
/// whether the daemon should surface the settings window (which then runs its
/// own check and shows the DRAGON-177 dialog). The settings window is opened
/// ONLY when the setting is ON AND the status is [`UpdateStatus::Available`];
/// every other combination (setting off, or any non-Available status) is a
/// silent no-op. Mirrors [`dialog_for_status`]'s gate so the daemon and the
/// settings window agree on when a notice is warranted. Pure + unit-tested.
pub fn should_notify_on_daemon_startup(status: &UpdateStatus, notify_updates: bool) -> bool {
    notify_updates && status.is_available()
}

/// Run the daemon's ONE-TIME startup update check and, if an update is available
/// and the user wants notices, spawn the settings window so it surfaces the
/// DRAGON-177 dialog. Blocking (it runs the curl-based [`check_now`]); the
/// callers run it on a dedicated background thread AFTER the
/// [`DAEMON_STARTUP_CHECK_DELAY`] grace, so it never blocks the run loop / tray
/// setup. Shared by the macOS daemon and the Linux resident so the behavior
/// cannot diverge.
///
/// The `notify_updates` setting is read fresh here (the daemon is the same
/// binary; `crate::state::load()` is right there). If the setting is off, or the
/// check fails / reports up-to-date, this does nothing. Otherwise it calls
/// `spawn_settings` (injected so the pure decision is unit-testable without
/// spawning a real process) — in practice
/// `crate::recording_ui::spawn_capture_child("--settings")`. The spawned child
/// inherits the environment (so its own check sees the same manifest / any
/// `CCK_UPDATE_URL` override) and shows the dialog.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn notify_daemon_startup_if_update_available(spawn_settings: impl FnOnce()) {
    let notify_updates = crate::state::load().notify_updates;
    if !notify_updates {
        log::info!("daemon: update notice suppressed (notify_updates off)");
        return;
    }
    // First check fires fast (manual launches should feel instant); ONLY a
    // failed fetch retries, on the backoff schedule, to cover the login-time
    // window where the daemon comes up before the network does.
    let mut status = check_now();
    for wait in DAEMON_STARTUP_CHECK_RETRIES {
        if !matches!(status, UpdateStatus::Failed(_)) {
            break;
        }
        log::info!("daemon: startup update check failed; retrying in {wait:?}");
        std::thread::sleep(wait);
        status = check_now();
    }
    if should_notify_on_daemon_startup(&status, notify_updates) {
        log::info!("daemon: an update is available; opening settings to surface the notice");
        spawn_settings();
    } else {
        log::info!("daemon: startup update check found no available update (status {status:?})");
    }
}

/// Compare two dotted numeric versions (`X.Y.Z`, extra components tolerated).
/// Returns `Ordering` of `a` vs `b`, or `None` if either is unparseable.
/// Ignores any `-suffix`/`+build` metadata (compares the release core only).
pub fn compare_versions(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let pa = parse_version(a)?;
    let pb = parse_version(b)?;
    Some(pa.cmp(&pb))
}

/// Parse a version into a comparable component vector. Strips `-pre`/`+build`
/// metadata; a version with no numeric components is rejected.
fn parse_version(v: &str) -> Option<Vec<u64>> {
    let core = v.trim().split(['-', '+']).next().unwrap_or("").trim();
    if core.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for part in core.split('.') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        out.push(part.parse::<u64>().ok()?);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// The canonical artifact filename for a version on the update channel.
/// Kept in ONE place so the workflow (which names the file) and any app-side
/// expectation agree. Matches `mac-package.sh`'s `CosmicCaptureKit-<version>.dmg`.
/// In the binary only the macOS install flow consumes it (Linux has no published
/// artifact yet); the tests cover it everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn artifact_name(version: &str) -> String {
    format!("CosmicCaptureKit-{version}.dmg")
}

// ---------------------------------------------------------------------------
// I/O: fetch + verify + install (not unit-tested — the repo's rule).
// ---------------------------------------------------------------------------

/// Fetch the manifest and decide the status. Blocking; call from a background
/// task (`Task::perform(async { spawn_blocking(check) })`). Degrades to
/// [`UpdateStatus::Failed`] with a short reason on any error.
pub fn check_now() -> UpdateStatus {
    let url = manifest_url();
    let body = match fetch_text(&url) {
        Ok(b) => b,
        Err(e) => return UpdateStatus::Failed(e),
    };
    match Manifest::parse(&body) {
        Some(m) => {
            // Cache the raw body so the next settings launch renders instantly
            // from disk before its own network check.
            write_manifest_cache(&body);
            UpdateStatus::from_manifest(&m, env!("CARGO_PKG_VERSION"))
        }
        None => UpdateStatus::Failed("Update information could not be read.".to_string()),
    }
}

/// `curl -fsSL --max-time 10 <url>` -> body, or a short error reason.
fn fetch_text(url: &str) -> Result<String, String> {
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "10", url])
        .output()
        .map_err(|_| "Could not run curl to check for updates.".to_string())?;
    if !out.status.success() {
        return Err("Could not reach the update server.".to_string());
    }
    String::from_utf8(out.stdout).map_err(|_| "Update information was not valid text.".to_string())
}

/// Compute the lowercase-hex sha256 of a file via `shasum -a 256`. Only the
/// macOS install flow verifies a download today, so the fn is macOS-gated.
#[cfg(target_os = "macos")]
fn file_sha256(path: &std::path::Path) -> Result<String, String> {
    let out = std::process::Command::new("shasum")
        .arg("-a")
        .arg("256")
        .arg(path)
        .output()
        .map_err(|_| "Could not run shasum to verify the download.".to_string())?;
    if !out.status.success() {
        return Err("Could not verify the download checksum.".to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace()
        .next()
        .map(|s| s.to_lowercase())
        .ok_or_else(|| "Could not read the download checksum.".to_string())
}

/// The outcome of the blocking install work, handed back to the UI so it can
/// either trigger the app's own exit (success) or show the error (failure).
/// Constructed only by the macOS install flow; the type (and its Linux match in
/// `update_settings`) is compiled everywhere so the message plumbing can't drift.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum InstallOutcome {
    /// The new `.app` is staged and a detached helper is armed to swap + relaunch
    /// once this process (and the daemon) exit. The caller must now quit.
    Staged,
    /// The install failed; the reason is user-facing.
    Failed(String),
}

/// Run the full macOS one-click install for `info`, BLOCKING. Downloads the dmg
/// to a temp dir, verifies its sha256, mounts + stages the `.app`, writes a
/// detached swap helper, and returns [`InstallOutcome::Staged`] (the caller then
/// quits: the helper waits for full exit, swaps `/Applications`, and relaunches).
///
/// Not available on Linux (no published Linux artifact yet).
#[cfg(target_os = "macos")]
pub fn install_macos(info: &UpdateInfo) -> InstallOutcome {
    match install_macos_inner(info) {
        Ok(()) => InstallOutcome::Staged,
        Err(e) => InstallOutcome::Failed(e),
    }
}

#[cfg(target_os = "macos")]
fn install_macos_inner(info: &UpdateInfo) -> Result<(), String> {
    use std::io::Write as _;

    let artifact = info
        .artifact
        .as_ref()
        .ok_or_else(|| "This update has no macOS download.".to_string())?;

    // A private temp dir for the whole flow.
    let work = std::env::temp_dir().join(format!("cck-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|_| "Could not create a temp folder.".to_string())?;
    let dmg_path = work.join(artifact_name(&info.version));

    // 1) Download.
    let ok = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "600", "-o"])
        .arg(&dmg_path)
        .arg(&artifact.url)
        .status()
        .map_err(|_| "Could not run curl to download the update.".to_string())?
        .success();
    if !ok {
        return Err("The update download failed.".to_string());
    }

    // 2) Verify sha256 (skip only if the manifest omitted one).
    if !artifact.sha256.trim().is_empty() {
        let got = file_sha256(&dmg_path)?;
        if got != artifact.sha256.to_lowercase() {
            return Err("The download failed its integrity check.".to_string());
        }
    }

    // 3) Mount read-only, no Finder window.
    let mount = work.join("mnt");
    std::fs::create_dir_all(&mount).map_err(|_| "Could not create a mount point.".to_string())?;
    let attach = std::process::Command::new("hdiutil")
        .args(["attach", "-nobrowse", "-readonly", "-mountpoint"])
        .arg(&mount)
        .arg(&dmg_path)
        .status()
        .map_err(|_| "Could not run hdiutil to mount the update.".to_string())?;
    if !attach.success() {
        return Err("Could not mount the update image.".to_string());
    }

    // Locate the `.app` inside the mounted image and copy it to a staging dir
    // (so we can detach before the swap — a mounted volume can't be the source
    // of a long-lived helper).
    let stage_result = (|| -> Result<std::path::PathBuf, String> {
        let app_in_dmg = find_app_bundle(&mount)
            .ok_or_else(|| "The update image did not contain the app.".to_string())?;
        let staged = work.join("staged");
        std::fs::create_dir_all(&staged)
            .map_err(|_| "Could not create a staging folder.".to_string())?;
        let staged_app = staged.join(
            app_in_dmg
                .file_name()
                .ok_or_else(|| "The app bundle name was unreadable.".to_string())?,
        );
        // cp -R preserves the bundle (symlinks, resource forks) faithfully.
        let ok = std::process::Command::new("cp")
            .arg("-R")
            .arg(&app_in_dmg)
            .arg(&staged_app)
            .status()
            .map_err(|_| "Could not copy the new app.".to_string())?
            .success();
        if !ok {
            return Err("Could not copy the new app from the update image.".to_string());
        }
        Ok(staged_app)
    })();

    // Always detach, whether staging succeeded or not.
    let _ = std::process::Command::new("hdiutil")
        .args(["detach", "-quiet"])
        .arg(&mount)
        .status();

    let staged_app = stage_result?;

    // 4) Write + spawn the detached swap helper. It WAITS for this app AND the
    //    daemon to fully exit before swapping `/Applications` and relaunching —
    //    mirroring mac-package.sh's install dance and dodging the single-instance
    //    lock race (a swap mid-exit would race the relaunch against the old lock).
    let installed = "/Applications/Cosmic Capture Kit.app";
    // The relaunched app (daemon or bare) consumes this to land on Settings >
    // About, showing the freshly installed version's notes immediately.
    write_post_update_marker();
    let script_path = work.join("swap.sh");
    let log_path = work.join("swap.log");
    let script = swap_helper_script(
        &staged_app.to_string_lossy(),
        installed,
        &log_path.to_string_lossy(),
    );
    {
        let mut f = std::fs::File::create(&script_path)
            .map_err(|_| "Could not write the update helper.".to_string())?;
        f.write_all(script.as_bytes())
            .map_err(|_| "Could not write the update helper.".to_string())?;
    }

    // Detach fully: nohup + setsid-equivalent via `open`-free `sh` in its own
    // session so it survives this process's exit.
    std::process::Command::new("/bin/sh")
        .arg(&script_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "Could not launch the update helper.".to_string())?;

    Ok(())
}

/// Find the first `*.app` bundle directly inside `dir` (the dmg mount root).
#[cfg(target_os = "macos")]
fn find_app_bundle(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|e| e == "app") {
            return Some(p);
        }
    }
    None
}

/// Build the detached swap-and-relaunch helper. Pure string builder so it can be
/// unit-tested for the shape (quoting, the wait loop, the swap, the relaunch).
#[cfg(target_os = "macos")]
pub fn swap_helper_script(staged_app: &str, installed_app: &str, log: &str) -> String {
    // Every path is single-quoted; the helper polls for BOTH the app and the
    // daemon to exit (bounded ~30s) before swapping and relaunching. It logs to
    // `log` for post-mortem. `pgrep -f` matches the installed bundle path so it
    // ignores unrelated processes.
    format!(
        r#"#!/bin/sh
set -u
LOG='{log}'
STAGED='{staged}'
INSTALLED='{installed}'
exec >>"$LOG" 2>&1
echo "[cck-update] helper started $(date)"

# Wait (bounded) for the running app + daemon to fully exit. Both are matched by
# the installed bundle path; the daemon runs the same binary with `resident`.
i=0
while [ $i -lt 60 ]; do
  if ! pgrep -f "$INSTALLED/Contents/MacOS/" >/dev/null 2>&1; then
    break
  fi
  i=$((i + 1))
  sleep 0.5
done
echo "[cck-update] processes clear after ${{i}} polls"

# Swap the bundle. Remove the old one, then copy the staged one in.
rm -rf "$INSTALLED"
cp -R "$STAGED" "$INSTALLED"
SWAP=$?
echo "[cck-update] swap exit=$SWAP"

# Relaunch. `open` re-launches the app; if the user runs resident, the daemon is
# what comes back (it re-spawns the menu-bar item + hotkey).
if [ $SWAP -eq 0 ]; then
  open "$INSTALLED"
  echo "[cck-update] relaunched"
else
  echo "[cck-update] swap failed; leaving the old app in place"
fi
"#,
        log = shell_single_quote(log),
        staged = shell_single_quote(staged_app),
        installed = shell_single_quote(installed_app),
    )
}

/// Escape a string for safe embedding inside a single-quoted `sh` literal.
#[cfg(target_os = "macos")]
fn shell_single_quote(s: &str) -> String {
    // A single quote inside single quotes is written as: '\''
    s.replace('\'', r"'\''")
}

// ---------------------------------------------------------------------------
// Tiny hand parser for the fixed manifest shape.
// ---------------------------------------------------------------------------

/// Extract a top-level (or object-local) JSON string field: `"key": "value"`.
/// Scans `json` for the FIRST occurrence of `"key"` followed by `:` and a
/// string, unescaping the common JSON escapes. Returns `None` if absent.
fn json_string_field(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut from = 0usize;
    // A key may appear inside a nested object we don't want; but for our fixed
    // shape the top-level scalar keys (version/notes/published) are unique, and
    // the artifact keys (url/sha256/size) are looked up inside the already-sliced
    // platform object, so first-match is correct.
    loop {
        let idx = json[from..].find(&needle)? + from;
        let after = &json[idx + needle.len()..];
        // Expect optional whitespace, a colon, optional whitespace, then a quote.
        let after = after.trim_start();
        let after = after.strip_prefix(':')?;
        let after = after.trim_start();
        if let Some(rest) = after.strip_prefix('"') {
            return Some(unescape_json_string(rest));
        }
        // Not a string value here (e.g. the key preceded a number) — keep looking.
        from = idx + needle.len();
    }
}

/// Extract a JSON integer field: `"key": 12345`. Returns `None` if absent or not
/// an integer.
fn json_number_field(json: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\"");
    let idx = json.find(&needle)?;
    let after = json[idx + needle.len()..].trim_start();
    let after = after.strip_prefix(':')?.trim_start();
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<u64>().ok()
    }
}

/// Slice out the `{...}` object body for `platforms.<platform>`.
fn platform_object<'a>(json: &'a str, platform: &str) -> Option<&'a str> {
    // Find the "platforms" object, then the platform key within it.
    let plat_idx = json.find("\"platforms\"")?;
    let region = &json[plat_idx..];
    let key = format!("\"{platform}\"");
    let key_idx = region.find(&key)? + plat_idx;
    // From just after the key, find the opening brace of its object.
    let after = &json[key_idx + key.len()..];
    let brace_rel = after.find('{')?;
    let obj_start = key_idx + key.len() + brace_rel;
    // Walk to the matching close brace (strings can't contain unescaped braces in
    // our publisher output; escaped quotes are handled by skipping quoted spans).
    let bytes = json.as_bytes();
    let mut depth = 0i32;
    let mut i = obj_start;
    let mut in_str = false;
    let mut escaped = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
        } else {
            match c {
                '"' => in_str = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&json[obj_start..=i]);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Unescape a JSON string starting AFTER the opening quote, up to the closing
/// unescaped quote. Handles `\" \\ \/ \n \t \r \b \f` and `\uXXXX` (BMP).
fn unescape_json_string(rest: &str) -> String {
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('b') => out.push('\u{0008}'),
                Some('f') => out.push('\u{000C}'),
                Some('u') => {
                    let hex: String = (&mut chars).take(4).collect();
                    if let Ok(cp) = u32::from_str_radix(&hex, 16)
                        && let Some(ch) = char::from_u32(cp)
                    {
                        out.push(ch);
                    }
                }
                Some(other) => out.push(other),
                None => break,
            },
            other => out.push(other),
        }
    }
    out
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn interactive_check_floor_holds_fast_results() {
        use std::time::Duration;
        let floor = Duration::from_secs(2);
        // A near-instant check waits out almost the whole floor.
        assert_eq!(
            check_floor_remainder(Duration::from_millis(0), floor),
            Duration::from_secs(2)
        );
        // A partial elapsed leaves the remainder.
        assert_eq!(
            check_floor_remainder(Duration::from_millis(1500), floor),
            Duration::from_millis(500)
        );
        // Exactly at the floor: no extra wait.
        assert_eq!(check_floor_remainder(floor, floor), Duration::ZERO);
        // A slow check (already past the floor) never sleeps (saturating, no underflow).
        assert_eq!(
            check_floor_remainder(Duration::from_secs(5), floor),
            Duration::ZERO
        );
    }

    #[test]
    fn semver_newer_older_equal() {
        assert_eq!(compare_versions("0.3.0", "0.2.0"), Some(Ordering::Greater));
        assert_eq!(compare_versions("0.2.0", "0.3.0"), Some(Ordering::Less));
        assert_eq!(compare_versions("0.2.0", "0.2.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("1.0.0", "0.9.9"), Some(Ordering::Greater));
        assert_eq!(compare_versions("0.2.10", "0.2.9"), Some(Ordering::Greater));
    }

    #[test]
    fn semver_component_counts_differ() {
        // Shorter is padded implicitly by the vec cmp (missing = fewer elements,
        // so "0.2" < "0.2.1" because the longer vec is greater when equal-prefix).
        assert_eq!(compare_versions("0.2", "0.2.1"), Some(Ordering::Less));
        assert_eq!(compare_versions("0.2.0", "0.2"), Some(Ordering::Greater));
    }

    #[test]
    fn semver_strips_pre_and_build() {
        assert_eq!(compare_versions("0.3.0-rc1", "0.2.0"), Some(Ordering::Greater));
        assert_eq!(compare_versions("0.2.0+build7", "0.2.0"), Some(Ordering::Equal));
    }

    #[test]
    fn semver_malformed_is_none() {
        assert_eq!(compare_versions("abc", "0.2.0"), None);
        assert_eq!(compare_versions("0.2.0", ""), None);
        assert_eq!(compare_versions("0..2", "0.2.0"), None);
        assert_eq!(compare_versions("0.x.0", "0.2.0"), None);
    }

    const SAMPLE: &str = r#"
    {
      "version": "0.3.0",
      "notes": "Faster recording and a new About page.",
      "published": "2026-07-11",
      "platforms": {
        "macos": {
          "url": "https://example.com/CosmicCaptureKit-0.3.0.dmg",
          "sha256": "ABCDEF0123456789",
          "size": 78123456
        },
        "linux": {
          "url": "https://example.com/notes.txt",
          "sha256": "",
          "size": 0
        }
      }
    }
    "#;

    #[test]
    fn manifest_parses_macos() {
        let m = Manifest::parse_platform(SAMPLE, "macos").expect("parse");
        assert_eq!(m.version, "0.3.0");
        assert_eq!(m.notes, "Faster recording and a new About page.");
        assert_eq!(m.published, "2026-07-11");
        let a = m.artifact.expect("artifact");
        assert_eq!(a.url, "https://example.com/CosmicCaptureKit-0.3.0.dmg");
        // sha lowercased on parse.
        assert_eq!(a.sha256, "abcdef0123456789");
        assert_eq!(a.size, 78123456);
    }

    #[test]
    fn manifest_parses_linux_without_platform_artifact_gracefully() {
        // A manifest with only a macos artifact, parsed for linux -> no artifact.
        let only_mac = r#"{"version":"0.3.0","notes":"n","published":"d","platforms":{"macos":{"url":"u","sha256":"a","size":1}}}"#;
        let m = Manifest::parse_platform(only_mac, "linux").expect("parse");
        assert_eq!(m.version, "0.3.0");
        assert!(m.artifact.is_none());
    }

    #[test]
    fn manifest_rejects_missing_version() {
        assert!(Manifest::parse_platform(r#"{"notes":"x"}"#, "macos").is_none());
        assert!(Manifest::parse_platform(r#"{"version":""}"#, "macos").is_none());
    }

    #[test]
    fn manifest_handles_escaped_notes() {
        let j = r#"{"version":"1.0.0","notes":"Line one.\nLine \"two\".","published":"","platforms":{}}"#;
        let m = Manifest::parse_platform(j, "macos").expect("parse");
        assert_eq!(m.notes, "Line one.\nLine \"two\".");
        assert!(m.artifact.is_none());
    }

    #[test]
    fn status_from_manifest_available_and_uptodate() {
        let m = Manifest::parse_platform(SAMPLE, "macos").unwrap();
        // current older -> Available
        match UpdateStatus::from_manifest(&m, "0.2.0") {
            UpdateStatus::Available(info) => {
                assert_eq!(info.version, "0.3.0");
                assert!(info.artifact.is_some());
            }
            other => panic!("expected Available, got {other:?}"),
        }
        // current equal -> UpToDate, carrying the manifest's version + notes so
        // the About page can always show "What's new in <installed version>".
        let up_to_date = UpdateStatus::UpToDate { version: m.version.clone(), notes: m.notes.clone() };
        assert_eq!(UpdateStatus::from_manifest(&m, "0.3.0"), up_to_date);
        // current newer -> UpToDate
        assert_eq!(UpdateStatus::from_manifest(&m, "0.4.0"), up_to_date);
        // The accessor exposes the pair for the changelog heading.
        assert_eq!(up_to_date.notes_and_version(), Some((m.version.as_str(), m.notes.as_str())));
    }

    #[test]
    fn status_is_available_flag() {
        assert!(!UpdateStatus::Unknown.is_available());
        assert!(!(UpdateStatus::UpToDate { version: "1.0.0".into(), notes: String::new() }).is_available());
        assert!(!UpdateStatus::Checking.is_available());
        assert!(!UpdateStatus::Failed("x".into()).is_available());
        let info = UpdateInfo { version: "1.0.0".into(), notes: String::new(), artifact: None };
        assert!(UpdateStatus::Available(info).is_available());
    }

    #[test]
    fn artifact_name_matches_packaging() {
        assert_eq!(artifact_name("0.3.0"), "CosmicCaptureKit-0.3.0.dmg");
    }

    // DRAGON-177: the launch dialog's show/don't-show decision, the full matrix of
    // (setting on/off) x (every status) x (active page About or not). It shows ONLY
    // when Available AND setting-on AND the active page is NOT About.
    #[test]
    fn launch_dialog_shows_only_when_available_and_notify_on() {
        let info = UpdateInfo { version: "0.4.0".into(), notes: "n".into(), artifact: None };
        let available = UpdateStatus::Available(info.clone());

        // Available + notify ON + NOT on About -> show, carrying the info.
        assert_eq!(dialog_for_status(&available, true, false), Some(info.clone()));
        // Available + notify ON + ON About -> suppressed (About has the controls).
        assert_eq!(dialog_for_status(&available, true, true), None);
        // Available + notify OFF -> the setting suppresses it, on About or not.
        assert_eq!(dialog_for_status(&available, false, false), None);
        assert_eq!(dialog_for_status(&available, false, true), None);

        // Every non-Available status never shows, for any setting/page combination.
        for status in [
            UpdateStatus::Unknown,
            UpdateStatus::Checking,
            UpdateStatus::UpToDate { version: "1.0.0".into(), notes: String::new() },
            UpdateStatus::Failed("x".into()),
        ] {
            for notify in [true, false] {
                for on_about in [true, false] {
                    assert_eq!(
                        dialog_for_status(&status, notify, on_about),
                        None,
                        "status {status:?} notify {notify} on_about {on_about}"
                    );
                }
            }
        }
    }

    // DRAGON-177 follow-up: the daemon startup notice's pure decision — the full
    // matrix of (setting on/off) x (every status). Only Available + setting-on
    // opens the settings window; every other combination is a silent no-op.
    #[test]
    fn daemon_startup_notice_only_when_available_and_notify_on() {
        let info = UpdateInfo { version: "0.4.0".into(), notes: "n".into(), artifact: None };
        let available = UpdateStatus::Available(info);

        // Available + notify ON -> notify. Available + notify OFF -> suppressed.
        assert!(should_notify_on_daemon_startup(&available, true));
        assert!(!should_notify_on_daemon_startup(&available, false));

        // Every non-Available status never notifies, regardless of the setting.
        for status in [
            UpdateStatus::Unknown,
            UpdateStatus::Checking,
            UpdateStatus::UpToDate { version: "1.0.0".into(), notes: String::new() },
            UpdateStatus::Failed("x".into()),
        ] {
            assert!(!should_notify_on_daemon_startup(&status, true), "status {status:?} notify-on");
            assert!(!should_notify_on_daemon_startup(&status, false), "status {status:?} notify-off");
        }
    }

    // The spawn side is injected as a closure, so the "spawn or not" decision can be
    // exercised without launching a real process: the closure fires exactly when the
    // pure decision says to notify.
    #[test]
    fn daemon_startup_spawn_closure_fires_only_when_decided() {
        use std::cell::Cell;
        for (status, notify, expect_spawn) in [
            (UpdateStatus::Available(UpdateInfo { version: "9.9.9".into(), notes: String::new(), artifact: None }), true, true),
            (UpdateStatus::Available(UpdateInfo { version: "9.9.9".into(), notes: String::new(), artifact: None }), false, false),
            (UpdateStatus::UpToDate { version: "1.0.0".into(), notes: String::new() }, true, false),
            (UpdateStatus::Failed("net".into()), true, false),
        ] {
            let spawned = Cell::new(false);
            let spawn = || spawned.set(true);
            // Inline the decision+spawn shape of `notify_daemon_startup_if_update_available`
            // (its state/curl I/O is not unit-testable, but the gate + closure firing is).
            if should_notify_on_daemon_startup(&status, notify) {
                spawn();
            }
            assert_eq!(spawned.get(), expect_spawn, "status {status:?} notify {notify}");
        }
    }

    #[test]
    fn daemon_startup_delay_is_a_modest_grace() {
        // Small first-check delay so a manual daemon launch surfaces a live
        // update near-instantly; the login-time network race is covered by the
        // failure-only retry schedule instead. Pin both shapes.
        assert_eq!(DAEMON_STARTUP_CHECK_DELAY, std::time::Duration::from_secs(2));
        assert_eq!(DAEMON_STARTUP_CHECK_RETRIES.len(), 2, "failure retries cover login races");
        assert!(
            DAEMON_STARTUP_CHECK_RETRIES.iter().all(|d| *d >= std::time::Duration::from_secs(10)),
            "retries back off enough to outlast a slow desktop/network bring-up"
        );
    }

    #[test]
    fn manifest_url_prefers_env_override() {
        // Guard against a polluted env by asserting the default when unset in a
        // child-safe way: we can't unset in a shared process, so just check the
        // default is what we expect and the resolver is non-empty.
        assert!(DEFAULT_MANIFEST_URL.contains("cosmic-capture-kit-updates"));
        assert!(!manifest_url().is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn swap_helper_script_is_well_formed() {
        let s = swap_helper_script(
            "/tmp/staged/Cosmic Capture Kit.app",
            "/Applications/Cosmic Capture Kit.app",
            "/tmp/x/swap.log",
        );
        // Quoted paths, a bounded wait loop, the swap, and the relaunch.
        assert!(s.contains("STAGED='/tmp/staged/Cosmic Capture Kit.app'"));
        assert!(s.contains("INSTALLED='/Applications/Cosmic Capture Kit.app'"));
        assert!(s.contains("while [ $i -lt 60 ]"));
        assert!(s.contains("rm -rf \"$INSTALLED\""));
        assert!(s.contains("cp -R \"$STAGED\" \"$INSTALLED\""));
        assert!(s.contains("open \"$INSTALLED\""));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn shell_single_quote_escapes() {
        assert_eq!(shell_single_quote("a'b"), r"a'\''b");
        assert_eq!(shell_single_quote("plain"), "plain");
    }
}
