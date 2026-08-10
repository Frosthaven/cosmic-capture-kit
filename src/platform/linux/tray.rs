//! System-tray recording controls: ONE StatusNotifierItem (via `ksni`) with a click
//! menu, the Linux counterpart of the macOS menu-bar item (`platform/mac/tray.rs`).
//!
//! A per-session status icon (DRAGON-174): raised at capture start (idle brackets + the
//! capture menu), kept for the WHOLE session, and flipped to the record dot + in-recording
//! menu once a recording begins. It ALWAYS carries the recording controls; the "Hide
//! toolbar on full screen captures" setting decides only the in-frame toolbar's visibility
//! (hidden when it can't fit outside a full-screen capture), never this icon's content.
//!
//! ## One item + menu (DRAGON-159, aligned to the mac daemon in DRAGON-172; the
//! DRAGON-574 restructure)
//!
//! The item shows ONE viewfinder icon whose centre glyph reads the state at a glance
//! (corner brackets always; + a solid centre dot while recording; + centre pause bars
//! while paused) and its menu is the ONE portable
//! [`crate::recording_ui::visible_tray_menu`] model, both states: while recording the
//! three controls lead (Pause / Resume Recording, Finish & Save Recording, Cancel &
//! Delete Recording), then the shared launcher group — Scanner, the "Capture" submenu,
//! the "Audio Recording: <state>" radio submenu, Settings... — with the flagged-off
//! rows HIDDEN (no Record, no Countdown Timer, no Quit while recording; the hide
//! round's why is on `visible_tray_menu`). This mirrors the resident's menu — labels, order, icons, and the
//! three-state icon all come from the ONE portable source (`crate::recording_ui`) so the
//! surfaces can never drift. The recording actions AND the radio picks send the same
//! [`TrayEvent`]s the app dispatches (on a child-owned surface the app process owns the
//! state: its handler flips + persists an audio arm while idle and drives the gain
//! automation while recording, and a countdown pick routes to the same `PickDelay`
//! handler as the overlay's delay chip); the launcher submenus spawn a fresh one-shot
//! process directly, like the daemon.
//!
//! The icon is rendered as a raw ARGB pixmap (NOT a themed icon name — several symbolic
//! names don't resolve in every panel, which showed as blank/white squares). We
//! rasterize the shared bracket SVG, tinted with the app's accent colour. `ksni`'s
//! blocking API owns its own thread + D-Bus connection. Failure-safe: if no host is
//! present, [`TraySession::start`] returns `None` and the caller keeps the in-frame
//! toolbar.

use crate::daemon_ipc::{Command, RecordingState};
use crate::recording_ui::{
    icon_state, visible_tray_menu, CaptureAction, IconState, MenuIcon, MenuItemKind,
    RadioSubmenu, RecordingAction, TrayItem,
};
use std::cell::RefCell;
use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Receiver, Sender};

/// A control the user activated from the tray. Mirrors the macOS enum so the app's
/// event handling is platform-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    Stop,
    TogglePause,
    ToggleMic,
    ToggleSystemAudio,
    /// An "Audio Recording" radio pick (DRAGON-558): the COMPLETE arm state chosen. The
    /// app — the owner of the arms — turns it into the toggle diff against its own state
    /// and runs each flip through the existing toggle handlers, so persist / meters /
    /// push-to-talk rules all apply exactly as if the toggles were clicked one by one.
    AudioArms(crate::recording_ui::AudioArmState),
    /// A "Countdown Timer" radio pick (DRAGON-574): the preset index (`delay_idx`)
    /// chosen. The app — the owner of the live delay state — routes it through the SAME
    /// `PickDelay` handler as the overlay's delay chip, so the pick persists and the
    /// chip follows in one place.
    CountdownPick(usize),
    /// Cancel + discard the recording. Emitted by this tray's Cancel recording menu item
    /// (DRAGON-172; the app dispatches it to `RecordingMsg::CancelRecording`, mirroring
    /// the macOS daemon menu). Now constructed on Linux (the menu builds it), so the
    /// DRAGON-170 `allow(dead_code)` for this variant is no longer needed here.
    Cancel,
    /// Quit the whole capture session/app (DRAGON-174). Emitted by the IDLE session icon's
    /// Quit item — a child-owned icon has no resident to quit, so the least-weird uniform
    /// behavior is to end this capture session (the app dispatches it to `finish_session`).
    Quit,
}

/// The state the item renders: whether a recording is in progress (idle vs recording icon
/// and menu), whether it's paused (record vs pause glyph), the two audio arms, and the
/// persisted countdown preset index (DRAGON-574, the Countdown Timer submenu's title +
/// mark; the app passes its own live `delay_idx` at start and a pick routes back through
/// the app's `PickDelay`). An idle session icon (DRAGON-174) sits at `recording: false`
/// until a recording begins.
#[derive(Clone, Copy, PartialEq, Eq)]
struct TrayState {
    recording: bool,
    paused: bool,
    mic: bool,
    system_audio: bool,
    delay_idx: usize,
}

/// The single recording tray item: its live state, the app's accent colour (the icon
/// tint, matching the toolbar buttons), the click channel, and a cache of the
/// last-rendered icon pixmap.
struct RecordingTray {
    state: TrayState,
    accent: [u8; 3],
    tx: Sender<TrayEvent>,
    /// The last-rendered icon, keyed by [`IconState`], so a host re-querying
    /// `icon_pixmap()` (some poll it on every panel redraw) doesn't re-run the usvg parse
    /// and resvg render when nothing changed. `ksni::Tray` methods are `&self`, so the
    /// cache needs interior mutability; `icon_pixmap` is the only reader/writer.
    icon_cache: std::cell::RefCell<Option<(IconState, Vec<ksni::Icon>)>>,
}

impl RecordingTray {
    /// The three-state icon for the current state (DRAGON-172/174): shared with the macOS
    /// daemon via [`crate::recording_ui`]. The session icon exists for the WHOLE capture
    /// session, so all three states are reachable: [`IconState::Idle`] (corner brackets)
    /// while not recording (during selection), [`IconState::Recording`] (centre dot) while
    /// recording, [`IconState::Paused`] (pause bars) while paused. The corner brackets are
    /// constant so the icon never shifts; only the centre glyph changes.
    fn icon_state(&self) -> IconState {
        icon_state(self.state.recording, self.state.paused)
    }
}

