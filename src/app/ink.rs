//! Live recording annotation (DRAGON-691): the SURFACE, the scene and the lifecycle.
//!
//! The maths is `crate::ink` (pure, tested on every host) and the drawing is
//! `widgets::ink_canvas`. This file is what sits between them: the strokes the user has
//! drawn, the one extra surface they are drawn on, and the rules for when that surface
//! exists.
//!
//! # Why the ink gets its OWN surface
//!
//! The recording toolbar is not a window, it is iced UI drawn inside the per-output capture
//! overlay, and since DRAGON-690 that overlay OPTS OUT of the recordings we make: macOS
//! builds the stream's content filter excluding it, Windows sets `WDA_EXCLUDEFROMCAPTURE` on
//! it. Ink drawn on that surface would be excluded with it: on screen, absent from the file,
//! which is the exact opposite of the feature.
//!
//! So the ink gets a second surface, and its whole contract is that it does NOT call
//! `platform::exclude_window_from_capture`. That seam is per-window precisely so these two
//! can differ (its own doc says so, and `platform::mac_window_is_capture_excluded` has a test
//! naming this ticket), so the correct implementation here is to not ask. There is nothing to
//! decline on Linux, where no exclusion exists at all.
//!
//! # Z-order, per platform
//!
//! The toolbar has to stay visible and clickable ABOVE the ink, or the user can select a
//! drawing tool and then not reach the Pointer button that gives the screen back.
//!
//! * **Linux** puts the ink on `Layer::Top` and the capture overlays stay on
//!   `Layer::Overlay`, so the compositor orders them correctly by construction and nothing
//!   has to be re-minted when the ink appears.
//! * **macOS** raises the ink window to one level BELOW the capture overlays' shielding
//!   level (`platform::mac::window::place_ink_overlay`).
//! * **Windows** has one topmost band with no sub-levels, so the ink is placed topmost and
//!   the capture overlays are then re-asserted topmost, which puts them back on top of it.
//!
//! # Input, per platform
//!
//! While a DRAWING tool is selected the ink surface takes the pointer, EXCEPT over the
//! toolbar.
//!
//! * **Linux** says that exactly: the layer surface's input region is the output minus the
//!   toolbar rect (`crate::ink::rect_subtract`). A region can only be changed by re-minting
//!   the surface, so a tool change and a toolbar DRAG each re-mint this one surface. The
//!   capture overlays are never touched by it. The armed surface also takes `OnDemand`
//!   KEYBOARD, so Escape and the `V` disarm key reach the app while the user is drawing; see
//!   `shell::ink_surface`.
//! * **macOS and Windows** have no per-rect input region, so they use the same 60ms hover
//!   poll the countdown/recording overlays already use (`App::passthrough_poll`), one window
//!   richer: the pointer over the toolbar makes the ink click-through and the toolbar solid,
//!   and anywhere else does the reverse.
//!
//! The consequence on those two platforms is that while a drawing tool is selected the user
//! cannot click the application being recorded. That is accepted rather than overlooked: it
//! is what every screen-drawing tool does, it is a mode the user turned on, and FOUR things
//! turn it off: the Pointer button, Escape, a RIGHT CLICK and bare `V` (DRAGON-742). The last
//! three are one method, [`App::disarm_ink_tool`], on purpose.
//!
//! # The three TIERS, and what each one mints (DRAGON-693)
//!
//! Everything above is the GOLD tier: macOS, Windows, and any Wayland session with
//! `wlr-layer-shell` that can place what it is recording. Two more exist.
//! `crate::ink::tier` grades the session once, at `App::begin_ink_session`, and the rules
//! follow from the grade rather than from scattered branches.
//!
//! * **Silver** (region / monitor) mints ONE ordinary fullscreen toplevel when a tool is
//!   armed (`shell::ink_armed_window`), a deliberate exception to that teardown. It is
//!   focused and opaque to input, so this tier is "annotate OR control": the window hosts
//!   both the canvas and the same tool selector the toolbar shows. The portal's cast carries
//!   the ink live, so no bake is involved.
//! * **Bronze** (a window cast we cannot place) shows the last frame the recording SAW,
//!   fitted in that same window, and records strokes in STREAM pixels. That is what makes it
//!   need no window position: the bake is an identity mapping over the frame. The frame
//!   arrives through a one-shot `record::FramePeek` request, served by the CPU PipeWire
//!   worker; a zero-copy recording serves none (GPU buffers the CPU never maps) and the
//!   window falls back to a plain scrim, with the ink still working and still landing in the
//!   file.
//!
//!   TWO sessions land here. The no-layer-shell one recording a window is DRAGON-693's.
//!   DRAGON-738 added the other: ANY session, layer shell or not, whose portal window cast
//!   could not be matched to a toplevel, because only `zcosmic_toplevel_info_v1` carries a
//!   toplevel's geometry. KDE and wlroots can never match one; COSMIC may fail to when two
//!   windows share a size and DRAGON-737's pixel tie-break cannot separate them either.
//!   Those sessions were graded Gold, so they minted a layer surface, drew on it perfectly,
//!   and delivered a file with no ink in it whatsoever. Bronze is the only tier whose armed
//!   window is NOT in the recording (it casts one window's own buffer), which is why it is
//!   also the only one allowed to print anything on itself.
//!
//! Neither can leave ink up after a disarm, so both record a FINAL SWEEP
//! (`crate::ink::InkLog::wipe`) and the window goes when the sweep ends. Gold records none,
//! and its ink stays.
//!
//! # Window mode: the ink is DATA, and it is composited in afterwards
//!
//! Region and monitor recordings capture the ink the way they capture everything else on the
//! desktop, and nothing in `src/record/` is involved. WINDOW recordings split by BACKEND, not
//! by platform (DRAGON-736 corrected this; it used to say "by platform" and that cost the
//! whole feature on one Linux route):
//!
//! * **Linux NATIVE screencopy** records a fixed screen RECTANGLE even in window mode (that
//!   mode funnels into `spawn_screencopy_worker`'s region crop), so the overlay is captured
//!   live there too and there is nothing more to do.
//! * **macOS, Windows, and a Linux PORTAL window cast** capture the target window's OWN
//!   buffer, which no floating surface is part of. So the strokes are also kept as DATA, in
//!   MEDIA time (`crate::ink::InkLog`), together with a timeline of where the window was
//!   (`InkState::window_rects`). A recording with no ink is byte-identical to before.
//!
//! The replay is `crate::ink::visible_at` / `fill_runs_at`, and the overlay renders through
//! exactly those, so what lands in the file is what was watched.
//!
//! **WHERE that data is composited changed in DRAGON-724, and the two paths are not equals.**
//!
//! * The normal path is `record::ink_live`: [`App::publish_live_ink`] publishes the scene to the
//!   capture worker whenever it changes, and the worker paints it onto each frame on its way to
//!   the encoder. The finalize pass then stream-copies its video like every other recording.
//! * The FALLBACK is `record::ink_bake`, kept for a frame too big to paint per tick
//!   (`ink_live::live_ink_affordable`). [`App::publish_ink_bake`] fills that slot at stop, and
//!   the worker DROPS it unread when it painted live instead. The app deliberately does not know
//!   which path ran: the gate needs the encode size and the frame rate, which only the worker
//!   has, so publishing both is what makes the fallback need no coordination.
//!
//! Why it moved: that pass re-encoded the whole video (28.2s of spinner on a measured 3m36s
//! take, 7.7x realtime on the hardware tier), and, because it married two clocks, the marks led
//! the cursor that drew them by the length of the worker's opening phase. See `ink_live`'s
//! module doc for the measurement and for why the live path cannot inherit that offset.
//!
//! **The scene is APPEND-ONLY for the whole session, and that is a hard rule rather than
//! tidiness.** The same `InkLog` this file grows is what the bake replays from media 0
//! afterwards, so a stroke that has finished dissolving, or one an Erase has cleared, is still
//! part of the past and must stay in the log. This file used to drop both, and the note where
//! `retire_spent_ink` was records what that cost. Nothing here removes a clip, a generation or
//! an erase; what is on SCREEN is [`crate::ink::any_visible_at`]'s answer about the current
//! media instant, and the surface's lifetime is the only thing that reads it.
//!
//! **That timeline is a single point of failure, and it failed once.** `InkBake::is_empty` is
//! true whenever `window_rects` is empty, and an empty timeline means the bake never runs, so a
//! `platform::window_rect` arm that quietly answers `None` costs the user the entire drawing
//! with the video still arriving intact. DRAGON-691 shipped exactly that on macOS: the window
//! server was asked with `kCGWindowListOptionOnScreenOnly` and the target's own id in the
//! `relativeToWindow` slot, which returns the on-screen list MINUS that window, so the lookup
//! could never match. The lesson is in the seam's doc (`platform::window_rect`) and in the
//! macOS arm's; here, [`InkState::window_rect_misses`] exists so the warning below can say
//! WHICH of "nothing was asked" and "nothing answered" happened, rather than one line meaning
//! both.

use super::*;

/// One stroke of live ink.
///
/// The three parallel arrays are the same shape the preview editor's pencil keeps
//  (`AnnotKind::Pen` plus `EditState::pen_raw`), for the same reason: the smoothing pipeline
/// is CAUSAL, so the beautified stroke can be re-fitted on every sample and the settled part
/// never moves. `fit` is what is drawn, and it is re-derived from `raw` each time a sample
/// lands.
///
/// `raw_ms` is the one thing the preview's pencil does not carry: WHEN each raw sample was
/// taken, as an offset from this stroke's own start. Transferred onto the drawn curve as
/// `ages`, which is the record a future window-mode BAKE needs, and which keeps draw order
/// meaningful inside one stroke. The DISSOLVE itself is driven by the scene-wide activity
/// clock instead (`InkState::last_activity`); see `crate::ink`'s module doc.
/// The stroke in flight: the raw pointer trail the smoothing is re-fitted from.
///
/// Live only. It exists to be re-fitted, and when the stroke ends it is dropped: the finished
/// record is the clip's own `points` / `speed` / `point_ms` in `crate::ink::InkLog`, which is
/// what both the overlay and the bake read.
pub(super) struct LiveStroke {
    /// The raw pointer trail in SURFACE POINTS, thinned at
    /// [`crate::app::preview::annotate::PEN_MIN_STEP`] like the editor's pencil.
    raw: Vec<(f32, f32)>,
    /// Each raw sample's offset in MEDIA ms from the stroke's own start. Media rather than
    /// wall, so a sample taken while the recording is PAUSED carries the frozen offset and
    /// lands exactly where the file is frozen.
    raw_ms: Vec<u32>,
    /// The clip in `InkLog::clips` this trail is being fitted into. Stable for the stroke's
    /// whole life, because the log is append-only: the index of an existing clip can never
    /// move.
    clip: usize,
    /// The media instant the stroke began; `raw_ms` counts from here.
    start_ms: crate::ink::MediaMs,
}

/// The ink surface, once minted.
pub(super) struct InkSurface {
    /// The surface's own window id.
    pub id: window::Id,
    /// The NAME of the output it covers (`OutputState::name`), so a re-mint can find its
    /// output again and the view can size against it.
    pub output: String,
    /// DRAGON-693: is this an ordinary TOPLEVEL rather than a layer surface / native overlay?
    ///
    /// True only on the Silver and Bronze tiers, whose session has no layer shell at all. It
    /// decides two things a caller must not guess: how the surface is CLOSED (a toplevel is
    /// `window::close`, a layer surface is `destroy_layer_surface`, and using the wrong one
    /// leaves the surface up), and whether the view draws the armed chrome.
    pub plain: bool,
    /// The toplevel's own size in POINTS, as its last configure reported it. `Some` only for a
    /// `plain` surface, where it is what the Bronze snapshot is fitted inside and what the
    /// letterbox bridge to the granted monitor is built from. A layer surface needs none of
    /// it: it is exactly its output.
    pub win_size: Option<(f32, f32)>,
    /// Whether the surface was minted for a DRAWING tool. On Linux this decides its input
    /// region AND its keyboard interactivity, neither of which can be changed without
    /// re-minting, so the flag is what says a re-mint is owed; elsewhere it decides the
    /// click-through flip.
    pub armed: bool,
    /// The toolbar rect the input region was cut around, in this surface's POINTS. Linux
    /// only: the toolbar can be DRAGGED mid-recording, and a stale hole would leave the
    /// controls unclickable through the ink. `None` when the surface is not armed.
    #[cfg(target_os = "linux")]
    pub hole: Option<crate::ink::Rect>,
    /// macOS/Windows: whether the window is currently click-through. Tracked so the 60ms
    /// hover poll only issues a flip when the answer actually changes.
    #[cfg(not(target_os = "linux"))]
    pub passthrough: bool,
}

