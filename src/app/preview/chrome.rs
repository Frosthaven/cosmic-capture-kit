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
        self.tool_button_tinted(name, tip, msg, pos, None)
    }

    /// [`Self::tool_button`] with an optional explicit glyph COLOUR — the seam DRAGON-353's
    /// unsaved-changes warning tint rides. `None` leaves the glyph on the `Button::Icon`
    /// class's own colour, so an untinted call is byte-identical to the historical button.
    pub(super) fn tool_button_tinted(
        self,
        name: &'static str,
        tip: String,
        msg: PreviewMsg,
        pos: widget::tooltip::Position,
        tint: Option<fn(&cosmic::Theme) -> cosmic::iced::Color>,
    ) -> Element<'static, Msg> {
        let icon = widget::icon::icon(crate::widgets::icons::handle(name))
            .width(Length::Fixed(self.icon_box()))
            .height(Length::Fixed(self.icon_box()));
        let icon = match tint {
            Some(tint) => icon.class(cosmic::theme::Svg::Custom(std::rc::Rc::new(
                move |t: &cosmic::Theme| cosmic::widget::svg::Style { color: Some(tint(t)) },
            ))),
            None => icon,
        };
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
    pub(super) fn tool_cluster<'a>(
        self,
        items: Vec<Element<'a, Msg>>,
        chrome: ClusterChrome,
    ) -> Element<'a, Msg> {
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

    /// THE shared DROPDOWN-MENU surface (DRAGON-357 items 15 + 17): an OPAQUE popover panel —
    /// the standard component surface at full alpha (never frosted/translucent, unlike a
    /// flyout), a 1px divider outline, PANEL (`s`) rounding, and a small inset. Every editor
    /// dropdown menu (the text SIZE list, the text FONT list, the bottom-bar Fit/scale list)
    /// wears this ONE look so they read as a single control family. `width` fixes the menu
    /// width so it never resizes as its rows change.
    pub(super) fn menu_container<'a>(
        self,
        content: impl Into<Element<'a, Msg>>,
        width: f32,
    ) -> Element<'a, Msg> {
        widget::container(content)
            .width(Length::Fixed(width))
            .padding(4.0)
            .class(cosmic::theme::Container::custom(move |theme| {
                let c = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    // Opaque component base — the standard menu/popover surface, NO frost.
                    background: Some(Background::Color(c.background.component.base.into())),
                    border: Border {
                        radius: crate::app::theme::rounding(theme).s.into(),
                        width: 1.0,
                        color: c.background.divider.into(),
                    },
                    ..Default::default()
                }
            }))
            .into()
    }

    /// THE action group (DRAGON-353): Save / Save As / Copy / Delete, in one capsule.
    ///
    /// Delete joined the share actions here (it used to sit beside the size chip, which
    /// now stands alone — see [`Self::size_chip`]) so every way of DISPOSING of the capture
    /// reads as one set. `--preview` (`external`) files drop Delete: that file is the
    /// user's, not ours to remove.
    ///
    /// **The dirty tint**: while the document has unsaved edits, the three SHARING actions
    /// wear the accent's COMPANION colour (its complement — see
    /// [`super::annotate::companion`]) — each of them is the thing that would commit or
    /// carry those edits, so "there is something here you haven't dealt with" belongs on
    /// exactly those glyphs. Delete never takes the tint: it destroys rather than commits,
    /// and inviting the eye to it while work is unsaved would be a trap.
    pub(super) fn share_group(
        self,
        dirty: bool,
        external: bool,
        km: &crate::shortcuts::Keymap,
    ) -> Element<'static, Msg> {
        use crate::shortcuts::Action;
        // Top toolbar → tooltips drop below.
        let pos = widget::tooltip::Position::Bottom;
        // One shared tint for the three sharing actions, so they can never disagree about
        // whether the document is dirty. The accent's COMPANION (its complement), not the
        // WARNING amber — an unsaved document is a normal working state, not an alarm.
        let tint: Option<fn(&cosmic::Theme) -> cosmic::iced::Color> =
            dirty.then_some(super::annotate::companion as fn(&cosmic::Theme) -> cosmic::iced::Color);
        let mut buttons = vec![
            self.tool_button_tinted(
                "document-save-symbolic",
                action_tip("Save", Action::PreviewSave, km),
                PreviewMsg::Save,
                pos,
                tint,
            ),
            self.tool_button_tinted(
                "document-save-as-symbolic",
                action_tip("Save As", Action::PreviewSaveAs, km),
                PreviewMsg::SaveAs,
                pos,
                tint,
            ),
            self.tool_button_tinted(
                // DRAGON-357: the lucide clipboard-copy glyph, tooltip "Copy to clipboard" with
                // the live hotkey appended by `action_tip`.
                "clipboard-copy-symbolic",
                action_tip("Copy to clipboard", Action::PreviewCopy, km),
                PreviewMsg::Copy,
                pos,
                tint,
            ),
        ];
        if !external {
            // A plain (non-destructive-styled, never warning-tinted) trash button.
            buttons.push(self.tool_button(
                "edit-delete-symbolic",
                action_tip("Delete", Action::PreviewDelete, km),
                PreviewMsg::Delete,
                pos,
            ));
        }
        // DRAGON-357: the same bordered cluster the top-left tray groups wear (the per-button
        // companion tint on Save/Save As/Copy and the untinted Delete are unchanged — only the
        // group container gains its 1px border).
        self.tool_cluster(buttons, ClusterChrome::Surface)
    }

    /// The saved file's SIZE, in a capsule of its own (DRAGON-353 split it out of the old
    /// size+Delete group, so the actions read as actions and the size reads as information).
    /// `None` before the file exists.
    pub(super) fn size_chip(self, size: Option<u64>) -> Option<Element<'static, Msg>> {
        let bytes = size?;
        // Match a button's inner box (icon + its vertical padding) so the chip is the same
        // height as the action group; the text stays vertically centred within it.
        let label = widget::container(widget::text(friendly_size(bytes)).size(13))
            .height(Length::Fixed(self.icon_box() + 2.0 * self.btn_pad()))
            .padding([0.0, self.btn_pad()])
            .align_y(Alignment::Center);
        Some(self.tool_group(vec![label.into()]))
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

// THE MEDIA AREA CANNOT BE FADED in this iced — the standing finding from DRAGON-365, kept
// because it is expensive to re-derive. A DISSOLVE (the picture's own visibility going to zero)
// is not expressible in iced 0.14 (pinned via libcosmic): the only opacity exposed is
// per-widget on the raster widgets (`image` / `svg`'s `.opacity()`), and the `Renderer` trait
// carries no alpha layer (`start_layer` / `start_transformation` only), so nothing generic can
// fade an `Element` — least of all this one, which mixes `widget::image`, the `annotation_fx`
// pass, the covermark/text `LayerStack` and `AnnotationCanvas` geometry (custom wgpu/canvas
// primitives widget opacity cannot reach). An opaque scrim standing in for it was built and
// explicitly REJECTED: no animation beats a fake transparency. DRAGON-371 then removed the
// timed close-hold the fade was meant to cover, so there is no longer even an interval to
// animate — the editor closes outright. Do not re-add a scrim.

/// Compose the loaded preview from its parts. In WINDOWED mode the edit + action toolbars
/// pin to the top / bottom of the window on full-width bars (a standard, light/dark-respecting
/// surface colour), with the image filling the space between. In OVERLAY mode they centre
/// together as one floating group (the historical look), led by `overlay_header` — the
/// appearance / undo / redo / Close line (DRAGON-337), which the WINDOWED arm never gets
/// (those controls live in its titlebar) and so always passes `None`.
///
/// `toasts` (DRAGON-353) is stacked over the MEDIA element in both arms — never over the
/// column — which is what keeps a toast inside the picture area and clear of the toolbars,
/// the annotation tray and the video transport strip at any window size. The stack itself is
/// UNCONDITIONAL (DRAGON-375): a toast coming or going must not change the tree SHAPE under
/// this position, or iced discards the media subtree's state mid-gesture — see below.
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
    toasts: Option<Element<'a, Msg>>,
) -> Element<'a, Msg> {
    // The toast rides a stack over the media — and that stack is UNCONDITIONAL (DRAGON-375).
    //
    // WHY a placeholder instead of `match toasts`: iced reconciles state by TREE SHAPE. When the
    // widget at a position changes TAG, `Tree::diff` throws the whole subtree away
    // (`*self = Tree::new(..)`) and every widget under it loses its internal state. Swapping
    // `stack([media, toast])` for a bare `media` did exactly that to the `AnnotationCanvas`,
    // whose `State` holds the IN-FLIGHT gesture (what is being drawn, the press anchor, whether
    // the press has moved past the click threshold). The first interaction with a fresh document
    // shortens the opening toast to `TOAST_INTERACTION_TTL` (750 ms), so the toast expired
    // squarely inside the user's first stroke and aborted it; every stroke afterwards was fine
    // because by then the shape no longer changed. A toast APPEARING mid-gesture is the same
    // mechanism in the other direction, and the unconditional stack covers both.
    //
    // `Space::new()` is `Shrink`×`Shrink`, i.e. zero-size but NOT `Size::is_void` — void children
    // are silently DROPPED by `Stack::push`, which would put the child count (and so the bug)
    // straight back. Only the FIRST child dictates the stack's sizing, so the placeholder cannot
    // affect layout, and `Space` never captures a pointer event.
    let image: Element<'a, Msg> = cosmic::iced::widget::stack(vec![
        image,
        toasts.unwrap_or_else(|| widget::Space::new().into()),
    ])
    .into();
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

