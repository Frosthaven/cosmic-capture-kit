//! Real-time GPU rendering of the image editor's REGION EFFECTS — highlight / pixelate /
//! blur (DRAGON-330) — as a stack of wgpu shader passes with TRUE Z-ORDER and
//! destructive-samples-below, replacing the old CPU raster overlay. The effect geometry is a
//! tiny per-frame uniform, so a live drag just re-renders instantly (no CPU round-trip, no
//! raster upload) at any image size.
//!
//! # Pass architecture (ping-pong = destructive-samples-below + z-order)
//! The effect stack samples the ACCUMULATED result of everything below it, so a pixelate over
//! a highlight redacts the highlight. Because block-mean effects sample a NEIGHBOURHOOD of the
//! accumulated content, this needs two offscreen textures ping-ponged:
//!
//! 1. **seed** — downscale the base into `ping[0]` (RGB = base, alpha = 0 coverage).
//! 2. **per effect item, in z-order** — read the accumulated `ping[cur]`, apply THIS effect
//!    within its anti-aliased rounded-rect SDF mask, write `ping[1-cur]`; swap. Inside the
//!    mask the effect samples the accumulated texture (block mean / low-pass), folds edge
//!    coverage into RGB, and marks the coverage alpha; outside the mask it passes the
//!    accumulated texel through unchanged. A LATER effect therefore samples the earlier one's
//!    result — destructive redaction of everything below.
//! 3. **final** — sample the last accumulated texture and blit it over the base (the surface
//!    already holds the base image), straight-alpha blended, positioned to the primitive's
//!    on-screen bounds and scissored to the clip rect (the ZoomPan content viewport), so the
//!    effects zoom/pan with the picture and never bleed over the scrollbars.
//!
//! The intermediate passes render into full offscreen textures (no clip); only the final blit
//! uses the primitive's viewport + the clip scissor. This uses the [`shader::Primitive::render`]
//! (encoder) variant so the multiple offscreen passes + final blit run in one primitive.
//!
//! # WYSIWYG vs the bake
//! The GPU math MIRRORS the CPU bake core (`apply_effects` in
//! [`crate::app::preview::annotate`]): pixelate = a grid-aligned [`PIXELATE_BLOCK`]-cell block
//! mean of the accumulated content; blur = a [`BLUR_BLOCK`]-window mean; highlight = the
//! adaptive multiply/screen (`mix(screen, multiply, w)`, `w = smoothstep(0.35, 0.65,
//! luma(low-pass))`) weighted by [`HIGHLIGHT_ALPHA`]. Block sizes are matched in SOURCE px,
//! scaled by the display/source ratio in-shader. It is a GPU re-implementation, so it is NOT
//! pixel-identical to the CPU bake (edges are SDF `fwidth` AA vs tiny-skia coverage; blur is a
//! sliding-window box mean vs the bake's block-mean bilinear upsample) — it must VISUALLY
//! match, especially redaction coverage. The CPU `apply_effects` bake stays the exact save
//! path with its unit tests.
//!
//! # Why every piece of GPU state here is keyed by WINDOW
//! iced stores a shader `Pipeline` by the primitive's `TypeId` ALONE, and the `Engine`
//! holding that storage is cloned (an `Arc`) into each window's renderer — so there is
//! exactly ONE [`EffectsPipeline`] per PROCESS however many windows draw effects. Everything
//! that is per-preview therefore lives in a [`WindowFx`] looked up by `window::Id`: the base
//! texture, the ping-pong accumulators (and their dims), the uniform-buffer pool, the
//! per-frame pass plan, and the on-screen blit bounds. Sharing them would have two previews
//! destroy/recreate each other's base every frame (differing dims) and cross-write the plan
//! and viewport. Only the truly device-level things — the two render pipelines, the bind
//! group layout, the sampler, the texture format — stay shared, since they depend on nothing
//! but the device and the surface format.
//!
//! This is deliberately state SEPARATION rather than a claim about iced's per-window
//! prepare→render ordering (which cannot be verified headlessly): a window's `prepare`
//! writes only its own entry and its `render` reads only its own entry, so ANY interleaving
//! across windows is safe. The one ordering fact still relied on is intra-window and is
//! iced's own contract — a primitive's `prepare` precedes its `render` for the same frame.
//!
//! As with the layer stack, at most ONE `EffectsFx` may be mounted per window: a second one
//! in the same window would overwrite the first's plan. (Today `still_media` mounts exactly
//! one.) A CLOSED window's entry IS reclaimed: the next `prepare` of any surviving preview
//! evicts it (see [`EffectsPrimitive::live`]). That eviction has to ride the primitive
//! because iced's pipeline storage exposes no external handle — app code cannot reach in
//! here to free anything, and a closed window never prepares again to free its own.

use cosmic::iced::widget::shader::{self, Viewport};
use cosmic::iced::wgpu;
use cosmic::iced::window;
use cosmic::iced::{Rectangle, mouse};
use std::collections::HashMap;
use std::sync::Arc;

use crate::app::PixelFrame;

/// The max number of dim KNOCKOUT rects the shader's uniform array holds (DRAGON-329). A scene
/// with more than this many spotlight/box/highlight/box-highlight rects logs once and renders
/// only the first `MAX_KNOCKOUTS` (the CPU bake has no cap — it is always faithful).
pub const MAX_KNOCKOUTS: usize = 64;

/// A per-pass uniform: the std140 header + rect + color + params (64 bytes) FOLLOWED by the
/// dim block — `dim`, knockout count, pad, and the `MAX_KNOCKOUTS`-long knockout-rect array
/// (DRAGON-329). Every pass binds the whole buffer (WGSL uniform binding size = the full
/// struct), so all uniform buffers are this size even though only the dim pass reads the tail.
/// Layout: 64 (base) + 16 (dim/count/pad) + `MAX_KNOCKOUTS`×16 (rects). Must stay a multiple of
/// 16 and match the WGSL `Uni` struct byte-for-byte.
const UNIFORM_SIZE: u64 = 64 + 16 + (MAX_KNOCKOUTS as u64) * 16;

