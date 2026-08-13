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
/// Settings + close group width.
pub(super) const W_UTIL: f32 = 88.0;

impl App {
    /// Where the toolbar sits on output `o`: its rect and whether it lays out
    /// horizontally. Anchored beside a region selection, otherwise centred on the
    /// bottom of every screen. Shared by the renderer and the countdown
    /// input-zone setup.
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
        let window_rect = self
            .pending
            .as_ref()
            .filter(|_| active)
            .filter(|s| s.window_id.is_some())
            .map(|s| (s.x, s.y, s.width, s.height));
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
            let mut g = vec![(chip_w, GROUP_H_BASE, GROUP_H_BASE)];
            if self.kind == Kind::Video {
                g.push((W_AUDIO, GROUP_H_BASE, GROUP_H_BASE));
            }
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
        Some((rect, horizontal))
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
