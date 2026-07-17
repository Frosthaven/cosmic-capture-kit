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
                    widget::dropdown(
                        &crate::encode::NVENC_PRESET_LABELS,
                        Some(sel),
                        |a0| Msg::Settings(SettingsMsg::SetNvencPreset(a0)),
                    ),
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
                    widget::dropdown(
                        &crate::encode::X264_PRESET_LABELS,
                        Some(sel),
                        |a0| Msg::Settings(SettingsMsg::SetX264Preset(a0)),
                    ),
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
                    widget::dropdown(
                        &crate::encode::VAAPI_CL_LABELS,
                        Some(sel),
                        |a0| Msg::Settings(SettingsMsg::SetVaapiPreset(a0)),
                    ),
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
            widget::dropdown(&crate::encode::CODEC_LABELS, Some(sel), |a0| Msg::Settings(SettingsMsg::SetRecordCodec(a0))),
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
    pub(in crate::app::settings) fn capture_section<'a>(
        &'a self,
        capability_present: bool,
        picker: MethodPicker<'a>,
        noun: &'static str,
        cosmic_extra: Vec<Item<'a>>,
    ) -> SectionSpec<'a> {
        let MethodPicker { methods, selected, default_id, setter } = picker;
        // No capture method works: gate the selector (inert + red) and drop the
        // method-specific extras, which are moot without a working capture path.
        if !capability_present {
            return SectionSpec {
                title: "Capture",
                items: vec![gated_row("Capture method", "Not found", Severity::Error)],
            };
        }
        // `cur` is None when the saved backend isn't offered right now (a portal
        // choice while the portal is unreachable) — the dropdown shows no selection
        // and the Screen access note below explains the fallback.
        let cur = methods.position(selected);
        let ids = methods.ids.clone();
        let mut items: Vec<Item<'a>> = vec![
            Item::new(
                "Capture method",
                "",
                widget::dropdown(&methods.labels, cur, move |i| setter(ids[i].to_string())),
            )
            .reset_with(selected.to_string(), default_id, setter),
        ];
        if selected == crate::platform::backend::PORTAL_ID {
            // How the portal path behaves — folded into the Screen access row below
            // (after a blank line) instead of a separate note box.
            let portal_note = "Capture requests must be approved through the system portal. \
                 For region selections, you should choose the monitor that contains your \
                 selected region.";
            if !self.pipewire_available {
                items.push(
                    Item::new(
                        "Screen access",
                        format!(
                            "ScreenCast portal not reachable. {noun} fall back to the COSMIC \
                             compositor."
                        ),
                        widget::text(""),
                    )
                    .status(Severity::Warn),
                );
            } else if self.pw_restore_token.is_some() {
                items.push(Item::new(
                    "Saved screen permission",
                    format!(
                        "{noun} reuse the screen access you previously granted. Forget it to be \
                         asked again next time.\n\n{portal_note}"
                    ),
                    widget::button::standard("Forget").on_press(Msg::Settings(SettingsMsg::ResetScreencastPermission)),
                ));
            } else {
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
        } else {
            // Options that only apply to direct COSMIC capture.
            items.extend(cosmic_extra);
        }
        SectionSpec {
            title: "Capture",
            items,
        }
    }
}
