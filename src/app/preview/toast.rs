//! In-editor TOASTS (DRAGON-353): the transient success / error notices the preview
//! editor shows in its own top-right corner.
//!
//! They replace the desktop-notification chain the share actions used to fire. A capture
//! editor that stays open after every action needs its feedback INSIDE the surface: a
//! fullscreen overlay covers the shell's own notification area entirely, and a system
//! banner for "copied" is both easy to miss and impossible to correlate with WHICH of
//! several open documents acted.
//!
//! **Per document, always.** The state lives on [`super::PreviewState`], never on `App`:
//! with several documents open (DRAGON-336) a toast belongs to the one whose button was
//! pressed, and it must render in that document's surface whether that is the fullscreen
//! overlay or a CSD window. The rendering seam ([`toast_layer`]) is surface-agnostic for
//! exactly that reason — it is stacked over the media REGION (`chrome::toast_region`, from
//! `compose_preview` or from the loading view while the media is still arriving), so the two
//! appearances get identical placement from one builder and neither can overlap the chrome.
//!
//! Expiry is driven by a tick subscription (`sub_preview_toasts`) rather than by a timer
//! per toast: [`Toasts::expire`] is a pure sweep the tick calls, and it reports whether
//! anything actually changed so the subscription can stop once the queue drains.
//!
//! # Which end is "newest"
//!
//! [`Toasts::items`] is **oldest first, newest LAST** — that is the push order and the
//! expiry order, and [`Toasts::push_at`]'s dedupe re-appends a refreshed notice precisely
//! so it becomes the newest again. The RENDER is the reverse of it ([`toast_layer`] walks
//! `.rev()`), because on screen the newest card sits at the TOP of the stack. Both ends
//! are named here so the pairing can't silently invert: change one and the ordering test
//! (`push_order_renders_newest_first`) fails.

use super::*;
use std::time::{Duration, Instant};

/// How long a toast stays up before it expires.
pub(super) const TOAST_TTL: Duration = Duration::from_secs(4);

/// What a live toast has LEFT once the user gets hands-on with the document (DRAGON-353
/// follow-up): arming a tool, clicking or dragging in the media, working a video's
/// timeline. Someone who has started editing has read the notice or decided not to, and a
/// card parked over their picture for the rest of its four seconds is in the way.
///
/// Deliberately not an instant dismissal — a toast that vanishes on the same click that
/// caused it to vanish reads as a glitch. This is the courtesy tail: long enough to be a
/// deliberate exit, short enough to be out of the way before the next gesture lands.
///
/// Applied through [`Toasts::shorten_to`], which never LENGTHENS a toast, so this is a
/// ceiling on the remaining life and repeated interaction is idempotent.
pub(super) const TOAST_INTERACTION_TTL: Duration = Duration::from_millis(750);

/// How many toasts may be stacked at once. Beyond this the OLDEST is dropped: a burst of
/// notices should show the latest, not bury the editor.
///
/// It is also the honest answer to "what if the stack is taller than the media area?".
/// The stack is anchored INSIDE the media element (see [`toast_layer`]) and must never
/// grow over the toolbars or the video transport; three cards is ~135px, which fits any
/// media area big enough to be worth previewing, and a capture small enough to lose that
/// race has a window sized to it rather than a media area with room for chrome. Raising
/// this constant without re-checking that reasoning is how the stack would start
/// overhanging.
pub(super) const MAX_TOASTS: usize = 3;

/// A toast's severity — the ONE thing that colours it. Both colours come from the app's
/// canonical semantic palette (`app::theme`), so they track light/dark like every other
/// status colour and nothing here hardcodes a value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::app) enum ToastKind {
    /// The action did what it said (green).
    Success,
    /// The action failed, or could not be attempted (red).
    Error,
}

impl ToastKind {
    /// This severity's colour on `theme`.
    fn color(self, theme: &cosmic::Theme) -> cosmic::iced::Color {
        match self {
            Self::Success => crate::app::theme::success(theme),
            Self::Error => crate::app::theme::danger(theme),
        }
    }

    /// The leading glyph.
    fn icon(self) -> &'static str {
        match self {
            Self::Success => "emblem-ok-symbolic",
            Self::Error => "dialog-error-symbolic",
        }
    }
}

