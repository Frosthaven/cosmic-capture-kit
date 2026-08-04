//! About settings page section builder.

use super::super::*;
use super::super::row::{Item, SectionSpec};

/// The app icon, compiled in so the About page never depends on the icon being
/// installed system-wide (packaging installs the same file to hicolor).
const APP_ICON: &[u8] =
    include_bytes!("../../../../res/icons/dev.frosthaven.CosmicCaptureKit.svg");

/// The docs site's own rendered patch-notes history page. The Version row's description
/// links here ("View All Patch Notes") on every platform. This is text only, no download
/// assets; see [`GITHUB_RELEASES_URL`] for the actual GitHub releases page a download
/// comes from.
const PATCH_NOTES_URL: &str = "https://cck.thedragon.dev/releases/";
/// The GitHub releases page itself, where the downloadable assets live. Used by the Linux
/// "Get" button (no one-click install there yet, so it opens this to fetch the file by
/// hand) rather than [`PATCH_NOTES_URL`], since the docs site's page has no download links
/// of its own, only patch-notes text. Dead on macOS/Windows: those platforms one-click
/// install instead of opening this page at all.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
const GITHUB_RELEASES_URL: &str = "https://github.com/Frosthaven/cosmic-capture-kit/releases";
const ICON_ARTIST_URL: &str = "https://ashleythedesigner.com/";
/// Donations (DRAGON-226): PayPal is THE donation channel — no other sponsor
/// platforms. The same URL feeds `.github/FUNDING.yml`.
const DONATE_URL: &str = "https://paypal.me/Frosthaven";

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
        // The Version row: its NAME reads "Version <installed>" in a mixed-weight title
        // (bold label, regular version number), and its description is a "View All Patch
        // Notes" link to the releases history. The available-update version, when there is
        // one, reads in the action button's label ("Get <version>"), not here. Available
        // also shows the scrollable markdown notes below.
        // `.flush()` (DRAGON-495, first used by the Cloud Accounts page): none of these
        // three rows ever gets a reset-to-default action, so the fixed unit/reset slots
        // every other settings row reserves would just be dead space here.
        let version_row = Item::new("Version", "", self.version_row_control())
            .title_el(version_title())
            .desc_el(view_patch_notes_link())
            .flush();
        // Donations lead the section (owner request): a deliberate support ask sits right
        // below the app hero, above the version information. The update-notify toggle sits
        // right below Version (owner request), so the order is hero, donate, version,
        // notify, then the changelog. Donate shares its width with the Version row's
        // action button (owner request, see shared_button_width), so both are computed
        // from the SAME update-status read.
        use crate::update::UpdateStatus;
        let available_version =
            if let UpdateStatus::Available(info) = &self.update_status { Some(info.version.as_str()) } else { None };
        let donations = Item::new(
            "Supporting this project",
            "Like the software? Buy me a drink!",
            donate_button(shared_button_width(available_version)),
        )
        .flush();
        let mut items = vec![Item::note(hero()), donations, version_row];
        items.extend(self.update_items());
        // The changelog stays visible in the UpToDate state too (the manifest's
        // notes ARE the installed version's), so users can always read what is
        // in their version; notes_element carries its own "What's new" heading.
        if let Some(notes) = self.notes_element() {
            items.push(Item::note(notes));
        }
        let sections = vec![
            SectionSpec { title: "About This Software", items },
        ];
        // DRAGON-407 removed the Windows-only "Troubleshooting" section that named the
        // DRAGON-406 report folder and opened it. That instrument is gone; the Health page's
        // Debug row (DRAGON-419) is the one place that offers a log folder now, on every
        // platform.
        sections
    }

    /// The Version row's right-hand control: the ONE update action. When an update is
    /// Available it's the platform install/get button ("Get <version>"); otherwise it's
    /// the "Check for updates" button (no standalone check row). The installed version is
    /// no longer shown here (it reads in the row's title now), so the button stands alone,
    /// right-aligned, sharing [`shared_button_width`] with the Donate button.
    fn version_row_control(&self) -> Element<'_, Msg> {
        use crate::update::UpdateStatus;
        if let UpdateStatus::Available(info) = &self.update_status {
            let action_w = shared_button_width(Some(&info.version));
            update_action_button(info, self.update_installing, action_w)
        } else {
            let action_w = shared_button_width(None);
            check_button(matches!(self.update_status, UpdateStatus::Checking), action_w)
        }
    }

    /// The parsed release notes: a "What's new in <version>" heading (styled like
    /// the other option titles) above the markdown block. `None` when there are no
    /// notes to show. Link clicks route through the existing URL-open mechanism.
    ///
    /// Unconstrained height (DRAGON-187 originally capped this at a fixed height in its
    /// own inner scrollable, back when it sat mid-page with rows below it; now that the
    /// donation/notify/version cards all lead the page and this is the last thing on it,
    /// nothing below it to protect, and the page itself scrolls as a whole).
    fn notes_element(&self) -> Option<Element<'_, Msg>> {
        let (version, content) = self.update_notes.as_ref()?;
        let rendered = widget::markdown::view(content.items(), notes_markdown_settings())
            .map(|url| Msg::WindowChrome(WindowChromeMsg::OpenUrlOwned(url)));
        let heading = widget::text::body(format!("What's new in {version}"))
            .font(cosmic::font::bold());
        // The right padding lines the wrapped markdown's right edge up with the toggles'
        // right edge on the same page (DRAGON-187). The container is width-constrained
        // (Fill), so the markdown word-wraps within it rather than extending past that edge.
        let block: Element<'_, Msg> = widget::container(rendered)
            .width(Length::Fill)
            .padding(cosmic::iced::Padding::default().right(NOTES_TOGGLE_EDGE_GAP).top(4.0))
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
            )
            .flush(),
        ]
    }
}

