//! `Persisted` struct and its `#[serde(default = "…")]` helper functions.

use serde::{Deserialize, Serialize};

/// One covermark option's remembered zoom + opacity (see `Persisted::covermark_prefs`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CovermarkPref {
    /// The option's stable key (see `CovermarkKind::pref_key`).
    pub key: String,
    /// Remembered zoom (0 = default cover fit).
    #[serde(default)]
    pub zoom: f32,
    /// Remembered opacity (0..1).
    #[serde(default = "default_covermark_opacity")]
    pub opacity: f32,
}

/// Container-level `#[serde(default)]` (belt-and-suspenders against settings RESETS):
/// `load()` parses the whole config all-or-nothing and silently falls back to `defaults()`
/// on ANY deserialize error, so one field missing from an older config would wipe every
/// setting. With this, a field ABSENT from the file falls back to its default instead of
/// failing the parse — even if a new field is ever added WITHOUT its own
/// `#[serde(default = "…")]`. Per-field defaults still win for the fields that have them.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Persisted {
    /// Region selection persisted as a bare `(left, top, right, bottom)` tuple — in
    /// TOML a 4-int array, in the legacy RON a tuple. Runtime code uses `GlobalRect`;
    /// the conversion happens in `app::persist` (`to_tuple`/`from_tuple`), so this
    /// on-disk shape never changes. `None` is stored by omitting the key (TOML can't
    /// express a literal None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<(i32, i32, i32, i32)>,
    #[serde(default)]
    pub delay_idx: usize,
    /// Whether captures include the mouse cursor (default on).
    #[serde(default = "default_true")]
    pub capture_cursor: bool,
    /// Whether window captures keep their own transparency (default on).
    #[serde(default = "default_true")]
    pub capture_transparency: bool,
    /// Whether to EXCLUDE the wallpaper (compose windows only). Stored inverted
    /// so the default (false) keeps the wallpaper in shots.
    #[serde(default)]
    pub no_wallpaper: bool,
    /// DEPRECATED (config v7, DRAGON-191): the old window-capture decoration style
    /// (0 = Active+shadow, 1 = Inactive+shadow, 2 = Inactive, 3 = Raw). Replaced by
    /// the explicit `active_*`/`inactive_*` border fields + `window_drop_shadow`.
    /// Read only for the one-time migration (see `store::migrate`) and never written
    /// again (`skip_serializing`).
    #[serde(default = "default_window_border_style", skip_serializing)]
    pub window_border_style: u8,
    /// Window-capture ACTIVE (focused) border colour, RGBA. `None` = follow the
    /// system accent colour (resolved at draw time); `Some` = a user-pinned custom
    /// colour. DRAGON-191. Omitted from disk when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_border_color: Option<[u8; 4]>,
    /// Window-capture ACTIVE border width (logical px, 0-10; 0 = no border). The
    /// focused window in a region/monitor composite, and every single-window capture,
    /// gets this. DRAGON-191. Default 3.
    #[serde(default = "default_active_border_width")]
    pub active_border_width: u32,
    /// Window-capture INACTIVE (unfocused) border colour, RGBA. DRAGON-191. Default
    /// `[65, 69, 80, 255]` (0xff414550).
    #[serde(default = "default_inactive_border_color")]
    pub inactive_border_color: [u8; 4],
    /// Window-capture INACTIVE border width (logical px, 0-10; 0 = no border).
    /// DRAGON-191. Default 1.
    #[serde(default = "default_inactive_border_width")]
    pub inactive_border_width: u32,
    /// Window FOCUS APPEARANCE for a SINGLE-window capture (DRAGON-191): `true` =
    /// portray it as Active (the Active border), `false` = Inactive (the Inactive
    /// border). Region/monitor composites ignore this and pick per-window by the real
    /// focus state. Default true (Active). Replaces the old `window_border_style`
    /// dropdown's active-vs-inactive choice.
    #[serde(default = "default_true")]
    pub window_single_active: bool,
    /// Draw the reconstructed drop shadow behind window captures. DRAGON-191. The
    /// native macOS shadow isn't capturable in an isolated grab, so it's
    /// reconstructed (see `compose::with_shadow`); on Linux it approximates cosmic's
    /// window shadow. Default true.
    #[serde(default = "default_true")]
    pub window_drop_shadow: bool,
    /// Extra transparency multiplier for window captures (0..1): the fraction maps to a
    /// 1x..500x multiplier on translucent pixels' transparency (default 0 = none).
    #[serde(default)]
    pub window_transparency_multiplier: f32,
    /// Whether to add a transparent margin around window captures (default on).
    #[serde(default = "default_true")]
    pub window_padding: bool,
    /// Margin width (logical px) added around window captures when `window_padding`.
    #[serde(default = "default_window_padding_px")]
    pub window_padding_px: u32,
    /// Last settings-window size (logical w, h), restored on reopen (clamped to the
    /// monitor). None until the window has been resized once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_size: Option<(u32, u32)>,
    /// Whether to freeze the screen (snapshot at launch) while selecting.
    /// Default OFF (selection happens over the live screen); it only takes effect on the
    /// native capture path anyway (the portal path returns a finished frame and drops the
    /// option). Existing users keep whatever they had saved; only fresh installs get off.
    #[serde(default)]
    pub freeze: bool,
    /// The text used by the "Custom text" covermark in the preview overlay.
    #[serde(default = "default_covermark_text")]
    pub covermark_text: String,
    /// The remembered covermark zoom (0 = default cover fit). Applied to a covermark
    /// when it's chosen in the preview overlay.
    #[serde(default)]
    pub covermark_zoom: f32,
    /// The remembered covermark opacity (0..1). Applied to a covermark when chosen.
    #[serde(default = "default_covermark_opacity")]
    pub covermark_opacity: f32,
    /// Per-option remembered zoom/opacity: each covermark option (Confidential, Custom
    /// text, each file) keeps its own last-used scale + opacity, restored when re-picked.
    /// Options not listed here fall back to `covermark_zoom` / `covermark_opacity`.
    #[serde(default)]
    pub covermark_prefs: Vec<CovermarkPref>,
    /// Preview editor appearance: `true` = a resizable window, `false` = the fullscreen
    /// overlay (default). Chosen under Settings → General → Capture Preview.
    #[serde(default)]
    pub preview_windowed: bool,
    /// Automatically close the preview editor after a Save / Save As / Copy (default
    /// on — the historical always-close behaviour, now optional).
    #[serde(default = "default_true")]
    pub auto_close_preview: bool,
    /// COSMIC only: register a tiling exception so the windowed preview floats instead
    /// of auto-tiling (default off). Only meaningful with `preview_windowed` on COSMIC.
    #[serde(default)]
    pub preview_float_cosmic: bool,
    /// Whether to permit more than one overlay instance at a time (default off:
    /// a second launch is suppressed by the single-instance lock).
    #[serde(default)]
    pub allow_multiple: bool,
    /// Keep the resident tray/menu-bar process running so a capture is always one click
    /// away. On macOS (DRAGON-130) that is the menu-bar daemon (the global capture hotkey
    /// is dead without it, so it defaults ON there, DRAGON-134); on Linux (DRAGON-173) it
    /// is the ksni tray resident, which defaults OFF (opt-in — the one-shot model still
    /// exits, and PrintScreen is a COSMIC custom shortcut, so nothing breaks without it).
    /// ONE portable setting; the settings toggle now shows on both OSes.
    #[serde(default = "default_resident")]
    pub resident: bool,
    /// macOS (DRAGON-134): whether the one-time launch-at-login seeding has run.
    /// The first BUNDLED daemon startup registers the SMAppService login item
    /// (launch-at-login defaults on, for the same hotkey reason as `resident`)
    /// and sets this; after that the OS and the settings toggle own the state,
    /// so an explicit opt-out is never overridden. Linux never reads it.
    #[serde(default)]
    pub mac_login_item_seeded: bool,
    /// macOS (DRAGON-130): the global "Start Capture" hotkey the resident daemon
    /// registers, as a spec string (e.g. "PrintScreen", "F13", "Cmd+Shift+2").
    /// Default "PrintScreen" (the user's Linux capture key); a bare "PrintScreen"
    /// also registers F13, since a PC keyboard's PrintScreen surfaces as F13 on
    /// macOS. Parsed by `daemon::hotkey_spec`; an unparseable value falls back to
    /// the default at registration. Linux ignores this (its capture key is a COSMIC
    /// custom shortcut, not owned here); the settings row is macOS-only.
    #[serde(default = "default_capture_hotkey")]
    pub capture_hotkey: String,
    /// macOS (DRAGON-130): whether the first-run Screen Recording permission prompt
    /// has already been fired. On the first ever capture launch, if the grant is
    /// absent, the app requests it once (the OS dialog) and sets this so it never
    /// re-prompts — the Health page is the recovery path thereafter. Default false;
    /// Linux never reads it (the field just rides along, always false).
    #[serde(default)]
    pub mac_first_run_seen: bool,
    /// Opacity (0..1) of the black dim drawn outside the region selection.
    #[serde(default = "default_region_opacity")]
    pub region_overlay_opacity: f32,
    /// Opacity (0..1) of the dim + selection lines while a capture is "active"
    /// (counting down, or — later — recording).
    #[serde(default = "default_active_opacity")]
    pub active_overlay_opacity: f32,
    /// Opacity (0..1) of the black dim behind the post-capture preview overlay.
    #[serde(default = "default_preview_opacity")]
    pub preview_overlay_opacity: f32,
    /// Video recording frame rate (fps).
    #[serde(default = "default_record_fps")]
    pub record_fps: u32,
    /// Video recording peak-bitrate cap (Kbps); quality-based RC sets the quality.
    /// Default 8000; lower it for smaller shareable demos.
    #[serde(default = "default_record_bitrate")]
    pub record_bitrate_kbps: u32,
    /// Config schema version, for migrating saved indices when a preset list
    /// changes shape. Old configs (no field) deserialize to 0; fresh installs
    /// are written at the current version. See `migrate`.
    #[serde(default)]
    pub config_version: u32,
    /// Max-resolution preset index (0 = Original/no limit, … last = Custom). The
    /// recording is downscaled to fit; default 1080p (good for sharing clips).
    #[serde(default = "default_record_res_preset")]
    pub record_res_preset: u8,
    /// Custom max width/height (used when the preset is Custom). Default 1080p box.
    #[serde(default = "default_custom_width")]
    pub record_max_width: u32,
    #[serde(default = "default_custom_height")]
    pub record_max_height: u32,
    /// Per-encoder speed/quality preset (each encoder has its own preset namespace).
    /// Defaults reproduce the original behaviour (NVENC `p4`, x264 `veryfast`). VAAPI
    /// has no reliable preset, so it isn't stored.
    #[serde(default = "default_nvenc_preset")]
    pub nvenc_preset: String,
    #[serde(default = "default_x264_preset")]
    pub x264_preset: String,
    /// VAAPI `-compression_level` (the real AMD/Intel speed/quality knob); `-1` =
    /// driver default (the default, unchanged behaviour).
    #[serde(default = "default_vaapi_cl")]
    pub vaapi_compression_level: i32,
    /// Experimental: capture PipeWire recordings as GPU DMA-BUF frames and encode
    /// them in-process (zero-copy), falling back to the CPU path when unavailable.
    /// Default off.
    #[serde(default)]
    pub record_zero_copy: bool,
    /// Video codec: `auto` (H.264 ≤ 4096 px, else HEVC), `h264` (max compatibility),
    /// or `hevc` (smaller files). Default `auto`. Software encoding is always H.264.
    #[serde(default = "default_record_codec")]
    pub record_codec: String,
    /// Preferred video encoder id ("nvenc" | "vaapi" | "software"). Only used when
    /// hardware encoding is enabled. Defaults to the "auto" sentinel, which the app
    /// replaces with the best available concrete encoder on first launch (and then
    /// persists), so there's no user-facing "auto" option.
    #[serde(default = "default_preferred_encoder")]
    pub preferred_encoder: String,
    /// Detect QR codes / barcodes inside the region in region mode (default on).
    #[serde(default = "default_true")]
    pub scan_codes: bool,
    /// OCR recognisable text inside the region in region mode (default on).
    #[serde(default = "default_true")]
    pub scan_text: bool,
    /// Minimum OCR word confidence (0–100) to keep; word-like tokens are rescued down
    /// to ~0.4× this. Default 25.
    #[serde(default = "default_text_confidence")]
    pub text_confidence: f32,
    /// Directory recordings are saved to (`~` is expanded). Default
    /// `~/Capture`.
    #[serde(default = "default_record_dir")]
    pub record_dir: String,
    /// Use a hardware video encoder (NVENC/VAAPI) when one is available, falling
    /// back to software x264. Default on.
    #[serde(default = "default_true")]
    pub record_hardware: bool,
    /// Directory screenshots are saved to (`~` expanded). Default
    /// `~/Capture`.
    #[serde(default = "default_screenshot_dir")]
    pub screenshot_dir: String,
    /// Copy the capture to the clipboard (when it's at or under the size limit).
    #[serde(default = "default_true")]
    pub copy_to_clipboard: bool,
    /// Max size (MB) to copy to the clipboard.
    #[serde(default = "default_clipboard_max_mb")]
    pub clipboard_max_mb: u32,
    /// Record microphone audio with videos (default off).
    #[serde(default)]
    pub record_mic: bool,
    /// Record system/desktop audio with videos (default off).
    #[serde(default)]
    pub record_system_audio: bool,
    /// Hide the floating recording toolbar on full-screen captures (DRAGON-174):
    /// when the toolbar can't fit OUTSIDE the recording area, `true` hides it instead
    /// of placing it in-frame (the tray icon still carries the controls), `false`
    /// keeps it in-frame. Default `false` (do not hide). Replaces the retired
    /// `recording_tray` systray-vs-toolbar choice; see the v5 migration in `store.rs`.
    #[serde(default)]
    pub hide_toolbar_fullscreen: bool,
    /// Push-to-talk: when on, an armed mic stays muted during a recording except while
    /// the push-to-talk hotkey is held. Default off (the mic is live the whole time).
    #[serde(default)]
    pub push_to_talk: bool,
    /// DEPRECATED (config v3, DRAGON-129): the pre-backend-id recording-method
    /// choice. Read only so old configs keep parsing — `migrate` maps it into
    /// `record_backend` once — and never written again.
    #[serde(default = "default_true", skip_serializing)]
    pub prefer_pipewire: bool,
    /// DEPRECATED (config v3, DRAGON-129): the pre-backend-id screenshot-method
    /// choice. Read only for the one-time migration into `screenshot_backend`.
    #[serde(default, skip_serializing)]
    pub screenshot_pipewire: bool,
    /// The capture backend recordings go through, as a stable
    /// `platform::backend` id ("screencopy" | "portal" | "sck"). Replaces
    /// `prefer_pipewire` (config v3): Linux defaults to the portal (first launch
    /// swaps in the probe result, exactly as the boolean did); macOS has only
    /// ScreenCaptureKit. A saved id that doesn't exist in the running session is
    /// clamped at use, never rewritten.
    #[serde(default = "default_record_backend")]
    pub record_backend: String,
    /// The capture backend screenshots go through (same id space). Replaces
    /// `screenshot_pipewire` (config v3); defaults to the platform's native
    /// backend (screencopy is instant + needs no permission prompt).
    #[serde(default = "default_screenshot_backend")]
    pub screenshot_backend: String,
    /// ScreenCast restore token from the last successful grant — replayed to skip
    /// the portal dialog next time (cleared on cancel / wrong-monitor so the user
    /// can re-pick). Single token; a stale cross-type one just makes the portal
    /// re-prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pw_restore_token: Option<String>,
    /// Audio→video sync offset (ms) for recordings: positive delays the audio (when
    /// the sound lands before the picture), negative advances it. Device-latency
    /// dependent; default 0. When `audio_sync_auto` is on, this is maintained
    /// automatically (measured from each recording's frame timestamps).
    #[serde(default = "default_audio_sync_offset")]
    pub audio_sync_offset_ms: i32,
    /// Auto-calibrate the A/V sync offset from each recording's measured latency
    /// (updates `audio_sync_offset_ms` after each recording). Default on.
    #[serde(default = "default_true")]
    pub audio_sync_auto: bool,
    /// End-to-end A/V calibration base (ms), measured once via `--calibrate-sync`
    /// (DRAGON-119): the part of the audio-leads-video offset the app cannot observe
    /// live (compositor frame-delivery lag). Auto-calibration adds it on top of each
    /// recording's measured median. Clamped to -2000..=2000 on load; 0 (the serde
    /// default) = uncalibrated, i.e. the historical behaviour.
    #[serde(default)]
    pub av_calibration_base_ms: i32,
    /// The microphone capture device: a PulseAudio source name on Linux, an
    /// avfoundation device NAME on macOS (DRAGON-132 — names survive replugs where
    /// avfoundation's enumeration index shifts). Empty = the system default source
    /// (auto), matching the prior hardcoded behaviour.
    #[serde(default)]
    pub mic_device: String,
    /// Apply real-time noise reduction (RNNoise via nnnoiseless) to the captured mic.
    /// Default on.
    #[serde(default = "default_true", alias = "voice_isolation")]
    pub noise_reduction: bool,
    /// PulseAudio sink name whose monitor is used as the echo-cancellation reference
    /// (the speaker output bleeding into the mic). Empty = the system default sink.
    #[serde(default)]
    pub speaker_device: String,
    /// Cancel speaker audio bleeding into the mic (WebRTC AEC3 via sonora), using the
    /// chosen speaker's monitor as the far-end reference. Default on.
    #[serde(default = "default_true")]
    pub echo_cancellation: bool,
    /// Input-sensitivity (voice gate) threshold mode: true = automatic (track the noise
    /// floor and gate just above it), false = manual via the slider. Default automatic.
    #[serde(default = "default_true")]
    pub input_sensitivity_auto: bool,
    /// Manual voice-gate threshold, 0..1 on the meter dBFS scale (`(dbfs+60)/60`).
    #[serde(default = "default_input_sensitivity")]
    pub input_sensitivity: f32,
    /// Automatic Gain Control (AGC2 via sonora): keep the mic clear and consistent.
    /// Default on.
    #[serde(default = "default_true")]
    pub auto_gain: bool,
    /// Advanced Voice Activity: use the earshot neural VAD for the gate's speech
    /// decision (vs the RNNoise probability). Default on.
    #[serde(default = "default_true")]
    pub advanced_vad: bool,
    /// Keyboard-shortcut overrides — only the bindings changed from default (a `None`
    /// records an explicit unbind). Empty = all defaults; the live `Keymap` is rebuilt
    /// from these on load.
    #[serde(default, with = "shortcut_overrides", skip_serializing_if = "Vec::is_empty")]
    pub shortcuts: Vec<(crate::shortcuts::Action, Option<crate::shortcuts::Shortcut>)>,
    /// Show the post-capture preview window (review + Save/Copy/Save As/Cancel) instead
    /// of immediately saving and copying. Default on.
    #[serde(default = "default_true")]
    pub preview_after_capture: bool,
    /// Pause other media players (via MPRIS) while a video preview with sound is playing,
    /// resuming them when it closes. Default on.
    #[serde(default = "default_true")]
    pub mute_others_during_preview: bool,
    /// Duck (lower) the recorded system audio while the mic hears speech, so voiceover
    /// stays clear over desktop sound (DRAGON-128). Applied at capture into the
    /// recorded track. Default on.
    #[serde(default = "default_true")]
    pub duck_system_audio: bool,
    /// Appearance (DRAGON-139): follow the system theme (accent / mode / rounding) as
    /// COSMIC provides it. When ON (default) the override fields below are IGNORED and
    /// the app follows `cosmic::theme::system_preference()`. When OFF, the overrides
    /// compose onto the resolved base and are applied live + on startup.
    #[serde(default = "default_true")]
    pub appearance_use_system: bool,
    /// Appearance override: base mode — 0 = automatic (resolve dark/light from the
    /// system at apply time), 1 = dark, 2 = light. Only consulted while
    /// `appearance_use_system` is OFF. Default 0 (automatic).
    #[serde(default)]
    pub appearance_mode: u8,
    /// Appearance override: accent colour as linear-free sRGB `[r, g, b]` (0..1).
    /// `None` keeps the base theme's own accent. Only consulted while
    /// `appearance_use_system` is OFF. Omitted from disk when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance_accent: Option<[f32; 3]>,
    /// Appearance override: corner-rounding style — 0 = round, 1 = slightly round,
    /// 2 = square. Only consulted while `appearance_use_system` is OFF. Default is
    /// PER-PLATFORM (DRAGON-256): Windows = 1 (slightly round), the native Windows
    /// look, so turning OFF System Default doesn't drop to fully-round; macOS/Linux
    /// stay at 0 (round), byte-identical to before.
    #[serde(default = "default_appearance_roundness")]
    pub appearance_roundness: u8,
    /// Appearance (DRAGON-289): "Automatic Contrast Boost" — adapt the selected accent
    /// for optimal contrast. When ON (default) EVERY accent element (fills, lines,
    /// outlines AND chrome text) uses the contrast-corrected accent (unchanged when the
    /// picked accent already passes a 4:1 contrast test); when OFF every element uses the
    /// EXACT picked colour (text forced to match the fills). Only consulted while
    /// `appearance_use_system` is OFF — System Default forces it ON. Additive field: an
    /// absent key defaults ON, so no config migration is needed.
    #[serde(default = "default_true")]
    pub appearance_contrast_boost: bool,
    /// Region selection box thickness (logical px, 1-8). Drives the viewfinder corner
    /// brackets AND the side lines uniformly so they match. Always applies (NOT gated by
    /// `appearance_use_system`). DRAGON-209. Default 4.
    #[serde(default = "default_selection_box_thickness")]
    pub selection_box_thickness: u32,
    /// About (DRAGON-177): show the launch-time "a new update is available" dialog
    /// when the settings-open update check resolves `Available`. Default ON. The
    /// About page exposes it as "Notify me when an update is available"; the launch
    /// dialog's "Don't remind me again" checkbox turns it OFF. A defaulted new field
    /// (absent key ⇒ `true`), so it needs no config migration.
    #[serde(default = "default_true")]
    pub notify_updates: bool,
}

