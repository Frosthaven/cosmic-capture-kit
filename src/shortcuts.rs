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

use cosmic::iced::keyboard::{key::Named, Key, Location, Modifiers};
use serde::{Deserialize, Serialize};

/// Which surface an action belongs to. Actions in different contexts may share a
/// binding (e.g. Esc cancels both the capture overlay and the preview), since only one
/// context is ever active at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    // DRAGON-451: a `Region` context lived here — a drawn region selection, its own lane so
    // primary+C could mean "copy the drawn region" there and "copy recognized text" in the
    // capture overlay without the two fighting. Its sole action (`RegionCopy`) is retired,
    // so the context went with it and primary+C in the overlay is the OCR copy outright.
    //
    // DRAGON-627: an `Overlay` context lived here too, the capture overlay, holding exactly
    // the three OCR text actions (`CopyText`, `SelectAllText`, `DeselectText`). All three are
    // BAKED now ([`fixed_scanner_action`]), so the context has no members left and went the
    // same way `Region` did. The capture overlay still answers keys; it answers them from the
    // fixed matchers in `app::keyboard` rather than from a keymap lane.
    /// The post-capture preview overlay.
    Preview,
    /// An in-progress recording (stop / toggle mic / toggle system audio).
    Recording,
}

/// A user action that can be triggered by a keyboard shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Action {
    // DRAGON-627: `CopyText` (primary+C), `SelectAllText` (primary+A) and `DeselectText`
    // (primary+D) lived here, the scanner overlay's three OCR text actions and the only
    // members of the retired `Context::Overlay`. All three are BAKED now: fixed bindings, off
    // the Keyboard Shortcuts page, no longer remappable and no longer persisted. See
    // [`fixed_scanner_action`] for the chords and the whole of why. Their DEFAULTS are
    // unchanged, so nothing a user pressed before presses differently now.
    //
    // An override stored against any of the three deserializes to `None` and is dropped
    // (see `state::schema`'s `shortcut_overrides`), so a user who had customised OTHER
    // bindings keeps every one of them.
    //
    // DESELECT IS `Ctrl+D` IN EVERY CONTEXT (DRAGON-369) survives the removal as a rule, and
    // is worth keeping written down: select-all is one key everywhere and deselect follows
    // it, which is why [`Self::PreviewDeselectAll`] is `Ctrl+D` and the baked scanner
    // deselect is too. `Ctrl+D` is also Photoshop's Deselect. A future action that DESELECTS
    // something takes this key in its own context.
    // DRAGON-617: `PreviewSave` (primary+S), `PreviewUpload` (primary+U), `PreviewCancel`
    // (Escape), `PreviewUndo` (primary+Z) and `PreviewRedo` (primary+Shift+Z) lived here.
    // All five are BAKED now: fixed bindings, off the Keyboard Shortcuts page, no longer
    // remappable and no longer persisted. See [`fixed_editor_action`] for the chords and
    // the whole of why. Their DEFAULTS are unchanged, so nothing a user pressed before
    // presses differently now.
    //
    // An override stored against any of the five deserializes to `None` and is dropped
    // (see `state::schema`'s `shortcut_overrides`), so a user who had customised OTHER
    // bindings keeps every one of them.
    /// Preview: copy the capture to the clipboard (default Ctrl+C).
    PreviewCopy,
    // DRAGON-467: `PreviewDelete` (Ctrl+Shift+X, "delete the file and close") lived here,
    // with a long note on why that awkward chord was chosen for an irreversible unlink. The
    // editor does not delete anything any more, so the action went with the button. The
    // reasoning is still worth having if a destructive binding ever returns: Ctrl+D was
    // rejected because Photoshop spells it Deselect, Ctrl+X because it sits next to
    // Ctrl+C/Ctrl+S and is the natural home of a future annotation Cut, and Shift+Delete
    // because it is ONE modifier away from `PreviewDeleteSegment` (plain Delete) on the very
    // same screen. An override stored against the retired name deserializes to `None` and is
    // dropped (see `state::schema`'s `shortcut_overrides`).
    /// Preview: open/close the covermark picker (default W).
    PreviewCovermark,
    /// Preview: play/pause a recording inline (default P).
    PreviewPlay,
    /// Preview: step to the previous frame (default ,).
    PreviewFramePrev,
    /// Preview: step to the next frame (default .).
    PreviewFrameNext,
    /// Preview: delete the selected timeline segment (default Delete).
    PreviewDeleteSegment,
    /// Preview: the POINTER (pure selection) annotation tool (default V — Photoshop's Move
    /// tool) — DRAGON-341.
    PreviewAnnotPointer,
    /// Preview: select every annotation in the scene, engaging pointer mode (default Ctrl+A) —
    /// DRAGON-341.
    PreviewSelectAll,
    /// Preview: clear the annotation selection (default Ctrl+D) — DRAGON-369. The matched pair
    /// to [`Self::PreviewSelectAll`], and Photoshop's Deselect; the tombstone at the head of
    /// this enum carries the everywhere-we-deselect rule, and
    /// [`FixedScannerAction::Deselect`] is the other key that follows it.
    PreviewDeselectAll,
    /// Preview: the Arrow annotation tool (default A). SOLO on purpose — the highest-frequency
    /// tool in a screenshot annotator must never cost two presses (DRAGON-369).
    PreviewAnnotArrow,
    /// Preview: the Step Marker annotation tool (default I — Photoshop's Count tool, in the
    /// eyedropper group) — DRAGON-340. (The code calls it the "badge" everywhere; only the
    /// user-facing label was renamed.)
    PreviewAnnotBadge,
    /// Preview: the Box (rectangle) annotation tool. UNBOUND by default (DRAGON-369) — it is
    /// member 2 of the SHAPE slot, reached with [`Self::PreviewAnnotShapeCycle`] (`U`).
    PreviewAnnotBox,
    /// Preview: the Highlight (multiply box) annotation tool. UNBOUND by default (DRAGON-369) —
    /// member 1 of the SHAPE slot ([`Self::PreviewAnnotShapeCycle`], `U`).
    PreviewAnnotHighlight,
    /// Preview: the Box-Highlight (highlight fill + box outline) annotation tool. UNBOUND by
    /// default (DRAGON-369) — member 3 of the SHAPE slot
    /// ([`Self::PreviewAnnotShapeCycle`], `U`), the slowest row to reach, so this action is
    /// also its first-class escape hatch: bind it and the direct key wins (see
    /// [`Action::ALL`]).
    PreviewAnnotBoxHighlight,
    /// Preview: the Pixelate (destructive redaction) annotation tool. UNBOUND by default
    /// (DRAGON-369) — member 1 of the REDACTION slot
    /// ([`Self::PreviewAnnotRedactCycle`], `M`).
    PreviewAnnotPixelate,
    /// Preview: the Blur (destructive redaction) annotation tool. UNBOUND by default
    /// (DRAGON-369) — member 2 of the REDACTION slot
    /// ([`Self::PreviewAnnotRedactCycle`], `M`).
    PreviewAnnotBlur,
    /// Preview: the Spotlight (dim knockout) annotation tool — DRAGON-329. UNBOUND by default
    /// (DRAGON-384) — member 4 of the SHAPE slot ([`Self::PreviewAnnotShapeCycle`], `U`), reached
    /// by cycling; it still owns the Dim slider, which rides the shape group. It ships with a
    /// FIRST-CLASS per-tool escape hatch like the other slot members (bind it and the direct key
    /// wins). A persisted keymap from before DRAGON-384 that still holds the retired `S` default
    /// keeps working: `S` is applied as an explicit per-tool bind and, because this action is
    /// listed BEFORE the slot cycle in [`Action::ALL`], it resolves directly to spotlight (see
    /// [`Action::ALL`]) rather than crashing or silently swallowing the key.
    PreviewAnnotSpotlight,
    /// Preview: the freehand Pencil annotation tool (default B — Photoshop's Brush slot) —
    /// DRAGON-338.
    PreviewAnnotPen,
    /// Preview: the Text annotation tool (default T — Photoshop's Type tool) — DRAGON-354.
    PreviewAnnotText,
    /// Preview: the Eraser (removes pen strokes; default E) — DRAGON-338. Deliberately NOT
    /// folded into the Pencil's slot (DRAGON-369): inking and erasing are the pair you
    /// alternate between fastest.
    PreviewAnnotEraser,
    /// Preview: the REDACTION tool slot — Pixelate, then Blur (default M) — DRAGON-369.
    /// Arm-then-advance: see [`Action::ALL`]'s slot note.
    PreviewAnnotRedactCycle,
    /// Preview: the SHAPE tool slot — Highlight, Border, Border Highlight, Spotlight (default U,
    /// which IS Photoshop's shape-tool key) — DRAGON-369, spotlight folded in by DRAGON-384.
    /// Arm-then-advance: see [`Action::ALL`]'s slot note.
    PreviewAnnotShapeCycle,
    /// Preview: duplicate the selected annotation, offset toward the frame center (default D).
    PreviewAnnotDuplicate,
    /// Preview: cycle the annotation stroke width to the next preset, wrapping (default L).
    PreviewAnnotStrokeCycle,
    /// Preview: the CROP tool (default C, Photoshop's Crop key) - DRAGON-385. Starts or confirms a
    /// crop session exactly like the toolbar button ([`crate::app::message::preview::PreviewMsg`]'s
    /// `CropEnter`, which both opens and applies). A live session OWNS the keyboard (Enter accepts,
    /// Escape cancels; see `preview_modal_key`), so a press mid-session is swallowed rather than
    /// double-starting the tool.
    PreviewCrop,
    /// Preview: open the annotation color flyout (default S, "S for swatches" - moved off C for the
    /// crop tool in DRAGON-385, freed by DRAGON-384's spotlight consolidation).
    PreviewColorFlyout,
    /// Preview: swap the active annotation color to its COMPANION (its complement), toggling
    /// back on a second press (default X — Photoshop's foreground/background swap) — DRAGON-386.
    PreviewColorCompanionSwap,
    /// Preview: arm the HAND tool (default H — Photoshop's Hand key). DRAGON-392 turned panning
    /// from a MODE toggle into a real armed tool, so this arms it exactly like every other
    /// per-tool action; the key is unchanged. The old `PreviewTogglePan` spelling is accepted on
    /// load (`serde(alias)`) so a user who rebound the pan key keeps their binding.
    #[serde(alias = "PreviewTogglePan")]
    PreviewAnnotHand,
    /// Recording: stop + save the in-progress recording (default Shift+Alt+Enter).
    RecordStop,
    /// Recording: toggle the microphone channel (default Shift+Alt+M).
    RecordToggleMic,
    /// Recording: toggle the system-audio channel (default Shift+Alt+S).
    RecordToggleSystemAudio,
}

