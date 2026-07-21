//! Screenshots settings page section builder.

use super::super::*;
use super::super::row::{folder_btn, num_input, toggle, Item, SectionSpec};
use super::super::deps::DepId;
use super::capture::MethodPicker;

impl crate::app::App {
    pub(in crate::app::settings) fn screenshots_sections(&self) -> Vec<SectionSpec<'_>> {
        let d = crate::state::defaults();
        let mut secs = vec![SectionSpec {
            title: "Location",
            items: vec![Item::new(
                "Save screenshots to",
                "",
                widget::row(vec![
                    crate::widgets::hide_when_clipped(
                        widget::text_input("~/Capture", &self.screenshot_dir)
                            .on_input(|a0| Msg::Settings(SettingsMsg::SetScreenshotDir(a0)))
                            .width(Length::Fixed(280.0)),
                    ),
                    folder_btn(DirTarget::Screenshot),
                ])
                .spacing(6.0)
                .align_y(Alignment::Center),
            )
            .reset_with(
                self.screenshot_dir.clone(),
                d.screenshot_dir.clone(),
                |a0| Msg::Settings(SettingsMsg::SetScreenshotDir(a0)),
            )],
        }];
        // Surface the screenshot-availability note only when there's a problem; the Health
        // page lists it regardless.
        if let Some(note) = self.dep(DepId::Screenshot).note_if_issue() {
            secs.insert(0, SectionSpec { title: "Availability", items: vec![note] });
        }
        // The capture-extra toggles, offered per the ACTIVE backend's capability
        // set (DRAGON-186): each row renders only when that backend can honor the
        // extra, so a backend supporting none of them (the portal) simply shows
        // none — no note, no disabled rows. They sit inside the Capture section
        // under the method dropdown.
        let extras = self.active_screenshot_caps().capture_extras();
        let mut contents = Vec::new();
        if extras.freeze {
            // Freeze captures the launch-instant pixels (a live portal frame
            // can't be "frozen").
            contents.push(
                Item::new(
                    "Freeze pixels during selection",
                    "Great for capturing images in motion and OCR content.",
                    toggle(self.freeze, |a0| Msg::Settings(SettingsMsg::SetFreeze(a0))),
                )
                .reset_with(self.freeze, d.freeze, |a0| Msg::Settings(SettingsMsg::SetFreeze(a0))),
            );
        }
        if extras.cursor {
            contents.push(
                Item::new(
                    "Preserve mouse cursor",
                    "",
                    toggle(self.capture_cursor, |a0| Msg::Settings(SettingsMsg::SetCaptureCursor(a0))),
                )
                .reset_with(self.capture_cursor, d.capture_cursor, |a0| Msg::Settings(SettingsMsg::SetCaptureCursor(a0))),
            );
        }
        if extras.transparency {
            // Preserve window transparency applies to all three modes now (window always; region /
            // monitor when the wallpaper is off), so it sits with the shared capture options.
            contents.push(
                Item::new(
                    "Preserve window transparency",
                    "",
                    toggle(self.capture_transparency, |a0| Msg::Settings(SettingsMsg::SetCaptureTransparency(a0))),
                )
                .reset_with(self.capture_transparency, d.capture_transparency, |a0| Msg::Settings(SettingsMsg::SetCaptureTransparency(a0))),
            );
        }
        if extras.wallpaper {
            contents.push(
                Item::new(
                    "Preserve wallpaper",
                    "",
                    toggle(self.capture_wallpaper, |a0| Msg::Settings(SettingsMsg::SetCaptureWallpaper(a0))),
                )
                .reset_with(self.capture_wallpaper, !d.no_wallpaper, |a0| Msg::Settings(SettingsMsg::SetCaptureWallpaper(a0))),
            );
        }
        secs.push(self.capture_section(
            self.dep(DepId::Screenshot).is_present(),
            MethodPicker {
                methods: &self.screenshot_methods,
                selected: &self.screenshot_backend,
                default_id: d.screenshot_backend.clone(),
                setter: |a0| Msg::Settings(SettingsMsg::SetScreenshotBackend(a0)),
            },
            "Screenshots",
            contents,
        ));
        // Window-specific decoration options — the native compose path only (the
        // portal returns a finished frame, so none of these apply there).
        // DRAGON-186 Phase 2: keyed on the active backend's capability set rather
        // than `!screenshot_uses_portal()`. `extras.freeze` is the compositor-vs-
        // portal discriminator: it equals `screenshot` on the screencopy backend
        // (so on Linux this is byte-identical to the old `!screenshot_uses_portal()`
        // in every session shape) and false on the portal, and it is true on the
        // macOS SCK backend, where these decoration options really do apply but the
        // portal boolean is spuriously true (no Wayland screencopy).
        if extras.freeze {
            // "Window focus appearance" (DRAGON-191): how a SINGLE-window capture is
            // portrayed — Active (the Active border) or Inactive (the Inactive border).
            // Region/monitor composites ignore this and pick per-window by real focus.
            // The old "Inactive with shadow" and "Raw" entries are gone: shadow is now a
            // separate toggle, and "Raw" (no border) is covered by setting a width to 0.
            let focus_idx = if self.window_single_active { 0usize } else { 1 };
            let def_focus = if d.window_single_active { 0usize } else { 1 };
            let mut win_items = vec![
                Item::new(
                    "Window focus appearance",
                    "",
                    crate::widgets::arrow_cursor::arrow_cursor(widget::dropdown(
                        &WINDOW_FOCUS_APPEARANCES,
                        Some(focus_idx),
                        |a0| Msg::Settings(SettingsMsg::SetWindowFocusAppearance(a0)),
                    )),
                )
                .reset_with(focus_idx, def_focus, |a0| Msg::Settings(SettingsMsg::SetWindowFocusAppearance(a0))),
                // Active border: colour swatch (follows the accent when unpinned) + width
                // slider. The row reset restores the WHOLE border to default (colour back
                // to Follow-accent AND width back to default), enabled whenever either the
                // colour or the width differs; the swatch's own picker still resets colour
                // alone.
                Item::new(
                    "Active border",
                    "",
                    self.active_border_control(),
                )
                .reset_to(
                    Msg::Settings(SettingsMsg::ResetActiveBorder),
                    self.active_border_color != d.active_border_color
                        || self.active_border_width != d.active_border_width,
                ),
                // Inactive border: colour swatch + width slider. Row reset restores the
                // whole border (colour + width) to default.
                Item::new(
                    "Inactive border",
                    "",
                    self.inactive_border_control(),
                )
                .reset_to(
                    Msg::Settings(SettingsMsg::ResetInactiveBorder),
                    self.inactive_border_color != d.inactive_border_color
                        || self.inactive_border_width != d.inactive_border_width,
                ),
                Item::new(
                    "Drop shadow",
                    "",
                    toggle(self.window_drop_shadow, |a0| Msg::Settings(SettingsMsg::SetWindowDropShadow(a0))),
                )
                .reset_with(self.window_drop_shadow, d.window_drop_shadow, |a0| Msg::Settings(SettingsMsg::SetWindowDropShadow(a0))),
                Item::new(
                    "Add padding around the window",
                    "",
                    toggle(self.window_padding, |a0| Msg::Settings(SettingsMsg::SetWindowPadding(a0))),
                )
                .reset_with(self.window_padding, d.window_padding, |a0| Msg::Settings(SettingsMsg::SetWindowPadding(a0))),
            ];
            // Padding amount reveal (nested under its toggle).
            if self.window_padding {
                win_items.push(
                    Item::new(
                        "Padding",
                        "",
                        num_input(
                            "32",
                            &self.window_padding_px.text,
                            Some(|a0| Msg::Settings(SettingsMsg::SetWindowPaddingPx(a0))),
                        ),
                    )
                    .suffix("px")
                    .reset_with(
                        self.window_padding_px.text.clone(),
                        d.window_padding_px.to_string(),
                        |a0| Msg::Settings(SettingsMsg::SetWindowPaddingPx(a0)),
                    ),
                );
            }
            secs.push(SectionSpec {
                title: "Single Window Aesthetics",
                items: win_items,
            });
        }
        // Covermarks: the custom-text covermark's content (used by the preview
        // overlay's "Custom text" choice), plus a hint for where to drop more SVGs.
        secs.push(SectionSpec {
            title: "Covermarks",
            items: vec![
                Item::new(
                    "Custom overlayed text",
                    format!("Covermark SVG loaded from:\n{}", covermark_dir_display()),
                    crate::widgets::hide_when_clipped(
                        widget::text_input("CONFIGURE TEXT IN SETTINGS", &self.covermark_text)
                            .on_input(|a0| Msg::Settings(SettingsMsg::SetCovermarkText(a0)))
                            .width(Length::Fixed(280.0)),
                    ),
                )
                .reset_with(
                    self.covermark_text.clone(),
                    d.covermark_text.clone(),
                    |a0| Msg::Settings(SettingsMsg::SetCovermarkText(a0)),
                ),
            ],
        });
        secs
    }

    /// The Active-border row control (DRAGON-191): a colour swatch (showing the resolved
    /// colour — the accent when unpinned) that opens the picker, plus a 0-10px width
    /// slider.
    fn active_border_control(&self) -> Element<'_, Msg> {
        // The resolved colour: the pinned custom colour, else the live accent.
        let color = self
            .active_border_color
            .unwrap_or_else(crate::decoration::accent_rgba);
        widget::row(vec![
            border_swatch(color, crate::app::BorderColorTarget::Active),
            border_width_slider(self.active_border_width, |w| {
                Msg::Settings(SettingsMsg::SetActiveBorderWidth(w))
            }),
        ])
        .spacing(12.0)
        .align_y(Alignment::Center)
        .into()
    }

    /// The Inactive-border row control (DRAGON-191): a colour swatch + a 0-10px width
    /// slider.
    fn inactive_border_control(&self) -> Element<'_, Msg> {
        widget::row(vec![
            border_swatch(self.inactive_border_color, crate::app::BorderColorTarget::Inactive),
            border_width_slider(self.inactive_border_width, |w| {
                Msg::Settings(SettingsMsg::SetInactiveBorderWidth(w))
            }),
        ])
        .spacing(12.0)
        .align_y(Alignment::Center)
        .into()
    }
}

