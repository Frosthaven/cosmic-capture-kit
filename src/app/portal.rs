use super::*;

/// The per-source-type ScreenCast restore tokens (DRAGON-570): one slot for
/// monitor grants, one for window grants, so granting one kind never discards the
/// other (the single-token era overwrote whatever was stored on every grant).
///
/// The App field holding this keeps the historical name `pw_restore_token`, and
/// this type answers `is_some()`, so the capture settings page's existing
/// "Saved screen permission" read (`self.pw_restore_token.is_some()`) keeps
/// compiling and now honestly means "any slot holds a token". Renaming the field
/// means touching `settings/pages/capture.rs` in the same change.
///
/// Every grant REISSUES its token (using a token invalidates it), so the granted
/// slot is overwritten with whatever the portal returned, including `None`.
/// The clear semantics, one per cause:
/// - user cancel: [`Self::clear_source`] on the REQUESTED source only;
/// - the wrong-monitor region bounce: [`Self::clear_source`] on "monitor"
///   (region requests are monitor requests);
/// - the settings Forget row and factory reset: [`Self::clear`], both slots.
#[derive(Default, Clone)]
pub(crate) struct RestoreTokens {
    pub monitor: Option<String>,
    pub window: Option<String>,
}

impl RestoreTokens {
    /// Whether ANY slot holds a token. Drives the settings page's "Saved screen
    /// permission" row (its Forget button clears both slots).
    pub fn is_some(&self) -> bool {
        self.monitor.is_some() || self.window.is_some()
    }

    /// Store `token` in the slot for `source` ("monitor" / "window"). Any other
    /// key ("virtual" exists in the portal vocabulary but is never requested)
    /// has no slot and is dropped. `None` clears the slot: the portal reissues
    /// the token on every grant, so an absent reissue means the old token is
    /// spent and keeping it would only replay a dead grant.
    pub fn set(&mut self, source: &str, token: Option<String>) {
        match source {
            "monitor" => self.monitor = token,
            "window" => self.window = token,
            _ => {}
        }
    }

    /// Clear only the slot for `source` (the cancel / wrong-monitor causes).
    pub fn clear_source(&mut self, source: &str) {
        self.set(source, None);
    }

    /// Clear both slots (the settings Forget row, factory reset).
    pub fn clear(&mut self) {
        self.monitor = None;
        self.window = None;
    }
}

/// Settings-facing "honest forget" wording (DRAGON-570): clearing our tokens only
/// forces a new prompt; the portal's own permission-store entry (mode-2 persist,
/// see `platform/linux/portal/screencast.rs`) outlives this app and needs the
/// named command to remove. Used by the ResetScreencastPermission log line; the
/// capture settings page's "Saved screen permission" description should append it
/// too (that page file is wired separately).
pub(crate) const FORGET_SCREEN_ACCESS_NOTE: &str =
    "Forgetting only clears this app's saved tokens, so the next capture asks again. \
     The system portal keeps its own permission entry beyond this app; remove that \
     with: flatpak permission-remove screencast dev.thedragon.CosmicCaptureKit";

/// Where a ScreenCast request comes from, for the token-replay policy (the
/// DRAGON-570 refinement from the fifth live test). Passed EXPLICITLY by every
/// request call site — never inferred from session state, so a new call site
/// must say which it is.
///
/// Portable (no cfg): the off-Linux `portal_for_mode` stub carries it in its
/// signature so the portable toggle call sites compile everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestOrigin {
    /// The request that OPENS a fresh launch: the fallback monitor seed, or a
    /// pinned `--monitor` / `--window` launch driving the portal's picker.
    // Constructed only by the Linux seed dispatches (and the policy tests);
    // macOS/Windows compile the enum for the portable stub's signature but
    // never seed through a portal, so the variant is honestly dead there.
    #[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
    LaunchSeed,
    /// The user picking a DIFFERENT capture target inside a running session:
    /// the Monitor / Window buttons on the floating toolbar (DRAGON-580).
    /// Same nature as a launch pick, so it never replays either.
    TargetSwitch,
    /// A re-request that CONTINUES the target this session already resolved:
    /// the region commit after the seed, and a kind toggle (image ↔ video ↔
    /// scanner) that keeps the same monitor or window.
    InSession,
}

/// Pure, unit-tested (`replay_policy_tests`): whether a request of this origin
/// may replay a saved restore token.
///
/// The rule in one line: **whenever the USER's action expresses a target
/// choice, the picker shows.** Only a continuation of an already-chosen target
/// rides the saved token.
///
/// A LAUNCH request never replays. From the daemon or the CLI the pick IS the
/// target choice: the owner starts a capture precisely to choose a monitor, a
/// region's monitor, or a window, and a replayed token silently forces the
/// SAVED target instead ("it breaks if it forces us into the saved one"), which
/// on a window token even re-captures the same window with no picker at all. So
/// the portal picker always shows at launch, for every source type.
///
/// A TARGET SWITCH never replays either (DRAGON-580). This is the same defect
/// one step later in the session, and the owner hit it exactly that way:
/// Capture Region from the tray, then Window or Monitor on the floating
/// toolbar, and "both options are still using the saved permission item".
/// Pressing Monitor means "let me choose a monitor" as surely as launching a
/// monitor capture does; replaying there re-grants the seed's own monitor and
/// makes the button look broken. DRAGON-570 fixed the launch half and left this
/// one because the origin had only two variants, so a toolbar toggle rode the
/// same lane as the region commit.
///
/// An IN-SESSION CONTINUATION may replay: the session's target was already
/// picked once and is not being re-chosen, so replaying the reissued token is
/// what keeps one session from double-prompting (daemon Capture Region with a
/// video kind = ONE picker at the seed, zero at the commit; a kind toggle while
/// a monitor is already granted keeps that monitor). Grants still SAVE their
/// reissued tokens whatever the origin, since the slots serve this route.
// Consumed by the Linux request paths; compiled into every test build so the
// decision is proven on any host (the house pattern).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn replay_allowed(origin: RequestOrigin) -> bool {
    matches!(origin, RequestOrigin::InSession)
}

/// Pure, unit-tested: the restore token to REPLAY for a ScreenCast request of the
/// `requested` source type.
///
/// Evolution: DRAGON-544 stored ONE token plus the source it was granted for, and
/// this fn compared the two, because cosmic's portal treats a replayed token as
/// "restore that source" across source types: a Window request carrying a Monitor
/// token silently re-granted the whole monitor with no picker ("window mode
/// captures the monitor"). DRAGON-570 made the match STRUCTURAL: one slot per
/// source type ([`RestoreTokens`]), so a cross-type replay is impossible by
/// construction and granting one kind no longer discards the other's token. This
/// fn survives as the one reader of the slots for a request, keeping the decision
/// named and pinned; "virtual" (never requested) and unknown keys have no slot and
/// never replay.
// Its production callers are the Linux portal request paths; compiled into every
// test build so the decision is proven on any host (the house pattern).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn replayable_restore_token(tokens: &RestoreTokens, requested: &'static str) -> Option<String> {
    match requested {
        "monitor" => tokens.monitor.clone(),
        "window" => tokens.window.clone(),
        _ => None,
    }
}

/// Pure, unit-tested (`cursor_request_tests`): whether a ScreenCast request asks the
/// portal to keep the pointer in the stream (DRAGON-592).
///
/// Two terms, and the second is not a refinement of the first:
///
/// * `cursor_extra` is the EFFECTIVE "Preserve mouse cursor" extra, the persisted
///   preference ANDed with the active backend's `Caps::cursor_toggle`
///   ([`App::effective_capture_extras`]). Reading it here is what keeps this
///   decision from ever asking which backend is running: the capability table
///   already answered that.
/// * `color_picking` OVERRIDES it to [`CursorRequest::Omit`], whatever the setting
///   says. The picker's snapshot is a measurement instrument rather than a picture:
///   it reads one pixel per pointer move for the whole session, so a baked pointer
///   is not a cosmetic blemish, it is a permanent blind spot sitting over real
///   pixels the user is trying to sample. The portal GUARANTEES the worst version of
///   that, because its own permission dialog forces the pointer onto its OK button
///   at exactly the instant the seed frame is grabbed, which is precisely how this
///   was reported. It mirrors the native path, where
///   [`crate::app::launch_cursor_needed`] already answers false for a picker launch,
///   so the two surfaces cannot drift apart.
///
/// DRAGON-595 folded the RULE itself into [`crate::app::cursor_wanted`], which is now
/// the single copy. What survives here is the TRANSLATION of that one answer into this
/// backend's mechanism: the portal expresses "keep the pointer" as a stream cursor mode
/// chosen at request time, and nothing else can. The rule used to live here in full,
/// duplicating `launch_cursor_needed` term for term, with a test asserting the two
/// copies still agreed; the copies are gone, so there is nothing left to disagree.
// Consumed by the Linux portal request paths; compiled into every test build so the
// decision is proven on any host (the house pattern).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn cursor_request(
    cursor_extra: bool,
    color_picking: bool,
) -> crate::platform::screencast::CursorRequest {
    use crate::platform::screencast::CursorRequest;
    if crate::app::cursor_wanted(cursor_extra, color_picking) {
        CursorRequest::Keep
    } else {
        CursorRequest::Omit
    }
}

