//! Declarative row/section types and generic widget helpers for the settings
//! window. Page builders in `mod.rs` assemble these into the rendered sections.

use super::*;
use std::borrow::Cow;

/// Severity of a status / dependency line — the single vocabulary for the app's
/// success / warning / error styling. Drives both the icon glyph and the colour
/// (which come from the canonical [`theme`] palette). Ordered `Ok < Warn < Error`
/// so the worst severity in a set is just `.max()`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Severity {
    /// Present / passing (green).
    Ok,
    /// An optional feature is unavailable (amber).
    Warn,
    /// A critical dependency is missing (red).
    Error,
}

impl Severity {
    /// The symbolic icon glyph for this severity.
    pub(super) fn icon_name(self) -> &'static str {
        match self {
            Severity::Ok => "emblem-ok-symbolic",
            Severity::Warn => "dialog-warning-symbolic",
            Severity::Error => "dialog-error-symbolic",
        }
    }

    /// The canonical theme colour for this severity (green / amber / red).
    pub(super) fn color(self, theme: &cosmic::Theme) -> cosmic::iced::Color {
        match self {
            Severity::Ok => theme::success(theme),
            Severity::Warn => theme::warning(theme),
            Severity::Error => theme::danger(theme),
        }
    }
}

/// One settings row: a title, a subdued helper line, a control on the right, and
/// an optional reset — the setter carrying the default value, plus whether the
/// current value differs from that default (so the icon can stand out when so).
pub(super) struct Item<'a> {
    pub(super) title: Cow<'a, str>,
    pub(super) desc: Cow<'a, str>,
    pub(super) control: Element<'a, Msg>,
    pub(super) reset: Option<(Msg, bool)>,
    /// Render the title in the secondary tone (for informational lines like
    /// benchmark output, not real settings).
    pub(super) dim: bool,
    /// A unit label (fps, Kbps, …) shown in a fixed slot just right of the
    /// control, so the control's right edge lines up across all rows.
    pub(super) suffix: Option<&'static str>,
    /// Colour the helper line by severity (Ok=green, Warn=amber, Error=red). `None`
    /// leaves it the normal text colour.
    pub(super) severity: Option<Severity>,
    /// A rich helper line (e.g. with inline links) shown instead of `desc`.
    pub(super) desc_el: Option<Element<'a, Msg>>,
    /// Render `desc_el` as the WHOLE row (full width, no title, no control) - used for
    /// inline status notes like "[ok] ffmpeg: screen recording is available."
    pub(super) note: bool,
    /// This row's control is gated by a missing dependency: tint the title to this
    /// severity (the page supplies an inert, subdued value via `gated_row`).
    pub(super) gated: Option<Severity>,
}

impl<'a> Item<'a> {
    pub(super) fn new(
        title: impl Into<Cow<'a, str>>,
        desc: impl Into<Cow<'a, str>>,
        control: impl Into<Element<'a, Msg>>,
    ) -> Self {
        Self {
            title: title.into(),
            desc: desc.into(),
            control: control.into(),
            reset: None,
            dim: false,
            suffix: None,
            severity: None,
            desc_el: None,
            note: false,
            gated: None,
        }
    }

    /// A full-width informational line (no title, no control): renders `element` as
    /// the entire row. Used for inline dependency notes (see `deps::Dep::note`).
    pub(super) fn note(element: impl Into<Element<'a, Msg>>) -> Self {
        let mut it = Item::new("", "", widget::text(""));
        it.desc_el = Some(element.into());
        it.note = true;
        it
    }

    /// A full-width row that renders `element` as the ENTIRE row (like [`note`]) but
    /// keeps a searchable `title`/`desc` so the global settings search still finds it.
    /// Used where the control is a full-width block that must sit BELOW its label (the
    /// Accent swatches, DRAGON-268) rather than inline in the right-hand control slot.
    /// The caller is responsible for drawing the label (and any reset affordance)
    /// inside `element`.
    pub(super) fn full_width(
        title: impl Into<Cow<'a, str>>,
        desc: impl Into<Cow<'a, str>>,
        element: impl Into<Element<'a, Msg>>,
    ) -> Self {
        let mut it = Item::new(title, desc, widget::text(""));
        it.desc_el = Some(element.into());
        it.note = true;
        it
    }

