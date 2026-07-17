//! Monitor hover-picker widget.
//!
//! A leaf libcosmic `Widget` that fills its output surface and highlights the
//! whole output when hovered, letting the user pick a monitor to capture.

use cosmic::iced::core::renderer::Quad;
use cosmic::iced::core::widget::Tree;
use cosmic::iced::core::{
    Border, Clipboard, Color, Event, Layout, Length, Rectangle, Shadow, Shell, Size, mouse,
};
use cosmic::widget::Widget;

pub struct OutputSelection<Msg> {
    /// Whether THIS output is the one currently hovered — read from shared app state
    /// (`App::hovered_output`), NOT per-widget. Each overlay is a separate window on
    /// macOS and the previous one doesn't reliably get a cursor-left event, so hover
    /// can't be tracked locally; the app records which output the cursor is in and every
    /// overlay draws from that single source, so exactly one highlights.
    hovered: bool,
    /// Published (with this output's name) whenever the cursor is over this overlay, so
    /// the app can set it as the hovered output — whichever overlay the cursor is in
    /// wins, with no dependence on a cursor-left from the one it left.
    on_hover: Msg,
    on_press: Msg,
}

impl<Msg> OutputSelection<Msg> {
    pub fn new(hovered: bool, on_hover: Msg, on_press: Msg) -> Self {
        Self { hovered, on_hover, on_press }
    }
}

impl<Msg: Clone + 'static> Widget<Msg, cosmic::Theme, cosmic::Renderer> for OutputSelection<Msg> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &cosmic::Renderer,
        limits: &cosmic::iced::core::layout::Limits,
    ) -> cosmic::iced::core::layout::Node {
        cosmic::iced::core::layout::Node::new(
            limits
                .width(Length::Fill)
                .height(Length::Fill)
                .resolve(Length::Fill, Length::Fill, Size::ZERO),
        )
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }

    fn update(
        &mut self,
        _tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &cosmic::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Msg>,
        _viewport: &Rectangle,
    ) {
        if !cursor.is_over(layout.bounds()) {
            return;
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                shell.publish(self.on_press.clone());
                shell.capture_event();
            }
            // Claim the highlight for this output when the cursor is over it but the app
            // doesn't yet mark it hovered. Gating on the shared `self.hovered` (app
            // state) means we publish exactly on real transitions — no per-move spam
            // (same redraw cadence as before) — AND it's reliable across windows:
            // entering another monitor reassigns the highlight, and returning re-claims
            // it, with no dependence on a cursor-left from the overlay we left.
            Event::Mouse(mouse::Event::CursorMoved { .. }) if !self.hovered => {
                shell.publish(self.on_hover.clone());
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        _style: &cosmic::iced::core::renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        use cosmic::iced::core::Renderer as _;
        if !self.hovered {
            return;
        }
        let radius = crate::app::theme::rounding(theme).s;
        let mut accent = crate::app::theme::accent(theme);
        let bounds = layout.bounds();
        accent.a = 0.7;
        renderer.fill_quad(
            Quad {
                bounds,
                border: Border {
                    radius: radius.into(),
                    width: 12.0,
                    color: accent,
                },
                shadow: Shadow::default(),
                snap: true,
            },
            Color::TRANSPARENT,
        );
        accent.a = 1.0;
        renderer.fill_quad(
            Quad {
                bounds,
                border: Border {
                    radius: radius.into(),
                    width: 4.0,
                    color: accent,
                },
                shadow: Shadow::default(),
                snap: true,
            },
            Color::TRANSPARENT,
        );
    }
}

impl<'a, Msg: Clone + 'static> From<OutputSelection<Msg>> for cosmic::Element<'a, Msg> {
    fn from(w: OutputSelection<Msg>) -> cosmic::Element<'a, Msg> {
        cosmic::Element::new(w)
    }
}
