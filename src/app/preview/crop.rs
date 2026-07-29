//! The image crop tool (DRAGON-382; IMAGES only).
//!
//! # The model
//! A crop is a rectangle over the SOURCE image, in source pixels ([`CropRect`]), stored
//! NON-DESTRUCTIVELY on [`super::edit::EditState::crop`]: the decoded pixels are never
//! touched, so reopening the tool shows the full image again with the rectangle live and
//! repositionable. The rect may extend BEYOND the image bounds (a negative origin, or a
//! right/bottom past the frame) — the out-of-bounds area bakes to OPAQUE BLACK ([`crop_image`]).
//! The crop is applied only at export time ([`super::edit::bake_image`]); a crop equal to the
//! whole frame is stored as `None`, so a full-image crop is never dirty and never re-encodes.
//!
//! # The session
//! Entering the tool opens a transient [`super::edit::CropSession`] holding a WORKING COPY of
//! the rect. Dragging a side/corner/body edits the copy; Accept commits it to the model as one
//! shared-history [`super::edit::EditOp::Crop`] entry, Cancel discards it. The viewport is
//! zoomed out (leaving blank margin around the media) on entry and restored on exit.
//!
//! # Pure geometry
//! The drag resolution, the soft-snap-with-override, the entry zoom ([`crop_entry_zoom`] — the
//! margin the session opens with) and the black-fill extraction are pure functions here,
//! unit-tested below. The interactive overlay
//! (dim scrim, rule-of-thirds grid, handles, pointer routing) lives in
//! [`crate::widgets::crop_canvas`]; this module owns the model + math + the App-side session
//! lifecycle.

use super::*;
use crate::widgets::crop_canvas::CropHandle;
use ::image::RgbaImage;

/// The minimum crop size, in SOURCE px — a drag can never collapse a side past this.
pub const MIN_CROP_PX: f32 = 8.0;

/// The blank margin the crop tool opens with (DRAGON-396): at least this fraction of the VIEWPORT
/// free on EVERY side, so there is room to pull the crop out past the image.
///
/// WHY a margin target and not a zoom fraction. The obvious rule — "open at some fraction of FIT"
/// — assumes FIT is a constant proportion of the viewport. It is not: [`super::video::fit_dims`]
/// clamps its scale with `.min(1.0)`, so **fit never upscales**. A capture larger than the viewport
/// fills it exactly at fit (margin 0, so it genuinely needs zooming out), while a SMALL capture is
/// already shown at natural size with space around it — and a zoom fraction would shrink that one
/// AGAIN, for no reason, to something absurdly small. Small captures were hit hardest by exactly
/// that. Targeting the MARGIN instead is absolute: it asks for the room it wants and stops, so a
/// capture that already has the room is left alone.
pub const CROP_ENTRY_MARGIN: f32 = 0.2;

/// The floor on the entry zoom — a cheap guard, NOT the rule.
///
/// It provably cannot bind: with the media fitted to `(dw, dh)` in a `(vw, vh)` viewport, the
/// target is `0.6 · min(vw/dw, vh/dh)`. If the fit is viewport-limited the limiting axis has
/// `d == v` exactly, so that `min` is 1 and the target is 0.6; if the no-upscale clamp bound
/// instead, both `d < v`, the `min` exceeds 1, and the never-zoom-IN clamp pins the result to fit.
/// Either way `>= 0.6`. Kept only so a degenerate geometry can never produce a runaway zoom-out.
pub const CROP_ENTRY_MIN_ZOOM: f32 = 0.5;

/// The entry zoom must clear the session's own relaxed floor, or [`Viewport::set_zoom`] would
/// clamp it away. Compile-time, so the two can never silently drift apart.
const _: () = assert!(CROP_ENTRY_MIN_ZOOM > Viewport::CROP_MIN);

