//! Preview surface lifecycle and top-level views: opening (spinner/window/
//! overlay), the appearance toggle, resize/focus handling, and the composed
//! preview/loading/dialog views.
//! Split from `preview/mod.rs` (DRAGON-115) — pure code motion.

use super::*;

// ── The editor's own launch timeline (DRAGON-454) ────────────────────────────────────
//
// `App::init` → `acquire_scene` → `configure_overlay` were instrumented with
// `util::timing_mark`; the PREVIEW OPEN path was not, so the seconds between "the capture is
// written" and "the toolbar answers a click" were invisible to the DRAGON-419 debug log — the
// reason a stall everyone could feel could not be pointed at. These marks are the permanent
// instrument for that stretch, exactly as the launch marks are for the one before it. They are
// `timing_mark`, i.e. debug-level and silent unless the debug log (or `CCK_TIMING`) is on.
//
// The chain, in the order a capture walks it:
//   present_capture -> open_preview / spinner -> surface minted -> auto-copy -> decode thread
//   -> ImageReady -> view build #1 (spinner) -> window shown -> first view build WITH MEDIA
//
// View builds are marked with an index for the first [`MARKED_VIEW_BUILDS`] of them, because
// the GAP BETWEEN CONSECUTIVE BUILDS is the measurement that matters: a UI thread stuck in
// first-frame GPU work does not build views, so a long gap between #n and #n+1 is the stall,
// located to the frame.

/// How many preview view builds carry an indexed mark. Enough to cover the whole open
/// sequence — the spinner frames, the swap to media, the first painted frames, AND the
/// [`toast::TOAST_TTL`] window that follows it at one `ToastTick` redraw every 250ms (~16
/// ticks) — and no more. The instrument is for the OPEN, not for the editing session after it.
const MARKED_VIEW_BUILDS: u32 = 32;

/// Times one preview view build and records it on the launch timeline when it drops — so the
/// mark carries the build's own COST, not just when it started (DRAGON-454 follow-up: the
/// owner's "the editor became usable when the toast went away" points at the per-tick redraw,
/// and the only way to accept or reject that is to know what a build costs).
///
/// A guard rather than a pair of calls because `preview_view` has several early returns.
struct ViewBuildMark {
    started: std::time::Instant,
    index: u32,
    loaded: bool,
}

impl Drop for ViewBuildMark {
    fn drop(&mut self) {
        let ms = self.started.elapsed().as_secs_f64() * 1000.0;
        let what = if self.loaded { "media" } else { "spinner" };
        crate::util::timing_mark(&format!(
            "preview: view build #{} ({what}) took {ms:.1}ms",
            self.index
        ));
    }
}

/// Start timing one preview view build (see the note above). `loaded` says whether this build
/// carries the decoded media or the loading spinner; the first loaded build also gets its own
/// mark, since that is the frame the user is waiting for. `None` once the open sequence's
/// [`MARKED_VIEW_BUILDS`] budget is spent — an editing session is not instrumented.
fn mark_preview_view(loaded: bool) -> Option<ViewBuildMark> {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    static BUILDS: AtomicU32 = AtomicU32::new(0);
    static FIRST_LOADED: AtomicBool = AtomicBool::new(false);
    if loaded && !FIRST_LOADED.swap(true, Ordering::Relaxed) {
        crate::util::timing_mark(
            "preview: FIRST view build carrying the media (widget tree built; the frame it \
             describes has NOT been drawn yet)",
        );
    }
    if !crate::util::timing_on() {
        return None;
    }
    let n = BUILDS.fetch_add(1, Ordering::Relaxed);
    (n < MARKED_VIEW_BUILDS).then(|| ViewBuildMark {
        started: std::time::Instant::now(),
        index: n + 1,
        loaded,
    })
}

/// One ACTION button on the unsaved-changes dialog: the action's own toolbar glyph beside
/// its name, as a standard button. `tint` colours the glyph when the action deserves a
/// warning (Delete); `None` leaves it on the button's own text colour.
fn dialog_action(
    icon: &'static str,
    label: &'static str,
    msg: PreviewMsg,
    id: window::Id,
    tint: Option<fn(&cosmic::Theme) -> cosmic::iced::Color>,
) -> Element<'static, Msg> {
    let glyph = crate::widgets::icons::sized(icon, 16.0);
    let glyph = match tint {
        Some(tint) => glyph.class(cosmic::theme::Svg::Custom(std::rc::Rc::new(
            move |t: &cosmic::Theme| cosmic::widget::svg::Style { color: Some(tint(t)) },
        ))),
        None => glyph,
    };
    let content = widget::row(vec![glyph.into(), widget::text(label).size(13).into()])
        .spacing(6.0)
        .align_y(Alignment::Center);
    crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::custom(content)
            .class(cosmic::theme::Button::Standard)
            .padding([6.0, 12.0])
            .on_press(Msg::Preview(id, msg)),
    )
}

impl App {
    /// A fresh [`EditState`] seeded with the PERSISTED annotation defaults (the last-used
    /// stroke color + tool + step-marker size), so every new preview opens the way the user
    /// left the annotation controls (DRAGON-321). `None` color → the accent-complement
    /// default; `None` tool → neutral; `0.0` badge size → the built-in default.
    ///
    /// This is THE document-open seam for the remembered badge side: each document takes its
    /// own WORKING COPY here, and a placement/resize writes back to the persisted one
    /// (`App::remember_badge_size`). Two documents open at once may therefore briefly disagree
    /// — deliberately: last write wins on disk, and the next document opened takes it up. No
    /// cross-document sync.
    pub(super) fn new_edit_state(&self) -> EditState {
        EditState {
            annot_color: self.annot_color,
            tool: self.annot_tool,
            annot_stroke_w: self.annot_stroke_w,
            annot_badge_size: self.annot_badge_size,
            annot_text_size: self.annot_text_size,
            annot_text_font: self.annot_text_font,
            ..EditState::default()
        }
    }

    /// Open the preview overlay *now* with a loading spinner (no content, no path yet),
    /// for captures whose grab doesn't read the live composited screen (a window grab, a
    /// freeze crop, or a finished recording finalizing off-thread). The user sees the
    /// preview the instant they accept/stop, instead of a blank desktop while the work
    /// runs; [`Self::present_capture`] later fills in the saved path and the content.
    /// Returns the surface task (or none if preview is disabled or the output is unknown).
    pub(in crate::app) fn open_preview_spinner(
        &mut self,
        kind: PreviewKind,
        dims: Option<(u32, u32)>,
    ) -> Task<cosmic::Action<Msg>> {
        crate::util::timing_mark("preview: open_preview_spinner (pre-open, no path yet)");
        let Some((output, monitor)) = self.preview_output.clone() else {
            return Task::none();
        };
        // A pre-opened video spinner is a stopped recording; pause other media right now if it
        // captured audio ("either mic or stereo"). An image grab (window/freeze) has no sound.
        let with_audio = matches!(kind, PreviewKind::Video(_))
            && (self.record_mic || self.record_system_audio);
        // Size the window to the capture up front (dims are known at grab time) so the
        // compositor honours the initial size — a post-open resize gets overridden.
        let extra_h = transport_h_for(&kind, PreviewSurface::Window);
        let source_scale = self.preview_source_scale(Some(&output));
        let (id, open_task, monitor, surface) =
            self.preview_surface_for(None, Some(output), monitor, dims, extra_h);
        self.previews.push(PreviewState {
            window: id,
            surface,
            max_hint_pending: surface.is_window(),
            display_dims: dims,
            path: None,
            size: None,
            external: false,
            monitor,
            source_scale,
            loading_msg: random_loading_msg(),
            kind,
            edit: self.new_edit_state(),
            view: Viewport::default(),
            ducking: false,
            surface_open: true,
            toasts: Toasts::default(),
            copied_on_open: false,
            demoted: false,
            bake_src: None,
            saved_path: None,
        });
        // This is the in-flight capture's document; the capture flow addresses it by id.
        self.capture_preview = Some(id);
        self.focused_preview = Some(id);
        self.engage_preview_duck(id, with_audio);
        open_task
    }

    /// The CAPTURE SOURCE display's point→pixel backing scale, cached into
    /// `preview_output_scale` at capture time (`scale_for_selection`), used by the WINDOW
    /// open-fit to divide the capture's PHYSICAL pixels back into the LOGICAL points it
    /// occupied on screen (a Retina grab opens at true on-screen size, not 2×). This is the
    /// SELECTION's monitor scale, NOT the trigger display the preview now opens on
    /// (DRAGON-309): the media is physical pixels of the display it was grabbed from, so
    /// reading the TRIGGER's scale here (as this used to on macOS, via the passed `output`)
    /// shrank cross-display captures — a 1× grab shown on a 2× trigger opened half-size.
    /// Every platform resolves the real per-output scale here (COSMIC buffer scale, macOS
    /// backing scale, Windows per-monitor DPI); `1.0` means an UNSCALED output, a capture
    /// with no known output, or a `--preview` file — and on `1.0` the sizing math is
    /// byte-identical. `_output` is retained for call-site symmetry.
    pub(super) fn preview_source_scale(&self, _output: Option<&OutputHandle>) -> f32 {
        let s = self.preview_output_scale;
        if s > 0.0 { s } else { 1.0 }
    }

