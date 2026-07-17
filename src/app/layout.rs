//! Overlay / toolbar geometry: placement helpers and shared sizing constants.
//!
//! All constants here are the single source of truth for toolbar button/group
//! footprints; `toolbar_layout` (which sizes the placement box and click-through
//! input zone) and `capture_button_layer` (which builds the widgets) both derive
//! from these, so the input zone can never drift from what's drawn.

use cosmic::iced::{Background, Length};
use super::Msg;
use super::OutputState;

// ── Toolbar geometry constants ────────────────────────────────────────────────

/// Gap (logical px) between the toolbar and the selection it's placed beside.
pub(crate) const BADGE_GAP: f32 = 8.0;

/// Icon glyph box inside a toolbar button.
pub(crate) const ICON_BOX: f32 = 22.0;
/// A button's padding around its icon.
pub(crate) const BTN_PAD: f32 = 8.0;
/// A group container's padding around its button row.
pub(crate) const GROUP_PAD: f32 = 3.0;
/// Outer height of a standard group (an icon button in a padded group).
pub(crate) const GROUP_H_BASE: f32 = ICON_BOX + 2.0 * BTN_PAD + 2.0 * GROUP_PAD;

// ── Audio meter ───────────────────────────────────────────────────────────────

/// The meter's red line: the mic test's "too loud" threshold (-6 dBFS on the shared
/// `(dBFS+60)/60` meter scale). Below it the fill is green; at or above it the whole
/// fill turns red — same semantics as the mic-test waveform, no yellow in between.
pub(crate) const METER_RED_ZONE: f32 = 0.90;

/// Volume-meter fill for an armed audio button: a single-hue FADE anchored at the
/// BOTTOM and rising to `level` (0..1), transparent above. Green while the level is
/// healthy; the fill turns red only when the level crosses the mic test's red zone
/// (`METER_RED_ZONE`). Half-transparent so the icon on top stays readable. Empty
/// when silent.
pub(crate) fn meter_background(level: f32) -> Background {
    use cosmic::iced::{gradient::Linear, Color, Radians};
    // Canonical semantic palette (shared with the mic test + status captions).
    let l = level.clamp(0.0, 1.0);
    let hue = if l >= METER_RED_ZONE {
        crate::app::theme::DANGER
    } else {
        crate::app::theme::SUCCESS
    };
    let clear = Color::from_rgba(0.0, 0.0, 0.0, 0.0);
    // Angle 0 points up, so stop 0.0 is at the BOTTOM (where the fill starts) and
    // the bar rises toward the top.
    let mut g = Linear::new(Radians(0.0));
    if l <= 0.02 {
        // Effectively silent — no visible fill.
        return Background::from(g.add_stop(0.0, clear).add_stop(1.0, clear));
    }
    // Fade: half-alpha at the bottom easing out toward the fill's leading edge, so
    // the level reads as a rising tint under the icon rather than a colour block.
    let a = |alpha: f32| Color { a: alpha, ..hue };
    g = g.add_stop(0.0, a(0.5));
    g = g.add_stop(l, a(0.18));
    if l < 0.999 {
        // Cut to transparent just past the fill's leading edge.
        g = g.add_stop((l + 0.02).min(0.9999), clear).add_stop(1.0, clear);
    }
    Background::from(g)
}

// ── Overlay positioning helpers ───────────────────────────────────────────────

/// Place `content` at local (`lx`,`ly`) on an output overlay by padding a fill
/// container (the same trick the toolbar uses). For the detection-mark outlines +
/// tooltips, which sit at specific positions over the region.
pub(crate) fn positioned_mark<'a>(lx: f32, ly: f32, content: super::Element<'a, Msg>) -> super::Element<'a, Msg> {
    cosmic::widget::container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(cosmic::iced::Alignment::Start)
        .align_y(cosmic::iced::Alignment::Start)
        .padding(cosmic::iced::Padding {
            top: ly,
            left: lx,
            right: 0.0,
            bottom: 0.0,
        })
        .into()
}

