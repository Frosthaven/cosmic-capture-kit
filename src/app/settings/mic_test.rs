//! Mic-test canvas, sensitivity bar, waveform constants, and the credit-line
//! helpers ("Powered by …") that the Audio settings page uses.
//!
//! All items here are `pub(super)` where the parent settings module needs them,
//! or private where they're only used within this file.

use super::*;

// Mic-test reference levels on the meters' (dbfs+60)/60 normalized scale; see audio-levels.md.
// The bars are the PEAK short-term RMS per bucket, so they oscillate with speech. TWO green
// zones: the ideal range where loud peaks should land (bright), and the normal range the voice
// body fluctuates through between peaks (darker — still good). Below it is too quiet; above
// ideal is too loud.
const MIC_BODY: f32 = 0.60; // -24 dBFS: floor of the NORMAL range (darker green) — a voice's
// average sits ~-24..-18 dBFS, riding up toward the ideal peaks; below this it's too quiet.
const MIC_VOICE: f32 = 0.80; // -12 dBFS: ideal range starts (bright green) / normal range ends.
const MIC_IDEAL: f32 = 0.90; // -6 dBFS: top of the ideal range — bright green ends here.
const MIC_TOO_LOUD: f32 = 0.90; // -6 dBFS: above this → red (peaks will clip).
// Fine envelope columns aggregated per bar (their PEAK, so loud moments still show);
// the inter-bar gap; how many bars span the canvas width; and how many fine columns to
// retain (must exceed VISIBLE_BARS * BAR_GROUP so the leftmost bar always has data).
// 34 bars @ the same 5 px gap make each bar ~50% wider than 45; 7 columns per bar keeps
// the ~2.4 s span and fits 34*7 = 238 within the retained columns.
const MIC_BAR_GROUP: usize = 7;
const MIC_BAR_GAP: f32 = 5.0;
const MIC_VISIBLE_BARS: usize = 34;
// Retain comfortably more than VISIBLE_BARS * BAR_GROUP (238) so the smoothing lag never
// runs the leftmost bar past the oldest retained column (which left a gap at the edge).
pub(crate) const MIC_WAVE_COLUMNS: usize = 280;
// The waveform scrolls at a CONSTANT rate against wall-clock time, displayed a fixed
// buffer behind the newest captured column. ffmpeg delivers audio in ~50 ms bursts, so a
// ~200 ms buffer absorbs that jitter (and any late render frame) and the scroll glides at
// a steady velocity. A gentle proportional term nudges the rate to hold the buffer level,
// correcting slow capture-clock drift without reacting to individual bursts.
const MIC_SCROLL_RATE: f32 = 100.0; // nominal capture rate: 480-sample frames @ 48 kHz
// Buffer ~120 ms behind real data: the measured worst-case ffmpeg delivery gap is ~50 ms
// (bursts of ~5 columns), so this covers it ~2.4x (plus render jitter) while keeping the
// meter responsive — much less latency than a conservative 200 ms. (1 col = 10 ms.)
const MIC_BUFFER_COLS: f32 = 12.0;
const MIC_DRIFT_KP: f32 = 1.0; // gentle pull toward (produced - buffer); ~1 s time constant

/// An accent-coloured, clickable package name that opens its crates.io page. A Link
/// button (so it shows the pointer cursor on hover) with caption text + no padding so
/// it sits inline with the surrounding caption.
/// The default font in italic, for the "Provided by …" credit lines (and their links).
fn italic() -> cosmic::iced::Font {
    cosmic::iced::Font {
        style: cosmic::iced::font::Style::Italic,
        ..cosmic::font::default()
    }
}

fn pkg_link(name: &'static str, url: &'static str) -> Element<'static, Msg> {
    widget::button::custom(widget::text::caption(name).font(italic()))
        .class(cosmic::theme::Button::Link)
        .padding(0)
        .on_press(Msg::WindowChrome(WindowChromeMsg::OpenUrl(url)))
        .into()
}

/// "Provided by <nnnoiseless> and <sonora>." with the package names as accent links.
pub(super) fn provided_by() -> Element<'static, Msg> {
    widget::row(vec![
        widget::text::caption("Powered by ").font(italic()).into(),
        pkg_link("nnnoiseless", "https://crates.io/crates/nnnoiseless"),
        widget::text::caption(" and ").font(italic()).into(),
        pkg_link("sonora", "https://crates.io/crates/sonora"),
        widget::text::caption(".").font(italic()).into(),
    ])
    .align_y(Alignment::Center)
    .into()
}

