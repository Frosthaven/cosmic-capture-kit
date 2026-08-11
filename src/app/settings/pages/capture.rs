//! Capture-section helper and encoder/codec helpers, shared by the Screenshots
//! and Recordings pages (capture_section) and the Video page (encoder/codec rows).

use super::super::*;
use super::super::row::{gated_row, Item, SectionSpec, Severity};

/// One "Capture method" dropdown's wiring, bundled for [`crate::app::App::capture_section`]:
/// the derived backend choices, the persisted selection + its default (stable
/// backend ids), and the settings message the dropdown/reset dispatch.
pub(in crate::app::settings) struct MethodPicker<'a> {
    pub methods: &'a crate::platform::backend::MethodChoices,
    pub selected: &'a str,
    pub default_id: String,
    pub setter: fn(String) -> Msg,
}

/// One capture-extra toggle row, as an identity rather than a widget (DRAGON-603).
///
/// Naming the rows is what lets the section's SHAPE be decided by a pure function and
/// proven by a test, with the widget construction kept separate. That split is not
/// decoration here: the DRAGON-603 bug was a correct row list being discarded by a
/// later branch, which no test over the DECISION alone could ever have caught. The
/// shape has to be the thing under test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app::settings) enum ExtraRow {
    Freeze,
    Cursor,
    Transparency,
    Wallpaper,
}

impl ExtraRow {
    /// The row's settings title, which is also its stable identity in the tests.
    pub(in crate::app::settings) fn title(self) -> &'static str {
        match self {
            ExtraRow::Freeze => "Freeze pixels during selection",
            ExtraRow::Cursor => "Preserve mouse cursor",
            ExtraRow::Transparency => "Preserve window transparency",
            ExtraRow::Wallpaper => "Preserve wallpaper",
        }
    }
}

/// Pure, unit-tested (`capture_section_row_tests`): which capture-extra rows a
/// backend's CAPABILITY set offers, in display order. DRAGON-603.
///
/// One bit per row, straight off [`crate::platform::backend::Caps::capture_extras`],
/// with no second opinion about which backend is running. The persisted preferences
/// are deliberately NOT consulted: a preference decides a toggle's POSITION, never
/// whether the row exists, or turning an extra off would hide the control that turns
/// it back on.
pub(in crate::app::settings) fn capture_extra_rows(
    extras: crate::platform::backend::CaptureExtras,
) -> Vec<ExtraRow> {
    let mut rows = Vec::new();
    if extras.freeze {
        rows.push(ExtraRow::Freeze);
    }
    if extras.cursor {
        rows.push(ExtraRow::Cursor);
    }
    if extras.transparency {
        rows.push(ExtraRow::Transparency);
    }
    if extras.wallpaper {
        rows.push(ExtraRow::Wallpaper);
    }
    rows
}

/// Every row the Capture section renders, in order, as an identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app::settings) enum CaptureSectionRow {
    /// The inert red "Not found" selector: no capture method works at all.
    MethodGated,
    /// The "Capture method" dropdown.
    Method,
    /// The warn row explaining that a saved portal choice is serving through the
    /// compositor because the portal is unreachable.
    PortalUnreachable,
    /// One capture-extra toggle.
    Extra(ExtraRow),
    /// The portal's "Saved screen permission" row, with its Forget button.
    SavedPermission,
    /// The portal's "Screen access" readiness note.
    ScreenAccess,
}

