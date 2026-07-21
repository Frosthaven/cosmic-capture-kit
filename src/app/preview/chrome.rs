//! The preview's toolbar chrome: the `Tb` scale context and its button/group
//! builders, the toolbar/transport/window bars, and the edit-toolbar views
//! composed from them.
//! Split from `preview/mod.rs` (DRAGON-115) — pure code motion.

use super::*;

/// Toolbar-builder scale context — an explicit value replacing a former `thread_local`:
/// 1.0 for the fullscreen overlay, 0.82 for the windowed preview (its smaller window
/// wants tighter chrome; see [`PreviewSurface::btn_scale`]). Built once at the top of
/// `preview_view` and threaded down through every scale-dependent builder below (and
/// the `App` methods that call them) as a plain argument.
#[derive(Clone, Copy)]
pub(super) struct Tb {
    pub(super) scale: f32,
    /// Frosted-glass config when the preview is WINDOWED and frosting is on, else
    /// `None` (DRAGON-217). The toolbar button-group chips paint translucent so the
    /// window's glass shows through them. ALWAYS `None` for the fullscreen overlay
    /// (a layer-shell surface, never frosted) — so the overlay stays byte-identical.
    pub(super) glass: Option<crate::app::theme::GlassConfig>,
}

impl Tb {
    /// The scaled toolbar-button glyph box.
    pub(super) fn icon_box(self) -> f32 {
        ICON_BOX * self.scale
    }
    /// The scaled per-button inner padding.
    pub(super) fn btn_pad(self) -> f32 {
        BTN_PAD * self.scale
    }
    /// The scaled group padding.
    pub(super) fn grp_pad(self) -> f32 {
        GROUP_PAD * self.scale
    }
}

impl Tb {
    /// A toolbar icon button (a 22px glyph in a `Button::Icon`) with a hover tooltip at
    /// `pos` naming the action + its hotkey — the shared body behind every toolbar action
    /// and the enabled [`Tb::history_button`]. `pos` is the tooltip side: top-toolbar
    /// buttons pass `Bottom` (drop below, clear of the top edge) and bottom-toolbar
    /// buttons pass `Top`.
    pub(super) fn tool_button(
        self,
        name: &'static str,
        tip: &'static str,
        msg: PreviewMsg,
        pos: widget::tooltip::Position,
    ) -> Element<'static, Msg> {
        let icon = widget::icon::Icon::from(widget::icon::from_name(name).size(64))
            .width(Length::Fixed(self.icon_box()))
            .height(Length::Fixed(self.icon_box()));
        let button = crate::widgets::arrow_cursor::arrow_cursor(
            widget::button::custom(
                widget::container(icon)
                    .width(Length::Fill)
                    .align_x(Alignment::Center),
            )
            .class(cosmic::theme::Button::Icon)
            .on_press(Msg::Preview(msg))
            .padding(self.btn_pad()),
        );
        widget::tooltip(button, widget::text(tip).size(12), pos).into()
    }

    /// Wrap toolbar buttons in the capture toolbar's rounded group panel — the
    /// button token, so groups round like the buttons they hold (a capsule
    /// under the "round" preference).
    pub(super) fn tool_group(self, buttons: Vec<Element<'static, Msg>>) -> Element<'static, Msg> {
        let glass = self.glass;
        widget::container(widget::row(buttons).spacing(2.0).align_y(Alignment::Center))
            .padding(self.grp_pad())
            .class(cosmic::theme::Container::Custom(Box::new(move |theme| {
                let c = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(crate::app::theme::frost_color(
                        c.background.component.base.into(),
                        glass,
                    ))),
                    border: Border {
                        radius: crate::app::theme::rounding(theme).xl.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            })))
            .into()
    }

    /// The same group surface at PANEL rounding (the small token) — for tall
    /// popover panels like the covermark picker, where the button token's
    /// capsule would swallow the corners.
    pub(super) fn tool_panel(self, items: Vec<Element<'static, Msg>>) -> Element<'static, Msg> {
        let glass = self.glass;
        widget::container(widget::row(items).spacing(2.0).align_y(Alignment::Center))
            .padding(self.grp_pad())
            .class(cosmic::theme::Container::Custom(Box::new(move |theme| {
                let c = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(crate::app::theme::frost_color(
                        c.background.component.base.into(),
                        glass,
                    ))),
                    border: Border {
                        radius: crate::app::theme::rounding(theme).s.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            })))
            .into()
    }

    /// The shared Save / Save As / Copy actions group (same on image and video previews).
    /// Save is always shown: for a fresh capture it keeps the file; for a pre-existing
    /// `--preview` file it exits when unchanged, or overwrites (after a confirm dialog)
    /// when the preview has edits.
    pub(super) fn share_group(self) -> Element<'static, Msg> {
        // Top toolbar → tooltips drop below.
        let pos = widget::tooltip::Position::Bottom;
        let buttons = vec![
            self.tool_button("document-save-symbolic", "Save  (Ctrl+S)", PreviewMsg::Save, pos),
            self.tool_button(
                "document-save-as-symbolic",
                "Save As  (Ctrl+Shift+S)",
                PreviewMsg::SaveAs,
                pos,
            ),
            self.tool_button("edit-copy-symbolic", "Copy  (Ctrl+C)", PreviewMsg::Copy, pos),
        ];
        self.tool_group(buttons)
    }

    /// The info group pinned to the far right of the action bar: the saved file's size
    /// followed by a Delete (trash) button, together in one group. In `--preview`
    /// (`external`) the file is the user's, so there's no Delete — just the size. `None`
    /// when there's nothing to show.
    pub(super) fn info_group(self, size: Option<u64>, external: bool) -> Option<Element<'static, Msg>> {
        let mut items: Vec<Element<'static, Msg>> = Vec::new();
        if let Some(bytes) = size {
            // Match a button's inner box (icon + its vertical padding) so the group is the same
            // height as the action groups; the text stays vertically centred within it.
            let label = widget::container(widget::text(friendly_size(bytes)).size(13))
                .height(Length::Fixed(self.icon_box() + 2.0 * self.btn_pad()))
                .padding([0.0, self.btn_pad()])
                .align_y(Alignment::Center);
            items.push(label.into());
        }
        if !external {
            // A plain (non-destructive-styled) trash button — deletes the file, Ctrl+D.
            // Top toolbar → tooltip drops below.
            items.push(self.tool_button(
                "edit-delete-symbolic",
                "Delete  (Ctrl+D)",
                PreviewMsg::Delete,
                widget::tooltip::Position::Bottom,
            ));
        }
        if items.is_empty() {
            None
        } else {
            Some(self.tool_group(items))
        }
    }
}

