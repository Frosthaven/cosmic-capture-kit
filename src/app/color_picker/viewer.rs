//! The PALETTE VIEWER launch (DRAGON-680): the colour picker's result window on its own.
//!
//! # What it is, and what it deliberately is not
//!
//! `--palette-viewer` opens the window a pick would have opened, with NO overlay and NO
//! pick. The owner's framing is the whole specification: the user wants to see and reuse
//! the palette they have already built, and asking them to sample a pixel first, just to
//! reach a list of colours they saved earlier, is a toll on a window that is already
//! theirs.
//!
//! So this launch skips the overlay phase entirely rather than passing through it. Two
//! consequences, both deliberate:
//!
//! * It is a WINDOW launch, so [`crate::app::Startup::opens_overlays`] answers false and it
//!   takes the settings window's routing (the GPU renderer, no permission probe, no
//!   capture surfaces). On macOS it is also why the launch does not arm the
//!   [`crate::startup_guard`] budget: that guard's `Surfaces` snapshot recognises overlays,
//!   captures, previews and settings, none of which this launch has, so arming it would
//!   quietly kill a window somebody was reading.
//! * [`crate::app::App::color_picking`] stays FALSE here. That flag means "this process is
//!   the picker OVERLAY", and it gates the overlay's view, its keyboard grab and the frozen
//!   flats grab the sample needs. The viewer has none of those, so claiming it would arm
//!   machinery with nothing to act on.
//!
//! # Which colour it opens on
//!
//! The most recent entry in the persisted recents, or [`DEFAULT_COLOR`] when there are none
//! ([`viewer_color`]). The window always shows SOME colour, so the question is only which,
//! and the newest pick is the one the user was most recently working with.
//!
//! The load is a [`geom::ColorSource::RecentClick`], which is exactly what it is: loading a
//! colour that is already in the list. That source does not write the recents
//! ([`geom::writes_recents`]), which is the property that matters here. A viewer launch is
//! not a pick, so it must not reorder the palette it exists to display, and opening the
//! window ten times in a row must leave the list byte-identical.
//!
//! It also does not touch the CLIPBOARD. A pick copies because taking a colour IS the act
//! of asking for it; merely looking at the palette is not, and silently replacing whatever
//! the user had copied would be a side effect nobody asked for. The window's own copy
//! button is still one click away.
//!
//! # When a picker window is already open
//!
//! **At most one colour picker window exists at a time** (DRAGON-613, and see this
//! module's parent doc). That rule is CROSS-PROCESS, because every picker launch is its own
//! process, and the viewer honours it the same way a pick does: it looks for a live picker
//! window first, and only opens one of its own if nothing answered.
//!
//! What it sends is a RAISE ([`crate::preview_ipc::Request::Raise`]), not a colour, and the
//! distinction is the reason the verb was added rather than reusing the colour one. A
//! colour send is not inert: the receiver would show it, promote it to the front of ITS
//! recents and write it to the clipboard. Applied to a viewer launch that would mean
//! "opening the palette" silently replaced the live window's colour with this process's
//! own (possibly stale) front recent. A raise only brings the window forward, which is the
//! whole of what the user asked for.
//!
//! The check runs in `main`, BEFORE the GUI stack is paid for, so the common
//! already-open case costs a socket round trip and an exit rather than a whole iced boot.
//! A refusal of any kind (no window, one mid-close, an older build that does not know the
//! verb, a platform with no transport) falls through to opening a window here, which is
//! the same never-lose-the-request ladder every pick takes.

use super::*;

/// THE colour the picker window shows when it has no history to open on (DRAGON-682 item
/// 10): `#FF06B5`, opaque.
///
/// The owner's pick, and it replaces opaque white. White was chosen as "the neutral start
/// every colour tool opens on", which is true and is exactly the problem: a white window
/// on a first run is indistinguishable from a window that failed to load anything, and
/// every control in it (the gradient square's marker, the round swatch, the value boxes)
/// reads as blank. A saturated colour makes a first-run window obviously WORKING.
///
/// ONE constant, so nothing can open on a second opinion about what "no colour yet" means.
pub const DEFAULT_COLOR: Srgb = Srgb { r: 0xFF, g: 0x06, b: 0xB5 };