/// "Provided by <sonora>." — echo cancellation (AEC3) comes from sonora alone.
pub(super) fn provided_by_sonora() -> Element<'static, Msg> {
    widget::row(vec![
        widget::text::caption("Powered by ").font(italic()).into(),
        pkg_link("sonora", "https://crates.io/crates/sonora"),
        widget::text::caption(".").font(italic()).into(),
    ])
    .align_y(Alignment::Center)
    .into()
}

/// "Provided by <earshot>." — the neural VAD behind Advanced Voice Activity.
pub(super) fn provided_by_earshot() -> Element<'static, Msg> {
    widget::row(vec![
        widget::text::caption("Powered by ").font(italic()).into(),
        pkg_link("earshot", "https://crates.io/crates/earshot"),
        widget::text::caption(".").font(italic()).into(),
    ])
    .align_y(Alignment::Center)
    .into()
}

/// A legend entry: a solid rounded colour swatch + a label. `hint`, when set, shows its dBFS
/// range in a hover tooltip (so the labels stay short and uncluttered).
fn color_chip(
    color: cosmic::iced::Color,
    label: &'static str,
    hint: Option<&'static str>,
) -> Element<'static, Msg> {
    let chip = widget::row(vec![
        widget::container(
            widget::Space::new().width(Length::Fixed(14.0)).height(Length::Fixed(10.0)),
        )
        .class(cosmic::theme::Container::custom(move |t| {
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(color)),
                // The swatch follows the user's rounding rule, capped at the
                // historical 3.0 so the default look is unchanged.
                border: Border {
                    radius: crate::app::theme::rounding(t).xs.map(|x| x.min(3.0)).into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        }))
        .into(),
        widget::text::body(label).into(),
    ])
    .spacing(8.0)
    .align_y(Alignment::Center);
    match hint {
        Some(h) => widget::tooltip(chip, widget::text(h).size(12), widget::tooltip::Position::Top).into(),
        None => chip.into(),
    }
}

/// Draw one mirrored waveform bar: the raw (pre-filter) level behind in a faint subdued
/// tone — only the cap above the kept level shows, i.e. what the filters removed — and
/// the kept voice level in front, coloured by level: green in the ideal range
/// (-18..-12 dBFS), red when too loud (>= -6 dBFS), otherwise a muted tone.
/// `clean` is the already-gated voice level, so the front bar only appears when the
/// program is detecting voice (per the Input Sensitivity / filter settings). `x` is the
/// left edge.
#[allow(clippy::too_many_arguments)]
fn draw_wave_bar(
    frame: &mut cosmic::widget::canvas::Frame<cosmic::Renderer>,
    x: f32,
    bw: f32,
    cy: f32,
    half: f32,
    clean: f32,
    raw: f32,
    round: f32,
    muted: cosmic::iced::Color,
    good: cosmic::iced::Color,
    good_dim: cosmic::iced::Color,
    loud: cosmic::iced::Color,
    subdued: cosmic::iced::Color,
) {
    use cosmic::iced::{Point, Size};
    use cosmic::widget::canvas::Path;
    let mut bar = |level: f32, color: cosmic::iced::Color| {
        let bh = (level * half).max(0.5);
        // The user's rounding token, capped by the bar's own geometry (the
        // default token exceeds a bar's half-width, so "round" stays a pill).
        let r = round.min(bw / 2.0).min(bh);
        frame.fill(
            &Path::rounded_rectangle(Point::new(x, cy - bh), Size::new(bw, bh * 2.0), r.into()),
            color,
        );
    };
    // Removed-noise overlay behind: the cap of the raw level above the kept level.
    if raw > clean + 0.001 {
        bar(raw, subdued);
    }
    // Kept voice bar in front (only when voice is detected), coloured by which zone its peak
    // lands in: bright green = ideal peaks, darker green = normal body, muted = too quiet.
    if clean > 0.001 {
        let color = if clean >= MIC_TOO_LOUD {
            loud
        } else if clean >= MIC_VOICE {
            good
        } else if clean >= MIC_BODY {
            good_dim
        } else {
            muted
        };
        bar(clean, color);
    }
}

