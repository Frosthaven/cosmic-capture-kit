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
    // DRAGON-451: a `Region` context lived here — a drawn region selection, its own lane so
    // primary+C could mean "copy the drawn region" there and "copy recognized text" in
    // [`Self::Overlay`] without the two fighting. Its sole action (`RegionCopy`) is retired,
    // so the context went with it and primary+C in the overlay is the OCR copy outright.
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
    /// Clear the RECOGNIZED-text selection in the scanner overlay (default Ctrl+D).
    ///
    /// DESELECT IS `Ctrl+D` IN EVERY CONTEXT (DRAGON-369) — the rule, not a coincidence:
    /// select-all was already one key everywhere ([`Self::SelectAllText`] /
    /// [`Self::PreviewSelectAll`], both Ctrl+A), resolved by what is on screen, and deselect
    /// simply was not following it. `Ctrl+D` is also Photoshop's Deselect. A future action that
    /// DESELECTS something must join this key in its own context; [`Keymap::set`] de-duplicates
    /// only WITHIN a context, so that is a no-conflict addition by construction.
    DeselectText,
    /// Preview: save the capture, choosing where (default Ctrl+S). DRAGON-467 folded the
    /// separate `PreviewSaveAs` (Ctrl+Shift+S) into this one: Save opens the destination
    /// picker pre-filled with what a plain overwrite-save would have written, so the two
    /// actions had become the same action. An override stored against the retired name
    /// deserializes to `None` and is dropped (see `state::schema`'s `shortcut_overrides`).
    PreviewSave,
    /// Preview: copy the capture to the clipboard (default Ctrl+C).
    PreviewCopy,
    /// Preview: open the Upload flyout, which picks a connected cloud account and starts an
    /// upload (default Ctrl+U). DRAGON-482.
    ///
    /// `U` for Upload, and it is free in this context: the preview's other `primary+`
    /// bindings are S, C, Z, Shift+Z, A and D. It opens the FLYOUT rather than uploading
    /// straight away, because an upload picks a destination account and is not undoable.
    PreviewUpload,
    /// Preview: close without deleting (default Esc).
    PreviewCancel,
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
    /// Preview: the POINTER (pure selection) annotation tool (default V — Photoshop's Move
    /// tool) — DRAGON-341.
    PreviewAnnotPointer,
    /// Preview: select every annotation in the scene, engaging pointer mode (default Ctrl+A) —
    /// DRAGON-341.
    PreviewSelectAll,
    /// Preview: clear the annotation selection (default Ctrl+D) — DRAGON-369. The matched pair
    /// to [`Self::PreviewSelectAll`], and Photoshop's Deselect; see [`Self::DeselectText`] for
    /// the everywhere-we-deselect rule.
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
    /// Recording: stop + save the in-progress recording (default Enter).
    RecordStop,
    /// Recording: toggle the microphone channel (default M).
    RecordToggleMic,
    /// Recording: toggle the system-audio channel (default S).
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
    pub const ALL: [Action; 39] = [
        Action::SelectAllText,
        Action::DeselectText,
        Action::CopyText,
        Action::PreviewSave,
        Action::PreviewCopy,
        // Straight after Copy, matching the editor's own toolbar order (Save, Copy, Share,
        // Upload) and keeping the "Action Shortcuts" group contiguous, which `group()`
        // requires.
        Action::PreviewUpload,
        Action::PreviewCancel,
        Action::PreviewCovermark,
        Action::PreviewUndo,
        Action::PreviewRedo,
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
            Action::CopyText => "Copy selected text",
            Action::SelectAllText => "Select all text",
            Action::DeselectText => "Deselect all text",
            Action::PreviewSave => "Save",
            Action::PreviewCopy => "Copy to clipboard",
            Action::PreviewUpload => "Upload",
            Action::PreviewPlay => "Play",
            Action::PreviewFramePrev => "Previous frame",
            Action::PreviewFrameNext => "Next frame",
            Action::PreviewCancel => "Close",
            Action::PreviewCovermark => "Covermark",
            Action::PreviewUndo => "Undo",
            Action::PreviewRedo => "Redo",
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
            // OCR group: descriptions removed per DRAGON-158.
            Action::CopyText => "",
            Action::SelectAllText => "",
            Action::DeselectText => "",
            // Action Shortcuts group: descriptions removed per DRAGON-158.
            Action::PreviewSave => "",
            Action::PreviewCopy => "",
            Action::PreviewUpload => "Upload to a connected cloud account.",
            Action::PreviewCancel => "",
            Action::PreviewCovermark => "",
            Action::PreviewUndo => "",
            Action::PreviewRedo => "",
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
    pub fn group(self) -> &'static str {
        match self {
            Action::CopyText | Action::SelectAllText | Action::DeselectText => {
                "OCR Text Recognition"
            }
            Action::PreviewSave
            | Action::PreviewCopy
            | Action::PreviewUpload
            | Action::PreviewCancel
            | Action::PreviewCovermark
            | Action::PreviewUndo
            | Action::PreviewRedo => "Action Shortcuts",
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
            Action::CopyText
            | Action::SelectAllText
            | Action::DeselectText => Context::Overlay,
            Action::PreviewSave
            | Action::PreviewCopy
            | Action::PreviewUpload
            | Action::PreviewPlay
            | Action::PreviewFramePrev
            | Action::PreviewFrameNext
            | Action::PreviewCancel
            | Action::PreviewCovermark
            | Action::PreviewUndo
            | Action::PreviewRedo
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
            Action::CopyText => Shortcut::primary_char('c'),
            Action::SelectAllText => Shortcut::primary_char('a'),
            Action::DeselectText => Shortcut::primary_char('d'),
            Action::PreviewSave => Shortcut::primary_char('s'),
            Action::PreviewCopy => Shortcut::primary_char('c'),
            Action::PreviewUpload => Shortcut::primary_char('u'),
            Action::PreviewPlay => Shortcut::char('p'),
            Action::PreviewFramePrev => Shortcut::char(','),
            Action::PreviewFrameNext => Shortcut::char('.'),
            Action::PreviewCancel => Shortcut::named(NamedKey::Escape),
            Action::PreviewCovermark => Shortcut::char('w'),
            Action::PreviewUndo => Shortcut::primary_char('z'),
            Action::PreviewRedo => Shortcut::primary_shift_char('z'),
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
            Action::RecordStop => Shortcut::named(NamedKey::Enter),
            Action::RecordToggleMic => Shortcut::char('m'),
            Action::RecordToggleSystemAudio => Shortcut::char('s'),
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
    if !(modifiers.control() && modifiers.logo() && !modifiers.alt() && !modifiers.shift()) {
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
/// * `Ctrl+C` ALREADY has a second, different meaning in this overlay: in SCANNER kind it is
///   [`Action::CopyText`], which copies recognized text. Two configurable bindings on one chord
///   is a conflict the settings page would have to explain and the user would have to resolve.
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
        let copy = def(Action::CopyText); // primary+C
        assert!(copy.matches(PRIMARY, &ch("c")));
        assert!(copy.matches(PRIMARY, &ch("C"))); // case-insensitive
        assert!(!copy.matches(PRIMARY | Modifiers::SHIFT, &ch("c"))); // extra modifier
        assert!(!copy.matches(Modifiers::empty(), &ch("c"))); // missing modifier
        assert!(!copy.matches(PRIMARY, &ch("x"))); // wrong key
    }

    /// Select-all is primary+A and DESELECT IS primary+D — in EVERY context that has one
    /// (DRAGON-369). One key, one meaning, resolved by what is on screen; the old
    /// primary+Shift+A deselect is retired and left UNBOUND rather than reassigned. This test
    /// is the stated invariant: a future deselect-shaped action must join it here.
    #[test]
    fn deselect_is_primary_d_in_every_context() {
        let km = Keymap::defaults();
        for (ctx, select, deselect) in [
            (Context::Overlay, Action::SelectAllText, Action::DeselectText),
            (Context::Preview, Action::PreviewSelectAll, Action::PreviewDeselectAll),
        ] {
            assert_eq!(km.action_for(ctx, PRIMARY, &ch("a")), Some(select), "{ctx:?} select-all");
            assert_eq!(km.action_for(ctx, PRIMARY, &ch("d")), Some(deselect), "{ctx:?} deselect");
            // The chord it replaced is freed, not reassigned.
            assert_eq!(km.action_for(ctx, PRIMARY | Modifiers::SHIFT, &ch("a")), None, "{ctx:?}");
        }
        // Every action whose NAME says it deselects is on the key — the general rule, checked
        // by enumeration so a new one cannot quietly pick something else.
        for action in Action::ALL.into_iter().filter(|a| format!("{a:?}").contains("Deselect")) {
            let sc = km.get(action).unwrap_or_else(|| panic!("{action:?} must be bound"));
            assert!(sc.matches(PRIMARY, &ch("d")), "{action:?} must deselect on primary+D");
        }
    }

    /// Upload is primary+U in the preview, and only in the preview (DRAGON-482).
    ///
    /// The point of the test is not the letter, it is that the new action goes through the
    /// SAME machinery every other one does: the platform primary modifier resolves itself
    /// (Ctrl off macOS, Cmd on it, asserted by `primary_defaults_are_*`), the per-OS label
    /// rendering falls out of `Shortcut::label` with nothing added for it, and `Keymap::set`
    /// de-duplicates within the context so the key cannot be double-booked.
    #[test]
    fn upload_is_primary_u_in_the_preview() {
        let km = Keymap::defaults();
        assert_eq!(
            km.action_for(Context::Preview, PRIMARY, &ch("u")),
            Some(Action::PreviewUpload)
        );
        // Not a key the capture overlay or a recording answers: the editor owns it.
        assert_eq!(km.action_for(Context::Overlay, PRIMARY, &ch("u")), None);
        assert_eq!(km.action_for(Context::Recording, PRIMARY, &ch("u")), None);
        // BARE U keeps the shape-tool slot it has held since DRAGON-369: adding a primary+U
        // chord stole nothing. This is the assertion worth having, because `Keymap::set`
        // de-duplicates within a context and a careless default could have taken the tool key.
        assert_eq!(
            km.action_for(Context::Preview, Modifiers::empty(), &ch("u")),
            Some(Action::PreviewAnnotShapeCycle)
        );
        // It renders through the existing per-OS label machinery with no special case.
        let label = def(Action::PreviewUpload).label();
        assert!(label.ends_with('U'), "the label should name the key: {label}");
        assert!(!label.is_empty());
        // And it sits in the same settings group as the other committing actions, which is
        // what puts its row beside Save and Copy.
        assert_eq!(Action::PreviewUpload.group(), Action::PreviewCopy.group());
        assert!(!Action::PreviewUpload.label().is_empty());
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
        assert_eq!(km.action_for(Context::Overlay, PRIMARY, &ch("x")), None);
        // Plain Delete still belongs to the TIMELINE (deleting a segment, which is undoable
        // and touches no file). That one stays.
        assert_eq!(
            km.action_for(Context::Preview, Modifiers::empty(), &Key::Named(Named::Delete)),
            Some(Action::PreviewDeleteSegment)
        );
    }

    #[test]
    fn named_key_matches_and_has_no_modifiers() {
        let close = def(Action::PreviewCancel); // Esc
        assert!(close.matches(Modifiers::empty(), &Key::Named(Named::Escape)));
        assert!(!close.matches(Modifiers::CTRL, &Key::Named(Named::Escape)));
    }

    #[test]
    fn from_event_rejects_bare_modifiers() {
        assert!(Shortcut::from_event(PRIMARY, &Key::Named(Named::Control)).is_none());
        assert_eq!(
            Shortcut::from_event(PRIMARY, &ch("c")),
            Some(def(Action::CopyText))
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn label_formats_modifiers() {
        assert_eq!(def(Action::CopyText).label(), "Ctrl+C");
        assert_eq!(def(Action::DeselectText).label(), "Ctrl+D");
        assert_eq!(def(Action::PreviewCancel).label(), "Esc");
        assert_eq!(def(Action::PreviewUpload).label(), "Ctrl+U");
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
            Action::PreviewCopy,
            Action::PreviewUpload,
            Action::PreviewUndo,
            Action::PreviewRedo,
        ] {
            let sc = def(action);
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
            Action::PreviewCopy,
            Action::PreviewUpload,
            Action::PreviewUndo,
            Action::PreviewRedo,
        ] {
            let sc = def(action);
            assert!(sc.logo, "{action:?} should default to Cmd (logo) on macOS");
            assert!(!sc.ctrl, "{action:?} should not default to Ctrl on macOS");
        }
        // The shifted chords keep Shift alongside Cmd.
        assert!(def(Action::PreviewRedo).shift);
        // Bare-key defaults stay unmodified on macOS too.
        let play = def(Action::PreviewPlay);
        assert!(!play.ctrl && !play.logo && !play.alt && !play.shift);
        // macOS labels render the ⌘ glyph.
        assert_eq!(def(Action::CopyText).label(), "⌘C");
        assert_eq!(def(Action::PreviewSave).label(), "⌘S");
        assert_eq!(def(Action::PreviewUpload).label(), "⌘U");
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
        assert_eq!(km.get(Action::CopyText), Some(def(Action::CopyText)));
    }

    #[test]
    fn set_steals_on_conflict() {
        let mut km = Keymap::defaults();
        // Rebind "copy" to primary+A, which "select all" already holds.
        km.set(Action::CopyText, def(Action::SelectAllText));
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

    /// DRAGON-451 retired `RegionCopy` (the region "Copy selection" quick-action), and with
    /// it the whole `Context::Region` lane. Three tests covered its shared-primary+C
    /// arrangement; what survives them is the thing that still matters — primary+C in the
    /// capture overlay is the OCR copy, unconditionally, with nothing left to take it first.
    #[test]
    fn primary_c_in_the_overlay_is_the_ocr_copy() {
        let km = Keymap::defaults();
        assert_eq!(km.action_for(Context::Overlay, PRIMARY, &ch("c")), Some(Action::CopyText));
        assert_eq!(km.action_for(Context::Preview, PRIMARY, &ch("c")), Some(Action::PreviewCopy));
        // No action claims a region-draw context any more.
        assert!(!Action::ALL.iter().any(|a| a.label() == "Copy selection"));
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
