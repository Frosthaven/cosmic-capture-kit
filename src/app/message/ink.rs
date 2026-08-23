//! `InkMsg`, the live recording annotation's own message domain (DRAGON-691).
//!
//! Its own domain rather than more variants on `CaptureMsg` or `RecordingMsg`, on the house
//! rule that a message belongs to the state machine it drives: these all act on the ink scene
//! and its surface, which nothing else touches, and the handler is one file
//! (`app::update::ink`) exactly like every other domain's.

/// What the ink surface and its toolbar buttons can ask for.
#[derive(Debug, Clone)]
pub enum InkMsg {
    /// Move the tool selector: Pointer, Marker or Magic Marker, mutually exclusive. Selecting
    /// the tool that is already selected does nothing, which is what makes the three DISARM
    /// routes safe to route here: Escape, a right click and bare `V` all mean "back to
    /// Pointer" and none of them cycles anything (DRAGON-742, `App::disarm_ink_tool`).
    SelectTool(crate::ink::InkTool),
    /// Clear every stroke at once, both kinds. The Erase button; not a tool.
    Erase,
    /// A stroke begins at this surface-LOCAL point.
    StrokeBegin(f32, f32),
    /// The pointer moved to this surface-LOCAL point with the button still down.
    StrokeMove(f32, f32),
    /// The button came up (or the pointer left the surface mid-stroke).
    StrokeEnd,
    /// The pointer is at this surface-LOCAL point with NO button down, or has left the
    /// surface (DRAGON-739).
    ///
    /// Not a stroke, and deliberately not part of the ink SCENE either. An armed tool takes
    /// pointer focus off the recorded window, which is what makes the compositor stop drawing
    /// the cursor into a window recording's frames, so the capture worker draws one itself and
    /// this is where it learns the position. It rides its own slot on `record::ink_live::LiveInk`
    /// precisely so a moving pointer cannot invalidate the ink raster cache; see that module's
    /// pointer section.
    ///
    /// `None` means the pointer left the surface (over the toolbar, or onto another output),
    /// which stops the cursor being drawn rather than freezing it at a stale position.
    PointerAt(Option<(f32, f32)>),
    /// One frame of the Magic Marker's dissolve. Alive only while magic ink is on screen; see
    /// `App::sub_ink_fade`.
    FadeTick,
    /// macOS / Windows: the ink WINDOW has opened and can now be placed natively. `u8` is the
    /// attempt count, because the window's title is set asynchronously and the native match
    /// is by title, exactly like the capture overlays' `OverlayOpened`.
    ///
    /// Never CONSTRUCTED on Linux, whose ink surfaces need no native placement (a layer
    /// surface has none, and the Silver / Bronze armed toplevel is fullscreen from creation).
    /// The variant stays in the shared enum, with a no-op arm in the handler, because a stale
    /// message from a torn-down surface is an ordinary thing for a queue to hold.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    SurfaceOpened(cosmic::iced::window::Id, u8),
}
