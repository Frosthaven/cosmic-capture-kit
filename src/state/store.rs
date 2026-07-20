//! IO / persistence layer for [`Persisted`]: path, load, save, defaults, and migrate.
//!
//! The on-disk format is **TOML** at `~/.config/cosmic-capture-kit/config.toml` on
//! EVERY OS ([`crate::util::app_config_dir`]; XDG-respecting on Linux). macOS builds
//! before the uniform location wrote under `~/Library/Application Support` — the
//! helper migrates that whole directory once. Builds before the TOML switch stored
//! RON at `dirs::state_dir()/cosmic-capture-kit/state.ron` (Linux-only paths); that
//! file is still READ once for migration (then left in place for rollback) but
//! never written again.

use std::path::PathBuf;
use super::schema::Persisted;

/// The config file (current format, all platforms).
fn config_path() -> Option<PathBuf> {
    Some(crate::util::app_config_dir()?.join("config.toml"))
}

/// The config file's mtime, if it exists. The Linux resident daemon uses this as
/// its cheap "did a settings process save new state?" probe (DRAGON-179) — a full
/// re-read happens only when this changes; the Windows daemon polls it the same way
/// to re-tint its tray icon on an accent change (DRAGON-250). Unused on macOS (the
/// menu-bar glyph is an AppKit template image; no tint to follow).
#[cfg_attr(not(any(target_os = "linux", target_os = "windows")), expect(dead_code))]
pub fn config_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(config_path()?).ok()?.modified().ok()
}

/// The legacy RON state file older builds wrote (Linux-only paths). Read-only.
fn legacy_ron_path() -> Option<PathBuf> {
    let dir = dirs::state_dir().or_else(dirs::cache_dir)?;
    Some(dir.join("cosmic-capture-kit").join("state.ron"))
}

/// Whether a persisted state file already exists (i.e. this is not the very first
/// launch). Used to pick smart capture-method defaults only once. A legacy RON
/// file counts — a migrating user is not a fresh install.
pub fn file_exists() -> bool {
    config_path().map(|p| p.exists()).unwrap_or(false)
        || legacy_ron_path().map(|p| p.exists()).unwrap_or(false)
}

pub fn load() -> Persisted {
    let mut p = load_raw().unwrap_or_else(defaults);
    migrate(&mut p);
    p
}

/// Read the current TOML config; else migrate the legacy RON state once (writing
/// the TOML immediately so the migration happens exactly once — the RON stays on
/// disk untouched, as a rollback path for older builds).
fn load_raw() -> Option<Persisted> {
    if let Some(s) = config_path().and_then(|p| std::fs::read_to_string(p).ok())
        && let Ok(p) = toml::from_str::<Persisted>(&s)
    {
        return Some(p);
    }
    let legacy = legacy_ron_path().and_then(|p| std::fs::read_to_string(p).ok())?;
    let mut p: Persisted = ron::from_str(&legacy).ok()?;
    // Migrate BEFORE the one-time TOML write: deprecated fields (the v3 capture
    // booleans) are skip_serializing, so their values must reach their
    // replacements first or this write would drop the user's choice.
    migrate(&mut p);
    save(&p);
    Some(p)
}

/// Current config schema version. Bump when a stored index changes meaning and
/// add a guarded step in `migrate`.
pub const CONFIG_VERSION: u32 = 7;

