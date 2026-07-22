//! Messages for the macOS permission-checker window (DRAGON-130).
//!
//! Kept a distinct domain (like `SettingsMsg`) so the checker's actions never
//! entangle capture/settings handling. Every variant is macOS-only in effect (TCC
//! grants don't exist on Linux) but the enum itself is un-cfg'd so the `Msg` wrapper
//! and the `update` dispatch stay platform-uniform; on Linux the window is simply
//! never opened, so none of these ever fire.

/// Which permission a card action targets.
#[cfg(target_os = "macos")]
pub use crate::app::permissions::Permission;

#[derive(Debug, Clone)]
pub enum PermissionsMsg {
    /// Fire the one-shot OS prompt for a permission (Request button) — Screen
    /// Recording / Microphone / Notifications.
    #[cfg(target_os = "macos")]
    Request(Permission),
    /// Deep-link into the relevant System Settings pane for a permission (the
    /// "Open System Settings" button when denied / prompt spent).
    #[cfg(target_os = "macos")]
    OpenSettings(Permission),
    /// Relaunch the app (spawn a fresh copy of `current_exe()` detached, then exit)
    /// — Screen Recording only applies its grant to a NEW launch, so this button
    /// appears once the grant flips green while this (pre-grant) process is running.
    /// Un-cfg'd so the Linux build type-checks the handler; only the mac view
    /// constructs it.
    #[cfg_attr(not(target_os = "macos"), expect(dead_code))]
    Relaunch,
    /// Restart the resident menu-bar daemon so it picks up a freshly-granted
    /// Accessibility permission (DRAGON-311). The daemon is a SEPARATE long-running
    /// process that resolves the focused window via the AX API; SIGTERM it and respawn
    /// a fresh daemon that re-reads the (now-granted) trust on startup. This process
    /// (the permissions window) stays open so the user sees the card stay green. macOS
    /// only, but un-cfg'd so the Linux handler type-checks; only the mac view builds it.
    #[cfg_attr(not(target_os = "macos"), expect(dead_code))]
    RestartDaemon,
    /// The live-status poll tick (fired ~1s while the window is open): kick off the
    /// prompt-free re-probe as a `Task` so grants flip in place. The probe may briefly
    /// block on an async settings query, so it runs OFF the UI thread and folds its
    /// result back via [`Self::Refresh`] — never in `view`.
    #[cfg(target_os = "macos")]
    Poll,
    /// The freshly-probed status snapshot from a [`Self::Poll`] (or a post-Request
    /// re-probe). Stored on `PermissionsState`; `view` reads only this cached value.
    #[cfg(target_os = "macos")]
    Refresh(crate::app::permissions::Probe),
}
