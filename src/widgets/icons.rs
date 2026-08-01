//! The bundled Lucide (MIT) icon set + the single resolver every UI icon flows through
//! (DRAGON-324). Lucide glyphs are compiled in from `res/icons/lucide/*.svg` and chosen by
//! what each control DOES, not by the old freedesktop name.
//!
//! # Why self-bundle
//! `icon::from_name` resolves a freedesktop name against the system XDG theme (Linux) then
//! libcosmic's embedded `cosmic-icons` subset. Off-COSMIC (macOS / Windows) there is no
//! system theme, and several names the app uses are absent from the embedded subset — they
//! rendered blank, forcing a per-name "is it embedded?" workaround (the old
//! `vendored_icon_handle`). Bundling Lucide for EVERY icon makes resolution
//! platform-independent and retires that whole class of gap: [`handle`] maps each app name
//! to a compiled-in Lucide SVG, only falling back to `from_name` for an UNMAPPED name.
//!
//! Every Lucide SVG strokes with `currentColor`, so `.symbolic(true)` lets the widget tint
//! it with the active fg/accent color exactly like a COSMIC symbolic icon.

use cosmic::widget::icon::{self, Handle};

/// Resolve an app icon NAME to a bundled Lucide glyph handle. A mapped name embeds its
/// Lucide SVG (marked symbolic so the widget tint applies); an UNMAPPED name falls back to
/// the system/embedded theme (`from_name`), a safety net that preserves prior behavior.
pub fn handle(name: &str) -> Handle {
    match lucide_name(name) {
        Some(file) => icon::from_svg_bytes(lucide_bytes(file)).symbolic(true),
        None => icon::from_name(name).handle(),
    }
}

/// **THE icon-size seam (DRAGON-399): snap a glyph's logical box to a WHOLE pixel.**
///
/// iced rasterizes an SVG at `ceil(display_scale × bounds)` device px and then draws that
/// texture into the bounds it was given. When the bounds are FRACTIONAL the two disagree and the
/// texture is RESAMPLED to fit — a bilinear pass that softens every edge the rasterizer had
/// computed exactly. A whole-pixel box makes raster size == draw size, so the rasterizer's own
/// analytic coverage reaches the screen untouched.
///
/// The bug this fixes, measured: the preview toolbar's glyph box was `ICON_BOX 22 × CHROME_SCALE
/// 0.91 = 20.02` logical, rasterizing at 21px and drawn into 20.02 — a ~5% downscale. That is
/// what put the visible step in the accept checkmark's ascending arm. Reproduced outside the app:
/// a clean 61px `rsvg-convert` render downscaled to 60 lands on the app's exact glyph bounding
/// box (44×32) INCLUDING the step, while the un-resampled 61px render is clean at 45×33.
///
/// It also explains why six successive ASSET variants failed to help (miter join, stroke-to-path,
/// `check-line`, the crop glyph, two-path round caps, filling the viewBox): the resample happens
/// AFTER rasterization, so nothing expressible in the SVG could reach it. Do not go back to the
/// assets for a soft glyph before checking its box is integral.
///
/// **The rounding is deliberately independent of any particular scale VALUE.** It is not "0.91
/// happens to need it": every future [`crate::app::preview::surface::CHROME_SCALE`] stays sharp,
/// not only the ones where `22 × scale` lands on an integer by luck. Keep it that way — a
/// `if scale == …` shortcut here would silently blur the UI the next time the owner tunes the
/// chrome.
pub fn box_px(logical: f32) -> f32 {
    // `max(1.0)`: a rounded-to-zero box would make the glyph vanish rather than merely blur.
    logical.round().max(1.0)
}

