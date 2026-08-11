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
///
/// Public since DRAGON-419: `diag::init` runs as the first statement of `main` and needs the
/// one `debug_logging` key BEFORE the daemon branch or any subcommand returns. It reads this
/// file directly rather than calling [`load`], which runs migrations and can WRITE — side
/// effects that must not happen that early.
pub fn config_path() -> Option<PathBuf> {
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

// A `file_exists()` helper lived here, the "is this the very first launch?"
// probe behind the retired capture-method auto-default. DRAGON-575 removed both:
// see the tombstone at `CaptureMsg::PipewireProbed` (app/update/capture.rs) for
// why a file-existence trigger could never be a reliable first-launch signal.

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
pub const CONFIG_VERSION: u32 = 12;

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
    // v8 (DRAGON-355) fanned the combined "Automatically save & close on copy" key out into
    // `preview_save_on_copy` + `preview_close_on_copy`. DRAGON-467 deleted all three, so the
    // hook has nothing left to write and is gone with them. The note stays because the shape
    // is the lesson: a deprecated `skip_serializing` field plus a guarded hook is how a
    // SPLIT is done here, and the v3 capture booleans are the other instance.
    if p.config_version < 9 {
        // v9 (DRAGON-467): the six share-automation keys retire with NOTHING carried
        // forward, and that is the deliberate part.
        //
        // `preview_save_on_copy` / `preview_close_on_copy` / `preview_copy_on_delete` (and
        // their three `preview_video_*` twins) each modified an ACTION that no longer behaves
        // that way: Copy now copies the current state and nothing else, and Delete needs no
        // courtesy copy because every capture reaches the clipboard the moment it is taken.
        // There is no successor setting whose meaning is close enough to inherit a value.
        //
        // The tempting mapping is `preview_save_on_copy` -> `preview_save_originals`, on the
        // grounds that both are about "do I want files on disk". It is REFUSED, and the
        // asymmetry is why: both default ON, so the only value worth carrying is an OFF, and
        // an OFF there meant "a Copy shouldn't also write the -edited sibling" while an OFF
        // here means "don't write my captures to my folder at all". Carrying it would turn a
        // narrow opt-out into captures that live only in the runtime directory — data the
        // user thought was saved, quietly not. A migration may not invent that risk.
        //
        // Nothing to write, then: the old keys are absent from the schema, serde ignores
        // unknown fields (no `deny_unknown_fields` on `Persisted`), and the three new keys
        // per media kind take their `default_true`. That default reproduces today's
        // observable behaviour for everything that survived — captures still auto-save, the
        // unsaved-changes card still appears — so an upgrading user sees no jump. Same
        // treatment as `recording_tray` (v5) and `allow_multiple`; pinned by
        // `old_share_automation_keys_are_ignored_on_load`.
    }
    if p.config_version < 10 {
        // v10 (DRAGON-482): the two cloud-accounts keys arrive, and there is nothing to
        // carry forward because there was no such feature before.
        //
        // The version still bumps, and the hook still exists, because BOTH keys are
        // additive with serde defaults (`cloud_last_account` absent means "no account
        // picked yet", `cloud_auto_share` absent means its `default_true`), which is
        // exactly the state an upgrading config should land in. Writing anything here
        // would be inventing a preference the user never expressed.
        //
        // The version number earns its keep as a MARKER rather than as a migration: it
        // records which builds could have written the keys at all, which is what a later
        // change to their meaning would need. Same shape as the v5 and v9 entries above,
        // both of which also carry nothing.
    }
    if p.config_version < 11 {
        // v11 (DRAGON-570): the single ScreenCast restore-token pair
        // (`pw_restore_token` + `pw_restore_source`) became one slot per source
        // type, so a window grant no longer discards the monitor token. The legacy
        // pair is read ONCE here, routed into the slot its recorded source names,
        // and never written again (both fields are skip_serializing now, the v3
        // capture-boolean shape). A `None` or unknown source drops the token: it
        // never replayed under the DRAGON-544 gate either, so nothing is lost.
        //
        // The carried token is best-effort on top of that: it was minted under
        // persist mode 1, whose grant died with the granting process's D-Bus
        // connection (the DRAGON-570 root cause), so the portal will likely
        // re-prompt once and reissue a mode-2 token into the same slot. Carrying
        // it costs nothing and covers portals that honored the old token.
        if let Some(tok) = p.pw_restore_token.take() {
            match p.pw_restore_source.as_deref() {
                Some("monitor") if p.pw_restore_token_monitor.is_none() => {
                    p.pw_restore_token_monitor = Some(tok);
                }
                Some("window") if p.pw_restore_token_window.is_none() => {
                    p.pw_restore_token_window = Some(tok);
                }
                _ => {}
            }
        }
        p.pw_restore_source = None;
    }
    if p.config_version < 12 {
        // v12 (DRAGON-571): `preferred_encoder` used to default to an "auto" sentinel
        // that the app REPLACED with the best probed encoder on first access and
        // PERSISTED as if the user had chosen it. Any transient first-launch condition
        // (driver asleep, NVENC sessions held by a game or overlay, the ffmpeg sidecar
        // mid-install) therefore pinned "software" permanently; nothing ever
        // reconsidered. "auto" now stays persisted and resolves per recording (the
        // hint-first ladder over `encoder_auto_hint`).
        //
        // Every pre-v12 CONCRETE id migrates back to "auto" ONCE. On disk a real user
        // pick and a persisted auto-resolution are indistinguishable (both were
        // written through the same save), so we cannot know retroactively which were
        // real picks. A genuine picker choice costs that user one re-pick; the
        // alternative leaves every mis-pinned config broken forever. The old id seeds
        // the last-known-good hint (only when the hint is still empty), so a config
        // whose auto-pick was correct keeps its single happy-path probe. A "software"
        // seed is harmless: the probe order ignores software hints, since hoisting one
        // would re-create the exact pin this migration removes.
        //
        // The legacy `record_hardware = false` mapping to software is a read-time rule
        // and stays honored regardless of this reset.
        if p.preferred_encoder != "auto" {
            if p.encoder_auto_hint.is_empty() {
                p.encoder_auto_hint = p.preferred_encoder.clone();
            }
            p.preferred_encoder = "auto".to_string();
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

/// Update the persisted last-known-good AUTO encoder hint (DRAGON-571). The hint is
/// a CACHE, never user intent (`preferred_encoder` is written only by a real settings
/// pick), so it is writable from wherever resolution actually ran: in practice the
/// recording workers, off the UI thread, once at session start. A whole-file
/// load-then-save is safe against the settings window's own saves because the app's
/// snapshot carries this field from DISK, not from a live mirror (the
/// `mac_first_run_seen` pattern in `app/persist.rs`), so a later settings save cannot
/// revert a fresh note. The reverse race, this load-then-save landing over a
/// concurrent save from another writer, is the same last-writer-wins window every
/// existing `save` already has, only microseconds wide here. No-op when unchanged.
pub fn note_encoder_auto_hint(id: &str) {
    let mut p = load();
    if p.encoder_auto_hint == id {
        return;
    }
    p.encoder_auto_hint = id.to_string();
    save(&p);
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
        // DRAGON-582: `recent_colors` is a second ARRAY declared after the array of
        // tables, which is the one shape this test exists to catch. An array of plain
        // strings is a VALUE to toml, not a table, so it must survive alongside them.
        p.color_picker_overlay_opacity = 0.4;
        p.recent_colors = vec!["#FF8800".into(), "#123456".into()];
        // DRAGON-630: the remembered mode is a scalar declared after that array.
        p.color_picker_mode = "oklch".into();
        let s = toml::to_string(&p).expect("serialize must succeed");
        let back: Persisted = toml::from_str(&s).expect("deserialize must succeed");
        assert_eq!(back.capture_hotkey, "F13", "scalar after covermark_prefs survived");
        assert_eq!(back.region_overlay_opacity, 0.42);
        assert_eq!(back.covermark_prefs.len(), 1);
        assert_eq!(back.color_picker_overlay_opacity, 0.4);
        assert_eq!(back.recent_colors, vec!["#FF8800", "#123456"]);
        assert_eq!(back.color_picker_mode, "oklch", "the chosen mode round-trips");
    }

    /// DRAGON-582: the colour picker's own defaults, which a fresh install ships with.
    /// The dim is 33%, the same as the ACTIVE overlay, so the two overlays that sit over a
    /// working desktop match. It was briefly zero (DRAGON-588) and the owner took it back:
    /// with no dim, nothing says the picker is armed. Nothing has been picked yet.
    #[test]
    fn the_colour_picker_defaults_are_the_owners() {
        let d = defaults();
        assert_eq!(d.color_picker_overlay_opacity, 0.33);
        assert_eq!(
            d.color_picker_overlay_opacity, d.active_overlay_opacity,
            "the picker dim tracks the active overlay's, which is what 'like the other' means"
        );
        assert!(d.recent_colors.is_empty());
        assert!(d.color_picker_hotkey.is_empty(), "the global hotkey ships UNBOUND");
        // DRAGON-630: a fresh install (and any config predating the key) opens the value
        // row in HEX, which is what the tool always copied before modes existed.
        assert_eq!(d.color_picker_mode, "hex");
        // DRAGON-615: a fresh install opens the magnifier exactly where it always did, so
        // persisting the zoom changes nothing for anyone who has not touched it.
        assert_eq!(
            d.color_picker_zoom,
            crate::app::color_picker::geom::MAGNIFIER_ZOOM_DEFAULT,
            "the stored default must be the picker's own opening lens"
        );
    }

    /// DRAGON-615: an existing config predates the zoom key entirely, and must read as the
    /// default rather than as a zero that would ask for no magnification at all. This is the
    /// "do not write the default eagerly" contract seen from the load side.
    #[test]
    fn a_config_without_the_zoom_key_reads_as_the_default() {
        let p: Persisted =
            toml::from_str("record_fps = 30\n").expect("an old config still deserializes");
        assert_eq!(
            p.color_picker_zoom,
            crate::app::color_picker::geom::MAGNIFIER_ZOOM_DEFAULT
        );
        assert_eq!(p.record_fps, 30, "and the keys it DOES have are untouched");
    }

    /// DRAGON-615: a zoom the user actually chose survives a save and load unchanged. The
    /// canary style of `config_roundtrips_with_populated_covermark_prefs`: it is a scalar
    /// declared after an array, which is the shape TOML is fussy about.
    #[test]
    fn a_chosen_zoom_roundtrips() {
        let mut p = defaults();
        p.color_picker_zoom = crate::app::color_picker::geom::MAGNIFIER_ZOOM_MAX;
        let s = toml::to_string(&p).expect("serialize must succeed");
        let back: Persisted = toml::from_str(&s).expect("deserialize must succeed");
        assert_eq!(
            back.color_picker_zoom,
            crate::app::color_picker::geom::MAGNIFIER_ZOOM_MAX
        );
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
        assert_eq!(d.text_confidence, 20.0);
        assert_eq!(d.region_overlay_opacity, 0.66);
        assert_eq!(d.record_fps, 30);
        // DRAGON-467's preview-editor convenience toggles default ON (the settings-copy house
        // style). `preview_save_originals` ON is also what keeps the upgrade invisible: it is
        // the setting that decides whether a capture reaches the user's folder at all.
        assert!(d.preview_copy_on_exit);
        assert!(d.preview_save_originals);
        assert!(d.preview_ask_to_save);
        // The Video Editor group's three match the image defaults exactly, so an untouched
        // install answers the same question the same way for both media kinds.
        assert!(d.preview_video_copy_on_exit);
        assert!(d.preview_video_save_originals);
        assert!(d.preview_video_ask_to_save);
        // DRAGON-478: the top bar's group captions ship ON — the labels are the point of the
        // change, and OFF is the opt-out back to the historical chrome height.
        assert!(d.preview_toolbar_labels);
        assert_eq!(d.record_res_preset, 5); // 2K
        assert_eq!(d.nvenc_preset, "p4");
        assert_eq!(d.x264_preset, "fast");
        // Pinned to the constant, not a literal: a version bump is a routine, guarded event
        // (DRAGON-482 took it to 10), and a test that has to be hand-edited for each one adds
        // nothing the `migrate` steps do not already say.
        assert_eq!(d.config_version, CONFIG_VERSION);
        // DRAGON-482: a fresh install has no cloud account picked, and the share-link
        // convenience ships ON (the settings-copy convention for a convenience toggle).
        assert_eq!(d.cloud_last_account, None);
        assert!(d.cloud_auto_share);
        // DRAGON-174: the new toolbar-hiding setting defaults OFF (do not hide).
        assert!(!d.hide_toolbar_fullscreen);
        // DRAGON-584: residency defaults ON everywhere now, Linux included (the tray is the
        // app's primary launcher there, and on COSMIC it is the only live-recording control).
        // The login-item seed markers always start unset.
        assert!(d.resident);
        // DRAGON-296: launch-at-login defaults ON (gated by `resident` at reconcile time).
        assert!(d.autostart_on_login);
        assert!(!d.mac_login_item_seeded);
        assert!(!d.win_login_item_seeded);
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
                        shortcuts: [(PreviewCopy, None)])";
        let p: Persisted = ron::from_str(on_disk).expect("old RON must still parse");
        assert_eq!(p.region, Some((10, 20, 110, 220)));
        assert_eq!(p.delay_idx, 3);
        assert_eq!(p.pw_restore_token.as_deref(), Some("tok"));
        // The legacy TUPLE shortcut-override shape (incl. an unbound None) survives. The
        // action named has to be one this build still HAS: DRAGON-627 retired `CopyText`,
        // which stood here, and a retired name is dropped by design (see the tolerant-parse
        // tests below), which would have made this assert nothing.
        assert_eq!(p.shortcuts.len(), 1);
        assert_eq!(p.shortcuts[0].0, crate::shortcuts::Action::PreviewCopy);
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
        p.pw_restore_token_monitor = Some("yxxZ".into());
        p.pw_restore_token_window = Some("wxxW".into());
        p.record_bitrate_kbps = 2500;
        p.record_dir = "~/Videos".into();
        p.av_calibration_base_ms = -260;
        p.shortcuts = vec![(crate::shortcuts::Action::PreviewCopy, None)];
        let s = toml::to_string(&p).expect("serialize");
        let q: Persisted = toml::from_str(&s).expect("parse back");
        assert_eq!(q.region, p.region);
        assert_eq!(q.settings_size, p.settings_size);
        assert_eq!(q.pw_restore_token_monitor, p.pw_restore_token_monitor);
        assert_eq!(q.pw_restore_token_window, p.pw_restore_token_window);
        assert_eq!(q.record_bitrate_kbps, 2500);
        assert_eq!(q.record_dir, "~/Videos");
        assert_eq!(q.av_calibration_base_ms, -260);
        assert_eq!(q.shortcuts.len(), 1);
        assert_eq!(q.shortcuts[0].0, crate::shortcuts::Action::PreviewCopy);
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
                       action = \"PreviewCovermark\"\n";
        let p: Persisted = toml::from_str(on_disk).expect("stale override must not fail the parse");
        assert_eq!(p.record_dir, "~/Videos", "other settings survive");
        assert_eq!(p.shortcuts.len(), 1, "only the removed action's entry drops");
        assert_eq!(p.shortcuts[0].0, crate::shortcuts::Action::PreviewCovermark);

        let legacy = "(record_dir: \"~/Videos\", \
                      shortcuts: [(PreviewNoAi, None), (PreviewCovermark, None)])";
        let q: Persisted = ron::from_str(legacy).expect("stale RON override must not fail");
        assert_eq!(q.record_dir, "~/Videos");
        assert_eq!(q.shortcuts.len(), 1);
        assert_eq!(q.shortcuts[0].0, crate::shortcuts::Action::PreviewCovermark);
    }

    // DRAGON-617 BAKED five actions (Save, Upload, Close, Undo, Redo), the biggest single drop
    // the tolerant parse has had to absorb. A keymap saved by an older build can name all five
    // AND carry the user's real customisations beside them, so this pins the thing that
    // actually matters: the five fall away and everything else the user changed survives,
    // rather than one stale name resetting the whole config through load_raw's defaults.
    #[test]
    fn the_baked_five_drop_and_the_users_other_bindings_survive() {
        use crate::shortcuts::Action;
        let mut on_disk = String::from("record_dir = \"~/Videos\"\nrecord_fps = 60\n");
        for baked in ["PreviewSave", "PreviewUpload", "PreviewCancel", "PreviewUndo", "PreviewRedo"]
        {
            on_disk.push_str(&format!("[[shortcuts]]\naction = \"{baked}\"\n"));
        }
        // A real customisation, sitting AFTER the stale entries, so a parse that gave up part
        // way through would lose it.
        on_disk.push_str(
            "[[shortcuts]]\naction = \"PreviewCopy\"\n[shortcuts.shortcut]\nctrl = true\n\
             key = { Char = \"q\" }\n",
        );
        let p: Persisted =
            toml::from_str(&on_disk).expect("five stale overrides must not fail the parse");
        assert_eq!(p.record_dir, "~/Videos", "settings before the stale block survive");
        assert_eq!(p.record_fps, 60);
        assert_eq!(p.shortcuts.len(), 1, "exactly the five baked entries drop");
        assert_eq!(p.shortcuts[0].0, Action::PreviewCopy);
        assert!(p.shortcuts[0].1.is_some(), "the surviving override keeps its chord");

        // The legacy RON shape drops them too, since that is the format a long-installed
        // config is most likely to still be in.
        let legacy = "(record_dir: \"~/Videos\", shortcuts: [(PreviewUndo, None), \
                      (PreviewRedo, None), (PreviewCovermark, None)])";
        let q: Persisted = ron::from_str(legacy).expect("stale RON overrides must not fail");
        assert_eq!(q.shortcuts.len(), 1);
        assert_eq!(q.shortcuts[0].0, Action::PreviewCovermark);
    }

    // DRAGON-627 BAKED the scanner's three OCR text actions. They are the OLDEST names the
    // tolerant parse has had to absorb (`CopyText` / `SelectAllText` / `DeselectText` shipped
    // in the first build), so a long-lived config is the likeliest place to still hold one,
    // and one stale name must not reset every other setting through load_raw's defaults.
    #[test]
    fn the_baked_ocr_three_drop_and_the_users_other_bindings_survive() {
        use crate::shortcuts::Action;
        let mut on_disk = String::from("record_dir = \"~/Videos\"\nrecord_fps = 60\n");
        for baked in ["CopyText", "SelectAllText", "DeselectText"] {
            on_disk.push_str(&format!("[[shortcuts]]\naction = \"{baked}\"\n"));
        }
        // A real customisation, sitting AFTER the stale entries, so a parse that gave up part
        // way through would lose it. It is deliberately on primary+C, the chord `CopyText`
        // used to hold: the two never collided (different surfaces) and the rebind must
        // survive the removal exactly as it was.
        on_disk.push_str(
            "[[shortcuts]]\naction = \"PreviewCopy\"\n[shortcuts.shortcut]\nctrl = true\n\
             key = { Char = \"c\" }\n",
        );
        let p: Persisted =
            toml::from_str(&on_disk).expect("three stale overrides must not fail the parse");
        assert_eq!(p.record_dir, "~/Videos", "settings before the stale block survive");
        assert_eq!(p.record_fps, 60);
        assert_eq!(p.shortcuts.len(), 1, "exactly the three baked entries drop");
        assert_eq!(p.shortcuts[0].0, Action::PreviewCopy);
        assert!(p.shortcuts[0].1.is_some(), "the surviving override keeps its chord");

        // And in the legacy RON shape, beside a live entry, for the same reason.
        let legacy = "(record_dir: \"~/Videos\", shortcuts: [(SelectAllText, None), \
                      (DeselectText, None), (PreviewCovermark, None)])";
        let q: Persisted = ron::from_str(legacy).expect("stale RON overrides must not fail");
        assert_eq!(q.record_dir, "~/Videos");
        assert_eq!(q.shortcuts.len(), 1);
        assert_eq!(q.shortcuts[0].0, Action::PreviewCovermark);
    }

    // DRAGON-392 RENAMED an action (`PreviewTogglePan` → `PreviewAnnotHand`, when panning became
    // a real tool). A rename is NOT a removal: a user who had rebound the pan key must keep that
    // binding, so the old spelling is carried by `#[serde(alias)]` and must still LOAD — in both
    // on-disk shapes. (Contrast the test above: an action that genuinely no longer exists drops.)
    #[test]
    fn the_renamed_pan_action_still_loads_under_its_old_name() {
        let on_disk = "[[shortcuts]]\naction = \"PreviewTogglePan\"\n";
        let p: Persisted = toml::from_str(on_disk).expect("the alias must parse");
        assert_eq!(p.shortcuts.len(), 1, "the override survives the rename");
        assert_eq!(p.shortcuts[0].0, crate::shortcuts::Action::PreviewAnnotHand);

        let legacy = "(shortcuts: [(PreviewTogglePan, None)])";
        let q: Persisted = ron::from_str(legacy).expect("the alias must parse from RON too");
        assert_eq!(q.shortcuts.len(), 1);
        assert_eq!(q.shortcuts[0].0, crate::shortcuts::Action::PreviewAnnotHand);
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
        assert!(!has_key("pw_restore_token_monitor ="));
        assert!(!has_key("pw_restore_token_window ="));
        let q: Persisted = toml::from_str(&s).expect("parse back");
        assert_eq!(q.region, None);
        assert_eq!(q.settings_size, None);
    }

    // DRAGON-296: the Windows tray daemon owns the global capture hotkey, so residency defaults
    // ON there (matching macOS); the one-time login-item seed marker starts unset, must survive a
    // save (so the seed never re-runs and re-overrides an explicit opt-out), and loads as false
    // from an old config that predates the field.
    #[test]
    fn windows_resident_defaults_on_and_seed_marker_round_trips() {
        let d = defaults();
        #[cfg(windows)]
        assert!(d.resident, "Windows residency defaults on (DRAGON-296)");
        // DRAGON-584: Linux joined them. It defaulted off while the one-shot model plus a
        // COSMIC PrintScreen shortcut was the whole story; the tray is now the primary
        // launcher and, on COSMIC, the only control for a live recording.
        #[cfg(target_os = "linux")]
        assert!(d.resident, "Linux residency defaults on too (DRAGON-584)");
        assert!(!d.win_login_item_seeded, "the seed marker starts unset");
        let mut p = defaults();
        p.win_login_item_seeded = true;
        let s = toml::to_string(&p).expect("serialize");
        let q: Persisted = toml::from_str(&s).expect("parse back");
        assert!(q.win_login_item_seeded, "a set marker survives a save round trip");
        let old: Persisted = toml::from_str("record_dir = \"~/Videos\"").expect("parse");
        assert!(!old.win_login_item_seeded, "an old config without the key loads false");
    }

    // The remembered STEP-MARKER side is persisted (it used to be session-only): a set value
    // must survive a save so a later launch spawns markers at it, and an old config that
    // predates the key must load as UNSET (0.0) — which the reader turns into
    // `DEFAULT_BADGE_SIZE`, never into a zero-sized marker.
    #[test]
    fn annot_badge_size_persists_and_reads_unset_from_an_old_config() {
        assert_eq!(defaults().annot_badge_size, 0.0, "fresh install remembers nothing");
        let mut p = defaults();
        p.annot_badge_size = 132.0;
        let s = toml::to_string(&p).expect("serialize");
        let q: Persisted = toml::from_str(&s).expect("parse back");
        assert_eq!(q.annot_badge_size, 132.0, "a remembered side survives a save round trip");
        // An old config (no key at all) parses cleanly and reads as unset — no migration owed.
        let old: Persisted =
            toml::from_str("annot_stroke_w = 8.0\n").expect("an old config must still parse");
        assert_eq!(old.annot_stroke_w, 8.0, "its other annotation settings survive");
        assert_eq!(old.annot_badge_size, 0.0, "absent key = unset (falls back to the default)");
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
    fn migrate_v11_lands_the_legacy_token_in_its_matching_slot() {
        // DRAGON-570: the legacy single pair routes into the slot its recorded
        // source names, in BOTH directions, and is consumed (never written again).
        let mut m = defaults();
        m.config_version = 10;
        m.pw_restore_token = Some("mtok".into());
        m.pw_restore_source = Some("monitor".into());
        migrate(&mut m);
        assert_eq!(m.config_version, CONFIG_VERSION);
        assert_eq!(m.pw_restore_token_monitor.as_deref(), Some("mtok"));
        assert_eq!(m.pw_restore_token_window, None, "the other slot stays empty");
        assert_eq!(m.pw_restore_token, None, "the legacy token is consumed");
        assert_eq!(m.pw_restore_source, None);

        let mut w = defaults();
        w.config_version = 10;
        w.pw_restore_token = Some("wtok".into());
        w.pw_restore_source = Some("window".into());
        migrate(&mut w);
        assert_eq!(w.pw_restore_token_window.as_deref(), Some("wtok"));
        assert_eq!(w.pw_restore_token_monitor, None);
        assert_eq!(w.pw_restore_token, None);
    }

    #[test]
    fn migrate_v11_drops_a_legacy_token_with_no_or_unknown_source() {
        // A token whose source was never recorded (pre-DRAGON-544) or is not a
        // slot we request ("virtual") never replayed under the old gate either,
        // so it lands nowhere: one portal prompt, then the slot self-heals.
        for src in [None, Some("virtual".to_string()), Some("garbage".to_string())] {
            let mut p = defaults();
            p.config_version = 10;
            p.pw_restore_token = Some("tok".into());
            p.pw_restore_source = src;
            migrate(&mut p);
            assert_eq!(p.pw_restore_token_monitor, None);
            assert_eq!(p.pw_restore_token_window, None);
            assert_eq!(p.pw_restore_token, None, "consumed either way");
        }
    }

    #[test]
    fn migrate_v11_never_clobbers_an_already_filled_slot() {
        // Defensive: a hand-edited pre-v11 file carrying both the legacy pair and
        // a new slot keeps the slot (the newer grant); the legacy token drops.
        let mut p = defaults();
        p.config_version = 10;
        p.pw_restore_token = Some("stale".into());
        p.pw_restore_source = Some("monitor".into());
        p.pw_restore_token_monitor = Some("fresh".into());
        migrate(&mut p);
        assert_eq!(p.pw_restore_token_monitor.as_deref(), Some("fresh"));
    }

    #[test]
    fn migrate_v12_resets_a_concrete_encoder_to_auto_and_seeds_the_hint() {
        // DRAGON-571: pre-v12 builds persisted the auto-RESOLVED encoder as if the
        // user chose it, so a pre-v12 concrete id cannot be trusted as intent. It
        // resets to "auto" once, seeding the last-known-good hint so a config whose
        // auto-pick was correct keeps its single-probe happy path.
        let mut p = defaults();
        p.config_version = 11;
        p.preferred_encoder = "nvenc".to_string();
        migrate(&mut p);
        assert_eq!(p.config_version, CONFIG_VERSION);
        assert_eq!(p.preferred_encoder, "auto");
        assert_eq!(p.encoder_auto_hint, "nvenc");
    }

    #[test]
    fn migrate_v12_resets_a_software_pin_too() {
        // The mis-pinned "software" config this migration exists for resets like any
        // other concrete id. Its hint seed is harmless: the probe order ignores
        // software hints (see `encode::auto_probe_order`), so the next auto
        // resolution walks the full ladder and recovers the hardware encoder.
        let mut p = defaults();
        p.config_version = 11;
        p.preferred_encoder = "software".to_string();
        migrate(&mut p);
        assert_eq!(p.preferred_encoder, "auto");
        assert_eq!(p.encoder_auto_hint, "software");
    }

    #[test]
    fn migrate_v12_leaves_auto_and_an_existing_hint_alone() {
        // "auto" has nothing to reset and must not invent a hint.
        let mut p = defaults();
        p.config_version = 11;
        migrate(&mut p);
        assert_eq!(p.preferred_encoder, "auto");
        assert_eq!(p.encoder_auto_hint, "");
        // A pre-v12 file that somehow already carries a hint keeps it: the hint may
        // be fresher than the stale pick it would be seeded from.
        let mut q = defaults();
        q.config_version = 11;
        q.preferred_encoder = "software".to_string();
        q.encoder_auto_hint = "vaapi".to_string();
        migrate(&mut q);
        assert_eq!(q.preferred_encoder, "auto");
        assert_eq!(q.encoder_auto_hint, "vaapi");
    }

    #[test]
    fn migrate_v12_runs_once_a_current_concrete_pick_is_user_intent() {
        // The other direction: from v12 on, only a real settings-picker click writes
        // a concrete id, so a current config's pick must survive every later load
        // untouched (the one-time reset never repeats).
        let mut p = defaults();
        p.preferred_encoder = "nvenc".to_string(); // config_version is already current
        migrate(&mut p);
        assert_eq!(p.preferred_encoder, "nvenc", "a post-v12 concrete id is user intent");
        assert_eq!(p.encoder_auto_hint, "");
    }

    #[test]
    fn legacy_restore_pair_is_never_written_again() {
        // The retired pair is skip_serializing: even a struct that still carries
        // values (mid-migration state) writes only the per-source slots. Key-LINE
        // matching, because "pw_restore_token_monitor" contains the legacy name as
        // a substring.
        let mut p = defaults();
        p.pw_restore_token = Some("stale".into());
        p.pw_restore_source = Some("monitor".into());
        p.pw_restore_token_monitor = Some("kept".into());
        let s = toml::to_string(&p).expect("serialize");
        let has_key = |k: &str| s.lines().any(|l| l.trim_start().starts_with(k));
        assert!(!has_key("pw_restore_token ="), "legacy token written: {s}");
        assert!(!has_key("pw_restore_source"), "legacy source written: {s}");
        assert!(has_key("pw_restore_token_monitor ="), "the slot IS written: {s}");
        // And a config that still carries the legacy keys on disk parses them for
        // the migration (the read direction stays alive).
        let old = "pw_restore_token = \"tok\"\npw_restore_source = \"window\"\n";
        let mut q: Persisted = toml::from_str(old).expect("legacy keys must still parse");
        migrate(&mut q);
        assert_eq!(q.pw_restore_token_window.as_deref(), Some("tok"));
    }

    /// **ALL SIX preview-editor toggles default ON, on BOTH media kinds** (DRAGON-467, owner's
    /// requirement). Its own test rather than a line in `fresh_defaults_use_serde`, because
    /// the three ways a default can be stated have to AGREE and each can regress alone:
    ///
    /// 1. the `#[serde(default = …)]` attribute on the field, which is what an absent key
    ///    deserializes to (an old config, or a hand-trimmed one);
    /// 2. `defaults()`, which is what a fresh install gets AND what the settings page's
    ///    per-row "reset to default" writes;
    /// 3. an EMPTY document, which is the path `defaults()` itself takes.
    ///
    /// A regression in any one of them would silently flip a toggle off for some users and
    /// not others, which is exactly the class of bug a single assertion site would miss.
    #[test]
    fn every_preview_editor_toggle_defaults_on_for_both_media_kinds() {
        // (2) and (3): the fresh-install path, and the empty document behind it.
        let from_defaults = defaults();
        let from_empty: super::Persisted =
            toml::from_str("").expect("an empty config must parse to defaults");
        // (1): a config that carries OTHER keys but none of these — an upgrading user.
        let mut from_old: super::Persisted =
            toml::from_str("record_fps = 60\nconfig_version = 8\n").expect("parse");
        migrate(&mut from_old);

        for (label, p) in [
            ("defaults()", &from_defaults),
            ("empty document", &from_empty),
            ("upgraded config", &from_old),
        ] {
            // The named rows, image side then video side. Listed one per line on purpose: a
            // field added to either group should look wrong here until it is answered for.
            for (row, on) in [
                ("image: automatically copy changes on exit", p.preview_copy_on_exit),
                ("image: automatically save originals", p.preview_save_originals),
                ("image: ask to save edited screenshots", p.preview_ask_to_save),
                ("video: automatically copy changes on exit", p.preview_video_copy_on_exit),
                ("video: automatically save originals", p.preview_video_save_originals),
                ("video: ask to save edited videos", p.preview_video_ask_to_save),
            ] {
                assert!(on, "{label}: {row} must default ON");
            }
        }
        // And the three sources agree with each other, not merely with `true` — a future
        // default that is deliberately changed has to be changed in ONE place.
        assert_eq!(from_defaults.preview_save_originals, from_empty.preview_save_originals);
        assert_eq!(
            from_defaults.preview_video_save_originals,
            from_empty.preview_video_save_originals
        );
    }

    #[test]
    fn old_share_automation_keys_are_ignored_on_load() {
        // DRAGON-467 v9: the six retired keys (and the pre-v8 combined one that fed two of
        // them) must load without error and WITHOUT touching anything else. This is the whole
        // migration story — nothing is carried forward, so the proof obligation is "an old
        // config still works", not "a value moved".
        let old = "\
preview_save_close_on_copy = false\n\
preview_save_on_copy = false\n\
preview_close_on_copy = false\n\
preview_copy_on_delete = false\n\
preview_video_save_on_copy = false\n\
preview_video_close_on_copy = false\n\
preview_video_copy_on_delete = false\n\
record_fps = 60\n\
screenshot_dir = \"~/Shots\"\n\
config_version = 8\n";
        let mut p: super::Persisted =
            toml::from_str(old).expect("a config carrying the retired keys must still parse");
        migrate(&mut p);
        assert_eq!(p.config_version, CONFIG_VERSION);
        // Unrelated settings on the same file survive untouched.
        assert_eq!(p.record_fps, 60);
        assert_eq!(p.screenshot_dir, "~/Shots");
        // The successors take their defaults. In particular a `preview_save_on_copy = false`
        // does NOT become `preview_save_originals = false`: see the v9 note in `migrate` for
        // why that tempting mapping would silently stop saving the user's captures.
        assert!(p.preview_save_originals, "an OFF save-on-copy must not disable saving originals");
        assert!(p.preview_copy_on_exit && p.preview_ask_to_save);
        assert!(
            p.preview_video_save_originals
                && p.preview_video_copy_on_exit
                && p.preview_video_ask_to_save
        );
        // And none of the retired keys comes back on the next write.
        let written = toml::to_string(&p).expect("serialize");
        for key in [
            "preview_save_close_on_copy",
            "preview_save_on_copy",
            "preview_close_on_copy",
            "preview_copy_on_delete",
            "preview_video_save_on_copy",
            "preview_video_close_on_copy",
            "preview_video_copy_on_delete",
        ] {
            assert!(!written.contains(key), "retired key {key} reappeared: {written}");
        }
    }

    #[test]
    fn video_editor_preview_settings_are_independent_of_the_image_ones() {
        // Six SEPARATE persisted fields, not three shared ones (the DRAGON-420 split, carried
        // into DRAGON-467's rows). The regression this pins is a video row wired to an image
        // field (or vice versa), which would look right in the settings window and silently
        // move the wrong behaviour.
        //
        // 1. A config that carries only the IMAGE keys picks the video ones up at their
        //    defaults without disturbing it — absence has to be enough on its own.
        let mut p: super::Persisted = toml::from_str(
            "preview_copy_on_exit = false\npreview_save_originals = false\n\
             preview_ask_to_save = false\nconfig_version = 9\n",
        )
        .expect("parse");
        migrate(&mut p);
        assert!(!p.preview_copy_on_exit && !p.preview_save_originals && !p.preview_ask_to_save);
        assert!(
            p.preview_video_copy_on_exit
                && p.preview_video_save_originals
                && p.preview_video_ask_to_save,
            "absent video keys default ON regardless of the image choices"
        );
        // 2. And the reverse: video off, image untouched.
        let mut q: super::Persisted = toml::from_str(
            "preview_video_copy_on_exit = false\npreview_video_save_originals = false\n\
             preview_video_ask_to_save = false\nconfig_version = 9\n",
        )
        .expect("parse");
        migrate(&mut q);
        assert!(
            q.preview_copy_on_exit && q.preview_save_originals && q.preview_ask_to_save,
            "the image settings must not follow the video ones"
        );
        assert!(
            !q.preview_video_copy_on_exit
                && !q.preview_video_save_originals
                && !q.preview_video_ask_to_save
        );
        // 3. A fully parted choice round-trips through the file: every one of the six is its
        //    own key on disk, so nothing collapses into a shared value on the next load.
        let mut parted = defaults();
        parted.preview_copy_on_exit = false;
        parted.preview_save_originals = true;
        parted.preview_ask_to_save = false;
        parted.preview_video_copy_on_exit = true;
        parted.preview_video_save_originals = false;
        parted.preview_video_ask_to_save = true;
        let s = toml::to_string(&parted).expect("serialize");
        let back: super::Persisted = toml::from_str(&s).expect("parse back");
        assert_eq!(
            (
                back.preview_copy_on_exit,
                back.preview_save_originals,
                back.preview_ask_to_save,
                back.preview_video_copy_on_exit,
                back.preview_video_save_originals,
                back.preview_video_ask_to_save,
            ),
            (false, true, false, true, false, true)
        );
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

    #[test]
    fn old_preview_float_cosmic_key_is_ignored_on_load() {
        // DRAGON-317 removed the `preview_float_cosmic` setting. An existing config still
        // carries the key; serde must silently DROP it (no `deny_unknown_fields`, so an
        // unknown key is ignored) and every other setting must survive — a stale key must
        // never fail the whole parse (which `load()` answers with `defaults()`, wiping all
        // settings). Covers BOTH on-disk formats: TOML (current) and legacy RON.
        let toml_on_disk = "record_dir = \"~/Videos\"\npreview_float_cosmic = true\n";
        let p: super::Persisted =
            toml::from_str(toml_on_disk).expect("an old preview_float_cosmic key must not fail");
        assert_eq!(p.record_dir, "~/Videos", "other settings survive the retired key");

        let ron_on_disk = "(record_dir: \"~/Videos\", preview_float_cosmic: true)";
        let q: super::Persisted =
            ron::from_str(ron_on_disk).expect("a legacy RON preview_float_cosmic key must not fail");
        assert_eq!(q.record_dir, "~/Videos", "other settings survive in RON too");
    }

    #[test]
    fn old_allow_multiple_key_is_ignored_on_load() {
        // DRAGON-351 removed the `allow_multiple` ("Allow multiple capture instances")
        // setting — the behaviour is unconditional now. EVERY existing config carries the
        // key (it was always serialized, both values), so serde must silently DROP it: an
        // unknown key must never fail the parse, because `load()` answers a failed parse
        // with `defaults()`, which would wipe every other setting. Same treatment as the
        // retired `recording_tray` / `preview_float_cosmic` keys, and both on-disk formats
        // are covered: TOML (current) and legacy RON.
        for value in ["true", "false"] {
            let toml_on_disk =
                format!("record_dir = \"~/Videos\"\nallow_multiple = {value}\nrecord_fps = 60\n");
            let p: super::Persisted = toml::from_str(&toml_on_disk)
                .unwrap_or_else(|e| panic!("a retired allow_multiple = {value} must not fail: {e}"));
            assert_eq!(p.record_dir, "~/Videos", "other settings survive the retired key");
            assert_eq!(p.record_fps, 60, "settings written AFTER the retired key survive too");
        }

        let ron_on_disk = "(record_dir: \"~/Videos\", allow_multiple: true, record_fps: 60)";
        let q: super::Persisted =
            ron::from_str(ron_on_disk).expect("a legacy RON allow_multiple key must not fail");
        assert_eq!(q.record_dir, "~/Videos", "other settings survive in RON too");
        assert_eq!(q.record_fps, 60);

        // And the key is never WRITTEN again: a fresh save carries no trace of it.
        let s = toml::to_string(&defaults()).expect("serialize");
        assert!(!s.contains("allow_multiple"), "got: {s}");
    }

    #[test]
    fn old_preview_workflow_keys_are_ignored_on_load() {
        // DRAGON-353 retired four preview-workflow settings — `auto_close_preview`
        // ("Automatically close the preview editor on save or copy"), `copy_to_clipboard`
        // ("Automatically copy to clipboard"), `preview_after_capture` ("Open in preview
        // editor") and `clipboard_max_mb` ("Clipboard size limit"). The first three
        // behaviours are unconditional now; the fourth is the fixed
        // `share::AUTO_COPY_MAX_MB` (the editor toasts a named error when it declines an
        // automatic copy for size, so the knob only pre-empted a readable failure). EVERY
        // existing config carries all four keys (each was always serialized), so serde must
        // silently DROP them: an unknown key must never fail the parse, because `load()`
        // answers a failed parse with `defaults()`, which would wipe every other setting.
        // Same treatment as the retired `allow_multiple` / `recording_tray` /
        // `preview_float_cosmic` keys, and both on-disk formats are covered: TOML (current)
        // and legacy RON.
        const RETIRED: [(&str, &str); 4] = [
            ("auto_close_preview", "true"),
            ("copy_to_clipboard", "true"),
            ("preview_after_capture", "true"),
            ("clipboard_max_mb", "42"),
        ];
        for (key, sample) in RETIRED {
            for value in ["false", sample] {
                let toml_on_disk =
                    format!("record_dir = \"~/Videos\"\n{key} = {value}\nrecord_fps = 60\n");
                let p: super::Persisted = toml::from_str(&toml_on_disk)
                    .unwrap_or_else(|e| panic!("a retired {key} = {value} must not fail: {e}"));
                assert_eq!(p.record_dir, "~/Videos", "settings BEFORE {key} survive");
                assert_eq!(p.record_fps, 60, "settings AFTER {key} survive");
            }
            let ron_on_disk =
                format!("(record_dir: \"~/Videos\", {key}: {sample}, record_fps: 60)");
            let q: super::Persisted = ron::from_str(&ron_on_disk)
                .unwrap_or_else(|e| panic!("a legacy RON {key} must not fail: {e}"));
            assert_eq!(q.record_dir, "~/Videos");
            assert_eq!(q.record_fps, 60);
        }

        // A config carrying ALL FOUR at once (what every real upgrade looks like) still
        // loads with every remaining setting intact.
        let all = "record_dir = \"~/Videos\"\nauto_close_preview = false\n\
                   copy_to_clipboard = false\npreview_after_capture = false\n\
                   clipboard_max_mb = 42\nrecord_fps = 60\n";
        let p: super::Persisted =
            toml::from_str(all).expect("a config with every retired key must still load");
        assert_eq!(p.record_dir, "~/Videos");
        assert_eq!(p.record_fps, 60, "the surviving settings still read");

        // And none of them is ever WRITTEN again.
        let s = toml::to_string(&defaults()).expect("serialize");
        for (key, _) in RETIRED {
            assert!(!s.contains(key), "{key} must not be written; got: {s}");
        }
    }
}
