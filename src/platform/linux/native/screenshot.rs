//! High-level still-screenshot grabs built on the low-level screencopy client
//! ([`crate::screencopy`]). Each public grab drives one or more single-frame
//! captures and stitches/composites/decorates the result into a finished image:
//!
//! - [`output`] / [`all_outputs`]: a whole monitor (or every monitor).
//! - [`region`] / [`stitch_region`]: a region in global coords, composited across
//!   the outputs it overlaps.
//! - [`window`] / [`windows`] / [`all_windows`]: a toplevel (or several) by handle.
//! - [`region_windows`]: the active-workspace windows inside a region, composited
//!   with the wallpaper excluded.
//! - [`WindowCaptureJob`]: the off-thread window-capture pipeline used by the app.

use cosmic_client_toolkit::screencopy::{
    CaptureOptions, CaptureSession, CaptureSource, Formats, Rect, ScreencopyCursorSessionData,
    ScreencopyFrameData, ScreencopySessionData,
};
use cosmic_client_toolkit::sctk::shm::slot::SlotPool;
use crate::screencopy::{
    ScreencopyClient, apply_transform, connect, connect_for_toplevel, normalize_to_rgba, outputs,
    pick_format,
};
use crate::selection::Selection;
use image::RgbaImage;
use std::collections::HashMap;
use wayland_client::{Connection, EventQueue};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;

/// Borrowed [`OutputFrame`] (pixels by reference) for stitching without cloning.
pub type OutputFrameRef<'a> = (&'a RgbaImage, (i32, i32), (i32, i32));
/// A captured cursor: the sprite (real alpha) plus its global position and hotspot.
pub type CursorSprite = (RgbaImage, (i32, i32), (i32, i32));
/// One output's launch-snapshot geometry: `(name, logical_pos, logical_size)`.
pub type OutputGeom = (String, (i32, i32), (i32, i32));
/// Captured output frame plus the output's logical position + size — enough to
/// crop a global-coords region out of it later (without the live output list).
pub type OutputFrame = (RgbaImage, (i32, i32), (i32, i32));
/// A captured region window: its pixels, global rect, and whether it's active.
type CapturedWindow = (RgbaImage, (i32, i32, i32, i32), bool);

/// Create a screencopy session for `source` and pump events until the compositor
/// reports its formats (`init_done`). Returns the live session plus those formats.
/// This is the shared front half of every still grab (driven via [`capture_source`]).
fn create_session_and_wait_formats(
    conn: &Connection,
    queue: &mut EventQueue<ScreencopyClient>,
    data: &mut ScreencopyClient,
    source: &CaptureSource,
    cursor: bool,
) -> Option<(CaptureSession, Formats)> {
    data.formats = None;
    data.result = None;
    let qh = queue.handle();
    let options = if cursor {
        CaptureOptions::PaintCursors
    } else {
        CaptureOptions::empty()
    };
    let session = data
        .screencopy_state
        .capturer()
        .create_session(source, options, &qh, ScreencopySessionData::default())
        .ok()?;
    conn.flush().ok()?;

    // Wait for BufferSize/ShmFormat -> Done (init_done sets formats).
    let mut guard = 0;
    while data.formats.is_none() {
        queue.blocking_dispatch(data).ok()?;
        guard += 1;
        if guard > 200 {
            return None;
        }
    }
    let formats = data.formats.clone()?;
    Some((session, formats))
}

/// Drive a single capture of `source` to completion and return its pixels
/// (orientation-corrected, RGBA8, top-left origin).
fn capture_source(
    conn: &Connection,
    queue: &mut EventQueue<ScreencopyClient>,
    data: &mut ScreencopyClient,
    source: &CaptureSource,
    cursor: bool,
) -> Option<RgbaImage> {
    let (session, formats) = create_session_and_wait_formats(conn, queue, data, source, cursor)?;
    let qh = queue.handle();
    let (w, h) = formats.buffer_size;
    if w == 0 || h == 0 {
        return None;
    }

    // Prefer a format whose memory order is already R,G,B,A (Abgr*); otherwise
    // fall back to Argb*/Xrgb* and swizzle on read.
    let (format, swizzle, force_opaque) = pick_format(&formats.shm_formats)?;
    let stride = w * 4;
    let mut pool = SlotPool::new((stride * h) as usize, &data.shm).ok()?;
    let (buffer, _) = pool
        .create_buffer(w as i32, h as i32, stride as i32, format)
        .ok()?;

    session.capture(
        buffer.wl_buffer(),
        &[Rect { x: 0, y: 0, width: w as i32, height: h as i32 }],
        &qh,
        ScreencopyFrameData::default(),
    );
    conn.flush().ok()?;

    let mut guard = 0;
    while data.result.is_none() {
        queue.blocking_dispatch(data).ok()?;
        guard += 1;
        if guard > 400 {
            return None;
        }
    }
    let frame = data.result.clone()?.ok()?;

    let canvas = buffer.canvas(&mut pool)?;
    let mut bytes = canvas.to_vec();
    normalize_to_rgba(&mut bytes, swizzle, force_opaque);
    let rgba = RgbaImage::from_raw(w, h, bytes)?;
    Some(apply_transform(rgba, frame.transform))
}