    /// Create the preview surface per the appearance setting: a resizable WINDOW when
    /// `preview_windowed`, else the fullscreen layer-shell overlay. Returns the window
    /// id, the open task, the initial content size to scale to (the window's own
    /// size when windowed — corrected later by resize events — else the capture
    /// monitor), and the [`PreviewSurface`] kind that was minted (for the caller to
    /// record on `PreviewState`). `output` anchors the overlay (Some = capture
    /// monitor, None = active). `extra_h` is the media kind's transport-strip
    /// reserve for a WINDOW surface — callers derive it from the kind being opened
    /// via [`transport_h_for`] (a plain value, since the preview may not be stored in
    /// `self.previews` yet when a spinner pre-opens).
    ///
    /// `existing` names the preview being RE-minted (the appearance toggle, the
    /// cover→window swap, the Save-As re-open), whose already-loaded media supplies the
    /// size when the caller has no hint; `None` for a fresh document.
    ///
    /// THE FULLSCREEN OVERLAY IS THE LONE DOCUMENT'S APPEARANCE (DRAGON-336): as soon as a
    /// SECOND preview document exists, NONE of them is an overlay — they are all windows.
    /// So when any other document is open this mints a WINDOW regardless of the
    /// `preview_windowed` setting AND demotes a sibling still holding the overlay
    /// ([`Self::demote_preview_to_window`]), leaving either exactly one overlay or N
    /// windows, never a mix. (Beyond the mixed look, two mapped `Exclusive` layer surfaces
    /// would fight over the keyboard grab — the DRAGON-109 note at the top of `shell.rs`.)
    /// This is the single place the rule is enforced.
    pub(super) fn preview_surface_for(
        &mut self,
        existing: Option<window::Id>,
        output: Option<OutputHandle>,
        capture_monitor: (u32, u32),
        media_hint: Option<(u32, u32)>,
        extra_h: f32,
    ) -> (window::Id, Task<cosmic::Action<Msg>>, (u32, u32), PreviewSurface) {
        // DRAGON-454: the ONE mint choke point, so the launch timeline records every preview
        // surface — spinner, capture, handoff, appearance toggle — from a single place.
        crate::util::timing_mark("preview: preview_surface_for (mint decision, begin)");
        // DRAGON-336 phase 3b: BIND THE HANDOFF LISTENER BEFORE THE MARKER BELOW. The
        // marker IS the discovery record — a child that finds it goes looking for the
        // socket beside it — so binding second would open a window in which a capture
        // child sees a host that isn't listening and needlessly opens its own preview.
        // (That direction is merely a missed optimisation, never a lost capture: an
        // absent socket makes the child's connect fail into its own preview. Ordering it
        // this way simply removes the window.) Idempotent — every re-mint (the appearance
        // toggle, the cover→window swap, a Save-As re-open) routes through here and must
        // keep the ONE listener already bound. A bind failure is non-fatal: we just don't
        // host, and every child behaves exactly as it did before this existed.
        #[cfg(unix)]
        if self.preview_host.is_none() {
            match crate::preview_ipc::start_host() {
                Ok(host) => {
                    log::info!("preview handoff: hosting on {}", host.socket_path());
                    self.preview_host = Some(host);
                }
                Err(e) => log::warn!("preview handoff: not hosting ({e})"),
            }
        }
        // DRAGON-322: a preview editor is opening — advertise it cross-process (the single
        // mint choke point every preview-open routes through) so a concurrent capture's
        // sibling sweep spares this process (self-capture keeps the existing preview open).
        // Cleared at `finish_session` (a preview close ends the process).
        crate::instance::set_preview_marker(true);
        // A window pick pre-opens its spinner as the FULLSCREEN overlay even in WINDOWED mode
        // (the only focus-safe cover), then swaps it for the real window at `WindowGrabbed`.
        // Force the overlay branch while that pre-open is pending, regardless of
        // `preview_windowed`. Linux gates on `window_spinner_neutral` (the focus-neutral layer
        // surface); macOS (DRAGON-219) gates on `mac_preview_preopen` (the visible:false
        // shielding-level cover). Windows never sets either, keeping its decision byte-identical.
        #[cfg(target_os = "macos")]
        let force_neutral_overlay = self.mac_preview_preopen;
        // Windows (DRAGON-305): a WINDOWED window pick pre-opens the fullscreen blocker cover; force
        // the overlay branch while that pre-open is pending (the non-activating shielding cover).
        #[cfg(windows)]
        let force_neutral_overlay = self.win_preview_preopen;
        #[cfg(not(any(target_os = "macos", windows)))]
        let force_neutral_overlay = self.window_spinner_neutral;
        // NO OVERLAY ONCE A SECOND DOCUMENT EXISTS (DRAGON-336): the fullscreen preview is
        // only ever the LONE document's appearance, so another open document makes this one
        // a window whatever the setting says (see [`overlay_taken`] for the rule and its
        // reasons). Re-minting the SAME document (the appearance toggle / Save-As re-open)
        // doesn't count: its old surface is being torn down in the same pass. Never true
        // with a single preview open, so the single-document decision is byte-identical.
        let overlay_taken = self.overlay_barred(existing);
        // Bound rather than returned from each arm purely so ONE mark below records what was
        // minted, whichever branch produced it (DRAGON-454). The branches themselves are
        // unchanged.
        let minted =
        // Linux (layer-shell), macOS, and now Windows (DRAGON-233 fix 5 — a real
        // PlainWindows overlay, below) all honor `preview_windowed`. The cfg! folds to
        // `false` on all three (it only forces the WINDOW on an exotic platform with no
        // overlay implementation), so their decisions stay byte-identical to before —
        // and Windows now switches modes from the appearance toggle instead of always
        // opening a window.
        if (self.preview_windowed && !force_neutral_overlay)
            || (overlay_taken && !force_neutral_overlay)
            || cfg!(all(
                not(target_os = "linux"),
                not(target_os = "macos"),
                not(target_os = "windows")
            ))
        {
            // NEVER A MIXED STATE (DRAGON-336): this preview is opening as a WINDOW, so a
            // sibling still sitting on the fullscreen overlay comes down to a window in the
            // same pass — an overlay behind floating preview windows is the strange state
            // this rule exists to prevent. No-op with a single document (there is no
            // sibling), and no-op while a window pick's FOCUS-NEUTRAL cover is pre-opening
            // (it takes the overlay branch below; the demotion happens at its cover→window
            // swap instead, which routes back through here).
            let demote = self.demote_overlay_previews(existing);
            let (id, open, size) =
                self.mint_preview_window(existing, output, capture_monitor, media_hint, extra_h);
            (id, Task::batch([demote, open]), size, PreviewSurface::Window)
        } else {
            // macOS: a fullscreen always-on-top window covering the capture's display
            // (the pointer's display for `--preview`, which has no capture anchor) —
            // placed/finished natively after open (see `shell::preview_overlay_window`).
            // The process drops to the ACCESSORY activation policy FIRST, before the
            // open task runs: the DRAGON-154 AeroSpace popup opt-out is decided at the
            // window's first AX exposure and requires accessory (a windowed-preview
            // finalize or a `--preview` boot may have promoted us to Regular).
            #[cfg(target_os = "macos")]
            {
                crate::platform::mac::window::ensure_accessory_policy();
                let (pos, size) =
                    crate::platform::mac::overlay_preview_rect(output.as_deref());
                // DRAGON-216: while pre-opening to cover the grab, open `visible:false` so it
                // can't key/activate us; `finalize_preview_overlay` places + orders it front
                // non-key (no `gain_focus` until `WindowGrabbed`).
                let visible = !self.mac_preview_preopen;
                let (id, open) = super::shell::preview_overlay_window(pos, size, visible);
                let title = self
                    .set_window_title(super::shell::PREVIEW_OVERLAY_TITLE.to_string(), id);
                (id, Task::batch([open, title]), size, PreviewSurface::Overlay)
            }
            // Windows (DRAGON-233 fix 5): the fullscreen OVERLAY preview — a transparent
            // always-on-top window covering the capture's display (the pointer's for
            // `--preview --overlay`, which has no capture anchor), placed + shown natively
            // AFTER open (`shell::preview_overlay_window` → `finalize_preview_overlay` →
            // `place_overlay`, which OR-s in the komorebi opt-out bit before the show).
            #[cfg(target_os = "windows")]
            {
                let (pos, size) =
                    crate::platform::windows::window::preview_overlay_rect(output.as_deref());
                let (id, open) = super::shell::preview_overlay_window(pos, size);
                let title = self
                    .set_window_title(super::shell::PREVIEW_OVERLAY_TITLE.to_string(), id);
                (id, Task::batch([open, title]), size, PreviewSurface::Overlay)
            }
            #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
            {
                let id = window::Id::unique();
                let task = match output {
                    Some(o) => self.preview_surface(o, id),
                    None => self.preview_surface_active(id),
                };
                (id, task, capture_monitor, PreviewSurface::Overlay)
            }
        };
        crate::util::timing_mark(if minted.3.is_window() {
            "preview: surface minted (WINDOW; open task queued, not yet created)"
        } else {
            "preview: surface minted (OVERLAY; open task queued, not yet created)"
        });
        minted
    }

    /// Mint the preview WINDOW itself: the open-fit sizing, the surface, and its title —
    /// everything [`Self::preview_surface_for`]'s window branch does once the
    /// overlay-vs-window decision is made. Split out (DRAGON-336) so the DEMOTION of a
    /// sibling out of the fullscreen overlay ([`Self::demote_preview_to_window`]) sizes and
    /// opens through the SAME path a normally-opened window does, rather than a second
    /// hand-rolled copy of it. Returns the window id, the open task, and the initial
    /// content size to scale to.
    ///
    /// `existing` names the preview being RE-minted (its already-loaded media supplies the
    /// size when the caller has no hint); `None` for a fresh document.
    ///
    /// UNITS (DRAGON-449): `capture_monitor` is the target display in `output`'s CAPTURE
    /// space — PHYSICAL pixels on Windows, points on macOS and Linux. When `output` is
    /// `None` there is no capture space to name it in, so the caller has already resolved
    /// LOGICAL POINTS itself ([`largest_output_points`], or a window's own last size) and
    /// the conversion below is the identity. Everything past that conversion — the fit, the
    /// max-size hint, the `min_size` clamp, the returned content size — is points.
    fn mint_preview_window(
        &mut self,
        existing: Option<window::Id>,
        output: Option<OutputHandle>,
        capture_monitor: (u32, u32),
        media_hint: Option<(u32, u32)>,
        extra_h: f32,
    ) -> (window::Id, Task<cosmic::Action<Msg>>, (u32, u32)) {
        // Open sized to the media (aspect-matched, no letterbox), from the caller's hint
        // (a fresh region capture knows its pixel size before the decode) or the already-
        // loaded content (toggling to windowed). Only fall back to ~80% of the monitor
        // width / 90% of its height when the size is genuinely unknown (a pre-opened
        // spinner still decoding), since the compositor won't shrink an over-large
        // window after the fact.
        let media = media_hint
            // Videos size to their captured footprint (the encode upscales back
            // into it); stills to their decoded pixels — the same precedence as
            // `preview_viewport`, so a toggle reopens at the size that's shown.
            .or_else(|| {
                existing.and_then(|e| self.preview_for(e)).map(|p| p.sizing_media())
            })
            .filter(|&(w, h)| w > 0 && h > 0);
        // The media arrives in PHYSICAL capture pixels; divide by the SOURCE
        // display's backing scale so the window opens to the LOGICAL points the
        // picture occupied on screen (a Retina grab isn't shown 2× too large).
        // `1.0` on Linux (and any unknown output), so `media` is unchanged there.
        let source_scale = self.preview_source_scale(output.as_ref());
        let media = media.map(|px| sizing::to_points(px, source_scale));
        // The MONITOR arrives in CAPTURE space as well (PHYSICAL pixels on Windows), while
        // EVERY number derived from it below is LOGICAL POINTS: the fit budgets, the
        // size-unknown fallback's 80%/90% of the display, and the max-size hint plus the
        // `min_size` clamp `preview_window` builds from it. Cross once, here (DRAGON-449) —
        // feeding the physical rect straight through made all of them `dpi/96`x too
        // permissive, so at 300% the window could open TALLER than its own display.
        // `1.0` (unchanged) on Linux, macOS, and every 96-DPI Windows monitor.
        let monitor = monitor_fit_points(capture_monitor, monitor_point_scale(output.as_ref()));
        // Both platforms size through the SAME `windowed_fit_size`, which caps the
        // window at 90% of the monitor height (rule 3, DRAGON-221) so it clears the
        // Dock / menu bar / panels neither compositor can measure client-side —
        // macOS won't shrink an over-large window after open, and this keeps the
        // request inside the usable area up front. Only fall back to ~80% of the
        // monitor width / 90% of its height when the size is genuinely unknown (a
        // pre-opened spinner still decoding), since the compositor won't shrink an
        // over-large window later.
        let (w, h) = match media {
            Some(m) => windowed_fit_size(m, Some(monitor), extra_h, self.preview_toolbar_labels),
            None => (
                (monitor.0 as f32 * 0.8).clamp(super::shell::PREVIEW_MIN_W, 1600.0),
                (monitor.1 as f32 * 0.9).clamp(super::shell::PREVIEW_MIN_H, 1000.0),
            ),
        };
        // DRAGON-309: remember the intended open size so the macOS native finalize can
        // re-assert it after moving the window to the trigger monitor (winit births it on
        // the capture/active monitor, where macOS may clamp its height to a smaller screen
        // before we relocate it). Kept off `PreviewState.monitor`, which a resize overwrites.
        self.preview_open_size = Some((w.round().max(1.0) as u32, h.round().max(1.0) as u32));
        // The windowed preview is only minted AFTER the grab (a window pick covers the
        // grab with the fullscreen overlay, then swaps to this window, DRAGON-219), so it
        // always opens visible and takes focus normally.
        let (id, open) = super::shell::preview_window(
            (w, h),
            (monitor.0 as f32, monitor.1 as f32),
        );
        // Title the window (server-side decorations show it in the titlebar; on
        // macOS the native finalize matches this exact title — hence the shared const).
        let title =
            self.set_window_title(super::shell::PREVIEW_WINDOW_TITLE.to_string(), id);
        (id, Task::batch([open, title]), (w as u32, h as u32))
    }

    /// Bring every OTHER document still sitting on the fullscreen overlay down to a
    /// window, because a preview is opening as a window beside it (DRAGON-336). At most
    /// one sibling can qualify — the rule below never lets a second overlay exist — but
    /// this sweeps them all so the invariant is enforced rather than assumed.
    ///
    /// `minting` is the document being minted for; it is never its own sibling.
    fn demote_overlay_previews(&mut self, minting: Option<window::Id>) -> Task<cosmic::Action<Msg>> {
        let tasks: Vec<_> = overlay_siblings(&self.previews, minting)
            .into_iter()
            .map(|id| self.demote_preview_to_window(id))
            .collect();
        Task::batch(tasks)
    }

