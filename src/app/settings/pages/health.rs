//! Health-check settings page: one place that itemises every requirement the app
//! probes at runtime, each with ok / warning / error styling (inspired by the Nexus
//! Mods app's diagnostics panel), plus an overall status banner.
//!
//! The requirements themselves (the two required capabilities and the optional
//! tools) live in `settings::deps` (the single source of names, severities, and
//! messages); this module only lays them out. A missing optional feature is amber;
//! a missing required capability is red; everything satisfied is green.

use super::super::*;
use super::super::deps::Dep;
use super::super::row::{open_folder_btn, status_icon, toggle, Item, SectionSpec, Severity};

impl crate::app::App {
    pub(in crate::app::settings) fn health_sections(&self) -> Vec<SectionSpec<'_>> {
        let overall = self.health_level();
        let summary = match overall {
            Severity::Ok => "All dependencies are satisfied.",
            Severity::Warn => "Some optional features are unavailable. See below.",
            Severity::Error => "A required dependency is missing. Application may not work as expected.",
        };

        let mut secs = vec![SectionSpec {
            title: "Status",
            items: vec![
                Item::new("Overall health", summary, status_icon(overall)).status(overall),
            ],
        }];

        // Group required capabilities (red when missing) before optional features (amber).
        let (required, optional): (Vec<Dep>, Vec<Dep>) =
            self.deps().into_iter().partition(Dep::is_required);

        if !required.is_empty() {
            secs.push(SectionSpec {
                title: "Required",
                items: required.iter().map(|d| d.row()).collect(),
            });
        }
        if !optional.is_empty() {
            secs.push(SectionSpec {
                title: "Optional features",
                items: optional.iter().map(|d| d.row()).collect(),
            });
        }
        secs.push(self.debug_section());

        secs
    }

    /// DRAGON-419: the Debug group, at the BOTTOM of the Health page (owner's spec).
    ///
    /// One row. The description is JUST the resolved log folder — deliberately no prose: a
    /// support reply says "turn this on, do it again, send me the file", and the only thing
    /// the row has to answer is *which* file. Spelling the path out (rather than only offering
    /// a button) is what makes a remote-support call or a locked-down machine workable.
    ///
    /// The control slot pairs the folder button with the toggle so the folder is reachable
    /// whether or not logging is currently on — a customer who turned it on yesterday needs to
    /// find the file today.
    fn debug_section(&self) -> SectionSpec<'_> {
        let control = widget::row(vec![
            open_folder_btn(Msg::WindowChrome(WindowChromeMsg::OpenDebugLogFolder)),
            toggle(self.debug_logging, |b| Msg::Settings(SettingsMsg::SetDebugLogging(b))),
        ])
        .spacing(12.0)
        .align_y(Alignment::Center);
        SectionSpec {
            title: "Debug",
            items: vec![Item::new(
                "Enable Debug Logging",
                crate::diag::log_dir_display(),
                control,
            )
            .reset_with(self.debug_logging, false, |b| {
                Msg::Settings(SettingsMsg::SetDebugLogging(b))
            })],
        }
    }
}