fn default_input_sensitivity() -> f32 {
    0.5
}

fn default_screenshot_dir() -> String {
    "~/Capture".to_string()
}

fn default_clipboard_max_mb() -> u32 {
    1024
}

fn default_covermark_text() -> String {
    "CONFIGURE TEXT IN SETTINGS".to_string()
}

fn default_covermark_opacity() -> f32 {
    0.195
}

fn default_region_opacity() -> f32 {
    0.66
}

fn default_active_opacity() -> f32 {
    0.33
}

fn default_preview_opacity() -> f32 {
    0.9
}

/// The customize-mode corner-rounding default (used when `appearance_use_system` is
/// OFF). PER-PLATFORM (DRAGON-256): Windows defaults to 1 (slightly round) — the
/// native Windows look — so turning off System Default doesn't drop to fully-round;
/// macOS/Linux keep 0 (round), byte-identical to before. Both this serde default and
/// the reset target (`state::defaults()`, which deserializes an empty document) flow
/// through here, so a fresh install AND a per-setting reset land on the same value.
#[cfg(target_os = "windows")]
fn default_appearance_roundness() -> u8 {
    1
}
#[cfg(not(target_os = "windows"))]
fn default_appearance_roundness() -> u8 {
    0
}

fn default_record_fps() -> u32 {
    30
}

