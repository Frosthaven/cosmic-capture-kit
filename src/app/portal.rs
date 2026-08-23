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

/// **Pure**, unit-tested: WHERE the window a portal grant just handed us actually sits, found
/// by matching the granted stream's SIZE against the toplevels the compositor lists
/// (DRAGON-735).
///
/// # Why this is needed at all
///
/// The xdg ScreenCast portal names a PipeWire node and never a window, and COSMIC's
/// implementation goes further: it hard-codes `position: None` for every window stream. That is
/// read off the portal's own source, not inferred, and the portal notes its own limitation
/// there ("XXX: No way to get size of whole toplevel?"). So a window grant reaches us with no
/// on-screen anchor of any kind, and the placeholder `Selection` standing in for it carries a
/// 1x1 rectangle at the origin. Anything that wants to place UI clear of the recorded window,
/// which today is the toolbar and tomorrow may be the ink bake (DRAGON-736), has nothing.
///
/// # Why the SIZE is a sound key and not a guess
///
/// The portal builds a window stream's `size` from `info.geometry`, the cosmic toplevel-info
/// protocol's per-output geometry, taking the entry with the greatest area. Our own
/// `platform::compositor::list_toplevels` reads THE SAME FIELD for the same toplevels. So the
/// two numbers are the same protocol value carried by two routes rather than two measurements
/// that happen to agree, and the match is exact by construction. Confirmed on the dev box:
/// three toplevels listed at 1689x1384, 1694x1384 and 1689x1384, and the portal window
/// recording of the middle one negotiated 1694x1384.
///
/// # The rules, and what each is protecting
///
/// * A size of `None`, or a zero extent, resolves NOTHING. The portal writes `(0, 0)` when it
///   could not find the toplevel's info at all, and a zero-extent rect would anchor UI against
///   a rectangle nobody can see.
/// * ONE candidate of that exact size wins.
/// * SEVERAL candidates of that size fall to the one that is ACTIVATED, and only when exactly
///   one of them is. Identical window sizes are ordinary rather than exotic: the dev box's own
///   desktop has two toplevels at 1689x1384 side by side. The activation term is a prior and
///   not a proof, so it is deliberately last and deliberately requires uniqueness too. Its
///   worst case is anchoring beside a same-sized neighbour, which is no worse than the
///   centred fallback that anchoring replaces.
/// * Anything else resolves NOTHING, and the caller keeps whatever it did before. **A miss is
///   a first-class outcome here**, not a degraded one: the honest answer to "which of these
///   two identical windows did the user pick" is that we do not know.
///
/// This is deliberately NOT the move `PortalWindowDeco::apply` makes for a wallpaper crop,
/// where a synthetic anchor centred on the largest output stands in for a position. That crop
/// has no truth to preserve and says so. A place-the-UI-clear-of-the-window anchor does, and
/// inventing one would put the toolbar somewhere the user cannot account for.
// Its production caller is the Linux portal grant handler; compiled into every test build so
// the decision is proven on any host (the house pattern).
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn toplevel_for_grant_size(
    candidates: &[(crate::platform::compositor::WinRect, bool)],
    size: Option<(i32, i32)>,
) -> Option<crate::platform::compositor::WinRect> {
    let same = same_size_candidates(candidates, size);
    if same.len() == 1 {
        return Some(candidates[same[0]].0);
    }
    unique_activated(candidates, &same).map(|i| candidates[i].0)
}

/// **Pure**, unit-tested: the INDICES of the candidates whose size is exactly the granted
/// stream's, in list order (DRAGON-737).
///
/// The first rung of [`toplevel_for_grant_size`], split out because DRAGON-737 needed the
/// TIE as a value rather than as a refusal: two toplevels of one size is the case the pixel
/// evidence exists to settle, and it has to be handed the list of who tied.
///
/// A size of `None`, or a zero or negative extent, matches NOTHING. The portal writes
/// `(0, 0)` when it could not find the toplevel's info at all.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn same_size_candidates(
    candidates: &[(crate::platform::compositor::WinRect, bool)],
    size: Option<(i32, i32)>,
) -> Vec<usize> {
    let Some((sw, sh)) = size else { return Vec::new() };
    if sw <= 0 || sh <= 0 {
        return Vec::new();
    }
    candidates
        .iter()
        .enumerate()
        .filter(|(_, ((_, _, w, h), _))| *w == sw && *h == sh)
        .map(|(i, _)| i)
        .collect()
}

/// **Pure**, unit-tested: the one ACTIVATED candidate among `same`, when exactly one of them
/// is (DRAGON-737, split out of [`toplevel_for_grant_size`] unchanged).
///
/// The last rung of the ladder, and a PRIOR rather than a proof, which is why it insists on
/// uniqueness: two activated candidates say no more than none do. It stays LAST, now behind
/// the pixel evidence as well, because a picture of the window is a fact and "this one had
/// focus a moment ago" is a guess.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn unique_activated(
    candidates: &[(crate::platform::compositor::WinRect, bool)],
    same: &[usize],
) -> Option<usize> {
    let mut it = same.iter().copied().filter(|i| candidates[*i].1);
    let first = it.next()?;
    it.next().is_none().then_some(first)
}

