//! The colour picker's two views: the dimmed magnifier OVERLAY and the result WINDOW.
//!
//! Everything here is built from ordinary iced elements, with ONE exception, and the
//! exception is exactly as narrow as the constraint allows.
//!
//! **The constraint**: a process on iced's software rasterizer cannot draw a
//! `widget::shader` AT ALL. A shader in this view unconditionally would not be slower
//! there, it would be BLANK. That is why this file said "nothing here uses
//! `cosmic::iced::widget::shader`" and treated it as a hard rule.
//!
//! **The exception** (DRAGON-TBD): the magnifier disc is rebuilt on effectively every real
//! pointer move, and minting a fresh `image::Handle` each time makes iced allocate and trim a
//! texture-atlas entry per move, which is the churn `preview::layers` exists to stop with a
//! persistent GPU texture re-uploaded in place. So the disc now draws through that stack, and
//! the rule becomes a BRANCH rather than a ban: `super::MagnifierRaster` carries whichever form
//! this machine can draw, decided once in the update handler. The key was the platform's
//! `software_overlays()` until DRAGON-650 exempted the picker process from the Windows 10
//! software force (its overlay is an opaque snapshot needing no per-pixel window alpha);
//! it is now the process's own renderer, `app::process_forced_software_backend`, and the
//! image arm is the historical `widget::image` code byte for byte. Every other part of this
//! view (the ring, the hex chip, the swatches, the unavailable pill) is still a plain
//! container, so nothing else has to care.

use super::*;
// The persistent-texture layer stack, re-exported by `preview` for exactly this caller
// (DRAGON-TBD). Named imports rather than a glob so it is obvious at the use site that the
// magnifier is drawing through the SAME machinery the preview editor's video and covermark do.
use crate::app::preview::{Layer, LayerKey, LayerStack};

/// The gap between the magnifier's rim and the hex label.
const LABEL_GAP: f32 = 10.0;
/// The hex label's box, used BOTH for the placement decision and for the rendered chip,
/// so the geometry and the pixels cannot disagree. Wide enough for `#RRGGBB` at
/// [`LABEL_TEXT_SIZE`] plus its padding.
///
/// **The box is RE-DERIVED whenever the text size moves, never left behind.** DRAGON-601
/// doubled the text to 28pt and doubled this with it, `(96, 30)` to `(192, 60)`; the same
/// ticket then trimmed the text to 22pt on review, and this came down to `(152, 48)` in the
/// same step. A box left at the size a LARGER text needed is not harmless padding: it is what
/// [`geom::label_placement`] and [`geom::label_origin`] measure, so an oversized box makes the
/// ladder flip sides further from each screen edge than the visible chip has any reason to,
/// and near a corner it can send the chip to a rung the real text would never have needed.
/// [`label_metrics_tests`] pins that relation from BOTH ends, so the next size change cannot
/// quietly skip this constant.
///
/// The proportions are the ones the box has always had, roughly 6.9 x 2.2 ems of the text
/// size, so the chip's padding still reads the same relative to the text inside it. Rounded
/// to eights rather than carried to the decimal, because the exact proportional answer for
/// 22pt, `(150.9, 47.1)`, is a false precision: the padding it describes is a look, not a
/// measurement.
const LABEL_SIZE: (f32, f32) = (152.0, 48.0);
/// The hex label's text size. Named because [`label_metrics_tests`] measures against it.
///
/// The hex is the one value the whole tool exists to report, read at arm's length off a dimmed
/// screen, and at the original 14pt it was the smallest thing on the overlay. DRAGON-601 took
/// it to 28, which the owner then found too large on the shipped build, and settled at 22.
/// Whatever this becomes, [`LABEL_SIZE`] moves with it.
const LABEL_TEXT_SIZE: u16 = 22;

