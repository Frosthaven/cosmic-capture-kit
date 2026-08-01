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
//!
//! **libav is loaded LAZILY** (DRAGON-336). We take the bindgen *types* from
//! `ffmpeg_next::ffi` but call none of its `extern "C"` declarations directly —
//! every entry point goes through the [`Libav`] vtable below, `dlopen`ed on first
//! use. With zero direct symbol references the linker's `--as-needed` drops
//! `libavutil` / `libavcodec` / `libavfilter` from the binary's `DT_NEEDED`, so the
//! ~120-shared-object libav closure is no longer mapped before `main()` — the tray
//! daemon and every screenshot-only launch stop paying for an encoder they never
//! open. Only starting a GPU zero-copy recording pulls libav in.

// This is an FFI module: the `unsafe fn`s are wall-to-wall raw-pointer ffmpeg calls,
// so the per-op `unsafe {}` blocks edition 2024 wants would be pure noise here.
#![allow(unsafe_op_in_unsafe_fn)]

use ffmpeg_next::ffi::*;
use std::ffi::CString;
use std::ptr;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Lazy libav linkage (DRAGON-336)
// ---------------------------------------------------------------------------

/// The three `dlopen` handles a resolve pass reads symbols out of. Deliberately
/// never closed: once resolved the vtable holds pointers into these mappings for
/// the rest of the process.
struct Handles {
    avutil: *mut libc::c_void,
    avcodec: *mut libc::c_void,
    avfilter: *mut libc::c_void,
}

/// Declare every libav entry point we use, once: the [`Libav`] vtable field, its
/// `dlsym` resolve step, and a module-level shim of the SAME NAME as the C function.
///
/// The shim shadows the glob-imported `ffmpeg_next::ffi` declaration (explicit items
/// beat glob imports), so every call site in this file reads exactly like a direct
/// FFI call while actually dispatching through the vtable. When the library isn't
/// loadable a shim returns its "miss" value instead of panicking — null for pointer
/// returns, a negative code for `int` returns — which the existing null / `r < 0`
/// checks already treat as a failed encoder, so a missing libav degrades into the
/// caller's established CPU fallback.
macro_rules! libav_syms {
    ($( $lib:ident fn $name:ident ( $($an:ident : $at:ty),* ) -> $ret:ty = $miss:expr; )+) => {
        /// Resolved libav entry points. Built once by [`load_libav`].
        struct Libav {
            $( $name: unsafe extern "C" fn($($at),*) -> $ret, )+
        }

        impl Libav {
            /// `None` if any single symbol is missing — a partial vtable is never
            /// published, so a shim can only ever see "all there" or "none there".
            unsafe fn resolve(h: &Handles) -> Option<Self> {
                Some(Libav {
                    $( $name: std::mem::transmute::<
                        *mut libc::c_void,
                        unsafe extern "C" fn($($at),*) -> $ret,
                    >(dlsym(h.$lib, concat!(stringify!($name), "\0"))?), )+
                })
            }
        }

        $(
            #[inline]
            unsafe fn $name($($an: $at),*) -> $ret {
                match libav() {
                    Some(l) => (l.$name)($($an),*),
                    None => $miss,
                }
            }
        )+
    };
}

