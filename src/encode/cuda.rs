//! Importing a compositor dmabuf into CUDA, so NVENC can encode it without the
//! frame ever leaving the GPU (DRAGON-457).
//!
//! ## The gap this closes
//!
//! [`crate::encode::gpu`] encodes GPU-resident frames through **VAAPI**, and
//! `encode::vaapi_node` picks the render node to do it on. On a session that
//! renders on NVIDIA there is no such node to pick: the proprietary driver exposes
//! no VAAPI encode node at all, so DRAGON-425 correctly declines and the whole
//! recording falls back to the CPU readback path. The frame then makes a round trip
//! it never needed — compositor GPU → system RAM → back onto the same GPU for
//! NVENC, which was already the chosen encoder. This module removes that trip.
//!
//! ## Why the import is ours to do, and not ffmpeg's
//!
//! ffmpeg cannot map a dmabuf into CUDA. Measured on ffmpeg n8.1.2 built with
//! `--enable-cuda-llvm --enable-nvenc --enable-vulkan`, every route returns
//! `ENOSYS`: `hwmap` from Vulkan frames to CUDA (both directions, with and without
//! `reverse=1`), and deriving a CUDA device from DRM. Device DERIVATION succeeds
//! (`cuda=cu@vk`, `vulkan=vk@dr`) and then frame mapping fails anyway, so a
//! successful `av_hwdevice_ctx_create` proves nothing here.
//!
//! Every project that does this on Linux therefore performs the import itself and
//! hands ffmpeg a ready-made CUDA frame. That is what this module produces.
//!
//! ## The route
//!
//! `dmabuf fd` → `eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT)` → a GL texture the image
//! is bound to (`glEGLImageTargetTexture2DOES`) → `cuGraphicsGLRegisterImage` →
//! `cuMemcpy2D` into pitched device memory NVENC reads.
//!
//! It goes through GL because CUDA will not take the image directly:
//! `cuGraphicsEGLRegisterImage` answers `CUDA_ERROR_INVALID_VALUE` for a block-linear
//! compositor buffer. See the STATUS note at the bottom for what is still missing.
//!
//! No colour conversion is involved. The compositor hands us `XR24` (XRGB8888,
//! single plane), and `h264_nvenc` accepts a CUDA frames context whose `sw_format`
//! is `bgr0`/`bgra` directly — NVENC does RGB→YUV itself. This is worth stating
//! because the obvious-looking alternative is a dead end: `scale_cuda` cannot handle
//! RGB in an `--enable-cuda-llvm` build (its PTX carries no RGB kernels, and every
//! attempt fails with `CUDA_ERROR_NOT_FOUND: named symbol not found`).
//!
//! ## "Zero-copy", honestly
//!
//! An NVIDIA compositor buffer is block-linear and ffmpeg's CUDA frames want pitched
//! device memory, so [`CudaImport::copy_mapped`] issues a `cuMemcpy2D` between them.
//! That is a **device-to-device** copy: it stays in VRAM and never crosses PCIe,
//! which is where the cost being removed actually lives. Call it GPU-resident rather
//! than literally zero-copy, and measure it rather than assert it.
//!
//! ## Why the FBO blit is not optional
//!
//! CUDA will not register a texture whose storage IS an imported EGLImage — both
//! `cuGraphicsEGLRegisterImage` on the image and `cuGraphicsGLRegisterImage` on a
//! texture bound straight to it answer `CUDA_ERROR_INVALID_VALUE`. So the imported
//! texture is only ever a blit SOURCE, and what CUDA registers is a plain
//! `glTexImage2D` texture we allocated. That is why this module carries framebuffers
//! at all.
//!
//! The blit earns its keep twice over: it also does the DOWNSCALE for the user's
//! max-resolution cap, since `glBlitFramebuffer` scales between differently sized
//! rectangles with hardware filtering. Without it this path could only ever record at
//! full capture size.
//!
//! ## Loading
//!
//! `libcuda` and `libEGL` are `dlopen`ed here, never linked. This matters beyond
//! startup cost: a machine with no NVIDIA driver, the public GPL build, and every
//! `--no-default-features` config must behave exactly as before. The vtable is
//! SEPARATE from [`crate::encode::gpu`]'s libav one on purpose — that one publishes
//! nothing unless every symbol resolves, so folding CUDA into it would take VAAPI
//! zero-copy down on every AMD and Intel box that has no `libcuda`.

// An FFI module: the `unsafe fn`s are wall-to-wall raw-pointer driver calls, so the
// per-op `unsafe {}` blocks edition 2024 wants would be pure noise. Matches `gpu.rs`.
#![allow(unsafe_op_in_unsafe_fn)]
// The vtable fields and their shims carry the DRIVER's names (`cuMemcpy2D_v2`,
// `eglCreateImageKHR`) so that every call site reads as the C call it is and can be
// grepped against the CUDA/EGL docs. Renaming them to snake case would make this
// module harder to check against the headers it binds, which is the only thing
// keeping its ABI honest.
#![allow(non_snake_case)]

use std::os::fd::{AsRawFd, BorrowedFd};
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Types the driver headers define, restated (we bind no CUDA/EGL crate)
// ---------------------------------------------------------------------------

type CUresult = libc::c_int;
type CUdevice = libc::c_int;
type CUcontext = *mut libc::c_void;
type CUdeviceptr = usize;
type CUarray = *mut libc::c_void;
type CUgraphicsResource = *mut libc::c_void;
type CUstream = *mut libc::c_void;
type EGLDisplay = *mut libc::c_void;
type EGLImageKHR = *mut libc::c_void;
type EGLDeviceEXT = *mut libc::c_void;
type EGLContext = *mut libc::c_void;
type EGLConfig = *mut libc::c_void;
type EGLSurface = *mut libc::c_void;
type GLuint = libc::c_uint;
type GLenum = libc::c_uint;
type GLint = libc::c_int;
type EGLenum = libc::c_uint;
type EGLint = libc::c_int;
type EGLBoolean = libc::c_uint;

const CUDA_SUCCESS: CUresult = 0;

/// `cuGraphicsGLRegisterImage` flags. We only ever READ the compositor's buffer, but
/// drivers vary on which flag they accept for an imported texture, so registration
/// tries the restrictive one and falls back to `NONE`.
const CU_GRAPHICS_REGISTER_FLAGS_NONE: libc::c_uint = 0x00;
const CU_GRAPHICS_REGISTER_FLAGS_READ_ONLY: libc::c_uint = 0x01;