impl App {
    /// The colour picker OVERLAY for one output: the frozen scene, the configured dim,
    /// the magnifier disc under the pointer, the hex label, the transparent input surface,
    /// and (normally absent) the shared transient banner over everything.
    pub(in crate::app) fn color_picker_view(&self, o: &OutputState) -> Element<'_, Msg> {
        // macOS: the overlay window is created clamped below the menu bar and reframed a
        // frame or two later, so draw nothing until the placement lands (the same guard
        // `overlay_view` carries, and for the same reason).
        #[cfg(target_os = "macos")]
        if !o.placed.get() {
            return widget::space::Space::new().into();
        }
        let mut layers: Vec<Element<'_, Msg>> = Vec::new();
        // The frozen scene, ALWAYS — not gated on the freeze capture extra the way the
        // capture overlay's backdrop is. The picker samples the frozen snapshot, so
        // showing the live desktop underneath would put pixels on screen that are not
        // the pixels it reports (`super::PixelSource`).
        if let Some(bg) = self.frozen_bg_layer(o) {
            layers.push(bg);
        }
        // DRAGON-606: the picker's OWN configured dim (its own setting, not the region
        // one), scaled by the shared fade-in. The picker always grabs the flats, so this is
        // the overlay whose dim most needs to stay off the screen until the grab is done:
        // the picker READS that snapshot, and a dim baked into it would be reported back to
        // the user as the colour they picked. On Windows the fade is additionally held
        // until this overlay is revealed (DRAGON-653, `dim_now_revealed`).
        let dim = self.dim_now_revealed(o, self.color_picker_overlay_opacity);
        layers.push(
            widget::container(widget::space::Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
                .class(cosmic::theme::Container::custom(move |_t| {
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(cosmic::iced::Color {
                            a: dim,
                            ..cosmic::iced::Color::BLACK
                        })),
                        ..Default::default()
                    }
                }))
                .into(),
        );
        if self.color_picker.unavailable {
            layers.push(self.picker_unavailable_layer());
        } else if let Some(h) = self
            .color_picker
            .hover
            .as_ref()
            .filter(|h| h.output == o.name)
        {
            let viewport = o.units().size_to_point(o.logical_size);
            layers.push(self.magnifier_layer(h, o.id, viewport));
            layers.push(self.hex_label_layer(h, viewport));
        }
        // The input surface goes LAST so it is on top and receives the events.
        let output = o.name.clone();
        let pick_output = output.clone();
        layers.push(
            crate::widgets::color_pick::ColorPickSurface::new(
                move |p| Msg::ColorPicker(ColorPickerMsg::Moved(output.clone(), p)),
                move |p| Msg::ColorPicker(ColorPickerMsg::Pick(pick_output.clone(), p)),
                // The wheel / trackpad route into the SAME zoom message the numpad keys use.
                |steps| Msg::ColorPicker(ColorPickerMsg::Zoom(steps)),
            )
            // DRAGON-587: the pointer sprite hides the pixel being sampled, so it goes away.
            // Since DRAGON-597 that is every surface, layer shell included, so this predicate
            // answers true everywhere; it stays as the capability question rather than an
            // unconditional `true` so that dropping the iced [patch] degrades honestly. Two
            // tombstones sit behind it: the ARROW fallback and its one-point sample shift
            // (`color_picker`'s module doc and `geom`), and before that a displaced loupe that
            // parked in a free quadrant, floated tens of points from the pointer and jumped
            // sides at the top and left walls (`geom::disc_view`). The disc is centred on the
            // sample and CLIPS at a screen edge.
            .hide_pointer(crate::platform::overlay_pointer_hideable())
            // DRAGON-610: while the picker has no sample position, a redraw may supply one
            // from the cursor the toolkit is already holding. This is the ONLY correct
            // source for that gate: a flag inside the widget's own `State` re-arms whenever
            // the view's structure changes, which this very view does when the hover layers
            // above are inserted, and a later publish would reset the keyboard nudge.
            .needs_pointer(self.color_picker.pointer.is_none())
            // DRAGON-TBD: the magnifier's cadence IS the display's. While a recorded pointer
            // position has not reached the lens, each presented frame publishes one re-sample;
            // when it has, nothing is published and the picker costs nothing at rest. A fixed
            // timer stood here first and under-sampled a 120Hz panel by half.
            .needs_resample(self.color_picker.resample_due, || {
                Msg::ColorPicker(ColorPickerMsg::ResamplePoll)
            })
            .into(),
        );
        // The shared transient banner. Added by DRAGON-608 for a self-capture decline and
        // INERT at the time, but DRAGON-612 then gave it two writers that only a picker
        // session can reach: the held accept giving up ("the screen never finished loading")
        // and the no-pixel-source refusal ("this display cannot be read"). Both live in
        // `keyboard.rs` and both set `App::toast` expecting THIS to draw it, so removing this
        // layer mutes them, silently and with nothing for the compiler to catch. That nearly
        // happened when the self-capture chord was deleted; the banner outlived its original
        // reason and now belongs to DRAGON-612.
        //
        // AFTER the input surface on purpose, which looks wrong and is not. The capture
        // overlay stacks its own toast above its region surface in exactly the same order
        // (`overlay::overlay_view`) and picking still works there, because a plain
        // `container` captures no pointer events and `stack` passes them down to the surface
        // beneath. Putting it below would let the magnifier's own layers draw over it.
        if let Some(toast) = self.toast_layer() {
            layers.push(toast);
        }
        cosmic::iced::widget::stack(layers).into()
    }

    /// The magnifier disc, placed by [`geom::drawn_disc_origin`] from the LIVE sample point,
    /// and CLIPPED by the screen edge rather than moved or resized by it.
    ///
    /// Its centre is the SAMPLE POINT (DRAGON-587), which is the pointer itself where the sprite
    /// can be hidden and one point up and left of it where the arrow is on screen. Either way
    /// the lens sits on the pointer; the update handler already resolved which, and the disc's
    /// own contents are the same picture.
    ///
    /// DRAGON-650: placed from `h.sample` on every frame, NOT from `h.disc.origin`. The two
    /// agree exactly whenever the raster is fresh (`drawn_disc_origin`'s pinned identity), and
    /// they disagree precisely on the paced frames of a fast sweep, where `h.disc` is the
    /// identity of a raster up to `RASTER_MAX_INTERVAL` old. Placing from the raster there is
    /// what made the lens stand still and then jump 40ms of travel in one step — "skips around
    /// erratically" on a 60Hz panel — while the hex chip (placed from `h.sample` below) glided
    /// ahead of it. The picture inside the lens may lag during a sweep; its position may not.
    ///
    /// One piece now: the pre-rasterised disc, built in the update handler (never in `view` —
    /// re-rasterising per frame would re-upload the texture on every redraw), accent ring and
    /// all.
    ///
    /// DRAGON-587: the ring used to be a second, bordered container stacked over the image,
    /// and the image was placed at a clamped origin at the full diameter. Both had to go.
    /// A clamped origin stops the lens following the pointer at the top and left walls, and
    /// a full-diameter image inside a padded fill container gets less room than it asks for
    /// at the right and bottom walls, where `Image` resolves against the limits and then
    /// contain-fits: the disc was squashed. The raster is now built already clipped
    /// (`geom::disc_view`) and placed at exactly the size it occupies, so there is nothing
    /// left to clamp and nothing to rescale. The ring came along because a widget cannot be
    /// cropped the way a buffer can.
    ///
    /// DRAGON-TBD: two ways to put those same pixels on screen, chosen by
    /// [`super::MagnifierRaster`] (see this module's doc, and that type's, for why). Both are
    /// placed identically, at exactly the size the clipped disc occupies, and both sample
    /// NEAREST: a picker that smooths its cell boundaries is lying about which pixel is
    /// which, so the shader arm asks for it explicitly with [`Layer::nearest`] rather than
    /// inheriting the stack's smoothing default.
    ///
    /// `window` is the OUTPUT's overlay surface, which is what scopes the texture slot: each
    /// display's lens owns its own, so one display's disc can never draw another's pixels.
    fn magnifier_layer<'a>(
        &'a self,
        h: &'a Hover,
        window: window::Id,
        viewport: (f32, f32),
    ) -> Element<'a, Msg> {
        let (w, ht) = (h.disc.size.0 as f32, h.disc.size.1 as f32);
        let disc: Element<'a, Msg> = match &h.magnifier {
            // A software-forced process (dormant since DRAGON-650 exempted the picker from
            // the Windows 10 force; see `MagnifierRaster`). Unchanged from before
            // DRAGON-TBD, deliberately: the software rasterizer draws no shader at all, so
            // this arm is the whole tool there.
            super::MagnifierRaster::Image(handle) => widget::image::Image::new(handle.clone())
                .width(Length::Fixed(w))
                .height(Length::Fixed(ht))
                .filter_method(cosmic::iced::widget::image::FilterMethod::Nearest)
                .into(),
            // Everywhere else: the persistent GPU texture. `live_picker_windows` is the set
            // whose slots this prepare must not reclaim, the picker's answer to the preview
            // editor's `live_preview_windows`.
            super::MagnifierRaster::Layer(frame) => {
                let stack = LayerStack::new(
                    vec![Layer::full(LayerKey::color_magnifier(window), frame.clone()).nearest()],
                    self.live_picker_windows(),
                );
                Element::new(
                    cosmic::iced::widget::shader::Shader::new(stack)
                        .width(Length::Fixed(w))
                        .height(Length::Fixed(ht)),
                )
            }
        };
        // DRAGON-650: derived from the LIVE sample, not read off the raster's identity —
        // see this function's doc and `geom::drawn_disc_origin`.
        let origin = geom::drawn_disc_origin(h.sample, h.disc, viewport);
        absolute(disc, (origin.0 as f32, origin.1 as f32))
    }

    /// The hex chip: the picked colour as its own background, its hex in the ink that
    /// colour can be read against, placed by the pure ladder in [`geom::label_placement`].
    fn hex_label_layer<'a>(&'a self, h: &'a Hover, viewport: (f32, f32)) -> Element<'a, Msg> {
        let radius = geom::MAGNIFIER_DIAMETER as f32 / 2.0;
        // The label is placed against the DISC, and the disc is centred on the sample point
        // (DRAGON-587), so it rides along with the lens rather than with the raw pointer. The
        // label's own ladder still FLIPS near an edge, unlike the disc's clip: a circle cut off
        // by the wall is still a lens, but a hex string cut off by it cannot be read.
        let placement = geom::label_placement(h.sample, LABEL_SIZE, radius, LABEL_GAP, viewport);
        let origin =
            geom::label_origin(placement, h.sample, LABEL_SIZE, radius, LABEL_GAP, viewport);
        let color = h.color;
        let fill = cosmic::iced::Color::from_rgb8(color.r, color.g, color.b);
        let ink = if color.wants_dark_text() {
            cosmic::iced::Color::BLACK
        } else {
            cosmic::iced::Color::WHITE
        };
        // MONOSPACE (DRAGON-598, the owner's report), now BOLD and larger
        // (DRAGON-601). The hex is a fixed-width value read character by character while the
        // pointer moves, and in a proportional face every digit has its own advance, so the
        // string breathes and the whole label jitters under a pointer that is barely moving. A
        // mono face pins each column, so only the characters change. It is still the app's one
        // mono treatment (`theme::mono_font`), not a second font path and not a new asset.
        //
        // `theme::mono_font` rather than `Font::MONOSPACE` because that generic request is not
        // a guarantee: it resolves through ONE family name and silently renders proportional
        // when that name is not installed, which is what macOS and Windows were getting. The
        // helper's own doc carries the measurements.
        //
        // The INK is set on the text widget itself, not left to the chip's `text_color`
        // (DRAGON-601, the owner's "light gray text when hovering white"). The container's
        // `text_color` is only an INHERITED default: anything between it and the glyphs that
        // resolves a colour of its own wins, and the label sits inside a centring container
        // inside a stacked overlay. The border took the ink and the text did not, which is
        // exactly that shape. Setting it at the leaf is also what the rest of this app does
        // (see the toolbar's delay menu), so there is nothing left to inherit through.
        let chip = widget::container(
            widget::container(
                widget::text(color.hex())
                    .size(LABEL_TEXT_SIZE)
                    .font(crate::app::theme::mono_font(true))
                    .class(cosmic::theme::Text::Color(ink)),
            )
            .center_x(Length::Fill)
            .center_y(Length::Fill),
        )
        .width(Length::Fixed(LABEL_SIZE.0))
        .height(Length::Fixed(LABEL_SIZE.1))
        .class(cosmic::theme::Container::custom(move |t| {
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(fill)),
                text_color: Some(ink),
                border: Border {
                    radius: theme::rounding(t).s.into(),
                    width: 1.0,
                    color: ink,
                },
                ..Default::default()
            }
        }));
        absolute(chip.into(), origin)
    }

    /// The honest refusal: this output has no readable pixel source, so the picker says
    /// so instead of showing a magnifier full of guesses. Centred, like the region
    /// overlay's own hint pill.
    fn picker_unavailable_layer(&self) -> Element<'_, Msg> {
        let pill = widget::container(
            widget::text("This display's pixels could not be read, so no color can be picked here.")
                .size(15),
        )
        .padding(cosmic::iced::Padding { top: 10.0, bottom: 10.0, left: 18.0, right: 18.0 })
        .class(cosmic::theme::Container::custom(|theme| {
            let c = theme.cosmic();
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(c.background(false).component.base.into())),
                text_color: Some(c.background(false).component.on.into()),
                border: Border {
                    radius: theme::rounding(theme).m.into(),
                    width: 1.0,
                    color: c.background(false).component.divider.into(),
                },
                ..Default::default()
            }
        }));
        widget::container(pill)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .into()
    }

    // ── The result window ────────────────────────────────────────────────────

    /// The colour picker window (DRAGON-630's reference layout): a CSD header, the
    /// saturation/value square, the controls row (pipette, round current-colour swatch,
    /// hue and alpha strips), one value row of per-component boxes with the mode
    /// stepper, and the recent-colours strip.
    ///
    /// Nothing SCROLLS. The window is sized from these exact parts
    /// ([`geom::color_window_size`]), so a scrollbar would only ever mean the sizing
    /// function had drifted from the layout.
    pub(in crate::app) fn color_picker_window_view(&self) -> Element<'_, Msg> {
        let focused = self.core.focused_window() == self.color_picker.window;
        let header = widget::header_bar()
            .focused(focused)
            .on_drag(Msg::WindowChrome(WindowChromeMsg::ColorPickerWindowDrag));
        // DRAGON-676: the title is CENTRED only where something else owns the top-left
        // corner, and only macOS does. There the native traffic lights are drawn OVER our
        // header at exactly that corner (the window opens with a transparent titlebar, see
        // `open_color_picker_window`), so a flush-left title would sit under them. Windows
        // and Linux have nothing there: Windows' native caption cluster is top-RIGHT
        // (DRAGON-284, and `WIN_CAPTION_INSET` below already reserves its width at the END
        // of the header), and Linux draws our own close/minimize at the same end. So on
        // both of those the title goes where a window title belongs, flush left.
        //
        // Mechanically, left-aligning is not a flag on the title: `header_bar`'s `title`
        // IS its CENTER region (libcosmic builds it there as `widget::text::heading` and
        // centres it between the start and end slots). So the same heading goes in the
        // START region instead and the title is left unset, which is why the two arms look
        // like different calls rather than one call with a parameter.
        #[cfg(target_os = "macos")]
        let header = header.title(WINDOW_TITLE);
        #[cfg(not(target_os = "macos"))]
        let header = header.start(header_title());
        // DRAGON-649: minimize, but NEVER maximize. The window is deliberately
        // fixed-size (`min_size == max_size`, see `open_color_picker_window`), so a
        // maximize button would offer an operation the window refuses; minimize is the
        // one window control left worth having. The same asymmetry already holds on
        // macOS, where `MacPinWindow` disables the native zoom button and leaves the
        // native minimize alone.
        //
        // Who draws the buttons, per platform:
        // - macOS: the native traffic lights carry close and minimize (the window opens
        //   with a transparent titlebar over our header), so no CSD buttons at all.
        // - Windows 11: the finalize path installs the native DWM caption cluster
        //   (`install_native_caption_buttons`), which owns minimize/close top-right, so
        //   the CSD buttons are OMITTED and a trailing spacer reserves the cluster's
        //   width, exactly as the settings and preview headers do. (The CSD close this
        //   header used to draw unconditionally sat under that cluster.)
        // - Windows 10: the cluster hit-tests but never paints (DRAGON-403), so the CSD
        //   close + minimize render instead; minimize routes to the native helper
        //   because iced's `window::minimize` is a no-op for a frameless toplevel.
        // - Linux (and any other CSD platform): the app paints its own captions, so
        //   close and minimize are both ours.
        // DRAGON-682: the panel's expand/collapse toggle, in the header's END
        // region. That is the MIRROR of the settings window's nav toggle, which sits in its
        // header's START region, and it is where "along the right edge" can live without
        // moving a single point of the picker's own column: the header is above the
        // content, so a button in it costs the layout below nothing. Every other spot on
        // the right edge is inside the window's 16pt padding, which a 24pt control does not
        // fit in, and taking the width out of the content would move exactly what the owner
        // said must not move.
        //
        // It is added FIRST, so it sits inboard of whatever caption buttons the platform
        // puts at that end: Linux's own CSD close and minimize, or Windows 10's. On macOS
        // the traffic lights are at the START, so this end is the toggle's alone.
        //
        // **Except Windows with the native caption cluster** (DRAGON-685): there the
        // header's end region cannot put the toggle where the owner wants it. The end row
        // ends at the reserved `WIN_CAPTION_INSET` spacer plus the row's own spacing and
        // the header's trailing padding, which parked the icon ~25pt short of the
        // cluster's bounds, and the row centres its children in the 44pt header while the
        // native glyphs centre on their own ~15pt caption line, so the icon also sat ~8pt
        // low. On that path the toggle FLOATS in its own stack layer instead (see the
        // `layers` block below), pinned beside the cluster on its centreline, and the
        // header keeps only the spacer, which still exists so the TITLE can never slide
        // under the native buttons.
        #[cfg(windows)]
        let native_captions =
            crate::platform::windows::caption::native_caption_buttons_supported();
        #[cfg(windows)]
        let header = if native_captions {
            header.end(
                widget::space::Space::new()
                    .width(Length::Fixed(crate::app::settings::WIN_CAPTION_INSET)),
            )
        } else {
            header
                .end(panel_toggle(self.color_picker.expanded))
                .on_close(Msg::WindowChrome(WindowChromeMsg::Close))
                .on_minimize(Msg::WindowChrome(WindowChromeMsg::ColorPickerWindowMinimize))
        };
        #[cfg(target_os = "macos")]
        let header = header.end(panel_toggle(self.color_picker.expanded));
        #[cfg(all(not(target_os = "macos"), not(windows)))]
        let header = header
            .end(panel_toggle(self.color_picker.expanded))
            .on_close(Msg::WindowChrome(WindowChromeMsg::Close))
            .on_minimize(Msg::WindowChrome(WindowChromeMsg::ColorPickerWindowMinimize));

        // DRAGON-630's reference layout, as revised by DRAGON-680, top to bottom: the
        // saturation/value square, the controls row (pipette, COPY, round swatch, hue +
        // alpha strips), the value row (the boxes with the mode STEPPER at their right
        // end, then their captions), a hairline divider carrying the Add to recents button,
        // and the two-row colour history. The gaps are EXPLICIT spacers rather than one
        // column spacing, because the owner sized them individually on review (wider
        // air around the controls row, ordinary gaps around the divider).
        //
        // There is no MODE ROW between the controls and the boxes any more; the window is
        // 40pt shorter for it. `App::value_row`'s doc carries what was on it and where
        // each part went.
        let items: Vec<Element<'_, Msg>> = vec![
            self.sv_square(),
            vspace(geom::GAP_SQUARE_CONTROLS),
            self.controls_row(),
            vspace(geom::GAP_CONTROLS_VALUE),
            self.value_row(),
            // The two gaps around the divider differ by design: see
            // `geom::GAP_VALUE_DIVIDER`, which gives back the empty descender space the
            // caption band above the line carries and the swatches below it do not.
            vspace(geom::GAP_VALUE_DIVIDER),
            divider_band(),
        ];

        // **How the history's focus frame is paid for** (DRAGON-680, the owner's veto of
        // a frame that clipped the swatch rims). The frame sits OUTSIDE the grid by
        // `HISTORY_FOCUS_OUTSET` on all four sides, and that space comes out of margins
        // that were already blank rather than out of the grid:
        //
        // * the window's padding drops by the outset on the left, the right and the
        //   BOTTOM, and every section above the history gets that much back as its own
        //   horizontal padding, so nothing above moves by a single point;
        // * the gap above the history drops by the outset, and the frame's own top inset
        //   gives it back.
        //
        // The sums are unchanged in both axes, which is why `geom::color_window_size` did
        // not move; `geom`'s `the_history_frame_is_paid_for_by_the_margins_it_sits_in`
        // pins exactly that, and the const asserts beside the constant keep the outset
        // inside the margins it spends.
        let upper = widget::container(widget::column(items).width(Length::Fill))
            .padding([0.0, geom::HISTORY_FOCUS_OUTSET])
            .width(Length::Fill);
        let body = widget::column(vec![
            upper.into(),
            vspace(geom::SECTION_GAP - geom::HISTORY_FOCUS_OUTSET),
            self.recent_colors_grid(),
        ])
        .width(Length::Fill);

        // FIXED width, not `Fill` (DRAGON-682): in the collapsed window this is the whole
        // content area and nothing changes, and in the expanded one it is what keeps the
        // picker's column exactly where it was instead of stretching it across the new
        // half and dragging every row with it. The owner's requirement is that expanding
        // moves nothing, and this is where that is enforced.
        let content = widget::container(body)
            .padding(cosmic::iced::Padding {
                top: geom::WINDOW_PADDING,
                right: geom::WINDOW_PADDING - geom::HISTORY_FOCUS_OUTSET,
                bottom: geom::WINDOW_PADDING - geom::HISTORY_FOCUS_OUTSET,
                left: geom::WINDOW_PADDING - geom::HISTORY_FOCUS_OUTSET,
            })
            .width(Length::Fixed(geom::picker_column_w()))
            .height(Length::Fill);
        // The panel is drawn when the flag says so, and that is the whole rule (item 42).
        let body_row: Element<'_, Msg> = if self.color_picker.panel_mounted() {
            // Two FIXED columns, side by side. The row is `Fill` only so the pair sits at
            // the left edge of whatever surface exists; neither child is elastic, so a
            // surface that is momentarily the wrong size leaves BACKGROUND, never a
            // squeezed column. This much SURVIVED item 42's removal: it is ordinary layout
            // with no timing in it, and making either column elastic again would reintroduce
            // reflow to buy nothing.
            widget::row(vec![content.into(), self.side_panel()])
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        } else {
            content.into()
        };

        let stacked = widget::column(vec![header.into(), body_row])
            .width(Length::Fill)
            .height(Length::Fill);

        // The frosted-window recipe, copied from `permissions::view` rather than invented:
        // the surface is enrolled in the platform's blur at creation (compositor blur on
        // Linux, the masked vibrancy view on macOS, DWM Mica on Windows) and this outer
        // container has to paint TRANSLUCENT for any of it to show. Painting an opaque
        // background here is exactly how a window ends up correctly enrolled and visibly
        // flat, which the owner warned is the usual first-try mac mistake.
        let glass = self.glass;
        // The GHOST rides above everything, in its own stack layer (DRAGON-682 item 35).
        //
        // The layer is not decoration: within one layer iced draws every QUAD before every
        // IMAGE, so a ghost in the same layer as the window's rasters would be painted
        // UNDER them. `widget::stack` wraps each child after the first in
        // `renderer::with_layer`, which is the one mechanism this app has for drawing over
        // a picture, and the pinned cards already rely on it.
        let mut layers: Vec<Element<'_, Msg>> = vec![stacked.into()];
        // DRAGON-685: with the native caption cluster, the panel toggle floats HERE, in
        // its own layer pinned to the window's top-right, rather than riding the header's
        // end region (whose spacing and vertical centring cannot land it beside the
        // cluster — the header comment above carries the numbers). The layer is
        // whole-window `Fill` with the button alone in its top-right corner, so events
        // everywhere else fall through to the header drag and the content below it.
        #[cfg(windows)]
        if native_captions {
            layers.push(
                widget::container(panel_toggle(self.color_picker.expanded))
                    .align_x(cosmic::iced::alignment::Horizontal::Right)
                    .align_y(cosmic::iced::alignment::Vertical::Top)
                    .padding(cosmic::iced::Padding {
                        // The cluster's reserved bounds start `WIN_CAPTION_INSET` from the
                        // window's right edge; a small gap keeps the button's box out of
                        // the region `DwmDefWindowProc` hit-tests, so its clicks can never
                        // be swallowed as caption clicks. The stack sits inside the 1pt
                        // window border, which both paddings give back.
                        right: crate::app::settings::WIN_CAPTION_INSET
                            + geom::WIN_TOGGLE_CLUSTER_GAP
                            - geom::WINDOW_BORDER,
                        // Lands the 24pt button's centre on the native glyphs' own
                        // centreline (measured ~15pt from the window top at 100%), not the
                        // 44pt header's.
                        top: geom::WIN_TOGGLE_CENTERLINE - geom::PANEL_TOGGLE_W / 2.0
                            - geom::WINDOW_BORDER,
                        bottom: 0.0,
                        left: 0.0,
                    })
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into(),
            );
        }
        // The ZONE HIGHLIGHT goes UNDER the ghost and over the content (item 41): it is
        // about the region, and the thing the pointer is carrying has to stay on top of it.
        layers.extend(self.drop_zone_highlight());
        layers.extend(self.drag_ghost());
        // THE ROOT CHAIN'S SHAPE IS FIXED, drag or no drag (the DRAGON-687 drag-jump
        // root cause). This used to unwrap a single-layer stack to its bare child and
        // wrap `force_cursor` in only while dragging, so the chain above the panel's
        // scrollable CHANGED SHAPE the frame a drag went live. iced diffs the widget
        // tree positionally with mostly-stateless tags, so the inserted level shifted
        // every descendant's tree node one step out of alignment, the scrollable's
        // state node stopped matching, and its offset was rebuilt AT ZERO: press a
        // bottom-scrolled group, cross the threshold, and the list jumped to the top.
        // The arming latch and the exchange no-op were both innocent; the writer was
        // the toolkit rebuilding mis-aligned state. So the stack is ALWAYS a stack
        // (extra layers only ever APPEND, so child 0, the stateful subtree, keeps its
        // slot) and the cursor wrapper is ALWAYS present with only its VALUE
        // conditional (`force_cursor_maybe`; item 40's closed hand while a drag is
        // live, pass-through otherwise). Do not "simplify" either back to a
        // conditional wrapper: the shape IS the fix.
        let stacked: Element<'_, Msg> = cosmic::iced::widget::stack(layers)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let stacked: Element<'_, Msg> = crate::widgets::force_cursor::force_cursor_maybe(
            stacked,
            root_cursor(self.color_picker.dragging()),
        );
        let window: Element<'_, Msg> = widget::container(stacked)
            // COUNTED in `geom::color_window_size` as `WINDOW_BORDER`: this padding is
            // real content width the sections below never got, and every one of them is
            // laid out at exactly CONTENT_W.
            .padding(geom::WINDOW_BORDER)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(cosmic::theme::Container::custom(move |theme| {
                let cosmic = theme.cosmic();
                // Linux draws its own window edge, so it rounds. macOS and Windows are
                // rounded by the window server / DWM, and rounding here too paints a
                // second, mismatched corner just inside theirs.
                #[cfg(target_os = "linux")]
                let radius = theme::rounding(theme).window();
                #[cfg(not(target_os = "linux"))]
                let radius = [0.0f32; 4];
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(theme::frost_color(
                        cosmic.background(false).base.into(),
                        glass,
                    ))),
                    border: Border {
                        color: cosmic.bg_divider().into(),
                        #[cfg(target_os = "macos")]
                        width: 0.0,
                        #[cfg(not(target_os = "macos"))]
                        width: 1.0,
                        radius: radius.into(),
                    },
                    ..Default::default()
                }
            }))
            .into();
        // The WINDOW-LEVEL pointer report (DRAGON-687, the pencil's second stranding):
        // one root mouse_area whose region is the whole window, so motion is reported
        // wherever the pointer is and a region exit cannot starve the source the way
        // the scrolled content's own area was starved (moving up off the first title
        // left it silent forever). `on_move` only, plus a best-effort leave: children
        // still capture their own presses first, and a position anywhere outside the
        // palettes' scroll viewport maps to no title by construction
        // (`geom::hovered_palette_title_at`).
        let window: Element<'_, Msg> = widget::mouse_area(window)
            .on_move(|p| Msg::ColorPicker(ColorPickerMsg::WindowPointerMoved(p.x, p.y)))
            .on_exit(Msg::ColorPicker(ColorPickerMsg::WindowPointerLeft))
            .into();
        // The DELETE-PALETTE confirmation (DRAGON-687), stacked over the whole window in
        // the app's ONE modal shape (`settings::dialog_layers`: scrim, drag strip,
        // centred card): the owner's "these should get a confirmation to delete before
        // deleting", for both delete gestures. The backdrop DISMISSES, the reset
        // dialog's own rule: backing out of a destructive question is always safe.
        //
        // APPENDED to a permanent outer stack rather than wrapped by `stack_dialog`,
        // for the drag-jump root cause's exact reason (the layer block above): a root
        // that becomes a different widget when the dialog opens mis-aligns every
        // stateful descendant's tree node, and the panel's scroll offset dies in the
        // rebuild. The outer stack is always there; the dialog only ever appends.
        let mut outer: Vec<Element<'_, Msg>> = vec![window];
        if let Some(g) = self.color_picker.pending_group_delete {
            let name = self
                .color_picker
                .palettes
                .get(g)
                .map_or_else(|| "this palette".to_string(), |p| format!("\"{}\"", p.name));
            let n = self.color_picker.palettes.get(g).map_or(0, |p| p.colors.len());
            let colors = match n {
                1 => "its 1 color".to_string(),
                n => format!("its {n} colors"),
            };
            let card = widget::dialog()
                .title("Delete this palette?")
                .body(format!("{name} and {colors} will be removed. This cannot be undone."))
                .primary_action(crate::widgets::arrow_cursor::arrow_cursor(
                    widget::button::destructive("Delete")
                        .on_press(Msg::ColorPicker(ColorPickerMsg::ConfirmDeleteGroup(true))),
                ))
                .secondary_action(crate::widgets::arrow_cursor::arrow_cursor(
                    widget::button::text("Cancel")
                        .on_press(Msg::ColorPicker(ColorPickerMsg::ConfirmDeleteGroup(false))),
                ));
            outer.extend(crate::app::settings::dialog_layers(
                card.into(),
                Some(Msg::ColorPicker(ColorPickerMsg::ConfirmDeleteGroup(false))),
                // The strip drags THIS window, not the settings one.
                Msg::WindowChrome(WindowChromeMsg::ColorPickerWindowDrag),
            ));
        }
        cosmic::iced::widget::stack(outer)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// The DROP ZONE the pointer is over, lit up (DRAGON-682 item 41).
    ///
    /// A half-transparent accent wash with a dashed accent line around its boundary, over
    /// exactly the region a drop would land in. Only a zone that is VALID for the source
    /// being dragged ever lights, and only one at a time: `geom::zone_highlight` asks
    /// `geom::drop_action`, the same table the drop itself reads, so a lit region that does
    /// nothing (or a drop with no light) cannot happen.
    ///
    /// The dashed line is a raster because a quad cannot dash, exactly as the empty history
    /// slots' dotted outline is (`color::dashed_outline_rgba` is that function's bigger
    /// sibling). It is built when the highlight MOVES, in the update handler, not here.
    fn drop_zone_highlight(&self) -> Option<Element<'_, Msg>> {
        let cp = &self.color_picker;
        let drag = cp.drag.as_ref().filter(|d| d.live)?;
        let zone = drag.zone?;
        let shape = cp.panel_shape();
        let window = cp.window_size();
        // The two REORDER highlights are INSERTION LINES, not washes (DRAGON-687): a
        // dashed box around the bar would say "append" while the drop means "put it
        // here". Analytic quads derived from the live position every frame, never a
        // raster, because the slot moves per pixel.
        match (drag.source, zone) {
            (geom::DragSource::PaletteSwatch(g, _), geom::DropZone::PaletteGroup(to_g))
                if g == to_g =>
            {
                let n = shape.groups.get(g).copied().unwrap_or(0);
                let slot = geom::palette_color_slot(drag.at, n);
                // The zone rect is the WHOLE group since the owner widened it (title row
                // plus bar); the insertion line still belongs to the BAR band alone, so
                // it starts a title row and gap below the rect's (possibly clipped) top.
                let (bx, by, _, bh) = geom::zone_rect(zone, window, &shape);
                let bar_off = geom::PALETTE_TITLE_ROW_H + geom::PANEL_HEADING_GAP;
                let line_h = (bh - bar_off).min(geom::PANEL_SWATCH);
                if line_h <= 0.0 {
                    return None;
                }
                let x = bx + geom::palette_insert_line_x(slot, n);
                return Some(crate::widgets::positioned::positioned(
                    insert_line(2.0, line_h),
                    (x - 1.0 - geom::WINDOW_BORDER, by + bar_off - geom::WINDOW_BORDER),
                ));
            }
            (geom::DragSource::PaletteName(_), _) => {
                let slot = geom::palette_group_slot(drag.at, &shape);
                let y = geom::palettes_scroll_top() - shape.scroll
                    + geom::palette_group_line_y(slot, shape.groups.len());
                // Clipped to the scroll viewport, like the group highlights themselves.
                if y < geom::palettes_scroll_top() || y > geom::panel_scroll_bottom(window.1) {
                    return None;
                }
                let x = geom::WINDOW_BORDER + geom::picker_column_w() + geom::WINDOW_PADDING;
                return Some(crate::widgets::positioned::positioned(
                    insert_line(geom::bar_w(), 2.0),
                    (x - geom::WINDOW_BORDER, y - 1.0 - geom::WINDOW_BORDER),
                ));
            }
            _ => {}
        }
        let (x, y, w, h) = geom::zone_rect(zone, window, &shape);
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        let fill = widget::container(widget::space::Space::new())
            .width(Length::Fixed(w))
            .height(Length::Fixed(h))
            .class(cosmic::theme::Container::custom(move |t| {
                let accent = theme::accent(t);
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(cosmic::iced::Color {
                        a: geom::ZONE_HIGHLIGHT_FILL_ALPHA,
                        ..accent
                    })),
                    border: Border {
                        radius: geom::ZONE_HIGHLIGHT_RADIUS.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }));
        // The wash and its dashed edge are one stacked unit, so they can be placed together.
        let region = cosmic::iced::widget::stack(vec![
            fill.into(),
            raster_image(cp.zone_raster.as_ref().map(|(_, handle)| handle), w, h),
        ])
        .width(Length::Fixed(w))
        .height(Length::Fixed(h));
        Some(crate::widgets::positioned::positioned(
            region,
            (x - geom::WINDOW_BORDER, y - geom::WINDOW_BORDER),
        ))
    }

    /// The thing that follows the pointer while a drag is live (DRAGON-682 item 35).
    ///
    /// A HISTORY swatch, whatever it was dragged from, which is the owner's ask ("a small
    /// swatch that is the same shape as the swatches in the recent history area"): the same
    /// size, the same corner token, the same subdued rim, and for a translucent colour the
    /// same split raster with its checkerboard. It is drawn from the same
    /// [`swatch_radius`] / [`swatch_rim`] pair the real swatches read, so a theme change
    /// moves the ghost with them.
    ///
    /// `None` whenever there is nothing to draw: no drag, an armed press that has not
    /// travelled, or a source that has gone away.
    fn drag_ghost(&self) -> Option<Element<'_, Msg>> {
        let cp = &self.color_picker;
        let drag = cp.drag.as_ref().filter(|d| d.live)?;
        // A GROUP NAME's ghost is the name itself, as a small floating pill (DRAGON-687):
        // a name drag carries no colour, and a black square under the pointer would say
        // it does. The tooltip dress, because that is this window's one floating-card
        // look.
        if let geom::DragSource::PaletteName(g) = drag.source {
            let name = cp.palettes.get(g)?.name.clone();
            let pill = widget::container(widget::text(name).size(12))
                .padding([4, 8])
                .class(cosmic::theme::Container::Tooltip);
            let (gx, gy) = geom::ghost_origin(drag.at);
            return Some(crate::widgets::positioned::positioned(
                pill,
                (gx - geom::WINDOW_BORDER, gy - geom::WINDOW_BORDER),
            ));
        }
        // The payload, captured at the press and never re-resolved (item 41): the ghost is
        // the swatch that was picked up, not whatever is under the pointer now.
        let (c, alpha) = drag.payload;
        let d = geom::DRAG_GHOST;
        let painted = cp.drag_raster.is_some() && alpha != u8::MAX;
        let fill = cosmic::iced::Color::from_rgb8(c.r, c.g, c.b);
        let face = widget::container(match cp.drag_raster.as_ref() {
            Some(handle) if alpha != u8::MAX => raster_image(Some(handle), d, d),
            _ => widget::space::Space::new().into(),
        })
        .width(Length::Fixed(d))
        .height(Length::Fixed(d))
        .class(cosmic::theme::Container::custom(move |t| {
            let (width, color) = swatch_rim(t);
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(if painted {
                    cosmic::iced::Color::TRANSPARENT
                } else {
                    fill
                })),
                border: Border {
                    radius: swatch_radius(t).into(),
                    width,
                    color,
                },
                ..Default::default()
            }
        }));
        // Centred on the pointer, and NEVER clamped (item 41, the owner: "we can't drag a
        // swatch above the window or beyond the left edge"). `absolute` cannot express this:
        // it places by padding, and padding has no negative half, so the ghost stopped dead
        // at two of the four edges while clipping cleanly at the other two.
        // `widgets::positioned` places by LAYOUT instead, so the ghost leaves the surface the
        // same way in every direction and the surface clips it.
        //
        // The one correction: this stack sits INSIDE the window's frosted border, so its
        // origin is one `WINDOW_BORDER` in from the pointer's own window coordinates.
        let (gx, gy) = geom::ghost_origin(drag.at);
        Some(crate::widgets::positioned::positioned(
            face,
            (gx - geom::WINDOW_BORDER, gy - geom::WINDOW_BORDER),
        ))
    }

    /// The saturation/value square (DRAGON-630): the current hue's gradient under a
    /// draggable marker at the tracked S/V. The raster is built in the update handler
    /// (`refresh_sv_raster`) and only re-built when the HUE moves; dragging inside the
    /// square moves only the marker and the derived colour.
    fn sv_square(&self) -> Element<'_, Msg> {
        let cp = &self.color_picker;
        let content = raster_image(cp.sv_raster.as_ref(), geom::CONTENT_W, geom::SV_H);
        // The marker speaks the field's units: x is saturation, y runs down while
        // value runs up. The same inversion `SvChanged` applies, so the marker sits
        // exactly where the last drag put it.
        let marker = (cp.hsv[1] as f32, 1.0 - cp.hsv[2] as f32);
        // The ring's ink flips black/white with the colour UNDER it (the owner's spec),
        // which is exactly the colour the marker is parked on. Same crossover as the
        // hex chip's ink (`Srgb::wants_dark_text`), so one rule decides both.
        let ink = if cp.color.wants_dark_text() {
            cosmic::iced::Color::BLACK
        } else {
            cosmic::iced::Color::WHITE
        };
        let fill = cosmic::iced::Color::from_rgb8(cp.color.r, cp.color.g, cp.color.b);
        crate::widgets::color_field::ColorField::new(
            content,
            marker,
            crate::widgets::color_field::FieldAxis::Both,
            |x, y| Msg::ColorPicker(ColorPickerMsg::SvChanged(x, y)),
        )
        .marker_style(move |_t| crate::widgets::color_field::MarkerStyle {
            // FILLED with the selected colour (the owner's review; it was hollow): the
            // ring is a little swatch of exactly what a pick here means.
            fill: Some(fill),
            border: ink,
            border_width: 2.0,
            // The owner's "very faint" drop shadow, just enough to lift the ring off
            // the gradient when the ink and the colour run close.
            shadow: cosmic::iced::core::Shadow {
                color: cosmic::iced::Color { a: 0.18, ..cosmic::iced::Color::BLACK },
                offset: cosmic::iced::core::Vector::new(0.0, 1.0),
                blur_radius: 2.0,
            },
        })
        .into()
    }

    /// The controls row: the pick-again pipette, the COPY button beside it (DRAGON-680),
    /// the round current-colour swatch (checkerboard showing through a translucent
    /// alpha), and the stacked hue and alpha strips.
    ///
    /// **The COPY button is not here** (DRAGON-680 item 23). It joined this row mid-ticket,
    /// beside the pipette, and the owner then moved it on to the head of the value row
    /// where the thing it copies is. What that leaves is the row's original shape with the
    /// longest tracks the picker has had: every point the button and its gap were using
    /// went back to [`geom::STRIPS_W`], 164 to 268.
    fn controls_row(&self) -> Element<'_, Msg> {
        let cp = &self.color_picker;
        // The round swatch is a DRAG SOURCE (DRAGON-682 item 36): it reports the pointer's
        // comings and goings so a press can know it was picked up, and wears the grab hand
        // to say so. It gains no click behaviour: it had none, and the owner asked for none.
        // Since DRAGON-687 item seven it also answers a RIGHT press with its own menu (a
        // right press arms no drag: only `on_press`, the primary button, names a source)
        // and wears the shared adaptive hex card on hover.
        let swatch = crate::widgets::force_cursor::force_cursor(
            widget::mouse_area(swatch_disc(cp.swatch_raster.as_ref()))
                .on_press(Msg::ColorPicker(ColorPickerMsg::DragPressed(
                    geom::DragSource::Active,
                )))
                .on_right_press(Msg::ColorPicker(ColorPickerMsg::MainSwatchMenu(true))),
            cosmic::iced::mouse::Interaction::Grab,
        );
        // Menu, else hover card, never both (`widgets::copy_button`'s tombstone rule),
        // and a live drag silences the card like every other swatch's (item 35).
        let swatch: Element<'_, Msg> = if cp.main_menu {
            main_swatch_menu(
                swatch,
                cp.color,
                cp.alpha,
                &cp.palettes,
                cp.menu_page,
                cp.window_size(),
            )
        } else if cp.dragging() {
            swatch
        } else {
            hover_tip(swatch, swatch_hex_tip(cp.color, cp.alpha), cp.color)
        };
        let hue = gradient_strip(
            cp.hue_raster.as_ref(),
            (cp.hsv[0] / 360.0) as f32,
            |x, _| Msg::ColorPicker(ColorPickerMsg::HueChanged(x)),
        );
        let alpha = gradient_strip(
            cp.alpha_raster.as_ref(),
            cp.alpha as f32 / 255.0,
            |x, _| Msg::ColorPicker(ColorPickerMsg::AlphaChanged(x)),
        );
        let strips = widget::column(vec![hue, alpha]).spacing(geom::STRIP_GAP);
        // Explicit spacers: the swatch-to-tracks gap is wider than the pipette-to-swatch
        // one (the owner's review), so one row spacing cannot express the row.
        widget::row(vec![
            pick_again_button(),
            hspace(geom::ROW_SPACING),
            swatch,
            hspace(geom::GAP_SWATCH_TRACKS),
            strips.into(),
        ])
        .align_y(Alignment::Center)
        .height(Length::Fixed(geom::CONTROLS_H))
        .into()
    }

    /// The value row (DRAGON-630, rebuilt by DRAGON-680), TWO BANDS stacked:
    ///
    /// 1. the BOX band: the COPY button leading the row (DRAGON-680 item 23), then the
    ///    current mode's value boxes, then the MODE ACTIVATOR (an up chevron over a down
    ///    one, one control, opening the notation menu) at the right end. The two controls
    ///    centre on the band, which is the copy button's own 48pt height;
    /// 2. the CAPTION band: "R", "G", "B", "A", its cells the same widths and gaps as the
    ///    boxes above and offset by the leader, so each caption lands under its own box.
    ///
    /// **Which boxes is a property of the MODE, not a remembered toggle**
    /// ([`geom::splits_components`], the owner's DRAGON-680 rule): hex is ONE unified box
    /// holding `#RRGGBBAA` exactly as the copy button would take it, and every other
    /// notation is one box per component plus the shared alpha box. Both layouts span the
    /// same budget, so the stepper beside them never moves when the mode changes.
    ///
    /// Each box shows the live DRAFT while it is the one being typed into, and the
    /// canonical spelling otherwise, so a half-typed value is never rewritten under the
    /// caret (see `ColorPickerState::draft`). Focus and Tab: each box carries a stable id
    /// (`ColorPickerState::box_id`) and `select_on_focus`, so arriving at one selects its
    /// whole value, and the window focuses the first box on open and after a mode is chosen
    /// from the menu. Tab then walks the window's OWN ring (`geom::next_focus`: the boxes,
    /// the activator, the history), which it has to, because libcosmic's blanket keyboard
    /// navigation visits every `operation.focusable` widget and a cosmic BUTTON is one.
    ///
    /// **Tombstone: the MODE ROW.** There was a third band above the boxes, carrying the
    /// mode dropdown, the split-inputs toggle and the copy button. All three are gone
    /// (DRAGON-680): the toggle was deleted with the setting behind it, the copy button
    /// moved into the controls row at the pipette's size, and the dropdown's CHIP became
    /// the bare chevron activator beside the boxes (its MENU is unchanged and still opens
    /// upward, see [`mode_activator`]). So did the "Copied!" word that lived in the room
    /// the dropdown's DRAGON-676 shrink had freed: with the button one row up and wearing
    /// its own success tick, a word on a row that no longer exists had no home worth
    /// inventing, and the tick is the acknowledgement (see `widgets::copy_button`'s
    /// tombstone, which records the same decision from the other side).
    ///
    /// **What survives from that design is the BAND rule**, and it is worth keeping: within
    /// a band the boxes are the tallest thing and their neighbours centre on THEM, never on
    /// [`geom::VALUE_BOX_H`]. A text input measures its font's line height plus its own
    /// padding, which is a shade over that constant at the default scale and further out at
    /// a larger one, so anything given a fixed height beside one sits a point or two high.
    fn value_row(&self) -> Element<'_, Msg> {
        let cp = &self.color_picker;
        // The boxes and their captions are collected as two PARALLEL lists, one cell
        // each, so the two bands line up column by column without either one being nested
        // inside the other.
        let mut inputs: Vec<Element<'_, Msg>> = Vec::new();
        let mut captions: Vec<Element<'_, Msg>> = Vec::new();
        if geom::splits_components(cp.mode) {
            // Per-box widths, not one width times the count: the boxes share their whole
            // budget, so the floor()'s remainder is handed out a point at a time and the
            // row lands flush on both edges (`geom::value_box_widths`).
            let widths = geom::value_box_widths(cp.box_count());
            let mut labels = cp.mode.component_labels().to_vec();
            labels.push("A");
            // Zipped, so the widths and the labels cannot disagree about the count even
            // if `box_count` and `component_labels` ever drift apart.
            for (i, (label, bw)) in labels.iter().zip(widths).enumerate() {
                inputs.push(value_box(cp, i, i, bw));
                captions.push(caption_cell(label, bw));
            }
        } else {
            // Hex: ONE box holding the whole spelling, alpha included. Its draft index is
            // the WHOLE_VALUE_BOX sentinel (the handlers parse it with `parse_with_alpha`
            // rather than as one component) while its POSITION is 0, which is what makes
            // "focus the first box" mean the same thing in every mode.
            let w = geom::value_whole_width();
            inputs.push(value_box(cp, crate::app::color_picker::WHOLE_VALUE_BOX, 0, w));
            captions.push(caption_cell(cp.mode.label(), w));
        }
        // A FIXED-width frame around whichever layout is showing, so the two lay out
        // identically and the band is the same width either way. The caption band takes
        // the same frame, so the two stay aligned to the same left and right edges.
        let boxes = widget::container(widget::row(inputs).spacing(geom::BOX_GAP))
            .width(Length::Fixed(geom::value_whole_width()));
        let captions = widget::container(widget::row(captions).spacing(geom::BOX_GAP))
            .width(Length::Fixed(geom::value_whole_width()));
        // The flash the copy button wears (`copy_button::copied_recently`, a window that
        // closes by the clock).
        let copied = cp
            .copied
            .is_some_and(|(_, at)| crate::widgets::copy_button::copied_recently(Some(at)));
        // Band 1: the COPY button, the boxes, then the mode activator (DRAGON-680 item 23,
        // the owner: "move the copy button to instead be the start of the input row").
        // `align_y(Center)` centres the boxes and the activator against the tallest thing
        // on the row, which is now the 48pt button rather than a 34pt box, so the band is
        // `geom::BOX_BAND_H` and the window is 14pt taller for it. There is no honest way
        // to put a 48pt control in a 34pt row, and both alternatives were worse: a smaller
        // button is what the owner ruled out ("still same size"), and letting it overdraw
        // its row would put it over the caption band below.
        let box_band = widget::row(vec![
            copy_value_button(copied),
            hspace(geom::ROW_SPACING),
            boxes.into(),
            hspace(geom::ROW_SPACING),
            mode_activator(
                cp.mode,
                cp.mode_menu_open,
                cp.focus == Some(geom::PickerFocus::Mode),
            ),
        ])
        .align_y(Alignment::Center)
        .height(Length::Fixed(geom::BOX_BAND_H))
        .width(Length::Fill);
        // Band 2: the captions, tucked under the boxes they name. They are OFFSET by the
        // leader's width plus its gap, or every letter would sit under the wrong box: the
        // caption band is a parallel list of cells at the boxes' own widths, and the boxes
        // no longer start at the row's left edge.
        let captions = widget::row(vec![
            hspace(geom::CONTROLS_BUTTON + geom::ROW_SPACING),
            captions.into(),
        ]);
        widget::column(vec![
            box_band.into(),
            vspace(geom::VALUE_LABEL_GAP),
            captions.into(),
        ])
        .width(Length::Fill)
        .into()
    }

    /// The side PANEL (DRAGON-682): the window's second half, present only
    /// while it is expanded.
    ///
    /// A settings-shaped surface, deliberately: a tab strip pinned at the top and a
    /// SCROLLABLE body under it, which is the settings window's own composition
    /// (`settings::mod`'s head plus `scroll_body`). The picker's own column still scrolls
    /// nothing and is still sized to the point; this half is the one place in the window
    /// where content can outgrow the frame, and it must, because the Harmonies tab's card
    /// list is longer than any window we would want to open.
    fn side_panel(&self) -> Element<'_, Msg> {
        let cp = &self.color_picker;
        let body: Element<'_, Msg> = match cp.panel_tab {
            geom::PanelTab::Harmonies => self.harmony_groups(),
            geom::PanelTab::Palettes => self.palettes_tab(),
        };
        // The tab strip takes the SAME right inset the scrolled content does (item 16), so
        // the strip and the cards share one right edge and neither runs under the scrollbar.
        let tabs = widget::container(panel_tabs(&cp.panel_tab_model))
            .padding(cosmic::iced::Padding {
                top: 0.0,
                right: geom::PANEL_SCROLLBAR_GAP,
                bottom: 0.0,
                left: 0.0,
            });
        let mut column: Vec<Element<'_, Msg>> = vec![tabs.into(), vspace(geom::PANEL_TAB_GAP)];
        // The CREATE row (DRAGON-687): pinned ABOVE the scrollable, so "New Palette" is
        // reachable however long the list grows, and so the scroll arithmetic the drop
        // machine hit-tests against starts at a fixed y (`geom::palettes_scroll_top`).
        if cp.panel_tab == geom::PanelTab::Palettes {
            column.push(create_palette_row(cp));
            column.push(vspace(geom::PANEL_TAB_GAP));
        }
        column.push(
            // The scrollable takes the REST of the height, so only the content scrolls and
            // the tab strip stays put. Plain `widget::scrollable`, like every scrollable in
            // the settings window, plus `on_scroll` (DRAGON-687): the drop machine
            // hit-tests through the window's MIRROR of the offset, and the widget's own
            // report is what keeps that mirror honest under user scrolling.
            widget::scrollable(
                // The scrollbar's own lane (DRAGON-682 item 16, the owner's rows ran
                // underneath it). Reserved by padding the SCROLLED CONTENT, which is how
                // the settings window reserves the same space on its own pages.
                widget::container(body).padding(cosmic::iced::Padding {
                    top: 0.0,
                    right: geom::PANEL_SCROLLBAR_GAP,
                    // The palettes tab ends with a group gap below the last group (the
                    // owner's symmetry ask, drag-jump round item four): the layout half
                    // of `geom::palettes_content_h`'s trailing term, the same constant.
                    // Harmonies fills its viewport exactly and takes none. A padding
                    // VALUE, not a structural flip: the container is permanent, item
                    // one's tree-stability rule.
                    bottom: if cp.panel_tab == geom::PanelTab::Palettes {
                        geom::PANEL_GROUP_GAP
                    } else {
                        0.0
                    },
                    left: 0.0,
                }),
            )
            .id(cp.panel_scroll_id.clone())
            .on_scroll(|viewport| {
                Msg::ColorPicker(ColorPickerMsg::PanelScrolled(viewport.absolute_offset().y))
            })
            .height(Length::Fill)
            .width(Length::Fill)
            .into(),
        );
        widget::container(widget::column(column).width(Length::Fill).height(Length::Fill))
            .padding(geom::WINDOW_PADDING)
            .width(Length::Fixed(geom::panel_w()))
            .height(Length::Fill)
            .into()
    }

    /// The Harmonies tab's content: one settings-style GROUP per harmony, each a heading over
    /// a card of swatches, recalculated from the window's current colour on every redraw.
    ///
    /// "Recalculated" is not a mechanism, it is the absence of one: the swatches are
    /// derived in this function from `ColorPickerState::color`, so any change to that
    /// colour, from any source, produces new swatches on the next frame with nothing to
    /// invalidate. That is the owner's "always calculate from the current color", and it is
    /// the reason no harmony result is cached anywhere.
    fn harmony_groups(&self) -> Element<'_, Msg> {
        let cp = &self.color_picker;
        let groups: Vec<Element<'_, Msg>> = crate::color::Harmony::ALL
            .into_iter()
            .enumerate()
            .map(|(g, h)| {
                harmony_group(
                    h,
                    swatch_bar(
                        &h.swatches(cp.color),
                        cp.alpha,
                        g,
                        cp.checker_bar_raster.as_ref(),
                        BarState {
                            menu: cp.panel_menu,
                            cursor: cp
                                .focus
                                .filter(|f| *f == geom::PickerFocus::Panel)
                                .and(cp.panel_cursor),
                            // The local copy flash, if it is still inside its window
                            // (DRAGON-682 item 30). Read here rather than in the segment,
                            // so the clock is asked once per frame rather than once per
                            // swatch.
                            copied: cp.swatch_copied.filter(|(_, at)| {
                                crate::widgets::copy_button::copied_recently(Some(*at))
                            }).map(|(at, _)| at),
                            dragging: cp.dragging(),
                            // DRAGON-687 follow-up: what a segment's menu needs to fit
                            // itself inside the window from its scrolled anchor.
                            scroll: cp.panel_scroll_y,
                            window: cp.window_size(),
                        },
                        // The saved palettes and the open menu's page (DRAGON-687), for
                        // the swatch menus' "Add to palette ›" submenu.
                        &cp.palettes,
                        cp.menu_page,
                    ),
                    // The Harmonies groups are UNCARDED (item 27, the owner's ask).
                    false,
                )
            })
            .collect();
        widget::column(groups).spacing(geom::PANEL_GROUP_GAP).width(Length::Fill).into()
    }

    /// The SAVED PALETTES tab's scrolled content (DRAGON-687): one group per saved
    /// palette, in the user's own order, or the empty-state lines while none exist.
    ///
    /// The groups deliberately mirror the harmony groups' shape (a heading line over one
    /// swatch-tall row, the same heights, pinned in `geom`), because the two tabs share
    /// one scroll arithmetic, one keyboard cursor and one drop machine.
    fn palettes_tab(&self) -> Element<'_, Msg> {
        let cp = &self.color_picker;
        if cp.palettes.is_empty() {
            // The create button above says what to do; these lines say why the area is
            // empty. Body text, not subdued, the settings window's own rule for a line
            // that IS the content.
            return widget::container(
                widget::column(vec![
                    widget::text::body("No saved palettes yet.").into(),
                    widget::text::caption("Create a palette, then drag colors into it.").into(),
                ])
                .spacing(4.0)
                .align_x(Alignment::Center),
            )
            .width(Length::Fill)
            .padding(geom::PANEL_GROUP_GAP)
            .align_x(Alignment::Center)
            .into();
        }
        // The FILTERED view (item six): the rows are `visible_palettes`' real indices in
        // display order, and everything below builds by (row, real) pairs. An empty
        // MATCH is its own line, not the empty-list lines above: palettes exist, the
        // query just names none of them.
        let visible = cp.visible_palettes();
        if visible.is_empty() {
            return widget::container(widget::text::body("No palettes match."))
                .width(Length::Fill)
                .padding(geom::PANEL_GROUP_GAP)
                .align_x(Alignment::Center)
                .into();
        }
        // VIRTUALIZED (item eight): only the rows intersecting the scroll viewport
        // (plus `geom::VIRTUAL_ROW_BUFFER` each side) become widgets; spacers carry the
        // off-screen extent so the scrollbar and every offset are unchanged. The window
        // is pure O(1) arithmetic (`geom::visible_row_window`); the drop zones, the
        // keyboard grid and the auto-scroll never see it, because they compute from
        // indices and pitch, not from built widgets.
        let viewport_h =
            geom::panel_scroll_bottom(cp.window_size().1) - geom::palettes_scroll_top();
        // An OPEN rename's row is always built (`keep`): dropping the editor from the
        // tree would take its focus and caret with it, so scrolling away must not
        // silently end the edit. The rename names a REAL group; a rename filtered out
        // of the view maps to no row and keeps nothing extra.
        let keep = cp
            .rename
            .as_ref()
            .and_then(|(real, _)| visible.iter().position(|g| g == real));
        let win = geom::visible_row_window(visible.len(), cp.panel_scroll_y, viewport_h, keep);
        let mut children: Vec<Element<'_, Msg>> = Vec::new();
        if let Some(h) = win.top {
            children.push(
                widget::space::Space::new().height(Length::Fixed(h)).into(),
            );
        }
        // The hover pencil's row, resolved ONCE per frame (the per-group resolve
        // walked the filter again for every built row).
        let hovered_row = geom::hovered_palette_title_at(
            cp.window_pointer,
            cp.window_size(),
            cp.panel_scroll_y,
            visible.len(),
            cp.palette_drop_open(),
        );
        for (row, real) in visible.iter().enumerate().take(win.last).skip(win.first) {
            let hovered = hovered_row == Some(row) || cp.group_menu == Some(row);
            children.push(self.palette_group_view(row, *real, &cp.palettes[*real], hovered));
        }
        if let Some(h) = win.bottom {
            children.push(
                widget::space::Space::new().height(Length::Fixed(h)).into(),
            );
        }
        // (The pointer report that fed the hover pencil lived HERE for one merge, on
        // this scrolled content's own mouse_area, and the owner's repro retired it:
        // moving UP off the first title leaves this region entirely, so its report goes
        // silent with the stale position still inside a title rect. The report is the
        // window ROOT's now, `color_picker_window_view`'s wrapper, whose region is
        // everywhere the pointer can be.)
        widget::column(children).spacing(geom::PANEL_GROUP_GAP).width(Length::Fill).into()
    }

    /// ONE saved palette's group: the (renameable, draggable) heading over its bar row
    /// with its two icon buttons right-aligned IN the title row (DRAGON-687, re-shaped
    /// by its UX round so the bar spans the full card and "the palettes breathe").
    ///
    /// `g` is the visible ROW (geometry, gestures, transient view state) and `real` the
    /// palette's index in the FULL list (every mutating message, the rename identity),
    /// item six's one split, made explicit at this boundary.
    fn palette_group_view<'a>(
        &'a self,
        g: usize,
        real: usize,
        p: &'a geom::Palette,
        hovered: bool,
    ) -> Element<'a, Msg> {
        let cp = &self.color_picker;
        // The TITLE AREA: the rename editor while this group is being renamed, and the
        // name-with-pencil handle otherwise. The click-to-rename region is exactly this
        // area, never the row's empty middle: the mouse_area wraps the text (and its
        // hover pencil) at their natural width, inside the left-aligned fill below, so a
        // stray click between the name and the icons renames nothing.
        let title: Element<'a, Msg> = match &cp.rename {
            Some((group, draft)) if *group == real => rename_input(cp, draft),
            _ => {
                // DERIVED, never flagged, from the WINDOW-level pointer (the pencil's
                // second stranding: a content-scoped report went silent when the
                // pointer left it upward): the pencil shows exactly while the window
                // position maps onto this title's own rect
                // (`geom::hovered_palette_title_at`, resolved once by the caller), or
                // while this title's menu is open.
                // ELLIPSIZED to one line (DRAGON-687's UX round): a long name truncates
                // with the toolkit's own "…" (the window title's exact treatment,
                // `header_title`) instead of pushing the right-aligned icons off the
                // row; the full name is the tooltip below, and the RENAME editor always
                // shows it whole.
                let mut cells: Vec<Element<'a, Msg>> = vec![
                    widget::text::heading(&p.name)
                        .wrapping(cosmic::iced::advanced::text::Wrapping::None)
                        .ellipsize(cosmic::iced::advanced::text::Ellipsize::End(
                            cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                        ))
                        .into(),
                ];
                if hovered {
                    // The text-editor glyph, on hover only (the owner's ask), BESIDE the
                    // text where it always was: it says the name is editable, the
                    // right-aligned pair says what the group can do, and the fill between
                    // them is what keeps the three from ever colliding.
                    cells.push(
                        crate::widgets::icons::tinted(
                            crate::widgets::icons::handle("pencil-symbolic"),
                            theme::subtle,
                        )
                        .size(geom::PANEL_HINT_ICON)
                        .into(),
                    );
                }
                // The title is a DRAG SOURCE (reorder the groups, or off-window to
                // delete) and a click target (rename): the press names it, the release
                // completes the click, exactly the history swatches' machine. The GRAB
                // hand says "this moves"; the pencil says "this edits".
                let handle = crate::widgets::force_cursor::force_cursor(
                    widget::mouse_area(
                        widget::row(cells)
                            .spacing(geom::PANEL_HINT_GAP)
                            .align_y(Alignment::Center),
                    )
                    .on_press(Msg::ColorPicker(ColorPickerMsg::DragPressed(
                        geom::DragSource::PaletteName(g),
                    )))
                    .on_release(Msg::ColorPicker(ColorPickerMsg::GroupNameReleased(g)))
                    .on_right_press(Msg::ColorPicker(ColorPickerMsg::GroupMenu(Some(g)))),
                    cosmic::iced::mouse::Interaction::Grab,
                );
                // The full-name tooltip, exactly when the name TRUNCATES
                // (`geom::palette_title_truncates`): an ellipsis must be answerable, and
                // a tooltip repeating a short name would be noise.
                let handle: Element<'a, Msg> = if geom::palette_title_truncates(&p.name) {
                    widget::tooltip(
                        handle,
                        widget::text(p.name.clone()).size(12),
                        widget::tooltip::Position::Top,
                    )
                    .into()
                } else {
                    handle
                };
                if cp.group_menu == Some(g) {
                    group_menu(
                        handle,
                        real,
                        geom::palette_heading_anchor(g, cp.panel_scroll_y),
                        cp.window_size(),
                    )
                } else {
                    handle
                }
            }
        };
        // The TITLE ROW (the UX round): the title area filling leftward, then the
        // PIPETTE and the PLUS right-aligned as a pair, all centred on one line. The
        // row is a button tall (`geom::PALETTE_TITLE_ROW_H`), which is what the group
        // height counts.
        let title_row = widget::row(vec![
            widget::container(title)
                .align_y(cosmic::iced::alignment::Vertical::Center)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            palette_pipette_button(real),
            hspace(geom::PALETTE_PLUS_GAP),
            palette_plus_button(real),
        ])
        .align_y(Alignment::Center)
        .height(Length::Fixed(geom::PALETTE_TITLE_ROW_H))
        .width(Length::Fixed(geom::card_w()));
        // The BAR row: the full-width bar alone, since the icons moved up.
        let row = widget::row(vec![self.palette_bar(g, real, p)])
            .align_y(Alignment::Center)
            .height(Length::Fixed(geom::PANEL_SWATCH));
        widget::column(vec![title_row.into(), row.into()])
            .spacing(geom::PANEL_HEADING_GAP)
            .width(Length::Fill)
            .into()
    }

    /// ONE saved palette's BAR (DRAGON-687): the harmony bars' construction at the
    /// harmony bars' own full width (the UX round moved the icons into the title row),
    /// with this tab's own sources and menus. An
    /// EMPTY palette wears the empty history slots' own DOTTED outline at the bar's size
    /// (the owner's follow-up), so "nothing here yet" reads the same in both places; the
    /// bar stays the same drop target either way, and only the dress says it is empty.
    fn palette_bar<'a>(&'a self, g: usize, real: usize, p: &'a geom::Palette) -> Element<'a, Msg> {
        let cp = &self.color_picker;
        if p.colors.is_empty() {
            // The dots are a RASTER for `empty_slot`'s own reason: iced draws a border as
            // a solid ring with no dash vocabulary. And like that slot, a missing raster
            // degrades to a SOLID hairline of the same size rather than to nothing: the
            // rasters are built before the window opens, so this arm should never be
            // seen, and an empty block that looks like a loading failure is the shape of
            // bug the fallback exists to make visible.
            return match cp.empty_palette_raster.as_ref() {
                Some(_) => raster_image(
                    cp.empty_palette_raster.as_ref(),
                    geom::bar_w(),
                    geom::PANEL_SWATCH,
                ),
                None => widget::container(widget::space::Space::new())
                    .width(Length::Fixed(geom::bar_w()))
                    .height(Length::Fixed(geom::PANEL_SWATCH))
                    .class(cosmic::theme::Container::custom(|t| {
                        cosmic::iced::widget::container::Style {
                            border: Border {
                                radius: swatch_radius(t).into(),
                                width: 1.0,
                                color: theme::subdued(t),
                            },
                            ..Default::default()
                        }
                    }))
                    .into(),
            };
        }
        let n = p.colors.len();
        let widths = geom::segment_widths(n);
        let cursor = cp
            .focus
            .filter(|f| *f == geom::PickerFocus::Panel)
            .and(cp.panel_cursor)
            .filter(|(row, _)| *row == g);
        let copied = cp
            .swatch_copied
            .filter(|(_, at)| crate::widgets::copy_button::copied_recently(Some(*at)))
            .map(|(at, _)| at);
        let segments: Vec<Element<'a, Msg>> = p
            .colors
            .iter()
            .enumerate()
            .zip(widths.iter().copied())
            .map(|((i, e), w)| {
                palette_segment(
                    e.color,
                    e.alpha,
                    (g, i),
                    real,
                    w,
                    geom::segment_corners(i, n),
                    cp.palette_menu == Some((g, i)),
                    geom::swatch_tip(
                        copied == Some((g, i)),
                        cursor == Some((g, i)),
                        cp.dragging(),
                    ),
                    &cp.palettes,
                    cp.menu_page,
                    // DRAGON-687 follow-up: the menu's fitted placement reads the
                    // segment's scrolled anchor and the window it must stay inside.
                    geom::palette_swatch_anchor(g, i, n, cp.panel_scroll_y),
                    cp.window_size(),
                )
            })
            .collect();
        let row = widget::row(segments).width(Length::Fixed(geom::bar_w()));
        let mut layers: Vec<Element<'a, Msg>> = vec![
            // The harmony bars' own board: a palette bar is its exact size again.
            raster_image(
                cp.checker_bar_raster.as_ref(),
                geom::bar_w(),
                geom::PANEL_SWATCH,
            ),
            row.into(),
            bar_outline(None, geom::bar_w(), [true, true]),
        ];
        if let Some((_, i)) = cursor
            && i < n
        {
            layers.push(absolute(
                bar_outline(Some(()), widths[i], geom::segment_corners(i, n)),
                (geom::segment_x(i, n), 0.0),
            ));
        }
        cosmic::iced::widget::stack(layers)
            .width(Length::Fixed(geom::bar_w()))
            .height(Length::Fixed(geom::PANEL_SWATCH))
            .into()
    }

    /// The colour history: TWO rows of [`geom::RECENTS_PER_ROW`] swatches (the two-row
    /// grid is the owner's rev-2 ask, matching the reference layout; the rows have been
    /// eight, then ten, and are nine now that the window is narrower). A
    /// full row is JUSTIFIED across the content ([`geom::recents_gap`]), so its last swatch
    /// lands flush with the tracks' right edge; a part-filled row keeps the same grid
    /// positions rather than re-spacing itself as picks arrive.
    ///
    /// Clicking a swatch LOADS it and nothing else: the list never reorders (see
    /// [`geom::writes_recents`]). Each swatch carries its hex as a tooltip, and takes
    /// the same configured rounding as every swatch here.
    fn recent_colors_grid(&self) -> Element<'_, Msg> {
        let recents = &self.color_picker.recents;
        // The SELECTED entry is the one whose colour AND alpha the window is showing
        // (DRAGON-680): with alpha in the history, two entries can share a colour and
        // differ only in transparency, and ringing both of them would be a lie about
        // which one is loaded. Since DRAGON-682 item 7 it is NOT the same thing as the
        // navigated one: the arrows move a cursor that applies nothing, so the window can
        // be showing one swatch while the keyboard sits on another, and the two are drawn
        // differently (see `recent_swatch`).
        let current = geom::Recent::new(self.color_picker.color, self.color_picker.alpha);
        let cursor = self
            .color_picker
            .recent_cursor
            .filter(|_| self.color_picker.focus == Some(geom::PickerFocus::History));
        let gap = geom::recents_gap();
        // EVERY position up to the cap, filled or not (DRAGON-682 item 8): an unfilled one
        // draws a dotted outline, which is what makes the grid's full extent visible before
        // it fills up.
        //
        // Tombstone: an empty history used to replace the whole grid with the line "Colors
        // you pick appear here." (DRAGON-582), whose job was to stop an empty block reading
        // as broken. The dotted slots do that job in the place the colours will actually
        // appear, so the owner had the line removed with item 10 rather than have both.
        let swatches: Vec<Element<'_, Msg>> = (0..geom::RECENTS_CAP)
            .map(|i| match recents.get(i) {
                Some(entry) => recent_swatch(
                    *entry,
                    self.color_picker.recent_rasters.get(i).and_then(|r| r.as_ref()),
                    i,
                    SwatchState {
                        selected: *entry == current,
                        menu_open: self.color_picker.recents_menu == Some(i),
                        on_cursor: cursor == Some(i),
                        dragging: self.color_picker.dragging(),
                        hovered: self.color_picker.hovered_recent == Some(i),
                    },
                    // DRAGON-687: for the menu's Add-to-palette submenu, and the
                    // window size its fitted placement clamps against.
                    &self.color_picker.palettes,
                    self.color_picker.menu_page,
                    self.color_picker.window_size(),
                ),
                None => empty_slot(self.color_picker.empty_slot_raster.as_ref()),
            })
            .collect();
        let mut grid_rows: Vec<Element<'_, Msg>> = Vec::new();
        let mut row: Vec<Element<'_, Msg>> = Vec::new();
        for (i, s) in swatches.into_iter().enumerate() {
            row.push(s);
            if (i + 1) % geom::RECENTS_PER_ROW == 0 {
                grid_rows.push(
                    widget::row(std::mem::take(&mut row))
                        .spacing(gap)
                        .height(Length::Fixed(geom::RECENT_SWATCH))
                        .into(),
                );
            }
        }
        if !row.is_empty() {
            grid_rows.push(
                widget::row(row)
                    .spacing(gap)
                    .height(Length::Fixed(geom::RECENT_SWATCH))
                    .into(),
            );
        }
        let grid = widget::column(grid_rows).spacing(gap).width(Length::Fill);
        history_frame(
            grid.into(),
            self.color_picker.focus == Some(geom::PickerFocus::History),
        )
    }
}

