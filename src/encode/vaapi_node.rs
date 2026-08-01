//! Which VAAPI render node a captured frame can actually be encoded on (DRAGON-425).
//!
//! ## The bug this closes
//!
//! `gpu::default_vaapi_node` picks the first `/dev/dri/renderD*` whose sysfs driver is
//! `amdgpu`/`i915`/`xe`, alphabetically, with no idea what the session is rendering on. On a
//! single-GPU box that is always right. On a MULTI-GPU box it is a guess, and the zero-copy
//! PipeWire path baked it in before a single frame existed: a laptop rendering on NVIDIA
//! handed its dmabufs to an AMD node, the import failed, and zero-copy silently fell back to
//! CPU readback for the whole session.
//!
//! The frame itself carries the answer. A DRM format modifier's top byte is its vendor, so
//! the FIRST frame says which GPU produced it — and the encoder is created lazily on that
//! first frame, so the node choice can still be corrected with no new plumbing.
//!
//! The screencopy zero-copy path never had this problem: the compositor hands it a
//! `dmabuf_device` and it resolves the render node from that. Only the PipeWire path guessed.
//!
//! ## Why this module is not feature-gated
//!
//! The decisions here are pure functions over plain data, and the `zero-copy` feature
//! (gbm + ffmpeg-next) only builds on Linux with DRM. Gating the policy would make it
//! invisible to the macOS and Windows suites — the very machines most of this project's work
//! happens on. `plan.rs` set the precedent: policy ungated and tested everywhere, the
//! hardware glue gated. [`list_render_nodes`] is the one exception that touches the
//! filesystem, and its body is `cfg(target_os = "linux")` with an empty arm elsewhere; it
//! lives here so the FOUR `/dev/dri` walks this crate used to contain became one.
//!
//! ## Two belts, and why both
//!
//! The decision runs TWICE per session, against the same table:
//!
//! 1. A **pre-flight**, before anything is paid for. The compositor's
//!    `zwp_linux_dmabuf_v1` default-feedback `main_device` names the GPU this session
//!    renders on, and it is available with no frame and no capture — see
//!    [`resolve_vaapi_node_for_session`]. A decline here costs nothing: no audio pre-flight,
//!    no pump, no media-0 anchor.
//! 2. A **confirmation** on the first frame's modifier, via
//!    [`resolve_vaapi_node_for_vendor`]. This is the belt that catches a compositor whose
//!    frames disagree with its own feedback, and it still runs before `Encoder::new`.
//!
//! The pre-flight is the one that matters for the reported failure. On a box where the
//! compositor DECLINES cross-vendor dmabuf, no frame is ever delivered at all — the modifier
//! is never fixated, the stream handshake never completes, and a first-frame-only check
//! would never run.

// Every CONSUMER of this module is behind the `zero-copy` feature (the PipeWire recorder and
// `--dmabuf-test`), so without it the whole module is dead code — but it is still compiled
// and TESTED everywhere on purpose, which is the entire point of keeping the policy ungated.
// The same arrangement `crate::startup_guard` uses for its macOS-only decisions.
#![cfg_attr(not(feature = "zero-copy"), allow(dead_code))]

use std::path::{Path, PathBuf};

/// The sysfs driver names VAAPI can encode on. NVIDIA is absent on purpose: its proprietary
/// driver exposes no VAAPI encode node, so an NVIDIA-rendered session has no VAAPI answer at
/// all (see [`NodeDecision::Decline`]) — NVENC is the path it would want, which is a separate
/// question this module deliberately does not try to answer.
pub const VAAPI_DRIVERS: [&str; 3] = ["amdgpu", "i915", "xe"];

/// `DRM_FORMAT_MOD_INVALID` — the "no modifier agreed yet" sentinel. It names no vendor, so a
/// frame carrying it tells us nothing and must not move the node choice.
pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// The GPU vendor that produced a dmabuf, read from its DRM format modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmabufVendor {
    Intel,
    Amd,
    Nvidia,
}

impl DmabufVendor {
    /// The name used in logs and in the decline reason the user may end up reading.
    pub fn name(self) -> &'static str {
        match self {
            DmabufVendor::Intel => "Intel",
            DmabufVendor::Amd => "AMD",
            DmabufVendor::Nvidia => "NVIDIA",
        }
    }
}

