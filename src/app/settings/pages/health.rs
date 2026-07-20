//! Health-check settings page: one place that itemises every requirement the app
//! probes at runtime, each with ok / warning / error styling (inspired by the Nexus
//! Mods app's diagnostics panel), plus an overall status banner.
//!
//! The requirements themselves (the two required capabilities and the optional
//! tools) live in `settings::deps` (the single source of names, severities, and
//! messages); this module only lays them out. A missing optional feature is amber;
//! a missing required capability is red; everything satisfied is green.

use super::super::deps::Dep;
use super::super::row::{status_icon, Item, SectionSpec, Severity};

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

        secs
    }
}
