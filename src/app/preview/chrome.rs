//! The preview's toolbar chrome: the `Tb` scale context and its button/group
//! builders, the toolbar/transport/window bars, and the edit-toolbar views
//! composed from them.
//! Split from `preview/mod.rs` (DRAGON-115) — pure code motion.

use super::*;

/// The shared inner padding for every flyout panel (unscaled px; scaled by `Tb::scale`).
/// One source so the covermark picker, color palette, and custom color-wheel picker all
/// breathe identically.
const FLYOUT_PAD: f32 = 8.0;

/// The FIXED height of the covermark picker's card row (unscaled). Fixing it lets the
/// upward flyout offset itself by an exact panel height (see [`FlyoutDir::Up`]); it clears
/// one card (a 60px thumbnail + its label + button padding) with a little slack.
pub(super) const COVERMARK_PANEL_ROW_H: f32 = 96.0;

/// The FIXED content height (unscaled) of the annotation color PALETTE flyout — one row of
/// 28px swatches. Lets the palette's upward flyout offset itself by an exact panel height
/// (the swatch group now lives on the BOTTOM bar; see [`FlyoutDir::Up`]).
const ANNOT_PALETTE_ROW_H: f32 = 28.0;

/// The FIXED content height (unscaled) of the custom color-WHEEL picker flyout: the 168px
/// wheel + its column spacing + the Cancel/Apply button row. Same role as
/// [`COVERMARK_PANEL_ROW_H`] for the upward-floating picker on the bottom bar.
const ANNOT_PICKER_CONTENT_H: f32 = 214.0;

/// Format a tool tooltip as `"Name  (KEY)"`, reading the LIVE binding for `action` from the
/// keymap (the SAME lookup the settings shortcuts page uses: [`crate::shortcuts::Keymap::get`]
/// → [`crate::shortcuts::Shortcut::label`]). Falls back to just `"Name"` when the action is
/// unbound. The ONE place preview-editor tooltips resolve a hotkey, so a rebind in settings is
/// reflected everywhere (two spaces before the key match the historical hand style).
pub(super) fn action_tip(name: &str, action: crate::shortcuts::Action, km: &crate::shortcuts::Keymap) -> String {
    match km.get(action) {
        Some(sc) => format!("{name}  ({})", sc.label()),
        None => name.to_string(),
    }
}

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
    /// A flyout (the color palette or the covermark picker) is open — suppress every
    /// toolbar button's hover tooltip while it is, so a tooltip never draws split across
    /// the flyout (its text above, background below — an overlay layer-batching artifact).
    /// `false` restores the normal tooltips.
    pub(super) suppress_tooltips: bool,
}

