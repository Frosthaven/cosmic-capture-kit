//! `PermissionsMsg` handling — the macOS permission-checker window's actions.
//! Mirrors `update/settings.rs`'s TCC handlers (RequestScreenTcc / RequestMicTcc /
//! OpenTccPane) but drives them from the dedicated checker's cards.

use super::super::*;

impl App {
    pub(in crate::app) fn update_permissions(
        &mut self,
        message: PermissionsMsg,
    ) -> Task<cosmic::Action<Msg>> {
        match message {
            // Relaunch is the ONE cross-platform variant (it carries no macOS type),
            // so it is handled un-cfg'd; the request/open/refresh variants only exist
            // on macOS. Relaunch spawns a fresh copy of this binary detached, then ends
            // this instance — the honest recovery for Screen Recording, which macOS
            // only applies to a NEW launch.
            PermissionsMsg::Relaunch => {
                if let Ok(exe) = std::env::current_exe() {
                    let mut cmd = std::process::Command::new(exe);
                    // Reopen straight back into the permission window so the user sees
                    // the (now-applied) grant land green, and can grant the rest.
                    cmd.arg("--permissions");
                    match cmd.spawn() {
                        Ok(child) => log::info!("permissions: relaunched (pid {})", child.id()),
                        Err(e) => log::warn!("permissions: relaunch failed: {e}"),
                    }
                }
                // End this (pre-grant) instance; the fresh child owns the window now.
                self.quit_now()
            }
            // Restart the resident daemon so it picks up a freshly-granted Accessibility
            // permission, WITHOUT closing this permissions window. Un-cfg'd (carries no
            // macOS type) so the Linux build type-checks it; the daemon plumbing inside is
            // macOS-gated. `AXIsProcessTrusted()` re-reads the TCC database live, so the
            // daemon would eventually see the grant, but restarting guarantees the running
            // daemon uses it immediately (and clears any stale AX server connection) — the
            // same SIGTERM + detached-respawn the hotkey-change flow uses (the respawn
            // comes up as a daemon because `resident` is persisted on; the mac lock retry
            // makes it race-safe).
            PermissionsMsg::RestartDaemon => {
                #[cfg(target_os = "macos")]
                {
                    // Only respawn if a daemon was actually running (the SIGTERM landed):
                    // otherwise a bare respawn could bring a daemon up when the user has
                    // residency off. The respawn re-reads `resident` at startup, so it
                    // comes up as a daemon only when residency is on.
                    if crate::instance::signal_daemon_quit() {
                        log::info!("permissions: signalled the daemon to restart for Accessibility");
                        if let Ok(exe) = std::env::current_exe() {
                            match std::process::Command::new(&exe).spawn() {
                                Ok(child) => {
                                    log::info!("permissions: respawned daemon (pid {})", child.id())
                                }
                                Err(e) => log::warn!("permissions: daemon respawn failed: {e}"),
                            }
                        }
                    } else {
                        log::debug!("permissions: no running daemon to restart for Accessibility");
                    }
                }
                Task::none()
            }
            #[cfg(target_os = "macos")]
            PermissionsMsg::Request(perm) => {
                use crate::app::permissions::Permission;
                match perm {
                    Permission::ScreenRecording => {
                        // Fire the one-shot Screen Recording prompt and mark it spent
                        // (same flag the first-run flow sets) so neither this card nor a
                        // later capture launch re-fires it — System Settings becomes the
                        // recovery. The grant itself only applies to a fresh launch, which
                        // the card's Relaunch button then offers.
                        crate::platform::mac::tcc::request_screen_capture();
                        let mut p = crate::state::load();
                        p.mac_first_run_seen = true;
                        crate::state::save(&p);
                    }
                    Permission::Microphone => crate::platform::mac::tcc::request_mic(),
                    Permission::Notifications => {
                        // Bundle-gated: UN throws in a bare binary. Unbundled, the card
                        // isn't shown at all, so this is only reachable when bundled.
                        crate::platform::mac::tcc::request_notifications();
                    }
                    Permission::Accessibility => {
                        // Fire the prompting AX check (adds the app to the Accessibility
                        // list AND pops the prompt) and mark it spent, so — like Screen
                        // Recording, whose preflight can't distinguish NotDetermined from
                        // Denied — the card falls back to Open Settings rather than
                        // re-prompting to a standing decision. Optional: capture is never
                        // blocked without it.
                        crate::platform::mac::tcc::request_accessibility();
                        let mut p = crate::state::load();
                        p.mac_accessibility_prompt_seen = true;
                        crate::state::save(&p);
                    }
                }
                // Re-probe now so the pill updates on the next frame without waiting for
                // the poll tick (the request may have flipped a NotDetermined instantly).
                self.probe_permissions_task()
            }
            #[cfg(target_os = "macos")]
            PermissionsMsg::OpenSettings(perm) => {
                use crate::app::permissions::Permission;
                use crate::platform::mac::tcc::PrivacyPane;
                let pane = match perm {
                    Permission::ScreenRecording => PrivacyPane::ScreenCapture,
                    Permission::Microphone => PrivacyPane::Microphone,
                    Permission::Notifications => PrivacyPane::Notifications,
                    Permission::Accessibility => PrivacyPane::Accessibility,
                };
                crate::platform::mac::tcc::open_privacy_pane(pane);
                Task::none()
            }
            #[cfg(target_os = "macos")]
            PermissionsMsg::Poll => self.probe_permissions_task(),
            #[cfg(target_os = "macos")]
            PermissionsMsg::Refresh(probe) => {
                log::debug!(
                    "permissions: live refresh (screen_granted={}, mic={:?}, notif={:?}, ax={})",
                    probe.screen_granted,
                    probe.microphone,
                    probe.notifications,
                    probe.accessibility_granted
                );
                self.permissions.probe = probe;
                Task::none()
            }
        }
    }
}
