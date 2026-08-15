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
    /// DRAGON-680: step the value row's notation by this many places in
    /// `ColorFormat::ALL`, wrapping. `-1` is ArrowUp (the previous notation) and `+1`
    /// ArrowDown.
    ///
    /// Sent by the ARROW KEYS while the mode activator holds focus, which is the keyboard
    /// half of that control (its click half opens [`Self::ModeMenuToggled`]'s menu). The
    /// wrapping walk is `ColorFormat::cycled`, over `keynav::step`, so the notation list
    /// and every other keyboard-navigable list in the app wrap by one rule.
    ///
    /// Stepping PERSISTS the new mode, copies NOTHING (the owner's ask: cycling notations
    /// to LOOK at them used to overwrite the clipboard seven times) and does NOT move
    /// focus, so a second arrow steps again instead of landing in a text box.
    ModeStepped(i32),
    /// DRAGON-630 rev 4: open or close the mode activator's menu, the list of the seven
    /// notations. Restored by DRAGON-680 after that ticket briefly read the activator as a
    /// stepper and deleted it; the owner's correction is that the two chevrons "together
    /// act as a single hoverable unit that triggers the dropdown menu".
    ModeMenuToggled,
    /// DRAGON-630: the mode MENU chose the notation at this index of `ColorFormat::ALL`.
    ///
    /// Persists it, closes the menu and focuses the first value box (DRAGON-680: a choice
    /// made through the menu ends the interaction, so focus belongs back in the row). It
    /// copies NOTHING, the same rule [`Self::ModeStepped`] follows; re-selecting the
    /// current mode changes nothing at all.
    ModeSelected(usize),
    /// DRAGON-680: move the window's focus one stop around its own Tab ring
    /// (`geom::next_focus`), `true` forward. Sent by Tab and Shift+Tab while the picker
    /// window has the press.
    FocusStep(bool),
    /// An arrow key while the colour HISTORY holds focus: move the navigation CURSOR one
    /// step through the grid in reading order (`keynav::grid_step`).
    ///
    /// It LOADED the swatch it landed on until DRAGON-682 item 7, and the owner reversed
    /// that: arrowing the history now only moves a highlight, and [`Self::HistoryApply`]
    /// (Space or Enter) is what takes it.
    HistoryArrow(crate::shortcuts::Direction),
    /// DRAGON-680: the value box at this ROW POSITION took focus from a CLICK.
    ///
    /// It exists for the select-all rule ("focusing a new input box automatically selects
    /// all text in that box") and covers exactly the half `select_on_focus` cannot: that
    /// flag is consulted by libcosmic's `State::focus`, which a focus OPERATION calls
    /// (Tab, Shift+Tab, and our own focus tasks) but a mouse press does not, since the
    /// press places the caret where you clicked. The handler answers with
    /// `text_input::select_all`, so both routes end the same way.
    ///
    /// The position is the box's place in the row, NOT its component index: hex's single
    /// box is position 0 and component `WHOLE_VALUE_BOX`.
    BoxFocused(usize),
    /// A value BOX's text changed (DRAGON-630 turned the seven full-string rows into
    /// per-component boxes; the index counts the mode's own components with the alpha
    /// box one past them). Held as a DRAFT while the box is being edited, so the user's
    /// half-typed value is never rewritten under the caret; every other box, the square,
    /// the strips and the swatch follow it the moment it parses.
    BoxEdited(usize, String),
    /// The edited box was submitted (plain Enter). Drops the draft, so the box re-renders
    /// in its CANONICAL spelling: hex letters uppercased, numbers respelled the way the
    /// formatter writes them. That reformat is the whole of what plain Enter does now.
    ///
    /// **It used to ALSO file the colour into the history** (DRAGON-665: "Enter in a value
    /// box is the one gesture here that says 'this is the colour I meant'"). DRAGON-680
    /// took that half away, on the owner's ask: Enter is pressed while typing, so filing
    /// on it wrote history nobody asked for. Adding is the primary+Enter chord now
    /// (`shortcuts::is_add_color_chord`), which the "Add to recents" button advertises.
    BoxCommitted,
    /// Copy the current mode's value to the clipboard, replacing whatever the pick put
    /// there.
    CopyValue,
    /// DRAGON-587: the pipette on the recents row — start a NEW pick, exactly as launching
    /// the tool does. Spawns a detached `--color-picker` child, the same route the tray entry
    /// and the preview editor's own pipette take.
    PickAgain,
    /// DRAGON-680 item 24: open or close the colour history's context menu over the swatch
    /// at this index. A right press opens it; the flyout's own dismissal and Escape close
    /// it, exactly as the notation menu closes.
    RecentsMenu(Option<usize>),
    /// DRAGON-680 item 24: the pointer entered the history swatch at this index.
    RecentHovered(usize),
    /// DRAGON-680 item 24: the pointer left the history swatch at this index. It names the
    /// swatch it is leaving so that an ENTER already recorded for the neighbour is not
    /// cleared by the exit that follows it.
    RecentUnhovered(usize),
    /// DRAGON-680 item 24: forget the history entry at this index.
    ///
    /// ONE message for both routes, the context menu's "Remove from recents" and the
    /// Backspace / Delete key, so the write, the save and the raster refresh cannot drift
    /// apart. It is an explicit user WRITE of the recents, which `geom::writes_recents`
    /// does not govern: that rule is about which colour CHANGES may reorder the list
    /// behind the user's back.
    RemoveRecent(usize),
    /// DRAGON-682: open or close the panel, which doubles the window's width.
    /// Persists the new state and resizes the window.
    TogglePanel,
    /// DRAGON-682: the tab strip activated this ENTITY.
    ///
    /// An entity rather than a `PanelTab` because the strip is libcosmic's `tab_bar` over a
    /// `segmented_button` model (item 12), whose `on_activate` speaks entities; the handler
    /// reads the tab off the model's own data. Persisted, like the value row's notation.
    PanelTab(cosmic::widget::segmented_button::Entity),
    /// DRAGON-682 item 7: apply the swatch the history's cursor is on, exactly as a click
    /// on it would (colour and alpha, no history write, no clipboard).
    HistoryApply,
    /// DRAGON-682 item 9: move the PANEL's navigation cursor one step through the harmony
    /// cards. Nothing else happens: a panel swatch has no primary action.
    PanelCursor(crate::shortcuts::Direction),
    /// DRAGON-682: open or close a harmony swatch's context menu, over the swatch at this
    /// `(group, index)`. The menu is the only interaction a panel swatch has.
    PanelMenu(Option<(usize, usize)>),
    /// DRAGON-682: take this colour, at this alpha, as the window's current one.
    ///
    /// The SOURCE rides along because it decides what else happens: a harmony swatch also
    /// files the colour into the recents and a history swatch does not, and that is the
    /// source table's answer (`geom::writes_recents`) rather than two call sites doing
    /// different things (item 22).
    SetActiveColor(crate::color::Srgb, u8, crate::app::color_picker::geom::ColorSource),
    /// DRAGON-682 item 28: file this colour, at this alpha, into the recents, WITHOUT
    /// touching the active colour.
    ///
    /// The harmony menu's middle entry. It goes through the same one write path the divider
    /// button and its chord use (`App::add_to_history`), so the newest-first order, the
    /// duplicate rule, the cap and the save cannot fork between them; what it does NOT do is
    /// anything else, which is the owner's "without messing up the active color".
    AddColorToRecents(crate::color::Srgb, u8),
    /// DRAGON-682: copy this colour, at this alpha, to the clipboard, spelled in the
    /// REMEMBERED notation (`ColorPickerState::swatch_copy_text`).
    ///
    /// It does NOT raise the copy button's flash (item 15): `geom::copy_flashes` carries
    /// why. Sent by both swatch menus.
    CopyColor(crate::color::Srgb, u8),
    /// DRAGON-682 item 35: a swatch was PRESSED, and names itself.
    ///
    /// Sent by the three swatch widgets themselves (a harmony segment, a filled history
    /// entry, the round active swatch), so the source is the pressed thing rather than
    /// something looked up afterwards. Item 41 made it so: it used to be a bare "a press
    /// happened in this window" that resolved the source from hover bookkeeping, and hover
    /// bookkeeping leaks (see `geom`'s tombstone at `drag_source`), so a press ANYWHERE could
    /// arm a drag carrying the wrong swatch. Nothing else in the window sends this.
    DragPressed(crate::app::color_picker::geom::DragSource),
    /// A history swatch saw the left button come UP over it (DRAGON-682 item 41).
    ///
    /// This is where a click LOADS the colour now. The swatch used to be a button, whose
    /// press fires on release anyway; it cannot be one any more, because a cosmic button
    /// captures the left press and the drag machine has to see that press to know what was
    /// picked up. `geom::completes_click` is the rule: same swatch, pressed and released,
    /// with no travel in between.
    RecentReleased(usize),
    /// The pointer moved while a drag is armed, in WINDOW coordinates.
    ///
    /// The first one sets the origin (a press event has no position), and the one that
    /// passes `geom::DRAG_THRESHOLD` promotes the drag to live.
    DragMoved(f32, f32),
    /// The left button came up: resolve the drag through `geom::drop_action` and end it.
    DragReleased,
    /// End the drag having done NOTHING (Escape, or the window losing the pointer grab).
    DragCancelled,
    /// DRAGON-687 item five: set this swatch as the CURRENT colour, bumping the previous
    /// one into the recents first (`geom::swatch_click_outcome`, the one bump-only
    /// decision). Sent by a plain CLICK on a harmony or saved-palette swatch AND by
    /// those two surfaces' right-click "Set as active color" rows, one path, so a menu
    /// and a click cannot disagree. The recents' own swatches deliberately do not send
    /// this: their click is still a plain load.
    ApplySwatch(crate::color::Srgb, u8),
    /// A HARMONY segment saw the left button come up over it (DRAGON-687 item five):
    /// `RecentReleased`'s exact shape, completing the click that now applies-and-bumps.
    PanelSwatchReleased(usize, usize),
    /// A SAVED-PALETTE segment saw the left button come up over it: same machine.
    PaletteSwatchReleased(usize, usize),
    /// DRAGON-682 item 32: copy the swatch the PANEL's cursor is on, exactly as that
    /// swatch's right-click Copy does.
    ///
    /// It carries no colour: the cursor names a position in a grid that is recomputed from
    /// the window's own colour every frame, so the handler reads the swatch there rather
    /// than a value that could be one frame stale. Sent by Space and Enter while the panel
    /// holds the focus ring (`geom::accept_action`).
    CopyPanelCursor,
    /// Load the recent colour at this index. LOADS only: it never reorders, promotes or
    /// re-adds (see `color_picker::geom::writes_recents`).
    LoadRecent(usize),
    /// File the colour the window is showing, WITH its alpha (DRAGON-680), into the
    /// history: the "Add to recents" button on the divider, and the primary+Enter chord that
    /// button advertises.
    ///
    /// It exists because the history is written by PICKS only, and everything the window
    /// can do to a colour after the pick (the square, the strips, the value boxes, a
    /// loaded recent) deliberately does not write it. That rule is right, and it left no
    /// way at all to keep a colour you MADE. This is that way, and it is the only one:
    /// the button files the shown colour under exactly the rule a pick files one
    /// (`geom::push_recent`, same de-duplication, same cap), and changes nothing else
    /// about the window.
    AddToHistory,
    /// DRAGON-682 item 30: clear the harmony segment's local "Copied!" card.
    ///
    /// Its own message rather than the main flash's, because the two are separate
    /// acknowledgements with separate lifetimes: the copy button's flash is not raised by a
    /// swatch copy at all (item 15), and this one is anchored to a swatch.
    ClearSwatchCopied,
    /// Clear the transient "Copied" note.
    ClearCopied,
    /// DRAGON-587: the bounded wait for the result window's keyboard focus is over. A no-op
    /// unless the pick's hex copy is still waiting (`ColorPickerState::copy_waiting`), in
    /// which case the copy is reported as missed rather than quietly forgotten.
    PickCopyDeadline,
    // ── Saved palettes (DRAGON-687) ─────────────────────────────────────────
    /// The tab's "New Palette" button: create an empty group named by
    /// `geom::default_palette_name` and open its name for editing at once, selected, so
    /// typing replaces the placeholder name without a second click.
    CreatePalette,
    /// Begin renaming group `usize` inline: a click on its name (the release half of a
    /// press that never travelled, like a history swatch's own click). Any rename already
    /// open commits first.
    RenameStarted(usize),
    /// The rename editor's text changed: the live draft, never rewritten under the caret.
    RenameEdited(String),
    /// COMMIT the rename (Enter, or an interaction that moves focus). Trimmed; an empty
    /// or unchanged name keeps the old one (`geom::palette_rename`).
    RenameCommitted,
    /// REVERT the rename (Escape): the draft is dropped and the old name stands.
    RenameCancelled,
    /// A group NAME saw the left button come up over it: the click half of the
    /// press-names-itself machine, exactly `RecentReleased`'s shape. A completed click
    /// starts the rename. Carries the visible ROW (item six); the handler resolves the
    /// real group.
    GroupNameReleased(usize),
    /// Open or close a group name's context menu (Delete palette). By visible ROW: the
    /// view compares rows to know where the flyout hangs.
    GroupMenu(Option<usize>),
    /// The create row's SORT icon (item six): open or close the six-sorts flyout, which
    /// lived in the group-name menus until the owner moved it to the toolbar.
    SortMenu(bool),
    /// The create row's SEARCH icon (item six): expand the collapsed icon into the
    /// settings-style field and focus it, whole text selected.
    PaletteSearchActivate,
    /// The search field's text changed: the live filter, applied per keystroke.
    PaletteSearchInput(String),
    /// The field's clear button, and Escape while searching: empty the filter and
    /// collapse back to the icon (the settings search's own clear behaviour).
    PaletteSearchClear,
    /// The search field lost focus with nothing typed: collapse back to the icon. A
    /// non-empty query keeps the field up, because collapsing would silently keep
    /// filtering.
    PaletteSearchUnfocused,
    /// Open or close a palette swatch's context menu, over `(group, index)`.
    PaletteSwatchMenu(Option<(usize, usize)>),
    /// Turn the OPEN context menu to a submenu page (or back to Root). One message for
    /// all four pages because only one menu is ever open (`geom::MenuPage`).
    MenuPageChanged(crate::app::color_picker::geom::MenuPage),
    /// The per-palette PLUS button: append the window's CURRENT colour (with its alpha)
    /// to group `usize`, at the end, unless the group already holds it.
    AddActiveToPalette(usize),
    /// The per-palette PIPETTE button (DRAGON-687 follow-up): start a screen pick whose
    /// colour lands DIRECTLY in group `usize`, never on the main tool swatch.
    ///
    /// The pick is its own process, so the handler mints a nonce, snapshots the group's
    /// identity beside it, and spawns the ordinary `--color-picker` child with
    /// `COLOR_TO_PALETTE_ENV` set; the child delivers back over the picker IPC's tagged
    /// `color` verb and this window appends through the one shared append. See
    /// `PickDestination::PickerPalette` for what the child deliberately does NOT do
    /// (active colour, recents, clipboard).
    PickToPalette(usize),
    /// Append this colour to group `usize`: the "Add to palette ›" submenu rows and every
    /// colour DROP on a group. Appends at the END, duplicates are a no-op.
    AddColorToPalette(usize, crate::color::Srgb, u8),
    /// MOVE a palette colour to another group: removed from `from`, appended at `to`'s
    /// end. The menu's "Move to palette ›" rows ONLY, since the owner's reversal made
    /// the cross-group drag a copy: this is the explicit vacating form.
    MovePaletteColor { from: (usize, usize), to: usize },
    /// COPY a palette colour to another group (the "Copy to palette ›" rows, and a drag
    /// between groups, the owner's reversal): the source stays, the target appends
    /// through the one guarded admit.
    CopyPaletteColor { from: (usize, usize), to: usize },
    /// Reorder a colour within its group: the intra-bar drag's drop. `to` is an insertion
    /// slot in the group's original order.
    ReorderPaletteColor { group: usize, from: usize, to: usize },
    /// Forget one palette colour: a swatch dragged off the window. No confirmation
    /// (colours never confirm; groups do).
    RemovePaletteColor(usize, usize),
    /// Reorder the groups: a name drag's drop. `to` is an insertion slot in the original
    /// group order.
    ReorderGroup { from: usize, to: usize },
    /// ASK to delete group `usize`: the menu's "Delete palette" and the name dragged off
    /// the window both land here, and both wait on the dialog.
    RequestDeleteGroup(usize),
    /// The confirmation's answer: `true` deletes the pending group, `false` keeps it.
    /// Either way the dialog closes.
    ConfirmDeleteGroup(bool),
    /// Apply one of the six sorts to the group list (the sort flyout's rows; the
    /// create-row icon since item six, a group-menu submenu before it).
    SortGroups(crate::app::color_picker::geom::PaletteSort),
    /// The MAIN round swatch's context menu (item seven): open or close. Its rows are
    /// pinned by `geom::main_swatch_menu_labels`: recents, the gated palette submenu,
    /// copy, and deliberately no set-active.
    MainSwatchMenu(bool),
    /// The pointer moved over the picker WINDOW, at this window position (DRAGON-687):
    /// the one root-level report the hover pencil derives from
    /// (`geom::hovered_palette_title_at`). WINDOW-wide since the pencil's second
    /// stranding: a report scoped to the scrolled content went silent the moment the
    /// pointer left that region upward, freezing the last inside-a-title position; the
    /// root's `on_move` reports wherever the pointer is, and every position outside the
    /// content region maps to no title.
    WindowPointerMoved(f32, f32),
    /// The pointer left the window (best effort: a starved exit only leaves a stale
    /// POSITION at the window's edge padding, which maps to no title anyway, and the
    /// next entry corrects it).
    WindowPointerLeft,
    /// The panel scrollable reported its offset (`on_scroll`): keep the window's mirror
    /// in step, because the drop machine hit-tests through it.
    PanelScrolled(f32),
    /// The drag auto-scroll's tick (the owner's addendum): while a live drag sits in an
    /// edge band, walk the scroll one step and re-derive the drop highlight under the
    /// moved content. Driven by `sub_picker_drag_autoscroll`, which exists only while a
    /// live drag's pointer is actually in a band.
    DragAutoScroll,
    /// Ctrl+Tab / Ctrl+Shift+Tab (the owner's second addendum): activate the next
    /// (`true`) or previous panel tab, wrapping, exactly as clicking it would, persisted
    /// write included. A no-op while the panel is collapsed
    /// (`geom::panel_tab_after_cycle`).
    CyclePanelTab(bool),
}