/// Pure, unit-tested (`capture_section_row_tests`): the Capture section's whole row
/// list, in order. DRAGON-603.
///
/// [`crate::app::App::capture_section`] BUILDS FROM this list rather than deciding
/// alongside it, so the rendered section cannot contain a row this function omits, or
/// omit one it names. That is the entire point, and it is the specific defect this
/// ticket fixed: the extras were gated correctly by the caller and then dropped by an
/// `else` arm that only ran when the resolved method was not the portal. A Flatpak
/// session is clamped to the portal, so it lost every extra row, which is why
/// "Preserve mouse cursor" was invisible there even after DRAGON-592 made the portal
/// advertise the capability and honour it, and why "Preserve wallpaper" had been
/// invisible since DRAGON-562. The extras now sit in this list unconditionally: they
/// follow the CAPABILITY, never the method's identity.
///
/// Ordering is load-bearing and matches the pre-fix era for every non-portal session:
/// the method dropdown, then the portal-unreachable warn (which reads the saved
/// INTENT, because it exists to explain why the shown method is not the saved one),
/// then the extras, then the portal notes (which read the RESOLVED method, because
/// they describe how the serving path behaves).
pub(in crate::app::settings) fn capture_section_rows(
    capability_present: bool,
    effective_is_portal: bool,
    portal_intent_unreachable: bool,
    has_saved_token: bool,
    extras: &[ExtraRow],
) -> Vec<CaptureSectionRow> {
    // Nothing can capture: the selector is gated and the extras are moot, since there
    // is no working capture path for them to modify.
    if !capability_present {
        return vec![CaptureSectionRow::MethodGated];
    }
    let mut rows = vec![CaptureSectionRow::Method];
    if portal_intent_unreachable {
        rows.push(CaptureSectionRow::PortalUnreachable);
    }
    rows.extend(extras.iter().copied().map(CaptureSectionRow::Extra));
    if effective_is_portal {
        rows.push(if has_saved_token {
            CaptureSectionRow::SavedPermission
        } else {
            CaptureSectionRow::ScreenAccess
        });
    }
    rows
}

impl crate::app::App {
    /// The encoding-preset row for the encoder a recording will actually use: a full
    /// preset dropdown for NVENC / x264 (the whole `-preset` ladder, like OBS), or a
    /// dimmed explainer when the active encoder (VAAPI) has no usable speed preset.
    pub(in crate::app::settings) fn encoder_preset_item<'a>(&self) -> Item<'a> {
        match self.effective_encoder().as_str() {
            "nvenc" => {
                let sel = crate::encode::NVENC_PRESETS
                    .iter()
                    .position(|p| *p == self.nvenc_preset)
                    .unwrap_or(3);
                let def = crate::encode::NVENC_PRESETS
                    .iter()
                    .position(|p| *p == crate::encode::DEFAULT_NVENC_PRESET)
                    .unwrap_or(3);
                Item::new(
                    "Encoder quality preset",
                    "",
                    crate::widgets::arrow_cursor::arrow_cursor(crate::widgets::press_redraw(
                        widget::dropdown(
                            &crate::encode::NVENC_PRESET_LABELS,
                            Some(sel),
                            |a0| Msg::Settings(SettingsMsg::SetNvencPreset(a0)),
                        ),
                    )),
                )
                .reset_with(sel, def, |a0| Msg::Settings(SettingsMsg::SetNvencPreset(a0)))
            }
            "software" => {
                let sel = crate::encode::X264_PRESETS
                    .iter()
                    .position(|p| *p == self.x264_preset)
                    .unwrap_or(2);
                let def = crate::encode::X264_PRESETS
                    .iter()
                    .position(|p| *p == crate::encode::DEFAULT_X264_PRESET)
                    .unwrap_or(2);
                Item::new(
                    "Encoder quality preset",
                    "",
                    crate::widgets::arrow_cursor::arrow_cursor(crate::widgets::press_redraw(
                        widget::dropdown(
                            &crate::encode::X264_PRESET_LABELS,
                            Some(sel),
                            |a0| Msg::Settings(SettingsMsg::SetX264Preset(a0)),
                        ),
                    )),
                )
                .reset_with(sel, def, |a0| Msg::Settings(SettingsMsg::SetX264Preset(a0)))
            }
            "vaapi" => {
                let sel = crate::encode::VAAPI_CL_VALUES
                    .iter()
                    .position(|v| *v == self.vaapi_compression_level)
                    .unwrap_or(0);
                let def = crate::encode::VAAPI_CL_VALUES
                    .iter()
                    .position(|v| *v == crate::encode::DEFAULT_VAAPI_CL)
                    .unwrap_or(0);
                Item::new(
                    "Encoder quality preset",
                    "",
                    crate::widgets::arrow_cursor::arrow_cursor(crate::widgets::press_redraw(
                        widget::dropdown(
                            &crate::encode::VAAPI_CL_LABELS,
                            Some(sel),
                            |a0| Msg::Settings(SettingsMsg::SetVaapiPreset(a0)),
                        ),
                    )),
                )
                .reset_with(sel, def, |a0| Msg::Settings(SettingsMsg::SetVaapiPreset(a0)))
            }
            // Anything without a usable preset: explainer only, no control. Body
            // size, not caption: this text stands where the dropdown otherwise is,
            // and a disabled row must not read smaller than an active one
            // (DRAGON-158, same rule as the title side).
            _ => Item::new(
                "Encoder quality preset",
                "",
                widget::text::body("Driver default"),
            )
            .dim(),
        }
    }

