use super::super::super::*;

// ── Toolbar group-width / sizing constants ────────────────────────────────────
// These are the single source of truth for group footprints shared between
// `toolbar_layout` (placement + input-zone) and `capture_button_layer` (widget
// tree). Adding/removing a group automatically re-centres the toolbar with no
// hand-tuned total.

/// Countdown chip width: icon + NN + ✕.
pub(super) const CHIP_W: f32 = 80.0;
/// Recording group width: pause/resume button + stop chip (`MMM:SS`) + cancel ✕.
pub(super) const REC_W: f32 = 186.0;
/// Fixed width used for both the vertical-stack mode and the kind/timer group
/// width in `capture_button_layer`.
pub(super) const V_W: f32 = 148.0;
/// Bottom-of-screen clearance between toolbar and output edge. `pub(in crate::app::overlay)`
/// so the window picker can reserve this band (plus the toolbar height) below its grid.
pub(in crate::app::overlay) const BOTTOM_MARGIN: f32 = 32.0;
/// Gap between toolbar groups.
pub(super) const GAP: f32 = 8.0;
/// Kind trio (scanner/photo/video) + delay chip group width.
pub(super) const W_KIND: f32 = 188.0;
/// The kind group in scanner kind: the trio alone (no delay chip).
pub(super) const W_KIND_SCANNER: f32 = 130.0;
/// Region/window/monitor mode group width.
pub(super) const W_MODE: f32 = 130.0;
/// The scanner's refresh group (DRAGON-460): one 40pt button plus the group padding,
/// standing in the mode group's slot while the scanner kind is active.
pub(super) const W_SCAN: f32 = 46.0;
/// Mic + system audio group width (two buttons; video mode only).
pub(super) const W_AUDIO: f32 = 88.0;
/// Live-annotation group width (DRAGON-691): pointer + marker + magic marker + erase.
///
/// FOUR buttons since the amendment made the control a three-way tool selector, so one
/// button's worth (42pt, the step between [`W_AUDIO`]'s two and [`W_MODE`]'s three) wider than
/// the mode group. It has to be listed here as well as drawn, and the DRAGON-576 note further
/// down says why: a group that is drawn but missing from this list leaves the PLACED rect
/// short of what is on screen, so the bar sits off centre and its drag clamp lets it hang off
/// the output.
pub(super) const W_INK: f32 = 172.0;
/// Settings + close group width.
pub(super) const W_UTIL: f32 = 88.0;