libav_syms! {
    // libavutil
    avutil fn av_buffer_ref(buf: *const AVBufferRef) -> *mut AVBufferRef = ptr::null_mut();
    avutil fn av_buffer_unref(buf: *mut *mut AVBufferRef) -> () = ();
    avutil fn av_frame_alloc() -> *mut AVFrame = ptr::null_mut();
    avutil fn av_frame_free(frame: *mut *mut AVFrame) -> () = ();
    avutil fn av_frame_unref(frame: *mut AVFrame) -> () = ();
    avutil fn av_frame_get_buffer(frame: *mut AVFrame, align: libc::c_int) -> libc::c_int = MISSING;
    avutil fn av_free(ptr: *mut libc::c_void) -> () = ();
    avutil fn av_strerror(errnum: libc::c_int, errbuf: *mut libc::c_char, errbuf_size: usize) -> libc::c_int = MISSING;
    avutil fn av_hwdevice_ctx_create(device_ctx: *mut *mut AVBufferRef, type_: AVHWDeviceType, device: *const libc::c_char, opts: *mut AVDictionary, flags: libc::c_int) -> libc::c_int = MISSING;
    avutil fn av_hwframe_ctx_alloc(device_ctx: *mut AVBufferRef) -> *mut AVBufferRef = ptr::null_mut();
    avutil fn av_hwframe_ctx_init(ref_: *mut AVBufferRef) -> libc::c_int = MISSING;
    avutil fn av_hwframe_get_buffer(hwframe_ctx: *mut AVBufferRef, frame: *mut AVFrame, flags: libc::c_int) -> libc::c_int = MISSING;
    avutil fn av_hwframe_map(dst: *mut AVFrame, src: *const AVFrame, flags: libc::c_int) -> libc::c_int = MISSING;
    avutil fn av_hwframe_transfer_data(dst: *mut AVFrame, src: *const AVFrame, flags: libc::c_int) -> libc::c_int = MISSING;
    // libavcodec
    avcodec fn avcodec_alloc_context3(codec: *const AVCodec) -> *mut AVCodecContext = ptr::null_mut();
    avcodec fn avcodec_find_encoder_by_name(name: *const libc::c_char) -> *const AVCodec = ptr::null();
    avcodec fn avcodec_free_context(avctx: *mut *mut AVCodecContext) -> () = ();
    avcodec fn avcodec_open2(avctx: *mut AVCodecContext, codec: *const AVCodec, options: *mut *mut AVDictionary) -> libc::c_int = MISSING;
    avcodec fn avcodec_receive_packet(avctx: *mut AVCodecContext, avpkt: *mut AVPacket) -> libc::c_int = MISSING;
    avcodec fn avcodec_send_frame(avctx: *mut AVCodecContext, frame: *const AVFrame) -> libc::c_int = MISSING;
    avcodec fn av_packet_alloc() -> *mut AVPacket = ptr::null_mut();
    avcodec fn av_packet_free(pkt: *mut *mut AVPacket) -> () = ();
    avcodec fn av_packet_unref(pkt: *mut AVPacket) -> () = ();
    // libavfilter
    avfilter fn av_buffersink_get_frame(ctx: *mut AVFilterContext, frame: *mut AVFrame) -> libc::c_int = MISSING;
    avfilter fn av_buffersink_get_hw_frames_ctx(ctx: *const AVFilterContext) -> *mut AVBufferRef = ptr::null_mut();
    avfilter fn av_buffersrc_add_frame_flags(buffer_src: *mut AVFilterContext, frame: *mut AVFrame, flags: libc::c_int) -> libc::c_int = MISSING;
    avfilter fn av_buffersrc_parameters_alloc() -> *mut AVBufferSrcParameters = ptr::null_mut();
    avfilter fn av_buffersrc_parameters_set(ctx: *mut AVFilterContext, param: *mut AVBufferSrcParameters) -> libc::c_int = MISSING;
    avfilter fn avfilter_get_by_name(name: *const libc::c_char) -> *const AVFilter = ptr::null();
    avfilter fn avfilter_graph_alloc() -> *mut AVFilterGraph = ptr::null_mut();
    avfilter fn avfilter_graph_alloc_filter(graph: *mut AVFilterGraph, filter: *const AVFilter, name: *const libc::c_char) -> *mut AVFilterContext = ptr::null_mut();
    avfilter fn avfilter_graph_config(graphctx: *mut AVFilterGraph, log_ctx: *mut libc::c_void) -> libc::c_int = MISSING;
    avfilter fn avfilter_graph_free(graph: *mut *mut AVFilterGraph) -> () = ();
    avfilter fn avfilter_init_str(ctx: *mut AVFilterContext, args: *const libc::c_char) -> libc::c_int = MISSING;
    avfilter fn avfilter_link(src: *mut AVFilterContext, srcpad: libc::c_uint, dst: *mut AVFilterContext, dstpad: libc::c_uint) -> libc::c_int = MISSING;
}

/// What an `int`-returning shim reports when libav isn't loaded: `AVERROR(ENOSYS)`,
/// so it renders like any other ffmpeg failure and trips every `r < 0` check.
const MISSING: libc::c_int = -libc::ENOSYS;

