//! Wi-Fi join helper.

#[cfg(not(target_os = "windows"))]
use std::process::{Command, Stdio};

/// Whether `nmcli` (NetworkManager CLI) is on PATH — needed to join Wi-Fi.
#[cfg(not(target_os = "windows"))]
fn nmcli_available() -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join("nmcli").is_file()))
        .unwrap_or(false)
}

/// Join a Wi-Fi network via NetworkManager (it auto-detects the encryption).
/// Falls back to copying the password — so it can be pasted into the OS network
/// dialog — when nmcli isn't available.
#[cfg(not(target_os = "windows"))]
pub fn join_wifi(ssid: &str, password: &str, _encryption: &str) {
    if !nmcli_available() {
        super::clipboard::copy_text(if password.is_empty() { ssid } else { password });
        return;
    }
    let mut cmd = Command::new("nmcli");
    cmd.args(["device", "wifi", "connect", ssid]);
    if !password.is_empty() {
        cmd.args(["password", password]);
    }
    let _ = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Windows (DRAGON-229): dispatch to the `netsh wlan` join body under `platform/windows/`
/// (closed split). Real join (profile XML → add → connect), degrading to copy-password
/// exactly like the Linux nmcli-absent path above.
#[cfg(target_os = "windows")]
pub fn join_wifi(ssid: &str, password: &str, encryption: &str) {
    crate::platform::windows::services::join_wifi(ssid, password, encryption);
}