impl Action {
    /// Every action, in display + MATCH order. Preview actions are grouped so the
    /// settings page's contiguous-group builder yields "Action Shortcuts" (save /
    /// copy / close / delete / covermark / undo / redo), then "Image Editor
    /// Shortcuts" (annotation tools / color / pan), then "Video Editor Shortcuts"
    /// (play / frame step / delete segment) — image above video.
    ///
    /// # Tool SLOTS, and why the order here is load-bearing (DRAGON-369)
    ///
    /// Nine keys serve twelve annotation tools: six of them are reached through a per-slot
    /// CYCLE action ([`Action::PreviewAnnotRedactCycle`] `M`,
    /// [`Action::PreviewAnnotShapeCycle`] `U`) whose behaviour is **arm-then-advance** — the
    /// slot's current member if the slot is not armed, the next member (wrapping) if it is.
    /// A cycle action is one ordinary `Action` bound to one ordinary [`Shortcut`], exactly like
    /// [`Action::PreviewAnnotStrokeCycle`]; there is no keymap concept behind it. Membership
    /// and cycle ORDER come from `preview::chrome`'s declared `ANNOT_TRAY` (the tray is the
    /// single source of truth for both), and the per-slot cursor is runtime preview state.
    ///
    /// **There is no timeout, and there must never be one** — Photoshop has none, and a
    /// timeout would make one keystroke mean two different things depending on typing speed:
    /// unexplainable to a user and untestable headless.
    ///
    /// **A deliberate per-tool bind always beats a slot bind.** All twelve per-tool actions
    /// stay in this list (the six slot members simply default UNBOUND) and every one of them
    /// is listed BEFORE the slot actions, so [`Keymap::action_for`]'s `.find()` resolves a
    /// per-tool binding first BY CONSTRUCTION. That is what makes
    /// [`Keymap::apply_overrides`] — which does not de-duplicate against changed defaults —
    /// a no-op for anyone who had rebound a tool, and it gives the third member of a slot a
    /// first-class escape hatch. Keep the ordering when adding tools.
    pub const ALL: [Action; 31] = [
        // DRAGON-617 took Save and Upload out from either side of Copy, and Close, Undo and
        // Redo from after it. The "Action Shortcuts" group is down to these two, and they are
        // still CONTIGUOUS, which is what `group()` requires of every group in this list.
        //
        // DRAGON-627 took the three OCR text actions from the head of this list, and with them
        // the whole "OCR Text Recognition" group. Nothing maps to the settings page's Capture
        // tab any more, which is exactly how that tab stops being shown: see
        // `app::settings::ShortcutsTab::occupied`.
        Action::PreviewCopy,
        Action::PreviewCovermark,
        Action::PreviewAnnotPointer,
        // The HAND (DRAGON-392) rides with the pointer: both are selection-family tools that
        // draw nothing, and like every per-tool action it is listed BEFORE the slot cycles.
        Action::PreviewAnnotHand,
        Action::PreviewSelectAll,
        Action::PreviewDeselectAll,
        // The twelve PER-TOOL actions come first — see the slot note above.
        Action::PreviewAnnotArrow,
        Action::PreviewAnnotBadge,
        Action::PreviewAnnotBox,
        Action::PreviewAnnotHighlight,
        Action::PreviewAnnotBoxHighlight,
        Action::PreviewAnnotPixelate,
        Action::PreviewAnnotBlur,
        Action::PreviewAnnotSpotlight,
        Action::PreviewAnnotPen,
        Action::PreviewAnnotText,
        Action::PreviewAnnotEraser,
        // …then the SLOT cycles, so a per-tool bind always resolves first.
        Action::PreviewAnnotRedactCycle,
        Action::PreviewAnnotShapeCycle,
        Action::PreviewAnnotDuplicate,
        Action::PreviewAnnotStrokeCycle,
        Action::PreviewCrop,
        Action::PreviewColorFlyout,
        Action::PreviewColorCompanionSwap,
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
            Action::PreviewCopy => "Copy to clipboard",
            Action::PreviewPlay => "Play",
            Action::PreviewFramePrev => "Previous frame",
            Action::PreviewFrameNext => "Next frame",
            Action::PreviewCovermark => "Covermark",
            Action::PreviewDeleteSegment => "Delete segment",
            Action::PreviewAnnotPointer => "Select tool",
            Action::PreviewSelectAll => "Select all annotations",
            Action::PreviewDeselectAll => "Deselect all annotations",
            Action::PreviewAnnotArrow => "Arrow tool",
            Action::PreviewAnnotBadge => "Step marker tool",
            Action::PreviewAnnotBox => "Box tool",
            Action::PreviewAnnotHighlight => "Highlight tool",
            Action::PreviewAnnotBoxHighlight => "Box Highlight tool",
            Action::PreviewAnnotPixelate => "Pixelate tool",
            Action::PreviewAnnotBlur => "Blur tool",
            Action::PreviewAnnotSpotlight => "Spotlight tool",
            Action::PreviewAnnotPen => "Pencil tool",
            Action::PreviewAnnotText => "Text tool",
            Action::PreviewAnnotEraser => "Eraser",
            Action::PreviewAnnotRedactCycle => "Cycle redaction tools",
            Action::PreviewAnnotShapeCycle => "Cycle shape tools",
            Action::PreviewAnnotDuplicate => "Duplicate annotation",
            Action::PreviewAnnotStrokeCycle => "Cycle line width",
            Action::PreviewCrop => "Crop tool",
            Action::PreviewColorFlyout => "Color",
            Action::PreviewColorCompanionSwap => "Swap color",
            Action::PreviewAnnotHand => "Hand tool",
            Action::RecordStop => "Stop and save recording",
            Action::RecordToggleMic => "Toggle Microphone",
            Action::RecordToggleSystemAudio => "Toggle system audio",
        }
    }

    /// Helper line for the settings row.
    pub fn description(self) -> &'static str {
        match self {
            // Action Shortcuts group: descriptions removed per DRAGON-158.
            Action::PreviewCopy => "",
            Action::PreviewCovermark => "",
            Action::PreviewPlay => "Play or pause a recording in the preview.",
            Action::PreviewFramePrev => "Step to the previous frame.",
            Action::PreviewFrameNext => "Step to the next frame.",
            Action::PreviewDeleteSegment => "Delete the selected timeline segment.",
            Action::PreviewAnnotPointer => "Select, multi-select and move annotations.",
            Action::PreviewSelectAll => "Select every annotation on the image.",
            Action::PreviewDeselectAll => "Clear the annotation selection.",
            Action::PreviewAnnotArrow => "Draw an arrow.",
            Action::PreviewAnnotBadge => "Drop a numbered step marker.",
            Action::PreviewAnnotBox => "Draw a rectangle. Reached with Cycle shape tools.",
            Action::PreviewAnnotHighlight => {
                "Draw a multiply-blended highlight box. Reached with Cycle shape tools."
            }
            Action::PreviewAnnotBoxHighlight => {
                "Draw a highlight box with an outline. Reached with Cycle shape tools."
            }
            Action::PreviewAnnotPixelate => {
                "Pixelate a region (destructive redaction). Reached with Cycle redaction tools."
            }
            Action::PreviewAnnotBlur => {
                "Blur a region (destructive redaction). Reached with Cycle redaction tools."
            }
            Action::PreviewAnnotSpotlight => {
                "Draw a spotlight that dims everything outside it. Reached with Cycle shape tools."
            }
            Action::PreviewAnnotPen => "Draw freehand strokes.",
            Action::PreviewAnnotText => "Add a text caption: click to type, or drag a box to wrap within.",
            Action::PreviewAnnotEraser => "Erase pen strokes by dragging over them.",
            Action::PreviewAnnotRedactCycle => "Pixelate and blur. Press again to switch.",
            Action::PreviewAnnotShapeCycle => {
                "Highlight, border, and border highlight. Press again to switch."
            }
            Action::PreviewAnnotDuplicate => "Duplicate the selected annotation.",
            Action::PreviewAnnotStrokeCycle => "Cycle the line width.",
            Action::PreviewCrop => "Crop the image. Drag the handles, then press to apply.",
            Action::PreviewColorFlyout => "Open the annotation color picker.",
            Action::PreviewColorCompanionSwap => {
                "Swap the annotation color to its companion, and back again."
            }
            Action::PreviewAnnotHand => "Pan the picture by dragging it.",
            Action::RecordStop => "",
            Action::RecordToggleMic => "",
            Action::RecordToggleSystemAudio => "",
        }
    }

    /// The settings-page group this action is shown under (actions of the same group
    /// are contiguous in [`Action::ALL`], so the page can build one section per group).
    ///
    /// DRAGON-627 retired the one group that was NOT the preview editor's or the recording
    /// session's, "OCR Text Recognition", when its three actions were baked. No group here
    /// maps to `ShortcutsTab::Capture` any more, and that is what takes the tab off the strip.
    pub fn group(self) -> &'static str {
        match self {
            Action::PreviewCopy | Action::PreviewCovermark => "Action Shortcuts",
            Action::PreviewPlay
            | Action::PreviewFramePrev
            | Action::PreviewFrameNext
            | Action::PreviewDeleteSegment => "Video Editor Shortcuts",
            Action::PreviewAnnotPointer
            | Action::PreviewSelectAll
            | Action::PreviewDeselectAll
            | Action::PreviewAnnotArrow
            | Action::PreviewAnnotBadge
            | Action::PreviewAnnotBox
            | Action::PreviewAnnotHighlight
            | Action::PreviewAnnotBoxHighlight
            | Action::PreviewAnnotPixelate
            | Action::PreviewAnnotBlur
            | Action::PreviewAnnotSpotlight
            | Action::PreviewAnnotPen
            | Action::PreviewAnnotText
            | Action::PreviewAnnotEraser
            | Action::PreviewAnnotRedactCycle
            | Action::PreviewAnnotShapeCycle
            | Action::PreviewAnnotStrokeCycle
            | Action::PreviewAnnotDuplicate
            | Action::PreviewCrop
            | Action::PreviewColorFlyout
            | Action::PreviewColorCompanionSwap
            | Action::PreviewAnnotHand => "Image Editor Shortcuts",
            Action::RecordStop
            | Action::RecordToggleMic
            | Action::RecordToggleSystemAudio => "Recording",
        }
    }

    /// Which surface this action belongs to.
    pub fn context(self) -> Context {
        match self {
            Action::PreviewCopy
            | Action::PreviewPlay
            | Action::PreviewFramePrev
            | Action::PreviewFrameNext
            | Action::PreviewCovermark
            | Action::PreviewDeleteSegment
            | Action::PreviewAnnotPointer
            | Action::PreviewSelectAll
            | Action::PreviewDeselectAll
            | Action::PreviewAnnotArrow
            | Action::PreviewAnnotBadge
            | Action::PreviewAnnotBox
            | Action::PreviewAnnotHighlight
            | Action::PreviewAnnotBoxHighlight
            | Action::PreviewAnnotPixelate
            | Action::PreviewAnnotBlur
            | Action::PreviewAnnotSpotlight
            | Action::PreviewAnnotPen
            | Action::PreviewAnnotText
            | Action::PreviewAnnotEraser
            | Action::PreviewAnnotRedactCycle
            | Action::PreviewAnnotShapeCycle
            | Action::PreviewAnnotStrokeCycle
            | Action::PreviewAnnotDuplicate
            | Action::PreviewCrop
            | Action::PreviewColorFlyout
            | Action::PreviewColorCompanionSwap
            | Action::PreviewAnnotHand => Context::Preview,
            Action::RecordStop
            | Action::RecordToggleMic
            | Action::RecordToggleSystemAudio => Context::Recording,
        }
    }

    /// The factory-default binding — `None` for an action that ships UNBOUND.
    ///
    /// The annotation-tool letters are Photoshop's wherever Photoshop has an answer
    /// (DRAGON-369): `V` move/select, `T` type, `B` brush, `E` eraser, `U` shapes, `I` count,
    /// `H` hand, `M` for the redaction family. The six tools that ship UNBOUND are the members
    /// of the two cycling SLOTS — nothing is unreachable, and binding one of them back is the
    /// documented escape hatch (see [`Action::ALL`]). Spotlight joined the shape slot in
    /// DRAGON-384, retiring its former solo `S` default.
    pub fn default_shortcut(self) -> Option<Shortcut> {
        Some(match self {
            Action::PreviewCopy => Shortcut::primary_char('c'),
            Action::PreviewPlay => Shortcut::char('p'),
            Action::PreviewFramePrev => Shortcut::char(','),
            Action::PreviewFrameNext => Shortcut::char('.'),
            Action::PreviewCovermark => Shortcut::char('w'),
            Action::PreviewDeleteSegment => Shortcut::named(NamedKey::Delete),
            Action::PreviewAnnotPointer => Shortcut::char('v'),
            Action::PreviewSelectAll => Shortcut::primary_char('a'),
            Action::PreviewDeselectAll => Shortcut::primary_char('d'),
            Action::PreviewAnnotArrow => Shortcut::char('a'),
            Action::PreviewAnnotBadge => Shortcut::char('i'),
            // The six SLOT members ship unbound — reached through their slot's cycle key.
            Action::PreviewAnnotBox
            | Action::PreviewAnnotHighlight
            | Action::PreviewAnnotBoxHighlight
            | Action::PreviewAnnotSpotlight
            | Action::PreviewAnnotPixelate
            | Action::PreviewAnnotBlur => return None,
            Action::PreviewAnnotPen => Shortcut::char('b'),
            Action::PreviewAnnotText => Shortcut::char('t'),
            Action::PreviewAnnotEraser => Shortcut::char('e'),
            Action::PreviewAnnotRedactCycle => Shortcut::char('m'),
            Action::PreviewAnnotShapeCycle => Shortcut::char('u'),
            Action::PreviewAnnotDuplicate => Shortcut::char('d'),
            Action::PreviewAnnotStrokeCycle => Shortcut::char('l'),
            // DRAGON-385: C is the CROP tool (Photoshop parity); the color flyout moved to S.
            Action::PreviewCrop => Shortcut::char('c'),
            Action::PreviewColorFlyout => Shortcut::char('s'),
            Action::PreviewColorCompanionSwap => Shortcut::char('x'),
            // DRAGON-392: H arms the HAND TOOL (it toggled a pan MODE before) — same key, and
            // still Photoshop's Hand.
            Action::PreviewAnnotHand => Shortcut::char('h'),
            // The recording lane rides Shift+Alt (DRAGON-581, owner's call): see
            // `Shortcut::shift_alt` for why these three are the app's only chorded
            // defaults, and what the bare keys cost.
            Action::RecordStop => Shortcut::shift_alt_named(NamedKey::Enter),
            Action::RecordToggleMic => Shortcut::shift_alt_char('m'),
            Action::RecordToggleSystemAudio => Shortcut::shift_alt_char('s'),
        })
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

    /// [`Self::primary_char`] for a NAMED key (DRAGON-680's primary+Enter): Ctrl+key on
    /// Linux and Windows, Cmd+key on macOS. Same platform rule, same one place to change
    /// it, for the keys that are not characters.
    fn primary_named(k: NamedKey) -> Self {
        #[cfg(target_os = "macos")]
        {
            Shortcut { logo: true, ..Shortcut::named(k) }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Shortcut { ctrl: true, ..Shortcut::named(k) }
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

    /// A Shift+Alt default, the RECORDING lane's modifier pair (DRAGON-581).
    ///
    /// The three recording actions used to be bare keys (M, S, Enter), which made them
    /// the only bindings in the app that could collide with ordinary typing, and cost
    /// them their portal global shortcut: [`Self::xdg_trigger`] refuses an unmodified
    /// letter or Enter, because a desktop would either reject it or let it shadow every
    /// text field. Shift+Alt is the same shape on every platform (no Cmd-vs-Ctrl split,
    /// so unlike [`Self::primary_char`] there is nothing to branch on), and it clears
    /// that bar, so hold-to-talk and stop now register focus-free where the desktop ships
    /// the GlobalShortcuts interface.
    fn shift_alt(key: ShortcutKey) -> Self {
        Shortcut { ctrl: false, alt: true, shift: true, logo: false, key }
    }

    /// [`Self::shift_alt`] on a character key.
    fn shift_alt_char(c: char) -> Self {
        Self::shift_alt(ShortcutKey::Char(c.to_ascii_lowercase().to_string()))
    }

    /// [`Self::shift_alt`] on a named key.
    fn shift_alt_named(k: NamedKey) -> Self {
        Self::shift_alt(ShortcutKey::Named(k))
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

/// Whether a keypress is the macOS **Emoji & Symbols** chord — ⌃⌘Space, confirmed with the
/// owner as the trigger actually being pressed (DRAGON-361).
///
/// Deliberately NOT part of the editable keymap: it is a SYSTEM chord we merely recognize so
/// the mac seam can open the palette itself (`App::preview_modal_key` → the AppKit
/// `orderFrontCharacterPalette:` call), bypassing hotkey routing that never reaches an
/// accessory-policy app. The check is exact — Control + Command and NOTHING else — so adding
/// Shift or Option falls through to the normal text-edit handling instead of being swallowed.
///
/// The space bar has NO `Key::Named` spelling in iced's UI-Events key model (`Named` carries
/// no `Space` variant — space is a CHARACTER key), so the match is on the character. Both
/// spellings a Control-modified space can arrive as are accepted: the plain `" "` AppKit
/// reports through `charactersIgnoringModifiers`, and the ASCII NUL `"\0"` that Ctrl+Space
/// produces in the raw `characters` string — missing either would silently reinstate the bug,
/// and neither can be confused with another key while ⌃⌘ are the exact modifiers held.
///
/// The OTHER system trigger — the 🌐 Globe/Fn key, the Ventura+ default — is deliberately NOT
/// matched, because it is UNREACHABLE by construction and no predicate could help: on macOS
/// that key never arrives as a key event at all. It surfaces only through `flagsChanged:`,
/// where winit's `key_to_modifier` returns `None` for `NamedKey::Fn` and the event is dropped
/// before any `KeyEvent` is built; Apple's own documentation, quoted verbatim in winit's macOS
/// backend, says the Function key is handled "at a lower level and your app never sees" it. If
/// a Mac is ever configured to use Globe instead, the answer has to be a UI affordance
/// (toolbar / menu item) calling the same seam — never a hotkey hook.
///
/// Portable + pure ON PURPOSE (compiled everywhere, called only from the macOS arm) so the
/// decision logic is unit-testable on Linux, where the AppKit call it guards cannot be built.
/// Off macOS the only caller is that unit test, so the non-test build honestly declares it
/// dead there rather than hiding behind a blanket allow.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn is_character_palette_chord(modifiers: Modifiers, key: &Key) -> bool {
    // The chord is NAMED rather than tested inline so the rule reads forwards, as the doc
    // states it: Control + Command and nothing else. Spelled inline it is a negated
    // conjunction, which clippy's `nonminimal_bool` rewrites into four scattered negatives
    // that no longer look like a chord at all.
    let ctrl_cmd_only =
        modifiers.control() && modifiers.logo() && !modifiers.alt() && !modifiers.shift();
    if !ctrl_cmd_only {
        return false;
    }
    matches!(key, Key::Character(c) if c.as_str() == " " || c.as_str() == "\0")
}

/// **THE region-copy chord** (DRAGON-479): the platform's primary command modifier plus `C` —
/// `Ctrl+C` on Linux and Windows, `⌘C` on macOS, exactly like every other in-app primary chord
/// ([`Shortcut::primary_char`]).
///
/// **It is deliberately NOT a [`Keymap`] entry, and must not become one.** Everything in the
/// keymap is user-configurable by construction — that is what the keymap IS — and this chord
/// cannot be, for two reasons that both point the same way:
///
/// * it is the operating system's own copy convention, not an app preference. A user who rebound
///   it would be rebinding "copy" itself, which no other app on their desktop lets them do here.
/// * `Ctrl+C` ALREADY has a second, different meaning in this overlay: in SCANNER kind it
///   copies recognized text ([`FixedScannerAction::Copy`], a configurable `Action::CopyText`
///   until DRAGON-627 baked it too). Two configurable bindings on one chord is a conflict the
///   settings page would have to explain and the user would have to resolve.
///   Fixed, the precedence is a single readable rule in `keyboard.rs` (`region_copy_fires`):
///   scanner kind never fires this, so the OCR copy keeps the chord there untouched.
///
/// This mirrors "Search settings" (Ctrl+F / Cmd+F, DRAGON-158), the other fixed chord that lives
/// outside the keymap for the same reason. DRAGON-451 retired the CONFIGURABLE version of this
/// action (`Action::RegionCopy` and its whole `Context::Region` lane); DRAGON-479 brings the
/// behaviour back without the configurability that made it collide.
pub fn is_region_copy_chord(modifiers: Modifiers, key: &Key) -> bool {
    // Built from the same `Shortcut` its LABEL is rendered from, so the chord the app listens
    // for and the chord the tooltip advertises cannot drift. `matches` compares all four
    // modifier flags exactly, so a stray Shift or Alt does NOT fire it.
    Shortcut::primary_char('c').matches(modifiers, key)
}

/// A one-step movement on screen (DRAGON-599). The ONE thing an arrow key and its vim letter
/// both reduce to, so nothing downstream ever sees which key was pressed.
///
/// That is the whole reason it exists as a type rather than as a `(dx, dy)` pair produced at
/// two call sites: the owner asked for arrows AND `hjkl`, and two families of keys feeding two
/// handlers is two chances to disagree about what "left" does. Here they converge at the edge
/// of key dispatch ([`nudge_direction`]), and every consumer — the drawn region, the colour
/// picker's sample point — takes this and only this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    /// The movement as a `(dx, dy)` step of ONE unit, in screen orientation: x grows right,
    /// y grows DOWN, which is what every coordinate space in this app uses (compositor
    /// logical coordinates, surface points, image pixels alike). So `Up` is `-1` on y.
    ///
    /// The unit is the caller's: one logical pixel for a drawn region, one source pixel for
    /// the colour picker's sample. Pure.
    pub fn delta(self) -> (i32, i32) {
        match self {
            Direction::Left => (-1, 0),
            Direction::Right => (1, 0),
            Direction::Up => (0, -1),
            Direction::Down => (0, 1),
        }
    }
}

/// **Pure**, unit-tested: the FIXED directional-movement keys (DRAGON-599) — the four arrows
/// and the four vim letters `h` `j` `k` `l`, as one [`Direction`].
///
/// **Deliberately NOT [`Keymap`] entries**, for the reason [`is_region_copy_chord`] spells out
/// and with the same shape of argument. Directional movement is not an app preference: the
/// arrows are the operating system's own convention for "move this by one", `hjkl` are the
/// convention of every vim user's fingers, and a settings page offering to rebind "move left"
/// would be offering to break the only thing those keys mean. Eight bindings that must stay in
/// lockstep would also be eight rows to explain and eight ways to half-rebind the feature.
///
/// **No modifiers, exactly.** [`Modifiers::empty`] and nothing else, so `Ctrl+H`, `Alt+Left`
/// and `Shift+K` all fall through untouched. That leaves a modified arrow free for a future
/// bigger step, and it stops this from shadowing any chord a user has bound.
///
/// The letters are safe in the two surfaces that consult this, and that is checked rather than
/// assumed. The capture overlay answers exactly three chords of its own and all three are
/// primary chords ([`fixed_scanner_action`]: `Ctrl+C` / `Ctrl+A` / `Ctrl+D`, keymap entries in
/// a `Context::Overlay` lane until DRAGON-627 baked them); `Context::Recording`'s three are
/// all `Shift+Alt` chords.
/// The bare annotation letters — `H` hand, `L` stroke cycle, `V`, `T`, `B` and the rest — are
/// all `Context::Preview`, and a preview owns the keyboard through its own modal lane in
/// `app::keyboard`, which returns long before this is reached. No widget on either overlay
/// consumes a key PRESS at all (they read `ModifiersChanged` and nothing else), including the
/// scanner's, whose text selection is entirely mouse-driven.
pub fn nudge_direction(modifiers: Modifiers, key: &Key) -> Option<Direction> {
    if modifiers != Modifiers::empty() {
        return None;
    }
    match key {
        Key::Named(Named::ArrowLeft) => Some(Direction::Left),
        Key::Named(Named::ArrowRight) => Some(Direction::Right),
        Key::Named(Named::ArrowUp) => Some(Direction::Up),
        Key::Named(Named::ArrowDown) => Some(Direction::Down),
        // The vim directionals, and ONLY those four (the owner's limit): no counts, no `gg`,
        // no word motions. Case-insensitively, so a Caps Lock session still moves — Caps Lock
        // is not one of the four modifier flags, so the guard above lets `H` through.
        Key::Character(c) if c.eq_ignore_ascii_case("h") => Some(Direction::Left),
        Key::Character(c) if c.eq_ignore_ascii_case("l") => Some(Direction::Right),
        Key::Character(c) if c.eq_ignore_ascii_case("k") => Some(Direction::Up),
        Key::Character(c) if c.eq_ignore_ascii_case("j") => Some(Direction::Down),
        _ => None,
    }
}

/// How long a directional key must be HELD before it starts repeating (DRAGON-601).
///
/// **This is OUR delay, not the desktop's**, and owning it is the point. The compositor
/// announces a repeat cadence to every client at bind time, and on the owner's session that is
/// `wl_keyboard.repeat_info(rate = 25, delay = 600)`, cosmic-comp's built-in default. Those
/// numbers are tuned for TEXT ENTRY, where a repeat means "keep deleting" and 600ms of lead-in
/// is generous. Pixel nudging is a different job with a different tolerance: the owner asked
/// for "1px per tap, but hold to move should be available too", and a tap that costs two pixels
/// because it ran slightly long fails the first half of that sentence.
///
/// So the cadence is ours on every machine, and that is what makes the behaviour predictable:
/// a desktop configured with a 150ms text delay would otherwise turn a deliberate tap into
/// three pixels, and the same build would nudge differently on two machines for reasons the
/// user never connected to their keyboard settings.
///
/// 400ms is comfortably longer than any deliberate tap (a quick press is 80 to 150ms) and short
/// enough that "hold it down" feels like it starts moving promptly rather than sticking.
pub const NUDGE_HOLD_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

/// The fastest a HELD directional key may step, once [`NUDGE_HOLD_DELAY`] has passed.
///
/// **A ceiling, not a metronome.** We cannot invent repeats the compositor never sent, so this
/// can only ever discard repeats that arrive too close together, never manufacture extra ones.
/// On a session repeating at 25/s (40ms apart) nothing is discarded and the hold runs at the
/// desktop's own rate. On one configured to fire at 100/s it is capped here, so a hold crosses
/// the screen at a speed a human can still stop on.
///
/// Deliberately BELOW the common 25/s cadence so the ordinary case passes through untouched: a
/// cap that bit on a normal desktop would make hold-to-move feel slower than the system
/// everywhere, to defend against a configuration almost nobody has.
pub const NUDGE_REPEAT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(30);

const _: () = assert!(
    NUDGE_HOLD_DELAY.as_millis() >= NUDGE_REPEAT_INTERVAL.as_millis() * 5,
    "DRAGON-601: the hold delay must stay several repeat intervals long, or the first repeat \
     lands so soon after the press that a tap and a hold are the same gesture, which is exactly \
     the overshoot this pair was added to remove"
);

/// **Pure**, unit-tested: whether a directional key event should MOVE anything (DRAGON-601).
///
/// One decision, shared by both nudge consumers (the colour picker's sample and the drawn
/// region), so the two can never disagree about what a tap costs.
///
/// * `is_repeat` is iced's own flag: `false` for a real key press, `true` for an auto-repeat
///   the compositor synthesised. A PRESS always moves, exactly once, which is the whole of
///   "1px per tap" and it holds no matter what cadence the desktop is configured with.
/// * `held_for` is how long the key has been down. A repeat before [`NUDGE_HOLD_DELAY`] is
///   discarded, so the desktop's text-entry delay cannot leak into pixel nudging.
/// * `since_last_step` is how long since we last moved for this hold. A repeat closer than
///   [`NUDGE_REPEAT_INTERVAL`] is discarded, which caps the speed of a hold.
///
/// Discarding is always a no-op, never a swallow of some other meaning: these keys mean
/// "move" or they mean nothing at all on these surfaces.
pub fn nudge_step_allowed(
    is_repeat: bool,
    held_for: std::time::Duration,
    since_last_step: std::time::Duration,
) -> bool {
    if !is_repeat {
        return true;
    }
    held_for >= NUDGE_HOLD_DELAY && since_last_step >= NUDGE_REPEAT_INTERVAL
}

/// **Pure**, unit-tested: whether this directional key event starts a FRESH hold (DRAGON-601).
///
/// A real press obviously does. So does a REPEAT that arrives with no hold on record, and that
/// second case is the one worth naming: it is what a focus change mid-hold looks like, and what
/// a repeat delivered to a surface that only just appeared looks like. Treating it as a fresh
/// press measures our delay from an event we actually saw, instead of trusting an elapsed time
/// derived from a press that was delivered somewhere else.
///
/// The alternative, ignoring an orphan repeat, would strand the user: the key is already down,
/// so no further press is coming, and nudging would do nothing until they released and pressed
/// again.
pub fn nudge_rearms(has_hold: bool, is_repeat: bool) -> bool {
    !is_repeat || !has_hold
}

/// **Pure**, unit-tested: the colour picker's magnifier-zoom keys (DRAGON-587) — the NUMPAD
/// `+` and `-`, as a signed notch count, positive = zoom in.
///
/// **Deliberately NOT a [`Keymap`] entry**, for the reason [`is_region_copy_chord`] spells
/// out: everything in the keymap is user-configurable by construction, and these are not app
/// preferences, they are the universal zoom keys. Every viewer on the desktop uses `+` and
/// `-` for this, and the picker overlay owns the keyboard for the few seconds it is up, so
/// there is no other meaning to collide with and nothing for a settings page to explain.
///
/// It is keyed on the physical [`Location::Numpad`] group, which is the owner's request and
/// also the safe reading: the main-row `+` is `Shift+=` on most layouts and would have to
/// guess at the shift state, while the keypad keys are unambiguous. `KP_Add` / `KP_Subtract`
/// reach us as `Key::Character("+"/"-")` with that location on both the winit and the Wayland
/// layer-shell paths (`keysym_location`, DRAGON-364 plumbed the field through for the text
/// editor's numpad Enter; this is its second consumer). Verified in libcosmic's own keymap:
/// its `KP_Add => Named::Add` arm is commented out, so the keypad keys fall through to the
/// character path, and its round-trip table pairs `('+', Numpad)` back to `KP_Add`.
pub fn magnifier_zoom_step(key: &Key, location: Location) -> Option<i32> {
    if location != Location::Numpad {
        return None;
    }
    match key {
        Key::Character(c) if c.as_str() == "+" => Some(1),
        Key::Character(c) if c.as_str() == "-" => Some(-1),
        _ => None,
    }
}

/// **Pure**, unit-tested: the FIXED accept keys (DRAGON-612) — bare **Enter** and bare
/// **Space**, the two keys that mean "take what is on screen" everywhere else on the desktop.
///
/// **Deliberately NOT [`Keymap`] entries**, and the argument is [`nudge_direction`]'s rather
/// than a new one: accepting with Enter is an operating-system convention, not an app
/// preference, and a settings page offering to rebind it would be offering to break the only
/// thing that key means. The decisive term is that this COMPLETES the nudge. DRAGON-599 baked
/// the eight directional keys, and a loop with a baked half and a configurable half is worse
/// than either choice made whole. When the DRAGON-588 / DRAGON-589 fixed-shortcut tab lands,
/// these belong in it, listed and non-remappable, beside [`is_region_copy_chord`].
///
/// It also sidesteps a question the keymap could not answer as it stands: [`Keymap`] holds
/// exactly one [`Shortcut`] per [`Action`], and space has no [`NamedKey`] spelling at all
/// (iced's `Named` carries no `Space` variant — space is a CHARACTER key), so a bindable Space
/// would need a change to the persisted on-disk form before it could even be expressed.
/// Reducing both keys to one meaning here is the same move [`Direction`] makes for eight.
///
/// **No modifiers, exactly.** [`Modifiers::empty`] and nothing else, so `Ctrl+Enter`,
/// `Shift+Space` and the rest fall through untouched. That is also what keeps this clear of the
/// one default that names Enter at all: [`Action::RecordStop`] is `Shift+Alt+Enter`, and
/// [`Shortcut::matches`] compares all four modifier flags exactly, so the two cannot collide.
/// Pinned by `accept_key_tests`.
///
/// **Both Enters accept**, main row and keypad alike, so this takes no [`Location`]. That is
/// the opposite of the live text-annotation editor (DRAGON-364), where numpad Enter settles the
/// edit and main Enter inserts a newline: there the two keys had to mean different things, and
/// here there is only one thing to mean.
pub fn is_accept_key(modifiers: Modifiers, key: &Key) -> bool {
    if modifiers != Modifiers::empty() {
        return false;
    }
    match key {
        Key::Named(Named::Enter) => true,
        // Space is a CHARACTER key in iced's UI-Events model, not a `Named` one, so it is
        // matched on the character. Unlike [`is_character_palette_chord`]'s pair of spellings,
        // only the plain `" "` can reach here: the ASCII NUL twin is what a CONTROL-modified
        // space produces, and the guard above has already refused every modifier.
        Key::Character(c) => c.as_str() == " ",
        _ => false,
    }
}

/// The five BAKED editor actions (DRAGON-617): Save, Upload, Close, Undo and Redo.
///
/// They were ordinary [`Action`]s with ordinary [`Keymap`] entries until DRAGON-617 took them
/// out. See [`fixed_editor_action`] for why, and for the chords.
///
/// An enum rather than five predicates for the reason [`Direction`] is one: five bindings
/// that must stay in lockstep are five chances to half-change the feature, and every consumer
/// (the two dispatch sites, the five tooltips, the rebind refusal) wants the same vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixedEditorAction {
    /// Save the capture, choosing where. Opens the destination picker pre-filled with what a
    /// plain overwrite-save would have written (DRAGON-467 folded "Save As" into it).
    Save,
    /// Open the Upload FLYOUT, which picks a connected cloud account (DRAGON-482). The key
    /// opens the flyout rather than starting an upload: an upload picks a destination and
    /// cannot be undone, so a keystroke must not commit one.
    Upload,
    /// Close without deleting. On a preview this is the document's own close-or-confirm
    /// decision; on the capture overlay it dismisses the overlay.
    Close,
    /// Undo the last edit.
    Undo,
    /// Redo the last undone edit.
    Redo,
}

impl FixedEditorAction {
    /// Every baked action, in the order [`Self::index`] numbers them.
    pub const ALL: [FixedEditorAction; 5] = [
        FixedEditorAction::Save,
        FixedEditorAction::Upload,
        FixedEditorAction::Close,
        FixedEditorAction::Undo,
        FixedEditorAction::Redo,
    ];

    /// This action's position in [`Self::ALL`], for the memoized label table. Pinned by
    /// `fixed_editor_chord_tests` so the two can never drift.
    const fn index(self) -> usize {
        match self {
            FixedEditorAction::Save => 0,
            FixedEditorAction::Upload => 1,
            FixedEditorAction::Close => 2,
            FixedEditorAction::Undo => 3,
            FixedEditorAction::Redo => 4,
        }
    }

    /// The chord, as a [`Shortcut`]. Built from the SAME constructors the retired
    /// [`Action::default_shortcut`] arms used, so every one of these five is byte-identical
    /// to the binding it shipped with and nothing a user pressed before presses differently.
    fn chord(self) -> Shortcut {
        match self {
            FixedEditorAction::Save => Shortcut::primary_char('s'),
            FixedEditorAction::Upload => Shortcut::primary_char('u'),
            FixedEditorAction::Close => Shortcut::named(NamedKey::Escape),
            FixedEditorAction::Undo => Shortcut::primary_char('z'),
            FixedEditorAction::Redo => Shortcut::primary_shift_char('z'),
        }
    }

    /// A SECOND conventional spelling, where the desktop really has two.
    ///
    /// Only Redo has one, and only off macOS: `Ctrl+Y` is the redo convention on Windows
    /// (Office, Explorer) and is common on Linux (LibreOffice), beside the `Ctrl+Shift+Z`
    /// that GTK and Photoshop use. **Not `⌘Y` on macOS**, and that is the deliberate
    /// per-platform difference: ⌘Y is not redo there (Finder gives it Quick Look, Mail its
    /// own meaning), so adding it would be inventing a chord rather than honouring a
    /// convention, and honouring conventions is the entire premise of baking these five.
    ///
    /// This is what [`Keymap`] could not express, and the reason DRAGON-612's two accept keys
    /// became a matcher function too: the map holds exactly ONE [`Shortcut`] per [`Action`],
    /// so a second form was never available to the configurable version of this action.
    ///
    /// `primary_char('y')` rather than a literal Ctrl+Y because the alternate exists only
    /// where the primary modifier IS Ctrl, so the two spellings are the same thing said once.
    fn alt_chord(self) -> Option<Shortcut> {
        if !matches!(self, FixedEditorAction::Redo) {
            return None;
        }
        #[cfg(target_os = "macos")]
        {
            None
        }
        #[cfg(not(target_os = "macos"))]
        {
            Some(Shortcut::primary_char('y'))
        }
    }

    /// The DISPLAY form, for the editor tooltips: `"Ctrl+S"` / `"⌘S"`, `"Esc"`, and so on.
    ///
    /// The PRIMARY spelling only. A tooltip advertising both of Redo's forms would be teaching
    /// two keys for one button, where the job is to name the one to press.
    ///
    /// Rendered by [`Shortcut::label`], the ONE in-app chord formatter, from the same
    /// [`Self::chord`] the matcher listens on, so the key the app answers and the key the
    /// tooltip promises cannot drift. Memoized into `&'static str` for the reason
    /// [`region_copy_chord_label`] is: these reach a per-frame view and must not allocate.
    pub fn label(self) -> &'static str {
        static LABELS: std::sync::OnceLock<[String; 5]> = std::sync::OnceLock::new();
        LABELS.get_or_init(|| FixedEditorAction::ALL.map(|a| a.chord().label()))[self.index()]
            .as_str()
    }
}