/// Probe the ext-image-copy-capture CURSOR session: grab the mouse cursor sprite (with its real
/// alpha), plus its on-screen position and hotspot. Returns `(sprite, position, hotspot)`. Drives
/// the hidden `--test cursor-capture` harness that verifies the clean cursor-capture path
/// end-to-end (before building the cursor-over-transparent feature). The download half mirrors
/// [`capture_source`]; only the session is a cursor session instead of an output/toplevel one.
pub fn capture_cursor() -> Option<CursorSprite> {
    let (conn, mut queue, mut data) = connect(false)?;
    let qh = queue.handle();
    // The cursor session needs a pointer object; we never read its events.
    let seat = data.seat_state.seats().next()?;
    let pointer = seat.get_pointer(&qh, ());

    // The cursor may be on any output; try each until one yields a cursor frame.
    for (output, _name, opos, osize) in outputs(&data) {
        // This output's buffer scale (physical px per logical px) — the divisor that turns
        // the Position event's transformed BUFFER pixels into logical (DRAGON-213). It must
        // come from the OUTPUT's mode, NOT the cursor frame's `buffer_size` (that's the tiny
        // sprite, e.g. 24x24). 1.0 on a 1x output, so the mapping stays byte-identical to the
        // historical `opos + pos` there. Computed before `output` moves into the source.
        let buffer_scale = output_buffer_scale(&data, &output, osize).unwrap_or(1.0);
        let source = CaptureSource::Output(output);
        data.formats = None;
        data.result = None;
        data.cursor_pos = None;
        data.cursor_hotspot = None;

        let Ok(cursor_session) = data.screencopy_state.capturer().create_cursor_session(
            &source,
            &pointer,
            &qh,
            ScreencopyCursorSessionData::default(),
        ) else {
            continue;
        };
        let Ok(session) = cursor_session.capture_session(&qh, ScreencopySessionData::default())
        else {
            continue;
        };
        if conn.flush().is_err() {
            continue;
        }

        // Wait for the session formats (init_done).
        let mut guard = 0;
        while data.formats.is_none() {
            if queue.blocking_dispatch(&mut data).is_err() || guard > 100 {
                break;
            }
            guard += 1;
        }
        // Borrowed, not cloned: `formats` is unused past `pick_format` below, well
        // before `data` needs a mutable borrow again (this loop runs once per output).
        let Some(formats) = data.formats.as_ref() else { continue };
        let (w, h) = formats.buffer_size;
        if w == 0 || h == 0 {
            continue;
        }
        let Some((format, swizzle, force_opaque)) = pick_format(&formats.shm_formats) else {
            continue;
        };
        let stride = w * 4;
        let Ok(mut pool) = SlotPool::new((stride * h) as usize, &data.shm) else { continue };
        let Ok((buffer, _)) = pool.create_buffer(w as i32, h as i32, stride as i32, format) else {
            continue;
        };
        session.capture(
            buffer.wl_buffer(),
            &[Rect { x: 0, y: 0, width: w as i32, height: h as i32 }],
            &qh,
            ScreencopyFrameData::default(),
        );
        if conn.flush().is_err() {
            continue;
        }

        let mut guard = 0;
        while data.result.is_none() {
            if queue.blocking_dispatch(&mut data).is_err() || guard > 200 {
                break;
            }
            guard += 1;
        }
        let Some(Ok(frame)) = data.result.clone() else { continue };
        // Drain any pending cursor Enter/Position/Hotspot events for this output.
        for _ in 0..2 {
            let _ = queue.roundtrip(&mut data);
        }
        // Only accept the output the cursor is actually on — a Position event means it's present
        // there. Otherwise the frame is an empty (fully transparent) cursor buffer; try the next.
        let Some(pos) = data.cursor_pos else { continue };
        let Some(canvas) = buffer.canvas(&mut pool) else { continue };
        let mut bytes = canvas.to_vec();
        normalize_to_rgba(&mut bytes, swizzle, force_opaque);
        let Some(rgba) = RgbaImage::from_raw(w, h, bytes) else { continue };
        let rgba = apply_transform(rgba, frame.transform);
        let hotspot = data.cursor_hotspot.unwrap_or((0, 0));
        let global = cursor_global_logical(opos, buffer_scale, pos);
        return Some((rgba, global, hotspot));
    }
    None
}

/// An output's buffer scale (physical pixels per logical point) for [`capture_cursor`]
/// (DRAGON-213): the current mode's physical width (transform-swapped for a quarter turn,
/// so it matches the transform-applied logical width the rest of the pipeline uses)
/// divided by the logical width `osize.0`. Returns `None` when the output advertises no
/// current mode / logical size, so the caller falls back to 1.0 (the 1x, byte-identical
/// path). NOT derivable from the cursor frame's `buffer_size` — that is the sprite, not
/// the output.
fn output_buffer_scale(
    data: &ScreencopyClient,
    output: &wayland_client::protocol::wl_output::WlOutput,
    osize: (i32, i32),
) -> Option<f32> {
    use wayland_client::protocol::wl_output::Transform;
    let info = data.output_state.info(output)?;
    let (pw, ph) = info.modes.iter().find(|m| m.current).map(|m| m.dimensions)?;
    // Upright physical width matching the (transform-applied) logical width.
    let phys_w = match info.transform {
        Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270 => ph,
        _ => pw,
    };
    if osize.0 <= 0 || phys_w <= 0 {
        return None;
    }
    Some(phys_w as f32 / osize.0 as f32)
}

/// Map a cursor Position event to a GLOBAL LOGICAL coordinate (DRAGON-213). The event's
/// `pos` is the cursor hotspot "relative to the main buffer's top left corner in
/// transformed BUFFER PIXEL coordinates" (ext-image-copy-capture-v1) — i.e. PHYSICAL
/// pixels of the source output, NOT logical. The composites are laid out in global
/// LOGICAL coordinates (`overlay_cursor` and `cursor_over_window` both work there), and
/// the output origin `opos` is logical, so the buffer offset is divided by the output's
/// `buffer_scale` (physical/logical) before being added to `opos`.
///
/// On a 1x output `buffer_scale == 1.0`, so this is byte-identical to the historical
/// `opos + pos`; on a scaled or fractional output (e.g. 2x) the old code placed the
/// cursor ~2x too far right/down.
fn cursor_global_logical(opos: (i32, i32), buffer_scale: f32, pos: (i32, i32)) -> (i32, i32) {
    let s = if buffer_scale > 0.0 { buffer_scale } else { 1.0 };
    let lx = opos.0 + (pos.0 as f32 / s).round() as i32;
    let ly = opos.1 + (pos.1 as f32 / s).round() as i32;
    (lx, ly)
}

/// Overlay the captured cursor sprite onto a windows-over-black/transparent composite: place its
/// hotspot at the cursor's global position mapped into the canvas. `cursor` = (sprite, global pos,
/// hotspot); the sprite and canvas share the output scale, so no rescale is needed.
fn overlay_cursor(
    canvas: &mut RgbaImage,
    region_x: i32,
    region_y: i32,
    scale: f32,
    cursor: &CursorSprite,
) {
    let (sprite, (gx, gy), (hx, hy)) = cursor;
    let px = ((*gx - region_x) as f32 * scale).round() as i64 - *hx as i64;
    let py = ((*gy - region_y) as f32 * scale).round() as i64 - *hy as i64;
    image::imageops::overlay(canvas, sprite, px, py);
}

/// Every monitor as a backend-agnostic description (name + logical geometry) —
/// the [`crate::platform::backend`] trait's output enumeration.
pub(crate) fn output_descs() -> Vec<crate::platform::backend::OutputDesc> {
    let Some((_conn, _queue, data)) = connect(false) else {
        return Vec::new();
    };
    outputs(&data)
        .into_iter()
        .map(|(_, name, logical_pos, logical_size)| crate::platform::backend::OutputDesc {
            name,
            logical_pos,
            logical_size,
        })
        .collect()
}

/// Every display's TRUE capture pixel footprint for the encoder benchmark (DRAGON-163):
/// each output's CURRENT mode's physical resolution (transform-swapped for a 90/270
/// rotation so the size matches the upright frame the screencopy worker captures — the
/// same `apply_transform` the recording path uses). Falls back to the logical size when
/// the compositor advertises no current mode. See
/// [`crate::platform::backend::BenchMonitor`].
pub(crate) fn bench_monitors() -> Vec<crate::platform::backend::BenchMonitor> {
    use wayland_client::protocol::wl_output::Transform;
    let Some((_conn, _queue, data)) = connect(false) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for output in data.output_state.outputs() {
        let Some(info) = data.output_state.info(&output) else {
            continue;
        };
        let Some(name) = info.name.clone() else {
            continue;
        };
        // The upright physical footprint: the current mode's dimensions, swapped for a
        // quarter-turn rotation (the worker captures the transform-applied frame).
        let (pw, ph) = info
            .modes
            .iter()
            .find(|m| m.current)
            .map(|m| m.dimensions)
            .or(info.logical_size)
            .unwrap_or((0, 0));
        if pw <= 0 || ph <= 0 {
            continue;
        }
        let (px_w, px_h) = match info.transform {
            Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270 => {
                (ph as u32, pw as u32)
            }
            _ => (pw as u32, ph as u32),
        };
        out.push(crate::platform::backend::BenchMonitor {
            label: format!("{name} ({px_w}x{px_h})"),
            name,
            px_w,
            px_h,
        });
    }
    out
}

