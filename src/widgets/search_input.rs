//! The app's ONE expanding search field (DRAGON-687 item six extracted it; the settings
//! window's header search built it first, DRAGON-158/680).
//!
//! A plain `text_input` reproducing libcosmic's `search_input` styling (the Search
//! style, `[0, space_xxs]` padding, a 16px leading magnifier and a trailing clear
//! button) but drawing the glyphs through OUR Lucide icon set (`icons::handle`) rather
//! than the libcosmic built-ins: `system-search-symbolic` maps to Lucide `search`,
//! `window-close-symbolic` to Lucide `x`. The trailing clear button carries the caller's
//! clear message, the same action libcosmic's `on_clear` wires up.
//!
//! Extracted rather than copied when the colour picker's Saved Palettes tab grew the
//! same field: the construction was page-local to the settings window, and a second
//! hand-kept copy is how two search fields drift apart one bump at a time. The CALLER
//! keeps everything stateful (its active flag, its query string, its focus id and its
//! messages); this is only the widget the settings window always built.

use cosmic::iced::Length;
use cosmic::widget;

use crate::app::theme;

/// The expanded search field: value, focus id, fixed width, and the caller's input and
/// clear messages. The collapsed state's icon BUTTON stays with each caller, because its
/// dress is surface-specific (the settings header's macOS halo, the picker's create-row
/// icon pair) while this field is not.
///
/// `on_unfocus` is optional because the two callers genuinely differ: the picker's field
/// collapses back to its icon when it loses focus empty (its message carries that rule),
/// while the settings header's field stays up until Escape or its clear button says
/// otherwise, so it passes `None` rather than a message it would ignore.
pub fn search_input<'a, M: Clone + 'static>(
    placeholder: &'a str,
    value: &'a str,
    id: widget::Id,
    width: f32,
    on_input: impl Fn(String) -> M + 'a,
    on_clear: M,
    on_unfocus: Option<M>,
) -> cosmic::Element<'a, M> {
    let spacing = cosmic::theme::active().cosmic().space_xxs();
    let input = widget::text_input(placeholder, value)
        .padding([0, spacing])
        // DRAGON-680: the SEARCH appearance with the app's softened text-selection fill.
        .style(theme::input_style(theme::InputBase::Search))
        .leading_icon(
            widget::container(
                widget::icon::icon(crate::widgets::icons::handle("system-search-symbolic"))
                    .size(16),
            )
            .padding(8)
            .into(),
        )
        .trailing_icon(
            widget::button::custom(
                // ACCENT-tinted (the owner's ask: "the x should be accent colored"):
                // the live accent through the theme helper, the app's usual
                // tinted-glyph-in-an-icon-button shape (the palette pipette and plus),
                // so hover and pressed keep the accent ink and speak through the
                // button's own state chrome. `icons::tinted` rather than a bare icon
                // because an intervening container would otherwise swallow any
                // inherited ink (its doc carries that lesson). Both consumers inherit:
                // this is the ONE search construction.
                crate::widgets::icons::tinted(
                    crate::widgets::icons::handle("window-close-symbolic"),
                    crate::app::theme::accent,
                )
                .size(16),
            )
            .class(cosmic::theme::Button::Icon)
            .on_press(on_clear)
            .padding(8)
            .into(),
        )
        .width(Length::Fixed(width))
        .id(id)
        .on_input(on_input);
    match on_unfocus {
        // No `on_unfocus_maybe` upstream, so the Option is unpacked here.
        Some(m) => input.on_unfocus(m).into(),
        None => input.into(),
    }
}