/// Swatch edge length (logical px) for the border colour swatches.
const BORDER_SWATCH: f32 = 32.0;

/// A colour swatch button for a window-capture border: a fixed square filled with
/// `color` that opens the border colour-picker sidebar for `target` on press.
fn border_swatch<'a>(color: [u8; 4], target: crate::app::BorderColorTarget) -> Element<'a, Msg> {
    let c = cosmic::iced::Color::from_rgb(
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
    );
    crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::custom(widget::space::Space::new().width(Length::Fill).height(Length::Fill))
            .width(Length::Fixed(BORDER_SWATCH))
            .height(Length::Fixed(BORDER_SWATCH))
            .padding(0)
            .class(border_swatch_class(c))
            .on_press(Msg::Settings(SettingsMsg::ToggleBorderColorEditor(target, true))),
    )
}

fn border_swatch_style(color: cosmic::iced::Color, theme: &cosmic::Theme) -> cosmic::widget::button::Style {
    let cosmic = theme.cosmic();
    let mut s = cosmic::widget::button::Style::new();
    s.background = Some(Background::Color(color));
    s.border_radius = theme::rounding(theme).xs.into();
    s.border_width = 1.0;
    s.border_color = cosmic.palette.neutral_8.into();
    s
}

fn border_swatch_class(color: cosmic::iced::Color) -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(move |_f, t| border_swatch_style(color, t)),
        hovered: Box::new(move |_f, t| border_swatch_style(color, t)),
        pressed: Box::new(move |_f, t| border_swatch_style(color, t)),
        disabled: Box::new(move |t| border_swatch_style(color, t)),
    }
}

