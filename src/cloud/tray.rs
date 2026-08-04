//! The upload progress counter in the tray / menu bar (DRAGON-482).
//!
//! An upload runs in a DETACHED child (see [`super::child`]), so by the time the bytes are
//! moving there may be no window of ours left on screen at all. The counter is the whole
//! feedback that something is happening: a small number, 1 to 99, that appears when the
//! transfer starts and disappears when it finishes. That is the ticket's rule, and
//! [`counter`] is where it lives.
//!
//! # The seam
//!
//! [`UploadTray::start`] raises the surface, [`UploadTray::set_percent`] moves the number,
//! and DROPPING it takes the surface away. Three platform arms fill it in:
//!
//! | Platform | Surface | Body |
//! |---|---|---|
//! | Linux | this process's OWN `ksni` StatusNotifierItem, icon rendered per bucket | `platform/linux/upload_tray.rs` |
//! | macOS | an `NSStatusItem` whose button image is the SAME glyph, as a template image (DRAGON-495) | `platform/mac/services/upload_tray.rs` |
//! | Windows | a child-owned `Shell_NotifyIconW` behind a hidden message window | `platform/windows/upload_tray.rs` |
//! | anywhere else | nothing, honestly ([`backend`]'s stub) | here |
//!
//! **One tray item per upload CHILD, by design.** Two uploads running at once are two
//! processes, so they are two counters sitting side by side, each counting its own
//! transfer. The alternative (one aggregated item) would need a resident to own it and a
//! protocol to feed it, and it would report a number that belongs to no single upload. Two
//! small numbers say the true thing.
//!
//! # Identity, and why it is not the same rule on all three (DRAGON-516)
//!
//! For that design to hold, two children must never be mistaken for one item. Each platform
//! already reaches that, and by a DIFFERENT route, so the rule is written down here rather
//! than left implicit in three files:
//!
//! | Platform | What identifies the item | Per child because |
//! |---|---|---|
//! | Linux | the `ksni` item id, [`item_id`] | it interpolates the pid, and `ksni` registers its bus name as `org.kde.StatusNotifierItem-<pid>-<n>` besides |
//! | macOS | the `NSStatusItem` object | each child runs its own `NSApplication`, so there is nothing to share |
//! | Windows | the `(HWND, uID)` pair | each child creates its OWN hidden message window |
//!
//! **Windows keeps a FIXED `uID` on purpose**, and that is the one place where "make it
//! unique per child" would be actively wrong. The `(HWND, uID)` pair already separates two
//! children, because the window is per process. What a per-child `uID` would change is the
//! notification area's own bookkeeping: with no `guidItem`, the shell keys the user's
//! show-this-icon preference on the executable path plus the `uID`, so making it per-pid would
//! present every upload as an icon the user has never configured — and on a machine set to
//! hide unknown icons, the counter would go into the overflow chevron instead of the tray.
//! One stable `uID` means the user configures the upload counter ONCE and every later upload
//! inherits the choice. See `platform/windows/upload_tray.rs`.
//!
//! What did NOT work before this ticket was nothing about identity: an in-flight upload child
//! was being killed outright by a sibling capture's process sweep, so two counters could vanish
//! together for a reason that had nothing to do with the tray. That is `instance::UPLOAD_MARKER`.
//!
//! # Cancel (DRAGON-490)
//!
//! Every backend now carries exactly ONE extra control: a "Cancel upload" entry ([`CANCEL_LABEL`]).
//! This reverses an earlier, deliberate decision (the Linux backend's module doc used to say
//! outright that there would never be one, reasoning that a half-written remote file is worse
//! than waiting) — the owner asked for it anyway, so the tradeoff is now accepted rather than
//! avoided: a cancelled upload can leave a partial file at the provider, and [`super::providers`]
//! makes no attempt to clean one up (a nice-to-have this pass did not reach, not a safety
//! property depended on elsewhere). Choosing it sets the SAME `Arc<AtomicBool>`
//! `child::run_cloud_upload` already holds for the editor's cross-process cancel request, so
//! the provider's per-chunk check has exactly one flag to read regardless of which control
//! set it.
//!
//! # Decision here, syscall in the plugin
//!
//! The house split (CLAUDE.md). The BUCKETING ([`counter`]), the glyph geometry
//! ([`counter_svg`]) and the tooltip wording ([`tooltip`]) are pure and unit-tested on
//! every host, so the number the three platforms draw is decided ONCE and cannot drift.
//! Since DRAGON-495 all THREE platforms rasterize that same geometry: macOS used to set its
//! button's title to the number as text, which made it the one platform showing something
//! different from the other two. The remaining per-platform difference is colour alone, and
//! it is each platform's own convention (Linux and Windows recolour `#000` to the app accent;
//! macOS hands AppKit a template image, which the menu bar tints for light and dark).
//! The platform files do nothing but paint what they are handed, and now ALSO wire one
//! platform-native menu/click target to the `Arc<AtomicBool>` they are handed at [`start`] —
//! the ONE piece of this file that is necessarily unverified here (CLAUDE.md: GUI behaviour
//! is headless-unverifiable in this environment), so each backend's menu wiring follows that
//! platform's existing idioms as closely as possible rather than inventing a new one.
//!
//! # Privacy
//!
//! The label is the user's own account label and reaches the tooltip, which is a screen,
//! not a log. Nothing here logs the label, the file or the percentage.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

// The platform arm, under ONE name so the seam below carries no `cfg` at its call sites.
#[cfg(target_os = "linux")]
use crate::platform::linux::upload_tray as backend;
#[cfg(target_os = "macos")]
use crate::platform::mac::upload_tray as backend;
#[cfg(target_os = "windows")]
use crate::platform::windows::upload_tray as backend;

/// The honest no-surface arm for a platform with no tray of ours (a BSD build, a headless
/// run). [`UploadTray`] then costs one `None` and the upload is unaffected: the counter is
/// feedback, never a step of the transfer.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod backend {
    /// Nothing to hold: there is no surface on this platform.
    pub struct Item;

    /// Always `None`, so [`super::UploadTray`] never has anything to update or remove.
    pub fn start(
        _label: &str,
        _face: super::Face,
        _canceled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Option<Item> {
        None
    }

    impl Item {
        /// Unreachable (no item is ever minted here); present so the seam type-checks.
        pub fn set(&self, _label: &str, _face: super::Face) {}
    }
}

/// The tray/menu-bar Cancel entry's label. Pure; shared so all three backends (and any test
/// that wants to assert on it) draw from one wording rather than three hand-typed strings.
pub const CANCEL_LABEL: &str = "Cancel upload";

/// The tray item's identity for the upload child running as `pid` (DRAGON-516). Pure;
/// unit-tested.
///
/// Reverse-DNS like every other id this app publishes, with the pid as the last component so
/// two concurrent uploads are two items rather than one that a de-duplicating host collapses.
/// The recording tray can use a bare constant precisely because only one of it ever exists.
///
/// Shared rather than inlined at the one call site, so the "an upload child's item is keyed by
/// its pid" rule is stated where the module doc's identity table can point at it, and so the
/// characters it can produce are pinned by a test on every host. Linux is the only consumer
/// today (`ksni` is the one backend that takes an id string at all); macOS and Windows identify
/// their items structurally, for the reasons in the module doc.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub fn item_id(pid: u32) -> String {
    format!("dev.frosthaven.CosmicCaptureKit.Upload.{pid}")
}

/// How coarsely the number moves: it is re-drawn at multiples of this, never per percent.
///
/// A tray icon re-render is an SVG parse plus a rasterize (Linux, Windows), and a transfer
/// reports progress far more often than a 22-pixel glyph can show a difference. Five points
/// is about the finest step a two-digit number is worth re-drawing for.
pub const BUCKET_STEP: u8 = 5;

