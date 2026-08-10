//! macOS/Windows stub for the `ksni` StatusNotifierItem recording controls.
//!
//! The real tray is D-Bus (Linux). On macOS the recording controls become a
//! menu-bar `NSStatusItem` (DRAGON-94 phase 4); until then [`TraySession::start`]
//! returns `None`, exactly like a Linux session with no SNI host, so the app keeps
//! its in-frame recording toolbar.

/// A control the user activated from the tray. Mirrors the Linux enum so the app's
/// event handling is platform-free.
// The stub's `poll` never yields events, so these variants are matched (in the app's
// platform-free tray handling) but never CONSTRUCTED here — allow the lint rather than
// fragment that shared match. Real construction is the Linux `ksni` tray.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    Stop,
    TogglePause,
    ToggleMic,
    ToggleSystemAudio,
    /// An "Audio Recording" radio pick's complete arm state (DRAGON-558; see the Linux
    /// enum). Matched in the shared app handling, never constructed here.
    AudioArms(crate::recording_ui::AudioArmState),
    /// A "Countdown Timer" radio pick's preset index (DRAGON-574; see the Linux enum).
    /// Matched in the shared app handling, never constructed here.
    CountdownPick(usize),
    Cancel,
    /// Quit the whole capture session/app (DRAGON-174; the idle session icon's Quit).
    Quit,
}

/// A live session tray. Never constructed on this platform (`start_*` return `None`);
/// the type + methods exist so the app's `Option<TraySession>` state and its
/// poll/update calls compile unchanged. Mirrors the mac/Linux seam (DRAGON-174).
pub struct TraySession {}

impl TraySession {
    /// No tray on this platform, so the caller keeps the in-frame toolbar.
    pub fn start_recording(
        _mic: bool,
        _system_audio: bool,
        _accent: [u8; 3],
        _delay_idx: usize,
    ) -> Option<Self> {
        None
    }

    /// No resident/daemon on this platform.
    pub fn start_daemon(_mic: bool, _system_audio: bool) -> Option<Self> {
        None
    }

    pub fn poll(&self) -> Vec<TrayEvent> {
        Vec::new()
    }

    pub fn set_audio(&self, _mic: bool, _system_audio: bool) {}

    pub fn set_paused(&self, _paused: bool) {}
}

/// The countdown digits tray item (DRAGON-563). Never constructed on this platform
/// (`start` returns `None`, so the countdown simply has no tray digits); the type +
/// methods exist so the app's `Option<CountdownTraySession>` state and its poll/update
/// calls compile unchanged, the same shape as [`TraySession`] above.
pub struct CountdownTraySession {}

impl CountdownTraySession {
    /// No tray on this platform, so the countdown runs without digits.
    pub fn start(_remaining: u8) -> Option<Self> {
        None
    }

    pub fn set_remaining(&self, _remaining: u8) {}

    pub fn poll(&self) -> Vec<TrayEvent> {
        Vec::new()
    }
}