/// A 0-10px border-width slider with its px readout, as a row control.
fn border_width_slider<'a>(value: u32, msg: fn(u32) -> Msg) -> Element<'a, Msg> {
    widget::row(vec![
        widget::slider(0..=10, value, msg)
            .step(1u32)
            .width(Length::Fixed(160.0))
            .into(),
        // Fixed-width readout so the slider never shifts as the number's width changes.
        widget::container(widget::text(format!("{value}px")).size(13))
            .width(Length::Fixed(36.0))
            .align_x(Alignment::End)
            .into(),
    ])
    .spacing(8.0)
    .align_y(Alignment::Center)
    .into()
}

/// The covermark folder path for display, abbreviating `$HOME` to `~`.
fn covermark_dir_display() -> String {
    let Some(dir) = crate::app::preview::covermark_dir() else {
        return "~/.config/cosmic-capture-kit/covermarks".into();
    };
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = dir.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    dir.display().to_string()
}

#[cfg(test)]
mod parity_tests {
    //! DRAGON-234 parity contract for this (shared) Screenshots page.
    //!
    //! `screenshots_sections` renders each capture-extra row and the whole "Single
    //! Window Aesthetics" section by gating on the ACTIVE backend's
    //! `capture_extras()` bits (freeze / cursor / transparency / wallpaper +
    //! the freeze discriminator). These tests pin the mac-vs-Windows gap table as an
    //! executable contract, mirroring the two backend `caps()` shapes (backend.rs
    //! `MacBackend` + platform/windows/backend.rs `WindowsBackend`). If either backend
    //! flips a capture bit, reconcile the gap table in .dragon229/W5c-notes.md AND this
    //! test together — a divergence here means the settings UI parity changed.
    use crate::platform::backend::{Caps, CaptureExtras};