impl Tb {
    /// Wrap `content` in a hover tooltip UNLESS a flyout is open (then return the bare
    /// button). The single seam every preview toolbar tooltip routes through.
    pub(super) fn tip<'a>(
        self,
        content: impl Into<Element<'a, Msg>>,
        tip: impl Into<Element<'a, Msg>>,
        pos: widget::tooltip::Position,
    ) -> Element<'a, Msg> {
        if self.suppress_tooltips {
            content.into()
        } else {
            widget::tooltip(content, tip, pos).into()
        }
    }

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
    /// The total on-screen height of a flyout panel whose CONTENT is `content_h` tall — the
    /// content plus the shared [`FLYOUT_PAD`] top+bottom (scaled). The single place that math
    /// lives, so an upward flyout's offset matches exactly what [`Tb::panel_container`] draws.
    pub(super) fn flyout_panel_h(self, content_h: f32) -> f32 {
        content_h + 2.0 * FLYOUT_PAD * self.scale
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
        tip: String,
        msg: PreviewMsg,
        pos: widget::tooltip::Position,
    ) -> Element<'static, Msg> {
        let icon = widget::icon::icon(crate::widgets::icons::handle(name))
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
        self.tip(button, widget::text(tip).size(12), pos)
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
        self.panel_container(widget::row(items).spacing(2.0).align_y(Alignment::Center))
    }

    /// The shared flyout-panel surface (frosted component background, PANEL rounding + group
    /// padding) wrapping arbitrary `content` — the SAME look the covermark picker uses, so
    /// every flyout (covermark picker, color palette, custom hex picker) matches. Generic
    /// over `'a` so it can wrap borrowed content (e.g. the hex input).
    pub(super) fn panel_container<'a>(
        self,
        content: impl Into<Element<'a, Msg>>,
    ) -> Element<'a, Msg> {
        let glass = self.glass;
        widget::container(content)
            // The ONE flyout inner padding, shared by every flyout (covermark picker, color
            // palette, custom color-wheel picker) so their content never jams the panel edge
            // and all three match — noticeably roomier than the toolbar-group `grp_pad`.
            .padding(FLYOUT_PAD * self.scale)
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
    pub(super) fn share_group(self, km: &crate::shortcuts::Keymap) -> Element<'static, Msg> {
        use crate::shortcuts::Action;
        // Top toolbar → tooltips drop below.
        let pos = widget::tooltip::Position::Bottom;
        let buttons = vec![
            self.tool_button(
                "document-save-symbolic",
                action_tip("Save", Action::PreviewSave, km),
                PreviewMsg::Save,
                pos,
            ),
            self.tool_button(
                "document-save-as-symbolic",
                action_tip("Save As", Action::PreviewSaveAs, km),
                PreviewMsg::SaveAs,
                pos,
            ),
            self.tool_button(
                "edit-copy-symbolic",
                action_tip("Copy", Action::PreviewCopy, km),
                PreviewMsg::Copy,
                pos,
            ),
        ];
        self.tool_group(buttons)
    }

    /// The info group pinned to the far right of the action bar: the saved file's size
    /// followed by a Delete (trash) button, together in one group. In `--preview`
    /// (`external`) the file is the user's, so there's no Delete — just the size. `None`
    /// when there's nothing to show.
    pub(super) fn info_group(
        self,
        size: Option<u64>,
        external: bool,
        km: &crate::shortcuts::Keymap,
    ) -> Option<Element<'static, Msg>> {
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
                action_tip("Delete", crate::shortcuts::Action::PreviewDelete, km),
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
    pub(super) fn cancel_group(self, km: &crate::shortcuts::Keymap) -> Element<'static, Msg> {
        // Lives on the top edit toolbar → tooltip drops below.
        self.tool_group(vec![self.tool_button(
            "window-close-symbolic",
            action_tip("Close", crate::shortcuts::Action::PreviewCancel, km),
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
        // Top toolbar → tooltip drops below. No hotkey binds the appearance toggle, so this
        // tip carries no key — plain label.
        self.tool_group(vec![self.tool_button(
            icon,
            tip.to_string(),
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
        let icon = widget::icon::icon(crate::widgets::icons::handle(name))
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
        self.tip(wrapped, tip, tip_pos)
    }

    /// One segment of a SEGMENTED toggle, styled exactly like the region toolbar's
    /// scanner/image/video selector: the active segment is filled accent with an on-accent
    /// glyph, the others sit on the group's divider fill; only the pair's outer corners round.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn seg_toggle(
        self,
        icon: &'static str,
        active: bool,
        msg: PreviewMsg,
        tip: String,
        round_left: bool,
        round_right: bool,
        tip_pos: widget::tooltip::Position,
    ) -> Element<'static, Msg> {
        // Route through the shared Lucide resolver (DRAGON-324) — the pan/pointer glyphs
        // (`object-move-symbolic` → `move`, `input-mouse-symbolic` → `mouse-pointer`) are
        // bundled, so this renders identically on every platform.
        let glyph =
            widget::icon::icon(crate::widgets::icons::handle(icon))
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
        self.tip(btn, widget::text(tip).size(12), tip_pos)
    }

    /// The pointer / pan (grabby-hand) tool toggle — a segmented pair matching the region
    /// toolbar's scanner/image/video control. Lives on the BOTTOM bar, so tooltips rise above.
    pub(super) fn pan_tool_group(
        self,
        pan_mode: bool,
        km: &crate::shortcuts::Keymap,
    ) -> Element<'static, Msg> {
        let pos = widget::tooltip::Position::Top;
        widget::row(vec![
            self.seg_toggle(
                "input-mouse-symbolic",
                !pan_mode,
                PreviewMsg::SetPanMode(false),
                // The pointer/pan toggle's hotkey rides BOTH segments.
                action_tip("Pointer", crate::shortcuts::Action::PreviewTogglePan, km),
                true,
                false,
                pos,
            ),
            self.seg_toggle(
                "object-move-symbolic",
                pan_mode,
                PreviewMsg::SetPanMode(true),
                action_tip("Pan", crate::shortcuts::Action::PreviewTogglePan, km),
                false,
                true,
                pos,
            ),
        ])
        .into()
    }

    /// The annotation stroke-width toggle group: three segments — Thin (2px) / Medium (5px) /
    /// Thick (8px) — styled exactly like [`Self::pan_tool_group`]. Each glyph is a horizontal
    /// line at the matching thickness; the segment NEAREST `current` reads as active. Lives on
    /// the BOTTOM bar beside the color swatch, so tooltips rise above.
    pub(super) fn stroke_width_group(
        self,
        current: f32,
        km: &crate::shortcuts::Keymap,
    ) -> Element<'static, Msg> {
        use super::annotate::{stroke_width_nearest_index, STROKE_WIDTHS};
        let active = stroke_width_nearest_index(current);
        // The group now lives on the BOTTOM bar (beside the color swatch), so tooltips rise.
        let pos = widget::tooltip::Position::Top;
        // (glyph name, tooltip) per preset, thin → thick.
        // No px in the tooltip — the rendered thickness varies by tool (e.g. arrows render
        // thicker), and `action_tip` appends the live hotkey.
        let specs = [
            ("minus-2", "Thin line"),
            ("minus-5", "Medium line"),
            ("minus-8", "Thick line"),
        ];
        // The width group is driven by ONE cycle hotkey (PreviewAnnotStrokeCycle); every
        // segment advertises it.
        let segs: Vec<Element<'static, Msg>> = specs
            .iter()
            .enumerate()
            .map(|(i, (icon, tip))| {
                self.seg_toggle(
                    icon,
                    i == active,
                    PreviewMsg::SetAnnotStrokeW(STROKE_WIDTHS[i]),
                    action_tip(tip, crate::shortcuts::Action::PreviewAnnotStrokeCycle, km),
                    i == 0,
                    i == STROKE_WIDTHS.len() - 1,
                    pos,
                )
            })
            .collect();
        widget::row(segs).into()
    }
}

