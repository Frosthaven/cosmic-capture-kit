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
        #[cfg(windows)]
        let header = if crate::platform::windows::caption::native_caption_buttons_supported() {
            header.end(
                widget::space::Space::new()
                    .width(Length::Fixed(crate::app::settings::WIN_CAPTION_INSET)),
            )
        } else {
            header
                .on_close(Msg::WindowChrome(WindowChromeMsg::Close))
                .on_minimize(Msg::WindowChrome(WindowChromeMsg::ColorPickerWindowMinimize))
        };
        #[cfg(all(not(target_os = "macos"), not(windows)))]
        let header = header
            .on_close(Msg::WindowChrome(WindowChromeMsg::Close))
            .on_minimize(Msg::WindowChrome(WindowChromeMsg::ColorPickerWindowMinimize));

        // DRAGON-630: the reference layout, top to bottom — the saturation/value
        // square, the controls row (pipette, round swatch, hue + alpha strips), the one
        // value row (component boxes + mode dropdown + copy), a hairline divider, and
        // the two-row colour history. The gaps are EXPLICIT spacers rather than one
        // column spacing, because the owner sized them individually on review (wider
        // air around the controls row, ordinary gaps around the divider).
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
            vspace(geom::SECTION_GAP),
            self.recent_colors_grid(),
        ];

        let content = widget::container(widget::column(items).width(Length::Fill))
            .padding(geom::WINDOW_PADDING)
            .width(Length::Fill)
            .height(Length::Fill);

        let stacked = widget::column(vec![header.into(), content.into()])
            .width(Length::Fill)
            .height(Length::Fill);

        // The frosted-window recipe, copied from `permissions::view` rather than invented:
        // the surface is enrolled in the platform's blur at creation (compositor blur on
        // Linux, the masked vibrancy view on macOS, DWM Mica on Windows) and this outer
        // container has to paint TRANSLUCENT for any of it to show. Painting an opaque
        // background here is exactly how a window ends up correctly enrolled and visibly
        // flat, which the owner warned is the usual first-try mac mistake.
        let glass = self.glass;
        widget::container(stacked)
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
            .into()
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

    /// The controls row (DRAGON-630): the pick-again pipette, the round current-colour
    /// swatch (checkerboard showing through a translucent alpha), and the stacked hue
    /// and alpha strips.
    fn controls_row(&self) -> Element<'_, Msg> {
        let cp = &self.color_picker;
        let swatch = raster_image(
            cp.swatch_raster.as_ref(),
            geom::SWATCH_CIRCLE,
            geom::SWATCH_CIRCLE,
        );
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
        // Explicit spacers: the swatch-to-tracks gap is wider than the pipette-to-
        // swatch one (the owner's review), so one row spacing cannot express the row.
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

    /// The value row (DRAGON-630), THREE BANDS stacked:
    ///
    /// 1. the MODE row: the mode dropdown (its name plus the up/down caret), sized to its
    ///    own longest option, then the split-inputs toggle and the copy button
    ///    IMMEDIATELY after it, then the word "Copied!" while a copy is fresh. Both icons'
    ///    tooltips drop UPWARD and are always available (DRAGON-676: the acknowledgement
    ///    used to be a tooltip card pinned open over the copy button, which had to silence
    ///    the neighbour's tooltip for as long as it was up);
    /// 2. the BOX row: the current mode's component boxes and nothing else, spanning the
    ///    whole content width, flush at both edges;
    /// 3. the CAPTION band: "R", "G", "B", "A", its cells the same widths and gaps as the
    ///    boxes above, so each caption lands under its own box.
    ///
    /// Each box shows the live DRAFT while it is the one being typed into, and the
    /// canonical spelling otherwise, so a half-typed value is never rewritten under the
    /// caret (see `ColorPickerState::draft`).
    ///
    /// **Why three bands and not one row.** It was one row of per-box COLUMNS, each
    /// `[control, caption]`, with the chip and both icon buttons in their own fixed
    /// [`geom::VALUE_BOX_H`] boxes beside them, and it cost two things the owner named:
    ///
    /// * The fixed boxes silently assumed a text input measures exactly `VALUE_BOX_H`,
    ///   and it does not: it measures its font's line height plus its own padding, a
    ///   shade more at the default scale and further out at a larger one. Everything that
    ///   was NOT a text input sat a point or two high. Now nothing is measured against a
    ///   constant: within a band, the boxes are the tallest thing and their neighbours
    ///   centre on THEM.
    /// * The chip and the two icon buttons were eating 156pt of the row that the boxes,
    ///   and through them the window's whole width, were sized around. Lifting all three
    ///   onto their own row is what let [`geom::CONTENT_W`] come down by 146pt, and the
    ///   room it left up there is where "Copied!" now goes.
    fn value_row(&self) -> Element<'_, Msg> {
        let cp = &self.color_picker;
        // Split channel boxes, or the ONE whole-value box, by the remembered layout
        // toggle (DRAGON-630 rev 3). Both spans are the same budget, so the copy button
        // never moves between them. The boxes and their captions are collected as two
        // PARALLEL lists, one cell each, so the two bands line up column by column
        // without either one being nested inside the other.
        let mut inputs: Vec<Element<'_, Msg>> = Vec::new();
        let mut captions: Vec<Element<'_, Msg>> = Vec::new();
        if cp.split_inputs {
            // Per-box widths, not one width times the count: the boxes span the whole
            // content width now, so the floor()'s remainder is handed out a point at a
            // time and the row lands flush on both edges (`geom::value_box_widths`).
            let widths = geom::value_box_widths(cp.box_count());
            // Single letters everywhere, hex included (the owner's call): the hex boxes
            // hold pairs like `FF`, and the content is what says which dialect this is.
            let mut labels = cp.mode.component_labels().to_vec();
            labels.push("A");
            // Zipped, so the widths and the labels cannot disagree about the count even
            // if `box_count` and `component_labels` ever drift apart.
            for (i, (label, bw)) in labels.iter().zip(widths).enumerate() {
                inputs.push(
                    widget::text_input("", cp.box_text(i))
                        .on_input(move |s| Msg::ColorPicker(ColorPickerMsg::BoxEdited(i, s)))
                        .on_submit(|_| Msg::ColorPicker(ColorPickerMsg::BoxCommitted))
                        .width(Length::Fixed(bw))
                        .style(value_box_style())
                        .into(),
                );
                captions.push(caption_cell(label, bw));
            }
        } else {
            let w = geom::value_whole_width();
            inputs.push(
                widget::text_input("", cp.box_text(crate::app::color_picker::WHOLE_VALUE_BOX))
                    .on_input(|s| {
                        Msg::ColorPicker(ColorPickerMsg::BoxEdited(
                            crate::app::color_picker::WHOLE_VALUE_BOX,
                            s,
                        ))
                    })
                    .on_submit(|_| Msg::ColorPicker(ColorPickerMsg::BoxCommitted))
                    .width(Length::Fixed(w))
                    .style(value_box_style())
                    .into(),
            );
            captions.push(caption_cell(cp.mode.label(), w));
        }
        // A FIXED-width frame around whichever layout is showing, so the two lay out
        // identically and the band is the same width either way. The caption band takes
        // the same frame, so the two stay aligned to the same left and right edges.
        let boxes = widget::container(widget::row(inputs).spacing(geom::BOX_GAP))
            .width(Length::Fixed(geom::value_whole_width()));
        let captions = widget::container(widget::row(captions).spacing(geom::BOX_GAP))
            .width(Length::Fixed(geom::value_whole_width()));
        let copied = cp
            .copied
            .is_some_and(|(_, at)| crate::widgets::copy_button::copied_recently(Some(at)));
        // The app's ordinary copy control: a green tick for the flash window, and a hover
        // tooltip. Both tooltips on this row drop UPWARD, over the slider tracks, because
        // below them are the value boxes the row is about.
        //
        // It was `pinned_copy_button` until DRAGON-676, which held a "Copied!" tooltip
        // card open over the button on the app's own initiative, because the window opens
        // with the pick already on the clipboard and a hover-only acknowledgement says
        // nothing at the one moment there is something to say. That reasoning still holds
        // and the WORD is still here; only its container changed. The card cost two things
        // a word on the row does not: it had to silence the neighbouring tooltip for as
        // long as it was up (two cards in one place is two answers to one question), and
        // it covered the alpha track above the row.
        let copy = crate::widgets::copy_button::subtle_copy_button(
            copied,
            geom::ROW_ICON_HALO,
            widget::tooltip::Position::Top,
            "Copy",
            Msg::ColorPicker(ColorPickerMsg::CopyValue),
        );
        // Band 1, the mode row: the mode dropdown at its own width, then the layout toggle
        // and the copy button IMMEDIATELY after it (DRAGON-676, the owner's ask), then the
        // "Copied!" word while a copy is fresh.
        //
        // ONE mode control (the owner's review; a chevron pair stood here first, then the
        // stock dropdown widget, which could not take the icon-button hover, a fixed text
        // span or our own caret). All three controls are a VALUE BOX tall
        // (`geom::MODE_ROW_H`, which is defined as that height, and
        // `geom::ROW_ICON_HALO`, which squares the two icon buttons up to it), so the row
        // reads as one band of equals over the boxes rather than as smaller furniture.
        //
        // The chip used to STRETCH across everything the two icon buttons did not need,
        // which pushed them to the content's right edge. The argument for that was that a
        // full-width chip reads as the row's INPUT, the way the value boxes below own
        // their whole row. What it also did was spend the row on nothing: the chip's own
        // word is five characters at most, so the stretch was empty chip, and the width it
        // held is exactly what the acknowledgement needed. The chip keeps the boxes'
        // border either way, which is what still marks it as the thing carrying a value.
        let mut controls: Vec<Element<'_, Msg>> = vec![
            mode_picker(cp.mode, cp.mode_menu_open),
            hspace(geom::ROW_SPACING),
            layout_toggle_button(cp.split_inputs),
            hspace(geom::ROW_SPACING),
            copy,
        ];
        // The acknowledgement, in the SAME success green as the tick it follows, read from
        // the same `theme::success` the copy button's own glyph is tinted with rather than
        // spelled out again here. It is absent, not blank, while nothing has been copied:
        // an empty text widget would still take a line box on a row measured in points.
        if copied {
            controls.push(hspace(geom::COPIED_TEXT_GAP));
            controls.push(copied_word());
        }
        let mode_row = widget::row(controls)
            .align_y(Alignment::Center)
            .height(Length::Fixed(geom::MODE_ROW_H))
            .width(Length::Fill);
        // Band 2, the box row: nothing but the boxes, spanning the whole content width.
        //
        // EXPLICIT gaps, not one column spacing, because the two are not the same size
        // (the owner's ask): the mode row stands off the boxes by the boxes' OWN gap, so
        // the air around a box reads the same across and down, while a caption stays
        // tucked under the box it names.
        widget::column(vec![
            mode_row.into(),
            vspace(geom::BOX_GAP),
            boxes.into(),
            vspace(geom::VALUE_LABEL_GAP),
            captions.into(),
        ])
        .width(Length::Fill)
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
        let block_h = 2.0 * geom::RECENT_SWATCH + geom::recents_gap();
        let recents = &self.color_picker.recents;
        if recents.is_empty() {
            // Nothing picked yet on this machine. A grid of empty boxes would read as
            // broken, so say what the space is for instead.
            return widget::container(widget::text::caption("Colors you pick appear here."))
                .width(Length::Fill)
                .height(Length::Fixed(block_h))
                .align_y(Alignment::Center)
                .into();
        }
        let current = self.color_picker.color;
        let gap = geom::recents_gap();
        let swatches: Vec<Element<'_, Msg>> = recents
            .iter()
            .take(geom::RECENTS_CAP)
            .enumerate()
            .map(|(i, c)| recent_swatch(*c, i, *c == current))
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
        widget::column(grid_rows)
            .spacing(gap)
            .width(Length::Fill)
            .into()
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
    .marker_diameter(geom::STRIP_H + 4.0)
    .marker_style(|t| crate::widgets::color_field::MarkerStyle {
        fill: Some(t.cosmic().background(false).base.into()),
        border: theme::subdued(t),
        border_width: 1.0,
        shadow: cosmic::iced::core::Shadow::default(),
    })
    .into()
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