/// One registered output as [`output_for_grant_position`] needs it: `(logical_pos,
/// logical_size)`, in compositor logical coordinates. A plain pair rather than the whole
/// `OutputState`, so the decision stays pure and testable with no Wayland handle in sight.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
type OutputRect = ((i32, i32), (u32, u32));

/// Pure, unit-tested: which REGISTERED output a portal grant came from, given the granted
/// stream's logical position and the outputs' own [`OutputRect`]s. The
/// answer is an index into `outputs`, or `None` when the position names nothing we know.
///
/// # Why the portal grant is the capture-origin signal in a sandbox (DRAGON-549)
///
/// A portal session has NO capture overlay: the picker is the portal's own dialog. So the two
/// signals `App::active_trigger_display` normally uses are both absent. `capture_pointer_output`
/// is learned from the overlay's first pointer-enter, and there is no overlay; `trigger_display`
/// is the launch focused-toplevel guess, and resolving it needs `ext_foreign_toplevel_list_v1`,
/// which cosmic-comp hides from sandboxed clients. The ladder then fell through to
/// `self.outputs.first()`, which is wl_output REGISTRATION ORDER and means nothing.
///
/// That is not a small mistake on a mixed desktop. The owner's box has a 5120x1440 ultrawide
/// and an 800x480 auxiliary panel; picking the panel bounds the windowed editor's open fit to
/// 480pt of height, which floors at `PREVIEW_MIN_W`/`PREVIEW_MIN_H` — the SAME small window for
/// every capture, whatever was captured. That is the reported symptom exactly, and it cannot
/// happen on a native-capture session, where the overlay's pointer-enter answers first.
///
/// The grant knows. Two shapes, in order:
///
/// 1. **An exact `logical_pos` match.** A Monitor source's stream is that output, and the
///    portal reports the output's own origin. This is the same match
///    `App::on_fallback_cast_ready` already makes for the frozen backdrop.
/// 2. **The output CONTAINING the position.** A Window source's stream position is the
///    window's, not a monitor origin, so containment is what names its display.
///
/// `None` (no position, or a position on no known output) leaves the caller's existing
/// fallback untouched, which is the pre-DRAGON-549 behaviour. One caller layers more on
/// top of that `None` (DRAGON-549 reopened): COSMIC's portal sends `position: None` for
/// EVERY window stream, so shape 2 can never fire on a window grant, and
/// `on_pipewire_cast_ready` resolves those through the wallpaper compose's synthetic
/// anchor (`capture_flow::largest_output_index`) instead of letting the ladder fall to
/// `outputs.first()`.
// Its production callers are the Linux portal grant handlers; compiled into every test build
// so the decision is proven on any host (the house pattern).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn output_for_grant_position(
    outputs: &[OutputRect],
    position: Option<(i32, i32)>,
) -> Option<usize> {
    let (px, py) = position?;
    if let Some(i) = outputs.iter().position(|(pos, _)| *pos == (px, py)) {
        return Some(i);
    }
    outputs.iter().position(|((ox, oy), (ow, oh))| {
        px >= *ox && px < ox + *ow as i32 && py >= *oy && py < oy + *oh as i32
    })
}

/// The stable source-type key stored beside the restore token. Linux-only like the
/// request paths that feed it (ashpd is a Linux dependency).
#[cfg(target_os = "linux")]
fn source_key(source: ashpd::desktop::screencast::SourceType) -> &'static str {
    use ashpd::desktop::screencast::SourceType;
    match source {
        SourceType::Monitor => "monitor",
        SourceType::Window => "window",
        SourceType::Virtual => "virtual",
    }
}

impl App {
    /// Whether THIS session's compositor offers native screencopy at all
    /// (ext-image-copy-capture + output sources).
    #[cfg(target_os = "linux")]
    fn native_capture_available(&self) -> bool {
        let p = crate::platform::backend::wayland_protocols();
        p.image_copy_capture && p.output_source
    }

    /// macOS/Windows: no Wayland screencopy — capture is SCK/WGC. The portal path
    /// (which this gates, alongside `pipewire_available`) doesn't exist here.
    #[cfg(not(target_os = "linux"))]
    fn native_capture_available(&self) -> bool {
        false
    }

    /// Whether the SAVED screenshot-method choice (a `platform::backend` id),
    /// clamped to this session, lands on the portal: a preference for native
    /// screencopy can't apply on a compositor without it. Multi-session installs
    /// (COSMIC + GNOME/KDE logins sharing one config) hit this — a choice saved
    /// under COSMIC must not break captures after a GNOME login. Session-scoped;
    /// the persisted setting itself is never rewritten.
    pub(super) fn screenshot_uses_portal(&self) -> bool {
        self.screenshot_backend == crate::platform::backend::PORTAL_ID
            || !self.native_capture_available()
    }

    /// The saved recording-method choice, clamped the same way.
    pub(super) fn recording_uses_portal(&self) -> bool {
        self.record_backend == crate::platform::backend::PORTAL_ID
            || !self.native_capture_available()
    }

    /// The ACTIVE screenshot backend, as an OBJECT (DRAGON-595). "Active" is the
    /// backend a still capture would actually run on right now: the portal when the
    /// session-clamped choice lands there AND the portal is reachable (the same
    /// pairing every portal dispatch gate uses), otherwise native screencopy —
    /// including the portal-unreachable fallback, which really does grab through
    /// the compositor.
    ///
    /// This is THE place the Linux two-backend choice is resolved. Everything that
    /// used to ask `screenshot_uses_portal()` and then act on the answer should ask
    /// this and then ask the backend, so a capability, a mechanism or a label can
    /// never disagree with the plugin that implements it. The predicate itself stays
    /// (session shape still legitimately reads it: which surface to mint, whose
    /// picker presents the target choice, which settings rows render).
    #[cfg(target_os = "linux")]
    pub(super) fn active_screenshot_backend(
        &self,
    ) -> Box<dyn crate::platform::backend::CaptureBackend> {
        use crate::platform::backend::{wayland_protocols, PortalBackend, ScreencopyBackend};
        if self.screenshot_uses_portal() && self.pipewire_available {
            Box::new(PortalBackend {
                available: self.pipewire_available,
                ffmpeg: self.ffmpeg_available,
            })
        } else {
            Box::new(ScreencopyBackend {
                protocols: wayland_protocols(),
                ffmpeg: self.ffmpeg_available,
            })
        }
    }

    /// macOS: ScreenCaptureKit is the only backend, so it is always the active one.
    #[cfg(target_os = "macos")]
    pub(super) fn active_screenshot_backend(
        &self,
    ) -> Box<dyn crate::platform::backend::CaptureBackend> {
        Box::new(crate::platform::backend::MacBackend { ffmpeg: self.ffmpeg_available })
    }

    /// Windows (DRAGON-229): the Windows-Graphics-Capture backend is the only one, so
    /// it is always active. Its impl lives under `platform/windows/` (closed split);
    /// this dispatches to it.
    #[cfg(target_os = "windows")]
    pub(super) fn active_screenshot_backend(
        &self,
    ) -> Box<dyn crate::platform::backend::CaptureBackend> {
        Box::new(crate::platform::windows::backend::WindowsBackend {
            ffmpeg: self.ffmpeg_available,
        })
    }

