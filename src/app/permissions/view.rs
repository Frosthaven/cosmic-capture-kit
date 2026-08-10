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
                 Screen Recording is required; the rest can be skipped. Statuses update \
                 live as you grant them.",
            )
            .into(),
        ])
        .spacing(6.0);

        let mut cards: Vec<Element<'_, Msg>> = vec![intro.into()];
        cards.extend(self.permission_cards());
        cards.extend(self.skip_footer());

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

        // Frosted glass (DRAGON-217/533): window background paints translucent
        // (`theme::frost_color`) so the compositor blur / masked NSVisualEffectView
        // vibrancy enrolled on this surface (`open_permissions_window`'s `blur`,
        // `enable_window_vibrancy`'s reparenting) actually shows through. This
        // container's own comment used to claim it "matches the settings window's
        // outer container", but painted a hardcoded opaque `cosmic.background(false).base`
        // instead of settings' `frost_color(.., glass)`, so the window-level
        // vibrancy was set up correctly and then completely hidden behind this
        // view's own opaque paint. `glass` is captured by value (`Option<GlassConfig>`
        // is `Copy`) so the closure needs no lifetime tied to `self`.
        let glass = self.glass;
        widget::container(stacked)
            .padding(1)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(cosmic::theme::Container::custom(move |theme| {
                let cosmic = theme.cosmic();
                // macOS (matches settings::open_config_window's outer container): this
                // toplevel is NATIVE-decorated, so the window server already draws +
                // rounds the frame and clips content to it. Rounding this container TOO
                // paints a second, slightly-mismatched corner inside the OS frame's, a
                // "double corner" fringe. Fill SQUARE and let the window server do the
                // one rounding; Linux keeps the app-drawn radius since its window edge
                // IS the app's.
                #[cfg(target_os = "linux")]
                let radius = theme::rounding(theme).window();
                #[cfg(target_os = "macos")]
                let radius = [0.0f32; 4];
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(theme::frost_color(
                        cosmic.background(false).base.into(),
                        glass,
                    ))),
                    border: Border {
                        color: cosmic.bg_divider().into(),
                        // macOS: NO app border, matching the settings window's own
                        // reasoning (the OS-drawn native frame is the border; a 1px app
                        // stroke traces a bright fringe just inside the OS corner round).
                        #[cfg(target_os = "macos")]
                        width: 0.0,
                        #[cfg(not(target_os = "macos"))]
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
    ///
    /// Each card's tier word ("Required" / "Recommended" / "Optional") comes from
    /// [`Permission::tier`], NOT from the copy, so the word a user reads and the
    /// auto-open policy `should_auto_open` applies can never disagree (DRAGON-412).
    /// The `why` strings below therefore start after that word.
    fn permission_cards(&self) -> Vec<Element<'_, Msg>> {
        let p = &self.permissions.probe;
        let mut out: Vec<Element<'_, Msg>> = Vec::new();

        // Screen Recording (required).
        let screen = screen_status(p);
        out.push(self.permission_card(
            Permission::ScreenRecording,
            "Screen Recording",
            "camera-photo-symbolic",
            "Screenshots and recordings are blank without it. macOS applies this grant \
             on the NEXT launch, so relaunch after granting it.",
            screen,
            p.screen_request_spent,
        ));

        // Microphone (optional).
        if let Some(mic) = p.microphone {
            out.push(self.permission_card(
                Permission::Microphone,
                "Microphone",
                "audio-input-microphone-symbolic",
                "Records your voice with videos. Video-only recording still works \
                 without it.",
                mic,
                false,
            ));
        }

        // Notifications (optional, bundle-gated — Some only when bundled).
        if let Some(notif) = p.notifications {
            out.push(self.permission_card(
                Permission::Notifications,
                "Notifications",
                // `notification-symbolic` → the bundled Lucide `bell` (DRAGON-324): a banner
                // glyph matching this card's "banner when a capture is saved" wording.
                "notification-symbolic",
                "Shows a banner when a capture is saved, whose click reveals the file in \
                 Finder.",
                notif,
                false,
            ));
        }

        // Accessibility (RECOMMENDED, DRAGON-311 / DRAGON-412). Boolean preflight like
        // Screen Recording, so `accessibility_request_spent` decides
        // Request-vs-Open-Settings for a not-granted state. `input-keyboard-symbolic` is
        // in libcosmic's embedded subset and reads as "controls another app", fitting the
        // AX focus-resolution role. The copy names the exact feature it buys and the exact
        // cost of declining, because declining is a supported choice here.
        out.push(self.permission_card(
            Permission::Accessibility,
            "Accessibility",
            "input-keyboard-symbolic",
            "Lets Capture Active Window and Capture Active Monitor target the window you \
             are actually focused on, and capture it in its active appearance. Without it, \
             the app guesses from window stacking order, which is usually right but can \
             pick the wrong window.",
            accessibility_status(p),
            p.accessibility_request_spent,
        ));

        out
    }

    /// The escape route (DRAGON-412): a labelled way to continue without the recommended
    /// / optional grants, plus the sentence that makes the guarantee legible.
    ///
    /// Closing the window does the same thing, so this button changes no behaviour — it
    /// exists because a SILENTLY terminal dismissal cannot tell the user they won't be
    /// asked again, and "will I be nagged forever?" is exactly the question the old
    /// behaviour taught them to ask.
    ///
    /// Three states, so the footer never over-promises:
    /// * required grant missing — no button. Skipping cannot help; the window will keep
    ///   coming back for Screen Recording, and saying otherwise would be a lie.
    /// * nag already spent — no button, just the standing promise restated, so a user who
    ///   reopens the window deliberately can see that it will not reopen itself.
    /// * otherwise — the button.
    fn skip_footer(&self) -> Option<Element<'_, Msg>> {
        let p = &self.permissions.probe;
        if screen_status(p) != PermStatus::Granted {
            return None;
        }
        if p.nag_spent {
            return Some(
                widget::text::caption(
                    "You will not be asked about these again. Grant them any time from \
                     this window.",
                )
                .into(),
            );
        }
        Some(widget::column(vec![
            widget::button::standard("Continue Without These")
                .on_press(Msg::Permissions(PermissionsMsg::Skip))
                .into(),
            widget::text::caption(
                "Captures already work. Skipping closes this window and stops it opening \
                 on its own; grant the rest here whenever you want them.",
            )
            .into(),
        ])
        .spacing(6.0)
        .width(Length::Fill)
        .into())
    }

    /// Build one permission card. The card's [`Tier`] comes from `perm` and drives both
    /// the word the copy leads with and whether a missing grant reads red (Required) or
    /// amber (Recommended / Optional) — one source, so the label can't contradict the
    /// policy (DRAGON-412). `why` is the rest of the sentence after that word.
    /// `request_spent` decides Request-vs-OpenSettings for a NotDetermined grant whose
    /// preflight is boolean (Screen Recording, Accessibility).
    fn permission_card<'a>(
        &self,
        perm: Permission,
        name: &'a str,
        icon: &'a str,
        why: &'a str,
        status: PermStatus,
        request_spent: bool,
    ) -> Element<'a, Msg> {
        let tier = perm.tier();
        // The pill's tone: green granted, red denied (required) / amber otherwise.
        // Colour comes from a Tone enum so both the (fn-pointer-only) text class and
        // the (closure) container background pick it without capturing a fn pointer —
        // `cosmic::theme::Text::Custom` takes a NON-capturing fn, so each tone uses its
        // own static closure (the `row::severity_caption` pattern).
        let (tone, pill_text) = match status {
            PermStatus::Granted => (Tone::Ok, "Granted"),
            PermStatus::NotDetermined => (Tone::Warn, "Not requested"),
            PermStatus::Denied if tier.is_required() => (Tone::Danger, "Denied"),
            PermStatus::Denied => (Tone::Warn, "Denied"),
        };

        // Status pill: a rounded, tinted capsule with the status word.
        let pill = widget::container(tone.caption(pill_text))
            .padding(cosmic::iced::Padding { top: 3.0, right: 10.0, bottom: 3.0, left: 10.0 })
            .class(tone.pill_container());

        // Header row: icon + name (left), pill (right).
        let head = widget::row(vec![
            crate::widgets::icons::handle(icon).icon().size(18).into(),
            widget::text::body(name).font(cosmic::font::bold()).width(Length::Fill).into(),
            pill.into(),
        ])
        .spacing(10.0)
        .align_y(Alignment::Center);

        // "Required. …" / "Recommended. …" / "Optional. …" — the tier word is prepended
        // here rather than baked into each `why` string, so `Permission::tier` is the only
        // place the tier is decided.
        let mut col = widget::column(vec![
            head.into(),
            widget::text::caption(format!("{}. {why}", tier.label())).into(),
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
                    .leading_icon(crate::widgets::icons::handle("view-refresh-symbolic"))
                    .spacing(6)
                    .on_press(Msg::Permissions(PermissionsMsg::Relaunch))
                    .into(),
            );
        }
        // Accessibility: the resident menu-bar daemon is the process that resolves the
        // focused window, and it is separate from this window. `AXIsProcessTrusted()`
        // re-reads live, but restarting the daemon guarantees the running one picks up a
        // fresh grant now. Offer it whenever the grant is present (a no-op if no daemon
        // is running).
        if perm == Permission::Accessibility && status == PermStatus::Granted {
            buttons.push(
                widget::button::standard("Restart Background Helper")
                    .leading_icon(crate::widgets::icons::handle("view-refresh-symbolic"))
                    .spacing(6)
                    .on_press(Msg::Permissions(PermissionsMsg::RestartDaemon))
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
                    background: Some(Background::Color(cosmic.primary(false).base.into())),
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
