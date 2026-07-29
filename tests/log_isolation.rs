//! DRAGON-424: a test run must never write into the user's real debug log.
//!
//! # What went wrong
//!
//! `cargo test` was appending to the owner's live `~/.local/state/cosmic-capture-kit/logs/
//! debug.log` — caught because that log was open at the time, being read to diagnose a real
//! recording wedge, and it ended with a debug-profile 0.24.0 test binary's session header
//! interleaved into a release 0.23.0 session. The whole promise of DRAGON-419 is "enable it,
//! reproduce once, mail us the file"; a file with someone else's build in it is not evidence.
//!
//! # Why the test lives HERE and not next to the code
//!
//! The leak was not the harness writing. It was `tests/cli.rs` spawning the REAL binary as a
//! subprocess — a child compiled without `cfg(test)`, which resolves its own log path and
//! knows nothing about the harness that started it. Only a test that actually spawns the
//! binary can prove that case, so this file does exactly what the leak did: runs the built exe
//! with the debug log forced ON, and looks at where the bytes went.
//!
//! Each child runs against a PRIVATE home so the assertion is "the child did not write to the
//! log folder IT resolved", made without reading, touching or depending on the real one.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The binary under test, as cargo built it — the same handle `tests/cli.rs` uses.
const BIN: &str = env!("CARGO_BIN_EXE_cosmic-capture-kit");

/// A throwaway `$HOME` for one child process.
///
/// The point is that the child's OWN resolved user-log folder lands inside this directory, so
/// "did it write to the user's log?" can be answered by looking here — no read of the real
/// file, no dependence on whether the machine running the suite has one, and nothing left
/// behind that a person could confuse for their own log.
struct FakeHome {
    root: PathBuf,
}

impl FakeHome {
    fn new(tag: &str) -> FakeHome {
        let root = std::env::temp_dir()
            .join(format!("cck-d424-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create the fake home");
        FakeHome { root }
    }

    /// Where the app would put the log for this fake user, per platform.
    ///
    /// Windows resolves `%LOCALAPPDATA%` through the known-folder API rather than the
    /// environment, so a fake home cannot redirect it there; the Windows assertions below are
    /// the ones that do not need it.
    fn user_log(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.root.join("Library/Logs/cosmic-capture-kit/debug.log")
        } else {
            self.root.join("state/cosmic-capture-kit/logs/debug.log")
        }
    }

    /// Run the binary as a short CLI subcommand — the exact shape that leaked — with the debug
    /// log forced on and this fake home in place.
    fn run(&self, extra: &[(&str, &Path)]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.args(["--test", "definitely-not-a-test"])
            .env("CCK_DEBUG_LOG", "1")
            .env("HOME", &self.root)
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("LOCALAPPDATA", self.root.join("local"))
            .env_remove("CCK_LOG_FILE");
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output().expect("the binary runs")
    }
}

impl Drop for FakeHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The path the child announces on stderr when it isolates its log.
fn announced_path(out: &Output) -> PathBuf {
    let err = String::from_utf8_lossy(&out.stderr);
    let line = err
        .lines()
        .find(|l| l.contains("debug log isolated to"))
        .unwrap_or_else(|| panic!("no isolation notice on stderr:\n{err}"));
    let (_, tail) = line.split_once("isolated to ").expect("the notice names a path");
    PathBuf::from(tail.trim_end_matches(" (DRAGON-424)").trim())
}

/// THE regression test: a spawned child of a test, with logging on, writes nothing to the log
/// folder it resolved for the user.
#[test]
fn a_spawned_child_of_a_test_never_writes_to_the_user_s_log() {
    let home = FakeHome::new("child");
    let out = home.run(&[]);
    assert!(out.status.success(), "the child should exit 0");

    let user_log = home.user_log();
    assert!(
        !user_log.exists(),
        "a test's child wrote into the user's log folder: {user_log:?}"
    );
    // The whole fake home must be untouched, not just that one filename — a rotation backup
    // or a differently-named file would be the same defect.
    assert!(no_log_files_under(&home.root), "a test's child left log files under {:?}", home.root);

    // And the sink is REDIRECTED, not broken: the child still recorded its session, somewhere
    // that is ours. A fix that silently disabled the log would pass the assertion above and
    // teach us nothing about whether the log still works.
    let sandbox = announced_path(&out);
    assert!(sandbox.starts_with(std::env::temp_dir()), "sandbox not under temp: {sandbox:?}");
    assert!(!sandbox.starts_with(&home.root), "the sandbox must not be the user's home");
    let body = std::fs::read_to_string(&sandbox).expect("the sandbox log exists");
    assert!(body.contains("debug log (process start)"), "no session header in {sandbox:?}");
    assert!(body.contains("dev/test process"), "the sandbox log must say what it is");
}

/// The override still works — that is how a harness or a controlled experiment redirects the
/// log — but it cannot be pointed at the user's own folder.
#[test]
fn the_path_override_is_honoured_but_never_into_the_user_s_folder() {
    // Honoured: a neutral file gets the session.
    let home = FakeHome::new("override-ok");
    let named = home.root.join("named-by-the-caller.log");
    let out = home.run(&[("CCK_LOG_FILE", &named)]);
    assert!(out.status.success());
    let body = std::fs::read_to_string(&named).expect("the named file got the log");
    assert!(body.contains("debug log (process start)"));

    // Refused: the same variable aimed at the user's own log lands in the sandbox instead,
    // and the user's file is still not created.
    let home = FakeHome::new("override-refused");
    let user_log = home.user_log();
    let out = home.run(&[("CCK_LOG_FILE", &user_log)]);
    assert!(out.status.success());
    assert!(!user_log.exists(), "an override into the user's folder must be refused");
    assert!(announced_path(&out).starts_with(std::env::temp_dir()));
}

/// Nothing at all is written when the log is off, which is the default a customer has.
#[test]
fn a_child_with_the_log_off_writes_nowhere() {
    let home = FakeHome::new("off");
    let out = Command::new(BIN)
        .args(["--test", "definitely-not-a-test"])
        .env("CCK_DEBUG_LOG", "0")
        .env("HOME", &home.root)
        .env("XDG_STATE_HOME", home.root.join("state"))
        .env("XDG_CONFIG_HOME", home.root.join("config"))
        .env_remove("CCK_LOG_FILE")
        .output()
        .expect("the binary runs");
    assert!(out.status.success());
    assert!(no_log_files_under(&home.root));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("debug log isolated to"), "an off log must not announce anything");
}

/// Whether any `*.log` exists anywhere beneath `root`.
fn no_log_files_under(root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else { return true };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if !no_log_files_under(&p) {
                return false;
            }
        } else if p.extension().is_some_and(|x| x == "log") {
            return false;
        }
    }
    true
}
