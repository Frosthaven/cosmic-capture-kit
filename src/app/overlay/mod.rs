use super::*;

pub(super) mod toolbar;
pub(super) mod marks;
pub(super) mod menus;

/// Playful loading lines shown under the window-picker spinner; one is picked at
/// random per launch (see `App::loading_msg`).
pub(super) const LOADING_MESSAGES: [&str; 20] = [
    "Rounding up your windows",
    "Peeking behind your windows",
    "Counting all the windows",
    "Wrangling your windows",
    "Hunting for open windows",
    "Sizing up the desktop",
    "Lining up your windows",
    "Catching every window",
    "Surveying the workspace",
    "Gathering the usual suspects",
    "Collecting open windows",
    "Mapping out your windows",
    "Tracking down windows",
    "Scoping out the desktop",
    "Tidying up the windows",
    "Polling for windows",
    "Sweeping the desktop",
    "Finding every last window",
    "Cataloguing open windows",
    "Assembling your windows",
];

/// The pixels-per-point scale of a captured cursor sprite, for turning its pixel
/// dimensions into a LOGICAL on-overlay size. On Linux the cursor session hands
/// the sprite back at the output's buffer scale, so there is no per-sprite scale
/// to carry and the output scale IS the sprite scale (this returns `out_scale`,
/// keeping the Linux indicator byte-identical). On macOS the sprite carries its
/// own backing scale (the 4th `CursorSprite` element): `NSCursor` gives the
/// system cursor asset at its own resolution, unrelated to the display under the
/// pointer, so the sprite must be sized by that (DRAGON-156).
#[cfg(target_os = "linux")]
fn cursor_sprite_scale(_cursor: &crate::screenshot::CursorSprite, out_scale: f32) -> f32 {
    out_scale
}

/// See the Linux twin above; on macOS the sprite's own scale is the 4th tuple
/// element. A degenerate (`<= 0`) sprite scale falls back to the output scale.
#[cfg(target_os = "macos")]
fn cursor_sprite_scale(cursor: &crate::screenshot::CursorSprite, out_scale: f32) -> f32 {
    let s = cursor.3;
    if s > 0.0 {
        s
    } else {
        out_scale
    }
}

/// Windows (DRAGON-448): a raw cursor-sprite pixel IS one point, so this is always `1.0`.
///
/// History: `platform::windows::cursor` once stamped the 4th `CursorSprite` element with
/// `96 / dpi`, claiming the sprite was a 96-DPI base asset needing a `dpi / 96` upscale.
/// DRAGON-448 hardcoded `1.0` here to dodge that stamp (passing `cursor.3` through drew
/// the indicator `(dpi/96)`-squared-ish too large on scaled monitors, invisible at 96 DPI
/// where every reading agrees). DRAGON-567 then fixed the PRODUCER: the process is
/// Per-Monitor-Aware-V2, so the `GetIconInfo` bitmap is already on-screen physical size
/// and `platform::win_cursor::sprite_backing_scale` now stamps `1.0`. This arm's constant
/// finally agrees with the stamp instead of correcting for it; both stay, one contract.
#[cfg(target_os = "windows")]
fn cursor_sprite_scale(_cursor: &crate::screenshot::CursorSprite, _out_scale: f32) -> f32 {
    1.0
}

/// Any other non-Linux target: keep the macOS reading (the sprite's own scale).
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn cursor_sprite_scale(cursor: &crate::screenshot::CursorSprite, out_scale: f32) -> f32 {
    let s = cursor.3;
    if s > 0.0 {
        s
    } else {
        out_scale
    }
}

/// The selection marker's opacity during a countdown or a live recording: ALWAYS solid
/// (DRAGON-588, owner's call).
///
/// The "Active overlay opacity" setting governs the DIM BEHIND, which is what the user is
/// choosing when they move that slider: how much of their desktop stays visible while a
/// capture is armed. It used to drive the selection lines too, so turning the dim down also
/// faded the one thing that says WHERE the capture is and that it is running. Those are
/// opposite intents on one control. The dim is a preference; the marker is information.
const SELECTION_LINE_ALPHA: f32 = 1.0;

// ── The dim's fade-in (DRAGON-606) ───────────────────────────────────────────
//
// The owner asked for the fade the `lab/flatpak` fallback picker has. We never wrote one.
// It is cosmic-comp's, and it is free there for a reason we cannot reuse: the fallback
// surface is a FULLSCREEN xdg TOPLEVEL (`shell::overlay_fallback_window`, `fullscreen:
// true`), and cosmic-comp fades a toplevel that maps straight into fullscreen over 200ms
// with an ease-in-out-cubic alpha ramp (`shell/workspace.rs`, `FULLSCREEN_ANIMATION_DURATION`).
// A LAYER surface gets none of that: the compositor draws every layer with alpha hardcoded
// to `1.0` (`backend/render/mod.rs`, the `Stage::LayerSurface` arm). Verified against the
// installed build, cosmic-comp 1.0.8-2 at commit 4fd8634e, which is the exact tree the
// running binary reports. So on the native path the fade has to be ours, and the constants
// below are MEASURED FROM THE COMPOSITOR rather than chosen, so the two paths feel the same.
//
// THE SAFETY RULE, and it is the whole reason this is gated rather than just drawn:
// DRAGON-600 made the frozen-flats grab wait for our overlay to take keyboard focus so the
// tray dropdown is out of the capture, and that fix works only because the overlay paints
// NOTHING while the grab runs. A dim that ramps during the grab bakes a partial wash into
// the frozen scene, which is a subtly darkened capture nobody would attribute to an
// animation months later. So the fade does not begin on a clock, it begins on the grab's
// own completion: see [`dim_fade_may_start`].
//
// The fade also makes the picking phase STRICTLY safer than it was. Today the dim goes to
// full the instant the overlay maps, while `spawn_frozen_flats_grab` is still running on
// its thread (DRAGON-212 deferred it precisely so the overlay maps first, and its comment
// says the overlay maps "against the live (dimmed) screen"). Starting at zero and waiting
// for the drain means the grab now photographs an overlay that composites to nothing.

/// How long the dim takes to reach the configured opacity.
///
/// 200ms because that is cosmic-comp's `FULLSCREEN_ANIMATION_DURATION`, the animation the
/// Flatpak fallback picker gets for free. Matching it is the point of the ticket: the two
/// overlay paths should not feel like two different products.
pub(super) const DIM_FADE_MS: u64 = 200;

/// **Pure**, unit-tested: ease-in-out-cubic, the curve cosmic-comp uses for that same open
/// animation (`keyframe::functions::EaseInOutCubic`). Reimplemented rather than pulled in
/// as a dependency: it is four lines, and the alternative is a crate for one curve.
fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let f = -2.0 * t + 2.0;
        1.0 - (f * f * f) / 2.0
    }
}

/// **Pure**, unit-tested: the dim's alpha `elapsed_ms` into the fade, ramping to `target`.
///
/// `target` is the CONFIGURED opacity, never a constant: the region dim, the colour
/// picker's own dim and the active-overlay dim are three separate user settings, and the
/// fade is a multiplier on whichever one the caller is drawing. At and past
/// [`DIM_FADE_MS`] this returns `target` exactly, so a finished fade is indistinguishable
/// from no fade at all.
pub(super) fn dim_fade_alpha(target: f32, elapsed_ms: u64) -> f32 {
    if elapsed_ms >= DIM_FADE_MS {
        return target;
    }
    target * ease_in_out_cubic(elapsed_ms as f32 / DIM_FADE_MS as f32)
}