/// Which region effect an item draws (the always-on-top box/arrow vectors are NOT here — the
/// [`crate::widgets::annotation_canvas`] draws those).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FxEffect {
    Highlight,
    Pixelate,
    Blur,
}

/// One region effect to render, in SOURCE-pixel geometry (scaled into texture space in
/// [`EffectsPipeline::prepare`]). `color` is the highlight tint (straight, 0..1); unused by
/// the redactions. `pixelate_block` is the CONTENT-AWARE mosaic cell size (SOURCE px) the app
/// computed via `annotate::content_pixelate_block` — the SAME value the CPU bake uses, so the
/// mosaic granularity is WYSIWYG; it is `0.0` (ignored) for non-pixelate effects.
#[derive(Clone, Copy, Debug)]
pub struct FxItem {
    pub rect: [f32; 4],
    pub effect: FxEffect,
    pub color: [f32; 3],
    pub pixelate_block: f32,
}

// ── little-endian uniform packing (no bytemuck dependency, matching layers.rs style) ──────

fn push_f32(buf: &mut Vec<u8>, v: f32) {
    buf.extend_from_slice(&v.to_ne_bytes());
}
fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_ne_bytes());
}

/// The texture format matching iced's image atlas (sRGB decode only when the target is sRGB),
/// so the base samples the same values the image widget shows (see `layers.rs`).
fn tex_format(target: wgpu::TextureFormat) -> wgpu::TextureFormat {
    if target.is_srgb() {
        wgpu::TextureFormat::Rgba8UnormSrgb
    } else {
        wgpu::TextureFormat::Rgba8Unorm
    }
}

// ── the shader Program / Primitive ────────────────────────────────────────────────────────

/// The SOURCE-px block sizes + highlight weight the shader mirrors from the CPU bake. Passed
/// in from the app side (which owns the constants in `annotate.rs`) so the single source of
/// truth stays there — the widget is a pure renderer.
#[derive(Clone, Copy, Debug)]
pub struct FxConst {
    /// The blur / highlight-low-pass window size (SOURCE px) — `annotate::BLUR_BLOCK`.
    pub blur_block: f32,
    /// The highlight multiply weight (0..1) — `annotate::HIGHLIGHT_ALPHA` / 255.
    pub highlight_weight: f32,
    /// How many stacked box-blur passes the standalone Blur effect applies (≈ Gaussian) —
    /// `annotate::BLUR_PASSES`. The highlight low-pass always uses a single pass.
    pub blur_passes: u32,
}

/// The `shader::Program` placed in the view: the base pixels + the region effects in z-order,
/// plus the source + fitted-display dims and the shared corner curve.
pub struct EffectsFx {
    /// The preview window this shader belongs to — the key for all of its GPU state (the
    /// pipeline is process-wide; see the module doc).
    window: window::Id,
    /// Every preview window currently OPEN — see [`EffectsPrimitive::live`].
    live: Vec<window::Id>,
    base: Arc<PixelFrame>,
    items: Vec<FxItem>,
    /// Source pixel dims (the effect geometry's coordinate space).
    source: (f32, f32),
    /// The fitted on-screen picture size (LOGICAL points), sized to `dw`×`dh` in the view.
    display: (f32, f32),
    /// The shared corner curve radius (SOURCE px).
    curve_radius: f32,
    /// The bake-mirroring block sizes + highlight weight.
    consts: FxConst,
    /// The global dim amount (0..1) — DRAGON-329. `0` disables the dim pass entirely.
    dim: f32,
    /// The dim KNOCKOUT rects in SOURCE-pixel geometry (spotlight / box / highlight /
    /// box-highlight), capped to [`MAX_KNOCKOUTS`] by the caller.
    knockouts: Vec<[f32; 4]>,
}

impl EffectsFx {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        window: window::Id,
        live: Vec<window::Id>,
        base: Arc<PixelFrame>,
        items: Vec<FxItem>,
        source: (f32, f32),
        display: (f32, f32),
        curve_radius: f32,
        consts: FxConst,
        dim: f32,
        knockouts: Vec<[f32; 4]>,
    ) -> Self {
        Self { window, live, base, items, source, display, curve_radius, consts, dim, knockouts }
    }
}

impl<Message> shader::Program<Message> for EffectsFx {
    type State = ();
    type Primitive = EffectsPrimitive;
    fn draw(&self, _s: &(), _c: mouse::Cursor, _b: Rectangle) -> EffectsPrimitive {
        EffectsPrimitive {
            window: self.window,
            live: self.live.clone(),
            base: self.base.clone(),
            items: self.items.clone(),
            source: self.source,
            display: self.display,
            curve_radius: self.curve_radius,
            consts: self.consts,
            dim: self.dim,
            knockouts: self.knockouts.clone(),
        }
    }
}

pub struct EffectsPrimitive {
    /// The owning preview window — selects this primitive's [`WindowFx`] state.
    window: window::Id,
    /// Every preview window OPEN at the moment this primitive's view was built — the input
    /// to the closed-window eviction in [`EffectsPipeline::prepare`].
    ///
    /// It has to travel on the primitive because iced's `primitive::Storage` exposes no
    /// external handle: app code cannot reach into the pipeline to drop a closed preview's
    /// base/ping-pong textures and uniform pool, so the only way in is a primitive that is
    /// being prepared. It is the set of OPEN previews, NOT of drawn ones — which is exactly
    /// what stops a just-opened preview (open, not yet prepared) from being evicted by
    /// another window's prepare.
    live: Vec<window::Id>,
    base: Arc<PixelFrame>,
    items: Vec<FxItem>,
    source: (f32, f32),
    /// The fitted on-screen size — retained for context; the accumulator now sizes to `source`.
    #[allow(dead_code)]
    display: (f32, f32),
    curve_radius: f32,
    consts: FxConst,
    dim: f32,
    knockouts: Vec<[f32; 4]>,
}

impl std::fmt::Debug for EffectsPrimitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectsPrimitive").field("items", &self.items.len()).finish()
    }
}