impl Tb {
    /// An undo/redo icon button that is subdued + inert when its stack is empty.
    pub(super) fn history_button(
        self,
        icon: &'static str,
        tip: String,
        msg: PreviewMsg,
        enabled: bool,
        tip_pos: widget::tooltip::Position,
    ) -> Element<'static, Msg> {
        if !enabled {
            // Subdued, non-interactive: a dimmed glyph with no `on_press`.
            let glyph = widget::icon::icon(crate::widgets::icons::handle(icon))
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
    /// the covermark zoom / opacity + dim/spotlight controls. `to_msg` maps the slider value to
    /// a message; `commit` fires on drag RELEASE (covermark re-raster or dim undo coalesce).
    pub(super) fn slider_with_icon(
        self,
        icon: &'static str,
        range: std::ops::RangeInclusive<f32>,
        value: f32,
        to_msg: fn(f32) -> PreviewMsg,
        commit: PreviewMsg,
    ) -> Element<'static, Msg> {
        let glyph = widget::icon::icon(crate::widgets::icons::handle(icon))
            .width(Length::Fixed(self.icon_box() * SLIDER_SCALE))
            .height(Length::Fixed(self.icon_box() * SLIDER_SCALE));
        let slider = widget::slider(range, value, move |v| Msg::Preview(to_msg(v)))
            .step(0.02f32)
            // Commit only once the drag ends (covermark stays blink-free; dim coalesces undo).
            .on_release(Msg::Preview(commit))
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
        let km = &self.keymap;
        let history = tb.tool_group(vec![
            tb.history_button(
                "edit-undo-symbolic",
                action_tip("Undo", crate::shortcuts::Action::PreviewUndo, km),
                PreviewMsg::Undo,
                e.can_undo(),
                widget::tooltip::Position::Bottom,
            ),
            tb.history_button(
                "edit-redo-symbolic",
                action_tip("Redo", crate::shortcuts::Action::PreviewRedo, km),
                PreviewMsg::Redo,
                e.can_redo(),
                widget::tooltip::Position::Bottom,
            ),
        ]);
        // Pointer / pan tools, the color swatch + line-width groups all live on the BOTTOM
        // bar (see [`Self::edit_tools`]). The DRAW tools (Arrow/Highlight/Box/Pixelate/Blur)
        // stay on the TOP bar right after undo/redo, for IMAGES only (videos have no
        // annotation editor).
        let mut left = vec![tb.appearance_group(preview.surface.is_window()), history];
        if matches!(preview.kind, PreviewKind::Image(_)) {
            left.push(annotation_tools(preview, km, tb));
        }
        // Right: the size + Delete group, then Save / Save As / Copy, then Close.
        let mut right: Vec<Element<'a, Msg>> =
            tb.info_group(preview.size, preview.external, km).into_iter().collect();
        right.push(tb.share_group(km));
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
            right.push(tb.cancel_group(km));
        }
        toolbar_row(left, Vec::new(), right)
    }

    /// The bottom action bar's far-LEFT groups. For IMAGES: the color-swatch group and the
    /// line-width group (relocated here from the top bar), then the covermark group. For
    /// videos: just the covermark group. The color / covermark flyouts float UPWARD (this is
    /// the bottom bar); tooltips rise above.
    pub(super) fn edit_tools<'a>(
        &'a self,
        preview: &'a PreviewState,
        tb: Tb,
    ) -> Vec<Element<'a, Msg>> {
        let e = &preview.edit;
        // Image annotation controls sit at the far left, BEFORE the covermark group: the
        // color swatch (its palette / wheel flyout floats up) then the line-width toggle.
        let mut groups: Vec<Element<'a, Msg>> = Vec::new();
        if matches!(preview.kind, PreviewKind::Image(_)) {
            groups.push(annot_swatch_flyout(preview, &self.annot_recent_colors, &self.keymap, tb));
            groups.push(tb.stroke_width_group(e.stroke(), &self.keymap));
        }
        // Covermark toggle (bordered like the mic/speaker toggles): on = a covermark is
        // applied. Opens the picker dropdown floating over the preview. `Point` uses the
        // context-menu placement: it flips ABOVE the button when there's no room below —
        // which is always the case here, since this bar sits at the bottom.
        let covermark_base = tb.bordered_button(
            "insert-image-symbolic",
            // The covermark button's ring tracks the COVERMARK specifically — NOT the
            // general `dirty()` (which now also counts annotations, so a drawn shape must
            // not light this up). Bake gating still reads `dirty()`.
            e.covermark.is_some(),
            PreviewMsg::Covermark,
            widget::text(action_tip("Covermark", crate::shortcuts::Action::PreviewCovermark, &self.keymap))
                .size(12)
                .into(),
            widget::tooltip::Position::Top,
        );
        let covermark_btn: Element<'static, Msg> = match &e.picker {
            Some(picker) => flyout(
                covermark_base,
                self.covermark_picker(
                    picker,
                    e.flyout_selected(),
                    e.covermark.as_ref().map(|c| &c.kind),
                    tb,
                ),
                // Bottom bar → ALWAYS up, offset by the picker's exact panel height so the
                // bottom edge lands flush with the button top (deterministic on overlay AND
                // window; no reliance on the popover's room-below auto-flip).
                FlyoutDir::Up(tb.flyout_panel_h(COVERMARK_PANEL_ROW_H)),
                Msg::Preview(PreviewMsg::FlyoutClose),
            ),
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
                PreviewMsg::CommitCovermarkEdit,
            ));
            covermark_items.push(tb.slider_with_icon(
                "display-brightness-symbolic",
                0.0..=1.0,
                e.covermark.as_ref().map(|c| c.opacity).unwrap_or(0.0),
                PreviewMsg::SetOpacity,
                PreviewMsg::CommitCovermarkEdit,
            ));
        }
        groups.push(tb.tool_group(covermark_items));
        groups
    }
}

