//! Proton Drive, through Proton's own official CLI (DRAGON-485).
//!
//! # Why this provider is shaped differently from every other one
//!
//! The other four drives are OAuth plus REST, so `cloud/` reaches them with a token and
//! [`super::http`]. Proton has no third-party API at all. What it has, since mid-2026, is an
//! official, MIT-licensed, cross-platform command-line tool, `proton-drive`, built on Proton's
//! own Drive SDK. So this provider is a CHILD PROCESS seam rather than an HTTP one, and it
//! follows the pattern this app already uses for `ffmpeg` and `tesseract`: an optional external
//! tool, found through [`crate::util::proton_drive_path`], probed before use, and explained
//! rather than assumed when it is missing.
//!
//! # "Never bundled" held until DRAGON-566, and only half of it fell
//!
//! This doc used to say the tool is never bundled, and for the DIRECT builds that still holds,
//! for the original reasons: it is a ~118 MB standalone binary of someone else's application,
//! and a copy inside our artifacts would go stale on OUR update cadence rather than Proton's.
//! The FLATPAK bundles it (at `/app/bin`, exactly where [`crate::util::proton_drive_path`]'s
//! exe-adjacent arm looks), because both halves of that reasoning invert inside a sandbox: a
//! host install is INVISIBLE from in there, so "go install it" is advice the package itself
//! makes false, and the store is the update channel in a Flatpak, so the bundled copy is
//! updated the same way the app is. The MIT license is what makes shipping a copy clean.
//! [`missing_tool_note`] and [`install_refusal`] are where the user-facing guidance splits on
//! package kind, and [`install_press_downloads`] is why a bundled build never opens
//! [`DOWNLOAD_URL`].
//!
//! **A native reimplementation was built and then retired.** DRAGON-485 first went the other
//! way: Proton's SRP-6a login and both of its password derivations were implemented and unit
//! tested here (commit `355edf2`, if the route is ever reversed). It worked, and what it
//! established is why it was dropped. Password-SRP login structurally excludes accounts using
//! security keys or SSO; Proton challenges unfamiliar clients with a CAPTCHA that an unattended
//! sign-in cannot answer; and the unofficial protocol had already produced an eight-month upload
//! outage and a crypto migration that left files unreadable. The official tool's browser sign-in
//! sidesteps every one of those, and its crypto is Proton's own, kept current by Proton.
//!
//! # What the tool can actually do, established by reading its source
//!
//! Not guessed. Each of these is why a capability row in [`super::registry`] reads the way it
//! does:
//!
//! * **Sign-in is a browser flow the tool runs itself.** `auth login --json` prints
//!   `{"signInUrl": "..."}` on stdout, opens the browser, and then BLOCKS until the user
//!   finishes. That maps exactly onto the existing Browser step, which already shows a
//!   copyable sign-in URL and a countdown.
//! * **One account, and only one.** Credentials go into the OS secret store under a single
//!   fixed name (`auth-session`, service `ch.proton.drive/drive-sdk-cli`) with no account
//!   parameter anywhere. A second `auth login` REPLACES the first. So this app caps Proton at
//!   one connected account rather than offering a second that would silently evict it.
//! * **Folders browse normally.** `filesystem list <path> --json` lists a level, and the root
//!   for a user's own files is `/my-files`, so the ordinary folder step works unchanged.
//! * **Share links exist**, and the reply is not shaped like one. `sharing set-url <path>
//!   --json` creates or updates a public link and prints the node's WHOLE sharing state:
//!   invitations, members, `editorsCanShare`, and the link itself nested in `urlAccess.url`.
//!   Reading the top level for a `url` key finds nothing, which is exactly what DRAGON-522 was
//!   reported for. `providers::proton::parse_share_url` is where that is now read.
//! * **Delete is RECOVERABLE.** `filesystem trash` moves an item to Proton's trash, which
//!   `filesystem restore` can undo. Proton is the second provider here that can honestly
//!   promise that, and the delete confirmation's copy already keys on the capability.
//! * **There is NO upload progress to parse**, and DRAGON-522 proved it the hard way rather
//!   than assuming it: the tool's progress display needs BOTH a TTY stdout and no `--json`, and
//!   `--json` is what makes every other reply readable. `providers::proton`'s own module doc
//!   carries the captured frames and the full cost of adopting them; do not re-run that
//!   experiment. So an upload reports indeterminate and the meter says so honestly, rather than
//!   this module inventing a percentage.
//!
//! # Exit codes
//!
//! `0` success, `1` an error (both the tidy kind, where the message alone is printed, and the
//! fatal kind, which also dumps a stack trace), `2` an explicit exit, `130` an interrupt.
//! Because `1` covers everything, the exit code alone is never enough to classify a failure,
//! which is what [`classify_failure`] is for.
//!
//! # Privacy
//!
//! `cloud/`'s rules apply unchanged. The tool's stderr can name a path or a remote item, so it
//! is never logged verbatim: [`classify_failure`] reduces it to a category, and a log line
//! carries the category and the exit code. The sign-in URL is a credential-bearing address and
//! is treated exactly as the OAuth one is.