/// One transient notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct Toast {
    pub kind: ToastKind,
    pub text: String,
    /// An OPTIONAL per-toast leading glyph (DRAGON-357): `Some(app-icon-name)` overrides the
    /// severity default so a specific outcome (copied / saved / deleted / their failures) can
    /// carry its own icon. `None` falls back to [`ToastKind::icon`].
    pub icon: Option<&'static str>,
    /// When [`Toasts::expire`] should drop it.
    pub expires_at: Instant,
}

/// A document's toast queue — **oldest first, newest last**. That is push order, expiry
/// order and dedupe order; the top-right stack renders it REVERSED so the newest card is
/// on top (see the module doc).
#[derive(Default)]
pub(in crate::app) struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    /// Post a notice, expiring `ttl` after `now`.
    ///
    /// Re-posting an IDENTICAL notice (same severity, same text) does not stack a
    /// duplicate: the existing one is refreshed and moved to the newest slot. Mashing Copy
    /// should re-assure, not wallpaper the editor.
    ///
    /// The explicit `now` + `ttl` is what makes the queue's expiry, dedupe and overflow rules
    /// testable without sleeping, which is the whole reason this and `push_full` are separate
    /// (DRAGON-467: every PRODUCTION caller carries an icon now, so the tests below are its
    /// only other users — hence the allow).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn push_at(
        &mut self,
        kind: ToastKind,
        text: impl Into<String>,
        now: Instant,
        ttl: Duration,
    ) {
        self.push_full(kind, text, None, now, ttl);
    }

    /// [`Self::push_at`] carrying an explicit per-toast icon (DRAGON-357).
    pub(super) fn push_full(
        &mut self,
        kind: ToastKind,
        text: impl Into<String>,
        icon: Option<&'static str>,
        now: Instant,
        ttl: Duration,
    ) {
        let text = text.into();
        let expires_at = now + ttl;
        if let Some(i) = self.items.iter().position(|t| t.kind == kind && t.text == text) {
            let mut existing = self.items.remove(i);
            existing.expires_at = expires_at;
            existing.icon = icon;
            self.items.push(existing);
            return;
        }
        self.items.push(Toast { kind, text, icon, expires_at });
        // Newest wins: drop from the FRONT so the most recent notices survive.
        while self.items.len() > MAX_TOASTS {
            self.items.remove(0);
        }
    }

    // DRAGON-467: `push` (the icon-less "now, standard TTL" wrapper) lived here. Its last
    // caller was the delete's partial-failure toast, which went with the delete feature, so
    // every notice the editor posts now names its own glyph through `push_icon`.

    /// [`Self::push_at`] at the current instant with the standard TTL, carrying an explicit
    /// per-toast icon (DRAGON-357) — what every production caller uses.
    pub(super) fn push_icon(&mut self, kind: ToastKind, text: impl Into<String>, icon: &'static str) {
        self.push_full(kind, text, Some(icon), Instant::now(), TOAST_TTL);
    }

    /// Drop every toast whose time is up. Returns whether anything was removed, so the
    /// caller can skip a redraw when nothing changed.
    ///
    /// Per-toast and order-preserving: an entry expiring out of the middle of the stack
    /// leaves the rest in their relative order, and the stack simply reflows.
    pub(super) fn expire(&mut self, now: Instant) -> bool {
        let before = self.items.len();
        self.items.retain(|t| t.expires_at > now);
        self.items.len() != before
    }

    /// Cap every live toast's REMAINING life at `ttl` from `now` — what a hands-on
    /// interaction with the document does to its notices (see [`TOAST_INTERACTION_TTL`]).
    ///
    /// Only ever SHORTENS: a toast already due sooner keeps its earlier deadline, so this
    /// can never resurrect or extend one, and applying it repeatedly (every drag update
    /// fires it) is idempotent — the second call finds the deadline already at or below
    /// the ceiling. Returns whether anything moved, for the same "skip the redraw"
    /// reason [`Self::expire`] does.
    ///
    /// Applies to the WHOLE stack, not just the top card: the user dismissed the
    /// conversation, not one line of it.
    pub(super) fn shorten_to(&mut self, ttl: Duration, now: Instant) -> bool {
        let deadline = now + ttl;
        let mut changed = false;
        for t in &mut self.items {
            if t.expires_at > deadline {
                t.expires_at = deadline;
                changed = true;
            }
        }
        changed
    }

    pub(in crate::app) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(super) fn items(&self) -> &[Toast] {
        &self.items
    }
}