/// Which background state a [`Tb::tool_toggle`] button is painting — the theme's own
/// `icon_button` component colour for each, so the toggle keeps the stock `Button::Icon`
/// hover/press feedback while `tool_toggle_style` overlays the active accent ring.
#[derive(Clone, Copy)]
enum ToggleBg {
    Rest,
    Hover,
    Press,
}

/// The button [`Style`](cosmic::widget::button::Style) a [`Tb::tool_toggle`] paints (DRAGON-357
/// item 4). It reconstructs the stock `Button::Icon` look from the theme's `icon_button`
/// component (transparent rest fill, its hover/press tints) and, when the toggle is ON, adds a
/// 1px ACCENT border drawn INSIDE the button bounds — so the armed control gains an accent ring
/// with ZERO footprint change (the resting and active boxes are the same size, no reflow). The
/// glyph colour is set separately on the SVG (accent when on); this owns only fill + border +
/// the keyboard-focus outline. Cross-platform by construction: pure libcosmic theme tokens, no
/// platform arm.
fn tool_toggle_style(
    theme: &cosmic::Theme,
    on: bool,
    focused: bool,
    bg: ToggleBg,
) -> cosmic::widget::button::Style {
    let comp = &theme.cosmic().icon_button;
    let fill: cosmic::iced::Color = match bg {
        ToggleBg::Rest => comp.base.into(),
        ToggleBg::Hover => comp.hover.into(),
        ToggleBg::Press => comp.pressed.into(),
    };
    let mut style = cosmic::widget::button::Style::new();
    style.background = Some(Background::Color(fill));
    style.border_radius = crate::app::theme::rounding(theme).xl.into();
    if on {
        style.border_width = 1.0;
        style.border_color = crate::app::theme::accent(theme);
    }
    // Preserve the stock icon button's keyboard-focus ring (an accent outline) so tabbing to a
    // toggle still reads — the base `Button::Icon` drew this and a bare `Custom` would drop it.
    if focused {
        style.outline_width = 1.0;
        style.outline_color = crate::app::theme::accent(theme);
    }
    style
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

    // (see `tool_toggle_style` below for the active-border custom class the toggle wears.)

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
        // DRAGON-357 item 4: the ACTIVE toggle gains a 1px accent border, drawn INSIDE the
        // button bounds (`tool_toggle_style`) so the footprint is byte-identical in both states
        // (no reflow on selection). The Icon button's own base/hover/pressed backgrounds are
        // reconstructed from the theme's `icon_button` component so hover feedback is preserved;
        // the glyph colour still rides the SVG class above (accent when on). Shared by EVERY
        // toggle group — the top tray tools, the pointer/pan pair and the seven line-width
        // segments — so the accent ring appears consistently on whichever is armed.
        let btn = crate::widgets::arrow_cursor::arrow_cursor(
            widget::button::custom(
                widget::container(icon)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center),
            )
            .class(cosmic::theme::Button::Custom {
                active: Box::new(move |focused, t| {
                    tool_toggle_style(t, on, focused, ToggleBg::Rest)
                }),
                disabled: Box::new(move |t| tool_toggle_style(t, on, false, ToggleBg::Rest)),
                hovered: Box::new(move |focused, t| {
                    tool_toggle_style(t, on, focused, ToggleBg::Hover)
                }),
                pressed: Box::new(move |focused, t| {
                    tool_toggle_style(t, on, focused, ToggleBg::Press)
                }),
            })
            .on_press(Msg::Preview(self.pid, msg))
            .padding(self.btn_pad()),
        );
        self.tip(btn, tip, tip_pos)
    }

    /// One segment of a SEGMENTED toggle, styled exactly like the region toolbar's
    /// scanner/image/video selector: the active segment is filled accent with an on-accent
    /// glyph, the others sit on the group's divider fill; only the pair's outer corners round.
    /// Still used by the VIDEO timeline's pointer/scissor toggle (`video.rs`); the image
    /// editor's cursor/pan + line-width groups moved to the bare [`Self::tool_toggle`] look
    /// (DRAGON-357 item 10).
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
        let glyph = widget::icon::icon(crate::widgets::icons::handle(icon))
            .size(64)
            .width(Length::Fixed(self.icon_box()))
            .height(Length::Fixed(self.icon_box()));
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

    /// The pointer / pan (grabby-hand) tool toggle. DRAGON-357 item 10: the cursor/pan buttons
    /// wear the SAME bare white/accent glyph treatment as the top tray's tool buttons
    /// ([`Self::tool_toggle`]) — accent when active, foreground when not, no segment fill or ring
    /// — instead of the old segmented-capsule look. Lives on the BOTTOM bar, so tooltips rise.
    pub(super) fn pan_tool_group(
        self,
        pan_mode: bool,
        km: &crate::shortcuts::Keymap,
    ) -> Element<'static, Msg> {
        let pos = widget::tooltip::Position::Top;
        // The pointer/pan toggle's hotkey rides BOTH glyphs.
        let tip = |name: &str| {
            widget::text(action_tip(name, crate::shortcuts::Action::PreviewTogglePan, km))
                .size(12)
                .into()
        };
        self.tool_cluster(
            vec![
                self.tool_toggle(
                    "input-mouse-symbolic",
                    !pan_mode,
                    PreviewMsg::SetPanMode(false),
                    tip("Pointer"),
                    pos,
                ),
                self.tool_toggle(
                    "object-move-symbolic",
                    pan_mode,
                    PreviewMsg::SetPanMode(true),
                    tip("Pan"),
                    pos,
                ),
            ],
            ClusterChrome::Surface,
        )
    }

    /// The annotation stroke-width toggle group: Thin (2px) / Medium (5px) / Thick (8px). Each
    /// glyph is a horizontal line at the matching thickness; the one NEAREST `current` reads as
    /// active. DRAGON-357 item 10: the same bare white/accent [`Self::tool_toggle`] treatment as
    /// the pan + tray tools (no segment fill). Lives on the BOTTOM bar, so tooltips rise.
    pub(super) fn stroke_width_group(
        self,
        current: f32,
        km: &crate::shortcuts::Keymap,
    ) -> Element<'static, Msg> {
        use super::annotate::{stroke_width_nearest_index, STROKE_WIDTHS};
        let active = stroke_width_nearest_index(current);
        let pos = widget::tooltip::Position::Top;
        // (glyph name, tooltip) per preset, thin → thick — one entry per [`STROKE_WIDTHS`] (item
        // 9: seven presets). No px in the tooltip — the rendered thickness varies by tool (e.g.
        // arrows render thicker), and `action_tip` appends the live hotkey. The width group is
        // driven by ONE cycle hotkey (PreviewAnnotStrokeCycle). The glyph's OWN stroke scales
        // with the preset so all seven are visually distinguishable.
        let specs = [
            ("minus-1", "Thinnest line"),
            ("minus-2", "Thinner line"),
            ("minus-4", "Thin line"),
            ("minus-6", "Medium line"),
            ("minus-8", "Thick line"),
            ("minus-10", "Thicker line"),
            ("minus-12", "Thickest line"),
        ];
        debug_assert_eq!(specs.len(), STROKE_WIDTHS.len());
        let segs: Vec<Element<'static, Msg>> = specs
            .iter()
            .enumerate()
            .map(|(i, (icon, tip))| {
                self.tool_toggle(
                    icon,
                    i == active,
                    PreviewMsg::SetAnnotStrokeW(STROKE_WIDTHS[i]),
                    widget::text(action_tip(tip, crate::shortcuts::Action::PreviewAnnotStrokeCycle, km))
                        .size(12)
                        .into(),
                    pos,
                )
            })
            .collect();
        widget::row(segs).spacing(2.0).align_y(Alignment::Center).into()
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
    /// The windowed preview's TITLEBAR controls (DRAGON-337): the fullscreen-overlay toggle,
    /// then Settings (DRAGON-353) — both accent, being chrome rather than edit actions —
    /// followed by undo / redo (full-strength foreground when their stacks have entries,
    /// subdued + inert when empty). Returned in leading-edge order.
    pub(super) fn preview_header_controls(
        &self,
        preview: &PreviewState,
    ) -> Vec<Element<'static, Msg>> {
        let pid = preview.window;
        let mut items = vec![
            titlebar_button(
                pid,
                "view-fullscreen-symbolic",
                "Fullscreen overlay".to_string(),
                PreviewMsg::ToggleAppearance,
                crate::app::theme::accent,
            ),
            // DRAGON-353: Settings, immediately to the RIGHT of the appearance toggle — the
            // same slot the overlay header puts it in, so the two chromes agree.
            titlebar_button(
                pid,
                "emblem-system-symbolic",
                "Settings".to_string(),
                PreviewMsg::OpenSettings,
                crate::app::theme::accent,
            ),
        ];
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
        // DRAGON-357 item 7 (redo): the slider-adjacent glyph (covermark zoom/opacity,
        // spotlight/dim) must read at 50% opacity so it sits as a quiet LABEL beside its rail,
        // not a solid control.
        //
        // TRUE alpha is IMPOSSIBLE through the icon widget, and the earlier `c.a *= 0.6` pass was
        // silently discarded (the glyph stayed fully opaque): libcosmic's wgpu SVG rasterizer
        // (iced/wgpu/src/image/vector.rs, `Cache::upload`) applies the colour FILTER by
        // overwriting ONLY the RGB channels of every non-transparent texel and LEAVING ALPHA
        // untouched — `if rgba[3] > 0 { rgba[0] = color[0]; rgba[1] = color[1]; rgba[2] =
        // color[2]; }` — so the alpha we hand it in `svg::Style.color` never reaches a pixel
        // (it is part of the rasterization CACHE KEY but is applied to nothing). The mac build
        // renders through exactly that wgpu path.
        //
        // Fallback (as specced): BLEND the foreground 50% toward the cluster's SURFACE colour
        // (`background.component.base`, the fill `tool_cluster` paints behind this row), yielding
        // an OPAQUE colour that reproduces a 50%-opacity foreground glyph on that surface. Theme
        // tokens only (same `foreground` ramp the sibling toggle glyphs ride), no hardcoded
        // colour, no platform arm — so it tracks light/dark and the user's accent.
        let glyph = widget::icon::icon(crate::widgets::icons::handle(icon))
            .width(Length::Fixed(self.icon_box() * SLIDER_SCALE))
            .height(Length::Fixed(self.icon_box() * SLIDER_SCALE))
            .class(cosmic::theme::Svg::Custom(std::rc::Rc::new(|t: &cosmic::Theme| {
                let fg = crate::app::theme::foreground(t);
                let bg: cosmic::iced::Color = t.cosmic().background.component.base.into();
                let mix = |a: f32, b: f32| a + (b - a) * 0.5;
                let c = cosmic::iced::Color::from_rgb(
                    mix(fg.r, bg.r),
                    mix(fg.g, bg.g),
                    mix(fg.b, bg.b),
                );
                cosmic::widget::svg::Style { color: Some(c) }
            })));
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
    /// carrying the windowed-swap toggle, Settings (DRAGON-353) and undo / redo on the LEFT,
    /// with Close pushed to the far RIGHT by the split. Every button here is FLAT (a bare tinted glyph, no
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
        let mut left = vec![
            tb.header_button(
                "view-restore-symbolic",
                "Windowed".to_string(),
                PreviewMsg::ToggleAppearance,
                crate::app::theme::accent,
            ),
            // DRAGON-353: Settings sits immediately right of the appearance toggle here too.
            // Clicking it DEMOTES this overlay to a window first (an exclusive layer surface
            // would cover the settings window) — see `App::open_settings_from_preview`.
            tb.header_button(
                "emblem-system-symbolic",
                "Settings".to_string(),
                PreviewMsg::OpenSettings,
                crate::app::theme::accent,
            ),
        ];
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
        // DRAGON-357: on the macOS OVERLAY, Close sits to the LEFT of the Windowed toggle
        // (mac close-on-left convention) — scoped to this per-platform arm so every other OS
        // keeps Close on the right, byte-identical. The windowed preview gets Close from the
        // native window chrome, so this only affects the overlay row.
        #[cfg(target_os = "macos")]
        {
            let mut row = Vec::with_capacity(left.len() + 1);
            row.push(close);
            row.extend(left);
            toolbar_row(row, Vec::new(), Vec::new())
        }
        #[cfg(not(target_os = "macos"))]
        {
            toolbar_row(left, Vec::new(), vec![close])
        }
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
        // Right: the Save / Save As / Copy / Delete action group — the sharing actions tinted
        // with the accent's companion while there are unsaved edits (Delete stays untinted).
        // (DRAGON-357 item 8: for IMAGES the filesize block moved to the BOTTOM bar, before the
        // scaling slider — see `image.rs`. VIDEOS have no bottom zoom cluster, so their filesize
        // stays here on the top bar.)
        let mut right: Vec<Element<'a, Msg>> = Vec::new();
        if matches!(preview.kind, PreviewKind::Video(_)) {
            right.extend(tb.size_chip(preview.size));
        }
        right.push(tb.share_group(preview.unsaved(), preview.external, km));
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
            // DRAGON-357: the color swatch + the three line-width segments share ONE bordered
            // cluster (the same container the top-left tray groups use), instead of two
            // borderless controls.
            groups.push(tb.tool_cluster(
                vec![
                    annot_swatch_flyout(preview, &self.annot_recent_colors, &self.keymap, tb),
                    tb.stroke_width_group(e.stroke(), &self.keymap),
                ],
                ClusterChrome::Surface,
            ));
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
        // DRAGON-357: the covermark toggle (plus its zoom/opacity sliders when applied) rides a
        // bordered cluster like every other bottom-bar group, not its own borderless surface.
        groups.push(tb.tool_cluster(covermark_items, ClusterChrome::Surface));
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
    /// Anchor on the TOP toolbar → expand DOWNWARD (DRAGON-357 item 14). Uses the popover's own
    /// `Position::Bottom` (the popup's top at the anchor button's bottom, centered under it), so
    /// the top-bar dropdowns (text SIZE + text FONT) open DOWN into the canvas — no reliance on a
    /// hand-guessed anchor height.
    Down,
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
    // Up offsets by a KNOWN panel height (`Point`, left-aligned); Down uses the popover's own
    // `Bottom` (anchor-height-aware, no guessing).
    let position = match dir {
        FlyoutDir::Up(panel_h) => {
            widget::popover::Position::Point(cosmic::iced::Point::new(0.0, -panel_h))
        }
        FlyoutDir::Down => widget::popover::Position::Bottom,
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
        .position(position)
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

/// One GROUP of the annotation tray: its chrome treatment, its optional one-key tool SLOT,
/// plus its ordered items.
struct TrayGroup {
    /// Whether this group's [`Tb::tool_cluster`] paints a surface + border, or only occupies
    /// the footprint.
    chrome: ClusterChrome,
    /// The group's CYCLE action, when its tools share ONE hotkey (DRAGON-369) — pressing it
    /// arms the group's current member, pressing it again advances to the next, wrapping.
    /// `None` = every tool in the group owns its own key (or has none).
    ///
    /// Declaring the slot HERE is what makes the tray the single source of truth for the
    /// cycle: membership is the group's tools and the cycle ORDER is their declared order, so
    /// the keyboard order and the visible left-to-right button order are the same fact. Read
    /// it through [`slot_tools`] / [`slot_for_tool`]; the per-slot cursor is runtime preview
    /// state ([`super::edit::EditState::slot_cursor`]).
    cycle: Option<crate::shortcuts::Action>,
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
    TrayGroup { chrome: ClusterChrome::Bare, cycle: None, items: &[TrayItem::Tool(
        "pointer-select-symbolic",
        Tool::Pointer,
        "Select",
        crate::shortcuts::Action::PreviewAnnotPointer,
    )] },
    // Callouts: point at a thing, or number the steps. NO cycle key — the arrow is the
    // highest-frequency tool in a screenshot annotator and must never cost two presses, and
    // the step marker takes Photoshop's Count key.
    TrayGroup { chrome: ClusterChrome::Surface, cycle: None, items: &[
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
    // Destructive redactions: the pixelate mosaic and the strong blur. ONE family (both are
    // drag-a-rect destructive redactions), so ONE cycle key — `M` (DRAGON-369).
    TrayGroup {
        chrome: ClusterChrome::Surface,
        cycle: Some(crate::shortcuts::Action::PreviewAnnotRedactCycle),
        items: &[
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
        ],
    },
    // SHAPES: the three tools that draw an emphasis shape ON the image, sharing Photoshop's
    // own shape key `U` (DRAGON-369). This group was carved out of the old "Emphasis" group so
    // that a tray chip and a hotkey slot are the SAME set — the spotlight below is not a shape.
    TrayGroup {
        chrome: ClusterChrome::Surface,
        cycle: Some(crate::shortcuts::Action::PreviewAnnotShapeCycle),
        items: &[
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
        ],
    },
    // The spotlight and the dim range it works against: a global dim with a knockout rather
    // than a shape drawn on the image, and the only tool that owns a slider — hence its own
    // group and its own solo key (`S`), outside the shape slot.
    TrayGroup { chrome: ClusterChrome::Surface, cycle: None, items: &[
        TrayItem::Tool(
            "spotlight-symbolic",
            Tool::Spotlight,
            "Spotlight",
            crate::shortcuts::Action::PreviewAnnotSpotlight,
        ),
        TrayItem::DimSlider,
    ] },
    // Text captions (DRAGON-354): the lucide `type` glyph arms the text tool. Its size
    // dropdown + font toggle are injected right after this group in `annotation_tools` (they
    // are a flyout + toggle, not tray tool-buttons, so they can't live in the data-driven tray).
    TrayGroup { chrome: ClusterChrome::Surface, cycle: None, items: &[
        TrayItem::Tool(
            "text-tool-symbolic",
            Tool::Text,
            "Text",
            crate::shortcuts::Action::PreviewAnnotText,
        ),
    ] },
    // Freehand ink and the hand-undo that removes it. NO cycle key: inking and erasing are
    // the pair you alternate between fastest, so they keep a key each (`B` / `E`).
    TrayGroup { chrome: ClusterChrome::Surface, cycle: None, items: &[
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

/// The ordered TOOLS of the slot whose cycle action is `cycle` — the cycle's membership AND
/// its order, read straight off [`ANNOT_TRAY`] (DRAGON-369) so there is no second declaration
/// to drift. Empty for an action that names no slot, which makes a stray cycle message a
/// harmless no-op.
pub(super) fn slot_tools(cycle: crate::shortcuts::Action) -> Vec<Tool> {
    ANNOT_TRAY
        .iter()
        .filter(|g| g.cycle == Some(cycle))
        .flat_map(|g| g.items.iter())
        .filter_map(|i| match i {
            TrayItem::Tool(_, t, _, _) => Some(*t),
            TrayItem::DimSlider => None,
        })
        .collect()
}

/// The cycle action of the slot `tool` belongs to, if any — how arming a tool by ANY route
/// (tray click included) moves that slot's cursor, so keyboard and mouse can never disagree.
pub(super) fn slot_for_tool(tool: Tool) -> Option<crate::shortcuts::Action> {
    ANNOT_TRAY
        .iter()
        .find(|g| {
            g.cycle.is_some()
                && g.items
                    .iter()
                    .any(|i| matches!(i, TrayItem::Tool(_, t, _, _) if *t == tool))
        })
        .and_then(|g| g.cycle)
}

/// The member a slot's cycle key arms next — **arm-then-advance** (DRAGON-369):
///
/// * the slot is NOT armed → arm its CURRENT member (`cursor`, i.e. whichever member was last
///   armed however it was armed — a tray click counts), falling back to the first;
/// * the slot IS armed → ADVANCE from the armed member to the next, wrapping.
///
/// There is deliberately **no timeout**: Photoshop has none, and a timeout would make one
/// keystroke mean two things depending on typing speed. Repeat work is the normal case, so the
/// two-press cost of a switch is paid once per switch-away, never per use.
pub(super) fn next_slot_tool(members: &[Tool], armed: Option<Tool>, cursor: Option<Tool>) -> Option<Tool> {
    let pos = armed.and_then(|t| members.iter().position(|m| *m == t));
    match pos {
        Some(i) => members.get((i + 1) % members.len()).copied(),
        None => cursor
            .filter(|c| members.contains(c))
            .or_else(|| members.first().copied()),
    }
}

/// The index of the TEXT group in [`ANNOT_TRAY`], after which the size dropdown + font toggle
/// are injected in [`annotation_tools`] (DRAGON-354). A test pins this to the group whose sole
/// tool is [`Tool::Text`] so a tray reorder can't silently misplace the size control.
const TEXT_TRAY_GROUP: usize = 5;

/// Which action's LIVE binding a tray button's tooltip should show (DRAGON-369): its OWN
/// per-tool action while that is bound, otherwise its group's cycle action.
///
/// The order mirrors how a press actually resolves ([`crate::shortcuts::Keymap::action_for`]
/// walks the per-tool actions before the slot actions), so the tip can never advertise a key
/// that a deliberate per-tool bind has taken over — and a slot member, which ships unbound,
/// still shows the key that reaches it instead of no key at all.
fn tip_action(
    own: crate::shortcuts::Action,
    cycle: Option<crate::shortcuts::Action>,
    km: &crate::shortcuts::Keymap,
) -> crate::shortcuts::Action {
    match cycle {
        Some(slot) if km.get(own).is_none() => slot,
        _ => own,
    }
}

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
        .enumerate()
        .map(|(gi, group)| {
            let mut items: Vec<Element<'static, Msg>> = group
                .items
                .iter()
                .map(|entry| match *entry {
                    TrayItem::Tool(icon, tool, name, action) => tb.tool_toggle(
                        icon,
                        active == Some(tool),
                        PreviewMsg::ToolPressed(tool),
                        widget::text(action_tip(name, tip_action(action, group.cycle, km), km))
                            .size(12)
                            .into(),
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
            // DRAGON-357 item 7: the text SIZE + FONT dropdowns live INSIDE the text tool's
            // cluster (one bordered group: icon + size dropdown + style dropdown), not beside it.
            if gi == TEXT_TRAY_GROUP {
                items.extend(text_style_controls(preview, tb));
            }
            tb.tool_cluster(items, group.chrome)
        })
        .collect();
    widget::row(groups)
        .spacing(tb.btn_pad())
        .align_y(Alignment::Center)
        .into()
}

/// The fixed menu width (px) of the text SIZE + FONT dropdowns — wide enough for "128px" (item
/// 8) / the font labels plus the row padding.
const TEXT_DROPDOWN_MENU_W: f32 = 96.0;

/// The fixed CLOSED-control width (px) of the text SIZE dropdown chip, sized for the widest
/// "128px" label (item 8) plus the chevron so the control never resizes as the selection changes
/// (DRAGON-357 item 3). The font chip's width is MEASURED from its own faces (see
/// [`text_font_ctrl_w`]); the Fit chip keeps its own `COMBO_W`.
const TEXT_SIZE_CTRL_W: f32 = 62.0;

/// The fixed CLOSED-control width (px) of the text FONT dropdown chip: the WIDER of "Hand"
/// (Excalifont) / "Clean" (Inter), each measured in the face it actually renders with (item 3's
/// requirement, met by construction with the embedded metrics — identical on every platform),
/// plus the chevron + padding. Derived, so a font swap can't silently under-size the chip.
fn text_font_ctrl_w() -> f32 {
    let label_w = text_annot::measure(text_annot::TextFont::Hand, TEXT_CHIP_LABEL_SIZE, text_annot::TextFont::Hand.label())
        .max(text_annot::measure(text_annot::TextFont::Clean, TEXT_CHIP_LABEL_SIZE, text_annot::TextFont::Clean.label()));
    // + chevron (10) + row spacing (3) + button h-padding (2 * 6), with a couple px of slack.
    label_w + 10.0 + 3.0 + 12.0 + 4.0
}

/// The label text size (px) of a CLOSED dropdown chip, and of a dropdown MENU row. Named because
/// a face-previewing label must measure its metrics at the SAME size it renders at
/// ([`cap_centered_label`]); an inline literal could drift from the `.size(..)` beside it.
const TEXT_CHIP_LABEL_SIZE: f32 = 12.0;
const TEXT_MENU_LABEL_SIZE: f32 = 13.0;

// ── Cap-band label centring (DRAGON-363) ─────────────────────────────────────────────────────
//
// A label that PREVIEWS a text face in its own face is placed vertically by cosmic-text's LINE-BOX
// rule: inside a line box of height `LH` the baseline lands at `LH/2 + (ascent - descent)/2`
// (`cosmic_text::Buffer`'s `centering_offset + max_ascent`), i.e. the box is centred on the face's
// ascent/descent band. Two faces sharing one row therefore put their visible glyphs at DIFFERENT
// heights whenever their vertical metrics differ — the DRAGON-363 report: "Hand" (Excalifont)
// rides high while "Clean" (Inter) and the neighbouring "32px" chip look centred.
//
// WHY those two look right: Inter's metrics satisfy `ascent - |descent| == capHeight` EXACTLY
// (2728 - 680 == 2048 design units), so for Inter the line-box rule already coincides with
// centring the CAP BAND — and every UI-font label here is Inter too (it is the COSMIC interface
// font). Excalifont does not: 886 - 374 = 512 against a 600-unit cap height, so its cap band sits
// 0.088em high — the gap above its capitals is ~1.06px smaller than the gap below at the 12px
// chip. Half of that difference (0.044em ≈ 0.53px) is the correction below.
//
// The RULE applied here is therefore CAP-BAND centring: put the band from the baseline to the
// face's cap height in the middle of the row. One rule for every face — no per-face constant, no
// lookup table, no `if font == Hand`; a third face added to the picker is corrected by the same
// arithmetic on its own metrics, and a face that already satisfies Inter's identity is left
// untouched by construction (shift 0.0).
//
// Why NOT ink-box (visual glyph extent) centring: it is STRING-dependent, and the very control the
// report names as correctly aligned disproves it — "32px" carries a 'p' descender, so its ink-box
// centre sits 0.098em BELOW where Inter's baseline rule puts it; ink centring would visibly move a
// control that is already right, and would make two menu rows with different ascenders/descenders
// ("Hand" vs a future "Jump") sit on different baselines.

/// The vertical metrics ONE face contributes to [`cap_center_shift_em`], in FONT UNITS plus the
/// face's em. `ttf-parser`'s `ascender`/`descender` walk the SAME FreeType ladder as the `skrifa`
/// metrics `cosmic-text` lays out with (OS/2 typographic metrics when the `USE_TYPO_METRICS` flag
/// is set, else `hhea`, else the OS/2 fallbacks), so these are the numbers that actually placed
/// the glyphs.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FaceVMetrics {
    /// Design units per em (`head.unitsPerEm`).
    upem: f32,
    /// Baseline → top of the alignment box, font units (positive up).
    ascent: f32,
    /// Baseline → bottom of the alignment box, font units, as a POSITIVE magnitude.
    descent: f32,
    /// Baseline → top of a capital, font units. `None` when the face declares no cap height and
    /// has no 'H' outline to stand in — then NO correction is applied and the label keeps the
    /// historical line-box placement.
    cap: Option<f32>,
}

/// Read [`FaceVMetrics`] off a parsed face. The cap ladder is OS/2 `sCapHeight` → the 'H' outline's
/// top (the same band, measured, for faces whose OS/2 table predates v2) → give up.
fn face_v_metrics(face: &ttf_parser::Face<'_>) -> FaceVMetrics {
    let cap = face
        .capital_height()
        .map(|v| v as f32)
        .or_else(|| {
            face.glyph_index('H')
                .and_then(|g| face.glyph_bounding_box(g))
                .map(|bb| bb.y_max as f32)
        })
        .filter(|c| *c > 0.0);
    FaceVMetrics {
        upem: face.units_per_em() as f32,
        ascent: face.ascender() as f32,
        // `descender` is negative (below the baseline); we carry the magnitude.
        descent: -(face.descender() as f32),
        cap,
    }
}

/// How far DOWN (in em) a label in this face must move so its CAP BAND — not its line box — is
/// centred in the row. Pure, and the whole of the rule: cosmic-text puts the baseline
/// `(ascent - descent)/2` below the row centre, cap-band centring wants it `cap/2` below, and the
/// shift is the difference. `0.0` for a face that already agrees (Inter) or declares no cap.
fn cap_center_shift_em(m: FaceVMetrics) -> f32 {
    let (Some(cap), true) = (m.cap, m.upem > 0.0) else { return 0.0 };
    (cap - (m.ascent - m.descent)) / (2.0 * m.upem)
}

/// The line box + padding that realise a cap-centring shift WITHOUT changing the element's outer
/// height: the line box is shortened by twice the shift's magnitude and the padding puts it back
/// asymmetrically, so `pad_top + line_h + pad_bottom` is always the label's natural line-box
/// height. That is what lets the chip and the menu rows keep their exact historical size while the
/// glyphs inside move — and it makes a zero shift byte-identical to the historical layout.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CapCenteredBox {
    line_h: f32,
    pad_top: f32,
    pad_bottom: f32,
}

fn cap_centered_box(size: f32, shift_em: f32) -> CapCenteredBox {
    use cosmic::iced::advanced::text::LineHeight;
    // iced's OWN default line box for this text size (so the corrected label keeps exactly the
    // height an uncorrected one has, whatever that default is).
    let box_h = LineHeight::default().to_absolute(cosmic::iced::Pixels(size)).0;
    // A correction may never eat the line box: clamp to a quarter of it. No sane face comes close
    // — Excalifont, the largest correction among the embedded faces, wants 0.53px of 4.2px at the
    // 12px chip.
    let shift = (shift_em * size).clamp(-box_h / 4.0, box_h / 4.0);
    CapCenteredBox {
        line_h: box_h - 2.0 * shift.abs(),
        pad_top: shift.abs() + shift,
        pad_bottom: shift.abs() - shift,
    }
}

/// The cap-centring shift (em) for `font`'s face, parsed ONCE per process from the SAME embedded
/// bytes the UI font system was registered with ([`text_annot::UI_FONT_FACES`], keyed by family
/// name — so a face added there and to `TextFont` is picked up with no change here). An
/// unparseable or unlisted family yields `0.0`: the historical placement, never a panic.
fn face_shift_em(font: text_annot::TextFont) -> f32 {
    static SHIFTS: std::sync::OnceLock<Vec<(&'static str, f32)>> = std::sync::OnceLock::new();
    let table = SHIFTS.get_or_init(|| {
        text_annot::UI_FONT_FACES
            .iter()
            .map(|(family, bytes)| {
                let shift = ttf_parser::Face::parse(bytes, 0)
                    .map(|f| cap_center_shift_em(face_v_metrics(&f)))
                    .unwrap_or(0.0);
                (*family, shift)
            })
            .collect()
    });
    table
        .iter()
        .find(|(family, _)| *family == font.family())
        .map_or(0.0, |(_, shift)| *shift)
}

/// Wrap a `label` rendered in `font`'s OWN face (already sized/classed by the caller, at `size`)
/// so it is CAP-BAND centred in its row instead of line-box centred — the DRAGON-363 fix, applied
/// identically to the closed chip and to the dropdown's menu rows.
fn cap_centered_label<'a>(
    font: text_annot::TextFont,
    size: f32,
    label: widget::Text<'a, cosmic::Theme, cosmic::Renderer>,
) -> Element<'a, Msg> {
    use cosmic::iced::advanced::text::LineHeight;
    let b = cap_centered_box(size, face_shift_em(font));
    widget::container(label.line_height(LineHeight::Absolute(cosmic::iced::Pixels(b.line_h))))
        .padding([b.pad_top, 0.0, b.pad_bottom, 0.0])
        .into()
}

/// The shared CLOSED dropdown-control chip (DRAGON-357 items 2/3/15/16): a `Button::Text` chip
/// showing `label` (already fonted/sized by the caller) left-aligned with a size-10
/// `pan-down-symbolic` chevron pinned to the right, at a FIXED `width` so the control never
/// resizes as its selection changes (item 3). `Button::Text` supplies the SAME hover
/// trim/background as every editor dropdown — so the px, Hand/Clean and Fit chips read as ONE
/// control family in both light and dark themes (item 2 + the hover addendum, since the hover
/// styling lives in this ONE shared class, not per call site). `toggle` opens/closes its menu.
pub(super) fn dropdown_chip<'a>(
    pid: window::Id,
    label: Element<'a, Msg>,
    width: f32,
    toggle: PreviewMsg,
) -> Element<'a, Msg> {
    crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::custom(
            widget::row(vec![
                widget::container(label).width(Length::Fill).into(),
                widget::icon::icon(crate::widgets::icons::handle("pan-down-symbolic"))
                    .size(10)
                    .into(),
            ])
            .spacing(3.0)
            .align_y(Alignment::Center),
        )
        .class(cosmic::theme::Button::Text)
        .padding([2.0, 6.0])
        .width(Length::Fixed(width))
        .on_press(Msg::Preview(pid, toggle)),
    )
}

/// The iced UI font that PREVIEWS a [`text_annot::TextFont`] in its own face (DRAGON-354 item
/// 19): "Hand" in Excalifont, "Clean" in Inter. The faces are registered with the UI font
/// system at startup (`App::init`, reusing the same embedded bytes as the rasterizer); if that
/// registration ever fails the label just falls back to the default UI font.
fn font_preview_face(font: text_annot::TextFont) -> cosmic::iced::Font {
    cosmic::iced::Font::with_name(font.family())
}

/// The text SIZE dropdown + FONT dropdown (DRAGON-354 / DRAGON-357), returned as the two
/// controls that sit INSIDE the text tool's bordered cluster (item 7). Both open DOWNWARD
/// (item 14) with an OPAQUE menu (item 15), and both apply to NEW text and the edited/selected
/// box. The size button shows the current px; the font button shows the current family's label
/// rendered in that family's own face (item 19).
fn text_style_controls(preview: &PreviewState, tb: Tb) -> Vec<Element<'static, Msg>> {
    let e = &preview.edit;
    let pid = tb.pid;
    let size = e.text_size();
    let font = e.annot_text_font;
    // ── SIZE dropdown ("24px ⌄") ──────────────────────────────────────────────────────────
    let size_btn: Element<'static, Msg> = dropdown_chip(
        pid,
        widget::text(format!("{}px", size.round() as i32)).size(12).into(),
        TEXT_SIZE_CTRL_W,
        PreviewMsg::ToggleTextSizeFlyout,
    );
    let size_ctrl: Element<'static, Msg> =
        if e.flyout_kind() == Some(super::edit::FlyoutKind::TextSize) {
            flyout(
                size_btn,
                text_size_panel(pid, size, e.flyout_selected(), tb),
                FlyoutDir::Down,
                Msg::Preview(pid, PreviewMsg::FlyoutClose),
            )
        } else {
            size_btn
        };
    // ── FONT dropdown ("Hand ⌄" / "Clean ⌄", label in its own face) (item 16) ──────────────
    // The label is CAP-BAND centred (DRAGON-363), not line-box centred, so every face's glyphs
    // sit at the same height in the chip as the UI-font "24px" chip beside it.
    let font_btn: Element<'static, Msg> = dropdown_chip(
        pid,
        cap_centered_label(
            font,
            TEXT_CHIP_LABEL_SIZE,
            widget::text(font.label()).size(TEXT_CHIP_LABEL_SIZE).font(font_preview_face(font)),
        ),
        text_font_ctrl_w(),
        PreviewMsg::ToggleTextFontFlyout,
    );
    let font_ctrl: Element<'static, Msg> =
        if e.flyout_kind() == Some(super::edit::FlyoutKind::TextFont) {
            flyout(
                font_btn,
                text_font_panel(pid, font, e.flyout_selected(), tb),
                FlyoutDir::Down,
                Msg::Preview(pid, PreviewMsg::FlyoutClose),
            )
        } else {
            font_btn
        };
    vec![size_ctrl, font_ctrl]
}

