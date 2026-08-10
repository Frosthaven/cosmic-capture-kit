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
// The Application Permissions row is macOS-only, and so is the only button on this page.
#[cfg(target_os = "macos")]
use super::super::row::{centered_button, standard_button_class};

impl crate::app::App {
    pub(in crate::app::settings) fn health_sections(&self) -> Vec<SectionSpec<'_>> {
        let overall = self.health_level();
        let summary = match overall {
            Severity::Ok => "All dependencies are satisfied.",
            Severity::Warn => "Some optional features are unavailable. See below.",
            Severity::Error => "A required dependency is missing. Application may not work as expected.",
        };

        // Application Management (+ its Application Permissions row) is the very FIRST
        // section on the page, above even Status (owner request, superseding DRAGON-491's
        // "right after the status banner" placement, which itself had superseded
        // DRAGON-419's original bottom-of-page placement).
        let mut secs = vec![self.debug_section()];

        secs.push(SectionSpec {
            title: "Status",
            items: vec![
                Item::new("Overall health", summary, status_icon(overall)).status(overall),
            ],
        });

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
                title: "Optional Features",
                items: optional.iter().map(|d| d.row()).collect(),
            });
        }

        secs
    }

    /// DRAGON-419: the Debug group, displayed as "Application Management" (owner rename;
    /// the underlying debug-logging feature, its env var and its log target keep the
    /// `debug`/`diag` names throughout the code, this is a display title only). Now the
    /// very first section on the Health page, above even Status (owner's spec, superseding
    /// DRAGON-491's "right after the status banner" placement).
    ///
    /// First row's description is JUST the resolved log folder — deliberately no prose: a
    /// support reply says "turn this on, do it again, send me the file", and the only thing
    /// the row has to answer is *which* file. Spelling the path out (rather than only offering
    /// a button) is what makes a remote-support call or a locked-down machine workable.
    ///
    /// The control slot pairs the folder button with the toggle so the folder is reachable
    /// whether or not logging is currently on — a customer who turned it on yesterday needs to
    /// find the file today.
    ///
    /// The second row (DRAGON-491, macOS only, no other OS has TCC grants to manage) opens the
    /// permission-checker window (`app::permissions`) directly from Settings, rather than only
    /// via `--permissions` or missing-grant routing.
    fn debug_section(&self) -> SectionSpec<'_> {
        let control = widget::row(vec![
            open_folder_btn(Msg::WindowChrome(WindowChromeMsg::OpenDebugLogFolder)),
            toggle(self.debug_logging, |b| Msg::Settings(SettingsMsg::SetDebugLogging(b))),
        ])
        .spacing(12.0)
        .align_y(Alignment::Center);
        // The folder gets the same treatment as every other path the app shows (owner report):
        // subtle tone, led by a copy button, wrapping inside the pane, and `~` for the home
        // prefix. It was plain description prose, which is the one form a path should never
        // take here, because the whole job of this row is "which file do I send to support"
        // and copying it by hand off a wrapped line is exactly the friction the copy button
        // exists to remove.
        //
        // `path_line` alone rather than `path_desc`: this row deliberately carries no
        // introducing sentence (see the doc above), so there is no prose half to pair with.
        let log_dir = crate::diag::log_dir_display();
        let log_dir_copied =
            super::super::row::path_line_copied(&log_dir, &self.settings.health_copied);
        // `mut` only where something is pushed below: the Application Permissions row is
        // macOS-only, so every other platform builds this list once and never adds to it.
        #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
        let mut items = vec![Item::new("Enable Debug Logging", "", control)
            .desc_el(super::super::row::path_line(log_dir, log_dir_copied))
            .reset_with(self.debug_logging, false, |b| {
                Msg::Settings(SettingsMsg::SetDebugLogging(b))
            })];
        // `flush()` (DRAGON-495): this row's control is a lone button with no unit label
        // or reset, so it drops the fixed trailing slots every other row reserves for
        // those, matching the same fix the Cloud Accounts page already carries.
        #[cfg(target_os = "macos")]
        items.push(
            Item::new(
                "Application Permissions",
                "",
                centered_button(
                    Some("dialog-password-symbolic"),
                    "Manage permissions",
                    Length::Shrink,
                    standard_button_class(),
                    Some(Msg::Settings(SettingsMsg::OpenPermissionsWindow)),
                ),
            )
            .flush(),
        );
        SectionSpec {
            title: "Application Management",
            items,
        }
    }
}