/// Resolve `name` and size it to a whole-pixel box in ONE call — the path every sized glyph
/// should take, so a control added later cannot forget [`box_px`] the way scattered
/// `.round()` call sites would be forgotten. Chain `.class(..)` on the result as usual.
///
/// Some call sites legitimately do NOT come through here: cosmic's `Icon::size(u16)` takes a
/// whole number BY TYPE (it can't be fractional), and the About page's app icon is built from
/// raw bytes rather than a named handle. Both are noted where they sit.
pub fn sized(name: &str, box_px_logical: f32) -> icon::Icon {
    let px = box_px(box_px_logical);
    icon::icon(handle(name))
        .width(cosmic::iced::Length::Fixed(px))
        .height(cosmic::iced::Length::Fixed(px))
}

/// [`sized`], ROTATED by `radians` about the glyph's centre (DRAGON-460).
///
/// The same glyph the control shows at rest, turning — not a different widget swapped in
/// for the duration. That is what lets a button spin its own icon to say "working" without
/// the face changing under the user.
///
/// Goes through iced's `svg` widget rather than `icon::Icon`, because rotation lives on the
/// former. So it only works for a BUNDLED Lucide name, whose raw bytes we hold; an unmapped
/// name has no bytes here (it resolves through the system icon theme) and falls back to the
/// unrotated [`sized`] rather than silently rendering nothing. `.class(..)` chains on the
/// result exactly as it does for `sized`, so the caller still owns the tint.
///
/// The whole-pixel box from [`box_px`] is kept: a rotated glyph is resampled anyway, but
/// starting from an exactly-rasterized texture is still the sharper input.
/// `class` is taken rather than chained because the fallback below returns a DIFFERENT
/// widget type; passing the tint in keeps one call shape for the caller either way.
pub fn sized_rotated(
    name: &str,
    box_px_logical: f32,
    radians: f32,
    class: cosmic::theme::Svg,
) -> cosmic::Element<'static, crate::app::Msg> {
    let px = box_px(box_px_logical);
    let Some(file) = lucide_name(name) else {
        return sized(name, box_px_logical).class(class).into();
    };
    cosmic::widget::svg(cosmic::widget::svg::Handle::from_memory(lucide_bytes(file)))
        .width(cosmic::iced::Length::Fixed(px))
        .height(cosmic::iced::Length::Fixed(px))
        .rotation(cosmic::iced::Radians(radians))
        .class(class)
        .into()
}

/// Whether `name` resolves to a BUNDLED Lucide glyph rather than falling through to the
/// system/embedded theme (which renders blank off Linux for most names). The one public
/// probe over [`lucide_name`], so callers can assert a glyph really ships without exposing
/// the mapping itself. Test-only: production code just calls [`handle`], which already
/// falls back gracefully — this exists so a TEST can insist there is nothing to fall back to.
#[cfg(test)]
pub(crate) fn is_bundled(name: &str) -> bool {
    lucide_name(name).is_some()
}

