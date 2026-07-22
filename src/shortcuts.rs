//! Unified, app-owned keyboard-shortcut model.
//!
//! Every user-triggerable [`Action`] maps to a [`Shortcut`] (modifiers + key),
//! matched against `iced` keyboard events in one place (see `app::handle_key`)
//! rather than scattered `if key == …` branches. The types are plain serializable
//! data over `iced`'s keyboard types — no dependency on the COSMIC desktop, its
//! shortcut config, or even libcosmic's menu module — so remaps persist and the
//! whole layer carries to any platform `iced` runs on (a future macOS/Windows port
//! included). Modifier semantics are already cross-platform: `logo` is ⌘ on macOS,
//! the Super/Win key elsewhere.
//!
//! International keyboards: matching is on the logical key for now; the physical-key
//! fallback for non-Latin layouts (as libcosmic's `KeyBind` does) is a future add.

use cosmic::iced::keyboard::{key::Named, Key, Modifiers};
use serde::{Deserialize, Serialize};

/// Which surface an action belongs to. Actions in different contexts may share a
/// binding (e.g. Esc cancels both the capture overlay and the preview), since only one
/// context is ever active at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    /// The capture overlay (region/window/monitor select, OCR, settings search).
    Overlay,
    /// A drawn region selection on the capture overlay — the quick-action lane
    /// (copy the selection and finish). A SEPARATE context from [`Self::Overlay`]
    /// so a chord like primary+C can mean "copy the drawn region" here AND "copy
    /// recognized text" in [`Self::Overlay`] without the two fighting over the bind
    /// (the dispatcher picks the region lane first when a selection is drawn — see
    /// `app::handle_key`). Same pattern as Esc, which is one action shared across
    /// [`Self::Overlay`] and [`Self::Preview`].
    Region,
    /// The post-capture preview overlay.
    Preview,
    /// An in-progress recording (stop / toggle mic / toggle system audio).
    Recording,
}

/// A user action that can be triggered by a keyboard shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Action {
    /// Copy the selected recognized text (default Ctrl+C).
    CopyText,
    /// Select all recognized text in the region (default Ctrl+A).
    SelectAllText,
    /// Clear the text selection (default Ctrl+Shift+A).
    DeselectText,
    /// Region select: copy the drawn selection to the clipboard and finish
    /// (default Ctrl+C on Linux, Cmd+C on macOS).
    RegionCopy,
    /// Preview: keep the capture where it was saved (default Ctrl+S).
    PreviewSave,
    /// Preview: save the capture to a chosen location (default Ctrl+Shift+S).
    PreviewSaveAs,
    /// Preview: copy the capture to the clipboard (default Ctrl+C).
    PreviewCopy,
    /// Preview: close without deleting (default Esc).
    PreviewCancel,
    /// Preview: delete the file and close (default Ctrl+D).
    PreviewDelete,
    /// Preview: open/close the covermark picker (default W).
    PreviewCovermark,
    /// Preview: undo the last edit (default Ctrl+Z).
    PreviewUndo,
    /// Preview: redo the last undone edit (default Ctrl+Shift+Z).
    PreviewRedo,
    /// Preview: play/pause a recording inline (default P).
    PreviewPlay,
    /// Preview: step to the previous frame (default ,).
    PreviewFramePrev,
    /// Preview: step to the next frame (default .).
    PreviewFrameNext,
    /// Preview: delete the selected timeline segment (default Delete).
    PreviewDeleteSegment,
    /// Recording: stop + save the in-progress recording (default Enter).
    RecordStop,
    /// Recording: toggle the microphone channel (default M).
    RecordToggleMic,
    /// Recording: toggle the system-audio channel (default S).
    RecordToggleSystemAudio,
}

impl Action {
    /// Every action, in display + match order. Preview actions are grouped so the
    /// settings page's contiguous-group builder yields "Action Shortcuts" (save /
    /// copy / close / delete / covermark / undo / redo) then "Video Editor
    /// Shortcuts" (play / frame step / delete segment).
    pub const ALL: [Action; 19] = [
        Action::SelectAllText,
        Action::DeselectText,
        Action::CopyText,
        Action::RegionCopy,
        Action::PreviewSave,
        Action::PreviewSaveAs,
        Action::PreviewCopy,
        Action::PreviewCancel,
        Action::PreviewDelete,
        Action::PreviewCovermark,
        Action::PreviewUndo,
        Action::PreviewRedo,
        Action::PreviewPlay,
        Action::PreviewFramePrev,
        Action::PreviewFrameNext,
        Action::PreviewDeleteSegment,
        Action::RecordStop,
        Action::RecordToggleMic,
        Action::RecordToggleSystemAudio,
    ];

