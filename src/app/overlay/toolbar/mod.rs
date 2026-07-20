pub(super) mod layout;

use super::super::*;
use layout::V_W;

/// Resolve an icon NAME to a handle, intercepting the names libcosmic's embedded
/// `cosmic-icons` subset does NOT provide (so `from_name` would render blank on a
/// platform without a system icon theme — macOS) and serving those from SVGs
/// vendored into this repo. Every other name falls through to libcosmic's embedded
/// lookup, byte-identically to a bare `from_name(name).handle()`.
///
/// - The COSMIC screenshot mode icons (region/window/monitor) ship with
///   xdg-desktop-portal-cosmic on COSMIC systems, not with `cosmic-icons`, so
///   `from_name` finds them on COSMIC (system theme) but not on macOS. Vendored from
///   pop-os/xdg-desktop-portal-cosmic (`data/icons/scalable/actions/`).
/// - `object-move-symbolic` (the preview pan/grab tool) is filled by the system theme
///   on Linux but is absent from BOTH `cosmic-icons` and current upstream Adwaita, so
///   a project-owned symbolic glyph is bundled (see `res/icons/ATTRIBUTION.md`).
///
/// Shared (`pub(crate)`) so the preview chrome routes its vendored-name buttons
/// through the same seam instead of each carrying its own `include_bytes!`.
pub(crate) fn vendored_icon_handle(name: &str) -> cosmic::widget::icon::Handle {
    use cosmic::widget::icon;
    // `.symbolic(true)` is required: it marks the handle as a symbolic icon so cosmic
    // applies the widget's tint color (the `mode_icon` active/inactive class). Without
    // it the `currentColor` SVG renders with no tint — effectively invisible.
    match name {
        "screenshot-selection-symbolic" => icon::from_svg_bytes(
            include_bytes!("../../../../res/icons/cosmic/screenshot-selection-symbolic.svg")
                .as_slice(),
        )
        .symbolic(true),
        "screenshot-window-symbolic" => icon::from_svg_bytes(
            include_bytes!("../../../../res/icons/cosmic/screenshot-window-symbolic.svg").as_slice(),
        )
        .symbolic(true),
        "screenshot-screen-symbolic" => icon::from_svg_bytes(
            include_bytes!("../../../../res/icons/cosmic/screenshot-screen-symbolic.svg").as_slice(),
        )
        .symbolic(true),
        "object-move-symbolic" => icon::from_svg_bytes(
            include_bytes!("../../../../res/icons/local/object-move-symbolic.svg").as_slice(),
        )
        .symbolic(true),
        _ => icon::from_name(name).handle(),
    }
}

/// The timer/record chip's icon+text row — the shared shape behind the recording
/// elapsed time, the countdown remaining time, and the idle delay readout (each a
/// slightly different combination of the same pieces, formerly ~20 duplicated lines
/// per state). `lead_icon` is an optional fixed-size white symbolic glyph (the stop
/// dot, or the countdown's check/record dot); `text`/`text_size`/`mono` are the main
/// count; `trail` is an optional trailing glyph (the delay chip's caret/✕). Every text
/// piece is locked to the chip's `ICON_BOX` line height so mixed icon+text rows centre
/// together (an 18px font's default line box is taller than the chip, which otherwise
/// pins text to the top instead of centring it).
fn render_chip(
    lead_icon: Option<(&'static str, f32)>,
    text: String,
    text_size: u16,
    mono: bool,
    trail: Option<&'static str>,
    spacing: f32,
) -> Element<'static, Msg> {
    let white_icon = |name: &'static str, size: f32| -> Element<'static, Msg> {
        widget::icon::icon(vendored_icon_handle(name))
            .size(64)
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .class(cosmic::theme::Svg::Custom(Rc::new(|_t| cosmic::widget::svg::Style {
                color: Some(cosmic::iced::Color::WHITE),
            })))
            .into()
    };
    let line_height =
        cosmic::iced::widget::text::LineHeight::Absolute(cosmic::iced::Pixels(ICON_BOX));
    let mut row: Vec<Element<'static, Msg>> = Vec::new();
    if let Some((name, size)) = lead_icon {
        row.push(white_icon(name, size));
    }
    let main = widget::text(text).size(text_size);
    let main = if mono { main.font(cosmic::iced::Font::MONOSPACE) } else { main };
    row.push(main.line_height(line_height).into());
    if let Some(trail) = trail {
        row.push(widget::text(trail).size(16).line_height(line_height).into());
    }
    widget::row(row).spacing(spacing).align_y(Alignment::Center).into()
}

