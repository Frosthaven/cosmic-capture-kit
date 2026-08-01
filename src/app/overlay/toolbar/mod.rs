pub(super) mod layout;

use super::super::*;
use layout::V_W;

// Icon resolution now lives in the shared Lucide resolver `crate::widgets::icons::handle`
// (DRAGON-324): every glyph is bundled, so the old per-name "is it in the embedded set?"
// workaround (`vendored_icon_handle`) is gone and resolution is platform-independent.

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
        crate::widgets::icons::sized(name, size)
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
            let icon = crate::widgets::icons::sized(name, ICON_BOX)
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
            // DRAGON-392 correction — the ACTIVE REGION selector is an "accept region" button, so
            // it wears the SAME look as the preview editor's accept-crop button: the shared
            // accent-filled segment style with the solid on-accent glyph
            // (`theme::segment_style`, the one source both go through — never a second copy of
            // the accent values). Both confirm a rect the user actively dragged out.
            //
            // Scoped to REGION and only while it is the ACTIVE mode. Window and Monitor keep the
            // plain selector look: there is no user-authored geometry to accept there — picking
            // the target IS the action (the active Window selector is a literal no-op) — so
            // dressing them as an accept would promise a confirmation step that doesn't exist.
            let accept_region = active && m == Mode::Region;
            if accept_region {
                let seg_style = move |t: &cosmic::Theme, hovered: bool| {
                    // A standalone button, so BOTH outer corners round.
                    crate::app::theme::segment_style(t, true, hovered, true, true)
                };
                // The DEFAULT icon class: the button's own per-state `icon_color` (on-accent,
                // from the style above) colours the glyph, exactly as the kind pair does. An
                // `Svg::Custom` accent class here would paint accent-on-accent — invisible.
                return crate::widgets::arrow_cursor::arrow_cursor(
                    widget::button::custom(mode_icon(name, false))
                        .class(cosmic::theme::Button::Custom {
                            active: Box::new(move |_, t| seg_style(t, false)),
                            disabled: Box::new(move |t| seg_style(t, false)),
                            hovered: Box::new(move |_, t| seg_style(t, true)),
                            pressed: Box::new(move |_, t| seg_style(t, true)),
                        })
                        .on_press_maybe(msg)
                        .width(btn_width)
                        .padding(BTN_PAD),
                );
            }
            // Natural padding keeps the icon at its proper size (forcing the height
            // scaled/clipped it); the width is fixed horizontally and fills when
            // stacked so the buttons share the group evenly.
            crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::custom(mode_icon(name, true))
                    .selected(active)
                    .class(cosmic::theme::Button::Icon)
                    .on_press_maybe(msg)
                    .width(btn_width)
                    .padding(BTN_PAD),
            )
        };
        // Photo/video: a SEGMENTED pair (one control, two joined halves) rather than
        // two free-standing buttons — the active half is filled accent with an
        // on-accent glyph, the other half sits flat on the group with a subdued
        // glyph, and only the pair's outer corners are rounded.
        let kind_btn = |name: &'static str, active: bool, msg: Msg, round_left: bool, round_right: bool, enabled: bool| {
            // Default icon class: the button's per-state `icon_color` (below) colours
            // it, so the glyph can react to hover — an Svg::Custom class can't see
            // hover state.
            let icon = crate::widgets::icons::sized(name, ICON_BOX);
            // The shared segmented-pair style (theme.rs) — one source for this
            // pair and the preview's pointer/razor toggle.
            let seg_style = move |t: &cosmic::Theme, hovered: bool| {
                crate::app::theme::segment_style(t, active, hovered, round_left, round_right)
            };
            crate::widgets::arrow_cursor::arrow_cursor(
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
                // DRAGON-322: `enabled=false` (video kind while another instance records)
                // leaves NO on-press handler, so the segment is inert (the disabled class
                // renders it in the subdued/inactive look — reads as not-selectable).
                .on_press_maybe(enabled.then_some(msg))
                .width(btn_width)
                .padding(BTN_PAD),
            )
        };
        // Neutral icon button (settings/close) — same footprint as a mode button.
        let action_btn = |name: &'static str, msg: Msg| {
            crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::custom(mode_icon(name, false))
                    .class(cosmic::theme::Button::Icon)
                    .on_press(msg)
                    .width(btn_width)
                    .padding(BTN_PAD),
            )
        };
        // DRAGON-460: a SELECTOR-looking button that isn't tied to a `Mode` — the accent
        // glyph and footprint of the region/window/monitor buttons in their inactive
        // state, which is the look the scanner's refresh has to match to read as one of
        // them. Deliberately not `action_btn` (that is the subdued settings/close face)
        // and not `mode_btn` (that one's message is derived from a `Mode` it would have
        // to be given a fake of).
        let mode_icon_btn = |name: &'static str, msg: Msg| {
            crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::custom(mode_icon(name, true))
                    .class(cosmic::theme::Button::Icon)
                    .on_press(msg)
                    .width(btn_width)
                    .padding(BTN_PAD),
            )
        };
        // The selector-group shell: background + rounding, shared by the mode group and
        // (DRAGON-460) the scanner's refresh group that stands in its place. Extracted
        // rather than copied so the two can never drift into looking different — the
        // whole point of the refresh group is that it reads as one of these.
        let group_class = || {
            cosmic::theme::Container::Custom(Box::new(|theme| {
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
            }))
        };
        let mode_group = widget::container(
            widget::row(vec![
                mode_btn(
                    "screenshot-selection-symbolic",
                    Mode::Region,
                    self.mode == Mode::Region,
                ),
                mode_btn(
                    "screenshot-window-symbolic",
                    Mode::Window,
                    self.mode == Mode::Window,
                ),
                mode_btn(
                    "screenshot-screen-symbolic",
                    Mode::Monitor,
                    self.mode == Mode::Monitor,
                ),
            ])
            .spacing(2.0)
            .width(row_width)
            .align_y(Alignment::Center),
        )
        .width(group_width)
        .align_x(Alignment::Center)
        .padding(GROUP_PAD)
        .class(group_class());

        // DRAGON-460: the scanner's own selector group — a re-read of the screen, sitting
        // exactly where the region/window/monitor group sits in every other kind and
        // wearing the same shell, because it is the same class of control: the thing you
        // reach for to change what is about to be captured.
        //
        // It REPLACES the mode group rather than joining it. Scanning pins `Mode::Region`
        // (there is nothing to pick), so that group is hidden in scanner kind and this one
        // takes the slot — the toolbar keeps its shape instead of losing a group.
        //
        // This is also what retires DRAGON-456's hover face. That swapped the scan
        // segment's glyph to the refresh icon on hover, which meant the only thing telling
        // you the button had changed meaning was already having your pointer on it. A
        // visible button says it without being hunted for, and the face/action rule
        // DRAGON-456 set is kept trivially: this button is only built while the scanner is
        // active, so a press is always a refresh.
        // While a scan is in flight the button IS the progress indicator: the SAME refresh
        // glyph turns, tinted to the off/disabled wash, and takes no press.
        //
        // The glyph itself rotates rather than being swapped for a spinner widget. Swapping
        // changes the button's face mid-interaction, and libcosmic's `indeterminate_circular`
        // also exposes no style hook (its stylesheet style is `()`), so it would render at
        // accent and could not be tinted to match a disabled control. Rotating our own SVG
        // keeps both the identity and the colour under our control.
        //
        // Disabling is not decoration: `begin_scan_shot` ignores a press that arrives
        // mid-shot, so an enabled-looking button would silently do nothing.
        //
        // This replaces the small spinner that used to sit inset in the selection's
        // bottom-right corner. That badge could be clipped off-screen by a region drawn at a
        // display edge, said only that something was happening rather than which control was
        // busy, and — since the scanner now reads the selection — sat INSIDE the very crop it
        // was reporting on.
        let scanning = self.scanning();
        let refresh_btn: Element<'_, Msg> = if scanning {
            // The same subdued wash the mic/speaker toggles wear when they are off, so
            // "unavailable" reads identically across the whole toolbar.
            let spin_class = cosmic::theme::Svg::Custom(Rc::new(|t: &cosmic::Theme| {
                cosmic::widget::svg::Style { color: Some(state_mix(t, MIX_OFF)) }
            }));
            crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::custom(
                    widget::container(crate::widgets::icons::sized_rotated(
                        "scan-refresh-symbolic",
                        ICON_BOX,
                        self.scan_spin,
                        spin_class,
                    ))
                    .width(Length::Fill)
                    .align_x(Alignment::Center),
                )
                // No `on_press` => disabled: the press is refused by the model anyway.
                .class(cosmic::theme::Button::Icon)
                .width(btn_width)
                .padding(BTN_PAD),
            )
        } else {
            mode_icon_btn("scan-refresh-symbolic", Msg::Capture(CaptureMsg::SetKind(Kind::Scanner)))
        };
        let scan_group = widget::container(
            widget::row(vec![refresh_btn]).spacing(2.0).align_y(Alignment::Center),
        )
        .align_x(Alignment::Center)
        .padding(GROUP_PAD)
        .class(group_class());

        // DRAGON-460: the scan segment is a KIND selector again, and only that. It keeps
        // its glyph in every state and carries no hover tracking.
        //
        // DRAGON-456 had it swap to the refresh glyph on hover while the scanner was
        // already active, because a press in that state re-reads the screen rather than
        // switching kind. The rule was sound and is unchanged — `scan_press_refreshes`
        // still governs what the press DOES — but a face that only appears under the
        // pointer cannot advertise anything. The refresh now has its own visible button in
        // `scan_group`, so the segment no longer has to hint at a second meaning.
        let scan_seg: Element<'_, Msg> = kind_btn(
            "document-properties-symbolic",
            self.kind == Kind::Scanner,
            Msg::Capture(CaptureMsg::SetKind(Kind::Scanner)),
            true,
            false,
            true,
        );

        // Kind toggle: camera (image) | video. Recording isn't wired up yet, but
        // the toggle is live (mirrors the bottom toolbar).
        let kind_pair: Element<'_, Msg> = widget::row(vec![
            // Scanner kind: captures as a photo, and the only kind QR/OCR runs in.
            scan_seg,
            kind_btn(
                "camera-photo-symbolic",
                self.kind == Kind::Image,
                Msg::Capture(CaptureMsg::SetKind(Kind::Image)),
                false,
                false,
                true,
            ),
            kind_btn(
                "camera-video-symbolic",
                self.kind == Kind::Video,
                Msg::Capture(CaptureMsg::SetKind(Kind::Video)),
                false,
                true,
                // DRAGON-322: disabled while another instance is recording (only one
                // recording at a time; still image capture stays available).
                crate::instance::video_capture_allowed(self.external_recording),
            ),
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
        .interaction(cosmic::iced::mouse::Interaction::Idle);

        let delay_el: Element<'_, Msg> = if self.delay_menu_open && !active {
            let items: Vec<Element<'_, Msg>> = DELAYS
                .iter()
                .enumerate()
                .map(|(i, (_, s))| {
                    crate::widgets::arrow_cursor::arrow_cursor(
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
                        .class(cosmic::theme::Button::Text),
                    )
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
                action_btn(pause_icon, Msg::Recording(RecordingMsg::TogglePause)),
                delay_el,
                // Delete glyph (not a plain close): cancelling DISCARDS the recording,
                // matching the preview's delete button.
                action_btn("edit-delete-symbolic", Msg::Recording(RecordingMsg::CancelRecording)),
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
                action_btn("emblem-system-symbolic", Msg::WindowChrome(WindowChromeMsg::OpenGear)),
                action_btn("window-close-symbolic", Msg::WindowChrome(WindowChromeMsg::Quit)),
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
            let icon = crate::widgets::icons::sized(name, ICON_BOX)
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
            let btn = crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::custom(
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
                .padding(BTN_PAD),
            );
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
            // DRAGON-460: one selector group occupies this slot in every kind — the
            // mode group normally, the scanner's refresh in its place while scanning.
            if self.kind == Kind::Scanner {
                g.push(scan_group.into());
            } else {
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
