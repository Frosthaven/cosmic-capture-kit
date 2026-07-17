//! Low-level Wayland screencopy CLIENT (cctk's `screencopy` module), the shared
//! pixel-grab foundation under both the still-screenshot grabs (`crate::screenshot`)
//! and the video recording loops (`crate::record`). This is exactly how
//! xdg-desktop-portal-cosmic grabs pixels:
//!
//! - Monitor / region: capture a `CaptureSource::Output`, then crop for a region.
//! - Window: capture a `CaptureSource::Toplevel` directly by its foreign handle,
//!   so an occluded window is captured cleanly with no need to focus/raise it.
//!
//! The shm buffer is requested as `Abgr8888` whose little-endian byte order is
//! already R,G,B,A, so it drops straight into an `image::RgbaImage`. The frame's
//! reported `transform` is applied (orientation) before cropping/saving.

use cosmic_client_toolkit::screencopy::{
    CaptureCursorSession, CaptureFrame, CaptureSession, Formats, Frame, Rect, ScreencopyFrameData,
    ScreencopyHandler, ScreencopyState,
};
use cosmic_client_toolkit::sctk;
#[cfg(feature = "zero-copy")]
use cosmic_client_toolkit::sctk::dmabuf::{DmabufFeedback, DmabufHandler, DmabufState};
use cosmic_client_toolkit::sctk::output::{OutputHandler, OutputState};
use cosmic_client_toolkit::sctk::registry::{ProvidesRegistryState, RegistryState};
use cosmic_client_toolkit::sctk::seat::{Capability, SeatHandler, SeatState};
use cosmic_client_toolkit::sctk::shm::{Shm, ShmHandler, slot::SlotPool};
use cosmic_client_toolkit::toplevel_info::{ToplevelInfoHandler, ToplevelInfoState};
use image::RgbaImage;
use wayland_client::globals::registry_queue_init;
#[cfg(feature = "zero-copy")]
use wayland_client::protocol::wl_buffer;
use wayland_client::protocol::wl_output::{Transform, WlOutput};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_shm;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
#[cfg(feature = "zero-copy")]
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1;
#[cfg(feature = "zero-copy")]
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1;

/// An enumerated output: its handle, name, logical position, and logical size.
pub(crate) type OutputInfo = (WlOutput, String, (i32, i32), (i32, i32));

/// The screencopy client state: the bound Wayland globals plus the per-capture
/// scratch (`formats`/`result`) the handlers fill in. Shared by the screenshot
/// grabs and the recording loops; its fields are `pub(crate)` so both can drive it.
pub(crate) struct ScreencopyClient {
    pub(crate) registry_state: RegistryState,
    pub(crate) output_state: OutputState,
    pub(crate) toplevel_info_state: ToplevelInfoState,
    pub(crate) screencopy_state: ScreencopyState,
    pub(crate) shm: Shm,
    /// Seat binding, used only to enumerate a `wl_seat` so a `wl_pointer` can be created for the
    /// ext-image-copy-capture cursor session (the cursor-capture probe).
    pub(crate) seat_state: SeatState,
    /// Filled by the cursor session's Position event: the cursor's on-screen position.
    pub(crate) cursor_pos: Option<(i32, i32)>,
    /// Filled by the cursor session's Hotspot event: the cursor's hotspot offset.
    pub(crate) cursor_hotspot: Option<(i32, i32)>,
    /// Bound `zwp_linux_dmabuf_v1`, used to allocate GPU buffers for the
    /// screencopy zero-copy recording path. Harmless when unused (a screenshot
    /// just never asks it for a buffer); absent compositors fail gracefully.
    #[cfg(feature = "zero-copy")]
    pub(crate) dmabuf_state: DmabufState,
    /// Set when the session reports its formats (init_done).
    pub(crate) formats: Option<Formats>,
    /// Set when the frame finishes: Ok(frame) on ready, Err on failed/stopped.
    pub(crate) result: Option<Result<Frame, ()>>,
}

impl ProvidesRegistryState for ScreencopyClient {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    sctk::registry_handlers![OutputState, SeatState];
}

impl SeatHandler for ScreencopyClient {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
    fn new_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, _: Capability) {}
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, _: Capability) {
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

// The cursor session needs a `wl_pointer` object, but we never read pointer events (we only want
// the cursor's captured image + its Position/Hotspot, which arrive on the cursor session itself).
// So bind the pointer with `()` data and ignore every event.
impl Dispatch<WlPointer, ()> for ScreencopyClient {
    fn event(
        _: &mut Self,
        _: &WlPointer,
        _: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl OutputHandler for ScreencopyClient {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}

impl ToplevelInfoHandler for ScreencopyClient {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevel_info_state
    }
    fn new_toplevel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &ExtForeignToplevelHandleV1) {}
    fn update_toplevel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &ExtForeignToplevelHandleV1) {}
    fn toplevel_closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &ExtForeignToplevelHandleV1) {}
}