    /// The ACTIVE RECORDING backend (DRAGON-595). Same shape as
    /// [`Self::active_screenshot_backend`], keyed on the recording method's own
    /// session-clamped choice, because the two are configured independently
    /// (`screenshot_backend` / `record_backend` are separate persisted settings and a
    /// session can genuinely record through the portal while screenshotting natively).
    #[cfg(target_os = "linux")]
    pub(super) fn active_record_backend(
        &self,
    ) -> Box<dyn crate::platform::backend::CaptureBackend> {
        use crate::platform::backend::{wayland_protocols, PortalBackend, ScreencopyBackend};
        if self.recording_uses_portal() && self.pipewire_available {
            Box::new(PortalBackend {
                available: self.pipewire_available,
                ffmpeg: self.ffmpeg_available,
            })
        } else {
            Box::new(ScreencopyBackend {
                protocols: wayland_protocols(),
                ffmpeg: self.ffmpeg_available,
            })
        }
    }

    /// macOS/Windows: one backend serves both kinds, so the recording backend is the
    /// screenshot one.
    #[cfg(not(target_os = "linux"))]
    pub(super) fn active_record_backend(
        &self,
    ) -> Box<dyn crate::platform::backend::CaptureBackend> {
        self.active_screenshot_backend()
    }

    /// The active screenshot backend's capabilities (DRAGON-186), read off the ONE
    /// selected object rather than a backend reconstructed for the question.
    pub(super) fn active_screenshot_caps(&self) -> crate::platform::backend::Caps {
        self.active_screenshot_backend().caps()
    }

    /// Whether WINDOW capture mode can produce anything in this session (DRAGON-620).
    ///
    /// Two ways to say yes, and the portal one is easy to miss: the portal backend reports
    /// `window_list: false` because it has no NATIVE window grid, yet window mode works there
    /// because the PORTAL's own picker selects the window. So this asks the portal question
    /// first, and only then whether the native backend can really enumerate toplevels.
    ///
    /// The native half is what wlroots fails. It advertises `ext_foreign_toplevel_list_v1`, so
    /// it enumerates windows fine, but cctk fills the GEOMETRY only from
    /// `zcosmic_toplevel_info_v1`, so `list_toplevels` returns an empty map and the grid has
    /// nothing to draw. See `backend::window_list_supported`.
    pub(super) fn window_mode_supported(&self) -> bool {
        self.mode_uses_portal() || self.active_screenshot_caps().window_list
    }

    /// The persisted capture-extra preferences as one set (DRAGON-186).
    /// `fullscreen_aware` has no user toggle (a behavior capability), so it
    /// passes through as `true` and the capability alone decides.
    fn capture_extra_prefs(&self) -> crate::platform::backend::CaptureExtras {
        crate::platform::backend::CaptureExtras {
            freeze: self.freeze,
            cursor: self.capture_cursor,
            transparency: self.capture_transparency,
            wallpaper: self.capture_wallpaper,
            fullscreen_aware: true,
        }
    }

    /// The extras a capture actually applies (DRAGON-186): each persisted
    /// preference gated by the active backend's capability, so a stale "on"
    /// persisted under a supporting backend can't leak onto one (the portal)
    /// that can't honor it. The preferences themselves are never rewritten.
    ///
    /// DRAGON-604 added the capture KIND as a third term, and this is the ONE seam it
    /// enters through: every cursor reader in the tree, on either backend, asks this
    /// function. The rule and the reasoning are on
    /// [`crate::app::capture_extras_for_kind`]; keep them there rather than growing a
    /// second copy at a call site.
    pub(super) fn effective_capture_extras(&self) -> crate::platform::backend::CaptureExtras {
        crate::app::capture_extras_for_kind(
            self.active_screenshot_caps().capture_extras(),
            self.capture_extra_prefs(),
            self.kind,
        )
    }

    /// Whether the chosen capture source for the current kind is the PipeWire
    /// portal (video recording, or PipeWire screenshots) and it's reachable. Drives
    /// whether monitor/window go through the portal picker.
    pub(super) fn mode_uses_portal(&self) -> bool {
        self.pipewire_available
            && ((self.kind == Kind::Video && self.recording_uses_portal())
                || (matches!(self.kind, Kind::Image | Kind::Scanner)
                    && self.screenshot_uses_portal()))
    }

    /// macOS/Windows: no xdg portal picker (capture is SCK/WGC). `mode_uses_portal`
    /// is always false here, so this is never reached — an inert `None` keeps the seam.
    #[cfg(not(target_os = "linux"))]
    pub(super) fn portal_for_mode(
        &mut self,
        _m: Mode,
        _origin: RequestOrigin,
    ) -> Option<Task<cosmic::Action<Msg>>> {
        None
    }

    /// Launch the portal picker for a monitor/window mode (PipeWire source), so it
    /// replaces the native picker. `None` for region (which keeps its draw + the
    /// at-commit portal request). Used by the mode/kind toggles (`InSession`) and
    /// by a pinned `--monitor` / `--window` launch's seed dispatch (`LaunchSeed`);
    /// the caller says which, and the token-replay policy follows from it.
    #[cfg(target_os = "linux")]
    pub(super) fn portal_for_mode(
        &mut self,
        m: Mode,
        origin: RequestOrigin,
    ) -> Option<Task<cosmic::Action<Msg>>> {
        use ashpd::desktop::screencast::SourceType;
        let (source, sel) = match m {
            Mode::Monitor => (SourceType::Monitor, self.monitor_placeholder_sel()),
            Mode::Window => (
                SourceType::Window,
                Selection {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                    output: None,
                    window_id: None,
                },
            ),
            Mode::Region => return None,
        };
        Some(self.request_pipewire(source, None, sel, origin))
    }

    /// A placeholder whole-monitor selection (the first output) for naming + the
    /// recording overlay anchor when the portal — not our overlay — picks the source.
    /// Linux-only: consumed by the Linux `portal_for_mode` / PipeWire cast path.
    #[cfg(target_os = "linux")]
    fn monitor_placeholder_sel(&self) -> Selection {
        match self.outputs.first() {
            Some(o) => Selection {
                x: o.logical_pos.0,
                y: o.logical_pos.1,
                width: o.logical_size.0,
                height: o.logical_size.1,
                output: Some(o.name.clone()),
                window_id: None,
            },
            None => Selection {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                output: Some("monitor".into()),
                window_id: None,
            },
        }
    }

    /// `lab/flatpak`: whether THIS session's capture overlay is the PORTAL-FROZEN
    /// fallback (no layer shell, capture clamped to the portal). The decision itself is
    /// pure ([`crate::platform::overlay_fallback_seeding`]); this only feeds it the live
    /// seams. Both capture kinds' clamps are ORed: in the session the fallback exists for
    /// (native screencopy absent) they are both true, and a pref-only portal choice on a
    /// layer-shell session is filtered out by the `layer_overlay` term anyway.
    #[cfg(target_os = "linux")]
    pub(super) fn overlay_fallback_active(&self) -> bool {
        crate::platform::overlay_fallback_seeding(
            crate::platform::layer_overlay_available(),
            self.screenshot_uses_portal() || self.recording_uses_portal(),
        )
    }

    /// macOS/Windows: never. Their overlays are the PlainWindows path, and the portal
    /// this fallback grabs through does not exist there.
    #[cfg(not(target_os = "linux"))]
    pub(super) fn overlay_fallback_active(&self) -> bool {
        false
    }