// The staging `#![allow(dead_code)]` that covered this module while it landed ahead of its
// readers is GONE, on exactly the condition its own comment set: the moment Proton was wired
// into `providers::ops`, the picker's install-guidance state and the registry row, it came off.
// Nothing was kept behind a per-item attribute; `Availability::is_ready` was the one item left
// uncalled and it was deleted with the attribute rather than preserved, since the three-state
// is matched directly at both readers. Same rule `cloud/mod.rs` records for its own staging
// allow: anything genuinely dead on ONE platform only would get a targeted `cfg_attr`.

use std::time::Duration;

/// Where a user gets the tool. Shown, and opened, when it is not installed.
///
/// Proton's own support page rather than a direct binary link: the download page names the
/// per-platform builds and their checksums, and a direct link would rot at the next release.
///
/// Never offered by a package that BUNDLES the tool ([`install_press_downloads`]): there a
/// download could only produce a host install the sandbox cannot see.
pub const DOWNLOAD_URL: &str = "https://proton.me/support/drive-cli";

/// Whether this package BUNDLES the tool (DRAGON-566). Pure; unit-tested.
///
/// The Flatpak ships `proton-drive` at `/app/bin`, beside our own binary, which is exactly
/// where [`crate::util::proton_drive_path`]'s exe-adjacent arm looks. It is the only package
/// that does; the module doc carries the whole of why. The package kind is a legitimate axis
/// here because what changes with it is the ADVICE, not the capture behaviour.
pub fn bundled_in(kind: crate::util::PackageKind) -> bool {
    kind == crate::util::PackageKind::Flatpak
}

/// The install row's caption when the tool cannot be run, keyed on how this app is shipped.
/// Pure; unit-tested.
///
/// A Flatpak user told "put `proton-drive` in `PATH`" would install it on the HOST, where the
/// sandbox can never see it, chasing a fix that cannot work. The honest sentence there is that
/// this build includes the tool and its absence is our packaging fault, with reinstalling the
/// app as the one action that can restore it. Every other package keeps the historical wording
/// unchanged.
pub fn missing_tool_note(kind: crate::util::PackageKind, tool_name: &str) -> String {
    if bundled_in(kind) {
        format!("This build includes {tool_name}, but it is missing. Reinstall the app.")
    } else {
        format!("Requires {tool_name} CLI in PATH")
    }
}

/// The Connect refusal when the tool cannot be run, keyed the same way. Pure; unit-tested.
///
/// Same split as [`missing_tool_note`], in the longer form the connect dialog speaks:
/// a bundled build names a packaging fault instead of asking the user to install anything.
pub fn install_refusal(kind: crate::util::PackageKind, display_name: &str, tool_name: &str) -> String {
    if bundled_in(kind) {
        format!(
            "{display_name} connects through Proton's own {tool_name} command-line tool. This \
             build includes that tool, so its absence is a packaging fault, not something to \
             install. Reinstalling this app should restore it."
        )
    } else {
        format!(
            "{display_name} connects through Proton's own {tool_name} command-line tool, which is not \
             installed on this computer yet. Install it, then try again."
        )
    }
}

/// Whether the install row's press may open [`DOWNLOAD_URL`]. Pure; unit-tested.
///
/// In a package that bundles the tool, the download page IS the host-install advice this
/// module must never give, so the row's caption explains and the press goes nowhere, the same
/// shape as the already-connected face.
pub fn install_press_downloads(kind: crate::util::PackageKind) -> bool {
    !bundled_in(kind)
}

/// The tool's name, spelled as the command a user types and as `PATH` must carry it.
///
/// One constant so the install guidance, the log lines and [`crate::util::proton_drive_path`]
/// cannot disagree about what is being looked for.
pub const TOOL_NAME: &str = "proton-drive";

/// The top-level section that holds a user's photo TIMELINE: every photo, in one flat stream.
///
/// It is where an uploaded photo lands and how one is addressed afterwards
/// (`/photos/<name-or-uid>`), and it is NOT where albums live; see [`ALBUMS`], which is a
/// separate root and the one an album destination is built from. Getting those two the wrong way
/// round is a silent failure rather than a loud one, because a path under this root resolves by
/// walking the whole timeline for a photo of that name and simply reports "not found".
///
/// **`filesystem list` cannot list this root, nor [`ALBUMS`].** The tool's listing command
/// switches on the path's section and has no arm for either of them, so it refuses outright;
/// `album list` and `photo timeline` are the commands that read them.
pub const PHOTOS: &str = "photos";