/// The value boxes' style: cosmic's own text input with ONE change, the RESTING border
/// colour, which becomes `theme::subdued` so the boxes and the mode chip above them are
/// outlined in the same ink (the owner's ask).
///
/// Derived from the stock appearance rather than written out, so everything else about
/// the field (its fill, its rounding, its placeholder and selection colours, and the 2pt
/// width the chip matches) stays whatever libcosmic says it is. Hover and focus keep the
/// theme's ACCENT outline untouched: that is the app-wide "this is the field you are in"
/// signal and it is not ours to reinvent for one window.
fn value_box_style() -> cosmic::theme::TextInput {
    use cosmic::widget::text_input::StyleSheet as _;
    cosmic::theme::TextInput::Custom {
        active: Box::new(|t| {
            let mut a = t.active(&cosmic::theme::TextInput::Default);
            a.border_color = theme::subdued(t);
            a
        }),
        error: Box::new(|t| t.error(&cosmic::theme::TextInput::Default)),
        hovered: Box::new(|t| t.hovered(&cosmic::theme::TextInput::Default)),
        focused: Box::new(|t| t.focused(&cosmic::theme::TextInput::Default)),
        disabled: Box::new(|t| t.disabled(&cosmic::theme::TextInput::Default)),
    }
}

/// The corner radius a VALUE BOX actually draws, which the mode dropdown above them takes
/// too, closed face and popup menu alike (DRAGON-676, the owner's ask).
///
/// The mode row and the box row are one control group, so they have to round together. The
/// chip used to take `rounding().xl`, libcosmic's BUTTON token, and the menu `rounding().m`,
/// the panel token; both track the user's "Edge rounding" setting, but they track it to
/// different numbers than the boxes do, so the dropdown visibly drifted away from the row
/// beneath it whenever those tokens differed.
///
/// **Read, not restated.** This asks [`value_box_style`] itself, so whatever the boxes end up
/// drawing is by definition what the chip draws: today that resolves through libcosmic's
/// `TextInput::Default` to the theme's `radius_s`, and if a libcosmic bump moved it, or if
/// this file ever overrode the radius in `value_box_style` the way it already overrides the
/// border colour, the chip would follow with no second edit. A literal copied across, or even
/// a second `theme::rounding(t).s` lookup, is exactly the drift this replaces.
fn value_box_radius(theme: &cosmic::Theme) -> cosmic::iced::border::Radius {
    use cosmic::widget::text_input::StyleSheet as _;
    theme.active(&value_box_style()).border_radius
}

