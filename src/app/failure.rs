//! Telling the USER a capture failed (DRAGON-415, widened by DRAGON-418).
//!
//! ## Why this module exists
//!
//! Every non-delivery exit in the app was a `log::warn!` followed by
//! [`App::finish_session`]. On macOS that warning reached nothing at all, so the user's
//! whole experience of a failed capture was: the selector vanishes, and then nothing. No
//! file, no preview, no message. That silence is also what makes people press the hotkey
//! again, which is what piles up processes (DRAGON-413).
//!
//! DRAGON-419 fixed the half of that problem that faces US: those paths now record a
//! [`crate::diag::Failure`] and write an opt-in debug log we can ask a customer for. This
//! module is the other half, the one that faces the USER, at the time, without them having
//! to turn anything on or find a file. The two are complements and they share ONE
//! classification: a failure site calls `diag::note_failure(...)` exactly as it already
//! does, then [`App::fail_session`] instead of `finish_session`, and the alert is built
//! from the very record the log wrote. There is deliberately no second taxonomy to drift.
//!
//! ## What is pure and what is not
//!
//! [`alert_message`] is a PURE function of (which failure, its detail, what the Screen
//! Recording preflight says, which macOS this is, which folder we tried). It is unit-tested
//! on every platform — the message table is the part that can be gotten wrong in a way no
//! compiler catches. Presentation is native and per-platform, decided on the ticket: the
//! child that fails often has NO window yet (which is exactly why it can die invisibly), so
//! an iced window would mean standing up the renderer before the scene grab to serve a
//! failure case. macOS shows an AppKit `NSAlert` (`platform::mac::alert`); Windows shows a
//! `MessageBox` (`platform::windows::alert`, DRAGON-436 — same one-per-process latch and
//! best-effort one-per-machine marker). Only LINUX presents nothing: it is the one platform
//! where the `log::warn!` genuinely reaches somebody (a terminal), so [`App::fail_session`]
//! is byte-identically `finish_session` there.
//!
//! The two native presenters agree in the ONE way that matters to every caller: NEITHER
//! may be shown by blocking `App::update`'s own thread. Windows' `MessageBoxW` does not
//! pump our run loop, so blocking would park the thread that owns every HWND we have.
//! mac's `NSAlert::runModal` DOES pump the run loop — but pumping it NESTED inside the
//! winit-dispatched callback that is still on the stack panics winit's own re-entrancy
//! guard (a `RefCell` in `Handler::handle`); an earlier revision assumed this was safe and
//! it crashed every single time, silently, in place of the alert it was supposed to show.
//! Both presenters therefore go up on a thread of their own and hand back a `Dismissal`;
//! see [`App::fail_session`] and `platform::mac::alert`'s module doc for the mac history.
//!
//! ## The honesty rule
//!
//! Never blame permissions we have not checked. That is OBS's mistake on the Sonoma
//! content-fetch stall (`mac-sck-common.m` reports a missing screen-recording grant for a
//! failure that has nothing to do with grants), and it sends people to toggle settings that
//! were never the cause. [`ScreenPermission::Unchecked`] therefore never produces a message
//! that mentions Screen Recording at all, and only [`ScreenPermission::Missing`] — an
//! actual live `CGPreflightScreenCaptureAccess` answer — produces the "permission is
//! missing" message. The tests pin both.

use super::*;
use crate::diag::Failure;

/// The Screen Recording grant as OBSERVED at failure time — not as assumed, and not as it
/// was at launch. `Unchecked` is a first-class answer: it is what every non-macOS platform
/// reports, and it is what forbids the permission message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenPermission {
    /// `CGPreflightScreenCaptureAccess` answered yes.
    Granted,
    /// `CGPreflightScreenCaptureAccess` answered no — the grant is not usable.
    Missing,
    /// We did not ask (there is nothing to ask on this platform). Constructed only OFF
    /// macOS — and in the tests, which is where it earns its keep: it is the state the
    /// honesty rule is written against, so the table has to be exercised in it.
    #[cfg_attr(all(target_os = "macos", not(test)), allow(dead_code))]
    Unchecked,
}

/// A message ready for a native alert: a bold headline and the explanatory body beneath it
/// (`NSAlert`'s `messageText` / `informativeText`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertMessage {
    pub title: String,
    pub body: String,
}

/// How an off-UI-thread screenshot ended (`CaptureMsg::ShotSaved`).
///
/// This used to be a bare `bool`, and `false` laundered three genuinely different
/// situations into one silent exit: the grab produced no image, the write to disk failed,
/// and the worker thread PANICKED (the oneshot resolving to `Err` means the sender was
/// dropped without sending, which happens only on a panic — never on an empty grab).
///
/// It is a transport detail of one message, NOT a rival vocabulary: [`Self::failure`] maps
/// it straight into [`Failure`], which is what both the debug log and the alert are keyed
/// on. `Saved` is exactly the old `true`; every other variant is exactly the old `false`,
/// so nothing changes but which message each one produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShotOutcome {
    /// Captured and written.
    Saved,
    /// The grab returned nothing to write.
    NoImage,
    /// There were pixels; writing the file failed.
    SaveFailed,
    /// The capture worker thread died before reporting anything (a panic).
    WorkerDied,
}

impl ShotOutcome {
    /// The shared-vocabulary failure this outcome is, or `None` when it delivered.
    pub fn failure(self) -> Option<Failure> {
        match self {
            ShotOutcome::Saved => None,
            ShotOutcome::NoImage => Some(Failure::NoImage),
            ShotOutcome::SaveFailed => Some(Failure::SaveFailed),
            ShotOutcome::WorkerDied => Some(Failure::WorkerPanic),
        }
    }
}

/// The failures a missing Screen Recording grant genuinely EXPLAINS.
///
/// A save failure names the exact call that failed and had pixels to write, a decode
/// failure happened after a successful capture, and a panic is a panic — for those, blaming
/// a grant would be inventing a cause even though we checked. Everything else in the list
/// cannot happen with a working grant, so a missing one is the answer.
fn permission_explains(f: Failure) -> bool {
    matches!(
        f,
        Failure::NoOutputs
            | Failure::PermissionDenied
            | Failure::SceneGrabTimeout
            | Failure::NoImage
            | Failure::RecordingFailed
    )
}

/// Whether this macOS build is affected by the `SCShareableContent` completion-handler
/// hang (a wedged `replayd`). Pure over `(major, minor)` so the predicate is testable;
/// `None` (an unknown OS) is NOT affected — an unnamed build must never be told it has a
/// bug we cannot see.
///
/// DRAGON-439 widened this from 14.0-14.3 to ALL of macOS 14. Apple's 14.4 fix was
/// treated as closing the bug, so a stall on 14.4+ fell through to the generic "macOS did
/// not answer in time" message — but the wedged-`replayd` stall is reported right across
/// Sonoma, and the generic message names neither the cause nor the restart that clears it.
/// The version still matters for the ADVICE (only a pre-14.4 machine has an update to
/// take), not for whether the machine is affected.
fn sonoma_content_hang(os: Option<(u32, u32)>) -> bool {
    matches!(os, Some((14, _)))
}

