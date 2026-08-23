//! Black-box CLI tests: run the built binary and assert on its observable behavior.
//!
//! These exercise the binary's command surface rather than its internals, so they stay
//! stable across internal refactors and act as a safety net while the modules are
//! reorganized. They avoid anything that needs capture hardware (PulseAudio, PipeWire,
//! a compositor), so they run anywhere `cargo test` does.
//!
//! DRAGON-695 rewrote them for the subcommand grammar. The parser itself is pure and covered
//! exhaustively by unit tests in `src/cli/parse.rs`; what only the REAL BINARY can prove is
//! the thing those cannot: that a command line we do not understand ends here, on stderr with
//! a non-zero exit, rather than falling through to a capture overlay on the user's screen.

use assert_cmd::Command;
use predicates::prelude::*;

fn cck() -> Command {
    Command::cargo_bin("cosmic-capture-kit").expect("the binary builds")
}

/// The help is the whole public surface, and the hidden namespaces must not appear in it.
#[test]
fn help_lists_every_public_command_and_no_private_one() {
    for flag in ["--help", "-h"] {
        cck()
            .arg(flag)
            .assert()
            .success()
            .stdout(predicate::str::contains("cosmic-capture-kit <command> [options]"))
            .stdout(predicate::str::contains("scan"))
            .stdout(predicate::str::contains("color"))
            .stdout(predicate::str::contains("capture"))
            .stdout(predicate::str::contains("record"))
            .stdout(predicate::str::contains("editor <file>"))
            .stdout(predicate::str::contains("timer <0-99>"))
            .stdout(predicate::str::contains("sound <both|mic|system|none>"))
            .stdout(predicate::str::contains("settings"))
            .stdout(predicate::str::contains("permissions"))
            .stdout(predicate::str::contains("tray"))
            // The `debug` namespace is ours. A user who reads it starts filing tickets on it.
            .stdout(predicate::str::contains("debug").not());
    }
}

#[test]
fn version_prints_the_cargo_version() {
    for flag in ["--version", "-V"] {
        cck()
            .arg(flag)
            .assert()
            .success()
            .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
    }
}

/// THE regression this rework exists for: nothing we do not understand may reach a surface.
///
/// Black-box on purpose. Before DRAGON-695 every one of these fell through the whole of
/// `main` and opened a capture overlay, which is a fullscreen grab in answer to a typo. Exit
/// code 2 plus the offending word on stderr is what a script (or a desktop shortcut's error
/// report) reads.
#[test]
fn an_unknown_command_or_flag_exits_two_and_says_what_it_was() {
    for bad in [
        vec!["shoot"],
        vec!["--region"],
        vec!["capture"],
        vec!["capture", "--region", "--monitor"],
        vec!["capture", "--region", "--timer", "500"],
        vec!["color"],
        vec!["scan", "--action", "reload"],
        vec!["record"],
        vec!["editor"],
        vec!["timer", "abc"],
        vec!["sound", "microfone"],
        vec!["settings", "--now"],
    ] {
        cck()
            .args(&bad)
            .env("CCK_DEBUG_LOG", "0")
            .assert()
            .code(2)
            .stderr(predicate::str::is_empty().not());
    }
}