    /// Short title for the settings row.
    pub fn label(self) -> &'static str {
        match self {
            Action::CopyText => "Copy selected text",
            Action::SelectAllText => "Select all text",
            Action::DeselectText => "Deselect all text",
            Action::RegionCopy => "Copy selection",
            Action::PreviewSave => "Save",
            Action::PreviewSaveAs => "Save As",
            Action::PreviewCopy => "Copy to clipboard",
            Action::PreviewPlay => "Play",
            Action::PreviewFramePrev => "Previous frame",
            Action::PreviewFrameNext => "Next frame",
            Action::PreviewDelete => "Delete",
            Action::PreviewCancel => "Close",
            Action::PreviewCovermark => "Covermark",
            Action::PreviewUndo => "Undo",
            Action::PreviewRedo => "Redo",
            Action::PreviewDeleteSegment => "Delete segment",
            Action::RecordStop => "Stop and save recording",
            Action::RecordToggleMic => "Toggle Microphone",
            Action::RecordToggleSystemAudio => "Toggle system audio",
        }
    }

    /// Helper line for the settings row.
    pub fn description(self) -> &'static str {
        match self {
            // OCR group: descriptions removed per DRAGON-158.
            Action::CopyText => "",
            Action::SelectAllText => "",
            Action::DeselectText => "",
            Action::RegionCopy => {
                "Immediately copies the selected region to the clipboard and exits."
            }
            // Action Shortcuts group: descriptions removed per DRAGON-158.
            Action::PreviewSave => "",
            Action::PreviewSaveAs => "",
            Action::PreviewCopy => "",
            Action::PreviewDelete => "",
            Action::PreviewCancel => "",
            Action::PreviewCovermark => "",
            Action::PreviewUndo => "",
            Action::PreviewRedo => "",
            Action::PreviewPlay => "Play or pause a recording in the preview.",
            Action::PreviewFramePrev => "Step to the previous frame.",
            Action::PreviewFrameNext => "Step to the next frame.",
            Action::PreviewDeleteSegment => "Delete the selected timeline segment.",
            Action::RecordStop => "",
            Action::RecordToggleMic => "",
            Action::RecordToggleSystemAudio => "",
        }
    }

    /// The settings-page group this action is shown under (actions of the same group
    /// are contiguous in [`Action::ALL`], so the page can build one section per group).
    pub fn group(self) -> &'static str {
        match self {
            Action::CopyText | Action::SelectAllText | Action::DeselectText => {
                "OCR Text Recognition"
            }
            Action::RegionCopy => "Region Selection",
            Action::PreviewSave
            | Action::PreviewSaveAs
            | Action::PreviewCopy
            | Action::PreviewCancel
            | Action::PreviewDelete
            | Action::PreviewCovermark
            | Action::PreviewUndo
            | Action::PreviewRedo => "Action Shortcuts",
            Action::PreviewPlay
            | Action::PreviewFramePrev
            | Action::PreviewFrameNext
            | Action::PreviewDeleteSegment => "Video Editor Shortcuts",
            Action::RecordStop
            | Action::RecordToggleMic
            | Action::RecordToggleSystemAudio => "Recording",
        }
    }

    /// Which surface this action belongs to.
    pub fn context(self) -> Context {
        match self {
            Action::CopyText
            | Action::SelectAllText
            | Action::DeselectText => Context::Overlay,
            Action::RegionCopy => Context::Region,
            Action::PreviewSave
            | Action::PreviewSaveAs
            | Action::PreviewCopy
            | Action::PreviewPlay
            | Action::PreviewFramePrev
            | Action::PreviewFrameNext
            | Action::PreviewDelete
            | Action::PreviewCancel
            | Action::PreviewCovermark
            | Action::PreviewUndo
            | Action::PreviewRedo
            | Action::PreviewDeleteSegment => Context::Preview,
            Action::RecordStop
            | Action::RecordToggleMic
            | Action::RecordToggleSystemAudio => Context::Recording,
        }
    }

    /// The factory-default binding.
    pub fn default_shortcut(self) -> Shortcut {
        match self {
            Action::CopyText => Shortcut::primary_char('c'),
            Action::SelectAllText => Shortcut::primary_char('a'),
            Action::DeselectText => Shortcut::primary_shift_char('a'),
            Action::RegionCopy => Shortcut::primary_char('c'),
            Action::PreviewSave => Shortcut::primary_char('s'),
            Action::PreviewSaveAs => Shortcut::primary_shift_char('s'),
            Action::PreviewCopy => Shortcut::primary_char('c'),
            Action::PreviewPlay => Shortcut::char('p'),
            Action::PreviewFramePrev => Shortcut::char(','),
            Action::PreviewFrameNext => Shortcut::char('.'),
            Action::PreviewDelete => Shortcut::primary_char('d'),
            Action::PreviewCancel => Shortcut::named(NamedKey::Escape),
            Action::PreviewCovermark => Shortcut::char('w'),
            Action::PreviewUndo => Shortcut::primary_char('z'),
            Action::PreviewRedo => Shortcut::primary_shift_char('z'),
            Action::PreviewDeleteSegment => Shortcut::named(NamedKey::Delete),
            Action::RecordStop => Shortcut::named(NamedKey::Enter),
            Action::RecordToggleMic => Shortcut::char('m'),
            Action::RecordToggleSystemAudio => Shortcut::char('s'),
        }
    }
}

/// A non-character key we allow binding — a serializable mirror of the `iced`
/// `Named` keys we support (kept small and explicit so the on-disk form is stable).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NamedKey {
    Escape,
    Enter,
    Tab,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    // Extended function keys + PrintScreen (DRAGON-130): needed so the macOS
    // "Start Capture" GLOBAL hotkey can be RECORDED from a keypress (the default
    // is PrintScreen, which a PC keyboard surfaces as F13 on macOS). The in-app
    // keymap never binds these by default, but a captured chord may land on any
    // of them, and each serializes to a spec token the daemon parser accepts.
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
    PrintScreen,
}