/// Map an app icon name (freedesktop-style / legacy) to the bundled Lucide glyph that best
/// fits the control's MEANING. `None` = unmapped (the caller falls back to `from_name`). The
/// single source of the mapping decision — unit-tested; every returned file is embedded by
/// [`lucide_bytes`].
fn lucide_name(name: &str) -> Option<&'static str> {
    Some(match name {
        // Capture modes / overlay.
        "screenshot-selection-symbolic" => "crop", // region marquee
        "screenshot-window-symbolic" => "app-window",
        "screenshot-screen-symbolic" => "monitor",
        "object-move-symbolic" => "move", // pan / grab tool
        // The preview editor's HAND tool (DRAGON-392) — lucide `hand`, the open palm. Distinct
        // from `move` (the capture overlay's four-way drag handle) and from `hand-coins`.
        "hand-symbolic" => "hand",
        "input-mouse-symbolic" => "mouse-pointer", // pointer tool
        "camera-photo-symbolic" => "camera",
        "camera-video-symbolic" => "video",
        "media-record-symbolic" => "circle", // record dot
        "media-playback-start-symbolic" => "play",
        "media-playback-pause-symbolic" => "pause",
        "media-playback-stop-symbolic" => "square", // stop
        "emblem-ok-symbolic" => "check", // confirm / finish
        // The CROP session's ACCEPT mark (DRAGON-392). It is lucide `check`'s exact geometry and
        // the set's exact stroke weight — the ONE thing it changes is that the tick is TWO
        // SEPARATE PATHS (`M20 6 9 17` + `M9 17 4 12`) instead of one joined polyline.
        // **Do not "restore" it to the upstream single path**, and do not fold it back into
        // `check`: the split is the whole point here, and `check` is shared with the countdown
        // chip, the settings rows and the success toast, which must not shift.
        //
        // WHY, measured at the ~19 device px this actually renders at: with ONE joined path the
        // vertex is a stroke JOIN, and a round join's disc (radius stroke/2, well under a pixel
        // here) cannot fill the outer wedge — the tip antialiases into a visible chip. Two paths
        // make that vertex two overlapping round CAPS instead, which cannot under-resolve at any
        // size. Tip-pixel coverage, joined -> two-path: 102 -> 161 and 134 -> 198 (of 255).
        // (`stroke-linejoin="miter"` was an earlier fix and is measurably worse than the split;
        // outlining the stroke does NOT work at all — it bakes the round join's arcs and
        // rasterizes pixel-identically to the chipped original.)
        //
        // What it does NOT buy: the two 45° arms are the same diagonal at the same size, so they
        // stay stair-stepped. Only more pixels help that — no asset change will.
        "crop-accept-symbolic" => "check-split",
        "edit-delete-symbolic" => "trash-2",
        "window-close-symbolic" => "x",
        "emblem-system-symbolic" => "settings", // gear
        "document-properties-symbolic" => "view", // scanner kind (QR/OCR "look")
        // DRAGON-456: the scan kind button's HOVER face while the scanner is ALREADY the
        // active kind, where a press re-reads the screen instead of switching kind. The
        // COUNTER-clockwise arrows, deliberately distinct from `view-refresh-symbolic`'s
        // clockwise `refresh-cw` (a general "reload"): this one means "read it again".
        "scan-refresh-symbolic" => "refresh-ccw",
        "preferences-system-symbolic" => "settings",
        "audio-input-microphone-symbolic" => "mic",
        "audio-x-generic-symbolic" => "audio-lines", // Audio settings tab
        "object-merge-symbolic" => "merge", // Mixing settings tab
        "audio-volume-high-symbolic" => "volume-2", // system audio
        "notification-symbolic" => "bell",
        "input-keyboard-symbolic" => "keyboard",
        "view-refresh-symbolic" => "refresh-cw",
        // Preview action bar.
        "document-save-symbolic" => "save",
        "document-save-as-symbolic" => "save-all", // save variant
        "edit-copy-symbolic" => "copy",
        "view-fullscreen-symbolic" => "maximize",
        "view-restore-symbolic" => "minimize", // leave fullscreen → windowed
        "edit-undo-symbolic" => "undo-2",
        "edit-redo-symbolic" => "redo-2",
        // Covermark + annotation tools.
        "crop-symbolic" => "crop", // image crop tool (DRAGON-382)
        "object-flip-horizontal-symbolic" => "arrow-left-right", // annotation color swap (DRAGON-386)
        "insert-image-symbolic" => "flag", // covermark (flag/redact a region)
        "zoom-in-symbolic" => "zoom-in", // covermark zoom slider
        "display-brightness-symbolic" => "contrast", // covermark opacity slider
        "mail-forward-symbolic" => "arrow-up-right", // draw-arrow tool
        // The annotation POINTER (selection) tool (DRAGON-341) — a filled arrow cursor,
        // deliberately DISTINCT from the capture overlay's outline `mouse-pointer`.
        "pointer-select-symbolic" => "mouse-pointer-2",
        // Annotation stroke-width toggle group (DRAGON-357 item 9): a horizontal line drawn at
        // SEVEN thicknesses, the glyph's stroke scaling with the px preset it selects.
        "minus-1" => "minus-1",
        "minus-2" => "minus-2",
        "minus-4" => "minus-4",
        "minus-6" => "minus-6",
        "minus-8" => "minus-8",
        "minus-10" => "minus-10",
        "minus-12" => "minus-12",
        "format-text-highlight-symbolic" => "highlighter",
        "checkbox-symbolic" => "square", // box / rectangle tool
        "box-highlight-symbolic" => "box-highlight", // box outline + inner highlighter marker
        // The SEQUENCE BADGE tool (DRAGON-340). The ticket names lucide `circle-star` for the
        // tool; the badge the tool DRAWS is spelled out separately (disc + numeral + ring), so
        // this is the tray glyph, not the rendered mark.
        "sequence-badge-symbolic" => "circle-star",
        "pencil-symbolic" => "pencil-line", // freehand pencil tool (DRAGON-338)
        "text-tool-symbolic" => "type", // text annotation tool (DRAGON-354 items 6 + 5)
        "clipboard-copy-symbolic" => "clipboard-copy", // Copy-to-clipboard button (DRAGON-357)
        "clipboard-check-symbolic" => "clipboard-check", // "Copied" toast (DRAGON-357)
        "clipboard-x-symbolic" => "clipboard-x", // clipboard-error toast (DRAGON-357)
        "save-check-symbolic" => "save-check", // "Saved" success toast (DRAGON-357)
        "save-off-symbolic" => "save-off", // save-error toast (DRAGON-357)
        "eraser-symbolic" => "eraser", // pencil eraser (DRAGON-338)
        "sun-dim-symbolic" => "sun-dim", // dim/spotlight tool (sun with dashed rays)
        "spotlight-symbolic" => "spotlight", // spotlight tool (fixture + beam onto a lit spot)
        "view-grid-symbolic" => "grid-3x3", // pixelate redaction (mosaic)
        "image-filter-symbolic" => "droplet", // blur redaction
        "edit-cut-symbolic" => "scissors", // razor
        // The PREVIEW EDITOR's identity glyph (DRAGON-353): the settings nav page and the
        // Keyboard Shortcuts page's "Preview Editor" tab both wear it, so the two agree.
        "view-timeline-symbolic" => "timeline",
        "video-x-generic-symbolic" => "film", // video placeholder
        "pan-down-symbolic" => "chevron-down",
        // Settings + misc.
        "folder-open-symbolic" => "folder-open",
        "list-add-symbolic" => "plus",
        "system-search-symbolic" => "search",
        "navbar-open-symbolic" => "panel-left-open",
        "navbar-closed-symbolic" => "panel-left-close",
        "accessories-screenshot-symbolic" => "camera", // capture-modes tab
        "applications-multimedia-symbolic" => "clapperboard", // audio/video tab
        "applications-graphics-symbolic" => "image",
        "image-x-generic-symbolic" => "image",
        "video-display-symbolic" => "monitor",
        "utilities-system-monitor-symbolic" => "activity", // Health tab
        "speedometer-symbolic" => "gauge", // Run benchmark button
        "donate-symbolic" => "hand-coins", // About-page donate button
        "help-about-symbolic" => "info",
        "software-update-available-symbolic" => "download",
        "dialog-warning-symbolic" => "triangle-alert",
        "dialog-error-symbolic" => "circle-x",
        _ => return None,
    })
}

