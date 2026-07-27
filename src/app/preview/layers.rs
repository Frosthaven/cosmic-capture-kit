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
//! # Why a [`LayerKey`] carries its WINDOW
//! iced's shader `Pipeline` storage is keyed by the primitive's `TypeId` ALONE, and the
//! `Engine` holding it is cloned (an `Arc`) into every window's renderer — so exactly ONE
//! [`LayerStackPipeline`], and one `slots` map, exists per PROCESS no matter how many
//! windows draw a `LayerStack`. A plain per-layer id would therefore make two preview
//! windows share texture slots (each overwriting the other's pixels) AND let one window's
//! prune delete the other's slots every frame. The identity is consequently
//! (`window::Id`, [`LayerSlot`]): the slot constant says WHICH layer, the window says WHOSE.
//! `window::Id` is a process-unique counter (`Copy + Eq + Hash`), so it is used directly —
//! no hashing, no collisions.
//!
//! The remaining per-SURFACE invariant: at most ONE `LayerStack` may be mounted in a given
//! window's view, because the prune below treats a primitive's key set as the whole truth
//! FOR THE WINDOWS IT DREW. Two `LayerStack`s in one window would still take turns deleting
//! each other's slots (which is why the Windows overlay folds base + covermark into a single
//! stack — see `image.rs`/`video.rs`). Different windows are now independent.
//!
//! Adding a new editable layer:
//! 1. Add a new [`LayerSlot`] const — the stable WITHIN-window identity of its texture slot.
//! 2. Produce its pixels off-thread as an `Arc<PixelFrame>`, tracked by a [`RasterSlot`]
//!    (coalesces overlapping refresh requests, drops stale results).
//! 3. Push a `Layer { key: LayerKey::new(preview.window, SLOT), frame }` into the `Vec`
//!    passed to `LayerStack::new` in the view — draw order follows the `Vec`'s order, NOT
//!    key order.

use cosmic::iced::widget::shader::{self, Viewport};
use cosmic::iced::wgpu;
use cosmic::iced::window;
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

    /// The unique sequence id, so a consumer outside this module (e.g. the `annotation_fx`
    /// effects shader) can skip a GPU re-upload when the frame hasn't changed.
    pub fn seq(&self) -> u64 {
        self.seq
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

/// WHICH layer of a preview a texture slot holds — the stable identity WITHIN one window.
/// Pair it with the owning window (see [`LayerKey`]) before it names a texture slot.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct LayerSlot(pub u32);

impl LayerSlot {
    /// The playing/scrubbed video frame (or, on the Windows overlay, the still/poster base).
    pub const VIDEO: LayerSlot = LayerSlot(0);
    /// The covermark overlay raster. (The region effects — highlight / pixelate / blur — are
    /// NOT a raster layer: they render in real time through the `annotation_fx` GPU shader,
    /// DRAGON-330; box/arrow stay vector geometry drawn by the `AnnotationCanvas`.)
    pub const COVERMARK: LayerSlot = LayerSlot(1);
}

/// A layer's stable IDENTITY — maps to one persistent GPU texture slot that updates in
/// place across frames (the texture itself is only recreated when the layer's pixel
/// dimensions change). It is (owning window, [`LayerSlot`]) because the pipeline holding
/// the slots is ONE per process, shared by every window's renderer — see the module doc.
/// Draw order is the `Vec<Layer>` order passed to [`LayerStack::new`], NOT key order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct LayerKey {
    /// The preview window that owns this slot (`PreviewState::window`).
    window: window::Id,
    /// Which layer of that window.
    slot: LayerSlot,
}

impl LayerKey {
    /// The slot `slot` of window `window`.
    pub fn new(window: window::Id, slot: LayerSlot) -> Self {
        Self { window, slot }
    }

    /// This window's video/base layer.
    pub fn video(window: window::Id) -> Self {
        Self::new(window, LayerSlot::VIDEO)
    }

    /// This window's covermark layer.
    pub fn covermark(window: window::Id) -> Self {
        Self::new(window, LayerSlot::COVERMARK)
    }

    /// The window this slot belongs to — what makes the prune window-scoped.
    pub fn window(self) -> window::Id {
        self.window
    }
}

/// One layer to draw: a stable identity plus the pixels currently on it.
#[derive(Clone, Debug)]
pub struct Layer {
    pub key: LayerKey,
    pub frame: Arc<PixelFrame>,
}

