//! CLI diagnostics harness (`--test`) and the `--inspect` /
//! `--make-sync-clip` / `--calibrate-sync` / recording-control subcommands.
mod diagnostics;
mod inspect;
mod recording;
mod sync;
pub use diagnostics::run_test;
pub use inspect::inspect;
pub use recording::{
    recording_command_from_args, recording_commands_supported, run_recording_command,
    RECORDING_FLAGS,
};
pub use sync::{calibrate_sync, make_sync_clip, SYNC_WORKFLOW};