/// A flexible spacer that splits a toolbar row into a left-aligned and a right-aligned
/// section. The row it lives in must be `width(Length::Fill)` for this to expand.
pub(super) fn toolbar_split() -> Element<'static, Msg> {
    widget::Space::new().width(Length::Fill).into()
}

/// Compose a preview toolbar row with LEFT | CENTER | RIGHT sections: flexible splits push
/// left to the far left and right to the far right, with the center between them. An empty
/// center collapses to the classic left | right split (a single spacer).
pub(super) fn toolbar_row<'a>(
    left: Vec<Element<'a, Msg>>,
    center: Vec<Element<'a, Msg>>,
    right: Vec<Element<'a, Msg>>,
) -> Element<'a, Msg> {
    let mut items = left;
    items.push(toolbar_split());
    if !center.is_empty() {
        items.extend(center);
        items.push(toolbar_split());
    }
    items.extend(right);
    widget::row(items)
        .spacing(8.0)
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .into()
}

/// Format a byte count as a friendly size (`B`, `KB`, `MB`, `GB`, `TB`).
pub(super) fn friendly_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

impl Tb {
    /// The close (x) action group — shown alone while loading, and beside the other actions
    /// once the content is ready. Esc triggers the same `Cancel`; it never deletes (that's
    /// the Delete trash button), so it just closes.
    pub(super) fn cancel_group(self) -> Element<'static, Msg> {
        // Lives on the top edit toolbar → tooltip drops below.
        self.tool_group(vec![self.tool_button(
            "window-close-symbolic",
            "Close  (Esc)",
            PreviewMsg::Cancel,
            widget::tooltip::Position::Bottom,
        )])
    }
}

