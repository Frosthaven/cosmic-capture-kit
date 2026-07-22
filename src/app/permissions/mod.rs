//! The macOS permission-checker window (DRAGON-130).
//!
//! A dedicated, small onboarding surface — the CleanShot / Rectangle-style
//! "grant these permissions" screen — that lists each permission the app needs as
//! a card: name, one-line why, a LIVE status pill (green granted / amber
//! not-determined / red denied), and a per-state action button (Request fires the
//! one-shot OS prompt while the status is not-determined; "Open System Settings"
//! deep-links when denied; Screen Recording adds a Relaunch button once granted,
//! because macOS only applies that grant to a FRESH launch).
//!
//! ## Why its own window (not a Settings page)
//!
//! The Health page already lists these grants with per-state Request / Open
//! Settings buttons ([`crate::app::settings::deps`]); this is the RICHER, dedicated
//! surface a first run / a missing-permission capture routes to, with the caveat
//! text and the Relaunch affordance the compact Health rows can't carry. It mirrors
//! the `--settings` window plumbing exactly (a `PermissionsState` like
//! `SettingsState`, `open_permissions_window` like `open_config_window`, a
//! `view_window` branch, a `sub_*` live poll), so the two never diverge.
//!
//! ## Live refresh
//!
//! While the window is open, `sub_permission_poll` re-probes the prompt-free
//! statuses (`screen_capture_granted` / `mic_status` / `notification_status`) once
//! a second — statuses flip green in place as the user grants them in System
//! Settings, which is the whole point of this screen.
//!
//! ## Pure state model
//!
//! [`PermStatus`] + [`card_action`] map a probe result to the card's action,
//! unit-tested in isolation (a pure-chooser pattern). Everything that
//! touches AppKit lives behind the `#[cfg(target_os = "macos")]` probes; on Linux
//! the module compiles to an empty window state that is never opened (there are no
//! TCC grants to check), keeping Linux byte-identical.

use super::*;

// The rich view (cards, probe task) is macOS-only — permissions are a macOS
// concept and the window is never minted elsewhere. A non-macOS stub keeps the
// `view_window` router branch total without pulling AppKit in.
#[cfg(target_os = "macos")]
mod view;

#[cfg(not(target_os = "macos"))]
impl App {
    /// Non-macOS stub: the permission window is never opened here (no TCC grants), so
    /// this only exists to keep `view_window`'s branch total. Renders empty.
    pub(in crate::app) fn permissions_window_view(&self) -> Element<'_, Msg> {
        widget::Space::new().width(Length::Fill).height(Length::Fill).into()
    }
}

/// The permission window title — also the handle used to find/focus an already-open
/// permissions window in another instance.
pub(crate) const WINDOW_TITLE: &str = "Cosmic Capture Kit: Permissions";

/// Open a permission-checker toplevel and a Task reporting its id once mapped.
/// Mirrors `settings::open_config_window`'s window recipe (CSD, transparent
/// surface, resize border) at a smaller onboarding size. A fixed, compact size —
/// nothing here scrolls at the default height, and it isn't a size worth persisting.
pub(super) fn open_permissions_window() -> (window::Id, Task<cosmic::Action<Msg>>) {
    const W: f32 = 520.0;
    const H: f32 = 560.0;
    let (id, task) = window::open(window::Settings {
        size: cosmic::iced::Size::new(W, H),
        min_size: Some(cosmic::iced::Size::new(460.0, 460.0)),
        resizable: true,
        resize_border: 8,
        // macOS (DRAGON-135): native decorations with a hidden/transparent titlebar —
        // the traffic lights render over our CSD header, which drops its own close
        // button there. Same recipe as `settings::open_config_window`.
        #[cfg(target_os = "macos")]
        decorations: true,
        #[cfg(not(target_os = "macos"))]
        decorations: false,
        // macOS (DRAGON-146): opaque for the native masked corner (see
        // `settings::open_config_window`).
        #[cfg(target_os = "macos")]
        transparent: false,
        #[cfg(not(target_os = "macos"))]
        transparent: true,
        exit_on_close_request: false,
        #[cfg(target_os = "linux")]
        platform_specific: cosmic::iced::window::settings::PlatformSpecific {
            application_id: "dev.frosthaven.CosmicCaptureKit".to_string(),
            ..Default::default()
        },
        #[cfg(target_os = "macos")]
        platform_specific: cosmic::iced::window::settings::PlatformSpecific {
            title_hidden: true,
            titlebar_transparent: true,
            fullsize_content_view: true,
        },
        #[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
        platform_specific: cosmic::iced::window::settings::PlatformSpecific::default(),
        ..Default::default()
    });
    (
        id,
        task.map(|id| {
            cosmic::Action::App(Msg::WindowChrome(WindowChromeMsg::PermissionsWindowOpened(id)))
        }),
    )
}