    /// The video-codec row, filtered to what the active encoder can do: GPU encoders
    /// get Auto / H.264 / HEVC; software is H.264-only (a dimmed explainer).
    pub(in crate::app::settings) fn codec_item<'a>(&self) -> Item<'a> {
        // Software can do HEVC only when ffmpeg ships libx265; without it, H.264 only.
        if self.effective_encoder() == "software" && !crate::encode::software_supports_hevc() {
            // Body size for the same reason as the preset row's "Driver default":
            // it stands in for the dropdown and must not read smaller.
            return Item::new("Video codec", "", widget::text::body("H.264")).dim();
        }
        let sel = crate::encode::CODEC_VALUES
            .iter()
            .position(|c| *c == self.record_codec)
            .unwrap_or(0);
        let def = crate::encode::CODEC_VALUES
            .iter()
            .position(|c| *c == crate::encode::DEFAULT_CODEC)
            .unwrap_or(0);
        Item::new(
            "Video codec",
            "",
            crate::widgets::arrow_cursor::arrow_cursor(crate::widgets::press_redraw(widget::dropdown(&crate::encode::CODEC_LABELS, Some(sel), |a0| Msg::Settings(SettingsMsg::SetRecordCodec(a0))))),
        )
        .reset_with(sel, def, |a0| Msg::Settings(SettingsMsg::SetRecordCodec(a0)))
    }

