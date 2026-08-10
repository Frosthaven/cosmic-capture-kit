//! `DetectMsg` handling — QR/barcode marks and OCR text selection.
//! Split from `application.rs` (DRAGON-115).

use super::super::*;

impl App {
    pub(in crate::app) fn update_detect(&mut self, message: DetectMsg) -> Task<cosmic::Action<Msg>> {
        match message { // re-render to show progress/results
            DetectMsg::SetScanCodes(b) => {
                self.scan_codes = b;
                if !b {
                    // Stop scanning + drop its marks.
                    self.code_marks.clear();
                    self.last_code_region = None;
                }
                self.rebuild_marks();
                self.save_state();
                Task::none()
            }
            DetectMsg::SetScanText(b) => {
                self.scan_text = b;
                if !b {
                    // Stop OCR + drop its words / selection.
                    self.text_words.clear();
                    self.hovered_word = None;
                    self.text_sel.clear();
                    self.text_menu = None;
                    self.last_ocr_region = None;
                }
                self.rebuild_marks();
                self.save_state();
                Task::none()
            }
            DetectMsg::SetTextConfidence(v) => {
                self.text_confidence = v.clamp(0.0, 60.0);
                // Re-OCR the current region so the new threshold takes effect.
                self.last_ocr_region = None;
                self.save_state();
                Task::none()
            }
            DetectMsg::SetOcrLanguage(idx) => {
                // Every row is a real installed language now, so the index maps straight
                // across. An out-of-range index (a stale view against a refreshed list)
                // clears to empty, which is the pass-no-`-l` default.
                self.ocr_language = self.ocr_langs.get(idx).cloned().unwrap_or_default();
                // Re-OCR the current region so the new language takes effect immediately,
                // exactly as the confidence slider does.
                self.last_ocr_region = None;
                self.save_state();
                Task::none()
            }
            DetectMsg::MarksPoll => {
                use std::sync::atomic::Ordering::Relaxed;
                let mut changed = false;
                // Load finished QR + OCR passes. Results landing AFTER the scanner
                // was toggled off still go into the cache (the display is gated on
                // scanner mode, so nothing shows) — that keeps the cached marks
                // in sync with their `last_*_region` keys, so re-enabling over the
                // same region shows them without reprocessing.
                if let Some(codes) = self.code_scan.lock().ok().and_then(|mut g| g.take()) {
                    self.code_marks = codes;
                    self.code_busy.store(false, Relaxed);
                    changed = true;
                }
                if let Some(text) = self.text_scan.lock().ok().and_then(|mut g| g.take()) {
                    self.text_words = text;
                    // A re-OCR invalidates any indices the old selection referenced.
                    self.hovered_word = None;
                    self.text_sel.clear();
                    self.text_menu = None;
                    self.ocr_busy.store(false, Relaxed);
                    changed = true;
                }
                let region = self.normalized_region();
                // QR/barcode + OCR run in region mode, photo or video alike.
                let in_region = self.mode == Mode::Region;
                let want_codes = self.kind == Kind::Scanner
                    && self.scan_codes
                    && in_region
                    && region != self.last_code_region
                    && !self.region_dragging
                    && !self.code_busy.load(Relaxed);
                let want_ocr = self.kind == Kind::Scanner
                    && self.scan_text
                    && self.tesseract_available
                    && in_region
                    && region != self.last_ocr_region
                    && !self.region_dragging
                    && !self.ocr_busy.load(Relaxed);
                // DRAGON-460: the scan source is a LIVE shot of the selection, not a crop of
                // the launch-instant flats. What is on screen and what gets read are the
                // same pixels because they ARE the same pixels — no freeze, no backdrop, and
                // nothing to go stale behind the user's back.
                //
                // The shot is asynchronous (a real screen capture, tens of ms), so it cannot
                // be taken inline here the way `crop_frozen`'s memory copy could. The
                // sequence is: decide a read is wanted -> `begin_scan_shot` clears the marks
                // -> one tick later the shot is taken off-thread -> it lands in
                // `scan_shot_slot` -> this poll picks it up and runs the passes against it.
                //
                // ONE shot feeds both scanners, as one crop used to: QR and OCR settle on
                // the same region in the common case and must not photograph the screen
                // twice (they would also be reading two DIFFERENT instants of it).
                let landed = self.scan_shot_slot.lock().ok().and_then(|mut g| g.take());
                let crop: Option<std::sync::Arc<image::RgbaImage>> = match landed {
                    Some(shot) => {
                        self.scan_shot = ScanShot::Idle;
                        // `Some(None)` = the platform returned nothing. Leave the previous
                        // answer standing rather than clearing to an empty result: a failed
                        // read must never be indistinguishable from "there is nothing here".
                        shot.map(std::sync::Arc::new)
                    }
                    None => {
                        // No shot in hand. If one is wanted and none is in flight, start it;
                        // the passes run on the next poll once it lands.
                        if (want_codes || want_ocr) && self.scan_shot == ScanShot::Idle {
                            self.begin_scan_shot();
                        }
                        None
                    }
                };
                // (Re-)scan the region's QR codes / barcodes when it settles + changes.
                if want_codes {
                    if let (Some((x, y, w, h)), Some(img)) = (region, crop.clone()) {
                        self.last_code_region = region;
                        self.code_busy.store(true, Relaxed);
                        let out = self.code_scan.clone();
                        std::thread::spawn(move || {
                            let marks = crate::detect::scan_codes(&img, x, y, w, h);
                            if let Ok(mut g) = out.lock() {
                                *g = Some(marks);
                            }
                        });
                    } else if region.is_none() {
                        self.last_code_region = None;
                        if !self.code_marks.is_empty() {
                            self.code_marks.clear();
                            changed = true;
                        }
                    }
                }
                // (Re-)OCR the region when it settles + changes.
                if want_ocr {
                    if let (Some((x, y, w, h)), Some(img)) = (region, crop) {
                        self.last_ocr_region = region;
                        self.ocr_busy.store(true, Relaxed);
                        let out = self.text_scan.clone();
                        let conf = self.text_confidence;
                        std::thread::spawn(move || {
                            let marks = crate::detect::scan_text(&img, x, y, w, h, conf);
                            if let Ok(mut g) = out.lock() {
                                *g = Some(marks);
                            }
                        });
                    } else if region.is_none() {
                        self.last_ocr_region = None;
                        if !self.text_words.is_empty() {
                            self.text_words.clear();
                            self.hovered_word = None;
                            self.text_sel.clear();
                            self.text_menu = None;
                            changed = true;
                        }
                    }
                }
                if changed {
                    self.rebuild_marks();
                }
                Task::none()
            }
            DetectMsg::HoverMark(idx) => {
                self.hovered_mark = idx;
                Task::none()
            }
            DetectMsg::ActivateMark(idx) => {
                if let Some(hit) = self.marks.get(idx) {
                    match &hit.action {
                        crate::detect::MarkAction::Open(url) => {
                            crate::platform::services::open_uri(url);
                        }
                        crate::detect::MarkAction::Copy(text) => {
                            crate::platform::services::copy_text(text);
                        }
                        crate::detect::MarkAction::Wifi {
                            ssid,
                            password,
                            encryption,
                        } => {
                            crate::platform::services::join_wifi(ssid, password, encryption);
                        }
                        crate::detect::MarkAction::SaveOpen { ext, content } => {
                            crate::platform::services::save_and_open(ext, content);
                        }
                    }
                }
                // An action was taken — close the program.
                self.finish_session()
            }
            DetectMsg::HoverWord(idx) => {
                self.hovered_word = idx;
                Task::none()
            }
            DetectMsg::TextSelectBegin(anchor, additive) => {
                // Snapshot the base (empty unless additive) so the drag recomputes from
                // it; seed the selection with the anchor word.
                let base = if additive {
                    self.text_sel.clone()
                } else {
                    std::collections::BTreeSet::new()
                };
                self.text_sel = base.clone();
                self.text_sel.insert(anchor);
                self.text_drag = Some((anchor, additive, base));
                self.text_menu = None;
                Task::none()
            }
            DetectMsg::TextSelectTo(end) => {
                if let Some((anchor, _additive, base)) = &self.text_drag {
                    let (lo, hi) = ((*anchor).min(end), (*anchor).max(end));
                    let mut sel = base.clone();
                    sel.extend(lo..=hi);
                    self.text_sel = sel;
                }
                Task::none()
            }
            DetectMsg::TextDeselect => {
                self.text_sel.clear();
                self.text_menu = None;
                Task::none()
            }
            DetectMsg::TextToggle(idx) => {
                // Ctrl-click: add/remove a single word.
                if !self.text_sel.remove(&idx) {
                    self.text_sel.insert(idx);
                }
                self.text_menu = None;
                Task::none()
            }
            DetectMsg::TextExpand(idx, count) => {
                // Double-click selects the word's line; triple-click selects all.
                self.text_menu = None;
                if count >= 3 {
                    self.text_sel = (0..self.text_words.len()).collect();
                } else if let Some(line) = self.text_words.get(idx).map(|w| w.line) {
                    self.text_sel = self
                        .text_words
                        .iter()
                        .enumerate()
                        .filter(|(_, w)| w.line == line)
                        .map(|(i, _)| i)
                        .collect();
                }
                Task::none()
            }
            DetectMsg::TextSelectAll => {
                self.text_sel = (0..self.text_words.len()).collect();
                self.text_menu = None;
                Task::none()
            }
            DetectMsg::TextCopy => {
                // Copy the active selection to the clipboard and stay open (the capture
                // isn't ended by selecting/copying text). Words are joined in reading
                // order with the usual spacing/line breaks.
                self.text_menu = None;
                let picked: Vec<crate::detect::TextWord> = self
                    .text_sel
                    .iter()
                    .filter_map(|&i| self.text_words.get(i).cloned())
                    .collect();
                let text = crate::detect::join_words(&picked);
                if text.is_empty() {
                    return Task::none();
                }
                // `copy_text_task`: the scanner overlay is focused when this fires, and on a
                // compositor with no data-control that window is the only thing that can hold
                // the selection. `copy_text` would have reported nothing and copied nothing.
                crate::share::copy_text_task(&text)
            }
            DetectMsg::WordMenu(idx, x, y) => {
                // Right-clicking a word outside the current selection selects just it;
                // inside it keeps the existing selection. Then open the menu.
                if !self.text_sel.contains(&idx) {
                    self.text_sel.clear();
                    self.text_sel.insert(idx);
                }
                self.text_menu = Some((x, y));
                Task::none()
            }
            DetectMsg::DismissTextMenu => {
                self.text_menu = None;
                Task::none()
            }
            DetectMsg::CodeMenu(idx, x, y) => {
                self.text_menu = None;
                self.code_menu = Some((idx, x, y));
                Task::none()
            }
            DetectMsg::CopyCodeContents(idx) => {
                // Copy the code's full decoded value and stay open (unlike a left-click,
                // which runs the code's action and exits).
                self.code_menu = None;
                let Some(mark) = self.marks.get(idx).filter(|m| !m.value.is_empty()) else {
                    return Task::none();
                };
                // See the OCR copy above for why this is the task form.
                crate::share::copy_text_task(&mark.value)
            }
            DetectMsg::DismissCodeMenu => {
                self.code_menu = None;
                Task::none()
            }
        }
    }
}