/// **Pure**, unit-tested: the FIXED editor actions (DRAGON-617) — Save (primary+S), Upload
/// (primary+U), Close (Escape), Undo (primary+Z) and Redo (primary+Shift+Z, plus `Ctrl+Y` off
/// macOS).
///
/// **Deliberately NOT [`Keymap`] entries**, and the argument is [`is_region_copy_chord`]'s
/// rather than a new one: everything in the keymap is user-configurable by construction, and
/// these five are operating-system conventions, not app preferences. Nobody wants to rebind
/// Ctrl+Z; they want it to BE Ctrl+Z. That is the same line DRAGON-612 drew for the accept
/// keys and DRAGON-599 for the directional keys. When the DRAGON-588 / DRAGON-589
/// fixed-shortcut tab lands, these belong in it, listed and non-remappable, beside
/// [`is_region_copy_chord`] and [`is_accept_key`].
///
/// **Baked WINS over a user binding, so this is asked BEFORE the keymap lane.** That is the
/// opposite of where [`is_accept_key`] and [`nudge_direction`] sit, and the difference is the
/// same test the file already applies: is there a real collision to arbitrate? For those two
/// there was none, so the safe order let a configured binding win. Here there is. These five
/// chords were the DEFAULTS of the five actions this replaces, right up to this commit, so a
/// persisted keymap on disk can still name any of them for some other action. A chord that is
/// fixed has to be fixed.
///
/// The remaining exposure is small and closed at the other end: [`is_fixed_editor_chord`]
/// makes the rebind capture REFUSE one of these chords, so no new collision can be created,
/// and `no_remaining_default_collides_with_a_baked_chord` pins that a default install has
/// none. What is left is a keymap customised before this build shipped, which loses one
/// binding rather than silently doing the wrong thing.
///
/// **Escape is the one BARE key here**, and it is worth saying out loud because the other
/// four are modifier chords and the hazard is not the same. Key presses reach dispatch with no
/// `Status` filter, so a bare key can collide with typing. Escape does not, for two reasons:
/// it is not a printable key, and every modal lane in `preview_modal_key` that wants Escape
/// (the crop session, the live text edit, the unsaved-changes dialog, the colour wheel, the
/// flyout, the annotation deselect) is asked BEFORE this is. Nothing about that ordering
/// changed; this only replaces what the bottom of that function resolves to.
///
/// **No modifiers on Escape, exactly.** [`Shortcut::matches`] compares all four flags, so
/// `Shift+Escape` and the rest fall through untouched, exactly as the retired
/// `Action::PreviewCancel` binding did.
pub fn fixed_editor_action(modifiers: Modifiers, key: &Key) -> Option<FixedEditorAction> {
    FixedEditorAction::ALL.into_iter().find(|a| {
        a.chord().matches(modifiers, key)
            || a.alt_chord().is_some_and(|alt| alt.matches(modifiers, key))
    })
}

/// **Pure**, unit-tested: is `shortcut` one of the baked chords (DRAGON-617)?
///
/// The rebind capture consults this and REFUSES a match, which is the other half of
/// [`fixed_editor_action`]'s precedence. Without it a user could bind some other action to
/// Ctrl+Z, watch the row accept it, and then find the key still undoing: the binding would be
/// real, persisted and permanently dead, because the baked matcher answers first. Refusing at
/// capture time says no once, out loud, instead of failing quietly forever.
///
/// It covers Redo's SECOND form as well as its first, so `Ctrl+Y` is refused off macOS and
/// accepted on it, matching exactly what each platform actually answers.
pub fn is_fixed_editor_chord(shortcut: &Shortcut) -> bool {
    FixedEditorAction::ALL
        .iter()
        .any(|a| a.chord() == *shortcut || a.alt_chord().as_ref() == Some(shortcut))
}

/// The three BAKED scanner actions (DRAGON-627): Copy, Select all and Deselect, on the
/// RECOGNIZED TEXT the scanner overlay has read off the screen.
///
/// They were ordinary [`Action`]s with ordinary [`Keymap`] entries, and the only members of a
/// `Context::Overlay` lane, until DRAGON-627 took them out. See [`fixed_scanner_action`] for
/// why, and for the chords.
///
/// An enum rather than three predicates for the reason [`FixedEditorAction`] is one: three
/// bindings that must stay in lockstep are three chances to half-change the feature, and the
/// one consumer wants one vocabulary to match on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixedScannerAction {
    /// Copy the selected recognized text.
    Copy,
    /// Select every recognized text run in the region.
    SelectAll,
    /// Clear the recognized-text selection.
    Deselect,
}