    /// DEMOTE one open document out of the fullscreen overlay into a normal window,
    /// preserving it completely: same [`PreviewState`] (pending covermark/annotation
    /// edits, the shared undo/redo history, the zoom/pan viewport, video playback position
    /// and timeline edits), re-pointed onto the fresh surface by [`Self::repoint_preview`]
    /// — which also carries the focus/capture cursors and this document's audio-duck hold.
    /// The window is minted through [`Self::mint_preview_window`], the SAME open-fit path
    /// (`windowed_fit_size` + the transient `max_size` hint) a normally-opened window
    /// takes, so it lands at a sane size instead of cosmic-comp's 2/3 reshape.
    ///
    /// ONCE DEMOTED, THE DOCUMENT STAYS WINDOWED for the rest of the session (the sticky
    /// [`PreviewState::demoted`] pin, read by [`overlay_taken`]): it is NOT promoted back
    /// to the overlay when its siblings close, because silently re-entering fullscreen as
    /// windows disappear would be jarring. The user's own appearance toggle still clears
    /// the pin — an explicit choice is not a silent one.
    ///
    /// The appearance SETTING is deliberately untouched: a demotion is forced by the
    /// second document, not chosen, so it must not become the default for the next
    /// capture (unlike [`Self::toggle_preview_appearance`], which persists its flip).
    ///
    /// No-op for a document that is already a window, or one whose surface was torn down
    /// while it stays loaded (a background bake, or the overlay's Save-As dialog): there
    /// is nothing on screen to demote, and re-minting a window for it would resurrect a
    /// surface that was closed on purpose. Such a document re-opens through
    /// [`Self::reopen_preview_surface`], which consults the same rule and gets a window.
    /// Tear ONE preview surface down while the document stays loaded, and hand back the close
    /// task — the ONE seam every such teardown goes through (DRAGON-469).
    ///
    /// This exists to make an ordering STRUCTURAL that the whole Save As fix rests on. Off
    /// Linux a preview surface is a real winit window, so the destroy this returns will echo a
    /// `window::Event::Closed` back at us, indistinguishable from a window manager taking the
    /// surface away. [`super::surface_closed`] tells the two apart by reading
    /// [`PreviewState::surface_open`] — which therefore has to be cleared BEFORE the destroy
    /// is issued, not after. Two statements in the right order is a rule a later edit can
    /// break silently (and DRAGON-467 is rewriting `save_as_dialog` right now); minting the
    /// flag clear and the task TOGETHER is a rule it cannot.
    ///
    /// The surface is torn down via its ACTUALLY RECORDED kind, never the appearance setting.
    /// A document whose surface is already down yields `Task::none()`, so a second call can
    /// never issue a destroy for a dead id.
    ///
    /// This is NOT a document close: `close_preview` is still the "this document is finished"
    /// seam, and it removes the document BEFORE destroying its surface, which is why its own
    /// echo lands on a document that no longer exists.
    pub(super) fn hide_preview_surface(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for_mut(id) else {
            return Task::none();
        };
        if !p.surface_open {
            return Task::none();
        }
        let (surface, window) = (p.surface, p.window);
        // Ordered, and inseparable: the flag first, the destroy second.
        p.mark_surface_torn_down();
        surface.close(window)
    }

    pub(super) fn demote_preview_to_window(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        let Some(p) = self.preview_for(id) else {
            return Task::none();
        };
        if p.surface.is_window() || !p.surface_open {
            return Task::none();
        }
        let old_id = p.window;
        let external = p.external;
        let fallback_monitor = p.monitor;
        let source_scale = p.source_scale;
        let extra_h = transport_h_for(&p.kind, PreviewSurface::Window);
        // `--preview` files anchor to the active output (None); in-app captures to the
        // capture monitor — the same anchoring as `toggle_preview_appearance`'s
        // overlay→window direction.
        let (output, monitor) = if external {
            (None, fallback_monitor)
        } else {
            match self.preview_output.clone() {
                Some((o, m)) => (Some(o), m),
                None => (None, fallback_monitor),
            }
        };
        // Tear the overlay down through the ONE teardown seam (DRAGON-469): its ACTUAL
        // recorded kind, never the setting, with the liveness flag cleared first. The
        // `repoint_preview` below re-arms that flag for the fresh surface, so the net state
        // is exactly what it was before this went through the seam.
        let close = self.hide_preview_surface(old_id);
        // `mint_preview_window` sizes through `self.preview_output_scale` — the scale of the
        // capture being minted for, which is NOT this document's (a demotion is triggered by
        // some OTHER document opening, and a handed-over one carries the scale of the
        // display IT was grabbed from). Swap in this document's own scale for the mint and
        // put the caller's back, the same dance `open_handoff_preview` does. `1.0` on an
        // unscaled output, where the sizing math is unchanged.
        let saved_scale = self.preview_output_scale;
        self.preview_output_scale = source_scale;
        // No media hint: `mint_preview_window` reads the already-loaded content's size off
        // the document, exactly as the appearance toggle does.
        let (new_id, open_task, new_monitor) =
            self.mint_preview_window(Some(id), output, monitor, None, extra_h);
        self.preview_output_scale = saved_scale;
        self.repoint_preview(old_id, new_id, PreviewSurface::Window, new_monitor);
        // Pin it windowed for the rest of the session (see the doc above).
        if let Some(p) = self.preview_for_mut(new_id) {
            p.demoted = true;
        }
        // The window has a different viewport than the overlay did, so a pan that was
        // in-range before can now leave the picture's edge floating — re-clamp it to the
        // new bounds (the same follow-up the appearance toggle does).
        if let Some(((minx, maxx), (miny, maxy))) =
            self.preview_for(new_id).map(|p| self.preview_pan_bounds(p))
            && let Some(p) = self.preview_for_mut(new_id)
        {
            p.view.pan.0 = p.view.pan.0.clamp(minx, maxx);
            p.view.pan.1 = p.view.pan.1.clamp(miny, maxy);
        }
        Task::batch([close, open_task])
    }

    /// DRAGON-216/219 (windowed): swap the pre-opened fullscreen OVERLAY spinner that covered
    /// the grab for the real preview WINDOW now that the grab is done. A real toplevel takes
    /// activation (cosmic-comp's initial-map focus path is unconditional; macOS keys/activates
    /// on open), so it can only open AFTER the grab; before that the fullscreen overlay covered
    /// the wait without stealing the picked window's focus. Mints the window, re-points the
    /// existing `PreviewState` (still a loading spinner — content arrives later via
    /// `present_capture`), and keeps the OLD overlay alive painting its cover
    /// ([`Self::grab_cover_view`]) until the window's FIRST configure closes it
    /// ([`Self::preview_resized`]), so the window maps UNDER the cover with no desktop flash.
    /// One-shot; the appearance setting is NOT flipped. Portable (Linux neutral layer surface
    /// / macOS shielding-level overlay).
    pub(in crate::app) fn swap_neutral_spinner_to_window(&mut self) -> Task<cosmic::Action<Msg>> {
        // The pre-open belongs to the IN-FLIGHT capture, so that document is the one to swap.
        let Some(id) = self.capture_preview else {
            return Task::none();
        };
        let Some(p) = self.preview_for(id) else {
            return Task::none();
        };
        let old_overlay = p.window;
        let dims = p.display_dims;
        let fallback_monitor = p.monitor;
        let extra_h = transport_h_for(&p.kind, PreviewSurface::Window);
        // Anchor the window to the capture monitor (as a fresh windowed preview would),
        // falling back to the spinner's own monitor.
        let (output, monitor) = match self.preview_output.clone() {
            Some((o, m)) => (Some(o), m),
            None => (None, fallback_monitor),
        };
        // `window_spinner_neutral` was already taken (false) by the caller, so
        // `preview_surface_for` mints the WINDOW branch, not another neutral overlay.
        let (new_id, open_task, new_monitor, surface) =
            self.preview_surface_for(Some(id), output, monitor, dims, extra_h);
        self.repoint_preview(old_overlay, new_id, surface, new_monitor);
        // Keep the overlay painting its cover until the window's first configure closes it,
        // so there's never a bare desktop between the two.
        self.grab_overlay_closing = Some(old_overlay);
        open_task
    }

    /// Re-point an OPEN preview at a freshly-minted surface (the appearance toggle, the
    /// cover→window swap, the Save-As overlay re-open). The document keeps its loaded
    /// media and every edit; only the surface identity changes — so every id-keyed piece
    /// of ambient state that named the OLD surface has to follow it here (the focus +
    /// capture cursors and the audio-duck refcount). The one place that renaming
    /// happens, so a re-mint can never strand a reference on a dead id.
    pub(super) fn repoint_preview(
        &mut self,
        old_id: window::Id,
        new_id: window::Id,
        surface: PreviewSurface,
        monitor: (u32, u32),
    ) {
        if let Some(p) = self.preview_for_mut(old_id) {
            p.window = new_id;
            p.surface = surface;
            p.monitor = monitor;
            // A freshly-minted window carries the transient max_size hint again.
            p.max_hint_pending = surface.is_window();
            p.surface_open = true;
        }
        if self.focused_preview == Some(old_id) {
            self.focused_preview = Some(new_id);
        }
        if self.capture_preview == Some(old_id) {
            self.capture_preview = Some(new_id);
        }
        // The document is only changing surfaces, so its duck hold moves with it — the
        // guard is never engaged or dropped by a re-mint.
        self.preview_duck_refs.rename(old_id, new_id);
    }

    /// Flip the open preview between the fullscreen overlay and a resizable window,
    /// live: tear down the CURRENT surface (via its actual recorded kind, not the
    /// just-flipped setting), mint one of the other kind, and re-point the existing
    /// `PreviewState` (window, monitor, surface) at it — so the loaded content and
    /// every edit survive. The new appearance is also persisted as the default.
    /// No-op when no preview is open.
    pub(super) fn toggle_preview_appearance(&mut self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        // DRAGON-427: nothing to flip TO on Windows 10 — the overlay editor would inherit the
        // software rasterizer that cannot draw its shader layers. The button is hidden there
        // (`chrome.rs`), so this only catches a keyboard/message route reaching it anyway; it
        // must still refuse, or the flip would strand the user on an unusable surface.
        if !crate::platform::overlay_preview_available() {
            return Task::none();
        }
        let Some(p) = self.preview_for(id) else {
            return Task::none();
        };
        let old_id = p.window;
        let old_surface = p.surface;
        let external = p.external;
        let fallback_monitor = p.monitor;

        // NO OVERLAY ONCE A SECOND DOCUMENT EXISTS (DRAGON-336): flipping a WINDOW toward
        // the fullscreen overlay while another document is open could only mint another
        // window (the rule bars the overlay) — a close/re-open flicker that changes
        // nothing on screen. Refuse the flip instead, leaving the persisted appearance
        // alone: the appearance isn't the user's to choose while several documents are
        // open. Inert with a single preview open, so that toggle is byte-identical.
        if old_surface.is_window() && self.previews.iter().any(|p| p.window != old_id) {
            return Task::none();
        }
        // NOT WHILE THE SETTINGS PANE IS UP (DRAGON-469). Promoting back to the fullscreen
        // overlay would undo, on purpose, the very thing
        // [`Self::open_settings_from_preview`] demoted this document for: on Linux the
        // preview overlay is an `Exclusive` layer surface, so it takes the keyboard away from
        // the settings window AND covers it, and the pane is a SEPARATE process that cannot
        // defend itself. Fixing the toggle's direction made that newly reachable, so it is
        // refused rather than shipped as a trap. The refusal leaves the persisted appearance
        // alone and mints nothing, so it cannot reintroduce the reload it replaced; the toast
        // is there because a button that silently does nothing is the complaint this ticket
        // opened with.
        //
        // Honest platform note: `settings_pane_is_open` answers `false` on Windows BY DESIGN
        // (a named mutex with no non-retaining probe — see its doc), so this refusal cannot
        // fire there and a Windows user can still cover their settings window with the
        // overlay. That is acceptable where it would not be on Linux: the Windows overlay is
        // an ordinary topmost window with no keyboard grab, so the pane stays alt-tabbable and
        // fully usable underneath. Not worth a named-event probe of its own.
        if overlay_promotion_blocked(
            !toggled_preview_windowed(old_surface),
            self.settings.window.is_some() || crate::instance::settings_pane_is_open(),
        ) {
            self.preview_toast_icon(
                id,
                ToastKind::Error,
                "Close the settings window to go fullscreen",
                "emblem-system-symbolic",
            );
            return Task::none();
        }

        // An explicit appearance choice clears a forced demotion's sticky pin (see
        // [`Self::demote_preview_to_window`]) — the "stays windowed" rule is about never
        // re-entering fullscreen SILENTLY, not about overriding the user.
        if let Some(p) = self.preview_for_mut(id) {
            p.demoted = false;
        }

        // Persist the flip so it also becomes the default for the next capture.
        //
        // DRAGON-469: derived from the surface that is ACTUALLY OPEN, never by inverting the
        // setting. The two can disagree, and inverting in a disagreeing state asked for the
        // kind that was already up, so the button tore the window down and minted an identical
        // one ("clicking the button just seems to reload our window"). Identical to the old
        // expression whenever the setting and the surface agree; see
        // [`toggled_preview_windowed`], whose doc enumerates every disagreeing state.
        self.preview_windowed = toggled_preview_windowed(old_surface);
        self.save_state();

        // Tear down the CURRENT surface through the ONE teardown seam (DRAGON-469): its
        // ACTUALLY open kind, not the (already-flipped) setting, so this can't mis-close a
        // surface mid-toggle, and with the liveness flag honoured both ways — a document
        // whose surface is already down (a Save As chooser is up) now yields no destroy at all
        // instead of one for a dead id. `repoint_preview` below re-arms the flag on the fresh
        // surface, so the net state is unchanged for every reachable toggle.
        let close = self.hide_preview_surface(old_id);

        // `--preview` files anchor to the active output (None); in-app captures to the
        // capture monitor. Fall back to the preview's last-known monitor size otherwise.
        let (output, monitor) = if external {
            (None, fallback_monitor)
        } else {
            match self.preview_output.clone() {
                Some((o, m)) => (Some(o), m),
                None => (None, fallback_monitor),
            }
        };
        // DRAGON-309 behavior 2: toggling a WINDOWED editor to the fullscreen OVERLAY must open
        // on the monitor the WINDOW is CURRENTLY on — the user may have dragged it to another
        // display since it opened, so the stored capture-time anchor (`preview_output`) is
        // wrong. Query the window's live monitor and override the anchor with it. Off Linux
        // only (the OutputHandle is a display-name String there; on Linux the compositor owns
        // the layer-shell/toplevel output and the toggle keeps the capture-time anchor). Only
        // on the windowed->overlay direction (`old_surface` is the window, the flip is heading
        // to overlay); the reverse (overlay->window) and `--preview` keep their anchors.
        #[cfg(not(target_os = "linux"))]
        let (output, monitor) = if !external && old_surface.is_window() && !self.preview_windowed {
            match crate::platform::window_current_display(super::shell::PREVIEW_WINDOW_TITLE) {
                Some(name) => (Some(name), monitor),
                None => (output, monitor),
            }
        } else {
            (output, monitor)
        };
        // preview_surface_for reads the now-flipped `preview_windowed`, so this mints the
        // OTHER kind of surface.
        let extra_h = self
            .preview_for(id)
            .map(|p| transport_h_for(&p.kind, PreviewSurface::Window))
            .unwrap_or(0.0);
        let (new_id, open_task, new_monitor, surface) =
            self.preview_surface_for(Some(id), output, monitor, None, extra_h);
        self.repoint_preview(old_id, new_id, surface, new_monitor);
        // The new appearance has a different viewport, so a pan that was in-range before can
        // now leave the picture's edge floating — re-clamp it to the new bounds.
        if let Some(((minx, maxx), (miny, maxy))) =
            self.preview_for(new_id).map(|p| self.preview_pan_bounds(p))
            && let Some(p) = self.preview_for_mut(new_id)
        {
            p.view.pan.0 = p.view.pan.0.clamp(minx, maxx);
            p.view.pan.1 = p.view.pan.1.clamp(miny, maxy);
        }
        Task::batch([close, open_task])
    }