/// Whether this macOS can run a capture AT ALL (DRAGON-431, lowered to 13.0 by DRAGON-444).
///
/// The app installs on macOS 13 (`LSMinimumSystemVersion` is 13.0 and stays that way).
/// DRAGON-444 landed the pre-14 capture path: the two ScreenCaptureKit APIs that used to
/// force a 14.0 floor — `SCScreenshotManager` (one-shot stills) and
/// `SCShareableContent.infoForFilter:` (pixel scale + content rect, which HARD-ABORTS the
/// process on 13 with no exception handler) — are now replaced below 14 by primitives
/// available since 12.3/13.0 (a one-shot `SCStream` grab, plus CoreGraphics /
/// `SCWindow.frame` sizing). So both stills AND recording work down to 13.0, which is where
/// audio capture (`SCStreamConfiguration.capturesAudio`) and this app's minimum bottom out.
///
/// macOS 12 and older are still gated here and told why: content enumeration exists on 12.3
/// but the audio-capture selectors the recording pipeline needs do not, so we hold the floor
/// at 13.0 rather than ship a half-working 12 with a silent hole.
///
/// `None` (the version could not be read) is TRUE — fail OPEN. Never lock a healthy modern
/// box out of capture because a version probe failed; if such a machine really is too old,
/// the SCK call itself will say so. Pure over `(major, minor)` so the whole table is
/// testable on any platform.
// `test` as well as macOS so the Linux/Windows suites exercise it; the guard that reads it
// is macOS-only, hence dead elsewhere.
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn capture_supported_on(os: Option<(u32, u32)>) -> bool {
    !matches!(os, Some((major, _)) if major < 13)
}

/// [`capture_supported_on`] with the DRAGON-431 QA override folded in — the exact question
/// the three macOS guards ask.
///
/// `forced_unsupported` is the `CCK_TEST_FORCE_UNSUPPORTED_OS=1` review aid (see
/// `platform::mac::force_unsupported_os`), in the spirit of the `CCK_HEALTH_FORCE_*` flags:
/// it exists so the macOS 13 experience can be exercised on a modern Mac, since the real
/// path is otherwise only reachable on 13 hardware.
///
/// It is deliberately folded in HERE and nowhere near `os_version()`. Shadowing the version
/// itself would perturb every other consumer of it — the Sonoma-stall predicate above most of
/// all, which would then name the wrong bug in an alert. This forces the GUARD's answer and
/// changes nothing else.
///
/// The override can only ever make the answer MORE restrictive: it never turns an unsupported
/// OS into a supported one, so a stray environment variable cannot re-enable a path that
/// aborts the process.
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn capture_supported_with(os: Option<(u32, u32)>, forced_unsupported: bool) -> bool {
    !forced_unsupported && capture_supported_on(os)
}

/// Trim a reason string and give it a full stop if it has no sentence-ending punctuation,
/// so a worker's terse error reads as a sentence in the alert.
fn as_sentence(reason: &str) -> String {
    let r = reason.trim();
    if r.is_empty() || r.ends_with(['.', '!', '?']) {
        r.to_string()
    } else {
        format!("{r}.")
    }
}

/// The permission message: the highest-value line in this ticket. Screen Recording is the
/// grant a macOS upgrade routinely invalidates, and when it is not usable ScreenCaptureKit
/// returns an EMPTY list rather than an error — which is why every capture mode then fails
/// identically, with no file and (until now) no message.
fn permission_missing() -> AlertMessage {
    AlertMessage {
        title: "Cosmic Capture Kit can't record the screen.".into(),
        body: "macOS Screen Recording permission is missing. This often happens after a \
               macOS update.\n\nOpen System Settings → Privacy & Security → Screen \
               Recording, and turn Cosmic Capture Kit on. If it is already on, remove it \
               with the minus button and relaunch to grant it again."
            .into(),
    }
}

/// The message for "it broke and we cannot name why". Used for a dead worker, for a panic,
/// and for a runtime that failed to run at all.
fn unknown_failure() -> AlertMessage {
    AlertMessage {
        title: "The capture didn't complete.".into(),
        body: "Something went wrong and nothing was saved.".into(),
    }
}

