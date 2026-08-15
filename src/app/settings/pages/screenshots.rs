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
                            .width(Length::Fixed(280.0))
                            // DRAGON-680: the app's softened text-selection fill.
                            .style(theme::input_style(theme::InputBase::Default)),
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
        let caps = self.active_screenshot_caps();
        let extras = caps.capture_extras();
        // DRAGON-625: on Linux's PORTAL backend, a region/monitor grant is a single
        // already-flattened frame (there are no separate per-window pixels to subtract a
        // wallpaper from), so "Preserve wallpaper" only ever changes anything for a
        // single-window still there (`capture_flow::PortalWindowDeco`, which composites the
        // portal's already-window-isolated frame over a crop of the wallpaper FILE, read
        // straight off disk, never over a captured desktop frame). Native screencopy has no
        // such limit: it recomposites region/monitor from real per-window pixels
        // (`crate::screenshot::region_windows`), so the caveat below only belongs on the
        // portal choice.
        //
        // `screenshot_uses_portal()` is spuriously true off Linux (its
        // `!native_capture_available()` fallback; see this file's `window_aesthetics` note
        // further down for the same landmine), and there is no "Portal" backend choice on
        // mac/Windows at all, so this is Linux-gated rather than calling it bare.
        #[cfg(target_os = "linux")]
        let portal_wallpaper_gap = self.screenshot_uses_portal();
        #[cfg(not(target_os = "linux"))]
        let portal_wallpaper_gap = false;
        // DRAGON-603: WHICH rows exist is the pure `capture_extra_rows`, so the answer
        // is the capability table's and this loop only builds what it names. The rows
        // then survive into the section unconditionally; see `capture_section_rows`.
        let mut contents = Vec::new();
        for id in super::capture::capture_extra_rows(extras) {
            let item = match id {
                // Freeze captures the launch-instant pixels (a live portal frame
                // can't be "frozen").
                super::capture::ExtraRow::Freeze => Item::new(
                    id.title(),
                    "Great for capturing images in motion and OCR content.",
                    toggle(self.freeze, |a0| Msg::Settings(SettingsMsg::SetFreeze(a0))),
                )
                .reset_with(self.freeze, d.freeze, |a0| Msg::Settings(SettingsMsg::SetFreeze(a0))),
                super::capture::ExtraRow::Cursor => Item::new(
                    id.title(),
                    "",
                    toggle(self.capture_cursor, |a0| Msg::Settings(SettingsMsg::SetCaptureCursor(a0))),
                )
                .reset_with(self.capture_cursor, d.capture_cursor, |a0| Msg::Settings(SettingsMsg::SetCaptureCursor(a0))),
                // Preserve window transparency applies to all three modes now (window always; region /
                // monitor when the wallpaper is off), so it sits with the shared capture options.
                super::capture::ExtraRow::Transparency => Item::new(
                    id.title(),
                    "",
                    toggle(self.capture_transparency, |a0| Msg::Settings(SettingsMsg::SetCaptureTransparency(a0))),
                )
                .reset_with(self.capture_transparency, d.capture_transparency, |a0| Msg::Settings(SettingsMsg::SetCaptureTransparency(a0))),
                super::capture::ExtraRow::Wallpaper => Item::new(
                    id.title(),
                    if portal_wallpaper_gap {
                        "Only applies to window capture."
                    } else {
                        ""
                    },
                    toggle(self.capture_wallpaper, |a0| Msg::Settings(SettingsMsg::SetCaptureWallpaper(a0))),
                )
                .reset_with(self.capture_wallpaper, !d.no_wallpaper, |a0| Msg::Settings(SettingsMsg::SetCaptureWallpaper(a0))),
            };
            contents.push((id, item));
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
        // Window-specific decoration options, keyed on the active backend's
        // `window_aesthetics` capability. History: DRAGON-186 Phase 2 keyed this on
        // `extras.freeze` as the compositor-vs-portal discriminator (rather than
        // `!screenshot_uses_portal()`, which is spuriously true on macOS). That
        // stand-in held while the two always coincided; DRAGON-562 broke the
        // coincidence — the portal backend decorates the finished window frame it
        // is handed (padding / borders / shadow / rounding / backdrop are pure
        // image math) while still having nothing to freeze. Native backends
        // declare the bit equal to their freeze bit, so their gating here is
        // byte-identical to the freeze-keyed era; only portal sessions gain the
        // section.
        if caps.window_aesthetics {
            // "Window focus appearance" (DRAGON-191): how a SINGLE-window capture is
            // portrayed — Active (the Active border) or Inactive (the Inactive border).
            // Region/monitor composites ignore this and pick per-window by real focus.
            // The old "Inactive with shadow" and "Raw" entries are gone: shadow is now a
            // separate toggle, and "Raw" (no border) is covered by setting a width to 0.
            let focus_idx = if self.window_single_active { 0usize } else { 1 };
            let def_focus = if d.window_single_active { 0usize } else { 1 };
            // Which rows render is the pure `aesthetic_rows` matrix (unit-tested),
            // two axes:
            //
            // - The MASTER "Enable single window aesthetic effects" toggle leads
            //   the section and every other row is bound to it: master OFF hides
            //   them all (the capture then delivers the bare frame through the
            //   `capture_flow::window_recomposite` gate) while the underlying
            //   preferences stay persisted, so re-enabling restores them exactly.
            //   Its description (`RECOMPOSITING_DESC`) renders whether the toggle
            //   is on or off (owner's call).
            // - The state-keyed rows (the "Window focus appearance" selector and the
            //   "Inactive border") also HIDE on a portal-fallback session, the
            //   DRAGON-569 hide-where-dead rule: the grant carries no activation
            //   info and the portal cannot drive window focus, so every portal
            //   window still draws the ACTIVE border
            //   (`capture_flow::single_window_border_active` pins it) and both rows
            //   are inert there. A choice that cannot apply is HIDDEN, never an
            //   inert or warning row. The Active border row stays visible on the
            //   fallback (it governs every portal window capture), so the section
            //   header keeps its group and stays too.
            //
            // Persisted values are untouched by hiding; a native session with the
            // master on renders every row exactly as before.
            let rows = aesthetic_rows(self.window_recompositing, self.overlay_fallback_active());
            let mut win_items = vec![
                Item::new(
                    "Enable single window aesthetic effects",
                    RECOMPOSITING_DESC,
                    toggle(self.window_recompositing, |a0| {
                        Msg::Settings(SettingsMsg::SetWindowRecompositing(a0))
                    }),
                )
                .reset_with(self.window_recompositing, d.window_recompositing, |a0| {
                    Msg::Settings(SettingsMsg::SetWindowRecompositing(a0))
                }),
            ];
            if rows.focus_selector {
                win_items.push(
                    Item::new(
                        "Window focus appearance",
                        "",
                        crate::widgets::arrow_cursor::arrow_cursor(crate::widgets::press_redraw(
                            widget::dropdown(
                                &WINDOW_FOCUS_APPEARANCES,
                                Some(focus_idx),
                                |a0| Msg::Settings(SettingsMsg::SetWindowFocusAppearance(a0)),
                            ),
                        )),
                    )
                    .reset_with(focus_idx, def_focus, |a0| Msg::Settings(SettingsMsg::SetWindowFocusAppearance(a0))),
                );
            }
            // Active border: colour swatch (follows the accent when unpinned) + width
            // slider. The row reset restores the WHOLE border to default (colour back
            // to Follow-accent AND width back to default), enabled whenever either the
            // colour or the width differs; the swatch's own picker still resets colour
            // alone.
            if rows.active_border {
                win_items.push(
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
                );
            }
            // Inactive border: colour swatch + width slider. Row reset restores the
            // whole border (colour + width) to default.
            if rows.inactive_border {
                win_items.push(
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
                );
            }
            if rows.shadow {
                win_items.push(
                    Item::new(
                        "Drop shadow",
                        "",
                        toggle(self.window_drop_shadow, |a0| Msg::Settings(SettingsMsg::SetWindowDropShadow(a0))),
                    )
                    .reset_with(self.window_drop_shadow, d.window_drop_shadow, |a0| Msg::Settings(SettingsMsg::SetWindowDropShadow(a0))),
                );
            }
            if rows.padding {
                win_items.push(
                    Item::new(
                        "Add padding around the window",
                        "",
                        toggle(self.window_padding, |a0| Msg::Settings(SettingsMsg::SetWindowPadding(a0))),
                    )
                    .reset_with(self.window_padding, d.window_padding, |a0| Msg::Settings(SettingsMsg::SetWindowPadding(a0))),
                );
            }
            // Padding amount reveal (nested under its toggle, so also master-bound).
            if rows.padding && self.window_padding {
                win_items.push(
                    Item::new(
                        "Padding",
                        "",
                        num_input(
                            "32",
                            &self.window_padding_px.text,
                            |a0| Msg::Settings(SettingsMsg::SetWindowPaddingPx(a0)),
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
        // DRAGON-353: the "Covermarks" section lived here. It configures the PREVIEW
        // EDITOR's covermark picker rather than anything about taking a screenshot, so it
        // moved verbatim to the Preview Editor page (`pages/preview_editor.rs`) — same
        // field, message and reset. The Screenshots tab keeps its other sections.
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

/// The master recompositing toggle's description, the owner's exact wording. Rendered
/// whether the toggle is on or off.
const RECOMPOSITING_DESC: &str = "This setting will allow extra aesthetic decorations \
to be composited onto your window screenshot such as neon borders, wallpaper padding, \
and window glass effects on supported systems.";

/// Which Single Window Aesthetics rows the page offers (besides the always-present
/// master toggle). See [`aesthetic_rows`], the pure decision that fills it.
struct AestheticRows {
    /// The "Window focus appearance" Active/Inactive selector.
    focus_selector: bool,
    /// The "Active border" colour + width row.
    active_border: bool,
    /// The "Inactive border" colour + width row.
    inactive_border: bool,
    /// The "Drop shadow" toggle.
    shadow: bool,
    /// The "Add padding around the window" toggle (its px reveal nests under it).
    padding: bool,
}

/// Pure, unit-tested (`aesthetic_rows_tests`): the Single Window Aesthetics row
/// visibility matrix, two axes.
///
/// - `master` is the "Enable single window aesthetic effects" toggle: OFF hides
///   EVERY other aesthetic row (the capture delivers the bare frame through
///   `capture_flow::window_recomposite`; the preferences stay persisted).
/// - `fallback` is the portal-fallback session (`App::overlay_fallback_active`): it
///   additionally hides the state-keyed rows (the focus-appearance selector and the
///   Inactive border), because the portal cannot drive window focus and every portal
///   window still draws the ACTIVE border
///   (`capture_flow::single_window_border_active`). Hide-where-dead, the DRAGON-569
///   rule: never an inert or warning row.
fn aesthetic_rows(master: bool, fallback: bool) -> AestheticRows {
    AestheticRows {
        focus_selector: master && !fallback,
        active_border: master,
        inactive_border: master && !fallback,
        shadow: master,
        padding: master,
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
        crate::widgets::arrow_cursor::arrow_cursor(
            widget::slider(0..=10, value, msg)
                .step(1u32)
                .width(Length::Fixed(160.0)),
        ),
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

#[cfg(test)]
mod aesthetic_rows_tests {
    //! The Single Window Aesthetics visibility matrix (master x fallback). The master
    //! toggle row itself is unconditional; these pin every OTHER row's gate.
    use super::aesthetic_rows;

    // Master ON, native session: every row renders, the pre-master page.
    #[test]
    fn master_on_native_shows_every_row() {
        let rows = aesthetic_rows(true, false);
        assert!(rows.focus_selector);
        assert!(rows.active_border);
        assert!(rows.inactive_border);
        assert!(rows.shadow);
        assert!(rows.padding);
    }

    // Master ON, portal fallback: only the state-keyed rows hide (the selector and
    // the Inactive border); the Active border governs every portal window capture
    // and stays, with the shadow and padding rows.
    #[test]
    fn fallback_hides_only_the_state_keyed_rows() {
        let rows = aesthetic_rows(true, true);
        assert!(!rows.focus_selector);
        assert!(!rows.inactive_border);
        assert!(rows.active_border);
        assert!(rows.shadow);
        assert!(rows.padding);
    }

    // Master OFF: every other aesthetic row hides, on both session kinds. The
    // capture delivers the bare frame; the preferences stay persisted.
    #[test]
    fn master_off_hides_every_row_on_both_paths() {
        for fallback in [false, true] {
            let rows = aesthetic_rows(false, fallback);
            assert!(!rows.focus_selector, "fallback={fallback}");
            assert!(!rows.active_border, "fallback={fallback}");
            assert!(!rows.inactive_border, "fallback={fallback}");
            assert!(!rows.shadow, "fallback={fallback}");
            assert!(!rows.padding, "fallback={fallback}");
        }
    }
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
            cursor_toggle: true,
            layer_overlay: false,
            wallpaper_path: true,
            freeze: true,
            transparency,
            wallpaper_compose,
            fullscreen_aware: true,
            // Mirrors freeze, as both native still backends declare it (DRAGON-562).
            window_aesthetics: true,
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
        // freeze gates the "Freeze pixels" row; the Single Window Aesthetics
        // section (focus appearance / active+inactive border / drop shadow /
        // padding) reads `Caps::window_aesthetics` since DRAGON-562, which both
        // native still backends declare equal to their freeze bit. cursor,
        // wallpaper (wallpaper-behind), and the fullscreen-aware skip all ride
        // the shared compose pipeline — all already wired on Windows.
        assert!(win.freeze, "freeze row");
        assert!(caps(true, true).window_aesthetics, "single-window aesthetics section");
        assert!(win.cursor, "preserve mouse cursor row");
        assert!(win.wallpaper, "preserve wallpaper (wallpaper-behind) row");
        assert!(win.fullscreen_aware, "fullscreen-window compositing skip");
        assert!(win.transparency, "preserve window transparency row (WGC, DRAGON-276)");
    }
}