/// One-time migrations for configs saved by older versions, keyed on
/// `config_version`. Idempotent — running it on an already-current config is a
/// no-op, so it is safe to call on every load (even if the result is never
/// re-saved).
fn migrate(p: &mut Persisted) {
    if p.config_version < 1 {
        // v1 inserted "360p" at max-resolution index 1, shifting every preset
        // from 480p up (old index >= 1) one slot higher. Original (0) is fixed.
        if p.record_res_preset >= 1 {
            p.record_res_preset += 1;
        }
    }
    if p.config_version < 2 {
        // v2 raised the default bitrate: 450 kbps crushed chroma (magenta on dark
        // areas). Bump anyone still on the old default; leave explicit choices alone.
        if p.record_bitrate_kbps == 450 {
            p.record_bitrate_kbps = 1800;
        }
    }
    if p.config_version < 3 {
        // v3 (DRAGON-129): the capture-method booleans became backend ids. Map the
        // saved choice — true → the portal, false → the platform's native backend
        // (a "portal" from a foreign config is clamped at use on platforms without
        // one). v3+ configs never carry the booleans (skip_serializing), so this
        // runs only on genuinely old files.
        p.record_backend = pipewire_bool_to_backend(p.prefer_pipewire);
        p.screenshot_backend = pipewire_bool_to_backend(p.screenshot_pipewire);
    }
    if p.config_version < 4 {
        // v4 raised the default bitrate cap from 1800 to 8000 kbps (crisper clips).
        // Bump anyone still on the old default; leave explicit choices alone.
        if p.record_bitrate_kbps == 1800 {
            p.record_bitrate_kbps = 8000;
        }
    }
    if p.config_version < 5 {
        // v5 (DRAGON-174): the `recording_tray` systray-vs-toolbar choice was replaced by
        // `hide_toolbar_fullscreen` (hide the floating toolbar when it can't fit outside a
        // full-screen capture). The two DEFAULTS are opposite (`recording_tray` defaulted
        // ON, `hide_toolbar_fullscreen` defaults OFF) and the meanings only APPROXIMATE
        // each other, so the migration is conservative: every pre-v5 config lands on the
        // new default OFF (do not hide). This is enforced structurally — `recording_tray`
        // was removed from the schema, so serde drops the old key on load and
        // `hide_toolbar_fullscreen` is absent → its serde default (`false`). We do NOT
        // honor a true->true / false->false carry-over: the old default was ON, so every
        // config an old build wrote carries `recording_tray = true` whether or not the user
        // ever touched it (it was not skip_serializing then), so an explicit choice is
        // indistinguishable from the old default on disk. Mapping true->true would flip
        // nearly every existing user into hiding the toolbar, the opposite of the new
        // intent, so we drop the value and everyone re-opts-in from OFF. Nothing to write
        // here beyond the version bump — the field's absence already yields OFF.
    }
    if p.config_version < 6 {
        // v6: the default window padding was raised 36 -> 50. Mirror the bitrate
        // migrations (v2/v4): a config still on the OLD default bumps to the new
        // one; an explicitly chosen value is preserved.
        if p.window_padding_px == 36 {
            p.window_padding_px = 50;
        }
    }
    if p.config_version < 7 {
        // v7 (DRAGON-191): the single `window_border_style` dropdown (0 Active+shadow,
        // 1 Inactive+shadow, 2 Inactive no-shadow, 3 Raw) became explicit
        // Active/Inactive border colour+width fields plus a drop-shadow toggle. Map the
        // old style into those new fields, then `window_border_style` retires
        // (skip_serializing, read only here). Colour fields are left at their new
        // defaults (Active follows the accent = `None`; Inactive = 0xff414550), matching
        // what the old styles drew from the theme/JankyBorders.
        //   - style 3 (Raw): no border at all -> both widths 0, no shadow.
        //   - style 2 (Inactive, no shadow): keep the border widths, drop the shadow.
        //   - styles 0/1 (had a shadow): keep the border widths, keep the shadow.
        // A single-window capture's active-vs-inactive portrayal carries over into
        // `window_single_active` (old style 0 = Active -> true; styles 1/2 = Inactive
        // -> false). Region/monitor composites now pick per-window automatically.
        match p.window_border_style {
            3 => {
                // Raw: no border at all.
                p.active_border_width = 0;
                p.inactive_border_width = 0;
                p.window_drop_shadow = false;
                // Widths are 0 so the single-window choice is moot; leave the default.
            }
            2 => {
                // Inactive, no shadow.
                p.window_drop_shadow = false;
                p.window_single_active = false;
            }
            1 => {
                // Inactive with shadow.
                p.window_single_active = false;
            }
            // 0 (Active + shadow) and any unknown value keep the new defaults
            // (widths 3/1, shadow on, single-window = Active).
            _ => {}
        }
    }
    // Version-independent safety net: an empty id (hand-edited config) falls back
    // to the platform default rather than persisting as unset.
    if p.record_backend.is_empty() {
        p.record_backend = crate::platform::backend::native_backend_id().to_string();
    }
    if p.screenshot_backend.is_empty() {
        p.screenshot_backend = crate::platform::backend::native_backend_id().to_string();
    }
    p.config_version = CONFIG_VERSION;
}