/// Choose the message for a failure, or `None` when this outcome warrants no dialog.
///
/// PURE — this is the table the tests pin. `detail` is the `diag` note's own text and is
/// surfaced ONLY for [`Failure::RecordingFailed`], where the worker's reason IS the
/// diagnosis; every other detail is developer-facing telemetry and stays in the log.
/// `folder` is the capture directory, shown only where it is the actionable part.
/// See the module doc for the honesty rule the ordering encodes.
pub fn alert_message(
    failure: Failure,
    detail: &str,
    permission: ScreenPermission,
    os: Option<(u32, u32)>,
    folder: Option<&str>,
) -> Option<AlertMessage> {
    // Endings that are not failures, and failures where a dialog would be wrong:
    //
    // * `Cancelled` / `HandoffAccepted` are ordinary endings (`is_loss()` is false).
    // * `NoPreviewOutput` DELIVERS — the capture is saved, copied and notified; only the
    //   editor is missing, and a modal on top of that notification is noise.
    // * `PreviewSurfaceLost` cannot be told apart from a legitimate close confidently
    //   enough to risk a false dialog on a platform this change cannot be run on. It is
    //   recorded in the debug log (DRAGON-419), where a wrong guess costs nothing.
    if matches!(
        failure,
        Failure::Cancelled
            | Failure::HandoffAccepted
            | Failure::NoPreviewOutput
            | Failure::PreviewSurfaceLost
    ) {
        return None;
    }
    // A missing grant is both the most likely cause of a silent capture failure and the
    // only one the user can fix, so it wins wherever it genuinely explains the failure.
    if permission == ScreenPermission::Missing && permission_explains(failure) {
        return Some(permission_missing());
    }
    Some(match failure {
        // Recorded when the preflight answered no. Reaching here means the LIVE preflight
        // now says otherwise; the recorded fact is the more specific one, so report it
        // rather than falling through to a vaguer message.
        Failure::PermissionDenied => permission_missing(),
        Failure::NoOutputs => AlertMessage {
            title: "Cosmic Capture Kit couldn't find a display to capture.".into(),
            // With the grant CHECKED and reported present, say so and give the one repair
            // that fixes a grant which is listed but no longer working. Stating it here is
            // not guessing: it is reporting the check we ran, plus the contradiction. Only
            // a real preflight answers `Granted`, so this branch is macOS-only in practice
            // and keeps its macOS-specific repair.
            body: if permission == ScreenPermission::Granted {
                "macOS reports that Screen Recording permission is granted, but returned no \
                 displays, so nothing was captured.\n\nRestarting your Mac usually clears \
                 this. If it keeps happening, remove Cosmic Capture Kit from System Settings \
                 → Privacy & Security → Screen Recording with the minus button, then relaunch \
                 to grant it again."
                    .into()
            } else {
                // DRAGON-436: Windows reaches this branch too (its preflight is always
                // `Unchecked`), where naming macOS was simply false. Nothing here is
                // platform-specific, so nothing here names a platform.
                "Your system returned no displays, so nothing was captured. Restarting your \
                 computer usually clears this."
                    .into()
            },
        },
        // DRAGON-437. Deliberately NOT the `NoOutputs` message: the displays were found and
        // are fine, so "restart your computer" would send the user after the wrong thing.
        // Names no cause because the honest answer is that OUR window did not appear, and
        // there is nothing the user can do about that from here.
        Failure::OverlayNeverShown => AlertMessage {
            title: "The capture couldn't be shown.".into(),
            body: "The capture window could not be brought on screen, so nothing was captured."
                .into(),
        },
        Failure::SceneGrabTimeout => AlertMessage {
            title: "Screen capture isn't responding.".into(),
            body: if sonoma_content_hang(os) {
                // Named ONLY on Sonoma. A stall on a later macOS is a different problem
                // and must not be mislabelled as this one.
                let mut b = String::from(
                    "This is a known macOS 14 (Sonoma) problem: the system's screen \
                     capture service stopped responding, and nothing was captured. \
                     Restarting your Mac usually clears it.",
                );
                // Below 14.4 there is still an update to take, and it is worth taking —
                // but it makes the stall RARER rather than ending it (DRAGON-439), so the
                // sentence promises exactly that and only where it applies. Matched on the
                // full `(14, minor)` rather than a wildcard major, so the advice stays
                // correct on its own if `sonoma_content_hang` ever widens again.
                if matches!(os, Some((14, minor)) if minor <= 3) {
                    b.push_str(" Updating to macOS 14.4 or later makes it happen less often.");
                }
                b
            } else {
                "macOS did not answer the screen capture request in time, so nothing was \
                 captured. Restarting your Mac usually clears this."
                    .into()
            },
        },
        Failure::NoImage => AlertMessage {
            title: "Nothing was captured.".into(),
            body: "The capture came back empty, so no file was saved.".into(),
        },
        Failure::SaveFailed => AlertMessage {
            title: "The capture couldn't be saved.".into(),
            body: match folder {
                Some(f) => format!(
                    "Cosmic Capture Kit captured the screen but could not write the file to \
                     {f}.\n\nCheck that the folder still exists and that the disk is not full."
                ),
                None => "Cosmic Capture Kit captured the screen but could not write the file \
                         to the capture folder.\n\nCheck that the folder still exists and that \
                         the disk is not full."
                    .into(),
            },
        },
        Failure::RecordingFailed => AlertMessage {
            title: "The recording couldn't be completed.".into(),
            // The worker's reason is the whole diagnosis for a recording that produced
            // nothing, so it is repeated verbatim; a reason that is somehow empty leaves
            // the headline standing on its own.
            body: match as_sentence(detail).as_str() {
                "" => "Nothing was saved.".to_string(),
                r => format!("Nothing was saved.\n\n{r}"),
            },
        },
        // DRAGON-423. Deliberately NOT the `RecordingFailed` message: a wedged session is
        // usually salvaged, so "nothing was saved" would be a lie, and where the take went
        // is the only thing the user needs from this dialog. `progress::wedge_detail` is
        // written to be exactly that sentence, in the log and here, so it stands alone.
        Failure::RecordingWedged => AlertMessage {
            title: "The recording stopped responding.".into(),
            body: match as_sentence(detail).as_str() {
                "" => "It was ended so it could not keep running in the background.".to_string(),
                r => r.to_string(),
            },
        },
        Failure::DecodeFailed => AlertMessage {
            title: "The editor couldn't open.".into(),
            body: "Your capture was saved to the capture folder, but the preview editor \
                   could not open it."
                .into(),
        },
        // DRAGON-431. Deliberately NOT routed through `permission_explains`: a missing grant
        // is not why this failed, and sending a user on macOS 13 to the Screen Recording pane
        // would waste their time on a setting that was never the cause. The version is the
        // whole answer, so it is the whole message.
        Failure::UnsupportedOs => AlertMessage {
            title: "This version of macOS can't capture yet.".into(),
            body: "Cosmic Capture Kit needs macOS 13 or later to capture the screen, so \
                   nothing was captured. Settings and the preview editor still work on this \
                   version."
                .into(),
        },
        // A dead worker tells us the session broke, not why — so say exactly that.
        Failure::WorkerPanic => unknown_failure(),
        // Unreachable (filtered above), but a total match beats a `_` arm that would
        // silently absorb a NEW `Failure` variant into some wrong message.
        Failure::Cancelled
        | Failure::HandoffAccepted
        | Failure::NoPreviewOutput
        | Failure::PreviewSurfaceLost => return None,
    })
}

/// The message for a process dying from a panic — the one failure that has to be built
/// without an `App` (the panic hook has no access to one) and without touching TCC (a
/// permission never explains a crash, and a panic hook should do the minimum).
pub fn crash_alert() -> AlertMessage {
    AlertMessage {
        title: "Cosmic Capture Kit stopped unexpectedly.".into(),
        // The panic log and DRAGON-419's debug log share this folder, so one sentence
        // points at everything support could ask for.
        body: "The capture did not complete and nothing was saved.\n\nDetails were written \
               to Library/Logs/cosmic-capture-kit in your home folder."
            .into(),
    }
}

/// The message for the iced/winit runtime failing outright (`cosmic::app::run` returning
/// `Err`), where no in-app path ever ran. Names no cause because none is available here.
pub fn runtime_failure_alert() -> AlertMessage {
    unknown_failure()
}

/// Windows (DRAGON-442): the panic-hook message.
///
/// A separate builder from [`crash_alert`] because the mac one hard-codes
/// `Library/Logs/cosmic-capture-kit`, and that sentence is true there for a reason that does
/// NOT hold on Windows: mac writes `panic.log` and the DRAGON-419 debug log into the SAME
/// folder, so one path covers everything support asks for. Windows splits them —
/// `panic.log` goes to the app config dir (`~/.config/cosmic-capture-kit`, via
/// `util::app_config_dir`) and the debug log to `%LOCALAPPDATA%\cosmic-capture-kit\logs`.
/// Naming only one of them would send a customer to a folder without the file we asked for.
///
/// Pure over both folders, so the copy is testable without crashing a process. Either may be
/// `None` when the path could not be resolved (no home directory), and the sentence for it is
/// then omitted rather than printed empty.
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
pub fn windows_crash_alert(panic_log_dir: Option<&str>, debug_log_dir: Option<&str>) -> AlertMessage {
    let mut body = String::from("The capture did not complete and nothing was saved.");
    if let Some(p) = panic_log_dir {
        body.push_str(&format!("\n\nDetails were written to {p}."));
    }
    if let Some(d) = debug_log_dir {
        body.push_str(&format!("\n\nThe debug log is in {d}."));
    }
    AlertMessage { title: "Cosmic Capture Kit stopped unexpectedly.".into(), body }
}

/// Windows (DRAGON-442): the message for `cosmic::app::run` returning `Err`.
///
/// Unlike [`runtime_failure_alert`]'s deliberately vague "something went wrong", this seam
/// DOES know something: the app never got its display system up. That is worth saying,
/// because it points a reader at drivers or a remote session rather than at the capture.
/// It still names no specific cause, because none is available here.
#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
pub fn windows_runtime_failure_alert(debug_log_dir: Option<&str>) -> AlertMessage {
    let mut body = String::from(
        "Cosmic Capture Kit could not start its display system, so nothing was captured. \
         This is usually a graphics driver problem, or a remote session with no display.",
    );
    if let Some(d) = debug_log_dir {
        body.push_str(&format!("\n\nThe debug log is in {d}."));
    }
    AlertMessage { title: "Cosmic Capture Kit couldn't start.".into(), body }
}