/// A snapshot of every card's LIVE status, produced by the prompt-free probes and
/// carried into the view (so `view` — which must never block — reads a plain value
/// rather than calling AppKit). Refreshed each poll tick + after every Request.
///
/// `notifications` is `Some` only when bundled (UN throws otherwise) AND the async
/// settings query answered; unbundled or on failure it stays `None`, which the view
/// reads as "hide the Notifications card".
// Fields are only read by the mac-gated probe/view paths; compiled (and
// type-checked) everywhere on purpose.
#[cfg_attr(not(target_os = "macos"), expect(dead_code))]
#[derive(Debug, Clone, Copy, Default)]
pub struct Probe {
    /// Screen Recording: preflight (granted / not-granted). Whether a not-granted
    /// state reads as NotDetermined vs Denied is decided by `request_spent`.
    pub screen_granted: bool,
    /// Whether the Screen Recording one-shot prompt is spent (`mac_first_run_seen`).
    pub screen_request_spent: bool,
    /// Microphone TCC status (honest three-state), reduced to our [`PermStatus`].
    pub microphone: Option<PermStatus>,
    /// Notifications status — `Some` only when bundled + the query answered.
    pub notifications: Option<PermStatus>,
    /// Accessibility (DRAGON-311): preflight (granted / not-granted). Like Screen
    /// Recording the preflight is boolean, so `accessibility_request_spent` decides
    /// whether a not-granted state reads as NotDetermined vs Denied. OPTIONAL.
    pub accessibility_granted: bool,
    /// Whether the Accessibility prompt is spent (`mac_accessibility_prompt_seen`).
    pub accessibility_request_spent: bool,
}

/// All UI state for the permission-checker window, grouped so `App` carries a single
/// field (mirrors `SettingsState`).
#[derive(Default)]
pub struct PermissionsState {
    /// The permissions toplevel window, when open (`None` while closed).
    pub window: Option<window::Id>,
    /// Launched via `--permissions` (or first-run routing): closing the window exits
    /// the app, exactly as `SettingsState::only` does for `--settings`.
    pub only: bool,
    /// The most recent live-status snapshot (seeded at open, refreshed each poll).
    #[cfg_attr(not(target_os = "macos"), expect(dead_code))]
    pub probe: Probe,
}

/// Run every prompt-free probe and assemble a [`Probe`] snapshot. macOS-only (the
/// grants don't exist elsewhere); the poll subscription + the window's off-view probe
/// task both call this, since `notification_status` may briefly block on an async
/// query. Includes the notification query — use [`probe_now_fast`] on the launch
/// critical path where that block must not run.
#[cfg(target_os = "macos")]
pub fn probe_now() -> Probe {
    probe_with(true)
}