impl ksni::Tray for RecordingTray {
    /// A left-click opens the menu (all controls live there now) instead of firing a
    /// single action, so every control is one click away. Right-click also shows it.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "dev.thedragon.CosmicCaptureKit.Recording".to_string()
    }

    fn title(&self) -> String {
        "Cosmic Capture Kit recording".to_string()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let key = self.icon_state();
        if let Some((cached_key, icons)) = &*self.icon_cache.borrow()
            && *cached_key == key
        {
            return icons.clone();
        }
        let icons = render_state_icon(key, self.accent).map(|i| vec![i]).unwrap_or_default();
        *self.icon_cache.borrow_mut() = Some((key, icons.clone()));
        icons
    }

    /// The control menu: the ONE portable [`visible_tray_menu`] model (DRAGON-574 +
    /// the hide round), both states. While NOT recording (the idle session icon):
    /// Scanner, the Capture and Record submenus, the Countdown Timer and Audio
    /// Recording radio submenus, a separator, Settings..., Quit (a child-owned idle
    /// icon quits the session/app). While recording: the three controls and a
    /// separator lead, then the same group with the flagged-off rows HIDDEN (no
    /// Record, no Countdown Timer, no Quit; the why is on `visible_tray_menu`).
    /// Labels, order, icons and visibility all come from the model so this surface
    /// matches the resident / daemons exactly. A left-click opens the menu, so every
    /// control is one click away.
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        build_menu_items(self.state, self.accent)
    }
}

/// Map a shared `RecordingAction` to the `TrayEvent` the app dispatches, and send it.
/// Module-level (not nested in `menu`) since DRAGON-558. The Audio Recording submenu does
/// NOT come through here: a radio pick sends [`TrayEvent::AudioArms`] whole, and the app
/// (which owns the arms in both menu states on a child-owned surface) computes the toggle
/// diff itself — so no context split is needed in tray code, unlike the resident's
/// `audio_pick`.
fn send_recording(t: &RecordingTray, action: RecordingAction) {
    let ev = match action {
        RecordingAction::TogglePause => TrayEvent::TogglePause,
        RecordingAction::ToggleMic => TrayEvent::ToggleMic,
        RecordingAction::ToggleSystemAudio => TrayEvent::ToggleSystemAudio,
        RecordingAction::Stop => TrayEvent::Stop,
        RecordingAction::Cancel => TrayEvent::Cancel,
    };
    let _ = t.tx.send(ev);
}

/// The whole child-owned dropdown as ksni items, walked from the ONE portable
/// [`visible_tray_menu`] model (DRAGON-574; hidden rows never reach us). Differences
/// from the resident's walker, all
/// child-owned routing rather than shape: the controls and the radio picks go through
/// the [`TrayEvent`] channel (the APP owns the state on this surface), Quit uses the
/// "Quit Cosmic Capture Kit" label (there is no resident to quit; while idle it ends the
/// session, and its enabled flag comes from the model), and the launcher spawns are
/// identical (a fresh one-shot process, like the daemon).
fn build_menu_items(state: TrayState, accent: [u8; 3]) -> Vec<ksni::MenuItem<RecordingTray>> {
    use ksni::menu::{StandardItem, SubMenu};
    let mut items: Vec<ksni::MenuItem<RecordingTray>> = Vec::new();
    for entry in visible_tray_menu(state.recording, state.paused) {
        match entry {
            TrayItem::Separator => items.push(ksni::MenuItem::Separator),
            TrayItem::Control(control) => match control.kind {
                MenuItemKind::Separator => items.push(ksni::MenuItem::Separator),
                MenuItemKind::Standard | MenuItemKind::Checkmark(_) => {
                    let action = control.action;
                    items.push(
                        StandardItem {
                            label: control.label.to_string(),
                            // The control rows: `menu_icon_png` applies the shared
                            // three-way rule (Finish success-green, Discard red,
                            // Pause/Resume the accent), whatever the accent.
                            icon_data: menu_icon_png(control.icon, accent),
                            activate: Box::new(move |t: &mut RecordingTray| {
                                send_recording(t, action)
                            }),
                            ..Default::default()
                        }
                        .into(),
                    );
                }
            },
            TrayItem::Action { item, enabled } => match item.action {
                CaptureAction::Quit => items.push(
                    StandardItem {
                        label: "Quit Cosmic Capture Kit".to_string(),
                        enabled,
                        icon_data: menu_icon_png(item.icon, accent),
                        activate: Box::new(move |t: &mut RecordingTray| {
                            let _ = t.tx.send(TrayEvent::Quit);
                        }),
                        ..Default::default()
                    }
                    .into(),
                ),
                action => {
                    // `spawn_args` because a record entry's argv is its capture twin's
                    // flag plus `--video` (DRAGON-559). A row that spawns a child defers
                    // past the menu dismiss (DRAGON-574: the dropdown was getting baked
                    // into the fallback's frozen frame; DRAGON-598 derived the set from
                    // `spawn_args` so a new row cannot fall out of it). DRAGON-600 moved
                    // the wait into the child: the host closes its dropdown only when the
                    // launched process takes keyboard focus, so waiting here would only
                    // hold the menu up longer.
                    let args = action.spawn_args().unwrap_or(&["--region"]);
                    let from_menu = crate::recording_ui::spawn_waits_for_menu_dismiss(action);
                    items.push(
                        StandardItem {
                            label: item.label.to_string(),
                            enabled,
                            icon_data: menu_icon_png(item.icon, accent),
                            activate: Box::new(move |_t: &mut RecordingTray| {
                                if from_menu {
                                    crate::recording_ui::spawn_capture_child_args_from_menu(
                                        args,
                                        Vec::new(),
                                    );
                                } else {
                                    crate::recording_ui::spawn_capture_child_args(args, &[]);
                                }
                            }),
                            ..Default::default()
                        }
                        .into(),
                    );
                }
            },
            TrayItem::Launchers { menu, enabled, items: rows } => items.push(
                SubMenu {
                    label: menu.label().to_string(),
                    enabled,
                    icon_data: menu_icon_png(Some(menu.icon()), accent),
                    submenu: rows
                        .into_iter()
                        .map(|row| {
                            let args = row.action.spawn_args().unwrap_or(&["--region"]);
                            StandardItem {
                                label: row.label.to_string(),
                                // Belt and braces: a host that opens a disabled submenu
                                // still gets inert rows.
                                enabled,
                                icon_data: menu_icon_png(row.icon, accent),
                                // Every launcher row grabs pixels, so every one is tagged
                                // as menu-launched (DRAGON-574, DRAGON-600).
                                activate: Box::new(move |_t: &mut RecordingTray| {
                                    crate::recording_ui::spawn_capture_child_args_from_menu(
                                        args,
                                        Vec::new(),
                                    );
                                }),
                                ..Default::default()
                            }
                            .into()
                        })
                        .collect(),
                    ..Default::default()
                }
                .into(),
            ),
            TrayItem::Radio { menu: RadioSubmenu::Audio, enabled } => {
                items.push(audio_submenu_items(state, enabled, accent));
            }
            TrayItem::Radio { menu: RadioSubmenu::Countdown, enabled } => {
                items.push(countdown_submenu_items(state, enabled, accent));
            }
        }
    }
    items
}