/// DRAGON-583, respelled: a recording-control command with nothing recording must END, saying
/// so, and must never fall through to a normal launch.
///
/// `XDG_RUNTIME_DIR` is redirected at a temp dir, deliberately and not just for hygiene:
/// against the developer's real runtime dir this test could FIND a live recording and stop
/// it. Off Linux the commands are inert, and the message says which, so the assertion is on
/// the exit status and the command spelling, which both platforms share.
#[test]
fn a_recording_command_with_no_recording_reports_and_exits() {
    let dir = std::env::temp_dir().join(format!("cck-cli-no-recording-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp runtime dir");
    for args in [
        vec!["record", "--action", "pause"],
        vec!["record", "--action", "save"],
        vec!["record", "--action", "cancel"],
        vec!["record", "--tool", "erase"],
        vec!["record", "--sound", "mic"],
    ] {
        cck()
            .args(&args)
            .env("XDG_RUNTIME_DIR", &dir)
            // Keep the run out of the developer's debug log as well.
            .env("CCK_DEBUG_LOG", "0")
            .assert()
            .code(1)
            .stderr(predicate::str::contains("record"));
    }
    // The `--sound` variant points at the command that DOES persist a default, so a user who
    // reached this by accident is one line from what they meant.
    cck()
        .args(["record", "--sound", "none"])
        .env("XDG_RUNTIME_DIR", &dir)
        .env("CCK_DEBUG_LOG", "0")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("sound <both|mic|system|none>"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The scanner refresh answers the same way with no scanner open.
#[test]
fn a_scan_refresh_with_no_scanner_reports_and_exits() {
    let dir = std::env::temp_dir().join(format!("cck-cli-no-scanner-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp runtime dir");
    cck()
        .args(["scan", "--action", "refresh"])
        .env("XDG_RUNTIME_DIR", &dir)
        .env("CCK_DEBUG_LOG", "0")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("scan --action refresh"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `editor <file>` rejects a file that is not there rather than opening an empty window.
#[test]
fn the_editor_reports_a_missing_file() {
    let missing = std::env::temp_dir().join("cck-cli-definitely-not-here.png");
    let _ = std::fs::remove_file(&missing);
    cck()
        .arg("editor")
        .arg(&missing)
        .env("CCK_DEBUG_LOG", "0")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("file not found"));
}

/// DRAGON-695 review follow-up: what a SETTING command prints, that command accepts back.
///
/// End to end rather than as a unit test, because the property spans the printer and the
/// parser and only the real binary exercises both. It used to fail on the wording alone: bare
/// `sound` answered `sound: None`, capitalised, and `timer` answered `timer: 0s`, neither of
/// which is a value the command takes.
///
/// A fake `HOME` (and the XDG dirs under it) so this never reads or writes the developer's own
/// config, which these two commands really do persist to.
#[test]
fn a_setting_command_prints_a_value_it_accepts_back() {
    let home = std::env::temp_dir().join(format!("cck-cli-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("temp home");
    let run = |args: &[&str]| -> String {
        let out = cck()
            .args(args)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("XDG_RUNTIME_DIR", home.join("run"))
            .env("LOCALAPPDATA", home.join("local"))
            .env("CCK_DEBUG_LOG", "0")
            .output()
            .expect("the binary runs");
        assert!(out.status.success(), "{args:?} should succeed");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // `sound`: every state prints its own lowercase word, and typing that word back lands on
    // the same state.
    for word in ["both", "mic", "system", "none"] {
        assert_eq!(run(&["sound", word]), format!("sound: {word}"));
        assert_eq!(run(&["sound"]), format!("sound: {word}"));
    }
    let printed = run(&["sound"]);
    let value = printed.strip_prefix("sound: ").expect("the print names its value");
    assert_eq!(value, value.to_ascii_lowercase(), "the CLI vocabulary is lowercase");
    assert_eq!(run(&["sound", value]), printed, "the printed value must round-trip");

    // `timer`: a bare number, no unit suffix and no prose, including for the 0 that clears it.
    for secs in ["0", "7", "99"] {
        assert_eq!(run(&["timer", secs]), format!("timer: {secs}"));
        assert_eq!(run(&["timer"]), format!("timer: {secs}"));
    }
    let printed = run(&["timer"]);
    let value = printed.strip_prefix("timer: ").expect("the print names its value");
    assert!(value.parse::<u64>().is_ok(), "`timer` must print a bare number, got {value:?}");
    assert_eq!(run(&["timer", value]), printed, "the printed value must round-trip");

    let _ = std::fs::remove_dir_all(&home);
}

// ── The hidden `debug` namespace ─────────────────────────────────────────────

#[test]
fn debug_test_help_lists_the_harnesses() {
    cck()
        .args(["debug", "test", "help"])
        .assert()
        .success()
        .stderr(predicate::str::contains("debug test <name>"))
        .stderr(predicate::str::contains("bench-encoders"))
        .stderr(predicate::str::contains("scan <image>"));
}

#[test]
fn debug_test_with_no_name_prints_help() {
    cck().args(["debug", "test"]).assert().success().stderr(predicate::str::contains("usage:"));
}

#[test]
fn debug_test_unknown_subcommand_reports_then_lists() {
    cck()
        .args(["debug", "test", "definitely-not-a-test"])
        .assert()
        .success()
        .stderr(predicate::str::contains("unknown test 'definitely-not-a-test'"))
        .stderr(predicate::str::contains("usage:"));
}

#[test]
fn debug_inspect_reports_when_no_metadata_present() {
    // A plain file carries no Cosmic Capture Kit metadata; `debug inspect` should say so and
    // still exit 0 (it's a query, not a failure). Works with or without ffprobe present,
    // since both the non-media and the missing-tool paths yield "no metadata".
    let tmp = std::env::temp_dir().join("cck-cli-inspect-none.bin");
    std::fs::write(&tmp, b"not a capture file").expect("write temp file");
    cck()
        .args(["debug", "inspect"])
        .arg(&tmp)
        .assert()
        .success()
        .stderr(predicate::str::contains("No Cosmic Capture Kit metadata"));
    let _ = std::fs::remove_file(&tmp);
}