    /// A heads-up shown under the codec choice when the resolution settings would push
    /// an encode past a limit and force a downscale (or, for Auto on a hardware encoder,
    /// a silent switch to HEVC). Only fires when the size is actually KNOWN to exceed —
    /// a fixed preset's dimensions, a Custom size, or the largest known display for
    /// "Original" — so it never false-positives on small screens. `None` otherwise.
    /// (H.264 ≤ 4096 px, HEVC ≤ 8192 px on NVENC/VAAPI/VideoToolbox; the SOFTWARE x264
    /// path additionally caps to a real-time-sustainable side per frame rate — DRAGON-162.)
    pub(in crate::app::settings) fn codec_size_warning(&self) -> Option<String> {
        let backend = self.effective_encoder();
        if backend != "nvenc" && backend != "vaapi" && backend != "videotoolbox"
            && backend != "software"
        {
            return None;
        }
        let preset = self.record_res_preset as usize;
        let side: u32 = if preset == RES_CUSTOM {
            self.record_max_width.value.max(self.record_max_height.value)
        } else if preset == 0 {
            // "Original": the biggest display we know about (may be unknown here). On
            // macOS this is the logical/points side; the physical capture is `× scale`
            // larger, so a 5K display reads ~2560 here — the software cap below is set
            // below that, so the note still fires for a Retina 5K/4K target.
            self.outputs
                .iter()
                .map(|o| o.logical_size.0.max(o.logical_size.1))
                .max()
                .filter(|&m| m > 0)?
        } else {
            let (w, h) = res_dims(preset);
            w.max(h)
        };
        // The SOFTWARE path caps to a real-time-sustainable side (DRAGON-162): a 5K/4K
        // capture is downscaled so x264 keeps the frame rate instead of freezing. This
        // is independent of the codec choice, so check it first.
        if backend == "software" {
            let cap = crate::encode::software_realtime_max_side(self.record_fps.value.max(1));
            if side > cap {
                return Some(format!(
                    "The software encoder can't keep {} fps at this size, so a large \
                     capture is downscaled to about {cap}px to stay smooth. Pick the \
                     hardware encoder to record at full resolution.",
                    self.record_fps.value.max(1)
                ));
            }
            return None;
        }
        match self.record_codec.as_str() {
            "h264" if side > 4096 => Some(format!(
                "A full capture here (~{side}px) is over H.264's 4096px limit, so it \
                 will be downscaled to fit. Choose HEVC or Auto to keep full resolution."
            )),
            "hevc" if side > 8192 => Some(format!(
                "A full capture here (~{side}px) is over HEVC's 8192px limit and will be \
                 downscaled to fit."
            )),
            "auto" if side > 4096 => Some(format!(
                "A full capture here (~{side}px) is over 4096px, so it will use HEVC, \
                 which plays in fewer places than H.264. Choose H.264 to force \
                 compatibility (it downscales instead)."
            )),
            _ => None,
        }
    }