/// Everything the live annotation feature owns.
#[derive(Default)]
pub(super) struct InkState {
    /// The selected tool. [`crate::ink::InkTool::Pointer`] is the default and the ordinary
    /// recording state; the surface may still exist, display-only, while marker ink the user
    /// has not erased is on screen.
    pub tool: crate::ink::InkTool,
    /// The surface, once minted.
    pub surface: Option<InkSurface>,
    /// THE scene, in MEDIA time. The overlay renders `ink::visible_at(&log, now)` and a
    /// window-mode bake renders the same call per frame, so the two cannot drift.
    ///
    /// APPEND-ONLY for the session; see this module's doc and `crate::ink::InkLog`'s.
    pub log: crate::ink::InkLog,
    /// The stroke being drawn, if any.
    pub live: Option<LiveStroke>,
    /// Where the pointer is on the ink surface, in the same POINTS the strokes are recorded
    /// in, or `None` when it has left (DRAGON-739).
    ///
    /// NOT part of [`Self::log`], and that is deliberate rather than incidental. The log is
    /// the SCENE, append-only for the session and replayed by both compositors; the pointer is
    /// a fact about right now that is never replayed and never baked. Keeping it here is also
    /// what lets `publish_live_cursor` write it into its own slot without republishing the
    /// scene, which is what stops a moving cursor throwing the worker's raster cache away on
    /// every frame.
    pub pointer: Option<(f32, f32)>,
    /// macOS/Windows WINDOW mode: where the recorded window was, sampled over media time.
    ///
    /// Those platforms capture the window's OWN buffer, so ink drawn in screen coordinates
    /// has to be mapped into that buffer at bake time, and the window can move and resize
    /// while it is being recorded. Empty on every other path, including Linux window mode,
    /// which records a fixed screen rectangle and therefore captures the ink live.
    pub window_rects: Vec<(crate::ink::MediaMs, crate::platform::compositor::WinRect)>,
    /// How many times the poll ASKED the platform where the recorded window was and got no
    /// answer (DRAGON-691 follow-up).
    ///
    /// Diagnostic only, and it earns its place: an empty `window_rects` has two very different
    /// causes, "this session never had a window id to ask about" and "we asked N times and the
    /// platform never answered", and the shipped bug was the second, reported as one line that
    /// could not tell them apart. `0` with ink drawn now means the question was never put.
    pub window_rect_misses: u32,
    /// This session's support tier (DRAGON-693), decided ONCE at recording start and read
    /// everywhere the three behaviours differ. `Gold` until a recording begins, which is the
    /// state in which no tier rule applies at all.
    pub tier: crate::ink::InkTier,
    /// Silver / Bronze: the recorded output's geometry, snapshotted at recording START.
    ///
    /// It has to be snapshotted, and that is the whole reason this field exists rather than a
    /// lookup. A no-layer-shell session tears every surface down when recording begins and
    /// CLEARS `App::outputs` with them (`recreate_active_overlays`), so by the time the user
    /// arms a tool there is no output list left to ask. The same defensive move
    /// `snapshot_preview_anchor` makes a few lines earlier, for the same reason.
    pub armed_output: Option<ArmedOutput>,
    /// Bronze: the frozen frame the armed window draws on, once the worker has served one.
    ///
    /// `None` before the first snapshot lands, and for the whole session on a zero-copy
    /// recording, whose frames are GPU buffers the CPU never sees. The armed window then draws
    /// a plain scrim instead: the ink still works and still lands in the file, because the
    /// strokes are in STREAM coordinates either way, and only the backdrop is missing.
    pub snapshot: Option<InkSnapshot>,
}

/// The recorded output's geometry, kept past the surfaces it came from (DRAGON-693).
pub(super) struct ArmedOutput {
    /// The output's name, for logs and for a re-home the fallback path cannot do anyway. Read
    /// only by the Linux armed-window mint, so it is honestly dead off Linux, where the tier is
    /// always Gold and no armed output is ever snapshotted at all.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub name: String,
    /// Its top-left in CAPTURE space.
    pub logical_pos: (i32, i32),
    /// Its size in CAPTURE space.
    pub logical_size: (u32, u32),
    /// Capture units per POINT for it.
    pub point_scale: f32,
}

/// Bronze's frozen frame: what the recording last saw, and how big it is (DRAGON-693).
pub(super) struct InkSnapshot {
    /// The frame's size in STREAM pixels. This IS the space the strokes are recorded in, and
    /// the space the bake's identity mapping is expressed over.
    pub size: (u32, u32),
    /// The decoded frame, ready for `widget::image`.
    pub image: cosmic::widget::image::Handle,
}

impl InkState {
    /// The id of the ink surface, if one exists.
    pub fn window(&self) -> Option<window::Id> {
        self.surface.as_ref().map(|s| s.id)
    }
}

impl App {
    /// Are the live-annotation tools offered right now? The pure rule is
    /// [`crate::ink::tools_available`]; this only collects its fact.
    ///
    /// ONE fact since DRAGON-693. The tools used to be withheld from a hidden toolbar (no door
    /// to open them by) and from the portal fallback (no surface to draw on); the tray's Live
    /// Annotations submenu is the first door and [`crate::ink::InkTier`] is the second answer,
    /// so neither is a reason to refuse any more.
    pub(super) fn ink_available(&self) -> bool {
        crate::ink::tools_available(self.recording.is_some())
    }

    /// This session's support tier (DRAGON-693). Read from the state where a recording has
    /// decided one, and derived live otherwise so a caller before `begin_ink_session` still
    /// gets the honest answer rather than a stale default.
    pub(super) fn ink_tier(&self) -> crate::ink::InkTier {
        if self.recording.is_some() {
            self.ink.tier
        } else {
            crate::ink::tier(
                self.overlay_fallback_active(),
                self.mode == Mode::Window,
                self.portal_window_unlocated(),
            )
        }
    }

