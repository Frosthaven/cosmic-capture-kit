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

/// The LABEL on a live crop session's accept button (DRAGON-410). Named because two places read
/// it: [`Tb::crop_items`], which draws it, and the test that proves the labelled pair still fits
/// the narrowest preview the editor will open at.
pub(super) const CROP_ACCEPT_LABEL: &str = "Apply Crop";

// ── The top bar's GROUP CAPTIONS (DRAGON-478) ────────────────────────────────────────────────
//
// Each cluster on the TOP edit bar gets a small, very subdued word under it saying what the
// family is for ("Canvas", "Shape", "Redact", …). The words live where the grouping lives: on
// [`TrayGroup::caption`] for the declared annotation tray, and in [`SHARE_GROUP_CAPTION`] for the
// one top-bar group that is not a tray row. Nothing else may spell one.
//
// The band is OPTIONAL (the "Show toolbar group labels" setting) and it is a LAYOUT feature, not
// only a cosmetic one: it makes the top bar taller, so [`caption_band_h`] is added into
// `PreviewSurface::chrome_h` — THE one chrome-height source the windowed open fit, the overlay
// column and the fit/zoom math all read. With the setting off the band is exactly zero and the
// editor's geometry is byte-identical to the pre-DRAGON-478 build.

/// The caption's font size in UNSCALED px (multiplied by [`Tb::scale`], then snapped to a whole
/// pixel). **An owner-tuned dial, settled on screen, not derived**: 10.0 rendered 8px and read
/// as a squished smear (the owner's screenshot), doubling to 20.0 rendered 16px and read too
/// large, and a third off (13.4, 11px rendered) is where the owner landed. Retune by eye, and
/// keep [`CAPTION_LINE_H`] moving proportionally with it.
pub(super) const CAPTION_TEXT_SIZE: f32 = 13.4;

/// The caption's LINE BOX in unscaled px, pinned absolutely rather than left to the font's own
/// line height. That is what makes the band's height arithmetic exact: what
/// [`caption_band_h`] reserves is what the text draws into, at every scale and on every
/// platform's font stack. Tracks [`CAPTION_TEXT_SIZE`] at the same proportion: the glyphs
/// centre inside this box, so the box clearing the text is what keeps the word from hugging
/// the cluster above it.
pub(super) const CAPTION_LINE_H: f32 = 16.0;

/// The unscaled gap between a cluster's bottom edge and its caption's line box — the label's
/// top padding, owner-tuned by eye (4px in the round that settled the size, then 6). Also
/// the label's BOTTOM padding when the captions are on, via [`top_bar_bottom_pad`]: one
/// dial moves both sides of the word together.
pub(super) const CAPTION_GAP: f32 = 6.0;

/// The toolbar bars' standard vertical padding (flat px) — the 8 in [`preview_bar`]'s
/// historical `[8, 12]` — named so the caption-symmetry seam below and
/// `PreviewSurface::chrome_h` read the SAME number the bar draws with.
pub(super) const BAR_V_PAD: f32 = 8.0;

/// The TOP bar's BOTTOM padding: [`BAR_V_PAD`] as always, except with the captions on,
/// where it becomes [`CAPTION_GAP`] at the same scale the gap above the word uses.
///
/// This is the fix for the owner's "padding doesn't do what I think" screenshot: the space
/// ABOVE a caption is the gap (~5px scaled), but BELOW it sat the bar's full 8px padding on
/// top of the line box's slack, so the word read top-heavy in a band of dead bar. Swapping
/// the bar's bottom padding for the scaled gap makes the band symmetric BY CONSTRUCTION —
/// the line box's own slack already sits evenly around the glyphs — and keeps it symmetric
/// through every future retune of the gap. The OVERLAY is deliberately out of scope: it has
/// no bar fill under its toolbar, so the space below its captions is column rhythm, not a
/// bar that can read as empty.
///
/// Pure and unit-tested; `PreviewSurface::chrome_h` reserves exactly this, so the view and
/// the open fit cannot drift.
pub(super) fn top_bar_bottom_pad(labels: bool, scale: f32) -> f32 {
    if labels { CAPTION_GAP * scale } else { BAR_V_PAD }
}

const _: () = assert!(
    CAPTION_LINE_H > CAPTION_TEXT_SIZE,
    "the caption's pinned line box must clear its glyph size, or the reserved band clips the label"
);

/// The caption under the Save / Copy / Share / Upload group ([`Tb::share_group`]) — the one
/// top-bar cluster that is not an [`ANNOT_TRAY`] row, so it carries its caption here rather than
/// as a declared field.
pub(super) const SHARE_GROUP_CAPTION: &str = "File Actions";

/// The EXTRA vertical space the top edit bar takes when the group captions are drawn: the gap
/// plus the caption's pinned line box, at the chrome scale. Zero when they are off, which is
/// what returns the whole editor to its pre-DRAGON-478 geometry.
///
/// Pure (no theme, no widgets, no platform) and unit-tested, because it is read from two
/// different places that must not drift: the view (`Tb::captioned`, which builds the column this
/// measures) and `PreviewSurface::chrome_h` (which reserves it).
pub(super) fn caption_band_h(labels: bool, scale: f32) -> f32 {
    if labels {
        scale * (CAPTION_GAP + CAPTION_LINE_H)
    } else {
        0.0
    }
}

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

/// A tooltip for a control whose key is **not** in the keymap — same `"Name  (KEY)"` shape as
/// [`action_tip`], with the key spelled literally.
///
/// Two families of caller, for the same underlying reason: there is nothing in the keymap to
/// read, because the key is not user-configurable.
///
/// * The crop session's ACCEPT (Enter) and REJECT (Esc). A live crop session is MODAL and takes
///   the keyboard whole in `keyboard.rs`'s `preview_modal_key`, matching `Named::Enter` /
///   `Named::Escape` directly and swallowing everything else, exactly as a live text edit does.
/// * The five DRAGON-617 BAKED actions (Save, Upload, Close, Undo, Redo), whose key comes from
///   [`crate::shortcuts::FixedEditorAction::label`]. Those callers pass a rendered label rather
///   than a literal, so the tooltip still resolves the chord from the SAME `Shortcut` the
///   matcher listens on and cannot drift from it. On a default install every one of these
///   tooltips reads exactly as it did when the actions were bindable.
///
/// Keep the FORM here so all of them still match the rest of the tray's.
///
/// (Verified rather than assumed, DRAGON-392: Enter really does accept and Esc really does
/// cancel; the crop branch runs BEFORE the keymap dispatch, so Esc cancels the SESSION and never
/// closes the editor; and a session can no longer coexist with a live text edit — entering one
/// settles it — so Enter has one meaning.)
pub(super) fn fixed_key_tip(name: &str, key: &str) -> String {
    format!("{name}  ({key})")
}

