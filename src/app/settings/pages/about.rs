//! About settings page section builder.

use super::super::*;
use super::super::row::{Item, SectionSpec};

/// The app icon, compiled in so the About page never depends on the icon being
/// installed system-wide (packaging installs the same file to hicolor).
const APP_ICON: &[u8] =
    include_bytes!("../../../../res/icons/dev.frosthaven.CosmicCaptureKit.svg");

/// Still the Linux "Get Update" target (releases page); the About page no longer
/// shows a source-code row (DRAGON-226).
const REPO_URL: &str = "https://github.com/Frosthaven/cosmic-capture-kit";
const ICON_ARTIST_URL: &str = "https://ashleythedesigner.com/";
/// Donations (DRAGON-226): PayPal is THE donation channel — no other sponsor
/// platforms. The same URL feeds `.github/FUNDING.yml`.
const DONATE_URL: &str = "https://paypal.me/Frosthaven";

/// The maximum height of the scrollable release-notes block: long notes never
/// stretch the About page, they scroll within this box (DRAGON-177).
const NOTES_MAX_HEIGHT: f32 = 280.0;

/// Right padding applied to the notes content so its right edge lines up with the
/// right edge of the toggles on the same page (DRAGON-187). Every settings row
/// reserves a fixed unit slot + reset slot right of its control (see
/// `render_specs`), so a control's (toggle's) right edge is inset from the row's
/// right content edge by that fixed amount. The note row spans the full row width
/// with no such slots, so it needs the same inset baked in as right padding for
/// its text to stop exactly where the toggles do.
///
/// Breakdown (matches the `control` row in `render_specs`): the control row is
/// `[control, spacing, suffix_slot, spacing, reset_slot]` - the toggle's right
/// edge trails the row edge by `spacing + suffix_slot + spacing + reset_slot`.
const NOTES_TOGGLE_EDGE_GAP: f32 = {
    // Keep these in lockstep with `render_specs`' slot widths + `.spacing(8.0)`.
    const CONTROL_ROW_SPACING: f32 = 8.0;
    const SUFFIX_SLOT_W: f32 = 24.0;
    const RESET_SLOT_W: f32 = 28.0;
    CONTROL_ROW_SPACING + SUFFIX_SLOT_W + CONTROL_ROW_SPACING + RESET_SLOT_W
};

