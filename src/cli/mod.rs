//! The command line: the parser that owns every flag spelling, and the one-shot commands it
//! dispatches to.
//!
//! DRAGON-695 replaced a flat bag of flags with a subcommand grammar, and moved the parsing
//! itself out of `main` and into [`parse`]. That module's doc carries the whole story,
//! including why a fall-through parser was the wrong shape. Everything else here is a
//! command's BODY: the diagnostics harness (`debug test`), the metadata dump (`debug
//! inspect`), the A/V sync tools, the recording controls and the scanner refresh.
pub mod parse;

mod diagnostics;
mod inspect;
mod recording;
mod scanner;
mod sync;
pub use diagnostics::run_test;
pub use inspect::inspect;
pub use parse::{parse, Plan};
pub use recording::{
    recording_commands_supported, run_recording_commands, RECORDING_COMMANDS,
    SCAN_REFRESH_COMMAND,
};
pub use scanner::run_scan_refresh;
pub use sync::{calibrate_sync, make_sync_clip};
