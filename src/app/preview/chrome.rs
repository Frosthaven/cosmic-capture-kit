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
    /// The PREVIEW SURFACE every message these builders emit is addressed to
    /// (DRAGON-336 phase 2). Built once in `preview_view` from the document being drawn,
    /// so a toolbar click can never drive a different preview window.
    pub(super) pid: window::Id,
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
            .on_press(Msg::Preview(self.pid, msg))
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

    /// A GROUP CONTAINER for a cluster of related annotation tools (DRAGON-340): the same
    /// frosted, rounded, padded surface as [`Self::tool_group`] PLUS a 1px hairline border —
    /// the border/padding treatment that used to ring each individual tool button, repurposed
    /// to enclose a whole SET of them, so the tray reads as grouped families instead of a flat
    /// run of icons. One border per GROUP, never per icon.
    ///
    /// The border token is the DROPDOWN's: `background.divider` at 1px — the exact colour the
    /// zoom preset menu (the one dropdown on this same preview's bottom bar) outlines itself
    /// with. It replaced the old `state_mix(MIX_OFF)` wash, which read too loud around a whole
    /// group. The RADIUS stays the button (`xl`) token: the cluster is a capsule holding
    /// capsule buttons, and the dropdown's panel (`s`) rounding would square its corners off
    /// under the "round" preference.
    ///
    /// `chrome` decides whether that surface is PAINTED at all ([`ClusterChrome`]): a `Bare`
    /// group keeps the identical padded footprint (so the row's spacing and alignment don't
    /// shift) and simply draws no fill and no border.
    ///
    /// A one-member cluster gets exactly the same container as a five-member one (same border,
    /// padding and corner treatment) — that is deliberate, and the reason there is no
    /// single-member special case anywhere.
    pub(super) fn tool_cluster(
        self,
        items: Vec<Element<'static, Msg>>,
        chrome: ClusterChrome,
    ) -> Element<'static, Msg> {
        let glass = self.glass;
        widget::container(widget::row(items).spacing(2.0).align_y(Alignment::Center))
            .padding(self.grp_pad())
            .class(cosmic::theme::Container::Custom(Box::new(move |theme| {
                let radius = crate::app::theme::rounding(theme).xl.into();
                match chrome {
                    ClusterChrome::Surface => cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(crate::app::theme::frost_color(
                            theme.cosmic().background.component.base.into(),
                            glass,
                        ))),
                        border: Border {
                            radius,
                            width: 1.0,
                            // The DROPDOWN's outline token — a group outline is chrome, never
                            // a state, so it never tracks selection.
                            color: theme.cosmic().background.divider.into(),
                        },
                        ..Default::default()
                    },
                    // Footprint only: no fill, no border, nothing to see.
                    ClusterChrome::Bare => cosmic::iced::widget::container::Style {
                        border: Border { radius, ..Default::default() },
                        ..Default::default()
                    },
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

/// Compose the loaded preview from its parts. In WINDOWED mode the edit + action toolbars
/// pin to the top / bottom of the window on full-width bars (a standard, light/dark-respecting
/// surface colour), with the image filling the space between. In OVERLAY mode they centre
/// together as one floating group (the historical look), led by `overlay_header` — the
/// appearance / undo / redo / Close line (DRAGON-337), which the WINDOWED arm never gets
/// (those controls live in its titlebar) and so always passes `None`.
#[allow(clippy::too_many_arguments)]
pub(super) fn compose_preview<'a>(
    windowed: bool,
    overlay_width: f32,
    overlay_header: Option<Element<'a, Msg>>,
    edit_toolbar: Element<'a, Msg>,
    image: Element<'a, Msg>,
    transport: Option<Element<'a, Msg>>,
    action_toolbar: Element<'a, Msg>,
    glass: Option<crate::app::theme::GlassConfig>,
) -> Element<'a, Msg> {
    if windowed {
        // The windowed preview's titlebar owns those controls; nothing to slot in here.
        drop(overlay_header);
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
        // The header row (appearance / undo / redo ⟨split⟩ Close) leads the column;
        // `PreviewSurface::chrome_h` reserves its height + one more column gap.
        let mut col: Vec<Element<'a, Msg>> = Vec::new();
        col.extend(overlay_header);
        col.extend([edit_toolbar, image]);
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
    // The appearance toggle no longer has a capsuled `tool_group` form (DRAGON-337): it is a
    // FLAT header button in both surfaces — `App::overlay_header_row` for the overlay,
    // `App::preview_header_controls` (the CSD titlebar) for the window.

    // The old per-icon `bordered_button` (a glyph inside its own 1px on/off ring) is GONE: the
    // annotation tray dropped it in DRAGON-340 (the ring became the group's, see
    // `Self::tool_cluster`) and its last holdout — the bottom bar's covermark toggle — followed,
    // so the whole preview editor now speaks ONE control language. Every on/off state in this
    // chrome is carried by icon COLOUR ([`Tb::tool_toggle`]). Don't reintroduce per-icon rings.

    /// A preview-chrome TOGGLE button (DRAGON-340 restyle): a bare symbolic glyph whose COLOUR
    /// alone carries its state — [`crate::app::theme::accent`] when ON (a tool is armed, a
    /// covermark is applied), the plain [`crate::app::theme::foreground`] ("white" in the dark
    /// themes, near-black in the light ones) when it isn't. No ring, no chip, no fill, in ANY
    /// state: the border/padding that used to ring each icon now encloses a whole GROUP
    /// ([`Self::tool_cluster`]).
    ///
    /// Both colours come from the theme's own ramps, so the tray tracks light/dark and the
    /// user's accent without a hardcoded value anywhere. The footprint is identical to the
    /// button it replaced (`icon_box` + `btn_pad`), so nothing else reflows.
    pub(super) fn tool_toggle(
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
                    crate::app::theme::accent(t)
                } else {
                    crate::app::theme::foreground(t)
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
            .class(cosmic::theme::Button::Icon)
            .on_press(Msg::Preview(self.pid, msg))
            .padding(self.btn_pad()),
        );
        self.tip(btn, tip, tip_pos)
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
            .on_press(Msg::Preview(self.pid, msg))
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

/// A FLAT header-style icon button (DRAGON-337): a tinted symbolic glyph in a bare
/// `Button::Icon` with a square halo and NO group capsule behind it — the treatment the
/// settings window's collapse / search controls use. Both the windowed preview's titlebar
/// ([`titlebar_button`]) and the overlay preview's header row ([`overlay_header_button`])
/// render through here, so the two can never drift apart.
///
/// The button ALWAYS takes `on_press`, even when it currently does nothing (a spent
/// undo/redo stack sends its message into a no-op update): a "disabled" control must still
/// ABSORB the click, or the press falls through to the titlebar beneath — where two quick
/// taps on dead undo read as a titlebar double-click. Spent-ness is conveyed by `tint`
/// alone. `tint` always names the glyph colour explicitly — a `Button::Icon` otherwise
/// inherits the class's own text colour, which is what made these read accent-ish.
/// `top_pin` stretches the control to the header's full height and pins it to the TOP (the
/// macOS titlebar case, where its centre must land on the native traffic lights' centreline).
#[allow(clippy::too_many_arguments)]
fn flat_icon_button(
    pid: window::Id,
    name: &'static str,
    tip: String,
    msg: PreviewMsg,
    tint: fn(&cosmic::Theme) -> cosmic::iced::Color,
    glyph_px: u16,
    halo: u16,
    top_pin: bool,
) -> Element<'static, Msg> {
    let glyph = widget::icon::icon(crate::widgets::icons::handle(name))
        .size(glyph_px)
        .class(cosmic::theme::Svg::Custom(std::rc::Rc::new(
            move |t: &cosmic::Theme| cosmic::widget::svg::Style { color: Some(tint(t)) },
        )));
    let btn = widget::button::custom(glyph)
        .class(cosmic::theme::Button::Icon)
        .on_press(Msg::Preview(pid, msg));
    let btn = crate::widgets::arrow_cursor::arrow_cursor(btn.padding(halo));
    let control: Element<'static, Msg> = if top_pin {
        widget::container(btn).height(Length::Fill).align_y(Alignment::Start).into()
    } else {
        btn
    };
    // Below the button, clear of the top edge (the same side the top toolbar's tips drop to).
    widget::tooltip(control, widget::text(tip).size(12), widget::tooltip::Position::Bottom).into()
}

/// A [`flat_icon_button`] sized for the WINDOWED preview's CSD titlebar — the settings
/// header's own 16px glyph. Platform alignment mirrors that header exactly and nothing else:
/// * macOS — the halo shrinks to [`crate::app::settings::MAC_HEADER_GLYPH_HALO`] and the button
///   is TOP-pinned, so its centre lands on the native traffic lights' centreline; the leading
///   traffic-light inset itself is reserved by the caller (`preview_view`), like
///   `config_window_view` does.
/// * Windows — nothing here: the DWM caption cluster sits at the TRAILING edge, and the
///   preview header already reserves `WIN_CAPTION_INSET` there; these buttons are leading.
/// * Linux — the plain 8px halo, unchanged from the settings header's non-mac arm.
fn titlebar_button(
    pid: window::Id,
    name: &'static str,
    tip: String,
    msg: PreviewMsg,
    tint: fn(&cosmic::Theme) -> cosmic::iced::Color,
) -> Element<'static, Msg> {
    #[cfg(target_os = "macos")]
    let (halo, top_pin) = (crate::app::settings::MAC_HEADER_GLYPH_HALO, true);
    #[cfg(not(target_os = "macos"))]
    let (halo, top_pin) = (8u16, false);
    flat_icon_button(pid, name, tip, msg, tint, 16, halo, top_pin)
}

