//! A custom wgpu shader primitive that draws a STACK of pixel layers, each owned by a
//! persistent GPU texture slot keyed by [`LayerKey`] — re-uploaded in place each frame.
//! Using `image::Handle` per frame churned iced's texture atlas (allocate + trim every
//! frame), which flickered; every layer instead owns its own texture that's re-uploaded
//! (never reallocated, unless its dimensions change) as long as its key stays alive, so
//! playback and overlay edits are both smooth. Colours match the image widget (the
//! texture is `Rgba8UnormSrgb`, like iced's atlas, unless the target is already linear —
//! see `tex_format` below).
//!
//! One shader widget can show several layers at once (e.g. a playing video frame PLUS a
//! covermark overlay) without them fighting over a texture. The old single-texture shader
//! program keyed its pipeline storage by primitive TYPE, so the video frame and the
//! covermark overlay — both the same shader-primitive type — shared ONE texture: both
//! `prepare`s wrote it and both `draw`s sampled whichever upload happened last — a real
//! defect during playback-with-covermark. Each [`LayerKey`] now owns its own
//! [`TextureSlot`], so that collision is gone, and draw order (the `Vec<Layer>` order) is
//! independent of it.
//!
//! Adding a new editable layer:
//! 1. Add a new [`LayerKey`] const — a stable identity for the layer's texture slot.
//! 2. Produce its pixels off-thread as an `Arc<PixelFrame>`, tracked by a [`RasterSlot`]
//!    (coalesces overlapping refresh requests, drops stale results).
//! 3. Push a `Layer { key, frame }` into the `Vec` passed to `LayerStack::new` in the view
//!    — draw order follows the `Vec`'s order, NOT key order.

use cosmic::iced::widget::shader::{self, Viewport};
use cosmic::iced::wgpu;
use cosmic::iced::{Rectangle, mouse};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic frame id so a slot only re-uploads when its frame actually changed.
static NEXT_SEQ: AtomicU64 = AtomicU64::new(1);

/// One decoded frame's pixels (raw RGBA, `w`×`h`) plus a unique `seq`.
pub struct PixelFrame {
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
    seq: u64,
}

impl PixelFrame {
    /// Wrap decoded RGBA pixels, stamping a fresh sequence id.
    pub fn new(rgba: Vec<u8>, w: u32, h: u32) -> Arc<Self> {
        Arc::new(Self {
            rgba,
            w,
            h,
            seq: NEXT_SEQ.fetch_add(1, Ordering::Relaxed),
        })
    }
}

impl std::fmt::Debug for PixelFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PixelFrame")
            .field("w", &self.w)
            .field("h", &self.h)
            .field("seq", &self.seq)
            .finish()
    }
}

/// A layer's stable IDENTITY — maps to one persistent GPU texture slot that updates in
/// place across frames (the texture itself is only recreated when the layer's pixel
/// dimensions change). Draw order is the `Vec<Layer>` order passed to
/// [`LayerStack::new`], NOT key order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct LayerKey(pub u32);

impl LayerKey {
    /// The playing/scrubbed video frame.
    pub const VIDEO: LayerKey = LayerKey(0);
    /// The covermark overlay raster.
    pub const COVERMARK: LayerKey = LayerKey(1);
}

/// One layer to draw: a stable identity plus the pixels currently on it.
#[derive(Clone, Debug)]
pub struct Layer {
    pub key: LayerKey,
    pub frame: Arc<PixelFrame>,
}

/// The `shader::Program` placed in the view, holding the layers to draw (in order).
pub struct LayerStack {
    layers: Vec<Layer>,
}

impl LayerStack {
    pub fn new(layers: Vec<Layer>) -> Self {
        Self { layers }
    }
}

impl<Message> shader::Program<Message> for LayerStack {
    type State = ();
    type Primitive = LayerStackPrimitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> LayerStackPrimitive {
        // Arc clones are cheap — this runs every view build.
        LayerStackPrimitive { layers: self.layers.clone() }
    }
}

/// The per-frame primitive — the layers to upload + draw, in order.
#[derive(Debug)]
pub struct LayerStackPrimitive {
    layers: Vec<Layer>,
}

impl shader::Primitive for LayerStackPrimitive {
    type Pipeline = LayerStackPipeline;