/// The mode chip and, while open, its menu (DRAGON-630 rev 4).
///
/// Hand-built on the zoom combo's chip + `chrome::flyout` recipe because the stock
/// dropdown widget could meet none of the owner's three asks: its closed face styles
/// through the theme-global pick_list catalog (no icon-button hover wash), its caret is
/// whatever the SYSTEM icon theme resolves for `pan-down-symbolic` (not the app's
/// vendored lucide glyph), and its text span breathes with the selection. The chip is an
/// icon button's fills under a subdued outline ([`mode_chip_style`]), the mode's name at
/// the left and the lucide up/down caret at the RIGHT EDGE; the menu opens UPWARD by a
/// known panel height, exactly like the zoom combo (the history block below leaves no
/// room down), and a click anywhere else closes it through the flyout's own dismissal.
///
/// The caret is pushed there by a flexible spacer rather than following the word. The
/// label used to carry a fixed span, wide enough for "OKLCH", so that the caret beside it
/// could not shuffle as the mode changed; the chip's own far edge does not move whatever
/// the word is, so the spacer alone holds the caret still. Since DRAGON-676 that spacer is
/// also where [`geom::mode_chip_width`]'s measurement headroom goes: the chip is sized for
/// the widest label as MEASURED through an embedded face, and whatever the drawing face
/// does not use of that allowance shows up here as air between the word and the caret.
fn mode_picker<'a>(mode: ColorFormat, open: bool) -> Element<'a, Msg> {
    let label = widget::container(widget::text(mode.label()).size(geom::MODE_LABEL_SIZE))
        .align_y(Alignment::Center);
    // The SELECTOR caret, up over down (the owner's ask). A lone chevron-down says "this
    // opens downward" and this menu opens UPWARD, since the history block below leaves no
    // room; the pair says "this cycles a list", which is what the control does.
    let caret = widget::container(
        widget::icon(crate::widgets::icons::handle("pan-up-down-symbolic"))
            .size(geom::MODE_CARET_ICON),
    )
    .align_y(Alignment::Center);
    // The row is wrapped in a CENTRING fill container because a cosmic button
    // positions its content at its padding rather than centring it (the same quirk
    // `chrome::dropdown_chip_tall` documents): the bare row sat top-aligned in the
    // chip.
    let chip = widget::button::custom(
        widget::container(
            widget::row(vec![
                label.into(),
                widget::space::Space::new().width(Length::Fill).into(),
                caret.into(),
            ])
            .align_y(Alignment::Center)
            // FILL on BOTH the row and the container around it, or the flexible spacer
            // above resolves to nothing: a shrink-width parent gives a shrink-width row,
            // and a shrink row has no spare width to give a `Fill` child. That is exactly
            // how the caret ends up hugging the word instead of sitting at the far edge.
            .width(Length::Fill),
        )
        .width(Length::Fill)
        .center_y(Length::Fill),
    )
    // The chip is exactly as wide as its LONGEST option needs (DRAGON-676, the owner's
    // ask; it spanned the row but for the two icon buttons until then). The mode's name
    // stays at the left edge and the caret at the right, so neither moves as the mode
    // changes. Still the EXPLICIT `geom::mode_chip_width()` and never `Shrink`, because
    // the MENU is a fixed-width panel that has to be the same number, and one function
    // answering both is the only way they cannot drift apart.
    .width(Length::Fixed(geom::mode_chip_width()))
    .height(Length::Fixed(geom::MODE_ROW_H))
    .padding([0, geom::MODE_CHIP_PAD])
    .class(cosmic::theme::Button::Custom {
        active: Box::new(|_f, t| mode_chip_style(t, false, false)),
        hovered: Box::new(|_f, t| mode_chip_style(t, true, false)),
        pressed: Box::new(|_f, t| mode_chip_style(t, true, true)),
        disabled: Box::new(|t| mode_chip_style(t, false, false)),
    })
    .on_press(Msg::ColorPicker(ColorPickerMsg::ModeMenuToggled));
    let chip: Element<'a, Msg> = crate::widgets::arrow_cursor::arrow_cursor(chip);
    if !open {
        return chip;
    }
    let items: Vec<Element<'a, Msg>> = crate::color::ColorFormat::ALL
        .into_iter()
        .enumerate()
        .map(|(i, f)| {
            // The current mode reads accent, like every other menu in the app.
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
                // FIXED height, and the centring container that goes with it (a cosmic
                // button lays its content out at its padding rather than centring it).
                // The height is not cosmetic: `mode_menu_panel_h` has to know the panel's
                // exact height to place it, and a row left to its own natural size is a
                // number this file can only guess at.
                widget::button::custom(
                    widget::container(text).center_y(Length::Fill),
                )
                .width(Length::Fill)
                .height(Length::Fixed(MODE_MENU_ITEM_H))
                .padding([0, geom::MODE_CHIP_PAD])
                .class(cosmic::theme::Button::Text)
                .on_press(Msg::ColorPicker(ColorPickerMsg::ModeSelected(i))),
            )
        })
        .collect();
    // The panel is exactly the CHIP's width, and its item column FILLS it, so the popup
    // lines up with the control on both edges and each row highlights across the whole
    // panel (the owner's ask). That is the same call it always was, and it is what carries
    // DRAGON-676's shrink through to the menu: the chip is measured from the longest
    // option, so the panel is too, and the two cannot part company. `mode_menu_tests`
    // pins that a row still fits inside it. The `Fill` on the column is load-bearing, not
    // decoration: a shrink-width column would hand its `Fill` buttons nothing to fill, and
    // every item would collapse to the width of its own word, which is what made the menu
    // look like it hung off the lettering rather than off the chip.
    let menu = widget::container(
        widget::column(items).spacing(MODE_MENU_GAP).width(Length::Fill),
    )
    .width(Length::Fixed(geom::mode_chip_width()))
    .padding(MODE_MENU_PAD)
    .class(cosmic::theme::Container::custom(mode_menu_style));
    crate::app::preview::chrome::flyout(
        chip,
        menu.into(),
        crate::app::preview::chrome::FlyoutDir::Up(mode_menu_panel_h()),
        Msg::ColorPicker(ColorPickerMsg::ModeMenuToggled),
    )
}

