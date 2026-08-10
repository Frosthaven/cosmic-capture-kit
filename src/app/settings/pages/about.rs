//! About settings page section builder.

use super::super::*;
use super::super::row::{Item, SectionSpec};

/// The app icon, compiled in so the About page never depends on the icon being
/// installed system-wide (packaging installs the same file to hicolor).
const APP_ICON: &[u8] =
    include_bytes!("../../../../res/icons/dev.thedragon.CosmicCaptureKit.svg");

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
        // (bold label, regular version number). The "View All Patch Notes" link used to be
        // this row's description; it now closes the changelog block instead (owner request:
        // read what changed, then the link to the rest is right there). The
        // available-update version, when there is one, reads in the action button's label
        // ("Get <version>"), not here. Available also shows the scrollable markdown notes
        // below.
        // `.flush()` (DRAGON-495, first used by the Cloud Accounts page): none of these
        // three rows ever gets a reset-to-default action, so the fixed unit/reset slots
        // every other settings row reserves would just be dead space here.
        let version_row = Item::new("Version", "", self.version_row_control())
            .title_el(version_title())
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
        //
        // It also stays visible on a build with NO update channel (a Flatpak,
        // DRAGON-605), where `update_items` above returned the store card instead of
        // the update controls. The notes go BELOW that card on purpose: the card says
        // where updates come from, the notes say what is in them, and the pair is more
        // useful than either alone. Nothing here is conditional, because the notes are
        // not the channel's to withhold; `update::notes_source` decides only who
        // FETCHES them, and on a Flatpak that is `SettingsMsg::FetchReleaseNotes`.
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
    ///
    /// On a build with no update channel (a Flatpak, DRAGON-561) there is no action to
    /// offer: no check button, and Available can never arrive (nothing checks). The
    /// control slot stays empty, and the store row from [`Self::update_items`] says
    /// where updates come from instead.
    fn version_row_control(&self) -> Element<'_, Msg> {
        use crate::update::UpdateStatus;
        if !crate::update::channel_available() {
            return widget::text("").into();
        }
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
        // The link CLOSES the notes (owner request): left-aligned on its own line under the
        // markdown, in the styling it has always had, so "what changed here" flows straight
        // into "everything that ever changed". It lives with the notes rather than the
        // Version row now, and shows exactly when they do.
        //
        // The blank line above the link (owner request, DRAGON-526 round): at the column's
        // own 4px the link read as one more line of the notes; a spacer one text line tall
        // separates "what changed here" from the link out. A Space rather than a bigger
        // column spacing, so the heading keeps hugging its markdown.
        let link_gap = widget::Space::new().height(Length::Fixed(16.0));
        Some(
            widget::column(vec![heading.into(), block, link_gap.into(), view_patch_notes_link()])
                .spacing(4.0)
                .into(),
        )
    }

    /// The notify toggle (the check button lives on the Version row), or, on a build
    /// with no update channel (a Flatpak, DRAGON-561), ONE inert informational row in
    /// place of both update controls: the store owns updates there, so a check button
    /// and a notify toggle would each promise something this build cannot do. The copy
    /// deliberately names no specific store (owner decision): a Flatpak cannot tell
    /// which remote installed it.
    fn update_items(&self) -> Vec<Item<'_>> {
        if !crate::update::channel_available() {
            return vec![
                Item::new(
                    "Updates come from your software store",
                    "This build is a Flatpak. New versions arrive through the store you \
                     installed it from, or by running flatpak update.",
                    widget::text(""),
                )
                .flush(),
            ];
        }
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
/// version ("Get <version>"). A build that can install itself does so in one click (swapping
/// to "Installing..." mid-install, or a plain disabled "Update Available" if no artifact is
/// attached to the release yet); one that cannot opens the releases page instead. The label
/// is centered within the fixed action width in every state.
///
/// The split is [`crate::update::one_click_install_available`], and since DRAGON-532 it is a
/// RUNTIME question rather than a `cfg`: on Linux the AppImage can replace itself and the
/// ZIP (BIN) build cannot, yet both are the same compiled binary.
fn update_action_button<'a>(
    info: &crate::update::UpdateInfo,
    installing: bool,
    width: f32,
) -> Element<'a, Msg> {
    if !crate::update::one_click_install_available() {
        // No install location we own, so the honest offer is the download page. The version
        // rides in the label; centered in the shared action width.
        return crate::app::settings::row::centered_button(
            None,
            format!("Get {}", info.version),
            Length::Fixed(width),
            cosmic::theme::Button::Suggested,
            Some(Msg::WindowChrome(WindowChromeMsg::OpenUrl(GITHUB_RELEASES_URL))),
        );
    }
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
// Ungated since DRAGON-532: whether these states can appear is a RUNTIME question, so the
// labels are compiled everywhere and `one_click_install_available` decides at draw time.
const INSTALLING_LABEL: &str = "Installing...";
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
            // Only the one-click states can appear, so only they can widen the slot. Matches
            // `update_action_button`'s own branch, which since DRAGON-532 is a runtime
            // question on Linux: an AppImage swaps its label to "Installing...", the
            // ZIP (BIN) build never does and keeps the width it always had.
            if crate::update::one_click_install_available() {
                labels.extend([INSTALLING_LABEL, UPDATE_AVAILABLE_LABEL]);
            }
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