    /// Open the SETTINGS pane from the preview editor's chrome (DRAGON-353).
    ///
    /// # Why this is NOT `App::open_settings`
    ///
    /// The capture overlay's gear CONVERTS its own process into the settings pane (Linux)
    /// or hands off and tears the session down (macOS) — both end the capture. A preview
    /// editor is a multi-document HOST (DRAGON-336): converting or ending it would destroy
    /// every open document, including siblings handed to it by other processes. So the
    /// pane is always a SEPARATE detached child here, and this process keeps its documents.
    ///
    /// # Open vs. refocus
    ///
    /// Only one settings pane may exist across all instances, so the decision is:
    ///
    /// * this process already HAS a pane → focus it and do nothing else. Unreachable in
    ///   practice (a `--settings` launch never opens a preview, and the gear that converts
    ///   a capture into a pane runs before any preview exists), but cheap and honest to
    ///   handle rather than assume;
    /// * a pane is open ELSEWHERE ([`crate::instance::settings_pane_is_open`], a probe that
    ///   does not retain the lock) → spawn the `--focus-settings` helper, exactly as the
    ///   capture gear does, so the activation outlives the spawn;
    /// * otherwise → spawn a `--settings` child.
    ///
    /// The probe is TOCTOU-racy by nature and deliberately tolerated: if a pane appears in
    /// the gap, our `--settings` child finds the lock taken and (macOS/Windows) pokes the
    /// holder or (Linux) returns quietly — the same outcome the other branch aimed for.
    ///
    /// # The fullscreen overlay must come down first
    ///
    /// An `Exclusive` layer surface owns the keyboard and covers the display, so a settings
    /// window would open behind it and be unreachable. A document on the overlay is
    /// therefore DEMOTED to a window first, through the very same
    /// [`Self::demote_preview_to_window`] the second-document rule uses — which means it
    /// takes the sticky `demoted` pin too. That is the behaviour we want: the conversion is
    /// FORCED (by needing the screen), not chosen, so it must neither flip the persisted
    /// appearance nor let the document silently jump back to fullscreen while settings is
    /// up. The user's own appearance toggle still clears the pin, as always.
    ///
    /// The demotion task is batched FIRST so the exclusive surface is on its way out before
    /// the child is spawned.
    pub(super) fn open_settings_from_preview(
        &mut self,
        id: window::Id,
    ) -> Task<cosmic::Action<Msg>> {
        // Same-process pane (see above): just focus it, and leave the surfaces alone.
        if let Some(settings) = self.settings.window {
            return window::gain_focus(settings);
        }
        let demote = self.demote_preview_to_window(id);
        let flag = if crate::instance::settings_pane_is_open() {
            "--focus-settings"
        } else {
            "--settings"
        };
        crate::recording_ui::spawn_capture_child(flag);
        demote
    }

    /// Pause OTHER apps' media the instant a preview overlay opens — not when its asset finishes
    /// loading — for as long as the overlay stays up (resumed in `stop_preview_playback`, or when
    /// we exit). Only when the user wants it and the preview has sound (`with_audio`). The pause
    /// runs in a child process, so this is just a quick spawn: no D-Bus on our threads, no UI hitch.
    ///
    /// REFCOUNTED across preview documents (DRAGON-336 phase 2): `id` registers as a
    /// holder and the guard is engaged only for the FIRST one, so several video previews
    /// share one process-wide duck and the desktop stays muted until the LAST of them
    /// releases (see [`Self::release_preview_duck`]).
    pub(super) fn engage_preview_duck(&mut self, id: window::Id, with_audio: bool) {
        if !(self.mute_others_during_preview && with_audio) {
            return;
        }
        if let Some(p) = self.preview_for_mut(id) {
            p.ducking = true;
        }
        if self.preview_duck_refs.acquire(id) && self.preview_duck.is_none() {
            self.preview_duck = Some(crate::audio::ducking::OtherAudioDuck::engage());
        }
    }

    /// Release `id`'s hold on the "pause other apps' media" guard; the guard itself is
    /// dropped (other players resume) only when this was the LAST holder. Idempotent.
    pub(super) fn release_preview_duck(&mut self, id: window::Id) {
        if let Some(p) = self.preview_for_mut(id) {
            p.ducking = false;
        }
        self.preview_duck_refs.release(id);
        if !self.preview_duck_refs.held() {
            // The last holder let go — resume the other players.
            self.preview_duck = None;
        }
    }

    /// Open the preview overlay for `path` now (surface + content prep), on
    /// `preview_output`. `external` marks a pre-existing file (`--preview`). Returns the
    /// surface+content batch, or none if the output is unknown.
    pub(super) fn open_preview(
        &mut self,
        path: PathBuf,
        size: u64,
        is_video: bool,
        external: bool,
        dims: Option<(u32, u32)>,
    ) -> Task<cosmic::Action<Msg>> {
        let Some((output, monitor)) = self.preview_output.clone() else {
            // DRAGON-419 (silent-exit path S8, the near half). There is no display to open an
            // editor on, so the editor silently never appears — while the file IS on disk and
            // a notification may already have been posted. On macOS this means `output_descs()`
            // came back empty, i.e. ScreenCaptureKit stopped answering; on Linux, that no
            // output was ever seeded. It returns `Task::none()` rather than ending the
            // session, which is the one shape where "nothing happened" is literally true.
            crate::diag::note_failure(
                crate::diag::Failure::NoPreviewOutput,
                &format!(
                    "no display to open the preview editor on (outputs={}, video={is_video}, \
                     external={external}); the capture IS written",
                    self.outputs.len(),
                ),
            );
            return Task::none();
        };
        let kind = if is_video {
            PreviewKind::Video(VideoPreview::loading())
        } else {
            PreviewKind::Image(ImagePreview::loading())
        };
        let extra_h = transport_h_for(&kind, PreviewSurface::Window);
        let source_scale = self.preview_source_scale(Some(&output));
        let (id, open_task, monitor, surface) =
            self.preview_surface_for(None, Some(output), monitor, dims, extra_h);
        // The content prep is ADDRESSED to the surface we just minted, so its completion
        // lands on THIS document however many previews are open (DRAGON-336 phase 2).
        let task = if is_video {
            video::poster_task(id, path.clone())
        } else {
            image::decode_task(id, path.clone())
        };
        crate::util::timing_mark("preview: content prep task queued (decode / poster)");
        self.previews.push(PreviewState {
            window: id,
            surface,
            max_hint_pending: surface.is_window(),
            display_dims: dims,
            path: Some(path),
            size: Some(size),
            external,
            monitor,
            source_scale,
            loading_msg: random_loading_msg(),
            kind,
            edit: self.new_edit_state(),
            view: Viewport::default(),
            ducking: false,
            surface_open: true,
            toasts: Toasts::default(),
            copied_on_open: false,
            demoted: false,
            bake_src: None,
            saved_path: None,
        });
        // This is the in-flight capture's document; the capture flow addresses it by id.
        self.capture_preview = Some(id);
        self.focused_preview = Some(id);
        // Post-capture overlay just opened — pause other media now if this recording has audio.
        self.engage_preview_duck(id, is_video && (self.record_mic || self.record_system_audio));
        // DRAGON-353: the editor copies the capture to the clipboard as it opens (the old
        // "Automatically copy to clipboard" setting, now unconditional). The path is
        // already known on this path, so it starts right here — on a worker thread since
        // DRAGON-454, so it no longer stands between this call and the surface opening.
        let copy = self.auto_copy_preview_on_open(id);
        Task::batch([open_task, task, copy])
    }

    /// Open a preview for a pre-existing file (`--preview <file>`) on the *active* output
    /// (the monitor the user is on) — read-only toward the file: Save hidden, Cancel just
    /// exits, Save As copies. The monitor size starts as a placeholder and is corrected by
    /// the surface's resize event.
    pub(in crate::app) fn open_external_preview(
        &mut self,
        path: PathBuf,
        size: u64,
        is_video: bool,
    ) -> Task<cosmic::Action<Msg>> {
        let kind = if is_video {
            PreviewKind::Video(VideoPreview::loading())
        } else {
            PreviewKind::Image(ImagePreview::loading())
        };
        // Size the windowed preview to the file up front (the compositor won't shrink it
        // later): the picture's header dims (cheap read; video probes async, so unknown here)
        // against the largest known output as the monitor bound.
        let dims = if is_video {
            None
        } else {
            ::image::image_dimensions(&path).ok()
        };
        // In LOGICAL POINTS (DRAGON-449): there is no capture anchor here, so `mint_preview_window`
        // has no display to resolve a factor from and takes this bound as-is.
        let monitor = largest_output_points(&self.outputs).unwrap_or((1920, 1080));
        let extra_h = transport_h_for(&kind, PreviewSurface::Window);
        let (id, open_task, monitor, surface) =
            self.preview_surface_for(None, None, monitor, dims, extra_h);
        // Addressed to the surface just minted (see `open_preview`).
        let task = if is_video {
            video::poster_task(id, path.clone())
        } else {
            image::decode_task(id, path.clone())
        };
        self.previews.push(PreviewState {
            window: id,
            surface,
            max_hint_pending: surface.is_window(),
            display_dims: dims,
            path: Some(path),
            size: Some(size),
            external: true,
            monitor,
            // A `--preview` file is a plain viewer: its pixels ARE its own resolution,
            // with no capture backing-scale to undo, so present it 1:1 in logical points.
            source_scale: 1.0,
            loading_msg: random_loading_msg(),
            kind,
            edit: self.new_edit_state(),
            view: Viewport::default(),
            ducking: false,
            surface_open: true,
            toasts: Toasts::default(),
            copied_on_open: false,
            demoted: false,
            bake_src: None,
            saved_path: None,
        });
        // A `--preview` launch has exactly this one document; the capture flow's
        // deferred-open paths address it the same way a captured preview is addressed.
        self.capture_preview = Some(id);
        self.focused_preview = Some(id);
        // `--preview` is a viewer for an arbitrary file; we can't know if it has sound without
        // probing (the delay we're avoiding), so pause other media for any video as it opens.
        self.engage_preview_duck(id, is_video);
        Task::batch([open_task, task])
    }