/// **Pure**, unit-tested: the entry a palette-viewer launch opens on, given the recents
/// list (newest first). The entry carries its saved ALPHA (DRAGON-680's history model),
/// so a palette filed with transparency comes back whole.
///
/// The newest entry, or [`DEFAULT_COLOR`] for an empty list. Written as a decision of its
/// own, in the shared tree, because it is the one thing about this launch that could
/// silently go wrong on a platform nobody is testing: an off-by-one here would open the
/// viewer on the user's SECOND-newest colour, which no compiler and no clippy lint can see.
pub fn viewer_color(recents: &[geom::Recent]) -> geom::Recent {
    recents.first().copied().unwrap_or(geom::Recent::opaque(DEFAULT_COLOR))
}

impl App {
    /// Open the palette viewer's window (DRAGON-680): the picker's result window, loaded
    /// with [`viewer_color`], with no overlay and no pick behind it.
    ///
    /// The window-side setup is the SAME set `color_picker_pick` performs after a pick, and
    /// it has to be, because what opens is the same window: the handoff listener is bound
    /// BEFORE the marker is published (a later pick that discovers this process must find it
    /// listening), and the marker is what makes this window discoverable at all, both as the
    /// destination of a later pick and as the sibling a later capture must spare.
    ///
    /// Idempotent: a second call while a window is already open does nothing, so the
    /// single-window rule holds WITHIN this process as well as across processes.
    pub(in crate::app) fn open_palette_viewer(&mut self) -> Task<cosmic::Action<Msg>> {
        if self.color_picker.window.is_some() {
            return Task::none();
        }
        // The colour itself is user content and is never logged (see the parent doc's
        // privacy section); the count is behaviour, not content.
        log::debug!(
            "palette viewer: opening the picker window over {} saved colors",
            self.color_picker.recents.len()
        );
        let entry = viewer_color(&self.color_picker.recents);
        // `RecentClick`, so the load cannot write the recents: a viewer must not reorder the
        // palette it exists to show. It routes through `set_picker_color` rather than
        // assigning the field, because that is the one place the HSV tracking and the
        // gradient rasters are kept in step with the colour. The alpha rides in the call
        // (DRAGON-680 kept it whole; DRAGON-687 item ten made it the parameter), and
        // item ten's outgoing bump inside the apply skips this load by its window-open
        // gate: the window is minted below, so what this replaces is the state's
        // default, not a colour the user held.
        self.set_picker_color(entry.color, Some(entry.alpha), geom::ColorSource::RecentClick);
        // EVERY raster, before the window exists (DRAGON-682 item 25). THIS is the path the
        // bug lived on: a viewer launch writes no recents, so nothing here had ever built
        // the history's swatches, the empty slots' dots or the harmony checkerboard, and the
        // window showed a solid-outline fallback and an invisible board until the user
        // happened to remove a recent. One call now, and it asserts its own completeness.
        self.ensure_all_rasters();
        let (id, open) = super::open_color_picker_window(self.color_picker.expanded);
        self.color_picker.window = Some(id);
        // Bind the listener BEFORE the marker below, the ordering `color_picker_pick`
        // documents: the marker IS the discovery record, so binding second would leave a
        // window in which a pick finds a window that is not listening. A bind failure is
        // non-fatal (we simply do not receive).
        #[cfg(any(unix, windows))]
        if self.handoff_host.is_none() {
            let addr = crate::instance::color_picker_host_address(std::process::id());
            match crate::preview_ipc::start_host_at(addr) {
                Ok(host) => {
                    log::info!("palette viewer: listening for picks on {}", host.address());
                    self.handoff_host = Some(host);
                }
                Err(e) => log::warn!("palette viewer: not listening for picks ({e})"),
            }
        }
        // Advertise the open window: a later capture's sibling sweep must spare this
        // process, and a later pick must find this window instead of opening a second one.
        // Cleared at `finish_session`, like the preview marker.
        crate::instance::set_color_picker_marker(true);
        open
    }

