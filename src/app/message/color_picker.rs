//! `ColorPickerMsg` — the colour picker tool's message domain (DRAGON-582).
//!
//! Two surfaces, one domain: the dimmed picker OVERLAY (move, pick, cancel) and the
//! result WINDOW (edit a row, copy a row, load a recent, close). They share a domain
//! because they share one state machine and one colour; splitting them would mean two
//! enums whose handlers both reach into the same `ColorPickerState`.

use crate::color::ColorFormat;

/// The colour picker's messages.
#[derive(Debug, Clone)]
pub enum ColorPickerMsg {
    /// The pointer moved over the named output's picker overlay, to this SURFACE-LOCAL
    /// point. Re-samples the pixel and, when the sampled PIXEL changed, re-rasterises the
    /// magnifier disc.
    Moved(String, (f32, f32)),
    // DRAGON-601 added a `PointerSeed` variant here, carrying a launch-time pointer position
    // read on a throwaway connection, because on a Wayland layer surface the toolkit passed on
    // neither the pointer's arrival nor its position until it MOVED. DRAGON-609 deleted it.
    //
    // Keep the reason, because the variant looked reasonable and would be re-proposed: the
    // seed never fired. Its probe waited for a `wl_pointer.enter`, and the enter is exactly
    // what cosmic-comp fails to terminate with the mandatory `wl_pointer.frame`, so the probe
    // was beaten by the same defect it was written to route around. The fix belongs in the
    // pointer dispatch, not in a second opinion about where the pointer is; `Moved` above now
    // arrives on the enter, as it always should have.
    /// A left click on the named output's overlay at this surface-local point: sample,
    /// copy the hex, tear the overlays down and open the result window.
    Pick(String, (f32, f32)),
    /// DRAGON-587: change the magnifier's magnification by this many NOTCHES, positive = in.
    ///
    /// One message for all three routes (the trackpad gesture, the mouse wheel and the numpad
    /// `+` / `-`), because they are the same intent arriving three ways. The clamp lives in
    /// `geom::zoom_after_step`, so a route cannot widen the range by shouting louder.
    ///
    /// The "trackpad gesture" above is a two-finger SCROLL, arriving as an ordinary iced
    /// `WheelScrolled` event (`widgets::color_pick`), not a true pinch/magnify gesture: iced and
    /// winit surface no such event at all. [`Self::PinchPoll`] is the separate route for that.
    Zoom(i32),
    /// macOS: drain any pending trackpad pinch magnification and apply it as a zoom, in whole
    /// notches (`geom::pinch_notches`). Fired by `App::sub_color_picker_pinch` while the
    /// picker's dimmed overlay is open.
    ///
    /// `PreviewMsg::PinchPoll`'s exact shape, reading from the same
    /// `platform::mac::pinch` gesture recognizer, but converted to a discrete notch count
    /// rather than a continuous zoom step, since [`Self::Zoom`] takes notches, not a float.
    #[cfg(target_os = "macos")]
    PinchPoll,
    /// DRAGON-599: move the SAMPLE POINT one source pixel, from an arrow key or its vim letter
    /// (`shortcuts::nudge_direction`).
    ///
    /// It moves the sample, NOT the pointer, because a Wayland client cannot warp the pointer.
    /// The offset is held in `ColorPickerState::nudge` and a real pointer motion resets it
    /// (`geom::nudge_after`), so the lens can never drift permanently away from the cursor.
    Nudge(crate::shortcuts::Direction),
    /// A value row's text changed. Held as a DRAFT while the row is being edited, so the
    /// user's half-typed value is never rewritten under the caret; every other row and
    /// the swatch follow it the moment it parses.
    RowEdited(ColorFormat, String),
    /// The edited row was submitted (Enter). Drops the draft, so the row re-renders in
    /// its canonical spelling.
    RowCommitted,
    /// Copy this row's value to the clipboard, replacing whatever the pick put there.
    CopyRow(ColorFormat),
    /// DRAGON-587: the pipette on the recents row — start a NEW pick, exactly as launching
    /// the tool does. Spawns a detached `--color-picker` child, the same route the tray entry
    /// and the preview editor's own pipette take.
    PickAgain,
    /// Load the recent colour at this index. LOADS only: it never reorders, promotes or
    /// re-adds (see `color_picker::geom::writes_recents`).
    LoadRecent(usize),
    /// Clear the transient "Copied" note.
    ClearCopied,
    /// DRAGON-587: the bounded wait for the result window's keyboard focus is over. A no-op
    /// unless the pick's hex copy is still waiting (`ColorPickerState::copy_waiting`), in
    /// which case the copy is reported as missed rather than quietly forgotten.
    PickCopyDeadline,
}