/// The Version row's action button for an available update. Its label carries the NEW
/// version ("Get <version>"): macOS/Windows one-click install it (swapping to
/// "Installing..." mid-install, or a plain disabled "Update Available" if no artifact is
/// attached yet); Linux opens the releases page to download it (no one-click there yet).
/// The label is centered within the fixed action width on every platform.
// `installing` is only consumed on the one-click platforms; Linux opens the releases page.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(unused_variables))]
fn update_action_button<'a>(
    info: &crate::update::UpdateInfo,
    installing: bool,
    width: f32,
) -> Element<'a, Msg> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        if info.artifact.is_some() {
            install_button(&info.version, installing, width)
        } else {
            // No platform artifact attached to this release yet: an honest disabled label,
            // centered in the shared action width like the other states.
            crate::app::settings::row::centered_button(
                None,
                UPDATE_AVAILABLE_LABEL,
                Length::Fixed(width),
                cosmic::theme::Button::Suggested,
                None,
            )
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // Linux: no one-click install yet, so open the releases page to fetch it. The
        // version rides in the label; centered in the shared action width.
        crate::app::settings::row::centered_button(
            None,
            format!("Get {}", info.version),
            Length::Fixed(width),
            cosmic::theme::Button::Suggested,
            Some(Msg::WindowChrome(WindowChromeMsg::OpenUrl(GITHUB_RELEASES_URL))),
        )
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

// The static labels the Version row's action button can display. The check/checking pair
// swaps in place, and (macOS/Windows) install swaps to "Installing..."; each such swap
// shares ONE width so the button never reflows mid-swap (DRAGON-187). The available-update
// button now BAKES the new version into its label ("Get <version>", built at runtime,
// reversing DRAGON-187's no-version-in-label rule on purpose), so its width is sized per
// version rather than from a static constant.
const CHECK_LABEL: &str = "Check for updates";
const CHECKING_LABEL: &str = "Checking...";
#[cfg(any(target_os = "macos", target_os = "windows"))]
const INSTALLING_LABEL: &str = "Installing...";
#[cfg(any(target_os = "macos", target_os = "windows"))]
const UPDATE_AVAILABLE_LABEL: &str = "Update Available";
const DONATE_LABEL: &str = "Donate";

/// The ONE fixed width shared by every button in this section (owner request): Donate,
/// Check for updates / Checking, and (when an update IS available) Get <version> /
/// Installing / Update Available. `version` is the available update's version string when
/// there is one (folded into "Get <version>", which is why the width depends on it); pass
/// `None` when there is no update available so the slot is sized from the check-state
/// labels alone. Donate is ALWAYS a candidate, since it is always on screen regardless of
/// update state, so the shared width never shrinks smaller than what its own label needs.
/// Pure logic (see [`fixed_button_width`]), unit-tested below.
fn shared_button_width(version: Option<&str>) -> f32 {
    let get = version.map(|v| format!("Get {v}"));
    let mut labels: Vec<&str> = vec![DONATE_LABEL];
    match &get {
        Some(get) => {
            labels.push(get.as_str());
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            labels.extend([INSTALLING_LABEL, UPDATE_AVAILABLE_LABEL]);
        }
        None => labels.extend([CHECK_LABEL, CHECKING_LABEL]),
    }
    fixed_button_width(&labels)
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
    // Paint it in the shared frosted PILL MATERIAL (DRAGON-279) like the other settings
    // buttons/pills/cards, so on a frosted window (Linux glass / Windows Mica / macOS
    // vibrancy, DRAGON-268) it reads as translucent glass instead of a solid chip that
    // doesn't match. Cross-platform: the pill material is a translucent fill everywhere.
    // Centred within the fixed action width so the label sits mid-button, not
    // flush-left (DRAGON-268 follow-up). The fixed width is preserved so swapping
    // "Check for updates" <-> "Checking..." never reflows.
    crate::app::settings::row::centered_button(
        None,
        if checking { CHECKING_LABEL } else { CHECK_LABEL },
        Length::Fixed(width),
        crate::app::settings::row::standard_button_class(),
        (!checking).then_some(Msg::Settings(SettingsMsg::CheckForUpdates)),
    )
}

/// The one-click install button (macOS + Windows) for an available update: reads
/// "Get <version>" (the NEW version), disabled and reading "Installing..." while an
/// install is running. Centred within the shared action width so the label swap never
/// reflows it.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn install_button<'a>(version: &str, installing: bool, width: f32) -> Element<'a, Msg> {
    // Centred within the fixed action width (DRAGON-268 follow-up), matching the
    // Check button; the fixed width still prevents any reflow as the label swaps.
    let label = if installing {
        INSTALLING_LABEL.to_string()
    } else {
        format!("Get {version}")
    };
    crate::app::settings::row::centered_button(
        None,
        label,
        Length::Fixed(width),
        cosmic::theme::Button::Suggested,
        (!installing).then_some(Msg::Settings(SettingsMsg::InstallUpdate)),
    )
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

/// The Version row's title element: the word "Version" in bold, then a space, then the
/// CURRENTLY INSTALLED version number (`CARGO_PKG_VERSION`) in regular weight, e.g.
/// "Version 0.27.0". Inline weight-mixing is a small row of two body-size texts, the way
/// the app mixes weights inline elsewhere (`cosmic::font::bold()` on a `widget::text`); the
/// 6px spacing reads as the space between the two words.
fn version_title() -> Element<'static, Msg> {
    widget::row(vec![
        widget::text::body("Version").font(cosmic::font::bold()).into(),
        widget::text::body(env!("CARGO_PKG_VERSION")).into(),
    ])
    .spacing(6.0)
    .align_y(Alignment::Center)
    .into()
}