/// What NUMBER the counter should display at `percent`, or `None` when there is no honest
/// number left to show. Pure; unit-tested.
///
/// Two rules:
///
/// * **It counts 1 to 99.** A "0" badge reads as broken and a "100" badge is a thing that has
///   already finished, so the number is clamped into the range that means "working".
/// * **`None` at 100.** Every byte is acknowledged, but the OUTCOME has not arrived: the
///   provider still has to commit the upload and answer, and that answer can be a failure.
///   So there is nothing to draw, and the item keeps its last face until
///   [`UploadTray::finish`] replaces it with the tick or the cross.
///
/// It used to mean "take the item away", and that is what changed in the DRAGON-482 fix round
/// (F7). A counter that silently vanished said nothing about how the upload ENDED: a success
/// and a failure looked identical, and both looked like the tray had simply given up. The end
/// state is now shown for [`FINISH_HOLD`] and then removed.
///
/// The value is BUCKETED to [`BUCKET_STEP`] so the same input always yields the same
/// drawing, and so all three platforms show the same number at the same moment.
pub fn counter(percent: u8) -> Option<u8> {
    if percent >= 100 {
        return None;
    }
    Some((percent / BUCKET_STEP * BUCKET_STEP).clamp(1, 99))
}

/// Whether a raw `on_progress` value is genuine EVIDENCE that a transfer is reporting real
/// interim progress (DRAGON-490 dynamic follow-up), as opposed to the synthetic `0`
/// [`super::providers::upload`] calls once before dispatching to a provider, or the synthetic
/// `100` every provider calls once at the very end regardless of whether it ever had a real
/// midpoint (a true single-request upload, and a chunked one whose own first chunk covers the
/// whole file, both do this). Pure; unit-tested.
///
/// This is deliberately a PREDICATE over one raw value, not a threshold over a file's size:
/// the previous approach (`providers::reports_progress`, since removed) predicted the answer
/// upfront from each provider's own chunk size, which needed re-tuning for every new provider
/// and could drift from the upload path's actual behaviour. Watching what a transfer actually
/// DOES needs no such threshold and is automatically correct for any future provider.
pub fn is_genuine_progress(percent: u8) -> bool {
    percent != 0 && percent != 100
}

/// Whether a tray/session should STILL be showing the spinner, given whether it currently is
/// and the raw percent just reported. Pure; unit-tested.
///
/// Sticky in one direction only: once a genuine value has flipped this to `false`, it stays
/// `false` even if a later call happens to land on exactly 0 or 100 again (which should not
/// happen mid-transfer, but a transfer that has shown a real percentage must never revert to a
/// spinner over a stray edge value).
pub fn still_indeterminate(currently: bool, percent: u8) -> bool {
    currently && !is_genuine_progress(percent)
}

/// What the tray item is showing right now.
///
/// The seam type all three backends take, so "a percentage" and "the transfer ended" are one
/// vocabulary rather than a number plus a side channel. Every backend renders all three: two
/// raster [`face_svg`], and macOS draws [`face_title`] as menu-bar text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    /// A percentage, already bucketed by [`counter`], 1 to 99.
    Percent(u8),
    /// Transferring, but no genuine interim percentage has arrived yet (DRAGON-490 follow-up,
    /// dynamic detection — see [`still_indeterminate`]): every transfer starts here, and
    /// [`UploadTray::set_percent`] switches to [`Self::Percent`] the moment one does. A true
    /// single-request upload, or a chunked one whose own first chunk covers the whole file,
    /// simply never leaves this state. Drawn as lucide's `cloud-upload` outline (DRAGON-516)
    /// with no number under it, rather than a percentage that would either freeze or lie about
    /// progress. It is therefore the face MOST uploads wear for their whole life, which is why
    /// the owner noticed it was the one glyph in the app that was a filled blob rather than a
    /// lucide outline.
    Indeterminate,
    /// The upload finished and the provider confirmed it: a tick.
    Done,
    /// The upload did not finish: a cross.
    Failed,
}

/// How long the finished item stays on screen before it goes. See [`UploadTray::finish`].
///
/// Four seconds. Long enough that a user who was not looking at the tray at the exact instant
/// still sees the outcome, short enough that it is gone before it becomes clutter, and well
/// inside the desktop-notification window that carries the same news in words.
pub const FINISH_HOLD: std::time::Duration = std::time::Duration::from_secs(4);

const _: () = assert!(
    FINISH_HOLD.as_secs() >= 2 && FINISH_HOLD.as_secs() <= 10,
    "DRAGON-482: below this the end state is gone before it is noticed, above it the tray \
     keeps an icon for a transfer that is over"
);

/// The wire form of a [`Face`], for a seam that can only carry an integer. Pure; unit-tested.
///
/// The Windows backend posts the face to its own pump thread as a window message's `WPARAM`,
/// which is a number and nothing else. 1..=99 IS the percentage, so the other faces take the
/// codes just above it, which a percentage can never reach because [`counter`] clamps to 99.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
pub const FACE_DONE_CODE: u8 = 100;

/// The wire form of [`Face::Failed`]. See [`FACE_DONE_CODE`].
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
pub const FACE_FAILED_CODE: u8 = 101;

/// The wire form of [`Face::Indeterminate`] (DRAGON-490 follow-up). See [`FACE_DONE_CODE`].
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
pub const FACE_INDETERMINATE_CODE: u8 = 102;

// The three non-percentage codes have to sit above every percentage `counter` can produce,
// and stay distinct from each other, or two faces would decode to each other across the seam.
const _: () = assert!(
    FACE_DONE_CODE > 99
        && FACE_FAILED_CODE > 99
        && FACE_INDETERMINATE_CODE > 99
        && FACE_DONE_CODE != FACE_FAILED_CODE
        && FACE_DONE_CODE != FACE_INDETERMINATE_CODE
        && FACE_FAILED_CODE != FACE_INDETERMINATE_CODE,
    "DRAGON-482: a non-percentage face's wire code must not collide with a percentage or \
     another face"
);

/// Encode a face for the Windows message seam. Pure; unit-tested.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
pub fn face_code(face: Face) -> u8 {
    match face {
        Face::Percent(n) => n.min(99),
        Face::Indeterminate => FACE_INDETERMINATE_CODE,
        Face::Done => FACE_DONE_CODE,
        Face::Failed => FACE_FAILED_CODE,
    }
}

/// Decode a face from the Windows message seam. Pure; unit-tested.
///
/// Anything unrecognised reads as a percentage, clamped: this decodes a value that crossed a
/// `WPARAM`, and a face nobody can draw would be worse than a number that is merely wrong.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
pub fn face_from_code(code: u8) -> Face {
    match code {
        FACE_DONE_CODE => Face::Done,
        FACE_INDETERMINATE_CODE => Face::Indeterminate,
        FACE_FAILED_CODE => Face::Failed,
        n => Face::Percent(n.clamp(1, 99)),
    }
}

/// The counter's tooltip: which account this upload is going to, and where it has got to.
/// Pure; unit-tested.
///
/// The label leads, because a user with two uploads in flight is reading the tooltips to
/// tell the two counters apart. An empty label (an account connected before labels, or a
/// hand-edited file) degrades to a generic that still names the feature.
pub fn tooltip(label: &str, face: Face) -> String {
    let who = if label.trim().is_empty() { "your cloud account" } else { label.trim() };
    match face {
        Face::Percent(n) => format!("Uploading to {who} ({n}%)"),
        // No number: there is nothing honest to put after it (DRAGON-490 follow-up).
        Face::Indeterminate => format!("Uploading to {who}"),
        Face::Done => format!("Uploaded to {who}"),
        Face::Failed => format!("Upload to {who} failed"),
    }
}