/// The v3 boolean→backend-id mapping: the portal when the boolean said so AND the
/// platform has one, else the native backend.
fn pipewire_bool_to_backend(portal: bool) -> String {
    if portal && cfg!(target_os = "linux") {
        crate::platform::backend::PORTAL_ID.to_string()
    } else {
        crate::platform::backend::native_backend_id().to_string()
    }
}

/// Defaults for a fresh install. The derived `Default` would give raw type-zeros
/// (e.g. confidence 0.0); instead deserialize an EMPTY TOML document so every field
/// falls back to its `#[serde(default = …)]`, i.e. the documented defaults. The
/// version is pinned to current explicitly (its serde default is 0, which would
/// otherwise send fresh defaults through the old-config migrations).
///
/// Also the source of per-setting "reset to default" values in the settings UI.
pub fn defaults() -> Persisted {
    let mut p: Persisted = toml::from_str("").unwrap_or_default();
    p.config_version = CONFIG_VERSION;
    p
}

pub fn save(p: &Persisted) {
    let Some(path) = config_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(s) = toml::to_string(p) {
        let _ = std::fs::write(path, s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Guard: TOML requires tables/arrays-of-tables to be emitted AFTER scalar keys.
    // `covermark_prefs` (an array-of-tables) sits mid-struct with scalars declared after
    // it; the `toml` crate reorders tables last so the round trip is correct, but this
    // pins that behavior — a regression there would drop every scalar after it on save.
    #[test]
    fn config_roundtrips_with_populated_covermark_prefs() {
        let mut p = defaults();
        p.covermark_prefs = vec![crate::state::schema::CovermarkPref {
            key: "confidential".into(),
            zoom: 1.5,
            opacity: 0.5,
        }];
        // Canaries: scalars declared AFTER `covermark_prefs` in the struct.
        p.capture_hotkey = "F13".into();
        p.region_overlay_opacity = 0.42;
        let s = toml::to_string(&p).expect("serialize must succeed");
        let back: Persisted = toml::from_str(&s).expect("deserialize must succeed");
        assert_eq!(back.capture_hotkey, "F13", "scalar after covermark_prefs survived");
        assert_eq!(back.region_overlay_opacity, 0.42);
        assert_eq!(back.covermark_prefs.len(), 1);
    }

    // The real reset mechanism: `load()` falls back to `defaults()` if `toml::from_str`
    // ERRORS. An OLD config (missing every field added later) must still deserialize —
    // each absent field must fall back to its default, never fail the whole parse. This
    // simulates a user's pre-upgrade config and asserts their kept settings survive.
    #[test]
    fn old_partial_config_still_deserializes() {
        let old = "\
delay_idx = 2\n\
capture_cursor = false\n\
region_overlay_opacity = 0.3\n\
record_fps = 60\n";
        let p: Persisted = toml::from_str(old)
            .expect("an old config missing newer fields must deserialize (no reset)");
        // Kept settings survive...
        assert_eq!(p.delay_idx, 2);
        assert!(!p.capture_cursor);
        assert_eq!(p.record_fps, 60);
        // ...and fields absent from the old file take their defaults.
        assert_eq!(p.active_border_width, 3);
        assert_eq!(p.inactive_border_width, 1);
        assert!(p.window_drop_shadow);
    }

    // The container-level `#[serde(default)]` guarantee: an EMPTY config parses cleanly to
    // full defaults (no field is ever "required"), so a future field added without its own
    // `#[serde(default = "…")]` can never fail the parse and reset every setting.
    #[test]
    fn empty_config_parses_to_defaults_never_errors() {
        let p: Persisted = toml::from_str("").expect("empty config must parse (no field required)");
        assert!(p.capture_cursor); // a per-field default still applies
        assert_eq!(p.active_border_width, 3);
        assert_eq!(p.record_fps, 30);
    }

    // Fresh-install defaults must come from the `#[serde(default = …)]` fns, not
    // raw type-zeros — so the empty TOML in `defaults()` has to parse.
    #[test]
    fn fresh_defaults_use_serde() {
        let d = defaults();
        assert_eq!(d.text_confidence, 25.0);
        assert_eq!(d.region_overlay_opacity, 0.66);
        assert_eq!(d.record_fps, 30);
        assert!(d.copy_to_clipboard);
        assert_eq!(d.record_res_preset, 5); // 2K
        assert_eq!(d.nvenc_preset, "p4");
        assert_eq!(d.x264_preset, "fast");
        assert_eq!(d.config_version, 7);
        // DRAGON-174: the new toolbar-hiding setting defaults OFF (do not hide).
        assert!(!d.hide_toolbar_fullscreen);
        // Residency defaults on where the global hotkey needs the daemon (macOS);
        // Linux stays one-shot. The login-item seed marker always starts unset.
        assert_eq!(d.resident, cfg!(target_os = "macos"));
        assert!(!d.mac_login_item_seeded);
        // Fresh backend-id defaults mirror the retired booleans' defaults per platform.
        assert_eq!(d.screenshot_backend, crate::platform::backend::native_backend_id());
        #[cfg(target_os = "linux")]
        assert_eq!(d.record_backend, crate::platform::backend::PORTAL_ID);
        #[cfg(not(target_os = "linux"))]
        assert_eq!(d.record_backend, crate::platform::backend::native_backend_id());
        assert_eq!(d.record_bitrate_kbps, 8000);
        assert!(d.capture_cursor);
        // DRAGON-191: explicit Active/Inactive border fields + drop-shadow toggle.
        assert_eq!(d.active_border_color, None); // follow the accent
        assert_eq!(d.active_border_width, 3);
        assert_eq!(d.inactive_border_color, [65, 69, 80, 255]);
        assert_eq!(d.inactive_border_width, 1);
        assert!(d.window_drop_shadow);
        assert!(d.window_single_active); // single-window capture = Active by default
        assert_eq!(d.window_transparency_multiplier, 0.0);
        assert!(d.window_padding);
        assert_eq!(d.window_padding_px, 50);
        assert!(d.scan_codes);
        assert!(d.scan_text);
        assert_eq!(d.region, None);
        assert_eq!(d.delay_idx, 0);
        // Uncalibrated by default; an old config (no key) reads the same way.
        assert_eq!(d.av_calibration_base_ms, 0);
        // Appearance (DRAGON-139): follow the system by default, overrides at their
        // neutral values (automatic mode, no accent override). The customize-mode
        // roundness default is PER-PLATFORM (DRAGON-256): Windows slightly-round (1),
        // macOS/Linux round (0).
        assert!(d.appearance_use_system);
        assert_eq!(d.appearance_mode, 0);
        assert_eq!(d.appearance_accent, None);
        // Automatic Contrast Boost (DRAGON-289) defaults ON (absent key ⇒ true).
        assert!(d.appearance_contrast_boost);
        #[cfg(target_os = "windows")]
        assert_eq!(d.appearance_roundness, 1);
        #[cfg(not(target_os = "windows"))]
        assert_eq!(d.appearance_roundness, 0);
    }

    #[test]
    fn migrate_v0_shifts_res_preset_for_inserted_360p() {
        // Old (pre-360p) config: 1080p lived at index 3, Custom at 6.
        for (old, new) in [(0u8, 0u8), (1, 2), (3, 4), (5, 6), (6, 7)] {
            let mut p = defaults();
            p.config_version = 0;
            p.record_res_preset = old;
            migrate(&mut p);
            assert_eq!(p.record_res_preset, new, "old preset {old}");
            assert_eq!(p.config_version, CONFIG_VERSION);
        }
        // A current config is left untouched — no double-shift.
        let mut q = defaults();
        q.record_res_preset = 2;
        migrate(&mut q);
        assert_eq!(q.record_res_preset, 2);
    }

    // Legacy RON state files (every field shape older builds could write) must keep
    // PARSING so the one-time migration is lossless. RON is never written anymore,
    // so only the read direction is pinned.
    #[test]
    fn legacy_ron_still_parses() {
        let on_disk = "(region: Some((10, 20, 110, 220)), delay_idx: 3, config_version: 2, \
                        pw_restore_token: Some(\"tok\"), \
                        shortcuts: [(CopyText, None)])";
        let p: Persisted = ron::from_str(on_disk).expect("old RON must still parse");
        assert_eq!(p.region, Some((10, 20, 110, 220)));
        assert_eq!(p.delay_idx, 3);
        assert_eq!(p.pw_restore_token.as_deref(), Some("tok"));
        // The legacy TUPLE shortcut-override shape (incl. an unbound None) survives.
        assert_eq!(p.shortcuts.len(), 1);
        assert_eq!(p.shortcuts[0].0, crate::shortcuts::Action::CopyText);
        assert!(p.shortcuts[0].1.is_none());
    }

    // The TOML round trip must preserve everything the legacy format could hold —
    // including the shapes TOML can't express natively (None region via key
    // omission; unbound shortcut overrides via the entry table form).
    #[test]
    fn toml_round_trips_non_defaults() {
        let mut p = defaults();
        p.region = Some((-1920, 0, 236, 902)); // negative coords (left-of-primary)
        p.settings_size = Some((2542, 1384));
        p.pw_restore_token = Some("yxxZ".into());
        p.record_bitrate_kbps = 2500;
        p.record_dir = "~/Videos".into();
        p.av_calibration_base_ms = -260;
        p.shortcuts = vec![(crate::shortcuts::Action::CopyText, None)];
        let s = toml::to_string(&p).expect("serialize");
        let q: Persisted = toml::from_str(&s).expect("parse back");
        assert_eq!(q.region, p.region);
        assert_eq!(q.settings_size, p.settings_size);
        assert_eq!(q.pw_restore_token, p.pw_restore_token);
        assert_eq!(q.record_bitrate_kbps, 2500);
        assert_eq!(q.record_dir, "~/Videos");
        assert_eq!(q.av_calibration_base_ms, -260);
        assert_eq!(q.shortcuts.len(), 1);
        assert_eq!(q.shortcuts[0].0, crate::shortcuts::Action::CopyText);
        assert!(q.shortcuts[0].1.is_none());
    }

    // Overrides for actions a later build REMOVED (DRAGON-158 dropped FocusSearch
    // and PreviewNoAi) must drop silently on load — a parse error here would fail
    // the whole config and load_raw would fall back to defaults, losing every
    // persisted setting. Both readable shapes are pinned: the TOML entry table and
    // the legacy RON tuple.
    #[test]
    fn removed_action_overrides_drop_without_poisoning_the_config() {
        let on_disk = "record_dir = \"~/Videos\"\n\
                       [[shortcuts]]\n\
                       action = \"FocusSearch\"\n\
                       [[shortcuts]]\n\
                       action = \"CopyText\"\n";
        let p: Persisted = toml::from_str(on_disk).expect("stale override must not fail the parse");
        assert_eq!(p.record_dir, "~/Videos", "other settings survive");
        assert_eq!(p.shortcuts.len(), 1, "only the removed action's entry drops");
        assert_eq!(p.shortcuts[0].0, crate::shortcuts::Action::CopyText);

        let legacy = "(record_dir: \"~/Videos\", \
                      shortcuts: [(PreviewNoAi, None), (CopyText, None)])";
        let q: Persisted = ron::from_str(legacy).expect("stale RON override must not fail");
        assert_eq!(q.record_dir, "~/Videos");
        assert_eq!(q.shortcuts.len(), 1);
        assert_eq!(q.shortcuts[0].0, crate::shortcuts::Action::CopyText);
    }

    // None-valued options must serialize by OMISSION (TOML has no literal None) and
    // deserialize back to None from an absent key.
    #[test]
    fn toml_omits_none_fields() {
        let p = defaults();
        let s = toml::to_string(&p).expect("serialize");
        let has_key = |k: &str| s.lines().any(|l| l.trim_start().starts_with(k));
        assert!(!has_key("region ="), "None region must be omitted, got: {s}");
        assert!(!has_key("settings_size ="));
        assert!(!has_key("pw_restore_token ="));
        let q: Persisted = toml::from_str(&s).expect("parse back");
        assert_eq!(q.region, None);
        assert_eq!(q.settings_size, None);
    }

    #[test]
    fn migrate_v3_maps_pipewire_booleans_to_backend_ids() {
        // An old (v2) config's method booleans become backend ids exactly once.
        let mut p = defaults();
        p.config_version = 2;
        p.screenshot_pipewire = true;
        p.prefer_pipewire = false;
        migrate(&mut p);
        assert_eq!(p.config_version, CONFIG_VERSION);
        #[cfg(target_os = "linux")]
        {
            assert_eq!(p.screenshot_backend, "portal");
            assert_eq!(p.record_backend, "screencopy");
        }
        // Platforms without a portal map both to the native backend.
        #[cfg(not(target_os = "linux"))]
        {
            assert_eq!(p.screenshot_backend, crate::platform::backend::native_backend_id());
            assert_eq!(p.record_backend, crate::platform::backend::native_backend_id());
        }
        // A current (v3) config keeps its saved ids — the stale boolean defaults
        // (prefer_pipewire deserializes true) must not clobber them.
        let mut q = defaults();
        q.record_backend = crate::platform::backend::native_backend_id().to_string();
        migrate(&mut q);
        assert_eq!(q.record_backend, crate::platform::backend::native_backend_id());
        // An empty id (hand-edited file) recovers to the platform default.
        let mut r = defaults();
        r.screenshot_backend = String::new();
        migrate(&mut r);
        assert_eq!(r.screenshot_backend, crate::platform::backend::native_backend_id());
    }

    #[test]
    fn deprecated_pipewire_booleans_are_never_written() {
        // v3 configs must not carry the retired booleans (their presence is what
        // marks a config as pre-v3 for the one-time migration).
        let s = toml::to_string(&defaults()).expect("serialize");
        assert!(!s.contains("prefer_pipewire"), "got: {s}");
        assert!(!s.contains("screenshot_pipewire"), "got: {s}");
        // The backend ids are written and round-trip.
        let q: Persisted = toml::from_str(&s).expect("parse back");
        assert_eq!(q.screenshot_backend, defaults().screenshot_backend);
        assert_eq!(q.record_backend, defaults().record_backend);
    }

    #[test]
    fn migrate_v2_then_v4_bumps_old_default_bitrates() {
        // A pre-v2 config on the ancient 450 kbps default rides both bumps: v2 lifts
        // 450 → 1800, then v4 lifts that same 1800 → the current default (8000).
        let mut p = defaults();
        p.config_version = 1;
        p.record_bitrate_kbps = 450;
        migrate(&mut p);
        assert_eq!(p.record_bitrate_kbps, 8000);
        assert_eq!(p.config_version, CONFIG_VERSION);
        // An explicit (non-default) bitrate is preserved through every step.
        let mut q = defaults();
        q.config_version = 1;
        q.record_bitrate_kbps = 2000;
        migrate(&mut q);
        assert_eq!(q.record_bitrate_kbps, 2000);
    }

    #[test]
    fn migrate_v4_bumps_old_default_bitrate() {
        // A v3 config still on the previous default (1800) → the new default (8000).
        let mut p = defaults();
        p.config_version = 3;
        p.record_bitrate_kbps = 1800;
        migrate(&mut p);
        assert_eq!(p.record_bitrate_kbps, 8000);
        assert_eq!(p.config_version, CONFIG_VERSION);
        // An explicit (non-default) v3 bitrate is preserved.
        let mut q = defaults();
        q.config_version = 3;
        q.record_bitrate_kbps = 3500;
        migrate(&mut q);
        assert_eq!(q.record_bitrate_kbps, 3500);
    }

    #[test]
    fn migrate_v5_lands_on_the_new_toolbar_default_off() {
        // DRAGON-174: the retired `recording_tray` field is gone from the schema, so an old
        // config's key is dropped on load and `hide_toolbar_fullscreen` is absent → its
        // serde default OFF. A pre-v5 config (regardless of what the old field held) must
        // therefore migrate to the new default OFF and land on the current version.
        let mut p = defaults();
        p.config_version = 4;
        p.hide_toolbar_fullscreen = false; // absent old key ⇒ serde default
        migrate(&mut p);
        assert_eq!(p.config_version, CONFIG_VERSION);
        assert!(!p.hide_toolbar_fullscreen, "pre-v5 configs land on the new default OFF");
    }

    #[test]
    fn migrate_v6_bumps_old_default_window_padding() {
        // A pre-v6 config still on the previous default (36) → the new default (50).
        let mut p = defaults();
        p.config_version = 5;
        p.window_padding_px = 36;
        migrate(&mut p);
        assert_eq!(p.window_padding_px, 50);
        assert_eq!(p.config_version, CONFIG_VERSION);
        // An explicitly chosen padding is preserved.
        let mut q = defaults();
        q.config_version = 5;
        q.window_padding_px = 24;
        migrate(&mut q);
        assert_eq!(q.window_padding_px, 24);
    }

    #[test]
    fn migrate_v7_maps_window_border_style_to_explicit_border_fields() {
        // DRAGON-191: each old style maps into the new Active/Inactive width fields +
        // the drop-shadow toggle. Colours stay at the new defaults regardless.
        let case = |style: u8| {
            let mut p = defaults();
            p.config_version = 6;
            p.window_border_style = style;
            // Ensure the new fields start at their defaults so the mapping is what's tested.
            p.active_border_width = 3;
            p.inactive_border_width = 1;
            p.window_drop_shadow = true;
            p.window_single_active = true;
            migrate(&mut p);
            assert_eq!(p.config_version, CONFIG_VERSION);
            p
        };
        // Style 0 (Active + shadow): keep default widths, keep the shadow, single=Active.
        let s0 = case(0);
        assert_eq!((s0.active_border_width, s0.inactive_border_width), (3, 1));
        assert!(s0.window_drop_shadow);
        assert!(s0.window_single_active);
        // Style 1 (Inactive + shadow): same widths, shadow kept, single=Inactive.
        let s1 = case(1);
        assert_eq!((s1.active_border_width, s1.inactive_border_width), (3, 1));
        assert!(s1.window_drop_shadow);
        assert!(!s1.window_single_active);
        // Style 2 (Inactive, no shadow): widths kept, shadow dropped, single=Inactive.
        let s2 = case(2);
        assert_eq!((s2.active_border_width, s2.inactive_border_width), (3, 1));
        assert!(!s2.window_drop_shadow);
        assert!(!s2.window_single_active);
        // Style 3 (Raw): no border at all, no shadow.
        let s3 = case(3);
        assert_eq!((s3.active_border_width, s3.inactive_border_width), (0, 0));
        assert!(!s3.window_drop_shadow);
        // Colours land on the new defaults for every style.
        assert_eq!(s3.active_border_color, None);
        assert_eq!(s3.inactive_border_color, [65, 69, 80, 255]);
    }

    #[test]
    fn window_border_style_is_never_written() {
        // v7 retired the field: it must not appear on disk (its presence is what marks
        // a pre-v7 config for the one-time migration).
        let s = toml::to_string(&defaults()).expect("serialize");
        assert!(!s.contains("window_border_style"), "got: {s}");
        // The new border fields round-trip.
        let q: super::Persisted = toml::from_str(&s).expect("parse back");
        assert_eq!(q.active_border_width, 3);
        assert_eq!(q.inactive_border_width, 1);
        assert_eq!(q.inactive_border_color, [65, 69, 80, 255]);
        assert!(q.window_drop_shadow);
        // active_border_color = None is omitted (Option skip_serializing_if). Match the
        // KEY line ("active_border_color =") so the "inactive_border_color" substring
        // doesn't false-positive.
        assert!(
            !s.lines().any(|l| l.trim_start().starts_with("active_border_color")),
            "None accent-follow omitted: {s}"
        );
    }

    #[test]
    fn old_recording_tray_key_is_ignored_on_load() {
        // A real old config on disk carries `recording_tray = true` (its old default). The
        // v5 rename removed that field, so serde must silently DROP the unknown key (not
        // fail the parse) and `hide_toolbar_fullscreen` must fall to its default OFF.
        let on_disk = "record_dir = \"~/Videos\"\nrecording_tray = true\n";
        let p: super::Persisted =
            toml::from_str(on_disk).expect("an old recording_tray key must not fail the parse");
        assert_eq!(p.record_dir, "~/Videos", "other settings survive");
        assert!(!p.hide_toolbar_fullscreen, "the retired key does not set the new one");
    }
}
