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
//! A window MAY mount several `LayerStack`s (DRAGON-373 — the text annotations are one layer per
//! item, drawn by the `AnnotationCanvas` so they can interleave with the vector kinds in true
//! z-order), on ONE condition: every stack of that window must carry the same window-wide key set
//! in [`LayerStack::part`]. The prune below reclaims what a window stopped drawing, and it reads
//! that set rather than the individual primitive's layers — without it, each stack's prepare
//! would delete the others' slots and every layer would flicker. [`LayerStack::new`] is the
//! single-stack form (the window's layers ARE its whole intent). Different windows are
//! independent either way.
//!
//! Adding a new editable layer:
//! 1. Add a new [`LayerSlot`] const — the stable WITHIN-window identity of its texture slot.
//! 2. Produce its pixels off-thread as an `Arc<PixelFrame>`, tracked by a [`RasterSlot`]
//!    (coalesces overlapping refresh requests, drops stale results).
//! 3. Push a `Layer::full(LayerKey::new(preview.window, SLOT), frame)` into the `Vec`
//!    passed to `LayerStack::new` in the view — draw order follows the `Vec`'s order, NOT
//!    key order. If the layer's content occupies only PART of the picture, use
//!    `Layer::at(key, frame, dest)` and raster only that region — see [`Layer::dest`].
//!
//! # Not only the preview editor (DRAGON-TBD)
//! Nothing above is preview-specific, and the colour picker's magnifier is the first user
//! outside it: a [`LayerKey`] is (window, slot), and a picker OVERLAY is simply another
//! window, so it gets its own texture slots for free. Two things came with that guest, both
//! additive: [`LayerSlot::COLOR_MAGNIFIER`], and [`Filter`], because the magnifier is the one
//! layer that must NOT be smoothed. The word "preview" in the names below is history, not a
//! constraint; `live` in [`LayerStack::new`] means "every window whose slots must survive this
//! prepare", which for a picker session is its overlays (`App::live_picker_windows`).
//!
//! # Sizing a layer's raster (DRAGON-362)
//! Two rules, both cost-critical on a large (5K+) capture:
//! * Raster at the layer's ON-SCREEN device-pixel size, via `edit::layer_raster_scale`
//!   (`view_zoom × preview_visual_scale`), capped at the source frame and at
//!   `edit::MAX_LAYER_DIM`. Smaller than that and the layer is UPSAMPLED into a canvas whose
//!   base image was DOWNSAMPLED — the "fuzzy overlay beside crisp pixels" defect.
//! * Raster only the region the content covers ([`Layer::dest`]). Both the CPU render and the
//!   GPU upload are `O(raster area)`, and layers that re-render per keystroke (text) must not
//!   pay for the whole capture.

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
    /// DRAGON-330; box/arrow stay vector geometry drawn by the `AnnotationCanvas`, which since
    /// DRAGON-373 also draws the per-item TEXT layers so they interleave with those vectors.)
    pub const COVERMARK: LayerSlot = LayerSlot(1);
    /// The colour picker's magnifier disc (DRAGON-TBD). A raster rebuilt on nearly every real
    /// pointer move, which is exactly the churn this shader exists to stop: the older
    /// `widget::image` path minted a fresh `Handle` per move and iced allocated + trimmed an
    /// atlas entry for each one. It is the only slot so far that does NOT belong to a preview
    /// document, and nothing about the pipeline had to change for that: a [`LayerKey`] is
    /// (window, slot), and a picker overlay is simply another window.
    ///
    /// It is also the only slot that asks for [`Filter::Nearest`] (see that type): the tool
    /// exists to show exact, unblurred source pixels.
    pub const COLOR_MAGNIFIER: LayerSlot = LayerSlot(2);
    /// Where the per-text-item slots start (DRAGON-373). Well clear of the fixed slots above,
    /// and of any that get added beside them.
    const TEXT_BASE: u32 = 0x8000_0000;

    /// The raster of ONE text annotation (DRAGON-354, split per item by DRAGON-373): its glyphs
    /// drawn by the shared embedded-font renderer. A raster layer (not a canvas vector) so the
    /// glyphs are pixel-identical to the bake and a per-keystroke re-render never churns iced's
    /// atlas.
    ///
    /// # Why one slot PER ITEM rather than one for all text
    /// The live view has to interleave text with the vector kinds in true z-order — a rectangle
    /// brought over one caption and under another is an ordinary thing to draw, and the bake has
    /// always rendered it that way (`rasterize_scene` walks every kind in ONE in-order loop). A
    /// single composited text raster can only ever sit at ONE depth, so it cannot express that
    /// however it is stacked. Per item, the canvas simply draws each raster at its own place in
    /// the item order and interleaving falls out. It is also cheaper for captions that are far
    /// apart: the old shared layer had to span their UNION, which on a large capture is most of
    /// the picture, while each item's raster tracks only its own ink.
    ///
    /// `id` is the annotation's id — a per-document monotonic counter (`EditState::next_annot_id`)
    /// that starts at 1, so the low 32 bits identify it uniquely in any real scene, and slots are
    /// window-scoped ([`LayerKey`]) so two documents can never collide at all.
    pub fn text(id: u64) -> LayerSlot {
        LayerSlot(Self::TEXT_BASE.wrapping_add(id as u32))
    }
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

    /// **Pure**, unit-tested: this window's colour-picker magnifier disc (DRAGON-TBD).
    ///
    /// `window` here is a picker OVERLAY, one per output, not a preview document. The pipeline
    /// needs no notion of that: the key already says "whose", so the picker's slots and a
    /// preview's cannot collide even in the impossible case of one process owning both.
    pub fn color_magnifier(window: window::Id) -> Self {
        Self::new(window, LayerSlot::COLOR_MAGNIFIER)
    }

    /// This window's raster for the text annotation `id` (DRAGON-354/373).
    pub fn text(window: window::Id, id: u64) -> Self {
        Self::new(window, LayerSlot::text(id))
    }

    /// The window this slot belongs to — what makes the prune window-scoped.
    pub fn window(self) -> window::Id {
        self.window
    }
}

