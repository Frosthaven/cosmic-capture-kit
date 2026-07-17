//! `DetectMsg` sub-enum split out of the former flat `Msg` (see app/mod.rs).

#[derive(Debug, Clone)]
pub enum DetectMsg {
    /// Screenshots: toggle QR/barcode scanning.
    SetScanCodes(bool),
    /// Screenshots: toggle OCR text scanning.
    SetScanText(bool),
    /// Screenshots: minimum OCR word confidence (0–100).
    SetTextConfidence(f32),
    /// Screenshots: toggle dropping OCR'd UI symbols/icons.
    /// Poll the background scans (load results, re-OCR a changed region).
    MarksPoll,
    /// A detected mark is hovered (index into `marks`), or none.
    HoverMark(Option<usize>),
    /// A detected mark was clicked — perform its action and exit.
    ActivateMark(usize),
    /// An OCR word is hovered (index into `text_words`), or none.
    HoverWord(Option<usize>),
    /// Begin a range selection anchored at a word. The bool is additive: true unions
    /// the range into the existing selection (ctrl+shift), false replaces it. Snapshots
    /// the base selection so a drag can recompute continuously.
    TextSelectBegin(usize, bool),
    /// Extend the in-progress range selection to this word (drag / shift-click target).
    TextSelectTo(usize),
    /// Toggle a single word in/out of the selection (ctrl-click).
    TextToggle(usize),
    /// Multi-click expand: double-click (count 2) selects word `idx`'s line, triple
    /// (3+) selects all.
    TextExpand(usize, u8),
    /// Select every OCR word (ctrl+A / menu).
    TextSelectAll,
    /// Clear the text selection (menu "Select none").
    TextDeselect,
    /// Copy the active text selection to the clipboard (keyboard or menu); stays open.
    TextCopy,
    /// Right-click on word `idx` at global position (x, y) — open the "Copy" menu.
    WordMenu(usize, i32, i32),
    /// Right-click on code mark `idx` at global (x, y) — open its "Copy contents" menu.
    CodeMenu(usize, i32, i32),
    /// Copy code mark `idx`'s full decoded contents to the clipboard (stays open).
    CopyCodeContents(usize),
    /// Dismiss the code "Copy contents" menu without copying.
    DismissCodeMenu,
    /// Dismiss the text "Copy" menu without copying.
    DismissTextMenu,
}
