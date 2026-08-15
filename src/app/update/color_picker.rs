//! `ColorPickerMsg` handlers (DRAGON-582) — the picker overlay's pointer lane and the
//! result window's editing lane.
//!
//! The decisions these bodies apply are all pure and live in `app::color_picker::geom`
//! (which pixel, where the label goes, what may write the recents). What is here is the
//! wiring: sampling through the `PixelSource` seam, rasterising the magnifier only when
//! the sampled pixel actually changed, and the one-shot handover from overlay to window.
//!
//! # Privacy
//!
//! A picked colour is the user's content. Log lines here name the notation
//! (`ColorFormat::id`) or the event, never a colour value.

use super::super::*;
use crate::app::color_picker::{build_magnifier_raster, geom, Hover};
use crate::color::Srgb;

/// The corner radius the window's gradient rasters take, so the square and the tracks
/// follow the appearance page's "Edge rounding" exactly as the swatches do
/// (DRAGON-630, the owner's review): the same `s` token `view::swatch_radius` reads,
/// off the live theme the way `picker_ring` reads the accent.
fn picker_corner_radius() -> f64 {
    crate::app::theme::rounding(&cosmic::theme::active()).s[0] as f64
}

/// The round swatch's rim tone as raster bytes: the theme's SUBDUED tone
/// (`theme::subdued`), the owner's "subdued, not white/black" for swatch borders.
/// `theme::subtle` stood here first and the owner flagged it as brighter than asked.
fn picker_rim() -> [u8; 3] {
    let c = crate::app::theme::subdued(&cosmic::theme::active());
    [
        (c.r * 255.0).round().clamp(0.0, 255.0) as u8,
        (c.g * 255.0).round().clamp(0.0, 255.0) as u8,
        (c.b * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

/// May a resample leave the magnifier's RASTER stale to keep up with a fast pointer
/// (DRAGON-TBD)?
///
/// One question, asked once, because the answer is a property of the CALLER and not of the
/// picker's state. Getting it from the state instead is the mistake this type exists to make
/// impossible: the pacing measures pointer speed, and a keyboard nudge arriving while the hand
/// is still coasting would inherit that speed and have its raster deferred, so the arrow keys
/// would feel broken in exactly the moment the user reached for them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RasterPolicy {
    /// Rebuild the raster whenever the picture changed. Every route but the pointer's own
    /// per-frame one: the keyboard nudge (DRAGON-599), the frozen-snapshot delivery
    /// (DRAGON-601) and a session's first sample. All byte-identical to before the pacing.
    Always,
    /// The pointer's per-frame path, and ONLY it: may decline a raster while the pointer is
    /// sweeping (`geom::raster_due`), re-arming itself so the settle is never missed.
    Paced,
}

/// **Pure**, unit-tested: may this resample leave the magnifier's raster stale (DRAGON-TBD)?
///
/// One line, and it has its own name and its own tests because of what the FIRST term
/// guarantees: [`RasterPolicy::Always`] refuses to skip whatever the pointer is doing, so a
/// keyboard nudge pressed while the hand is still coasting from a sweep cannot inherit that
/// speed and have its raster deferred. That is the failure this seam exists to prevent, it is
/// invisible in a headless build, and a person reading the call site cannot tell it holds; a
/// test can.
fn skips_raster(
    policy: RasterPolicy,
    since_last_raster: Option<std::time::Duration>,
    speed: f32,
) -> bool {
    policy == RasterPolicy::Paced && !geom::raster_due(since_last_raster, speed)
}

impl App {
    pub(in crate::app) fn update_color_picker(
        &mut self,
        message: ColorPickerMsg,
    ) -> Task<cosmic::Action<Msg>> {
        match message {
            ColorPickerMsg::Moved(output, point) => {
                // DRAGON-599: the pointer really moved, so the sample belongs to the pointer
                // again and the keyboard's displacement is dropped. Recording the position and
                // resetting the nudge are the whole of this arm.
                //
                // DRAGON-TBD: and, once the lens exists, it is now nearly the whole arm. The
                // resample used to run right here, once per raw `CursorMoved`, and that is more
                // often than the screen is redrawn: every intermediate raster between two
                // presented frames is built and thrown away unseen, and macOS delivers pointer
                // motion uncoupled from the display's cadence, so it paid for most of them.
                // The lens is re-sampled once per RENDERED FRAME instead, by the redraw
                // published from `widgets::color_pick` while `resample_due` is set, so the
                // cadence is whatever the display actually runs at (120Hz on a ProMotion panel,
                // where a fixed 16ms timer visibly under-sampled and read as stepped motion).
                //
                // The bookkeeping stays synchronous because it is two field writes and because
                // `needs_pointer` (DRAGON-610) reads `pointer` on the very next redraw.
                //
                // DRAGON-650: this message itself now arrives at most once per PRESENTED
                // frame, coalesced by `widgets::color_pick` (its module doc, "Pointer reports
                // ride the redraw", carries the Windows present-starvation measurement that
                // forced it). Nothing here had to change for that: the arm was already shaped
                // for "record the latest position", however often it arrives.
                self.color_picker.nudge =
                    geom::nudge_after(self.color_picker.nudge, geom::SampleMove::Pointer);
                self.color_picker.pointer = Some((output.clone(), point));
                self.color_picker.resample_due = true;
                // …with ONE exception, and it is a correctness one rather than a latency one.
                // While there is NO hover yet, "the pointer is known, the snapshot has landed
                // and there is still no sample" is what `keyboard::picker_sample_state` reads
                // as UNREADABLE, and an accept key pressed in that gap would tell the user this
                // display cannot be read (DRAGON-612). Before the deferral that gap could not
                // exist, because the move that set the pointer also set the hover. So the FIRST
                // sample after any gap still runs inline, which is also what keeps the loupe
                // appearing the instant the pointer is located (DRAGON-609/610).
                //
                // Nothing is lost by it: with no hover there is nothing to coalesce, and every
                // reason a hover STAYS absent (the snapshot still in flight, a display with no
                // pixel source, a disc entirely off-surface) makes the resample return early
                // without rastering anything at all. The burst this defers is a moving pointer
                // over a hover that already exists, which is the whole of the reported problem.
                if self.color_picker.hover.is_none() {
                    self.color_picker.resample_due = false;
                    return self.color_picker_resample();
                }
                Task::none()
            }
            // DRAGON-TBD: one per RENDERED FRAME, published by `widgets::color_pick` from the
            // redraw it is already handed while `resample_due` is set (or riding the frame's
            // own coalesced `Moved`, DRAGON-650). It reads the pointer position that report
            // just recorded, so a burst of motion costs ONE look per frame rather than one
            // per event, and it arrives at the display's own cadence rather than a number we
            // picked.
            //
            // Clearing the flag FIRST is what stops the loop: iced re-dispatches its redraw
            // event after a message published from one, so a flag left set would publish again
            // in the same pass. The paced branch inside the resample is the one thing allowed
            // to set it back, which is precisely the "look again next frame" it needs.
            ColorPickerMsg::ResamplePoll => {
                self.color_picker.resample_due = false;
                self.color_picker_resample_with(RasterPolicy::Paced)
            }
            ColorPickerMsg::Pick(output, point) => self.color_picker_pick(&output, point),
            ColorPickerMsg::Zoom(steps) => self.color_picker_zoom(steps),
            // macOS: `PreviewMsg::PinchPoll`'s exact shape (`preview/mod.rs`). Ensure the
            // recognizer is attached (idempotent, cheap once installed), then drain the
            // accumulated magnification and reduce it to whole notches through
            // `geom::pinch_notches`, the same accumulate-a-remainder shape
            // `widgets::color_pick`'s own wheel handler uses, kept here instead of there
            // because this poll fires from the app/subscription layer, not from that widget's
            // `Event` handler. No pinch pending is a no-op, same as the preview route.
            #[cfg(target_os = "macos")]
            ColorPickerMsg::PinchPoll => {
                crate::platform::mac::pinch::install_pinch();
                let delta = crate::platform::mac::pinch::take_pinch();
                if delta == 0.0 {
                    return Task::none();
                }
                let (notches, remainder) =
                    geom::pinch_notches(self.color_picker.pinch_accum, delta);
                self.color_picker.pinch_accum = remainder;
                if notches == 0 {
                    return Task::none();
                }
                self.color_picker_zoom(notches)
            }
            // DRAGON-599: one source pixel, from an arrow key or its vim letter. It moves the
            // SAMPLE, never the pointer, which no Wayland client can move.
            ColorPickerMsg::Nudge(dir) => {
                let (dx, dy) = dir.delta();
                self.color_picker.nudge =
                    geom::nudge_after(self.color_picker.nudge, geom::SampleMove::Keys(dx, dy));
                self.color_picker_resample()
            }
            // DRAGON-630: the gradient square. The square hands back field units: x is
            // saturation, y runs DOWN while value runs up. The TRACKED HSV is the
            // master here, written before the colour, so a drag through the achromatic
            // edge cannot snap the hue (`color::hsv_tracking`), and `apply_picker_color`
            // (not `set_picker_color`) so the byte-quantised colour cannot re-track the
            // exact position the hand just chose.
            ColorPickerMsg::SvChanged(nx, ny) => {
                let hsv = [
                    self.color_picker.hsv[0],
                    nx.clamp(0.0, 1.0) as f64,
                    (1.0 - ny.clamp(0.0, 1.0)) as f64,
                ];
                self.color_picker.hsv = hsv;
                self.color_picker.draft = None;
                self.apply_picker_color(crate::color::srgb_from_hsv(hsv), None, geom::ColorSource::Edit);
                Task::none()
            }
            // DRAGON-630: the hue strip. Same master-HSV rule as the square; this is
            // also the one interaction that re-rasters the square (its whole picture is
            // the hue).
            ColorPickerMsg::HueChanged(nx) => {
                let mut hsv = self.color_picker.hsv;
                hsv[0] = (nx.clamp(0.0, 1.0) as f64) * 360.0;
                self.color_picker.hsv = hsv;
                self.color_picker.draft = None;
                self.refresh_sv_raster();
                self.apply_picker_color(crate::color::srgb_from_hsv(hsv), None, geom::ColorSource::Edit);
                Task::none()
            }
            // DRAGON-630: the alpha strip. Only the alpha moves: the colour, the recents
            // and the square stay put, and only the swatch disc re-rasters (the strip
            // itself paints the alpha RANGE, which does not depend on the alpha).
            ColorPickerMsg::AlphaChanged(na) => {
                self.color_picker.alpha = (na.clamp(0.0, 1.0) * 255.0).round() as u8;
                self.color_picker.draft = None;
                self.refresh_color_rasters();
                Task::none()
            }
            // DRAGON-680: the mode STEPPER's two chevrons. Persist (the remembered mode
            // is the owner's ask, and this is its one writer, so the save rides here),
            // drop the draft (it belonged to a box this mode may not even have), and
            // focus the first box of the new layout.
            //
            // **It copies NOTHING**, which reverses DRAGON-630's contract on the owner's
            // ask ("let's not automatically copy the color when we change the mode, but
            // we can still copy on open and on copy icon click"). Cycling the seven
            // notations to LOOK at them used to overwrite the clipboard seven times.
            //
            // A zero step, or a step that lands on the mode we are already in, still
            // re-focuses: the press landed, and the user's next keystroke should go
            // somewhere predictable either way. Nothing is saved when nothing moved.
            // DRAGON-680: the ARROW KEYS while the mode activator holds focus. Persist the
            // new notation and copy NOTHING (see the message's doc for both).
            //
            // Focus deliberately STAYS on the activator. Sending it back to the first box,
            // the way a menu SELECTION does, would mean a user could step exactly once
            // before the caret landed in a text field and the next arrow moved a caret
            // instead of the notation. The two are different gestures: the menu ends an
            // interaction, an arrow step continues one.
            ColorPickerMsg::ModeStepped(steps) => {
                let mode = self.color_picker.mode.cycled(steps);
                if mode == self.color_picker.mode {
                    return Task::none();
                }
                self.color_picker.mode = mode;
                self.color_picker.draft = None;
                self.save_state();
                Task::none()
            }
            // DRAGON-630 rev 4: the activator's menu. Transient view state, no save.
            ColorPickerMsg::ModeMenuToggled => {
                self.color_picker.mode_menu_open = !self.color_picker.mode_menu_open;
                Task::none()
            }
            // DRAGON-630: a notation chosen from the MENU. Persist it, close the menu, and
            // put focus back in the first value box (DRAGON-680): a choice made through the
            // menu ends the interaction, unlike an arrow step, which continues one.
            //
            // Re-selecting the current mode is a no-op beyond closing the menu: nothing
            // changed, so nothing is saved. It still copies NOTHING, which reverses
            // DRAGON-630's contract on the owner's ask.
            ColorPickerMsg::ModeSelected(idx) => {
                // A selection always CLOSES the menu, even the no-op re-selection of the
                // current mode: the click landed, so the menu's job is done.
                self.color_picker.mode_menu_open = false;
                let Some(mode) = crate::color::ColorFormat::ALL.get(idx).copied() else {
                    return Task::none();
                };
                if mode != self.color_picker.mode {
                    self.color_picker.mode = mode;
                    self.color_picker.draft = None;
                    self.save_state();
                }
                self.focus_first_value_box()
            }
            // DRAGON-680: Tab / Shift+Tab, one stop around the window's own focus ring.
            ColorPickerMsg::FocusStep(forward) => {
                let cp = &self.color_picker;
                let next = geom::next_focus(
                    cp.focus,
                    forward,
                    cp.box_count(),
                    !cp.recents.is_empty(),
                    // No panel, no panel stop: Tab must never land somewhere that is not on
                    // screen. Since item 42 that is the persisted flag itself.
                    cp.panel_mounted(),
                );
                self.apply_picker_focus(next)
            }
            // DRAGON-682 item 7: an arrow key while the HISTORY holds focus moves a
            // navigation CURSOR and nothing else.
            //
            // It used to LOAD each swatch as it passed, which is what the owner reversed:
            // "when we arrow navigate the recents, we shouldn't immediately update the
            // current swatch. only if we hit space or enter". Applying on the way past
            // costs a user the colour they had loaded just to LOOK at their history, and
            // there is no way back to it once the arrow has moved on.
            //
            // The cursor starts wherever `apply_picker_focus` seeded it (the shown entry,
            // or the first), so the first arrow steps from something visible.
            ColorPickerMsg::HistoryArrow(dir) => {
                let cp = &self.color_picker;
                if cp.recents.is_empty() {
                    return Task::none();
                }
                let at = cp.recent_cursor.unwrap_or_else(|| cp.selected_recent().unwrap_or(0));
                self.color_picker.recent_cursor = Some(crate::keynav::grid_step(
                    at,
                    dir,
                    cp.recents.len(),
                    geom::RECENTS_PER_ROW,
                ));
                Task::none()
            }
            // DRAGON-682 item 7: Space or Enter applies what the history's cursor is on,
            // through `LoadRecent`, which is the CLICK's own message. One loading path, so
            // the keyboard and the pointer cannot come to mean different things.
            ColorPickerMsg::HistoryApply => match self.color_picker.recent_cursor {
                Some(i) => self.update_color_picker(ColorPickerMsg::LoadRecent(i)),
                None => Task::none(),
            },
            // DRAGON-682 item 9: the PANEL's cursor. Moving it applies NOTHING, at any
            // point: the active colour is what the harmony cards are computed from, so a
            // cursor that changed it would be walking a grid that moves under it. Space and
            // Enter on the swatch it lands on COPY that swatch (item 32,
            // `ColorPickerMsg::CopyPanelCursor`), which changes no colour either.
            ColorPickerMsg::PanelCursor(dir) => {
                // The rows are whichever tab is showing (DRAGON-687): the harmony cards,
                // or the saved palettes' own lengths. `ragged_step` already skips empty
                // rows, so an empty palette is walked past rather than landed on.
                let rows = self.color_picker.panel_rows();
                if rows.iter().all(|n| *n == 0) {
                    return Task::none();
                }
                let at = self.color_picker.panel_cursor.unwrap_or((0, 0));
                let next = crate::keynav::ragged_step(at, dir, &rows);
                self.color_picker.panel_cursor = Some(next);
                // SCROLL the cursor into view (DRAGON-682 item 9). The panel scrolls, so a
                // cursor that walked past the fold would otherwise be invisible, and its
                // pinned hex card would be clipped by the scroll viewport with it. The
                // offset is the group's own top, PER TAB since the UX round gave the
                // palette groups their taller icon-bearing title row
                // (`palette_group_offset` vs `harmony_group_offset`), so a move WITHIN a
                // card scrolls nowhere new and a move between cards brings the whole
                // card on screen rather than just the swatch.
                //
                // The MIRROR follows the request (clamped to the tab's own max,
                // DRAGON-687), because the widget's `on_scroll` reports user scrolling
                // and cannot be relied on to echo an operation: a drag started right
                // after a keyboard walk must hit-test the offset the walk really left.
                let tab = self.color_picker.panel_tab;
                let raw = match tab {
                    geom::PanelTab::Harmonies => geom::harmony_group_offset(next.0),
                    geom::PanelTab::Palettes => geom::palette_group_offset(next.0),
                };
                let offset = raw.min(geom::panel_max_scroll_for(
                    tab,
                    self.color_picker.window_size().1,
                    // The VISIBLE rows (item six): the cursor walks the filtered list,
                    // and item nine's rule is that every clamp reads the one extent of
                    // what is actually laid out.
                    self.color_picker.visible_palettes().len(),
                ));
                self.color_picker.panel_scroll_y = offset;
                cosmic::iced::widget::scrollable::scroll_to(
                    self.color_picker.panel_scroll_id.clone(),
                    cosmic::iced::widget::scrollable::AbsoluteOffset {
                        x: Some(0.0),
                        y: Some(offset),
                    },
                )
            }
            // DRAGON-687 item five: click-to-apply, ONE path for the plain click and
            // the two panel menus' set-active rows. The bump (the outgoing colour into
            // the recents) is NOT here: item ten generalised it into
            // `apply_picker_color` itself (`geom::files_outgoing`), where every discrete
            // replacement gets it, so this handler only applies the clicked colour
            // exactly as a recents load does.
            ColorPickerMsg::ApplySwatch(c, alpha) => {
                self.close_swatch_menus();
                self.set_picker_color(c, Some(alpha), geom::ColorSource::RecentClick);
                Task::none()
            }
            // The click half of a harmony segment's press-names-itself machine
            // (`RecentReleased`'s exact rule): a sub-threshold press-release applies the
            // swatch; a real drag never does.
            ColorPickerMsg::PanelSwatchReleased(g, i) => {
                // `RecentReleased`'s exact ordering: decide, disarm, apply.
                let drag = self.color_picker.drag.map(|d| (d.source, d.live));
                if geom::release_disarms(drag) {
                    self.color_picker.drag = None;
                }
                if !geom::completes_click(drag, geom::DragSource::Harmony(g, i)) {
                    return Task::none();
                }
                let Some((c, alpha)) = self.color_picker.panel_swatch((g, i)) else {
                    return Task::none();
                };
                self.update_color_picker(ColorPickerMsg::ApplySwatch(c, alpha))
            }
            // And a saved-palette segment's, with the entry's own alpha.
            ColorPickerMsg::PaletteSwatchReleased(g, i) => {
                // `RecentReleased`'s exact ordering: decide, disarm, apply.
                let drag = self.color_picker.drag.map(|d| (d.source, d.live));
                if geom::release_disarms(drag) {
                    self.color_picker.drag = None;
                }
                if !geom::completes_click(drag, geom::DragSource::PaletteSwatch(g, i)) {
                    return Task::none();
                }
                let Some((c, alpha)) = self.color_picker.palette_swatch_at((g, i)) else {
                    return Task::none();
                };
                self.update_color_picker(ColorPickerMsg::ApplySwatch(c, alpha))
            }
            // DRAGON-682: the harmony swatch's context menu, and its two actions.
            ColorPickerMsg::PanelMenu(at) => {
                self.color_picker.panel_menu = at;
                // Every OPEN starts at the root page (DRAGON-687 follow-up): a submenu
                // page left behind by a click-away or Escape close must not become the
                // next right-click's first face. `geom::menu_page_on_open` is the
                // invariant; running it on close too is harmless hygiene.
                self.color_picker.menu_page =
                    geom::menu_page_on_open(self.color_picker.menu_page);
                Task::none()
            }
            // Exactly a recents click's load: the colour AND (for a harmony swatch) its
            // The colour AND its alpha, from either swatch menu. What ELSE happens is the
            // SOURCE's business (DRAGON-682 item 22): a harmony apply also files the colour
            // into the recents, a history one does not, and `geom::writes_recents` is the
            // single place that says which.
            ColorPickerMsg::SetActiveColor(c, alpha, source) => {
                self.close_swatch_menus();
                // The alpha travels WITH the colour (DRAGON-687 item ten): the apply
                // needs the outgoing pair intact for the bump, and the recents write
                // inside it still sees the alpha this colour is really taken at.
                self.set_picker_color(c, Some(alpha), source);
                Task::none()
            }
            // The clipboard, spelled in the REMEMBERED notation at the swatch's own alpha
            // (`swatch_copy_text`).
            // DRAGON-682 item 28: file a HARMONY swatch, and change nothing else.
            ColorPickerMsg::AddColorToRecents(c, alpha) => {
                self.close_swatch_menus();
                self.add_to_history(geom::Recent::new(c, alpha));
                Task::none()
            }
            ColorPickerMsg::CopyColor(c, alpha) => {
                // The anchor for the local card is the menu that sent this, read BEFORE it
                // closes (DRAGON-682 item 30). No segment travels in the message: the menu
                // is open over exactly one, and reading it here is what keeps the two from
                // disagreeing. Since DRAGON-687 that menu can be a harmony segment's OR a
                // palette swatch's; only one is ever open, so the first `Some` is the one.
                let at = self.color_picker.panel_menu.or(self.color_picker.palette_menu);
                self.copy_swatch(c, alpha, at)
            }
            // DRAGON-682 item 32: the KEYBOARD's copy, from the panel's cursor. It resolves
            // the swatch here (the grid is recomputed from the window's colour every frame)
            // and then goes through the SAME `copy_swatch` the menu entry does, so the
            // clipboard spelling, the missing button flash and the Copied! card cannot
            // drift between the two routes.
            ColorPickerMsg::CopyPanelCursor => {
                let Some(at) = self.color_picker.panel_cursor else {
                    return Task::none();
                };
                // Tab-aware since DRAGON-687: the cursor names a harmony swatch or a
                // saved palette's colour, and either copies exactly as its menu would.
                let Some((c, alpha)) = self.color_picker.panel_cursor_swatch(at) else {
                    return Task::none();
                };
                self.copy_swatch(c, alpha, Some(at))
            }
            // DRAGON-682: the panel itself. Both facts are persisted, and the WINDOW is
            // resized to match, which is the whole of the expand feature.
            //
            // NAIVE on purpose since item 42: flip the flag, ask for the size, done. The
            // panel is drawn from the flag alone, so it leaves the view on the same frame the
            // collapse is asked for and joins it on the frame the expand is. Items 31, 33 and
            // 34 sequenced this two ways to stop the transition jittering and neither
            // worked; `geom`'s tombstone beside `picker_column_w` records both attempts and
            // what to do instead.
            ColorPickerMsg::TogglePanel => {
                // A toggle moves the interaction on, so an open rename commits
                // (DRAGON-687): collapsing the panel takes its editor off screen.
                self.commit_rename_if_open();
                self.color_picker.expanded = !self.color_picker.expanded;
                // A closing panel cannot keep the focus ring's panel stop.
                if !self.color_picker.expanded
                    && self.color_picker.focus == Some(geom::PickerFocus::Panel)
                {
                    self.color_picker.focus = None;
                    self.color_picker.panel_cursor = None;
                }
                self.color_picker.panel_menu = None;
                self.color_picker.palette_menu = None;
                self.color_picker.group_menu = None;
                self.save_state();
                self.resize_color_picker_window()
            }
            ColorPickerMsg::PanelTab(entity) => {
                // The widget has already moved its own activation; what it hands back is
                // the entity, and the TAB rides on that entity's data (item 12), so the
                // enum this window reasons about and persists is read from the model rather
                // than guessed from an index.
                let Some(tab) =
                    self.color_picker.panel_tab_model.data::<geom::PanelTab>(entity).copied()
                else {
                    return Task::none();
                };
                self.set_panel_tab(tab)
            }
            // Ctrl+Tab / Ctrl+Shift+Tab (DRAGON-687, the owner's second addendum): the
            // next or previous tab, wrapping, through the SAME activation a click takes
            // (`set_panel_tab`), persisted write included. The collapsed no-op lives in
            // the pure decision.
            ColorPickerMsg::CyclePanelTab(forward) => {
                let Some(tab) = geom::panel_tab_after_cycle(
                    self.color_picker.panel_tab,
                    forward,
                    self.color_picker.panel_mounted(),
                ) else {
                    return Task::none();
                };
                self.set_panel_tab(tab)
            }
            // DRAGON-680: a CLICK focused this box, so select its whole text. See
            // `ColorPickerMsg::BoxFocused` for why the click needs a message at all when
            // Tab does not.
            ColorPickerMsg::BoxFocused(pos) => {
                // A click into a value box moves the interaction on, so an open rename
                // commits first (DRAGON-687).
                self.commit_rename_if_open();
                // The click moved the window's own focus too, or the ring would carry on
                // from wherever Tab last left it.
                self.color_picker.focus = Some(geom::PickerFocus::Box(pos));
                // A draft belongs to the box it was typed in. Focusing a DIFFERENT box
                // leaves the old one showing half-typed text it can no longer commit, so
                // it is dropped here and that box re-renders canonically. Dropping it
                // loses nothing: an unparseable draft never moved the colour, and a
                // parseable one already did.
                let mine = geom::draft_index(self.color_picker.mode, pos);
                if self
                    .color_picker
                    .draft
                    .as_ref()
                    .is_some_and(|(m, i, _)| *m != self.color_picker.mode || *i != mine)
                {
                    self.color_picker.draft = None;
                }
                match self.color_picker.box_id(pos) {
                    Some(id) => widget::text_input::select_all(id),
                    None => Task::none(),
                }
            }
            ColorPickerMsg::BoxEdited(idx, text) => {
                // The colour follows the box the moment the text PARSES; a value that
                // does not parse yet (half typed) leaves the colour where it was, so the
                // swatch never flashes through nonsense on the way to a valid value.
                // The WHOLE-VALUE box (the layout toggle's collapsed state) parses the
                // full spelling; a channel box parses its one component.
                let parsed = if idx == crate::app::color_picker::WHOLE_VALUE_BOX {
                    self.color_picker.mode.parse_with_alpha(&text)
                } else {
                    self.color_picker.mode.with_component(
                        self.color_picker.color,
                        self.color_picker.alpha,
                        idx,
                        &text,
                    )
                };
                if let Some((c, a)) = parsed {
                    self.set_picker_color(c, Some(a), geom::ColorSource::Edit);
                }
                self.color_picker.draft = Some((self.color_picker.mode, idx, text));
                Task::none()
            }
            ColorPickerMsg::BoxCommitted => {
                // Plain Enter: drop the draft so every box, including this one,
                // re-renders in its canonical spelling. That reformat IS the feature the
                // owner kept here ("enter can still cause the formatting to update like
                // it does now, like uppercasing hex letters"), and it needs no code of its
                // own: `box_text` falls back to the formatter the moment the draft is
                // gone.
                //
                // It used to ALSO file the colour into the history (DRAGON-665).
                // DRAGON-680 moved that onto the primary+Enter chord, because Enter is
                // pressed while typing and filing on it wrote history nobody asked for.
                // The chord is handled in `keyboard.rs` and reaches
                // `ColorPickerMsg::AddToHistory`, the same message the "Add to recents" button
                // sends, so there is still exactly one add path.
                self.color_picker.draft = None;
                Task::none()
            }
            ColorPickerMsg::CopyValue => self.copy_picker_value(),
            ColorPickerMsg::PickAgain => {
                // The SAME route every other launcher takes (the tray entry, the global
                // shortcut, the editor's toolbar pipette): a detached `--color-picker` child.
                //
                // A fresh LAUNCH is the point of this, not a shortcut around the work
                // (DRAGON-594, and this is the owner's own reason). A launch mints overlays
                // for the outputs it finds and freezes them then, so the next pick can come
                // from a DIFFERENT MONITOR. Re-entering the overlay in THIS process would
                // reuse what this session already holds and quietly pin the user to one
                // display. DO NOT "simplify" this into an in-process re-entry: the fresh
                // launch is carrying a decision the owner made deliberately, twice.
                //
                // Re-entry is also more expensive than it looks: it would mean re-grabbing the
                // frozen scene and, on the portal fallback, re-requesting a ScreenCast
                // mid-session. A launch already does all of that correctly.
                //
                // This window STAYS OPEN, and DRAGON-613 is what makes that the whole story:
                // the child hands its colour BACK to this window (we are listening on our
                // per-pid picker socket, advertised by our colour-picker marker, which also
                // keeps the sibling sweep off us), so the new colour lands here and becomes
                // the newest recent, and no second window appears. Fresh launch, one window:
                // the multi-monitor reason above and the single-window rule are satisfied by
                // changing only where the value is DELIVERED.
                //
                // The spawn stays best-effort with no channel back, so closing on the strength
                // of it would risk throwing away the colour the user has in hand for a child
                // that never appeared. A child that cannot reach us opens its own window,
                // which is the pre-DRAGON-613 behaviour and never a lost colour.
                log::debug!("color picker: the pipette started another pick");
                // `CCK_COLOR_TO_PID=0` says "no editor" EXPLICITLY, and it has to be said
                // rather than left unset (DRAGON-613). A child inherits our environment, and
                // this process may itself have been launched by an editor whose delivery
                // failed — that is exactly how a window comes to exist with the variable
                // still set. Without this the child would target that dead editor, fail, and
                // (before the ladder in `deliver_pick`) open a second window. `0` is already
                // rejected by `color_target_pid`, so this rides the existing rule instead of
                // adding one, and the spawn seam can only ADD variables, never clear them.
                crate::recording_ui::spawn_capture_child_args(
                    &["--color-picker"],
                    &[
                        (crate::app::color_picker::COLOR_TO_PID_ENV, "0"),
                        // And "no palette" (DRAGON-687 follow-up), for the same
                        // inheritance reason: this window may itself have been born from
                        // a palette-destined pick whose delivery failed, and its own
                        // pipette's children must not inherit that dead target.
                        (crate::app::color_picker::COLOR_TO_PALETTE_ENV, "0"),
                    ],
                );
                Task::none()
            }
            ColorPickerMsg::LoadRecent(index) => {
                // A LIVE drag swallows this (DRAGON-682 item 37). The swatch is a cosmic
                // button, so it fires its press on RELEASE while the pointer is over it,
                // which is exactly the release that ends a drag that started here and came
                // back. The drop's own action is dispatched through this same message from
                // `DragReleased`, AFTER the drag has ended, so a real load still lands.
                if self.color_picker.dragging() {
                    return Task::none();
                }
                // LOADS only. `set_picker_color`'s `RecentClick` source is what stops
                // this reordering the list (`geom::writes_recents`); the LOADED entry
                // neither rewrites nor moves. Item ten's bump inside the apply is the
                // one addition: a predecessor the history does not hold files first, so
                // no colour goes missing. The alpha rides in the call (DRAGON-680 kept
                // it; item ten moved it from a follow-up write into the parameter).
                if let Some(entry) = self.color_picker.recents.get(index).copied() {
                    self.set_picker_color(
                        entry.color,
                        Some(entry.alpha),
                        geom::ColorSource::RecentClick,
                    );
                    self.refresh_color_rasters();
                }
                Task::none()
            }
            // DRAGON-680 item 24: the history's context menu. Transient view state, no save,
            // exactly like the notation menu's toggle.
            ColorPickerMsg::RecentsMenu(at) => {
                self.color_picker.recents_menu = at;
                // The root-page-on-open invariant, `PanelMenu`'s own reason.
                self.color_picker.menu_page =
                    geom::menu_page_on_open(self.color_picker.menu_page);
                Task::none()
            }
            // ── The DRAG machine (DRAGON-682 items 35 to 41) ────────────────────────
            //
            // Four messages and one state. The pure decisions all live in `geom`: whether a
            // source is real (`arms_drag`), when a press becomes a drag (`drag_is_live`),
            // which zone a point is in (`drop_zone`), whether it is off the window at all
            // (`off_window`), what the pair MEANS (`drop_action`), and which zone to light
            // (`zone_highlight`, which asks `drop_action` so the highlight cannot promise
            // something the drop will not do).
            //
            // The press names its OWN source (item 41). It used to be read from the window's
            // hover bookkeeping, which is not reliable: `mouse_area` captures the move that
            // enters it and every other `mouse_area` bails on a captured event before
            // publishing its own exit, so "the pointer left me" goes missing and the flag
            // stays set. `geom`'s tombstone at the old `drag_source` carries the whole story.
            ColorPickerMsg::DragPressed(source) => {
                if self.color_picker.drag.is_some() {
                    return Task::none();
                }
                // A press anywhere that can start a drag also moves the interaction on,
                // so an open rename commits first (DRAGON-687): its editor cannot see the
                // press, and leaving it up under a drag would let a Backspace meant for
                // the name reach the drag's own swallow rule.
                self.commit_rename_if_open();
                let cp = &self.color_picker;
                if !geom::arms_drag(source, cp.recents.len(), cp.panel_mounted(), &cp.panel_shape())
                {
                    return Task::none();
                }
                // The payload is read HERE, from the swatch that was pressed, and never
                // again: the ghost draws it and the drop files it, so they cannot disagree.
                let payload = match source {
                    geom::DragSource::Harmony(g, i) => cp.panel_swatch((g, i)),
                    other => cp.press_payload(other),
                };
                let Some(payload) = payload else {
                    return Task::none();
                };
                // ARMED, not live: nothing is drawn and nothing is suppressed until the
                // pointer travels, so a plain click on any of the three sources still does
                // exactly what it did before this feature existed.
                self.color_picker.drag = Some(crate::app::color_picker::DragState {
                    source,
                    payload,
                    zone: None,
                    origin: None,
                    at: (0.0, 0.0),
                    live: false,
                    // Disarmed until a sample lands outside the edge bands
                    // (`geom::autoscroll_arms`): a drag born inside one must not scroll.
                    autoscroll_armed: false,
                });
                Task::none()
            }
            // The other half of a history swatch's CLICK (item 41): the press armed the
            // machine, and this release completes it only if nothing travelled in between.
            ColorPickerMsg::RecentReleased(index) => {
                // ONE ordering for every released handler (the lost-release fix): the
                // click decision is read against the machine's state at the release,
                // then an armed-but-not-live machine DISARMS (`geom::release_disarms`;
                // a live drag's release is `DragReleased`'s drop, untouched here), then
                // the click applies. The disarm is the widget-level half of the
                // invariant; `sub_picker_release_watch` is the window-level half.
                let drag = self.color_picker.drag.map(|d| (d.source, d.live));
                if geom::release_disarms(drag) {
                    self.color_picker.drag = None;
                }
                if !geom::completes_click(drag, geom::DragSource::Recent(index)) {
                    return Task::none();
                }
                self.update_color_picker(ColorPickerMsg::LoadRecent(index))
            }
            ColorPickerMsg::DragMoved(x, y) => {
                let window = self.color_picker.window_size();
                let shape = self.color_picker.panel_shape();
                let Some(drag) = self.color_picker.drag.as_mut() else {
                    return Task::none();
                };
                drag.at = (x, y);
                // ARM the edge auto-scroll only once the pointer has been outside the
                // bands (the drag-scroll round): grabbing the topmost visible title used
                // to start the drag INSIDE the top band and scroll the tab under a
                // stationary pointer. Sampled on every move, latching.
                drag.autoscroll_armed =
                    geom::autoscroll_arms(drag.autoscroll_armed, (x, y), window, &shape);
                let origin = *drag.origin.get_or_insert((x, y));
                let source = drag.source;
                let mut went_live = Task::none();
                if !drag.live {
                    if !geom::drag_is_live(origin, (x, y)) {
                        return Task::none();
                    }
                    drag.live = true;
                    // GOING LIVE. Two things happen once, here: the panel switches to Saved
                    // Palettes (item 39, transiently, remembering what it was showing), and
                    // the ghost's raster is built for a translucent colour.
                    log::debug!("color picker: a drag went live from {source:?}");
                    went_live = self.start_drag_tab_switch();
                    self.refresh_drag_raster();
                }
                // The highlighted zone, recomputed per sample because it is a function of
                // where the pointer is. Its dashed outline is rastered only when the zone
                // CHANGES: it is one image the size of a region, not something to rebuild at
                // pointer rate. (The INSERTION lines are not rasters at all: they are
                // analytic quads the view derives from the live position, DRAGON-687.)
                self.refresh_drag_zone(source, (x, y));
                went_live
            }
            ColorPickerMsg::DragCancelled => self.end_drag(),
            ColorPickerMsg::DragReleased => {
                let Some(drag) = self.color_picker.drag else {
                    return Task::none();
                };
                // The drop is resolved against the state the user RELEASED over, so the
                // shape is read (and the action decided) BEFORE the drag ends and the
                // transient tab switch reverts (DRAGON-687): reverting first would tell
                // the hit test the palettes tab was never showing, and every panel drop
                // would read as "over nothing".
                let window = self.color_picker.window_size();
                let shape = self.color_picker.panel_shape();
                if !drag.live {
                    // A press-release that never travelled. It is a CLICK, and the widget
                    // under it has already answered for it.
                    return self.end_drag();
                }
                let (c, alpha) = drag.payload;
                let off = geom::off_window(drag.at, window);
                let zone = geom::drop_zone(drag.at, window, &shape);
                let action = geom::drop_action(drag.source, zone, drag.at, &shape, off);
                // WHICH ending depends on the outcome (item five of the drag-jump
                // round): a drop INTO a saved palette COMMITS the transient tab switch,
                // everything else reverts it. `geom::drop_commits_palette_tab` is the
                // whole table.
                let ended = if action.is_some_and(geom::drop_commits_palette_tab) {
                    self.end_drag_committing_palettes()
                } else {
                    self.end_drag()
                };
                let Some(action) = action else {
                    log::debug!("color picker: a drag ended over nothing");
                    return ended;
                };
                log::debug!("color picker: a drag ended as {action:?}");
                // ROW space ends here (item six): the drop machine works on the
                // filtered list's rows, the mutating messages carry REAL indices, and
                // this dispatch is the one seam that maps between them. A row that no
                // longer resolves (the filter changed mid-flight) drops the action
                // rather than guessing.
                let visible = self.color_picker.visible_palettes();
                let real = |row: usize| visible.get(row).copied();
                let acted = match action {
                    geom::DropAction::SetActive => self.update_color_picker(
                        ColorPickerMsg::SetActiveColor(c, alpha, geom::ColorSource::Harmony),
                    ),
                    // A palette colour taken back is a LOAD, not a derivation, so its
                    // source is a recents click's: alpha kept, recents untouched
                    // (DRAGON-687).
                    geom::DropAction::SetActiveNoFile => self.update_color_picker(
                        ColorPickerMsg::SetActiveColor(c, alpha, geom::ColorSource::RecentClick),
                    ),
                    geom::DropAction::AddToRecents => {
                        self.update_color_picker(ColorPickerMsg::AddColorToRecents(c, alpha))
                    }
                    geom::DropAction::LoadRecent(i) => {
                        self.update_color_picker(ColorPickerMsg::LoadRecent(i))
                    }
                    geom::DropAction::RemoveRecent(i) => {
                        self.update_color_picker(ColorPickerMsg::RemoveRecent(i))
                    }
                    geom::DropAction::AppendToPalette(g) => match real(g) {
                        Some(g) => self.update_color_picker(ColorPickerMsg::AddColorToPalette(
                            g, c, alpha,
                        )),
                        None => Task::none(),
                    },
                    // A cross-group drag COPIES (the owner's reversal; the menu's Move
                    // is the explicit vacating form): the one guarded copy, so an
                    // already-present target is a graceful no-op with the source intact.
                    geom::DropAction::CopyToPalette { from, to } => {
                        match (real(from.0), real(to)) {
                            (Some(fg), Some(to)) => self.update_color_picker(
                                ColorPickerMsg::CopyPaletteColor { from: (fg, from.1), to },
                            ),
                            _ => Task::none(),
                        }
                    }
                    geom::DropAction::ReorderPaletteColor { group, from, to } => {
                        match real(group) {
                            Some(group) => self.update_color_picker(
                                ColorPickerMsg::ReorderPaletteColor { group, from, to },
                            ),
                            None => Task::none(),
                        }
                    }
                    geom::DropAction::RemovePaletteColor { group, index } => match real(group) {
                        Some(group) => self.update_color_picker(
                            ColorPickerMsg::RemovePaletteColor(group, index),
                        ),
                        None => Task::none(),
                    },
                    // A group reorder's `to` is an insertion SLOT in the visible order:
                    // it anchors before the real group at that slot, or at the very end
                    // (`geom::visible_slot_to_real`), so reordering under a filter moves
                    // the dragged group where the user SEES it landing.
                    geom::DropAction::ReorderGroup { from, to } => match real(from) {
                        Some(from) => {
                            let to = geom::visible_slot_to_real(
                                &visible,
                                to,
                                self.color_picker.palettes.len(),
                            );
                            self.update_color_picker(ColorPickerMsg::ReorderGroup { from, to })
                        }
                        None => Task::none(),
                    },
                    geom::DropAction::DeleteGroupRequest(g) => match real(g) {
                        Some(g) => {
                            self.update_color_picker(ColorPickerMsg::RequestDeleteGroup(g))
                        }
                        None => Task::none(),
                    },
                };
                Task::batch([ended, acted])
            }
            ColorPickerMsg::RecentHovered(i) => {
                self.color_picker.hovered_recent = Some(i);
                Task::none()
            }
            ColorPickerMsg::RecentUnhovered(i) => {
                // Only if it is still the one we recorded: the pointer crossing from one
                // swatch to its neighbour can deliver the ENTER before the EXIT, and a
                // blind clear would then forget a swatch the pointer is really over.
                if self.color_picker.hovered_recent == Some(i) {
                    self.color_picker.hovered_recent = None;
                }
                Task::none()
            }
            // DRAGON-680 item 24: forget one history entry. The shared tail of the context
            // menu's "Remove from recents" and the Backspace / Delete key.
            ColorPickerMsg::RemoveRecent(i) => {
                let before = self.color_picker.recents.len();
                self.color_picker.recents =
                    geom::remove_recent(&self.color_picker.recents, i);
                // The menu closes whatever happened: the click landed, so its job is done.
                self.color_picker.recents_menu = None;
                if self.color_picker.recents.len() == before {
                    // Out of range, which a pick delivered from another process between
                    // the menu opening and the click can produce. Nothing was written, so
                    // nothing is saved.
                    return Task::none();
                }
                // The list SHIFTS under the pointer, so the hover is stale by definition;
                // the next real motion re-reports it. Nothing here re-derives the
                // selection: `selected_recent` matches on colour and alpha, so removing
                // the selected entry simply leaves the grid with none, which its arrow
                // keys already read as "enter at the first swatch".
                self.color_picker.hovered_recent = None;
                self.refresh_recent_rasters();
                self.save_state();
                log::debug!("color picker: removed a color from the history");
                Task::none()
            }
            ColorPickerMsg::AddToHistory => {
                self.add_shown_color_to_history();
                Task::none()
            }
            ColorPickerMsg::ClearSwatchCopied => {
                self.color_picker.swatch_copied = None;
                Task::none()
            }
            ColorPickerMsg::ClearCopied => {
                self.color_picker.copied = None;
                Task::none()
            }
            ColorPickerMsg::PickCopyDeadline => {
                // The latch is cleared by whichever of the focus and this deadline arrives
                // first, so reaching here with it still set means the window never took the
                // keyboard inside the budget. Say so: a copy nobody can find is worse when
                // nothing in the log admits it did not happen.
                if !self.color_picker.copy_waiting {
                    return Task::none();
                }
                self.color_picker.copy_waiting = false;
                log::error!(
                    "color picker: the picked color could not be copied — this session serves \
                     the clipboard from a focused window and the result window never took the \
                     keyboard within {}s. The window's Copy button still works.",
                    crate::share::WINDOW_COPY_FOCUS_BUDGET.as_secs()
                );
                Task::none()
            }
            // ── Saved palettes (DRAGON-687) ─────────────────────────────────
            ColorPickerMsg::CreatePalette => {
                self.commit_rename_if_open();
                // Creating CLEARS the filter (item six): the new group's placeholder
                // name will not match an arbitrary query, and a create whose result is
                // invisible, with its rename editor filtered out mid-edit, is a trap.
                self.color_picker.palette_search.clear();
                self.color_picker.palette_search_active = false;
                // PREPENDED at the top (the owner's correction; it appended until the
                // drag-scroll round): `geom::palettes_with_new` is the decision, naming
                // included, and the persisted order is exactly this order.
                self.color_picker.palettes = geom::palettes_with_new(&self.color_picker.palettes);
                self.save_palettes();
                log::debug!("color picker: created a palette");
                // The fresh group is FIRST, so the tab scrolls to the TOP, through the
                // same honest write every scroll takes: mirror and widget together.
                self.color_picker.panel_scroll_y = 0.0;
                let scroll = cosmic::iced::widget::scrollable::scroll_to(
                    self.color_picker.panel_scroll_id.clone(),
                    cosmic::iced::widget::scrollable::AbsoluteOffset {
                        x: Some(0.0),
                        y: Some(0.0),
                    },
                );
                // Straight into the rename, selected: the create button's whole point is
                // a NAMED group, so the placeholder is offered pre-selected for typing
                // over. (A pipette pick out while this prepends resolves by NAME through
                // its snapshot, `resolve_palette_target`'s own rule, so the shift costs
                // it nothing.)
                Task::batch([scroll, self.begin_rename(0)])
            }
            ColorPickerMsg::RenameStarted(group) => {
                self.commit_rename_if_open();
                if group >= self.color_picker.palettes.len() {
                    return Task::none();
                }
                self.begin_rename(group)
            }
            ColorPickerMsg::RenameEdited(text) => {
                if let Some((_, draft)) = self.color_picker.rename.as_mut() {
                    *draft = text;
                }
                Task::none()
            }
            ColorPickerMsg::RenameCommitted => {
                self.commit_rename_if_open();
                Task::none()
            }
            ColorPickerMsg::RenameCancelled => {
                self.color_picker.rename = None;
                Task::none()
            }
            // The click half of a group name's press-names-itself machine: same rule as
            // `RecentReleased`, and what it completes is the RENAME.
            ColorPickerMsg::GroupNameReleased(group) => {
                // `RecentReleased`'s exact ordering: decide, disarm, apply.
                let drag = self.color_picker.drag.map(|d| (d.source, d.live));
                if geom::release_disarms(drag) {
                    self.color_picker.drag = None;
                }
                if !geom::completes_click(drag, geom::DragSource::PaletteName(group)) {
                    return Task::none();
                }
                // The gesture is ROW space, the rename is REAL (item six): the rename
                // must survive the filter changing under it.
                match self.color_picker.real_palette(group) {
                    Some(real) => self.update_color_picker(ColorPickerMsg::RenameStarted(real)),
                    None => Task::none(),
                }
            }
            ColorPickerMsg::GroupMenu(at) => {
                self.color_picker.group_menu = at;
                self.color_picker.palette_menu = None;
                self.color_picker.panel_menu = None;
                self.color_picker.recents_menu = None;
                // The root-page-on-open invariant, `PanelMenu`'s own reason.
                self.color_picker.menu_page =
                    geom::menu_page_on_open(self.color_picker.menu_page);
                Task::none()
            }
            ColorPickerMsg::PaletteSwatchMenu(at) => {
                self.color_picker.palette_menu = at;
                self.color_picker.group_menu = None;
                self.color_picker.panel_menu = None;
                self.color_picker.recents_menu = None;
                // The root-page-on-open invariant, `PanelMenu`'s own reason.
                self.color_picker.menu_page =
                    geom::menu_page_on_open(self.color_picker.menu_page);
                Task::none()
            }
            ColorPickerMsg::MenuPageChanged(page) => {
                self.color_picker.menu_page = page;
                Task::none()
            }
            // The per-palette PLUS button: the window's CURRENT colour, appended
            // (DRAGON-687, the owner's 'Add current color'). One path with the drop's own
            // append, so the duplicate rule cannot fork.
            ColorPickerMsg::AddActiveToPalette(group) => {
                let entry = geom::Recent::new(self.color_picker.color, self.color_picker.alpha);
                self.apply_palette_change(geom::palette_append(
                    &self.color_picker.palettes,
                    group,
                    entry,
                ));
                Task::none()
            }
            // The per-palette PIPETTE (DRAGON-687 follow-up): a fresh pick child whose
            // colour lands DIRECTLY in this group. A fresh LAUNCH for `PickAgain`'s exact
            // multi-monitor reason; what differs is only the destination riding the
            // environment: a minted nonce this window can resolve when the colour comes
            // back over the IPC, with the group's identity snapshotted BESIDE the nonce
            // (never on the wire or in the child's environment, where the name, user
            // content, would be visible).
            ColorPickerMsg::PickToPalette(group) => {
                let Some(name) = self.color_picker.palettes.get(group).map(|p| p.name.clone())
                else {
                    return Task::none();
                };
                self.commit_rename_if_open();
                self.color_picker.palette_pick_seq += 1;
                let nonce = self.color_picker.palette_pick_seq;
                self.color_picker.palette_pick_targets.push((nonce, group, name));
                // Prune to the newest few: a cancelled pick sends nothing back, so its
                // entry would otherwise sit for the window's life.
                let len = self.color_picker.palette_pick_targets.len();
                if len > crate::app::color_picker::PALETTE_PICK_CAP {
                    self.color_picker
                        .palette_pick_targets
                        .drain(..len - crate::app::color_picker::PALETTE_PICK_CAP);
                }
                log::debug!("color picker: a palette's pipette started a pick");
                let nonce_str = nonce.to_string();
                crate::recording_ui::spawn_capture_child_args(
                    &["--color-picker"],
                    &[
                        // "No editor", explicitly, `PickAgain`'s own inheritance guard.
                        (crate::app::color_picker::COLOR_TO_PID_ENV, "0"),
                        (crate::app::color_picker::COLOR_TO_PALETTE_ENV, nonce_str.as_str()),
                    ],
                );
                Task::none()
            }
            ColorPickerMsg::AddColorToPalette(group, c, alpha) => {
                self.close_swatch_menus();
                self.apply_palette_change(geom::palette_append(
                    &self.color_picker.palettes,
                    group,
                    geom::Recent::new(c, alpha),
                ));
                Task::none()
            }
            ColorPickerMsg::MovePaletteColor { from, to } => {
                self.close_swatch_menus();
                self.apply_palette_change(geom::palette_move_color(
                    &self.color_picker.palettes,
                    from,
                    to,
                ));
                Task::none()
            }
            ColorPickerMsg::CopyPaletteColor { from, to } => {
                self.close_swatch_menus();
                self.apply_palette_change(geom::palette_copy_color(
                    &self.color_picker.palettes,
                    from,
                    to,
                ));
                Task::none()
            }
            ColorPickerMsg::ReorderPaletteColor { group, from, to } => {
                self.apply_palette_change(geom::palette_reorder_color(
                    &self.color_picker.palettes,
                    group,
                    from,
                    to,
                ));
                Task::none()
            }
            ColorPickerMsg::RemovePaletteColor(group, index) => {
                // Shared by the drag-off and, since the follow-up, the swatch menu's
                // "Remove from palette": the menu must not outlive the swatch it was
                // opened on.
                self.close_swatch_menus();
                self.apply_palette_change(geom::palette_remove_color(
                    &self.color_picker.palettes,
                    group,
                    index,
                ));
                Task::none()
            }
            ColorPickerMsg::ReorderGroup { from, to } => {
                self.apply_palette_change(geom::palette_reorder_group(
                    &self.color_picker.palettes,
                    from,
                    to,
                ));
                Task::none()
            }
            // BOTH delete gestures land here, and neither removes anything: the dialog
            // does (the owner: "these should get a confirmation to delete before
            // deleting").
            ColorPickerMsg::RequestDeleteGroup(group) => {
                if group >= self.color_picker.palettes.len() {
                    return Task::none();
                }
                self.close_swatch_menus();
                self.color_picker.pending_group_delete = Some(group);
                Task::none()
            }
            ColorPickerMsg::ConfirmDeleteGroup(delete) => {
                let pending = self.color_picker.pending_group_delete.take();
                if delete && let Some(group) = pending {
                    // The cursor and any open rename may name the group that just left.
                    self.color_picker.rename = None;
                    self.color_picker.panel_cursor = None;
                    self.apply_palette_change(geom::palette_delete(
                        &self.color_picker.palettes,
                        group,
                    ));
                    log::debug!("color picker: deleted a palette");
                }
                Task::none()
            }
            // The create row's SORT flyout (item six): the six sorts moved here from
            // the group-name menus, so opening it closes any swatch menu and vice versa
            // (the shared sweep's rule: one menu at a time).
            ColorPickerMsg::SortMenu(open) => {
                self.commit_rename_if_open();
                self.close_swatch_menus();
                self.color_picker.sort_menu_open = open;
                Task::none()
            }
            // The SEARCH (item six), the settings header's exact machine: expand and
            // focus with any prior text selected, filter per keystroke, clear-collapses.
            ColorPickerMsg::PaletteSearchActivate => {
                self.commit_rename_if_open();
                self.close_swatch_menus();
                self.color_picker.palette_search_active = true;
                let id = self.color_picker.palette_search_id.clone();
                Task::batch([
                    widget::text_input::focus(id.clone()),
                    widget::text_input::select_all(id),
                ])
            }
            ColorPickerMsg::PaletteSearchInput(q) => {
                self.color_picker.palette_search = q;
                // The rows under every open transient just changed identity: the cursor
                // and the menus were positions in the OLD visible list.
                self.color_picker.panel_cursor = None;
                self.close_swatch_menus();
                // The filtered list can be shorter than the scroll position: clamp the
                // mirror and the widget together (item nine's one-extent rule).
                let max = geom::palettes_max_scroll(
                    self.color_picker.window_size().1,
                    self.color_picker.visible_palettes().len(),
                );
                if self.color_picker.panel_scroll_y > max {
                    self.color_picker.panel_scroll_y = max;
                    return cosmic::iced::widget::scrollable::scroll_to(
                        self.color_picker.panel_scroll_id.clone(),
                        cosmic::iced::widget::scrollable::AbsoluteOffset {
                            x: Some(0.0),
                            y: Some(max),
                        },
                    );
                }
                Task::none()
            }
            ColorPickerMsg::PaletteSearchClear => {
                self.color_picker.palette_search.clear();
                self.color_picker.palette_search_active = false;
                self.color_picker.panel_cursor = None;
                Task::none()
            }
            ColorPickerMsg::PaletteSearchUnfocused => {
                // Collapse only when EMPTY: a non-blank query keeps its field visible,
                // because a hidden filter silently truncating the list is the one state
                // this design must never produce.
                if self.color_picker.palette_search.trim().is_empty() {
                    self.color_picker.palette_search.clear();
                    self.color_picker.palette_search_active = false;
                }
                Task::none()
            }
            // The MAIN swatch's context menu (item seven): open/close, the shared
            // one-menu sweep first.
            ColorPickerMsg::MainSwatchMenu(open) => {
                self.commit_rename_if_open();
                self.close_swatch_menus();
                self.color_picker.main_menu = open;
                Task::none()
            }
            ColorPickerMsg::SortGroups(sort) => {
                self.close_swatch_menus();
                let sorted = geom::sort_palettes(&self.color_picker.palettes, sort);
                if sorted == self.color_picker.palettes {
                    return Task::none();
                }
                // A sort invalidates every index-shaped transient.
                self.color_picker.rename = None;
                self.color_picker.panel_cursor = None;
                self.color_picker.palettes = sorted;
                self.save_palettes();
                log::debug!("color picker: sorted the palettes ({sort:?})");
                Task::none()
            }
            // The WINDOW-level pointer report (the pencil's second stranding): a
            // POSITION, one per move, wherever the pointer is over the window, that the
            // pencil's visibility derives from per frame. No index bookkeeping and no
            // region whose exit can go unreported: everywhere outside the palettes'
            // scroll viewport simply maps to no title.
            ColorPickerMsg::WindowPointerMoved(x, y) => {
                self.color_picker.window_pointer = Some((x, y));
                Task::none()
            }
            ColorPickerMsg::WindowPointerLeft => {
                self.color_picker.window_pointer = None;
                Task::none()
            }
            // The scrollable's own report: the mirror follows the widget's truth, which
            // is what keeps the drop machine's hit tests honest under user scrolling.
            ColorPickerMsg::PanelScrolled(y) => {
                self.color_picker.panel_scroll_y = y;
                // A live drag's highlight has to track the content that just moved
                // under it (the owner's addendum names exactly this).
                if let Some(d) = self.color_picker.drag.filter(|d| d.live) {
                    self.refresh_drag_zone(d.source, d.at);
                }
                Task::none()
            }
            // One auto-scroll step (DRAGON-687, the owner's addendum): velocity from the
            // pure ramp, one tick's travel, clamped at the extents so the edge holds
            // still instead of jittering, and the highlight re-derived under the moved
            // content. The subscription only exists while a live drag's pointer is in a
            // band, so this arm IS the whole drive.
            ColorPickerMsg::DragAutoScroll => {
                // Armed as well as live (the drag-scroll round): a tick that raced the
                // disarm must move nothing.
                let Some(drag) =
                    self.color_picker.drag.filter(|d| d.live && d.autoscroll_armed)
                else {
                    return Task::none();
                };
                let window = self.color_picker.window_size();
                let shape = self.color_picker.panel_shape();
                let v = geom::drag_autoscroll_velocity(drag.at, window, &shape);
                if v == 0.0 {
                    return Task::none();
                }
                let max =
                    geom::palettes_max_scroll(
                        window.1,
                        self.color_picker.visible_palettes().len(),
                    );
                let next = (self.color_picker.panel_scroll_y
                    + v * geom::AUTOSCROLL_TICK.as_secs_f32())
                .clamp(0.0, max);
                if next == self.color_picker.panel_scroll_y {
                    return Task::none();
                }
                self.color_picker.panel_scroll_y = next;
                self.refresh_drag_zone(drag.source, drag.at);
                cosmic::iced::widget::scrollable::scroll_to(
                    self.color_picker.panel_scroll_id.clone(),
                    cosmic::iced::widget::scrollable::AbsoluteOffset {
                        x: Some(0.0),
                        y: Some(next),
                    },
                )
            }
        }
    }

    /// THE surface point this picker reads on `output`, for a pointer at `pointer`
    /// (DRAGON-599). One function, asked by every consumer, so the lens, the hex label and the
    /// pick itself can never disagree about which pixel is being taken.
    ///
    /// Two layers, in order. The POINTER's own point first (DRAGON-587's one-point offset used to
    /// sit here and was removed by DRAGON-597:
    /// the pointer itself where the sprite can be hidden, one point up and left of it where
    /// the arrow is on screen), then the KEYBOARD's displacement on top of that
    /// (`geom::nudged_sample`). With no keys pressed the second layer is the identity, so
    /// every existing path is byte-identical.
    ///
    /// A display whose snapshot has not arrived has no pixel size to step by, and nothing to
    /// sample either, so the base is returned unchanged rather than guessing a step.
    fn picker_sample(&self, o: &OutputState, pointer: (f32, f32)) -> (f32, f32) {
        let viewport = o.units().size_to_point(o.logical_size);
        // DRAGON-597 removed the one-point offset that used to sit here: the pointer can be
        // hidden on every surface now, so there is no arrow tip to escape and the pointer's own
        // point IS the sample. The keyboard nudge (DRAGON-599) still applies on top of it.
        let Some(frozen) = self.frozen.get(&o.name) else {
            return pointer;
        };
        let image = (frozen.img.width(), frozen.img.height());
        geom::nudged_sample(pointer, self.color_picker.nudge, viewport, image)
    }

    /// Re-read the pixel the picker is pointing at, and re-rasterise the magnifier only if the
    /// SOURCE PIXEL changed.
    ///
    /// The guard matters: a move inside one pixel is the common case at any sensible
    /// pointer speed, and rebuilding the disc would raster ~24k pixels and re-upload them (on
    /// the software-forced image arm, mint a fresh atlas entry too) for a picture that has
    /// not changed.
    /// It is also what makes DRAGON-TBD's 16ms poll free on a tick where nothing moved.
    ///
    /// The point READ is the pointer's own point, plus any keyboard nudge. It used to be
    /// displaced one point up and left as well, to escape an arrow sprite that covered its own
    /// hotspot pixel (DRAGON-587); DRAGON-597 removed that once the pointer could be hidden on
    /// every surface, and the tombstone is in `color_picker::geom`. Everything downstream, the
    /// disc's centre, the hex label and the pick itself, uses that one point, so the lens shows
    /// exactly what a click would take.
    ///
    /// DRAGON-599: this is driven by the KEYBOARD as well as the pointer, which is why it takes
    /// no position. Both routes write `ColorPickerState::pointer` / `nudge` first and then run
    /// this, so there is one sampling path rather than a pointer one and a nearly-identical
    /// keyboard one that could drift.
    ///
    /// DRAGON-TBD: this entry point is the ACCURATE one and always was. It rebuilds the raster
    /// whenever the picture changed, full stop, and it is what the keyboard nudge, the
    /// frozen-snapshot delivery and the first sample of a session all call. Only the pointer's
    /// own per-frame path goes through [`Self::color_picker_resample_with`] with
    /// [`RasterPolicy::Paced`].
    pub(in crate::app) fn color_picker_resample(&mut self) -> Task<cosmic::Action<Msg>> {
        self.color_picker_resample_with(RasterPolicy::Always)
    }

    /// The body of [`Self::color_picker_resample`], with the one question the two callers
    /// disagree about handed in (DRAGON-TBD). See [`RasterPolicy`].
    fn color_picker_resample_with(&mut self, policy: RasterPolicy) -> Task<cosmic::Action<Msg>> {
        let Some((output, pointer)) = self.color_picker.pointer.clone() else {
            return Task::none();
        };
        let output = output.as_str();
        let Some(o) = self.outputs.iter().find(|o| o.name == output) else {
            return Task::none();
        };
        let viewport = o.units().size_to_point(o.logical_size);
        let sample = self.picker_sample(o, pointer);
        let Some((px, py, color)) = self.sample_pixel(o, sample) else {
            // STILL LOADING is not the same as UNREADABLE. The flats grab is deferred off
            // the launch critical path (so the overlay maps immediately), so the first
            // pointer moves of every picker launch arrive before the snapshot does.
            // Saying "this display cannot be read" there would be a false alarm on a
            // perfectly good display, once per launch.
            if self.frozen_pending {
                self.color_picker.hover = None;
                return Task::none();
            }
            // No readable source for this display. Say so once, and keep saying it
            // until the pointer reaches a display we CAN read.
            if !self.color_picker.unavailable {
                log::warn!(
                    "color picker: no pixel source for display '{output}', so no color can \
                     be reported there"
                );
            }
            self.color_picker.unavailable = true;
            self.color_picker.hover = None;
            return Task::none();
        };
        self.color_picker.unavailable = false;
        let zoom = self.color_picker.zoom;
        // How much of the lens is on screen at this position. `None` means none of it is,
        // which is not a state the pointer can reach over its own overlay, but a surface
        // reporting a stale point could: draw nothing rather than an empty disc.
        let Some(disc) = geom::disc_view(sample, viewport) else {
            self.color_picker.hover = None;
            return Task::none();
        };
        // How fast the pointer is travelling, from the PREVIOUS sample point (which is exactly
        // `Hover::sample`) and the instant it was taken. Computed before the early returns
        // below so the clock advances on every look, not only on the ones that raster.
        let now = std::time::Instant::now();
        let speed = match (self.color_picker.hover.as_ref(), self.color_picker.sampled_at) {
            (Some(h), Some(at)) => geom::sample_speed(h.sample, sample, now.duration_since(at)),
            // No previous sample to measure against is not a fast pointer, it is no
            // information, and no information rasters (`geom::sample_speed`'s own rule).
            _ => 0.0,
        };
        self.color_picker.sampled_at = Some(now);
        let same_picture = self.color_picker.hover.as_ref().is_some_and(|h| {
            h.output == output && h.pixel == (px, py) && h.zoom == zoom && h.disc == disc
        });
        if same_picture {
            // Only the position moved. Keep the rasterised disc and slide it.
            if let Some(h) = self.color_picker.hover.as_mut() {
                h.sample = sample;
            }
            return Task::none();
        }
        // DRAGON-TBD: the picture DID change, but on the pointer's own per-frame path a fast
        // sweep changes it every single frame, and none of those pictures is one the user is
        // reading: they are travelling. `geom::raster_min_interval` carries the measurement
        // (57.5µs a raster, so this is about not doing invisible work rather than about a
        // frame budget) and the ramp.
        //
        // What is skipped is ONLY the raster. The lens still follows the pointer and the hex
        // chip still reports the pixel under it, because `sample` and `color` are not what the
        // raster describes. `pixel`, `zoom` and `disc` deliberately are NOT updated: they are
        // the IDENTITY of the pixels currently in the buffer, so writing them here would make
        // the `same_picture` guard above agree with a lens showing something else, and a
        // pointer that then stopped would keep that stale picture for good.
        //
        // DRAGON-650: "the lens still follows the pointer" is now actually true, and it was
        // not when this pacing landed. The view used to PLACE the disc from `h.disc.origin`,
        // the raster's own identity, so on every skipped frame the lens stood still and then
        // jumped up to `RASTER_MAX_INTERVAL` of travel in one step — the reported "skips
        // around erratically", obvious on a 60Hz panel where ordinary motion clears
        // `DELIBERATE_SPEED`. The view now derives the drawn position from `h.sample` on
        // every frame (`geom::drawn_disc_origin`), so this branch's `h.sample` write IS the
        // lens's movement, and the raster identity stays honestly stale.
        //
        // The skip also requires the hover to be on THIS output (DRAGON-650): `h.sample` is a
        // point in `h.output`'s own surface space, so writing a NEW output's coordinates into
        // a hover still tagged with the old one would place the lens and the chip at a
        // meaningless position on the wrong display for up to a pacing interval. A monitor
        // crossing happens once per crossing, not once per frame, so rastering it immediately
        // costs nothing the pacing exists to save.
        //
        // Leaving `resample_due` set is what guarantees the settle: it buys one more look on
        // the next frame, and the frame after a hand stops measures zero speed, so it rasters.
        let hover_on_this_output =
            self.color_picker.hover.as_ref().is_some_and(|h| h.output == output);
        if hover_on_this_output
            && skips_raster(
                policy,
                self.color_picker.rastered_at.map(|t| now.duration_since(t)),
                speed,
            )
        {
            if let Some(h) = self.color_picker.hover.as_mut() {
                h.sample = sample;
                h.color = color;
            }
            self.color_picker.resample_due = true;
            return Task::none();
        }
        let Some(frozen) = self.frozen.get(output) else {
            return Task::none();
        };
        let (w, h, rgba) = geom::magnifier_rgba(&frozen.img, (px, py), zoom, self.picker_ring(), disc);
        self.color_picker.hover = Some(Hover {
            output: output.to_string(),
            sample,
            pixel: (px, py),
            color,
            // DRAGON-650: keyed on what THIS process was forced to render with, not on the
            // platform fact — the Windows 10 picker keeps wgpu now, so asking the platform
            // would park its lens on the atlas-churning image arm for no reason. See
            // `process_forced_software_backend`.
            magnifier: build_magnifier_raster(
                w,
                h,
                rgba,
                crate::app::process_forced_software_backend(),
            ),
            zoom,
            disc,
        });
        self.color_picker.rastered_at = Some(now);
        Task::none()
    }

    /// The magnifier's accent RING, as the raster wants it: thickness in points and a
    /// straight RGBA ink (DRAGON-587 baked it into the buffer so it clips with the disc).
    ///
    /// (The two free functions below feed the WINDOW rasters the same way: theme facts
    /// read at build time, because this all runs in `update` with no theme in hand.)
    ///
    /// The thickness is the user's "Selection box thickness", the same setting the region
    /// selector's box reads, which is what DRAGON-582 asked for: one width for both. The
    /// colour is the live accent, read the way every other non-view site in the app reads it
    /// (`cosmic::theme::active()`), because this runs in `update`, not in a view with a theme
    /// in hand.
    fn picker_ring(&self) -> (f32, [u8; 4]) {
        let thickness = self.selection_box_thickness.clamp(1, 8) as f32;
        let accent = crate::app::theme::accent(&cosmic::theme::active());
        (
            thickness,
            [
                (accent.r * 255.0).round().clamp(0.0, 255.0) as u8,
                (accent.g * 255.0).round().clamp(0.0, 255.0) as u8,
                (accent.b * 255.0).round().clamp(0.0, 255.0) as u8,
                255,
            ],
        )
    }

    /// DRAGON-587: zoom the magnifier by `steps` notches, positive = in.
    ///
    /// The ONE handler behind all three routes (trackpad, wheel, numpad `+`/`-`): each of them
    /// only decides how many notches it is worth, and the arithmetic and the clamp are
    /// [`geom::zoom_after_step`]'s. A step that lands on the value we already have does
    /// nothing at all, so holding `+` at the ceiling costs no re-raster.
    ///
    /// The disc is rebuilt HERE rather than in `view`, for the reason `Hover` documents:
    /// rasterising per frame would re-upload the disc on every redraw.
    ///
    /// It stays SYNCHRONOUS, unlike the pointer's resample (DRAGON-TBD). A zoom route is a
    /// wheel notch, a pinch notch or a key press, all of which are already coarse, and the
    /// clamp above makes a step that changes nothing cost nothing; there is no burst here to
    /// coalesce.
    fn color_picker_zoom(&mut self, steps: i32) -> Task<cosmic::Action<Msg>> {
        let zoom = geom::zoom_after_step(self.color_picker.zoom, steps);
        if zoom == self.color_picker.zoom {
            return Task::none();
        }
        self.color_picker.zoom = zoom;
        // Re-rasterise what the pointer is already over, at the new magnification. Nothing
        // else about the hover changes: the same pixel is still being sampled.
        let Some(h) = self.color_picker.hover.as_ref() else {
            return Task::none();
        };
        let Some(frozen) = self.frozen.get(&h.output) else {
            return Task::none();
        };
        let (pixel, disc) = (h.pixel, h.disc);
        let (w, ht, rgba) = geom::magnifier_rgba(&frozen.img, pixel, zoom, self.picker_ring(), disc);
        if let Some(h) = self.color_picker.hover.as_mut() {
            // DRAGON-650: the same forced-backend predicate as the resample's raster, and
            // for the same reason.
            h.magnifier = build_magnifier_raster(
                w,
                ht,
                rgba,
                crate::app::process_forced_software_backend(),
            );
            h.zoom = zoom;
        }
        Task::none()
    }

    /// A left click on a picker overlay: take the colour, copy its hex, tear the
    /// overlays down and open the result window.
    ///
    /// A click on a display with NO readable source is INERT. Refusing is the only
    /// honest answer there: the alternative is delivering a colour we did not read.
    ///
    /// It reads the SAME point the move handler does, the pointer's own, and that is not
    /// optional: the two reading different points would deliver a colour the magnifier never
    /// showed, which is the one thing a colour picker may not do. This used to say "through the
    /// same sampling seam", which existed only while the arrow fallback displaced the
    /// sample (DRAGON-587, removed in DRAGON-597).
    fn color_picker_pick(
        &mut self,
        output: &str,
        point: (f32, f32),
    ) -> Task<cosmic::Action<Msg>> {
        let Some(o) = self.outputs.iter().find(|o| o.name == output) else {
            return Task::none();
        };
        // DRAGON-599: the SAME resolved sample the lens is showing, keyboard nudge included.
        // A click carries the pointer's raw position and nothing else, so reading the pointer
        // directly here would take the pixel under the cursor while the loupe was showing the
        // nudged one — a picker reporting a colour it never displayed, which is the one thing
        // it may never do.
        let sample = self.picker_sample(o, point);
        let Some((_, _, color)) = self.sample_pixel(o, sample) else {
            // Same split as the move handler: a click while the snapshot is still in
            // flight is simply early, not a failure, and must not be reported as one.
            if self.frozen_pending {
                return Task::none();
            }
            self.color_picker.unavailable = true;
            log::warn!("color picker: the click landed on a display with no pixel source");
            return Task::none();
        };
        // A PALETTE-DESTINED pick first (DRAGON-687 follow-up, the owner: "it should
        // directly add to the palette instead of going to the main tool swatch"), BEFORE
        // any of the ordinary filing below: for this destination the child must not touch
        // the active colour, the recents or the clipboard
        // (`PickDestination::files_pick_ordinarily`, the one place that exception lives).
        // The tagged send goes to the one live picker window, which appends and acks; ANY
        // failure (no window, one mid-close, an older build answering `err version`, no
        // transport) falls through to the ordinary flow below, where the destination
        // matches no editor and rides the untagged picker rung, so the colour is never
        // lost, only the filing shortcut.
        let dest = crate::app::color_picker::pick_destination(
            std::env::var(crate::app::color_picker::COLOR_TO_PID_ENV).ok().as_deref(),
            std::env::var(crate::app::color_picker::COLOR_TO_PALETTE_ENV).ok().as_deref(),
        );
        if !dest.files_pick_ordinarily()
            && let crate::app::color_picker::PickDestination::PickerPalette(nonce) = dest
        {
            match crate::preview_ipc::send_color_to_picker(
                [color.r, color.g, color.b],
                Some(nonce),
            ) {
                Ok(pid) => {
                    log::debug!(
                        "color picker: the picker window at pid {pid} took the pick for a \
                         palette"
                    );
                    let mut cmds = self.destroy_surfaces();
                    cmds.push(self.finish_session());
                    return Task::batch(cmds);
                }
                Err(e) => log::info!(
                    "color picker: the palette-destined pick found no taker ({e}); falling \
                     back to the ordinary pick"
                ),
            }
        }
        // A PICK is the one thing that writes the recents. That rule is the same whoever
        // launched the picker, including the editor below (DRAGON-587).
        self.set_picker_color(color, None, geom::ColorSource::Pick);
        self.color_picker.pick_output = Some(output.to_string());
        self.save_state();
        // Hand the colour to whoever it is FOR (DRAGON-587 for an editor, DRAGON-613 for the
        // one picker window). `Some` means a live consumer positively ACKNOWLEDGED it and
        // this process is done; `None` means nobody took it, and we open the result window
        // below exactly as we always did.
        //
        // `deliver_pick` is a LADDER and this is its bottom rung, which is what makes
        // "a pick is never lost" structural rather than a list of handled cases: no
        // consumer, a consumer mid-close, a wedged one, a version-mismatched one and a
        // platform with no transport all arrive here the same way, and all get a window the
        // user can see. Nothing above may consume the pick without a positive ack.
        if let Some(task) = self.deliver_pick(color) {
            return task;
        }
        log::debug!("color picker: picked a color and opened the result window");
        let mut cmds = self.destroy_surfaces();
        // The picked value goes on the clipboard as part of the pick, IN THE REMEMBERED
        // MODE's spelling (DRAGON-630, the owner's ask: what the window copies on
        // opening is the mode you left it in, not always hex). A pick is opaque and
        // `set_picker_color` above has already reset the alpha, so this is the mode's
        // plain spelling. HOW it gets there is the one question every copy in the app
        // asks (`share::copy_step`).
        //
        // DRAGON-587, the owner's report that this copy simply did not happen. It used to be
        // an unconditional `copy_text_task` pushed into THIS batch, which is right on the
        // standalone route and impossible on the window one: the same batch destroys every
        // overlay and only QUEUES the result window's open, so at the moment the write runs
        // this process has no focused surface to carry the selection serial, and the write is
        // dropped. That is the exact shape DRAGON-550 fixed for the preview editor's
        // open-time copy, so it takes the same ladder rather than a second one.
        //
        // The window cannot possibly hold focus yet (it is minted three lines below), so the
        // focus term is a literal `false` rather than a lookup that could only ever say so.
        let value = self.color_picker.value_text();
        match crate::share::copy_step(crate::share::copy_route(), false, false) {
            // A detached worker owns the selection and outlives us: spawn it now, exactly as
            // before. `copy_text_task` returns an empty task on this route.
            crate::share::CopyStep::Detached => {
                cmds.push(crate::share::copy_text_task(&value));
                // The window is about to open with the pick already on the clipboard, so
                // it opens wearing the tick and the word (the owner's ask).
                cmds.push(self.flash_copied(geom::CopySource::Pick));
            }
            // The window route: arm the latch and bound the wait. `flush_deferred_pick_copy`
            // writes it the moment the result window takes the keyboard, which is also the
            // input event whose serial the selection needs.
            crate::share::CopyStep::WaitForFocus => {
                self.color_picker.copy_waiting = true;
                cmds.push(Task::perform(
                    async {
                        tokio::time::sleep(crate::share::WINDOW_COPY_FOCUS_BUDGET).await;
                    },
                    |()| {
                        cosmic::Action::App(Msg::ColorPicker(ColorPickerMsg::PickCopyDeadline))
                    },
                ));
            }
            // Neither is reachable from here (the focus term is false and the budget is
            // fresh), and both are answered by the deferral above rather than by a write this
            // surface cannot serve. Written out so a future change to the ladder cannot leave
            // this arm silently doing nothing.
            crate::share::CopyStep::ThroughWindow | crate::share::CopyStep::ReportFailed => {
                self.color_picker.copy_waiting = true;
            }
        }
        // EVERY raster, before the window exists (DRAGON-682 items 13 and 25). One call, so
        // a raster added later cannot be left out of this path the way two of them already
        // were; `ensure_all_rasters` asserts its own completeness.
        self.ensure_all_rasters();
        let (id, open) =
            crate::app::color_picker::open_color_picker_window(self.color_picker.expanded);
        self.color_picker.window = Some(id);
        // DRAGON-613: BIND THE LISTENER BEFORE THE MARKER BELOW, the same ordering
        // `preview_surface_for` uses and for the same reason: the marker IS the discovery
        // record, so binding second would open a window in which a later pick finds a window
        // that is not listening and needlessly opens a second one. (That direction only
        // costs the optimisation, never the colour.) A bind failure is non-fatal: we simply
        // do not receive, and every later pick behaves exactly as it did before this existed.
        #[cfg(any(unix, windows))]
        if self.handoff_host.is_none() {
            let addr = crate::instance::color_picker_host_address(std::process::id());
            match crate::preview_ipc::start_host_at(addr) {
                Ok(host) => {
                    log::info!("color picker: listening for picks on {}", host.address());
                    self.handoff_host = Some(host);
                }
                Err(e) => log::warn!("color picker: not listening for picks ({e})"),
            }
        }
        // DRAGON-582: advertise the open window so a LATER capture's sibling sweep spares
        // this process. Picking a colour and then screenshotting something to use it in is
        // the obvious next step, and without this the screenshot would kill the window
        // holding the value. Cleared at `finish_session`, like the preview marker.
        // DRAGON-613: it is ALSO the discovery record a later pick finds, so that pick can
        // update this window instead of opening a second one.
        crate::instance::set_color_picker_marker(true);
        cmds.push(open);
        Task::batch(cmds)
    }

    /// Hand this pick to whoever it is FOR, and end the session if they took it.
    ///
    /// `Some(task)` ONLY when a live consumer has positively ACKNOWLEDGED the colour, in
    /// which case this process is done: the colour is where the user wanted it. `None` for
    /// every other outcome, so the caller opens the result window exactly as it always did.
    ///
    /// **ONE LADDER, tried in order, each rung only if it applies:** the editor that asked
    /// (if [`crate::app::color_picker::PickDestination`] names one), then THE colour picker
    /// window (if one is open anywhere), then this process's own new window. Written as a
    /// ladder rather than as two independent destinations because a rung that fails must
    /// fall to the NEXT one, not to the bottom.
    ///
    /// That distinction is load-bearing for the single-window rule. An editor pick whose
    /// editor has since closed used to go straight to opening a window, which would open a
    /// SECOND one whenever a picker window was already up. Degrading it into the picker rung
    /// keeps "at most one window" true in every case, including the failure cases, and the
    /// colour is still never lost because the bottom rung is unconditional.
    fn deliver_pick(&mut self, color: Srgb) -> Option<Task<cosmic::Action<Msg>>> {
        use crate::app::color_picker::PickDestination;
        let rgb = [color.r, color.g, color.b];
        let dest = crate::app::color_picker::pick_destination(
            std::env::var(crate::app::color_picker::COLOR_TO_PID_ENV).ok().as_deref(),
            std::env::var(crate::app::color_picker::COLOR_TO_PALETTE_ENV).ok().as_deref(),
        );
        // Rung 1, DRAGON-587: a pick launched from a preview editor's PIPETTE belongs to that
        // editor. Deliver it and end with no result window: the user is mid-annotation and
        // asked for a drawing colour, not for a window to read and close, and the colour is
        // immediately visible in the editor's own swatch.
        if let PickDestination::Editor(pid) = dest {
            match crate::preview_ipc::send_color_to_pid(pid, rgb) {
                Ok(()) => {
                    log::debug!("color picker: the editor at pid {pid} took the picked color");
                    // The value COPY is deliberately skipped here when the session copies
                    // through a window, and the reason is stated rather than hidden: there is
                    // no window here to serve a selection from, and the colour has already
                    // gone where it was asked to go. On a data-control session the detached
                    // worker still runs, so the value (in the remembered mode's spelling,
                    // DRAGON-630) is on the clipboard as a bonus.
                    if crate::share::copy_route() == crate::share::CopyRoute::Standalone {
                        crate::share::copy_text(&self.color_picker.mode.format(color));
                    }
                    let mut cmds = self.destroy_surfaces();
                    cmds.push(self.finish_session());
                    return Some(Task::batch(cmds));
                }
                Err(e) => log::info!(
                    "color picker: the editor at pid {pid} did not take the color ({e}); \
                     trying the picker window"
                ),
            }
        }
        // Rung 2, DRAGON-613: if a picker window is open ANYWHERE it takes the colour, which
        // becomes the colour it shows and its newest recent, and no second window appears.
        // A palette-destined pick whose TAGGED send already failed (`color_picker_pick`'s
        // own rung) resolves to its variant here, matches no editor, and rides this rung
        // UNTAGGED: the ordinary delivery is exactly what its degrade story promises.
        match crate::preview_ipc::send_color_to_picker(rgb, None) {
            Ok(pid) => {
                log::debug!(
                    "color picker: the open picker window at pid {pid} took the picked color"
                );
                // The clipboard is the RECEIVER's job on this rung, and deliberately so. It
                // has a real window that can hold the keyboard, which is the only way a
                // selection goes out on the `ThisWindow` route; writing it here would put
                // nothing on the clipboard for exactly the sessions DRAGON-587 fixed. See
                // `App::apply_handoff_pick`.
                let mut cmds = self.destroy_surfaces();
                cmds.push(self.finish_session());
                Some(Task::batch(cmds))
            }
            // Rung 3 is the caller's: no consumer took it, so this process opens its own
            // window, exactly as it did before any of this existed. This is the ONE place
            // every failure lands (no window, one mid-close, a wedged one, a
            // version-mismatched one, a platform with no transport), which is what makes
            // "a pick is never lost" structural rather than a list of handled cases.
            Err(e) => {
                log::info!(
                    "color picker: no open picker window took the color ({e}); opening one here"
                );
                None
            }
        }
    }

    /// DRAGON-613: a pick made by ANOTHER process has arrived for this window. Apply it
    /// exactly as a local pick would.
    ///
    /// `None` when this process has no picker window, which the drain turns into a refusal so
    /// the sender opens its own window rather than losing the colour. `Some(task)` once the
    /// colour is actually in our state, which is the only point at which the drain may ack.
    ///
    /// It routes through the SAME `set_picker_color(.., ColorSource::Pick)` a local pick
    /// uses, which is the one place `geom::writes_recents` is applied. So a handed-over pick
    /// IS a pick: it becomes the shown colour and is promoted to the front of the recents,
    /// under the one rule, with no second copy of that rule here.
    ///
    /// It also takes the CLIPBOARD job the sender could not do. Before this, a second pick
    /// opened a second window and that window's focus is what carried the hex out on a
    /// [`crate::share::CopyRoute::ThisWindow`] session (Flatpak, GNOME, sandboxed niri and
    /// Hyprland). Handing off and exiting with no window would have quietly stopped that, so
    /// the copy moves to the process that still HAS a window, through the app's one copy
    /// ladder (`share::copy_step`) rather than a second reading of it.
    // Its only caller is the drain (`drain_preview_handoffs`), which is a `Task::none()`
    // stub only where no transport exists (neither unix sockets nor Windows named pipes).
    // So this is honestly dead only there, and it stays portable: the body is plain app
    // state plus the shared copy ladder. DRAGON-651 proved the old claim here — that a
    // Windows named-pipe transport would call it unchanged — by doing exactly that.
    #[cfg_attr(not(any(unix, windows)), allow(dead_code))]
    pub(in crate::app) fn apply_handoff_pick(
        &mut self,
        rgb: [u8; 3],
    ) -> Option<Task<cosmic::Action<Msg>>> {
        let id = self.color_picker.window?;
        let color = Srgb::new(rgb[0], rgb[1], rgb[2]);
        self.set_picker_color(color, None, geom::ColorSource::Pick);
        self.save_state();
        // The colour itself is user content and is never logged.
        log::debug!("color picker: took a color picked by another process, and promoted it");
        // Raise the window: the user just picked a colour and this window is the only thing
        // reporting it, so leaving it behind whatever they were looking at would read as the
        // pick having done nothing.
        let mut cmds = vec![window::gain_focus(id)];
        // The remembered mode's spelling (DRAGON-630), like every other pick copy; the
        // pick just reset the alpha, so this is the mode's plain form.
        let value = self.color_picker.value_text();
        // Unlike the pick handler, the window here already EXISTS, so the focus term is a
        // real lookup rather than a literal `false`, and an already-focused window writes
        // immediately instead of waiting for a focus event that would never come.
        let focused = self.focused_window == Some(id);
        match crate::share::copy_step(crate::share::copy_route(), focused, false) {
            crate::share::CopyStep::Detached | crate::share::CopyStep::ThroughWindow => {
                cmds.push(crate::share::copy_text_task(&value));
                cmds.push(self.flash_copied(geom::CopySource::Pick));
            }
            // Our window exists but is not focused. The `gain_focus` above is the request
            // that fixes that, and `flush_deferred_pick_copy` writes the hex when it lands.
            crate::share::CopyStep::WaitForFocus => {
                self.color_picker.copy_waiting = true;
                cmds.push(Task::perform(
                    async {
                        tokio::time::sleep(crate::share::WINDOW_COPY_FOCUS_BUDGET).await;
                    },
                    |()| cosmic::Action::App(Msg::ColorPicker(ColorPickerMsg::PickCopyDeadline)),
                ));
            }
            // Unreachable with a fresh budget, and written out rather than folded into the
            // arm above so a future change to the ladder cannot leave it silently doing
            // nothing. The deadline handler reports the miss.
            crate::share::CopyStep::ReportFailed => self.color_picker.copy_waiting = true,
        }
        Some(Task::batch(cmds))
    }

    /// DRAGON-687 follow-up: a PALETTE-DESTINED pick has arrived (the tagged `color`
    /// verb): append it to the group the nonce names and touch NOTHING else. No active
    /// colour, no recents, no clipboard, no copy flash — the owner's spec, stated at
    /// [`crate::app::color_picker::PickDestination::files_pick_ordinarily`].
    ///
    /// `None` when this process has no picker window (the drain refuses, the sender falls
    /// back). A nonce this window did not mint, or a snapshot whose group is GONE
    /// (deleted, or renamed while the pick was out: `geom::resolve_palette_target`), does
    /// not lose the pick: it degrades to [`Self::apply_handoff_pick`], the ordinary
    /// delivery, so the colour lands as the active colour and a recent instead of
    /// vanishing. The compromise is deliberate and logged: a mid-pick rename costs the
    /// filing shortcut, never the colour.
    ///
    /// A duplicate append is `palette_append`'s ordinary no-op; the pick still acks
    /// (the colour IS in the palette, which is what the sender asked). The window is
    /// raised either way, and the panel turned to Saved Palettes with the group scrolled
    /// into view WHEN THE PANEL IS OPEN, so the user sees the colour land; a collapsed
    /// panel stays collapsed (a pick must not resize the window under the user).
    #[cfg_attr(not(any(unix, windows)), allow(dead_code))]
    pub(in crate::app) fn apply_handoff_palette_pick(
        &mut self,
        rgb: [u8; 3],
        nonce: u64,
    ) -> Option<Task<cosmic::Action<Msg>>> {
        let id = self.color_picker.window?;
        // Consume the snapshot whatever happens next: the nonce is one pick's.
        let rec = self
            .color_picker
            .palette_pick_targets
            .iter()
            .position(|(n, _, _)| *n == nonce)
            .map(|i| self.color_picker.palette_pick_targets.remove(i));
        let target = rec.and_then(|(_, index, name)| {
            geom::resolve_palette_target(&self.color_picker.palettes, index, &name)
        });
        let Some(group) = target else {
            log::info!(
                "color picker: a palette-destined pick arrived for a group that is gone; \
                 applying it as an ordinary pick"
            );
            return self.apply_handoff_pick(rgb);
        };
        let entry = geom::Recent::opaque(Srgb::new(rgb[0], rgb[1], rgb[2]));
        let changed = self.apply_palette_change(geom::palette_append(
            &self.color_picker.palettes,
            group,
            entry,
        ));
        log::debug!(
            "color picker: a picked color was filed into a palette{}",
            if changed { "" } else { " it already held" }
        );
        let mut cmds = vec![window::gain_focus(id)];
        if self.color_picker.panel_mounted() {
            cmds.push(self.set_panel_tab(geom::PanelTab::Palettes));
            // The group scrolled into view, at the palette groups' own height. `group`
            // is REAL; the scroll needs its visible ROW (item six), and a group the
            // live filter hides is not scrolled to at all: the append above landed and
            // persisted either way, and yanking the user's filter away to prove it
            // would trade a quiet success for a surprise.
            let visible = self.color_picker.visible_palettes();
            if let Some(row) = visible.iter().position(|g| *g == group) {
                let offset = geom::palette_group_offset(row).min(geom::palettes_max_scroll(
                    self.color_picker.window_size().1,
                    visible.len(),
                ));
                self.color_picker.panel_scroll_y = offset;
                cmds.push(cosmic::iced::widget::scrollable::scroll_to(
                    self.color_picker.panel_scroll_id.clone(),
                    cosmic::iced::widget::scrollable::AbsoluteOffset {
                        x: Some(0.0),
                        y: Some(offset),
                    },
                ));
            }
        }
        Some(Task::batch(cmds))
    }

    /// DRAGON-587: the result window took keyboard focus — if the pick's hex copy is waiting
    /// on exactly that, write it now.
    ///
    /// The twin of `App::flush_deferred_auto_copy`, and for the same reason: on the
    /// [`crate::share::CopyRoute::ThisWindow`] route the selection goes out over
    /// `wl_data_device`, which needs a serial from an input event delivered to this client,
    /// and a keyboard focus IS that event.
    ///
    /// A no-op for every other window, every other route, and a pick whose deadline already
    /// fired: `copy_waiting` is the one-shot latch. The cost of this route is the documented
    /// one, so it is stated rather than hidden: the selection lives exactly as long as this
    /// process, so closing the picker window takes the copy with it unless a clipboard manager
    /// has claimed it in the meantime.
    pub(in crate::app) fn flush_deferred_pick_copy(
        &mut self,
        id: window::Id,
    ) -> Task<cosmic::Action<Msg>> {
        if !self.color_picker.copy_waiting || self.color_picker.window != Some(id) {
            return Task::none();
        }
        self.color_picker.copy_waiting = false;
        log::debug!("color picker: the result window took focus, writing the pick's copy");
        // Re-derived from the live state rather than stored twice (and in the remembered
        // mode's spelling, DRAGON-630). Nothing can have changed it between the pick and
        // this focus: the window is only now becoming interactive.
        let write = crate::share::copy_text_task(&self.color_picker.value_text());
        // THIS is the moment the pick reaches the clipboard on the window route, so this
        // is where the flash belongs: the window has just appeared and taken the
        // keyboard, which is exactly the "opened the panel" instant the owner means.
        Task::batch([write, self.flash_copied(geom::CopySource::Pick)])
    }

    /// Set the colour the window shows, and write the recents only when the SOURCE says
    /// so ([`geom::writes_recents`]). The one place that rule is applied.
    ///
    /// The tracked HSV follows the colour here (`color::hsv_tracking`), which is what
    /// keeps the square and the hue strip honest after a pick, a recent-click, a typed
    /// edit or a handed-over pick. The square and hue handlers deliberately do NOT come
    /// through here: they own the HSV and call [`Self::apply_picker_color`] directly,
    /// so byte quantisation cannot re-track the exact position the hand just chose.
    // `pub(in crate::app)` since DRAGON-680: the palette viewer's window open
    // (`color_picker::viewer`) loads its colour through this same one place, so the HSV
    // tracking and the gradient rasters cannot drift between the two launch shapes.
    /// `alpha` is the INCOMING alpha for a source that carries one (a history entry, a
    /// swatch, a set-active), or `None` for a source that does not: a pick resets to
    /// opaque (`geom::keeps_alpha`), an edit keeps the field its own handlers manage.
    /// It is a parameter rather than a caller's pre-write since DRAGON-687 item ten,
    /// because the bump below must see the OUTGOING pair before anything moves.
    pub(in crate::app) fn set_picker_color(
        &mut self,
        color: Srgb,
        alpha: Option<u8>,
        source: geom::ColorSource,
    ) {
        self.color_picker.hsv = crate::color::hsv_tracking(self.color_picker.hsv, color);
        self.refresh_sv_raster();
        self.apply_picker_color(color, alpha, source);
    }

    /// The body of [`Self::set_picker_color`] minus the HSV tracking (DRAGON-630).
    fn apply_picker_color(&mut self, color: Srgb, alpha: Option<u8>, source: geom::ColorSource) {
        // The incoming alpha: the caller's, or the source's own rule. A PICK is a screen
        // pixel and has no transparency, so it resets to opaque; an EDIT's handlers own
        // the field and pass `None` to leave it be (`geom::keeps_alpha`).
        let incoming_alpha = match alpha {
            Some(a) => a,
            None if !geom::keeps_alpha(source) => u8::MAX,
            None => self.color_picker.alpha,
        };
        // DRAGON-687 item ten, the owner's closing rule: a DISCRETE replacement files
        // the OUTGOING colour first if the history does not hold it, so no colour the
        // user held goes missing. `geom::files_outgoing` is the whole table (the Edit
        // exemption, the absent-check, the self-replacement no-op, and the window-open
        // gate that keeps the launch loads from filing the state's default). Inside THIS
        // function so no call site can forget it; item five's click-specific bump
        // retired into it.
        let outgoing = geom::Recent::new(self.color_picker.color, self.color_picker.alpha);
        if geom::files_outgoing(
            self.color_picker.window.is_some(),
            source,
            outgoing,
            geom::Recent::new(color, incoming_alpha),
            &self.color_picker.recents,
        ) {
            self.add_to_history(outgoing);
        }
        self.color_picker.alpha = incoming_alpha;
        self.color_picker.color = color;
        // …and it makes the harmonies stale too: they are derived from this colour, so a
        // segment's "Copied!" card would end up over a segment holding something else
        // (DRAGON-682 item 30).
        self.color_picker.swatch_copied = None;
        // A new colour makes any in-flight draft stale: it belonged to the old value.
        if source != geom::ColorSource::Edit {
            self.color_picker.draft = None;
        }
        if geom::writes_recents(source) {
            // At the alpha the colour is really being taken at: opaque for a pick (the
            // reset above), and the window's own for a harmony swatch (DRAGON-682 item 22,
            // which is why this reads the field rather than asserting opacity).
            let entry = geom::Recent::new(color, self.color_picker.alpha);
            self.color_picker.recents =
                geom::push_recent(&self.color_picker.recents, entry, geom::RECENTS_CAP);
            self.refresh_recent_rasters();
        }
        self.refresh_color_rasters();
    }

    /// Build EVERY raster the picker window can draw, before it opens (DRAGON-682 item 25).
    ///
    /// **The one call a window-open path makes.** The rasters used to be built by whichever
    /// handler changed their inputs, which meant "is everything ready" was answered by a
    /// scatter of call sites, and each new raster had to remember to join them. Twice it did
    /// not: a window opened from the palette viewer had no history rasters at all, and then
    /// no harmony checkerboard and no empty-slot dots, until the user removed a recent and
    /// the recents-changed path healed everything at once. That is the shape of bug this
    /// replaces: invisible on the developer's machine, obvious on the owner's, and silent.
    ///
    /// The three refreshers each build a related group; what makes this complete rather than
    /// hopeful is the assertion after them, against
    /// `ColorPickerState::missing_rasters`'s inventory. A raster added to the state and to
    /// that inventory but not to a refresher fails here in a debug build, immediately, on
    /// the first window that opens.
    pub(in crate::app) fn ensure_all_rasters(&mut self) {
        self.refresh_sv_raster();
        self.refresh_color_rasters();
        self.refresh_recent_rasters();
        debug_assert!(
            self.color_picker.missing_rasters().is_empty(),
            "the picker window opened without {:?}; a raster was added to the state and to \
             the inventory but nothing builds it",
            self.color_picker.missing_rasters()
        );
    }

    /// Rebuild the SATURATION/VALUE square's raster (and the hue strip's, once): the
    /// square's whole picture is the hue, so only hue changes come here (DRAGON-630).
    /// Rasters are built in update, never in `view`, the magnifier's own rule: a handle
    /// minted per frame churns iced's texture atlas.
    fn refresh_sv_raster(&mut self) {
        use crate::app::color_picker::geom as g;
        let radius = picker_corner_radius();
        let cp = &mut self.color_picker;
        if cp.hue_raster.is_none() {
            let (w, h) = (g::STRIPS_W as u32, g::STRIP_H as u32);
            cp.hue_raster = Some(widget::image::Handle::from_rgba(
                w,
                h,
                crate::color::hue_strip_rgba(w, h, radius),
            ));
        }
        let (w, h) = (g::CONTENT_W as u32, g::SV_H as u32);
        cp.sv_raster = Some(widget::image::Handle::from_rgba(
            w,
            h,
            crate::color::sv_square_rgba(cp.hsv[0], w, h, radius),
        ));
    }

    /// Rebuild the rasters that follow the COLOUR and the ALPHA: the alpha strip (the
    /// colour ramps across it) and the round swatch (DRAGON-630).
    pub(in crate::app) fn refresh_color_rasters(&mut self) {
        use crate::app::color_picker::geom as g;
        let radius = picker_corner_radius();
        let rim = picker_rim();
        let cp = &mut self.color_picker;
        let (w, h) = (g::STRIPS_W as u32, g::STRIP_H as u32);
        cp.alpha_raster = Some(widget::image::Handle::from_rgba(
            w,
            h,
            crate::color::alpha_strip_rgba(cp.color, w, h, radius),
        ));
        // ONE raster pixel per logical point, and the disc's own edge stops
        // `SWATCH_EDGE_MASK` short of the buffer's rim because the VIEW draws the visible
        // silhouette as an analytic quad ring over this (DRAGON-680; `geom::SWATCH_RING_W`
        // carries why a raster cannot draw that edge and why supersampling made it no
        // better). What is left here is the interior, where 1:1 is exactly right: the
        // checkerboard's own squares stay crisp instead of being resampled.
        let d = g::SWATCH_CIRCLE as u32;
        cp.swatch_raster = Some(widget::image::Handle::from_rgba(
            d,
            d,
            crate::color::swatch_circle_rgba(
                cp.color,
                cp.alpha,
                d,
                rim,
                f64::from(g::SWATCH_EDGE_MASK),
            ),
        ));
    }

    /// Rebuild the HISTORY swatches' rasters (DRAGON-680): one per TRANSLUCENT entry,
    /// `None` for an opaque one.
    ///
    /// Only the translucent entries need a raster at all, and that is the point of the
    /// `Option`: an opaque swatch is still the flat button background it always was, so
    /// the common case costs nothing, allocates nothing and cannot look different from
    /// before this ticket. A translucent one needs real pixels, because what it shows is
    /// the split (colour | colour-over-checkerboard) that no combination of container
    /// fills can draw with the swatch's rounded corners.
    ///
    /// Built HERE and never in `view`, the same rule every other raster in this window
    /// follows: a handle minted per frame churns iced's texture atlas, and this one would
    /// mint up to [`geom::RECENTS_CAP`] of them.
    ///
    /// Called from every path that changes the list or the theme's corner radius: a pick,
    /// an explicit add, and the persisted load. It is cheap (a 28x28 buffer per
    /// translucent entry) and idempotent, so an extra call is only ever waste, never a
    /// wrong picture.
    pub(in crate::app) fn refresh_recent_rasters(&mut self) {
        use crate::app::color_picker::geom as g;
        // 1:1 like every other raster in this window (DRAGON-680 tried 3x here too, and
        // `geom::SWATCH_RING_W` records why that reached the screen as decimation rather
        // than as smoothing). A history swatch's rounded corners are short arcs at a small
        // radius, so its own analytic coverage ramp is the same quality the gradient square
        // and the strips have always had, under the button's own 1pt analytic border.
        let radius = picker_corner_radius();
        let rim = picker_rim();
        let d = g::RECENT_SWATCH as u32;
        let cp = &mut self.color_picker;
        // The EMPTY-slot outline (DRAGON-682 item 8): ONE raster for every unfilled
        // position, since they are all the same size and the same ink. Built here so it
        // follows the theme's subdued tone and the rounding setting like everything else
        // in this window, and rebuilt with the swatches because the same two inputs move
        // it.
        cp.empty_slot_raster = Some(widget::image::Handle::from_rgba(
            d,
            d,
            crate::color::dotted_outline_rgba(d, d, radius, rim),
        ));
        // The harmony bars' shared CHECKERBOARD (DRAGON-682 item 19). Same story: one
        // raster for every bar, depending on nothing but the size and the rounding, so it
        // is built here rather than per card in `view`.
        let (bw, bh) = (g::bar_w() as u32, g::PANEL_SWATCH as u32);
        cp.checker_bar_raster = Some(widget::image::Handle::from_rgba(
            bw,
            bh,
            crate::color::checkerboard_rgba(bw, bh, radius),
        ));
        // (`checker_palette_raster` was built here, the same board at the narrower width
        // the bar-row buttons left the palette bars. The UX round moved the buttons into
        // the title row, a palette bar is a harmony bar's exact size again, and the
        // palette bars simply draw `checker_bar_raster` above.)
        // An EMPTY palette group's dotted outline (DRAGON-687 follow-up): the empty
        // history slots' own construction, at the bar's width, height and outer rounding.
        // `dotted_outline_rgba` already takes any rectangle (the dashed zone outline is
        // the same family scaled the other way), so this is reuse, not a new shape: same
        // dot rhythm, same subdued ink, same rounding token, one raster for every empty
        // group because they are all the same size.
        cp.empty_palette_raster = Some(widget::image::Handle::from_rgba(
            bw,
            bh,
            crate::color::dotted_outline_rgba(bw, bh, radius, rim),
        ));
        cp.recent_rasters = cp
            .recents
            .iter()
            .map(|e| {
                (e.alpha != u8::MAX).then(|| {
                    widget::image::Handle::from_rgba(
                        d,
                        d,
                        crate::color::recent_swatch_rgba(e.color, e.alpha, d, d, radius),
                    )
                })
            })
            .collect();
    }

    /// A drag has gone LIVE: show Saved Palettes while it lasts (DRAGON-682 item 39).
    ///
    /// TRANSIENT, and that is the whole design: the tab the user was on is remembered here
    /// and put back by [`Self::end_drag`], and nothing is saved in between, so a drag can
    /// never change what the window opens on next time. It does nothing at all while the
    /// window is collapsed: a drag must not mount the panel (item 39's own rule).
    fn start_drag_tab_switch(&mut self) -> Task<cosmic::Action<Msg>> {
        if !self.color_picker.panel_mounted()
            || self.color_picker.panel_tab == geom::PanelTab::Palettes
        {
            return Task::none();
        }
        let from = self.color_picker.panel_tab;
        self.color_picker.drag_prev_tab = Some(from);
        self.color_picker.panel_tab = geom::PanelTab::Palettes;
        self.color_picker.sync_panel_tab_model();
        // The transient switch RESTORES the palettes' remembered offset (the UX round's
        // scroll memory), and the auto-scroll then moves the live value as the drag
        // needs; the revert in `end_drag` stores wherever the drag left it.
        self.switch_panel_scroll(from, geom::PanelTab::Palettes)
    }

    // **Tombstone: `reset_panel_scroll`** (DRAGON-687, retired by its UX round). Every
    // tab change used to put the ONE shared scrollable back at the top, because the two
    // tabs' contents are different lengths and the widget clamping an over-long offset
    // on its own would have stranded the drop machine's hit-test mirror at the
    // unclamped number. The owner asked for the opposite ("the palette tab should
    // remember where we scrolled to"), so the reset became a per-tab RESTORE
    // (`switch_panel_scroll`), which keeps the same desync impossible by the same means:
    // the restored value is clamped to the tab's current extent BEFORE it goes to both
    // the widget and the mirror, so the two still move together.

    /// Store the CURRENT tab's offset and restore `to`'s remembered one, clamped to its
    /// current extent, moving the widget and the hit-test mirror together
    /// (`geom::scroll_exchange` is the whole rule; see the tombstone above for what this
    /// replaced). The one switch path every tab change takes: the strip's click, the
    /// Ctrl+Tab cycle, the drag's transient switch and its revert.
    fn switch_panel_scroll(
        &mut self,
        from: geom::PanelTab,
        to: geom::PanelTab,
    ) -> Task<cosmic::Action<Msg>> {
        // The VISIBLE rows (item six): the restore clamps against what the tab will
        // actually lay out, filter included (item nine's one-extent rule).
        let rows = self.color_picker.visible_palettes().len();
        let cp = &mut self.color_picker;
        let max = geom::panel_max_scroll_for(to, geom::color_window_size().1, rows);
        let (mem, restored) =
            geom::scroll_exchange(cp.panel_tab_scroll, cp.panel_scroll_y, from, to, max);
        cp.panel_tab_scroll = mem;
        cp.panel_scroll_y = restored;
        cosmic::iced::widget::scrollable::scroll_to(
            cp.panel_scroll_id.clone(),
            cosmic::iced::widget::scrollable::AbsoluteOffset { x: Some(0.0), y: Some(restored) },
        )
    }

    /// END a drag, whatever ended it: a drop that did not land in a palette, a release
    /// over nothing, Escape, or the window losing the pointer.
    ///
    /// The reverting exit. Its one sibling, [`Self::end_drag_committing_palettes`], is
    /// the palette-drop ending (item five of the drag-jump round: the transient tab
    /// switch commits instead of reverting), and `geom::drop_commits_palette_tab` is
    /// the only chooser between the two, so the pair cannot be joined by a third path
    /// that forgets the ghost raster or the tab. It deliberately does NOT clear
    /// `palette_notice`: the notice outlives the drag that produced it (item 39),
    /// because the tab reverts underneath it and a notice nobody can read is not a
    /// notice.
    /// END a drag whose drop COMMITTED the Saved Palettes tab (item five of the
    /// drag-jump round): the transient activation becomes the real one. The remembered
    /// pre-drag tab is discarded instead of restored, the activation is PERSISTED
    /// exactly as `set_panel_tab` would persist a click, and the scroll stays wherever
    /// the drag (auto-scroll included) left it, which the per-tab memory picks up on
    /// the next real switch. The pre-drag tab's cursor is cleared for `set_panel_tab`'s
    /// own reason: it indexed the tab that is no longer coming back.
    fn end_drag_committing_palettes(&mut self) -> Task<cosmic::Action<Msg>> {
        self.color_picker.drag = None;
        self.color_picker.drag_raster = None;
        self.color_picker.zone_raster = None;
        if self.color_picker.drag_prev_tab.take().is_some() {
            self.color_picker.panel_cursor = None;
            self.save_state();
        }
        Task::none()
    }

    fn end_drag(&mut self) -> Task<cosmic::Action<Msg>> {
        self.color_picker.drag = None;
        self.color_picker.drag_raster = None;
        self.color_picker.zone_raster = None;
        if let Some(tab) = self.color_picker.drag_prev_tab.take() {
            let from = self.color_picker.panel_tab;
            self.color_picker.panel_tab = tab;
            self.color_picker.sync_panel_tab_model();
            // The revert half of the drag's transient switch (the UX round's scroll
            // memory): store where the drag left the palettes, auto-scroll included, and
            // restore the prior tab's own remembered offset.
            return self.switch_panel_scroll(from, tab);
        }
        Task::none()
    }

    /// Activate a panel tab: the ONE body behind the strip's click and the Ctrl+Tab cycle
    /// (DRAGON-687), so the persisted write, the cursor and menu clears and the scroll
    /// reset cannot fork between the two routes.
    fn set_panel_tab(&mut self, tab: geom::PanelTab) -> Task<cosmic::Action<Msg>> {
        // A tab change moves the interaction on, so an open rename commits (its group is
        // about to leave the screen).
        self.commit_rename_if_open();
        if self.color_picker.panel_tab == tab {
            // Still re-point the model: the widget's own activation can be ahead of the
            // enum when the click was on the already-active tab.
            self.color_picker.sync_panel_tab_model();
            return Task::none();
        }
        let from = self.color_picker.panel_tab;
        self.color_picker.panel_tab = tab;
        self.color_picker.sync_panel_tab_model();
        // The cursor, the menus and the local copy card belonged to the tab that is
        // leaving.
        self.color_picker.panel_cursor = None;
        self.color_picker.panel_menu = None;
        self.color_picker.palette_menu = None;
        self.color_picker.group_menu = None;
        self.color_picker.swatch_copied = None;
        self.save_state();
        self.switch_panel_scroll(from, tab)
    }

    /// Open the inline rename on `group`, seeded with its current name, focused and
    /// selected (DRAGON-687): the editor's `select_on_focus` answers the focus task with
    /// the whole name selected, the value boxes' own mechanism.
    fn begin_rename(&mut self, group: usize) -> Task<cosmic::Action<Msg>> {
        let Some(name) = self.color_picker.palettes.get(group).map(|p| p.name.clone()) else {
            return Task::none();
        };
        self.color_picker.rename = Some((group, name));
        // The ring's focus would fight the editor for the keyboard: Tab commits and
        // re-enters the ring (`keyboard.rs`), so nothing is lost by parking it.
        self.color_picker.focus = None;
        // Focus AND select-all, explicitly. `select_on_focus` covers the focus that
        // really changes; the explicit select covers re-entering a rename while the id
        // already held the caret (one shared editor id, so back-to-back renames are the
        // same widget to the toolkit and the focus operation is a no-op there).
        Task::batch([
            widget::text_input::focus(self.color_picker.rename_id.clone()),
            widget::text_input::select_all(self.color_picker.rename_id.clone()),
        ])
    }

    /// COMMIT any open rename (DRAGON-687): the shared tail of Enter, of the unfocus, and
    /// of every interaction that moves the user on (a tab change, a panel toggle, a drag
    /// press, a click into a value box). `geom::palette_rename` holds the rule: trimmed,
    /// empty or unchanged keeps the old name and saves nothing.
    pub(in crate::app) fn commit_rename_if_open(&mut self) {
        let Some((group, draft)) = self.color_picker.rename.take() else {
            return;
        };
        if self.apply_palette_change(geom::palette_rename(
            &self.color_picker.palettes,
            group,
            &draft,
        )) {
            log::debug!("color picker: renamed a palette");
        }
    }

    /// Apply a palette mutation's answer: `Some` assigns and saves, `None` is the shared
    /// "nothing changed, save nothing". Returns whether anything changed, so callers can
    /// log their own verb.
    fn apply_palette_change(&mut self, change: Option<Vec<geom::Palette>>) -> bool {
        let Some(palettes) = change else {
            return false;
        };
        self.color_picker.palettes = palettes;
        self.save_palettes();
        true
    }

    /// Write the saved palettes to THEIR file (DRAGON-687: `state::palettes`, beside the
    /// config, so factory resets spare them). The one writer every palette mutation
    /// funnels through, spelling each colour the recents' own way (`geom::Recent::hex`).
    fn save_palettes(&self) {
        let entries: Vec<crate::state::SavedPalette> = self
            .color_picker
            .palettes
            .iter()
            .map(|p| crate::state::SavedPalette {
                name: p.name.clone(),
                colors: p.colors.iter().map(|e| e.hex()).collect(),
            })
            .collect();
        crate::state::save_palettes(&entries);
    }

    /// Close every swatch and group context menu, whichever was open: the shared head of
    /// the palette actions, which can arrive from a menu OR a drop and must not leave a
    /// flyout floating over a list that just changed under it.
    fn close_swatch_menus(&mut self) {
        self.color_picker.panel_menu = None;
        self.color_picker.recents_menu = None;
        self.color_picker.palette_menu = None;
        self.color_picker.group_menu = None;
        // The toolbar's sort flyout (item six) and the main swatch's menu (item seven)
        // joined the one-menu-at-a-time family and close under the same sweep.
        self.color_picker.sort_menu_open = false;
        self.color_picker.main_menu = false;
        self.color_picker.menu_page = geom::MenuPage::Root;
    }

    /// The GHOST's raster, for a translucent colour (DRAGON-682 item 35).
    ///
    /// The same split swatch a translucent history entry wears
    /// ([`Self::refresh_recent_rasters`] builds those), because the owner asked for the
    /// ghost to be "the same shape as the swatches in the recent history area", and a
    /// half-transparent colour's shape includes its checkerboard. An OPAQUE colour needs no
    /// raster at all: the ghost paints it as a flat fill, exactly as an opaque history entry
    /// does.
    fn refresh_drag_raster(&mut self) {
        let Some((c, alpha)) = self.color_picker.drag.map(|d| d.payload) else {
            return;
        };
        if alpha == u8::MAX {
            self.color_picker.drag_raster = None;
            return;
        }
        let d = geom::DRAG_GHOST as u32;
        self.color_picker.drag_raster = Some(widget::image::Handle::from_rgba(
            d,
            d,
            crate::color::recent_swatch_rgba(c, alpha, d, d, picker_corner_radius()),
        ));
    }

    /// Recompute a live drag's highlighted ZONE for the pointer at `at`, rastering the
    /// dashed outline only when its RECT actually changed (DRAGON-682 item 41; one shared
    /// body since DRAGON-687 because THREE things move the highlight now: the pointer,
    /// the user's own wheel scrolling, and the drag auto-scroll).
    ///
    /// The raster refresh runs UNCONDITIONALLY and keys itself on the rect
    /// (`geom::zone_raster_size`, the drag-jump round's item three). It was gated on the
    /// zone's IDENTITY changing here, and that was the stale key: a viewport-clipped
    /// group rect GROWS under scrolling while its identity stands, so the outline froze
    /// at the clipped size while the wash quad tracked the live rect.
    fn refresh_drag_zone(&mut self, source: geom::DragSource, at: (f32, f32)) {
        let window = self.color_picker.window_size();
        let shape = self.color_picker.panel_shape();
        let zone = geom::zone_highlight(source, at, window, &shape);
        if let Some(drag) = self.color_picker.drag.as_mut() {
            drag.zone = zone;
        }
        self.refresh_zone_raster(source, zone);
    }

    /// The HIGHLIGHTED zone's dashed outline (DRAGON-682 item 41).
    ///
    /// Built when the highlight moves, not per frame: it is one image the size of a whole
    /// region, and a drag crosses at most a handful of them. The ink is the live accent, so
    /// the highlight follows the user's accent colour and their theme, exactly as the empty
    /// slots' dotted outline follows the subdued tone.
    ///
    /// Two DRAGON-687 zones deliberately raster NOTHING: the strip (a name drag's target
    /// is an insertion SLOT, drawn as an analytic line by the view), and a group being
    /// reordered WITHIN (same reason: the line is the highlight there, and a dashed box
    /// around the bar would say "append" while the drop means "reorder").
    fn refresh_zone_raster(&mut self, source: geom::DragSource, zone: Option<geom::DropZone>) {
        let boxed = match (source, zone) {
            (_, None) | (_, Some(geom::DropZone::PaletteStrip)) => None,
            (geom::DragSource::PaletteName(_), _) => None,
            (
                geom::DragSource::PaletteSwatch(g, _),
                Some(geom::DropZone::PaletteGroup(to_g)),
            ) if g == to_g => None,
            (_, Some(zone)) => Some(zone),
        };
        let Some(zone) = boxed else {
            self.color_picker.zone_raster = None;
            return;
        };
        // The SAME rect derivation the view's wash quad reads (`zone_rect` over the
        // live shape), so the two halves of the highlight cannot disagree; the size is
        // the cache key, and a matching cached raster is kept (equal sizes draw equal
        // dashes, so reuse across zone identities is correct too).
        let (_, _, w, h) =
            geom::zone_rect(zone, self.color_picker.window_size(), &self.color_picker.panel_shape());
        let cached = self.color_picker.zone_raster.as_ref().map(|(size, _)| *size);
        let Some((w, h)) = geom::zone_raster_size(cached, w, h) else {
            return;
        };
        let accent = crate::app::theme::accent(&cosmic::theme::active());
        let ink = [
            (accent.r * 255.0).round() as u8,
            (accent.g * 255.0).round() as u8,
            (accent.b * 255.0).round() as u8,
        ];
        self.color_picker.zone_raster = Some((
            (w, h),
            widget::image::Handle::from_rgba(
                w,
                h,
                crate::color::dashed_outline_rgba(
                    w,
                    h,
                    f64::from(geom::ZONE_HIGHLIGHT_RADIUS),
                    f64::from(geom::ZONE_HIGHLIGHT_STROKE),
                    ink,
                ),
            ),
        ));
    }

    /// Resize the picker window to whatever its CURRENT expansion state asks for, and
    /// re-pin the lock at the new size (DRAGON-682).
    ///
    /// **The window is pinned by `min_size == max_size`** (`open_color_picker_window`'s
    /// doc carries the whole story), which is exactly what stops a plain resize: a request
    /// to grow is clamped straight back to the old maximum. So the sequence is release,
    /// resize, re-pin, and it is CHAINED rather than batched, because `Task::batch`
    /// interleaves its streams and the three steps have to arrive in that order. A batch
    /// here would work most of the time and clamp the resize away the rest of it, which is
    /// the worst kind of bug to own.
    ///
    /// **Nothing platform-specific is disturbed.** The style mask is untouched on every
    /// platform, so macOS keeps `Titled | Resizable` (the DRAGON-130 abort winit's
    /// `is_zoomed` would otherwise hit) and non-mac keeps `resizable: false`; the min/max
    /// pair is the portable lock and the only thing that moves. macOS's other half,
    /// `pin_window_size`, disables the ZOOM BUTTON and full-screen spaces and knows nothing
    /// about the size, so it does not need re-running: it is still pinning, just at a
    /// different number. Windows' own centring and caption-button install are one-shot at
    /// open and equally size-independent, and the DRAGON-680 drag fix reads the title, not
    /// the geometry.
    /// THE swatch-copy path (DRAGON-682 items 30 and 32): the harmony menu's Copy entry,
    /// the recents menu's, and the panel cursor's accept key all arrive here.
    ///
    /// `at` is where the transient "Copied!" card is anchored, or `None` for a copy that has
    /// no swatch on screen to point at (the recents menu's, whose own feedback is the
    /// clipboard). It is passed IN rather than read from state because the two routes know
    /// different things: the menu route knows which menu is open, the keyboard route knows
    /// where its cursor is, and guessing from state would put a card on a harmony swatch
    /// after a copy from the history's menu.
    fn copy_swatch(
        &mut self,
        c: crate::color::Srgb,
        alpha: u8,
        at: Option<(usize, usize)>,
    ) -> Task<cosmic::Action<Msg>> {
        self.close_swatch_menus();
        self.color_picker.swatch_copied = at.map(|a| (a, std::time::Instant::now()));
        let text = self.color_picker.swatch_copy_text(c, alpha);
        log::debug!("color picker: copied a swatch as {}", self.color_picker.mode.id());
        // NO FLASH (DRAGON-682 item 15). It goes through the same `flash_copied` every
        // other copy does, and that function asks `geom::copy_flashes`, so "a swatch copy
        // does not light the button" is one decision with a test rather than the absence of
        // a line here.
        // The local card expires by the CLOCK, so it needs exactly one redraw at its end:
        // one delayed message, the same shape (and the same window) the copy button's own
        // flash uses.
        let expire = at.map(|_| {
            Task::perform(
                async {
                    tokio::time::sleep(crate::widgets::copy_button::COPIED_FLASH).await;
                },
                |()| cosmic::Action::App(Msg::ColorPicker(ColorPickerMsg::ClearSwatchCopied)),
            )
        });
        Task::batch(
            [
                Some(crate::share::copy_text_task(&text)),
                Some(self.flash_copied(geom::CopySource::SwatchMenu)),
                expire,
            ]
            .into_iter()
            .flatten(),
        )
    }

    pub(in crate::app) fn resize_color_picker_window(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(id) = self.color_picker.window else {
            return Task::none();
        };
        let (w, h) = geom::color_window_size_for(self.color_picker.expanded);
        let size = cosmic::iced::Size::new(w, h);
        log::debug!(
            "color picker: the panel is now {}",
            if self.color_picker.expanded { "open" } else { "closed" }
        );
        let resize = window::set_min_size(id, None)
            .chain(window::set_max_size(id, None))
            .chain(window::resize(id, size))
            .chain(window::set_min_size(id, Some(size)))
            .chain(window::set_max_size(id, Some(size)));
        // Windows only (DRAGON-668, re-run here by DRAGON-682): the native caption cluster
        // carves its frame OUT OF THE CLIENT, so a window pinned to an exact size lays its
        // content into a client that is short by the frame. `enforce_client_floor` measures
        // the live band and pushes the outer rect out to compensate, and the new width is a
        // new floor, so it has to run again after every resize rather than only at open.
        #[cfg(windows)]
        let resize = resize.chain(Task::done(cosmic::Action::App(Msg::WindowChrome(
            WindowChromeMsg::ColorPickerWindowFloor,
        ))));
        resize
    }

    /// Focus the FIRST value box (DRAGON-680, the owner's ask: "on window open or on mode
    /// change it should automatically have the first input focused").
    pub(in crate::app) fn focus_first_value_box(&mut self) -> Task<cosmic::Action<Msg>> {
        self.apply_picker_focus(geom::PickerFocus::Box(0))
    }

    /// Move the window's focus to `next`: record it, and make the TOOLKIT agree
    /// (DRAGON-680).
    ///
    /// The two halves are what this function is for. `geom::PickerFocus` is the app's own
    /// model, because the window's Tab ring is not the toolkit's (see `geom::next_focus`),
    /// and the toolkit still has to be told something on every move:
    ///
    /// * a BOX takes real focus, so it gets the caret and its own accent outline, and its
    ///   `select_on_focus` selects the whole value on arrival (libcosmic's `State::focus`
    ///   consults that flag, which is why this is a plain focus task and not a focus plus
    ///   select-all pair);
    /// * the ACTIVATOR and the HISTORY draw their frame from our state instead, and the
    ///   toolkit's focus is CLEARED, by aiming the focus operation at
    ///   `ColorPickerState::blur_id`, an id no widget carries. Leaving the caret in a box
    ///   behind them would be visible (two focus rings) and worse than visible: the box
    ///   would eat the arrow keys those two stops navigate with.
    pub(in crate::app) fn apply_picker_focus(
        &mut self,
        next: geom::PickerFocus,
    ) -> Task<cosmic::Action<Msg>> {
        self.color_picker.focus = Some(next);
        // The two navigation CURSORS live and die with their own stop (DRAGON-682 items 7
        // and 9). Entering seeds one: the history starts on the swatch the window is
        // currently showing, or its first, and the panel starts at its first swatch.
        // Leaving clears it, so a stop never draws a highlight it does not own and the
        // next visit starts somewhere predictable rather than where the last one ended.
        self.color_picker.recent_cursor = match next {
            geom::PickerFocus::History if !self.color_picker.recents.is_empty() => {
                Some(self.color_picker.selected_recent().unwrap_or(0))
            }
            _ => None,
        };
        self.color_picker.panel_cursor = match next {
            geom::PickerFocus::Panel => Some((0, 0)),
            _ => None,
        };
        let id = match next {
            geom::PickerFocus::Box(pos) => self.color_picker.box_id(pos),
            geom::PickerFocus::Mode
            | geom::PickerFocus::History
            | geom::PickerFocus::Panel => Some(self.color_picker.blur_id.clone()),
        };
        match id {
            Some(id) => widget::text_input::focus(id),
            None => Task::none(),
        }
    }

    /// File the colour the window is SHOWING into the history: the shared tail of the
    /// "Add to recents" button on the divider and of Enter in a value box.
    ///
    /// The colour is already the one on screen, so nothing about the window changes. This
    /// is deliberately NOT `apply_picker_color`, whose non-Edit sources reset the alpha
    /// and drop the draft: right for a colour that just ARRIVED, wrong for one the user
    /// built and is now keeping. What it borrows from a pick is the part that matters,
    /// `geom::push_recent`'s rule: the colour goes to the front, any earlier copy of it is
    /// removed rather than duplicated, and the list is capped. Filing a colour that
    /// already leads the history is therefore a no-op, which is what has to happen for a
    /// gesture that cannot know whether you made it twice.
    ///
    /// The save is the one a pick performs, for the same reason: an entry that vanished
    /// when this one-shot window closed would not be history.
    /// File `entry` into the history: THE one write path every explicit add goes through
    /// (DRAGON-682 item 28).
    ///
    /// The divider button and its chord file the colour the window is SHOWING; the harmony
    /// menu files a swatch the user pointed at. Both land here, so the rule
    /// (`geom::recents_after_add`: newest first, an existing copy promoted rather than
    /// duplicated, capped), the raster refresh and the save are one behaviour rather than
    /// two that agree today.
    ///
    /// It touches NOTHING else: not the active colour, not the alpha, not the tracked HSV,
    /// not the harmonies. That is what makes it safe for a menu on a swatch that is not the
    /// active colour.
    fn add_to_history(&mut self, entry: geom::Recent) {
        self.color_picker.recents =
            geom::recents_after_add(&self.color_picker.recents, entry, geom::RECENTS_CAP);
        self.refresh_recent_rasters();
        self.save_state();
        log::debug!("color picker: added a color to the history");
    }

    fn add_shown_color_to_history(&mut self) {
        // WITH the alpha (DRAGON-680, the owner's "we should be able to save to history
        // with transparency intact"). A pick is opaque by construction, so the Add button,
        // its primary+Enter chord and the harmony menu's own add are the only ways an alpha
        // you authored is ever kept.
        self.add_to_history(geom::Recent::new(self.color_picker.color, self.color_picker.alpha));
    }

    /// Copy the current mode's whole value and flash the copy affordance: the shared
    /// tail of the copy button and the mode stepper (DRAGON-630).
    fn copy_picker_value(&mut self) -> Task<cosmic::Action<Msg>> {
        let value = self.color_picker.value_text();
        log::debug!("color picker: copied the {} value", self.color_picker.mode.id());
        Task::batch([
            crate::share::copy_text_task(&value),
            self.flash_copied(geom::CopySource::CopyButton),
        ])
    }

    /// Raise the "Copied!" flash: the copy button's tick plus the word beside it, for
    /// [`crate::widgets::copy_button::COPIED_FLASH`].
    ///
    /// Called from every path that ACTUALLY writes the clipboard, which is why it is its
    /// own function rather than two lines inside the Copy button's handler. The window's
    /// own opening copy is one of those paths (the owner's ask: the flash should be up
    /// the moment the window appears, because by then the pick is already on the
    /// clipboard), and on the deferred route that moment is when the window takes focus
    /// and the write goes out, NOT when the pick happened. A copy that misses its
    /// deadline raises nothing: the flash says "this worked", so it may only appear where
    /// something did.
    ///
    /// The returned task is one delayed clear rather than a subscription ticking for two
    /// seconds: the flash needs exactly one redraw, at its end.
    fn flash_copied(&mut self, source: geom::CopySource) -> Task<cosmic::Action<Msg>> {
        // NOT every copy flashes (DRAGON-682 item 15): a swatch menu's does not, because
        // the flash is the copy BUTTON's acknowledgement and that button copies the
        // window's own value. Asked here, once, rather than by each caller remembering.
        if !geom::copy_flashes(source) {
            return Task::none();
        }
        self.color_picker.copied = Some((self.color_picker.mode, std::time::Instant::now()));
        Task::perform(
            async {
                tokio::time::sleep(crate::widgets::copy_button::COPIED_FLASH).await;
            },
            |()| cosmic::Action::App(Msg::ColorPicker(ColorPickerMsg::ClearCopied)),
        )
    }

    /// Finish the result window natively once its async-set title has landed: the mac
    /// traffic-light centring (which also reveals the window vibrancy), and the Windows
    /// centre-then-show plus caption buttons and Mica.
    ///
    /// Title-matched, so it polls on the same 30 x 40ms budget every other native
    /// finalize in the app uses, then gives up loudly rather than retrying forever.
    pub(in crate::app) fn finalize_color_picker_window(
        &mut self,
        id: window::Id,
        attempt: u8,
    ) -> Task<cosmic::Action<Msg>> {
        if self.color_picker.window != Some(id) {
            return Task::none();
        }
        let title_task =
            self.set_window_title(crate::app::color_picker::WINDOW_TITLE.to_string(), id);
        #[cfg(target_os = "macos")]
        {
            // This window is a REAL window, so it gets the REGULAR activation policy: a
            // Dock icon, a Cmd+Tab entry, and focus from another app, exactly like the
            // settings window. Without it the picker window was reachable only by clicking
            // it, and a Cmd+Tab away from it left no way back.
            //
            // WHY here and not in `app::boots_regular_policy`, which is where every other
            // window launch is answered: a colour-picker launch is overlay-FIRST. It mints
            // the same per-output capture-shaped overlays a screenshot does, and only a
            // pick turns it into a window launch. Booting Regular would therefore promote
            // the OVERLAY phase, which breaks two invariants at once: the DRAGON-154
            // AeroSpace opt-out only ignores a window whose owner is `.accessory` at the
            // window's first AX exposure, so the picker's overlays would start being tiled
            // off their displays, and DRAGON-151's menu-bar stamp would land in the frozen
            // snapshot the picker reads its colours FROM. It would also put a Dock icon up
            // for the two picker launches that open no window at all (a pick delivered to
            // an editor, DRAGON-587, or handed to an already-live picker window,
            // DRAGON-613). So the promotion waits for the window, the same post-boot flip
            // `finalize_preview_window` makes for the windowed preview and for the same
            // reason: the overlays are gone by now (`color_picker_pick` destroys them in
            // the batch that queues this open), and a flip with a window already up is
            // keyboard-healthy where the DRAGON-150 pre-window flip was not.
            crate::platform::mac::window::ensure_regular_policy();
            // The same activation pair the permission window uses: `gain_focus` reaches
            // winit's `activateIgnoringOtherApps`, and `activate_our_app` adds the macOS
            // 14+ cooperative form winit never calls. This window is opened by a process
            // that is not frontmost (a tray spawn, or its own overlay just closed), which
            // is exactly when the deprecated form can be declined. It runs AFTER the policy
            // above, since activating an Accessory app mints no Dock icon to activate into.
            crate::platform::mac::window::activate_our_app();
            let _ = attempt;
            Task::batch([
                window::gain_focus(id),
                title_task,
                Task::done(cosmic::Action::App(Msg::WindowChrome(
                    WindowChromeMsg::MacCenterTitlebar(crate::app::color_picker::WINDOW_TITLE, 0),
                ))),
                // DRAGON-587: and take the zoom button + full screen away, which is the half
                // of "never resizable" that `min_size == max_size` cannot reach. Its own
                // message rather than a fold into the centring poll above, because that one
                // also serves the settings window, which IS resizable.
                Task::done(cosmic::Action::App(Msg::WindowChrome(
                    WindowChromeMsg::MacPinWindow(crate::app::color_picker::WINDOW_TITLE, 0),
                ))),
            ])
        }
        #[cfg(windows)]
        {
            const MAX_ATTEMPTS: u8 = 30;
            const RETRY_MS: u64 = 40;
            let title = crate::app::color_picker::WINDOW_TITLE;
            // Centre it on the display the pick happened on, which is where the user is
            // looking. `preview_overlay_rect(None)` falls back to the pointer's display.
            let (pos, size) = crate::platform::windows::window::preview_overlay_rect(
                self.color_picker.pick_output.as_deref(),
            );
            let monitor = (pos.0, pos.1, size.0 as i32, size.1 as i32);
            if crate::platform::windows::window::show_centered(title, monitor) {
                // Chrome, not glass: the caption buttons are installed unconditionally,
                // exactly as the preview window does.
                crate::platform::windows::caption::install_native_caption_buttons(title);
                // ...and that install is what makes the window narrower than its own layout
                // (DRAGON-668). The subclass carves a non-client frame OUT OF THE CLIENT
                // (`caption::calc_frame`), 16x8 physical px measured here, while leaving the
                // OUTER rect alone. The picker is pinned `min_size == max_size ==
                // color_window_size()`, so unlike every other window it cannot grow to absorb
                // that: its 366pt of sections were being laid out into a 350pt client, and the
                // shortfall lands entirely on the last element of each row — the copy button,
                // and the fourth of the four component boxes.
                //
                // `enforce_client_floor` is the settings window's existing answer and it fits
                // exactly: it takes a LOGICAL client size, measures the live non-client band
                // rather than predicting it, and returns without touching anything once the
                // client is already at or above the floor, so re-running it on a DPI change
                // cannot make the window creep. The floor here IS the window's fixed size,
                // which is the one size the layout is built for.
                let (cw, ch) = crate::app::color_picker::geom::color_window_size();
                crate::platform::windows::window::enforce_client_floor(
                    title,
                    (cw.round().max(1.0) as u32, ch.round().max(1.0) as u32),
                );
                if self.glass.is_some_and(|g| g.frosted_windows) {
                    crate::platform::windows::window::apply_window_glass(title);
                }
                return title_task;
            }
            if attempt >= MAX_ATTEMPTS {
                log::warn!(
                    "color picker window never matched its title after {MAX_ATTEMPTS} attempts \
                     — it may stay hidden"
                );
                return title_task;
            }
            Task::batch([
                title_task,
                Task::perform(
                    async move {
                        tokio::time::sleep(std::time::Duration::from_millis(RETRY_MS)).await;
                    },
                    move |()| {
                        cosmic::Action::App(Msg::WindowChrome(
                            WindowChromeMsg::ColorPickerWindowOpened(id, attempt + 1),
                        ))
                    },
                ),
            ])
        }
        #[cfg(all(not(target_os = "macos"), not(windows)))]
        {
            let _ = attempt;
            title_task
        }
    }
}

/// DRAGON-TBD: which resample routes may leave the magnifier's raster stale. The pacing is a
/// performance choice and the keyboard nudge is a correctness one, so the property pinned here
/// is that the first can never reach the second.
#[cfg(test)]
mod raster_policy_tests {
    use super::*;
    use std::time::Duration;

    /// Every speed worth naming, from stopped to an absurd one, so "any speed" below is not
    /// three convenient samples.
    const SPEEDS: [f32; 7] =
        [0.0, 100.0, geom::DELIBERATE_SPEED, 2_000.0, geom::FLICK_SPEED, 50_000.0, f32::NAN];

    /// THE guarantee the owner asked for. An arrow key (or its vim letter) must move the
    /// sample and rebuild the lens immediately, EVERY time, and it routes through
    /// `color_picker_resample`, which is `RasterPolicy::Always`. No pointer speed and no
    /// recent raster may make that skip.
    ///
    /// The realistic hazard is not hypothetical: a nudge is most likely pressed just after the
    /// hand has stopped moving, which is exactly when the measured speed is still high and a
    /// raster is freshly spent. Both conditions are pinned here together.
    #[test]
    fn a_keyboard_nudge_is_never_paced_whatever_the_pointer_was_doing() {
        for speed in SPEEDS {
            for since in [None, Some(Duration::ZERO), Some(Duration::from_micros(1)),
                          Some(Duration::from_millis(39)), Some(Duration::from_secs(9))]
            {
                assert!(
                    !skips_raster(RasterPolicy::Always, since, speed),
                    "an unpaced route skipped its raster at {speed} pt/s, {since:?} since the last"
                );
            }
        }
    }

    /// The same for the two other `Always` routes, which are correctness paths too: the
    /// frozen-snapshot delivery that makes the loupe appear at all (DRAGON-601) and a
    /// session's first sample. Both share the arm above, so this is really one more statement
    /// of intent than a second mechanism, and it is worth stating because the pacing would be
    /// invisible on those routes until someone reported a picker that opened blank.
    #[test]
    fn the_first_lens_of_a_session_is_never_paced() {
        for speed in SPEEDS {
            assert!(!skips_raster(RasterPolicy::Always, None, speed));
            // …and even on the paced route, "nothing rastered yet" always rasters.
            assert!(!skips_raster(RasterPolicy::Paced, None, speed));
        }
    }

    /// The pacing still DOES something, or this seam would be decoration: on the pointer's own
    /// route, mid-flick and one frame after a raster, it declines.
    #[test]
    fn the_pointer_route_still_paces_a_flick() {
        let one_frame = Some(Duration::from_micros(8_333));
        assert!(skips_raster(RasterPolicy::Paced, one_frame, geom::FLICK_SPEED));
        // And releases as soon as the hand slows, on the very next look.
        assert!(!skips_raster(RasterPolicy::Paced, one_frame, 0.0));
        assert!(!skips_raster(RasterPolicy::Paced, one_frame, geom::DELIBERATE_SPEED));
    }
}

/// DRAGON-587: what the PICK's clipboard copy does, on each session shape. It pins the
/// picker to the app's ONE copy ladder (`share::copy_step`) rather than to a second copy of
/// the reasoning, which is the whole point of the fix.
#[cfg(test)]
mod pick_copy_tests {
    use crate::share::{CopyRoute, CopyStep, copy_step};

    /// The pick's own inputs: the result window is minted BY this handler, so it can never
    /// hold focus yet, and the budget is fresh. Those two terms are fixed; only the session's
    /// route varies, and it decides everything.
    const AT_PICK: (bool, bool) = (false, false);

    /// A data-control Linux session, macOS and Windows are BYTE-IDENTICAL to before the fix:
    /// the detached worker takes the selection in the pick's own batch and outlives us.
    #[test]
    fn a_standalone_session_still_copies_in_the_pick_itself() {
        assert_eq!(
            copy_step(CopyRoute::Standalone, AT_PICK.0, AT_PICK.1),
            CopyStep::Detached
        );
    }

    /// THE reported bug. A Flatpak on COSMIC (and GNOME, and sandboxed niri/Hyprland) has no
    /// data-control, so the hex can only ride one of our own focused surfaces — and at the
    /// instant of the pick there is none, because the same batch destroys the overlay and only
    /// QUEUES the result window. The answer must be "wait", never "write now".
    #[test]
    fn a_window_route_pick_waits_for_the_result_windows_focus() {
        assert_eq!(
            copy_step(CopyRoute::ThisWindow, AT_PICK.0, AT_PICK.1),
            CopyStep::WaitForFocus
        );
        assert_ne!(
            copy_step(CopyRoute::ThisWindow, AT_PICK.0, AT_PICK.1),
            CopyStep::ThroughWindow,
            "writing at pick time is exactly what put nothing on the clipboard"
        );
    }

    /// Once the window HAS the keyboard the deferred write goes out, and a wait that outlives
    /// the budget is reported rather than forgotten. These are the two ends
    /// `flush_deferred_pick_copy` and `PickCopyDeadline` implement.
    #[test]
    fn the_focus_writes_and_the_budget_reports() {
        assert_eq!(copy_step(CopyRoute::ThisWindow, true, false), CopyStep::ThroughWindow);
        assert_eq!(copy_step(CopyRoute::ThisWindow, false, true), CopyStep::ReportFailed);
    }
}

/// The value the pick puts on the clipboard: the REMEMBERED mode's spelling
/// (DRAGON-630, the owner's ask). Hex is the default mode, so DRAGON-582's original
/// wording ("the hex INCLUDING the leading `#`") still holds on an untouched config,
/// and both facts are pinned because the copy is a string the user pastes into a
/// stylesheet: its exact spelling is the feature.
#[cfg(test)]
mod pick_copy_text_tests {
    use crate::app::color_picker::ColorPickerState;
    use crate::color::{ColorFormat, Srgb};

    #[test]
    fn the_default_mode_still_copies_the_leading_hash_hex() {
        let st = ColorPickerState { color: Srgb::new(255, 136, 0), ..Default::default() };
        assert_eq!(st.value_text(), "#FF8800");
        assert!(
            st.value_text().starts_with('#'),
            "a hex without the # is not a colour anyone can paste"
        );
    }

    /// A remembered RGB mode makes the pick copy `rgb(…)`: the mode the user left the
    /// window in is the spelling the next pick delivers.
    #[test]
    fn a_remembered_mode_spells_the_pick_copy() {
        let st = ColorPickerState {
            color: Srgb::new(255, 136, 0),
            mode: ColorFormat::Rgb,
            ..Default::default()
        };
        assert_eq!(st.value_text(), "rgb(255, 136, 0)");
    }
}
