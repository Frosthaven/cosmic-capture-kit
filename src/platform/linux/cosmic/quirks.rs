//! COSMIC behavior quirks: the tiling-exception writer that makes the preview
//! window FLOAT instead of auto-tiling under COSMIC's tiling compositor. A
//! COSMIC-brand config write (`com.system76.CosmicSettings.WindowRules`), so it
//! lives in the COSMIC profile rather than the portable core.

use std::path::PathBuf;

/// Our app id — half of the pair COSMIC's tiling exception matches (both `app_id` AND `title`
/// must match, so a float is scoped to the one titled window, never another of ours).
const APP_ID: &str = "dev.frosthaven.CosmicCaptureKit";

/// The preview window's PRE-DRAGON-301 title. FROZEN history — used only to MIGRATE an exception
/// a prior build wrote under the old title to the current (renamed) title, so an existing user's
/// float rule keeps working after `shell::PREVIEW_WINDOW_TITLE` became "… - Preview Editor". The
/// CURRENT title is never hardcoded here: `set_cosmic_preview_float` takes it from the caller
/// (routed from the single source of truth, `crate::app::shell::PREVIEW_WINDOW_TITLE`).
const LEGACY_PREVIEW_TITLE: &str = "Cosmic Capture Kit - Preview";

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

/// Register (or remove) a COSMIC tiling exception for OUR window titled `title`, so it
/// FLOATS instead of auto-tiling. Scoped to our own `app_id` AND the exact title, so it
/// can never affect another app's (or another of our) windows. Idempotent: our entry is
/// de-duplicated on every write (all matching entries are stripped first, then exactly
/// one is re-added when enabling). Preserves the user's other exceptions, and never
/// clobbers a file it can't parse. No-op off COSMIC.
pub fn set_tiling_exception(title: &str, enable: bool) {
    write_tiling_exception(title, &[], enable);
}

/// Register (or remove) the tiling exception for the preview WINDOW specifically — the
/// original DRAGON-173 entry point, kept for the `preview_windowed` setting handler. `title`
/// is the CURRENT preview title, routed from the single source of truth
/// (`crate::app::shell::PREVIEW_WINDOW_TITLE`) by the caller.
///
/// DRAGON-301: also MIGRATES any exception a prior build wrote under [`LEGACY_PREVIEW_TITLE`] —
/// the reconcile strips the old-title entry and (when enabling) writes the new title in its
/// place — so an existing user's float rule keeps working after the preview window was renamed.
///
/// (Linux/COSMIC has no arm in the portable `crate::platform::opt_out_of_tiling` seam:
/// the capture overlays that seam serves are layer-shell surfaces COSMIC never tiles, and
/// a real toplevel that wants to float opts out through THIS persisted WindowRules config
/// write, not a per-open call. `set_tiling_exception` is the general form for any title.)
pub fn set_cosmic_preview_float(title: &str, enable: bool) {
    write_tiling_exception(title, &[LEGACY_PREVIEW_TITLE], enable);
}

/// Read → reconcile → write the tiling-exception file for OUR `title` (see [`reconcile_list`]
/// for the pure decision, incl. the `legacy_titles` rename migration). Bails on an unparseable
/// non-empty file so we never clobber it, and writes only when the reconcile reports a change.
/// No-op off COSMIC.
fn write_tiling_exception(title: &str, legacy_titles: &[&str], enable: bool) {
    if !super::is_cosmic() {
        return;
    }
    let Some(path) = cosmic_tiling_exceptions_path() else {
        return;
    };
    // Read + parse; bail on an unparseable non-empty file so we never clobber it.
    let list: Vec<TilingException> = match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => match ron::from_str(&s) {
            Ok(v) => v,
            Err(_) => return,
        },
        _ => Vec::new(),
    };
    let Some(next) = reconcile_list(list, title, legacy_titles, enable) else {
        return; // already in the desired state; no write.
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(s) = ron::to_string(&next) {
        let _ = std::fs::write(&path, s);
    }
}