/// Where in the widget's bounds a layer's texture is drawn, as `[x, y, w, h]` FRACTIONS of
/// those bounds (y down from the top). [`DEST_FULL`] — the whole canvas — is what every layer
/// did before DRAGON-362.
pub type Dest = [f32; 4];

/// The whole widget: the placement every full-frame layer (video, covermark) uses.
pub const DEST_FULL: Dest = [0.0, 0.0, 1.0, 1.0];

/// How a layer's texture is SAMPLED when the pixels it holds and the device pixels it covers
/// are not the same count, which is nearly always: a zoomed preview, a fractional display
/// scale, a raster drawn at its own POINT size on a HiDPI output.
///
/// [`Filter::Linear`] is the default and is what every layer this shader was originally built
/// for wants: video frames, the covermark and the text rasters are all photographic or
/// antialiased content, and smoothing is the correct answer for them.
///
/// [`Filter::Nearest`] exists for the one kind of layer that must NOT be smoothed, the colour
/// picker's magnifier (DRAGON-TBD). That tool's whole job is to show exact, unblurred source
/// pixels, so its `widget::image` always asked for `FilterMethod::Nearest`; routing it through
/// the shared Linear sampler would blur every magnifier cell boundary, which is the lens lying
/// about which pixel is which rather than a cosmetic nit.
///
/// It is a property of the LAYER rather than of the pipeline because one stack can hold both
/// kinds at once, and it is carried into the [`TextureSlot`] so a slot whose filter changed is
/// re-bound instead of silently keeping the old sampler.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Filter {
    /// Smoothed. Every preview-editor layer.
    #[default]
    Linear,
    /// Blocky, exact texels. The colour picker's magnifier, and nothing else so far.
    Nearest,
}