/// `CUmemorytype` values used by [`CudaMemcpy2D`].
const CU_MEMORYTYPE_DEVICE: libc::c_uint = 2;
const CU_MEMORYTYPE_ARRAY: libc::c_uint = 3;

// EGL constants (eglext.h). Restated rather than bound so this module pulls in no
// EGL crate and no build-time EGL headers.
const EGL_NONE: EGLint = 0x3038;
const EGL_HEIGHT: EGLint = 0x3056;
const EGL_WIDTH: EGLint = 0x3057;
const EGL_LINUX_DMA_BUF_EXT: EGLenum = 0x3270;
const EGL_LINUX_DRM_FOURCC_EXT: EGLint = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: EGLint = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: EGLint = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: EGLint = 0x3274;
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: EGLint = 0x3443;
const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: EGLint = 0x3444;
const EGL_EXTENSIONS: EGLint = 0x3055;
/// Desktop OpenGL, NOT OpenGL ES. CUDA's GL interop
/// (`cuGraphicsGLRegisterImage`) is a desktop-GL facility; registering a texture that
/// belongs to a GLES context answers `CUDA_ERROR_INVALID_VALUE`.
const EGL_OPENGL_API: EGLenum = 0x30A2;
/// `EGL_KHR_no_config_context`: a context with no framebuffer config, which is what a
/// surfaceless import-only context wants.
const EGL_NO_CONFIG_KHR: EGLConfig = std::ptr::null_mut();
/// GL texture target the imported image is bound to, and the only one CUDA is asked
/// to register.
const GL_TEXTURE_2D: GLenum = 0x0DE1;
const GL_TEXTURE_MIN_FILTER: GLenum = 0x2801;
const GL_TEXTURE_MAG_FILTER: GLenum = 0x2800;
const GL_NEAREST: GLint = 0x2600;
const GL_LINEAR: GLint = 0x2601;
const GL_RGBA8: GLint = 0x8058;
const GL_RGBA: GLenum = 0x1908;
const GL_UNSIGNED_BYTE: GLenum = 0x1401;
const GL_READ_FRAMEBUFFER: GLenum = 0x8CA8;
const GL_DRAW_FRAMEBUFFER: GLenum = 0x8CA9;
const GL_COLOR_ATTACHMENT0: GLenum = 0x8CE0;
const GL_COLOR_BUFFER_BIT: libc::c_uint = 0x0000_4000;
const GL_FRAMEBUFFER_COMPLETE: GLenum = 0x8CD5;
/// `EGL_EXT_platform_device`: the display is a specific GPU, not "whatever the loader
/// picks". Required here — see [`CudaDevice::open_nvidia_display`].
const EGL_PLATFORM_DEVICE_EXT: EGLenum = 0x313F;
/// `EGL_EXT_device_drm_render_node`: an `EGLDeviceEXT`'s `/dev/dri/renderD*` path.
const EGL_DRM_RENDER_NODE_FILE_EXT: EGLint = 0x3377;
/// `EGL_EXT_device_drm`: an `EGLDeviceEXT`'s `/dev/dri/card*` path — the fallback when
/// the render-node extension is absent.
const EGL_DRM_DEVICE_FILE_EXT: EGLint = 0x3233;

/// `CUDA_MEMCPY2D` (cuda.h). Field order and types are load-bearing; the enum members
/// are `unsigned int` with the pointer members naturally aligned after them.
#[repr(C)]
#[derive(Clone, Copy)]
struct CudaMemcpy2D {
    src_x_in_bytes: usize,
    src_y: usize,
    src_memory_type: libc::c_uint,
    src_host: *const libc::c_void,
    src_device: CUdeviceptr,
    src_array: CUarray,
    src_pitch: usize,
    dst_x_in_bytes: usize,
    dst_y: usize,
    dst_memory_type: libc::c_uint,
    dst_host: *mut libc::c_void,
    dst_device: CUdeviceptr,
    dst_array: CUarray,
    dst_pitch: usize,
    width_in_bytes: usize,
    height: usize,
}

impl Default for CudaMemcpy2D {
    fn default() -> Self {
        // SAFETY: every field is an integer or a raw pointer, for which an all-zero
        // bit pattern is a valid value (null pointers, zero offsets, CU_MEMORYTYPE_HOST
        // — all of which the caller overwrites before use).
        unsafe { std::mem::zeroed() }
    }
}

// ---------------------------------------------------------------------------
// Lazy libcuda + libEGL linkage
// ---------------------------------------------------------------------------

/// The two `dlopen` handles a resolve pass reads symbols out of. Deliberately never
/// closed: once resolved, the vtable holds pointers into these mappings for the rest
/// of the process. Same arrangement as `gpu.rs`'s libav handles.
struct Handles {
    cuda: *mut libc::c_void,
    egl: *mut libc::c_void,
    gles: *mut libc::c_void,
}