/// The mode chip's look: an icon button's own fills (transparent at rest, a wash on
/// hover, a firmer one on press) under a border in the app's SUBDUED text colour, at the
/// VALUE BOXES' own border width (the owner's ask, both parts).
///
/// The border is what separates this control from the two bare icon buttons beside it.
/// They are one glyph each and read as actions; this one carries a VALUE you can change,
/// so it wants the outline the boxes below it have. `theme::subdued` is the app's one
/// subdued-text token, the same one the slider thumbs are ringed with, so the outline
/// tracks the theme rather than being a colour invented here.
///
/// The ROUNDING is [`value_box_radius`], the boxes' own (DRAGON-676). It was
/// `rounding().xl`, libcosmic's button token, on the argument that a chip is a button and
/// the user's "Edge rounding" setting should reach it. It does reach it either way; the
/// mistake was WHICH corner it reached, because the chip sits directly on top of the value
/// boxes and the two rows are one control group. On a theme whose button and input tokens
/// differ, the chip visibly rounded away from the row under it. Same reasoning as the two
/// swatch sizes sharing [`swatch_radius`], one row down.
fn mode_chip_style(
    theme: &cosmic::Theme,
    hovered: bool,
    pressed: bool,
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
    s.border_radius = value_box_radius(theme);
    s.border_width = INPUT_BORDER_W;
    s.border_color = theme::subdued(theme);
    s.icon_color = Some(theme.cosmic().background(false).on.into());
    s
}

