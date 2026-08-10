//! The colour picker tool (DRAGON-582): a dimmed full-screen overlay with a magnifier
//! that follows the pointer, and the result window a pick opens.
//!
//! # Shape of the feature
//!
//! A colour-picker launch is its OWN process, exactly like a capture (`--color-picker`,
//! or the tray / preview-editor / global-shortcut entry points, which all spawn that
//! flag). It mints the SAME per-output overlays a capture launch does, through the same
//! `app::shell` seam, and only the view and the click handling differ. Nothing here adds
//! a new surface kind, and nothing here adds a new lifecycle: a cancel goes through
//! `App::teardown` and a delivered pick keeps the process alive for its window, which
//! then exits through `App::finish_session` like everything else.
//!
//! # Where the pixels come from: the `PixelSource` seam
//!
//! **The picker reads its colour from the LAUNCH-TIME FROZEN SNAPSHOT, on every
//! platform, in v1.** [`PixelSource`] names that choice so it can be changed per platform
//! later without touching the UX, and nothing downstream caches the answer: the source is
//! asked again for every sample, so a future arm can differ mid-session.
//!
//! Why frozen, stated properly, because it is not the obvious answer and the region
//! selector does the opposite:
//!
//! * **The dim is the forcing function.** Region selection never reads pixels while it is
//!   up; it reads them at COMMIT, once its overlay is gone. The picker reads continuously
//!   while its own dimming layer is on screen, so any read of the live composite comes
//!   back dimmed. A picker that reports a dimmed colour is reporting the wrong colour,
//!   which is worse than one that refuses.
//! * **The precedent exists.** The app already freezes the desktop during region
//!   selection when the freeze capture extra is on, and that is owner-approved behaviour.
//!
//! What each platform COULD do later, with the exact API, so this is a decision and not
//! an assumption:
//!
//! * **Windows can go live.** `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)`
//!   excludes our overlay from capture, so a fresh grab would see the desktop without us.
//! * **macOS can go live.** A ScreenCaptureKit content filter can exclude our own overlay
//!   window (the older below-window capture options could too).
//! * **Linux/Wayland cannot.** `ext-image-copy-capture` takes the WHOLE output including
//!   our surface, and neither it nor the cosmic protocols offer per-surface exclusion.
//!   The snapshot is the only exact source there, and always will be until a protocol
//!   changes.
//!
//! # The portal's own PickColor, and why it is not the default
//!
//! `org.freedesktop.portal.Screenshot` advertises `PickColor` at version 2 on the owner's
//! COSMIC session (verified with `busctl --user introspect`), so a Linux pick with NO
//! overlay and NO ScreenCast grant is possible. It is not the default because it hands
//! the whole interaction to the desktop: no dim at the configured opacity, no magnifier,
//! no placed hex label, none of the design this ticket specifies, and a different look on
//! every desktop. It is recorded here as the honest fallback for a Linux session where our
//! overlay genuinely cannot work, and it is deliberately NOT implemented yet: adding a
//! second, differently-behaving pick path before anything needs it would be a second
//! vocabulary for one feature.
//!
//! # Where a pick GOES: one window, and the destination model (DRAGON-613)
//!
//! **At most one colour picker window exists at a time, and any new pick updates it.** That
//! is a CROSS-PROCESS rule, not a state update, because every pick is its own one-shot
//! process. So a pick that is about to show a window first looks for a live picker window
//! ([`crate::instance::live_color_picker_windows`]) and hands its colour there
//! ([`crate::preview_ipc::send_color_to_picker`]), becoming that window's current colour and
//! the newest entry in its recents. Only if nothing takes it does this process open a window
//! of its own.
//!
//! **A pick still LAUNCHES a whole new process, and that is deliberate.** DRAGON-594 asked
//! for the picker window's pipette to re-pick in place, and the owner then CANCELLED it with
//! a better reason: a re-pick in place inherits this session's outputs and quietly locks you
//! to one monitor, when you may well want to pick from a different one each time. DRAGON-613
//! then asked for the single window. Both are satisfied at once by changing only WHERE the
//! value is delivered: the launch stays fresh, so it mints overlays for every output and can
//! sample any monitor, and only the DELIVERY is redirected. Do not "simplify" the fresh
//! launch away; it is carrying the owner's multi-monitor decision.
//!
//! [`PickDestination`] is how the two consumers stay apart, and it is a DESTINATION rather
//! than a source so the next consumer added does not have to be special-cased everywhere a
//! pick is handled.
//!
//! # A pick that belongs to a preview editor (DRAGON-587)
//!
//! The editor's toolbar pipette exists so you can take a colour off the screen and draw with
//! it. Every picker launch is its OWN PROCESS, so that colour has to travel back:
//!
//! 1. The editor spawns the picker with [`COLOR_TO_PID_ENV`] set to its own pid. The target
//!    is part of the LAUNCH, never inferred later from whatever editor happens to be open,
//!    so a pick from the tray, the CLI or a global shortcut can never reach into one.
//! 2. On the pick, this process sends the colour to exactly that pid over the preview
//!    handoff socket (`preview_ipc::send_color_to_pid`, the `color` verb). Same channel,
//!    same address, same bounded acknowledgement as a capture handoff, because it is the
//!    same shape of message.
//! 3. The editor applies it through its OWN custom-colour path
//!    (`App::apply_custom_annot_color`), so the swatch, the active colour, any selected
//!    annotation, the MRU and the persisted default all move exactly as they do when a
//!    colour is applied from the wheel.
//!
//! **A delivered pick opens NO result window** and ends the session there. The user is
//! mid-annotation and asked for a drawing colour, not for a window to read and dismiss; the
//! colour is already visible in the editor's swatch, and a window appearing over the drawing
//! they are about to make would be in the way. It also cannot steal focus if it never exists.
//!
//! **A delivery that does not land is not a failure.** If the editor closed while the pick
//! was in progress (or the host is an older build, or the platform has no transport), the
//! pick falls through to the ordinary result window, so the colour is never lost. The recents
//! are written either way: a pick is a pick ([`geom::writes_recents`]).
//!
//! # The magnifier at a screen edge (DRAGON-587)
//!
//! **The lens is always centred on the SAMPLE POINT, always the same size, always round, and it
//! CLIPS at the boundary.** The pointer is already stopped at the edge by the compositor, so
//! a second constraint on the disc buys nothing: one that slid it back on screen would lie
//! about where the sample is, one that shrank it would distort the pixels the user is
//! judging, and one that FLIPPED it to the pointer's other side would make the user re-find it
//! at the one moment they are concentrating hardest. All three were happening at one time or
//! another (a clamped origin at the top and left, a contain-fitted image at the right and
//! bottom, then a four-quadrant preference ladder); [`geom::disc_view`] carries the reasoning
//! and the fix, and the ladder's tombstone sits beside it.
//!
//! Three separate things, easy to conflate, so they are named:
//!
//! * The **drawn circle** is unclamped and unscaled, cut off by the surface.
//! * The **sampled pixel** is the pixel under the pointer's own point. The far edge pixels stay
//!   reachable, and that is a tested guarantee rather than an argument: [`geom::source_pixel`]
//!   maps from the FRACTIONAL offset and clamps per axis, so a pointer stopped a hair inside
//!   the wall still answers the last column and the last row (see `geom`'s `edge_pixel_tests`).
//! * The **magnified content** near the edge of the frozen image is TRANSPARENT, so the
//!   dimmed backdrop shows through and the boundary is unmistakable. Never a repeated edge
//!   pixel and never a wrap: both would read as real screen content that is not there.
//!
//! One thing this does NOT do, stated plainly because it looks like the same problem: a disc
//! near the boundary between two ADJACENT outputs is clipped by that boundary too, and only the
//! output the pointer is currently over draws anything. So on a desktop where a second display
//! abuts the first (the owner's is 5120x1440 at the origin with an 800x480 panel at 5120,960),
//! pushing the pointer across the seam moves the whole lens to the other display. That is
//! honest, since the pixel under the pointer really is on the other display, and since crossing
//! is never NECESSARY: the last column of each display is readable from that display
//! (`geom`'s `edge_pixel_tests` pins the owner's exact two-output geometry). Drawing one circle
//! across two overlays would mean a raster per output and a
//! per-output part list in [`Hover`]; it is a real option, not a fix that was forgotten.
//!
//! # Zooming the magnifier (DRAGON-587)
//!
//! Three input routes, ONE piece of state and ONE arithmetic. The trackpad and the mouse
//! wheel arrive as the same iced `WheelScrolled` event with different delta units (the widget
//! converts pixels to notches so both feel the same), and the numpad `+` / `-` arrive through
//! `keyboard.rs`'s picker lane. All three reduce to a signed NOTCH COUNT, become one
//! [`ColorPickerMsg::Zoom`], and move [`ColorPickerState::zoom`] only through
//! [`geom::zoom_after_step`], so no route can escape the clamp or step by its own amount.
//!
//! The range is the owner's, and it is described at BOTH ends in the same terms: how many
//! source pixels the disc holds edge to edge. The floor ([`geom::MAGNIFIER_ZOOM_MIN`] = 3,
//! DRAGON-598) holds 52 of them and stops a little SHORT of 1:1, because at 1:1 the disc shows
//! exactly what the eye already sees and below three rendered pixels per source pixel the
//! sample marker no longer fits inside its own cell. The ceiling
//! ([`geom::MAGNIFIER_ZOOM_MAX`] = 26, DRAGON-601) holds 6, the last magnification where the
//! sample still has neighbours on every side to aim by. The picker OPENS between them, at
//! [`geom::MAGNIFIER_ZOOM_DEFAULT`] = `MAGNIFIER_CELL`, holding the designed 13.
//!
//! The default and the ceiling were the SAME number until DRAGON-601, so zooming could only
//! ever travel down; the owner asked for headroom above the shipped magnification, and the
//! ceiling moved while the default deliberately did not. The disc's on-screen size is fixed at
//! every zoom; what changes is how much of the screen it holds.
//!
//! # The pointer, while picking (DRAGON-587, then DRAGON-597)
//!
//! The pointer SPRITE covers the very pixel the magnifier is reading, so the picker HIDES it,
//! everywhere, and the disc sits centred on the pointer's own point. The surface asks for
//! `Interaction::Hidden`, which reaches `Window::set_cursor_visible(false)`.
//!
//! **Asking once is not enough, and asking EARLY is the same as not asking** (DRAGON-601).
//! This overlay maps under a pointer nobody is touching, so it asks for its cursor on its
//! first frame, roughly 55 ms before the compositor sends `wl_pointer.enter`. A cursor request
//! needs an enter serial, so that first ask cannot be made and is discarded silently, and the
//! seat then behaves as though it had succeeded. The surface therefore runs the DRAGON-331
//! re-assert dance (`widgets::cursor_reassert`), armed from the surface's own APPEARANCE
//! rather than from a pointer enter the toolkit never passes on. `widgets::color_pick`'s
//! `State` carries the full reasoning, including the two wrong diagnoses that came first.
//!
//! That is only true on every surface since DRAGON-597. A plain toplevel (macOS, Windows, and
//! on Linux the portal-fallback path plus any desktop that gives us no layer shell) always
//! honoured it. A LAYER SURFACE did not: libcosmic's Wayland backend left `set_cursor_visible`
//! an unimplemented TODO for the surfaces it manages itself, so asking there hid nothing. That
//! was never a COSMIC quirk, it was a hole in the toolkit, so it applied to COSMIC, sway,
//! hyprland and river alike. Our iced fork fills it; see the iced `[patch]` block in
//! `Cargo.toml`.
//!
//! **Tombstone, because the design it replaced looked deliberate and was a workaround.** While
//! the sprite could not be hidden the picker designed around it. It asked for the DEFAULT ARROW
//! rather than a crosshair, and that was the crux rather than a detail: an arrow's hotspot is
//! its TIP, so its visual mass falls down and right and everything up and left of the tip stays
//! visible, while a crosshair is drawn AROUND its own hotspot and covers the read pixel from
//! every side with nowhere to move to. What then moved was the SAMPLE, by one point up and
//! left, to the nearest pixel the tip did not cover, with the lens centred on that. Both halves
//! are gone: [`crate::widgets::color_pick`] no longer names the arrow, and `geom`'s DRAGON-597
//! tombstone carries the shift and its far-wall reachability rule. If the fork is ever dropped
//! they come back together, never one without the other.
//!
//! That arrow design had itself replaced a much worse idea, and that tombstone is worth keeping
//! too. The first attempt moved the whole LENS, a full diameter up and left into a free
//! quadrant, with a preference ladder that flipped quadrants near an edge. It failed three ways
//! at once: the offset was measured to the bounding box rather than the circle, so the pointer
//! floated tens of points from the rim; the lens jumped sides at the top and left walls, when
//! the owner had asked twice for it to CLIP; and it left the sample under the tip anyway, which
//! was the only thing that ever needed fixing.
//!
//! Two routes to a hidden pointer on a layer surface remain closed from inside this app, and
//! [`crate::platform::overlay_hides_pointer`] records them so nobody re-runs the search: a
//! transparent CUSTOM cursor (iced's cursor vocabulary is a closed enum of named shapes, and
//! the layer backend still TODOs the custom arm), and this repo's own `cursor_reassert`, which
//! never reaches below that enum. The third route, the canonical
//! `wl_pointer.set_cursor(serial, NULL, 0, 0)`, is the one the fork took.
//!
//! # Moving the sample with the keyboard (DRAGON-599)
//!
//! The arrow keys and the vim letters `h` `j` `k` `l` move the sampled pixel, one source pixel
//! per press, hold to repeat. Four things about that are decisions rather than details.
//!
//! **The repeat cadence is OURS, not the desktop's** (DRAGON-601). A tap moves exactly one
//! pixel, and repeats only start once the key has been held past `shortcuts::NUDGE_HOLD_DELAY`,
//! capped at `shortcuts::NUDGE_REPEAT_INTERVAL`. Inheriting the compositor's cadence looked
//! free and is not: those numbers are tuned for text entry, so on a desktop with a short repeat
//! delay one deliberate tap became two or three pixels, and the same build nudged differently
//! on two machines. The gate is shared with the region nudge so the two cannot drift.
//!
//! **A Wayland client cannot warp the pointer, so the keys do not move it.** There is no
//! protocol call for it: `wl_pointer` has no "set position", and the closest thing,
//! `zwp_pointer_constraints`'s locked-pointer hint, only asks the compositor where to put the
//! cursor when a LOCK is released, which is not a thing an overlay may hold while the user is
//! choosing a colour. So what moves is the SAMPLE, held as a displacement from wherever the
//! pointer last really was ([`ColorPickerState::nudge`], in source pixels), and the lens and
//! the hex label follow it because both are placed against the sample and always were.
//!
//! **Where the pointer sprite is visible, the arrow and the sample visibly separate**, and
//! that is accepted. On a layer surface the sprite cannot be hidden at all (see the section
//! above), so after four presses the arrow sits four pixels from the lens. It is acceptable
//! because the loupe still shows the truth: the disc is centred on the sample, the outlined
//! cell inside it IS the pixel that will be taken, and the hex chip reads that pixel's value.
//! The alternative was refusing the feature on the surfaces that need it most, since nudging
//! exists precisely for the pixel a hand cannot hold the mouse still enough to hit. If
//! DRAGON-597 lands a hideable pointer on the layer path, the divergence disappears on its own
//! and nothing here has to change.
//!
//! **Any real pointer motion resets the displacement to zero** ([`geom::nudge_after`]). This
//! is the whole safety of the design: an offset that survived motion would compound with every
//! mouse move and the lens would drift permanently away from the cursor, unrecoverably, for
//! the rest of the session. Resetting means the smallest twitch of the mouse re-marries the
//! two, which is exactly what a user reaches for when the sample has wandered.
//!
//! The nudged sample obeys the SAME reachability rule the pointer's does rather than a
//! parallel one: it is clamped to the same `0 ..= extent` interval, so `(0,0)` and
//! `(w-1, h-1)` stay selectable, and `geom`'s `edge_pixel_tests` proves both halves side by
//! side. The step is `viewport / image` points rather than a flat one point, so a press is one
//! SOURCE pixel on a HiDPI display too, where a one-point step would skip every other pixel.
//!
//! # Privacy
//!
//! A picked colour is the user's CONTENT. Nothing in this module logs a colour value:
//! log lines name the notation ([`crate::color::ColorFormat::id`]) or the event, never
//! the hex. The recents list is persisted, so it is written to the config like any other
//! setting and never to the debug log.