/// The top-level section that holds a user's photo ALBUMS.
///
/// # Albums are FLAT, and that drives a second selection model (owner redesign)
///
/// The Photos destination is an ALBUM, not a folder, and albums do not nest: `album list` takes
/// no path argument at all. So the Photos tab reuses the folder browser's chassis (same rows,
/// same padding, same create row, same refresh spin) with one semantic difference, and the
/// difference is deliberate rather than an oversight:
///
/// * **Files tab: the folder you are VIEWING is the destination** (DRAGON-517). There is no
///   check and no selection state, because navigating INTO a folder is what chooses it.
/// * **Photos tab: the album you CLICK is the destination.** A click selects rather than
///   navigates, the row takes a check, and that check is real state the dialog holds. There is
///   no "viewing" a flat album, so the Files model has nothing to express here.
///
/// **Do not unify these two into one model.** They look alike on screen on purpose and they are
/// not the same thing underneath; collapsing them would either reintroduce navigation into
/// something with nowhere to navigate, or put a redundant check on the Files tab that DRAGON-517
/// deliberately removed. One chassis, two selection strategies, and this paragraph is why.
///
/// The default album is [`super::APP_FOLDER`], found or created on first use, exactly as the app
/// folder is on the Files side. `album delete` exists, so album rows carry the same per-row
/// trash the folder rows do.
///
/// **An album is addressed by its UID, never by its name.** A name lookup walks every album and
/// two albums may share a name, so [`album_path`] builds `/albums/<uid>` from the `uid` the
/// listing reports. `album create` hands the new album's uid straight back, which is what makes
/// that possible for one this app has just made.
pub const ALBUMS: &str = "albums";

/// The top-level section of a Proton Drive that holds a user's own files.
///
/// The tool addresses everything by path, and `/my-files` is the root of the ordinary file
/// tree; the siblings (`/photos`, `/trash`, `/shared-by-me`, `/devices`) are other sections
/// this app does not write to. Named here because it is the first segment of every path this
/// app builds.
pub const MY_FILES: &str = "my-files";

/// How long the availability probe may take.
///
/// A `version` call on a 118 MB self-contained binary is mostly process start-up, so this is
/// generous rather than tight. It still has to be BOUNDED: the probe runs while the settings
/// page is drawing its provider list, and a tool wedged on a locked keyring must not hold that.
pub const PROBE_BUDGET: Duration = Duration::from_secs(10);

/// Whether this machine can use the Proton provider, and if not, why not.
///
/// Three states rather than a `bool` because the middle one is real and the user-facing answer
/// differs: a tool that is installed but cannot run (a partial download, a missing `libsecret`
/// on Linux, an architecture mismatch, a file without its executable bit) is not the same
/// situation as one that was never installed, even though neither can connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// The tool is there and answered the probe. The provider connects normally.
    Ready,
    /// The tool is there and did NOT answer. Treated as unusable, and the row offers the same
    /// install guidance, because "reinstall it" is the action either way.
    Broken,
    /// Nothing was found on the override, beside our executable, or on `PATH`.
    Missing,
}

/// Decide the three-state from what a probe observed. Pure; unit-tested.
///
/// The house split: this is the DECISION, and running the process is the effect. `found` is
/// whether a binary was located at all, `exit_status` the probe's exit code (`None` when it
/// could not be spawned or outlived [`PROBE_BUDGET`]).
///
/// **A non-zero exit is `Broken`, not `Missing`.** The binary exists, so telling the user it is
/// not installed would send them to re-download something they already have; the distinction is
/// kept even though both rows currently offer the same action, because the LOG line differs and
/// that is what a support conversation runs on.
pub fn classify_probe(found: bool, exit_status: Option<i32>) -> Availability {
    if !found {
        return Availability::Missing;
    }
    match exit_status {
        Some(0) => Availability::Ready,
        _ => Availability::Broken,
    }
}

/// Find the tool and ask it its version, and say whether the provider can be used.
///
/// The EFFECT half of the house split: [`classify_probe`] holds the decision and is tested on
/// every platform, and this collects the two facts it needs. Nothing here decides anything.
///
/// Bounded by [`PROBE_BUDGET`], because this runs while the settings page draws its provider
/// list. A tool that cannot be spawned at all is reported as absent rather than broken, since
/// that is what a bare name failing to resolve on `PATH` means.
pub fn probe() -> Availability {
    let path = crate::util::proton_drive_path();
    let mut command = crate::util::quiet_command(&path);
    command
        .arg("version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let Ok(mut child) = command.spawn() else {
        // Nothing to run: either the override names a file that is not there, or the bare name
        // did not resolve on PATH.
        log::debug!("cloud accounts: the proton-drive tool could not be started");
        return Availability::Missing;
    };
    let deadline = std::time::Instant::now() + PROBE_BUDGET;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let availability = classify_probe(true, status.code());
                log::debug!("cloud accounts: proton-drive probe says {availability:?}");
                return availability;
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(PROBE_POLL);
            }
            Ok(None) => {
                // Wedged past its budget. Kill it rather than leaving a child behind, and
                // report the tool as unusable rather than waiting any longer.
                let _ = child.kill();
                let _ = child.wait();
                log::debug!("cloud accounts: the proton-drive probe outlived its budget");
                return Availability::Broken;
            }
            Err(_) => return Availability::Broken,
        }
    }
}