/// A modifier's vendor byte (bits 56..63 of the DRM format modifier).
fn vendor_byte(modifier: u64) -> u8 {
    ((modifier >> 56) & 0xff) as u8
}

/// The GPU vendor a dmabuf came from, or `None` when the modifier names none.
///
/// `None` covers three genuinely different cases that all mean the same thing here — we
/// cannot tell, so do not act:
///
/// * [`DRM_FORMAT_MOD_INVALID`], the unfixated sentinel.
/// * Vendor byte `0`, which is `DRM_FORMAT_MOD_NONE` / linear — a vendorless layout ANY GPU
///   can produce, so it identifies nobody.
/// * A vendor byte we do not have a case for.
pub fn vendor_from_modifier(modifier: u64) -> Option<DmabufVendor> {
    if modifier == DRM_FORMAT_MOD_INVALID {
        return None;
    }
    match vendor_byte(modifier) {
        1 => Some(DmabufVendor::Intel),
        2 => Some(DmabufVendor::Amd),
        3 => Some(DmabufVendor::Nvidia),
        _ => None,
    }
}

/// The human label for a modifier's vendor, as `--dmabuf-test` prints it.
///
/// Shares [`vendor_from_modifier`]'s table so the diagnostic and the recorder can never
/// disagree about who made a frame — the decode used to live only in `cli/diagnostics.rs`,
/// with a comment saying the source GPU ought to pick the encoder, and nothing wired it up.
pub fn modifier_vendor_label(modifier: u64) -> String {
    if modifier == DRM_FORMAT_MOD_INVALID {
        return "INVALID/unfixated".to_string();
    }
    match vendor_from_modifier(modifier) {
        Some(v) => v.name().to_string(),
        None if vendor_byte(modifier) == 0 => "none/linear".to_string(),
        None => format!("vendor 0x{:02x}", vendor_byte(modifier)),
    }
}

/// Whether a sysfs DRM driver name belongs to `vendor`.
///
/// `nouveau` counts as NVIDIA: it is a different driver for the same silicon, and for the
/// question this answers — "did this GPU produce that buffer" — the silicon is what matters.
pub fn driver_matches_vendor(driver: &str, vendor: DmabufVendor) -> bool {
    match vendor {
        DmabufVendor::Amd => driver == "amdgpu",
        DmabufVendor::Intel => matches!(driver, "i915" | "xe"),
        DmabufVendor::Nvidia => matches!(driver, "nvidia" | "nouveau"),
    }
}

/// The vendor behind a sysfs DRM driver name — the inverse of [`driver_matches_vendor`], for
/// the PRE-FLIGHT, which learns the session's GPU as a driver name rather than a modifier.
///
/// `None` for a driver we do not recognise, which the caller treats exactly like an unknown
/// modifier: change nothing.
pub fn vendor_from_driver(driver: &str) -> Option<DmabufVendor> {
    [DmabufVendor::Intel, DmabufVendor::Amd, DmabufVendor::Nvidia]
        .into_iter()
        .find(|v| driver_matches_vendor(driver, *v))
}

/// The libva driver name to load for a DRM node's kernel driver: `amdgpu` → `radeonsi`,
/// Intel `i915`/`xe` → `iHD`. `None` for anything VAAPI cannot encode on.
///
/// THE one copy of this mapping. It is not cosmetic: libva often cannot auto-pick a driver
/// when an nvidia node is also present — precisely the multi-GPU boxes DRAGON-425 fires on —
/// so an encoder opened without naming it can fail on the very machine the node switch was
/// made for. `plan.rs` names it for its ffmpeg CHILD through `EncodePlan::env`; the zero-copy
/// path has no child (libva is loaded in-process), so it sets the process variable instead.
pub fn libva_driver_for(driver: &str) -> Option<&'static str> {
    match driver {
        "amdgpu" => Some("radeonsi"),
        "i915" | "xe" => Some("iHD"),
        _ => None,
    }
}

