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
pub(crate) const WINDOW_TITLE: &str = "Cosmic Capture Kit - Permissions";

/// Open a permission-checker toplevel and a Task reporting its id once mapped.
/// Mirrors `settings::open_config_window`'s window recipe (CSD, transparent
/// surface, resize border) at a smaller onboarding size.
///
/// **The window is exactly 629x707 and cannot be resized** (DRAGON-587, the owner's
/// instruction, and the same treatment the colour-picker result window gets).
///
/// Two things had to be checked before taking the resize away, because this window exists to
/// tell the user what is WRONG and clipping that would be worse than letting it grow:
///
/// * **Can the content be taller than 707?** Yes, in the fullest state (all four cards, with
///   Screen Recording granted adding its Relaunch button and Accessibility granted adding
///   Restart Background Helper, plus the skip footer). It cannot CLIP, though: the whole card
///   column lives inside a `widget::scrollable` (`view::permissions_window_view`), so a tall
///   state scrolls, exactly as it already did at this size before the resize went away.
/// * **Does 707 still fit?** Yes on every supported Mac at its default scaling: the smallest
///   is the 13" MacBook Air at 1440x900 points, leaving roughly 90pt spare under the menu bar
///   and over the Dock. It is only tight on a NON-default "Larger Text" scaled mode
///   (1280x800 leaves about 705) and does not fit at 1024x640. That is not a regression: 707
///   has been the min_size FLOOR since DRAGON-533, so those displays could never shrink this
///   window anyway. What the pin removes is only the ability to make it BIGGER.
///
/// The lock is `min_size == max_size` on every platform, which is what actually pins it.
/// `resizable` STAYS TRUE, and must: on macOS clearing it drops `NSWindowStyleMask::Resizable`,
/// and winit's `is_zoomed` reacts to a mask missing `Titled | Resizable` by rewriting the mask
/// mid-reframe, which is the DRAGON-130 abort. `platform::mac::window::pin_window_size`
/// (dispatched from `PermissionsWindowOpened`) covers what the size clamp cannot: the zoom
/// button and full-screen spaces. Its own doc carries the whole argument.
pub(super) fn open_permissions_window() -> (window::Id, Task<cosmic::Action<Msg>>) {
    // DRAGON-533: the old 520x560 read as too short (some cards' content clipped/scrolled
    // more than felt right). Owner resized their live window to a comfortable 629x707 and
    // asked for that to become the new default AND the floor.
    const W: f32 = 629.0;
    const H: f32 = 707.0;
    // Frosted glass (DRAGON-217/533): this window's own doc claims to mirror
    // `settings::open_config_window`'s recipe, but was missing the one field that
    // actually enrolls it in the compositor's backdrop blur. `MacCenterTitlebar`
    // (dispatched below, in `PermissionsWindowOpened`) already calls
    // `enable_window_vibrancy` for this window's title exactly like it does for
    // Settings; without `blur: true` at creation there is nothing for that call to
    // reveal, so the window opened opaque with no visible vibrancy regardless. See
    // `settings::open_config_window`'s own comment for the Windows Mica caveat this
    // mirrors too.
    #[cfg(not(windows))]
    let blur = crate::app::theme::glass_windows_enabled();
    #[cfg(windows)]
    let blur = false;
    let (id, task) = window::open(window::Settings {
        size: cosmic::iced::Size::new(W, H),
        blur,
        // The opening size IS the floor (owner report: the window read as too short at
        // the old 460x460 minimum). Was smaller than the default, so shrinking it ever
        // clipped content the default size was chosen to fit. DRAGON-587 made it the
        // CEILING too: equal floor and ceiling, so the size IS the window.
        min_size: Some(cosmic::iced::Size::new(W, H)),
        max_size: Some(cosmic::iced::Size::new(W, H)),
        // Stays TRUE on macOS on purpose: clearing it hands winit's `is_zoomed` a style mask
        // to rewrite mid-reframe (the DRAGON-130 abort). The equal min/max above are what
        // make it unresizable, and `pin_window_size` takes the zoom button. See the doc.
        resizable: true,
        // No drag-resize band to reserve, since there is no resize left to start.
        resize_border: 0,
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
            application_id: "dev.thedragon.CosmicCaptureKit".to_string(),
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
    /// whether a not-granted state reads as NotDetermined vs Denied. RECOMMENDED
    /// (DRAGON-412).
    pub accessibility_granted: bool,
    /// Whether the Accessibility prompt is spent (`mac_accessibility_prompt_seen`).
    pub accessibility_request_spent: bool,
    /// DRAGON-412: whether the RECOMMENDED / OPTIONAL nag is spent
    /// (`mac_permission_nag_spent`) — the window has been presented once and left
    /// without addressing those grants. Once true, no non-required permission can
    /// force the window open again; only the required Screen Recording grant still
    /// can. Set by Skip and by simply closing the window.
    pub nag_spent: bool,
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
        // Accessibility is RECOMMENDED, so FORCE_WARN (not FORCE_DANGER) drives it
        // missing + prompt spent ⇒ amber Denied (Open Settings), matching how FORCE_WARN
        // exercises the other non-required cards' remediation button.
        accessibility_granted: !force_warn && tcc::accessibility_granted(),
        accessibility_request_spent: force_warn || crate::state::load().mac_accessibility_prompt_seen,
        // DRAGON-412: read straight off the store — the review flags exercise CARD
        // states, and this one only gates AUTO-open, which they don't drive.
        nag_spent: crate::state::load().mac_permission_nag_spent,
    }
}

/// Mark the RECOMMENDED / OPTIONAL nag spent (DRAGON-412): the user has now seen the
/// permission window once and left it without addressing those grants, so it must
/// never auto-open for them again. Called from BOTH escape routes — the explicit
/// "Continue Without These" button and a plain window close — so dismissing and
/// skipping are one behaviour, and only one of them needs a label.
///
/// Deliberately does NOT touch `mac_accessibility_prompt_seen`: that flag records that
/// the OS prompt itself is spent (which is what turns the card's Request button into
/// Open System Settings, and what makes a missing grant read as Denied). A dismissal
/// is not a denial, and must not be rendered as one.
///
/// A no-op off macOS (no TCC grants, the window is never minted); kept un-cfg'd so the
/// shared close path type-checks everywhere.
pub fn spend_nag() {
    let mut p = crate::state::load();
    if !p.mac_permission_nag_spent {
        p.mac_permission_nag_spent = true;
        crate::state::save(&p);
    }
}

/// The Accessibility card's status from a [`Probe`]. Mirrors [`screen_status`]:
/// granted ⇒ Granted; else the boolean preflight can't tell NotDetermined from Denied,
/// so the spent flag decides (unspent ⇒ NotDetermined / offer Request, spent ⇒ Denied
/// / offer Open Settings). RECOMMENDED, so the view colours a missing state amber, not
/// red. Note the spent flag here is `mac_accessibility_prompt_seen` — the OS prompt —
/// NOT the DRAGON-412 dismissal flag, which must never make an unanswered grant read
/// as Denied.
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
/// daemon startup. Two tiers decide it, and they are not symmetric ([`Tier`]):
///
/// * **Required** ([`Permission::ScreenRecording`]): a missing grant ALWAYS forces the
///   window, regardless of NotDetermined vs Denied and regardless of `nag_spent` —
///   capture without it only yields blank frames, so there is nothing to continue to.
///   This arm is deliberately un-weakened by everything below.
/// * **Recommended / Optional** (accessibility, microphone, notifications): each may
///   force the window ONCE, while still NotDetermined (never prompted). `nag_spent`
///   ends that once, for all of them together.
///
/// `nag_spent` is the DRAGON-412 fix. Before it, "once" was terminated only by the OS
/// prompt firing (which flips the status to Granted/Denied). Dismissing the window
/// without acting left every status NotDetermined, so the window came back on the next
/// capture and the next daemon start, forever — the only escape was to trigger the
/// prompt and explicitly deny. A customer read that as "the tool just wouldn't open"
/// and stacked up six retrying processes. Closing the window IS an answer, and this
/// flag is the record of it, so the escape terminates.
///
/// Explicitly DENIED non-required permissions still do not trigger this either — a
/// user who declined one must not be nagged. Once every non-required permission is
/// addressed (granted, denied, or skipped) only the required Screen Recording grant
/// can still force the window.
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
    nag_spent: bool,
) -> bool {
    // Required: nothing below can weaken this, and nothing below runs when it fires.
    if !screen_granted {
        return true;
    }
    // Recommended / Optional: one presentation each, ended for all of them at once by
    // a Skip or a dismissal. Structurally an early return rather than an `&&` per arm,
    // so a future non-required permission cannot reintroduce an unbounded nag by
    // forgetting to consult the flag.
    if nag_spent {
        return false;
    }
    mic == PermStatus::NotDetermined
        // `None` = unbundled / query unanswered = hidden card, never a reason to open.
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
        probe.nag_spent,
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

/// How badly a permission is wanted (DRAGON-412) — the ONE source of both the word
/// the card prints and the auto-open policy [`should_auto_open`] applies, so the copy
/// and the behaviour can't drift apart (they had: `Permission::Accessibility`'s doc
/// claimed it never forced the window while the predicate forced it forever).
///
/// * [`Tier::Required`] — capture is broken without it. Forces the window until granted.
/// * [`Tier::Recommended`] — capture works, but degrades in a way worth one explanation.
///   Presents itself ONCE; a grant, a denial, OR a dismissal ends that for good.
/// * [`Tier::Optional`] — a side feature. Same once-only presentation as Recommended;
///   the tiers differ only in the word on the card and in how strongly it is urged.
// `allow`, not `expect`, and the difference is not style (DRAGON-625). `Tier` is
// reachable only from `Permission::tier`, which is itself dead off macOS, and rustc
// only learned to propagate deadness through a chain of dead items after 1.95. The
// Flatpak SDK ships rustc 1.95, where the `dead_code` lint therefore does NOT fire
// here, so an `expect` is unfulfilled and warns; the dev box's newer rustc fulfils it
// and says nothing. `allow` is correct on every toolchain, and it is what the other
// ~276 dead-code gates in this tree already use. Do not "tighten" this back to
// `expect` without building on the SDK's rustc first.
#[cfg_attr(all(not(target_os = "macos"), not(test)), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Required,
    Recommended,
    Optional,
}

