//! In-app update channel (DRAGON-175; channel consolidated in DRAGON-226).
//!
//! The channel is the PUBLIC repo's GitHub Releases: `publish-release.yml` (or
//! `scripts/mirror-release.sh`, its manual fallback) attaches a manifest
//! (`update.json`) next to the release's artifacts, and the app polls the stable
//! `releases/latest/download/update.json` URL, compares its `version` against
//! `CARGO_PKG_VERSION`, and where it can, installs in one click: macOS
//! downloads/verifies/swaps the `.app`; Windows downloads/verifies then runs a silent
//! per-user `.msi` install (DRAGON-287; the Windows body lives in
//! `crate::platform::windows::update_install`); a Linux AppImage downloads/verifies
//! then renames the new file over `$APPIMAGE` (DRAGON-532).
//!
//! **[`platform_key`] is a function, not a constant, and that is the load-bearing
//! detail.** Linux ships TWO artifacts from ONE compiled binary, the AppImage and the
//! ZIP (BIN) build, so no `cfg` can tell them apart; only the running process knows,
//! from `$APPIMAGE`. They read SEPARATE manifest entries (`appimage` vs `linux`)
//! because the entry carries the download URL, and a shared key would hand one
//! artifact the other's download. Only the AppImage can install itself
//! ([`one_click_install_available`]); the zip was unpacked wherever the user chose, so
//! there is no location we own to replace and it stays a link to the releases page.
//!
//! HISTORY: while the source repo was fully private the channel was a separate
//! Pages repo (`cosmic-capture-kit-updates`), and `publish-release.yml` shipped to
//! BOTH for a transition period. DRAGON-372 retired it: the Pages repo is gone, so
//! that step could only fail — and because it ran before the release mirror under
//! `set -e`, its failure took the whole publish job down with it. Installs at
//! <= 0.12.0 polled the old URL and can no longer self-update; they must be
//! replaced by hand. Every release from 0.13.0 on carries this file and polls
//! [`DEFAULT_MANIFEST_URL`].
//!
//! Everything here is dependency-light on purpose:
//!   * fetch is a `curl` shell-out (present on macOS and virtually every Linux),
//!   * the manifest is parsed by a tiny hand parser for its FIXED shape (no
//!     `serde_json` in the tree — see CLAUDE.md's "don't add deps lightly"),
//!   * sha256 is the platform's own tool, `shasum -a 256` / `sha256sum` (no hashing
//!     crate in the tree).
//!
//! The pure islands — [`compare_versions`], [`Manifest::parse`],
//! [`UpdateStatus::from_manifest`], [`artifact_name`] — are unit-tested at the
//! file bottom. The I/O (curl / hdiutil / the detached swap helper) is not, by
//! the repo's testing rule.
//!
//! **A Flatpak build has no update channel at all** (DRAGON-561). Updates flow through
//! the Flatpak remote (the store), so a manifest poll would only advertise versions the
//! store delivers on its own schedule, and the updater could never act on one anyway:
//! `/app` is read-only inside the sandbox, so there is no install location we own. The
//! one gate is [`channel_available`] ([`channel_available_for`] over
//! [`crate::util::package_kind`]), and every trigger consults it: the settings-open
//! check (both mint paths), the About page's controls, the on-disk cache seed, and the
//! daemons' startup notice (gated inside [`notify_daemon_startup_if_update_available`],
//! the shared body every resident daemon calls). A Flatpak therefore never runs an
//! update check, and [`platform_key`] never grows a `flatpak` key.
//!
//! **The notes are a SEPARATE question from the channel** (DRAGON-605). That one gate was
//! answering two things at once: "can this build install an update itself", which on a
//! Flatpak is permanently no, and "can this build tell the user what changed", which has
//! nothing to do with who performs the install. Turning the channel off took the About
//! page's "What's new" block down with it, so a Flatpak user could see the store card and
//! nothing about what the store was going to hand them.
//!
//! [`notes_source_for`] is the split, and it is total: EVERY package kind shows notes.
//! What differs is where they come from. A channel build already has them, because
//! [`check_now`] parses them out of the same manifest it compares versions against
//! ([`NotesSource::UpdateCheck`]). A Flatpak runs no check, so it fetches the notes on
//! their own ([`NotesSource::OwnFetch`], [`fetch_release_notes`]). The two predicates are
//! exact complements and a test says so, which is what keeps the relationship stated in
//! code rather than implied by a comment.
//!
//! **Why fetched and not bundled at build time.** Bundling would need no network at all,
//! and it would answer the nicer question ("what is new in what I am running"), so it was
//! the preferred option until the release pipeline was checked. `release.yml` generates
//! the notes in its FINAL job, after every artifact is already built, and a human then
//! edits them inside the `cck-notes` markers before clicking Publish. The shipping notes
//! therefore do not exist when the binary is compiled and change again afterwards, so
//! baking them in is not a matter of adding a build step: it would need a second build
//! after the draft exists, or a hand-maintained notes file in the repo, which is exactly
//! the second source of truth this must not invent.
//!
//! **So the shipped UI answers: what is new in the newest PUBLISHED version.** That is the
//! same question every other build's About page answers. On a channel build that version
//! is either the one you have or the one you can install in a click; on a Flatpak it is
//! the one the store will deliver on its own schedule, and the heading naming a version
//! you do not have yet is the honest signal that it has not arrived.
//!
//! **This is not an update check by the back door**, and four things keep it from becoming
//! one. It runs ONLY when the About page becomes active, once per session, never from a
//! daemon, a capture launch or a settings-window mint. It returns a [`ReleaseNotes`],
//! which carries a version string and markdown and NOTHING installable: no URL, no sha,
//! no size, because [`parse_release_notes`] never reads the manifest's `platforms` object
//! and [`platform_key`] is still never computed on a Flatpak. It never produces an
//! [`UpdateStatus`], so it cannot tint the nav rail, raise the DRAGON-177 dialog or put a
//! button on the Version row. And it never writes the manifest cache, so
//! [`seeded_status_from_cache`] can never read a file a Flatpak wrote. A failed fetch
//! degrades to no notes at all: an error banner about updates would be nonsense on a
//! build that cannot update.

/// The default update channel: the public repo's latest-release `update.json`
/// asset (GitHub serves `releases/latest/download/<asset>` as a stable URL for
/// the newest non-draft release). Overridable at runtime with `CCK_UPDATE_URL`
/// for local E2E testing (point it at a `file://`/`http://localhost` manifest).
pub const DEFAULT_MANIFEST_URL: &str =
    "https://github.com/Frosthaven/cosmic-capture-kit/releases/latest/download/update.json";

/// Environment override for the manifest URL (dev/testing).
pub const MANIFEST_URL_ENV: &str = "CCK_UPDATE_URL";

/// The project page the "Update Now" launch dialog opens on a build with no one-click
/// install, matching the About page's "Open releases" link destination. Since DRAGON-532
/// that is the Linux ZIP (BIN) build specifically, not all of Linux: an AppImage takes the
/// one-click path like macOS and Windows. Compiled everywhere so the plumbing can't drift.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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

/// The payload the CURRENT code writes into the post-update marker (DRAGON-465). It is a
/// version tag, and the only thing it has to be is "not what the old builds wrote".
///
/// What it buys: a marker written by THIS code proves the installer that wrote it also
/// relaunches with [`crate::instance::RESIDENT_ARG`], so a BARE launch found next to it
/// cannot be that relaunch and must be a real keypress. Builds up to and including the one
/// this ticket ships against wrote `"1"`, and could only relaunch bare, so their marker
/// cannot carry that proof. See [`PostUpdateMarker`].
const MARKER_V2: &str = "2";

/// Which installer wrote the post-update marker this launch found, which is really a
/// question about what a BARE launch next to it can mean.
///
/// The distinction exists because the marker outlives the install window it belongs to. An
/// MSI install takes 30 to 90 seconds, the marker is on disk for all of it, and the user can
/// press the capture key in the middle. Without a version there is no way to tell that press
/// apart from the installer's own relaunch, and the DRAGON-465 owed-capture gate answered
/// "post-update relaunch" for both, so a real keypress lost its capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostUpdateMarker {
    /// Written by a build that predates [`MARKER_V2`] (payload `"1"`, empty, or unreadable).
    /// Those installers always relaunched BARE, so a bare launch beside this marker is
    /// ambiguous: it is probably the relaunch, and it might be a keypress. Resolved in favour
    /// of the relaunch, which costs a keypress its capture exactly once, on the single update
    /// that installs this code.
    Legacy,
    /// Written by this code or later. Its installer relaunches with the resident token, so a
    /// BARE launch found beside it is NOT the relaunch and can only be a real keypress.
    V2,
}

/// Write the post-update marker: the installer drops it once the swap helper is really on
/// its way, and the relaunch consumes it to land the user on Settings > About (the new
/// version's "What's new" front and center).
///
/// Always writes [`MARKER_V2`], including on a re-arm. A re-arm is written BY this code, so
/// the proof the payload stands for holds for it too: whatever relaunch was pending has
/// already happened, and a later bare launch is a keypress.
///
/// Writers, and only the first two are installers. The macOS one-click flow and the Windows
/// MSI updater (`crate::platform::windows::update_install`) ARM it. The Windows daemon
/// RE-ARMS it in two places: when a capture-key press arrives mid-install and takes its
/// capture instead of the About window (DRAGON-465), and when a reader took the marker but
/// could not deliver the notes (DRAGON-472), because taking it without showing anything
/// would destroy the only record that they are still owed. See [`post_update_marker_spent`]
/// for that invariant.
///
/// Compiled (and type-checked) everywhere on purpose.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", target_os = "linux")),
    expect(dead_code)
)]
pub(crate) fn write_post_update_marker() {
    if let Some(path) = state_file("post-update") {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(&path, MARKER_V2.as_bytes()) {
            // The path is described, never printed: it sits under the user's home and a
            // username is exactly what the debug log may not carry (CLAUDE.md's privacy rule).
            Ok(()) => log::info!(
                "update: wrote post-update marker ({})",
                crate::diag::path_shape(&path)
            ),
            Err(e) => log::warn!("update: could not write post-update marker: {e}"),
        }
    }
}

/// Consume (remove) the post-update marker and say WHICH installer wrote it, or `None` when
/// there is none to take.
///
/// Reads the payload BEFORE removing, because the answer is the file's content and a removed
/// file has none. An unreadable or unrecognised payload is [`PostUpdateMarker::Legacy`]: the
/// version tag is a POSITIVE proof about the writer, so anything that cannot present it does
/// not get its benefit.
pub fn take_post_update_marker_kind() -> Option<PostUpdateMarker> {
    let path = state_file("post-update")?;
    // Read first: after the remove there is nothing left to classify.
    let payload = std::fs::read_to_string(&path).ok();
    if let Err(e) = std::fs::remove_file(&path) {
        // A marker we cannot remove is a marker that will be found again, so the ONLY safe
        // answer is "no marker" — reporting one we did not consume would let two launches act
        // on the same update. Worth a warn: it silently demotes the post-update handling of
        // this launch to an ordinary one, and nothing else in the log would say why.
        if path.exists() {
            log::warn!(
                "update: found a post-update marker but could not consume it ({e}); treating \
                 this launch as an ordinary one, so Settings > About will not open for it"
            );
        }
        return None;
    }
    let kind = match payload.as_deref().map(str::trim) {
        Some(MARKER_V2) => PostUpdateMarker::V2,
        _ => PostUpdateMarker::Legacy,
    };
    log::info!("update: consumed post-update marker ({kind:?})");
    Some(kind)
}

/// Consume the post-update marker; true when this launch is the first one after an update
/// install. The kind-blind wrapper over [`take_post_update_marker_kind`], for the readers
/// that only ask "is this a post-update launch": every non-Windows resident, and `app::run`.
pub fn take_post_update_marker() -> bool {
    take_post_update_marker_kind().is_some()
}

/// The arguments a post-update RELAUNCH must be started with, given the persisted
/// `resident` setting. Pure + unit-tested.
///
/// An installer's last act is to start the app again. That relaunch has to say what it is,
/// because this binary reads a launch with no arguments as "the user pressed the capture
/// key" (see [`crate::instance::daemon_intent_from_args`]): on Windows a bare launch with
/// `resident` on becomes the tray daemon AND spawns the overlay it thinks it owes the user,
/// and Linux's resident has the same rule. A finished update owes the user nothing of the
/// kind, so DRAGON-465 makes the relaunch state its intent instead:
///
/// * `resident` on: come back as the RESIDENT, exactly as the autostart entry does. The
///   daemon then consumes the post-update marker and opens Settings > About.
/// * `resident` off: come back with no arguments. There is no daemon to raise, and the
///   marker turns this launch into a settings window on About inside `app::run` — the
///   historical non-resident path, unchanged.
///
/// The setting is the only input: it is what decides which of the two shapes the app was
/// running in before the update, and the relaunch's whole job is to restore that shape.
///
/// The macOS installer does not use this yet — its swap helper relaunches with `open`,
/// which raises the daemon WITHOUT the capture-intent rule (`mac/daemon.rs`'s `run` takes
/// no intent argument), so the mac flow was never affected. It lives in the shared tree
/// anyway because the rule belongs to the update channel, not to one platform's installer,
/// and because Linux would inherit the exact Windows defect the day it gains an in-app
/// install flow.
///
/// **That day arrived** (DRAGON-532): Linux now has one, and it relaunches through here.
// Consumed by the Windows swap helper (`platform::windows::update_install::run_swap`), the
// Linux AppImage installer's relaunch helper, and the tests below.
#[cfg_attr(not(any(target_os = "windows", target_os = "linux")), allow(dead_code))]
pub fn post_update_relaunch_args(resident: bool) -> &'static [&'static str] {
    if resident {
        &[crate::instance::RESIDENT_ARG]
    } else {
        &[]
    }
}