/// Resolve one symbol, or `None` (the whole vtable then fails to build).
unsafe fn dlsym(handle: *mut libc::c_void, name: &str) -> Option<*mut libc::c_void> {
    // `name` is a `concat!(.., "\0")` literal, so it is already NUL-terminated.
    let p = libc::dlsym(handle, name.as_ptr().cast::<libc::c_char>());
    if p.is_null() {
        log::warn!("libav: symbol {} missing; GPU encoding unavailable", name.trim_end_matches('\0'));
        return None;
    }
    Some(p)
}

/// `dlopen` one libav library, pinned to the ABI major we were COMPILED against.
///
/// This matters: the struct layouts, field offsets and constants we use come from
/// that major's headers via bindgen, so loading a different one would silently
/// misread every field. We try the exact soname first, then the unversioned dev
/// symlink, and in both cases confirm the major through the library's own
/// `<name>_version()` before accepting it.
unsafe fn dlopen_libav(name: &str, major: u32) -> Option<*mut libc::c_void> {
    let ver_sym = CString::new(format!("{name}_version")).ok()?;
    for soname in [format!("lib{name}.so.{major}"), format!("lib{name}.so")] {
        let Ok(c_soname) = CString::new(soname.as_str()) else { continue };
        let h = libc::dlopen(c_soname.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
        if h.is_null() {
            continue;
        }
        let v = libc::dlsym(h, ver_sym.as_ptr());
        let ok = !v.is_null() && {
            let f = std::mem::transmute::<*mut libc::c_void, unsafe extern "C" fn() -> libc::c_uint>(v);
            (f() >> 16) == major
        };
        if ok {
            return Some(h);
        }
        libc::dlclose(h);
    }
    log::warn!("libav: no lib{name}.so.{major} on this system; GPU encoding unavailable");
    None
}

/// Open all three libraries and resolve the vtable. `None` on any miss.
unsafe fn load_libav() -> Option<Libav> {
    let h = Handles {
        avutil: dlopen_libav("avutil", LIBAVUTIL_VERSION_MAJOR as u32)?,
        avcodec: dlopen_libav("avcodec", LIBAVCODEC_VERSION_MAJOR as u32)?,
        avfilter: dlopen_libav("avfilter", LIBAVFILTER_VERSION_MAJOR as u32)?,
    };
    Libav::resolve(&h)
}

/// The process-wide vtable, resolved on first use. `None` means "libav isn't
/// usable here" — logged once (the `OnceLock` caches the verdict, misses included).
fn libav() -> Option<&'static Libav> {
    static LIBAV: OnceLock<Option<Libav>> = OnceLock::new();
    // SAFETY: dlopen/dlsym of well-known sonames; the ABI major is verified before
    // any symbol is used, and `OnceLock` makes the load happen exactly once.
    LIBAV.get_or_init(|| unsafe { load_libav() }).as_ref()
}

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
        // The one door into this module's ffmpeg calls: pull libav in here (the whole
        // point of the lazy linkage) and name the failure plainly. The per-shim miss
        // values below are belt-and-braces for the same condition.
        if libav().is_none() {
            return Err("libavutil/libavcodec/libavfilter not available".into());
        }
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

// ---------------------------------------------------------------------------
// NVENC, fed from CUDA memory (DRAGON-457)
// ---------------------------------------------------------------------------

/// `av_hwdevice_ctx_create` flag (hwcontext_cuda.h): ADOPT the CUDA context that is
/// already current on this thread, instead of creating a private one.
///
/// Load-bearing, not a tuning knob. [`crate::encode::cuda`] imports the compositor's
/// buffer into its own context, and a device pointer means nothing in a different
/// context — without this, ffmpeg opens a second one and NVENC reads garbage or fails.
///
/// The sibling flag `AV_CUDA_USE_PRIMARY_CONTEXT` (1) looks like it would do as well
/// and does not: on this stack `av_hwdevice_ctx_create` answers `EOPNOTSUPP` (-95) for
/// it, while unflagged creation succeeds. Adopting the current context is both the
/// precise thing to ask for and the one that works.
#[cfg(target_os = "linux")]
const AV_CUDA_USE_CURRENT_CONTEXT: libc::c_int = 2;