    /// DRAGON-336 phase 3b — the HOST half of the preview handoff: drain every request other
    /// processes' capture children have posted, opening each as a NEW preview document.
    ///
    /// The ack is written in THIS SAME arm, only after the document is actually in
    /// `self.previews` — that is what makes the "host is exiting" race safe: a child
    /// concludes "accepted" only because a live host explicitly said so, and a host that
    /// dies (or drops the request) closes the connection unacked, which the child reads as
    /// "open your own preview". A request we cannot serve is REJECTED rather than dropped,
    /// so the child stops waiting immediately instead of burning `ACK_TIMEOUT`.
    #[cfg(unix)]
    pub(in crate::app) fn drain_preview_handoffs(&mut self) -> Task<cosmic::Action<Msg>> {
        // Take the whole batch off the channel first: opening a preview needs `&mut self`,
        // which can't be borrowed while `self.preview_host` is.
        let mut inbound = Vec::new();
        if let Some(host) = &self.preview_host {
            while let Ok(h) = host.requests.try_recv() {
                inbound.push(h);
            }
        }
        let mut tasks = Vec::new();
        for handoff in inbound {
            // Cloned so the borrow of `handoff` ends before it is consumed by accept/reject.
            let req = handoff.request().clone();
            match self.open_handoff_preview(&req) {
                Some(task) => {
                    log::info!("preview handoff: took ownership of {}", req.path.display());
                    tasks.push(task);
                    // Only NOW may the child exit — the document is in our state.
                    handoff.accept();
                }
                None => {
                    log::warn!(
                        "preview handoff: refusing {} — the child keeps it",
                        req.path.display()
                    );
                    handoff.reject(crate::preview_ipc::RejectReason::Busy);
                }
            }
        }
        Task::batch(tasks)
    }

    /// Off unix nothing can host (no transport), so there is never anything to drain — a
    /// stub keeping the `CaptureMsg::HandoffPoll` dispatch platform-uniform.
    #[cfg(not(unix))]
    pub(in crate::app) fn drain_preview_handoffs(&mut self) -> Task<cosmic::Action<Msg>> {
        Task::none()
    }

    /// Open a preview document for a capture HANDED OVER by another process.
    ///
    /// Deliberately NOT [`Self::open_preview`]: that one claims `capture_preview` (the
    /// IN-FLIGHT capture's document), which a handed-over file is not — this host may still
    /// have a capture of its own running, and clobbering that pointer would misdirect its
    /// cover→window swap and content prep. Everything else mirrors `open_preview`, fed from
    /// the wire fields (which carry the child's own `PreviewState` values under the same
    /// names) so the document is the one the child would have opened.
    ///
    /// `None` = we will not take it, and the caller must reject the handoff.
    ///
    /// DRAGON-427 made this PORTABLE (it was `#[cfg(unix)]` alongside the socket transport,
    /// though nothing in the body ever was). Windows 10's spawned editor child feeds the same
    /// `OpenRequest` in from its command line at startup, and it must land on the identical
    /// mapping — two transports that opened subtly different documents would be exactly the
    /// drift the wire format's strictness exists to prevent.
    pub(in crate::app) fn open_handoff_preview(
        &mut self,
        req: &crate::preview_ipc::OpenRequest,
    ) -> Option<Task<cosmic::Action<Msg>>> {
        // We share the filesystem with the child, but not necessarily its view of it: refuse
        // a path we cannot see rather than taking ownership of a document we could never
        // load. The child then opens its own preview, where the file certainly exists.
        if !req.path.is_file() {
            return None;
        }
        let kind = if req.video {
            PreviewKind::Video(VideoPreview::loading())
        } else {
            PreviewKind::Image(ImagePreview::loading())
        };
        // Place it where a fresh capture's preview would go when this session still knows
        // its capture monitor, else on the largest known output, else a placeholder the
        // first resize corrects (the `open_external_preview` fallback) — a long-lived host
        // may have finished its own capture long ago.
        // With a capture anchor the size is that display's CAPTURE space (the mint converts
        // it); without one it is already LOGICAL POINTS (DRAGON-449) — see
        // [`largest_output_points`] and the `open_external_preview` fallback it mirrors.
        let (output, monitor) = match self.preview_output.clone() {
            Some((o, m)) => (Some(o), m),
            None => (None, largest_output_points(&self.outputs).unwrap_or((1920, 1080))),
        };
        let extra_h = transport_h_for(&kind, PreviewSurface::Window);
        // `preview_surface_for` sizes through `self.preview_output_scale` — THIS process's
        // capture scale. A handed-over document carries the scale of the display IT was
        // grabbed from, so swap that in for the mint and put ours back afterwards (an
        // in-flight capture of our own keeps its value). Both sides are `1.0` on an unscaled
        // output, where the sizing math is unchanged.
        let saved_scale = self.preview_output_scale;
        self.preview_output_scale = req.source_scale;
        let (id, open_task, monitor, surface) =
            self.preview_surface_for(None, output, monitor, req.display_dims, extra_h);
        self.preview_output_scale = saved_scale;
        // Addressed to the surface just minted (see `open_preview`).
        let task = if req.video {
            video::poster_task(id, req.path.clone())
        } else {
            image::decode_task(id, req.path.clone())
        };
        self.previews.push(PreviewState {
            window: id,
            surface,
            max_hint_pending: surface.is_window(),
            display_dims: req.display_dims,
            path: Some(req.path.clone()),
            size: req.size,
            external: req.external,
            monitor,
            source_scale: req.source_scale,
            loading_msg: random_loading_msg(),
            kind,
            edit: self.new_edit_state(),
            view: Viewport::default(),
            ducking: false,
            surface_open: true,
            toasts: Toasts::default(),
            copied_on_open: false,
            demoted: false,
            bake_src: None,
            saved_path: None,
        });
        // NOT `capture_preview` (see above) — but it IS what the user just captured, so it
        // takes the keyboard-routing focus.
        self.focused_preview = Some(id);
        // Same rule as a locally-opened preview: pause other players for a recording with
        // sound (the audio toggles are persisted, so they read the same in both processes).
        // The child's own duck died with it; re-engaging here is refcounted, so a second
        // video document can't double-engage or un-mute the first.
        self.engage_preview_duck(id, req.video && (self.record_mic || self.record_system_audio));
        // A handed-over capture is still a capture the user just took: copy it on open,
        // exactly as a locally-opened one is (DRAGON-353).
        let copy = self.auto_copy_preview_on_open(id);
        Some(Task::batch([open_task, task, copy]))
    }

    /// DRAGON-336 phase 3b — the CHILD half: try to hand this finished capture to an
    /// ALREADY-RUNNING preview host instead of keeping this whole process alive (~233 MB of
    /// iced + wgpu + fonts) just to display it.
    ///
    /// `Some(task)` means the host POSITIVELY ACKNOWLEDGED ownership, so this one-shot
    /// process is done and the task ends the session. `None` means no handoff happened and
    /// the caller MUST open its own preview exactly as it always has.
    ///
    /// THE RULE IS "never lose the user's capture", so the fallback is the DEFAULT rather
    /// than an error path: every failure mode — no host, a host mid-exit, a wedged host, a
    /// version mismatch, an explicit refusal, a platform with no transport — arrives as
    /// `Err` from `send_to_host` and lands on `None`. Nothing is consumed without the ack,
    /// and the ack can only come from a host that already has the document in its state.
    fn try_handoff_capture(
        &mut self,
        path: &std::path::Path,
        size: u64,
        is_video: bool,
        dims: Option<(u32, u32)>,
    ) -> Option<Task<cosmic::Action<Msg>>> {
        // DRAGON-351: the handoff used to be gated on `instance::handoff_allowed` (the
        // "allow multiple capture instances" setting) so that with the setting OFF a fresh
        // capture kept COLLAPSING the existing preview instead of accumulating windows.
        // The setting is gone and multi-document preview is the behaviour, so the attempt
        // below is unconditional — the checks that remain are all about whether a handoff
        // is POSSIBLE, never whether it is permitted.
        //
        // No transport ⇒ nobody can be hosting (Windows): skip the discovery scan entirely
        // so that platform's capture path is byte-identical. Branching on the seam rather
        // than a `cfg` keeps this one code path on every OS.
        if !crate::preview_ipc::host_supported() {
            return None;
        }
        // NEVER hand off while we are ourselves hosting ANOTHER process's document: we are
        // the host in that case, and handing our own capture further down a chain would exit
        // this process and take the documents we accepted with it. (Our own in-flight
        // capture's preview — a pre-opened spinner — is not "another document".)
        if self.previews.iter().any(|p| Some(p.window) != self.capture_preview) {
            return None;
        }
        // Advisory fast-out for the common first-capture case: nobody is hosting, so don't
        // build a request at all. NEVER a substitute for the acknowledged exchange below —
        // a host can exit between these two lines, which is exactly why `send_to_host` is
        // the only thing that may conclude "handed off".
        crate::preview_ipc::host_pid()?;
        let req = crate::preview_ipc::OpenRequest {
            path: path.to_path_buf(),
            video: is_video,
            display_dims: dims,
            source_scale: self.preview_source_scale(None),
            external: false,
            size: Some(size),
        };
        match crate::preview_ipc::send_to_host(&req) {
            Ok(pid) => {
                log::info!(
                    "preview handoff: pid {pid} owns {} now — this process is done",
                    path.display()
                );
                // DRAGON-419 (silent-exit path S6). This is a SUCCESS, but from outside it is
                // indistinguishable from a silent self-close: the capture child vanishes and
                // whether an editor appears is now another process's problem. Recording it —
                // as an explicit non-loss — is what lets a reader stop looking here and go
                // look at pid N instead. The residual exposure the handoff's own module doc
                // names (a host that acks and then fails to present) is exactly this line
                // followed by no editor.
                crate::diag::note_failure(
                    crate::diag::Failure::HandoffAccepted,
                    &format!("host pid {pid} accepted the capture; this process is done"),
                );
                Some(self.finish_session())
            }
            Err(e) => {
                log::info!("preview handoff: opening our own preview instead ({e})");
                None
            }
        }
    }

    /// DRAGON-427 (Windows 10) — the SPAWN handoff: start the preview editor as its own
    /// process and end this one.
    ///
    /// **Why a whole process.** On Windows 10 this process is rendering in SOFTWARE, because
    /// that is the only way its overlays are translucent (`platform::win_build_software_overlays`).
    /// iced binds ONE compositor for a process's whole life, chosen at the first window, so
    /// an editor minted here would inherit that rasterizer — and the editor draws its media
    /// through `iced::widget::shader`, which tiny-skia cannot render at all. The two surfaces
    /// genuinely cannot share a process.
    ///
    /// That is not merely tidier for the picker flow; it is REQUIRED by the picker-free
    /// captures (`--active-window` / `--active-monitor`), where a fullscreen loader that must
    /// be transparent is followed immediately by an editor that must not be, with no selector
    /// in between and no ordering trick available.
    ///
    /// Unlike the socket handoff there is no host to acknowledge us — we are creating the
    /// peer, not finding one — so the proof of life is narrower by construction: a spawn that
    /// the OS refused. `Some(task)` means a child process exists and this one is done;
    /// `None` means the caller must deliver the capture some other way. The caller's `None`
    /// branch is deliberately NOT "open our own preview" here (see [`Self::present_capture`]).
    #[cfg(windows)]
    fn try_spawn_editor_child(
        &mut self,
        path: &std::path::Path,
        size: u64,
        is_video: bool,
        dims: Option<(u32, u32)>,
    ) -> Option<Task<cosmic::Action<Msg>>> {
        // Windows 11 renders everything on wgpu and keeps its single-process editor exactly
        // as it ships today. Only the software-rendered band splits.
        if !crate::platform::software_overlays() {
            return None;
        }
        let req = crate::preview_ipc::OpenRequest {
            path: path.to_path_buf(),
            video: is_video,
            display_dims: dims,
            source_scale: self.preview_source_scale(None),
            // The capture is OURS — a file we wrote, which the editor may manage, save in
            // place and discard. `--preview`'s viewer semantics (`external = true`) would
            // quietly refuse all of that.
            external: false,
            size: Some(size),
        };
        match crate::platform::windows::process::spawn_editor_child(&req.encode()) {
            Ok(pid) => {
                log::info!(
                    "windows 10 preview handoff: spawned editor child pid {pid} for {} \
                     — this process is done",
                    path.display()
                );
                // The same classification the socket handoff records (DRAGON-419 path S6):
                // a SUCCESS that from outside looks like a silent self-close, so the log has
                // to say which pid to go and look at.
                crate::diag::note_failure(
                    crate::diag::Failure::HandoffAccepted,
                    &format!("spawned editor child pid {pid}; this process is done"),
                );
                Some(self.finish_session())
            }
            Err(e) => {
                log::warn!("windows 10 preview handoff: could not spawn the editor child ({e})");
                None
            }
        }
    }