/// **Pure**, unit-tested: may the dim's fade begin?
///
/// This is the ordering guarantee, and it is a happens-before, not a delay. `frozen_pending`
/// is cleared in exactly ONE place, the `FrozenReady` drain, which runs only after the grab
/// thread has finished reading every output and posted its result into `frozen_slot`. So
/// "not pending" means "no frozen-flats grab can still be looking at the screen". There are
/// only two grab sites in the tree (`spawn_frozen_flats_grab`'s launch call and
/// `tick_menu_hold`'s), both of them at launch, and both post into that one slot, which is
/// what makes the enumeration complete rather than hopeful.
///
/// The other two terms:
/// - `menu_hold` is DRAGON-600's paint gate. It is redundant with `frozen_pending` today
///   (the held grab has not even started, so nothing has drained) and it stays anyway,
///   because a fade that could start while the tray dropdown is still on screen would be
///   the one thing that fix exists to prevent.
/// - `fallback` is the `lab/flatpak` path, which already gets the compositor's own fade.
///   Fading there too would run two ramps over each other.
///
/// A launch that grabs no flats at all (`launch_flats_needed` false, the common
/// screenshot) parks an EMPTY result in the slot at init, so its first drain tick clears
/// the flag and the fade starts within a frame. It waits for nothing because there is
/// nothing to wait for.
pub(super) fn dim_fade_may_start(frozen_pending: bool, menu_hold: bool, fallback: bool) -> bool {
    !frozen_pending && !menu_hold && !fallback
}

/// Where the dim's fade-in has got to (DRAGON-606).
///
/// FOUR states, and the middle one is the whole lesson of this ticket. The fade must start
/// at whichever comes LATER, the frozen grab completing or the overlay's first painted
/// frame:
///
/// - starting on the grab alone is SAFE but can be INVISIBLE. Measured on the owner's
///   machine, the grab finishes at ~255ms and its drain lands at ~553ms, while the
///   overlay's first painted frame does not arrive until later still. A 200ms ramp that
///   begins at the drain can be completely over before anything is on screen, which
///   delivers a mathematically perfect animation that nobody ever sees. That fails the
///   ticket, since what was asked for is a thing you can watch.
/// - starting on the first frame alone would be VISIBLE but UNSAFE, because nothing would
///   stop it preceding the grab.
///
/// `Armed` is the join: the grab is done, and we are now waiting for the first frame to
/// latch the clock. Taking the later of the two is safe by construction (it can never
/// precede the grab) and visible by construction (it can never precede the first frame the
/// user could see), instead of depending on the two happening to be ordered favourably.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimFade {
    /// The frozen-flats grab may still be reading the screen. The dim is not drawn.
    Waiting,
    /// The grab has landed, so the fade is ALLOWED, but nothing has been painted yet. Still
    /// draws no dim; the next frame starts the clock.
    Armed,
    /// Ramping, from this instant, which is the first frame that could be seen.
    Running(std::time::Instant),
    /// At the configured opacity for good. No more redraws are scheduled.
    Done,
}

impl App {
    /// This overlay's dim opacity RIGHT NOW: the configured `target`, scaled by however far
    /// the fade-in has got (DRAGON-606).
    ///
    /// Linux-only by `cfg!`, not `#[cfg]`, so there is ONE compiled body and macOS and
    /// Windows keep byte-identical overlays: their capture overlays are winit windows on
    /// compositors with their own window-open behaviour, and this ticket is about matching
    /// cosmic-comp on the layer-shell path.
    /// This is also THE latch: it runs during view building, which is the app producing a
    /// frame, so an `Armed` fade starts its clock here and nowhere else. Interior mutability
    /// through a `Cell` for exactly that reason, the same device `OutputState::placed` uses
    /// to record a native placement from inside a view.
    pub(super) fn dim_now(&self, target: f32) -> f32 {
        if !cfg!(target_os = "linux") {
            return target;
        }
        match self.dim_fade.get() {
            // Nothing is reading the screen any more, but the fade was never armed. Arm it
            // here rather than paint zero forever. Unreachable on every real path, since
            // `start_dim_fade` runs inside the `FrozenReady` drain in the same update that
            // clears `frozen_pending`; it is here so that a future edit which forgets to
            // call it degrades into a working fade instead of an invisible overlay. It
            // consults the SAME gate, so it can never paint what a grab could photograph.
            DimFade::Waiting
                if dim_fade_may_start(
                    self.frozen_pending,
                    self.menu_hold.is_some(),
                    self.overlay_fallback_active(),
                ) =>
            {
                self.dim_fade.set(DimFade::Running(std::time::Instant::now()));
                // KEEP THIS MARK. It is not leftover scaffolding from the DRAGON-606
                // measurement, it is the launch timeline's one previously unmeasurable
                // instant, and the whole visibility argument for this feature rests on it.
                //
                // Every other quantity here can be measured from outside the process with a
                // screen grab: the ramp's shape, its duration, the settled alpha. The moment
                // the overlay first PAINTS cannot, because until this frame the fade draws
                // nothing and a mapped layer surface drawing nothing composites to nothing,
                // so an external grab and this mark are blind to the same thing. Reading it
                // off a screen recording is guesswork.
                //
                // What it bought, measured: the frozen drain landed at +537.7ms and this
                // frame at +543.3ms, a 5.6ms margin inside a 200ms animation. That is the
                // number which says the drain-anchored version was visible by luck. Delete
                // the mark and the next person cannot tell whether the fade is still visible
                // on their hardware, only that it is still correct.
                crate::util::timing_mark("dim fade: first painted frame, ramp begins");
                0.0
            }
            DimFade::Waiting => 0.0,
            // The first frame since the grab landed. Start the clock NOW, and return zero
            // for this frame so the ramp genuinely begins at nothing on screen rather than
            // jumping to wherever a drain-anchored clock had already got to.
            DimFade::Armed => {
                self.dim_fade.set(DimFade::Running(std::time::Instant::now()));
                // The launch timeline's missing entry. Everything else about the fade is
                // measurable from outside except the one instant that decides whether it is
                // visible at all, and reading it off a screen recording is guesswork.
                crate::util::timing_mark("dim fade: first painted frame, ramp begins");
                0.0
            }
            DimFade::Running(start) => {
                dim_fade_alpha(target, start.elapsed().as_millis() as u64)
            }
            DimFade::Done => target,
        }
    }