/// Every `/dev/dri/renderD*` node paired with its sysfs driver name, sorted by node name.
///
/// THE one render-node listing in this crate. There were four separate `/dev/dri` walks
/// before DRAGON-425 (`gpu::default_vaapi_node`, `plan::vaapi_device`, and two in the
/// zero-copy paths); every one of them re-derived "which nodes exist and what drives them",
/// and they could disagree. A node whose driver link cannot be read is dropped — unreadable
/// means unusable, which is how each of those walks already treated it.
///
/// The body is Linux-only; everywhere else there is no DRM and the answer is empty. It is
/// declared here rather than behind the `zero-copy` feature because `plan::vaapi_device` (the
/// ordinary, non-zero-copy VAAPI encoder path) shares it.
pub fn list_render_nodes() -> Vec<(PathBuf, String)> {
    #[cfg(target_os = "linux")]
    {
        let Ok(dir) = std::fs::read_dir("/dev/dri") else {
            return Vec::new();
        };
        // Sort the NAMES, which is what the walks this replaces did, so node ordering — and
        // therefore which node is "the default" — is byte-for-byte unchanged.
        let mut names: Vec<String> = dir
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.starts_with("renderD").then_some(n)
            })
            .collect();
        names.sort();
        names
            .into_iter()
            .filter_map(|n| {
                let driver = std::fs::read_link(format!("/sys/class/drm/{n}/device/driver"))
                    .ok()
                    .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))?;
                Some((PathBuf::from(format!("/dev/dri/{n}")), driver))
            })
            .collect()
    }
    #[cfg(not(target_os = "linux"))]
    Vec::new()
}

/// The node `gpu::default_vaapi_node` picks: the first VAAPI-capable one in listing order.
///
/// Lives here, ungated, so the rule itself is testable and so the gated helper cannot drift
/// from it — `default_vaapi_node` is now this function over `vaapi_nodes_with_drivers()`, and
/// its behaviour is unchanged (the listing is sorted, and it kept the same driver set).
pub fn default_vaapi_node_from(nodes: &[(PathBuf, String)]) -> Option<PathBuf> {
    nodes
        .iter()
        .find(|(_, driver)| VAAPI_DRIVERS.contains(&driver.as_str()))
        .map(|(path, _)| path.clone())
}

/// What to do with the node the encoder was about to be built on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeDecision {
    /// Use the node already chosen. Either the frame named no vendor (so there is nothing to
    /// correct and today's behaviour stands), or the default is already on the right GPU.
    KeepDefault,
    /// The default is on the wrong GPU and this listed node is on the right one.
    SwitchTo(PathBuf),
    /// The session renders on a GPU no VAAPI node can import from. Zero-copy cannot work at
    /// all here, so decline immediately and let the CPU readback path have it.
    ///
    /// TWO strings, because they have two different readers. `user` can reach a failure
    /// dialog verbatim (a fallback reason rides `Failure::RecordingFailed`), so it says what
    /// happened in the user's terms and names no hardware. `detail` is for the log and for
    /// us: which GPU, which nodes, which node we would have used.
    Decline { user: String, detail: String },
}

impl NodeDecision {
    /// The reason a caller should carry into its own fallback outcome — the USER-facing half,
    /// never the technical one.
    // Its only caller today is the test that pins the user/detail split: both PRODUCTION
    // call sites (`record::zero_copy`'s pre-flight and its first-frame confirmation) sit in
    // a `match` over the whole enum, where destructuring `Decline { user, .. }` hands them
    // an owned `String` and this `Option<&str>` would not fit. Kept rather than deleted
    // because the split it reads is the point of the two fields.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn user_reason(&self) -> Option<&str> {
        match self {
            NodeDecision::Decline { user, .. } => Some(user),
            _ => None,
        }
    }
}

/// The user-facing half of every decline. One sentence, no vendor names, no device paths:
/// it can land verbatim in a failure dialog, where "no VAAPI node can import its buffers"
/// would tell the reader nothing they can act on. The specifics go to the log.
const DECLINE_USER_REASON: &str =
    "this computer's graphics setup can't use the fast recording path";