/// How often [`probe`] wakes to see whether the tool has answered.
const PROBE_POLL: Duration = Duration::from_millis(20);

/// What went wrong with a `proton-drive` invocation, in the terms this app acts on.
///
/// Deliberately NOT a new failure vocabulary: `diag::Failure` is the app's only one, and this
/// is the same shape `super::providers` already uses for its own per-provider mapping. It
/// exists so the reconnect decision is made once, from the tool's own output, rather than by
/// three call sites each pattern-matching stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The session is gone: never signed in, signed out elsewhere, revoked, or expired past
    /// recovery. The account must be connected again, and the message carries
    /// [`super::oauth::RECONNECT_PREFIX`] so the settings row lights up its Reconnect.
    NeedsReconnect,
    /// Something that another attempt could plausibly fix: the network, a server error, a
    /// locked keyring the user is about to unlock.
    Transient,
    /// Everything else. Reported, not retried.
    Other,
}

/// The sentence a signed-out tool prints, and the one signal that means "connect it again".
///
/// The tool raises this before it makes any request at all, when it finds no stored session, so
/// it is the cheap and unambiguous case. A session that exists but is refused by the server
/// surfaces through the account API instead, which is why [`classify_failure`] also looks for
/// the vocabulary below rather than only for this.
const SIGNED_OUT_MARKER: &str = "you need to login first";

/// Words that mean the SERVER rejected the session rather than the tool noticing it was absent.
///
/// Matched case-insensitively against stderr. Kept as a small list of narrow phrases rather
/// than a single word like "auth", which would swallow ordinary authorisation errors about a
/// file and turn a one-off refusal into a spurious "connect it again".
const REVOKED_MARKERS: &[&str] = &[
    "invalid refresh token",
    "session expired",
    "session is invalid",
    "unauthorized",
    "401",
];

/// Words that mean trying again could work.
const TRANSIENT_MARKERS: &[&str] = &[
    "network",
    "timed out",
    "timeout",
    "connection",
    "temporarily",
    "try again",
    "503",
    "502",
    "429",
];

/// Classify a failed invocation from its exit code and stderr. Pure; unit-tested.
///
/// **Why stderr and not the exit code.** The tool exits `1` for every error it handles, tidy or
/// fatal alike, so the code says only "it failed". The text is the only thing that separates
/// "you are signed out" from "the network is down", and getting that separation right is what
/// decides whether the user is shown a Reconnect button or a Try again button.
///
/// An interrupt (`130`) is [`Failure::Transient`]: it means something killed the child, which
/// on our side is a cancel, and a cancel is never a reason to tell someone their account needs
/// reconnecting.
pub fn classify_failure(exit_status: Option<i32>, stderr: &str) -> Failure {
    let text = stderr.to_lowercase();
    if text.contains(SIGNED_OUT_MARKER) {
        return Failure::NeedsReconnect;
    }
    if REVOKED_MARKERS.iter().any(|m| text.contains(m)) {
        return Failure::NeedsReconnect;
    }
    if TRANSIENT_MARKERS.iter().any(|m| text.contains(m)) {
        return Failure::Transient;
    }
    match exit_status {
        // The child was killed, which on our side is a cancel.
        Some(130) | None => Failure::Transient,
        _ => Failure::Other,
    }
}

/// Turn a classified failure into the sentence a user reads. Pure; unit-tested.
///
/// `operation` names what was being attempted, in the user's terms ("upload this capture"), so
/// one table produces copy for every call site without any of them writing their own.
///
/// The reconnect case is the only one that carries a machine-read prefix, and it carries the
/// SAME one the OAuth providers use, so the settings page's existing
/// [`super::oauth::needs_reconnect`] lights up the existing Reconnect button with no Proton
/// branch anywhere in the UI.
pub fn failure_message(failure: Failure, operation: &str) -> String {
    match failure {
        Failure::NeedsReconnect => super::oauth::reconnect_message(&format!(
            "Your Proton Drive sign-in has ended, so this app could not {operation}. \
             Connect the account again to continue."
        )),
        Failure::Transient => {
            format!("Proton Drive could not {operation} just now. Please try again.")
        }
        Failure::Other => format!("Proton Drive could not {operation}."),
    }
}