/// The `shader::Program` placed in the view, holding the layers to draw (in order) plus
/// the set of preview windows that are currently OPEN (see [`LayerStackPrimitive::live`]).
pub struct LayerStack {
    layers: Vec<Layer>,
    live: Vec<window::Id>,
}

impl LayerStack {
    /// `live` must be EVERY open preview window (`App::live_preview_windows`), not just the
    /// one drawing — it is what lets this primitive free a CLOSED preview's textures.
    pub fn new(layers: Vec<Layer>, live: Vec<window::Id>) -> Self {
        Self { layers, live }
    }
}

impl<Message> shader::Program<Message> for LayerStack {
    type State = ();
    type Primitive = LayerStackPrimitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> LayerStackPrimitive {
        // Arc clones are cheap — this runs every view build.
        LayerStackPrimitive { layers: self.layers.clone(), live: self.live.clone() }
    }
}

/// The per-frame primitive — the layers to upload + draw, in order, plus the live-window
/// set that drives eviction.
#[derive(Debug)]
pub struct LayerStackPrimitive {
    layers: Vec<Layer>,
    /// Every preview window OPEN at the moment this primitive's view was built.
    ///
    /// Needed because iced's `primitive::Storage` exposes no external handle: app code
    /// cannot reach into the pipeline to free a closed preview's textures, so the only way
    /// in is through a primitive that is being prepared. Carrying the live set lets ANY
    /// preview's prepare reclaim the slots of previews that are gone.
    ///
    /// It is the set of OPEN previews, NOT of drawn ones — that distinction is the whole
    /// safety property: a preview that has just opened and has not yet been prepared is
    /// still open, so it is in here, so another window's prepare cannot wipe it.
    live: Vec<window::Id>,
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
        // Two reclaims in one pass over the process-wide slot map:
        //  * WITHIN a window this primitive drew, anything it stopped pushing (e.g. the
        //    covermark was cleared) is freed — at most one LayerStack exists per window, so
        //    its key set IS the whole picture for that window.
        //  * For any window that is no longer an OPEN preview, everything is freed — the
        //    closed-preview leak, which no prepare OF THAT WINDOW can ever fix because that
        //    window is never drawn again.
        // Anything else — another LIVE window's slots — is untouchable here.
        let present: HashSet<LayerKey> = self.layers.iter().map(|l| l.key).collect();
        let drawn: HashSet<window::Id> = present.iter().map(|k| k.window()).collect();
        let live: HashSet<window::Id> = self.live.iter().copied().collect();
        pipeline.slots.retain(|k, _| slot_survives(*k, &present, &drawn, &live));
    }

    fn draw(&self, pipeline: &LayerStackPipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        pipeline.draw(&self.layers, render_pass)
    }
}

/// The prune predicate, split out as pure logic so it can be unit-tested without a GPU:
/// does the existing texture slot `key` survive a prepare whose layer keys are `present`
/// (belonging to the windows `drawn`), given that `live` are the preview windows currently
/// OPEN?
///
/// Two independent reasons to free a slot, and nothing else may:
/// 1. **Its window is closed** — not in `live`. This is the reclaim for a preview that went
///    away while others stayed open; no prepare of that window will ever run again.
/// 2. **Its window WAS drawn by this primitive and the slot wasn't in it** — the
///    within-window reclaim (a cleared covermark), unchanged from before.
///
/// The safety property that makes (1) sound is that `live` is the set of OPEN previews, not
/// of drawn ones: a preview that has just opened and has not yet been prepared is open, so
/// it is in `live`, so another window's prepare leaves it alone. As a belt for a caller that
/// somehow has no live set at all, an EMPTY `live` disables (1) entirely rather than wiping
/// the process's slots — an unknown set must never be read as "everything is closed".
/// An empty primitive (`present` empty ⇒ `drawn` empty) likewise prunes nothing under (2).
fn slot_survives(
    key: LayerKey,
    present: &HashSet<LayerKey>,
    drawn: &HashSet<window::Id>,
    live: &HashSet<window::Id>,
) -> bool {
    // A window this primitive is DRAWING is trivially alive, whatever `live` claims, so
    // rule (1) can never fire on it (belt against a caller passing a stale set).
    let drawing = drawn.contains(&key.window());
    if !drawing && !live.is_empty() && !live.contains(&key.window()) {
        return false; // (1) the owning preview closed
    }
    // (2) within a window this primitive drew, only what it drew survives.
    !drawing || present.contains(&key)
}

