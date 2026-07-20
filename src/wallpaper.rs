//! Wallpaper decoding + placement. Decoded wallpapers are memoized for the
//! process lifetime so the launch-time precapture thread can warm the cache and
//! the capture-path composite skips re-decoding. `wallpaper_crop` reproduces
//! cosmic's placement transform to recover exactly what sat behind a window.

use image::RgbaImage;
use std::path::PathBuf;
use std::sync::Arc;

// ── Wallpaper detection ladder ────────────────────────────────────────────────

/// The desktop wallpaper image, wherever this session keeps it: cosmic-bg's
/// config → GNOME gsettings → KDE Plasma's applet config → sway/hyprland paper
/// configs. `None` when nothing detectable — the features that need the FILE
/// (window-picker background, wallpaper-behind-window composite, the freeze
/// scene's wallpaper layer) degrade gracefully; flat captures and
/// windows-over-black never need it. Probed once per process (it shells out to
/// gsettings on GNOME) and cached.
///
/// The ladder walks [`crate::platform::linux::PROFILES`] in its fixed order
/// (cosmic, gnome, kde, wlroots) taking the first profile that yields a path —
/// the same precedence the old `cosmic_bg().or_else(gnome)...` chain had, with
/// the final is-a-file presence filter applied once at the end (DRAGON-220).
#[cfg(target_os = "linux")]
pub fn detect() -> Option<PathBuf> {
    static DETECTED: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DETECTED
        .get_or_init(|| {
            crate::platform::linux::PROFILES
                .iter()
                .find_map(|p| p.wallpaper_path())
                .filter(|p| p.is_file())
        })
        .clone()
}

/// macOS (DRAGON-130): the config-file ladder above has no analogue — the whole
/// probe is AppKit's per-display desktop picture instead
/// (`crate::platform::mac::wallpaper`, which applies the is-a-file +
/// decodable-extension honesty guard — rotating-set folders and `.heic` dynamic
/// wallpapers come back `None` — and owns the once-per-process cache, so an
/// off-main-thread caller can't poison it; see its doc).
#[cfg(target_os = "macos")]
pub fn detect() -> Option<PathBuf> {
    crate::platform::mac::wallpaper::wallpaper_path()
}

/// Windows (DRAGON-229 M1): the per-monitor `IDesktopWallpaper` resolver
/// (`platform::windows::wallpaper`, which applies the is-a-file + decodable-extension
/// honesty guard — slideshow / solid-color / `.heic` come back `None`) for the PRIMARY
/// monitor, falling back to the `SPI_GETDESKWALLPAPER` single path. Cached once per
/// process (it does COM + monitor enumeration), mirroring the Linux/mac arms, so a
/// repeated `caps()` probe stays cheap and an off-main-thread caller can't poison it.
#[cfg(target_os = "windows")]
pub fn detect() -> Option<PathBuf> {
    static DETECTED: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DETECTED
        .get_or_init(crate::platform::windows::wallpaper::wallpaper_path)
        .clone()
}

// The per-desktop wallpaper source readers (cosmic-bg RON, GNOME gsettings, KDE
// appletsrc, sway/hyprland paper configs) moved into the profile modules under
// `platform::linux::{cosmic,gnome,kde,wlroots}` (DRAGON-220); `detect` walks them
// through the `DesktopProfile` registry above.

/// Process-lifetime memo of decoded wallpapers, keyed by path. The capture app is
/// short-lived (it exits right after one capture), so a simple grow-only cache is
/// fine. The launch-time precapture thread warms it (via [`decode_wallpaper`]) in
/// parallel, so the capture-path composite skips the wallpaper PNG decode entirely
/// (~70ms for a 5120x1440 wallpaper even with optimized deps) instead of paying it
/// a second time on the UI thread.
fn wallpaper_memo()
-> &'static std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, Arc<RgbaImage>>> {
    static M: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, Arc<RgbaImage>>>,
    > = std::sync::OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Decode a wallpaper to RGBA, memoized for the process lifetime. Call from the
/// launch precapture thread to pre-warm the cache; the capture-time composite
/// ([`wallpaper_crop`]) then reuses the decoded pixels instead of decoding again.
pub fn decode_wallpaper(path: &std::path::Path) -> Option<Arc<RgbaImage>> {
    if let Some(img) = wallpaper_memo().lock().ok().and_then(|m| m.get(path).cloned()) {
        return Some(img);
    }
    let img = Arc::new(image::open(path).ok()?.to_rgba8());
    if let Ok(mut m) = wallpaper_memo().lock() {
        m.insert(path.to_path_buf(), img.clone());
    }
    Some(img)
}

/// Crop the wallpaper image to a window's on-screen area, the way cosmic places it, so
/// only the wallpaper (no occluding windows) ends up behind the captured window, at
/// `(rw, rh)` px. `rx, ry` are the area's top-left in the output's physical px;
/// `out_w, out_h` the output's physical size. `stretch` matches cosmic's `Stretch`
/// scaling (each axis fills independently); otherwise it's `Zoom` (cover, centred) —
/// which is also what `Fit` falls back to here. Crucially the output-space rect is
/// mapped back through the *same* placement transform, so the wallpaper lines up with
/// what was really behind the window.
#[allow(clippy::too_many_arguments)]
pub fn wallpaper_crop(
    path: &std::path::Path,
    stretch: bool,
    out_w: u32,
    out_h: u32,
    rx: i32,
    ry: i32,
    rw: u32,
    rh: u32,
) -> Option<RgbaImage> {
    let wp = decode_wallpaper(path)?;
    let wp: &RgbaImage = &wp;
    let (ww, wh) = (wp.width() as f32, wp.height() as f32);
    if ww < 1.0 || wh < 1.0 {
        return None;
    }
    // Map the area (output px) back into source-wallpaper px under cosmic's placement.
    let (sx, sy, sw, sh) = if stretch {
        let (kx, ky) = (ww / out_w as f32, wh / out_h as f32);
        (
            (rx as f32 * kx).max(0.0),
            (ry as f32 * ky).max(0.0),
            (rw as f32 * kx).ceil().max(1.0),
            (rh as f32 * ky).ceil().max(1.0),
        )
    } else {
        // Cover scale + centred crop offset (in scaled-wallpaper px).
        let s = (out_w as f32 / ww).max(out_h as f32 / wh);
        let ox = (ww * s - out_w as f32) / 2.0;
        let oy = (wh * s - out_h as f32) / 2.0;
        (
            ((rx as f32 + ox) / s).max(0.0),
            ((ry as f32 + oy) / s).max(0.0),
            (rw as f32 / s).ceil().max(1.0),
            (rh as f32 / s).ceil().max(1.0),
        )
    };
    let cw = (sw as u32).min(wp.width().saturating_sub(sx as u32)).max(1);
    let ch = (sh as u32).min(wp.height().saturating_sub(sy as u32)).max(1);
    let crop = image::imageops::crop_imm(wp, sx as u32, sy as u32, cw, ch).to_image();
    Some(image::imageops::resize(
        &crop,
        rw.max(1),
        rh.max(1),
        image::imageops::FilterType::Lanczos3,
    ))
}