/// Width (logical px) of the selection's accent outline — matches the line drawn
/// by `RegionSelection`. A region capture is inset by this so only what's visibly
/// inside the outline is grabbed.
pub(crate) const SELECTION_LINE_W: i32 = 2;

/// Inset a region selection by the outline width so a capture grabs only what's
/// visibly inside the line. Window/monitor selections are returned unchanged (they
/// capture the full target, with the outline drawn outside it).
pub(crate) fn inset_region(sel: super::Selection) -> super::Selection {
    use super::Selection;
    if sel.window_id.is_some() || sel.output.is_some() {
        return sel;
    }
    let i = SELECTION_LINE_W;
    Selection {
        x: sel.x + i,
        y: sel.y + i,
        width: sel.width.saturating_sub((2 * i) as u32),
        height: sel.height.saturating_sub((2 * i) as u32),
        ..sel
    }
}

/// Where to place an action box (`bw`×`bh`) for the selection on output `o`, in
/// that output's surface-local coords. Prefers just *outside* the selection —
/// bottom-centred, then top-centred (both **horizontal**), then left/right
/// (**vertical**) — falling back inside-top-right (horizontal) when nothing
/// outside fits (e.g. a whole-monitor selection). Returns `(rect, horizontal)`,
/// or `None` when the selection doesn't touch this output.
/// Place a control box just outside the region, preferring below/above (where it
/// lays out *horizontally* with dims `hbw`×`hbh`) and falling back to left/right
/// (where it lays out *vertically* with dims `vbw`×`vbh`). The returned bool is
/// true for a horizontal placement. The box is centred on the region along its
/// free axis and clamped on-screen.
#[allow(clippy::too_many_arguments)]
// OutputState is private to app; placement is pub(crate) only to satisfy the
// re-export in mod.rs; callers are always within app.
#[allow(private_interfaces)]
pub(crate) fn placement(
    o: &OutputState,
    sx: i32,
    sy: i32,
    sw: u32,
    sh: u32,
    hbw: f32,
    hbh: f32,
    vbw: f32,
    vbh: f32,
) -> Option<(cosmic::iced::Rectangle, bool)> {
    let ow = o.logical_size.0 as f32;
    let oh = o.logical_size.1 as f32;
    let lx = (sx - o.logical_pos.0) as f32;
    let ly = (sy - o.logical_pos.1) as f32;
    let lw = sw as f32;
    let lh = sh as f32;
    if lx + lw <= 0.0 || ly + lh <= 0.0 || lx >= ow || ly >= oh {
        return None;
    }
    let cx = (lx + (lw - hbw) / 2.0).clamp(0.0, (ow - hbw).max(0.0));
    let cy = (ly + (lh - vbh) / 2.0).clamp(0.0, (oh - vbh).max(0.0));
    let below = ly + lh + BADGE_GAP;
    let above = ly - BADGE_GAP - hbh;
    let left = lx - BADGE_GAP - vbw;
    let right = lx + lw + BADGE_GAP;
    // Prefer below, then above; only when neither horizontal slot fits do we go to
    // a side — left or right, whichever has more clear space. Last resort: inside
    // the top-right corner (selection fills the output).
    let left_room = lx;
    let right_room = ow - (lx + lw);
    let (x, y, bw, bh, horizontal) = if below + hbh <= oh {
        (cx, below, hbw, hbh, true)
    } else if above >= 0.0 {
        (cx, above, hbw, hbh, true)
    } else {
        let left_fits = left >= 0.0;
        let right_fits = right + vbw <= ow;
        match (left_fits, right_fits) {
            (true, true) if left_room >= right_room => (left, cy, vbw, vbh, false),
            (true, true) => (right, cy, vbw, vbh, false),
            (true, false) => (left, cy, vbw, vbh, false),
            (false, true) => (right, cy, vbw, vbh, false),
            (false, false) => (
                (lx + lw - hbw - BADGE_GAP).clamp(0.0, (ow - hbw).max(0.0)),
                (ly + BADGE_GAP).clamp(0.0, (oh - hbh).max(0.0)),
                hbw,
                hbh,
                true,
            ),
        }
    };
    Some((cosmic::iced::Rectangle { x, y, width: bw, height: bh }, horizontal))
}
