//! DRAGON-406: the PURE half of the Windows chrome/transparency diagnostics report —
//! formatting, redaction, the briefing text, and the rotation policy, with no Win32 in
//! sight so the whole thing is unit-tested on Linux.
//!
//! DRAGON-408 widened this from a Windows **10** instrument to a Windows instrument. The
//! report is now written on EVERY Windows run of every build, 10 and 11 alike, with no
//! trigger of any kind, because the open question is no longer "what is wrong on 10" but
//! "what is DIFFERENT between 10 and 11" — and that needs a control sample from a machine
//! that works. See [`BRIEFING`] for the whole story as the file itself tells it.
//!
//! The Win32 half (the file writer, the style read-backs, the swapchain probe) lives in
//! `platform/windows/diag.rs`, which is stripped from the public GPL export; this module
//! stays in the shared tree precisely because it is portable logic the Linux gate can
//! cover. Everything here is dead code off Windows (hence the module-wide
//! `cfg_attr(not(windows), allow(dead_code))` on each item) — the TESTS still run
//! everywhere, which is the point.
//!
//! # Removal (DRAGON-407)
//!
//! This whole file is DRAGON-406 instrumentation and comes out wholesale with the ticket.
//! Every insertion point elsewhere carries a `DRAGON-406` tag so the removal is a
//! `grep -rn DRAGON-406` and a delete. The two things DRAGON-407 keeps
//! (`DwmSetWindowAttribute` HRESULT checking and the ex-style read-back, both in
//! `platform/windows/wm/window.rs`) are marked `DRAGON-407 KEEP` at their definitions and
//! do NOT live here.
//!
//! # No PII, by construction
//!
//! Two helpers exist solely to make that a property of the code rather than a habit:
//! [`known_window_role`] maps only OUR OWN fixed window titles to a fixed role string and
//! returns `None` for anything else (the caller then emits a positional
//! `overlay-for-display-N`, never the title), and [`redact_args`] passes through flags and
//! short bare words but replaces every path-shaped argv token with `<value>`. A customer
//! must be able to mail this file without mailing the names of the documents they were
//! screenshotting.

/// Windows extended-style bits, most-significant meaning first for readability. Kept as a
/// plain table (rather than pulled from the `windows` crate) so this module is portable and
/// the names read identically in a log a human is scanning.
#[cfg_attr(not(windows), allow(dead_code))]
const EX_STYLE_BITS: &[(u32, &str)] = &[
    (0x0000_0001, "DLGMODALFRAME"),
    (0x0000_0004, "NOPARENTNOTIFY"),
    (0x0000_0008, "TOPMOST"),
    (0x0000_0010, "ACCEPTFILES"),
    (0x0000_0020, "TRANSPARENT"),
    (0x0000_0040, "MDICHILD"),
    (0x0000_0080, "TOOLWINDOW"),
    (0x0000_0100, "WINDOWEDGE"),
    (0x0000_0200, "CLIENTEDGE"),
    (0x0000_0400, "CONTEXTHELP"),
    (0x0000_1000, "RIGHT"),
    (0x0000_2000, "RTLREADING"),
    (0x0000_4000, "LEFTSCROLLBAR"),
    (0x0001_0000, "CONTROLPARENT"),
    (0x0002_0000, "STATICEDGE"),
    (0x0004_0000, "APPWINDOW"),
    (0x0008_0000, "LAYERED"),
    (0x0010_0000, "NOINHERITLAYOUT"),
    (0x0020_0000, "NOREDIRECTIONBITMAP"),
    (0x0040_0000, "LAYOUTRTL"),
    (0x0200_0000, "COMPOSITED"),
    (0x0800_0000, "NOACTIVATE"),
];