impl FixedScannerAction {
    /// Every baked scanner action, in the order the overlay's own context menu lists them
    /// (`app::overlay::menus`: Copy, Select all, Select none).
    pub const ALL: [FixedScannerAction; 3] = [
        FixedScannerAction::Copy,
        FixedScannerAction::SelectAll,
        FixedScannerAction::Deselect,
    ];

    /// The chord, as a [`Shortcut`]. Built from the SAME constructor the retired
    /// [`Action::default_shortcut`] arms used, so every one of these three is byte-identical
    /// to the binding it shipped with and nothing a user pressed before presses differently.
    ///
    /// Deselect is `primary+D` because [`Action::PreviewDeselectAll`] is (DRAGON-369's
    /// deselect-is-one-key rule), and select-all is `primary+A` for the same reason.
    fn chord(self) -> Shortcut {
        match self {
            FixedScannerAction::Copy => Shortcut::primary_char('c'),
            FixedScannerAction::SelectAll => Shortcut::primary_char('a'),
            FixedScannerAction::Deselect => Shortcut::primary_char('d'),
        }
    }
}

/// **Pure**, unit-tested: the FIXED scanner text actions (DRAGON-627). Copy (primary+C),
/// Select all (primary+A) and Deselect (primary+D), on the text the scanner has recognized.
///
/// **Deliberately NOT [`Keymap`] entries**, and the argument is [`fixed_editor_action`]'s
/// rather than a new one: everything in the keymap is user-configurable by construction, and
/// these three are the operating system's own selection and copy conventions, not app
/// preferences. Nobody wants to rebind Ctrl+C; they want it to BE Ctrl+C. That is the same
/// line DRAGON-617 drew for save/upload/close/undo/redo, DRAGON-612 for the accept keys and
/// DRAGON-599 for the directional keys. When the DRAGON-588 / DRAGON-589 fixed-shortcut tab
/// lands, these belong in it, listed and non-remappable, beside [`is_region_copy_chord`],
/// [`is_accept_key`] and the five above.
///
/// **A SEPARATE matcher from [`fixed_editor_action`], and folding the two together would be a
/// bug, not a tidy-up.** That matcher is asked BEFORE the keymap on the preview's own modal
/// lane, and these three chords are the LIVE defaults of three actions that are still
/// configurable: [`Action::PreviewCopy`] is primary+C, [`Action::PreviewSelectAll`] primary+A
/// and [`Action::PreviewDeselectAll`] primary+D. Merged, Ctrl+C in an open preview would copy
/// recognized text instead of the picture. The two sets are kept apart by SURFACE, which is
/// exactly what the retired `Context` split expressed, and it is why these chords are also
/// absent from [`is_fixed_editor_chord`]: the rebind rows must go on accepting Ctrl+C for a
/// preview action.
///
/// **Asked where the keymap lane it replaces was asked**, at the bottom of `handle_key`'s
/// overlay run, so precedence is unchanged: the baked Close (Escape), then the region-copy
/// chord (whose `region_copy_fires` already declines in scanner kind, leaving primary+C to
/// this), then the fixed settings-search chord, then this.
///
/// **Every one of the three is a modifier CHORD, and no bare key is involved.** Key presses
/// reach dispatch with no `Status` filter, so a bare key can collide with typing and needs
/// the `press_on_overlay` delivery test the accept and directional lanes use. A primary chord
/// is not typing, which is why the region-copy chord and the five baked editor chords do not
/// consult it either, and why this does not.
///
/// **No stray modifiers.** [`Shortcut::matches`] compares all four flags, so `Ctrl+Shift+C`
/// and the rest fall through untouched, exactly as the retired keymap bindings did.
pub fn fixed_scanner_action(modifiers: Modifiers, key: &Key) -> Option<FixedScannerAction> {
    FixedScannerAction::ALL
        .into_iter()
        .find(|a| a.chord().matches(modifiers, key))
}

/// [`is_region_copy_chord`]'s DISPLAY form, for the capture toolbar's tooltip: `"Ctrl+C"` on
/// Linux and Windows, `"⌘C"` on macOS.
///
/// Rendered by [`Shortcut::label`] — the ONE in-app chord formatter, the same one `action_tip`
/// shows every configurable binding through — rather than by `mac_symbolic_spec` /
/// `win_readable_spec`, which render daemon SPEC STRINGS (a different input and a different
/// vocabulary: they say "Win" where an in-app chord says "Super"). Same per-OS symbols, right
/// source.
///
/// Memoized into a `&'static str` so the tooltip stays a borrow: `capture_button_layer` rebuilds
/// on every frame, and its other thirteen labels are `&'static str` literals that allocate
/// nothing.
pub fn region_copy_chord_label() -> &'static str {
    static LABEL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    LABEL.get_or_init(|| Shortcut::primary_char('c').label()).as_str()
}

/// **Pure**, unit-tested: is this press the colour picker window's "keep this colour"
/// chord (DRAGON-680)? Cmd+Enter on macOS, Ctrl+Enter on Linux and Windows.
///
/// **Why it is a chord at all.** Filing the shown colour into the history used to be plain
/// ENTER in a value box (DRAGON-665, on the argument that Enter is the one gesture saying
/// "this is the colour I meant"). Enter is also what you press when you finish typing a
/// number, so in practice it wrote history nobody asked for. The owner moved it onto a
/// deliberate chord and left plain Enter its other job, committing the draft so the box
/// re-renders canonically.
///
/// **FIXED rather than bindable**, the same call [`is_region_copy_chord`] makes and for the
/// same reasons: it belongs to one window rather than to a context the keymap models, and
/// primary+Enter is not a default of any [`Action`], so there is nothing to arbitrate.
/// [`Shortcut::matches`] compares all four modifier flags, so Shift+Ctrl+Enter and friends
/// fall through untouched.
pub fn is_add_color_chord(modifiers: Modifiers, key: &Key) -> bool {
    add_color_chord().matches(modifiers, key)
}

/// The chord [`is_add_color_chord`] matches and [`add_color_chord_label`] renders. ONE
/// definition, so the key handler and the button's tooltip cannot describe different keys.
fn add_color_chord() -> Shortcut {
    Shortcut::primary_named(NamedKey::Enter)
}

/// **Pure**, unit-tested: is this press the colour picker window's COPY chord (DRAGON-680)?
/// Cmd+Shift+C on macOS, Ctrl+Shift+C on Linux and Windows.
///
/// **Shift is what makes it ours, and that is the whole design.** A value box is a real
/// text input, and libcosmic's own copy-the-selection binding inside it is the BARE primary
/// chord (Cmd+C / Ctrl+C). Claiming that would mean a user who selected part of a value and
/// pressed the copy chord got the whole colour instead of the characters they had
/// highlighted. [`Shortcut::matches`] compares all four modifier flags exactly, so the bare
/// chord never reaches this and keeps doing what every text field in every app does.
///
/// FIXED rather than bindable, for [`is_region_copy_chord`]'s reasons: it belongs to one
/// window rather than to a context the keymap models. It does collide with nothing: the
/// only primary+C default is `Action::PreviewCopy`, which has no Shift, and the region-copy
/// chord is bare primary+C too.
pub fn is_copy_value_chord(modifiers: Modifiers, key: &Key) -> bool {
    copy_value_chord().matches(modifiers, key)
}

/// The chord [`is_copy_value_chord`] matches and [`copy_value_chord_label`] renders.
fn copy_value_chord() -> Shortcut {
    Shortcut::primary_shift_char('c')
}

/// [`copy_value_chord`]'s DISPLAY form, for the copy button's tooltip: `"Ctrl+Shift+C"` on
/// Linux and Windows, `"⇧⌘C"` on macOS. Memoized, like its two siblings.
pub fn copy_value_chord_label() -> &'static str {
    static LABEL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    LABEL.get_or_init(|| copy_value_chord().label()).as_str()
}

/// **Pure**, unit-tested: does this press step a window's own focus ring, and which way
/// (DRAGON-680)? `Some(true)` for a bare Tab, `Some(false)` for Shift+Tab.
///
/// Any OTHER modifier declines: Cmd+Tab and Alt+Tab belong to the desktop, and a window
/// that swallowed them would be taking the window switcher away. Ctrl+Tab declines here
/// too, and since DRAGON-687 it is not the desktop's either: it is [`tab_cycle_step`]'s
/// tab-strip cycle, a separate predicate so the two meanings of the Tab key can never
/// claim one press.
///
/// It exists because a window can need its OWN Tab order. libcosmic drives Tab through
/// `keyboard_nav::subscription` into iced's `focus_next` / `focus_previous`, which visit
/// every focusable widget, and in libcosmic a BUTTON is focusable; a window whose Tab ring
/// must skip its buttons (the colour picker's, see `color_picker::geom::next_focus`) has to
/// turn that blanket navigation off and answer Tab itself. This is the key half of that.
pub fn tab_step(modifiers: Modifiers, key: &Key) -> Option<bool> {
    if !matches!(key, Key::Named(Named::Tab)) {
        return None;
    }
    if modifiers.control() || modifiers.alt() || modifiers.logo() {
        return None;
    }
    Some(!modifiers.shift())
}

/// **Pure**, unit-tested: does this press CYCLE a window's tab strip, and which way
/// (DRAGON-687)? `Some(true)` for Ctrl+Tab, `Some(false)` for Ctrl+Shift+Tab.
///
/// **The modifier is CONTROL on every platform, macOS included.** This is the browser
/// convention, and browsers use Control for it on the Mac too (Cmd+Tab is the system's
/// own app switcher and can never reach us). So unlike the app's primary-command chords,
/// which map to Cmd on macOS, this one deliberately does not: `modifiers.control()`,
/// never `logo`.
///
/// Alt declines (Alt+Tab belongs to the desktop on two platforms), and the bare and
/// Shift-only forms are [`tab_step`]'s, the focus ring's own keys. The two predicates are
/// disjoint by construction: `tab_step` refuses Control and this requires it.
///
/// ONE predicate for both consumers, the colour picker's panel tabs and the settings
/// window's in-page strips, so the chord cannot come to mean different keys in the two
/// windows that answer it.
pub fn tab_cycle_step(modifiers: Modifiers, key: &Key) -> Option<bool> {
    if !matches!(key, Key::Named(Named::Tab)) {
        return None;
    }
    if !modifiers.control() || modifiers.alt() || modifiers.logo() {
        return None;
    }
    Some(!modifiers.shift())
}

/// **Pure**, unit-tested: the four ARROW keys as a [`Direction`], and nothing else
/// (DRAGON-680).
///
/// Deliberately narrower than [`nudge_direction`], which also accepts the vim letters `h`
/// `j` `k` `l`. This one answers presses inside a WINDOW that carries text inputs, where a
/// letter is a letter: `l` typed into a value box must insert an `l`, not move a selection.
/// Any modifier declines for the same reason a chord is not a bare key.
pub fn arrow_direction(modifiers: Modifiers, key: &Key) -> Option<Direction> {
    if modifiers.control() || modifiers.alt() || modifiers.shift() || modifiers.logo() {
        return None;
    }
    match key {
        Key::Named(Named::ArrowLeft) => Some(Direction::Left),
        Key::Named(Named::ArrowRight) => Some(Direction::Right),
        Key::Named(Named::ArrowUp) => Some(Direction::Up),
        Key::Named(Named::ArrowDown) => Some(Direction::Down),
        _ => None,
    }
}

/// [`add_color_chord`]'s DISPLAY form, for the picker window's "Add to recents" button:
/// `"Ctrl+Enter"` on Linux and Windows, `"⌘Enter"` on macOS.
///
/// Rendered by [`Shortcut::label`], the ONE in-app chord formatter (the same one
/// `action_tip` and [`region_copy_chord_label`] use), which is what puts the mac symbol
/// glyph there rather than the word "Cmd" and drops the `+` on that platform, per the
/// house style. Memoized into a `&'static str` for the same reason its sibling is: the
/// button rebuilds with the view.
pub fn add_color_chord_label() -> &'static str {
    static LABEL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    LABEL.get_or_init(|| add_color_chord().label()).as_str()
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

/// DRAGON-452: canonical form of a daemon capture-hotkey SPEC string, so two spellings of
/// the SAME physical chord compare equal. Returns `None` for a spec that binds nothing —
/// the cleared/empty state, modifiers with no key, or two different keys in one spec (which
/// the daemon parsers reject too), because none of those can collide with anything.
///
/// The canonical form is `cmd+ctrl+alt+shift+<key>`: modifiers in a FIXED order (so
/// "Shift+Ctrl+K" and "Ctrl+Shift+K" match), every token lowercased (so "ctrl+k" and
/// "Ctrl+K" match), each modifier alias folded to one name (Cmd/Command/Super/Win/Logo/Meta
/// are all the same key, and it is the one `daemon_spec` writes as "Cmd" but Windows shows
/// as "Win"), and the key token folded through the alias set the daemon parsers accept
/// ("Print"/"PrtSc" are "PrintScreen", "Esc" is "Escape", "Up" is "ArrowUp", …).
///
/// This is the ONE place chords are compared. Everything a user records arrives through
/// `Shortcut::daemon_spec`, which is already canonical, so the folding only matters for a
/// hand-edited config; it is here so a hand edit can never smuggle a duplicate past the
/// check. NOT folded: the macOS bare-PrintScreen alias (the mac daemon also registers F13
/// for it), so a slot on bare "F13" and a slot on bare "PrintScreen" are not reported as a
/// conflict even though mac registers both. That alias is a mac registration detail, not a
/// chord identity, and treating it as one would report a phantom conflict on Windows.
///
/// Compiled everywhere (pure, unit-tested on Linux) but only CALLED on the two OSes with
/// daemon-owned global hotkeys, so Linux marks it honestly dead rather than hiding it.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub fn normalized_chord(spec: &str) -> Option<String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let (mut ctrl, mut alt, mut shift, mut logo) = (false, false, false, false);
    let mut key: Option<String> = None;
    for tok in spec.split('+') {
        let t = tok.trim();
        // An EMPTY segment (the gap in "Ctrl++" or the trailing one in "Ctrl+") is skipped,
        // exactly as the daemon parsers skip it. Such a spec therefore ends up with no key
        // and binds nothing, which is the truth: neither daemon can register a "+" key
        // written this way, so two slots holding one cannot fight over a registration.
        if t.is_empty() {
            continue;
        }
        let token = t.to_ascii_lowercase();
        match token.as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" | "option" | "opt" => alt = true,
            "shift" => shift = true,
            "cmd" | "command" | "super" | "logo" | "win" | "meta" => logo = true,
            _ => {
                let folded = canonical_key_token(&token);
                match &key {
                    // A SECOND, different key is a malformed spec that registers nothing
                    // (the Windows parser rejects it outright), so it collides with nothing.
                    Some(existing) if *existing != folded => return None,
                    Some(_) => {}
                    None => key = Some(folded),
                }
            }
        }
    }
    // Modifiers with no key bind nothing.
    let key = key?;
    let mut out = String::new();
    if logo {
        out.push_str("cmd+");
    }
    if ctrl {
        out.push_str("ctrl+");
    }
    if alt {
        out.push_str("alt+");
    }
    if shift {
        out.push_str("shift+");
    }
    out.push_str(&key);
    Some(out)
}

/// Fold a lowercased spec KEY token onto its canonical name. The alias set mirrors the
/// Windows daemon parser's `key_to_vk` table (the mac parser, `global_hotkey`'s, accepts the
/// long names), so the two spellings of one physical key normalize together. Anything
/// unrecognized passes through unchanged, which is right: an unknown token still compares
/// equal to itself and unequal to everything else.
fn canonical_key_token(tok: &str) -> String {
    match tok {
        "printscreen" | "print" | "prtsc" | "snapshot" => "printscreen",
        "escape" | "esc" => "escape",
        "enter" | "return" => "enter",
        "delete" | "del" => "delete",
        "insert" | "ins" => "insert",
        "pageup" | "pgup" => "pageup",
        "pagedown" | "pgdn" => "pagedown",
        "arrowup" | "up" => "arrowup",
        "arrowdown" | "down" => "arrowdown",
        "arrowleft" | "left" => "arrowleft",
        "arrowright" | "right" => "arrowright",
        other => other,
    }
    .to_string()
}