/// Capture a whole monitor by output name. `cursor` overlays the pointer.
pub(crate) fn output(
    name: &str,
    cursor: Option<&CursorSprite>,
) -> Option<RgbaImage> {
    let (conn, mut queue, mut data) = connect(false)?;
    let (output, _, opos, osize) = outputs(&data).into_iter().find(|o| o.1 == name)?;
    let mut img =
        capture_source(&conn, &mut queue, &mut data, &CaptureSource::Output(output), false)?;
    // The LAUNCH-LOCKED cursor, not PaintCursors: the compositor would stamp the
    // pointer at its capture-instant position (wherever the mouse physically sits a
    // beat after teardown — usually the toolbar), not where it was when the tool
    // opened. The locked sprite matches the on-overlay indicator and every other path.
    if let Some(cur) = cursor {
        let scale = img.width() as f32 / osize.0.max(1) as f32;
        overlay_cursor(&mut img, opos.0, opos.1, scale, cur);
    }
    Some(img)
}

/// Capture every output (full frames) in one connection, keyed by output name.
/// Used for "freeze pixels": snapshot the whole screen at launch.
pub(crate) fn all_outputs(cursor: bool) -> HashMap<String, OutputFrame> {
    let mut out = HashMap::new();
    let Some((conn, mut queue, mut data)) = connect(false) else {
        return out;
    };
    let targets = outputs(&data);
    for (output, name, pos, size) in targets {
        if let Some(img) =
            capture_source(&conn, &mut queue, &mut data, &CaptureSource::Output(output), cursor)
        {
            out.insert(name, (img, pos, size));
        }
    }
    out
}

/// Capture a region (global logical coords) by capturing every output it
/// overlaps and stitching their on-screen parts together, then trimming the
/// off-monitor remainder. A region inside a single monitor captures just that
/// monitor; one straddling two monitors is composited across both. `cursor`
/// overlays the pointer.
pub(crate) fn region(
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    cursor: Option<&CursorSprite>,
) -> Option<RgbaImage> {
    let (conn, mut queue, mut data) = connect(false)?;
    let (sx0, sy0, sx1, sy1) = (x, y, x + w as i32, y + h as i32);
    let mut frames: Vec<OutputFrame> = Vec::new();
    for (output, _name, (ox, oy), (ow, oh)) in outputs(&data) {
        // Skip outputs the selection doesn't touch (no needless screencopy).
        if sx1.min(ox + ow) <= sx0.max(ox) || sy1.min(oy + oh) <= sy0.max(oy) {
            continue;
        }
        if let Some(img) =
            capture_source(&conn, &mut queue, &mut data, &CaptureSource::Output(output), false)
        {
            frames.push((img, (ox, oy), (ow, oh)));
        }
    }
    let refs: Vec<OutputFrameRef<'_>> =
        frames.iter().map(|(img, pos, size)| (img, *pos, *size)).collect();
    let mut img = stitch_region(&refs, x, y, w, h)?;
    // Launch-locked cursor overlay (see `output` for why not PaintCursors). The
    // stitch is at the dominant output's scale; derive it from the result itself.
    if let Some(cur) = cursor {
        let scale = img.width() as f32 / w.max(1) as f32;
        overlay_cursor(&mut img, x, y, scale, cur);
    }
    Some(img)
}