/// Declare every CUDA/EGL entry point once: the [`Driver`] vtable field, its `dlsym`
/// step, and a module-level shim of the same name that dispatches through the vtable.
///
/// Unlike `gpu.rs`'s equivalent there is no "miss" value: nothing in this module may
/// be called before [`load`] has published a vtable, and every entry point is reached
/// through a [`CudaDevice`] that only exists once loading succeeded. A shim that ran
/// without a vtable would be a bug, so it says so rather than inventing a return.
/// Two resolution mechanisms, because EGL needs both:
///
/// * `dlsym` — ordinary exported symbols. Every CUDA entry point, and EGL's own core.
/// * `eglproc` — EGL **extension** entry points, which are NOT in `libEGL.so.1`'s
///   symbol table at all under libglvnd and resolve only through `eglGetProcAddress`.
///   `eglCreateImageKHR` is the one that matters here, and looking it up with `dlsym`
///   simply reports it missing, which reads exactly like "no NVIDIA driver installed".
macro_rules! driver_syms {
    (
        dlsym { $( $lib:ident fn $name:ident ( $($an:ident : $at:ty),* ) -> $ret:ty; )+ }
        eglproc { $( fn $ename:ident ( $($ean:ident : $eat:ty),* ) -> $eret:ty; )+ }
    ) => {
        /// Resolved CUDA/EGL entry points. Built once by [`driver`].
        struct Driver {
            $( $name: unsafe extern "C" fn($($at),*) -> $ret, )+
            $( $ename: unsafe extern "C" fn($($eat),*) -> $eret, )+
        }

        impl Driver {
            /// `None` if any single symbol is missing — a partial vtable is never
            /// published, so callers see "all there" or "none there".
            unsafe fn resolve(h: &Handles) -> Option<Self> {
                $( let $name = std::mem::transmute::<
                    *mut libc::c_void,
                    unsafe extern "C" fn($($at),*) -> $ret,
                >(dlsym(h.$lib, concat!(stringify!($name), "\0"))?); )+
                // Resolved here rather than taken from the list above: macro hygiene
                // keeps a call-site `$name` binding invisible to this def-site code.
                let getproc = std::mem::transmute::<
                    *mut libc::c_void,
                    unsafe extern "C" fn(*const libc::c_char) -> *mut libc::c_void,
                >(dlsym(h.egl, "eglGetProcAddress\0")?);
                $( let $ename = std::mem::transmute::<
                    *mut libc::c_void,
                    unsafe extern "C" fn($($eat),*) -> $eret,
                >(eglproc(getproc, concat!(stringify!($ename), "\0"))?); )+
                Some(Driver { $( $name, )+ $( $ename, )+ })
            }
        }

        $(
            #[inline]
            // These mirror C signatures verbatim (`glTexImage2D` takes 9,
            // `glBlitFramebuffer` 10); the whole point of the shim is that it reads
            // exactly like the call it dispatches to.
            #[allow(clippy::too_many_arguments)]
            unsafe fn $name($($an: $at),*) -> $ret {
                let d = driver().expect("CUDA/EGL entry point called before the vtable loaded");
                (d.$name)($($an),*)
            }
        )+
        $(
            #[inline]
            unsafe fn $ename($($ean: $eat),*) -> $eret {
                let d = driver().expect("CUDA/EGL entry point called before the vtable loaded");
                (d.$ename)($($ean),*)
            }
        )+
    };
}

driver_syms! {
    dlsym {
        cuda fn cuInit(flags: libc::c_uint) -> CUresult;
        cuda fn cuDeviceGet(device: *mut CUdevice, ordinal: libc::c_int) -> CUresult;
        cuda fn cuDevicePrimaryCtxRetain(pctx: *mut CUcontext, dev: CUdevice) -> CUresult;
        cuda fn cuDevicePrimaryCtxRelease_v2(dev: CUdevice) -> CUresult;
        cuda fn cuCtxPushCurrent_v2(ctx: CUcontext) -> CUresult;
        cuda fn cuCtxPopCurrent_v2(pctx: *mut CUcontext) -> CUresult;
        cuda fn cuMemAllocPitch_v2(dptr: *mut CUdeviceptr, pitch: *mut usize, width_in_bytes: usize, height: usize, element_size: libc::c_uint) -> CUresult;
        cuda fn cuMemFree_v2(dptr: CUdeviceptr) -> CUresult;
        cuda fn cuMemcpy2D_v2(pcopy: *const CudaMemcpy2D) -> CUresult;
        cuda fn cuGraphicsGLRegisterImage(pcuda_resource: *mut CUgraphicsResource, image: GLuint, target: GLenum, flags: libc::c_uint) -> CUresult;
        cuda fn cuGraphicsMapResources(count: libc::c_uint, resources: *mut CUgraphicsResource, stream: CUstream) -> CUresult;
        cuda fn cuGraphicsUnmapResources(count: libc::c_uint, resources: *mut CUgraphicsResource, stream: CUstream) -> CUresult;
        cuda fn cuGraphicsSubResourceGetMappedArray(parray: *mut CUarray, resource: CUgraphicsResource, array_index: libc::c_uint, mip_level: libc::c_uint) -> CUresult;
        cuda fn cuGraphicsUnregisterResource(resource: CUgraphicsResource) -> CUresult;
        cuda fn cuStreamSynchronize(stream: CUstream) -> CUresult;
        egl fn eglInitialize(dpy: EGLDisplay, major: *mut EGLint, minor: *mut EGLint) -> EGLBoolean;
        egl fn eglQueryString(dpy: EGLDisplay, name: EGLint) -> *const libc::c_char;
        egl fn eglBindAPI(api: EGLenum) -> EGLBoolean;
        egl fn eglCreateContext(dpy: EGLDisplay, config: EGLConfig, share: EGLContext, attrib_list: *const EGLint) -> EGLContext;
        egl fn eglMakeCurrent(dpy: EGLDisplay, draw: EGLSurface, read: EGLSurface, ctx: EGLContext) -> EGLBoolean;
        egl fn eglGetError() -> EGLint;
        gles fn glGenTextures(n: libc::c_int, textures: *mut GLuint) -> ();
        gles fn glBindTexture(target: GLenum, texture: GLuint) -> ();
        gles fn glTexParameteri(target: GLenum, pname: GLenum, param: GLint) -> ();
        gles fn glTexImage2D(target: GLenum, level: GLint, internalformat: GLint, width: libc::c_int, height: libc::c_int, border: GLint, format: GLenum, ty: GLenum, pixels: *const libc::c_void) -> ();
        gles fn glDeleteTextures(n: libc::c_int, textures: *const GLuint) -> ();
        gles fn glGenFramebuffers(n: libc::c_int, framebuffers: *mut GLuint) -> ();
        gles fn glBindFramebuffer(target: GLenum, framebuffer: GLuint) -> ();
        gles fn glFramebufferTexture2D(target: GLenum, attachment: GLenum, textarget: GLenum, texture: GLuint, level: GLint) -> ();
        gles fn glCheckFramebufferStatus(target: GLenum) -> GLenum;
        gles fn glBlitFramebuffer(sx0: GLint, sy0: GLint, sx1: GLint, sy1: GLint, dx0: GLint, dy0: GLint, dx1: GLint, dy1: GLint, mask: libc::c_uint, filter: GLenum) -> ();
        gles fn glDeleteFramebuffers(n: libc::c_int, framebuffers: *const GLuint) -> ();
        gles fn glGetError() -> GLenum;
        gles fn glFinish() -> ();
    }
    eglproc {
        fn eglCreateImageKHR(dpy: EGLDisplay, ctx: *mut libc::c_void, target: EGLenum, buffer: *mut libc::c_void, attrib_list: *const EGLint) -> EGLImageKHR;
        fn eglDestroyImageKHR(dpy: EGLDisplay, image: EGLImageKHR) -> EGLBoolean;
        fn eglQueryDevicesEXT(max_devices: EGLint, devices: *mut EGLDeviceEXT, num_devices: *mut EGLint) -> EGLBoolean;
        fn eglQueryDeviceStringEXT(device: EGLDeviceEXT, name: EGLint) -> *const libc::c_char;
        fn eglGetPlatformDisplayEXT(platform: EGLenum, native_display: *mut libc::c_void, attrib_list: *const EGLint) -> EGLDisplay;
        fn glEGLImageTargetTexture2DOES(target: GLenum, image: EGLImageKHR) -> ();
    }
}