impl NamedKey {
    /// The libxkbcommon keysym name for this key, as the XDG "shortcuts" spec
    /// (portal GlobalShortcuts trigger strings) expects.
    fn xdg_name(self) -> &'static str {
        match self {
            NamedKey::Escape => "Escape",
            NamedKey::Enter => "Return",
            NamedKey::Tab => "Tab",
            NamedKey::Backspace => "BackSpace",
            NamedKey::Delete => "Delete",
            NamedKey::Insert => "Insert",
            NamedKey::Home => "Home",
            NamedKey::End => "End",
            NamedKey::PageUp => "Page_Up",
            NamedKey::PageDown => "Page_Down",
            NamedKey::Up => "Up",
            NamedKey::Down => "Down",
            NamedKey::Left => "Left",
            NamedKey::Right => "Right",
            NamedKey::F1 => "F1",
            NamedKey::F2 => "F2",
            NamedKey::F3 => "F3",
            NamedKey::F4 => "F4",
            NamedKey::F5 => "F5",
            NamedKey::F6 => "F6",
            NamedKey::F7 => "F7",
            NamedKey::F8 => "F8",
            NamedKey::F9 => "F9",
            NamedKey::F10 => "F10",
            NamedKey::F11 => "F11",
            NamedKey::F12 => "F12",
            NamedKey::F13 => "F13",
            NamedKey::F14 => "F14",
            NamedKey::F15 => "F15",
            NamedKey::F16 => "F16",
            NamedKey::F17 => "F17",
            NamedKey::F18 => "F18",
            NamedKey::F19 => "F19",
            NamedKey::F20 => "F20",
            NamedKey::F21 => "F21",
            NamedKey::F22 => "F22",
            NamedKey::F23 => "F23",
            NamedKey::F24 => "F24",
            NamedKey::PrintScreen => "Print",
        }
    }

    /// The token this key serializes to in the macOS daemon's "Start Capture"
    /// hotkey SPEC (the `global-hotkey` parser's vocabulary — see
    /// [`crate::daemon`]). Distinct from [`Self::xdg_name`] (libxkbcommon keysyms):
    /// e.g. Enter is `"Enter"` here but `"Return"` there, PrintScreen is
    /// `"PrintScreen"` here but `"Print"` there. Used on macOS + Windows (the daemon-hotkey
    /// OSes), but kept here beside the other names so the key table stays in one place.
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    fn daemon_name(self) -> &'static str {
        match self {
            NamedKey::Escape => "Escape",
            NamedKey::Enter => "Enter",
            NamedKey::Tab => "Tab",
            NamedKey::Backspace => "Backspace",
            NamedKey::Delete => "Delete",
            NamedKey::Insert => "Insert",
            NamedKey::Home => "Home",
            NamedKey::End => "End",
            NamedKey::PageUp => "PageUp",
            NamedKey::PageDown => "PageDown",
            NamedKey::Up => "ArrowUp",
            NamedKey::Down => "ArrowDown",
            NamedKey::Left => "ArrowLeft",
            NamedKey::Right => "ArrowRight",
            NamedKey::F1 => "F1",
            NamedKey::F2 => "F2",
            NamedKey::F3 => "F3",
            NamedKey::F4 => "F4",
            NamedKey::F5 => "F5",
            NamedKey::F6 => "F6",
            NamedKey::F7 => "F7",
            NamedKey::F8 => "F8",
            NamedKey::F9 => "F9",
            NamedKey::F10 => "F10",
            NamedKey::F11 => "F11",
            NamedKey::F12 => "F12",
            NamedKey::F13 => "F13",
            NamedKey::F14 => "F14",
            NamedKey::F15 => "F15",
            NamedKey::F16 => "F16",
            NamedKey::F17 => "F17",
            NamedKey::F18 => "F18",
            NamedKey::F19 => "F19",
            NamedKey::F20 => "F20",
            NamedKey::F21 => "F21",
            NamedKey::F22 => "F22",
            NamedKey::F23 => "F23",
            NamedKey::F24 => "F24",
            NamedKey::PrintScreen => "PrintScreen",
        }
    }

    /// Whether this is a function key — a reasonable GLOBAL hotkey even without
    /// modifiers (unlike, say, a bare letter or Enter). PrintScreen counts too
    /// (it is the macOS "Start Capture" default).
    fn is_function_key(self) -> bool {
        matches!(
            self,
            NamedKey::F1
                | NamedKey::F2
                | NamedKey::F3
                | NamedKey::F4
                | NamedKey::F5
                | NamedKey::F6
                | NamedKey::F7
                | NamedKey::F8
                | NamedKey::F9
                | NamedKey::F10
                | NamedKey::F11
                | NamedKey::F12
                | NamedKey::F13
                | NamedKey::F14
                | NamedKey::F15
                | NamedKey::F16
                | NamedKey::F17
                | NamedKey::F18
                | NamedKey::F19
                | NamedKey::F20
                | NamedKey::F21
                | NamedKey::F22
                | NamedKey::F23
                | NamedKey::F24
                | NamedKey::PrintScreen
        )
    }

    /// Map an `iced` named key to a bindable one (None for keys we don't support,
    /// including the bare modifier keys).
    fn from_iced(named: &Named) -> Option<NamedKey> {
        Some(match named {
            Named::Escape => NamedKey::Escape,
            Named::Enter => NamedKey::Enter,
            Named::Tab => NamedKey::Tab,
            Named::Backspace => NamedKey::Backspace,
            Named::Delete => NamedKey::Delete,
            Named::Insert => NamedKey::Insert,
            Named::Home => NamedKey::Home,
            Named::End => NamedKey::End,
            Named::PageUp => NamedKey::PageUp,
            Named::PageDown => NamedKey::PageDown,
            Named::ArrowUp => NamedKey::Up,
            Named::ArrowDown => NamedKey::Down,
            Named::ArrowLeft => NamedKey::Left,
            Named::ArrowRight => NamedKey::Right,
            Named::F1 => NamedKey::F1,
            Named::F2 => NamedKey::F2,
            Named::F3 => NamedKey::F3,
            Named::F4 => NamedKey::F4,
            Named::F5 => NamedKey::F5,
            Named::F6 => NamedKey::F6,
            Named::F7 => NamedKey::F7,
            Named::F8 => NamedKey::F8,
            Named::F9 => NamedKey::F9,
            Named::F10 => NamedKey::F10,
            Named::F11 => NamedKey::F11,
            Named::F12 => NamedKey::F12,
            Named::F13 => NamedKey::F13,
            Named::F14 => NamedKey::F14,
            Named::F15 => NamedKey::F15,
            Named::F16 => NamedKey::F16,
            Named::F17 => NamedKey::F17,
            Named::F18 => NamedKey::F18,
            Named::F19 => NamedKey::F19,
            Named::F20 => NamedKey::F20,
            Named::F21 => NamedKey::F21,
            Named::F22 => NamedKey::F22,
            Named::F23 => NamedKey::F23,
            Named::F24 => NamedKey::F24,
            Named::PrintScreen => NamedKey::PrintScreen,
            _ => return None,
        })
    }

    /// The `iced` named key this binds to.
    fn to_iced(self) -> Named {
        match self {
            NamedKey::Escape => Named::Escape,
            NamedKey::Enter => Named::Enter,
            NamedKey::Tab => Named::Tab,
            NamedKey::Backspace => Named::Backspace,
            NamedKey::Delete => Named::Delete,
            NamedKey::Insert => Named::Insert,
            NamedKey::Home => Named::Home,
            NamedKey::End => Named::End,
            NamedKey::PageUp => Named::PageUp,
            NamedKey::PageDown => Named::PageDown,
            NamedKey::Up => Named::ArrowUp,
            NamedKey::Down => Named::ArrowDown,
            NamedKey::Left => Named::ArrowLeft,
            NamedKey::Right => Named::ArrowRight,
            NamedKey::F1 => Named::F1,
            NamedKey::F2 => Named::F2,
            NamedKey::F3 => Named::F3,
            NamedKey::F4 => Named::F4,
            NamedKey::F5 => Named::F5,
            NamedKey::F6 => Named::F6,
            NamedKey::F7 => Named::F7,
            NamedKey::F8 => Named::F8,
            NamedKey::F9 => Named::F9,
            NamedKey::F10 => Named::F10,
            NamedKey::F11 => Named::F11,
            NamedKey::F12 => Named::F12,
            NamedKey::F13 => Named::F13,
            NamedKey::F14 => Named::F14,
            NamedKey::F15 => Named::F15,
            NamedKey::F16 => Named::F16,
            NamedKey::F17 => Named::F17,
            NamedKey::F18 => Named::F18,
            NamedKey::F19 => Named::F19,
            NamedKey::F20 => Named::F20,
            NamedKey::F21 => Named::F21,
            NamedKey::F22 => Named::F22,
            NamedKey::F23 => Named::F23,
            NamedKey::F24 => Named::F24,
            NamedKey::PrintScreen => Named::PrintScreen,
        }
    }

    /// Display label.
    fn label(self) -> &'static str {
        match self {
            NamedKey::Escape => "Esc",
            NamedKey::Enter => "Enter",
            NamedKey::Tab => "Tab",
            NamedKey::Backspace => "Backspace",
            NamedKey::Delete => "Delete",
            NamedKey::Insert => "Insert",
            NamedKey::Home => "Home",
            NamedKey::End => "End",
            NamedKey::PageUp => "Page Up",
            NamedKey::PageDown => "Page Down",
            NamedKey::Up => "↑",
            NamedKey::Down => "↓",
            NamedKey::Left => "←",
            NamedKey::Right => "→",
            NamedKey::F1 => "F1",
            NamedKey::F2 => "F2",
            NamedKey::F3 => "F3",
            NamedKey::F4 => "F4",
            NamedKey::F5 => "F5",
            NamedKey::F6 => "F6",
            NamedKey::F7 => "F7",
            NamedKey::F8 => "F8",
            NamedKey::F9 => "F9",
            NamedKey::F10 => "F10",
            NamedKey::F11 => "F11",
            NamedKey::F12 => "F12",
            NamedKey::F13 => "F13",
            NamedKey::F14 => "F14",
            NamedKey::F15 => "F15",
            NamedKey::F16 => "F16",
            NamedKey::F17 => "F17",
            NamedKey::F18 => "F18",
            NamedKey::F19 => "F19",
            NamedKey::F20 => "F20",
            NamedKey::F21 => "F21",
            NamedKey::F22 => "F22",
            NamedKey::F23 => "F23",
            NamedKey::F24 => "F24",
            NamedKey::PrintScreen => "PrintScreen",
        }
    }
}