/// A [`probe_now`] variant that SKIPS the notification query (DRAGON-201): that query
/// blocks up to 1.5s on `getNotificationSettingsWithCompletionHandler`, a cost every
/// capture launch used to pay synchronously before the overlay maps, purely to decide
/// routing. The routing decision only needs the required Screen Recording grant (plus
/// the honest, non-blocking mic status); a NotDetermined notification is a soft nag the
/// permission window re-probes live via its 1s poll, so it is dropped from the launch
/// decision. `notifications` stays `None` here (read as "hidden card / no reason to
/// open"), exactly as the unbundled / unanswered case already is.
#[cfg(target_os = "macos")]
pub fn probe_now_fast() -> Probe {
    probe_with(false)
}

/// Shared body for [`probe_now`] / [`probe_now_fast`]: `include_notifications` gates the
/// one potentially-blocking query (the notification settings fetch); everything else is
/// prompt-free. macOS-only.
#[cfg(target_os = "macos")]
fn probe_with(include_notifications: bool) -> Probe {
    use crate::platform::mac::{is_bundled, tcc};
    // Review aids (inert unless set), matching the Health page's CCK_HEALTH_FORCE_*
    // flags so the checker's amber/red states can be exercised without actually
    // revoking a grant: FORCE_DANGER forces the REQUIRED Screen Recording card
    // missing (red / Open Settings), FORCE_WARN forces the OPTIONAL cards
    // (Microphone / Notifications) missing (amber). `force_denied` reads a forced
    // card as Denied (Open Settings) rather than fabricating a NotDetermined.
    let force_danger = std::env::var_os("CCK_HEALTH_FORCE_DANGER").is_some();
    let force_warn = std::env::var_os("CCK_HEALTH_FORCE_WARN").is_some();

    // FORCE_WARN drives the mic card to NotDetermined (amber / Request) and the
    // notification card to Denied (amber / Open Settings), so ONE flag exercises
    // both optional-state buttons for review.
    let microphone = if force_warn {
        Some(PermStatus::NotDetermined)
    } else {
        Some(tcc_to_perm(tcc::mic_status()))
    };
    // Notifications: bundle-gated (UN throws unbundled) — None ⇒ the card is hidden.
    // The FORCE_WARN review flag surfaces the card (as Denied) even unbundled so its
    // amber state is reviewable on a dev binary. When `include_notifications` is false
    // (DRAGON-201, the launch-routing probe) the up-to-1.5s query is skipped entirely
    // and this stays None — the window's live poll fills the card in shortly after open.
    let notifications = if force_warn {
        Some(PermStatus::Denied)
    } else if include_notifications && is_bundled() {
        tcc::notification_status().map(tcc_to_perm)
    } else {
        None
    };
    Probe {
        // FORCE_DANGER forces the screen grant missing + prompt spent ⇒ Denied.
        screen_granted: !force_danger && tcc::screen_capture_granted(),
        screen_request_spent: force_danger || crate::state::load().mac_first_run_seen,
        microphone,
        notifications,
        // Accessibility is OPTIONAL, so FORCE_WARN (not FORCE_DANGER) drives it missing
        // + prompt spent ⇒ amber Denied (Open Settings), matching how FORCE_WARN
        // exercises the other optional cards' remediation button.
        accessibility_granted: !force_warn && tcc::accessibility_granted(),
        accessibility_request_spent: force_warn || crate::state::load().mac_accessibility_prompt_seen,
    }
}

/// The Accessibility card's status from a [`Probe`]. Mirrors [`screen_status`]:
/// granted ⇒ Granted; else the boolean preflight can't tell NotDetermined from Denied,
/// so the spent flag decides (unspent ⇒ NotDetermined / offer Request, spent ⇒ Denied
/// / offer Open Settings). OPTIONAL, so the view colours a missing state amber, not red.
#[cfg_attr(not(target_os = "macos"), expect(dead_code))]
pub fn accessibility_status(probe: &Probe) -> PermStatus {
    if probe.accessibility_granted {
        PermStatus::Granted
    } else if probe.accessibility_request_spent {
        PermStatus::Denied
    } else {
        PermStatus::NotDetermined
    }
}