/// Compose the loaded preview from its three parts. In WINDOWED mode the edit + action
/// toolbars pin to the top / bottom of the window on full-width bars (a standard,
/// light/dark-respecting surface colour), with the image filling the space between. In
/// OVERLAY mode all three centre together as one floating group (the historical look).
pub(super) fn compose_preview<'a>(
    windowed: bool,
    overlay_width: f32,
    edit_toolbar: Element<'a, Msg>,
    image: Element<'a, Msg>,
    transport: Option<Element<'a, Msg>>,
    action_toolbar: Element<'a, Msg>,
    glass: Option<crate::app::theme::GlassConfig>,
) -> Element<'a, Msg> {
    if windowed {
        let mut col: Vec<Element<'a, Msg>> = vec![
            preview_bar(edit_toolbar, false, glass),
            widget::container(image)
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into(),
        ];
        // The video transport strip sits between the canvas and the action bar.
        if let Some(t) = transport {
            col.push(transport_bar(t, true, glass));
        }
        // The bottom bar sits at the window's bottom edge, so round its bottom corners
        // to match the CSD window's rounded corners (no square nubs past the rounding).
        col.push(preview_bar(action_toolbar, true, glass));
        widget::column(col)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    } else {
        // The toolbars are Fill-width so their left/right split reaches the column
        // edges. `overlay_width` hugs the fitted picture (never below what the
        // toolbar groups need); the outer container centres this column. The video
        // transport strip slots between the canvas and the action toolbar.
        let mut col: Vec<Element<'a, Msg>> = vec![edit_toolbar, image];
        if let Some(t) = transport {
            col.push(transport_bar(t, false, glass));
        }
        col.push(action_toolbar);
        widget::column(col)
            .spacing(12.0)
            .width(Length::Fixed(overlay_width))
            .align_x(Alignment::Center)
            .into()
    }
}

/// The transport strip's wrapper: the play/seek row on a QUIETER surface than the
/// toolbars (it's a control strip, not chrome). Sizes to its CONTENT — the strip's
/// height is never hard-coded here; the reserve math reads `preview_transport_h`.
/// `windowed` gets the plain window background (reads as part of the body, quieter
/// than the `primary` toolbar bars); the overlay gets a subdued translucent panel
/// spanning the hugged column.
pub(super) fn transport_bar(
    row: Element<'_, Msg>,
    windowed: bool,
    glass: Option<crate::app::theme::GlassConfig>,
) -> Element<'_, Msg> {
    widget::container(row)
        .width(Length::Fill)
        .align_x(Alignment::Center)
        .padding([8.0, 12.0])
        .class(cosmic::theme::Container::custom(move |theme| {
            let c = theme.cosmic();
            if windowed {
                // The body colour itself — visually part of the canvas area. Frosted
                // (DRAGON-217): drop the fill entirely so the window's single glass
                // layer shows through the transport strip too (a fill would stack over
                // that glass and read opaque). Plain body colour otherwise.
                let background = if glass.is_some_and(|g| g.frosted_windows) {
                    None
                } else {
                    Some(Background::Color(c.background.base.into()))
                };
                cosmic::iced::widget::container::Style { background, ..Default::default() }
            } else {
                // A washed-down group panel: present but quieter than the toolbars.
                let mut bg: cosmic::iced::Color = c.background.component.base.into();
                bg.a *= 0.65;
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(bg)),
                    border: Border {
                        radius: crate::app::theme::rounding(theme).s.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }
        }))
        .into()
}

/// A full-width top/bottom toolbar bar for the windowed preview: the toolbar row
/// centred on a standard surface colour (light/dark respecting) that spans the window.
/// `round_bottom` rounds the bottom corners so the bottom bar meets the CSD window's
/// rounded corners cleanly.
pub(super) fn preview_bar(
    row: Element<'_, Msg>,
    round_bottom: bool,
    glass: Option<crate::app::theme::GlassConfig>,
) -> Element<'_, Msg> {
    widget::container(row)
        .width(Length::Fill)
        .align_x(Alignment::Center)
        .padding([8.0, 12.0])
        .class(cosmic::theme::Container::custom(move |theme| {
            let c = theme.cosmic();
            let br = if round_bottom {
                let r = crate::app::theme::rounding(theme).window();
                Border {
                    radius: [0.0, 0.0, r[2], r[3]].into(),
                    ..Default::default()
                }
            } else {
                Border::default()
            };
            // Frosted (DRAGON-217): the top/bottom toolbar bars DROP their opaque
            // primary fill so the window's single glass layer (the outer container's
            // frosted background) shows through the toolbars uniformly — a solid
            // primary tint here would stack over that glass and read as a plain gray
            // bar, not glass. Fully opaque primary otherwise (today's look).
            let background = if glass.is_some_and(|g| g.frosted_windows) {
                None
            } else {
                Some(Background::Color(c.primary.base.into()))
            };
            cosmic::iced::widget::container::Style {
                background,
                border: br,
                ..Default::default()
            }
        }))
        .into()
}