/// The window title as the header's START region: flush left on Windows and Linux
/// (DRAGON-676; macOS keeps the centred title, see `color_picker_window_view`).
///
/// It is libcosmic's OWN construction for a header title, copied out of
/// `header_bar::view`'s centre arm: `text::heading`, no wrapping, ellipsized to one line.
/// Copied rather than approximated because the only thing DRAGON-676 changes is WHERE the
/// title sits; a title that also became a different size or weight on two platforms would
/// be a second change nobody asked for. The wrapping and ellipsize matter even for a title
/// this short: the start region is laid out inside what the end region leaves, which on
/// Windows is the whole 146pt caption inset, and a title allowed to wrap would grow the
/// header instead of ending in an ellipsis.
#[cfg(not(target_os = "macos"))]
fn header_title<'a>() -> Element<'a, Msg> {
    use cosmic::iced::advanced::text::{Ellipsize, EllipsizeHeightLimit, Wrapping};
    widget::text::heading(WINDOW_TITLE)
        .wrapping(Wrapping::None)
        .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
        .into()
}

/// A window raster as a fixed-size NEAREST-free image, or an equally-sized blank while
/// the first refresh has not run (a one-frame state at most; the rasters are built in
/// the update handler before the window opens). Fixed sizes on both arms, so the layout
/// cannot shift when the raster lands.
fn raster_image<'a>(
    raster: Option<&widget::image::Handle>,
    w: f32,
    h: f32,
) -> Element<'a, Msg> {
    match raster {
        Some(handle) => widget::image::Image::new(handle.clone())
            .width(Length::Fixed(w))
            .height(Length::Fixed(h))
            .into(),
        None => widget::container(widget::space::Space::new())
            .width(Length::Fixed(w))
            .height(Length::Fixed(h))
            .into(),
    }
}