/// Is a post-update marker waiting, WITHOUT consuming it?
///
/// A PEEK, deliberately, and the distinction matters. [`take_post_update_marker`] is
/// single-shot precisely so two launches cannot both act on one update, so a caller that
/// only needs to know "did an update just happen" must not take it: the launch that goes on
/// to become the resident is the one that has to consume it and open About.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn post_update_marker_pending() -> bool {
    state_file("post-update").is_some_and(|p| p.exists())
}

// DRAGON-473 REMOVED `signal_capture_after_lost_lock(daemon_intent, post_update_pending)`, a
// Linux-only predicate that answered "should a launch which could not take the daemon lock ask
// the running daemon to capture?" with `!(daemon_intent && post_update_pending)`.
//
// Keep the reason, because the deletion looks like a regression from here: it was written
// (DRAGON-532) to stop a post-update relaunch that lost the lock race from dropping an
// unrequested overlay on screen seconds after an update, which is a real bug and was observed
// for real while testing the AppImage self-update. That behaviour is NOT gone. It moved.
//
// The predicate's flaw was that it granted the capture to every OTHER daemon-intent launch, so
// the autostart entry and the settings tray toggle still asked a live resident for a capture
// when they had only ever wanted a tray (DRAGON-473). Once the general rule is "only capture
// intent hands over a capture", the post-update case is simply one instance of it and this
// function has nothing left of its own to say.
//
// Its replacement is `instance::held_lock_action`, which all three platforms consult, and which
// still distinguishes the post-update launch — it returns `RestoreAbout` there, versus
// `ProbeOnly` for an ordinary `resident` launch, so Linux keeps its distinct log line and
// Windows keeps delivering the release notes. A second vocabulary for one decision is the thing
// this codebase most consistently refuses; two of them disagreeing about which launches may ask
// for a capture is exactly how the DRAGON-473 hole survived DRAGON-471.

/// What a launch that has just become the resident must do, given the post-update marker it
/// found. Both fields together, because they are one decision: whether to spawn the capture
/// this launch would otherwise owe, and whether to write the marker back for a later launch
/// to deliver the About window.
///
/// One function rather than two so the pair can never disagree. "The capture proceeds" and
/// "About is deferred, so re-arm the marker" are the same fact stated twice, and splitting
/// them into two predicates would let a future edit break the tie silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub struct PostUpdateStart {
    /// Spawn the capture overlay this launch owes the user.
    pub owes_capture: bool,
    /// Write the marker back: the release notes are still owed, and THIS launch is not the
    /// one delivering them.
    pub rearm_marker: bool,
}

/// What a launch that has just become the resident should do about the capture it may owe
/// and the marker it may have found. Pure + unit-tested.
///
/// The owed capture (DRAGON-181 on Linux, DRAGON-438 on Windows) exists because a user who
/// presses the capture key with no daemon running becomes the daemon and would otherwise get
/// nothing. A post-update RELAUNCH reaches that same code path with nobody having pressed
/// anything, so the debt is not real there and the overlay is noise over the release notes.
/// Telling the two apart is the whole job, and the marker's version is what makes it possible.
///
/// The table, and why each cell is what it is:
///
/// | capture intent | marker | capture | re-arm |
/// |---|---|---|---|
/// | no  | any     | no  | no  | A daemon-intent launch owes no capture. About is delivered by this launch's own marker check, so nothing is deferred. |
/// | yes | none    | YES | no  | An ordinary keypress. Untouched by any of this. |
/// | yes | Legacy  | no  | no  | The TRANSITIONAL cell. A pre-DRAGON-465 installer relaunches BARE, so a bare launch beside its marker is almost certainly that relaunch; suppress the overlay and show About. It costs a real keypress its capture if the user pressed the key during that ONE update's install window, which is the price of the old installer having left no way to tell. |
/// | yes | V2      | YES | YES | A real keypress. This marker's writer relaunches with the resident token, so a bare launch CANNOT be the relaunch. The capture proceeds, and the marker goes back so the relaunch (or the next daemon start) still shows the notes. |
///
/// The V2 cell is the one this exists for. An MSI install runs for 30 to 90 seconds with the
/// marker on disk, and a user pressing the capture key in that window is not unusual. Before
/// DRAGON-465's review this dropped their capture AND spent the marker, so the finished
/// update then came back to a marker-free disk, read as an ordinary launch, and pulsed a
/// surprise overlay with no About: one wrong answer becoming two wrong outcomes.
///
/// Suppressing a capture is the heavier cost and it is confined to `Legacy`. The rule
/// elsewhere in this launch path is the one [`crate::instance::held_lock_action`] states:
/// a person pressing a key keeps their capture, because a redundant About window is cheap and
/// a dropped capture is not.
// Consumed by the Windows tray daemon's `finish_startup`; dead elsewhere until another
// resident gains an in-app install flow (Linux's owed capture has the identical shape).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn post_update_start(capture_intent: bool, marker: Option<PostUpdateMarker>) -> PostUpdateStart {
    match (capture_intent, marker) {
        // Daemon intent: no capture is owed, and this launch shows About itself.
        (false, _) => PostUpdateStart { owes_capture: false, rearm_marker: false },
        // A keypress with no update in flight: the ordinary case.
        (true, None) => PostUpdateStart { owes_capture: true, rearm_marker: false },
        // The transitional cell: an old installer's bare relaunch is indistinguishable from a
        // keypress, and the relaunch is the likelier reading.
        (true, Some(PostUpdateMarker::Legacy)) => {
            PostUpdateStart { owes_capture: false, rearm_marker: false }
        }
        // A keypress DURING an install: the new installer never relaunches bare, so this is a
        // person. Serve them, and hand the release notes to the next daemon-intent start.
        (true, Some(PostUpdateMarker::V2)) => {
            PostUpdateStart { owes_capture: true, rearm_marker: true }
        }
    }
}

/// Whether an attempt to restore the post-update About window SPENT the marker, given
/// whether the settings child was actually STARTED. Pure + unit-tested.
///
/// The rule it names (DRAGON-472): **any reader that can OBSERVE its own delivery failure
/// must write the marker back.** Taking the marker is a `remove_file`, so a reader that
/// takes it and then delivers nothing has destroyed the only record that the notes are still
/// owed, and no later launch can put them up.
///
/// Deliberately NOT stated as a universal invariant, because it is not enforceable at every
/// take. A reader can only put the marker back for a failure it SEES. Both Windows takers
/// (`claim_or_signal`'s held-lock restore and `finish_startup`'s About spawn) see theirs,
/// because the spawn seam returns whether the child was created, and both write it back. The
/// Linux resident's take is unreachable (no in-app install flow writes a marker there), and
/// the macOS daemon's take is left alone under the byte-identity rule, so a mac About spawn
/// that fails still defers nothing: that residual is real and named rather than papered over.
/// A process that is KILLED between the take and the spawn observes nothing and cannot
/// comply either; `finish_startup`'s doc carries that window explicitly.
///
/// `about_child_started` is exactly what the spawn seam proves: `CreateProcess` succeeded.
/// It is NOT "the About page is on screen". The child can still exit early, and the
/// settings-mutex case is the concrete one: if a settings window is already open, the child
/// pokes the live holder to come forward and exits, and that holder does NOT switch to the
/// About tab. Treating the spawn as delivery is the right trade anyway, because the
/// alternative is an ack protocol for a release-notes window, and a marker that survives a
/// successful spawn would re-open About on every later start.
///
/// It exists as a named function rather than a bare `!` at the call site because it is a
/// RULE, and the rule is what a reader needs to find.
// Consumed by both Windows About-restore sites; dead elsewhere until another platform grows
// an in-app install flow.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn post_update_marker_spent(about_child_started: bool) -> bool {
    about_child_started
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

/// The platform key this build looks for in the manifest's `platforms` map. It is
/// the JSON object key under `"platforms"` (see [`platform_object`]): the Windows
/// `.msi` artifact lands at `platforms.windows.{url,sha256,size}` (DRAGON-287),
/// exactly the shape [`Manifest::parse_platform`] slices for `macos`/`linux`.
///
/// A FUNCTION, not a constant, because Linux ships TWO artifacts from one binary
/// (DRAGON-532). The AppImage and the plain ZIP (BIN) build are the same compiled code, so
/// no `cfg` can tell them apart; only the running process knows, from `$APPIMAGE`. They
/// must not share a key: the entry carries the download URL, and handing an AppImage the
/// zip's URL (or the reverse) would install the wrong artifact the moment either side gains
/// a one-click install.
///
/// There is deliberately NO `flatpak` key, and a Flatpak build never computes one
/// (DRAGON-561): every path that reads the manifest is gated off by [`channel_available`]
/// before it gets here, and [`one_click_install_available`]'s Flatpak arm answers before
/// its Linux arm would ask. The store owns Flatpak updates, so a key here could only ever
/// hand the sandbox a download it has no way to install.
///
/// **This never reads [`crate::util::PackageKind`], and it must not start** (DRAGON-614).
/// The two look like the same question and are not. This one picks which ARTIFACT an update
/// downloads, and it answers from `cfg!(target_os)`, `$APPIMAGE` and `cfg!(target_arch)`
/// directly. `PackageKind` names how the running build was DELIVERED, for the About badge
/// and the debug log. Adding cases there (DRAGON-614 added `MacOs` and `Windows`) therefore
/// cannot merge a key here or change a download URL, which is the property that made that
/// change safe. Routing this through `PackageKind` would also lose the architecture, which
/// that enum does not carry and which a wrong answer for would install a binary the CPU
/// cannot run.
pub fn platform_key() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
    #[cfg(target_os = "linux")]
    {
        // Two axes, deliberately read differently: the artifact KIND is a runtime fact (both
        // kinds are this same binary), the ARCH is a compile-time one.
        linux_platform_key(
            crate::util::appimage_path().is_some(),
            cfg!(target_arch = "aarch64"),
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        "unknown"
    }
}

/// **Pure**, unit-tested: whether a build of this package kind has an in-app update
/// channel at all (DRAGON-561).
///
/// A Flatpak does not, twice over. Updates flow through the Flatpak remote (the store),
/// so a manifest poll would only advertise versions the store delivers on its own
/// schedule, and the updater could never act on one anyway: `/app` is read-only inside
/// the sandbox, so there is no install location to write over (the same structural fact
/// [`one_click_install_available`] answers from). Every other kind keeps the channel
/// exactly as it was. Kept as its own decision, in the shared tree, so every trigger
/// asks the one question and `cargo test` proves the answer on any host.
pub fn channel_available_for(kind: crate::util::PackageKind) -> bool {
    kind != crate::util::PackageKind::Flatpak
}

/// Whether THIS build has an in-app update channel: [`channel_available_for`] over the
/// real package kind. The one gate every update-check trigger consults (the
/// settings-open check on both mint paths, the About page's controls, the on-disk cache
/// seed, and the daemons' startup notice), so no two of them can disagree about whether
/// a check may run.
pub fn channel_available() -> bool {
    channel_available_for(crate::util::package_kind())
}

/// Where a build's "What's new" release notes come from (DRAGON-605).
///
/// The enum is deliberately total: there is no "no notes" variant, because every package
/// kind shows them. That is the whole point of the split. [`channel_available_for`] says
/// who performs an install, and this says who fetches the text; the two used to be one
/// answer, and a Flatpak lost its changelog to a rule about installing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotesSource {
    /// The update check already carries them. [`check_now`] parses `notes` out of the
    /// very manifest it compares versions against, so a build with a channel gets the
    /// notes for free and must not fetch them a second time.
    UpdateCheck,
    /// A notes-only fetch ([`fetch_release_notes`]), because this build runs no update
    /// check at all and nothing else would ever fill them in.
    OwnFetch,
}

/// **Pure**, unit-tested: where a build of this package kind gets its release notes.
///
/// The body is one line on purpose. Deriving it FROM [`channel_available_for`], rather
/// than re-deciding it against `PackageKind`, is what makes the two exact complements by
/// construction: a future package kind that gains or loses a channel moves both answers
/// together, and cannot end up checking for updates while fetching notes separately (two
/// requests for one manifest) or running no check while showing no notes (the DRAGON-605
/// bug). `notes_source_complements_the_channel_gate` pins it anyway, because a later
/// "simplification" could easily inline the wrong constant here.
pub fn notes_source_for(kind: crate::util::PackageKind) -> NotesSource {
    if channel_available_for(kind) { NotesSource::UpdateCheck } else { NotesSource::OwnFetch }
}