impl App {
    /// Where the toolbar sits on output `o`: its rect and whether it lays out
    /// horizontally. Anchored beside a region selection, otherwise centred on the
    /// bottom of every screen. Shared by the renderer and the countdown
    /// input-zone setup.
    /// DRAGON-735: the PORTAL-granted window's rect, in the shape [`App::toolbar_layout`]
    /// anchors on, or `None` when this session did not take that route or the window could not
    /// be named.
    ///
    /// Guarded on the same predicate the ink crop uses ([`crate::selection::target_unlocated`])
    /// so the two readers of that placeholder can never disagree about what it means: a WINDOW
    /// recording whose target our own picker did not choose. A native window pick has its rect
    /// on the selection and never reaches here.
    ///
    /// Reading only, and a field read at that. `toolbar_layout` runs on every view of every
    /// overlay; the compositor enumeration that FILLS the field happens once, at grant time.
    ///
    /// Linux-only in effect, since the portal only exists there. The other platforms answer a
    /// constant `None` and their toolbar placement is byte-identical to before.
    ///
    /// DRAGON-740: `pub(in crate::app)` because it has a SECOND reader now,
    /// [`App::recording_toolbar_oversized`], which decides whether the bar is worth showing
    /// at all. Those two must read ONE answer. While this was private the placement half knew
    /// where the recorded window was and the visibility half did not, so the bar was docked
    /// perfectly beside a window it was then declared unable to fit beside, and vanished.
    pub(in crate::app) fn portal_window_anchor(&self) -> Option<(i32, i32, u32, u32)> {
        let sel = self.pending.as_ref()?;
        if !crate::selection::target_unlocated(self.mode == Mode::Window, sel) {
            return None;
        }
        #[cfg(target_os = "linux")]
        {
            let (x, y, w, h) = self.portal_window_rect?;
            (w > 0 && h > 0).then_some((x, y, w as u32, h as u32))
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    pub(in crate::app) fn toolbar_layout(&self, o: &OutputState) -> Option<(cosmic::iced::Rectangle, bool)> {
        // Hidden only while actively drawing/resizing/moving a region.
        if self.mode == Mode::Region && self.region_dragging {
            return None;
        }
        // "Active" = a countdown or a recording in progress; only the chip shows.
        let active = self.countdown.is_some() || self.recording.is_some();
        // While a window capture is active, anchor the toolbar beside the window's
        // rect (as if it were a region selection) so the chip sits next to it — and
        // outside the recorded crop — instead of at the screen bottom. Region
        // captures keep their region anchor; monitor captures and the window picker
        // stay centred at the bottom.
        //
        // DRAGON-735: the rect comes from TWO places now, because a window picked in the
        // PORTAL's dialog leaves no rect on the selection at all. `window_id` is what our own
        // picker fills, and the placeholder standing in for a portal pick has neither that nor
        // a real rectangle (see `selection::target_unlocated`), so this used to answer `None`,
        // fall through `region_sel`, and bottom-centre the bar in the middle of the screen,
        // which is exactly where a recorded window usually is. The owner's words: "our toolbar
        // is supposed to move out of the way if possible, but its being pulled into window
        // capture instead of moving to the left/right which have open area."
        // `portal_window_anchor` is the compositor's answer for that case and `None` whenever
        // it could not name the window, so the centred placement stays the honest fallback.
        let window_rect = self
            .pending
            .as_ref()
            .filter(|_| active)
            .filter(|s| s.window_id.is_some())
            .map(|s| (s.x, s.y, s.width, s.height))
            .or_else(|| if active { self.portal_window_anchor() } else { None });
        // DRAGON-663: a COLOUR PICKER session declines the region anchor. `self.region` is
        // PERSISTED, so a picker launch carries the last rectangle the user drew in some
        // earlier capture session, and it never draws it. Anchoring to it would park the
        // countdown chip against an invisible box, in a corner the user has no way to
        // account for. A pick has no selection at all, so it takes the same centred-bottom
        // anchor monitor captures and the window picker take.
        //
        // The term is exact rather than broad: a picker session reaches this function ONLY
        // through the countdown view (its own overlay draws no toolbar), so nothing that
        // existed before this ticket changes.
        let region_sel = window_rect.or_else(|| {
            if self.mode == Mode::Region && !self.color_picking() {
                self.normalized_region()
            } else {
                None
            }
        });

        // (width, horizontal height, vertical height) of each group actually shown —
        // matches `capture_button_layer`. The kind group is the only one whose height
        // differs when stacked (the delay chip wraps below the segment trio).
        let groups: Vec<(f32, f32, f32)> = if active {
            // Active: the chip group (stands in for the taller shutter), plus the
            // audio group for a video capture.
            let chip_w = if self.recording.is_some() {
                REC_W
            } else {
                CHIP_W
            };
            let mut g = Vec::new();
            // DRAGON-691: the live-annotation group, in the SAME order `capture_button_layer`
            // builds it (first while a recording is live), and on the same condition. The
            // widths list and the widget tree have to agree group for group; see [`W_INK`].
            if self.ink_available() {
                g.push((W_INK, GROUP_H_BASE, GROUP_H_BASE));
            }
            if self.kind == Kind::Video {
                g.push((W_AUDIO, GROUP_H_BASE, GROUP_H_BASE));
            }
            g.push((chip_w, GROUP_H_BASE, GROUP_H_BASE));
            g
        } else {
            let scanner = self.kind == Kind::Scanner;
            // Stacked, the trio + chip wrap into two rows: one extra row of content
            // (button height + the 4px column gap) on top of the base group height.
            let kind_v_h = if scanner {
                GROUP_H_BASE
            } else {
                GROUP_H_BASE + (GROUP_H_BASE - 2.0 * GROUP_PAD) + 4.0
            };
            let mut g = vec![(
                if scanner { W_KIND_SCANNER } else { W_KIND },
                GROUP_H_BASE,
                kind_v_h,
            )];
            if self.kind == Kind::Video {
                g.push((W_AUDIO, GROUP_H_BASE, GROUP_H_BASE));
            }
            // DRAGON-576: the selector slot is occupied in EVERY kind (DRAGON-460),
            // the mode group normally, the scanner's refresh group in scanner kind.
            // The refresh group used to be missing from this list, so the scanner
            // toolbar's placed rect was one group short of what was drawn: the
            // bottom-centred bar sat off centre and its drag clamp let it hang off
            // the output, and the side-docked stack overflowed its clamped box.
            g.push((if scanner { W_SCAN } else { W_MODE }, GROUP_H_BASE, GROUP_H_BASE));
            g.push((W_UTIL, GROUP_H_BASE, GROUP_H_BASE));
            g
        };
        let n = groups.len() as f32;
        // Side by side (horizontal): widths add, height is the tallest group.
        let h_w = groups.iter().map(|g| g.0).sum::<f32>() + GAP * (n - 1.0);
        let h_h = groups.iter().map(|g| g.1).fold(0.0_f32, f32::max);
        // Stacked (left/right anchor): non-active groups are width-matched to V_W;
        // an active stack takes its widest group. The stacked heights add.
        let v_w = if active {
            groups.iter().map(|g| g.0).fold(0.0_f32, f32::max)
        } else {
            V_W
        };
        let v_h = groups.iter().map(|g| g.2).sum::<f32>() + GAP * (n - 1.0);

        // Centre on the bottom of this output — the placement window/monitor capture uses,
        // and (DRAGON-207) the fallback for a region-mode output that holds no selection.
        let bottom_centre = || {
            // POINTS, not `logical_size` (DRAGON-448): the group widths above are point
            // constants and the rect below becomes iced padding, so the output extent it
            // is centred/clamped against has to be the iced VIEWPORT. On a scaled Windows
            // monitor `logical_size` is `point_scale`× bigger, which centred the toolbar
            // on a screen that does not exist and dropped it off the bottom.
            let (ow, oh) = o.point_size();
            let cx = ((ow - h_w) / 2.0).max(0.0);
            let cy = (oh - h_h - BOTTOM_MARGIN).max(0.0);
            (
                cosmic::iced::Rectangle {
                    x: cx,
                    y: cy,
                    width: h_w,
                    height: h_h,
                },
                true,
            )
        };
        let (rect, horizontal) = match region_sel {
            // DRAGON-207: `placement` returns None when the region rect lands entirely on
            // ANOTHER output; that monitor still gets the toolbar, bottom-centred, instead
            // of nothing — the controls stay reachable wherever the region ends up.
            Some((x, y, w, h)) => {
                placement(o, x, y, w, h, h_w, h_h, v_w, v_h).unwrap_or_else(bottom_centre)
            }
            None => bottom_centre(),
        };
        // Apply this output's drag nudge (kept across countdown/recording so the toolbar
        // stays where it was dragged), clamped onto the output. Per-output so dragging one
        // monitor's toolbar never moves another's (DRAGON-207).
        // POINTS again (DRAGON-448): the drag nudge arrives from `DragArea` as point
        // deltas, so the clamp that keeps the dragged toolbar on screen must be the
        // viewport, not the capture extent.
        let (ow, oh) = o.point_size();
        let (offx, offy) = self.toolbar_offset.get(&o.name).copied().unwrap_or((0.0, 0.0));
        let rect = cosmic::iced::Rectangle {
            x: (rect.x + offx).clamp(0.0, (ow - rect.width).max(0.0)),
            y: (rect.y + offy).clamp(0.0, (oh - rect.height).max(0.0)),
            ..rect
        };
        // DRAGON-692: keep the bar out of a notched Mac's camera housing, LAST and over the
        // already-clamped rect. This function is the ONE place a toolbar rect is minted (the
        // view, the click-through input zone, the ink guard and
        // `recording_toolbar_oversized` all read it), so applying the rule once here is what
        // makes it hold for the bottom-centred default, the region-mode outside placement,
        // the stacked side-dock AND the drag nudge alike. After the clamp, not before, so a
        // drag cannot push the bar back under the housing the moment the rule has moved it.
        let rect = avoid_camera_housing(rect, self.toolbar_housing(o), ow, oh);
        Some((rect, horizontal))
    }

    /// The camera housing output `o` has, if any, in that overlay's own POINT space
    /// (DRAGON-692). macOS asks the real screen; every other platform has no such thing,
    /// so this is a compile-time `None` there and [`avoid_camera_housing`] is an identity.
    ///
    /// The same shape as `App::picker_top_inset`, the DRAGON-270 rule that keeps the WINDOW
    /// PICKER clear of the same band: a portable `fn` whose body branches by `cfg`, so the
    /// caller carries none.
    fn toolbar_housing(&self, o: &OutputState) -> Option<CameraHousing> {
        #[cfg(target_os = "macos")]
        {
            crate::platform::mac::notch_housing(&o.name).map(|(x, width, height)| CameraHousing {
                x: x as f32,
                width: width as f32,
                height: height as f32,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = o;
            None
        }
    }
}

/// The band at the TOP of one output that a camera housing blocks, in that overlay's POINT
/// space with the origin at its top-left (DRAGON-692). `x`/`width` are the housing's
/// horizontal span; `height` is how far down from the top edge it reaches.
///
/// Only macOS ever produces one (`platform::mac::notch_housing`), and only for a built-in
/// panel with a notch. The TYPE and the rule that consumes it are portable on purpose, so
/// the Linux gate proves the geometry on a machine that has no notch to test against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::app) struct CameraHousing {
    /// Left edge of the housing, in output-local points.
    pub x: f32,
    /// How wide the housing is.
    pub width: f32,
    /// How far down from the output's top edge it reaches.
    pub height: f32,
}

/// Move a placed toolbar `rect` out from under the camera `housing`, on an output `ow`×`oh`
/// points. Pure geometry, unit-tested below.
///
/// The order of preference, which is the owner's ("its fine if we nudge it left or right"):
///
/// 1. **No housing, or no overlap: return `rect` untouched.** This is the identity that
///    keeps Linux, Windows and every notchless Mac byte-identical, and it is also the
///    answer for the bottom-centred default placement, which never reaches the band.
/// 2. **Slide sideways into the nearer wing.** The housing has usable screen either side of
///    it (that is what `auxiliaryTop*Area` describes), so the bar right-aligns against the
///    housing's left edge or left-aligns past its right edge, whichever is the SHORTER move,
///    provided the bar fits there. It keeps its y, so it stays where the placement or the
///    drag wanted it vertically, and dragging further up just slides it along the housing's
///    side rather than teleporting it.
/// 3. **Otherwise drop below the band.** When the bar is wider than either wing, or the
///    housing was reported as the whole band because the wings read as unusable, there is no
///    sideways answer and the bar clears the housing vertically instead. Still clamped onto
///    the output, so a bar taller than its own screen degrades to the top edge rather than
///    walking off the bottom.
///
/// Ties go left, so the rule is deterministic for a bar centred exactly under the housing.
fn avoid_camera_housing(
    rect: cosmic::iced::Rectangle,
    housing: Option<CameraHousing>,
    ow: f32,
    oh: f32,
) -> cosmic::iced::Rectangle {
    let Some(h) = housing else {
        return rect;
    };
    if h.width <= 0.0 || h.height <= 0.0 {
        return rect;
    }
    let (hx0, hx1) = (h.x, h.x + h.width);
    // Clear of the band vertically, or entirely to one side of the housing already.
    if rect.y >= h.height || rect.x + rect.width <= hx0 || rect.x >= hx1 {
        return rect;
    }
    // Right-aligned against the housing's left edge, and left-aligned past its right edge.
    let (left_x, right_x) = (hx0 - rect.width, hx1);
    let beside = match (left_x >= 0.0, right_x + rect.width <= ow) {
        (true, true) => {
            Some(if (left_x - rect.x).abs() <= (right_x - rect.x).abs() { left_x } else { right_x })
        }
        (true, false) => Some(left_x),
        (false, true) => Some(right_x),
        (false, false) => None,
    };
    match beside {
        Some(x) => cosmic::iced::Rectangle { x, ..rect },
        None => cosmic::iced::Rectangle {
            y: h.height.min((oh - rect.height).max(0.0)),
            ..rect
        },
    }
}

/// Which side of the toolbar its buttons' hover tooltips open on (DRAGON-475). Pure
/// geometry over the placed rect, unit-tested below.
///
/// A fixed side cannot work here: the toolbar sits bottom-centred by default, anchors to
/// ANY side of a region selection, and can be dragged anywhere on the output. The rule is
/// "open into the larger free band" — above/below the bar when it lays out horizontally,
/// beside it when it is stacked — so a tooltip never opens toward an edge the toolbar is
/// hugging, where it would only be clamped back over the very buttons it labels.
pub(super) fn tooltip_side(
    horizontal: bool,
    rect: &cosmic::iced::Rectangle,
    ow: f32,
    oh: f32,
) -> widget::tooltip::Position {
    use widget::tooltip::Position;
    if horizontal {
        if rect.y >= oh - (rect.y + rect.height) { Position::Top } else { Position::Bottom }
    } else if rect.x >= ow - (rect.x + rect.width) {
        Position::Left
    } else {
        Position::Right
    }
}

/// Whether the capture toolbar offers hover tooltips at all (DRAGON-483). Pure decision
/// over the two states that suppress them, unit-tested below.
///
/// `recording` is the hard one: this toolbar sits IN the recorded frame, so a tooltip that
/// opens while a recording runs is baked into the video. It was reported on the mic and
/// system-audio toggles (today the only tipped buttons the recording bar still shows), but
/// the rule is the WHOLE bar, so adding a tooltip to the pause or cancel button later
/// cannot quietly reintroduce it. A live recording is also the one phase where the labels
/// buy nothing: the user already chose every control on that bar to get here.
///
/// `delay_menu_open` is the older DRAGON-475 rule, folded in here so the bar has ONE answer
/// to "tooltips?". A tooltip drawn across the delay popover splits (text above it, backdrop
/// below), the same overlay layer-batching artifact the preview editor suppresses its own
/// tooltips under an open flyout for.
///
/// A COUNTDOWN is deliberately not suppressed. Nothing it draws reaches a file: a still is
/// reconstructed from the scene frozen at launch, and a video countdown's tooltip is gone
/// the moment `recording` flips true and the toolbar rebuilds through this predicate.
pub(super) fn toolbar_tooltips_enabled(recording: bool, delay_menu_open: bool) -> bool {
    !recording && !delay_menu_open
}

#[cfg(test)]
mod toolbar_tooltips_enabled_tests {
    use super::*;

    /// The idle toolbar is the case DRAGON-475 added tooltips for.
    #[test]
    fn the_idle_toolbar_shows_its_tooltips() {
        assert!(toolbar_tooltips_enabled(false, false));
    }

    /// DRAGON-483: a live recording captures this bar, so no tooltip may open over it.
    #[test]
    fn a_live_recording_suppresses_every_tooltip() {
        assert!(!toolbar_tooltips_enabled(true, false));
    }

    /// The DRAGON-475 flyout rule still holds on its own.
    #[test]
    fn an_open_delay_menu_suppresses_every_tooltip() {
        assert!(!toolbar_tooltips_enabled(false, true));
    }

    /// Either reason alone is enough, so both together stay suppressed.
    #[test]
    fn both_reasons_at_once_stay_suppressed() {
        assert!(!toolbar_tooltips_enabled(true, true));
    }
}

#[cfg(test)]
mod avoid_camera_housing_tests {
    use super::*;
    use cosmic::iced::Rectangle;

    /// The owner's built-in panel, measured (DRAGON-692): 2048x1330 points, a 43pt
    /// safe-area inset, and 899pt wings either side, so the housing spans x 899..1149.
    const OW: f32 = 2048.0;
    const OH: f32 = 1330.0;
    const NOTCH: CameraHousing = CameraHousing { x: 899.0, width: 250.0, height: 43.0 };

    /// The housing that a panel reports when its wings read as unusable: the whole band.
    const FULL_BAND: CameraHousing = CameraHousing { x: 0.0, width: OW, height: 43.0 };

    /// The live recording toolbar at its widest today: ink + audio + the recording group,
    /// with the two gaps between them.
    fn rec_bar(x: f32, y: f32) -> Rectangle {
        Rectangle {
            x,
            y,
            width: W_INK + W_AUDIO + REC_W + 2.0 * GAP,
            height: GROUP_H_BASE,
        }
    }

    /// The stacked side-dock variant: narrow and tall.
    fn stacked(x: f32, y: f32) -> Rectangle {
        Rectangle { x, y, width: V_W, height: 3.0 * GROUP_H_BASE + 2.0 * GAP }
    }

    fn overlaps(r: &Rectangle, h: &CameraHousing) -> bool {
        r.y < h.height && r.x < h.x + h.width && r.x + r.width > h.x
    }

    /// THE regression pin for every non-mac build and every notchless Mac: no housing means
    /// the rect comes back BIT-IDENTICAL, so Linux and Windows cannot have changed at all.
    #[test]
    fn no_housing_is_the_identity() {
        for y in [0.0f32, 10.0, 43.0, 600.0, OH - GROUP_H_BASE] {
            for x in [0.0f32, 400.0, 793.0, 1500.0, OW - 462.0] {
                let r = rec_bar(x, y);
                assert_eq!(avoid_camera_housing(r, None, OW, OH), r, "moved at {x},{y}");
                assert_eq!(avoid_camera_housing(stacked(x, y), None, OW, OH), stacked(x, y));
            }
        }
    }

    /// A degenerate housing (zero width or zero depth) is no housing either, so a reader
    /// that hands back a hollow band can never move the bar.
    #[test]
    fn a_degenerate_housing_is_the_identity() {
        let r = rec_bar(900.0, 0.0);
        let flat = CameraHousing { height: 0.0, ..NOTCH };
        let thin = CameraHousing { width: 0.0, ..NOTCH };
        assert_eq!(avoid_camera_housing(r, Some(flat), OW, OH), r);
        assert_eq!(avoid_camera_housing(r, Some(thin), OW, OH), r);
    }

    /// The DEFAULT placement, bottom-centred, is nowhere near the band, so the common path
    /// on a notched Mac is untouched too.
    #[test]
    fn the_bottom_centred_default_is_untouched_on_a_notched_panel() {
        let r = rec_bar((OW - 462.0) / 2.0, OH - GROUP_H_BASE - BOTTOM_MARGIN);
        assert_eq!(avoid_camera_housing(r, Some(NOTCH), OW, OH), r);
    }

    /// A bar sitting in the band but wholly to one SIDE of the housing is already legal,
    /// so it keeps its place: the wings are usable screen, not forbidden ground.
    #[test]
    fn a_bar_beside_the_housing_keeps_its_place() {
        let left = rec_bar(400.0, 4.0);
        assert_eq!(avoid_camera_housing(left, Some(NOTCH), OW, OH), left);
        let right = rec_bar(1200.0, 4.0);
        assert_eq!(avoid_camera_housing(right, Some(NOTCH), OW, OH), right);
    }

    /// Overlapping from the left: the bar slides LEFT until its right edge meets the
    /// housing, and keeps the y it was placed or dragged to.
    #[test]
    fn a_bar_overlapping_from_the_left_nudges_left() {
        let r = rec_bar(700.0, 6.0);
        let out = avoid_camera_housing(r, Some(NOTCH), OW, OH);
        assert_eq!(out.x + out.width, NOTCH.x, "right edge must meet the housing");
        assert_eq!(out.y, 6.0, "the nudge is horizontal only");
        assert_eq!((out.width, out.height), (r.width, r.height), "never resized");
        assert!(!overlaps(&out, &NOTCH));
    }

    /// Overlapping from the right: the bar slides RIGHT to start at the housing's far edge.
    #[test]
    fn a_bar_overlapping_from_the_right_nudges_right() {
        let r = rec_bar(1100.0, 6.0);
        let out = avoid_camera_housing(r, Some(NOTCH), OW, OH);
        assert_eq!(out.x, NOTCH.x + NOTCH.width, "left edge must meet the housing");
        assert_eq!(out.y, 6.0);
        assert!(!overlaps(&out, &NOTCH));
    }

    /// Dead-centre under the housing is an exact tie, and a tie goes LEFT. Pinned because a
    /// tie-break that depended on float noise would make the bar flicker between wings.
    #[test]
    fn a_bar_centred_under_the_housing_ties_left() {
        let bar_w = rec_bar(0.0, 0.0).width;
        // Centre the bar on the housing's centre, so both wings are the same move away.
        let r = rec_bar(NOTCH.x + NOTCH.width / 2.0 - bar_w / 2.0, 0.0);
        let out = avoid_camera_housing(r, Some(NOTCH), OW, OH);
        assert_eq!(out.x + out.width, NOTCH.x);
        assert!(!overlaps(&out, &NOTCH));
    }

    /// The nearer wing wins, both ways round, so the bar never crosses the whole screen to
    /// clear a housing it was only just touching.
    #[test]
    fn the_nudge_always_takes_the_shorter_move() {
        // Only its right edge is over the housing → the left wing is much nearer.
        let barely_left = rec_bar(NOTCH.x - 400.0, 0.0);
        let out = avoid_camera_housing(barely_left, Some(NOTCH), OW, OH);
        assert_eq!(out.x + out.width, NOTCH.x, "should have gone left");
        // Only its left edge is over the housing → the right wing is much nearer.
        let barely_right = rec_bar(NOTCH.x + NOTCH.width - 20.0, 0.0);
        let out = avoid_camera_housing(barely_right, Some(NOTCH), OW, OH);
        assert_eq!(out.x, NOTCH.x + NOTCH.width, "should have gone right");
    }

    /// The STACKED variant obeys the same rule: it is narrow, so it fits in a wing easily,
    /// and it clears the housing without losing its vertical anchor.
    #[test]
    fn the_stacked_variant_nudges_beside_the_housing() {
        let r = stacked(1000.0, 0.0);
        let out = avoid_camera_housing(r, Some(NOTCH), OW, OH);
        assert_eq!(out.y, 0.0, "a stack keeps its anchored y");
        assert_eq!((out.width, out.height), (r.width, r.height));
        assert!(!overlaps(&out, &NOTCH));
        assert!(out.x >= 0.0 && out.x + out.width <= OW);
    }

    /// A bar too wide for either wing has no sideways answer, so it drops below the band —
    /// exactly clear of the housing, not a point further.
    #[test]
    fn a_bar_too_wide_for_either_wing_drops_below_the_band() {
        // Wings are 899pt here; a 1000pt bar fits in neither.
        let r = Rectangle { x: 500.0, y: 0.0, width: 1000.0, height: GROUP_H_BASE };
        let out = avoid_camera_housing(r, Some(NOTCH), OW, OH);
        assert_eq!(out.y, NOTCH.height, "cleared the band vertically");
        assert_eq!(out.x, 500.0, "and kept its x, since it did not move sideways");
        assert!(!overlaps(&out, &NOTCH));
    }

    /// The full-band fallback (a panel whose wings read as unusable) leaves no room beside
    /// the housing at all, so every overlapping bar clears it vertically.
    #[test]
    fn the_full_band_fallback_always_drops_below() {
        for x in [0.0f32, 500.0, 1400.0] {
            let out = avoid_camera_housing(rec_bar(x, 0.0), Some(FULL_BAND), OW, OH);
            assert_eq!(out.y, FULL_BAND.height, "x {x}");
            assert_eq!(out.x, x, "a vertical clear never moves x");
            assert!(!overlaps(&out, &FULL_BAND), "x {x}");
        }
    }

    /// A bar taller than its own output has nowhere below to go. It clamps to the top edge
    /// rather than walking off the bottom: the honest degenerate, and the same shape the
    /// drag clamp in `toolbar_layout` already uses.
    #[test]
    fn a_bar_taller_than_the_output_clamps_instead_of_escaping() {
        let r = Rectangle { x: 0.0, y: 0.0, width: OW, height: OH + 200.0 };
        let out = avoid_camera_housing(r, Some(FULL_BAND), OW, OH);
        assert_eq!(out.y, 0.0);
    }

    /// THE drag property, swept: whatever the clamp in `toolbar_layout` hands over, the
    /// answer never overlaps the housing and never leaves the output. This is what makes a
    /// drag unable to park the bar under the notch, from any direction.
    #[test]
    fn no_dragged_position_can_park_the_bar_under_the_housing() {
        for &h in &[NOTCH, FULL_BAND] {
            for yi in 0..=60 {
                let y = yi as f32;
                for xi in 0..=64 {
                    // Every x the clamp can produce, coarsely swept edge to edge.
                    let x = (xi as f32 / 64.0) * (OW - rec_bar(0.0, 0.0).width);
                    let out = avoid_camera_housing(rec_bar(x, y), Some(h), OW, OH);
                    assert!(!overlaps(&out, &h), "{h:?} let {x},{y} sit under the housing");
                    assert!(out.x >= 0.0 && out.x + out.width <= OW + 0.001, "x escaped at {x},{y}");
                    assert!(out.y >= 0.0 && out.y + out.height <= OH + 0.001, "y escaped at {x},{y}");
                }
            }
        }
    }

    /// Dragging UP into the housing SLIDES the bar along the housing's side: the x it is
    /// pushed to is the same for every y inside the band, so the bar hugs the edge instead
    /// of jumping around as the drag continues.
    #[test]
    fn a_drag_toward_the_housing_slides_along_its_edge() {
        let start = 700.0;
        let mut seen: Option<f32> = None;
        for yi in 0..43 {
            let out = avoid_camera_housing(rec_bar(start, yi as f32), Some(NOTCH), OW, OH);
            match seen {
                None => seen = Some(out.x),
                Some(x) => assert_eq!(out.x, x, "the bar jumped at y {yi}"),
            }
        }
        assert_eq!(seen, Some(NOTCH.x - rec_bar(0.0, 0.0).width));
        // ...and one point BELOW the band it is released again, back where the drag put it.
        let free = avoid_camera_housing(rec_bar(start, NOTCH.height), Some(NOTCH), OW, OH);
        assert_eq!(free.x, start, "clear of the band, the bar keeps the x it was dragged to");
    }

    /// Applying the rule twice is applying it once: the output of a nudge is already legal,
    /// so nothing can drift on a re-layout (this function runs on EVERY view build).
    #[test]
    fn the_rule_is_idempotent() {
        for &h in &[NOTCH, FULL_BAND] {
            for x in [0.0f32, 600.0, 900.0, 1100.0, 1500.0] {
                for y in [0.0f32, 20.0, 42.0] {
                    let once = avoid_camera_housing(rec_bar(x, y), Some(h), OW, OH);
                    let twice = avoid_camera_housing(once, Some(h), OW, OH);
                    assert_eq!(once, twice, "{h:?} drifted from {x},{y}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tooltip_side_tests {
    use super::*;
    use cosmic::iced::Rectangle;
    use widget::tooltip::Position;

    /// The default bottom-centred toolbar has more room above it than below → open UP.
    #[test]
    fn the_bottom_toolbar_opens_its_tooltips_upward() {
        let r = Rectangle { x: 800.0, y: 1000.0, width: 320.0, height: 48.0 };
        assert!(matches!(tooltip_side(true, &r, 1920.0, 1080.0), Position::Top));
    }

    /// Dragged to hug the top edge, opening up would clamp back over the bar → open DOWN.
    #[test]
    fn a_toolbar_dragged_to_the_top_edge_opens_downward() {
        let r = Rectangle { x: 800.0, y: 8.0, width: 320.0, height: 48.0 };
        assert!(matches!(tooltip_side(true, &r, 1920.0, 1080.0), Position::Bottom));
    }

    /// Stacked beside a region on the LEFT half of the output → open toward the free right.
    #[test]
    fn a_left_anchored_stack_opens_rightward() {
        let r = Rectangle { x: 40.0, y: 300.0, width: 148.0, height: 400.0 };
        assert!(matches!(tooltip_side(false, &r, 1920.0, 1080.0), Position::Right));
    }

    /// Stacked on the RIGHT half → open toward the free left.
    #[test]
    fn a_right_anchored_stack_opens_leftward() {
        let r = Rectangle { x: 1700.0, y: 300.0, width: 148.0, height: 400.0 };
        assert!(matches!(tooltip_side(false, &r, 1920.0, 1080.0), Position::Left));
    }
}
