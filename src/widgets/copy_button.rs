//! The app's ONE copy-to-clipboard control (DRAGON-489, extracted and shared by DRAGON-520).
//!
//! A small icon button that copies something and then says so: for [`COPIED_FLASH`] after a
//! press its glyph becomes a success-green tick and its tooltip reads "Copied!", and both revert
//! on their own. That two-second acknowledgement is the whole reason this is a component rather
//! than three lines at each call site. A copy is invisible: nothing on screen changes, the
//! clipboard is somewhere else, and without the flash a user cannot tell a press that worked from
//! one that missed the button.
//!
//! # Why it lives here and not in the settings page
//!
//! It was written for the cloud connect step's sign-in link (DRAGON-489) and stayed in
//! `app::settings::pages::cloud`. DRAGON-520 gave the preview editor's upload meter a second copy
//! button, for re-copying a finished upload's share link, and the owner asked for the two to be
//! the same control rather than a lookalike. Two lookalikes drift: the tick, the flash window and
//! the tooltip wording are three things that would have had to be kept in step by hand forever.
//!
//! # What a caller owns
//!
//! The FLASH STATE, as an `Option<Instant>` stamped when the press is handled, read back through
//! [`copied_recently`]. It is not held here because this is a view helper with no state of its
//! own, and because the two call sites keep it in different places (the settings dialog's
//! `CloudAdd`, the editor's `UploadWatch`). A caller also owns making sure something REDRAWS
//! while the flash is up, or the tick never reverts: the settings step has its once-a-second
//! countdown tick, the meter keeps its upload poll alive for the window
//! (`app::preview::edit::upload_needs_poll`).
//!
//! # Privacy
//!
//! Nothing here logs, and nothing here holds the copied VALUE: the caller hands over a message
//! that already carries whatever is to be copied. A share link is the user's content and must not
//! reach a log line.

use std::time::{Duration, Instant};

use cosmic::Element;
use cosmic::iced::Background;
use cosmic::widget;

use crate::app::theme;

/// How long the copy button reads "Copied!" (and shows a green tick) after a press.
///
/// Two seconds: long enough to be READ by someone whose eyes were on the thing they copied
/// rather than on the button, short enough that a second copy a moment later still looks like a
/// second copy rather than the first one still being acknowledged.
pub const COPIED_FLASH: Duration = Duration::from_secs(2);

/// Whether a copy stamped at `at` is still inside its flash window. Pure; unit-tested.
///
/// `None` (nothing has been copied from this control yet) is never flashing, which is also what
/// makes the flash need no explicit clear: it expires by the clock, not by a message.
pub fn copied_recently(at: Option<Instant>) -> bool {
    at.is_some_and(|stamp| stamp.elapsed() < COPIED_FLASH)
}

/// The copy control (DRAGON-489, shared since DRAGON-520).
///
/// `copied` is [`copied_recently`]'s answer, `halo` the padding around the 16px glyph (0 where
/// the button rides inside something whose height it must not drive, such as the upload meter's
/// pill), `tip_pos` where the "Copied!" / "Copy URL" tooltip drops (Bottom for the titlebar meter,
/// so it clears the icons beside it; Top for the cloud sign-in row, so it clears the QR code below
/// it), and `press` the message to send. The glyph size is libcosmic's own default for a symbolic
/// icon button, 16px, which is what both call sites want and neither should restate.
///
/// The tooltip is the explicit `widget::tooltip(button, text, pos)` wrapper the rest of the app
/// uses, NOT the `.tooltip(&str)` shorthand: the shorthand has no position control, so it dropped
/// its card wherever it liked and, in the titlebar meter, on top of the icon it was describing.
pub fn copy_button<M: Clone + 'static>(
    copied: bool,
    halo: u16,
    tip_pos: widget::tooltip::Position,
    press: M,
) -> Element<'static, M> {
    copy_button_with(copied, halo, tip_pos, "Copy URL", theme::accent, press)
}

/// The same control tinted to match SUBTLE text instead of the accent (DRAGON-540): for a copy
/// icon that sits INSIDE a line of quiet secondary text, such as the Health page's "here is
/// where ffmpeg actually is" line. The accent tint is right for a control that has to be found
/// on a page; beside its own text it reads as a second, louder thing to look at, and the owner
/// asked for it to match the text it follows.
///
/// Only the RESTING tint changes. The "Copied!" flash stays success-green, because that green
/// is the app's one "this worked" colour and an acknowledgement nobody notices is no
/// acknowledgement. `label` is the resting tooltip ("Copy URL" is [`copy_button`]'s, and is
/// wrong for anything that is not a link).
pub fn subtle_copy_button<M: Clone + 'static>(
    copied: bool,
    halo: u16,
    tip_pos: widget::tooltip::Position,
    label: &'static str,
    press: M,
) -> Element<'static, M> {
    copy_button_with(copied, halo, tip_pos, label, theme::subtle, press)
}

