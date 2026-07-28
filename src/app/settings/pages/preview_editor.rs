//! Preview Editor settings page section builders (DRAGON-353).
//!
//! The post-capture editor grew enough behaviour of its own to want a page rather than a
//! group borrowed from someone else's: which surface it opens as, what a Copy does with
//! the document, what a Delete does with the clipboard, and the covermark content the
//! editor's picker offers. It sits directly after Capture Modes in the nav, because it is
//! what happens NEXT.
//!
//! Group order — **General** (the surface the editor opens as) → **Image Editor** (what
//! its actions do) → **Covermarks** (content it OFFERS). Widest-scope first: the
//! appearance decides how every row below it is experienced, and the covermark text is
//! not a behaviour at all, which is why it trails rather than joining Image Editor.
//!
//! A single-page section list (no in-page tab strip yet) — `general.rs`'s split is the
//! precedent for adding one when this outgrows one screen.

use super::super::*;
use super::super::row::{toggle, Item, SectionSpec};

/// The "Editor appearance" dropdown options (index 0 = overlay, 1 = windowed — matches
/// `preview_windowed` as a bool). Moved here from `general.rs` with the row it serves
/// (DRAGON-353).
const PREVIEW_APPEARANCES: [&str; 2] = ["Overlay", "Windowed"];

impl crate::app::App {
    /// Every Preview Editor section — the single source of both the page view and the
    /// global settings SEARCH (which scans `sections_for` per page, so a row added here is
    /// searchable with no extra wiring).
    pub(in crate::app::settings) fn preview_editor_sections(&self) -> Vec<SectionSpec<'_>> {
        let d = crate::state::defaults();
        vec![
            // What the editor IS before what it does: the surface it opens as decides how
            // every other row on this page is experienced, so the general group leads.
            // RELOCATED from General → "Capture Preview" (DRAGON-353), which the same
            // change emptied. A pure move — same `preview_windowed` field, same message,
            // same reset, same dropdown — with the label shortened to "Editor appearance":
            // "Preview editor appearance mode" was spelling out context the nav entry
            // ("Preview Editor") and this group now supply for free.
            SectionSpec {
                title: "General",
                items: vec![
                    Item::new(
                        "Editor appearance",
                        "",
                        crate::widgets::arrow_cursor::arrow_cursor(widget::dropdown(
                            &PREVIEW_APPEARANCES,
                            Some(usize::from(self.preview_windowed)),
                            |i| Msg::Settings(SettingsMsg::SetPreviewWindowed(i == 1)),
                        )),
                    )
                    .reset_with(
                        usize::from(self.preview_windowed),
                        usize::from(d.preview_windowed),
                        |i| Msg::Settings(SettingsMsg::SetPreviewWindowed(i == 1)),
                    ),
                ],
            },
            SectionSpec {
                title: "Image Editor",
                items: vec![
                    Item::new(
                        "Automatically save on copy to clipboard",
                        "",
                        toggle(self.preview_save_on_copy, |a0| {
                            Msg::Settings(SettingsMsg::SetPreviewSaveOnCopy(a0))
                        }),
                    )
                    .reset_with(
                        self.preview_save_on_copy,
                        d.preview_save_on_copy,
                        |a0| Msg::Settings(SettingsMsg::SetPreviewSaveOnCopy(a0)),
                    ),
                    Item::new(
                        "Automatically close on copy to clipboard",
                        "",
                        toggle(self.preview_close_on_copy, |a0| {
                            Msg::Settings(SettingsMsg::SetPreviewCloseOnCopy(a0))
                        }),
                    )
                    .reset_with(
                        self.preview_close_on_copy,
                        d.preview_close_on_copy,
                        |a0| Msg::Settings(SettingsMsg::SetPreviewCloseOnCopy(a0)),
                    ),
                    Item::new(
                        "Automatically copy to clipboard on delete",
                        "",
                        toggle(self.preview_copy_on_delete, |a0| {
                            Msg::Settings(SettingsMsg::SetPreviewCopyOnDelete(a0))
                        }),
                    )
                    .reset_with(
                        self.preview_copy_on_delete,
                        d.preview_copy_on_delete,
                        |a0| Msg::Settings(SettingsMsg::SetPreviewCopyOnDelete(a0)),
                    ),
                    // DRAGON-353: a "Clipboard size limit" row briefly lived here (relocated
                    // from General → "After a Capture"). The limit is now the fixed
                    // `share::AUTO_COPY_MAX_MB` and the row is GONE: the editor toasts a
                    // named error when it declines an automatic copy for size, so the knob
                    // only ever pre-empted a failure the user can now simply read.
                ],
            },
            // Covermarks: the custom-text covermark's content (used by the preview editor's
            // "Custom text" choice), plus a hint for where to drop more SVGs. RELOCATED here
            // from the Screenshots tab (DRAGON-353) — it configures the editor's covermark
            // picker, not the capture — as a pure move: same field, same message, same reset.
            // It keeps its own group rather than joining "Image Editor": it is content the
            // editor OFFERS, not a behaviour it performs.
            SectionSpec {
                title: "Covermarks",
                items: vec![
                    Item::new(
                        "Custom overlayed text",
                        format!("Covermark SVG loaded from:\n{}", covermark_dir_display()),
                        crate::widgets::hide_when_clipped(
                            widget::text_input("CONFIGURE TEXT IN SETTINGS", &self.covermark_text)
                                .on_input(|a0| Msg::Settings(SettingsMsg::SetCovermarkText(a0)))
                                .width(Length::Fixed(280.0)),
                        ),
                    )
                    .reset_with(
                        self.covermark_text.clone(),
                        d.covermark_text.clone(),
                        |a0| Msg::Settings(SettingsMsg::SetCovermarkText(a0)),
                    ),
                ],
            },
        ]
    }
}

/// The covermark folder path for display, abbreviating `$HOME` to `~`. Moved here with the
/// row it serves (DRAGON-353).
fn covermark_dir_display() -> String {
    let Some(dir) = crate::app::preview::covermark_dir() else {
        return "~/.config/cosmic-capture-kit/covermarks".into();
    };
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = dir.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    dir.display().to_string()
}