    /// `lab/flatpak`: the fallback path's ONE seed-time ScreenCast request
    /// (`SourceType::Monitor`, the persisted restore token). The portal's own picker IS
    /// the monitor selection on this path. There is no overlay yet to yield, and the
    /// result lands in `pw_slot` for [`Self::on_fallback_cast_ready`]. Deliberately NOT
    /// routed through `pw_pending`: that context means "a capture is committed and waits
    /// on this grant", and the seed grant commits nothing.
    #[cfg(target_os = "linux")]
    pub(super) fn request_fallback_cast(
        &mut self,
        origin: RequestOrigin,
    ) -> Task<cosmic::Action<Msg>> {
        log::info!(
            "overlay fallback: no layer shell, requesting a portal monitor stream to \
             freeze for the selection window (origin={origin:?})"
        );
        let slot = self.pw_slot.clone();
        // DRAGON-604 made the origin a parameter. It was hard-coded `LaunchSeed`, which
        // was honest while the fresh-launch dispatch was the only caller; the scanner's
        // mid-session re-seed is the second one, and it must REPLAY (switching capture
        // mode is not a target choice) or the user gets the picker again for a monitor
        // they already chose. The policy itself is untouched and still routes through
        // the one decision fn.
        let token = replay_allowed(origin)
            .then(|| replayable_restore_token(&self.pw_restore_token, "monitor"))
            .flatten();
        // DRAGON-592: this is ALSO the colour picker's portal request. A picker
        // launch is a Region launch (`portal_for_mode` answers None for Region), so
        // it seeds through here, and its frozen snapshot is what every sample reads
        // for the whole session. `cursor_request` is where the picker's override
        // lives; see its doc.
        let want_cursor = self.effective_capture_extras().cursor;
        let cursor = cursor_request(want_cursor, self.color_picking());
        // DRAGON-604: remember what THESE pixels will carry. The portal bakes the
        // pointer in at grab time, so this is the only record of it, and it is what a
        // later kind change compares against (`fallback_reseed_needed`).
        self.fallback_seed_cursor = Some(cursor == crate::platform::screencast::CursorRequest::Keep);
        log::debug!(
            "portal request: source=monitor origin={origin:?} replay={} cursor={cursor:?}",
            match (origin, token.is_some()) {
                (RequestOrigin::LaunchSeed, _) => "no (a launch pick is the target choice)",
                (RequestOrigin::TargetSwitch, _) => "no (the user is choosing a new target)",
                (RequestOrigin::InSession, true) => "yes (riding this session's saved token)",
                (RequestOrigin::InSession, false) => "allowed, but no saved token (the picker shows)",
            }
        );
        Task::perform(
            async move {
                let r = crate::platform::screencast::request(
                    ashpd::desktop::screencast::SourceType::Monitor,
                    token,
                    cursor,
                )
                .await;
                if let Ok(mut g) = slot.lock() {
                    *g = Some(r);
                }
            },
            |()| cosmic::Action::App(Msg::Capture(CaptureMsg::FallbackCastReady)),
        )
    }

    /// `lab/flatpak`: the seed-time grant landed. Persist the restore token, resolve the
    /// granted monitor against the registered outputs, and pull ONE frame off the UI
    /// thread (the PipeWire loop blocks; `grab_frame` carries its own 5s watchdog). The
    /// frame lands in `fallback_frame_slot` → [`Self::on_fallback_frozen_ready`].
    ///
    /// Every ending here is loud: a dismissed portal dialog is the session's Cancelled
    /// ending (the dialog stands in for the overlay, so dismissing it IS Escape), and an
    /// unreachable portal or an empty grant is `OverlayNeverShown` + `fail_session`:
    /// there is no layer-shell overlay to fall back to, which is the whole premise.
    /// `lab/flatpak`, DRAGON-604: re-grab the seed frame when a KIND change has moved
    /// the pointer answer, replaying this session's monitor token so the user is not
    /// asked for permission twice.
    ///
    /// Entering the scanner is the case that matters: on this path the seed frame is
    /// also the pixels the scanner decodes (`scan_reads_frozen`), and the portal baked
    /// the pointer into them at grab time, so the veto in
    /// [`crate::app::capture_extras_for_kind`] cannot reach them retroactively. Leaving
    /// the scanner re-seeds for the mirror-image reason: the pointer the user asked for
    /// has to come back, or the veto would be a one-way door for the rest of the
    /// session.
    ///
    /// `None` when nothing is needed, which is every session with layer shell, every
    /// kind change that does not move the bit, and any moment before the launch seed.
    /// A re-seed already in flight also answers `None` rather than stacking a second
    /// request on the one `pw_slot`.
    #[cfg(target_os = "linux")]
    pub(super) fn reseed_fallback_for_kind(&mut self) -> Option<Task<cosmic::Action<Msg>>> {
        // Two live preconditions, both about there being something to replace, kept out
        // of the pure rule because they are facts about this session rather than the
        // decision itself. A re-seed already in flight must not stack a second request
        // on the one `pw_slot`, and the LAUNCH seed must have finished: until its frame
        // lands there is no overlay window, so `fallback_seed_cursor` describes a grab
        // still in flight rather than pixels on screen. No UI exists to change the kind
        // in that window today, which is why this is a guard and not a bug fix, but the
        // guard is what keeps it that way.
        if self.fallback_reseeding || self.fallback_window.is_none() {
            return None;
        }
        let wants = self.effective_capture_extras().cursor;
        if !crate::app::fallback_reseed_needed(
            self.overlay_fallback_active(),
            self.fallback_seed_cursor,
            wants,
        ) {
            return None;
        }
        log::info!(
            "overlay fallback: the capture kind changed the pointer answer to \
             cursor={wants}; re-grabbing the seed frame on the saved grant (no new \
             prompt) so the frozen pixels match what this kind captures"
        );
        self.fallback_reseeding = true;
        Some(self.request_fallback_cast(RequestOrigin::InSession))
    }

    /// `lab/flatpak`, DRAGON-604: a REPLACEMENT seed request failed. Keep the session
    /// and the frame already on screen, say why once, and let the user carry on; the
    /// launch seed's fatal endings are not right here because there IS an overlay.
    /// Returns the no-op task the handlers hand back.
    #[cfg(target_os = "linux")]
    fn abandon_fallback_reseed(&mut self, why: &str) -> Task<cosmic::Action<Msg>> {
        self.fallback_reseeding = false;
        log::warn!(
            "overlay fallback: the seed re-grab did not complete ({why}); keeping the \
             frame already on screen, so this capture may not match the pointer \
             preference for its kind"
        );
        Task::none()
    }