// -- The pixel tie-break (DRAGON-737) -----------------------------------------------------
//
// TWO ROUTES ARE CLOSED HERE, and both were investigated to a primary source. Do not
// reopen either.
//
// * COMPOSITOR GEOMETRY off COSMIC. `zcosmic_toplevel_info_v1` is the only protocol that
//   carries a toplevel's geometry, which `platform::backend::WaylandProtocols` already says
//   in its own docs; `ext_foreign_toplevel_list_v1` carries identity and nothing else, and
//   core Wayland never tells a client where a window is. That is why
//   `platform::window_rect` answers `None` on Linux.
// * THE POINTER OUT OF PIPEWIRE CURSOR METADATA. COSMIC, GNOME and KDE all gate a WINDOW
//   stream's cursor metadata on pointer FOCUS, so it goes blank exactly while the user is
//   drawing on our own surface. KDE made that deliberate in 2024 (commit `b452a5a5fb`,
//   `BUG: 487294`). And the obvious alternative is closed by the interface itself: the
//   ScreenCast spec says the stream `position` key is "Only available for monitor streams"
//   (`/usr/share/dbus-1/interfaces/org.freedesktop.portal.ScreenCast.xml`).
//
// What IS available is the picture. The portal hands us a node whose frames are the
// recorded window's own buffer, and `screenshot::windows` can grab any toplevel by
// identifier. Comparing those two answers "which window did the user pick" with evidence.

/// The downscale a frame fingerprint is taken at: nine columns by eight rows, so the 64 bits
/// below are exactly one per horizontal neighbour pair.
#[cfg(any(target_os = "linux", test))]
const HASH_W: usize = 9;
/// See [`HASH_W`].
#[cfg(any(target_os = "linux", test))]
const HASH_H: usize = 8;

/// The most a granted frame may differ from the toplevel grab it names, in differing bits of
/// 64. Deliberately tight: this guards the case where the frame corresponds to NONE of the
/// candidates (a window that repainted between the two grabs, a route that letterboxes), and
/// a refusal there is the honest answer.
#[cfg(any(target_os = "linux", test))]
const MATCH_MAX_BITS: u32 = 16;

/// How much CLEARER the winner has to be than the runner-up, in bits of 64. Two genuinely
/// different windows sit tens of bits apart and one window seen twice sits a handful apart,
/// so this separates "obvious" from "arguable" with room to spare, and refuses the arguable.
#[cfg(any(target_os = "linux", test))]
const MATCH_MARGIN_BITS: u32 = 8;

/// **Pure**, unit-tested: a 64-bit DIFFERENCE hash of one frame (DRAGON-737).
///
/// A difference hash rather than pixel equality, because the two pictures being compared come
/// from two different capture routes: the PipeWire node delivers the portal's negotiated
/// buffer and `screenshot::windows` runs its own screencopy, so they can differ in scale, in
/// how alpha is carried, in whether the pointer is baked in, and by whatever the window
/// repainted in between. Equality would fail on every real pair. What survives all of that is
/// the coarse ARRANGEMENT of light and dark, which is what this measures.
///
/// The image is reduced to a [`HASH_W`] by [`HASH_H`] grid of average luminance and one bit
/// is set per cell that is brighter than its right-hand neighbour. Comparing NEIGHBOURS
/// rather than an overall mean is what makes it survive a whole-frame brightness or gamma
/// difference between the routes: a constant offset moves both cells and changes no bit.
///
/// Each cell samples at most 8 by 8 source pixels on a fixed stride rather than averaging
/// every pixel it covers. That bounds the whole hash at about 4600 samples whatever the frame
/// size, which matters because this runs on the UI thread between the portal dialog closing
/// and the countdown starting, and a 4K frame has eight million pixels. The stride is fixed,
/// so the function stays deterministic.
///
/// A zero-extent image hashes to 0, which no real frame can be within [`MATCH_MAX_BITS`] of
/// unless it is nearly uniform, and a uniform window cannot be told from another one anyway.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn frame_hash(img: &image::RgbaImage) -> u64 {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return 0;
    }
    let buf = img.as_raw();
    let stride = w as usize * 4;
    let mut cell = [[0f32; HASH_W]; HASH_H];
    for (cy, row) in cell.iter_mut().enumerate() {
        let y0 = (cy as u32 * h) / HASH_H as u32;
        let y1 = ((((cy + 1) as u32 * h) / HASH_H as u32).max(y0 + 1)).min(h);
        let ystep = ((y1 - y0).div_ceil(8)).max(1) as usize;
        for (cx, out) in row.iter_mut().enumerate() {
            let x0 = (cx as u32 * w) / HASH_W as u32;
            let x1 = ((((cx + 1) as u32 * w) / HASH_W as u32).max(x0 + 1)).min(w);
            let xstep = ((x1 - x0).div_ceil(8)).max(1) as usize;
            let mut sum = 0f32;
            let mut n = 0u32;
            for y in (y0 as usize..y1 as usize).step_by(ystep) {
                let base = y * stride;
                for x in (x0 as usize..x1 as usize).step_by(xstep) {
                    let i = base + x * 4;
                    // Rec. 601 luma. The alpha channel is deliberately ignored: for an opaque
                    // pixel the two routes agree whether or not either premultiplied, and a
                    // translucent one is a small minority of any real window.
                    sum += 0.299 * buf[i] as f32
                        + 0.587 * buf[i + 1] as f32
                        + 0.114 * buf[i + 2] as f32;
                    n += 1;
                }
            }
            *out = if n == 0 { 0.0 } else { sum / n as f32 };
        }
    }
    let mut bits = 0u64;
    for (cy, row) in cell.iter().enumerate() {
        for cx in 0..HASH_W - 1 {
            if row[cx] > row[cx + 1] {
                bits |= 1u64 << (cy * (HASH_W - 1) + cx);
            }
        }
    }
    bits
}