/// Resolve an EGL extension entry point, reporting absence rather than handing back a
/// null to be called. Takes the already-resolved `eglGetProcAddress` directly, since
/// the vtable it would otherwise come from is still being built.
unsafe fn eglproc(
    get: unsafe extern "C" fn(*const libc::c_char) -> *mut libc::c_void,
    name: &str,
) -> Option<*mut libc::c_void> {
    let p = get(name.as_ptr().cast());
    (!p.is_null()).then_some(p)
}

/// The kernel driver behind a `/dev/dri/*` node, via sysfs — `"nvidia"`, `"amdgpu"`,
/// `"i915"`. `None` when the node has no driver link (it vanished, or is not a DRM
/// node at all).
fn driver_for_node(node: &str) -> Option<String> {
    let name = std::path::Path::new(node).file_name()?.to_str()?;
    let link = std::fs::read_link(format!("/sys/class/drm/{name}/device/driver")).ok()?;
    Some(link.file_name()?.to_str()?.to_string())
}

/// `dlsym` that reports absence rather than returning a null to be called.
unsafe fn dlsym(handle: *mut libc::c_void, name: &str) -> Option<*mut libc::c_void> {
    let p = libc::dlsym(handle, name.as_ptr().cast());
    (!p.is_null()).then_some(p)
}

/// `dlopen` with the standard soname, then the unversioned fallback.
unsafe fn dlopen_any(names: &[&str]) -> Option<*mut libc::c_void> {
    for n in names {
        let h = libc::dlopen(n.as_ptr().cast(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
        if !h.is_null() {
            return Some(h);
        }
    }
    None
}

static DRIVER: OnceLock<Option<Driver>> = OnceLock::new();

/// The resolved vtable, loading on first use. `None` once means `None` forever: a box
/// with no NVIDIA driver must not re-`dlopen` on every frame.
fn driver() -> Option<&'static Driver> {
    DRIVER
        .get_or_init(|| unsafe {
            let handles = Handles {
                cuda: dlopen_any(&["libcuda.so.1\0", "libcuda.so\0"])?,
                egl: dlopen_any(&["libEGL.so.1\0", "libEGL.so\0"])?,
                // Desktop GL, for the CUDA interop reason at `EGL_OPENGL_API`. The
                // GLES library is the fallback rather than the target.
                gles: dlopen_any(&[
                    "libGL.so.1\0",
                    "libGL.so\0",
                    "libGLESv2.so.2\0",
                    "libGLESv2.so\0",
                ])?,
            };
            Driver::resolve(&handles)
        })
        .as_ref()
}

/// Whether this machine can import dmabufs into CUDA at all: both libraries load,
/// every symbol resolves, EGL initialises, and a CUDA device exists.
///
/// This is the probe behind the optional-dependency row on the Health page, so it
/// must be cheap enough to call from settings and must never panic. It is memoised
/// through [`CudaDevice::get`], so asking twice costs nothing.
pub fn available() -> bool {
    CudaDevice::get().is_some()
}

// ---------------------------------------------------------------------------
// The device: an EGL display + a CUDA primary context, one per process
// ---------------------------------------------------------------------------

/// Holds this thread's CUDA context current, popping it again on drop. See
/// [`CudaDevice::make_current`].
pub struct ContextGuard {
    _private: (),
}

impl Drop for ContextGuard {
    fn drop(&mut self) {
        unsafe {
            let mut popped: CUcontext = std::ptr::null_mut();
            cuCtxPopCurrent_v2(&mut popped);
        }
    }
}

/// One plane of a compositor dmabuf, as [`CudaDevice::import`] needs to see it.
///
/// The fd is borrowed: EGL takes its own reference to the underlying buffer, so the
/// caller keeps ownership and can hand the same fd over for every frame.
pub struct DmabufDesc<'a> {
    pub fd: BorrowedFd<'a>,
    /// DRM fourcc, as the compositor reports it (`XR24` here).
    pub fourcc: u32,
    /// DRM format modifier — the layout. NVIDIA's are block-linear, not linear.
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
    pub offset: u32,
    pub stride: u32,
    /// The size to ENCODE at. When it differs from `width`/`height` the downscale
    /// happens inside the GL blit that this path already performs
    /// (`glBlitFramebuffer` scales, with hardware filtering), so honouring the user's
    /// max-resolution cap costs nothing extra and still never touches the CPU.
    pub dst_width: u32,
    pub dst_height: u32,
}

/// The process-wide EGL display and CUDA context every import is made against.
///
/// One per process on purpose: `cuDevicePrimaryCtxRetain` hands back the same context
/// the rest of the driver stack uses, which is what lets the imported memory be read
/// by an NVENC session ffmpeg opened separately.
pub struct CudaDevice {
    display: EGLDisplay,
    ctx: CUcontext,
    device: CUdevice,
    /// A surfaceless GL context on [`Self::display`].
    ///
    /// Needed because CUDA will not register the EGLImage directly:
    /// block-linear compositor buffer. The supported route binds the EGLImage to a GL
    /// texture (`glEGLImageTargetTexture2DOES`) and registers THAT
    /// (`cuGraphicsGLRegisterImage`), which is what the reference implementations do.
    gl_ctx: EGLContext,
}

// SAFETY: the EGL display and the CUDA primary context are process-wide handles the
// driver itself synchronises; the recording worker owns the only `CudaImport`s and
// pushes the context around each use.
unsafe impl Send for CudaDevice {}
unsafe impl Sync for CudaDevice {}

static DEVICE: OnceLock<Option<CudaDevice>> = OnceLock::new();

