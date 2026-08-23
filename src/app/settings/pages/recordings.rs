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
                            .width(Length::Fixed(280.0))
                            // DRAGON-680: the app's softened text-selection fill.
                            .style(theme::input_style(theme::InputBase::Default)),
                    ),
                    folder_btn(DirTarget::Recording),
                ])
                .spacing(6.0)
                .align_y(Alignment::Center),
            )
            .reset_with(self.record_dir.clone(), d.record_dir.clone(), |a0| Msg::Settings(SettingsMsg::SetRecordDir(a0)))],
        });

        // DRAGON-695: the two audio arms had NO settings row at all, only the recording
        // toolbar's toggles and the tray's Audio Recording submenu, which meant the default a
        // new recording starts from could not be set from the settings window. They are the
        // page twin of the `sound <both|mic|system|none>` command, and they write exactly what
        // it writes.
        //
        // Deliberately `SetRecordMicDefault` rather than the live `RecordingMsg::ToggleMic`:
        // that one carries the meters, the push-to-talk rule and the tray refresh for a
        // session in progress, and this row is about the next recording, not this one.
        secs.push(SectionSpec {
            title: "Audio",
            items: vec![
                Item::new(
                    "Record the microphone",
                    "New recordings start with the microphone armed.",
                    toggle(self.record_mic, |a0| {
                        Msg::Settings(SettingsMsg::SetRecordMicDefault(a0))
                    }),
                )
                .reset_with(self.record_mic, d.record_mic, |a0| {
                    Msg::Settings(SettingsMsg::SetRecordMicDefault(a0))
                }),
                Item::new(
                    "Record system audio",
                    "New recordings start with system audio armed.",
                    toggle(self.record_system_audio, |a0| {
                        Msg::Settings(SettingsMsg::SetRecordSystemAudioDefault(a0))
                    }),
                )
                .reset_with(self.record_system_audio, d.record_system_audio, |a0| {
                    Msg::Settings(SettingsMsg::SetRecordSystemAudioDefault(a0))
                }),
            ],
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
        //
        // DRAGON-569: on the portal-fallback path (`overlay_fallback_active`, no layer
        // shell) the in-frame toolbar this governs can NEVER appear — every fallback
        // recording tears its surfaces down at record start (`recreate_active_overlays`),
        // whatever mode picked it, so the setting is inert there. The rule for a choice
        // that cannot apply is to HIDE the row entirely (never an inert/warning row), and
        // this row is the Behavior group's only item, so the group header hides with it.
        // The persisted value is untouched; layer-shell sessions are byte-identical.
        if !self.overlay_fallback_active() {
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
        }

        secs
    }
}
