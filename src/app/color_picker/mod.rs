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
//! # Opening the window WITHOUT picking: the palette viewer (DRAGON-680)
//!
//! `--palette-viewer` is the second launch shape of this feature: the same result window,
//! opened directly, with no overlay and no pick, so the saved palette can be read and
//! reused without sampling a pixel first. It reuses the one-window rule above rather than
//! sitting beside it, and asks for a RAISE instead of sending a colour, because a colour
//! send would change what the live window is showing. [`viewer`]'s module doc carries the
//! whole shape, including which colour the window loads and why the load may not write the
//! recents.
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
pub mod viewer;

/// The picker window's title: the OS title, the CSD header title, the handle the native
/// finalize helpers match on, and the title a tiling-WM float rule keys on (see the
/// README's window-rules section). One const, like `shell::PREVIEW_WINDOW_TITLE`.
pub(crate) const WINDOW_TITLE: &str = "CCK Palette Viewer";

/// Which preview editor this pick belongs to, as a pid, set by the editor that launched it
/// (DRAGON-587).
///
/// An environment variable rather than a flag because that is how this app already hands a
/// child a fact about its launch (`CCK_ACTIVE_WIN_ID`, `CCK_ACTIVE_DISPLAY`,
/// `CCK_SETTINGS_TAB`), and because it is not a user-facing option: a person running
/// `--color-picker` from a terminal has no editor to name.
pub const COLOR_TO_PID_ENV: &str = "CCK_COLOR_TO_PID";

/// Which SAVED PALETTE this pick is for, as the NONCE the picker window minted for its
/// pipette-to-palette launch (DRAGON-687 follow-up), set by the palette row's pipette
/// button. An environment variable for [`COLOR_TO_PID_ENV`]'s exact reasons; `0` is the
/// explicit "no palette" spelling, needed because a child inherits its parent's
/// environment and the window spawning it may itself have been born with a nonce set
/// (a palette-destined pick whose delivery failed becomes a window, and its pipette's
/// children must not inherit the dead target).
///
/// A NONCE, not the group's index or name: the group can be renamed, re-sorted or
/// deleted while the pick is out, so the launch carries a token only the minting window
/// can interpret (against the `(index, name)` snapshot it kept), and the palette's NAME,
/// which is user content, stays out of the child's environment and off `ps e`.
pub const COLOR_TO_PALETTE_ENV: &str = "CCK_COLOR_TO_PALETTE";

/// The most pipette-to-palette target snapshots the window keeps (see
/// `ColorPickerState::palette_pick_targets`): cancelled picks send nothing back, so the
/// list is pruned to the newest few rather than grown for the session's life. Eight is
/// far past any real number of picks genuinely in flight at once.
pub const PALETTE_PICK_CAP: usize = 8;

/// Pure, unit-tested: the palette-pick nonce in [`COLOR_TO_PALETTE_ENV`]'s raw value.
///
/// `None` for absent, empty, junk and ZERO: zero is the explicit "no palette" every
/// ordinary launch of a pipette-spawned window writes (the [`color_target_pid`] rule,
/// applied to this variable), and the window's mint counter starts at 1 so a real nonce
/// can never be zero.
pub fn palette_pick_nonce(raw: Option<&str>) -> Option<u64> {
    let nonce: u64 = raw?.trim().parse().ok()?;
    (nonce > 0).then_some(nonce)
}

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
    /// A SAVED PALETTE in the picker window, named by the nonce that window minted
    /// (DRAGON-687 follow-up): the palette row's pipette button. The colour is APPENDED
    /// to that group and nothing else moves; see [`PickDestination::files_pick_ordinarily`].
    PickerPalette(u64),
    /// The colour picker's own result window: the value becomes the colour it shows and the
    /// newest entry in its recents. Every launch that is not an editor's or a palette
    /// row's, which is the common case: the tray, the CLI, a global shortcut, and the
    /// picker window's own pipette.
    PickerWindow,
}

impl PickDestination {
    /// **Pure**, unit-tested: does a pick for this destination FILE ordinarily in the
    /// child, i.e. become the active colour, write the recents and take the clipboard
    /// (DRAGON-687 follow-up)?
    ///
    /// `false` for exactly the palette destination, the owner's spec: "it should directly
    /// add to the palette instead of going to the main tool swatch". This is the one
    /// place that exception is stated, and [`geom::writes_recents`]'s and
    /// [`geom::CopySource`]'s docs point here, so the who-writes-what rules stay one
    /// table: those two govern the WINDOW's own colour changes and copies, and this
    /// governs which pick launches reach them at all. A palette-destined pick whose
    /// tagged delivery FAILS re-enters the ordinary flow, where this answers true again
    /// by construction (the fallback re-resolves as [`Self::PickerWindow`] behaviour).
    pub fn files_pick_ordinarily(self) -> bool {
        !matches!(self, Self::PickerPalette(_))
    }
}