/// Build a Proton Drive path from its segments, as the tool addresses one. Pure; unit-tested.
///
/// Always rooted at [`MY_FILES`], because that is the only section this app writes to, and
/// always absolute. Empty and whitespace-only segments are dropped, which is what makes a
/// stored destination of `""` (a hand-edited accounts file, a trailing slash) address the app
/// folder rather than a nameless child of it.
///
/// The tool takes paths as ordinary arguments, so nothing here needs shell quoting; a segment
/// containing a space is passed through as one argv entry by
/// [`std::process::Command`] and arrives intact.
pub fn drive_path(segments: &[&str]) -> String {
    let mut path = format!("/{MY_FILES}");
    for segment in segments {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        path.push('/');
        path.push_str(trimmed);
    }
    path
}

/// How long the sign-in waits for the tool to PRINT its sign-in address.
///
/// Short, because this is not the wait for the user: the tool builds the URL and prints it
/// before it blocks, so anything past a few seconds means the tool is not going to answer at
/// all. Bounded separately from the sign-in itself so a tool that never prints a URL fails
/// quickly instead of leaving a dialog waiting out the whole browser deadline with nothing on
/// screen to click.
const SIGN_IN_URL_BUDGET: Duration = Duration::from_secs(20);

const _: () = assert!(
    SIGN_IN_URL_BUDGET.as_secs() < super::oauth::BROWSER_DEADLINE.as_secs(),
    "DRAGON-485: waiting for the tool to PRINT its sign-in address must be the inner bound; a \
     budget at or past the browser deadline would make the URL wait indistinguishable from the \
     user's own wait, and the dialog would sit with nothing to click"
);

/// Pull the sign-in address out of one line of `auth login --json`. Pure; unit-tested.
///
/// The tool prints exactly `{"signInUrl": "..."}` and then BLOCKS until the browser flow
/// finishes, so this runs per line as output arrives rather than over a finished stdout.
///
/// **Only an https URL is accepted**, and that is not cosmetic: this string becomes a clickable
/// link and a clipboard entry, so anything else (a shell-handler scheme, a `file://`, a
/// diagnostic line that happens to be JSON) is refused exactly as
/// `providers::proton::parse_share_url` refuses one.
pub fn parse_sign_in_url(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let url = value.get("signInUrl")?.as_str()?;
    url.starts_with("https://").then(|| url.to_string())
}

/// Sign in, by letting the tool run its own browser flow (DRAGON-485).
///
/// This is the external-tool answer to `oauth::connect_interactive_with`, and it hands back
/// nothing on success because there is nothing to hand back: the tool owns the session, in the
/// OS secret store, under its own identity. `cloud::secrets` stores no Proton token, which is
/// also why `providers::access_token` gives this provider's ops an empty one.
///
/// `on_url` receives the sign-in address the moment the tool prints it, so the dialog's Browser
/// step shows the same clickable, copyable link every other provider's does. **The tool ALSO
/// opens the browser itself**, which this app cannot switch off; the link is what makes the step
/// recoverable when that silently fails (no default browser, the wrong browser, a remote
/// session), which is the same reason this app stopped opening one automatically.
///
/// Two nested bounds, per the house rule that nothing waits unboundedly:
/// [`SIGN_IN_URL_BUDGET`] for the tool to print the address, and `browser_deadline` for the
/// whole flow. A child that outlives the outer one is KILLED rather than left holding a
/// half-finished sign-in.
pub fn sign_in(
    browser_deadline: Duration,
    on_url: &mut dyn FnMut(&str),
) -> Result<(), String> {
    use std::io::{BufRead, BufReader, Read};

    const OPERATION: &str = "sign in to Proton Drive";
    let mut command = crate::util::quiet_command(crate::util::proton_drive_path());
    command
        .args(["auth", "login", "--json"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().map_err(|_| {
        format!("This app could not run the {TOOL_NAME} tool, so it could not {OPERATION}.")
    })?;
    // **Both pipes are drained by their own threads**, and neither is optional. The child keeps
    // running for as long as the user takes to sign in, so a pipe nobody reads fills and blocks
    // it; and the thread reading stdout is also what lets the URL be reported mid-flight, which
    // `wait_with_output` (which only answers once the child is over) could never do.
    let (url_tx, url_rx) = std::sync::mpsc::channel::<String>();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut sent = false;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if !sent && let Some(url) = parse_sign_in_url(&line) {
                    sent = url_tx.send(url).is_ok();
                }
            }
        });
    }
    // The tool's stderr can name a path or a remote item, so it is never logged verbatim; it is
    // collected only to be reduced to a category by [`classify_failure`].
    let stderr_slot = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(mut stderr) = child.stderr.take() {
        let slot = std::sync::Arc::clone(&stderr_slot);
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            if let Ok(mut g) = slot.lock() {
                *g = text;
            }
        });
    }
    match url_rx.recv_timeout(SIGN_IN_URL_BUDGET) {
        Ok(url) => {
            // The address is credential-bearing, exactly as an OAuth authorize URL is, so it
            // reaches a log only through the same redaction.
            log::debug!(
                "cloud accounts: the proton-drive sign-in page is ready ({})",
                crate::diag::redact_oauth(&url)
            );
            on_url(&url);
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            log::debug!("cloud accounts: the proton-drive sign-in never reported an address");
            return Err(format!(
                "The {TOOL_NAME} tool did not open a Proton sign-in page. Check that it is \
                 installed correctly, then try again."
            ));
        }
    }
    let deadline = std::time::Instant::now() + browser_deadline;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(PROBE_POLL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "The Proton Drive sign-in was not finished in time, so this app could not \
                     {OPERATION}. Please try again."
                ));
            }
            Err(_) => return Err(failure_message(Failure::Other, OPERATION)),
        }
    };
    if status.success() {
        log::debug!("cloud accounts: the proton-drive sign-in completed");
        return Ok(());
    }
    let stderr = stderr_slot.lock().map(|g| g.clone()).unwrap_or_default();
    let failure = classify_failure(status.code(), &stderr);
    log::debug!(
        "cloud accounts: the proton-drive sign-in failed, exit {:?}, classified {failure:?}",
        status.code()
    );
    // **NOT the reconnect wording, whatever the classifier said.** This IS the connect; telling
    // a user in the middle of signing in that they need to sign in again is a sentence with
    // nowhere to go.
    Err(match failure {
        Failure::Transient => {
            format!("Proton Drive could not {OPERATION} just now. Please try again.")
        }
        _ => format!(
            "The Proton sign-in did not complete, so this app could not {OPERATION}."
        ),
    })
}

