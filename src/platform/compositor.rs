//! One-shot enumeration of toplevel identities on the active workspace, per
//! output, via the cosmic toplevel-info protocol (cctk). Window selection uses
//! this to lay out the picker grid; the actual pixels come from
//! [`crate::record`], which captures each toplevel directly by handle (so
//! occlusion is a non-issue — no focusing required). Modeled on
//! xdg-desktop-portal-cosmic's wayland module.
//!
//! The [`Toplevel`]/[`WinRect`] identities are portable (they feed the
//! backend-agnostic picker grid + capture-scene model); the Wayland enumeration
//! below is Linux-only. On macOS the window list comes from ScreenCaptureKit via
//! the mac capture backend (DRAGON-94), so these entry points are stubs there.

/// Window rectangle in GLOBAL compositor logical coords (x, y, w, h).
pub type WinRect = (i32, i32, i32, i32);

/// A toplevel on the active workspace: its global rect plus the stable
/// `identifier` used to capture its pixels (via [`crate::record`]).
#[derive(Clone, Debug)]
pub struct Toplevel {
    pub rect: WinRect,
    pub id: String,
    /// Whether this is the focused/active window (gets the active-hint border).
    pub active: bool,
    /// The toplevel's title (may be empty), used to name window captures.
    pub title: String,
}

// Linux body of this facade: the cctk toplevel enumeration + activation lives in
// the COSMIC profile (DRAGON-220). Re-export it so `platform::compositor::*`
// resolves unchanged for every caller.
#[cfg(target_os = "linux")]
pub use crate::platform::linux::cosmic::compositor::{
    activate, activate_title, activate_until, list_toplevels,
};

/// macOS/Windows: no Wayland toplevel-info. The window list + activation move
/// behind the OS capture backend (DRAGON-94/95); until those land these are
/// honest no-ops (empty grid, no focus change).
/// macOS: the toplevel list comes from ScreenCaptureKit (the mac capture backend),
/// grouped by the output each window's centre sits on — the same shape the Wayland
/// path returns, so the picker grid + capture-scene model consume it unchanged. Called
/// off the UI thread (the launch pre-capture worker), so the blocking SCK enumeration
/// is fine here.
#[cfg(target_os = "macos")]
pub fn list_toplevels() -> std::collections::HashMap<String, Vec<Toplevel>> {
    let outputs = crate::platform::mac::output_descs();
    let mut result: std::collections::HashMap<String, Vec<Toplevel>> =
        std::collections::HashMap::new();
    for w in crate::platform::mac::list_windows() {
        let name = output_name_for_window(w.rect, &outputs);
        result.entry(name).or_default().push(w);
    }
    result
}

/// The name of the output a window occupies, keyed on the window CENTRE: the first
/// output in `outputs` whose logical rect contains the centre of `rect`, else the
/// first output as a fallback, else `""` when there are no outputs at all. This is
/// the pure core of the macOS [`list_toplevels`] grouping, split out so it is
/// unit-testable without SCK/AppKit (the signature is plain rects/points, so it also
/// compiles on every platform).
///
/// `rect` is a window's `(x, y, w, h)` and `outputs` their global TOP-LEFT logical
/// geometry. The centre is `(x + w / 2, y + h / 2)` with INTEGER truncation (matching
/// the original inline arithmetic exactly). Containment is HALF-OPEN on the right and
/// bottom edges (`cx >= ox && cx < ox + ow`, likewise Y), so a centre landing exactly
/// on the shared boundary of two abutting outputs belongs to the one whose LEFT/TOP
/// edge it sits on (the right/lower output), never the one whose right/bottom edge it
/// grazes. First-match wins, so with overlapping outputs the earliest in `outputs`
/// order takes the window.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
fn output_name_for_window(
    rect: (i32, i32, i32, i32),
    outputs: &[crate::platform::backend::OutputDesc],
) -> String {
    let (wx, wy, ww, wh) = rect;
    let (cx, cy) = (wx + ww / 2, wy + wh / 2);
    outputs
        .iter()
        .find(|o| {
            let (ox, oy) = o.logical_pos;
            let (ow, oh) = o.logical_size;
            cx >= ox && cx < ox + ow && cy >= oy && cy < oy + oh
        })
        .or_else(|| outputs.first())
        .map(|o| o.name.clone())
        .unwrap_or_default()
}