/// **Pure**, unit-tested: the destination for a pick, from [`COLOR_TO_PID_ENV`]'s and
/// [`COLOR_TO_PALETTE_ENV`]'s raw values.
///
/// TOTAL, which is the point: every launch has a destination, so no caller has to handle an
/// "unknown consumer" case that cannot exist. Anything that is not a usable editor pid or a
/// usable palette nonce is the picker window, because a colour shown in the picker's own
/// window is always a correct outcome, while a colour delivered to a guessed pid is not.
///
/// The EDITOR wins where both are somehow set: no launch of ours mints both (the palette
/// pipette writes pid `0` explicitly), so the precedence only ever decides an inherited or
/// hand-built environment, and there it keeps DRAGON-587's behaviour byte-identical.
pub fn pick_destination(raw: Option<&str>, palette_raw: Option<&str>) -> PickDestination {
    if let Some(pid) = color_target_pid(raw) {
        return PickDestination::Editor(pid);
    }
    match palette_pick_nonce(palette_raw) {
        Some(nonce) => PickDestination::PickerPalette(nonce),
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

/// The magnifier disc's pixels, in whatever form THIS renderer can actually draw
/// (DRAGON-TBD).
///
/// One raster, two destinations, and the split is a RUNTIME question rather than a `cfg`
/// because the constraint is a Windows BUILD NUMBER, not a target
/// ([`crate::platform::software_overlays`]).
///
/// # Why the shader arm exists
/// The disc is rebuilt on effectively every real pointer move (a fresh ~156x156 RGBA buffer),
/// and a fresh `widget::image::Handle` per rebuild makes iced allocate and trim a texture-atlas
/// entry each time. That churn is the exact problem `preview::layers` was built to solve for the
/// preview editor, with a persistent per-layer GPU texture re-uploaded in place, and the
/// magnifier is the same shape of content. It was never given that treatment only because the
/// view could not use `widget::shader` at all while Windows 10 was in scope.
///
/// # Why the image arm stays
/// A process on iced's software rasterizer cannot draw a shader widget AT ALL: routing the
/// magnifier there unconditionally would not be slow, it would be BLANK. So a
/// software-forced process keeps the historical `widget::image` path, byte for byte.
///
/// DRAGON-650 changed WHO that is. The split used to key on the platform fact
/// (`platform::software_overlays`, true on every Windows 10 launch), but the picker process
/// is now exempt from the Windows 10 software force — its overlay is an opaque snapshot
/// that never needs the per-pixel window alpha that force exists to work around — so the
/// key is the process's OWN renderer (`app::process_forced_software_backend`). With the
/// exemption in place no picker launch is ever forced, so this arm is expected dormant; it
/// stays because the constraint it answers (no shaders on tiny-skia) is a property of the
/// renderer, not of DRAGON-650's exemption, and any future force that reaches a picker
/// process lands here safely instead of on a blank lens.
///
/// Both arms show the same pixels with the same NEAREST sampling, which is not an optimisation
/// but the tool's whole point: a colour picker that blurs its cell boundaries is lying about
/// which pixel is which.
pub enum MagnifierRaster {
    /// A software-forced process (`app::process_forced_software_backend`): the software
    /// rasterizer cannot draw a shader widget, so the disc stays an ordinary image handle.
    Image(widget::image::Handle),
    /// Everywhere else: a persistent GPU texture through `preview::LayerStack`, re-uploaded in
    /// place instead of re-allocated.
    Layer(std::sync::Arc<crate::app::preview::PixelFrame>),
}

/// **Pure**, unit-tested: wrap a freshly rasterised magnifier disc in the form this renderer
/// can draw (DRAGON-TBD). See [`MagnifierRaster`] for why there are two.
///
/// `software` is `app::process_forced_software_backend`'s answer (DRAGON-650; it was the
/// platform's `software_overlays` before the picker was exempted from the Windows 10
/// force), taken as a plain bool rather than read in here, the same way
/// `preview::effective_preview_windowed` takes its platform facts: it is what keeps the
/// decision testable on a host that is not the one it describes.
pub(super) fn build_magnifier_raster(
    w: u32,
    h: u32,
    rgba: Vec<u8>,
    software: bool,
) -> MagnifierRaster {
    if software {
        MagnifierRaster::Image(widget::image::Handle::from_rgba(w, h, rgba))
    } else {
        MagnifierRaster::Layer(crate::app::preview::PixelFrame::new(rgba, w, h))
    }
}

/// What the pointer is currently over, recomputed on every move.
///
/// `magnifier` is built in the update handler, NOT in `view`: minting the raster per frame
/// would re-upload it on every redraw (and on the `widget::image` arm mint a fresh atlas entry
/// too, the same trap `cursor_indicator` documents). It is rebuilt only when the SOURCE PIXEL
/// changes, so a mouse move inside one pixel costs nothing.
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
    /// The magnifier disc, pre-rasterised for that pixel, in whatever form this renderer can
    /// draw ([`MagnifierRaster`]).
    pub magnifier: MagnifierRaster,
    /// The magnification the disc was rasterised AT (DRAGON-587). The re-raster guard reads
    /// the pixel AND this: a zoom change with the pointer still over the same pixel is a
    /// different picture of the same place, and skipping it would leave the lens stuck.
    pub zoom: u32,
    /// Which PART of the disc that raster is (DRAGON-587): near a screen edge the disc is
    /// CLIPPED, so the buffer is smaller than the full diameter. This is the raster's
    /// IDENTITY, not the drawn position: the view draws the buffer at [`Self::disc`]'s size
    /// but PLACES it from the live [`Self::sample`] through `geom::drawn_disc_origin`
    /// (DRAGON-650), so a paced frame's lens follows the pointer while its picture lags.
    /// The two agree exactly whenever the raster is fresh. The re-raster guard reads this
    /// too: a pointer sliding along an edge changes the clip without necessarily changing
    /// the pixel.
    pub disc: geom::DiscView,
}

/// All state the colour picker owns, grouped so `App` carries one field (the shape
/// `SettingsState` and `PermissionsState` already use).
///
/// `Default` is hand-written rather than derived because the magnifier's zoom opens at
/// [`geom::MAGNIFIER_ZOOM_DEFAULT`], and a derived `0` would be an invalid magnification.
/// A drag in flight (DRAGON-682 items 35 to 40).
///
/// Armed by a press over a source, promoted to LIVE by movement past
/// `geom::DRAG_THRESHOLD`, and resolved by the release. Every position in here is in WINDOW
/// coordinates, which is what the platform's own pointer events report and what
/// `geom::drop_zone` hit-tests against: nothing in this machine works in a widget's local
/// space, because the panel's content scrolls and a scrolled offset would silently move
/// every zone.
#[derive(Clone, Copy, Debug)]
pub struct DragState {
    /// What was picked up, as the pressed widget named itself.
    pub source: geom::DragSource,
    /// The colour and alpha this drag is CARRYING, read ONCE at the press from the swatch
    /// that was pressed (DRAGON-682 item 41).
    ///
    /// Captured rather than resolved per frame, and that is the fix for the ghost showing
    /// the wrong colour: anything resolved later can be resolved from a window that has
    /// moved on. The ghost draws this, the drop files this, and the two are the same value.
    pub payload: (Srgb, u8),
    /// The zone the pointer is over that is VALID for this source, if any: the one being
    /// highlighted. Kept so the update handler can tell when it CHANGES, which is when the
    /// highlight's dashed outline needs rastering.
    pub zone: Option<geom::DropZone>,
    /// Where the pointer was when the drag was armed, or `None` until the first move is
    /// seen.
    ///
    /// A press event carries no position of its own, so the first move after the press IS
    /// the origin. In practice that is the same frame the pointer starts to travel, which
    /// costs one sample of the threshold and nothing the user can feel.
    pub origin: Option<(f32, f32)>,
    /// Where the pointer is now.
    pub at: (f32, f32),
    /// Whether the threshold has been passed. A press-release that never gets here is an
    /// ordinary click and must behave like one.
    pub live: bool,
    /// Whether the edge AUTO-SCROLL is armed (DRAGON-687's drag-scroll round): false
    /// until some live sample lands OUTSIDE both bands ([`geom::autoscroll_arms`]), so a
    /// drag born inside one (grabbing the topmost visible title, the owner's bug) cannot
    /// scroll the tab it started on; a band must be ENTERED to scroll.
    pub autoscroll_armed: bool,
}

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
    /// The mode activator's menu is open (DRAGON-630 rev 4). Transient view state, never
    /// persisted: a menu that reopened itself on the next launch would be a bug.
    ///
    /// DRAGON-680 deleted this for one revision, while the activator was read as a two
    /// button stepper, and the owner put the menu back: the chevrons "were still supposed
    /// to together act as a single hoverable unit that triggers the dropdown menu".
    pub mode_menu_open: bool,
    /// The notation the value row currently shows (DRAGON-630). PERSISTED
    /// (`state::schema::color_picker_mode`): the picker is a one-shot process, so an
    /// in-memory mode would reset on every launch, and the owner asked for the
    /// remembered one. It is also the spelling the pick's own clipboard copy takes.
    pub mode: ColorFormat,
    /// One stable widget id per value-box POSITION, minted once (DRAGON-680).
    ///
    /// [`geom::MAX_VALUE_BOXES`] of them, because the count varies with the mode (one for
    /// hex, four or five for the rest) and an id has to survive a mode change and every
    /// rebuild of the view. Two things need them, and neither can work with ids minted
    /// per frame: the window FOCUSES box 0 on open and after a mode change
    /// (`text_input::focus`), and a click into a box asks for its own text to be selected
    /// (`text_input::select_all`).
    ///
    /// Not persisted, and not a `Vec` that grows: a widget id is a runtime handle, and a
    /// fixed list indexed by position is what makes "the first box" a thing the update
    /// handler can name without consulting the view.
    pub box_ids: Vec<widget::Id>,
    /// The panel's SCROLLABLE, so the keyboard cursor can scroll itself into view
    /// (DRAGON-682 item 9). Minted once, like the value boxes' own ids.
    pub panel_scroll_id: widget::Id,
    /// An id NO widget carries, for CLEARING toolkit focus (DRAGON-680).
    ///
    /// iced's focus operation focuses the widget whose id matches and UNFOCUSES every other
    /// focusable it visits (`core::widget::operation::focusable::focus`), so aiming it at an
    /// id nobody has is the only way this toolkit offers to say "nothing is focused". The
    /// window needs that whenever Tab leaves the value boxes for the mode activator or the
    /// history: those two draw their own focus frame from [`Self::focus`], and a text input
    /// left holding the caret behind them would swallow the arrow keys they need.
    pub blur_id: widget::Id,
    /// Which history swatch a CONTEXT MENU is open over (DRAGON-680 item 24), by index.
    ///
    /// Transient view state like [`Self::mode_menu_open`], and one `Option` rather than a
    /// bool beside an index because "open, over that one" is a single fact: two fields
    /// could represent "open over nothing", which the view would have to invent an answer
    /// for.
    pub recents_menu: Option<usize>,
    /// Which harmony segment just had its colour COPIED from its menu, and WHEN
    /// (DRAGON-682 item 30), for the local "Copied!" card.
    ///
    /// Per segment, because the card is anchored at the segment the user copied from, and
    /// stamped, because it expires by the clock exactly as the copy button's own flash does
    /// (`copy_button::copied_recently`, the same window). It is CLEARED when the colour
    /// changes: the harmonies are recalculated from the current colour, so a stamp that
    /// survived would put the card on a segment holding a different colour than the one that
    /// was copied.
    pub swatch_copied: Option<((usize, usize), std::time::Instant)>,
    /// Which harmony swatch a context menu is open over (DRAGON-682), as
    /// `(group, swatch)`. Transient view state, like [`Self::recents_menu`].
    pub panel_menu: Option<(usize, usize)>,
    /// Which history swatch the POINTER is over (DRAGON-680 item 24), by index.
    ///
    /// Tracked because Backspace and Delete remove what you are POINTING at
    /// (`geom::remove_target`), and a key press carries no position. Enter sets it and the
    /// matching exit clears it; the exit names its own index so an enter that arrives
    /// before its neighbour's exit cannot be cancelled by it.
    pub hovered_recent: Option<usize>,
    /// The drag in flight, if any (DRAGON-682 items 35 to 40).
    ///
    /// ONE machine for all three sources: what differs between them is what a drop MEANS,
    /// and that is `geom::drop_action`'s table, not a second state.
    pub drag: Option<DragState>,
    /// The tab the panel was showing before a drag started (DRAGON-682 item 39).
    ///
    /// A live drag switches the panel to Saved Palettes, TRANSIENTLY: nothing is persisted
    /// and the drag's end puts this back, whether it ended in a drop, a cancel or Escape.
    pub drag_prev_tab: Option<geom::PanelTab>,
    // `palette_notice` sat here (DRAGON-682 item 39): the colour a drop left on the
    // then-placeholder Saved Palettes tab, so the "coming soon" card could name what
    // WOULD have been filed. DRAGON-687 built the real tab, a drop files the colour into
    // a real group, and the notice went with the placeholder it explained.
    /// The SAVED PALETTES (DRAGON-687): named, ordered colour groups, loaded from and
    /// persisted to `state::schema::saved_palettes` on every mutation's own save. The
    /// ORDER is state (groups drag-sort and menu-sort), so nothing here re-sorts on load.
    pub palettes: Vec<geom::Palette>,
    /// The group name being RENAMED inline, with its live draft (DRAGON-687): `Some`
    /// while the mostly-transparent editor is up. The index is REAL (the full list's),
    /// item six's rule for anything that must survive the search filter changing. Enter and the interactions that move
    /// focus COMMIT it, Escape reverts it; the draft rule is the value boxes' own (the
    /// half-typed text is never rewritten under the caret).
    pub rename: Option<(usize, String)>,
    /// The rename editor's stable widget id, minted once like the value boxes' ids: what
    /// lets a rename start focus the input and select its whole text
    /// (`select_on_focus`), on create and on every later click into the name.
    pub rename_id: widget::Id,
    /// Which group NAME a context menu is open over (DRAGON-687). Transient view state,
    /// one `Option` for the same reason [`Self::recents_menu`] is one. A visible ROW
    /// (item six), like every transient position: the menu hangs where the user sees
    /// it, and the handlers resolve the real group when a row acts.
    pub group_menu: Option<usize>,
    /// Which PALETTE swatch a context menu is open over, as `(visible row, index)`
    /// (DRAGON-687). Transient, like [`Self::panel_menu`]; row space, item six.
    pub palette_menu: Option<(usize, usize)>,
    /// Which PAGE the open context menu is showing (DRAGON-687): the root rows or one of
    /// the submenu lists. ONE field for every menu in the window, reset to Root whenever
    /// any menu opens, because only one menu is ever open at a time.
    pub menu_page: geom::MenuPage,
    /// The pointer's position over the WINDOW, in window coordinates (DRAGON-687): the
    /// ONE source of truth the hover pencil derives from
    /// ([`geom::hovered_palette_title_at`]), tracked unconditionally by a root-level
    /// `on_move` while the result window is up.
    ///
    /// Twice hardened, and the second lesson is the one to keep. The pencil was first a
    /// per-title enter/exit FLAG, which stranded exactly as `geom`'s `drag_source`
    /// tombstone warns (one mouse_area's enter starves another's exit). The first fix
    /// made it a POSITION, but reported by the scrolled CONTENT's own area, and the
    /// owner's repro named that hole: moving UP off the first title exits the reporting
    /// region entirely, so no further report arrives and the stale position sits inside
    /// the title rect forever. Level-triggered only self-heals while reports keep
    /// coming, so the source must cover EVERYWHERE the pointer can go: the whole
    /// window, where motion is reported wherever the pointer is, and every position
    /// outside the scroll viewport maps to no title by construction.
    pub window_pointer: Option<(f32, f32)>,
    /// The panel scrollable's current offset, in points: the window's MIRROR of the
    /// widget's own truth, written by its `on_scroll` and by the auto-scroll's own moves
    /// (DRAGON-687). The drop machine hit-tests through it, which is why it has to exist:
    /// the widget does not answer questions, it only reports.
    pub panel_scroll_y: f32,
    /// Each tab's REMEMBERED offset, indexed by [`geom::PanelTab::index`] (DRAGON-687's
    /// UX round, the owner: "the palette tab should remember where we scrolled to when
    /// activating"). Session-scoped, never persisted: it is where the user was, not a
    /// preference. Written and restored ONLY through `App::switch_panel_scroll`
    /// (`geom::scroll_exchange`), so the mirror above and the widget always move
    /// together and a stale offset clamps to the tab's current extent.
    pub panel_tab_scroll: [f32; 2],
    /// A group DELETION waiting on its confirmation (DRAGON-687): the group index the
    /// dialog is asking about. Both delete gestures (the menu entry and the name dragged
    /// off the window) arm this; nothing is removed until the dialog's Delete.
    pub pending_group_delete: Option<usize>,
    // `checker_palette_raster` sat here (DRAGON-687): the harmony bars' board at the
    // narrower width the bar-row buttons left the palette bars. The UX round moved the
    // pipette and plus into the title row, a palette bar is a harmony bar's exact size
    // again, and the palette bars draw `checker_bar_raster` itself.
    /// The dotted outline an EMPTY palette group's bar wears (DRAGON-687 follow-up, the
    /// owner: the same dotted outline the empty history slots get): ONE raster shared by
    /// every empty group, `color::dotted_outline_rgba` at the bar's own width, height and
    /// outer rounding. Dress only: the empty bar keeps its size, its plus button and its
    /// drop behaviour, and this just says "nothing here yet" in the history's own voice.
    pub empty_palette_raster: Option<widget::image::Handle>,
    /// The pipette-to-palette picks currently OUT (DRAGON-687 follow-up), as
    /// `(nonce, group index at launch, group name at launch)`.
    ///
    /// The nonce is what the child echoes back over the IPC (see [`COLOR_TO_PALETTE_ENV`]
    /// for why the wire carries a token rather than the group); the snapshot beside it is
    /// what [`geom::resolve_palette_target`] resolves against when the colour arrives,
    /// so a re-sort mid-pick still files into the right group and a rename or deletion
    /// degrades to the ordinary delivery instead of guessing. Pruned to the newest
    /// [`PALETTE_PICK_CAP`]: a cancelled pick sends nothing and would otherwise leave its
    /// entry forever, and nobody has that many pipette picks genuinely in flight.
    pub palette_pick_targets: Vec<(u64, usize, String)>,
    /// The nonce mint counter, starting at 1 (`0` is the explicit "no palette" spelling
    /// in the launch environment, so a real nonce is never zero).
    pub palette_pick_seq: u64,
    /// Whether the create row's SEARCH is expanded into its field (DRAGON-687 item six).
    /// Transient view state, never persisted: a filter is a moment's question, not a
    /// preference, exactly the settings window's own `search_active` rule.
    pub palette_search_active: bool,
    /// The live palette-name filter (item six): case-insensitive substring over the
    /// group NAMES, applied per keystroke through [`geom::visible_palettes`]. Blank
    /// means unfiltered. Cleared by the clear button, by Escape, by collapsing, and by
    /// Create (a new palette must be visible to be named).
    pub palette_search: String,
    /// The search field's stable widget id, minted once like the rename editor's: what
    /// lets expanding the field focus it in the same turn.
    pub palette_search_id: widget::Id,
    /// Whether the create row's SORT flyout is open (item six): the six sorts moved
    /// here from the group-name menus, so this is its own one-window flag beside the
    /// swatch menus', closed by the same `close_swatch_menus` sweep.
    pub sort_menu_open: bool,
    /// Whether the MAIN round swatch's context menu is open (item seven). Same
    /// transient shape as the other menus; [`geom::main_swatch_menu_labels`] pins its
    /// rows.
    pub main_menu: bool,
    /// The GHOST's picture, for a translucent colour: the same split swatch a translucent
    /// history entry wears. `None` for an opaque one, which is painted as a flat fill.
    pub drag_raster: Option<widget::image::Handle>,
    /// The DASHED outline of the zone currently highlighted (DRAGON-682 item 41), rastered
    /// when the highlight moves rather than per frame: it is one image the size of a whole
    /// region, and a drag crosses at most a few zones. Carried WITH the size it was built
    /// at, which is the cache key (the drag-jump round's item three,
    /// [`geom::zone_raster_size`]): the zone's identity was the key once, and a
    /// viewport-clipped group rect grows under scrolling while its identity stands, so
    /// the outline froze at the clipped size while the wash tracked the live rect.
    pub zone_raster: Option<((u32, u32), widget::image::Handle)>,
    /// The side PANEL is open, doubling the window's width (DRAGON-682).
    /// PERSISTED (`state::schema::color_picker_expanded`), like the value row's notation:
    /// the picker is a one-shot process, so an in-memory flag would collapse the panel on
    /// every launch and the owner asked for it to be remembered.
    pub expanded: bool,
    /// Which panel TAB is showing (DRAGON-682). PERSISTED
    /// (`state::schema::color_picker_panel_tab`), for the same reason.
    ///
    /// This is the SOURCE OF TRUTH; [`Self::panel_tab_model`] is the toolkit's own copy of
    /// it, kept in step by [`Self::sync_panel_tab_model`].
    pub panel_tab: geom::PanelTab,
    /// The tab strip's `segmented_button` model (DRAGON-682 item 12).
    ///
    /// **The strip is libcosmic's `tab_bar::horizontal`, which is what the settings window
    /// uses**, and that widget draws from a model rather than from a flag. A hand-built pair
    /// of buttons stood here for one commit and the owner rejected it on sight: matching the
    /// settings strip "including icons" means BEING the settings strip, and every attempt to
    /// approximate a toolkit widget's shape, spacing, active indication and hover is a
    /// promise to keep approximating it after the next libcosmic bump.
    ///
    /// So the window owns both: the enum, which is what persists and what the rest of the
    /// code reasons about, and this, which is what the widget draws. They are synced at the
    /// two moments the enum can change without the widget knowing (a persisted load, and the
    /// window opening), and the widget's own activation is what moves them the other way.
    pub panel_tab_model: widget::segmented_button::SingleSelectModel,
    /// The history's navigation CURSOR: which swatch the arrow keys are on (DRAGON-682
    /// item 7), which is NOT the same thing as which swatch is loaded.
    ///
    /// The two were one thing until the owner split them: arrowing the history used to
    /// apply each swatch as it passed, and now it moves a highlight that Space or Enter
    /// applies. So the window has a SELECTED entry (derived, `selected_recent`: the one
    /// whose colour and alpha the window is showing) and a NAVIGATED one (this), and they
    /// can point at different swatches. The view draws them differently.
    ///
    /// Transient, never persisted: it is where the keyboard is, not a preference. It is
    /// seeded when the History stop takes focus and cleared when focus leaves.
    pub recent_cursor: Option<usize>,
    /// The panel's navigation cursor, `(group, swatch)` (DRAGON-682 item 9).
    ///
    /// Transient like [`Self::recent_cursor`], and the same lifecycle: seeded when the
    /// Panel stop takes focus, cleared when focus leaves. There is no "selected" swatch to
    /// distinguish it from here, because a panel swatch has no primary action at all.
    pub panel_cursor: Option<(usize, usize)>,
    /// The CHECKERBOARD every harmony bar is drawn over (DRAGON-682 item 19): ONE raster,
    /// the width of a bar and a swatch tall, with the bar's own outer rounding.
    ///
    /// Shared by every card because every bar is the same size, and because a board that
    /// restarted per segment would draw a grid of little boards and make the seams the
    /// loudest thing in the panel. It depends on nothing but the size, the rounding token
    /// and the checker's own greys, so it is built beside the other rasters and never
    /// rebuilt for a colour change.
    pub checker_bar_raster: Option<widget::image::Handle>,
    /// The RASTER every empty history slot draws (DRAGON-682 item 8): one dotted outline,
    /// shared by all of them because they are the same size and the same ink.
    ///
    /// Built beside the swatch rasters (`refresh_recent_rasters`), so it follows the theme's
    /// subdued tone and the "Edge rounding" setting exactly as the filled swatches do.
    pub empty_slot_raster: Option<widget::image::Handle>,
    /// Which of the window's focus STOPS has focus (DRAGON-680): a value box, the mode
    /// activator, or the whole colour history. `None` before the window opens, and after a
    /// click lands somewhere that is not a stop.
    ///
    /// This is the app's own focus model, not the toolkit's, and it has to be because the
    /// toolkit's Tab cycle visits every button in the window (see [`geom::next_focus`] for
    /// why that is wrong here). Only the `Box` stop is mirrored into real toolkit focus.
    pub focus: Option<geom::PickerFocus>,
    /// The window's alpha (DRAGON-630), `255` = opaque. A PICK (and a recent-click)
    /// resets it: a screen pixel is opaque, and the alpha is an authoring control for
    /// the value being copied out, not a property of anything sampled.
    pub alpha: u8,
    /// The HSV the gradient square and the hue strip move through (DRAGON-630),
    /// tracked by `color::hsv_tracking` so the user's aim survives the achromatic
    /// colours where RGB stops carrying a hue (white, greys) or a saturation (black).
    pub hsv: [f64; 3],
    /// The window's pre-rastered gradients (DRAGON-630), rebuilt in the update handler
    /// only when their inputs change and NEVER in `view` (the magnifier's own rule:
    /// minting image handles per frame churns iced's texture atlas). `sv` follows the
    /// hue, `alpha_strip` and `swatch` follow the colour and the alpha, and
    /// `hue_strip` never changes once built.
    pub sv_raster: Option<widget::image::Handle>,
    pub hue_raster: Option<widget::image::Handle>,
    pub alpha_raster: Option<widget::image::Handle>,
    pub swatch_raster: Option<widget::image::Handle>,
    /// The value BOX being typed into: the mode it belongs to, its index (the mode's
    /// own components, alpha one past them), and its RAW text.
    ///
    /// While a box is being edited the window must not rewrite it from the canonical
    /// colour: the user would see their half-typed value replaced by a normalised one
    /// mid-keystroke. So the edited box renders its draft and every OTHER box renders
    /// the derived value.
    pub draft: Option<(ColorFormat, usize, String)>,
    /// Recent colours, newest first, each with its ALPHA (DRAGON-680). A PICK writes this
    /// (see [`geom::writes_recents`]) and so does the explicit "Add to recents", which is the
    /// only path that can file a translucent entry: a pick is opaque by construction.
    pub recents: Vec<geom::Recent>,
    /// One raster per recents entry, or `None` where the entry is OPAQUE (DRAGON-680).
    ///
    /// A translucent swatch is drawn as a picture (left half the colour, right half the
    /// colour at its alpha over the checkerboard, `color::recent_swatch_rgba`); an opaque
    /// one is still the button's own flat background, exactly as before this ticket, so
    /// the common case allocates nothing. Built in the update handler
    /// (`App::refresh_recent_rasters`) and never in `view`, the same rule the magnifier
    /// and the window's other rasters follow: a handle minted per frame churns iced's
    /// texture atlas, and this one would mint up to [`geom::RECENTS_CAP`] of them.
    ///
    /// Indexed in step with [`Self::recents`]. A missing or short list is not a bug to
    /// panic on: the view falls back to the flat fill for that entry.
    pub recent_rasters: Vec<Option<widget::image::Handle>>,
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
    /// DRAGON-TBD: a pointer position has been recorded that the lens has not caught up with
    /// yet, so the next RENDERED FRAME should re-sample.
    ///
    /// This is the whole of the picker's re-sampling cadence, and it is a flag rather than a
    /// timer on purpose. [`ColorPickerMsg::Moved`] sets it and
    /// [`crate::widgets::color_pick::ColorPickSurface`] publishes one
    /// [`ColorPickerMsg::ResamplePoll`] per redraw while it is true, so the lens updates
    /// exactly once per frame the display actually presents, at whatever rate that display
    /// runs, and the app goes fully idle the moment it clears.
    ///
    /// It is ALSO what makes the raster pacing ([`geom::raster_due`]) safe: a resample that
    /// declines to raster leaves this true, which buys exactly one more look on the next
    /// frame, and repeats until the pointer has slowed enough to take one. Without that, a
    /// sweep that ended on a paced frame would leave stale pixels in the lens with nothing
    /// left to correct them.
    pub resample_due: bool,
    /// When the last resample read the pointer, for [`geom::sample_speed`] (DRAGON-TBD). The
    /// position it is paired with is [`Hover::sample`], which is already the previous sample
    /// point, so no second copy of it is kept here.
    pub sampled_at: Option<std::time::Instant>,
    /// When the magnifier's raster was last rebuilt, for [`geom::raster_due`] (DRAGON-TBD).
    /// `None` until the first one, which is never paced.
    pub rastered_at: Option<std::time::Instant>,
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
            // The window never opens on this in the ordinary flows (a pick delivers a
            // colour and the viewer loads one through `viewer::viewer_color`), but it is
            // what a window with nothing to load would show, so it is the SAME constant
            // rather than a second opinion about "no colour yet" (DRAGON-682 item 10).
            color: viewer::DEFAULT_COLOR,
            mode: ColorFormat::Hex,
            box_ids: (0..geom::MAX_VALUE_BOXES).map(|_| widget::Id::unique()).collect(),
            drag: None,
            drag_prev_tab: None,
            palettes: Vec::new(),
            rename: None,
            rename_id: widget::Id::unique(),
            group_menu: None,
            palette_menu: None,
            menu_page: geom::MenuPage::default(),
            window_pointer: None,
            panel_scroll_y: 0.0,
            panel_tab_scroll: [0.0; 2],
            pending_group_delete: None,
            empty_palette_raster: None,
            palette_pick_targets: Vec::new(),
            palette_pick_seq: 0,
            palette_search_active: false,
            palette_search: String::new(),
            palette_search_id: widget::Id::unique(),
            sort_menu_open: false,
            main_menu: false,
            drag_raster: None,
            zone_raster: None,
            panel_scroll_id: widget::Id::unique(),
            blur_id: widget::Id::unique(),
            focus: None,
            recents_menu: None,
            panel_menu: None,
            swatch_copied: None,
            hovered_recent: None,
            expanded: false,
            panel_tab: geom::PanelTab::default(),
            panel_tab_model: geom::PanelTab::model(),
            recent_cursor: None,
            panel_cursor: None,
            empty_slot_raster: None,
            checker_bar_raster: None,
            mode_menu_open: false,
            alpha: u8::MAX,
            hsv: [0.0, 0.0, 0.0],
            sv_raster: None,
            hue_raster: None,
            alpha_raster: None,
            swatch_raster: None,
            draft: None,
            recents: Vec::new(),
            recent_rasters: Vec::new(),
            copied: None,
            pick_output: None,
            copy_waiting: false,
            zoom: geom::MAGNIFIER_ZOOM_DEFAULT,
            pointer: None,
            nudge: (0, 0),
            resample_due: false,
            sampled_at: None,
            rastered_at: None,
            pinch_accum: 0.0,
        }
    }
}