    /// The shared "Capture" section (method selector + portal notes), used by both
    /// the Screenshots and Recordings pages. `noun` is the capitalised plural used
    /// in the copy ("Screenshots" / "Recordings"). The dropdown enumerates
    /// `picker.methods` — this environment's backends with the relevant capability,
    /// derived from `platform::backend::backends()` (`App::rebuild_capture_methods`)
    /// — so a new backend shows up here by registering itself there, and a
    /// single-backend platform (macOS) naturally gets a one-entry list.
    ///
    /// `extra_rows` are the caller's capability-gated toggle rows, each paired with the
    /// [`ExtraRow`] identity it renders (the Screenshots page's capture extras; the
    /// Recordings page passes none). This function does NOT decide which of them to
    /// show: it renders exactly the plan [`capture_section_rows`] returns, so the
    /// section's shape is decided once, in a pure function, and proven by a test.
    ///
    /// DRAGON-603: the extras used to be a bare `Vec<Item>` called `cosmic_extra`,
    /// rendered inside a not-the-portal `else` arm, which silently discarded all of
    /// them on any portal session. Building from the plan is what makes that class of
    /// mistake unrepresentable rather than merely fixed.
    pub(in crate::app::settings) fn capture_section<'a>(
        &'a self,
        capability_present: bool,
        picker: MethodPicker<'a>,
        noun: &'static str,
        extra_rows: Vec<(ExtraRow, Item<'a>)>,
    ) -> SectionSpec<'a> {
        let MethodPicker { methods, selected, default_id, setter } = picker;
        // The dropdown DISPLAYS the resolved method (DRAGON-575, the DRAGON-571
        // intent+display shape): the saved backend when this session offers it,
        // otherwise the method that will really serve: so a persisted native id in
        // a portal-only sandbox shows the portal, and a one-entry list is always
        // selected. Display-only: the persisted intent is rewritten by nothing but
        // a real dropdown click (`setter`). The notes below follow the RESOLVED
        // method (they describe how the serving path behaves); only the
        // portal-unreachable warn keeps reading the INTENT, because it explains why
        // the shown method is not the saved one.
        let cur = methods.display_position(selected);
        let effective = methods.display_id(selected).unwrap_or(selected);
        let ids = methods.ids.clone();
        // The whole shape is decided ONCE, here, by the pure planner. Everything below
        // is construction: each row the plan names gets built, and nothing else is.
        let extra_ids: Vec<ExtraRow> = extra_rows.iter().map(|(r, _)| *r).collect();
        let plan = capture_section_rows(
            capability_present,
            effective == crate::platform::backend::PORTAL_ID,
            selected == crate::platform::backend::PORTAL_ID && !self.pipewire_available,
            self.pw_restore_token.is_some(),
            &extra_ids,
        );
        // How the portal path behaves, folded into whichever portal row renders
        // (after a blank line) instead of a separate note box.
        let portal_note = "Capture requests must be approved through the system portal. \
             For region selections, you should choose the monitor that contains your \
             selected region.";
        let mut extras: Vec<Option<Item<'a>>> = extra_rows.into_iter().map(|(_, i)| Some(i)).collect();
        let mut method_cell = Some(
            Item::new(
                "Capture method",
                "",
                crate::widgets::arrow_cursor::arrow_cursor(crate::widgets::press_redraw(widget::dropdown(&methods.labels, cur, move |i| setter(ids[i].to_string())))),
            )
            .reset_with(selected.to_string(), default_id, setter),
        );
        let mut items: Vec<Item<'a>> = Vec::with_capacity(plan.len());
        for row in plan {
            match row {
                // No capture method works: an inert red selector, and the extras are
                // moot without a working capture path (the planner omits them).
                CaptureSectionRow::MethodGated => {
                    items.push(gated_row("Capture method", "Not found", Severity::Error));
                }
                // `method_cell` is a one-shot because the dropdown closure owns
                // `ids`/`setter`; the planner names this row exactly once.
                CaptureSectionRow::Method => {
                    if let Some(item) = method_cell.take() {
                        items.push(item);
                    }
                }
                // Intent-vs-served hint: a saved portal choice while the portal is
                // unreachable serves through the compositor (the dispatch clamp); the
                // dropdown above already shows the serving method, this says why it is
                // not the saved one.
                CaptureSectionRow::PortalUnreachable => items.push(
                    Item::new(
                        "Screen access",
                        format!(
                            "ScreenCast portal not reachable. {noun} fall back to the COSMIC \
                             compositor."
                        ),
                        widget::text(""),
                    )
                    .status(Severity::Warn),
                ),
                // DRAGON-603: rendered for EVERY resolved method. The capability table
                // already decided which extras exist; nothing here may re-decide it.
                CaptureSectionRow::Extra(id) => {
                    if let Some(slot) = extra_ids.iter().position(|r| *r == id)
                        && let Some(item) = extras.get_mut(slot).and_then(Option::take)
                    {
                        items.push(item);
                    }
                }
                CaptureSectionRow::SavedPermission => items.push(Item::new(
                    "Saved screen permission",
                    format!(
                        "{noun} reuse the screen access you previously granted. Forget it to be \
                         asked again next time.\n\n{}\n\n{portal_note}",
                        crate::app::portal::FORGET_SCREEN_ACCESS_NOTE
                    ),
                    crate::widgets::arrow_cursor::arrow_cursor(widget::button::standard("Forget").on_press(Msg::Settings(SettingsMsg::ResetScreencastPermission))),
                )),
                CaptureSectionRow::ScreenAccess => {
                    // The portal is reachable by construction here: an unreachable
                    // portal is never offered, so it cannot be the resolved method.
                    let t = self.pipewire_source_types;
                    let mut kinds: Vec<&str> = Vec::new();
                    if t & 1 != 0 {
                        kinds.push("monitor");
                    }
                    if t & 2 != 0 {
                        kinds.push("window");
                    }
                    if t & 4 != 0 {
                        kinds.push("virtual");
                    }
                    items.push(Item::new(
                        "Screen access",
                        format!("ScreenCast portal ready ({}).\n\n{portal_note}", kinds.join(" + ")),
                        widget::text(""),
                    ));
                }
            }
        }
        SectionSpec {
            title: "Capture",
            items,
        }
    }
}