/// Where THIS build gets its release notes: [`notes_source_for`] over the real package
/// kind. The About page's one question, asked the same way the update triggers ask
/// [`channel_available`].
pub fn notes_source() -> NotesSource {
    notes_source_for(crate::util::package_kind())
}

/// The "What's new" text for a published version, with nothing installable attached.
///
/// The absence of an artifact is the load-bearing part, not an omission. A
/// [`PlatformArtifact`] carries a download URL, and a value carrying one is one refactor
/// away from a button that offers it. This type cannot grow that button because it has
/// nothing to hand it: see [`parse_release_notes`], which never reads `platforms` at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseNotes {
    /// The published version the notes describe. On a store-managed build this may be
    /// AHEAD of `CARGO_PKG_VERSION`, and the About heading says so by naming it.
    pub version: String,
    /// The markdown notes, guaranteed non-empty (an empty `notes` field yields `None`
    /// from the parser rather than an empty block on the page).
    pub notes: String,
}

/// **Pure**, unit-tested: read a manifest for its notes and nothing else.
///
/// Deliberately NOT [`Manifest::parse`]. That one resolves [`platform_key`] to slice out
/// this build's artifact, and a Flatpak must never compute a platform key: there is no
/// `flatpak` entry to find, so it would silently read the `linux` one and hold a download
/// URL for the ZIP build. Reading only the two text fields means the notes path has no
/// artifact to leak, whatever anyone does to the UI later.
///
/// `None` when the manifest is malformed, when `version` is blank, or when there are no
/// notes to show. All three are the same outcome for the caller: leave the page as it is.
pub fn parse_release_notes(json: &str) -> Option<ReleaseNotes> {
    let version = json_string_field(json, "version")?;
    if version.trim().is_empty() {
        return None;
    }
    let notes = json_string_field(json, "notes").unwrap_or_default();
    if notes.trim().is_empty() {
        return None;
    }
    Some(ReleaseNotes { version, notes })
}

/// Fetch ONLY the release notes from the manifest. Blocking; call from a background task
/// (`Task::perform(app::off_thread(...))`), exactly as [`check_now`] is called.
///
/// The one caller is the About page on a build whose [`notes_source`] is
/// [`NotesSource::OwnFetch`]. Every failure, a missing `curl`, no network, a timeout, a
/// malformed manifest, comes back as `None` and shows nothing. There is deliberately no
/// [`UpdateStatus::Failed`] equivalent here: an "update check failed" notice on a build
/// that cannot update would be reporting a fault in a feature it does not have.
///
/// It does NOT [`write_manifest_cache`]. The cache exists to seed an [`UpdateStatus`] at
/// the next settings launch, which is precisely the state this build must not have, and a
/// file written here would be read by [`seeded_status_from_cache`] the moment anyone
/// relaxed its gate.
pub fn fetch_release_notes() -> Option<ReleaseNotes> {
    let url = manifest_url();
    match fetch_text(&url) {
        Ok(body) => {
            let notes = parse_release_notes(&body);
            if notes.is_none() {
                log::info!("release notes: the manifest carried none");
            }
            notes
        }
        Err(reason) => {
            // Logged, never surfaced: this build cannot update, so a user-facing update
            // error would name a feature it does not have. The reason is our own short
            // copy, not the server's, so nothing user-specific reaches the log.
            log::info!("release notes: not fetched ({reason})");
            None
        }
    }
}

/// Whether THIS build can install an update itself, or can only point the user at the
/// releases page to download one by hand.
///
/// macOS and Windows always can: both install into a layout their packaging defined, so
/// there is a known thing to replace. Linux depends on WHICH artifact is running, which is
/// not a `cfg` (both are the same compiled binary). An AppImage is one file we can rename
/// over. The ZIP (BIN) build was unpacked wherever the user chose, possibly onto a read-only
/// mount, possibly renamed, with its own sidecars beside it; there is no install location we
/// own, so there is nothing safe to swap and the honest answer is a download link.
pub fn one_click_install_available() -> bool {
    // A FLATPAK can never install itself, and this is structural rather than a policy we are
    // choosing to honour (`lab/flatpak`). `/app` and `/usr` are read-only mounts inside the
    // sandbox, so there is nothing to write over even if we wanted to; the packaging system
    // owns updates. Checked before the per-OS arms because it is true on every platform a
    // Flatpak can run on, and because the Linux arm below would otherwise answer from
    // `$APPIMAGE`, which a Flatpak does not have and would read as the ZIP build: the right
    // answer for the wrong reason, and one that would break the moment anything else changed.
    if crate::util::flatpak_sandboxed() {
        return false;
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        true
    }
    #[cfg(target_os = "linux")]
    {
        // Expressed through `platform_key` on purpose: "can install itself" and "reads an
        // appimage manifest entry" are the same fact, and deriving one from the other means
        // the button and the download URL can never disagree about which artifact this is.
        // Any architecture's AppImage qualifies, which is what `is_appimage_key` centralises.
        is_appimage_key(platform_key())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

/// The four Linux manifest keys, one per (artifact kind x architecture) pair. Named rather
/// than spelled inline so the reader here and the publisher's writer cannot drift apart on a
/// typo, which would silently look like "no update available" rather than like an error.
///
/// **The unsuffixed pair means x86_64, and must stay that way.** They were the only Linux
/// keys when they shipped (`linux` in DRAGON-521, `appimage` in DRAGON-532), so every
/// installed build already looks for exactly those strings. A published manifest is read by
/// versions we can no longer change; renaming a live key to `linux-x86_64` would stop
/// updates dead for everyone already on it, while adding a suffixed sibling cannot.
///
/// All four are Linux-only, and honestly dead elsewhere: no macOS or Windows build ever
/// looks at them. `test` is in each gate so the tests that pin the strings still run on
/// every host, which is where a typo would otherwise reach a release unnoticed.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const APPIMAGE_PLATFORM_KEY: &str = "appimage";
/// The aarch64 AppImage (DRAGON-529). See [`APPIMAGE_PLATFORM_KEY`] for why this one carries
/// the suffix and the x86_64 one does not.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const APPIMAGE_AARCH64_PLATFORM_KEY: &str = "appimage-aarch64";
/// The x86_64 ZIP (BIN) build (DRAGON-521).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const LINUX_PLATFORM_KEY: &str = "linux";
/// The aarch64 ZIP (BIN) build (DRAGON-529).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const LINUX_AARCH64_PLATFORM_KEY: &str = "linux-aarch64";

/// **Pure**, unit-tested: [`platform_key`]'s Linux decision, with both inputs injected.
///
/// The two axes are genuinely different in kind, which is why they are separate parameters
/// rather than one. `is_appimage` is a RUNTIME fact, read from `$APPIMAGE` at the edge by
/// [`crate::util::appimage_path`], because the AppImage and the zip are the same compiled
/// binary. `is_aarch64` is a COMPILE-TIME fact, a `cfg!(target_arch)`, because an x86_64
/// build simply cannot run on aarch64. Both must reach the key: getting the arch wrong is
/// worse than getting the kind wrong, since the download would not even execute.
///
/// Kept OUT of any platform module so it is compiled and tested on every host, per the
/// repo's decision-in-the-shared-tree rule.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux_platform_key(is_appimage: bool, is_aarch64: bool) -> &'static str {
    match (is_appimage, is_aarch64) {
        (true, false) => APPIMAGE_PLATFORM_KEY,
        (true, true) => APPIMAGE_AARCH64_PLATFORM_KEY,
        (false, false) => LINUX_PLATFORM_KEY,
        (false, true) => LINUX_AARCH64_PLATFORM_KEY,
    }
}

/// **Pure**, unit-tested: whether `key` names an AppImage, for ANY architecture.
///
/// ONE place knows the answer. The install button and the manifest reader must agree about
/// what this build is, and a second architecture is exactly the kind of addition that
/// otherwise updates one of them and forgets the other.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_appimage_key(key: &str) -> bool {
    key == APPIMAGE_PLATFORM_KEY || key == APPIMAGE_AARCH64_PLATFORM_KEY
}

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
        Manifest::parse_platform(json, platform_key())
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
// Consumed by `notify_daemon_startup_if_update_available` on EVERY resident platform
// (DRAGON-335: Windows joined them when DRAGON-237 landed its tray daemon) + the pure
// tests below, so it needs no dead-code allowance anywhere.
pub const DAEMON_STARTUP_CHECK_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// Retry schedule for the daemon startup check when the fetch FAILS (network
/// not up yet, typical right after login). Each entry is a wait before the
/// next attempt. Up-to-date / Available results never retry; only failures do.
// Consumed by every resident platform's daemon startup path + the pure tests below
// (DRAGON-335), so it needs no dead-code allowance anywhere.
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
// Consumed by `notify_daemon_startup_if_update_available` on every resident platform +
// the pure tests below (DRAGON-335), so it needs no dead-code allowance anywhere.
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
///
/// DRAGON-335: NOT cfg-gated. This was `macos|linux` only, written when Windows had no
/// resident daemon — DRAGON-237 then added one and this gate was never revisited, so the
/// Windows tray daemon silently never checked for updates. Since `resident` DEFAULTS ON
/// for Windows (`state/schema.rs` `default_resident`), that was the typical Windows launch,
/// and the launch-time check only ever happens when something opens a settings window.
/// The body is fully portable, so every resident daemon now shares it. Keep it ungated.
pub fn notify_daemon_startup_if_update_available(spawn_settings: impl FnOnce()) {
    // DRAGON-561: a Flatpak build has no update channel; the store owns its updates.
    // Gated HERE, in the shared body every resident daemon calls, rather than in each
    // daemon, so no platform's daemon can drift back into polling.
    if !channel_available() {
        log::info!("daemon: update check skipped (this package kind has no in-app update channel)");
        return;
    }
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

/// The canonical macOS artifact filename for a version on the update channel.
/// Kept in ONE place so the workflow (which names the file) and any app-side
/// expectation agree. Matches `mac-package.sh`'s
/// `CosmicCaptureKit-<version>-aarch64.dmg` (arch-suffixed since DRAGON-508).
/// In the binary only the macOS install flow consumes it (Linux has no published
/// artifact yet); the tests cover it everywhere.
///
/// **Not how the download is ADDRESSED.** The URL always comes from the manifest
/// ([`Artifact::url`]), so a renamed asset needs no app change and an old install
/// keeps updating. This is only what the downloaded file is called in the private
/// temp dir on the way to `hdiutil`, and it stays in step with the packaging name
/// so a log line or a stray temp file still reads as the thing it is.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn artifact_name(version: &str) -> String {
    format!("CosmicCaptureKit-{version}-aarch64.dmg")
}

// ---------------------------------------------------------------------------
// I/O: fetch + verify + install (not unit-tested — the repo's rule).
//
// Every child spawned below is reaped against a deadline (DRAGON-499). The rule is the
// record path's (DRAGON-118) and it holds here for the same reason: a tool's own
// `--max-time` is the tool policing ITSELF, which is worth nothing when the tool is wedged,
// is the wrong tool (a `PATH` shim), or is stuck below its own clock in a resolver or a TLS
// handshake. The update check runs on EVERY settings mint, so an unbounded one is a hang the
// user never asked for.
// ---------------------------------------------------------------------------

/// How long `curl` gets to fetch the manifest, and the `--max-time` it is handed.
///
/// ONE constant for both, so the tool's own policing and the Rust-side deadline cannot drift
/// apart: see [`fetch_argv`], which is where the two meet.
const FETCH_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// How long `curl` gets to download a release artifact, and the `--max-time` it is handed.
///
/// Shared by both install flows (`platform::windows::update_install` reads it too), so the
/// download's budget is named once rather than repeated as a literal per platform.
// Dead on Linux: no published artifact there, so nothing downloads one (the tests still
// pin its relation to the reap deadline on every platform).
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", target_os = "linux")),
    allow(dead_code)
)]
pub(crate) const DOWNLOAD_BUDGET: std::time::Duration = std::time::Duration::from_secs(600);

/// How long the checksum tool (`shasum` on mac, `certutil` on Windows) gets.
///
/// It has no clock of its own, so this is the whole bound. It is an OUTER ring rather than an
/// expectation: hashing a file we just finished writing is seconds of local disk read, and a
/// tool still going after two minutes is stuck, not slow.
// Dead on Linux for the same reason: only the two install flows verify a download.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", target_os = "linux")),
    allow(dead_code)
)]
pub(crate) const HASH_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// How long `hdiutil` gets to attach or detach the downloaded image (macOS).
///
/// Generous because an attach can checksum the whole image on the way in; still a bound,
/// because a `hdiutil` waiting on a dialog or a stuck volume never returns at all.
#[cfg(target_os = "macos")]
const IMAGE_BUDGET: std::time::Duration = std::time::Duration::from_secs(180);

