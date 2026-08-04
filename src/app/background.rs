//! How this app starts background work: [`off_thread`], never `tokio::task::spawn_blocking`
//! (DRAGON-497, lifted here in DRAGON-499).
//!
//! # The rule
//!
//! A job that blocks runs on a DETACHED OS thread and answers through a oneshot channel, so
//! `Task::perform` still gets a future and the caller still gets a message. What it does NOT
//! get is a handle the executor owns.
//!
//! # Why the blocking pool is forbidden
//!
//! iced's executor IS a `tokio::runtime::Runtime`, and it is dropped on the MAIN thread inside
//! `cosmic::app::run`, before `app::run`'s `libc::_exit(2)` can be reached. Dropping a runtime
//! drops its blocking pool, and that drop waits with NO timeout for every blocking closure that
//! has already started. A `spawn_blocking` closure cannot be cancelled either: dropping its
//! `JoinHandle` (which is exactly what iced does to a `Task` when the window goes away) only
//! detaches it. So ONE background job still running at close parks the main thread in a condvar
//! for as long as that job takes, with the window still on screen and nothing left to pump it.
//!
//! A detached thread is owned by nobody, so the exit path has nothing to wait on and
//! `libc::_exit(2)` cuts the worker off mid-flight, which is exactly the teardown philosophy
//! `app::run` already documents. Linux and macOS have no backstop here either: the hard-exit
//! watchdog in `App::finish_session` is `#[cfg(windows)]`.
//!
//! # Who lives by it
//!
//! * The Cloud Accounts page (DRAGON-497). Its longest job is one the user is EXPECTED to walk
//!   away from: an abandoned sign-in keeps its loopback listener polling for the full
//!   `cloud::oauth::BROWSER_DEADLINE` (five minutes).
//! * The update path (DRAGON-499): the check that EVERY settings mint fires, and the one-click
//!   install. The check is the worse of the two, because it is not something the user asked
//!   for: opening settings and closing it again should never be able to leave the process
//!   parked on a `curl` that a slow network (or a wedged one) has not finished with.
//! * The Windows encoder probe (DRAGON-238, joined in DRAGON-499): the other job a settings
//!   window starts, and seconds long, because it runs `ffmpeg -encoders` plus real hardware
//!   probe-encodes.
//! * The preview editor's auto-copy worker (DRAGON-454) predates this module and hand-rolls the
//!   same thread + oneshot shape in `app::preview::share`. It is the same rule, written down
//!   there before there was anywhere shared to put it.
//!
//! ONE `spawn_blocking` is left in `app`, deliberately: the macOS early tiling-WM pause in
//! `surfaces.rs`. It is awaited during a capture LAUNCH, before any surface exists, rather than
//! held across a close, and the work it waits on is bounded inside `wait_for_early_pause`. It is
//! named here so the next reader does not have to wonder whether it was missed.
//!
//! The rule is about TEARDOWN, not about bounds. A detached worker that hangs still leaks a
//! thread for the life of the process, so a job that shells out still owes its child a bounded
//! reap (DRAGON-118); `crate::update::run_bounded` is the update path's.

/// Run `op` on a DETACHED OS thread and hand its result back as a future to `Task::perform`.
///
/// **This is how the app starts background work off the UI thread**, and the module doc lists
/// the two sites that predate it. See it for why `tokio::task::spawn_blocking` is forbidden in
/// anything a window close can outrun: the executor's blocking pool is
/// joined, unbounded, on the main thread at process exit, so a job started there can hold the
/// whole process open after its window is gone.
///
/// `None` means the worker died without answering (it panicked). Every caller turns that into
/// the same sentence it used to give a `JoinError`, so the observable behaviour of a broken
/// worker is unchanged.
pub(crate) fn off_thread<T: Send + 'static>(
    op: impl FnOnce() -> T + Send + 'static,
) -> impl std::future::Future<Output = Option<T>> + Send {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(op());
    });
    async move { rx.await.ok() }
}