/// The Version row's description: a "View All Patch Notes" link to the releases history,
/// styled as an inline accent link (the `Button::Link` idiom, so it gets the pointer
/// cursor), replacing the old dynamic update-status caption.
fn view_patch_notes_link() -> Element<'static, Msg> {
    widget::button::custom(widget::text::caption("View All Patch Notes"))
        .class(cosmic::theme::Button::Link)
        .padding(0)
        .on_press(Msg::WindowChrome(WindowChromeMsg::OpenUrl(PATCH_NOTES_URL)))
        .into()
}

/// PayPal donation button (DRAGON-226): the accent-filled (trim-colored) suggested
/// button, opening the PayPal page. No PayPal trademark art. Centred within `width`,
/// [`shared_button_width`], the same fixed width the Version row's action button uses,
/// so every button in this section reads as one family (owner request).
fn donate_button(width: f32) -> Element<'static, Msg> {
    crate::app::settings::row::centered_button(
        Some("donate-symbolic"),
        DONATE_LABEL,
        Length::Fixed(width),
        cosmic::theme::Button::Suggested,
        Some(Msg::WindowChrome(WindowChromeMsg::OpenUrl(DONATE_URL))),
    )
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
    fn check_width_covers_donate_and_both_check_labels() {
        // The check-state width must cover Donate (always on screen) as well as both
        // labels the check button itself swaps between, so nothing in the section reflows.
        let width = shared_button_width(None);
        for label in [DONATE_LABEL, CHECK_LABEL, CHECKING_LABEL] {
            assert!(width >= fixed_button_width(&[label]), "shared width must cover {label:?}");
        }
    }

    #[test]
    fn available_width_covers_donate_and_get_and_swap_labels() {
        // The available-update button bakes the new version into a "Get <version>" label;
        // the shared width must cover that label, Donate, and (macOS/Windows) the
        // "Installing..." / "Update Available" states it swaps through.
        let version = "0.27.0";
        let width = shared_button_width(Some(version));
        let get = format!("Get {version}");
        let mut candidates = vec![DONATE_LABEL, get.as_str()];
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        candidates.extend([INSTALLING_LABEL, UPDATE_AVAILABLE_LABEL]);
        for label in candidates {
            assert!(width >= fixed_button_width(&[label]), "shared width must cover {label:?}");
        }
    }

    #[test]
    fn available_width_tracks_version_length() {
        // The version now rides in the label (reversing DRAGON-187 on purpose), so a
        // longer version string can only widen the button, never shrink it.
        let short = shared_button_width(Some("1.0.0"));
        let long = shared_button_width(Some("10.20.30-rc1"));
        assert!(long >= short, "a longer version must not shrink the button");
    }

    #[test]
    fn no_available_update_still_covers_donate() {
        // Even with no update in play, the shared width must be at least as wide as Donate,
        // since Donate is always on screen regardless of update state.
        let width = shared_button_width(None);
        assert!(width >= fixed_button_width(&[DONATE_LABEL]));
    }

    #[test]
    fn fixed_button_width_covers_the_text() {
        // A monotone sanity check: more characters never yields a smaller width,
        // and an empty set yields just the padding (never negative/NaN).
        assert!(fixed_button_width(&["ab"]) < fixed_button_width(&["abcd"]));
        assert!(fixed_button_width(&[]) >= 0.0);
    }
}