    #[cfg(target_os = "linux")]
    pub(super) fn on_fallback_cast_ready(&mut self) -> Task<cosmic::Action<Msg>> {
        let result = self.pw_slot.lock().ok().and_then(|mut g| g.take());
        // DRAGON-604: a re-seed's endings are non-fatal (see `abandon_fallback_reseed`).
        // Checked BEFORE the match so every arm below can assume it is the launch seed.
        if self.fallback_reseeding {
            let session = match result {
                Some(Ok(s)) => s,
                Some(Err(crate::platform::screencast::CastError::Cancelled)) | None => {
                    // The user declined the replacement. Do NOT clear the monitor slot:
                    // the grant they declined is not the one the session is still using,
                    // and clearing it would cost the next launch its silent replay.
                    return self.abandon_fallback_reseed("the request was declined");
                }
                Some(Err(crate::platform::screencast::CastError::Unavailable(e))) => {
                    return self.abandon_fallback_reseed(&format!("the portal is unavailable: {e}"));
                }
            };
            self.pw_restore_token.set("monitor", session.restore_token.clone());
            self.save_state();
            let Some(stream) = session.streams.into_iter().next() else {
                return self.abandon_fallback_reseed("the grant carried no stream");
            };
            // Deliberately KEEP the already-resolved `fallback_grant` and
            // `portal_origin_output` rather than re-deriving them from this stream. A
            // replayed token restores the same source, so the geometry cannot have
            // moved, and re-anchoring a live overlay onto a different output mid-session
            // is a much worse failure than a stale pointer. `on_fallback_frozen_ready`
            // reads that same grant to place the replacement frame.
            if self.fallback_grant.is_none() {
                return self.abandon_fallback_reseed("the session has no recorded grant");
            }
            let slot = self.fallback_frame_slot.clone();
            let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel::<()>();
            let node_id = stream.node_id;
            let fd = session.fd;
            std::thread::spawn(move || {
                let img = crate::platform::pipewire::grab_frame(fd, node_id, None);
                if let Ok(mut g) = slot.lock() {
                    *g = Some(img);
                }
                let _ = tx.send(());
            });
            return Task::perform(
                async move {
                    let _ = rx.await;
                },
                |()| cosmic::Action::App(Msg::Capture(CaptureMsg::FallbackFrozenReady)),
            );
        }
        let session = match result {
            Some(Ok(s)) => s,
            Some(Err(crate::platform::screencast::CastError::Cancelled)) | None => {
                // Clear the monitor slot (this seed request's source) so the next
                // launch re-prompts instead of silently replaying a grant the user
                // just declined (the commit path's shape). A window token, granted
                // separately, is not what was declined and survives.
                self.pw_restore_token.clear_source("monitor");
                self.save_state();
                return self.teardown();
            }
            Some(Err(crate::platform::screencast::CastError::Unavailable(e))) => {
                crate::diag::note_failure(
                    crate::diag::Failure::OverlayNeverShown,
                    &format!(
                        "no wlr-layer-shell and the ScreenCast portal fallback is \
                         unreachable ({e}); no selection surface can be created"
                    ),
                );
                log::error!(
                    "overlay fallback: the ScreenCast portal is unavailable ({e}); \
                     without layer shell there is nothing left to draw the overlay with"
                );
                return self.fail_session();
            }
        };
        // Save the REISSUED token into the monitor slot (using a token invalidates
        // it, so the reissue is the only live one; a None reissue clears the slot).
        self.pw_restore_token.set("monitor", session.restore_token.clone());
        self.save_state();
        let Some(stream) = session.streams.into_iter().next() else {
            crate::diag::note_failure(
                crate::diag::Failure::OverlayNeverShown,
                "the seed-time portal grant carried no stream, so there is nothing to \
                 freeze for the fallback selection window",
            );
            return self.fail_session();
        };
        // Resolve the granted monitor against the wl_output-registered geometry (those
        // events DO arrive in a sandbox): by the stream's logical position, else the
        // first output (position-less portal impls), else fail: without an OutputState
        // there is no window to route the view to.
        let matched = stream
            .position
            .and_then(|pos| self.outputs.iter().find(|o| o.logical_pos == pos))
            .or_else(|| self.outputs.first());
        let Some(o) = matched else {
            crate::diag::note_failure(
                crate::diag::Failure::NoOutputs,
                "the portal granted a monitor but no wl_output ever registered, so the \
                 fallback selection window has no output to describe",
            );
            return self.fail_session();
        };
        if stream.position.is_some() && stream.position != Some(o.logical_pos) {
            log::warn!(
                "overlay fallback: the granted stream's position matches no registered \
                 output; anchoring to the first one"
            );
        }
        // DRAGON-549: this IS the capture-origin monitor, and on this path it is the only
        // thing that knows. Recorded for the preview's own anchor + fit bound, which without
        // it fell through to `outputs.first()` (see `output_for_grant_position`).
        self.portal_origin_output = Some(o.name.clone());
        self.fallback_grant = Some(FallbackGrant {
            name: o.name.clone(),
            pos: o.logical_pos,
            size: o.logical_size,
        });
        // The portal pick IS the monitor act on this path; what remains is drawing a
        // region over its frozen frame, so the selector opens in Region mode whatever
        // mode the launch asked for (the same in-memory bounce the cancel path takes).
        self.mode = Mode::Region;
        // Pull the one frame on a dedicated OS thread (the PipeWire loop blocks). The
        // oneshot only signals COMPLETION; the pixels ride the slot. An `Err` on the
        // await means the grab thread died: post the empty result so the handler runs
        // its no-frame ending instead of the session hanging on a slot nobody fills.
        let slot = self.fallback_frame_slot.clone();
        let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel::<()>();
        let node_id = stream.node_id;
        let fd = session.fd;
        std::thread::spawn(move || {
            let img = crate::platform::pipewire::grab_frame(fd, node_id, None);
            if let Ok(mut g) = slot.lock() {
                *g = Some(img);
            }
            let _ = tx.send(());
        });
        Task::perform(
            async move {
                let _ = rx.await;
            },
            |()| cosmic::Action::App(Msg::Capture(CaptureMsg::FallbackFrozenReady)),
        )
    }

    /// `lab/flatpak`: the seed frame posted (or its worker died). With a frame: build
    /// the [`FrozenOutput`] backdrop for the granted monitor and mint the ONE fullscreen
    /// fallback toplevel, recording its winit id into that monitor's `OutputState` so
    /// the existing `view_window` → `overlay_view` → `with_frozen_bg` chain renders
    /// region selection over the still with no view changes. Without one: the launch
    /// scene never arrived, said in both vocabularies, then `fail_session`.
    #[cfg(target_os = "linux")]
    pub(super) fn on_fallback_frozen_ready(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(grant) = self.fallback_grant.clone() else {
            return Task::none(); // no grant recorded; nothing to mint
        };
        let Some(frame) = self.fallback_frame_slot.lock().ok().and_then(|mut g| g.take()) else {
            return Task::none(); // a stray repeat; the slot was already drained
        };
        let Some(img) = frame else {
            // DRAGON-604: on a re-seed the previous frame is still on screen and still
            // usable, so a missed replacement is a warning, not the end of the session.
            if self.fallback_reseeding {
                return self.abandon_fallback_reseed("the replacement frame never arrived");
            }
            crate::diag::note_failure(
                crate::diag::Failure::SceneGrabTimeout,
                "the seed-time portal stream delivered no frame within the grab budget, \
                 so the fallback overlay has no frozen scene to select over",
            );
            return self.fail_session();
        };
        log::info!(
            "overlay fallback: frozen frame ready ({}x{} px for a {}x{} logical output); {}",
            img.width(),
            img.height(),
            grant.size.0,
            grant.size.1,
            // DRAGON-604: a re-seed replaces the scene under a window that is already up,
            // so the launch wording would be a lie on that path.
            if self.fallback_reseeding {
                "replacing the frozen scene under the open selection window"
            } else {
                "minting the fullscreen selection window"
            },
        );
        let img = std::sync::Arc::new(img);
        let handle = shared_rgba_handle(&img);
        self.frozen.insert(
            grant.name.clone(),
            FrozenOutput {
                img,
                handle,
                logical_pos: grant.pos,
                logical_size: (grant.size.0 as i32, grant.size.1 as i32),
            },
        );
        // DRAGON-604: a re-seed REPLACES the entry above and stops there. The window is
        // already up (`mint_fallback_window` is idempotent, but saying so here keeps the
        // two paths' intent visible), and the backdrop plus the scanner's read source
        // both come off `self.frozen`, so swapping the entry is the whole update. A scan
        // in progress is re-armed so it decodes the frame that just landed rather than
        // keeping an answer read off the pointer-bearing one.
        if self.fallback_reseeding {
            self.fallback_reseeding = false;
            log::info!("overlay fallback: the re-grabbed seed frame replaced the frozen scene");
            if self.kind == Kind::Scanner {
                self.begin_scan_shot();
            }
            return Task::none();
        }
        self.mint_fallback_window()
    }

    /// `lab/flatpak`: mint (or RE-mint) the fallback selection toplevel from the kept
    /// seed grant. Idempotent: a window already up, or no grant recorded yet, is a
    /// no-op, so the restore/countdown callers can invoke it unconditionally. A fresh
    /// mint resets the granted output's recorded window size (the new window's resize
    /// event reports the real one, from which `units` builds the letterbox bridge) and
    /// records the new winit id in both the `OutputState` and `fallback_window`,
    /// keeping view routing and the close paths lined up.
    #[cfg(target_os = "linux")]
    pub(super) fn mint_fallback_window(&mut self) -> Task<cosmic::Action<Msg>> {
        if self.fallback_window.is_some() {
            return Task::none();
        }
        let Some(grant) = self.fallback_grant.clone() else {
            return Task::none();
        };
        let (id, open) = super::shell::overlay_fallback_window(grant.size);
        if let Some(o) = self.outputs.iter_mut().find(|o| o.name == grant.name) {
            o.id = id;
            o.fallback_win_size = None;
        }
        self.fallback_window = Some(id);
        open
    }