// NOTE: `sliders-horizontal` / `eye-off` are still bundled (referenced by `lucide_bytes`)
// though no name maps to them anymore (scanner → `view`, covermark → `flag`, DRAGON-324).
// Kept as ready-to-use glyphs; harmless.

/// The bytes of a bundled Lucide SVG by its file stem. Exhaustive over every file
/// [`lucide_name`] can return; an unbundled stem is a programmer error (the two lists must
/// stay in sync — the `every_mapped_name_embeds` test enforces it).
fn lucide_bytes(file: &str) -> &'static [u8] {
    macro_rules! svg {
        ($f:literal) => {
            include_bytes!(concat!("../../res/icons/lucide/", $f, ".svg")).as_slice()
        };
    }
    match file {
        "crop" => svg!("crop"),
        "arrow-left-right" => svg!("arrow-left-right"),
        "app-window" => svg!("app-window"),
        "monitor" => svg!("monitor"),
        "move" => svg!("move"),
        "hand" => svg!("hand"),
        "mouse-pointer" => svg!("mouse-pointer"),
        "mouse-pointer-2" => svg!("mouse-pointer-2"),
        "camera" => svg!("camera"),
        "video" => svg!("video"),
        "circle" => svg!("circle"),
        "circle-star" => svg!("circle-star"),
        "play" => svg!("play"),
        "pause" => svg!("pause"),
        "square" => svg!("square"),
        "check" => svg!("check"),
        "check-split" => svg!("check-split"),
        "trash-2" => svg!("trash-2"),
        "x" => svg!("x"),
        "settings" => svg!("settings"),
        "sliders-horizontal" => svg!("sliders-horizontal"),
        "mic" => svg!("mic"),
        "audio-lines" => svg!("audio-lines"),
        "merge" => svg!("merge"),
        "volume-2" => svg!("volume-2"),
        "bell" => svg!("bell"),
        "keyboard" => svg!("keyboard"),
        "refresh-cw" => svg!("refresh-cw"),
        "refresh-ccw" => svg!("refresh-ccw"), // scanner re-read (DRAGON-456)
        "save" => svg!("save"),
        "save-all" => svg!("save-all"),
        "copy" => svg!("copy"),
        "maximize" => svg!("maximize"),
        "minimize" => svg!("minimize"),
        "undo-2" => svg!("undo-2"),
        "redo-2" => svg!("redo-2"),
        "eye-off" => svg!("eye-off"),
        "zoom-in" => svg!("zoom-in"),
        "contrast" => svg!("contrast"),
        "arrow-up-right" => svg!("arrow-up-right"),
        "minus-1" => svg!("minus-1"),
        "minus-2" => svg!("minus-2"),
        "minus-4" => svg!("minus-4"),
        "minus-6" => svg!("minus-6"),
        "minus-8" => svg!("minus-8"),
        "minus-10" => svg!("minus-10"),
        "minus-12" => svg!("minus-12"),
        "highlighter" => svg!("highlighter"),
        "box-highlight" => svg!("box-highlight"),
        "pencil-line" => svg!("pencil-line"),
        "type" => svg!("type"),
        "clipboard-copy" => svg!("clipboard-copy"),
        "clipboard-check" => svg!("clipboard-check"),
        "clipboard-x" => svg!("clipboard-x"),
        "save-check" => svg!("save-check"),
        "save-off" => svg!("save-off"),
        "eraser" => svg!("eraser"),
        "sun-dim" => svg!("sun-dim"),
        "spotlight" => svg!("spotlight"),
        "grid-3x3" => svg!("grid-3x3"),
        "droplet" => svg!("droplet"),
        "scissors" => svg!("scissors"),
        "timeline" => svg!("timeline"),
        "film" => svg!("film"),
        "chevron-down" => svg!("chevron-down"),
        "folder-open" => svg!("folder-open"),
        "plus" => svg!("plus"),
        "search" => svg!("search"),
        "panel-left-open" => svg!("panel-left-open"),
        "panel-left-close" => svg!("panel-left-close"),
        "clapperboard" => svg!("clapperboard"),
        "image" => svg!("image"),
        "activity" => svg!("activity"),
        "gauge" => svg!("gauge"),
        "hand-coins" => svg!("hand-coins"),
        "info" => svg!("info"),
        "download" => svg!("download"),
        "triangle-alert" => svg!("triangle-alert"),
        "circle-x" => svg!("circle-x"),
        "view" => svg!("view"), // scanner kind
        "flag" => svg!("flag"), // covermark
        other => panic!("unbundled lucide icon `{other}` — add it to res/icons/lucide/"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every app name the codebase maps must resolve to a bundled Lucide file whose bytes
    /// embed (no drift between [`lucide_name`] and [`lucide_bytes`], no panic).
    #[test]
    fn every_mapped_name_embeds() {
        // The full set of app names routed through the resolver (must match `lucide_name`).
        let names = [
            "screenshot-selection-symbolic", "screenshot-window-symbolic",
            "screenshot-screen-symbolic", "object-move-symbolic", "input-mouse-symbolic",
            "camera-photo-symbolic", "camera-video-symbolic", "media-record-symbolic",
            "media-playback-start-symbolic", "media-playback-pause-symbolic",
            "media-playback-stop-symbolic", "emblem-ok-symbolic", "edit-delete-symbolic",
            "window-close-symbolic", "emblem-system-symbolic", "document-properties-symbolic",
            "preferences-system-symbolic", "audio-input-microphone-symbolic",
            "audio-x-generic-symbolic", "object-merge-symbolic",
            "audio-volume-high-symbolic", "notification-symbolic", "input-keyboard-symbolic",
            "view-refresh-symbolic", "scan-refresh-symbolic",
            "document-save-symbolic", "document-save-as-symbolic",
            "edit-copy-symbolic", "view-fullscreen-symbolic", "view-restore-symbolic",
            "edit-undo-symbolic", "edit-redo-symbolic", "crop-symbolic", "crop-accept-symbolic",
            "insert-image-symbolic",
            "zoom-in-symbolic", "display-brightness-symbolic", "mail-forward-symbolic",
            "pointer-select-symbolic",
            "format-text-highlight-symbolic", "checkbox-symbolic", "box-highlight-symbolic",
            "pencil-symbolic", "text-tool-symbolic", "eraser-symbolic",
            "clipboard-copy-symbolic", "clipboard-check-symbolic", "clipboard-x-symbolic",
            "save-check-symbolic", "save-off-symbolic",
            "sequence-badge-symbolic",
            "sun-dim-symbolic", "spotlight-symbolic", "view-grid-symbolic",
            "image-filter-symbolic", "edit-cut-symbolic", "view-timeline-symbolic",
            "minus-1", "minus-2", "minus-4", "minus-6", "minus-8", "minus-10", "minus-12",
            "video-x-generic-symbolic", "pan-down-symbolic", "folder-open-symbolic",
            "list-add-symbolic", "system-search-symbolic", "navbar-open-symbolic",
            "navbar-closed-symbolic", "accessories-screenshot-symbolic",
            "applications-multimedia-symbolic", "applications-graphics-symbolic",
            "image-x-generic-symbolic", "video-display-symbolic",
            "utilities-system-monitor-symbolic", "speedometer-symbolic", "donate-symbolic",
            "help-about-symbolic",
            "software-update-available-symbolic", "dialog-warning-symbolic",
            "dialog-error-symbolic",
        ];
        for n in names {
            let file = lucide_name(n).unwrap_or_else(|| panic!("`{n}` is unmapped"));
            assert!(!lucide_bytes(file).is_empty(), "`{file}` embeds no bytes");
            assert!(lucide_bytes(file).starts_with(b"<svg"), "`{file}` is not an SVG");
        }
    }

    /// **One stroke weight across the toolbar set** (DRAGON-392): the bundled glyphs stroke at 2
    /// — lucide's own default — so no icon reads heavier or thinner than the ones beside it. The
    /// owner's rule, after a `+1px` pass on the crop / accept / cancel trio was tried and
    /// withdrawn: "there shouldn't be out-of-sync variants".
    ///
    /// This is the guard that turns that from a preference into something the suite enforces — a
    /// new bold variant fails HERE rather than shipping and being spotted by eye.
    ///
    /// ONE exemption remains, and it is permanent:
    ///
    /// * `minus-N` — the annotation line-width PREVIEW swatches. The varying stroke IS the
    ///   content: each one shows the thickness it selects, so a single weight would make the
    ///   whole control meaningless.
    ///
    /// **`move` and `type` used to be exempt too, and no longer are** (DRAGON-399). They had
    /// been thickened to stroke 3 on the grounds that they "render poorly on HiDPI" — which was
    /// TRUE, but the cause was the fractional-icon-box resample ([`box_px`]), not the glyphs.
    /// Measured at the device size a 2× display produced (`22 × 0.82 × 2 = 36.08`, rastered at
    /// 37 and squeezed into 36.08): the resample cost `move` **140 of its 384 solid pixels** and
    /// `type` **116 of 254**, turning them into partial coverage — and it cost the untouched
    /// `crop` glyph 92 the same way, which is what shows the defect was universal rather than
    /// something about those two. With the box snapped, the compensation is double-counting, so
    /// they are back at the set weight and byte-identical to upstream lucide.
    ///
    /// Do not reinstate them from folklore. A new exemption needs a reason of the `minus-N` kind
    /// — one where the weight is the CONTENT — recorded beside it. Weight parity is the default.
    #[test]
    fn bundled_glyphs_share_one_stroke_weight_unless_deliberately_thickened() {
        /// The set's weight: lucide's default, and what every toolbar glyph wears.
        const SET_WEIGHT: &str = "2";
        // See the doc above for why each of these is exempt.
        let exempt = |stem: &str| stem.starts_with("minus-");

        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("res/icons/lucide");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).expect("the bundled icon directory") {
            let path = entry.expect("a directory entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("svg") {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).expect("a file stem").to_string();
            let svg = std::fs::read_to_string(&path).expect("a readable SVG");
            // EVERY occurrence, not just the first: a future multi-path glyph could carry a
            // per-path weight, and that is exactly the drift this is here to catch.
            let widths: Vec<&str> = svg
                .match_indices("stroke-width=\"")
                .map(|(i, m)| {
                    let rest = &svg[i + m.len()..];
                    &rest[..rest.find('"').expect("a closed stroke-width attribute")]
                })
                .collect();
            assert!(!widths.is_empty(), "`{stem}` declares no stroke-width");
            if exempt(&stem) {
                continue;
            }
            for w in widths {
                assert_eq!(
                    w, SET_WEIGHT,
                    "`{stem}` strokes at {w}, not the set's {SET_WEIGHT} — an out-of-sync \
                     variant. If the deviation is deliberate (a rendering compensation, or the \
                     stroke IS the content), exempt it in this test WITH the reason."
                );
            }
            checked += 1;
        }
        // A directory that failed to read, or a rename that emptied it, must not pass silently.
        assert!(checked > 60, "only {checked} glyphs checked — the bundled set should be larger");
    }

    /// **Every icon box is a WHOLE logical pixel** (DRAGON-399) — across a spread of chrome
    /// scales, not just today's. A fractional box makes iced raster at one size and draw at
    /// another, and the bilinear resample that follows softens every glyph edge; that defect
    /// survived six asset rewrites before it was found, so it is worth failing loudly for.
    ///
    /// The spread deliberately includes values that are hostile to `ICON_BOX 22` — 0.91 (the
    /// shipping one, `20.02` raw), 0.82 (`18.04`), and thirds/sevenths that land nowhere near an
    /// integer. If a future scale is chosen for how the chrome LOOKS, this keeps the glyphs sharp
    /// without anyone having to remember why.
    #[test]
    fn every_icon_box_snaps_to_a_whole_pixel() {
        let whole = |v: f32| (v.fract() == 0.0, v);
        // The seam itself, over raw fractional inputs.
        for raw in [18.04_f32, 20.02, 16.4, 0.2, 21.5, 22.0, 33.333, 47.999] {
            let (ok, v) = whole(box_px(raw));
            assert!(ok, "box_px({raw}) = {v}, not a whole pixel");
            assert!(v >= 1.0, "box_px({raw}) = {v} would make the glyph vanish");
        }
        // …and through the app's own derivation, at scales chosen to be awkward for ICON_BOX.
        for scale in [0.82_f32, 0.91, 1.0, 1.09, 0.5, 1.0 / 3.0, 2.0 / 7.0, 1.618] {
            let base = crate::app::ICON_BOX * scale;
            let (ok, v) = whole(box_px(base));
            assert!(ok, "chrome scale {scale}: glyph box {base} snapped to {v}, not whole");
            // The slider glyph derives ANOTHER box from that one — integral today only because
            // 20 x 0.8 = 16, so it must go through the seam too rather than ride on luck. (The
            // 0.8 is `preview::chrome::SLIDER_SCALE`, private to that module; mirrored here as a
            // literal because this test is about the SEAM, not about that constant's value.)
            let (ok, sv) = whole(box_px(v * 0.8));
            assert!(ok, "chrome scale {scale}: slider glyph box {sv} is not whole");
        }
    }

    /// Meaning-based remaps that aren't literal name matches — guard the deliberate choices.
    #[test]
    fn meaning_based_remaps() {
        assert_eq!(lucide_name("insert-image-symbolic"), Some("flag")); // covermark
        assert_eq!(lucide_name("document-properties-symbolic"), Some("view")); // scanner kind
        assert_eq!(lucide_name("mail-forward-symbolic"), Some("arrow-up-right"));
        assert_eq!(lucide_name("display-brightness-symbolic"), Some("contrast"));
        assert_eq!(lucide_name("document-save-as-symbolic"), Some("save-all"));
        assert_eq!(lucide_name("checkbox-symbolic"), Some("square"));
        assert_eq!(lucide_name("box-highlight-symbolic"), Some("box-highlight")); // DRAGON-333
        assert_eq!(lucide_name("sun-dim-symbolic"), Some("sun-dim")); // DRAGON-329
        assert_eq!(lucide_name("pencil-symbolic"), Some("pencil-line")); // DRAGON-338
        assert_eq!(lucide_name("eraser-symbolic"), Some("eraser")); // DRAGON-338
        // DRAGON-340: the sequence-badge TOOL wears lucide's circle-star; the badge it draws is
        // its own vector (disc + numeral + ring), not this glyph.
        assert_eq!(lucide_name("sequence-badge-symbolic"), Some("circle-star"));
        // DRAGON-341: the annotation pointer tool gets the FILLED cursor glyph, never the
        // capture overlay's outline one (the two controls must not look alike).
        assert_eq!(lucide_name("pointer-select-symbolic"), Some("mouse-pointer-2"));
        assert_eq!(lucide_name("input-mouse-symbolic"), Some("mouse-pointer"));
        // DRAGON-354 item 5: the text tool wears lucide's `type` glyph (was `case-sensitive`).
        assert_eq!(lucide_name("text-tool-symbolic"), Some("type"));
        // DRAGON-456: the scanner's re-read hover face is the COUNTER-clockwise refresh, and
        // it must stay distinct from the clockwise one every other "reload" control wears —
        // the two are one character apart in lucide's own naming, so pin both.
        assert_eq!(lucide_name("scan-refresh-symbolic"), Some("refresh-ccw"));
        assert_eq!(lucide_name("view-refresh-symbolic"), Some("refresh-cw"));
    }

    /// The DRAGON-338 pencil + eraser glyphs embed and are real SVGs (the tools' toolbar icons).
    #[test]
    fn pencil_and_eraser_icons_are_embedded() {
        for name in ["pencil-symbolic", "eraser-symbolic"] {
            let file = lucide_name(name).unwrap_or_else(|| panic!("`{name}` is mapped"));
            assert!(lucide_bytes(file).starts_with(b"<svg"), "`{file}` embeds an SVG");
        }
    }

    /// The DRAGON-333 box-highlight glyph embeds and is a real SVG (the tool's toolbar icon).
    #[test]
    fn box_highlight_icon_is_embedded() {
        let file = lucide_name("box-highlight-symbolic").expect("box-highlight is mapped");
        assert_eq!(file, "box-highlight");
        assert!(lucide_bytes(file).starts_with(b"<svg"), "box-highlight embeds an SVG");
    }

    /// An unknown name falls through to the system/embedded theme (None from the mapper).
    #[test]
    fn unknown_name_falls_through() {
        assert_eq!(lucide_name("definitely-not-an-icon"), None);
    }
}