/// The fit-relative zoom a crop session opens at: zoom OUT until at least [`CROP_ENTRY_MARGIN`] of
/// the viewport is free on every side — i.e. the media occupies at most `1 - 2·margin` of each axis
/// — and stop. `viewport` is the available space and `disp` the media's on-screen size at zoom 1
/// (its FIT size); both in screen px.
///
/// Two clamps bound it:
/// * never zoom IN — a capture that already has the margin at fit opens at fit, a true no-op;
/// * never past [`CROP_ENTRY_MIN_ZOOM`] (see there — it cannot bind in practice).
///
/// The axis `min` is what makes "on every side" hold on BOTH axes: the limiting one wins, exactly
/// as the fit itself is a per-axis minimum, so a wide capture in a tall viewport cannot pass just
/// because its height happened to clear. Unknown geometry opens at fit rather than guessing.
/// Pure — unit-tested.
pub fn crop_entry_zoom(viewport: (f32, f32), disp: (f32, f32)) -> f32 {
    let ((vw, vh), (dw, dh)) = (viewport, disp);
    if !(vw > 0.0 && vh > 0.0 && dw > 0.0 && dh > 0.0) {
        return Viewport::FIT;
    }
    // The fraction of each axis the media may occupy, and the zoom that achieves it on the
    // limiting axis.
    let fill = 1.0 - 2.0 * CROP_ENTRY_MARGIN;
    let want = fill * (vw / dw).min(vh / dh);
    want.clamp(CROP_ENTRY_MIN_ZOOM, Viewport::FIT)
}

/// The soft-snap distance in SCREEN px: a crop edge within this of an image boundary snaps to
/// it, unless the override modifier (Cmd/Ctrl) is held (Photoshop's behaviour).
pub const SNAP_SCREEN_PX: f32 = 10.0;

/// A crop rectangle over the SOURCE image, in source pixels. May extend beyond the image
/// bounds; the out-of-bounds area bakes to opaque black. See the module doc.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl CropRect {
    /// The whole frame (the default crop when the tool first opens over an un-cropped image).
    pub fn full(frame: (u32, u32)) -> Self {
        Self { x: 0.0, y: 0.0, w: frame.0 as f32, h: frame.1 as f32 }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// The rect's size in whole SOURCE pixels (rounded, floored at 1) — the DISPLAY-frame
    /// dimensions the editor frames to once the crop is applied (DRAGON-385), and the pixel
    /// size [`crop_image`] bakes to. Independent of where the rect sits (a negative origin
    /// still yields the same size; the out-of-bounds part bakes black).
    pub fn pixel_size(&self) -> (u32, u32) {
        ((self.w.round() as i64).max(1) as u32, (self.h.round() as i64).max(1) as u32)
    }

    /// Whether this rect is (within half a pixel) the whole frame — a full crop is NOT dirty
    /// and is stored as `None` so the bake stays byte-identical to an un-cropped image.
    pub fn is_full(&self, frame: (u32, u32)) -> bool {
        let eps = 0.5;
        self.x.abs() < eps
            && self.y.abs() < eps
            && (self.w - frame.0 as f32).abs() < eps
            && (self.h - frame.1 as f32).abs() < eps
    }
}

/// Resolve a crop drag to a new rect. `orig` is the rect at drag-begin, `press` the pointer
/// image point at drag-begin, `now` the current pointer image point (both SOURCE px). Edges and
/// corners move by the drag DELTA (so grabbing anywhere near a handle drags smoothly, and the
/// opposite side stays put); [`CropHandle::Move`] translates the whole rect. Every side is kept
/// at least [`MIN_CROP_PX`] from its opposite. Dragging past the image bounds is allowed.
pub fn resolve_drag(orig: CropRect, handle: CropHandle, press: (f32, f32), now: (f32, f32)) -> CropRect {
    let dx = now.0 - press.0;
    let dy = now.1 - press.1;
    let (mut l, mut t, mut r, mut b) = (orig.x, orig.y, orig.right(), orig.bottom());
    if handle == CropHandle::Move {
        l += dx;
        r += dx;
        t += dy;
        b += dy;
        return CropRect { x: l, y: t, w: r - l, h: b - t };
    }
    if handle.moves_west() {
        // The left edge can never cross the right edge minus the minimum.
        l = (orig.x + dx).min(r - MIN_CROP_PX);
    }
    if handle.moves_east() {
        r = (orig.right() + dx).max(l + MIN_CROP_PX);
    }
    if handle.moves_north() {
        t = (orig.y + dy).min(b - MIN_CROP_PX);
    }
    if handle.moves_south() {
        b = (orig.bottom() + dy).max(t + MIN_CROP_PX);
    }
    CropRect { x: l, y: t, w: r - l, h: b - t }
}