fn default_record_bitrate() -> u32 {
    // Peak-bitrate CAP — quality-based RC sets the actual quality, this just limits the
    // peak. 8000 kbps gives crisp shareable clips; busy content can still spend up to it.
    8000
}

fn default_record_res_preset() -> u8 {
    5 // 2K (2560x1440)
}

fn default_custom_width() -> u32 {
    1920
}

fn default_custom_height() -> u32 {
    1080
}

fn default_nvenc_preset() -> String {
    "p4".to_string() // middle preset (p1..p7)
}

fn default_x264_preset() -> String {
    "fast".to_string() // middle preset (ultrafast..veryslow)
}

fn default_vaapi_cl() -> i32 {
    3 // middle of the 0..6 compression-level scale
}

fn default_record_codec() -> String {
    "auto".to_string()
}

fn default_audio_sync_offset() -> i32 {
    0
}

fn default_text_confidence() -> f32 {
    25.0
}

fn default_preferred_encoder() -> String {
    "auto".to_string()
}

fn default_record_dir() -> String {
    "~/Capture".to_string()
}

fn default_true() -> bool {
    true
}

fn default_resident() -> bool {
    // The macOS global capture hotkey only works while the resident daemon runs,
    // so residency defaults on there; Linux stays one-shot.
    cfg!(target_os = "macos")
}