/// **Pure**, unit-tested: which candidate fingerprint the granted frame names, when it names
/// one at all (DRAGON-737).
///
/// Two conditions, and BOTH have to hold:
///
/// * the winner is within [`MATCH_MAX_BITS`] of the target, so a frame that looks like none
///   of the candidates picks none of them;
/// * every other candidate is at least [`MATCH_MARGIN_BITS`] further away, so two windows
///   showing the same thing pick neither.
///
/// **A refusal is a first-class outcome**, exactly as it is for [`toplevel_for_grant_size`].
/// The caller falls back to the size and activation ladder, and a session that cannot resolve
/// its window at all is a defined mode rather than a wrong answer (DRAGON-738).
///
/// A single candidate has no runner-up, so only the first condition applies. The caller never
/// asks with one, but the rule has to be total.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn closest_frame(target: u64, candidates: &[u64]) -> Option<usize> {
    let mut best: Option<(usize, u32)> = None;
    let mut runner_up: Option<u32> = None;
    for (i, c) in candidates.iter().enumerate() {
        let d = (target ^ *c).count_ones();
        match best {
            Some((_, bd)) if d < bd => {
                runner_up = Some(bd);
                best = Some((i, d));
            }
            Some(_) => {
                if runner_up.is_none_or(|r| d < r) {
                    runner_up = Some(d);
                }
            }
            None => best = Some((i, d)),
        }
    }
    let (i, d) = best?;
    if d > MATCH_MAX_BITS {
        return None;
    }
    match runner_up {
        Some(r) if r < d + MATCH_MARGIN_BITS => None,
        _ => Some(i),
    }
}

/// How long the granted node gets to deliver ONE frame for the tie-break.
///
/// Short on purpose. This runs on the UI thread between the portal dialog closing and the
/// countdown starting, so the whole of it is time the user spends looking at nothing; the
/// five second default `pipewire::grab_frame` carries would read as a hang. It still has to
/// cover a format negotiation and a frame, so it is a WEDGE detector rather than a race.
/// Expiring costs nothing but the fallback.
///
/// The debug line below prints the elapsed time on every run, so the first real tie on a real
/// desktop says whether this number is right. Move it on that measurement, not on a guess.
#[cfg(target_os = "linux")]
const TIE_BREAK_FRAME_BUDGET: std::time::Duration = std::time::Duration::from_millis(1200);

/// The most tied candidates worth grabbing, because each one is a full-resolution screencopy.
///
/// Beyond a handful of identically sized windows the margin rule is unlikely to find a clear
/// winner anyway, so the cost would be paid for a refusal. Two is the case this exists for.
#[cfg(target_os = "linux")]
const TIE_BREAK_MAX_CANDIDATES: usize = 6;