/// An in-process NVENC encoder that reads frames already sitting in CUDA memory.
///
/// This is the NVIDIA counterpart to [`Encoder`]: same job, different reason for
/// existing. VAAPI cannot encode on an NVIDIA-rendered session at all (no VAAPI encode
/// node exists), so without this the whole recording falls back to a CPU readback of
/// every frame — on a 5120x1440 display, 29.5 MB across PCIe per frame, to encode on
/// the very GPU the frame started on.
///
/// **No scaling.** NVENC does not resize, and `scale_cuda` cannot handle RGB in an
/// `--enable-cuda-llvm` build (its PTX has no RGB kernels). So a session that needs a
/// different encode size is declined here rather than silently ignored, and the CPU
/// path takes it — see [`NvencEncoder::new`].
///
/// **H.264 stops at 4096 pixels wide.** On an ultrawide desktop (5120x1440 is the box
/// this was written on) `h264_nvenc` refuses to open with "Width 5120 exceeds 4096",
/// which libav reports as a bare `ENOSYS`. Since this path cannot downscale,
/// [`NvencEncoder::new`] DECLINES rather than switching the codec: which codec to use
/// is the user's setting, `plan.rs` owns that policy, and quietly handing someone who
/// chose H.264 an HEVC file is not this module's call to make. The CPU path can
/// downscale, so declining keeps their choice.
///
/// **No colour conversion either**, which is the pleasant surprise: `h264_nvenc`
/// accepts a CUDA frames context whose `sw_format` is `bgr0` and does RGB→YUV itself.
#[cfg(target_os = "linux")]
pub struct NvencEncoder {
    device: *mut AVBufferRef,
    frames: *mut AVBufferRef,
    ctx: *mut AVCodecContext,
    pkt: *mut AVPacket,
    pts: i64,
}

// Same discipline as `Encoder`: one recording thread owns it.
#[cfg(target_os = "linux")]
unsafe impl Send for NvencEncoder {}

#[cfg(target_os = "linux")]
impl NvencEncoder {
    /// Open an NVENC encoder for `w`×`h` BGR0 frames living in CUDA memory.
    ///
    /// `dst_w`/`dst_h` must equal `w`/`h`; a mismatch is an error rather than a silent
    /// full-size encode, because the caller asked for a smaller file and would
    /// otherwise not get one.
    pub fn new(
        hevc: bool,
        w: u32,
        h: u32,
        dst_w: u32,
        dst_h: u32,
        fps: u32,
        bitrate_kbps: u32,
    ) -> Result<Self, String> {
        if libav().is_none() {
            return Err("libavutil/libavcodec not available".into());
        }
        // The ENCODE size is what matters here: the GL blit inside `encode::cuda`
        // scales the captured frame into it, so the user's max-resolution cap is
        // honoured on the GPU and `w`/`h` (the capture size) only bound the source.
        let even = |v: u32| (v & !1) as i32;
        let _ = (even(w), even(h));
        let (w, h) = (even(dst_w), even(dst_h));
        if w < 2 || h < 2 {
            return Err("invalid encode size".into());
        }
        // H.264 tops out at 4096 wide. DECLINE rather than quietly switch to HEVC: the
        // codec is the user's setting, and `plan.rs::software_plan` already draws that
        // line ("h264" => false, only `auto` switches above 4096). Overriding here
        // would hand somebody who chose H.264 an HEVC file with nothing said. The CPU
        // path takes it instead, and their choice survives.
        if !hevc && w > 4096 {
            return Err(format!(
                "H.264 cannot encode {w} pixels wide (its limit is 4096); \
                 the CPU path handles it"
            ));
        }
        unsafe { Self::open(hevc, w, h, fps.max(1) as i32, bitrate_kbps.max(100)) }
    }