    /// Attach a rich helper line (with inline links etc.) shown instead of `desc`.
    pub(super) fn desc_el(mut self, el: impl Into<Element<'a, Msg>>) -> Self {
        self.desc_el = Some(el.into());
        self
    }

    /// Mark this as an informational (dimmed) line rather than a setting.
    pub(super) fn dim(mut self) -> Self {
        self.dim = true;
        self
    }

    /// Colour the helper line by severity (Ok=green, Warn=amber, Error=red).
    pub(super) fn status(mut self, sev: Severity) -> Self {
        self.severity = Some(sev);
        self
    }

    /// Mark this row as gated by a missing dependency: tint the title to `sev`.
    pub(super) fn gated(mut self, sev: Severity) -> Self {
        self.gated = Some(sev);
        self
    }

    /// Attach a unit label shown in the fixed slot right of the control.
    pub(super) fn suffix(mut self, suffix: &'static str) -> Self {
        self.suffix = Some(suffix);
        self
    }

    /// Attach a reset-to-default action directly: the `msg` to dispatch and whether
    /// the value currently differs from default (so the icon stands out). For rows
    /// whose setter needs to carry extra context the `fn(T) -> Msg` form can't (e.g.
    /// a per-action keyboard binding).
    pub(super) fn reset_to(mut self, msg: Msg, changed: bool) -> Self {
        self.reset = Some((msg, changed));
        self
    }

    /// Attach a reset-to-default action. `current`/`default` build both the reset
    /// message (the `setter` applied to `default`, reusing the normal update
    /// path) and the "changed" flag (whether `current` differs from `default`).
    pub(super) fn reset_with<T: PartialEq>(
        mut self,
        current: T,
        default: T,
        setter: fn(T) -> Msg,
    ) -> Self {
        let changed = current != default;
        self.reset = Some((setter(default), changed));
        self
    }
}

/// A titled group of rows.
pub(super) struct SectionSpec<'a> {
    pub(super) title: &'a str,
    pub(super) items: Vec<Item<'a>>,
}

/// A toggle control (used as the right-aligned control of a settings row).
pub(super) fn toggle<'a>(on: bool, msg: fn(bool) -> Msg) -> Element<'a, Msg> {
    widget::toggler(on).on_toggle(msg).into()
}

/// The "choose folder" button used next to a path field. Translucent fill
/// (see [`standard_button_class`]).
pub(super) fn folder_btn<'a>(target: DirTarget) -> Element<'a, Msg> {
    widget::button::custom(
        widget::icon::Icon::from(widget::icon::from_name("folder-open-symbolic").size(16))
            .width(Length::Fixed(20.0))
            .height(Length::Fixed(20.0)),
    )
    .class(standard_button_class())
    .on_press(Msg::Settings(SettingsMsg::PickDir(target)))
    .padding(8.0)
    .into()
}

/// A standard text button used as a settings row's control — e.g. a Health row's
/// remediation action ("Open Settings" / "Request"). `msg` fires on press.
/// Translucent fill (see [`standard_button_class`]).
pub(super) fn action_button<'a>(label: &'a str, msg: Msg) -> Element<'a, Msg> {
    widget::button::standard(label)
        .class(standard_button_class())
        .on_press(msg)
        .into()
}