impl Tb {
    /// The appearance-toggle group at the far top-left: one button that flips the preview
    /// between the fullscreen overlay and a resizable window, live. The glyph advertises the
    /// *destination* — a restore/window glyph while in the overlay (click to pop out), a
    /// fullscreen glyph while windowed (click to go fullscreen).
    pub(super) fn appearance_group(self, windowed: bool) -> Element<'static, Msg> {
        let (icon, tip) = if windowed {
            ("view-fullscreen-symbolic", "Fullscreen overlay")
        } else {
            ("view-restore-symbolic", "Windowed")
        };
        // Top toolbar → tooltip drops below.
        self.tool_group(vec![self.tool_button(
            icon,
            tip,
            PreviewMsg::ToggleAppearance,
            widget::tooltip::Position::Bottom,
        )])
    }

    /// A toolbar icon button styled exactly like the capture toolbar's mic/speaker
    /// toggles: the glyph and a 1px border ring both track on/off (foreground + accent
    /// when on, the subdued wash when off), so the outline is always present and the
    /// footprint never shifts. `tip` is any hover element.
    pub(super) fn bordered_button(
        self,
        name: &'static str,
        on: bool,
        msg: PreviewMsg,
        tip: Element<'static, Msg>,
        tip_pos: widget::tooltip::Position,
    ) -> Element<'static, Msg> {
        let icon = widget::icon::Icon::from(widget::icon::from_name(name).size(64))
            .width(Length::Fixed(self.icon_box()))
            .height(Length::Fixed(self.icon_box()))
            .class(cosmic::theme::Svg::Custom(std::rc::Rc::new(move |t: &cosmic::Theme| {
                let color = if on {
                    t.cosmic().background.component.on.into()
                } else {
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
            .selected(on)
            .class(cosmic::theme::Button::Icon)
            .on_press(Msg::Preview(msg))
            .padding(self.btn_pad()),
        );
        let wrapped = widget::container(btn).class(cosmic::theme::Container::Custom(Box::new(
            move |theme| {
                cosmic::iced::widget::container::Style {
                    border: Border {
                        // The button token: the ring hugs the (framework-styled)
                        // button inside, which rounds at the same token.
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
            },
        )));
        widget::tooltip(wrapped, tip, tip_pos).into()
    }

    /// One segment of a SEGMENTED toggle, styled exactly like the region toolbar's
    /// scanner/image/video selector: the active segment is filled accent with an on-accent
    /// glyph, the others sit on the group's divider fill; only the pair's outer corners round.
    pub(super) fn seg_toggle(
        self,
        icon: &'static str,
        active: bool,
        msg: PreviewMsg,
        tip: &'static str,
        round_left: bool,
        round_right: bool,
    ) -> Element<'static, Msg> {
        // Route through the shared vendored-icon seam: the pan half's
        // `object-move-symbolic` isn't in libcosmic's embedded bundle (blank on
        // macOS), so it's served from a bundled SVG; every other name (the pointer
        // half's `input-mouse-symbolic`) falls through to `from_name` unchanged.
        let glyph =
            widget::icon::icon(crate::app::overlay::toolbar::vendored_icon_handle(icon))
                .size(64)
                .width(Length::Fixed(self.icon_box()))
                .height(Length::Fixed(self.icon_box()));
        // The shared segmented-pair style (theme.rs) — the SAME rendering as
        // the capture toolbar's scanner/image/video pair, embossed inactive
        // glyph included, so the two toggles can't drift apart.
        let style = move |t: &cosmic::Theme, hovered: bool| {
            crate::app::theme::segment_style(t, active, hovered, round_left, round_right)
        };
        let btn = crate::widgets::arrow_cursor::arrow_cursor(
            widget::button::custom(
                widget::container(glyph).width(Length::Fill).align_x(Alignment::Center),
            )
            .class(cosmic::theme::Button::Custom {
                active: Box::new(move |_, t| style(t, false)),
                disabled: Box::new(move |t| style(t, false)),
                hovered: Box::new(move |_, t| style(t, true)),
                pressed: Box::new(move |_, t| style(t, true)),
            })
            .on_press(Msg::Preview(msg))
            .width(Length::Fixed(self.icon_box() + 2.0 * self.btn_pad()))
            .padding(self.btn_pad()),
        );
        widget::tooltip(btn, tip, widget::tooltip::Position::Top).into()
    }

    /// The pointer / pan (grabby-hand) tool toggle — a segmented pair matching the region
    /// toolbar's scanner/image/video control.
    pub(super) fn pan_tool_group(self, pan_mode: bool) -> Element<'static, Msg> {
        widget::row(vec![
            self.seg_toggle(
                "input-mouse-symbolic",
                !pan_mode,
                PreviewMsg::SetPanMode(false),
                "Pointer",
                true,
                false,
            ),
            self.seg_toggle(
                "object-move-symbolic",
                pan_mode,
                PreviewMsg::SetPanMode(true),
                "Pan: drag to move",
                false,
                true,
            ),
        ])
        .into()
    }
}

impl Tb {
    /// An undo/redo icon button that is subdued + inert when its stack is empty.
    pub(super) fn history_button(
        self,
        icon: &'static str,
        tip: &'static str,
        msg: PreviewMsg,
        enabled: bool,
        tip_pos: widget::tooltip::Position,
    ) -> Element<'static, Msg> {
        if !enabled {
            // Subdued, non-interactive: a dimmed glyph with no `on_press`.
            let glyph = widget::icon::Icon::from(widget::icon::from_name(icon).size(64))
                .width(Length::Fixed(self.icon_box()))
                .height(Length::Fixed(self.icon_box()))
                .class(cosmic::theme::Svg::custom(|theme| cosmic::widget::svg::Style {
                    color: Some(crate::app::theme::subdued(theme)),
                }));
            return crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::custom(
                    widget::container(glyph).width(Length::Fill).align_x(Alignment::Center),
                )
                .class(cosmic::theme::Button::Icon)
                .padding(self.btn_pad()),
            );
        }
        // Enabled: a normal icon button; the caller picks where the tooltip goes
        // (below for the top edit toolbar, above for the bottom transport strip).
        self.tool_button(icon, tip, msg, tip_pos)
    }
}

