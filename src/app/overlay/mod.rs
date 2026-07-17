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
#[cfg(not(target_os = "linux"))]
fn cursor_sprite_scale(cursor: &crate::screenshot::CursorSprite, out_scale: f32) -> f32 {
    let s = cursor.3;
    if s > 0.0 {
        s
    } else {
        out_scale
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
        let mut rs = RegionSelection::new(o.logical_pos, rect, |a0| Msg::Capture(CaptureMsg::RegionChange(a0)), Msg::Capture(CaptureMsg::RegionDone))
            .non_interactive()
            .dim_alpha(self.active_overlay_opacity)
            .line_alpha(self.active_overlay_opacity);
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
            let rs = RegionSelection::new(o.logical_pos, rect, |a0| Msg::Capture(CaptureMsg::RegionChange(a0)), Msg::Capture(CaptureMsg::RegionDone))
                .non_interactive()
                .dim_alpha(self.active_overlay_opacity)
                .line_alpha(self.active_overlay_opacity);
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
    pub(super) fn window_view(&self, o: &OutputState) -> Element<'_, Msg> {
        let empty: &[WindowThumb] = &[];
        let thumbs = self.windows.get(&o.name).map(|v| v.as_slice()).unwrap_or(empty);

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
                widget::text("No windows on this display").into()
            };
            widget::container(inner)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
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
            let (pw, ph) = (o.logical_size.0 as f32, o.logical_size.1 as f32);
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
            let avail_h = (ph - 24.0 - toolbar_reserve).max(1.0);
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
            // sits entirely above it.
            .padding(cosmic::iced::Padding {
                top: 24.0,
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
            let dim_alpha = self.region_overlay_opacity;
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
                    o.logical_pos,
                    self.region,
                    |a0| Msg::Capture(CaptureMsg::RegionChange(a0)),
                    Msg::Capture(CaptureMsg::RegionDone),
                )
                .dim_alpha(self.region_overlay_opacity)
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
        if let Some(spinner) = self.scan_spinner_layer(o) {
            layers.push(spinner);
        }
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
    fn toast_layer(&self) -> Option<Element<'_, Msg>> {
        let text = self.toast.as_ref()?;
        let pill = widget::container(widget::text(text.clone()).size(14))
            .padding(cosmic::iced::Padding {
                top: 10.0,
                bottom: 10.0,
                left: 18.0,
                right: 18.0,
            })
            .class(cosmic::theme::Container::Custom(Box::new(|theme| {
                let c = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(c.background.component.base.into())),
                    text_color: Some(c.background.component.on.into()),
                    border: Border {
                        radius: crate::app::theme::rounding(theme).m.into(),
                        width: 1.0,
                        color: c.background.component.divider.into(),
                    },
                    ..Default::default()
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
                    background: Some(Background::Color(c.background.component.base.into())),
                    text_color: Some(c.background.component.on.into()),
                    border: Border {
                        radius: crate::app::theme::rounding(theme).m.into(),
                        width: 1.0,
                        color: c.background.component.divider.into(),
                    },
                    ..Default::default()
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
        let lx = ((*gx - ox) as f32 - *hx as f32 / sprite_scale).max(0.0);
        let ly = ((*gy - oy) as f32 - *hy as f32 / sprite_scale).max(0.0);
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

    /// Layer the output's frozen snapshot behind `selection` (freeze mode); a
    /// no-op when there's no snapshot for this output.
    pub(super) fn with_frozen_bg<'a>(
        &'a self,
        o: &OutputState,
        selection: Element<'a, Msg>,
    ) -> Element<'a, Msg> {
        match self.frozen.get(&o.name).filter(|_| self.freeze_backdrop_active()) {
            Some(f) => {
                let bg: Element<'a, Msg> = widget::image::Image::new(f.handle.clone())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .content_fit(cosmic::iced::ContentFit::Fill)
                    .into();
                cosmic::iced::widget::stack(vec![bg, selection]).into()
            }
            None => selection,
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