impl Tb {
    /// A [`flat_icon_button`] at THIS surface's toolbar metrics — the OVERLAY header row's
    /// buttons. Same flat look as the titlebar's, but sized to the overlay's own (scaled-up)
    /// glyph box so the row doesn't read tiny against the chrome below it. Never top-pinned:
    /// the row is app-drawn, with no native window buttons to line up against.
    fn header_button(
        self,
        name: &'static str,
        tip: String,
        msg: PreviewMsg,
        tint: fn(&cosmic::Theme) -> cosmic::iced::Color,
    ) -> Element<'static, Msg> {
        flat_icon_button(self.pid, name, tip, msg, tint, self.icon_box() as u16, self.btn_pad() as u16, false)
    }

    /// The undo / redo pair as FLAT header buttons: full-strength foreground ("white" in the
    /// dark themes) while the stack has entries, the subdued wash when it's empty — the same
    /// on/off colours [`Self::history_button`] gives the toolbar version. A spent button KEEPS
    /// its `on_press` (undo/redo on an empty stack no-op in the update) so it still absorbs
    /// clicks — see [`flat_icon_button`]. Shared by the overlay header row and (via
    /// [`App::preview_header_controls`]) the titlebar, so the two stay identical.
    fn flat_history(
        e: &EditState,
        km: &crate::shortcuts::Keymap,
        build: impl Fn(&'static str, String, PreviewMsg, fn(&cosmic::Theme) -> cosmic::iced::Color) -> Element<'static, Msg>,
    ) -> Vec<Element<'static, Msg>> {
        let one = |icon, name, action, msg, on: bool| {
            let tint: fn(&cosmic::Theme) -> cosmic::iced::Color =
                if on { crate::app::theme::foreground } else { crate::app::theme::subdued };
            build(icon, action_tip(name, action, km), msg, tint)
        };
        vec![
            one(
                "edit-undo-symbolic",
                "Undo",
                crate::shortcuts::Action::PreviewUndo,
                PreviewMsg::Undo,
                e.can_undo(),
            ),
            one(
                "edit-redo-symbolic",
                "Redo",
                crate::shortcuts::Action::PreviewRedo,
                PreviewMsg::Redo,
                e.can_redo(),
            ),
        ]
    }
}