    /// A backend cap shape parameterised on the only two bits that differ between the
    /// mac and Windows still backends today; every other capture bit is true on both.
    fn caps(transparency: bool, wallpaper_compose: bool) -> Caps {
        Caps {
            name: "test",
            screenshot: true,
            record: true,
            window_list: true,
            window_capture: true,
            cursor_session: true,
            layer_overlay: false,
            wallpaper_path: true,
            freeze: true,
            transparency,
            wallpaper_compose,
            fullscreen_aware: true,
        }
    }

    /// macOS ScreenCaptureKit backend: every capture-extra advertised.
    fn mac_extras() -> CaptureExtras {
        caps(true, true).capture_extras()
    }

    /// Windows backend: now byte-identical to macOS — per-window transparency is preserved via
    /// WGC `CreateForWindow` when "Preserve window transparency" is on (DRAGON-276), so the row
    /// shows just like mac. (Before, PrintWindow rendered opaque and the row was hidden.)
    fn windows_extras() -> CaptureExtras {
        caps(true, true).capture_extras()
    }

    #[test]
    fn windows_screenshot_extras_match_mac() {
        // DRAGON-276: Windows now offers every screenshot extra macOS does, including the
        // "Preserve window transparency" row — the surfaces render identical rows + the Single
        // Window Aesthetics section.
        assert_eq!(windows_extras(), mac_extras());
    }

    #[test]
    fn windows_advertises_every_other_screenshot_extra() {
        let win = windows_extras();
        // freeze gates the "Freeze pixels" row AND the Single Window Aesthetics section
        // (focus appearance / active+inactive border / drop shadow / padding); cursor,
        // wallpaper (wallpaper-behind), and the fullscreen-aware skip all ride the shared
        // compose pipeline — all already wired on Windows.
        assert!(win.freeze, "freeze row + aesthetics section");
        assert!(win.cursor, "preserve mouse cursor row");
        assert!(win.wallpaper, "preserve wallpaper (wallpaper-behind) row");
        assert!(win.fullscreen_aware, "fullscreen-window compositing skip");
        assert!(win.transparency, "preserve window transparency row (WGC, DRAGON-276)");
    }
}
