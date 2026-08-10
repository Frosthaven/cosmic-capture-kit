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
    /// MASTER switch for the single-window screenshot recompositing ("Enable
    /// single window aesthetic effects"): the aesthetic decorations composited
    /// onto a captured window (borders, drop shadow, rounding, padding, the
    /// wallpaper backdrop). OFF delivers the BARE captured frame on every platform
    /// and path (native and portal) while PRESERVING every individual aesthetic
    /// preference below, so re-enabling restores them exactly. Default on.
    #[serde(default = "default_true")]
    pub window_recompositing: bool,
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
    /// The last-selected annotation stroke color (RGBA), so a fresh preview opens with it.
    /// `None` = fall back to the accent complement (the default). DRAGON-321.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annot_color: Option<[u8; 4]>,
    /// The last-selected annotation tool ("arrow" | "box"). `None` / unknown = neutral
    /// (no draw tool). DRAGON-321.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annot_tool: Option<String>,
    /// The last-selected annotation stroke width in logical POINTS (DRAGON-383), so a fresh
    /// preview opens with it and new box/arrow shapes seed from it (scaled to the document's
    /// SOURCE px at the seed site). Defaults to `DEFAULT_ANNOT_STROKE`.
    ///
    /// MIGRATION: an older build persisted this as raw SOURCE px. On Linux (and any 1x panel)
    /// points == source px, so an existing value is unchanged. On a 2x mac the stored NUMBER is
    /// reused verbatim but now MEANS points — the points interpretation wins because the value
    /// only ever came from picking a (point) preset off the ladder in the first place, so
    /// reinterpreting it restores the intended visual size; it also self-corrects the instant a
    /// preset is picked. Acceptable because the pre-fix mac behavior was itself the bug.
    #[serde(default = "default_annot_stroke_w")]
    pub annot_stroke_w: f32,
    /// The remembered SEQUENCE-BADGE ("step marker") side in logical POINTS (DRAGON-383):
    /// whatever the last badge placed or resized in ANY preview editor settled at (brought back
    /// to points from its source-px side), so the next editor — a new capture process, a later
    /// launch, a DIFFERENT-scale display — spawns markers at the same visual size. `0.0` (and an
    /// absent key, which reads the same) means UNSET: the caller falls back to
    /// `app::preview::annotate::DEFAULT_BADGE_SIZE`, which is why this has no `default = …` fn
    /// of its own. MIGRATION as for `annot_stroke_w`: a pre-fix value was source px (identity on
    /// an unscaled output; reinterpreted as points on a scaled one, self-correcting on the next
    /// placement).
    #[serde(default)]
    pub annot_badge_size: f32,
    /// The last-selected TEXT annotation size in logical POINTS (DRAGON-383), so a fresh preview
    /// opens the text tool at it (scaled to SOURCE px when it seeds a box). `0.0` / absent =
    /// UNSET (falls back to `preview::text_annot::DEFAULT_TEXT_SIZE`), so no `default = …` fn.
    /// DRAGON-354. MIGRATION as for `annot_stroke_w` (identity on an unscaled output;
    /// reinterpreted as points on a scaled one, self-correcting on the next dropdown pick).
    #[serde(default)]
    pub annot_text_size: f32,
    /// The last-selected TEXT font family ("hand" | "clean"). `None` / unknown = the default
    /// (handwritten). DRAGON-354.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annot_text_font: Option<String>,
    /// The last 5 CUSTOM annotation colors chosen (most-recent-first, RGBA), shown as MRU
    /// swatches in the color flyout. DRAGON-321.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annot_recent_colors: Vec<[u8; 4]>,
    /// Preview editor appearance: `true` = a resizable window (default), `false` = the
    /// fullscreen overlay. Chosen under Settings → General → Capture Preview. Existing
    /// users keep whatever they saved; only a fresh install picks up the windowed default.
    #[serde(default = "default_true")]
    pub preview_windowed: bool,
    /// Preview editor: draw the small CAPTION under each group of the top toolbar
    /// ("Canvas", "Shape", "Redact", …). Default **on**. Settings → Preview Editor →
    /// General → "Show toolbar group labels".
    ///
    /// It is a LAYOUT setting, not only a cosmetic one: the caption band makes the top bar
    /// taller, so `PreviewSurface::chrome_h` reads this and answers differently for on and
    /// off. Off returns the editor's geometry to exactly what it was before DRAGON-478 —
    /// the open fit, the fit/zoom math and the overlay column all follow from that one
    /// number, so there is nothing else to switch.
    #[serde(default = "default_true")]
    pub preview_toolbar_labels: bool,
    /// DRAGON-419: write the opt-in debug log. Default **OFF** — a support switch, not a
    /// feature. Settings → Health → Debug.
    ///
    /// Read TWICE by design, from two different places: `crate::diag::init` reads THIS key
    /// straight out of the TOML as the first statement of `main` (before any subcommand or
    /// the daemon branch, so a launch that never reaches `App::init` is still covered), and
    /// the App mirrors it like every other setting so the toggle can take effect immediately.
    /// Keep the name in step with the one-field struct in `diag::persisted_setting`,
    /// INCLUDING the default: both must say `default_true` or a capture child launched
    /// before settings ever saved would disagree with the settings window about whether
    /// logging is on.
    ///
    /// Default **ON** since v0.28.0 (owner decision): the log records what the app DID and
    /// never what was captured (the diag privacy rules), it is bounded under ~5 MiB total by
    /// rotation, and a support conversation that starts with "turn on logging and reproduce
    /// it" loses the one run that mattered. The Health page toggle turns it off.
    #[serde(default = "default_true")]
    pub debug_logging: bool,
    // DRAGON-467 removed SIX fields from here, plus the deprecated seventh that fed two of
    // them: `preview_save_on_copy`, `preview_close_on_copy`, `preview_copy_on_delete`, their
    // three `preview_video_*` twins, and `preview_save_close_on_copy` (the pre-v8 combined
    // toggle the v8 hook read). All three rules described ACTIONS that no longer work that
    // way: a Copy no longer saves or closes anything (it copies the current state, full
    // stop), and a Delete no longer needs a courtesy copy because the capture went on the
    // clipboard the moment it was taken. The three rows below replace them per media kind.
    // Removed rather than kept as deprecated read-only fields: there is nothing to translate
    // into (see `store::migrate`'s v9 note, and `old_share_automation_keys_are_ignored_on_load`
    // for the proof that an old config still loads with every other setting intact).
    /// Preview editor: put the EDITED result on the clipboard when the editor closes.
    /// Default on. Settings → Preview Editor → Image Editor.
    ///
    /// The capture itself already went on the clipboard when it was taken (DRAGON-467 made
    /// that unconditional), so this is specifically about the EDITS: annotations, a crop, a
    /// covermark. Without it the clipboard still holds the untouched grab after an edit
    /// session, which is almost never what the user meant.
    ///
    /// Nothing is copied when the scene is clean — there would be no difference to carry.
    #[serde(default = "default_true")]
    pub preview_copy_on_exit: bool,
    /// Write every screenshot into `screenshot_dir` as it is captured. Default on.
    /// Settings → Preview Editor → Image Editor.
    ///
    /// OFF routes the capture through the session runtime directory instead
    /// (`util::runtime_dir`), so nothing lands in the user's folder until they choose Save.
    /// The editor still opens on it, the clipboard still gets it, and a Save still offers
    /// `screenshot_dir` as its destination — the only difference is that an untouched
    /// capture leaves no file behind.
    #[serde(default = "default_true")]
    pub preview_save_originals: bool,
    /// Ask before closing the image editor with edits that were never saved. Default on.
    /// Settings → Preview Editor → Image Editor.
    ///
    /// This is the gate on the unsaved-changes card (`close_needs_confirmation`). OFF closes
    /// straight away and the un-baked edits go with it — which is a reasonable choice
    /// alongside `preview_copy_on_exit`, since the edited result still reaches the clipboard.
    #[serde(default = "default_true")]
    pub preview_ask_to_save: bool,
    // The three above are the IMAGE editor's; the three below are the VIDEO editor's own
    // copies of the same rules (the DRAGON-420 split, carried forward). Separate FIELDS
    // rather than one shared set because that separation is the whole feature: a recording is
    // expensive to re-encode and slow to re-open, so a user may well want different answers
    // per media kind.
    /// Preview editor, VIDEO documents: put the edited recording on the clipboard when the
    /// editor closes. Default on. Settings → Preview Editor → Video Editor.
    #[serde(default = "default_true")]
    pub preview_video_copy_on_exit: bool,
    /// Write every recording into `record_dir` as it is captured. Default on.
    /// Settings → Preview Editor → Video Editor. OFF routes it through the session runtime
    /// directory instead, exactly as `preview_save_originals` does for stills.
    #[serde(default = "default_true")]
    pub preview_video_save_originals: bool,
    /// Ask before closing the video editor with cuts that were never saved. Default on.
    /// Settings → Preview Editor → Video Editor.
    #[serde(default = "default_true")]
    pub preview_video_ask_to_save: bool,
    // DRAGON-353: `auto_close_preview` ("Automatically close the preview editor on save or
    // copy") lived here. NO share action closes the editor any more — Save / Save As / Copy
    // all keep the document open — so the setting had no behaviour left to name and a
    // setting that does nothing is dishonest UI. REMOVED (not kept as a deprecated
    // read-only field: there is nothing to migrate into). Same treatment as `allow_multiple`
    // below; pinned by `store::tests::old_preview_workflow_keys_are_ignored_on_load`.
    // DRAGON-351: `allow_multiple` ("Allow multiple capture instances") lived here. Every
    // launch now runs as its own instance unconditionally, so the field is REMOVED rather
    // than kept as a deprecated read-only one — there is nothing to migrate into. Old
    // configs still carry the key; serde ignores unknown fields (no `deny_unknown_fields`
    // anywhere on this struct), so they keep loading with every other setting intact —
    // pinned by `store::tests::old_allow_multiple_key_is_ignored_on_load`, the same
    // treatment `recording_tray` (v5) and `preview_float_cosmic` (DRAGON-317) got. No
    // `config_version` bump: nothing about the remaining fields' meaning changed.
    /// Keep the resident tray/menu-bar process running so a capture is always one click
    /// away. On macOS (DRAGON-130) that is the menu-bar daemon (the global capture hotkey
    /// is dead without it, so it defaults ON there, DRAGON-134); on Windows (DRAGON-296) it
    /// is the Win32 tray daemon that owns the same global capture hotkey, so it ALSO defaults
    /// ON there now, matching macOS; on Linux (DRAGON-173) it is the ksni tray resident, which
    /// defaults OFF (opt-in — the one-shot model still exits, and PrintScreen is a COSMIC
    /// custom shortcut, so nothing breaks without it). ONE portable setting; the settings
    /// toggle now shows on all OSes.
    #[serde(default = "default_resident")]
    pub resident: bool,
    /// Launch the resident at login (DRAGON-296): register the OS login item / autostart
    /// entry so the tray resident comes back after a reboot/login. GATED by `resident` — the
    /// settings row is hidden when the tray is off, and the actual login item is registered
    /// only when BOTH this and `resident` are on (see `App::reconcile_login_item`). Default
    /// ON. An additive serde-defaulted field (absent key ⇒ `true`), so old configs load
    /// without a store-version bump.
    #[serde(default = "default_true")]
    pub autostart_on_login: bool,
    /// macOS (DRAGON-134): whether the one-time launch-at-login seeding has run.
    /// The first BUNDLED daemon startup registers the SMAppService login item
    /// (launch-at-login defaults on, for the same hotkey reason as `resident`)
    /// and sets this; after that the OS and the settings toggle own the state,
    /// so an explicit opt-out is never overridden. Linux never reads it.
    #[serde(default)]
    pub mac_login_item_seeded: bool,
    /// Windows (DRAGON-296): the Windows analog of `mac_login_item_seeded`. The first tray-daemon
    /// startup registers the HKCU `Run` login item (launch-at-login defaults on, since the tray's
    /// global capture hotkey needs the daemon — the same reason `resident` defaults on) and sets
    /// this; after that the registry entry and the settings toggle own the state, so an explicit
    /// opt-out is never re-seeded. An additive serde-defaulted field (absent key ⇒ `false`), so
    /// old configs load without a store-version bump. macOS/Linux never read it.
    #[serde(default)]
    pub win_login_item_seeded: bool,
    /// macOS/Windows (DRAGON-130 / DRAGON-295): the global "Capture All In One" hotkey
    /// the resident daemon registers, as a spec string (e.g. "PrintScreen", "F13",
    /// "Cmd+Shift+2"). Opens the full capture overlay (region/window/monitor picker).
    /// Default UNSET (empty) since DRAGON-295 — a fresh install ships with NO global
    /// capture hotkey, and the three capture actions are opt-in. A bare "PrintScreen"
    /// also registers F13, since a PC keyboard's PrintScreen surfaces as F13 on macOS.
    /// Parsed by `daemon::hotkey_spec`; an unparseable value falls back to the default
    /// at registration. EXISTING users keep whatever value their config already holds
    /// (the field is always serialized, so an upgrade preserves the old "PrintScreen").
    /// Linux ignores this (its capture key is a COSMIC custom shortcut, not owned here);
    /// the settings row is macOS/Windows-only.
    #[serde(default = "default_capture_hotkey")]
    pub capture_hotkey: String,
    /// macOS/Windows (DRAGON-295): the global "Capture Active Window" hotkey the resident
    /// daemon registers, as a spec string (same vocabulary as `capture_hotkey`).
    /// Immediately captures the frontmost/active window with NO picker overlay. Default
    /// UNSET (empty). A serde-defaulted additive field, so old configs load with it
    /// absent (no store-version bump). Linux ignores it; the settings row is macOS/
    /// Windows-only.
    #[serde(default = "default_capture_active_window_hotkey")]
    pub capture_active_window_hotkey: String,
    /// macOS/Windows (DRAGON-295): the global "Capture Active Monitor" hotkey the resident
    /// daemon registers, as a spec string (same vocabulary as `capture_hotkey`).
    /// Immediately captures the monitor under the cursor with NO picker overlay. Default
    /// UNSET (empty). A serde-defaulted additive field, so old configs load with it
    /// absent (no store-version bump). Linux ignores it; the settings row is macOS/
    /// Windows-only.
    #[serde(default = "default_capture_active_monitor_hotkey")]
    pub capture_active_monitor_hotkey: String,
    /// macOS/Windows (DRAGON-428): the "no editor" twin of [`Self::capture_hotkey`] — the
    /// same All In One capture picker, but the finished capture is saved, copied and
    /// notified WITHOUT opening the preview editor (the daemon adds `--no-editor`).
    /// Default UNSET (empty), like every capture hotkey. A serde-defaulted additive field,
    /// so old configs load with it absent (no store-version bump). Linux ignores it; the
    /// settings row is macOS/Windows-only.
    #[serde(default = "default_capture_hotkey")]
    pub capture_no_editor_hotkey: String,
    /// macOS/Windows (DRAGON-428): the "no editor" twin of
    /// [`Self::capture_active_window_hotkey`]. Same picker-free frontmost-window capture,
    /// delivered without the editor. Default UNSET (empty).
    #[serde(default = "default_capture_hotkey")]
    pub capture_active_window_no_editor_hotkey: String,
    /// macOS/Windows (DRAGON-428): the "no editor" twin of
    /// [`Self::capture_active_monitor_hotkey`]. Same picker-free monitor capture, delivered
    /// without the editor. Default UNSET (empty).
    #[serde(default = "default_capture_hotkey")]
    pub capture_active_monitor_no_editor_hotkey: String,
    /// macOS/Windows (DRAGON-582): the resident daemon's global hotkey for the COLOUR
    /// PICKER tool. Default UNSET (empty), like every other global hotkey here. A
    /// serde-defaulted additive field, so old configs load with it absent (no
    /// store-version bump). Linux has no daemon-owned global hotkey and ignores it; a
    /// Linux user binds a COSMIC custom shortcut to `--color-picker` instead (README).
    #[serde(default = "default_capture_hotkey")]
    pub color_picker_hotkey: String,
    /// macOS (DRAGON-130): whether the first-run Screen Recording permission prompt
    /// has already been fired. On the first ever capture launch, if the grant is
    /// absent, the app requests it once (the OS dialog) and sets this so it never
    /// re-prompts — the Health page is the recovery path thereafter. Default false;
    /// Linux never reads it (the field just rides along, always false).
    #[serde(default)]
    pub mac_first_run_seen: bool,
    /// macOS (DRAGON-311): whether the Accessibility permission prompt has already
    /// been fired from the permissions helper. Accessibility is OPTIONAL (capture is
    /// never blocked without it), but its preflight is boolean like Screen Recording's,
    /// so once the prompt is spent a missing grant reads as Denied (Open Settings) and
    /// the card stops offering Request. A serde-defaulted additive bool, so old configs
    /// load with it absent (no store-version bump). Default false; Linux never reads it.
    #[serde(default)]
    pub mac_accessibility_prompt_seen: bool,
    /// macOS (DRAGON-412): whether the permission window's RECOMMENDED / OPTIONAL
    /// nag is spent — the user has been shown the window once and left it without
    /// addressing those grants (Skip, or simply closing it). Set on dismissal, which
    /// is the whole point: before this existed, a never-prompted Accessibility /
    /// microphone / notification grant re-forced the window on EVERY capture launch
    /// and every daemon start, forever, because closing the window was not a decision
    /// the code recognised. It is deliberately SEPARATE from
    /// `mac_accessibility_prompt_seen`: that flag means "the OS prompt itself is
    /// spent", which is what decides Request-vs-Open-Settings on the card, and
    /// claiming it on a dismissal would render a permission the user never denied as
    /// Denied. Required grants (Screen Recording) ignore this flag entirely and keep
    /// forcing the window while missing. A serde-defaulted additive bool, so old
    /// configs load with it absent (no store-version bump). Linux never reads it.
    #[serde(default)]
    pub mac_permission_nag_spent: bool,
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
    /// DRAGON-582: opacity (0..1) of the black dim behind the COLOUR PICKER overlay.
    ///
    /// Its own setting rather than sharing the region one: the picker dims the screen to
    /// make a magnifier readable, not to mark a selection. Default 33%, the same figure as
    /// [`default_active_opacity`], so the two overlays that sit over a working desktop feel
    /// alike.
    ///
    /// It went to ZERO briefly during DRAGON-588, on the reasoning that you are judging
    /// COLOURS and any dim changes what every pixel looks like. The owner took it back to
    /// 33% because a picker with no dim is indistinguishable from an ordinary desktop, and
    /// on the portal-fallback path that is a fullscreen toplevel sitting invisibly over
    /// everything. The dim is what says the tool is armed. Do not re-argue this without a
    /// different reason: the colour REPORTED is unaffected either way, since the sample
    /// comes from the frozen snapshot rather than the composited screen (see
    /// `app::color_picker`'s `PixelSource`), so this is purely about legibility.
    #[serde(default = "default_color_picker_opacity")]
    pub color_picker_overlay_opacity: f32,
    /// DRAGON-615: the magnifier's magnification, in rendered pixels per source pixel.
    ///
    /// Remembered between sessions because the picker is a ONE-SHOT process: without this a
    /// user who prefers a tighter or wider lens re-sets it on every single launch.
    ///
    /// Never trusted as read. `color_picker::geom::zoom_from_persisted` clamps it into the
    /// bounds THIS build allows, because the ceiling has already moved once (12 to 26,
    /// DRAGON-601) and a config written either side of such a change must not be able to
    /// produce an out-of-range lens. A missing key reads as the default, so anyone who has
    /// never touched the zoom is unaffected.
    ///
    /// Deliberately absent from the Settings window: it is a per-use view preference the
    /// picker already exposes directly on three routes (trackpad, wheel, numpad `+`/`-`), not
    /// a configuration choice, so a settings row would be a second way to say the same thing.
    #[serde(default = "default_color_picker_zoom")]
    pub color_picker_zoom: u32,
    /// DRAGON-582: recently PICKED colours, newest first, as `#RRGGBB` strings.
    ///
    /// Persisted because the app is one-shot: the picker process exits when its window
    /// closes, so an in-memory list would be empty every single time the window opened
    /// and the feature would do nothing. Capped at `color_picker::geom::RECENTS_CAP` when
    /// written; an over-long or malformed list from a hand-edited config is truncated and
    /// filtered on load rather than rejected.
    ///
    /// This is user CONTENT. It is written to the config like any other setting and NEVER
    /// to the debug log (see `app::color_picker`'s privacy note).
    #[serde(default)]
    pub recent_colors: Vec<String>,
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
    /// Preferred video encoder id: the "auto" sentinel (the default) or a concrete id
    /// ("nvenc" | "vaapi" | "software" on Linux, plus "videotoolbox" / "amf" / "qsv"
    /// off Linux). "auto" STAYS persisted until the user explicitly picks in the
    /// settings encoder dropdown; an auto resolution is NEVER written back here
    /// (DRAGON-571: it used to be, so one transient first-launch probe failure, a
    /// sleeping driver or an NVENC session held by a game, pinned "software" forever
    /// with nothing ever reconsidering). Auto resolves per recording through the
    /// hint-first ladder seeded by [`encoder_auto_hint`](Self::encoder_auto_hint).
    /// The legacy `record_hardware = false` toggle still maps this to software at
    /// read time.
    #[serde(default = "default_preferred_encoder")]
    pub preferred_encoder: String,
    /// Last-known-good AUTO encoder resolution (DRAGON-571): a CACHE, not a choice.
    /// The auto ladder probes this id first (one probe-encode on the happy path, the
    /// same single probe a concrete pick pays) and any recording session may overwrite
    /// it with its winner (`state::note_encoder_auto_hint`). Empty = no resolution
    /// recorded yet. Kept separate from `preferred_encoder` so the cache can be
    /// written freely while the preference stays the user's alone; the settings
    /// snapshot carries this field from DISK (see `app/persist.rs`), so a settings
    /// save never reverts a worker's fresh note.
    #[serde(default)]
    pub encoder_auto_hint: String,
    /// Detect QR codes / barcodes inside the region in region mode (default on).
    #[serde(default = "default_true")]
    pub scan_codes: bool,
    /// OCR recognisable text inside the region in region mode (default on).
    #[serde(default = "default_true")]
    pub scan_text: bool,
    /// Minimum OCR word confidence (0–100) to keep; word-like tokens are rescued down
    /// to ~0.4× this. Default 20.
    #[serde(default = "default_text_confidence")]
    pub text_confidence: f32,
    /// Which tesseract language pack OCR runs with, as its short code (`eng`, `deu`, …),
    /// or several joined by `+` (`eng+deu`), which is tesseract's own `-l` syntax.
    ///
    /// **Empty means "don't pass `-l` at all"**, leaving tesseract on its built-in `eng`
    /// default. That is deliberately the default here: every build before DRAGON-527 passed
    /// no language, so an existing config that has never seen this field behaves exactly as
    /// it always did rather than silently changing what OCR recognises.
    #[serde(default)]
    pub ocr_language: String,
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
    // DRAGON-353: `copy_to_clipboard` ("Automatically copy to clipboard") and
    // `clipboard_max_mb` ("Clipboard size limit") both lived here. The preview editor now
    // copies the capture the moment it opens (and the immediate, preview-less share path
    // copies too), so the behaviour is unconditional and the toggle is REMOVED; the size
    // limit is REMOVED as a setting too and lives on as the fixed
    // `share::AUTO_COPY_MAX_MB` — the editor toasts a named error when it declines a copy
    // for size, so the knob existed only to pre-empt a failure the user can now read. See
    // that constant's doc for why the limit itself survives.
    /// Record microphone audio with videos (owner's call: default ON, mic + system is the
    /// expected default recording mode). `default_true` only fires when the key is absent
    /// from the file at all, i.e. a genuinely fresh install: `save` always writes the WHOLE
    /// struct, so anyone who has ever saved settings already has their own explicit `true`
    /// or `false` on disk, which always wins over this default.
    #[serde(default = "default_true")]
    pub record_mic: bool,
    /// Record system/desktop audio with videos (owner's call: default ON, same reasoning as
    /// `record_mic`).
    #[serde(default = "default_true")]
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
    /// `prefer_pipewire` (config v3): Linux defaults to the portal; macOS has
    /// only ScreenCaptureKit. This field is USER INTENT (DRAGON-575, the
    /// DRAGON-571 rule): only a real settings pick writes it. A saved id that
    /// doesn't exist in the running session is clamped at use and shown resolved
    /// in the method dropdown, never rewritten. (A first-launch probe used to
    /// write its auto-pick here once; see the tombstone at
    /// `CaptureMsg::PipewireProbed`.)
    #[serde(default = "default_record_backend")]
    pub record_backend: String,
    /// The capture backend screenshots go through (same id space). Replaces
    /// `screenshot_pipewire` (config v3); defaults to the platform's native
    /// backend (screencopy is instant + needs no permission prompt). User intent
    /// like `record_backend`: on a session without the native protocols (GNOME/
    /// KDE, the Flatpak sandbox) the default id is clamped to the portal at use
    /// and shown resolved, never rewritten.
    #[serde(default = "default_screenshot_backend")]
    pub screenshot_backend: String,
    /// DEPRECATED (config v11, DRAGON-570): the single-token era's ScreenCast
    /// restore token. Read only for the one-time migration into the per-source
    /// slot named by `pw_restore_source` (see `store::migrate`) and never written
    /// again (`skip_serializing`). The single slot was the bug's second half: a
    /// window grant overwrote the monitor token and vice versa, so alternating
    /// capture kinds re-prompted even when replay worked.
    #[serde(default, skip_serializing)]
    pub pw_restore_token: Option<String>,
    /// DEPRECATED (config v11, DRAGON-570): which source type the legacy
    /// `pw_restore_token` was granted for ("monitor" / "window"), the DRAGON-544
    /// cross-type replay guard. Read only so the migration can route the legacy
    /// token into its matching slot; a `None` or unknown source drops the token
    /// (it never replayed under DRAGON-544's gate either). Never written again.
    #[serde(default, skip_serializing)]
    pub pw_restore_source: Option<String>,
    /// ScreenCast restore token from the last successful MONITOR grant, replayed
    /// to skip the portal dialog for the next monitor request (region and monitor
    /// captures). One slot PER SOURCE TYPE since DRAGON-570, so a window grant no
    /// longer discards the monitor token. Every use reissues the token (the spent
    /// one is invalid), so this always holds the latest reissue. Cleared on a
    /// declined monitor request, the wrong-monitor region bounce, the settings
    /// Forget row, and factory reset. Persisted grants survive our one-shot
    /// processes because the portal request uses persist mode 2
    /// (`ExplicitlyRevoked`, the on-disk permission store); see
    /// `platform/linux/portal/screencast.rs` for why mode 1 could never work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pw_restore_token_monitor: Option<String>,
    /// ScreenCast restore token from the last successful WINDOW grant, the window
    /// captures' own slot. Same lifecycle as `pw_restore_token_monitor`: reissued
    /// on every use, cleared on a declined window request, the settings Forget
    /// row, and factory reset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pw_restore_token_window: Option<String>,
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
    // DRAGON-353: `preview_after_capture` ("Open in preview editor") lived here. The editor
    // is now the capture's destination unconditionally — it copies to the clipboard on open
    // and owns every save/share action — so the toggle is REMOVED. The preview-less
    // save+copy+notify path still exists for a `--no-editor` launch (DRAGON-428, including
    // the daemon's "(no editor)" hotkeys) and for the cases where no editor CAN open (no
    // known output). DRAGON-451 retired the third user of it, the region "Copy selection"
    // quick-action; DRAGON-479 brought that user back — as a FIXED, non-configurable primary+C
    // chord with no keymap entry and no settings row, so it is a third caller of the same
    // preview-less delivery and still not a persisted setting.
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
    /// brackets AND the side lines uniformly so they match, and (DRAGON-582) the colour
    /// picker's magnifier ring. Always applies (NOT gated by `appearance_use_system`).
    /// DRAGON-209. Default 3 (owner's call, DRAGON-588; it was 2, and this doc line said
    /// 4 while the code said 2, so the two are now one number in one place).
    #[serde(default = "default_selection_box_thickness")]
    pub selection_box_thickness: u32,
    /// About (DRAGON-177): show the launch-time "a new update is available" dialog
    /// when the settings-open update check resolves `Available`. Default ON. The
    /// About page exposes it as "Notify me when an update is available"; the launch
    /// dialog's "Don't remind me again" checkbox turns it OFF. A defaulted new field
    /// (absent key ⇒ `true`), so it needs no config migration.
    #[serde(default = "default_true")]
    pub notify_updates: bool,
    /// Cloud accounts (DRAGON-482): the account the preview editor's Upload flyout offers
    /// first, by `cloud::accounts::CloudAccount::id`. `None` until one is chosen; an id
    /// naming an account that has since been disconnected is ignored at use, so nothing
    /// has to clean it up. NOT a credential and NOT the account list: both of those live
    /// outside `config.toml` (see `cloud::accounts` for why), and this is only a memory of
    /// the last pick. Omitted from disk when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_last_account: Option<String>,
    /// Cloud accounts (DRAGON-482): create a share link for an upload and put it on the
    /// clipboard, without a second click. Default ON, per the settings-copy convention
    /// that a convenience toggle ships on. A defaulted new field (absent key yields
    /// `true`), so it needs no config migration of its own.
    #[serde(default = "default_true")]
    pub cloud_auto_share: bool,
}