/// The macOS menu-bar item's TITLE text for a face. Pure; unit-tested.
///
/// macOS draws text where the other two raster an icon, so the end states need characters
/// rather than geometry. The tick and cross are plain BMP symbols that every system font on
/// every supported macOS has; a coloured emoji would render at a size and baseline nothing
/// else in the menu bar uses.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub fn face_title(face: Face) -> String {
    match face {
        Face::Percent(n) => format!("{n}%"),
        // No digits to show (DRAGON-490 follow-up): a plain ellipsis reads as "working" in a
        // menu-bar title the same way it does anywhere else, without claiming a number.
        Face::Indeterminate => "\u{2026}".to_string(),
        Face::Done => "\u{2713}".to_string(),
        Face::Failed => "\u{2715}".to_string(),
    }
}

// ── The counter glyph (Linux + Windows raster it; macOS draws text) ───────────

/// The icon for a face, as a 24x24 SVG in `#000`. Pure; unit-tested.
///
/// One glyph per face, each owning the whole box: the seven-segment number
/// ([`counter_svg`]), the tick or the cross ([`mark_svg`]), or lucide's `cloud-upload`
/// outline ([`glyph_svg`]) while there is no honest percentage to show. They stopped
/// SHARING the box in DRAGON-495; this doc said "an upload cloud with the number under it"
/// until DRAGON-516, describing a layout two tickets dead.
///
/// `#000` throughout because that is the contract the two rasterizers already share: the
/// Linux tray's `render_icon` recolours `#000` to the app accent, and the Windows daemon's
/// tray SVG does the same substitution. So this glyph tints itself the same way the
/// recording icon does, with no second colour rule.
///
/// **The digits are SEGMENTS, not text.** `<text>` would need a font database loaded into
/// usvg, which means a system font scan in a process whose whole job is one upload, and it
/// would render NOTHING if the scan came back empty. Seven-segment digits are geometry: they
/// need no fonts, they are identical on every machine, and they stay legible at the 22-ish
/// pixels a panel actually gives an icon.
pub fn face_svg(face: Face) -> String {
    match face {
        Face::Percent(n) => counter_svg(n),
        Face::Indeterminate => glyph_svg(),
        Face::Done => mark_svg(TICK),
        Face::Failed => mark_svg(CROSS),
    }
}

/// The plain upload glyph alone, with no number/tick/cross under it (DRAGON-490 follow-up):
/// [`Face::Indeterminate`]'s icon, for a transfer with no real percentage to show. Since
/// DRAGON-516 it is the lucide `cloud-upload` mark ([`cloud_mark`]), drawn at the size of the
/// whole box.
///
/// The stroke attributes live on the GROUP rather than on each path, which is both how lucide
/// ships its own icons and what keeps ONE `#000` in the file for the accent substitution to
/// find (`platform/linux/tray.rs`'s `recolour`, and the identical `replace` in the Windows
/// arm).
fn glyph_svg() -> String {
    let mut out = String::with_capacity(512);
    out.push_str(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">"#,
    );
    out.push_str(
        r##"<g fill="none" stroke="#000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">"##,
    );
    out.push_str(cloud_mark());
    out.push_str("</g></svg>");
    out
}

/// The stroke geometry of the tick, as `(x1, y1, x2, y2)` bar centre lines in the same 24x24
/// box the digits use, across the WHOLE box (DRAGON-495).
const TICK: &[(f32, f32, f32, f32)] = &[(2.6, 13.0, 9.0, 19.6), (9.0, 19.6, 21.4, 4.6)];

/// The stroke geometry of the cross, same box.
const CROSS: &[(f32, f32, f32, f32)] = &[(3.6, 3.6, 20.4, 20.4), (20.4, 3.6, 3.6, 20.4)];

/// The end-state mark (tick or cross), alone in the box. Pure; unit-tested.
///
/// Strokes rather than a path with a stroke ATTRIBUTE, for the same reason the digits are
/// rects: every shape this file emits is a filled primitive, so a rasterizer that ignores
/// stroke properties (or a recolour pass that only rewrites `fill`) still draws it. Each
/// stroke is a thin rect rotated about its own start point, which is the shortest valid SVG
/// that produces a diagonal bar.
///
/// **The cloud is gone from here too** (DRAGON-495). It used to sit above these strokes, which
/// pinned them to the bottom half; the digits it shared the box with have taken the whole box,
/// so the tick and the cross take it as well and the item never changes scale as an upload
/// finishes.
fn mark_svg(strokes: &[(f32, f32, f32, f32)]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(768);
    out.push_str(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">"#,
    );
    out.push_str(r##"<g fill="#000">"##);
    // The bar thickness, scaled up with the mark now that it owns the full box.
    const T: f32 = 3.4;
    for (x1, y1, x2, y2) in strokes {
        let (dx, dy) = (x2 - x1, y2 - y1);
        let len = dx.hypot(dy);
        let angle = dy.atan2(dx).to_degrees();
        let _ = write!(
            out,
            r#"<rect x="{x1:.2}" y="{:.2}" width="{len:.2}" height="{T:.2}" rx="{:.2}" transform="rotate({angle:.2} {x1:.2} {y1:.2})"/>"#,
            y1 - T / 2.0,
            T / 2.0
        );
    }
    out.push_str("</g></svg>");
    out
}

/// The upload mark: lucide's `cloud-upload`, path data verbatim (DRAGON-516).
///
/// Three subpaths, which are lucide's own: the cloud OUTLINE (one arc sweeping up over the
/// top and a smaller one closing the right shoulder), the vertical shaft, and the chevron that
/// makes the shaft an up-arrow. Drawn in the shared 24-unit box at lucide's own coordinates,
/// so this is the icon, not an impression of it. The caller supplies the stroke
/// ([`glyph_svg`]); the geometry here carries none, exactly as lucide's own file does.
///
/// **It used to be a FILLED silhouette** — three overlapping `<circle>`s plus a rounded
/// `<rect>` for the cloud body and a `<polygon>` arrowhead above it — assembled here rather
/// than taken from lucide, on the reasoning (in this function's old doc) that "every shape here
/// is trivially valid SVG, which matters because nothing in this repo can look at the result".
/// The reasoning was sound and the premise was wrong: the recording tray's own icons
/// (`recording_ui::ICON_BORDER` and friends) have shipped a stroked `<path>` with elliptical
/// arc segments through this exact rasterizer since DRAGON-173, so arcs and strokes were
/// already proven here. What the hand-built version actually cost was a solid blob where every
/// other glyph in the app is a lucide outline, which is what the owner reported.
///
/// Two things had to move with it, and both are the kind of detail this comment exists for:
///
/// * The three fitted rasterizers now measure `abs_stroke_bounding_box()` instead of
///   `abs_bounding_box()`. usvg's plain bounding box excludes the stroke, so fitting this
///   glyph's ink by it would push half a stroke width — about 3px of a 64px pixmap — past the
///   cell and shave the round caps. A FILLED face measures identically either way, so the
///   digits, tick and cross are byte-identical.
/// * The stroke colour is `#000` like every fill here, because the accent substitution the two
///   rasterizing platforms share is a plain `replace("#000", …)` and does not care which
///   attribute it lands in.
fn cloud_mark() -> &'static str {
    concat!(
        // The shaft: down the centre, from under the cloud to the baseline.
        r#"<path d="M12 13v8"/>"#,
        // The cloud outline itself.
        r#"<path d="M4 14.899A7 7 0 1 1 15.71 8h1.79a4.5 4.5 0 0 1 2.5 8.242"/>"#,
        // The arrowhead, pointing up into the cloud.
        r#"<path d="m8 17 4-4 4 4"/>"#,
    )
}