/// The text-size dropdown MENU (DRAGON-354): one row per [`text_annot::TEXT_SIZES`] entry, the
/// current/highlighted one in accent. Opaque menu surface (item 15); keyboard-navigable through
/// the shared flyout state (arrows move `selected`, Enter applies).
fn text_size_panel(
    pid: window::Id,
    current: f32,
    selected: Option<usize>,
    tb: Tb,
) -> Element<'static, Msg> {
    let rows: Vec<Element<'static, Msg>> = text_annot::TEXT_SIZES
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let hot = selected == Some(i) || (selected.is_none() && (s - current).abs() < 0.5);
            let label = if hot {
                widget::text(format!("{}px", s.round() as i32)).size(13).class(
                    cosmic::theme::Text::Custom(|t| cosmic::iced::widget::text::Style {
                        color: Some(crate::app::theme::accent(t)),
                        ..Default::default()
                    }),
                )
            } else {
                widget::text(format!("{}px", s.round() as i32)).size(13)
            };
            crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::custom(label)
                    .width(Length::Fill)
                    .class(cosmic::theme::Button::Text)
                    .on_press(Msg::Preview(pid, PreviewMsg::SetTextSize(s))),
            )
        })
        .collect();
    tb.menu_container(widget::column(rows).spacing(2.0), TEXT_DROPDOWN_MENU_W)
}

