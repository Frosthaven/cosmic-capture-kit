//! General settings page section builder.

use super::super::*;
use super::super::row::{num_input, opacity_slider, toggle, Item, SectionSpec};

impl crate::app::App {
    /// Every General-page section (both in-page tabs concatenated). The single
    /// source for the global settings SEARCH, which scans every page's items
    /// regardless of the active in-page tab — so a hit on an Appearance-tab item
    /// stays reachable in the results (DRAGON-138). The normal page view instead
    /// renders one tab's subset via `general_settings_sections` /
    /// `general_appearance_sections`.
    pub(in crate::app::settings) fn general_sections(&self) -> Vec<SectionSpec<'_>> {
        let mut secs = self.general_settings_sections();
        secs.extend(self.general_appearance_sections());
        secs
    }

    /// The "Settings" in-page tab (DRAGON-138): everything on the General page
    /// except the appearance (overlay-opacity) group — Behavior, Capture Preview,
    /// After a Capture.
    pub(in crate::app::settings) fn general_settings_sections(&self) -> Vec<SectionSpec<'_>> {
        let d = crate::state::defaults();
        vec![
            SectionSpec {
                title: "Behavior",
                items: {
                    #[cfg_attr(
                        not(any(target_os = "macos", target_os = "linux")),
                        allow(unused_mut)
                    )]
                    let mut items = vec![
                        Item::new(
                            "Allow multiple capture instances",
                            "Capture yourself capturing yourself!",
                            toggle(self.allow_multiple, |a0| Msg::Settings(SettingsMsg::SetAllowMultiple(a0))),
                        )
                        .reset_with(self.allow_multiple, d.allow_multiple, |a0| Msg::Settings(SettingsMsg::SetAllowMultiple(a0))),
                    ];
                    // Stay resident: keep the tray/menu-bar RESIDENT process alive so a
                    // capture is always one click away. macOS (DRAGON-130) is a menu-bar
                    // daemon with a global hotkey; Linux (DRAGON-173) is a ksni tray
                    // resident (PrintScreen stays a COSMIC custom shortcut, so the resident
                    // adds the always-available tray launchers + recording controls, not a
                    // global hotkey). One portable `resident` setting drives both; the row
                    // is gated to the two OSes that have a resident.
                    #[cfg(any(target_os = "macos", target_os = "linux"))]
                    {
                        #[cfg(target_os = "macos")]
                        let desc = "Cosmic Capture Kit will remain in the background, enabling \
                                    global hotkey use and faster launch.";
                        #[cfg(target_os = "linux")]
                        let desc = "Cosmic Capture Kit will keep a tray icon running in the \
                                    background for quick capture and recording controls.";
                        items.push(
                            Item::new(
                                "Keep running in the background",
                                desc,
                                toggle(self.resident, |a0| Msg::Settings(SettingsMsg::SetResident(a0))),
                            )
                            .reset_with(self.resident, d.resident, |a0| Msg::Settings(SettingsMsg::SetResident(a0))),
                        );
                    }
                    items
                },
            },
            SectionSpec {
                title: "Capture Preview",
                items: {
                    let mut items = vec![
                        Item::new(
                            "Preview editor appearance mode",
                            "",
                            widget::dropdown(
                                &PREVIEW_APPEARANCES,
                                Some(usize::from(self.preview_windowed)),
                                |i| Msg::Settings(SettingsMsg::SetPreviewWindowed(i == 1)),
                            ),
                        )
                        .reset_with(
                            usize::from(self.preview_windowed),
                            usize::from(d.preview_windowed),
                            |i| Msg::Settings(SettingsMsg::SetPreviewWindowed(i == 1)),
                        ),
                    ];
                    // COSMIC only, and only when the windowed appearance is chosen: let
                    // the preview window float instead of auto-tiling (registers a COSMIC
                    // tiling exception scoped to the preview window's title). The COSMIC
                    // check lives in the Linux-only COSMIC profile now (DRAGON-220); off
                    // Linux it was always false (the row never appeared), so a cfg-selected
                    // `false` keeps this byte-identical while the branch stays compiled.
                    #[cfg(target_os = "linux")]
                    let is_cosmic = crate::platform::linux::cosmic::is_cosmic();
                    #[cfg(not(target_os = "linux"))]
                    let is_cosmic = false;
                    if self.preview_windowed && is_cosmic {
                        items.push(
                            Item::new(
                                "Float the preview window (don't tile)",
                                "Register a COSMIC tiling exception so the preview window \
                                 opens floating instead of being tiled.",
                                toggle(self.preview_float_cosmic, |a0| Msg::Settings(SettingsMsg::SetPreviewFloatCosmic(a0))),
                            )
                            .reset_with(
                                self.preview_float_cosmic,
                                d.preview_float_cosmic,
                                |a0| Msg::Settings(SettingsMsg::SetPreviewFloatCosmic(a0)),
                            ),
                        );
                    }
                    items.push(
                        Item::new(
                            "Automatically close the preview editor on save or copy",
                            "",
                            toggle(self.auto_close_preview, |a0| Msg::Settings(SettingsMsg::SetAutoClosePreview(a0))),
                        )
                        .reset_with(
                            self.auto_close_preview,
                            d.auto_close_preview,
                            |a0| Msg::Settings(SettingsMsg::SetAutoClosePreview(a0)),
                        ),
                    );
                    items
                },
            },
            SectionSpec {
                title: "After a Capture",
                items: {
                    let mut items = vec![
                        Item::new(
                            "Automatically copy to clipboard",
                            "",
                            toggle(self.copy_to_clipboard, |a0| Msg::Settings(SettingsMsg::SetCopyToClipboard(a0))),
                        )
                        .reset_with(
                            self.copy_to_clipboard,
                            d.copy_to_clipboard,
                            |a0| Msg::Settings(SettingsMsg::SetCopyToClipboard(a0)),
                        ),
                    ];
                    // Only relevant when copy-to-clipboard is on. Sits with the
                    // copy toggle, above the preview-editor option.
                    if self.copy_to_clipboard {
                        items.push(
                            Item::new(
                                "Clipboard size limit",
                                "Anything under this size will get copied to the clipboard. \
                                 Great for sharing!",
                                num_input(
                                    "10",
                                    &self.clipboard_max_mb.text,
                                    Some(|a0| Msg::Settings(SettingsMsg::SetClipboardMaxMb(a0))),
                                ),
                            )
                            .suffix("MB")
                            .reset_with(
                                self.clipboard_max_mb.text.clone(),
                                d.clipboard_max_mb.to_string(),
                                |a0| Msg::Settings(SettingsMsg::SetClipboardMaxMb(a0)),
                            ),
                        );
                    }
                    items.push(
                        Item::new(
                            "Open in preview editor",
                            "Enables extra post-editing of images and video content.",
                            toggle(self.preview_after_capture, |a0| Msg::Settings(SettingsMsg::SetPreviewAfterCapture(a0))),
                        )
                        .reset_with(
                            self.preview_after_capture,
                            d.preview_after_capture,
                            |a0| Msg::Settings(SettingsMsg::SetPreviewAfterCapture(a0)),
                        ),
                    );
                    items
                },
            },
        ]
    }

    /// The "Appearance" in-page tab (DRAGON-138): the theme overrides (DRAGON-139)
    /// followed by the overlay-opacity group.
    pub(in crate::app::settings) fn general_appearance_sections(&self) -> Vec<SectionSpec<'_>> {
        let d = crate::state::defaults();
        let mut secs = Vec::new();

        // ── Theme overrides (DRAGON-139) ─────────────────────────────────────
        // The "Use System Settings" toggle at the top; when OFF, the Mode / Accent
        // Color / Style override rows appear below it in the SAME group (and apply
        // live). All four are titled rows, so each is searchable and carries the
        // standard reset slot.
        let use_system = self.appearance_use_system;
        secs.push(SectionSpec {
            title: "Theme",
            items: {
                let mut items = vec![
                    Item::new(
                        "System Default",
                        "Disable to customize the theme.",
                        toggle(use_system, |a0| Msg::Settings(SettingsMsg::SetUseSystemAppearance(a0))),
                    )
                    .reset_with(use_system, d.appearance_use_system, |a0| {
                        Msg::Settings(SettingsMsg::SetUseSystemAppearance(a0))
                    }),
                ];
                if !use_system {
                    // Mode: Automatic / Dark / Light — the base the overrides compose on.
                    // `&[..]` is const-promoted, so the dropdown's borrow is 'static.
                    items.push(
                        Item::new(
                            "Light/dark mode",
                            "Automatic follows the system's light or dark preference.",
                            widget::dropdown(
                                &["Automatic", "Dark", "Light"],
                                Some(self.appearance_mode.min(2) as usize),
                                |i| Msg::Settings(SettingsMsg::SetAppearanceMode(i as u8)),
                            ),
                        )
                        .reset_with(self.appearance_mode, d.appearance_mode, |a0| {
                            Msg::Settings(SettingsMsg::SetAppearanceMode(a0))
                        }),
                    );
                    // Accent Color: the active theme's 9 accent swatches (read live),
                    // plus a custom-color swatch / "+" opening the picker sidebar.
                    items.push(
                        Item::new("Accent", "", self.accent_swatches())
                            .reset_with(self.appearance_accent, d.appearance_accent, |a0| {
                                Msg::Settings(SettingsMsg::SetAppearanceAccent(a0))
                            }),
                    );
                    // Style: three corner-rounding previews.
                    items.push(
                        Item::new("Edge rounding", "", style_previews(self.appearance_roundness))
                            .reset_with(self.appearance_roundness, d.appearance_roundness, |a0| {
                                Msg::Settings(SettingsMsg::SetAppearanceRoundness(a0))
                            }),
                    );
                }
                // DRAGON-209: region selection box thickness — ALWAYS visible (NOT gated by
                // the System Default theme toggle above), placed just under Edge rounding.
                items.push(
                    Item::new(
                        "Selection box thickness",
                        "",
                        box_thickness_slider(self.selection_box_thickness),
                    )
                    .reset_with(self.selection_box_thickness, d.selection_box_thickness, |a0| {
                        Msg::Settings(SettingsMsg::SetSelectionBoxThickness(a0))
                    }),
                );
                items
            },
        });

        secs.push(SectionSpec {
            title: "Overlay Opacity",
            items: vec![
                Item::new(
                    "During Region Selection",
                    "",
                    opacity_slider(self.region_overlay_opacity, |a0| Msg::Settings(SettingsMsg::SetRegionOpacity(a0))),
                )
                .reset_with(
                    self.region_overlay_opacity,
                    d.region_overlay_opacity,
                    |a0| Msg::Settings(SettingsMsg::SetRegionOpacity(a0)),
                ),
                Item::new(
                    "During Countdown & Recording",
                    "",
                    opacity_slider(self.active_overlay_opacity, |a0| Msg::Settings(SettingsMsg::SetActiveOpacity(a0))),
                )
                .reset_with(
                    self.active_overlay_opacity,
                    d.active_overlay_opacity,
                    |a0| Msg::Settings(SettingsMsg::SetActiveOpacity(a0)),
                ),
                Item::new(
                    "During Preview",
                    "",
                    opacity_slider(self.preview_overlay_opacity, |a0| Msg::Settings(SettingsMsg::SetPreviewOpacity(a0))),
                )
                .reset_with(
                    self.preview_overlay_opacity,
                    d.preview_overlay_opacity,
                    |a0| Msg::Settings(SettingsMsg::SetPreviewOpacity(a0)),
                ),
            ],
        });
        secs
    }

    /// The Accent Color row content (DRAGON-139): the active theme's 9 accent
    /// swatches (read live from the palette), plus a custom-colour swatch (when the
    /// override is a non-palette colour) or a "+" button — both open the picker
    /// sidebar. Rendered as one full-width `Item::note`.
    fn accent_swatches(&self) -> Element<'_, Msg> {
        let active = cosmic::theme::active();
        let pal = &active.cosmic().palette;
        let palette: [cosmic::iced::Color; 9] = [
            srgba_to_color(pal.accent_blue),
            srgba_to_color(pal.accent_indigo),
            srgba_to_color(pal.accent_purple),
            srgba_to_color(pal.accent_pink),
            srgba_to_color(pal.accent_red),
            srgba_to_color(pal.accent_orange),
            srgba_to_color(pal.accent_yellow),
            srgba_to_color(pal.accent_green),
            srgba_to_color(pal.accent_warm_grey),
        ];
        let current = self.appearance_accent;
        let mut row: Vec<Element<'_, Msg>> = palette
            .iter()
            .map(|&c| {
                let rgb = [c.r, c.g, c.b];
                let selected = current.is_some_and(|cur| approx_rgb(cur, rgb));
                accent_swatch(c, selected, Msg::Settings(SettingsMsg::SetAppearanceAccent(Some(rgb))))
            })
            .collect();
        // The custom entry: a filled swatch when the override is a non-palette colour
        // (selected), otherwise the "+" opener.
        let custom_is_selected = current.is_some_and(|cur| !palette.iter().any(|c| approx_rgb(cur, [c.r, c.g, c.b])));
        row.push(if let Some([r, g, b]) = current.filter(|_| custom_is_selected) {
            accent_swatch(
                cosmic::iced::Color::from_rgb(r, g, b),
                true,
                Msg::Settings(SettingsMsg::ToggleAccentEditor(true)),
            )
        } else {
            widget::button::custom(
                widget::container(
                    widget::icon::from_name("list-add-symbolic").icon().size(16),
                )
                .center_x(Length::Fill)
                .center_y(Length::Fill),
            )
            .width(Length::Fixed(SWATCH))
            .height(Length::Fixed(SWATCH))
            .padding(0)
            .class(cosmic::theme::Button::Standard)
            .on_press(Msg::Settings(SettingsMsg::ToggleAccentEditor(true)))
            .into()
        });
        // A plain row, NOT `flex_row`: as a settings-row CONTROL the element sits in
        // a Shrink context, where FlexRow's taffy layout measures to nothing and the
        // swatches vanish entirely (DRAGON-152) — the Style previews right below
        // render through `widget::row` and were never affected.
        widget::row(row).spacing(12.0).align_y(Alignment::Center).into()
    }
}