/// Toolbar-builder scale context — an explicit value replacing a former `thread_local`.
/// ONE [`super::surface::CHROME_SCALE`] for both surface kinds (it was per-surface once; see
/// [`PreviewSurface::btn_scale`]). Built once at the top of
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
    /// Draw the group CAPTIONS under the top bar's clusters (DRAGON-478; the "Show toolbar
    /// group labels" setting). Threaded on `Tb` because that is how every other view-wide
    /// chrome fact already reaches the builders — and it must agree with the SAME flag
    /// `PreviewSurface::chrome_h` is given, or the bar would draw a band nothing reserved.
    pub(super) labels: bool,
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

    /// The scaled toolbar-button glyph box, SNAPPED to a whole logical pixel.
    ///
    /// The rounding is the DRAGON-399 fix and is load-bearing, not tidiness: iced rasterizes an
    /// SVG at `ceil(display_scale × bounds)` and draws it into the bounds given, so a fractional
    /// box (`ICON_BOX 22 × CHROME_SCALE 0.91 = 20.02`) rasters at 21px, is drawn into 20.02, and
    /// gets bilinearly RESAMPLED — which is what put the visible step in the accept checkmark's
    /// arm and defeated six successive asset fixes. See [`crate::widgets::icons::box_px`] for the
    /// measurement and for why this must never become scale-value-specific.
    pub(super) fn icon_box(self) -> f32 {
        crate::widgets::icons::box_px(ICON_BOX * self.scale)
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

    /// Put `caption` under a built top-bar cluster (DRAGON-478), centred on it. Returns the
    /// cluster UNCHANGED when the labels setting is off, so that path is byte-identical to the
    /// pre-DRAGON-478 bar.
    ///
    /// An EMPTY `caption` still reserves the band and draws no word. That is how a top-bar group
    /// with no caption of its own (the macOS fullscreen Close) stays on the same line as its
    /// captioned neighbours: the row centres its items vertically, so an item shorter by the band
    /// would visibly sink.
    ///
    /// TONE: [`quiet_on_bar`], not [`quiet_on_cluster`]. The caption sits on the BAR, OUTSIDE the
    /// cluster's chip, and the two tokens are measured against different surfaces
    /// (`primary.base` vs `background.component.base`) — the document-info block made the same
    /// call for the same reason. The fullscreen OVERLAY draws this bar with no `preview_bar`
    /// fill behind it at all, so `primary.base` is an approximation there; it is still the closer
    /// of the two, and the alternative would be a third token for one word.
    pub(super) fn captioned<'a>(
        self,
        cluster: Element<'a, Msg>,
        caption: &'static str,
    ) -> Element<'a, Msg> {
        if !self.labels {
            return cluster;
        }
        let label = widget::text(caption)
            // Snapped to a whole pixel like every other scaled chrome measure, and floored so a
            // future smaller scale can never render it as an unreadable smear.
            .size((CAPTION_TEXT_SIZE * self.scale).round().max(8.0))
            .line_height(cosmic::iced::widget::text::LineHeight::Absolute(
                cosmic::iced::Pixels(CAPTION_LINE_H * self.scale),
            ))
            .class(cosmic::theme::Text::Custom(|t: &cosmic::Theme| {
                cosmic::iced::widget::text::Style {
                    color: Some(quiet_on_bar(t)),
                    ..Default::default()
                }
            }));
        widget::column(vec![cluster, label.into()])
            .spacing(CAPTION_GAP * self.scale)
            .align_x(Alignment::Center)
            .into()
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
        let icon = crate::widgets::icons::sized(name, self.icon_box());
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

    /// [`Self::tool_button_tinted`] that can be INERT (DRAGON-467) — the action-group twin of
    /// [`Self::history_button`]'s disabled arm, for a capability this build does not have
    /// (Share) or a feature that does not exist yet (Upload).
    ///
    /// Disabled means three things at once, and all three matter:
    ///
    /// * **no `on_press`**, so the message is never emitted. That is the real gate; the
    ///   colour is only how it is communicated.
    /// * **the [`disabled_label_tone`] glyph**, the cluster-border token shared with every
    ///   other disabled control here, so "off" reads the same across the editor.
    /// * **the tooltip stays**. `history_button` drops its tooltip when it greys out, which is
    ///   right for undo/redo (an empty stack needs no explanation). It is wrong here: a
    ///   permanently disabled Share or Upload is exactly the button a user WILL hover to ask
    ///   why, and the tooltip is the only place the answer can live.
    ///
    /// The `tint` argument is accepted and deliberately IGNORED while disabled, so a caller
    /// can pass the group's shared dirty tint without having to special-case these two.
    pub(super) fn tool_button_gated(
        self,
        name: &'static str,
        tip: String,
        msg: PreviewMsg,
        pos: widget::tooltip::Position,
        enabled: bool,
        tint: Option<fn(&cosmic::Theme) -> cosmic::iced::Color>,
    ) -> Element<'static, Msg> {
        if enabled {
            return self.tool_button_tinted(name, tip, msg, pos, tint);
        }
        let glyph = crate::widgets::icons::sized(name, self.icon_box()).class(
            cosmic::theme::Svg::custom(|theme| cosmic::widget::svg::Style {
                color: Some(disabled_label_tone(theme)),
            }),
        );
        let button = crate::widgets::arrow_cursor::arrow_cursor(
            widget::button::custom(
                widget::container(glyph).width(Length::Fill).align_x(Alignment::Center),
            )
            .class(cosmic::theme::Button::Icon)
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
                        c.background(false).component.base.into(),
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
    /// A one-member cluster gets exactly the same container as a five-member one (same border,
    /// padding and corner treatment) — that is deliberate, and the reason there is no
    /// single-member special case anywhere.
    ///
    /// EVERY cluster paints this one surface. There used to be a declared per-group `chrome`
    /// axis (`ClusterChrome::Bare`: the identical padded footprint with no fill and no border)
    /// for the lone Select button, which read as one more bordered box when it stood by itself;
    /// DRAGON-392 made Select part of a real three-peer group (Select / Hand / Crop), leaving no
    /// bare group anywhere. If a group ever needs the footprint-only look again, the axis comes
    /// back HERE and as a field on [`TrayGroup`] — never as a special case in view code.
    pub(super) fn tool_cluster<'a>(
        self,
        items: Vec<Element<'a, Msg>>,
    ) -> Element<'a, Msg> {
        let glass = self.glass;
        widget::container(widget::row(items).spacing(2.0).align_y(Alignment::Center))
            .padding(self.grp_pad())
            .class(cosmic::theme::Container::Custom(Box::new(move |theme| {
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(crate::app::theme::frost_color(
                        theme.cosmic().background(false).component.base.into(),
                        glass,
                    ))),
                    border: Border {
                        radius: crate::app::theme::rounding(theme).xl.into(),
                        width: 1.0,
                        // The DROPDOWN's outline token — a group outline is chrome, never
                        // a state, so it never tracks selection. Shared with the disabled
                        // tone through [`cluster_border`].
                        color: cluster_border(theme),
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
                        c.background(false).component.base.into(),
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
    /// width so it never resizes as its rows change, and it is the CARD's outer width: what a
    /// row inside actually gets is that minus [`MENU_CARD_PAD`] on each side.
    pub(super) fn menu_container<'a>(
        self,
        content: impl Into<Element<'a, Msg>>,
        width: f32,
    ) -> Element<'a, Msg> {
        widget::container(content)
            .width(Length::Fixed(width))
            .padding(MENU_CARD_PAD)
            .class(cosmic::theme::Container::custom(move |theme| {
                let c = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    // Opaque component base — the standard menu/popover surface, NO frost.
                    background: Some(Background::Color(c.background(false).component.base.into())),
                    border: Border {
                        radius: crate::app::theme::rounding(theme).s.into(),
                        width: 1.0,
                        color: c.background(false).divider.into(),
                    },
                    ..Default::default()
                }
            }))
            .into()
    }

    /// THE action group (DRAGON-467): Save / Copy / Share / Upload, then Delete, in one
    /// capsule.
    ///
    /// The four leading buttons and their lucide glyphs are the ticket's own list:
    ///
    /// * **Save** (`save`) opens the destination picker pre-filled with the path a plain
    ///   overwrite-save would have written. The separate "Save As" button is GONE — with the
    ///   prefill there is nothing left for it to do differently.
    /// * **Copy** (`copy`) puts the current state on the clipboard and nothing else.
    /// * **Share** (`share`, the box-with-an-arrow-out; DRAGON-478 moved it off `share-2`, the
    ///   three-node graph that reads as social sharing) hands the file to the system share
    ///   sheet, and appears only on
    ///   systems that HAVE one (`platform::services::share_available`), which the ticket
    ///   scoped it to. WINDOWS has one since DRAGON-474, so the button is there; macOS and
    ///   Linux do not, so it is absent there (see `share::share_sheet`'s module doc for what
    ///   each platform would call). It differs from Upload deliberately: Upload is a feature
    ///   that does not exist ANYWHERE yet, so showing it disabled advertises what is coming,
    ///   while Share is a capability of the MACHINE, and a permanently dead control on a
    ///   desktop that will never have one is just noise.
    /// * **Upload** (`cloud-upload` since DRAGON-478 — the cloud is what says "account", where
    ///   plain `upload` was one arrow away from Save) sends the capture to a connected cloud
    ///   account. DRAGON-467 shipped it permanently disabled because the accounts feature did
    ///   not exist; DRAGON-482 built it, so the button is now live whenever at least ONE
    ///   account is connected and still disabled (with a tooltip saying why) when none is. It
    ///   opens a flyout rather than acting: an upload needs a destination, and unlike every
    ///   other button in this group it cannot be taken back.
    ///
    /// **Delete is GONE** (user decision after testing DRAGON-467: "remove delete from the
    /// preview editor, not needed anymore"). It made sense when every capture was written to
    /// the user's folder the instant it was taken, so the editor needed a way to take it back.
    /// It does not now: with "Automatically save originals" off, an unwanted capture is never
    /// in the user's folder at all, and closing the editor is the whole of "no thanks". The
    /// trash button, its Ctrl+Shift+X action, the ask-card's Delete option and the file-unlink
    /// plumbing behind them all went together — see `PreviewState`'s note where `written` and
    /// `delete_paths` used to live.
    ///
    /// **The dirty tint**: while the document has unsaved edits, the ENABLED committing
    /// actions wear the accent's COMPANION colour (its complement — see
    /// [`super::annotate::companion`]) — each is the thing that would commit or carry those
    /// edits, so "there is something here you haven't dealt with" belongs on exactly those
    /// glyphs. Delete never takes the tint: it destroys rather than commits, and inviting the
    /// eye to it while work is unsaved would be a trap. A DISABLED button never takes it
    /// either: it already has a tone that says "not now", and two states on one glyph is one
    /// too many.
    ///
    /// `auto_share` is the persisted "also copy a share link" preference, threaded in because
    /// the Upload flyout binds a checkbox to it and the toolbar has no other reason to know
    /// the App's settings. Everything else the flyout draws (the accounts, the highlighted
    /// row) is on `preview` already.
    pub(super) fn share_group<'a>(
        self,
        preview: &'a PreviewState,
        dirty: bool,
        km: &crate::shortcuts::Keymap,
        auto_share: bool,
    ) -> Element<'a, Msg> {
        use crate::shortcuts::Action;
        // Top toolbar → tooltips drop below.
        let pos = widget::tooltip::Position::Bottom;
        // One shared tint for the committing actions, so they can never disagree about
        // whether the document is dirty. The accent's COMPANION (its complement), not the
        // WARNING amber — an unsaved document is a normal working state, not an alarm.
        let tint: Option<fn(&cosmic::Theme) -> cosmic::iced::Color> =
            dirty.then_some(super::annotate::companion as fn(&cosmic::Theme) -> cosmic::iced::Color);
        let can_share = crate::platform::services::share_available();
        // DRAGON-482: the document's own accounts SNAPSHOT decides whether Upload is live.
        // Never `accounts::list()` from here: this runs on every frame, and that reads and
        // parses a file (see `EditState::cloud_accounts`).
        let accounts = &preview.edit.cloud_accounts;
        // DRAGON-617 baked Upload, so the key is fixed and always present.
        let upload_key = crate::shortcuts::FixedEditorAction::Upload.label();
        // DRAGON-514: one upload at a time. The predicate is shared with the handler the
        // keybind arrives through, so the button and Ctrl+U cannot disagree about whether an
        // upload may start; here it only decides how the button DRAWS.
        let busy = super::edit::upload_in_flight(&preview.edit.uploads);
        let upload_btn = self.tool_button_gated(
            "upload-symbolic",
            upload_tip(!accounts.is_empty(), busy, upload_key),
            PreviewMsg::UploadFlyoutToggle,
            pos,
            !accounts.is_empty() && !busy,
            tint,
        );
        // The flyout hangs off the button itself, so it is anchored to the control that
        // opened it in both surface kinds. `Down` because this is the TOP bar (the covermark
        // picker's `Up` is for the bottom one), and it needs no hand-guessed panel height.
        let upload_btn: Element<'a, Msg> =
            if preview.edit.flyout_kind() == Some(super::edit::FlyoutKind::Upload) {
                flyout(
                    upload_btn,
                    upload_panel(
                        accounts,
                        preview.edit.flyout_selected(),
                        preview.edit.upload_account_menu_open,
                        auto_share,
                        preview.edit.upload_visibility_menu_open,
                        self,
                    ),
                    FlyoutDir::Down,
                    Msg::Preview(self.pid, PreviewMsg::UploadFlyoutClose),
                )
            } else {
                upload_btn
            };
        let mut buttons: Vec<Element<'a, Msg>> = vec![
            self.tool_button_tinted(
                "document-save-symbolic",
                fixed_key_tip("Save", crate::shortcuts::FixedEditorAction::Save.label()),
                PreviewMsg::Save,
                pos,
                tint,
            ),
            self.tool_button_tinted(
                "document-copy-symbolic",
                action_tip("Copy to clipboard", Action::PreviewCopy, km),
                PreviewMsg::Copy,
                pos,
                tint,
            ),
            upload_btn,
        ];
        if can_share {
            // Between Copy and Upload, so the group reads share-with-this-machine then
            // share-with-an-account. Inserted rather than pushed for exactly that ordering.
            buttons.insert(
                2,
                self.tool_button_gated(
                    "share-symbolic",
                    "Share".to_string(),
                    PreviewMsg::Share,
                    pos,
                    true,
                    tint,
                ),
            );
        }
        // DRAGON-357: the same bordered cluster the top-left tray groups wear.
        self.tool_cluster(buttons)
    }

    /// **THE document-info block** (DRAGON-392): the media's RESOLUTION over the saved file's
    /// SIZE, two quiet lines of BARE TEXT on the bottom bar — no group fill, no border (the owner
    /// removed the chip: it is information, not a control, and a chip made it read as one). Built
    /// ONCE for both media kinds, and both place it immediately LEFT of the scale/zoom group (a
    /// video has no zoom group, so there it is simply the right-hand item).
    ///
    /// `res` is the DISPLAY frame (`PreviewState::display_frame`), so a CROPPED still reports the
    /// framing that would be exported, not the original grab. `None` for either half simply drops
    /// that line (the size is unknown until the file exists, the resolution until the decode /
    /// probe lands); with neither there is nothing to show at all.
    ///
    /// Sizing/typography, all deliberate:
    /// * both lines wear [`TEXT_MENU_LABEL_SIZE`] — the SAME size as the text tool's px list
    ///   (the owner's reference point), read from that constant rather than a copied literal;
    /// * both are quiet via [`quiet_on_bar`], measured against the BAR (`primary.base`) now that
    ///   there is no chip between the text and it. Plain `theme::subdued` is still not the right
    ///   token here — it is blended toward `background.base`, which the bar is not;
    /// * horizontal padding is [`TOOLBAR_ROW_GAP`], the row's own inter-item gap, rather than the
    ///   cluster's inner inset it used while it had a border: bare text needs a gap of its own to
    ///   clear its neighbours' borders (see that constant);
    /// * the block is PINNED to the standard button-box height, so it can never make the bar
    ///   taller than the groups beside it — the toolbar height is reserved arithmetically
    ///   (`PreviewSurface::chrome_h`) and a bar that outgrew its reserve would push the media out
    ///   of its fitted box. (2 × 13 × 1.15 ≈ 30px inside the 31px box; the digits and "×" carry
    ///   no descenders, so nothing is clipped.)
    pub(super) fn info_chip(
        self,
        res: Option<(u32, u32)>,
        size: Option<u64>,
    ) -> Option<Element<'static, Msg>> {
        let quiet = |s: String| {
            widget::text(s)
                .size(TEXT_MENU_LABEL_SIZE)
                .line_height(cosmic::iced::widget::text::LineHeight::Relative(1.15))
                .class(cosmic::theme::Text::Custom(|t: &cosmic::Theme| {
                    cosmic::iced::widget::text::Style {
                        color: Some(quiet_on_bar(t)),
                        ..Default::default()
                    }
                }))
        };
        let mut lines: Vec<Element<'static, Msg>> = Vec::new();
        if let Some((w, h)) = res.filter(|(w, h)| *w > 0 && *h > 0) {
            lines.push(quiet(format!("{w} × {h}")).into());
        }
        if let Some(bytes) = size {
            lines.push(quiet(friendly_size(bytes)).into());
        }
        if lines.is_empty() {
            return None;
        }
        let block = widget::container(
            widget::column(lines).spacing(0.0).align_x(Alignment::Start),
        )
        .height(Length::Fixed(self.icon_box() + 2.0 * self.btn_pad()))
        .padding([0.0, TOOLBAR_ROW_GAP])
        .align_y(Alignment::Center);
        Some(block.into())
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
        .spacing(TOOLBAR_ROW_GAP)
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
    /// (DRAGON-617 baked Close, so this no longer takes the keymap: its key is fixed.)
    pub(super) fn cancel_group(self) -> Element<'static, Msg> {
        // Lives on the top edit toolbar → tooltip drops below.
        self.tool_group(vec![self.tool_button(
            "window-close-symbolic",
            fixed_key_tip("Close", crate::shortcuts::FixedEditorAction::Close.label()),
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
/// `toasts` (DRAGON-353) is stacked over the media VIEWPORT in both arms — the whole region
/// BETWEEN the toolbars — and never over the column, which is what keeps a toast clear of the
/// toolbars, the annotation tray and the video transport strip at any window size. It used to
/// anchor to the media ELEMENT (its fitted pixels), which held that same guarantee but made the
/// anchor as small as the picture: a small capture put the toast near the window's CENTRE and
/// then flitted it right as the media settled into its fitted box (DRAGON-393). Anchoring to
/// the REGION is the fix — same clearance, a position that no longer depends on the media's
/// size. The stack itself is UNCONDITIONAL (DRAGON-375): a toast coming or going must not
/// change the tree SHAPE under this position, or iced discards the media subtree's state
/// mid-gesture — see below.
#[allow(clippy::too_many_arguments)]
pub(super) fn compose_preview<'a>(
    windowed: bool,
    labels: bool,
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
    //
    // The stack's BASE layer is the media REGION, not the bare media element (DRAGON-393): the
    // fill/centre container that used to sit OUTSIDE the stack is now its first child
    // ([`toast_region`]), so the stack — which takes its size from that first child alone —
    // spans the whole space between the toolbars. The media still centres inside it exactly as
    // before (the same container, only moved), and the toast layer (`Fill`×`Fill`, top-right
    // aligned) now measures against the region instead of the picture. WINDOWED the region fills
    // both axes; in the OVERLAY it is the hugged column's width by the media row's own height —
    // the same box the toolbars span.
    let region = if windowed {
        toast_region(image, Length::Fill, Length::Fill)
    } else {
        toast_region(image, Length::Fill, Length::Shrink)
    };
    let image: Element<'a, Msg> = cosmic::iced::widget::stack(vec![
        region,
        toasts.unwrap_or_else(|| widget::Space::new().into()),
    ])
    .into();
    if windowed {
        // The windowed preview's titlebar owns those controls; nothing to slot in here.
        drop(overlay_header);
        // The TOP bar's bottom padding is the caption-symmetry seam (`top_bar_bottom_pad`):
        // both surfaces share the one chrome scale, which is what lets this arm read it
        // without a `Tb` in hand.
        let mut col: Vec<Element<'a, Msg>> = vec![
            preview_bar(
                edit_toolbar,
                false,
                glass,
                top_bar_bottom_pad(labels, super::surface::CHROME_SCALE),
            ),
            image,
        ];
        // The video transport strip sits between the canvas and the action bar.
        if let Some(t) = transport {
            col.push(transport_bar(t, true, glass));
        }
        // The bottom bar sits at the window's bottom edge, so round its bottom corners
        // to match the CSD window's rounded corners (no square nubs past the rounding).
        // No caption band below it, so it keeps the standard symmetric padding.
        col.push(preview_bar(action_toolbar, true, glass, BAR_V_PAD));
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

/// **THE toast anchor** (DRAGON-393): wrap `content` (the media, or the loading spinner) in the
/// REGION a toast is placed against, centring it inside. The returned element takes `width` /
/// `height` from its ARGUMENTS, never from what it wraps — that is the whole point. The toast
/// layer is `Fill`×`Fill` top-right aligned, and a `stack` sizes itself from its FIRST child, so
/// whatever this reports is exactly the box the toast corners on.
///
/// Before this existed the stack's base layer was the media element itself, whose size is the
/// picture's FITTED pixels: a small capture gave a small, centred anchor, so a toast opened near
/// the middle of the window and flitted to the right as the media settled. Wrapping keeps every
/// property that made "over the media" the original choice — the region is still strictly the
/// space BETWEEN the toolbars, so a toast can never sit over the toolbars, the annotation tray or
/// the video transport strip — while making the position independent of the media's size.
pub(super) fn toast_region<'a>(
    content: Element<'a, Msg>,
    width: Length,
    height: Length,
) -> Element<'a, Msg> {
    // `center_x`/`center_y` set the axis length AND its alignment, so this is the same container
    // the windowed arm always wrapped its media in.
    widget::container(content).center_x(width).center_y(height).into()
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
                    Some(Background::Color(c.background(false).base.into()))
                };
                cosmic::iced::widget::container::Style { background, ..Default::default() }
            } else {
                // A washed-down group panel: present but quieter than the toolbars.
                let mut bg: cosmic::iced::Color = c.background(false).component.base.into();
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
/// rounded corners cleanly. `bottom_pad` is [`BAR_V_PAD`] for the bottom bar and the
/// caption-symmetry answer ([`top_bar_bottom_pad`]) for the top one — explicit rather than
/// derived here, so the ONE seam `chrome_h` reserves from is also the one the caller reads.
pub(super) fn preview_bar(
    row: Element<'_, Msg>,
    round_bottom: bool,
    glass: Option<crate::app::theme::GlassConfig>,
    bottom_pad: f32,
) -> Element<'_, Msg> {
    widget::container(row)
        .width(Length::Fill)
        .align_x(Alignment::Center)
        .padding(cosmic::iced::Padding {
            top: BAR_V_PAD,
            right: 12.0,
            bottom: bottom_pad,
            left: 12.0,
        })
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
                Some(Background::Color(c.primary(false).base.into()))
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
        self.tool_toggle_gated(name, on, true, msg, tip, tip_pos)
    }

    /// [`Self::tool_toggle`] with an ENABLED gate (DRAGON-392 correction): a disabled toggle keeps
    /// the identical footprint but draws its glyph in [`disabled_label_tone`] — the group's own
    /// BORDER colour, the faintest line this chrome draws — and carries NO `on_press`, so it
    /// reads and behaves as unavailable. A crop SESSION gates the tray this way; an ordinary call
    /// passes `true` and is byte-identical to the historical button.
    ///
    /// (`history_button`'s disabled undo/redo keep `theme::subdued` instead: those sit FLAT on
    /// the header bar with no cluster behind them, so the page-background token is the right one
    /// there. Same principle — measure against the surface you are actually drawn on.)
    #[allow(clippy::too_many_arguments)]
    pub(super) fn tool_toggle_gated(
        self,
        name: &'static str,
        on: bool,
        enabled: bool,
        msg: PreviewMsg,
        tip: Element<'static, Msg>,
        tip_pos: widget::tooltip::Position,
    ) -> Element<'static, Msg> {
        let icon = crate::widgets::icons::sized(name, self.icon_box())
            .class(cosmic::theme::Svg::Custom(std::rc::Rc::new(move |t: &cosmic::Theme| {
                let color = match (enabled, on) {
                    (false, _) => disabled_label_tone(t),
                    (true, true) => crate::app::theme::accent(t),
                    (true, false) => crate::app::theme::foreground(t),
                };
                cosmic::widget::svg::Style { color: Some(color) }
            })));
        // DRAGON-357 item 4: the ACTIVE toggle gains a 1px accent border, drawn INSIDE the
        // button bounds (`tool_toggle_style`) so the footprint is byte-identical in both states
        // (no reflow on selection). The Icon button's own base/hover/pressed backgrounds are
        // reconstructed from the theme's `icon_button` component so hover feedback is preserved;
        // the glyph colour still rides the SVG class above (accent when on). Shared by EVERY
        // toggle group — the top tray tools (Select / Hand / Crop included) and the seven
        // line-width segments — so the accent ring appears consistently on whichever is armed.
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
            // Disabled = NO `on_press` at all (the `history_button` pattern): the press can't
            // fire and the button reports itself as inert, hover feedback included.
            .on_press_maybe(enabled.then_some(Msg::Preview(self.pid, msg)))
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
        // DRAGON-392 correction — WHY the colour is set here explicitly. Every OTHER button in
        // this chrome ([`Self::tool_toggle`]) hands its glyph an explicit `Svg::Custom` colour;
        // this one used to leave the class at its default, so the bundled lucide SVG rendered
        // with its own `currentColor` instead of the segment's. On the accent-filled ACTIVE
        // segment that reads as a dark knockout — the owner's "the icons seem like they are
        // cutouts". An explicit class also makes this glyph immune to the container below,
        // which would otherwise replace its ink (see `theme::on_accent_content`).
        //
        // DRAGON-607: the colour is now `theme::segment_ink(t, active)` rather than a flat
        // `on_accent`. It has to depend on `active`, because only the ACTIVE segment is filled
        // with the accent; the inactive one wears the `state_mix` wash, and on-accent ink there
        // was near-black on dark grey. `segment_ink` is the SAME function `segment_style` uses
        // for the button's own `icon_color`/`text_color`, so the explicit class here and the
        // button style can never disagree. The owner chose legibility over one uniform glyph
        // colour when that tradeoff was put to them; `segment_style`'s doc carries the history.
        let glyph = crate::widgets::icons::sized(icon, self.icon_box())
            .class(cosmic::theme::Svg::Custom(std::rc::Rc::new(move |t: &cosmic::Theme| {
                cosmic::widget::svg::Style {
                    color: Some(crate::app::theme::segment_ink(t, active)),
                }
            })));
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

    /// The CROP control's buttons (DRAGON-382; DRAGON-392 moved it into the tray group beside
    /// Select and Hand and made it return ITEMS rather than its own cluster, so it sits inside
    /// that one group's border). Idle: a plain crop icon. Active (a live crop session): that slot
    /// becomes the LABELLED **Apply Crop** button plus an x that cancels, so the one slot opens,
    /// confirms and abandons the crop. Lives on the TOP tray, so tooltips drop below.
    ///
    /// **DRAGON-410 — why accept carries a LABEL, and why it is a stock `button::suggested`.**
    /// A crop session now hides every other top-bar control ([`top_bar_item_state`]), so the pair
    /// is alone on the bar: nothing is being crowded, and the one control the user must find gets
    /// named rather than left as a glyph to decode. It is built as
    /// `button::suggested(..).leading_icon(..)` — so the icon-to-label gap, the label's
    /// size/weight and the accent fill are libcosmic's own and match a settings button BY
    /// CONSTRUCTION. Copying a spacing
    /// number here instead would drift the moment the theme's `space_xxxs` moves. The button's
    /// stock height (`space_l`, 32px) is a hair over the tray's scaled button box (~31px), which
    /// the cluster absorbs; it is NOT overridden, because overriding it is exactly the
    /// hand-tuning this composition exists to avoid.
    ///
    /// This doc used to add "the SAME composition as the settings About page's donate
    /// button". It was not, and the difference was the DRAGON-607 bug: donate goes through
    /// `settings::row::centered_button`, which wraps its label in a centring container, and
    /// a container overrides the ink beneath it, so donate drew a near-white label on the
    /// accent while THIS button drew the correct dark one. They match now because that
    /// wrapper carries the on-accent ink, but the compositions are still different, so do
    /// not reason about one from the other.
    ///
    /// **The accept mark is a CHECKMARK at the set's own stroke weight** — no bold variant. The
    /// owner's rule (DRAGON-392): the crop, accept and cancel glyphs carry the same weight as
    /// every other toolbar icon, because out-of-sync variants read as a mistake. A `+1px` pass on
    /// this pair was tried and withdrawn; a test now pins the whole bundled set to one weight
    /// (`widgets::icons`).
    ///
    /// The accept glyph is still a SPLIT check (two paths, so its vertex is two overlapping round
    /// caps rather than a stroke join) — that fix is independent of weight and is what stopped
    /// the tip chipping; see `widgets::icons` for the measurements. Cancel is the stock shared
    /// `x`, and stays ICON-ONLY: it is the ordinary "get me out of here" affordance every close
    /// in this chrome wears, and labelling it too would give the destructive half the same visual
    /// weight as the one the user is meant to reach for.
    pub(super) fn crop_items(
        self,
        active: bool,
        km: &crate::shortcuts::Keymap,
    ) -> Vec<Element<'static, Msg>> {
        let pos = widget::tooltip::Position::Bottom;
        // The two branches are EXCLUSIVE: the idle crop button is not drawn during a session, and
        // the accept/reject pair does not exist outside one.
        if active {
            // Enter/Escape drive the same two messages (see `preview_modal_key`). The tooltip
            // still carries the key hint even though the button is labelled — the label says
            // WHAT, the tip says HOW to do it from the keyboard.
            let accept = self.tip(
                crate::widgets::arrow_cursor::arrow_cursor(
                    widget::button::suggested(CROP_ACCEPT_LABEL)
                        .leading_icon(crate::widgets::icons::handle("crop-accept-symbolic"))
                        .on_press(Msg::Preview(self.pid, PreviewMsg::CropAccept)),
                ),
                widget::text(fixed_key_tip("Apply crop", "Enter")).size(12),
                pos,
            );
            // DRAGON-392 correction: the X is PURE foreground — white in the dark themes,
            // near-black in the light ones — not the `subdued` wash it wore, which read as
            // "unavailable" next to the accent-filled accept. The glyph is the SHARED stock `x`
            // (every Close button's), at the set's one stroke weight.
            let cancel = self.tool_button_tinted(
                "window-close-symbolic",
                fixed_key_tip("Cancel crop", "Esc"),
                PreviewMsg::CropCancel,
                pos,
                Some(crate::app::theme::foreground),
            );
            vec![accept, cancel]
        } else {
            vec![self.tool_toggle(
                "crop-symbolic",
                // Never "on" — see the note above. Not a state to compute: a literal, so there is
                // nothing here for a future edit to wire a document fact back into.
                false,
                PreviewMsg::CropEnter,
                // The tooltip carries the live shortcut hint (DRAGON-385: C).
                widget::text(action_tip("Crop", crate::shortcuts::Action::PreviewCrop, km))
                    .size(12)
                    .into(),
                pos,
            )]
        }
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
    ///
    /// (DRAGON-617 baked both keys, so this no longer takes the keymap.)
    fn flat_history(
        e: &EditState,
        build: impl Fn(&'static str, String, PreviewMsg, fn(&cosmic::Theme) -> cosmic::iced::Color) -> Element<'static, Msg>,
    ) -> Vec<Element<'static, Msg>> {
        use crate::shortcuts::FixedEditorAction;
        let one = |icon, name, action: FixedEditorAction, msg, on: bool| {
            let tint: fn(&cosmic::Theme) -> cosmic::iced::Color =
                if on { crate::app::theme::foreground } else { crate::app::theme::subdued };
            build(icon, fixed_key_tip(name, action.label()), msg, tint)
        };
        vec![
            one(
                "edit-undo-symbolic",
                "Undo",
                FixedEditorAction::Undo,
                PreviewMsg::Undo,
                e.can_undo(),
            ),
            one(
                "edit-redo-symbolic",
                "Redo",
                FixedEditorAction::Redo,
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
        let mut items: Vec<Element<'static, Msg>> = Vec::new();
        // No "pop out to fullscreen" button where there is no fullscreen editor to pop out
        // to: Windows 10, whose overlay would inherit the software rasterizer that cannot
        // draw these shader layers (DRAGON-427), and a Linux session with no layer shell,
        // where the overlay preview has no surface type to be at all (`lab/flatpak`: cosmic-comp
        // hides the global from sandboxed clients, and mutter never shipped it). Offering a
        // button that refuses is worse than not offering it. Every session that CAN show the
        // overlay editor keeps the button exactly where it was.
        if crate::platform::overlay_preview_available() {
            items.push(titlebar_button(
                pid,
                "view-fullscreen-symbolic",
                "Fullscreen overlay".to_string(),
                PreviewMsg::ToggleAppearance,
                crate::app::theme::accent,
            ));
        }
        // DRAGON-353: Settings, immediately to the RIGHT of the appearance toggle — the
        // same slot the overlay header puts it in, so the two chromes agree.
        items.push(titlebar_button(
            pid,
            "emblem-system-symbolic",
            "Settings".to_string(),
            PreviewMsg::OpenSettings,
            crate::app::theme::accent,
        ));
        items.extend(Tb::flat_history(&preview.edit, move |i, t, m, c| {
            titlebar_button(pid, i, t, m, c)
        }));
        items
    }

    /// The windowed preview's titlebar upload indicator + Cancel (DRAGON-490): a small,
    /// FIXED-SIZE progress track (no numeric percentage, per the owner's request) plus a
    /// Cancel control, shown only while this document has an upload it started still in
    /// flight ([`EditState::uploads`]). Empty when there is none, so the caller's `.end(...)`
    /// fold is simply a no-op then.
    ///
    /// Meant to be folded onto the header bar's `.end(...)` slot BEFORE the platform's own
    /// caption-button tail (`preview_view`'s call site), so it always sits to their LEFT on
    /// every platform — "before any caption buttons", per the owner's own wording. The header
    /// bar's row height is fixed regardless of its content (`HeaderBarWidget`), so nothing
    /// pushed here can ever grow the titlebar.
    ///
    /// Returns AT MOST ONE element, not one per control (DRAGON-490 follow-up, owner
    /// live-test): Cancel and the indicator were built through unrelated helpers with no reason
    /// to agree on a rendered height, so pushing them as two SEPARATE `.end(...)` items left
    /// them centred independently rather than centred RELATIVE TO EACH OTHER. Bundling them
    /// into one `Alignment::Center` row, before either ever reaches `.end(...)`, is what
    /// actually pins their shared centreline — a fixed padding nudge on one side would only
    /// re-drift the moment either control's own size changes. DRAGON-500 took that further and
    /// moved Cancel INSIDE [`upload_meter`] at the meter's own glyph box, so the two no longer
    /// have separate sizes to reconcile at all; this method now just names the upload.
    ///
    /// The OLDEST tracked upload (first-started) is what the bar and the Cancel button both
    /// act on. Several uploads from the same document at once is a real but rare state
    /// (nothing stops pressing Upload again before the first finishes); showing and
    /// cancelling them one at a time keeps this from growing into a list inside the
    /// titlebar. The tooltip names the count when there is more than one.
    pub(super) fn preview_upload_titlebar(&self, preview: &PreviewState) -> Vec<Element<'static, Msg>> {
        let Some(watch) = preview.edit.uploads.first() else {
            return Vec::new();
        };
        vec![upload_meter(preview.window, watch, preview.edit.upload_anim)]
    }
}