    /// Yield the overlay and fire the async ScreenCast request for `source` (so the
    /// portal dialog — a normal window — isn't hidden behind our Overlay layer). The
    /// result lands in `pw_slot` and is handled by [`Self::on_pipewire_cast_ready`].
    /// `origin` drives the token-replay policy ([`replay_allowed`]): a launch seed
    /// always shows the picker, an in-session re-request replays its session's token.
    #[cfg(target_os = "linux")]
    pub(super) fn request_pipewire(
        &mut self,
        source: ashpd::desktop::screencast::SourceType,
        region: Option<RegionTarget>,
        sel: Selection,
        origin: RequestOrigin,
    ) -> Task<cosmic::Action<Msg>> {
        if self.pw_pending.is_some() {
            return Task::none(); // a request is already in flight
        }
        let key = source_key(source);
        self.pw_pending = Some(PwPending { sel, region, source_key: key });
        let mut cmds = self.yield_overlays();
        let slot = self.pw_slot.clone();
        // Replay policy first (a launch pick is the target choice; see
        // `replay_allowed`), then only the requested source type's own slot; see
        // `replayable_restore_token` for the cross-type mis-grant the per-source
        // slots make structurally impossible.
        let token = replay_allowed(origin)
            .then(|| replayable_restore_token(&self.pw_restore_token, key))
            .flatten();
        // DRAGON-592: the pointer follows the effective "Preserve mouse cursor"
        // extra, which is where a portal session's cursor toggle finally reaches the
        // request (it used to be a hard-coded `Embedded`). See `cursor_request`.
        let cursor = cursor_request(self.effective_capture_extras().cursor, self.color_picking());
        log::debug!(
            "portal request: source={key} origin={origin:?} cursor={cursor:?} replay={}",
            match (origin, token.is_some()) {
                (RequestOrigin::LaunchSeed, _) => "no (a launch pick is the target choice)",
                (RequestOrigin::TargetSwitch, _) => "no (the user is choosing a new target)",
                (RequestOrigin::InSession, true) => "yes (riding this session's saved token)",
                (RequestOrigin::InSession, false) =>
                    "allowed, but no saved token (the picker shows)",
            }
        );
        cmds.push(Task::perform(
            async move {
                let r = crate::platform::screencast::request(source, token, cursor).await;
                if let Ok(mut g) = slot.lock() {
                    *g = Some(r);
                }
            },
            |()| cosmic::Action::App(Msg::Capture(CaptureMsg::PipewireCastReady)),
        ));
        Task::batch(cmds)
    }

    /// Process the async ScreenCast result: hold the granted stream and continue the
    /// capture (countdown then record), fall back to screencopy when the portal is
    /// unavailable, or — on cancel / a region wrong-monitor pick — return to the
    /// selector (with a toast for the wrong monitor).
    pub(super) fn on_pipewire_cast_ready(&mut self) -> Task<cosmic::Action<Msg>> {
        let result = self.pw_slot.lock().ok().and_then(|mut g| g.take());
        let Some(pending) = self.pw_pending.take() else {
            return self.restore_interactive_overlays();
        };
        let session = match result {
            Some(Ok(s)) => s,
            // User cancelled / no result: go back to region select, clearing only
            // the REQUESTED source's slot so the next attempt of that kind
            // re-prompts. The other kind's grant was not declined and survives.
            Some(Err(crate::platform::screencast::CastError::Cancelled)) | None => {
                self.pw_restore_token.clear_source(pending.source_key);
                self.mode = Mode::Region;
                self.save_state();
                return self.restore_interactive_overlays();
            }
            Some(Err(crate::platform::screencast::CastError::Unavailable(e))) => {
                log::warn!("portal unavailable ({e}); falling back to screencopy");
                return self.proceed_capture(pending.sel);
            }
        };
        // Save the REISSUED token into the requested source's own slot (using a
        // token invalidates it, so the reissue is the only live one; a None reissue
        // clears the slot). The other source type's slot is untouched: granting one
        // kind must not discard the other (DRAGON-570).
        self.pw_restore_token.set(pending.source_key, session.restore_token.clone());
        self.save_state();
        let Some(stream) = session.streams.into_iter().next() else {
            return self.proceed_capture(pending.sel);
        };

        // DRAGON-549: record which registered output this grant came from, BEFORE any of the
        // per-mode handling below, because every mode wants it and only this moment has it.
        // A `--window` / `--monitor` portal launch mints no capture overlay at all, so the
        // preview's own anchor ladder (`active_trigger_display`) has neither a pointer output
        // nor a resolvable focused toplevel and was falling through to `outputs.first()` —
        // registration order, which on a mixed desktop is the wrong display and bounds the
        // editor's open fit to it. See `output_for_grant_position` for the whole reasoning.
        #[cfg(target_os = "linux")]
        let window_grant = {
            let rects: Vec<OutputRect> =
                self.outputs.iter().map(|o| (o.logical_pos, o.logical_size)).collect();
            let origin = output_for_grant_position(&rects, stream.position);
            if let Some(i) = origin {
                self.portal_origin_output = Some(self.outputs[i].name.clone());
            } else if pending.source_key == "window" && stream.position.is_none() {
                // DRAGON-549 reopened: COSMIC's portal sends `position: None` for EVERY
                // window stream (the DRAGON-562 measurement), so on a window grant the
                // containment above never fires and the preview's anchor ladder fell
                // through to `outputs.first()`: registration order, the owner's 800x480
                // side panel, which floored every window capture's editor at the same
                // small window. Resolve the SAME output the wallpaper compose synthesizes
                // its anchor on (`capture_flow::largest_output_index`, one source of
                // truth): the largest registered output, where desktop windows
                // overwhelmingly live, so the fit bound is the 5120x1440 ultrawide and
                // the media opens 1:1.
                let sizes: Vec<(i32, i32)> = self
                    .outputs
                    .iter()
                    .map(|o| (o.logical_size.0 as i32, o.logical_size.1 as i32))
                    .collect();
                if let Some(i) = super::capture_flow::largest_output_index(&sizes) {
                    self.portal_origin_output = Some(self.outputs[i].name.clone());
                    log::debug!(
                        "portal grant: the window stream carried no position; preview \
                         origin anchored to the largest registered output (the wallpaper \
                         compose's synthetic-anchor rule)"
                    );
                }
            }
            // DRAGON-562: a WINDOW grant carries everything the single-window
            // aesthetics need, and only this moment has it — the window's global
            // position (the stream's), the registered outputs' geometry (torn down
            // with the overlays before the capture runs), and the origin output's
            // buffer scale. Snapshot it onto the held stream; the still path
            // (`do_pixel_capture`) decides at capture time whether to decorate.
            (pending.source_key == "window").then(|| {
                let origin = origin.map(|i| &self.outputs[i]);
                PortalWindowGrant {
                    pos: stream.position,
                    origin_rect: origin.map(|o| {
                        (
                            o.logical_pos.0,
                            o.logical_pos.1,
                            o.logical_size.0 as i32,
                            o.logical_size.1 as i32,
                        )
                    }),
                    scale: origin.map(|o| o.scale).unwrap_or(1.0),
                    outputs: self
                        .outputs
                        .iter()
                        .map(|o| {
                            (
                                o.name.clone(),
                                o.logical_pos,
                                (o.logical_size.0 as i32, o.logical_size.1 as i32),
                            )
                        })
                        .collect(),
                }
            })
        };

        // Region: the granted monitor must be the one the region was clamped to;
        // otherwise return to draw with a message. Then crop to the region.
        let crop = match &pending.region {
            Some(rt) => {
                if stream.position != Some(rt.out_pos) {
                    self.toast = Some("Selected region not found in selected output".into());
                    // The wrong-monitor bounce clears the MONITOR slot (a region
                    // request is a monitor request): replaying this grant would
                    // restore the same wrong monitor. A window grant is unrelated.
                    self.pw_restore_token.clear_source("monitor");
                    self.save_state();
                    return self.restore_interactive_overlays();
                }
                let (sw, sh) = stream
                    .size
                    .unwrap_or((rt.out_size.0 as i32, rt.out_size.1 as i32));
                let scale_x = sw as f32 / rt.out_size.0.max(1) as f32;
                let scale_y = sh as f32 / rt.out_size.1.max(1) as f32;
                let relx = (rt.rect.0 - rt.out_pos.0) as f32;
                let rely = (rt.rect.1 - rt.out_pos.1) as f32;
                let even = |v: f32| (v.round().max(0.0) as u32) & !1;
                Some((
                    (relx * scale_x).round().max(0.0) as u32,
                    (rely * scale_y).round().max(0.0) as u32,
                    even(rt.rect.2 as f32 * scale_x),
                    even(rt.rect.3 as f32 * scale_y),
                ))
            }
            None => None,
        };

        // Monitor mode: the placeholder selection guessed `outputs.first()`, but the
        // portal dialog let the user pick ANY monitor — re-anchor the selection to
        // the output the stream was actually granted on (matched by the stream's
        // logical position), so the filename, the preview's monitor bound, and the
        // toolbar/tray logic all track the RECORDED monitor, not the first one.
        // (Proven wrong the other way: a 5120×1440 stream saved under another
        // output's name, with the preview fitted to that other monitor's size.)
        let mut sel = pending.sel;
        if self.mode == Mode::Monitor
            && pending.region.is_none()
            && let Some(pos) = stream.position
            && let Some(o) = self.outputs.iter().find(|o| o.logical_pos == pos)
        {
            sel = Selection {
                x: o.logical_pos.0,
                y: o.logical_pos.1,
                width: o.logical_size.0,
                height: o.logical_size.1,
                output: Some(o.name.clone()),
                window_id: None,
            };
        }

        // Hold the stream and run the normal commit flow (countdown then record);
        // `start_recording` consumes the held stream when recording actually begins.
        self.pw_held = Some(HeldStream {
            fd: session.fd,
            node_id: stream.node_id,
            crop,
            #[cfg(target_os = "linux")]
            window_grant,
        });
        self.proceed_capture(sel)
    }