/// Swatch edge length (logical px) for the accent palette + custom entry.
const SWATCH: f32 = 40.0;

/// The "Preview editor appearance mode" dropdown options (index 0 = overlay, 1 = windowed
/// — matches `preview_windowed` as a bool).
const PREVIEW_APPEARANCES: [&str; 2] = ["Overlay", "Windowed"];

/// A palette `Srgba` (0..1 components) as an opaque iced `Color` — built by
/// component so it never depends on a `From<Srgba>` impl.
fn srgba_to_color(c: cosmic::cosmic_theme::palette::Srgba) -> cosmic::iced::Color {
    cosmic::iced::Color::from_rgb(c.red, c.green, c.blue)
}

/// Whether two sRGB triples are the same colour within ~1/255 (float-equality is
/// unsafe across the Srgba→Color→persist round trip).
fn approx_rgb(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.004)
}

/// One accent swatch button: a fixed square filled with `color`, with an accent
/// ring when `selected`. `msg` fires on press.
fn accent_swatch<'a>(color: cosmic::iced::Color, selected: bool, msg: Msg) -> Element<'a, Msg> {
    widget::button::custom(widget::space::Space::new().width(Length::Fill).height(Length::Fill))
        .width(Length::Fixed(SWATCH))
        .height(Length::Fixed(SWATCH))
        .padding(0)
        .class(swatch_class(color, selected))
        .on_press(msg)
        .into()
}

