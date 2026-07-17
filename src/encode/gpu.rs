//! In-process hardware (VAAPI) video encoding for the experimental "GPU zero-copy"
//! recording path (opt-in; the default recorder still pipes raw frames to the
//! `ffmpeg` binary).
//!
//! The win this enables: a captured frame can stay GPU-resident from capture
//! through encode, instead of the default round trip (compositor GPU → CPU readback
//! → pipe → ffmpeg → re-upload to GPU). We open a VAAPI encoder ourselves, hand it
//! GPU surfaces, and get compressed H.264/HEVC packets back — those tiny packets are
//! what we pipe onward, so ffmpeg only has to mux them with audio (`-c:v copy`),
//! keeping all the existing audio / A/V-sync / finalize machinery unchanged.
//!
//! When the encode size differs from the capture size (the user's max-resolution
//! cap, or H.264's 4096px limit), the downscale happens ON THE GPU via a
//! `scale_vaapi` filter — so the frame still never touches the CPU.
//!
//! Two inputs are supported:
//! - [`Encoder::encode_dmabuf`] maps a PipeWire dmabuf onto a VAAPI surface via
//!   `av_hwframe_map` (true zero-copy) and encodes it.
//! - [`Encoder::encode_nv12`] uploads a CPU NV12 frame instead (not zero-copy, but
//!   it's what the tests exercise headlessly, and a natural fallback).
//!
//! All the hwdevice / hwframes / filter plumbing has no safe wrapper in
//! `ffmpeg-next`, so we use its raw `ffi`. Every raw pointer is owned by
//! [`Encoder`] and freed in `Drop`.

// This is an FFI module: the `unsafe fn`s are wall-to-wall raw-pointer ffmpeg calls,
// so the per-op `unsafe {}` blocks edition 2024 wants would be pure noise here.
#![allow(unsafe_op_in_unsafe_fn)]

use ffmpeg_next::ffi::*;
use std::ffi::CString;
use std::ptr;

/// An in-process VAAPI H.264/HEVC encoder bound to one DRM render node, producing
/// Annex-B packets (SPS/PPS repeated in-band, so the byte stream is self-contained
/// for `ffmpeg -f h264|hevc -i -`). Scales on the GPU when the encode size differs
/// from the source size.
pub struct Encoder {
    device: *mut AVBufferRef,
    /// VAAPI pool at the SOURCE (capture) size — dmabufs map / CPU frames upload here.
    src_frames: *mut AVBufferRef,
    ctx: *mut AVCodecContext,
    pkt: *mut AVPacket,
    /// `scale_vaapi` graph (source size → encode size); null when no scaling is needed.
    graph: *mut AVFilterGraph,
    buffersrc: *mut AVFilterContext,
    buffersink: *mut AVFilterContext,
    /// Reused output frame pulled from the filter sink.
    filt_frame: *mut AVFrame,
    src_w: i32,
    src_h: i32,
    pts: i64,
}

// The encoder + its ffmpeg contexts live on one recording thread; the raw pointers
// never cross threads concurrently. Send lets the owning struct move onto that thread.
unsafe impl Send for Encoder {}