impl shader::Primitive for EffectsPrimitive {
    type Pipeline = EffectsPipeline;

    fn prepare(
        &self,
        pipeline: &mut EffectsPipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        pipeline.prepare(device, queue, self, bounds, viewport);
    }

    // The offscreen ping-pong passes + the final blit need the CommandEncoder, so opt into the
    // `render` variant (returning false here).
    fn draw(&self, _pipeline: &EffectsPipeline, _pass: &mut wgpu::RenderPass<'_>) -> bool {
        false
    }

    fn render(
        &self,
        pipeline: &EffectsPipeline,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clip_bounds: &Rectangle<u32>,
    ) {
        // Replays only THIS window's plan — never another preview's (see the module doc).
        pipeline.render(self.window, encoder, target, clip_bounds);
    }
}

// ── GPU state ─────────────────────────────────────────────────────────────────────────────

/// Where a planned pass writes.
enum PassTarget {
    /// An intermediate ping-pong texture (offscreen pipeline, full-texture viewport).
    Ping(usize),
    /// The surface — the final straight-alpha blit over the base (final pipeline, positioned
    /// to the primitive bounds + scissored to the clip rect).
    Surface,
}

/// One planned pass: the bind group (uniform + source texture) and its target.
struct PassPlan {
    bind_group: wgpu::BindGroup,
    target: PassTarget,
}

/// The base texture + its bookkeeping (re-uploaded only when the frame changes).
struct BaseTex {
    view: wgpu::TextureView,
    seq: u64,
    dims: (u32, u32),
}

/// ONE preview window's effect state. Every field here is per-window because the pipeline
/// owning it is process-wide (module doc): the base texture and the ping-pong accumulators
/// are sized to THAT preview's source, and the plan + blit bounds describe THAT window's
/// current frame. Starts empty (nothing allocated until the window first draws effects).
#[derive(Default)]
struct WindowFx {
    base: Option<BaseTex>,
    ping: Vec<wgpu::Texture>,
    ping_views: Vec<wgpu::TextureView>,
    ping_dims: (u32, u32),
    /// Reusable uniform buffers, grown as the effect count grows.
    uniforms: Vec<wgpu::Buffer>,
    /// This frame's plan (seed → per-effect → final), or empty when there's nothing to draw.
    plan: Vec<PassPlan>,
    /// The primitive's on-screen physical bounds (the final blit viewport).
    phys_bounds: Rectangle,
}

/// Persistent GPU state across frames: the DEVICE-level shared bits (the two pipelines —
/// offscreen replace, final alpha blend — the bind group layout, the sampler, the texture
/// format), plus one [`WindowFx`] per preview window holding everything that is per-preview
/// (base texture, ping-pong accumulators, uniform pool, pass plan, blit bounds). iced keys
/// this whole struct by primitive TYPE only and shares it across every window's renderer, so
/// the `windows` map is what keeps two previews from trampling each other.
pub struct EffectsPipeline {
    pipeline_offscreen: wgpu::RenderPipeline,
    pipeline_final: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    tex_format: wgpu::TextureFormat,
    windows: HashMap<window::Id, WindowFx>,
}

/// The eviction predicate for [`EffectsPipeline::windows`], split out as pure logic so it
/// is unit-testable without a GPU: does `window`'s [`WindowFx`] survive a prepare issued by
/// `drawing`, given that `live` are the preview windows currently OPEN?
///
/// An entry is dropped ONLY when its window is definitely gone. Three things keep it:
/// * it is the window doing the drawing (trivially alive, whatever `live` claims);
/// * it is in `live` — and `live` is the set of OPEN previews, not of DRAWN ones, which is
///   precisely what protects a preview that has opened but not yet been prepared from being
///   wiped by another window's prepare;
/// * `live` is EMPTY, i.e. unknown. An absent set must never be read as "everything is
///   closed", so it disables eviction rather than clearing the map.
fn window_fx_survives(window: window::Id, drawing: window::Id, live: &[window::Id]) -> bool {
    window == drawing || live.is_empty() || live.contains(&window)
}

impl shader::Pipeline for EffectsPipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cck-fx-effects-shader"),
            source: wgpu::ShaderSource::Wgsl(WGSL.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cck-fx-effects-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cck-fx-effects-pl"),
            bind_group_layouts: &[&bgl],
            immediate_size: 0,
        });
        // The intermediate ping-pong textures are plain linear Unorm (raw encoded values,
        // matching the CPU's byte-space math); the final blit targets the surface format.
        let pipeline_offscreen = make_pipeline(
            device,
            &shader,
            &layout,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::BlendState::REPLACE,
        );
        let pipeline_final =
            make_pipeline(device, &shader, &layout, format, wgpu::BlendState::ALPHA_BLENDING);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("cck-fx-effects-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline_offscreen,
            pipeline_final,
            bgl,
            sampler,
            tex_format: tex_format(format),
            windows: HashMap::new(),
        }
    }
}

impl WindowFx {
    /// (Re)upload the base texture when its frame changed (dims change forces a new texture).
    fn ensure_base(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &PixelFrame,
        tex_format: wgpu::TextureFormat,
    ) {
        let (w, h) = (frame.w.max(1), frame.h.max(1));
        let fresh = match &self.base {
            Some(b) => b.dims != (w, h),
            None => true,
        };
        if fresh {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("cck-fx-base"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: tex_format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &frame.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * w),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            self.base = Some(BaseTex { view, seq: frame.seq(), dims: (w, h) });
            return;
        }
        // Same dims but a new frame (rare — the base is stable during editing): re-upload.
        let base = self.base.as_mut().expect("checked Some above");
        if base.seq != frame.seq() {
            // Recreate the texture to re-upload (dims unchanged); cheap and keeps the path simple.
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("cck-fx-base"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: tex_format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &frame.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * w),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            *base = BaseTex { view, seq: frame.seq(), dims: (w, h) };
        }
    }