impl App {
    /// The capture toolbar. When a region is drawn it sits just outside the
    /// selection (following resize/move); otherwise it pins to the bottom centre of
    /// every screen. The mode selectors double as the shutter (pressing the active
    /// Region/Monitor selector captures). During a countdown it shows here too, with
    /// the timer chip counting down (and cancelling on click) in place of the
    /// removed badge. Groups sit side by side, stacking vertically (width-matched)
    /// only when anchored to the left/right of a region.
    pub(super) fn capture_button_layer(&self, o: &OutputState) -> Option<Element<'_, Msg>> {
        // The recording controls moved to the system tray: hide the in-frame toolbar
        // entirely while the tray OWNS the control surface (the tray + hotkeys drive
        // stop/mic/system-audio). DRAGON-172: `tray_hides_toolbar`, NOT `tray.is_some()` —
        // on macOS a daemon relay can be attached in toolbar-placement mode with the
        // in-frame toolbar STILL visible alongside the daemon menu (both dispatch the same
        // idempotent actions). If no control surface replaced the toolbar, it stays, so
        // control is never lost.
        if self.tray_hides_toolbar {
            return None;
        }
        let (rect, horizontal) = self.toolbar_layout(o)?;
        // During a countdown the chip counts down (cancel on click); during a
        // recording it's a record indicator (stop on click). Either way only the
        // chip group shows.
        let counting = self.countdown.is_some();
        let recording = self.recording.is_some();
        let rec_paused = self.recording_paused();
        let active = counting || recording;

        // Group/button geometry is module-level (ICON_BOX, BTN_PAD, GROUP_PAD) so
        // `toolbar_layout`'s placement + input zone derive from the exact same
        // numbers the widgets are built from.
        // When stacked (left/right placement), groups take the wider group's
        // width so their backdrops line up. A lone countdown chip is never stacked.
        let group_width = if recording || horizontal || counting {
            Length::Shrink
        } else {
            Length::Fixed(V_W)
        };
        // Stacked groups are a fixed width, so their buttons fill and share that
        // space evenly; laid out horizontally (or active) they keep their natural
        // footprint. The recording chip holds the record dot + elapsed time; the
        // size readout is a separate connected group.
        let (btn_width, chip_width, row_width) = if recording {
            // Record dot + `MMM:SS` (room for 3-digit minutes) elapsed time.
            (Length::Fixed(40.0), Length::Fixed(92.0), Length::Shrink)
        } else if counting {
            // Wider: icon + NN + ✕.
            (Length::Fixed(40.0), Length::Fixed(74.0), Length::Shrink)
        } else if horizontal {
            (Length::Fixed(40.0), Length::Fixed(54.0), Length::Shrink)
        } else {
            (Length::Fill, Length::Fill, Length::Fill)
        };