/// The path that addresses one ALBUM, as the tool addresses one. Pure; unit-tested.
///
/// `uid` is the album's own identifier from `album list` / `album create`, NOT its name: see
/// [`ALBUMS`] for why a name is the wrong key here. A blank uid answers the bare section root,
/// which the tool refuses rather than acting on, so a hand-edited destination cannot silently
/// address every album at once.
pub fn album_path(uid: &str) -> String {
    let trimmed = uid.trim();
    if trimmed.is_empty() {
        return format!("/{ALBUMS}");
    }
    format!("/{ALBUMS}/{trimmed}")
}

/// The path that addresses one photo in the TIMELINE by its file name. Pure; unit-tested.
///
/// The tool resolves a name under [`PHOTOS`] itself, which is what lets an upload be filed into
/// an album without this app ever learning the photo's uid: `photo upload` reports a transfer
/// SUMMARY and no identifier at all, so the name it was given is the only handle there is.
pub fn photo_path(name: &str) -> String {
    format!("/{PHOTOS}/{}", name.trim())
}

#[cfg(test)]
mod path_shape_tests {
    use super::*;

    /// Albums are addressed by uid under their OWN root, never under the timeline's: a path
    /// built under `/photos` resolves by scanning the timeline for a photo and simply reports
    /// nothing found, which is a silent wrong answer rather than a loud one.
    #[test]
    fn an_album_is_addressed_by_uid_under_the_albums_root() {
        assert_eq!(album_path("vol~node"), "/albums/vol~node");
        assert!(!album_path("vol~node").starts_with("/photos"), "the two roots are not the same");
    }

    #[test]
    fn a_blank_album_uid_addresses_no_album() {
        assert_eq!(album_path(""), "/albums");
        assert_eq!(album_path("   "), "/albums");
    }

    #[test]
    fn a_photo_is_addressed_by_name_under_the_timeline_root() {
        assert_eq!(photo_path("Screenshot 2026-08-04.png"), "/photos/Screenshot 2026-08-04.png");
        assert_eq!(photo_path("  shot.png "), "/photos/shot.png");
    }
}

#[cfg(test)]
mod install_guidance_tests {
    use super::*;
    use crate::util::PackageKind;

    /// The whole point of the split (DRAGON-566): a Flatpak never sends its user to install
    /// the CLI on the host, because the sandbox could not see that install anyway. It says the
    /// build includes the tool and a missing one is a packaging fault.
    #[test]
    fn a_flatpak_blames_the_package_not_the_user() {
        let note = missing_tool_note(PackageKind::Flatpak, TOOL_NAME);
        assert!(note.contains("includes"), "{note}");
        assert!(note.contains("Reinstall"), "{note}");
        assert!(!note.contains("PATH"), "a PATH hint is host-install advice: {note}");
        let refusal = install_refusal(PackageKind::Flatpak, "Proton Drive", TOOL_NAME);
        assert!(refusal.contains("packaging fault"), "{refusal}");
        assert!(!refusal.contains("Install it"), "{refusal}");
    }

