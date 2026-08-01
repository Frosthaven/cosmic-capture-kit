//! The startup self-exit guard (DRAGON-413): a capture child that never presents
//! anything ends itself.
//!
//! ## The leak this closes
//!
//! macOS capture children are spawned by the menu-bar daemon "fully detached (their
//! own session), so there is no SIGCHLD to reap and no supervision to do"
//! ([`crate::daemon`]'s module doc), and — since DRAGON-351 — they take no
//! single-instance lock either. Nothing therefore notices a child that never reaches
//! [`crate::app::App::finish_session`], and nothing caps how many can stack: a user
//! whose captures silently fail retries, and each retry leaves another invisible
//! process behind (a customer on Sonoma accumulated six).
//!
//! The fix is deliberately SELF-termination rather than parent-side supervision,
//! because it is the only mechanism that still works when the daemon has crashed,
//! been force-quit, or is itself wedged. It also survives a stall that happens
//! BEFORE the iced runtime is up: the guard is a plain detached thread armed in
//! `main`, so a hang inside `App::init` (a TCC probe, the scene grab, wgpu) is
//! covered where an in-app timer subscription could never fire.
//!
//! ## The three edges
//!
//! 1. **A child showing the permission window is doing its job.** That state was the
//!    literal trigger of the leak (DRAGON-412), so a naive "kill anything that has
//!    not captured" rule would kill the legitimate case. The budget is therefore
//!    SUSPENDED, not merely generous, while a permission window is up
//!    ([`Presence::AwaitingUser`] contributes ZERO to the clock, for as long as the
//!    user reads it).
//! 2. **The budget must never fire on a slow-but-working launch.** See
//!    [`DEFAULT_BUDGET`] for the number and its justification.
//! 3. **"Presented" is a precise point.** See [`presence`].
//!
//! ## Shape
//!
//! * The decision is a pure state machine ([`Budget::advance`] over [`Presence`]),
//!   unit-tested on every platform including the suspension case.
//! * The live status is PUSHED from the app's `update` dispatch, which recomputes it
//!   from existing `App` state (open surfaces) — no new lifecycle coupling, no new
//!   fields, and it cannot go stale in a way that matters: [`Presence::Presented`]
//!   latches permanently in the clock, and the permission window's own 1s poll keeps
//!   [`Presence::AwaitingUser`] refreshed while it is open.
//! * Exit is `std::process::exit(0)` after the same pre-exit teardown
//!   `finish_session` does — quiet and clean, never a panic, never a crash dialog.
//!
//! **No logging, no telemetry** (explicit owner constraint on DRAGON-413): the guard
//! reports nothing anywhere, it just stops being a process.
//!
//! ## Scope
//!
//! This guard BOUNDS the pile; it does not prevent a second child existing. Keeping
//! one from ever stacking behind the first (the role-keyed single-instance macOS
//! lacks and Windows has) is DRAGON-416 — deliberately not done here, because the
//! capture lock was removed on purpose in DRAGON-351 and re-adding one is a design
//! decision, not a bug fix.
//!
//! Armed on macOS only, and only for CAPTURE launches (`--settings`, `--permissions`
//! and `--preview <file>` are windows the user asked for and keep their historical
//! unbounded life). Linux and Windows are byte-identical — the module is not even
//! compiled there outside `cfg(test)`. Both have the same detached-child shape and
//! could opt in later by calling [`arm`] from their own launch paths; nothing here is
//! macOS-specific except the one pre-exit teardown call.

// Compiled on every platform under `cfg(test)` so the pure decision logic is covered
// by the Linux suite (this is a Linux dev box; the macOS lifecycle is
// headless-unverifiable here), but only ARMED on macOS — hence the runtime half is
// dead code elsewhere.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