/// The round CURRENT-COLOUR swatch: its interior from the raster, its SILHOUETTE from an
/// analytic quad ring stacked on top (DRAGON-680).
///
/// **The ring is what makes the edge smooth, and the raster cannot be** — see
/// `geom::SWATCH_RING_W` for the two raster-side attempts that failed and exactly why the
/// second one (a 3x supersampled buffer) reached the screen looking identical to the first:
/// iced's image atlas has no mipmaps, so its linear sampler decimates a downscale rather
/// than averaging it. A quad with a corner radius is drawn from a signed distance field at
/// the display's real resolution, which is the same machinery that makes the slider thumbs
/// one row up look right on the very same window.
///
/// **The stacking order is load-bearing and it is not free.** Within ONE iced layer every
/// quad is drawn before every image, whatever order a widget issues them in
/// (`widgets::color_field`'s module doc carries the bug that taught this codebase so), so a
/// ring drawn beside the image would land UNDER it and be invisible. `widget::stack` wraps
/// every child after the first in `renderer.with_layer`, which is what puts the ring in
/// front. Do not "simplify" this into a container with a border around the image: that is
/// one layer, and the ring would vanish.
///
/// The ring is the same subdued ink the raster paints its own rim band in, so the two
/// cannot show a seam, and the raster stops `geom::SWATCH_EDGE_MASK` inside the ring's outer
/// edge so its stepped boundary is under the ring's opaque band.
fn swatch_disc(raster: Option<&widget::image::Handle>) -> Element<'_, Msg> {
    let d = geom::SWATCH_CIRCLE;
    let ring = widget::container(widget::space::Space::new())
        .width(Length::Fixed(d))
        .height(Length::Fixed(d))
        .class(cosmic::theme::Container::custom(move |t| {
            cosmic::iced::widget::container::Style {
                border: Border {
                    // A full half-diameter radius IS a circle, drawn analytically.
                    radius: (d / 2.0).into(),
                    width: geom::SWATCH_RING_W,
                    // The rim tone the raster uses, read from the same place
                    // (`picker_rim` hands `theme::subdued` to the raster builder).
                    color: theme::subdued(t),
                },
                ..Default::default()
            }
        }));
    cosmic::iced::widget::stack(vec![raster_image(raster, d, d), ring.into()])
        .width(Length::Fixed(d))
        .height(Length::Fixed(d))
        .into()
}

/// One slider strip (DRAGON-630): the pre-rastered gradient under `widgets::color_field`'s
/// draggable marker, at normalized position `t`.
///
/// The thumb is the owner's spec: a solid disc in the WINDOW's own background with a
/// subdued-text border, so it reads as a control sitting on the window rather than as a
/// hole in the gradient. `theme::subtle` is the app's one subdued-text token.
fn gradient_strip<'a>(
    raster: Option<&widget::image::Handle>,
    t: f32,
    on_change: impl Fn(f32, f32) -> Msg + 'a,
) -> Element<'a, Msg> {
    crate::widgets::color_field::ColorField::new(
        raster_image(raster, geom::STRIPS_W, geom::STRIP_H),
        (t, 0.5),
        crate::widgets::color_field::FieldAxis::X,
        on_change,
    )
    // DERIVED from the track's thickness (`geom::STRIP_MARKER_D`), which is what made
    // DRAGON-680's "slightly narrower, with smaller circles to match" one edit instead of
    // two: the thumb followed the strip from 24 to 20 with nothing to keep in step here.
    .marker_diameter(geom::STRIP_MARKER_D)
    .marker_style(|t| crate::widgets::color_field::MarkerStyle {
        // The window's own background at 90% (DRAGON-680, the owner: "EVER SO SLIGHTLY 10%
        // see through"), so the track's gradient reads faintly through the thumb and the
        // thumb still reads as a solid control sitting ON the track rather than as a hole
        // in it. The RING keeps its full strength: it is what holds the disc's shape
        // against a bright gradient, and fading it would soften the one edge that has to
        // stay crisp.
        fill: Some(cosmic::iced::Color {
            a: STRIP_THUMB_ALPHA,
            ..t.cosmic().background(false).base.into()
        }),
        border: theme::subdued(t),
        border_width: 1.0,
        shadow: cosmic::iced::core::Shadow::default(),
    })
    .into()
}

/// ONE value box: the text input at `w` points wide, wired to the draft plumbing at
/// component index `idx` and to the focus plumbing at row POSITION `pos`.
///
/// The two indices differ for hex alone, and that is why both are parameters: its single
/// box is [`crate::app::color_picker::WHOLE_VALUE_BOX`] to the draft (so `BoxEdited`
/// parses the whole spelling rather than one component) and position 0 to the focus ids
/// (so "the first box" is the same idea in every mode).
///
/// Two DRAGON-680 behaviours ride here, and they are complementary rather than redundant:
///
/// * `select_on_focus` selects the whole value when focus arrives through a focus
///   OPERATION, which is Tab, Shift+Tab and the window's own "focus the first box" on
///   open and after a mode change (libcosmic's `State::focus` consults the flag);
/// * `on_focus` reports a CLICK that lands on an unfocused box, which the flag does not
///   cover, because the click path places the caret where you clicked instead of calling
///   `focus()`. The handler answers with `text_input::select_all`, so both routes end with
///   the value selected and the next keystroke replacing it, which is what the owner asked
///   for ("focusing a new input box automatically selects all text in that box").
fn value_box<'a>(
    cp: &'a super::ColorPickerState,
    idx: usize,
    pos: usize,
    w: f32,
) -> Element<'a, Msg> {
    let input = widget::text_input("", cp.box_text(idx))
        .on_input(move |s| Msg::ColorPicker(ColorPickerMsg::BoxEdited(idx, s)))
        // Enter COMMITS the draft and nothing else since DRAGON-680: the box re-renders
        // in its canonical spelling (hex letters uppercased, numbers respelled), and
        // FILING the colour into the history moved to the primary+Enter chord that the
        // "Add to recents" button now advertises.
        .on_submit(|_| Msg::ColorPicker(ColorPickerMsg::BoxCommitted))
        .on_focus(Msg::ColorPicker(ColorPickerMsg::BoxFocused(pos)))
        .select_on_focus(true)
        .width(Length::Fixed(w))
        .style(value_box_style());
    match cp.box_id(pos) {
        Some(id) => input.id(id).into(),
        // Unreachable while `MAX_VALUE_BOXES` covers every mode (pinned by
        // `geom::value_layout_tests`); an id-less box still edits, it just cannot be
        // focused programmatically, which is the safe way for that to fail.
        None => input.into(),
    }
}

/// The MODE ACTIVATOR (DRAGON-680): an up chevron over a down one, at the right end of
/// the box band, vertically centred on the boxes, and, while `open`, the menu of the seven
/// notations floating above it.
///
/// **ONE control, not two.** The pair is a single button with a single hover wash, and a
/// click anywhere on it opens the menu. That is the owner's correction to this ticket's
/// first revision, which read the chevrons as a stepper (up = previous notation, down =
/// next) and deleted the menu: "they were still supposed to together act as a single
/// hoverable unit that triggers the dropdown menu". The two chevrons are a SELECTOR glyph,
/// the same thing `chevrons-up-down` says in one box, drawn as two so the pair can carry
/// the air the owner asked for between them.
///
/// What did NOT come back with the menu is the CHIP the activator used to be: no border,
/// no fill, no notation name beside the chevrons (the owner: "get rid of the dropdown
/// styling and text"). So nothing on the closed control says which notation is current.
/// Two things still do, and that is why this is not a hole: the caption band directly under
/// the boxes spells it ("HEX", or "R G B A"), and the open menu marks the current row in
/// accent.
///
/// The unit is one value box tall ([`geom::MODE_STEP_H`] twice plus
/// [`geom::MODE_STEP_GAP`], asserted in `geom`), so the band's height stays the boxes'.
///
/// The menu opens UPWARD, because the divider and the two history rows below leave no room,
/// and RIGHT-ALIGNED to this control, because this control sits at the content's right edge
/// where the app's ordinary left-aligned flyout would run off the window
/// (`chrome::FlyoutDir::UpRight`, added for exactly this).
fn mode_activator<'a>(mode: ColorFormat, open: bool, focused: bool) -> Element<'a, Msg> {
    // Tinted at the LEAF like the controls row's own glyphs, for the reason
    // `controls_icon_button` spells out: the centring containers below would otherwise
    // decide the ink themselves. These two want the ordinary foreground, which is what the
    // container happened to supply, so nothing about them LOOKED wrong; routing them
    // through the same function is what keeps the family answering one question.
    let chevron = |icon: &'static str| -> Element<'a, Msg> {
        widget::container(
            crate::widgets::icons::tinted(crate::widgets::icons::handle(icon), |t| {
                controls_icon_ink(t, false)
            })
            .size(geom::MODE_STEP_ICON),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fixed(geom::MODE_STEP_H))
        .into()
    };
    // The centring container inside the button is the cosmic-button quirk this file
    // documents twice over: a button lays its content out at its padding rather than
    // centring it, so a bare column sits top-aligned in a fixed-height button.
    let unit = widget::button::custom(
        widget::container(
            widget::column(vec![chevron("pan-up-symbolic"), chevron("pan-down-symbolic")])
                .spacing(geom::MODE_STEP_GAP)
                .width(Length::Fill),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill),
    )
    .width(Length::Fixed(geom::MODE_STEP_W))
    .height(Length::Fixed(geom::VALUE_BOX_H))
    .padding(0)
    // The controls row's own bare-icon dress, so the one hover wash covers the whole unit
    // and reads as the same kind of control as the pipette and the copy button, plus the
    // FOCUS frame when the window's Tab ring is parked here (DRAGON-680, the owner's
    // "with correct highlight border just like inputs get").
    .class(cosmic::theme::Button::Custom {
        active: Box::new(move |_f, t| focus_frame(controls_icon_style(t, false, false, false), t, focused)),
        hovered: Box::new(move |_f, t| focus_frame(controls_icon_style(t, true, false, false), t, focused)),
        pressed: Box::new(move |_f, t| focus_frame(controls_icon_style(t, true, true, false), t, focused)),
        disabled: Box::new(move |t| focus_frame(controls_icon_style(t, false, false, false), t, focused)),
    })
    .on_press(Msg::ColorPicker(ColorPickerMsg::ModeMenuToggled));
    let unit: Element<'a, Msg> = crate::widgets::arrow_cursor::arrow_cursor(unit);
    if !open {
        // ONE tooltip on the unit, and only while the menu is CLOSED. It is worth having
        // because the ask that stripped the chip also deleted the only place the control
        // said what it was for; it is silenced while the menu is up for the reason
        // `widgets::copy_button`'s tombstone gives, that a card beside an open panel is two
        // answers to one question. Upward, like every other tooltip in this window.
        return widget::tooltip(
            unit,
            widget::text("Change notation").size(12),
            widget::tooltip::Position::Top,
        )
        .into();
    }
    let items: Vec<Element<'a, Msg>> = ColorFormat::ALL
        .into_iter()
        .enumerate()
        .map(|(i, f)| {
            // The current mode reads accent, like every other menu in the app. With the
            // chip's label gone this is also the ONLY place the notation is named while
            // the menu is up, so it is carrying more than decoration.
            let text = if f == mode {
                widget::text(f.label()).size(geom::MODE_LABEL_SIZE).class(
                    cosmic::theme::Text::Custom(|t| cosmic::iced::widget::text::Style {
                        color: Some(theme::accent(t)),
                        ..Default::default()
                    }),
                )
            } else {
                widget::text(f.label()).size(geom::MODE_LABEL_SIZE)
            };
            crate::widgets::arrow_cursor::arrow_cursor(
                // FIXED height, and the centring container that goes with it (the same
                // cosmic-button quirk as above). The height is not cosmetic:
                // `geom::mode_menu_panel_h` has to know the panel's exact height to place
                // it, and a row left to its own natural size is a number nothing here can
                // do better than guess at.
                widget::button::custom(widget::container(text).center_y(Length::Fill))
                    .width(Length::Fill)
                    .height(Length::Fixed(geom::MODE_MENU_ITEM_H))
                    .padding([0, geom::MODE_MENU_ROW_PAD])
                    .class(cosmic::theme::Button::Text)
                    .on_press(Msg::ColorPicker(ColorPickerMsg::ModeSelected(i))),
            )
        })
        .collect();
    // The panel is a FIXED width, measured from the longest option (`geom::mode_menu_width`),
    // and its item column FILLS it so a hover highlight is as wide as the row it highlights.
    // The `Fill` on the column is load-bearing, not decoration: a shrink-width column would
    // hand its `Fill` buttons nothing to fill and every item would collapse to the width of
    // its own word, which is what once made this menu look like it hung off the lettering
    // rather than off the control.
    let menu = widget::container(
        widget::column(items).spacing(geom::MODE_MENU_GAP).width(Length::Fill),
    )
    .width(Length::Fixed(geom::mode_menu_width()))
    .padding(geom::MODE_MENU_PAD)
    .class(cosmic::theme::Container::custom(mode_menu_style));
    crate::app::preview::chrome::flyout(
        unit,
        menu.into(),
        crate::app::preview::chrome::FlyoutDir::UpRight {
            panel_h: geom::mode_menu_panel_h(),
            dx: geom::mode_menu_dx(),
        },
        Msg::ColorPicker(ColorPickerMsg::ModeMenuToggled),
    )
}

/// The mode MENU panel's look: the opaque menu surface every editor dropdown wears (see
/// `chrome::menu_container`), component base at full alpha under a divider outline.
///
/// It takes the PANEL rounding token, `rounding().m`. DRAGON-676 had it take the value
/// boxes' input radius instead, and that was right at the time for a reason that has since
/// expired: the panel hung off a bordered CHIP that sat directly on the box row, so the
/// three were one control group and a popup rounding differently from the chip read as a
/// second, unrelated surface. There is no chip now, and the panel floats over the value row
/// rather than sitting on it, so it goes back to being what it is: a menu.
fn mode_menu_style(theme: &cosmic::Theme) -> cosmic::iced::widget::container::Style {
    let c = theme.cosmic();
    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(c.background(false).component.base.into())),
        border: Border {
            radius: theme::rounding(theme).m.into(),
            width: 1.0,
            color: c.background(false).component.divider.into(),
        },
        ..Default::default()
    }
}

/// One cell of the value row's CAPTION band: the channel letter centred in its own box's
/// width, in a band of fixed height. The reference layout's "255 over R" shape
/// (DRAGON-630), built as a separate band from the boxes so the controls beside them can
/// centre on the boxes themselves (see [`App::value_row`]).
///
/// SUBTLE text (the owner's ask). These letters name their box; they are not the value,
/// and at full strength they competed with the numbers above them for the same glance.
/// `theme::subtle` and not `theme::subdued`, which is the deeper dimming used for the
/// OUTLINES around these boxes: at that strength a single letter stopped being readable,
/// which is the same reason the cloud page moved its sentences off it.
fn caption_cell<'a>(label: &'a str, w: f32) -> Element<'a, Msg> {
    widget::container(widget::text::caption(label).class(cosmic::theme::Text::Custom(|t| {
        cosmic::iced::widget::text::Style { color: Some(theme::subtle(t)), ..Default::default() }
    })))
    .center_x(Length::Fixed(w))
    .height(Length::Fixed(geom::VALUE_LABEL_H))
    .into()
}

/// The value boxes' style: cosmic's own text input with TWO changes, the RESTING border
/// colour (`theme::subdued`, the owner's ask, so the boxes are outlined in the app's quiet
/// ink) and the app-wide SELECTION fill (`theme::soften_selection`, DRAGON-680).
///
/// Derived from the stock appearance rather than written out, so everything else about
/// the field (its fill, its rounding, its placeholder colour and its border width) stays
/// whatever libcosmic says it is. Hover and focus keep the theme's ACCENT outline
/// untouched: that is the app-wide "this is the field you are in" signal and it is not
/// ours to reinvent for one window.
///
/// It composes the selection override by hand rather than calling `theme::input_style`,
/// for the reason that helper's own doc gives: this style also changes the border, and
/// asking for the shared style would replace that instead of adding to it. Every state
/// carries the softened selection, including the ones this file does not otherwise touch,
/// because a selection that changed colour when the pointer entered the box would be
/// stranger than either colour on its own.
fn value_box_style() -> cosmic::theme::TextInput {
    use cosmic::widget::text_input::StyleSheet as _;
    cosmic::theme::TextInput::Custom {
        active: Box::new(|t| {
            let mut a = theme::soften_selection(t.active(&cosmic::theme::TextInput::Default), t);
            a.border_color = theme::subdued(t);
            a
        }),
        error: Box::new(|t| {
            theme::soften_selection(t.error(&cosmic::theme::TextInput::Default), t)
        }),
        hovered: Box::new(|t| {
            theme::soften_selection(t.hovered(&cosmic::theme::TextInput::Default), t)
        }),
        focused: Box::new(|t| {
            theme::soften_selection(t.focused(&cosmic::theme::TextInput::Default), t)
        }),
        disabled: Box::new(|t| {
            theme::soften_selection(t.disabled(&cosmic::theme::TextInput::Default), t)
        }),
    }
}

// ── The mode DROPDOWN, and why it is gone (DRAGON-680) ───────────────────────
//
// Five functions and five constants stood here: `mode_picker` (the chip plus its upward
// flyout menu), `mode_chip_style`, `mode_menu_style`, `mode_menu_panel_h`, the panel's
// item height / gap / padding, `INPUT_BORDER_W`, and `value_box_radius` (the chip read
// the VALUE BOXES' own corner radius out of `value_box_style`, so the two rows could not
// round differently). The owner asked for all of it to go: "get rid of the dropdown
// styling and text, and just make it the up and down chevron".
//
// Keep the reasoning, because it was hard-won and the control looked deliberate:
//
// * the chip was HAND-BUILT rather than a `widget::dropdown` because the stock widget
//   could take none of the three things asked of it (the icon-button hover wash, a fixed
//   text span, the app's own vendored caret);
// * its menu opened UPWARD, by a known panel height, because the history block below it
//   leaves no room, which is also why every part of that panel had a FIXED height: the
//   flyout's offset is an exact number, and a row left to its natural size is a number
//   this file can only guess at;
// * it wore the value boxes' border WIDTH and RADIUS, read from the boxes rather than
//   restated, because the mode row sat directly on the box row and the two were one
//   control group.
//
// If a menu is ever wanted here again, `preview::chrome::flyout` is still the recipe and
// the third bullet is the part most likely to be forgotten. What replaced the chip is
// `mode_stepper` above: two bare icon buttons, no border, no text, no popup.


/// The divider BAND: the hairline between the value block and the colour history, with
/// the "Add to recents" button centred on it (the owner's ask).
///
/// A stack rather than a row of line-gap-button-gap-line, because the button is meant to
/// sit ON the rule the way a slider's thumb sits on its track, which is also where it
/// takes its look from: the line runs behind it and its opaque fill hides the crossing.
///
/// The line keeps its old construction exactly, a divider wrapped at [`geom::DIVIDER_H`],
/// because the divider widget carries its own padding and un-counted height here is what
/// once clamped the second history row short. That 1pt wrapper is then centred in the
/// band, so the visible rule stays where the section gaps say it is however tall the
/// button gets.
fn divider_band<'a>() -> Element<'a, Msg> {
    let line = widget::container(
        widget::container(widget::divider::horizontal::default())
            .width(Length::Fill)
            .height(Length::Fixed(geom::DIVIDER_H)),
    )
    .width(Length::Fill)
    .center_y(Length::Fill);
    let button = widget::container(add_color_button())
        .center_x(Length::Fill)
        .center_y(Length::Fill);
    cosmic::iced::widget::stack(vec![line.into(), button.into()])
        .width(Length::Fill)
        .height(Length::Fixed(geom::DIVIDER_BAND_H))
        .into()
}

/// The "Add to recents" button on the divider: file the colour the window is showing into the
/// history (`ColorPickerMsg::AddToHistory`).
///
/// It is dressed as a SLIDER THUMB (the owner's ask): the window's own background under a
/// subdued-text hairline, which is `gradient_strip`'s marker exactly. That is the right
/// borrow rather than a coincidence, because it is the same idea in a different place, a
/// small control sitting ON a track. The plus is the app's lucide glyph at
/// [`geom::ADD_COLOR_ICON`], deliberately smaller than the surrounding icons since it
/// stands beside 12pt text rather than alone.
///
/// **It ADVERTISES its keyboard chord in a TOOLTIP** (DRAGON-680): `⌘Enter` on macOS,
/// `Ctrl+Enter` on Linux and Windows, rendered by `shortcuts::add_color_chord_label` rather
/// than spelled out here, so it uses the app's ONE in-app chord formatter
/// (`Shortcut::label`, symbols on mac and words elsewhere) and cannot drift from the chord
/// the key handler actually matches.
///
/// Saying it at all is the point: plain Enter used to file the colour from a value box,
/// which is a lot of history to write by accident while typing, so the owner moved it onto
/// a deliberate chord, and a shortcut nobody can see is a shortcut nobody uses. Saying it
/// in the TOOLTIP rather than in the label is the owner's correction ("i asked for the
/// command enter or control enter to be a tooltip, not baked into the add color button"):
/// the button sits ON the divider rule at a size chosen to be small, and a second span of
/// text inside it made it wide enough to read as a section header. Built like every other
/// tooltip in this window, an explicit `widget::tooltip` dropping UPWARD.
fn add_color_button<'a>() -> Element<'a, Msg> {
    let content = widget::row(vec![
        widget::icon(crate::widgets::icons::handle("list-add-symbolic"))
            .size(geom::ADD_COLOR_ICON)
            .into(),
        widget::text("Add to recents").size(12).into(),
    ])
    .spacing(4.0)
    .align_y(Alignment::Center);
    // The centring container is the cosmic-button quirk the mode activator documents: a
    // button lays its content out at its padding rather than centring it, so a bare row
    // sits top-aligned in a fixed-height button.
    let button = widget::button::custom(widget::container(content).center_y(Length::Fill))
        .height(Length::Fixed(geom::DIVIDER_BAND_H))
        .padding([0, 10])
        .class(cosmic::theme::Button::Custom {
            active: Box::new(|_f, t| add_color_style(t, false, false)),
            hovered: Box::new(|_f, t| add_color_style(t, true, false)),
            pressed: Box::new(|_f, t| add_color_style(t, true, true)),
            disabled: Box::new(|t| add_color_style(t, false, false)),
        })
        .on_press(Msg::ColorPicker(ColorPickerMsg::AddToHistory));
    // "Add to recents (⌘Enter)" / "Add to recents (Ctrl+Enter)": the owner's format, the
    // same shape the copy button's card takes, through the same chord formatter.
    static TIP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let tip = TIP.get_or_init(|| {
        format!("Add to recents ({})", crate::shortcuts::add_color_chord_label())
    });
    widget::tooltip(
        crate::widgets::arrow_cursor::arrow_cursor(button),
        widget::text(tip.as_str()).size(12),
        widget::tooltip::Position::Top,
    )
    .into()
}

/// The add-colour button's look, and the reason it is a custom class: it has to be the
/// SLIDER THUMB's dress (`gradient_strip`'s `marker_style`), which no button class in the
/// theme is, since a thumb is opaque window background under a subdued-text hairline
/// while every stock button is a component fill.
///
/// Only the FILL moves between states, to the component's own hover and pressed colours,
/// so the control still answers a pointer without breaking the borrowed look. The
/// rounding is `rounding().xl`, the button token, so the user's "Edge rounding" setting
/// reaches it like every other button here.
fn add_color_style(
    theme: &cosmic::Theme,
    hovered: bool,
    pressed: bool,
) -> cosmic::widget::button::Style {
    let bg = theme.cosmic().background(false);
    let fill: cosmic::iced::Color = if pressed {
        bg.component.pressed.into()
    } else if hovered {
        bg.component.hover.into()
    } else {
        bg.base.into()
    };
    let mut s = cosmic::widget::button::Style::new();
    s.background = Some(Background::Color(fill));
    s.border_radius = theme::rounding(theme).xl.into();
    s.border_width = 1.0;
    s.border_color = theme::subdued(theme);
    s.icon_color = Some(bg.on.into());
    s.text_color = Some(bg.on.into());
    s
}

/// A fixed vertical gap, for the window column whose section gaps the owner sized
/// individually (so one uniform column spacing cannot express them).
/// A reorder's INSERTION LINE (DRAGON-687): a small accent quad, `w` by `h`, marking the
/// slot a drop would land in. Analytic, so it moves per pixel with no raster to rebuild;
/// the accent, because it marks the same "this is where it goes" the zone wash marks.
fn insert_line<'a>(w: f32, h: f32) -> Element<'a, Msg> {
    widget::container(widget::space::Space::new())
        .width(Length::Fixed(w))
        .height(Length::Fixed(h))
        .class(cosmic::theme::Container::custom(|t| cosmic::iced::widget::container::Style {
            background: Some(Background::Color(theme::accent(t))),
            border: Border { radius: 1.0.into(), ..Default::default() },
            ..Default::default()
        }))
        .into()
}