impl crate::app::App {
    pub(in crate::app::settings) fn about_sections(&self) -> Vec<SectionSpec<'_>> {
        // The Version row (DRAGON-177 polish, DRAGON-187): the installed version stays
        // visible. EVERY state carries the status caption under the version - including
        // Available, where it names the new version (the button is now a short, generic
        // action label with no version number). Available also shows the scrollable
        // markdown notes below.
        let version_row = Item::new("Version", "", self.version_row_control())
            .desc_el(self.version_status_caption());
        let mut items = vec![Item::note(hero()), version_row];
        // The changelog stays visible in the UpToDate state too (the manifest's
        // notes ARE the installed version's), so users can always read what is
        // in their version; notes_element carries its own "What's new" heading.
        if let Some(notes) = self.notes_element() {
            items.push(Item::note(notes));
        }
        items.extend(self.update_items());
        items.push(Item::new(
            "Donations",
            "Free forever. Donations help keep development going.",
            donate_button(),
        ));
        vec![SectionSpec { title: "About", items }]
    }

    /// The Version row's right-hand control. Always shows the installed version,
    /// paired with the ONE update action: the platform install/get button when an
    /// update is Available, the "Check for updates" button otherwise (no
    /// standalone check row).
    fn version_row_control(&self) -> Element<'_, Msg> {
        use crate::update::UpdateStatus;
        let version = widget::text::body(env!("CARGO_PKG_VERSION"));
        // The action slot swaps between several buttons (check / checking / install /
        // installing / get) as the state changes. Size them all to ONE shared width -
        // the widest label any of them can ever show - so the button never changes
        // size when its text swaps (DRAGON-187).
        let action_w = action_button_width();
        let action = if let UpdateStatus::Available(info) = &self.update_status {
            update_action_button(info, self.update_installing, action_w)
        } else {
            check_button(matches!(self.update_status, UpdateStatus::Checking), action_w)
        };
        widget::row(vec![version.into(), action])
            .spacing(12.0)
            .align_y(Alignment::Center)
            .into()
    }

    /// The version row's description caption: the update status as a colored caption
    /// (subdued when unchecked/checking, success when current, warning on a failed
    /// check). In the Available state it names the NEW version here (DRAGON-187), so
    /// the action button can stay a short, generic label without the version number.
    fn version_status_caption(&self) -> Element<'_, Msg> {
        use crate::update::UpdateStatus;
        match &self.update_status {
            UpdateStatus::Unknown => {
                super::super::row::subdued_caption("Update status not checked yet.")
            }
            UpdateStatus::Checking => {
                super::super::row::subdued_caption("Checking for updates...")
            }
            UpdateStatus::UpToDate { .. } => {
                super::super::row::success_caption("You have the latest version.")
            }
            UpdateStatus::Failed(reason) => super::super::row::warning_caption(reason.clone()),
            UpdateStatus::Available(info) => {
                super::super::row::success_caption(format!("Version {} is available.", info.version))
            }
        }
    }

    /// The parsed release notes: a "What's new in <version>" heading (styled like
    /// the other option titles) above the scrollable markdown block, capped at
    /// [`NOTES_MAX_HEIGHT`]. `None` when there are no notes to show. Link clicks
    /// route through the existing URL-open mechanism.
    fn notes_element(&self) -> Option<Element<'_, Msg>> {
        let (version, content) = self.update_notes.as_ref()?;
        let rendered = widget::markdown::view(content.items(), notes_markdown_settings())
            .map(|url| Msg::WindowChrome(WindowChromeMsg::OpenUrlOwned(url)));
        let heading = widget::text::body(format!("What's new in {version}"))
            .font(cosmic::font::bold());
        let block: Element<'_, Msg> = widget::container(
            // The inner container's right padding lines the wrapped markdown's
            // right edge up with the toggles' right edge on the same page
            // (DRAGON-187), and keeps it clear of the scrollbar gutter. The
            // container is width-constrained (Fill), so the markdown word-wraps
            // within it and never extends past that edge (top/left/bottom stay
            // flush). The scrollbar stays put in the outer container's gutter.
            widget::scrollable(
                widget::container(rendered)
                    .width(Length::Fill)
                    .padding(cosmic::iced::Padding::default().right(NOTES_TOGGLE_EDGE_GAP)),
            )
            .height(Length::Shrink),
        )
        .max_height(NOTES_MAX_HEIGHT)
        .width(Length::Fill)
        .padding([4.0, 0.0])
        .into();
        Some(widget::column(vec![heading.into(), block]).spacing(4.0).into())
    }

    /// The always-present notify toggle (the check button lives on the Version row).
    fn update_items(&self) -> Vec<Item<'_>> {
        vec![
            // DRAGON-177: the launch-time update-dialog toggle (no description). This is
            // the SAME setting the dialog's "Don't remind me again" checkbox drives.
            Item::new(
                "Notify me when an update is available",
                "",
                super::super::row::toggle(self.notify_updates, |on| {
                    Msg::Settings(SettingsMsg::SetNotifyUpdates(on))
                }),
            ),
        ]
    }
}

/// The Version row's action button for an available update. macOS: a suggested
/// "Install Update" (one-click install, or a plain "Update Available" label if no
/// artifact is attached). Linux: "Get Update" opening the releases page (no
/// one-click there yet, so "Install" would be a lie). The version number is NOT in
/// the button (DRAGON-187): it reads in the row's description caption instead, so
/// the button stays a short, fixed action label.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
fn update_action_button<'a>(
    info: &crate::update::UpdateInfo,
    installing: bool,
    width: f32,
) -> Element<'a, Msg> {
    #[cfg(target_os = "macos")]
    {
        if info.artifact.is_some() {
            install_button(installing, width)
        } else {
            // No macOS artifact attached to this release yet: an honest disabled label.
            widget::button::suggested(UPDATE_AVAILABLE_LABEL)
                .width(Length::Fixed(width))
                .into()
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Linux: no one-click install yet, so open the releases page. Honest label.
        widget::button::suggested(GET_UPDATE_LABEL)
            .width(Length::Fixed(width))
            .on_press(Msg::WindowChrome(WindowChromeMsg::OpenUrl(REPO_URL)))
            .into()
    }
}

/// Markdown render settings for the release notes: the widget defaults, with the
/// base text size trimmed to body size and links tinted to the theme accent so
/// they read as clickable and match the rest of the About page.
fn notes_markdown_settings() -> widget::markdown::Settings {
    let theme = cosmic::theme::active();
    // Start from the widget's own default palette-derived style, then override the
    // link colour with the app accent (the palette default's primary is not the
    // COSMIC accent). `with_text_size` scales the heading sizes off the base.
    let base = if theme.cosmic().is_dark {
        cosmic::iced::theme::Palette::DARK
    } else {
        cosmic::iced::theme::Palette::LIGHT
    };
    let mut style = widget::markdown::Style::from_palette(base);
    style.link_color = theme::accent(&theme);
    widget::markdown::Settings::with_text_size(14.0, style)
}