/// How long `cp -R` gets to copy the new `.app` out of the mounted image (macOS).
///
/// The bundle is a few hundred megabytes off a just-mounted local image, so this is minutes
/// of headroom on a job that takes seconds.
#[cfg(target_os = "macos")]
const COPY_BUDGET: std::time::Duration = std::time::Duration::from_secs(300);

/// The extra time a child gets to EXIT after its own budget should have ended its work.
///
/// A transfer that hits `--max-time` still has to wind down and flush, so the reap must not
/// race the tool's own timeout; anything still alive after the budget PLUS this grace is
/// wedged. The same shape as `cloud::http`'s `REAP_GRACE`, smaller because nothing here
/// uploads.
const REAP_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// How often the reap loop wakes to check whether the child has exited.
const REAP_POLL: std::time::Duration = std::time::Duration::from_millis(25);

/// The wall time a child of budget `budget` is given before it is killed.
///
/// Pure; unit-tested. Kept as its own function so every call site's deadline is built the one
/// way, and so the relation "the deadline is strictly outside the tool's own timeout" is a
/// property a test can state rather than a habit.
fn reap_deadline(budget: std::time::Duration) -> std::time::Duration {
    budget.saturating_add(REAP_GRACE)
}

/// What a bounded run produced.
pub(crate) struct Reaped {
    /// The child exited ON ITS OWN with a success status. A killed child is never a success,
    /// so a call site that only cares whether the job worked can read this alone.
    pub success: bool,
    /// The deadline expired and the child was killed. A killed child is a named failure at
    /// the call site, never a hang and never a partial answer treated as an answer. Read it
    /// where the DISTINCTION is worth a different sentence to the user.
    pub killed: bool,
    /// Whatever the child wrote to stdout, when the caller PIPED stdout. Empty otherwise.
    pub stdout: Vec<u8>,
}

/// Run `cmd` to completion, reaping it against [`reap_deadline`] and killing one that
/// outlives it (DRAGON-499).
///
/// Modelled on [`crate::record::wait_or_kill`] and on `cloud::http`'s `run_bounded`, and here
/// for the reason DRAGON-118 put the first one there: nothing may wait unboundedly. It is a
/// third copy rather than a call into either, because those two carry their domains with them
/// (ffmpeg's stderr ring, curl's stdin config and host allowlist) and this one has to reap
/// whatever tool the platform hands the update flow.
///
/// The call site owns the child's stdio, and this function drains only what was PIPED: a
/// child that has filled its stdout pipe blocks in `write` and never exits, so an undrained
/// pipe would turn every deadline into the full grace period and then a kill. `Stdio::null`
/// and inherited stdio are left exactly as the call site set them, which is what keeps the
/// success path byte-identical to the `.output()` / `.status()` calls this replaced.
///
/// `what` names the job for the log line and nothing else; it never carries a path.
pub(crate) fn run_bounded(
    cmd: &mut std::process::Command,
    budget: std::time::Duration,
    what: &str,
) -> std::io::Result<Reaped> {
    run_bounded_within(cmd, reap_deadline(budget), what)
}

/// [`run_bounded`] with the allowance INJECTED rather than derived, the repo's `foo_with`
/// seam: the kill path is then something a test can reach in milliseconds instead of waiting
/// out a real budget plus [`REAP_GRACE`].
fn run_bounded_within(
    cmd: &mut std::process::Command,
    allowance: std::time::Duration,
    what: &str,
) -> std::io::Result<Reaped> {
    use std::io::Read as _;

    let mut child = cmd.spawn()?;
    let stdout_reader = child.stdout.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });
    // Drained and DROPPED, exactly as `.output()` did with the stderr nothing here reads.
    let stderr_reader = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut sink = Vec::new();
            let _ = pipe.read_to_end(&mut sink);
        })
    });

    let deadline = std::time::Instant::now() + allowance;
    let mut killed = false;
    let mut success = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                success = status.success();
                break;
            }
            Ok(None) => {}
            Err(e) => return Err(e),
        }
        if std::time::Instant::now() >= deadline {
            log::warn!(
                "update: {what} was still running {}s after it started, so it was stopped",
                allowance.as_secs()
            );
            let _ = child.kill();
            let _ = child.wait();
            killed = true;
            break;
        }
        std::thread::sleep(REAP_POLL);
    }
    // Joined AFTER the child is gone, so every read has hit EOF and no join can block.
    let stdout = stdout_reader.and_then(|h| h.join().ok()).unwrap_or_default();
    if let Some(h) = stderr_reader {
        let _ = h.join();
    }
    Ok(Reaped { success, killed, stdout })
}

/// The argv `curl` gets for a manifest fetch.
///
/// Pure; unit-tested, which is the point: the `--max-time` it carries and the deadline
/// [`run_bounded`] reaps against are built from the SAME [`FETCH_BUDGET`], and a test says so.
fn fetch_argv(url: &str) -> Vec<String> {
    vec![
        "-fsSL".to_string(),
        "--max-time".to_string(),
        FETCH_BUDGET.as_secs().to_string(),
        url.to_string(),
    ]
}

/// Fetch the manifest and decide the status. Blocking; call from a background
/// task (`Task::perform(app::off_thread(check))`). Degrades to
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

/// `curl -fsSL --max-time <FETCH_BUDGET> <url>` -> body, or a short error reason.
fn fetch_text(url: &str) -> Result<String, String> {
    // Quiet spawn (DRAGON-236): `curl` (present on Windows 10+) is console-subsystem and
    // this fires on every settings launch (the auto update check) — a bare spawn flashes a
    // console before the settings window. Route through the console-free seam. Byte-identical
    // off Windows.
    //
    // The stdio is what `.output()` used to set for us (both pipes captured, stdin closed);
    // it is spelled out now because the reap is ours (DRAGON-499) and `run_bounded` drains
    // exactly what the call site pipes.
    let out = run_bounded(
        crate::util::quiet_command("curl")
            .args(fetch_argv(url))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
        FETCH_BUDGET,
        "the update check",
    )
    .map_err(|_| "Could not run curl to check for updates.".to_string())?;
    if out.killed {
        // Not "could not reach": we reached something, and it would not let go. Named, so the
        // About page says what happened instead of showing "Checking..." forever.
        return Err("The update server stopped answering, so the check was ended.".to_string());
    }
    if !out.success {
        return Err("Could not reach the update server.".to_string());
    }
    String::from_utf8(out.stdout).map_err(|_| "Update information was not valid text.".to_string())
}

/// The checksum tool for this platform, as a ready `Command`: `shasum -a 256` on macOS
/// (perl, in the base system), `sha256sum` on Linux (coreutils, on every distro we target).
/// Both print `<hash>  <path>`, which is why [`file_sha256`]'s parse is shared.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn sha256_command() -> std::process::Command {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("shasum");
        cmd.arg("-a").arg("256");
        cmd
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("sha256sum")
    }
}

/// Compute the lowercase-hex sha256 of a file. Used by every install flow that verifies a
/// download before running it: macOS (DRAGON-175) and the Linux AppImage (DRAGON-532).
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn file_sha256(path: &std::path::Path) -> Result<String, String> {
    // Bounded reap (DRAGON-499): the stdio is what `.output()` set, the deadline is ours.
    let out = run_bounded(
        sha256_command()
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
        HASH_BUDGET,
        "the download checksum",
    )
    .map_err(|_| "Could not run the checksum tool to verify the download.".to_string())?;
    // A killed `shasum` lands here too: it is not a success, and its partial stdout must
    // never be read as a hash.
    if !out.success {
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
/// Constructed by the macOS one-click install and the Windows MSI updater
/// (DRAGON-287); the type (and its Linux match in `update_settings`) is compiled
/// everywhere so the message plumbing can't drift.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", target_os = "linux")),
    allow(dead_code)
)]
pub enum InstallOutcome {
    /// The new `.app` is staged and a detached helper is armed to swap + relaunch
    /// once this process (and the daemon) exit. The caller must now quit.
    Staged,
    /// The install failed; the reason is user-facing.
    Failed(String),
}

/// Run this platform's one-click install for `info`, BLOCKING.
///
/// The portable seam the UI calls, so no call site carries a `cfg` (the repo's shape: a
/// portable `fn` whose body branches, not a branching caller). Both entry points, the About
/// page's install button and the launch dialog's "Install now", go through here, which is
/// what keeps them from drifting on which platforms install and which only link out.
///
/// Only reachable when [`one_click_install_available`] is true; the UI does not offer the
/// button otherwise. The Linux arm re-checks anyway and fails with a readable message,
/// because the alternative is a button that silently does nothing.
pub fn install(info: &UpdateInfo) -> InstallOutcome {
    #[cfg(target_os = "macos")]
    {
        install_macos(info)
    }
    #[cfg(target_os = "windows")]
    {
        install_windows(info)
    }
    #[cfg(target_os = "linux")]
    {
        install_linux_appimage(info)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = info;
        InstallOutcome::Failed("This platform has no in-app installer.".to_string())
    }
}

/// Run the full macOS one-click install for `info`, BLOCKING. Downloads the dmg
/// to a temp dir, verifies its sha256, mounts + stages the `.app`, writes a
/// detached swap helper, and returns [`InstallOutcome::Staged`] (the caller then
/// quits: the helper waits for full exit, swaps `/Applications`, and relaunches).
///
/// Reached through [`install`]; see [`install_linux_appimage`] for the Linux counterpart,
/// which is much shorter because an AppImage is one file and installing it is a rename.
#[cfg(target_os = "macos")]
pub fn install_macos(info: &UpdateInfo) -> InstallOutcome {
    match install_macos_inner(info) {
        Ok(()) => InstallOutcome::Staged,
        Err(e) => InstallOutcome::Failed(e),
    }
}

/// Run the full Windows one-click install for `info`, BLOCKING (DRAGON-287). Downloads
/// the `.msi`, verifies its sha256, writes the post-update marker, and spawns a detached
/// swap helper that installs silently (`msiexec /qn`, per-user — no UAC) and relaunches;
/// returns [`InstallOutcome::Staged`] (the caller then signals the daemon to quit + exits).
/// One-line dispatch into the closed Windows body (strict split — LOG law 7).
#[cfg(target_os = "windows")]
pub fn install_windows(info: &UpdateInfo) -> InstallOutcome {
    crate::platform::windows::update_install::install(info)
}

/// Run the full Linux AppImage one-click install for `info`, BLOCKING (DRAGON-532).
/// Downloads the new `.AppImage` beside the running one, verifies its sha256, renames it
/// over `$APPIMAGE`, and arms a detached helper to relaunch once this process exits.
///
/// **Only for a build launched from an `.AppImage`.** The plain ZIP (BIN) Linux build was
/// unpacked wherever the user chose, under no install layout we defined, so there is nothing
/// here we can safely replace; it stays notify-only and the About page keeps sending it to
/// the releases page. [`crate::util::appimage_path`] returning `None` is that case, and it
/// is an error rather than a silent no-op because reaching here at all means the UI offered
/// a button it should not have.
///
/// **Why this is so much smaller than the macOS flow.** There is no image to mount, nothing
/// to stage, and no waiting for the old process to exit first: an AppImage is ONE file, so
/// installing it is a rename. `rename(2)` is atomic and is safe while the old version runs,
/// because the running instance's FUSE mount pins the old inode until it exits. What we must
/// NOT do is write to `$APPIMAGE` in place, which fails `ETXTBSY` (Text file busy) against
/// the running program. The same lesson landed in the AppImage build script, which assembles
/// aside and renames for exactly this reason.
///
/// **The filename is never parsed and never changed.** The user's autostart entry
/// (`platform::linux::autostart` writes `Exec=$APPIMAGE resident`) and whatever capture
/// shortcut they bound both name this exact path, so a version-stamped rename would leave
/// them pointing at a file that no longer exists. The release asset stays versioned for the
/// download page; the installed file keeps whatever the user called it, with new contents
/// underneath. This mirrors macOS shipping a versioned dmg into a fixed `.app`.
#[cfg(target_os = "linux")]
pub fn install_linux_appimage(info: &UpdateInfo) -> InstallOutcome {
    match install_linux_appimage_inner(info) {
        Ok(()) => InstallOutcome::Staged,
        Err(e) => InstallOutcome::Failed(e),
    }
}