/// Composite the on-screen parts of a selection (global logical coords) from the
/// captured outputs it overlaps, trimming to the covered bounding box so any
/// fully off-monitor margin is removed (not padded with empty pixels). Each frame
/// is its image plus the output's logical position and size. Returns None when
/// the selection touches no output. Mixed-DPI outputs are resampled to the
/// dominant (largest-overlap) output's scale so one flat image comes out.
pub(crate) fn stitch_region(
    frames: &[OutputFrameRef<'_>],
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) -> Option<RgbaImage> {
    let (sx0, sy0, sx1, sy1) = (x, y, x + w as i32, y + h as i32);
    struct Part<'a> {
        img: &'a RgbaImage,
        scale: f32,
        ox: i32,
        oy: i32,
        gx0: i32,
        gy0: i32,
        gx1: i32,
        gy1: i32,
    }
    let mut parts: Vec<Part> = Vec::new();
    let (mut cx0, mut cy0, mut cx1, mut cy1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    let (mut ref_scale, mut best_area) = (1.0f32, -1i64);
    for (img, (ox, oy), (ow, oh)) in frames {
        let (gx0, gy0) = (sx0.max(*ox), sy0.max(*oy));
        let (gx1, gy1) = (sx1.min(ox + ow), sy1.min(oy + oh));
        if gx1 <= gx0 || gy1 <= gy0 {
            continue;
        }
        let scale = img.width() as f32 / (*ow).max(1) as f32;
        let area = (gx1 - gx0) as i64 * (gy1 - gy0) as i64;
        if area > best_area {
            best_area = area;
            ref_scale = scale;
        }
        cx0 = cx0.min(gx0);
        cy0 = cy0.min(gy0);
        cx1 = cx1.max(gx1);
        cy1 = cy1.max(gy1);
        parts.push(Part { img, scale, ox: *ox, oy: *oy, gx0, gy0, gx1, gy1 });
    }
    if parts.is_empty() {
        return None;
    }
    let cw = (((cx1 - cx0) as f32) * ref_scale).round() as u32;
    let ch = (((cy1 - cy0) as f32) * ref_scale).round() as u32;
    if cw == 0 || ch == 0 {
        return None;
    }
    let mut canvas = RgbaImage::new(cw, ch); // transparent; void stays empty
    for p in parts {
        let bx = (((p.gx0 - p.ox) as f32) * p.scale).round().max(0.0) as u32;
        let by = (((p.gy0 - p.oy) as f32) * p.scale).round().max(0.0) as u32;
        let mut bw = (((p.gx1 - p.gx0) as f32) * p.scale).round() as u32;
        let mut bh = (((p.gy1 - p.gy0) as f32) * p.scale).round() as u32;
        bw = bw.min(p.img.width().saturating_sub(bx));
        bh = bh.min(p.img.height().saturating_sub(by));
        if bw == 0 || bh == 0 {
            continue;
        }
        let sub = image::imageops::crop_imm(p.img, bx, by, bw, bh).to_image();
        // Resample to the reference scale only when DPIs differ.
        let dw = (((p.gx1 - p.gx0) as f32) * ref_scale).round().max(1.0) as u32;
        let dh = (((p.gy1 - p.gy0) as f32) * ref_scale).round().max(1.0) as u32;
        let sub = if dw != bw || dh != bh {
            image::imageops::resize(&sub, dw, dh, image::imageops::FilterType::Lanczos3)
        } else {
            sub
        };
        let dx = (((p.gx0 - cx0) as f32) * ref_scale).round() as i64;
        let dy = (((p.gy0 - cy0) as f32) * ref_scale).round() as i64;
        image::imageops::overlay(&mut canvas, &sub, dx, dy);
    }
    Some(canvas)
}

/// Capture a single toplevel directly by its stable identifier (occlusion-proof).
/// `cursor` overlays the pointer.
pub(crate) fn window(identifier: &str, cursor: bool) -> Option<RgbaImage> {
    // Wait only until THIS window appears (not for the whole toplevel list to
    // stabilize, as connect(true) does) — we already know which one we want.
    let (conn, mut queue, mut data) = connect_for_toplevel(identifier)?;
    let handle = data
        .toplevel_info_state
        .toplevels()
        .find(|t| t.identifier == identifier)
        .map(|t| t.foreign_toplevel.clone())?;
    capture_source(&conn, &mut queue, &mut data, &CaptureSource::Toplevel(handle), cursor)
}

/// Whether the window `identifier` is TRULY fullscreen beyond what the portable
/// [`crate::app::capture_flow::is_fullscreen`] geometry gate can already tell. On COSMIC a
/// fullscreen toplevel's rect equals its output rect, so the geometry gate is sufficient
/// and this refinement is a no-op (`false`) — the macOS Space-type override
/// (`platform/mac`) is the only place it matters. DRAGON-186 follow-up.
pub(crate) fn window_is_fullscreen(_identifier: &str) -> bool {
    false
}

/// Capture the active-workspace windows intersecting a region (global logical
/// coords) and composite them — i.e. the same area as a region/monitor grab but
/// with the wallpaper (and anything behind the windows) excluded. Background is
/// transparent when `keep_transparency`, else flat black. Each window is
/// rounded/flattened; the focused window also gets the active-hint border when
/// `border` is `Some((width_logical, rgba))`. Tiled layouts composite exactly;
/// overlapping windows are painted in enumeration order (no true z-order).
pub(crate) fn region_windows(
    sel: &Selection,
    radius_logical: f32,
    keep_transparency: bool,
    borders: crate::decoration::WindowBorders,
    cursor: Option<&CursorSprite>,
) -> Option<RgbaImage> {
    let (x, y, w, h) = (sel.x, sel.y, sel.width, sel.height);
    let groups = crate::platform::compositor::list_toplevels();
    let (conn, mut queue, mut data) = connect(true)?;

    // Output the region sits on → fallback scale if there are no windows.
    let outs = outputs(&data);
    let cx = x + w as i32 / 2;
    let cy = y + h as i32 / 2;
    let fallback_scale = outs
        .iter()
        .find(|(_, _, (ox, oy), (ow, oh))| cx >= *ox && cx < ox + ow && cy >= *oy && cy < oy + oh)
        .and_then(|(o, _, _, _)| data.output_state.info(o))
        .map(|i| i.scale_factor.max(1) as f32)
        .unwrap_or(1.0);

    // Capture every active-workspace window that intersects the region (raw).
    let wins: Vec<crate::platform::compositor::Toplevel> = groups.values().flatten().cloned().collect();
    let mut captured: Vec<CapturedWindow> = Vec::new();
    for win in wins {
        let (wx, wy, ww, wh) = win.rect;
        if wx + ww <= x || wx >= x + w as i32 || wy + wh <= y || wy >= y + h as i32 {
            continue; // no overlap with the region
        }
        let handle = data
            .toplevel_info_state
            .toplevels()
            .find(|t| t.identifier == win.id)
            .map(|t| t.foreign_toplevel.clone());
        let Some(handle) = handle else { continue };
        if let Some(img) =
            capture_source(&conn, &mut queue, &mut data, &CaptureSource::Toplevel(handle), false)
        {
            captured.push((img, win.rect, win.active));
        }
    }

    // Derive the real scale from a capture (handles fractional scale).
    let scale = captured
        .first()
        .map(|(img, rect, _)| img.width() as f32 / rect.2.max(1) as f32)
        .unwrap_or(fallback_scale);
    let cw = (w as f32 * scale).round() as u32;
    let ch = (h as f32 * scale).round() as u32;
    if cw == 0 || ch == 0 {
        return None;
    }
    // Transparent, or flat black when transparency is off.
    let mut canvas = if keep_transparency {
        RgbaImage::new(cw, ch)
    } else {
        RgbaImage::from_pixel(cw, ch, image::Rgba([0, 0, 0, 255]))
    };
    let r = (radius_logical * scale).round() as u32;
    // Frosted glass (DRAGON-218): when the theme frosts windows AND transparency
    // is kept, each window's backdrop — whatever is already on the canvas below
    // it (windows painted earlier; the transparent/black ground) — is blurred
    // within its rounded footprint before the window lands, reproducing live
    // glass between overlapping windows. `glass_config()` is None off COSMIC and
    // opaque mode has no alpha to see through, so `glass` is None and the
    // composite is byte-identical to today.
    let glass = keep_transparency
        .then(crate::app::theme::glass_config)
        .flatten()
        .filter(|g| g.frosted_windows);
    for (img, rect, active) in captured {
        // Per window, confirm its OWN alpha reads as a frosted libcosmic surface
        // before frosting it (DRAGON-218 follow-up): cosmic-comp only blurs behind
        // windows that set a blur region, and exposes no client signal for it, so
        // a window the user merely made translucent (a terminal) must stay sharp.
        let frost_sigma = glass
            .filter(|g| crate::glass::looks_frosted(&img, g.alpha))
            .map(|g| crate::glass::sigma_for_strength(g.strength_ordinal));
        let fin = crate::compose::finish_window(img, r, keep_transparency);
        let mut px = (((rect.0 - x) as f32) * scale).round() as i64;
        let mut py = (((rect.1 - y) as f32) * scale).round() as i64;
        if let Some(sigma) = frost_sigma {
            crate::glass::frost_region(&mut canvas, px, py, fin.width(), fin.height(), r, sigma);
        }
        // Each window gets its own border by focus: the ACTIVE border on the focused
        // window, the INACTIVE border on the others (DRAGON-191). Width 0 = no border.
        let bw = (borders.for_active(active).width as f32 * scale).round() as u32;
        let drawn = if bw > 0 {
            let color = borders.for_active(active).color;
            px -= bw as i64;
            py -= bw as i64;
            crate::compose::add_border(fin, bw, color, r + bw)
        } else {
            fin
        };
        image::imageops::overlay(&mut canvas, &drawn, px, py);
    }
    // The cursor is a compositor overlay, absent from the windows-only composite; overlay the
    // captured sprite so "Preserve mouse cursor" works without the wallpaper. Trimmed with the rest.
    if let Some(c) = cursor {
        overlay_cursor(&mut canvas, x, y, scale, c);
    }

    // Trim off-monitor areas: crop to where the selection actually overlaps a
    // monitor (union of per-output intersections), so a region dragged past a
    // screen edge doesn't keep the empty void — matching the wallpaper-on path.
    let (sx0, sy0, sx1, sy1) = (x, y, x + w as i32, y + h as i32);
    let (mut gx0, mut gy0, mut gx1, mut gy1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for (_, _, (ox, oy), (ow, oh)) in &outs {
        let ix0 = sx0.max(*ox);
        let iy0 = sy0.max(*oy);
        let ix1 = sx1.min(ox + ow);
        let iy1 = sy1.min(oy + oh);
        if ix1 <= ix0 || iy1 <= iy0 {
            continue;
        }
        gx0 = gx0.min(ix0);
        gy0 = gy0.min(iy0);
        gx1 = gx1.max(ix1);
        gy1 = gy1.max(iy1);
    }
    if gx1 <= gx0 || gy1 <= gy0 {
        return None; // selection lies entirely off every monitor
    }
    let crop_x = (((gx0 - x) as f32) * scale).round().max(0.0) as u32;
    let crop_y = (((gy0 - y) as f32) * scale).round().max(0.0) as u32;
    let crop_w = (((gx1 - gx0) as f32) * scale).round() as u32;
    let crop_h = (((gy1 - gy0) as f32) * scale).round() as u32;
    let crop_w = crop_w.min(canvas.width().saturating_sub(crop_x));
    let crop_h = crop_h.min(canvas.height().saturating_sub(crop_y));
    if crop_w == 0 || crop_h == 0 {
        return None;
    }
    if crop_x == 0 && crop_y == 0 && crop_w == canvas.width() && crop_h == canvas.height() {
        return Some(canvas); // fully on-monitor — no trim needed
    }
    Some(image::imageops::crop_imm(&canvas, crop_x, crop_y, crop_w, crop_h).to_image())
}

/// Frozen-mode inputs for [`region_windows_frozen`]: the launch-instant window
/// captures (z-order back to front) plus what's needed to size/trim the canvas when
/// there's nothing captured to derive the real scale from — each monitor's
/// (logical_pos, logical_size), and the fallback pixel scale.
pub(crate) struct FrozenWindows {
    pub(crate) captured: Vec<CapturedWindow>,
    pub(crate) out_rects: Vec<((i32, i32), (i32, i32))>,
    pub(crate) fallback_scale: f32,
}

/// Frozen counterpart of [`region_windows`]: composite already-captured window pixels (grabbed at
/// launch) over black/transparent, cropped to the region and trimmed to the monitors. Identical
/// compositing to the live path — only the pixel SOURCE differs (the launch freeze scene instead of
/// a live screencopy), so freeze + no-wallpaper keeps window transparency, rounding, and the
/// active-hint border. `captured` is the intersecting windows in z-order (back to front);
/// `out_rects` is each monitor's (logical_pos, logical_size); `fallback_scale` is used only when no
/// window was captured (an empty region → a correctly-sized black rectangle).
pub(crate) fn region_windows_frozen(
    frozen: FrozenWindows,
    sel: &Selection,
    radius_logical: f32,
    keep_transparency: bool,
    borders: crate::decoration::WindowBorders,
    cursor: Option<&CursorSprite>,
) -> Option<RgbaImage> {
    let FrozenWindows { captured, out_rects, fallback_scale } = frozen;
    let out_rects = &out_rects[..];
    let (x, y, w, h) = (sel.x, sel.y, sel.width, sel.height);
    // Real scale from a capture (handles fractional scale), else the caller's fallback.
    let scale = captured
        .first()
        .map(|(img, rect, _)| img.width() as f32 / rect.2.max(1) as f32)
        .unwrap_or(fallback_scale);
    let cw = (w as f32 * scale).round() as u32;
    let ch = (h as f32 * scale).round() as u32;
    if cw == 0 || ch == 0 {
        return None;
    }
    let mut canvas = if keep_transparency {
        RgbaImage::new(cw, ch)
    } else {
        RgbaImage::from_pixel(cw, ch, image::Rgba([0, 0, 0, 255]))
    };
    let r = (radius_logical * scale).round() as u32;
    // Frosted glass (DRAGON-218), identically to the live path: blur the canvas
    // below each window within its rounded footprint before it lands. None off
    // COSMIC / frosting off / opaque mode → byte-identical composite.
    let glass = keep_transparency
        .then(crate::app::theme::glass_config)
        .flatten()
        .filter(|g| g.frosted_windows);
    for (img, rect, active) in captured {
        // Frost only windows whose own alpha reads as a frosted libcosmic surface
        // (DRAGON-218 follow-up), matching the live path.
        let frost_sigma = glass
            .filter(|g| crate::glass::looks_frosted(&img, g.alpha))
            .map(|g| crate::glass::sigma_for_strength(g.strength_ordinal));
        let fin = crate::compose::finish_window(img, r, keep_transparency);
        let mut px = (((rect.0 - x) as f32) * scale).round() as i64;
        let mut py = (((rect.1 - y) as f32) * scale).round() as i64;
        if let Some(sigma) = frost_sigma {
            crate::glass::frost_region(&mut canvas, px, py, fin.width(), fin.height(), r, sigma);
        }
        // Per-window border by focus (DRAGON-191): Active on the focused window,
        // Inactive on the others; width 0 = no border.
        let bw = (borders.for_active(active).width as f32 * scale).round() as u32;
        let drawn = if bw > 0 {
            let color = borders.for_active(active).color;
            px -= bw as i64;
            py -= bw as i64;
            crate::compose::add_border(fin, bw, color, r + bw)
        } else {
            fin
        };
        image::imageops::overlay(&mut canvas, &drawn, px, py);
    }
    // Overlay the frozen cursor sprite (windows-only composite has none). Trimmed with the rest.
    if let Some(c) = cursor {
        overlay_cursor(&mut canvas, x, y, scale, c);
    }
    // Trim off-monitor areas (union of per-output intersections), matching the wallpaper-on path.
    let (sx0, sy0, sx1, sy1) = (x, y, x + w as i32, y + h as i32);
    let (mut gx0, mut gy0, mut gx1, mut gy1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for ((ox, oy), (ow, oh)) in out_rects {
        let ix0 = sx0.max(*ox);
        let iy0 = sy0.max(*oy);
        let ix1 = sx1.min(ox + ow);
        let iy1 = sy1.min(oy + oh);
        if ix1 <= ix0 || iy1 <= iy0 {
            continue;
        }
        gx0 = gx0.min(ix0);
        gy0 = gy0.min(iy0);
        gx1 = gx1.max(ix1);
        gy1 = gy1.max(iy1);
    }
    if gx1 <= gx0 || gy1 <= gy0 {
        return None; // selection lies entirely off every monitor
    }
    let crop_x = (((gx0 - x) as f32) * scale).round().max(0.0) as u32;
    let crop_y = (((gy0 - y) as f32) * scale).round().max(0.0) as u32;
    let crop_w = (((gx1 - gx0) as f32) * scale).round() as u32;
    let crop_h = (((gy1 - gy0) as f32) * scale).round() as u32;
    let crop_w = crop_w.min(canvas.width().saturating_sub(crop_x));
    let crop_h = crop_h.min(canvas.height().saturating_sub(crop_y));
    if crop_w == 0 || crop_h == 0 {
        return None;
    }
    if crop_x == 0 && crop_y == 0 && crop_w == canvas.width() && crop_h == canvas.height() {
        return Some(canvas);
    }
    Some(image::imageops::crop_imm(&canvas, crop_x, crop_y, crop_w, crop_h).to_image())
}

/// Capture many toplevels (by identifier) at full resolution in one connection.
/// Used to populate the window-picker grid at launch (the caller flattens,
/// rounds, and downscales).
pub(crate) fn windows(identifiers: &[String]) -> HashMap<String, RgbaImage> {
    let mut out = HashMap::new();
    let Some((conn, mut queue, mut data)) = connect(true) else {
        return out;
    };
    // Snapshot id -> foreign handle up front (borrow ends before capture).
    let handles: Vec<(String, ExtForeignToplevelHandleV1)> = identifiers
        .iter()
        .filter_map(|id| {
            data.toplevel_info_state
                .toplevels()
                .find(|t| &t.identifier == id)
                .map(|t| (id.clone(), t.foreign_toplevel.clone()))
        })
        .collect();
    for (id, handle) in handles {
        // Raw full-res captures (no cursor); the caller flattens/rounds/scales.
        if let Some(img) =
            capture_source(&conn, &mut queue, &mut data, &CaptureSource::Toplevel(handle), false)
        {
            out.insert(id, img);
        }
        // Yield a frame between full-res screencopies. Back-to-back captures keep
        // cosmic-comp's renderer busy the whole time, which starves presentation
        // of OUR overlay — the loading spinner can't paint and the screen looks
        // frozen until every window is grabbed. A short gap lets the compositor
        // present a spinner frame between captures.
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    out
}

/// Hidden `--bench-capture` harness: times the window-capture pipeline stages
/// (Wayland connect + toplevel screencopy + output screencopy + PNG encode +
/// wallpaper decode) so the pipeline can be profiled from the CLI. Prints to
/// stderr; makes no state changes.
pub fn bench_window_capture() {
    use std::time::Instant;
    eprintln!("== cosmic-capture-kit capture bench (3 rounds) ==");
    for round in 0..3 {
        let t = Instant::now();
        let conn = connect(true);
        let connect_true = t.elapsed();
        let Some((conn, mut queue, mut data)) = conn else {
            eprintln!("connect(true) failed (is WAYLAND_DISPLAY set?)");
            return;
        };
        let tops: Vec<(String, ExtForeignToplevelHandleV1)> = data
            .toplevel_info_state
            .toplevels()
            .map(|t| (t.identifier.clone(), t.foreign_toplevel.clone()))
            .collect();
        let outs = outputs(&data);
        eprint!("round {round}: toplevels={} connect(true)={connect_true:?}", tops.len());
        if let Some((id, h)) = tops.first() {
            let t = Instant::now();
            let img = capture_source(
                &conn,
                &mut queue,
                &mut data,
                &CaptureSource::Toplevel(h.clone()),
                false,
            );
            eprint!(
                " | toplevel[{}] capture_source={:?} dims={:?}",
                id,
                t.elapsed(),
                img.as_ref().map(|i| (i.width(), i.height()))
            );
            if let Some(img) = img {
                let t = Instant::now();
                let mut buf = std::io::Cursor::new(Vec::new());
                let _ = image::DynamicImage::ImageRgba8(img)
                    .write_to(&mut buf, image::ImageFormat::Png);
                eprint!(" png_encode={:?} ({}KB)", t.elapsed(), buf.into_inner().len() / 1024);
            }
        }
        if let Some((o, name, _, sz)) = outs.into_iter().next() {
            let t = Instant::now();
            let _ = capture_source(&conn, &mut queue, &mut data, &CaptureSource::Output(o), false);
            eprint!(" | output[{name} {sz:?}] capture_source={:?}", t.elapsed());
        }
        eprintln!();
    }
    if let Some(p) = bench_wallpaper_path() {
        let t = Instant::now();
        let wp = crate::wallpaper::decode_wallpaper(&p);
        let cold = t.elapsed();
        let t = Instant::now();
        let _warm = crate::wallpaper::decode_wallpaper(&p);
        eprintln!(
            "wallpaper decode [{}] cold={cold:?} warm(memo)={:?} dims={:?}",
            p.display(),
            t.elapsed(),
            wp.as_ref().map(|i| (i.width(), i.height()))
        );
    }
    // Targeted connect: time how long until a known window id is visible (what the
    // real capture path now waits for, vs connect(true)'s full-list stabilization).
    if let Some((_, _, data)) = connect(true)
        && let Some(id) = data.toplevel_info_state.toplevels().next().map(|t| t.identifier.clone())
    {
        let t = Instant::now();
        let _ = connect_for_toplevel(&id);
        eprintln!("connect_for_toplevel([{id}]) = {:?}", t.elapsed());
    }
}

/// First per-output (or `all`) wallpaper path from the cosmic-bg config (bench only).
fn bench_wallpaper_path() -> Option<std::path::PathBuf> {
    let dir = dirs::config_dir()?.join("cosmic/com.system76.CosmicBackground/v1");
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("output.") || name == "all") {
            continue;
        }
        if let Ok(t) = std::fs::read_to_string(entry.path())
            && let Some(i) = t.find("Path(")
        {
            let after = &t[i + 5..];
            if let Some(q1) = after.find('"') {
                let rest = &after[q1 + 1..];
                if let Some(q2) = rest.find('"') {
                    let p = std::path::PathBuf::from(&rest[..q2]);
                    if p.exists() {
                        return Some(p);
                    }
                }
            }
        }
    }
    None
}

/// Everything the off-thread window capture needs, owned so it can move into a
/// worker thread (no `&self` / no `App`). Built on the UI thread in `do_pixel_capture`,
/// so the picker overlay can tear down while `run()` does the slow work.
pub(crate) struct WindowCaptureJob {
    pub(crate) id: String,
    pub(crate) cursor: bool,
    pub(crate) sel: Selection,
    pub(crate) capture_transparency: bool,
    pub(crate) capture_wallpaper: bool,
    pub(crate) window_radius: f32,
    /// The border to draw around this window (DRAGON-191). A single-window capture is
    /// the active window, so this is the ACTIVE border spec; width 0 = no border.
    pub(crate) border: crate::decoration::BorderSpec,
    pub(crate) window_shadow: bool,
    /// TOTAL margin from the window edge (border lives inside it); 0 when padding off.
    pub(crate) pad_logical: f32,
    pub(crate) dark: bool,
    /// Launch snapshot's per-output (name, logical_pos, logical_size) for the composite.
    pub(crate) frozen_geom: Vec<OutputGeom>,
    /// Freeze mode: the window's pixels grabbed at launch. When `Some`, `run()` decorates THESE
    /// instead of a live screencopy, so a window grab is frozen (motion stopped) yet still
    /// honours transparency + wallpaper-behind exactly like the live path.
    pub(crate) frozen_px: Option<image::RgbaImage>,
    /// Freeze mode: the captured cursor (sprite, global pos, hotspot) to overlay — the frozen
    /// window pixels were grabbed without a cursor. `None` for a live grab (which paints its own
    /// via the `cursor` flag above).
    pub(crate) cursor_overlay: Option<CursorSprite>,
}

impl WindowCaptureJob {
    /// Grab the toplevel, finish/round it, add the border/shadow/padding, then put the
    /// chosen background behind it (wallpaper / black / kept transparent). Same pipeline
    /// the UI thread used to run inline; returns None if the toplevel grab fails.
    pub(crate) fn run(mut self) -> Option<image::RgbaImage> {
        // Transparency multiplier parked (linear-light over() reproduces the live
        // see-through, making it redundant). Kept commented for easy revival:
        // if self.capture_transparency && multiplier > 0.0 { apply_transparency_multiplier }
        // Freeze mode uses the launch-instant pixels; otherwise grab the toplevel live now.
        let raw = match self.frozen_px.take() {
            Some(px) => px,
            None => window(&self.id, self.cursor)?,
        };
        // Scale is pixels-per-point of the display — derive it from the RAW grab BEFORE
        // any trimming so a gutter trim never distorts it.
        let scale = raw.width() as f32 / self.sel.width.max(1) as f32;
        let r = (self.window_radius * scale).round() as u32;
        // DRAGON-190 (platform-agnostic): trim any FULLY-transparent dead gutter (e.g. a
        // CSD shadow margin) off the raw window BEFORE rounding/decoration/wallpaper, so
        // the border hugs the real content and the wallpaper aligns to it. The guard is
        // the theme radius `r` that `finish_window` rounds to (Linux screencopy has no
        // native alpha corners to read off, unlike macOS SCK). A left/top trim shifts the
        // origin, so bump the effective selection top-left (logical points) to keep the
        // wallpaper crop + cursor overlay aligned. An opaque window trims to a no-op.
        let (img, (trim_left, trim_top, _, _)) = crate::compose::trim_transparent_gutter(&raw, r);
        if trim_left > 0 || trim_top > 0 {
            self.sel.x += (trim_left as f32 / scale.max(f32::EPSILON)).round() as i32;
            self.sel.y += (trim_top as f32 / scale.max(f32::EPSILON)).round() as i32;
        }
        // Frosted glass (DRAGON-218 follow-up): decide from the RAW window alpha —
        // before `finish_window` consumes it — whether this window reads as a
        // frosted libcosmic surface (its backdrop painted at the theme's
        // blurred_alpha). cosmic-comp only blurs behind windows that set a blur
        // region and exposes no client signal for it, so a window the user merely
        // made translucent (a terminal) must stay sharp. None off COSMIC / frosting
        // off / opaque mode → byte-identical composite.
        let glass = self
            .capture_transparency
            .then(crate::app::theme::glass_config)
            .flatten()
            .filter(|g| g.frosted_windows)
            .filter(|g| crate::glass::looks_frosted(&img, g.alpha));
        let fin = crate::compose::finish_window(img, r, self.capture_transparency);
        // Window-content dims within the decorated canvas — the frosted-glass
        // footprint (DRAGON-218), captured before decoration consumes `fin`.
        let (win_w, win_h) = (fin.width(), fin.height());
        let bw_logical = self.border.to_compose().map(|(w, _)| w).unwrap_or(0.0);
        let (bordered, outer_r) = match self.border.to_compose() {
            Some((_, color)) => {
                let bw = (bw_logical * scale).round() as u32;
                (crate::compose::add_border(fin, bw, color, r + bw), r + bw)
            }
            None => (fin, r),
        };
        let margin_logical = (self.pad_logical - bw_logical).max(0.0);
        let margin_px = (margin_logical * scale).round() as u32;
        let total_margin_logical = bw_logical + margin_logical; // == padding when on
        // The shadow draws into whatever margin exists and is clipped at the edges
        // when there isn't room (padding off) — we never grow the canvas to fit it.
        let decorated = if self.window_shadow {
            crate::compose::with_shadow(bordered, margin_px, outer_r, scale, self.dark)
        } else if margin_px > 0 {
            crate::compose::pad_transparent(bordered, margin_px)
        } else {
            bordered
        };
        // Frosted glass (DRAGON-218): when the window reads as a frosted libcosmic
        // surface (decided above) the re-rendered wallpaper is blurred within the
        // window's rounded footprint before the composite, so the preserved alpha
        // reveals it like live glass. The footprint sits at the total margin offset
        // (padding + border ring) in the decorated canvas; sigma comes from
        // cosmic-comp's strength table (`glass::sigma_for_strength`).
        let frost = glass.map(|g| FrostSpec {
            off: margin_px as i64 + (bw_logical * scale).round() as i64,
            w: win_w,
            h: win_h,
            radius: r,
            sigma: crate::glass::sigma_for_strength(g.strength_ordinal),
        });
        let mut out = if self.capture_wallpaper {
            // Composite over JUST the wallpaper behind the window (no other windows);
            // rounded corners + the margin reveal the desktop whether or not the window
            // is translucent.
            composite_over_wallpaper(
                decorated,
                &self.sel,
                total_margin_logical,
                scale,
                &self.frozen_geom,
                frost,
            )
        } else if !self.capture_transparency {
            // Opaque, flat-black background.
            crate::compose::on_black(decorated)
        } else {
            // Keep the window's own transparency, plus the shadow halo.
            decorated
        };
        // Freeze grabbed the window without a cursor; overlay the captured sprite. The window's
        // content starts at the total margin offset in the decorated image, at `scale`.
        // ONLY when the pointer was actually over the window (DRAGON-213): the decorated
        // canvas extends past the window content (padding margin, shadow, wallpaper-
        // behind), so relying on canvas clipping alone floated a cursor beside the
        // window whenever the pointer was merely nearby at launch.
        if let Some((sprite, (gx, gy), (hx, hy))) = self.cursor_overlay.as_ref()
            && cursor_over_window(*gx, *gy, &self.sel)
        {
            let off = (total_margin_logical * scale).round() as i64;
            let px = ((*gx - self.sel.x) as f32 * scale).round() as i64 + off - *hx as i64;
            let py = ((*gy - self.sel.y) as f32 * scale).round() as i64 + off - *hy as i64;
            image::imageops::overlay(&mut out, sprite, px, py);
        }
        Some(out)
    }
}

/// Whether the launch-locked pointer position (global logical) lies within the
/// picked window's logical rect (post gutter-trim — the caller already bumped
/// `sel` for any trimmed margin). The window-capture cursor overlay is gated on
/// this (DRAGON-213): a pointer outside the window must not render in the
/// capture's padding/shadow/wallpaper margins.
fn cursor_over_window(gx: i32, gy: i32, sel: &crate::selection::Selection) -> bool {
    gx >= sel.x
        && gx < sel.x + sel.width as i32
        && gy >= sel.y
        && gy < sel.y + sel.height as i32
}

/// Composite a finished (bordered/padded) window over ONLY the desktop wallpaper
/// behind it — no occluding windows. Re-renders the wallpaper file the way cosmic
/// placed it on the window's output (per-output image + Zoom/Stretch scaling) and crops
/// the window's footprint out of it, so transparency, rounded corners, and the padding
/// margin reveal the real wallpaper at the right spot. `frozen_geom` is the launch
/// snapshot's per-output (name, logical_pos, logical_size) — the overlay (and
/// `self.outputs`) is torn down before the capture runs, so we can't use it. Falls back
/// to the window as-is if the output / wallpaper can't be resolved.
/// Frosted-glass reproduction spec for one composited window (DRAGON-218): the
/// window content's rounded footprint within the decorated canvas — offset
/// `off` (both axes; the padding + border ring), `w`×`h` physical px, corners
/// at `radius` — and the blur `sigma` for the user's frosted strength. Built
/// only when the glass reader says frosted_windows AND transparency is kept.
struct FrostSpec {
    off: i64,
    w: u32,
    h: u32,
    radius: u32,
    sigma: f32,
}

fn composite_over_wallpaper(
    bordered: image::RgbaImage,
    sel: &Selection,
    border_logical: f32,
    scale: f32,
    frozen_geom: &[OutputGeom],
    frost: Option<FrostSpec>,
) -> image::RgbaImage {
    let cx = sel.x + sel.width as i32 / 2;
    let cy = sel.y + sel.height as i32 / 2;
    let Some((name, pos, size)) = frozen_geom.iter().find(|(_, pos, size)| {
        let ((ox, oy), (ow, oh)) = (*pos, *size);
        cx >= ox && cx < ox + ow && cy >= oy && cy < oy + oh
    }) else {
        return bordered;
    };
    let Some((path, stretch)) = wallpaper_for_output(name) else {
        return bordered;
    };
    let bnd = border_logical.round() as i32;
    let out_w = (size.0 as f32 * scale).round() as i32;
    let out_h = (size.1 as f32 * scale).round() as i32;
    let (rw, rh) = (bordered.width() as i32, bordered.height() as i32);
    // Top-left of the padded image within the output (physical px).
    let mut rx = (((sel.x - bnd - pos.0) as f32) * scale).round() as i32;
    let mut ry = (((sel.y - bnd - pos.1) as f32) * scale).round() as i32;
    // Nudge the crop inside the output so the padding margin stays even on all sides
    // when the window sits near a screen edge. (When the footprint is larger than the
    // output, wallpaper_crop clamps + rescales.)
    if rw <= out_w {
        rx = rx.clamp(0, out_w - rw);
    }
    if rh <= out_h {
        ry = ry.clamp(0, out_h - rh);
    }
    match crate::wallpaper::wallpaper_crop(
        &path,
        stretch,
        out_w.max(1) as u32,
        out_h.max(1) as u32,
        rx,
        ry,
        rw.max(1) as u32,
        rh.max(1) as u32,
    ) {
        Some(mut wp) => {
            // Frosted glass (DRAGON-218): blur (+ grain) the wallpaper within the
            // window's rounded footprint before compositing, so the window's
            // preserved alpha reveals a frosted backdrop like the live compositor.
            // The margins/shadow outside the footprint keep the sharp wallpaper
            // (cosmic only blurs BEHIND the window).
            if let Some(f) = frost {
                crate::glass::frost_region(&mut wp, f.off, f.off, f.w, f.h, f.radius, f.sigma);
            }
            crate::compose::over(wp, &bordered)
        }
        None => bordered,
    }
}

/// The wallpaper image + whether cosmic stretches it, for a specific output, matching how
/// cosmic-bg renders it: the per-output entry, or the shared `all` entry when "same on
/// all" is set, falling back to `all`. `scaling_mode: Stretch` distorts to fill; anything
/// else (Zoom / Fit) cover-fits, which is what `wallpaper_crop` reproduces.
fn wallpaper_for_output(name: &str) -> Option<(std::path::PathBuf, bool)> {
    let dir = dirs::config_dir()?.join("cosmic/com.system76.CosmicBackground/v1");
    let same = std::fs::read_to_string(dir.join("same-on-all"))
        .map(|s| s.trim() == "true")
        .unwrap_or(true);
    let primary = if same {
        "all".to_string()
    } else {
        format!("output.{name}")
    };
    let text = std::fs::read_to_string(dir.join(&primary))
        .or_else(|_| std::fs::read_to_string(dir.join("all")))
        .ok()?;
    let i = text.find("Path(\"")? + 6;
    let e = text[i..].find('"')?;
    let path = std::path::PathBuf::from(&text[i..i + e]);
    let stretch = text.contains("scaling_mode: Stretch");
    Some((path, stretch))
}

#[cfg(test)]
mod cursor_coord_tests {
    use super::cursor_global_logical;

    // 1x output: scale 1.0, so the mapping is the historical `opos + pos` — byte-identical
    // (the DRAGON-213 fix must not move 1x cursors, which is the user's 5120x1440 DP-3).
    #[test]
    fn unscaled_output_is_origin_plus_offset() {
        assert_eq!(cursor_global_logical((0, 0), 1.0, (100, 200)), (100, 200));
        // A second output to the right at (1920,0); a buffer offset of (50,60) -> (1970,60).
        assert_eq!(cursor_global_logical((1920, 0), 1.0, (50, 60)), (1970, 60));
        // A degenerate scale (<= 0) also falls back to 1.0 (no divide-by-zero blowup).
        assert_eq!(cursor_global_logical((0, 0), 0.0, (100, 200)), (100, 200));
    }

    // 2x (HiDPI) output: the Position event is in PHYSICAL buffer pixels, so it must be
    // halved before being added to the LOGICAL origin. The old `opos + pos` placed the
    // cursor ~2x too far into the output — the bug on a scaled monitor.
    #[test]
    fn scaled_output_divides_buffer_offset_by_scale() {
        // Buffer offset (1000,800) at scale 2 is logical (500,400); origin (2560,0) ->
        // global (3060,400).
        assert_eq!(cursor_global_logical((2560, 0), 2.0, (1000, 800)), (3060, 400));
    }

    // Fractional scale (1.5x): rounds to the nearest logical pixel.
    #[test]
    fn fractional_scale_rounds() {
        // Buffer offset (150,150) at scale 1.5 -> (100,100).
        assert_eq!(cursor_global_logical((0, 0), 1.5, (150, 150)), (100, 100));
    }
}

#[cfg(test)]
mod cursor_overlay_tests {
    use super::cursor_over_window;
    use crate::selection::Selection;

    fn sel(x: i32, y: i32, w: u32, h: u32) -> Selection {
        Selection { x, y, width: w, height: h, output: None, window_id: None }
    }

    // The DRAGON-213 gate: the pointer must be INSIDE the window rect for the
    // launch-locked cursor to render — nearby (the padding/shadow/wallpaper
    // margins of the decorated canvas) is out.
    #[test]
    fn pointer_inside_the_window_renders() {
        let s = sel(100, 200, 800, 600);
        assert!(cursor_over_window(100, 200, &s)); // top-left corner (inclusive)
        assert!(cursor_over_window(500, 400, &s)); // middle
        assert!(cursor_over_window(899, 799, &s)); // bottom-right inside edge
    }

    #[test]
    fn pointer_outside_the_window_is_clipped() {
        let s = sel(100, 200, 800, 600);
        assert!(!cursor_over_window(99, 400, &s)); // just left
        assert!(!cursor_over_window(900, 400, &s)); // just right (exclusive edge)
        assert!(!cursor_over_window(500, 199, &s)); // just above
        assert!(!cursor_over_window(500, 800, &s)); // just below (exclusive edge)
        assert!(!cursor_over_window(-50, -50, &s)); // far away / other monitor
    }
}