/// The value row's WHOLE-VALUE box, as a component index (DRAGON-630 rev 3): hex's one
/// unified box edits the full spelling through the same draft and message plumbing the
/// channel boxes use, and this sentinel is how the handlers tell the two apart.
/// `usize::MAX` can never collide with a real component index (the widest mode has five).
///
/// It named the layout TOGGLE's collapsed state until DRAGON-680; the sentinel outlived
/// the toggle because what it marks is a real difference in what the box HOLDS (a whole
/// spelling, parsed by `parse_with_alpha`) rather than which layout is switched on.
pub const WHOLE_VALUE_BOX: usize = usize::MAX;

impl ColorPickerState {
    /// The current mode's whole value in its canonical spelling, alpha included: what
    /// the copy button and the pick's own copy put on the clipboard (DRAGON-630), and
    /// what hex's one unified box shows.
    ///
    /// The mode STEPPER is deliberately not in that list any more (DRAGON-680): changing
    /// the notation no longer copies anything (see `update_color_picker`'s `ModeStepped`).
    pub fn value_text(&self) -> String {
        self.mode.format_with_alpha(self.color, self.alpha)
    }

    /// The text box `idx` shows: the live draft for the box being edited, the canonical
    /// component text for every other box (DRAGON-630; the draft rule is DRAGON-582's,
    /// per box now instead of per row). [`WHOLE_VALUE_BOX`] is hex's single box and shows
    /// the full spelling.
    pub fn box_text(&self, idx: usize) -> String {
        match &self.draft {
            Some((m, i, text)) if *m == self.mode && *i == idx => text.clone(),
            _ if idx == WHOLE_VALUE_BOX => self.value_text(),
            _ => self.mode.component_text(self.color, self.alpha, idx),
        }
    }