/// Windows base-style bits. `WS_MINIMIZEBOX`/`WS_MAXIMIZEBOX` share their values with
/// `WS_GROUP`/`WS_TABSTOP`; on a top-level window (all we ever log) the box meaning is the
/// right one, so that is the name used.
#[cfg_attr(not(windows), allow(dead_code))]
const STYLE_BITS: &[(u32, &str)] = &[
    (0x8000_0000, "POPUP"),
    (0x4000_0000, "CHILD"),
    (0x2000_0000, "MINIMIZE"),
    (0x1000_0000, "VISIBLE"),
    (0x0800_0000, "DISABLED"),
    (0x0400_0000, "CLIPSIBLINGS"),
    (0x0200_0000, "CLIPCHILDREN"),
    (0x0100_0000, "MAXIMIZE"),
    (0x0080_0000, "BORDER"),
    (0x0040_0000, "DLGFRAME"),
    (0x0020_0000, "VSCROLL"),
    (0x0010_0000, "HSCROLL"),
    (0x0008_0000, "SYSMENU"),
    (0x0004_0000, "THICKFRAME"),
    (0x0002_0000, "MINIMIZEBOX"),
    (0x0001_0000, "MAXIMIZEBOX"),
];

/// Render `bits` as `0x00080088(TOOLWINDOW|LAYERED|…)` against a name table. Unknown bits
/// are folded into a trailing `+0x…` term so a value is never silently lost — the whole
/// point of the read-back is that we see EXACTLY what the OS has, including bits nobody
/// here put there.
#[cfg_attr(not(windows), allow(dead_code))]
fn describe_bits(bits: u32, table: &[(u32, &str)]) -> String {
    let mut names: Vec<&str> = Vec::new();
    let mut known = 0u32;
    for (mask, name) in table {
        if bits & mask == *mask {
            names.push(name);
            known |= mask;
        }
    }
    let leftover = bits & !known;
    let mut body = names.join("|");
    if leftover != 0 {
        if !body.is_empty() {
            body.push('|');
        }
        body.push_str(&format!("+{leftover:#010x}"));
    }
    if body.is_empty() {
        body.push_str("none");
    }
    format!("{bits:#010x}({body})")
}

/// Human-readable extended window style — see [`describe_bits`].
#[cfg_attr(not(windows), allow(dead_code))]
pub fn describe_ex_style(ex: u32) -> String {
    describe_bits(ex, EX_STYLE_BITS)
}

/// Human-readable base window style — see [`describe_bits`].
#[cfg_attr(not(windows), allow(dead_code))]
pub fn describe_style(style: u32) -> String {
    describe_bits(style, STYLE_BITS)
}

/// The name of an HRESULT we care about on this path, or `None` for anything else.
///
/// `E_INVALIDARG` is the headline: a `DwmSetWindowAttribute` for an attribute Windows 10
/// does not know (every `DWMWA_` past `DWMWA_CLOAKED`) fails with exactly that, silently,
/// which is what made the DRAGON-403/405 round blind. `DWM_E_COMPOSITIONDISABLED` and
/// `DWM_E_NO_REDIRECTION_SURFACE_AVAILABLE` are the other two that would rewrite the whole
/// diagnosis if they ever showed up.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn hresult_name(hr: i32) -> Option<&'static str> {
    Some(match hr as u32 {
        0x0000_0000 => "S_OK",
        0x0000_0001 => "S_FALSE",
        0x8000_4001 => "E_NOTIMPL",
        0x8000_4005 => "E_FAIL",
        0x8000_FFFF => "E_UNEXPECTED",
        0x8007_0005 => "E_ACCESSDENIED",
        0x8007_0006 => "E_HANDLE",
        0x8007_000E => "E_OUTOFMEMORY",
        0x8007_0057 => "E_INVALIDARG",
        0x8007_0490 => "ERROR_NOT_FOUND",
        0x8026_3001 => "DWM_E_COMPOSITIONDISABLED",
        0x8026_3002 => "DWM_E_REMOTING_NOT_SUPPORTED",
        0x8026_3003 => "DWM_E_NO_REDIRECTION_SURFACE_AVAILABLE",
        0x8026_3005 => "DWM_E_ADAPTER_NOT_FOUND",
        _ => return None,
    })
}