/// The one-click install button for an available update on a build that can install itself
/// (macOS, Windows, and since DRAGON-532 a Linux AppImage): reads "Get <version>" (the NEW
/// version), disabled and reading "Installing..." while an install is running. Centred
/// within the shared action width so the label swap never reflows it.
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

/// Wrap the Cargo description "generally around where the first sentence ends"
/// (the owner's rule, DRAGON-578, third reading of the same request: not a fixed
/// pixel width, not sentence-per-line). The opening sentence is line one and
/// DEFINES the measure; the rest of the description word-wraps greedily to that
/// measure, in characters. A character count is deliberate, not a font metric:
/// the caption font is proportional, but "around" is the spec, and a pure count
/// keeps the decision testable with no font stack (the `text_annot::wrap_with`
/// precedent). Words are never split; a word longer than the measure gets its
/// own line. A description with no ". " boundary comes back as a single line.
/// Pure; unit-tested in `tagline_wrap_tests`.
fn wrap_tagline(desc: &str) -> Vec<String> {
    let Some((first, rest)) = desc.split_once(". ") else {
        return if desc.is_empty() { Vec::new() } else { vec![desc.to_string()] };
    };
    let first = format!("{first}.");
    let measure = first.chars().count();
    let mut lines = vec![first];
    let mut line = String::new();
    for word in rest.split_whitespace() {
        if line.is_empty() {
            line = word.to_string();
        } else if line.chars().count() + 1 + word.chars().count() <= measure {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(std::mem::take(&mut line));
            line = word.to_string();
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
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
            // The tagline comes from Cargo.toml rather than a literal, so the crate metadata
            // and the About page cannot drift (owner request). `Cargo.toml` is the single
            // source: change the wording there and this follows. The block wraps around
            // where the first sentence ends (owner's call, DRAGON-578): wrap_tagline
            // pre-breaks the lines, each rendered unwrapped and centered.
            {
                let lines = wrap_tagline(env!("CARGO_PKG_DESCRIPTION"));
                let mut col = widget::column::with_capacity(lines.len());
                for line in lines {
                    col = col.push(widget::text::caption(line).align_x(Alignment::Center));
                }
                col.align_x(Alignment::Center).into()
            },
            release_kind(),
        ])
        .spacing(8.0)
        .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .align_x(Alignment::Center)
    .padding([12.0, 0.0])
    .into()
}

