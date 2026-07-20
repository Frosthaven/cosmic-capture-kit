use super::*;

impl App {
    pub(super) fn save_state(&self) {
        crate::state::save(&self.to_persisted());
    }

    /// The current settings as a `Persisted`.
    fn to_persisted(&self) -> crate::state::Persisted {
        // When the settings window is open (via `--settings` OR the gear button), this
        // instance is no longer the live region editor — its `self.region` is whatever
        // it loaded at launch, which is stale the moment another overlay draws a new
        // box. Writing it back would clobber that newer region (the "changing a setting
        // resets my drawn region" bug). So while settings are showing, keep whatever
        // region is currently on disk; only the actual region editor persists its own.
        let region = if self.settings.window.is_some() {
            crate::state::load().region
        } else {
            // Persist as the bare tuple so the on-disk RON shape never changes.
            self.region.map(|r| r.to_tuple())
        };
        crate::state::Persisted {
            region,
            delay_idx: self.delay_idx,
            capture_cursor: self.capture_cursor,
            capture_transparency: self.capture_transparency,
            no_wallpaper: !self.capture_wallpaper,
            // Deprecated (DRAGON-191): retired to the explicit border fields below and
            // never serialized (skip_serializing). Carry the schema default; it's dead
            // on disk now (only pre-v7 configs still hold it, for the one-time migration).
            window_border_style: 0,
            active_border_color: self.active_border_color,
            active_border_width: self.active_border_width,
            inactive_border_color: self.inactive_border_color,
            inactive_border_width: self.inactive_border_width,
            window_drop_shadow: self.window_drop_shadow,
            window_single_active: self.window_single_active,
            window_transparency_multiplier: self.window_transparency_multiplier,
            window_padding: self.window_padding,
            window_padding_px: self.window_padding_px.value,
            settings_size: self.settings_size,
            freeze: self.freeze,
            covermark_text: self.covermark_text.clone(),
            covermark_zoom: self.covermark_zoom,
            covermark_opacity: self.covermark_opacity,
            covermark_prefs: {
                // Stable order (by key) so the on-disk file doesn't churn between saves.
                let mut prefs: Vec<crate::state::CovermarkPref> = self
                    .covermark_prefs
                    .iter()
                    .map(|(key, &(zoom, opacity))| crate::state::CovermarkPref {
                        key: key.clone(),
                        zoom,
                        opacity,
                    })
                    .collect();
                prefs.sort_by(|a, b| a.key.cmp(&b.key));
                prefs
            },
            allow_multiple: self.allow_multiple,
            resident: self.resident,
            capture_hotkey: self.capture_hotkey.clone(),
            // Not cached on App (they're one-time lifecycle markers, not live
            // settings): carry whatever's on disk forward so a normal save never
            // clobbers them. Only init's first-run flow / the daemon's login-item
            // seed write them (directly).
            mac_first_run_seen: crate::state::load().mac_first_run_seen,
            mac_login_item_seeded: crate::state::load().mac_login_item_seeded,
            region_overlay_opacity: self.region_overlay_opacity,
            active_overlay_opacity: self.active_overlay_opacity,
            preview_overlay_opacity: self.preview_overlay_opacity,
            record_fps: self.record_fps.value,
            record_bitrate_kbps: self.record_bitrate_kbps.value,
            preferred_encoder: self.preferred_encoder(),
            scan_codes: self.scan_codes,
            scan_text: self.scan_text,
            text_confidence: self.text_confidence,
            record_dir: self.record_dir.clone(),
            record_hardware: true, // deprecated (removed toggle); kept for back-compat read
            screenshot_dir: self.screenshot_dir.clone(),
            copy_to_clipboard: self.copy_to_clipboard,
            clipboard_max_mb: self.clipboard_max_mb.value,
            record_mic: self.record_mic,
            hide_toolbar_fullscreen: self.hide_toolbar_fullscreen,
            push_to_talk: self.push_to_talk,
            record_system_audio: self.record_system_audio,
            record_backend: self.record_backend.clone(),
            screenshot_backend: self.screenshot_backend.clone(),
            // Deprecated mirrors (never serialized; the struct still carries them
            // so pre-v3 configs keep parsing).
            prefer_pipewire: self.record_backend == crate::platform::backend::PORTAL_ID,
            screenshot_pipewire: self.screenshot_backend == crate::platform::backend::PORTAL_ID,
            config_version: crate::state::CONFIG_VERSION,
            record_res_preset: self.record_res_preset,
            record_max_width: self.record_max_width.value,
            record_max_height: self.record_max_height.value,
            nvenc_preset: self.nvenc_preset.clone(),
            x264_preset: self.x264_preset.clone(),
            vaapi_compression_level: self.vaapi_compression_level,
            record_zero_copy: self.record_zero_copy,
            record_codec: self.record_codec.clone(),
            pw_restore_token: self.pw_restore_token.clone(),
            audio_sync_offset_ms: self.audio_sync_offset_ms.value,
            audio_sync_auto: self.audio_sync_auto,
            av_calibration_base_ms: self.av_calibration_base_ms,
            mic_device: self.mic_device.clone(),
            noise_reduction: self.noise_reduction,
            speaker_device: self.speaker_device.clone(),
            echo_cancellation: self.echo_cancellation,
            input_sensitivity_auto: self.input_sensitivity_auto,
            input_sensitivity: self.input_sensitivity,
            auto_gain: self.auto_gain,
            advanced_vad: self.advanced_vad,
            shortcuts: self.keymap.overrides(),
            preview_after_capture: self.preview_after_capture,
            preview_windowed: self.preview_windowed,
            auto_close_preview: self.auto_close_preview,
            preview_float_cosmic: self.preview_float_cosmic,
            mute_others_during_preview: self.mute_others_during_preview,
            duck_system_audio: self.duck_system_audio,
            appearance_use_system: self.appearance_use_system,
            appearance_mode: self.appearance_mode,
            appearance_accent: self.appearance_accent,
            appearance_roundness: self.appearance_roundness,
            appearance_contrast_boost: self.appearance_contrast_boost,
            selection_box_thickness: self.selection_box_thickness,
            notify_updates: self.notify_updates,
        }
    }