/// Format an HRESULT as `0x80070057 (E_INVALIDARG)`, or bare hex when unrecognised. Used by
/// BOTH the always-on `warn!` (DRAGON-407 keeps that) and the diagnostics file, so the two
/// never disagree about what a failure was.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn format_hresult(hr: i32) -> String {
    match hresult_name(hr) {
        Some(name) => format!("{:#010x} ({name})", hr as u32),
        None => format!("{:#010x}", hr as u32),
    }
}

/// The ROLE of one of our own windows, derived from the title we set — or `None` when the
/// title is not one of our fixed constants (which, in this app, means it is a capture
/// overlay titled with the DISPLAY NAME).
///
/// This is the PII seam: a caller must never log a title, and the only titles this returns
/// anything for are compile-time constants of ours. A `None` sends the caller to a
/// positional `overlay-for-display-N` instead, so a monitor's model string never reaches
/// the file either.
///
/// Titles are matched with the same ` [` tiling-WM-annotation tolerance
/// `platform::windows::window::title_matches` uses, since komorebi rewrites them.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn known_window_role(title: &str) -> Option<&'static str> {
    const ROLES: &[(&str, &str)] = &[
        ("Cosmic Capture Kit - Settings", "settings"),
        ("Cosmic Capture Kit - Preview Editor", "preview-window"),
        ("Cosmic Capture Kit - Preview Overlay", "preview-overlay"),
        ("Cosmic Capture Kit: Permissions", "permissions"),
    ];
    ROLES.iter().find_map(|(want, role)| {
        let hit = title == *want
            || title.strip_prefix(*want).is_some_and(|rest| rest.starts_with(" ["));
        hit.then_some(*role)
    })
}

/// Whether an argv token is safe to log verbatim: a flag (`-x` / `--flag`), or a short bare
/// word with no path shape. Everything else is a value that could be a file path, a save
/// target, or a window title, and is redacted by [`redact_args`].
#[cfg_attr(not(windows), allow(dead_code))]
fn arg_is_safe(token: &str) -> bool {
    if token.starts_with('-') {
        // A flag can still smuggle a path in `--flag=<path>` form; keep only the flag part
        // there (handled by the caller splitting on '='), so a bare `-`-led token with no
        // separators is safe.
        return !token.contains(['/', '\\', ':']);
    }
    token.len() <= 24
        && !token.is_empty()
        && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Redact a process's argv for the log: flags survive, path-shaped values become `<value>`.
/// `argv[0]` (the exe path) is dropped entirely — it carries the user's install location and
/// tells us nothing the version line doesn't.
///
/// This is what lets the log answer "which launch path was this?" (hotkey child, tray child,
/// direct run) without carrying a single byte of the user's filesystem.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn redact_args<S: AsRef<str>>(args: &[S]) -> String {
    let mut out: Vec<String> = Vec::new();
    for a in args.iter().skip(1) {
        let a = a.as_ref();
        match a.split_once('=') {
            // `--flag=value`: keep the flag, judge the value on its own merits.
            Some((flag, value)) if flag.starts_with('-') => {
                let shown = if arg_is_safe(value) { value } else { "<value>" };
                out.push(format!("{flag}={shown}"));
            }
            _ => out.push(if arg_is_safe(a) { a.to_string() } else { "<value>".into() }),
        }
    }
    if out.is_empty() { "(none)".into() } else { out.join(" ") }
}

/// The size at which the shared diagnostics file is rotated to its single `.1` backup, so a
/// months-old install never grows something a customer cannot attach to an email. 2 MiB of
/// this log is many hundreds of sessions.
#[cfg_attr(not(windows), allow(dead_code))]
pub const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

/// The per-PROCESS write budget. Rotation happens at process start, so a single long-lived
/// process (the resident tray daemon) could otherwise outrun it; this caps what any one
/// process can add. Well above a full noisy session (a session header plus a few dozen
/// window lines is single-digit KiB).
#[cfg_attr(not(windows), allow(dead_code))]
pub const MAX_SESSION_BYTES: u64 = 256 * 1024;