/// Reduce a [`crate::platform::mac::tcc::TccStatus`] to our [`PermStatus`] (the same
/// three cases, decoupled so the permissions module needn't expose the tcc type in
/// its public API / tests).
#[cfg(target_os = "macos")]
fn tcc_to_perm(s: crate::platform::mac::tcc::TccStatus) -> PermStatus {
    use crate::platform::mac::tcc::TccStatus;
    match s {
        TccStatus::Granted => PermStatus::Granted,
        TccStatus::Denied => PermStatus::Denied,
        TccStatus::NotDetermined => PermStatus::NotDetermined,
    }
}

/// The Screen Recording card's status from a [`Probe`]. Granted ⇒ Granted; else the
/// preflight can't tell NotDetermined from Denied, so the spent flag decides: unspent
/// prompt ⇒ NotDetermined (offer Request), spent ⇒ Denied (offer Open Settings).
#[cfg_attr(not(target_os = "macos"), expect(dead_code))]
pub fn screen_status(probe: &Probe) -> PermStatus {
    if probe.screen_granted {
        PermStatus::Granted
    } else if probe.screen_request_spent {
        PermStatus::Denied
    } else {
        PermStatus::NotDetermined
    }
}

/// Whether the permission-checker window should AUTO-OPEN on a capture launch or at
/// daemon startup. True when EITHER the required Screen Recording grant is missing
/// (`!screen_granted` — regardless of NotDetermined vs Denied, since it is required
/// and a missing grant only yields blank captures), OR an OPTIONAL permission is
/// still NotDetermined (never prompted): microphone NotDetermined, notifications
/// NotDetermined when the notification card is present (`Some`; `None` = unbundled /
/// query unanswered = hidden card, never a reason to open), or Accessibility
/// NotDetermined (never prompted — worth one prompt so "Capture Active Window" can
/// target the focused window).
///
/// Explicitly DENIED optional permissions (mic / notifications / accessibility)
/// deliberately do NOT trigger this — a user who declined one must not be nagged.
/// Only the required Screen Recording keeps forcing the window once every optional
/// permission has been addressed (granted or denied).
///
/// Semantics of a NotDetermined trigger: acting on the card fires the OS prompt,
/// which flips the status to Granted or Denied, so the window stops auto-opening once
/// everything has been addressed. Dismissing the window WITHOUT acting leaves the
/// status NotDetermined, so it may reopen on the next capture / daemon start — that
/// is the chosen semantic (an unaddressed never-prompted permission is worth one more
/// look, a declined one is not).
///
/// Pure over the reduced statuses so the full truth table is unit-testable; callers
/// pass values assembled by [`probe_now`] (see [`should_auto_open_probe`]).
// `not(test)`: the truth-table tests below exercise this on every platform.
#[cfg_attr(all(not(target_os = "macos"), not(test)), expect(dead_code))]
pub fn should_auto_open(
    screen_granted: bool,
    mic: PermStatus,
    notifications: Option<PermStatus>,
    accessibility: PermStatus,
) -> bool {
    !screen_granted
        || mic == PermStatus::NotDetermined
        || notifications == Some(PermStatus::NotDetermined)
        || accessibility == PermStatus::NotDetermined
}

/// [`should_auto_open`] over a live [`Probe`] snapshot — the form both auto-open sites
/// call. macOS-only (the `Probe` only carries meaningful grants there). The mic status
/// falls back to NotDetermined if the probe couldn't read it (absent constant), which
/// conservatively offers the window rather than hiding a possibly-unaddressed grant.
#[cfg(target_os = "macos")]
pub fn should_auto_open_probe(probe: &Probe) -> bool {
    should_auto_open(
        probe.screen_granted,
        probe.microphone.unwrap_or(PermStatus::NotDetermined),
        probe.notifications,
        accessibility_status(probe),
    )
}

/// A permission's reduced status — the three cases every card renders. Screen
/// Recording is preflight-only (it can't tell NotDetermined from Denied), so the
/// `mac_first_run_seen` flag decides which of those two a missing screen grant is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermStatus {
    Granted,
    Denied,
    NotDetermined,
}