use super::*;
use crate::color::{ColorFormat, Srgb};

pub mod geom;
mod view;

/// The picker window's title: the OS title, the CSD header title, the handle the native
/// finalize helpers match on, and the title a tiling-WM float rule keys on (see the
/// README's window-rules section). One const, like `shell::PREVIEW_WINDOW_TITLE`.
pub(crate) const WINDOW_TITLE: &str = "Cosmic Capture Kit - Color Picker";

/// Which preview editor this pick belongs to, as a pid, set by the editor that launched it
/// (DRAGON-587).
///
/// An environment variable rather than a flag because that is how this app already hands a
/// child a fact about its launch (`CCK_ACTIVE_WIN_ID`, `CCK_ACTIVE_DISPLAY`,
/// `CCK_SETTINGS_TAB`), and because it is not a user-facing option: a person running
/// `--color-picker` from a terminal has no editor to name.
pub const COLOR_TO_PID_ENV: &str = "CCK_COLOR_TO_PID";

/// Pure, unit-tested: the editor pid this pick should be delivered to, from
/// [`COLOR_TO_PID_ENV`]'s raw value.
///
/// `None` for every launch that is not an editor's (the tray, the CLI, a global shortcut),
/// which is the common case and the one that must stay byte-identical. Also `None` for
/// nonsense and for pid 0: a colour delivered to the wrong process is worse than a colour
/// shown in the picker's own window, so anything unclear declines.
pub fn color_target_pid(raw: Option<&str>) -> Option<u32> {
    let pid: u32 = raw?.trim().parse().ok()?;
    (pid > 0 && pid != std::process::id()).then_some(pid)
}