    unsafe fn open(
        hevc: bool,
        w: i32,
        h: i32,
        fps: i32,
        bitrate_kbps: u32,
    ) -> Result<Self, String> {
        let mut enc = NvencEncoder {
            device: ptr::null_mut(),
            frames: ptr::null_mut(),
            ctx: ptr::null_mut(),
            pkt: ptr::null_mut(),
            pts: 0,
        };

        // The import's context must be current BEFORE this, so ffmpeg adopts it — see
        // `AV_CUDA_USE_CURRENT_CONTEXT`. The guard pops it when `open` returns.
        let cuda = crate::encode::cuda::CudaDevice::get()
            .ok_or("no CUDA/EGL import on this machine")?;
        let _ctx_guard = cuda.make_current()?;

        let dev = cstr("0");
        let r = av_hwdevice_ctx_create(
            &mut enc.device,
            AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
            dev.as_ptr(),
            ptr::null_mut(),
            AV_CUDA_USE_CURRENT_CONTEXT,
        );
        if r < 0 {
            return Err(format!("open CUDA device: {} (code {r})", averr(r)));
        }

        enc.frames = av_hwframe_ctx_alloc(enc.device);
        if enc.frames.is_null() {
            return Err("alloc CUDA hwframe context".into());
        }
        let fctx = (*enc.frames).data as *mut AVHWFramesContext;
        (*fctx).format = AVPixelFormat::AV_PIX_FMT_CUDA;
        // The compositor hands us XR24, which is BGR0. NVENC takes it directly.
        (*fctx).sw_format = AVPixelFormat::AV_PIX_FMT_BGR0;
        (*fctx).width = w;
        (*fctx).height = h;
        (*fctx).initial_pool_size = 4;
        let r = av_hwframe_ctx_init(enc.frames);
        if r < 0 {
            return Err(format!("init CUDA hwframe context: {}", averr(r)));
        }

        let name = cstr(if hevc { "hevc_nvenc" } else { "h264_nvenc" });
        let codec = avcodec_find_encoder_by_name(name.as_ptr());
        if codec.is_null() {
            return Err("NVENC encoder not built into ffmpeg".into());
        }
        enc.ctx = avcodec_alloc_context3(codec);
        if enc.ctx.is_null() {
            return Err("alloc encoder context".into());
        }
        (*enc.ctx).width = w;
        (*enc.ctx).height = h;
        (*enc.ctx).time_base = AVRational { num: 1, den: fps };
        (*enc.ctx).framerate = AVRational { num: fps, den: 1 };
        (*enc.ctx).pix_fmt = AVPixelFormat::AV_PIX_FMT_CUDA;
        (*enc.ctx).bit_rate = (bitrate_kbps as i64) * 1000;
        (*enc.ctx).rc_max_rate = (bitrate_kbps as i64) * 1000;
        (*enc.ctx).rc_buffer_size = (bitrate_kbps as i32).saturating_mul(2000);
        // Matches the VAAPI encoder: no B-frames, 2s GOP.
        (*enc.ctx).max_b_frames = 0;
        (*enc.ctx).gop_size = (fps * 2).max(1);
        (*enc.ctx).hw_frames_ctx = av_buffer_ref(enc.frames);

        let r = avcodec_open2(enc.ctx, codec, ptr::null_mut());
        if r < 0 {
            return Err(format!("open NVENC encoder: {}", averr(r)));
        }
        enc.pkt = av_packet_alloc();
        if enc.pkt.is_null() {
            return Err("alloc packet".into());
        }
        Ok(enc)
    }

    /// Encode one frame straight out of an imported compositor buffer.
    ///
    /// Takes a frame from ffmpeg's own CUDA pool and has the import write INTO it, so
    /// the pixels make exactly one device-to-device hop and never enter system memory.
    pub fn encode_import(
        &mut self,
        import: &crate::encode::cuda::CudaImport,
    ) -> Result<Vec<u8>, String> {
        unsafe {
            let mut frame = av_frame_alloc();
            if frame.is_null() {
                return Err("alloc cuda frame".into());
            }
            let r = av_hwframe_get_buffer(self.frames, frame, 0);
            if r < 0 {
                av_frame_free(&mut frame);
                return Err(format!("get cuda frame: {}", averr(r)));
            }
            // For AV_PIX_FMT_CUDA, `data[0]` IS the device pointer and `linesize[0]`
            // its pitch — which is exactly what the import needs to write to.
            let dst = (*frame).data[0] as usize;
            let pitch = (*frame).linesize[0] as usize;
            if let Err(e) = import.refresh_into(dst, pitch) {
                av_frame_free(&mut frame);
                return Err(e);
            }
            let res = self.submit(frame);
            av_frame_free(&mut frame);
            res
        }
    }