use crate::widgets::annotation_canvas::Tool;

/// Which way a toolbar flyout (popout panel) expands from its anchor button. Every preview
/// flyout now lives on the BOTTOM bar (covermark picker + color palette / wheel), so all of
/// them float UP; the former `Down` (top-bar) direction is gone with the swatch's move down.
#[derive(Clone, Copy)]
pub(super) enum FlyoutDir {
    /// Anchor on the BOTTOM toolbar → expand UPWARD, LEFT-aligned to the button. Carries the
    /// panel's KNOWN height so the popup is offset up by exactly that (bottom flush with the
    /// button top) — deterministic in BOTH surface kinds, not dependent on the popover's
    /// room-below auto-flip (which fails on the fullscreen overlay, where there is always
    /// room below the bottom bar). Requires a FIXED-height panel so the offset is exact.
    Up(f32),
}

/// The ONE shared toolbar-flyout mechanism used by both the covermark picker and the color
/// palette / wheel: `base` (the anchor button) with `panel` floating above it (`Up`),
/// dismissed by `on_close`. The panel itself is responsible for highlighting its selected
/// option (the covermark picker seeds its keyboard index to the applied mark; the palette
/// flags the active swatch) — this helper owns only the popout + direction.
///
/// `Up` uses libcosmic's `Position::Point` (context-menu placement), so it is LEFT-aligned to
/// the button (the popup's left edge = the button's left edge) and DETERMINISTIC — the same in
/// the fullscreen overlay and the windowed preview. `Up = Point(0, -panel_h)`: the popup's top
/// is placed `panel_h` above the button top, so its BOTTOM lands flush with the button top.
/// This does NOT rely on the popover's room-below auto-flip (which never triggers on the
/// fullscreen overlay, where the bottom bar has open space below it) — hence the panel must be
/// a KNOWN fixed height.
pub(super) fn flyout<'a>(
    base: Element<'a, Msg>,
    panel: Element<'a, Msg>,
    dir: FlyoutDir,
    on_close: Msg,
) -> Element<'a, Msg> {
    let point = match dir {
        FlyoutDir::Up(panel_h) => cosmic::iced::Point::new(0.0, -panel_h),
    };
    // The popover dismisses (non-modal) on ANY press that isn't over the ANCHOR button —
    // including presses on the panel's own padding / inter-widget gaps, which no inner widget
    // captures, so they leak through and close the flyout (worst on the tall color-wheel
    // picker with its large empty regions). Wrapping the panel in a `mouse_area` captures
    // every left-press over the panel bounds AFTER delegating to the inner controls: the
    // tabs / wheel / slider / buttons still work (they capture first), and any leftover press
    // inside the panel is swallowed here so it can't reach the dismiss check. Only a press
    // truly OUTSIDE the panel bounds still dismisses.
    // `.interaction(Idle)` makes the shield yield a CONCRETE cursor over its whole bounds
    // (DRAGON-324): mouse_area already DELEGATES to its children, so a real control (the
    // Hex/RGB tabs, wheel, buttons) still reports its own cursor — Pointer over the tabs —
    // but over the panel's empty gaps, where the children report `Interaction::None`, this
    // returns `Idle` (arrow) instead. Without it those `None` regions let iced fall back to
    // the BASE layer's cursor (the AnnotationCanvas crosshair / arrow), which bled through the
    // popup. The stray-click swallow (mouse_area captures the left-press so it can't reach the
    // dismiss backdrop) is unchanged.
    let panel = widget::mouse_area(panel).interaction(cosmic::iced::core::mouse::Interaction::Idle);
    widget::popover(base)
        .popup(panel)
        .position(widget::popover::Position::Point(point))
        .on_close(on_close)
        .into()
}