    fn prepare(
        &self,
        pipeline: &mut LayerStackPipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
        for layer in &self.layers {
            pipeline.upsert(device, queue, layer.key, &layer.frame);
        }
        // At most one LayerStack exists per window surface, so this primitive's key set
        // IS the whole picture for that surface — prune anything else so a layer that
        // stopped being pushed (e.g. the covermark was cleared) doesn't leave its
        // texture parked in VRAM forever.
        let keys: HashSet<LayerKey> = self.layers.iter().map(|l| l.key).collect();
        pipeline.slots.retain(|k, _| keys.contains(k));
    }

    fn draw(&self, pipeline: &LayerStackPipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        pipeline.draw(&self.layers, render_pass)
    }
}

/// One layer's persistent GPU texture + the bind group wrapping it.
struct TextureSlot {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    dims: (u32, u32),
    seq: u64,
}

/// The shared GPU state: one render pipeline + one texture PER LAYER KEY, each
/// re-uploaded in place when its frame changes. Persists across frames in the shader
/// `Storage`, keyed by [`LayerStackPrimitive`]'s type.
pub struct LayerStackPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// The frame texture format, chosen to match iced's image atlas: sRGB only when the
    /// target is sRGB (i.e. gamma correction is on). libcosmic builds with `web-colors`,
    /// so the target is linear `Unorm` and the texture must NOT sRGB-decode (else the
    /// video samples darker than the poster).
    tex_format: wgpu::TextureFormat,
    slots: HashMap<LayerKey, TextureSlot>,
}

impl shader::Pipeline for LayerStackPipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cck-video-shader"),
            source: wgpu::ShaderSource::Wgsl(WGSL.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cck-video-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cck-video-pl"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cck-video-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("cck-video-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        // Match iced's image atlas: sRGB texture only when the target is sRGB.
        let tex_format = if format.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        Self {
            pipeline,
            bind_group_layout,
            sampler,
            tex_format,
            slots: HashMap::new(),
        }
    }
}

impl LayerStackPipeline {
    /// Upsert `key`'s slot: (re)create its texture when missing or its dimensions
    /// changed (forcing a re-upload below), then upload `frame`'s pixels — but skip the
    /// upload when the frame hasn't changed since last time (its `seq` already matches).
    fn upsert(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, key: LayerKey, frame: &PixelFrame) {
        let (w, h) = (frame.w, frame.h);
        if w == 0 || h == 0 {
            return;
        }
        let needs_new = match self.slots.get(&key) {
            Some(slot) => slot.dims != (w, h),
            None => true,
        };
        if needs_new {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("cck-video-texture"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.tex_format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("cck-video-bg"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.slots.insert(key, TextureSlot { texture, bind_group, dims: (w, h), seq: 0 });
        }
        let slot = self.slots.get_mut(&key).expect("just inserted above when missing");
        if slot.seq != frame.seq {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &slot.texture,
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
            slot.seq = frame.seq;
        }
    }

    /// Draw each layer's textured quad, in order, into the shared render pass (already
    /// scissored to bounds). Returns whether anything was actually drawn — `false` only
    /// when NO layer has a slot yet (e.g. every frame so far was 0×0).
    fn draw(&self, layers: &[Layer], pass: &mut wgpu::RenderPass<'_>) -> bool {
        let mut drew_any = false;
        for layer in layers {
            let Some(slot) = self.slots.get(&layer.key) else {
                continue;
            };
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &slot.bind_group, &[]);
            pass.draw(0..6, 0..1);
            drew_any = true;
        }
        drew_any
    }
}

/// Fullscreen-quad vertex shader (the render pass is scissored to the widget's bounds) +
/// a texture-sampling fragment shader.
const WGSL: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

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

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv);
}
"#;

/// The CPU-side producer state a dynamic layer needs to coalesce off-thread refreshes:
/// at most one raster in flight at a time, with overlapping requests collapsed into a
/// single re-run once it lands, and stale results (superseded before they finished)
/// dropped instead of shown. Extracted from the covermark's own `EditState` bookkeeping
/// so future overlay layers (annotations, timeline ghosts, …) get this for free.
#[derive(Default)]
pub struct RasterSlot {
    current: Option<Arc<PixelFrame>>,
    generation: u64,
    refreshing: bool,
    pending: bool,
}

impl RasterSlot {
    /// The current raster, if one has been produced (and wasn't cleared since).
    pub fn frame(&self) -> Option<&Arc<PixelFrame>> {
        self.current.as_ref()
    }