/// The "Audio Recording: <state>" radio SUBMENU as one ksni item (DRAGON-558), the
/// child-owned twin of the resident's `audio_submenu_items`. The title and radio mark come
/// from the app-reported arms — which ARE the persisted arms while idle (the app's toggle
/// handler persists on every flip) and the live channel arms while recording, so no config
/// read is needed here. A pick maps its index back through
/// [`crate::recording_ui::audio_arm_choice_at`] (a stray index arms nothing) and sends
/// [`TrayEvent::AudioArms`]: the APP, which owns the arms, computes the toggle diff against
/// its own state and applies each flip through its usual handlers, in both menu states.
fn audio_submenu_items(
    state: TrayState,
    enabled: bool,
    accent: [u8; 3],
) -> ksni::MenuItem<RecordingTray> {
    use crate::recording_ui::{
        audio_arm_choice_at, audio_arm_items, audio_submenu_title, AudioArmState,
        AUDIO_ARM_ORDER,
    };
    use ksni::menu::{RadioGroup, RadioItem, SubMenu};
    let current = AudioArmState::from_arms(state.mic, state.system_audio);
    let rows = audio_arm_items(current);
    let selected = AUDIO_ARM_ORDER.iter().position(|s| *s == current).unwrap_or(0);
    SubMenu {
        label: audio_submenu_title(current),
        enabled,
        icon_data: menu_icon_png(Some(RadioSubmenu::Audio.icon()), accent),
        submenu: vec![RadioGroup {
            selected,
            select: Box::new(|t: &mut RecordingTray, index: usize| {
                if let Some(choice) = audio_arm_choice_at(index) {
                    let _ = t.tx.send(TrayEvent::AudioArms(choice));
                }
            }),
            options: rows
                .into_iter()
                .map(|row| RadioItem { label: row.label.to_string(), ..Default::default() })
                .collect(),
        }
        .into()],
        ..Default::default()
    }
    .into()
}

/// The "Countdown Timer: NN" radio SUBMENU as one ksni item (DRAGON-574), the child-owned
/// twin of the resident's `countdown_submenu_items`. The title and radio mark come from
/// the app-reported `delay_idx` (the app passes its live value at tray start). A pick maps
/// its index back through [`crate::recording_ui::countdown_choice_at`] (a stray index
/// persists nothing) and sends [`TrayEvent::CountdownPick`]: the APP, which owns the delay
/// state, routes it through the same `PickDelay` handler as the overlay's chip, so persist
/// and the live chip follow in one place. The pick callback re-checks the recording flag
/// because a host may still deliver picks into a disabled submenu.
fn countdown_submenu_items(
    state: TrayState,
    enabled: bool,
    accent: [u8; 3],
) -> ksni::MenuItem<RecordingTray> {
    use crate::recording_ui::{countdown_choice_at, countdown_items, countdown_submenu_title};
    use ksni::menu::{RadioGroup, RadioItem, SubMenu};
    let rows = countdown_items(state.delay_idx);
    let selected = rows.iter().position(|r| r.selected).unwrap_or(0);
    SubMenu {
        label: countdown_submenu_title(state.delay_idx),
        enabled,
        icon_data: menu_icon_png(Some(RadioSubmenu::Countdown.icon()), accent),
        submenu: vec![RadioGroup {
            selected,
            select: Box::new(|t: &mut RecordingTray, index: usize| {
                if t.state.recording {
                    return;
                }
                if let Some(idx) = countdown_choice_at(index) {
                    t.state.delay_idx = idx;
                    let _ = t.tx.send(TrayEvent::CountdownPick(idx));
                }
            }),
            options: rows
                .into_iter()
                .map(|row| RadioItem { label: row.label, ..Default::default() })
                .collect(),
        }
        .into()],
        ..Default::default()
    }
    .into()
}

/// Rasterize a menu-item ICON (DRAGON-574) as the PNG bytes ksni's `icon_data` carries:
/// the shared [`MenuIcon`] glyph tinted by the ONE portable rule
/// ([`crate::recording_ui::menu_icon_tint`]): the app's accent for the ordinary rows
/// (the same colour as the tray icon's brackets, visible on light and dark menu
/// backgrounds alike, Pause/Resume included), success green for Finish & Save and
/// RECORDING_RED for Cancel & Delete (the fixed-tint pair). Rendered at
/// a 16px cell: the freedesktop MENU-ICON standard size, the platform's own metric for
/// a menu row's leading glyph (each platform derives its cell from its own standard,
/// see the Windows SM_CXMENUCHECK read and the mac 16pt NSMenu convention). Hosts
/// that do not render menu icons ignore the field, so this is
/// additive; whether COSMIC's own applet renders them is a live-test question. Cached
/// per (icon, tint): hosts re-query the menu on every state change and the usvg parse
/// is not free. `None` (no icon assigned) and any rasterize/encode failure yield an
/// empty Vec, which ksni treats as "no icon".
pub(crate) fn menu_icon_png(icon: Option<MenuIcon>, color: [u8; 3]) -> Vec<u8> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    /// The (icon, tint) to PNG-bytes cache behind [`menu_icon_png`].
    type IconPngCache = HashMap<(MenuIcon, [u8; 3]), Vec<u8>>;
    let Some(icon) = icon else {
        return Vec::new();
    };
    let tint = crate::recording_ui::menu_icon_tint(icon, color);
    static CACHE: Mutex<Option<IconPngCache>> = Mutex::new(None);
    let key = (icon, tint);
    if let Ok(guard) = CACHE.lock()
        && let Some(cache) = &*guard
        && let Some(png) = cache.get(&key)
    {
        return png.clone();
    }
    let png = render_menu_icon_png(icon, tint).unwrap_or_default();
    if let Ok(mut guard) = CACHE.lock() {
        guard.get_or_insert_with(HashMap::new).insert(key, png.clone());
    }
    png
}

/// The uncached raster body of [`menu_icon_png`]: tint via the shared
/// [`crate::recording_ui::menu_icon_svg_tinted`] substitution, scale the glyphs' 24-unit
/// box down to the cell (it rendered 1:1 while the cell was 24px), encode PNG.
fn render_menu_icon_png(icon: MenuIcon, color: [u8; 3]) -> Option<Vec<u8>> {
    /// The menu icon cell: 16px, the freedesktop MENU-ICON standard (the icon-theme
    /// spec's "menu" context ships 16x16, and GTK/Qt menus request that size). The
    /// platform metric, not taste: the size round first cut the 24px default by a
    /// third, and the owner's follow-up set the rule to "each platform's native
    /// menu-item icon size", which on Linux is the same 16. The cache key stays
    /// (icon, tint): the size is this compile-time const, so it cannot vary within a
    /// build.
    const S: u32 = 16;
    /// The Lucide glyphs' own viewBox, the source of the scale below.
    const BOX: f32 = 24.0;
    let svg = crate::recording_ui::menu_icon_svg_tinted(icon, color);
    let tree =
        resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(S, S)?;
    let scale = S as f32 / BOX;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().ok()
}