    /// (Re)create the two ping-pong offscreen textures when the target resolution changed.
    fn ensure_ping(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        if self.ping_dims == (w, h) && self.ping.len() == 2 {
            return;
        }
        self.ping.clear();
        self.ping_views.clear();
        for i in 0..2 {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(if i == 0 { "cck-fx-ping-0" } else { "cck-fx-ping-1" }),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            self.ping_views.push(texture.create_view(&wgpu::TextureViewDescriptor::default()));
            self.ping.push(texture);
        }
        self.ping_dims = (w, h);
    }

    /// Ensure `n` uniform buffers exist (reused across frames), then return them.
    fn ensure_uniforms(&mut self, device: &wgpu::Device, n: usize) {
        while self.uniforms.len() < n {
            self.uniforms.push(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cck-fx-ubo"),
                size: UNIFORM_SIZE,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
    }
}

impl EffectsPipeline {
    /// Build this frame's pass plan + write every uniform, in the OWNING WINDOW's state. All
    /// GPU allocation happens here (`render` gets no device), so `prepare` creates the
    /// textures/buffers/bind-groups and records the plan the encoder replays. Nothing outside
    /// `prim.window`'s [`WindowFx`] (plus the shared device-level pipeline/sampler/layout) is
    /// read or written, so a concurrent preview's state cannot be disturbed.
    fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        prim: &EffectsPrimitive,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        // Reclaim CLOSED previews' state first (base texture + both accumulators + the
        // uniform pool + the stale plan): nothing else can, since a closed window never
        // prepares again and app code cannot reach into iced's pipeline storage. Runs on
        // EVERY prepare — it is a handful of `window::Id` comparisons over a map with one
        // entry per open preview, and doing it here means the VRAM goes back the first time
        // any surviving preview redraws.
        self.windows.retain(|w, _| window_fx_survives(*w, prim.window, &prim.live));
        // Disjoint field borrows: the shared device-level bits alongside this window's state.
        let (bgl, sampler, tex_format) = (&self.bgl, &self.sampler, self.tex_format);
        let wfx = self.windows.entry(prim.window).or_default();
        wfx.plan.clear();
        let effects = prim.items.len();
        // DRAGON-329: the shader also runs for a global dim (even with no region effects — a
        // dim-only or spotlight-only scene). Nothing to draw only when BOTH are absent, keeping
        // the no-dim / no-effect path byte-identical (prepare returns, render draws nothing).
        let dim_on = prim.dim > 0.0;
        if effects == 0 && !dim_on {
            return;
        }
        let sf = viewport.scale_factor() as f32;
        // The offscreen accumulator lives at SOURCE resolution, so the seed (base → accumulator) is
        // a LOSSLESS 1:1 copy rather than a resample. Effects then compose in full-res source space
        // (stacked effects sample the accumulated result below — z-order preserved), and only the
        // ONE final blit samples the accumulator → surface — the same single hop the base image
        // widget makes. This is what stops the effect regions looking softer than the base (the old
        // display-res accumulator added a second base resample: source → accumulator → surface).
        // Capped only to bound VRAM on absurd (>8K) captures; normal captures render 1:1.
        const FX_ACCUM_MAX: f32 = 8192.0;
        let (sw, sh) = (prim.source.0.max(1.0), prim.source.1.max(1.0));
        let k = (FX_ACCUM_MAX / sw.max(sh)).min(1.0);
        let tex_w = (sw * k).round().max(1.0) as u32;
        let tex_h = (sh * k).round().max(1.0) as u32;
        // Snap the surface viewport to the physical pixel grid, expanding to the far edge (floor the
        // origin, CEIL the right/bottom) so the dim overlay + effect blit fully COVER the base image.
        // A raw `bounds * sf` can land a fractional pixel short on the right/bottom, leaving a ~1px
        // undimmed seam there (the base rasterizes to whole pixels, our float viewport didn't reach
        // them). floor/ceil guarantees coverage; the clip scissor still bounds it to the content view.
        let px = (bounds.x * sf).floor();
        let py = (bounds.y * sf).floor();
        let pr = ((bounds.x + bounds.width) * sf).ceil();
        let pb = ((bounds.y + bounds.height) * sf).ceil();
        wfx.phys_bounds = Rectangle { x: px, y: py, width: pr - px, height: pb - py };

        wfx.ensure_base(device, queue, &prim.base, tex_format);
        wfx.ensure_ping(device, tex_w, tex_h);
        // seed + (optional dim) + one per effect + final blit.
        wfx.ensure_uniforms(device, effects + 2 + dim_on as usize);

        // Texture-space scale: SOURCE px → offscreen px (aspect-preserving fit ⇒ isotropic).
        let s = tex_w as f32 / prim.source.0.max(1.0);
        let blur_block_tex = (prim.consts.blur_block * s).round().max(1.0);

        // Uniform writers -----------------------------------------------------------------
        // Kinds: 0 seed, 1 final, 2 highlight, 3 pixelate, 4 blur. `radius` = this effect's OWN corner
        // radius (0 for pixelate → square edges); `ko_radius` = the scene curve used for the KNOCKOUT
        // rounded-rects that pixelate/blur read to skip the dim inside a spotlight (params.z). The
        // knockout tail (count + rects, SOURCE px → tex px ×s) rides along so those effects can test it.
        let write = |queue: &wgpu::Queue, buf: &wgpu::Buffer, kind: u32, block: u32,
                     rect: [f32; 4], color: [f32; 4], radius: f32, blur_block: f32, dim: f32,
                     ko_radius: f32| {
            let n = prim.knockouts.len().min(MAX_KNOCKOUTS);
            let mut b = Vec::with_capacity(80 + n * 16);
            push_f32(&mut b, tex_w as f32);
            push_f32(&mut b, tex_h as f32);
            push_u32(&mut b, kind);
            push_u32(&mut b, block);
            for v in rect {
                push_f32(&mut b, v);
            }
            for v in color {
                push_f32(&mut b, v);
            }
            push_f32(&mut b, radius); // params.x = own corner radius (0 = pixelate → square)
            push_f32(&mut b, blur_block); // params.y
            push_f32(&mut b, ko_radius); // params.z = knockout corner radius (scene curve, tex px)
            push_f32(&mut b, 0.0); // params.w
            push_f32(&mut b, dim); // Uni.dim — pixelate/blur darken their output by this (others 0)
            push_u32(&mut b, n as u32); // ko_count — pixelate/blur un-dim inside these knockouts
            push_f32(&mut b, 0.0); // ko_pad.x
            push_f32(&mut b, 0.0); // ko_pad.y
            for r in prim.knockouts.iter().take(n) {
                for v in r {
                    push_f32(&mut b, v * s); // SOURCE px → tex px (isotropic accumulator fit)
                }
            }
            queue.write_buffer(buf, 0, &b);
        };

        // A bind group over (uniform, source texture, sampler).
        let mk_bg = |device: &wgpu::Device, bgl: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler,
                     buf: &wgpu::Buffer, tex: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("cck-fx-effects-bg"),
                layout: bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(tex),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            })
        };
        // The dim-pass uniform writer (DRAGON-329): the base 64-byte header (kind 5, params.x =
        // corner radius in tex px) FOLLOWED by the dim block — `dim`, knockout count, pad, then
        // the knockout rects (in tex space). Only the first `ko_count` rects are written / read.
        let write_dim = |queue: &wgpu::Queue, buf: &wgpu::Buffer, kind: u32, dim: f32, radius: f32,
                         rects: &[[f32; 4]]| {
            let n = rects.len().min(MAX_KNOCKOUTS);
            let mut b = Vec::with_capacity(80 + n * 16);
            push_f32(&mut b, tex_w as f32);
            push_f32(&mut b, tex_h as f32);
            push_u32(&mut b, kind); // 5 = in-stack dim (darkens accumulator), 6 = dim overlay
            push_u32(&mut b, 0);
            for _ in 0..4 {
                push_f32(&mut b, 0.0); // rect (unused)
            }
            for _ in 0..4 {
                push_f32(&mut b, 0.0); // color (unused)
            }
            push_f32(&mut b, radius); // params.x = corner radius (tex px)
            push_f32(&mut b, 0.0); // params.y (blur block, unused)
            push_f32(&mut b, 0.0);
            push_f32(&mut b, 0.0);
            push_f32(&mut b, dim); // dim amount (0..1)
            push_u32(&mut b, n as u32); // knockout count
            push_f32(&mut b, 0.0); // pad
            push_f32(&mut b, 0.0);
            for r in &rects[..n] {
                for v in [r[0] * s, r[1] * s, r[2] * s, r[3] * s] {
                    push_f32(&mut b, v);
                }
            }
            queue.write_buffer(buf, 0, &b);
        };
        let base_view = &wfx.base.as_ref().expect("base uploaded above").view;

        // DIM-ONLY fast path (the common spotlight case): with NO destructive effect, the dim is
        // just a darkening. Skip the seed/accumulator entirely and paint a translucent-black
        // overlay (with knockout holes) DIRECTLY over the crisp base on the surface — the base is
        // never resampled, so it stays as sharp as `widget::image`, only darkened. Matches the
        // bake's `apply_dim` (base × (1 − dim) outside knockouts).
        if effects == 0 && dim_on {
            let ubo = &wfx.uniforms[0];
            write_dim(queue, ubo, 6, prim.dim, prim.curve_radius * s, &prim.knockouts);
            let bg = mk_bg(device, bgl, sampler, ubo, base_view);
            wfx.plan.push(PassPlan { bind_group: bg, target: PassTarget::Surface });
            return;
        }

        // The running uniform index. The dim (when present) is now an OVERLAY over the crisp base
        // (a surface pass), NOT baked into the accumulator — so the base stays sharp everywhere the
        // effects don't actually cover. Knockouts keep full brightness. This must come BEFORE the
        // effect final blit so the effects composite on top of the dimmed base.
        let mut next_u = 0usize;
        if dim_on {
            let ubo = &wfx.uniforms[next_u];
            write_dim(queue, ubo, 6, prim.dim, prim.curve_radius * s, &prim.knockouts);
            let bg = mk_bg(device, bgl, sampler, ubo, base_view);
            wfx.plan.push(PassPlan { bind_group: bg, target: PassTarget::Surface });
            next_u += 1;
        }

        // seed: base → ping[0] WITH the knockout-aware dim baked in (coverage 0). Effects then read
        // already-dimmed content and don't self-dim. dim==0 ⇒ an undimmed 1:1 copy (byte-identical).
        write(
            queue, &wfx.uniforms[next_u], 0, 0, [0.0; 4], [0.0; 4], 0.0, 0.0, prim.dim,
            prim.curve_radius * s,
        );
        let seed_bg = mk_bg(device, bgl, sampler, &wfx.uniforms[next_u], base_view);
        wfx.plan.push(PassPlan { bind_group: seed_bg, target: PassTarget::Ping(0) });
        next_u += 1;
        let mut cur = 0usize;

        // per effect item, in z-order — read ping[cur], write ping[1-cur]. Pixelate/blur re-apply
        // the dim to their OWN output (they aren't knockouts, so they stay dimmed) — matching the
        // bake, since block_mean(content)×(1−dim) == block_mean(content×(1−dim)).
        for item in prim.items.iter() {
            let (kind, block) = match item.effect {
                FxEffect::Highlight => (2u32, 0u32),
                // The per-item content-aware cell size (SOURCE px) → texture px. Mirrors the CPU
                // bake, which sizes the same block from the same source (WYSIWYG).
                FxEffect::Pixelate => (3u32, (item.pixelate_block * s).round().max(1.0) as u32),
                FxEffect::Blur => (4u32, blur_block_tex as u32),
            };
            let rect = [item.rect[0] * s, item.rect[1] * s, item.rect[2] * s, item.rect[3] * s];
            let color = match item.effect {
                FxEffect::Highlight => {
                    [item.color[0], item.color[1], item.color[2], prim.consts.highlight_weight]
                }
                _ => [0.0; 4],
            };
            let ubo = &wfx.uniforms[next_u];
            // Pixelate gets SQUARE edges (own radius 0); every other effect follows the scene curve.
            // The knockout test always uses the scene curve so a redaction un-dims cleanly inside a
            // rounded spotlight regardless of its own corner style.
            let own_radius =
                if matches!(item.effect, FxEffect::Pixelate) { 0.0 } else { prim.curve_radius * s };
            write(
                queue, ubo, kind, block, rect, color, own_radius, blur_block_tex, prim.dim,
                prim.curve_radius * s,
            );
            // Blur STACKS `blur_passes` box-mean passes (≈ Gaussian) for a strong smooth blur;
            // every other effect is a single pass. All passes of one blur share the one uniform
            // (identical params); only the source ping alternates, and `cur` tracks the last
            // write so downstream items + the final blit read the right accumulator.
            let passes =
                if item.effect == FxEffect::Blur { prim.consts.blur_passes.max(1) } else { 1 };
            for _ in 0..passes {
                let src = &wfx.ping_views[cur];
                let dst = 1 - cur;
                let bg = mk_bg(device, bgl, sampler, ubo, src);
                wfx.plan.push(PassPlan { bind_group: bg, target: PassTarget::Ping(dst) });
                cur = dst;
            }
            next_u += 1;
        }

        // final: the last accumulated ping → the surface, over the base + dim overlay. Its alpha
        // is the effects' coverage, so ONLY the effect regions are drawn; the crisp base (dimmed
        // by the overlay outside knockouts) shows through everywhere else.
        let fubo = &wfx.uniforms[next_u];
        write(queue, fubo, 1, 0, [0.0; 4], [0.0; 4], 0.0, 0.0, 0.0, 0.0);
        let final_bg = mk_bg(device, bgl, sampler, fubo, &wfx.ping_views[cur]);
        wfx.plan.push(PassPlan { bind_group: final_bg, target: PassTarget::Surface });
    }

    /// Replay `window`'s planned passes into the encoder. Intermediate passes render into
    /// full offscreen textures; the final blit is positioned to the primitive bounds and
    /// scissored to the clip rect (the ZoomPan content viewport). Reads ONLY that window's
    /// state, so it is unaffected by any other preview's `prepare` running in between.
    fn render(
        &self,
        window: window::Id,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clip_bounds: &Rectangle<u32>,
    ) {
        // No entry yet ⇒ this window has never prepared anything to draw.
        let Some(wfx) = self.windows.get(&window) else {
            return;
        };
        if wfx.plan.is_empty() || clip_bounds.width == 0 || clip_bounds.height == 0 {
            return;
        }
        for pass in &wfx.plan {
            match pass.target {
                PassTarget::Ping(i) => {
                    let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("cck-fx-offscreen"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &wfx.ping_views[i],
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    rp.set_pipeline(&self.pipeline_offscreen);
                    rp.set_bind_group(0, &pass.bind_group, &[]);
                    rp.draw(0..6, 0..1);
                }
                PassTarget::Surface => {
                    let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("cck-fx-final"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: target,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                // LOAD: the surface already holds the base image drawn beneath.
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    rp.set_pipeline(&self.pipeline_final);
                    // Position the effect quad exactly over the base image (post zoom/pan) and
                    // scissor to the clip rect — mirroring iced's own `draw`-variant viewport.
                    rp.set_viewport(
                        wfx.phys_bounds.x,
                        wfx.phys_bounds.y,
                        wfx.phys_bounds.width,
                        wfx.phys_bounds.height,
                        0.0,
                        1.0,
                    );
                    rp.set_scissor_rect(
                        clip_bounds.x,
                        clip_bounds.y,
                        clip_bounds.width,
                        clip_bounds.height,
                    );
                    rp.set_bind_group(0, &pass.bind_group, &[]);
                    rp.draw(0..6, 0..1);
                }
            }
        }
    }
}

/// A fullscreen-quad pipeline over the shared shader with the given target format + blend.
fn make_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
    blend: wgpu::BlendState,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("cck-fx-effects-pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState { count: 1, mask: !0, alpha_to_coverage_enabled: false },
        multiview_mask: None,
        cache: None,
    })
}