/// The one body both public copy controls are built from: `tint` is the RESTING glyph colour
/// and `label` the resting tooltip, and everything else (the glyph pair, the flash, the tick,
/// the "Copied!" wording, the tooltip mechanics) is shared so the two cannot drift.
fn copy_button_with<M: Clone + 'static>(
    copied: bool,
    halo: u16,
    tip_pos: widget::tooltip::Position,
    label: &'static str,
    tint: fn(&cosmic::Theme) -> cosmic::iced::Color,
    press: M,
) -> Element<'static, M> {
    let button = crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::icon(crate::widgets::icons::handle(if copied {
            // The same tick the folder list's chosen level wears, so "this worked" looks the
            // same everywhere in the app.
            "emblem-ok-symbolic"
        } else {
            "document-copy-symbolic"
        }))
        .class(icon_button_class(copied, tint))
        .padding(halo)
        .on_press(press),
    );
    widget::tooltip(
        button,
        widget::text(if copied { "Copied!" } else { label }).size(12),
        tip_pos,
    )
    .into()
}

/// The style an accent icon button carries (DRAGON-489): a steady accent tint with a faint hover
/// highlight, since it has no "at rest, nothing to do" state to grey out the way
/// `settings::row::reset_button`'s does. `success` flips it to the theme's success GREEN for the
/// [`COPIED_FLASH`] window, reusing `theme::success` (the same green `row::success_caption` uses)
/// rather than a new colour.
///
/// Public because the cloud settings page dresses three other icon buttons in it (the folder
/// browser's create, refresh and delete controls), which is why this is the button STYLE rather
/// than something private to [`copy_button`].
pub fn accent_icon_button_class(success: bool) -> cosmic::theme::Button {
    icon_button_class(success, theme::accent)
}

/// [`accent_icon_button_class`] with the resting tint chosen by the caller (see
/// [`subtle_copy_button`]). Private: the accent one is the page-level control style, and a
/// second public knob for it would invite icon buttons in colours nothing else on the page
/// uses.
fn icon_button_class(
    success: bool,
    tint: fn(&cosmic::Theme) -> cosmic::iced::Color,
) -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| icon_button_style(false, success, tint, theme)),
        hovered: Box::new(move |_focused, theme| icon_button_style(true, success, tint, theme)),
        pressed: Box::new(move |_focused, theme| icon_button_style(true, success, tint, theme)),
        disabled: Box::new(move |theme| icon_button_style(false, success, tint, theme)),
    }
}

fn icon_button_style(
    hovered: bool,
    success: bool,
    tint: fn(&cosmic::Theme) -> cosmic::iced::Color,
    theme: &cosmic::Theme,
) -> cosmic::widget::button::Style {
    let cosmic = theme.cosmic();
    let mut s = cosmic::widget::button::Style::new();
    s.border_radius = theme::rounding(theme).xl.into();
    // The flash is success GREEN whatever the resting tint is: one "this worked" colour for
    // the whole app.
    s.icon_color = Some(if success { theme::success(theme) } else { tint(theme) });
    if hovered {
        let mut bg: cosmic::iced::Color = cosmic.palette.neutral_5.into();
        bg.a = 0.1;
        s.background = Some(Background::Color(bg));
    }
    s
}

#[cfg(test)]
mod flash_tests {
    use super::*;

    /// The flash is a WINDOW, not a flag: it opens on the stamp and closes by the clock, so no
    /// message has to remember to clear it.
    #[test]
    fn the_flash_opens_on_a_press_and_closes_by_itself() {
        assert!(!copied_recently(None), "nothing copied yet is not a flash");
        assert!(copied_recently(Some(Instant::now())), "a press just now flashes");
        let stale = Instant::now().checked_sub(COPIED_FLASH * 2).expect("a representable instant");
        assert!(!copied_recently(Some(stale)), "an old copy has stopped flashing");
    }

    /// The tint is the ONLY thing a caller varies (DRAGON-540): at rest the subtle control
    /// wears the same colour as the text it sits in, and both controls flash the one success
    /// green. A tint that leaked into the flash would give the app two "this worked" colours.
    #[test]
    fn only_the_resting_tint_varies_and_the_flash_is_always_green() {
        let t = cosmic::Theme::default();
        assert_eq!(icon_button_style(false, false, theme::subtle, &t).icon_color, Some(theme::subtle(&t)));
        assert_eq!(icon_button_style(false, false, theme::accent, &t).icon_color, Some(theme::accent(&t)));
        assert_ne!(theme::subtle(&t), theme::accent(&t), "the two tints must be distinguishable");
        for tint in [theme::subtle as fn(&cosmic::Theme) -> cosmic::iced::Color, theme::accent] {
            assert_eq!(
                icon_button_style(false, true, tint, &t).icon_color,
                Some(theme::success(&t)),
                "the flash must be success green whatever the resting tint is"
            );
        }
    }

    /// Long enough to be read, short enough that a second copy reads as a second copy. Pinned so
    /// neither call site can quietly retune the other's acknowledgement.
    #[test]
    fn the_flash_window_is_readable_and_brief() {
        assert!(COPIED_FLASH >= Duration::from_secs(1), "too brief to be read");
        assert!(COPIED_FLASH <= Duration::from_secs(4), "long enough to look stuck");
    }
}