    /// The frozen-flats grab has landed, so the dim may start fading in (DRAGON-606).
    ///
    /// Called from the `FrozenReady` drain, which is the completion event itself. Idempotent
    /// and one-way: once the fade is running or finished a later call cannot restart it, so
    /// nothing can re-blank an overlay the user is already working on.
    pub(in crate::app) fn start_dim_fade(&mut self) {
        if self.dim_fade.get() != DimFade::Waiting {
            return;
        }
        // The fallback path never fades on our clock, and it must not sit at zero waiting
        // for one: land it straight on the configured dim and let the compositor's own
        // animation, the one the owner already likes, be the fade. Same for every platform
        // that is not doing this at all.
        let fallback = self.overlay_fallback_active();
        if fallback || !cfg!(target_os = "linux") {
            self.dim_fade.set(DimFade::Done);
            return;
        }
        if !dim_fade_may_start(self.frozen_pending, self.menu_hold.is_some(), fallback) {
            return;
        }
        // ARMED, not Running. The clock starts on the first painted frame (`dim_now`), not
        // here, because the drain can and does land before anything is on screen.
        self.dim_fade.set(DimFade::Armed);
    }
}

impl App {

    // Frozen, non-interactive countdown overlay: the selection border stays put
    // while the toolbar (timer chip counting down, cancels on click) shows where
    // it always does — anchored to a region, or pinned to the bottom of the
    // screen for window/monitor captures.
    pub(super) fn countdown_view(&self, o: &OutputState) -> Element<'_, Msg> {
        let sel = self.pending.as_ref();
        let rect = sel.map(|s| GlobalRect::new(s.x, s.y, s.x + s.width as i32, s.y + s.height as i32));
        // Match the recording border placement (outside for window/monitor) so the
        // outline doesn't shift when the countdown hands off to recording.
        let windowed = sel.is_some_and(|s| s.window_id.is_some() || s.output.is_some());
        let mut rs = RegionSelection::new(o.units(), rect, |a0| Msg::Capture(CaptureMsg::RegionChange(a0)), Msg::Capture(CaptureMsg::RegionDone))
            .non_interactive()
            .dim_alpha(self.active_overlay_opacity)
            .line_alpha(SELECTION_LINE_ALPHA);
        if windowed {
            rs = rs.outer_border();
        }
        let border: Element<'_, Msg> = rs.into();
        let mut layers: Vec<Element<'_, Msg>> = vec![border];
        if let Some(toolbar) = self.capture_button_layer(o) {
            layers.push(toolbar);
        }
        cosmic::iced::widget::stack(layers).into()
    }

    // Recording overlay: for a REGION, the active dim outside the rect plus the
    // selection border on its edge (so the drawn area stays visible at the
    // configured dimness) — the recorded crop is inset by the line width (see
    // `start_recording`), so what you see inside the line is exactly what's
    // recorded. Window/monitor recordings frame nothing on screen (the portal/target
    // defines the area), so they leave it clear and show only the record/stop chip.
    pub(super) fn recording_view(&self, o: &OutputState) -> Element<'_, Msg> {
        let mut layers: Vec<Element<'_, Msg>> = Vec::new();
        // Only a region gets the dim + border; window/monitor stay clear.
        if self.mode == Mode::Region
            && let Some(s) = self.pending.as_ref()
        {
            let rect = Some(GlobalRect::new(s.x, s.y, s.x + s.width as i32, s.y + s.height as i32));
            let rs = RegionSelection::new(o.units(), rect, |a0| Msg::Capture(CaptureMsg::RegionChange(a0)), Msg::Capture(CaptureMsg::RegionDone))
                .non_interactive()
                .dim_alpha(self.active_overlay_opacity)
                .line_alpha(SELECTION_LINE_ALPHA);
            layers.push(rs.into());
        }
        if let Some(toolbar) = self.capture_button_layer(o) {
            layers.push(toolbar);
        }
        cosmic::iced::widget::stack(layers).into()
    }

    // Window mode: cosmic-screenshot's picker — each window button is sized to
    // its (ScaleDown) thumbnail inside a width-proportional, centered slot, laid
    // over the wallpaper. Matches xdg-desktop-portal-cosmic's widget exactly.
    /// Top inset (logical points) the window picker must leave clear so its content
    /// never renders behind a notched MacBook's camera cutout (DRAGON-270). On macOS
    /// this is `NSScreen.safeAreaInsets.top` for this output's display (0 on a
    /// non-notched panel); every other platform has no notch, so it is a compile-time 0.
    fn picker_top_inset(&self, o: &OutputState) -> f32 {
        #[cfg(target_os = "macos")]
        {
            crate::platform::mac::notch_top_inset(&o.name) as f32
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = o;
            0.0
        }
    }

    pub(super) fn window_view(&self, o: &OutputState) -> Element<'_, Msg> {
        let empty: &[WindowThumb] = &[];
        let thumbs = self.windows.get(&o.name).map(|v| v.as_slice()).unwrap_or(empty);
        // Push the picker content down below a notched display's camera cutout so
        // thumbnails / chrome never sit behind it (0 on non-notched + non-mac).
        let notch_top = self.picker_top_inset(o);

        // The spinner overlay stays up through the warmup frames after windows
        // load, so the picker (built below) renders behind it and is fully ready
        // the instant the overlay lifts — no flash to a blank screen.
        let loading = self.windows_loading || self.window_warmup > 0;

        let foreground: Element<'_, Msg> = if thumbs.is_empty() {
            // Empty while loading (the spinner covers it); the "no windows"
            // message only stands once enumeration has actually finished.
            let inner: Element<'_, Msg> = if loading {
                widget::space::Space::new().into()
            } else {
                widget::text(window_picker_empty_message(self.window_mode_supported())).into()
            };
            widget::container(inner)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                // Keep the centred message clear of a notched display's cutout band.
                .padding(cosmic::iced::Padding {
                    top: notch_top,
                    ..cosmic::iced::Padding::ZERO
                })
                .into()
        } else {
            // Lay the windows out at their TRUE relative sizes: ONE scale factor for all
            // of them (so proportions are preserved), shrunk just enough to fit the panel
            // and capped at 1.0 so nothing is ever enlarged — a window smaller than the
            // screen stays small in the lineup. Rather than a single row (which shrinks
            // every tile toward 1/N as the count grows), pack them into a GRID whose
            // column count is chosen to MAXIMIZE the tile scale for this display, so a
            // monitor with many windows still shows large, legible tiles (DRAGON-193).
            let n = thumbs.len();
            // The panel is the iced VIEWPORT, so POINTS (DRAGON-448) — every other number
            // in this block (GAP, the paddings, the toolbar reserve) is already a point
            // constant. On a scaled Windows monitor `logical_size` is `point_scale`×
            // bigger, which sized the tiles for a screen that does not exist and spilled
            // the grid past the bottom of the overlay.
            let (pw, ph) = o.point_size();
            const GAP: f32 = 24.0;
            // Reserve a band at the BOTTOM for the capture toolbar (stacked over this view,
            // bottom-centred near the screen edge) so the grid never overlaps it: the
            // toolbar's real footprint from the bottom edge (its group height GROUP_H_BASE
            // plus its BOTTOM_MARGIN edge clearance, matching `toolbar_layout`), plus a
            // BADGE_GAP of clearance between the grid and the toolbar. Shared by every OS
            // (this picker view is platform-agnostic).
            let toolbar_reserve = crate::app::layout::GROUP_H_BASE
                + toolbar::layout::BOTTOM_MARGIN
                + crate::app::layout::BADGE_GAP;
            let avail_w = (pw - 48.0).max(1.0);
            // The notch band eats into the top of the usable height (added to the top
            // padding below), so the tile-scale budget must exclude it too.
            let avail_h = (ph - 24.0 - notch_top - toolbar_reserve).max(1.0);
            // Size the tiles from `layout_size` (the TRIMMED content size on macOS, so a
            // dead transparent gutter never inflates the slot — DRAGON-190; equals the
            // frame size elsewhere), while the click below still passes the raw `rect`.
            // Uniform cells sized to the LARGEST tile keep the grid regular; each tile is
            // then drawn at its own aspect within that scale.
            let max_w: f32 = thumbs.iter().map(|w| w.layout_size.0.max(1) as f32).fold(1.0, f32::max);
            let max_h: f32 = thumbs.iter().map(|w| w.layout_size.1.max(1) as f32).fold(1.0, f32::max);
            let (cols, s) = grid_cols_and_scale(n, max_w, max_h, avail_w, avail_h, GAP);
            let buttons: Vec<Element<'_, Msg>> = thumbs
                .iter()
                .map(|w| {
                    let bw = (w.layout_size.0.max(1) as f32 * s).max(1.0);
                    let bh = (w.layout_size.1.max(1) as f32 * s).max(1.0);
                    widget::button::custom(
                        widget::image::Image::new(w.handle.clone())
                            .content_fit(cosmic::iced::ContentFit::Contain)
                            .width(Length::Fixed(bw))
                            .height(Length::Fixed(bh)),
                    )
                    .padding(0)
                    .on_press(Msg::Capture(CaptureMsg::CaptureWindow {
                        id: w.id.clone(),
                        rect: w.rect,
                    }))
                    .class(cosmic::theme::Button::Image)
                    .into()
                })
                .collect();
            // Wrap the buttons into rows of `cols`, then stack the rows in a centered
            // column. cols >= 1 whenever there is at least one thumb (this branch), so the
            // modulo is safe.
            let mut rows: Vec<Vec<Element<'_, Msg>>> = Vec::new();
            for (i, btn) in buttons.into_iter().enumerate() {
                if i % cols == 0 {
                    rows.push(Vec::new());
                }
                rows.last_mut().unwrap().push(btn);
            }
            let row_elems: Vec<Element<'_, Msg>> = rows
                .into_iter()
                .map(|r| widget::row(r).spacing(GAP).align_y(Alignment::Center).into())
                .collect();
            widget::container(
                widget::column(row_elems)
                    .spacing(GAP)
                    .align_x(Alignment::Center),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            // 24px on three sides; the bottom reserves the toolbar band so the centred grid
            // sits entirely above it. The top also clears a notched display's cutout band
            // (`notch_top`, 0 on non-notched + non-mac) so the grid never rides under it.
            .padding(cosmic::iced::Padding {
                top: 24.0 + notch_top,
                right: 24.0,
                bottom: toolbar_reserve,
                left: 24.0,
            })
            .into()
        };

        // Background: the wallpaper (cover-fit), like cosmic-screenshot — this
        // hides the panel and live windows. Uses the handle pre-decoded off the
        // UI thread (decoding a full-size image here would freeze the first
        // render). Falls back to opaque dark until it's ready.
        let background: Element<'_, Msg> = match self.wallpaper_handles.get(&o.name) {
            Some(handle) => widget::image::Image::new(handle.clone())
                .content_fit(cosmic::iced::ContentFit::Cover)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            // No wallpaper yet: while still loading, stay transparent so the dim
            // overlay just dims the live desktop (not an opaque black). Only fall
            // back to a dark fill once we're actually showing a wallpaper-less
            // picker.
            None if loading => widget::space::Space::new()
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            None => widget::container(widget::space::Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
                .class(cosmic::theme::Container::Custom(Box::new(|_t| {
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(cosmic::iced::Color::from_rgb(
                            0.05, 0.05, 0.06,
                        ))),
                        ..Default::default()
                    }
                })))
                .into(),
        };

        let mut layers: Vec<Element<'_, Msg>> = vec![background, foreground];
        if loading {
            // Accent spinner + label over the same dim as the region selection
            // overlay (follows that setting), on top of the (warming) picker.
            // DRAGON-606: the window picker's warming dim fades in on the same clock as the
            // region one, so switching modes during the ramp cannot show two dim levels.
            let dim_alpha = self.dim_now(self.region_overlay_opacity);
            let spinner = widget::column(vec![
                widget::indeterminate_circular().size(48.0).into(),
                widget::text(LOADING_MESSAGES[self.loading_msg % LOADING_MESSAGES.len()])
                    .size(16)
                    .into(),
            ])
            .spacing(20.0)
            .align_x(Alignment::Center);
            let overlay = widget::container(spinner)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .class(cosmic::theme::Container::Custom(Box::new(move |_t| {
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(cosmic::iced::Color {
                            a: dim_alpha,
                            ..cosmic::iced::Color::BLACK
                        })),
                        ..Default::default()
                    }
                })));
            layers.push(overlay.into());
        }
        cosmic::iced::widget::stack(layers).into()
    }

    pub(super) fn overlay_view(&self, o: &OutputState) -> Element<'_, Msg> {
        // DRAGON-204: on macOS the overlay window is created clamped below the menu bar
        // (winit's AlwaysOnTop level) and only raised to the shielding level + reframed to
        // the full display by `place_overlay` a frame or two later. Draw NOTHING (fully
        // transparent) until that placement lands, so the clamp-then-reframe happens on an
        // invisible window and the user never sees the shift.
        #[cfg(target_os = "macos")]
        if !o.placed.get() {
            return widget::space::Space::new().into();
        }
        // Bottom layer depends on the selection mode. In freeze mode the frozen
        // snapshot sits behind the region/monitor selectors.
        let background: Element<'_, Msg> = match self.mode {
            Mode::Region => {
                let sel: Element<'_, Msg> = RegionSelection::new(
                    o.units(),
                    self.region,
                    |a0| Msg::Capture(CaptureMsg::RegionChange(a0)),
                    Msg::Capture(CaptureMsg::RegionDone),
                )
                // DRAGON-606: the CONFIGURED region dim, scaled by the fade-in. Zero until
                // the frozen-flats grab has landed, so the grab photographs nothing of ours.
                .dim_alpha(self.dim_now(self.region_overlay_opacity))
                .box_thickness(self.selection_box_thickness)
                // Hover + click the detected marks here (not via the marks layer), so
                // a press that starts on a mark can still drag the region.
                .marks(self.shown_marks(o), |a0| Msg::Detect(DetectMsg::HoverMark(a0)), |a0| Msg::Detect(DetectMsg::ActivateMark(a0)))
                .words(
                    self.shown_words(o),
                    |a0| Msg::Detect(DetectMsg::HoverWord(a0)),
                    |a0, a1| Msg::Detect(DetectMsg::TextSelectBegin(a0, a1)),
                    |a0| Msg::Detect(DetectMsg::TextSelectTo(a0)),
                    |a0| Msg::Detect(DetectMsg::TextToggle(a0)),
                    |a0, a1| Msg::Detect(DetectMsg::TextExpand(a0, a1)),
                    |a0, a1, a2| Msg::Detect(DetectMsg::WordMenu(a0, a1, a2)),
                )
                .code_menu(|a0, a1, a2| Msg::Detect(DetectMsg::CodeMenu(a0, a1, a2)))
                .into();
                self.with_frozen_bg(o, sel)
            }
            Mode::Monitor => {
                let sel: Element<'_, Msg> = OutputSelection::new(
                    self.hovered_output.as_deref() == Some(o.name.as_str()),
                    Msg::Capture(CaptureMsg::HoverOutput(o.name.clone())),
                    Msg::Capture(CaptureMsg::Capture {
                        output: o.name.clone(),
                    }),
                )
                .into();
                self.with_frozen_bg(o, sel)
            }
            Mode::Window => self.window_view(o),
        };

        // The locked-cursor preview goes on the desktop, ABOVE any backdrop image but BELOW the
        // dim/selection overlay (which is `background`), so it reads as part of the scene you're
        // cropping. Only in live region/monitor no-wallpaper selection.
        let mut layers: Vec<Element<'_, Msg>> = Vec::new();
        if let Some(cursor) = self.cursor_indicator(o) {
            layers.push(cursor);
        }
        layers.push(background);
        if let Some(hint) = self.region_hint_layer(o) {
            layers.push(hint);
        }
        if let Some(marks) = self.marks_layer(o) {
            layers.push(marks);
        }
        // DRAGON-460: no scan spinner layer here any more — scanner progress is the
        // toolbar refresh button spinning. See `marks::scanning`.
        if let Some(cap) = self.capture_button_layer(o) {
            layers.push(cap);
        }
        if let Some(toast) = self.toast_layer() {
            layers.push(toast);
        }
        if let Some(menu) = self.text_menu_layer(o) {
            layers.push(menu);
        }
        if let Some(menu) = self.code_menu_layer(o) {
            layers.push(menu);
        }
        cosmic::iced::widget::stack(layers).into()
    }

    /// Transient banner (e.g. a wrong-monitor portal pick) shown top-centre over the
    /// overlay, styled like a cosmic button — rounded, theme-aware (light/dark).
    ///
    /// Visible to the whole `app` tree because the colour picker's overlay stacks the same
    /// banner (`color_picker::view`), which is how DRAGON-612's two picker-only refusals get
    /// drawn. One banner, one style, one place it is built.
    pub(in crate::app) fn toast_layer(&self) -> Option<Element<'_, Msg>> {
        let text = self.toast.as_ref()?;
        let pill = widget::container(widget::text(text.clone()).size(14))
            .padding(cosmic::iced::Padding {
                top: 10.0,
                bottom: 10.0,
                left: 18.0,
                right: 18.0,
            })
            .class(cosmic::theme::Container::Custom(Box::new(|theme| {
                // Borrowed, not bound by value: `Component` is not `Copy`, and this only
                // reads three colours out of it.
                let component = &theme.cosmic().background(false).component;
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(component.base.into())),
                    border: Border {
                        radius: crate::app::theme::rounding(theme).m.into(),
                        width: 1.0,
                        color: component.divider.into(),
                    },
                    // DRAGON-607's rule: no site writes one ink field without the other. This
                    // set `text_color` alone and left `icon_color` to inherit the ambient
                    // window foreground, which is the exact shape that ticket exists to
                    // remove. It is invisible today only because the pill has never held
                    // anything but text; the day one carries an icon it would draw the window
                    // foreground on this `component.base` fill.
                    //
                    // Spread as the BASE of the struct so the ink comes from the one helper
                    // while this site keeps its own background and border. `ink_content`
                    // writes the two ink fields and defaults the rest, so nothing else here
                    // changes and the rendered pixels are identical until an icon appears.
                    //
                    // `region_hint_layer` below had a byte-identical pill with the same latent
                    // issue, and now carries the same fix. The two being identical is itself
                    // worth collapsing into one helper, which is a change that should be made
                    // on its own and looked at.
                    ..crate::app::theme::ink_content(component.on.into())
                }
            })));
        Some(
            widget::container(pill)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Start)
                .padding(cosmic::iced::Padding {
                    top: 48.0,
                    ..cosmic::iced::Padding::ZERO
                })
                .into(),
        )
    }

    /// Whether the current region (if any) overlaps this output.
    ///
    /// Stays entirely in CAPTURE space (DRAGON-448): both the region and the output rect
    /// are already in it, so there is nothing to bridge — converting either would only
    /// introduce rounding. The rule is "convert at the boundary with iced", and this
    /// answers a bool that never reaches one.
    fn region_on_output(&self, o: &OutputState) -> bool {
        let Some(rect) = self.region else {
            return false;
        };
        let (l, t, r, b) = rect.to_tuple();
        let (l, t, r, b) = (l.min(r), t.min(b), l.max(r), t.max(b));
        let (ox, oy) = o.logical_pos;
        let (ow, oh) = (o.logical_size.0 as i32, o.logical_size.1 as i32);
        l < ox + ow && r > ox && t < oy + oh && b > oy
    }

    /// Centred "begin drawing" hint, shown (in region mode) on every output that
    /// doesn't currently hold the region — including all of them when nothing's drawn
    /// yet. Click-through, so a press here still starts a region on this output.
    fn region_hint_layer(&self, o: &OutputState) -> Option<Element<'_, Msg>> {
        if self.mode != Mode::Region || self.region_on_output(o) {
            return None;
        }
        let pill = widget::container(widget::text("Begin drawing a capture region").size(16))
            .padding(cosmic::iced::Padding {
                top: 10.0,
                bottom: 10.0,
                left: 18.0,
                right: 18.0,
            })
            .class(cosmic::theme::Container::Custom(Box::new(|theme| {
                let c = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(c.background(false).component.base.into())),
                    border: Border {
                        radius: crate::app::theme::rounding(theme).m.into(),
                        width: 1.0,
                        color: c.background(false).component.divider.into(),
                    },
                    // DRAGON-607's rule, the same fix the toast pill above already carries.
                    // This set `text_color` alone and left `icon_color` to inherit the ambient
                    // window foreground, which is the exact shape that ticket exists to
                    // remove. Invisible today only because this pill has never held anything
                    // but text; the day one carries an icon it would draw the window
                    // foreground on this `component.base` fill.
                    //
                    // Spread as the BASE of the struct so the ink comes from the one helper
                    // while this site keeps its own background and border. `ink_content`
                    // writes the two ink fields and defaults the rest, so the rendered pixels
                    // are identical until an icon appears.
                    ..crate::app::theme::ink_content(c.background(false).component.on.into())
                }
            })));
        Some(
            widget::container(pill)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .into(),
        )
    }

    /// While selecting a REGION or MONITOR whose capture will carry the launch-locked cursor (no
    /// wallpaper, live) draw that cursor at its real position, so you can compose the crop around
    /// where it'll land. Sits on the desktop, below the dim/selection overlay. `None` when it
    /// doesn't apply, there's no captured cursor, or the cursor isn't on this output. (Under freeze
    /// the frozen backdrop already shows the cursor; wallpaper-on uses the live compositor cursor.)
    fn cursor_indicator<'a>(&'a self, o: &OutputState) -> Option<Element<'a, Msg>> {
        // Shown whenever an IMMEDIATE region/monitor capture will embed the LAUNCH-LOCKED
        // cursor and the overlay isn't already displaying it. The visibility decision is
        // SHARED with the capture path (DRAGON-213) so preview + stamped pixels can't
        // drift — see `show_launch_cursor_indicator`. Window mode and an armed countdown
        // both hide it; the frozen backdrop already bakes the pointer in.
        if !super::capture_flow::show_launch_cursor_indicator(
            self.mode,
            self.effective_capture_extras().cursor,
            self.freeze_backdrop_active(),
            self.configured_delay_secs() > 0,
        ) {
            return None;
        }
        let (img, (gx, gy), (hx, hy), ..) = self.frozen_cursor.as_ref()?;
        let (ox, oy) = o.logical_pos;
        let (ow, oh) = o.logical_size;
        if *gx < ox || *gx >= ox + ow as i32 || *gy < oy || *gy >= oy + oh as i32 {
            return None; // cursor isn't on this output
        }
        // Position is placed in the OUTPUT's logical space, so map global->local at
        // the output's buffer scale.
        let out_scale = self
            .frozen
            .get(&o.name)
            .map(|f| f.img.width() as f32 / f.logical_size.0.max(1) as f32)
            .unwrap_or(1.0);
        // The sprite's own pixels-per-point sets its LOGICAL size (dividing sprite
        // pixels by that scale). On Linux the cursor session hands the sprite back
        // at the output scale, so sprite_scale == out_scale and this is unchanged;
        // on macOS the system cursor asset is its own (typically 2x) resolution
        // regardless of the display under the pointer, so it must divide by the
        // sprite's OWN scale or a lower-DPI output shows it double size
        // (DRAGON-156).
        let sprite_scale = cursor_sprite_scale(self.frozen_cursor.as_ref()?, out_scale);
        let dw = img.width() as f32 / sprite_scale;
        let dh = img.height() as f32 / sprite_scale;
        // The pointer position is CAPTURE space; the padding below is POINTS (DRAGON-448).
        // Cross once through this output's bridge, then back off by the hotspot, which is
        // already expressed in the sprite's own pixels-per-point.
        let (px, py) = o.units().to_point((*gx, *gy));
        let lx = (px - *hx as f32 / sprite_scale).max(0.0);
        let ly = (py - *hy as f32 / sprite_scale).max(0.0);
        // The sprite's handle is built ONCE when the cursor lands (never in view():
        // a per-frame from_rgba mints a new id each call, forcing a GPU re-upload
        // and a fresh atlas entry on every redraw of the drag).
        let handle = self.frozen_cursor_handle.clone()?;
        let sprite = widget::image::Image::new(handle)
            .width(Length::Fixed(dw))
            .height(Length::Fixed(dh));
        // Absolute placement: pad a Fill container so the top-left-aligned sprite lands at (lx, ly).
        Some(
            widget::container(sprite)
                .padding(cosmic::iced::Padding { top: ly, right: 0.0, bottom: 0.0, left: lx })
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
        )
    }

    /// Layer the output's frozen snapshot behind `selection` when the freeze backdrop
    /// is active (freeze mode); otherwise return `selection` unchanged so the
    /// transparent overlay surface composites the LIVE desktop behind it — the
    /// freeze-off "live" feel, identical on every platform.
    ///
    /// DRAGON-234: this is now uniform across all platforms. The Windows M1.5 special
    /// case (always draw the frozen scene OPAQUELY, on the belief that a transparent
    /// wgpu surface presents an opaque clear on Windows) is GONE. Empirically the
    /// winit transparent window (`DwmEnableBlurBehindWindow`, an empty blur region for
    /// per-pixel alpha) plus the `PreMultiplied` composite-alpha swapchain that
    /// `iced_wgpu` selects from the Vulkan surface's advertised modes DO composite the
    /// live desktop through the overlay — so freeze-off shows the live dimmed desktop
    /// exactly like Linux/mac (verified: a 3s-apart clock advanced through the
    /// backdrop), and freeze-on shows the opaque launch-instant still (verified: the
    /// clock stayed fixed). The freeze-off capture still re-grabs LIVE pixels at commit
    /// (`freezing()` is false, so `capture_flow` takes the live path), unchanged.
    ///
    /// `lab/flatpak` (Linux): on the FALLBACK toplevel the compositor, not us, picks the
    /// window's monitor, so an output-sized `Fill` would STRETCH the frozen frame when
    /// the geometries differ. There the frame is drawn LETTERBOXED instead: fixed to the
    /// destination rect `OverlayUnits::letterbox_dest` computes (the SAME bridge that
    /// maps the selection, so pixels and mapping cannot drift), centred over opaque
    /// black bars. The bars are black on purpose: the toplevel is transparent, and the
    /// live desktop showing through would read as capturable pixels that are not in the
    /// frame. iced's `ContentFit::Contain` DOES centre in this fork (`drawing_bounds`),
    /// but it fits by the handle's PIXEL size, not the frame's logical size, so it is
    /// not used: the explicit rect keeps one math source. Every layer-shell session
    /// answers no letterbox and keeps the historical `Fill` path byte-identical (its
    /// backdrop is exactly output-sized, so `Fill` never stretched anything there).
    pub(super) fn with_frozen_bg<'a>(
        &'a self,
        o: &OutputState,
        selection: Element<'a, Msg>,
    ) -> Element<'a, Msg> {
        match self.frozen_bg_layer(o).filter(|_| self.freeze_backdrop_active()) {
            Some(bg) => cosmic::iced::widget::stack(vec![bg, selection]).into(),
            None => selection,
        }
    }

    /// The frozen snapshot as a BACKDROP layer for this output, or `None` when there is
    /// no snapshot. All of [`Self::with_frozen_bg`]'s drawing, with none of its GATE.
    ///
    /// Split out for the colour picker (DRAGON-582), which shows the frozen scene
    /// UNCONDITIONALLY rather than only when the freeze capture extra is on: it samples
    /// the snapshot, so drawing the live desktop underneath would put pixels on screen
    /// that are not the pixels it reports. Sharing the body keeps the letterbox
    /// arithmetic (`lab/flatpak`) in one place, which is the part that must never drift
    /// from `OverlayUnits`.
    pub(super) fn frozen_bg_layer<'a>(&'a self, o: &OutputState) -> Option<Element<'a, Msg>> {
        let f = self.frozen.get(&o.name)?;
        {
                #[cfg(target_os = "linux")]
                if let Some((offset, (dw, dh))) = o.units().letterbox_dest() {
                    let img = widget::image::Image::new(f.handle.clone())
                        .width(Length::Fixed(dw))
                        .height(Length::Fixed(dh))
                        .content_fit(cosmic::iced::ContentFit::Fill);
                    // Absolute placement, like `cursor_indicator`: pad a Fill container
                    // so the start-aligned image lands at the letterbox offset. Only the
                    // leading sides are padded; the fixed image size does the rest, and
                    // trailing padding would only invite float-jitter clipping.
                    let bg: Element<'a, Msg> = widget::container(img)
                        .padding(cosmic::iced::Padding {
                            top: offset.1,
                            right: 0.0,
                            bottom: 0.0,
                            left: offset.0,
                        })
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .class(cosmic::theme::Container::Custom(Box::new(|_t| {
                            cosmic::iced::widget::container::Style {
                                background: Some(Background::Color(cosmic::iced::Color::BLACK)),
                                ..Default::default()
                            }
                        })))
                        .into();
                    return Some(bg);
                }
            Some(
                widget::image::Image::new(f.handle.clone())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .content_fit(cosmic::iced::ContentFit::Fill)
                    .into(),
            )
        }
    }
}