/// Soft-snap the crop edges the `handle` is moving to the image boundary when within `thresh`
/// SOURCE px of it — unless `suppress` (the override modifier) is held. [`CropHandle::Move`]
/// snaps by TRANSLATING the whole rect so a near edge aligns (never resizing it); the resize
/// handles snap only the edges they own. Pure — unit-tested.
pub fn snap_edges(r: CropRect, frame: (u32, u32), thresh: f32, suppress: bool, handle: CropHandle) -> CropRect {
    if suppress || thresh <= 0.0 {
        return r;
    }
    let (fw, fh) = (frame.0 as f32, frame.1 as f32);
    if handle == CropHandle::Move {
        // Translate so whichever edge is within range aligns with the canvas edge.
        let dx = if r.x.abs() <= thresh {
            -r.x
        } else if (r.right() - fw).abs() <= thresh {
            fw - r.right()
        } else {
            0.0
        };
        let dy = if r.y.abs() <= thresh {
            -r.y
        } else if (r.bottom() - fh).abs() <= thresh {
            fh - r.bottom()
        } else {
            0.0
        };
        return CropRect { x: r.x + dx, y: r.y + dy, ..r };
    }
    let (mut l, mut t, mut rt, mut bt) = (r.x, r.y, r.right(), r.bottom());
    if handle.moves_west() && l.abs() <= thresh {
        l = 0.0;
    }
    if handle.moves_north() && t.abs() <= thresh {
        t = 0.0;
    }
    if handle.moves_east() && (rt - fw).abs() <= thresh {
        rt = fw;
    }
    if handle.moves_south() && (bt - fh).abs() <= thresh {
        bt = fh;
    }
    CropRect { x: l, y: t, w: rt - l, h: bt - t }
}

/// Extract `rect` (SOURCE px) from `base`, producing a new RGBA image of the rect's pixel size,
/// filling any area OUTSIDE `base` with OPAQUE BLACK (the ticket's black-pixel fill for a crop
/// dragged past the image bounds). The rect is rounded to whole pixels; a degenerate rect yields
/// a 1x1 image. Pure — unit-tested.
pub fn crop_image(base: &RgbaImage, rect: CropRect) -> RgbaImage {
    let x0 = rect.x.round() as i64;
    let y0 = rect.y.round() as i64;
    let w = (rect.w.round() as i64).max(1);
    let h = (rect.h.round() as i64).max(1);
    let (bw, bh) = base.dimensions();
    let mut out = RgbaImage::from_pixel(w as u32, h as u32, ::image::Rgba([0, 0, 0, 255]));
    for oy in 0..h {
        let sy = y0 + oy;
        if sy < 0 || sy >= bh as i64 {
            continue;
        }
        for ox in 0..w {
            let sx = x0 + ox;
            if sx < 0 || sx >= bw as i64 {
                continue;
            }
            out.put_pixel(ox as u32, oy as u32, *base.get_pixel(sx as u32, sy as u32));
        }
    }
    out
}