    /// How many BOXES the current mode's value row shows (DRAGON-680): ONE for hex, the
    /// mode's own components plus the shared alpha box for everything else.
    ///
    /// It reads [`geom::splits_components`] rather than restating the rule, so the count,
    /// the widths and the view can never disagree about which layout a mode is in. Hex
    /// counted like everyone else for one ticket, while a persisted toggle decided the
    /// layout for ALL modes at once; the owner replaced that with the per-mode rule.
    pub fn box_count(&self) -> usize {
        if geom::splits_components(self.mode) {
            self.mode.component_labels().len() + 1
        } else {
            1
        }
    }

    /// The widget id of the value box at POSITION `pos` in the row (DRAGON-680), for the
    /// focus and select-all tasks. `None` past the last box a mode can have.
    ///
    /// Positional, not the draft's component index: hex's one box is [`WHOLE_VALUE_BOX`]
    /// to the draft plumbing and position 0 here, which is what lets "focus the first
    /// box" mean the same thing in every mode.
    pub fn box_id(&self, pos: usize) -> Option<widget::Id> {
        self.box_ids.get(pos).cloned()
    }

    /// **Pure**, unit-tested: every raster this window can draw that is NOT built yet, by
    /// name (DRAGON-682 item 25).
    ///
    /// **This is an INVENTORY, and it exists because the old pattern could not stop
    /// failing.** Each raster used to be built by whichever handler happened to change its
    /// inputs, so "is everything ready" was answered by a scatter of call sites that a new
    /// raster had to remember to join. It did not: the harmony checkerboard and the empty
    /// slots' dots were built only by the recents-CHANGED path, so a window opened from the
    /// palette viewer showed neither until the user removed a history entry, which is
    /// exactly what the owner reported twice.
    ///
    /// So the question is asked in ONE place instead. `App::ensure_all_rasters` builds them
    /// all and then asserts this list is empty, and this list is what a test can check
    /// without a GPU, a theme or a window: an `image::Handle` is plain data, so a test can
    /// fill every field and prove the inventory covers exactly the fields that exist.
    ///
    /// A recents raster is only REQUIRED for a translucent entry: an opaque swatch is drawn
    /// as the button's own flat fill and legitimately has none (see `view::recent_swatch`).
    pub fn missing_rasters(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        for (name, present) in [
            ("sv square", self.sv_raster.is_some()),
            ("hue strip", self.hue_raster.is_some()),
            ("alpha strip", self.alpha_raster.is_some()),
            ("round swatch", self.swatch_raster.is_some()),
            ("empty history slot", self.empty_slot_raster.is_some()),
            ("harmony checkerboard", self.checker_bar_raster.is_some()),
            ("empty palette outline", self.empty_palette_raster.is_some()),
        ] {
            if !present {
                missing.push(name);
            }
        }
        let translucent_needs_one = self
            .recents
            .iter()
            .enumerate()
            .filter(|(_, e)| e.alpha != u8::MAX)
            .any(|(i, _)| self.recent_rasters.get(i).and_then(|r| r.as_ref()).is_none());
        if translucent_needs_one {
            missing.push("a translucent history swatch");
        }
        missing
    }

