//! The colour picker's two views: the dimmed magnifier OVERLAY and the result WINDOW.
//!
//! Both are built from ordinary iced elements. Nothing here uses
//! `cosmic::iced::widget::shader`, and that is a hard constraint rather than a
//! preference: Windows 10 renders our overlays through iced's software rasterizer
//! (`platform::software_overlays`), which cannot draw a shader widget at all. The
//! magnifier is a `widget::image` over a `widget::container` ring, and the swatches are
//! containers, so the whole tool works on every renderer the app ships with.

use super::*;

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
        // the user as the colour they picked.
        let dim = self.dim_now(self.color_picker_overlay_opacity);
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
            layers.push(self.magnifier_layer(h));
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

    /// The magnifier disc, absolutely placed where [`geom::disc_view`] put it, and CLIPPED
    /// by the screen edge rather than moved or resized by it.
    ///
    /// Its centre is the SAMPLE POINT (DRAGON-587), which is the pointer itself where the sprite
    /// can be hidden and one point up and left of it where the arrow is on screen. Either way
    /// the lens sits on the pointer; the update handler already resolved which, and the disc's
    /// own contents are the same picture.
    ///
    /// One piece now: the pre-rasterised disc, built in the update handler (never in `view` —
    /// a per-frame `Handle::from_rgba` mints a new id and forces a GPU re-upload every
    /// redraw), accent ring and all.
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
    fn magnifier_layer<'a>(&'a self, h: &'a Hover) -> Element<'a, Msg> {
        let disc = widget::image::Image::new(h.magnifier.clone())
            .width(Length::Fixed(h.disc.size.0 as f32))
            .height(Length::Fixed(h.disc.size.1 as f32))
            .filter_method(cosmic::iced::widget::image::FilterMethod::Nearest);
        absolute(disc.into(), (h.disc.origin.0 as f32, h.disc.origin.1 as f32))
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

    /// The colour picker window: a CSD header, the full-width swatch, one row per
    /// notation, and the recent-colours strip.
    ///
    /// Nothing SCROLLS. The window is sized from these exact parts
    /// ([`geom::color_window_size`]), so a scrollbar would only ever mean the sizing
    /// function had drifted from the layout.
    pub(in crate::app) fn color_picker_window_view(&self) -> Element<'_, Msg> {
        let focused = self.core.focused_window() == self.color_picker.window;
        let header = widget::header_bar()
            .title(WINDOW_TITLE)
            .focused(focused)
            .on_drag(Msg::WindowChrome(WindowChromeMsg::ColorPickerWindowDrag));
        // macOS: the native traffic lights carry close (the window opens with a
        // transparent titlebar over our header), so no CSD close is drawn there. Every
        // other platform draws its own, exactly as the settings window does.
        #[cfg(not(target_os = "macos"))]
        let header = header.on_close(Msg::WindowChrome(WindowChromeMsg::Close));

        let mut items: Vec<Element<'_, Msg>> = vec![self.color_swatch()];
        items.push(
            widget::column(
                crate::color::ColorFormat::ALL
                    .into_iter()
                    .map(|f| self.color_row(f))
                    .collect::<Vec<_>>(),
            )
            .spacing(geom::ROW_GAP)
            .into(),
        );
        items.push(self.recent_colors_row());

        let content = widget::container(
            widget::column(items).spacing(geom::SECTION_GAP).width(Length::Fill),
        )
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
            .padding(1)
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

    /// The full-width swatch of the current colour. Its corners come from the app's
    /// configured "Edge rounding" through [`swatch_radius`], the ONE radius every swatch in
    /// this window shares, so switching Round / Slightly Round / Square in settings moves
    /// the whole window's corners together.
    fn color_swatch(&self) -> Element<'_, Msg> {
        let c = self.color_picker.color;
        let fill = cosmic::iced::Color::from_rgb8(c.r, c.g, c.b);
        widget::container(widget::space::Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(geom::SWATCH_H))
            .class(cosmic::theme::Container::custom(move |t| {
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(fill)),
                    border: Border {
                        radius: swatch_radius(t).into(),
                        width: 1.0,
                        color: t.cosmic().palette.neutral_8.into(),
                    },
                    ..Default::default()
                }
            }))
            .into()
    }

    /// One notation row: its label, an editable value box, and a copy button.
    ///
    /// The value box shows the live DRAFT while this row is the one being typed into,
    /// and the canonical spelling otherwise, so a half-typed value is never rewritten
    /// under the caret (see `ColorPickerState::draft`).
    fn color_row(&self, format: crate::color::ColorFormat) -> Element<'_, Msg> {
        let text = self.color_picker.row_text(format);
        let copied = self
            .color_picker
            .copied
            .is_some_and(|(f, at)| f == format && crate::widgets::copy_button::copied_recently(Some(at)));
        widget::row(vec![
            widget::container(widget::text::body(format.label()))
                .width(Length::Fixed(geom::ROW_LABEL_W))
                .align_y(Alignment::Center)
                .into(),
            widget::text_input("", text)
                .on_input(move |s| Msg::ColorPicker(ColorPickerMsg::RowEdited(format, s)))
                .on_submit(|_| Msg::ColorPicker(ColorPickerMsg::RowCommitted))
                .width(Length::Fixed(geom::ROW_INPUT_W))
                .into(),
            crate::widgets::copy_button::subtle_copy_button(
                copied,
                4,
                widget::tooltip::Position::Left,
                "Copy",
                Msg::ColorPicker(ColorPickerMsg::CopyRow(format)),
            ),
        ])
        .spacing(geom::ROW_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fixed(geom::ROW_H))
        .into()
    }

    /// The bottom row: the recent-colours strip from the left, and the pick-again pipette
    /// pinned to the RIGHT edge of the same row (DRAGON-587).
    ///
    /// Clicking a swatch LOADS it and nothing else: the list never reorders (see
    /// [`geom::writes_recents`]). Each swatch carries its hex as a tooltip, and takes
    /// the same configured rounding as the big swatch above.
    ///
    /// The strip is the row's FILL child, so the pipette holds the right edge whether there
    /// are no recents or a full ten. The window is sized so both ends fit at the cap
    /// ([`geom::color_window_size`]), which matters more now that it cannot be resized.
    fn recent_colors_row(&self) -> Element<'_, Msg> {
        let recents = &self.color_picker.recents;
        let strip: Element<'_, Msg> = if recents.is_empty() {
            // Nothing picked yet on this machine. A row of empty boxes would read as
            // broken, so say what the strip is for instead.
            widget::container(widget::text::caption("Colors you pick appear here."))
                .height(Length::Fixed(geom::RECENT_SWATCH))
                .align_y(Alignment::Center)
                .into()
        } else {
            let current = self.color_picker.color;
            let swatches: Vec<Element<'_, Msg>> = recents
                .iter()
                .take(geom::RECENTS_CAP)
                .enumerate()
                .map(|(i, c)| recent_swatch(*c, i, *c == current))
                .collect();
            widget::row(swatches)
                .spacing(geom::RECENT_GAP)
                .align_y(Alignment::Center)
                .height(Length::Fixed(geom::RECENT_SWATCH))
                .into()
        };
        widget::row(vec![
            widget::container(strip).width(Length::Fill).into(),
            pick_again_button(),
        ])
        .spacing(geom::ROW_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fixed(geom::RECENT_SWATCH))
        .into()
    }
}

/// The pick-again pipette: start a new pick, exactly as launching the tool does.
///
/// The same lucide `pipette` the tray entry and the editor's toolbar button wear
/// (`MenuIcon::ColorPicker` vendors it, `icons::handle` maps the name), so the tool has one
/// glyph everywhere. Square and swatch-sized so it lines up with the row it shares, but
/// dressed as a BARE ICON BUTTON rather than as a swatch: see [`pick_again_style`].
fn pick_again_button<'a>() -> Element<'a, Msg> {
    let glyph = widget::icon(crate::widgets::icons::handle("color-select-symbolic")).size(16);
    let button = widget::button::custom(
        widget::container(glyph).center_x(Length::Fill).center_y(Length::Fill),
    )
    .width(Length::Fixed(geom::PICK_AGAIN_W))
    .height(Length::Fixed(geom::RECENT_SWATCH))
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
        s.border_color = theme.cosmic().palette.neutral_8.into();
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