/// How long a capture child may go without presenting ANYTHING before it exits.
///
/// The cost asymmetry is the whole argument: killing a legitimate capture is far
/// worse than the leak being fixed, so this is chosen to be unmistakably longer than
/// any working launch rather than as tight as the leak would like.
///
/// What the clock actually measures is `main()` → the first user-visible surface, and
/// only the part of it the user is NOT responsible for (the permission window freezes
/// it). On this app that stretch is the iced/winit/wgpu boot, `App::init`'s prompt-free
/// TCC probes, the capture-scene grab, and the bounded (~4s) AeroSpace pause latch —
/// a couple of seconds on a warm machine. The assumptions behind 90s:
///
/// * A cold first launch on modest hardware (a Mac mini, a spinning disk, an Intel
///   Mac doing first-run Metal shader compilation) is assumed to be able to take
///   10-20s. 90s is ~5x that pathological case and ~20-40x a normal launch.
/// * Gatekeeper's first-launch verification of a freshly downloaded app happens
///   before our process exists, so it does not spend this budget.
/// * A machine so loaded it cannot present a surface within 90s of wall clock is
///   not a machine where one more capture attempt was going to work anyway.
/// * Bounding the pile is what matters, not bounding it tightly: a user retrying a
///   broken capture every ~30s tops out at ~3 live children instead of the six the
///   customer saw growing without limit.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(90);

/// QA override for [`DEFAULT_BUDGET`], in whole seconds; `0` disables the guard
/// entirely. Exists so the macOS behaviour can actually be exercised by hand
/// (`CCK_STARTUP_BUDGET_SECS=5` makes a stuck child disappear in five seconds
/// instead of ninety) — it is a review aid in the same spirit as the
/// `CCK_HEALTH_FORCE_*` flags, not a supported setting.
pub const BUDGET_ENV: &str = "CCK_STARTUP_BUDGET_SECS";

/// How often the guard thread wakes to re-read the live [`Presence`]. Far finer than
/// the budget, so the SUSPENSION reacts to a permission window opening/closing within
/// a quarter second, and coarse enough to be free.
const STEP: Duration = Duration::from_millis(250);

/// What the child is doing, as far as the guard is concerned. The three cases are
/// ordered by how they treat the clock: burn it, freeze it, stop it for good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Presence {
    /// Nothing user-visible yet. The budget burns. This is the state a wedged
    /// startup stays in forever, and the only one that can end the process.
    Starting = 0,
    /// A window the USER must act on is up (on macOS, the permission checker). The
    /// child is doing its job and the human owns the clock, so the budget is FROZEN
    /// for as long as this lasts — however long that is. Not a longer budget: no
    /// amount of reading a permission card may ever add up to a kill.
    AwaitingUser = 1,
    /// The child has put its work in front of the user. TERMINAL: the guard disarms
    /// permanently the first time it sees this, so nothing later in the session —
    /// an overlay torn down for a preview handoff, a preview closed — can ever
    /// re-arm it. From here the one-shot model's own seam
    /// ([`crate::app::App::finish_session`]) owns the exit.
    Presented = 2,
}

impl Presence {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Presence::AwaitingUser,
            2 => Presence::Presented,
            // Anything unrecognised reads as "not presented yet", which is the state
            // that keeps the guard ARMED — the conservative direction is to keep
            // watching, never to silently disarm.
            _ => Presence::Starting,
        }
    }
}

/// Classify the child from what it currently has on screen.
///
/// **"Presented a capture" is defined as: the child owns at least one surface or
/// in-flight capture the user can see the result of.** Concretely the caller passes
/// `visible_work = true` once ANY of these holds — a capture overlay is PLACED (a
/// minted-but-unplaced overlay renders a transparent `Space` and the user sees NOTHING,
/// DRAGON-439), a countdown is ticking, a pixel capture is in flight, a recording is
/// running, a preview editor is open, or a settings window is up.
///
/// Why that point and not, say, "a file was written": it is the first moment the
/// child is demonstrably not the failure this ticket is about. Before it, the child
/// is invisible and a user who sees nothing happen will retry — that is exactly how
/// the pile forms. After it, the user can see the session and end it themselves, and
/// every path onward routes through `finish_session`. It is also deliberately BROAD
/// (an immediate `--active-window` capture never mints an overlay, and a
/// `--active-monitor --video` recording can legitimately run for hours) so that no
/// legitimate flow is left burning budget.
///
/// `awaiting_user` (a permission window is open) only matters when there is no
/// visible work: real work always wins, since it is terminal and the stronger signal.
pub fn presence(visible_work: bool, awaiting_user: bool) -> Presence {
    if visible_work {
        Presence::Presented
    } else if awaiting_user {
        Presence::AwaitingUser
    } else {
        Presence::Starting
    }
}