/// The key portion of a shortcut.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShortcutKey {
    /// A character key, stored lowercased (e.g. "c"). Matched case-insensitively.
    Char(String),
    /// A named non-character key.
    Named(NamedKey),
}

/// A keyboard shortcut: a set of modifiers plus a key. Plain serializable data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shortcut {
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
    /// The Super/Win key on Linux/Windows, ⌘ on macOS.
    #[serde(default)]
    pub logo: bool,
    pub key: ShortcutKey,
}

impl Shortcut {
    fn char(c: char) -> Self {
        Shortcut {
            ctrl: false,
            alt: false,
            shift: false,
            logo: false,
            key: ShortcutKey::Char(c.to_ascii_lowercase().to_string()),
        }
    }

    /// A Ctrl+`c` binding. Used by tests to build platform-neutral overrides (a
    /// literal Ctrl chord, e.g. a Linux config carried to a Mac).
    #[cfg(test)]
    fn ctrl_char(c: char) -> Self {
        Shortcut {
            ctrl: true,
            ..Shortcut::char(c)
        }
    }

    /// A default binding on the platform's PRIMARY command modifier: Ctrl on
    /// Linux/Windows, Cmd (⌘, the logo/Super key) on macOS. The in-app editor
    /// bindings (copy/save/undo/…) default here so a Mac user gets the native
    /// Cmd chords they expect, while Linux keeps the exact Ctrl defaults it always
    /// had. Only DEFAULTS route through this; a persisted user override carries its
    /// own literal modifiers and is untouched by the platform choice.
    fn primary_char(c: char) -> Self {
        #[cfg(target_os = "macos")]
        {
            Shortcut {
                logo: true,
                ..Shortcut::char(c)
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Shortcut {
                ctrl: true,
                ..Shortcut::char(c)
            }
        }
    }

    /// A default binding on the primary command modifier PLUS Shift (e.g. the
    /// "Save As" / "Redo" chords): Ctrl+Shift on Linux, Cmd+Shift on macOS.
    fn primary_shift_char(c: char) -> Self {
        Shortcut {
            shift: true,
            ..Shortcut::primary_char(c)
        }
    }

    fn named(k: NamedKey) -> Self {
        Shortcut {
            ctrl: false,
            alt: false,
            shift: false,
            logo: false,
            key: ShortcutKey::Named(k),
        }
    }

    /// Whether a keypress (modifiers + logical key) triggers this shortcut. Modifiers
    /// must match exactly, so Ctrl+A and Ctrl+Shift+A are distinct.
    pub fn matches(&self, modifiers: Modifiers, key: &Key) -> bool {
        self.ctrl == modifiers.control()
            && self.alt == modifiers.alt()
            && self.shift == modifiers.shift()
            && self.logo == modifiers.logo()
            && self.key_matches(key)
    }

    fn key_matches(&self, key: &Key) -> bool {
        match (&self.key, key) {
            (ShortcutKey::Char(c), Key::Character(k)) => c.eq_ignore_ascii_case(k),
            (ShortcutKey::Named(n), Key::Named(k)) => NamedKey::from_iced(k) == Some(*n),
            _ => false,
        }
    }

    /// Build a shortcut from a keypress, or `None` if the key isn't bindable (a bare
    /// modifier, or a named key we don't support) — used when capturing a rebind.
    pub fn from_event(modifiers: Modifiers, key: &Key) -> Option<Shortcut> {
        let sk = match key {
            Key::Character(c) => {
                let c = c.trim();
                if c.is_empty() {
                    return None;
                }
                ShortcutKey::Char(c.to_ascii_lowercase())
            }
            Key::Named(named) => ShortcutKey::Named(NamedKey::from_iced(named)?),
            _ => return None,
        };
        Some(Shortcut {
            ctrl: modifiers.control(),
            alt: modifiers.alt(),
            shift: modifiers.shift(),
            logo: modifiers.logo(),
            key: sk,
        })
    }

    /// The XDG "shortcuts" spec trigger string for this binding (e.g.
    /// `CTRL+SHIFT+F9`) — the PREFERRED-trigger hint when registering portal
    /// global shortcuts. `None` when the combination makes a poor global hotkey
    /// (an unmodified letter/Enter would shadow normal typing; desktops reject
    /// or remap those anyway) — the portal then lets the user pick a trigger in
    /// the desktop's own UI.
    pub fn xdg_trigger(&self) -> Option<String> {
        let global_ok = self.ctrl
            || self.alt
            || self.logo
            || matches!(&self.key, ShortcutKey::Named(n) if n.is_function_key());
        if !global_ok {
            return None;
        }
        let mut s = String::new();
        if self.logo {
            s.push_str("LOGO+");
        }
        if self.ctrl {
            s.push_str("CTRL+");
        }
        if self.alt {
            s.push_str("ALT+");
        }
        if self.shift {
            s.push_str("SHIFT+");
        }
        match &self.key {
            ShortcutKey::Char(c) => s.push_str(c),
            ShortcutKey::Named(n) => s.push_str(n.xdg_name()),
        }
        Some(s)
    }

    /// Serialize this chord to the macOS "Start Capture" hotkey SPEC string the
    /// resident daemon's parser accepts (see [`crate::daemon`]) — e.g. `"PrintScreen"`,
    /// `"F13"`, `"Cmd+Shift+2"`. Modifiers render as `Cmd`/`Ctrl`/`Alt`/`Shift`
    /// (logo → Cmd), the key as its `global-hotkey` token (a letter/digit/symbol
    /// uppercased, a named key via [`NamedKey::daemon_name`]).
    ///
    /// This is the inverse of the daemon's `hotkey_spec::parse`, so
    /// `parse(chord.daemon_spec())` round-trips (a bare PrintScreen additionally
    /// yields the F13 alias inside the macOS `parse`, which is expected and harmless; the
    /// Windows parser maps PrintScreen straight to `VK_SNAPSHOT`). Every chord `from_event`
    /// can produce is serializable, so this never fails; it returns `String` rather than
    /// `Option` for that reason. Built on macOS + Windows (the two daemon-hotkey OSes), but
    /// kept here beside `label`/`xdg_trigger` so all the spellings live together.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn daemon_spec(&self) -> String {
        let mut s = String::new();
        if self.logo {
            s.push_str("Cmd+");
        }
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        match &self.key {
            // Uppercase so a letter matches the parser's "KEYA"|"A" arm and a symbol
            // (already single-char) matches its literal arm; digits are unchanged.
            ShortcutKey::Char(c) => s.push_str(&c.to_uppercase()),
            ShortcutKey::Named(n) => s.push_str(n.daemon_name()),
        }
        s
    }