fn vspace<'a>(h: f32) -> Element<'a, Msg> {
    widget::space::Space::new().height(Length::Fixed(h)).into()
}

/// A fixed horizontal gap, same reason, for the rows whose gaps differ pair by pair.
fn hspace<'a>(w: f32) -> Element<'a, Msg> {
    widget::space::Space::new().width(Length::Fixed(w)).into()
}

// ── The split-inputs TOGGLE and the "Copied!" word, both gone (DRAGON-680) ───
//
// `layout_toggle_button` (the lucide list-chevrons pair, showing the remembered
// split-vs-collapsed state) and `copied_word` (the acknowledgement beside the copy
// button, in `theme::success`) both lived here and both were owner-reversed:
//
// * the TOGGLE went with the setting behind it. The layout is a property of the mode now
//   (`geom::splits_components`): hex is one unified box, everything else splits. The
//   persisted `color_picker_split_inputs` is out of the schema, and `state::store`'s
//   retired-key test is what proves an existing config still loads.
// * the WORD went with the row it sat on. It replaced a "Copied!" tooltip card that the
//   app pinned open (DRAGON-676), and that reasoning still holds where it applies: a copy
//   the app performs for you has nobody's pointer on the button, so a hover-only
//   acknowledgement says nothing at the one moment there is something to say. What
//   changed is that the copy button moved into the CONTROLS row, where there is no spare
//   width beside it and nothing left of the mode row to put a word on. The success-green
//   TICK is the acknowledgement now, and it is still up the moment the window opens,
//   because the open-time copy raises the same flash it always did (`flash_copied`).
//
// Do not restore the pinned card to get the word back: `widgets::copy_button`'s own
// tombstone records why the card could not stay.

/// The pick-again pipette: start a new pick, exactly as launching the tool does. It
/// leads the CONTROLS row since DRAGON-630 (the reference layout's eyedropper
/// position); it shared the recents row before that.
///
/// The same lucide `pipette` the tray entry and the editor's toolbar button wear
/// (`MenuIcon::ColorPicker` vendors it, `icons::handle` maps the name), so the tool has one
/// glyph everywhere. Dressed as a BARE ICON BUTTON rather than as a swatch: see
/// [`controls_icon_style`].
fn pick_again_button<'a>() -> Element<'a, Msg> {
    // The BUTTON is the round swatch's own square, so the controls row leads with
    // something the size of what it sits beside. The GLYPH inside it doubled to 32 on
    // DRAGON-630's review and came back to 24 on DRAGON-680's ("the icon itself needs
    // some padding inside of the hoverable circle"), which is the same hover area with
    // 12pt of inset instead of 8.
    controls_icon_button(
        "color-select-symbolic",
        false,
        "Pick another color",
        Msg::ColorPicker(ColorPickerMsg::PickAgain),
    )
}

/// The COPY button, at the pipette's size and immediately to its right (DRAGON-680, the
/// owner's ask). It copies the current mode's whole value, exactly as the value row's
/// smaller copy button did before it moved here.
///
/// It is built from [`controls_icon_button`] rather than from
/// `widgets::copy_button::subtle_copy_button`, because that helper's whole job is the
/// SMALL bare icon button (libcosmic's 16pt glyph in a caller-sized halo) and this one has
/// to be the pipette's twin instead. What it does borrow from that module is everything
/// that must not fork: the two glyph names (`copy_button::glyph_name`), the flash WINDOW
/// (`copied_recently`, so the tick lasts exactly as long as it does everywhere else), and
/// the success green the tick is tinted with. So there is still one "this worked" colour
/// and one copy glyph in the app.
fn copy_value_button<'a>(copied: bool) -> Element<'a, Msg> {
    // "Copy (⌘⇧C)" / "Copy (Ctrl+Shift+C)" (DRAGON-680, the owner's format), through the
    // app's ONE chord formatter so the card cannot describe a different key from the one
    // `keyboard.rs` matches. While the flash is up the card says "Copied!" instead, which
    // is what every other copy control in the app says (`widgets::copy_button`): the chord
    // is a reminder for next time, and this moment has something better to report.
    static TIP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let tip = TIP.get_or_init(|| {
        format!("Copy ({})", crate::shortcuts::copy_value_chord_label())
    });
    controls_icon_button(
        crate::widgets::copy_button::glyph_name(copied),
        copied,
        if copied { "Copied!" } else { tip.as_str() },
        Msg::ColorPicker(ColorPickerMsg::CopyValue),
    )
}

/// One of the controls row's two big bare icon buttons: a [`geom::CONTROLS_ICON`] glyph
/// centred in a [`geom::CONTROLS_BUTTON`] square, no border in any state, the app's
/// ordinary icon-button hover wash, and a tooltip above it.
///
/// `success` tints the glyph the app's success green (the copy button's tick); the
/// pipette always passes `false`.
fn controls_icon_button<'a>(
    icon: &'static str,
    success: bool,
    tip: &'static str,
    press: Msg,
) -> Element<'a, Msg> {
    // The ink is set on the GLYPH ITSELF, and that is the whole of DRAGON-680's copy-tick
    // bug: the button's class does set `icon_color`, and libcosmic's button does pass it
    // down, but the CENTRING CONTAINER between them resolves an icon colour of its own
    // (`Container::Transparent`, the default class, answers `Some(component.on)` for both
    // ink fields) and a symbolic svg only inherits the renderer's colour when nothing
    // nearer has set one. So the success green reached the container and stopped there,
    // and the tick rendered in the ordinary foreground while the GLYPH still swapped to a
    // checkmark, which is exactly what the owner saw.
    //
    // This is the same trap the hex chip's label hit in DRAGON-601, one file over, and the
    // same fix: set the colour at the LEAF, where nothing can shadow it. The button class
    // keeps its `icon_color` too, so anything that does inherit still gets the right
    // answer.
    let ink = crate::widgets::icons::tinted(
        crate::widgets::icons::handle(icon),
        move |t| controls_icon_ink(t, success),
    );
    let glyph = ink.size(geom::CONTROLS_ICON);
    let button = widget::button::custom(
        widget::container(glyph).center_x(Length::Fill).center_y(Length::Fill),
    )
    .width(Length::Fixed(geom::CONTROLS_BUTTON))
    .height(Length::Fixed(geom::CONTROLS_BUTTON))
    .padding(0)
    .class(cosmic::theme::Button::Custom {
        active: Box::new(move |_f, t| controls_icon_style(t, false, false, success)),
        hovered: Box::new(move |_f, t| controls_icon_style(t, true, false, success)),
        pressed: Box::new(move |_f, t| controls_icon_style(t, true, true, success)),
        disabled: Box::new(move |t| controls_icon_style(t, false, false, success)),
    })
    .on_press(press);
    widget::tooltip(
        crate::widgets::arrow_cursor::arrow_cursor(button),
        widget::text(tip).size(12),
        widget::tooltip::Position::Top,
    )
    .into()
}

/// The pipette's look: a BARE ICON BUTTON. No border in any state, and the app's ordinary
/// icon-button fill as its hover affordance (DRAGON-594).
///
/// It used to take the swatches' 1px border and their rounding, so that it would read as part
/// of the recents row. That was the wrong read, and the owner called it: the swatches are
/// colour SAMPLES and the pipette is an ACTION, so dressing the action as a sample invites a
/// click that expects a colour to load. Sharing a row is not sharing a kind.
///
/// What replaces the border is what every other bare icon button in this app already does, so
/// nothing here is invented:
///
/// * the preview toolbar's `chrome::Tb::tool_button` hands its glyph
///   `cosmic::theme::Button::Icon`, whose entire feedback is the theme's `icon_button`
///   component fill: transparent at rest, a wash on hover, a firmer one on press. The three
///   fills read here are exactly the ones that class reads, which is also how
///   `chrome::tool_toggle_style` reconstructs the look when it needs a custom class;
/// * the settings and upload-meter copy controls (`widgets::copy_button`) are the same shape
///   of control and carry no border either, only a faint hover wash;
/// * the preview chrome says it outright, that per-icon rings are gone and every control there
///   speaks one language. This brings the picker window's one ACTION button into that language
///   while the swatches beside it keep theirs.
///
/// The ROUNDING moves with the border, from the swatch token to `rounding().xl`, the token
/// libcosmic gives buttons and button groups. Same reason: a control that rounds like a swatch
/// still reads like one. The user's "Edge rounding" setting still reaches this button, through
/// the BUTTON token rather than the swatch token, so it keeps tracking the theme.
///
/// Pure, and unit-tested below. What is worth pinning is the pair of promises the owner's
/// request actually makes: no state may grow a border back, and borderless must not mean
/// feedback-less.
///
/// DRAGON-680 gave it a `success` arm and a second tenant, the copy button that moved into
/// this row: a bare icon button whose GLYPH turns the app's success green while a copy is
/// fresh. Only the glyph colour moves; the fills, the absent border and the rounding are
/// the pipette's, which is what makes the pair read as one kind of control. A
/// `pick_again_style(t, h, p)` wrapper stood in front of it for one revision and went when
/// its last caller did; the pipette is `success: false`.
fn controls_icon_style(
    theme: &cosmic::Theme,
    hovered: bool,
    pressed: bool,
    success: bool,
) -> cosmic::widget::button::Style {
    let comp = &theme.cosmic().icon_button;
    let fill: cosmic::iced::Color = if pressed {
        comp.pressed.into()
    } else if hovered {
        comp.hover.into()
    } else {
        comp.base.into()
    };
    let mut s = cosmic::widget::button::Style::new();
    s.background = Some(Background::Color(fill));
    s.border_radius = theme::rounding(theme).xl.into();
    // Kept for anything that INHERITS the button's ink, but it is not what tints the
    // glyph: a container between this and the icon would shadow it (see
    // `controls_icon_button`). Both read [`controls_icon_ink`], so they cannot disagree.
    s.icon_color = Some(controls_icon_ink(theme, success));
    s
}

/// **Pure**, unit-tested: the ink a controls-row glyph draws in.
///
/// `success` is the copy button's live flash and false for everything else. The green is
/// `theme::success`, the app's ONE "this worked" colour, read from the same place
/// `copy_button::icon_button_style` reads it so the two copy controls in this process
/// cannot end up two different greens; the resting colour is the ordinary foreground.
///
/// It is a named function rather than two lines inside the style closure because the
/// answer has to reach the screen through TWO paths that used to disagree: the button's
/// `icon_color` (inherited, and shadowed by the centring container) and the glyph's own
/// svg class (what actually draws). One decision, both callers, and a test that pins the
/// table rather than another visual trace.
fn controls_icon_ink(theme: &cosmic::Theme, success: bool) -> cosmic::iced::Color {
    if success {
        theme::success(theme)
    } else {
        theme.cosmic().background(false).on.into()
    }
}

/// The colour history's FOCUS FRAME: `inner` inside the outset that keeps the frame clear
/// of the swatches (DRAGON-680).
///
/// **Outside the block, with air, and clipping nothing** (the owner's veto of the first
/// attempt, which drew the border on the block's own bounds and overlapped the outer
/// swatches' rims). The inset is `geom::HISTORY_FOCUS_OUTSET` on all four sides, and the
/// window column pays for it out of the blank margins around this section rather than out
/// of the grid: see the comment in `color_picker_window_view`, which is where the other
/// half of the arithmetic lives.
///
/// The wrapper is ALWAYS in the tree, lit or not and grid or not, because its inset is part
/// of the window's fixed height. A frame that only existed while focused would make the
/// window's last section jump 12pt every time Tab reached it.
///
/// The ROUNDING is the swatches' own corner token plus the outset, which is what a rounded
/// rectangle offset outwards actually looks like: taking the token unchanged would pinch
/// the frame's corners against the swatches it is standing off from.
fn history_frame<'a>(inner: Element<'a, Msg>, focused: bool) -> Element<'a, Msg> {
    widget::container(inner)
        .width(Length::Fill)
        .padding(geom::HISTORY_FOCUS_OUTSET)
        .class(cosmic::theme::Container::custom(move |t| {
            let r = swatch_radius(t);
            cosmic::iced::widget::container::Style {
                border: Border {
                    radius: cosmic::iced::border::Radius {
                        top_left: r[0] + geom::HISTORY_FOCUS_OUTSET,
                        top_right: r[1] + geom::HISTORY_FOCUS_OUTSET,
                        bottom_right: r[2] + geom::HISTORY_FOCUS_OUTSET,
                        bottom_left: r[3] + geom::HISTORY_FOCUS_OUTSET,
                    },
                    // The same accent outline the value boxes wear when focused.
                    width: if focused { geom::FOCUS_RING_W } else { 0.0 },
                    color: if focused {
                        theme::accent(t)
                    } else {
                        cosmic::iced::Color::TRANSPARENT
                    },
                },
                ..Default::default()
            }
        }))
        .into()
}

/// THE window's focus frame (DRAGON-680): the accent outline a stop wears while the Tab
/// ring is parked on it, laid over whatever look the control already had.
///
/// **It is the value boxes' own focused outline**, deliberately, because the owner asked
/// for the other stops to be highlighted "just like inputs get": libcosmic's focused text
/// input paints its border in the theme's accent at [`geom::FOCUS_RING_W`], so the activator and
/// the history read as the same kind of "you are here" rather than as two inventions. The
/// RADIUS is left to whatever the control already draws, since a chevron unit and a two-row
/// grid do not round like a text field and should not pretend to.
///
/// `focused == false` returns the style untouched, so an unfocused control is
/// byte-identical to what it was before this ticket.
fn focus_frame(
    mut s: cosmic::widget::button::Style,
    theme: &cosmic::Theme,
    focused: bool,
) -> cosmic::widget::button::Style {
    if focused {
        s.border_width = geom::FOCUS_RING_W;
        s.border_color = theme::accent(theme);
    }
    s
}

/// The thumb FILL's opacity on the hue and alpha strips (DRAGON-680, the owner: "lets also
/// make the slider handle background color in the circles 90% opaque so that they are EVER
/// SO SLIGHTLY 10% see through").
///
/// The fill alone. The thumb's subdued RING stays fully opaque, because the ring is what
/// holds the disc's shape against a bright gradient, and so does the "Add to recents" button on
/// the divider, which borrows this thumb's look: that button's opaque fill is what hides
/// the rule running behind it, so fading it would show a line through the middle of a
/// control rather than a hint of gradient.
const STRIP_THUMB_ALPHA: f32 = 0.9;

/// THE corner radius every SWATCH in this window uses: the big one at the top and the recents
/// along the bottom. The pipette that shares the recents row is deliberately NOT one of them
/// (DRAGON-594); it rounds like a button, see [`controls_icon_style`].
///
/// One lookup on purpose (DRAGON-587). The recents were reading `rounding().xs` while the
/// swatch above read `.s`, so with Edge rounding set to Round the top swatch was visibly
/// rounder than the row below it, which is the defect the owner reported. Both read the LIVE
/// theme, which is where the "Edge rounding" setting lands, so unifying the token is the whole
/// fix: switching Round / Slightly Round / Square now moves every swatch together.
fn swatch_radius(theme: &cosmic::Theme) -> [f32; 4] {
    theme::rounding(theme).s
}

/// Absolutely place `content` at `(left, top)` in the surface, by padding a Fill
/// container so the start-aligned child lands there. The same trick `cursor_indicator`
/// and the letterboxed backdrop use; only the leading sides are padded, because a
/// trailing pad would only invite float-jitter clipping.
fn absolute<'a>(content: Element<'a, Msg>, at: (f32, f32)) -> Element<'a, Msg> {
    widget::container(content)
        .padding(cosmic::iced::Padding { top: at.1, right: 0.0, bottom: 0.0, left: at.0 })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// One recent-colour swatch: a fixed square carrying the remembered colour AND its alpha
/// (DRAGON-680), with an accent ring when it is the entry currently loaded, and its hex as
/// the tooltip.
///
/// **Two renderings, one control.** An OPAQUE entry is what it always was: the colour as
/// the button's own flat background, byte-identical to before this ticket. A TRANSLUCENT
/// entry draws `raster` instead, the split swatch the owner asked for (left half the
/// colour at full strength, right half the colour at its real alpha over the checkerboard,
/// `color::recent_swatch_rgba`), and the button's background goes transparent so the
/// checkerboard is not blended with a flat fill underneath it. Everything else, the size,
/// the rounding, the subdued rim, the hover edge, the selection outline, the tooltip and
/// the one `LoadRecent` press, is shared: the two halves are one hoverable, clickable unit
/// because they are one BUTTON with a picture in it, not two controls side by side.
///
/// The raster is built in the update handler like every other raster in this window
/// (`refresh_recent_rasters`); a translucent entry whose raster has not landed yet falls
/// back to the flat opaque fill for that frame rather than drawing an empty box.
///
/// The TOOLTIP spells the alpha when there is one (`#RRGGBBAA`) and stays `#RRGGBB` when
/// there is not, through `ColorFormat::Hex.format_with_alpha`, which is the same spelling
/// the value row and the clipboard use. A tooltip that said `#FF0000` for a half
/// transparent entry would be naming a different colour from the one the swatch shows.
/// The transient facts ONE history swatch needs (DRAGON-682): whether it is the entry the
/// window is showing, whether its menu is open, whether the keyboard cursor is on it, whether
/// a drag is in flight, and whether the pointer is over it.
///
/// They travel together for the same reason [`BarState`]'s do: they are all read once per
/// frame at the caller, they are all "what is happening to this swatch right now", and as
/// five parameters they made an eight-argument function.
#[derive(Clone, Copy, Default)]
struct SwatchState {
    selected: bool,
    menu_open: bool,
    on_cursor: bool,
    dragging: bool,
    hovered: bool,
}

fn recent_swatch<'a>(
    entry: geom::Recent,
    raster: Option<&widget::image::Handle>,
    index: usize,
    state: SwatchState,
    palettes: &'a [geom::Palette],
    page: geom::MenuPage,
    window: (f32, f32),
) -> Element<'a, Msg> {
    let SwatchState { selected, menu_open, on_cursor, dragging, hovered } = state;
    let translucent = entry.alpha != u8::MAX;
    let split = raster.filter(|_| translucent).cloned();
    let fill = cosmic::iced::Color::from_rgb8(entry.color.r, entry.color.g, entry.color.b);
    let painted = split.is_some();
    let style = move |theme: &cosmic::Theme| {
        let mut s = cosmic::widget::button::Style::new();
        // Transparent under the split raster: it carries its own checkerboard, and a flat
        // colour behind it would show through the transparent half and hide the board.
        s.background = Some(Background::Color(if painted {
            cosmic::iced::Color::TRANSPARENT
        } else {
            fill
        }));
        // The SAME radius the big swatch takes (DRAGON-587): one lookup, `swatch_radius`.
        s.border_radius = swatch_radius(theme).into();
        // SUBDUED, the owner's review: the same quiet-text tone the slider thumbs and
        // the round swatch's rim wear, not a near-white/near-black neutral. Read from
        // `swatch_rim`, which the harmony bars' outline reads too (DRAGON-682 item 27).
        (s.border_width, s.border_color) = swatch_rim(theme);
        if hovered {
            // The hover affordance: a brighter edge, since the fill IS the content and must
            // not change. Driven by the window's own hover state rather than by the button's
            // status (DRAGON-682 item 41): the button no longer takes a press, so the
            // toolkit considers it disabled and would never show a hovered style.
            s.border_color = theme::accent(theme);
            s.border_width = 2.0;
        }
        if selected {
            // An outline OUTSIDE the swatch reads as selection without recolouring it,
            // the same affordance the settings accent palette uses.
            s.outline_width = 2.0;
            s.outline_color = theme::accent(theme);
        }
        if on_cursor {
            // The keyboard CURSOR is a border INSIDE the swatch, where the selection's
            // outline is outside it (DRAGON-682 item 7). They are two different facts now
            // that arrowing no longer applies what it passes: one swatch is the colour the
            // window is showing, another is where the keyboard is, and on the swatch where
            // they coincide you see both rings at once, which is exactly right.
            s.border_width = geom::FOCUS_RING_W;
            s.border_color = theme::accent(theme);
        }
        s
    };
    let content: Element<'a, Msg> = match split {
        Some(handle) => widget::image::Image::new(handle)
            .width(Length::Fixed(geom::RECENT_SWATCH))
            .height(Length::Fixed(geom::RECENT_SWATCH))
            .into(),
        None => widget::space::Space::new().width(Length::Fill).height(Length::Fill).into(),
    };
    // **A button with NO `on_press`** (DRAGON-682 item 41), which is not the contradiction it
    // looks like. It is here for its DRESS: the swatch's fill, radius, rim, selection outline
    // and cursor ring are `button::Style` fields, and an outline that draws OUTSIDE the
    // bounds without taking layout space is something only the button style has. What it must
    // NOT do any more is take the press: a cosmic button with an `on_press` CAPTURES the left
    // button, so the `mouse_area` wrapped around it never saw a press begin here, which is
    // why the drag used to guess its source from hover state (`geom`'s tombstone at
    // `drag_source` carries what that cost). With no `on_press` it captures nothing, the
    // toolkit styles it through the `disabled` arm, and every arm here is the same function,
    // so it looks exactly as it did. The click that LOADS the colour moved to the mouse
    // area's release, which is when a button fires anyway.
    let button = widget::button::custom(content)
        .width(Length::Fixed(geom::RECENT_SWATCH))
        .height(Length::Fixed(geom::RECENT_SWATCH))
        .padding(0)
        .class(cosmic::theme::Button::Custom {
            active: Box::new(move |_f, t| style(t)),
            hovered: Box::new(move |_f, t| style(t)),
            pressed: Box::new(move |_f, t| style(t)),
            disabled: Box::new(move |t| style(t)),
        });
    // The press NAMES this entry (item 41), the release LOADS it (which is when the button
    // used to fire), the right press opens the menu, and the hover pair drives both the hover
    // edge above and the Backspace / Delete key, which carries no position:
    // `geom::remove_target` removes what the pointer is OVER. The exit names its own index so
    // the enter of a neighbour the pointer has already reached cannot be cancelled by the exit
    // that follows it.
    //
    // The GRAB hand (DRAGON-682 item 40) is FORCED, because `mouse_area`'s own
    // `.interaction()` only speaks when its content reports `Interaction::None`.
    let swatch = crate::widgets::force_cursor::force_cursor(
        widget::mouse_area(crate::widgets::arrow_cursor::arrow_cursor(button))
            .on_press(Msg::ColorPicker(ColorPickerMsg::DragPressed(
                geom::DragSource::Recent(index),
            )))
            .on_release(Msg::ColorPicker(ColorPickerMsg::RecentReleased(index)))
            .on_right_press(Msg::ColorPicker(ColorPickerMsg::RecentsMenu(Some(index))))
            .on_enter(Msg::ColorPicker(ColorPickerMsg::RecentHovered(index)))
            .on_exit(Msg::ColorPicker(ColorPickerMsg::RecentUnhovered(index))),
        cosmic::iced::mouse::Interaction::Grab,
    );
    // While ITS menu is open the swatch drops its tooltip, for the reason
    // `widgets::copy_button`'s tombstone gives: a hover card beside an open panel is two
    // answers to one question. The flyout replaces the tooltip for as long as it is up.
    if menu_open {
        return recents_menu(swatch, index, entry, palettes, page, window);
    }
    // A live DRAG silences every card in the window, the harmony swatches' and these
    // (DRAGON-682 item 35): the pointer is carrying something, and a hover card about the
    // swatch it is passing over answers a question nobody asked.
    if dragging {
        return swatch;
    }
    let hex = swatch_hex_tip(entry.color, entry.alpha);
    // A swatch the KEYBOARD is on shows its hex without waiting for a pointer
    // (DRAGON-682 item 9), through the same pinned card the panel uses; hover and
    // pinned are the ONE adaptive card since item seven.
    if on_cursor {
        return pinned_tip(swatch, hex, entry.color);
    }
    hover_tip(swatch, hex, entry.color)
}

/// An EMPTY history position (DRAGON-682 item 8): a 1px dotted outline in the theme's
/// subdued ink, and nothing else.
///
/// **A placeholder, not a control.** No button, no mouse area, no tooltip, and neither the
/// arrows nor the Tab ring can land on it: it exists so the grid's full extent is visible
/// before the history fills up, which is also why the "Colors you pick appear here." line
/// it replaced is gone.
///
/// The dots are a RASTER (`color::dotted_outline_rgba`, one shared handle for every slot)
/// because iced draws a border as a solid signed-distance ring with no dash pattern. It is
/// built beside the swatch rasters, so it follows the theme's subdued tone and the "Edge
/// rounding" setting rather than being baked to one palette.
fn empty_slot<'a>(raster: Option<&widget::image::Handle>) -> Element<'a, Msg> {
    // A missing raster draws a SOLID hairline slot rather than nothing (DRAGON-682 item
    // 13). The rasters are built before the window opens, so this should never be seen;
    // it exists because the failure it replaces was invisible in the worst way, an empty
    // block that looks exactly like a window that has not finished loading. A slot that
    // degrades to a slightly different slot cannot do that.
    match raster {
        Some(_) => raster_image(raster, geom::RECENT_SWATCH, geom::RECENT_SWATCH),
        None => widget::container(widget::space::Space::new())
            .width(Length::Fixed(geom::RECENT_SWATCH))
            .height(Length::Fixed(geom::RECENT_SWATCH))
            .class(cosmic::theme::Container::custom(|t| {
                cosmic::iced::widget::container::Style {
                    border: Border {
                        radius: swatch_radius(t).into(),
                        width: 1.0,
                        color: theme::subdued(t),
                    },
                    ..Default::default()
                }
            }))
            .into(),
    }
}