    /// Off Windows nothing spawns an editor child — every platform keeps its historical
    /// single-process preview. A stub so [`Self::present_capture`] stays one code path.
    #[cfg(not(windows))]
    #[allow(clippy::unused_self)]
    fn try_spawn_editor_child(
        &mut self,
        _path: &std::path::Path,
        _size: u64,
        _is_video: bool,
        _dims: Option<(u32, u32)>,
    ) -> Option<Task<cosmic::Action<Msg>>> {
        None
    }

    /// Update the open preview's monitor size when its surface is (re)sized — needed for
    /// `--preview`, which opens on the active output before its size is known. Also the
    /// windowed preview's post-map hook: the first configure means the surface is mapped,
    /// so the transient `max_size` hint (only there to disarm cosmic-comp's initial-map
    /// 2/3 reshape — see [`super::shell::preview_window`]) is cleared and the user
    /// resizes freely from here on.
    pub(in crate::app) fn preview_resized(&mut self, id: window::Id, w: u32, h: u32) -> Task<cosmic::Action<Msg>> {
        if w == 0 || h == 0 {
            return Task::none();
        }
        if self.preview_for(id).is_none() {
            return Task::none();
        }
        // Windows (DRAGON-446): hold the windowed preview's CLIENT floor, exactly as the settings
        // window's `ConfigWindowResized` arm does. Same cause: winit hands our logical `min_size`
        // to `WM_GETMINMAXINFO` as `ptMinTrackSize` verbatim, but that is an OUTER minimum, and on
        // Win11 the DRAGON-284 caption subclass carves a real 16x8 band out of the client — so a
        // drag could bottom out 16x8 UNDER `PREVIEW_MIN_W`/`PREVIEW_MIN_H` and clip the toolbars
        // the floor exists to protect. `caption_subproc` now raises `ptMinTrackSize` by that carve;
        // this is the app-level net behind it. `enforce_client_floor` measures the band itself,
        // caps the floor at what the monitor can hold (mirroring `preview_window`'s own `min_size`
        // clamp on small displays), raises only the short axis, and leaves the window where it is.
        //
        // WINDOW surface only — the OVERLAY preview is fullscreen and has no floor to hold. And
        // only with ONE document open: the native helper resolves by TITLE and every preview
        // window carries the same one, so with siblings (DRAGON-336) it could grow the wrong
        // window. The `ptMinTrackSize` fix is per-HWND and covers those correctly regardless.
        //
        // mac/Linux enforce inner minimums correctly, so they are untouched.
        #[cfg(windows)]
        if ((w as f32) < crate::app::shell::PREVIEW_MIN_W
            || (h as f32) < crate::app::shell::PREVIEW_MIN_H)
            && self.previews.len() == 1
            && self.preview_for(id).is_some_and(|p| p.surface.is_window())
        {
            crate::platform::windows::window::enforce_client_floor(
                crate::app::shell::PREVIEW_WINDOW_TITLE,
                (
                    crate::app::shell::PREVIEW_MIN_W as u32,
                    crate::app::shell::PREVIEW_MIN_H as u32,
                ),
            );
        }
        // This configure is the WINDOW's FIRST map iff the transient max-size hint was still
        // pending (set at mint, cleared here exactly once) — the signal DRAGON-317's re-home
        // below piggybacks on (Linux-only, so gated to stay warning-clean elsewhere).
        #[cfg(target_os = "linux")]
        let first_window_configure =
            self.preview_for(id).is_some_and(|p| p.max_hint_pending);
        let clear_hint = match self.preview_for_mut(id) {
            Some(p) if p.max_hint_pending => {
                p.max_hint_pending = false;
                window::set_max_size(id, None)
            }
            _ => Task::none(),
        };
        // DRAGON-317 (Linux windowed): the first map of the preview WINDOW. cosmic-comp maps a
        // fresh xdg_toplevel on the seat's ACTIVE (pointer) output (`Shell::map_window` →
        // `seat.active_output()`, updated on pointer motion) — and an xdg_toplevel cannot
        // self-place, so the ONLY client-side lever to relocate it is the toplevel-management
        // `move_to_ext_workspace` request (see `platform::compositor::move_toplevel_to_output`).
        //
        // The move target is `preview_output_name`, cached PRE-TEARDOWN (`self.outputs` is
        // EMPTIED by `destroy_surfaces` before this configure fires). Its value is the RELIABLE
        // capture-origin monitor: the CURSOR's output. The interactive picker learns it from the
        // capture overlay's first pointer-enter (`capture_pointer_output`); an overlay-less
        // IMMEDIATE capture (`--active-*`) resolves the SAME field from the momentary
        // `pointer_output()` probe. So for immediate captures the move now FIRES, targeting the
        // cursor's monitor — a no-op via the helper's own "already on target output" skip
        // whenever cosmic-comp already mapped the fresh window on the pointer's output (the
        // common case). It stays None only for `--preview` (no capture) or a probe/overlay miss,
        // where we SUPPRESS the move and leave cosmic-comp's native pointer-output placement
        // untouched. The DRAGON-317 regression was setting this target from the launch
        // FOCUSED-TOPLEVEL guess: when the focused window sat on a different, empty-of-the-user
        // monitor, the move dragged the correctly-placed preview there. Do NOT reintroduce a
        // focused-toplevel/primary fallback for the move target.
        // CAVEAT: the move preserves floating ONLY if the destination workspace isn't
        // auto-tiling (see the helper's doc); an auto-tiling workspace force-tiles the moved
        // window regardless — the DRAGON-222 blocker. The OVERLAY preview is untouched (it
        // already pins its output at layer-surface creation).
        #[cfg(target_os = "linux")]
        if first_window_configure {
            match self.preview_output_name.clone() {
                // A RELIABLE capture-origin monitor — the cursor's output (overlay pointer-enter
                // for the picker, or the `pointer_output()` probe for an immediate capture):
                // re-home the freshly-mapped window there. The helper no-ops when the window
                // already mapped on that output (the common single-/same-monitor case).
                Some(name) => {
                    let title = super::super::shell::PREVIEW_WINDOW_TITLE.to_string();
                    std::thread::spawn(move || {
                        crate::platform::compositor::move_toplevel_to_output(&title, &name);
                    });
                }
                // No reliable origin (`--preview`, or the cursor output never resolved):
                // SUPPRESS the move and leave cosmic-comp's native pointer-output placement
                // (already where the user is) untouched. Do NOT fall back to the
                // focused-toplevel/primary output here — that fallback WAS the DRAGON-317
                // regression (the preview flew onto the focused window's monitor instead of the
                // one the user was on).
                None => {
                    log::debug!(
                        "preview re-home suppressed: no reliable capture-origin output \
                         (keeping cosmic-comp's native pointer-output placement)"
                    );
                }
            }
        }
        // The stored zoom is FIT-relative, so a plain resize would change the displayed size
        // while the % readout stayed put. Preserve the native scale across the resize (except
        // "Fit to screen", which re-fits to the new width) so 100% stays 100%.
        let old_fit = self.preview_for(id).map(|p| self.preview_fit_scale(p)).unwrap_or(1.0);
        let old_native = self.preview_for(id).map(|p| p.view.zoom).unwrap_or(1.0) * old_fit;
        if let Some(p) = self.preview_for_mut(id) {
            p.monitor = (w, h);
        }
        let fit_to_screen = self.preview_for(id).is_some_and(|p| p.view.zoom_preset == Some(0));
        let new_fit = self.preview_for(id).map(|p| self.preview_fit_scale(p)).unwrap_or(1.0);
        // DRAGON-401: preserving the NATIVE scale raises the fit-relative zoom whenever the
        // window shrinks, so this path writes a zoom too. It is size-preserving by
        // construction (the media's physical extent is what is being held constant), but it
        // reaches `set_zoom` — which knows only the backstop — so it takes the device ceiling
        // like every other writer rather than relying on that argument staying true.
        let maxz = self.preview_for(id).map(|p| self.max_view_zoom(p)).unwrap_or(Viewport::MAX);
        if let Some(p) = self.preview_for_mut(id) {
            // "Fit to screen" re-fits (whole picture, zoom 1.0); any other zoom keeps its
            // native scale across the resize.
            let z = if fit_to_screen {
                Viewport::FIT
            } else {
                old_native / new_fit.max(0.0001)
            };
            p.view.set_zoom(z.min(maxz));
        }
        // DRAGON-216 (Linux windowed swap): this configure is the swapped-in window's FIRST
        // map (it's now `p.window`), so the neutral overlay that covered the grab has served
        // its purpose — close it. The window mapped UNDER the cover, so closing it now reveals
        // the window with no desktop flash. (macOS closes the cover from
        // `finalize_preview_window` instead — its first `Resized` can fire before the native
        // placement, which would drop the cover too early; DRAGON-219.)
        #[cfg(target_os = "linux")]
        if let Some(overlay) = self.grab_overlay_closing.take() {
            return Task::batch([clear_hint, super::super::shell::close_surface(overlay)]);
        }
        clear_hint
    }

    /// Re-focus the preview WINDOW (windowed mode only). No-op for the overlay (a layer
    /// surface with its own exclusive keyboard grab).
    pub(super) fn focus_preview_window(&self, id: window::Id) -> Task<cosmic::Action<Msg>> {
        if let Some(p) = self.preview_for(id)
            && p.surface.is_window()
        {
            return window::gain_focus(p.window);
        }
        Task::none()
    }

    /// Route a finished capture: show it in the preview overlay (when enabled), else fall
    /// back to the immediate copy/notify/exit share.
    pub(in crate::app) fn present_capture(
        &mut self,
        path: PathBuf,
        size: u64,
        is_video: bool,
        dims: Option<(u32, u32)>,
    ) -> Task<cosmic::Action<Msg>> {
        // DRAGON-454: the head of the editor's launch timeline — the capture is on disk and a
        // route is about to be chosen. Everything after this mark is the stretch the user
        // experiences as "the editor opening".
        crate::util::timing_mark("preview: present_capture (capture written; choosing a route)");
        // DRAGON-428: this launch asked for no editor (`--no-editor`, or a daemon
        // "(no editor)" capture hotkey). Deliver through `finish_share` — the SAME
        // editor-less path a capture takes when no editor can be opened at all — so there
        // is exactly one no-editor delivery in the app rather than a second one that could
        // drift from it.
        //
        // It has to sit HERE, above everything else, because all three routes below open an
        // editor of some kind: the handoff hands the file to a running preview HOST, the
        // Windows 10 branch SPAWNS an editor child, and the pre-opened-spinner branch fills
        // a surface that is already up. Checking any lower would still produce an editor.
        //
        // Read, not taken: this describes the launch, so a second capture in the same
        // process (there is none today) would honour it too.
        //
        // DRAGON-479 ORs in the region-copy chord's one-shot override rather than adding a
        // second early return, so the two ways of asking for an editor-less capture reach the
        // one delivery through the one decision. It is TAKEN (not read): it describes this
        // capture only, and a later capture in the same process behaves normally again.
        let one_shot = std::mem::take(&mut self.copy_selection_pending);
        if self.no_editor || one_shot {
            return self.finish_share(&path, size, is_video);
        }
        // DRAGON-353: the editor is the capture's destination unconditionally (the "Open in
        // preview editor" setting is gone); the preview-less `finish_share` fallback below
        // now fires ONLY when no editor can be opened at all (no known output).
        // DRAGON-336 phase 3b: a preview is what this capture wants — so before minting one
        // HERE, offer the finished file to an already-running preview host. On a positive
        // ack the host owns it and this one-shot process ends right now (the whole memory
        // saving: one preview process, several documents). On ANY other outcome we fall
        // through and open our own preview exactly as before — see `try_handoff_capture`,
        // where that fallback is the default rather than an error path.
        if let Some(done) = self.try_handoff_capture(&path, size, is_video, dims) {
            return done;
        }
        // DRAGON-427 (Windows 10): the editor cannot live in THIS process — it is rendering
        // in software so its overlays are translucent, and the editor cannot draw at all
        // under that rasterizer. Spawn it as its own GPU-rendered process and end here.
        //
        // Inert everywhere else, and on Windows 11 (which keeps its single-process editor).
        if crate::platform::software_overlays() {
            if let Some(done) = self.try_spawn_editor_child(&path, size, is_video, dims) {
                return done;
            }
            // The spawn failed. Falling through would open the editor HERE — visibly broken,
            // because tiny-skia cannot render its shader layers. So take the app's existing
            // editor-less delivery instead: copy to the clipboard, notify, exit. The capture
            // is already written to disk at this point, so nothing is lost — the user gets the
            // file and the clipboard, just no editor. `finish_share` routes through
            // `finish_session` like every other "we're done" path.
            // Not a `diag::note_failure`: this delivers, so it is not a "we delivered
            // NOTHING" path. It is loud in the log because it is a degraded experience the
            // user WILL notice (no editor at all) and a reader needs the reason.
            log::warn!(
                "windows 10: no editor child, delivering {} without the preview editor",
                path.display()
            );
            return self.finish_share(&path, size, is_video);
        }
        // Pre-opened (window/freeze grab, or a stopped recording): the spinner overlay is
        // already up — record where the capture landed and kick off the content prep into
        // the existing surface (image decode, or video poster extraction).
        if let Some(id) = self.capture_preview {
            if let Some(p) = self.preview_for_mut(id) {
                p.path = Some(path.clone());
                p.size = Some(size);
            }
            // A PRE-OPENED spinner had no path when it opened, so the open-time automatic
            // clipboard copy (DRAGON-353) waits for this moment — the file has just landed.
            // Idempotent, so the swap below can't double-copy.
            //
            // THIS is the seam DRAGON-454 was reported against: the editor is ALREADY on
            // screen here, so running the copy inline froze a surface the user was looking at.
            // It is a worker thread now, and its outcome arrives as `AutoCopied`.
            let copy = self.auto_copy_preview_on_open(id);
            // DRAGON-221 follow-up: a WINDOWED window-pick deferred its cover→window
            // swap to THIS moment, when the COMPOSED dims are finally known (the
            // selection dims under-size the window — padding/shadow/wallpaper margins
            // grew the picture — and a post-open resize is not honored on COSMIC).
            // Feed the composed dims into the sizing state, mint the real window at
            // its correct size, and prep the content into it.
            let swap = if std::mem::take(&mut self.windowed_swap_pending) {
                if let (Some(d), Some(p)) = (dims, self.preview_for_mut(id)) {
                    p.display_dims = Some(d);
                }
                self.swap_neutral_spinner_to_window()
            } else {
                Task::none()
            };
            // The swap may have re-minted the surface, so address the content prep at the
            // document's CURRENT id (`capture_preview` followed it through `repoint_preview`).
            let id = self.capture_preview.unwrap_or(id);
            let prep = match self.preview_for(id).map(|p| &p.kind) {
                Some(PreviewKind::Image(_)) => image::decode_task(id, path),
                Some(PreviewKind::Video(_)) => video::poster_task(id, path),
                None => Task::none(),
            };
            return Task::batch([swap, prep, copy]);
        }
        // Not pre-opened (live region/monitor image grab, which needs a clean screen):
        // open the overlay now (spinner), then prep the content. The caller's `dims` let the
        // windowed preview open already sized to the picture (the compositor won't shrink it
        // after the fact).
        if self.preview_output.is_some() {
            return self.open_preview(path, size, is_video, false, dims);
        }
        self.finish_share(&path, size, is_video)
    }