impl Encoder {
    /// Open a VAAPI encoder on `drm_node` (e.g. `/dev/dri/renderD128`) that takes
    /// `src_w`×`src_h` frames and encodes them at `dst_w`×`dst_h` (downscaling on the
    /// GPU when they differ), at `fps`, targeting `bitrate_kbps`. `hevc` picks
    /// `hevc_vaapi` over `h264_vaapi`. Dimensions are rounded to even.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        drm_node: &str,
        hevc: bool,
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
        fps: u32,
        bitrate_kbps: u32,
    ) -> Result<Self, String> {
        let even = |v: u32| (v & !1) as i32;
        let (sw, sh, dw, dh) = (even(src_w), even(src_h), even(dst_w), even(dst_h));
        if sw < 2 || sh < 2 || dw < 2 || dh < 2 {
            return Err("invalid encode size".into());
        }
        let fps = fps.max(1) as i32;
        unsafe { Self::open(drm_node, hevc, sw, sh, dw, dh, fps, bitrate_kbps.max(100)) }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn open(
        drm_node: &str,
        hevc: bool,
        sw: i32,
        sh: i32,
        dw: i32,
        dh: i32,
        fps: i32,
        bitrate_kbps: u32,
    ) -> Result<Self, String> {
        // Stash allocations as we go so any early `Err` drops what's built so far.
        let mut enc = Encoder {
            device: ptr::null_mut(),
            src_frames: ptr::null_mut(),
            ctx: ptr::null_mut(),
            pkt: ptr::null_mut(),
            graph: ptr::null_mut(),
            buffersrc: ptr::null_mut(),
            buffersink: ptr::null_mut(),
            filt_frame: ptr::null_mut(),
            src_w: sw,
            src_h: sh,
            pts: 0,
        };

        let node = CString::new(drm_node).map_err(|_| "bad drm node path")?;
        let r = av_hwdevice_ctx_create(
            &mut enc.device,
            AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
            node.as_ptr(),
            ptr::null_mut(),
            0,
        );
        if r < 0 {
            return Err(format!("open VAAPI device {drm_node}: {}", averr(r)));
        }

        // Source-size VAAPI pool: dmabufs map (or CPU frames upload) into these.
        enc.src_frames = Self::make_frames_ctx(enc.device, sw, sh)?;

        // If we need to scale, build a `scale_vaapi` graph and take the encoder's input
        // frames context from the sink; otherwise the encoder takes the source pool.
        let enc_frames = if (sw, sh) != (dw, dh) {
            enc.build_scaler(sw, sh, dw, dh, fps)?
        } else {
            enc.src_frames
        };

        let name = cstr(if hevc { "hevc_vaapi" } else { "h264_vaapi" });
        let codec = avcodec_find_encoder_by_name(name.as_ptr());
        if codec.is_null() {
            return Err("VAAPI encoder not built into ffmpeg".into());
        }
        enc.ctx = avcodec_alloc_context3(codec);
        if enc.ctx.is_null() {
            return Err("alloc encoder context".into());
        }
        (*enc.ctx).width = dw;
        (*enc.ctx).height = dh;
        (*enc.ctx).time_base = AVRational { num: 1, den: fps };
        (*enc.ctx).framerate = AVRational { num: fps, den: 1 };
        (*enc.ctx).pix_fmt = AVPixelFormat::AV_PIX_FMT_VAAPI;
        (*enc.ctx).bit_rate = (bitrate_kbps as i64) * 1000;
        (*enc.ctx).rc_max_rate = (bitrate_kbps as i64) * 1000;
        (*enc.ctx).rc_buffer_size = (bitrate_kbps as i32).saturating_mul(2000);
        // No B-frames: lower latency + measured throughput win on VAAPI (see
        // RECORDING_PERFORMANCE.md); also keeps the Annex-B stream simple to mux.
        (*enc.ctx).max_b_frames = 0;
        (*enc.ctx).gop_size = (fps * 2).max(1);
        (*enc.ctx).hw_frames_ctx = av_buffer_ref(enc_frames);

        let r = avcodec_open2(enc.ctx, codec, ptr::null_mut());
        if r < 0 {
            return Err(format!("open VAAPI encoder: {}", averr(r)));
        }
        enc.pkt = av_packet_alloc();
        enc.filt_frame = av_frame_alloc();
        if enc.pkt.is_null() || enc.filt_frame.is_null() {
            return Err("alloc packet/frame".into());
        }
        Ok(enc)
    }

    /// Allocate + init a VAAPI surface pool (NV12 sw_format) at `w`×`h`.
    unsafe fn make_frames_ctx(device: *mut AVBufferRef, w: i32, h: i32) -> Result<*mut AVBufferRef, String> {
        let frames = av_hwframe_ctx_alloc(device);
        if frames.is_null() {
            return Err("alloc hwframe context".into());
        }
        let fctx = (*frames).data as *mut AVHWFramesContext;
        (*fctx).format = AVPixelFormat::AV_PIX_FMT_VAAPI;
        (*fctx).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
        (*fctx).width = w;
        (*fctx).height = h;
        (*fctx).initial_pool_size = 8;
        let r = av_hwframe_ctx_init(frames);
        if r < 0 {
            let mut f = frames;
            av_buffer_unref(&mut f);
            return Err(format!("init hwframe context: {}", averr(r)));
        }
        Ok(frames)
    }

    /// Build a `buffer → scale_vaapi → buffersink` graph from the source pool to
    /// `dw`×`dh`, returning the sink's output (VAAPI) frames context for the encoder.
    unsafe fn build_scaler(&mut self, sw: i32, sh: i32, dw: i32, dh: i32, fps: i32) -> Result<*mut AVBufferRef, String> {
        self.graph = avfilter_graph_alloc();
        if self.graph.is_null() {
            return Err("alloc filter graph".into());
        }
        // Buffer source: a VAAPI input carrying our source-size hw frames context.
        let bs = cstr("buffer");
        let in_name = cstr("in");
        self.buffersrc = avfilter_graph_alloc_filter(self.graph, avfilter_get_by_name(bs.as_ptr()), in_name.as_ptr());
        if self.buffersrc.is_null() {
            return Err("alloc buffersrc".into());
        }
        let par = av_buffersrc_parameters_alloc();
        if par.is_null() {
            return Err("alloc buffersrc params".into());
        }
        (*par).format = AVPixelFormat::AV_PIX_FMT_VAAPI as i32;
        (*par).width = sw;
        (*par).height = sh;
        (*par).time_base = AVRational { num: 1, den: fps };
        (*par).hw_frames_ctx = av_buffer_ref(self.src_frames);
        let r = av_buffersrc_parameters_set(self.buffersrc, par);
        av_free(par as *mut _);
        if r < 0 {
            return Err(format!("set buffersrc params: {}", averr(r)));
        }
        if avfilter_init_str(self.buffersrc, ptr::null()) < 0 {
            return Err("init buffersrc".into());
        }
        // scale_vaapi to the target size.
        let sv = cstr("scale_vaapi");
        let sc_name = cstr("scale");
        let scale = avfilter_graph_alloc_filter(self.graph, avfilter_get_by_name(sv.as_ptr()), sc_name.as_ptr());
        if scale.is_null() {
            return Err("scale_vaapi filter unavailable".into());
        }
        let scale_args = cstr(&format!("w={dw}:h={dh}"));
        if avfilter_init_str(scale, scale_args.as_ptr()) < 0 {
            return Err("init scale_vaapi".into());
        }
        // Buffer sink.
        let bk = cstr("buffersink");
        let out_name = cstr("out");
        self.buffersink = avfilter_graph_alloc_filter(self.graph, avfilter_get_by_name(bk.as_ptr()), out_name.as_ptr());
        if self.buffersink.is_null() || avfilter_init_str(self.buffersink, ptr::null()) < 0 {
            return Err("init buffersink".into());
        }
        if avfilter_link(self.buffersrc, 0, scale, 0) < 0 || avfilter_link(scale, 0, self.buffersink, 0) < 0 {
            return Err("link filters".into());
        }
        let r = avfilter_graph_config(self.graph, ptr::null_mut());
        if r < 0 {
            return Err(format!("configure filter graph: {}", averr(r)));
        }
        // The sink now carries the scaled VAAPI frames context (AVFilterLink is opaque
        // in ffmpeg 8, so use the accessor).
        let frames = av_buffersink_get_hw_frames_ctx(self.buffersink);
        if frames.is_null() {
            return Err("scaler produced no hw frames context".into());
        }
        Ok(frames)
    }

    /// Encode one CPU NV12 frame at the SOURCE size (`src_w*src_h` Y + interleaved
    /// UV). Uploads to a GPU surface, scales (if needed) + encodes, returning any
    /// Annex-B bytes ready. The headless-testable path; the recorder uses
    /// [`Self::encode_dmabuf`].
    #[allow(dead_code)]
    pub fn encode_nv12(&mut self, nv12: &[u8]) -> Result<Vec<u8>, String> {
        let need = (self.src_w * self.src_h * 3 / 2) as usize;
        if nv12.len() < need {
            return Err("nv12 frame too small".into());
        }
        unsafe {
            let mut sw = av_frame_alloc();
            (*sw).format = AVPixelFormat::AV_PIX_FMT_NV12 as i32;
            (*sw).width = self.src_w;
            (*sw).height = self.src_h;
            if av_frame_get_buffer(sw, 0) < 0 {
                av_frame_free(&mut sw);
                return Err("alloc cpu frame".into());
            }
            let yw = self.src_w as usize;
            let ys = (*sw).linesize[0] as usize;
            for row in 0..self.src_h as usize {
                ptr::copy_nonoverlapping(nv12.as_ptr().add(row * yw), (*sw).data[0].add(row * ys), yw);
            }
            let uv_off = (self.src_w * self.src_h) as usize;
            let cs = (*sw).linesize[1] as usize;
            for row in 0..(self.src_h as usize / 2) {
                ptr::copy_nonoverlapping(nv12.as_ptr().add(uv_off + row * yw), (*sw).data[1].add(row * cs), yw);
            }

            let mut hw = match self.src_surface() {
                Ok(h) => h,
                Err(e) => {
                    av_frame_free(&mut sw);
                    return Err(e);
                }
            };
            let r = av_hwframe_transfer_data(hw, sw, 0);
            av_frame_free(&mut sw);
            if r < 0 {
                av_frame_free(&mut hw);
                return Err(format!("upload to gpu: {}", averr(r)));
            }
            let res = self.submit(hw);
            av_frame_free(&mut hw);
            res
        }
    }

    /// Encode a frame held in a DMA-BUF (zero-copy): wrap the dmabuf in a DRM-PRIME
    /// frame, map it onto a VAAPI surface (no CPU touch), scale (if needed) + encode.
    /// `fourcc`/`modifier` are the DRM format + modifier; `planes` is per-plane
    /// `(fd, offset, pitch)`. The fds are borrowed for the call only.
    ///
    /// The import depends on a live portal dmabuf — exercise on real hardware. On any
    /// failure the caller should fall back to the CPU path.
    pub fn encode_dmabuf(
        &mut self,
        fourcc: u32,
        modifier: u64,
        planes: &[(i32, u32, u32)],
    ) -> Result<Vec<u8>, String> {
        if planes.is_empty() || planes.len() > AV_DRM_MAX_PLANES as usize {
            return Err("bad dmabuf plane count".into());
        }
        unsafe {
            let mut desc: AVDRMFrameDescriptor = std::mem::zeroed();
            desc.nb_objects = 1;
            desc.objects[0].fd = planes[0].0;
            desc.objects[0].size = 0; // unknown; the driver maps by fd
            desc.objects[0].format_modifier = modifier;
            desc.nb_layers = 1;
            desc.layers[0].format = fourcc;
            desc.layers[0].nb_planes = planes.len() as i32;
            for (i, (_fd, offset, pitch)) in planes.iter().enumerate() {
                desc.layers[0].planes[i].object_index = 0;
                desc.layers[0].planes[i].offset = *offset as isize;
                desc.layers[0].planes[i].pitch = *pitch as isize;
            }

            let drm = av_frame_alloc();
            (*drm).format = AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
            (*drm).width = self.src_w;
            (*drm).height = self.src_h;
            (*drm).data[0] = &mut desc as *mut _ as *mut u8;

            let mut hw = match self.src_surface() {
                Ok(h) => h,
                Err(e) => {
                    let mut d = drm;
                    av_frame_free(&mut d);
                    return Err(e);
                }
            };
            let r = av_hwframe_map(hw, drm, AV_HWFRAME_MAP_READ as i32 | AV_HWFRAME_MAP_DIRECT as i32);
            let mut d = drm;
            av_frame_free(&mut d);
            if r < 0 {
                av_frame_free(&mut hw);
                return Err(format!("map dmabuf to VAAPI: {}", averr(r)));
            }
            let res = self.submit(hw);
            av_frame_free(&mut hw);
            res
        }
    }

    /// Get a fresh surface from the source pool.
    unsafe fn src_surface(&mut self) -> Result<*mut AVFrame, String> {
        let mut hw = av_frame_alloc();
        let r = av_hwframe_get_buffer(self.src_frames, hw, 0);
        if r < 0 {
            av_frame_free(&mut hw);
            return Err(format!("get gpu surface: {}", averr(r)));
        }
        Ok(hw)
    }

    /// Stamp the PTS and run a source-size surface through the scaler (if any) into
    /// the encoder, collecting packet bytes.
    unsafe fn submit(&mut self, surface: *mut AVFrame) -> Result<Vec<u8>, String> {
        (*surface).pts = self.pts;
        self.pts += 1;
        if self.buffersrc.is_null() {
            return self.send_and_drain(surface);
        }
        let r = av_buffersrc_add_frame_flags(self.buffersrc, surface, AV_BUFFERSRC_FLAG_KEEP_REF as i32);
        if r < 0 {
            return Err(format!("buffersrc add: {}", averr(r)));
        }
        let mut out = Vec::new();
        loop {
            let r = av_buffersink_get_frame(self.buffersink, self.filt_frame);
            if r == AVERROR(EAGAIN) || r == AVERROR_EOF {
                break;
            }
            if r < 0 {
                return Err(format!("buffersink get: {}", averr(r)));
            }
            let bytes = self.send_and_drain(self.filt_frame);
            av_frame_unref(self.filt_frame);
            out.extend_from_slice(&bytes?);
        }
        Ok(out)
    }

    /// Submit a GPU frame (or null to flush) and collect whatever packets come out.
    unsafe fn send_and_drain(&mut self, frame: *mut AVFrame) -> Result<Vec<u8>, String> {
        let r = avcodec_send_frame(self.ctx, frame);
        if r < 0 && r != AVERROR(EAGAIN) {
            return Err(format!("send frame: {}", averr(r)));
        }
        let mut out = Vec::new();
        loop {
            let r = avcodec_receive_packet(self.ctx, self.pkt);
            if r == AVERROR(EAGAIN) || r == AVERROR_EOF {
                break;
            }
            if r < 0 {
                return Err(format!("receive packet: {}", averr(r)));
            }
            let data = (*self.pkt).data;
            let size = (*self.pkt).size as usize;
            if !data.is_null() && size > 0 {
                out.extend_from_slice(std::slice::from_raw_parts(data, size));
            }
            av_packet_unref(self.pkt);
        }
        Ok(out)
    }

    /// Flush the scaler then the encoder at end of stream, returning trailing bytes.
    pub fn finish(&mut self) -> Result<Vec<u8>, String> {
        unsafe {
            let mut out = Vec::new();
            if !self.buffersrc.is_null() {
                av_buffersrc_add_frame_flags(self.buffersrc, ptr::null_mut(), 0); // signal EOF
                loop {
                    let r = av_buffersink_get_frame(self.buffersink, self.filt_frame);
                    if r == AVERROR(EAGAIN) || r == AVERROR_EOF {
                        break;
                    }
                    if r < 0 {
                        break;
                    }
                    if let Ok(b) = self.send_and_drain(self.filt_frame) {
                        out.extend_from_slice(&b);
                    }
                    av_frame_unref(self.filt_frame);
                }
            }
            let b = self.send_and_drain(ptr::null_mut())?;
            out.extend_from_slice(&b);
            Ok(out)
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            if !self.filt_frame.is_null() {
                av_frame_free(&mut self.filt_frame);
            }
            if !self.pkt.is_null() {
                av_packet_free(&mut self.pkt);
            }
            if !self.ctx.is_null() {
                avcodec_free_context(&mut self.ctx);
            }
            // The graph owns buffersrc/buffersink; freeing it covers them.
            if !self.graph.is_null() {
                avfilter_graph_free(&mut self.graph);
            }
            if !self.src_frames.is_null() {
                av_buffer_unref(&mut self.src_frames);
            }
            if !self.device.is_null() {
                av_buffer_unref(&mut self.device);
            }
        }
    }
}