impl CudaDevice {
    /// The shared device, initialising on first call. `None` when this machine cannot
    /// do the import, which is the caller's cue to decline zero-copy and let the CPU
    /// readback path have the recording.
    pub fn get() -> Option<&'static CudaDevice> {
        DEVICE.get_or_init(|| unsafe { Self::init() }).as_ref()
    }

    unsafe fn init() -> Option<CudaDevice> {
        // Resolving the vtable is the first thing that can fail, and the common way:
        // no NVIDIA driver installed at all.
        driver()?;

        let display = Self::open_nvidia_display()?;
        // The import is `EGL_EXT_image_dma_buf_import`, and a buffer carrying a DRM
        // modifier additionally needs `..._modifiers`. Check rather than discover it
        // through a null image later, so the decline names its own reason.
        let exts = eglQueryString(display, EGL_EXTENSIONS);
        if exts.is_null() {
            log::debug!("cuda import: EGL exposes no extension string");
            return None;
        }
        let exts = std::ffi::CStr::from_ptr(exts).to_string_lossy().into_owned();
        if !exts.contains("EGL_EXT_image_dma_buf_import") {
            log::debug!("cuda import: EGL_EXT_image_dma_buf_import missing");
            return None;
        }

        if cuInit(0) != CUDA_SUCCESS {
            log::debug!("cuda import: cuInit failed");
            return None;
        }
        let mut device: CUdevice = 0;
        if cuDeviceGet(&mut device, 0) != CUDA_SUCCESS {
            log::debug!("cuda import: no CUDA device 0");
            return None;
        }
        let mut ctx: CUcontext = std::ptr::null_mut();
        if cuDevicePrimaryCtxRetain(&mut ctx, device) != CUDA_SUCCESS {
            log::debug!("cuda import: cuDevicePrimaryCtxRetain failed");
            return None;
        }

        // A surfaceless GLES context to hang the imported texture on. `EGL_NO_CONFIG`
        // + no surface needs `EGL_KHR_no_config_context` and
        // `EGL_KHR_surfaceless_context`, both of which NVIDIA has.
        if eglBindAPI(EGL_OPENGL_API) == 0 {
            log::debug!("cuda import: eglBindAPI(OPENGL) failed");
            return None;
        }
        let ctx_attribs: [EGLint; 1] = [EGL_NONE];
        let gl_ctx = eglCreateContext(
            display,
            EGL_NO_CONFIG_KHR,
            std::ptr::null_mut(), // EGL_NO_CONTEXT
            ctx_attribs.as_ptr(),
        );
        if gl_ctx.is_null() {
            log::debug!("cuda import: eglCreateContext failed (0x{:x})", eglGetError());
            return None;
        }
        log::debug!("cuda import: EGL + GL + CUDA ready (dmabuf import available)");
        Some(CudaDevice { display, ctx, device, gl_ctx })
    }

    /// Open an EGL display bound to the NVIDIA GPU specifically.
    ///
    /// `eglGetDisplay(EGL_DEFAULT_DISPLAY)` is NOT good enough and fails in a way that
    /// looks like a driver problem: under libglvnd it can hand back **Mesa's** EGL,
    /// which then reports `pci id ... driver (null)` and `failed to create dri2 screen`
    /// for an NVIDIA card, and every subsequent `eglCreateImageKHR` declines the
    /// buffer. The display has to name the device, so we enumerate `EGLDeviceEXT`s and
    /// pick the one whose DRM node the kernel says is `nvidia`.
    unsafe fn open_nvidia_display() -> Option<EGLDisplay> {
        // Extension entry points are resolved through `eglGetProcAddress` against no
        // display, which is legal for these three (they are client extensions).
        let mut count: EGLint = 0;
        if eglQueryDevicesEXT(0, std::ptr::null_mut(), &mut count) == 0 || count <= 0 {
            log::debug!("cuda import: eglQueryDevicesEXT reported no devices");
            return None;
        }
        let mut devices: Vec<EGLDeviceEXT> = vec![std::ptr::null_mut(); count as usize];
        if eglQueryDevicesEXT(count, devices.as_mut_ptr(), &mut count) == 0 {
            log::debug!("cuda import: eglQueryDevicesEXT failed");
            return None;
        }
        devices.truncate(count.max(0) as usize);

        for dev in devices {
            // The render node is what our dmabufs are allocated on, so match on it
            // first; `EGL_DRM_DEVICE_FILE_EXT` is the older spelling.
            let node = [EGL_DRM_RENDER_NODE_FILE_EXT, EGL_DRM_DEVICE_FILE_EXT]
                .into_iter()
                .find_map(|q| {
                    let s = eglQueryDeviceStringEXT(dev, q);
                    (!s.is_null())
                        .then(|| std::ffi::CStr::from_ptr(s).to_string_lossy().into_owned())
                });
            let Some(node) = node else { continue };
            let Some(driver) = driver_for_node(&node) else { continue };
            if driver != "nvidia" {
                continue;
            }
            let display =
                eglGetPlatformDisplayEXT(EGL_PLATFORM_DEVICE_EXT, dev, std::ptr::null());
            if display.is_null() {
                log::debug!("cuda import: eglGetPlatformDisplayEXT failed for {node}");
                continue;
            }
            if eglInitialize(display, std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
                log::debug!("cuda import: eglInitialize failed for {node}");
                continue;
            }
            log::debug!("cuda import: EGL display bound to {node} (nvidia)");
            return Some(display);
        }
        log::debug!("cuda import: no EGL device is an NVIDIA DRM node");
        None
    }

    /// Make this device's CUDA context current on the calling thread until the guard
    /// drops.
    ///
    /// The encoder needs this: a device pointer only means anything inside the context
    /// it was allocated in, so ffmpeg has to be told to ADOPT ours
    /// (`AV_CUDA_USE_CURRENT_CONTEXT`) rather than open one of its own. Asking libav
    /// for the primary context instead is not equivalent, and on this stack it is not
    /// even accepted — `av_hwdevice_ctx_create` answers `EOPNOTSUPP` for
    /// `AV_CUDA_USE_PRIMARY_CONTEXT` while plain creation succeeds.
    pub fn make_current(&self) -> Result<ContextGuard, String> {
        unsafe {
            if cuCtxPushCurrent_v2(self.ctx) != CUDA_SUCCESS {
                return Err("cuCtxPushCurrent failed".into());
            }
        }
        Ok(ContextGuard { _private: () })
    }

    /// Whether `fourcc` is a single-plane packed RGB format NVENC accepts directly, so
    /// the import needs no colour conversion. `XR24`/`AR24` (XRGB8888/ARGB8888) are
    /// what cosmic-comp hands us.
    pub fn format_supported(fourcc: u32) -> bool {
        // Little-endian fourcc codes, as reported by the compositor.
        const XR24: u32 = 0x34325258; // 'XR24'
        const AR24: u32 = 0x34325241; // 'AR24'
        const XB24: u32 = 0x34324258; // 'XB24'
        const AB24: u32 = 0x34324241; // 'AB24'
        matches!(fourcc, XR24 | AR24 | XB24 | AB24)
    }

    /// Import one dmabuf plane and expose it as CUDA device memory.
    ///
    /// The fd is BORROWED for the duration of the call: EGL takes its own reference to
    /// the underlying buffer, so the caller's `OwnedFd` stays the owner and may be
    /// reused for the next frame.
    pub fn import(&self, buf: DmabufDesc<'_>) -> Result<CudaImport, String> {
        unsafe { self.import_inner(buf) }
    }

    unsafe fn import_inner(&self, buf: DmabufDesc<'_>) -> Result<CudaImport, String> {
        let DmabufDesc { fd, fourcc, modifier, width, height, offset, stride, dst_width, dst_height } = buf;
        let attribs: [EGLint; 17] = [
            EGL_WIDTH,
            width as EGLint,
            EGL_HEIGHT,
            height as EGLint,
            EGL_LINUX_DRM_FOURCC_EXT,
            fourcc as EGLint,
            EGL_DMA_BUF_PLANE0_FD_EXT,
            fd.as_raw_fd(),
            EGL_DMA_BUF_PLANE0_OFFSET_EXT,
            offset as EGLint,
            EGL_DMA_BUF_PLANE0_PITCH_EXT,
            stride as EGLint,
            EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
            (modifier & 0xffff_ffff) as EGLint,
            EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
            (modifier >> 32) as EGLint,
            EGL_NONE,
        ];
        let image = eglCreateImageKHR(
            self.display,
            std::ptr::null_mut(), // EGL_NO_CONTEXT
            EGL_LINUX_DMA_BUF_EXT,
            std::ptr::null_mut(),
            attribs.as_ptr(),
        );
        if image.is_null() {
            return Err(format!(
                "eglCreateImageKHR declined the buffer (fourcc=0x{fourcc:08x} \
                 modifier=0x{modifier:016x})"
            ));
        }

        // The GL context must be current on THIS thread before any GL call or any
        // CUDA-GL interop call, and the CUDA context must be pushed for the CUDA side.
        if eglMakeCurrent(
            self.display,
            std::ptr::null_mut(), // EGL_NO_SURFACE
            std::ptr::null_mut(),
            self.gl_ctx,
        ) == 0
        {
            let e = eglGetError();
            eglDestroyImageKHR(self.display, image);
            return Err(format!("eglMakeCurrent failed (0x{e:x})"));
        }
        if cuCtxPushCurrent_v2(self.ctx) != CUDA_SUCCESS {
            eglDestroyImageKHR(self.display, image);
            return Err("cuCtxPushCurrent failed".into());
        }
        let out = self.register(image, width, height, dst_width, dst_height);
        let mut popped: CUcontext = std::ptr::null_mut();
        cuCtxPopCurrent_v2(&mut popped);
        match out {
            Ok(import) => Ok(import),
            Err(e) => {
                eglDestroyImageKHR(self.display, image);
                Err(e)
            }
        }
    }

    /// Bind the EGLImage to a GL texture, blit it into a normally allocated texture,
    /// register THAT with CUDA, and allocate the pitched memory NVENC will read.
    /// Called with the GL context current and the CUDA context pushed.
    ///
    /// The blit is not decoration. CUDA refuses to register a texture whose storage is
    /// an imported EGLImage (`CUDA_ERROR_INVALID_VALUE`), so the imported texture is
    /// only ever a blit SOURCE, and the texture CUDA sees is one GL allocated itself.
    unsafe fn register(
        &self,
        image: EGLImageKHR,
        width: u32,
        height: u32,
        dst_width: u32,
        dst_height: u32,
    ) -> Result<CudaImport, String> {
        // 1. The imported texture: storage is the compositor's buffer.
        let mut src_tex: GLuint = 0;
        glGenTextures(1, &mut src_tex);
        if src_tex == 0 {
            return Err("glGenTextures produced no texture".into());
        }
        glBindTexture(GL_TEXTURE_2D, src_tex);
        // An imported EGLImage has no mip chain, so filtering must be non-mipmapped or
        // the texture is incomplete and the blit reads black.
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
        glEGLImageTargetTexture2DOES(GL_TEXTURE_2D, image);
        let gl_err = glGetError();
        if gl_err != 0 {
            glDeleteTextures(1, &src_tex);
            return Err(format!("glEGLImageTargetTexture2DOES failed (GL 0x{gl_err:x})"));
        }

        // 2. The destination: ordinary GL storage, which is what CUDA will register.
        let mut dst_tex: GLuint = 0;
        glGenTextures(1, &mut dst_tex);
        glBindTexture(GL_TEXTURE_2D, dst_tex);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
        glTexImage2D(
            GL_TEXTURE_2D,
            0,
            GL_RGBA8,
            dst_width as libc::c_int,
            dst_height as libc::c_int,
            0,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            std::ptr::null(),
        );
        glBindTexture(GL_TEXTURE_2D, 0);
        let gl_err = glGetError();
        if gl_err != 0 {
            glDeleteTextures(1, &src_tex);
            glDeleteTextures(1, &dst_tex);
            return Err(format!("allocating the blit destination failed (GL 0x{gl_err:x})"));
        }

        // 3. Framebuffers to blit between, built once and reused every frame.
        let (mut read_fbo, mut draw_fbo): (GLuint, GLuint) = (0, 0);
        glGenFramebuffers(1, &mut read_fbo);
        glGenFramebuffers(1, &mut draw_fbo);
        glBindFramebuffer(GL_READ_FRAMEBUFFER, read_fbo);
        glFramebufferTexture2D(GL_READ_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, src_tex, 0);
        let read_ok = glCheckFramebufferStatus(GL_READ_FRAMEBUFFER);
        glBindFramebuffer(GL_DRAW_FRAMEBUFFER, draw_fbo);
        glFramebufferTexture2D(GL_DRAW_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, dst_tex, 0);
        let draw_ok = glCheckFramebufferStatus(GL_DRAW_FRAMEBUFFER);
        glBindFramebuffer(GL_READ_FRAMEBUFFER, 0);
        glBindFramebuffer(GL_DRAW_FRAMEBUFFER, 0);
        if read_ok != GL_FRAMEBUFFER_COMPLETE || draw_ok != GL_FRAMEBUFFER_COMPLETE {
            glDeleteFramebuffers(1, &read_fbo);
            glDeleteFramebuffers(1, &draw_fbo);
            glDeleteTextures(1, &src_tex);
            glDeleteTextures(1, &dst_tex);
            return Err(format!(
                "framebuffers incomplete (read 0x{read_ok:x}, draw 0x{draw_ok:x})"
            ));
        }

        // 4. CUDA registers the ORDINARY texture.
        let mut res: CUgraphicsResource = std::ptr::null_mut();
        // READ_ONLY is what we mean, but drivers vary on which flags they accept, so
        // take either rather than fail on the stricter one.
        let mut r = cuGraphicsGLRegisterImage(
            &mut res,
            dst_tex,
            GL_TEXTURE_2D,
            CU_GRAPHICS_REGISTER_FLAGS_READ_ONLY,
        );
        if r != CUDA_SUCCESS {
            r = cuGraphicsGLRegisterImage(
                &mut res,
                dst_tex,
                GL_TEXTURE_2D,
                CU_GRAPHICS_REGISTER_FLAGS_NONE,
            );
        }
        if r != CUDA_SUCCESS {
            glDeleteFramebuffers(1, &read_fbo);
            glDeleteFramebuffers(1, &draw_fbo);
            glDeleteTextures(1, &src_tex);
            glDeleteTextures(1, &dst_tex);
            return Err(format!("cuGraphicsGLRegisterImage failed ({r})"));
        }

        // 5. The pitched memory NVENC reads.
        // 4 bytes per pixel: every format `format_supported` admits is packed 32-bit.
        let width_in_bytes = dst_width as usize * 4;
        let mut dptr: CUdeviceptr = 0;
        let mut pitch: usize = 0;
        let r = cuMemAllocPitch_v2(&mut dptr, &mut pitch, width_in_bytes, dst_height as usize, 16);
        if r != CUDA_SUCCESS {
            cuGraphicsUnregisterResource(res);
            glDeleteFramebuffers(1, &read_fbo);
            glDeleteFramebuffers(1, &draw_fbo);
            glDeleteTextures(1, &src_tex);
            glDeleteTextures(1, &dst_tex);
            return Err(format!("cuMemAllocPitch failed ({r})"));
        }
        let import = CudaImport {
            display: self.display,
            gl_ctx: self.gl_ctx,
            cu_ctx: self.ctx,
            image,
            src_tex,
            dst_tex,
            read_fbo,
            draw_fbo,
            resource: res,
            device_ptr: dptr,
            pitch,
            src_w: width as libc::c_int,
            src_h: height as libc::c_int,
            dst_w: dst_width as libc::c_int,
            dst_h: dst_height as libc::c_int,
            width_in_bytes,
            height: dst_height as usize,
        };
        import.blit_and_copy()?;
        Ok(import)
    }
}