impl App {
    /// The media's on-screen size at zoom 1 (the fit box) for a preview — the `content_px`
    /// the ZoomPan/CanvasMap use, so the crop overlay maps 1:1 with the picture.
    pub(super) fn crop_content_px(&self, preview: &PreviewState) -> (f32, f32) {
        let (iw, ih) = preview.frame_points();
        if iw == 0 || ih == 0 {
            return (0.0, 0.0);
        }
        let (avail_w, avail_h) = self.preview_viewport(preview);
        video::fit_dims(iw, ih, avail_w, avail_h)
    }

    /// The scale from image SOURCE px to on-screen px at the current zoom (`disp/source * zoom`),
    /// so a screen-px snap threshold converts to source px.
    fn crop_img_to_screen_scale(&self, preview: &PreviewState) -> f32 {
        let (dw, _) = self.crop_content_px(preview);
        let (fw, _) = preview.edit.frame;
        if fw == 0 || dw <= 0.0 {
            return 1.0;
        }
        (dw / fw as f32) * preview.view.zoom
    }

    /// Enter the crop tool: open a session over the CURRENT committed crop (or the whole frame),
    /// zoom the media out until [`CROP_ENTRY_MARGIN`] of the viewport is free on every side (a
    /// no-op when it already is), and relax the zoom floor so the user can pull out much farther.
    /// IMAGES only; a no-op otherwise, or when a session is already open. Any open flyout /
    /// annotation selection is cleared so the crop UI owns the surface.
    pub(super) fn crop_enter(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        // The entry zoom reads viewport geometry (`self`), so resolve it before borrowing mut.
        let Some(preview) = self.preview_for(id) else { return Task::none() };
        if !matches!(preview.kind, PreviewKind::Image(_)) || preview.edit.crop_session.is_some() {
            return Task::none();
        }
        let frame = preview.edit.frame;
        if frame.0 == 0 || frame.1 == 0 {
            return Task::none();
        }
        // The WHOLE picture's fit size (`crop_content_px`, not the display frame's): a session
        // reveals the whole image, so that is the media the margin is measured against. Resolved
        // FIRST, while `preview` is still borrowed — `set_annot_tool` below takes `&mut self`.
        let entry_zoom = crop_entry_zoom(self.preview_viewport(preview), self.crop_content_px(preview));
        // DRAGON-392 correction: the session owns the canvas, so NOTHING may still look armed
        // while it runs. Remember what was held and disarm through the ordinary funnel
        // (`set_annot_tool`, which also settles any live text edit — so a session can never
        // coexist with one, and Enter is unambiguously "accept the crop"). Both exits hand the
        // tool back, whatever it was, `None` included.
        let saved_tool = self.preview_for(id).and_then(|p| p.edit.tool);
        self.set_annot_tool(id, None);
        if let Some(p) = self.preview_for_mut(id) {
            let rect = p.edit.crop.unwrap_or_else(|| CropRect::full(frame));
            p.edit.close_flyout();
            p.edit.sel.clear();
            p.edit.crop_session = Some(edit::CropSession {
                rect,
                saved_view: p.view,
                saved_tool,
                drag: None,
            });
            // Relax the floor, then zoom out to the entry margin and recentre.
            p.view.crop_mode = true;
            p.view.set_zoom(entry_zoom);
            p.view.pan = (0.0, 0.0);
            p.view.zoom_preset = None;
            p.view.zoom_menu_open = false;
        }
        // No covermark refresh here (DRAGON-402): the mark is not drawn during a session, so
        // re-rastering it for the session's canvas produced a raster nobody could see — and left
        // the WRONG one behind for the exits to find. The slot is now untouched for the duration
        // and both exits do the real work.
        Task::none()
    }

    /// Restore the viewport a session saved and clear the crop mode — shared by Accept (when the
    /// crop did NOT change) + Cancel.
    fn crop_restore_view(&mut self, id: window::Id, saved: Viewport) {
        if let Some(p) = self.preview_for_mut(id) {
            p.view = saved;
            p.view.crop_mode = false;
        }
    }