/// What the child currently has on screen, as plain data (DRAGON-439).
///
/// The app fills this in from the surfaces it owns (`App::startup_presence`) and hands it
/// to [`classify`]. Splitting the snapshot from the decision is what makes the decision
/// testable: the fields are the app's real state, but nothing here needs an `App`, a
/// compositor or a Mac, so the whole table is exercised by the local suite on any
/// platform. Every field is "is this thing visible RIGHT NOW", never "was it ever".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Surfaces {
    /// A capture overlay is PLACED — raised to the shielding level and framed to its
    /// display, which is the first moment it draws anything.
    ///
    /// Deliberately not "an overlay exists". macOS mints the overlay window first and
    /// places it a frame or two later; between the two it renders a fully transparent
    /// `Space` (the DRAGON-204 anti-flicker gate). Reading the MINT as presented is the
    /// DRAGON-439 bug: a runtime that wedged in that gap disarmed the guard while the
    /// user saw an empty screen, leaving exactly the invisible immortal child this
    /// module exists to stop.
    pub overlay_placed: bool,
    /// A preview editor window/overlay is open.
    pub preview_open: bool,
    /// A capture countdown is ticking (the user can see the counter).
    pub countdown: bool,
    /// A pixel capture is in flight.
    pub capturing: bool,
    /// A recording is running (an immediate `--active-monitor --video` launch mints NO
    /// overlay at all and can legitimately run for hours).
    pub recording: bool,
    /// The in-app settings window is open.
    pub settings_open: bool,
    /// The macOS permission checker is up — the user owns the clock (DRAGON-412).
    pub permission_window: bool,
    /// A capture-failure alert is up — likewise the user's to dismiss (DRAGON-415).
    pub alert: bool,
}

/// Map a [`Surfaces`] snapshot onto the guard's three cases.
///
/// The split is simply which column each surface belongs in: the work ones DISARM the
/// guard for good, the two user-owned windows FREEZE it, and nothing at all burns the
/// budget. Real work outranks a user-owned window, exactly as in [`presence`], which this
/// delegates to so there is only ever one copy of that rule.
pub fn classify(s: &Surfaces) -> Presence {
    presence(
        s.overlay_placed
            || s.preview_open
            || s.countdown
            || s.capturing
            || s.recording
            || s.settings_open,
        s.permission_window || s.alert,
    )
}

/// What the guard should do after one step of the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Keep watching.
    Wait,
    /// Stop watching, for good — the child presented.
    Disarm,
    /// The budget is spent with nothing presented: end the process.
    Exit,
}

/// The pure budget clock: unsuspended time spent so far against the allowance, plus
/// the terminal disarm latch.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    spent: Duration,
    allowance: Duration,
    disarmed: bool,
}

impl Budget {
    pub fn new(allowance: Duration) -> Self {
        Budget { spent: Duration::ZERO, allowance, disarmed: false }
    }

    /// Unsuspended time accumulated so far (test/reasoning aid).
    #[cfg_attr(not(test), allow(dead_code))] // test-only; no production caller yet
    pub fn spent(&self) -> Duration {
        self.spent
    }

    /// Advance the clock by `dt` given the child's current [`Presence`].
    ///
    /// The one rule that matters: `dt` is added ONLY under [`Presence::Starting`].
    /// [`Presence::AwaitingUser`] is not a slower burn or a bigger allowance — it
    /// contributes nothing at all, so a permission window can be read for an hour
    /// and the child is exactly as far from being killed as when it opened.
    pub fn advance(&mut self, dt: Duration, presence: Presence) -> Verdict {
        if self.disarmed {
            return Verdict::Disarm;
        }
        match presence {
            Presence::Presented => {
                self.disarmed = true;
                Verdict::Disarm
            }
            Presence::AwaitingUser => Verdict::Wait,
            Presence::Starting => {
                self.spent = self.spent.saturating_add(dt);
                if self.spent >= self.allowance {
                    Verdict::Exit
                } else {
                    Verdict::Wait
                }
            }
        }
    }
}

/// Resolve the budget from the raw [`BUDGET_ENV`] value. `None` means the guard is
/// DISABLED (an explicit `0`); anything absent, blank or unparseable falls back to
/// [`DEFAULT_BUDGET`] rather than to a shorter one — a typo must never make the guard
/// more trigger-happy.
pub fn budget_from_env(raw: Option<&str>) -> Option<Duration> {
    let secs = raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<u64>().ok());
    match secs {
        Some(0) => None,
        Some(s) => Some(Duration::from_secs(s)),
        None => Some(DEFAULT_BUDGET),
    }
}