/// Choose the VAAPI node for a session whose first frame came from `vendor`.
///
/// `nodes` is every render node with its sysfs driver, in listing order (see
/// `gpu::vaapi_nodes_with_drivers`); the default is derived from it by
/// [`default_vaapi_node_from`], so a caller cannot pass a default that is not really the one.
///
/// The rules, and why each is what it is:
///
/// | frame vendor | node situation                    | decision      |
/// |--------------|-----------------------------------|---------------|
/// | unknown      | anything                          | `KeepDefault` |
/// | known        | default is already on that GPU    | `KeepDefault` |
/// | known        | another listed node is on that GPU| `SwitchTo`    |
/// | known        | no VAAPI node on that GPU         | `Decline`     |
///
/// An unknown vendor keeps the default deliberately: it is exactly today's behaviour, and
/// single-GPU boxes — where the guess is always right — must not regress because a
/// compositor handed us a linear or unfixated modifier.
///
/// A `Decline` is not a failure of this recording. Zero-copy declining is a normal outcome the
/// CPU readback path is built to cover, and declining EARLY (before any encoder or muxer
/// exists) is the whole point: the session used to discover this by failing an import, after
/// paying for the attempt.
pub fn resolve_vaapi_node_for_vendor(
    nodes: &[(PathBuf, String)],
    vendor: Option<DmabufVendor>,
) -> NodeDecision {
    let Some(vendor) = vendor else {
        return NodeDecision::KeepDefault;
    };
    // A candidate has to satisfy BOTH halves: on the frame's GPU, and one VAAPI can encode
    // on. NVIDIA is the case that needs both — `nvidia` matches the vendor and can never be
    // a VAAPI node, so checking only the vendor would hand the encoder a node it cannot use.
    let matching = nodes.iter().find(|(_, driver)| {
        VAAPI_DRIVERS.contains(&driver.as_str()) && driver_matches_vendor(driver, vendor)
    });
    let Some((matched, _)) = matching else {
        // Drivers DEDUPED and listed plainly: a box with three amdgpu nodes should read
        // "amdgpu, nvidia", not "amdgpu, amdgpu, amdgpu, nvidia", and the line carries no
        // nested parentheses so it stays readable inside a longer log message.
        let mut found: Vec<&str> = Vec::new();
        for (_, d) in nodes {
            if !found.contains(&d.as_str()) {
                found.push(d.as_str());
            }
        }
        let found = if found.is_empty() { "none".to_string() } else { found.join(", ") };
        return NodeDecision::Decline {
            user: DECLINE_USER_REASON.to_string(),
            detail: format!(
                "the session renders on {}; no VAAPI node can import its buffers - \
                 render node drivers present: {found}",
                vendor.name()
            ),
        };
    };
    match default_vaapi_node_from(nodes) {
        // Already on the right GPU — the single-GPU case, and the multi-GPU case where the
        // alphabetical guess happened to be right.
        Some(default) if default == *matched => NodeDecision::KeepDefault,
        _ => NodeDecision::SwitchTo(matched.clone()),
    }
}

/// The PRE-FLIGHT decision: the same table, keyed on the session's renderer DRIVER instead of
/// a frame's modifier (DRAGON-425 fix round).
///
/// This is the belt that actually catches the reported failure. The first-frame check can
/// only run once a frame arrives, and on a box where the compositor DECLINES cross-vendor
/// dmabuf no frame ever arrives — the modifier is never fixated and the stream handshake
/// never completes, so the session simply stalls and then falls back, having already paid for
/// the audio pre-flight, the pump and the media-0 anchor.
///
/// `session_driver` is the kernel driver behind the compositor's `zwp_linux_dmabuf_v1`
/// default-feedback `main_device` — the protocol's own answer to "which GPU is this session
/// on", available before any capture exists. `None` (no Wayland, a compositor too old for
/// feedback, an unreadable sysfs entry, a driver we do not know) means we learned nothing, and
/// the answer is [`NodeDecision::KeepDefault`] — never a guess.
pub fn resolve_vaapi_node_for_session(
    nodes: &[(PathBuf, String)],
    session_driver: Option<&str>,
) -> NodeDecision {
    resolve_vaapi_node_for_vendor(nodes, session_driver.and_then(vendor_from_driver))
}