fn default_input_sensitivity() -> f32 {
    0.5
}

fn default_screenshot_dir() -> String {
    "~/Capture".to_string()
}

fn default_covermark_text() -> String {
    "CONFIGURE TEXT IN SETTINGS".to_string()
}

fn default_covermark_opacity() -> f32 {
    0.195
}

fn default_annot_stroke_w() -> f32 {
    5.0
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

/// The colour picker's dim (DRAGON-582), the owner's 33%: light enough to read colours
/// through, dark enough to say the screen is not interactive right now.
fn default_color_picker_opacity() -> f32 {
    0.33
}

/// The magnifier's opening magnification (DRAGON-615), which is exactly the value the picker
/// used before it was persisted, so a user who has never changed it sees no difference at all.
///
/// Deferring to the picker's own const rather than repeating the number keeps this honest if
/// the designed span ever moves again, and keeps the `floor < default < ceiling` compile-time
/// assert in `color_picker::geom` the single authority on the bounds.
fn default_color_picker_zoom() -> u32 {
    crate::app::color_picker::geom::MAGNIFIER_ZOOM_DEFAULT
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
    20.0
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
    // ON everywhere (DRAGON-584, owner's call). macOS and Windows (DRAGON-296) have always
    // defaulted on, because their global capture hotkeys are dead without the daemon.
    //
    // LINUX defaulted OFF until now, on the reasoning that the one-shot model still exits and
    // PrintScreen is a COSMIC custom shortcut nothing else owns. That reasoning expired:
    // DRAGON-574 turned the tray into the app's primary launcher (Capture / Record / Countdown
    // / Audio submenus), and on a sandboxed install the user's PrintScreen shortcut points at a
    // dev binary path the sandbox does not even have, so a Linux user with no tray has no
    // obvious way in at all. DRAGON-583 goes further: with no portal GlobalShortcuts interface
    // on COSMIC, the tray plus the CLI is the ONLY way to control a live recording there.
    //
    // Existing configs are untouched: `resident` is a persisted field, so this changes what a
    // FRESH install gets and what the settings row's reset arrow restores, not anyone's saved
    // choice.
    true
}

fn default_record_backend() -> String {
    // Mirrors the retired `prefer_pipewire` default (on): Linux prefers the
    // portal (the dispatch clamp falls back to native per session when the
    // portal is unreachable); elsewhere the native backend is the only one.
    if cfg!(target_os = "linux") {
        crate::platform::backend::PORTAL_ID.to_string()
    } else {
        crate::platform::backend::native_backend_id().to_string()
    }
}

fn default_screenshot_backend() -> String {
    crate::platform::backend::native_backend_id().to_string()
}

/// The default "Capture All In One" global hotkey spec (the resident daemon's main key).
/// See [`Persisted::capture_hotkey`]. UNSET (empty) since DRAGON-295: a fresh install ships
/// with no global capture hotkey; the three capture actions are opt-in. Public so the daemon
/// can fall back to it (an empty fallback registers nothing, which is the intended "no key").
pub fn default_capture_hotkey() -> String {
    String::new()
}

/// The default "Capture Active Window" global hotkey spec. UNSET (empty) — opt-in
/// (DRAGON-295). See [`Persisted::capture_active_window_hotkey`].
pub fn default_capture_active_window_hotkey() -> String {
    String::new()
}

/// The default "Capture Active Monitor" global hotkey spec. UNSET (empty) — opt-in
/// (DRAGON-295). See [`Persisted::capture_active_monitor_hotkey`].
pub fn default_capture_active_monitor_hotkey() -> String {
    String::new()
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

/// Default region selection box thickness (px). DRAGON-209; raised 2 to 3 by the owner
/// (DRAGON-588). Persisted values are untouched: this moves what a fresh install gets and
/// what the settings reset arrow restores.
fn default_selection_box_thickness() -> u32 {
    3
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
    /// comes back as `None` instead of a parse error. This matters because a single
    /// failing entry fails the whole `Persisted` parse, and `store::load_raw` answers a
    /// failed parse with DEFAULTS — a stale override must drop silently, never take
    /// every other setting down with it.
    ///
    /// Retired names this has already had to absorb, so the list stays honest about how
    /// often it earns its keep: DRAGON-158 dropped `FocusSearch` and `PreviewNoAi`,
    /// DRAGON-467 dropped `PreviewSaveAs` and `PreviewDelete`, DRAGON-617 dropped the
    /// five it BAKED (`PreviewSave`, `PreviewUpload`, `PreviewCancel`, `PreviewUndo`,
    /// `PreviewRedo`), and DRAGON-627 dropped the three IT baked, the scanner's OCR text
    /// actions (`CopyText`, `SelectAllText`, `DeselectText`). The DRAGON-617 group is the
    /// biggest single drop, and it is exactly the case this was built for: a user who had
    /// customised some OTHER binding keeps it, because only the entries naming a retired
    /// action fall away.
    ///
    /// The DRAGON-627 three are the OLDEST names on that list, shipped since the first
    /// build, so they are the ones most likely to be sitting in a long-lived config. Pinned
    /// by `store`'s `the_baked_ocr_three_drop_and_the_users_other_bindings_survive`.
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