/// The text-font dropdown MENU (DRAGON-357 item 16): "Hand" (Excalifont) and "Clean" (Inter),
/// each label previewing its own face (item 19), the active one in accent. Opaque menu surface;
/// keyboard-navigable through the shared flyout state.
fn text_font_panel(
    pid: window::Id,
    current: text_annot::TextFont,
    selected: Option<usize>,
    tb: Tb,
) -> Element<'static, Msg> {
    let rows: Vec<Element<'static, Msg>> = [text_annot::TextFont::Hand, text_annot::TextFont::Clean]
        .into_iter()
        .enumerate()
        .map(|(i, f)| {
            let hot = selected == Some(i) || (selected.is_none() && f == current);
            let mut label = widget::text(f.label()).size(TEXT_MENU_LABEL_SIZE).font(font_preview_face(f));
            if hot {
                label = label.class(cosmic::theme::Text::Custom(|t| {
                    cosmic::iced::widget::text::Style {
                        color: Some(crate::app::theme::accent(t)),
                        ..Default::default()
                    }
                }));
            }
            // Same cap-band centring as the closed chip (DRAGON-363): the rows preview two faces
            // side by side, so line-box centring would stagger them against each other.
            let label = cap_centered_label(f, TEXT_MENU_LABEL_SIZE, label);
            crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::custom(label)
                    .width(Length::Fill)
                    .class(cosmic::theme::Button::Text)
                    .on_press(Msg::Preview(pid, PreviewMsg::SetTextFont(f))),
            )
        })
        .collect();
    tb.menu_container(widget::column(rows).spacing(2.0), TEXT_DROPDOWN_MENU_W)
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

    // ── DRAGON-375: a toast must never change the composed tree's SHAPE ────────────────

    /// A distinctive stand-in for the MEDIA element: a stateful widget (a button carries its
    /// own `tree::State`), so it can be located by tag in the composed tree and nothing else
    /// in these fixtures shares it.
    fn media_marker() -> Element<'static, Msg> {
        widget::button::custom(widget::text("media")).into()
    }

    /// Where the media element sits in a composed tree: the index path to it, its own child
    /// count, and its PARENT's child count — i.e. everything iced's reconciler uses to decide
    /// whether the retained state at that position still belongs to the same widget.
    #[derive(Debug, PartialEq, Eq)]
    struct Site {
        path: Vec<usize>,
        children: usize,
        siblings: usize,
    }

    /// Locate the first node tagged `tag`. `None` when the tag isn't in the tree at all.
    fn marker_site(
        el: &Element<'_, Msg>,
        tag: cosmic::iced::core::widget::tree::Tag,
    ) -> Option<Site> {
        fn walk(
            node: &cosmic::iced::core::widget::Tree,
            tag: cosmic::iced::core::widget::tree::Tag,
            path: &mut Vec<usize>,
            siblings: usize,
        ) -> Option<Site> {
            if node.tag == tag {
                return Some(Site { path: path.clone(), children: node.children.len(), siblings });
            }
            for (i, child) in node.children.iter().enumerate() {
                path.push(i);
                if let Some(found) = walk(child, tag, path, node.children.len()) {
                    return Some(found);
                }
                path.pop();
            }
            None
        }
        let tree = cosmic::iced::core::widget::Tree::new(el.as_widget());
        walk(&tree, tag, &mut Vec::new(), 1)
    }

    /// Compose one preview with and without a toast and return where the media landed in each.
    fn media_sites(windowed: bool) -> (Site, Site) {
        let tag = cosmic::iced::core::widget::Tree::new(media_marker().as_widget()).tag;
        let compose = |toasts: Option<Element<'static, Msg>>| {
            let header = (!windowed).then(|| widget::Space::new().into());
            compose_preview(
                windowed,
                600.0,
                header,
                widget::Space::new().into(),
                media_marker(),
                None,
                widget::Space::new().into(),
                None,
                toasts,
            )
        };
        let with = marker_site(&compose(Some(widget::text("toast").into())), tag)
            .expect("the media element is in the composed tree");
        let without =
            marker_site(&compose(None), tag).expect("the media element is in the composed tree");
        (with, without)
    }

    /// The regression this exists for: iced reconciles state by tree shape, so if the media
    /// element moves (or its parent changes tag) when a toast comes or goes, every widget under
    /// it — including the `AnnotationCanvas` holding the IN-FLIGHT gesture — is rebuilt from
    /// scratch and the stroke the user is drawing is thrown away. The opening toast expires
    /// 750 ms after the first interaction, i.e. inside the first stroke.
    #[test]
    fn a_toast_coming_or_going_never_moves_the_media_element() {
        for windowed in [false, true] {
            let (with, without) = media_sites(windowed);
            assert_eq!(
                with, without,
                "windowed={windowed}: the media element must sit at the same path (and keep the \
                 same children) whether or not a toast is showing"
            );
        }
    }

    /// …and the placeholder must actually SURVIVE into the stack. `Stack::push` silently drops
    /// any child whose size hint `is_void` (a `Fixed(0.0)` axis), which would restore the very
    /// child-count change this fix removes — so assert the toast SLOT is occupied (two children
    /// beside the media) with no toast, rather than trusting the placeholder's sizing.
    #[test]
    fn the_toast_slot_is_occupied_even_with_no_toast() {
        for windowed in [false, true] {
            let (with, without) = media_sites(windowed);
            assert_eq!(with.siblings, 2, "windowed={windowed}: media + toast");
            assert_eq!(
                without.siblings, 2,
                "windowed={windowed}: the empty toast slot must not be dropped from the stack"
            );
            // The media is child 0 of the stack in both arms; the toast/placeholder is child 1.
            assert_eq!(
                with.path.last(),
                Some(&0),
                "windowed={windowed}: the media stays the stack's BASE layer"
            );
        }
    }

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
                Tool::Text,
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
        // pointer(1), callouts(2), redact(2), SHAPES(3), spotlight+dim(2, DRAGON-369 split the
        // old five-member "Emphasis" group in two so a chip is exactly a slot), TEXT(1,
        // DRAGON-354), pencil+eraser(2).
        assert_eq!(shape, vec![1, 2, 2, 3, 2, 1, 2]);
        // A ONE-member group exists and is rendered through the very same container as the
        // others — there is no single-member branch anywhere in the builder.
        assert!(shape.contains(&1));
        assert!(matches!(ANNOT_TRAY[4].items[1], TrayItem::DimSlider));
        // The size dropdown is injected right after the TEXT group, which must be the group
        // whose sole tool is Text.
        assert!(matches!(
            ANNOT_TRAY[TEXT_TRAY_GROUP].items,
            [TrayItem::Tool(_, Tool::Text, _, _)]
        ));
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

    /// Every tray entry advertises a hotkey action that lives in the PREVIEW context, and every
    /// tool is REACHABLE from the keyboard out of the box — directly, or through its group's
    /// cycle key (DRAGON-369). The tooltip shows whichever one actually reaches it.
    #[test]
    fn every_tray_tool_binds_a_preview_action() {
        use crate::shortcuts::{Context, Keymap};
        let km = Keymap::defaults();
        for group in ANNOT_TRAY {
            for entry in group.items {
                let TrayItem::Tool(_, _, name, action) = entry else { continue };
                assert_eq!(action.context(), Context::Preview, "{name}");
                let tip = tip_action(*action, group.cycle, &km);
                assert_eq!(tip.context(), Context::Preview, "{name} tip");
                assert!(km.get(tip).is_some(), "{name} is unreachable from the keyboard");
            }
        }
    }

    // ── one-key tool SLOTS (DRAGON-369) ──────────────────────────────────────────────

    /// The slots are read off the tray itself: membership IS the group's tools and the cycle
    /// order IS their declared left-to-right order — learn the tray, you have learned the cycle.
    #[test]
    fn the_slots_are_the_declared_tray_groups() {
        use crate::shortcuts::Action;
        assert_eq!(
            slot_tools(Action::PreviewAnnotRedactCycle),
            vec![Tool::Pixelate, Tool::Blur]
        );
        assert_eq!(
            slot_tools(Action::PreviewAnnotShapeCycle),
            vec![Tool::Highlight, Tool::Rect, Tool::BoxHighlight]
        );
        // Every slot's tools appear in the SAME order the tray renders them.
        for slot in [Action::PreviewAnnotRedactCycle, Action::PreviewAnnotShapeCycle] {
            let members = slot_tools(slot);
            let idx: Vec<usize> = members
                .iter()
                .map(|m| tray_tools().iter().position(|t| t == m).expect("in the tray"))
                .collect();
            assert!(idx.windows(2).all(|w| w[0] < w[1]), "{slot:?} cycles against tray order");
            // …and each member maps back to its own slot.
            for m in &members {
                assert_eq!(slot_for_tool(*m), Some(slot));
            }
        }
        // An action that names no slot has no members — a stray cycle press is a no-op.
        assert!(slot_tools(Action::PreviewSave).is_empty());
        // Solo tools belong to no slot, so their key can never be stolen by a cycle.
        for solo in [Tool::Pointer, Tool::Arrow, Tool::Badge, Tool::Spotlight, Tool::Text, Tool::Pen, Tool::Eraser] {
            assert_eq!(slot_for_tool(solo), None, "{solo:?}");
        }
    }

    /// Arm-then-advance, the whole behaviour: a cold press arms the first member, a repeat
    /// advances and wraps, leaving the slot and coming back re-arms the member you left on
    /// (the cursor) rather than restarting at member one.
    #[test]
    fn a_slot_key_arms_then_advances_and_wraps() {
        let shape = [Tool::Highlight, Tool::Rect, Tool::BoxHighlight];
        // Cold: nothing armed, no cursor → member 1.
        assert_eq!(next_slot_tool(&shape, None, None), Some(Tool::Highlight));
        // Armed → advance, and the last member wraps to the first.
        assert_eq!(next_slot_tool(&shape, Some(Tool::Highlight), Some(Tool::Highlight)), Some(Tool::Rect));
        assert_eq!(next_slot_tool(&shape, Some(Tool::Rect), Some(Tool::Rect)), Some(Tool::BoxHighlight));
        assert_eq!(
            next_slot_tool(&shape, Some(Tool::BoxHighlight), Some(Tool::BoxHighlight)),
            Some(Tool::Highlight)
        );
        // Away on another tool → re-arm the CURSOR (however it was set — a tray click counts),
        // so repeat work costs one press.
        assert_eq!(next_slot_tool(&shape, Some(Tool::Arrow), Some(Tool::Rect)), Some(Tool::Rect));
        // A cursor from another slot (or a stale one) never wins over the slot's own order.
        assert_eq!(next_slot_tool(&shape, None, Some(Tool::Blur)), Some(Tool::Highlight));
        // An empty slot arms nothing at all.
        assert_eq!(next_slot_tool(&[], Some(Tool::Rect), None), None);
    }

    /// The tooltip resolves the key that ACTUALLY reaches the tool, in the same order a press
    /// does: a per-tool bind wins, and a slot member (unbound by default) shows its cycle key.
    #[test]
    fn a_slot_members_tip_falls_back_to_the_cycle_key() {
        use crate::shortcuts::{Action, Keymap, Shortcut, ShortcutKey};
        let mut km = Keymap::defaults();
        let slot = Some(Action::PreviewAnnotShapeCycle);
        assert_eq!(
            tip_action(Action::PreviewAnnotBoxHighlight, slot, &km),
            Action::PreviewAnnotShapeCycle
        );
        // The escape hatch: bind the tool directly and the tip follows the direct key.
        km.set(
            Action::PreviewAnnotBoxHighlight,
            Shortcut { ctrl: false, alt: false, shift: false, logo: false, key: ShortcutKey::Char("g".into()) },
        );
        assert_eq!(
            tip_action(Action::PreviewAnnotBoxHighlight, slot, &km),
            Action::PreviewAnnotBoxHighlight
        );
        // A tool in a group with no slot always shows its own action.
        assert_eq!(tip_action(Action::PreviewAnnotArrow, None, &km), Action::PreviewAnnotArrow);
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

    // ── Cap-band label centring (DRAGON-363) ────────────────────────────────────────────────
    //
    // The rendered result is headless-unverifiable, so the rule is pinned as pure arithmetic over
    // the EMBEDDED faces' real metrics, against a MODEL of cosmic-text's own placement.

    /// Metrics of the face registered under `family`, read from the same embedded bytes the UI
    /// font system gets.
    fn embedded_metrics(family: &str) -> FaceVMetrics {
        let (_, bytes) = text_annot::UI_FONT_FACES
            .iter()
            .find(|(name, _)| *name == family)
            .unwrap_or_else(|| panic!("{family} is registered as a UI font face"));
        let face = ttf_parser::Face::parse(bytes, 0).expect("embedded face parses");
        face_v_metrics(&face)
    }

    /// cosmic-text's OWN placement rule, as the test's model: inside a line box of height
    /// `line_h` the baseline lands at `line_h/2 + (ascent - descent)/2` (`cosmic_text::Buffer`'s
    /// `centering_offset + max_ascent`). Returns the baseline measured from the top of the WHOLE
    /// padded element.
    fn modelled_baseline(m: FaceVMetrics, size: f32, b: CapCenteredBox) -> f32 {
        b.pad_top + b.line_h / 2.0 + (m.ascent - m.descent) * size / (2.0 * m.upem)
    }

    /// The gap between the element's vertical centre and the centre of the face's CAP BAND
    /// (baseline → cap height) once the correction is applied. Zero = the rule is satisfied.
    fn cap_band_offset(m: FaceVMetrics, size: f32, b: CapCenteredBox) -> f32 {
        let outer_h = b.pad_top + b.line_h + b.pad_bottom;
        let baseline = modelled_baseline(m, size, b);
        let cap_center = baseline - m.cap.unwrap() * size / (2.0 * m.upem);
        cap_center - outer_h / 2.0
    }

    /// The reason "Clean" (and every UI-font label in the toolbar, the interface font being Inter)
    /// already reads centred: Inter's metrics satisfy `ascent - |descent| == capHeight` exactly, so
    /// the line-box rule IS cap-band centring for it. The correction must therefore be exactly
    /// zero — and a zero correction must reproduce the historical layout byte for byte.
    #[test]
    fn inter_is_already_cap_centred_so_clean_never_moves() {
        let m = embedded_metrics("Inter");
        assert_eq!(m.ascent - m.descent, m.cap.unwrap(), "Inter's alignment box IS its cap band");
        assert_eq!(cap_center_shift_em(m), 0.0);
        for size in [12.0_f32, 13.0, 24.0] {
            let b = cap_centered_box(size, cap_center_shift_em(m));
            assert_eq!((b.pad_top, b.pad_bottom), (0.0, 0.0), "no padding is introduced");
            let natural = cosmic::iced::advanced::text::LineHeight::default()
                .to_absolute(cosmic::iced::Pixels(size))
                .0;
            assert_eq!(b.line_h, natural, "the line box is the untouched default at {size}px");
        }
    }

    /// Excalifont is the face the report is about: its alignment box is SHORTER than its cap band
    /// (886 - 374 = 512 units against a 600-unit cap height), so line-box centring rides its
    /// capitals high by half that difference. The correction is that half, pushing DOWN.
    #[test]
    fn excalifont_rides_high_by_exactly_half_its_metric_gap() {
        let m = embedded_metrics("Excalifont");
        let gap = (m.cap.unwrap() - (m.ascent - m.descent)) / m.upem;
        assert!(gap > 0.0, "the cap band overshoots the alignment box");
        let shift = cap_center_shift_em(m);
        assert!((shift - gap / 2.0).abs() < 1e-6, "the shift is half the gap");
        assert!((shift - 0.044).abs() < 5e-4, "0.044em — ~0.53px at the 12px chip, got {shift}");
        // And it moves the label DOWN (the report: the label sits too high).
        let b = cap_centered_box(12.0, shift);
        assert!(b.pad_top > 0.0 && b.pad_bottom == 0.0);
    }

    /// THE rule, applied to every embedded face at both label sizes the picker uses (the closed
    /// chip's 12px and the menu rows' 13px): the cap band ends up centred, and the element keeps
    /// the exact height an uncorrected label has — so neither the chip nor a menu row changes
    /// size. A face added to `UI_FONT_FACES` is covered here with no new code.
    #[test]
    fn every_embedded_face_centres_its_cap_band_without_changing_height() {
        for (family, bytes) in text_annot::UI_FONT_FACES {
            let face = ttf_parser::Face::parse(bytes, 0).expect("embedded face parses");
            let m = face_v_metrics(&face);
            assert!(m.cap.is_some(), "{family} declares a cap height");
            for size in [TEXT_CHIP_LABEL_SIZE, TEXT_MENU_LABEL_SIZE, 24.0] {
                let b = cap_centered_box(size, cap_center_shift_em(m));
                let natural = cosmic::iced::advanced::text::LineHeight::default()
                    .to_absolute(cosmic::iced::Pixels(size))
                    .0;
                assert!(
                    (b.pad_top + b.line_h + b.pad_bottom - natural).abs() < 1e-4,
                    "{family} at {size}px keeps the natural {natural}px element height"
                );
                assert!(b.line_h > 0.0 && b.pad_top >= 0.0 && b.pad_bottom >= 0.0);
                assert!(
                    cap_band_offset(m, size, b).abs() < 1e-4,
                    "{family} at {size}px centres its cap band, off by {}",
                    cap_band_offset(m, size, b)
                );
            }
        }
    }

    /// The picker's two faces land at the SAME height as each other — which is all the user can
    /// see. Measured as the distance from the row centre to each face's cap-band centre.
    #[test]
    fn the_two_picker_faces_agree_after_the_correction() {
        for size in [TEXT_CHIP_LABEL_SIZE, TEXT_MENU_LABEL_SIZE] {
            let mut offsets = Vec::new();
            for font in [text_annot::TextFont::Hand, text_annot::TextFont::Clean] {
                let m = embedded_metrics(font.family());
                let b = cap_centered_box(size, face_shift_em(font));
                offsets.push(cap_band_offset(m, size, b));
            }
            assert!(
                (offsets[0] - offsets[1]).abs() < 1e-4,
                "the faces disagree by {} at {size}px",
                offsets[0] - offsets[1]
            );
        }
        // The un-corrected placement is what the report saw: the faces disagreed by a real amount.
        let (hand, clean) = (embedded_metrics("Excalifont"), embedded_metrics("Inter"));
        let uncorrected = |m: FaceVMetrics| {
            let b = cap_centered_box(TEXT_CHIP_LABEL_SIZE, 0.0);
            cap_band_offset(m, TEXT_CHIP_LABEL_SIZE, b)
        };
        assert!(
            (uncorrected(hand) - uncorrected(clean)).abs() > 0.5,
            "the historical placement staggered the faces by more than half a pixel"
        );
    }

    /// The rule is arithmetic on whatever metrics a face declares — it never needs to know WHICH
    /// face it is. A synthetic face whose alignment box OVERSHOOTS its cap band (the common case
    /// for `hhea`-metric faces) is pushed the other way, and an absurd face is clamped so the line
    /// box can never vanish.
    #[test]
    fn the_rule_generalises_to_any_face() {
        let tall = FaceVMetrics { upem: 1000.0, ascent: 1000.0, descent: 300.0, cap: Some(700.0) };
        let shift = cap_center_shift_em(tall);
        assert_eq!(shift, 0.0, "700 == 1000 - 300: already cap-centred");
        let leggy = FaceVMetrics { upem: 1000.0, ascent: 1100.0, descent: 400.0, cap: Some(650.0) };
        let b = cap_centered_box(12.0, cap_center_shift_em(leggy));
        assert!(cap_center_shift_em(leggy) < 0.0, "this face renders LOW, so it moves up");
        assert!(b.pad_bottom > 0.0 && b.pad_top == 0.0);
        assert!(cap_band_offset(leggy, 12.0, b).abs() < 1e-4);
        // Absurd metrics: clamped, and the line box survives.
        let absurd = FaceVMetrics { upem: 1000.0, ascent: 5000.0, descent: 0.0, cap: Some(500.0) };
        let b = cap_centered_box(12.0, cap_center_shift_em(absurd));
        assert!(b.line_h > 0.0 && b.pad_top >= 0.0 && b.pad_bottom >= 0.0);
        // A face that declares nothing usable keeps the historical placement.
        let blind = FaceVMetrics { upem: 1000.0, ascent: 900.0, descent: 200.0, cap: None };
        assert_eq!(cap_center_shift_em(blind), 0.0);
        assert_eq!(cap_centered_box(12.0, 0.0).pad_top, 0.0);
    }

    /// An unregistered family (or one whose bytes never parsed) is a no-op, not a panic — the
    /// label just keeps the historical placement.
    #[test]
    fn a_known_face_resolves_and_the_lookup_is_total() {
        assert_eq!(face_shift_em(text_annot::TextFont::Clean), 0.0);
        assert!(face_shift_em(text_annot::TextFont::Hand) > 0.0);
        for font in [text_annot::TextFont::Hand, text_annot::TextFont::Clean] {
            assert!(
                text_annot::UI_FONT_FACES.iter().any(|(f, _)| *f == font.family()),
                "{} must be registered for its preview label to render in its own face",
                font.family()
            );
        }
    }
}
