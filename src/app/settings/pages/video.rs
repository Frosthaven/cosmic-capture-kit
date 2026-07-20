//! Video settings page section builder.

use super::super::*;
use super::super::row::{num_input, success_caption, Item, SectionSpec, Severity};
use super::super::deps::DepId;
#[cfg(feature = "zero-copy")]
use super::super::row::toggle;

impl crate::app::App {
    /// Video settings page: resolution / bitrate / frame rate, the encoder + its
    /// preset / codec, and the experimental GPU zero-copy + benchmark group.
    pub(in crate::app::settings) fn video_sections(&self) -> Vec<SectionSpec<'_>> {
        let d = crate::state::defaults();
        let mut secs: Vec<SectionSpec<'_>> = Vec::new();
        // Frame rate / bitrate / max resolution head the Video group built below.
        let video_items = {
                let mut items = vec![
                    Item::new(
                        "Frame rate",
                        "",
                        num_input("30", &self.record_fps.text, Some(|a0| Msg::Settings(SettingsMsg::SetRecordFps(a0)))),
                    )
                    .suffix("fps")
                    .reset_with(
                        self.record_fps.text.clone(),
                        d.record_fps.to_string(),
                        |a0| Msg::Settings(SettingsMsg::SetRecordFps(a0)),
                    ),
                    Item::new(
                        "Max bitrate",
                        "",
                        num_input("8000", &self.record_bitrate_kbps.text, Some(|a0| Msg::Settings(SettingsMsg::SetRecordBitrate(a0)))),
                    )
                    .suffix("Kbps")
                    .reset_with(
                        self.record_bitrate_kbps.text.clone(),
                        d.record_bitrate_kbps.to_string(),
                        |a0| Msg::Settings(SettingsMsg::SetRecordBitrate(a0)),
                    ),
                    Item::new(
                        "Max resolution",
                        "",
                        widget::dropdown(
                            &RES_LABELS,
                            Some(self.record_res_preset as usize),
                            |a0| Msg::Settings(SettingsMsg::SetRecordResPreset(a0)),
                        ),
                    )
                    .reset_with(
                        self.record_res_preset as usize,
                        d.record_res_preset as usize,
                        |a0| Msg::Settings(SettingsMsg::SetRecordResPreset(a0)),
                    ),
                ];
                if self.record_res_preset as usize == RES_CUSTOM {
                    items.push(
                        Item::new(
                            "Max width",
                            "",
                            num_input(
                                "1920",
                                &self.record_max_width.text,
                                Some(|a0| Msg::Settings(SettingsMsg::SetRecordMaxWidth(a0))),
                            ),
                        )
                        .suffix("px")
                        .reset_with(
                            self.record_max_width.text.clone(),
                            d.record_max_width.to_string(),
                            |a0| Msg::Settings(SettingsMsg::SetRecordMaxWidth(a0)),
                        ),
                    );
                    items.push(
                        Item::new(
                            "Max height",
                            "",
                            num_input(
                                "1080",
                                &self.record_max_height.text,
                                Some(|a0| Msg::Settings(SettingsMsg::SetRecordMaxHeight(a0))),
                            ),
                        )
                        .suffix("px")
                        .reset_with(
                            self.record_max_height.text.clone(),
                            d.record_max_height.to_string(),
                            |a0| Msg::Settings(SettingsMsg::SetRecordMaxHeight(a0)),
                        ),
                    );
                }
                items
        };

        // Video group: frame rate / bitrate / max resolution first, then the encoder
        // and its preset / codec, the downscale note, GPU zero-copy, and the benchmark.
        let mut enc_items = video_items;
        // Windows (DRAGON-238): the encoder / preset / codec rows all read the probed
        // encoder list, which resolves OFF the UI thread (its ffmpeg `-encoders` + hardware
        // probe-encodes take seconds). Render those rows only once the peek is ready;
        // otherwise a lightweight "detecting…" placeholder — never the blocking `encoders()`.
        // Linux/mac probe synchronously on first read (timing untouched).
        #[cfg(windows)]
        let encoders_ready = self.encoders_peek().is_some();
        #[cfg(not(windows))]
        let encoders_ready = true;
        if encoders_ready {
            let encoders = self.encoders();
            let preferred_encoder = self.preferred_encoder();
            let selected = encoders
                .iter()
                .position(|e| e.id == preferred_encoder);
            enc_items.push(
                Item::new(
                    "Encoder",
                    "",
                    widget::dropdown(encoders, selected, |a0| Msg::Settings(SettingsMsg::SetPreferredEncoder(a0))),
                )
                // Default = the best available encoder (index 0).
                .reset_with(selected.unwrap_or(0), 0, |a0| Msg::Settings(SettingsMsg::SetPreferredEncoder(a0))),
            );
            // Surface the hardware-encoder note only when there's a problem; the Health page
            // lists it regardless.
            if let Some(note) = self.dep(DepId::HwEncoder).note_if_issue() {
                enc_items.push(note);
            }
            enc_items.push(self.encoder_preset_item());
            enc_items.push(self.codec_item());
            // Heads-up if the codec + resolution settings would force a downscale.
            if let Some(warn) = self.codec_size_warning() {
                enc_items.push(Item::new("Resolution note", warn, widget::text("")).status(Severity::Warn));
            }
        }
        #[cfg(windows)]
        if !encoders_ready {
            enc_items.push(
                Item::new("Encoder", "Detecting available encoders…", widget::text("")).dim(),
            );
        }
        secs.push(SectionSpec {
            title: "Video",
            items: enc_items,
        });

