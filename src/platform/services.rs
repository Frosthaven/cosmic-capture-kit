//! Desktop-services seam: everything the app asks the OS to do that isn't
//! capture (which is [`crate::platform::backend`]) or surfaces (which is
//! `app::shell`). The app calls THESE paths; each maps to a per-OS
//! implementation, so porting a platform means swapping what this module
//! re-exports (cfg-gated), not touching call sites.
//!
//! | Service            | Linux (this impl)                          | macOS (DRAGON-94)              | Windows (DRAGON-95)        |
//! |--------------------|--------------------------------------------|--------------------------------|----------------------------|
//! | Clipboard          | wl-clipboard-rs via re-exec (`share`)      | arboard (NSPasteboard)         | arboard (CF_DIB/file list) |
//! | Notifications      | zbus → org.freedesktop.Notifications       | UNUserNotificationCenter       | WinRT toast                |
//! | Open / reveal      | portal OpenURI (xdg-open fallback)         | NSWorkspace / `open`           | `explorer /select`         |
//! | Wi-Fi join (QR)    | nmcli (copy-password fallback)             | `networksetup` (copy fallback) | `netsh wlan` (copy fallback)|
//! | Single instance    | flock in the runtime dir (`crate::instance`)| flock                          | named mutex                |
//! | Media pause (duck) | MPRIS babysitter (`crate::audio::ducking`) | (no public API — gated off)    | GSMTC babysitter (`crate::audio::ducking`) |
//!
//! The Linux implementations live in [`crate::share`] (spawn-side API here; the
//! `run_*` worker halves stay internal — they execute in a re-exec'd child, an
//! implementation detail of this platform, wired in `main.rs`).

pub use crate::share::{copy_text, copy_to_clipboard, join_wifi, notify, open_uri, save_and_open};