/// The digit face for `n`: the NUMBER ALONE, filling the box. See [`face_svg`], which is the
/// entry point every backend uses.
///
/// **No cloud mark** (DRAGON-495, owner report). The number used to share the box with the
/// upload glyph above it, which left it half-height: at the ~22 physical pixels a panel gives a
/// tray item, two digits in the bottom half are around 8px tall, and the owner could not read
/// them. The glyph was saying what the tooltip already says and what a counter appearing in the
/// tray at all implies, so the digits took the space.
///
/// The geometry below is the whole of that change: [`Y`]/[`H`] span the box top to bottom
/// instead of its lower half, and the digit boxes are as wide as two of them fit.
pub fn counter_svg(n: u8) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">"#,
    );
    out.push_str(r##"<g fill="#000">"##);
    // The number, across the WHOLE box. One digit is centred and wider; two are spread to
    // the full width with a gap between them.
    //
    // A 1-unit margin all round rather than a flush 0..24: a tray rasterizer scales this into
    // a box it chooses, and a glyph whose ink touches the viewBox edge is the one that comes
    // back clipped on the platform that rounds down.
    const Y: f32 = 1.0;
    const H: f32 = 22.0;
    // Thicker with the digits (the old 1.9 was sized for an 11-unit digit); still under a
    // quarter of the height, which is what keeps a `0` from closing up into a filled block.
    const T: f32 = 3.2;
    // One digit's width when there are two, and the gap between them. The two digits plus the
    // gap plus a 1-unit margin each side are the whole 24, which the assert below pins: a
    // change to either width that stops filling the box is the bug this whole edit was.
    const W2: f32 = 10.4;
    const GAP: f32 = 1.2;
    const _: () = assert!(
        2.0 + W2 + GAP + W2 <= 24.0 && 2.0 + W2 + GAP + W2 > 23.9,
        "two digits plus their gap and margins must fill the 24-unit icon box"
    );
    // A lone digit is wider (there is room) but not full width: a seven-segment glyph much
    // past this reads as a rectangle rather than as a number.
    const W1: f32 = 13.0;
    let n = n.min(99);
    if n >= 10 {
        digit_rects(n / 10, 1.0, Y, W2, H, T, &mut out);
        digit_rects(n % 10, 1.0 + W2 + GAP, Y, W2, H, T, &mut out);
    } else {
        digit_rects(n, (24.0 - W1) / 2.0, Y, W1, H, T, &mut out);
    }
    out.push_str("</g></svg>");
    out
}

/// Which of the seven segments each digit lights, in the classic `a b c d e f g` order
/// (top, top-right, bottom-right, bottom, bottom-left, top-left, middle).
const SEGMENTS: [[bool; 7]; 10] = [
    [true, true, true, true, true, true, false],    // 0
    [false, true, true, false, false, false, false], // 1
    [true, true, false, true, true, false, true],   // 2
    [true, true, true, true, false, false, true],   // 3
    [false, true, true, false, false, true, true],  // 4
    [true, false, true, true, false, true, true],   // 5
    [true, false, true, true, true, true, true],    // 6
    [true, true, true, false, false, false, false], // 7
    [true, true, true, true, true, true, true],     // 8
    [true, true, true, true, false, true, true],    // 9
];

/// Append one seven-segment digit's rects to `out`, in a `w` by `h` box at (`x`, `y`) with
/// stroke thickness `t`. Pure geometry; the caller owns placement.
fn digit_rects(d: u8, x: f32, y: f32, w: f32, h: f32, t: f32, out: &mut String) {
    let lit = SEGMENTS[(d % 10) as usize];
    let half = (h + t) / 2.0;
    let mid = y + (h - t) / 2.0;
    // (x, y, w, h) per segment, in the a b c d e f g order of `SEGMENTS`.
    let boxes = [
        (x, y, w, t),                  // a: top
        (x + w - t, y, t, half),       // b: top-right
        (x + w - t, mid, t, half),     // c: bottom-right
        (x, y + h - t, w, t),          // d: bottom
        (x, mid, t, half),             // e: bottom-left
        (x, y, t, half),               // f: top-left
        (x, mid, w, t),                // g: middle
    ];
    for (on, (bx, by, bw, bh)) in lit.into_iter().zip(boxes) {
        if on {
            use std::fmt::Write as _;
            let _ = write!(
                out,
                r#"<rect x="{bx:.2}" y="{by:.2}" width="{bw:.2}" height="{bh:.2}"/>"#
            );
        }
    }
}

// ── Rasterizing a face into a tray cell ──────────────────────────────────────

/// The share of a tray cell's edge left as MARGIN when a face is fitted into it
/// ([`icon_ink_fit`]): 3%, so a 64px pixmap keeps about two pixels of air on each side.
///
/// Small on purpose. A panel already places its items with their own spacing, so the margin
/// here exists only to keep a rounded corner or an antialiased edge off the very last pixel
/// row, which is where a host that rounds a scale down would eat it. The Windows notification
/// area asks for slightly more (`platform/windows/daemon.rs` uses 6.25%), which is why this is
/// an argument there rather than a hard-coded value.
pub const ICON_INK_MARGIN: f32 = 0.03;

/// How to draw a face's INK so it fills the tray cell it is given: the uniform scale, and the
/// x / y offsets that centre it. Pure; unit-tested.
///
/// **Why fitting the INK and not the viewBox** (DRAGON-500). Every face here shares one 24-unit
/// box, but they do not fill the same amount of it: the digits span nearly all of it (`Y` 1 to
/// 23), the tick spans 4.6 to 19.6, and [`Face::Indeterminate`]'s lone cloud mark covers barely
/// two fifths of it. Rasterizing the BOX 1:1 therefore draws each face at a different visual
/// weight, and draws the smallest of them at about 40% of the cell the panel gave it, which is
/// the "the upload icon is tiny" the owner reported on the COSMIC panel. Windows already
/// learned this the same way and fixed it the same way for its recording icon (DRAGON-249: an
/// icon that does not fill its cell reads as broken next to neighbours that do); this is that
/// lesson, generalized, so the artwork does not have to be redrawn per face to carry it.
///
/// Uniform scale on the LONGER extent, with the shorter one centred, so nothing is stretched
/// and a face's own proportions survive. `ink` is `(x, y, w, h)` in viewBox units, the ink
/// bounding box a rasterizer can ask its parsed tree for; `side` is the viewBox edge and `px`
/// the pixmap edge. A DEGENERATE ink box (an empty or unparsed glyph) falls back to the plain
/// 1:1 box fit, which is what every caller did before this existed: a face with nothing in it
/// should draw nothing, never divide by zero or scale to infinity.
///
/// The result is meant for `resvg::tiny_skia::Transform::from_row(scale, 0, 0, scale, dx, dy)`.
pub fn icon_ink_fit(ink: (f32, f32, f32, f32), side: f32, px: f32, margin: f32) -> (f32, f32, f32) {
    let (x, y, w, h) = ink;
    let longest = w.max(h);
    // `is_finite` first, so a NaN out of a degenerate parse takes the fallback rather than
    // sliding through a comparison that is false for every operand.
    if !longest.is_finite() || longest <= 0.0 || !side.is_finite() || side <= 0.0 {
        return (px / side.max(1.0), 0.0, 0.0);
    }
    let scale = (px - 2.0 * px * margin) / longest;
    ((scale), (px - w * scale) / 2.0 - x * scale, (px - h * scale) / 2.0 - y * scale)
}

// ── The live counter ─────────────────────────────────────────────────────────

/// A live upload counter. Raise it when the transfer starts, feed it the percentages, and
/// drop it (or let it reach 100) when the transfer ends.
///
/// Not `Send`: the platform surfaces are main-thread-bound (`NSStatusItem`) or own their own
/// thread (`ksni`, the Windows message window), and the upload child drives this from the one
/// thread it has.
pub struct UploadTray {
    /// The platform surface, or `None` when there is none to raise (no host, no tray, an
    /// unsupported platform) or when the counter has finished and taken itself away.
    backing: Option<backend::Item>,
    /// The account label, kept for the tooltip on every update.
    label: String,
    /// The number currently drawn, so an unchanged bucket costs no re-render. Always `None`
    /// while [`Self::indeterminate`] is set: there is no bucket to compare against.
    shown: Option<u8>,
    /// Whether this transfer is STILL showing the spinner rather than a real percentage
    /// (DRAGON-490 dynamic follow-up). Every upload starts `true`; [`Self::set_percent`] is
    /// the only place it ever flips, and only forward — see [`still_indeterminate`].
    indeterminate: bool,
}