/// DRAGON-452: which OTHER capture-hotkey slot already holds `proposed`, if any.
///
/// `specs` is every slot's current spec in slot order and `slot` is the index being recorded
/// into, so re-recording a slot to the chord it ALREADY holds is not a conflict (its own
/// entry is skipped). An unset/empty slot holds no chord and can never conflict, and neither
/// can a `proposed` that binds nothing. The comparison is by [`normalized_chord`], so a
/// different modifier ORDER or CASE still counts as the same chord.
///
/// Pure and slot-index-based on purpose: the caller owns the slot vocabulary (the settings
/// UI's `CaptureHotkeySlot`, whose `ALL` array defines this order), and this compiles and
/// unit-tests on Linux where that enum does not exist.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub fn capture_hotkey_conflict(specs: &[&str], slot: usize, proposed: &str) -> Option<usize> {
    let want = normalized_chord(proposed)?;
    for (i, other) in specs.iter().enumerate() {
        if i == slot {
            continue;
        }
        if normalized_chord(other).as_deref() == Some(want.as_str()) {
            return Some(i);
        }
    }
    None
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
                .map(|a| (a, a.default_shortcut()))
                .collect(),
        }
    }

    /// The binding for an action: `Some(shortcut)` if bound, `None` if unbound.
    pub fn get(&self, action: Action) -> Option<Shortcut> {
        match self.binds.get(&action) {
            Some(binding) => binding.clone(),
            None => action.default_shortcut(),
        }
    }

    /// Whether an action is at its default binding (an action that ships UNBOUND is at its
    /// default while it stays unbound).
    pub fn is_default(&self, action: Action) -> bool {
        self.get(action) == action.default_shortcut()
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

    /// An action's default binding, for the many tests that assert on a chord known to be
    /// bound out of the box (DRAGON-369 made the default an `Option` — five slot-member tools
    /// now ship unbound).
    fn def(a: Action) -> Shortcut {
        a.default_shortcut().unwrap_or_else(|| panic!("{a:?} ships unbound"))
    }

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

    /// DRAGON-361: the Emoji & Symbols chord is ⌃⌘Space EXACTLY — both spellings a
    /// Control-modified space arrives as count, and any extra modifier (or a different key)
    /// must fall through so the text editor still sees it.
    #[test]
    fn character_palette_chord_is_ctrl_cmd_space_exactly() {
        let both = Modifiers::CTRL | Modifiers::LOGO;
        assert!(is_character_palette_chord(both, &ch(" ")));
        assert!(is_character_palette_chord(both, &ch("\0"))); // Ctrl+Space's raw NUL
        // Missing one half of the chord.
        assert!(!is_character_palette_chord(Modifiers::CTRL, &ch(" ")));
        assert!(!is_character_palette_chord(Modifiers::LOGO, &ch(" ")));
        assert!(!is_character_palette_chord(Modifiers::empty(), &ch(" ")));
        // An EXTRA modifier is a different chord — never swallowed.
        assert!(!is_character_palette_chord(both | Modifiers::SHIFT, &ch(" ")));
        assert!(!is_character_palette_chord(both | Modifiers::ALT, &ch(" ")));
        // Right modifiers, wrong key.
        assert!(!is_character_palette_chord(both, &ch("a")));
        assert!(!is_character_palette_chord(both, &Key::Named(Named::Enter)));
    }

    #[test]
    fn matches_is_modifier_exact_and_case_insensitive() {
        let copy = def(Action::PreviewCopy); // primary+C
        assert!(copy.matches(PRIMARY, &ch("c")));
        assert!(copy.matches(PRIMARY, &ch("C"))); // case-insensitive
        assert!(!copy.matches(PRIMARY | Modifiers::SHIFT, &ch("c"))); // extra modifier
        assert!(!copy.matches(Modifiers::empty(), &ch("c"))); // missing modifier
        assert!(!copy.matches(PRIMARY, &ch("x"))); // wrong key
    }

    /// Select-all is primary+A and DESELECT IS primary+D, on EVERY surface that has one
    /// (DRAGON-369). One key, one meaning, resolved by what is on screen; the old
    /// primary+Shift+A deselect is retired and left UNBOUND rather than reassigned. This test
    /// is the stated invariant: a future deselect-shaped action must join it here.
    ///
    /// DRAGON-627 baked the scanner's pair, so half of this is now asserted through
    /// [`fixed_scanner_action`] rather than through the keymap. The RULE is unchanged and so
    /// are both chords; only where they are matched moved.
    #[test]
    fn deselect_is_primary_d_on_every_surface() {
        let km = Keymap::defaults();
        // The preview's pair, still configurable, still on the two keys.
        assert_eq!(
            km.action_for(Context::Preview, PRIMARY, &ch("a")),
            Some(Action::PreviewSelectAll)
        );
        assert_eq!(
            km.action_for(Context::Preview, PRIMARY, &ch("d")),
            Some(Action::PreviewDeselectAll)
        );
        // The chord it replaced is freed, not reassigned.
        assert_eq!(km.action_for(Context::Preview, PRIMARY | Modifiers::SHIFT, &ch("a")), None);
        // The scanner's pair, baked, on the same two keys.
        assert_eq!(
            fixed_scanner_action(PRIMARY, &ch("a")),
            Some(FixedScannerAction::SelectAll)
        );
        assert_eq!(
            fixed_scanner_action(PRIMARY, &ch("d")),
            Some(FixedScannerAction::Deselect)
        );
        assert_eq!(fixed_scanner_action(PRIMARY | Modifiers::SHIFT, &ch("a")), None);
        // Every action whose NAME says it deselects is on the key — the general rule, checked
        // by enumeration so a new one cannot quietly pick something else.
        for action in Action::ALL.into_iter().filter(|a| format!("{a:?}").contains("Deselect")) {
            let sc = km.get(action).unwrap_or_else(|| panic!("{action:?} must be bound"));
            assert!(sc.matches(PRIMARY, &ch("d")), "{action:?} must deselect on primary+D");
        }
    }

    /// Upload is primary+U in the preview (DRAGON-482), and DRAGON-617 BAKED it, so the
    /// assertion moved from the keymap to the matcher while the chord itself did not move
    /// at all.
    ///
    /// What still matters here is the thing the original test was really protecting: BARE U
    /// keeps the shape-tool slot it has held since DRAGON-369. A chord and a bare letter are
    /// different bindings, and this is the assertion that says so.
    #[test]
    fn upload_is_primary_u_and_bare_u_is_still_the_shape_slot() {
        let km = Keymap::defaults();
        assert_eq!(
            fixed_editor_action(PRIMARY, &ch("u")),
            Some(FixedEditorAction::Upload)
        );
        // No keymap lane answers it any more, in any context: it is not an `Action`.
        for ctx in [Context::Preview, Context::Recording] {
            assert_eq!(km.action_for(ctx, PRIMARY, &ch("u")), None, "{ctx:?}");
        }
        assert_eq!(
            km.action_for(Context::Preview, Modifiers::empty(), &ch("u")),
            Some(Action::PreviewAnnotShapeCycle)
        );
        // It renders through the existing per-OS label machinery with no special case.
        let label = FixedEditorAction::Upload.label();
        assert!(label.ends_with('U'), "the label should name the key: {label}");
    }

    // ── DRAGON-369: the Photoshop-parity preview keymap ──────────────────────────────

    /// THE tool map: ten keys, twelve tools, every letter a real Photoshop key or an
    /// uncontested mnemonic. Asserted as a table so a future re-map is a visible diff.
    #[test]
    fn the_preview_tool_keys_are_the_photoshop_map() {
        let km = Keymap::defaults();
        let bare = Modifiers::empty();
        for (key, action) in [
            ("v", Action::PreviewAnnotPointer),        // PS Move
            ("a", Action::PreviewAnnotArrow),          // solo: the highest-frequency tool
            ("i", Action::PreviewAnnotBadge),          // PS Count
            ("m", Action::PreviewAnnotRedactCycle),    // pixelate / blur
            ("u", Action::PreviewAnnotShapeCycle),     // PS shape tools (spotlight joined here)
            ("t", Action::PreviewAnnotText),           // PS Type
            ("b", Action::PreviewAnnotPen),            // PS Brush
            ("e", Action::PreviewAnnotEraser),         // PS Eraser
            ("h", Action::PreviewAnnotHand),           // PS Hand (DRAGON-392: a real tool)
            // Unchanged neighbours that share the row.
            ("d", Action::PreviewAnnotDuplicate),
            ("l", Action::PreviewAnnotStrokeCycle),
            ("c", Action::PreviewCrop),                // PS Crop (DRAGON-385)
            ("s", Action::PreviewColorFlyout),         // "S for swatches" (moved off C, DRAGON-385)
            ("x", Action::PreviewColorCompanionSwap), // PS foreground/background swap (DRAGON-386)
            ("w", Action::PreviewCovermark),
            ("p", Action::PreviewPlay),
        ] {
            assert_eq!(km.action_for(Context::Preview, bare, &ch(key)), Some(action), "{key}");
        }
        // The retired letters are FREE — left unbound for future tools, not reassigned. `s` left
        // this list in DRAGON-385 (it now opens the color flyout); `x` left in DRAGON-386 (the
        // companion-color swap).
        for key in ["g", "n", "q", "y", "z", "r", "o", "j", "k"] {
            assert_eq!(km.action_for(Context::Preview, bare, &ch(key)), None, "{key} must be free");
        }
        // The six slot members ship unbound — reachable only through their cycle key…
        for tool in [
            Action::PreviewAnnotPixelate,
            Action::PreviewAnnotBlur,
            Action::PreviewAnnotHighlight,
            Action::PreviewAnnotBox,
            Action::PreviewAnnotBoxHighlight,
            Action::PreviewAnnotSpotlight,
        ] {
            assert_eq!(km.get(tool), None, "{tool:?} must ship unbound");
            // …and "unbound" IS their default, so the settings row shows no stale reset.
            assert!(km.is_default(tool), "{tool:?} unbound is its default");
        }
    }

    /// DRAGON-384 migration: a keymap persisted before spotlight joined the shape slot still
    /// carries the retired `S` default as an explicit override. It must keep selecting spotlight
    /// — not crash, not be silently swallowed. It resolves directly because `PreviewAnnotSpotlight`
    /// is listed BEFORE the slot cycle in [`Action::ALL`] (the same escape hatch every slot member
    /// has), so the old `S` wins over the `U` slot and the `U` slot still cycles spotlight in too.
    #[test]
    fn an_old_spotlight_s_binding_still_selects_spotlight() {
        let bare = Modifiers::empty();
        // A fresh keymap: `S` opens the color flyout since DRAGON-385 (it moved off C); `U` is the
        // shape slot.
        let km = Keymap::defaults();
        assert_eq!(
            km.action_for(Context::Preview, bare, &ch("s")),
            Some(Action::PreviewColorFlyout)
        );
        assert_eq!(
            km.action_for(Context::Preview, bare, &ch("u")),
            Some(Action::PreviewAnnotShapeCycle)
        );
        // A pre-DRAGON-384 config persisted `PreviewAnnotSpotlight -> S` (then a default, now an
        // override). Applied literally, `S` resolves straight to spotlight: it is listed BEFORE
        // the color flyout in [`Action::ALL`], so `action_for`'s `.find()` still returns it ahead
        // of the flyout's own (now default) `S` binding.
        let mut old = Keymap::defaults();
        old.apply_overrides(&[(Action::PreviewAnnotSpotlight, Some(Shortcut::char('s')))]);
        assert_eq!(
            old.action_for(Context::Preview, bare, &ch("s")),
            Some(Action::PreviewAnnotSpotlight)
        );
        // …and the shape slot key is untouched — spotlight is reachable BOTH ways for that user.
        assert_eq!(
            old.action_for(Context::Preview, bare, &ch("u")),
            Some(Action::PreviewAnnotShapeCycle)
        );
    }

    /// The migration rule, structural: every per-TOOL action precedes both slot actions in
    /// [`Action::ALL`], so `action_for`'s `.find()` resolves a deliberate per-tool binding
    /// first BY CONSTRUCTION — which is also what makes `apply_overrides` (no de-duplication
    /// against changed defaults) a no-op for anyone who had rebound a tool.
    #[test]
    fn a_per_tool_bind_beats_its_slot_bind() {
        let pos = |a: Action| Action::ALL.iter().position(|x| *x == a).expect("in ALL");
        let slots = [Action::PreviewAnnotRedactCycle, Action::PreviewAnnotShapeCycle];
        // The twelve per-TOOL actions (one per tray tool) — the list `chrome`'s tray mirrors.
        let tools = [
            Action::PreviewAnnotPointer,
            Action::PreviewAnnotArrow,
            Action::PreviewAnnotBadge,
            Action::PreviewAnnotPixelate,
            Action::PreviewAnnotBlur,
            Action::PreviewAnnotHighlight,
            Action::PreviewAnnotBox,
            Action::PreviewAnnotBoxHighlight,
            Action::PreviewAnnotSpotlight,
            Action::PreviewAnnotText,
            Action::PreviewAnnotPen,
            Action::PreviewAnnotEraser,
        ];
        for tool in tools {
            for slot in slots {
                assert!(pos(tool) < pos(slot), "{tool:?} must precede {slot:?} in ALL");
            }
        }
        // An old config that had rebound a tool ONTO a slot key still wins there, with no
        // de-duplication pass: the ordering alone decides.
        let mut km = Keymap::defaults();
        km.apply_overrides(&[(Action::PreviewAnnotBoxHighlight, Some(Shortcut::char('u')))]);
        assert_eq!(
            km.action_for(Context::Preview, Modifiers::empty(), &ch("u")),
            Some(Action::PreviewAnnotBoxHighlight)
        );
        // The slot action is untouched (still bound) — nothing was stolen behind the user's back.
        assert_eq!(km.get(Action::PreviewAnnotShapeCycle), Some(Shortcut::char('u')));
    }

    /// DRAGON-467 retired the file-delete binding with the feature. Its chords must stay
    /// FREE rather than being reassigned opportunistically: Ctrl+X is still the natural home
    /// of a future annotation Cut, and Ctrl+Shift+X should not be silently inherited by
    /// something else just because it became available.
    #[test]
    fn the_retired_delete_chords_stay_free() {
        let km = Keymap::defaults();
        let shift = PRIMARY | Modifiers::SHIFT;
        assert_eq!(km.action_for(Context::Preview, shift, &ch("x")), None);
        assert_eq!(km.action_for(Context::Preview, PRIMARY, &ch("x")), None);
        // Nor does the capture overlay's baked set claim either of them (DRAGON-627: that
        // surface answers three chords now, and `x` is not one).
        assert_eq!(fixed_scanner_action(PRIMARY, &ch("x")), None);
        assert_eq!(fixed_scanner_action(shift, &ch("x")), None);
        // Plain Delete still belongs to the TIMELINE (deleting a segment, which is undoable
        // and touches no file). That one stays.
        assert_eq!(
            km.action_for(Context::Preview, Modifiers::empty(), &Key::Named(Named::Delete)),
            Some(Action::PreviewDeleteSegment)
        );
    }

    /// A NAMED-key binding takes no modifiers, so a modified press falls through. Asserted
    /// on the timeline's plain Delete now that Close (the historical subject, Esc) is baked;
    /// `fixed_editor_chord_tests` makes the same assertion for Escape on the matcher.
    #[test]
    fn named_key_matches_and_has_no_modifiers() {
        let del = def(Action::PreviewDeleteSegment); // Delete
        assert!(del.matches(Modifiers::empty(), &Key::Named(Named::Delete)));
        assert!(!del.matches(Modifiers::CTRL, &Key::Named(Named::Delete)));
    }

    #[test]
    fn from_event_rejects_bare_modifiers() {
        assert!(Shortcut::from_event(PRIMARY, &Key::Named(Named::Control)).is_none());
        assert_eq!(
            Shortcut::from_event(PRIMARY, &ch("c")),
            Some(def(Action::PreviewCopy))
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn label_formats_modifiers() {
        assert_eq!(def(Action::PreviewCopy).label(), "Ctrl+C");
        assert_eq!(def(Action::PreviewDeselectAll).label(), "Ctrl+D");
        // The baked five render through the very same formatter (DRAGON-617), including the
        // named-key form that has no modifier to print.
        assert_eq!(FixedEditorAction::Close.label(), "Esc");
        assert_eq!(FixedEditorAction::Upload.label(), "Ctrl+U");
    }

    /// DRAGON-581: the three recording actions default to Shift+Alt, the same chord
    /// shape on every platform (no primary-modifier split), and the exact keys the
    /// owner named. Pinned as a set because their value is being CHORDED at all: as
    /// bare M / S / Enter they were the only defaults that could collide with typing,
    /// and `xdg_trigger` refused them as global hotkeys.
    #[test]
    fn the_recording_lane_defaults_to_shift_alt() {
        for (action, key) in [
            (Action::RecordToggleMic, ShortcutKey::Char("m".into())),
            (Action::RecordToggleSystemAudio, ShortcutKey::Char("s".into())),
            (Action::RecordStop, ShortcutKey::Named(NamedKey::Enter)),
        ] {
            let sc = def(action);
            assert_eq!(sc.key, key, "{action:?} keeps its key");
            assert!(sc.shift && sc.alt, "{action:?} defaults to Shift+Alt");
            assert!(!sc.ctrl && !sc.logo, "{action:?} takes no other modifier");
            // The point of the chord: these can now be portal global shortcuts.
            assert!(sc.xdg_trigger().is_some(), "{action:?} must be globally bindable");
        }
    }

    /// Linux/Windows defaults use Ctrl (the primary command modifier), never logo,
    /// for the in-app editor chords — locking the historical bindings byte-identical.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn primary_defaults_are_ctrl_off_macos() {
        for action in [
            Action::PreviewCopy,
            Action::PreviewSelectAll,
            Action::PreviewDeselectAll,
        ] {
            let sc = def(action);
            assert!(sc.ctrl, "{action:?} should default to Ctrl off macOS");
            assert!(!sc.logo, "{action:?} should not default to logo off macOS");
        }
        // The five DRAGON-617 baked chords resolve the primary modifier the same way, and
        // they are checked here too, because leaving them out of this test is exactly how
        // the modifier could drift on one platform without anything noticing.
        for baked in FixedEditorAction::ALL {
            let sc = baked.chord();
            if matches!(sc.key, ShortcutKey::Named(_)) {
                continue; // Escape takes no modifier at all.
            }
            assert!(sc.ctrl, "{baked:?} should use Ctrl off macOS");
            assert!(!sc.logo, "{baked:?} should not use logo off macOS");
        }
        // …and so do the three DRAGON-627 baked scanner chords, for the same reason.
        for baked in FixedScannerAction::ALL {
            let sc = baked.chord();
            assert!(sc.ctrl, "{baked:?} should use Ctrl off macOS");
            assert!(!sc.logo, "{baked:?} should not use logo off macOS");
        }
    }

    /// macOS defaults route the same in-app editor chords to Cmd (logo/⌘), the
    /// native modifier, while non-modified defaults (Play/Covermark/…) stay bare.
    #[cfg(target_os = "macos")]
    #[test]
    fn primary_defaults_are_cmd_on_macos() {
        for action in [
            Action::PreviewCopy,
            Action::PreviewSelectAll,
            Action::PreviewDeselectAll,
        ] {
            let sc = def(action);
            assert!(sc.logo, "{action:?} should default to Cmd (logo) on macOS");
            assert!(!sc.ctrl, "{action:?} should not default to Ctrl on macOS");
        }
        // The DRAGON-617 baked chords resolve the primary modifier the same way.
        for baked in FixedEditorAction::ALL {
            let sc = baked.chord();
            if matches!(sc.key, ShortcutKey::Named(_)) {
                continue; // Escape takes no modifier at all.
            }
            assert!(sc.logo, "{baked:?} should use Cmd (logo) on macOS");
            assert!(!sc.ctrl, "{baked:?} should not use Ctrl on macOS");
        }
        // …and so do the three DRAGON-627 baked scanner chords.
        for baked in FixedScannerAction::ALL {
            let sc = baked.chord();
            assert!(sc.logo, "{baked:?} should use Cmd (logo) on macOS");
            assert!(!sc.ctrl, "{baked:?} should not use Ctrl on macOS");
        }
        // The shifted chord keeps Shift alongside Cmd.
        assert!(FixedEditorAction::Redo.chord().shift);
        // Bare-key defaults stay unmodified on macOS too.
        let play = def(Action::PreviewPlay);
        assert!(!play.ctrl && !play.logo && !play.alt && !play.shift);
        // macOS labels render the ⌘ glyph.
        assert_eq!(def(Action::PreviewCopy).label(), "⌘C");
        assert_eq!(FixedEditorAction::Save.label(), "⌘S");
        assert_eq!(FixedEditorAction::Upload.label(), "⌘U");
    }

    /// A persisted override (e.g. a Linux config carried to a Mac) is applied
    /// LITERALLY onto the platform defaults — its own modifiers are kept, never
    /// rewritten to the platform's primary. Only untouched actions get the
    /// platform default.
    #[test]
    fn overrides_are_literal_over_platform_defaults() {
        // A Linux-shaped override: Ctrl+K for PreviewSelectAll (logo unset).
        let ov = vec![(Action::PreviewSelectAll, Some(Shortcut::ctrl_char('k')))];
        let mut km = Keymap::defaults();
        km.apply_overrides(&ov);
        let fs = km.get(Action::PreviewSelectAll).unwrap();
        assert!(fs.ctrl && !fs.logo, "override keeps its literal Ctrl, not the platform primary");
        // An action with no override still gets the platform default.
        assert_eq!(km.get(Action::PreviewCopy), Some(def(Action::PreviewCopy)));
    }

    #[test]
    fn set_steals_on_conflict() {
        let mut km = Keymap::defaults();
        // Rebind "copy to clipboard" to primary+A, which "select all annotations" holds.
        km.set(Action::PreviewCopy, def(Action::PreviewSelectAll));
        assert_eq!(
            km.action_for(Context::Preview, PRIMARY, &ch("a")),
            Some(Action::PreviewCopy)
        );
        // Select-all loses primary+A (now unbound); Copy's old primary+C is no longer matched.
        assert_eq!(km.get(Action::PreviewSelectAll), None);
        assert_eq!(km.action_for(Context::Preview, PRIMARY, &ch("c")), None);
    }

    #[test]
    fn a_surface_and_a_context_share_a_chord_without_conflict() {
        let km = Keymap::defaults();
        // primary+C means "copy" on BOTH surfaces by default; each resolves where it lives.
        // The scanner's half is baked (DRAGON-627), the preview's is a keymap entry, and
        // `handle_key` reaches the preview's modal lane before the scanner's ever runs.
        assert_eq!(fixed_scanner_action(PRIMARY, &ch("c")), Some(FixedScannerAction::Copy));
        assert_eq!(km.action_for(Context::Preview, PRIMARY, &ch("c")), Some(Action::PreviewCopy));
        // Escape belongs to NO context now (DRAGON-617 baked Close). It is one fixed key with
        // two meanings resolved by surface in `handle_key`, which is what the keymap could
        // never express: it holds one `Shortcut` per `Action`, per context.
        let esc = Key::Named(Named::Escape);
        for ctx in [Context::Preview, Context::Recording] {
            assert_eq!(km.action_for(ctx, Modifiers::empty(), &esc), None, "{ctx:?}");
        }
        assert_eq!(
            fixed_editor_action(Modifiers::empty(), &esc),
            Some(FixedEditorAction::Close)
        );
    }

    /// DRAGON-451 retired `RegionCopy` (the region "Copy selection" quick-action), and with
    /// it the whole `Context::Region` lane. Three tests covered its shared-primary+C
    /// arrangement; what survives them is the thing that still matters — primary+C in the
    /// capture overlay is the OCR copy, unconditionally, with nothing left to take it first.
    /// DRAGON-627 baked that copy, so the assertion reads the matcher; the chord and the
    /// precedence are the same.
    #[test]
    fn primary_c_in_the_overlay_is_the_ocr_copy() {
        assert_eq!(fixed_scanner_action(PRIMARY, &ch("c")), Some(FixedScannerAction::Copy));
        let km = Keymap::defaults();
        assert_eq!(km.action_for(Context::Preview, PRIMARY, &ch("c")), Some(Action::PreviewCopy));
        // No action claims a region-draw context any more.
        assert!(!Action::ALL.iter().any(|a| a.label() == "Copy selection"));
    }

    #[test]
    fn unbind_removes_trigger_and_persists() {
        let mut km = Keymap::defaults();
        km.unbind(Action::PreviewCopy);
        assert_eq!(km.get(Action::PreviewCopy), None);
        assert_eq!(km.action_for(Context::Preview, PRIMARY, &ch("c")), None);
        // An unbind is an override and round-trips through persistence.
        let mut km2 = Keymap::defaults();
        km2.apply_overrides(&km.overrides());
        assert_eq!(km2.get(Action::PreviewCopy), None);
    }

    #[test]
    fn overrides_roundtrip() {
        let mut km = Keymap::defaults();
        assert!(km.overrides().is_empty());
        km.set(Action::PreviewSelectAll, Shortcut::ctrl_char('k'));
        let ov = km.overrides();
        let mut km2 = Keymap::defaults();
        km2.apply_overrides(&ov);
        assert_eq!(
            km2.action_for(Context::Preview, Modifiers::CTRL, &ch("k")),
            Some(Action::PreviewSelectAll)
        );
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

/// DRAGON-452: the truth table for the six global capture-hotkey slots. The bug this pins is
/// that every slot could be recorded to the SAME chord with no warning, and only one of them
/// can win the OS registration. Pure, so it runs on EVERY platform including Linux, where
/// the slots themselves do not exist.
#[cfg(test)]
mod capture_hotkey_conflict_tests {
    use super::{capture_hotkey_conflict, normalized_chord};

    /// The six slots in their fixed order (All In One / Active Window / Active Monitor, then
    /// the DRAGON-428 "(no editor)" twins), all UNSET — the fresh-install state.
    fn empty_slots() -> [&'static str; 6] {
        [""; 6]
    }

    #[test]
    fn a_fresh_chord_is_free() {
        let mut slots = empty_slots();
        slots[0] = "Ctrl+Alt+K";
        // A chord no slot holds is accepted for any slot.
        assert_eq!(capture_hotkey_conflict(&slots, 1, "Ctrl+Alt+J"), None);
        assert_eq!(capture_hotkey_conflict(&slots, 5, "PrintScreen"), None);
    }

    #[test]
    fn a_chord_another_slot_holds_conflicts_and_names_it() {
        let mut slots = empty_slots();
        slots[2] = "Ctrl+Alt+K";
        // Recording the SAME chord into any other slot reports slot 2 as the owner.
        assert_eq!(capture_hotkey_conflict(&slots, 0, "Ctrl+Alt+K"), Some(2));
        assert_eq!(capture_hotkey_conflict(&slots, 5, "Ctrl+Alt+K"), Some(2));
    }

    #[test]
    fn re_recording_a_slot_to_the_chord_it_already_holds_is_not_a_conflict() {
        let mut slots = empty_slots();
        slots[3] = "Cmd+Shift+2";
        assert_eq!(capture_hotkey_conflict(&slots, 3, "Cmd+Shift+2"), None);
        // Even spelled differently: it is still that slot's own chord.
        assert_eq!(capture_hotkey_conflict(&slots, 3, "shift+cmd+2"), None);
    }

    #[test]
    fn unset_slots_never_conflict() {
        // Every slot empty: nothing to collide with, and an empty PROPOSED chord (the row's
        // "x" clear) collides with nothing either, not even with other empty slots.
        let slots = empty_slots();
        assert_eq!(capture_hotkey_conflict(&slots, 0, "PrintScreen"), None);
        assert_eq!(capture_hotkey_conflict(&slots, 0, ""), None);
        let mut held = empty_slots();
        held[1] = "PrintScreen";
        held[4] = "Ctrl+K";
        assert_eq!(capture_hotkey_conflict(&held, 0, ""), None);
        assert_eq!(capture_hotkey_conflict(&held, 0, "   "), None);
    }

    #[test]
    fn comparison_is_by_normalized_chord_not_by_text() {
        let mut slots = empty_slots();
        slots[0] = "Ctrl+Alt+Shift+K";
        // Different modifier ORDER, different CASE, and a modifier ALIAS all still collide.
        assert_eq!(capture_hotkey_conflict(&slots, 1, "Shift+Alt+Ctrl+K"), Some(0));
        assert_eq!(capture_hotkey_conflict(&slots, 1, "ctrl+alt+shift+k"), Some(0));
        assert_eq!(capture_hotkey_conflict(&slots, 1, "control+option+shift+K"), Some(0));
        // The logo key is written "Cmd" by `daemon_spec` and shown as "Win" on Windows: the
        // same physical key, so the two spellings must collide.
        let mut logo = empty_slots();
        logo[0] = "Cmd+Shift+2";
        assert_eq!(capture_hotkey_conflict(&logo, 3, "Win+Shift+2"), Some(0));
        assert_eq!(capture_hotkey_conflict(&logo, 3, "super+shift+2"), Some(0));
        // A DIFFERENT chord is still different: one missing modifier is not a match.
        assert_eq!(capture_hotkey_conflict(&logo, 3, "Shift+2"), None);
    }

    #[test]
    fn key_aliases_fold_together() {
        let mut slots = empty_slots();
        slots[0] = "PrintScreen";
        assert_eq!(capture_hotkey_conflict(&slots, 1, "printscreen"), Some(0));
        assert_eq!(capture_hotkey_conflict(&slots, 1, "PrtSc"), Some(0));
        // A modifier on the same key is a DIFFERENT chord.
        assert_eq!(capture_hotkey_conflict(&slots, 1, "Ctrl+PrintScreen"), None);
        let mut named = empty_slots();
        named[2] = "Ctrl+ArrowUp";
        assert_eq!(capture_hotkey_conflict(&named, 0, "Ctrl+Up"), Some(2));
    }

    #[test]
    fn the_first_owner_is_reported_when_several_already_collide() {
        // A config written before this check existed can hold the same chord in several
        // slots. The lowest-numbered other owner is named; clearing it and re-recording
        // walks the rest one at a time rather than silently dropping bindings in bulk.
        let mut slots = empty_slots();
        slots[1] = "Ctrl+Alt+K";
        slots[4] = "ctrl+alt+k";
        assert_eq!(capture_hotkey_conflict(&slots, 0, "Ctrl+Alt+K"), Some(1));
    }

    #[test]
    fn specs_that_bind_nothing_normalize_to_none() {
        // Cleared, whitespace, modifiers with no key, a dangling separator, and two
        // different keys in one spec: none of these register anything on either daemon, so
        // none can conflict.
        for dead in ["", "   ", "Ctrl+", "Ctrl++", "Ctrl+Shift", "Ctrl+A+B"] {
            assert_eq!(normalized_chord(dead), None, "{dead:?} should bind nothing");
        }
        // A real chord normalizes to its canonical text.
        assert_eq!(normalized_chord("Ctrl+Alt+K").as_deref(), Some("ctrl+alt+k"));
        assert_eq!(normalized_chord("  shift + cmd + 2  ").as_deref(), Some("cmd+shift+2"));
    }
}

/// DRAGON-587: the colour picker's magnifier-zoom keys. Its own module because it pins ONE
/// predicate: which presses zoom, and, just as importantly, which do not.
#[cfg(test)]
mod magnifier_zoom_key_tests {
    use super::*;

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }

    /// The owner's request, both directions: numpad `+` zooms IN, numpad `-` zooms OUT, one
    /// notch each, which is the same notch the wheel and the trackpad send.
    #[test]
    fn the_numpad_plus_and_minus_are_one_notch_each() {
        assert_eq!(magnifier_zoom_step(&ch("+"), Location::Numpad), Some(1));
        assert_eq!(magnifier_zoom_step(&ch("-"), Location::Numpad), Some(-1));
    }

    /// The MAIN-ROW `+` and `-` are not these keys. On most layouts the main `+` is `Shift+=`,
    /// so honouring it would mean guessing at a shift state; the keypad group is unambiguous,
    /// which is why the request named it.
    #[test]
    fn the_main_row_keys_are_left_alone() {
        for loc in [Location::Standard, Location::Left, Location::Right] {
            assert_eq!(magnifier_zoom_step(&ch("+"), loc), None, "{loc:?}");
            assert_eq!(magnifier_zoom_step(&ch("-"), loc), None, "{loc:?}");
        }
    }

    /// Nothing else on the keypad zooms — not its digits, not its Enter (which the live text
    /// editor claims, DRAGON-364), not its other operators. A predicate that fired on "any
    /// numpad press" would swallow keys other lanes own.
    #[test]
    fn no_other_keypad_key_zooms() {
        for other in ["0", "5", "*", "/", ".", "="] {
            assert_eq!(magnifier_zoom_step(&ch(other), Location::Numpad), None, "{other}");
        }
        assert_eq!(magnifier_zoom_step(&Key::Named(Named::Enter), Location::Numpad), None);
        assert_eq!(magnifier_zoom_step(&Key::Named(Named::Escape), Location::Numpad), None);
    }

    /// It is NOT a keymap entry and must not become one: these are the universal zoom keys,
    /// not an app preference (the `is_region_copy_chord` reasoning). Pinned by checking that
    /// no action's DEFAULT binding is a bare `+` or `-`, so the picker's lane cannot be
    /// swallowing a binding that already means something else.
    #[test]
    fn no_keymap_action_claims_these_keys() {
        let map = Keymap::default();
        for action in Action::ALL {
            if let Some(sc) = map.get(action) {
                assert!(
                    !sc.matches(Modifiers::default(), &ch("+"))
                        && !sc.matches(Modifiers::default(), &ch("-")),
                    "{action:?} would collide with the picker's zoom keys"
                );
            }
        }
    }
}

/// DRAGON-599: the fixed directional keys, and the one thing both families reduce to.
#[cfg(test)]
mod nudge_direction_tests {
    use super::*;

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }

    fn named(n: Named) -> Key {
        Key::Named(n)
    }

    /// THE test this module exists for: `h` `j` `k` `l` are the four arrows, exactly. Written
    /// as a paired table rather than as two lists, so the two families cannot drift apart
    /// without this failing.
    #[test]
    fn the_vim_letters_are_the_arrows() {
        let pairs = [
            (Named::ArrowLeft, "h", Direction::Left),
            (Named::ArrowDown, "j", Direction::Down),
            (Named::ArrowUp, "k", Direction::Up),
            (Named::ArrowRight, "l", Direction::Right),
        ];
        for (arrow, letter, want) in pairs {
            let from_arrow = nudge_direction(Modifiers::empty(), &named(arrow));
            let from_letter = nudge_direction(Modifiers::empty(), &ch(letter));
            assert_eq!(from_arrow, Some(want), "{arrow:?}");
            assert_eq!(from_letter, from_arrow, "'{letter}' must be {arrow:?}");
        }
    }

    /// Screen orientation, stated once: x grows right and y grows DOWN, so Up is negative y.
    /// Every consumer works in a space with that convention, so getting it backwards here
    /// would invert movement everywhere at once.
    #[test]
    fn a_step_is_one_unit_in_screen_orientation() {
        assert_eq!(Direction::Left.delta(), (-1, 0));
        assert_eq!(Direction::Right.delta(), (1, 0));
        assert_eq!(Direction::Up.delta(), (0, -1));
        assert_eq!(Direction::Down.delta(), (0, 1));
    }

    /// Caps Lock is not one of the four modifier flags, so a shouting keyboard still moves.
    #[test]
    fn the_letters_are_case_insensitive() {
        assert_eq!(nudge_direction(Modifiers::empty(), &ch("H")), Some(Direction::Left));
        assert_eq!(nudge_direction(Modifiers::empty(), &ch("J")), Some(Direction::Down));
        assert_eq!(nudge_direction(Modifiers::empty(), &ch("K")), Some(Direction::Up));
        assert_eq!(nudge_direction(Modifiers::empty(), &ch("L")), Some(Direction::Right));
    }

    /// A MODIFIED arrow or letter is somebody else's. `Ctrl+H` must not move anything, and
    /// neither must `Shift+Left` — which also leaves a modified arrow free for a future
    /// bigger step.
    #[test]
    fn any_modifier_declines() {
        for m in [Modifiers::CTRL, Modifiers::ALT, Modifiers::SHIFT, Modifiers::LOGO] {
            for key in [named(Named::ArrowLeft), ch("h"), named(Named::ArrowUp), ch("k")] {
                assert_eq!(nudge_direction(m, &key), None, "{m:?} + {key:?}");
            }
        }
        // And a combination, in case one flag were ever tested instead of all four.
        assert_eq!(
            nudge_direction(Modifiers::CTRL | Modifiers::SHIFT, &ch("l")),
            None
        );
    }

    /// "Just the directionals" is a LIMIT (the owner's word). No other vim motion, no counts,
    /// and no other named key, so this lane can never swallow a press that means something
    /// else on the surfaces that consult it.
    #[test]
    fn nothing_else_is_a_direction() {
        for other in ["g", "w", "b", "0", "$", "y", "d", "v", "u", "i", "x", "m", "t", "e"] {
            assert_eq!(nudge_direction(Modifiers::empty(), &ch(other)), None, "{other}");
        }
        for other in [Named::Enter, Named::Escape, Named::Tab, Named::Home, Named::PageUp] {
            assert_eq!(nudge_direction(Modifiers::empty(), &named(other)), None, "{other:?}");
        }
    }

    /// These are FIXED bindings, outside the keymap, so the surfaces that consult them must
    /// not already answer the same bare key. Pinned against the Recording context's DEFAULTS
    /// and against the capture overlay's own baked chords (DRAGON-627 replaced that surface's
    /// keymap lane with [`fixed_scanner_action`], so its half of this check reads the matcher
    /// now); the preview's bare annotation letters are `Context::Preview`, which a preview's
    /// own modal lane answers long before either nudge lane is reached.
    #[test]
    fn no_overlay_or_recording_binding_claims_a_bare_directional() {
        let map = Keymap::default();
        let keys = [
            named(Named::ArrowLeft),
            named(Named::ArrowRight),
            named(Named::ArrowUp),
            named(Named::ArrowDown),
            ch("h"),
            ch("j"),
            ch("k"),
            ch("l"),
        ];
        for action in Action::ALL {
            if action.context() != Context::Recording {
                continue;
            }
            let Some(sc) = map.get(action) else { continue };
            for key in &keys {
                assert!(
                    !sc.matches(Modifiers::empty(), key),
                    "{action:?} would collide with the directional {key:?}"
                );
            }
        }
        for key in &keys {
            assert!(
                fixed_scanner_action(Modifiers::empty(), key).is_none(),
                "the capture overlay's baked set would collide with the directional {key:?}"
            );
        }
    }
}

/// DRAGON-617: the five BAKED editor chords, Save / Upload / Close / Undo / Redo.
#[cfg(test)]
mod fixed_editor_chord_tests {
    use super::*;

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }

    /// The platform PRIMARY command modifier, matching the main test module's.
    #[cfg(target_os = "macos")]
    const PRIMARY: Modifiers = Modifiers::LOGO;
    #[cfg(not(target_os = "macos"))]
    const PRIMARY: Modifiers = Modifiers::CTRL;

    /// THE table: every baked chord, and the one action it means. Written out as a table
    /// rather than derived from `chord()`, so a change to a chord is a visible diff here and
    /// not a test that silently agrees with whatever the code now says.
    #[test]
    fn every_baked_chord_maps_to_its_one_action() {
        let esc = Key::Named(Named::Escape);
        let cases: [(Modifiers, Key, FixedEditorAction); 5] = [
            (PRIMARY, ch("s"), FixedEditorAction::Save),
            (PRIMARY, ch("u"), FixedEditorAction::Upload),
            (Modifiers::empty(), esc, FixedEditorAction::Close),
            (PRIMARY, ch("z"), FixedEditorAction::Undo),
            (PRIMARY | Modifiers::SHIFT, ch("z"), FixedEditorAction::Redo),
        ];
        for (m, key, want) in cases {
            assert_eq!(fixed_editor_action(m, &key), Some(want), "{m:?} + {key:?}");
        }
    }

    /// No two baked chords collide, and every action in the enum is reachable. The pair of
    /// properties the table above cannot prove on its own: it could name the same chord twice
    /// and still pass every individual case.
    #[test]
    fn the_five_are_distinct_and_all_reachable() {
        let mut chords: Vec<Shortcut> =
            FixedEditorAction::ALL.iter().map(|a| a.chord()).collect();
        let before = chords.len();
        chords.sort_by_key(|c| format!("{c:?}"));
        chords.dedup();
        assert_eq!(chords.len(), before, "two baked actions share a chord");
        // Every alternate form is distinct from every primary form too.
        for a in FixedEditorAction::ALL {
            if let Some(alt) = a.alt_chord() {
                assert!(
                    FixedEditorAction::ALL.iter().all(|b| b.chord() != alt),
                    "{a:?}'s alternate collides with a primary chord"
                );
            }
        }
        // Each action's own chord resolves back to it, which is what "reachable" means.
        for a in FixedEditorAction::ALL {
            let sc = a.chord();
            assert!(is_fixed_editor_chord(&sc), "{a:?} is not recognised as baked");
        }
    }

    /// `ALL` and `index` are two statements of one order, and the memoized label table reads
    /// the second to index the first. Drift between them would silently label Undo "Ctrl+U".
    #[test]
    fn the_index_matches_the_all_order() {
        for (i, a) in FixedEditorAction::ALL.into_iter().enumerate() {
            assert_eq!(a.index(), i, "{a:?}");
        }
        // And the labels really line up with their actions, not just with their slots.
        for a in FixedEditorAction::ALL {
            assert_eq!(a.label(), a.chord().label(), "{a:?}");
        }
    }

    /// Redo's SECOND conventional form, and the deliberate per-platform difference.
    ///
    /// `Ctrl+Y` is the Windows redo convention (Office, Explorer) and common on Linux
    /// (LibreOffice). `⌘Y` is NOT redo on macOS (Finder gives it Quick Look), so adding it
    /// there would be inventing a chord rather than honouring a convention, and honouring
    /// conventions is the entire premise of baking these five.
    #[test]
    fn redo_takes_ctrl_y_off_macos_and_never_on_it() {
        let got = fixed_editor_action(PRIMARY, &ch("y"));
        if cfg!(target_os = "macos") {
            assert_eq!(got, None, "Cmd+Y must not be redo on macOS");
        } else {
            assert_eq!(got, Some(FixedEditorAction::Redo));
        }
        // Either way the SHIFTED form works everywhere: it is the primary spelling and the
        // only one macOS has.
        assert_eq!(
            fixed_editor_action(PRIMARY | Modifiers::SHIFT, &ch("z")),
            Some(FixedEditorAction::Redo)
        );
        // The tooltip advertises the primary form only, on every platform.
        assert_eq!(FixedEditorAction::Redo.label(), FixedEditorAction::Redo.chord().label());
    }

    /// Modifiers are compared EXACTLY, so a stray flag falls through to whatever else wants
    /// the key. Escape is the case that matters most: it is the one bare key in the set, and
    /// `Shift+Escape` must not close a document.
    #[test]
    fn a_stray_modifier_declines() {
        let esc = Key::Named(Named::Escape);
        for m in [Modifiers::CTRL, Modifiers::ALT, Modifiers::SHIFT, Modifiers::LOGO] {
            assert_eq!(fixed_editor_action(m, &esc), None, "{m:?} + Escape");
        }
        // Undo is primary+Z and nothing else: bare Z and primary+Alt+Z both fall through.
        assert_eq!(fixed_editor_action(Modifiers::empty(), &ch("z")), None);
        assert_eq!(fixed_editor_action(PRIMARY | Modifiers::ALT, &ch("z")), None);
        // Undo and Redo differ by Shift alone, so this is the pair most at risk of merging.
        assert_eq!(fixed_editor_action(PRIMARY, &ch("z")), Some(FixedEditorAction::Undo));
    }

    /// **A baked EDITOR chord wins over the keymap, so no remaining DEFAULT may claim one.**
    ///
    /// This is what keeps a default install free of the one residual the design accepts (a
    /// keymap customised before this build shipped can still name a baked chord for some
    /// other action, and loses that binding). Checked by enumeration over every surviving
    /// action in every context, so a future default cannot quietly land on Ctrl+Z.
    ///
    /// **The DRAGON-627 scanner chords are deliberately NOT part of this check, and that is
    /// the interesting half.** They are baked too, but on a different SURFACE: the capture
    /// overlay, matched at the bottom of `handle_key`'s overlay run, where a preview binding
    /// can never be reached (a preview owns the keyboard through its own modal lane, which
    /// returns first). So primary+C, primary+A and primary+D remain the live defaults of
    /// `PreviewCopy`, `PreviewSelectAll` and `PreviewDeselectAll`, and asserting them free
    /// here would be asserting the opposite of what the feature wants.
    /// `fixed_scanner_chord_tests` pins that separation from the other end.
    #[test]
    fn no_remaining_default_collides_with_a_baked_chord() {
        for action in Action::ALL {
            let Some(sc) = action.default_shortcut() else {
                continue; // ships unbound
            };
            assert!(
                !is_fixed_editor_chord(&sc),
                "{action:?} defaults to {}, which DRAGON-617 baked",
                sc.label()
            );
        }
        // The two baked SETS must not collide with EACH OTHER either. They are matched on
        // different surfaces, so a shared chord would not be a crash, it would be one key
        // quietly meaning two things depending on what is on screen, which is precisely the
        // ambiguity both sets exist to remove.
        for scanner in FixedScannerAction::ALL {
            assert!(
                !is_fixed_editor_chord(&scanner.chord()),
                "{scanner:?} takes {}, which DRAGON-617 already baked",
                scanner.chord().label()
            );
        }
    }

    /// The rebind capture's refusal list is exactly the baked set, no more and no less.
    ///
    /// The "no less" half is the interesting one: a chord that fires but is not refused is a
    /// binding the settings page would happily accept and then never honour, which is the
    /// silent-death shape this refusal exists to prevent.
    #[test]
    fn the_refusal_covers_every_chord_that_fires_and_nothing_else() {
        for a in FixedEditorAction::ALL {
            assert!(is_fixed_editor_chord(&a.chord()), "{a:?}");
            if let Some(alt) = a.alt_chord() {
                assert!(is_fixed_editor_chord(&alt), "{a:?} alternate");
            }
        }
        // An ordinary preview binding is NOT refused, or the page would reject half the keys
        // a user might reasonably choose.
        for action in [Action::PreviewCopy, Action::PreviewCovermark, Action::PreviewCrop] {
            let sc = action.default_shortcut().expect("these three ship bound");
            assert!(!is_fixed_editor_chord(&sc), "{action:?} must stay bindable");
        }
        // Ctrl+Y is refused exactly where it fires.
        let ctrl_y = Shortcut::primary_char('y');
        assert_eq!(is_fixed_editor_chord(&ctrl_y), !cfg!(target_os = "macos"));
    }
}