/// Windows (DRAGON-229): the toplevel list comes from Win32 `EnumWindows` (the Windows
/// capture backend), grouped by the output each window's centre sits on via the shared
/// [`output_name_for_window`] helper — the same shape the Wayland/mac paths return, so
/// the picker grid + capture-scene model consume it unchanged. Called off the UI thread
/// (the launch pre-capture worker), so the blocking enumeration is fine here.
#[cfg(target_os = "windows")]
pub fn list_toplevels() -> std::collections::HashMap<String, Vec<Toplevel>> {
    let outputs = crate::platform::windows::output_descs();
    let mut result: std::collections::HashMap<String, Vec<Toplevel>> =
        std::collections::HashMap::new();
    for w in crate::platform::windows::list_windows() {
        let name = output_name_for_window(w.rect, &outputs);
        result.entry(name).or_default().push(w);
    }
    result
}

/// Any other target (not Linux / macOS / Windows): no toplevel enumeration.
#[cfg(all(not(target_os = "linux"), not(target_os = "macos"), not(target_os = "windows")))]
pub fn list_toplevels() -> std::collections::HashMap<String, Vec<Toplevel>> {
    std::collections::HashMap::new()
}

#[cfg(not(target_os = "linux"))]
pub fn activate(_identifier: &str) {}

/// macOS (DRAGON-153): focus the already-open settings window. Cross-process window
/// lookup by TITLE needs Accessibility, so activate by PID instead — the settings
/// lock already records its holder. Settings processes run with the REGULAR
/// activation policy, so activating the app raises + keys its window.
#[cfg(target_os = "macos")]
pub fn activate_title(_title: &str) {
    // POKE first, unconditionally: the pane polls for this file and focuses ITSELF
    // (self-activation from a process with a visible window sticks, where every
    // cross-process route is cooperative-only on macOS 14+ and usually declined).
    let _ = std::fs::write(settings_focus_poke_path(), b"");
    // Best-effort cooperative activation on top (harmless when declined).
    if let Some(pid) = crate::instance::settings_lock_pid()
        && let Some(running) = objc2_app_kit::NSRunningApplication::
            runningApplicationWithProcessIdentifier(pid as i32)
    {
        #[allow(deprecated)]
        running.activateWithOptions(
            objc2_app_kit::NSApplicationActivationOptions::ActivateIgnoringOtherApps
                | objc2_app_kit::NSApplicationActivationOptions::ActivateAllWindows,
        );
    }
}

/// The file the blocked `--settings` launch touches to ask the live settings pane to
/// focus itself (`sub_settings_poke` polls it while the pane is open). macOS + Windows
/// (DRAGON-153 / DRAGON-246): both raise the pane by SELF-activation from the owning
/// process (cross-process focus is cooperative-only on macOS 14+ and foreground-locked on
/// Windows), so the launcher only leaves this poke. A pure path in the per-user runtime dir.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn settings_focus_poke_path() -> String {
    format!("{}/cosmic-capture-kit-settings.poke", crate::util::runtime_dir())
}

/// Windows (DRAGON-246): a blocked second `--settings` launch (the daemon spawns one per
/// "Settings" click; the in-app gear spawns `--focus-settings`) asks the live holder to come
/// forward + un-hide its own settings window. Write the poke file the holder polls
/// (`sub_settings_poke` → `SettingsFocusPoke` → `window::show_and_focus`) — the Windows
/// analog of the macOS poke above. Cross-process `SetForegroundWindow` is foreground-locked,
/// so — like macOS — the RELIABLE path is self-activation by the owning process; the launcher
/// only pokes. No Win32 here (pure fs), so it stays in this shared file; the native un-hide
/// lives under `platform::windows::window` (strict split).
#[cfg(target_os = "windows")]
pub fn activate_title(_title: &str) {
    let _ = std::fs::write(settings_focus_poke_path(), b"");
}

#[cfg(all(not(target_os = "linux"), not(target_os = "macos"), not(target_os = "windows")))]
pub fn activate_title(_title: &str) {}

#[cfg(test)]
mod tests {
    use super::output_name_for_window;
    use crate::platform::backend::OutputDesc;