/// Canvas for the live mic-test: a centered, mirrored peak-envelope waveform (newest
/// column at the right) with horizontal reference lines at the ideal and too-loud
/// levels. Columns are coloured by level — green near/at ideal, red when too loud,
/// accent while building.
struct WaveformCanvas {
    /// The live capture buffer, read directly each vsync frame: `(clean, raw)` columns
    /// (oldest first) + the total produced count. Reading it here — rather than a cloned
    /// snapshot passed via the view — keeps the waveform fresh at the display's refresh
    /// rate without rebuilding the settings view on a fast timer (which dropped frames).
    shared: std::sync::Arc<
        std::sync::Mutex<(std::collections::VecDeque<crate::audio::clean_mic::MicColumn>, usize)>,
    >,
    dark: bool,
}

/// Per-canvas animation state: a continuous scroll position that eases toward the real
/// produced count each vsync frame, so motion is smooth, drift-free, and never steps back.
#[derive(Default)]
struct WaveState {
    pos: f32,
    last: Option<std::time::Instant>,
}

impl<M> cosmic::widget::canvas::Program<M, cosmic::Theme, cosmic::Renderer> for WaveformCanvas {
    type State = WaveState;

    /// Ease the smoothed scroll position toward the real produced count, then re-request a
    /// redraw, so the canvas re-renders at the display's refresh rate (vsync) without
    /// rebuilding the settings view. The loop stops when the modal closes (widget gone).
    fn update(
        &self,
        state: &mut WaveState,
        event: &cosmic::widget::canvas::Event,
        _bounds: cosmic::iced::Rectangle,
        _cursor: cosmic::iced::mouse::Cursor,
    ) -> Option<cosmic::widget::canvas::Action<M>> {
        use cosmic::iced::window;
        if let cosmic::widget::canvas::Event::Window(window::Event::RedrawRequested(at)) = event {
            let produced = self
                .shared
                .lock()
                .map(|g| g.1 as f32)
                .unwrap_or(state.pos + MIC_BUFFER_COLS);
            match state.last {
                // Start a buffer-length behind the newest captured column.
                None => state.pos = (produced - MIC_BUFFER_COLS).max(0.0),
                Some(prev) => {
                    // Advance at the nominal capture rate (so motion is a smooth function
                    // of wall-clock time, immune to data bursts and late frames), with a
                    // gentle proportional nudge to hold ~MIC_BUFFER_COLS behind real data
                    // (corrects clock drift). Clamp: never backwards, never past real data.
                    let dt = at.duration_since(prev).as_secs_f32().min(0.1);
                    let target = (produced - MIC_BUFFER_COLS).max(0.0);
                    let vel = (MIC_SCROLL_RATE + (target - state.pos) * MIC_DRIFT_KP).max(0.0);
                    state.pos = (state.pos + vel * dt).clamp(0.0, produced);
                }
            }
            state.last = Some(*at);
            return Some(cosmic::widget::canvas::Action::request_redraw());
        }
        None
    }