/// DRAGON-627: the three BAKED scanner chords, Copy / Select all / Deselect.
#[cfg(test)]
mod fixed_scanner_chord_tests {
    use super::*;

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }

    /// The platform PRIMARY command modifier, matching the main test module's.
    #[cfg(target_os = "macos")]
    const PRIMARY: Modifiers = Modifiers::LOGO;
    #[cfg(not(target_os = "macos"))]
    const PRIMARY: Modifiers = Modifiers::CTRL;

    /// THE table: every baked scanner chord, and the one action it means. Written out rather
    /// than derived from `chord()`, so a change to a chord is a visible diff here and not a
    /// test that silently agrees with whatever the code now says.
    ///
    /// These are the DEFAULTS the retired `Action::CopyText` / `SelectAllText` /
    /// `DeselectText` shipped with, unchanged, so nothing a user pressed before presses
    /// differently now.
    #[test]
    fn every_baked_scanner_chord_maps_to_its_one_action() {
        let cases: [(Modifiers, Key, FixedScannerAction); 3] = [
            (PRIMARY, ch("c"), FixedScannerAction::Copy),
            (PRIMARY, ch("a"), FixedScannerAction::SelectAll),
            (PRIMARY, ch("d"), FixedScannerAction::Deselect),
        ];
        for (m, key, want) in cases {
            assert_eq!(fixed_scanner_action(m, &key), Some(want), "{m:?} + {key:?}");
        }
    }

    /// No two of the three collide, and every action in the enum is reachable. The pair of
    /// properties the table above cannot prove on its own: it could name the same chord twice
    /// and still pass every individual case.
    #[test]
    fn the_three_are_distinct_and_all_reachable() {
        let mut chords: Vec<Shortcut> =
            FixedScannerAction::ALL.iter().map(|a| a.chord()).collect();
        let before = chords.len();
        chords.sort_by_key(|c| format!("{c:?}"));
        chords.dedup();
        assert_eq!(chords.len(), before, "two baked scanner actions share a chord");
        // Each action's own chord resolves back to it, which is what "reachable" means. All
        // three are primary chords, so the modifiers are the platform primary and nothing
        // else; `a_stray_modifier_declines` pins that from the other side.
        for a in FixedScannerAction::ALL {
            assert_eq!(
                fixed_scanner_action(PRIMARY, &a.chord().iced_key()),
                Some(a),
                "{a:?} does not resolve back to itself"
            );
        }
    }

    /// **The separation from the DRAGON-617 set, asserted from both ends.**
    ///
    /// These three chords must NOT be refused by the rebind capture, because they are the
    /// live defaults of three actions that are still configurable. Folding them into
    /// `is_fixed_editor_chord` would make the Keyboard Shortcuts page reject Ctrl+C for
    /// "Copy to clipboard", the very binding it ships with.
    #[test]
    fn the_scanner_chords_stay_bindable_for_the_preview() {
        for a in FixedScannerAction::ALL {
            assert!(
                !is_fixed_editor_chord(&a.chord()),
                "{a:?}'s chord must stay bindable: a preview action holds it by default"
            );
        }
        // Named explicitly, so a future rename cannot quietly drop one from the check.
        for (action, scanner) in [
            (Action::PreviewCopy, FixedScannerAction::Copy),
            (Action::PreviewSelectAll, FixedScannerAction::SelectAll),
            (Action::PreviewDeselectAll, FixedScannerAction::Deselect),
        ] {
            let sc = action.default_shortcut().expect("these three ship bound");
            assert_eq!(sc, scanner.chord(), "{action:?} shares {scanner:?}'s chord by design");
            assert!(!is_fixed_editor_chord(&sc), "{action:?} must stay bindable");
        }
    }

    /// Modifiers are compared EXACTLY, so a stray flag falls through to whatever else wants
    /// the key. That is also what keeps these clear of the bare directional keys and the bare
    /// accept keys, which take no modifier at all.
    #[test]
    fn a_stray_modifier_declines() {
        for key in ["c", "a", "d"] {
            assert_eq!(fixed_scanner_action(Modifiers::empty(), &ch(key)), None, "bare {key}");
            assert_eq!(
                fixed_scanner_action(PRIMARY | Modifiers::SHIFT, &ch(key)),
                None,
                "shifted {key}"
            );
            assert_eq!(
                fixed_scanner_action(PRIMARY | Modifiers::ALT, &ch(key)),
                None,
                "alted {key}"
            );
        }
        // A key none of them claims falls through.
        assert_eq!(fixed_scanner_action(PRIMARY, &ch("q")), None);
        assert_eq!(fixed_scanner_action(Modifiers::empty(), &Key::Named(Named::Escape)), None);
    }

    /// **No `Action` is left on the capture overlay**, which is the whole reason the settings
    /// page's Capture tab disappears. `ShortcutsTab::occupied` enumerates `Action::ALL` and
    /// finds nothing for that tab; this asserts the fact that makes it so, from the shortcuts
    /// side, where a new action would be added.
    #[test]
    fn nothing_in_the_keymap_belongs_to_the_capture_overlay_any_more() {
        for action in Action::ALL {
            assert!(
                matches!(action.context(), Context::Preview | Context::Recording),
                "{action:?} claims a surface the keymap no longer serves"
            );
        }
        // The three retired group titles are gone with them, so no section can route to the
        // Capture tab.
        assert!(
            !Action::ALL.iter().any(|a| a.group() == "OCR Text Recognition"),
            "the OCR group must be retired with its actions"
        );
    }
}