/// Who a pick is FOR (DRAGON-613). Resolved ONCE, from the launch, and then carried to
/// every consumer, so nothing downstream re-derives it.
///
/// **A destination, deliberately not a source.** The tempting model is "which UI opened the
/// picker", and it does not scale: every new consumer would have to be special-cased at
/// every site that handles a pick. "Who is this value for" adds one variant here, one wire
/// word in [`crate::preview_ipc::ColorDest`], and one arm in the drain, and nothing else
/// changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickDestination {
    /// A preview editor's annotation swatch, at this pid. The editor's toolbar pipette set
    /// [`COLOR_TO_PID_ENV`] on the launch, so the target is part of the LAUNCH and can never
    /// be guessed later from whichever editor happens to be open.
    Editor(u32),
    /// The colour picker's own result window: the value becomes the colour it shows and the
    /// newest entry in its recents. Every launch that is not an editor's, which is the
    /// common case: the tray, the CLI, a global shortcut, and the picker window's own
    /// pipette.
    PickerWindow,
}

/// **Pure**, unit-tested: the destination for a pick, from [`COLOR_TO_PID_ENV`]'s raw value.
///
/// TOTAL, which is the point: every launch has a destination, so no caller has to handle an
/// "unknown consumer" case that cannot exist. Anything that is not a usable editor pid is
/// the picker window, because a colour shown in the picker's own window is always a correct
/// outcome, while a colour delivered to a guessed pid is not.
pub fn pick_destination(raw: Option<&str>) -> PickDestination {
    match color_target_pid(raw) {
        Some(pid) => PickDestination::Editor(pid),
        None => PickDestination::PickerWindow,
    }
}

