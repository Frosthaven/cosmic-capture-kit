//! Scanner settings page section builder.

use super::super::*;
use super::super::row::{gated_row, toggle, Item, SectionSpec, Severity};
use super::super::deps::DepId;

impl crate::app::App {
    pub(in crate::app::settings) fn scanner_sections(&self) -> Vec<SectionSpec<'_>> {
        let d = crate::state::defaults();
        // OCR depends on the `tesseract` binary, via the same `dep` the Health page
        // uses. When it's missing, gate the enable toggle (inert + tinted) and explain
        // it with the note; the strictness slider only applies when OCR can run.
        let tess = self.dep(DepId::Tesseract).is_present();
        let ocr_toggle = if tess {
            Item::new(
                "Enable text recognition (OCR)",
                "Recognized words become naturally selectable.",
                toggle(self.scan_text, |a0| Msg::Detect(DetectMsg::SetScanText(a0))),
            )
            .reset_with(self.scan_text, d.scan_text, |a0| Msg::Detect(DetectMsg::SetScanText(a0)))
        } else {
            // tesseract missing: a None-handler toggler still renders enabled, so show the
            // state as inert subdued text (tinted), with the note below explaining why.
            gated_row(
                "Enable text recognition (OCR)",
                if self.scan_text { "On" } else { "Off" },
                Severity::Warn,
            )
        };
        let mut ocr_items = vec![ocr_toggle];
        // Surface the note only when there's a problem (the Health page lists it
        // regardless); the gated toggle above already shows the unavailable state.
        if let Some(note) = self.dep(DepId::Tesseract).note_if_issue() {
            ocr_items.push(note);
        }
        if let Some(note) = self.dep(DepId::TesseractLang).note_if_issue() {
            ocr_items.push(note);
        }
        if tess && self.scan_text {
            ocr_items.push(
                Item::new(
                    "Text matching strictness",
                    "",
                    widget::row(vec![
                        widget::slider(
                            0.0..=60.0,
                            self.text_confidence,
                            |a0| Msg::Detect(DetectMsg::SetTextConfidence(a0)),
                        )
                        .step(1.0_f32)
                        .width(Length::Fixed(200.0))
                        .into(),
                        widget::text(format!("{:.0}", self.text_confidence))
                            .size(13)
                            .into(),
                    ])
                    .spacing(8.0)
                    .align_y(Alignment::Center),
                )
                .reset_with(
                    self.text_confidence,
                    d.text_confidence,
                    |a0| Msg::Detect(DetectMsg::SetTextConfidence(a0)),
                ),
            );
        }
        vec![
            SectionSpec {
                title: "QR/Barcode Recognition",
                items: vec![
                    Item::new(
                        "Enable QR/barcode recognition",
                        "Recognized QR codes and barcodes become interactible.",
                        toggle(self.scan_codes, |a0| Msg::Detect(DetectMsg::SetScanCodes(a0))),
                    )
                    .reset_with(self.scan_codes, d.scan_codes, |a0| Msg::Detect(DetectMsg::SetScanCodes(a0))),
                ],
            },
            SectionSpec {
                title: "Text Recognition (OCR)",
                items: ocr_items,
            },
        ]
    }
}