/// A one-line record of what was decided, for the session log. Written so a support log makes
/// the multi-GPU story readable without the reader knowing any of this module.
///
/// `node` is the node the encoder WOULD have used — named on every branch including the
/// decline, because "we declined" without saying what we were about to open leaves the next
/// reader with half the story.
pub fn decision_log_line(node: &Path, vendor: Option<DmabufVendor>, d: &NodeDecision) -> String {
    let from = vendor.map(|v| v.name()).unwrap_or("an unidentified GPU");
    match d {
        NodeDecision::KeepDefault => {
            format!("zero-copy: encoding {from} frames on {}", node.display())
        }
        NodeDecision::SwitchTo(to) => format!(
            "zero-copy: switching the encoder from {} to {} because the capture renders on {from}",
            node.display(),
            to.display()
        ),
        NodeDecision::Decline { detail, .. } => format!(
            "zero-copy: declined before encoding, {detail}; would have used {}",
            node.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a modifier with `vendor` in its top byte and an arbitrary payload below, the
    /// shape a real compositor hands over.
    fn modifier(vendor: u8) -> u64 {
        ((vendor as u64) << 56) | 0x0000_0000_0000_0004
    }

    fn nodes(list: &[(&str, &str)]) -> Vec<(PathBuf, String)> {
        list.iter().map(|(p, d)| (PathBuf::from(p), d.to_string())).collect()
    }

    // ── Decoding the frame's vendor ───────────────────────────────────────────

    #[test]
    fn the_vendor_byte_names_the_gpu() {
        assert_eq!(vendor_from_modifier(modifier(1)), Some(DmabufVendor::Intel));
        assert_eq!(vendor_from_modifier(modifier(2)), Some(DmabufVendor::Amd));
        assert_eq!(vendor_from_modifier(modifier(3)), Some(DmabufVendor::Nvidia));
    }

    #[test]
    fn a_modifier_that_names_nobody_is_unknown() {
        // The unfixated sentinel: no format agreed yet.
        assert_eq!(vendor_from_modifier(DRM_FORMAT_MOD_INVALID), None);
        // Linear (vendor byte 0) is a layout ANY GPU can produce, so it identifies nobody.
        // Reading it as a vendor would be worse than not reading it at all.
        assert_eq!(vendor_from_modifier(modifier(0)), None);
        assert_eq!(vendor_from_modifier(0), None);
        // A vendor we have no case for stays unknown rather than guessing.
        assert_eq!(vendor_from_modifier(modifier(9)), None);
    }

    #[test]
    fn the_diagnostic_label_matches_what_dmabuf_test_has_always_printed() {
        // Pinned because `--dmabuf-test`'s output is what a customer pastes into a report;
        // this is now the same table the recorder decides on, so it cannot drift.
        assert_eq!(modifier_vendor_label(DRM_FORMAT_MOD_INVALID), "INVALID/unfixated");
        assert_eq!(modifier_vendor_label(modifier(0)), "none/linear");
        assert_eq!(modifier_vendor_label(modifier(1)), "Intel");
        assert_eq!(modifier_vendor_label(modifier(2)), "AMD");
        assert_eq!(modifier_vendor_label(modifier(3)), "NVIDIA");
        assert_eq!(modifier_vendor_label(modifier(0x2a)), "vendor 0x2a");
    }

    // ── Matching a driver to a vendor ─────────────────────────────────────────

    #[test]
    fn drivers_map_to_their_silicon() {
        assert!(driver_matches_vendor("amdgpu", DmabufVendor::Amd));
        assert!(driver_matches_vendor("i915", DmabufVendor::Intel));
        // Intel's newer driver for Xe silicon is still Intel.
        assert!(driver_matches_vendor("xe", DmabufVendor::Intel));
        // Same silicon, either driver.
        assert!(driver_matches_vendor("nvidia", DmabufVendor::Nvidia));
        assert!(driver_matches_vendor("nouveau", DmabufVendor::Nvidia));
        // And no crossing over.
        assert!(!driver_matches_vendor("amdgpu", DmabufVendor::Intel));
        assert!(!driver_matches_vendor("i915", DmabufVendor::Amd));
        assert!(!driver_matches_vendor("nvidia", DmabufVendor::Amd));
        assert!(!driver_matches_vendor("something-else", DmabufVendor::Amd));
    }

    // ── The node decision ─────────────────────────────────────────────────────

    #[test]
    fn the_owners_field_case_declines_instead_of_guessing() {
        // THE reported bug, in its exact shape: the session renders on NVIDIA, and the only
        // VAAPI-capable node on the box is AMD. Nothing can import those buffers, so the
        // honest answer is to decline zero-copy at the first frame and let the CPU readback
        // path take the recording — not to hand an AMD node NVIDIA dmabufs and fail the
        // import after paying for the attempt.
        let listing = nodes(&[("/dev/dri/renderD128", "amdgpu"), ("/dev/dri/renderD129", "nvidia")]);
        let d = resolve_vaapi_node_for_vendor(&listing, Some(DmabufVendor::Nvidia));
        let NodeDecision::Decline { user, detail } = &d else {
            panic!("expected a decline, got {d:?}");
        };
        // The DETAIL has to name both sides, or a support log leaves the reader guessing.
        assert!(detail.contains("NVIDIA"), "{detail}");
        assert!(detail.contains("no VAAPI node can import"), "{detail}");
        assert!(detail.contains("amdgpu"), "{detail}");
        // The USER half can land verbatim in a failure dialog, so it must name no hardware
        // and no device path — "nvidia" or "/dev/dri" there is a support ticket, not a
        // message.
        assert!(!user.contains("NVIDIA") && !user.to_lowercase().contains("nvidia"), "{user}");
        assert!(!user.contains("VAAPI") && !user.contains("/dev/"), "{user}");
        assert!(user.contains("fast recording path"), "{user}");
        assert_eq!(d.user_reason(), Some(user.as_str()));
    }

    /// The SAME box, decided at PRE-FLIGHT from the compositor's renderer driver — before a
    /// frame exists. This is the belt that actually fires on the reported hardware: a
    /// compositor that declines cross-vendor dmabuf never delivers a frame at all, so the
    /// modifier check above would never run and the session would pay for its audio
    /// pre-flight, pump and media-0 anchor before stalling out.
    #[test]
    fn the_owners_field_case_is_caught_at_preflight_without_a_frame() {
        let listing = nodes(&[("/dev/dri/renderD128", "amdgpu"), ("/dev/dri/renderD129", "nvidia")]);
        assert!(matches!(
            resolve_vaapi_node_for_session(&listing, Some("nvidia")),
            NodeDecision::Decline { .. }
        ));
        // nouveau is the same silicon and the same answer.
        assert!(matches!(
            resolve_vaapi_node_for_session(&listing, Some("nouveau")),
            NodeDecision::Decline { .. }
        ));
    }

    #[test]
    fn the_preflight_switches_and_keeps_exactly_like_the_frame_check() {
        // Intel session, AMD default → switch, from a driver name rather than a modifier.
        let listing = nodes(&[("/dev/dri/renderD128", "amdgpu"), ("/dev/dri/renderD129", "i915")]);
        assert_eq!(
            resolve_vaapi_node_for_session(&listing, Some("i915")),
            NodeDecision::SwitchTo(PathBuf::from("/dev/dri/renderD129"))
        );
        // Right GPU already → nothing moves.
        assert_eq!(
            resolve_vaapi_node_for_session(&listing, Some("amdgpu")),
            NodeDecision::KeepDefault
        );
    }

    #[test]
    fn an_unlearnable_session_driver_changes_nothing() {
        // No Wayland, a compositor too old for dmabuf feedback, an unreadable sysfs entry, or
        // a driver we do not have a case for. Every one of them means we learned nothing, and
        // the no-regression rule is the same as for an unknown modifier: keep the default.
        let listing = nodes(&[("/dev/dri/renderD128", "amdgpu"), ("/dev/dri/renderD129", "nvidia")]);
        assert_eq!(resolve_vaapi_node_for_session(&listing, None), NodeDecision::KeepDefault);
        assert_eq!(
            resolve_vaapi_node_for_session(&listing, Some("some-future-driver")),
            NodeDecision::KeepDefault
        );
    }

    #[test]
    fn the_decline_detail_dedupes_drivers_and_nests_no_parens() {
        // Three AMD nodes and one NVIDIA one: the reader wants "amdgpu, nvidia", not the same
        // word four times. And the line is embedded in a longer log message, so it must not
        // open a paren of its own.
        let listing = nodes(&[
            ("/dev/dri/renderD128", "amdgpu"),
            ("/dev/dri/renderD129", "amdgpu"),
            ("/dev/dri/renderD130", "amdgpu"),
            ("/dev/dri/renderD131", "nvidia"),
        ]);
        let d = resolve_vaapi_node_for_session(&listing, Some("nvidia"));
        let NodeDecision::Decline { detail, .. } = &d else {
            panic!("expected a decline, got {d:?}");
        };
        assert!(detail.ends_with("amdgpu, nvidia"), "{detail}");
        assert!(!detail.contains('('), "{detail}");
    }

    // ── The driver ↔ vendor ↔ libva mappings ──────────────────────────────────

    #[test]
    fn a_driver_name_resolves_to_its_vendor() {
        assert_eq!(vendor_from_driver("amdgpu"), Some(DmabufVendor::Amd));
        assert_eq!(vendor_from_driver("i915"), Some(DmabufVendor::Intel));
        assert_eq!(vendor_from_driver("xe"), Some(DmabufVendor::Intel));
        assert_eq!(vendor_from_driver("nvidia"), Some(DmabufVendor::Nvidia));
        assert_eq!(vendor_from_driver("nouveau"), Some(DmabufVendor::Nvidia));
        assert_eq!(vendor_from_driver("virtio_gpu"), None);
        assert_eq!(vendor_from_driver(""), None);
        // And it agrees with the predicate it inverts, for every driver either knows.
        for d in ["amdgpu", "i915", "xe", "nvidia", "nouveau"] {
            let v = vendor_from_driver(d).unwrap();
            assert!(driver_matches_vendor(d, v), "{d}");
        }
    }

    /// The libva driver a node needs. Not cosmetic: libva often cannot auto-pick when an
    /// nvidia node is present, which is exactly the multi-GPU box a switch happens on.
    #[test]
    fn every_vaapi_driver_names_its_libva_driver() {
        assert_eq!(libva_driver_for("amdgpu"), Some("radeonsi"));
        assert_eq!(libva_driver_for("i915"), Some("iHD"));
        assert_eq!(libva_driver_for("xe"), Some("iHD"));
        // NVIDIA has no VAAPI encode driver to name, which is why it declines instead.
        assert_eq!(libva_driver_for("nvidia"), None);
        assert_eq!(libva_driver_for("nouveau"), None);
        assert_eq!(libva_driver_for("virtio_gpu"), None);
        // Every driver VAAPI can encode on must have one, or a switch could land on a node
        // with no driver name to set.
        for d in VAAPI_DRIVERS {
            assert!(libva_driver_for(d).is_some(), "{d} has no libva driver");
        }
    }

    #[test]
    fn an_nvidia_node_is_never_offered_to_the_vaapi_encoder() {
        // Even with the frame's own GPU present in the listing: `nvidia` matches the vendor
        // but is not a VAAPI driver, so a vendor-only match would hand the encoder a node it
        // cannot open. Both halves are required.
        let listing = nodes(&[("/dev/dri/renderD128", "nvidia")]);
        assert!(matches!(
            resolve_vaapi_node_for_vendor(&listing, Some(DmabufVendor::Nvidia)),
            NodeDecision::Decline { .. }
        ));
    }

    #[test]
    fn a_cross_vendor_box_switches_to_the_gpu_that_made_the_frame() {
        // Intel iGPU + AMD dGPU. The alphabetical default is the AMD node, but the session
        // renders on Intel, so the encoder moves to the Intel node.
        let listing = nodes(&[("/dev/dri/renderD128", "amdgpu"), ("/dev/dri/renderD129", "i915")]);
        assert_eq!(
            resolve_vaapi_node_for_vendor(&listing, Some(DmabufVendor::Intel)),
            NodeDecision::SwitchTo(PathBuf::from("/dev/dri/renderD129"))
        );
        // And the mirror image, so this is a real match and not a bias toward one vendor.
        let listing = nodes(&[("/dev/dri/renderD128", "i915"), ("/dev/dri/renderD129", "amdgpu")]);
        assert_eq!(
            resolve_vaapi_node_for_vendor(&listing, Some(DmabufVendor::Amd)),
            NodeDecision::SwitchTo(PathBuf::from("/dev/dri/renderD129"))
        );
    }

    #[test]
    fn the_default_is_kept_when_it_is_already_the_right_gpu() {
        // The single-GPU case — every box that works today — must not move.
        let listing = nodes(&[("/dev/dri/renderD128", "amdgpu")]);
        assert_eq!(
            resolve_vaapi_node_for_vendor(&listing, Some(DmabufVendor::Amd)),
            NodeDecision::KeepDefault
        );
        // And the multi-GPU case where the alphabetical guess happened to be right: no
        // pointless switch to an identical node.
        let listing = nodes(&[("/dev/dri/renderD128", "i915"), ("/dev/dri/renderD129", "amdgpu")]);
        assert_eq!(
            resolve_vaapi_node_for_vendor(&listing, Some(DmabufVendor::Intel)),
            NodeDecision::KeepDefault
        );
    }

    #[test]
    fn an_unknown_vendor_never_moves_anything() {
        // The no-regression rule. A linear or unfixated modifier tells us nothing, and every
        // box that works today works with the alphabetical guess — so silence must leave it
        // exactly as it is, on any listing.
        for listing in [
            nodes(&[]),
            nodes(&[("/dev/dri/renderD128", "amdgpu")]),
            nodes(&[("/dev/dri/renderD128", "nvidia"), ("/dev/dri/renderD129", "i915")]),
        ] {
            assert_eq!(
                resolve_vaapi_node_for_vendor(&listing, None),
                NodeDecision::KeepDefault,
                "{listing:?}"
            );
        }
    }

    #[test]
    fn the_first_matching_node_wins_in_listing_order() {
        // Two nodes on the same GPU (a real shape: a card can expose more than one render
        // node, and a box can have two cards of one vendor). The listing is sorted, so
        // "first" is stable across runs — a session must not pick a different node each time.
        let listing = nodes(&[
            ("/dev/dri/renderD128", "nvidia"),
            ("/dev/dri/renderD129", "amdgpu"),
            ("/dev/dri/renderD130", "amdgpu"),
        ]);
        assert_eq!(
            resolve_vaapi_node_for_vendor(&listing, Some(DmabufVendor::Amd)),
            NodeDecision::KeepDefault,
            "renderD129 is both the default and the match"
        );
        // With the NVIDIA node between two Intel ones, the earlier Intel node is chosen.
        let listing = nodes(&[
            ("/dev/dri/renderD128", "amdgpu"),
            ("/dev/dri/renderD129", "i915"),
            ("/dev/dri/renderD130", "xe"),
        ]);
        assert_eq!(
            resolve_vaapi_node_for_vendor(&listing, Some(DmabufVendor::Intel)),
            NodeDecision::SwitchTo(PathBuf::from("/dev/dri/renderD129"))
        );
    }

    #[test]
    fn an_empty_listing_declines_a_known_vendor() {
        // No render nodes at all. The gated caller bails before this on `default_vaapi_node()`
        // returning None, but the predicate must still be total and must not claim a switch.
        assert!(matches!(
            resolve_vaapi_node_for_vendor(&nodes(&[]), Some(DmabufVendor::Amd)),
            NodeDecision::Decline { .. }
        ));
    }

    // ── The default-node rule, shared with the gated helper ───────────────────

    #[test]
    fn the_default_is_the_first_vaapi_capable_node_in_order() {
        // This IS `default_vaapi_node`'s rule, lifted so it cannot drift from the listing.
        let listing = nodes(&[
            ("/dev/dri/renderD128", "nvidia"), // skipped: no VAAPI encode node
            ("/dev/dri/renderD129", "i915"),
            ("/dev/dri/renderD130", "amdgpu"),
        ]);
        assert_eq!(
            default_vaapi_node_from(&listing),
            Some(PathBuf::from("/dev/dri/renderD129"))
        );
        // Nothing usable at all.
        assert_eq!(default_vaapi_node_from(&nodes(&[("/dev/dri/renderD128", "nvidia")])), None);
        assert_eq!(default_vaapi_node_from(&nodes(&[])), None);
    }

    // ── The session log line ──────────────────────────────────────────────────

    #[test]
    fn every_decision_logs_something_a_human_can_act_on() {
        let node = PathBuf::from("/dev/dri/renderD128");
        let keep = decision_log_line(&node, Some(DmabufVendor::Amd), &NodeDecision::KeepDefault);
        assert!(keep.contains("renderD128") && keep.contains("AMD"), "{keep}");
        let switch = decision_log_line(
            &node,
            Some(DmabufVendor::Intel),
            &NodeDecision::SwitchTo(PathBuf::from("/dev/dri/renderD129")),
        );
        assert!(switch.contains("renderD128") && switch.contains("renderD129"), "{switch}");
        assert!(switch.contains("Intel"), "{switch}");
        let decline = decision_log_line(
            &node,
            None,
            &NodeDecision::Decline {
                user: "user half".into(),
                detail: "because reasons".into(),
            },
        );
        // The LOG gets the technical half, and must name the node it was about to open —
        // "we declined" without that leaves the next reader with half the story.
        assert!(decline.contains("because reasons"), "{decline}");
        assert!(decline.contains("renderD128"), "{decline}");
        assert!(!decline.contains("user half"), "{decline}");
        // An unknown vendor must still read as a sentence, not as an empty gap.
        let unknown = decision_log_line(&node, None, &NodeDecision::KeepDefault);
        assert!(unknown.contains("unidentified GPU"), "{unknown}");
    }
}