/// Pure core of [`write_tiling_exception`]: given the current exception `list`, return the new
/// list to WRITE, or `None` when nothing changed (so the file is left untouched). Strips every
/// prior copy of OUR `title` exception (dedupe) AND every entry written under a `legacy_title`
/// (the DRAGON-301 rename migration), then re-adds exactly one `title` entry when `enable`.
/// Other apps' exceptions are always preserved.
fn reconcile_list(
    mut list: Vec<TilingException>,
    title: &str,
    legacy_titles: &[&str],
    enable: bool,
) -> Option<Vec<TilingException>> {
    let is_ours = |e: &TilingException| e.appid == APP_ID && e.title == title;
    let is_legacy =
        |e: &TilingException| e.appid == APP_ID && legacy_titles.iter().any(|&t| e.title == t);
    let ours_count = list.iter().filter(|e| is_ours(e)).count();
    let legacy_count = list.iter().filter(|e| is_legacy(e)).count();
    let had = ours_count > 0;
    // No change: the desired presence already holds, there are no stale duplicates of ours, and
    // there is no legacy-title entry to migrate away.
    if enable == had && ours_count <= 1 && legacy_count == 0 {
        return None;
    }
    // Strip EVERY prior copy of ours (dedupe) AND any legacy-title entry (migration), then add
    // exactly one `title` entry back when enabling.
    list.retain(|e| !is_ours(e) && !is_legacy(e));
    if enable {
        list.push(TilingException {
            enabled: true,
            appid: APP_ID.to_string(),
            title: title.to_string(),
        });
    }
    Some(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ours(title: &str) -> TilingException {
        TilingException { enabled: true, appid: APP_ID.to_string(), title: title.to_string() }
    }
    fn other(title: &str) -> TilingException {
        TilingException { enabled: true, appid: "com.other.App".to_string(), title: title.to_string() }
    }
    const NEW: &str = "Cosmic Capture Kit - Preview Editor";
    const LEGACY: &[&str] = &[LEGACY_PREVIEW_TITLE];

    #[test]
    fn reconcile_migrates_a_legacy_title_exception_to_the_new_title() {
        // DRAGON-301: enabling with an old-title entry present migrates it — the legacy entry is
        // stripped and exactly one NEW-title entry is written; other apps' rules are preserved.
        let out = reconcile_list(vec![other("Keep"), ours(LEGACY_PREVIEW_TITLE)], NEW, LEGACY, true)
            .expect("a migration is a change");
        assert!(out.iter().any(|e| e.appid == APP_ID && e.title == NEW));
        assert!(!out.iter().any(|e| e.title == LEGACY_PREVIEW_TITLE));
        assert!(out.iter().any(|e| e.title == "Keep"), "another app's rule is preserved");
        assert_eq!(out.iter().filter(|e| e.appid == APP_ID).count(), 1, "exactly one of ours");
    }

    #[test]
    fn reconcile_disable_removes_both_new_and_legacy() {
        let out = reconcile_list(vec![ours(NEW), ours(LEGACY_PREVIEW_TITLE)], NEW, LEGACY, false)
            .expect("removing entries is a change");
        assert!(!out.iter().any(|e| e.appid == APP_ID), "no exception of ours remains");
    }

    #[test]
    fn reconcile_reports_no_change_when_already_settled() {
        // Enabled, exactly one NEW entry, no legacy: nothing to do.
        assert!(reconcile_list(vec![ours(NEW), other("Keep")], NEW, LEGACY, true).is_none());
        // Disabled, none of ours present: nothing to do.
        assert!(reconcile_list(vec![other("Keep")], NEW, LEGACY, false).is_none());
    }

    #[test]
    fn reconcile_collapses_stale_duplicates() {
        // Two NEW entries with presence unchanged still rewrites to collapse the duplicate.
        let out = reconcile_list(vec![ours(NEW), ours(NEW)], NEW, LEGACY, true).expect("dedupe");
        assert_eq!(out.iter().filter(|e| e.appid == APP_ID && e.title == NEW).count(), 1);
    }
}