/// The upload METER, as both preview surfaces draw it (DRAGON-490, restyled DRAGON-495): the
/// account's brand mark, the Cancel control, and the progress track, in one bordered pill.
///
/// **One builder, two surfaces.** The windowed editor folds it onto the header bar
/// ([`App::preview_upload_titlebar`]) and the overlay puts it in its own header row
/// ([`App::overlay_header_row`]), which used to have no upload readout at all: an upload
/// started from the fullscreen editor showed nothing anywhere in the app (the owner's report).
/// EVERYTHING is shared now, Cancel included (DRAGON-500, see [`meter_cancel`]), so the meter
/// cannot drift between the two; only `pid` differs, and that is the same window either way.
///
/// **The container is the toolbar-group treatment** at a LOWER profile (owner's ask): the
/// component-base fill, hairline [`cluster_border`] outline and capsule rounding
/// [`Tb::tool_cluster`] wears, but with its own small padding rather than a group's, since this
/// rides inside a header row whose height it must not drive. The padding is what keeps the
/// brand mark and the track off the border.
///
/// **The brand mark leads it** so a glance says WHERE the capture is going, which the tooltip
/// otherwise has to be hovered for. Drawn through the un-tinted `brand()` handle (see
/// `widgets::icons::brand`: a brand mark keeps the provider's own colours and must never take a
/// colouring class), from the provider snapshotted on the watch. An account whose provider this
/// build does not know simply has no mark, and the meter starts with the track.
///
/// **Cancel sits BETWEEN the mark and the track** (owner's ordering feedback): "cancel the
/// thing" reads left-to-right before "here's how far along it is". What it offers is
/// [`super::edit::meter_cancel_action`]'s answer (DRAGON-500, and the stale link to a
/// `meter_cancelable` that has not existed since is fixed in DRAGON-514).
///
/// **The meter PERSISTS after the upload ends** (DRAGON-514): it announces the outcome, settles
/// (`super::edit::MeterFace::Settled`), and then stays until this document starts another
/// upload or the editor closes. It used to retire itself a few seconds after finishing, which
/// took the undo with it.
///
/// **The meter itself carries NO tooltip** (DRAGON-514, owner's call). DRAGON-500 had narrowed
/// it from the whole row to the TRACK alone, so hovering the brand mark or the X no longer
/// raised "42% uploaded to …" on top of the X's own tip. The remaining one is gone outright:
/// the meter is a readout the user is already looking at, in a header they are moving the
/// pointer across on the way to Save and Close, and a hover card explaining a bar that is
/// visibly filling was noise on the path to every other control. Nothing replaces it, in any
/// state. The X keeps its own tip, which names the action and the account
/// ([`meter_cancel`]) and is the one thing on this pill a press can be wrong about.
fn upload_meter(
    pid: window::Id,
    watch: &super::edit::UploadWatch,
    anim: f32,
) -> Element<'static, Msg> {
    let face = super::edit::meter_face(watch);
    let mut items: Vec<Element<'static, Msg>> = Vec::with_capacity(4);
    if let Some(spec) = crate::cloud::provider(&watch.provider) {
        // The meter's own glyph box, the one the X wears too, not the meter's height: a brand
        // mark scaled up to a container's full height reads as a logo, not as a label.
        items.push(crate::widgets::icons::sized(spec.icon, METER_ICON).into());
    }
    // **Copy, then the X** (DRAGON-520, owner's call: "just before the trash icon"). Offered
    // only by an upload that finished, succeeded and actually made a share link
    // (`UploadWatch::copyable`), which is the same upload whose "Copied to clipboard" toast the
    // user already saw. It re-copies that link, for the very ordinary case of having copied
    // something else since. `METER_ICON` with no halo, the square the brand mark and the X wear,
    // so the pill's height is still `METER_PILL_H` and the asserts below still describe it.
    if watch.copyable().is_some() {
        items.push(crate::widgets::copy_button::copy_button(
            crate::widgets::copy_button::copied_recently(watch.copied_at),
            0,
            // Below the pill, the same "below the button" position every other titlebar control
            // here uses, so the card clears the icon it describes instead of covering it.
            widget::tooltip::Position::Bottom,
            Msg::Preview(pid, PreviewMsg::UploadCopyLink(watch.session_id.clone())),
        ));
    }
    if let Some(action) = super::edit::meter_cancel_action(face, watch.undoable()) {
        items.push(meter_cancel(pid, watch.session_id.clone(), action, &watch.label));
    }
    // Bare, with no tooltip wrapper of any kind: see the note above.
    items.push(upload_progress_track(face, anim));
    let row = widget::row(items).spacing(6.0).align_y(Alignment::Center);
    widget::container(row)
        .padding(METER_PAD)
        .align_y(Alignment::Center)
        .class(cosmic::theme::Container::Custom(Box::new(move |theme| {
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(
                    theme.cosmic().background(false).component.base.into(),
                )),
                border: Border {
                    radius: crate::app::theme::rounding(theme).xl.into(),
                    width: METER_BORDER,
                    color: cluster_border(theme),
                },
                ..Default::default()
            }
        })))
        .into()
}

/// The meter's own CANCEL control (DRAGON-500): the stop glyph at exactly [`METER_ICON`], the
/// SAME square the brand mark beside it occupies, with NO halo of its own.
///
/// **This is the clipping fix.** Cancel used to be built by the CALLER, each surface handing in
/// its own standard header control: the windowed titlebar's [`titlebar_button`] (a 16px glyph in
/// an 8px halo) and the overlay's [`Tb::header_button`] (an 18px glyph in a 6.56px halo). Both
/// are, by construction, exactly as tall as their row — libcosmic's `HeaderBarWidget` content
/// row is a fixed 32px and the overlay's is `icon_box + 2 × btn_pad` = 31.12px, and each of
/// those buttons is that same number. Put one inside this pill and it has to fit the row MINUS
/// the pill's `2 × METER_PAD.vertical` = 6px, which it cannot, so iced clamps it: a button
/// resolves its size around its content, so the 6px comes off the GLYPH box, not off the halo.
/// The titlebar's 16px X was drawn into 10px and the overlay's 18px one into 12px, roughly
/// two thirds of their height in a row of full-height siblings — the X "cut in half". On macOS
/// it was worse again, since `titlebar_button` also TOP-pins there, so the squashed glyph sat
/// against the pill's top edge instead of centred in it.
///
/// Nothing was wrong with the pill's padding (DRAGON-495 shrank it deliberately, and this rides
/// in a row whose height it must never drive). The X was simply carrying a full-size button's
/// footprint into a container that has to fit INSIDE one. At [`METER_ICON`] with no halo it
/// needs 16px of the 26px the pill has, and the compile-time asserts below keep the pill itself
/// inside both rows. ONE constant does both squares, so the X and the brand mark beside it
/// cannot drift apart again.
///
/// Never TOP-pinned, unlike the mac arm of [`titlebar_button`]: this control is centred in a
/// pill, not aligned to the traffic lights, and the meter rides in the header's TRAILING slot
/// where there are none. It keeps its own tooltip, which is the only tip a hover over it raises.
///
/// **One control, two jobs** (DRAGON-507). `action` is [`super::edit::meter_cancel_action`]'s
/// answer, never a guess made here: while the transfer runs the X CANCELS it, and once a
/// successful upload has landed the same square UNDOES it by deleting the file. The glyph and
/// the wording change with the job, because "stop this" and "take that back" are different
/// promises; the position, size and colour do not, because it is still the one control the
/// meter has.
///
/// The undo is available for as long as the meter is (DRAGON-514), which is now until the next
/// upload or the editor's close, rather than the few seconds the old finish-hold allowed.
fn meter_cancel(
    pid: window::Id,
    session: String,
    action: super::edit::MeterAction,
    label: &str,
) -> Element<'static, Msg> {
    use super::edit::MeterAction;
    let (glyph, tip, msg) = match action {
        MeterAction::Cancel => (
            "process-stop-symbolic",
            "Cancel upload".to_string(),
            PreviewMsg::UploadCancel(session),
        ),
        // The trash glyph, not an undo arrow: what this does is DELETE the uploaded file, and
        // the tooltip names the account so nobody presses it thinking it undoes the edit.
        // DRAGON-514: `user-trash-symbolic` is now MAPPED in `widgets::icons` (to lucide
        // `trash-2`, the can the folder browser's own delete already wears), so this square is
        // the app's trash rather than whatever the system icon theme happened to supply next to
        // lucide glyphs on either side of it.
        MeterAction::Undo => (
            "user-trash-symbolic",
            format!("Remove from {label}"),
            PreviewMsg::UploadUndo(session),
        ),
    };
    flat_icon_button(pid, glyph, tip, msg, crate::app::theme::accent, METER_ICON as u16, 0, false)
}

/// The glyph box for BOTH of the meter's square marks: the leading brand mark and the Cancel X
/// ([`meter_cancel`]). One constant on purpose (DRAGON-500) — two equal literals would drift the
/// first time either was tuned, and the X being a different square from the mark beside it is
/// exactly what the pill is too small to absorb.
const METER_ICON: f32 = 16.0;

/// The meter pill's inner padding: `[vertical, horizontal]`. Deliberately SMALLER than a
/// toolbar group's (`Tb::grp_pad`), which is what makes this a lower-profile version of the
/// same treatment rather than a second toolbar inside the header; enough that neither the
/// brand mark nor the track's rounded ends touch the border.
const METER_PAD: [f32; 2] = [3.0, 8.0];

/// The pill's hairline outline, in the [`cluster_border`] colour a toolbar group wears.
const METER_BORDER: f32 = 1.0;

/// What the meter pill asks for vertically: its tallest content (the two [`METER_ICON`]
/// squares; the track is far shorter) plus its padding and its outline on both edges. The
/// outline is drawn INSIDE the bounds and so costs no layout space; counting it anyway is
/// deliberate slack, so the check below also guarantees the hairline never lands on a glyph.
const METER_PILL_H: f32 = METER_ICON + 2.0 * METER_PAD[0] + 2.0 * METER_BORDER;

/// The windowed preview's titlebar content height. Not ours: libcosmic's `header_bar` fixes its
/// `HeaderBarWidget` row at a flat 32px and adds its padding OUTSIDE that. Mirrored here only so
/// the assert below can see it, which is the whole point — the meter has to fit in a number it
/// cannot change.
const TITLEBAR_CONTENT_H: f32 = 32.0;

// DRAGON-500: the meter is a passenger in two FIXED-height rows and can never make either
// taller, so a pill that wants more than the row has is not a layout that stretches, it is a
// control that gets clamped and drawn cut in half. Both bounds are checked here rather than
// discovered on screen. (The overlay row is laid out at `icon_box + 2 × btn_pad`, whose
// `box_px` rounding is not const-evaluable; `OVERLAY_HEADER_H` is the same expression without
// the rounding, and the ~7px of slack below swallows the difference many times over.)
const _: () = assert!(
    METER_PILL_H <= TITLEBAR_CONTENT_H,
    "the upload meter must fit inside libcosmic's fixed titlebar row, or its contents get clamped"
);
const _: () = assert!(
    METER_PILL_H <= super::surface::OVERLAY_HEADER_H,
    "the upload meter must fit inside the overlay's fixed header row, or its contents get clamped"
);

/// The upload progress track (DRAGON-490, DRAGON-495): a small rounded, subdued-background bar
/// with a FILL proportional to the transfer's progress, or a plain spinner while no genuine
/// percentage has arrived yet (`MeterFace::Spinner` — every upload starts there, DYNAMICALLY
/// switching per transfer the moment the child observes a real interim value,
/// `cloud::tray::still_indeterminate`; it is never predicted from the file's size). Same "no
/// numbers, no lies" rule either way: a static bar frozen on one value reads as broken, not as
/// "there is nothing to show yet".
///
/// **The fill's colour is the outcome** (DRAGON-495): the accent while it runs, the theme's
/// SUCCESS colour once it completes, the danger colour if it stopped. The success state is only
/// reachable because the watch is now HELD past completion (`UploadWatch::finished`); before
/// that the meter simply vanished when the upload ended. Both end colours come from the theme
/// tokens every other success/failure in the app uses (`app::theme::success` / `danger`), never
/// a literal green or red.
///
/// **A finished upload keeps its colour until the next one starts** (DRAGON-516, owner's call,
/// REVERSING DRAGON-514). DRAGON-514 made the meter persistent and then, reasoning that
/// "full-strength success green held indefinitely is a notification that never stops
/// notifying", dropped `MeterFace::Settled(Done)` to `app::theme::subdued` once the
/// announcement ran out. Seen on screen the owner read that as the upload having been
/// forgotten rather than as it having quietly succeeded: the whole point of keeping the meter
/// is that a glance at the header says "that capture is in the cloud", and a faded bar does not
/// say it. So the two finished faces now draw IDENTICALLY, and settling is no longer visible at
/// all. It still does real work — it is what stops the 500ms poll
/// (`super::edit::upload_needs_poll`) — it simply no longer changes the picture.
fn upload_progress_track(face: super::edit::MeterFace, anim: f32) -> Element<'static, Msg> {
    use super::edit::MeterFace;
    if matches!(face, MeterFace::Spinner) {
        // Sized to the meter's own glyph box (`METER_ICON`, what the brand mark and the Cancel
        // X both wear) rather than the widget's 40px default — the loading view's own use,
        // `preview_loading_view`, wants that larger size; a header row does not.
        // Self-animating (`Circular`'s own `update` requests a redraw on every frame), so
        // nothing here has to drive a tick for it.
        return widget::indeterminate_circular().size(METER_ICON).bar_height(2.0).into();
    }
    if matches!(face, MeterFace::Finalizing) {
        // The finalize wait (DRAGON-537): full in the accent, with the darker block sweep
        // saying "still working" where a motionless full bar read as done or stuck. `anim`
        // is `EditState::upload_anim`, advanced by `sub_upload_finalize_anim` while any
        // meter is in this state; unlike the spinner above, this widget does not drive its
        // own redraws, so the tick is what animates it.
        return upload_track_shell(crate::widgets::upload_stripes::upload_stripes(
            TRACK_W,
            TRACK_H,
            anim,
            meter_tint(face),
        ));
    }
    upload_track_bar(super::edit::meter_fill(face), meter_tint(face))
}

/// Which theme colour a face's FILL is drawn in. Pure; unit-tested.
///
/// Split out of [`upload_progress_track`] (DRAGON-516) for the usual reason: the track builds
/// an `Element`, which nothing headless can look at, while this is the whole decision and is
/// three theme lookups. The rows are argued for in [`upload_progress_track`]'s own doc.
fn meter_tint(face: super::edit::MeterFace) -> fn(&cosmic::Theme) -> cosmic::iced::Color {
    use super::edit::{MeterFace, UploadOutcome};
    match face {
        // DRAGON-516: the announced and the SETTLED success are one colour. A completed upload
        // holds its green until another upload replaces the meter or the editor closes, which
        // is what makes a glance at the header answer "did that capture go up?".
        MeterFace::Finished(UploadOutcome::Done) | MeterFace::Settled(UploadOutcome::Done) => {
            crate::app::theme::success
        }
        // DRAGON-537: the finalize wait KEEPS the running colour (owner's call). The colour
        // says "still working"; the stripe sweep, not a colour change, is what marks the
        // state. Success green here would claim an outcome that has not arrived.
        MeterFace::Finalizing => crate::app::theme::accent,
        // DRAGON-507: a pressed X turns the bar red AT ONCE, on the same token a genuine stop
        // uses. The press is the event; nothing waits for the provider to agree. A failure was
        // never faded even under DRAGON-514, for the reason that still holds: it is not
        // resolved by time passing, it is still a file that did not arrive.
        MeterFace::Finished(UploadOutcome::Stopped)
        | MeterFace::Settled(UploadOutcome::Stopped)
        | MeterFace::Ending(_) => crate::app::theme::danger,
        // Running: the spinner never reaches here (it returns above), so this is a percentage.
        MeterFace::Spinner | MeterFace::Progress(_) => crate::app::theme::accent,
    }
}

/// The drawn bar: a `fraction`-wide `tint` fill inside the track.
///
/// The TRACK behind the fill is [`cluster_border`] — the theme's `background.divider`, already
/// flattened to an opaque colour — which is the same hairline the meter pill draws its own
/// outline in. So the empty part of the bar reads as a channel cut into the pill's chrome,
/// while the FILL keeps every colour that carries meaning (running / succeeded / stopped).
///
/// **`app::theme::subtle` was the wrong kind of token, and this is the second time the track
/// has been retuned** (DRAGON-516). DRAGON-514 moved it there from a raw `palette.neutral_5`,
/// wanting a meter that stays on screen to recede into the header. `subtle` cannot do that job:
/// it is a TEXT token, built by blending the ON-BACKGROUND colour 39% toward the background, so
/// in a dark theme it lands near 60% white. That is correct for a line of secondary copy and
/// wrong for 5px of unfilled background, which is why the owner reported the track reading as a
/// bright grey bar. A background token cannot make that mistake, because it is derived from the
/// background rather than from the foreground.
///
/// `cluster_border` in particular, over a low-alpha neutral, because its own doc carries
/// MEASURED numbers in both directions — dark `0.266` against a `0.182` fill, light `0.689`
/// against `0.960` — which is exactly the property this needs and the one `subtle` failed: it
/// recedes in the light theme too, and it is documented as never invisible, which guards
/// against re-earning the earlier "the track is barely visible" report from the other side.
/// It is also opaque, so it composites the same on the windowed titlebar and on the overlay's
/// header, which sit on different surfaces.
/// The track's own footprint: small enough to sit comfortably in a titlebar row without
/// crowding the controls beside it, and fixed regardless of state so the indicator
/// appearing/disappearing never nudges anything else in the row sideways by a variable
/// amount — only the FILL inside it moves. Doubled from an original 40px (DRAGON-490
/// follow-up, owner live-test): the narrower track read as barely visible next to the
/// Cancel button. Module-level since DRAGON-537, because the finalize arm sizes its
/// animated fill to the same numbers.
const TRACK_W: f32 = 40.0 * 2.0;
const TRACK_H: f32 = 5.0;

