//! Preview Editor settings page section builders (DRAGON-353).
//!
//! The post-capture editor grew enough behaviour of its own to want a page rather than a
//! group borrowed from someone else's: which surface it opens as, what a Copy does with
//! the document, what a Delete does with the clipboard, and the covermark content the
//! editor's picker offers. It sits directly after Capture Modes in the nav, because it is
//! what happens NEXT.
//!
//! Group order — **General** (the surface the editor opens as) → **Image Editor** →
//! **Video Editor** (what its actions do, per media kind) → **Covermarks** (content it
//! OFFERS). Widest-scope first: the appearance decides how every row below it is
//! experienced, and the covermark text is not a behaviour at all, which is why it trails
//! rather than joining the two editor groups.
//!
//! The two editor groups carry the SAME three rows against DIFFERENT fields (DRAGON-420):
//! the group title is what says which media a row governs, so the row wording stays
//! identical rather than growing "…(video)" suffixes. The one exception is the ask-to-save
//! row, which names its media outright ("edited screenshots" / "edited videos") because the
//! ticket does, and because a warning is clearer when it says what it is warning about.
//! Because this function is also the search index, both copies are searchable, and a query
//! for "copy changes" turns up both with their group names attached.
//!
//! DRAGON-467 replaced the three rows each group used to carry ("Automatically save on copy
//! to clipboard", "Automatically close on copy to clipboard", "Automatically copy to
//! clipboard on delete"). The Windows 11 Snipping Tool is the model for two of the new ones,
//! down to the wording: it ships exactly "Automatically save original screenshots" and "Ask
//! to save edited screenshots".
//!
//! DRAGON-478 stripped the DESCRIPTION text from all six of those rows (owner's call): the
//! labels already say what each one does, and six paragraphs of explanation under six
//! near-identical rows made the page read as documentation rather than as settings. The rows
//! themselves, their fields, their order and their behaviour are untouched — only the
//! second line is gone. The same change added "Show toolbar group labels" to General, also
//! label-only, for the same reason.
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
                // Where there is no overlay editor there is no choice to offer: Windows 10
                // (DRAGON-427) and a Linux session with no layer shell (`lab/flatpak`). The
                // row used to stay visible but INERT (a gated row with a warning saying
                // why); the owner's third live test overruled that: a setting that cannot
                // change is noise, not a warning to read, so the row is HIDDEN outright
                // (and with it its search entry, which could only lead somewhere inert).
                // Hiding is display-only. Nothing here writes `preview_windowed`, so a
                // normal session still sees the user's real choice, and the other half of
                // the job stands unchanged: `App::preview_windowed` is forced windowed in
                // `effective_preview_windowed`, so a config that already says
                // `preview_windowed = false` lands on the window rather than on an overlay
                // editor that cannot draw, or has no surface to draw on. The group keeps
                // its header either way: the toolbar-labels row below is unconditional.
                items: {
                    let mut items = Vec::new();
                    if crate::platform::overlay_preview_available() {
                        items.push(
                            Item::new(
                                "Editor appearance",
                                "",
                                crate::widgets::arrow_cursor::arrow_cursor(
                                    crate::widgets::press_redraw(widget::dropdown(
                                        &PREVIEW_APPEARANCES,
                                        Some(usize::from(self.preview_windowed)),
                                        |i| Msg::Settings(SettingsMsg::SetPreviewWindowed(i == 1)),
                                    )),
                                ),
                            )
                            .reset_with(
                                usize::from(self.preview_windowed),
                                usize::from(d.preview_windowed),
                                |i| Msg::Settings(SettingsMsg::SetPreviewWindowed(i == 1)),
                            ),
                        );
                    }
                    // DRAGON-478: the small captions under the top toolbar's groups ("Canvas",
                    // "Shape", "Redact", …). It belongs beside the appearance row because it is
                    // the same kind of choice — what the editor LOOKS like before what it does —
                    // and it is a layout switch as much as a cosmetic one: the caption band
                    // makes the top bar taller, which `PreviewSurface::chrome_h` reads. No
                    // description: the label says the whole of it.
                    items.push(
                        Item::new(
                            "Show toolbar group labels",
                            "",
                            toggle(self.preview_toolbar_labels, |a0| {
                                Msg::Settings(SettingsMsg::SetPreviewToolbarLabels(a0))
                            }),
                        )
                        .reset_with(
                            self.preview_toolbar_labels,
                            d.preview_toolbar_labels,
                            |a0| Msg::Settings(SettingsMsg::SetPreviewToolbarLabels(a0)),
                        ),
                    );
                    items
                },
            },
            SectionSpec {
                title: "Image Editor",
                items: vec![
                    Item::new(
                        "Automatically copy changes on exit",
                        "",
                        toggle(self.preview_copy_on_exit, |a0| {
                            Msg::Settings(SettingsMsg::SetPreviewCopyOnExit(a0))
                        }),
                    )
                    .reset_with(
                        self.preview_copy_on_exit,
                        d.preview_copy_on_exit,
                        |a0| Msg::Settings(SettingsMsg::SetPreviewCopyOnExit(a0)),
                    ),
                    Item::new(
                        "Automatically save originals",
                        "",
                        toggle(self.preview_save_originals, |a0| {
                            Msg::Settings(SettingsMsg::SetPreviewSaveOriginals(a0))
                        }),
                    )
                    .reset_with(
                        self.preview_save_originals,
                        d.preview_save_originals,
                        |a0| Msg::Settings(SettingsMsg::SetPreviewSaveOriginals(a0)),
                    ),
                    Item::new(
                        "Ask to save edited screenshots",
                        "",
                        toggle(self.preview_ask_to_save, |a0| {
                            Msg::Settings(SettingsMsg::SetPreviewAskToSave(a0))
                        }),
                    )
                    .reset_with(
                        self.preview_ask_to_save,
                        d.preview_ask_to_save,
                        |a0| Msg::Settings(SettingsMsg::SetPreviewAskToSave(a0)),
                    ),
                    // DRAGON-353: a "Clipboard size limit" row briefly lived here (relocated
                    // from General → "After a Capture"). The limit is now the fixed
                    // `share::AUTO_COPY_MAX_MB` and the row is GONE: the editor toasts a
                    // named error when it declines an automatic copy for size, so the knob
                    // only ever pre-empted a failure the user can now simply read.
                    //
                    // DRAGON-467: "Automatically save on copy to clipboard", "Automatically
                    // close on copy to clipboard" and "Automatically copy to clipboard on
                    // delete" lived here (and in the group below) until the actions they
                    // modified stopped working that way. See `state::schema` for the fields
                    // and `state::store`'s v9 note for why nothing was carried forward.
                ],
            },
            // The SAME three rows for recordings, on their own fields (the DRAGON-420 split).
            // One group per kind, near-identical wording — the group title is what
            // disambiguates, so only the row that must name the media ("edited videos") does
            // — and identical defaults.
            SectionSpec {
                title: "Video Editor",
                items: vec![
                    Item::new(
                        "Automatically copy changes on exit",
                        "",
                        toggle(self.preview_video_copy_on_exit, |a0| {
                            Msg::Settings(SettingsMsg::SetPreviewVideoCopyOnExit(a0))
                        }),
                    )
                    .reset_with(
                        self.preview_video_copy_on_exit,
                        d.preview_video_copy_on_exit,
                        |a0| Msg::Settings(SettingsMsg::SetPreviewVideoCopyOnExit(a0)),
                    ),
                    Item::new(
                        "Automatically save originals",
                        "",
                        toggle(self.preview_video_save_originals, |a0| {
                            Msg::Settings(SettingsMsg::SetPreviewVideoSaveOriginals(a0))
                        }),
                    )
                    .reset_with(
                        self.preview_video_save_originals,
                        d.preview_video_save_originals,
                        |a0| Msg::Settings(SettingsMsg::SetPreviewVideoSaveOriginals(a0)),
                    ),
                    Item::new(
                        "Ask to save edited videos",
                        "",
                        toggle(self.preview_video_ask_to_save, |a0| {
                            Msg::Settings(SettingsMsg::SetPreviewVideoAskToSave(a0))
                        }),
                    )
                    .reset_with(
                        self.preview_video_ask_to_save,
                        d.preview_video_ask_to_save,
                        |a0| Msg::Settings(SettingsMsg::SetPreviewVideoAskToSave(a0)),
                    ),
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
                        "Covermark SVG loaded from:",
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
                    )
                    // The folder gets the Health page's treatment: subtle tone, led by a copy
                    // button, wrapping inside the pane. It was a bare line inside the
                    // description, which could not be copied and read as ordinary prose.
                    // Only the PATH is toned down — `path_desc` keeps the sentence above it in
                    // the ordinary description tone, which the first cut got wrong.
                    .desc_el({
                        let dir = covermark_dir_display();
                        let copied =
                            super::super::row::path_line_copied(&dir, &self.settings.health_copied);
                        super::super::row::path_desc("Covermark SVG loaded from:", dir, copied)
                    }),
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
    // The `~` abbreviation itself is `settings::row::path_line`'s job now, applied to every
    // path the app shows rather than to this one row. The hand-rolled copy that used to live
    // here was the only place that did it, which is exactly why the other paths leaked the
    // account name into screenshots.
    dir.display().to_string()
}