/// The whole shader: a fullscreen-quad vertex stage, a rounded-rect SDF, in-shader block/box
/// means over the accumulated texture, and one fragment entry that branches on the pass kind
/// (seed / final / highlight / pixelate / blur). `uv` is 0..1 with (0,0) at the top-left
/// (source y-down); the picture is flipped into the framebuffer the same way iced's raster is.
const WGSL: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VsOut {
    var positions = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0)
    );
    var uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 0.0),
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(1.0, 0.0)
    );
    var out: VsOut;
    out.pos = vec4<f32>(positions[idx], 0.0, 1.0);
    out.uv = uvs[idx];
    return out;
}

struct Uni {
    tex_dims: vec2<f32>,
    kind: u32,
    block: u32,
    rect: vec4<f32>,
    color: vec4<f32>,
    params: vec4<f32>, // x = corner radius (tex px), y = blur block (tex px)
    // Dim pass (kind 5, DRAGON-329): the global dim + its knockout rects (tex space). The array
    // length MUST equal MAX_KNOCKOUTS in annotation_fx.rs (locked by the naga test).
    dim: f32,
    ko_count: u32,
    ko_pad: vec2<f32>,
    ko_rects: array<vec4<f32>, 64>,
};
@group(0) @binding(0) var<uniform> U: Uni;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