fn swatch_style(
    color: cosmic::iced::Color,
    selected: bool,
    theme: &cosmic::Theme,
) -> cosmic::widget::button::Style {
    let cosmic = theme.cosmic();
    let mut s = cosmic::widget::button::Style::new();
    s.background = Some(Background::Color(color));
    s.border_radius = theme::rounding(theme).xs.into();
    s.border_width = 1.0;
    s.border_color = cosmic.palette.neutral_8.into();
    if selected {
        // A 2px accent outline OUTSIDE the swatch reads as selection without
        // recolouring the swatch itself.
        s.outline_width = 2.0;
        s.outline_color = theme::accent(theme);
    }
    s
}

fn swatch_class(color: cosmic::iced::Color, selected: bool) -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(move |_f, t| swatch_style(color, selected, t)),
        hovered: Box::new(move |_f, t| swatch_style(color, selected, t)),
        pressed: Box::new(move |_f, t| swatch_style(color, selected, t)),
        disabled: Box::new(move |t| swatch_style(color, selected, t)),
    }
}

/// The region selection box thickness row (DRAGON-209): a 1-8px slider with its px
/// readout. Mirrors the window border-width slider's shape.
fn box_thickness_slider<'a>(value: u32) -> Element<'a, Msg> {
    widget::row(vec![
        widget::slider(1..=8, value, |w| Msg::Settings(SettingsMsg::SetSelectionBoxThickness(w)))
            .step(1u32)
            .width(Length::Fixed(160.0))
            .into(),
        widget::container(widget::text(format!("{value}px")).size(13))
            .width(Length::Fixed(36.0))
            .align_x(Alignment::End)
            .into(),
    ])
    .spacing(8.0)
    .align_y(Alignment::Center)
    .into()
}