/// Rasterize the three-state recording glyph for `state` into a square ARGB32 tray icon —
/// full-bleed so it fills the panel's icon slot. Rendered ourselves (not a themed icon name)
/// so it looks identical on every host.
///
/// The COLOURS come from `recording_ui::tray_icon_svg`, shared with the Windows tray: accent
/// brackets, red centre glyph while a recording exists. Until DRAGON-541 this took a
/// pre-blackened SVG and one colour, and tinted the whole thing with it, so the recording dot
/// and the pause bars came out the same accent as the brackets around them and a recording
/// never read red on Linux. Taking the STATE rather than an SVG is what moved that choice to
/// the one place both platforms read it from.
///
/// `pub(crate)` so the resident tray (`crate::daemon_linux`, DRAGON-173) renders its
/// three-state icon through the SAME rasterizer, keeping the two Linux surfaces pixel-
/// identical.
pub(crate) fn render_state_icon(state: IconState, accent: [u8; 3]) -> Option<ksni::Icon> {
    render_hex(&crate::recording_ui::tray_icon_svg(state, accent), false)
}

/// [`render_icon`], but fitting the glyph's INK to the pixmap instead of its viewBox
/// (DRAGON-500). For the upload counter (`platform/linux/upload_tray.rs`), whose faces do not
/// all use the same amount of the shared 24-unit box.
///
/// The recording icon deliberately does NOT come through here. Its bracket glyph carries a
/// ~13% margin of its own and has looked that way on the COSMIC panel since DRAGON-173; the
/// icon nobody complained about stays byte-identical. Passing it through this instead would
/// grow it by about a quarter, which is what Windows chose for the same glyph and the same
/// reason (DRAGON-249) — a one-line swap here if the owner ever wants the two to match.
pub(crate) fn render_icon_fitted(svg_black: &str, color: [u8; 3]) -> Option<ksni::Icon> {
    render_hex(&recolour(svg_black, color), true)
}

/// Rasterize an already PIXEL-GRID SVG 1:1 at its own `px` cell (DRAGON-539): identity
/// transform, so every integer-aligned block edge stays a hard pixel. For the upload
/// counter's digit faces, which `cloud::tray::counter_pixel_svg` generates at exactly this
/// size; the glyph faces keep the ink-fitted 64px path above, and the recording icon keeps
/// [`render_icon`].
pub(crate) fn render_icon_exact(svg_black: &str, color: [u8; 3], px: u32) -> Option<ksni::Icon> {
    let svg = recolour(svg_black, color);
    let tree =
        resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px)?;
    resvg::render(&tree, resvg::tiny_skia::Transform::identity(), &mut pixmap.as_mut());
    Some(pack_argb(&pixmap))
}

/// Recolour a `#000` template SVG to `color`, the one substitution both rasterizers share.
fn recolour(svg_black: &str, color: [u8; 3]) -> String {
    let hex = format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]);
    // Recolour both the stroked brackets and the filled centre glyph to the accent.
    svg_black.replace("#000", &hex)
}

/// Rasterize an already-coloured SVG into the ARGB32 tray icon pixmap. `fit_ink` scales the
/// glyph's own ink box up to the cell rather than mapping the viewBox 1:1; see
/// [`render_icon_fitted`] for who wants which.
fn render_hex(svg: &str, fit_ink: bool) -> Option<ksni::Icon> {
    // 64px source: comfortably above typical panel icon sizes (22-48) so the host
    // downscales (crisp) rather than upscales (blurry).
    const S: u32 = 64;
    /// The viewBox edge every glyph this renders is drawn in (`cloud::tray`, `recording_ui`).
    const BOX: f32 = 24.0;
    let tree = resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(S, S)?;
    let transform = if fit_ink {
        // usvg measures the ink for us, so no face has to hand-declare its own bounds the way
        // the Windows daemon's rasterizer does; a redrawn glyph is re-fitted automatically.
        //
        // The STROKE box, not the plain one (DRAGON-516). usvg's `abs_bounding_box` is the
        // geometry alone, so a stroked glyph — which is what `Face::Indeterminate` became when
        // it took lucide's `cloud-upload` outline — would be fitted as if its outline had no
        // width, and half a stroke width would land outside the cell: on a 64px pixmap that is
        // about 3px, against a 1.9px margin, so the round caps came back shaved. A FILLED face
        // measures the same either way, so the digits, tick and cross are untouched.
        let ink = tree.root().abs_stroke_bounding_box();
        let (scale, dx, dy) = crate::cloud::upload::tray::icon_ink_fit(
            (ink.x(), ink.y(), ink.width(), ink.height()),
            BOX,
            S as f32,
            crate::cloud::upload::tray::ICON_INK_MARGIN,
        );
        resvg::tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy)
    } else {
        let scale = S as f32 / BOX;
        resvg::tiny_skia::Transform::from_scale(scale, scale)
    };
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(pack_argb(&pixmap))
}

/// Pack a rendered pixmap as the icon an SNI host expects: tiny-skia is premultiplied, so
/// emit straight-alpha ARGB32 in network (big-endian) byte order, [A, R, G, B] per pixel.
/// Shared by the fitted and the exact rasterizers (DRAGON-539), so the packing cannot
/// drift between them.
fn pack_argb(pixmap: &resvg::tiny_skia::Pixmap) -> ksni::Icon {
    let mut data = Vec::with_capacity((pixmap.width() * pixmap.height() * 4) as usize);
    for px in pixmap.pixels() {
        let c = px.demultiply();
        data.extend_from_slice(&[c.alpha(), c.red(), c.green(), c.blue()]);
    }
    ksni::Icon { width: pixmap.width() as i32, height: pixmap.height() as i32, data }
}

/// A live recording control surface. Two backings share the seam the app calls
/// (`start` / `start_daemon` / `poll` / `set_audio` / `set_paused` / `Drop`),
/// byte-identical to the macOS `TraySession` (DRAGON-173):
///
/// * [`Backing::Daemon`] — when the resident tray RESIDENT (`crate::daemon_linux`)
///   spawned this recording, the in-recording actions live in the RESIDENT's ONE tray
///   item. This session raises NO item of its own: it relays the recording state to the
///   resident over the IPC socket and drains the resident's menu commands (as
///   [`TrayEvent`]s) from a reader thread. Preferred whenever a resident is reachable, so
///   a resident-launched recording shows a SINGLE tray item, not two.
/// * [`Backing::Own`] — the historical Linux control surface: this process's OWN `ksni`
///   StatusNotifierItem + menu. Used for recordings with NO resident (terminal / CLI
///   launches, or the resident off), so non-resident recordings never lose their controls.
pub struct TraySession {
    backing: Backing,
}

enum Backing {
    /// Relay to the resident's tray item over the IPC socket.
    Daemon(DaemonBacking),
    /// This process's own `ksni` item (no resident attached).
    Own(OwnBacking),
}