impl UploadTray {
    /// Raise the counter for an upload to `label`, always starting as the spinner
    /// ([`Face::Indeterminate`]). Always returns a value: a platform with no surface (or a
    /// desktop with no StatusNotifierItem host) yields a counter that simply draws nothing, so
    /// no caller has to branch on whether a tray exists.
    ///
    /// `canceled` (DRAGON-490) is the SAME flag `child::run_cloud_upload` checks between
    /// chunks; the backend wires its own Cancel entry to set it directly (same process, no
    /// IPC needed for this trigger), the mirror of the editor's cross-process one, which sets
    /// it through `session::cancel_requested`'s poll instead.
    ///
    /// # Why every upload starts as the spinner, unconditionally (DRAGON-490 follow-up)
    ///
    /// This used to seed [`Face::Percent`] or [`Face::Indeterminate`] from a caller-supplied
    /// flag, itself predicted upfront from the file's size (`providers::reports_progress`).
    /// The owner asked for that to be OBSERVED instead of predicted: start every transfer as
    /// the spinner and let [`Self::set_percent`] switch to a real number the moment one
    /// genuinely arrives, which needs no per-provider byte threshold at all and is
    /// automatically correct for a future provider nobody has tuned a threshold for yet.
    pub fn start(label: &str, canceled: Arc<AtomicBool>) -> Self {
        UploadTray {
            backing: backend::start(label, Face::Indeterminate, canceled),
            label: label.to_string(),
            shown: None,
            indeterminate: true,
        }
    }

    /// Whether this transfer is still showing the spinner rather than a real percentage.
    /// Read by `child::run_cloud_upload` to decide whether the SAME switch should apply to
    /// the cross-process session marker: one flag, read by both surfaces, so the tray and the
    /// titlebar can never disagree about which state a transfer is in.
    pub fn is_indeterminate(&self) -> bool {
        self.indeterminate
    }

    /// Move the counter to `percent` (0-100). Re-draws only when the bucket changes.
    ///
    /// At 100 there is no new number to draw and the item is LEFT as it is: the outcome has
    /// not arrived yet, and [`Self::finish`] is what replaces the face with the tick or the
    /// cross. Removing the item here (the previous behaviour) meant a success and a failure
    /// looked exactly alike, and both looked like the tray giving up.
    ///
    /// **The spinner-to-percentage switch** (DRAGON-490 dynamic follow-up) happens HERE, via
    /// [`still_indeterminate`]: every call while still indeterminate is checked for whether it
    /// is genuine evidence of a real midpoint, and the moment one arrives this draws it
    /// immediately rather than waiting for a later call — the first genuine value IS the first
    /// thing worth showing.
    pub fn set_percent(&mut self, percent: u8) {
        self.indeterminate = still_indeterminate(self.indeterminate, percent);
        if self.indeterminate {
            return;
        }
        let Some(next) = counter(percent) else { return };
        if Some(next) == self.shown {
            return;
        }
        if let Some(item) = &self.backing {
            item.set(&self.label, Face::Percent(next));
        }
        self.shown = Some(next);
    }

    /// Show how the upload ENDED, hold it for [`FINISH_HOLD`], then take the item away.
    ///
    /// Takes `self` by value, so the removal is the drop at the end of this function and
    /// there is no state in which a finished counter can be fed another percentage.
    ///
    /// **This BLOCKS for the hold**, and that is the whole design rather than an oversight.
    /// The only caller is the detached upload child (`cloud::child`), whose remaining work
    /// after this point is posting one desktop notification; four seconds there costs nobody
    /// anything, and the child's own 30-minute backstop still bounds it. A timer thread would
    /// have to outlive the item it is holding, which in a process that is about to exit means
    /// keeping the whole process alive for the timer, which is strictly worse.
    /// A CANCEL shows nothing and holds for nothing: the item simply goes (DRAGON-495). See
    /// [`finish_face`].
    pub fn finish(self, ending: Ending) {
        let Some(item) = &self.backing else { return };
        let Some(face) = finish_face(ending) else {
            // `self` drops here, immediately: nothing to say, nothing to hold.
            return;
        };
        item.set(&self.label, face);
        std::thread::sleep(FINISH_HOLD);
        // `self` drops here, and every backend's `Drop` removes the item.
    }
}

/// How an upload ended, for the tray.
///
/// Three states rather than `finish(success: bool)` (DRAGON-495): a CANCEL is neither. It is
/// not a success, and drawing it as a failure would put a red cross in the tray for something
/// the user themselves asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// The file is at the provider.
    Done,
    /// The upload failed.
    Failed,
    /// The user cancelled it, from this tray's own menu or from the editor.
    Canceled,
}

/// The face the tray holds after an upload ends, or `None` to take the item away at once.
/// Pure; unit-tested.
///
/// `None` for a cancel, and that is the whole of the DRAGON-495 change here: the hold exists
/// so an upload does not simply vanish without saying how it went, but a user who just pressed
/// Cancel already knows how it went. Holding a mark there would make them watch the answer to
/// their own question for four seconds.
pub fn finish_face(ending: Ending) -> Option<Face> {
    match ending {
        Ending::Done => Some(Face::Done),
        Ending::Failed => Some(Face::Failed),
        Ending::Canceled => None,
    }
}

#[cfg(test)]
mod counter_tests {
    use super::*;