// DRAGON-597 deleted `sample_point_for`, the seam that chose between two sampling modes.
//
// There is only one mode now: the sample IS the pointer. The second mode existed because a
// Wayland layer surface could not hide the pointer sprite, so the picker asked for the default
// arrow and read one point up and left of its tip. The two were driven by ONE question
// (`platform::overlay_pointer_hideable`) precisely so they could never disagree, since an arrow
// over an unshifted sample reports a pixel nobody can see. Our iced fork hides the pointer on a
// layer surface too, so the arrow mode is unreachable and the shift went with it;
// `geom`'s DRAGON-597 tombstone carries the reachability reasoning.

/// **Pure**, unit-tested: whether the arrival of the frozen snapshot should make the picker
/// re-read the pixel it is already pointing at (DRAGON-601).
///
/// Two terms, and each is a term rather than an assumption:
///
/// * `color_picking` — every capture launch grabs the same flats, and only a picker session
///   has a loupe waiting on them. Without this the delivery would run picker code on an
///   ordinary screenshot launch.
/// * `has_pointer` — the resample reads `ColorPickerState::pointer` for the position to sample
///   FROM, so with no pointer recorded there is nothing to re-read and the call would do
///   nothing but cost a lookup.
///
/// A `false` here leaves the session exactly as it behaved before: the next real pointer move
/// samples as it always did.
///
/// **This gate was blamed once, wrongly, and the record is worth keeping.** DRAGON-601 found
/// `has_pointer` false at every delivery and concluded the gate needed a second term, a
/// launch-time probe seeding the pointer. That was treating the symptom. The real cause
/// (DRAGON-609) was that cosmic-comp sends `wl_pointer.enter` without the `wl_pointer.frame`
/// that must terminate it, so the toolkit correctly held the enter in a buffer and iced was
/// never told the pointer had arrived. The seed was deleted, the dispatch was fixed in our
/// iced fork, and `has_pointer` is now true on the enter, which is when it always should have
/// been true. This gate was correct all along.
pub fn picker_wants_frozen_resample(color_picking: bool, has_pointer: bool) -> bool {
    color_picking && has_pointer
}