/// The own-item control: the single `ksni` item handle (dropped → the item vanishes)
/// and the shared channel of clicks the app drains on a timer.
struct OwnBacking {
    handle: ksni::blocking::Handle<RecordingTray>,
    events: Receiver<TrayEvent>,
}

/// The resident-backed control: the socket write half (to push state) + the last full
/// state (so a partial `set_audio` / `set_paused` resends the whole snapshot) behind a
/// `RefCell` (the seam is `&self`; this session is drained only on the app's UI thread, so
/// the borrow is never contended), and the command channel a reader thread fills from the
/// resident's menu clicks. Mirrors the macOS `DaemonBacking` exactly.
struct DaemonBacking {
    conn: RefCell<UnixStream>,
    state: RefCell<RecordingState>,
    commands: Receiver<TrayEvent>,
}

impl TraySession {
    /// Raise this process's OWN tray item in the RECORDING state: a recording is
    /// starting with no resident present. The own icon exists ONLY while a recording
    /// is live (DRAGON-182 — no idle session icon) and drops with it. Returns `None`
    /// when no StatusNotifierItem host is available (the caller keeps the in-frame
    /// toolbar). `delay_idx` (DRAGON-574) is the app's live countdown preset, which the
    /// disabled-while-recording Countdown Timer submenu titles itself from.
    ///
    /// Prefer [`TraySession::start_daemon`] first: when a resident is present, ALL
    /// in-recording controls belong in the resident's ONE tray item.
    pub fn start_recording(
        mic: bool,
        system_audio: bool,
        accent: [u8; 3],
        delay_idx: usize,
    ) -> Option<Self> {
        let own = OwnBacking::start(true, mic, system_audio, accent, delay_idx)?;
        Some(TraySession { backing: Backing::Own(own) })
    }

    /// Try to route the in-recording controls to the resident tray RESIDENT's item
    /// (DRAGON-173): connect the IPC socket, push the initial state, and drain the
    /// resident's menu commands. Returns `None` when no resident is listening (the caller
    /// then decides between its own item and the in-frame toolbar). Preferred whenever a
    /// resident is present, so a resident-launched recording shows a SINGLE tray item.
    /// Portable seam parity with the macOS `TraySession::start_daemon`.
    pub fn start_daemon(mic: bool, system_audio: bool) -> Option<Self> {
        let d = DaemonBacking::connect(mic, system_audio)?;
        Some(TraySession { backing: Backing::Daemon(d) })
    }

    /// Drain and return every click since the last poll.
    pub fn poll(&self) -> Vec<TrayEvent> {
        match &self.backing {
            Backing::Daemon(d) => d.commands.try_iter().collect(),
            Backing::Own(o) => o.events.try_iter().collect(),
        }
    }

    /// Reflect the current mic / system-audio state (resident: resend the snapshot; own:
    /// update the menu checkmarks via the ksni handle).
    pub fn set_audio(&self, mic: bool, system_audio: bool) {
        match &self.backing {
            Backing::Daemon(d) => {
                {
                    let mut st = d.state.borrow_mut();
                    st.mic = mic;
                    st.system = system_audio;
                }
                d.push();
            }
            Backing::Own(o) => {
                o.handle.update(|t| {
                    t.state.mic = mic;
                    t.state.system_audio = system_audio;
                });
            }
        }
    }

    /// Reflect pause state (resident: resend the snapshot so the icon + Pause/Resume label
    /// update there; own: flip the item's icon + Pause/Resume label via the ksni handle).
    pub fn set_paused(&self, paused: bool) {
        match &self.backing {
            Backing::Daemon(d) => {
                d.state.borrow_mut().paused = paused;
                d.push();
            }
            Backing::Own(o) => {
                o.handle.update(|t| {
                    t.state.paused = paused;
                });
            }
        }
    }
}

impl OwnBacking {
    /// Raise this process's own `ksni` session item in the given state (idle when
    /// `recording` is false, recording otherwise). Returns `None` when no
    /// StatusNotifierItem host is available.
    fn start(
        recording: bool,
        mic: bool,
        system_audio: bool,
        accent: [u8; 3],
        delay_idx: usize,
    ) -> Option<Self> {
        use ksni::blocking::TrayMethods;
        let (tx, events) = std::sync::mpsc::channel();
        let tray = RecordingTray {
            state: TrayState { recording, paused: false, mic, system_audio, delay_idx },
            accent,
            tx,
            icon_cache: std::cell::RefCell::new(None),
        };
        // Same sandbox rule as the resident's tray; see `daemon.rs` for why owning the
        // well-known StatusNotifierItem name is impossible inside a Flatpak.
        match tray.disable_dbus_name(crate::util::flatpak_sandboxed()).spawn() {
            Ok(handle) => Some(OwnBacking { handle, events }),
            Err(e) => {
                log::warn!("recording tray unavailable ({e}); keeping the in-frame toolbar");
                None
            }
        }
    }
}

impl DaemonBacking {
    /// Try to attach to a running resident: connect the IPC socket, push the initial
    /// recording state, and start a reader thread that turns the resident's command lines
    /// into [`TrayEvent`]s. Returns `None` when no resident is listening (the socket is
    /// absent or refuses), so the caller falls back to its own item. Mirrors the macOS
    /// `DaemonBacking::connect`.
    fn connect(mic: bool, system_audio: bool) -> Option<Self> {
        let path = crate::daemon_ipc::socket_path();
        let conn = UnixStream::connect(&path).ok()?;
        let reader = conn.try_clone().ok()?;
        let (tx, commands) = std::sync::mpsc::channel();
        // Reader thread: block on resident command lines, map each to its TrayEvent, and
        // push into the channel the app drains on its tray poll. Exits on EOF / a closed
        // receiver (session dropped) — the write half closing signals the resident too.
        std::thread::Builder::new()
            .name("cck-resident-ipc-reader".into())
            .spawn(move || {
                use std::io::{BufRead as _, BufReader};
                let r = BufReader::new(reader);
                for line in r.lines() {
                    let Ok(line) = line else { break };
                    if let Some(cmd) = Command::parse(&line) {
                        let ev = match cmd {
                            Command::TogglePause => TrayEvent::TogglePause,
                            Command::ToggleMic => TrayEvent::ToggleMic,
                            Command::ToggleSystemAudio => TrayEvent::ToggleSystemAudio,
                            Command::Stop => TrayEvent::Stop,
                            Command::Cancel => TrayEvent::Cancel,
                        };
                        if tx.send(ev).is_err() {
                            break;
                        }
                    }
                }
            })
            .ok()?;
        let d = DaemonBacking {
            conn: RefCell::new(conn),
            state: RefCell::new(RecordingState {
                recording: true,
                paused: false,
                mic,
                system: system_audio,
            }),
            commands,
        };
        d.push();
        Some(d)
    }

    /// Push the current full state to the resident (best-effort; a broken pipe is ignored,
    /// the resident reverts on EOF).
    fn push(&self) {
        let line = self.state.borrow().encode();
        let mut conn = self.conn.borrow_mut();
        let _ = conn.write_all(line.as_bytes());
        let _ = conn.flush();
    }
}