/// The Style row content (DRAGON-139): three corner-rounding previews (Round /
/// Slightly Round / Square), each a small box that demonstrates the rounding, with
/// a label below and an accent ring when selected.
fn style_previews<'a>(current: u8) -> Element<'a, Msg> {
    let one = |label: &'a str, value: u8| -> Element<'a, Msg> {
        let radius = cosmic::cosmic_theme::CornerRadii::from(theme::roundness_from_u8(value)).radius_m;
        let btn = widget::button::custom(
            widget::space::Space::new().width(Length::Fill).height(Length::Fill),
        )
        .width(Length::Fixed(80.0))
        .height(Length::Fixed(52.0))
        .padding(0)
        .class(style_preview_class(radius, current == value))
        .on_press(Msg::Settings(SettingsMsg::SetAppearanceRoundness(value)));
        widget::column(vec![btn.into(), widget::text::caption(label).into()])
            .spacing(6.0)
            .align_x(Alignment::Center)
            .into()
    };
    widget::row(vec![
        one("Round", 0),
        one("Slightly Round", 1),
        one("Square", 2),
    ])
    .spacing(16.0)
    .align_y(Alignment::Center)
    .into()
}

fn style_preview_style(
    radius: [f32; 4],
    selected: bool,
    theme: &cosmic::Theme,
) -> cosmic::widget::button::Style {
    let cosmic = theme.cosmic();
    let mut s = cosmic::widget::button::Style::new();
    s.background = Some(Background::Color(cosmic.background.component.base.into()));
    s.border_radius = radius.into();
    if selected {
        s.border_width = 2.0;
        s.border_color = theme::accent(theme);
    } else {
        s.border_width = 1.0;
        s.border_color = cosmic.palette.neutral_8.into();
    }
    s
}

fn style_preview_class(radius: [f32; 4], selected: bool) -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(move |_f, t| style_preview_style(radius, selected, t)),
        hovered: Box::new(move |_f, t| style_preview_style(radius, selected, t)),
        pressed: Box::new(move |_f, t| style_preview_style(radius, selected, t)),
        disabled: Box::new(move |t| style_preview_style(radius, selected, t)),
    }
}