fn upload_track_bar(
    fraction: f32,
    tint: fn(&cosmic::Theme) -> cosmic::iced::Color,
) -> Element<'static, Msg> {
    let fill_w = TRACK_W * fraction.clamp(0.0, 1.0);
    let fill: Element<'static, Msg> =
        widget::container(widget::Space::new().width(Length::Fill).height(Length::Fill))
            .width(Length::Fixed(fill_w))
            .height(Length::Fill)
            .class(cosmic::theme::Container::custom(move |theme| {
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(tint(theme))),
                    border: Border { radius: (TRACK_H / 2.0).into(), ..Default::default() },
                    ..Default::default()
                }
            }))
            .into();
    upload_track_shell(fill)
}

/// The track's outer shell: the fixed footprint and the channel-toned rounded background
/// every state's fill sits in. Split out of [`upload_track_bar`] by DRAGON-537, so the
/// finalize arm's animated fill and the plain fills share one footprint and one tone and
/// cannot drift.
fn upload_track_shell(fill: Element<'static, Msg>) -> Element<'static, Msg> {
    widget::container(fill)
        .width(Length::Fixed(TRACK_W))
        .height(Length::Fixed(TRACK_H))
        .class(cosmic::theme::Container::custom(|theme| cosmic::iced::widget::container::Style {
            background: Some(Background::Color(upload_track_tone(theme))),
            border: Border { radius: (TRACK_H / 2.0).into(), ..Default::default() },
            ..Default::default()
        }))
        .into()
}

/// The unfilled part of the upload meter's track (DRAGON-516). See [`upload_track_bar`] for
/// why it is this token and not a text one.
///
/// A named function rather than an inline call so the test below can assert on the SAME thing
/// the widget draws, instead of re-stating the choice and then agreeing with itself.
fn upload_track_tone(theme: &cosmic::Theme) -> cosmic::iced::Color {
    cluster_border(theme)
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
            let glyph = crate::widgets::icons::sized(icon, self.icon_box())
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

/// **The horizontal inset a toolbar cluster gives CONTENT that isn't a button** — the zoom
/// cluster's slider, which would otherwise hug the group's border.
pub(super) const CLUSTER_INNER_PAD: f32 = 8.0;

/// **The gap between two items of a toolbar row** ([`toolbar_row`]) — i.e. what separates one
/// bordered group from the next. Named because the document-info block, which has no group chip
/// of its own any more, pads itself by exactly this: bare text needs its own gap so it clears
/// its neighbours' borders instead of nearly touching them (one gap from the row + one of its
/// own = it reads as separate from the controls, which is what it is).
pub(super) const TOOLBAR_ROW_GAP: f32 = 8.0;

/// **The QUIET foreground for content drawn on `surface`** — the shared "present but secondary"
/// tone of the preview chrome: the slider glyphs, the document-info block, and every DISABLED
/// control (which is why there is exactly one of these rather than a separate disabled tone).
///
/// WHY it takes a surface instead of using [`crate::app::theme::subdued`]. That token is the
/// foreground blended 78% toward `background.base`, the PAGE background — correct for a caption
/// on the plain background (settings rows, the covermark picker's "None"), and wrong anywhere
/// else, because the preview chrome paints on OTHER surfaces: a cluster is
/// `background.component.base` (lighter), a toolbar bar is `primary.base`. Text subdued against
/// the page can land almost exactly on the fill it is actually sitting on, which is what made the
/// info block unreadable (DRAGON-392 correction).
///
/// So: blend the foreground HALFWAY toward the surface the content really sits on. Same idea,
/// right backing, and a fixed 50% keeps every quiet thing in the chrome at one strength. Theme
/// tokens only, so it tracks light/dark and the user's accent. (True alpha is not available
/// through the icon widget — see [`Tb::slider_with_icon`]'s note — so this is a real, opaque
/// colour rather than a transparency.)
fn quiet_on(t: &cosmic::Theme, surface: cosmic::iced::Color) -> cosmic::iced::Color {
    let fg = crate::app::theme::foreground(t);
    let mix = |a: f32, b: f32| a + (b - a) * 0.5;
    cosmic::iced::Color::from_rgb(
        mix(fg.r, surface.r),
        mix(fg.g, surface.g),
        mix(fg.b, surface.b),
    )
}

// **THE cluster BORDER colour** — the 1px hairline [`Tb::tool_cluster`] outlines every toolbar
// group with, and (DRAGON-392 correction) the tone a DISABLED control's glyphs/text wear, at the
// owner's instruction: "make the subdued color for these toolbar buttons actually match the
// button group border". Read through this ONE import by both, so a theme change can never move
// the border without moving the disabled tone with it. The body (and the token's full story)
// moved to `app::theme` in DRAGON-475, when the capture toolbar became its second consumer.
pub(super) use crate::app::theme::cluster_border;

/// [`quiet_on`] the CLUSTER fill (`background.component.base`) — content inside a bordered
/// toolbar group: the slider glyphs, and every control the crop session disables.
pub(super) fn quiet_on_cluster(t: &cosmic::Theme) -> cosmic::iced::Color {
    quiet_on(t, t.cosmic().background(false).component.base.into())
}

/// [`quiet_on`] the BAR fill (`primary.base`, what [`preview_bar`] paints) — content drawn
/// directly on a toolbar with no group chip behind it. The document-info block is the one such
/// case (DRAGON-392 correction: the owner removed its group fill and border).
///
/// Note it is NOT `theme::subdued`: the bar is `primary.base`, not `background.base`, so the
/// plain token is still measured against a surface this text does not sit on. It is close enough
/// in the light themes and visibly wrong in the dark ones.
pub(super) fn quiet_on_bar(t: &cosmic::Theme) -> cosmic::iced::Color {
    quiet_on(t, t.cosmic().primary(false).base.into())
}

/// **THE rail width (px) of every slider that rides a toolbar button group** — the covermark
/// zoom / opacity and the spotlight dim range ([`Tb::slider_with_icon`]). Scaled by
/// [`SLIDER_SCALE`] at every use, so ONE number moves them together and the width reserve in
/// `surface.rs` reads the same constant.
///
/// The bottom bar's ZOOM rail used to be in this set too (DRAGON-392 brought it here from a
/// one-off 120px that read oversized beside them). It has since diverged — see
/// [`ZOOM_SLIDER_W`] — so this is once again the width of the ICON-led sliders only.
pub(super) const SLIDER_W: f32 = 80.0;

/// **The ZOOM rail's own width (px)** — the shared toolbar rail ([`SLIDER_W`] × [`SLIDER_SCALE`]
/// = 64) widened by a third, rounded to a whole pixel, at the owner's request.
///
/// **This deliberately contradicts DRAGON-392, which unified these widths a few commits ago —
/// do not "fix" it back.** That unification was right for what it saw: three sliders sitting in
/// the same toolbar at three different lengths, with no reason for the difference. The reason
/// arrived afterwards. DRAGON-400 took the zoom rail to a **5× range** (50%–500%), while the
/// covermark zoom/opacity and the dim range still express a bounded 0–100%. Same visual family,
/// different amount of information to carry: the zoom rail now has to resolve 450 percentage
/// points where the others resolve 100, so it needs the extra travel to stay precise — and every
/// pixel of rail also sharpens the 100% detent, which is specified in RAIL pixels
/// (`viewport::SNAP_RAIL_PX`). The others gain nothing from extra width.
///
/// So: same look, different length, for a stated reason. If the owner ever wants ALL sliders
/// wider, that is a change to [`SLIDER_W`] — not to this.
pub(super) const ZOOM_SLIDER_W: f32 = 85.0;

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
    scale * (2.0 * BTN_PAD + ICON_BOX * SLIDER_SCALE) + 6.0 + SLIDER_W * SLIDER_SCALE
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
        // `icon_box() × SLIDER_SCALE` is 20 × 0.8 = 16.0 today, integral only by luck of those
        // two numbers — so it goes through the seam like every other computed box (DRAGON-399).
        let glyph = crate::widgets::icons::sized(icon, self.icon_box() * SLIDER_SCALE)
            .class(cosmic::theme::Svg::Custom(std::rc::Rc::new(|t: &cosmic::Theme| {
                cosmic::widget::svg::Style { color: Some(quiet_on_cluster(t)) }
            })));
        let pid = self.pid;
        let slider = widget::slider(range, value, move |v| Msg::Preview(pid, to_msg(v)))
            .step(0.02f32)
            // Commit only once the drag ends (covermark stays blink-free; dim coalesces undo).
            .on_release(Msg::Preview(pid, commit))
            // The preview editor's own (smaller) thumb — see `PREVIEW_SLIDER_THUMB`.
            .class(preview_slider_class())
            .width(Length::Fixed(SLIDER_W * SLIDER_SCALE));
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
        // (No keymap read here since DRAGON-617: every key this row advertises, undo, redo
        // and Close, is baked, so their labels come from `FixedEditorAction` directly.)
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
        left.extend(Tb::flat_history(&preview.edit, move |n, t, m, c| {
            tb.header_button(n, t, m, c)
        }));
        // Esc triggers the same `Cancel`; it never deletes (that's the Delete trash button).
        let close = tb.header_button(
            "window-close-symbolic",
            fixed_key_tip("Close", crate::shortcuts::FixedEditorAction::Close.label()),
            PreviewMsg::Cancel,
            crate::app::theme::accent,
        );
        // DRAGON-495: the upload meter, in the overlay's counterpart of the windowed
        // titlebar's `.end(...)` slot — the RIGHT-hand group, immediately before Close, which
        // is where the windowed arm puts it relative to its own caption buttons. It had no
        // home here at all until now: an upload started from the fullscreen editor showed
        // nothing anywhere in the app. Same builder, so the meter, its brand mark, its Cancel
        // control and its tooltip are all identical in both surfaces (DRAGON-500 moved Cancel
        // in there too: at this row's own button metrics it did not fit the pill).
        //
        // The mac arm below puts Close on the LEFT (DRAGON-357), and the meter does NOT follow
        // it there: it is a status readout, not a window control, so it stays with the trailing
        // group on every platform.
        let meter: Vec<Element<'static, Msg>> = match preview.edit.uploads.first() {
            Some(watch) => {
                vec![upload_meter(tb.pid, watch, preview.edit.upload_anim)]
            }
            None => Vec::new(),
        };
        // DRAGON-357: on the macOS OVERLAY, Close sits to the LEFT of the Windowed toggle
        // (mac close-on-left convention) — scoped to this per-platform arm so every other OS
        // keeps Close on the right, byte-identical. The windowed preview gets Close from the
        // native window chrome, so this only affects the overlay row.
        #[cfg(target_os = "macos")]
        {
            let mut row = Vec::with_capacity(left.len() + 1);
            row.push(close);
            row.extend(left);
            toolbar_row(row, Vec::new(), meter)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let mut right = meter;
            right.push(close);
            toolbar_row(left, Vec::new(), right)
        }
    }

    /// The top edit bar above the preview content: the annotation DRAW tools (images only)
    /// on the left, then the size + Delete group and Save / Save As / Copy pushed to the
    /// right by the split. The appearance toggle, undo / redo and Close moved OUT of this
    /// bar in DRAGON-337 — to the windowed preview's titlebar
    /// ([`App::preview_header_controls`]) and the overlay's own header row
    /// ([`Self::overlay_header_row`]). The do-not-train + covermark tools live on the BOTTOM
    /// bar (see [`Self::edit_tools`]).
    ///
    /// DRAGON-478: every group present on THIS bar (and only this bar) carries a small caption
    /// under it ([`Tb::captioned`]), when the "Show toolbar group labels" setting is on. Only
    /// groups actually drawn get one — the conditions are the same `if`s that decide whether the
    /// group is built at all — and the extra height is reserved once, on the same flag, by
    /// [`PreviewSurface::chrome_h`].
    pub(super) fn edit_toolbar<'a>(&'a self, preview: &'a PreviewState, tb: Tb) -> Element<'a, Msg> {
        let km = &self.keymap;
        // DRAGON-410: a live crop session is modal over this WHOLE bar, not just the tray inside
        // it — the share group below is swept by the same rule the tray items go through.
        let cropping = preview.edit.crop_session.is_some();
        // Pointer / pan tools, the color swatch + line-width groups all live on the BOTTOM
        // bar (see [`Self::edit_tools`]). The DRAW tools (Arrow/Highlight/Box/Pixelate/Blur)
        // lead the TOP bar, for IMAGES only (videos have no annotation editor).
        let mut left: Vec<Element<'a, Msg>> = Vec::new();
        if matches!(preview.kind, PreviewKind::Image(_)) {
            left.push(annotation_tools(preview, km, tb));
        }
        // (The document-info block is NOT here: both media kinds carry it on the BOTTOM bar,
        // immediately left of the scale/zoom group — see `image.rs` / `video.rs`.)
        // Right: the Save / Save As / Copy / Delete action group — the sharing actions tinted
        // with the accent's companion while there are unsaved edits (Delete stays untinted).
        // A crop session HIDES it (DRAGON-410): every one of those actions commits or discards
        // the document, and doing that with an uncommitted crop rectangle live plainly
        // contradicts the session — Save would have written the UN-cropped image.
        let mut right: Vec<Element<'a, Msg>> = Vec::new();
        if top_bar_item_state(TopBarItem::Share, cropping) != TrayItemState::Hidden {
            // DRAGON-478: captioned like every tray cluster, and on the SAME presence condition,
            // so a crop session (which hides this group whole) hides its caption with it.
            right.push(tb.captioned(
                tb.share_group(preview, preview.unsaved(), km, self.cloud_auto_share),
                SHARE_GROUP_CAPTION,
            ));
        }
        // The Close (x) button normally lives elsewhere: the OVERLAY draws it on its header
        // row, the WINDOWED preview gets it from the native window chrome. DRAGON-268
        // follow-up (fullscreen header vanish): in NATIVE fullscreen the windowed preview's
        // traffic lights auto-hide AND our CSD titlebar goes with them, so without an
        // app-drawn Close the user has no reachable way to leave the preview — add it back
        // to this bar in that state. macOS-only signal (`preview_fullscreen`, set from the
        // resize handler); off macOS this arm is never reached and the bar carries no Close.
        //
        // DRAGON-410 deliberately does NOT sweep it. It is not an editing control that a crop
        // session could contradict — it is the WINDOW's own chrome, standing in for the titlebar
        // the OS took away, and it is the only pointer route out of a fullscreen preview. The
        // same line is drawn everywhere else: the overlay's header row and the windowed
        // titlebar (appearance / settings / undo / redo / close) are not swept either. What the
        // modal rule owns is the top bar's EDIT controls.
        #[cfg(target_os = "macos")]
        let show_close = preview.surface.is_window() && self.preview_fullscreen;
        #[cfg(not(target_os = "macos"))]
        let show_close = false;
        if show_close {
            // No caption of its own (it is the WINDOW's chrome, not an editing family), but it
            // takes the empty-caption band so it stays on the same line as the captioned groups
            // beside it — the row centres its items vertically, so an item short by the band
            // would visibly sink.
            right.push(tb.captioned(tb.cancel_group(), ""));
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
            groups.push(tb.tool_cluster(vec![
                // DRAGON-582: the colour picker tool sits BEFORE the colour swatch (the
                // owner's placement). It launches the picker as its own child, because
                // every picker launch is its own process.
                //
                // This comment used to say the pick does NOT set the annotation colour.
                // DRAGON-587 made it do exactly that, and the correction is worth keeping
                // rather than just deleting: the launch carries this editor's pid
                // (`color_picker::COLOR_TO_PID_ENV`, set by `PreviewMsg::OpenColorPicker`),
                // the picked colour comes back over the preview handoff socket
                // (`preview_ipc`'s `color` verb) and lands in
                // `App::apply_custom_annot_color`, which is where the colour wheel's own
                // custom colour lands too. A pick that reaches an editor opens no result
                // window at all.
                //
                // So the two controls are not rivals: the swatch chooses a colour you
                // already have, the pipette takes one off the screen, and both end in the
                // same place.
                tb.tool_button(
                    "color-select-symbolic",
                    "Color Picker".to_string(),
                    PreviewMsg::OpenColorPicker,
                    widget::tooltip::Position::Top,
                ),
                annot_swatch_flyout(preview, &self.annot_recent_colors, &self.keymap, tb),
                tb.stroke_width_group(e.stroke(), &self.keymap),
            ]));
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
        groups.push(tb.tool_cluster(covermark_items));
        groups
    }
}

use crate::widgets::annotation_canvas::Tool;