/// The TOP-bar annotation DRAW tools: Arrow / Highlight / Box / Pixelate / Blur. The color
/// swatch + line-width groups moved to the BOTTOM bar (see [`annot_swatch_flyout`] +
/// [`App::edit_tools`]). Add a tool = add a `bordered_button` here (plus its `Tool` variant +
/// rasterize + bake arms + hotkey; see `annotate.rs`'s module doc), and map it to its
/// [`crate::shortcuts::Action`] so the tooltip shows the LIVE key.
fn annotation_tools<'a>(
    preview: &'a PreviewState,
    km: &crate::shortcuts::Keymap,
    tb: Tb,
) -> Element<'a, Msg> {
    use crate::shortcuts::Action;
    let e = &preview.edit;
    // Top toolbar → tooltips drop below. The active draw tool shows its ring; NEUTRAL
    // (`None`) = no tool ringed. Clicking/hotkeying a tool only ever SETS it.
    let pos = widget::tooltip::Position::Bottom;
    let active = e.tool;
    let btn = |icon: &'static str, t: Tool, name: &str, action: Action| {
        tb.bordered_button(
            icon,
            active == Some(t),
            PreviewMsg::SelectTool(t),
            widget::text(action_tip(name, action, km)).size(12).into(),
            pos,
        )
    };
    // The draw-tool buttons sit bare (no capsule); the Spotlight tool + dim slider share ONE
    // group capsule at the end — the SAME treatment as the covermark button + its opacity
    // slider. Highlight sits just left of Box; the dim slider is ALWAYS usable, never gated on
    // the active tool (DRAGON-329).
    widget::row(vec![
        btn("mail-forward-symbolic", Tool::Arrow, "Arrow", Action::PreviewAnnotArrow),
        // Destructive redactions sit right after Arrow: pixelate mosaic + strong blur.
        btn("view-grid-symbolic", Tool::Pixelate, "Pixelate", Action::PreviewAnnotPixelate),
        btn("image-filter-symbolic", Tool::Blur, "Blur", Action::PreviewAnnotBlur),
        // A filled highlighter marker (distinct from Box's outline square).
        btn("format-text-highlight-symbolic", Tool::Highlight, "Highlight", Action::PreviewAnnotHighlight),
        btn("checkbox-symbolic", Tool::Rect, "Box Outline", Action::PreviewAnnotBox),
        // A highlight FILL wrapped in a box OUTLINE (DRAGON-333).
        btn("box-highlight-symbolic", Tool::BoxHighlight, "Box Highlight", Action::PreviewAnnotBoxHighlight),
        // Spotlight + dim slider, grouped like the covermark button + opacity slider. The
        // slider is REVERSED so it reads as image visibility: 100% = fully transparent (no dim,
        // the default), 0% = fully opaque; stored `e.dim` is the dim amount (0 = none), so the
        // display value + message both invert. Commits ONE undo entry on release.
        tb.tool_group(vec![
            btn("spotlight-symbolic", Tool::Spotlight, "Spotlight", Action::PreviewAnnotSpotlight),
            tb.slider_with_icon(
                "display-brightness-symbolic",
                0.0..=1.0,
                1.0 - e.dim,
                |v| PreviewMsg::SetDim(1.0 - v),
                PreviewMsg::CommitDimEdit,
            ),
        ]),
    ])
    .spacing(tb.btn_pad())
    .align_y(Alignment::Center)
    .into()
}