/// Build a `CString` from a `&str` we KNOW has no interior NUL byte — a filter/pad
/// name literal, or `w=..:h=..` built from validated (even, non-negative) dimensions
/// via `format!`. Centralizes that invariant instead of repeating the same
/// `.expect(...)` justification at every call site.
fn cstr(s: &str) -> CString {
    CString::new(s).expect("literal/int-formatted string has no interior NUL byte")
}

/// Render an ffmpeg error code as a readable string.
fn averr(code: i32) -> String {
    // c_char, not i8: char is unsigned on aarch64-linux, so a hard-coded i8
    // buffer only compiles on x86_64.
    let mut buf = [0 as std::ffi::c_char; 256];
    unsafe {
        if av_strerror(code, buf.as_mut_ptr(), buf.len()) == 0 {
            std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
        } else {
            format!("ffmpeg error {code}")
        }
    }
}

/// The first VAAPI-capable render node (amdgpu/Intel), so tests + callers without a
/// specific device can find one. `None` if there's no usable node.
pub fn default_vaapi_node() -> Option<String> {
    let mut nodes: Vec<String> = std::fs::read_dir("/dev/dri")
        .ok()?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("renderD").then(|| format!("/dev/dri/{n}"))
        })
        .collect();
    nodes.sort();
    nodes.into_iter().find(|dev| {
        let node = dev.rsplit('/').next().unwrap_or("");
        std::fs::read_link(format!("/sys/class/drm/{node}/device/driver"))
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
            .map(|drv| matches!(drv.as_str(), "amdgpu" | "i915" | "xe"))
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vaapi_node() -> Option<String> {
        ffmpeg_next::init().ok();
        let node = default_vaapi_node()?;
        // Driver name matters when an nvidia node is also present.
        // SAFETY: test-only, before any encoder thread starts.
        unsafe { std::env::set_var("LIBVA_DRIVER_NAME", "radeonsi") };
        Some(node)
    }

    fn run(src: (u32, u32), dst: (u32, u32)) -> Option<usize> {
        let node = vaapi_node()?;
        let mut enc = match Encoder::new(&node, false, src.0, src.1, dst.0, dst.1, 30, 6000) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("VAAPI encoder unavailable ({e}); skipping");
                return None;
            }
        };
        let mut nv12 = vec![16u8; (src.0 * src.1 * 3 / 2) as usize];
        let mut total = 0usize;
        for i in 0..30 {
            for (j, p) in nv12.iter_mut().take((src.0 * src.1) as usize).enumerate() {
                *p = ((j + i * 9) & 0xff) as u8;
            }
            total += enc.encode_nv12(&nv12).expect("encode").len();
        }
        total += enc.finish().expect("flush").len();
        Some(total)
    }

    /// In-process VAAPI encode of synthetic NV12 frames must produce real packets.
    #[test]
    fn vaapi_encodes_synthetic_frames() {
        let Some(total) = run((1280, 720), (1280, 720)) else { return };
        assert!(total > 0, "encoder produced no packets");
    }

    /// The GPU `scale_vaapi` downscale path (source ≠ encode size) must also produce
    /// packets — this is what honours the max-resolution cap / H.264 4096 limit.
    #[test]
    fn vaapi_scales_then_encodes() {
        let Some(total) = run((1920, 1080), (1280, 720)) else { return };
        assert!(total > 0, "scaler+encoder produced no packets");
    }
}
