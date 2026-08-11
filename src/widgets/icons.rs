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
//!
//! # The second directory: `res/icons/brands/` (DRAGON-482)
//!
//! Lucide has no cloud-provider marks, so the ones the cloud-accounts feature needs are the
//! providers' OWN official marks: sanitized copies of the vector each brand publishes, with
//! the source URL recorded in a comment at the top of every file. Nothing is restyled. Each
//! sits in a square 24x24 viewBox with the mark centred, which is the only change made to any
//! of them: the geometry and the colours are the brand's.
//!
//! **They are the one exception to this set's tint rule, and it is the whole point.** A brand
//! mark is recognised BY its colours, and a Google Drive triangle flattened to the theme's
//! foreground is not a Google Drive triangle; the first build shipped them as hand-drawn
//! monochrome glyphs and the owner's answer on seeing them was that they had to be the real
//! marks. So [`brand`] is the ONE resolution path that does not mark its handle symbolic, and
//! the SVG's own fills and gradients reach the screen. [`handle`] routes every brand name
//! through it, so no call site has to remember which names are brands.
//!
//! Using an unmodified mark to say which service an account connects to is the case every one
//! of these brands' guidelines sanctions. Keep it that way: do not recolour one, do not
//! restyle one, and do not put one on anything but the provider it belongs to.
//!
//! **Proton Drive is a real mark again, by owner decision (2026-08-07).** DRAGON-566 had
//! swapped `proton-drive.svg` for a neutral locked-cloud glyph, because the sanction above
//! assumes an integration the brand OFFERS and Proton has no third-party API and does not
//! officially support this one (it runs through Proton's MIT-licensed CLI). The owner
//! reviewed Proton's third-party branding terms (quoted in DRAGON-566) and accepted the
//! risk, so the official mark is restored and the neutral glyph and the registry row's
//! `support_note` disclosure were removed at owner direction, not by oversight. Proton's
//! stance itself is unchanged; the file's own comment carries the same record.
//!
//! They live in their OWN directory rather than in `lucide/`, and that is deliberate:
//! `res/icons/lucide/` is a vendored MIT-licensed upstream set, and mixing files of different
//! provenance into it would make its licensing a guess. [`lucide_bytes`] embeds from both.
//!
//! iCloud Drive (no official API, not listed in the add-account picker since DRAGON-498)
//! keeps its real mark although nothing draws it today: the REGISTRY still carries the
//! provider, so anything that names it, now or later, has its mark ready and does not have to
//! invent a downgraded stand-in. Proton Drive sat in that same sentence until DRAGON-485 made
//! it connectable through its own CLI, and DRAGON-566 then swapped its glyph as above.

use cosmic::widget::icon::{self, Handle};

/// Resolve an app icon NAME to a bundled glyph handle.
///
/// A brand name resolves through [`brand`], in the brand's own colours. Any other mapped name
/// embeds its Lucide SVG marked symbolic, so the widget tint applies. An UNMAPPED name falls
/// back to the system/embedded theme (`from_name`), a safety net that preserves prior
/// behavior.
///
/// The brand branch lives HERE rather than at the four call sites that draw a provider mark,
/// because two of them pass a name they did not choose (an account whose provider this build
/// does not know falls back to `cloud-symbolic`) and none of them should have to know which
/// names are brands. One decision, one place.
pub fn handle(name: &str) -> Handle {
    if let Some(mark) = brand(name) {
        return mark;
    }
    match lucide_name(name) {
        Some(file) => icon::from_svg_bytes(lucide_bytes(file)).symbolic(true),
        None => icon::from_name(name).handle(),
    }
}

/// The handle for a BRAND mark, in the brand's own colours (DRAGON-482). Pure; unit-tested.
///
/// `None` for every other name, which is what lets a caller hand over a name it did not pick
/// without deciding anything itself.
///
/// The handle is deliberately NOT `.symbolic(true)`. libcosmic substitutes the theme's icon
/// colour only when the handle says symbolic AND the widget's class leaves the colour unset
/// (`iced/widget/src/svg.rs`), so an unmarked handle renders the file's own fills and
/// gradients. That also means a brand icon must not be given a colouring `.class(..)`: the
/// class wins over the file either way round.
pub fn brand(name: &str) -> Option<Handle> {
    brand_file(name).map(|file| icon::from_svg_bytes(lucide_bytes(file)))
}