/// Where the picker reads the pixel under the cursor.
///
/// One variant today. It is an enum rather than an implicit assumption because the
/// module doc's per-platform analysis has to have somewhere to attach, and because the
/// two live arms are real future work rather than speculation. [`pixel_source`] is asked
/// per SAMPLE, never cached, so adding an arm cannot require a lifecycle change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelSource {
    /// The launch-time frozen snapshot of the output under the cursor
    /// (`App::frozen`). Exact, and unaffected by our own dimming layer.
    Frozen,
}

/// This machine's pixel source, asked FRESH for every sample (see [`PixelSource`]).
///
/// Constant today on every platform. When a live arm lands it belongs here, keyed on the
/// platform capability rather than on a launch-time snapshot of it.
pub fn pixel_source() -> PixelSource {
    PixelSource::Frozen
}

/// What the pointer is currently over, recomputed on every move.
///
/// `magnifier` is built in the update handler, NOT in `view`: a per-frame
/// `Handle::from_rgba` mints a new id each call and forces a GPU re-upload plus a fresh
/// atlas entry on every redraw (the same trap `cursor_indicator` documents). It is
/// rebuilt only when the SOURCE PIXEL changes, so a mouse move inside one pixel costs
/// nothing.
pub struct Hover {
    /// Which output's overlay the pointer is over.
    pub output: String,
    /// The point being READ, in that surface's POINT space (DRAGON-587): the pointer's own
    /// point, since the sprite is hidden on every surface (DRAGON-597). The disc is centred here
    /// and the hex label is placed against it, so what the user sees and what the picker reports
    /// are the same point by construction.
    pub sample: (f32, f32),
    /// The pixel at [`Self::sample`], in the frozen snapshot's own pixel space.
    pub pixel: (u32, u32),
    /// The colour of that pixel.
    pub color: Srgb,
    /// The magnifier disc, pre-rasterised for that pixel.
    pub magnifier: widget::image::Handle,
    /// The magnification the disc was rasterised AT (DRAGON-587). The re-raster guard reads
    /// the pixel AND this: a zoom change with the pointer still over the same pixel is a
    /// different picture of the same place, and skipping it would leave the lens stuck.
    pub zoom: u32,
    /// Which PART of the disc that raster is, and where it goes (DRAGON-587). Near a screen
    /// edge the disc is CLIPPED, so the buffer is smaller than the full diameter and the
    /// view places it at this origin, at this size. The guard reads it too: a pointer sliding
    /// along an edge changes the clip without necessarily changing the pixel.
    pub disc: geom::DiscView,
}

/// All state the colour picker owns, grouped so `App` carries one field (the shape
/// `SettingsState` and `PermissionsState` already use).
///
/// `Default` is hand-written rather than derived because the magnifier's zoom opens at
/// [`geom::MAGNIFIER_ZOOM_DEFAULT`], and a derived `0` would be an invalid magnification.
pub struct ColorPickerState {
    /// This process IS a colour-picker launch. Drives the overlay view, the overlay's
    /// keyboard grab, and the frozen-flats grab the sample needs.
    pub active: bool,
    /// What the pointer is over right now. `None` before the first move, and on an
    /// output whose snapshot never arrived (see [`ColorPickerState::unavailable`]).
    pub hover: Option<Hover>,
    /// True once a sample has been attempted over an output with NO frozen snapshot.
    /// The overlay then says so instead of showing a magnifier full of nothing, and the
    /// click is inert: refusing is the only honest answer when the pixel cannot be read.
    pub unavailable: bool,
    /// The result window, once a colour has been picked (or the launch reopened one).
    pub window: Option<window::Id>,
    /// The colour the window is showing.
    pub color: Srgb,
    /// The row being typed into, and its RAW text.
    ///
    /// While a row is being edited the window must not rewrite that box from the
    /// canonical colour: the user would see their half-typed value replaced by a
    /// normalised one mid-keystroke. So the edited row renders its draft and every OTHER
    /// row renders the derived value.
    pub draft: Option<(ColorFormat, String)>,
    /// Recent colours, newest first. Only a PICK writes this (see [`geom::writes_recents`]).
    pub recents: Vec<Srgb>,
    /// The row whose Copy button was last pressed, and WHEN, for the transient
    /// "Copied" tick (`widgets::copy_button::copied_recently`).
    pub copied: Option<(ColorFormat, std::time::Instant)>,
    /// The display the pick happened on, so the result window opens where the user is
    /// looking rather than wherever the window system would have cascaded it.
    /// Consumed by the Windows native centring; `None` until a pick lands.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub pick_output: Option<String>,
    /// DRAGON-587: the pick's hex copy is WAITING for the result window's keyboard focus.
    ///
    /// Only ever true on the [`crate::share::CopyRoute::ThisWindow`] route, where the
    /// selection has to be served over one of our own surfaces. It is a one-shot latch: the
    /// first of the window's focus or the bounded deadline clears it, so a late focus can
    /// never write a copy the deadline has already reported as missed. The COLOUR itself is
    /// not stored again — the write re-derives the hex from [`Self::color`], the same way the
    /// preview editor's deferred copy re-derives its path.
    pub copy_waiting: bool,
    /// The magnifier's magnification, in rendered pixels per source pixel (DRAGON-587).
    ///
    /// ONE piece of state for all three zoom routes (trackpad, wheel, numpad `+`/`-`), moved
    /// only through [`geom::zoom_after_step`], so no route can escape the clamp or step by a
    /// different amount.
    ///
    /// **Persisted since DRAGON-615**, reversing the original call. This said "deliberately
    /// NOT persisted: a picker that opened at last week's zoom would surprise the next
    /// launch", and the owner asked for the opposite. The reasoning was wrong about which
    /// way the surprise runs: the picker is a ONE-SHOT process, so "not persisted" does not
    /// mean "fresh each time you look", it means the preference is destroyed on every single
    /// use, and someone who wants a tighter lens re-sets it every launch forever. A lens that
    /// stays where you left it is the ordinary behaviour of a zoom control.
    ///
    /// Stored as `state::schema::color_picker_zoom` and read back through
    /// [`geom::zoom_from_persisted`], which clamps it into this build's bounds. The write
    /// rides on the save a PICK already performs; there is no per-step save, because the
    /// routes include a scroll wheel and a held key.
    pub zoom: u32,
    /// Where the pointer REALLY was, last time it moved: `(output name, surface-local point)`
    /// (DRAGON-599).
    ///
    /// Kept because a directional key carries no position. The nudge is a displacement from
    /// this, so a key press has to be able to ask "from where?", and re-deriving it from
    /// [`Self::hover`] would be circular: the hover holds the ALREADY NUDGED sample, so each
    /// press would compound the last one.
    pub pointer: Option<(String, (f32, f32))>,
    /// How far the KEYBOARD has moved the sample from the pointer's own answer, in SOURCE
    /// PIXELS (DRAGON-599).
    ///
    /// A Wayland client cannot warp the pointer, so an arrow key cannot move the cursor; it
    /// moves this instead, and the sample and the lens both follow. Changed only through
    /// [`geom::nudge_after`], which is also where "a real pointer motion resets it" is
    /// stated, so no call site can accumulate an offset the mouse can never undo.
    pub nudge: (i32, i32),
    /// macOS: the FRACTION of a magnifier zoom notch a trackpad pinch has carried since the
    /// last whole notch was published ([`geom::pinch_notches`]).
    ///
    /// The widget-level wheel/scroll route keeps its own remainder in
    /// `widgets::color_pick::State::zoom_accum`, in the widget's `tree.state`. A pinch cannot
    /// use that home: it is drained by an app-level poll (`App::sub_color_picker_pinch`), not
    /// by the widget's own `Event` handler, so it needs a home reachable from there instead.
    /// NOT persisted, like the widget's own remainder: it is a per-gesture leftover, not a
    /// preference, and a fresh picker launch (this is a one-shot process) starts it at 0.0
    /// regardless.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub pinch_accum: f32,
}