    /// **Two concurrent uploads are two items** (DRAGON-516). The id is what a
    /// StatusNotifierItem host de-duplicates on, so two children must never produce the same
    /// string, and one child must produce the SAME string every time it is asked (a host
    /// re-reads the property).
    #[test]
    fn the_item_id_is_one_per_child_and_stable_within_it() {
        assert_ne!(item_id(1234), item_id(1235), "two children, two items");
        assert_eq!(item_id(1234), item_id(1234), "one child, one item, however often asked");
        assert!(item_id(1234).ends_with(".1234"), "the pid is what makes it per child");
        assert!(item_id(1).starts_with("dev.frosthaven.CosmicCaptureKit."), "{}", item_id(1));
        // A D-Bus-shaped name: dot-separated elements of `[A-Za-z0-9_]`, nothing that would
        // need escaping in a bus property or a window class name.
        for pid in [0u32, 1, 4242, u32::MAX] {
            let id = item_id(pid);
            assert!(
                id.split('.').all(|e| !e.is_empty()
                    && e.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')),
                "{id} is not a plain dotted name"
            );
        }
    }

    /// The Cancel entry's wording (DRAGON-490): a whole, readable label, no dash, matching
    /// this app's house rule on every other user-facing string.
    #[test]
    fn the_cancel_label_reads_as_a_plain_control_name() {
        assert_eq!(CANCEL_LABEL, "Cancel upload");
        assert!(!CANCEL_LABEL.contains('\u{2014}') && !CANCEL_LABEL.contains('\u{2013}'));
    }

    /// The ticket's rule, spelled out: 1 to 99 while it works, no number at 100.
    #[test]
    fn the_counter_shows_one_to_ninety_nine_and_stops_at_a_hundred() {
        assert_eq!(counter(0), Some(1), "a fresh upload says it is working, not zero");
        assert_eq!(counter(100), None, "there is no honest number once every byte is in");
        // Nothing above 100 produces one either (a provider that overshoots, a rounding
        // error in a chunked transfer).
        for p in [101u8, 150, 200, 255] {
            assert_eq!(counter(p), None, "{p} has no number either");
        }
        for p in 0..100u8 {
            let n = counter(p).expect("still working");
            assert!((1..=99).contains(&n), "{p} drew {n}, outside 1..=99");
        }
    }

    /// **DRAGON-490 dynamic follow-up.** The two synthetic values every provider calls around
    /// a single-request transfer (`providers::upload`'s own leading 0, and the trailing 100
    /// every provider calls at the end regardless) are NOT evidence; everything else is.
    #[test]
    fn only_the_two_synthetic_values_are_not_genuine_progress() {
        assert!(!is_genuine_progress(0));
        assert!(!is_genuine_progress(100));
        for p in 1..100u8 {
            assert!(is_genuine_progress(p), "{p} should read as real progress");
        }
    }

    /// **The whole state machine, spelled out.** Starts indeterminate, a synthetic value keeps
    /// it that way, one genuine value flips it, and nothing flips it back — including a later
    /// call that happens to land on exactly 0 or 100 again.
    #[test]
    fn indeterminate_flips_once_on_the_first_genuine_value_and_never_back() {
        // A synthetic value never flips it.
        assert!(still_indeterminate(true, 0));
        assert!(still_indeterminate(true, 100));
        // A genuine value flips it immediately.
        assert!(!still_indeterminate(true, 1));
        assert!(!still_indeterminate(true, 40));
        assert!(!still_indeterminate(true, 99));
        // Already determinate: stays that way regardless of what arrives next, including the
        // exact synthetic values that would have kept it indeterminate from the start.
        for p in [0u8, 1, 50, 99, 100] {
            assert!(!still_indeterminate(false, p), "{p} must not revert to indeterminate");
        }
    }

    /// It never goes backwards, and it lands on the bucket step. Both matter for the same
    /// reason: the number is re-drawn only when it CHANGES, so a non-monotonic bucketing
    /// would flicker between two icons on a steady transfer.
    #[test]
    fn buckets_are_monotonic_and_land_on_the_step() {
        let mut last = 0u8;
        for p in 0..100u8 {
            let n = counter(p).expect("still working");
            assert!(n >= last, "{p} went backwards: {last} then {n}");
            last = n;
            assert!(
                n == 1 || n.is_multiple_of(BUCKET_STEP),
                "{p} drew {n}, not a step of {BUCKET_STEP}"
            );
        }
        // The step really coalesces: a whole bucket of percentages draws one number.
        assert_eq!(counter(45), counter(46));
        assert_eq!(counter(46), counter(49));
        assert_ne!(counter(49), counter(50));
        // And the top of the range is the last NUMBER shown before the end state.
        assert_eq!(counter(99), Some(95));
    }

    /// The tooltip names the account, because two concurrent uploads are two counters and
    /// the tooltip is how a user tells them apart.
    #[test]
    fn the_tooltip_names_the_account_and_the_progress() {
        let pct = |n| tooltip("Work Drive", Face::Percent(n));
        assert_eq!(pct(40), "Uploading to Work Drive (40%)");
        // An empty or whitespace label still says something useful.
        assert_eq!(tooltip("", Face::Percent(5)), "Uploading to your cloud account (5%)");
        assert_eq!(tooltip("   ", Face::Percent(5)), "Uploading to your cloud account (5%)");
        // Surrounding whitespace in a real label is trimmed, not rendered.
        assert_eq!(tooltip("  Work  ", Face::Percent(7)), "Uploading to Work (7%)");
        // The END states get their own sentence, in the past tense, and a failure never
        // reads as a success. The same rule the desktop banner keeps.
        assert_eq!(tooltip("Work Drive", Face::Done), "Uploaded to Work Drive");
        assert_eq!(tooltip("Work Drive", Face::Failed), "Upload to Work Drive failed");
        assert!(!tooltip("W", Face::Failed).contains("Uploaded"));
        assert_eq!(tooltip("", Face::Done), "Uploaded to your cloud account");
        // The house rule on user-facing strings.
        for t in [pct(40), tooltip("", Face::Percent(1)), tooltip("W", Face::Done)] {
            assert!(!t.contains('\u{2014}') && !t.contains('\u{2013}'), "dash in {t:?}");
        }
    }

    /// **DRAGON-490 follow-up.** [`Face::Indeterminate`] reads as "still working" everywhere,
    /// but never claims a percentage it does not have.
    #[test]
    fn the_indeterminate_face_names_no_percentage() {
        assert_eq!(tooltip("Work Drive", Face::Indeterminate), "Uploading to Work Drive");
        assert_eq!(tooltip("", Face::Indeterminate), "Uploading to your cloud account");
        let title = face_title(Face::Indeterminate);
        assert!(!title.contains('%') && !title.chars().any(|c| c.is_ascii_digit()), "{title}");
        assert!(!title.is_empty());
        // It is the plain upload glyph (no digits under it), not a blank icon.
        assert_ne!(face_svg(Face::Indeterminate), face_svg(Face::Percent(1)));
        assert!(!face_svg(Face::Indeterminate).is_empty());
    }

    /// **The end state is what the user actually reads** (DRAGON-482 F7). The counter used to
    /// vanish at 100, which looked the same whether the upload succeeded, failed, or the
    /// child died: a tick and a cross are the difference, and they are held long enough to be
    /// seen.
    #[test]
    fn a_finished_upload_shows_its_outcome_before_it_goes() {
        assert_ne!(face_title(Face::Done), face_title(Face::Failed));
        assert_ne!(face_svg(Face::Done), face_svg(Face::Failed));
        // The macOS text is a symbol, not a percentage, and carries no digits to be misread
        // as one.
        for face in [Face::Done, Face::Failed] {
            let title = face_title(face);
            assert!(!title.contains('%'), "{face:?} still reads as progress: {title}");
            assert!(!title.chars().any(|c| c.is_ascii_digit()), "{title}");
            assert!(!title.is_empty());
        }
        assert_eq!(face_title(Face::Percent(40)), "40%");
        // Long enough to be noticed, short enough not to become clutter.
        assert!(FINISH_HOLD >= std::time::Duration::from_secs(2));
        assert!(FINISH_HOLD <= std::time::Duration::from_secs(10));
    }

    /// The Windows seam carries the face as a number, so the round trip has to be exact for
    /// every value the seam can see. A percentage can never collide with an end state because
    /// `counter` clamps to 99.
    #[test]
    fn a_face_survives_the_windows_message_seam() {
        for face in [
            Face::Percent(1),
            Face::Percent(40),
            Face::Percent(99),
            Face::Indeterminate,
            Face::Done,
            Face::Failed,
        ] {
            assert_eq!(face_from_code(face_code(face)), face, "{face:?} did not round trip");
        }
        // A value nobody sends still decodes to something drawable rather than panicking.
        assert_eq!(face_from_code(0), Face::Percent(1));
        assert_eq!(face_from_code(255), Face::Percent(99));
    }
}

#[cfg(test)]
mod glyph_tests {
    use super::*;

    /// **The one that matters**: every number the counter can show must PARSE as SVG. The
    /// two rasterizing platforms hand this straight to usvg, and a glyph that fails to parse
    /// is an upload with no visible progress at all.
    #[test]
    fn every_counter_value_parses_as_an_svg() {
        for n in 1..=99u8 {
            let svg = counter_svg(n);
            assert!(
                resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default())
                    .is_ok(),
                "the glyph for {n} does not parse: {svg}"
            );
        }
    }

    /// The digits are drawn, not written: no `<text>` anywhere, because a font database is
    /// exactly what this icon must not need.
    #[test]
    fn the_glyph_needs_no_font() {
        let svg = counter_svg(42);
        assert!(!svg.contains("<text"), "the counter must not depend on a font stack");
        assert!(!svg.contains("font"), "the counter must not depend on a font stack");
    }