    /// Send a frame and drain whatever packets the encoder is ready to hand back.
    unsafe fn submit(&mut self, frame: *mut AVFrame) -> Result<Vec<u8>, String> {
        (*frame).pts = self.pts;
        self.pts += 1;
        let r = avcodec_send_frame(self.ctx, frame);
        if r < 0 {
            return Err(format!("send frame to NVENC: {}", averr(r)));
        }
        self.drain()
    }

    /// Flush the encoder at end of session and return its remaining packets.
    pub fn flush(&mut self) -> Result<Vec<u8>, String> {
        unsafe {
            let r = avcodec_send_frame(self.ctx, ptr::null());
            if r < 0 {
                return Err(format!("flush NVENC: {}", averr(r)));
            }
            self.drain()
        }
    }

    unsafe fn drain(&mut self) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        loop {
            let r = avcodec_receive_packet(self.ctx, self.pkt);
            // EAGAIN / EOF are "nothing more right now", not failures.
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
}

#[cfg(target_os = "linux")]
impl Drop for NvencEncoder {
    fn drop(&mut self) {
        unsafe {
            if !self.pkt.is_null() {
                av_packet_free(&mut self.pkt);
            }
            if !self.ctx.is_null() {
                avcodec_free_context(&mut self.ctx);
            }
            if !self.frames.is_null() {
                av_buffer_unref(&mut self.frames);
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

/// Every `/dev/dri/renderD*` node paired with its sysfs driver name, sorted by node name.
///
/// A thin alias for the ungated [`crate::encode::list_render_nodes`], which is THE one
/// listing of this box's render nodes (DRAGON-425) — shared with `plan::vaapi_device` so the
/// zero-copy and ordinary VAAPI paths can never disagree about what hardware is present.
/// Kept as a name here because the zero-copy call sites read better for it.
pub fn vaapi_nodes_with_drivers() -> Vec<(std::path::PathBuf, String)> {
    crate::encode::list_render_nodes()
}

/// The first VAAPI-capable render node (amdgpu/Intel), so tests + callers without a
/// specific device can find one. `None` if there's no usable node.
///
/// Behaviour is unchanged (DRAGON-425 only re-expressed it): the same sorted walk, the same
/// driver set, the same "first match wins". Both halves now live somewhere they can be
/// tested — the listing in [`vaapi_nodes_with_drivers`], the RULE in the ungated
/// [`crate::encode::default_vaapi_node_from`] — so the guess this makes and the correction
/// the recorder applies to it are decided from one place.
pub fn default_vaapi_node() -> Option<String> {
    crate::encode::default_vaapi_node_from(&vaapi_nodes_with_drivers())
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vaapi_node() -> Option<String> {
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

    /// The lazy `dlopen` loader must either resolve the whole vtable or decline
    /// cleanly — never panic, either way — and cache its verdict. Needs no GPU: it
    /// only exercises the loader plus one alloc/free pair through the vtable.
    #[test]
    fn libav_loads_or_cleanly_declines() {
        let loaded = libav().is_some();
        assert_eq!(loaded, libav().is_some(), "the load verdict must be cached, not re-probed");
        // SAFETY: an alloc/free pair on a frame nothing else can see. When libav is
        // absent both shims take their miss path (null / no-op) instead of calling.
        unsafe {
            let mut f = av_frame_alloc();
            assert_eq!(f.is_null(), !loaded, "av_frame_alloc must allocate iff libav resolved");
            av_frame_free(&mut f);
            assert!(f.is_null(), "av_frame_free must null the handle");
        }
        if !loaded {
            // Declining must look like an ordinary encoder failure, not a panic.
            let Err(err) = Encoder::new("/dev/dri/renderD128", false, 320, 240, 320, 240, 30, 1000)
            else {
                panic!("no libav must mean no encoder")
            };
            assert!(err.contains("not available"), "unexpected error: {err}");
        }
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