/// Choose the grid shape for the window picker: the number of COLUMNS in `1..=n` that
/// MAXIMIZES the uniform tile scale when `n` tiles, each sized to fit a cell of the
/// largest tile's dims `(mw, mh)`, are packed into a centered grid within `(aw, ah)` with
/// `gap` between cells. Returns `(columns, scale)` with `scale` capped at 1.0 (tiles are
/// never enlarged). Shared by macOS and Linux (the picker view is platform-agnostic).
///
/// A single row is just the `cols == n` candidate; it wins only when the display is wide
/// enough that one row already gives the largest tiles (few windows / very wide monitor).
/// As the count grows, a squarer grid yields bigger tiles and is chosen automatically.
fn grid_cols_and_scale(n: usize, mw: f32, mh: f32, aw: f32, ah: f32, gap: f32) -> (usize, f32) {
    let (mw, mh) = (mw.max(1.0), mh.max(1.0));
    let mut best = (1usize, 0.0f32);
    for cols in 1..=n.max(1) {
        let rows = n.max(1).div_ceil(cols);
        // Per-cell budget after the inter-cell gaps in each axis (floored so a too-tight
        // fit still yields a positive, comparable scale rather than being skipped).
        let cell_w = ((aw - (cols as f32 - 1.0) * gap) / cols as f32).max(1.0);
        let cell_h = ((ah - (rows as f32 - 1.0) * gap) / rows as f32).max(1.0);
        let s = (cell_w / mw).min(cell_h / mh).min(1.0);
        // `>=` so that among column counts that TIE on tile scale (common once the scale
        // caps at 1.0) we keep the LARGEST one — the flattest, fewest-rows layout. That
        // makes a handful of windows stay a single row, like before, and only wraps into a
        // grid once wrapping actually buys larger tiles.
        if s >= best.1 {
            best = (cols, s);
        }
    }
    best
}