impl App {
    /// The windowed preview's TITLEBAR controls (DRAGON-337): the fullscreen-overlay toggle
    /// (accent — it's the appearance switch, not an edit action) followed by undo / redo
    /// (full-strength foreground when their stacks have entries, subdued + inert when empty).
    /// Returned in leading-edge order.
    pub(super) fn preview_header_controls(
        &self,
        preview: &PreviewState,
    ) -> Vec<Element<'static, Msg>> {
        let pid = preview.window;
        let mut items = vec![titlebar_button(
            pid,
            "view-fullscreen-symbolic",
            "Fullscreen overlay".to_string(),
            PreviewMsg::ToggleAppearance,
            crate::app::theme::accent,
        )];
        items.extend(Tb::flat_history(&preview.edit, &self.keymap, move |i, t, m, c| {
            titlebar_button(pid, i, t, m, c)
        }));
        items
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

/// **How big the preview editor's slider THUMBS ("nibs") are, relative to the libcosmic
/// default.** `1.0` is the stock 20px resting / 26px hover-and-drag thumb; `0.5` halves it.
/// The ONE knob for every slider inside the preview editor — nudge this alone to retune them.
///
/// SCOPE: preview-editor sliders only. It is applied by [`preview_slider_class`], which each
/// preview slider opts into explicitly; the SETTINGS window's sliders (`settings::row`'s
/// shared `opacity_slider`, among others) keep the stock class and are untouched.
///
/// The thumb is purely VISUAL: iced's slider grabs anywhere over its whole bounds, so a
/// smaller thumb does not shrink the hit target at all — the rail is as easy to grab as it
/// ever was.
pub(super) const PREVIEW_SLIDER_THUMB: f32 = 0.5;

/// `style` with its thumb scaled by [`PREVIEW_SLIDER_THUMB`] and everything else — rail,
/// colours, borders, breakpoint — left exactly as the theme resolved it.
fn thumb_scaled(
    mut style: cosmic::iced::widget::slider::Style,
) -> cosmic::iced::widget::slider::Style {
    use cosmic::iced::widget::slider::HandleShape;
    let k = PREVIEW_SLIDER_THUMB;
    let scale_u16 = |v: u16| ((v as f32 * k).round() as u16).max(1);
    style.handle.shape = match style.handle.shape {
        HandleShape::Circle { radius } => HandleShape::Circle { radius: radius * k },
        HandleShape::Rectangle { width, height, mut border_radius } => {
            // The corner radius rides the thumb, so a halved thumb keeps the same silhouette
            // instead of collapsing into a disc.
            border_radius.top_left *= k;
            border_radius.top_right *= k;
            border_radius.bottom_right *= k;
            border_radius.bottom_left *= k;
            HandleShape::Rectangle {
                width: scale_u16(width),
                height: scale_u16(height),
                border_radius,
            }
        }
    };
    style.handle.border_width *= k;
    style
}

/// The slider class every PREVIEW-EDITOR slider wears: the stock cosmic slider styling with
/// only the thumb rescaled by [`PREVIEW_SLIDER_THUMB`].
///
/// It resolves through the theme's OWN `Slider::Standard` styling for each status and then
/// adjusts the handle, so the rail colours, the high-contrast arm, the hover/drag growth and
/// any future upstream change all still apply — this never re-implements the theme.
pub(super) fn preview_slider_class() -> cosmic::style::iced::Slider {
    use cosmic::iced::widget::slider::{Catalog, Status};
    use cosmic::style::iced::Slider;
    fn at(status: Status) -> std::rc::Rc<dyn Fn(&cosmic::Theme) -> cosmic::iced::widget::slider::Style>
    {
        std::rc::Rc::new(move |t: &cosmic::Theme| {
            thumb_scaled(<cosmic::Theme as Catalog>::style(t, &Slider::Standard, status))
        })
    }
    Slider::Custom {
        active: at(Status::Active),
        hovered: at(Status::Hovered),
        dragging: at(Status::Dragged),
    }
}

/// The natural width of one [`Tb::slider_with_icon`] item at chrome scale `scale` (glyph +
/// gap + slider + h-padding), used to size the overlay control area. Kept in sync with the
/// builder below: the padding and the glyph ride `Tb`'s scale, while the row gap and the
/// slider's own fixed width do not.
pub(super) fn slider_item_w(scale: f32) -> f32 {
    scale * (2.0 * BTN_PAD + ICON_BOX * SLIDER_SCALE) + 6.0 + 80.0 * SLIDER_SCALE
}

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
        let pid = self.pid;
        let slider = widget::slider(range, value, move |v| Msg::Preview(pid, to_msg(v)))
            .step(0.02f32)
            // Commit only once the drag ends (covermark stays blink-free; dim coalesces undo).
            .on_release(Msg::Preview(pid, commit))
            // The preview editor's own (smaller) thumb — see `PREVIEW_SLIDER_THUMB`.
            .class(preview_slider_class())
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
    /// The OVERLAY preview's HEADER row (DRAGON-337) — a line above the rest of the chrome
    /// carrying the windowed-swap toggle and undo / redo on the LEFT, with Close pushed to
    /// the far RIGHT by the split. Every button here is FLAT (a bare tinted glyph, no
    /// `tool_group` capsule behind it), matching the windowed preview's titlebar treatment:
    /// the swap and Close read accent, undo / redo the foreground-vs-subdued pair.
    ///
    /// Only the fullscreen overlay draws this row; the windowed preview puts the same
    /// controls in its CSD titlebar ([`App::preview_header_controls`]) and gets Close from
    /// the native window chrome. [`PreviewSurface::chrome_h`]'s overlay arm reserves this
    /// row's height — ONE flat button box, NOT a full toolbar group (there's no capsule
    /// padding around it any more).
    pub(super) fn overlay_header_row(&self, preview: &PreviewState, tb: Tb) -> Element<'static, Msg> {
        let km = &self.keymap;
        // The glyph advertises the DESTINATION: a restore/window glyph while in the overlay
        // (click to pop out into a window). No hotkey binds the appearance toggle, so the
        // tip carries no key.
        let mut left = vec![tb.header_button(
            "view-restore-symbolic",
            "Windowed".to_string(),
            PreviewMsg::ToggleAppearance,
            crate::app::theme::accent,
        )];
        left.extend(Tb::flat_history(&preview.edit, km, move |n, t, m, c| {
            tb.header_button(n, t, m, c)
        }));
        // Esc triggers the same `Cancel`; it never deletes (that's the Delete trash button).
        let close = tb.header_button(
            "window-close-symbolic",
            action_tip("Close", crate::shortcuts::Action::PreviewCancel, km),
            PreviewMsg::Cancel,
            crate::app::theme::accent,
        );
        toolbar_row(left, Vec::new(), vec![close])
    }

    /// The top edit bar above the preview content: the annotation DRAW tools (images only)
    /// on the left, then the size + Delete group and Save / Save As / Copy pushed to the
    /// right by the split. The appearance toggle, undo / redo and Close moved OUT of this
    /// bar in DRAGON-337 — to the windowed preview's titlebar
    /// ([`App::preview_header_controls`]) and the overlay's own header row
    /// ([`Self::overlay_header_row`]). The do-not-train + covermark tools live on the BOTTOM
    /// bar (see [`Self::edit_tools`]).
    pub(super) fn edit_toolbar<'a>(&'a self, preview: &'a PreviewState, tb: Tb) -> Element<'a, Msg> {
        let km = &self.keymap;
        // Pointer / pan tools, the color swatch + line-width groups all live on the BOTTOM
        // bar (see [`Self::edit_tools`]). The DRAW tools (Arrow/Highlight/Box/Pixelate/Blur)
        // lead the TOP bar, for IMAGES only (videos have no annotation editor).
        let mut left: Vec<Element<'a, Msg>> = Vec::new();
        if matches!(preview.kind, PreviewKind::Image(_)) {
            left.push(annotation_tools(preview, km, tb));
        }
        // Right: the size + Delete group, then Save / Save As / Copy.
        let mut right: Vec<Element<'a, Msg>> =
            tb.info_group(preview.size, preview.external, km).into_iter().collect();
        right.push(tb.share_group(km));
        // The Close (x) button normally lives elsewhere: the OVERLAY draws it on its header
        // row, the WINDOWED preview gets it from the native window chrome. DRAGON-268
        // follow-up (fullscreen header vanish): in NATIVE fullscreen the windowed preview's
        // traffic lights auto-hide AND our CSD titlebar goes with them, so without an
        // app-drawn Close the user has no reachable way to leave the preview — add it back
        // to this bar in that state. macOS-only signal (`preview_fullscreen`, set from the
        // resize handler); off macOS this arm is never reached and the bar carries no Close.
        #[cfg(target_os = "macos")]
        let show_close = preview.surface.is_window() && self.preview_fullscreen;
        #[cfg(not(target_os = "macos"))]
        let show_close = false;
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
        let pid = preview.window;
        let e = &preview.edit;
        // Image annotation controls sit at the far left, BEFORE the covermark group: the
        // color swatch (its palette / wheel flyout floats up) then the line-width toggle.
        let mut groups: Vec<Element<'a, Msg>> = Vec::new();
        if matches!(preview.kind, PreviewKind::Image(_)) {
            groups.push(annot_swatch_flyout(preview, &self.annot_recent_colors, &self.keymap, tb));
            groups.push(tb.stroke_width_group(e.stroke(), &self.keymap));
        }
        // Covermark toggle: on = a covermark is applied. It now wears the SAME treatment as
        // every other control in this chrome — a bare glyph tinted accent when on, foreground
        // when off ([`Tb::tool_toggle`]); the old per-icon ring is gone (it was the last
        // `bordered_button` in the preview). Opens the picker dropdown floating over the
        // preview. `Point` uses the context-menu placement: it flips ABOVE the button when
        // there's no room below — which is always the case here, since this bar sits at the
        // bottom.
        let covermark_base = tb.tool_toggle(
            "insert-image-symbolic",
            // The covermark button's state tracks the COVERMARK specifically — NOT the
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
                Msg::Preview(pid, PreviewMsg::FlyoutClose),
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

/// How a [`Tb::tool_cluster`] paints its container — a DECLARED per-group property of the
/// [`ANNOT_TRAY`] layout, never a special case keyed on which tools a group happens to hold.
/// (The tray's grouping and styling have been re-cut several times; keep both declarative.)
#[derive(Clone, Copy, PartialEq)]
pub(super) enum ClusterChrome {
    /// The normal frosted, dropdown-bordered surface.
    Surface,
    /// Footprint only — same padding and geometry, but no fill and no border. The pointer
    /// (selection) group uses this: it is a mode, not a family of draw tools, and a chip
    /// around it read as one more bordered box in the row.
    Bare,
}

/// One entry in the annotation tray's DECLARED layout ([`ANNOT_TRAY`]).
#[derive(Clone, Copy)]
enum TrayItem {
    /// A tool button: its glyph name, the [`Tool`] it arms, its label, and the
    /// [`crate::shortcuts::Action`] whose LIVE binding the tooltip shows.
    Tool(&'static str, Tool, &'static str, crate::shortcuts::Action),
    /// The global dim ("spotlight range") slider. Not a tool — it belongs to the spotlight's
    /// group but is always usable, whatever tool is armed (DRAGON-329).
    DimSlider,
}

/// One GROUP of the annotation tray: its chrome treatment plus its ordered items.
struct TrayGroup {
    /// Whether this group's [`Tb::tool_cluster`] paints a surface + border, or only occupies
    /// the footprint.
    chrome: ClusterChrome,
    items: &'static [TrayItem],
}

/// **THE annotation tray layout** (DRAGON-340): the ordered GROUPS of the top bar's draw
/// tools, each rendered inside one [`Tb::tool_cluster`], in the order given. This list is the
/// single source of the tray's grouping, its order AND its per-group chrome — the builder
/// below just walks it, so regrouping or re-styling the tray means editing these rows and
/// changing nothing else. (Both have already changed several times; keep them declarative.)
///
/// A ONE-member group is not special-cased anywhere: it gets exactly the container a
/// five-member group gets, minus whatever its own [`ClusterChrome`] says.
///
/// Adding a tool = add a [`TrayItem::Tool`] row to the right group (plus its [`Tool`] variant +
/// model/rasterize/bake arms + hotkey; see `annotate.rs`'s module doc).
const ANNOT_TRAY: &[TrayGroup] = &[
    // Selection. The POINTER creates nothing (DRAGON-341): click / Ctrl-Shift-click /
    // rubber-band to build a selection, drag it to move it all. It is a MODE rather than a
    // family of draw tools, so it wears no chip — only the footprint that keeps it aligned
    // with the bordered groups after it.
    TrayGroup { chrome: ClusterChrome::Bare, items: &[TrayItem::Tool(
        "pointer-select-symbolic",
        Tool::Pointer,
        "Select",
        crate::shortcuts::Action::PreviewAnnotPointer,
    )] },
    // Callouts: point at a thing, or number the steps.
    TrayGroup { chrome: ClusterChrome::Surface, items: &[
        TrayItem::Tool(
            "mail-forward-symbolic",
            Tool::Arrow,
            "Arrow",
            crate::shortcuts::Action::PreviewAnnotArrow,
        ),
        TrayItem::Tool(
            "sequence-badge-symbolic",
            Tool::Badge,
            "Step Marker",
            crate::shortcuts::Action::PreviewAnnotBadge,
        ),
    ] },
    // Destructive redactions: the pixelate mosaic and the strong blur.
    TrayGroup { chrome: ClusterChrome::Surface, items: &[
        TrayItem::Tool(
            "view-grid-symbolic",
            Tool::Pixelate,
            "Pixelate",
            crate::shortcuts::Action::PreviewAnnotPixelate,
        ),
        TrayItem::Tool(
            "image-filter-symbolic",
            Tool::Blur,
            "Blur",
            crate::shortcuts::Action::PreviewAnnotBlur,
        ),
    ] },
    // Emphasis: everything that draws attention to a region, ending with the spotlight and
    // the dim range it works against.
    TrayGroup { chrome: ClusterChrome::Surface, items: &[
        TrayItem::Tool(
            "format-text-highlight-symbolic",
            Tool::Highlight,
            "Highlight",
            crate::shortcuts::Action::PreviewAnnotHighlight,
        ),
        TrayItem::Tool(
            "checkbox-symbolic",
            Tool::Rect,
            "Border",
            crate::shortcuts::Action::PreviewAnnotBox,
        ),
        TrayItem::Tool(
            "box-highlight-symbolic",
            Tool::BoxHighlight,
            "Border Highlight",
            crate::shortcuts::Action::PreviewAnnotBoxHighlight,
        ),
        TrayItem::Tool(
            "spotlight-symbolic",
            Tool::Spotlight,
            "Spotlight",
            crate::shortcuts::Action::PreviewAnnotSpotlight,
        ),
        TrayItem::DimSlider,
    ] },
    // Freehand ink and the hand-undo that removes it.
    TrayGroup { chrome: ClusterChrome::Surface, items: &[
        TrayItem::Tool(
            "pencil-symbolic",
            Tool::Pen,
            "Pencil",
            crate::shortcuts::Action::PreviewAnnotPen,
        ),
        TrayItem::Tool(
            "eraser-symbolic",
            Tool::Eraser,
            "Eraser",
            crate::shortcuts::Action::PreviewAnnotEraser,
        ),
    ] },
];

/// The TOP-bar annotation DRAW tools, rendered from the declared [`ANNOT_TRAY`] grouping: one
/// bordered, padded [`Tb::tool_cluster`] per group, and inside it bare glyph buttons whose
/// COLOUR alone shows which tool is armed ([`Tb::tool_toggle`]) — accent when selected, the
/// theme foreground otherwise.
///
/// Clicking/hotkeying a tool only ever SETS it — except that DOUBLE-clicking a tray button also
/// drops a ready-made item in the middle of the picture (DRAGON-339), which is why the tray
/// emits `ToolPressed` (detector-aware) rather than the bare `SelectTool` the hotkeys use.
fn annotation_tools<'a>(
    preview: &'a PreviewState,
    km: &crate::shortcuts::Keymap,
    tb: Tb,
) -> Element<'a, Msg> {
    let e = &preview.edit;
    // Top toolbar → tooltips drop below. NEUTRAL (`None`) = no tool tinted.
    let pos = widget::tooltip::Position::Bottom;
    let active = e.tool;
    let groups: Vec<Element<'a, Msg>> = ANNOT_TRAY
        .iter()
        .map(|group| {
            let items: Vec<Element<'static, Msg>> = group
                .items
                .iter()
                .map(|entry| match *entry {
                    TrayItem::Tool(icon, tool, name, action) => tb.tool_toggle(
                        icon,
                        active == Some(tool),
                        PreviewMsg::ToolPressed(tool),
                        widget::text(action_tip(name, action, km)).size(12).into(),
                        pos,
                    ),
                    // The slider is REVERSED so it reads as image VISIBILITY: 100% = fully
                    // transparent (no dim, the default), 0% = fully opaque; stored `e.dim` is
                    // the dim amount (0 = none), so the display value + message both invert.
                    // Commits ONE undo entry on release.
                    TrayItem::DimSlider => tb.slider_with_icon(
                        "display-brightness-symbolic",
                        0.0..=1.0,
                        1.0 - e.dim,
                        |v| PreviewMsg::SetDim(1.0 - v),
                        PreviewMsg::CommitDimEdit,
                    ),
                })
                .collect();
            tb.tool_cluster(items, group.chrome)
        })
        .collect();
    widget::row(groups)
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
            Msg::Preview(tb.pid, PreviewMsg::FlyoutClose),
        )
    } else if let Some(model) = &e.annot_picker {
        flyout(
            swatch_seg,
            annot_color_picker_panel(model, tb),
            FlyoutDir::Up(tb.flyout_panel_h(ANNOT_PICKER_CONTENT_H)),
            Msg::Preview(tb.pid, PreviewMsg::AnnotColorEditor(false)),
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
        .on_press(Msg::Preview(tb.pid, PreviewMsg::ToggleAnnotPalette))
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
            PaletteEntry::Color(c) => items.push(annot_palette_swatch(tb.pid, *c, hi)),
            PaletteEntry::Custom => items.push(annot_plus_swatch(tb.pid, hi)),
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
fn annot_plus_swatch(pid: window::Id, hi: bool) -> Element<'static, Msg> {
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
    .on_press(Msg::Preview(pid, PreviewMsg::AnnotColorEditor(true)));
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
    // compact, opaque box.
    //
    // The builder wants a bare `fn` POINTER, which can't carry the addressed window id
    // (DRAGON-336 phase 2), so build the wheel over its own `ColorPickerUpdate` message
    // (the identity `fn`, still non-capturing) and `Element::map` the result into the
    // addressed `Msg` — one wrapping closure instead of a captured fn pointer.
    const PICKER_W: f32 = 272.0;
    let pid = tb.pid;
    let wheel = Element::from(
        model
            .builder(std::convert::identity)
            .width(Length::Fixed(PICKER_W))
            .height(Length::Fixed(168.0))
            .build("Recent", "Copy to clipboard", "Copied"),
    )
    .map(move |u| Msg::Preview(pid, PreviewMsg::AnnotColorPickerUpdate(u)));
    let cancel = crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::standard("Cancel")
            .on_press(Msg::Preview(tb.pid, PreviewMsg::AnnotColorEditor(false))),
    );
    let apply = crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::suggested("Apply").on_press(Msg::Preview(tb.pid, PreviewMsg::AnnotColorApply)),
    );
    let buttons = widget::row(vec![cancel, apply]).spacing(8.0);
    let col = widget::column(vec![Element::from(wheel), buttons.into()])
        .spacing(10.0)
        .align_x(Alignment::Center)
        .width(Length::Fixed(PICKER_W));
    tb.panel_container(col)
}