// **Tombstone: `grab_hand` / `grabbing_hand`** (DRAGON-685, added and reverted the same
// day at the owner's call). winit's Windows backend maps `Grab`/`Grabbing` to
// `IDC_SIZEALL` — Windows ships no grab hand, open or closed — and for one build the
// picker's drag surfaces substituted the pointing hand (`Pointer` → `IDC_HAND`) there.
// The owner reverted it for CONSISTENCY: the preview editor's drag surfaces (zoom_pan,
// crop, annotations) all show the stock `IDC_SIZEALL` on Windows, so a picker that shows
// a hand makes the same gesture read as two different things one window apart. If grab
// hands ever come back, they come back for EVERY drag surface at the winit or iced layer
// (real hand cursors need custom cursor art — no native shape exists), not as a per-site
// interaction substitution here.

/// The panel's EXPAND / COLLAPSE toggle (DRAGON-682), in the header's end region — except
/// on Windows with the native caption cluster, where the SAME button floats in its own
/// stack layer beside the cluster (DRAGON-685; `color_picker_window_view`'s layer block
/// carries the geometry and the why).
///
/// Modelled on the settings window's nav toggle, mirrored: same 16pt symbolic glyph and the
/// same compact icon-button box. The glyphs are lucide's right-handed pair, vendored beside
/// the left-handed pair the settings toggle uses (`widgets::icons`,
/// `panel-open-right-symbolic` / `panel-close-right-symbolic`), which is the "maybe mirrored
/// variants" the owner asked for made literal.
///
/// **The icon names the STATE, not the action** (DRAGON-682 item 14). It shipped the other
/// way round for one build, on the settings toggle's own stated rule, and the owner read it
/// as reversed: so a COLLAPSED window shows the glyph whose panel edge is closed, and an
/// EXPANDED one shows the glyph whose panel edge is open. Do not "restore consistency" with
/// the settings toggle without asking; this pair was looked at and called backwards.
///
/// The TOOLTIP says the action in words (item 18), so the two together cannot be ambiguous:
/// the picture is what the window IS, the words are what the click DOES.
fn panel_toggle<'a>(expanded: bool) -> Element<'a, Msg> {
    let icon = crate::widgets::icons::tinted(
        crate::widgets::icons::handle(if expanded {
            "panel-open-right-symbolic"
        } else {
            "panel-close-right-symbolic"
        }),
        |t| controls_icon_ink(t, false),
    )
    .size(geom::PANEL_TOGGLE_ICON);
    let button = widget::button::custom(
        widget::container(icon).center_x(Length::Fill).center_y(Length::Fill),
    )
    .width(Length::Fixed(geom::PANEL_TOGGLE_W))
    .height(Length::Fixed(geom::PANEL_TOGGLE_W))
    .padding(0)
    .class(cosmic::theme::Button::Icon)
    .on_press(Msg::ColorPicker(ColorPickerMsg::TogglePanel));
    widget::tooltip(
        crate::widgets::arrow_cursor::arrow_cursor(button),
        // The owner's own wording (item 18). It names the ACTION, which is the half the
        // flipped glyph no longer carries.
        widget::text(if expanded { "Hide Palettes Panel" } else { "Show Palettes Panel" })
            .size(12),
        // DOWNWARD, unlike every other tooltip in this window: this control lives in the
        // header, so there is nothing above it but the window's own edge.
        widget::tooltip::Position::Bottom,
    )
    .into()
}

/// The panel's TAB STRIP (DRAGON-682 item 12): the SETTINGS window's own strip.
///
/// `widget::tab_bar::horizontal` over a `segmented_button` model, with the same
/// `button_alignment`, the same `button_spacing` (the theme's `space_xxs`, whose default is
/// 0 and jams the icon against its label) and the same `arrow_cursor` wrapper
/// `settings::mod`'s `tab_strip` uses. Matching the settings strip "including icons" means
/// BEING it: a hand-built pair of buttons stood here for one commit and the owner rejected
/// it on sight, and an approximation of a toolkit widget is a promise to keep approximating
/// it after every bump.
///
/// Wrapped in `hover_redraw` (DRAGON-681, adopted here once both landed on main): the same
/// tab-to-tab hover starvation the settings strips had, for the same reason, so see that
/// module's doc.
fn panel_tabs(model: &widget::segmented_button::SingleSelectModel) -> Element<'_, Msg> {
    let gap = cosmic::theme::active().cosmic().space_xxs();
    crate::widgets::arrow_cursor::arrow_cursor(crate::widgets::hover_redraw(
        widget::tab_bar::horizontal(model)
            .button_alignment(Alignment::Center)
            .button_spacing(gap)
            .on_activate(|entity| Msg::ColorPicker(ColorPickerMsg::PanelTab(entity))),
    ))
}


// **Tombstone: `palettes_placeholder`** (DRAGON-682 item 39, retired by DRAGON-687). The
// Saved Palettes tab was staged as a placeholder while Harmonies shipped first, and a
// colour dropped on it was answered with a "coming soon" card naming what WOULD have been
// filed (`ColorPickerState::palette_notice` carried it, outliving the drag on purpose so
// the reverting tab could not take the message with it). The real tab files real drops
// now (`App::palettes_tab` and the drop matrix's palette rows), so the placeholder, the
// notice state and the `DropAction::PaletteNotice` arm all retired together.

/// The CREATE row (DRAGON-687), grown into the tab's TOOLBAR by item six: the sort and
/// search icons left-aligned as a pair (the owner's lucide `list-sort-descending` and
/// `search`), the fill, then the "New Palette" button holding the top right the owner
/// gave it. The row height is fixed and the scroll arithmetic counts it
/// (`geom::PALETTE_CREATE_ROW_H`).
///
/// SORT lives here now, not in the group-name menus: sorting is a LIST operation, and
/// hanging it off one group's name implied it acted on that group. Its flyout is the
/// menus' own machinery (`menu_fit` from `geom::sort_icon_anchor`).
///
/// SEARCH expands in place into the app's one search construction
/// (`widgets::search_input`, the settings header's field), filtering live; while
/// expanded the icon pair becomes icon-plus-field, and everything else stays put.
///
/// The create button is TEXT-ONLY: it briefly wore a leading lucide `plus`
/// (`leading_icon`) and the owner reverted it in the drag-jump round.
fn create_palette_row<'a>(cp: &'a super::ColorPickerState) -> Element<'a, Msg> {
    let sort_btn = toolbar_icon_button(
        "sort-palettes-symbolic",
        // The label the submenu row used to wear (`SORT_GROUPS_LABEL`), now the icon's
        // tooltip: same words, new home.
        geom::SORT_GROUPS_LABEL,
        Msg::ColorPicker(ColorPickerMsg::SortMenu(!cp.sort_menu_open)),
    );
    let sort: Element<'a, Msg> = if cp.sort_menu_open {
        sort_menu(sort_btn, cp.window_size())
    } else {
        sort_btn
    };
    let search: Element<'a, Msg> = if cp.palette_search_active {
        crate::widgets::search_input(
            "Search palettes",
            &cp.palette_search,
            cp.palette_search_id.clone(),
            geom::PALETTE_SEARCH_W,
            |q| Msg::ColorPicker(ColorPickerMsg::PaletteSearchInput(q)),
            Msg::ColorPicker(ColorPickerMsg::PaletteSearchClear),
            // Losing focus EMPTY collapses back to the icon; a live query keeps the
            // field up, so a filter is never applied invisibly.
            Some(Msg::ColorPicker(ColorPickerMsg::PaletteSearchUnfocused)),
        )
    } else {
        toolbar_icon_button(
            "system-search-symbolic",
            "Search palettes",
            Msg::ColorPicker(ColorPickerMsg::PaletteSearchActivate),
        )
    };
    // TEXT-ONLY (the owner reverted the leading plus of the earlier follow-up); the
    // vendored glyph stays for the per-group plus buttons.
    let button = crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::standard("New Palette")
            .on_press(Msg::ColorPicker(ColorPickerMsg::CreatePalette)),
    );
    let button = widget::tooltip(
        button,
        widget::text("Create a new palette").size(12),
        widget::tooltip::Position::Bottom,
    );
    widget::container(
        widget::row(vec![
            sort,
            hspace(geom::PALETTE_PLUS_GAP),
            search,
            widget::space::Space::new().width(Length::Fill).into(),
            button.into(),
        ])
        .align_y(Alignment::Center),
    )
    .height(Length::Fixed(geom::PALETTE_CREATE_ROW_H))
    .width(Length::Fill)
    // The same right inset the tab strip and the scrolled content take (item 16), so the
    // button's right edge lines up with the cards below it.
    .padding(cosmic::iced::Padding {
        top: 0.0,
        right: geom::PANEL_SCROLLBAR_GAP,
        bottom: 0.0,
        left: 0.0,
    })
    .into()
}

/// One toolbar ICON button (item six): the pipette/plus buttons' exact box (a
/// history-swatch square, `Button::Icon`, 14px tinted glyph) so the create row's pair
/// reads as the same family as the per-group pair below it.
fn toolbar_icon_button<'a>(icon: &str, tip: &'static str, msg: Msg) -> Element<'a, Msg> {
    let icon = crate::widgets::icons::tinted(crate::widgets::icons::handle(icon), |t| {
        controls_icon_ink(t, false)
    })
    .size(14);
    let button = widget::button::custom(
        widget::container(icon).center_x(Length::Fill).center_y(Length::Fill),
    )
    .width(Length::Fixed(geom::RECENT_SWATCH))
    .height(Length::Fixed(geom::RECENT_SWATCH))
    .padding(0)
    .class(cosmic::theme::Button::Icon)
    .on_press(msg);
    widget::tooltip(
        crate::widgets::arrow_cursor::arrow_cursor(button),
        widget::text(tip).size(12),
        widget::tooltip::Position::Bottom,
    )
    .into()
}

/// The relocated SORT flyout (item six): the six sorts that were the group-name menus'
/// submenu, now hanging off the toolbar's sort icon, in the shared menu dress, fitted
/// from `geom::sort_icon_anchor` so it opens downward into the list it sorts.
fn sort_menu<'a>(anchor: Element<'a, Msg>, window: (f32, f32)) -> Element<'a, Msg> {
    let rows: Vec<Element<'a, Msg>> = geom::PaletteSort::ALL
        .into_iter()
        .map(|s| {
            menu_row(s.label().to_string(), Msg::ColorPicker(ColorPickerMsg::SortGroups(s)))
        })
        .collect();
    let width =
        geom::menu_width_for_labels(geom::PaletteSort::ALL.into_iter().map(|s| s.label()));
    let menu_anchor = geom::sort_icon_anchor();
    let (x, y) = geom::menu_fit(
        menu_anchor,
        geom::RECENT_SWATCH,
        menu_anchor.0,
        (width, geom::menu_panel_h_for(rows.len())),
        window,
    );
    crate::app::preview::chrome::flyout(
        anchor,
        menu_panel(rows, width),
        crate::app::preview::chrome::FlyoutDir::At { x, y },
        Msg::ColorPicker(ColorPickerMsg::SortMenu(false)),
    )
}

/// The inline RENAME editor (DRAGON-687): a text input dressed to be "mostly transparent
/// to the user" (the owner's words): no fill, no border in any state, just the name's own
/// text with a caret in it, at the heading's place.
///
/// `select_on_focus` is what selects the whole name when the edit begins (the focus task
/// `App::begin_rename` issues goes through libcosmic's `State::focus`, which consults the
/// flag), the value boxes' own mechanism. Enter commits through `on_submit`; a click that
/// takes the focus elsewhere commits through `on_unfocus`, so a name is never silently
/// lost to a stray click; Escape reverts in `keyboard.rs`.
fn rename_input<'a>(cp: &'a super::ColorPickerState, draft: &'a str) -> Element<'a, Msg> {
    widget::text_input("Palette name", draft)
        .id(cp.rename_id.clone())
        .on_input(|s| Msg::ColorPicker(ColorPickerMsg::RenameEdited(s)))
        .on_submit(|_| Msg::ColorPicker(ColorPickerMsg::RenameCommitted))
        .on_unfocus(Msg::ColorPicker(ColorPickerMsg::RenameCommitted))
        .select_on_focus(true)
        // The title's own budget (the UX round), so the right-aligned icons stay put
        // while the editor is up; the FULL name is always shown and edited here, the
        // ellipsis being display-only.
        .width(Length::Fixed(geom::palette_title_w()))
        .style(transparent_input_style())
        .into()
}

/// The rename editor's dress: cosmic's own input with every fill and border stripped, in
/// every state, so what the user sees is the text and the caret and nothing box-shaped.
/// The softened selection stays (`theme::soften_selection`, the app-wide rule), because
/// select-all-on-focus is the first thing this editor does and the selection is the one
/// piece of chrome it needs.
fn transparent_input_style() -> cosmic::theme::TextInput {
    use cosmic::widget::text_input::StyleSheet as _;
    fn strip(
        a: cosmic::widget::text_input::Appearance,
        t: &cosmic::Theme,
    ) -> cosmic::widget::text_input::Appearance {
        let mut a = theme::soften_selection(a, t);
        a.background = Background::Color(cosmic::iced::Color::TRANSPARENT);
        a.border_color = cosmic::iced::Color::TRANSPARENT;
        a.border_width = 0.0;
        a
    }
    cosmic::theme::TextInput::Custom {
        active: Box::new(|t| strip(t.active(&cosmic::theme::TextInput::Default), t)),
        error: Box::new(|t| strip(t.error(&cosmic::theme::TextInput::Default), t)),
        hovered: Box::new(|t| strip(t.hovered(&cosmic::theme::TextInput::Default), t)),
        focused: Box::new(|t| strip(t.focused(&cosmic::theme::TextInput::Default), t)),
        disabled: Box::new(|t| strip(t.disabled(&cosmic::theme::TextInput::Default), t)),
    }
}

/// A palette's PIPETTE button (DRAGON-687 follow-up): the owner's lucide `pipette`,
/// BEFORE the plus, dressed exactly like it. It starts a fresh screen pick whose colour
/// lands DIRECTLY in this group (`ColorPickerMsg::PickToPalette` carries the
/// cross-process design), never on the main tool swatch.
fn palette_pipette_button<'a>(group: usize) -> Element<'a, Msg> {
    let icon = crate::widgets::icons::tinted(
        crate::widgets::icons::handle("color-select-symbolic"),
        |t| controls_icon_ink(t, false),
    )
    .size(14);
    let button = widget::button::custom(
        widget::container(icon).center_x(Length::Fill).center_y(Length::Fill),
    )
    .width(Length::Fixed(geom::RECENT_SWATCH))
    .height(Length::Fixed(geom::RECENT_SWATCH))
    .padding(0)
    .class(cosmic::theme::Button::Icon)
    .on_press(Msg::ColorPicker(ColorPickerMsg::PickToPalette(group)));
    widget::tooltip(
        crate::widgets::arrow_cursor::arrow_cursor(button),
        // The owner's exact wording.
        widget::text("Add color from picker").size(12),
        widget::tooltip::Position::Top,
    )
    .into()
}

/// A palette's PLUS button (DRAGON-687): the owner's lucide `plus`, a history-swatch
/// square at the bar's right, with the specified tooltip. It appends the window's CURRENT
/// colour; a colour the palette already holds is a no-op (`geom::palette_append`).
fn palette_plus_button<'a>(group: usize) -> Element<'a, Msg> {
    let icon = crate::widgets::icons::tinted(
        crate::widgets::icons::handle("list-add-symbolic"),
        |t| controls_icon_ink(t, false),
    )
    .size(14);
    let button = widget::button::custom(
        widget::container(icon).center_x(Length::Fill).center_y(Length::Fill),
    )
    .width(Length::Fixed(geom::RECENT_SWATCH))
    .height(Length::Fixed(geom::RECENT_SWATCH))
    .padding(0)
    .class(cosmic::theme::Button::Icon)
    .on_press(Msg::ColorPicker(ColorPickerMsg::AddActiveToPalette(group)));
    widget::tooltip(
        crate::widgets::arrow_cursor::arrow_cursor(button),
        // The owner's exact wording.
        widget::text("Add current color").size(12),
        widget::tooltip::Position::Top,
    )
    .into()
}

/// ONE segment of a saved palette's bar (DRAGON-687): [`segment`]'s construction with
/// this tab's own identity, menu and drag source. A parallel function rather than more
/// parameters on `segment`, so the harmony path stays byte-identical.
///
/// `at.0` is the visible ROW (the drag machine's and the cursor's space); `real` is the
/// group's FULL-list index, which is what the menu's mutating messages carry (item six).
#[allow(clippy::too_many_arguments)]
fn palette_segment<'a>(
    c: Srgb,
    alpha: u8,
    at: (usize, usize),
    real: usize,
    w: f32,
    corners: [bool; 2],
    menu_open: bool,
    tip: geom::SwatchTip,
    palettes: &'a [geom::Palette],
    page: geom::MenuPage,
    menu_anchor: (f32, f32),
    window: (f32, f32),
) -> Element<'a, Msg> {
    let fill = cosmic::iced::Color::from_rgba8(c.r, c.g, c.b, f32::from(alpha) / 255.0);
    let (first, last) = (corners[0], corners[1]);
    let face = widget::container(widget::space::Space::new())
        .width(Length::Fixed(w))
        .height(Length::Fixed(geom::PANEL_SWATCH))
        .class(cosmic::theme::Container::custom(move |t| {
            let r = swatch_radius(t)[0];
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(fill)),
                border: Border {
                    radius: cosmic::iced::border::Radius {
                        top_left: if first { r } else { 0.0 },
                        bottom_left: if first { r } else { 0.0 },
                        top_right: if last { r } else { 0.0 },
                        bottom_right: if last { r } else { 0.0 },
                    },
                    // No border of its own: the bar wears one rim around the run,
                    // exactly the harmony bars' rule.
                    width: 0.0,
                    color: cosmic::iced::Color::TRANSPARENT,
                },
                ..Default::default()
            }
        }));
    let seg = crate::widgets::force_cursor::force_cursor(
        widget::mouse_area(face)
            .on_right_press(Msg::ColorPicker(ColorPickerMsg::PaletteSwatchMenu(Some(at))))
            // The press NAMES this swatch (item 41's rule): what a drag carries is what
            // was pressed. The RELEASE completes a click (DRAGON-687 item five reversed
            // the click-does-nothing rule): a sub-threshold press-release applies this
            // swatch as the current colour, bumping the previous one into the recents.
            .on_press(Msg::ColorPicker(ColorPickerMsg::DragPressed(
                geom::DragSource::PaletteSwatch(at.0, at.1),
            )))
            .on_release(Msg::ColorPicker(ColorPickerMsg::PaletteSwatchReleased(
                at.0, at.1,
            ))),
        cosmic::iced::mouse::Interaction::Grab,
    );
    if menu_open {
        return palette_swatch_menu(
            seg, c, alpha, at, real, palettes, page, menu_anchor, window,
        );
    }
    match tip {
        geom::SwatchTip::Silent => seg,
        geom::SwatchTip::Copied => {
            pinned_tip(seg, crate::widgets::copy_button::COPIED_LABEL.to_string(), c)
        }
        geom::SwatchTip::PinnedHex => pinned_tip(seg, swatch_hex_tip(c, alpha), c),
        geom::SwatchTip::Hover => hover_tip(seg, swatch_hex_tip(c, alpha), c),
    }
}

/// One MENU row, the shape every context menu here draws (extracted by DRAGON-687 so the
/// five menus cannot drift): a fixed-height text button in the notation menu's dress.
fn menu_row<'a>(label: String, msg: Msg) -> Element<'a, Msg> {
    crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::custom(
            widget::container(widget::text(label).size(geom::MODE_LABEL_SIZE))
                .center_y(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fixed(geom::MODE_MENU_ITEM_H))
        .padding([0, geom::MODE_MENU_ROW_PAD])
        .class(cosmic::theme::Button::Text)
        .on_press(msg),
    )
}

/// A submenu-opening row: the label with the [`geom::SUBMENU_MARK`] the owner's `>`
/// spells, turning the open menu to `page` in place.
fn submenu_row<'a>(label: &str, page: geom::MenuPage) -> Element<'a, Msg> {
    menu_row(
        format!("{label}{}", geom::SUBMENU_MARK),
        Msg::ColorPicker(ColorPickerMsg::MenuPageChanged(page)),
    )
}

/// The rows of a MOVE/COPY/ADD target page: one per target palette, named, ellipsized by
/// the panel's own cap ([`geom::menu_width_for_labels`] bounds the panel; a longer name
/// simply fills the row).
fn palette_target_rows<'a>(
    palettes: &'a [geom::Palette],
    exclude: Option<usize>,
    mk: impl Fn(usize) -> Msg,
) -> Vec<Element<'a, Msg>> {
    geom::palette_targets(palettes.len(), exclude)
        .into_iter()
        .map(|g| menu_row(palettes[g].name.clone(), mk(g)))
        .collect()
}

/// The panel that wraps a menu's rows at `width`: the notation menu's own container.
fn menu_panel<'a>(rows: Vec<Element<'a, Msg>>, width: f32) -> Element<'a, Msg> {
    widget::container(widget::column(rows).spacing(geom::MODE_MENU_GAP).width(Length::Fill))
        .width(Length::Fixed(width))
        .padding(geom::MODE_MENU_PAD)
        .class(cosmic::theme::Container::custom(mode_menu_style))
        .into()
}

/// A palette SWATCH's context menu (DRAGON-687): the owner's three shared entries (set
/// active, add to recents, copy), plus "Move to palette ›" and "Copy to palette ›" when
/// more than one palette exists. The submenus swap the panel's rows in place
/// (`geom::MenuPage`), which is this window's whole submenu mechanism.
/// `at` is ROW space (where the menu hangs); `real` is the group's full-list index, and
/// it is what every message below carries and what the target lists exclude, so a menu
/// opened under a search filter moves and removes from the palette the user sees, not
/// whichever one happens to share its row number (item six).
#[allow(clippy::too_many_arguments)]
fn palette_swatch_menu<'a>(
    anchor: Element<'a, Msg>,
    c: Srgb,
    alpha: u8,
    at: (usize, usize),
    real: usize,
    palettes: &'a [geom::Palette],
    page: geom::MenuPage,
    menu_anchor: (f32, f32),
    window: (f32, f32),
) -> Element<'a, Msg> {
    let offers = geom::offers_move_copy_to_palette(palettes.len());
    let (rows, width): (Vec<Element<'a, Msg>>, f32) = match page {
        geom::MenuPage::MoveTo if offers => (
            palette_target_rows(palettes, Some(real), |to| {
                Msg::ColorPicker(ColorPickerMsg::MovePaletteColor { from: (real, at.1), to })
            }),
            geom::menu_width_for_labels(palettes.iter().map(|p| p.name.as_str())),
        ),
        geom::MenuPage::CopyTo if offers => (
            palette_target_rows(palettes, Some(real), |to| {
                Msg::ColorPicker(ColorPickerMsg::CopyPaletteColor { from: (real, at.1), to })
            }),
            geom::menu_width_for_labels(palettes.iter().map(|p| p.name.as_str())),
        ),
        _ => {
            let mut rows = vec![
                menu_row(
                    geom::SET_ACTIVE_LABEL.to_string(),
                    // ONE path with the plain click (DRAGON-687 item five): apply and
                    // bump the outgoing colour into the recents.
                    Msg::ColorPicker(ColorPickerMsg::ApplySwatch(c, alpha)),
                ),
                menu_row(
                    geom::ADD_TO_RECENTS_LABEL.to_string(),
                    Msg::ColorPicker(ColorPickerMsg::AddColorToRecents(c, alpha)),
                ),
                menu_row(
                    geom::COPY_COLOR_LABEL.to_string(),
                    Msg::ColorPicker(ColorPickerMsg::CopyColor(c, alpha)),
                ),
            ];
            if offers {
                rows.push(submenu_row(geom::MOVE_TO_PALETTE_LABEL, geom::MenuPage::MoveTo));
                rows.push(submenu_row(geom::COPY_TO_PALETTE_LABEL, geom::MenuPage::CopyTo));
            }
            // LAST (DRAGON-687 follow-up), the destructive entry's usual place: the same
            // removal the drag-off performs, through the same message, no confirmation
            // (colours never confirm; groups do).
            rows.push(menu_row(
                geom::REMOVE_FROM_PALETTE_LABEL.to_string(),
                Msg::ColorPicker(ColorPickerMsg::RemovePaletteColor(real, at.1)),
            ));
            let width = geom::menu_width_for_labels(
                [
                    geom::SET_ACTIVE_LABEL,
                    geom::ADD_TO_RECENTS_LABEL,
                    geom::COPY_COLOR_LABEL,
                    geom::MOVE_TO_PALETTE_LABEL,
                    geom::COPY_TO_PALETTE_LABEL,
                    geom::REMOVE_FROM_PALETTE_LABEL,
                ]
                .into_iter(),
            );
            (rows, width)
        }
    };
    // FITTED from THIS page's size (DRAGON-687 follow-up), harmony_menu's own recipe
    // over the palette bar's geometry.
    let n = palettes.get(real).map_or(1, |p| p.colors.len().max(1));
    let desired_left = menu_anchor.0 - geom::harmony_menu_dx(at.1, n, width);
    let (x, y) = geom::menu_fit(
        menu_anchor,
        geom::PANEL_SWATCH,
        desired_left,
        (width, geom::menu_panel_h_for(rows.len())),
        window,
    );
    crate::app::preview::chrome::flyout(
        anchor,
        menu_panel(rows, width),
        crate::app::preview::chrome::FlyoutDir::At { x, y },
        Msg::ColorPicker(ColorPickerMsg::PaletteSwatchMenu(None)),
    )
}