        // Dedicated Experimental group at the bottom: GPU zero-copy + the benchmark.
        let mut exp_items: Vec<Item<'_>> = Vec::new();
        // GPU zero-copy needs a hardware encoder and a reachable capture path; it
        // negotiates DMA-BUF frames and encodes them on the buffer's own GPU, falling
        // back to the CPU path when that can't be done.
        #[cfg(feature = "zero-copy")]
        if self.effective_encoder() != "software"
            && (self.record_backend != crate::platform::backend::PORTAL_ID
                || self.pipewire_available)
        {
            exp_items.push(
                Item::new(
                    "GPU zero-copy capture",
                    "Performance setting to preprocess frames on the GPU instead of the \
                     CPU when available.",
                    toggle(self.record_zero_copy, |a0| Msg::Settings(SettingsMsg::SetRecordZeroCopy(a0))),
                )
                .reset_with(self.record_zero_copy, d.record_zero_copy, |a0| Msg::Settings(SettingsMsg::SetRecordZeroCopy(a0))),
            );
        }
        // Monitor picker (DRAGON-163): the benchmark runs against the SELECTED monitor's
        // TRUE capture footprint, so its verdict predicts real recording of that display.
        // Only shown when monitors enumerated (settings launch with capture permission).
        if !self.bench_monitors.is_empty() {
            // Owned label clones so the dropdown's `Cow` owns them (no borrow of `self`
            // escaping into the returned `Item<'a>`).
            let labels: Vec<String> =
                self.bench_monitors.iter().map(|m| m.label.clone()).collect();
            let selected = self.bench_monitor_idx.min(labels.len().saturating_sub(1));
            exp_items.push(Item::new(
                "Benchmark monitor",
                "The encoder benchmark tests this monitor's true capture resolution.",
                widget::dropdown(labels, Some(selected), |i| {
                    Msg::Settings(SettingsMsg::SetBenchMonitor(i))
                }),
            ));
        }
        exp_items.push(Item::new(
            "Benchmark encoders",
            "Encoders that appear in green can sustain your currently configured frame \
             rate. Encoders that use fewer cores will leave more processing for other \
             programs.",
            widget::button::standard("Run benchmark").on_press(Msg::Settings(SettingsMsg::RunBenchmark)),
        ));
        if let Some(b) = self.bench.as_ref().and_then(|b| b.lock().ok()) {
            if b.finished {
                // Name the monitor tested, so a size-driven verdict is diagnosable at a
                // glance (a green VideoToolbox + capped software on a 6K display, etc).
                if !b.monitor_label.is_empty() {
                    exp_items.push(
                        Item::new("Tested monitor", b.monitor_label.clone(), widget::text(""))
                            .dim(),
                    );
                }
                // Rank by fitness for recording, not raw speed: encoders that sustain
                // the configured frame rate first, cheapest CPU among them (a hardware
                // encoder's fps ceiling is pacing/wrapper overhead, not capability —
                // see DRAGON-133); the rest by fps.
                let target = self.record_fps.value as f32;
                let mut results: Vec<&crate::app::BenchResult> = b.results.iter().collect();
                results.sort_by(|x, y| {
                    let (xm, ym) = (x.score.fps >= target, y.score.fps >= target);
                    ym.cmp(&xm).then_with(|| {
                        if xm && ym {
                            x.score.cores.partial_cmp(&y.score.cores)
                        } else {
                            y.score.fps.partial_cmp(&x.score.fps)
                        }
                        .unwrap_or(std::cmp::Ordering::Equal)
                    })
                });
                for r in results {
                    let score = &r.score;
                    // The dimensions + codec this encoder actually resolved to for the
                    // tested monitor: makes a downscale (software 5K) or the h264->HEVC
                    // route (above 4096) visible, so future size regressions are obvious.
                    let codec = if r.is_hevc { "HEVC" } else { "H.264" };
                    let desc = format!("{}x{} {}", r.enc_w, r.enc_h, codec);
                    // Green when the encoder sustains at least the configured frame rate.
                    let caption = if score.fps > 0.0 {
                        let val = if score.cores > 0.0 {
                            format!("{:.0} fps, ~{:.1} CPU cores", score.fps, score.cores)
                        } else {
                            format!("{:.0} fps", score.fps)
                        };
                        if score.fps >= target {
                            success_caption(val)
                        } else {
                            widget::text::caption(val).into()
                        }
                    } else {
                        widget::text::caption("unsupported".to_string()).into()
                    };
                    exp_items.push(Item::new(r.label.clone(), desc, caption).dim());
                }
            } else {
                let frac = if b.total > 0 { b.done as f32 / b.total as f32 } else { 0.0 };
                exp_items.push(
                    Item::new(
                        format!("Testing {} ({}/{})", b.current, b.done, b.total),
                        "",
                        widget::container(cosmic::iced::widget::progress_bar(0.0..=1.0, frac))
                            .width(Length::Fixed(160.0)),
                    )
                    .dim(),
                );
            }
        }
        secs.push(SectionSpec {
            title: "Experimental",
            items: exp_items,
        });

        secs
    }
}