    /// Clamp `sel` to the output it overlaps most — the target monitor + the region
    /// trimmed to it (global logical coords). `None` if no output overlaps.
    /// Linux-only: the Wayland region-capture flow (`capture_flow.rs`) is the sole caller.
    #[cfg(target_os = "linux")]
    pub(super) fn region_clamped(&self, sel: &Selection) -> Option<RegionTarget> {
        let (rx, ry) = (sel.x, sel.y);
        let (rx1, ry1) = (rx + sel.width as i32, ry + sel.height as i32);
        let overlap = |o: &OutputState| {
            let (ox, oy) = o.logical_pos;
            let (ow, oh) = (o.logical_size.0 as i32, o.logical_size.1 as i32);
            let ix = rx1.min(ox + ow) - rx.max(ox);
            let iy = ry1.min(oy + oh) - ry.max(oy);
            (ix.max(0) as i64) * (iy.max(0) as i64)
        };
        let o = self.outputs.iter().max_by_key(|o| overlap(o))?;
        if overlap(o) == 0 {
            return None;
        }
        let (ox, oy) = o.logical_pos;
        let (ow, oh) = (o.logical_size.0 as i32, o.logical_size.1 as i32);
        let cx0 = rx.max(ox);
        let cy0 = ry.max(oy);
        let cx1 = rx1.min(ox + ow);
        let cy1 = ry1.min(oy + oh);
        Some(RegionTarget {
            out_pos: o.logical_pos,
            out_size: o.logical_size,
            rect: (cx0, cy0, (cx1 - cx0) as u32, (cy1 - cy0) as u32),
        })
    }
}

/// DRAGON-549: the portal grant as the capture-origin signal. Its own module because it pins
/// ONE function and one bug — a portal session anchoring the preview on an arbitrary display.
#[cfg(test)]
mod grant_origin_tests {
    use super::output_for_grant_position;

    /// The owner's actual desktop: a 5120x1440 ultrawide beside an 800x480 auxiliary panel.
    /// Registration order puts the small one FIRST, which is exactly what made
    /// `outputs.first()` the wrong answer.
    const MIXED: [super::OutputRect; 2] = [((5120, 0), (800, 480)), ((0, 0), (5120, 1440))];

    /// A Monitor grant reports the output's own origin, so the match is exact — the same one
    /// the frozen-backdrop path already makes.
    #[test]
    fn a_monitor_grant_matches_its_output_exactly() {
        assert_eq!(output_for_grant_position(&MIXED, Some((0, 0))), Some(1));
        assert_eq!(output_for_grant_position(&MIXED, Some((5120, 0))), Some(0));
    }

    /// A Window grant's position is the WINDOW's, not a monitor origin, so containment is
    /// what names its display. This is the case the owner hit: a window capture on the
    /// ultrawide, with the preview sized against the 800x480 panel.
    #[test]
    fn a_window_grant_resolves_by_containment() {
        assert_eq!(output_for_grant_position(&MIXED, Some((1200, 700))), Some(1));
        assert_eq!(output_for_grant_position(&MIXED, Some((5300, 100))), Some(0));
        // The exact-origin rule wins over containment when both could apply, so a monitor
        // grant can never be mistaken for a window sitting at the top-left corner.
        assert_eq!(output_for_grant_position(&MIXED, Some((0, 0))), Some(1));
    }

    /// Nothing to go on leaves the caller's existing ladder alone — the pre-DRAGON-549
    /// behaviour, and what every native-capture launch gets.
    #[test]
    fn an_unknown_or_absent_position_answers_nothing() {
        assert_eq!(output_for_grant_position(&MIXED, None), None);
        // Off every registered output (a display unplugged between grant and commit).
        assert_eq!(output_for_grant_position(&MIXED, Some((-4000, -4000))), None);
        assert_eq!(output_for_grant_position(&[], Some((0, 0))), None);
    }

    /// Edges are half-open, so two abutting outputs cannot both claim the seam.
    #[test]
    fn output_edges_are_half_open() {
        let outs = [((0, 0), (100u32, 100u32)), ((100, 0), (100, 100))];
        assert_eq!(output_for_grant_position(&outs, Some((99, 99))), Some(0));
        // (100, 0) is the SECOND output's exact origin, and its containment start.
        assert_eq!(output_for_grant_position(&outs, Some((100, 0))), Some(1));
        assert_eq!(output_for_grant_position(&outs, Some((100, 50))), Some(1));
        assert_eq!(output_for_grant_position(&outs, Some((200, 50))), None);
    }
}

/// The DRAGON-570 refinement: replay is an ORIGIN policy, not a per-source one.
/// Tiny on purpose — the point is the named pin, so a future "just replay at
/// launch, it saves a prompt" reads this and the reasoning on `replay_allowed`
/// before regressing the daemon's target choice.
#[cfg(test)]
mod replay_policy_tests {
    use super::{RequestOrigin, replay_allowed};

    // A launch pick IS the target choice: never replay, for every source type
    // (a replayed WINDOW token silently re-captures the same window, the same
    // disease as a forced monitor).
    #[test]
    fn launch_seed_never_replays() {
        assert!(!replay_allowed(RequestOrigin::LaunchSeed));
    }

    // DRAGON-580: a toolbar Monitor / Window press is a target choice too, so
    // it shows the picker exactly like a launch. The owner's report: region
    // from the tray, then Window or Monitor on the toolbar, both silently
    // reusing the saved grant.
    #[test]
    fn a_target_switch_never_replays() {
        assert!(!replay_allowed(RequestOrigin::TargetSwitch));
    }

    // The continuation replays, so one session never double-prompts (daemon
    // Capture Region with a video kind = one picker at the seed, zero at the
    // commit; a kind toggle keeps the monitor it was already given).
    #[test]
    fn a_session_continuation_replays() {
        assert!(replay_allowed(RequestOrigin::InSession));
    }

    // The whole policy as one table, so adding a variant forces a decision here
    // rather than defaulting into someone else's lane.
    #[test]
    fn only_a_continuation_of_an_already_chosen_target_replays() {
        for origin in [RequestOrigin::LaunchSeed, RequestOrigin::TargetSwitch] {
            assert!(!replay_allowed(origin), "{origin:?} expresses a target choice");
        }
        assert!(replay_allowed(RequestOrigin::InSession));
    }
}

/// DRAGON-592: the cursor half of the request policy. The picker override earns its
/// own pins rather than riding on the setting, because "the picker happens to have
/// the preference off" is not a guarantee of anything: the preference defaults ON,
/// and a picker session with a baked pointer has a permanent blind spot over the
/// pixels it exists to read.
#[cfg(test)]
mod cursor_request_tests {
    use super::cursor_request;
    use crate::platform::screencast::CursorRequest;

    // An ordinary capture follows the effective extra, both ways. This is the whole
    // user-visible feature: before it, a portal capture was `Embedded` regardless.
    #[test]
    fn an_ordinary_capture_follows_the_preference() {
        assert_eq!(cursor_request(true, false), CursorRequest::Keep);
        assert_eq!(cursor_request(false, false), CursorRequest::Omit);
    }

