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
//!
//! # DRAGON-433 rides along
//!
//! The CONFIG directory had the identical leak — a test process reading (and potentially
//! writing) the developer's real `config.toml` — fixed with the identical mechanism, sharing
//! `util::is_dev_process` and `util::sandbox_key`. So it gets its proof here, against the same
//! spawned child and the same fake home, rather than in a second file that would duplicate all
//! of this to ask one more question of one more path.

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
    //
    // Not on Windows: the app resolves the user's log folder through the known-folder API
    // (see `user_log_dir`), so the fake home cannot stand in for it — the fake path reads as
    // neutral and is rightly honoured. Aiming the override at the REAL folder instead would
    // risk the exact pollution DRAGON-424 forbids, so the refused half is Linux/mac-only;
    // the honoured half above still runs everywhere.
    #[cfg(not(windows))]
    {
        let home = FakeHome::new("override-refused");
        let user_log = home.user_log();
        let out = home.run(&[("CCK_LOG_FILE", &user_log)]);
        assert!(out.status.success());
        assert!(!user_log.exists(), "an override into the user's folder must be refused");
        assert!(announced_path(&out).starts_with(std::env::temp_dir()));
    }
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
/// DRAGON-433: the same child, asked the same question about its CONFIG.
///
/// The log leak and the config leak are the same shape — a spawned child resolving a
/// user-facing path for itself — so they get the same proof. `state::load()` in a test used to
/// read whoever-ran-the-suite's `config.toml`, which made test behaviour depend on the
/// developer's settings, and a test that SAVED could overwrite them.
///
/// The child here has a private `$HOME`, so the config directory it would resolve as a real
/// user lands inside it. Nothing may appear there: a dev process resolves into the temp
/// sandbox instead, and it must do so whether it reads or writes.
#[test]
fn a_spawned_child_of_a_test_never_touches_the_user_s_config() {
    let home = FakeHome::new("config");
    let out = home.run(&[]);
    assert!(out.status.success(), "the child should exit 0");
    for dir in [
        // The uniform location the app pins to on every OS (`~/.config/cosmic-capture-kit`).
        home.root.join(".config").join("cosmic-capture-kit"),
        // And the XDG one Linux resolves through, which the fake home also redirects.
        home.root.join("config").join("cosmic-capture-kit"),
    ] {
        assert!(
            !dir.exists(),
            "a test's child created a config dir in the user's home: {dir:?}"
        );
    }
    assert!(no_config_files_under(&home.root), "a test's child left config under {:?}", home.root);
}

/// Whether `root` is free of anything this app would recognise as its own config state — the
/// config file itself, and the two sidecars that live beside it (`update.rs`'s manifest cache,
/// `preview::edit`'s covermark drop folder). Named files rather than an extension sweep,
/// because a fake home legitimately contains other `.toml`/`.json` from nothing to do with us.
fn no_config_files_under(root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else { return true };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "covermarks") || !no_config_files_under(&p) {
                return false;
            }
        } else if p
            .file_name()
            .is_some_and(|n| n == "config.toml" || n == "update-manifest.json")
        {
            return false;
        }
    }
    true
}

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