/// Settle a SIZE tie by looking at the pictures (DRAGON-737): the index into `ids` of the
/// toplevel the granted stream is actually showing, or `None`.
///
/// The effectful half of the tie-break. The DECISIONS are [`frame_hash`] and
/// [`closest_frame`], both pure and both proven by the Linux gate; this only collects the two
/// pictures they compare and reports what happened.
///
/// It runs ONCE, at grant time, and only when the size key tied. Never on a unique match,
/// never per frame, and never during a recording: everything it calls blocks.
///
/// # What it costs, and where that is affordable
///
/// One bounded PipeWire frame ([`TIE_BREAK_FRAME_BUDGET`]) plus one `screenshot::windows`
/// connection and one full-resolution screencopy per tied candidate. On the dev box the
/// enumeration this sits beside was measured at 167ms and each window grab is of the same
/// order, so a two-way tie should cost a few hundred milliseconds and the worst case is
/// bounded near one and a half seconds. It is affordable HERE for exactly the reason
/// `on_pipewire_cast_ready`'s existing `list_toplevels` call is: the user has just dismissed
/// the portal dialog and what follows is a countdown or a multi-second worker warmup.
///
/// # Every failure is a refusal, and refusing is free
///
/// No probe fd, too many candidates, no frame inside the budget, or ANY candidate that did
/// not grab: all answer `None` and the caller keeps the size-and-activation ladder it always
/// had. A missing grab has to refuse rather than compare the rest, because dropping a
/// candidate from the field would let the margin rule pass on a runner-up that was never
/// measured.
///
/// # The one thing to watch on a real desktop
///
/// This connects a consumer to the granted node and then disconnects it, BEFORE the recording
/// worker connects its own. That is meant to be a non-event: the node belongs to the portal
/// SESSION, which stays open (we still hold its first fd), and consumers coming and going is
/// how every screen-share client reconnects. If a compositor ever tore the node down with its
/// last consumer, the symptom would be a recording with no video that only ever happens on a
/// tie, and the "pixel tie-break over N same-size toplevels" line below is what says the
/// tie-break ran at all.
#[cfg(target_os = "linux")]
fn resolve_grant_by_pixels(
    probe_fd: Option<std::os::fd::OwnedFd>,
    node_id: u32,
    ids: &[String],
    tied: &[usize],
) -> Option<usize> {
    let Some(probe_fd) = probe_fd else {
        log::debug!(
            "portal window rect: {} toplevels tie on the granted size and this grant carries \
             no second PipeWire remote, so there is no frame to compare them against",
            tied.len()
        );
        return None;
    };
    if tied.len() > TIE_BREAK_MAX_CANDIDATES {
        log::debug!(
            "portal window rect: {} toplevels tie on the granted size, past the {} the pixel \
             tie-break will grab; keeping the size and activation keys",
            tied.len(),
            TIE_BREAK_MAX_CANDIDATES
        );
        return None;
    }
    let started = std::time::Instant::now();
    let Some(frame) = crate::platform::pipewire::grab_frame_within(
        probe_fd,
        node_id,
        None,
        TIE_BREAK_FRAME_BUDGET,
    ) else {
        log::debug!(
            "portal window rect: the granted node delivered no frame within {:?}, so the {} \
             tied toplevels cannot be told apart by pixels",
            TIE_BREAK_FRAME_BUDGET,
            tied.len()
        );
        return None;
    };
    let target = frame_hash(&frame);
    // Titles never reach the log (`diag`'s privacy rule) and neither does the picture. What
    // is recorded below is counts, geometry and Hamming distances.
    let want: Vec<String> = tied.iter().map(|i| ids[*i].clone()).collect();
    // ONE toplevel can appear in the enumeration more than once: `list_toplevels` emits an
    // entry per OUTPUT a window has geometry on, all carrying the same identifier. Two tied
    // entries that are the same window would grab the same pixels twice and refuse on the
    // margin anyway, so refuse first and skip the screencopies.
    if want.iter().collect::<std::collections::HashSet<_>>().len() != want.len() {
        log::debug!(
            "portal window rect: the {} tied entries are not {} distinct toplevels (a window \
             with geometry on more than one output is listed once per output), so the pixel \
             tie-break has nothing to separate",
            want.len(),
            want.len()
        );
        return None;
    }
    let grabs = crate::screenshot::windows(&want);
    let mut hashes: Vec<u64> = Vec::with_capacity(want.len());
    for id in &want {
        let Some(img) = grabs.get(id) else {
            log::debug!(
                "portal window rect: {} of {} tied toplevels answered a screencopy, so the \
                 pixel tie-break has an incomplete field and refuses",
                grabs.len(),
                want.len()
            );
            return None;
        };
        hashes.push(frame_hash(img));
    }
    let picked = closest_frame(target, &hashes);
    let dists: Vec<u32> = hashes.iter().map(|h| (target ^ *h).count_ones()).collect();
    log::debug!(
        "portal window rect: pixel tie-break over {} same-size toplevels took {:?}; \
         distances {:?} of 64 (accept at most {}, margin {}), picked {:?}",
        want.len(),
        started.elapsed(),
        dists,
        MATCH_MAX_BITS,
        MATCH_MARGIN_BITS,
        picked
    );
    picked.map(|k| tied[k])
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

    /// DRAGON-718: the ScreenCast probe as a THREE-valued answer — `None` until it lands,
    /// then whether the portal is reachable.
    ///
    /// `pipewire_available` alone cannot express "not yet", and its initial `false` reads
    /// as a decided no. Every other caller genuinely wants "can we use the portal right
    /// now", which is what that bool means; only the launch seed has to tell the two apart,
    /// and conflating them is what made a pinned `--monitor` launch ignore the setting.
    #[cfg(target_os = "linux")]
    pub(super) fn pipewire_probe(&self) -> Option<bool> {
        self.pipewire_probed.then_some(self.pipewire_available)
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
    /// at-commit portal request). The caller says which origin it is, and the
    /// token-replay policy follows from it. Three kinds of caller:
    ///
    /// * the mode/kind toggles (`InSession`);
    /// * the lab/flatpak fallback seed (`LaunchSeed`, in `surfaces.rs`);
    /// * a pinned `--monitor` / `--window` launch, once the ScreenCast probe has answered
    ///   (`LaunchSeed`, from `App::resolve_portal_seed_hold`).
    ///
    /// That third caller is DRAGON-718 and it is NEW. This doc claimed it for a year
    /// before it existed: a pinned launch writes `mode` straight into the struct at init
    /// and never runs `SetMode`, so outside the fallback path nothing dispatched here at
    /// startup at all, and `record_backend = "portal"` was silently ignored for exactly
    /// those launches.
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
        // DRAGON-732: the portal request is OVER, whichever way it went, so the launch seed
        // hands picking back to the ordinary overlay lifecycle. It matters on the cancel
        // arms below: those set `Mode::Region` and restore the interactive overlays, and a
        // monitor hotplugged after that must still get one. Leaving the seed on `Portal`
        // would silently withhold it. Every path from here on behaves exactly as it did
        // before DRAGON-718.
        #[cfg(target_os = "linux")]
        {
            self.portal_seed = crate::app::capture_flow::PortalSeed::Native;
        }
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

        // DRAGON-735: ask the COMPOSITOR where that window is, since the portal will not say.
        // Only for a window grant, and only here, because this is the one moment that holds
        // the granted size. `toplevel_for_grant_size` carries the whole argument for why the
        // size is a sound key; what this half owes is the enumeration and an honest log line.
        //
        // It is an inline blocking call on the UI thread, measured at 167ms on the dev box
        // (`list_toplevels` runs a settle loop over its own throwaway Wayland connection).
        // That is affordable HERE and nowhere else on this path: the user has just dismissed
        // the portal dialog and what follows is a countdown or a multi-second worker warmup,
        // so the wait is covered. `capture_flow::resolve_immediate_target` already calls the
        // same function the same way. Do not move it into the per-frame or per-view path.
        #[cfg(target_os = "linux")]
        if pending.source_key == "window" {
            let listed = crate::platform::compositor::list_toplevels();
            let mut candidates: Vec<(crate::platform::compositor::WinRect, bool)> = Vec::new();
            let mut ids: Vec<String> = Vec::new();
            for t in listed.values().flatten() {
                candidates.push((t.rect, t.active));
                ids.push(t.id.clone());
            }
            // DRAGON-737: when the size key TIES, look at the pictures before falling back to
            // the activation prior. The pixel evidence is deliberately ahead of activation in
            // the ladder: a frame of the window is a fact, and "this one had focus a moment
            // ago" is a guess that the owner's two same-size windows defeat every time. The
            // probe fd is consumed either way, and dropping it unused only closes a socket.
            let tied = same_size_candidates(&candidates, stream.size);
            let by_pixels = if tied.len() > 1 {
                resolve_grant_by_pixels(session.probe_fd, stream.node_id, &ids, &tied)
            } else {
                None
            };
            self.portal_window_rect = match by_pixels {
                Some(i) => Some(candidates[i].0),
                None => toplevel_for_grant_size(&candidates, stream.size),
            };
            // Geometry and counts only: a toplevel's TITLE is the user's content and never
            // reaches the log (`diag`'s privacy rule).
            match self.portal_window_rect {
                Some(r) => log::debug!(
                    "portal window rect: resolved to {r:?} from the granted size {:?} \
                     ({} toplevel(s) listed, {} of them that size, {})",
                    stream.size,
                    candidates.len(),
                    tied.len(),
                    if by_pixels.is_some() { "settled by pixels" } else { "settled by the size and activation keys" }
                ),
                None => log::debug!(
                    "portal window rect: unresolved for the granted size {:?} \
                     ({} toplevel(s) listed, {} of them that size, and the pixels did not \
                     separate them); UI that would move clear of the window keeps its \
                     centred placement, and the ink session falls to its snapshot tier",
                    stream.size,
                    candidates.len(),
                    tied.len()
                ),
            }
        }

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

/// DRAGON-735: recovering a portal window grant's on-screen rectangle from the compositor's
/// own toplevel list. Its own module because it pins ONE function and one bug, the recording
/// toolbar parking itself on top of the window it is recording.
#[cfg(test)]
mod grant_window_rect_tests {
    use super::toplevel_for_grant_size;
    use crate::platform::compositor::WinRect;

    /// The dev box's actual desktop, read out of `debug test selftest` on 2026-08-20: three
    /// toplevels tiled across the 5120x1440 ultrawide, TWO OF THEM THE SAME SIZE. That is the
    /// ordinary case, not the awkward one, and it is why uniqueness is a rule rather than an
    /// afterthought. The middle one is the window the owner recorded, and the portal's stream
    /// reported exactly its 1694x1384.
    const DESKTOP: [(WinRect, bool); 3] = [
        ((12, 44, 1689, 1384), false),
        ((1713, 44, 1694, 1384), true),
        ((3419, 44, 1689, 1384), false),
    ];

    #[test]
    fn a_uniquely_sized_window_resolves_exactly() {
        assert_eq!(
            toplevel_for_grant_size(&DESKTOP, Some((1694, 1384))),
            Some((1713, 44, 1694, 1384))
        );
    }

    /// Two windows of the same size, one of them focused: the activation prior breaks the tie.
    /// It is a prior and not a proof, so its worst case is anchoring beside a same-sized
    /// neighbour, which is no worse than the centred placement it replaces.
    #[test]
    fn equal_sizes_fall_to_the_one_that_is_activated() {
        let tied: [(WinRect, bool); 3] = [
            ((12, 44, 1689, 1384), false),
            ((1713, 44, 1694, 1384), false),
            ((3419, 44, 1689, 1384), true),
        ];
        assert_eq!(
            toplevel_for_grant_size(&tied, Some((1689, 1384))),
            Some((3419, 44, 1689, 1384))
        );
    }

    /// Two of a size and NEITHER activated, or BOTH: we genuinely do not know which the user
    /// picked, and saying so is the answer. The caller keeps its centred placement.
    #[test]
    fn an_ambiguous_size_resolves_nothing() {
        assert_eq!(toplevel_for_grant_size(&DESKTOP, Some((1689, 1384))), None);
        let both: [(WinRect, bool); 2] =
            [((0, 0, 800, 600), true), ((900, 0, 800, 600), true)];
        assert_eq!(toplevel_for_grant_size(&both, Some((800, 600))), None);
    }

    /// A size nothing on screen carries: the window moved to another workspace, closed, or
    /// resized between the grant and the enumeration.
    #[test]
    fn a_size_matching_no_toplevel_resolves_nothing() {
        assert_eq!(toplevel_for_grant_size(&DESKTOP, Some((640, 480))), None);
        assert_eq!(toplevel_for_grant_size(&[], Some((1694, 1384))), None);
    }

    /// The portal writes `(0, 0)` when it could not find the toplevel's info at all, and a
    /// MONITOR grant reaches this function never. Neither may anchor anything.
    #[test]
    fn an_absent_or_zero_size_resolves_nothing() {
        assert_eq!(toplevel_for_grant_size(&DESKTOP, None), None);
        assert_eq!(toplevel_for_grant_size(&DESKTOP, Some((0, 0))), None);
        assert_eq!(toplevel_for_grant_size(&DESKTOP, Some((1694, 0))), None);
        assert_eq!(toplevel_for_grant_size(&DESKTOP, Some((-1694, -1384))), None);
        // A zero-sized toplevel can never be matched either, not even by a zero size.
        let degenerate: [(WinRect, bool); 1] = [((0, 0, 0, 0), true)];
        assert_eq!(toplevel_for_grant_size(&degenerate, Some((0, 0))), None);
    }

    /// The match is on SIZE alone, so where the window sits never influences which one is
    /// chosen. Pinned because "prefer the one nearest the origin" is the tempting tiebreak and
    /// it would be a fiction: the portal tells us nothing about position at all.
    #[test]
    fn position_never_participates_in_the_match() {
        let moved: [(WinRect, bool); 2] =
            [((-4000, -900, 1694, 1384), false), ((12, 44, 1689, 1384), false)];
        assert_eq!(
            toplevel_for_grant_size(&moved, Some((1694, 1384))),
            Some((-4000, -900, 1694, 1384))
        );
    }
}

/// DRAGON-737's two new rungs, pinned separately from the ladder they came out of.
///
/// `same_size_candidates` is the one that had to change shape: DRAGON-735 threw the TIE away
/// and DRAGON-737 needs it as a value, because the tie is exactly the case the pixel evidence
/// exists to settle. `unique_activated` came out unchanged and is pinned here so a future
/// reader can see that the ladder's last rung really is the same rule it always was.
#[cfg(test)]
mod grant_tie_rung_tests {
    use super::{same_size_candidates, unique_activated};
    use crate::platform::compositor::WinRect;

    const DESKTOP: [(WinRect, bool); 3] = [
        ((12, 44, 1689, 1384), false),
        ((1713, 44, 1694, 1384), true),
        ((3419, 44, 1689, 1384), false),
    ];

    /// The owner's actual desktop: two toplevels of one size, which is the whole ticket.
    #[test]
    fn the_tie_is_reported_as_a_list_in_enumeration_order() {
        assert_eq!(same_size_candidates(&DESKTOP, Some((1689, 1384))), vec![0, 2]);
        assert_eq!(same_size_candidates(&DESKTOP, Some((1694, 1384))), vec![1]);
    }

    /// The same refusals `toplevel_for_grant_size` has always made, now stated one rung down:
    /// no size, a zero or negative extent, or a size nothing on screen carries.
    #[test]
    fn an_absent_zero_or_unmatched_size_ties_nothing() {
        assert!(same_size_candidates(&DESKTOP, None).is_empty());
        assert!(same_size_candidates(&DESKTOP, Some((0, 0))).is_empty());
        assert!(same_size_candidates(&DESKTOP, Some((1694, 0))).is_empty());
        assert!(same_size_candidates(&DESKTOP, Some((-1694, -1384))).is_empty());
        assert!(same_size_candidates(&DESKTOP, Some((640, 480))).is_empty());
        assert!(same_size_candidates(&[], Some((1694, 1384))).is_empty());
    }

    /// The activation prior is a PRIOR: exactly one activated candidate resolves, none or
    /// several resolve nothing. Two activated windows say no more than zero do.
    #[test]
    fn activation_resolves_only_when_exactly_one_is_activated() {
        assert_eq!(unique_activated(&DESKTOP, &[0, 1, 2]), Some(1));
        assert_eq!(unique_activated(&DESKTOP, &[0, 2]), None);
        let both: [(WinRect, bool); 2] = [((0, 0, 800, 600), true), ((900, 0, 800, 600), true)];
        assert_eq!(unique_activated(&both, &[0, 1]), None);
        assert_eq!(unique_activated(&both, &[]), None);
    }
}

/// DRAGON-737's frame fingerprint. The properties pinned here are the ones the two capture
/// routes actually differ by, so each test names a way the portal's buffer and our own
/// screencopy of the same window can disagree without being different windows.
#[cfg(test)]
mod frame_hash_tests {
    use super::frame_hash;

    /// A blocky greyscale pattern defined in RELATIVE coordinates, so the same `seed` gives
    /// the same picture at any size. Blocky rather than noisy on purpose: per-pixel noise
    /// averages to flat under the downscale and would prove nothing.
    fn synth(w: u32, h: u32, seed: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(w, h, |x, y| {
            let bx = (x * 12) / w.max(1);
            let by = (y * 8) / h.max(1);
            let v = ((bx * 37 + by * 91 + seed * 53) % 251) as u8;
            image::Rgba([v, v, v, 255])
        })
    }

    fn dist(a: u64, b: u64) -> u32 {
        (a ^ b).count_ones()
    }

    /// A flat frame has no cell brighter than its neighbour, so no bit is set. Two flat
    /// windows are genuinely indistinguishable, which is the honest answer.
    #[test]
    fn a_uniform_frame_sets_no_bits() {
        let flat = image::RgbaImage::from_pixel(64, 48, image::Rgba([90, 90, 90, 255]));
        assert_eq!(frame_hash(&flat), 0);
    }

    /// The two ends of the axis this hash measures: brightness falling to the right sets
    /// every bit, rising to the right sets none.
    #[test]
    fn a_horizontal_ramp_sets_every_bit_or_none() {
        let w = 90u32;
        let rising = image::RgbaImage::from_fn(w, 32, |x, _| {
            let v = ((x * 250) / w) as u8;
            image::Rgba([v, v, v, 255])
        });
        let falling = image::RgbaImage::from_fn(w, 32, |x, _| {
            let v = (250 - (x * 250) / w) as u8;
            image::Rgba([v, v, v, 255])
        });
        assert_eq!(frame_hash(&rising), 0);
        assert_eq!(frame_hash(&falling), u64::MAX);
    }

    /// SCALE invariance. The portal negotiates its own buffer size and `screenshot::windows`
    /// runs a full-resolution screencopy, so the same window routinely arrives at two sizes.
    /// Comparing a downscale is what makes that a non-event.
    #[test]
    fn the_same_picture_at_two_sizes_hashes_the_same() {
        assert_eq!(frame_hash(&synth(300, 200, 3)), frame_hash(&synth(1500, 1000, 3)));
    }

    /// BRIGHTNESS invariance. A difference hash compares neighbours, so a constant offset
    /// across the whole frame, which is what a gamma or alpha-handling difference between two
    /// routes looks like, moves both cells and changes no bit.
    #[test]
    fn a_whole_frame_brightness_shift_changes_nothing() {
        let base = synth(300, 200, 5);
        let lifted = image::RgbaImage::from_fn(300, 200, |x, y| {
            let p = base.get_pixel(x, y).0;
            image::Rgba([
                p[0].saturating_add(20),
                p[1].saturating_add(20),
                p[2].saturating_add(20),
                255,
            ])
        });
        assert_eq!(frame_hash(&base), frame_hash(&lifted));
    }

    /// And the property that makes the whole thing worth doing: two different windows are far
    /// apart, well past the acceptance threshold, so the tie-break can tell them apart.
    #[test]
    fn two_different_pictures_are_far_apart() {
        let a = frame_hash(&synth(300, 200, 1));
        let b = frame_hash(&synth(300, 200, 2));
        assert!(
            dist(a, b) > super::MATCH_MAX_BITS,
            "two different windows hashed {} bits apart, inside the {} acceptance window",
            dist(a, b),
            super::MATCH_MAX_BITS
        );
    }

    /// Degenerate input answers rather than panicking or dividing by zero: a frame smaller
    /// than the grid still hashes, and a zero-extent one is 0.
    #[test]
    fn degenerate_sizes_answer_instead_of_panicking() {
        assert_eq!(frame_hash(&image::RgbaImage::new(0, 0)), 0);
        assert_eq!(frame_hash(&image::RgbaImage::new(0, 10)), 0);
        assert_eq!(frame_hash(&image::RgbaImage::new(10, 0)), 0);
        // 3x2 is narrower and shorter than the 9x8 grid: every cell collapses onto the same
        // few pixels, which must still produce a hash.
        let _ = frame_hash(&synth(3, 2, 7));
    }
}

/// DRAGON-737's match rule. The two thresholds are what stop the pixel evidence from being a
/// louder guess than the activation prior it overtakes, so both refusals are pinned.
#[cfg(test)]
mod closest_frame_tests {
    use super::{MATCH_MARGIN_BITS, MATCH_MAX_BITS, closest_frame};

    /// A hash `bits` away from zero, for building a field by distance.
    fn at(bits: u32) -> u64 {
        if bits == 0 { 0 } else { u64::MAX >> (64 - bits) }
    }

    #[test]
    fn a_clear_winner_is_named() {
        assert_eq!(closest_frame(0, &[at(30), at(4), at(28)]), Some(1));
    }

    /// The margin refusal: two windows showing much the same thing. This is the case that
    /// feeds DRAGON-738, where the session falls to the snapshot tier rather than guessing.
    #[test]
    fn two_similar_candidates_name_neither() {
        let near = MATCH_MARGIN_BITS - 1;
        assert_eq!(closest_frame(0, &[at(2), at(2 + near)]), None);
        assert_eq!(closest_frame(0, &[at(5), at(5)]), None);
    }

    /// The distance refusal: the frame looks like NONE of them, so it names none of them. A
    /// window that repainted between the two grabs, or a route that letterboxed, lands here.
    #[test]
    fn a_frame_matching_nothing_names_nothing() {
        assert_eq!(closest_frame(0, &[at(MATCH_MAX_BITS + 1), at(60)]), None);
    }

    /// Totality. The caller only ever asks with a real tie, but the rule answers anyway.
    #[test]
    fn an_empty_or_single_field_answers_honestly() {
        assert_eq!(closest_frame(0, &[]), None);
        assert_eq!(closest_frame(0, &[at(3)]), Some(0));
        assert_eq!(closest_frame(0, &[at(MATCH_MAX_BITS + 1)]), None);
    }

    /// The winner is the CLOSEST, whatever order the field arrives in: the enumeration order
    /// of `list_toplevels` is not a preference.
    #[test]
    fn enumeration_order_never_decides() {
        assert_eq!(closest_frame(0, &[at(3), at(40)]), Some(0));
        assert_eq!(closest_frame(0, &[at(40), at(3)]), Some(1));
    }
}

/// The mechanism end to end, over the pure half: two windows of the SAME SIZE, a granted
/// frame that is one of them seen through the other capture route, and the right answer.
///
/// This is the test that would have caught DRAGON-737 in the first place. The frame is
/// deliberately not a copy: it is rescaled and brightness-shifted, which is what the portal's
/// buffer and our own screencopy of one window really differ by.
#[cfg(test)]
mod grant_tie_pixel_tests {
    use super::{closest_frame, frame_hash, same_size_candidates};
    use crate::platform::compositor::WinRect;

    fn synth(w: u32, h: u32, seed: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(w, h, |x, y| {
            let bx = (x * 12) / w.max(1);
            let by = (y * 8) / h.max(1);
            let v = ((bx * 37 + by * 91 + seed * 53) % 251) as u8;
            image::Rgba([v, v, v, 255])
        })
    }

    #[test]
    fn the_pixels_pick_the_window_the_size_key_could_not() {
        // The owner's desktop: two 1689x1384 toplevels, neither activated.
        let desktop: [(WinRect, bool); 3] = [
            ((12, 44, 1689, 1384), false),
            ((1713, 44, 1694, 1384), false),
            ((3419, 44, 1689, 1384), false),
        ];
        let tied = same_size_candidates(&desktop, Some((1689, 1384)));
        assert_eq!(tied, vec![0, 2]);

        // Our own screencopy of each tied candidate, full resolution.
        let grabs = [synth(1689, 1384, 1), synth(1689, 1384, 2)];
        let hashes: Vec<u64> = grabs.iter().map(frame_hash).collect();

        // The portal's own buffer of the SECOND one: a different size, and lifted 25 levels.
        // Built ONCE outside the closure: `from_fn` calls it per pixel, and rebuilding a
        // 584k-pixel image inside it turns a test into a hang (this cost a run).
        let base = synth(844, 692, 2);
        let granted = image::RgbaImage::from_fn(844, 692, |x, y| {
            let p = base.get_pixel(x, y).0;
            image::Rgba([
                p[0].saturating_add(25),
                p[1].saturating_add(25),
                p[2].saturating_add(25),
                255,
            ])
        });

        let picked = closest_frame(frame_hash(&granted), &hashes).expect("a clear winner");
        assert_eq!(tied[picked], 2, "the pixels must name the toplevel at x=3419");
    }

    /// Two windows showing the same thing: the pixels refuse too, and the session is left
    /// with no window origin at all. DRAGON-738 is what turns that into a defined mode
    /// instead of ink that silently never arrives.
    #[test]
    fn two_identical_windows_still_resolve_nothing() {
        let grabs = [synth(1689, 1384, 4), synth(1689, 1384, 4)];
        let hashes: Vec<u64> = grabs.iter().map(frame_hash).collect();
        let granted = synth(844, 692, 4);
        assert_eq!(closest_frame(frame_hash(&granted), &hashes), None);
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