    // The picker omits the pointer whatever the setting says. The ON case is the one
    // that matters: it is the default, and it is the reported bug.
    #[test]
    fn the_colour_picker_always_omits_the_pointer() {
        assert_eq!(cursor_request(true, true), CursorRequest::Omit);
        assert_eq!(cursor_request(false, true), CursorRequest::Omit);
    }

    // The whole table, so a future term has to be placed deliberately rather than
    // defaulting into someone else's lane. Read as: KEEP happens in exactly one of
    // the four states.
    #[test]
    fn only_a_non_picker_capture_with_the_extra_on_keeps_the_pointer() {
        for (extra, picking) in [(true, true), (false, true), (false, false)] {
            assert_eq!(
                cursor_request(extra, picking),
                CursorRequest::Omit,
                "extra={extra} picking={picking}"
            );
        }
        assert_eq!(cursor_request(true, false), CursorRequest::Keep);
    }

    // DRAGON-604, the end-to-end check: a SCANNER capture reaches this mechanism as
    // Omit, so a portal scan's stream is asked to leave the pointer out rather than the
    // app merely declining to draw one. Inputs come from the real
    // `app::capture_extras_for_kind`, with the capability and the preference both ON,
    // which is the default state and the strongest case the mode veto has to beat. This
    // is the surface the owner reported: on a Flatpak session the stream's cursor mode
    // is the ONLY control that exists, because the portal hands back a finished frame.
    #[test]
    fn a_scanner_capture_asks_the_stream_to_omit_the_pointer() {
        use crate::platform::backend::CaptureExtras;
        let all_on = CaptureExtras {
            freeze: true,
            cursor: true,
            transparency: true,
            wallpaper: true,
            fullscreen_aware: true,
        };
        let scan = crate::app::capture_extras_for_kind(all_on, all_on, crate::app::Kind::Scanner);
        assert_eq!(cursor_request(scan.cursor, false), CursorRequest::Omit);
        // And the same inputs in a picture kind still Keep it, so this is the MODE
        // deciding and not the extras collapsing.
        let shot = crate::app::capture_extras_for_kind(all_on, all_on, crate::app::Kind::Image);
        assert_eq!(cursor_request(shot.cursor, false), CursorRequest::Keep);
    }

    // The picker's rule is the SAME shape the native path uses for its launch cursor
    // grab: a picker launch neither grabs a sprite natively nor asks the portal for
    // one. Since DRAGON-595 that is true BY CONSTRUCTION (both read
    // `app::cursor_wanted`), so this pins the translation rather than an agreement
    // between two copies: whatever the shared rule answers, the portal's mechanism
    // says Keep for yes and Omit for no, and never inverts.
    #[test]
    fn the_portal_mechanism_translates_the_shared_rule_faithfully() {
        for (extra, picking) in [(true, true), (true, false), (false, true), (false, false)] {
            let wanted = crate::app::cursor_wanted(extra, picking);
            assert_eq!(
                cursor_request(extra, picking) == CursorRequest::Keep,
                wanted,
                "extra={extra} picking={picking}"
            );
            // And the native launch gate reads the same rule, so the two surfaces
            // still cannot drift.
            assert_eq!(crate::app::launch_cursor_needed(extra, picking), wanted);
        }
    }
}

#[cfg(test)]
mod restore_token_tests {
    use super::{RestoreTokens, replayable_restore_token};

    fn both() -> RestoreTokens {
        RestoreTokens { monitor: Some("mtok".into()), window: Some("wtok".into()) }
    }

    // The lab/flatpak live-test bug the gate exists for: a Window request replaying
    // a MONITOR token made cosmic's portal silently restore the monitor source, so
    // "pick a window" captured the whole monitor with no picker shown. Per-source
    // slots (DRAGON-570) make the cross-type replay structurally impossible: each
    // request can only ever read its own slot.
    #[test]
    fn a_request_only_reads_its_own_slot() {
        let monitor_only = RestoreTokens { monitor: Some("mtok".into()), window: None };
        assert_eq!(replayable_restore_token(&monitor_only, "window"), None);
        assert_eq!(replayable_restore_token(&monitor_only, "monitor"), Some("mtok".to_string()));
        let window_only = RestoreTokens { monitor: None, window: Some("wtok".into()) };
        assert_eq!(replayable_restore_token(&window_only, "monitor"), None);
        assert_eq!(replayable_restore_token(&window_only, "window"), Some("wtok".to_string()));
    }

    // With both kinds granted, each request replays its own grant: the whole point
    // of the slots is that holding one kind's grant no longer costs the other's.
    #[test]
    fn both_slots_replay_independently() {
        assert_eq!(replayable_restore_token(&both(), "monitor"), Some("mtok".to_string()));
        assert_eq!(replayable_restore_token(&both(), "window"), Some("wtok".to_string()));
    }

    // "virtual" exists in the portal's source vocabulary but is never requested by
    // this app; it has no slot, so it (and any unknown key) never replays anything.
    #[test]
    fn a_sourceless_kind_never_replays() {
        assert_eq!(replayable_restore_token(&both(), "virtual"), None);
    }

    // Empty slots replay nothing.
    #[test]
    fn no_token_means_no_replay() {
        let none = RestoreTokens::default();
        assert_eq!(replayable_restore_token(&none, "monitor"), None);
        assert_eq!(replayable_restore_token(&none, "window"), None);
    }
}

/// DRAGON-570: the slot store's own semantics, one test per cause of change. The
/// handlers reduce to one-line calls on [`RestoreTokens`], so these pin the whole
/// clear/save behavior on any host.
#[cfg(test)]
mod restore_slot_tests {
    use super::RestoreTokens;

    fn both() -> RestoreTokens {
        RestoreTokens { monitor: Some("mtok".into()), window: Some("wtok".into()) }
    }

    // A grant saves the reissued token into ITS slot and leaves the other alone.
    // This is the headline bug: the single-token era discarded the monitor grant
    // the moment a window grant landed (and vice versa).
    #[test]
    fn a_grant_fills_its_slot_without_discarding_the_other() {
        let mut t = RestoreTokens { monitor: Some("old-mtok".into()), window: None };
        t.set("window", Some("wtok".into()));
        assert_eq!(t.monitor.as_deref(), Some("old-mtok"));
        assert_eq!(t.window.as_deref(), Some("wtok"));
        // And the reissue overwrites the spent token of its own kind.
        t.set("monitor", Some("new-mtok".into()));
        assert_eq!(t.monitor.as_deref(), Some("new-mtok"));
        assert_eq!(t.window.as_deref(), Some("wtok"));
    }

    // A grant whose response carries no token clears the slot: the old token was
    // spent by the request that just used it, so keeping it would replay a dead
    // grant next time.
    #[test]
    fn a_tokenless_reissue_clears_the_spent_slot() {
        let mut t = both();
        t.set("monitor", None);
        assert_eq!(t.monitor, None);
        assert_eq!(t.window.as_deref(), Some("wtok"));
    }

    // User cancel (and the wrong-monitor bounce, which is a monitor-request
    // outcome) clears only the requested source's slot.
    #[test]
    fn cancel_clears_only_the_requested_source() {
        let mut t = both();
        t.clear_source("monitor");
        assert_eq!(t.monitor, None);
        assert_eq!(t.window.as_deref(), Some("wtok"));
        let mut u = both();
        u.clear_source("window");
        assert_eq!(u.monitor.as_deref(), Some("mtok"));
        assert_eq!(u.window, None);
    }

    // The settings Forget row and factory reset clear BOTH slots.
    #[test]
    fn forget_clears_both_slots() {
        let mut t = both();
        t.clear();
        assert_eq!(t.monitor, None);
        assert_eq!(t.window, None);
        assert!(!t.is_some());
    }

    // `is_some` is the settings page's "show the Forget row" read: any one slot
    // holding a token counts.
    #[test]
    fn any_slot_counts_as_a_saved_permission() {
        assert!(!RestoreTokens::default().is_some());
        assert!(RestoreTokens { monitor: Some("m".into()), window: None }.is_some());
        assert!(RestoreTokens { monitor: None, window: Some("w".into()) }.is_some());
        assert!(both().is_some());
    }

    // A source kind without a slot ("virtual" / garbage) is dropped, never stored.
    #[test]
    fn a_slotless_source_is_dropped() {
        let mut t = RestoreTokens::default();
        t.set("virtual", Some("vtok".into()));
        assert!(!t.is_some());
    }
}