/// Pure rotation policy: rotate when the file has reached the cap. Split out so the bound
/// is a tested fact rather than an inline comparison — the failure mode it guards (an
/// unsendable log) is silent until a customer hits it.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn should_rotate(current_size: u64, max_bytes: u64) -> bool {
    current_size >= max_bytes
}

/// One log line: `[<iso8601> pid=<pid> <component>] <message>\n`.
///
/// Every process — the resident daemon, a hotkey-spawned capture child, a tray-spawned
/// child — appends to the SAME file, so the pid and component are what let a reader
/// untangle them. The trailing newline is part of the returned string because the writer
/// issues exactly ONE append per line (Windows append writes are atomic, so interleaving
/// between processes can never split a line).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn log_line(timestamp: &str, pid: u32, component: &str, message: &str) -> String {
    format!("[{timestamp} pid={pid} {component}] {message}\n")
}

/// How this OS `build` should be NAMED in the report — the fact that decides whether a file
/// is a bug report or a control sample.
///
/// DRAGON-408 replaced the old `FORCED-NOT-WIN10` line marker with this. That marker meant
/// "this run was forced on with an env var, so do not read it as Windows 10 evidence"; now
/// that the report is written unconditionally on every Windows machine, nothing is forced
/// and the marker would have been a label whose meaning had quietly changed — worse than no
/// label at all. A reader needs to know what the MACHINE is, not how the file was triggered,
/// so that is what the header states.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn os_classification(build: Option<u32>) -> &'static str {
    match build {
        Some(b) if b >= crate::platform::WIN11_MIN_BUILD => "Windows 11 or newer",
        Some(b) if b >= crate::platform::WIN10_MIN_BUILD => "Windows 10",
        Some(_) => "older than Windows 10",
        None => "UNKNOWN (the OS build number could not be read)",
    }
}

/// The header written ONCE at the top of a fresh report file, before any log line.
///
/// DRAGON-408: this was a copy-paste prompt for an agent session running ON the Windows
/// machine. That is no longer the flow — the person with Windows 10 is a CUSTOMER who
/// emails the file back, and analysis happens elsewhere. A customer opening this file
/// found seven kilobytes of PowerShell build commands and instructions addressed to an
/// AI, which is at best confusing and at worst alarming.
///
/// So it is now addressed to the human holding the file: what it is, that it carries no
/// personal data, and what to do with it. The developer-facing part is kept to the facts
/// that are easy to misread, because those cost real time when they are missed.
///
/// The two corrections are load-bearing and stay. A Windows 11 file is a CONTROL SAMPLE,
/// not a bug report. And `alpha_modes` is a constant of the requested surface kind, not a
/// measurement of the machine — two runs agreeing on it means nothing.
///
/// Ticket identifiers only (DRAGON-408 / DRAGON-406) — never an external repository, issue
/// URL or `owner/repo#N`, per the repo's backlink rule.
#[cfg_attr(not(windows), allow(dead_code))]
pub const BRIEFING: &str = "\
=============================================================================
  Cosmic Capture Kit — diagnostics report
=============================================================================

WHAT THIS FILE IS
  A technical report the app writes about your graphics hardware and how it
  draws its windows. It was created because a display problem on Windows 10
  cannot be reproduced on the developer's machines.

WHAT IT CONTAINS
  Graphics adapter details, Windows version, and window settings. That is all.
  No screenshots. No file names. No window titles. No personal information of
  any kind — the app is built so it cannot write those here.
  You are welcome to read it. It is plain text.

WHAT TO DO WITH IT
  Send this file to the developer. Then you can delete it; the app writes a
  fresh one and deleting it never breaks anything.

WHAT IS BEING INVESTIGATED
  The dark shaded overlay shown when choosing a capture area should be
  see-through. On Windows 10 it comes out solid black. On Windows 11 it is
  correct. Frosted-glass window effects on Windows 10 appear to share the cause.

-----------------------------------------------------------------------------
  Notes for the developer reading this report
