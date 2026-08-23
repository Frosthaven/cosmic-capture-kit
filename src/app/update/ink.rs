//! `InkMsg` handling (DRAGON-691): the live recording annotation's dispatch.
//!
//! Thin by design, like every other domain's: the decisions live in `app::ink` (the scene and
//! the surface) and `crate::ink` (the maths), and this only routes.
//!
//! The one rule worth stating here is the one every arm follows: a handler that can change
//! whether ink is on screen ends by calling `sync_ink_surface`, which is the single place the
//! surface's existence is decided. Selecting a drawing tool mints it, going back to Pointer
//! with ink left makes it display-only, and the last stroke dissolving away takes it down.

use super::super::*;

impl App {
    pub(in crate::app) fn update_ink(&mut self, message: InkMsg) -> Task<cosmic::Action<Msg>> {
        // DRAGON-739: where the pointer is, kept before anything routes, because three arms
        // carry it and only one of them is about the pointer. `PointerAt` is a hover or a
        // leave; the two stroke arms carry it as a by-product of drawing, which is what stops
        // the widget having to report the same position twice while a button is down.
        match &message {
            InkMsg::PointerAt(at) => self.ink.pointer = *at,
            InkMsg::StrokeBegin(x, y) | InkMsg::StrokeMove(x, y) => {
                self.ink.pointer = Some((*x, *y))
            }
            _ => {}
        }
        // A hover changes nothing about the SCENE, so it must not republish one. That is not
        // an optimisation, it is the design: the worker's ink raster is cached per published
        // scene, and republishing on every mouse sample would re-rasterize the whole drawing
        // on every frame written, which is the exact cost `record::ink_live`'s cache exists to
        // avoid. See that module's pointer section.
        if matches!(message, InkMsg::PointerAt(_)) {
            self.publish_live_cursor();
            return Task::none();
        }
        let task = self.route_ink(message);
        // DRAGON-724: every one of those arms can change what is on screen, and a WINDOW
        // recording whose capture excludes our overlays has a worker painting that scene onto
        // its frames. Publishing here rather than per arm is the same move `sync_ink_surface`
        // makes for the surface: one place, so no handler has to remember. A session with no
        // live composite (every other mode) returns from it immediately.
        self.publish_live_ink();
        // DRAGON-739: and the pointer beside it, because arming or disarming a tool is what
        // decides whether one is drawn at all.
        self.publish_live_cursor();
        task
    }

    /// The routing itself. Split from [`App::update_ink`] so the one thing every arm owes,
    /// republishing the scene, cannot be forgotten by a new arm.
    fn route_ink(&mut self, message: InkMsg) -> Task<cosmic::Action<Msg>> {
        match message {
            InkMsg::SelectTool(tool) => self.ink_select(tool),
            InkMsg::Erase => self.ink_erase(),
            InkMsg::StrokeBegin(x, y) => self.ink_stroke_begin(x, y),
            InkMsg::StrokeMove(x, y) => self.ink_stroke_move(x, y),
            // A stroke ending can retire the surface: a Magic Marker tap made while the tool
            // was made as the tool went back to Pointer leaves nothing behind, and the sync
            // is what notices.
            InkMsg::StrokeEnd => {
                let end = self.ink_stroke_end();
                Task::batch([end, self.sync_ink_surface()])
            }
            // DRAGON-739: handled entirely in `update_ink` above, which owns the pointer slot
            // and returns before this routes. The arm is here so the match stays exhaustive
            // and so a future reader is not left wondering where it went.
            InkMsg::PointerAt(_) => Task::none(),
            InkMsg::FadeTick => self.ink_fade_tick(),
            // macOS/Windows only. On Linux the surface is a layer surface that needs no
            // native placement, so nothing ever publishes this there; the arm is a no-op
            // rather than an `unreachable!`, because a stale message from a torn-down surface
            // is an ordinary thing for a message queue to hold.
            #[cfg(not(target_os = "linux"))]
            InkMsg::SurfaceOpened(id, attempt) => self.configure_ink_surface(id, attempt),
            #[cfg(target_os = "linux")]
            InkMsg::SurfaceOpened(_, _) => Task::none(),
        }
    }
}