impl Tier {
    /// The word the card leads with. "Recommended" is the DRAGON-412 ask: Accessibility
    /// used to print "Optional", which undersold it while the code nagged about it
    /// hardest.
    #[cfg_attr(all(not(target_os = "macos"), not(test)), expect(dead_code))]
    pub fn label(self) -> &'static str {
        match self {
            Tier::Required => "Required",
            Tier::Recommended => "Recommended",
            Tier::Optional => "Optional",
        }
    }

    /// Whether a missing grant in this tier is an error (red pill) or a soft state
    /// (amber). Only Required is red — the others are choices, not faults.
    #[cfg_attr(all(not(target_os = "macos"), not(test)), expect(dead_code))]
    pub fn is_required(self) -> bool {
        matches!(self, Tier::Required)
    }
}

/// Which permission a card represents — the stable id the view + update route on.
// `not(test)`: the tier tests below construct these variants on every platform.
#[cfg_attr(all(not(target_os = "macos"), not(test)), expect(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    ScreenRecording,
    Microphone,
    Notifications,
    /// Accessibility (DRAGON-311, RECOMMENDED as of DRAGON-412): lets "Capture Active
    /// Window" / "Capture Active Monitor" resolve the FOCUSED window (and capture it in
    /// its active appearance) via the AX API. Absent, capture degrades gracefully to a
    /// z-order guess, which is usually right but can pick the wrong window.
    ///
    /// It therefore forces the permission window AT MOST ONCE (while never prompted),
    /// unlike the required Screen Recording grant which forces it until granted — and
    /// unlike the pre-DRAGON-412 behaviour, where "at most once" was a comment here
    /// that the predicate did not implement.
    Accessibility,
}

