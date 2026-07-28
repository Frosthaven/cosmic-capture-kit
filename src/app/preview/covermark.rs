//! The covermark picker dropdown and the covermark display plumbing (the
//! remembered per-mark prefs and the coalesced overlay re-raster).
//! Split from `preview/mod.rs` (DRAGON-115) — pure code motion.

use super::*;

/// Picker card styling: the keyboard-selected entry carries an accent outline.
pub(super) fn picker_card_class(selected: bool) -> cosmic::theme::Button {
    fn style(selected: bool, hovered: bool, theme: &cosmic::Theme) -> cosmic::widget::button::Style {
        let cosmic = theme.cosmic();
        let mut s = cosmic::widget::button::Style::new();
        s.border_radius = crate::app::theme::rounding(theme).s.into();
        if selected {
            s.border_width = 2.0;
            s.border_color = crate::app::theme::accent(theme);
        }
        if hovered {
            let mut bg: cosmic::iced::Color = cosmic.palette.neutral_5.into();
            bg.a = 0.15;
            s.background = Some(Background::Color(bg));
        }
        s
    }
    if selected {
        cosmic::theme::Button::Custom {
            active: Box::new(|_f, t| style(true, false, t)),
            hovered: Box::new(|_f, t| style(true, true, t)),
            pressed: Box::new(|_f, t| style(true, true, t)),
            disabled: Box::new(|t| style(true, false, t)),
        }
    } else {
        cosmic::theme::Button::Custom {
            active: Box::new(|_f, t| style(false, false, t)),
            hovered: Box::new(|_f, t| style(false, true, t)),
            pressed: Box::new(|_f, t| style(false, true, t)),
            disabled: Box::new(|t| style(false, false, t)),
        }
    }
}