impl Default for ColorPickerState {
    fn default() -> Self {
        Self {
            active: false,
            hover: None,
            unavailable: false,
            window: None,
            color: Srgb::default(),
            draft: None,
            recents: Vec::new(),
            copied: None,
            pick_output: None,
            copy_waiting: false,
            zoom: geom::MAGNIFIER_ZOOM_DEFAULT,
            pointer: None,
            nudge: (0, 0),
            pinch_accum: 0.0,
        }
    }
}

impl ColorPickerState {
    /// The text a row shows: the live draft for the row being edited, the canonical value
    /// for every other row.
    pub fn row_text(&self, format: ColorFormat) -> String {
        match &self.draft {
            Some((f, text)) if *f == format => text.clone(),
            _ => format.format(self.color),
        }
    }
}

impl App {
    /// Whether this process is the colour picker. The ONE question the overlay view, the
    /// keyboard grab and the flats gate all ask, so no surface re-derives it.
    pub(super) fn color_picking(&self) -> bool {
        self.color_picker.active
    }

    /// Sample the pixel under a POINT on `output`'s overlay, through [`pixel_source`].
    ///
    /// `None` when this output has no readable source: a snapshot that never arrived, or
    /// a degenerate geometry. The caller turns that into the overlay's explicit
    /// unavailable state rather than into a guessed colour.
    pub(super) fn sample_pixel(
        &self,
        output: &OutputState,
        point: (f32, f32),
    ) -> Option<(u32, u32, Srgb)> {
        match pixel_source() {
            PixelSource::Frozen => {
                let frozen = self.frozen.get(&output.name)?;
                let offset = output.units().capture_offset_f(point);
                let (px, py) = geom::source_pixel(
                    offset,
                    frozen.logical_size,
                    (frozen.img.width(), frozen.img.height()),
                )?;
                let p = frozen.img.get_pixel(px, py).0;
                Some((px, py, Srgb::new(p[0], p[1], p[2])))
            }
        }
    }
}