/// The margin between a toast and the top edge of the media area.
const TOAST_TOP_MARGIN: f32 = 12.0;

/// The margin to a toast's right. It clears a VERTICAL scrollbar as well as the media edge:
/// the layer is stacked over the zoom/pan viewport, whose scrollbar sits flush against its
/// right edge, so the reserve is the widget's own [`crate::widgets::zoom_pan::SCROLLBAR_TOTAL`]
/// plus the visual margin. Reading that constant (rather than a hand-picked number) is what
/// keeps the toast off the bar if its thickness ever changes.
const TOAST_RIGHT_MARGIN: f32 = crate::widgets::zoom_pan::SCROLLBAR_TOTAL + 12.0;

/// How opaque a toast card's fill is (DRAGON-357): 85% opaque (15% transparent) for EVERY
/// toast and surface. It used to be 0.92 only when there was no compositor glass, and the
/// chrome's own (often much lower) frost translucency otherwise — which left glass toasts hard
/// to read over a busy capture. One flat 0.85 keeps them legible while still reading as
/// floating over the picture rather than punched into it.
const TOAST_OPACITY: f32 = 0.85;

/// The stacked toast cards, aligned to the TOP-RIGHT of the MEDIA AREA, or `None` when
/// nothing is showing.
///
/// **Newest on top.** The queue is oldest-first, so the column walks it in REVERSE: a new
/// notice pushes in above whatever is already showing, and the stack reads newest → oldest
/// going down. That is also what makes the dedupe-refresh visible — a repeated notice is
/// re-appended as the newest entry, so it visibly jumps back to the top of the stack
/// instead of quietly resetting its timer somewhere in the middle. Each card expires on
/// its own clock and the ones below slide up to fill the gap.
///
/// **Anchoring**: the returned layer is stacked over the media REGION — the space BETWEEN the
/// toolbars ([`super::chrome::toast_region`], from `compose_preview` for a loaded document and
/// from the loading view while one is still arriving) — never over the whole surface. That is
/// what guarantees it can't overlap the toolbars, the annotation tray or the video transport
/// strip in EITHER appearance: in a WINDOW the region is the Fill box between the two bars; in
/// the fullscreen OVERLAY it is the hugged column's width by the media row's height. Both give
/// the same top-right placement from one builder, at any window size, with no per-surface
/// arithmetic to keep in sync.
///
/// It anchors to the region rather than to the media ELEMENT (DRAGON-393): the element is only
/// as big as the fitted picture, so a small capture gave a small centred anchor and the toast
/// visibly flitted from the window's middle to the right as the media settled.
///
/// Deliberately inert: no `mouse_area`, no button, nothing that captures a press — so a
/// toast can never eat a click meant for the picture or the chrome beneath it. It expires
/// on its own; there is nothing to dismiss.
///
/// **DRAGON-454 checked that claim against iced rather than trusting it**, because the owner
/// reported the editor becoming usable at about the moment the opening toast disappeared, and
/// this repo elsewhere records that `stack` does not reliably pass an ignored mouse event from
/// an upper sibling down to a lower one (see `image.rs`, where the annotation canvas OWNS the
/// ZoomPan for exactly that reason). What `stack::update` actually does: it calls `update` on
/// EVERY child, top-down, stopping early only when one CAPTURES the event, and it makes the
/// cursor levitate for the children below an upper child only when that upper child's
/// `mouse_interaction()` is something other than `Interaction::None`. This layer is
/// `container(column(container(row(icon, text))))` — `container` delegates, `column` takes the
/// `max` of its children, and neither the icon nor the text overrides the `Widget` default,
/// which is `Interaction::None`. So a live toast neither captures a press nor levitates the
/// cursor away from the media beneath it. The inertness is real, not merely intended.
pub(super) fn toast_layer(
    toasts: &Toasts,
    glass: Option<crate::app::theme::GlassConfig>,
) -> Option<Element<'static, Msg>> {
    if toasts.is_empty() {
        return None;
    }
    // `.rev()`: the queue is oldest-first, the stack is newest-on-top.
    let cards: Vec<Element<'static, Msg>> =
        toasts.items().iter().rev().map(|t| card(t, glass)).collect();
    Some(
        widget::container(widget::column(cards).spacing(8.0).align_x(Alignment::End))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::End)
            .align_y(Alignment::Start)
            .padding([TOAST_TOP_MARGIN, TOAST_RIGHT_MARGIN, 0.0, 0.0])
            .into(),
    )
}

