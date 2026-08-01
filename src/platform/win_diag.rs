//! Portable Windows chrome helpers: extended-style bit names, `HRESULT` formatting and the
//! Windows 10-vs-11 build classification, with no Win32 in sight so all of it is unit-tested
//! on Linux.
//!
//! # What this used to be
//!
//! Introduced by DRAGON-406 as the PURE half of a temporary Windows-10 diagnostics report —
//! formatting, redaction, a customer briefing and a rotation policy. DRAGON-407 deleted that
//! instrument, and with it most of this file: the briefing, the rotation policy, the log-line
//! formatter, the argv redactor, the window-role classifier and the full-style describer all
//! went, because the report was their only consumer.
//!
//! # What survived, and why
//!
//! Three things earn their keep independently of that investigation, and DRAGON-407 kept them
//! deliberately (see that ticket's KEEP list):
//!
//! * [`format_hresult`] — used by `wm/window.rs`'s `dwm_set_attr`, which now WARNS on a failed
//!   `DwmSetWindowAttribute` instead of feeding a file. A silent `E_INVALIDARG` is exactly how
//!   DRAGON-403 stayed hidden, so naming the code is the durable lesson.
//! * [`describe_ex_style`] — used by `wm/window.rs`'s `set_ex_style_checked`, which warns when
//!   the style the OS ends up with is not the style we asked for.
//! * [`os_classification`] — used by the DRAGON-426 leak probe's report header, where "is this
//!   a Windows 10 or a Windows 11 machine" is the question the probe exists to settle.
//!
//! Everything here is dead code off Windows (hence the per-item
//! `cfg_attr(not(windows), allow(dead_code))`) — the TESTS still run everywhere, which is the
//! point of it living in the shared tree rather than the stripped Windows plugin.


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

/// The `HRESULT`s this investigation actually turns on, by name.
///
/// `E_INVALIDARG` is the prime suspect: a `DwmSetWindowAttribute` for an attribute the OS
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

/// Format an HRESULT as `0x80070057 (E_INVALIDARG)`, or bare hex when unrecognised. Its one
/// caller is `dwm_set_attr`'s failure `warn!` — the durable half of DRAGON-406 that
/// DRAGON-407 kept when it deleted the report this also used to feed.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn format_hresult(hr: i32) -> String {
    match hresult_name(hr) {
        Some(name) => format!("{:#010x} ({name})", hr as u32),
        None => format!("{:#010x}", hr as u32),
    }
}

/// How this OS `build` should be NAMED to a human — the fact that decides whether a report
/// from a machine is evidence about Windows 10 or a control sample from Windows 11.
///
/// Introduced by DRAGON-408 for the diagnostics report's header. DRAGON-407 deleted that
/// report; this survives as the header of the DRAGON-426 leak probe, where "which Windows is
/// this" is precisely the question being settled. An unreadable build must SAY so rather than
/// defaulting to either OS — a silently mislabelled sample is worse than no label.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn os_classification(build: Option<u32>) -> &'static str {
    match build {
        Some(b) if b >= crate::platform::WIN11_MIN_BUILD => "Windows 11 or newer",
        Some(b) if b >= crate::platform::WIN10_MIN_BUILD => "Windows 10",
        Some(_) => "older than Windows 10",
        None => "UNKNOWN (the OS build number could not be read)",
    }
}

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
    fn the_os_is_classified_by_what_the_machine_is() {
        // It must name the MACHINE, because that is what tells a bug report from a control
        // sample. Introduced by DRAGON-408 for the diagnostics report's header; it now heads
        // the DRAGON-426 leak probe, where "which Windows is this" IS the finding.
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

}