/// The covermark sliders (and their glyphs) render 20% smaller than the toolbar buttons
/// in every view — they read fine compact and free up toolbar width.
pub(super) const SLIDER_SCALE: f32 = 0.8;

/// The natural width of one [`slider_with_icon`] item (glyph + gap + slider + h-padding),
/// used to size the overlay control area. Kept in sync with the builder below.
pub(super) const SLIDER_ITEM_W: f32 = 2.0 * BTN_PAD + ICON_BOX * SLIDER_SCALE + 6.0 + 80.0 * SLIDER_SCALE;

/// The little breathing gap the overlay control area keeps between the split's two sides
/// when the picture isn't wide enough to spread them further.
pub(super) const SPLIT_MIN_GAP: f32 = 40.0;

impl Tb {
    /// An icon + a fixed-width slider, vertically centred to a toolbar button's height —
    /// the covermark zoom / opacity controls. `to_msg` maps the slider value to a message.
    pub(super) fn slider_with_icon(
        self,
        icon: &'static str,
        range: std::ops::RangeInclusive<f32>,
        value: f32,
        to_msg: fn(f32) -> PreviewMsg,
    ) -> Element<'static, Msg> {
        let glyph = widget::icon::Icon::from(widget::icon::from_name(icon).size(64))
            .width(Length::Fixed(self.icon_box() * SLIDER_SCALE))
            .height(Length::Fixed(self.icon_box() * SLIDER_SCALE));
        let slider = widget::slider(range, value, move |v| Msg::Preview(to_msg(v)))
            .step(0.02f32)
            // Commit (re-raster) only once the drag ends, so sliding stays blink-free.
            .on_release(Msg::Preview(PreviewMsg::CommitCovermarkEdit))
            .width(Length::Fixed(80.0 * SLIDER_SCALE));
        widget::container(
            widget::row(vec![
                glyph.into(),
                crate::widgets::arrow_cursor::arrow_cursor(slider),
            ])
            .spacing(6.0)
            .align_y(Alignment::Center),
        )
        .height(Length::Fixed(self.icon_box() + 2.0 * self.btn_pad()))
        .align_y(Alignment::Center)
        .padding([0.0, self.btn_pad()])
        .into()
    }
}