// Signed distance to a rounded rect `rect`=(x,y,w,h) (tex px), corner radius `r` (clamped to
// half the smaller side). Negative inside. Same geometry as the bake's rounded-rect mask.
fn sdf_round_rect(p: vec2<f32>, rect: vec4<f32>, r: f32) -> f32 {
    let half = rect.zw * 0.5;
    let c = rect.xy + half;
    let rr = min(r, min(half.x, half.y));
    let q = abs(p - c) - (half - vec2<f32>(rr));
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - rr;
}

// Screen-space anti-aliased coverage of the SDF (~1 tex-px edge via fwidth).
fn sdf_coverage(d: f32) -> f32 {
    let w = max(fwidth(d), 1e-4);
    return clamp(0.5 - d / w, 0.0, 1.0);
}

// GRID-ALIGNED block mean (pixelate): every texel of a `block`×`block` cell (aligned to the
// texture origin, like the bake's `block_means`) collapses to the cell average — a crisp mosaic.
// sRGB transfer functions. The pixelate block mean averages in sRGB (gamma) space to match the CPU
// bake, which averages the sRGB bytes: averaging in LINEAR light (the shader's native space, since
// the sRGB texture auto-decodes) over-weights bright pixels and washes the darks out of a mosaic of
// e.g. dark text on white; sRGB averaging preserves the high/low luminance the way the save does.
fn lin2srgb(c: vec3<f32>) -> vec3<f32> {
    let m = max(c, vec3<f32>(0.0));
    return select(1.055 * pow(m, vec3<f32>(1.0 / 2.4)) - 0.055, m * 12.92, m <= vec3<f32>(0.0031308));
}
fn srgb2lin(c: vec3<f32>) -> vec3<f32> {
    let m = max(c, vec3<f32>(0.0));
    return select(pow((m + 0.055) / 1.055, vec3<f32>(2.4)), m / 12.92, m <= vec3<f32>(0.04045));
}