impl Permission {
    /// This permission's [`Tier`]. See the module tests for the tier's contract.
    #[cfg_attr(all(not(target_os = "macos"), not(test)), expect(dead_code))]
    pub fn tier(self) -> Tier {
        match self {
            Permission::ScreenRecording => Tier::Required,
            // DRAGON-412: promoted from Optional. It is the one non-required grant that
            // changes what a CAPTURE produces (which window you get), rather than adding
            // a side feature, so it is urged more strongly than mic/notifications — but
            // it is still a choice, and skipping it is honoured permanently.
            Permission::Accessibility => Tier::Recommended,
            // Microphone / Notifications stay OPTIONAL: each gates a separable feature
            // (voice-over on a recording; a save banner), not the quality of a capture,
            // and their existing copy is accurate. They DO share the Recommended tier's
            // once-only presentation — the unbounded-nag defect was never specific to
            // Accessibility, and fixing one tier while leaving these two able to re-force
            // the window on every launch would leave the reported symptom intact.
            Permission::Microphone | Permission::Notifications => Tier::Optional,
        }
    }
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
        assert!(should_auto_open(false, PermStatus::Granted, Some(PermStatus::Granted), PermStatus::Granted, false));
        assert!(should_auto_open(false, PermStatus::Denied, Some(PermStatus::Denied), PermStatus::Denied, false));
        assert!(should_auto_open(false, PermStatus::NotDetermined, None, PermStatus::Granted, false));
    }

    #[test]
    fn auto_open_when_an_optional_is_not_determined() {
        // Screen granted but a never-prompted optional → open (offer the prompt once).
        assert!(should_auto_open(true, PermStatus::NotDetermined, Some(PermStatus::Granted), PermStatus::Granted, false));
        assert!(should_auto_open(true, PermStatus::Granted, Some(PermStatus::NotDetermined), PermStatus::Granted, false));
        assert!(should_auto_open(true, PermStatus::NotDetermined, None, PermStatus::Granted, false));
        // Accessibility never-prompted alone (mic/notif addressed) → open.
        assert!(should_auto_open(true, PermStatus::Granted, Some(PermStatus::Granted), PermStatus::NotDetermined, false));
    }

    #[test]
    fn no_auto_open_when_everything_addressed() {
        // Screen granted and every optional addressed (granted OR denied) → stay shut.
        assert!(!should_auto_open(true, PermStatus::Granted, Some(PermStatus::Granted), PermStatus::Granted, false));
        assert!(!should_auto_open(true, PermStatus::Granted, None, PermStatus::Granted, false));
    }

    #[test]
    fn denied_optionals_do_not_nag() {
        // The key decision: a user who DECLINED mic / notifications / accessibility is
        // not re-nagged while Screen Recording is granted.
        assert!(!should_auto_open(true, PermStatus::Denied, Some(PermStatus::Denied), PermStatus::Denied, false));
        assert!(!should_auto_open(true, PermStatus::Denied, Some(PermStatus::Granted), PermStatus::Granted, false));
        assert!(!should_auto_open(true, PermStatus::Granted, Some(PermStatus::Denied), PermStatus::Granted, false));
        assert!(!should_auto_open(true, PermStatus::Denied, None, PermStatus::Granted, false));
        // Declined accessibility alone is not a reason to nag.
        assert!(!should_auto_open(true, PermStatus::Granted, Some(PermStatus::Granted), PermStatus::Denied, false));
    }

    #[test]
    fn accessibility_not_determined_opens_once() {
        // Never-prompted Accessibility (everything else addressed) → open to offer the
        // one prompt; declined does not, mirroring the other optionals.
        assert!(should_auto_open(true, PermStatus::Granted, None, PermStatus::NotDetermined, false));
        assert!(!should_auto_open(true, PermStatus::Granted, None, PermStatus::Denied, false));
        assert!(!should_auto_open(true, PermStatus::Granted, None, PermStatus::Granted, false));
    }

    #[test]
    fn hidden_notification_card_is_never_a_reason_to_open() {
        // Notifications None (unbundled / unanswered) must not trigger the window on
        // its own — only Some(NotDetermined) does.
        assert!(!should_auto_open(true, PermStatus::Granted, None, PermStatus::Granted, false));
    }

    // DRAGON-201: the launch-routing probe skips the (blocking) notification query, so
    // it always passes `notifications = None`. Screen Recording missing must STILL route
    // to the window, and a never-prompted mic must too — the required grant and the
    // honest mic status are all routing needs. A notification NotDetermined, invisible
    // to this probe, simply doesn't gate launch (the window's live poll surfaces it).
    #[test]
    fn routing_without_notifications_still_opens_on_missing_screen_recording() {
        // Screen Recording missing → open, regardless of the (absent) notification status.
        assert!(should_auto_open(false, PermStatus::Granted, None, PermStatus::Granted, false));
        assert!(should_auto_open(false, PermStatus::Denied, None, PermStatus::Granted, false));
        assert!(should_auto_open(false, PermStatus::NotDetermined, None, PermStatus::Granted, false));
    }

    #[test]
    fn routing_without_notifications_opens_on_never_prompted_mic() {
        // Screen granted, mic never prompted → still worth one prompt.
        assert!(should_auto_open(true, PermStatus::NotDetermined, None, PermStatus::Granted, false));
    }

    #[test]
    fn routing_without_notifications_stays_shut_when_screen_and_mic_addressed() {
        // Screen granted + mic addressed (granted or denied), notifications absent from
        // the fast probe, accessibility addressed → launch is NOT gated on a
        // possibly-NotDetermined notification.
        assert!(!should_auto_open(true, PermStatus::Granted, None, PermStatus::Granted, false));
        assert!(!should_auto_open(true, PermStatus::Denied, None, PermStatus::Granted, false));
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

    // ---- DRAGON-412: the tier contract -------------------------------------------
    //
    // The failure mode being fixed is UNBOUNDED REPETITION, which no single-shot
    // assertion catches: the old predicate was correct on any one call and wrong
    // because the call kept coming back. So these walk the WHOLE truth table rather
    // than sampling it.

    /// Every reduced status, for exhaustive sweeps.
    const ALL: [PermStatus; 3] =
        [PermStatus::Granted, PermStatus::Denied, PermStatus::NotDetermined];

    /// Every notification-card state, including the hidden card.
    fn all_notifications() -> [Option<PermStatus>; 4] {
        [
            None,
            Some(PermStatus::Granted),
            Some(PermStatus::Denied),
            Some(PermStatus::NotDetermined),
        ]
    }

    #[test]
    fn spent_nag_can_never_force_the_window_in_any_combination() {
        // THE regression net. Once the recommended / optional nag is spent (the user
        // granted, denied, or DISMISSED), no combination of their statuses may force the
        // window while the required grant is present — 3 (mic) x 4 (notifications) x 3
        // (accessibility) = 36 cases, all false.
        for &mic in &ALL {
            for notif in all_notifications() {
                for &ax in &ALL {
                    assert!(
                        !should_auto_open(true, mic, notif, ax, true),
                        "spent nag forced the window: mic={mic:?} notif={notif:?} ax={ax:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn spending_the_nag_is_terminal_and_idempotent() {
        // The customer's loop, played out: a never-prompted Accessibility opens the
        // window (once), the user closes it (which spends the nag), and every subsequent
        // launch must decide NOT to open — forever, not just on the next one. Repetition
        // is the whole bug, so assert over repeated evaluations.
        let (screen, mic, notif, ax) =
            (true, PermStatus::Granted, None, PermStatus::NotDetermined);
        assert!(
            should_auto_open(screen, mic, notif, ax, false),
            "the first look must still happen"
        );
        for launch in 0..100 {
            assert!(
                !should_auto_open(screen, mic, notif, ax, true),
                "launch {launch} re-opened the window after dismissal"
            );
        }
    }

    #[test]
    fn required_screen_recording_still_forces_the_window_even_with_the_nag_spent() {
        // The guard against fixing the recommended tier by weakening the required one:
        // a missing Screen Recording grant forces the window in EVERY combination,
        // including nag_spent — 2 (spent) x 3 x 4 x 3 = 72 cases, all true.
        for &nag_spent in &[false, true] {
            for &mic in &ALL {
                for notif in all_notifications() {
                    for &ax in &ALL {
                        assert!(
                            should_auto_open(false, mic, notif, ax, nag_spent),
                            "missing Screen Recording did not force the window: \
                             nag_spent={nag_spent} mic={mic:?} notif={notif:?} ax={ax:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn an_unspent_nag_still_offers_the_one_look() {
        // The tier is "once", not "never": while unspent, a never-prompted recommended /
        // optional grant must still open the window exactly as before, so this fix can't
        // be mistaken for silently dropping the onboarding.
        for &mic in &ALL {
            for notif in all_notifications() {
                for &ax in &ALL {
                    let any_unprompted = mic == PermStatus::NotDetermined
                        || notif == Some(PermStatus::NotDetermined)
                        || ax == PermStatus::NotDetermined;
                    assert_eq!(
                        should_auto_open(true, mic, notif, ax, false),
                        any_unprompted,
                        "unspent nag mis-decided: mic={mic:?} notif={notif:?} ax={ax:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn tiers_match_the_documented_policy() {
        // The contradiction DRAGON-412 also fixes: the tier a card PRINTS is the tier the
        // predicate applies, because both read `Permission::tier`.
        assert_eq!(Permission::ScreenRecording.tier(), Tier::Required);
        assert_eq!(Permission::Accessibility.tier(), Tier::Recommended);
        assert_eq!(Permission::Microphone.tier(), Tier::Optional);
        assert_eq!(Permission::Notifications.tier(), Tier::Optional);
        // Accessibility must read "Recommended", never "Optional" — the ticket's ask.
        assert_eq!(Permission::Accessibility.tier().label(), "Recommended");
        assert_eq!(Permission::ScreenRecording.tier().label(), "Required");
        assert_eq!(Permission::Microphone.tier().label(), "Optional");
        // Only Required paints a missing grant red.
        assert!(Tier::Required.is_required());
        assert!(!Tier::Recommended.is_required());
        assert!(!Tier::Optional.is_required());
    }
}