/// Open the colour-picker result window, and a Task reporting its id once mapped.
///
/// Mirrors `permissions::open_permissions_window`'s recipe: CSD, transparent surface,
/// resize border, and (the field that one was once MISSING) `blur`, which is what
/// actually enrols the surface in the platform's backdrop material. Without it a window
/// opens enrolled in nothing and the view's translucent paint shows only a flat
/// background. That is the mistake the owner warned is usually made on the first mac
/// attempt, so it is copied deliberately rather than reinvented.
///
/// **The window is exactly [`geom::color_window_size`] and cannot be resized** (DRAGON-587,
/// the owner's instruction). Every part of it is a fixed size and nothing scrolls, so a
/// larger window would only add empty space and a smaller one would clip rows.
///
/// The lock is `min_size == max_size` on EVERY platform, which is what actually pins it: it
/// is the hint the compositor and the window server both honour, and it is portable.
/// `resizable: false` goes with it everywhere EXCEPT macOS, where it must not: winit's
/// `is_zoomed` temporarily rewrites the style mask of a window that lacks
/// `Titled | Resizable`, and doing that mid-reframe is the DRAGON-130 abort the overlays and
/// the preview window both dodge by keeping both bits.
///
/// macOS therefore gets the same guarantee in two other pieces, neither of which touches the
/// style mask (DRAGON-587):
///
/// * the equal min and max become `contentMinSize` / `contentMaxSize`, and AppKit clamps
///   every drag to that range, so no edge or corner can change the size;
/// * `platform::mac::window::pin_window_size` disables the ZOOM button and opts the window
///   out of full-screen spaces, which the size clamp does not cover (the green button
///   full-screens, and full screen ignores `contentMaxSize`).
///
/// What is left on mac is cosmetic: the resize CURSOR can still appear at the frame edge,
/// because that affordance belongs to the style bit we are deliberately not clearing. A
/// cursor that does nothing is a much smaller cost than the crash.
pub(super) fn open_color_picker_window() -> (window::Id, Task<cosmic::Action<Msg>>) {
    let (w, h) = geom::color_window_size();
    let fixed = cosmic::iced::Size::new(w, h);
    #[cfg(not(windows))]
    let blur = crate::app::theme::glass_windows_enabled();
    // Windows takes DWM Mica applied AFTER the show instead (`apply_window_glass`);
    // winit's `blur` there is the legacy accent-policy blur-behind, which COMPETES with
    // `DWMWA_SYSTEMBACKDROP_TYPE`, so Mica would never appear. Same reasoning, and the
    // same comment, as `shell::preview_window`.
    #[cfg(windows)]
    let blur = false;
    let (id, task) = window::open(window::Settings {
        size: fixed,
        blur,
        // Equal floor and ceiling: the size IS the window, on every platform.
        min_size: Some(fixed),
        max_size: Some(fixed),
        // macOS keeps `Titled | Resizable` in the style mask on purpose (see the doc): the
        // equal min/max above already make it unresizable, and clearing the bit is what
        // hands winit's `is_zoomed` a mask to rewrite mid-reframe.
        #[cfg(target_os = "macos")]
        resizable: true,
        #[cfg(not(target_os = "macos"))]
        resizable: false,
        // No drag-resize band to reserve, since there is no resize.
        resize_border: 0,
        // macOS (DRAGON-135 / DRAGON-146): native decorations with a hidden, transparent
        // titlebar so the traffic lights render over our CSD header, and OPAQUE so the
        // native masked corner radius survives. Both are the settings/permissions recipe.
        #[cfg(target_os = "macos")]
        decorations: true,
        #[cfg(not(target_os = "macos"))]
        decorations: false,
        #[cfg(target_os = "macos")]
        transparent: false,
        #[cfg(not(target_os = "macos"))]
        transparent: true,
        exit_on_close_request: false,
        #[cfg(target_os = "linux")]
        platform_specific: cosmic::iced::window::settings::PlatformSpecific {
            application_id: "dev.thedragon.CosmicCaptureKit".to_string(),
            ..Default::default()
        },
        #[cfg(target_os = "macos")]
        platform_specific: cosmic::iced::window::settings::PlatformSpecific {
            title_hidden: true,
            titlebar_transparent: true,
            fullsize_content_view: true,
        },
        #[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
        platform_specific: cosmic::iced::window::settings::PlatformSpecific::default(),
        // Windows (like the preview window): open HIDDEN so it is shown only once its
        // async-set title has landed and it has been centred natively.
        #[cfg(windows)]
        visible: false,
        ..Default::default()
    });
    (
        id,
        task.map(|id| {
            cosmic::Action::App(Msg::WindowChrome(WindowChromeMsg::ColorPickerWindowOpened(
                id, 0,
            )))
        }),
    )
}

/// DRAGON-601: the loupe must appear when the SNAPSHOT lands, not when the user next moves
/// the mouse. This is the gate on the delivery that makes that happen.
#[cfg(test)]
mod frozen_resample_tests {
    use super::*;

    /// The shape the owner reported: a picker session whose overlay already knows where the
    /// pointer is, waiting on a snapshot that was still being grabbed when it first sampled.
    #[test]
    fn a_picker_with_a_known_pointer_re_reads_when_the_snapshot_lands() {
        assert!(picker_wants_frozen_resample(true, true));
    }

    /// An ordinary capture launch grabs the same flats and must be byte-identical to before:
    /// the delivery does not run picker code on a screenshot session.
    #[test]
    fn an_ordinary_capture_launch_is_untouched() {
        assert!(!picker_wants_frozen_resample(false, true));
        assert!(!picker_wants_frozen_resample(false, false));
    }

    /// A picker whose pointer has never reported has nothing to re-read: the resample samples
    /// FROM the recorded position, so calling it would only cost a lookup.
    ///
    /// This is not a rare corner. On a Wayland layer surface it is the state of EVERY session
    /// until the user moves the mouse, which is why this gate alone left the loupe missing.
    #[test]
    fn a_picker_that_has_never_seen_the_pointer_waits() {
        assert!(!picker_wants_frozen_resample(true, false));
    }

    /// And it does not touch an ordinary capture launch, which grabs the same flats and must
    /// stay byte-identical.
    #[test]
    fn an_ordinary_launch_is_untouched_by_the_gate() {
        for has_pointer in [true, false] {
            assert!(!picker_wants_frozen_resample(false, has_pointer));
        }
    }
}

/// DRAGON-587: which pick belongs to an editor. One predicate, because "does this pick
/// target an editor" has to be an explicit property of the LAUNCH rather than something
/// guessed later from whatever editor happens to be open.
#[cfg(test)]
mod color_target_tests {
    use super::*;