/// One layer to draw: a stable identity, the pixels currently on it, and WHERE in the canvas
/// they go ([`Layer::dest`]).
#[derive(Clone, Debug)]
pub struct Layer {
    pub key: LayerKey,
    pub frame: Arc<PixelFrame>,
    /// The sub-rect of the widget's bounds this layer covers, in `[x, y, w, h]` fractions
    /// (see [`Dest`]). [`DEST_FULL`] for a layer whose raster spans the whole picture.
    ///
    /// WHY a layer may cover only PART of the canvas (DRAGON-362): a full-frame raster costs
    /// `O(frame area)` to render AND to upload, and the text layer re-renders on every
    /// keystroke — at 5120×2880 that is ~59 MB and ~82 ms per event, which is what made the
    /// editor unusable on a large capture. A layer whose content occupies a small region
    /// (a caption) rasters just that region and places it here instead, so its cost tracks
    /// the CONTENT's area rather than the capture's.
    pub dest: Dest,
    /// How this layer's texels are sampled, see [`Filter`]. [`Filter::Linear`] unless the
    /// layer opted out with [`Layer::nearest`], so every pre-DRAGON-TBD call site is
    /// byte-identical.
    pub filter: Filter,
}

impl Layer {
    /// A layer whose raster spans the whole picture (video frames, the covermark).
    pub fn full(key: LayerKey, frame: Arc<PixelFrame>) -> Self {
        Self { key, frame, dest: DEST_FULL, filter: Filter::Linear }
    }

    /// A layer whose raster covers only `dest` (fractions of the canvas) — see [`Layer::dest`].
    pub fn at(key: LayerKey, frame: Arc<PixelFrame>, dest: Dest) -> Self {
        Self { key, frame, dest, filter: Filter::Linear }
    }

    /// **Pure**, unit-tested: draw this layer's texels UNSMOOTHED ([`Filter::Nearest`]).
    ///
    /// Opt-in, and deliberately a builder rather than a parameter on the two constructors: the
    /// smoothed answer is right for every layer but one, so the constructors stay the shape
    /// they were and only the colour picker's magnifier says otherwise (DRAGON-TBD).
    pub fn nearest(mut self) -> Self {
        self.filter = Filter::Nearest;
        self
    }
}

/// The `shader::Program` placed in the view, holding the layers to draw (in order) plus
/// the set of preview windows that are currently OPEN (see [`LayerStackPrimitive::live`]).
pub struct LayerStack {
    layers: Vec<Layer>,
    live: Vec<window::Id>,
    /// See [`LayerStackPrimitive::window_keys`]. Equal to `layers`' keys for a window that
    /// mounts exactly one stack.
    window_keys: Vec<LayerKey>,
}

impl LayerStack {
    /// The whole of a window's layer stack: `layers` is everything that window draws this
    /// frame. `live` must be EVERY open preview window (`App::live_preview_windows`), not just
    /// the one drawing — it is what lets this primitive free a CLOSED preview's textures.
    pub fn new(layers: Vec<Layer>, live: Vec<window::Id>) -> Self {
        let window_keys = layers.iter().map(|l| l.key).collect();
        Self { layers, live, window_keys }
    }

    /// PART of a window's layer stack (DRAGON-373): this primitive draws `layers`, while
    /// `window_keys` is every key the owning window draws THIS FRAME across all of its
    /// primitives. See [`LayerStackPrimitive::window_keys`] for why that is needed the moment a
    /// window mounts more than one stack.
    pub fn part(layers: Vec<Layer>, live: Vec<window::Id>, window_keys: Vec<LayerKey>) -> Self {
        Self { layers, live, window_keys }
    }
}

impl<Message> shader::Program<Message> for LayerStack {
    type State = ();
    type Primitive = LayerStackPrimitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> LayerStackPrimitive {
        // Arc clones are cheap — this runs every view build.
        LayerStackPrimitive {
            layers: self.layers.clone(),
            live: self.live.clone(),
            window_keys: self.window_keys.clone(),
        }
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
    /// Every layer key the OWNING WINDOW draws this frame — across ALL of its primitives, not
    /// just this one (DRAGON-373).
    ///
    /// The within-window reclaim (rule 2 of [`slot_survives`]) frees whatever the window stopped
    /// drawing, and it needs the window's whole intent to do that. While a window mounted exactly
    /// one `LayerStack` its own layers WERE that intent, which is why this used to be implicit.
    /// Since the text annotations became one layer per item drawn by the `AnnotationCanvas` (so
    /// they can interleave with the vector kinds in true z-order), a window submits several
    /// stacks per frame — and with the implicit rule each one's prepare would delete the others'
    /// slots, so every layer would be re-created every frame and each would VANISH on the frames
    /// it lost. Carrying the same window-wide key set in every primitive makes the reclaim agree
    /// with itself however many stacks a window mounts.
    window_keys: Vec<LayerKey>,
}

impl shader::Primitive for LayerStackPrimitive {
    type Pipeline = LayerStackPipeline;