    /// The preview overlay: a spinner while loading, then the media-specific content
    /// (image or video poster) with its action bar, centred on a dimmed full-screen
    /// surface.
    pub(in crate::app) fn preview_view(&self, id: window::Id) -> Element<'_, Msg> {
        let Some(preview) = self.preview_for(id) else {
            return widget::space::Space::new()
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
        };
        // Shrink the toolbar buttons in the windowed preview (its smaller window wants
        // tighter chrome) by 18%; full size for the fullscreen overlay. Built once and
        // threaded explicitly into every builder below (the button helpers take it).
        // Frosted glass (DRAGON-217): the WINDOWED preview's chrome paints translucent
        // so the compositor blur (enrolled on the window surface) shows through the
        // toolbars, titlebar, and image-background margins. The fullscreen overlay is a
        // layer-shell surface and is NEVER frosted (DRAGON-166), so glass is None there.
        let glass = if preview.surface.is_window() { self.glass } else { None };
        // Suppress every toolbar tooltip while a flyout (the color palette or the covermark
        // picker) is open, so a hovered tooltip never draws split across the flyout (an
        // overlay layer-batching artifact). Normal tooltips otherwise.
        let suppress_tooltips =
            preview.edit.flyout.is_some() || preview.edit.annot_picker.is_some();
        // DRAGON-478: the top bar's group captions, from the setting. The SAME flag every
        // sizing path hands `PreviewSurface::chrome_h`, so what the bar draws and what the
        // layout reserved can never disagree.
        let tb = Tb {
            pid: preview.window,
            scale: preview.surface.btn_scale(),
            glass,
            suppress_tooltips,
            labels: self.preview_toolbar_labels,
        };
        // The document's toasts (DRAGON-353), built once and handed to whichever content
        // branch draws. They anchor to the MEDIA area, so the builders own the placement
        // rather than this function stacking them over the whole surface (which would cover
        // the chrome too).
        //
        // (No closing FADE rides along here any more — see the removal note above
        // `compose_preview` in `chrome.rs`: a dissolve is not expressible in this iced, and a
        // scrim standing in for one was rejected.)
        let toasts = toast::toast_layer(&preview.toasts, glass);
        // DRAGON-454: one entry on the launch timeline per view build (bounded) carrying the
        // build's own cost. Both halves matter — the GAP between consecutive builds is where a
        // blocked UI thread shows up, and the COST says whether the block is this build.
        let _build_mark = mark_preview_view(!preview.is_loading());
        let content: Element<'_, Msg> = if preview.is_loading() {
            self.preview_loading_view(preview, tb, toasts)
        } else {
            match &preview.kind {
                PreviewKind::Image(img) => self.image_loaded_view(preview, img, tb, toasts),
                PreviewKind::Video(vid) => self.video_loaded_view(preview, vid, tb, toasts),
            }
        };

        let base: Element<'_, Msg> = if preview.surface.is_window() {
            // A real window with CLIENT-side decorations (like the settings window): our own
            // header bar drawn into the surface (so a window capture keeps the title bar),
            // over the editor content, wrapped in the framework's rounded background + hairline
            // border. The transparent surface only shows through outside the rounded corners.
            let focused = self.core.focused_window() == Some(preview.window);
            let header = widget::header_bar()
                .title(super::shell::PREVIEW_WINDOW_TITLE)
                .focused(focused)
                .on_drag(Msg::Preview(id, PreviewMsg::WindowDrag))
                .on_double_click(Msg::Preview(id, PreviewMsg::WindowMaximize));
            // DRAGON-337: the fullscreen-overlay toggle + undo / redo live in the TITLEBAR
            // (space in the toolbars was getting cramped), styled exactly like the settings
            // window's collapse / search controls. macOS: reserve the native traffic lights'
            // leading width first — collapsed to 0 in NATIVE fullscreen, where the lights
            // auto-hide, exactly as `config_window_view` does.
            #[cfg(target_os = "macos")]
            let header = header.start(widget::Space::new().width(Length::Fixed(
                if self.preview_fullscreen {
                    0.0
                } else {
                    crate::app::settings::TRAFFIC_LIGHTS_INSET
                },
            )));
            let header = self
                .preview_header_controls(preview)
                .into_iter()
                .fold(header, |h, control| h.start(control));
            // macOS (DRAGON-146): native traffic lights own close/min/zoom (kept +
            // centred in `finalize_preview_window`), so no CSD window buttons here;
            // native close routes through WindowCloseRequested → preview Cancel.
            // Windows (DRAGON-284): the native DWM caption buttons (installed post-show in
            // `finalize_preview_window`) likewise own close/minimize/maximize, so OMIT the
            // CSD buttons and reserve the trailing `WIN_CAPTION_INSET` spacer so the header
            // content clears the cluster. Native ✕ → WindowCloseRequested → preview Cancel;
            // the header double-click → the same native `toggle_maximize` as the max button.
            // DRAGON-403: only where DWM actually PAINTS those native buttons — Win11 (build
            // 22000+). On Windows 10 they hit-test but never render, so fall back to the CSD
            // buttons this window used before DRAGON-284 (their messages already route to the
            // native `toggle_maximize` / `minimize` helpers, DRAGON-258); the DWM recipe is
            // skipped under the same gate so nothing swallows their clicks. The Win11 branch
            // is the byte-identical builder chain.
            #[cfg(windows)]
            let header = if crate::platform::windows::caption::native_caption_buttons_supported() {
                header.end(
                    widget::Space::new()
                        .width(Length::Fixed(crate::app::settings::WIN_CAPTION_INSET)),
                )
            } else {
                header
                    .on_close(Msg::Preview(id, PreviewMsg::Cancel))
                    .on_maximize(Msg::Preview(id, PreviewMsg::WindowMaximize))
                    .on_minimize(Msg::Preview(id, PreviewMsg::WindowMinimize))
            };
            #[cfg(all(not(target_os = "macos"), not(windows)))]
            let header = header
                .on_close(Msg::Preview(id, PreviewMsg::Cancel))
                .on_maximize(Msg::Preview(id, PreviewMsg::WindowMaximize))
                .on_minimize(Msg::Preview(id, PreviewMsg::WindowMinimize));
            // The editor fills the space below the header; centre it so the loading spinner
            // sits in the middle (the loaded view already fills, so centring is a no-op there).
            let body = widget::container(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill);
            let stacked = widget::column(vec![header.into(), body.into()])
                .width(Length::Fill)
                .height(Length::Fill);
            // Frosted glass (DRAGON-217): the windowed preview's background paints
            // translucent so the compositor blur enrolled on this surface (shell.rs)
            // shows through the chrome. No-op (opaque, today's look) off COSMIC / when
            // frosted windows are off. The fullscreen OVERLAY branch below is a
            // layer-shell surface and is NEVER frosted (DRAGON-166).
            let glass = self.glass;
            widget::container(stacked)
                .padding(1)
                .width(Length::Fill)
                .height(Length::Fill)
                .class(cosmic::theme::Container::custom(move |theme| {
                    let cosmic = theme.cosmic();
                    // Windows (DRAGON-276) + macOS: the compositor / window server rounds the
                    // window and clips the content to it; don't double-round the outer container
                    // (thick corner) or draw a 1px app border (which reads as a corner fringe).
                    // Fill SQUARE and draw no border; the OS owns the edge. Linux keeps the
                    // app-drawn radius + border (its window edge is the app's). See the matching
                    // note in `settings::config_window_view`.
                    #[cfg(target_os = "linux")]
                    let radius = crate::app::theme::rounding(theme).window();
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(crate::app::theme::frost_color(
                            cosmic.background.base.into(),
                            glass,
                        ))),
                        border: Border {
                            color: cosmic.bg_divider().into(),
                            #[cfg(target_os = "macos")]
                            width: 0.0,
                            #[cfg(not(target_os = "macos"))]
                            width: 1.0,
                            #[cfg(not(target_os = "linux"))]
                            radius: 0.0.into(),
                            #[cfg(target_os = "linux")]
                            radius: radius.into(),
                        },
                        ..Default::default()
                    }
                }))
                .into()
        } else {
            // Overlay: dim the whole monitor (opacity from settings); centre the content.
            let dim = self.preview_overlay_opacity;
            widget::container(content)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .padding(40)
                .width(Length::Fill)
                .height(Length::Fill)
                .class(cosmic::theme::Container::custom(move |_theme| {
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(cosmic::iced::Color::from_rgba(
                            0.0, 0.0, 0.0, dim,
                        ))),
                        ..Default::default()
                    }
                }))
                .into()
        };
        // ── The editor's own overlays, bottom to top (DRAGON-353) ────────────────────
        // Identical in BOTH appearances: they stack over `base`, which is already the
        // whole composed editor for whichever surface this document lives on, so the
        // fullscreen overlay and the CSD window get the same behaviour from one builder.
        let mut layers: Vec<Element<'_, Msg>> = vec![base];
        // A bake / export is committing edits — dim the editor behind the spinner (which
        // also reads as "your input is being held", which it is).
        if preview.edit.baking {
            layers.push(self.processing_overlay(preview));
        }
        // (The toast layer is NOT stacked here — it anchors inside the media area, so the
        // content builders above own it. See `toast::toast_layer`.)
        // The modal unsaved-changes card is topmost: it must be reachable over the
        // overlay's exclusive keyboard grab as well as in a window.
        if preview.edit.confirm_close {
            layers.extend(self.unsaved_dialog(id, preview));
        }
        if layers.len() == 1 {
            // Nothing stacked — hand back the bare editor so the common case adds no
            // wrapper at all (the stack widget is not free, and this is every frame).
            return layers.remove(0);
        }
        cosmic::iced::widget::stack(layers).into()
    }

    /// The PROCESSING overlay (DRAGON-353): a dimming scrim over the editor carrying the
    /// same spinner the load state uses, plus one of [`PREVIEW_PROCESSING_MESSAGES`].
    ///
    /// This replaced the desktop "Processing capture" notification, which only existed
    /// because the editor used to VANISH for the duration of a bake. It stays up now, so
    /// the progress belongs in it. The spinner is INDETERMINATE on purpose: the work is an
    /// `ffmpeg`/encode run behind a blocking call on a worker thread with no progress
    /// channel back, so a progress BAR could only lie. (Wiring one would mean parsing
    /// ffmpeg's `-progress` stream for videos and instrumenting the image bake — a real
    /// feature, not a view change.)
    ///
    /// It swallows stray presses so a click during the bake can't reach the chrome
    /// underneath; the update loop already holds every input behind `edit.baking`, and
    /// this makes that visible rather than mysterious.
    fn processing_overlay(&self, preview: &PreviewState) -> Element<'_, Msg> {
        let msg = PREVIEW_PROCESSING_MESSAGES
            [preview.edit.processing_msg % PREVIEW_PROCESSING_MESSAGES.len()];
        let status = widget::column(vec![
            widget::indeterminate_circular().size(56.0).into(),
            widget::text(msg).size(16).into(),
        ])
        .spacing(20.0)
        .align_x(Alignment::Center);
        widget::mouse_area(
            widget::container(status)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .width(Length::Fill)
                .height(Length::Fill)
                .class(cosmic::theme::Container::custom(|_t| {
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(crate::app::theme::SCRIM)),
                        ..Default::default()
                    }
                })),
        )
        .on_press(Msg::WindowChrome(WindowChromeMsg::Ignore))
        .interaction(cosmic::iced::mouse::Interaction::Idle)
        .into()
    }

    /// The UNSAVED-CHANGES modal (DRAGON-353): raised when a close is attempted with edits
    /// that have never been baked into a file. Returns the backdrop + the centred card, to
    /// be stacked over the editor by [`Self::preview_view`].
    ///
    /// **EXACTLY THREE OPTIONS** (user decision after testing DRAGON-467), and the card is
    /// the question "you are leaving with unsaved edits, what now?" answered completely:
    ///
    /// * **Save** — the ordinary save-then-close (`share_then_close` → the destination
    ///   picker). Cancelling the picker returns to the editor with the close disarmed.
    /// * **Continue editing** — dismiss the card and stay.
    /// * **Close without saving** — the discard route, which still honours "Automatically
    ///   copy changes on exit": "without saving" is about the DISK, and that setting is about
    ///   the clipboard.
    ///
    /// The card used to mirror the whole action bar (Save / Save As / Copy / Delete). Save As
    /// went when Save became the picker, and Copy and Delete go here: Delete no longer exists
    /// anywhere in the editor, and Copy was never an answer to "what about your unsaved
    /// edits" — it neither saves them nor discards them, so offering it as a way OUT of the
    /// card invited a close that quietly dropped work the user thought they had dealt with.
    /// Anyone who wants a copy can take one from the toolbar and then leave, and the exit
    /// setting does it for them anyway.
    ///
    /// Rendered in-app rather than as a real dialog window so it is clickable over the
    /// fullscreen overlay's exclusive keyboard grab as well as inside the CSD window. The
    /// layout is deliberately SHORT — a title, one line, and two button rows — because the
    /// overlay-mode card must fit inside `PREVIEW_MIN_H` (545px) with the editor's chrome
    /// already claiming `PreviewSurface::chrome_h`.
    fn unsaved_dialog<'a>(
        &'a self,
        id: window::Id,
        preview: &'a PreviewState,
    ) -> Vec<Element<'a, Msg>> {
        let backdrop: Element<'a, Msg> = widget::mouse_area(
            widget::container(widget::Space::new().width(Length::Fill).height(Length::Fill))
                .width(Length::Fill)
                .height(Length::Fill)
                .class(cosmic::theme::Container::custom(|_t| {
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(crate::app::theme::SCRIM)),
                        ..Default::default()
                    }
                })),
        )
        // Swallow; never dismiss on a stray click — a mis-click must not decide the fate
        // of unsaved work.
        .on_press(Msg::WindowChrome(WindowChromeMsg::Ignore))
        .interaction(cosmic::iced::mouse::Interaction::Idle)
        .into();

        // ONE action: Save. The other two ways out are the buttons below (Continue editing /
        // Close without saving), which together with this make the three options the card
        // offers. See this function's doc for why Copy and Delete are not among them.
        let actions: Vec<Element<'a, Msg>> =
            vec![dialog_action("document-save-symbolic", "Save", PreviewMsg::SaveAndClose, id, None)];

        // A FAILED attempt turns this card into the failure report (DRAGON-353 follow-up):
        // same four actions (so the top row IS the retry), but the wording says what went
        // wrong and the way out is renamed to what it now means — leaving KNOWING the save
        // did not happen. One extra confirmation of an informed intent, not a new route:
        // it still fires `DiscardAndClose`.
        let failure = preview.edit.close_error.as_deref();
        let (title, body, leave) = match failure {
            Some(reason) => ("Couldn't finish that", reason, "Exit anyway"),
            None => (
                "Unsaved changes",
                "Your edits haven't been written to a file yet. Save them, keep working, or \
                 close and let them go.",
                "Close without saving",
            ),
        };

        let choices: Element<'a, Msg> = widget::row(vec![
            crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::suggested("Continue editing")
                    .on_press(Msg::Preview(id, PreviewMsg::KeepEditing)),
            ),
            crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::destructive(leave)
                    .on_press(Msg::Preview(id, PreviewMsg::DiscardAndClose)),
            ),
        ])
        .spacing(8.0)
        .into();

        // The reason is drawn in the theme's DANGER colour so a failure can never be
        // mistaken for the ordinary "you have unsaved work" prompt.
        let body_text = widget::text(body.to_string()).size(13);
        let body_el: Element<'a, Msg> = if failure.is_some() {
            body_text
                .class(cosmic::theme::Text::Custom(|t| cosmic::iced::widget::text::Style {
                    color: Some(crate::app::theme::danger(t)),
                    ..Default::default()
                }))
                .into()
        } else {
            body_text.into()
        };

        let col: Vec<Element<'a, Msg>> = vec![
            widget::text::title4(title).into(),
            body_el,
            widget::row(actions).spacing(8.0).into(),
            choices,
        ];
        let card = widget::container(widget::column(col).spacing(16.0))
            .padding(24.0)
            .max_width(460.0)
            .class(cosmic::theme::Container::custom(|theme| {
                let c = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(c.background.base.into())),
                    border: Border {
                        radius: crate::app::theme::rounding(theme).m.into(),
                        width: 1.0,
                        color: c.background.divider.into(),
                    },
                    ..Default::default()
                }
            }));
        let centered: Element<'a, Msg> = widget::container(card)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        vec![backdrop, centered]
    }

    /// Loading state: a centred spinner + a playful status line (picked from the kind's
    /// message set), with the close (x) cancel group below so the wait is cancellable — this
    /// portable view is what mac (its existing X affordance) and Windows both show, so the SAME
    /// X abort is present on both platforms. The X fires `PreviewMsg::Cancel` — the SAME abort
    /// the Esc hotkey triggers (`keyboard.rs`: `Action::PreviewCancel -> PreviewMsg::Cancel`),
    /// which routes through `finish_session` for a clean one-shot teardown (focus / menu-bar /
    /// window-order state restored, then exit). It stays clickable even while the window
    /// focus-then-grab is slow/hung because that grab runs OFF the UI thread
    /// (`window_focus_grab`'s `Send` closure on the capture worker, DRAGON-215), so the iced
    /// loop keeps pumping this surface's events.
    ///
    /// The whole loading content is wrapped in a `mouse_area` that SWALLOWS stray background
    /// presses (`Ignore`), so while windows are being rearranged behind this overlay a click
    /// off the cancel affordance can never fall through to a window underneath — only the X is
    /// interactive. On macOS/Windows the surface is itself an opaque, full-display winit window
    /// (never a click-through layer), so it already eats all pointer events; the swallow is the
    /// portable belt-and-suspenders that matches `overwrite_dialog`.
    pub(super) fn preview_loading_view<'a>(
        &'a self,
        preview: &'a PreviewState,
        tb: Tb,
        toasts: Option<Element<'a, Msg>>,
    ) -> Element<'a, Msg> {
        let msgs: &[&str] = match &preview.kind {
            PreviewKind::Image(_) => &PREVIEW_LOADING_MESSAGES,
            PreviewKind::Video(_) => &video::PREVIEW_VIDEO_LOADING_MESSAGES,
        };
        let status = widget::column(vec![
            widget::indeterminate_circular().size(56.0).into(),
            widget::text(msgs[preview.loading_msg % msgs.len()])
                .size(16)
                .into(),
        ])
        .spacing(20.0)
        .align_x(Alignment::Center);
        let content = widget::column(vec![status.into(), tb.cancel_group(&self.keymap)])
            .spacing(20.0)
            .align_x(Alignment::Center);
        // Swallow any press that lands off the cancel affordance so it can't reach a window
        // being rearranged behind the overlay. `Interaction::Idle` keeps the normal cursor over
        // the dead area (the X provides its own pointer feedback).
        let swallowed: Element<'a, Msg> = widget::mouse_area(content)
            .on_press(Msg::WindowChrome(WindowChromeMsg::Ignore))
            .interaction(cosmic::iced::mouse::Interaction::Idle)
            .into();
        // The open-time automatic clipboard copy toasts BEFORE the decode lands, so the
        // loading state has to be able to show one too (DRAGON-353). Nothing here is
        // chrome, so the top-right anchor is unobstructed.
        //
        // DRAGON-393: anchor it to the whole loading REGION, not to the spinner. The spinner
        // column is `Shrink` and centred, so stacking the toast straight over it put the toast
        // in the middle of the window — and the moment the decode landed it jumped to the
        // top-right of the media area. That is the centre-to-right flit the owner reported: it
        // happens BEFORE `compose_preview` ever runs, so re-anchoring the loaded view alone
        // would not have fixed it. Filling the region here makes the FIRST placement the final
        // one horizontally. The stack is unconditional for the same reason it is in
        // `compose_preview` (DRAGON-375's class): a toast coming or going must not change the
        // tree shape under this position. `Space::new()` is zero-size but not `is_void`, so
        // `Stack::push` keeps the slot.
        //
        // WIDTH: a WINDOW's region is the whole body. The OVERLAY's is the hugged control width
        // — the very width the loaded editor's column will take ([`App::overlay_control_width`])
        // — and both are centred by the same outer container, so once the media dims are known
        // (a fresh capture knows its footprint at open) the toast's top-right corner is already
        // the corner it will keep. Height stays `Fill` in both: the loading state has no
        // toolbars, so a toast does start higher than it will sit once they appear.
        let width = if preview.surface.is_window() {
            Length::Fill
        } else {
            Length::Fixed(self.overlay_control_width(preview))
        };
        let region = super::chrome::toast_region(swallowed, width, Length::Fill);
        cosmic::iced::widget::stack(vec![
            region,
            toasts.unwrap_or_else(|| widget::Space::new().into()),
        ])
        .into()
    }

    /// Whether `id` is the open preview window.
    pub(in crate::app) fn is_preview_window(&self, id: window::Id) -> bool {
        self.preview_for(id).is_some()
    }

    /// DRAGON-216 (Linux windowed): the transient loading cover painted by the pre-opened
    /// neutral OVERLAY after its preview was swapped to a real window — kept up (over the
    /// mapping window) until the window's first configure closes it, so there's no desktop
    /// flash. Matches the overlay's loading look (same dim + spinner + message) so the swap
    /// is seamless; no cancel group (it's about to close, and the real window carries one).
    /// Portable view code (the overlay swap that drives it is Linux-only, so on other
    /// platforms `grab_overlay_closing` is never set and this is never reached).
    pub(in crate::app) fn grab_cover_view(&self) -> Element<'_, Msg> {
        let dim = self.preview_overlay_opacity;
        // The cover belongs to the capture whose preview was just swapped to a window.
        let msg_idx = self
            .capture_preview
            .and_then(|id| self.preview_for(id))
            .map_or(0, |p| p.loading_msg);
        let status = widget::column(vec![
            widget::indeterminate_circular().size(56.0).into(),
            widget::text(PREVIEW_LOADING_MESSAGES[msg_idx % PREVIEW_LOADING_MESSAGES.len()])
                .size(16)
                .into(),
        ])
        .spacing(20.0)
        .align_x(Alignment::Center);
        widget::container(status)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .padding(40)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(cosmic::theme::Container::custom(move |_t| {
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(cosmic::iced::Color::from_rgba(
                        0.0, 0.0, 0.0, dim,
                    ))),
                    ..Default::default()
                }
            }))
            .into()
    }
}