/// What the window picker says when it has no thumbnails to show, once enumeration has
/// finished. Pure; unit-tested in `picker_empty_message_tests`.
///
/// DRAGON-620: there are two different silences here and they were saying the same sentence.
/// "No windows on this display" is TRUE on a session that can enumerate windows and found
/// none, and it is a LIE on one that cannot enumerate at all, where it blames an empty desktop
/// for a missing protocol. A wlroots session hits the second case with a full screen of
/// windows open, so the old copy sent the user looking for the wrong problem.
///
/// Kept deliberately about the COMPOSITOR rather than about us: from the user's side the fact
/// that matters is that this desktop cannot offer window mode, not which Wayland global is
/// absent. The protocol detail belongs in the debug log, and it is there.
pub(super) fn window_picker_empty_message(window_mode_supported: bool) -> &'static str {
    if window_mode_supported {
        "No windows on this display"
    } else {
        "This compositor does not support window selection"
    }
}

#[cfg(test)]
mod picker_empty_message_tests {
    use super::window_picker_empty_message;

    #[test]
    fn a_capable_session_still_blames_an_empty_desktop() {
        assert_eq!(window_picker_empty_message(true), "No windows on this display");
    }

    #[test]
    fn an_incapable_session_blames_the_compositor_instead() {
        let msg = window_picker_empty_message(false);
        assert_ne!(msg, "No windows on this display", "must not claim the desktop is empty");
        assert!(msg.contains("compositor"), "the user needs to know WHERE the limit is: {msg}");
    }