/// One toast card: a severity-tinted glyph beside its text on a rounded, translucent panel
/// whose hairline border carries the same severity colour.
///
/// # The "glass" here, honestly (DRAGON-353)
///
/// The platform backdrop effects — Windows Mica, macOS vibrancy, cosmic-comp's frosted
/// blur — are WINDOW/surface-level: the compositor blurs what is BEHIND the surface. They
/// cannot be pointed at one widget inside an already-composed iced surface, and a toast
/// floats over our OWN media, not over the desktop, so there is nothing behind it for them
/// to blur.
///
/// What ships is therefore the honest analogue: a TRANSLUCENT theme-derived fill through
/// the same [`crate::app::theme::frost_color`] seam the preview's toolbars use, so when the
/// window really is frosted the toast takes exactly the chrome's translucency and reads as
/// part of the same glass; otherwise it falls back to [`TOAST_OPACITY`]. The severity
/// colours stay theme-sourced either way.
///
/// A genuinely BLURRED backdrop is possible — the wgpu effects pipeline
/// (`widgets::annotation_fx`) already runs blur passes over this same media — but it is
/// per-window retained GPU state keyed to the picture's own geometry, and a transient card
/// that also overhangs the picture's edges is not a natural fit for it. Left as a named
/// follow-up rather than bolted on.
fn card(
    toast: &Toast,
    glass: Option<crate::app::theme::GlassConfig>,
) -> Element<'static, Msg> {
    let kind = toast.kind;
    // A per-toast icon overrides the severity default (DRAGON-357).
    let icon_name = toast.icon.unwrap_or_else(|| kind.icon());
    let glyph = crate::widgets::icons::sized(icon_name, 16.0)
        .class(cosmic::theme::Svg::Custom(std::rc::Rc::new(
            move |t: &cosmic::Theme| cosmic::widget::svg::Style { color: Some(kind.color(t)) },
        )));
    let row = widget::row(vec![glyph.into(), widget::text(toast.text.clone()).size(13).into()])
        .spacing(8.0)
        .align_y(Alignment::Center);
    widget::container(row)
        .padding([8.0, 12.0])
        .max_width(360.0)
        .class(cosmic::theme::Container::Custom(Box::new(move |theme| {
            let mut bg = crate::app::theme::frost_color(
                theme.cosmic().background(false).component.base.into(),
                glass,
            );
            // 85% opaque for EVERY toast/surface (DRAGON-357) — a flat readability floor,
            // replacing the glass-only 0.92 that left frosted toasts too see-through.
            bg.a = TOAST_OPACITY;
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(bg)),
                border: Border {
                    radius: crate::app::theme::rounding(theme).s.into(),
                    width: 1.0,
                    color: kind.color(theme),
                },
                ..Default::default()
            }
        })))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    /// A pushed toast is live until its TTL elapses, then the sweep drops it — and the
    /// sweep reports whether it actually changed anything (so the tick can stand down).
    #[test]
    fn a_toast_lives_for_its_ttl_then_expires() {
        let now = t0();
        let mut q = Toasts::default();
        assert!(q.is_empty());
        q.push_at(ToastKind::Success, "Copied to clipboard", now, TOAST_TTL);
        assert_eq!(q.items().len(), 1);

        // Not yet due: nothing removed, nothing to redraw.
        assert!(!q.expire(now + TOAST_TTL - Duration::from_millis(1)));
        assert_eq!(q.items().len(), 1);
        // Due: removed, and the sweep says so.
        assert!(q.expire(now + TOAST_TTL + Duration::from_millis(1)));
        assert!(q.is_empty());
        // A sweep of an empty queue changes nothing.
        assert!(!q.expire(now + Duration::from_secs(60)));
    }

    /// A per-toast icon (DRAGON-357) is stored and overrides the severity default; a plain
    /// push carries none. Re-pushing the same notice refreshes its icon too.
    #[test]
    fn per_toast_icon_is_carried_and_defaults_to_none() {
        let now = t0();
        let mut q = Toasts::default();
        q.push_at(ToastKind::Success, "plain", now, TOAST_TTL);
        assert_eq!(q.items()[0].icon, None, "a plain push has no icon override");
        q.push_full(ToastKind::Success, "copied", Some("clipboard-check-symbolic"), now, TOAST_TTL);
        assert_eq!(q.items()[1].icon, Some("clipboard-check-symbolic"));
        // A refresh (same kind+text) updates the icon of the moved-to-newest entry.
        q.push_full(ToastKind::Success, "copied", Some("save-check-symbolic"), now, TOAST_TTL);
        assert_eq!(q.items().len(), 2, "the repeat refreshes, not stacks");
        assert_eq!(q.items()[1].icon, Some("save-check-symbolic"));
    }

    /// Severity is carried per toast, and both severities coexist in the queue.
    #[test]
    fn severity_is_per_toast() {
        let now = t0();
        let mut q = Toasts::default();
        q.push_at(ToastKind::Success, "Saved", now, TOAST_TTL);
        q.push_at(ToastKind::Error, "Couldn't copy to clipboard", now, TOAST_TTL);
        let kinds: Vec<ToastKind> = q.items().iter().map(|t| t.kind).collect();
        assert_eq!(kinds, vec![ToastKind::Success, ToastKind::Error]);
    }

    /// The queue is CAPPED, dropping the OLDEST — a burst shows the newest notices rather
    /// than burying the editor under a growing column.
    #[test]
    fn the_queue_caps_at_max_and_drops_the_oldest() {
        let now = t0();
        let mut q = Toasts::default();
        for i in 0..MAX_TOASTS + 2 {
            q.push_at(ToastKind::Success, format!("notice {i}"), now, TOAST_TTL);
        }
        assert_eq!(q.items().len(), MAX_TOASTS);
        let texts: Vec<&str> = q.items().iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["notice 2", "notice 3", "notice 4"]);
    }

    /// PUSH ORDER → RENDER ORDER: the queue is oldest-first, the stack draws newest-FIRST
    /// (top). This is the pairing [`toast_layer`]'s `.rev()` implements; if either end is
    /// ever flipped without the other, this fails.
    #[test]
    fn push_order_renders_newest_first() {
        let now = t0();
        let mut q = Toasts::default();
        for text in ["oldest", "middle", "newest"] {
            q.push_at(ToastKind::Success, text, now, TOAST_TTL);
        }
        let queued: Vec<&str> = q.items().iter().map(|t| t.text.as_str()).collect();
        assert_eq!(queued, vec!["oldest", "middle", "newest"], "the queue is oldest-first");
        let rendered: Vec<&str> = q.items().iter().rev().map(|t| t.text.as_str()).collect();
        assert_eq!(rendered, vec!["newest", "middle", "oldest"], "the stack is newest-on-top");
    }

    /// PER-TOAST expiry out of the MIDDLE of a stack: only that entry goes and the rest
    /// keep their order, so the stack reflows rather than rebuilding.
    #[test]
    fn one_toast_expiring_leaves_the_rest_ordered() {
        let now = t0();
        let mut q = Toasts::default();
        q.push_at(ToastKind::Success, "first", now, Duration::from_secs(10));
        q.push_at(ToastKind::Success, "second", now, Duration::from_secs(1));
        q.push_at(ToastKind::Success, "third", now, Duration::from_secs(10));
        assert!(q.expire(now + Duration::from_secs(2)), "the middle one is due");
        let texts: Vec<&str> = q.items().iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["first", "third"], "the survivors keep their relative order");
    }

    /// Interaction shortens EVERY live toast to the interaction TTL, never lengthens one,
    /// and is idempotent — so a drag that fires it on every motion event can't hold a
    /// toast alive or keep resetting it.
    #[test]
    fn interaction_shortens_every_toast_and_never_lengthens_one() {
        let now = t0();
        let mut q = Toasts::default();
        q.push_at(ToastKind::Success, "long", now, TOAST_TTL);
        q.push_at(ToastKind::Error, "already brief", now, Duration::from_millis(100));
        let brief_deadline = q.items()[1].expires_at;

        assert!(q.shorten_to(TOAST_INTERACTION_TTL, now), "the long one moves");
        assert_eq!(q.items()[0].expires_at, now + TOAST_INTERACTION_TTL);
        assert_eq!(
            q.items()[1].expires_at, brief_deadline,
            "a toast already due sooner keeps its earlier deadline"
        );

        // Idempotent at the same instant, and a LATER interaction never pushes the
        // deadline back out.
        assert!(!q.shorten_to(TOAST_INTERACTION_TTL, now), "nothing left to shorten");
        let later = now + Duration::from_millis(400);
        assert!(!q.shorten_to(TOAST_INTERACTION_TTL, later), "a later ceiling is not applied");
        assert_eq!(q.items()[0].expires_at, now + TOAST_INTERACTION_TTL);

        // And the shortened toast really does get swept at its new time (well short of
        // the full TTL).
        assert!(q.expire(now + TOAST_INTERACTION_TTL + Duration::from_millis(1)));
        assert!(q.is_empty());
    }

    /// PER DOCUMENT: shortening one document's queue leaves another's untouched. (The
    /// dispatch is keyed by window id; this pins the state itself as unshared.)
    #[test]
    fn shortening_one_document_leaves_another_alone() {
        let now = t0();
        let mut a = Toasts::default();
        let mut b = Toasts::default();
        a.push_at(ToastKind::Success, "doc a", now, TOAST_TTL);
        b.push_at(ToastKind::Success, "doc b", now, TOAST_TTL);
        a.shorten_to(TOAST_INTERACTION_TTL, now);
        assert_eq!(a.items()[0].expires_at, now + TOAST_INTERACTION_TTL);
        assert_eq!(b.items()[0].expires_at, now + TOAST_TTL, "the sibling is untouched");
    }

    /// Re-posting the SAME notice refreshes it instead of stacking a duplicate — and it
    /// moves to the newest slot, so its timer really is restarted.
    #[test]
    fn an_identical_notice_is_refreshed_not_duplicated() {
        let now = t0();
        let mut q = Toasts::default();
        q.push_at(ToastKind::Success, "Copied to clipboard", now, TOAST_TTL);
        q.push_at(ToastKind::Success, "Saved", now, TOAST_TTL);
        q.push_at(ToastKind::Success, "Copied to clipboard", now + Duration::from_secs(1), TOAST_TTL);
        assert_eq!(q.items().len(), 2, "the repeat must not stack");
        let texts: Vec<&str> = q.items().iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["Saved", "Copied to clipboard"], "the repeat becomes newest");
        // …which, since the stack draws newest-first, means it visibly JUMPS BACK TO THE
        // TOP rather than quietly resetting its timer in the middle of the column.
        let rendered: Vec<&str> = q.items().iter().rev().map(|t| t.text.as_str()).collect();
        assert_eq!(rendered, vec!["Copied to clipboard", "Saved"]);
        // Its expiry was pushed out by the second post, so it outlives the older one.
        assert!(q.expire(now + TOAST_TTL + Duration::from_millis(1)));
        assert_eq!(q.items().len(), 1);
        assert_eq!(q.items()[0].text, "Copied to clipboard");

        // The SAME text at a DIFFERENT severity is a different notice (an error after a
        // success must be visible, not swallowed as a repeat).
        let mut q = Toasts::default();
        q.push_at(ToastKind::Success, "Copied to clipboard", now, TOAST_TTL);
        q.push_at(ToastKind::Error, "Copied to clipboard", now, TOAST_TTL);
        assert_eq!(q.items().len(), 2);
    }
}
