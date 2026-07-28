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
        "input-mouse-symbolic" => "mouse-pointer", // pointer tool
        "camera-photo-symbolic" => "camera",
        "camera-video-symbolic" => "video",
        "media-record-symbolic" => "circle", // record dot
        "media-playback-start-symbolic" => "play",
        "media-playback-pause-symbolic" => "pause",
        "media-playback-stop-symbolic" => "square", // stop
        "emblem-ok-symbolic" => "check", // confirm / finish
        "edit-delete-symbolic" => "trash-2",
        "window-close-symbolic" => "x",
        "emblem-system-symbolic" => "settings", // gear
        "document-properties-symbolic" => "view", // scanner kind (QR/OCR "look")
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
        "app-window" => svg!("app-window"),
        "monitor" => svg!("monitor"),
        "move" => svg!("move"),
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
            "view-refresh-symbolic", "document-save-symbolic", "document-save-as-symbolic",
            "edit-copy-symbolic", "view-fullscreen-symbolic", "view-restore-symbolic",
            "edit-undo-symbolic", "edit-redo-symbolic", "insert-image-symbolic",
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