/// DRAGON-612: THE accept keys, bare Enter and bare Space.
///
/// This module proves the KEY half of the feature; the other half, that the surface really is
/// in a state that can accept, is `app::keyboard`'s `accept_verdict_tests`.
#[cfg(test)]
mod accept_key_tests {
    use super::*;

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }

    /// The whole of the feature at the key level: both keys, no modifiers, one meaning.
    #[test]
    fn bare_enter_and_bare_space_accept() {
        assert!(is_accept_key(Modifiers::empty(), &Key::Named(Named::Enter)));
        assert!(is_accept_key(Modifiers::empty(), &ch(" ")));
    }

    /// A MODIFIED Enter or Space belongs to somebody else. This is also the term that keeps
    /// the accept clear of [`Action::RecordStop`], whose default is `Shift+Alt+Enter`: proved
    /// against the real `Shortcut` rather than against a hand-written modifier set, so a
    /// change to that default cannot pass this test while breaking the app.
    #[test]
    fn any_modifier_declines_and_record_stop_keeps_its_chord() {
        for m in [Modifiers::CTRL, Modifiers::ALT, Modifiers::SHIFT, Modifiers::LOGO] {
            for key in [Key::Named(Named::Enter), ch(" ")] {
                assert!(!is_accept_key(m, &key), "{m:?} + {key:?}");
            }
        }
        let shift_alt = Modifiers::SHIFT | Modifiers::ALT;
        let enter = Key::Named(Named::Enter);
        assert!(!is_accept_key(shift_alt, &enter));
        let stop = Action::RecordStop.default_shortcut().expect("RecordStop ships bound");
        assert!(
            stop.matches(shift_alt, &enter),
            "Shift+Alt+Enter must still stop a recording"
        );
        assert!(
            !stop.matches(Modifiers::empty(), &enter),
            "a bare Enter must never stop a recording"
        );
    }

    /// No OTHER key accepts. The near misses are the ones worth naming: Escape is the cancel
    /// on these very surfaces, and the arrows are the nudge this feature completes, so any of
    /// them leaking into the accept would commit a capture the user was still aiming.
    #[test]
    fn no_other_key_accepts() {
        for named in [
            Named::Escape,
            Named::Tab,
            Named::Backspace,
            Named::Delete,
            Named::ArrowLeft,
            Named::ArrowRight,
            Named::ArrowUp,
            Named::ArrowDown,
        ] {
            assert!(!is_accept_key(Modifiers::empty(), &Key::Named(named)), "{named:?}");
        }
        // The vim directionals and the picker's zoom characters, all bare, all not an accept.
        for c in ["h", "j", "k", "l", "+", "-", "a", "c", "\0", ""] {
            assert!(!is_accept_key(Modifiers::empty(), &ch(c)), "{c:?}");
        }
    }

    /// The accept keys and the NUDGE keys are disjoint, stated as a crossing rather than as
    /// two lists. They arrive at the same fall-through lanes in `app::keyboard`, so one key
    /// answering both would make the dispatch order load-bearing, which it must not be.
    #[test]
    fn the_accept_keys_are_disjoint_from_the_nudge_keys() {
        let keys = [
            Key::Named(Named::Enter),
            ch(" "),
            Key::Named(Named::ArrowLeft),
            Key::Named(Named::ArrowRight),
            Key::Named(Named::ArrowUp),
            Key::Named(Named::ArrowDown),
            ch("h"),
            ch("j"),
            ch("k"),
            ch("l"),
        ];
        for key in keys {
            let accepts = is_accept_key(Modifiers::empty(), &key);
            let nudges = nudge_direction(Modifiers::empty(), &key).is_some();
            assert!(!(accepts && nudges), "{key:?} cannot be both an accept and a nudge");
        }
    }

    /// NUMPAD Enter accepts too. The picker's zoom keys are keyed on `Location::Numpad`
    /// (DRAGON-587) and the text editor tells the two Enters apart (DRAGON-364), so the
    /// keypad group is live in this codebase and "both Enters" has to be a decision rather
    /// than an accident. This function takes no `Location` at all, which IS that decision.
    #[test]
    fn the_keypad_enter_accepts_as_well() {
        // Same logical key on both the winit and the Wayland layer-shell paths; only the
        // `Location` differs, and this predicate never asks for it.
        assert!(is_accept_key(Modifiers::empty(), &Key::Named(Named::Enter)));
        assert_eq!(magnifier_zoom_step(&Key::Named(Named::Enter), Location::Numpad), None);
    }
}