    /// Apply a `Persisted` to this instance's live settings fields (+ their text
    /// mirrors) and save it. Used by the reset-to-defaults actions.
    pub(super) fn apply_persisted(&mut self, p: crate::state::Persisted) {
        crate::state::save(&p);
        self.region = p.region.map(GlobalRect::from_tuple);
        self.delay_idx = p.delay_idx.min(DELAYS.len() - 1);
        self.capture_cursor = p.capture_cursor;
        self.capture_transparency = p.capture_transparency;
        self.capture_wallpaper = !p.no_wallpaper;
        self.active_border_color = p.active_border_color;
        self.active_border_width = p.active_border_width.min(10);
        self.inactive_border_color = p.inactive_border_color;
        self.inactive_border_width = p.inactive_border_width.min(10);
        self.window_drop_shadow = p.window_drop_shadow;
        self.window_single_active = p.window_single_active;
        self.window_transparency_multiplier = p.window_transparency_multiplier.clamp(0.0, 1.0);
        self.window_padding = p.window_padding;
        self.window_padding_px.set_value(p.window_padding_px.min(512));
        self.settings_size = p.settings_size;
        self.freeze = p.freeze;
        self.allow_multiple = p.allow_multiple;
        self.resident = p.resident;
        self.capture_hotkey = p.capture_hotkey.clone();
        self.region_overlay_opacity = p.region_overlay_opacity.clamp(0.0, 1.0);
        self.active_overlay_opacity = p.active_overlay_opacity.clamp(0.0, 1.0);
        self.preview_overlay_opacity = p.preview_overlay_opacity.clamp(0.0, 1.0);
        self.record_fps.set_value(p.record_fps.clamp(1, 240));
        self.record_bitrate_kbps.set_value(p.record_bitrate_kbps.clamp(100, 500_000));
        self.record_res_preset = p.record_res_preset.min(RES_CUSTOM as u8);
        self.record_max_width.set_value(p.record_max_width.clamp(2, 8192));
        self.record_max_height.set_value(p.record_max_height.clamp(2, 8192));
        self.nvenc_preset = p.nvenc_preset;
        self.x264_preset = p.x264_preset;
        self.vaapi_compression_level = p.vaapi_compression_level.clamp(-1, 6);
        self.record_zero_copy = p.record_zero_copy;
        self.record_codec = p.record_codec;
        self.audio_sync_offset_ms.set_value(p.audio_sync_offset_ms.clamp(-1000, 1000));
        self.audio_sync_auto = p.audio_sync_auto;
        self.av_calibration_base_ms = p.av_calibration_base_ms.clamp(-2000, 2000);
        self.mic_device = p.mic_device;
        self.noise_reduction = p.noise_reduction;
        self.speaker_device = p.speaker_device;
        self.echo_cancellation = p.echo_cancellation;
        self.input_sensitivity_auto = p.input_sensitivity_auto;
        self.input_sensitivity = p.input_sensitivity.clamp(0.0, 1.0);
        self.auto_gain = p.auto_gain;
        self.advanced_vad = p.advanced_vad;
        crate::audio::config::set_mic_source(&self.mic_device);
        self.record_dir = p.record_dir;
        self.scan_codes = p.scan_codes;
        self.scan_text = p.scan_text;
        self.text_confidence = p.text_confidence.clamp(0.0, 60.0);
        self.screenshot_dir = p.screenshot_dir;
        self.copy_to_clipboard = p.copy_to_clipboard;
        self.clipboard_max_mb.set_value(p.clipboard_max_mb.max(1));
        self.record_mic = p.record_mic;
        self.record_system_audio = p.record_system_audio;
        self.record_backend = p.record_backend;
        self.screenshot_backend = p.screenshot_backend;
        // preferred_encoder is the "auto" sentinel in defaults() — resolve it to the
        // best concrete encoder, like first launch does. Runs only on factory/page
        // reset (a settings-window action), so the encoder list is already resolved.
        if self.encoders().iter().any(|e| e.id == p.preferred_encoder) {
            self.set_preferred_encoder(p.preferred_encoder);
        } else if let Some(best) = self.encoders().first().map(|e| e.id.clone()) {
            self.set_preferred_encoder(best);
        }
        // Rebuild the live keymap from the persisted overrides (empty = all defaults).
        let mut keymap = crate::shortcuts::Keymap::defaults();
        keymap.apply_overrides(&p.shortcuts);
        self.keymap = keymap;
        self.preview_after_capture = p.preview_after_capture;
        self.preview_windowed = p.preview_windowed;
        self.auto_close_preview = p.auto_close_preview;
        self.preview_float_cosmic = p.preview_float_cosmic;
        self.mute_others_during_preview = p.mute_others_during_preview;
        self.duck_system_audio = p.duck_system_audio;
        self.appearance_use_system = p.appearance_use_system;
        self.appearance_mode = p.appearance_mode.min(2);
        self.appearance_accent = p.appearance_accent;
        self.appearance_roundness = p.appearance_roundness.min(2);
        self.appearance_contrast_boost = p.appearance_contrast_boost;
        self.selection_box_thickness = p.selection_box_thickness.clamp(1, 8);
        self.notify_updates = p.notify_updates;
    }