impl Drop for DaemonBacking {
    /// Tell the resident the recording ended (state recording=false), then close the
    /// connection so the resident reverts even if the final line is lost.
    fn drop(&mut self) {
        self.state.borrow_mut().recording = false;
        self.push();
        // Dropping `conn` (and the reader's clone once the reader thread exits) closes the
        // socket; the resident's read hits EOF and reverts to idle.
    }
}

impl Drop for OwnBacking {
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

// ── The countdown tray (DRAGON-563) ──────────────────────────────────────────
//
// While a pre-capture countdown runs, the remaining seconds render IN the tray icon:
// the upload counter's pixel digits (`cloud::upload::tray::counter_pixel_svg`, one
// exact pixmap per `upload_tray::DIGIT_SIZES` cell), tinted the SAME red as the
// recording glyph (`recording_ui::RECORDING_RED_RGB`), with one menu entry, Cancel
// countdown. Minted on EVERY session (the owner ungated it: "we can have that across
// the board"): layer-shell sessions keep their on-screen countdown and get the digits
// in addition, while the `lab/flatpak` portal-frozen fallback path gets them as its
// ONLY countdown surface: there the plain toplevel had no live desktop behind it (the
// timer counted down over a gray sheet), window/monitor countdowns had no surface at
// all, and with the selection window closed the tray is the only cancel surface left
// (Escape has nothing to land on). The item follows the recording tray's own lifecycle
// shape (`OwnBacking`): raised at countdown start, updated once per tick, dropped when
// the countdown fires or is cancelled. No daemon relay: the resident IPC protocol
// carries recording state only, and a countdown is a seconds-long child-local phase; a
// second icon for its duration is the honest cost.

/// The countdown item: the remaining seconds it draws, the click channel, and the
/// per-second icon cache (hosts re-query `icon_pixmap` on every panel redraw).
struct CountdownTray {
    remaining: u8,
    tx: Sender<TrayEvent>,
    icon_cache: RefCell<Option<(u8, Vec<ksni::Icon>)>>,
}

impl ksni::Tray for CountdownTray {
    /// A left-click opens the menu (the cancel entry), like every other item of ours.
    const MENU_ON_ACTIVATE: bool = true;

    /// A bare constant, like the recording tray's: only one countdown child exists at a
    /// time (the single-instance locks see to that), so no per-pid suffix is needed.
    fn id(&self) -> String {
        "dev.thedragon.CosmicCaptureKit.Countdown".to_string()
    }

    fn title(&self) -> String {
        crate::recording_ui::countdown_tooltip(self.remaining)
    }

