//! Build script. macOS-only: compile the tiny MediaRemote proxy dylib the
//! preview media-ducker loads under `/usr/bin/perl` (DRAGON-171). On every other
//! target this is a no-op — the ducker's mac arm is `#[cfg(target_os = "macos")]`,
//! and Linux/Windows builds stay byte-identical (no compiler invoked, no output).
//!
//! The dylib (`src/platform/mac/services/duck_mac/mrduck.m`) is built into `OUT_DIR` and the
//! ducker embeds it with `include_bytes!(concat!(env!("OUT_DIR"), ...))`, so the
//! proxy ships INSIDE our binary — it works for both the packaged `.app` and the
//! bare `target/release/` the daemon launches, with no external file to locate,
//! and no change to `scripts/mac-package.sh`. At engage time the ducker writes
//! the bytes to a temp file and hands its path to the perl driver.

fn main() {
    #[cfg(target_os = "macos")]
    {
        // DRAGON-199: the private SkyLight framework (the CGS window-server client API
        // used for the active-Space / window-Space queries in src/platform/mac/spaces.rs)
        // lives in /System/Library/PrivateFrameworks, which is NOT on the default linker
        // framework search path. Add it so `#[link(name = "SkyLight")]` resolves — the
        // same path every macOS window manager (yabai, AeroSpace) links SkyLight from.
        println!("cargo:rustc-link-search=framework=/System/Library/PrivateFrameworks");
        build_mrduck();
    }
}

#[cfg(target_os = "macos")]
fn build_mrduck() {
    let src = "src/platform/mac/services/duck_mac/mrduck.m";
    println!("cargo:rerun-if-changed={src}");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let dylib = std::path::Path::new(&out_dir).join("libmrduck.dylib");

    // Compile the Objective-C proxy to a standalone dylib. We can't use the `cc`
    // crate's `compile()` (it builds a static archive for linking INTO our
    // binary); we need a loadable dylib the perl driver `dl_load_file`s at
    // runtime. So drive the same compiler `cc` discovers, with our own flags.
    let compiler = cc::Build::new().get_compiler();
    let mut cmd = compiler.to_command();
    cmd.arg("-dynamiclib")
        .arg("-fobjc-arc")
        .arg("-fvisibility=hidden")
        .arg("-framework")
        .arg("Foundation")
        .arg("-o")
        .arg(&dylib)
        .arg(src);

    let status = cmd.status().expect("failed to spawn the mrduck compiler");
    assert!(status.success(), "mrduck.m compile failed: {status}");
}