// Every label the Version row's action button (check / checking / install /
// installing / get / update-available) can ever display. They all SWAP in and out
// of the same slot, so they share ONE width - the widest of them all - and the
// button never changes size as its text changes (DRAGON-187): no reflow, no jump.
// None of these carry the version number (that reads in the row's description
// caption instead), so they stay short, fixed action labels.
const CHECK_LABEL: &str = "Check for updates";
const CHECKING_LABEL: &str = "Checking...";
#[cfg(target_os = "macos")]
const INSTALL_LABEL: &str = "Install Update";
#[cfg(target_os = "macos")]
const INSTALLING_LABEL: &str = "Installing...";
#[cfg(target_os = "macos")]
const UPDATE_AVAILABLE_LABEL: &str = "Update Available";
#[cfg(not(target_os = "macos"))]
const GET_UPDATE_LABEL: &str = "Get Update";

/// The shared fixed width for the Version row's action button, sized to the widest
/// label it can EVER show. Every button in that slot uses this one width so
/// swapping labels never reflows it. Pure logic (see [`fixed_button_width`]) -
/// unit-tested below. The labels are static now (no version number), so the width
/// is the same in every state.
fn action_button_width() -> f32 {
    #[cfg(target_os = "macos")]
    let labels: &[&str] = &[
        CHECK_LABEL,
        CHECKING_LABEL,
        INSTALL_LABEL,
        INSTALLING_LABEL,
        UPDATE_AVAILABLE_LABEL,
    ];
    #[cfg(not(target_os = "macos"))]
    let labels: &[&str] = &[CHECK_LABEL, CHECKING_LABEL, GET_UPDATE_LABEL];
    fixed_button_width(labels)
}

/// Estimate a fixed button width that fits the widest of `labels`. Body text is
/// ~14px; an em-agnostic ~7.5px/char average comfortably covers the button's
/// proportional font (a small over-estimate is fine - it only guarantees no
/// clipping and no reflow), plus the standard button's horizontal padding.
fn fixed_button_width(labels: &[&str]) -> f32 {
    /// Approximate advance width per character at the button's body text size.
    const CHAR_W: f32 = 7.5;
    /// The standard button's total left+right inner padding.
    const BUTTON_PAD_X: f32 = 32.0;
    let widest = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    widest as f32 * CHAR_W + BUTTON_PAD_X
}

/// The "Check for updates" button (disabled while a check is running). Fixed to the
/// shared action-slot width so it never reflows as the text swaps (DRAGON-187).
fn check_button<'a>(checking: bool, width: f32) -> Element<'a, Msg> {
    let btn = widget::button::standard(if checking { CHECKING_LABEL } else { CHECK_LABEL })
        .width(Length::Fixed(width));
    if checking {
        btn.into()
    } else {
        btn.on_press(Msg::Settings(SettingsMsg::CheckForUpdates)).into()
    }
}

/// The one-click "Install Update" button (macOS), disabled and reading
/// "Installing..." while an install is running. The version reads in the row's
/// description caption, not the button (DRAGON-187). Fixed to the shared action-slot
/// width so it never reflows as the label swaps.
#[cfg(target_os = "macos")]
fn install_button<'a>(installing: bool, width: f32) -> Element<'a, Msg> {
    if installing {
        widget::button::suggested(INSTALLING_LABEL)
            .width(Length::Fixed(width))
            .into()
    } else {
        widget::button::suggested(INSTALL_LABEL)
            .width(Length::Fixed(width))
            .on_press(Msg::Settings(SettingsMsg::InstallUpdate))
            .into()
    }
}

/// Centered header: the app icon (with the icon-credit badge tucked at its
/// corner), the app name, and the tagline.
fn hero() -> Element<'static, Msg> {
    let icon = widget::icon::icon(widget::icon::from_svg_bytes(APP_ICON))
        .width(Length::Fixed(96.0))
        .height(Length::Fixed(96.0));
    // A leading spacer mirrors the badge + gap so the LOGO is what's centered;
    // the "?" hangs off its side instead of shoving it left.
    widget::container(
        widget::column(vec![
            widget::row(vec![
                widget::Space::new().width(Length::Fixed(26.0)).into(),
                icon.into(),
                widget::Space::new().width(Length::Fixed(8.0)).into(),
                credit_badge(),
            ])
            .align_y(Alignment::End)
            .into(),
            widget::text::title3("Cosmic Capture Kit").into(),
            widget::text::caption("Desktop Screenshot & Recorder").into(),
        ])
        .spacing(8.0)
        .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .align_x(Alignment::Center)
    .padding([12.0, 0.0])
    .into()
}