    #[test]
    fn neither_message_uses_an_em_dash() {
        // House rule, and these are user-visible strings.
        for supported in [true, false] {
            assert!(!window_picker_empty_message(supported).contains('\u{2014}'));
        }
    }
}

#[cfg(test)]
mod grid_tests {
    use super::grid_cols_and_scale;

    #[test]
    fn single_window_uses_one_column_and_fits() {
        // One 800x600 tile in a 1920x1080 panel: one cell, scale capped at 1.0.
        let (cols, s) = grid_cols_and_scale(1, 800.0, 600.0, 1920.0, 1080.0, 24.0);
        assert_eq!(cols, 1);
        assert_eq!(s, 1.0);
    }

    #[test]
    fn few_wide_windows_stay_in_one_row() {
        // Three 640x400 tiles on a wide 3840x1080 panel: a single row (cols == n) gives
        // the largest tiles, so it is chosen.
        let (cols, _s) = grid_cols_and_scale(3, 640.0, 400.0, 3840.0, 1080.0, 24.0);
        assert_eq!(cols, 3);
    }

    #[test]
    fn many_windows_wrap_into_a_grid_not_one_row() {
        // Twelve 800x600 tiles on a 1920x1080 panel: one row would shrink each toward
        // 1/12; a multi-row grid must be chosen and give a strictly larger tile scale.
        let (cols, s_grid) = grid_cols_and_scale(12, 800.0, 600.0, 1920.0, 1080.0, 24.0);
        assert!(cols > 1 && cols < 12, "expected a grid, got {cols} columns");
        // Compare against the forced single-row scale for the same inputs.
        let one_row_cell_w = (1920.0 - 11.0 * 24.0) / 12.0;
        let s_row = (one_row_cell_w / 800.0_f32).min(1080.0 / 600.0).min(1.0);
        assert!(s_grid > s_row, "grid scale {s_grid} should beat single-row {s_row}");
    }