impl ShmHandler for ScreencopyClient {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

#[cfg(feature = "zero-copy")]
impl DmabufHandler for ScreencopyClient {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }
    // We allocate buffers eagerly with `create_immed`, so the async
    // created/failed/released callbacks carry no state we act on.
    fn dmabuf_feedback(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpLinuxDmabufFeedbackV1,
        _: DmabufFeedback,
    ) {
    }
    fn created(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpLinuxBufferParamsV1,
        _: wl_buffer::WlBuffer,
    ) {
    }
    fn failed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &ZwpLinuxBufferParamsV1) {}
    fn released(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_buffer::WlBuffer) {}
}

impl ScreencopyHandler for ScreencopyClient {
    fn screencopy_state(&mut self) -> &mut ScreencopyState {
        &mut self.screencopy_state
    }
    fn init_done(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &CaptureSession,
        formats: &Formats,
    ) {
        self.formats = Some(formats.clone());
    }
    fn ready(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &CaptureFrame, frame: Frame) {
        self.result = Some(Ok(frame));
    }
    fn failed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &CaptureFrame,
        _: WEnum<cosmic_client_toolkit::screencopy::FailureReason>,
    ) {
        if self.result.is_none() {
            self.result = Some(Err(()));
        }
    }
    fn stopped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &CaptureSession) {
        if self.result.is_none() {
            self.result = Some(Err(()));
        }
    }
    fn cursor_position(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &CaptureCursorSession,
        x: i32,
        y: i32,
    ) {
        self.cursor_pos = Some((x, y));
    }
    fn cursor_hotspot(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &CaptureCursorSession,
        x: i32,
        y: i32,
    ) {
        self.cursor_hotspot = Some((x, y));
    }
}

sctk::delegate_output!(ScreencopyClient);
sctk::delegate_seat!(ScreencopyClient);
sctk::delegate_registry!(ScreencopyClient);
sctk::delegate_shm!(ScreencopyClient);
#[cfg(feature = "zero-copy")]
sctk::delegate_dmabuf!(ScreencopyClient);
cosmic_client_toolkit::delegate_toplevel_info!(ScreencopyClient);
cosmic_client_toolkit::delegate_screencopy!(ScreencopyClient);

/// Connect and bind the states needed for capture (output, toplevel-info,
/// screencopy, shm). Outputs enumerate within a couple of roundtrips; toplevels
/// arrive asynchronously, so when `wait_toplevels` is set we dispatch over a
/// short real-time window until the toplevel count settles (see `gather`).
/// Open the Wayland connection and bind the capture globals, pumping just enough
/// roundtrips to register them. The toplevel list is NOT guaranteed populated yet
/// (its events trickle in over later roundtrips) — callers that need it use the
/// wait loop in [`connect`] or the targeted wait in [`connect_for_toplevel`].
fn connect_raw() -> Option<(Connection, EventQueue<ScreencopyClient>, ScreencopyClient)> {
    let conn = Connection::connect_to_env().ok()?;
    let (globals, mut queue) = registry_queue_init::<ScreencopyClient>(&conn).ok()?;
    let qh = queue.handle();
    let registry_state = RegistryState::new(&globals);
    let output_state = OutputState::new(&globals, &qh);
    let toplevel_info_state = ToplevelInfoState::new(&registry_state, &qh);
    let screencopy_state = ScreencopyState::new(&globals, &qh);
    let shm = Shm::bind(&globals, &qh).ok()?;
    let seat_state = SeatState::new(&globals, &qh);
    #[cfg(feature = "zero-copy")]
    let dmabuf_state = DmabufState::new(&globals, &qh);
    let mut data = ScreencopyClient {
        registry_state,
        output_state,
        toplevel_info_state,
        screencopy_state,
        shm,
        seat_state,
        cursor_pos: None,
        cursor_hotspot: None,
        #[cfg(feature = "zero-copy")]
        dmabuf_state,
        formats: None,
        result: None,
    };
    for _ in 0..3 {
        queue.roundtrip(&mut data).ok()?;
    }
    Some((conn, queue, data))
}