fn block_mean_snapped(p: vec2<f32>, block: i32) -> vec3<f32> {
    let dims = vec2<i32>(i32(U.tex_dims.x), i32(U.tex_dims.y));
    let b = max(block, 1);
    let ip = vec2<i32>(i32(floor(p.x)), i32(floor(p.y)));
    let x0 = (ip.x / b) * b;
    let y0 = (ip.y / b) * b;
    let x1 = min(x0 + b, dims.x);
    let y1 = min(y0 + b, dims.y);
    var sum = vec3<f32>(0.0);
    var n = 0.0;
    for (var yy = y0; yy < y1; yy = yy + 1) {
        for (var xx = x0; xx < x1; xx = xx + 1) {
            sum = sum + lin2srgb(textureLoad(src, vec2<i32>(xx, yy), 0).rgb);
            n = n + 1.0;
        }
    }
    return srgb2lin(sum / max(n, 1.0)); // sRGB-space mean, back to linear for the sRGB target
}

// CENTERED box mean (blur / highlight low-pass): a smooth sliding-window average of size
// `block`, clamped to the image (out-of-bounds texels skipped, like the bake's partial blocks).
fn box_mean_centered(p: vec2<f32>, block: i32) -> vec3<f32> {
    let dims = vec2<i32>(i32(U.tex_dims.x), i32(U.tex_dims.y));
    let b = max(block, 1);
    let r = b / 2;
    let ip = vec2<i32>(i32(floor(p.x)), i32(floor(p.y)));
    var sum = vec3<f32>(0.0);
    var n = 0.0;
    for (var dy = -r; dy < b - r; dy = dy + 1) {
        let yy = ip.y + dy;
        if (yy < 0 || yy >= dims.y) { continue; }
        for (var dx = -r; dx < b - r; dx = dx + 1) {
            let xx = ip.x + dx;
            if (xx < 0 || xx >= dims.x) { continue; }
            sum = sum + textureLoad(src, vec2<i32>(xx, yy), 0).rgb;
            n = n + 1.0;
        }
    }
    return sum / max(n, 1.0);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // seed: downscale the base into the accumulator AND apply the knockout-aware dim, so every effect
    // reads ALREADY-DIMMED content (matching the bake, which dims the base before drawing effects).
    // Dimming here — not per-effect — is what fixes: (a) a stacked blur compounding the dim to a black
    // mass (each pass no longer re-darkens), (b) the pixelate's bright edge fringe (the passthrough is
    // now the dimmed base, so the final blit's edge bleed matches the dimmed surface), and (c) a
    // redaction in a spotlight staying bright (ko → 1 ⇒ no dim here). Coverage 0. dim==0 ⇒ base as-is.
    if (U.kind == 0u) {
        let base = textureSampleLevel(src, samp, in.uv, 0.0).rgb;
        let p0 = in.uv * U.tex_dims;
        var ko0 = 0.0;
        for (var i = 0u; i < U.ko_count; i = i + 1u) {
            ko0 = max(ko0, sdf_coverage(sdf_round_rect(p0, U.ko_rects[i], U.params.z)));
        }
        return vec4<f32>(base * (1.0 - U.dim * (1.0 - ko0)), 0.0);
    }
    // final: blit the accumulator (rgb = composite, a = coverage) over the base.
    if (U.kind == 1u) {
        return textureSampleLevel(src, samp, in.uv, 0.0);
    }
    // dim (DRAGON-329): darken the accumulated content toward black by dim×(1−maxKnockoutCov).
    // The knockout coverage is the MAX over every knockout rect (same rounded-rect SDF the
    // effects use, so edges match the bake). alpha → 1 so the final blit owns the whole frame
    // (rgb IS the dimmed composite), and later effects sample the dimmed rgb (a highlight over a
    // knockout still reads bright content, since the knockout kept da = 0 there).
    if (U.kind == 5u) {
        let below = textureSampleLevel(src, samp, in.uv, 0.0);
        let p = in.uv * U.tex_dims;
        var maxcov = 0.0;
        for (var i = 0u; i < U.ko_count; i = i + 1u) {
            let d = sdf_round_rect(p, U.ko_rects[i], U.params.x);
            maxcov = max(maxcov, sdf_coverage(d));
        }
        let da = U.dim * (1.0 - maxcov);
        return vec4<f32>(below.rgb * (1.0 - da), max(below.a, 1.0));
    }
    // dim OVERLAY (kind 6): the dim-only fast path — output translucent BLACK straight over the
    // crisp base (no seed, no resample). Straight-alpha blend `base*(1-da) + 0*da` = base darkened
    // by `da`, exactly the bake's `apply_dim`, with the base kept pixel-crisp.
    if (U.kind == 6u) {
        let p6 = in.uv * U.tex_dims;
        var mc = 0.0;
        for (var i = 0u; i < U.ko_count; i = i + 1u) {
            mc = max(mc, sdf_coverage(sdf_round_rect(p6, U.ko_rects[i], U.params.x)));
        }
        return vec4<f32>(0.0, 0.0, 0.0, U.dim * (1.0 - mc));
    }
    // EFFECTS (kinds 2/3/4) — read the ACCUMULATED content below (so stacked effects compose:
    // a pixelate over a highlight redacts the highlight), apply this effect inside its SDF mask,
    // pass through outside. The accumulator is at SOURCE resolution (the seed is a lossless 1:1
    // copy, not a resample), so this stays as crisp as the base — the single final blit does the
    // one source→surface sample, matching the base image widget.
    let below = textureSampleLevel(src, samp, in.uv, 0.0);
    let p = in.uv * U.tex_dims;
    let d = sdf_round_rect(p, U.rect, U.params.x);
    let cov = sdf_coverage(d);
    if (cov <= 0.0) {
        return below; // passthrough, carrying the accumulated (already-dimmed) coverage
    }
    // The accumulator is ALREADY dimmed (in the seed, knockout-aware), so effects just composite —
    // no self-dim, no per-pass compounding. The AA edge fades to `below` (the dimmed base), matching
    // the dimmed surface and the bake, so there's no bright/black fringe.
    var out_rgb: vec3<f32>;
    if (U.kind == 3u) {
        // pixelate: mix toward the grid-block mean of the (already-dimmed) accumulator.
        let m = block_mean_snapped(p, i32(U.block));
        out_rgb = mix(below.rgb, m, cov);
    } else if (U.kind == 4u) {
        // blur: mix toward the box mean of the (already-dimmed) accumulator — stacking passes just
        // blur, they no longer re-darken it toward black.
        let m = box_mean_centered(p, i32(U.block));
        out_rgb = mix(below.rgb, m, cov);
    } else {
        // highlight: adaptive multiply/screen keyed on the low-pass background luminance.
        let bg = box_mean_centered(p, i32(U.params.y));
        let bl = dot(bg, vec3<f32>(0.2126, 0.7152, 0.0722));
        let w = smoothstep(0.35, 0.65, bl);
        let mult = below.rgb * U.color.rgb;
        let scr = vec3<f32>(1.0) - (vec3<f32>(1.0) - below.rgb) * (vec3<f32>(1.0) - U.color.rgb);
        let blended = mix(scr, mult, w);
        out_rgb = mix(below.rgb, blended, U.color.a * cov);
    }
    // Coverage alpha becomes 1 wherever any effect touched — the AA is carried in `out_rgb`
    // (which fades to the accumulated content as `cov` → 0), so the straight-alpha composite
    // over the base reproduces the bake's edge without a black fringe.
    return vec4<f32>(out_rgb, max(below.a, 1.0));
}
"#;