/// DRAGON-603: the Capture section's SHAPE, pinned platform-free. Its own module
/// because it guards one promise, that a capability-gated extra row reaches the
/// rendered section on EVERY capture method, which is exactly what an `else` arm
/// keyed on the portal used to break.
///
/// These tests deliberately assert the row LIST rather than any widget. A test over
/// the extras decision alone would have passed throughout the bug, because the
/// decision was already right and a later branch discarded it, so the shape is the
/// only thing worth pinning.
#[cfg(test)]
mod capture_section_row_tests {
    use super::{CaptureSectionRow, ExtraRow, capture_extra_rows, capture_section_rows};
    use crate::platform::backend::CaptureExtras;
    #[cfg(target_os = "linux")]
    use crate::platform::backend::{CaptureBackend, PortalBackend, ScreencopyBackend};

    /// The real PORTAL capability set, read off the backend itself rather than a
    /// literal, so this tracks `PortalBackend::caps()` instead of a copy of it. A
    /// reachable portal with ffmpeg present is the Flatpak session the owner reported.
    #[cfg(target_os = "linux")]
    fn portal_extras() -> CaptureExtras {
        PortalBackend { available: true, ffmpeg: true }.caps().capture_extras()
    }

    /// THE regression pin, and the four-row prediction this ticket was closed on.
    /// A portal session offers exactly the cursor and wallpaper rows: `cursor_toggle`
    /// since DRAGON-592 and `wallpaper_compose` since DRAGON-562. Freeze and
    /// transparency stay absent because the portal genuinely cannot do them, a live
    /// frame having nothing to freeze and `convert_crop` forcing every pixel opaque.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_portal_session_offers_exactly_the_cursor_and_wallpaper_rows() {
        let rows = capture_extra_rows(portal_extras());
        assert_eq!(rows, vec![ExtraRow::Cursor, ExtraRow::Wallpaper]);
        assert!(!rows.contains(&ExtraRow::Freeze), "a live portal frame cannot be frozen");
        assert!(!rows.contains(&ExtraRow::Transparency), "the portal forces pixels opaque");
    }

    /// And those rows SURVIVE into the rendered section on a portal session. This is
    /// the assertion the bug would have failed: the extras were computed correctly and
    /// then dropped, so the section held only the method row and the portal note.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_portal_section_actually_contains_the_extra_rows() {
        let extras = capture_extra_rows(portal_extras());
        let plan = capture_section_rows(true, true, false, false, &extras);
        assert_eq!(
            plan,
            vec![
                CaptureSectionRow::Method,
                CaptureSectionRow::Extra(ExtraRow::Cursor),
                CaptureSectionRow::Extra(ExtraRow::Wallpaper),
                CaptureSectionRow::ScreenAccess,
            ]
        );
    }

    /// The native session is unchanged: a full-capability compositor still offers all
    /// four extras, in the historical order, with no portal note.
    #[test]
    fn a_native_session_still_offers_all_four_extras_in_order() {
        let all = CaptureExtras {
            freeze: true,
            cursor: true,
            transparency: true,
            wallpaper: true,
            fullscreen_aware: true,
        };
        let extras = capture_extra_rows(all);
        assert_eq!(
            extras,
            vec![ExtraRow::Freeze, ExtraRow::Cursor, ExtraRow::Transparency, ExtraRow::Wallpaper]
        );
        let plan = capture_section_rows(true, false, false, false, &extras);
        assert_eq!(plan.first(), Some(&CaptureSectionRow::Method));
        assert_eq!(plan.len(), 5, "method + four extras, and no portal note");
        assert!(!plan.contains(&CaptureSectionRow::ScreenAccess));
        assert!(!plan.contains(&CaptureSectionRow::SavedPermission));
    }

    /// The generalised form of the bug: for ANY capability set, every offered extra
    /// reaches the section under BOTH resolved methods. Stated over the whole power
    /// set so no future branch can quietly make the row list method-dependent again.
    #[test]
    fn every_offered_extra_reaches_the_section_on_every_method() {
        for bits in 0u8..16 {
            let extras = CaptureExtras {
                freeze: bits & 1 != 0,
                cursor: bits & 2 != 0,
                transparency: bits & 4 != 0,
                wallpaper: bits & 8 != 0,
                fullscreen_aware: true,
            };
            let offered = capture_extra_rows(extras);
            for is_portal in [true, false] {
                let plan = capture_section_rows(true, is_portal, false, false, &offered);
                for row in &offered {
                    assert!(
                        plan.contains(&CaptureSectionRow::Extra(*row)),
                        "bits={bits} portal={is_portal} lost {row:?}"
                    );
                }
            }
        }
    }

    /// A preference must never hide its own row. The row list reads the CAPABILITY
    /// only, so turning "Preserve mouse cursor" off leaves the toggle on screen to
    /// turn it back on. The scanner's DRAGON-604 veto is likewise a capture-time rule,
    /// never a settings-visibility one.
    #[test]
    fn a_preference_never_hides_its_own_row() {
        let caps_on = CaptureExtras {
            freeze: false,
            cursor: true,
            transparency: false,
            wallpaper: false,
            fullscreen_aware: true,
        };
        assert_eq!(capture_extra_rows(caps_on), vec![ExtraRow::Cursor]);
    }

    /// No working capture method: the selector is gated and nothing else renders,
    /// extras included, because they modify a path that does not exist.
    #[test]
    fn a_dead_session_gates_the_selector_and_drops_everything_else() {
        let all = [ExtraRow::Freeze, ExtraRow::Cursor];
        assert_eq!(
            capture_section_rows(false, true, true, true, &all),
            vec![CaptureSectionRow::MethodGated]
        );
    }

    /// Ordering, which is load-bearing for the non-portal sessions whose layout must
    /// stay byte-identical: the unreachable-portal warn sits between the dropdown and
    /// the extras, and the portal note always comes last.
    #[test]
    fn the_row_order_is_method_then_warn_then_extras_then_notes() {
        let extras = [ExtraRow::Cursor];
        assert_eq!(
            capture_section_rows(true, false, true, false, &extras),
            vec![
                CaptureSectionRow::Method,
                CaptureSectionRow::PortalUnreachable,
                CaptureSectionRow::Extra(ExtraRow::Cursor),
            ]
        );
        assert_eq!(
            capture_section_rows(true, true, false, true, &extras),
            vec![
                CaptureSectionRow::Method,
                CaptureSectionRow::Extra(ExtraRow::Cursor),
                CaptureSectionRow::SavedPermission,
            ]
        );
    }

    /// A native backend's real capability set still yields all four rows, so the
    /// portal assertions above are about the portal and not about the planner.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_real_native_backend_offers_all_four() {
        let protocols = crate::platform::backend::WaylandProtocols {
            image_copy_capture: true,
            output_source: true,
            toplevel_source: true,
            toplevel_list: true,
            cosmic_toplevel_info: true,
            cosmic_toplevel_manager: true,
            layer_shell: true,
            data_control: true,
        };
        let caps = ScreencopyBackend { protocols, ffmpeg: true }.caps();
        assert_eq!(capture_extra_rows(caps.capture_extras()).len(), 4);
    }

    /// Titles are the rows' stable identity in these tests and the user-visible copy;
    /// pinned so a rename has to be deliberate.
    #[test]
    fn row_titles_are_stable() {
        assert_eq!(ExtraRow::Cursor.title(), "Preserve mouse cursor");
        assert_eq!(ExtraRow::Wallpaper.title(), "Preserve wallpaper");
        assert_eq!(ExtraRow::Freeze.title(), "Freeze pixels during selection");
        assert_eq!(ExtraRow::Transparency.title(), "Preserve window transparency");
    }
}