    /// Reframe the view to FIT the (new) DISPLAY frame (DRAGON-385): the committed crop just
    /// changed, so the pre-session zoom/pan no longer describe the content. Drop to fit + centred
    /// so "a crop to the bottom right shows only the bottom right" reads immediately, and any
    /// stale pan can never sit out of the new (smaller) framing's bounds. Shared by Accept and by
    /// undo/redo of a crop.
    pub(super) fn crop_reframe(&mut self, id: window::Id) {
        if let Some(p) = self.preview_for_mut(id) {
            p.view.crop_mode = false;
            p.view.set_zoom(Viewport::FIT);
            p.view.pan = (0.0, 0.0);
            p.view.zoom_preset = Some(0); // "Fit"
            p.view.zoom_menu_open = false;
        }
    }

    /// Accept the live crop: commit the session rect to the model as one undo entry (or `None`
    /// when it equals the whole frame, so a full-image crop is never dirty), then restore the
    /// viewport and close the session.
    pub(super) fn crop_accept(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for_mut(id) else { return Task::none() };
        let Some(session) = p.edit.crop_session.take() else { return Task::none() };
        let frame = p.edit.frame;
        let next = (!session.rect.is_full(frame)).then_some(session.rect);
        // Only record an op when the committed crop actually changes.
        let changed = next != p.edit.crop;
        if changed {
            p.edit.set_crop(next);
        }
        // A changed crop reframes the view to the new framing; an unchanged one just restores the
        // pre-session zoom/pan (nothing about the content moved).
        if changed {
            self.crop_reframe(id);
            // BOTH accept paths hand the tool back — this branch returns early, so it needs its
            // own call or a crop that actually changed would swallow the armed tool.
            self.set_annot_tool(id, session.saved_tool);
            // The covermark comes back for the NEW canvas (DRAGON-391/402). Drop the old raster
            // first: it was rendered for the PREVIOUS framing, and the re-raster below is async, so
            // leaving it in the slot would flash the old mark stretched across the new crop before
            // the right one lands. Absent for a beat beats wrong for a beat.
            if let Some(p) = self.preview_for_mut(id) {
                p.edit.cm_raster.clear();
            }
            return self.refresh_covermark_for_view(id);
        }
        self.crop_restore_view(id, session.saved_view);
        // The tool the session disarmed comes back — on ACCEPT as much as on cancel: which tool
        // was held has nothing to do with the crop's outcome (DRAGON-392 correction).
        self.set_annot_tool(id, session.saved_tool);
        // The covermark becomes visible again over the framing it already had — the raster is
        // untouched (DRAGON-402), so this normally short-circuits to nothing. It is called anyway
        // so this path can never be the one that forgets, the way it used to.
        self.refresh_covermark_for_view(id)
    }

    /// Cancel the live crop: discard the session (the committed crop is untouched) and restore
    /// the viewport.
    pub(super) fn crop_cancel(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for_mut(id) else { return Task::none() };
        let Some(session) = p.edit.crop_session.take() else { return Task::none() };
        self.crop_restore_view(id, session.saved_view);
        // Cancel changes NOTHING about the editing state, the armed tool included.
        self.set_annot_tool(id, session.saved_tool);
        // The covermark becomes visible again over exactly the framing it had before the session
        // (DRAGON-402): nothing about the document changed, and the raster was left untouched, so
        // this short-circuits — the mark simply reappears as it was.
        self.refresh_covermark_for_view(id)
    }

    /// Begin a crop drag: remember the handle, the rect and the pointer image point at press.
    pub(super) fn crop_drag_begin(&mut self, id: window::Id, handle: CropHandle, x: f32, y: f32) {
        if let Some(p) = self.preview_for_mut(id)
            && let Some(s) = &mut p.edit.crop_session
        {
            s.drag = Some(edit::CropDrag { handle, orig: s.rect, press: (x, y) });
        }
    }