/// Which way a toolbar flyout (popout panel) expands from its anchor button. Every preview
/// flyout now lives on the BOTTOM bar (covermark picker + color palette / wheel), so all of
/// them float UP; the former `Down` (top-bar) direction is gone with the swatch's move down.
#[derive(Clone, Copy)]
pub(in crate::app) enum FlyoutDir {
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
pub(in crate::app) fn flyout<'a>(
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

/// One entry in the annotation tray's DECLARED layout ([`ANNOT_TRAY`]).
#[derive(Clone, Copy)]
enum TrayItem {
    /// A tool button: its glyph name, the [`Tool`] it arms, its label, and the
    /// [`crate::shortcuts::Action`] whose LIVE binding the tooltip shows.
    Tool(&'static str, Tool, &'static str, crate::shortcuts::Action),
    /// The global dim ("spotlight range") slider. Not a tool — it belongs to the spotlight's
    /// group but is always usable, whatever tool is armed (DRAGON-329).
    DimSlider,
    /// The CROP control (DRAGON-392 moved it into the tray, beside Select and Hand). Not a
    /// [`Tool`]: it opens a crop SESSION rather than arming a draw mode, and while that session
    /// is live it renders as the accept + cancel pair instead of one button
    /// ([`Tb::crop_items`]) — which is why it is its own tray item rather than a `Tool` row.
    Crop,
}

/// One GROUP of the annotation tray: its chrome treatment, its optional one-key tool SLOT,
/// plus its ordered items.
struct TrayGroup {
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
    /// The group's CAPTION (DRAGON-478) — the one word drawn under the cluster naming what the
    /// family is for. Declared HERE for the same reason `cycle` is: the tray is the single
    /// source of the grouping, so the caption is a property of the group rather than a lookup
    /// keyed on its index. Never empty for a tray group; see [`Tb::captioned`] for what an
    /// empty one means elsewhere on the bar.
    caption: &'static str,
    items: &'static [TrayItem],
}

/// **THE annotation tray layout** (DRAGON-340): the ordered GROUPS of the top bar's draw
/// tools, each rendered inside one [`Tb::tool_cluster`], in the order given. This list is the
/// single source of the tray's grouping and its order — the builder below just walks it, so
/// regrouping the tray means editing these rows and changing nothing else. (It has already
/// been re-cut several times; keep it declarative.)
///
/// A ONE-member group is not special-cased anywhere: it gets exactly the container a
/// five-member group gets.
///
/// Adding a tool = add a [`TrayItem::Tool`] row to the right group (plus its [`Tool`] variant +
/// model/rasterize/bake arms + hotkey; see `annotate.rs`'s module doc).
const ANNOT_TRAY: &[TrayGroup] = &[
    // NAVIGATION + FRAMING (DRAGON-392): the three controls that never draw anything — CROP,
    // SELECT, HAND, in that order. The POINTER creates nothing (DRAGON-341): click /
    // Ctrl-Shift-click / rubber-band to build a selection, drag it to move it all. The HAND pans
    // (it replaced the old pointer/pan mode toggle that used to sit on the bottom bar). CROP
    // opens the crop session (it used to sit beside the zoom control). Three peers, so this is a
    // REAL group with the same bordered surface every other family wears — it lost the `Bare`,
    // footprint-only treatment that suited the single button it once held. NO cycle key: each
    // owns its own letter (C / V / H).
    //
    // **The order is the owner's final call** (it supersedes the ticket's original "Select, Hand,
    // Crop" bullets): CROP leads. DRAGON-392 had a second reason for that — the session's
    // accept/reject pair took the two LEADING slots while SELECT hid, so the group's footprint
    // never changed — and DRAGON-410 retired it: a session now hides the ENTIRE rest of the bar
    // ([`top_bar_item_state`]), so this group is redrawn as the crop pair ALONE and there is no
    // footprint left to preserve. The declared order stands on the idle bar's reading order
    // alone.
    TrayGroup { cycle: None, caption: "Canvas", items: &[
        TrayItem::Crop,
        TrayItem::Tool(
            "pointer-select-symbolic",
            Tool::Pointer,
            "Select",
            crate::shortcuts::Action::PreviewAnnotPointer,
        ),
        TrayItem::Tool(
            // lucide `hand`.
            "hand-symbolic",
            Tool::Hand,
            "Hand",
            crate::shortcuts::Action::PreviewAnnotHand,
        ),
    ] },
    // Callouts: point at a thing, or number the steps. NO cycle key — the arrow is the
    // highest-frequency tool in a screenshot annotator and must never cost two presses, and
    // the step marker takes Photoshop's Count key.
    TrayGroup { cycle: None, caption: "Shape", items: &[
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
        cycle: Some(crate::shortcuts::Action::PreviewAnnotRedactCycle),
        caption: "Redact",
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
    // SHAPES: the tools that draw an emphasis shape ON the image, sharing Photoshop's own shape
    // key `U` (DRAGON-369). Spotlight joined this slot in DRAGON-384 — it is the last member of
    // the cycle, and its dim-range slider rides the group with it (a slider, not a tool button,
    // so `slot_tools` skips it and the cycle stays a four-tool chain). Membership here IS the
    // cycle: a tray chip and the hotkey slot are the SAME set.
    TrayGroup {
        cycle: Some(crate::shortcuts::Action::PreviewAnnotShapeCycle),
        // The owner's caption for this cluster, and it deliberately does NOT match the group's
        // internal name (the SHAPE cycle slot, `U`). The captions name what a user reaches the
        // family FOR, not what the hotkey model calls it; the cycle slot's own name stays on
        // `cycle` above. Note the neighbouring Arrow/Step-Marker group is captioned "Shape" for
        // the same reason — do not "correct" either to the other.
        caption: "Box Highlight",
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
            TrayItem::Tool(
                "spotlight-symbolic",
                Tool::Spotlight,
                "Spotlight",
                crate::shortcuts::Action::PreviewAnnotSpotlight,
            ),
            // The spotlight's dim range: always usable, whatever tool is armed (DRAGON-329).
            TrayItem::DimSlider,
        ],
    },
    // Text captions (DRAGON-354): the lucide `type` glyph arms the text tool. Its size
    // dropdown + font toggle are injected right after this group in `annotation_tools` (they
    // are a flyout + toggle, not tray tool-buttons, so they can't live in the data-driven tray).
    TrayGroup { cycle: None, caption: "Text", items: &[
        TrayItem::Tool(
            "text-tool-symbolic",
            Tool::Text,
            "Text",
            crate::shortcuts::Action::PreviewAnnotText,
        ),
    ] },
    // Freehand ink and the hand-undo that removes it. NO cycle key: inking and erasing are
    // the pair you alternate between fastest, so they keep a key each (`B` / `E`).
    TrayGroup { cycle: None, caption: "Draw", items: &[
        TrayItem::Tool(
            "pencil-symbolic",
            Tool::Pen,
            // DRAGON-478: DISPLAY copy only. The tool, its `Tool::Pen` variant, its
            // `PreviewAnnotPen` action and the keymap's "Pencil tool" entry are unchanged —
            // the owner renamed what the tooltip SAYS, because the glyph is a pencil but the
            // thing it does is freehand ink.
            "Freehand",
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
            // Neither is a tool: the dim slider is always-on, the crop control opens a session.
            TrayItem::DimSlider | TrayItem::Crop => None,
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
const TEXT_TRAY_GROUP: usize = 4;

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

/// What the top edit bar does with ONE control while a crop session is (or isn't) live —
/// **the single statement of the modal rule** (DRAGON-392, completed by DRAGON-410), read by the
/// builders and asserted by the tests, so a control added later cannot silently stay visible.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TrayItemState {
    /// Drawn and usable.
    Live,
    /// Drawn in the disabled tone, with no way to act (no `on_press` / no menu).
    ///
    /// **No caller produces this today** (DRAGON-410 turned the crop session's greyed controls
    /// into hidden ones), and it is kept on the owner's explicit instruction — "i like the
    /// ability to disable" — because the machinery behind it is real and wired: `tool_toggle_gated`
    /// draws a glyph in [`disabled_label_tone`] with no `on_press`, `dropdown_chip` recolours a
    /// chip's label AND caret together, and both builders take the enabled flag straight from this
    /// state. A future modal state that wants "present but unavailable" says so here and gets all
    /// of that for free. The lint allowance is the honest price of keeping an unused capability
    /// rather than deleting and re-deriving it.
    #[allow(dead_code)]
    Disabled,
    /// Not drawn at all — it gives up its slot.
    Hidden,
}

/// Every control FAMILY the TOP edit bar can draw — the domain the modal rule is stated over.
///
/// DRAGON-410 widened that domain from the declared tray to the WHOLE bar. Two of the bar's
/// controls are not [`TrayItem`] rows and never could be: the text SIZE + FONT dropdowns are
/// flyout chips INJECTED into the text cluster (see [`annotation_tools`]), and the share group
/// (Save / Save As / Copy / Delete) sits past the bar's split. A rule stated only over
/// `ANNOT_TRAY` could not reach either — which is exactly how the ticket's "sweep the whole top
/// bar, not just `ANNOT_TRAY`" gets satisfied by construction instead of by inspection.
///
/// Adding a control to the top bar means adding a variant here (or a tray row, which maps in
/// through [`TrayItem::top_bar`]); [`top_bar_item_state`] matches EXHAUSTIVELY with no wildcard
/// arm, so the new variant does not compile until a crop session's treatment of it is decided.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TopBarItem {
    /// The crop control — one button idle, the labelled accept + cancel pair mid-session.
    Crop,
    /// A tray tool button.
    Tool(Tool),
    /// The global dim ("spotlight range") slider and its glyph.
    DimSlider,
    /// The text SIZE + FONT dropdown chips injected into the text tool's cluster.
    TextStyle,
    /// The Save / Save As / Copy / Delete group at the far right of the bar.
    Share,
}

impl TrayItem {
    /// The top-bar family this declared tray row belongs to — the bridge that puts every tray row
    /// under the same rule as the two controls that are not tray rows.
    fn top_bar(&self) -> TopBarItem {
        match self {
            TrayItem::Tool(_, t, _, _) => TopBarItem::Tool(*t),
            TrayItem::DimSlider => TopBarItem::DimSlider,
            TrayItem::Crop => TopBarItem::Crop,
        }
    }
}

/// THE modal rule. Outside a crop session everything is [`TrayItemState::Live`]. Inside one, the
/// session owns the canvas and the top bar shows the CROP control — the accept / cancel pair, the
/// two actions the session is for — and **nothing else at all**.
///
/// DRAGON-410 made that literal. DRAGON-392 had greyed the tray and carved the DIM slider out of
/// the rule on the reasoning that dim is a viewing aid rather than an edit; the owner reversed
/// both. Greying leaves a bar full of dead controls competing with the two live ones, and the dim
/// carve-out was answering the wrong question — you cannot judge a crop through a dimmed image at
/// all, so the session forces the dim CLEAR for its duration
/// ([`super::edit::EditState::view_dim`]) and the slider has nothing left to do. Hiding it is the
/// same call as hiding the covermark (DRAGON-402): a session shows the picture the user is
/// framing, not the treatments queued over it.
fn top_bar_item_state(item: TopBarItem, session: bool) -> TrayItemState {
    if !session {
        return TrayItemState::Live;
    }
    // Exhaustive on purpose — see [`TopBarItem`].
    match item {
        TopBarItem::Crop => TrayItemState::Live,
        TopBarItem::Tool(_)
        | TopBarItem::DimSlider
        | TopBarItem::TextStyle
        | TopBarItem::Share => TrayItemState::Hidden,
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
    // A live crop session is MODAL, and since DRAGON-410 literally so: accept and cancel are the
    // only meaningful actions, so every other control on this bar is REMOVED for the duration.
    let cropping = e.crop_session.is_some();
    let groups: Vec<Element<'a, Msg>> = ANNOT_TRAY
        .iter()
        .enumerate()
        .filter_map(|(gi, group)| {
            let mut items: Vec<Element<'static, Msg>> = group
                .items
                .iter()
                // The modal rule, applied once per control (`top_bar_item_state`).
                .filter(|entry| {
                    top_bar_item_state(entry.top_bar(), cropping) != TrayItemState::Hidden
                })
                .map(|entry| match *entry {
                    TrayItem::Tool(icon, tool, name, action) => tb.tool_toggle_gated(
                        icon,
                        active == Some(tool),
                        top_bar_item_state(entry.top_bar(), cropping) == TrayItemState::Live,
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
                    // DRAGON-392: the crop control renders IN PLACE in its declared slot — one
                    // button idle, the accept/cancel PAIR while a session is live. It is the only
                    // tray item that can contribute two buttons, so it carries its own row at the
                    // cluster's own 2px spacing: nested or not, the buttons sit identically.
                    TrayItem::Crop => widget::row(tb.crop_items(cropping, km))
                    .spacing(2.0)
                    .align_y(Alignment::Center)
                    .into(),
                })
                .collect();
            // DRAGON-357 item 7: the text SIZE + FONT dropdowns live INSIDE the text tool's
            // cluster (one bordered group: icon + size dropdown + style dropdown), not beside it.
            // They are injected here rather than declared as tray rows (they are flyout chips,
            // not tool buttons), so they reach the modal rule through their OWN
            // [`TopBarItem`] — the state decides both whether they are drawn at all and, if they
            // are, whether they are live (the `Disabled` capability stays wired for a future
            // modal state that wants it).
            let text_state = top_bar_item_state(TopBarItem::TextStyle, cropping);
            if gi == TEXT_TRAY_GROUP && text_state != TrayItemState::Hidden {
                items.extend(text_style_controls(
                    preview,
                    tb,
                    text_state == TrayItemState::Live,
                ));
            }
            // A group whose every control hid contributes NOTHING — never an empty bordered
            // cluster, which is what a plain `map` would have drawn once the sweep emptied five
            // of the six groups. DRAGON-478's caption rides the SAME condition, so a caption can
            // never float over a group that is not there.
            (!items.is_empty()).then(|| tb.captioned(tb.tool_cluster(items), group.caption))
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

/// The shared menu CARD's own inner padding ([`Tb::menu_container`]): the inset between the
/// card's outline and whatever it holds, on all four sides.
///
/// Named in DRAGON-500 (it was a literal in the builder). A width handed to `menu_container` is
/// the OUTER card, so anything that has to line its edges up with a card's contents has to
/// subtract this twice, and the subtraction must not be a hand-copied number.
pub(super) const MENU_CARD_PAD: f32 = 4.0;

/// The fixed menu width (px) of the UPLOAD flyout (DRAGON-482). Wider than the text dropdowns
/// because its widest line is not a list entry but the "Automatically Share & Copy URL"
/// checkbox; the account rows (a brand mark plus a user-chosen label) sit comfortably inside
/// it, and a long label ellipsizes ([`upload_account_face`]) rather than stretching the panel
/// per document.
///
/// Doubled from its original 232px (DRAGON-490 follow-up): the owner found the narrower panel
/// cramped once real account labels (a user-typed name, not the short placeholder) were in
/// play. Written as the doubling itself, not the resulting number, so the relationship to the
/// original size stays visible at the definition.
///
/// Then cut by a QUARTER (DRAGON-495, owner's live review): the doubling overshot, and a
/// 464px panel hanging off a toolbar icon read as a window rather than a flyout. 348px still
/// clears every row in it — the checkbox line is the widest at roughly 250px of text, the
/// account rows are a 20px mark plus an ellipsizing label, and the visibility row is a short
/// label plus a 92px chip ([`UPLOAD_VISIBILITY_CHIP_W`]) — so nothing here clips; a long
/// account name ellipsizes as it always has.
const UPLOAD_MENU_W: f32 = 232.0 * 2.0 * 0.75;

/// The Upload panel's own extra inset INSIDE the shared menu card ([`MENU_CARD_PAD`]).
///
/// More breathing room than the card alone gives (DRAGON-489 follow-up, the owner's report that
/// the panel read as cramped), scoped to THIS panel rather than to `menu_container`, which the
/// plainer text-size / font dropdowns also wear and were not reported as cramped. Named in
/// DRAGON-500 because the account chip's width is now DERIVED from it: the two must move
/// together or the chip stops lining up with the rows under it.
const UPLOAD_PANEL_PAD: f32 = 6.0;

/// The width every item inside the Upload flyout gets: the card, less the card's own padding
/// and this panel's inset, on both sides. This is what a `Length::Fill` row (the checkbox, the
/// Upload button) actually measures, so anything given this width lines its edges up with them
/// and leaves ONE margin, `MENU_CARD_PAD + UPLOAD_PANEL_PAD`, against the flyout's edge on each
/// side (DRAGON-500).
const UPLOAD_CONTENT_W: f32 = UPLOAD_MENU_W - 2.0 * (MENU_CARD_PAD + UPLOAD_PANEL_PAD);

/// The fixed CLOSED-control width (px) of the Upload flyout's own NESTED account-picker chip
/// (DRAGON-489 follow-up), AND of the popup menu it opens (DRAGON-500).
///
/// It is exactly [`UPLOAD_CONTENT_W`]. It used to be `UPLOAD_MENU_W - 24`, four pixels short of
/// its column, so the chip's right edge stopped short of the Upload button's below it; and the
/// popup was sized to the full [`UPLOAD_MENU_W`], twenty pixels WIDER than the chip that opened
/// it. Both are the owner's DRAGON-500 report: the control did not sit on the panel's margins,
/// and its menu did not sit on the control.
///
/// **One constant for the control and the menu is also what aligns them.** The picker opens with
/// [`FlyoutDir::Down`], libcosmic's `popover::Position::Bottom`, which anchors the popup at the
/// control's bottom-CENTRE and then places it at `centre_x - popup_width / 2`. Equal widths make
/// that arithmetic the identity: the popup's left edge lands on the control's left edge and its
/// right on the right, with no offset to zero out and nothing to drift. (The popover clamps the
/// result into the viewport, which can still shift a menu opened hard against a screen edge —
/// that clamp is the one thing keeping a menu on screen at all, and it applies to the control's
/// own flyout equally.)
const UPLOAD_ACCOUNT_CHIP_W: f32 = UPLOAD_CONTENT_W;

// The popover ROUNDS the anchor's bottom-centre to a whole pixel before subtracting half the
// popup's width, so an odd or fractional width would re-introduce the half-pixel offset this
// pairing exists to remove. Pinned rather than left to whoever next retunes `UPLOAD_MENU_W`.
const _: () = assert!(
    UPLOAD_ACCOUNT_CHIP_W == (UPLOAD_ACCOUNT_CHIP_W as u32) as f32
        && (UPLOAD_ACCOUNT_CHIP_W as u32).is_multiple_of(2),
    "the account chip's width must be an even whole number of pixels, or its menu opens half a pixel off it"
);

/// The CLOSED account chip's height (px): roughly twice what its content asks for
/// (DRAGON-500, the owner's ask). Its natural height is about 22 — a 16px brand mark or a
/// 13px label's ~17px line, whichever is taller, plus the shared chip padding on each side —
/// which reads as a thin strip against the flyout's other rows now that it spans the full
/// column. The contents are CENTRED in the extra space rather than sitting at the top, which
/// is where a plain fixed height would leave them: a cosmic button lays its content out from
/// its padding, not from its middle.
///
/// The chip is the only control here that gets this. The visibility picker beside its own row
/// label ([`UPLOAD_VISIBILITY_CHIP_W`]) keeps the standard chip height, so the family a chip
/// belongs to is still recognisable.
const UPLOAD_ACCOUNT_CHIP_H: f32 = 44.0;

/// The fixed CLOSED-control width (px) of the Upload flyout's NESTED visibility-picker chip
/// (DRAGON-493). Unlike the account picker's chip, this one does not need to span the panel:
/// its labels are short ("Public" / "Unlisted" / "Private") and it sits beside its own
/// "Visibility" row label, the same shape [`TEXT_SIZE_CTRL_W`] gives the text-size chip beside
/// the text tool's own controls.
const UPLOAD_VISIBILITY_CHIP_W: f32 = 92.0;

/// The fixed menu width (px) of that same nested visibility picker's OPEN list: a small,
/// dedicated constant rather than reusing [`TEXT_DROPDOWN_MENU_W`], so retuning one dropdown's
/// size can never silently move the other's.
const UPLOAD_VISIBILITY_MENU_W: f32 = 110.0;

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
/// `enabled` (DRAGON-392 correction) gates the whole chip: no `on_press` — so the press can't
/// fire and the menu can't open — AND the CARET is recoloured to the disabled tone with it. The
/// caret is called out because a chip with dimmed text and a full-strength chevron is the obvious
/// half-disabled failure: the two must always move together. The LABEL is the caller's (it may
/// carry its own font/face), so the caller applies the same tone to it — [`disabled_label_tone`]
/// exists so there is one tone to apply, not two that can drift.
pub(super) fn dropdown_chip<'a>(
    pid: window::Id,
    label: Element<'a, Msg>,
    width: f32,
    toggle: PreviewMsg,
    enabled: bool,
) -> Element<'a, Msg> {
    dropdown_chip_tall(pid, label, width, None, toggle, enabled)
}

/// [`dropdown_chip`] with an optional FIXED height (DRAGON-500), for a chip that has to stand
/// up to the rows around it — today only the Upload flyout's account picker
/// ([`UPLOAD_ACCOUNT_CHIP_H`]). `None` is the natural height every other chip has always had,
/// byte-identical.
///
/// The height is applied to the button AND the content is wrapped to fill it, because a cosmic
/// button positions its content at its padding rather than centring it: setting the height
/// alone would give a tall chip with its mark and label stuck along the top edge.
pub(super) fn dropdown_chip_tall<'a>(
    pid: window::Id,
    label: Element<'a, Msg>,
    width: f32,
    height: Option<f32>,
    toggle: PreviewMsg,
    enabled: bool,
) -> Element<'a, Msg> {
    let caret = widget::icon::icon(crate::widgets::icons::handle("pan-down-symbolic")).size(10);
    // Enabled: the chip's own default caret colour (unchanged). Disabled: the shared quiet tone,
    // in lockstep with the label.
    let caret = if enabled {
        caret
    } else {
        caret.class(cosmic::theme::Svg::Custom(std::rc::Rc::new(|t: &cosmic::Theme| {
            cosmic::widget::svg::Style { color: Some(disabled_label_tone(t)) }
        })))
    };
    // KNOWN, and deliberately left alone (DRAGON-607). These un-classed containers are the
    // SAME trap that ticket fixed on the accent-filled buttons: a container overrides the ink
    // of everything below it, so the label never receives the `Button::Text` class's own
    // colour. Since libcosmic's `text_button.on` IS `accent_text`, that silently strips the
    // accent from these chip labels and they draw in the ordinary window foreground.
    //
    // It is not being changed here because it is not what was reported, the current look is
    // what these dropdown chips have always had, and restyling them unasked would be a visible
    // change to a surface nobody complained about. This note exists so the next person finds a
    // recorded decision instead of rediscovering it as a fresh bug. `theme::on_accent_content`
    // is the fix shape if it is ever wanted.
    let row = widget::row(vec![
        widget::container(label).width(Length::Fill).into(),
        caret.into(),
    ])
    .spacing(3.0)
    .align_y(Alignment::Center);
    // A fixed height gets the centring wrapper; the natural chip is left exactly as it was.
    let body: Element<'a, Msg> = match height {
        Some(_) => widget::container(row)
            .height(Length::Fill)
            .align_y(Alignment::Center)
            .into(),
        None => row.into(),
    };
    let btn = widget::button::custom(body)
        .class(cosmic::theme::Button::Text)
        .padding([2.0, 6.0])
        .width(Length::Fixed(width))
        .on_press_maybe(enabled.then_some(Msg::Preview(pid, toggle)));
    let btn = match height {
        Some(h) => btn.height(Length::Fixed(h)),
        None => btn,
    };
    crate::widgets::arrow_cursor::arrow_cursor(btn)
}

/// THE tone a DISABLED control's text/glyphs wear inside a toolbar cluster — one function so a
/// dropdown's label and its caret, and every disabled tray button, can never disagree.
///
/// It is the group's own BORDER colour (DRAGON-392 correction, superseding the small sliders'
/// glyph tone this used to share): a disabled control recedes to the faintest line the chrome
/// already draws, rather than sitting at the "quiet but present" strength that reads as usable.
/// [`quiet_on_cluster`] keeps its original user — the slider glyphs, which are quiet by nature,
/// not disabled — untouched.
pub(super) fn disabled_label_tone(t: &cosmic::Theme) -> cosmic::iced::Color {
    cluster_border(t)
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
fn text_style_controls(preview: &PreviewState, tb: Tb, enabled: bool) -> Vec<Element<'static, Msg>> {
    let e = &preview.edit;
    let pid = tb.pid;
    let size = e.text_size();
    let font = e.annot_text_font;
    // DRAGON-392 correction: a crop session disables these along with the rest of the tray —
    // LABEL and CARET together (`dropdown_chip` recolours the caret; the label is built here, so
    // it takes the same tone from the same function). A half-disabled chip — dimmed text, bright
    // chevron — is the failure this pairing exists to prevent.
    let label_class = |t: widget::Text<'static, cosmic::Theme>| {
        if enabled {
            t
        } else {
            t.class(cosmic::theme::Text::Custom(|th: &cosmic::Theme| {
                cosmic::iced::widget::text::Style {
                    color: Some(disabled_label_tone(th)),
                    ..Default::default()
                }
            }))
        }
    };
    // ── SIZE dropdown ("24px ⌄") ──────────────────────────────────────────────────────────
    let size_btn: Element<'static, Msg> = dropdown_chip(
        pid,
        label_class(widget::text(format!("{}px", size.round() as i32)).size(12)).into(),
        TEXT_SIZE_CTRL_W,
        PreviewMsg::ToggleTextSizeFlyout,
        enabled,
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
            label_class(
                widget::text(font.label())
                    .size(TEXT_CHIP_LABEL_SIZE)
                    .font(font_preview_face(font)),
            ),
        ),
        text_font_ctrl_w(),
        PreviewMsg::ToggleTextFontFlyout,
        enabled,
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

/// The Upload button's tooltip (DRAGON-482, third state DRAGON-514). Pure; unit-tested.
///
/// Three states, and the tooltip is the ONLY place a disabled one can explain itself. A
/// gated button in this group keeps its tip for exactly that reason (see
/// [`Tb::tool_button_gated`]). The shape matches [`action_tip`]'s in all three: name, two
/// spaces, a parenthetical. So a disabled tip reads as the same control wearing a different
/// answer, not as a different kind of label.
///
/// `busy` is an upload this document already has in flight (`edit::upload_in_flight`): the
/// trigger is one-at-a-time, and a button that simply went dead with no explanation would look
/// like the same "no accounts" dead as the state below it. It is checked SECOND because a
/// missing account is the more fundamental answer, and an account disconnected mid-transfer can
/// genuinely be in both.
///
/// `keybind` is the label for Upload's chord, so an enabled Upload advertises its key exactly
/// like Save and Copy beside it.
///
/// It used to be an `Option`, for a user who had unbound the action. DRAGON-617 BAKED Upload,
/// so there is no longer any way to unbind it and the `None` arm had become a state the app
/// cannot reach. Keeping it would have been a branch that reads as a supported case and is
/// really dead, so the parameter says what is now true: there is always a key.
pub(super) fn upload_tip(has_accounts: bool, busy: bool, keybind: &str) -> String {
    if !has_accounts {
        // Says what is MISSING, not that the feature is missing: DRAGON-467's wording
        // ("accounts are not available yet") was true when there was no feature at all, and
        // would now send a user looking for a build that has one instead of into Settings.
        return "Upload  (no cloud accounts yet)".to_string();
    }
    if busy {
        return "Upload  (an upload is already running)".to_string();
    }
    format!("Upload  ({keybind})")
}

/// One row's content (icon + label, the label tinted accent when `hot`) shared by the
/// account picker's CLOSED chip (whichever account is currently chosen) and its expanded
/// popup's row list (DRAGON-489 follow-up), so the two can never draw an account
/// differently. The provider's own mark makes the list scannable by brand rather than by
/// reading every label; an account whose provider THIS build does not know still gets a row
/// (`accounts::list` keeps it) and falls back to the generic cloud glyph.
///
/// The label is `Length::Fill` plus a single-line `Ellipsize::End` (DRAGON-490 follow-up): a
/// user-typed account name has no length limit, and before this it just overflowed the fixed
/// chip / menu width instead of stopping at it. `Length::Fill` on THIS row too, not just the
/// label, is what lets the fill actually reach the label: a row that stayed `Shrink` would
/// still size itself to the label's full natural width before the label's own fill had
/// anything to divide.
fn upload_account_face<'a>(acct: &'a crate::cloud::accounts::CloudAccount, hot: bool) -> Element<'a, Msg> {
    use cosmic::iced::advanced::text::{Ellipsize, EllipsizeHeightLimit, Wrapping};
    let icon =
        crate::widgets::icons::sized(acct.spec().map_or("cloud-symbolic", |p| p.icon), 16.0);
    let mut label = widget::text(acct.display_label())
        .size(TEXT_MENU_LABEL_SIZE)
        .width(Length::Fill)
        .wrapping(Wrapping::None)
        .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
    if hot {
        label = label.class(cosmic::theme::Text::Custom(|t| cosmic::iced::widget::text::Style {
            color: Some(crate::app::theme::accent(t)),
            ..Default::default()
        }));
    }
    widget::row(vec![icon.into(), label.into()])
        .width(Length::Fill)
        .spacing(8.0)
        .align_y(Alignment::Center)
        .into()
}

/// The Upload flyout's PANEL (DRAGON-482): the account picker, the per-account visibility
/// control (DRAGON-493), the share-link checkbox, and the button that starts the
/// transfer: the whole "where is this going, and how" question in one place, on the shared
/// opaque menu surface every editor dropdown wears.
///
/// The account picker is a NESTED `dropdown_chip` (DRAGON-489 follow-up), closed by default
/// and showing whichever account is currently chosen, matching the top bar's other dropdowns
/// (the text size / font chips) instead of the flat always-expanded list this used to be.
/// `menu_open` is that inner picker's own open state (`EditState::upload_account_menu_open`);
/// `selected` is the keyboard highlight, seeded to the preselected account when the OUTER
/// Upload flyout opens (see `edit::upload_preselect`), so the highlighted row IS the
/// destination and there is no second notion of "chosen" to fall out of step with it.
///
/// The visibility row is CAPABILITY-GATED on the SELECTED account's own provider
/// (`ProviderCaps::visibility`), never on its id: a future provider that adds the capability
/// gets the row for free, and switching the account picker to a provider without it hides the
/// row immediately, since it is recomputed from `selected` on every build of this panel.
///
/// A second, "Delete after N hours" row sat beside it from DRAGON-493 until DRAGON-505
/// removed the auto-delete feature; see `cloud/mod.rs`'s tombstone for why.
fn upload_panel<'a>(
    accounts: &'a [crate::cloud::accounts::CloudAccount],
    selected: Option<usize>,
    menu_open: bool,
    auto_share: bool,
    visibility_menu_open: bool,
    tb: Tb,
) -> Element<'a, Msg> {
    let pid = tb.pid;
    let selected_account = selected.and_then(|i| accounts.get(i));
    // The chip's face: whichever account is currently chosen. `selected` is `None` only when
    // there are no accounts, which is also the state where the Upload button itself is not
    // pressable (this panel is never reached), so the placeholder below is only ever a
    // defensive fallback, never a state a user can actually see.
    let chip_label: Element<'a, Msg> = match selected_account {
        Some(acct) => upload_account_face(acct, false),
        None => widget::text("Choose an account").size(TEXT_MENU_LABEL_SIZE).into(),
    };
    let chip = dropdown_chip_tall(
        pid,
        chip_label,
        UPLOAD_ACCOUNT_CHIP_W,
        Some(UPLOAD_ACCOUNT_CHIP_H),
        PreviewMsg::UploadAccountMenu(!menu_open),
        true,
    );
    let account_picker: Element<'a, Msg> = if menu_open {
        let rows: Vec<Element<'a, Msg>> = accounts
            .iter()
            .enumerate()
            .map(|(i, acct)| {
                crate::widgets::arrow_cursor::arrow_cursor(
                    widget::button::custom(upload_account_face(acct, selected == Some(i)))
                        .width(Length::Fill)
                        .class(cosmic::theme::Button::Text)
                        .on_press(Msg::Preview(
                            pid,
                            PreviewMsg::UploadAccountSelected(acct.id.clone()),
                        )),
                )
            })
            .collect();
        flyout(
            chip,
            // DRAGON-500: the menu is the CHIP's width, not the panel's, so the two read as one
            // column — see [`UPLOAD_ACCOUNT_CHIP_W`] for why equal widths are also what aligns
            // them under `Position::Bottom`. A long account label ellipsizes at the narrower
            // width exactly as it already did at the wider one (`upload_account_face`).
            tb.menu_container(widget::column(rows).spacing(2.0), UPLOAD_ACCOUNT_CHIP_W),
            FlyoutDir::Down,
            Msg::Preview(pid, PreviewMsg::UploadAccountMenu(false)),
        )
    } else {
        chip
    };
    let caps = selected_account.and_then(|a| a.spec()).map(|s| s.caps);
    let visibility_row = caps
        .is_some_and(|c| c.visibility)
        .then(|| upload_visibility_row(selected_account, visibility_menu_open, tb));
    // "Share & Copy URL" rather than "share link": what the option actually does on this side
    // is put the URL on the clipboard, and the upload child is what makes the link.
    let auto = crate::widgets::arrow_cursor::arrow_cursor(
        widget::checkbox(auto_share)
            .label("Automatically Share & Copy URL")
            .text_size(TEXT_MENU_LABEL_SIZE)
            .on_toggle(move |on| Msg::Preview(pid, PreviewMsg::UploadAutoShareToggled(on))),
    );
    // DRAGON-607: the centring container carries the on-accent ink rather than the default
    // class, which would overwrite it with the ambient window foreground. Without this the
    // label rendered near-white on the accent fill, while the crop tray's Apply Crop button
    // (a stock `button::suggested`, so nothing sits between it and its label) rendered the
    // correct dark ink. Two accent buttons in one chrome, two answers: that pair is the
    // owner's report. See `theme::on_accent_content` for the rule.
    let start = crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::custom(
            widget::container(widget::text("Upload").size(TEXT_MENU_LABEL_SIZE))
                .width(Length::Fill)
                .align_x(Alignment::Center)
                .class(crate::app::theme::button_content_class(
                    &cosmic::theme::Button::Suggested,
                )),
        )
        .width(Length::Fill)
        .class(cosmic::theme::Button::Suggested)
        .padding([8.0, 10.0])
        .on_press(Msg::Preview(pid, PreviewMsg::UploadStart)),
    );
    // More breathing room than the SHARED `menu_container`'s own `MENU_CARD_PAD` gives
    // (DRAGON-489 follow-up, owner's report the panel read as cramped): an extra inset plus
    // wider inter-item spacing, scoped to THIS panel alone rather than to `menu_container`
    // itself, which the plainer text-size/font dropdowns also wear and were not reported as
    // cramped. Every item here is then `UPLOAD_CONTENT_W` wide, the account chip included.
    let mut items: Vec<Element<'a, Msg>> =
        vec![account_picker, widget::divider::horizontal::default().into()];
    items.extend(visibility_row);
    items.push(auto);
    items.push(start);
    tb.menu_container(
        widget::column(items).spacing(10.0).padding(UPLOAD_PANEL_PAD),
        UPLOAD_MENU_W,
    )
}