        // A button's icon: a fixed 22px glyph centered in a fill-width box, so a
        // button stretched to fill its (stacked) group keeps the glyph at its true
        // size and centered instead of stretching it.
        let mode_icon = |name: &'static str, active: bool| {
            let icon = widget::icon::icon(vendored_icon_handle(name))
                .size(64)
                .width(Length::Fixed(ICON_BOX))
                .height(Length::Fixed(ICON_BOX))
                .class(if active {
                    cosmic::theme::Svg::Custom(Rc::new(|t| cosmic::widget::svg::Style {
                        color: Some(crate::app::theme::accent(t)),
                    }))
                } else {
                    cosmic::theme::Svg::default()
                });
            widget::container(icon)
                .width(Length::Fill)
                .align_x(Alignment::Center)
        };
        let mode_btn = |name: &'static str, m: Mode, active: bool| {
            // The selectors ARE the shutter (the dedicated capture button is gone):
            // pressing the active Region selector captures the drawn region, and the
            // active Monitor selector captures the monitor the toolbar sits on. The
            // active Window selector is a no-op (you capture by clicking the window).
            // An inactive selector just switches mode. All three render accent
            // ("purple") like the region option always did; the active one is marked
            // by the selected backdrop.
            let msg = if active {
                match m {
                    Mode::Region | Mode::Monitor => {
                        Some(Msg::Capture(CaptureMsg::Capture { output: o.name.clone() }))
                    }
                    Mode::Window => None,
                }
            } else {
                Some(Msg::Capture(CaptureMsg::SetMode(m)))
            };
            // Natural padding keeps the icon at its proper size (forcing the height
            // scaled/clipped it); the width is fixed horizontally and fills when
            // stacked so the buttons share the group evenly.
            widget::button::custom(mode_icon(name, true))
                .selected(active)
                .class(cosmic::theme::Button::Icon)
                .on_press_maybe(msg)
                .width(btn_width)
                .padding(BTN_PAD)
        };
        // Photo/video: a SEGMENTED pair (one control, two joined halves) rather than
        // two free-standing buttons — the active half is filled accent with an
        // on-accent glyph, the other half sits flat on the group with a subdued
        // glyph, and only the pair's outer corners are rounded.
        let kind_btn = |name: &'static str, active: bool, msg: Msg, round_left: bool, round_right: bool| {
            // Default icon class: the button's per-state `icon_color` (below) colours
            // it, so the glyph can react to hover — an Svg::Custom class can't see
            // hover state.
            let icon = widget::icon::icon(vendored_icon_handle(name))
                .size(64)
                .width(Length::Fixed(ICON_BOX))
                .height(Length::Fixed(ICON_BOX));
            // The shared segmented-pair style (theme.rs) — one source for this
            // pair and the preview's pointer/pan + pointer/razor toggles.
            let seg_style = move |t: &cosmic::Theme, hovered: bool| {
                crate::app::theme::segment_style(t, active, hovered, round_left, round_right)
            };
            widget::button::custom(
                widget::container(icon)
                    .width(Length::Fill)
                    .align_x(Alignment::Center),
            )
            .class(cosmic::theme::Button::Custom {
                active: Box::new(move |_, t| seg_style(t, false)),
                disabled: Box::new(move |t| seg_style(t, false)),
                hovered: Box::new(move |_, t| seg_style(t, true)),
                pressed: Box::new(move |_, t| seg_style(t, true)),
            })
            .on_press(msg)
            .width(btn_width)
            .padding(BTN_PAD)
        };
        // Neutral icon button (settings/close) — same footprint as a mode button.
        let action_btn = |name: &'static str, msg: Msg| {
            widget::button::custom(mode_icon(name, false))
                .class(cosmic::theme::Button::Icon)
                .on_press(msg)
                .width(btn_width)
                .padding(BTN_PAD)
        };
        let mode_group = widget::container(
            widget::row(vec![
                mode_btn(
                    "screenshot-selection-symbolic",
                    Mode::Region,
                    self.mode == Mode::Region,
                )
                .into(),
                mode_btn(
                    "screenshot-window-symbolic",
                    Mode::Window,
                    self.mode == Mode::Window,
                )
                .into(),
                mode_btn(
                    "screenshot-screen-symbolic",
                    Mode::Monitor,
                    self.mode == Mode::Monitor,
                )
                .into(),
            ])
            .spacing(2.0)
            .width(row_width)
            .align_y(Alignment::Center),
        )
        .width(group_width)
        .align_x(Alignment::Center)
        .padding(GROUP_PAD)
        .class(cosmic::theme::Container::Custom(Box::new(|theme| {
            let c = theme.cosmic();
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(c.background.component.base.into())),
                border: Border {
                    // The button token: groups round like the buttons they hold
                    // (a capsule under the "round" preference). Capped at the group
                    // half-height so it matches the stacked kind+timer group and
                    // never over-rounds; byte-identical for this short group.
                    radius: crate::app::theme::rounding(theme)
                        .xl_capped(GROUP_H_BASE / 2.0)
                        .into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })));

        // Kind toggle: camera (image) | video. Recording isn't wired up yet, but
        // the toggle is live (mirrors the bottom toolbar).
        let kind_pair: Element<'_, Msg> = widget::row(vec![
            // Scanner kind: captures as a photo, and the only kind QR/OCR runs in.
            kind_btn(
                "document-properties-symbolic",
                self.kind == Kind::Scanner,
                Msg::Capture(CaptureMsg::SetKind(Kind::Scanner)),
                true,
                false,
            )
            .into(),
            kind_btn(
                "camera-photo-symbolic",
                self.kind == Kind::Image,
                Msg::Capture(CaptureMsg::SetKind(Kind::Image)),
                false,
                false,
            )
            .into(),
            kind_btn(
                "camera-video-symbolic",
                self.kind == Kind::Video,
                Msg::Capture(CaptureMsg::SetKind(Kind::Video)),
                false,
                true,
            )
            .into(),
        ])
        .spacing(0.0)
        .align_y(Alignment::Center)
        .into();

        // Timer chip: normally the configured delay (mono `NN` + caret, opens the
        // delay menu). During a countdown it shows the remaining seconds + ✕ and
        // cancels on click — standing in for the old top-right badge.
        let chip_secs = match self.countdown {
            Some(n) => n as u64,
            None => self.configured_delay_secs(),
        };
        let chip_trail = if counting { "✕" } else { "⌄" };
        let cancel_hovered = self.hover == Hover::Cancel;
        // "00" (no delay) gets the same subdued wash as an off toggle; a real delay
        // reads in the theme foreground (white on dark, dark on light).
        let zero_delay = !active && self.configured_delay_secs() == 0;
        // While recording the chip is a white stop glyph + mono `MMM:SS` elapsed time
        // (it IS the stop button). Otherwise mono `NN`. Both routes share their
        // format+layout via `render_chip` (see its doc).
        let chip_inner: Element<'_, Msg> = if recording {
            // RECORDED time — freezes while paused (wall time minus pauses).
            let secs = self.recording_elapsed_secs();
            // MM:SS, minutes space-padded to 3 so the monospace string is a constant
            // width — minutes grow leftward into the reserved room right after the
            // icon, while the colon, seconds, icon and cancel button never shift.
            render_chip(
                Some(("media-playback-stop-symbolic", 16.0)),
                format!("{:>3}:{:02}", secs / 60, secs % 60),
                14,
                true,
                None,
                6.0,
            )
        } else {
            // During a countdown, prepend what it'll do: a check for a photo, a record
            // dot for a video.
            let lead = if counting {
                let cd_icon = if self.kind == Kind::Video {
                    "media-record-symbolic"
                } else {
                    "emblem-ok-symbolic"
                };
                Some((cd_icon, 15.0))
            } else {
                None
            };
            render_chip(lead, format!("{chip_secs:02}"), 18, true, Some(chip_trail), 5.0)
        };
        // The chip matches the base button height (the taller dedicated shutter it
        // once stood in for is gone), so every toolbar group lines up.
        let chip = widget::mouse_area(
            widget::container(
                widget::container(chip_inner)
                    .height(Length::Fixed(ICON_BOX))
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center),
            )
                .width(chip_width)
                .padding(cosmic::iced::Padding::from([BTN_PAD, 0.0]))
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .class(cosmic::theme::Container::Custom(Box::new(move |theme| {
                    let c = theme.cosmic();
                    if recording {
                        // Solid red, brighter on hover. White glyph. Paused: the
                        // countdown's darker red family — clearly "not live".
                        let base = match (rec_paused, cancel_hovered) {
                            (true, true) => crate::app::theme::RECORD_DIM_HOVER,
                            (true, false) => crate::app::theme::RECORD_DIM,
                            (false, true) => crate::app::theme::RECORD_HOVER,
                            (false, false) => crate::app::theme::RECORD,
                        };
                        cosmic::iced::widget::container::Style {
                            background: Some(Background::Color(base)),
                            text_color: Some(cosmic::iced::Color::WHITE),
                            border: Border {
                                radius: crate::app::theme::rounding(theme).xl.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }
                    } else if counting {
                        // Same red family as the recording chip, but darker (it's a
                        // pre-capture state, not a live recording); brighten on hover.
                        let bg = if cancel_hovered {
                            crate::app::theme::RECORD_DIM_HOVER
                        } else {
                            crate::app::theme::RECORD_DIM
                        };
                        cosmic::iced::widget::container::Style {
                            background: Some(Background::Color(bg)),
                            text_color: Some(cosmic::iced::Color::WHITE),
                            border: Border {
                                radius: crate::app::theme::rounding(theme).xl.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }
                    } else {
                        // "00" (no delay) → the toggles' subdued wash; a set delay →
                        // the theme foreground. Lighten the backing on hover so it
                        // matches the other toolbar buttons.
                        let fg = if zero_delay {
                            state_mix(theme, MIX_OFF)
                        } else {
                            c.background.component.on.into()
                        };
                        let bg = if cancel_hovered {
                            c.background.component.hover
                        } else {
                            c.background.component.base
                        };
                        cosmic::iced::widget::container::Style {
                            background: Some(Background::Color(bg.into())),
                            text_color: Some(fg),
                            // A set delay is an armed state: same 1px trim ring as
                            // the toggles — accent when set, subdued when "00".
                            border: Border {
                                radius: crate::app::theme::rounding(theme).xl.into(),
                                width: 1.0,
                                color: if zero_delay {
                                    state_mix(theme, MIX_OFF)
                                } else {
                                    crate::app::theme::accent(theme)
                                },
                            },
                            ..Default::default()
                        }
                    }
                }))),
        )
        .on_press(if recording {
            Msg::Recording(RecordingMsg::StopRecording)
        } else if counting {
            Msg::Capture(CaptureMsg::CancelCapture)
        } else {
            Msg::Capture(CaptureMsg::ToggleDelayMenu)
        })
        .on_enter(Msg::Capture(CaptureMsg::SetHover(Hover::Cancel)))
        .on_exit(Msg::Capture(CaptureMsg::SetHover(Hover::None)))
        .interaction(cosmic::iced::mouse::Interaction::Pointer);

        let delay_el: Element<'_, Msg> = if self.delay_menu_open && !active {
            let items: Vec<Element<'_, Msg>> = DELAYS
                .iter()
                .enumerate()
                .map(|(i, (_, s))| {
                    widget::button::custom(
                        widget::text(format!("{s:02}"))
                            .font(cosmic::iced::Font::MONOSPACE)
                            .size(16)
                            // Match the chip: theme foreground, not the text-button accent.
                            .class(cosmic::theme::Text::Custom(|t| {
                                cosmic::iced::widget::text::Style {
                                    color: Some(t.cosmic().background.component.on.into()),
                                    ..Default::default()
                                }
                            })),
                    )
                    .on_press(Msg::Capture(CaptureMsg::PickDelay(i)))
                    .width(Length::Fill)
                    .class(cosmic::theme::Button::Text)
                    .into()
                })
                .collect();
            let menu = widget::container(widget::column(items).spacing(2.0))
                .padding(4.0)
                .width(Length::Fixed(72.0))
                .class(cosmic::theme::Container::Custom(Box::new(|theme| {
                    let c = theme.cosmic();
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(c.background.component.base.into())),
                        border: Border {
                            radius: crate::app::theme::rounding(theme).s.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                })));
            widget::popover(chip)
                .position(cosmic::widget::popover::Position::Bottom)
                .popup(menu)
                .on_close(Msg::Capture(CaptureMsg::ToggleDelayMenu))
                .into()
        } else {
            chip.into()
        };

        // Shared rounded backdrop for a group of connected buttons — the button
        // token, so groups round like the buttons they hold.
        let group_bg = || {
            cosmic::theme::Container::Custom(Box::new(|theme| {
                let c = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(c.background.component.base.into())),
                    border: Border {
                        // Cap at the standard group half-height so the STACKED
                        // kind+timer group (taller than wide once the delay chip
                        // wraps below the kind trio) rounds like the horizontal
                        // groups instead of ballooning into a blob under the
                        // "round" preference. Byte-identical for every short group
                        // (their clamp was already this value); see `xl_capped`.
                        radius: crate::app::theme::rounding(theme)
                            .xl_capped(GROUP_H_BASE / 2.0)
                            .into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }))
        };

        // Group 1: kind toggle + delay chip — but just the chip while counting
        // down. While recording, the pause/resume button leads the group, then
        // the (filled) stop chip, then a normal-coloured ✕ to cancel and discard.
        let timer_content: Element<'_, Msg> = if recording {
            // Pause bars while live; the play glyph while paused (press = resume).
            let pause_icon = if rec_paused {
                "media-playback-start-symbolic"
            } else {
                "media-playback-pause-symbolic"
            };
            widget::row(vec![
                action_btn(pause_icon, Msg::Recording(RecordingMsg::TogglePause)).into(),
                delay_el,
                // Delete glyph (not a plain close): cancelling DISCARDS the recording,
                // matching the preview's delete button.
                action_btn("edit-delete-symbolic", Msg::Recording(RecordingMsg::CancelRecording)).into(),
            ])
            .spacing(4.0)
            .width(row_width)
            .align_y(Alignment::Center)
            .into()
        } else if active {
            widget::row(vec![delay_el])
                .spacing(4.0)
                .width(row_width)
                .align_y(Alignment::Center)
                .into()
        } else if self.kind == Kind::Scanner {
            // Scanner never counts down, so the delay chip hides with it.
            widget::row(vec![kind_pair])
                .spacing(4.0)
                .width(row_width)
                .align_y(Alignment::Center)
                .into()
        } else if horizontal {
            widget::row(vec![kind_pair, delay_el])
                .spacing(4.0)
                .width(row_width)
                .align_y(Alignment::Center)
                .into()
        } else {
            // Stacked beside the region (left/right anchor): the trio + chip don't
            // fit the narrow stack width side by side (the glyphs clipped), so the
            // chip moves BELOW the segment trio.
            widget::column(vec![kind_pair, delay_el])
                .spacing(4.0)
                .width(row_width)
                .align_x(Alignment::Center)
                .into()
        };
        let kind_timer_group = widget::container(timer_content)
        .width(group_width)
        .align_x(Alignment::Center)
        .padding(GROUP_PAD)
        .class(group_bg());

        // Group 4: settings + close.
        let util_group = widget::container(
            widget::row(vec![
                action_btn("emblem-system-symbolic", Msg::WindowChrome(WindowChromeMsg::OpenGear)).into(),
                action_btn("window-close-symbolic", Msg::WindowChrome(WindowChromeMsg::Quit)).into(),
            ])
            .spacing(2.0)
            .width(row_width)
            .align_y(Alignment::Center),
        )
        .width(group_width)
        .align_x(Alignment::Center)
        .padding(GROUP_PAD)
        .class(group_bg());

        // Toggle group: scanner + mic + system audio, toggleable in EVERY mode. One
        // unified palette carries state: On = accent (or white over the live meter
        // when a channel is armed in video mode), Off = the subdued wash — same
        // glyph either way, no slashed-icon variants.
        let video = self.kind == Kind::Video;
        // `level` is Some only when the channel is armed — then the button shows the
        // half-transparent level fill (green, red past the mic test's red zone) with
        // a white icon for contrast.
        // `msg` is `None` for a non-interactive button (push-to-talk mic: hold-only, no
        // click-to-toggle).
        let toggle_btn = |name: &'static str,
                          on: bool,
                          msg: Option<Msg>,
                          level: Option<f32>|
         -> Element<'static, Msg> {
            let metering = level.is_some();
            let icon = widget::icon::Icon::from(widget::icon::from_name(name).size(64))
                .width(Length::Fixed(ICON_BOX))
                .height(Length::Fixed(ICON_BOX))
                .class(cosmic::theme::Svg::Custom(Rc::new(move |t: &cosmic::Theme| {
                    let color = if metering || on {
                        // On: the default icon foreground (same as the gear/close
                        // buttons — white in dark mode, dark in light mode), also
                        // legible over the half-transparent meter tint when armed.
                        t.cosmic().background.component.on.into()
                    } else {
                        // Turned off: subtle, but clearly present.
                        state_mix(t, MIX_OFF)
                    };
                    cosmic::widget::svg::Style { color: Some(color) }
                })));
            let btn = widget::button::custom(
                widget::container(icon)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center),
            )
            .selected(on && !metering)
            .class(cosmic::theme::Button::Icon)
            .on_press_maybe(msg)
            .width(btn_width)
            .padding(BTN_PAD);
            // One wrapper for both adornments: the live meter fill (when armed) and a
            // 1px trim ring — accent while ON, the subdued wash while off (so the
            // outline is always present, only its strength changes). The border draws
            // inside the container's own bounds (no padding), so the button's
            // footprint never changes.
            widget::container(btn)
                .class(cosmic::theme::Container::Custom(Box::new(move |theme| {
                    cosmic::iced::widget::container::Style {
                        background: level.map(meter_background),
                        border: Border {
                            radius: crate::app::theme::rounding(theme).xl.into(),
                            width: 1.0,
                            color: if on {
                                crate::app::theme::accent(theme)
                            } else {
                                state_mix(theme, MIX_OFF)
                            },
                        },
                        ..Default::default()
                    }
                })))
                .into()
        };
        // Push-to-talk: the mic is armed but muted, lit only while the hotkey is held,
        // and NOT clickable (hold-to-talk, no toggle). Otherwise it's the usual toggle.
        let ptt = self.ptt_active();
        let mic_on = if ptt { self.ptt_held } else { self.record_mic };
        let mic_level = (video && mic_on).then_some(self.mic_level);
        let sys_level = (video && self.record_system_audio).then_some(self.sys_level);
        // Mic + speaker (the scanner is a kind segment now, not a toggle here). The
        // group only exists in video mode — audio has no effect on a photo/scan.
        let toggle_row: Vec<Element<'_, Msg>> = vec![
            toggle_btn(
                "audio-input-microphone-symbolic",
                mic_on,
                (!ptt).then(|| Msg::Recording(RecordingMsg::ToggleMic)),
                mic_level,
            ),
            toggle_btn(
                "audio-volume-high-symbolic",
                self.record_system_audio,
                Some(Msg::Recording(RecordingMsg::ToggleSystemAudio)),
                sys_level,
            ),
        ];
        let audio_group = widget::container(
            widget::row(toggle_row)
                .spacing(2.0)
                .width(row_width)
                .align_y(Alignment::Center),
        )
        .width(group_width)
        .align_x(Alignment::Center)
        .padding(GROUP_PAD)
        .class(group_bg());

        // Kind+timer, mode switcher, audio, [capture], then settings/close. The
        // capture button is only present when anchored to a region; the bottom
        // toolbar (no selection / window / monitor) omits it. While counting down,
        // only the timer chip group shows. Side by side normally, stacked only when
        // anchored to the left/right of a region.
        let groups: Vec<Element<'_, Msg>> = if active {
            // During a video countdown/recording, keep the audio group visible so
            // channels can be toggled live — placed before the timer/record chip.
            let mut g = Vec::new();
            if self.kind == Kind::Video {
                g.push(audio_group.into());
            }
            g.push(kind_timer_group.into());
            g
        } else {
            let mut g = vec![kind_timer_group.into()];
            if video {
                g.push(audio_group.into());
            }
            if self.kind != Kind::Scanner {
                g.push(mode_group.into());
            }
            g.push(util_group.into());
            g
        };
        let groups_el: Element<'_, Msg> = if horizontal {
            widget::row(groups)
                .spacing(8.0)
                .align_y(Alignment::Center)
                .into()
        } else {
            widget::column(groups)
                .spacing(8.0)
                .align_x(Alignment::Center)
                .into()
        };
        // The whole toolbar is draggable from anywhere on it (taps still click
        // through to the buttons); dragging emits offset deltas, and a drag-end
        // re-syncs the active overlay's click-through input region to the chip's
        // new position.
        let out_name = o.name.clone();
        let content: Element<'_, Msg> = crate::widgets::DragArea::new(groups_el, move |a0, a1| {
            Msg::Capture(CaptureMsg::ToolbarPan(out_name.clone(), a0, a1))
        })
        .on_drag_end(Msg::Capture(CaptureMsg::ToolbarDragEnd))
        .into();
        // `placement` already centered the (now exact-width) box over the region,
        // clamping it onto the screen only when it would hang off an edge. Drop
        // the content at that box's top-left.
        Some(
            widget::container(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Start)
                .align_y(Alignment::Start)
                .padding(cosmic::iced::Padding {
                    top: rect.y,
                    left: rect.x,
                    right: 0.0,
                    bottom: 0.0,
                })
                .into(),
        )
    }
}