    /// **DRAGON-490 follow-up.** [`Face::Indeterminate`]'s glyph is a standalone mark like
    /// every other face's icon: it must parse, need no font, and recolour the same way.
    #[test]
    fn the_indeterminate_glyph_parses_and_recolours() {
        let svg = face_svg(Face::Indeterminate);
        assert!(
            resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default()).is_ok(),
            "the indeterminate glyph does not parse: {svg}"
        );
        assert!(!svg.contains("<text") && !svg.contains("font"));
        assert!(svg.contains("#000"), "the accent substitution keys on #000");
        assert_eq!(
            svg.matches("#000").count(),
            1,
            "one colour for the whole mark, so the recolour is one substitution"
        );
        assert!(svg.contains(r#"viewBox="0 0 24 24""#), "the shared 24-unit icon box");
    }

    /// **DRAGON-516, the owner's report: it must be lucide's `cloud-upload`.**
    ///
    /// An OUTLINE cloud with an up arrow, not the filled silhouette this used to hand-assemble
    /// from circles and a polygon. The assertions are about the two things that make it that
    /// icon rather than a lookalike: the drawing MODE (stroked, nothing filled) and lucide's
    /// own path data, verbatim.
    ///
    /// The path strings are pinned literally on purpose. They are copied artwork, so the useful
    /// failure is "somebody redrew it by hand again", and only comparing the real coordinates
    /// can say that.
    #[test]
    fn the_indeterminate_glyph_is_the_lucide_cloud_upload_outline() {
        let svg = face_svg(Face::Indeterminate);
        // Stroked, not filled: `fill="none"` plus a stroke is what an outline icon IS, and the
        // filled primitives the old mark was built from must be gone.
        assert!(svg.contains(r##"fill="none""##), "an outline cloud fills nothing: {svg}");
        assert!(svg.contains(r##"stroke="#000""##), "the mark is drawn as a stroke: {svg}");
        assert!(!svg.contains("<circle"), "the filled cloud body survives: {svg}");
        assert!(!svg.contains("<polygon"), "the filled arrowhead survives: {svg}");
        assert!(!svg.contains("<rect"), "the filled cloud/shaft rects survive: {svg}");
        // Lucide's own geometry, all three subpaths.
        assert!(svg.contains(r#"d="M12 13v8""#), "the arrow's shaft: {svg}");
        assert!(
            svg.contains(r#"d="M4 14.899A7 7 0 1 1 15.71 8h1.79a4.5 4.5 0 0 1 2.5 8.242""#),
            "the cloud outline: {svg}"
        );
        assert!(svg.contains(r#"d="m8 17 4-4 4 4""#), "the arrowhead chevron: {svg}");
        // Lucide's stroke presentation, so it sits at the same weight as every other lucide
        // glyph in the app rather than as a heavier or lighter cousin.
        assert!(svg.contains(r#"stroke-width="2""#));
        assert!(svg.contains(r#"stroke-linecap="round""#));
        assert!(svg.contains(r#"stroke-linejoin="round""#));
    }

    /// **The stroked glyph really fits its cell** (DRAGON-516). usvg's plain bounding box
    /// excludes the stroke, so fitting by it would push half the outline's width outside the
    /// pixmap and shave the round caps; the three rasterizers measure
    /// `abs_stroke_bounding_box()` instead.
    ///
    /// Asserted HERE, against the real parsed tree, rather than in one platform file: all three
    /// arms feed the same numbers into the same [`icon_ink_fit`], so the property is the shared
    /// artwork's, not any one panel's. (The Linux arm additionally proves it end to end on the
    /// rendered ALPHA, which is the only thing a panel ever sees.)
    #[test]
    fn fitting_the_stroked_glyph_keeps_it_inside_the_cell() {
        const SIDE: f32 = 24.0;
        const PX: f32 = 64.0;
        let svg = face_svg(Face::Indeterminate);
        let tree =
            resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default())
                .expect("the glyph parses");
        let plain = tree.root().abs_bounding_box();
        let stroked = tree.root().abs_stroke_bounding_box();
        // The two really differ for this face, which is the whole reason the rasterizers had
        // to change; a test that passed with either box would prove nothing.
        assert!(
            stroked.width() > plain.width() && stroked.height() > plain.height(),
            "a stroked glyph must measure larger with its outline counted: {plain:?} {stroked:?}"
        );
        // Fitted by the STROKE box, every drawn pixel lands inside the pixmap.
        let (scale, dx, dy) =
            icon_ink_fit((stroked.x(), stroked.y(), stroked.width(), stroked.height()), SIDE, PX, ICON_INK_MARGIN);
        for (lo, hi) in [
            (stroked.x() * scale + dx, (stroked.x() + stroked.width()) * scale + dx),
            (stroked.y() * scale + dy, (stroked.y() + stroked.height()) * scale + dy),
        ] {
            assert!(lo >= -0.01 && hi <= PX + 0.01, "the fitted glyph spilled out of the cell");
        }
        // And the OLD measurement would have spilled, which is the bug this pins.
        let (bad_scale, bad_dx, _) =
            icon_ink_fit((plain.x(), plain.y(), plain.width(), plain.height()), SIDE, PX, ICON_INK_MARGIN);
        let spilled = (stroked.x() * bad_scale + bad_dx) < -0.5
            || ((stroked.x() + stroked.width()) * bad_scale + bad_dx) > PX + 0.5;
        assert!(spilled, "the plain-box fit must be the one that clips, or this test is vacuous");
    }

    /// A FILLED face measures identically with or without its stroke counted, so the
    /// DRAGON-516 rasterizer change left the digits, the tick and the cross byte-identical.
    /// This is what makes that a safe edit to three platform files at once.
    #[test]
    fn a_filled_face_measures_the_same_either_way() {
        for face in [Face::Percent(7), Face::Percent(88), Face::Done, Face::Failed] {
            let svg = face_svg(face);
            let tree =
                resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default())
                    .expect("the face parses");
            let plain = tree.root().abs_bounding_box();
            let stroked = tree.root().abs_stroke_bounding_box();
            assert_eq!(
                (plain.x(), plain.y(), plain.width(), plain.height()),
                (stroked.x(), stroked.y(), stroked.width(), stroked.height()),
                "{face:?} is filled, so counting a stroke it does not have must change nothing"
            );
        }
    }

    /// Recolouring is what tints the icon to the app accent on both rasterizing platforms,
    /// and it works by replacing `#000`. A glyph that named its colour any other way would
    /// come out black on a dark panel.
    #[test]
    fn the_glyph_is_black_so_the_accent_substitution_reaches_it() {
        let svg = counter_svg(7);
        assert!(svg.contains("#000"), "the accent substitution keys on #000");
        assert_eq!(svg.matches("#000").count(), 1, "one fill for the whole mark");
        assert!(svg.contains(r#"viewBox="0 0 24 24""#), "the shared 24-unit icon box");
    }

    /// One digit versus two is a layout difference, so the two must really differ, and a
    /// two-digit number must place its digits apart rather than on top of each other.
    #[test]
    fn one_and_two_digit_numbers_are_laid_out_differently() {
        let one = counter_svg(7);
        let two = counter_svg(70);
        assert_ne!(one, two);
        // 8 lights every segment, so a two-digit 88 has exactly seven rects more than a
        // single 8 (nothing else contributes a rect since DRAGON-495 took the cloud away).
        let rects = |s: &str| s.matches("<rect").count();
        assert_eq!(rects(&counter_svg(88)) - rects(&counter_svg(8)), 7);
        // 1 lights two segments, 8 lights seven: the map is really being read.
        assert_eq!(rects(&counter_svg(8)) - rects(&counter_svg(1)), 5);
    }

    /// The bounding box of every `<rect>` in `svg`, as `(x0, y0, x1, y1)`.
    ///
    /// Only meaningful for the DIGIT faces, whose rects are axis-aligned and untransformed;
    /// the tick and the cross rotate theirs about their own start points, so their drawn
    /// extent is not the rect's own extent.
    fn ink_bounds(svg: &str) -> (f32, f32, f32, f32) {
        let attr = |tag: &str, name: &str| -> f32 {
            let key = format!("{name}=\"");
            let start = tag.find(&key).map(|i| i + key.len()).expect("the attribute is present");
            let rest = &tag[start..];
            let end = rest.find('"').expect("the attribute is closed");
            rest[..end].parse::<f32>().expect("a number")
        };
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for chunk in svg.split("<rect").skip(1) {
            let tag = &chunk[..chunk.find("/>").expect("the rect is closed")];
            let (x, y) = (attr(tag, "x"), attr(tag, "y"));
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + attr(tag, "width"));
            y1 = y1.max(y + attr(tag, "height"));
        }
        (x0, y0, x1, y1)
    }

    /// **DRAGON-495: the number owns the whole icon.** It used to share the box with the upload
    /// cloud, which left two digits about eight physical pixels tall in a panel and, as the
    /// owner reported, unreadable. The cloud's own shapes must be gone from the counter, and the
    /// digits' ink must reach every edge of the 24-unit box (bar the 1-unit safety margin).
    #[test]
    fn the_counter_is_digits_alone_filling_the_box() {
        let svg = counter_svg(88);
        assert!(!svg.contains("<circle"), "the cloud mark survives in the counter");
        assert!(!svg.contains("<polygon"), "the cloud's arrow survives in the counter");
        let (x0, y0, x1, y1) = ink_bounds(&svg);
        assert!(x0 <= 1.0 && x1 >= 23.0, "the digits do not span the box across: {x0}..{x1}");
        assert!(y0 <= 1.0 && y1 >= 23.0, "the digits do not span the box down: {y0}..{y1}");
        // A single digit is centred, and just as tall: an upload must not appear to change
        // size as it crosses 10%.
        let (sx0, sy0, sx1, sy1) = ink_bounds(&counter_svg(8));
        assert!(sy0 <= 1.0 && sy1 >= 23.0, "a lone digit is not full height: {sy0}..{sy1}");
        assert!(sx0 > x0 && sx1 < x1, "a lone digit is not narrower than two");
        assert!(
            (sx0 - (24.0 - sx1)).abs() < 0.05,
            "a lone digit is not centred: {sx0} left, {} right",
            24.0 - sx1
        );
    }

    /// The end states lost the cloud with the digits (DRAGON-495), so the tick and the cross
    /// are the only ink in their own boxes and the item never changes scale as it finishes.
    #[test]
    fn the_end_states_are_the_mark_alone() {
        for face in [Face::Done, Face::Failed] {
            let svg = face_svg(face);
            assert!(!svg.contains("<circle"), "{face:?} still draws the cloud");
            assert!(!svg.contains("<polygon"), "{face:?} still draws the cloud's arrow");
            // Two strokes, and nothing else.
            assert_eq!(svg.matches("<rect").count(), 2, "{face:?} draws something unexpected");
            assert!(
                resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default())
                    .is_ok(),
                "{face:?} does not parse: {svg}"
            );
        }
    }

    /// Every digit has its own shape. A copy-paste slip in the segment table would show as
    /// two numbers rendering identically, which nothing else would catch.
    #[test]
    fn each_digit_draws_something_different() {
        let mut seen: Vec<String> = Vec::new();
        for d in 0..10u8 {
            let mut svg = String::new();
            digit_rects(d, 0.0, 0.0, 9.0, 11.0, 1.9, &mut svg);
            assert!(!svg.is_empty(), "digit {d} drew nothing");
            assert!(!seen.contains(&svg), "digit {d} draws the same as an earlier one");
            seen.push(svg);
        }
    }
}

#[cfg(test)]
mod icon_ink_fit_tests {
    use super::*;

    /// The 24-unit box every face is drawn in, and a 64px pixmap: the Linux tray's own numbers.
    const SIDE: f32 = 24.0;
    const PX: f32 = 64.0;

    /// Where a viewBox coordinate lands in the pixmap under a fit.
    fn map(v: f32, scale: f32, offset: f32) -> f32 {
        v * scale + offset
    }

    /// The point of the whole thing: whatever a face's ink measures, it comes out filling the
    /// cell, edge to edge bar the margin. Checked on the two extremes the faces actually span
    /// — the digits, which nearly fill the box already, and the lone cloud mark, which covers
    /// about two fifths of it and was the icon the owner could barely see.
    #[test]
    fn every_face_fills_the_cell_whatever_its_ink_measured() {
        for ink in [
            (1.0, 1.0, 22.0, 22.0),   // the digit face: already almost the whole box
            (5.5, 7.1, 10.2, 9.8),    // Face::Indeterminate's cloud: two fifths, off-centre
            (2.6, 4.6, 18.8, 15.0),   // the tick
        ] {
            let (scale, dx, dy) = icon_ink_fit(ink, SIDE, PX, ICON_INK_MARGIN);
            let (x, y, w, h) = ink;
            let drawn = (w.max(h)) * scale;
            assert!(
                (drawn - PX * (1.0 - 2.0 * ICON_INK_MARGIN)).abs() < 0.01,
                "{ink:?} drew its longest extent at {drawn}px of {PX}"
            );
            // Inside the pixmap, on both axes, with the margin honoured on the long one.
            for (lo, hi) in [(map(x, scale, dx), map(x + w, scale, dx)), (map(y, scale, dy), map(y + h, scale, dy))] {
                assert!(lo >= -0.01 && hi <= PX + 0.01, "{ink:?} spilled out of the cell");
                // Centred: the air before the ink is the air after it.
                assert!((lo - (PX - hi)).abs() < 0.01, "{ink:?} is not centred");
            }
        }
    }

    /// Uniform scale, so a face is never stretched to fill the cell: the shorter axis keeps
    /// its proportion and gets the leftover space as air, split evenly.
    #[test]
    fn a_lopsided_glyph_keeps_its_shape() {
        let ink = (2.0, 10.0, 20.0, 4.0); // wide and flat
        let (scale, _, dy) = icon_ink_fit(ink, SIDE, PX, ICON_INK_MARGIN);
        let (w, h) = (ink.2 * scale, ink.3 * scale);
        assert!((w / h - ink.2 / ink.3).abs() < 0.001, "the aspect ratio changed");
        assert!(h < w, "a flat glyph must stay flat");
        // The short axis is centred in what is left.
        let top = map(ink.1, scale, dy);
        assert!((top - (PX - (top + h))).abs() < 0.01, "the leftover air is not split evenly");
    }

    /// A margin of zero fills the cell exactly, and a bigger one shrinks the ink by exactly
    /// that share of the edge: the constant is a dial, not a magic number baked into the math.
    #[test]
    fn the_margin_is_the_share_of_the_edge_it_says_it_is() {
        let ink = (1.0, 1.0, 22.0, 22.0);
        let (flush, ..) = icon_ink_fit(ink, SIDE, PX, 0.0);
        assert!((22.0 * flush - PX).abs() < 0.01, "no margin must reach both edges");
        let (tenth, ..) = icon_ink_fit(ink, SIDE, PX, 0.1);
        assert!((22.0 * tenth - PX * 0.8).abs() < 0.01, "a tenth each side leaves four fifths");
    }

    /// An empty or unparsed glyph has no ink to fit. It falls back to the 1:1 box fit every
    /// caller used before this existed, so the worst case is the OLD picture, never a divide
    /// by zero or a glyph scaled to infinity.
    #[test]
    fn a_glyph_with_no_ink_falls_back_to_the_plain_box_fit() {
        for empty in [(0.0, 0.0, 0.0, 0.0), (5.0, 5.0, 0.0, 0.0), (5.0, 5.0, -1.0, -1.0)] {
            assert_eq!(icon_ink_fit(empty, SIDE, PX, ICON_INK_MARGIN), (PX / SIDE, 0.0, 0.0));
        }
        // A zero-width viewBox cannot divide either, however odd the input.
        let (scale, ..) = icon_ink_fit((0.0, 0.0, 0.0, 0.0), 0.0, PX, ICON_INK_MARGIN);
        assert!(scale.is_finite() && scale > 0.0);
    }
}