    /// The direct builds keep the historical wording BYTE-IDENTICAL: the guidance change is
    /// package-kind-scoped, and everywhere else nothing moved. macOS and Windows are in the
    /// list since DRAGON-614 gave them their own kinds, and the point is that the wording they
    /// get is the same one they got as `Binary`.
    #[test]
    fn the_direct_builds_keep_the_historical_wording() {
        for kind in [
            PackageKind::Binary,
            PackageKind::AppImage,
            PackageKind::MacOs,
            PackageKind::Windows,
        ] {
            assert_eq!(
                missing_tool_note(kind, TOOL_NAME),
                "Requires proton-drive CLI in PATH",
                "{kind:?}"
            );
            assert_eq!(
                install_refusal(kind, "Proton Drive", TOOL_NAME),
                "Proton Drive connects through Proton's own proton-drive command-line tool, \
                 which is not installed on this computer yet. Install it, then try again.",
                "{kind:?}"
            );
        }
    }

    /// The download page opens only where downloading is the fix. A bundled build's install
    /// row explains instead, so its press must go nowhere.
    #[test]
    fn only_an_unbundled_package_offers_the_download_page() {
        assert!(!install_press_downloads(PackageKind::Flatpak));
        assert!(install_press_downloads(PackageKind::Binary));
        assert!(install_press_downloads(PackageKind::AppImage));
        // DRAGON-614: the two new kinds bundle nothing, so both keep the host-install advice
        // they already had as `Binary`. Answered here rather than left to the `== Flatpak`
        // body, because "which packages ship the tool" is a packaging fact and a new package
        // is exactly when it needs re-asking.
        assert!(install_press_downloads(PackageKind::MacOs));
        assert!(install_press_downloads(PackageKind::Windows));
    }

    /// The three deciders answer from ONE fact, so they can never disagree about which
    /// package bundles the tool.
    #[test]
    fn the_guidance_split_and_the_press_gate_agree() {
        for kind in [
            PackageKind::Binary,
            PackageKind::AppImage,
            PackageKind::Flatpak,
            PackageKind::MacOs,
            PackageKind::Windows,
        ] {
            assert_eq!(
                install_press_downloads(kind),
                !bundled_in(kind),
                "{kind:?}: the press offers a download exactly where nothing is bundled"
            );
            assert_eq!(
                missing_tool_note(kind, TOOL_NAME).contains("PATH"),
                !bundled_in(kind),
                "{kind:?}: PATH advice belongs only to the unbundled packages"
            );
        }
    }
}

#[cfg(test)]
mod availability_tests {
    use super::*;

    #[test]
    fn nothing_found_is_missing() {
        assert_eq!(classify_probe(false, None), Availability::Missing);
        // Even a nonsense status cannot make an absent tool present.
        assert_eq!(classify_probe(false, Some(0)), Availability::Missing);
    }

    #[test]
    fn found_and_answering_is_ready() {
        assert_eq!(classify_probe(true, Some(0)), Availability::Ready);
    }

    /// Present but unusable is its own state, and it is NOT reported as missing.
    #[test]
    fn found_but_failing_is_broken() {
        for status in [Some(1), Some(2), Some(127), None] {
            assert_eq!(classify_probe(true, status), Availability::Broken, "{status:?}");
        }
    }

    /// Only ONE of the three permits a connect, and the picker reads that by matching this
    /// three-state directly (`pages::cloud::tool_entry`). An `is_ready()` predicate stood here
    /// and was deleted with the module's staging allow: its readers have to answer all three,
    /// because the two refusals offer different actions.
    #[test]
    fn only_ready_permits_a_connect() {
        for refusal in [Availability::Broken, Availability::Missing] {
            assert_ne!(refusal, Availability::Ready, "{refusal:?} is not a connectable state");
        }
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;

    #[test]
    fn the_signed_out_sentence_asks_for_a_reconnect() {
        assert_eq!(
            classify_failure(Some(1), "You need to login first"),
            Failure::NeedsReconnect
        );
    }

    /// The tool's casing is not a contract, so the match must not depend on it.
    #[test]
    fn the_signed_out_sentence_is_matched_case_insensitively() {
        for text in ["you need to login first", "YOU NEED TO LOGIN FIRST"] {
            assert_eq!(classify_failure(Some(1), text), Failure::NeedsReconnect, "{text}");
        }
    }

    /// A session the SERVER refused is the other way this ends, and it must reach the same
    /// answer: the stored session is worthless either way.
    #[test]
    fn a_refused_session_asks_for_a_reconnect() {
        for text in [
            "AccountApiError: invalid refresh token",
            "request failed: 401 Unauthorized",
            "the session expired",
        ] {
            assert_eq!(classify_failure(Some(1), text), Failure::NeedsReconnect, "{text}");
        }
    }

    #[test]
    fn a_network_failure_is_transient() {
        for text in [
            "network request failed",
            "connection refused",
            "the request timed out",
            "server responded 503",
        ] {
            assert_eq!(classify_failure(Some(1), text), Failure::Transient, "{text}");
        }
    }

    /// A cancel kills the child, and a cancel must never be read as "your account is broken".
    #[test]
    fn an_interrupt_is_transient_not_a_reconnect() {
        assert_eq!(classify_failure(Some(130), ""), Failure::Transient);
        assert_eq!(classify_failure(None, ""), Failure::Transient);
    }

    #[test]
    fn an_unrecognised_failure_is_reported_not_retried() {
        assert_eq!(classify_failure(Some(1), "something specific went wrong"), Failure::Other);
        assert_eq!(classify_failure(Some(2), ""), Failure::Other);
    }

    /// The word "unauthorized" about a FILE must not be mistaken for a dead session, which is
    /// why the transient and revoked lists are narrow phrases rather than single words. This
    /// pins the ordering that makes the distinction hold.
    #[test]
    fn a_reconnect_signal_beats_a_transient_one_when_both_appear() {
        assert_eq!(
            classify_failure(Some(1), "network error: 401 Unauthorized"),
            Failure::NeedsReconnect,
            "a dead session is the root cause and must win"
        );
    }
}

#[cfg(test)]
mod message_tests {
    use super::*;

