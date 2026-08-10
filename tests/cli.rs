//! Black-box CLI tests: run the built binary and assert on its observable behavior.
//!
//! These exercise the binary's command surface rather than its internals, so they stay
//! stable across internal refactors and act as a safety net while the modules are
//! reorganized. They avoid anything that needs capture hardware (PulseAudio, PipeWire,
//! a compositor), so they run anywhere `cargo test` does.

use assert_cmd::Command;
use predicates::prelude::*;

fn cck() -> Command {
    Command::cargo_bin("cosmic-capture-kit").expect("the binary builds")
}

#[test]
fn test_help_lists_the_subcommands() {
    cck()
        .args(["--test", "help"])
        .assert()
        .success()
        .stderr(predicate::str::contains("--test <name>"))
        .stderr(predicate::str::contains("bench-encoders"))
        .stderr(predicate::str::contains("scan <image>"));
}

#[test]
fn test_with_no_name_prints_help() {
    cck().arg("--test").assert().success().stderr(predicate::str::contains("usage:"));
}

#[test]
fn test_unknown_subcommand_reports_then_lists() {
    cck()
        .args(["--test", "definitely-not-a-test"])
        .assert()
        .success()
        .stderr(predicate::str::contains("unknown test 'definitely-not-a-test'"))
        .stderr(predicate::str::contains("usage:"));
}

/// DRAGON-583: a recording-control flag with nothing recording must END, saying so, and
/// must never fall through to a normal launch.
///
/// That fall-through is the whole reason this test is black-box: an argument `main` does
/// not recognise reaches the ordinary launch path and opens a CAPTURE OVERLAY, so the flag
/// being parsed at all is a property only the real binary can prove. A non-zero exit with
/// the reason on stderr is also what a script (or a desktop shortcut's error report) reads.
///
/// `XDG_RUNTIME_DIR` is redirected at a temp dir, deliberately and not just for hygiene:
/// against the developer's real runtime dir this test could FIND a live recording and stop
/// it. Off Linux the flags are inert, and the message says which, so the assertion is on
/// the exit status and the flag name, which both platforms share.
#[test]
fn a_recording_command_with_no_recording_reports_and_exits() {
    let dir = std::env::temp_dir().join(format!("cck-cli-no-recording-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp runtime dir");
    for flag in [
        "--toggle-mic",
        "--toggle-system-audio",
        "--pause-recording",
        "--finish-recording",
        "--cancel-recording",
    ] {
        cck()
            .arg(flag)
            .env("XDG_RUNTIME_DIR", &dir)
            // Keep the run out of the developer's debug log as well.
            .env("CCK_DEBUG_LOG", "0")
            .assert()
            .code(1)
            .stderr(predicate::str::contains(flag));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn inspect_reports_when_no_metadata_present() {
    // A plain file carries no Cosmic Capture Kit metadata; --inspect should say so and
    // still exit 0 (it's a query, not a failure). Works with or without ffprobe present,
    // since both the non-media and the missing-tool paths yield "no metadata".
    let tmp = std::env::temp_dir().join("cck-cli-inspect-none.bin");
    std::fs::write(&tmp, b"not a capture file").expect("write temp file");
    cck()
        .arg("--inspect")
        .arg(&tmp)
        .assert()
        .success()
        .stderr(predicate::str::contains("No Cosmic Capture Kit metadata"));
    let _ = std::fs::remove_file(&tmp);
}