/// DRAGON-601: the nudge CADENCE. The owner asked for "1px per tap, but hold to move should be
/// available too", and the two halves of that sentence pull in opposite directions, so the rule
/// that separates them is worth pinning exhaustively.
#[cfg(test)]
mod nudge_cadence_tests {
    use super::*;
    use std::time::Duration;

    const ZERO: Duration = Duration::ZERO;
    /// Longer than any deliberate tap, shorter than the hold delay.
    const LONGISH_TAP: Duration = Duration::from_millis(250);

    /// THE first half of the requirement, and the one the owner reported broken: a real press
    /// moves, exactly once, no matter how the rest of the world is configured. Neither timer
    /// can veto it, which is why the press arm returns before either is consulted.
    #[test]
    fn a_press_always_moves_exactly_once() {
        for held in [ZERO, LONGISH_TAP, NUDGE_HOLD_DELAY, Duration::from_secs(30)] {
            for since in [ZERO, Duration::from_millis(1), Duration::from_secs(30)] {
                assert!(
                    nudge_step_allowed(false, held, since),
                    "a press must move (held={held:?} since={since:?})"
                );
            }
        }
    }

    /// A repeat that arrives before OUR delay is discarded, however early it turns up. This is
    /// what makes a tap exact on a desktop whose text-entry repeat delay is short: the owner's
    /// session announces 600ms, but the build must behave the same on one announcing 150.
    #[test]
    fn an_early_repeat_never_moves() {
        for held in [ZERO, Duration::from_millis(150), LONGISH_TAP] {
            assert!(held < NUDGE_HOLD_DELAY, "{held:?} must be inside the tap window");
            assert!(
                !nudge_step_allowed(true, held, Duration::from_secs(30)),
                "a repeat at {held:?} is still part of a tap"
            );
        }
    }

    /// The second half: once the key really has been held, repeats DO move. Hold-to-move is a
    /// feature, not noise to discard, so the delay must open rather than close the door.
    #[test]
    fn a_held_key_moves_once_the_delay_has_passed() {
        assert!(nudge_step_allowed(true, NUDGE_HOLD_DELAY, NUDGE_REPEAT_INTERVAL));
        assert!(nudge_step_allowed(
            true,
            NUDGE_HOLD_DELAY + Duration::from_secs(5),
            NUDGE_REPEAT_INTERVAL
        ));
    }

    /// The interval is a CEILING on speed. A repeat closer than it is dropped, so a compositor
    /// configured to fire very fast cannot make the region skate away from the user.
    #[test]
    fn the_interval_caps_how_fast_a_hold_can_step() {
        let held = NUDGE_HOLD_DELAY + Duration::from_secs(1);
        assert!(!nudge_step_allowed(true, held, ZERO), "two steps in the same instant");
        assert!(!nudge_step_allowed(
            true,
            held,
            NUDGE_REPEAT_INTERVAL - Duration::from_millis(1)
        ));
        assert!(nudge_step_allowed(true, held, NUDGE_REPEAT_INTERVAL), "exactly at the interval");
    }

    /// A fresh hold starts on every real press, and also on an ORPHAN repeat, one that turns up
    /// with no press on record. That is what a focus change mid-hold looks like, and ignoring it
    /// would strand the user: the key is already down, so no further press is coming.
    #[test]
    fn a_press_or_an_orphan_repeat_starts_a_fresh_hold() {
        assert!(nudge_rearms(false, false), "a press with no hold");
        assert!(nudge_rearms(true, false), "a press always re-arms, even mid-hold");
        assert!(nudge_rearms(false, true), "an orphan repeat is treated as a press");
        assert!(!nudge_rearms(true, true), "a repeat during a known hold continues it");
    }

    /// The measured cadence of the owner's own session, `repeat_info(rate = 25, delay = 600)`,
    /// played through the gate as the compositor would deliver it. Two properties matter and
    /// both are checked: nothing repeats during a tap, and once the hold is real EVERY repeat
    /// the compositor sends is honoured, so the cap does not make their hold feel sluggish.
    #[test]
    fn the_measured_cosmic_cadence_taps_once_and_holds_smoothly() {
        // cosmic-comp's announced defaults, measured with `wayland-info -i wl_seat`.
        let compositor_delay = Duration::from_millis(600);
        let compositor_period = Duration::from_millis(1000 / 25);
        // A tap: one press, released long before the compositor would repeat at all.
        assert!(nudge_step_allowed(false, ZERO, Duration::from_secs(30)));
        // The compositor's first repeat, at 600ms, is past our 400ms delay, so the hold starts.
        assert!(compositor_delay >= NUDGE_HOLD_DELAY, "our delay must not out-wait the desktop");
        assert!(nudge_step_allowed(true, compositor_delay, Duration::from_secs(30)));
        // And every subsequent repeat clears the cap, so the hold runs at the desktop's rate.
        assert!(
            compositor_period >= NUDGE_REPEAT_INTERVAL,
            "the cap must not bite at the common 25/s cadence"
        );
        let mut held = compositor_delay;
        for _ in 0..10 {
            held += compositor_period;
            assert!(
                nudge_step_allowed(true, held, compositor_period),
                "a steady 25/s hold dropped a step at {held:?}"
            );
        }
    }
}

/// DRAGON-680: the colour picker window's "keep this colour" chord. It TOOK an action away
/// from plain Enter, so what these pin is both halves: the chord fires, and the bare key it
/// replaced does not.
#[cfg(test)]
mod add_color_chord_tests {
    use super::*;

    /// The primary command modifier on this platform: Cmd on macOS, Ctrl elsewhere. The
    /// same rule every other in-app default follows (`Shortcut::primary_char`).
    fn primary() -> Modifiers {
        #[cfg(target_os = "macos")]
        {
            Modifiers::LOGO
        }
        #[cfg(not(target_os = "macos"))]
        {
            Modifiers::CTRL
        }
    }

    /// The chord fires on the primary modifier plus Enter, and on nothing else.
    #[test]
    fn the_primary_enter_chord_fires() {
        let enter = Key::Named(Named::Enter);
        assert!(is_add_color_chord(primary(), &enter));
        // The OTHER platform's modifier must not: a Linux config carried to a Mac (or the
        // reverse) would otherwise give one machine two ways to file a colour and the
        // other none.
        let other = if primary() == Modifiers::CTRL { Modifiers::LOGO } else { Modifiers::CTRL };
        assert!(!is_add_color_chord(other, &enter), "the other platform's modifier declines");
    }

    /// **BARE Enter does not file a colour**, which is the whole reason this chord exists
    /// (DRAGON-680 took that off plain Enter, which is pressed while typing). Plain Enter
    /// still commits the draft, but that is the text input's own `on_submit` and never
    /// reaches this predicate.
    #[test]
    fn bare_enter_never_files_a_colour() {
        assert!(!is_add_color_chord(Modifiers::empty(), &Key::Named(Named::Enter)));
    }

    /// No STRAY modifiers: `Shortcut::matches` compares all four flags, so a chord with
    /// Shift or Alt added belongs to somebody else and falls through untouched.
    #[test]
    fn a_stray_modifier_declines() {
        let enter = Key::Named(Named::Enter);
        for extra in [Modifiers::SHIFT, Modifiers::ALT] {
            assert!(!is_add_color_chord(primary() | extra, &enter), "{extra:?}");
        }
    }

    /// And no other KEY fires it, including the near misses on that window: Escape closes
    /// it, and Space is the accept key on the picker's overlays.
    #[test]
    fn no_other_key_files_a_colour() {
        for key in [
            Key::Named(Named::Escape),
            Key::Named(Named::Tab),
            Key::Character(" ".into()),
            Key::Character("a".into()),
            Key::Character("c".into()),
        ] {
            assert!(!is_add_color_chord(primary(), &key), "{key:?}");
        }
    }

    /// The LABEL on the "Add to recents" button describes the chord the handler matches, and it
    /// is rendered in the app's house style: mac symbols with no separator, readable words
    /// with `+` elsewhere. One `Shortcut`, so the two can never describe different keys.
    #[test]
    fn the_label_matches_the_chord_in_the_house_style() {
        let label = add_color_chord_label();
        assert!(label.contains("Enter"), "the key is named: {label}");
        #[cfg(target_os = "macos")]
        {
            assert_eq!(label, "⌘Enter");
            assert!(!label.contains("Cmd"), "macOS shows the SYMBOL, never the word");
            assert!(!label.contains('+'), "macOS joins symbols without a separator");
        }
        #[cfg(not(target_os = "macos"))]
        assert_eq!(label, "Ctrl+Enter");
        // Memoized, so the button can borrow it every frame.
        assert_eq!(label.as_ptr(), add_color_chord_label().as_ptr());
    }

    /// It is not a KEYMAP entry, and it must not collide with one: no configurable action
    /// ships with primary+Enter as its default, so there is nothing to arbitrate and the
    /// lane can sit outside the keymap (the `is_region_copy_chord` reasoning).
    #[test]
    fn no_configurable_action_claims_this_chord() {
        let enter = Key::Named(Named::Enter);
        for action in Action::ALL {
            if let Some(sc) = action.default_shortcut() {
                assert!(
                    !sc.matches(primary(), &enter),
                    "{:?} defaults to the add-color chord",
                    action
                );
            }
        }
    }
}

/// DRAGON-680: the colour picker window's COPY chord, and the two key helpers its own Tab
/// ring and arrow navigation are built on. What matters about all three is what they do NOT
/// claim: the text inputs in that window own the bare copy chord, the desktop owns the
/// modified Tabs, and a value box owns its own arrows.
#[cfg(test)]
mod picker_window_key_tests {
    use super::*;

    /// The primary command modifier on this platform: Cmd on macOS, Ctrl elsewhere.
    fn primary() -> Modifiers {
        #[cfg(target_os = "macos")]
        {
            Modifiers::LOGO
        }
        #[cfg(not(target_os = "macos"))]
        {
            Modifiers::CTRL
        }
    }

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }

    /// The copy chord fires on primary+Shift+C, and its LABEL says so in the house style.
    #[test]
    fn the_copy_chord_fires_and_reads_correctly() {
        assert!(is_copy_value_chord(primary() | Modifiers::SHIFT, &ch("c")));
        assert!(is_copy_value_chord(primary() | Modifiers::SHIFT, &ch("C")), "case-blind");
        let label = copy_value_chord_label();
        #[cfg(target_os = "macos")]
        assert_eq!(label, "⇧⌘C");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(label, "Ctrl+Shift+C");
        assert_eq!(label.as_ptr(), copy_value_chord_label().as_ptr(), "memoized");
    }

    /// **The BARE primary+C is not ours**, which is the whole reason the chord carries
    /// Shift: that is the text inputs' own copy-the-selection binding, and a user who
    /// highlighted three characters of a value and pressed it must get those three
    /// characters, not the whole colour.
    #[test]
    fn the_bare_copy_chord_belongs_to_the_text_inputs() {
        assert!(!is_copy_value_chord(primary(), &ch("c")));
        assert!(!is_copy_value_chord(Modifiers::empty(), &ch("c")));
        // And it is still the region-copy chord and `PreviewCopy`'s default, neither of
        // which gains a Shift.
        assert!(is_region_copy_chord(primary(), &ch("c")));
        assert!(!is_region_copy_chord(primary() | Modifiers::SHIFT, &ch("c")));
        let copy = Action::PreviewCopy.default_shortcut().expect("PreviewCopy ships bound");
        assert!(!copy.matches(primary() | Modifiers::SHIFT, &ch("c")));
    }

    /// No configurable action claims the copy chord either, so the lane can sit outside
    /// the keymap without arbitrating anything.
    #[test]
    fn no_configurable_action_claims_the_copy_chord() {
        for action in Action::ALL {
            if let Some(sc) = action.default_shortcut() {
                assert!(
                    !sc.matches(primary() | Modifiers::SHIFT, &ch("c")),
                    "{action:?} defaults to the copy chord"
                );
            }
        }
    }

    /// Tab steps forward, Shift+Tab backward, and every OTHER modifier declines: Ctrl+Tab
    /// and Cmd+Tab belong to the desktop's window switcher, and a window that swallowed
    /// them would be taking that away.
    #[test]
    fn tab_steps_both_ways_and_leaves_the_desktops_tabs_alone() {
        let tab = Key::Named(Named::Tab);
        assert_eq!(tab_step(Modifiers::empty(), &tab), Some(true));
        assert_eq!(tab_step(Modifiers::SHIFT, &tab), Some(false));
        for m in [Modifiers::CTRL, Modifiers::ALT, Modifiers::LOGO] {
            assert_eq!(tab_step(m, &tab), None, "{m:?}");
            assert_eq!(tab_step(m | Modifiers::SHIFT, &tab), None, "{m:?}+Shift");
        }
        assert_eq!(tab_step(Modifiers::empty(), &ch("a")), None, "only the Tab key");
    }

    /// The tab-CYCLE chord (DRAGON-687): Ctrl+Tab forward, Ctrl+Shift+Tab backward, on
    /// EVERY platform including macOS (the browser convention uses Control there, never
    /// Command), and nothing else. Disjoint from `tab_step` by construction, pinned here
    /// so the focus ring's keys and the strip-cycling keys can never claim one press.
    #[test]
    fn ctrl_tab_cycles_and_nothing_else_does() {
        let tab = Key::Named(Named::Tab);
        assert_eq!(tab_cycle_step(Modifiers::CTRL, &tab), Some(true));
        assert_eq!(tab_cycle_step(Modifiers::CTRL | Modifiers::SHIFT, &tab), Some(false));
        // The focus ring's own keys are not a cycle.
        assert_eq!(tab_cycle_step(Modifiers::empty(), &tab), None);
        assert_eq!(tab_cycle_step(Modifiers::SHIFT, &tab), None);
        // Alt+Tab and Cmd+Tab still belong to the desktop, with or without Control.
        for extra in [Modifiers::ALT, Modifiers::LOGO] {
            assert_eq!(tab_cycle_step(Modifiers::CTRL | extra, &tab), None, "{extra:?}");
            assert_eq!(tab_cycle_step(extra, &tab), None, "{extra:?}");
        }
        assert_eq!(tab_cycle_step(Modifiers::CTRL, &ch("a")), None, "only the Tab key");
        // Disjoint with `tab_step` over the whole modifier space this window can see.
        for m in [
            Modifiers::empty(),
            Modifiers::SHIFT,
            Modifiers::CTRL,
            Modifiers::CTRL | Modifiers::SHIFT,
            Modifiers::ALT,
            Modifiers::LOGO,
        ] {
            assert!(
                tab_step(m, &tab).is_none() || tab_cycle_step(m, &tab).is_none(),
                "{m:?}: one press must never mean both a ring step and a strip cycle"
            );
        }
    }

    /// The four arrows, and ONLY the arrows: the vim letters `nudge_direction` accepts are
    /// deliberately absent, because this predicate answers presses in a window full of
    /// text inputs where `l` is a letter someone is typing.
    #[test]
    fn only_the_bare_arrows_navigate() {
        for (key, dir) in [
            (Named::ArrowLeft, Direction::Left),
            (Named::ArrowRight, Direction::Right),
            (Named::ArrowUp, Direction::Up),
            (Named::ArrowDown, Direction::Down),
        ] {
            assert_eq!(arrow_direction(Modifiers::empty(), &Key::Named(key)), Some(dir));
            // A modifier makes it somebody else's press.
            assert_eq!(arrow_direction(Modifiers::SHIFT, &Key::Named(key)), None);
            assert_eq!(arrow_direction(primary(), &Key::Named(key)), None);
        }
        for letter in ["h", "j", "k", "l"] {
            assert_eq!(
                arrow_direction(Modifiers::empty(), &ch(letter)),
                None,
                "{letter} is a letter here, not a direction"
            );
            // …and it really is the difference from the overlay's own lane.
            assert!(nudge_direction(Modifiers::empty(), &ch(letter)).is_some());
        }
    }
}