    fn draw(
        &self,
        state: &WaveState,
        renderer: &cosmic::Renderer,
        theme: &cosmic::Theme,
        bounds: cosmic::iced::Rectangle,
        _cursor: cosmic::iced::mouse::Cursor,
    ) -> Vec<cosmic::widget::canvas::Geometry<cosmic::Renderer>> {
        use cosmic::iced::{Color, Point, Size};
        use cosmic::widget::canvas::Frame;
        let mut frame = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        let cy = h / 2.0;
        let half = (h / 2.0) - 4.0;
        // The bars follow the user's COSMIC rounding rule (geometry-capped in
        // `draw_wave_bar`, so the default token keeps today's pill shape).
        let round = crate::app::theme::rounding(theme).s1();

        let good = crate::app::theme::SUCCESS;
        // A darker shade of the success green for the "normal range" zone (bars + band).
        let good_dim = Color::from_rgb(good.r * 0.6, good.g * 0.64, good.b * 0.6);
        let loud = crate::app::theme::DANGER;
        // Detected voice below the normal range reads in a muted tone (its previous
        // colour), turning green/red only in the normal / ideal / too-loud bands.
        let muted = if self.dark {
            Color::from_rgb(0.40, 0.40, 0.43)
        } else {
            Color::from_rgb(0.62, 0.62, 0.65)
        };
        // No panel background or border any more — just a faint centre baseline.
        let guide = if self.dark {
            Color::from_rgba(1.0, 1.0, 1.0, 0.08)
        } else {
            Color::from_rgba(0.0, 0.0, 0.0, 0.08)
        };
        frame.fill_rectangle(Point::new(0.0, cy - 0.5), Size::new(w, 1.0), guide);
        // Two faint bands behind the bars marking the level zones, so the target is visible even
        // in silence: brighter green for the ideal-peak range (0.80..0.90), darker green for the
        // normal range below it (0.70..0.80). Each is mirrored above and below the centre line.
        let dim_band = Color::from_rgba(good_dim.r, good_dim.g, good_dim.b, 0.12);
        let ideal_band = Color::from_rgba(good.r, good.g, good.b, 0.14);
        // Normal range (MIC_BODY..MIC_VOICE), mirrored above and below centre.
        frame.fill_rectangle(Point::new(0.0, cy - MIC_VOICE * half), Size::new(w, (MIC_VOICE - MIC_BODY) * half), dim_band);
        frame.fill_rectangle(Point::new(0.0, cy + MIC_BODY * half), Size::new(w, (MIC_VOICE - MIC_BODY) * half), dim_band);
        // Ideal-peak range (MIC_VOICE..MIC_IDEAL), mirrored.
        frame.fill_rectangle(Point::new(0.0, cy - MIC_IDEAL * half), Size::new(w, (MIC_IDEAL - MIC_VOICE) * half), ideal_band);
        frame.fill_rectangle(Point::new(0.0, cy + MIC_VOICE * half), Size::new(w, (MIC_IDEAL - MIC_VOICE) * half), ideal_band);
        // Removed-noise overlay tone: a faint ghost of the text colour, so what the
        // filters stripped reads as just-visible behind the kept voice bars.
        let subdued = if self.dark {
            Color::from_rgba(1.0, 1.0, 1.0, 0.06)
        } else {
            Color::from_rgba(0.0, 0.0, 0.0, 0.06)
        };

        // Bars are anchored to ABSOLUTE audio buckets (each bar = the PEAK of a fixed
        // group of columns), so a bar's height is fixed once its bucket completes. The
        // whole set slides left by a CONTINUOUS, time-interpolated column count (not the
        // integer snapshot), so motion is smooth at the display refresh rate instead of
        // stepping once per whole column. Bucket indexing still uses the real snapshot.
        // Snapshot the live buffer under the lock (a quick copy), so the canvas renders
        // fresh data every vsync frame with no settings-view rebuild.
        let (samples, snap) = match self.shared.lock() {
            Ok(g) => (g.0.iter().copied().collect::<Vec<_>>(), g.1 as f32),
            Err(_) => (Vec::new(), state.pos),
        };
        let src = &samples;
        let len = src.len();
        let g = MIC_BAR_GROUP as f32;
        let snap_i = snap as usize; // real columns available
        // The constant-rate scroll position from the canvas state: advanced against
        // wall-clock time a fixed buffer behind real data, so it's continuous, drift-free,
        // and only ever shows captured columns.
        let pf = state.pos;

        let pitch = (w / MIC_VISIBLE_BARS as f32).max(1.0);
        let unit = pitch / g; // px the set slides per column
        let bw = (pitch - MIC_BAR_GAP).max(1.0);
        let base = snap_i.saturating_sub(len); // absolute index of src[0] (REAL data)
        let complete_f = (pf / g).floor();
        let complete = complete_f as usize;
        let phase = pf - complete_f * g; // continuous [0, BAR_GROUP) slide progress
        // Peak (gated clean, raw) levels of a column slice.
        let peaks = |d0: usize, d1: usize| {
            src[d0..d1]
                .iter()
                .fold((0.0f32, 0.0f32), |(c, r), &(cc, rr, _)| (c.max(cc), r.max(rr)))
        };
        // Partial (still-filling) newest bucket; data capped to the real snapshot so the
        // interpolation never reads past captured columns. Slides in from the right edge.
        let pstart = complete * MIC_BAR_GROUP;
        let pend = snap_i.min(pf.floor() as usize);
        if pend > pstart && pstart >= base {
            let d0 = pstart - base;
            let d1 = (pend - base).min(len);
            if d1 > d0 {
                let (clean, raw) = peaks(d0, d1);
                let x = (w + (g - phase) * unit) - pitch + (pitch - bw) / 2.0;
                draw_wave_bar(&mut frame, x, bw, cy, half, clean, raw, round, muted, good, good_dim, loud, subdued);
            }
        }
        // Complete buckets, newest first.
        for vis in 0..=MIC_VISIBLE_BARS {
            if vis >= complete {
                break;
            }
            let a0 = (complete - 1 - vis) * MIC_BAR_GROUP;
            if a0 < base {
                break; // scrolled out of the retained columns
            }
            let x_right = w - vis as f32 * pitch - phase * unit;
            if x_right <= 0.0 {
                break; // off the left edge
            }
            let d0 = a0 - base;
            let d1 = (d0 + MIC_BAR_GROUP).min(len);
            if d1 <= d0 {
                continue;
            }
            let (clean, raw) = peaks(d0, d1);
            let x = x_right - pitch + (pitch - bw) / 2.0;
            draw_wave_bar(&mut frame, x, bw, cy, half, clean, raw, round, muted, good, good_dim, loud, subdued);
        }
        vec![frame.into_geometry()]
    }
}