/// The visibility PICKER row (DRAGON-493): a "Visibility" label plus a NESTED `dropdown_chip`
/// of Public / Unlisted / Private, the same nested-picker shape [`upload_panel`]'s own account
/// picker uses. Shown only for a provider whose `ProviderCaps::visibility` is true (today:
/// YouTube); `upload_panel` is what decides that, so this always has SOMETHING to draw.
fn upload_visibility_row<'a>(
    account: Option<&'a crate::cloud::accounts::CloudAccount>,
    menu_open: bool,
    tb: Tb,
) -> Element<'a, Msg> {
    use crate::cloud::accounts::Visibility;
    let pid = tb.pid;
    let current = account.and_then(|a| a.visibility).unwrap_or_else(Visibility::default_choice);
    let chip_label: Element<'a, Msg> =
        widget::text(visibility_label(current)).size(TEXT_MENU_LABEL_SIZE).into();
    let chip = dropdown_chip(
        pid,
        chip_label,
        UPLOAD_VISIBILITY_CHIP_W,
        PreviewMsg::UploadVisibilityMenu(!menu_open),
        true,
    );
    let picker: Element<'a, Msg> = if menu_open {
        let rows: Vec<Element<'a, Msg>> = [Visibility::Public, Visibility::Unlisted, Visibility::Private]
            .into_iter()
            .map(|v| {
                let hot = v == current;
                let mut label = widget::text(visibility_label(v)).size(TEXT_MENU_LABEL_SIZE);
                if hot {
                    label = label.class(cosmic::theme::Text::Custom(|t| {
                        cosmic::iced::widget::text::Style {
                            color: Some(crate::app::theme::accent(t)),
                            ..Default::default()
                        }
                    }));
                }
                crate::widgets::arrow_cursor::arrow_cursor(
                    widget::button::custom(label)
                        .width(Length::Fill)
                        .class(cosmic::theme::Button::Text)
                        .on_press(Msg::Preview(pid, PreviewMsg::UploadVisibilitySelected(v))),
                )
            })
            .collect();
        flyout(
            chip,
            tb.menu_container(widget::column(rows).spacing(2.0), UPLOAD_VISIBILITY_MENU_W),
            FlyoutDir::Down,
            Msg::Preview(pid, PreviewMsg::UploadVisibilityMenu(false)),
        )
    } else {
        chip
    };
    widget::row(vec![
        widget::text("Visibility").size(TEXT_MENU_LABEL_SIZE).width(Length::Fill).into(),
        picker,
    ])
    .align_y(Alignment::Center)
    .into()
}

/// [`crate::cloud::accounts::Visibility`]'s label in the picker. Pure.
fn visibility_label(v: crate::cloud::accounts::Visibility) -> &'static str {
    use crate::cloud::accounts::Visibility;
    match v {
        Visibility::Public => "Public",
        Visibility::Unlisted => "Unlisted",
        Visibility::Private => "Private",
    }
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
            annot_palette_panel(e.flyout_selected(), recents, km, tb),
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

/// The palette flyout (covermark-style panel background): a leading SWAP affordance (DRAGON-386
/// — the `X` companion-color swap, mirroring Photoshop's foreground/background swap), then the
/// ordered entry list from [`super::annotate::palette_entries`] — complement, accent | palette |
/// last-5 custom | "+" — with a separator between each group. The keyboard-highlighted index
/// (`sel`) shows a ring; each color swatch sets the annotation color, the "+" opens the custom
/// hex picker.
fn annot_palette_panel(
    sel: Option<usize>,
    recents: &[[u8; 4]],
    km: &crate::shortcuts::Keymap,
    tb: Tb,
) -> Element<'static, Msg> {
    use super::annotate::{PaletteEntry, PALETTE_COLOR_COUNT, PALETTE_LEAD};
    let entries = super::annotate::palette_entries(recents);
    // Separators AFTER the [complement, accent] group and AFTER the palette group.
    let sep_a = PALETTE_LEAD;
    let sep_b = PALETTE_LEAD + PALETTE_COLOR_COUNT;
    // The swap button leads the row (with its own separator), so the color-pair toggle sits
    // beside the accent/complement lead pair it swaps between — discoverable and out of the way.
    let mut items: Vec<Element<'static, Msg>> = vec![annot_swap_swatch(km, tb), annot_palette_sep()];
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