/// The small "?" circle beside the icon: hover explains the credit, click opens
/// the artist's site (the Link class gives it the pointer cursor).
fn credit_badge() -> Element<'static, Msg> {
    widget::tooltip(
        widget::button::custom(
            widget::container(widget::text::caption("?"))
                .width(Length::Fixed(18.0))
                .height(Length::Fixed(18.0))
                .align_x(Alignment::Center)
                .align_y(Alignment::Center),
        )
        .class(badge_class())
        .padding(0)
        .on_press(Msg::WindowChrome(WindowChromeMsg::OpenUrl(ICON_ARTIST_URL))),
        widget::text("Icon hand crafted by Ashley Ball").size(12),
        widget::tooltip::Position::Right,
    )
    .into()
}

/// Circular badge styling: a filled subdued chip at rest, accent on hover.
fn badge_class() -> cosmic::theme::Button {
    fn style(hovered: bool, theme: &cosmic::Theme) -> cosmic::widget::button::Style {
        let cosmic = theme.cosmic();
        let mut s = cosmic::widget::button::Style::new();
        // An intrinsic circle (half the 18px badge box), like a radio button —
        // exempt from the theme rounding rule on purpose.
        s.border_radius = 9.0.into();
        s.border_width = 1.0;
        if hovered {
            s.border_color = theme::accent(theme);
            s.text_color = Some(theme::accent(theme));
        } else {
            let mut bg = theme::subdued(theme);
            bg.a = 0.25;
            s.background = Some(Background::Color(bg));
            s.border_color = theme::subdued(theme);
            s.text_color = Some(cosmic.on_bg_color().into());
        }
        s
    }
    cosmic::theme::Button::Custom {
        active: Box::new(|_focused, theme| style(false, theme)),
        hovered: Box::new(|_focused, theme| style(true, theme)),
        pressed: Box::new(|_focused, theme| style(true, theme)),
        disabled: Box::new(|theme| style(false, theme)),
    }
}


/// PayPal donation button (DRAGON-226): the accent-filled (trim-colored) suggested
/// button, opening the PayPal page. No PayPal trademark art.
fn donate_button() -> Element<'static, Msg> {
    widget::button::suggested("Donate")
        .on_press(Msg::WindowChrome(WindowChromeMsg::OpenUrl(DONATE_URL)))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_toggle_edge_gap_matches_row_slots() {
        // The note's right padding must equal the toggle's inset from the row's
        // right edge: spacing + suffix slot + spacing + reset slot (render_specs).
        assert_eq!(NOTES_TOGGLE_EDGE_GAP, 8.0 + 24.0 + 8.0 + 28.0);
    }

    #[test]
    fn check_button_sized_to_widest_label() {
        // The widest label ("Check for updates") drives the width; the narrower
        // "Checking..." never grows it, so swapping labels can't reflow the button.
        let widest = fixed_button_width(&[CHECK_LABEL]);
        let both = fixed_button_width(&[CHECK_LABEL, CHECKING_LABEL]);
        let narrow = fixed_button_width(&[CHECKING_LABEL]);
        assert_eq!(both, widest, "widest label must set the width");
        assert!(both > narrow, "the fixed width must exceed the narrow label's");
    }

    #[test]
    fn action_width_is_one_shared_max_across_all_labels() {
        // Every button in the action slot (check / checking / install / installing /
        // update-available / get) uses this ONE width, so swapping between them never
        // reflows. The labels are static (no version number now, DRAGON-187), so the
        // width is state-independent and must cover each candidate label.
        let width = action_button_width();
        let mut candidates = vec![CHECK_LABEL, CHECKING_LABEL];
        #[cfg(target_os = "macos")]
        candidates.extend([INSTALL_LABEL, INSTALLING_LABEL, UPDATE_AVAILABLE_LABEL]);
        #[cfg(not(target_os = "macos"))]
        candidates.push(GET_UPDATE_LABEL);
        for label in candidates {
            assert!(
                width >= fixed_button_width(&[label]),
                "shared width must cover {label:?}"
            );
        }
    }

    #[test]
    fn fixed_button_width_covers_the_text() {
        // A monotone sanity check: more characters never yields a smaller width,
        // and an empty set yields just the padding (never negative/NaN).
        assert!(fixed_button_width(&["ab"]) < fixed_button_width(&["abcd"]));
        assert!(fixed_button_width(&[]) >= 0.0);
    }
}