    /// Human-readable label, e.g. "Ctrl+C" (or "⌃C" on macOS).
    pub fn label(&self) -> String {
        let key = match &self.key {
            ShortcutKey::Char(c) => c.to_uppercase(),
            ShortcutKey::Named(n) => n.label().to_string(),
        };
        #[cfg(target_os = "macos")]
        {
            let mut s = String::new();
            if self.ctrl {
                s.push('⌃');
            }
            if self.alt {
                s.push('⌥');
            }
            if self.shift {
                s.push('⇧');
            }
            if self.logo {
                s.push('⌘');
            }
            s.push_str(&key);
            s
        }
        #[cfg(not(target_os = "macos"))]
        {
            let mut parts: Vec<&str> = Vec::new();
            if self.ctrl {
                parts.push("Ctrl");
            }
            if self.alt {
                parts.push("Alt");
            }
            if self.shift {
                parts.push("Shift");
            }
            if self.logo {
                parts.push("Super");
            }
            let key_owned = key;
            parts.push(&key_owned);
            parts.join("+")
        }
    }

    /// The `iced` key this binds to (used by tests / potential reverse mapping).
    #[allow(dead_code)]
    pub fn iced_key(&self) -> Key {
        match &self.key {
            ShortcutKey::Char(c) => Key::Character(c.as_str().into()),
            ShortcutKey::Named(n) => Key::Named(n.to_iced()),
        }
    }
}

/// macOS (DRAGON-294): render a daemon "Start Capture" hotkey SPEC string (the form
/// `Shortcut::daemon_spec` produces / the daemon parses — e.g. `"Cmd+Shift+2"`,
/// `"Ctrl+Alt+K"`, `"PrintScreen"`) into the native macOS modifier SYMBOLS: Control ⌃,
/// Option/Alt ⌥, Shift ⇧, Command ⌘, keeping the platform ORDER ⌃⌥⇧⌘ regardless of how
/// the spec was written. So `Ctrl+Alt+Shift+1` → `⌃⌥⇧1` and `Cmd+Shift+2` → `⌘⇧2`. The
/// key token is passed through mostly verbatim (uppercased for a bare letter), with a few
/// named keys mapped to their glyphs to match `Shortcut::label`. This is the ONE mac
/// formatter every keybind DISPLAY routes through on macOS; Windows/Linux keep the plain
/// spec text (this is never called there). Pure, so it unit-tests without any AppKit.
///
/// An empty spec (the cleared / unset state) yields an empty string — the caller decides
/// what to show for "no binding" (e.g. "Unbound"). Unknown tokens pass through unchanged,
/// so a spec this can't fully classify still renders legibly rather than being dropped.
#[cfg(target_os = "macos")]
pub fn mac_symbolic_spec(spec: &str) -> String {
    let spec = spec.trim();
    if spec.is_empty() {
        return String::new();
    }
    // Split into `+`-joined tokens, classify each as a modifier or the key. Collect the
    // modifiers as a set so they can be re-emitted in the canonical ⌃⌥⇧⌘ order (the spec
    // may list them in any order), and keep the non-modifier tokens as the key text.
    let (mut ctrl, mut alt, mut shift, mut logo) = (false, false, false, false);
    let mut key_parts: Vec<String> = Vec::new();
    for tok in spec.split('+') {
        let t = tok.trim();
        if t.is_empty() {
            // A literal "+" key (a spec like "Cmd++"): an empty split segment between two
            // separators. Render it as the plus sign rather than dropping it.
            key_parts.push("+".to_string());
            continue;
        }
        match t.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" | "option" | "opt" => alt = true,
            "shift" => shift = true,
            "cmd" | "command" | "super" | "logo" | "win" => logo = true,
            _ => key_parts.push(mac_key_glyph(t)),
        }
    }
    let mut out = String::new();
    if ctrl {
        out.push('⌃');
    }
    if alt {
        out.push('⌥');
    }
    if shift {
        out.push('⇧');
    }
    if logo {
        out.push('⌘');
    }
    out.push_str(&key_parts.join(""));
    out
}