impl App {
    /// The top edit bar above the preview content: the appearance toggle and undo / redo
    /// on the left, then Save / Save As / Copy (and Close, outside windowed mode) pushed to
    /// the right by the split. The do-not-train + covermark tools live on the BOTTOM bar
    /// (see [`Self::edit_tools`]).
    pub(super) fn edit_toolbar<'a>(&'a self, preview: &'a PreviewState, tb: Tb) -> Element<'a, Msg> {
        let e = &preview.edit;
        let history = tb.tool_group(vec![
            tb.history_button(
                "edit-undo-symbolic",
                "Undo  (Ctrl+Z)",
                PreviewMsg::Undo,
                e.can_undo(),
                widget::tooltip::Position::Bottom,
            ),
            tb.history_button(
                "edit-redo-symbolic",
                "Redo  (Ctrl+Shift+Z)",
                PreviewMsg::Redo,
                e.can_redo(),
                widget::tooltip::Position::Bottom,
            ),
        ]);
        // Pointer / pan tools live on the bottom bar (next to the zoom scale), not here.
        let left = vec![tb.appearance_group(preview.surface.is_window()), history];
        // Right: the size + Delete group, then Save / Save As / Copy, then Close.
        let mut right: Vec<Element<'a, Msg>> =
            tb.info_group(preview.size, preview.external).into_iter().collect();
        right.push(tb.share_group());
        // The Close (x) button is drawn for the OVERLAY preview (no native window
        // chrome) and normally omitted for the WINDOWED preview (its native
        // traffic-light close does the job). DRAGON-268 follow-up (fullscreen header
        // vanish): in NATIVE fullscreen the windowed preview's traffic lights auto-hide,
        // so without the app-drawn Close the user has no reachable way to leave the
        // preview — add it back in that state. macOS-only signal (`preview_fullscreen`,
        // set from the resize handler); off macOS the windowed preview never enters this
        // arm, so the historical omit-when-windowed behavior is byte-identical.
        #[cfg(target_os = "macos")]
        let show_close = !preview.surface.is_window() || self.preview_fullscreen;
        #[cfg(not(target_os = "macos"))]
        let show_close = !preview.surface.is_window();
        if show_close {
            right.push(tb.cancel_group());
        }
        toolbar_row(left, Vec::new(), right)
    }

    /// The covermark group, pinned to the far LEFT of the bottom action bar: the
    /// covermark button with its zoom + opacity sliders (shown only when a covermark
    /// is applied). The covermark picker floats UPWARD (it's on the bottom bar) as a
    /// dropdown; tooltips rise above.
    pub(super) fn edit_tools(&self, preview: &PreviewState, tb: Tb) -> Vec<Element<'static, Msg>> {
        let e = &preview.edit;
        // Covermark toggle (bordered like the mic/speaker toggles): on = a covermark is
        // applied. Opens the picker dropdown floating over the preview. `Point` uses the
        // context-menu placement: it flips ABOVE the button when there's no room below —
        // which is always the case here, since this bar sits at the bottom.
        let covermark_base = tb.bordered_button(
            "insert-image-symbolic",
            e.dirty(),
            PreviewMsg::Covermark,
            widget::text("Covermark  (W)").size(12).into(),
            widget::tooltip::Position::Top,
        );
        let covermark_btn: Element<'static, Msg> = match &e.picker {
            Some(picker) => widget::popover(covermark_base)
                .popup(self.covermark_picker(picker, e.covermark.as_ref().map(|c| &c.kind), tb))
                .position(widget::popover::Position::Point(cosmic::iced::Point::new(0.0, 0.0)))
                .on_close(Msg::Preview(PreviewMsg::PickerClose))
                .into(),
            None => covermark_base,
        };
        // Zoom + opacity sliders, each with an icon — shown ONLY when a covermark is
        // applied. Zoom: 0 = cover fit, up to 3× (snaps to 0). Opacity: 0..1.
        let mut covermark_items = vec![covermark_btn];
        if e.covermark.is_some() {
            covermark_items.push(tb.slider_with_icon(
                "zoom-in-symbolic",
                0.0..=3.0,
                e.zoom(),
                |z| PreviewMsg::SetZoom(if z < 0.08 { 0.0 } else { z }),
            ));
            covermark_items.push(tb.slider_with_icon(
                "display-brightness-symbolic",
                0.0..=1.0,
                e.covermark.as_ref().map(|c| c.opacity).unwrap_or(0.0),
                PreviewMsg::SetOpacity,
            ));
        }
        vec![tb.tool_group(covermark_items)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_size_stays_in_bytes_below_1024() {
        assert_eq!(friendly_size(0), "0 B");
        assert_eq!(friendly_size(1023), "1023 B");
    }

    #[test]
    fn friendly_size_switches_units_at_1024_and_keeps_one_decimal() {
        assert_eq!(friendly_size(1024), "1.0 KB");
        assert_eq!(friendly_size(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn friendly_size_drops_the_decimal_at_100_and_above() {
        assert_eq!(friendly_size(99 * 1024), "99.0 KB");
        assert_eq!(friendly_size(100 * 1024), "100 KB");
    }
}