/// The live mic-level bar under the manual input-sensitivity slider: a rounded track
/// with the current level filled in (green above the threshold, muted below) and a
/// threshold marker, so it reads as one unit with the slider directly above it. Both are
/// the same width, so the marker lines up with the slider handle.
struct SensitivityBar {
    /// Current mic level, 0..1 on the meter dBFS scale.
    level: f32,
    /// Gate threshold, 0..1 (the slider value).
    threshold: f32,
    dark: bool,
}

impl<M> cosmic::widget::canvas::Program<M, cosmic::Theme, cosmic::Renderer> for SensitivityBar {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &cosmic::Renderer,
        theme: &cosmic::Theme,
        bounds: cosmic::iced::Rectangle,
        _cursor: cosmic::iced::mouse::Cursor,
    ) -> Vec<cosmic::widget::canvas::Geometry<cosmic::Renderer>> {
        use cosmic::iced::{Color, Point, Size};
        use cosmic::widget::canvas::{Frame, Path};
        let mut frame = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        // The track follows the user's rounding rule, capped at half the bar
        // height (the default token gives the historical pill ends).
        let r = crate::app::theme::rounding(theme).s1().min(h / 2.0);
        let (track, muted) = if self.dark {
            (Color::from_rgb(0.16, 0.16, 0.18), Color::from_rgb(0.34, 0.34, 0.37))
        } else {
            (Color::from_rgb(0.86, 0.86, 0.88), Color::from_rgb(0.66, 0.66, 0.70))
        };
        let good = crate::app::theme::SUCCESS;
        frame.fill(
            &Path::rounded_rectangle(Point::new(0.0, 0.0), Size::new(w, h), r.into()),
            track,
        );
        let lvl_w = self.level.clamp(0.0, 1.0) * w;
        let thr_x = self.threshold.clamp(0.0, 1.0) * w;
        // Below-threshold portion of the level reads muted; above-threshold reads green
        // (that part would pass the gate).
        let below = lvl_w.min(thr_x);
        if below > 0.0 {
            frame.fill(
                &Path::rounded_rectangle(Point::new(0.0, 0.0), Size::new(below, h), r.into()),
                muted,
            );
        }
        if lvl_w > thr_x {
            frame.fill_rectangle(Point::new(thr_x, 0.0), Size::new(lvl_w - thr_x, h), good);
        }
        // Threshold marker (lines up with the slider handle above).
        let marker = if self.dark {
            Color::from_rgba(1.0, 1.0, 1.0, 0.7)
        } else {
            Color::from_rgba(0.0, 0.0, 0.0, 0.6)
        };
        frame.fill_rectangle(Point::new(thr_x - 1.0, 0.0), Size::new(2.0, h), marker);
        vec![frame.into_geometry()]
    }
}

/// Width shared by the manual sensitivity slider and the live bar beneath it, so they
/// line up as one unit.
const SENS_WIDTH: f32 = 280.0;

/// The manual input-sensitivity control: the threshold slider stacked directly above the
/// live mic-level bar, same width, so you can talk and set the slider where your voice
/// crosses the green.
pub(super) fn sensitivity_control(level: f32, threshold: f32, dark: bool) -> Element<'static, Msg> {
    widget::column(vec![
        widget::slider(0.0..=1.0, threshold, |a0| Msg::Settings(SettingsMsg::SetInputSensitivity(a0)))
            .step(0.01_f32)
            .width(Length::Fixed(SENS_WIDTH))
            .into(),
        cosmic::widget::Canvas::new(SensitivityBar { level, threshold, dark })
            .width(Length::Fixed(SENS_WIDTH))
            .height(Length::Fixed(12.0))
            .into(),
    ])
    .spacing(5.0)
    .into()
}