fn default_record_backend() -> String {
    // Mirrors the retired `prefer_pipewire` default (on): Linux prefers the portal
    // until the first-launch probe picks the real default; elsewhere the native
    // backend is the only one.
    if cfg!(target_os = "linux") {
        crate::platform::backend::PORTAL_ID.to_string()
    } else {
        crate::platform::backend::native_backend_id().to_string()
    }
}

fn default_screenshot_backend() -> String {
    crate::platform::backend::native_backend_id().to_string()
}

/// The default "Start Capture" global hotkey spec (the resident daemon's key). See
/// [`Persisted::capture_hotkey`]. Public so the daemon can fall back to it.
pub fn default_capture_hotkey() -> String {
    "PrintScreen".to_string()
}

fn default_window_padding_px() -> u32 {
    50
}

fn default_window_border_style() -> u8 {
    0 // Active: accent border + shadow + rounding (DEPRECATED — migration only)
}

/// Default ACTIVE window-capture border width (px). DRAGON-191.
fn default_active_border_width() -> u32 {
    3
}

/// Default INACTIVE window-capture border width (px). DRAGON-191.
fn default_inactive_border_width() -> u32 {
    1
}

/// Default region selection box thickness (px). DRAGON-209.
fn default_selection_box_thickness() -> u32 {
    4
}