/// The live Screen Recording preflight, at the moment a failure is reported.
///
/// macOS only — and it is the NON-prompting probe (`CGPreflightScreenCaptureAccess` via
/// `tcc::screen_capture_granted`), so reporting a failure never pops a system dialog on
/// top of the alert we are about to show. Everywhere else there is nothing to check, which
/// is [`ScreenPermission::Unchecked`], not an assumption.
fn screen_permission_now() -> ScreenPermission {
    #[cfg(target_os = "macos")]
    {
        if crate::platform::mac::tcc::screen_capture_granted() {
            ScreenPermission::Granted
        } else {
            ScreenPermission::Missing
        }
    }
    #[cfg(not(target_os = "macos"))]
    ScreenPermission::Unchecked
}

/// Windows (DRAGON-436): how long `fail_session` will wait for the user to click OK before
/// ending the session anyway.
///
/// Two minutes is far more than anyone needs for one sentence and one button, and it is
/// FINITE on purpose: nobody is guaranteed to be at the screen. A disconnected RDP session
/// or a headless machine is exactly a zero-displays scenario, which is exactly when this
/// alert fires. Ending the session after the budget is not a new silent exit — the alert was
/// presented, and the `diag::Failure` behind it was recorded before any of this ran.
#[cfg(windows)]
/// `pub(crate)` since DRAGON-442: the two app-less seams (the panic hook and the
/// runtime-`Err` exit) wait on the same number, so there is one budget, not three.
pub(crate) const ALERT_DISMISS_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// Windows (DRAGON-436): await an alert's dismissal without blocking the UI thread.
///
/// The repo's standing pattern for a native Windows modal (see `app::mod`'s `pick_folder`):
/// the blocking wait happens on a thread of its own, and the async side only awaits a
/// oneshot. `Task::perform` runs this on iced's executor, so `App::update` returns
/// immediately and the event loop keeps servicing our windows while the box is up.
#[cfg(windows)]
pub(super) async fn await_dismissal(dismissal: crate::platform::windows::alert::Dismissal) {
    use crate::platform::windows::alert::AlertOutcome;
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        // Each outcome gets its own line, because they mean different things to whoever
        // reads the log. Collapsing them is how "the presenter died instantly" once got
        // reported as "nobody was at the screen for two minutes".
        match dismissal.wait(ALERT_DISMISS_BUDGET) {
            AlertOutcome::Dismissed => {
                log::info!("DRAGON-436: the user dismissed the failure alert");
            }
            AlertOutcome::TimedOut => log::warn!(
                "DRAGON-436: the failure alert was not dismissed within {}s — ending the \
                 session anyway (nobody appears to be at the screen)",
                ALERT_DISMISS_BUDGET.as_secs()
            ),
            AlertOutcome::PresenterGone => log::warn!(
                "DRAGON-436: the failure alert never made it on screen (its presenter ended \
                 without reporting) — ending the session; the user was told nothing"
            ),
        }
        let _ = tx.send(());
    });
    let _ = rx.await;
}

/// macOS: await an `NSAlert`'s dismissal without blocking the winit thread.
///
/// The same pattern as Windows' [`await_dismissal`] (and the repo's standing one for a
/// native modal — see `app::mod`'s `pick_folder`): the blocking wait happens on a thread
/// of its own, and the async side only awaits a oneshot, so `App::update` returns
/// immediately and the event loop keeps servicing our windows while the alert is up. Unlike
/// Windows there is no dismiss budget here: `NSAlert::runModal` genuinely pumps the run
/// loop on its OWN thread once it is not nested inside winit's, so a disconnected-screen
/// session just leaves that one thread parked rather than wedging anything the app needs.
#[cfg(target_os = "macos")]
pub(super) async fn await_mac_dismissal(dismissal: crate::platform::mac::alert::Dismissal) {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        dismissal.wait();
        log::info!("DRAGON-415: the failure alert was dismissed");
        let _ = tx.send(());
    });
    let _ = rx.await;
}

impl App {
    /// Present this session's failure to the user WITHOUT ending the session — for the one
    /// caller that owns its own teardown (a preview document that could not open still has
    /// to go through `close_preview`, which ends the process only if it was the last one).
    ///
    /// LINUX ONLY. It presents nothing: its warnings reach a terminal, and adding a modal
    /// dialog to a path that already speaks would be a behaviour change for no gain — a
    /// deliberate no-op, so its behaviour is byte-identical to before.
    ///
    /// Neither Windows nor macOS compile this. Windows never did (DRAGON-436 round 2): a
    /// `MessageBox` does not block, so there is no such thing as "present it and carry on"
    /// there — its callers go through [`App::show_failure_alert`] + [`await_dismissal`]
    /// instead. macOS stopped compiling it at DRAGON-415's mac crash fix: `NSAlert` DOES
    /// block, but only safely from a thread that is not the one pumping our run loop (see
    /// `platform::mac::alert`'s module doc) — so mac's callers go through
    /// [`App::report_failure_deferred`] + [`await_mac_dismissal`] the same shape as Windows.
    #[cfg(not(any(windows, target_os = "macos")))]
    pub(super) fn report_failure(&mut self) {
        // `permission` is always `Unchecked` here, which is the honest answer, not a
        // placeholder.
        let _ = screen_permission_now();
    }

    /// macOS (DRAGON-415's crash fix): the async twin of the (now Linux-only)
    /// [`App::report_failure`] — puts the alert up and returns AT ONCE instead of blocking,
    /// because `NSAlert::runModal`'s nested run loop panics winit's re-entrancy guard when
    /// it runs nested inside the winit-dispatched callback that call this (see
    /// `platform::mac::alert`'s module doc for the crash this replaced). `None` means
    /// nothing was shown (suppressed, or the presenter thread could not be spawned) and
    /// there is nothing to wait for.
    ///
    /// Reads the ROOT failure this process recorded (`diag::root_failure`), so a symptom
    /// noted downstream cannot displace the diagnosis noted upstream: a ScreenCaptureKit
    /// call that never answered is what the user hears about, not the empty grab it caused.
    #[cfg(target_os = "macos")]
    pub(super) fn report_failure_deferred(&mut self) -> Option<crate::platform::mac::alert::Dismissal> {
        let permission = screen_permission_now();
        // DRAGON-413 carve-out. The startup guard kills a child that never presents
        // anything, and its budget is SUSPENDED while a window the user must act on is up —
        // a child showing the permission checker is doing its job, and so is one showing
        // this. Published HERE, before the alert goes up, so the guard cannot spend against
        // a presence published only after the dialog already existed.
        crate::startup_guard::report(crate::startup_guard::Presence::AwaitingUser);
        let Some((failure, detail)) = crate::diag::root_failure() else {
            // Nothing was recorded, which should not happen at a `fail_session` site — but
            // a session ending with nothing must still SAY so rather than fall back to the
            // silence this whole ticket exists to remove.
            let msg = unknown_failure();
            return crate::platform::mac::alert::show_deferred(&msg.title, &msg.body);
        };
        let folder = self.capture_folder_display();
        let os = crate::platform::mac::os_version();
        let msg = alert_message(failure, &detail, permission, Some(os), folder.as_deref())?;
        crate::platform::mac::alert::show_deferred(&msg.title, &msg.body)
    }

