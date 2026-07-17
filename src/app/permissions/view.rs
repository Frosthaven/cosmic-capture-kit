//! The permission-checker window's view + its live-status probe task.
//!
//! Layout: a CSD header bar (draggable, ✕ to close) over a scrollable column of
//! permission cards. Each card is a rounded panel — an icon + name + one-line "why"
//! on the left, a coloured status pill on the right, and (when action is needed) a
//! button row beneath. The visual language is the app's existing settings palette
//! (`theme::success`/`warning`/`danger`, `theme::rounding`) so it reads native next
//! to the Settings window.
//!
//! Everything AppKit-touching is behind `probe_now` (called off the view); `view`
//! itself only reads the cached [`Probe`] snapshot, so it never blocks the UI.

use super::*;

/// A pill's colour tone. `cosmic::theme::Text::Custom` accepts only a NON-capturing
/// `fn` pointer, so each tone carries its caption/container through its own static
/// closure rather than capturing a colour fn — the same trick `settings::row`'s
/// `severity_caption` / `severity_title` use.
#[derive(Clone, Copy)]
enum Tone {
    Ok,
    Warn,
    Danger,
}

impl Tone {
    /// A caption in this tone's colour (green / amber / red).
    fn caption(self, s: &str) -> Element<'_, Msg> {
        let text = widget::text::caption(s.to_string());
        match self {
            Tone::Ok => text.class(cosmic::theme::Text::Custom(|t| {
                cosmic::iced::widget::text::Style { color: Some(theme::success(t)), ..Default::default() }
            })),
            Tone::Warn => text.class(cosmic::theme::Text::Custom(|t| {
                cosmic::iced::widget::text::Style { color: Some(theme::warning(t)), ..Default::default() }
            })),
            Tone::Danger => text.class(cosmic::theme::Text::Custom(|t| {
                cosmic::iced::widget::text::Style { color: Some(theme::danger(t)), ..Default::default() }
            })),
        }
        .into()
    }

    /// The pill's rounded, faintly-tinted capsule background in this tone.
    fn pill_container(self) -> cosmic::theme::Container<'static> {
        fn style(color: cosmic::iced::Color, t: &cosmic::Theme) -> cosmic::iced::widget::container::Style {
            let mut bg = color;
            bg.a = 0.15;
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(bg)),
                border: Border { radius: theme::rounding(t).xl.into(), ..Default::default() },
                ..Default::default()
            }
        }
        match self {
            Tone::Ok => cosmic::theme::Container::custom(|t| style(theme::success(t), t)),
            Tone::Warn => cosmic::theme::Container::custom(|t| style(theme::warning(t), t)),
            Tone::Danger => cosmic::theme::Container::custom(|t| style(theme::danger(t), t)),
        }
    }
}

impl App {
    /// A `Task` that re-probes every permission (off the view/update thread, since
    /// `notification_status` may briefly block on an async settings query) and folds
    /// the fresh snapshot back in via `PermissionsMsg::Refresh`. macOS-only; on Linux
    /// there is nothing to probe (this is never called — the window is never opened).
    #[cfg(target_os = "macos")]
    pub(in crate::app) fn probe_permissions_task(&self) -> Task<cosmic::Action<Msg>> {
        Task::perform(async { probe_now() }, |probe| {
            cosmic::Action::App(Msg::Permissions(PermissionsMsg::Refresh(probe)))
        })
    }

    /// The permission-checker window's content.
    pub(in crate::app) fn permissions_window_view(&self) -> Element<'_, Msg> {
        let focused = self.core.focused_window() == self.permissions.window;
        let header = widget::header_bar()
            .title(WINDOW_TITLE)
            .focused(focused)
            .on_drag(Msg::WindowChrome(WindowChromeMsg::PermissionsWindowDrag));
        // macOS (DRAGON-135): the native traffic lights carry close (the window opens
        // with a transparent titlebar over our header), so no CSD close is drawn.
        // The Linux arm keeps it, though the window is only ever minted on macOS.
        #[cfg(not(target_os = "macos"))]
        let header =
            header.on_close(Msg::WindowChrome(WindowChromeMsg::ClosePermissionsWindow));

        // Intro line under the header, then one card per permission.
        let intro = widget::column(vec![
            widget::text::title3("Permissions").into(),
            widget::text::body(
                "Cosmic Capture Kit needs macOS to allow it to capture your screen. \
                 Grant the permissions below; statuses update live as you do.",
            )
            .into(),
        ])
        .spacing(6.0);

        let mut cards: Vec<Element<'_, Msg>> = vec![intro.into()];
        cards.extend(self.permission_cards());

        let inner = widget::column(cards).spacing(16.0).width(Length::Fill);

        let content = widget::scrollable(
            widget::container(inner)
                .max_width(720.0)
                .padding(cosmic::iced::Padding {
                    top: 8.0,
                    right: 24.0,
                    bottom: 24.0,
                    left: 24.0,
                }),
        )
        .height(Length::Fill)
        .width(Length::Fill);

        let stacked = widget::column(vec![header.into(), content.into()])
            .width(Length::Fill)
            .height(Length::Fill);