/// The mode MENU panel's look: the opaque menu surface every editor dropdown wears (see
/// `chrome::menu_container`), component base at full alpha under a divider outline, with the
/// CHIP's corner radius rather than the panel token's.
///
/// A named function and not the inline closure it used to be, so `mode_menu_tests` can hold
/// it against [`mode_chip_style`]: the closed face and the popup are one control, they are
/// already one WIDTH ([`geom::mode_chip_width`]), and DRAGON-676 makes them one rounding too.
/// A popup that rounded differently from the chip it hangs off reads as a second, unrelated
/// surface, which is the same complaint from the other direction.
fn mode_menu_style(theme: &cosmic::Theme) -> cosmic::iced::widget::container::Style {
    let c = theme.cosmic();
    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(c.background(false).component.base.into())),
        border: Border {
            radius: value_box_radius(theme),
            width: 1.0,
            color: c.background(false).component.divider.into(),
        },
        ..Default::default()
    }
}

/// The border width a value box draws, which the mode chip above them now matches.
///
/// Restated rather than read, because libcosmic hard-codes it in its own theme
/// (`theme::style::iced`'s `TextInput::Default`) and exposes no token for it. If a
/// libcosmic bump ever changes it, this is the one line that has to follow, and the chip
/// looking a hair thinner or fatter than the boxes under it is the symptom.
///
/// Its sibling [`value_box_radius`] does not have to be restated, because the RADIUS is on
/// the same `Appearance` and can simply be asked for. This width could be read the same way
/// (`theme.active(&value_box_style()).border_width`) and probably should be, next time
/// something here changes; DRAGON-676 left it alone rather than move a second property in a
/// ticket about rounding.
const INPUT_BORDER_W: f32 = 2.0;

/// One mode-menu row, the gap between rows, and the panel's own inset. All three are
/// FIXED so [`mode_menu_panel_h`] can be exact instead of an estimate.
const MODE_MENU_ITEM_H: f32 = 28.0;
const MODE_MENU_GAP: f32 = 2.0;
const MODE_MENU_PAD: u16 = 4;