// ---------------------------------------------------------------------------
// A live import
// ---------------------------------------------------------------------------

/// One imported dmabuf, readable as CUDA device memory for as long as it is held.
///
/// Dropping it unregisters the CUDA resource and destroys the EGLImage; the caller's
/// dmabuf fd is untouched, since EGL held its own reference to the buffer.
pub struct CudaImport {
    display: EGLDisplay,
    gl_ctx: EGLContext,
    cu_ctx: CUcontext,
    image: EGLImageKHR,
    /// The compositor's buffer as a GL texture. Blit SOURCE only — CUDA will not
    /// register a texture whose storage is an imported EGLImage.
    src_tex: GLuint,
    /// Ordinary GL storage, the blit destination, and what CUDA registers.
    dst_tex: GLuint,
    read_fbo: GLuint,
    draw_fbo: GLuint,
    resource: CUgraphicsResource,
    /// Pitched device memory NVENC reads, filled by [`Self::blit_and_copy`].
    device_ptr: CUdeviceptr,
    pitch: usize,
    src_w: libc::c_int,
    src_h: libc::c_int,
    dst_w: libc::c_int,
    dst_h: libc::c_int,
    width_in_bytes: usize,
    height: usize,
}

// SAFETY: the handles are driver-owned and only used from the recording worker, which
// owns the import for its whole life.
unsafe impl Send for CudaImport {}