pub(crate) fn connect(
    wait_toplevels: bool,
) -> Option<(Connection, EventQueue<ScreencopyClient>, ScreencopyClient)> {
    let (conn, mut queue, mut data) = connect_raw()?;
    if wait_toplevels {
        let mut last = 0usize;
        let mut stable = 0;
        for _ in 0..40 {
            queue.roundtrip(&mut data).ok()?;
            let n = data.toplevel_info_state.toplevels().count();
            if n > 0 && n == last {
                stable += 1;
                if stable >= 3 {
                    break;
                }
            } else {
                stable = 0;
            }
            last = n;
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    }
    Some((conn, queue, data))
}

/// Connect and pump events only until `identifier` shows up as a toplevel. We
/// already know exactly which window we want (the picker enumerated it), so there's
/// no need to wait for the whole toplevel list to *stabilize* the way `connect(true)`
/// does — stop the instant our target is visible. Falls through after the same 40x15ms
/// budget if it never appears (caller then has an empty/partial list to fail on).
pub(crate) fn connect_for_toplevel(
    identifier: &str,
) -> Option<(Connection, EventQueue<ScreencopyClient>, ScreencopyClient)> {
    let (conn, mut queue, mut data) = connect_raw()?;
    let present = |data: &ScreencopyClient| {
        data.toplevel_info_state.toplevels().any(|t| t.identifier == identifier)
    };
    if !present(&data) {
        for _ in 0..40 {
            queue.roundtrip(&mut data).ok()?;
            if present(&data) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    }
    Some((conn, queue, data))
}

/// Pick an shm format and how to normalize it to RGBA. Returns
/// (wl_shm format, swap_r_b, force_alpha_opaque).
pub(crate) fn pick_format(formats: &[wl_shm::Format]) -> Option<(wl_shm::Format, bool, bool)> {
    use wl_shm::Format::*;
    // (candidate, swap_r_b, force_opaque)
    let prefs = [
        (Abgr8888, false, false),
        (Xbgr8888, false, true),
        (Argb8888, true, false),
        (Xrgb8888, true, true),
    ];
    for (fmt, swap, opaque) in prefs {
        if formats.contains(&fmt) {
            return Some((fmt, swap, opaque));
        }
    }
    // Last resort: assume the first advertised format is RGBA-order.
    formats.first().map(|f| (*f, false, false))
}

/// Normalize raw shm bytes (4 bytes/pixel) to RGBA8 in place.
pub(crate) fn normalize_to_rgba(bytes: &mut [u8], swap_r_b: bool, force_opaque: bool) {
    if !swap_r_b && !force_opaque {
        return;
    }
    // One combined pass (swizzle R<->B and/or force alpha). On a big frame it's the
    // screencopy path's main per-pixel cost, so split it across cores — mirroring the
    // PipeWire crop and the NV12 conversion, which are already threaded.
    if bytes.len() < 320_000 {
        for px in bytes.as_chunks_mut::<4>().0 {
            if swap_r_b {
                px.swap(0, 2);
            }
            if force_opaque {
                px[3] = 255;
            }
        }
        return;
    }
    let nthreads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let band = (bytes.len() / 4).div_ceil(nthreads).max(1) * 4; // whole pixels per band
    std::thread::scope(|s| {
        let mut rest: &mut [u8] = bytes;
        while !rest.is_empty() {
            let take = band.min(rest.len());
            let (chunk, tail) = rest.split_at_mut(take);
            rest = tail;
            s.spawn(move || {
                for px in chunk.as_chunks_mut::<4>().0 {
                    if swap_r_b {
                        px.swap(0, 2);
                    }
                    if force_opaque {
                        px[3] = 255;
                    }
                }
            });
        }
    });
}

/// Apply the compositor-reported output transform so the image is upright.
pub(crate) fn apply_transform(img: RgbaImage, transform: WEnum<Transform>) -> RgbaImage {
    use image::metadata::Orientation as O;
    let o = match transform {
        WEnum::Value(Transform::Normal) => return img,
        WEnum::Value(Transform::_90) => O::Rotate90,
        WEnum::Value(Transform::_180) => O::Rotate180,
        WEnum::Value(Transform::_270) => O::Rotate270,
        WEnum::Value(Transform::Flipped) => O::FlipHorizontal,
        WEnum::Value(Transform::Flipped90) => O::Rotate90FlipH,
        WEnum::Value(Transform::Flipped180) => O::FlipVertical,
        WEnum::Value(Transform::Flipped270) => O::Rotate270FlipH,
        _ => return img,
    };
    let mut d = image::DynamicImage::from(img);
    d.apply_orientation(o);
    d.into_rgba8()
}

/// The largest output's logical size (w, h), used to clamp the settings window so
/// it never restores larger than a monitor. Fast: enumerates outputs only (no
/// toplevel wait). Returns None if no output reports a size.
pub fn largest_output_size() -> Option<(u32, u32)> {
    let (_conn, _queue, data) = connect(false)?;
    outputs(&data)
        .into_iter()
        .filter_map(|(_, _, _, (w, h))| (w > 0 && h > 0).then_some((w as u32, h as u32)))
        .max_by_key(|&(w, h)| w as u64 * h as u64)
}

/// (WlOutput, name, logical position, logical size) for each enumerated output.
pub(crate) fn outputs(data: &ScreencopyClient) -> Vec<OutputInfo> {
    let mut v = Vec::new();
    for output in data.output_state.outputs() {
        if let Some(info) = data.output_state.info(&output)
            && let (Some(name), Some(pos), Some(size)) =
                (info.name.clone(), info.logical_position, info.logical_size)
            {
                v.push((output, name, pos, size));
            }
    }
    v
}

/// Capture one frame from a PERSISTENT `session` into its reused `buffer`, returned
/// upright (transform applied). Reusing the session across frames is what makes
/// high-fps recording possible: a fresh session per frame costs ~25ms on a 5K
/// output (the compositor re-inits the capture each time), vs ~6ms when reused.
#[allow(clippy::too_many_arguments)]
pub(crate) fn grab_frame(
    conn: &Connection,
    queue: &mut EventQueue<ScreencopyClient>,
    data: &mut ScreencopyClient,
    qh: &QueueHandle<ScreencopyClient>,
    session: &CaptureSession,
    buffer: &cosmic_client_toolkit::sctk::shm::slot::Buffer,
    pool: &mut SlotPool,
    w: u32,
    h: u32,
    swizzle: bool,
    force_opaque: bool,
) -> Option<RgbaImage> {
    data.result = None;
    session.capture(
        buffer.wl_buffer(),
        &[Rect { x: 0, y: 0, width: w as i32, height: h as i32 }],
        qh,
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
    let canvas = buffer.canvas(pool)?;
    let mut bytes = canvas.to_vec();
    normalize_to_rgba(&mut bytes, swizzle, force_opaque);
    let rgba = RgbaImage::from_raw(w, h, bytes)?;
    Some(apply_transform(rgba, frame.transform))
}

/// Capture one frame and return ONLY the cropped region's RGBA bytes, copied from
/// the mapped canvas in a single pass (vs. copying the whole frame then cropping).
/// `bx,by,bw,bh` are in upright (post-transform) pixels. Rotated outputs fall back
/// to the full transform-then-crop path.
#[allow(clippy::too_many_arguments)]
pub(crate) fn grab_cropped(
    conn: &Connection,
    queue: &mut EventQueue<ScreencopyClient>,
    data: &mut ScreencopyClient,
    qh: &QueueHandle<ScreencopyClient>,
    session: &CaptureSession,
    buffer: &cosmic_client_toolkit::sctk::shm::slot::Buffer,
    pool: &mut SlotPool,
    cw: u32,
    swizzle: bool,
    force_opaque: bool,
    bx: u32,
    by: u32,
    bw: u32,
    bh: u32,
) -> Option<(Vec<u8>, bool)> {
    data.result = None;
    session.capture(
        buffer.wl_buffer(),
        &[Rect { x: 0, y: 0, width: cw as i32, height: (data.formats.as_ref()?.buffer_size.1) as i32 }],
        qh,
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
    // Whether the compositor reported any damage for this capture. Empty damage means
    // nothing changed since the last frame, which the recorder uses to skip redundant
    // encode work (the readback already happened — the protocol has no skip-before-
    // copy mode). Empty for a region only ever means "safe to skip"; it never drops a
    // changed frame (damage covers the whole surface, so an overlap is reported).
    let had_damage = !frame.damage.is_empty();
    let canvas = buffer.canvas(pool)?;
    let stride = (cw * 4) as usize;
    // Fast path: upright output → copy just the region's rows straight from the
    // canvas, normalizing only those pixels.
    if matches!(frame.transform, WEnum::Value(Transform::Normal)) {
        let row_bytes = (bw * 4) as usize;
        let mut out = vec![0u8; row_bytes * bh as usize];
        for row in 0..bh as usize {
            let src = (by as usize + row) * stride + (bx * 4) as usize;
            let dst = row * row_bytes;
            out[dst..dst + row_bytes].copy_from_slice(canvas.get(src..src + row_bytes)?);
        }
        normalize_to_rgba(&mut out, swizzle, force_opaque);
        return Some((out, had_damage));
    }
    // Fallback (rotated/flipped output): process the whole frame, then crop.
    let h = canvas.len() / stride;
    let mut bytes = canvas.to_vec();
    normalize_to_rgba(&mut bytes, swizzle, force_opaque);
    let rgba = RgbaImage::from_raw(cw, h as u32, bytes)?;
    let upright = apply_transform(rgba, frame.transform);
    let cropped = image::imageops::crop_imm(&upright, bx, by, bw, bh).to_image().into_raw();
    Some((cropped, had_damage))
}