/// The action a card offers, chosen purely from status (so it is unit-testable);
/// the view maps each to a concrete button + message.
// The permission `view` is macOS-only (`mod view` is `cfg(macos)`), so on Linux this
// is exercised only by the card-action tests below — dead in a non-test Linux build.
#[cfg_attr(all(not(target_os = "macos"), not(test)), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardAction {
    /// The one-shot OS prompt is still available — offer "Request".
    Request,
    /// The prompt is spent / access denied — deep-link to System Settings.
    OpenSettings,
    /// Granted and nothing more to do — no button (just the green pill).
    None,
}

/// The per-card remediation, purely from status. `request_spent` matters only for
/// Screen Recording: its preflight can't distinguish NotDetermined from Denied, so
/// once the one-shot prompt is spent (`mac_first_run_seen`) a missing grant can only
/// be recovered in System Settings. For the mic/notification cards the status is
/// honest, so pass `request_spent = false` (their NotDetermined truly offers a
/// prompt).
// Dead in a non-test Linux build (the `view` that calls it is macOS-only); the
// card-action tests below exercise it on every platform.
#[cfg_attr(all(not(target_os = "macos"), not(test)), allow(dead_code))]
pub fn card_action(status: PermStatus, request_spent: bool) -> CardAction {
    match status {
        PermStatus::Granted => CardAction::None,
        PermStatus::Denied => CardAction::OpenSettings,
        PermStatus::NotDetermined => {
            if request_spent {
                CardAction::OpenSettings
            } else {
                CardAction::Request
            }
        }
    }
}