    fn prepare(
        &self,
        pipeline: &mut LayerStackPipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        // DRAGON-401: record what THIS device accepts for `set_viewport` (and the scale factor
        // iced multiplies our bounds by), which is what bounds the preview's zoom. The app
        // never holds a `wgpu::Device`; `prepare` is the only place both facts are in hand.
        crate::widgets::gpu::observe(device, viewport);
        // DRAGON-454: the first time any layer reaches the GPU. Pipeline + bind-group creation
        // and the first texture upload all happen under this call, on the render thread — so on
        // the editor's launch timeline this is where "first-frame wgpu work" lands.
        {
            use std::sync::atomic::{AtomicBool, Ordering};
            static FIRST: AtomicBool = AtomicBool::new(false);
            if !self.layers.is_empty() && !FIRST.swap(true, Ordering::Relaxed) {
                crate::util::timing_mark("preview: FIRST layer-stack GPU prepare (begin)");
            }
        }
        for layer in &self.layers {
            pipeline.upsert(device, queue, layer.key, &layer.frame, layer.dest, layer.filter);
        }
        // Two reclaims in one pass over the process-wide slot map:
        //  * WITHIN a window this primitive drew, anything the WINDOW stopped drawing (e.g. the
        //    covermark was cleared, or a text box was deleted) is freed — `window_keys` is that
        //    window's whole intent for this frame, so several stacks in one window agree.
        //  * For any window that is no longer an OPEN preview, everything is freed — the
        //    closed-preview leak, which no prepare OF THAT WINDOW can ever fix because that
        //    window is never drawn again.
        // Anything else — another LIVE window's slots — is untouchable here.
        let present: HashSet<LayerKey> = self.window_keys.iter().copied().collect();
        let drawn: HashSet<window::Id> = self.layers.iter().map(|l| l.key.window()).collect();
        let live: HashSet<window::Id> = self.live.iter().copied().collect();
        pipeline.slots.retain(|k, _| slot_survives(*k, &present, &drawn, &live));
    }