/// The BOTTOM-bar color swatch with its upward flyout (the palette grid, or — once the "+"
/// opens it — the custom color-wheel picker). The swatch itself is a single-segment capsule
/// matching the pan / width toggle groups (see [`annot_color_swatch`]); the flyout floats UP
/// so its bottom edge lands flush with the swatch top, mirroring the covermark picker on this
/// same bottom bar. Borrows `preview`/`recents`, so the returned element lives as long as they.
fn annot_swatch_flyout<'a>(
    preview: &'a PreviewState,
    recents: &'a [[u8; 4]],
    km: &crate::shortcuts::Keymap,
    tb: Tb,
) -> Element<'a, Msg> {
    let e = &preview.edit;
    // The current color, defaulting to the accent's COMPLEMENT (the companion).
    let color = e.annot_color.unwrap_or_else(super::annotate::default_annot_color);
    let swatch_seg = annot_color_swatch(color, km, tb);
    if e.flyout_kind() == Some(super::edit::FlyoutKind::Color) {
        flyout(
            swatch_seg,
            annot_palette_panel(e.flyout_selected(), recents, tb),
            FlyoutDir::Up(tb.flyout_panel_h(ANNOT_PALETTE_ROW_H)),
            Msg::Preview(PreviewMsg::FlyoutClose),
        )
    } else if let Some(model) = &e.annot_picker {
        flyout(
            swatch_seg,
            annot_color_picker_panel(model, tb),
            FlyoutDir::Up(tb.flyout_panel_h(ANNOT_PICKER_CONTENT_H)),
            Msg::Preview(PreviewMsg::AnnotColorEditor(false)),
        )
    } else {
        swatch_seg
    }
}

/// The color-swatch as a single-segment capsule styled EXACTLY like the pan/pointer and
/// line-width toggle groups (`segment_style`, resting/inactive, both sides round) — so the
/// swatch reads as one of the segmented groups, not a bare square. The current annotation
/// color is a filled, rounded chip for the segment's content (the analogue of a seg_toggle
/// glyph, so fill / rounding / padding / height match). Clicking opens the palette flyout.
fn annot_color_swatch(
    color: [u8; 4],
    km: &crate::shortcuts::Keymap,
    tb: Tb,
) -> Element<'static, Msg> {
    let c = cosmic::iced::Color::from_rgb(
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
    );
    // The color chip fills the segment's glyph box (the same footprint a seg_toggle glyph
    // gets), rounded + hairline-bordered, so the segment padding frames it like a toggle.
    let chip = widget::container(widget::Space::new().width(Length::Fill).height(Length::Fill))
        .width(Length::Fixed(tb.icon_box()))
        .height(Length::Fixed(tb.icon_box()))
        .class(cosmic::theme::Container::Custom(Box::new(move |theme| {
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(c)),
                border: Border {
                    radius: crate::app::theme::rounding(theme).xs.into(),
                    width: 1.0,
                    color: theme.cosmic().palette.neutral_8.into(),
                },
                ..Default::default()
            }
        })));
    // Keep the group's footprint (padding / height, matching the toggle groups) but a TRANSPARENT
    // fill (user request) — the color chip is the only visible content, no segment background.
    let style = |_t: &cosmic::Theme, _hovered: bool| cosmic::widget::button::Style {
        background: None,
        ..cosmic::widget::button::Style::new()
    };
    let btn = crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::custom(
            widget::container(chip).width(Length::Fill).align_x(Alignment::Center),
        )
        .class(cosmic::theme::Button::Custom {
            active: Box::new(move |_, t| style(t, false)),
            disabled: Box::new(move |t| style(t, false)),
            hovered: Box::new(move |_, t| style(t, true)),
            pressed: Box::new(move |_, t| style(t, true)),
        })
        .on_press(Msg::Preview(PreviewMsg::ToggleAnnotPalette))
        .width(Length::Fixed(tb.icon_box() + 2.0 * tb.btn_pad()))
        .padding(tb.btn_pad()),
    );
    // Bottom bar → tooltip rises above.
    tb.tip(
        btn,
        widget::text(action_tip("Color", crate::shortcuts::Action::PreviewColorFlyout, km)).size(12),
        widget::tooltip::Position::Top,
    )
}

