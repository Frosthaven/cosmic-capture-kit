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