#[cfg(test)]
mod off_thread_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// How long a test worker pretends to work for. Long enough that a pinned teardown is
    /// unambiguous against scheduler noise, short enough not to weigh on the suite.
    const WORK: Duration = Duration::from_millis(400);

    /// A worker that reports when it has actually STARTED, then works for [`WORK`].
    ///
    /// Reporting the start matters: tokio discards blocking tasks that are still QUEUED at
    /// shutdown, so a test that dropped the runtime before the closure got going would prove
    /// the opposite of what it claims.
    fn slow_worker(started: std::sync::mpsc::Sender<()>) -> impl FnOnce() -> u8 + Send + 'static {
        move || {
            let _ = started.send(());
            std::thread::sleep(WORK);
            7
        }
    }

    fn await_it<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime")
            .block_on(f)
    }

    /// **The DRAGON-497 wedge, reproduced.** A job on the executor's blocking pool holds the
    /// runtime's DROP for as long as the job runs, and that drop happens on the main thread
    /// inside `cosmic::app::run`, upstream of `app::run`'s `libc::_exit`. So the settings
    /// window closing does not end the process; the last background job does.
    ///
    /// This is the behaviour the app must never go back to, which is why it is pinned here
    /// rather than merely described in the module doc.
    #[test]
    fn a_blocking_pool_job_pins_the_executor_teardown() {
        let rt = tokio::runtime::Runtime::new().expect("a tokio runtime");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        rt.spawn_blocking(slow_worker(started_tx));
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the blocking job must actually start before the runtime is dropped");

        let began = Instant::now();
        drop(rt);
        assert!(
            began.elapsed() >= WORK / 2,
            "dropping the executor returned in {:?}, so this test is no longer reproducing \
             the wedge it exists to pin",
            began.elapsed()
        );
    }

    /// **The fix.** The same job on a detached OS thread costs the teardown nothing: the
    /// executor owns no handle to it, so its drop returns at once and the process reaches
    /// `_exit`, which is what cuts the worker off mid-flight.
    #[test]
    fn a_detached_worker_does_not_pin_the_executor_teardown() {
        let rt = tokio::runtime::Runtime::new().expect("a tokio runtime");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let result = off_thread(slow_worker(started_tx));
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the detached worker must actually start before the runtime is dropped");

        let began = Instant::now();
        drop(rt);
        assert!(
            began.elapsed() < WORK / 2,
            "dropping the executor waited {:?} on a DETACHED worker, so something re-attached \
             this app's background work to the blocking pool",
            began.elapsed()
        );

        // And detaching costs the caller nothing: the answer still arrives.
        assert_eq!(await_it(result), Some(7));
    }

    /// DRAGON-499: the update check is the job the settings window fires on EVERY mint, so it
    /// is the one most likely to still be running when the window closes. Shaped exactly like
    /// the check's own worker (fetch, then the interactive floor), it must cost the teardown
    /// nothing and still deliver its status.
    #[test]
    fn an_update_check_shaped_worker_does_not_pin_the_executor_teardown() {
        let rt = tokio::runtime::Runtime::new().expect("a tokio runtime");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        // The real closure: a blocking fetch, then `check_floor_remainder` held on the SAME
        // thread. Both halves used to be `spawn_blocking` calls, so either could pin the drop.
        let status = off_thread(move || {
            let _ = started_tx.send(());
            let began = Instant::now();
            std::thread::sleep(WORK);
            let remainder =
                crate::update::check_floor_remainder(began.elapsed(), WORK * 2);
            if !remainder.is_zero() {
                std::thread::sleep(remainder);
            }
            crate::update::UpdateStatus::Unknown
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the check worker must actually start before the runtime is dropped");

        let began = Instant::now();
        drop(rt);
        assert!(
            began.elapsed() < WORK / 2,
            "dropping the executor waited {:?} on the update check, so it is back on the \
             blocking pool and a slow check stalls the settings close again",
            began.elapsed()
        );
        assert_eq!(await_it(status), Some(crate::update::UpdateStatus::Unknown));
    }
}