impl CudaImport {
    /// The device pointer NVENC reads, and its row pitch in bytes.
    pub fn device_ptr(&self) -> (usize, usize) {
        (self.device_ptr, self.pitch)
    }

    /// Re-read the compositor's buffer into our device memory. Call once per captured
    /// frame, before encoding.
    ///
    /// The contexts are made current here rather than by the caller: the recording
    /// worker calls this per frame and has no business knowing about EGL.
    pub fn refresh(&self) -> Result<(), String> {
        unsafe {
            if eglMakeCurrent(
                self.display,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                self.gl_ctx,
            ) == 0
            {
                return Err(format!("eglMakeCurrent failed (0x{:x})", eglGetError()));
            }
            if cuCtxPushCurrent_v2(self.cu_ctx) != CUDA_SUCCESS {
                return Err("cuCtxPushCurrent failed".into());
            }
            let out = self.blit_and_copy();
            let mut popped: CUcontext = std::ptr::null_mut();
            cuCtxPopCurrent_v2(&mut popped);
            out
        }
    }

    /// Refresh from the compositor's buffer straight into CALLER-owned pitched device
    /// memory — the encoder's path, where the destination is a frame ffmpeg allocated.
    ///
    /// Writing into ffmpeg's own frame is what keeps this to ONE device-to-device copy.
    /// [`Self::refresh`] exists for the probe and lands in memory this import owns.
    pub fn refresh_into(&self, dst: usize, dst_pitch: usize) -> Result<(), String> {
        unsafe {
            if eglMakeCurrent(
                self.display,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                self.gl_ctx,
            ) == 0
            {
                return Err(format!("eglMakeCurrent failed (0x{:x})", eglGetError()));
            }
            if cuCtxPushCurrent_v2(self.cu_ctx) != CUDA_SUCCESS {
                return Err("cuCtxPushCurrent failed".into());
            }
            let out = self.blit().and_then(|()| self.copy_mapped_into(dst, dst_pitch));
            let mut popped: CUcontext = std::ptr::null_mut();
            cuCtxPopCurrent_v2(&mut popped);
            out
        }
    }