/// The mode menu's on-screen height, for the upward flyout's fixed offset.
///
/// The flyout places the panel's TOP exactly this far above the chip's top
/// (`chrome::FlyoutDir::Up`), so this number decides where the panel's BOTTOM lands: right,
/// and the menu sits flush on the chip's top edge; under-counted, and the menu slides down
/// INTO the chip. It used to assume a "~27pt natural height" per row and the rows measured
/// more than that, which is what the owner saw as the menu bottoming out at the top of the
/// chip's lettering instead of at the chip. Every part of the panel now has a fixed height
/// and this sum is the panel exactly.
fn mode_menu_panel_h() -> f32 {
    let n = crate::color::ColorFormat::ALL.len() as f32;
    n * MODE_MENU_ITEM_H + (n - 1.0) * MODE_MENU_GAP + 2.0 * f32::from(MODE_MENU_PAD)
}

/// The divider BAND: the hairline between the value block and the colour history, with
/// the "Add color" button centred on it (the owner's ask).
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

/// The "Add color" button on the divider: file the colour the window is showing into the
/// history (`ColorPickerMsg::AddToHistory`).
///
/// It is dressed as a SLIDER THUMB (the owner's ask): the window's own background under a
/// subdued-text hairline, which is `gradient_strip`'s marker exactly. That is the right
/// borrow rather than a coincidence, because it is the same idea in a different place, a
/// small control sitting ON a track. The plus is the app's lucide glyph at
/// [`geom::ADD_COLOR_ICON`], deliberately smaller than the mode row's icons since it
/// stands beside 12pt text rather than alone.
fn add_color_button<'a>() -> Element<'a, Msg> {
    let content = widget::row(vec![
        widget::icon(crate::widgets::icons::handle("list-add-symbolic"))
            .size(geom::ADD_COLOR_ICON)
            .into(),
        widget::text("Add color").size(12).into(),
    ])
    .spacing(4.0)
    .align_y(Alignment::Center);
    // The centring container is the cosmic-button quirk the mode chip documents: a button
    // lays its content out at its padding rather than centring it, so a bare row sits
    // top-aligned in a fixed-height button. No tooltip: the button says what it does.
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
    crate::widgets::arrow_cursor::arrow_cursor(button)
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
fn vspace<'a>(h: f32) -> Element<'a, Msg> {
    widget::space::Space::new().height(Length::Fixed(h)).into()
}

/// A fixed horizontal gap, same reason, for the rows whose gaps differ pair by pair.
fn hspace<'a>(w: f32) -> Element<'a, Msg> {
    widget::space::Space::new().width(Length::Fixed(w)).into()
}

/// The value row's layout toggle (DRAGON-630 rev 3), left of the copy button: the
/// lucide list-chevrons pair, SHOWING the remembered state, chevrons pointing outward
/// while the channels are split apart, inward while they are collapsed into the one
/// whole-value box. Dressed as a bare icon button like its copy neighbour.
///
/// It took a `tips` flag until DRAGON-676, and lost it with the thing that needed it: the
/// copy button beside it used to pin a "Copied!" tooltip card open, so this button had to
/// go SILENT for the flash window, because a hover card popping up next to a card the app
/// pinned reads as two answers to one question. The acknowledgement is a word on the row
/// now and covers nothing, so this button keeps its tooltip at all times and the caller
/// has one fewer state to get right.
fn layout_toggle_button<'a>(split: bool) -> Element<'a, Msg> {
    // The copy button's own construction, one glyph (`copy_button::subtle_icon_button`,
    // same halo, same tint, same tooltip mechanics), so the pair beside each other read
    // as one family: the first version hand-rolled a bigger button and the owner
    // called the padding mismatch out. One tooltip either way (the owner's wording):
    // the icon carries the state, the tooltip names the control. The halo is the row's
    // own (`geom::ROW_ICON_HALO`), uniform on four sides, which is what squares both
    // buttons up to the dropdown beside them.
    let icon = if split { "list-expand-symbolic" } else { "list-collapse-symbolic" };
    crate::widgets::copy_button::subtle_icon_button(
        icon,
        geom::ROW_ICON_HALO,
        // Upward, like its copy-button neighbour: below this row are the value boxes the
        // row is about, and a card dropped over them hides the answer.
        widget::tooltip::Position::Top,
        "Toggle split inputs",
        Msg::ColorPicker(ColorPickerMsg::InputLayoutToggled),
    )
}

/// The "Copied!" acknowledgement beside the copy button (DRAGON-676), in the flash's own
/// success green.
///
/// The colour is `theme::success`, which is where `copy_button::icon_button_style` reads
/// the tick's tint from too, so the word and the glyph beside it cannot end up two
/// different greens: that helper's own test calls it "the app's one 'this worked' colour",
/// and a hex picked to match it here would be a second one the moment the theme moves.
fn copied_word<'a>() -> Element<'a, Msg> {
    widget::text("Copied!")
        .size(geom::COPIED_TEXT_SIZE)
        .class(cosmic::theme::Text::Custom(|t| cosmic::iced::widget::text::Style {
            color: Some(theme::success(t)),
            ..Default::default()
        }))
        .into()
}

/// The pick-again pipette: start a new pick, exactly as launching the tool does. It
/// leads the CONTROLS row since DRAGON-630 (the reference layout's eyedropper
/// position); it shared the recents row before that.
///
/// The same lucide `pipette` the tray entry and the editor's toolbar button wear
/// (`MenuIcon::ColorPicker` vendors it, `icons::handle` maps the name), so the tool has one
/// glyph everywhere. Dressed as a BARE ICON BUTTON rather than as a swatch: see
/// [`pick_again_style`].
fn pick_again_button<'a>() -> Element<'a, Msg> {
    // The glyph doubled (16 to 32) on the owner's review, and the button grew to the
    // round swatch's own square so the controls row leads with something the size of
    // what it sits beside; the strips gave up the width.
    let glyph = widget::icon(crate::widgets::icons::handle("color-select-symbolic"))
        .size(geom::PICK_AGAIN_ICON);
    let button = widget::button::custom(
        widget::container(glyph).center_x(Length::Fill).center_y(Length::Fill),
    )
    .width(Length::Fixed(geom::PICK_AGAIN_W))
    .height(Length::Fixed(geom::PICK_AGAIN_W))
    .padding(0)
    .class(cosmic::theme::Button::Custom {
        active: Box::new(|_f, t| pick_again_style(t, false, false)),
        hovered: Box::new(|_f, t| pick_again_style(t, true, false)),
        pressed: Box::new(|_f, t| pick_again_style(t, true, true)),
        disabled: Box::new(|t| pick_again_style(t, false, false)),
    })
    .on_press(Msg::ColorPicker(ColorPickerMsg::PickAgain));
    widget::tooltip(
        crate::widgets::arrow_cursor::arrow_cursor(button),
        widget::text("Pick another color").size(12),
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
fn pick_again_style(
    theme: &cosmic::Theme,
    hovered: bool,
    pressed: bool,
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
    s.icon_color = Some(theme.cosmic().background(false).on.into());
    s
}

/// THE corner radius every SWATCH in this window uses: the big one at the top and the recents
/// along the bottom. The pipette that shares the recents row is deliberately NOT one of them
/// (DRAGON-594); it rounds like a button, see [`pick_again_style`].
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

/// One recent-colour swatch: a fixed square in the colour, with an accent ring when it
/// is the colour currently loaded, and its hex as the tooltip.
fn recent_swatch<'a>(c: Srgb, index: usize, selected: bool) -> Element<'a, Msg> {
    let fill = cosmic::iced::Color::from_rgb8(c.r, c.g, c.b);
    let style = move |theme: &cosmic::Theme| {
        let mut s = cosmic::widget::button::Style::new();
        s.background = Some(Background::Color(fill));
        // The SAME radius the big swatch takes (DRAGON-587): one lookup, `swatch_radius`.
        s.border_radius = swatch_radius(theme).into();
        s.border_width = 1.0;
        // SUBDUED, the owner's review: the same quiet-text tone the slider thumbs and
        // the round swatch's rim wear, not a near-white/near-black neutral.
        s.border_color = theme::subdued(theme);
        if selected {
            // An outline OUTSIDE the swatch reads as selection without recolouring it,
            // the same affordance the settings accent palette uses.
            s.outline_width = 2.0;
            s.outline_color = theme::accent(theme);
        }
        s
    };
    let button = widget::button::custom(
        widget::space::Space::new().width(Length::Fill).height(Length::Fill),
    )
    .width(Length::Fixed(geom::RECENT_SWATCH))
    .height(Length::Fixed(geom::RECENT_SWATCH))
    .padding(0)
    .class(cosmic::theme::Button::Custom {
        active: Box::new(move |_f, t| style(t)),
        hovered: Box::new(move |_f, t| {
            let mut s = style(t);
            // The hover affordance: a brighter edge, since the fill IS the content and
            // must not change.
            s.border_color = theme::accent(t);
            s.border_width = 2.0;
            s
        }),
        pressed: Box::new(move |_f, t| style(t)),
        disabled: Box::new(move |t| style(t)),
    })
    .on_press(Msg::ColorPicker(ColorPickerMsg::LoadRecent(index)));
    widget::tooltip(
        crate::widgets::arrow_cursor::arrow_cursor(button),
        widget::text(c.hex()).size(12),
        widget::tooltip::Position::Top,
    )
    .into()
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

/// DRAGON-676: the mode chip shrank to its longest label, and the MENU is hung off that
/// same number. The panel's own inset and its rows' padding live in this file, not in
/// `geom`, so this is the only place the two halves can be checked against each other.
#[cfg(test)]
mod mode_menu_tests {
    use super::*;

    /// The label measurement the chip is sized from (`geom::widest_mode_label`), restated
    /// here because that one is private to `geom` and this test needs the same face.
    fn label_w(s: &str) -> f32 {
        crate::app::preview::text_annot::measure(
            crate::app::preview::text_annot::TextFont::Clean,
            geom::MODE_LABEL_SIZE,
            s,
        )
    }

    /// Every menu row's word fits inside the panel, with the panel's inset and the row's
    /// own padding taken off first. The chip is sized for the label PLUS a caret; a menu
    /// row has no caret, so it should fit with room to spare, and this is what would fail
    /// if the caret ever stopped being part of the chip's arithmetic.
    #[test]
    fn every_menu_row_fits_the_panel_the_chip_sizes() {
        let row_w = geom::mode_chip_width()
            - 2.0 * f32::from(MODE_MENU_PAD)
            - 2.0 * f32::from(geom::MODE_CHIP_PAD);
        for f in crate::color::ColorFormat::ALL {
            assert!(
                row_w >= label_w(f.label()),
                "{}: {row_w}pt of row for a {}pt word",
                f.id(),
                label_w(f.label())
            );
        }
    }

    /// The dropdown rounds like the VALUE BOXES it sits on, in every state and on both
    /// sides of the control (DRAGON-676, the owner's ask): the mode row and the box row
    /// are one group, and the chip used to take the BUTTON token while the boxes took the
    /// input one, so the two drifted apart on any theme where those differ.
    ///
    /// Checked on the dark and the light themes rather than only on `default()`, because
    /// the corner radii are a THEME field: a regression that read the right token on one
    /// and the wrong one on the other would pass a single-theme test.
    #[test]
    fn the_chip_and_its_menu_round_like_the_value_boxes() {
        for (name, t) in
            [("dark", cosmic::theme::Theme::dark()), ("light", cosmic::theme::Theme::light())]
        {
            let want = value_box_radius(&t);
            for (hovered, pressed) in [(false, false), (true, false), (true, true)] {
                assert_eq!(
                    mode_chip_style(&t, hovered, pressed).border_radius,
                    want,
                    "{name}: the closed chip (hovered={hovered} pressed={pressed}) drifted \
                     from the boxes"
                );
            }
            assert_eq!(
                mode_menu_style(&t).border.radius,
                want,
                "{name}: the popup drifted from the chip it hangs off"
            );
        }
    }

    /// And it really is a DIFFERENT number from the tokens it used to read, or the test
    /// above would pass on an unchanged file. Guarded by the tokens actually differing on
    /// this theme, since a theme is free to make every corner the same.
    #[test]
    fn the_boxes_radius_is_not_the_button_or_panel_token() {
        let t = cosmic::theme::Theme::dark();
        let want = value_box_radius(&t);
        let button = cosmic::iced::border::Radius::from(theme::rounding(&t).xl);
        let panel = cosmic::iced::border::Radius::from(theme::rounding(&t).m);
        assert!(
            want != button && want != panel,
            "the input, button and panel corners are all {want:?} on this theme, so nothing \
             here distinguishes the old derivation from the new one"
        );
    }

    /// The panel's height is still the exact sum of its parts, which is what the upward
    /// flyout's offset is. Nothing in DRAGON-676 touched it, and that is the point: the
    /// chip's WIDTH moved and the menu's placement did not.
    #[test]
    fn the_panel_height_is_still_the_sum_of_its_rows() {
        let n = crate::color::ColorFormat::ALL.len() as f32;
        assert_eq!(
            mode_menu_panel_h(),
            n * MODE_MENU_ITEM_H + (n - 1.0) * MODE_MENU_GAP + 2.0 * f32::from(MODE_MENU_PAD)
        );
    }
}

/// DRAGON-594: the pipette shares the recents row but is an ACTION, not a swatch, so it must
/// never wear a swatch's border, and it must still answer a hover.
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
            let s = pick_again_style(&t, hovered, pressed);
            assert_eq!(s.border_width, 0.0, "hovered={hovered} pressed={pressed}: a border came back");
        }
    }

    /// Borderless must not mean feedback-less. Hover and press are the app's ordinary
    /// icon-button fills, so both have to differ from the resting one: a control that answers
    /// nothing at all is a worse defect than the one this replaced.
    #[test]
    fn hover_and_press_still_answer() {
        let t = cosmic::Theme::default();
        let rest = fill(&pick_again_style(&t, false, false));
        assert_ne!(fill(&pick_again_style(&t, true, false)), rest, "hover says nothing");
        assert_ne!(fill(&pick_again_style(&t, true, true)), rest, "press says nothing");
    }

    /// It rounds like a BUTTON, not like a swatch. Both tokens follow the user's "Edge
    /// rounding" setting, so the control still tracks the theme; it just tracks it as the kind
    /// of thing it is.
    #[test]
    fn it_rounds_like_a_button_not_like_a_swatch() {
        let t = cosmic::Theme::default();
        let got = pick_again_style(&t, false, false).border_radius;
        assert_eq!(got.top_left, theme::rounding(&t).xl[0], "not the button token");
        assert_ne!(got.top_left, swatch_radius(&t)[0], "still rounding like a swatch");
    }
}