    /// Windows (DRAGON-436): the message this session's failure deserves, or `None` when the
    /// outcome warrants no dialog at all.
    ///
    /// The mac arm's shape, minus the two things Windows does not have: no startup guard to
    /// suspend (that is macOS-only, so there is no budget being spent here), and no OS
    /// version to pass — the only version-keyed copy in the table names a macOS bug, and
    /// `SceneGrabTimeout` is noted nowhere but the mac plugin anyway. The preflight is
    /// `Unchecked` here by construction (see [`screen_permission_now`]), which is what keeps
    /// every message off the subject of grants we never checked.
    ///
    #[cfg(windows)]
    fn windows_failure_alert(&self) -> Option<AlertMessage> {
        let Some((failure, detail)) = crate::diag::root_failure() else {
            // Nothing was recorded, which should not happen at a `fail_session` site — but a
            // session ending with nothing must still SAY so rather than fall back to the
            // silence this whole ticket exists to remove.
            return Some(unknown_failure());
        };
        let folder = self.capture_folder_display();
        alert_message(failure, &detail, screen_permission_now(), None, folder.as_deref())
    }

    /// Windows (DRAGON-436): put this session's failure on screen and hand back the wait.
    ///
    /// THE Windows entry point — both callers use it, and both then wait, because on Windows
    /// there is no other honest option (see [`App::report_failure`]'s note). `None` means
    /// nothing was shown and nothing needs waiting for: the outcome warranted no dialog, the
    /// once-per-process latch had already fired, another instance is speaking, or the
    /// presenter could not start.
    #[cfg(windows)]
    pub(super) fn show_failure_alert(&self) -> Option<crate::platform::windows::alert::Dismissal> {
        self.windows_failure_alert()
            .and_then(|msg| crate::platform::windows::alert::show(&msg.title, &msg.body))
    }

    /// Tell the user what happened, then end the one-shot session.
    ///
    /// THE seam for every non-delivery exit: the alert goes up, the user dismisses it, and
    /// only then does `finish_session` run — so the child exits cleanly through the normal
    /// path instead of dying invisibly. On Linux this is exactly `finish_session`, unchanged
    /// (nothing is ever shown there).
    ///
    /// Windows and macOS reach "only then" the SAME way: neither may block `App::update`'s
    /// own thread to wait for the dismissal (Windows' `MessageBoxW` would park the thread
    /// that owns every HWND we have; macOS's `NSAlert::runModal` would nest its own pump
    /// inside the winit callback still on the stack and panic winit's re-entrancy guard —
    /// see `platform::mac::alert`'s module doc for the crash this replaced). So both hand
    /// back a `Dismissal`, wait for it on a dedicated blocking thread, and re-enter through
    /// `WindowChromeMsg::FailureAlertDismissed` — the update loop keeps pumping throughout.
    /// See `platform::windows::alert` / `platform::mac::alert` for the full reasoning.
    ///
    /// Call it AFTER the site's `diag::note_failure`, which is what it reads.
    // Both the Windows and macOS bodies are cfg'd `{ return … }` blocks followed by a
    // cfg'd-out Linux-only tail; dropping the `return` there is a compile error, so allow
    // the lint on those two platforms only (Linux compiles the tail branch and never sees
    // it). Same shape as `seed_outputs_mac`'s macOS allow.
    #[cfg_attr(any(windows, target_os = "macos"), allow(clippy::needless_return))]
    pub(super) fn fail_session(&mut self) -> Task<cosmic::Action<Msg>> {
        #[cfg(windows)]
        {
            let Some(dismissal) = self.show_failure_alert() else {
                // Nothing was put up (suppressed, or the presenter could not start), so
                // there is nothing to wait for and the session ends now, as it always did.
                return self.finish_session();
            };
            return Task::perform(await_dismissal(dismissal), |()| {
                cosmic::Action::App(Msg::WindowChrome(WindowChromeMsg::FailureAlertDismissed))
            });
        }
        #[cfg(target_os = "macos")]
        {
            let Some(dismissal) = self.report_failure_deferred() else {
                // Nothing was put up (suppressed, or the presenter thread could not be
                // spawned), so there is nothing to wait for and the session ends now.
                return self.finish_session();
            };
            return Task::perform(await_mac_dismissal(dismissal), |()| {
                cosmic::Action::App(Msg::WindowChrome(WindowChromeMsg::FailureAlertDismissed))
            });
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            self.report_failure();
            self.finish_session()
        }
    }

