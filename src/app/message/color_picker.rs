//! `ColorPickerMsg` — the colour picker tool's message domain (DRAGON-582).
//!
//! Two surfaces, one domain: the dimmed picker OVERLAY (move, pick, cancel) and the
//! result WINDOW (edit a row, copy a row, load a recent, close). They share a domain
//! because they share one state machine and one colour; splitting them would mean two
//! enums whose handlers both reach into the same `ColorPickerState`.

/// The colour picker's messages.
#[derive(Debug, Clone)]
pub enum ColorPickerMsg {
    /// The pointer moved over the named output's picker overlay, to this SURFACE-LOCAL
    /// point. RECORDS the position (and drops any keyboard nudge); the sample and the
    /// magnifier's re-raster then happen on the next [`Self::ResamplePoll`] (DRAGON-TBD),
    /// EXCEPT while there is no hover yet, where the first sample still runs inline so the
    /// loupe appears at once and the accept key never sees a momentary "unreadable".
    Moved(String, (f32, f32)),
    /// DRAGON-TBD: re-read the pixel under the freshest recorded pointer, and re-raster the
    /// magnifier if the picture changed and the pointer is not sweeping past it.
    ///
    /// Published by `widgets::color_pick::ColorPickSurface` from the REDRAW it is already
    /// handed, once per presented frame, while `ColorPickerState::resample_due` is set.
    ///
    /// It exists because raw pointer motion arrives FASTER than the screen is redrawn, most
    /// visibly on macOS, where the OS does not couple the two: doing the work on
    /// [`Self::Moved`] built a raster per event and threw away every one but the last before
    /// each frame. The name still says "Poll" from the version that WAS a timer; the timer is
    /// gone (see the tombstone in `subscriptions.rs`) because 16ms is 62.5Hz and this app runs
    /// on 120Hz displays, so the lens updated on fewer than half the frames and a sweep read as
    /// stepped. A redraw needs no number and tracks a variable-refresh panel for free.
    ///
    /// Cross-platform, with no `cfg`: every platform redraws, and the mismatch between event
    /// rate and frame rate is not proven to be macOS-only.
    ResamplePoll,
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
    /// DRAGON-630: the saturation/value square moved, to this normalized position
    /// (`x` = saturation `0..1`, `y` = 1 - value, both in field units straight from
    /// `widgets::color_field`). The window derives the colour from its tracked HSV.
    SvChanged(f32, f32),
    /// DRAGON-630: the hue strip moved, to this normalized position (`x * 360` degrees).
    HueChanged(f32),
    /// DRAGON-630: the alpha strip moved, to this normalized alpha (`0..1`).
    AlphaChanged(f32),
    /// DRAGON-630: the mode DROPDOWN chose the notation at this index of
    /// `ColorFormat::ALL`. ONE control that opens a picker, by the owner's review: a
    /// first pass shipped two separate chevron step buttons here, and the owner asked
    /// for the combined widget with a dropdown instead. Selecting a different mode
    /// persists it and copies the value in the new mode's spelling; re-selecting the
    /// current one changes nothing and copies nothing.
    ModeSelected(usize),
    /// A value BOX's text changed (DRAGON-630 turned the seven full-string rows into
    /// per-component boxes; the index counts the mode's own components with the alpha
    /// box one past them). Held as a DRAFT while the box is being edited, so the user's
    /// half-typed value is never rewritten under the caret; every other box, the square,
    /// the strips and the swatch follow it the moment it parses.
    BoxEdited(usize, String),
    /// The edited box was submitted (Enter). Drops the draft, so the box re-renders in
    /// its canonical spelling.
    BoxCommitted,
    /// DRAGON-630 rev 3: the value row's layout toggle, between the split per-channel
    /// boxes and the one whole-value box. Persisted like the mode: the picker is
    /// one-shot, and the owner asked for the remembered state.
    InputLayoutToggled,
    /// DRAGON-630 rev 4: open or close the mode chip's menu. The chip replaced the
    /// stock dropdown widget on the owner's review: the stock control could not take
    /// the icon-button hover wash, a fixed text span, or the app's own lucide caret,
    /// so the chip + `chrome::flyout` recipe (the zoom combo's) stands in its place.
    ModeMenuToggled,
    /// Copy the current mode's value to the clipboard, replacing whatever the pick put
    /// there.
    CopyValue,
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