fn annot_palette_swatch(pid: window::Id, color: [u8; 4], selected: bool) -> Element<'static, Msg> {
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
            .on_press(Msg::Preview(pid, PreviewMsg::SetAnnotColor(color))),
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

    // ── the declared annotation tray (DRAGON-340) ────────────────────────────────────

    /// Every tool in the tray, flattened in render order.
    fn tray_tools() -> Vec<Tool> {
        ANNOT_TRAY
            .iter()
            .flat_map(|g| g.items.iter())
            .filter_map(|i| match i {
                TrayItem::Tool(_, t, _, _) => Some(*t),
                TrayItem::DimSlider => None,
            })
            .collect()
    }

    /// THE tool inventory: the tray carries every annotation tool exactly once, in the owner's
    /// declared order. A new `Tool` variant that isn't placed in a group will fail here rather
    /// than silently going missing from the toolbar.
    #[test]
    fn the_tray_carries_every_annotation_tool_exactly_once() {
        assert_eq!(
            tray_tools(),
            vec![
                Tool::Pointer,
                Tool::Arrow,
                Tool::Badge,
                Tool::Pixelate,
                Tool::Blur,
                Tool::Highlight,
                Tool::Rect,
                Tool::BoxHighlight,
                Tool::Spotlight,
                Tool::Pen,
                Tool::Eraser,
            ]
        );
        // No duplicates (the same tool in two groups would light up twice).
        let mut seen: Vec<&'static str> = tray_tools().iter().map(|t| t.as_str()).collect();
        let n = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), n, "a tool appears in more than one group");
    }

    /// The declared GROUPING itself — the thing the owner reassigns. One border per group, in
    /// this order; the dim slider rides the spotlight's group rather than standing alone.
    #[test]
    fn the_tray_groups_match_the_declared_layout() {
        let shape: Vec<usize> = ANNOT_TRAY.iter().map(|g| g.items.len()).collect();
        assert_eq!(shape, vec![1, 2, 2, 5, 2]);
        // A ONE-member group exists and is rendered through the very same container as the
        // others — there is no single-member branch anywhere in the builder.
        assert!(shape.contains(&1));
        assert!(matches!(ANNOT_TRAY[3].items[4], TrayItem::DimSlider));
    }

    /// The per-group CHROME is declared, not inferred: only the pointer (selection) group is
    /// `Bare` — every family of draw tools keeps the bordered surface. A future re-style moves
    /// these flags and touches no builder code.
    #[test]
    fn only_the_pointer_group_is_bare() {
        for (i, g) in ANNOT_TRAY.iter().enumerate() {
            let holds_pointer = g
                .items
                .iter()
                .any(|it| matches!(it, TrayItem::Tool(_, Tool::Pointer, _, _)));
            let want = if holds_pointer { ClusterChrome::Bare } else { ClusterChrome::Surface };
            assert!(g.chrome == want, "group {i} chrome");
        }
    }

    /// Every tray entry advertises a hotkey action that lives in the PREVIEW context, so its
    /// tooltip resolves a real live binding.
    #[test]
    fn every_tray_tool_binds_a_preview_action() {
        use crate::shortcuts::{Context, Keymap};
        let km = Keymap::defaults();
        for group in ANNOT_TRAY {
            for entry in group.items {
                let TrayItem::Tool(_, _, name, action) = entry else { continue };
                assert_eq!(action.context(), Context::Preview, "{name}");
                assert!(km.get(*action).is_some(), "{name} has no default binding");
            }
        }
    }

    // ── preview-editor slider thumbs ─────────────────────────────────────────────────

    /// The preview's thumb scale is applied to the handle and NOTHING else — the rail and
    /// colours stay exactly as the theme resolved them.
    #[test]
    fn the_preview_thumb_scale_only_touches_the_handle() {
        use cosmic::iced::widget::slider::{Handle, HandleShape, Rail, Style};
        let stock = Style {
            rail: Rail {
                backgrounds: (
                    Background::Color(cosmic::iced::Color::WHITE),
                    Background::Color(cosmic::iced::Color::BLACK),
                ),
                border: Border::default(),
                width: 4.0,
            },
            handle: Handle {
                shape: HandleShape::Rectangle { width: 20, height: 20, border_radius: 8.0.into() },
                background: Background::Color(cosmic::iced::Color::WHITE),
                border_width: 3.0,
                border_color: cosmic::iced::Color::BLACK,
            },
            breakpoint: cosmic::iced::widget::slider::Breakpoint {
                color: cosmic::iced::Color::WHITE,
            },
        };
        let scaled = thumb_scaled(stock);
        // Halved (the documented starting point), silhouette preserved.
        let HandleShape::Rectangle { width, height, border_radius } = scaled.handle.shape else {
            panic!("the cosmic thumb is a rounded rectangle");
        };
        assert_eq!((width, height), (10, 10));
        assert_eq!(border_radius.top_left, 4.0);
        assert_eq!(scaled.handle.border_width, 1.5);
        // The rail is untouched.
        assert_eq!(scaled.rail.width, 4.0);
        // And the knob really is the documented fraction.
        assert_eq!(PREVIEW_SLIDER_THUMB, 0.5);
    }

    /// A circular handle scales too, and a thumb never rounds away to nothing.
    #[test]
    fn a_circular_thumb_scales_and_never_vanishes() {
        use cosmic::iced::widget::slider::HandleShape;
        let mut s = thumb_scaled(cosmic::iced::widget::slider::Style {
            rail: cosmic::iced::widget::slider::Rail {
                backgrounds: (
                    Background::Color(cosmic::iced::Color::WHITE),
                    Background::Color(cosmic::iced::Color::WHITE),
                ),
                border: Border::default(),
                width: 4.0,
            },
            handle: cosmic::iced::widget::slider::Handle {
                shape: HandleShape::Circle { radius: 9.0 },
                background: Background::Color(cosmic::iced::Color::WHITE),
                border_width: 0.0,
                border_color: cosmic::iced::Color::WHITE,
            },
            breakpoint: cosmic::iced::widget::slider::Breakpoint {
                color: cosmic::iced::Color::WHITE,
            },
        });
        assert!(matches!(s.handle.shape, HandleShape::Circle { radius } if radius == 4.5));
        // A 1px rectangular thumb floors at 1 rather than rounding to a zero-size handle.
        s.handle.shape = HandleShape::Rectangle { width: 1, height: 1, border_radius: 0.0.into() };
        let s = thumb_scaled(s);
        assert!(matches!(s.handle.shape, HandleShape::Rectangle { width: 1, height: 1, .. }));
    }
}