    /// Refresh the destination texture from the compositor's buffer, then read it into
    /// pitched CUDA memory.
    ///
    /// Both halves stay on the GPU: a GL blit inside VRAM, then a device-to-device
    /// `cuMemcpy2D`. The frame never crosses PCIe, which is the cost this path exists
    /// to remove.
    fn blit_and_copy(&self) -> Result<(), String> {
        self.blit()?;
        self.copy_mapped()
    }

    /// The GL half: refresh the destination texture from the compositor's buffer.
    fn blit(&self) -> Result<(), String> {
        unsafe {
            glBindFramebuffer(GL_READ_FRAMEBUFFER, self.read_fbo);
            glBindFramebuffer(GL_DRAW_FRAMEBUFFER, self.draw_fbo);
            // Scaling blit: source rect is the whole captured frame, destination the
            // encode size. GL_LINEAR when downscaling (NEAREST would alias badly on a
            // desktop full of text); NEAREST when the sizes match, which is exact.
            let filter =
                if (self.src_w, self.src_h) == (self.dst_w, self.dst_h) { GL_NEAREST } else { GL_LINEAR };
            glBlitFramebuffer(
                0,
                0,
                self.src_w,
                self.src_h,
                0,
                0,
                self.dst_w,
                self.dst_h,
                GL_COLOR_BUFFER_BIT,
                filter as GLenum,
            );
            let gl_err = glGetError();
            glBindFramebuffer(GL_READ_FRAMEBUFFER, 0);
            glBindFramebuffer(GL_DRAW_FRAMEBUFFER, 0);
            if gl_err != 0 {
                return Err(format!("glBlitFramebuffer failed (GL 0x{gl_err:x})"));
            }
            // CUDA reads the texture next, so the blit has to have landed first.
            glFinish();
        }
        Ok(())
    }

    /// Map the registered texture, copy it into our own pitched memory, unmap.
    fn copy_mapped(&self) -> Result<(), String> {
        self.copy_mapped_into(self.device_ptr, self.pitch)
    }

    /// Map the registered texture, copy it into `dst`, unmap.
    ///
    /// The copy is **device to device**: the frame stays in VRAM and never crosses
    /// PCIe, which is the cost this whole path exists to remove. A block-linear
    /// texture cannot be read by NVENC directly, so the blit is what makes it linear.
    fn copy_mapped_into(&self, dst: CUdeviceptr, dst_pitch: usize) -> Result<(), String> {
        unsafe {
            let mut res = self.resource;
            let r = cuGraphicsMapResources(1, &mut res, std::ptr::null_mut());
            if r != CUDA_SUCCESS {
                return Err(format!("cuGraphicsMapResources failed ({r})"));
            }
            let mut array: CUarray = std::ptr::null_mut();
            let r = cuGraphicsSubResourceGetMappedArray(&mut array, res, 0, 0);
            if r != CUDA_SUCCESS {
                cuGraphicsUnmapResources(1, &mut res, std::ptr::null_mut());
                return Err(format!("cuGraphicsSubResourceGetMappedArray failed ({r})"));
            }
            let copy = CudaMemcpy2D {
                src_memory_type: CU_MEMORYTYPE_ARRAY,
                src_array: array,
                dst_memory_type: CU_MEMORYTYPE_DEVICE,
                dst_device: dst,
                dst_pitch,
                width_in_bytes: self.width_in_bytes,
                height: self.height,
                ..CudaMemcpy2D::default()
            };
            let r = cuMemcpy2D_v2(&copy);
            let sync = cuStreamSynchronize(std::ptr::null_mut());
            cuGraphicsUnmapResources(1, &mut res, std::ptr::null_mut());
            if r != CUDA_SUCCESS {
                return Err(format!("cuMemcpy2D failed ({r})"));
            }
            if sync != CUDA_SUCCESS {
                return Err(format!("cuStreamSynchronize failed ({sync})"));
            }
            Ok(())
        }
    }
}

impl Drop for CudaImport {
    fn drop(&mut self) {
        unsafe {
            // Frees happen in our context, like the allocation did.
            let Some(dev) = DEVICE.get().and_then(|d| d.as_ref()) else {
                return;
            };
            let _ = eglMakeCurrent(
                self.display,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                self.gl_ctx,
            );
            if cuCtxPushCurrent_v2(dev.ctx) == CUDA_SUCCESS {
                cuMemFree_v2(self.device_ptr);
                cuGraphicsUnregisterResource(self.resource);
                let mut popped: CUcontext = std::ptr::null_mut();
                cuCtxPopCurrent_v2(&mut popped);
            }
            glDeleteFramebuffers(1, &self.read_fbo);
            glDeleteFramebuffers(1, &self.draw_fbo);
            glDeleteTextures(1, &self.src_tex);
            glDeleteTextures(1, &self.dst_tex);
            eglDestroyImageKHR(self.display, self.image);
        }
    }
}

impl Drop for CudaDevice {
    fn drop(&mut self) {
        // Only reached if the OnceLock is ever torn down (it is not, in practice);
        // written for correctness rather than as a live path.
        unsafe {
            cuDevicePrimaryCtxRelease_v2(self.device);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The formats cosmic-comp actually offers, and the ones NVENC takes without a
    // colour conversion. Pure, so it is checked on every platform.
    #[test]
    fn packed_rgb_formats_are_importable() {
        assert!(CudaDevice::format_supported(0x34325258), "XR24 is what we are handed");
        assert!(CudaDevice::format_supported(0x34325241), "AR24");
        assert!(CudaDevice::format_supported(0x34324258), "XB24");
    }

    #[test]
    fn planar_and_unknown_formats_are_declined() {
        // NV12 ('NV12') is planar: NVENC would take it, but the import path here is
        // written for single-plane packed RGB and must not claim it.
        assert!(!CudaDevice::format_supported(0x3231564e));
        assert!(!CudaDevice::format_supported(0));
    }

    // A wrong `CUDA_MEMCPY2D` layout corrupts the driver's view of the copy, and the
    // failure would look like garbled video rather than an error. Pin the size against
    // the header's own (16 members: 10 pointer-width, 3 enums padded to pointer width).
    #[test]
    fn memcpy2d_layout_matches_the_driver_abi() {
        assert_eq!(
            std::mem::size_of::<CudaMemcpy2D>(),
            16 * std::mem::size_of::<usize>(),
            "CUDA_MEMCPY2D must stay ABI-identical to cuda.h"
        );
    }

}