    /// Point the tab strip's model at [`Self::panel_tab`] (DRAGON-682 item 12).
    ///
    /// Called wherever the enum moves WITHOUT the widget's help: a persisted load, and the
    /// window opening (the panel cannot be seen before that, so syncing there is early
    /// enough). The other direction needs nothing: an activation in the widget publishes the
    /// message that sets the enum, and the widget has already moved itself.
    pub fn sync_panel_tab_model(&mut self) {
        let want = self.panel_tab;
        let entity = self
            .panel_tab_model
            .iter()
            .find(|e| self.panel_tab_model.data::<geom::PanelTab>(*e) == Some(&want));
        if let Some(entity) = entity {
            self.panel_tab_model.activate(entity);
        }
    }

    /// The colour a press picks up, read at the PRESS (DRAGON-682 item 41).
    ///
    /// Every source resolves its own way and all of them resolve HERE, once, so the ghost
    /// and the drop can never carry different values: a harmony swatch is computed from the
    /// window's colour at that instant, a history entry is that entry with its own alpha,
    /// the active swatch is what the window is showing, and a palette colour is the saved
    /// entry itself (DRAGON-687). `None` names a source the window no longer has, which
    /// arms nothing.
    pub fn press_payload(&self, source: geom::DragSource) -> Option<(Srgb, u8)> {
        match source {
            geom::DragSource::Active => Some((self.color, self.alpha)),
            geom::DragSource::Recent(i) => self.recents.get(i).map(|e| (e.color, e.alpha)),
            // A harmony swatch is resolved by the caller through [`Self::panel_swatch`],
            // since its identity IS its position in the grid.
            geom::DragSource::Harmony(..) => None,
            // A palette colour carries itself, alpha included (DRAGON-687).
            geom::DragSource::PaletteSwatch(g, i) => self.palette_swatch_at((g, i)),
            // A group NAME carries no colour at all; the ghost draws the name instead
            // (`view::drag_ghost`'s name arm). The payload slot still has to hold
            // SOMETHING because the machine is one struct for every source; black-opaque
            // is a placeholder nothing ever draws or files.
            geom::DragSource::PaletteName(g) => {
                // ROW space (item six): valid while the filtered list has this row.
                (g < self.visible_palettes().len()).then_some((Srgb::new(0, 0, 0), u8::MAX))
            }
        }
    }