    /// The tooltip a panel shows on hover: the same sentence as the title, from the ONE
    /// shared builder, so the two can never say different things.
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: crate::recording_ui::countdown_tooltip(self.remaining),
            ..Default::default()
        }
    }

    /// The remaining seconds as pixel digits, one exact-size pixmap per advertised cell
    /// (the DRAGON-539 rule: the host picks its nearest and shows it 1:1, so the blocks
    /// stay on the pixel grid), tinted [`crate::recording_ui::RECORDING_RED_RGB`]: the
    /// recording glyph's own red, per the owner's DRAGON-563 spec, never a new one.
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        if let Some((cached, icons)) = &*self.icon_cache.borrow()
            && *cached == self.remaining
        {
            return icons.clone();
        }
        let icons: Vec<ksni::Icon> = crate::platform::linux::upload_tray::DIGIT_SIZES
            .iter()
            .filter_map(|&px| {
                render_icon_exact(
                    &crate::cloud::upload::tray::counter_pixel_svg(self.remaining, px),
                    crate::recording_ui::RECORDING_RED_RGB,
                    px,
                )
            })
            .collect();
        *self.icon_cache.borrow_mut() = Some((self.remaining, icons.clone()));
        icons
    }

    /// One entry: [`crate::recording_ui::COUNTDOWN_CANCEL_LABEL`]. The same
    /// `StandardItem` + `activate` shape the recording tray's `menu()` uses; the closure
    /// sends [`TrayEvent::Cancel`], which the app routes to the ordinary Cancelled ending.
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: crate::recording_ui::COUNTDOWN_CANCEL_LABEL.to_string(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(TrayEvent::Cancel);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// A live countdown item (DRAGON-563). Dropping it removes the item from the panel.
pub struct CountdownTraySession {
    handle: ksni::blocking::Handle<CountdownTray>,
    events: Receiver<TrayEvent>,
}

impl CountdownTraySession {
    /// Raise the countdown item showing `remaining` seconds. Returns `None` when no
    /// StatusNotifierItem host is available; what stands in then is the caller's call
    /// (`enter_countdown`): normal sessions keep the historical window countdown, the
    /// portal-fallback path proceeds with no visual countdown at all (DRAGON-563
    /// reopened — the gray sheet must never come back as a failure-safe).
    ///
    /// BOTH outcomes log (DRAGON-563 reopened): the owner's fourth sandbox test produced
    /// a session where the countdown misbehaved and the log carried not one line about
    /// this item, which made the report undiagnosable. Behavior only, never content.
    pub fn start(remaining: u8) -> Option<Self> {
        use ksni::blocking::TrayMethods;
        let (tx, events) = std::sync::mpsc::channel();
        let tray = CountdownTray { remaining, tx, icon_cache: RefCell::new(None) };
        // Same sandbox rule as the recording tray; see `daemon.rs` for why owning the
        // well-known StatusNotifierItem name is impossible inside a Flatpak.
        match tray.disable_dbus_name(crate::util::flatpak_sandboxed()).spawn() {
            Ok(handle) => {
                log::info!("countdown tray up ({remaining}s)");
                Some(CountdownTraySession { handle, events })
            }
            Err(e) => {
                log::warn!("countdown tray unavailable ({e})");
                None
            }
        }
    }

    /// Draw the new remaining seconds (and re-title for the tooltip).
    pub fn set_remaining(&self, remaining: u8) {
        self.handle.update(|t: &mut CountdownTray| {
            t.remaining = remaining;
        });
    }

    /// Drain and return every click since the last poll.
    pub fn poll(&self) -> Vec<TrayEvent> {
        self.events.try_iter().collect()
    }
}

impl Drop for CountdownTraySession {
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `id()` / `title()` are `ksni::Tray` trait methods; the trait must be in scope to
    // call them (the `impl` alone does not import it). DRAGON-159 added the test on mac,
    // where `tray.rs` is never compiled, so this only surfaced on the Linux build.
    use ksni::Tray as _;

    fn state(recording: bool, paused: bool, mic: bool, system_audio: bool) -> TrayState {
        TrayState { recording, paused, mic, system_audio, delay_idx: 0 }
    }

    fn tray(state: TrayState) -> RecordingTray {
        let (tx, _rx) = std::sync::mpsc::channel();
        RecordingTray {
            state,
            accent: [0x33, 0x99, 0xff],
            tx,
            icon_cache: std::cell::RefCell::new(None),
        }
    }

    #[test]
    fn icon_state_covers_idle_recording_and_paused() {
        // The session icon (DRAGON-174) reaches all three states: idle brackets during
        // selection, the record dot while recording, the pause bars while paused — the
        // shared three-state icon from `recording_ui`.
        let idle = tray(state(false, false, false, false));
        let recording = tray(state(true, false, true, true));
        let paused = tray(state(true, true, true, true));
        assert_eq!(idle.icon_state(), IconState::Idle);
        assert_eq!(recording.icon_state(), IconState::Recording);
        assert_eq!(paused.icon_state(), IconState::Paused);
        // The SVGs the TRAY actually rasterizes differ between all three states, which is a
        // stronger claim than the raw geometry differing: DRAGON-541's bug was two states
        // whose geometry differed and whose COLOURS did not, so a paused recording and an
        // idle selector were the same accent and the dot never read as recording.
        let accent = [0x00, 0x78, 0xd4];
        let svg = |t: &RecordingTray| crate::recording_ui::tray_icon_svg(t.icon_state(), accent);
        assert_ne!(svg(&idle), svg(&recording));
        assert_ne!(svg(&recording), svg(&paused));
        // Both ACTIVE states carry the red centre; the idle one has no centre to redden.
        for (name, t) in [("recording", &recording), ("paused", &paused)] {
            assert!(
                svg(t).contains(crate::recording_ui::RECORDING_RED),
                "{name} must show a red centre glyph on the Linux tray, not the accent"
            );
            assert!(svg(t).contains("#0078d4"), "{name} keeps accent brackets");
        }
        assert!(!svg(&idle).contains(crate::recording_ui::RECORDING_RED));
    }

    /// Flatten a ksni menu to `(label, enabled)` pairs (a submenu becomes its own label;
    /// "" for a separator; the "?" arm never fires for our items).
    fn top_shapes(items: &[ksni::MenuItem<RecordingTray>]) -> Vec<(String, bool)> {
        items
            .iter()
            .map(|i| match i {
                ksni::MenuItem::Standard(s) => (s.label.clone(), s.enabled),
                ksni::MenuItem::Checkmark(c) => (c.label.clone(), c.enabled),
                ksni::MenuItem::SubMenu(s) => (s.label.clone(), s.enabled),
                ksni::MenuItem::Separator => (String::new(), true),
                _ => ("?".to_string(), true),
            })
            .collect()
    }

    #[test]
    fn idle_menu_is_the_owners_shape_with_quit_enabled() {
        // DRAGON-574: while NOT recording, the session icon shows the owner's idle shape
        // (Scanner, the Capture / Record submenus, the Countdown Timer and Audio
        // Recording radio submenus, Settings..., Quit ENABLED — a child-owned idle icon
        // quits the capture session/app). The radio submenu titles come from the
        // app-reported state (mic-only arms + preset index 2 here).
        let t = tray(TrayState {
            recording: false,
            paused: false,
            mic: true,
            system_audio: false,
            delay_idx: 2,
        });
        let menu = t.menu();
        assert_eq!(
            top_shapes(&menu),
            vec![
                ("Color Picker".to_string(), true),
                ("Scanner".to_string(), true),
                ("Capture".to_string(), true),
                ("Record".to_string(), true),
                ("Countdown Timer: 05".to_string(), true),
                ("Audio Recording: Mic Only".to_string(), true),
                (String::new(), true), // separator
                ("Settings...".to_string(), true),
                ("Quit Cosmic Capture Kit".to_string(), true),
            ]
        );
        // The Capture / Record submenus hold the full-label trios, all enabled.
        // Indices 2 and 3: Color Picker leads the group since DRAGON-582.
        for (i, expected) in [
            (2usize, vec!["Capture Region", "Capture Window", "Capture Monitor"]),
            (3, vec!["Record Region", "Record Window", "Record Monitor"]),
        ] {
            let sub = match &menu[i] {
                ksni::MenuItem::SubMenu(s) => s,
                _ => panic!("item {i} is a submenu"),
            };
            let rows: Vec<(String, bool)> = top_shapes(&sub.submenu);
            assert_eq!(
                rows,
                expected.into_iter().map(|l| (l.to_string(), true)).collect::<Vec<_>>()
            );
        }
        // The audio entry is a real submenu holding the one radio group, current row
        // marked with the DRAGON-574 labels.
        let audio = match &menu[5] {
            ksni::MenuItem::SubMenu(s) => s,
            _ => panic!("Audio Recording is a submenu"),
        };
        let radio = match audio.submenu.as_slice() {
            [ksni::MenuItem::RadioGroup(r)] => r,
            _ => panic!("the audio submenu holds exactly one radio group"),
        };
        let row_labels: Vec<&str> = radio.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(row_labels, vec!["Mic + System", "Mic Only", "System Only", "None"]);
        assert_eq!(radio.selected, 1, "mic-only marks the second row");
        // The countdown entry holds the zero-padded presets with index 2 marked.
        let countdown = match &menu[4] {
            ksni::MenuItem::SubMenu(s) => s,
            _ => panic!("Countdown Timer is a submenu"),
        };
        let radio = match countdown.submenu.as_slice() {
            [ksni::MenuItem::RadioGroup(r)] => r,
            _ => panic!("the countdown submenu holds exactly one radio group"),
        };
        let row_labels: Vec<&str> = radio.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(row_labels, vec!["00", "03", "05", "10"]);
        assert_eq!(radio.selected, 2, "preset index 2 marks the third row");
    }

    #[test]
    fn recording_menu_leads_with_audio_then_the_controls() {
        // DRAGON-574 (superseding the DRAGON-558/559 recording shape) + the hide
        // round + the recolour-round amendment: the Audio Recording submenu (live
        // arms) leads the WHOLE menu, then the three controls and a separator, then
        // the same group with the flagged-off rows HIDDEN — no Record, no Countdown
        // Timer, no Quit — while Scanner, Capture and Settings remain. (The COSMIC
        // applet cannot gray or subdue a dbusmenu row, so the uniform answer
        // everywhere is omission; see `visible_tray_menu`.)
        let t = tray(TrayState {
            recording: true,
            paused: false,
            mic: true,
            system_audio: false,
            delay_idx: 1,
        });
        let menu = t.menu();
        assert_eq!(
            top_shapes(&menu),
            vec![
                ("Audio Recording: Mic Only".to_string(), true),
                ("Pause Recording".to_string(), true),
                ("Finish & Save Recording".to_string(), true),
                ("Cancel & Delete Recording".to_string(), true),
                (String::new(), true), // separator
                ("Color Picker".to_string(), true),
                ("Scanner".to_string(), true),
                ("Capture".to_string(), true),
                (String::new(), true), // separator
                ("Settings...".to_string(), true),
            ]
        );
        // Paused flips the first CONTROL (the row under the audio submenu) to Resume.
        let paused = tray(TrayState {
            recording: true,
            paused: true,
            mic: true,
            system_audio: false,
            delay_idx: 0,
        });
        assert_eq!(top_shapes(&paused.menu())[1].0, "Resume Recording");
    }

    /// An idle countdown pick updates the rendered mark and sends the pick to the app
    /// (which owns persist + the live overlay chip).
    #[test]
    fn an_idle_countdown_pick_marks_and_emits() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut t = RecordingTray {
            state: state(false, false, false, false),
            accent: [0x33, 0x99, 0xff],
            tx,
            icon_cache: std::cell::RefCell::new(None),
        };
        // Index 4: Color Picker, Scanner, Capture, Record, THEN Countdown Timer
        // (DRAGON-582 put the picker at the head of the launcher group).
        let countdown = match t.menu().swap_remove(4) {
            ksni::MenuItem::SubMenu(s) => s,
            _ => panic!("Countdown Timer is a submenu"),
        };
        if let [ksni::MenuItem::RadioGroup(r)] = &countdown.submenu[..] {
            (r.select)(&mut t, 3);
            // A stray index emits nothing.
            (r.select)(&mut t, 9);
        } else {
            panic!("the countdown submenu holds exactly one radio group");
        }
        assert_eq!(t.state.delay_idx, 3);
        let events: Vec<TrayEvent> = rx.try_iter().collect();
        assert_eq!(events, vec![TrayEvent::CountdownPick(3)]);
    }

    /// The menu icons ride ksni's `icon_data` as PNGs: the named rows carry one, the
    /// separator carries none, and the raster is cached per (icon, colour).
    #[test]
    fn menu_icons_render_as_png_data_on_the_named_rows() {
        let t = tray(state(false, false, false, false));
        let menu = t.menu();
        for (i, item) in menu.iter().enumerate() {
            match item {
                ksni::MenuItem::Standard(s) => {
                    assert!(
                        s.icon_data.starts_with(b"\x89PNG"),
                        "row {i} ({}) misses its PNG icon",
                        s.label
                    );
                }
                ksni::MenuItem::SubMenu(s) => {
                    assert!(
                        s.icon_data.starts_with(b"\x89PNG"),
                        "submenu {i} ({}) misses its PNG icon",
                        s.label
                    );
                    // Radio ROWS never carry icons (the toggle indicator owns that slot).
                    if let [ksni::MenuItem::RadioGroup(_)] = &s.submenu[..] {
                        // RadioItem has no icon field at all: structural guarantee.
                    }
                }
                ksni::MenuItem::Separator => {}
                _ => panic!("unexpected item kind at {i}"),
            }
        }
        // The cache answers the second render with the same bytes.
        let a = menu_icon_png(Some(MenuIcon::Scanner), [1, 2, 3]);
        let b = menu_icon_png(Some(MenuIcon::Scanner), [1, 2, 3]);
        assert!(!a.is_empty() && a == b);
        // No icon assigned = no bytes.
        assert!(menu_icon_png(None, [1, 2, 3]).is_empty());
    }

    #[test]
    fn id_is_stable_and_carries_the_dev_prefix() {
        let t = tray(state(true, false, false, false));
        assert_eq!(t.id(), "dev.thedragon.CosmicCaptureKit.Recording");
        assert_eq!(t.title(), "Cosmic Capture Kit recording");
    }
}