#[cfg(target_os = "linux")]
fn install_linux_appimage_inner(info: &UpdateInfo) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    let artifact = info
        .artifact
        .as_ref()
        .ok_or_else(|| "This update has no AppImage download.".to_string())?;

    let target = crate::util::appimage_path().ok_or_else(|| {
        "This build is not an AppImage, so it cannot replace itself. Download the new version \
         from the releases page."
            .to_string()
    })?;
    // Resolve symlinks before swapping. `$APPIMAGE` is normally already the real path (the
    // runtime derives it from `/proc/self/exe`, which the kernel resolves), but the README
    // tells users they can keep a short-named symlink pointing at the versioned file, so
    // being explicit costs one syscall and removes an assumption. It matters because
    // `rename` over a SYMLINK replaces the LINK with a regular file: the user's tidy setup
    // would quietly become two copies, the newer one wearing the symlink's name. Falling
    // back to the unresolved path keeps a build with an odd `$APPIMAGE` working as before.
    let target = std::fs::canonicalize(&target).unwrap_or(target);
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| "Could not find the folder the AppImage is in.".to_string())?;

    // Stage BESIDE the target, never in a temp dir. The install is a rename, and a rename
    // across filesystems fails with EXDEV; `/tmp` is a tmpfs on most desktops, so staging
    // there would work right up until the swap and then fail on every real machine. Leading
    // dot so a file manager open on the folder does not flash it at the user mid-download.
    let staged = parent.join(format!(".cck-update-{}.AppImage.part", std::process::id()));

    // Creating the staging file IS the writability probe, and doing it FIRST is the point:
    // an AppImage in /opt or /usr/local/bin belongs to root, and telling the user that after
    // a 30MB download rather than before it is a worse experience for no benefit.
    std::fs::File::create(&staged).map_err(|_| {
        "Could not save the update next to the AppImage. Move the AppImage somewhere you own, \
         such as ~/Applications, then try again."
            .to_string()
    })?;

    // Everything that can leave a partial file behind runs inside here, so ONE cleanup
    // covers every failure. After a successful rename there is nothing left to remove.
    let staged_ok = (|| -> Result<(), String> {
        // 1) Download. Bounded reap (DRAGON-499), stdio inherited as elsewhere.
        let max_time = DOWNLOAD_BUDGET.as_secs().to_string();
        let out = run_bounded(
            std::process::Command::new("curl")
                .args(["-fsSL", "--max-time", max_time.as_str(), "-o"])
                .arg(&staged)
                .arg(&artifact.url),
            DOWNLOAD_BUDGET,
            "the update download",
        )
        .map_err(|_| "Could not run curl to download the update.".to_string())?;
        if out.killed {
            return Err("The update download stopped responding, so it was ended.".to_string());
        }
        if !out.success {
            return Err("The update download failed.".to_string());
        }

        // 2) Verify sha256. Skipped only if the manifest omitted one, matching macOS; the
        //    publisher always writes it (`publish-release.yml`).
        if !artifact.sha256.trim().is_empty() {
            let got = file_sha256(&staged)?;
            if got != artifact.sha256.to_lowercase() {
                return Err("The download failed its integrity check.".to_string());
            }
        }

        // 3) Carry the old file's mode across before the swap, so a user who tightened it
        //    keeps their choice, and guarantee execute: a fresh download is 0644, and a
        //    non-executable AppImage is just a file the desktop cannot open.
        let existing = std::fs::metadata(&target).ok().map(|m| m.permissions().mode());
        std::fs::set_permissions(
            &staged,
            std::fs::Permissions::from_mode(installed_appimage_mode(existing)),
        )
        .map_err(|_| "Could not make the downloaded AppImage executable.".to_string())?;

        // 4) The install itself. Atomic, and safe against the running copy (see the doc).
        std::fs::rename(&staged, &target)
            .map_err(|_| "Could not replace the AppImage with the new version.".to_string())
    })();
    if staged_ok.is_err() {
        // A dotfile left next to the user's AppImage is litter they would have to find
        // themselves, so clear it on every failure path.
        let _ = std::fs::remove_file(&staged);
    }
    staged_ok?;

    // The new version is on disk from here on. Anything below can only affect whether the
    // app comes BACK by itself, never whether the update landed, and the messages say so.

    // The relaunched app consumes this to land on Settings > About with the freshly
    // installed version's notes.
    write_post_update_marker();

    // Relaunch as whatever the user actually runs: `resident` brings the tray daemon back,
    // a bare launch does not start one they never asked for. The rule already existed for
    // the Windows installer and belongs to the update channel, not to one platform.
    let resident = crate::state::load().resident;
    let work = std::env::temp_dir().join(format!("cck-update-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&work);
    let script_path = work.join("relaunch.sh");
    let log_path = work.join("relaunch.log");
    let script = appimage_relaunch_script(
        &target.to_string_lossy(),
        post_update_relaunch_args(resident),
        std::process::id(),
        &log_path.to_string_lossy(),
    );
    let wrote = std::fs::File::create(&script_path)
        .and_then(|mut f| f.write_all(script.as_bytes()))
        .is_ok();
    // Deliberately never reaped: spawned to OUTLIVE us, since its whole job is to wait until
    // we are gone. Its own wait loop is bounded, in the script.
    let spawned = wrote
        && std::process::Command::new("/bin/sh")
            .arg(&script_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok();
    if !spawned {
        // Report it honestly rather than as a failed install: the swap already happened, so
        // claiming failure here would send the user to re-download what they already have.
        // `Failed` also means the caller does NOT quit, so this message stays on screen.
        return Err("The update was installed, but this app could not restart itself. \
                    Quit and start Cosmic Capture Kit again to finish."
            .to_string());
    }
    Ok(())
}

/// **Pure**, unit-tested: the file mode a freshly downloaded AppImage should carry.
///
/// Preserve what the replaced file had, so a user who tightened permissions keeps that, and
/// force owner-execute regardless: `curl -o` writes 0644, and an AppImage without the
/// execute bit cannot be launched by the desktop, the autostart entry, or us. `None` (the
/// old file's mode was unreadable) falls back to the mode a downloaded AppImage is normally
/// given by hand.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn installed_appimage_mode(existing: Option<u32>) -> u32 {
    existing.unwrap_or(0o755) | 0o100
}

/// Build the detached relaunch helper for the AppImage install. Pure string builder so its
/// shape (quoting, the bounded wait, the relaunch arguments) is unit-testable.
///
/// Far smaller than the macOS [`swap_helper_script`], and deliberately so: the swap has
/// ALREADY happened by the time this runs, because replacing an AppImage is a rename that is
/// safe against the running copy. There is no install left to do. The only job is to wait
/// out the old processes so the relaunch does not race the single-instance lock, then start
/// the new file.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn appimage_relaunch_script(appimage: &str, args: &[&str], caller_pid: u32, log: &str) -> String {
    // Each argument single-quoted separately, so the command line is safe even though these
    // are our own constants today. `shell_single_quote` escapes the INSIDE of a literal; the
    // surrounding quotes are the caller's, exactly as in the macOS helper's template.
    let args = args
        .iter()
        .map(|a| format!(" '{}'", shell_single_quote(a)))
        .collect::<String>();
    format!(
        r#"#!/bin/sh
set -u
LOG='{log}'
APPIMAGE='{appimage}'
CALLER={caller_pid}
exec >>"$LOG" 2>&1
echo "[cck-update] helper started $(date)"

# Two waits, both bounded (~30s each), because the relaunch must not race the
# single-instance lock and leave the user with nothing running.
#
# First the process that spawned us, BY PID. Exact, and immune to the AppImage living at a
# path containing regex characters, which the pgrep below is not: `pgrep -f` takes an ERE, so
# a `+` or `(` in the user's folder name could make it match nothing and fall straight
# through. The pid check is the one that must not fail.
i=0
while [ $i -lt 60 ]; do
  kill -0 "$CALLER" 2>/dev/null || break
  i=$((i + 1))
  sleep 0.5
done
echo "[cck-update] caller gone after ${{i}} polls"

# Then any SIBLING still holding the lock: the resident daemon, a preview editor, a settings
# window. Best effort by path, since their pids are not ours to know. This helper cannot
# match itself, its own argv being `/bin/sh <script>`.
j=0
while [ $j -lt 60 ]; do
  pgrep -f "$APPIMAGE" >/dev/null 2>&1 || break
  j=$((j + 1))
  sleep 0.5
done
echo "[cck-update] siblings clear after ${{j}} polls"

# A short settle so the flock released at exit is really gone before we contend for it.
sleep 1

# setsid so the new process outlives this helper rather than dying with it.
setsid "$APPIMAGE"{args} >/dev/null 2>&1 &
echo "[cck-update] relaunched"
"#,
        log = shell_single_quote(log),
        appimage = shell_single_quote(appimage),
        caller_pid = caller_pid,
        args = args,
    )
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

    // 1) Download. Bounded reap (DRAGON-499); the stdio is left inherited, as `.status()` had
    //    it, so nothing about a successful download changes.
    let max_time = DOWNLOAD_BUDGET.as_secs().to_string();
    let out = run_bounded(
        std::process::Command::new("curl")
            .args(["-fsSL", "--max-time", max_time.as_str(), "-o"])
            .arg(&dmg_path)
            .arg(&artifact.url),
        DOWNLOAD_BUDGET,
        "the update download",
    )
    .map_err(|_| "Could not run curl to download the update.".to_string())?;
    if out.killed {
        return Err("The update download stopped responding, so it was ended.".to_string());
    }
    if !out.success {
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
    let attach = run_bounded(
        std::process::Command::new("hdiutil")
            .args(["attach", "-nobrowse", "-readonly", "-mountpoint"])
            .arg(&mount)
            .arg(&dmg_path),
        IMAGE_BUDGET,
        "the update image mount",
    )
    .map_err(|_| "Could not run hdiutil to mount the update.".to_string())?;
    if !attach.success {
        // Covers the killed case too: a killed `hdiutil` mounted nothing, and the next step
        // would look for an app bundle in an empty directory.
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
        let out = run_bounded(
            std::process::Command::new("cp")
                .arg("-R")
                .arg(&app_in_dmg)
                .arg(&staged_app),
            COPY_BUDGET,
            "the update staging copy",
        )
        .map_err(|_| "Could not copy the new app.".to_string())?;
        if !out.success {
            // A killed `cp` leaves a half-copied bundle, which must never be staged; the
            // caller's error path detaches the image and the temp dir goes with the session.
            return Err("Could not copy the new app from the update image.".to_string());
        }
        Ok(staged_app)
    })();

    // Always detach, whether staging succeeded or not. Best effort, but still bounded: this
    // runs on the install worker, and a `hdiutil detach` that never returns would strand it
    // (DRAGON-499).
    let _ = run_bounded(
        std::process::Command::new("hdiutil")
            .args(["detach", "-quiet"])
            .arg(&mount),
        IMAGE_BUDGET,
        "the update image detach",
    );

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
    // session so it survives this process's exit. The ONE child here that is deliberately
    // never reaped: it is spawned to OUTLIVE us (it waits for this process to be gone before
    // it swaps), so there is nothing to bound. Its own wait loop is bounded, in the script.
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

/// Escape a string for safe embedding inside a single-quoted `sh` literal. Shared by the
/// macOS swap helper and the Linux AppImage relaunch helper, both of which interpolate
/// user-controlled paths into a script.
///
/// `test` is in the gate so the script builders that call it stay testable on Windows too
/// (they are compiled there under `cfg(test)` for exactly that reason).
#[cfg(any(target_os = "macos", target_os = "linux", test))]
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
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

    /// DRAGON-465. The bug this pins: the Windows installer relaunched the app with NO
    /// arguments, `main` read that as "the user pressed the capture key", and a resident
    /// install came back with the About page AND a capture overlay over it.
    ///
    /// The assertion that matters is the second one — the relaunch argv is run through the
    /// SAME predicate `main` uses, so the two can never drift into disagreeing about what a
    /// post-update relaunch is asking for.
    #[test]
    fn a_post_update_relaunch_never_asks_for_a_capture() {
        // Resident on: the relaunch says so, exactly as the autostart entry does.
        assert_eq!(post_update_relaunch_args(true), [crate::instance::RESIDENT_ARG]);
        // Resident off: nothing to raise; the marker turns this into a settings launch.
        assert!(post_update_relaunch_args(false).is_empty());

        // The property, stated through `main`'s own reader: a resident relaunch is daemon
        // intent, so the daemon it starts owes no overlay.
        assert!(crate::instance::daemon_intent_from_args(post_update_relaunch_args(true)));
        // A non-resident relaunch is not daemon intent, and must not be: with `resident`
        // off there is no daemon branch to reach at all, and the launch has to fall through
        // to `app::run` for the marker to become the About window.
        assert!(!crate::instance::daemon_intent_from_args(post_update_relaunch_args(false)));

        // Neither shape may ever carry a capture-mode flag: those are what `main` turns into
        // a mode/kind, and they would open an overlay even with the daemon out of the way.
        for resident in [true, false] {
            for arg in post_update_relaunch_args(resident) {
                assert!(
                    !arg.starts_with("--"),
                    "a post-update relaunch must pass no CLI flags, got {arg}"
                );
            }
        }

        // The argv has to be spendable AS argv. `Command` is cross-platform, so compiling
        // this on the Linux gate type-checks the one line of the Windows swap helper that
        // no machine here can build. Constructed and dropped; nothing is spawned.
        for resident in [true, false] {
            let mut cmd = std::process::Command::new("cosmic-capture-kit");
            cmd.args(post_update_relaunch_args(resident));
            assert_eq!(cmd.get_args().count(), post_update_relaunch_args(resident).len());
        }
    }

    /// DRAGON-472: the marker is consumed exactly when the About window was delivered. A
    /// reader that takes it and then fails has destroyed the only record that the release
    /// notes are still owed, so it must put the marker back.
    #[test]
    fn the_post_update_marker_is_only_spent_by_a_delivered_about_window() {
        assert!(
            post_update_marker_spent(true),
            "the settings child started: the marker did its job"
        );
        assert!(
            !post_update_marker_spent(false),
            "no child started: the notes are still owed, so the marker must go back"
        );
    }

    /// DRAGON-465, the second half: the receiving side refuses the owed capture on a
    /// post-update relaunch, whatever the argv said. This is what covers the ONE update
    /// the argv fix cannot — the one whose swap helper is still the old exe.
    ///
    /// Every cell of the table, because the interesting ones are the two that look alike:
    /// a bare launch beside a LEGACY marker (probably the old installer's relaunch) and a
    /// bare launch beside a V2 marker (certainly a person, since the new installer never
    /// relaunches bare).
    #[test]
    fn a_post_update_relaunch_is_never_owed_a_capture() {
        let legacy = Some(PostUpdateMarker::Legacy);
        let v2 = Some(PostUpdateMarker::V2);

        // An ordinary keypress with no update in flight: capture, nothing deferred.
        assert_eq!(
            post_update_start(true, None),
            PostUpdateStart { owes_capture: true, rearm_marker: false }
        );
        // The transitional cell: a pre-DRAGON-465 installer relaunched bare, so suppress.
        // Nothing is re-armed, because About is delivered by this very launch.
        assert_eq!(
            post_update_start(true, legacy),
            PostUpdateStart { owes_capture: false, rearm_marker: false }
        );
        // A REAL keypress during an install window. The capture must survive, and the
        // release notes must not be thrown away with it.
        assert_eq!(
            post_update_start(true, v2),
            PostUpdateStart { owes_capture: true, rearm_marker: true }
        );
        // Daemon intent owes no capture and defers nothing, marker or not.
        for marker in [None, legacy, v2] {
            assert_eq!(
                post_update_start(false, marker),
                PostUpdateStart { owes_capture: false, rearm_marker: false },
                "daemon intent with marker {marker:?}"
            );
        }

        // The invariant that ties the two fields together: the marker is only ever re-armed
        // by a launch that is NOT delivering About, which here means one taking its capture
        // instead. Never re-arm on a launch that shows the notes.
        for intent in [true, false] {
            for marker in [None, legacy, v2] {
                let plan = post_update_start(intent, marker);
                if plan.rearm_marker {
                    assert!(plan.owes_capture, "re-armed without taking the capture instead");
                    assert!(marker.is_some(), "re-armed a marker that was never there");
                }
            }
        }
    }

    /// DRAGON-465 review, the CASCADE. One press of the capture key during an MSI install
    /// used to cost the user both things: the capture was suppressed AND the marker was
    /// spent, so the installer's own relaunch arrived to a marker-free disk, read as an
    /// ordinary launch, and put up a surprise overlay with no release notes.
    ///
    /// Walked as a sequence, because neither step is wrong on its own.
    #[test]
    fn a_keypress_during_an_install_keeps_its_capture_and_still_gets_the_notes() {
        // Step 1: the install is running, its V2 marker is on disk, the user presses the
        // capture key. Bare launch, no daemon (the app quit for the update), so this launch
        // becomes the daemon with capture intent.
        let press = post_update_start(true, Some(PostUpdateMarker::V2));
        assert!(press.owes_capture, "the user pressed a key and must get their overlay");
        assert!(press.rearm_marker, "the notes are still owed, so the marker goes back");

        // Step 2: the marker written back is a V2 one (`write_post_update_marker` only ever
        // writes that payload), so the state the next launch finds is unchanged in kind.
        assert_eq!(MARKER_V2.trim(), MARKER_V2, "the payload carries no stray whitespace");

        // Step 3: the swap helper's relaunch arrives. It is DAEMON intent, because
        // `post_update_relaunch_args` gave it the resident token, and it finds the re-armed
        // marker. No capture, nothing deferred: this is the launch that shows About.
        let relaunch_argv = post_update_relaunch_args(true);
        assert!(crate::instance::daemon_intent_from_args(relaunch_argv));
        let relaunch = post_update_start(false, Some(PostUpdateMarker::V2));
        assert!(!relaunch.owes_capture, "the relaunch must not add an overlay of its own");
        assert!(!relaunch.rearm_marker, "the relaunch delivers the notes, so it spends them");

        // Both things happened exactly once across the sequence.
        assert_eq!(
            [press.owes_capture, relaunch.owes_capture].iter().filter(|x| **x).count(),
            1,
            "exactly one overlay: the one the user asked for"
        );
    }

    /// The marker's version tag has to survive a round trip through the file's text, and
    /// anything that is NOT the tag has to read as Legacy. This is the classification
    /// `take_post_update_marker_kind` applies to the payload it reads.
    #[test]
    fn only_the_exact_version_payload_reads_as_v2() {
        // The classifier, as the taker applies it (trimmed payload vs the tag).
        let classify = |payload: Option<&str>| match payload.map(str::trim) {
            Some(MARKER_V2) => PostUpdateMarker::V2,
            _ => PostUpdateMarker::Legacy,
        };
        assert_eq!(classify(Some(MARKER_V2)), PostUpdateMarker::V2);
        // Trailing newlines happen to any file written by hand or by a text editor.
        assert_eq!(classify(Some("2\n")), PostUpdateMarker::V2);
        assert_eq!(classify(Some(" 2 \r\n")), PostUpdateMarker::V2);
        // What the shipped builds wrote, plus every shape of "we cannot tell".
        assert_eq!(classify(Some("1")), PostUpdateMarker::Legacy);
        assert_eq!(classify(Some("")), PostUpdateMarker::Legacy);
        assert_eq!(classify(None), PostUpdateMarker::Legacy, "unreadable is not proof");
        assert_eq!(classify(Some("22")), PostUpdateMarker::Legacy);
        assert_eq!(classify(Some("v2")), PostUpdateMarker::Legacy);
        // The tag must never be the old payload, or every legacy marker would read as V2.
        assert_ne!(MARKER_V2, "1");
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
    fn manifest_parses_windows_entry_and_stays_backward_compatible() {
        // DRAGON-287: a manifest carrying BOTH mac and windows artifacts resolves the
        // windows one for the windows key (url + sha lowercased + size), independent of
        // the host running the test.
        let both = r#"{
          "version": "0.13.0",
          "notes": "Windows MSI + background updates.",
          "published": "2026-07-19",
          "platforms": {
            "macos":   { "url": "https://example.com/CosmicCaptureKit-0.13.0.dmg", "sha256": "AA", "size": 111 },
            "windows": { "url": "https://example.com/CosmicCaptureKit-0.13.0.msi", "sha256": "BBCD", "size": 222 }
          }
        }"#;
        let w = Manifest::parse_platform(both, "windows").expect("parse windows");
        let a = w.artifact.expect("windows artifact");
        assert_eq!(a.url, "https://example.com/CosmicCaptureKit-0.13.0.msi");
        assert_eq!(a.sha256, "bbcd"); // lowercased on parse
        assert_eq!(a.size, 222);
        // The same manifest still resolves the mac entry (both coexist).
        assert!(Manifest::parse_platform(both, "macos").unwrap().artifact.is_some());
        // A mac-ONLY manifest yields no windows artifact — no update offered on Windows,
        // yet it still parses (backward compat with the pre-Windows channel).
        let mac_only = r#"{"version":"0.13.0","notes":"n","published":"d","platforms":{"macos":{"url":"u","sha256":"a","size":1}}}"#;
        let m = Manifest::parse_platform(mac_only, "windows").expect("parse mac-only for windows");
        assert_eq!(m.version, "0.13.0");
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

    /// The name has to match what `scripts/mac-package.sh` writes, including the architecture
    /// suffix DRAGON-508 added. Pinned as a literal rather than rebuilt from parts, because
    /// this string's whole job is to be the same string the packaging script produces.
    #[test]
    fn artifact_name_matches_packaging() {
        assert_eq!(artifact_name("0.3.0"), "CosmicCaptureKit-0.3.0-aarch64.dmg");
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

    /// DRAGON-335 regression guard. The startup notice was `cfg(macos|linux)` for so long
    /// that DRAGON-237's Windows daemon shipped without ever checking for updates — the gate
    /// was written when Windows had no resident and nobody revisited it. This takes the
    /// function's ADDRESS (never calls it — calling would fire a real network check), so the
    /// build FAILS on any target where the notice is compiled out. A behavioral test could
    /// not have caught this: the bug was the absence of code on one platform.
    #[test]
    fn daemon_startup_notice_is_compiled_on_every_platform() {
        let _f: fn(fn()) = notify_daemon_startup_if_update_available;
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

    // DRAGON-241: the platform key must be the JSON object key under `platforms` for THIS
    // build (what `platform_object` slices), so an artifact can resolve. Windows used to
    // fall through to "unknown" and could never match one.
    #[test]
    fn platform_key_is_the_manifest_json_key() {
        #[cfg(target_os = "windows")]
        assert_eq!(platform_key(), "windows");
        #[cfg(target_os = "macos")]
        assert_eq!(platform_key(), "macos");
        // Linux is asserted by `linux_platform_key_tests` instead: the answer depends on
        // `$APPIMAGE`, and `cargo test` must not care whether the runner happens to have it.
        // Whatever the key is, a manifest carrying an artifact under it must parse for
        // this platform (proves the value is a usable `platforms.<key>` selector).
        let key = platform_key();
        let json = format!(
            r#"{{"version":"9.9.9","notes":"","published":"","platforms":{{"{key}":{{"url":"https://example.com/a","sha256":"","size":1}}}}}}"#
        );
        let m = Manifest::parse(&json).expect("parse for this platform's key");
        assert!(m.artifact.is_some(), "artifact under the platform key must resolve");
    }

    #[test]
    fn manifest_url_prefers_env_override() {
        // Guard against a polluted env by asserting the default when unset in a
        // child-safe way: we can't unset in a shared process, so just check the
        // default is what we expect and the resolver is non-empty. The channel is
        // the PUBLIC repo's latest-release manifest (DRAGON-226) — the stable
        // `releases/latest/download` form, never the legacy Pages URL.
        assert!(DEFAULT_MANIFEST_URL.ends_with("/releases/latest/download/update.json"));
        assert!(DEFAULT_MANIFEST_URL.contains("github.com/Frosthaven/cosmic-capture-kit/"));
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

    // Compiled wherever the helper is (DRAGON-532 added the Linux relaunch script as a
    // second consumer), so the escaping stays covered on both.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn shell_single_quote_escapes() {
        assert_eq!(shell_single_quote("a'b"), r"a'\''b");
        assert_eq!(shell_single_quote("plain"), "plain");
    }
}

/// DRAGON-499: every child on the update path is reaped against a deadline, and the deadline
/// is built from the same constant the tool is told to police itself with.
#[cfg(test)]
mod reap_bounds_tests {
    use super::*;
    use std::time::Duration;

    /// The relation the whole ticket rests on: the Rust-side deadline is strictly OUTSIDE the
    /// budget the tool was given, so a tool that honours its own timeout is never killed for
    /// winding down, and one that ignores it always is.
    #[test]
    fn every_deadline_sits_outside_the_budget_it_belongs_to() {
        for budget in [FETCH_BUDGET, DOWNLOAD_BUDGET, HASH_BUDGET] {
            assert!(
                reap_deadline(budget) > budget,
                "a {budget:?} budget must be reaped LATER than it, not at it"
            );
            assert_eq!(reap_deadline(budget), budget + REAP_GRACE);
        }
        // A zero budget (nobody passes one, but the arithmetic must not underflow into an
        // instant kill) still gets the grace.
        assert_eq!(reap_deadline(Duration::ZERO), REAP_GRACE);
        // And the grace is a real one: a zero grace would race every healthy tool.
        assert!(!REAP_GRACE.is_zero());
    }

    /// The check runs on every settings mint, so its two clocks (curl's `--max-time` and our
    /// reap) must come from ONE number. This is what stops a later edit from changing the
    /// argv and leaving the deadline behind, which would silently restore the unbounded wait
    /// for anything slower than the old literal.
    #[test]
    fn the_fetch_argv_carries_the_budget_the_reap_is_built_from() {
        let argv = fetch_argv("https://example.invalid/update.json");
        let at = argv
            .iter()
            .position(|a| a == "--max-time")
            .expect("the manifest fetch must police itself as well as being reaped");
        assert_eq!(argv[at + 1], FETCH_BUDGET.as_secs().to_string());
        assert_eq!(
            argv.last().map(String::as_str),
            Some("https://example.invalid/update.json")
        );
        // Nothing else: -f (fail on HTTP error), -s -S (quiet but keep errors), -L (follow).
        assert_eq!(argv[0], "-fsSL");
        assert_eq!(argv.len(), 4);
    }

    /// A wedged child is KILLED, not waited on. The allowance is injected so this costs the
    /// suite milliseconds; the real path derives it from [`reap_deadline`].
    ///
    /// This is the DRAGON-118 property, stated for the update path: the failure mode it
    /// exists to prevent is not a slow update check, it is a settings window that never
    /// closes because a `curl` (or a `PATH` shim pretending to be one) never exits.
    #[cfg(unix)]
    #[test]
    fn a_child_that_will_not_exit_is_killed_rather_than_waited_on() {
        let began = std::time::Instant::now();
        let out = run_bounded_within(
            std::process::Command::new("sleep").arg("30"),
            Duration::from_millis(200),
            "a test child",
        )
        .expect("spawning `sleep` must work on any unix");
        assert!(out.killed, "the child outlived its allowance, so it had to be killed");
        assert!(!out.success, "a killed child is a failure, never a partial answer");
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "the reap took {:?}, so it waited on the child instead of killing it",
            began.elapsed()
        );
    }

    /// And the success path is untouched: a child that finishes on its own reports its own
    /// status and hands back everything it wrote, which is what `fetch_text` reads as the
    /// manifest body.
    #[cfg(unix)]
    #[test]
    fn a_child_that_finishes_keeps_its_status_and_its_output() {
        let out = run_bounded(
            std::process::Command::new("echo")
                .arg("hello")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
            FETCH_BUDGET,
            "a test child",
        )
        .expect("spawning `echo` must work on any unix");
        assert!(out.success);
        assert!(!out.killed);
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
    }
}

// DRAGON-473: `signal_capture_after_lost_lock_tests` lived here. The cases it pinned were not
// dropped — they moved to `instance::held_lock_action`'s test module with the decision itself,
// including the one this ticket exists for (the post-update relaunch that lost the lock race
// and must stay quiet). See the tombstone above `post_update_relaunch_args` for why the
// function went away.

/// DRAGON-532: the file mode a freshly downloaded AppImage is given before it replaces the
/// running one.
#[cfg(test)]
mod installed_appimage_mode_tests {
    use super::installed_appimage_mode;

    /// The ordinary case: the file we are replacing was 0755, so the new one is too.
    #[test]
    fn it_preserves_the_replaced_files_mode() {
        assert_eq!(installed_appimage_mode(Some(0o755)), 0o755);
    }

    /// THE CASE THIS EXISTS FOR. `curl -o` writes 0644, and an AppImage without the execute
    /// bit cannot be launched by the desktop, the autostart entry, or the relaunch helper.
    /// A swap that dropped it would leave the user with an app that will not start.
    #[test]
    fn it_always_grants_owner_execute() {
        assert_eq!(installed_appimage_mode(Some(0o644)) & 0o100, 0o100);
        assert_eq!(installed_appimage_mode(None) & 0o100, 0o100);
    }

    /// A user who tightened permissions (no group/other access) keeps that choice; we add
    /// execute, never breadth.
    #[test]
    fn it_never_widens_access() {
        let out = installed_appimage_mode(Some(0o700));
        assert_eq!(out, 0o700, "0700 already has owner execute, so nothing changes");
        assert_eq!(installed_appimage_mode(Some(0o600)) & 0o077, 0, "group/other stay closed");
    }

    /// An unreadable old mode falls back to what a hand-downloaded AppImage is normally
    /// given, rather than to something the desktop cannot launch.
    #[test]
    fn an_unknown_previous_mode_falls_back_to_executable() {
        assert_eq!(installed_appimage_mode(None), 0o755);
    }
}

/// DRAGON-532: the detached helper that relaunches after the AppImage swap.
#[cfg(test)]
mod appimage_relaunch_script_tests {
    use super::{appimage_relaunch_script, post_update_relaunch_args};

    fn script(resident: bool) -> String {
        appimage_relaunch_script(
            "/home/u/Apps/Cosmic.AppImage",
            post_update_relaunch_args(resident),
            4242,
            "/tmp/cck-update-1/relaunch.log",
        )
    }

    /// The resident user gets their tray daemon back; a non-resident user gets a plain
    /// launch and NOT a daemon they never asked for.
    #[test]
    fn the_relaunch_matches_how_the_user_runs_the_app() {
        assert!(script(true).contains(r#"setsid "$APPIMAGE" 'resident'"#));
        assert!(script(false).contains(r#"setsid "$APPIMAGE" >"#));
    }

    /// The helper waits for the old processes so the relaunch does not race the
    /// single-instance lock, and EVERY wait is BOUNDED (DRAGON-118 applies to every wait we
    /// write): a wedged old process must not strand the helper forever.
    #[test]
    fn every_wait_for_the_old_processes_is_bounded() {
        let s = script(true);
        assert!(s.contains("while [ $i -lt 60 ]"), "the caller wait has a fixed ceiling");
        assert!(s.contains("while [ $j -lt 60 ]"), "the sibling wait has a fixed ceiling");
        assert_eq!(s.matches("while ").count(), 2, "no unbounded loop crept in");
    }

    /// The caller is waited on BY PID, not by path. `pgrep -f` takes an ERE, so an AppImage
    /// living under a folder with a `+` or `(` in its name could match nothing and fall
    /// straight through to a relaunch that races the still-running old process for the lock.
    /// The pid check cannot fail that way, which is why it is the primary signal and the
    /// path-based sweep only mops up siblings afterwards.
    #[test]
    fn the_caller_is_waited_on_by_pid_not_by_path() {
        let s = script(true);
        assert!(s.contains("CALLER=4242"), "the caller's pid is baked in");
        assert!(s.contains(r#"kill -0 "$CALLER""#), "and waited on exactly");
        // Ordering matters: the exact wait must come first, so a broken pattern in the
        // best-effort one cannot skip it. Matched on the CODE, not the prose (the comment
        // above the pid loop names `pgrep -f` too).
        let by_pid = s.find(r#"kill -0 "$CALLER""#).expect("pid wait present");
        let by_path = s.find(r#"pgrep -f "$APPIMAGE""#).expect("sibling wait present");
        assert!(by_pid < by_path, "the exact wait runs before the best-effort one");
    }

    /// Paths are interpolated into a shell script, so they are single-quoted. A path with a
    /// quote in it must not be able to end the literal and run anything.
    #[test]
    fn paths_are_quoted_against_breaking_out() {
        let s = appimage_relaunch_script(
            "/home/u/it's here/Cosmic.AppImage",
            &[],
            4242,
            "/tmp/cck-update-1/relaunch.log",
        );
        // The sh idiom for a quote inside a single-quoted literal: close, escaped quote,
        // reopen.
        assert!(s.contains(r#"APPIMAGE='/home/u/it'\''s here/Cosmic.AppImage'"#));
        // And the raw quote never survives unescaped, which is what would end the literal
        // early and hand the rest of the path to the shell as code.
        assert!(!s.contains("it's here"), "no unescaped quote reaches the script");
    }

    /// We GENERATE this script and never parse it at build time, so a syntax error would
    /// first surface on a user's machine, mid-update, with the app already swapped and
    /// exiting. `sh -n` parses without executing, which is the cheapest possible guard.
    /// (`reap_bounds_tests` sets the precedent for shelling out in a unit test here.)
    #[cfg(unix)]
    #[test]
    fn the_generated_script_is_valid_sh() {
        use std::io::Write as _;
        for (label, s) in [("resident", script(true)), ("bare", script(false))] {
            let mut child = std::process::Command::new("/bin/sh")
                .arg("-n")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawning `/bin/sh` must work on any unix");
            child
                .stdin
                .take()
                .expect("stdin is piped")
                .write_all(s.as_bytes())
                .expect("writing the script must succeed");
            let out = child.wait_with_output().expect("reaping `sh -n` must succeed");
            assert!(
                out.status.success(),
                "sh -n rejected the {label} script: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// It does NOT swap anything. The rename happened before this helper was spawned,
    /// because replacing an AppImage is safe against the running copy; a helper that also
    /// tried to install would be a second, racing writer.
    #[test]
    fn it_only_relaunches_and_never_installs() {
        let s = script(true);
        assert!(!s.contains("mv "), "no move");
        assert!(!s.contains("cp "), "no copy");
        assert!(!s.contains("rm "), "nothing removed");
    }
}

/// DRAGON-532: which manifest entry a Linux build reads. Compiled and run on EVERY host,
/// because the two Linux artifacts are one binary and the rule is not a `cfg`.
#[cfg(test)]
mod linux_platform_key_tests {
    use super::{
        APPIMAGE_AARCH64_PLATFORM_KEY, APPIMAGE_PLATFORM_KEY, LINUX_AARCH64_PLATFORM_KEY,
        LINUX_PLATFORM_KEY, is_appimage_key, linux_platform_key,
    };

    /// The ZIP (BIN) build, and every source build: `platforms.linux`, unchanged since
    /// DRAGON-521.
    #[test]
    fn a_plain_binary_reads_the_linux_entry() {
        assert_eq!(linux_platform_key(false, false), "linux");
    }

    #[test]
    fn an_appimage_reads_its_own_entry() {
        assert_eq!(linux_platform_key(true, false), APPIMAGE_PLATFORM_KEY);
    }

    /// THE POINT OF THE KIND SPLIT. Both artifacts are the same compiled code, so if these
    /// two ever collapsed to one key the manifest entry (which carries the DOWNLOAD URL)
    /// would hand an AppImage the zip's URL, or the reverse, and the installer would replace
    /// the running program with a different artifact kind.
    #[test]
    fn the_two_linux_artifacts_never_share_a_key() {
        assert_ne!(linux_platform_key(true, false), linux_platform_key(false, false));
        assert_ne!(linux_platform_key(true, true), linux_platform_key(false, true));
    }

    /// THE POINT OF THE ARCH SPLIT, and the worse of the two failures (DRAGON-529). A wrong
    /// KIND at least still executes; a wrong ARCH downloads a binary this CPU cannot run and
    /// renames it over the working one, leaving nothing that starts.
    #[test]
    fn the_two_architectures_never_share_a_key() {
        assert_ne!(linux_platform_key(true, false), linux_platform_key(true, true));
        assert_ne!(linux_platform_key(false, false), linux_platform_key(false, true));
    }

    /// All four combinations are distinct. Stated once over the whole matrix, so neither
    /// axis can be added to in future without this failing.
    #[test]
    fn all_four_linux_keys_are_distinct() {
        let keys = [
            linux_platform_key(false, false),
            linux_platform_key(false, true),
            linux_platform_key(true, false),
            linux_platform_key(true, true),
        ];
        let unique: std::collections::BTreeSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len(), "every (kind, arch) pair needs its own key: {keys:?}");
    }

    /// The publisher writes these literals into `update.json`, so a typo on either side
    /// reads as "no update available" rather than as an error. Pin all four.
    ///
    /// The UNSUFFIXED two mean x86_64 and must never gain a suffix: they are what every
    /// already-installed build looks for, and a published manifest is read by versions we
    /// can no longer change. Renaming one stops updates dead for everyone on it.
    #[test]
    fn the_key_strings_are_stable() {
        assert_eq!(LINUX_PLATFORM_KEY, "linux");
        assert_eq!(APPIMAGE_PLATFORM_KEY, "appimage");
        assert_eq!(LINUX_AARCH64_PLATFORM_KEY, "linux-aarch64");
        assert_eq!(APPIMAGE_AARCH64_PLATFORM_KEY, "appimage-aarch64");
    }

    /// One-click install follows the KIND, across every architecture. If this only knew
    /// about x86_64, an aarch64 AppImage would silently fall back to "open the releases
    /// page" despite being perfectly able to replace itself.
    #[test]
    fn every_appimage_key_is_recognised_as_an_appimage() {
        assert!(is_appimage_key(APPIMAGE_PLATFORM_KEY));
        assert!(is_appimage_key(APPIMAGE_AARCH64_PLATFORM_KEY));
        assert!(!is_appimage_key(LINUX_PLATFORM_KEY));
        assert!(!is_appimage_key(LINUX_AARCH64_PLATFORM_KEY));
        // And nothing else is, including the other platforms' keys.
        assert!(!is_appimage_key("macos"));
        assert!(!is_appimage_key("windows"));
    }

    /// A real published manifest carries ALL FOUR keys at once (`publish-release.yml` emits
    /// every artifact attached to the release), so each build must slice out its OWN entry.
    ///
    /// This is the failure the split exists to prevent, and it is not hypothetical: the
    /// entry carries the download URL, so an AppImage that resolved the `linux` entry would
    /// fetch the unpacked zip and rename THAT over itself. The keys share a prefix-free
    /// substring search, and the AppImage's own URL ends in `.AppImage`, one case-fold away
    /// from the key it must not match.
    #[test]
    fn every_platform_slices_its_own_artifact_out_of_one_manifest() {
        const PUBLISHED: &str = r#"
        {
          "version": "0.29.0",
          "notes": "",
          "published": "2026-08-06T00:00:00Z",
          "platforms": {
            "macos":    { "url": "https://e.com/CosmicCaptureKit-0.29.0-aarch64.dmg",       "sha256": "aa", "size": 1 },
            "windows":  { "url": "https://e.com/CosmicCaptureKit-0.29.0-x86_64.msi",        "sha256": "bb", "size": 2 },
            "linux":    { "url": "https://e.com/CosmicCaptureKit-0.29.0-x86_64.zip",        "sha256": "cc", "size": 3 },
            "appimage": { "url": "https://e.com/CosmicCaptureKit-0.29.0-x86_64.AppImage",   "sha256": "dd", "size": 4 }
          }
        }
        "#;
        for (key, want_url, want_sha) in [
            ("macos", "https://e.com/CosmicCaptureKit-0.29.0-aarch64.dmg", "aa"),
            ("windows", "https://e.com/CosmicCaptureKit-0.29.0-x86_64.msi", "bb"),
            ("linux", "https://e.com/CosmicCaptureKit-0.29.0-x86_64.zip", "cc"),
            ("appimage", "https://e.com/CosmicCaptureKit-0.29.0-x86_64.AppImage", "dd"),
        ] {
            let m = super::Manifest::parse_platform(PUBLISHED, key)
                .unwrap_or_else(|| panic!("{key} must parse"));
            let a = m
                .artifact
                .unwrap_or_else(|| panic!("{key} must resolve an artifact"));
            assert_eq!(a.url, want_url, "{key} resolved another platform's download");
            assert_eq!(a.sha256, want_sha, "{key} resolved another platform's checksum");
        }
    }

    /// THE BACKWARD-COMPATIBILITY TEST, and the reason the unsuffixed keys may never be
    /// renamed (DRAGON-529).
    ///
    /// Every already-installed build looks for the literal `linux`, `macos` or `windows`,
    /// and a published manifest is read by versions we can no longer change. Adding the
    /// suffixed siblings must therefore be INVISIBLE to them. The risk is real rather than
    /// theoretical, because [`platform_object`] resolves a key by substring search: if
    /// `"linux"` could match inside `"linux-aarch64"`, an existing x86_64 install would
    /// start downloading the ARM zip, which its CPU cannot run.
    ///
    /// It cannot, because the search includes BOTH quotes, and `"linux-aarch64"` has a
    /// hyphen where the closing quote would be. Pinned here in the worst arrangement: the
    /// suffixed key placed FIRST, so a prefix match would be found before the real one.
    #[test]
    fn adding_the_aarch64_keys_does_not_disturb_the_existing_ones() {
        // Suffixed keys deliberately BEFORE their unsuffixed counterparts.
        const NEW: &str = r#"{"version":"0.30.0","notes":"","published":"","platforms":{
            "linux-aarch64":    {"url":"https://e.com/arm.zip",      "sha256":"aa","size":1},
            "appimage-aarch64": {"url":"https://e.com/arm.AppImage", "sha256":"bb","size":2},
            "linux":            {"url":"https://e.com/x86.zip",      "sha256":"cc","size":3},
            "appimage":         {"url":"https://e.com/x86.AppImage", "sha256":"dd","size":4},
            "macos":            {"url":"https://e.com/a.dmg",        "sha256":"ee","size":5},
            "windows":          {"url":"https://e.com/a.msi",        "sha256":"ff","size":6}}}"#;
        for (key, want) in [
            ("linux", "https://e.com/x86.zip"),
            ("appimage", "https://e.com/x86.AppImage"),
            ("linux-aarch64", "https://e.com/arm.zip"),
            ("appimage-aarch64", "https://e.com/arm.AppImage"),
        ] {
            let a = super::Manifest::parse_platform(NEW, key)
                .unwrap_or_else(|| panic!("{key} must parse"))
                .artifact
                .unwrap_or_else(|| panic!("{key} must resolve"));
            assert_eq!(a.url, want, "{key} resolved the wrong architecture's artifact");
        }
    }

    /// A manifest published BEFORE this key existed (every release up to 0.28.x) must leave
    /// an AppImage with no artifact rather than falling back to the zip. No artifact means
    /// the About page shows "Update Available" without an install button, which is the
    /// honest state; falling back would install the wrong thing.
    #[test]
    fn an_older_manifest_gives_the_appimage_no_artifact() {
        const OLD: &str = r#"{"version":"0.28.0","notes":"","published":"","platforms":{
            "macos":{"url":"https://e.com/a.dmg","sha256":"aa","size":1},
            "linux":{"url":"https://e.com/a.zip","sha256":"cc","size":3}}}"#;
        let m = super::Manifest::parse_platform(OLD, APPIMAGE_PLATFORM_KEY).expect("parse");
        assert_eq!(m.version, "0.28.0", "the version still reads, so the check still works");
        assert!(m.artifact.is_none(), "no appimage entry must not fall back to the zip");
    }
}

#[cfg(test)]
mod channel_availability_tests {
    use super::channel_available_for;
    use crate::util::PackageKind;

    /// THE DRAGON-561 gate: a Flatpak has no in-app update channel. Updates arrive
    /// through the store, and /app is read-only, so a poll could only ever advertise
    /// something this build has no way to install.
    #[test]
    fn a_flatpak_has_no_update_channel() {
        assert!(!channel_available_for(PackageKind::Flatpak));
    }

    /// Every other package kind keeps the channel exactly as it was: the plain binary
    /// and the AppImage both still check, and the AppImage still one-click installs.
    ///
    /// **macOS and Windows are answered here rather than by default** (DRAGON-614). Both had
    /// this channel before they were their own `PackageKind`, because both reported
    /// `Binary`, and both must still have it: the dmg and the MSI install into a layout
    /// their packaging defined, which is exactly what `one_click_install_available` says
    /// they can replace. `channel_available_for` gives them `true` through its `!= Flatpak`
    /// body with nothing added, and this pins that the body keeps doing so.
    #[test]
    fn every_other_package_kind_keeps_the_channel() {
        assert!(channel_available_for(PackageKind::Binary));
        assert!(channel_available_for(PackageKind::AppImage));
        assert!(channel_available_for(PackageKind::MacOs));
        assert!(channel_available_for(PackageKind::Windows));
    }
}

// The DRAGON-605 split: the channel gate answers "who installs", this answers "who
// fetches the text". One gate was answering both, and a Flatpak lost its changelog to a
// rule about installing.
#[cfg(test)]
mod notes_source_tests {
    use super::{NotesSource, channel_available_for, notes_source_for};
    use crate::util::PackageKind;

    /// Every kind this app ships as. Spelled out rather than derived so adding a variant
    /// to `PackageKind` fails the exhaustive match below and forces a decision here.
    const EVERY_KIND: [PackageKind; 5] = [
        PackageKind::Binary,
        PackageKind::AppImage,
        PackageKind::Flatpak,
        PackageKind::MacOs,
        PackageKind::Windows,
    ];

    /// THE bug. A Flatpak has no update channel and must still show what changed; those
    /// are different questions and only the first one is impossible in a sandbox.
    #[test]
    fn a_flatpak_shows_notes_despite_having_no_channel() {
        assert!(!channel_available_for(PackageKind::Flatpak), "the DRAGON-561 rule holds");
        assert_eq!(
            notes_source_for(PackageKind::Flatpak),
            NotesSource::OwnFetch,
            "no check runs, so nothing else would ever fill the notes in"
        );
    }

    /// A build WITH a channel must not fetch twice. Its update check already parsed the
    /// notes out of the same manifest, so a second request would buy nothing.
    ///
    /// DRAGON-614's two new kinds are answered explicitly for the same reason the channel
    /// test answers them: they are channel builds, so they ride the check, exactly as they
    /// did while they both reported `Binary`.
    #[test]
    fn a_channel_build_rides_its_update_check() {
        assert_eq!(notes_source_for(PackageKind::Binary), NotesSource::UpdateCheck);
        assert_eq!(notes_source_for(PackageKind::AppImage), NotesSource::UpdateCheck);
        assert_eq!(notes_source_for(PackageKind::MacOs), NotesSource::UpdateCheck);
        assert_eq!(notes_source_for(PackageKind::Windows), NotesSource::UpdateCheck);
    }

    /// The relationship, stated once: the two predicates are exact complements over every
    /// package kind. A kind that checks for updates never fetches notes separately, and a
    /// kind that does not check always does. Drift either way is a live defect: two
    /// requests for one manifest, or the DRAGON-605 blank changelog.
    #[test]
    fn notes_source_complements_the_channel_gate() {
        for kind in EVERY_KIND {
            let expected = if channel_available_for(kind) {
                NotesSource::UpdateCheck
            } else {
                NotesSource::OwnFetch
            };
            assert_eq!(notes_source_for(kind), expected, "{kind:?} must not drift");
        }
    }

    /// There is no "no notes" answer, for any kind. The enum is total on purpose: this is
    /// the guarantee that turning a channel off can never again take the changelog with
    /// it. Exhaustive, so a new `PackageKind` variant will not compile until it is listed
    /// in `EVERY_KIND` and answered here.
    #[test]
    fn no_package_kind_is_left_without_notes() {
        for kind in EVERY_KIND {
            match notes_source_for(kind) {
                NotesSource::UpdateCheck | NotesSource::OwnFetch => {}
            }
        }
        assert_eq!(EVERY_KIND.len(), 5, "list every PackageKind variant here");
    }
}

// `parse_release_notes` reads the two text fields and nothing else, which is what stops
// the notes path from ever holding something installable (DRAGON-605).
#[cfg(test)]
mod release_notes_parse_tests {
    use super::parse_release_notes;

    /// A real-shaped manifest: the version and the markdown come back intact.
    #[test]
    fn it_reads_the_version_and_the_notes() {
        const JSON: &str = r###"{"version":"0.31.0","notes":"## Fixed\n- a thing\n",
            "published":"2026-08-09","platforms":{
            "linux":{"url":"https://e.com/a.zip","sha256":"cc","size":3}}}"###;
        let n = parse_release_notes(JSON).expect("parse");
        assert_eq!(n.version, "0.31.0");
        assert!(n.notes.contains("- a thing"), "the markdown survives: {}", n.notes);
    }

    /// THE structural guarantee. The parser must not read `platforms`, so no artifact,
    /// URL, sha or size can reach the notes path and turn into a download button on a
    /// build that cannot install one. The type has no field for it; this pins that the
    /// values do not sneak into the two it does have.
    #[test]
    fn nothing_installable_comes_back() {
        const JSON: &str = r#"{"version":"0.31.0","notes":"just text","platforms":{
            "linux":{"url":"https://e.com/a.zip","sha256":"deadbeef","size":42},
            "appimage":{"url":"https://e.com/a.AppImage","sha256":"feedface","size":43}}}"#;
        let n = parse_release_notes(JSON).expect("parse");
        for leaked in ["https://", "deadbeef", "feedface", ".AppImage", ".zip"] {
            assert!(!n.notes.contains(leaked), "{leaked} must not reach the notes");
            assert!(!n.version.contains(leaked), "{leaked} must not reach the version");
        }
    }

    /// No notes means no block, not an empty one. A whitespace-only field counts as none.
    #[test]
    fn an_empty_notes_field_shows_nothing() {
        assert!(parse_release_notes(r#"{"version":"0.31.0","notes":""}"#).is_none());
        assert!(parse_release_notes(r#"{"version":"0.31.0","notes":"  \n "}"#).is_none());
        assert!(parse_release_notes(r#"{"version":"0.31.0"}"#).is_none());
    }

    /// A manifest with no usable version is unusable: the heading reads "What's new in
    /// <version>", so there is nothing honest to render without one.
    #[test]
    fn a_missing_or_blank_version_shows_nothing() {
        assert!(parse_release_notes(r#"{"notes":"text"}"#).is_none());
        assert!(parse_release_notes(r#"{"version":"  ","notes":"text"}"#).is_none());
    }

    /// Garbage degrades quietly, like every other read on this path.
    #[test]
    fn malformed_input_shows_nothing() {
        assert!(parse_release_notes("").is_none());
        assert!(parse_release_notes("<html>404</html>").is_none());
    }

    /// The notes-only read and the full manifest read must agree about the text, so the
    /// Flatpak page and every other platform's page show the same changelog for the same
    /// release. Two parsers over one document is exactly where a drift would hide.
    #[test]
    fn it_agrees_with_the_full_manifest_parse() {
        const JSON: &str = r###"{"version":"0.31.0","notes":"## What\n- one\n- two\n",
            "published":"2026-08-09","platforms":{
            "macos":{"url":"https://e.com/a.dmg","sha256":"aa","size":1}}}"###;
        let full = super::Manifest::parse_platform(JSON, "macos").expect("full parse");
        let notes = parse_release_notes(JSON).expect("notes parse");
        assert_eq!(notes.version, full.version);
        assert_eq!(notes.notes, full.notes);
    }
}