/// The live [`Presence`], published by [`report`] and read by the guard thread.
/// Starts at [`Presence::Starting`]: a process that never reaches its `update` loop
/// at all is precisely the case being guarded against.
static PRESENCE: AtomicU8 = AtomicU8::new(Presence::Starting as u8);

/// Publish the child's current [`Presence`]. Called from the app's `update` dispatch
/// on every message, recomputed from existing state — cheap (one relaxed store) and
/// impossible to forget at a mutation site.
pub fn report(p: Presence) {
    PRESENCE.store(p as u8, Ordering::Relaxed);
}

fn current() -> Presence {
    Presence::from_u8(PRESENCE.load(Ordering::Relaxed))
}

/// Start the guard. `budget` of `None` (see [`budget_from_env`]) does nothing at all —
/// not even a thread.
///
/// The thread is detached and outlives nothing: a normal session either disarms it
/// ([`Presence::Presented`]) or exits out from under it.
pub fn arm(budget: Option<Duration>) {
    let Some(allowance) = budget else { return };
    let _ = std::thread::Builder::new()
        .name("cck-startup-guard".into())
        .spawn(move || {
            let mut clock = Budget::new(allowance);
            let mut last = std::time::Instant::now();
            loop {
                std::thread::sleep(STEP);
                let now = std::time::Instant::now();
                // Real elapsed, not the nominal STEP: an oversleeping (heavily loaded)
                // machine must not get a secretly longer budget than it asked for.
                let dt = now.saturating_duration_since(last);
                last = now;
                match clock.advance(dt, current()) {
                    Verdict::Wait => {}
                    Verdict::Disarm => return,
                    Verdict::Exit => give_up(),
                }
            }
        });
}