    /// An editor's launch names its own pid, and that is the only shape that delivers.
    #[test]
    fn an_editors_pid_is_the_only_thing_that_targets_one() {
        assert_eq!(color_target_pid(Some("4242")), Some(4242));
        assert_eq!(color_target_pid(Some("  4242 ")), Some(4242), "argv whitespace survives");
    }

    /// Every OTHER launch, which is the common one: the tray, the CLI, a global shortcut.
    /// They set nothing, so they deliver to nobody and behave exactly as before.
    #[test]
    fn a_launch_with_no_editor_targets_nobody() {
        assert_eq!(color_target_pid(None), None);
        assert_eq!(color_target_pid(Some("")), None);
        assert_eq!(color_target_pid(Some("   ")), None);
    }

    /// Anything unclear declines. A colour delivered to the wrong process is worse than one
    /// shown in the picker's own window, so junk, a negative, an overflow and pid 0 all
    /// answer "nobody" rather than guessing.
    #[test]
    fn nonsense_never_becomes_a_delivery_target() {
        for raw in ["0", "-1", "abc", "12x", "3.5", "99999999999999999999", "1 2"] {
            assert_eq!(color_target_pid(Some(raw)), None, "{raw}");
        }
    }

    /// And never OURSELVES. A picker process is not a preview host; a pid loop would mean a
    /// colour addressed to a socket this process does not own, and at worst to a recycled pid
    /// belonging to something else.
    #[test]
    fn a_picker_never_delivers_to_itself() {
        assert_eq!(color_target_pid(Some(&std::process::id().to_string())), None);
    }
}

/// DRAGON-613: who a pick is FOR. The one switch the pick handler reads, so it is pinned
/// here rather than re-derived at the call site.
#[cfg(test)]
mod pick_destination_tests {
    use super::*;

    /// An editor's launch names its own pid, and that is the ONLY thing that sends a colour
    /// somewhere other than the picker window.
    #[test]
    fn only_an_editors_pid_targets_an_editor() {
        assert_eq!(pick_destination(Some("4242")), PickDestination::Editor(4242));
        assert_eq!(pick_destination(Some("  4242 ")), PickDestination::Editor(4242));
    }

    /// Every general launch, which is the common case and the one DRAGON-613 is about: the
    /// tray, the CLI, a global shortcut, and the picker window's own pipette. All of them
    /// are for THE picker window.
    #[test]
    fn every_general_launch_targets_the_picker_window() {
        for raw in [None, Some(""), Some("   ")] {
            assert_eq!(pick_destination(raw), PickDestination::PickerWindow, "{raw:?}");
        }
    }

    /// TOTAL, and it falls the safe way. Anything unclear is the picker window, because a
    /// colour shown in the picker's own window is always a correct outcome while a colour
    /// delivered to a guessed pid is not. No input produces "no destination", so no caller
    /// has to handle a case that cannot happen.
    #[test]
    fn nonsense_falls_back_to_the_window_rather_than_guessing_a_pid() {
        for raw in ["0", "-1", "abc", "12x", "3.5", "99999999999999999999", "1 2"] {
            assert_eq!(pick_destination(Some(raw)), PickDestination::PickerWindow, "{raw}");
        }
        // Including our own pid, which would otherwise address a socket we do not own.
        assert_eq!(
            pick_destination(Some(&std::process::id().to_string())),
            PickDestination::PickerWindow
        );
    }

    /// DRAGON-587's behaviour is UNCHANGED by the widening: the editor arm fires on exactly
    /// the inputs `color_target_pid` accepted, and on no others. Pinned as an equivalence so
    /// the two can never drift into disagreeing about who owns a pick.
    #[test]
    fn the_editor_arm_is_exactly_the_old_predicate() {
        let self_pid = std::process::id().to_string();
        for raw in [None, Some(""), Some("  "), Some("0"), Some("7"), Some("4242"), Some("x"),
                    Some(self_pid.as_str())]
        {
            let expected = match color_target_pid(raw) {
                Some(pid) => PickDestination::Editor(pid),
                None => PickDestination::PickerWindow,
            };
            assert_eq!(pick_destination(raw), expected, "{raw:?}");
        }
    }
}

#[cfg(test)]
mod row_text_tests {
    use super::*;

    /// THE rule that keeps typing usable: the row being EDITED shows the user's raw text,
    /// every other row shows the canonical value derived from the colour.
    ///
    /// Without it, each keystroke would re-render the box from the parsed colour and
    /// replace the half-typed value under the caret. It is worth a test rather than a
    /// comment because the failure only shows up with a human at the keyboard.
    #[test]
    fn only_the_edited_row_shows_its_draft() {
        let mut st = ColorPickerState { color: Srgb::new(255, 136, 0), ..Default::default() };
        // No draft: every row is canonical.
        assert_eq!(st.row_text(ColorFormat::Hex), "#FF8800");
        assert_eq!(st.row_text(ColorFormat::Rgb), "rgb(255, 136, 0)");
        // Mid-edit in the RGB box, with a value that does not parse yet.
        st.draft = Some((ColorFormat::Rgb, "rgb(255, 13".to_string()));
        assert_eq!(st.row_text(ColorFormat::Rgb), "rgb(255, 13", "the caret's row is left alone");
        assert_eq!(st.row_text(ColorFormat::Hex), "#FF8800", "every other row is derived");
        assert_eq!(st.row_text(ColorFormat::Lab), ColorFormat::Lab.format(st.color));
        // Committing drops the draft, so the row re-renders canonically.
        st.draft = None;
        assert_eq!(st.row_text(ColorFormat::Rgb), "rgb(255, 136, 0)");
    }
}