/// A text button whose icon + label are CENTERED within the button's width, rather
/// than sitting flush-left. `button::standard`/`suggested`/`custom` all lay their
/// inner row out leading, so when the button is wider than its content (a Fill or
/// a Fixed action width) the text hangs at the left edge. This mirrors the
/// `toggle_nav` idiom (mod.rs): wrap the content row in a `center_x(Length::Fill)`
/// container so it centres in whatever box the button paints. `class` is the
/// button material (e.g. [`standard_button_class`] for the settings pill, or
/// `cosmic::theme::Button::Suggested` for the accent Install button); the 8px
/// icon/label spacing and the box (space_l height, space_s inner horizontal
/// padding) match `button::standard`/`suggested`, so the only visible change is
/// the content's horizontal alignment.
///
/// `icon` is an optional leading symbolic icon name; `width` is the button box
/// width (pass `Length::Fill` for the full-width rail button, `Length::Fixed`
/// for the fixed action slot, `Length::Shrink` to size to content). `on_press`
/// is `None` for a disabled (non-clickable) button.
pub(super) fn centered_button<'a>(
    icon: Option<&'static str>,
    label: impl Into<Cow<'a, str>> + 'a,
    width: Length,
    class: cosmic::theme::Button,
    on_press: Option<Msg>,
) -> Element<'a, Msg> {
    // The same box tokens `button::standard`/`suggested` derive from the theme
    // (space_l tall, space_s inner horizontal padding), so the centred custom
    // button paints an identically sized chip.
    let (h, pad_x) = {
        let t = cosmic::theme::active();
        let c = t.cosmic();
        (c.space_l() as f32, c.space_s() as f32)
    };
    let mut inner = widget::row::with_capacity(2).spacing(8).align_y(Alignment::Center);
    if let Some(name) = icon {
        inner = inner.push(widget::icon::from_name(name).icon().size(16));
    }
    inner = inner.push(widget::text(label));
    let btn = widget::button::custom(
        widget::container(inner)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .class(class)
    .padding([0.0, pad_x])
    .height(Length::Fixed(h))
    .width(width);
    match on_press {
        Some(msg) => btn.on_press(msg).into(),
        None => btn.into(),
    }
}

/// The Standard settings button painted in the SHARED PILL MATERIAL (DRAGON-279,
/// user decision 2026-07-19): the same `neutral_5` fill the nav rail's active pill
/// and the item-row cards use ([`theme::pill_fill`]), resting at
/// [`theme::PILL_ALPHA`] and bumping on hover/press so it still reads as a
/// control. A button inside a row stacks its fill over the row's — the Fluent
/// layering — and any retune of the shared material moves pills, rows, and
/// buttons together. Text/icon colours are left unset (they inherit), and the
/// focus ring + high-contrast border are preserved.
pub(super) fn standard_button_class() -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(move |focused, theme| standard_frost_style(theme, focused, StdBtnState::Active)),
        hovered: Box::new(move |focused, theme| standard_frost_style(theme, focused, StdBtnState::Hover)),
        pressed: Box::new(move |focused, theme| standard_frost_style(theme, focused, StdBtnState::Pressed)),
        disabled: Box::new(move |theme| standard_frost_style(theme, false, StdBtnState::Disabled)),
    }
}

/// The interaction state a [`standard_frost_style`] draws — one per closure slot of
/// the custom [`cosmic::theme::Button`].
#[derive(Clone, Copy)]
enum StdBtnState {
    Active,
    Hover,
    Pressed,
    Disabled,
}

/// The pill-material settings-button [`Style`](cosmic::widget::button::Style) for
/// one interaction state (see [`standard_button_class`]): [`theme::pill_fill`] at
/// the state's alpha — rest = the nav pill's exact active fill, hover/pressed
/// bumped, disabled dimmed. The border radius, focus ring, and high-contrast
/// border match libcosmic's shared button tail.
fn standard_frost_style(
    theme: &cosmic::Theme,
    focused: bool,
    state: StdBtnState,
) -> cosmic::widget::button::Style {
    let cosmic = theme.cosmic();
    let comp = &cosmic.button; // still used for the high-contrast border colour below
    let mut s = cosmic::widget::button::Style::new();
    s.border_radius = cosmic.corner_radii.radius_xl.into();
    let alpha = match state {
        StdBtnState::Active => theme::PILL_ALPHA,
        StdBtnState::Hover => theme::PILL_HOVER_ALPHA,
        StdBtnState::Pressed => theme::PILL_PRESSED_ALPHA,
        StdBtnState::Disabled => theme::PILL_DISABLED_ALPHA,
    };
    s.background = Some(Background::Color(theme::pill_fill(theme, alpha)));
    // High-contrast Standard draws a 1px component border at rest.
    if theme.theme_type.is_high_contrast() {
        s.border_color = comp.border.into();
        s.border_width = 1.0;
    }
    // Focus ring — matches libcosmic's shared `appearance` tail (overrides the border).
    if focused {
        s.outline_width = 1.0;
        s.outline_color = cosmic.accent.base.into();
        s.border_width = 2.0;
        s.border_color = cosmic::iced::Color::TRANSPARENT;
    }
    s
}