/// Default INACTIVE window-capture border colour (0xff414550 = the user's prior
/// JankyBorders inactive grey). DRAGON-191.
fn default_inactive_border_color() -> [u8; 4] {
    [65, 69, 80, 255]
}

/// (De)serialization for the shortcut overrides that works in BOTH formats. TOML
/// cannot express `None` inside an array element, so the tuple shape the legacy RON
/// used (`(Action, Option<Shortcut>)`) is unwritable there. We WRITE an array of
/// tables (`{ action = …, shortcut = … }`, key omitted when unbound) and READ either
/// that or the legacy tuple, so old RON state files migrate losslessly.
mod shortcut_overrides {
    use crate::shortcuts::{Action, Shortcut};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    struct Entry {
        action: Action,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shortcut: Option<Shortcut>,
    }

    /// An action name parsed TOLERANTLY: a name that no longer exists in [`Action`]
    /// (one a later build removed — DRAGON-158 dropped `FocusSearch` and
    /// `PreviewNoAi`) comes back as `None` instead of a parse error. This matters
    /// because a single failing entry fails the whole `Persisted` parse, and
    /// `store::load_raw` answers a failed parse with DEFAULTS — a stale override
    /// must drop silently, never take every other setting down with it.
    struct CompatAction(Option<Action>);