    /// Build an `OutputDesc` from a name and a `(x, y, w, h)` logical rect.
    fn out(name: &str, x: i32, y: i32, w: i32, h: i32) -> OutputDesc {
        OutputDesc {
            name: name.to_string(),
            logical_pos: (x, y),
            logical_size: (w, h),
        }
    }

    #[test]
    fn centre_inside_picks_that_output() {
        // Two side-by-side 1000-wide outputs. A window centred deep inside the second
        // output's rect groups under it, not the first.
        let outputs = [out("A", 0, 0, 1000, 800), out("B", 1000, 0, 1000, 800)];
        // Window at x=1200,w=400 -> centre x = 1200 + 200 = 1400, inside B.
        assert_eq!(output_name_for_window((1200, 100, 400, 300), &outputs), "B");
        // Window fully inside A -> centre (500, 400).
        assert_eq!(output_name_for_window((300, 200, 400, 400), &outputs), "A");
    }

    #[test]
    fn centre_on_shared_edge_picks_the_right_output() {
        // A ends and B begins at x = 1000. A centre landing EXACTLY on x = 1000
        // belongs to B, never A: containment is half-open on the right edge
        // (`cx < ox + ow`), so A (ox=0, ow=1000) rejects cx == 1000 while B
        // (ox=1000) accepts it via `cx >= ox`. This matches the original inline
        // `cx >= ox && cx < ox + ow` comparison byte-for-byte.
        let outputs = [out("A", 0, 0, 1000, 800), out("B", 1000, 0, 1000, 800)];
        // Choose a window whose centre x is exactly 1000: x=800, w=400 -> 800 + 200.
        assert_eq!(output_name_for_window((800, 100, 400, 300), &outputs), "B");
        // The same reasoning on the BOTTOM edge: A is 0..800 in Y, C starts at y=800.
        // A centre at y = 800 belongs to C (top edge, `>=`), not A (bottom edge, `<`).
        let stacked = [out("A", 0, 0, 1000, 800), out("C", 0, 800, 1000, 800)];
        // x centre 500 (inside both columns), y centre exactly 800: y=700,h=200.
        assert_eq!(output_name_for_window((100, 700, 800, 200), &stacked), "C");
    }

    #[test]
    fn window_off_every_output_falls_back_to_first() {
        // A window whose centre lands on no output takes the FIRST output as the
        // fallback (`.or_else(|| outputs.first())`), so it is never dropped from the
        // grouping.
        let outputs = [out("A", 0, 0, 1000, 800), out("B", 1000, 0, 1000, 800)];
        // Centre far to the left of every output (negative x).
        assert_eq!(output_name_for_window((-5000, -5000, 100, 100), &outputs), "A");
        // Centre far below every output.
        assert_eq!(output_name_for_window((500, 100000, 100, 100), &outputs), "A");
    }

    #[test]
    fn no_outputs_yields_empty_name() {
        // With no outputs at all both the `find` and the `first` fallback miss, so the
        // name defaults to "" (`unwrap_or_default`) rather than panicking.
        assert_eq!(output_name_for_window((100, 100, 200, 200), &[]), "");
    }

    #[test]
    fn negative_coordinate_outputs_group_correctly() {
        // A secondary monitor to the LEFT of and ABOVE the primary sits at negative
        // global coordinates (a valid macOS arrangement). Centre containment must work
        // in that quadrant exactly as in the positive one.
        let outputs = [
            out("primary", 0, 0, 1920, 1080),
            out("left", -1600, -200, 1600, 900),
        ];
        // Window on the negative-coordinate monitor: x=-1200,w=400 -> centre x=-1000;
        // y=100,h=200 -> centre y=200; inside "left" (-1600..0 x, -200..700 y).
        assert_eq!(
            output_name_for_window((-1200, 100, 400, 200), &outputs),
            "left"
        );
        // Window back on the primary.
        assert_eq!(
            output_name_for_window((200, 200, 400, 400), &outputs),
            "primary"
        );
    }

    #[test]
    fn overlapping_outputs_keep_first_match() {
        // If two outputs overlap the centre (mirrored / cloned displays), the FIRST in
        // `outputs` order wins, because `find` short-circuits on the first hit.
        let outputs = [out("first", 0, 0, 1000, 1000), out("second", 0, 0, 1000, 1000)];
        assert_eq!(output_name_for_window((100, 100, 200, 200), &outputs), "first");
    }
}
