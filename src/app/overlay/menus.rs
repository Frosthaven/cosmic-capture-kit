use super::super::*;

impl App {
    /// The right-click "Copy" menu for a text selection, shown at the cursor on the
    /// output it was opened on. A full-surface backdrop dismisses it on an outside
    /// click; the menu item copies the selection (and stays in the capture).
    pub(super) fn text_menu_layer(&self, o: &OutputState) -> Option<Element<'_, Msg>> {
        let (gx, gy) = self.text_menu?;
        let (ox, oy) = o.logical_pos;
        let (ow, oh) = o.logical_size;
        if gx < ox || gx >= ox + ow as i32 || gy < oy || gy >= oy + oh as i32 {
            return None;
        }
        let (lx, ly) = ((gx - ox) as f32, (gy - oy) as f32);

        let backdrop: Element<'_, Msg> = widget::mouse_area(
            widget::Space::new().width(Length::Fill).height(Length::Fill),
        )
        .on_press(Msg::Detect(DetectMsg::DismissTextMenu))
        .on_right_press(Msg::Detect(DetectMsg::DismissTextMenu))
        .into();

        let menu_item = |label: &str, msg: Msg| {
            widget::button::custom(widget::text(label.to_string()).size(14))
                .padding(cosmic::iced::Padding::from([6.0, 14.0]))
                .width(Length::Fill)
                .on_press(msg)
                .class(cosmic::theme::Button::Text)
        };
        let items = widget::column(vec![
            menu_item("Copy", Msg::Detect(DetectMsg::TextCopy)).into(),
            menu_item("Select all", Msg::Detect(DetectMsg::TextSelectAll)).into(),
            menu_item("Select none", Msg::Detect(DetectMsg::TextDeselect)).into(),
        ]);
        let menu = widget::container(items)
            .width(Length::Fixed(130.0))
            .padding(4)
            .class(cosmic::theme::Container::Custom(Box::new(|t| {
                let c = t.cosmic();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(c.background.component.base.into())),
                    text_color: Some(c.background.component.on.into()),
                    border: Border {
                        radius: crate::app::theme::rounding(t).s.into(),
                        width: 1.0,
                        color: c.background.component.divider.into(),
                    },
                    ..Default::default()
                }
            })));

        Some(
            cosmic::iced::widget::stack(vec![backdrop, positioned_mark(lx, ly, menu.into())])
                .into(),
        )
    }

    /// The right-click "Copy contents" menu for a detected code (QR/barcode), shown at
    /// the cursor on the output it was opened on. Copies the code's full decoded value
    /// (unlike a left-click, which runs the code's action — open URL, join wifi, …).
    pub(super) fn code_menu_layer(&self, o: &OutputState) -> Option<Element<'_, Msg>> {
        let (idx, gx, gy) = self.code_menu?;
        let mark = self.marks.get(idx)?;
        let (ox, oy) = o.logical_pos;
        let (ow, oh) = o.logical_size;
        if gx < ox || gx >= ox + ow as i32 || gy < oy || gy >= oy + oh as i32 {
            return None;
        }
        let (lx, ly) = ((gx - ox) as f32, (gy - oy) as f32);

        let backdrop: Element<'_, Msg> = widget::mouse_area(
            widget::Space::new().width(Length::Fill).height(Length::Fill),
        )
        .on_press(Msg::Detect(DetectMsg::DismissCodeMenu))
        .on_right_press(Msg::Detect(DetectMsg::DismissCodeMenu))
        .into();

        let label = if mark.is_qr {
            "Copy QR Contents"
        } else {
            "Copy contents"
        };
        let item = widget::button::custom(widget::text(label).size(14))
            .padding(cosmic::iced::Padding::from([6.0, 14.0]))
            .width(Length::Fill)
            .on_press(Msg::Detect(DetectMsg::CopyCodeContents(idx)))
            .class(cosmic::theme::Button::Text);
        let menu = widget::container(item)
            .width(Length::Fixed(170.0))
            .padding(4)
            .class(cosmic::theme::Container::Custom(Box::new(|t| {
                let c = t.cosmic();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(c.background.component.base.into())),
                    text_color: Some(c.background.component.on.into()),
                    border: Border {
                        radius: crate::app::theme::rounding(t).s.into(),
                        width: 1.0,
                        color: c.background.component.divider.into(),
                    },
                    ..Default::default()
                }
            })));

        Some(
            cosmic::iced::widget::stack(vec![backdrop, positioned_mark(lx, ly, menu.into())])
                .into(),
        )
    }
}