    /// The VISIBLE palette rows, as REAL indices in display order (DRAGON-687 item six):
    /// the whole list while the filter is blank, the name matches under it. Every
    /// row-indexed surface (the drop machine, the keyboard cursor, the menus, the
    /// virtualized view) walks this list; every mutation resolves through it back to the
    /// real index first.
    pub fn visible_palettes(&self) -> Vec<usize> {
        geom::visible_palettes(&self.palettes, &self.palette_search)
    }

    /// The REAL palette a visible ROW names, or `None` for a row the filtered list no
    /// longer has (item six): the one row-to-real seam the handlers resolve through.
    pub fn real_palette(&self, row: usize) -> Option<usize> {
        self.visible_palettes().get(row).copied()
    }

    /// The palette colour at `(visible row, index)`, with its own alpha (DRAGON-687), or
    /// `None` for an identity that no longer names anything. ROW space (item six): the
    /// drag machine and the keyboard cursor both live on the filtered list.
    pub fn palette_swatch_at(&self, at: (usize, usize)) -> Option<(Srgb, u8)> {
        let real = self.real_palette(at.0)?;
        let e = self.palettes.get(real)?.colors.get(at.1)?;
        Some((e.color, e.alpha))
    }

    /// The panel facts the drop machine reads ([`geom::PanelShape`]), assembled in ONE
    /// place so no call site can pair a stale scroll with a fresh group list. The groups
    /// are the VISIBLE rows' lengths (item six): geometry only ever describes what is on
    /// screen.
    pub fn panel_shape(&self) -> geom::PanelShape {
        geom::PanelShape {
            palettes: self.palette_drop_open(),
            scroll: self.panel_scroll_y,
            groups: self
                .visible_palettes()
                .into_iter()
                .filter_map(|g| self.palettes.get(g).map(|p| p.colors.len()))
                .collect(),
        }
    }

    /// The ragged rows the PANEL's keyboard cursor walks, for whichever tab is showing
    /// (DRAGON-687): the harmony cards' lengths, or the VISIBLE saved palettes' own
    /// (item six: the cursor walks what is drawn).
    pub fn panel_rows(&self) -> Vec<usize> {
        match self.panel_tab {
            geom::PanelTab::Harmonies => geom::harmony_card_lengths(),
            geom::PanelTab::Palettes => self
                .visible_palettes()
                .into_iter()
                .filter_map(|g| self.palettes.get(g).map(|p| p.colors.len()))
                .collect(),
        }
    }

    /// The swatch the panel CURSOR names, on whichever tab is showing (DRAGON-687): a
    /// harmony position resolves through [`Self::panel_swatch`] and a palette position
    /// through [`Self::palette_swatch_at`]. What the keyboard's copy reads.
    pub fn panel_cursor_swatch(&self, at: (usize, usize)) -> Option<(Srgb, u8)> {
        match self.panel_tab {
            geom::PanelTab::Harmonies => self.panel_swatch(at),
            geom::PanelTab::Palettes => self.palette_swatch_at(at),
        }
    }

    /// The window's own size, from its LAYOUT (DRAGON-682 item 35).
    ///
    /// Drop zones are hit-tested against this. It is the size the window was asked to be,
    /// which is the size it has except for the few frames a toggle is in flight, and nothing
    /// is being dragged in those (a drag holds the pointer down; a toggle needs a click).
    /// It read the live reported width until item 42, and that was part of the jitter
    /// machinery rather than anything the drag needed.
    pub fn window_size(&self) -> (f32, f32) {
        geom::color_window_size_for(self.expanded)
    }

    /// Whether the panel is showing SAVED PALETTES right now, which is the only tab that
    /// takes a drop (DRAGON-682 item 39).
    pub fn palette_drop_open(&self) -> bool {
        self.panel_mounted() && self.panel_tab == geom::PanelTab::Palettes
    }

    /// Whether a drag has passed its threshold and is being drawn (DRAGON-682 item 35).
    ///
    /// THE question the view asks: an armed press that has not moved is not a drag, shows no
    /// ghost, silences no tooltip and swallows no click.
    pub fn dragging(&self) -> bool {
        self.drag.as_ref().is_some_and(|d| d.live)
    }

    /// Whether the panel is DRAWN (DRAGON-682, simplified by item 42).
    ///
    /// It IS the persisted flag now, and this is kept as a named question rather than
    /// inlined because three unrelated things ask it: the view mounts the subtree on it, the
    /// focus ring offers its panel stop on it, and a drag will not pick up a harmony swatch
    /// without it. It was a width-and-clock rule for items 33 and 34; `geom`'s tombstone
    /// beside `picker_column_w` says why that went.
    pub fn panel_mounted(&self) -> bool {
        self.expanded
    }

    /// The harmony swatch at a PANEL CURSOR position, with the alpha it is drawn at
    /// (DRAGON-682 item 32), or `None` for a position no card holds.
    ///
    /// Derived, never stored: the cards are recomputed from [`Self::color`] on every frame,
    /// so the keyboard's copy has to resolve its cursor the same way the view resolves it,
    /// at the moment the key is pressed. The alpha is the WINDOW's (item 19), which is what
    /// the segment is painted with.
    pub fn panel_swatch(&self, at: (usize, usize)) -> Option<(Srgb, u8)> {
        let (group, index) = at;
        let h = *crate::color::Harmony::ALL.get(group)?;
        Some((*h.swatches(self.color).get(index)?, self.alpha))
    }

    /// The clipboard spelling for ANY swatch in this window (DRAGON-682): the remembered
    /// notation, at the swatch's OWN alpha.
    ///
    /// Deliberately the same formatter [`Self::value_text`] uses, so a colour copied from a
    /// menu and the same colour copied from the copy button are spelled identically. The
    /// alpha is a parameter because the two swatch kinds carry different ones: a history
    /// entry has the alpha it was filed at, and a harmony segment is drawn at the window's
    /// current alpha (item 19). An opaque one spells with no alpha digits at all, which is
    /// `format_with_alpha`'s own rule and is what keeps an opaque copy byte-identical.
    pub fn swatch_copy_text(&self, c: Srgb, alpha: u8) -> String {
        self.mode.format_with_alpha(c, alpha)
    }

    /// Which history entry the window is currently SHOWING, if any (DRAGON-680).
    ///
    /// DERIVED rather than stored, and that is what keeps it honest: `geom::push_recent`
    /// cannot hold two identical entries, so a colour-and-alpha match is unique, and any
    /// path that changes the shown colour (a pick, a typed edit, a strip drag, a loaded
    /// swatch) moves this answer with it for free. A colour that is not in the history is
    /// `None`, which the grid's arrow keys read as "enter at the first swatch".
    ///
    /// It is what the history's focus ring navigates from AND what the grid draws its
    /// selection outline on, so the two cannot disagree about which swatch is current.
    pub fn selected_recent(&self) -> Option<usize> {
        let showing = geom::Recent::new(self.color, self.alpha);
        self.recents.iter().position(|e| *e == showing)
    }
}

impl App {
    /// Whether this process is the colour picker. The ONE question the overlay view, the
    /// keyboard grab and the flats gate all ask, so no surface re-derives it.
    pub(super) fn color_picking(&self) -> bool {
        self.color_picker.active
    }