/// The bundled file `name` resolves to WHEN it is one of the brand marks. Pure; unit-tested.
///
/// Keyed on the `brand-` stem prefix rather than a second list of names, so adding a provider
/// mark is one row in [`lucide_name`] plus one in [`lucide_bytes`] and nothing else.
fn brand_file(name: &str) -> Option<&'static str> {
    lucide_name(name).filter(|file| file.starts_with("brand-"))
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
        // The About page's release-kind line (Linux Flatpak): lucide `package`, a shipped
        // box, which is exactly what the line names. Its four siblings are
        // `application-x-executable-symbolic` / `application-x-appimage-symbolic` below and
        // `package-macos-symbolic` / `package-windows-symbolic` here.
        "package-x-generic-symbolic" => "package",
        // DRAGON-614: the two non-Linux release kinds, the owner's picks. Named as siblings
        // of `package-x-generic-symbolic` rather than with a `brand-` prefix, which is
        // reserved for the colour provider marks in `res/icons/brands/`. These two are
        // ordinary symbolic glyphs and must tint like every other one.
        "package-macos-symbolic" => "apple",
        "package-windows-symbolic" => "grid-2x2",
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
        // The SAME trash can, under freedesktop's name for "delete this thing that already
        // exists somewhere else" (DRAGON-514: the upload meter's undo, and the toast that
        // follows it). Its own entry rather than a call-site swap to `edit-delete-symbolic`,
        // for the reason `process-stop-symbolic` keeps its own below: removing a file from a
        // cloud account and deleting a local one are different promises, and a redesign of one
        // must not silently move the other. Unmapped until now, so both call sites were falling
        // back to whatever trash the system theme had, next to lucide glyphs everywhere else.
        "user-trash-symbolic" => "trash-2",
        "window-close-symbolic" => "x",
        // The same plain X glyph, under freedesktop's name for "cancel an in-progress
        // action" (DRAGON-490's titlebar upload Cancel), kept as its OWN entry rather than
        // reusing `window-close-symbolic` directly at the call site: the two names mean
        // different things (dismiss this control vs. close the whole window) even though
        // they render identically today, and a future redesign of one must not silently
        // move the other.
        "process-stop-symbolic" => "x",
        "emblem-system-symbolic" => "settings", // gear
        "document-properties-symbolic" => "view", // scanner kind (QR/OCR "look")
        // DRAGON-456: the scan kind button's HOVER face while the scanner is ALREADY the
        // active kind, where a press re-reads the screen instead of switching kind. The
        // COUNTER-clockwise arrows, deliberately distinct from `view-refresh-symbolic`'s
        // clockwise `refresh-cw` (a general "reload"): this one means "read it again".
        // DRAGON-460: the CLOCKWISE glyph, because the button spins it clockwise. A
        // counter-clockwise arrowhead turning clockwise reads as an animation bug.
        "scan-refresh-symbolic" => "refresh-cw",
        "preferences-system-symbolic" => "settings",
        // The macOS Health page's "Manage permissions" button (DRAGON-412): the same
        // Lucide `key` the tray daemon's own "Manage Permissions" menu item uses
        // (`recording_ui::MenuIcon::Permissions`), so the two affordances read as one.
        "dialog-password-symbolic" => "key",
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
        // DRAGON-582: the colour picker tool, in the preview editor's bottom bar and
        // named directly by the owner (Lucide `pipette`).
        "color-select-symbolic" => "pipette",
        // DRAGON-588: the Keyboard Shortcuts page's Global tab, the OS-owned hotkeys.
        // Lucide `globe`, the owner's pick: these are the keys that work anywhere on the
        // system rather than inside one of our windows.
        "globe-symbolic" => "globe",
        // DRAGON-591: the About page's release-kind line, one glyph per package kind
        // (`util::PackageKind`). The owner's picks: lucide `binary` for a bare executable,
        // `file-archive` for the AppImage (one file carrying a packed filesystem). The
        // Flatpak keeps `package`, which the owner was happy with.
        "application-x-executable-symbolic" => "binary",
        "application-x-appimage-symbolic" => "file-archive",
        // DRAGON-467's top-right group: the ticket named these three lucide glyphs directly
        // (`copy`, `share-2`, `upload`), so the mapping is one-to-one rather than
        // interpretive. `document-copy-symbolic` is the plain sheets-of-paper copy, distinct
        // from `clipboard-copy-symbolic` (a clipboard with an arrow), which the TOASTS keep.
        "document-copy-symbolic" => "copy",
        // DRAGON-478, the owner's picks: `share` (the box-with-an-arrow-out, the OS share
        // affordance on both mac and Windows) rather than `share-2` (the three-node graph,
        // which reads as social sharing), and `cloud-upload` rather than plain `upload`,
        // because the Upload button is the ACCOUNTS feature and the cloud says so.
        "share-symbolic" => "share",
        "upload-symbolic" => "cloud-upload",
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
        // The freehand tool (DRAGON-338). `pen-line`, not `pencil-line`, since DRAGON-475
        // (owner's pick): the same glyph minus the pencil crossbar, which read as clutter
        // at toolbar size.
        "pencil-symbolic" => "pen-line",
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
        "pan-up-symbolic" => "chevron-up", // vendored for DRAGON-630's first stepper; kept bundled
        // The picker value row's layout toggle (DRAGON-630): split channel boxes vs the
        // one whole-value box. Outward chevrons say "expand into channels", inward say
        // "collapse into one".
        "list-expand-symbolic" => "list-chevrons-up-down",
        "list-collapse-symbolic" => "list-chevrons-down-up",
        // Settings + misc.
        "folder-open-symbolic" => "folder-open",
        // The PHOTO ALBUM mark (DRAGON-485, the owner's pick): lucide `book-image`, a book with
        // a picture in it. Deliberately NOT `image` and NOT the folder mark: it sits beside both
        // in the cloud setup step's two tabs, where it has to say "this is an album, and an
        // album is not a folder" at a glance.
        "book-image-symbolic" => "book-image",
        // The MY FILES destination tab (DRAGON-522, the owner's pick): lucide `inbox`, a tray
        // things arrive in. It sits beside the Photos tab's `image` mark, and the pair reads as
        // "the file tree" and "the photo library" rather than as two flavours of folder. NOT
        // `folder-open`, which the rows and the breadcrumb below it already wear.
        "inbox-symbolic" => "inbox",
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
        // Cloud accounts (DRAGON-482). `cloud` is the settings PAGE's glyph, deliberately
        // the plain cloud rather than `cloud-upload` (which the preview editor's Upload
        // BUTTON already wears): the page is where accounts live, not an action.
        "cloud-symbolic" => "cloud",
        // The provider marks, from `res/icons/brands/` (see the module doc).
        "brand-gdrive-symbolic" => "brand-gdrive",
        "brand-onedrive-symbolic" => "brand-onedrive",
        "brand-dropbox-symbolic" => "brand-dropbox",
        "brand-proton-drive-symbolic" => "brand-proton-drive",
        "brand-icloud-symbolic" => "brand-icloud",
        // YouTube (DRAGON-493): the first video-hosting provider, alongside the four
        // file-storage marks above.
        "brand-youtube-symbolic" => "brand-youtube",
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