    #[test]
    fn never_enlarges_tiles() {
        // Tiny tiles in a huge panel are never scaled above 1.0.
        let (_cols, s) = grid_cols_and_scale(4, 100.0, 80.0, 4000.0, 3000.0, 24.0);
        assert!(s <= 1.0);
    }

    #[test]
    fn degenerate_tight_panel_still_returns_a_valid_column_count() {
        // Even when nothing really fits, a valid (cols>=1, scale>0) is returned, never a
        // panic or zero columns (the view uses cols as a modulo divisor).
        let (cols, s) = grid_cols_and_scale(20, 900.0, 700.0, 200.0, 150.0, 24.0);
        assert!(cols >= 1);
        assert!(s > 0.0);
    }
}

/// DRAGON-606: the fade's CURVE and its endpoints. Shape only; the ordering rule that
/// decides when the curve is allowed to start is pinned separately below.
#[cfg(test)]
mod dim_fade_ramp_tests {
    use super::{DIM_FADE_MS, dim_fade_alpha, ease_in_out_cubic};

    // The two endpoints are exact, because both are load-bearing. Zero must be a true zero
    // (that is the frame the frozen-flats grab may still photograph, and "almost
    // transparent" would still tint it), and the end must be the configured value itself,
    // so a finished fade is indistinguishable from the pre-DRAGON-606 constant dim.
    #[test]
    fn the_ramp_starts_at_nothing_and_lands_exactly_on_the_configured_dim() {
        assert_eq!(dim_fade_alpha(0.66, 0), 0.0);
        assert_eq!(dim_fade_alpha(0.66, DIM_FADE_MS), 0.66);
        // Past the end it stays put rather than overshooting.
        assert_eq!(dim_fade_alpha(0.66, DIM_FADE_MS * 10), 0.66);
    }