/// How this build was delivered, as a subdued icon plus label under the tagline: "Linux Binary
/// Release", "Linux AppImage Release", "Linux Flatpak Release", "macOS Release" or "Windows
/// Release" (owner request; DRAGON-614 added the last two and put the platform on the first
/// three, which stopped the badge reading as Linux-only).
///
/// It answers a question support threads open with and users cannot otherwise answer: WHICH
/// build is this. The Linux three behave differently in ways that matter the moment something
/// is wrong, since they disagree about where the app's files live, whether it can update
/// itself, and which copy of ffmpeg or tesseract is really being spawned.
///
/// The wording and the debug log's `package:` line come from ONE source
/// ([`crate::util::PackageKind`]), so a screenshot of this row and a log file cannot contradict
/// each other. They are deliberately not the same STRING: the log says what the thing is
/// ("plain binary"), this names a release channel.
///
/// Subdued and small on purpose. It is provenance, not a headline, and it sits below the
/// tagline so it reads as a footnote to the identity above it rather than as a feature.
fn release_kind() -> Element<'static, Msg> {
    let kind = crate::util::package_kind();
    widget::row(vec![
        crate::widgets::icons::sized(kind.icon_name(), 14.0)
            .class(cosmic::theme::Svg::Custom(std::rc::Rc::new(|t: &cosmic::Theme| {
                cosmic::widget::svg::Style { color: Some(theme::subtle(t)) }
            })))
            .into(),
        widget::text::caption(kind.label())
            .class(cosmic::theme::Text::Custom(|t| cosmic::iced::widget::text::Style {
                color: Some(theme::subtle(t)),
                ..Default::default()
            }))
            .into(),
    ])
    .spacing(6.0)
    .align_y(Alignment::Center)
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

// The tagline wrap: the first sentence is line one and sets the measure; the rest of the
// description wraps to it (DRAGON-578).
#[cfg(test)]
mod tagline_wrap_tests {
    use super::wrap_tagline;

    #[test]
    fn the_first_sentence_is_line_one_and_nothing_runs_wider() {
        let lines = wrap_tagline(env!("CARGO_PKG_DESCRIPTION"));
        assert!(
            lines[0].starts_with("Quickly capture") && lines[0].ends_with('.'),
            "line one is the whole opening sentence: {}",
            lines[0]
        );
        assert!(lines.len() > 2, "the remainder wraps into more than one line: {lines:?}");
        let measure = lines[0].chars().count();
        for line in &lines[1..] {
            assert!(
                line.chars().count() <= measure,
                "wrapped line exceeds the first-sentence measure: {line}"
            );
        }
    }

    #[test]
    fn no_content_is_lost_or_reordered() {
        assert_eq!(wrap_tagline(env!("CARGO_PKG_DESCRIPTION")).join(" "), env!("CARGO_PKG_DESCRIPTION"));
    }

    #[test]
    fn the_remainder_wraps_greedily_to_the_measure() {
        assert_eq!(wrap_tagline("AAAA. bb cc dd"), vec!["AAAA.", "bb cc", "dd"]);
    }

    #[test]
    fn a_word_longer_than_the_measure_gets_its_own_line() {
        assert_eq!(
            wrap_tagline("AA. supercalifragilistic word"),
            vec!["AA.", "supercalifragilistic", "word"]
        );
    }

    #[test]
    fn a_single_sentence_description_degrades_to_one_line() {
        assert_eq!(wrap_tagline("Just one line"), vec!["Just one line"]);
        assert_eq!(wrap_tagline("Ends with a period."), vec!["Ends with a period."]);
    }

    #[test]
    fn an_empty_description_renders_nothing() {
        assert!(wrap_tagline("").is_empty());
    }
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
        // the shared width must cover that label, Donate, and, on a build that installs
        // itself, the "Installing..." / "Update Available" states it swaps through.
        let version = "0.27.0";
        let width = shared_button_width(Some(version));
        let get = format!("Get {version}");
        let mut candidates = vec![DONATE_LABEL, get.as_str()];
        // The SAME runtime condition `shared_button_width` uses, not a `cfg`: since
        // DRAGON-532 a Linux AppImage has the swap states and the ZIP (BIN) build does not,
        // and asking the question the same way is what keeps the two from drifting.
        if crate::update::one_click_install_available() {
            candidates.extend([INSTALLING_LABEL, UPDATE_AVAILABLE_LABEL]);
        }
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