/// The bytes of a bundled SVG by its file stem. Exhaustive over every file [`lucide_name`]
/// can return; an unbundled stem is a programmer error (the two lists must stay in sync,
/// and the `every_mapped_name_embeds` test enforces it).
///
/// Two sources, one lookup: `svg!` embeds from the vendored Lucide set, `brand!` from our
/// own `res/icons/brands/` (see the module doc for why they are separate directories). A
/// `brand-` stem is the only thing that comes from the second one.
fn lucide_bytes(file: &str) -> &'static [u8] {
    macro_rules! svg {
        ($f:literal) => {
            include_bytes!(concat!("../../res/icons/lucide/", $f, ".svg")).as_slice()
        };
    }
    macro_rules! brand {
        ($f:literal) => {
            include_bytes!(concat!("../../res/icons/brands/", $f, ".svg")).as_slice()
        };
    }
    match file {
        "cloud" => svg!("cloud"),
        "package" => svg!("package"),
        "brand-gdrive" => brand!("gdrive"),
        "brand-onedrive" => brand!("onedrive"),
        "brand-dropbox" => brand!("dropbox"),
        "brand-proton-drive" => brand!("proton-drive"),
        "brand-icloud" => brand!("icloud"),
        "brand-youtube" => brand!("youtube"),
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
        "key" => svg!("key"),
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
        "pipette" => svg!("pipette"),
        "globe" => svg!("globe"),
        "binary" => svg!("binary"),
        "file-archive" => svg!("file-archive"),
        "apple" => svg!("apple"),
        "grid-2x2" => svg!("grid-2x2"),
        "share" => svg!("share"),
        "cloud-upload" => svg!("cloud-upload"),
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
        "pen-line" => svg!("pen-line"),
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
        "chevron-up" => svg!("chevron-up"),
        "list-chevrons-up-down" => svg!("list-chevrons-up-down"),
        "list-chevrons-down-up" => svg!("list-chevrons-down-up"),
        "folder-open" => svg!("folder-open"),
        "book-image" => svg!("book-image"),
        "inbox" => svg!("inbox"),
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
            "user-trash-symbolic",
            "window-close-symbolic", "process-stop-symbolic", "emblem-system-symbolic", "document-properties-symbolic",
            "preferences-system-symbolic", "audio-input-microphone-symbolic",
            "audio-x-generic-symbolic", "object-merge-symbolic",
            "audio-volume-high-symbolic", "notification-symbolic", "input-keyboard-symbolic",
            "view-refresh-symbolic", "scan-refresh-symbolic",
            "document-save-symbolic", "document-save-as-symbolic",
            "document-copy-symbolic", "share-symbolic", "upload-symbolic",
            "edit-copy-symbolic", "color-select-symbolic",
            "view-fullscreen-symbolic", "view-restore-symbolic",
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
            "video-x-generic-symbolic", "pan-down-symbolic", "pan-up-symbolic",
            "list-expand-symbolic", "list-collapse-symbolic",
            "folder-open-symbolic",
            // The cloud setup step's two destination tabs and its album rows (DRAGON-485, tab
            // marks added by DRAGON-522).
            "book-image-symbolic", "inbox-symbolic",
            "list-add-symbolic", "system-search-symbolic", "navbar-open-symbolic",
            "navbar-closed-symbolic", "accessories-screenshot-symbolic",
            "applications-multimedia-symbolic", "applications-graphics-symbolic",
            "image-x-generic-symbolic", "video-display-symbolic",
            "utilities-system-monitor-symbolic", "speedometer-symbolic", "donate-symbolic",
            "help-about-symbolic",
            // The About page's release-kind line, one glyph per `util::PackageKind`
            // (DRAGON-591 for the three Linux kinds, DRAGON-614 for macOS and Windows). All
            // five were missing from this list until DRAGON-614, so a half-vendored icon
            // would have shipped as a blank badge rather than failing here.
            // `util::package_kind_tests::each_package_kind_has_its_own_glyph` pins the other
            // side of the pairing, that each kind names a DIFFERENT one of these.
            "application-x-executable-symbolic", "application-x-appimage-symbolic",
            "package-x-generic-symbolic", "package-macos-symbolic", "package-windows-symbolic",
            "software-update-available-symbolic", "dialog-warning-symbolic",
            "dialog-error-symbolic",
            // Cloud accounts (DRAGON-482, plus YouTube DRAGON-493): the settings page's glyph
            // plus every provider mark. The provider ones also have to embed, or a provider row
            // renders blank off Linux. `cloud::registry_tests::every_provider_is_named_and_iconed`
            // asserts the registry side of the same pairing.
            "cloud-symbolic", "brand-gdrive-symbolic", "brand-onedrive-symbolic",
            "brand-dropbox-symbolic", "brand-proton-drive-symbolic", "brand-icloud-symbolic",
            "brand-youtube-symbolic",
        ];
        for n in names {
            let file = lucide_name(n).unwrap_or_else(|| panic!("`{n}` is unmapped"));
            assert!(!lucide_bytes(file).is_empty(), "`{file}` embeds no bytes");
            assert!(lucide_bytes(file).starts_with(b"<svg"), "`{file}` is not an SVG");
        }
    }

    /// **Every brand mark embeds, and embeds IN COLOUR** (DRAGON-482, plus YouTube DRAGON-493).
    ///
    /// The sibling of [`every_mapped_name_embeds`], for the half of the bundle that must NOT
    /// go through the symbolic path. It pins the three things that would each silently undo
    /// the marks: a brand name resolving to a tinted handle, a brand file drawn in
    /// `currentColor` (which has no colour of its own to render), and a file arriving with no
    /// record of where it came from.
    ///
    /// It also pins the NEGATIVE side. `cloud-symbolic` is the fallback two of the call sites
    /// pass for an account whose provider this build does not know, and it is a Lucide glyph:
    /// if it ever started resolving as a brand it would lose its tint and go invisible against
    /// a matching theme.
    #[test]
    fn brand_marks_resolve_to_their_own_colours() {
        let brands = [
            ("brand-gdrive-symbolic", "brand-gdrive"),
            ("brand-onedrive-symbolic", "brand-onedrive"),
            ("brand-dropbox-symbolic", "brand-dropbox"),
            ("brand-proton-drive-symbolic", "brand-proton-drive"),
            ("brand-icloud-symbolic", "brand-icloud"),
            ("brand-youtube-symbolic", "brand-youtube"),
        ];
        for (name, file) in brands {
            assert_eq!(brand_file(name), Some(file), "`{name}` must map to `{file}`");
            assert!(brand(name).is_some(), "`{name}` must resolve as a brand");
            assert!(
                !handle(name).symbolic,
                "`{name}` renders symbolic — the theme tint would flatten the brand's colours"
            );
            let svg = std::str::from_utf8(lucide_bytes(file)).expect("a UTF-8 SVG");
            assert!(svg.starts_with("<svg"), "`{file}` is not an SVG");
            assert!(
                !svg.contains("currentColor"),
                "`{file}` paints with currentColor, which has no colour of its own on the \
                 un-tinted path"
            );
            assert!(
                svg.contains("fill=\"#") || svg.contains("stop-color=\"#"),
                "`{file}` names no colour at all"
            );
            // Every entry is an official mark and must record where it came from.
            // (Proton was the one NEUTRAL exception from DRAGON-566 until the owner's
            // 2026-08-07 decision restored its official mark; see the module doc.)
            assert!(
                svg.contains("Source: https://"),
                "`{file}` must record where the official mark came from"
            );
            assert!(
                svg.contains("viewBox=\"0 0 24 24\""),
                "`{file}` must be a square 24x24 box, like every other bundled glyph"
            );
        }
        // A Lucide glyph is not a brand and must keep its tint.
        assert_eq!(brand_file("cloud-symbolic"), None);
        assert!(handle("cloud-symbolic").symbolic, "a Lucide glyph must stay symbolic");
        assert!(brand("emblem-ok-symbolic").is_none());
        // An unmapped name is not a brand either (it falls through to the system theme).
        assert!(brand("definitely-not-an-icon").is_none());
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
        assert_eq!(lucide_name("pencil-symbolic"), Some("pen-line")); // DRAGON-338 + DRAGON-475
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
        assert_eq!(lucide_name("scan-refresh-symbolic"), Some("refresh-cw"));
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
