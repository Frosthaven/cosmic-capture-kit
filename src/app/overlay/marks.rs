use super::super::*;

impl App {
    /// Rebuild the displayed code `marks` from the live `scan_codes` toggle, so
    /// flipping it on/off takes effect on the next render. (OCR words are a separate
    /// layer; see `shown_words`.)
    pub(in crate::app) fn rebuild_marks(&mut self) {
        self.marks.clear();
        if self.scan_codes {
            self.marks.extend(self.code_marks.iter().cloned());
        }
        if self.hovered_mark.is_some_and(|i| i >= self.marks.len()) {
            self.hovered_mark = None;
        }
    }

    /// Keep only rects that fall inside the region and are centred on output `o`,
    /// tagging each with its source index. Shared by the code-mark + OCR-word layers
    /// and their hit-testing (so they never block a region drag).
    ///
    /// All CAPTURE space (DRAGON-448): mark rects come off the scanned pixels, the region
    /// and the output rect are capture space too, and the answer is a filtered list that
    /// stays capture space (the widget hit-tests it there). Nothing here meets iced.
    fn shown_in_region<'a>(
        &self,
        o: &OutputState,
        rects: impl Iterator<Item = (usize, (i32, i32, i32, i32))> + 'a,
    ) -> Vec<(usize, (i32, i32, i32, i32))> {
        let Some((rx, ry, rw, rh)) = self.normalized_region() else {
            return Vec::new();
        };
        let (rl, rt, rr, rb) = (rx, ry, rx + rw as i32, ry + rh as i32);
        let (ox, oy) = o.logical_pos;
        let (ow, oh) = o.logical_size;
        rects
            .filter(|&(_, (gx, gy, gw, gh))| {
                if gx + gw <= rl || gx >= rr || gy + gh <= rt || gy >= rb {
                    return false;
                }
                let (cx, cy) = (gx + gw / 2, gy + gh / 2);
                cx >= ox && cx < ox + ow as i32 && cy >= oy && cy < oy + oh as i32
            })
            .collect()
    }

    /// Code marks (QR/barcode) shown on output `o`: `(mark index, global rect)`.
    pub(super) fn shown_marks(&self, o: &OutputState) -> Vec<(usize, (i32, i32, i32, i32))> {
        if self.kind != Kind::Scanner || !self.scan_codes || self.mode != Mode::Region {
            return Vec::new();
        }
        self.shown_in_region(o, self.marks.iter().enumerate().map(|(i, m)| (i, m.rect)))
    }

    /// OCR words shown on output `o`: `(word index, global 4-corner poly)` — the
    /// selectable text layer's hit shapes (skewed when the text was deskewed). Region
    /// filtering uses each word's axis-aligned bbox.
    pub(super) fn shown_words(&self, o: &OutputState) -> Vec<(usize, [(i32, i32); 4])> {
        if self.kind != Kind::Scanner || !self.scan_text || self.mode != Mode::Region {
            return Vec::new();
        }
        self.shown_in_region(o, self.text_words.iter().enumerate().map(|(i, w)| (i, w.rect)))
            .into_iter()
            .map(|(i, _rect)| (i, self.text_words[i].poly))
            .collect()
    }

    /// Overlay drawn over the region on a canvas. QR/barcode marks get an orientation-
    /// following orange outline (skewed codes → skewed quad) plus a hover wash + a
    /// tooltip. OCR words get only a translucent cyan wash — no outline — on the
    /// hovered word and the active selection span, with slightly padded, rounded boxes.
    /// Drawing only — hover / click / drag-select are owned by the region widget so
    /// they never intercept a region drag.
    pub(super) fn marks_layer(&self, o: &OutputState) -> Option<Element<'_, Msg>> {
        let codes = self.shown_marks(o);
        let words = self.shown_words(o);
        if codes.is_empty() && words.is_empty() {
            return None;
        }
        // Mark/word polys are CAPTURE space (they come off the scanned pixels); the canvas
        // and the tooltip placement below are POINTS. Cross once, here (DRAGON-448).
        let units = o.units();
        let mut shapes: Vec<MarkShape> = Vec::new();
        let mut tooltip: Option<Element<'_, Msg>> = None;

        // OCR words: every word always carries a faint cyan wash (so the selectable
        // text is visible); the hovered word is a touch stronger and selected words
        // are more opaque still.
        for (idx, poly) in words {
            let alpha = if self.text_sel.contains(&idx) {
                0.40
            } else if self.hovered_word == Some(idx) {
                0.24
            } else {
                0.15
            };
            // Output-local corners (follow the text slant), padded outward from centroid.
            let pts = poly.map(|g| {
                let (lx, ly) = units.to_point(g);
                cosmic::iced::Point::new(lx, ly)
            });
            let cx = pts.iter().map(|p| p.x).sum::<f32>() / 4.0;
            let cy = pts.iter().map(|p| p.y).sum::<f32>() / 4.0;
            const PAD: f32 = 4.0;
            let quad = pts.map(|p| {
                let (dx, dy) = (p.x - cx, p.y - cy);
                let len = dx.hypot(dy).max(1.0);
                cosmic::iced::Point::new(p.x + dx / len * PAD, p.y + dy / len * PAD)
            });
            shapes.push(MarkShape {
                quad,
                fill: Some(alpha),
                stroke: false,
            });
        }

        // Code marks: outline + hover wash + tooltip.
        for (idx, (gx, gy, _gw, _gh)) in codes {
            let hit = &self.marks[idx];
            let hovered = self.hovered_mark == Some(idx);
            let quad = hit.poly.map(|g| {
                let (lx, ly) = units.to_point(g);
                cosmic::iced::Point::new(lx, ly)
            });
            shapes.push(MarkShape {
                quad,
                fill: hovered.then_some(0.22),
                stroke: true,
            });

            if hovered {
                let (lx, ly) = units.to_point((gx, gy));
                let (lx, ly) = (lx.max(0.0), ly.max(0.0));
                tooltip = Some(positioned_mark(lx, (ly - 30.0).max(0.0), tooltip_box(&hit.label)));
            }
        }

        let canvas = cosmic::widget::Canvas::new(MarksCanvas { shapes })
            .width(Length::Fill)
            .height(Length::Fill);
        let mut layers: Vec<Element<'_, Msg>> = vec![canvas.into()];
        if let Some(tt) = tooltip {
            layers.push(tt);
        }
        Some(cosmic::iced::widget::stack(layers).into())
    }

    /// Whether the scanner is busy — a QR/OCR pass running, OR the live region shot those
    /// passes read being taken (DRAGON-460).
    ///
    /// The shot counts because from the user's side it is all one wait: they press refresh,
    /// the screen is read, the passes run, marks appear. Reporting busy only for the passes
    /// would leave the toolbar button idle-looking for the read, then spin — a flicker on
    /// every scan rather than one continuous "working".
    pub(in crate::app) fn scanning(&self) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        self.code_busy.load(Relaxed)
            || self.ocr_busy.load(Relaxed)
            || self.scan_shot != crate::app::ScanShot::Idle
    }

    // DRAGON-460 removed `scan_spinner_layer` — the small accent spinner inset in the
    // bottom-right of the selection while a pass ran.
    //
    // The scanner's progress now shows on the toolbar's refresh button, which spins and
    // disables itself while [`Self::scanning`]. That is where the user's attention already
    // is (they pressed it), it cannot be clipped off-screen by a region drawn at the edge
    // of a display the way an inset badge could, and it says WHICH control is unavailable
    // rather than only that something is happening.
    //
    // It also stops the spinner sitting INSIDE the crop the scanner is about to read.
}