    /// Live crop drag: resolve the pointer to a new rect and soft-snap it (unless `suppress`).
    pub(super) fn crop_drag_to(&mut self, id: window::Id, x: f32, y: f32, suppress: bool) {
        let Some(preview) = self.preview_for(id) else { return };
        let scale = self.crop_img_to_screen_scale(preview);
        let thresh = SNAP_SCREEN_PX / scale.max(1e-6);
        if let Some(p) = self.preview_for_mut(id)
            && let Some(s) = &mut p.edit.crop_session
            && let Some(drag) = s.drag
        {
            let frame = p.edit.frame;
            let raw = resolve_drag(drag.orig, drag.handle, drag.press, (x, y));
            s.rect = snap_edges(raw, frame, thresh, suppress, drag.handle);
        }
    }

    /// End a crop drag.
    pub(super) fn crop_drag_end(&mut self, id: window::Id) {
        if let Some(p) = self.preview_for_mut(id)
            && let Some(s) = &mut p.edit.crop_session
        {
            s.drag = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> CropRect {
        CropRect { x, y, w, h }
    }

    #[test]
    fn is_full_tolerates_subpixel_and_rejects_a_real_crop() {
        let frame = (1920, 1080);
        assert!(CropRect::full(frame).is_full(frame));
        assert!(rect(0.2, -0.1, 1920.0, 1079.8).is_full(frame));
        assert!(!rect(10.0, 0.0, 1900.0, 1080.0).is_full(frame));
        assert!(!rect(0.0, 0.0, 1920.0, 1000.0).is_full(frame));
    }

    #[test]
    fn pixel_size_rounds_and_floors_at_one() {
        // The DISPLAY-frame dims a crop frames to (DRAGON-385): rounded to whole pixels.
        assert_eq!(rect(10.0, 20.0, 640.4, 360.6).pixel_size(), (640, 361));
        // A crop dragged past the source keeps its SIZE (the origin, not the size, goes negative).
        assert_eq!(rect(-40.0, -10.0, 300.0, 200.0).pixel_size(), (300, 200));
        // Never zero — a degenerate rect still frames at least 1x1.
        assert_eq!(rect(0.0, 0.0, 0.2, 0.2).pixel_size(), (1, 1));
    }

    #[test]
    fn resolve_drag_moves_only_the_handle_edges() {
        let o = rect(100.0, 100.0, 400.0, 300.0); // l100 t100 r500 b400
        // East edge drags right by 50: only the right edge moves.
        let e = resolve_drag(o, CropHandle::E, (500.0, 250.0), (550.0, 250.0));
        assert_eq!((e.x, e.y, e.right(), e.bottom()), (100.0, 100.0, 550.0, 400.0));
        // North-west corner drags up-left: left + top move, right + bottom pinned.
        let nw = resolve_drag(o, CropHandle::NW, (100.0, 100.0), (60.0, 40.0));
        assert_eq!((nw.x, nw.y, nw.right(), nw.bottom()), (60.0, 40.0, 500.0, 400.0));
    }

    #[test]
    fn resolve_drag_move_translates_the_whole_rect() {
        let o = rect(100.0, 100.0, 400.0, 300.0);
        let m = resolve_drag(o, CropHandle::Move, (200.0, 200.0), (230.0, 180.0));
        assert_eq!((m.x, m.y, m.w, m.h), (130.0, 80.0, 400.0, 300.0));
    }

    #[test]
    fn resolve_drag_enforces_the_minimum_side() {
        let o = rect(100.0, 100.0, 400.0, 300.0);
        // Drag the WEST edge far past the east edge: it stops MIN_CROP_PX short of it.
        let w = resolve_drag(o, CropHandle::W, (100.0, 250.0), (10_000.0, 250.0));
        assert_eq!(w.x, o.right() - MIN_CROP_PX);
        assert!(w.w >= MIN_CROP_PX);
    }

    #[test]
    fn resolve_drag_allows_extending_past_the_image_bounds() {
        let o = rect(0.0, 0.0, 100.0, 100.0);
        // Dragging the west edge left of 0 is allowed (bakes to black there).
        let w = resolve_drag(o, CropHandle::W, (0.0, 50.0), (-40.0, 50.0));
        assert_eq!(w.x, -40.0);
        assert_eq!(w.right(), 100.0);
    }

    #[test]
    fn snap_pulls_a_near_edge_to_the_boundary_unless_suppressed() {
        let frame = (1000, 800);
        // Right edge at 996, snap threshold 10 → snaps to 1000.
        let r = rect(10.0, 10.0, 986.0, 500.0); // right = 996
        let snapped = snap_edges(r, frame, 10.0, false, CropHandle::E);
        assert_eq!(snapped.right(), 1000.0);
        // Override held: no snap.
        let free = snap_edges(r, frame, 10.0, true, CropHandle::E);
        assert_eq!(free.right(), 996.0);
        // A non-owned edge is never snapped by this handle.
        let west = rect(4.0, 10.0, 500.0, 500.0);
        assert_eq!(snap_edges(west, frame, 10.0, false, CropHandle::E).x, 4.0);
        assert_eq!(snap_edges(west, frame, 10.0, false, CropHandle::W).x, 0.0);
    }

    #[test]
    fn snap_move_translates_without_resizing() {
        let frame = (1000, 800);
        let r = rect(4.0, 300.0, 500.0, 200.0); // left near 0
        let s = snap_edges(r, frame, 10.0, false, CropHandle::Move);
        assert_eq!(s.x, 0.0);
        assert_eq!((s.w, s.h), (500.0, 200.0), "move-snap must not resize");
    }

    /// The media's fit size in a viewport, the way `crop_content_px` derives it — including the
    /// `.min(1.0)` NO-UPSCALE clamp, which is the whole reason the entry rule targets a margin
    /// rather than a fraction of fit.
    fn fit_size(media: (f32, f32), viewport: (f32, f32)) -> (f32, f32) {
        let scale = (viewport.0 / media.0).min(viewport.1 / media.1).min(1.0);
        (media.0 * scale, media.1 * scale)
    }

    /// The blank margin left on each side, as a fraction of the viewport, per axis.
    fn margins(disp: (f32, f32), viewport: (f32, f32), zoom: f32) -> (f32, f32) {
        (
            (viewport.0 - disp.0 * zoom) / 2.0 / viewport.0,
            (viewport.1 - disp.1 * zoom) / 2.0 / viewport.1,
        )
    }

    /// DRAGON-396: the crop session opens with at least [`CROP_ENTRY_MARGIN`] of the viewport free
    /// on EVERY side — and stops there. A LARGE capture fills the viewport at fit, so it zooms out
    /// to 0.6; a SMALL one is already shown at natural size with room around it (fit never
    /// upscales), so it must not move at all.
    #[test]
    fn crop_entry_leaves_a_fifth_of_the_viewport_free_on_every_side() {
        let viewport = (1000.0f32, 700.0f32);

        // 1. LARGE capture (4K into a 1000×700 viewport): fit is width-limited and fills the
        //    viewport exactly, so there is no margin to start with — zoom out to 60% of each axis.
        let disp = fit_size((3840.0, 2160.0), viewport);
        let z = crop_entry_zoom(viewport, disp);
        assert!((z - 0.6).abs() < 1e-4, "a viewport-filling capture opens at 0.6, got {z}");
        let (mx, my) = margins(disp, viewport, z);
        assert!(mx >= CROP_ENTRY_MARGIN - 1e-4 && my >= CROP_ENTRY_MARGIN - 1e-4, "{mx}, {my}");

        // 2. SMALL capture: fit does NOT upscale it, so it already sits at natural size with ~30%
        //    free on each side. The entry zoom must be EXACTLY fit — a literal no-op, which is the
        //    reported complaint (the old rule halved it again, to something absurdly small).
        let disp = fit_size((400.0, 300.0), viewport);
        assert_eq!(disp, (400.0, 300.0), "fit must not upscale a small capture");
        let z = crop_entry_zoom(viewport, disp);
        assert!((z - Viewport::FIT).abs() < 1e-6, "a small capture must not move, got {z}");

        // A capture sitting at EXACTLY the target margin is the boundary of that no-op.
        let z = crop_entry_zoom(viewport, (600.0, 420.0));
        assert!((z - Viewport::FIT).abs() < 1e-6, "already at 20% margin → no change, got {z}");
        // One hair inside it does zoom out (never IN, so it can only shrink).
        assert!(crop_entry_zoom(viewport, (620.0, 420.0)) < Viewport::FIT);

        // 3. WIDE capture in a TALL viewport: the axes disagree, and the LIMITING one must win. A
        //    3840×1080 capture fitted into a 700×1000 viewport is width-limited — it spans the full
        //    width (no margin at all) while already having ~40% free above and below. Keying off
        //    the height would call that done and leave zero room on the sides, which is the axis
        //    the user is about to crop on; the `min` is what zooms it out.
        let tall_viewport = (700.0f32, 1000.0f32);
        let wide = fit_size((3840.0, 1080.0), tall_viewport);
        assert!((wide.0 - 700.0).abs() < 1e-3, "the fit is width-limited: {wide:?}");
        let (mx0, my0) = margins(wide, tall_viewport, Viewport::FIT);
        assert!(mx0 < 1e-3 && my0 > CROP_ENTRY_MARGIN, "one axis clears at fit, one does not");
        let z = crop_entry_zoom(tall_viewport, wide);
        assert!((z - 0.6).abs() < 1e-4, "the limiting (horizontal) axis decides, got {z}");
        let (mx, my) = margins(wide, tall_viewport, z);
        assert!(mx >= CROP_ENTRY_MARGIN - 1e-4, "the tight axis reaches the margin: {mx}");
        assert!(my > mx, "the roomy axis keeps more than the minimum: {my} vs {mx}");

        // The floor is a guard, not the rule: it cannot bind for any real fit (see the constant),
        // but a degenerate `disp` larger than the viewport still cannot run away.
        assert!((crop_entry_zoom(viewport, (5000.0, 3500.0)) - CROP_ENTRY_MIN_ZOOM).abs() < 1e-6);
        // Unknown geometry opens at fit rather than guessing.
        assert_eq!(crop_entry_zoom((0.0, 0.0), (100.0, 100.0)), Viewport::FIT);
        assert_eq!(crop_entry_zoom(viewport, (0.0, 0.0)), Viewport::FIT);
    }

    #[test]
    fn crop_image_copies_the_rect_and_black_fills_out_of_bounds() {
        let mut base = RgbaImage::from_pixel(10, 10, ::image::Rgba([20, 40, 60, 255]));
        base.put_pixel(0, 0, ::image::Rgba([1, 2, 3, 255]));
        // A rect that starts one pixel LEFT of the image: column 0 of the output is black,
        // column 1 is the image's (0,0).
        let out = crop_image(&base, rect(-1.0, 0.0, 3.0, 2.0));
        assert_eq!(out.dimensions(), (3, 2));
        assert_eq!(*out.get_pixel(0, 0), ::image::Rgba([0, 0, 0, 255]), "out-of-bounds is black");
        assert_eq!(*out.get_pixel(1, 0), ::image::Rgba([1, 2, 3, 255]), "in-bounds copies through");
        assert_eq!(*out.get_pixel(2, 0), ::image::Rgba([20, 40, 60, 255]));
    }

    #[test]
    fn crop_image_fully_outside_is_all_black() {
        let base = RgbaImage::from_pixel(4, 4, ::image::Rgba([9, 9, 9, 255]));
        let out = crop_image(&base, rect(100.0, 100.0, 2.0, 2.0));
        assert_eq!(out.dimensions(), (2, 2));
        for p in out.pixels() {
            assert_eq!(*p, ::image::Rgba([0, 0, 0, 255]));
        }
    }
}
