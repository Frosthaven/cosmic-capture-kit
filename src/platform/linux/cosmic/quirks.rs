//! COSMIC behavior quirks: the tiling-exception writer that makes the preview
//! window FLOAT instead of auto-tiling under COSMIC's tiling compositor. A
//! COSMIC-brand config write (`com.system76.CosmicSettings.WindowRules`), so it
//! lives in the COSMIC profile rather than the portable core.

use std::path::PathBuf;

/// Our app id + the preview window title — the pair COSMIC's tiling exception matches
/// (both `app_id` AND `title` must match, so this scopes the float to the preview
/// window only, never the settings window).
const APP_ID: &str = "dev.frosthaven.CosmicCaptureKit";
const PREVIEW_TITLE: &str = "Cosmic Capture Kit - Preview";

/// COSMIC's user tiling-exception file (windows here always float instead of tiling).
fn cosmic_tiling_exceptions_path() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("cosmic/com.system76.CosmicSettings.WindowRules/v1/tiling_exception_custom"),
    )
}

/// One COSMIC tiling-exception entry (`[(enabled: true, appid: "…", title: "…")]`).
#[derive(serde::Serialize, serde::Deserialize, PartialEq)]
struct TilingException {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    appid: String,
    #[serde(default)]
    title: String,
}

/// Register (or remove) a COSMIC tiling exception so the preview WINDOW floats instead
/// of auto-tiling. Idempotent: our entry is de-duplicated on every write (all matching
/// entries are stripped first, then exactly one is re-added when enabling). Preserves
/// the user's other exceptions, and never clobbers a file it can't parse. No-op off
/// COSMIC.
pub fn set_cosmic_preview_float(enable: bool) {
    if !super::is_cosmic() {
        return;
    }
    let Some(path) = cosmic_tiling_exceptions_path() else {
        return;
    };
    // Read + parse; bail on an unparseable non-empty file so we never clobber it.
    let mut list: Vec<TilingException> = match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => match ron::from_str(&s) {
            Ok(v) => v,
            Err(_) => return,
        },
        _ => Vec::new(),
    };
    let is_ours = |e: &TilingException| e.appid == APP_ID && e.title == PREVIEW_TITLE;
    let ours_count = list.iter().filter(|e| is_ours(e)).count();
    let had = ours_count > 0;
    // Strip EVERY prior copy of ours (dedupe), then add exactly one back when enabling.
    list.retain(|e| !is_ours(e));
    if enable {
        list.push(TilingException {
            enabled: true,
            appid: APP_ID.to_string(),
            title: PREVIEW_TITLE.to_string(),
        });
    }
    // Write only when something actually changed: presence flipped, or there were
    // stale duplicates to collapse.
    if enable == had && ours_count <= 1 {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(s) = ron::to_string(&list) {
        let _ = std::fs::write(&path, s);
    }
}