/// The hover tooltip bubble (theme-styled, clamped to 600 chars). Shared by code
/// marks (their decoded value) and the OCR selection preview.
fn tooltip_box(text: &str) -> Element<'static, Msg> {
    let mut label: String = text.chars().take(600).collect();
    if text.chars().count() > 600 {
        label.push('…');
    }
    widget::container(widget::text(label).size(13))
        .padding(cosmic::iced::Padding::from([4.0, 8.0]))
        .class(cosmic::theme::Container::Custom(Box::new(|t| {
            let c = t.cosmic();
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(c.background.component.base.into())),
                text_color: Some(c.background.component.on.into()),
                border: Border {
                    radius: crate::app::theme::rounding(t).s.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })))
        .into()
}

/// One shape for the canvas: its output-local quad, an optional fill alpha (a
/// translucent wash), and whether to stroke (outline) it. Every shape — QR/barcode
/// outlines and OCR text washes alike — is drawn in the theme accent colour.
struct MarkShape {
    quad: [cosmic::iced::Point; 4],
    fill: Option<f32>,
    stroke: bool,
}

/// Canvas program that strokes each detected mark's orientation-following outline,
/// washing the hovered one with a translucent fill. Purely visual: it never reports a
/// cursor interaction or captures events, so the region widget underneath keeps
/// owning hover, clicks, and the crosshair cursor.
struct MarksCanvas {
    shapes: Vec<MarkShape>,
}