impl App {
    /// The live mic-test modal: a dim backdrop + a card with the rolling waveform,
    /// the ideal/too-loud reference lines + legend, and a Close button, stacked over
    /// `window`. The waveform itself is the live indicator (no textual verdict).
    /// Reference levels follow `audio-levels.md` (ideal peak -12 dBFS, too loud -6).
    pub(super) fn mic_test_modal<'a>(&'a self, window: Element<'a, Msg>) -> Element<'a, Msg> {
        use cosmic::iced::Color;
        // Hand the canvas the live shared buffer (a cheap Arc clone) so it reads fresh
        // columns itself each vsync frame instead of from a per-tick view rebuild.
        let shared = self
            .mic_test
            .as_ref()
            .map(|t| t.shared.clone())
            .unwrap_or_else(|| {
                std::sync::Arc::new(std::sync::Mutex::new((std::collections::VecDeque::new(), 0)))
            });
        let dark = super::theme_is_dark();
        let good = crate::app::theme::SUCCESS;
        let good_dim = Color::from_rgb(good.r * 0.6, good.g * 0.64, good.b * 0.6);
        let loud = crate::app::theme::DANGER;
        // A faint translucent swatch matching the removed-noise overlay's subdued tone
        // (clearly fainter than the active muted bars), nudged up just enough to read.
        let filtered = if dark {
            Color::from_rgba(1.0, 1.0, 1.0, 0.20)
        } else {
            Color::from_rgba(0.0, 0.0, 0.0, 0.18)
        };

        // Borderless, backgroundless waveform — it blends into the modal; bar colour
        // (text / green / red) carries the meaning instead of a panel + reference lines.
        let waveform = cosmic::widget::Canvas::new(WaveformCanvas { shared, dark })
            .width(Length::Fill)
            .height(Length::Fixed(275.0));

        let title: Element<'_, Msg> = widget::text::title4("Microphone test").into();
        let desc: Element<'_, Msg> = widget::text::body(
            "Speak normally. All active input filters are applied for this session.",
        )
        .into();
        // The card pads vertically only; the text / legend / button rows get their own
        // horizontal padding, while the waveform spans the full dialog width edge to edge.
        fn pad(el: Element<'_, Msg>) -> Element<'_, Msg> {
            widget::container(el).padding([0, 20]).width(Length::Fill).into()
        }
        let mut col: Vec<Element<'_, Msg>> = vec![
            pad(title),
            pad(desc),
            waveform.into(),
            pad(widget::row(vec![
                color_chip(good, "Ideal Peaks", Some("-12 to -6 dBFS")),
                color_chip(good_dim, "Normal", Some("-24 to -12 dBFS")),
                color_chip(loud, "Too Loud", Some("above -6 dBFS")),
                color_chip(filtered, "Filtered Out", None),
            ])
            .spacing(16.0)
            .into()),
        ];
        col.push(pad(widget::row(vec![
            widget::Space::new().width(Length::Fill).height(Length::Fixed(0.0)).into(),
            widget::button::standard("Close").on_press(Msg::Settings(SettingsMsg::CloseMicTest)).into(),
        ])
        .into()));
        let card = widget::container(widget::column(col).spacing(14.0))
        .padding([20, 0])
        .width(Length::Fixed(620.0))
        .class(cosmic::theme::Container::custom(|theme| {
            let c = theme.cosmic();
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(c.background.base.into())),
                border: Border {
                    color: c.bg_divider().into(),
                    width: 1.0,
                    radius: crate::app::theme::rounding(theme).s.into(),
                },
                ..Default::default()
            }
        }));

        // The backdrop swallows clicks (so the settings page behind stays inert) but
        // does NOT dismiss — only the Close button closes the test, so it can't be lost
        // with a stray click.
        let backdrop: Element<'_, Msg> = widget::mouse_area(
            widget::container(widget::Space::new().width(Length::Fill).height(Length::Fill))
                .width(Length::Fill)
                .height(Length::Fill)
                .class(cosmic::theme::Container::custom(|_t| {
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(crate::app::theme::SCRIM)),
                        ..Default::default()
                    }
                })),
        )
        .on_press(Msg::WindowChrome(WindowChromeMsg::Ignore))
        // Report a cursor interaction over the whole backdrop so the stack
        // levitates the pointer away from the settings page beneath it —
        // otherwise dropdowns and rows below the modal still light up on hover.
        .interaction(cosmic::iced::mouse::Interaction::Idle)
        .into();
        let centered: Element<'_, Msg> = widget::container(card)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        cosmic::iced::widget::stack(vec![window, backdrop, centered]).into()
    }
}