    /// Map an action name to its variant, `None` when the name is unknown/removed.
    fn known_action(name: &str) -> Option<Action> {
        use serde::de::IntoDeserializer;
        let d: serde::de::value::StrDeserializer<serde::de::value::Error> =
            name.into_deserializer();
        Action::deserialize(d).ok()
    }

    /// A variant identifier read leniently as a plain string. Goes through
    /// `deserialize_identifier` (RON's bare identifiers only answer that call —
    /// asking for a String there errors `ExpectedString`), which TOML serves too.
    struct VariantName(String);

    impl<'de> serde::Deserialize<'de> for VariantName {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> serde::de::Visitor<'de> for V {
                type Value = VariantName;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("a variant name")
                }
                fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<VariantName, E> {
                    Ok(VariantName(s.to_owned()))
                }
            }
            d.deserialize_identifier(V)
        }
    }

    impl<'de> serde::Deserialize<'de> for CompatAction {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> serde::de::Visitor<'de> for V {
                type Value = CompatAction;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("an action name")
                }
                // TOML hands the entry-table string straight to the visitor.
                fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<CompatAction, E> {
                    Ok(CompatAction(known_action(s)))
                }
                // RON hands the legacy tuple's bare identifier through enum access.
                fn visit_enum<A: serde::de::EnumAccess<'de>>(
                    self,
                    data: A,
                ) -> Result<CompatAction, A::Error> {
                    use serde::de::VariantAccess;
                    let (name, variant) = data.variant::<VariantName>()?;
                    variant.unit_variant()?;
                    Ok(CompatAction(known_action(&name.0)))
                }
            }
            // The variants list is advisory (error text only) — neither TOML nor
            // RON validates against it, and the visitor accepts ANY name so removed
            // ones can degrade to `None` rather than erroring.
            d.deserialize_enum("Action", &["action"], V)
        }
    }

    /// One override, readable from BOTH shapes. serde's `untagged` can't do this
    /// over RON (its content-buffering loses RON's tuple/variant structure), so a
    /// manual visitor branches on the self-describing form instead: a map is the
    /// new `{action, shortcut}` entry, a seq is the legacy `(Action, Option)` tuple.
    /// A `None` action is an override for a REMOVED action; the outer deserialize
    /// drops it.
    struct Compat(Option<Action>, Option<Shortcut>);

    impl<'de> serde::Deserialize<'de> for Compat {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> serde::de::Visitor<'de> for V {
                type Value = Compat;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("a shortcut override (entry table or legacy tuple)")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> Result<Compat, A::Error> {
                    let (mut action, mut shortcut): (Option<CompatAction>, _) = (None, None);
                    while let Some(key) = map.next_key::<String>()? {
                        match key.as_str() {
                            "action" => action = Some(map.next_value::<CompatAction>()?),
                            "shortcut" => shortcut = map.next_value::<Option<Shortcut>>()?,
                            _ => {
                                map.next_value::<serde::de::IgnoredAny>()?;
                            }
                        }
                    }
                    let action = action
                        .ok_or_else(|| serde::de::Error::missing_field("action"))?;
                    Ok(Compat(action.0, shortcut))
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut seq: A,
                ) -> Result<Compat, A::Error> {
                    let action: CompatAction = seq
                        .next_element()?
                        .ok_or_else(|| serde::de::Error::invalid_length(0, &self))?;
                    let shortcut: Option<Shortcut> = seq.next_element()?.unwrap_or(None);
                    Ok(Compat(action.0, shortcut))
                }
            }
            d.deserialize_any(V)
        }
    }

    pub fn serialize<S: Serializer>(
        v: &[(Action, Option<Shortcut>)],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let entries: Vec<Entry> = v
            .iter()
            .map(|(action, shortcut)| Entry { action: *action, shortcut: shortcut.clone() })
            .collect();
        entries.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<(Action, Option<Shortcut>)>, D::Error> {
        Ok(Vec::<Compat>::deserialize(d)?
            .into_iter()
            .filter_map(|Compat(action, shortcut)| Some((action?, shortcut)))
            .collect())
    }
}