-----------------------------------------------------------------------------

  Tickets DRAGON-408 (issue) and DRAGON-406 (this instrument).

  Read first:
    os:        build + classification. `Windows 11 or newer` = CONTROL SAMPLE
               from a working machine. Nothing to fix; read it as a baseline.
    gpu:       adapter + device_type. `device_type=Cpu` means software
               rendering (a VM). Such a run cannot say anything about
               transparency in either direction — discard it and ask for a
               report from real hardware.
    swapchain: `surface_created` per requested kind. A real measurement:
               whether a DirectComposition-backed surface exists on this
               machine at all.
    window:    place_overlay — styles asked for vs. read back off the OS.
    dwm_set:   HRESULTs. A non-S_OK from Windows 10 on a Windows-11-era
               attribute is a live suspect.

  DO NOT read `alpha_modes` as evidence. On DX12 it is not measured — it is a
  fixed list keyed on the surface target kind, with no OS check. An HWND target
  always reports [Opaque]; a DirectComposition target always reports the full
  list. Identical on Windows 10 and 11, on hardware and in a VM. The two
  `requested=` passes below WILL differ, and that difference is the source code
  talking, not this machine.

  The log is append-shared by every process — the app, and in resident mode the
  tray daemon and each capture child — tagged with pid and component. It rotates
  to a single .1 backup.

=============================== END OF HEADER ===============================