/// Which permission a card represents — the stable id the view + update route on.
#[cfg_attr(not(target_os = "macos"), expect(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    ScreenRecording,
    Microphone,
    Notifications,
    /// Accessibility (DRAGON-311, OPTIONAL): lets "Capture Active Window" / "Capture
    /// Active Monitor" resolve the FOCUSED window (and capture it in its active
    /// appearance) via the AX API. Absent, capture degrades gracefully to a z-order
    /// guess, so its lack never routes/forces the window like Screen Recording does.
    Accessibility,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn granted_offers_no_action() {
        assert_eq!(card_action(PermStatus::Granted, false), CardAction::None);
        assert_eq!(card_action(PermStatus::Granted, true), CardAction::None);
    }

    #[test]
    fn denied_opens_settings() {
        assert_eq!(card_action(PermStatus::Denied, false), CardAction::OpenSettings);
        assert_eq!(card_action(PermStatus::Denied, true), CardAction::OpenSettings);
    }

    #[test]
    fn not_determined_with_unspent_prompt_offers_request() {
        assert_eq!(
            card_action(PermStatus::NotDetermined, false),
            CardAction::Request
        );
    }

    #[test]
    fn auto_open_when_screen_missing_regardless_of_optionals() {
        // Screen missing is required → open, whatever the optional cards say (even all
        // granted, even all denied). Accessibility addressed (Granted) so only the
        // screen grant drives these.
        assert!(should_auto_open(false, PermStatus::Granted, Some(PermStatus::Granted), PermStatus::Granted));
        assert!(should_auto_open(false, PermStatus::Denied, Some(PermStatus::Denied), PermStatus::Denied));
        assert!(should_auto_open(false, PermStatus::NotDetermined, None, PermStatus::Granted));
    }

    #[test]
    fn auto_open_when_an_optional_is_not_determined() {
        // Screen granted but a never-prompted optional → open (offer the prompt once).
        assert!(should_auto_open(true, PermStatus::NotDetermined, Some(PermStatus::Granted), PermStatus::Granted));
        assert!(should_auto_open(true, PermStatus::Granted, Some(PermStatus::NotDetermined), PermStatus::Granted));
        assert!(should_auto_open(true, PermStatus::NotDetermined, None, PermStatus::Granted));
        // Accessibility never-prompted alone (mic/notif addressed) → open.
        assert!(should_auto_open(true, PermStatus::Granted, Some(PermStatus::Granted), PermStatus::NotDetermined));
    }

    #[test]
    fn no_auto_open_when_everything_addressed() {
        // Screen granted and every optional addressed (granted OR denied) → stay shut.
        assert!(!should_auto_open(true, PermStatus::Granted, Some(PermStatus::Granted), PermStatus::Granted));
        assert!(!should_auto_open(true, PermStatus::Granted, None, PermStatus::Granted));
    }

    #[test]
    fn denied_optionals_do_not_nag() {
        // The key decision: a user who DECLINED mic / notifications / accessibility is
        // not re-nagged while Screen Recording is granted.
        assert!(!should_auto_open(true, PermStatus::Denied, Some(PermStatus::Denied), PermStatus::Denied));
        assert!(!should_auto_open(true, PermStatus::Denied, Some(PermStatus::Granted), PermStatus::Granted));
        assert!(!should_auto_open(true, PermStatus::Granted, Some(PermStatus::Denied), PermStatus::Granted));
        assert!(!should_auto_open(true, PermStatus::Denied, None, PermStatus::Granted));
        // Declined accessibility alone is not a reason to nag.
        assert!(!should_auto_open(true, PermStatus::Granted, Some(PermStatus::Granted), PermStatus::Denied));
    }

    #[test]
    fn accessibility_not_determined_opens_once() {
        // Never-prompted Accessibility (everything else addressed) → open to offer the
        // one prompt; declined does not, mirroring the other optionals.
        assert!(should_auto_open(true, PermStatus::Granted, None, PermStatus::NotDetermined));
        assert!(!should_auto_open(true, PermStatus::Granted, None, PermStatus::Denied));
        assert!(!should_auto_open(true, PermStatus::Granted, None, PermStatus::Granted));
    }

    #[test]
    fn hidden_notification_card_is_never_a_reason_to_open() {
        // Notifications None (unbundled / unanswered) must not trigger the window on
        // its own — only Some(NotDetermined) does.
        assert!(!should_auto_open(true, PermStatus::Granted, None, PermStatus::Granted));
    }

    // DRAGON-201: the launch-routing probe skips the (blocking) notification query, so
    // it always passes `notifications = None`. Screen Recording missing must STILL route
    // to the window, and a never-prompted mic must too — the required grant and the
    // honest mic status are all routing needs. A notification NotDetermined, invisible
    // to this probe, simply doesn't gate launch (the window's live poll surfaces it).
    #[test]
    fn routing_without_notifications_still_opens_on_missing_screen_recording() {
        // Screen Recording missing → open, regardless of the (absent) notification status.
        assert!(should_auto_open(false, PermStatus::Granted, None, PermStatus::Granted));
        assert!(should_auto_open(false, PermStatus::Denied, None, PermStatus::Granted));
        assert!(should_auto_open(false, PermStatus::NotDetermined, None, PermStatus::Granted));
    }

    #[test]
    fn routing_without_notifications_opens_on_never_prompted_mic() {
        // Screen granted, mic never prompted → still worth one prompt.
        assert!(should_auto_open(true, PermStatus::NotDetermined, None, PermStatus::Granted));
    }

    #[test]
    fn routing_without_notifications_stays_shut_when_screen_and_mic_addressed() {
        // Screen granted + mic addressed (granted or denied), notifications absent from
        // the fast probe, accessibility addressed → launch is NOT gated on a
        // possibly-NotDetermined notification.
        assert!(!should_auto_open(true, PermStatus::Granted, None, PermStatus::Granted));
        assert!(!should_auto_open(true, PermStatus::Denied, None, PermStatus::Granted));
    }

    #[test]
    fn not_determined_with_spent_prompt_opens_settings() {
        // Screen Recording after its one-shot prompt is spent: a re-request just
        // returns the standing decision, so System Settings is the honest action.
        assert_eq!(
            card_action(PermStatus::NotDetermined, true),
            CardAction::OpenSettings
        );
    }
}