/// Windows (DRAGON-294): render a daemon capture-hotkey SPEC string (the form
/// `Shortcut::daemon_spec` produces — e.g. "Cmd+Shift+2", "Ctrl+Alt+K", "PrintScreen") with
/// Windows-native modifier NAMES. The one that matters: the logo/command token — which
/// `daemon_spec` always serializes as "Cmd" (logo → Cmd, shared with macOS) — reads as "Win"
/// on Windows, where users never call it "Cmd". Ctrl/Alt/Shift keep their text and the key
/// token passes through verbatim, so "Cmd+Shift+2" shows as "Win+Shift+2" and the common
/// "Ctrl+Alt+K" is unchanged. Pure, so it unit-tests without any Win32. macOS uses
/// `mac_symbolic_spec` instead; Linux never shows daemon hotkeys (COSMIC custom shortcut).
#[cfg(windows)]
pub fn win_readable_spec(spec: &str) -> String {
    let spec = spec.trim();
    if spec.is_empty() {
        return String::new();
    }
    spec.split('+')
        .map(|tok| match tok.trim().to_ascii_lowercase().as_str() {
            "cmd" | "command" | "super" | "logo" | "win" | "meta" => "Win",
            "ctrl" | "control" => "Ctrl",
            "alt" | "option" | "opt" => "Alt",
            "shift" => "Shift",
            // The key token (or an unknown / empty "+"-key segment): pass through verbatim.
            _ => tok,
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// The macOS display glyph/text for a spec KEY token (not a modifier). Arrow names map to
/// arrow glyphs (matching `NamedKey::label`); a bare single letter is uppercased; every
/// other token (digits, symbols, F-keys, PrintScreen, Enter, …) passes through verbatim.
#[cfg(target_os = "macos")]
fn mac_key_glyph(tok: &str) -> String {
    match tok.to_ascii_lowercase().as_str() {
        "arrowup" | "up" => "↑".to_string(),
        "arrowdown" | "down" => "↓".to_string(),
        "arrowleft" | "left" => "←".to_string(),
        "arrowright" | "right" => "→".to_string(),
        _ => {
            let mut chars = tok.chars();
            // A single character (letter/symbol): uppercase it, matching `Shortcut::label`.
            if let (Some(c), None) = (chars.next(), chars.clone().next()) {
                return c.to_uppercase().to_string();
            }
            tok.to_string()
        }
    }
}

/// The live `Action → Shortcut` table. The single source of truth for matching key
/// events and for the settings UI; persisted as a list of overrides from default.
#[derive(Clone, Debug)]
pub struct Keymap {
    /// Per-action binding: `Some` = bound, `None` = explicitly unbound.
    binds: std::collections::HashMap<Action, Option<Shortcut>>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::defaults()
    }
}

impl Keymap {
    /// All actions at their factory-default bindings.
    pub fn defaults() -> Self {
        Keymap {
            binds: Action::ALL
                .into_iter()
                .map(|a| (a, Some(a.default_shortcut())))
                .collect(),
        }
    }

    /// The binding for an action: `Some(shortcut)` if bound, `None` if unbound.
    pub fn get(&self, action: Action) -> Option<Shortcut> {
        match self.binds.get(&action) {
            Some(binding) => binding.clone(),
            None => Some(action.default_shortcut()),
        }
    }

    /// Whether an action is at its default binding.
    pub fn is_default(&self, action: Action) -> bool {
        self.get(action) == Some(action.default_shortcut())
    }

    /// The action in `context` currently bound to `shortcut`, if any.
    fn action_with_bind(&self, context: Context, shortcut: &Shortcut) -> Option<Action> {
        Action::ALL
            .into_iter()
            .filter(|a| a.context() == context)
            .find(|a| self.get(*a).as_ref() == Some(shortcut))
    }

    /// Bind `action` to `shortcut`. If another action in the same context already holds
    /// that bind it loses it (becomes unbound), so no two actions on one surface share a
    /// key — but the same key can still serve a different surface.
    pub fn set(&mut self, action: Action, shortcut: Shortcut) {
        if let Some(other) = self
            .action_with_bind(action.context(), &shortcut)
            .filter(|&o| o != action)
        {
            self.binds.insert(other, None);
        }
        self.binds.insert(action, Some(shortcut));
    }

    /// Remove an action's binding — it can't be triggered until rebound.
    pub fn unbind(&mut self, action: Action) {
        self.binds.insert(action, None);
    }

    /// The first action in `context` whose binding matches this keypress.
    pub fn action_for(&self, context: Context, modifiers: Modifiers, key: &Key) -> Option<Action> {
        Action::ALL
            .into_iter()
            .filter(|a| a.context() == context)
            .find(|a| self.get(*a).is_some_and(|sc| sc.matches(modifiers, key)))
    }

    /// Bindings that differ from default — the compact form persisted to disk
    /// (`None` records an explicit unbind).
    pub fn overrides(&self) -> Vec<(Action, Option<Shortcut>)> {
        Action::ALL
            .into_iter()
            .filter(|a| !self.is_default(*a))
            .map(|a| (a, self.get(a)))
            .collect()
    }

    /// Apply persisted overrides onto the defaults.
    pub fn apply_overrides(&mut self, overrides: &[(Action, Option<Shortcut>)]) {
        for (action, shortcut) in overrides {
            self.binds.insert(*action, shortcut.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::keyboard::{key::Named, Key, Modifiers};

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }

    /// The platform PRIMARY command modifier the editor chords default to: Ctrl on
    /// Linux/Windows, Cmd (logo) on macOS. Tests match against it so they assert the
    /// default chord fires on either platform rather than a hardcoded modifier.
    #[cfg(target_os = "macos")]
    const PRIMARY: Modifiers = Modifiers::LOGO;
    #[cfg(not(target_os = "macos"))]
    const PRIMARY: Modifiers = Modifiers::CTRL;

    #[test]
    fn matches_is_modifier_exact_and_case_insensitive() {
        let copy = Action::CopyText.default_shortcut(); // primary+C
        assert!(copy.matches(PRIMARY, &ch("c")));
        assert!(copy.matches(PRIMARY, &ch("C"))); // case-insensitive
        assert!(!copy.matches(PRIMARY | Modifiers::SHIFT, &ch("c"))); // extra modifier
        assert!(!copy.matches(Modifiers::empty(), &ch("c"))); // missing modifier
        assert!(!copy.matches(PRIMARY, &ch("x"))); // wrong key
    }

    #[test]
    fn select_vs_deselect_disambiguate_by_shift() {
        let km = Keymap::defaults();
        assert_eq!(km.action_for(Context::Overlay, PRIMARY, &ch("a")), Some(Action::SelectAllText));
        assert_eq!(
            km.action_for(Context::Overlay, PRIMARY | Modifiers::SHIFT, &ch("a")),
            Some(Action::DeselectText)
        );
    }

    #[test]
    fn named_key_matches_and_has_no_modifiers() {
        let close = Action::PreviewCancel.default_shortcut(); // Esc
        assert!(close.matches(Modifiers::empty(), &Key::Named(Named::Escape)));
        assert!(!close.matches(Modifiers::CTRL, &Key::Named(Named::Escape)));
    }

    #[test]
    fn from_event_rejects_bare_modifiers() {
        assert!(Shortcut::from_event(PRIMARY, &Key::Named(Named::Control)).is_none());
        assert_eq!(
            Shortcut::from_event(PRIMARY, &ch("c")),
            Some(Action::CopyText.default_shortcut())
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn label_formats_modifiers() {
        assert_eq!(Action::CopyText.default_shortcut().label(), "Ctrl+C");
        assert_eq!(Action::DeselectText.default_shortcut().label(), "Ctrl+Shift+A");
        assert_eq!(Action::PreviewCancel.default_shortcut().label(), "Esc");
    }

    /// Linux/Windows defaults use Ctrl (the primary command modifier), never logo,
    /// for the in-app editor chords — locking the historical bindings byte-identical.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn primary_defaults_are_ctrl_off_macos() {
        for action in [
            Action::CopyText,
            Action::SelectAllText,
            Action::DeselectText,
            Action::PreviewSave,
            Action::PreviewSaveAs,
            Action::PreviewCopy,
            Action::PreviewDelete,
            Action::PreviewUndo,
            Action::PreviewRedo,
        ] {
            let sc = action.default_shortcut();
            assert!(sc.ctrl, "{action:?} should default to Ctrl off macOS");
            assert!(!sc.logo, "{action:?} should not default to logo off macOS");
        }
    }

    /// macOS defaults route the same in-app editor chords to Cmd (logo/⌘), the
    /// native modifier, while non-modified defaults (Play/Covermark/…) stay bare.
    #[cfg(target_os = "macos")]
    #[test]
    fn primary_defaults_are_cmd_on_macos() {
        for action in [
            Action::CopyText,
            Action::SelectAllText,
            Action::DeselectText,
            Action::PreviewSave,
            Action::PreviewSaveAs,
            Action::PreviewCopy,
            Action::PreviewDelete,
            Action::PreviewUndo,
            Action::PreviewRedo,
        ] {
            let sc = action.default_shortcut();
            assert!(sc.logo, "{action:?} should default to Cmd (logo) on macOS");
            assert!(!sc.ctrl, "{action:?} should not default to Ctrl on macOS");
        }
        // The shifted chords keep Shift alongside Cmd.
        assert!(Action::PreviewSaveAs.default_shortcut().shift);
        assert!(Action::PreviewRedo.default_shortcut().shift);
        // Bare-key defaults stay unmodified on macOS too.
        let play = Action::PreviewPlay.default_shortcut();
        assert!(!play.ctrl && !play.logo && !play.alt && !play.shift);
        // macOS labels render the ⌘ glyph.
        assert_eq!(Action::CopyText.default_shortcut().label(), "⌘C");
        assert_eq!(Action::PreviewSaveAs.default_shortcut().label(), "⇧⌘S");
    }

    /// A persisted override (e.g. a Linux config carried to a Mac) is applied
    /// LITERALLY onto the platform defaults — its own modifiers are kept, never
    /// rewritten to the platform's primary. Only untouched actions get the
    /// platform default.
    #[test]
    fn overrides_are_literal_over_platform_defaults() {
        // A Linux-shaped override: Ctrl+K for SelectAllText (logo unset).
        let ov = vec![(Action::SelectAllText, Some(Shortcut::ctrl_char('k')))];
        let mut km = Keymap::defaults();
        km.apply_overrides(&ov);
        let fs = km.get(Action::SelectAllText).unwrap();
        assert!(fs.ctrl && !fs.logo, "override keeps its literal Ctrl, not the platform primary");
        // An action with no override still gets the platform default.
        assert_eq!(km.get(Action::CopyText), Some(Action::CopyText.default_shortcut()));
    }

    #[test]
    fn set_steals_on_conflict() {
        let mut km = Keymap::defaults();
        // Rebind "copy" to primary+A, which "select all" already holds.
        km.set(Action::CopyText, Action::SelectAllText.default_shortcut());
        assert_eq!(km.action_for(Context::Overlay, PRIMARY, &ch("a")), Some(Action::CopyText));
        // Select-all loses primary+A (now unbound); Copy's old primary+C is no longer matched.
        assert_eq!(km.get(Action::SelectAllText), None);
        assert_eq!(km.action_for(Context::Overlay, PRIMARY, &ch("c")), None);
    }

    #[test]
    fn contexts_share_bindings_without_conflict() {
        let km = Keymap::defaults();
        // primary+C is bound in BOTH contexts by default; each resolves per surface. Esc is the
        // single Close action (Preview context); the overlay reuses its binding in handle_key.
        let esc = Key::Named(Named::Escape);
        assert_eq!(km.action_for(Context::Overlay, Modifiers::empty(), &esc), None);
        assert_eq!(km.action_for(Context::Preview, Modifiers::empty(), &esc), Some(Action::PreviewCancel));
        assert_eq!(km.action_for(Context::Overlay, PRIMARY, &ch("c")), Some(Action::CopyText));
        assert_eq!(km.action_for(Context::Preview, PRIMARY, &ch("c")), Some(Action::PreviewCopy));
    }

    /// The region "Copy selection" quick-action shares primary+C with the OCR CopyText
    /// (Overlay) and the Preview copy, but lives in its OWN context (Region). Each context
    /// resolves the chord to its own action independently — no context steals another's
    /// bind. The dispatcher (`app::handle_key`) picks the Region lane first when a region
    /// is drawn; here we assert the keymap keeps all three distinct on the same chord.
    #[test]
    fn region_copy_shares_primary_c_across_contexts() {
        let km = Keymap::defaults();
        assert_eq!(km.action_for(Context::Region, PRIMARY, &ch("c")), Some(Action::RegionCopy));
        // The other two contexts keep their own primary+C actions untouched.
        assert_eq!(km.action_for(Context::Overlay, PRIMARY, &ch("c")), Some(Action::CopyText));
        assert_eq!(km.action_for(Context::Preview, PRIMARY, &ch("c")), Some(Action::PreviewCopy));
        // A wrong key in the Region context resolves to nothing.
        assert_eq!(km.action_for(Context::Region, PRIMARY, &ch("x")), None);
        // RegionCopy is the sole action in the Region context.
        assert_eq!(Action::RegionCopy.context(), Context::Region);
    }

    /// RegionCopy defaults to the platform PRIMARY modifier + C: Ctrl+C on Linux/Windows,
    /// Cmd+C on macOS — the same convention as the other editor chords, asserted on both
    /// cfg branches so a future modifier tweak can't silently diverge them.
    #[test]
    fn region_copy_default_is_primary_c() {
        let sc = Action::RegionCopy.default_shortcut();
        assert!(matches!(&sc.key, ShortcutKey::Char(c) if c == "c"));
        assert!(!sc.alt && !sc.shift);
        assert!(sc.matches(PRIMARY, &ch("c")));
        assert!(sc.matches(PRIMARY, &ch("C"))); // case-insensitive
        #[cfg(target_os = "macos")]
        {
            assert!(sc.logo && !sc.ctrl, "RegionCopy should default to Cmd on macOS");
            assert_eq!(sc.label(), "⌘C");
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert!(sc.ctrl && !sc.logo, "RegionCopy should default to Ctrl off macOS");
            assert_eq!(sc.label(), "Ctrl+C");
        }
    }

    /// Rebinding RegionCopy does NOT disturb CopyText (they are in different contexts, so
    /// `set`'s conflict-stealing — which is per-context — never touches the other). This is
    /// the guarantee that the shared-chord design keeps the OCR scanner copy intact.
    #[test]
    fn region_copy_rebind_leaves_copytext_intact() {
        let mut km = Keymap::defaults();
        km.set(Action::RegionCopy, Shortcut::ctrl_char('y'));
        // RegionCopy moved; CopyText's primary+C in the Overlay context is untouched.
        assert_eq!(km.action_for(Context::Region, Modifiers::CTRL, &ch("y")), Some(Action::RegionCopy));
        assert_eq!(km.get(Action::CopyText), Some(Action::CopyText.default_shortcut()));
        assert_eq!(km.action_for(Context::Overlay, PRIMARY, &ch("c")), Some(Action::CopyText));
    }

    #[test]
    fn unbind_removes_trigger_and_persists() {
        let mut km = Keymap::defaults();
        km.unbind(Action::CopyText);
        assert_eq!(km.get(Action::CopyText), None);
        assert_eq!(km.action_for(Context::Overlay, PRIMARY, &ch("c")), None);
        // An unbind is an override and round-trips through persistence.
        let mut km2 = Keymap::defaults();
        km2.apply_overrides(&km.overrides());
        assert_eq!(km2.get(Action::CopyText), None);
    }

    #[test]
    fn overrides_roundtrip() {
        let mut km = Keymap::defaults();
        assert!(km.overrides().is_empty());
        km.set(Action::SelectAllText, Shortcut::ctrl_char('k'));
        let ov = km.overrides();
        let mut km2 = Keymap::defaults();
        km2.apply_overrides(&ov);
        assert_eq!(km2.action_for(Context::Overlay, Modifiers::CTRL, &ch("k")), Some(Action::SelectAllText));
    }
}

/// DRAGON-294: the macOS daemon-spec → modifier-symbol formatter. Pure, so it tests without
/// any AppKit. The vocabulary matches `daemon_spec` / the daemon parser, case-insensitively.
#[cfg(all(test, target_os = "macos"))]
mod mac_symbolic_spec_tests {
    use super::mac_symbolic_spec;

    #[test]
    fn every_modifier_renders_its_glyph_in_canonical_order() {
        // Written out of order in the spec; rendered in the platform order ⌃⌥⇧⌘ — the SAME
        // order `Shortcut::label` uses (Command LAST), so e.g. "Save As" reads ⇧⌘S in both.
        assert_eq!(mac_symbolic_spec("Cmd+Shift+Alt+Ctrl+K"), "⌃⌥⇧⌘K");
        assert_eq!(mac_symbolic_spec("Ctrl+Alt+Shift+1"), "⌃⌥⇧1");
        assert_eq!(mac_symbolic_spec("Cmd+Shift+2"), "⇧⌘2");
    }

    #[test]
    fn modifier_aliases_and_case_insensitive() {
        // Command/Super → ⌘, Option → ⌥, Control → ⌃, all case-insensitive.
        assert_eq!(mac_symbolic_spec("command+shift+s"), "⇧⌘S");
        assert_eq!(mac_symbolic_spec("super+9"), "⌘9");
        assert_eq!(mac_symbolic_spec("option+k"), "⌥K");
        assert_eq!(mac_symbolic_spec("control+c"), "⌃C");
    }

    #[test]
    fn bare_keys_pass_through() {
        // Named keys / F-keys / PrintScreen have no modifiers and render verbatim.
        assert_eq!(mac_symbolic_spec("PrintScreen"), "PrintScreen");
        assert_eq!(mac_symbolic_spec("F13"), "F13");
        assert_eq!(mac_symbolic_spec("a"), "A"); // single letter uppercased
        assert_eq!(mac_symbolic_spec("Enter"), "Enter");
    }

    #[test]
    fn arrow_names_render_as_glyphs() {
        assert_eq!(mac_symbolic_spec("Cmd+ArrowUp"), "⌘↑");
        assert_eq!(mac_symbolic_spec("ArrowDown"), "↓");
    }

    #[test]
    fn empty_spec_is_empty() {
        assert_eq!(mac_symbolic_spec(""), "");
        assert_eq!(mac_symbolic_spec("   "), "");
    }
}

/// DRAGON-294 (Windows): the daemon capture-hotkey spec renders with Windows modifier NAMES
/// — the logo token "Cmd" (as `daemon_spec` serializes it) reads "Win", not "Cmd". Pure, so
/// it tests without any Win32. Vocabulary matches `daemon_spec` / the daemon parser.
#[cfg(all(test, windows))]
mod win_readable_spec_tests {
    use super::win_readable_spec;

    #[test]
    fn logo_token_reads_win_not_cmd() {
        // The reported issue: `daemon_spec` serializes the logo/Win key as "Cmd", which is
        // mac terminology — on Windows it must read "Win".
        assert_eq!(win_readable_spec("Cmd+Shift+2"), "Win+Shift+2");
        // Every logo alias the daemon parser accepts collapses to "Win".
        assert_eq!(win_readable_spec("command+9"), "Win+9");
        assert_eq!(win_readable_spec("super+k"), "Win+k");
        assert_eq!(win_readable_spec("win+a"), "Win+a");
        assert_eq!(win_readable_spec("meta+f1"), "Win+f1");
    }

    #[test]
    fn common_modifiers_stay_text_and_key_passes_through() {
        // The common Windows chord (no logo key) is unchanged bar canonical modifier casing.
        assert_eq!(win_readable_spec("Ctrl+Alt+K"), "Ctrl+Alt+K");
        assert_eq!(win_readable_spec("ctrl+alt+shift+1"), "Ctrl+Alt+Shift+1");
        assert_eq!(win_readable_spec("option+j"), "Alt+j");
        // A bare key / named key passes through verbatim (no modifiers to rewrite).
        assert_eq!(win_readable_spec("PrintScreen"), "PrintScreen");
        assert_eq!(win_readable_spec("F13"), "F13");
    }

    #[test]
    fn empty_is_empty() {
        assert_eq!(win_readable_spec(""), "");
        assert_eq!(win_readable_spec("   "), "");
    }
}