impl<M> cosmic::widget::canvas::Program<M, cosmic::Theme, cosmic::Renderer> for MarksCanvas {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &cosmic::Renderer,
        theme: &cosmic::Theme,
        bounds: cosmic::iced::Rectangle,
        _cursor: cosmic::iced::mouse::Cursor,
    ) -> Vec<cosmic::widget::canvas::Geometry<cosmic::Renderer>> {
        use cosmic::widget::canvas::{Frame, Stroke};
        let accent = crate::app::theme::accent(theme);
        // The marks follow the user's COSMIC rounding rule; capped at the
        // historical 6.0 so the default look is unchanged.
        let quad_r = crate::app::theme::rounding(theme).s1().min(6.0);
        let mut frame = Frame::new(renderer, bounds.size());
        for s in &self.shapes {
            let path = rounded_quad_path(&s.quad, quad_r);
            if let Some(a) = s.fill {
                let mut fill = accent;
                fill.a = a;
                frame.fill(&path, fill);
            }
            if s.stroke {
                frame.stroke(
                    &path,
                    Stroke::default().with_color(accent).with_width(2.0),
                );
            }
        }
        vec![frame.into_geometry()]
    }
}

/// A closed path around the four (possibly skewed) corners with rounded joins: at each
/// vertex we stop `radius` short along both edges and curve through the corner, so the
/// outline keeps soft corners even when it's rotated/sheared. `radius` is clamped to
/// half the shortest edge so thin marks don't self-overlap.
fn rounded_quad_path(quad: &[cosmic::iced::Point; 4], radius: f32) -> cosmic::widget::canvas::Path {
    use cosmic::iced::Point;
    let mut r = radius;
    for i in 0..4 {
        let (a, b) = (quad[i], quad[(i + 1) % 4]);
        r = r.min((b.x - a.x).hypot(b.y - a.y) / 2.0);
    }
    let unit = |from: Point, to: Point| {
        let (dx, dy) = (to.x - from.x, to.y - from.y);
        let l = dx.hypot(dy).max(1e-3);
        (dx / l, dy / l)
    };
    cosmic::widget::canvas::Path::new(|p| {
        for i in 0..4 {
            let (vp, vi, vn) = (quad[(i + 3) % 4], quad[i], quad[(i + 1) % 4]);
            let din = unit(vp, vi); // along the incoming edge
            let dout = unit(vi, vn); // along the outgoing edge
            let a = Point::new(vi.x - din.0 * r, vi.y - din.1 * r);
            let b = Point::new(vi.x + dout.0 * r, vi.y + dout.1 * r);
            if i == 0 {
                p.move_to(a);
            } else {
                p.line_to(a);
            }
            p.quadratic_curve_to(vi, b);
        }
        p.close();
    })
}