impl App {
    /// The covermark picker dropdown: a keyboard- and mouse-navigable strip of SVG
    /// previews (←/→ move, Enter applies, Esc closes; click applies/toggles). The
    /// currently-applied covermark is marked. Owned data only, so it's `'static`.
    pub(super) fn covermark_picker(
        &self,
        picker: &Picker,
        selected_idx: Option<usize>,
        active: Option<&CovermarkKind>,
        tb: Tb,
    ) -> Element<'static, Msg> {
        let mut items: Vec<Element<'static, Msg>> = Vec::new();
        for (i, entry) in picker.entries.iter().enumerate() {
            let selected = selected_idx == Some(i);
            // The thumbnail: real covermarks render their SVG; the "None" card shows a
            // subdued X (an enable/disable list, so None disables).
            let thumb: Element<'static, Msg> = match entry {
                None => widget::container(
                    widget::icon::icon(crate::widgets::icons::handle("window-close-symbolic")).size(32)
                        .class(cosmic::theme::Svg::custom(|t| cosmic::widget::svg::Style {
                            color: Some(crate::app::theme::subdued(t)),
                        })),
                )
                .width(Length::Fixed(96.0))
                .height(Length::Fixed(60.0))
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .into(),
                Some(kind) => {
                    let handle = match kind {
                        CovermarkKind::Confidential => {
                            widget::svg::Handle::from_memory(edit::confidential_svg())
                        }
                        CovermarkKind::Text(text) => {
                            widget::svg::Handle::from_memory(edit::text_svg_bytes(text))
                        }
                        CovermarkKind::File(p) => widget::svg::Handle::from_path(p),
                    };
                    widget::svg(handle)
                        .width(Length::Fixed(96.0))
                        .height(Length::Fixed(60.0))
                        .into()
                }
            };
            // Active when this entry matches the applied covermark (or "None" when
            // nothing is applied) — labelled with a ✓ so the list reads as toggles.
            let is_active = match entry {
                None => active.is_none(),
                Some(kind) => active == Some(kind),
            };
            let name = match entry {
                None => "None".to_string(),
                Some(kind) => kind.name(),
            };
            let label = if is_active {
                widget::text(format!("✓ {name}")).size(11).class(
                    cosmic::theme::Text::Custom(|t| cosmic::iced::widget::text::Style {
                        color: Some(crate::app::theme::accent(t)),
                        ..Default::default()
                    }),
                )
            } else if entry.is_none() {
                // "None" reads subdued (it's the empty choice, not a real mark).
                widget::text(name).size(11).class(cosmic::theme::Text::Custom(|t| {
                    cosmic::iced::widget::text::Style { color: Some(crate::app::theme::subdued(t)), ..Default::default() }
                }))
            } else {
                widget::text(name).size(11)
            };
            let card = widget::column(vec![thumb, label.into()])
                .spacing(4.0)
                .align_x(Alignment::Center);
            items.push(
                crate::widgets::arrow_cursor::arrow_cursor(
                    widget::button::custom(card)
                        .class(picker_card_class(selected))
                        .padding(6.0)
                        .on_press(Msg::Preview(tb.pid, PreviewMsg::PickerPick(i))),
                ),
            );
        }
        // A popover panel (not a button group): panel rounding, so the tall card strip keeps
        // its corners under the "round" preference. The card row is pinned to a FIXED height
        // (`COVERMARK_PANEL_ROW_H`) so the upward flyout can offset itself by the panel's exact
        // height and land flush with the button top on every surface kind (cards center within
        // it). Built directly (not via `tool_panel`) so that height reaches the row.
        let row = widget::row(items)
            .spacing(2.0)
            .align_y(Alignment::Center)
            .height(Length::Fixed(super::chrome::COVERMARK_PANEL_ROW_H));
        tb.panel_container(row)
    }

    /// Store the active covermark's current zoom + opacity as THIS option's remembered
    /// pref (keyed by `pref_key`), and mirror it into the global last-used values (the
    /// fallback for an option picked for the first time). No-op when no covermark is set.
    pub(super) fn remember_covermark_pref(&mut self, id: window::Id) {
        let Some((key, zoom, opacity)) = self.preview_for(id)
            .and_then(|p| p.edit.covermark.as_ref())
            .map(|cm| (cm.kind.pref_key(), cm.zoom, cm.opacity))
        else {
            return;
        };
        self.covermark_prefs.insert(key, (zoom, opacity));
        self.covermark_zoom = zoom;
        self.covermark_opacity = opacity;
    }

    /// Kick a COALESCED live-preview re-raster for `edit` — the SHARED live-slider path (see
    /// [`super::edit::LiveEdit`]). Called on every drag tick AND on release; the target
    /// [`layers::RasterSlot`]'s coalescing debounces a fast drag to one raster in flight + one
    /// pending re-run. Every live-adjustable edit routes through here, so the debounce is never
    /// hand-rolled per slider; a new one (dim/spotlight, DRAGON-329) adds a match arm.
    pub(super) fn refresh_live_edit(&mut self, id: window::Id, edit: super::edit::LiveEdit) -> Task<cosmic::Action<Msg>> {
        match edit {
            super::edit::LiveEdit::Covermark => self.refresh_edit_display(id),
        }
    }

    /// Re-raster the covermark OVERLAY for the current covermark, OFF-THREAD and COALESCED
    /// via [`layers::RasterSlot`] (a rapid change can't pile up rasters). The overlay is a
    /// small, mostly-transparent RGBA layer stacked over the untouched base image/video via
    /// a persistent-texture shader — so the base never re-uploads and the overlay's own
    /// texture updates in place (no atlas churn), which is what keeps edits blink-free. The
    /// bake still composites at full source resolution.
    pub(super) fn refresh_edit_display(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        // The visual scale needs the App (viewport geometry), so resolve it under an IMMUTABLE
        // borrow before taking the mutable one below.
        let Some(vscale) = self.preview_for(id).map(|p| self.preview_visual_scale(p)) else {
            return Task::none();
        };
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        let Some(covermark) = p.edit.covermark.clone() else {
            p.edit.cm_raster.clear();
            return Task::none();
        };
        let Some(generation) = p.edit.cm_raster.begin() else {
            // A raster is already in flight — `begin` coalesced this request; it re-runs
            // once that raster lands (see the `CovermarkRasterReady` handler below).
            return Task::none();
        };
        // Size the raster to the covermark layer's ON-SCREEN device-pixel footprint at the
        // current zoom (capped at the source frame) so it is exactly as crisp as the base
        // pixels beside it (DRAGON-324, corrected in DRAGON-362 — the old fixed 1024 baseline
        // left a big capture's mark upsampled at fit zoom); remember it so a no-op zoom skips
        // a re-raster.
        let (pw, ph) = p.edit.covermark_raster_size(p.view.zoom, vscale);
        p.edit.cm_raster_px = (pw, ph);
        let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let frame = edit::rasterize_preview(&covermark, pw, ph).map(|img| {
                let (w, h) = img.dimensions();
                crate::app::PixelFrame::new(img.into_raw(), w, h)
            });
            let _ = tx.send(frame);
        });
        Task::perform(rx, move |res| {
            cosmic::Action::App(Msg::Preview(id, PreviewMsg::CovermarkRasterReady(
                generation,
                res.ok().flatten(),
            )))
        })
    }

    /// Re-raster the covermark at the CURRENT view zoom when a mark is applied AND the zoom
    /// now wants a DIFFERENT resolution than the last raster (DRAGON-324). Called after every
    /// zoom change so a magnified covermark sharpens toward the source resolution — without
    /// re-rastering on a zoom step that doesn't change the wanted resolution (e.g. already at
    /// the source cap, or below fit). No covermark → nothing to do.
    pub(super) fn refresh_covermark_for_zoom(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for(id) else {
            return Task::none();
        };
        if p.edit.covermark.is_none()
            || p.edit.covermark_raster_size(p.view.zoom, self.preview_visual_scale(p))
                == p.edit.cm_raster_px
        {
            return Task::none();
        }
        self.refresh_edit_display(id)
    }
}