/// The companion-color SWAP button that leads the palette flyout (DRAGON-386): a 28px icon
/// swatch (matching the color swatches' footprint) whose arrow-swap glyph fires
/// [`PreviewMsg::AnnotColorCompanionSwap`] — the same toggle the `X` hotkey drives. Its tooltip
/// names the live `X` binding via [`action_tip`]. A plain `widget::tooltip` (not [`Tb::tip`]) so
/// it still shows while the flyout is open, since it draws on the frontmost popover.
fn annot_swap_swatch(km: &crate::shortcuts::Keymap, tb: Tb) -> Element<'static, Msg> {
    let glyph = crate::widgets::icons::sized("object-flip-horizontal-symbolic", 18.0);
    let btn = widget::button::custom(
        widget::container(glyph)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center),
    )
    .width(Length::Fixed(28.0))
    .height(Length::Fixed(28.0))
    .padding(0)
    .class(cosmic::theme::Button::Custom {
        // The same resting swatch surface the "+" opener wears (never keyboard-highlighted —
        // the swap is not part of the arrow-nav entry list), so it reads as one of the swatches.
        active: Box::new(|_f, t| plus_swatch_style(false, t)),
        hovered: Box::new(|_f, t| plus_swatch_style(false, t)),
        pressed: Box::new(|_f, t| plus_swatch_style(false, t)),
        disabled: Box::new(|t| plus_swatch_style(false, t)),
    })
    .on_press(Msg::Preview(tb.pid, PreviewMsg::AnnotColorCompanionSwap));
    let btn = crate::widgets::arrow_cursor::arrow_cursor(btn);
    widget::tooltip(
        btn,
        widget::text(action_tip("Swap color", crate::shortcuts::Action::PreviewColorCompanionSwap, km))
            .size(12),
        widget::tooltip::Position::Top,
    )
    .into()
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
    s.background = Some(Background::Color(cosmic.background(false).component.base.into()));
    s.text_color = Some(cosmic.background(false).component.on.into());
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
                false,
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
    ///
    /// DRAGON-393 re-anchored the toast by moving the fill/centre CONTAINER inside the stack (it
    /// is the base layer now, with the media inside it). A container carries no `tree::State` and
    /// adds NO level to `Tree::new`, so the shape these assertions describe is unchanged — which
    /// is exactly why the re-anchor was safe, and why this test is left as it was rather than
    /// adjusted to fit it.
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

    /// DRAGON-393: the toast ANCHOR must report the size it was ASKED for, never the size of
    /// what it wraps. A `stack` takes its dimensions from its first child, so a base layer that
    /// inherited a shrink-sized media is precisely what put an opening toast in the middle of the
    /// window on a small capture.
    #[test]
    fn the_toast_region_never_inherits_the_media_size() {
        use cosmic::iced::core::Length;
        // A deliberately SHRINK child — the small-capture case.
        let small = || -> Element<'static, Msg> { widget::text("tiny").into() };
        assert!(matches!(small().as_widget().size().width, Length::Shrink), "fixture must shrink");

        // Windowed: the whole area between the toolbars, both axes.
        let win = toast_region(small(), Length::Fill, Length::Fill);
        assert_eq!(win.as_widget().size().width, Length::Fill);
        assert_eq!(win.as_widget().size().height, Length::Fill);

        // Overlay: the hugged column's full width; the height is the media row's own.
        let overlay = toast_region(small(), Length::Fill, Length::Shrink);
        assert_eq!(overlay.as_widget().size().width, Length::Fill);

        // And a fixed-width anchor (the overlay's loading state, which pins the region to the
        // width the loaded column will take) reports exactly that width.
        let fixed = toast_region(small(), Length::Fixed(640.0), Length::Fill);
        assert_eq!(fixed.as_widget().size().width, Length::Fixed(640.0));
    }

    /// **The slider widths, and why they are no longer one number.** DRAGON-392 unified them;
    /// the zoom rail then diverged (see [`ZOOM_SLIDER_W`]) because DRAGON-400 gave it a 5× range
    /// to express where the icon-led sliders still express a bounded 0–100%. This pins the NEW
    /// rule — zoom wider, the other two matched — so the divergence reads as intentional to
    /// whoever finds it, and so re-unifying them has to be a deliberate act with a reason of its
    /// own rather than a tidy-up.
    #[test]
    fn the_zoom_rail_is_wider_than_the_matched_icon_led_sliders() {
        let shared = SLIDER_W * SLIDER_SCALE;
        assert_eq!(shared, 64.0, "the covermark + dim rails");
        // A third wider, to the nearest whole pixel — the owner's ask, stated as arithmetic so
        // the relationship survives a future retune of the shared width.
        assert_eq!(ZOOM_SLIDER_W, (shared * 1.33).round(), "the zoom rail is the shared rail +33%");
        assert!(ZOOM_SLIDER_W > shared);

        // …and the ICON-LED item width still describes the SHARED rail, not the zoom one. This
        // is the half that would actually break something: `slider_item_w` reserves bottom-bar
        // width for the covermark's two sliders, so pointing it at the zoom rail would over-
        // reserve for controls that never grew.
        let icon_and_gap = 2.0 * BTN_PAD + ICON_BOX * SLIDER_SCALE + 6.0;
        let rail_in_item = slider_item_w(1.0) - icon_and_gap;
        assert!((rail_in_item - shared).abs() < 1e-3, "the item carries the shared rail");
        assert!(
            (rail_in_item - ZOOM_SLIDER_W).abs() > 1.0,
            "the icon-led item must NOT have picked up the zoom rail's width"
        );
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
    ///
    /// Asserted on Copy since DRAGON-617 baked Save, which used to be the subject. Copy is the
    /// right replacement precisely because it is still bindable: this test exists to prove the
    /// resolver follows the keymap, so its subject has to be something the keymap still owns.
    #[test]
    fn action_tip_reads_live_key_and_omits_when_unbound() {
        use crate::shortcuts::{Action, Keymap, Shortcut, ShortcutKey};
        let mut km = Keymap::defaults();
        // Bound: two spaces then the keymap's own label in parens.
        let key = km.get(Action::PreviewCopy).unwrap().label();
        assert_eq!(
            action_tip("Copy to clipboard", Action::PreviewCopy, &km),
            format!("Copy to clipboard  ({key})")
        );
        // A rebind is reflected in the tip.
        km.set(
            Action::PreviewCopy,
            Shortcut { ctrl: true, alt: false, shift: false, logo: false, key: ShortcutKey::Char("q".into()) },
        );
        let rekey = km.get(Action::PreviewCopy).unwrap().label();
        assert_eq!(
            action_tip("Copy to clipboard", Action::PreviewCopy, &km),
            format!("Copy to clipboard  ({rekey})")
        );
        // Unbound: name only.
        km.unbind(Action::PreviewCopy);
        assert_eq!(action_tip("Copy to clipboard", Action::PreviewCopy, &km), "Copy to clipboard");
    }

    /// DRAGON-617: the five BAKED buttons wear the same tooltip SHAPE as their bindable
    /// neighbours, and they wear the same STRING they wore before the bake.
    ///
    /// The second half is what makes this worth a test. The bake was meant to change where a
    /// chord is matched and nothing a user can see, so the tooltips are the visible surface
    /// where a regression would show, and "it still says Ctrl+S" is the claim.
    #[test]
    fn the_baked_buttons_keep_the_tooltip_shape_and_the_string() {
        use crate::shortcuts::FixedEditorAction;
        for (name, action) in [
            ("Save", FixedEditorAction::Save),
            ("Upload", FixedEditorAction::Upload),
            ("Close", FixedEditorAction::Close),
            ("Undo", FixedEditorAction::Undo),
            ("Redo", FixedEditorAction::Redo),
        ] {
            let tip = fixed_key_tip(name, action.label());
            assert!(tip.starts_with(&format!("{name}  (")), "{tip} broke the tip shape");
            assert!(tip.ends_with(')'), "{tip} broke the tip shape");
            assert!(!tip.contains('\u{2014}'), "{tip} must stay em-dash free");
        }
        // The exact strings the pre-bake keymap produced, off macOS. Pinned literally,
        // because deriving them from the same `label()` the code uses would assert nothing.
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(fixed_key_tip("Save", FixedEditorAction::Save.label()), "Save  (Ctrl+S)");
            assert_eq!(fixed_key_tip("Close", FixedEditorAction::Close.label()), "Close  (Esc)");
            assert_eq!(fixed_key_tip("Undo", FixedEditorAction::Undo.label()), "Undo  (Ctrl+Z)");
            assert_eq!(
                fixed_key_tip("Redo", FixedEditorAction::Redo.label()),
                "Redo  (Ctrl+Shift+Z)"
            );
        }
    }

    // ── the declared annotation tray (DRAGON-340) ────────────────────────────────────

    /// Every tool in the tray, flattened in render order.
    fn tray_tools() -> Vec<Tool> {
        ANNOT_TRAY
            .iter()
            .flat_map(|g| g.items.iter())
            .filter_map(|i| match i {
                TrayItem::Tool(_, t, _, _) => Some(*t),
                TrayItem::DimSlider | TrayItem::Crop => None,
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
                Tool::Hand,
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
    /// this order; the dim slider rides the shape group (with spotlight) rather than standing alone.
    #[test]
    fn the_tray_groups_match_the_declared_layout() {
        let shape: Vec<usize> = ANNOT_TRAY.iter().map(|g| g.items.len()).collect();
        // NAVIGATION(3 = select/hand/crop, DRAGON-392), callouts(2), redact(2), SHAPES(5 =
        // highlight/border/border-highlight/spotlight + the dim slider, DRAGON-384 folded
        // spotlight and its slider into the shape slot), TEXT(1, DRAGON-354), pencil+eraser(2).
        assert_eq!(shape, vec![3, 2, 2, 5, 1, 2]);
        // A ONE-member group exists and is rendered through the very same container as the
        // others — there is no single-member branch anywhere in the builder.
        assert!(shape.contains(&1));
        // The dim slider is the LAST item of the shape group, riding it after the four tools.
        assert!(matches!(ANNOT_TRAY[3].items[4], TrayItem::DimSlider));
        // The size dropdown is injected right after the TEXT group, which must be the group
        // whose sole tool is Text.
        assert!(matches!(
            ANNOT_TRAY[TEXT_TRAY_GROUP].items,
            [TrayItem::Tool(_, Tool::Text, _, _)]
        ));
    }

    /// DRAGON-392's group, spelled out: Crop, Select and Hand are ONE group, IN THAT ORDER (the
    /// owner's final call, which supersedes the ticket body's "Select, Hand, Crop"), and the crop
    /// control is the tray's only non-`Tool` item. Pinned here because the order is what makes
    /// the crop session redraw as `accept | reject | Hand` with the Hand never moving.
    #[test]
    fn the_navigation_group_is_crop_then_select_then_hand() {
        assert!(matches!(
            ANNOT_TRAY[0].items,
            [
                TrayItem::Crop,
                TrayItem::Tool(_, Tool::Pointer, "Select", _),
                TrayItem::Tool(_, Tool::Hand, "Hand", _),
            ]
        ));
        // Crop appears exactly once, and nowhere else in the tray.
        let crops = ANNOT_TRAY
            .iter()
            .flat_map(|g| g.items.iter())
            .filter(|i| matches!(i, TrayItem::Crop))
            .count();
        assert_eq!(crops, 1);
    }

    /// A `Tb` at scale 1 for the pure-arithmetic tests below.
    fn test_tb() -> Tb {
        Tb {
            pid: cosmic::iced::window::Id::unique(),
            scale: 1.0,
            glass: None,
            suppress_tooltips: false,
            labels: true,
        }
    }

    /// How many BUTTONS a tray group actually draws for a given crop state. The crop item is
    /// measured by BUILDING it ([`Tb::crop_items`]) rather than by a parallel arity table, so the
    /// invariants below are asserted against the real builder and can't be satisfied by a stale
    /// count. Sliders and the text dropdowns aren't buttons; a hidden control contributes nothing.
    fn tray_group_slots(group: &TrayGroup, session: bool) -> usize {
        let tb = test_tb();
        let km = crate::shortcuts::Keymap::defaults();
        group
            .items
            .iter()
            .filter(|i| top_bar_item_state(i.top_bar(), session) != TrayItemState::Hidden)
            .map(|i| match i {
                TrayItem::Tool(..) => 1,
                TrayItem::Crop => tb.crop_items(session, &km).len(),
                TrayItem::DimSlider => 0,
            })
            .sum()
    }

    /// DRAGON-410 — **the sweep, measured against the real builder**: with a session live the tray
    /// draws the crop group's accept + cancel PAIR and not one button anywhere else. DRAGON-392's
    /// footprint invariant (the group kept three slots in both states, SELECT hiding to make room
    /// for the pair) is deliberately gone: nothing is left beside the pair for a width change to
    /// shove sideways.
    #[test]
    fn a_crop_session_draws_only_the_crop_pair() {
        assert_eq!(tray_group_slots(&ANNOT_TRAY[0], false), 3, "idle: Crop | Select | Hand");
        assert_eq!(tray_group_slots(&ANNOT_TRAY[0], true), 2, "session: accept | cancel");
        for (i, g) in ANNOT_TRAY.iter().enumerate().skip(1) {
            assert!(tray_group_slots(g, false) > 0, "group {i} draws when idle");
            assert_eq!(tray_group_slots(g, true), 0, "group {i} must vanish mid-crop");
        }
        // …so the whole tray is exactly the pair.
        let total: usize = ANNOT_TRAY.iter().map(|g| tray_group_slots(g, true)).sum();
        assert_eq!(total, 2, "the mid-crop top bar is the crop pair and nothing else");
    }

    /// A group that hides EVERY one of its controls must contribute no cluster at all — an empty
    /// bordered capsule would be five stray boxes across the mid-crop bar. Asserted on the same
    /// emptiness test the builder branches on.
    #[test]
    fn a_fully_hidden_group_contributes_no_cluster() {
        let empty = |g: &TrayGroup, session: bool| {
            g.items
                .iter()
                .all(|i| top_bar_item_state(i.top_bar(), session) == TrayItemState::Hidden)
        };
        for (i, g) in ANNOT_TRAY.iter().enumerate() {
            assert!(!empty(g, false), "group {i} must draw when idle");
            // The text group's cluster survives only via its injected chips, which hide too.
            let injected_alive = i == TEXT_TRAY_GROUP
                && top_bar_item_state(TopBarItem::TextStyle, true) != TrayItemState::Hidden;
            assert_eq!(
                empty(g, true) && !injected_alive,
                i != 0,
                "group {i}: only the crop group survives a session"
            );
        }
    }

    /// DRAGON-392 correction: the DISABLED tone must BE the cluster's border colour — the owner
    /// asked for exactly that match, and matching it by eye would drift the moment the theme
    /// moves. Both read [`cluster_border`], so this asserts they still do.
    ///
    /// It also pins the tone as VISIBLE in both themes, because a border colour is chosen to read
    /// as a hairline and is being reused as a glyph FILL here. Measured at the time of writing
    /// (luminance): dark `0.266` on a `0.182` cluster fill — a 0.084 gap; light `0.689` on
    /// `0.960` — a 0.272 gap. Dark is the faint case by design, and a 22px glyph carries that
    /// contrast far better than the 1px border the owner pointed at does. The floor below is
    /// deliberately low (it guards "vanishes entirely", not "is subtle") — if a theme change
    /// pushes the tone onto the fill, this fails instead of silently rendering nothing.
    #[test]
    fn the_disabled_tone_is_the_cluster_border_and_stays_visible_in_both_themes() {
        let lum = |c: cosmic::iced::Color| 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
        for (name, theme) in [
            ("dark", cosmic::theme::Theme::dark()),
            ("light", cosmic::theme::Theme::light()),
        ] {
            let tone = disabled_label_tone(&theme);
            let border = cluster_border(&theme);
            assert_eq!(
                (tone.r, tone.g, tone.b),
                (border.r, border.g, border.b),
                "{name}: the disabled tone must be the group's border colour"
            );
            // Opaque, so the SVG rasterizer's alpha-discarding tint path renders it faithfully
            // (see `cluster_border`). A translucent token here would silently paint full-strength.
            assert!((tone.a - 1.0).abs() < f32::EPSILON, "{name}: the tone must be opaque");
            let fill: cosmic::iced::Color = theme.cosmic().background(false).component.base.into();
            let gap = (lum(tone) - lum(fill)).abs();
            assert!(gap > 0.05, "{name}: the disabled glyph vanished into the cluster fill ({gap})");
        }
    }

    /// **DRAGON-516: the upload track must RECEDE, in both themes.**
    ///
    /// The owner's report was that the track read as a bright grey bar. The cause was a
    /// category error rather than a bad shade: DRAGON-514 painted it with `theme::subtle`,
    /// which is built by blending the ON-BACKGROUND colour toward the background, so in a dark
    /// theme it lands NEAR THE FOREGROUND. A 5px unfilled bar wants the opposite.
    ///
    /// So this asserts the property, not the token: the track must sit nearer the surface it is
    /// drawn on than to that surface's text colour, in dark AND in light. The second assertion
    /// is what makes this a regression test rather than a restatement — it fails for `subtle`,
    /// the exact value that shipped.
    #[test]
    fn the_upload_track_recedes_into_the_header_in_both_themes() {
        let lum = |c: cosmic::iced::Color| 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
        for (name, theme) in
            [("dark", cosmic::theme::Theme::dark()), ("light", cosmic::theme::Theme::light())]
        {
            let track = upload_track_tone(&theme);
            // The meter pill's own fill: the surface this bar is drawn on.
            let surface: cosmic::iced::Color = theme.cosmic().background(false).component.base.into();
            let text = crate::app::theme::foreground(&theme);
            let to_surface = (lum(track) - lum(surface)).abs();
            let to_text = (lum(track) - lum(text)).abs();
            assert!(
                to_surface < to_text,
                "{name}: the track sits closer to the text colour than to its own background \
                 ({to_surface} vs {to_text}) — it will read as a bright bar, not as a channel"
            );
            // The token that shipped and was reported: it fails the rule above in the dark
            // theme, which is what this test exists to keep out.
            let subtle = crate::app::theme::subtle(&theme);
            assert!(
                to_surface < (lum(subtle) - lum(surface)).abs(),
                "{name}: the track must recede further than `subtle`, the text token it replaced"
            );
            // Still VISIBLE, so this does not fix "too bright" by producing "invisible" — the
            // narrow track was already reported as barely there once (DRAGON-490 follow-up).
            assert!(to_surface > 0.02, "{name}: the track vanished into the pill ({to_surface})");
            // Opaque, so the windowed titlebar and the overlay header (different surfaces
            // underneath) composite it identically.
            assert!((track.a - 1.0).abs() < f32::EPSILON, "{name}: the track must be opaque");
        }
    }

    /// **DRAGON-516: a finished upload keeps its colour until the next one starts.**
    ///
    /// The whole tint table in one place. The row that changed is the settled success: DRAGON-514
    /// faded it to `theme::subdued` so a persistent meter would stop "notifying", and the owner
    /// overruled that after seeing it — a faded bar reads as the upload having been forgotten,
    /// not as it having quietly succeeded.
    ///
    /// Asserted on the RESOLVED colours under both real themes rather than on the function
    /// pointers, which Rust does not promise to compare meaningfully and which would also say
    /// nothing about what a viewer actually sees.
    #[test]
    fn a_settled_upload_keeps_the_face_it_announced() {
        use super::edit::{MeterAction, MeterFace, UploadOutcome};
        let rgb = |c: cosmic::iced::Color| (c.r, c.g, c.b, c.a);
        for (name, theme) in
            [("dark", cosmic::theme::Theme::dark()), ("light", cosmic::theme::Theme::light())]
        {
            let tint = |face| rgb(meter_tint(face)(&theme));
            let (success, danger, accent, subdued) = (
                rgb(crate::app::theme::success(&theme)),
                rgb(crate::app::theme::danger(&theme)),
                rgb(crate::app::theme::accent(&theme)),
                rgb(crate::app::theme::subdued(&theme)),
            );
            // Success: green while announcing AND after settling, the same colour for both.
            assert_eq!(tint(MeterFace::Finished(UploadOutcome::Done)), success, "{name}");
            assert_eq!(
                tint(MeterFace::Settled(UploadOutcome::Done)),
                success,
                "{name}: a settled success must stay green (DRAGON-516)"
            );
            assert_ne!(
                tint(MeterFace::Settled(UploadOutcome::Done)),
                subdued,
                "{name}: the DRAGON-514 fade is what the owner overruled"
            );
            // Failure: danger in both, which DRAGON-514 already had right.
            for stopped in [
                MeterFace::Finished(UploadOutcome::Stopped),
                MeterFace::Settled(UploadOutcome::Stopped),
            ] {
                assert_eq!(tint(stopped), danger, "{name} {stopped:?}");
            }
            // A pressed X is red at once, whichever action it was.
            for action in [MeterAction::Cancel, MeterAction::Undo] {
                assert_eq!(tint(MeterFace::Ending(action)), danger, "{name} {action:?}");
            }
            // Running is the accent.
            for running in [MeterFace::Progress(0), MeterFace::Progress(99)] {
                assert_eq!(tint(running), accent, "{name} {running:?}");
            }
            // The three states really are three different colours, so this is not passing by
            // everything collapsing to one token.
            assert_ne!(success, danger, "{name}");
            assert_ne!(success, accent, "{name}");
        }
        // And the LENGTH does not change either: a settled bar stays full, so the meter is a
        // finished bar in the outcome's colour and nothing about it decays.
        for outcome in [UploadOutcome::Done, UploadOutcome::Stopped] {
            let announced = super::edit::meter_fill(MeterFace::Finished(outcome));
            let settled = super::edit::meter_fill(MeterFace::Settled(outcome));
            assert!((settled - 1.0).abs() < f32::EPSILON, "{outcome:?} settled at {settled}");
            assert!((settled - announced).abs() < f32::EPSILON);
        }
    }

    /// DRAGON-467: the Share BUTTON and the Share ACTION read ONE capability, so the group can
    /// never offer something the platform will refuse.
    ///
    /// A headless test cannot look at the toolbar, so this drives the real seam rather than a
    /// hardcoded expectation: whatever `share_available()` says on the host running the suite,
    /// `share_file` must agree with it. That is what makes the test survive a platform arm
    /// landing later — it starts exercising the OTHER branch on its own.
    #[test]
    fn the_share_button_and_the_share_action_read_one_capability() {
        let path = std::path::Path::new("/shots/a.png");
        match crate::platform::services::share_available() {
            // No sheet: the button is not built at all (see `share_group`), and the action
            // refuses with a sentence rather than silently doing nothing.
            false => {
                let err = crate::platform::services::share_file(path, false)
                    .expect_err("a system with no share sheet must refuse the action");
                assert!(err.contains("Sharing"), "the reason names the action: {err:?}");
            }
            // A sheet exists: the action must not be the permanent-refusal stub any more.
            true => {
                let err = crate::platform::services::share_file(path, false).err();
                assert!(
                    !err.is_some_and(|e| e.contains("isn't available on this system")),
                    "a platform that advertises a share sheet must have a real body"
                );
            }
        }
    }

    /// EVERY top-bar control family, enumerated — the tray's own rows plus the two that are not
    /// tray rows.
    ///
    /// The `witness` below is a TOTAL function over [`TopBarItem`] written without a wildcard
    /// arm: a variant added to the enum stops this file compiling until it is named here AND
    /// listed in the vector, so a control added to the top bar cannot skip the sweep test.
    fn all_top_bar_items() -> Vec<TopBarItem> {
        fn witness(i: TopBarItem) -> &'static str {
            match i {
                TopBarItem::Crop => "crop",
                TopBarItem::Tool(_) => "tool",
                TopBarItem::DimSlider => "dim slider",
                TopBarItem::TextStyle => "text size/font chips",
                TopBarItem::Share => "save / copy / share / upload / delete",
            }
        }
        let mut all = vec![
            TopBarItem::Crop,
            TopBarItem::DimSlider,
            TopBarItem::TextStyle,
            TopBarItem::Share,
        ];
        all.extend(tray_tools().into_iter().map(TopBarItem::Tool));
        // Every listed item resolves through the witness (and so through the enum's own arms).
        assert!(all.iter().all(|i| !witness(*i).is_empty()));
        all
    }

    /// **THE live set during a crop session, enumerated** (DRAGON-410). The session owns the
    /// canvas, so the top bar shows the CROP control — accept + cancel — and nothing else at all.
    /// DRAGON-392's two survivors are both gone: the greyed tools are hidden outright, and the DIM
    /// slider's carve-out was reversed (the session forces the dim clear, so there is nothing for
    /// the slider to do — see `EditState::view_dim`).
    ///
    /// Stated over EVERY top-bar family rather than over `ANNOT_TRAY` alone, because the text
    /// chips and the share group are not tray rows and a tray-only rule could never have swept
    /// them.
    #[test]
    fn a_crop_session_leaves_only_the_crop_control_live() {
        for item in all_top_bar_items() {
            let want = if item == TopBarItem::Crop {
                TrayItemState::Live
            } else {
                TrayItemState::Hidden
            };
            assert_eq!(top_bar_item_state(item, true), want, "mid-crop: {item:?}");
            // Outside a session everything is live again — the rule is a session rule only.
            assert_eq!(top_bar_item_state(item, false), TrayItemState::Live, "idle: {item:?}");
        }
        // Every DECLARED tray row maps into that domain, so none of them is judged by a second
        // rule of its own.
        for g in ANNOT_TRAY {
            for i in g.items {
                assert!(
                    all_top_bar_items().contains(&i.top_bar()),
                    "a tray row maps outside the enumerated top bar"
                );
            }
        }
    }

    /// `Disabled` is KEPT (the owner: "i like the ability to disable") even though the crop
    /// session no longer uses it — and it must stay a distinct third state, not quietly become an
    /// alias for one of the other two, or the capability is gone in all but name.
    #[test]
    fn the_disabled_state_is_still_a_distinct_third_state() {
        assert_ne!(TrayItemState::Disabled, TrayItemState::Live);
        assert_ne!(TrayItemState::Disabled, TrayItemState::Hidden);
        // Both builders take their enabled flag as `state == Live` and their drawn-at-all test as
        // `state != Hidden`. Replayed here so the three states still mean three different
        // renderings: DRAWN and usable, DRAWN and inert, and not drawn.
        let drawn = |s: TrayItemState| s != TrayItemState::Hidden;
        let enabled = |s: TrayItemState| s == TrayItemState::Live;
        assert_eq!((drawn(TrayItemState::Live), enabled(TrayItemState::Live)), (true, true));
        assert_eq!(
            (drawn(TrayItemState::Disabled), enabled(TrayItemState::Disabled)),
            (true, false),
            "disabled must still mean DRAWN but inert — not hidden, not usable"
        );
        assert_eq!((drawn(TrayItemState::Hidden), enabled(TrayItemState::Hidden)), (false, false));
    }

    /// The crop control's own arity: one button idle, the accept + cancel pair mid-session — and
    /// accept is the LABELLED one (DRAGON-410).
    #[test]
    fn the_crop_control_is_one_button_idle_and_a_labelled_pair_mid_session() {
        let tb = test_tb();
        let km = crate::shortcuts::Keymap::defaults();
        assert_eq!(tb.crop_items(false, &km).len(), 1);
        assert_eq!(tb.crop_items(true, &km).len(), 2);
        assert_eq!(CROP_ACCEPT_LABEL, "Apply Crop");
    }

    /// DRAGON-410: the labelled accept button WIDENS the crop group, so it must still fit the
    /// narrowest preview the editor ever opens (`PREVIEW_MIN_W`, the DRAGON-392 floor).
    ///
    /// Estimated the same way `about.rs` sizes its fixed-width action buttons — a generous
    /// per-character advance at the button's own font size — plus libcosmic's OWN metrics for the
    /// parts we do not choose (`space_s` padding, `space_xxxs` icon-to-label gap, the 16px icon),
    /// read off the theme rather than copied, so a theme change moves the estimate with it.
    ///
    /// The margin is enormous, and that is the finding: mid-crop this bar carries the crop cluster
    /// ALONE (the share group hides with everything else), where the idle bar's own floor already
    /// reserves a four-button share group. Labelling accept cannot be what overflows the bar.
    #[test]
    fn the_labelled_crop_pair_fits_the_minimum_preview_width() {
        /// A comfortable over-estimate of one character's advance at the button's 14px label
        /// (`about.rs` uses 7.5 at the same size; rounding up keeps this a ceiling).
        const CHAR_W: f32 = 8.0;
        let theme = cosmic::theme::Theme::dark();
        let c = theme.cosmic();
        let s = PreviewSurface::Overlay.btn_scale();
        // ACCEPT: libcosmic's `button::suggested` — h-padding `space_s` each side, a 16px leading
        // icon, `space_xxxs` between icon and label, then the label.
        let accept = 2.0 * f32::from(c.space_s())
            + 16.0
            + f32::from(c.space_xxxs())
            + CROP_ACCEPT_LABEL.chars().count() as f32 * CHAR_W;
        // CANCEL: an ordinary scaled toolbar icon button.
        let cancel = s * (ICON_BOX + 2.0 * BTN_PAD);
        // The cluster: group padding both sides + the pair + the 2px inter-button spacing.
        let cluster = 2.0 * s * GROUP_PAD + accept + cancel + 2.0;
        // The bar: the cluster, the row gap to the split, and the split's minimum gap.
        let bar = cluster + TOOLBAR_ROW_GAP + SPLIT_MIN_GAP;
        assert!(
            bar <= crate::app::shell::PREVIEW_MIN_W,
            "the mid-crop top bar ({bar}px) must fit PREVIEW_MIN_W ({}px)",
            crate::app::shell::PREVIEW_MIN_W
        );
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

    /// DRAGON-392 correction: a crop session DISARMS the tray (nothing may look armed while the
    /// session owns the canvas) and re-arms on exit. That must not disturb the cycle slots — the
    /// next `M`/`U` press has to behave exactly as it would have without the crop.
    ///
    /// It holds because the disarm moves no cursor (`set_annot_tool(None)` skips the insert) and
    /// arm-then-advance reads the CURSOR when nothing is armed: with the tool put back, the slot
    /// is armed again and advances from it; with nothing armed, the slot re-arms its remembered
    /// member rather than restarting at the first.
    #[test]
    fn disarming_for_a_crop_session_leaves_the_cycle_slots_where_they_were() {
        use crate::shortcuts::Action;
        let members = slot_tools(Action::PreviewAnnotRedactCycle);
        // The user had armed the slot's SECOND member, then a crop session disarmed everything.
        assert_eq!(next_slot_tool(&members, None, Some(Tool::Blur)), Some(Tool::Blur));
        // With the tool handed back, the key advances from it exactly as before the session.
        assert_eq!(next_slot_tool(&members, Some(Tool::Blur), Some(Tool::Blur)), members.first().copied());
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
            vec![Tool::Highlight, Tool::Rect, Tool::BoxHighlight, Tool::Spotlight]
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
        assert!(slot_tools(Action::PreviewCopy).is_empty());
        // Solo tools belong to no slot, so their key can never be stolen by a cycle. Spotlight is
        // NOT among them any more — DRAGON-384 moved it into the shape slot.
        for solo in [Tool::Pointer, Tool::Arrow, Tool::Badge, Tool::Text, Tool::Pen, Tool::Eraser] {
            assert_eq!(slot_for_tool(solo), None, "{solo:?}");
        }
    }

    /// Arm-then-advance, the whole behaviour: a cold press arms the first member, a repeat
    /// advances and wraps, leaving the slot and coming back re-arms the member you left on
    /// (the cursor) rather than restarting at member one.
    #[test]
    fn a_slot_key_arms_then_advances_and_wraps() {
        // The real shape chain since DRAGON-384: spotlight is the last member before the wrap.
        let shape = [Tool::Highlight, Tool::Rect, Tool::BoxHighlight, Tool::Spotlight];
        // Cold: nothing armed, no cursor → member 1.
        assert_eq!(next_slot_tool(&shape, None, None), Some(Tool::Highlight));
        // Armed → advance, and the last member wraps to the first.
        assert_eq!(next_slot_tool(&shape, Some(Tool::Highlight), Some(Tool::Highlight)), Some(Tool::Rect));
        assert_eq!(next_slot_tool(&shape, Some(Tool::Rect), Some(Tool::Rect)), Some(Tool::BoxHighlight));
        assert_eq!(
            next_slot_tool(&shape, Some(Tool::BoxHighlight), Some(Tool::BoxHighlight)),
            Some(Tool::Spotlight)
        );
        assert_eq!(
            next_slot_tool(&shape, Some(Tool::Spotlight), Some(Tool::Spotlight)),
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

/// DRAGON-478: the top bar's GROUP CAPTIONS — the declared words, and the height band they cost.
#[cfg(test)]
mod caption_tests {
    use super::*;

    /// The owner's caption list, in TRAY ORDER. Spelled out here rather than derived from
    /// `ANNOT_TRAY` (which would only prove the list equals itself): this is the review-approved
    /// copy, so a tray regroup that silently re-labels a family fails here.
    ///
    /// Two of these deliberately do NOT match their group's internal name — "Shape" captions the
    /// Arrow / Step-Marker group while the SHAPE cycle slot is the one captioned "Box Highlight".
    /// That is the owner's mapping, matched to the tools each cluster actually holds; see the
    /// note on the shape group in [`ANNOT_TRAY`].
    const EXPECTED: [&str; 6] = ["Canvas", "Shape", "Redact", "Box Highlight", "Text", "Draw"];

    #[test]
    fn every_tray_group_carries_its_declared_caption() {
        assert_eq!(ANNOT_TRAY.len(), EXPECTED.len(), "a tray group was added or removed");
        for (i, (g, want)) in ANNOT_TRAY.iter().zip(EXPECTED).enumerate() {
            assert_eq!(g.caption, want, "tray group {i}");
            assert!(!g.caption.is_empty(), "tray group {i} must be captioned");
        }
        // The one top-bar cluster that is not a tray row carries its own.
        assert_eq!(SHARE_GROUP_CAPTION, "File Actions");
    }

    /// The captions name what the FAMILY is for; the tool tooltips name the individual tools. In
    /// a MULTI-tool group a caption repeating one member's name would read as a label for that
    /// one button rather than for the row (which is why the highlight family is not captioned
    /// "Highlight"). A group holding exactly ONE tool is exempt by definition — naming the family
    /// and naming the tool are the same fact there, and the Text group is the case.
    ///
    /// This also pins the DRAGON-478 rename, since "Draw" now sits over "Freehand".
    #[test]
    fn a_multi_tool_caption_never_repeats_one_of_its_own_members() {
        for (i, g) in ANNOT_TRAY.iter().enumerate() {
            let tools = g
                .items
                .iter()
                .filter(|i| matches!(i, TrayItem::Tool(..)))
                .count();
            if tools < 2 {
                continue;
            }
            for item in g.items {
                if let TrayItem::Tool(_, _, name, _) = item {
                    assert_ne!(*name, g.caption, "group {i}: caption repeats its tool's name");
                }
            }
        }
        // The pencil's DISPLAY copy (its `Tool`/`Action`/keymap names are untouched).
        let pen = ANNOT_TRAY
            .iter()
            .flat_map(|g| g.items.iter())
            .find_map(|i| match i {
                TrayItem::Tool(_, Tool::Pen, name, _) => Some(*name),
                _ => None,
            });
        assert_eq!(pen, Some("Freehand"));
    }

    /// The band is the gap plus the caption's PINNED line box, scaled — and it is a literal zero
    /// when the labels are off, which is what makes `PreviewSurface::chrome_h(false)` the
    /// historical number rather than a near miss.
    #[test]
    fn the_caption_band_is_the_gap_plus_the_line_box_and_zero_when_off() {
        for scale in [1.0f32, 0.82, 0.5, 2.0] {
            assert_eq!(caption_band_h(false, scale), 0.0, "off must cost nothing at {scale}");
            assert_eq!(caption_band_h(true, scale), scale * (CAPTION_GAP + CAPTION_LINE_H));
        }
        // Monotonic in the scale, and never zero while on — a band that rounded to nothing
        // would draw a caption into no reserved space at all.
        assert!(caption_band_h(true, super::super::surface::CHROME_SCALE) > 0.0);
    }

    /// The caption band is symmetric by construction: the padding UNDER the word (what the
    /// top bar draws at its bottom edge with the labels on) is the same scaled value as the
    /// gap ABOVE it, so retuning [`CAPTION_GAP`] can never re-open the top-heavy look the
    /// owner's screenshot showed. Labels off is the historical bar padding, untouched.
    #[test]
    fn the_pad_under_a_caption_equals_the_gap_above_it() {
        for scale in [1.0f32, 0.82, 0.5] {
            assert_eq!(top_bar_bottom_pad(true, scale), CAPTION_GAP * scale);
        }
        assert_eq!(top_bar_bottom_pad(false, 0.82), BAR_V_PAD);
    }

    /// The caption's MECHANICS, deliberately not its size. The size is an owner-tuned dial
    /// that has moved three times on sight (10 to 20 to 13.4), and the first two rounds each
    /// broke a test that had pinned the then-current value against the other label sizes, so
    /// no relative-size claim belongs here. What holds through every retune: the 8px floor,
    /// whole-pixel rasterization, and a line box that clears the word it reserves space for.
    #[test]
    fn the_caption_mechanics_hold_at_any_owner_tuned_size() {
        let rendered = |scale: f32| (CAPTION_TEXT_SIZE * scale).round().max(8.0);
        assert_eq!(rendered(0.1), 8.0, "the floor holds at any scale");
        for scale in [1.0f32, 0.82, 0.5] {
            // Whole pixels, so the text rasterizes at the size it draws at.
            assert_eq!(rendered(scale).fract(), 0.0, "scale {scale} left a fractional size");
            // The reserved line box always clears the glyphs it holds, at the same scale
            // arithmetic `caption_band_h` uses, so the band can never clip its own word.
            assert!(
                CAPTION_LINE_H * scale >= rendered(scale) * 0.9,
                "scale {scale}: the line box no longer clears the floored glyph size"
            );
        }
    }
}

#[cfg(test)]
mod upload_tip_tests {
    use super::*;
    use rstest::rstest;

    /// Every state the Upload tooltip has, one case each: with and without accounts, and
    /// (DRAGON-514) already busy. A table because they are one rule seen from several sides,
    /// and a loop would report "one of four" instead of naming the state that broke.
    ///
    /// The two UNBOUND cases are gone (DRAGON-617): Upload is baked, so there is no longer a
    /// way to unbind it and those rows described a state the app cannot reach.
    #[rstest]
    #[case::live(true, false, "Ctrl+U", "Upload  (Ctrl+U)")]
    // With NO accounts the tip is the only explanation the disabled button has, so it names
    // what is MISSING rather than declaring the feature unavailable, and it never advertises
    // a key that would do nothing.
    #[case::gated(false, false, "Ctrl+U", "Upload  (no cloud accounts yet)")]
    // DRAGON-514: one upload at a time. The busy tip must not advertise the key either, for
    // the same reason the account-less one does not: pressing it does nothing.
    #[case::busy(true, true, "Ctrl+U", "Upload  (an upload is already running)")]
    // No accounts AND busy: the account answer wins, matching `edit::upload_toggle`'s order.
    #[case::busy_without_accounts(false, true, "Ctrl+U", "Upload  (no cloud accounts yet)")]
    fn the_upload_tip_says_what_the_button_will_do(
        #[case] has_accounts: bool,
        #[case] busy: bool,
        #[case] keybind: &str,
        #[case] expected: &str,
    ) {
        let tip = upload_tip(has_accounts, busy, keybind);
        assert_eq!(tip, expected);
        if busy || !has_accounts {
            assert!(!tip.contains("Ctrl"), "a dead button must not advertise a key: {tip}");
        }
        // The house separator: a name, TWO spaces, one parenthetical.
        assert!(tip.starts_with("Upload  ("), "{tip} broke the tip shape");
        assert!(tip.ends_with(')'), "{tip} broke the tip shape");
        assert!(!tip.contains('\u{2014}'), "{tip} must stay em-dash free");
    }

    /// The live tip is the SAME shape its neighbours get, proven against `fixed_key_tip`
    /// rather than asserted, so Save, Upload and Close read as one control family.
    #[test]
    fn a_live_upload_advertises_its_key_like_its_neighbours() {
        use crate::shortcuts::FixedEditorAction;
        let key = FixedEditorAction::Upload.label();
        assert_eq!(upload_tip(true, false, key), fixed_key_tip("Upload", key));
    }
}

#[cfg(test)]
mod upload_flyout_geometry_tests {
    use super::*;

    /// Where libcosmic puts a popup opened with `popover::Position::Bottom`, given the anchor's
    /// left edge and width and the popup's width (`widget/popover.rs`: the anchor point is
    /// `bounds.x + bounds.width / 2`, ROUNDED, and the overlay is then laid out at
    /// `position.x - width / 2`, clamped into the viewport). Reproduced here because it is the
    /// premise the account picker's sizing rests on, and GUI placement cannot be observed in
    /// this environment. The viewport clamp is left out: it only ever moves a menu that would
    /// leave the screen, which is not what alignment is about.
    fn popup_left(anchor_x: f32, anchor_w: f32, popup_w: f32) -> f32 {
        (anchor_x + anchor_w / 2.0).round() - popup_w / 2.0
    }

    /// DRAGON-500: the account chip spans its column, so it starts and ends exactly where the
    /// checkbox and the Upload button under it do, leaving ONE margin against the flyout's edge
    /// on each side.
    #[test]
    fn the_account_chip_sits_on_the_flyouts_content_margins() {
        let margin = MENU_CARD_PAD + UPLOAD_PANEL_PAD;
        assert!((UPLOAD_ACCOUNT_CHIP_W - (UPLOAD_MENU_W - 2.0 * margin)).abs() < f32::EPSILON);
        // Stated as the picture: left margin, control, right margin, and nothing left over.
        assert!(
            (margin + UPLOAD_ACCOUNT_CHIP_W + margin - UPLOAD_MENU_W).abs() < f32::EPSILON,
            "the chip plus its two margins must be the whole flyout"
        );
        // And the margins really are the same on both sides, which is the owner's actual ask.
        let (left, right) = (margin, UPLOAD_MENU_W - (margin + UPLOAD_ACCOUNT_CHIP_W));
        assert!((left - right).abs() < f32::EPSILON, "{left} left vs {right} right");
    }

    /// The menu opens FLUSH with the control it belongs to: same width, so the popover's
    /// centre-anchored placement resolves to the control's own left edge, at every anchor
    /// position and with no offset applied anywhere. The old pairing (a 348px menu under a
    /// 324px chip) is pinned as the thing that drifted, so the two can never quietly diverge
    /// again without this failing.
    #[test]
    fn the_account_menu_opens_flush_with_its_control() {
        for anchor_x in [0.0, 1.0, 37.0, 640.0, 1913.0] {
            let left = popup_left(anchor_x, UPLOAD_ACCOUNT_CHIP_W, UPLOAD_ACCOUNT_CHIP_W);
            assert!((left - anchor_x).abs() < f32::EPSILON, "the menu opened {left} not {anchor_x}");
            let right = left + UPLOAD_ACCOUNT_CHIP_W;
            assert!(
                (right - (anchor_x + UPLOAD_ACCOUNT_CHIP_W)).abs() < f32::EPSILON,
                "the menu's right edge must land on the control's"
            );
        }
        // What it used to do: a menu wider than its control hangs off BOTH sides of it.
        let drifted = popup_left(100.0, UPLOAD_MENU_W - 24.0, UPLOAD_MENU_W);
        assert!(drifted < 100.0, "the historical pairing really did open left of its control");
    }

    /// The chip stands up to the rows around it: about twice its own content height, and
    /// distinctly taller than the standard chip a natural-height picker draws.
    #[test]
    fn the_account_chip_is_about_twice_its_content_height() {
        // A 16px brand mark or a 13px label's line, plus the shared chip padding either side.
        let natural = TEXT_MENU_LABEL_SIZE.max(16.0) + 2.0 * 2.0;
        let ratio = UPLOAD_ACCOUNT_CHIP_H / natural;
        assert!((1.8..=2.4).contains(&ratio), "{UPLOAD_ACCOUNT_CHIP_H} is {ratio}x the natural height");
    }
}