/// A small "reset to default" icon button; `msg` is the setter carrying the
/// default value, so pressing it goes through the normal update path. `changed`
/// (the value differs from default) makes it stand out in the accent colour.
pub(super) fn reset_button(msg: Msg, changed: bool) -> Element<'static, Msg> {
    let btn = widget::button::icon(widget::icon::from_name("edit-undo-symbolic").size(14))
        .padding(4)
        .class(reset_class(changed));
    if changed {
        // Only interactive when there's something to reset: clickable, hoverable,
        // with a tooltip.
        widget::tooltip(
            btn.on_press(msg),
            widget::text("Reset to default").size(12),
            widget::tooltip::Position::Top,
        )
        .into()
    } else {
        // At default: a disabled (non-hoverable, non-clickable) subdued icon.
        btn.into()
    }
}

/// Style for the reset icon. At rest it's the accent (trim) colour when the value
/// differs from default, otherwise a faint subdued tone (the button widget
/// clobbers icon alpha, so subtlety comes from the colour). Hover brightens
/// non-default icons to the full foreground and adds a faint background.
fn reset_style(
    hovered: bool,
    changed: bool,
    theme: &cosmic::Theme,
) -> cosmic::widget::button::Style {
    let cosmic = theme.cosmic();
    let mut s = cosmic::widget::button::Style::new();
    // The button token — matches libcosmic's own icon buttons.
    s.border_radius = theme::rounding(theme).xl.into();
    s.icon_color = Some(if changed {
        theme::accent(theme)
    } else if hovered {
        cosmic.on_bg_color().into()
    } else {
        theme::subdued(theme)
    });
    if hovered {
        let mut bg: cosmic::iced::Color = cosmic.palette.neutral_5.into();
        bg.a = 0.1;
        s.background = Some(Background::Color(bg));
    }
    s
}

fn reset_class(changed: bool) -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| reset_style(false, changed, theme)),
        hovered: Box::new(move |_focused, theme| reset_style(true, changed, theme)),
        pressed: Box::new(move |_focused, theme| reset_style(true, changed, theme)),
        disabled: Box::new(move |theme| reset_style(false, changed, theme)),
    }
}

/// Caption text in the theme's success colour (green) — for a passing value.
pub(super) fn success_caption<'a>(s: impl Into<Cow<'a, str>> + 'a) -> Element<'a, Msg> {
    widget::text::caption(s)
        .class(cosmic::theme::Text::Custom(|theme| {
            cosmic::iced::widget::text::Style {
                color: Some(theme::success(theme)),
                ..Default::default()
            }
        }))
        .into()
}

/// Caption text in the theme's warning colour — for cautionary helper lines.
pub(super) fn warning_caption<'a>(s: impl Into<Cow<'a, str>> + 'a) -> Element<'a, Msg> {
    widget::text::caption(s)
        .class(cosmic::theme::Text::Custom(|theme| {
            cosmic::iced::widget::text::Style {
                color: Some(theme::warning(theme)),
                ..Default::default()
            }
        }))
        .into()
}

/// Caption text in the theme's danger colour (red) — for error/critical notices.
pub(super) fn danger_caption<'a>(s: impl Into<Cow<'a, str>> + 'a) -> Element<'a, Msg> {
    widget::text::caption(s)
        .class(cosmic::theme::Text::Custom(|theme| {
            cosmic::iced::widget::text::Style {
                color: Some(theme::danger(theme)),
                ..Default::default()
            }
        }))
        .into()
}

/// Caption text coloured by [`Severity`] (green / amber / red) — the unified entry
/// point so every status line in settings draws from the same palette.
pub(super) fn severity_caption<'a>(sev: Severity, s: impl Into<Cow<'a, str>> + 'a) -> Element<'a, Msg> {
    match sev {
        Severity::Ok => success_caption(s),
        Severity::Warn => warning_caption(s),
        Severity::Error => danger_caption(s),
    }
}

