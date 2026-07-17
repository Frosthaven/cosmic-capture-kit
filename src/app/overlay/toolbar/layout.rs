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
        let region_sel = window_rect.or_else(|| {
            if self.mode == Mode::Region {
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
            if !scanner {
                g.push((W_MODE, GROUP_H_BASE, GROUP_H_BASE));
            }
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
            let ow = o.logical_size.0 as f32;
            let oh = o.logical_size.1 as f32;
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
        let ow = o.logical_size.0 as f32;
        let oh = o.logical_size.1 as f32;
        let (offx, offy) = self.toolbar_offset.get(&o.name).copied().unwrap_or((0.0, 0.0));
        let rect = cosmic::iced::Rectangle {
            x: (rect.x + offx).clamp(0.0, (ow - rect.width).max(0.0)),
            y: (rect.y + offy).clamp(0.0, (oh - rect.height).max(0.0)),
            ..rect
        };
        Some((rect, horizontal))
    }
}