    fn draw(&self, pipeline: &LayerStackPipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        pipeline.draw(&self.layers, render_pass)
    }
}

/// The prune predicate, split out as pure logic so it can be unit-tested without a GPU:
/// does the existing texture slot `key` survive a prepare that drew layers for the windows
/// `drawn`, whose owning windows intend to draw `present` this frame
/// ([`LayerStackPrimitive::window_keys`]), given that `live` are the preview windows currently
/// OPEN?
///
/// Two independent reasons to free a slot, and nothing else may:
/// 1. **Its window is closed** — not in `live`. This is the reclaim for a preview that went
///    away while others stayed open; no prepare of that window will ever run again.
/// 2. **Its window WAS drawn by this primitive and the WINDOW isn't drawing that slot** — the
///    within-window reclaim (a cleared covermark, a deleted text box). `present` is the
///    window's whole frame, not this primitive's share of it, so a window that mounts several
///    stacks reclaims the same set from every one of them (DRAGON-373).
///
/// The safety property that makes (1) sound is that `live` is the set of OPEN previews, not
/// of drawn ones: a preview that has just opened and has not yet been prepared is open, so
/// it is in `live`, so another window's prepare leaves it alone. As a belt for a caller that
/// somehow has no live set at all, an EMPTY `live` disables (1) entirely rather than wiping
/// the process's slots — an unknown set must never be read as "everything is closed".
/// A primitive that drew NOTHING (`drawn` empty) likewise prunes nothing under (2), whatever
/// `present` says — a window only reclaims on a frame it actually rendered.
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

/// One layer's persistent GPU texture + the bind group wrapping it, plus the small uniform
/// carrying its [`Layer::dest`] placement.
struct TextureSlot {
    texture: wgpu::Texture,
    /// The 16-byte placement uniform (`vec4<f32>` = `[x, y, w, h]` fractions of the widget).
    /// Lives alongside the texture in the SAME bind group, so it is created once with the
    /// slot and only re-WRITTEN (never reallocated) when the placement moves.
    dest_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    dims: (u32, u32),
    /// The placement currently in `dest_buf`, so a redraw with an unchanged rect skips the
    /// write — the same "only touch the GPU when something actually changed" rule the `seq`
    /// check applies to the pixels.
    dest: Dest,
    /// Which sampler the [`Self::bind_group`] above was built against (DRAGON-TBD). A sampler
    /// is baked into the bind group, so it cannot be swapped in place: a layer that changed
    /// its [`Filter`] has to be re-bound, and that is only detectable by remembering what the
    /// slot was built with.
    filter: Filter,
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
    /// The smoothing sampler every preview-editor layer takes ([`Filter::Linear`]).
    sampler_linear: wgpu::Sampler,
    /// The exact-texel sampler ([`Filter::Nearest`], DRAGON-TBD), for the colour picker's
    /// magnifier. Created once beside the other rather than per slot: a sampler carries no
    /// per-layer state, so two of them cover every layer this stack will ever hold.
    sampler_nearest: wgpu::Sampler,
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
                // The per-slot placement rect (DRAGON-362), read by the VERTEX stage to put
                // the quad somewhere other than the whole canvas.
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
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
        let sampler_linear = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("cck-video-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        // The same descriptor with both filters flipped to Nearest (DRAGON-TBD). Everything
        // else about it, the clamped address modes included, must match: only the smoothing
        // differs between a video frame and the picker's magnifier.
        let sampler_nearest = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("cck-video-sampler-nearest"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
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
            sampler_linear,
            sampler_nearest,
            tex_format,
            slots: HashMap::new(),
        }
    }
}

impl LayerStackPipeline {
    /// The sampler a layer asking for `filter` binds to (DRAGON-TBD). Both are created once in
    /// `new`; nothing here allocates.
    fn sampler_for(&self, filter: Filter) -> &wgpu::Sampler {
        match filter {
            Filter::Linear => &self.sampler_linear,
            Filter::Nearest => &self.sampler_nearest,
        }
    }

    /// Upsert `key`'s slot: (re)create its texture when missing, its dimensions
    /// changed, or its [`Filter`] changed (any of which forces a re-upload below), then
    /// upload `frame`'s pixels, but skip the upload when the frame hasn't changed since last
    /// time (its `seq` already matches). `dest` is the layer's placement; it is only written
    /// when it actually moved.
    fn upsert(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: LayerKey,
        frame: &PixelFrame,
        dest: Dest,
        filter: Filter,
    ) {
        let (w, h) = (frame.w, frame.h);
        if w == 0 || h == 0 {
            return;
        }
        // A sampler is baked into the bind group, so a filter change is rebuilt exactly like a
        // dimension change rather than patched in place. It cannot happen on any layer the app
        // draws today (a layer's filter is a property of WHAT it is), so this is the cheap
        // honest answer rather than a hot path.
        let needs_new = match self.slots.get(&key) {
            Some(slot) => slot.dims != (w, h) || slot.filter != filter,
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
            let dest_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cck-video-dest"),
                size: std::mem::size_of::<Dest>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
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
                        resource: wgpu::BindingResource::Sampler(self.sampler_for(filter)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: dest_buf.as_entire_binding(),
                    },
                ],
            });
            // A sentinel placement that can never equal a real one, so the write below always
            // runs for a fresh slot (the buffer's contents are undefined until it does).
            self.slots.insert(
                key,
                TextureSlot {
                    texture,
                    dest_buf,
                    bind_group,
                    dims: (w, h),
                    dest: [f32::NAN; 4],
                    filter,
                    seq: 0,
                },
            );
        }
        let slot = self.slots.get_mut(&key).expect("just inserted above when missing");
        if slot.dest != dest {
            let mut bytes = [0u8; std::mem::size_of::<Dest>()];
            for (i, v) in dest.iter().enumerate() {
                bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
            }
            queue.write_buffer(&slot.dest_buf, 0, &bytes);
            slot.dest = dest;
        }
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