    /// Bring this process's picker window forward for a palette-viewer launch that found it
    /// (DRAGON-680).
    ///
    /// `None` when this process has no picker window, which the drain turns into a refusal
    /// so the sender opens its own rather than raising nothing. `Some(task)` once the raise
    /// is actually in flight, which is the only point at which the drain may ack.
    ///
    /// It changes NO state, and that is the point of the verb: the window keeps the colour
    /// it was showing, its recents keep their order, and the clipboard is untouched. The
    /// only thing that happens is the thing that was asked for.
    // Its only caller is the drain (`drain_preview_handoffs`), which is a `Task::none()`
    // stub where no transport exists, so this is honestly dead only there.
    #[cfg_attr(not(any(unix, windows)), allow(dead_code))]
    pub(in crate::app) fn raise_picker_window(&mut self) -> Option<Task<cosmic::Action<Msg>>> {
        let id = self.color_picker.window?;
        log::debug!("palette viewer: another launch asked for this window; raising it");
        // macOS: `gain_focus` reaches winit's `activateIgnoringOtherApps`, and
        // `activate_our_app` adds the macOS 14+ cooperative form winit never calls. The
        // request comes from a process that is not frontmost, which is exactly when the
        // deprecated form can be declined, and a raise that does not raise is the one
        // outcome this path may not have. The window is already up, so the Regular policy
        // flip `finalize_color_picker_window` performs has long since happened.
        #[cfg(target_os = "macos")]
        crate::platform::mac::window::activate_our_app();
        Some(window::gain_focus(id))
    }
}

#[cfg(test)]
mod viewer_color_tests {
    use super::*;

    fn entry(r: u8, g: u8, b: u8, alpha: u8) -> geom::Recent {
        geom::Recent {
            color: Srgb::new(r, g, b),
            alpha,
        }
    }

    /// THE rule: the window opens on the newest entry in the palette, alpha included.
    #[test]
    fn a_viewer_opens_on_the_newest_recent() {
        let recents = vec![entry(10, 20, 30, 128), entry(40, 50, 60, 255)];
        let got = viewer_color(&recents);
        assert_eq!(got.color, Srgb::new(10, 20, 30));
        assert_eq!(got.alpha, 128, "the saved transparency comes back whole");
    }

    /// The empty-palette case, which is every first run: the owner's default colour
    /// (DRAGON-682 item 10, replacing opaque white), never a panic and never a window with
    /// nothing in it.
    #[test]
    fn an_empty_palette_opens_on_the_default_colour() {
        let got = viewer_color(&[]);
        assert_eq!(got.color, DEFAULT_COLOR);
        assert_eq!(got.color.hex(), "#FF06B5", "the owner's own value");
        assert_eq!(got.alpha, u8::MAX, "and opaque");
    }

    /// A one-entry palette is the same rule, stated separately because it is the shape a
    /// `first()`-vs-`last()` mix-up cannot be caught in.
    #[test]
    fn a_single_recent_is_the_one_it_opens_on() {
        let only = entry(7, 8, 9, 42);
        let got = viewer_color(&[only]);
        assert_eq!(got.color, only.color);
        assert_eq!(got.alpha, only.alpha);
    }

    /// The list is read, never rewritten. The viewer's whole promise is that looking at the
    /// palette leaves it alone, and the load's `ColorSource` is what guarantees it.
    #[test]
    fn loading_the_viewers_colour_never_writes_the_recents() {
        assert!(!geom::writes_recents(geom::ColorSource::RecentClick));
    }
}