#[cfg(test)]
mod countdown_tray_tests {
    use super::*;
    use ksni::Tray as _;

    fn tray(remaining: u8) -> (CountdownTray, Receiver<TrayEvent>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (CountdownTray { remaining, tx, icon_cache: RefCell::new(None) }, rx)
    }

    /// The one menu entry cancels: the owner's label, and activating it sends
    /// [`TrayEvent::Cancel`] through the channel the app drains, the same event the
    /// recording tray's cancel sends, so the app-side routing has one vocabulary.
    #[test]
    fn the_cancel_entry_sends_cancel_and_nothing_else() {
        let (mut t, rx) = tray(5);
        let menu = t.menu();
        assert_eq!(menu.len(), 1, "one entry: Cancel countdown");
        let ksni::MenuItem::Standard(item) = &menu[0] else {
            panic!("the cancel entry must be a StandardItem");
        };
        assert_eq!(item.label, crate::recording_ui::COUNTDOWN_CANCEL_LABEL);
        (item.activate)(&mut t);
        let events: Vec<TrayEvent> = rx.try_iter().collect();
        assert_eq!(events, vec![TrayEvent::Cancel]);
    }

    /// The digits come in the upload counter's exact pixel sizes (one pixmap per
    /// advertised cell, DRAGON-539) and their ink is the RECORDING red: the same value
    /// the recording glyph uses, byte-for-byte, per the DRAGON-563 spec.
    #[test]
    fn the_digits_render_at_the_shared_sizes_in_the_recording_red() {
        let (t, _rx) = tray(7);
        let icons = t.icon_pixmap();
        let sizes: Vec<i32> = icons.iter().map(|i| i.width).collect();
        assert_eq!(
            sizes,
            crate::platform::linux::upload_tray::DIGIT_SIZES.map(|s| s as i32).to_vec(),
            "one pixmap per cell size"
        );
        let [r, g, b] = crate::recording_ui::RECORDING_RED_RGB;
        for icon in &icons {
            assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
            // ARGB32 (see `pack_argb`): a fully-opaque interior block pixel carries the
            // red exactly (no anti-aliasing on the integer pixel grid to soften it).
            let solid_red = icon
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[0] == 0xff && px[1] == r && px[2] == g && px[3] == b);
            assert!(solid_red, "{}px cell has no solid recording-red pixel", icon.width);
        }
    }

    /// The cache answers the second call with the same pixels, and a different remaining
    /// second is a different picture (the whole point of the item).
    #[test]
    fn the_icon_is_cached_per_second_and_changes_with_it() {
        let (t, _rx) = tray(9);
        let first = t.icon_pixmap();
        assert!(!first.is_empty(), "the countdown renders pixmaps");
        assert_eq!(t.icon_pixmap()[0].data, first[0].data, "the cache must not re-render");
        let (other, _rx2) = tray(8);
        assert_ne!(other.icon_pixmap()[0].data, first[0].data, "8 drew the same icon as 9");
    }

    /// The title and the tooltip come from the one shared builder and carry the
    /// remaining seconds; the id is stable and keeps the dev prefix.
    #[test]
    fn the_identity_and_tooltip_say_what_is_counting() {
        let (t, _rx) = tray(3);
        assert_eq!(t.id(), "dev.thedragon.CosmicCaptureKit.Countdown");
        assert_eq!(t.title(), crate::recording_ui::countdown_tooltip(3));
        assert_eq!(t.tool_tip().title, t.title());
        assert!(t.title().contains("3s"));
    }
}