        // Opaque rounded window background + hairline border (matches the settings
        // window's outer container), so the transparent surface only shows through
        // outside the rounded corners.
        widget::container(stacked)
            .padding(1)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(cosmic::theme::Container::custom(|theme| {
                let cosmic = theme.cosmic();
                let radius = theme::rounding(theme).window();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(cosmic.background.base.into())),
                    border: Border {
                        color: cosmic.bg_divider().into(),
                        width: 1.0,
                        radius: radius.into(),
                    },
                    ..Default::default()
                }
            }))
            .into()
    }

    /// One card per permission the app needs, in check order (Screen Recording first
    /// — capture is blank without it). Notifications is included only when bundled +
    /// its status probed (`Probe::notifications` is `Some`), matching the Health page.
    fn permission_cards(&self) -> Vec<Element<'_, Msg>> {
        let p = &self.permissions.probe;
        let mut out: Vec<Element<'_, Msg>> = Vec::new();

        // Screen Recording (required).
        let screen = screen_status(p);
        out.push(self.permission_card(
            Permission::ScreenRecording,
            "Screen Recording",
            "camera-photo-symbolic",
            "Required for capturing screenshots and recordings. macOS applies this grant \
             on the NEXT launch, so relaunch after granting it.",
            screen,
            true,
            p.screen_request_spent,
        ));

        // Microphone (optional).
        if let Some(mic) = p.microphone {
            out.push(self.permission_card(
                Permission::Microphone,
                "Microphone",
                "audio-input-microphone-symbolic",
                "Optional. Records your voice with videos; video-only recording still \
                 works without it.",
                mic,
                false,
                false,
            ));
        }

        // Notifications (optional, bundle-gated — Some only when bundled).
        if let Some(notif) = p.notifications {
            out.push(self.permission_card(
                Permission::Notifications,
                "Notifications",
                // `preferences-system-notifications-symbolic` is NOT in libcosmic's
                // 619-name embedded cosmic-icons subset, so `from_name` renders it blank
                // on macOS (no system icon theme). `notification-symbolic` (a banner
                // glyph, matching this card's "banner when a capture is saved" wording)
                // IS embedded, so it resolves on both platforms.
                "notification-symbolic",
                "Optional. Shows a banner when a capture is saved, whose click reveals the \
                 file in Finder.",
                notif,
                false,
                false,
            ));
        }

        out
    }

    /// Build one permission card. `required` colours a missing state red (vs amber for
    /// optional). `screen_relaunch` (Screen Recording only) adds the Relaunch button
    /// once granted, since the grant only takes on a fresh launch. `request_spent`
    /// decides Request-vs-OpenSettings for a NotDetermined screen grant.
    #[allow(clippy::too_many_arguments)]
    fn permission_card<'a>(
        &self,
        perm: Permission,
        name: &'a str,
        icon: &'a str,
        why: &'a str,
        status: PermStatus,
        required: bool,
        request_spent: bool,
    ) -> Element<'a, Msg> {
        // The pill's tone: green granted, red denied (required) / amber otherwise.
        // Colour comes from a Tone enum so both the (fn-pointer-only) text class and
        // the (closure) container background pick it without capturing a fn pointer —
        // `cosmic::theme::Text::Custom` takes a NON-capturing fn, so each tone uses its
        // own static closure (the `row::severity_caption` pattern).
        let (tone, pill_text) = match status {
            PermStatus::Granted => (Tone::Ok, "Granted"),
            PermStatus::NotDetermined => (Tone::Warn, "Not requested"),
            PermStatus::Denied if required => (Tone::Danger, "Denied"),
            PermStatus::Denied => (Tone::Warn, "Denied"),
        };

        // Status pill: a rounded, tinted capsule with the status word.
        let pill = widget::container(tone.caption(pill_text))
            .padding(cosmic::iced::Padding { top: 3.0, right: 10.0, bottom: 3.0, left: 10.0 })
            .class(tone.pill_container());

        // Header row: icon + name (left), pill (right).
        let head = widget::row(vec![
            widget::icon::from_name(icon).icon().size(18).into(),
            widget::text::body(name).font(cosmic::font::bold()).width(Length::Fill).into(),
            pill.into(),
        ])
        .spacing(10.0)
        .align_y(Alignment::Center);

        let mut col = widget::column(vec![
            head.into(),
            widget::text::caption(why).into(),
        ])
        .spacing(6.0)
        .width(Length::Fill);

        // Action buttons, from the pure `card_action` chooser plus the screen Relaunch.
        let mut buttons: Vec<Element<'a, Msg>> = Vec::new();
        match card_action(status, request_spent) {
            CardAction::Request => buttons.push(
                widget::button::suggested("Request")
                    .on_press(Msg::Permissions(PermissionsMsg::Request(perm)))
                    .into(),
            ),
            CardAction::OpenSettings => buttons.push(
                widget::button::standard("Open System Settings")
                    .on_press(Msg::Permissions(PermissionsMsg::OpenSettings(perm)))
                    .into(),
            ),
            CardAction::None => {}
        }
        // Screen Recording only applies its grant to a fresh launch — offer Relaunch
        // whenever it is granted (this process may be a pre-grant one that still can't
        // capture until it restarts).
        if perm == Permission::ScreenRecording && status == PermStatus::Granted {
            buttons.push(
                widget::button::standard("Relaunch")
                    .leading_icon(widget::icon::from_name("view-refresh-symbolic"))
                    .spacing(6)
                    .on_press(Msg::Permissions(PermissionsMsg::Relaunch))
                    .into(),
            );
        }
        if !buttons.is_empty() {
            col = col.push(widget::row(buttons).spacing(8.0));
        }

        // The card panel: a rounded, subtly-filled container.
        widget::container(col)
            .padding(16.0)
            .width(Length::Fill)
            .class(cosmic::theme::Container::custom(|theme| {
                let cosmic = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(cosmic.primary.base.into())),
                    border: Border {
                        color: cosmic.bg_divider().into(),
                        width: 1.0,
                        radius: theme::rounding(theme).s.into(),
                    },
                    ..Default::default()
                }
            }))
            .into()
    }
}
