//! THE composition every settings dialog is stacked with (DRAGON-502).
//!
//! A settings modal is always the same three layers: the settings WINDOW, a full-window
//! SCRIM that dims it and swallows pointer input, and the dialog CARD centred over both.
//! Five dialogs each built that by hand: the reset confirmation and the launch-time update
//! dialog (`settings/mod.rs`), the microphone test (`settings/mic_test.rs`), and Cloud
//! Accounts' add + disconnect (`settings/pages/cloud.rs`, which had already factored ITS
//! two into a local `stack_dialog`). All five come through [`stack_dialog`] now, and it is
//! the only place that knows what a modal looks like.
//!
//! # Why the unification is part of the fix, not a tidy-up beside it
//!
//! DRAGON-502: the scrim covered the window's TITLE AREA too, so while any dialog was up
//! the settings window could not be dragged at all, on any platform. The fix is a DRAG
//! STRIP: the scrim keeps its full-window dim, and its top strip, exactly the header's
//! height, hands presses to the window manager instead of eating them. Applied to one of
//! five hand-built copies that fix leaves four dialogs still pinning the window, which is
//! why the copies had to become one helper first.
//!
//! # The strip's height is not a constant, it IS a header bar
//!
//! The strip is a BARE `widget::header_bar()`: no title, no window controls, nothing packed
//! into it. libcosmic gives a header bar a fixed height derived from the theme's density and
//! the window's maximized state (`32 + its vertical padding`), and with `is_ssd` false it
//! paints no background and no border, so an empty one is an exactly-header-tall rectangle
//! that draws nothing. The strip therefore MEASURES the real header rather than guessing at
//! it: no second height constant to keep in step, and a theme density change moves the
//! header and the strip together by construction.
//!
//! This is also why the strip is not [`crate::widgets::DragArea`], the preview toolbar's
//! drag wrapper. That widget emits PAN DELTAS, for a layer-shell surface the app positions
//! itself. A real toplevel is moved by the compositor instead, through `window::drag`, which
//! is exactly what a header bar's own `on_drag` issues: the strip and the real header end up
//! sending the same `ConfigWindowDrag` message, so they cannot drift apart either.
//!
//! # Why the strip can never double as a dismiss
//!
//! `header_bar` wraps itself in a `mouse_area`, and a `mouse_area` CAPTURES every left press
//! over its bounds, whether or not it carries an `on_press`. The strip sits above the scrim
//! in the stack, and `stack` walks its children topmost-first and stops at the first capture,
//! so a press on the strip never reaches the backdrop beneath it. A dismissing backdrop (the
//! reset and disconnect confirmations) therefore dismisses only BELOW the strip, and there is
//! no hit test anywhere: the composition IS the decision, so there is nothing here to keep
//! pure and nothing to unit-test.
//!
//! The card is stacked ABOVE the strip for the same reason: a tall dialog reaching the top
//! of the window keeps its own controls, and only the scrim around it turns into a handle.
//!
//! # macOS fullscreen
//!
//! In fullscreen the settings window insets its header below the menu-bar reveal zone
//! (`FULLSCREEN_TOP_INSET`), so there the strip covers the inset rather than the header. A
//! fullscreen window cannot be dragged anyway, and DRAGON-303 already records that macOS
//! storms mouse enter/exit at that screen edge and cancels clicks there, so nothing is lost:
//! the strip is inert in a place that was already unreliable.

use super::*;

/// Stack a dialog card over the settings window with a scrim behind it, leaving the window's
/// title area draggable.
///
/// `on_backdrop` is the whole difference between the two dialog shapes: `Some` gives a
/// DISMISSING backdrop (right for a confirmation, where backing out is always safe), `None`
/// an INERT one (right for a flow that must not be lost to a stray click: the cloud
/// add/reconnect dialog, the update dialog, the mic test). Either way the top strip is a
/// drag surface and never a dismiss.
pub(in crate::app::settings) fn stack_dialog<'a>(
    window: Element<'a, Msg>,
    card: Element<'a, Msg>,
    on_backdrop: Option<Msg>,
) -> Element<'a, Msg> {
    let backdrop: Element<'a, Msg> = widget::mouse_area(
        widget::container(widget::Space::new().width(Length::Fill).height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .class(cosmic::theme::Container::custom(|_t| {
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(theme::SCRIM)),
                    ..Default::default()
                }
            })),
    )
    .on_press(on_backdrop.unwrap_or(Msg::WindowChrome(WindowChromeMsg::Ignore)))
    // Report a cursor interaction over the whole backdrop so the stack levitates the
    // pointer away from the settings page beneath it. Otherwise dropdowns and rows below
    // the modal still light up on hover.
    .interaction(cosmic::iced::mouse::Interaction::Idle)
    .into();
    // The drag strip (DRAGON-502): the window's title area stays a window handle while the
    // dialog is up. Bare, so it is exactly the header's height and draws nothing over the
    // scrim's dim; `on_drag`, so it issues the SAME `ConfigWindowDrag` the real header does.
    // The window BUTTONS underneath stay blocked, which is ordinary modal behaviour: only
    // the drag is given back.
    let drag_strip: Element<'a, Msg> = widget::header_bar()
        .on_drag(Msg::WindowChrome(WindowChromeMsg::ConfigWindowDrag))
        .into();
    let centered: Element<'a, Msg> = widget::container(card)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    cosmic::iced::widget::stack(vec![window, backdrop, drag_strip, centered]).into()
}