    /// Reset everything to defaults (factory reset). Also drops cached/saved state
    /// that isn't a "setting" — the saved region, the PipeWire restore token, and the
    /// last-scan regions — so nothing lingers after a reset.
    pub(super) fn factory_reset(&mut self) {
        self.apply_persisted(crate::state::defaults());
        self.region = None;
        self.pw_restore_token = None;
        self.last_code_region = None;
        self.last_ocr_region = None;
        // apply_persisted saved defaults already; re-save so the cleared token/region
        // can't be written back from this instance's stale in-memory copy.
        self.save_state();
    }

    /// Reset only the settings shown on `tab` to their defaults.
    pub(super) fn reset_page(&mut self, tab: ConfigTab) {
        let mut p = self.to_persisted();
        let d = crate::state::defaults();
        match tab {
            ConfigTab::General => {
                // The General page is split into in-page tabs (DRAGON-138), and the
                // "Reset to defaults" button sits under whichever tab is showing —
                // so it resets ONLY the visible tab's settings, not the whole page.
                match self.settings.active_general_tab() {
                    settings::GeneralTab::Settings => {
                        p.allow_multiple = d.allow_multiple;
                        p.resident = d.resident;
                        p.copy_to_clipboard = d.copy_to_clipboard;
                        p.clipboard_max_mb = d.clipboard_max_mb;
                        p.preview_after_capture = d.preview_after_capture;
                    }
                    settings::GeneralTab::Appearance => {
                        p.region_overlay_opacity = d.region_overlay_opacity;
                        p.active_overlay_opacity = d.active_overlay_opacity;
                        p.preview_overlay_opacity = d.preview_overlay_opacity;
                        p.appearance_use_system = d.appearance_use_system;
                        p.appearance_mode = d.appearance_mode;
                        p.appearance_accent = d.appearance_accent;
                        p.appearance_roundness = d.appearance_roundness;
                        p.appearance_contrast_boost = d.appearance_contrast_boost;
                        p.selection_box_thickness = d.selection_box_thickness;
                    }
                }
            }
            ConfigTab::CaptureModes => {
                // The Capture Modes page is split into in-page tabs (DRAGON-140), and
                // the "Reset to defaults" button sits under whichever tab is showing —
                // so it resets ONLY the visible tab's settings, not all three.
                match self.settings.active_capture_tab() {
                    settings::CaptureTab::Scanner => {
                        p.scan_codes = d.scan_codes;
                        p.scan_text = d.scan_text;
                        p.text_confidence = d.text_confidence;
                    }
                    settings::CaptureTab::Screenshots => {
                        p.capture_cursor = d.capture_cursor;
                        p.capture_transparency = d.capture_transparency;
                        p.no_wallpaper = d.no_wallpaper;
                        p.active_border_color = d.active_border_color;
                        p.active_border_width = d.active_border_width;
                        p.inactive_border_color = d.inactive_border_color;
                        p.inactive_border_width = d.inactive_border_width;
                        p.window_drop_shadow = d.window_drop_shadow;
                        p.window_single_active = d.window_single_active;
                        p.window_transparency_multiplier = d.window_transparency_multiplier;
                        p.window_padding = d.window_padding;
                        p.window_padding_px = d.window_padding_px;
                        p.freeze = d.freeze;
                        p.screenshot_dir = d.screenshot_dir.clone();
                        p.screenshot_backend = d.screenshot_backend.clone();
                    }
                    settings::CaptureTab::Recordings => {
                        p.record_mic = d.record_mic;
                        p.record_system_audio = d.record_system_audio;
                        p.record_dir = d.record_dir.clone();
                        p.record_backend = d.record_backend.clone();
                    }
                }
            }
            ConfigTab::AudioVideo => {
                // The Audio & Video page is split into in-page tabs (DRAGON-141), and the
                // "Reset to defaults" button sits under whichever tab is showing — so it
                // resets ONLY the visible tab's settings, not both.
                match self.settings.active_audio_video_tab() {
                    settings::AudioVideoTab::Audio => {
                        p.audio_sync_offset_ms = d.audio_sync_offset_ms;
                        p.audio_sync_auto = d.audio_sync_auto;
                        p.av_calibration_base_ms = d.av_calibration_base_ms;
                        p.mic_device = d.mic_device.clone();
                        p.noise_reduction = d.noise_reduction;
                        p.speaker_device = d.speaker_device.clone();
                        p.echo_cancellation = d.echo_cancellation;
                        p.input_sensitivity_auto = d.input_sensitivity_auto;
                        p.input_sensitivity = d.input_sensitivity;
                        p.auto_gain = d.auto_gain;
                        p.advanced_vad = d.advanced_vad;
                        p.mute_others_during_preview = d.mute_others_during_preview;
                        p.duck_system_audio = d.duck_system_audio;
                    }
                    settings::AudioVideoTab::Video => {
                        p.record_fps = d.record_fps;
                        p.record_bitrate_kbps = d.record_bitrate_kbps;
                        p.record_res_preset = d.record_res_preset;
                        p.record_max_width = d.record_max_width;
                        p.record_max_height = d.record_max_height;
                        p.preferred_encoder = d.preferred_encoder.clone();
                        p.nvenc_preset = d.nvenc_preset.clone();
                        p.x264_preset = d.x264_preset.clone();
                        p.vaapi_compression_level = d.vaapi_compression_level;
                        p.record_codec = d.record_codec.clone();
                        p.record_zero_copy = d.record_zero_copy;
                    }
                }
            }
            ConfigTab::Shortcuts => {
                // The Shortcuts page is split into in-page tabs (DRAGON-142), and the
                // "Reset to defaults" button sits under whichever tab is showing — so it
                // drops ONLY the visible tab's binding overrides; the other tabs' custom
                // bindings survive. Per-row resets are untouched by this.
                let tab = self.settings.active_shortcuts_tab();
                p.shortcuts
                    .retain(|(a, _)| settings::ShortcutsTab::for_group(a.group()) != tab);
                // The macOS global "Start Capture" hotkey row lives on the Capture tab.
                if tab == settings::ShortcutsTab::Capture {
                    p.capture_hotkey = d.capture_hotkey.clone();
                }
            }
            // About and Health are read-only (nothing to reset).
            ConfigTab::About | ConfigTab::Health => {}
        }
        self.apply_persisted(p);
    }
}