    /// Mark the producing state as changed (a new raster is needed) without touching
    /// what's currently displayed — the caller drives the actual refresh via `begin`.
    pub fn invalidate(&mut self) {
        self.generation += 1;
    }

    /// Invalidate AND drop the current raster immediately (e.g. the layer was turned
    /// off) instead of waiting for a fresh raster to land.
    pub fn clear(&mut self) {
        self.invalidate();
        self.current = None;
    }

    /// Start a refresh for the CURRENT generation: `Some(generation)` means the caller
    /// should spawn a raster stamped with it; `None` means one is already in flight, and
    /// this request has been coalesced into `pending` (exactly one re-run once it
    /// lands, however many callers asked for a refresh while it was busy).
    pub fn begin(&mut self) -> Option<u64> {
        if self.refreshing {
            self.pending = true;
            return None;
        }
        self.refreshing = true;
        Some(self.generation)
    }

    /// A raster stamped `generation` finished. Clears the in-flight flag; the frame is
    /// stored only when `generation` is still current (a stale result — the state moved
    /// on while it was rendering — is dropped instead of flashing an outdated frame).
    /// Returns whether a re-run was requested while this one was in flight — the caller
    /// should `begin()` again when it does.
    pub fn finish(&mut self, generation: u64, frame: Option<Arc<PixelFrame>>) -> bool {
        self.refreshing = false;
        if generation == self.generation {
            self.current = frame;
        }
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_frame() -> Arc<PixelFrame> {
        PixelFrame::new(vec![0, 0, 0, 0], 1, 1)
    }

    #[test]
    fn begin_returns_none_and_finish_reruns_exactly_once() {
        let mut slot = RasterSlot::default();
        let generation = slot.begin().expect("first begin starts a refresh");
        assert!(slot.begin().is_none(), "a second begin while refreshing must coalesce");
        assert!(
            slot.finish(generation, Some(dummy_frame())),
            "the coalesced request must trigger exactly one re-run"
        );
        // Nothing else asked for a refresh in the meantime, so the re-run's own finish
        // must NOT ask for yet another one.
        let generation2 = slot.begin().expect("re-begins after being told to");
        assert!(!slot.finish(generation2, Some(dummy_frame())));
    }

    #[test]
    fn stale_generation_is_dropped() {
        let mut slot = RasterSlot::default();
        let generation = slot.begin().expect("starts a refresh");
        slot.invalidate(); // generation moves on while the raster is in flight
        assert!(!slot.finish(generation, Some(dummy_frame())), "no coalesced request was made");
        assert!(slot.frame().is_none(), "a stale-generation raster must be dropped");
    }

    #[test]
    fn clear_drops_the_current_frame_and_bumps_generation() {
        let mut slot = RasterSlot::default();
        let generation = slot.begin().unwrap();
        assert!(!slot.finish(generation, Some(dummy_frame())));
        assert!(slot.frame().is_some(), "finish with a matching generation must store the frame");
        slot.clear();
        assert!(slot.frame().is_none(), "clear must drop the current frame");
        // A raster for the pre-clear generation landing afterward must be dropped.
        assert!(!slot.finish(generation, Some(dummy_frame())));
        assert!(slot.frame().is_none());
    }

    #[test]
    fn begin_when_idle_always_returns_the_current_generation() {
        let mut slot = RasterSlot::default();
        assert_eq!(slot.begin(), Some(0));
        assert!(!slot.finish(0, None));
        slot.invalidate();
        assert_eq!(slot.begin(), Some(1));
    }

    #[test]
    fn clear_while_a_refresh_is_in_flight_still_reruns_for_a_coalesced_request() {
        // `clear` doesn't touch `refreshing`/`pending` — only `generation`/`current` — so
        // calling it mid-flight (e.g. the layer was turned off while its raster was still
        // being produced) must still coalesce a subsequent request and rerun once the
        // stale (pre-clear) raster lands and is dropped.
        let mut slot = RasterSlot::default();
        let generation = slot.begin().expect("starts a refresh");
        slot.clear(); // turned off mid-flight: generation bumps, current stays cleared
        assert!(
            slot.begin().is_none(),
            "still refreshing — the pre-clear raster hasn't landed yet"
        );
        assert!(
            slot.finish(generation, Some(dummy_frame())),
            "the coalesced request made during clear must trigger exactly one re-run"
        );
        assert!(slot.frame().is_none(), "the stale pre-clear raster must not be shown");
    }
}