/// A GROUP NAME's context menu (DRAGON-687): Delete palette, behind the confirmation.
/// The six sorts lived here as a submenu until item six of the follow-up run moved them
/// to the create row's sort icon: sorting is a LIST operation, and hanging it off one
/// group's name implied it acted on that group. `group` is the REAL palette index; the
/// caller resolves the filtered row before building this.
fn group_menu<'a>(
    anchor: Element<'a, Msg>,
    group: usize,
    menu_anchor: (f32, f32),
    window: (f32, f32),
) -> Element<'a, Msg> {
    let rows: Vec<Element<'a, Msg>> = vec![menu_row(
        geom::DELETE_PALETTE_LABEL.to_string(),
        Msg::ColorPicker(ColorPickerMsg::RequestDeleteGroup(group)),
    )];
    let width = geom::menu_width_for_labels([geom::DELETE_PALETTE_LABEL].into_iter());
    // FITTED from THIS page's size (DRAGON-687 follow-up). The heading sits at the
    // panel content's left edge, where a left-aligned panel always fits horizontally;
    // the fit earns its keep near the window's bottom edge.
    let (x, y) = geom::menu_fit(
        menu_anchor,
        geom::PALETTE_TITLE_ROW_H,
        menu_anchor.0,
        (width, geom::menu_panel_h_for(rows.len())),
        window,
    );
    crate::app::preview::chrome::flyout(
        anchor,
        menu_panel(rows, width),
        crate::app::preview::chrome::FlyoutDir::At { x, y },
        Msg::ColorPicker(ColorPickerMsg::GroupMenu(None)),
    )
}

/// One harmony group: a heading over a card of swatches, in the settings window's card
/// style (the owner's ask: "we can treat these like the item groups in the settings window
/// in the card style and the group names").
///
/// The card is the settings' `section_card` shape reduced to what one row of swatches
/// needs: the same `theme::pill_fill` material, the same corner token, the same 8pt between
/// a heading and its card. It is NOT a call into `settings::section_card`, which is a
/// private method on the settings page that builds `Item` rows with reset buttons and
/// search text; borrowing the LOOK is the point, and borrowing the machinery would drag a
/// whole page model into a panel that has none.
fn harmony_group<'a>(
    h: crate::color::Harmony,
    bar: Element<'a, Msg>,
    carded: bool,
) -> Element<'a, Msg> {
    // The settings-card dress is a PARAMETER, not a deletion (DRAGON-682 item 27): the
    // owner took it off the HARMONIES groups specifically, and what Saved Palettes wants
    // when it has content is a later decision. A carded group keeps the settings shape it
    // had; an uncarded one is a heading over a bar, and the bar's own rim is what gives it
    // an edge.
    let card: Element<'a, Msg> = if carded {
        widget::container(bar)
            .width(Length::Fixed(geom::card_w()))
            .padding(geom::PANEL_CARD_PAD)
            .class(cosmic::theme::Container::custom(|theme| {
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(theme::pill_fill(
                        theme,
                        theme::PILL_ALPHA,
                    ))),
                    border: Border {
                        radius: theme::rounding(theme).s.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }))
            .into()
    } else {
        bar
    };
    // The heading, then its one-sentence explainer (DRAGON-682 item 23): a subdued question
    // mark that is hover-only chrome, not a control. No button, no mouse area, nothing in
    // the Tab ring and no keyboard cursor stop; a tooltip is its whole behaviour.
    let hint = widget::tooltip(
        crate::widgets::icons::tinted(
            crate::widgets::icons::handle("help-hint-symbolic"),
            theme::subtle,
        )
        .size(geom::PANEL_HINT_ICON),
        widget::text(h.hint()).size(12),
        widget::tooltip::Position::Top,
    );
    let heading = widget::row(vec![widget::text::heading(h.label()).into(), hint.into()])
        .spacing(geom::PANEL_HINT_GAP)
        .align_y(Alignment::Center);
    widget::column(vec![heading.into(), card])
        .spacing(geom::PANEL_HEADING_GAP)
        .width(Length::Fill)
        .into()
}

/// THE rim a FILLED swatch wears: one point of the app's subdued tone (DRAGON-682 item 27).
///
/// Read by the history swatches and by the harmony bars' outline, so "the same border the
/// filled recent swatches get" is one lookup rather than two that agree today. The WEIGHT is
/// restated here because libcosmic exposes no token for it and this app has always drawn a
/// swatch's edge at a point; the COLOUR is the theme's, so it follows the palette.
fn swatch_rim(theme: &cosmic::Theme) -> (f32, cosmic::iced::Color) {
    (1.0, theme::subdued(theme))
}

/// THE hex spelling a swatch's tooltip shows, history and harmony alike (DRAGON-682).
///
/// One function so the two surfaces cannot drift: an opaque swatch reads `#RRGGBB` and a
/// translucent one `#RRGGBBAA`, which is the same rule, and the same formatter, the value
/// row and the clipboard use.
fn swatch_hex_tip(c: Srgb, alpha: u8) -> String {
    ColorFormat::Hex.format_with_alpha(c, alpha)
}

/// **Pure**, unit-tested: the ROOT's forced cursor, item 40's closed hand exactly while
/// a drag is live and pass-through otherwise (DRAGON-687's drag-jump round).
///
/// The VALUE is the only thing allowed to vary here. The wrapper it feeds
/// (`force_cursor_maybe`) is permanent, and so is the outer stack: the drag-jump bug
/// was the root chain changing SHAPE on this exact flag (wrapper in, stack materialized
/// the frame a drag went live), which mis-aligned iced's positional tree diff and
/// rebuilt the panel scrollable's offset at zero. Keeping the shape fixed and varying
/// only this value IS the fix, so this function is deliberately the whole surface the
/// drag state has on the root chain.
fn root_cursor(dragging: bool) -> Option<cosmic::iced::mouse::Interaction> {
    dragging.then_some(cosmic::iced::mouse::Interaction::Grabbing)
}

/// THE swatch card (DRAGON-687 item seven): every card a swatch shows, hover-summoned
/// or app-pinned, built HERE, in the overlay hex chip's own adaptive dress
/// (`hex_label_layer`): the swatch's colour as the fill, black or white ink by the one
/// contrast rule (`Srgb::wants_dark_text`), the ink as a hairline border, the app's
/// bold mono for the value. One builder because the owner asked for one look, and two
/// card constructions in one window is how a look drifts; the two ANCHORS below
/// (`hover_tip`, `pinned_tip`) stay separate only because the toolkit forces it (a
/// hover tooltip cannot be shown by the keyboard, a popover cannot be summoned by
/// hover), and both consume exactly this.
///
/// The fill is the colour OPAQUE regardless of the swatch's alpha: a card's job is to
/// be read, and a translucent card over whatever happens to be behind it would gamble
/// the ink's contrast on the background. The TEXT carries the alpha (`#RRGGBBAA`), so
/// nothing is hidden by the choice. The "Copied!" flash rides the same card, which is
/// what makes it one card and not two.
fn swatch_card<'a>(text: String, c: Srgb) -> Element<'a, Msg> {
    let fill = cosmic::iced::Color::from_rgb8(c.r, c.g, c.b);
    let ink = if c.wants_dark_text() {
        cosmic::iced::Color::BLACK
    } else {
        cosmic::iced::Color::WHITE
    };
    widget::container(
        widget::text(text)
            .size(12)
            .font(crate::app::theme::mono_font(true))
            // At the LEAF, `hex_label_layer`'s own lesson (DRAGON-601): an inherited
            // text colour loses to anything in between that resolves one.
            .class(cosmic::theme::Text::Color(ink)),
    )
    .padding([4, 8])
    .class(cosmic::theme::Container::custom(move |t| {
        cosmic::iced::widget::container::Style {
            background: Some(Background::Color(fill)),
            text_color: Some(ink),
            border: Border {
                radius: theme::rounding(t).s.into(),
                width: 1.0,
                color: ink,
            },
            ..Default::default()
        }
    }))
    .into()
}

/// The HOVER anchor for [`swatch_card`]: the toolkit's tooltip machinery carrying our
/// card. The helper's default `Container::Tooltip` dress is overridden to transparent
/// with no padding, because the card IS the box; leaving both would draw the toolkit's
/// grey plate behind the coloured card.
fn hover_tip<'a>(anchor: Element<'a, Msg>, text: String, c: Srgb) -> Element<'a, Msg> {
    widget::tooltip(anchor, swatch_card(text, c), widget::tooltip::Position::Top)
        .class(cosmic::theme::Container::Transparent)
        .padding(0)
        .into()
}

/// A tooltip card the APP shows, rather than one a pointer summons (DRAGON-682 items 9
/// and 30): the keyboard cursor's hex, and a swatch's transient "Copied!". Since item
/// seven it is [`swatch_card`] in a popover, so the keyboard's card and the pointer's
/// are the same card.
///
/// **iced's tooltip is hover-driven and has no "show it anyway".** So a keyboard cursor
/// cannot use one, and this pins the card as a POPOVER instead. That is not a new idea
/// here, it is the mechanism `widgets::copy_button`'s tombstone describes (a "Copied!"
/// card the app pinned open), and its warning applies unchanged: a pinned card is a
/// second answer beside any hover tooltip, so a swatch that has this must not also
/// offer that one.
///
/// **Two traps, both already paid for in this file.** The card is drawn in the popover's
/// own overlay layer, so it lands above the swatch rather than under it (the quads-before-
/// images lesson `swatch_disc` documents); and inside a SCROLLABLE, an overlay is clipped
/// by the scroll viewport, which is why the panel scrolls its cursor into view
/// rather than letting a card hang past the edge.
fn pinned_tip<'a>(anchor: Element<'a, Msg>, text: String, c: Srgb) -> Element<'a, Msg> {
    // `Position::Point` measured UP from the anchor's top-left, which is the same
    // placement `chrome::flyout`'s upward direction uses: the popover vocabulary has
    // Center, Bottom and Point, and only the last can put a card above its anchor.
    widget::popover(anchor)
        .popup(swatch_card(text, c))
        .position(widget::popover::Position::Point(cosmic::iced::Point::new(
            0.0,
            -PINNED_TIP_H,
        )))
        .into()
}

/// The height a pinned hex card is offset by: its own, near enough, since the card is one
/// 12pt line inside 4pt of padding. It only has to clear the swatch, and being a point or
/// two out puts the card a point or two further from it.
const PINNED_TIP_H: f32 = 24.0;

/// ONE swatch BAR: a full-width run of equal segments, no gaps, rounded at the bar's two
/// ends only (DRAGON-682 item 17), drawn at the window's current ALPHA over one continuous
/// checkerboard (item 19).
///
/// **The shape is the owner's**: "a full row sequence where the first color gets the
/// rounding on the top left and bottom left, and the last color gets top right and bottom
/// right. no spacing between them, and it should be full width no matter how many segments
/// subdivide it". `geom::segment_widths` is the arithmetic that makes the last part true at
/// any count, handing the floor's remainder out a point at a time so the bar lands flush on
/// the card's right edge; the rounding is per position, square in the middle.
///
/// **ONE checkerboard behind the whole bar, not one per segment** (item 19, my call): the
/// board is a texture that says "this is transparent", and restarting it at every segment
/// boundary would draw a grid of little boards and make the seams the loudest thing in the
/// card. A single board also lets the colours' own edges be the only edges. The segments
/// are translucent QUADS over it, so their fills are analytic at any scale and only the
/// board is a raster.
///
/// The stack order is load-bearing for the same reason `swatch_disc`'s is: within one iced
/// layer every quad draws before every image, so the segments would land UNDER the board.
/// `widget::stack` wraps each child after the first in `renderer.with_layer`, which is what
/// puts them in front.
///
/// Built to be reused: the Palettes tab gets the same bar when it has colours to show, which
/// is why this takes a colour slice and a group index rather than reading the harmony list.
///
/// The four TRANSIENT facts travel together in [`BarState`]: they are all "where in the panel
/// is something happening right now", they are all read once per frame at the caller, and
/// passing them as four parameters made a seven-argument function that grew an eighth.
/// The transient panel state ONE bar needs (DRAGON-682): which segment has its menu open,
/// which the keyboard cursor is on, which just copied, and whether a drag is in flight.
///
/// Positions rather than booleans, because a bar is asked about all of its own segments and
/// comparing `Some((group, i))` once per segment is what keeps the caller from carrying four
/// parallel lists.
#[derive(Clone, Copy, Default)]
struct BarState {
    menu: Option<(usize, usize)>,
    cursor: Option<(usize, usize)>,
    copied: Option<(usize, usize)>,
    dragging: bool,
    /// The panel's live scroll offset and the window's size (DRAGON-687 follow-up): what
    /// a segment's menu needs to FIT itself inside the window from wherever the scroll
    /// has put its anchor (`geom::menu_fit`).
    scroll: f32,
    window: (f32, f32),
}

fn swatch_bar<'a>(
    colors: &[Srgb],
    alpha: u8,
    group: usize,
    board: Option<&widget::image::Handle>,
    state: BarState,
    palettes: &'a [geom::Palette],
    page: geom::MenuPage,
) -> Element<'a, Msg> {
    let BarState { menu, cursor, copied, dragging, scroll, window } = state;
    let n = colors.len().max(1);
    let widths = geom::segment_widths(n);
    let segments: Vec<Element<'a, Msg>> = colors
        .iter()
        .enumerate()
        .zip(widths.iter().copied())
        .map(|((i, c), w)| {
            segment(
                *c,
                alpha,
                (group, i),
                w,
                geom::segment_corners(i, n),
                menu == Some((group, i)),
                // WHICH tip this segment shows is decided here, once, so the segment takes
                // one answer rather than two competing flags (item 30's precedence).
                geom::swatch_tip(
                    copied == Some((group, i)),
                    cursor == Some((group, i)),
                    dragging,
                ),
                palettes,
                page,
                geom::harmony_swatch_anchor(group, i, n, scroll),
                window,
            )
        })
        .collect();
    let row = widget::row(segments).width(Length::Fixed(geom::bar_w()));
    let mut layers: Vec<Element<'a, Msg>> = vec![
        // The board is the bar's own size and carries the bar's own outer rounding, so a
        // translucent END segment shows a rounded corner of board rather than a square one
        // poking out from under it.
        raster_image(board, geom::bar_w(), geom::PANEL_SWATCH),
        row.into(),
        // ONE rim around the whole run (DRAGON-682 item 27, the owner: "treat all colors in
        // a series as a single bordered item, not individually bordered"). The segments
        // themselves carry NO border at all, so the only lines in a bar are its own outline.
        //
        // It is a stack LAYER rather than a border on the container around them, because a
        // container's border is a quad and within one iced layer every quad draws before
        // every image: it would land under the checkerboard. `widget::stack` puts each child
        // after the first in its own layer, so this one draws over the fills.
        bar_outline(None, geom::bar_w(), [true, true]),
    ];
    // The keyboard CURSOR, laid over the finished bar rather than drawn into a segment, so
    // it reads as a highlight ON the bar and never as one segment's own border. Placed from
    // the segment's own x, which is not a constant stride: the bar hands its rounding
    // remainder out a point at a time (`geom::segment_x`).
    if let Some((g, i)) = cursor
        && g == group
        && i < n
    {
        layers.push(absolute(
            bar_outline(Some(()), widths[i], geom::segment_corners(i, n)),
            (geom::segment_x(i, n), 0.0),
        ));
    }
    cosmic::iced::widget::stack(layers)
        .width(Length::Fixed(geom::bar_w()))
        .height(Length::Fixed(geom::PANEL_SWATCH))
        .into()
}

/// An OUTLINE the width of a bar or of one of its segments: the bar's own rim when
/// `focus` is `None`, and the window's focus frame when it is `Some` (DRAGON-682 item 27).
///
/// One function for both because they are the same shape in the same place and must round
/// the same way; what differs is the ink and the weight, and reading the rim from
/// [`swatch_rim`] is what keeps a bar's outline identical to a filled history swatch's.
fn bar_outline<'a>(focus: Option<()>, w: f32, corners: [bool; 2]) -> Element<'a, Msg> {
    let (first, last) = (corners[0], corners[1]);
    widget::container(widget::space::Space::new())
        .width(Length::Fixed(w))
        .height(Length::Fixed(geom::PANEL_SWATCH))
        .class(cosmic::theme::Container::custom(move |t| {
            let r = swatch_radius(t)[0];
            let (width, color) = match focus {
                Some(()) => (geom::FOCUS_RING_W, theme::accent(t)),
                None => swatch_rim(t),
            };
            cosmic::iced::widget::container::Style {
                border: Border {
                    radius: cosmic::iced::border::Radius {
                        top_left: if first { r } else { 0.0 },
                        bottom_left: if first { r } else { 0.0 },
                        top_right: if last { r } else { 0.0 },
                        bottom_right: if last { r } else { 0.0 },
                    },
                    width,
                    color,
                },
                ..Default::default()
            }
        }))
        .into()
}

/// ONE segment of a bar: a colour at `alpha`, its own corner rounding, a hex tooltip and a
/// right-click menu.
///
/// **A plain left click does NOTHING** (the owner: "clicking them shouldnt automatically
/// change the ui"), so this is not a button at all: a cosmic button with no `on_press`
/// renders DISABLED, and one with a no-op press would answer a click by pretending to do
/// something. It is a container in the swatch dress, inside a `mouse_area` that catches the
/// right press, which is the whole of its interaction.
#[allow(clippy::too_many_arguments)]
fn segment<'a>(
    c: Srgb,
    alpha: u8,
    at: (usize, usize),
    w: f32,
    corners: [bool; 2],
    menu_open: bool,
    tip: geom::SwatchTip,
    palettes: &'a [geom::Palette],
    page: geom::MenuPage,
    menu_anchor: (f32, f32),
    window: (f32, f32),
) -> Element<'a, Msg> {
    let fill = cosmic::iced::Color::from_rgba8(c.r, c.g, c.b, f32::from(alpha) / 255.0);
    let (first, last) = (corners[0], corners[1]);
    let face = widget::container(widget::space::Space::new())
        .width(Length::Fixed(w))
        .height(Length::Fixed(geom::PANEL_SWATCH))
        .class(cosmic::theme::Container::custom(move |t| {
            let r = swatch_radius(t)[0];
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(fill)),
                border: Border {
                    // Only the BAR's own ends round; a middle segment is square on both
                    // sides, which is what makes the run read as one bar.
                    radius: cosmic::iced::border::Radius {
                        top_left: if first { r } else { 0.0 },
                        bottom_left: if first { r } else { 0.0 },
                        top_right: if last { r } else { 0.0 },
                        bottom_right: if last { r } else { 0.0 },
                    },
                    // NO border of its own, ever (DRAGON-682 item 27): the bar wears one
                    // rim around the whole run, and the keyboard cursor is an overlay above
                    // it. A border here would draw a line down every seam.
                    width: 0.0,
                    color: cosmic::iced::Color::TRANSPARENT,
                },
                ..Default::default()
            }
        }));
    // The pointer's comings and goings, so a press can know what it picked up
    // (DRAGON-682 item 35: a press event carries no target, and `geom::drag_source` reads
    // the hover). The GRAB hand says the swatch can be picked up, and it has to be forced:
    // the face is a container, so `mouse_area`'s own `.interaction()` would work here, but
    // the recents swatch next door wraps a BUTTON where it would not, and one mechanism for
    // all three sources is worth more than the smaller wrapper here.
    let seg = crate::widgets::force_cursor::force_cursor(
        widget::mouse_area(face)
            .on_right_press(Msg::ColorPicker(ColorPickerMsg::PanelMenu(Some(at))))
            // The press NAMES this swatch (DRAGON-682 item 41). Nothing is looked up
            // afterwards, which is what makes a drag carry the colour that was pressed and
            // stops a press anywhere else in the window arming anything. The RELEASE
            // completes a click (DRAGON-687 item five reversed the original
            // click-does-nothing rule): a sub-threshold press-release applies this
            // swatch, bumping the previous colour into the recents.
            .on_press(Msg::ColorPicker(ColorPickerMsg::DragPressed(
                geom::DragSource::Harmony(at.0, at.1),
            )))
            .on_release(Msg::ColorPicker(ColorPickerMsg::PanelSwatchReleased(at.0, at.1))),
        cosmic::iced::mouse::Interaction::Grab,
    );
    // Its own menu first, and then one of three tips, never two: two cards over one swatch
    // is two answers to one question. `geom::swatch_tip` is the precedence (the caller asks
    // it), and the reason it is a decision rather than an `if` chain here is that "Copied!"
    // and the cursor's hex card genuinely compete for the same segment (item 30).
    if menu_open {
        return harmony_menu(seg, c, alpha, at.1, palettes, page, menu_anchor, window);
    }
    match tip {
        // A drag is in flight: no card at all, so the ghost is the only thing following the
        // pointer (item 35).
        geom::SwatchTip::Silent => seg,
        geom::SwatchTip::Copied => {
            pinned_tip(seg, crate::widgets::copy_button::COPIED_LABEL.to_string(), c)
        }
        geom::SwatchTip::PinnedHex => pinned_tip(seg, swatch_hex_tip(c, alpha), c),
        geom::SwatchTip::Hover => hover_tip(seg, swatch_hex_tip(c, alpha), c),
    }
}

/// A harmony swatch's CONTEXT MENU: take this colour, or copy it.
///
/// The owner's three entries (set active, add to recents, copy), and the same machinery the
/// history's own menu uses so the two read as one vocabulary. The history's menu deliberately
/// does NOT gain the middle one: a recents swatch is already in the recents, and an entry
/// that files it again would be a no-op dressed as an action.
///
/// The shared machinery: the same flyout, the same row height and dress, the same
/// click-away dismissal, and Escape closes it in `keyboard.rs` beside the others. The
/// alignment rule differs because the anchor does: a harmony swatch sits in a card whose
/// left edge is the panel's, so the menu hangs left-aligned off it and only shifts when
/// that would take it past the panel's right edge.
#[allow(clippy::too_many_arguments)]
fn harmony_menu<'a>(
    anchor: Element<'a, Msg>,
    c: Srgb,
    alpha: u8,
    column: usize,
    palettes: &'a [geom::Palette],
    page: geom::MenuPage,
    menu_anchor: (f32, f32),
    window: (f32, f32),
) -> Element<'a, Msg> {
    let offers = geom::offers_add_to_palette(palettes.len());
    let (rows, width): (Vec<Element<'a, Msg>>, f32) = match page {
        // The "Add to palette ›" target list (DRAGON-687): every palette, since a
        // harmony swatch belongs to none of them.
        geom::MenuPage::AddTo if offers => (
            palette_target_rows(palettes, None, |to| {
                Msg::ColorPicker(ColorPickerMsg::AddColorToPalette(to, c, alpha))
            }),
            geom::menu_width_for_labels(palettes.iter().map(|p| p.name.as_str())),
        ),
        _ => {
            let mut rows = vec![
                menu_row(
                    geom::SET_ACTIVE_LABEL.to_string(),
                    // ONE path with the plain click (DRAGON-687 item five): the bump
                    // files the PREVIOUS colour; item 22's file-the-clicked source now
                    // belongs to the drop alone.
                    Msg::ColorPicker(ColorPickerMsg::ApplySwatch(c, alpha)),
                ),
                // BETWEEN the two (DRAGON-682 item 28, the owner's own placement): taking
                // a colour and keeping a colour are the two things you do with one you
                // like, and copying is the one you do with one you are taking elsewhere.
                menu_row(
                    geom::ADD_TO_RECENTS_LABEL.to_string(),
                    Msg::ColorPicker(ColorPickerMsg::AddColorToRecents(c, alpha)),
                ),
            ];
            if offers {
                // Keeping-a-colour's second home (DRAGON-687), so it sits with "Add to
                // recents" and before Copy, the same taking/keeping/copying order.
                rows.push(submenu_row(geom::ADD_TO_PALETTE_LABEL, geom::MenuPage::AddTo));
            }
            rows.push(menu_row(
                geom::COPY_COLOR_LABEL.to_string(),
                Msg::ColorPicker(ColorPickerMsg::CopyColor(c, alpha)),
            ));
            let width = if offers {
                geom::menu_width_for_labels(
                    [
                        geom::SET_ACTIVE_LABEL,
                        geom::ADD_TO_RECENTS_LABEL,
                        geom::ADD_TO_PALETTE_LABEL,
                        geom::COPY_COLOR_LABEL,
                    ]
                    .into_iter(),
                )
            } else {
                // With no palettes the menu is byte-identical to DRAGON-682's.
                geom::harmony_menu_width()
            };
            (rows, width)
        }
    };
    // FITTED from THIS page's size (DRAGON-687 follow-up): the column rule chooses the
    // left edge, menu_fit re-clamps per page swap, and a first-card anchor near the
    // panel's top now flips DOWN instead of clipping above the frame.
    let desired_left = menu_anchor.0 - geom::harmony_menu_dx(column, geom::MAX_SEGMENTS, width);
    let (x, y) = geom::menu_fit(
        menu_anchor,
        geom::PANEL_SWATCH,
        desired_left,
        (width, geom::menu_panel_h_for(rows.len())),
        window,
    );
    crate::app::preview::chrome::flyout(
        anchor,
        menu_panel(rows, width),
        crate::app::preview::chrome::FlyoutDir::At { x, y },
        Msg::ColorPicker(ColorPickerMsg::PanelMenu(None)),
    )
}