/// The palette flyout (covermark-style panel background): the ordered entry list from
/// [`super::annotate::palette_entries`] — complement, accent | palette | last-5 custom | "+"
/// — with a separator between each group. The keyboard-highlighted index (`sel`) shows a
/// ring; each color swatch sets the annotation color, the "+" opens the custom hex picker.
fn annot_palette_panel(sel: Option<usize>, recents: &[[u8; 4]], tb: Tb) -> Element<'static, Msg> {
    use super::annotate::{PaletteEntry, PALETTE_COLOR_COUNT, PALETTE_LEAD};
    let entries = super::annotate::palette_entries(recents);
    // Separators AFTER the [complement, accent] group and AFTER the palette group.
    let sep_a = PALETTE_LEAD;
    let sep_b = PALETTE_LEAD + PALETTE_COLOR_COUNT;
    let mut items: Vec<Element<'static, Msg>> = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        if i == sep_a || i == sep_b {
            items.push(annot_palette_sep());
        }
        let hi = sel == Some(i);
        match entry {
            PaletteEntry::Color(c) => items.push(annot_palette_swatch(*c, hi)),
            PaletteEntry::Custom => items.push(annot_plus_swatch(hi)),
        }
    }
    tb.tool_panel(items)
}

/// A thin vertical divider between palette groups. The theme's `background.divider` is a
/// low-alpha hairline that washes out entirely on the frosted flyout panel; a solid
/// `neutral_8` reads too bold. This splits the difference — `neutral_8` at reduced alpha, a
/// gentle-but-visible divider. Wrapped with horizontal padding so it sits in a clear gap
/// between the two swatch groups.
fn annot_palette_sep() -> Element<'static, Msg> {
    let line = widget::container(
        widget::Space::new().width(Length::Fixed(1.0)).height(Length::Fixed(24.0)),
    )
    .class(cosmic::theme::Container::custom(|theme| {
        let mut c: cosmic::iced::Color = theme.cosmic().palette.neutral_8.into();
        c.a = 0.4;
        cosmic::iced::widget::container::Style {
            background: Some(Background::Color(c)),
            ..Default::default()
        }
    }));
    widget::container(line).padding([0.0, 5.0]).into()
}

/// The custom-color "+" opener at the end of the palette (`hi` = keyboard-highlighted).
fn annot_plus_swatch(hi: bool) -> Element<'static, Msg> {
    let plus = widget::text("+").size(20);
    let btn = widget::button::custom(
        widget::container(plus)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center),
    )
    .width(Length::Fixed(28.0))
    .height(Length::Fixed(28.0))
    .padding(0)
    .class(cosmic::theme::Button::Custom {
        active: Box::new(move |_f, t| plus_swatch_style(hi, t)),
        hovered: Box::new(move |_f, t| plus_swatch_style(hi, t)),
        pressed: Box::new(move |_f, t| plus_swatch_style(hi, t)),
        disabled: Box::new(move |t| plus_swatch_style(hi, t)),
    })
    .on_press(Msg::Preview(PreviewMsg::AnnotColorEditor(true)));
    crate::widgets::arrow_cursor::arrow_cursor(btn)
}

fn plus_swatch_style(hi: bool, theme: &cosmic::Theme) -> cosmic::widget::button::Style {
    let cosmic = theme.cosmic();
    let mut s = cosmic::widget::button::Style::new();
    s.background = Some(Background::Color(cosmic.background.component.base.into()));
    s.text_color = Some(cosmic.background.component.on.into());
    s.border_radius = crate::app::theme::rounding(theme).xs.into();
    s.border_width = 1.0;
    s.border_color = cosmic.palette.neutral_8.into();
    if hi {
        s.outline_width = 2.0;
        s.outline_color = crate::app::theme::accent(theme);
    }
    s
}