    /// Every picker OVERLAY surface currently mapped, for the magnifier layer's closed-window
    /// eviction (DRAGON-TBD). The picker's answer to `App::live_preview_windows`, and it is
    /// needed for the same reason: iced keys a shader `Pipeline` by primitive TYPE and shares
    /// it across every window's renderer, exposing NO external handle, so the only way a slot
    /// is ever freed is a prepare being told which windows are still alive.
    ///
    /// `self.outputs` is exactly that set: it is what the overlay maps to, and
    /// `destroy_surfaces` clears it. A picker LAUNCH is its own process (see this module's
    /// doc), so there are no preview documents here to protect as well; if that ever changes,
    /// this is the one place that has to learn about them.
    ///
    /// Known and accepted: an output the pointer has LEFT stops mounting its stack, so its
    /// magnifier slot is not reclaimed until the process exits. That is bounded by the display
    /// count (a disc is ~97KB) and a picker session is short-lived, which is the same trade
    /// `layers`' own doc accepts for a preview that closed.
    pub(in crate::app) fn live_picker_windows(&self) -> Vec<window::Id> {
        self.outputs.iter().map(|o| o.id).collect()
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
/// **The window is exactly [`geom::color_window_size_for`] and cannot be resized BY THE
/// USER** (DRAGON-587, the owner's instruction). Every part of the picker's own column is a
/// fixed size, so a larger window would only add empty space and a smaller one would clip
/// rows.
///
/// It has TWO such sizes since DRAGON-682: the base window, and twice its width with the
/// panel open. `expanded` is the persisted state the launch opens at, so the window
/// appears at the size it was left rather than opening narrow and jumping. Toggling later
/// goes through `App::resize_color_picker_window`, which has to RELEASE the min/max lock
/// below before it can resize and re-pin it after: with equal floor and ceiling, a resize
/// request is otherwise clamped straight back.
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
///
/// **Windows has its own lesson, and it is the mirror image of the mac one** (DRAGON-680).
/// `resizable: false` there is real: winit leaves `WS_SIZEBOX` off the HWND, so this is the
/// only window in the app that Windows will not run an interactive SIZE loop for. That is the
/// whole point, and it is also what broke dragging the window by its title. Windows sends
/// `WM_EXITSIZEMOVE` only when it actually ENTERED a move/size loop, and that message is the
/// ONLY thing that clears the private latch winit sets in its `drag_window` before posting the
/// `WM_NCLBUTTONDOWN`. So a size request Windows declined, on a window with no sizing border,
/// left the latch stuck on, and from then on every header drag posted nothing: the title area
/// stopped dragging for the life of the window. The owner's words: "if I try to resize the
/// window I can't, which is good, but then I can't drag the color picker window by its title
/// anymore."
///
/// The fix does NOT go here, and that is the point worth remembering. Handing the window a
/// sizing border to keep the loops running would trade a dead drag for a live resize handle on
/// a window whose whole contract is that it has exactly one size, and it would still not cover
/// the other ways Windows can decline a drag. The drag path itself is what heals instead: the
/// `ColorPickerWindowDrag` arm calls `platform::windows::window::end_stale_window_drag` right
/// before `window::drag`, which is inert unless a latch is actually stuck. That function's doc
/// carries the mechanism in full.
pub(super) fn open_color_picker_window(expanded: bool) -> (window::Id, Task<cosmic::Action<Msg>>) {
    let (w, h) = geom::color_window_size_for(expanded);
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

/// DRAGON-TBD: which renderer draws the magnifier. The whole risk of the change is in this
/// one branch: get it wrong on Windows 10 and the disc is not slow, it is BLANK, and no test
/// that runs on this machine can see that happen. So the decision is pure and pinned here,
/// separately from the platform read that feeds it.
#[cfg(test)]
mod magnifier_raster_tests {
    use super::*;

    /// A raster small enough to write out by hand, so the test can prove the PIXELS survive
    /// the wrap rather than just the variant.
    fn raster() -> (u32, u32, Vec<u8>) {
        (2, 1, vec![1, 2, 3, 4, 250, 251, 252, 253])
    }

    /// The GPU arm, which is every machine but Windows 10: a `PixelFrame` for the persistent
    /// texture slot, carrying the same dimensions and the same bytes.
    #[test]
    fn a_gpu_renderer_takes_the_persistent_texture_path() {
        let (w, h, rgba) = raster();
        match build_magnifier_raster(w, h, rgba.clone(), false) {
            MagnifierRaster::Layer(frame) => {
                assert_eq!((frame.w, frame.h), (w, h));
                assert_eq!(frame.rgba, rgba, "the disc's pixels must survive the wrap");
            }
            MagnifierRaster::Image(_) => panic!("a GPU renderer must take the shader path"),
        }
    }

    /// Windows 10, the arm nothing here can render-test: the software rasterizer cannot draw a
    /// shader widget at all, so the disc has to stay an ordinary image handle. A wrong answer
    /// here is a blank magnifier on that machine.
    #[test]
    fn a_software_rasterizer_keeps_the_image_handle() {
        let (w, h, rgba) = raster();
        match build_magnifier_raster(w, h, rgba.clone(), true) {
            MagnifierRaster::Image(handle) => match handle {
                widget::image::Handle::Rgba { width, height, pixels, .. } => {
                    assert_eq!((width, height), (w, h));
                    assert_eq!(pixels.as_ref(), rgba.as_slice());
                }
                other => panic!("the software arm must mint an RGBA handle, got {other:?}"),
            },
            MagnifierRaster::Layer(_) => {
                panic!("the software rasterizer cannot draw a shader widget")
            }
        }
    }

    /// And the two answers are genuinely different, so a future refactor that collapsed the
    /// branch (in either direction) fails here rather than on a machine nobody on the project
    /// owns.
    #[test]
    fn the_branch_actually_branches() {
        let (w, h, rgba) = raster();
        let gpu = build_magnifier_raster(w, h, rgba.clone(), false);
        let software = build_magnifier_raster(w, h, rgba, true);
        assert!(matches!(gpu, MagnifierRaster::Layer(_)));
        assert!(matches!(software, MagnifierRaster::Image(_)));
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
    /// somewhere other than the picker window or one of its palettes.
    #[test]
    fn only_an_editors_pid_targets_an_editor() {
        assert_eq!(pick_destination(Some("4242"), None), PickDestination::Editor(4242));
        assert_eq!(pick_destination(Some("  4242 "), None), PickDestination::Editor(4242));
    }

    /// Every general launch, which is the common case and the one DRAGON-613 is about: the
    /// tray, the CLI, a global shortcut, and the picker window's own pipette. All of them
    /// are for THE picker window.
    #[test]
    fn every_general_launch_targets_the_picker_window() {
        for raw in [None, Some(""), Some("   ")] {
            assert_eq!(pick_destination(raw, None), PickDestination::PickerWindow, "{raw:?}");
            // The explicit "no palette" every pipette-spawned window's own children carry.
            assert_eq!(pick_destination(raw, Some("0")), PickDestination::PickerWindow);
        }
    }

    /// The palette destination (DRAGON-687 follow-up): a usable nonce and no editor pid.
    /// Zero and junk are the picker window (zero is the explicit "no palette"), and an
    /// editor pid WINS over a nonce, keeping DRAGON-587 byte-identical for any inherited
    /// or hand-built environment that somehow carries both.
    #[test]
    fn a_palette_nonce_targets_the_palette_and_the_editor_still_wins() {
        assert_eq!(pick_destination(None, Some("7")), PickDestination::PickerPalette(7));
        assert_eq!(pick_destination(Some("0"), Some(" 7 ")), PickDestination::PickerPalette(7));
        for raw in ["0", "", "abc", "-1", "1.5"] {
            assert_eq!(
                pick_destination(None, Some(raw)),
                PickDestination::PickerWindow,
                "{raw}"
            );
        }
        assert_eq!(pick_destination(Some("4242"), Some("7")), PickDestination::Editor(4242));
        // The nonce parser's own vocabulary, pinned beside the pid parser's.
        assert_eq!(palette_pick_nonce(Some("1")), Some(1));
        assert_eq!(palette_pick_nonce(Some(&u64::MAX.to_string())), Some(u64::MAX));
        for raw in [None, Some(""), Some("0"), Some("junk"), Some("-2")] {
            assert_eq!(palette_pick_nonce(raw), None, "{raw:?}");
        }
    }

    /// The owner's exception, stated as the table it is (DRAGON-687 follow-up): a
    /// palette-destined pick files NOTHING ordinarily (no active colour, no recents, no
    /// clipboard in the child), and every other destination is untouched.
    #[test]
    fn only_a_palette_pick_skips_the_ordinary_filing() {
        assert!(!PickDestination::PickerPalette(1).files_pick_ordinarily());
        assert!(PickDestination::PickerWindow.files_pick_ordinarily());
        assert!(PickDestination::Editor(4242).files_pick_ordinarily());
    }

    /// TOTAL, and it falls the safe way. Anything unclear is the picker window, because a
    /// colour shown in the picker's own window is always a correct outcome while a colour
    /// delivered to a guessed pid is not. No input produces "no destination", so no caller
    /// has to handle a case that cannot happen.
    #[test]
    fn nonsense_falls_back_to_the_window_rather_than_guessing_a_pid() {
        for raw in ["0", "-1", "abc", "12x", "3.5", "99999999999999999999", "1 2"] {
            assert_eq!(pick_destination(Some(raw), None), PickDestination::PickerWindow, "{raw}");
        }
        // Including our own pid, which would otherwise address a socket we do not own.
        assert_eq!(
            pick_destination(Some(&std::process::id().to_string()), None),
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
            assert_eq!(pick_destination(raw, None), expected, "{raw:?}");
        }
    }
}

#[cfg(test)]
mod box_text_tests {
    use super::*;

    /// THE rule that keeps typing usable (DRAGON-582, per box since DRAGON-630): the
    /// box being EDITED shows the user's raw text, every other box shows the canonical
    /// value derived from the colour.
    ///
    /// Without it, each keystroke would re-render the box from the parsed colour and
    /// replace the half-typed value under the caret. It is worth a test rather than a
    /// comment because the failure only shows up with a human at the keyboard.
    #[test]
    fn only_the_edited_box_shows_its_draft() {
        let mut st = ColorPickerState {
            color: Srgb::new(255, 136, 0),
            mode: ColorFormat::Rgb,
            ..Default::default()
        };
        // No draft: every box is canonical.
        assert_eq!(st.box_text(0), "255");
        assert_eq!(st.box_text(1), "136");
        assert_eq!(st.box_text(3), "1", "the alpha box shows opaque");
        // Mid-edit in the G box, with a value that does not parse yet.
        st.draft = Some((ColorFormat::Rgb, 1, "13.".to_string()));
        assert_eq!(st.box_text(1), "13.", "the caret's box is left alone");
        assert_eq!(st.box_text(0), "255", "every other box is derived");
        // Committing drops the draft, so the box re-renders canonically.
        st.draft = None;
        assert_eq!(st.box_text(1), "136");
        // A draft from ANOTHER mode never leaks into this one's boxes.
        st.draft = Some((ColorFormat::Hsl, 1, "50".to_string()));
        assert_eq!(st.box_text(1), "136", "a stale mode's draft is ignored");
    }

    /// The value the window copies is the MODE's whole spelling, alpha included, which
    /// is what makes the stepper's copy and the pick's copy one vocabulary.
    #[test]
    fn value_text_is_the_modes_whole_spelling() {
        let mut st = ColorPickerState {
            color: Srgb::new(255, 136, 0),
            mode: ColorFormat::Rgb,
            ..Default::default()
        };
        assert_eq!(st.value_text(), "rgb(255, 136, 0)");
        st.alpha = 204;
        assert_eq!(st.value_text(), "rgba(255, 136, 0, 0.8)");
        st.mode = ColorFormat::Hex;
        assert_eq!(st.value_text(), "#FF8800CC");
    }

    /// The box counts the layout depends on (DRAGON-680): hex is ONE unified box, and
    /// every other mode is its own components plus the shared alpha box.
    ///
    /// Hex was four channel-pair boxes for one ticket, while a persisted toggle chose
    /// the layout for every mode at once. The owner's rule is per mode now, and this is
    /// the state-side half of it (`geom::splits_components` is the decision).
    #[test]
    fn box_counts_match_the_modes() {
        let mut st = ColorPickerState::default();
        assert_eq!(st.box_count(), 1, "hex is ONE unified box");
        for (mode, want) in [
            (ColorFormat::Hex, 1),
            (ColorFormat::Rgb, 4),
            (ColorFormat::Hsl, 4),
            (ColorFormat::Hsv, 4),
            (ColorFormat::Oklch, 4),
            (ColorFormat::Cmyk, 5),
            (ColorFormat::Lab, 4),
        ] {
            st.mode = mode;
            assert_eq!(st.box_count(), want, "{}", mode.id());
            assert_eq!(
                st.box_count() > 1,
                geom::splits_components(mode),
                "{}: the count and the layout decision disagree",
                mode.id()
            );
        }
    }

    /// DRAGON-682: a swatch menu's COPY spells the colour in the remembered notation, at
    /// the alpha it was drawn at. Pinned across every mode, because "respecting the chosen mode" is the whole
    /// of the owner's ask and a single-mode test would pass on a hard-coded hex.
    #[test]
    fn a_swatch_copies_in_the_remembered_mode_and_opaque() {
        let mut st = ColorPickerState::default();
        let c = Srgb::new(255, 136, 0);
        for (mode, want) in [
            (ColorFormat::Hex, "#FF8800"),
            (ColorFormat::Rgb, "rgb(255, 136, 0)"),
            (ColorFormat::Hsl, "hsl(32, 100%, 50%)"),
        ] {
            st.mode = mode;
            assert_eq!(st.swatch_copy_text(c, u8::MAX), want, "{}", mode.id());
        }
        // The window's own alpha does NOT reach a swatch copy: a harmony swatch has no
        // transparency of its own, and copying one as half transparent would be inventing a
        // value the panel never showed.
        st.mode = ColorFormat::Hex;
        st.alpha = 128;
        st.color = c;
        // A swatch spells the alpha IT is drawn at (DRAGON-682 item 19): a harmony segment
        // takes the window's, a history entry takes its own, and an opaque one spells with
        // no alpha digits at all.
        assert_eq!(st.swatch_copy_text(c, 128), "#FF880080");
        assert_eq!(st.swatch_copy_text(c, u8::MAX), "#FF8800");
        assert_eq!(st.value_text(), "#FF880080", "the window's OWN value still carries it");
    }

    /// DRAGON-682 item 25: the window's raster INVENTORY is complete, and it is what
    /// `App::ensure_all_rasters` asserts against before a window opens.
    ///
    /// This is the test that replaces a bug that shipped twice: rasters were built by
    /// whichever handler changed their inputs, so a window opened from the palette viewer
    /// had no history swatches, no empty-slot dots and no harmony checkerboard until the
    /// user removed a recent. An `image::Handle` is plain data with no GPU behind it, so a
    /// test can fill every field and prove the inventory names exactly the fields that
    /// exist: a raster added to the state without a line here leaves this test passing on a
    /// fresh state and failing on a full one.
    #[test]
    fn a_fresh_state_is_missing_every_raster_and_a_full_one_is_missing_none() {
        // A colour with transparency, which is the one history entry that NEEDS a raster.
        let mut st = ColorPickerState {
            recents: vec![
                geom::Recent::opaque(Srgb::new(1, 2, 3)),
                geom::Recent::new(Srgb::new(4, 5, 6), 128),
            ],
            ..Default::default()
        };
        let missing = st.missing_rasters();
        for want in [
            "sv square",
            "hue strip",
            "alpha strip",
            "round swatch",
            "empty history slot",
            "harmony checkerboard",
            "empty palette outline",
            "a translucent history swatch",
        ] {
            assert!(missing.contains(&want), "the inventory does not name {want:?}");
        }
        assert_eq!(missing.len(), 8, "the inventory named something unexpected: {missing:?}");

        // Fill every one, the way `ensure_all_rasters` does.
        let handle = || widget::image::Handle::from_rgba(1, 1, vec![0u8; 4]);
        st.sv_raster = Some(handle());
        st.hue_raster = Some(handle());
        st.alpha_raster = Some(handle());
        st.swatch_raster = Some(handle());
        st.empty_slot_raster = Some(handle());
        st.checker_bar_raster = Some(handle());
        st.empty_palette_raster = Some(handle());
        st.recent_rasters = vec![None, Some(handle())];
        assert!(st.missing_rasters().is_empty(), "still missing {:?}", st.missing_rasters());

        // An OPAQUE entry legitimately has no raster of its own, so a `None` beside it is
        // not a gap; a translucent one's `None` is.
        st.recent_rasters = vec![None, None];
        assert_eq!(st.missing_rasters(), vec!["a translucent history swatch"]);
        // A recents list longer than the raster list is the same gap by another route,
        // which is exactly what a stale refresh leaves behind.
        st.recent_rasters = vec![None];
        assert_eq!(st.missing_rasters(), vec!["a translucent history swatch"]);
    }

    /// Every box a mode can show has its OWN focus id, and the ids are stable
    /// (DRAGON-680). Tab moves between the boxes by focusing these, and the window
    /// focuses the first one on open and after a mode change, so two boxes sharing an id
    /// would silently stop the cycle.
    #[test]
    fn every_box_has_its_own_stable_focus_id() {
        let mut st = ColorPickerState::default();
        assert_eq!(st.box_ids.len(), geom::MAX_VALUE_BOXES);
        // Stable across reads: the same box answers the same id twice, which is what
        // makes a focus task issued in `update` land on the box the next `view` builds.
        assert_eq!(st.box_id(0), st.box_id(0), "the same box answers the same id twice");
        for mode in ColorFormat::ALL {
            st.mode = mode;
            for pos in 0..st.box_count() {
                assert!(st.box_id(pos).is_some(), "{}: box {pos} has no id", mode.id());
            }
        }
        // And they are distinct, or "the next box" would be "this box".
        let ids: Vec<String> = st.box_ids.iter().map(|i| format!("{i:?}")).collect();
        let mut uniq = ids.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), ids.len(), "two boxes share a focus id: {ids:?}");
    }
}