/// A severity-coloured status icon (tick / warning / error) sized for a row's
/// control slot. Symbolic icons inherit the SVG colour filter, so this tints the
/// glyph to the canonical severity colour.
pub(super) fn status_icon<'a>(sev: Severity) -> Element<'a, Msg> {
    widget::icon::from_name(sev.icon_name())
        .icon()
        .size(16)
        .class(cosmic::theme::Svg::custom(move |theme| cosmic::widget::svg::Style {
            color: Some(sev.color(theme)),
        }))
        .into()
}

/// Bold title text tinted to a [`Severity`] colour (for a gated/unavailable row).
/// `theme::Text::Custom` is a fn pointer, so each severity uses its own static
/// closure rather than capturing `sev`.
pub(super) fn severity_title<'a>(sev: Severity, s: impl Into<Cow<'a, str>> + 'a) -> Element<'a, Msg> {
    let base = widget::text::body(s).font(cosmic::font::bold());
    match sev {
        Severity::Ok => base.class(cosmic::theme::Text::Custom(|t| {
            cosmic::iced::widget::text::Style { color: Some(theme::success(t)), ..Default::default() }
        })),
        Severity::Warn => base.class(cosmic::theme::Text::Custom(|t| {
            cosmic::iced::widget::text::Style { color: Some(theme::warning(t)), ..Default::default() }
        })),
        Severity::Error => base.class(cosmic::theme::Text::Custom(|t| {
            cosmic::iced::widget::text::Style { color: Some(theme::danger(t)), ..Default::default() }
        })),
    }
    .into()
}

/// Caption text in the subdued tone - for an inert value shown in place of a control.
pub(super) fn subdued_caption<'a>(s: impl Into<Cow<'a, str>> + 'a) -> Element<'a, Msg> {
    widget::text::caption(s)
        .class(cosmic::theme::Text::Custom(|t| {
            cosmic::iced::widget::text::Style { color: Some(theme::subdued(t)), ..Default::default() }
        }))
        .into()
}

/// A setting row whose control is unavailable because a dependency is missing: the
/// title is tinted to `sev`, and `value` is shown as inert, subdued text in place of
/// the real control (no interaction, no pointer cursor).
pub(super) fn gated_row<'a>(
    title: impl Into<Cow<'a, str>>,
    value: impl Into<Cow<'a, str>> + 'a,
    sev: Severity,
) -> Item<'a> {
    Item::new(title, "", subdued_caption(value)).gated(sev)
}

/// A fixed-width numeric input (the unit label, if any, is added by the row via
/// `Item::suffix`). `on_input` is `None` for a read-only field.
pub(super) fn num_input<'a>(
    placeholder: &'a str,
    value: &'a str,
    on_input: Option<fn(String) -> Msg>,
) -> Element<'a, Msg> {
    let mut input = widget::text_input(placeholder, value).width(Length::Fixed(80.0));
    if let Some(f) = on_input {
        input = input.on_input(f);
    }
    // Hide the input's glyphs cleanly when it scrolls under the pinned tab strip
    // (text_input leaks its value text past the scroll clip; see the wrapper doc).
    crate::widgets::hide_when_clipped(input)
}

/// An opacity slider (0–100%) with its percentage readout, as a row control.
pub(super) fn opacity_slider<'a>(value: f32, msg: fn(f32) -> Msg) -> Element<'a, Msg> {
    widget::row(vec![
        widget::slider(0.0..=1.0, value, msg)
            .step(0.01_f32)
            .width(Length::Fixed(200.0))
            .into(),
        // Fixed-width, right-aligned readout so the slider never shifts as the
        // percentage text changes width (e.g. "5%" -> "100%").
        widget::container(widget::text(format!("{:.0}%", value * 100.0)).size(13))
            .width(Length::Fixed(40.0))
            .align_x(Alignment::End)
            .into(),
    ])
    .spacing(8.0)
    .align_y(Alignment::Center)
    .into()
}