/// One layer's persistent GPU texture + the bind group wrapping it.
struct TextureSlot {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    dims: (u32, u32),
    seq: u64,
}

/// The shared GPU state: one render pipeline + one texture PER LAYER KEY (window + slot),
/// each re-uploaded in place when its frame changes. Persists across frames in the shader
/// `Storage`, keyed by [`LayerStackPrimitive`]'s TYPE — and iced's `Engine` (hence that
/// storage) is cloned into every window's renderer, so this struct is process-wide and
/// `slots` holds EVERY window's textures at once. That is exactly why [`LayerKey`] carries
/// its window.
///
/// A CLOSED preview's slots are reclaimed by the next prepare of any SURVIVING preview,
/// which is what keeps a long-lived multi-document host from parking dead previews' textures
/// in VRAM until the process exits. That eviction has to ride the primitive
/// ([`LayerStackPrimitive::live`]) because iced's storage exposes no external handle: app
/// code cannot reach in here to free anything.
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

/// Windows (DRAGON-235): lift an iced RGBA image handle back into a [`PixelFrame`] so a
/// still base / video poster can be drawn through this persistent-texture shader instead of
/// `widget::image`. iced's raster-image pipeline does NOT composite on the premultiplied
/// transparent OVERLAY surface (empirically: the identical opaque pixels show through the
/// opaque windowed surface but vanish on the overlay; the shader — same ALPHA_BLENDING —
/// composites them correctly). Returns `None` for a non-RGBA handle (e.g. a decode that fell
/// back to `Handle::from_path`), leaving the caller on `widget::image`. Copies the pixels
/// (the shader owns its upload buffer); called only for the STATIC overlay preview (a still,
/// or a paused poster), never per playback frame.
#[cfg(windows)]
pub(super) fn rgba_handle_frame(handle: &cosmic::widget::image::Handle) -> Option<Arc<PixelFrame>> {
    match handle {
        cosmic::widget::image::Handle::Rgba { width, height, pixels, .. } => {
            Some(PixelFrame::new(pixels.to_vec(), *width, *height))
        }
        _ => None,
    }
}

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

    /// The slot prune, exercised exactly as `prepare` runs it: `held` are the slots the
    /// process-wide pipeline currently holds, `layers` the keys of the primitive being
    /// prepared, `open` the preview windows still open. Returns the surviving slots.
    fn prune(held: &[LayerKey], layers: &[LayerKey], open: &[window::Id]) -> Vec<LayerKey> {
        let present: HashSet<LayerKey> = layers.iter().copied().collect();
        let drawn: HashSet<window::Id> = present.iter().map(|k| k.window()).collect();
        let live: HashSet<window::Id> = open.iter().copied().collect();
        held.iter().copied().filter(|k| slot_survives(*k, &present, &drawn, &live)).collect()
    }

    /// The same slot in two different windows must be two DISTINCT keys (they are separate
    /// texture slots in the one process-wide pipeline), while the same (window, slot) pair
    /// stays equal + hash-equal so a slot is found across frames.
    #[test]
    fn layer_keys_are_scoped_to_their_window() {
        let (a, b) = (window::Id::unique(), window::Id::unique());
        assert_ne!(a, b, "iced hands out unique window ids");
        assert_ne!(LayerKey::video(a), LayerKey::video(b), "same slot, different window");
        assert_ne!(LayerKey::video(a), LayerKey::covermark(a), "different slot, same window");
        assert_eq!(LayerKey::video(a), LayerKey::new(a, LayerSlot::VIDEO));
        assert_eq!(LayerKey::covermark(b), LayerKey::new(b, LayerSlot::COVERMARK));
        // Hash identity is what the `slots` HashMap actually looks up on.
        let mut map = HashMap::new();
        map.insert(LayerKey::video(a), 1);
        map.insert(LayerKey::video(b), 2);
        map.insert(LayerKey::covermark(a), 3);
        assert_eq!(map.len(), 3, "each (window, slot) pair owns its own texture slot");
        assert_eq!(map.get(&LayerKey::new(a, LayerSlot::VIDEO)), Some(&1));
    }

    /// The whole point of the window scoping: window A's prepare must never be able to
    /// select — let alone delete — window B's slots, however few layers A draws.
    #[test]
    fn a_prune_for_one_window_never_touches_another_windows_slots() {
        let (a, b) = (window::Id::unique(), window::Id::unique());
        let held =
            [LayerKey::video(a), LayerKey::covermark(a), LayerKey::video(b), LayerKey::covermark(b)];
        // A draws only its video layer (its covermark was cleared): A's covermark is
        // reclaimed, B keeps BOTH of its slots.
        let survivors = prune(&held, &[LayerKey::video(a)], &[a, b]);
        assert_eq!(
            survivors,
            vec![LayerKey::video(a), LayerKey::video(b), LayerKey::covermark(b)],
            "only the drawn window's absent slots may be pruned"
        );
        // Symmetrically for B.
        let survivors = prune(&held, &[LayerKey::covermark(b)], &[a, b]);
        assert_eq!(
            survivors,
            vec![LayerKey::video(a), LayerKey::covermark(a), LayerKey::covermark(b)]
        );
    }

    /// The closed-window reclaim: with several previews open, closing one must free ITS
    /// textures on the next prepare of ANY surviving preview — nothing else ever will, since
    /// a closed window is never drawn again.
    #[test]
    fn a_closed_previews_slots_are_evicted_by_another_windows_prepare() {
        let (a, b) = (window::Id::unique(), window::Id::unique());
        let held =
            [LayerKey::video(a), LayerKey::covermark(a), LayerKey::video(b), LayerKey::covermark(b)];
        // B closed; A redraws. Everything of B's goes, A keeps what it drew.
        let survivors = prune(&held, &[LayerKey::video(a), LayerKey::covermark(a)], &[a]);
        assert_eq!(survivors, vec![LayerKey::video(a), LayerKey::covermark(a)]);
        // A window can be evicted even by a prepare that draws NOTHING of its own.
        assert_eq!(prune(&held, &[LayerKey::video(a)], &[a]), vec![LayerKey::video(a)]);
    }

    /// The hazard the live set exists to avoid: a preview that has OPENED but not yet been
    /// prepared has no layers in anyone's primitive, and must survive another window's
    /// prepare regardless. It is in the OPEN set, which is what protects it.
    #[test]
    fn a_newly_opened_preview_is_never_wiped_before_it_first_draws() {
        let (a, b) = (window::Id::unique(), window::Id::unique());
        // B just opened (its slots exist from its very first upsert, or not at all) while A
        // is the one being prepared. B is open, so nothing of B's may be touched.
        let held = [LayerKey::video(a), LayerKey::video(b)];
        assert_eq!(
            prune(&held, &[LayerKey::video(a)], &[a, b]),
            held.to_vec(),
            "an open-but-undrawn preview keeps its slots"
        );
        // Belt: an EMPTY live set means "unknown", never "everything closed".
        assert_eq!(prune(&held, &[LayerKey::video(a)], &[]), held.to_vec());
    }

    /// Single-window behaviour is unchanged (the pre-scoping semantics): everything the
    /// primitive didn't draw is pruned. A primitive with NO layers names no window, so it
    /// prunes nothing rather than wiping the whole process's slots.
    #[test]
    fn prune_within_one_window_is_unchanged_and_an_empty_stack_prunes_nothing() {
        let a = window::Id::unique();
        let held = [LayerKey::video(a), LayerKey::covermark(a)];
        assert_eq!(prune(&held, &[LayerKey::video(a)], &[a]), vec![LayerKey::video(a)]);
        assert_eq!(prune(&held, &held, &[a]), held.to_vec(), "drawn slots always survive");
        assert_eq!(prune(&held, &[], &[a]), held.to_vec(), "an empty primitive prunes nothing");
    }

    /// DRAGON-235: an RGBA handle lifts back into a `PixelFrame` (dims + pixels preserved) so
    /// the Windows overlay can draw it through the shader; a non-RGBA handle (e.g. a path
    /// decode fallback) yields `None`, leaving the caller on `widget::image`.
    #[cfg(windows)]
    #[test]
    fn rgba_handle_frame_lifts_rgba_and_rejects_path() {
        use cosmic::widget::image::Handle;
        let pixels = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let frame = rgba_handle_frame(&Handle::from_rgba(2, 1, pixels.clone()))
            .expect("an rgba handle lifts to a PixelFrame");
        assert_eq!((frame.w, frame.h), (2, 1));
        assert_eq!(frame.rgba, pixels);
        assert!(
            rgba_handle_frame(&Handle::from_path("does-not-exist.png")).is_none(),
            "a non-RGBA (path) handle must not lift — the caller keeps widget::image"
        );
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