    /// The capture folder as the USER would recognise it, for a save-failure message.
    ///
    /// Shown only in the alert, which is on the user's own screen. The debug log keeps
    /// using `diag::path_shape`, because that file gets emailed to us and a capture path
    /// can name the user's documents.
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    fn capture_folder_display(&self) -> Option<String> {
        let raw = self.screenshot_dir.trim();
        (!raw.is_empty()).then(|| raw.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shorthand: the common case (permission granted, a modern OS, a known folder).
    fn msg(f: Failure) -> AlertMessage {
        alert_message(f, "", ScreenPermission::Granted, Some((14, 8)), Some("~/Capture"))
            .expect("this failure warrants an alert")
    }

    /// Every failure in the shared vocabulary, so the sweeps below cannot silently miss a
    /// variant added later.
    fn all_failures() -> Vec<Failure> {
        vec![
            Failure::NoOutputs,
            Failure::OverlayNeverShown,
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
            Failure::UnsupportedOs,
        ]
    }

    // ── The headline message ────────────────────────────────────────────────

    #[test]
    fn a_missing_grant_names_the_permission_and_the_repair() {
        let m = alert_message(Failure::NoImage, "", ScreenPermission::Missing, Some((14, 8)), None)
            .unwrap();
        assert_eq!(m.title, "Cosmic Capture Kit can't record the screen.");
        assert!(m.body.contains("Screen Recording permission is missing"));
        assert!(m.body.contains("after a macOS update"));
        assert!(m.body.contains("System Settings → Privacy & Security → Screen Recording"));
        // The stale-grant repair — the case where the app is already listed.
        assert!(m.body.contains("minus button"));
    }

    #[test]
    fn a_missing_grant_wins_over_every_failure_it_explains() {
        for f in [
            Failure::NoOutputs,
            Failure::PermissionDenied,
            Failure::SceneGrabTimeout,
            Failure::NoImage,
            Failure::RecordingFailed,
        ] {
            let m =
                alert_message(f, "boom", ScreenPermission::Missing, Some((14, 3)), None).unwrap();
            assert_eq!(
                m.title, "Cosmic Capture Kit can't record the screen.",
                "{f:?} should report the missing grant"
            );
        }
    }

    #[test]
    fn a_missing_grant_does_not_hijack_failures_it_cannot_explain() {
        // Pixels existed and the write failed; the decode case had already saved a file; a
        // panic is a panic. Claiming a grant for these would be inventing a cause.
        for f in [Failure::SaveFailed, Failure::DecodeFailed, Failure::WorkerPanic] {
            let m = alert_message(f, "", ScreenPermission::Missing, Some((14, 8)), None).unwrap();
            assert_ne!(
                m.title, "Cosmic Capture Kit can't record the screen.",
                "{f:?} must not be reported as a permission problem"
            );
        }
    }

    // ── Never claim a permission problem we did not establish ───────────────

    #[test]
    fn nothing_claims_a_missing_permission_without_a_missing_preflight() {
        for perm in [ScreenPermission::Granted, ScreenPermission::Unchecked] {
            for f in all_failures() {
                // `PermissionDenied` is the recorded ANSWER of a preflight that said no, so
                // it legitimately reports one whatever a later probe says.
                if f == Failure::PermissionDenied {
                    continue;
                }
                for os in [None, Some((14, 3)), Some((14, 8)), Some((15, 0))] {
                    let Some(m) = alert_message(f, "d", perm, os, Some("~/Capture")) else {
                        continue;
                    };
                    assert!(
                        !m.body.contains("permission is missing"),
                        "{f:?} / {perm:?} must not claim a missing permission"
                    );
                    assert!(!m.title.contains("can't record the screen"));
                }
            }
        }
    }

    #[test]
    fn an_unchecked_permission_never_mentions_screen_recording_at_all() {
        // Off macOS there is no grant to check, and no macOS path records `PermissionDenied`
        // there either, so the messages must not send anyone to a privacy pane on the
        // strength of a check that never ran.
        for f in all_failures() {
            if f == Failure::PermissionDenied {
                continue;
            }
            let Some(m) = alert_message(f, "d", ScreenPermission::Unchecked, None, None) else {
                continue;
            };
            assert!(
                !m.body.contains("Screen Recording") && !m.title.contains("Screen Recording"),
                "{f:?} mentioned Screen Recording without checking it"
            );
        }
    }

    #[test]
    fn a_granted_preflight_may_report_the_contradiction_it_actually_observed() {
        // Checked, reported granted, and yet zero displays: state both facts and offer the
        // repair for a grant that is listed but no longer working. This is a report, not a
        // guess — and it still must not say the permission is missing.
        let m = msg(Failure::NoOutputs);
        assert!(m.body.contains("reports that Screen Recording permission is granted"));
        assert!(m.body.contains("returned no displays"));
        assert!(!m.body.contains("permission is missing"));
    }

    #[test]
    fn an_unchecked_no_display_report_names_no_operating_system() {
        // DRAGON-436: this branch now reaches Windows, whose display enumeration depends on
        // no grant at all — so "macOS returned no displays" was simply false there, and the
        // repair it offered ("restart your Mac") named a machine the user does not own. Only
        // a live preflight can answer `Granted`, so the macOS-specific branch above stays
        // macOS-only and keeps its macOS-specific wording.
        let m =
            alert_message(Failure::NoOutputs, "", ScreenPermission::Unchecked, None, None).unwrap();
        assert!(m.body.contains("Your system returned no displays"));
        assert!(m.body.contains("Restarting your computer"));
        assert!(!m.body.contains("macOS") && !m.body.contains("Mac"));
    }

    // ── The Sonoma stall is named ONLY where it exists ──────────────────────

    /// The `SceneGrabTimeout` body for one OS, which is the only thing these three tests
    /// differ on.
    fn stall_body(os: Option<(u32, u32)>) -> String {
        alert_message(Failure::SceneGrabTimeout, "", ScreenPermission::Granted, os, None)
            .unwrap()
            .body
    }

    #[test]
    fn a_stall_on_sonoma_before_14_4_also_offers_the_update() {
        for minor in [0, 1, 2, 3] {
            let b = stall_body(Some((14, minor)));
            assert!(b.contains("known macOS 14 (Sonoma) problem"), "14.{minor}");
            assert!(b.contains("screen capture service stopped responding"), "14.{minor}");
            assert!(b.contains("Restarting your Mac"), "14.{minor}");
            // The update is still worth taking below 14.4, but it makes the stall rarer
            // rather than curing it — the wording must not promise a fix.
            assert!(
                b.contains("Updating to macOS 14.4 or later makes it happen less often"),
                "14.{minor}"
            );
            assert!(!b.contains("fixes it"), "14.{minor}");
        }
    }

    #[test]
    fn a_stall_on_14_4_or_later_sonoma_still_names_sonoma_but_offers_no_update() {
        // DRAGON-439: these builds used to fall through to the generic timeout message,
        // which named neither the cause nor the restart that clears it.
        for minor in [4, 5, 6, 7, 8] {
            let b = stall_body(Some((14, minor)));
            assert!(b.contains("known macOS 14 (Sonoma) problem"), "14.{minor}");
            assert!(b.contains("Restarting your Mac"), "14.{minor}");
            // Already past the update, so it must not be advertised.
            assert!(!b.contains("14.4"), "14.{minor} must not advise updating to 14.4");
        }
    }

    // ── DRAGON-431: which macOS can capture at all ──────────────────────────

    /// macOS 12 and below cannot capture: DRAGON-444 lowered the floor to 13.0 (where the
    /// pre-14 capture path plus audio-capture selectors bottom out), so 12 and older are
    /// still gated. The app still INSTALLS there (`LSMinimumSystemVersion` stays 13.0) — a
    /// pre-13 launch just says why instead of vanishing.
    #[test]
    fn macos_12_and_older_cannot_capture() {
        assert!(!capture_supported_on(Some((12, 3))));
        assert!(!capture_supported_on(Some((12, 0))));
        assert!(!capture_supported_on(Some((11, 0))));
        assert!(!capture_supported_on(Some((10, 15))));
    }

    #[test]
    fn macos_13_and_newer_capture_normally() {
        // DRAGON-444: 13.x now captures through the pre-14 fallback path, alongside the whole
        // 14+ range — including the versions the Sonoma stall predicate above also fires on
        // (being affected by that bug is not the same as being unable to capture, and these
        // two predicates must not be conflated).
        for os in [(13, 0), (13, 3), (13, 6), (14, 0), (14, 3), (14, 4), (14, 8), (15, 0), (26, 1)] {
            assert!(capture_supported_on(Some(os)), "{os:?}");
        }
    }

    /// The fail-OPEN rule, which is the one that could hurt real users if it were wrong: an
    /// unreadable version must never gate capture off. A modern box whose version probe
    /// failed still captures, and if it genuinely is too old the SCK call speaks for itself.
    #[test]
    fn an_unreadable_version_still_captures() {
        assert!(capture_supported_on(None));
    }

    /// The QA override (`CCK_TEST_FORCE_UNSUPPORTED_OS=1`) forces the guard's answer and
    /// nothing else. Unset, it is exactly `capture_supported_on`; set, every version reads as
    /// unsupported, so the macOS 13 experience is reachable on a modern Mac.
    #[test]
    fn the_qa_override_forces_the_guard_and_only_the_guard() {
        for os in [None, Some((13, 6)), Some((14, 0)), Some((14, 8)), Some((26, 1))] {
            // Unset: identical to the unforced predicate, on every input.
            assert_eq!(
                capture_supported_with(os, false),
                capture_supported_on(os),
                "unset override changed the answer for {os:?}"
            );
            // Set: unsupported, whatever the machine actually is.
            assert!(!capture_supported_with(os, true), "{os:?} should be forced unsupported");
        }
    }

    /// It can only ever be MORE restrictive. A stray environment variable must not be able to
    /// re-enable capture on a genuinely unsupported macOS. DRAGON-444 moved the floor to 13.0,
    /// so the still-unsupported examples are now 12 and below.
    #[test]
    fn the_qa_override_can_never_re_enable_an_unsupported_os() {
        for forced in [false, true] {
            assert!(!capture_supported_with(Some((12, 6)), forced), "forced={forced}");
            assert!(!capture_supported_with(Some((12, 0)), forced), "forced={forced}");
            assert!(!capture_supported_with(Some((11, 0)), forced), "forced={forced}");
        }
    }

    /// The message a macOS 13 user actually sees. It has to name the requirement (there is
    /// nothing else they can act on), say nothing was captured, and — because the app still
    /// installs and half of it works — not read as "this app is broken".
    #[test]
    fn an_unsupported_macos_names_the_version_it_needs() {
        let m = msg(Failure::UnsupportedOs);
        assert_eq!(m.title, "This version of macOS can't capture yet.");
        assert!(m.body.contains("macOS 13 or later"), "{}", m.body);
        assert!(m.body.contains("nothing was captured"), "{}", m.body);
        // Never a permission story: a grant is not why this failed, and sending someone to
        // the Screen Recording pane would waste their time on a setting that was never it.
        assert!(!m.body.contains("Screen Recording"), "{}", m.body);
        assert!(!m.body.contains("permission"), "{}", m.body);
        // And it stays honest about what DOES work, so the app does not read as dead.
        assert!(m.body.contains("still work"), "{}", m.body);
    }

    /// A missing grant must not hijack this one. On an unsupported macOS the grant is
    /// irrelevant — the capture cannot run at any permission state — so the version message
    /// has to survive even a `Missing` preflight. (DRAGON-444 moved the floor to 13.0, so the
    /// unsupported example is now 12.)
    #[test]
    fn an_unsupported_macos_is_never_reported_as_a_permission_problem() {
        for perm in
            [ScreenPermission::Granted, ScreenPermission::Missing, ScreenPermission::Unchecked]
        {
            let m = alert_message(Failure::UnsupportedOs, "d", perm, Some((12, 6)), None).unwrap();
            assert_eq!(m.title, "This version of macOS can't capture yet.", "{perm:?}");
        }
    }

    #[test]
    fn a_stall_after_sonoma_gets_the_generic_timeout_message() {
        for os in [Some((15, 0)), Some((26, 1)), None] {
            let b = stall_body(os);
            assert!(b.contains("did not answer the screen capture request in time"), "{os:?}");
            assert!(!b.contains("Sonoma"), "{os:?} must not claim the Sonoma stall");
            assert!(!b.contains("14.4"), "{os:?} must not advise updating to 14.4");
        }
    }

    #[test]
    fn the_sonoma_predicate_is_all_of_macos_14() {
        for minor in [0, 3, 4, 8] {
            assert!(sonoma_content_hang(Some((14, minor))), "14.{minor}");
        }
        assert!(!sonoma_content_hang(Some((13, 3))));
        assert!(!sonoma_content_hang(Some((15, 0))));
        assert!(!sonoma_content_hang(None));
    }

    // ── Endings that must NOT put up a dialog ───────────────────────────────

    #[test]
    fn ordinary_endings_and_deliveries_get_no_alert() {
        for f in [
            // Not failures at all.
            Failure::Cancelled,
            Failure::HandoffAccepted,
            // Delivered: saved, copied and notified; only the editor is absent.
            Failure::NoPreviewOutput,
            // Indistinguishable from the user closing the window; logged, not alerted.
            Failure::PreviewSurfaceLost,
        ] {
            for perm in
                [ScreenPermission::Granted, ScreenPermission::Missing, ScreenPermission::Unchecked]
            {
                assert!(
                    alert_message(f, "d", perm, Some((14, 3)), None).is_none(),
                    "{f:?} / {perm:?} must not raise a dialog"
                );
            }
        }
    }

    #[test]
    fn every_other_failure_does_get_an_alert() {
        for f in all_failures() {
            let silent = matches!(
                f,
                Failure::Cancelled
                    | Failure::HandoffAccepted
                    | Failure::NoPreviewOutput
                    | Failure::PreviewSurfaceLost
            );
            assert_eq!(
                alert_message(f, "d", ScreenPermission::Granted, Some((14, 8)), None).is_some(),
                !silent,
                "{f:?}"
            );
        }
    }

    // ── The remaining causes stay distinct ──────────────────────────────────

    #[test]
    fn a_save_failure_names_the_folder_when_known() {
        let m = alert_message(
            Failure::SaveFailed,
            "",
            ScreenPermission::Granted,
            Some((14, 8)),
            Some("/Users/x/Capture"),
        )
        .unwrap();
        assert!(m.body.contains("/Users/x/Capture"));
        assert!(m.body.contains("disk is not full"));
        // And degrades honestly without one, rather than printing an empty path.
        let m =
            alert_message(Failure::SaveFailed, "", ScreenPermission::Granted, None, None).unwrap();
        assert!(m.body.contains("the capture folder"));
    }

    #[test]
    fn a_recording_failure_carries_its_reason_verbatim() {
        let m = alert_message(
            Failure::RecordingFailed,
            "worker reported: audio pre-flight failed: no pulse server",
            ScreenPermission::Granted,
            Some((14, 8)),
            None,
        )
        .unwrap();
        assert!(m.body.contains("audio pre-flight failed: no pulse server"));
        assert!(m.body.contains("Nothing was saved"));
    }

    #[test]
    fn only_the_recording_failures_surface_their_diag_detail() {
        // Every other detail is developer-facing telemetry (selection geometry, path
        // shapes) and must never reach a dialog. The two exceptions are the recording ones,
        // whose details are WRITTEN to be read by the user — the worker's own reason for a
        // failure, and `progress::wedge_detail` for a wedge (which has its own test proving
        // it quotes a file name and never a path).
        let secret = "branch=region sel=13x7@1,2 frozen_flats=3";
        for f in all_failures() {
            if matches!(f, Failure::RecordingFailed | Failure::RecordingWedged) {
                continue;
            }
            let Some(m) = alert_message(f, secret, ScreenPermission::Granted, Some((14, 8)), None)
            else {
                continue;
            };
            assert!(!m.body.contains(secret), "{f:?} leaked its diag detail into the dialog");
        }
    }

    #[test]
    fn an_overlay_that_never_appeared_is_not_reported_as_a_missing_display() {
        // DRAGON-437. The two failures are one step apart and their repairs have nothing in
        // common: `NoOutputs` means the machine reported no displays (restart), this means
        // the displays were fine and OUR window never made it on screen. Sending someone to
        // reboot over the second is the same class of mistake as blaming a permission.
        let m = msg(Failure::OverlayNeverShown);
        assert_eq!(m.title, "The capture couldn't be shown.");
        assert!(m.body.contains("could not be brought on screen"));
        assert!(!m.body.contains("Restart"), "not a restart-your-computer problem");
        assert!(!m.body.contains("no displays"));
        // And it stays distinct from the message it must not be confused with.
        assert_ne!(m.title, msg(Failure::NoOutputs).title);
    }

    #[test]
    fn a_never_shown_overlay_is_never_blamed_on_a_permission() {
        // It is deliberately absent from `permission_explains`: a grant cannot explain a
        // window that was created and then failed to be placed.
        let m = alert_message(
            Failure::OverlayNeverShown,
            "",
            ScreenPermission::Missing,
            Some((14, 8)),
            None,
        )
        .unwrap();
        assert_eq!(m.title, "The capture couldn't be shown.");
    }

    #[test]
    fn an_unknown_cause_is_admitted_rather_than_guessed() {
        let m = msg(Failure::WorkerPanic);
        assert_eq!(m.title, "The capture didn't complete.");
        assert!(m.body.contains("nothing was saved"));
        // No cause is named, because none is known.
        assert!(!m.body.contains("Screen Recording"));
        assert!(!m.body.contains("macOS 14"));
    }

    #[test]
    fn a_decode_failure_says_the_capture_survived() {
        assert!(msg(Failure::DecodeFailed).body.contains("saved"));
    }

    // ── DRAGON-442: the Windows app-less seams ──────────────────────────────

    /// The Windows panic body must name the folder `panic.log` REALLY goes to, and the
    /// debug log's separate folder as well. The mac copy names one folder because mac puts
    /// both files in it; Windows splits them, so naming one would send a customer to a
    /// folder that does not contain the file we asked them for.
    #[test]
    fn the_windows_panic_body_names_both_real_folders() {
        let m = windows_crash_alert(
            Some(r"C:\Users\x\.config\cosmic-capture-kit"),
            Some(r"C:\Users\x\AppData\Local\cosmic-capture-kit\logs"),
        );
        assert_eq!(m.title, "Cosmic Capture Kit stopped unexpectedly.");
        assert!(m.body.contains("nothing was saved"), "{}", m.body);
        assert!(m.body.contains(r".config\cosmic-capture-kit"), "{}", m.body);
        assert!(m.body.contains(r"AppData\Local\cosmic-capture-kit\logs"), "{}", m.body);
        // The mac path must never leak into the Windows copy.
        assert!(!m.body.contains("Library/Logs"), "{}", m.body);
    }

    /// An unresolvable folder omits its sentence rather than printing an empty path — the
    /// failure mode being "Details were written to ." on a machine with no home directory.
    #[test]
    fn an_unresolvable_folder_is_omitted_not_printed_empty() {
        let both_missing = windows_crash_alert(None, None);
        assert!(!both_missing.body.contains("Details were written"), "{}", both_missing.body);
        assert!(!both_missing.body.contains("debug log"), "{}", both_missing.body);
        // Still a real message, still a full sentence.
        assert!(both_missing.body.contains("nothing was saved"));
        // One at a time, each naming only what it knows.
        let panic_only = windows_crash_alert(Some(r"C:\p"), None);
        assert!(panic_only.body.contains(r"C:\p") && !panic_only.body.contains("debug log"));
        let debug_only = windows_crash_alert(None, Some(r"C:\d"));
        assert!(debug_only.body.contains(r"C:\d") && !debug_only.body.contains("Details were"));
    }

    /// The runtime seam knows more than the generic "something went wrong": the app never
    /// got its display system up, which points a reader at drivers or a remote session
    /// rather than at the capture.
    #[test]
    fn the_windows_runtime_body_says_what_actually_failed() {
        let m = windows_runtime_failure_alert(Some(r"C:\logs"));
        assert_eq!(m.title, "Cosmic Capture Kit couldn't start.");
        assert!(m.body.contains("could not start its display system"), "{}", m.body);
        assert!(m.body.contains("nothing was captured"), "{}", m.body);
        assert!(m.body.contains(r"C:\logs"), "{}", m.body);
        // No folder known: no dangling sentence.
        let bare = windows_runtime_failure_alert(None);
        assert!(!bare.body.contains("debug log"), "{}", bare.body);
    }

    /// Both new messages obey the same house rules the `alert_message` sweeps enforce —
    /// they live outside that table, so they need their own sweep or they drift.
    #[test]
    fn the_app_less_windows_messages_follow_the_house_style() {
        let messages = [
            windows_crash_alert(Some(r"C:\p"), Some(r"C:\d")),
            windows_crash_alert(None, None),
            windows_runtime_failure_alert(Some(r"C:\d")),
            windows_runtime_failure_alert(None),
        ];
        for m in messages {
            assert!(!m.title.is_empty() && !m.body.is_empty());
            assert!(m.title.ends_with('.'), "{}", m.title);
            assert!(m.body.ends_with('.'), "{}", m.body);
            assert!(!m.title.contains('—') && !m.body.contains('—'), "{}", m.body);
            // These fire with no App and no TCC probe, so they must never blame a grant.
            assert!(!m.body.contains("Screen Recording"), "{}", m.body);
            assert!(!m.body.contains("permission"), "{}", m.body);
        }
    }

    /// The two seams that have no `App` to ask: the panic hook and a runtime that failed to
    /// run at all. Both must produce a real message, and neither may invent a cause.
    #[test]
    fn the_app_less_seams_report_without_diagnosing() {
        for m in [crash_alert(), runtime_failure_alert()] {
            assert!(!m.title.is_empty() && !m.body.is_empty());
            assert!(!m.body.contains("Screen Recording"));
            assert!(!m.body.contains("macOS 14"));
        }
        assert_eq!(crash_alert().title, "Cosmic Capture Kit stopped unexpectedly.");
        // Must point at where the logs ACTUALLY go — DRAGON-419 put the debug log in the
        // same folder as panic.log — or it sends a customer somewhere empty.
        assert!(crash_alert().body.contains("Library/Logs/cosmic-capture-kit"));
    }

    // ── House style ─────────────────────────────────────────────────────────

    #[test]
    fn every_message_is_non_empty_and_ends_a_sentence() {
        for perm in
            [ScreenPermission::Granted, ScreenPermission::Missing, ScreenPermission::Unchecked]
        {
            for f in all_failures() {
                let Some(m) = alert_message(f, "a reason", perm, Some((14, 8)), Some("~/Capture"))
                else {
                    continue;
                };
                assert!(!m.title.is_empty() && !m.body.is_empty(), "{f:?} / {perm:?}");
                assert!(m.title.ends_with('.'), "{f:?} / {perm:?}: {}", m.title);
                assert!(m.body.ends_with('.'), "{f:?} / {perm:?}: {}", m.body);
            }
        }
    }

    #[test]
    fn no_message_uses_an_em_dash() {
        // House style for user-facing copy: commas, colons and full stops instead.
        for perm in
            [ScreenPermission::Granted, ScreenPermission::Missing, ScreenPermission::Unchecked]
        {
            for f in all_failures() {
                let Some(m) = alert_message(f, "a reason", perm, Some((14, 2)), Some("~/Capture"))
                else {
                    continue;
                };
                assert!(!m.title.contains('—') && !m.body.contains('—'), "{f:?} / {perm:?}");
            }
        }
    }

    // ── The async-screenshot seam maps into the SHARED vocabulary ───────────

    #[test]
    fn a_saved_shot_is_not_a_failure() {
        assert_eq!(ShotOutcome::Saved.failure(), None);
    }

    #[test]
    fn the_three_former_false_cases_map_to_three_different_failures() {
        assert_eq!(ShotOutcome::NoImage.failure(), Some(Failure::NoImage));
        assert_eq!(ShotOutcome::SaveFailed.failure(), Some(Failure::SaveFailed));
        assert_eq!(ShotOutcome::WorkerDied.failure(), Some(Failure::WorkerPanic));
        // And they read differently to the user, which is the point.
        let titles: Vec<String> =
            [ShotOutcome::NoImage, ShotOutcome::SaveFailed, ShotOutcome::WorkerDied]
                .into_iter()
                .map(|o| msg(o.failure().unwrap()).title)
                .collect();
        assert_eq!(titles[0], "Nothing was captured.");
        assert_eq!(titles[1], "The capture couldn't be saved.");
        assert_eq!(titles[2], "The capture didn't complete.");
    }
}