/// End the process quietly. Not a panic (the macOS panic hook would write a crash log
/// and the default hook would print), not `abort` (that raises the crash reporter) —
/// a plain zero exit, after the small pre-exit teardown `finish_session` also does.
///
/// Only the teardown that is SAFE from a foreign thread and bounded belongs here: the
/// tiling-WM resume (idempotent, a detached spawn, and skipping it would leave a
/// user's AeroSpace disabled) and this instance's cross-process state markers. Nothing
/// here touches AppKit or the iced runtime, which by definition may be wedged.
fn give_up() -> ! {
    #[cfg(target_os = "macos")]
    crate::platform::mac::window::resume_tiling_wm();
    crate::instance::set_recording_marker(false);
    crate::instance::set_preview_marker(false);
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: Duration = Duration::from_secs(90);

    // ── `presence`: the classification ────────────────────────────────────────

    #[test]
    fn nothing_on_screen_is_starting() {
        assert_eq!(presence(false, false), Presence::Starting);
    }

    #[test]
    fn a_permission_window_alone_is_awaiting_user() {
        assert_eq!(presence(false, true), Presence::AwaitingUser);
    }

    #[test]
    fn visible_work_is_presented_and_outranks_a_permission_window() {
        assert_eq!(presence(true, false), Presence::Presented);
        // Both at once (a capture overlay up while the checker is still open):
        // Presented wins, because it is the terminal, stronger signal.
        assert_eq!(presence(true, true), Presence::Presented);
    }

    // ── `classify`: the surface snapshot (DRAGON-439) ─────────────────────────
    //
    // These pin `classify`'s truth table, which is the whole of the decision. They do
    // NOT cover the line that FILLS the snapshot — `App::startup_presence`'s
    // `outputs.iter().any(|o| o.user_visible())` — because that needs a live `App` and a
    // real overlay. Off macOS `user_visible` is hardcoded `true`, so even the local
    // suite's type-check cannot tell whether the mac arm reads the right field. That one
    // line is mac-QA only.

    #[test]
    fn a_minted_but_unplaced_overlay_is_not_presented() {
        // THE regression this ticket is about. The overlay window exists — the app has
        // an `OutputState` for it — but `place_overlay` has not landed, so it draws a
        // transparent `Space` and the user is looking at nothing. The guard must keep
        // watching, i.e. the mint→placement stretch BURNS budget like any other part of
        // a slow start.
        assert_eq!(classify(&Surfaces::default()), Presence::Starting);
    }

    #[test]
    fn a_placed_overlay_presents_and_outranks_a_permission_window() {
        let s = Surfaces { overlay_placed: true, ..Surfaces::default() };
        assert_eq!(classify(&s), Presence::Presented);
        // Both at once (the checker still up while the overlay lands): Presented wins.
        let s = Surfaces { overlay_placed: true, permission_window: true, ..Surfaces::default() };
        assert_eq!(classify(&s), Presence::Presented);
    }

    #[test]
    fn a_user_owned_window_freezes_the_clock_while_the_overlay_is_still_unplaced() {
        // Waiting on the permission checker (or on a failure alert) is the user's time,
        // and an overlay that has not been placed yet does not change that.
        let s = Surfaces { permission_window: true, ..Surfaces::default() };
        assert_eq!(classify(&s), Presence::AwaitingUser);
        let s = Surfaces { alert: true, ..Surfaces::default() };
        assert_eq!(classify(&s), Presence::AwaitingUser);
    }

    #[test]
    fn every_other_kind_of_work_presents_on_its_own() {
        // None of these mints an overlay, and each is a legitimate whole session: an
        // `--active-window` still, a countdown, an overlay-less `--active-monitor
        // --video` recording, a preview handed off from another child, `--settings`.
        // Any one of them alone must disarm the guard, or the guard kills real work.
        let none = Surfaces::default();
        for (what, s) in [
            ("preview", Surfaces { preview_open: true, ..none }),
            ("countdown", Surfaces { countdown: true, ..none }),
            ("capturing", Surfaces { capturing: true, ..none }),
            ("recording", Surfaces { recording: true, ..none }),
            ("settings", Surfaces { settings_open: true, ..none }),
        ] {
            assert_eq!(classify(&s), Presence::Presented, "{what} alone must present");
        }
    }

    #[test]
    fn a_mint_then_wedge_still_exits() {
        // The customer's exact shape, driven through the real clock: the overlay window
        // is minted (so the old `!outputs.is_empty()` test would have reported
        // Presented and disarmed here), placement never lands, and the runtime is stuck.
        // 90s later the child must end itself.
        let minted_unplaced = Surfaces::default();
        let mut c = Budget::new(B);
        let step = Duration::from_millis(250);
        let mut verdict = Verdict::Wait;
        for _ in 0..(90 * 4) {
            verdict = c.advance(step, classify(&minted_unplaced));
            if verdict != Verdict::Wait {
                break;
            }
        }
        assert_eq!(verdict, Verdict::Exit);
        // And the same timeline with placement landing on the first step disarms
        // instead — the guard is not simply killing every launch.
        let placed = Surfaces { overlay_placed: true, ..Surfaces::default() };
        let mut c = Budget::new(B);
        assert_eq!(c.advance(step, classify(&placed)), Verdict::Disarm);
    }

    // ── The clock ─────────────────────────────────────────────────────────────

    #[test]
    fn a_stuck_child_exits_when_the_budget_is_spent() {
        let mut c = Budget::new(B);
        let step = Duration::from_secs(1);
        for _ in 0..89 {
            assert_eq!(c.advance(step, Presence::Starting), Verdict::Wait);
        }
        assert_eq!(c.advance(step, Presence::Starting), Verdict::Exit);
    }

    #[test]
    fn the_budget_is_not_spent_early() {
        let mut c = Budget::new(B);
        assert_eq!(c.advance(Duration::from_millis(89_999), Presence::Starting), Verdict::Wait);
        assert_eq!(c.advance(Duration::from_millis(1), Presence::Starting), Verdict::Exit);
    }

    // ── The sharpest edge: the permission window suspends the budget ──────────

    #[test]
    fn a_permission_window_freezes_the_clock_indefinitely() {
        // The DRAGON-412 shape: the child presents the permission checker and the
        // user reads it. An hour of that must not move the budget one millisecond —
        // this is the case most likely to regress into killing a live session.
        let mut c = Budget::new(B);
        for _ in 0..(60 * 60) {
            assert_eq!(
                c.advance(Duration::from_secs(1), Presence::AwaitingUser),
                Verdict::Wait
            );
        }
        assert_eq!(c.spent(), Duration::ZERO);
    }

    #[test]
    fn suspension_is_a_freeze_not_a_reset() {
        // Burn half the budget, sit in the permission window for ages, then resume:
        // the child still gets exactly the REMAINING half, no more and no less.
        let mut c = Budget::new(B);
        let s = Duration::from_secs(1);
        for _ in 0..45 {
            assert_eq!(c.advance(s, Presence::Starting), Verdict::Wait);
        }
        for _ in 0..600 {
            assert_eq!(c.advance(s, Presence::AwaitingUser), Verdict::Wait);
        }
        assert_eq!(c.spent(), Duration::from_secs(45));
        for _ in 0..44 {
            assert_eq!(c.advance(s, Presence::Starting), Verdict::Wait);
        }
        assert_eq!(c.advance(s, Presence::Starting), Verdict::Exit);
    }

    #[test]
    fn a_permission_window_can_never_exit_on_its_own() {
        // Even at the very edge of the budget, the suspended state never fires.
        let mut c = Budget::new(B);
        assert_eq!(c.advance(Duration::from_millis(89_999), Presence::Starting), Verdict::Wait);
        for _ in 0..1000 {
            assert_eq!(c.advance(Duration::from_secs(60), Presence::AwaitingUser), Verdict::Wait);
        }
    }

    // ── Presenting disarms, permanently ───────────────────────────────────────

    #[test]
    fn presenting_disarms() {
        let mut c = Budget::new(B);
        assert_eq!(c.advance(Duration::from_secs(89), Presence::Starting), Verdict::Wait);
        assert_eq!(c.advance(Duration::from_secs(1), Presence::Presented), Verdict::Disarm);
    }

    #[test]
    fn the_disarm_is_terminal() {
        // Once presented, later states can never re-arm the guard — a capture overlay
        // torn down for a preview handoff, or a preview closed on the way to
        // `finish_session`, reports Starting again and must stay harmless.
        let mut c = Budget::new(B);
        assert_eq!(c.advance(Duration::from_secs(1), Presence::Presented), Verdict::Disarm);
        for _ in 0..10_000 {
            assert_eq!(c.advance(Duration::from_secs(60), Presence::Starting), Verdict::Disarm);
        }
        assert_eq!(
            c.advance(Duration::from_secs(60), Presence::AwaitingUser),
            Verdict::Disarm
        );
    }

    #[test]
    fn a_long_capture_session_is_never_killed() {
        // The `--active-monitor --video` shape: no overlay, no preview, just a
        // recording running for two hours. It reports Presented from the first
        // message on, so the guard is gone long before.
        let mut c = Budget::new(B);
        assert_eq!(c.advance(Duration::from_millis(250), Presence::Presented), Verdict::Disarm);
        for _ in 0..(2 * 60 * 60) {
            assert_eq!(c.advance(Duration::from_secs(1), Presence::Presented), Verdict::Disarm);
        }
    }

    // ── Budget resolution ─────────────────────────────────────────────────────

    #[test]
    fn budget_defaults_when_unset_or_unparseable() {
        assert_eq!(budget_from_env(None), Some(DEFAULT_BUDGET));
        assert_eq!(budget_from_env(Some("")), Some(DEFAULT_BUDGET));
        assert_eq!(budget_from_env(Some("   ")), Some(DEFAULT_BUDGET));
        assert_eq!(budget_from_env(Some("soon")), Some(DEFAULT_BUDGET));
        assert_eq!(budget_from_env(Some("-5")), Some(DEFAULT_BUDGET));
        assert_eq!(budget_from_env(Some("2.5")), Some(DEFAULT_BUDGET));
    }

    #[test]
    fn budget_honours_an_explicit_value() {
        assert_eq!(budget_from_env(Some("5")), Some(Duration::from_secs(5)));
        assert_eq!(budget_from_env(Some(" 5 ")), Some(Duration::from_secs(5)));
        assert_eq!(budget_from_env(Some("600")), Some(Duration::from_secs(600)));
    }

    #[test]
    fn zero_disables_the_guard() {
        assert_eq!(budget_from_env(Some("0")), None);
    }

    // ── The published status ──────────────────────────────────────────────────

    #[test]
    fn presence_round_trips_through_the_atomic() {
        for p in [Presence::Starting, Presence::AwaitingUser, Presence::Presented] {
            report(p);
            assert_eq!(current(), p);
        }
        // An unknown byte reads as Starting — armed, never silently disarmed.
        assert_eq!(Presence::from_u8(200), Presence::Starting);
        report(Presence::Starting);
    }
}