";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ex_style_names_the_bits_that_matter() {
        // The overlay's Win11 shape: TOOLWINDOW | LAYERED (DRAGON-280/403). The read-back
        // is only useful if a human can see LAYERED in it at a glance.
        let s = describe_ex_style(0x0008_0080);
        assert!(s.starts_with("0x00080080("), "{s}");
        assert!(s.contains("TOOLWINDOW"), "{s}");
        assert!(s.contains("LAYERED"), "{s}");
        // The Win10 shape after DRAGON-403 withholds LAYERED: same line, no LAYERED term.
        let win10 = describe_ex_style(0x0000_0080);
        assert!(win10.contains("TOOLWINDOW"), "{win10}");
        assert!(!win10.contains("LAYERED"), "{win10}");
    }

    #[test]
    fn ex_style_never_loses_an_unknown_bit() {
        // An OS/WM-set bit we have no name for must still show up — "we asked for X, the OS
        // has Y" is the entire value of the read-back, so a silent drop would defeat it.
        let s = describe_ex_style(0x0000_0080 | 0x0000_0800);
        assert!(s.contains("TOOLWINDOW"), "{s}");
        assert!(s.contains("+0x00000800"), "{s}");
    }

    #[test]
    fn zero_style_reads_as_none() {
        assert_eq!(describe_ex_style(0), "0x00000000(none)");
        assert_eq!(describe_style(0), "0x00000000(none)");
    }

    #[test]
    fn style_names_top_level_box_bits() {
        // WS_VISIBLE | WS_POPUP is the overlay; MINIMIZEBOX/MAXIMIZEBOX (not GROUP/TABSTOP)
        // is the right reading for the top-levels we log.
        let s = describe_style(0x1000_0000 | 0x8000_0000);
        assert!(s.contains("VISIBLE") && s.contains("POPUP"), "{s}");
        assert!(describe_style(0x0002_0000).contains("MINIMIZEBOX"));
        assert!(describe_style(0x0001_0000).contains("MAXIMIZEBOX"));
    }

    #[test]
    fn hresults_name_the_prime_suspect() {
        // The whole reason DwmSetWindowAttribute results stop being discarded.
        assert_eq!(format_hresult(0x8007_0057u32 as i32), "0x80070057 (E_INVALIDARG)");
        assert_eq!(format_hresult(0), "0x00000000 (S_OK)");
        assert_eq!(
            format_hresult(0x8026_3001u32 as i32),
            "0x80263001 (DWM_E_COMPOSITIONDISABLED)"
        );
        // An HRESULT we have no name for still prints its value (never swallowed).
        assert_eq!(format_hresult(0x8004_2222u32 as i32), "0x80042222");
        assert!(hresult_name(0x8004_2222u32 as i32).is_none());
    }

    #[test]
    fn roles_cover_our_titles_and_only_ours() {
        assert_eq!(known_window_role("Cosmic Capture Kit - Settings"), Some("settings"));
        assert_eq!(
            known_window_role("Cosmic Capture Kit - Preview Editor"),
            Some("preview-window")
        );
        assert_eq!(
            known_window_role("Cosmic Capture Kit - Preview Overlay"),
            Some("preview-overlay")
        );
        // komorebi annotates titles; the role must survive that (same rule as title_matches).
        assert_eq!(
            known_window_role("Cosmic Capture Kit - Settings [keepframe]"),
            Some("settings")
        );
        // The editor and the overlay share a prefix — they must not cross-match.
        assert_ne!(
            known_window_role("Cosmic Capture Kit - Preview Overlay"),
            known_window_role("Cosmic Capture Kit - Preview Editor")
        );
    }

    #[test]
    fn an_unknown_title_yields_no_role_so_it_is_never_logged() {
        // THE PII TEST. A capture overlay is titled with the display name, and a user's
        // document titles can look like anything. Neither may resolve to a role, because a
        // role is the only thing the caller is allowed to write.
        assert_eq!(known_window_role("\\\\.\\DISPLAY1"), None);
        assert_eq!(known_window_role("DELL U2720Q"), None);
        assert_eq!(known_window_role("Q4 layoffs - final.docx - Word"), None);
        assert_eq!(known_window_role(""), None);
        // A near-miss on one of our own titles must not match either.
        assert_eq!(known_window_role("Cosmic Capture Kit - SettingsX"), None);
    }

    #[test]
    fn redaction_keeps_flags_and_drops_paths() {
        // argv[0] (the exe path) is dropped outright; flags and short bare words survive.
        let args = ["C:\\Users\\jane\\cck.exe".to_string(), "resident".to_string()];
        assert_eq!(redact_args(&args), "resident");
        let args = [
            "cck.exe".to_string(),
            "--capture".to_string(),
            "region".to_string(),
        ];
        assert_eq!(redact_args(&args), "--capture region");
    }

    #[test]
    fn redaction_replaces_every_path_shaped_value() {
        // The values that could carry a user's filenames — absolute paths, UNC paths, and
        // the `--flag=<path>` form — all collapse to `<value>`.
        let args = [
            "cck.exe".to_string(),
            "--reveal".to_string(),
            "C:\\Users\\jane\\Pictures\\Q4 layoffs.png".to_string(),
            "--out=\\\\server\\share\\secret.mp4".to_string(),
        ];
        let out = redact_args(&args);
        assert_eq!(out, "--reveal <value> --out=<value>");
        assert!(!out.contains("jane") && !out.contains("layoffs") && !out.contains("secret"));
        // A long bare word is redacted too (it could be a filename without a separator).
        let long = ["x".to_string(), "a".repeat(64)];
        assert_eq!(redact_args(&long), "<value>");
    }

    #[test]
    fn redaction_handles_an_argv_of_just_the_exe() {
        assert_eq!(redact_args(&["cck.exe".to_string()]), "(none)");
        assert_eq!(redact_args::<String>(&[]), "(none)");
    }

    #[test]
    fn rotation_fires_at_the_cap_and_not_before() {
        assert!(!should_rotate(0, MAX_LOG_BYTES));
        assert!(!should_rotate(MAX_LOG_BYTES - 1, MAX_LOG_BYTES));
        assert!(should_rotate(MAX_LOG_BYTES, MAX_LOG_BYTES));
        assert!(should_rotate(MAX_LOG_BYTES * 4, MAX_LOG_BYTES));
        // The bound must stay small enough to mail: two files (live + .1 backup) at the cap.
        const { assert!(MAX_LOG_BYTES * 2 <= 8 * 1024 * 1024) };
        // One process can never fill the file on its own, so rotation always gets a turn.
        const { assert!(MAX_SESSION_BYTES < MAX_LOG_BYTES) };
    }

    #[test]
    fn the_os_is_classified_by_what_the_machine_is() {
        // DRAGON-408: this replaced the `FORCED-NOT-WIN10` line marker. It must name the
        // MACHINE, because that is what tells a bug report from a control sample now that
        // the report is written unconditionally on every Windows run.
        assert_eq!(os_classification(Some(19045)), "Windows 10"); // 22H2, the last Win10
        assert_eq!(os_classification(Some(10240)), "Windows 10"); // 1507 RTM, the floor
        assert_eq!(os_classification(Some(21999)), "Windows 10"); // just under Win11
        assert_eq!(os_classification(Some(22000)), "Windows 11 or newer"); // 21H2
        assert_eq!(os_classification(Some(26100)), "Windows 11 or newer"); // 24H2
        assert_eq!(os_classification(Some(9600)), "older than Windows 10"); // 8.1
        // An unreadable build must SAY so rather than defaulting to either OS — a silently
        // mislabelled control sample is exactly the failure this line exists to prevent.
        assert!(os_classification(None).starts_with("UNKNOWN"));
    }

    #[test]
    fn the_header_addresses_the_customer_and_carries_no_backlinks() {
        // DRAGON-408: the reader is a CUSTOMER who emails this file back, not an agent
        // session on the machine. The three questions they actually have must be answered
        // before any developer material.
        assert!(BRIEFING.contains("WHAT THIS FILE IS"), "no plain-language opener");
        assert!(BRIEFING.contains("WHAT IT CONTAINS"), "privacy answer missing");
        assert!(BRIEFING.contains("WHAT TO DO WITH IT"), "no instruction to the holder");
        assert!(BRIEFING.starts_with("=="), "must open on a rule, not mid-prose");

        // Privacy is the claim a stranger is most entitled to check, so it is stated
        // explicitly rather than implied by omission.
        assert!(BRIEFING.contains("No screenshots"), "privacy claim missing");
        assert!(BRIEFING.contains("No personal information"), "privacy claim missing");

        // The developer half must never resurrect the agent-session framing: no build
        // commands, no environment variables, nothing addressed to an AI. A customer
        // reading PowerShell in a diagnostics file is the bug this test pins.
        assert!(!BRIEFING.contains("CARGO_TARGET_DIR"), "build commands are back");
        assert!(!BRIEFING.contains("cargo "), "build commands are back");
        assert!(!BRIEFING.contains("PowerShell"), "build commands are back");
        assert!(!BRIEFING.contains("COPY FROM HERE"), "paste fences are back");
        assert!(!BRIEFING.contains("YOUR TASK"), "agent framing is back");

        // Ticket identifiers so the report can be tied to context; NO external repo
        // backlinks (repo rule: never `owner/repo#N` or an issue URL in shipped text).
        assert!(BRIEFING.contains("DRAGON-408") && BRIEFING.contains("DRAGON-406"));
        assert!(!BRIEFING.contains("github.com"), "backlink rule");
        assert!(!BRIEFING.contains("linear.app"), "backlink rule");
        assert!(!BRIEFING.contains("http"), "backlink rule");

        // The two readings that have already cost real time, kept for whoever analyses it.
        assert!(BRIEFING.contains("CONTROL SAMPLE"), "win11 framing missing");
        assert!(
            BRIEFING.contains("alpha_modes"),
            "the constant-not-a-measurement trap must stay called out"
        );

        // Short enough that a customer reads it instead of bouncing off it.
        assert!(BRIEFING.lines().count() < 90, "header has grown into a wall of text");
    }

    #[test]
    fn a_log_line_is_one_append() {
        let line = log_line("2026-07-29T10:11:12.345Z", 4242, "app", "hello");
        assert_eq!(line, "[2026-07-29T10:11:12.345Z pid=4242 app] hello\n");
        // Exactly one newline, at the end: the writer issues one atomic append per line, so
        // concurrent processes interleave BETWEEN lines and never split one.
        assert_eq!(line.matches('\n').count(), 1);
        assert!(line.ends_with('\n'));
    }
}