    /// The whole point of the reconnect arm: the SETTINGS page's existing detector has to
    /// recognise it, with no Proton-specific branch anywhere.
    #[test]
    fn the_reconnect_message_is_the_shared_one() {
        let message = failure_message(Failure::NeedsReconnect, "upload this capture");
        assert!(
            super::super::oauth::needs_reconnect(&message),
            "the settings page must see this as a reconnect: {message}"
        );
        assert!(message.contains("Proton Drive"), "{message}");
    }

    /// A failure retrying cannot fix must never say "try again", and one that retrying CAN fix
    /// should.
    #[test]
    fn only_the_retryable_failure_invites_another_attempt() {
        let transient = failure_message(Failure::Transient, "upload this capture");
        assert!(transient.contains("try again"), "{transient}");
        for other in [
            failure_message(Failure::NeedsReconnect, "upload this capture"),
            failure_message(Failure::Other, "upload this capture"),
        ] {
            assert!(!other.contains("try again"), "{other}");
        }
    }

    #[test]
    fn every_message_names_the_operation() {
        for failure in [Failure::NeedsReconnect, Failure::Transient, Failure::Other] {
            let message = failure_message(failure, "make a share link");
            assert!(message.contains("make a share link"), "{failure:?}: {message}");
        }
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn no_segments_is_the_files_root() {
        assert_eq!(drive_path(&[]), "/my-files");
    }

    #[test]
    fn segments_are_joined_below_the_root() {
        assert_eq!(
            drive_path(&["Cosmic Capture Kit", "2026"]),
            "/my-files/Cosmic Capture Kit/2026"
        );
    }

    /// A blank stored destination addresses the app folder, never a nameless child.
    #[test]
    fn blank_segments_are_dropped() {
        assert_eq!(drive_path(&["", "  ", "Shots"]), "/my-files/Shots");
        assert_eq!(drive_path(&["", ""]), "/my-files");
    }

    #[test]
    fn segments_are_trimmed() {
        assert_eq!(drive_path(&[" Shots "]), "/my-files/Shots");
    }
}

#[cfg(test)]
mod sign_in_tests {
    use super::*;

    /// The one line the tool prints before it blocks. Read per line rather than over a finished
    /// stdout, because the tool does not finish until the user has signed in.
    #[test]
    fn the_sign_in_address_is_read_from_the_tools_own_line() {
        let line = r#"{"signInUrl":"https://account.proton.me/authorize?x=1"}"#;
        assert_eq!(
            parse_sign_in_url(line).as_deref(),
            Some("https://account.proton.me/authorize?x=1")
        );
        // Whitespace and a trailing newline are what a line reader actually hands over.
        assert!(parse_sign_in_url(&format!("  {line}  \n")).is_some());
    }

    /// **This string becomes a clickable link and a clipboard entry**, so anything that is not
    /// an https URL is refused rather than passed along, exactly as a share link is.
    #[test]
    fn a_value_that_is_not_an_https_url_is_refused() {
        for line in [
            r#"{"signInUrl":"javascript:alert(1)"}"#,
            r#"{"signInUrl":"file:///etc/passwd"}"#,
            r#"{"signInUrl":"http://account.proton.me/authorize"}"#,
            r#"{"signInUrl":""}"#,
            r#"{"other":"https://account.proton.me/authorize"}"#,
            "{}",
            "Signing you in...",
            "",
        ] {
            assert_eq!(parse_sign_in_url(line), None, "{line}");
        }
    }

    /// The inner bound is what makes a tool that never prints an address fail QUICKLY, instead
    /// of leaving the dialog waiting out the whole browser deadline with nothing to click. The
    /// compile-time assert beside the constant pins the relation; this pins that both bounds are
    /// real.
    #[test]
    fn waiting_for_the_address_is_bounded_inside_the_browser_deadline() {
        assert!(SIGN_IN_URL_BUDGET > Duration::ZERO);
        assert!(SIGN_IN_URL_BUDGET < super::super::oauth::BROWSER_DEADLINE);
    }
}
