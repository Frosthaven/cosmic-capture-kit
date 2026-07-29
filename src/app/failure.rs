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
//! compiler catches. Presentation is a native AppKit `NSAlert` (`platform::mac::alert`),
//! decided on the ticket: the child that fails often has NO window yet (which is exactly
//! why it can die invisibly), so an iced window would mean standing up the renderer before
//! the scene grab, on every Mac, to serve a failure case. Linux and Windows present nothing
//! — their logs are reachable — so [`App::fail_session`] is byte-identically
//! `finish_session` there.
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
/// hang (a wedged `replayd`), which Apple fixed in 14.4. Pure over `(major, minor)` so the
/// predicate is testable; `None` (an unknown OS) is NOT affected — an unnamed build must
/// never be told to update to 14.4.
fn sonoma_content_hang(os: Option<(u32, u32)>) -> bool {
    matches!(os, Some((14, minor)) if minor <= 3)
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
            // not guessing: it is reporting the check we ran, plus the contradiction.
            body: if permission == ScreenPermission::Granted {
                "macOS reports that Screen Recording permission is granted, but returned no \
                 displays, so nothing was captured.\n\nRestarting your Mac usually clears \
                 this. If it keeps happening, remove Cosmic Capture Kit from System Settings \
                 → Privacy & Security → Screen Recording with the minus button, then relaunch \
                 to grant it again."
                    .into()
            } else {
                "macOS returned no displays, so nothing was captured. Restarting your Mac \
                 usually clears this."
                    .into()
            },
        },
        Failure::SceneGrabTimeout => AlertMessage {
            title: "Screen capture isn't responding.".into(),
            body: if sonoma_content_hang(os) {
                // Named ONLY on an affected build. A stall on 14.4 or later is a different
                // problem and must not be mislabelled as this one.
                "This is a known bug in macOS 14.0 to 14.3, and nothing was captured. \
                 Updating to macOS 14.4 or later fixes it. Restarting your Mac usually \
                 clears it until then."
                    .into()
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

impl App {
    /// Present this session's failure to the user WITHOUT ending the session — for the one
    /// caller that owns its own teardown (a preview document that could not open still has
    /// to go through `close_preview`, which ends the process only if it was the last one).
    ///
    /// Reads the ROOT failure this process recorded (`diag::root_failure`), so a symptom
    /// noted downstream cannot displace the diagnosis noted upstream: a ScreenCaptureKit
    /// call that never answered is what the user hears about, not the empty grab it caused.
    ///
    /// macOS only in effect. Linux and Windows do nothing here — a deliberate no-op, so
    /// their behaviour is byte-identical.
    pub(super) fn report_failure(&mut self) {
        let permission = screen_permission_now();
        #[cfg(target_os = "macos")]
        {
            // DRAGON-413 carve-out. The startup guard kills a child that never presents
            // anything, and its budget is SUSPENDED while a window the user must act on is
            // up — a child showing the permission checker is doing its job, and so is one
            // showing this. Published HERE rather than left to `startup_presence`, because
            // the modal below owns the run loop: `App::update` does not return until the
            // user dismisses it, so the guard would otherwise keep spending against a
            // presence published before the dialog existed.
            crate::startup_guard::report(crate::startup_guard::Presence::AwaitingUser);
            let Some((failure, detail)) = crate::diag::root_failure() else {
                // Nothing was recorded, which should not happen at a `fail_session` site —
                // but a session ending with nothing must still SAY so rather than fall back
                // to the silence this whole ticket exists to remove.
                let msg = unknown_failure();
                crate::platform::mac::alert::show(&msg.title, &msg.body);
                return;
            };
            let folder = self.capture_folder_display();
            let os = crate::platform::mac::os_version();
            if let Some(msg) =
                alert_message(failure, &detail, permission, Some(os), folder.as_deref())
            {
                crate::platform::mac::alert::show(&msg.title, &msg.body);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Linux and Windows present nothing: their warnings reach a terminal (Linux) or
            // the debug log (both), and adding a modal dialog to a path that already speaks
            // would be a behaviour change for no gain. `permission` is always `Unchecked`
            // here, which is the honest answer, not a placeholder.
            let _ = permission;
        }
    }

    /// Tell the user what happened, then end the one-shot session.
    ///
    /// THE seam for every non-delivery exit: the alert is shown app-modal, the user
    /// dismisses it, and only then does `finish_session` run — so the child exits cleanly
    /// through the normal path instead of dying invisibly. Off macOS this is exactly
    /// `finish_session`, unchanged.
    ///
    /// Call it AFTER the site's `diag::note_failure`, which is what it reads.
    pub(super) fn fail_session(&mut self) -> Task<cosmic::Action<Msg>> {
        self.report_failure();
        self.finish_session()
    }

    /// The capture folder as the USER would recognise it, for a save-failure message.
    ///
    /// Shown only in the alert, which is on the user's own screen. The debug log keeps
    /// using `diag::path_shape`, because that file gets emailed to us and a capture path
    /// can name the user's documents.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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

    // ── The 14.3 bug is named ONLY where it exists ──────────────────────────

    #[test]
    fn a_stall_on_an_affected_sonoma_names_the_bug_and_the_fix() {
        for minor in [0, 1, 2, 3] {
            let m = alert_message(
                Failure::SceneGrabTimeout,
                "",
                ScreenPermission::Granted,
                Some((14, minor)),
                None,
            )
            .unwrap();
            assert!(m.body.contains("known bug in macOS 14.0 to 14.3"), "14.{minor}");
            assert!(m.body.contains("14.4 or later fixes it"), "14.{minor}");
        }
    }

    #[test]
    fn a_stall_on_144_or_later_does_not_claim_the_143_bug() {
        for os in [Some((14, 4)), Some((14, 8)), Some((15, 0)), Some((26, 1)), None] {
            let m =
                alert_message(Failure::SceneGrabTimeout, "", ScreenPermission::Granted, os, None)
                    .unwrap();
            assert!(!m.body.contains("known bug"), "{os:?} must not claim the 14.3 bug");
            assert!(!m.body.contains("14.4"), "{os:?} must not advise updating to 14.4");
            assert!(m.body.contains("did not answer the screen capture request in time"));
        }
    }

    #[test]
    fn the_sonoma_predicate_is_exactly_14_0_through_14_3() {
        assert!(sonoma_content_hang(Some((14, 0))));
        assert!(sonoma_content_hang(Some((14, 3))));
        assert!(!sonoma_content_hang(Some((14, 4))));
        assert!(!sonoma_content_hang(Some((13, 3))));
        assert!(!sonoma_content_hang(Some((15, 3))));
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