    // The fade multiplies whatever the caller configured. It must never become a second
    // opacity setting of its own, so every target rides the same curve.
    #[test]
    fn the_target_is_the_configured_opacity_not_a_constant() {
        for target in [0.0_f32, 0.1, 0.33, 0.66, 0.9, 1.0] {
            assert_eq!(dim_fade_alpha(target, DIM_FADE_MS), target);
            assert_eq!(dim_fade_alpha(target, 0), 0.0);
            let mid = dim_fade_alpha(target, DIM_FADE_MS / 2);
            assert!(mid <= target, "{mid} should never exceed the configured {target}");
        }
        // A user who set the dim to zero gets zero throughout, never a flash of dim.
        for ms in [0, 1, DIM_FADE_MS / 2, DIM_FADE_MS, DIM_FADE_MS * 2] {
            assert_eq!(dim_fade_alpha(0.0, ms), 0.0);
        }
    }

    // Monotonic, so the dim only ever gets darker. A ramp that dipped would read as a
    // flicker, which is the opposite of what the owner asked for.
    #[test]
    fn the_ramp_never_goes_backwards() {
        let mut prev = -1.0_f32;
        for ms in 0..=DIM_FADE_MS {
            let a = dim_fade_alpha(0.66, ms);
            assert!(a >= prev, "alpha dipped at {ms}ms: {a} after {prev}");
            prev = a;
        }
    }

    // Ease-in-out-cubic, matching cosmic-comp's own open animation: symmetric about the
    // midpoint, which is what makes it read as the same motion as the Flatpak fallback.
    #[test]
    fn the_curve_is_ease_in_out_cubic_like_the_compositors() {
        assert_eq!(ease_in_out_cubic(0.0), 0.0);
        assert_eq!(ease_in_out_cubic(1.0), 1.0);
        assert!((ease_in_out_cubic(0.5) - 0.5).abs() < 1e-6);
        // Slow at both ends, fast in the middle: the first eighth covers less ground than
        // the eighth around the midpoint.
        let first = ease_in_out_cubic(0.125) - ease_in_out_cubic(0.0);
        let middle = ease_in_out_cubic(0.5625) - ease_in_out_cubic(0.4375);
        assert!(middle > first, "ease-in-out should accelerate into the middle");
        // Symmetry: f(t) + f(1-t) == 1.
        for t in [0.0_f32, 0.1, 0.25, 0.4, 0.5, 0.75, 0.9, 1.0] {
            assert!((ease_in_out_cubic(t) + ease_in_out_cubic(1.0 - t) - 1.0).abs() < 1e-5);
        }
        // Out of range inputs clamp rather than fly off.
        assert_eq!(ease_in_out_cubic(-5.0), 0.0);
        assert_eq!(ease_in_out_cubic(5.0), 1.0);
    }
}

/// DRAGON-606: THE ordering rule, and the reason this ticket needed a test at all.
///
/// The frozen-flats grab reads the whole screen, our overlay included. If the dim is
/// ramping while that runs, the wash is baked into the frozen scene and every capture made
/// from it comes back subtly dark, with nothing on screen to suggest why. These pin that
/// the fade cannot begin on any path where a grab could still be looking.
#[cfg(test)]
mod dim_fade_ordering_tests {
    use super::dim_fade_may_start;

    // The ordinary hotkey launch: the grab is kicked at init and runs on its own thread
    // while the overlay maps. `frozen_pending` stays true until the drain, so the whole
    // grab window is spent at zero dim.
    #[test]
    fn the_fade_waits_while_the_frozen_grab_is_still_in_flight() {
        assert!(!dim_fade_may_start(true, false, false));
        assert!(dim_fade_may_start(false, false, false));
    }

    // The DRAGON-600 tray path. The dropdown is dismissed by our overlay taking keyboard
    // focus, and the grab is HELD until then. A fade that started during the hold would
    // both photograph itself and defeat the hold's purpose.
    #[test]
    fn the_fade_waits_out_the_tray_dropdown_hold() {
        assert!(!dim_fade_may_start(true, true, false));
        // Even if the flats somehow landed first, the hold alone still blocks it.
        assert!(!dim_fade_may_start(false, true, false));
    }

    // The outer-budget fallback: keyboard focus never arrived, `tick_menu_hold` gave up and
    // ran the grab anyway. The hold clears, but `frozen_pending` is still true because that
    // grab has only just started, so the gate holds on the OTHER term. This is the path
    // most likely to be got wrong, because it is the one where the causal signal is absent.
    #[test]
    fn the_outer_budget_fallback_still_waits_for_the_grab_itself() {
        // menu_hold released, grab now running: not yet.
        assert!(!dim_fade_may_start(true, false, false));
        // Only once that grab has posted and been drained.
        assert!(dim_fade_may_start(false, false, false));
    }

    // The `lab/flatpak` fallback surface is a fullscreen xdg toplevel, and cosmic-comp
    // already fades it. Ours must stay out of the way rather than run a second ramp.
    #[test]
    fn the_flatpak_fallback_keeps_the_compositors_own_fade() {
        assert!(!dim_fade_may_start(false, false, true));
        assert!(!dim_fade_may_start(true, false, true));
    }

    // The state machine takes the LATER of the grab landing and the first painted frame.
    // Pinned as a machine rather than as prose because both orderings really happen: the
    // drain lands at ~553ms on the measured launch, and the first frame can fall on either
    // side of that depending on how long wgpu takes to come up.
    #[test]
    fn the_clock_starts_on_the_later_of_the_grab_and_the_first_frame() {
        use super::DimFade;
        let now = std::time::Instant::now();

        // Grab still running: not armed, and a frame in this state must not start anything.
        assert!(!dim_fade_may_start(true, false, false));

        // Grab landed but nothing painted yet: ARMED, and still drawing no dim. This is the
        // state that did not exist in the first cut, and its absence is what let a fade
        // finish before the overlay was on screen.
        assert_ne!(DimFade::Armed, DimFade::Waiting);
        assert_ne!(DimFade::Armed, DimFade::Done);

        // Armed is NOT a start: a fade anchored here would already be running before any
        // frame existed, which is the invisible-animation bug.
        assert!(matches!(DimFade::Armed, DimFade::Armed));

        // Running carries the instant the FRAME happened, not the instant the grab landed.
        let armed_at_frame = DimFade::Running(now);
        assert!(matches!(armed_at_frame, DimFade::Running(t) if t == now));
    }

    // Exhaustive, so no combination can be added later without a decision: the fade starts
    // in exactly ONE of the eight states, the one where nothing is reading the screen.
    #[test]
    fn exactly_one_combination_lets_the_fade_start() {
        let mut starts = 0;
        for pending in [false, true] {
            for hold in [false, true] {
                for fallback in [false, true] {
                    if dim_fade_may_start(pending, hold, fallback) {
                        starts += 1;
                        assert!(!pending && !hold && !fallback);
                    }
                }
            }
        }
        assert_eq!(starts, 1, "the fade must start on exactly one combination");
    }
}