/// The CUSTOM color-wheel picker: libcosmic's own cross-platform `ColorPickerModel` — an
/// interactive hue/saturation-value spectrum + hue slider + hex/RGB input — followed by our
/// own Cancel + Apply row. Reset / Save-Cancel are suppressed (those builder labels omitted);
/// recents only appear after the user finishes a drag. On the shared flyout-panel background.
/// Clicks stay inside the popup (only an OUTSIDE click closes it). Borrows `model`, so the
/// returned element lives as long as the picker state.
fn annot_color_picker_panel<'a>(
    model: &'a cosmic::widget::ColorPickerModel,
    tb: Tb,
) -> Element<'a, Msg> {
    // The picker's inner content width. The libcosmic `ColorPicker` widget is width-`Fill`,
    // so left to itself it stretches the whole (fullscreen-overlay) surface — the wheel
    // crammed left, the tabs edge-to-edge, and a huge TRANSPARENT gap whose clicks fall
    // through to the popover's dismiss backdrop. Pinning the wrapping column to a FIXED
    // width hard-caps the layout limits handed down to the widget, so the whole picker is a
    // compact, opaque box. A non-capturing closure coerces to the `fn` pointer the builder
    // wants.
    const PICKER_W: f32 = 272.0;
    let wheel = model
        .builder(|u| Msg::Preview(PreviewMsg::AnnotColorPickerUpdate(u)))
        .width(Length::Fixed(PICKER_W))
        .height(Length::Fixed(168.0))
        .build("Recent", "Copy to clipboard", "Copied");
    let cancel = crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::standard("Cancel")
            .on_press(Msg::Preview(PreviewMsg::AnnotColorEditor(false))),
    );
    let apply = crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::suggested("Apply").on_press(Msg::Preview(PreviewMsg::AnnotColorApply)),
    );
    let buttons = widget::row(vec![cancel, apply]).spacing(8.0);
    let col = widget::column(vec![Element::from(wheel), buttons.into()])
        .spacing(10.0)
        .align_x(Alignment::Center)
        .width(Length::Fixed(PICKER_W));
    tb.panel_container(col)
}

fn annot_palette_swatch(color: [u8; 4], selected: bool) -> Element<'static, Msg> {
    let c = cosmic::iced::Color::from_rgb(
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
    );
    let class = cosmic::theme::Button::Custom {
        active: Box::new(move |_f, t| annot_palette_style(c, selected, t)),
        hovered: Box::new(move |_f, t| annot_palette_style(c, selected, t)),
        pressed: Box::new(move |_f, t| annot_palette_style(c, selected, t)),
        disabled: Box::new(move |t| annot_palette_style(c, selected, t)),
    };
    crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::custom(widget::Space::new().width(Length::Fill).height(Length::Fill))
            .width(Length::Fixed(28.0))
            .height(Length::Fixed(28.0))
            .padding(0)
            .class(class)
            .on_press(Msg::Preview(PreviewMsg::SetAnnotColor(color))),
    )
}

fn annot_palette_style(
    color: cosmic::iced::Color,
    selected: bool,
    theme: &cosmic::Theme,
) -> cosmic::widget::button::Style {
    let cosmic = theme.cosmic();
    let mut s = cosmic::widget::button::Style::new();
    s.background = Some(Background::Color(color));
    s.border_radius = crate::app::theme::rounding(theme).xs.into();
    s.border_width = 1.0;
    s.border_color = cosmic.palette.neutral_8.into();
    if selected {
        s.outline_width = 2.0;
        s.outline_color = crate::app::theme::accent(theme);
    }
    s
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

    /// The tooltip key-resolver reads the LIVE binding: `"Name  (KEY)"` (two spaces) when
    /// bound, just `"Name"` when unbound, and it tracks a rebind.
    #[test]
    fn action_tip_reads_live_key_and_omits_when_unbound() {
        use crate::shortcuts::{Action, Keymap, Shortcut, ShortcutKey};
        let mut km = Keymap::defaults();
        // Bound: two spaces then the keymap's own label in parens.
        let key = km.get(Action::PreviewSave).unwrap().label();
        assert_eq!(action_tip("Save", Action::PreviewSave, &km), format!("Save  ({key})"));
        // A rebind is reflected in the tip.
        km.set(
            Action::PreviewSave,
            Shortcut { ctrl: true, alt: false, shift: false, logo: false, key: ShortcutKey::Char("q".into()) },
        );
        let rekey = km.get(Action::PreviewSave).unwrap().label();
        assert_eq!(action_tip("Save", Action::PreviewSave, &km), format!("Save  ({rekey})"));
        // Unbound: name only.
        km.unbind(Action::PreviewSave);
        assert_eq!(action_tip("Save", Action::PreviewSave, &km), "Save");
    }
}
