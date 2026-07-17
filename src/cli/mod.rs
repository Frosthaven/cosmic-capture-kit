//! CLI diagnostics harness (`--test`) and the `--inspect` /
//! `--make-sync-clip` / `--calibrate-sync` subcommands.
mod diagnostics;
mod inspect;
mod sync;
pub use diagnostics::run_test;
pub use inspect::inspect;
pub use sync::{calibrate_sync, make_sync_clip, SYNC_WORKFLOW};