#[cfg(test)]
mod tests {
    use super::{MAX_KNOCKOUTS, UNIFORM_SIZE, WGSL, window_fx_survives};
    use cosmic::iced::window;

    /// The closed-preview reclaim: with a long-lived multi-document host, closing one
    /// preview must free ITS base/accumulator/uniform state on the next prepare of any
    /// surviving preview — nothing else ever can, since a closed window never prepares again.
    #[test]
    fn a_closed_previews_effect_state_is_evicted_by_another_windows_prepare() {
        let (a, b) = (window::Id::unique(), window::Id::unique());
        assert_ne!(a, b);
        // B closed, A is drawing: B goes, A stays.
        assert!(window_fx_survives(a, a, &[a]));
        assert!(!window_fx_survives(b, a, &[a]), "a closed preview's GPU state must be freed");
    }

    /// The hazard the OPEN set exists to avoid: a preview that has opened but has not been
    /// prepared yet has no entry of its own to defend it, and must not be wiped by another
    /// window's prepare. Being OPEN (not DRAWN) is what protects it.
    #[test]
    fn an_open_but_undrawn_preview_is_never_evicted() {
        let (a, b) = (window::Id::unique(), window::Id::unique());
        assert!(window_fx_survives(b, a, &[a, b]), "an open preview keeps its state");
        // The drawing window survives even a stale set that has forgotten it...
        assert!(window_fx_survives(a, a, &[b]));
        // ...and an EMPTY set means "unknown", never "everything is closed".
        assert!(window_fx_survives(b, a, &[]));
    }

    /// The effect WGSL only compiles on the GPU at RUNTIME, so a bad shader sails through
    /// `cargo build`/`clippy` and aborts the first time an effect is drawn (this exact test was
    /// added after a `struct U` / `var<uniform> U` name collision crashed on the first draw).
    /// Parse + validate it here with the SAME naga front-end wgpu uses — the headless gate.
    /// Extended for the DRAGON-329 dim pass (the `ko_rects` uniform array + `kind == 5` branch).
    #[test]
    fn effect_wgsl_parses_and_validates() {
        let module = naga::front::wgsl::parse_str(WGSL)
            .unwrap_or_else(|e| panic!("effect WGSL parse error:\n{}", e.emit_to_string(WGSL)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("effect WGSL validation error: {e:?}"));
    }

    /// The WGSL `Uni.ko_rects` array length is hard-coded (a string literal can't read the Rust
    /// const), so lock the two together: the knockout cap is 64, and the uniform is sized to
    /// match the std140 layout the dim writer packs (64 base + 16 dim header + cap×16 rects).
    #[test]
    fn dim_uniform_layout_matches_the_shader() {
        assert_eq!(MAX_KNOCKOUTS, 64, "the WGSL `array<vec4<f32>, 64>` must match MAX_KNOCKOUTS");
        assert!(WGSL.contains("array<vec4<f32>, 64>"), "WGSL knockout array size drifted");
        assert_eq!(UNIFORM_SIZE, 64 + 16 + 64 * 16);
        assert_eq!(UNIFORM_SIZE % 16, 0, "uniform buffer must stay 16-byte aligned");
    }
}