/// Placed-quad vertex shader + a texture-sampling fragment shader.
///
/// iced sets the render pass's VIEWPORT to the widget's physical bounds (and the scissor to
/// its visible clip) before calling `Primitive::draw`, so clip space `-1..1` IS the widget
/// rectangle — no bounds transform belongs here. The quad is then placed WITHIN that
/// rectangle by the per-slot `dest` uniform (`[x, y, w, h]` fractions, y down from the top);
/// `DEST_FULL` = `(0, 0, 1, 1)` reproduces the historical full-widget quad exactly.
const WGSL: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> dest: vec4<f32>;

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VsOut {
    // The unit quad in TOP-DOWN texture space; the same six corners as the uv table below,
    // so corner k's uv is exactly its position in the destination rect.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 0.0),
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(1.0, 0.0)
    );
    let c = corners[idx];
    // Unit quad -> destination rect (widget fractions, y down) -> clip space (y up).
    let fx = dest.x + c.x * dest.z;
    let fy = dest.y + c.y * dest.w;
    var out: VsOut;
    out.pos = vec4<f32>(fx * 2.0 - 1.0, 1.0 - fy * 2.0, 0.0, 1.0);
    out.uv = c;
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

/// Windows QA hook (DRAGON-395): whether to BYPASS the DRAGON-235 fold and let the overlay
/// preview take the portable `widget::image` + separate-covermark path instead.
///
/// `CCK_TEST_UNFOLD_COVERMARK=1`. A review aid in the same spirit as
/// `CCK_STARTUP_BUDGET_SECS`, not a supported setting — it exists because the DRAGON-235
/// premise (iced's raster pipeline does not composite on the premultiplied transparent
/// overlay surface) has never been verified on a machine that could run it, and the fold it
/// justifies costs a z-order deviation from Linux and macOS (see [`rgba_handle_frame`] and
/// the fold sites in `preview::image` / `preview::video`). DRAGON-427 removed the overlay
/// editor from Windows 10, so the arm is Windows-11-only now — which is finally a machine we
/// have. One toggle, one A/B, and the answer either deletes the fold or documents it.
///
/// Read ONCE per process and cached: the view path this gates runs every frame.
#[cfg(windows)]
pub(super) fn unfold_covermark() -> bool {
    static UNFOLD: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *UNFOLD.get_or_init(|| {
        let on = std::env::var_os("CCK_TEST_UNFOLD_COVERMARK").is_some_and(|v| v == "1");
        if on {
            log::warn!(
                "CCK_TEST_UNFOLD_COVERMARK=1: the overlay preview will draw its base through \
                 widget::image with the covermark as a separate layer (the DRAGON-235 fold is \
                 bypassed). If the base or the covermark renders blank, DRAGON-235 still holds."
            );
        }
        on
    })
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

    /// The slot prune, exercised exactly as `prepare` runs it for a window that mounts ONE
    /// stack: `held` are the slots the process-wide pipeline currently holds, `layers` the keys
    /// of the primitive being prepared (which are then also its window's whole intent), `open`
    /// the preview windows still open. Returns the surviving slots.
    fn prune(held: &[LayerKey], layers: &[LayerKey], open: &[window::Id]) -> Vec<LayerKey> {
        prune_part(held, layers, layers, open)
    }

    /// The prune for ONE primitive of a window that mounts SEVERAL stacks (DRAGON-373):
    /// `layers` is this primitive's share, `window_keys` the whole window's frame.
    fn prune_part(
        held: &[LayerKey],
        layers: &[LayerKey],
        window_keys: &[LayerKey],
        open: &[window::Id],
    ) -> Vec<LayerKey> {
        let present: HashSet<LayerKey> = window_keys.iter().copied().collect();
        let drawn: HashSet<window::Id> = layers.iter().map(|k| k.window()).collect();
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

    /// DRAGON-TBD: the colour picker's magnifier is a slot like any other, and the property
    /// that matters is that it is DISTINCT: from the other slots of the same window, and from
    /// the same slot of another window (each picker overlay has its own). A collision would
    /// show as one display's lens drawing another's pixels, which is the one thing a colour
    /// picker may not do.
    #[test]
    fn the_magnifier_slot_is_its_own_texture_everywhere() {
        let (a, b) = (window::Id::unique(), window::Id::unique());
        assert_ne!(LayerKey::color_magnifier(a), LayerKey::video(a), "same window, video");
        assert_ne!(LayerKey::color_magnifier(a), LayerKey::covermark(a), "same window, covermark");
        assert_ne!(LayerKey::color_magnifier(a), LayerKey::text(a, 1), "same window, text");
        assert_ne!(LayerKey::color_magnifier(a), LayerKey::color_magnifier(b), "two overlays");
        assert_eq!(
            LayerKey::color_magnifier(a),
            LayerKey::new(a, LayerSlot::COLOR_MAGNIFIER),
            "the constructor is the slot, spelled once"
        );
        // Hash identity is what the `slots` HashMap actually looks up on, so pin it there too:
        // a two-display picker session holds two magnifier textures at once.
        let mut map = HashMap::new();
        map.insert(LayerKey::color_magnifier(a), 1);
        map.insert(LayerKey::color_magnifier(b), 2);
        map.insert(LayerKey::video(a), 3);
        assert_eq!(map.len(), 3);
        assert_eq!(map.get(&LayerKey::color_magnifier(b)), Some(&2));
    }

    /// DRAGON-TBD: smoothing is the DEFAULT and stays the default. Every preview-editor layer
    /// predates the filter and must be byte-identical, so both constructors answer Linear and
    /// only an explicit `nearest()` opts out.
    #[test]
    fn layers_smooth_unless_they_ask_not_to() {
        let key = LayerKey::video(window::Id::unique());
        assert_eq!(Layer::full(key, dummy_frame()).filter, Filter::Linear, "full() smooths");
        assert_eq!(
            Layer::at(key, dummy_frame(), [0.0, 0.0, 0.5, 0.5]).filter,
            Filter::Linear,
            "at() smooths"
        );
        assert_eq!(Filter::default(), Filter::Linear, "the default is the preview editor's");
        let n = Layer::full(key, dummy_frame()).nearest();
        assert_eq!(n.filter, Filter::Nearest, "nearest() is the opt-out");
        // The builder changes NOTHING else about the layer.
        let plain = Layer::full(key, dummy_frame());
        assert_eq!(n.key, plain.key);
        assert_eq!(n.dest, plain.dest);
        // …and it survives a `Layer::at` placement too, so a future partial-canvas layer can
        // take both.
        assert_eq!(
            Layer::at(key, dummy_frame(), [0.1, 0.2, 0.3, 0.4]).nearest().dest,
            [0.1, 0.2, 0.3, 0.4]
        );
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

    /// DRAGON-373: a window that mounts SEVERAL stacks in one frame (the covermark stack plus a
    /// text layer per annotation, so text can interleave with the vector kinds) must reclaim the
    /// same set from every one of them. Each primitive carries the window's whole frame, so no
    /// stack's prepare can delete another's slots — the failure this replaced was every layer
    /// being destroyed and re-created each frame, and vanishing on the frames it lost.
    #[test]
    fn several_stacks_in_one_window_never_delete_each_others_slots() {
        let w = window::Id::unique();
        let (cm, t1, t2) = (LayerKey::covermark(w), LayerKey::text(w, 1), LayerKey::text(w, 2));
        let held = [cm, t1, t2];
        let frame = [cm, t1, t2];
        // The covermark stack's prepare, then each text layer's: all three survive every one.
        for part in [&[cm][..], &[t1][..], &[t2][..]] {
            assert_eq!(
                prune_part(&held, part, &frame, &[w]),
                held.to_vec(),
                "the stack drawing {part:?} deleted another stack's slots"
            );
        }
        // …and the within-window reclaim still works on the WINDOW's intent: delete text box 2
        // and its slot goes, on the very next prepare of any of the window's stacks.
        let frame = [cm, t1];
        for part in [&[cm][..], &[t1][..]] {
            assert_eq!(prune_part(&held, part, &frame, &[w]), vec![cm, t1]);
        }
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
