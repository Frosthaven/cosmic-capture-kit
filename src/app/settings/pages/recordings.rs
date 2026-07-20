//! Recordings settings page section builder.

use super::super::*;
use super::super::row::{folder_btn, toggle, Item, SectionSpec};
use super::super::deps::DepId;
use super::capture::MethodPicker;

impl crate::app::App {
    /// The Recordings page is the most conditional, so it lives in its own fn.
    pub(in crate::app::settings) fn recordings_sections(&self) -> Vec<SectionSpec<'_>> {
        let d = crate::state::defaults();

        // Surface the recording-availability note only when there's a problem; the Health
        // page lists it regardless.
        let mut secs: Vec<SectionSpec<'_>> = Vec::new();
        let avail: Vec<Item<'_>> = [DepId::Recording, DepId::Ffmpeg, DepId::Ffprobe]
            .into_iter()
            .filter_map(|d| self.dep(d).note_if_issue())
            .collect();
        if !avail.is_empty() {
            secs.push(SectionSpec { title: "Availability", items: avail });
        }

        secs.push(SectionSpec {
            title: "Location",
            items: vec![Item::new(
                "Save recordings to",
                "",
                widget::row(vec![
                    crate::widgets::hide_when_clipped(
                        widget::text_input("~/Capture", &self.record_dir)
                            .on_input(|a0| Msg::Settings(SettingsMsg::SetRecordDir(a0)))
                            .width(Length::Fixed(280.0)),
                    ),
                    folder_btn(DirTarget::Recording),
                ])
                .spacing(6.0)
                .align_y(Alignment::Center),
            )
            .reset_with(self.record_dir.clone(), d.record_dir.clone(), |a0| Msg::Settings(SettingsMsg::SetRecordDir(a0)))],
        });

        secs.push(self.capture_section(
            self.dep(DepId::Recording).is_present(),
            MethodPicker {
                methods: &self.record_methods,
                selected: &self.record_backend,
                default_id: d.record_backend.clone(),
                setter: |a0| Msg::Settings(SettingsMsg::SetRecordBackend(a0)),
            },
            "Recordings",
            Vec::new(),
        ));

        // Hide the floating toolbar on full-screen captures (DRAGON-174): when the
        // toolbar can't sit OUTSIDE the recording area, hide it instead of placing it
        // in-frame. The tray icon always carries the recording controls regardless.
        secs.push(SectionSpec {
            title: "Behavior",
            items: vec![Item::new(
                "Hide toolbar on full screen captures",
                "When the floating toolbar can't fit outside of the recording area, this will hide it instead of placing it in-frame. You can still control the recording via the system tray icon.",
                toggle(self.hide_toolbar_fullscreen, |a0| {
                    Msg::Settings(SettingsMsg::SetHideToolbarFullscreen(a0))
                }),
            )
            .reset_with(
                self.hide_toolbar_fullscreen,
                d.hide_toolbar_fullscreen,
                |a0| Msg::Settings(SettingsMsg::SetHideToolbarFullscreen(a0)),
            )],
        });

        secs
    }
}