/// The MAIN round swatch's CONTEXT MENU (DRAGON-687 item seven): keep the shown colour
/// (recents or a palette) or copy it, and deliberately NO "Set as active color": the
/// main swatch IS the active colour, and `geom::main_swatch_menu_labels` pins both the
/// rows and that absence. The palette row is the harmony menu's own "Add to palette ›"
/// with its exact any-palette gate and its full-list targets (a search filter narrows
/// the PANEL, not where a colour may be filed).
fn main_swatch_menu<'a>(
    anchor: Element<'a, Msg>,
    c: Srgb,
    alpha: u8,
    palettes: &'a [geom::Palette],
    page: geom::MenuPage,
    window: (f32, f32),
) -> Element<'a, Msg> {
    let offers = geom::offers_add_to_palette(palettes.len());
    let (rows, width): (Vec<Element<'a, Msg>>, f32) = match page {
        geom::MenuPage::AddTo if offers => (
            palette_target_rows(palettes, None, |to| {
                Msg::ColorPicker(ColorPickerMsg::AddColorToPalette(to, c, alpha))
            }),
            geom::menu_width_for_labels(palettes.iter().map(|p| p.name.as_str())),
        ),
        _ => {
            let labels = geom::main_swatch_menu_labels(palettes.len());
            let rows = labels
                .iter()
                .map(|label| match *label {
                    geom::ADD_TO_PALETTE_LABEL => {
                        submenu_row(geom::ADD_TO_PALETTE_LABEL, geom::MenuPage::AddTo)
                    }
                    geom::COPY_COLOR_LABEL => menu_row(
                        geom::COPY_COLOR_LABEL.to_string(),
                        Msg::ColorPicker(ColorPickerMsg::CopyColor(c, alpha)),
                    ),
                    _ => menu_row(
                        geom::ADD_TO_RECENTS_LABEL.to_string(),
                        Msg::ColorPicker(ColorPickerMsg::AddColorToRecents(c, alpha)),
                    ),
                })
                .collect();
            (rows, geom::menu_width_for_labels(labels.into_iter()))
        }
    };
    let menu_anchor = geom::main_swatch_anchor();
    let (x, y) = geom::menu_fit(
        menu_anchor,
        geom::SWATCH_CIRCLE,
        menu_anchor.0,
        (width, geom::menu_panel_h_for(rows.len())),
        window,
    );
    crate::app::preview::chrome::flyout(
        anchor,
        menu_panel(rows, width),
        crate::app::preview::chrome::FlyoutDir::At { x, y },
        Msg::ColorPicker(ColorPickerMsg::MainSwatchMenu(false)),
    )
}

/// The colour history's CONTEXT MENU (DRAGON-680 item 24): one entry, "Remove from
/// recents", floating above the swatch it was opened on.
///
/// It is the notation menu's machinery with a different list, deliberately: the same
/// `chrome::flyout`, the same fixed-height row, the same panel dress
/// ([`mode_menu_style`]), the same click-away dismissal, and Escape closes it in
/// `keyboard.rs` beside the other one. A second, differently-behaving menu in one window
/// would be two answers to "how do menus work here".
///
/// What differs is the ALIGNMENT. The notation menu hangs off a control at the content's
/// right edge, so it is right-aligned; a swatch can be anywhere across the row, so its menu
/// is left-aligned unless that would run off the right edge, and then only as far left as
/// it must be (`geom::recents_menu_dx`). Both open UPWARD for the same reason: the history
/// is the last block in the window and there is nothing below it.
fn recents_menu<'a>(
    anchor: Element<'a, Msg>,
    index: usize,
    entry: geom::Recent,
    palettes: &'a [geom::Palette],
    page: geom::MenuPage,
    window: (f32, f32),
) -> Element<'a, Msg> {
    let offers = geom::offers_add_to_palette(palettes.len());
    let (rows, width): (Vec<Element<'a, Msg>>, f32) = match page {
        geom::MenuPage::AddTo if offers => (
            palette_target_rows(palettes, None, |to| {
                Msg::ColorPicker(ColorPickerMsg::AddColorToPalette(to, entry.color, entry.alpha))
            }),
            geom::menu_width_for_labels(palettes.iter().map(|p| p.name.as_str())),
        ),
        _ => {
            // "Set as active color" FIRST, and the same wording the panel's menu uses
            // (DRAGON-682 item 7, the owner's ask that the two menus read as one
            // vocabulary). It is the same action a left click performs, offered by name
            // for the keyboard route where there is no click to make.
            let mut rows = vec![menu_row(
                geom::SET_ACTIVE_LABEL.to_string(),
                Msg::ColorPicker(ColorPickerMsg::SetActiveColor(
                    entry.color,
                    entry.alpha,
                    // A history apply is a LOAD, so it does not file anything: the entry
                    // is already in the list (item 22).
                    geom::ColorSource::RecentClick,
                )),
            )];
            if offers {
                // A recents swatch's own "Add to palette ›" (DRAGON-687), between the
                // load and the removal: it is the keep action, and it sits where the
                // harmony menu keeps its own.
                rows.push(submenu_row(geom::ADD_TO_PALETTE_LABEL, geom::MenuPage::AddTo));
            }
            rows.push(menu_row(
                geom::REMOVE_RECENT_LABEL.to_string(),
                Msg::ColorPicker(ColorPickerMsg::RemoveRecent(index)),
            ));
            let width = if offers {
                geom::menu_width_for_labels(
                    [
                        geom::SET_ACTIVE_LABEL,
                        geom::ADD_TO_PALETTE_LABEL,
                        geom::REMOVE_RECENT_LABEL,
                    ]
                    .into_iter(),
                )
            } else {
                // With no palettes the menu is byte-identical to DRAGON-680's.
                geom::recents_menu_width()
            };
            (rows, width)
        }
    };
    // FITTED to the window from THIS page's size (DRAGON-687 follow-up): the column
    // rule still chooses the left edge, and `menu_fit` re-clamps whenever a page swap
    // changes the panel's height or width.
    let at = geom::history_swatch_anchor(index);
    let desired_left = at.0 - geom::recents_menu_dx(index, width);
    let (x, y) = geom::menu_fit(
        at,
        geom::RECENT_SWATCH,
        desired_left,
        (width, geom::menu_panel_h_for(rows.len())),
        window,
    );
    crate::app::preview::chrome::flyout(
        anchor,
        menu_panel(rows, width),
        crate::app::preview::chrome::FlyoutDir::At { x, y },
        Msg::ColorPicker(ColorPickerMsg::RecentsMenu(None)),
    )
}

/// DRAGON-598 put the hex label in a monospace face; DRAGON-601 resized it and made it bold.
/// Both are reasons to re-measure the box against the text, which is what this module does.
///
/// The tests here assert RELATIONS between the two constants, never their values. A test that
/// pins `LABEL_SIZE == (192, 60)` fails loudly on a size change and then gets its literal
/// updated, which teaches nothing; a test that pins "the box fits the text and is not the box
/// a bigger text needed" fails for the actual defect and passes for any honest re-derivation.
///
/// Bold costs nothing in WIDTH here, and that is a property of the face rather than luck: in a
/// real monospace family every face shares one advance, so the bold hex occupies exactly the
/// columns the regular one did (measured on the shipped path: Noto Sans Mono Regular and Bold
/// both advance 0.600 em). `theme::mono_font` is what guarantees the face really is monospace.
#[cfg(test)]
mod label_metrics_tests {
    use super::*;

    /// The widest advance, as a fraction of the em, that any monospace face the toolkit is
    /// likely to resolve to actually uses. DejaVu Sans Mono is 0.602, Noto Sans Mono and
    /// Liberation Mono 0.600, JetBrains Mono 0.600, Menlo 0.602, Consolas 0.550. 0.65 is
    /// deliberately above all of them, so this test measures the WORST case rather than the
    /// owner's machine: the label must not depend on which mono font a distro ships.
    const WIDEST_MONO_ADVANCE_EM: f32 = 0.65;

    /// `#RRGGBB`, the only string this chip ever shows.
    const HEX_CHARS: f32 = 7.0;

    /// The widest and narrowest the chip's padding may be, as a fraction of the text size.
    ///
    /// The LOWER bound is the one that has always been here: too little and the hex clips, or
    /// sits on the rounded corners and the 1pt border. The UPPER bound is DRAGON-601's second
    /// pass, and it is the more interesting half, because nothing but a person's attention was
    /// stopping a text-size change from leaving the box behind. Both are in ems rather than
    /// points so they describe the LOOK of the chip and hold at any size.
    ///
    /// Every box this label has ever worn sits inside the band: `(96, 30)` at 14pt and
    /// `(192, 60)` at 28pt both spend 2.31 em on padding, and today's `(152, 48)` at 22pt
    /// spends 2.36. The box that would fail is `(192, 60)` kept at 22pt, which spends 4.18: the
    /// exact mistake of trimming the text and leaving the box.
    const PAD_EM: std::ops::RangeInclusive<f32> = 1.5..=3.0;
    /// The same band for the chip's HEIGHT, as a multiple of the text size, and for the same
    /// two reasons. 14pt and 28pt both sat at 2.14, 22pt sits at 2.18, and the abandoned
    /// 60pt-tall box at 22pt would read 2.73.
    const HEIGHT_EM: std::ops::RangeInclusive<f32> = 1.4..=2.5;

    /// The seven characters fit inside the box, with room to spare, at the worst mono advance,
    /// AND the box is not the one a larger text needed.
    ///
    /// At today's 22pt the hex asks for 100pt of the 152. At 28pt it asked for 127 of 192, and
    /// at 14pt for 64 of 96, which is the same chip at three sizes.
    #[test]
    fn the_hex_fits_its_box_and_the_box_fits_the_hex() {
        let em = LABEL_TEXT_SIZE as f32;
        let width = HEX_CHARS * em * WIDEST_MONO_ADVANCE_EM;
        assert!(width < LABEL_SIZE.0, "{width}pt of hex in a {}pt box", LABEL_SIZE.0);
        // Comfortably, not by a hair: the chip centres the string, so the margin is what keeps
        // it off the rounded corners and the 1pt border.
        assert!(
            LABEL_SIZE.0 - width >= 24.0,
            "only {}pt spare, which is too tight to centre in",
            LABEL_SIZE.0 - width
        );
        let pad_em = (LABEL_SIZE.0 - width) / em;
        assert!(
            PAD_EM.contains(&pad_em),
            "the chip spends {pad_em:.2} em on horizontal padding, outside {PAD_EM:?}. Under the \
             band the hex crowds the border; over it the box is the one a bigger text needed, \
             and the placement ladder measures the BOX, so it flips on a chip nobody can see."
        );
        // And the line still fits vertically, without the chip being a slab either.
        let height_em = LABEL_SIZE.1 / em;
        assert!(
            HEIGHT_EM.contains(&height_em),
            "the chip is {height_em:.2} em tall, outside {HEIGHT_EM:?}"
        );
    }

    /// The box the RENDERER paints and the box the PLACEMENT LADDER is told about are one
    /// const, so the two can never disagree about where the chip is. Pinned by running the
    /// real ladder with the view's real constants: mid-screen it still answers Below, and the
    /// chip it puts there still sits clear of the disc and fully on the surface.
    #[test]
    fn one_box_feeds_both_the_placement_and_the_paint() {
        let radius = geom::MAGNIFIER_DIAMETER as f32 / 2.0;
        let viewport = (1920.0, 1080.0);
        let centre = (960.0, 540.0);
        let placement = geom::label_placement(centre, LABEL_SIZE, radius, LABEL_GAP, viewport);
        assert_eq!(placement, geom::LabelPlacement::Below, "open screen still reads below");
        let (x, y) = geom::label_origin(placement, centre, LABEL_SIZE, radius, LABEL_GAP, viewport);
        assert!(
            x >= 0.0 && y >= 0.0 && x + LABEL_SIZE.0 <= viewport.0 && y + LABEL_SIZE.1 <= viewport.1,
            "the chip left the surface at ({x}, {y})"
        );
        assert!(y >= centre.1 + radius, "the chip rode up onto the lens at y={y}");
    }

    /// DRAGON-601: the ladder MEASURES the box, so every change to the box moves where it
    /// flips, in both directions. That is the ladder doing its job, and the property that has
    /// to survive a resize is not "the same placement as before" but "wherever it goes, the
    /// whole chip is on screen".
    ///
    /// Walked over the whole surface rather than at a few chosen points, because the failure
    /// this guards against is a BAND near one edge where the chip no longer fits on the side
    /// the ladder picked, which a handful of sample points walks straight past. The three
    /// viewports are an ordinary display, an ultrawide (whose short axis is what squeezes a
    /// Below or Above chip) and a small one (where the disc plus the chip is a real fraction
    /// of the screen, so corners have to fall through to the second pass).
    #[test]
    fn the_chip_lands_on_screen_everywhere() {
        let radius = geom::MAGNIFIER_DIAMETER as f32 / 2.0;
        for viewport in [(1920.0, 1080.0), (5120.0, 1440.0), (800.0, 480.0)] {
            let mut xs = 0;
            while xs <= 40 {
                let mut ys = 0;
                while ys <= 40 {
                    let centre =
                        (viewport.0 * xs as f32 / 40.0, viewport.1 * ys as f32 / 40.0);
                    let p = geom::label_placement(centre, LABEL_SIZE, radius, LABEL_GAP, viewport);
                    let (x, y) =
                        geom::label_origin(p, centre, LABEL_SIZE, radius, LABEL_GAP, viewport);
                    assert!(
                        x >= 0.0
                            && y >= 0.0
                            && x + LABEL_SIZE.0 <= viewport.0
                            && y + LABEL_SIZE.1 <= viewport.1,
                        "{viewport:?} at {centre:?}: {p:?} put the chip at ({x}, {y})"
                    );
                    ys += 1;
                }
                xs += 1;
            }
        }
    }

    /// The chip near the BOTTOM edge flips above the lens rather than hanging off it. Pinned as
    /// a behaviour rather than a number, because the DISTANCE at which it flips is a function
    /// of the box and is expected to move whenever the text size does; what must never change
    /// is that the ladder still has both sides available at the size the chip is now.
    #[test]
    fn the_ladder_still_flips_rather_than_running_off_the_bottom() {
        let radius = geom::MAGNIFIER_DIAMETER as f32 / 2.0;
        let viewport = (1920.0, 1080.0);
        let low = (960.0, viewport.1 - 4.0);
        let p = geom::label_placement(low, LABEL_SIZE, radius, LABEL_GAP, viewport);
        assert_ne!(p, geom::LabelPlacement::Below, "there is no room below at the bottom wall");
        let (_, y) = geom::label_origin(p, low, LABEL_SIZE, radius, LABEL_GAP, viewport);
        assert!(y + LABEL_SIZE.1 <= viewport.1, "the flipped chip still hangs off at y={y}");
    }
}

/// The drag-jump root cause's pin (DRAGON-687 follow-up): the root chain varies only in
/// VALUE, never in shape.
#[cfg(test)]
mod root_chain_tests {
    use super::*;

    /// The whole surface the drag state has on the root chain: `Some(Grabbing)` live,
    /// `None` otherwise. The wrapper and the outer stack are unconditional in
    /// `color_picker_window_view` (its layer comment carries the mechanism: a
    /// conditional wrapper re-shaped the tree, iced's positional diff mis-aligned the
    /// stateful descendants, and the panel scrollable's offset was rebuilt at zero,
    /// which is the owner's bottom-scrolled drag jumping to the top). If this function
    /// ever gains an input that changes the WRAPPERS rather than the value, that bug is
    /// back.
    #[test]
    fn the_drag_flag_changes_the_cursor_value_and_nothing_structural() {
        assert_eq!(root_cursor(true), Some(cosmic::iced::mouse::Interaction::Grabbing));
        assert_eq!(root_cursor(false), None, "idle is pass-through, not a removed wrapper");
    }
}

/// DRAGON-601: the hex label's INK. The owner got light grey text on a white pick, which is
/// unreadable, and the fix is that the leaf sets the colour rather than inheriting it. What is
/// testable here is the DECISION the leaf is handed, at both ends and in between.
#[cfg(test)]
mod label_ink_tests {
    use crate::color::Srgb;

    /// The colour the chip paints its text in, which is exactly the expression
    /// `hex_label_layer` hands to the text widget. Black on light picks, white on dark ones.
    fn ink(c: Srgb) -> cosmic::iced::Color {
        if c.wants_dark_text() {
            cosmic::iced::Color::BLACK
        } else {
            cosmic::iced::Color::WHITE
        }
    }

    /// WCAG contrast of the ink against the chip's own fill, which is the picked colour. The
    /// label is legible or it is not, and this is the number that says which.
    fn contrast(c: Srgb) -> f64 {
        let bg = c.relative_luminance();
        let fg = if ink(c) == cosmic::iced::Color::BLACK { 0.0 } else { 1.0 };
        let (hi, lo) = if fg > bg { (fg, bg) } else { (bg, fg) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// The two ends the owner named, plus the middle. White is the one that was failing, and
    /// it must come out BLACK: light grey on white is the reported bug, and the flip has no
    /// opinion about grey at all, which is how we know the ink was not reaching the text.
    #[test]
    fn both_ends_and_the_middle_are_readable() {
        let white = Srgb::new(255, 255, 255);
        let black = Srgb::new(0, 0, 0);
        let grey = Srgb::new(128, 128, 128);
        assert_eq!(ink(white), cosmic::iced::Color::BLACK, "white must take black text");
        assert_eq!(ink(black), cosmic::iced::Color::WHITE, "black must take white text");
        for c in [white, black, grey] {
            assert!(contrast(c) >= 4.5, "{c:?} reads at only {:.2}:1", contrast(c));
        }
    }

    /// Saturated colours, where the naive "is it bright" guess goes wrong most often: yellow
    /// and cyan are light despite being vivid, blue and red are dark. Every one of them has to
    /// clear ordinary body-text contrast.
    #[test]
    fn saturated_picks_are_readable_too() {
        let cases = [
            Srgb::new(255, 0, 0),
            Srgb::new(0, 255, 0),
            Srgb::new(0, 0, 255),
            Srgb::new(255, 255, 0),
            Srgb::new(0, 255, 255),
            Srgb::new(255, 0, 255),
            Srgb::new(255, 136, 0),
        ];
        for c in cases {
            assert!(contrast(c) >= 4.5, "{c:?} reads at only {:.2}:1", contrast(c));
        }
    }

    /// And the flip really is a flip: it must answer differently at the two ends, so a future
    /// threshold change that collapsed it to one answer (which is what "always light grey"
    /// looks like from the outside) fails here.
    #[test]
    fn the_flip_actually_flips() {
        assert_ne!(ink(Srgb::new(255, 255, 255)), ink(Srgb::new(0, 0, 0)));
    }
}

/// DRAGON-680: the two big controls-row buttons are ONE kind of control, and the copy
/// button's success flash is the only thing that distinguishes them. `mode_menu_tests`
/// stood here and went with the dropdown it measured.
#[cfg(test)]
mod controls_row_tests {
    use super::*;

    /// The glyph a state paints, which is the whole of the copy button's acknowledgement
    /// now that the "Copied!" word is gone.
    fn glyph(s: &cosmic::widget::button::Style) -> cosmic::iced::Color {
        s.icon_color.expect("a controls-row button always tints its glyph")
    }

    /// The flash is the app's ONE success green, and it reaches the glyph in every state
    /// a button can be in: a tick that turned green only at rest would go back to
    /// ordinary the moment the pointer that pressed it stayed put.
    #[test]
    fn the_copy_flash_is_the_apps_success_green_in_every_state() {
        for (name, t) in
            [("dark", cosmic::theme::Theme::dark()), ("light", cosmic::theme::Theme::light())]
        {
            for (hovered, pressed) in [(false, false), (true, false), (true, true)] {
                assert_eq!(
                    glyph(&controls_icon_style(&t, hovered, pressed, true)),
                    theme::success(&t),
                    "{name}: hovered={hovered} pressed={pressed}"
                );
                assert_ne!(
                    glyph(&controls_icon_style(&t, hovered, pressed, false)),
                    theme::success(&t),
                    "{name}: a button that is NOT flashing must not read as success"
                );
            }
        }
    }

    /// THE regression this file earned the hard way (DRAGON-680): the tick has to be green
    /// where it is actually DRAWN, which is the glyph's own svg class, not where it was
    /// merely passed (the button's `icon_color`, which the centring container shadows).
    ///
    /// Pinned by resolving the real class the view builds, through the real theme, rather
    /// than by reading the source again: the first verification of this feature was a
    /// source trace and it was wrong, because the trace stopped one widget short.
    #[test]
    fn the_tick_is_green_where_the_glyph_is_actually_drawn() {
        use cosmic::iced::widget::svg::{Catalog as _, Status};
        for (name, t) in
            [("dark", cosmic::theme::Theme::dark()), ("light", cosmic::theme::Theme::light())]
        {
            for success in [false, true] {
                let class = cosmic::theme::Svg::custom(move |t| {
                    cosmic::iced::widget::svg::Style {
                        color: Some(controls_icon_ink(t, success)),
                    }
                });
                let drawn = t.style(&class, Status::Idle).color;
                assert_eq!(
                    drawn,
                    Some(controls_icon_ink(&t, success)),
                    "{name}: success={success} did not reach the svg"
                );
                assert_eq!(
                    drawn == Some(theme::success(&t)),
                    success,
                    "{name}: the drawn ink and the flash disagree"
                );
            }
            // The two paths agree, which is what stops one of them drifting: the button's
            // inherited ink and the glyph's own class are the same decision.
            for success in [false, true] {
                assert_eq!(
                    controls_icon_style(&t, false, false, success).icon_color,
                    Some(controls_icon_ink(&t, success)),
                    "{name}: the button's ink is not the glyph's"
                );
            }
        }
    }

    /// The RESTING ink is not the success green on either theme, or "it is green while
    /// flashing" would be satisfied by a control that is always green.
    #[test]
    fn the_resting_ink_is_the_ordinary_foreground() {
        for t in [cosmic::theme::Theme::dark(), cosmic::theme::Theme::light()] {
            assert_ne!(controls_icon_ink(&t, false), theme::success(&t));
            assert_eq!(
                controls_icon_ink(&t, false),
                cosmic::iced::Color::from(t.cosmic().background(false).on)
            );
        }
    }

    /// The pipette is exactly the same control with the flash off, which is what makes
    /// the pair beside each other read as one family (the owner's "same size as the color
    /// picker button"). Pinned as an equivalence so a future tweak to one has to be a
    /// tweak to both.
    #[test]
    fn the_pipette_is_the_same_button_without_the_flash() {
        let t = cosmic::Theme::default();
        for (hovered, pressed) in [(false, false), (true, false), (true, true)] {
            let a = controls_icon_style(&t, hovered, pressed, false);
            let b = controls_icon_style(&t, hovered, pressed, false);
            assert_eq!(a.border_radius, b.border_radius);
            assert_eq!(a.border_width, b.border_width);
            assert_eq!(glyph(&a), glyph(&b));
        }
    }

    /// The glyph really does sit INSIDE its button with room to spare (the owner's
    /// "needs some padding inside of the hoverable circle"), and the button did not
    /// shrink with it. Stated as the inset rather than as two literals, because what the
    /// owner asked for is the padding.
    #[test]
    fn the_controls_glyph_has_padding_inside_its_button() {
        let inset = (geom::CONTROLS_BUTTON - f32::from(geom::CONTROLS_ICON)) / 2.0;
        assert!(inset >= 10.0, "only {inset}pt of padding around the glyph");
        assert_eq!(geom::CONTROLS_BUTTON, geom::SWATCH_CIRCLE, "the hover area is unchanged");
        assert!(
            f32::from(geom::CONTROLS_ICON) < 32.0,
            "the glyph is smaller than the 32 it was, which is where the padding came from"
        );
    }
}

/// DRAGON-594: the pipette is an ACTION, not a swatch, so it must never wear a swatch's
/// border, and it must still answer a hover. Since DRAGON-680 the same style dresses the
/// copy button beside it, so these promises now cover both.
#[cfg(test)]
mod pick_again_style_tests {
    use super::*;

    /// The flat fill a state paints, which is the whole of this control's feedback now that it
    /// has no border. A gradient would mean somebody replaced the icon-button component with
    /// something the rest of the app does not use.
    fn fill(s: &cosmic::widget::button::Style) -> cosmic::iced::Color {
        match s.background {
            Some(Background::Color(c)) => c,
            other => panic!("the pipette must paint a flat fill, got {other:?}"),
        }
    }

    /// The owner's actual request, in every state a button can be in. A border here is what
    /// made an action read as a colour sample.
    #[test]
    fn no_state_wears_a_border() {
        let t = cosmic::Theme::default();
        for (hovered, pressed) in [(false, false), (true, false), (true, true), (false, true)] {
            let s = controls_icon_style(&t, hovered, pressed, false);
            assert_eq!(s.border_width, 0.0, "hovered={hovered} pressed={pressed}: a border came back");
        }
    }

    /// Borderless must not mean feedback-less. Hover and press are the app's ordinary
    /// icon-button fills, so both have to differ from the resting one: a control that answers
    /// nothing at all is a worse defect than the one this replaced.
    #[test]
    fn hover_and_press_still_answer() {
        let t = cosmic::Theme::default();
        let rest = fill(&controls_icon_style(&t, false, false, false));
        assert_ne!(fill(&controls_icon_style(&t, true, false, false)), rest, "hover says nothing");
        assert_ne!(fill(&controls_icon_style(&t, true, true, false)), rest, "press says nothing");
    }

    /// It rounds like a BUTTON, not like a swatch. Both tokens follow the user's "Edge
    /// rounding" setting, so the control still tracks the theme; it just tracks it as the kind
    /// of thing it is.
    #[test]
    fn it_rounds_like_a_button_not_like_a_swatch() {
        let t = cosmic::Theme::default();
        let got = controls_icon_style(&t, false, false, false).border_radius;
        assert_eq!(got.top_left, theme::rounding(&t).xl[0], "not the button token");
        assert_ne!(got.top_left, swatch_radius(&t)[0], "still rounding like a swatch");
    }
}