    /// Is this a PORTAL window cast whose recorded window we could not place on screen
    /// (DRAGON-738)?
    ///
    /// The session half of [`crate::ink::tier`]'s third fact. Two terms, and both are needed:
    ///
    /// * [`Self::portal_window_cast`], because a NATIVE Linux window recording crops a screen
    ///   rectangle, so its ink is captured live and no origin was ever wanted. Reporting it
    ///   unlocated would downgrade a session that is working perfectly.
    /// * the DRAGON-735 rect being absent, which is the whole question: the portal names a
    ///   PipeWire node and never a window, so that rect is the only route to an origin, and
    ///   `portal::toplevel_for_grant_size` plus DRAGON-737's pixel tie-break are the only
    ///   things that can fill it. On KDE and wlroots nothing can, because
    ///   `zcosmic_toplevel_info_v1` is the only protocol carrying a toplevel's geometry.
    ///
    /// `false` off Linux, and not merely by construction: there is no portal capture path on
    /// macOS or Windows, and `platform::window_rect` answers there from the window server for
    /// the whole session, so no window recording of theirs is ever unplaceable.
    pub(super) fn portal_window_unlocated(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.portal_window_cast() && self.portal_cast_window_rect().is_none()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    /// Decide and record this session's tier, at recording START (DRAGON-693).
    ///
    /// Called from `start_recording` BEFORE `recreate_active_overlays`, which on the fallback
    /// path takes every surface (and `App::outputs`) with it. Two things are therefore
    /// captured here and nowhere else: the tier, and, for the tiers that mint their own armed
    /// window, the recorded output's geometry.
    pub(super) fn begin_ink_session(&mut self) {
        let unlocated = self.portal_window_unlocated();
        let tier = crate::ink::tier(
            self.overlay_fallback_active(),
            self.mode == Mode::Window,
            unlocated,
        );
        self.ink.tier = tier;
        self.ink.armed_output = tier.arms_own_window().then(|| self.ink_output()).flatten().map(|o| {
            ArmedOutput {
                name: o.name.clone(),
                logical_pos: o.logical_pos,
                logical_size: o.logical_size,
                point_scale: o.point_scale,
            }
        });
        // DRAGON-738: say WHY, not just what. A session downgraded because the compositor
        // will not name the recorded window looks identical in the log to one downgraded for
        // want of a layer shell, and the two need different answers from whoever reads it.
        log::debug!(
            "ink: this session is {} ({} armed output geometry); layer shell {}, {} mode, \
             recorded window {}",
            tier.word(),
            if self.ink.armed_output.is_some() { "with" } else { "without" },
            if self.overlay_fallback_active() { "absent" } else { "present" },
            if self.mode == Mode::Window { "window" } else { "screen" },
            if unlocated {
                "UNPLACEABLE (a portal cast this desktop will not locate), so the ink is drawn \
                 on a frozen frame in stream coordinates"
            } else {
                "placed or not needed"
            }
        );
    }

    /// Does THIS session need its ink composited into the file by us, rather than captured
    /// live along with everything else on the screen?
    ///
    /// Two cases, and they reach the same compositing by different routes:
    ///
    /// * a WINDOW recording whose capture reads the target window's OWN buffer, where the ink
    ///   is on screen and the file is not;
    /// * a BRONZE session (DRAGON-693), where the ink is not on the screen being cast at all:
    ///   it is drawn on a frozen still of the stream, in stream coordinates.
    ///
    /// Everything else has the ink in the file already, because the compositor put it there.
    ///
    /// **The first term used to ask which OPERATING SYSTEM this is, and that was a bug**
    /// (DRAGON-736). It is a question about the BACKEND: macOS and Windows have one
    /// window-capture implementation each, so there the two questions coincide, but Linux has
    /// two and they answer opposite ways. A native screencopy window recording crops the
    /// SCREEN, so our ink surface is already in the frame; a portal WINDOW cast delivers the
    /// toplevel's own buffer, which no floating surface of ours is part of. Both Linux, both
    /// reachable in the same build on the same desktop. See
    /// [`crate::platform::window_capture_excludes_overlays`] for the split, and
    /// [`Self::portal_window_cast`] for the session fact it is fed.
    pub(super) fn ink_needs_bake(&self) -> bool {
        crate::ink::needs_composite(
            self.mode == Mode::Window,
            crate::platform::window_capture_excludes_overlays(self.portal_window_cast()),
            self.ink_tier(),
        )
    }

    /// Is THIS session's WINDOW recording a PORTAL cast rather than a native screencopy crop
    /// (DRAGON-736)?
    ///
    /// The session half of [`crate::platform::window_capture_excludes_overlays`], read where
    /// the backend is known. The pure decision lives over there; this only reports the fact.
    ///
    /// Derived rather than latched, and derived from the SELECTION rather than from
    /// `pw_held`. The held stream is TAKEN by `spawn_record_worker`, which on a countdown runs
    /// seconds before `begin_ink_session`, so by the time anything asks this there is no
    /// stream left to look at. What survives is the placeholder `Selection` the portal path
    /// stands in for a window pick: a 1x1 rectangle at the origin with NO window id, which is
    /// exactly what [`crate::selection::target_unlocated`] already names and already has tests
    /// for (DRAGON-734). A NATIVE window recording always carries the toplevel's id, since the
    /// screencopy worker crops to that window's rect and could not aim without one.
    ///
    /// `false` off Linux, and not merely by construction: there IS no portal capture path on
    /// macOS or Windows, so the platform term is the whole answer there and this one must
    /// never move it.
    pub(super) fn portal_window_cast(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.pending
                .as_ref()
                .is_some_and(|s| crate::selection::target_unlocated(self.mode == Mode::Window, s))
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    /// Where the window a PORTAL cast is recording sits on screen, when the compositor could
    /// name it (DRAGON-735), or `None` on every other session and platform.
    ///
    /// ONE rect for the whole recording, resolved at grant time. See
    /// [`Self::sample_ink_window_rect`] for why it is not re-asked, and for what that costs.
    fn portal_cast_window_rect(&self) -> Option<crate::platform::compositor::WinRect> {
        #[cfg(target_os = "linux")]
        {
            self.portal_window_rect
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    /// Which tool the toolbar shows as selected. [`crate::ink::InkTool::Pointer`] whenever the
    /// tools are not offered at all, so a stale selection can never outlive the session that
    /// offered it.
    pub(super) fn ink_tool(&self) -> crate::ink::InkTool {
        if self.ink_available() {
            self.ink.tool
        } else {
            crate::ink::InkTool::Pointer
        }
    }

    /// Is a DRAWING tool selected? The one question the surface's input state asks.
    pub(super) fn ink_drawing_armed(&self) -> bool {
        self.ink_tool().draws()
    }

    /// The output the ink surface belongs on: the one being recorded.
    ///
    /// A MONITOR recording names its output, so that is looked up directly. A REGION
    /// recording is resolved by the largest overlap ([`crate::ink::best_output`]), which is
    /// the display holding most of the drawn rectangle: where the user is looking, and where
    /// the toolbar anchored itself.
    ///
    /// A PORTAL WINDOW recording has neither (DRAGON-734): the selection standing in for it
    /// carries a 1x1 placeholder rectangle at the origin, so the overlap search below is
    /// really asking "which display contains the point (0, 0)". That answers correctly on a
    /// desktop laid out from the origin and answers NOTHING on one that is not, which would
    /// retire the ink surface altogether. So an unlocated target uses the output the portal
    /// grant was made on instead, which is a fact we hold independently of the placeholder.
    fn ink_output(&self) -> Option<&OutputState> {
        let sel = self.pending.as_ref()?;
        if let Some(name) = sel.output.as_deref()
            && let Some(o) = self.outputs.iter().find(|o| o.name == name)
        {
            return Some(o);
        }
        if crate::selection::target_unlocated(self.mode == Mode::Window, sel)
            && let Some(o) = self.portal_grant_output()
        {
            return Some(o);
        }
        let target = (
            sel.x as f32,
            sel.y as f32,
            sel.width as f32,
            sel.height as f32,
        );
        let rects: Vec<crate::ink::Rect> = self
            .outputs
            .iter()
            .map(|o| {
                (
                    o.logical_pos.0 as f32,
                    o.logical_pos.1 as f32,
                    o.logical_size.0 as f32,
                    o.logical_size.1 as f32,
                )
            })
            .collect();
        crate::ink::best_output(&rects, target).and_then(|i| self.outputs.get(i))
    }

    /// The registered output a PORTAL grant was made on, when this session took that route
    /// (DRAGON-734).
    ///
    /// `portal_origin_output` is Linux-only state, and so is the whole portal, so the other
    /// platforms answer a constant `None` and their `ink_output` ladder is byte-identical to
    /// before. A window grant always resolves it, because COSMIC's portal sends no position
    /// and `on_pipewire_cast_ready` falls back to the largest registered output for exactly
    /// that case: the same synthetic anchor the wallpaper compose stands its crop on.
    fn portal_grant_output(&self) -> Option<&OutputState> {
        #[cfg(target_os = "linux")]
        {
            let name = self.portal_origin_output.as_deref()?;
            self.outputs.iter().find(|o| o.name == name)
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    /// The output the ink surface is CURRENTLY on, by the name recorded at mint time.
    fn ink_surface_output(&self) -> Option<&OutputState> {
        let name = &self.ink.surface.as_ref()?.output;
        self.outputs.iter().find(|o| &o.name == name)
    }

    /// The rectangle ink may be drawn in, in `o`'s POINTS: the part of this output the
    /// recording actually keeps.
    ///
    /// A monitor recording keeps the whole output. A REGION recording keeps the crop, and the
    /// crop is the selection INSET by the selection line width (`layout::inset_region`, the
    /// same function `start_recording` crops with), not the drawn rectangle, whose outline
    /// sits just outside the recorded pixels. Clamping to the drawn rect instead would let a
    /// stroke's edge land in the two points the video does not have.
    fn ink_crop(&self, o: &OutputState) -> crate::ink::Rect {
        self.ink_crop_in(o.units(), o.point_size())
    }

    /// [`App::ink_crop`] over a units bridge and a surface size, rather than over an
    /// `OutputState` (DRAGON-693).
    ///
    /// Split out because the Silver tier has no `OutputState` to pass: its session tore every
    /// one of them down at recording start, and what it has instead is a snapshot of the
    /// recorded output's geometry plus its own armed toplevel's size. Both callers state the
    /// same rule over the same two quantities, which is what keeps a fallback region recording
    /// clamping its ink exactly where a layer-shell one does.
    ///
    /// **The crop question is asked through [`crate::selection::region_crop`] and NOT by
    /// testing for two absent fields** (DRAGON-734). This used to read "no `output` and no
    /// `window_id`, so it must be a region", which the portal's window placeholder also
    /// satisfies: a 1x1 rectangle at the origin standing in for a target the portal will not
    /// name. A portal WINDOW recording therefore clamped every stroke into one point of screen
    /// in the top-left corner, so the marker drew nothing at all, on the owner's box, for the
    /// whole of the first morning that path ever ran. The mode term is what separates the two,
    /// and `selection.rs` carries the reasoning.
    fn ink_crop_in(
        &self,
        units: crate::geometry::OverlayUnits,
        point_size: (f32, f32),
    ) -> crate::ink::Rect {
        let (pw, ph) = point_size;
        let full = (0.0, 0.0, pw, ph);
        let Some(sel) = self.pending.as_ref() else {
            return full;
        };
        if !crate::selection::region_crop(self.mode == Mode::Region, sel) {
            return full;
        }
        let rec = super::layout::inset_region(sel.clone());
        let (x0, y0) = units.to_point((rec.x, rec.y));
        let (x1, y1) = units.to_point((
            rec.x + rec.width as i32,
            rec.y + rec.height as i32,
        ));
        // Intersect with the surface: a region can hang off this output's edge, and a crop
        // rect reaching past the surface would let a stroke be drawn where there is no
        // surface to draw it on.
        let (lx, ly) = (x0.max(0.0), y0.max(0.0));
        let (hx, hy) = (x1.min(pw), y1.min(ph));
        if hx <= lx || hy <= ly {
            return full;
        }
        (lx, ly, hx - lx, hy - ly)
    }

    /// Silver: the units bridge and surface size for the ARMED toplevel (DRAGON-693).
    ///
    /// The toplevel is fullscreen, and Wayland gives a client no say in which monitor a
    /// fullscreen toplevel lands on, so it can be a different size from the monitor the portal
    /// granted. That is the exact case `OverlayUnits::letterbox` exists for, and reusing it
    /// here is what makes a Silver region recording clamp its ink to the same rectangle a
    /// layer-shell one does. Before the first configure there is no window size yet and the
    /// uniform bridge stands in, which is right for the overwhelmingly common case of a
    /// fullscreen window that did land on the granted monitor.
    fn ink_armed_units(&self) -> Option<(crate::geometry::OverlayUnits, (f32, f32))> {
        let armed = self.ink.armed_output.as_ref()?;
        let win = self.ink.surface.as_ref().and_then(|s| s.win_size);
        // `OverlayUnits::letterbox` is Linux-only in a release build (its one production caller
        // is `OutputState::units`, and this is the second), so the uniform bridge stands in off
        // Linux. Nothing reaches this there: the tier is always Gold on macOS and Windows.
        #[cfg(target_os = "linux")]
        if let Some(win) = win {
            let units = crate::geometry::OverlayUnits::letterbox(
                armed.logical_pos,
                armed.logical_size,
                win,
            );
            return Some((units, win));
        }
        #[cfg(not(target_os = "linux"))]
        let _ = win;
        let units = crate::geometry::OverlayUnits::new(armed.logical_pos, armed.point_scale);
        Some((units, units.size_to_point(armed.logical_size)))
    }

    /// Bronze: the STREAM's own pixel size, which is the space its strokes are recorded in.
    ///
    /// The snapshot's size when one has landed, and the recording's reported captured
    /// footprint otherwise. Both name the same frame; the second is what lets Bronze work at
    /// all on a zero-copy session, where a snapshot never arrives but the ink still has to know
    /// what it is being placed against.
    fn ink_stream_size(&self) -> Option<(u32, u32)> {
        if let Some(snap) = self.ink.snapshot.as_ref() {
            return Some(snap.size);
        }
        self.recording
            .as_ref()
            .and_then(|r| r.dims.lock().ok().and_then(|g| *g))
            .filter(|(w, h)| *w > 0 && *h > 0)
    }

    /// Bronze: where the frozen frame sits inside the armed window, and how big that frame is.
    fn ink_snapshot_fit(&self) -> Option<(crate::ink::Rect, (u32, u32))> {
        let size = self.ink_stream_size()?;
        let win = self.ink.surface.as_ref().and_then(|s| s.win_size)?;
        Some((crate::ink::fit_rect(size, win), size))
    }

    /// A pointer sample on the ink surface, in the space this tier RECORDS strokes in.
    ///
    /// The one place the three tiers' coordinate spaces are reconciled, so nothing downstream
    /// (the log, the replay, the bake) has to know which tier it is serving:
    ///
    /// * **Gold** and **Silver** record in the surface's own POINTS, which are screen points,
    ///   because that is what the capture sees. Both clamp to the part of the screen the
    ///   recording keeps.
    /// * **Bronze** records in STREAM pixels, because the capture is not of the screen at all.
    ///   The sample is un-fitted through the snapshot's placement and clamped into the frame.
    ///   That is what makes Bronze need no window position, ever, and what makes its bake an
    ///   identity mapping.
    ///
    /// `None` when the tier cannot say yet (a Bronze session whose first frame has not fixed
    /// the stream size), which drops the sample rather than recording it somewhere invented.
    fn ink_point(&self, x: f32, y: f32, width: f32) -> Option<(f32, f32)> {
        let margin = width * 0.5;
        if self.ink_tier().shows_snapshot() {
            let (fit, size) = self.ink_snapshot_fit()?;
            return Some(crate::ink::to_stream((x, y), fit, size, margin));
        }
        let crop = self.ink_bounds()?;
        Some(crate::ink::clamp_into((x, y), crop, margin))
    }

    /// The stroke width to record with, in the space this tier records in.
    ///
    /// [`crate::ink::WIDTH_PT`] everywhere except Bronze, whose strokes live in stream pixels
    /// while the user is drawing on a FITTED picture of them: a 4pt line drawn over a snapshot
    /// shown at half size has to be recorded 8 stream pixels wide to come back the same
    /// weight. The factor is the fit's own, so a snapshot enlarged to fill a big display
    /// records THINNER ink, which is likewise what it looked like.
    fn ink_width(&self) -> f32 {
        let base = crate::ink::WIDTH_PT;
        if !self.ink_tier().shows_snapshot() {
            return base;
        }
        let Some(((_, _, fw, fh), (sw, sh))) = self.ink_snapshot_fit() else {
            return base;
        };
        if fw <= 0.0 || fh <= 0.0 {
            return base;
        }
        let k = ((sw as f32 / fw) * (sh as f32 / fh)).max(0.0).sqrt();
        if k.is_finite() && k > 0.0 { base * k } else { base }
    }

    /// The rectangle the SMOOTHED curve is clamped into, in the space strokes are recorded in.
    ///
    /// The same rectangle [`App::ink_point`] clamped the samples into, restated for the fitted
    /// curve, which a centripetal spline can bulge a hair outside its own control points.
    fn ink_draw_rect(&self) -> crate::ink::Rect {
        if self.ink_tier().shows_snapshot() {
            let (w, h) = self.ink_stream_size().unwrap_or((1, 1));
            return (0.0, 0.0, w as f32, h as f32);
        }
        self.ink_bounds().unwrap_or((0.0, 0.0, f32::MAX, f32::MAX))
    }

    /// The rectangle ink may be drawn in, in the ARMED SURFACE's own points. Gold reads it off
    /// the output the surface was minted on; Silver off the snapshot it kept.
    fn ink_bounds(&self) -> Option<crate::ink::Rect> {
        if self.ink_tier().arms_own_window() {
            let (units, size) = self.ink_armed_units()?;
            return Some(self.ink_crop_in(units, size));
        }
        let o = self.ink_surface_output()?;
        Some(self.ink_crop(o))
    }

    // ── The scene ────────────────────────────────────────────────────────────────────────

    /// The CURRENT media time in ms, the clock every stroke is stamped against.
    ///
    /// Media time is what the recorded file is measured in, and it is what makes the scene
    /// replayable: a pause contributes none of it, so ink drawn while paused lands at the
    /// frozen instant and its dissolve does not run while the file is standing still.
    ///
    /// The expression is `recording_elapsed_secs`' to the millisecond, and it is the same
    /// quantity the worker's `MediaClock` produces: `recording_started` is media 0
    /// (`RecordHandle::media_zero_at`, DRAGON-725) and the pause accounting mirrors
    /// `MediaClock::pause`/`resume`, both contributing zero.
    ///
    /// It read `RecordHandle::settled_at` until DRAGON-725, and this doc asserted the two
    /// were the same instant. They are not: the worker's opening phase sits between them,
    /// measured at 328 to 480ms on a mac. Every stroke was therefore stamped that far early,
    /// and since `ink_bake::write_frames` paints a stamp onto frame `stamp * fps / 1000`, the
    /// baked mark appeared 10 to 15 frames before the cursor that drew it. The owner's report
    /// was "the marks appeared ahead of the cursor".
    ///
    /// The live composite (`record::ink_live`, DRAGON-724) never depended on that anchor being
    /// right, and it still must not: every quantity it reads is a DIFFERENCE of two values from
    /// THIS clock, so any constant offset in this clock cancels exactly, and it never converts
    /// to the worker's. That is a structural property to keep rather than a workaround now
    /// spent; `ink_live::offset_invariance_tests` pins it. See that module's doc.
    ///
    /// Before the worker anchors its clock there is no media 0 yet, and the answer is `0`:
    /// the warmup is over in a few hundred ms, the file begins there, and ink drawn in that
    /// window belongs at its start rather than nowhere.
    pub(super) fn ink_media_ms(&self) -> crate::ink::MediaMs {
        let Some(started) = self.recording_started else {
            return 0;
        };
        let until = self.recording_paused_at.unwrap_or_else(std::time::Instant::now);
        until
            .saturating_duration_since(started)
            .saturating_sub(self.recording_paused_accum)
            .as_millis()
            .min(u128::from(u64::MAX)) as crate::ink::MediaMs
    }

    /// Move the toolbar's tool selector to `tool`.
    ///
    /// A three-way selector rather than two arm/disarm toggles (the DRAGON-691 amendment), so
    /// selecting the tool that is already selected does NOTHING: a selector has no off
    /// position to toggle into, Pointer IS that position, and re-pressing a radio button is a
    /// no-op everywhere else in the app too. That rule is also what makes all three disarm
    /// routes safe to lean on ([`App::disarm_ink_tool`]: Escape, a right click and bare `V`):
    /// pressing any of them repeatedly cannot cycle anything, and none of them has to check
    /// first whether there is anything to disarm.
    pub(super) fn ink_select(&mut self, tool: crate::ink::InkTool) -> Task<cosmic::Action<Msg>> {
        if !self.ink_available() || self.ink.tool == tool {
            return Task::none();
        }
        // A tool change mid-stroke would append the rest of the drag to a stroke of the other
        // kind, so the stroke ends here. It cannot happen from the toolbar (the press that
        // hits a button is not on the ink surface) and is closed anyway, because "cannot
        // happen" is not a reason to leave a stroke open.
        self.ink.live = None;
        // DRAGON-693: on Silver and Bronze the armed window has to GO when the user asks for
        // the screen back, so the ink cannot simply stay. Hiding it is one frame with ink and
        // the next without, which the recording shows as a glitch, so the disarm records a
        // FINAL SWEEP instead: the same lift the Magic Marker does, at `FINAL_SWEEP_MS`, over
        // everything on screen. Gold records nothing here and its ink stays, which is the
        // whole of what that medal buys.
        //
        // Recorded on the LOG rather than applied, like every other clearing action, so the
        // bake replays the lift instead of finding the ink gone.
        if !tool.draws() && self.ink_tier().sweeps_on_disarm() && self.ink_showing() {
            let at = self.ink_media_ms();
            self.ink.log.wipe(at);
        }
        self.ink.tool = tool;
        // DRAGON-693: the tray's Live Annotations submenu shows the SELECTED tool, so it has
        // to follow every route that changes it, not just its own rows: the toolbar buttons,
        // the armed window's selector, Escape, and the tray itself.
        if let Some(tray) = self.tray.as_ref() {
            tray.set_ink(tool);
        }
        // Bronze draws on the last frame the recording saw, so arming asks for one. Cheap and
        // one-shot: the worker answers the next time round its loop and nothing waits.
        if tool.draws() && self.ink_tier().shows_snapshot() {
            self.request_ink_snapshot();
        }
        // DRAGON-694: arming or disarming is also what pauses and resumes other media
        // players, when the user has opted into that. See `app::ink_duck`.
        self.sync_ink_duck();
        self.sync_ink_surface()
    }

    /// Put the tool selector back on Pointer: THE disarm (DRAGON-742).
    ///
    /// One method rather than the same line written at each route, because there are three of
    /// them now and they have to end in the SAME state, not in three states that look alike:
    ///
    /// * **Escape**, since DRAGON-691's amendment (`WindowChromeMsg::Close`).
    /// * **A right click** on the recording's own surfaces
    ///   (`app::update::window_chrome::ink_disarm_fires`).
    /// * **Bare `V`** (`shortcuts::is_ink_disarm_key`), the key the preview editor already
    ///   spends on its own Pointer tool.
    ///
    /// The owner asked for the second and third as "the same way escape does", and routing all
    /// three through [`InkMsg::SelectTool`] is what makes that literally true rather than
    /// nearly true. It matters most for the things a disarm does BESIDE moving the selector,
    /// which are easy to forget and invisible when forgotten: the Silver / Bronze final sweep,
    /// the tray's Live Annotations submenu, the DRAGON-694 media un-duck, the surface sync,
    /// and DRAGON-739's PAINTED CURSOR, which `App::update_ink` drops to `None` after every ink
    /// message and which a route that only moved `ink.tool` would leave switched on, drawing a
    /// pointer into the recorded file for the rest of the session.
    ///
    /// Already on Pointer is a no-op: [`App::ink_select`] refuses a change to the tool that is
    /// already selected, which is what makes all three of these safe to lean on.
    pub(in crate::app) fn disarm_ink_tool(&mut self) -> Task<cosmic::Action<Msg>> {
        self.update_ink(InkMsg::SelectTool(crate::ink::InkTool::Pointer))
    }

    /// Bronze: ask the recording worker for the freshest captured frame (DRAGON-693).
    ///
    /// Fire and forget. A worker that never answers (the zero-copy path, whose frames are GPU
    /// buffers the CPU never touches) costs the armed window its backdrop and costs the
    /// recording nothing.
    fn request_ink_snapshot(&mut self) {
        if let Some(rec) = self.recording.as_ref() {
            rec.peek.request();
        }
    }

    /// Bronze: take a served snapshot, if one has arrived. Called from the recording poll.
    ///
    /// Non-blocking, and it decodes ONCE: the handle it builds is what the view hands
    /// `widget::image` on every frame after it.
    pub(super) fn poll_ink_snapshot(&mut self) {
        if !self.ink_tier().shows_snapshot() {
            return;
        }
        let Some(rec) = self.recording.as_ref() else { return };
        let Some((w, h, rgba)) = rec.peek.take() else { return };
        if w == 0 || h == 0 || rgba.len() < (w as usize) * (h as usize) * 4 {
            log::warn!("ink: the recording served a {w}x{h} snapshot that is not a frame");
            return;
        }
        log::debug!("ink: the armed window has a {w}x{h} snapshot of the stream");
        self.ink.snapshot = Some(InkSnapshot {
            size: (w, h),
            image: cosmic::widget::image::Handle::from_rgba(w, h, rgba),
        });
    }

    /// Clear every mark at once, both kinds. The "Clear Marks" action (the toolbar button and
    /// the tray row; it was called Erase until DRAGON-693 renamed it in every user-visible
    /// string, because what it clears is marks and the word had to match the tray's).
    ///
    /// The clips are NOT dropped: the clear is RECORDED, at its media instant, and the replay
    /// applies it. A window-mode bake has to know that the screen was cleared at 12.4 seconds,
    /// and it cannot know that from an empty list.
    ///
    /// It stays INSTANT on every tier, where a Silver / Bronze disarm lifts the ink over
    /// `FINAL_SWEEP_MS`. The two are different requests: a disarm means "give me the screen
    /// back" and the sweep is what stops the video showing a jump cut, while this one means
    /// "get rid of that", where a ramp would just be a slower answer to a question the user has
    /// already made up their mind about.
    pub(super) fn ink_erase(&mut self) -> Task<cosmic::Action<Msg>> {
        if !self.ink_showing() {
            return Task::none();
        }
        let at = self.ink_media_ms();
        self.ink.live = None;
        self.ink.log.erase(at);
        self.sync_ink_surface()
    }

    // There used to be a `retire_spent_ink` here, and it is worth saying why it cannot come
    // back (DRAGON-691 follow-up). It DROPPED clips the live scene could no longer show: a
    // magic generation past its sweep, and anything an Erase had cleared. Its justification was
    // that "gone at `now`" means gone at every later `now`, which is true of the overlay, which
    // only ever asks about the present, and false of the window-mode BAKE, which re-reads this
    // same log after the session and replays it from media 0. It cost the owner a whole
    // drawing: the middle of three, dissolved while the third was being drawn, deleted by the
    // fade tick that finished its sweep, and absent from the finished video at every instant
    // including the seconds it had been on screen.
    //
    // The live renderer already skips ink it cannot see, per frame, for free: an invisible clip
    // costs `visible_at` one comparison. So there is nothing to reclaim and a session's whole
    // history to lose. See `crate::ink::InkLog`'s own doc.

    /// Begin a stroke at a surface-LOCAL point.
    pub(super) fn ink_stroke_begin(&mut self, x: f32, y: f32) -> Task<cosmic::Action<Msg>> {
        let tool = self.ink_tool();
        if !tool.draws() {
            return Task::none();
        }
        // The nominal width is in the surface's own points; Bronze records in stream pixels,
        // where a point is worth `stream / fitted` of one, so the stroke has to be as wide in
        // its own space as it looked in the window. Without this a snapshot shown at half size
        // would record ink at half weight.
        let width = self.ink_width();
        let Some(p) = self.ink_point(x, y, width) else {
            return Task::none();
        };
        let now_ms = self.ink_media_ms();
        // The generation rule lives with the log, so one pure test can drive a whole session
        // through the code this runs (`crate::ink::InkLog::begin_clip`).
        let clip = self.ink.log.begin_clip(tool, p, width, now_ms);
        self.ink.live = Some(LiveStroke {
            raw: vec![p],
            raw_ms: vec![0],
            clip,
            start_ms: now_ms,
        });
        Task::none()
    }

    /// Extend the stroke in flight to a surface-LOCAL point.
    ///
    /// The trail is thinned at [`crate::app::preview::annotate::PEN_MIN_STEP`] and the
    /// beautified stroke is RE-FITTED on every kept sample, exactly like the preview editor's
    /// pencil: the smoothing pipeline is causal and linear, so this costs microseconds and
    /// the settled ink never moves. What you watch being drawn is what stays on screen, and
    /// what a bake replays.
    pub(super) fn ink_stroke_move(&mut self, x: f32, y: f32) -> Task<cosmic::Action<Msg>> {
        let width = self.ink.live.as_ref().and_then(|l| self.ink.log.clips.get(l.clip)).map(|c| c.width);
        let Some(width) = width else {
            return Task::none();
        };
        let Some(p) = self.ink_point(x, y, width) else {
            return Task::none();
        };
        // The fitted clamp is the tier's own; the FITTED curve below is clamped against the
        // same rectangle, which for Bronze is the stream frame and for the rest the crop.
        let crop = self.ink_draw_rect();
        let now_ms = self.ink_media_ms();
        let Some(live) = self.ink.live.as_mut() else {
            return Task::none();
        };
        let Some(clip) = self.ink.log.clips.get_mut(live.clip) else {
            return Task::none();
        };
        let far = live.raw.last().is_none_or(|l| {
            (p.0 - l.0).hypot(p.1 - l.1) >= crate::app::preview::annotate::PEN_MIN_STEP
        });
        if !far {
            return Task::none();
        }
        // The cap is `pen_stroke`'s own point budget: a stroke long enough to reach it stops
        // recording new RAW samples rather than being truncated, so nothing already drawn is
        // ever lost. In practice this is a scribble tens of thousands of points long.
        if live.raw.len() >= crate::pen_stroke::MAX_POINTS {
            return Task::none();
        }
        // MEDIA offset from the stroke's start, so a sample taken while the recording is
        // paused carries the frozen offset rather than the wall time nobody recorded.
        let offset = (now_ms - live.start_ms).clamp(0, u32::MAX as crate::ink::MediaMs) as u32;
        live.raw.push(p);
        live.raw_ms.push(offset);
        let fit = crate::pen_stroke::smooth_path(&live.raw, clip.width);
        // Clamp the fitted curve too: a centripetal spline can bulge a hair outside its own
        // control points, and the crop is the recorded boundary rather than a suggestion.
        let fit: Vec<(f32, f32)> = fit
            .into_iter()
            .map(|q| crate::ink::clamp_into(q, crop, clip.width * 0.5))
            .collect();
        clip.speed = crate::pen_stroke::pressure_along(&live.raw, &fit);
        clip.point_ms = crate::ink::ages_along(&live.raw, &live.raw_ms, &fit)
            .into_iter()
            .map(|off| live.start_ms + off as crate::ink::MediaMs)
            .collect();
        clip.points = fit;
        // Every Magic Marker sample keeps its own generation's hold open, which is the
        // amendment's "drawing anything holds everything" scoped to the batch it belongs to.
        // Marker ink has no generation and this does nothing for it.
        let i = live.clip;
        self.ink.log.touch_clip_generation(i, now_ms);
        Task::none()
    }

    /// End the stroke in flight. Nothing is re-shaped: the live fit IS the finished stroke.
    ///
    /// The RAW trail goes here, and that is the point at which a stroke stops being an input
    /// and becomes a record: nothing re-fits it again, and the clip's own points, speed and
    /// media stamps are what the overlay and the bake both read from here on.
    pub(super) fn ink_stroke_end(&mut self) -> Task<cosmic::Action<Msg>> {
        self.ink.live = None;
        Task::none()
    }

    /// One frame of the Magic Marker's dissolve.
    ///
    /// The tick does two things and neither is the animation: it produces the FRAME (any
    /// message rebuilds the view, and the alpha is recomputed there against the media clock),
    /// and it lets the surface go on the frame the last of the ink does.
    ///
    /// It touches the scene NOT AT ALL. It used to prune it, which is the bug this ticket
    /// closes; see the note where `retire_spent_ink` was. `sync_ink_surface` is the one place
    /// that decides, it is idempotent, and it answers `Task::none()` on every tick where
    /// nothing has changed, so calling it unconditionally costs a comparison.
    pub(super) fn ink_fade_tick(&mut self) -> Task<cosmic::Action<Msg>> {
        self.sync_ink_surface()
    }

    /// Is a dissolve still running? The one condition `sub_ink_fade` ticks on.
    ///
    /// True while any magic ink is still on screen, hold included: the pure rule is
    /// [`crate::ink::magic_visible_at`], and its doc says why the tick has to be armed for the
    /// whole clock rather than only for the sweep.
    pub(super) fn ink_fading(&self) -> bool {
        crate::ink::fading_at(&self.ink.log, self.ink_media_ms())
    }

    /// Is there anything on screen right now? Drives the surface's lifetime together with the
    /// selected tool: the surface outlives a switch back to Pointer while marker ink remains,
    /// and goes when both are gone.
    ///
    /// A question about the media INSTANT, not about the log's length: the log keeps every
    /// stroke of the session so a window-mode bake can replay it, so "anything was ever drawn"
    /// and "anything is showing" are different questions and only the second decides a surface.
    fn ink_showing(&self) -> bool {
        crate::ink::any_visible_at(&self.ink.log, self.ink_media_ms())
    }

    /// Is a dissolve actually MOVING, as opposed to merely counting down to one?
    ///
    /// The tick has to be armed for the whole clock (nothing else would produce the frame that
    /// starts a sweep, since a subscription is only re-evaluated when a message arrives), but
    /// it only has to be FAST while something is changing. During a hold every frame draws the
    /// identical scene, and each one rebuilds every stroke's ribbon, on a machine that is
    /// already encoding video. This is what lets `sub_ink_fade` idle at a coarse cadence while
    /// nothing moves and go to one frame's cadence when something does.
    ///
    /// The threshold is one coarse tick EARLY, so a sweep is running at the fine cadence from
    /// its first frame rather than starting on a stale one.
    pub(super) fn ink_sweeping(&self) -> bool {
        let now_ms = self.ink_media_ms();
        let lead = super::subscriptions::INK_HOLD_TICK_MS as crate::ink::MediaMs;
        let magic = self.ink.log.gens.iter().any(|g| {
            let sweep_at = g.last_activity_ms + crate::ink::MAGIC_HOLD_MS as crate::ink::MediaMs;
            let gone_at = g.last_activity_ms + crate::ink::MAGIC_GONE_MS as crate::ink::MediaMs;
            now_ms + lead >= sweep_at && now_ms < gone_at
        });
        // DRAGON-693: a FINAL sweep is all motion and no hold, so it is fine-cadence from the
        // instant it is recorded to the instant it ends. It is also the shortest thing the
        // overlay animates, and the one the video is guaranteed to be recording.
        let final_sweep = self.ink.log.wipes.iter().any(|&w| {
            now_ms >= w && now_ms < w + crate::ink::FINAL_SWEEP_MS as crate::ink::MediaMs
        });
        magic || final_sweep
    }

    // ── Drawing ──────────────────────────────────────────────────────────────────────────

    /// The live overlay's fills: [`crate::ink::fill_runs_at`] at the current media time, with
    /// the session's one ink colour applied to each run's opacity.
    ///
    /// The identity map and a scale of 1: the overlay draws in the ink surface's own points,
    /// which is the space the strokes were recorded in. A window-mode bake calls the same
    /// function with a screen-to-video map instead, which is the whole of the difference
    /// between what the user watched and what lands in the file.
    fn ink_fills(&self) -> Vec<crate::widgets::ink_canvas::InkFill> {
        let base = self.ink_color();
        // Gold and Silver draw in the surface's own points, which is the space the strokes were
        // recorded in, so the map is the identity. BRONZE recorded in stream pixels, so it maps
        // back out through the same fit the snapshot is drawn at: one expression, used forwards
        // by `ink_point` and backwards here, which is what keeps the ink under the pointer.
        let bronze = self.ink_tier().shows_snapshot().then(|| self.ink_snapshot_fit()).flatten();
        let runs = match bronze {
            Some(((fx, fy, fw, fh), (sw, sh))) if sw > 0 && sh > 0 => {
                let (kx, ky) = (fw / sw as f32, fh / sh as f32);
                let scale = (kx * ky).max(0.0).sqrt();
                crate::ink::fill_runs_at(
                    &self.ink.log,
                    self.ink_media_ms(),
                    |p| (fx + p.0 * kx, fy + p.1 * ky),
                    scale,
                )
            }
            _ => crate::ink::fill_runs_at(&self.ink.log, self.ink_media_ms(), |p| p, 1.0),
        };
        runs.into_iter()
            .map(|(alpha, polys)| {
                (cosmic::iced::Color { a: base.a * alpha, ..base }, polys)
            })
            .collect()
    }

    /// The ink's colour: the user's chosen annotation colour, or the preview editor's default
    /// (the accent's companion). ONE colour, no picker in v1; see `crate::ink::WIDTH_PT` for
    /// the same reasoning about the in-frame toolbar.
    pub(super) fn ink_color(&self) -> cosmic::iced::Color {
        let c = self
            .annot_color
            .unwrap_or_else(crate::app::preview::annotate::default_annot_color);
        cosmic::iced::Color::from_rgba(
            c[0] as f32 / 255.0,
            c[1] as f32 / 255.0,
            c[2] as f32 / 255.0,
            (c[3] as f32 / 255.0).max(0.0),
        )
    }

    /// The ink surface's view: the canvas, and nothing else. No dim, no chrome, no hint. The
    /// surface is on screen for the whole of a live recording and every pixel it paints is in
    /// the video.
    pub(super) fn ink_view(&self) -> Element<'_, Msg> {
        let armed = self.ink_drawing_armed();
        let fills = self.ink_fills();
        let ink = crate::widgets::ink_canvas::InkCanvas::new(
            fills,
            |p| Msg::Ink(InkMsg::StrokeBegin(p.0, p.1)),
            |p| Msg::Ink(InkMsg::StrokeMove(p.0, p.1)),
            || Msg::Ink(InkMsg::StrokeEnd),
        )
        .armed(armed);
        // DRAGON-739: the hover report is asked for ONLY where a cursor might actually have to
        // be drawn. Every other session (a region or monitor recording, a screen-rectangle
        // window recording, a platform that composites the pointer geometrically, a user who
        // asked for no pointer at all) reports no hover, so its ink surface publishes exactly
        // the messages it always did and redraws exactly as often.
        let ink = if self.live_cursor_wanted() {
            ink.on_hover(|at| Msg::Ink(InkMsg::PointerAt(at)))
        } else {
            ink
        };
        let canvas: Element<'_, Msg> = ink.into();
        if !self.ink.surface.as_ref().is_some_and(|s| s.plain) {
            return canvas;
        }
        self.ink_armed_view(canvas)
    }

    /// Silver / Bronze: the ARMED window's whole view (DRAGON-693).
    ///
    /// Three layers, bottom to top:
    ///
    /// 1. the BACKDROP. Bronze shows the frozen stream frame, fitted and centred, which is what
    ///    makes "annotate on a snapshot" literal. Silver shows a barely-there scrim instead of
    ///    nothing at all, because a fully transparent focused window that swallows every click
    ///    is indistinguishable from a frozen desktop; the scrim is what says the mode is on.
    ///    It is IN the recording, deliberately and unavoidably, and it is kept faint for that
    ///    reason.
    /// 2. the ink canvas, exactly the one Gold draws.
    /// 3. the SELECTOR, because this window is focused and opaque to input, so the recording
    ///    toolbar underneath it (if this session even had one) is unreachable. The tray arms
    ///    the first tool; from there the selector on the window is the door back out. On
    ///    BRONZE only, one line under it names this session's mode and why it got it
    ///    (`ink::armed_note`), because that downgrade is otherwise invisible and Bronze is
    ///    the one tier whose armed window is not itself in the recording.
    fn ink_armed_view<'a>(&'a self, canvas: Element<'a, Msg>) -> Element<'a, Msg> {
        use cosmic::iced::{Alignment, Length};
        use cosmic::widget;
        let backdrop: Element<'a, Msg> = match self.ink.snapshot.as_ref() {
            Some(snap) => widget::container(
                widget::image(snap.image.clone())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .content_fit(cosmic::iced::ContentFit::Contain),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .class(cosmic::theme::Container::custom(ink_scrim))
            .into(),
            None => widget::container(widget::space::Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
                .class(cosmic::theme::Container::custom(ink_scrim))
                .into(),
        };
        // DRAGON-738: a BRONZE armed window names its mode and its cause. A frozen picture
        // where the user expected their live desktop reads as a hang unless something on
        // screen says otherwise, and the debug log reaches nobody who is recording. The rule
        // is pure (`ink::armed_note`) and it answers `None` for Silver on purpose: THAT
        // window is in the cast, so a caption would be burned into the user's recording.
        // Bronze's is not, because it casts one window's own buffer.
        let mut chrome = widget::column::with_capacity(2)
            .push(self.ink_selector())
            .spacing(8.0)
            .align_x(Alignment::Center);
        if let Some(note) =
            crate::ink::armed_note(self.ink_tier(), self.portal_window_unlocated())
        {
            chrome = chrome.push(
                widget::container(widget::text::caption(note))
                    .padding([4.0, 10.0])
                    .class(cosmic::theme::Container::Card),
            );
        }
        let selector: Element<'a, Msg> = widget::container(chrome)
            .width(Length::Fill)
            .align_x(Alignment::Center)
            .padding(12.0)
            .into();
        cosmic::iced::widget::stack(vec![backdrop, canvas, selector])
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// The armed window's own tool selector: the SAME three tools and the same Clear Marks the
    /// recording toolbar and the tray offer, so the control reads identically wherever it is.
    fn ink_selector(&self) -> Element<'_, Msg> {
        use cosmic::iced::{Alignment, Length};
        use cosmic::widget;
        let current = self.ink_tool();
        let btn = |name: &'static str, msg: Msg, on: bool| -> Element<'_, Msg> {
            widget::button::custom(
                widget::container(crate::widgets::icons::sized(name, 20.0))
                    .width(Length::Fixed(36.0))
                    .align_x(Alignment::Center),
            )
            .class(if on { cosmic::theme::Button::Suggested } else { cosmic::theme::Button::Icon })
            .on_press(msg)
            .padding(6.0)
            .into()
        };
        let tool = |name: &'static str, t: crate::ink::InkTool| {
            btn(name, Msg::Ink(InkMsg::SelectTool(t)), current == t)
        };
        widget::container(
            widget::row(vec![
                tool("input-mouse-symbolic", crate::ink::InkTool::Pointer),
                tool("pencil-symbolic", crate::ink::InkTool::Marker),
                tool("format-text-highlight-symbolic", crate::ink::InkTool::MagicMarker),
                btn("eraser-symbolic", Msg::Ink(InkMsg::Erase), false),
            ])
            .spacing(4.0)
            .align_y(Alignment::Center),
        )
        .padding(6.0)
        .class(cosmic::theme::Container::Card)
        .into()
    }

    // ── The surface's lifetime ───────────────────────────────────────────────────────────

    /// Should an ink surface exist at all? A tool is armed, or ink the user has not erased is
    /// still on screen. The second half is what lets marker ink outlive its tool.
    fn ink_wanted(&self) -> bool {
        self.ink_available() && (self.ink.tool.draws() || self.ink_showing())
    }

    /// Bring the surface into line with the scene: mint it, retire it, or re-issue it with a
    /// different input region.
    ///
    /// The ONE place the surface's existence is decided, called after every ink message, so
    /// no handler has to remember which of them can change the answer.
    pub(super) fn sync_ink_surface(&mut self) -> Task<cosmic::Action<Msg>> {
        if !self.ink_wanted() {
            return self.close_ink_surface();
        }
        // DRAGON-693, Silver and Bronze: ONE armed toplevel, minted mid-recording. There is no
        // output to re-home it to (this session's outputs went when the recording started), and
        // nothing about it can be re-issued, so the whole of its lifetime is "exists or does
        // not". Gold's three-way sync below is untouched.
        #[cfg(target_os = "linux")]
        if self.ink.tier.arms_own_window() {
            return if self.ink.surface.is_some() {
                Task::none()
            } else {
                self.mint_ink_armed_window()
            };
        }
        let Some(target) = self.ink_output().map(|o| o.name.clone()) else {
            return self.close_ink_surface();
        };
        match self.ink.surface.as_ref() {
            None => self.mint_ink_surface(&target),
            // The recorded output cannot change mid-recording, so this is a safety net rather
            // than a live path: re-home instead of drawing onto a display that is no longer
            // the one being captured.
            Some(s) if s.output != target => {
                let close = self.close_ink_surface();
                Task::batch([close, self.mint_ink_surface(&target)])
            }
            Some(_) => self.refresh_ink_surface(),
        }
    }

    /// Retire the ink surface, keeping the scene. Called by every ink path that empties the
    /// screen, and by `destroy_surfaces` when the whole session's overlays go.
    pub(super) fn close_ink_surface(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(s) = self.ink.surface.take() else {
            return Task::none();
        };
        // The Bronze snapshot is per-ARMING, not per-session: the next arm asks for a fresh
        // frame, because the recording has moved on. Dropping it here also gives the frame's
        // memory back for the rest of the take.
        self.ink.snapshot = None;
        // A `plain` surface is an ordinary toplevel (DRAGON-693). Closing a toplevel with
        // `destroy_layer_surface` does nothing at all, which would leave a focused full-screen
        // window over the user's desktop for the rest of the recording.
        #[cfg(target_os = "linux")]
        if s.plain {
            return cosmic::iced::window::close(s.id);
        }
        super::shell::close_surface(s.id)
    }

    /// Silver / Bronze: mint the ARMED annotation toplevel (DRAGON-693).
    ///
    /// One ordinary fullscreen transparent toplevel, the same bridge
    /// `shell::overlay_fallback_window` builds for the capture overlay on this path, and for
    /// the same reason: with no `wlr-layer-shell` there is nothing else a client can put on
    /// screen. It is FOCUSED (the compositor focuses a fresh fullscreen window itself), which
    /// is what makes Escape work without a click and what makes the tier "annotate OR control"
    /// rather than both.
    ///
    /// This is a deliberate exception to the teardown `recreate_active_overlays` performs at
    /// recording start. That teardown exists because the SELECTION overlay would otherwise be
    /// baked into the recording and would swallow every click meant for the app being
    /// recorded; both are true of this window too, and both are what the user asked for by
    /// arming a drawing tool. It goes again the moment they ask for the screen back.
    #[cfg(target_os = "linux")]
    fn mint_ink_armed_window(&mut self) -> Task<cosmic::Action<Msg>> {
        let size = self
            .ink
            .armed_output
            .as_ref()
            .map(|o| o.logical_size)
            .unwrap_or((1920, 1080));
        let name = self
            .ink
            .armed_output
            .as_ref()
            .map(|o| o.name.clone())
            .unwrap_or_default();
        let (id, open) = super::shell::ink_armed_window(size);
        self.ink.surface = Some(InkSurface {
            id,
            output: name,
            armed: true,
            hole: None,
            plain: true,
            win_size: None,
        });
        log::debug!("ink: minting the {} armed annotation window", self.ink.tier.word());
        open
    }

    /// The ARMED window reported its size (DRAGON-693). Silver builds its letterbox bridge from
    /// it and Bronze fits the snapshot inside it, so both need it before the first stroke.
    pub(super) fn ink_window_resized(&mut self, id: window::Id, w: f32, h: f32) -> bool {
        let Some(s) = self.ink.surface.as_mut().filter(|s| s.id == id && s.plain) else {
            return false;
        };
        s.win_size = Some((w, h));
        true
    }

    /// The ink surface's input region, in POINTS, while a tool is armed: the whole output
    /// MINUS the toolbar, so the recording controls stay clickable through it.
    ///
    /// `None` (an empty region) when no tool is armed, which is what makes leftover marker
    /// ink display-only instead of a full-screen click trap.
    #[cfg(target_os = "linux")]
    fn ink_input_zone(&self, o: &OutputState) -> (Vec<cosmic::iced::Rectangle>, Option<crate::ink::Rect>) {
        if !self.ink_drawing_armed() {
            return (Vec::new(), None);
        }
        let (pw, ph) = o.point_size();
        let full = (0.0, 0.0, pw, ph);
        let Some((bar, _)) = self.toolbar_layout(o) else {
            return (vec![cosmic::iced::Rectangle { x: 0.0, y: 0.0, width: pw, height: ph }], None);
        };
        let hole = (bar.x, bar.y, bar.width, bar.height);
        let rects = crate::ink::rect_subtract(full, hole)
            .into_iter()
            .map(|(x, y, w, h)| cosmic::iced::Rectangle { x, y, width: w, height: h })
            .collect();
        (rects, Some(hole))
    }

    /// Linux: mint the ink layer surface on the output named `name`.
    #[cfg(target_os = "linux")]
    fn mint_ink_surface(&mut self, name: &str) -> Task<cosmic::Action<Msg>> {
        let Some(o) = self.outputs.iter().find(|o| o.name == name) else {
            return Task::none();
        };
        let (zone, hole) = self.ink_input_zone(o);
        // Read before the surface is stored: `o` borrows `self.outputs`, and the store below
        // takes `self` mutably.
        let (cx, cy, cw, ch) = self.ink_crop(o);
        let handle = o.output.clone();
        let id = window::Id::unique();
        let armed = self.ink_drawing_armed();
        self.ink.surface =
            Some(InkSurface { id, output: name.to_string(), armed, hole, plain: false, win_size: None });
        // DRAGON-734: name the OUTPUT and the drawable rectangle, not just the rect COUNT.
        // The old line said "4 input rects" for a session whose ink was being clamped into
        // one point of screen, so it read healthy while the marker drew nothing. What a
        // reader of this log needs is where the surface went and how much of it ink can
        // reach, and both are one call away here.
        log::debug!(
            "ink: minting the drawing surface on {name} ({} input rects, keyboard {}, \
             drawable {cw:.0}x{ch:.0} at {cx:.0},{cy:.0})",
            zone.len(),
            if armed { "on demand" } else { "off" },
        );
        super::shell::ink_surface(handle, id, Some(zone), armed)
    }

    /// Linux: re-issue the surface when its input region no longer matches the state.
    ///
    /// A `wlr-layer-shell` input region is fixed at creation, so "changing" it is a destroy
    /// and a create. Both go out in ONE batch: `shell.rs`'s own note records that a flip
    /// issued in the same batch as a creation races it, and that trap is why this is a
    /// re-mint rather than an update. Only the ink surface is touched, so the capture
    /// overlays (and the toolbar drawn on them) never flicker for it.
    ///
    /// Three things can change the answer: arming, disarming, and DRAGGING the toolbar, which
    /// moves the hole. The recorded stored `hole` is what makes the third one detectable.
    #[cfg(target_os = "linux")]
    fn refresh_ink_surface(&mut self) -> Task<cosmic::Action<Msg>> {
        // A Silver / Bronze armed toplevel has nothing to re-issue: its input is the whole
        // window and there is no toolbar under it to cut a hole for. Re-minting one would also
        // take its focus away and back mid-stroke.
        if self.ink.surface.as_ref().is_some_and(|s| s.plain) {
            return Task::none();
        }
        let Some(o) = self.ink_surface_output() else {
            return Task::none();
        };
        let armed = self.ink_drawing_armed();
        let (zone, hole) = self.ink_input_zone(o);
        let handle = o.output.clone();
        let Some(s) = self.ink.surface.as_ref() else {
            return Task::none();
        };
        let same = s.armed == armed
            && s.hole.map(|h| [h.0, h.1, h.2, h.3]) == hole.map(|h| [h.0, h.1, h.2, h.3]);
        if same {
            return Task::none();
        }
        let old = s.id;
        let id = window::Id::unique();
        let name = s.output.clone();
        self.ink.surface =
            Some(InkSurface { id, output: name, armed, hole, plain: false, win_size: None });
        Task::batch([
            super::shell::close_surface(old),
            super::shell::ink_surface(handle, id, Some(zone), armed),
        ])
    }

    /// macOS/Windows: mint the ink WINDOW on the output named `name`.
    ///
    /// Opened hidden and titled, then placed natively once the title has landed (the
    /// `SurfaceOpened` follow-up), exactly like the capture overlays. See
    /// `shell::ink_window` for why it may not open visible.
    #[cfg(not(target_os = "linux"))]
    fn mint_ink_surface(&mut self, name: &str) -> Task<cosmic::Action<Msg>> {
        let Some(o) = self.outputs.iter().find(|o| o.name == name) else {
            return Task::none();
        };
        let (pos, size) = (o.logical_pos, o.logical_size);
        let (id, open) = super::shell::ink_window(pos, size);
        self.ink.surface = Some(InkSurface {
            id,
            output: name.to_string(),
            armed: self.ink_drawing_armed(),
            // Minted solid: a tool is armed by the time this runs (nothing else mints it), so
            // the surface has to take the pointer. The hover poll takes over from here.
            passthrough: false,
            // Never a plain toplevel off Linux: those two platforms always have their own
            // window level, so their tier is always Gold.
            plain: false,
            win_size: None,
        });
        log::debug!("ink: minting the drawing window");
        Task::batch([
            open,
            self.set_window_title(super::shell::INK_WINDOW_TITLE.to_string(), id),
        ])
    }

    /// macOS/Windows: the arm state changed, so the window's click-through does too.
    ///
    /// The hover poll refines this every 60ms (the toolbar wins while the pointer is over
    /// it); this is the immediate answer so a disarm does not leave a full-screen click trap
    /// up for a frame.
    #[cfg(not(target_os = "linux"))]
    fn refresh_ink_surface(&mut self) -> Task<cosmic::Action<Msg>> {
        let armed = self.ink_drawing_armed();
        let Some(s) = self.ink.surface.as_mut() else {
            return Task::none();
        };
        if s.armed == armed {
            return Task::none();
        }
        s.armed = armed;
        self.set_ink_passthrough(!armed)
    }

    /// macOS/Windows: flip the ink window's click-through, remembering the state so a poll
    /// that changes nothing issues nothing.
    #[cfg(not(target_os = "linux"))]
    pub(super) fn set_ink_passthrough(&mut self, on: bool) -> Task<cosmic::Action<Msg>> {
        let Some(s) = self.ink.surface.as_mut() else {
            return Task::none();
        };
        if s.passthrough == on {
            return Task::none();
        }
        s.passthrough = on;
        #[cfg(target_os = "macos")]
        {
            let id = s.id;
            if on {
                window::enable_mouse_passthrough(id)
            } else {
                window::disable_mouse_passthrough(id)
            }
        }
        // Windows matches the window by TITLE, like every other native flip there, so the
        // recorded state above is all this arm needs from the surface.
        #[cfg(target_os = "windows")]
        {
            let _ = s;
            crate::platform::windows::window::set_click_through(
                super::shell::INK_WINDOW_TITLE,
                on,
            );
            Task::none()
        }
        // Any other non-Linux target has no native flip to make; the seam stays total.
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = s;
            Task::none()
        }
    }

    /// Linux has no separate ink window to flip; its input region carries the whole answer.
    ///
    /// Honestly DEAD here: the ONE caller is the 60ms hover poll, which is macOS/Windows only
    /// (`App::passthrough_poll`). The arm stays so the seam is total and a reader of either
    /// platform's ink code finds the same function name.
    #[cfg(target_os = "linux")]
    #[allow(dead_code)]
    pub(super) fn set_ink_passthrough(&mut self, _on: bool) -> Task<cosmic::Action<Msg>> {
        Task::none()
    }

    /// macOS/Windows: place the ink window natively once it has opened.
    ///
    /// The same retry-on-title-lag shape as `configure_overlay`, because the title is set by
    /// an async task and both platforms match the window by it. What differs is the LEVEL:
    /// this window must sit UNDER the capture overlays so the toolbar stays visible and
    /// clickable above the ink, which macOS expresses as a window level and Windows as a
    /// re-assert of the overlays' topmost position after this one lands.
    #[cfg(not(target_os = "linux"))]
    pub(super) fn configure_ink_surface(
        &self,
        id: window::Id,
        attempt: u8,
    ) -> Task<cosmic::Action<Msg>> {
        /// The title is applied asynchronously, so the match can lag the open by a frame or
        /// two. 30 x 40ms is the same budget the capture overlays use.
        const MAX_ATTEMPTS: u8 = 30;
        const RETRY_MS: u64 = 40;
        let Some(s) = self.ink.surface.as_ref().filter(|s| s.id == id) else {
            return Task::none();
        };
        let Some(o) = self.outputs.iter().find(|o| o.name == s.output) else {
            return Task::none();
        };
        let placed = self.place_ink_surface(o);
        if placed {
            return Task::none();
        }
        if attempt >= MAX_ATTEMPTS {
            // Not fatal, and deliberately not a `diag::Failure`: the recording is unaffected
            // and every other control still works. What is lost is the drawing, so it is a
            // loud warn rather than a silent nothing.
            log::warn!(
                "ink: the drawing surface never matched its native window after \
                 {MAX_ATTEMPTS} attempts, so drawing may not appear"
            );
            return Task::none();
        }
        Task::perform(
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(RETRY_MS)).await;
            },
            move |()| cosmic::Action::App(Msg::Ink(InkMsg::SurfaceOpened(id, attempt + 1))),
        )
    }

    /// macOS/Windows: ONE native placement attempt. `true` once the window was found and
    /// placed.
    #[cfg(not(target_os = "linux"))]
    fn place_ink_surface(&self, o: &OutputState) -> bool {
        #[cfg(target_os = "macos")]
        {
            crate::platform::mac::window::place_ink_overlay(
                super::shell::INK_WINDOW_TITLE,
                o.logical_pos,
                o.logical_size,
            )
        }
        #[cfg(target_os = "windows")]
        {
            let placed = crate::platform::windows::window::place_overlay(
                super::shell::INK_WINDOW_TITLE,
                o.logical_pos,
                o.logical_size,
                // A transient drawing surface shows no taskbar button, like the capture
                // selector.
                false,
                // And never takes focus: it is minted in the middle of a recording, and
                // activating this process there pulls focus out of the application being
                // recorded.
                false,
            );
            if placed {
                // Windows has ONE topmost band with no sub-levels, so the ink window landed
                // above the capture overlays simply by being placed later. Put them back on
                // top, or the toolbar is buried under the surface the user is drawing on.
                // Cheap, z-order only, and idempotent.
                for c in &self.outputs {
                    crate::platform::windows::window::reassert_topmost(&c.name);
                }
            }
            placed
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = o;
            false
        }
    }
}

// ── The window-mode bake: recording where the window was, and handing it over ─────────────

impl App {
    /// Sample the recorded window's on-screen rect, once, into the media-time timeline.
    ///
    /// Called from the recording poll (100ms) and only for a session that needs the bake, so
    /// an ordinary region or monitor recording does no platform query at all.
    ///
    /// A sample is kept only when the rect CHANGED, which for a window nobody touches is one
    /// entry for the whole recording. The timeline is therefore proportional to how much the
    /// user moved the window, not to how long they recorded.
    ///
    /// `true` when a sample was actually added. DRAGON-724's live composite reads that: its
    /// raster is cached per PUBLISHED scene, so republishing on every poll of a still drawing
    /// would throw the cache away ten times a second for a scene that had not changed.
    ///
    /// 100ms is coarse for a window being dragged, and deliberately so: this rides a poll that
    /// already exists rather than adding a timer to a live recording, and the failure it
    /// produces is ink lagging a drag by up to a tenth of a second, in the one situation
    /// (dragging the window you are recording while drawing on it) that is already unusual.
    pub(super) fn sample_ink_window_rect(&mut self) -> bool {
        // Bronze needs no rect at all: its strokes are already in the stream's own space, which
        // is the whole point of recording them there. Asking the platform where a window is on
        // a session that has no window id would only count misses.
        if self.ink_tier().shows_snapshot() {
            return false;
        }
        // Nothing drawn yet means nothing to map. The first stroke starts the timeline, and
        // `InkBake::rect_at` holds the first sample backwards for ink drawn in the moments
        // before this poll caught up.
        //
        // DRAGON-739 added the second half: an ARMED tool needs the timeline too, before any
        // stroke exists, because the pointer we now draw ourselves is placed through the very
        // same screen-to-video mapping the ink is. Arming is also exactly when the compositor
        // stops drawing the cursor, so waiting for the first stroke would leave the user with
        // no cursor for as long as they were deciding where to put one.
        if !self.ink_needs_bake() || (self.ink.log.clips.is_empty() && !self.ink_drawing_armed()) {
            return false;
        }
        // TWO routes to "where is the recorded window", and which one applies is a property of
        // the BACKEND rather than of the platform (DRAGON-736).
        //
        // * A target we hold an ID for (a macOS `CGWindowID`, a Windows `HWND`, a native Linux
        //   toplevel identifier) is asked afresh on every poll, so the timeline follows a
        //   window that is moved or resized while it is being recorded.
        // * A PORTAL window cast has no id of any kind: the xdg ScreenCast interface names a
        //   PipeWire node and never a window. Its rect is resolved ONCE, at grant time, by
        //   matching the granted stream's size against the toplevel list
        //   (`portal::toplevel_for_grant_size`), so its timeline is a single sample and ink
        //   drawn after the user MOVED the window is placed by where the window started. When
        //   several toplevels shared that size, DRAGON-737's pixel tie-break is what filled it
        //   at all; a session it could not settle has no rect and is Bronze instead.
        //   That ceiling is deliberate rather than an oversight: re-resolving means
        //   `compositor::list_toplevels`, a blocking Wayland settle loop measured at 167ms on
        //   the dev box, which cannot ride a 100ms poll during a live recording. It costs the
        //   video nothing, because a window cast delivers the toplevel's own buffer wherever
        //   the window is; only the screen-to-video mapping goes stale.
        let asked = match self.pending.as_ref().and_then(|s| s.window_id.clone()) {
            Some(id) => crate::platform::window_rect(&id),
            None if self.portal_window_cast() => self.portal_cast_window_rect(),
            // Nothing to ask with and no portal grant to fall back on. Not counted as a miss:
            // the stop-time warning distinguishes "nothing was asked" from "nothing answered"
            // and this is the former.
            None => return false,
        };
        let Some(rect) = asked else {
            self.ink.window_rect_misses = self.ink.window_rect_misses.saturating_add(1);
            return false;
        };
        if self.ink.window_rects.last().is_some_and(|(_, r)| *r == rect) {
            return false;
        }
        let at = self.ink_media_ms();
        self.ink.window_rects.push((at, rect));
        true
    }

    /// Publish the scene as it stands NOW, so the capture worker can paint it onto the frames
    /// it is feeding the encoder (DRAGON-724).
    ///
    /// The live half of the same feature `publish_ink_bake` below is the fallback for, and the
    /// preconditions are deliberately the SAME: an inked WINDOW recording whose capture reads
    /// the window's own buffer. What differs is the timing. This runs all through the
    /// session, after every ink message and on the recording poll, so the worker always has the
    /// current drawing; that one runs once, at stop.
    ///
    /// It carries the media instant it was taken at, and the worker replays exactly that instant
    /// rather than extrapolating from it. That is what removes the whole class of "the marks led
    /// the cursor" defect: nothing converts between the app's clock and the file's, because the
    /// scene and the frame are married while both are live.
    ///
    /// BRONZE is deliberately excluded, and since DRAGON-736 that exclusion has a consequence
    /// worth naming. Its strokes are already in STREAM coordinates, for the bake's identity
    /// mapping, so publishing them here would place them by the window's screen rect and draw
    /// them in the wrong part of the frame. This used to be free, because the Linux workers
    /// painted nothing; the CPU portal worker paints now, and it is the very worker a Bronze
    /// session runs on. So it builds a painter that is never fed, and its bake-drop rule asks
    /// `painted()` rather than `active()` for exactly that reason. See the drop site in
    /// `record::pipewire`.
    ///
    /// The cost is one clone of the ink log per call, which is the same order as the re-fit
    /// `ink_stroke_move` already runs on every accepted sample, and it is paid on the UI thread
    /// rather than the capture one.
    pub(super) fn publish_live_ink(&self) {
        let Some(rec) = self.recording.as_ref() else { return };
        if !self.ink_needs_bake() || self.ink_tier().shows_snapshot() {
            return;
        }
        if self.ink.window_rects.is_empty() {
            // The window was never located. A stale pick-time rectangle is deliberately NOT
            // used as a stand-in, for the reason `publish_ink_bake` states: a window we cannot
            // find is a window we cannot say anything about, and placing the drawing at a
            // guess is worse than not placing it.
            return;
        }
        // Nothing drawn AND no tool armed: there is nothing for the worker to paint. An armed
        // tool with an empty log DOES publish (DRAGON-739), because the scene is what carries
        // the screen-to-video placement the pointer sprite is positioned through, and the
        // cursor is missing from the file from the moment the tool is armed. The layer that
        // publishes is empty, so the worker rasterizes nothing and blends nothing for it.
        if self.ink.log.clips.is_empty() && !self.ink_drawing_armed() {
            return;
        }
        let Some(o) = self.ink_surface_output().or_else(|| self.ink_output()) else {
            return;
        };
        let color = self
            .annot_color
            .unwrap_or_else(crate::app::preview::annotate::default_annot_color);
        rec.live_ink.publish(
            crate::record::ink_bake::InkBake {
                log: self.ink.log.clone(),
                rects: self.ink.window_rects.clone(),
                origin: o.logical_pos,
                point_scale: o.point_scale,
                color,
            },
            self.ink_media_ms(),
        );
    }

    /// Publish where the POINTER is, so the capture worker can draw one onto its frames
    /// (DRAGON-739).
    ///
    /// The owner reported it as "it looks like i only lose the cursor while drawing", and the
    /// cause is a compositor rule we cannot reach: cosmic-comp composites the embedded cursor
    /// into a TOPLEVEL capture only while pointer FOCUS is on that toplevel, and an armed ink
    /// surface's input region is the output minus the toolbar. So the moment a tool is armed
    /// we own the pointer, focus leaves the recorded window, and the file stops having a
    /// cursor until the tool goes back to Pointer.
    ///
    /// This runs beside [`Self::publish_live_ink`] and is deliberately NOT part of it. The
    /// worker's ink raster is cached per published scene, and the pointer moves on every mouse
    /// sample, so carrying it inside the scene would re-rasterize the whole drawing on every
    /// frame written. It writes two floats into its own slot instead, with nothing cloned.
    ///
    /// [`crate::record::ink_live::live_cursor_needed`] is the whole policy and it lives over
    /// there, pure and unit-tested, so the truth table (the macOS and Windows rows above all,
    /// which must keep answering "no") is proven on the Linux gate. Publishing `None` whenever
    /// it refuses is what lets the worker draw whatever it is handed and decide nothing.
    pub(super) fn publish_live_cursor(&self) {
        let Some(rec) = self.recording.as_ref() else { return };
        let want = self.live_cursor_wanted();
        rec.live_ink.publish_cursor(if want { self.ink.pointer } else { None });
    }

    /// Must WE draw the pointer for this session, right now (DRAGON-739)?
    ///
    /// The ONE place the gate's facts are collected, read by both the publisher above and the
    /// view (which only asks the canvas to REPORT a hover where one could be used, so every
    /// other session's ink surface keeps its exact old event behaviour). Splitting the two
    /// would let a session report positions nobody draws, or draw from positions nobody
    /// reports.
    pub(super) fn live_cursor_wanted(&self) -> bool {
        crate::record::ink_live::live_cursor_needed(
            // The FIRST disjunct of `ink::needs_composite`, spelled out rather than taken from
            // `ink_needs_bake()`, because that answer is also true for BRONZE by its other
            // disjunct and Bronze must not draw a cursor. The next term is what says so.
            self.mode == Mode::Window
                && crate::platform::window_capture_excludes_overlays(self.portal_window_cast()),
            // Gold and Silver draw on the LIVE screen over the recorded window. Bronze draws
            // on a frozen still of the stream inside a window of ours, so its user's pointer
            // is over OUR window and the recorded one never had a cursor to lose.
            !self.ink_tier().shows_snapshot(),
            // The ONE cursor rule (`app::cursor_wanted`), over the EFFECTIVE extra, which is
            // the persisted preference already ANDed with the backend's own capability. That
            // is the same pair the portal's own `CursorRequest` was decided from, so a
            // recording the user asked to have no pointer in cannot gain one here.
            crate::app::cursor_wanted(self.effective_capture_extras().cursor, self.color_picking()),
            self.ink_drawing_armed(),
            crate::platform::window_cursor_source(),
        )
    }

    /// Hand the recording worker the ink to composite into a WINDOW recording.
    ///
    /// Called once, from `stop_recording`, before the stop flag goes up. A session that does
    /// not need a bake, or drew nothing, leaves the slot `None` and finalize builds exactly
    /// the command it always did.
    ///
    /// Since DRAGON-724 this is the FALLBACK rather than the normal path, and it is published
    /// unconditionally anyway: the worker DROPS it when its own size gate let it paint the ink
    /// onto the captured frames. That is on purpose. The gate needs the encode size and the
    /// frame rate, which only the worker knows, so having the app publish either way is what
    /// lets the fallback need no coordination at all.
    ///
    /// One last rect sample is taken here on purpose: the user may have drawn after the final
    /// poll, and a stroke with no rect at or before it would be mapped by the previous
    /// sample, which is the right answer only if the window had not moved since.
    pub(super) fn publish_ink_bake(&mut self) {
        if !self.ink_needs_bake() || self.ink.log.clips.is_empty() {
            return;
        }
        let color = self
            .annot_color
            .unwrap_or_else(crate::app::preview::annotate::default_annot_color);
        // BRONZE (DRAGON-693): the strokes are in STREAM pixels, so the whole mapping is one
        // rect sample covering the frame at the origin, with no scale. `point_to_video` is then
        // the identity when the encode kept the stream's size and the right rescale when the
        // resolution cap shrank it, which is exactly what "an identity mapping in stream space"
        // has to mean once an encoder is allowed to resize.
        if self.ink_tier().shows_snapshot() {
            let Some((sw, sh)) = self.ink_stream_size() else {
                log::warn!(
                    "ink bake: {} strokes drawn but the recording never reported its frame                      size, so the drawing cannot be placed in the video",
                    self.ink.log.clips.len()
                );
                return;
            };
            let bake = crate::record::ink_bake::InkBake {
                log: self.ink.log.clone(),
                rects: vec![(0, (0, 0, sw as i32, sh as i32))],
                origin: (0, 0),
                point_scale: 1.0,
                color,
            };
            if let Some(rec) = self.recording.as_ref()
                && let Ok(mut g) = rec.ink.lock()
            {
                log::debug!(
                    "ink bake (bronze): handing finalize {} strokes over a {sw}x{sh} stream",
                    bake.log.clips.len()
                );
                *g = Some(bake);
            }
            return;
        }
        let _ = self.sample_ink_window_rect();
        let Some(o) = self.ink_surface_output().or_else(|| self.ink_output()) else {
            log::warn!("ink bake: no output for the ink surface, so the drawing is not baked");
            return;
        };
        let (origin, point_scale) = (o.logical_pos, o.point_scale);
        let bake = crate::record::ink_bake::InkBake {
            log: self.ink.log.clone(),
            rects: self.ink.window_rects.clone(),
            origin,
            point_scale,
            color,
        };
        if bake.is_empty() {
            // Ink was drawn but the window was never located, so there is no mapping and
            // nothing honest to composite. A stale pick-time rectangle is deliberately NOT
            // used as a fallback: a window that cannot be found now is a window we cannot
            // say anything about, and placing the drawing at a guess is worse than not
            // placing it. Loud, because the drawing is what the user will look for, and it
            // NAMES which of the two causes it was, because the first cut of this message
            // could not (DRAGON-691: the platform query was broken and answered `None`
            // every time, which reads identically to a session that never asked).
            let why = if self.ink.window_rect_misses == 0 {
                "the recorded window was never asked about (no window id for this session)"
                    .to_string()
            } else {
                format!(
                    "the platform could not locate the recorded window on any of {} attempts",
                    self.ink.window_rect_misses
                )
            };
            log::warn!(
                "ink bake: {} strokes drawn but {why}, so the drawing cannot be placed in the \
                 video",
                self.ink.log.clips.len()
            );
            return;
        }
        let Some(rec) = self.recording.as_ref() else {
            return;
        };
        if let Ok(mut g) = rec.ink.lock() {
            let magic = bake
                .log
                .clips
                .iter()
                .filter(|c| c.tool.fades())
                .count();
            log::debug!(
                "ink bake: handing finalize {} strokes ({magic} magic), {} generations, {} \
                 erases and {} window rects",
                bake.log.clips.len(),
                bake.log.gens.len(),
                bake.log.erases.len(),
                bake.rects.len()
            );
            *g = Some(bake);
        }
    }
}

/// The armed window's backdrop wash (DRAGON-693): a faint scrim behind the ink.
///
/// Faint on purpose. This window is IN the recording on Silver, unavoidably (the whole point
/// is that the portal cast carries it), so anything it paints is in the file. A scrim heavy
/// enough to read as a mode but light enough to leave the desktop legible is the compromise;
/// a fully clear window would be a focused full-screen click trap with nothing to say so, and
/// an opaque one would hide the thing being annotated.
///
/// On Bronze it sits BEHIND the fitted snapshot, filling the letterbox bars, which is what
/// keeps the picture's edges legible against whatever the desktop happens to be.
fn ink_scrim(theme: &cosmic::Theme) -> cosmic::iced::widget::container::Style {
    let c: cosmic::iced::Color = theme.cosmic().background(false).base.into();
    cosmic::iced::widget::container::Style {
        background: Some(cosmic::iced::Background::Color(cosmic::iced::Color { a: 0.35, ..c })),
        ..Default::default()
    }
}

/// DRAGON-742: the three disarm routes end in ONE state change.
///
/// **Why this is a SOURCE guard and not a behavioural test.** The thing worth proving here is
/// not "V disarms", which is a single `if` anyone can read; it is that Escape, a right click
/// and bare `V` leave IDENTICAL state, so the three cannot drift apart later. That is a
/// statement about the CODE rather than about a value, because the state in question lives on
/// an `App` no test in this crate can construct (it needs a compositor connection and a
/// libcosmic `Core`), and because the parts that would drift are the ones a route does BESIDE
/// moving the selector: the Silver / Bronze final sweep, the tray submenu, the DRAGON-694
/// media un-duck, the surface sync, and DRAGON-739's painted cursor, which
/// `App::update_ink` drops to `None` after every ink message.
///
/// So the guarantee is made structural instead: there is exactly ONE place that spells the
/// disarm, and every route calls it. `src/util.rs`'s console-spawn guard is the same shape and
/// the precedent for reading the tree from a test.
///
/// # What these do when they are WRONG
///
/// A test that greps the tree fails differently from an ordinary one, so each failure names the
/// file, the line and the fix rather than leaving a reader to reverse-engineer the pattern:
///
/// * [`only_disarm_ink_tool_selects_the_pointer_tool`] prints every offending `path:line` and
///   the method to call instead. A FALSE positive is possible in one shape, a comment or a
///   string that happens to spell the call, and the fix there is to reword it; the guard is
///   still saying something true about what the file contains.
/// * [`escape_the_right_click_and_the_disarm_key_all_call_it`] prints the count it found beside
///   the count it wanted, per file, and says what each direction means: fewer is a route that
///   lost its call, more is a fourth route that was added without deciding to.
/// * [`sources`] refuses an empty or tiny scan rather than passing vacuously, which is the
///   failure mode that would make both of the above meaningless. The floor is deliberately far
///   below the real number (329 files here, 261 in the public export, which strips the two
///   closed platform dirs) so it can only ever fire on a scan that is actually broken.
#[cfg(test)]
mod disarm_route_tests {
    use std::path::{Path, PathBuf};

    /// This file, which is allowed to spell the disarm because it IS the disarm.
    const HOME: &str = "src/app/ink.rs";

    /// Every `.rs` under `src/`, as (repo-relative path, contents).
    fn sources() -> Vec<(String, String)> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut out = Vec::new();
        let mut stack: Vec<PathBuf> = vec![root.join("src")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, text));
            }
        }
        // A scan that found nothing would pass both guards vacuously, which is the one way a
        // source test can be worse than no test. The floor is far below the real count on
        // purpose (see the module doc), so it can only fire on a genuinely broken walk.
        assert!(
            out.len() > 100,
            "the source scan found only {} .rs files under src/, so these guards would pass \
             vacuously; is CARGO_MANIFEST_DIR pointing at the repo?",
            out.len()
        );
        out
    }

    /// Where every `SelectTool(<arg>)` in `text` is, as (line number, the argument).
    ///
    /// It reads the ARGUMENT by matching parentheses rather than by looking at the rest of the
    /// line, and that is the whole reason this helper exists. **This repo is not
    /// rustfmt-enforced, so a call wraps wherever its author wrapped it**, and a
    /// `SelectTool(\n    crate::ink::InkTool::Pointer,\n)` is exactly the disarm the guard
    /// below exists to catch and exactly what a line-at-a-time scan would miss. Getting that
    /// wrong has already cost this project once, in the other direction: see the working note
    /// about greping a bare identifier rather than a formatted assignment.
    ///
    /// Nesting is handled because the argument may contain its own parentheses, and an
    /// unterminated call (a truncated file) is skipped rather than panicked on.
    fn select_tool_args(text: &str) -> Vec<(usize, String)> {
        const NEEDLE: &str = "SelectTool(";
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut from = 0usize;
        while let Some(hit) = text[from..].find(NEEDLE) {
            let open = from + hit + NEEDLE.len();
            let mut depth = 1usize;
            let mut end = None;
            for (i, b) in bytes.iter().enumerate().skip(open) {
                match b {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(close) = end else { break };
            let line = text[..open].lines().count();
            out.push((line, text[open..close].to_string()));
            from = close;
        }
        out
    }

    /// **The convergence.** Nothing outside [`App::disarm_ink_tool`] may spell "select the
    /// Pointer tool" for itself. A fourth route that wrote its own version would look right and
    /// be wrong in exactly the way DRAGON-739's cursor is wrong: invisible until it is in the
    /// video. So this fails and names the method to call instead.
    ///
    /// Both spellings are caught, qualified and imported, because the file that would host a
    /// new route may already have `InkTool` in scope. The TOOLBAR selectors and the CLI's
    /// `record --tool` are untouched by it: they pass the tool as a VARIABLE, which is right,
    /// since neither of them is a disarm in particular.
    #[test]
    fn only_disarm_ink_tool_selects_the_pointer_tool() {
        let mut violations = Vec::new();
        for (rel, text) in sources() {
            if rel == HOME {
                continue;
            }
            for (line, arg) in select_tool_args(&text) {
                if arg.contains("InkTool::Pointer") {
                    violations.push(format!("{rel}:{line}"));
                }
            }
        }
        assert!(
            violations.is_empty(),
            "these spell the disarm for themselves instead of calling `App::disarm_ink_tool`, \
             so they can drift away from what Escape does (the final sweep, the tray submenu, \
             the media un-duck, the surface sync, and DRAGON-739's painted cursor). Call \
             `self.disarm_ink_tool()` instead: {violations:?}"
        );
    }

    /// And the three routes really are wired to it. The two window-chrome calls are Escape
    /// (`WindowChromeMsg::Close`, during a live recording) and the right press
    /// (`WindowChromeMsg::RightPressed`); the keyboard one is bare `V`.
    ///
    /// Counted EXACTLY rather than at least once, which cuts both ways on purpose: a route that
    /// quietly loses its call fails here, and so does a FOURTH route added without thinking
    /// about it. Adding one deliberately is a one-line edit to this test, and that line is the
    /// visible diff the change deserves.
    #[test]
    fn escape_the_right_click_and_the_disarm_key_all_call_it() {
        let files = sources();
        let calls = |rel: &str| -> usize {
            let (_, text) = files
                .iter()
                .find(|(p, _)| p == rel)
                .unwrap_or_else(|| panic!("{rel} must exist for this guard to mean anything"));
            text.matches("self.disarm_ink_tool()").count()
        };
        for (rel, want, who) in [
            (
                "src/app/update/window_chrome.rs",
                2,
                "Escape (WindowChromeMsg::Close) and the right press \
                 (WindowChromeMsg::RightPressed)",
            ),
            ("src/app/keyboard.rs", 1, "the bare V disarm key"),
        ] {
            let got = calls(rel);
            assert_eq!(
                got, want,
                "{rel} calls `self.disarm_ink_tool()` {got} times, expected {want} ({who}). \
                 FEWER means a route stopped routing through the one disarm and will drift; \
                 MORE means a route was added, which is fine, but update this count in the \
                 same commit so it is a decision rather than an accident."
            );
        }
    }
}
