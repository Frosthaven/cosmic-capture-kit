//! Preview HANDOFF IPC: a one-shot capture CHILD hands its finished file to an
//! already-running preview HOST instead of paying a second process's launch cost
//! (DRAGON-336, multi-document preview — phase 3a: transport + discovery only).
//!
//! ## Why this exists
//!
//! Every preview editor is its own OS process today (~233 MB RSS / ~60 MB PSS of fixed
//! launch overhead each: iced + wgpu + the font stack). The multi-document model keeps
//! ONE process hosting several preview windows, exiting when the last one closes. A
//! CAPTURE still has to be its own one-shot process — it must grab the frozen scene
//! before its overlay maps (the "capture scene model" invariant) — so the seam is at the
//! END of a capture: instead of opening its OWN preview, the child hands the finished
//! file to a live host and exits.
//!
//! ## The one rule: never lose what the user asked for
//!
//! A handoff is an OPTIMISATION, never a dependency. EVERY failure mode — no host, a
//! host that is exiting, a wedged host, a version-mismatched host, a refused connect, a
//! platform with no transport — surfaces as an `Err` from [`send_to_host`], and the
//! caller's ONLY correct response is to open its own preview exactly as it does today.
//! Nothing here may ever consume a capture without a positive acknowledgement.
//!
//! **This is ONE rule, not a list of handled cases, and that is what makes it structural**
//! (DRAGON-613). The three situations that look like they need their own handling all
//! collapse into it. A picker window that has JUST CLOSED cleared its marker and unlinked
//! its socket in `finish_session` before exiting, so the scan finds nothing: `NoHost`. One
//! that is MID-CLOSE may still have both on disk and may still accept a connection, but its
//! `Ack` drops unaccepted when its channel dies, so the sender reads EOF: `NotAccepted`. Any
//! OTHER failure is already one of those two. There is no fourth answer to write, because
//! every branch ends at the same place: the sender does the job itself, which is exactly
//! what it did before any of this existed, and says in the log why. So a colour can no more
//! vanish than a capture can, and neither can vanish SILENTLY.
//!
//! One thing the rule relies on and is worth naming: a peer in a DIFFERENT user session is
//! not discoverable at all, because the runtime dir is per-user and every candidate is
//! liveness-probed by pid before it is offered.
//!
//! ## Transport
//!
//! A dependency-free Unix domain socket, one per HOST pid, sitting in the runtime dir
//! next to that host's existing per-pid preview MARKER (`instance.rs`):
//!
//! ```text
//! {runtime_dir}/cosmic-capture-kit.<pid>.preview        <- the marker (discovery)
//! {runtime_dir}/cosmic-capture-kit.<pid>.preview.sock   <- this socket (transport)
//! ```
//!
//! Discovery therefore reuses the marker mechanism wholesale — there is no second
//! registry. [`crate::instance::live_preview_hosts`] returns the LIVE preview pids
//! (dead ones' markers are swept as it scans); this module turns each into its socket
//! path. A marker with no socket is simply a host that isn't listening (a preview from
//! an older build, or one that failed to bind): the connect fails and the child falls
//! back. See "Ordering at open" below.
//!
//! The protocol is one newline-terminated request line and one newline-terminated reply
//! line, in the plain-words spirit of [`crate::daemon_ipc`]:
//!
//! ```text
//! child -> host: cck-preview 5 open path=<hex> video=<0|1> dims=<WxH|-> scale=<f32> external=<0|1> size=<u64|->
//! child -> host: cck-preview 5 color to=<editor|picker> rgb=<RRGGBB> palette=<u64|->
//! child -> host: cck-preview 5 raise
//! host  -> child: ok
//!                 err version | err parse | err busy
//! ```
//!
//! * **Three verbs** ([`Request`]). `open` hands over a finished capture. `color` (DRAGON-587)
//!   hands a picked colour to whoever it is FOR: every picker launch is its own process, so
//!   "put this colour where it belongs" is a cross-process message of exactly this shape.
//!   `raise` (DRAGON-680) asks the one colour picker WINDOW to come forward, which is how a
//!   palette-viewer launch honours the single-window rule without sending a colour it does
//!   not mean to apply.
//! * **`color` carries its DESTINATION** ([`ColorDest`], DRAGON-613), and the two
//!   destinations are found in opposite ways on purpose. `to=editor` is ADDRESSED
//!   ([`send_color_to_pid`], the pid rides the launch), so an editor's pick can never reach
//!   an editor that did not ask for it. `to=picker` is SEARCHED for
//!   ([`send_color_to_picker`], via [`crate::instance::live_color_picker_windows`]), because
//!   the colour picker window is a singleton the user thinks of as one thing no matter which
//!   process owns it. A host handed a destination it cannot serve REFUSES, and the sender's
//!   answer to a refusal is the same as to every other failure below: do the job itself.
//! * **Versioned**: the `4` is [`PROTOCOL_VERSION`], checked BEFORE the verb, so a
//!   mismatched pair fails cleanly (`err version`) instead of half-parsing a future
//!   line. Bump it on ANY change to the field set or shape.
//! * **`path` is hex** (lowercase, raw bytes): a path may contain spaces, newlines, or
//!   (on unix) bytes that aren't UTF-8 at all, none of which survive whitespace
//!   splitting. Hex is still one plain word, and it round-trips ANY `PathBuf`.
//! * **Keys are order-independent, exhaustive, and closed**: every field must appear
//!   exactly once and no unknown key is tolerated — a silently-ignored field would mean
//!   a preview that quietly differs from what the child would have opened. New fields
//!   ride a version bump.
//!
//! ## How the host-is-exiting race is made SAFE
//!
//! The dangerous window is a host tearing down at the exact moment a child connects: the
//! listening socket can still be in the filesystem and its backlog can still accept a
//! connection while the process is on its way out. A fire-and-forget send would drop the
//! capture on the floor.
//!
//! So the handoff is **acknowledged, and the acknowledgement is owned by the host's
//! event loop, not by its accept thread**:
//!
//! 1. The accept thread reads the request and pushes a [`Handoff`] — the parsed request
//!    PLUS the still-open connection, wrapped in an [`Ack`] — onto the host's channel.
//! 2. The child BLOCKS (bounded, [`ACK_TIMEOUT`]) waiting for `ok`.
//! 3. Only when the host's `update` loop has actually taken ownership of the request
//!    does it call [`Handoff::accept`], which writes `ok` and closes.
//! 4. **Dropping an [`Ack`] without accepting closes the connection with no reply.** The
//!    child sees EOF and falls back. That is not an error path bolted on — it is the
//!    DEFAULT: a dropped channel receiver (host shutting down), a dropped `Handoff`, a
//!    panicking update, or plain process death all reduce to "the fd closed", which the
//!    child reads as "not accepted".
//!
//! There is no code path in which the child concludes "accepted" without a live host
//! having explicitly written `ok`. The residual window is a host that dies in the
//! microseconds between writing `ok` and minting the window — unclosable without a
//! second round trip, and the integration step keeps it minimal by acking from the same
//! `update` arm that stores the request in App state.
//!
//! ## FD discipline
//!
//! Every fd this module owns is `O_CLOEXEC` ([`ensure_cloexec`] asserts + repairs it on
//! the listener, on each accepted stream, and on the child's connection). `std`'s unix
//! sockets already ask for `SOCK_CLOEXEC`/`accept4`, but the record pipeline's hard-won
//! lesson (a leaked write end means the peer never sees EOF) is worth an explicit,
//! tested belt: a handoff connection leaked into a spawned ffmpeg would hold the ack
//! channel open past the process that owns it, and EOF is exactly the signal the child's
//! fallback rides on.
//!
//! ## How the App is wired to this (phase 3b — DONE)
//!
//! Mirroring how `daemon_ipc` reaches the app (a background thread feeds an `mpsc`
//! channel; a small named subscription drains it — see `tray.rs`'s reader thread +
//! `App::sub_tray_poll`):
//!
//! **Host side**
//! 1. [`start_host`] is called from `App::preview_surface_for` (the ONE choke point every
//!    preview mint routes through), storing the [`PreviewHost`] in `App::handoff_host`. It
//!    binds BEFORE `instance::set_preview_marker(true)`, so a marker a child discovers
//!    always implies a bound listener, and it is idempotent — re-mints keep the one listener.
//!    `App::finish_session` drops it (its `Drop` unlinks the socket, or stops the Windows
//!    pipe thread) as the process ends.
//! 2. `App::sub_preview_handoff` (`app/subscriptions.rs`) is a 100ms tick gated on
//!    `self.handoff_host.is_some()`, emitting `Msg::Capture(CaptureMsg::HandoffPoll)`. The
//!    poll lives in the CAPTURE domain because it has no preview to address — it may be
//!    what creates the first one, and `Msg::Preview` carries a `window::Id`.
//! 3. `App::drain_preview_handoffs` (`preview/open.rs`) drains with `try_recv`, opens each
//!    request through `App::open_handoff_preview` (which maps the identically-named fields
//!    onto `PreviewState` but deliberately does NOT claim `capture_preview`), and acks in
//!    the SAME arm; a request it will not serve is `reject`ed so the child stops waiting.
//!
//! **Child side**
//! 4. `App::try_handoff_capture`, called from `App::present_capture` at the exact point a
//!    capture would open its preview, builds an [`OpenRequest`] and calls [`send_to_host`].
//!    On `Ok(pid)` it routes to `finish_session`; on ANY `Err` it returns `None` and the
//!    caller opens the preview locally exactly as before. It is unconditional since
//!    DRAGON-351 (the temporary `instance::handoff_allowed` "allow multiple instances"
//!    gate went with that setting), except that it never hands off while this process is
//!    itself hosting another process's document.
//!
//! ## Platforms
//!
//! Unix (Linux + macOS) get the socket transport; both already share the runtime-dir
//! marker convention, so the code is one arm, not two. Windows has no unix sockets: it
//! speaks the SAME wire protocol over per-pid NAMED PIPES (DRAGON-651), which is exactly
//! `daemon_ipc`'s socket/pipe split (`daemon_ipc::pipe_name`) behind these same seams.
//! The addresses are `instance.rs`'s (`preview_host_address` /
//! `color_picker_host_address`), the Win32 server/client bodies live in
//! `platform::windows::preview_pipe` (closed split), and this module keeps the portable
//! spine — the wire protocol, [`Handoff`]/[`Ack`]/[`PreviewHost`], and thin per-platform
//! dispatch arms. Only a target that is NEITHER unix nor Windows keeps the honest stubs
//! (no [`start_host`], senders returning `Err(HandoffError::Unsupported)`).

// The HOST half of this protocol — the request PARSER, the reply writer, the listener and
// everything they reach — is compiled only where a transport exists (unix sockets, or the
// Windows named-pipe twin). On a target with neither, the child side is all that survives,
// so the parsing side is legitimately unreferenced there. Gated honestly per-platform
// rather than with a blanket allow: where a transport exists nothing here is exempt, so a
// genuinely dead item still shows up.
#![cfg_attr(not(any(unix, windows)), allow(dead_code))]

use std::path::PathBuf;
use std::time::Duration;

// ── Wire protocol ────────────────────────────────────────────────────────────

/// The first word of every request line — cheap proof the peer speaks this protocol at
/// all (the runtime dir holds other sockets).
const MAGIC: &str = "cck-preview";

/// Wire protocol version. Checked for EXACT equality before anything else is
/// interpreted, so a child and host from different builds fail cleanly (`err version` →
/// the child opens its own preview) instead of misparsing each other. Bump on ANY change
/// to the request line's field set, shape, or field semantics.
///
/// `2` since DRAGON-587 added the `color` verb (a colour picker launched BY an editor hands
/// its pick back to that editor). The verb set is part of the shape, so it takes the bump,
/// and a mixed pair simply falls back the way it always has.
///
/// `3` since DRAGON-613 gave the `color` verb its `to=` DESTINATION, because a picked colour
/// now has two possible consumers (an editor's swatch, or the one colour picker window) and
/// a receiver must never have to guess which it is holding.
///
/// **A mixed pair degrades cleanly, which matters because a user mid-upgrade IS one.** The
/// version is checked before the verb, so an old host answers a new sender `err version` and
/// a new host answers an old sender the same. Both sides read that as "not accepted", and
/// every "not accepted" in this module has exactly one meaning: the sender does the job
/// itself. So a v2 picker window plus a v3 pick simply opens a second window, which is the
/// behaviour that shipped before this feature, never a lost colour and never a lost capture.
///
/// `4` since DRAGON-680 added the `raise` verb (the palette viewer asks the one open picker
/// window to come forward instead of opening a second one). The verb set is part of the
/// shape, so it takes the bump, and a mixed pair degrades exactly as above: the viewer's
/// raise is refused and it opens its own window, which is what it would have done anyway
/// had no window been open.
///
/// `5` since DRAGON-687 gave the `color` verb its `palette=` field (a pick launched by a
/// palette row's pipette is FOR that saved palette, not for the window's active swatch).
/// The field set is part of the shape, so it takes the bump, and a mixed pair degrades on
/// the raise verb's exact story: an old window answers the new line `err version`, the
/// sender reads every "not accepted" as "do the job yourself", and the child runs the
/// ordinary pick flow (active colour, recents, clipboard, its own window if nothing takes
/// it). A colour is never lost; only the filing shortcut is.
pub const PROTOCOL_VERSION: u32 = 5;

/// Longest request/reply line accepted, in bytes. A path is the only unbounded field and
/// hex-doubles, so 64 KiB is ~32 KiB of path — far past any real one, while keeping a
/// garbage (or hostile) sender from making either side allocate without bound.
/// `pub(crate)` since DRAGON-651: the Windows pipe transport enforces the same cap (and
/// sizes its kernel buffers past it, which is what bounds its send with no write timeout).
pub(crate) const MAX_LINE: u64 = 64 * 1024;

/// How long a child waits for the host's `ok` before giving up and opening its own
/// preview. Generous next to the host's ~100ms drain tick, but BOUNDED: a wedged host
/// must cost a capture a short pause, never the capture itself.
pub const ACK_TIMEOUT: Duration = Duration::from_secs(3);

/// How long the child will spend writing its (single, small) request line. Unix only:
/// the Windows pipe's write is bounded structurally instead (its kernel buffer holds a
/// whole [`MAX_LINE`], so the one write never waits on the host).
#[cfg(unix)]
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the HOST waits for a connected peer to finish its request line before
/// dropping the connection. Keeps one stalled connector from pinning the accept thread
/// and starving real handoffs. `pub(crate)` since DRAGON-651: the Windows accept loop
/// applies the same bound.
pub(crate) const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Everything the host needs to open a preview IDENTICAL to the one the child would have
/// opened. The field names match `app::preview::PreviewState`'s deliberately; the host
/// maps them straight across.
///
/// `video` is carried rather than re-derived so the two sides can never disagree about a
/// file's kind — the child already classified it (`is_video_path` / `is_image_path`) when
/// it decided what preview to build.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenRequest {
    /// The finished capture's on-disk path (absolute; the host is a different process
    /// but shares the filesystem).
    pub path: PathBuf,
    /// `true` for a recording (video preview), `false` for a still (image preview).
    pub video: bool,
    /// The capture's ON-SCREEN footprint in physical px, when known — `PreviewState::display_dims`.
    pub display_dims: Option<(u32, u32)>,
    /// The SOURCE display's point→pixel backing scale — `PreviewState::source_scale`
    /// (`1.0` on an unscaled output, on every platform). Must be finite and positive.
    pub source_scale: f32,
    /// `true` when previewing a pre-existing file (`--preview`) rather than a fresh
    /// capture — `PreviewState::external`.
    pub external: bool,
    /// The saved file's size in bytes, once known — `PreviewState::size`.
    pub size: Option<u64>,
}

/// Why a request line could not be turned into an [`OpenRequest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    /// The line's protocol version isn't [`PROTOCOL_VERSION`] (carries theirs). Reported
    /// separately so the host can answer `err version` — a mismatched pair should learn
    /// WHY it failed rather than look like corruption.
    Version(u32),
    /// Not a well-formed request line for this version: wrong magic/verb, a missing,
    /// duplicated, unknown or unparseable field.
    Malformed,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Version(v) => {
                write!(f, "protocol version {v} (we speak {PROTOCOL_VERSION})")
            }
            WireError::Malformed => write!(f, "malformed request line"),
        }
    }
}

impl OpenRequest {
    /// Encode as the single newline-terminated request line the host parses.
    pub fn encode(&self) -> String {
        let dims = match self.display_dims {
            Some((w, h)) => format!("{w}x{h}"),
            None => "-".to_string(),
        };
        let size = match self.size {
            Some(n) => n.to_string(),
            None => "-".to_string(),
        };
        format!(
            "{MAGIC} {PROTOCOL_VERSION} open path={} video={} dims={dims} scale={} external={} size={size}\n",
            hex_encode(&path_bytes(&self.path)),
            self.video as u8,
            self.source_scale,
            self.external as u8,
        )
    }

    /// Parse one request line. Tolerates the trailing newline / surrounding whitespace.
    ///
    /// Strict by design: the version is checked FIRST (so a future protocol reports
    /// [`WireError::Version`], not a confusing parse error), every field must be present
    /// exactly once, and an unknown key is a hard error — a preview opened from a
    /// half-understood line is exactly the silent wrongness this protocol exists to avoid.
    pub fn parse(line: &str) -> Result<Self, WireError> {
        let mut it = line.split_whitespace();
        if it.next() != Some(MAGIC) {
            return Err(WireError::Malformed);
        }
        let ver: u32 = it.next().and_then(|v| v.parse().ok()).ok_or(WireError::Malformed)?;
        if ver != PROTOCOL_VERSION {
            return Err(WireError::Version(ver));
        }
        if it.next() != Some("open") {
            return Err(WireError::Malformed);
        }

        let mut path: Option<PathBuf> = None;
        let mut video: Option<bool> = None;
        let mut dims: Option<Option<(u32, u32)>> = None;
        let mut scale: Option<f32> = None;
        let mut external: Option<bool> = None;
        let mut size: Option<Option<u64>> = None;

        for tok in it {
            let (key, val) = tok.split_once('=').ok_or(WireError::Malformed)?;
            // `set` rejects a duplicate key: two values for one field is corruption, not
            // a last-writer-wins convenience.
            match key {
                "path" => set(&mut path, path_from_bytes(&hex_decode(val).ok_or(WireError::Malformed)?)
                    .ok_or(WireError::Malformed)?)?,
                "video" => set(&mut video, parse_bit(val).ok_or(WireError::Malformed)?)?,
                "dims" => set(&mut dims, parse_dims(val).ok_or(WireError::Malformed)?)?,
                "scale" => set(&mut scale, parse_scale(val).ok_or(WireError::Malformed)?)?,
                "external" => set(&mut external, parse_bit(val).ok_or(WireError::Malformed)?)?,
                "size" => set(&mut size, parse_opt_u64(val).ok_or(WireError::Malformed)?)?,
                _ => return Err(WireError::Malformed),
            }
        }

        Ok(OpenRequest {
            path: path.ok_or(WireError::Malformed)?,
            video: video.ok_or(WireError::Malformed)?,
            display_dims: dims.ok_or(WireError::Malformed)?,
            source_scale: scale.ok_or(WireError::Malformed)?,
            external: external.ok_or(WireError::Malformed)?,
            size: size.ok_or(WireError::Malformed)?,
        })
    }
}

/// What one request line ASKS FOR. One socket, one protocol, two verbs (DRAGON-587).
///
/// The second verb exists because the colour picker launched from a preview editor's pipette
/// is a separate PROCESS (every picker launch is), and its whole purpose is to put a colour
/// into the editor that asked for it. That is the same shape as a handoff, at the same
/// address, so it rides this socket rather than growing a second channel: the DRAGON-583
/// lesson, where the recording relay gained an address instead of a fork.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// `open`: take ownership of a finished capture and show it.
    Open(OpenRequest),
    /// `color`: deliver a picked sRGB triple to the consumer named by [`ColorDest`].
    ///
    /// Carries the colour and WHO IT IS FOR, and nothing else (DRAGON-613). The destination
    /// is on the wire rather than inferred by the receiver from its own identity, because
    /// "who is this value for" is the property being modelled: a host that is handed a
    /// destination it cannot serve REFUSES, which the sender turns into doing the job
    /// itself, instead of quietly applying a colour somewhere nobody asked for.
    Color {
        /// Which consumer this colour belongs to.
        to: ColorDest,
        /// The picked colour, straight sRGB.
        rgb: [u8; 3],
        /// DRAGON-687: the SAVED PALETTE this colour should be filed into, as the opaque
        /// nonce the picker window minted for its pipette launch, or `None` for every
        /// ordinary pick. On the wire it is `palette=<u64|->`, a required key with the
        /// dash spelling `open`'s `dims` and `size` already use for "no value".
        ///
        /// A NONCE rather than the group's index or name, deliberately: the group can be
        /// renamed, re-sorted or deleted while the pick is out, so the wire carries a
        /// token only the receiving window can interpret (against the snapshot it kept at
        /// launch), and a token it does not recognise degrades to an ordinary pick rather
        /// than filing into whichever group drifted under the index. It also keeps the
        /// palette's NAME, which is user content, off the wire and out of the child's
        /// environment.
        ///
        /// Only meaningful with `to=picker`: an editor line carrying one is MALFORMED
        /// (enforced in the parser), because an editor has no palettes and a field the
        /// receiver would have to ignore is exactly what this protocol's closed field
        /// sets exist to prevent.
        palette: Option<u64>,
    },
    /// `raise`: bring the colour picker window forward (DRAGON-680). Carries nothing.
    ///
    /// The palette viewer's launch is the only sender: at most ONE picker window exists
    /// (DRAGON-613), so a second "show me my palette" must raise the live one rather than
    /// put a duplicate on screen. It is a verb of its own rather than a re-send of the
    /// current colour through `color`, because a colour SEND has effects a raise must not
    /// have: it would replace what the live window is showing with this process's stale
    /// front recent, promote that entry, and rewrite the clipboard.
    ///
    /// **No `to=` field, deliberately.** `color` needs one because a colour has two
    /// possible consumers; there is exactly one raisable singleton, so a destination here
    /// would be a field with one legal value. A second raisable surface would add the field
    /// and a version bump, exactly as `color` did in v3.
    Raise,
}

/// Who a picked colour is FOR (DRAGON-613): the wire form of
/// `app::color_picker::PickDestination`.
///
/// It is a DESTINATION rather than a source on purpose. Keying on "which UI opened the
/// picker" means every future consumer has to be special-cased at every site that handles a
/// pick; keying on who the value is for means a new consumer adds one variant, one wire
/// word, and one arm in the drain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorDest {
    /// A preview editor's annotation swatch. The pick belongs to the editor whose pipette
    /// launched this picker, addressed by ITS pid, so it can never reach an unrelated one.
    Editor,
    /// The colour picker's own result window: this becomes the colour it shows and the
    /// newest entry in its recents.
    PickerWindow,
}

impl ColorDest {
    /// The wire word. One plain word, like every other value in this protocol.
    pub fn as_str(self) -> &'static str {
        match self {
            ColorDest::Editor => "editor",
            ColorDest::PickerWindow => "picker",
        }
    }

    /// The inverse of [`Self::as_str`]. `None` for anything else, which parses as
    /// malformed rather than defaulting: guessing a destination is exactly the failure
    /// this field exists to prevent.
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "editor" => Some(ColorDest::Editor),
            "picker" => Some(ColorDest::PickerWindow),
            _ => None,
        }
    }
}

impl Request {
    /// Encode as the single newline-terminated request line the host parses.
    pub fn encode(&self) -> String {
        match self {
            Request::Open(r) => r.encode(),
            Request::Color { to, rgb: [r, g, b], palette } => {
                let palette = match palette {
                    Some(nonce) => nonce.to_string(),
                    None => "-".to_string(),
                };
                format!(
                    "{MAGIC} {PROTOCOL_VERSION} color to={} rgb={r:02x}{g:02x}{b:02x} \
                     palette={palette}\n",
                    to.as_str()
                )
            }
            Request::Raise => format!("{MAGIC} {PROTOCOL_VERSION} raise\n"),
        }
    }

    /// Parse one request line, whichever verb it carries.
    ///
    /// The version is checked FIRST, before the verb, for the reason [`OpenRequest::parse`]
    /// gives: a future protocol must report [`WireError::Version`] rather than look like
    /// corruption. An unknown verb is [`WireError::Malformed`].
    pub fn parse(line: &str) -> Result<Self, WireError> {
        let mut it = line.split_whitespace();
        if it.next() != Some(MAGIC) {
            return Err(WireError::Malformed);
        }
        let ver: u32 = it.next().and_then(|v| v.parse().ok()).ok_or(WireError::Malformed)?;
        if ver != PROTOCOL_VERSION {
            return Err(WireError::Version(ver));
        }
        match it.next() {
            // Re-parsed from the whole line so the `open` verb keeps ONE parser, the strict
            // one with its exhaustive closed field set.
            Some("open") => OpenRequest::parse(line).map(Request::Open),
            Some("color") => {
                // Order-independent, exhaustive and closed, exactly like `open`: every field
                // exactly once, no unknown key. A `color` line missing its destination is
                // malformed rather than defaulted, because a colour applied to a guessed
                // consumer is precisely the silent wrongness this protocol exists to avoid.
                let mut to: Option<ColorDest> = None;
                let mut rgb: Option<[u8; 3]> = None;
                let mut palette: Option<Option<u64>> = None;
                for tok in it {
                    let (key, val) = tok.split_once('=').ok_or(WireError::Malformed)?;
                    match key {
                        "to" => set(&mut to, ColorDest::parse(val).ok_or(WireError::Malformed)?)?,
                        "rgb" => {
                            let bytes = hex_decode(val).ok_or(WireError::Malformed)?;
                            let [r, g, b] = bytes[..] else {
                                return Err(WireError::Malformed);
                            };
                            set(&mut rgb, [r, g, b])?
                        }
                        // DRAGON-687: the palette nonce, `-` for the ordinary pick. A
                        // ZERO nonce is malformed rather than a value: the picker never
                        // mints one (its counter starts at 1, `0` is its own "no target"
                        // spelling in the launch environment), so a zero on the wire is a
                        // constructed line, not a pick.
                        "palette" => match val {
                            "-" => set(&mut palette, None)?,
                            _ => {
                                let nonce: u64 =
                                    val.parse().map_err(|_| WireError::Malformed)?;
                                if nonce == 0 {
                                    return Err(WireError::Malformed);
                                }
                                set(&mut palette, Some(nonce))?
                            }
                        },
                        _ => return Err(WireError::Malformed),
                    }
                }
                let to = to.ok_or(WireError::Malformed)?;
                let palette = palette.ok_or(WireError::Malformed)?;
                // A palette target on an editor line is malformed, not ignored: an editor
                // has no palettes, and a field the receiver would have to skip is what
                // this protocol's closed field sets exist to prevent.
                if to == ColorDest::Editor && palette.is_some() {
                    return Err(WireError::Malformed);
                }
                Ok(Request::Color { to, rgb: rgb.ok_or(WireError::Malformed)?, palette })
            }
            // DRAGON-680: a fieldless verb, so the field set is closed by there being
            // nothing after it. A trailing token is malformed rather than ignored, the same
            // strictness the other two verbs apply to an unknown key.
            Some("raise") => match it.next() {
                None => Ok(Request::Raise),
                Some(_) => Err(WireError::Malformed),
            },
            _ => Err(WireError::Malformed),
        }
    }
}

/// Store a parsed field, failing if it was already set (a duplicate key).
fn set<T>(slot: &mut Option<T>, value: T) -> Result<(), WireError> {
    if slot.is_some() {
        return Err(WireError::Malformed);
    }
    *slot = Some(value);
    Ok(())
}

fn parse_bit(v: &str) -> Option<bool> {
    match v {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

fn parse_dims(v: &str) -> Option<Option<(u32, u32)>> {
    if v == "-" {
        return Some(None);
    }
    let (w, h) = v.split_once('x')?;
    Some(Some((w.parse().ok()?, h.parse().ok()?)))
}

/// Scales must be finite and positive — the receiver DIVIDES by this to get logical
/// points, so a zero / NaN / negative would poison the preview's whole sizing math.
fn parse_scale(v: &str) -> Option<f32> {
    let s: f32 = v.parse().ok()?;
    (s.is_finite() && s > 0.0).then_some(s)
}

fn parse_opt_u64(v: &str) -> Option<Option<u64>> {
    if v == "-" {
        return Some(None);
    }
    Some(Some(v.parse().ok()?))
}

/// The host's answer to a request. Exactly one line, then the connection closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    /// The host has TAKEN OWNERSHIP of the capture; the child may exit.
    Accepted,
    /// The host will not take it; the child must open its own preview.
    Rejected(RejectReason),
}

/// Why a host refused a handoff. Advisory only — every variant means the same thing to
/// the child (open your own preview); the word exists so logs say which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// The request line's protocol version isn't ours.
    Version,
    /// The request line didn't parse.
    Parse,
    /// The host is alive but can't take another document right now (or is shutting down).
    Busy,
}

impl RejectReason {
    /// The single wire word for this reason.
    pub fn as_str(self) -> &'static str {
        match self {
            RejectReason::Version => "version",
            RejectReason::Parse => "parse",
            RejectReason::Busy => "busy",
        }
    }
}

impl Reply {
    /// Encode as the newline-terminated reply line the child reads.
    pub fn encode(self) -> String {
        match self {
            Reply::Accepted => "ok\n".to_string(),
            Reply::Rejected(r) => format!("err {}\n", r.as_str()),
        }
    }

    /// Parse one reply line; `None` for anything unrecognised (which the child treats
    /// exactly like a rejection — never like an acceptance).
    pub fn parse(line: &str) -> Option<Self> {
        let mut it = line.split_whitespace();
        match it.next()? {
            "ok" => it.next().is_none().then_some(Reply::Accepted),
            "err" => {
                let reason = match it.next()? {
                    "version" => RejectReason::Version,
                    "parse" => RejectReason::Parse,
                    "busy" => RejectReason::Busy,
                    _ => return None,
                };
                it.next().is_none().then_some(Reply::Rejected(reason))
            }
            _ => None,
        }
    }
}

// ── Path <-> bytes (unix paths are bytes, not necessarily UTF-8) ──────────────

#[cfg(unix)]
fn path_bytes(p: &std::path::Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    p.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(p: &std::path::Path) -> Vec<u8> {
    // Windows paths are UTF-16; their lossy UTF-8 form round-trips every real path (a
    // path with unpaired surrogates would mangle, fail at open, and land on the fallback
    // rule — never a lost capture). The DRAGON-427 spawn handoff has fed this exact
    // encoding to `open_handoff_preview` since before the pipes existed.
    p.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
fn path_from_bytes(b: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(b)))
}

#[cfg(not(unix))]
fn path_from_bytes(b: &[u8]) -> Option<PathBuf> {
    Some(PathBuf::from(String::from_utf8(b.to_vec()).ok()?))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Decode lowercase/uppercase hex; `None` for odd length or a non-hex digit. An EMPTY
/// string decodes to empty bytes, which `path_from_bytes` turns into an empty path —
/// caught by the caller's own validation, not silently accepted as a file.
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in s.as_bytes().as_chunks::<2>().0 {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

// ── Transport: unix ──────────────────────────────────────────────────────────

/// Assert (and repair) `FD_CLOEXEC` on a socket fd.
///
/// `std` already asks the kernel for `SOCK_CLOEXEC` / `accept4`, but the record
/// pipeline's rule is that no fd of ours is CLOEXEC by assumption — a handoff connection
/// inherited by a spawned child (ffmpeg, a capture grandchild) would hold the ack channel
/// open past the process that owns it, and the child's fallback rides on exactly that
/// EOF. Cheap, idempotent, and unit-tested.
///
/// `pub(crate)` since DRAGON-583: the recording-control inlet in [`crate::daemon_ipc`] is
/// the same kind of runtime-dir unix socket under the same rule, and one tested helper is
/// better than a second copy of it.
#[cfg(unix)]
pub(crate) fn ensure_cloexec<F: std::os::fd::AsFd>(fd: &F) -> std::io::Result<()> {
    use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};
    let flags = fcntl_getfd(fd)?;
    if !flags.contains(FdFlags::CLOEXEC) {
        fcntl_setfd(fd, flags | FdFlags::CLOEXEC)?;
    }
    Ok(())
}

/// The connection an [`Ack`] answers over: the transport's own stream type.
#[cfg(unix)]
type AckConn = std::os::unix::net::UnixStream;
/// Windows (DRAGON-651): the accepted named-pipe instance, wrapped in a plain `File`.
/// Closing it without a reply is the rejection, exactly as with the socket. The written
/// reply survives the close (`CloseHandle` keeps buffered bytes readable; only
/// `DisconnectNamedPipe` would discard them, which is why the transport never calls it).
#[cfg(windows)]
type AckConn = std::fs::File;

/// The host's still-open connection back to a waiting child — the ACK half of a
/// [`Handoff`].
///
/// **Dropping an `Ack` without [`accept`](Ack::accept)ing rejects the handoff**: the
/// connection closes, the child reads EOF and opens its own preview. That default is what
/// makes the host-is-exiting race safe (a dropped channel receiver, a dropped `Handoff`,
/// a panicking update, or plain process death all land here), so there is deliberately no
/// `Drop` impl to get wrong — closing the connection IS the rejection.
#[cfg(any(unix, windows))]
pub struct Ack(Option<AckConn>);

#[cfg(any(unix, windows))]
impl Ack {
    /// Tell the child the host has TAKEN OWNERSHIP — the child may exit. Call this only
    /// once the request is actually stored in App state.
    pub fn accept(mut self) {
        self.reply(Reply::Accepted);
    }

    /// Tell the child we will not take it, so it opens its own preview immediately
    /// rather than waiting out [`ACK_TIMEOUT`]. (Dropping the `Ack` means the same
    /// thing, just without the reason.)
    pub fn reject(mut self, reason: RejectReason) {
        self.reply(Reply::Rejected(reason));
    }

    fn reply(&mut self, reply: Reply) {
        use std::io::Write as _;
        if let Some(mut conn) = self.0.take() {
            let _ = conn.write_all(reply.encode().as_bytes());
            let _ = conn.flush();
        }
    }
}

/// One inbound handoff: the parsed request plus the [`Ack`] that answers it.
#[cfg(any(unix, windows))]
pub struct Handoff {
    request: Request,
    ack: Ack,
}

/// Mint a [`Handoff`] from a connection the Windows pipe server accepted — the plugin's
/// (`platform::windows::preview_pipe`) way into the portable acknowledgement model, whose
/// fields stay private so nothing else can build one half-formed.
#[cfg(windows)]
pub(crate) fn pipe_handoff(request: Request, conn: std::fs::File) -> Handoff {
    Handoff { request, ack: Ack(Some(conn)) }
}

/// Answer a connection whose request line never parsed — the Windows accept loop's
/// error-reply path, the twin of the unix loop's own `ack.reject(...)` arms.
#[cfg(windows)]
pub(crate) fn pipe_reject(conn: std::fs::File, reason: RejectReason) {
    Ack(Some(conn)).reject(reason);
}

#[cfg(any(unix, windows))]
impl Handoff {
    /// What is being asked of this host.
    pub fn request(&self) -> &Request {
        &self.request
    }

    /// Accept: the host owns this capture now.
    pub fn accept(self) {
        self.ack.accept();
    }

    /// Refuse: the child opens its own preview.
    pub fn reject(self, reason: RejectReason) {
        self.ack.reject(reason);
    }
}

/// A bound preview-handoff listener plus the channel its accept thread feeds.
///
/// Hold one on `App` for as long as any preview window is open. `Drop` makes a host that
/// has stopped hosting stop being discoverable immediately: on unix it unlinks the socket
/// (the accept thread itself is detached and dies with the process — a one-shot-model
/// process exits when its last preview closes, so there is nothing to join); on Windows
/// it stops the accept thread, whose closed instance handle is what frees the pipe name
/// (see the Drop impl).
#[cfg(any(unix, windows))]
pub struct PreviewHost {
    /// The listener's address: the socket path on unix, the pipe name on Windows.
    address: String,
    /// Inbound handoffs. Drain with `try_recv` from the host's update loop; see the
    /// module doc's integration recipe.
    pub requests: std::sync::mpsc::Receiver<Handoff>,
    /// Windows: tells the accept thread to stop; `Drop` sets it and then wakes the
    /// thread's blocking connect with a throwaway self-connection.
    #[cfg(windows)]
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(any(unix, windows))]
impl PreviewHost {
    /// The address this host is listening on (the socket path / the pipe name).
    pub fn address(&self) -> &str {
        &self.address
    }
}

#[cfg(unix)]
impl Drop for PreviewHost {
    fn drop(&mut self) {
        // Best-effort: an unlink failure only leaves a stale socket, which
        // `instance::sweep_stale_markers` clears on the next launch.
        let _ = std::fs::remove_file(&self.address);
    }
}

#[cfg(windows)]
impl Drop for PreviewHost {
    fn drop(&mut self) {
        // A pipe name cannot be unlinked; it lives while any instance handle does. So:
        // STORE the stop flag, then SELF-CONNECT. The throwaway connection is what
        // unsticks the accept thread's blocking `ConnectNamedPipe`; the flag (re-checked
        // right after every connect and every fresh instance) is what it acts on — it
        // returns, its handle drops, the name disappears. A residual interleaving (the
        // store landing while the thread is between instances) leaves the thread parked
        // until process exit, which is the unix accept thread's ordinary shape anyway:
        // these are one-shot processes, and `finish_session` ends them moments after this
        // drop. A sender that connects in that window gets no ack and falls back, the
        // same mid-close rule the module doc names.
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        crate::platform::windows::preview_pipe::wake_accept(&self.address);
    }
}

/// Begin hosting preview handoffs for THIS process at its per-pid preview address
/// (`instance::preview_host_address`: the `….<pid>.preview.sock` socket on unix, the
/// `\\.\pipe\….<pid>.preview` pipe on Windows) and start the accept thread.
///
/// Bind BEFORE `instance::set_preview_marker(true)` so a marker a child discovers always
/// implies a listener that already exists.
#[cfg(any(unix, windows))]
pub fn start_host() -> std::io::Result<PreviewHost> {
    start_host_at(crate::instance::preview_host_address(std::process::id()))
}

/// [`start_host`] against an EXPLICIT address, so a process that is not a preview host
/// can listen on the same protocol at its OWN address (DRAGON-613: the colour picker
/// window listens at `instance::color_picker_host_address`).
///
/// Generalised rather than copied for the reason `instance::live_marker_hosts_in` was: the
/// bind, the cloexec repair, the accept thread and the unlink-on-drop are the transport, not
/// the preview, and two copies of them would be two things to keep in step.
///
/// A leftover socket at our own unix path (a previous process with the same pid,
/// SIGKILLed) is unlinked first — binding over it is the only way to reclaim the name,
/// and a live process cannot be holding our own pid's path. (A Windows pipe name needs no
/// such reclaim: it died with its owner's handles.)
#[cfg(unix)]
pub fn start_host_at(path: String) -> std::io::Result<PreviewHost> {
    use std::os::unix::net::UnixListener;

    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    ensure_cloexec(&listener)?;

    let (tx, requests) = std::sync::mpsc::channel::<Handoff>();
    let spawned = std::thread::Builder::new()
        .name("cck-preview-host-ipc".into())
        .spawn(move || accept_loop(&listener, &tx));
    if let Err(e) = spawned {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(PreviewHost { address: path, requests })
}

/// The Windows arm of [`start_host_at`] (DRAGON-651): create the pipe's first listening
/// instance SYNCHRONOUSLY — so a bind failure is reported to the caller exactly as a
/// failed socket bind is, never swallowed inside a thread — then hand it to the accept
/// loop in `platform::windows::preview_pipe`.
#[cfg(windows)]
pub fn start_host_at(address: String) -> std::io::Result<PreviewHost> {
    use crate::platform::windows::preview_pipe;

    let first = preview_pipe::create_instance(&address)?;
    let (tx, requests) = std::sync::mpsc::channel::<Handoff>();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread_stop = stop.clone();
    let name = address.clone();
    std::thread::Builder::new()
        .name("cck-preview-host-ipc".into())
        .spawn(move || preview_pipe::accept_loop(name, first, tx, thread_stop))?;
    Ok(PreviewHost { address, requests, stop })
}

/// The blocking accept loop, factored out so it can be driven directly by a test (or by
/// a future caller that owns its own thread). Reads ONE request per connection and posts
/// it as a [`Handoff`]; returns when the listener dies or the receiver is dropped.
///
/// Every error is per-connection: a peer that speaks garbage, stalls, or vanishes gets
/// its connection dropped (which the child reads as "not accepted") and the loop keeps
/// serving the next one.
#[cfg(unix)]
fn accept_loop(
    listener: &std::os::unix::net::UnixListener,
    tx: &std::sync::mpsc::Sender<Handoff>,
) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if ensure_cloexec(&stream).is_err() {
            continue;
        }
        // A stalled connector must never pin the accept thread and starve real handoffs.
        if stream.set_read_timeout(Some(REQUEST_READ_TIMEOUT)).is_err() {
            continue;
        }
        let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
        let line = match read_line(&stream) {
            Ok(l) => l,
            Err(_) => continue, // EOF / timeout / I/O: drop it, the child falls back
        };
        let ack = Ack(Some(stream));
        match Request::parse(&line) {
            Ok(request) => {
                // A dropped receiver means the host has stopped hosting: the returned
                // `Handoff` (and its `Ack`) drops here, closing the connection with no
                // reply — exactly the "not accepted" signal the child needs. Then stop.
                if tx.send(Handoff { request, ack }).is_err() {
                    return;
                }
            }
            Err(WireError::Version(v)) => {
                log::warn!("preview handoff: peer speaks protocol {v}, we speak {PROTOCOL_VERSION}");
                ack.reject(RejectReason::Version);
            }
            Err(WireError::Malformed) => ack.reject(RejectReason::Parse),
        }
    }
}

/// Read one bounded newline-terminated line from a socket.
#[cfg(unix)]
fn read_line<S: std::io::Read>(stream: S) -> std::io::Result<String> {
    use std::io::BufRead as _;
    let mut reader = std::io::BufReader::new(stream.take(MAX_LINE));
    let mut line = String::new();
    // `read_line` on a `Take` stops at the cap; a capped read with no newline is a
    // truncated (or hostile) line and parses as malformed, which is the right answer.
    if reader.read_line(&mut line)? == 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "no request line"));
    }
    Ok(line)
}

// ── Transport: the child side ────────────────────────────────────────────────

/// Why a handoff didn't happen. EVERY variant means the same thing to the caller: open
/// your own preview. The distinction exists for logs and tests.
#[derive(Debug)]
pub enum HandoffError {
    /// No live preview host is listening (the common case — this is the first capture).
    NoHost,
    /// A host was found but did not accept: it was exiting, wedged, version-mismatched,
    /// refused the connection, or explicitly rejected. Carries a reason for the log.
    NotAccepted(String),
    /// This platform has no preview-handoff transport (neither unix sockets nor Windows
    /// named pipes). Constructed ONLY by the no-transport sender stubs, so wherever a
    /// transport exists it is legitimately unreachable — the honest platform gate rather
    /// than a blanket module allow.
    #[cfg_attr(any(unix, windows), allow(dead_code))]
    Unsupported,
}

impl std::fmt::Display for HandoffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandoffError::NoHost => write!(f, "no preview host is listening"),
            HandoffError::NotAccepted(why) => write!(f, "preview host did not accept: {why}"),
            HandoffError::Unsupported => write!(f, "preview handoff unsupported on this platform"),
        }
    }
}

/// The pid of a live preview host, if any — the DISCOVERY half of this seam, exposed so
/// a caller can ask "is anyone hosting?" before it does preview-specific work.
///
/// Advisory ONLY: a host can exit between this call and [`send_to_host`], and a marker
/// with no socket isn't a host at all. Never branch on this alone; the acknowledged
/// [`send_to_host`] exchange is the only proof of a handoff.
#[cfg(any(unix, windows))]
pub fn host_pid() -> Option<u32> {
    crate::instance::live_preview_host()
}

/// No-transport stub (neither unix sockets nor Windows named pipes): there is never a
/// host to find, so the child's fast-out answers honestly without a `cfg` at the call
/// site (its companion is [`host_supported`]).
#[cfg(not(any(unix, windows)))]
pub fn host_pid() -> Option<u32> {
    None
}

/// Hand `req` to a running preview host, returning the host's pid on a POSITIVE
/// acknowledgement.
///
/// Tries every live preview host in preference order (most recently opened first), so a
/// host caught mid-exit doesn't cost the handoff when another is up. `Err` on ANY failure
/// — the caller MUST then open its own preview; that is the whole contract.
#[cfg(unix)]
pub fn send_to_host(req: &OpenRequest) -> Result<u32, HandoffError> {
    let hosts = crate::instance::live_preview_hosts();
    if hosts.is_empty() {
        return Err(HandoffError::NoHost);
    }
    let mut last: Option<String> = None;
    let req = Request::Open(req.clone());
    for pid in hosts {
        let path = crate::instance::preview_socket_path(pid);
        // A marker with no socket isn't a host at all (an older build's preview, or a
        // failed bind) — skip without counting it as a refusal.
        if !std::path::Path::new(&path).exists() {
            continue;
        }
        match send_to_socket(&path, &req) {
            Ok(()) => return Ok(pid),
            Err(why) => {
                log::info!("preview handoff to pid {pid} failed: {why}");
                last = Some(why);
            }
        }
    }
    match last {
        Some(why) => Err(HandoffError::NotAccepted(why)),
        None => Err(HandoffError::NoHost),
    }
}

/// Hand a picked colour to ONE named preview host (DRAGON-587): the editor whose pipette
/// launched this picker, and no other.
///
/// Addressed by pid rather than searched for, which is the whole point. A pick started from
/// the tray, the CLI or a keybinding has no target and never calls this; a pick started by an
/// editor targets THAT editor, so it can never reach into an unrelated one that happens to be
/// open. The pid rides the launch (`color_picker::COLOR_TO_PID_ENV`).
///
/// `Err` for every failure, including the ordinary one where the editor has since closed.
/// The caller's response is to carry on and show the picked colour its usual way; nothing
/// here may be treated as a lost pick.
#[cfg(unix)]
pub fn send_color_to_pid(pid: u32, rgb: [u8; 3]) -> Result<(), HandoffError> {
    let path = crate::instance::preview_socket_path(pid);
    if !std::path::Path::new(&path).exists() {
        return Err(HandoffError::NoHost);
    }
    send_to_socket(&path, &Request::Color { to: ColorDest::Editor, rgb, palette: None })
        .map_err(HandoffError::NotAccepted)
}

/// The Windows arm of [`send_color_to_pid`] (DRAGON-651): the same addressed delivery,
/// over the editor's per-pid named pipe. There is no socket FILE whose absence pre-answers
/// "nobody listening" — the connect's own not-found does, carried as
/// [`HandoffError::NoHost`] so the caller's ladder reads identically on every platform.
#[cfg(windows)]
pub fn send_color_to_pid(pid: u32, rgb: [u8; 3]) -> Result<(), HandoffError> {
    use crate::platform::windows::preview_pipe::SendError;
    let addr = crate::instance::preview_host_address(pid);
    match send_to_pipe(&addr, &Request::Color { to: ColorDest::Editor, rgb, palette: None }) {
        Ok(()) => Ok(()),
        Err(SendError::NoListener) => Err(HandoffError::NoHost),
        Err(SendError::Failed(why)) => Err(HandoffError::NotAccepted(why)),
    }
}

/// No-transport stub (neither unix sockets nor Windows named pipes): an editor-launched
/// pick simply shows its own result window, exactly as every other pick does.
#[cfg(not(any(unix, windows)))]
pub fn send_color_to_pid(_pid: u32, _rgb: [u8; 3]) -> Result<(), HandoffError> {
    Err(HandoffError::Unsupported)
}

/// Hand a picked colour to the EXISTING colour picker window, wherever it is (DRAGON-613).
///
/// The twin of [`send_color_to_pid`] and its deliberate opposite in one respect: an editor's
/// pick is ADDRESSED, because it must reach the one editor that asked for it, while a
/// general pick is SEARCHED for, because "the colour picker window" is a singleton the user
/// thinks of as one thing regardless of which process happens to own it this time.
///
/// Returns the pid that took it. `Err` for every failure, including the ordinary ones where
/// no window is open or the open one is mid-close, and the caller's only correct response is
/// the same in all of them: open a window of its own, exactly as it did before this existed.
///
/// `palette` (DRAGON-687) is the pipette-to-palette launch's nonce, `None` for the
/// ordinary pick; see [`Request::Color`]'s field for why it is a nonce. On any failure of
/// a TAGGED send the caller degrades to the ordinary pick flow, so the tag can only ever
/// lose the filing shortcut, never the colour.
#[cfg(unix)]
pub fn send_color_to_picker(rgb: [u8; 3], palette: Option<u64>) -> Result<u32, HandoffError> {
    let windows = crate::instance::live_color_picker_windows();
    if windows.is_empty() {
        return Err(HandoffError::NoHost);
    }
    let req = Request::Color { to: ColorDest::PickerWindow, rgb, palette };
    let mut last: Option<String> = None;
    for pid in windows {
        let path = crate::instance::color_picker_socket_path(pid);
        // A marker with no socket is not a listening window at all (a picker from an older
        // build, or one whose bind failed) — skip without counting it as a refusal.
        if !std::path::Path::new(&path).exists() {
            continue;
        }
        match send_to_socket(&path, &req) {
            Ok(()) => return Ok(pid),
            Err(why) => {
                log::info!("color handoff to picker window at pid {pid} failed: {why}");
                last = Some(why);
            }
        }
    }
    match last {
        Some(why) => Err(HandoffError::NotAccepted(why)),
        None => Err(HandoffError::NoHost),
    }
}

/// The Windows arm of [`send_color_to_picker`] (DRAGON-651): the same searched delivery —
/// the discovery scan is the SAME portable marker scan — over each candidate's per-pid
/// named pipe. A "nobody listening" connect is skipped without counting as a refusal,
/// exactly the skip the unix arm gives a marker with no socket file beside it.
#[cfg(windows)]
pub fn send_color_to_picker(rgb: [u8; 3], palette: Option<u64>) -> Result<u32, HandoffError> {
    use crate::platform::windows::preview_pipe::SendError;
    let windows = crate::instance::live_color_picker_windows();
    if windows.is_empty() {
        return Err(HandoffError::NoHost);
    }
    let req = Request::Color { to: ColorDest::PickerWindow, rgb, palette };
    let mut last: Option<String> = None;
    for pid in windows {
        match send_to_pipe(&crate::instance::color_picker_host_address(pid), &req) {
            Ok(()) => return Ok(pid),
            // A marker whose pipe no longer answers is not a listening window at all (its
            // owner is mid-exit, or its bind failed) — skip without counting it as a
            // refusal.
            Err(SendError::NoListener) => continue,
            Err(SendError::Failed(why)) => {
                log::info!("color handoff to picker window at pid {pid} failed: {why}");
                last = Some(why);
            }
        }
    }
    match last {
        Some(why) => Err(HandoffError::NotAccepted(why)),
        None => Err(HandoffError::NoHost),
    }
}

/// No-transport stub (neither unix sockets nor Windows named pipes): every pick opens its
/// own window there. Deliberately a stub and not a pretend success — see
/// [`HandoffError::Unsupported`].
#[cfg(not(any(unix, windows)))]
pub fn send_color_to_picker(_rgb: [u8; 3], _palette: Option<u64>) -> Result<u32, HandoffError> {
    Err(HandoffError::Unsupported)
}

/// Ask the EXISTING colour picker window to come forward (DRAGON-680), wherever it is.
///
/// The palette viewer's answer to the single-window rule, and the same SEARCHED discovery
/// [`send_color_to_picker`] uses, over the same per-pid sockets, because it is the same
/// singleton being addressed. Returns the pid that took it.
///
/// `Err` for every failure, including the ordinary ones (no window open, one mid-close, an
/// older build that does not know the verb), and the caller's response is the same in all of
/// them: open a window of its own. Nothing here may be treated as a lost request; a viewer
/// launch that raises nothing and opens nothing would simply look broken.
#[cfg(unix)]
pub fn send_raise_to_picker() -> Result<u32, HandoffError> {
    let windows = crate::instance::live_color_picker_windows();
    if windows.is_empty() {
        return Err(HandoffError::NoHost);
    }
    let mut last: Option<String> = None;
    for pid in windows {
        let path = crate::instance::color_picker_socket_path(pid);
        // A marker with no socket is not a listening window at all, so skip it without counting
        // it as a refusal, exactly as the colour sender does.
        if !std::path::Path::new(&path).exists() {
            continue;
        }
        match send_to_socket(&path, &Request::Raise) {
            Ok(()) => return Ok(pid),
            Err(why) => {
                log::info!("raise of the picker window at pid {pid} failed: {why}");
                last = Some(why);
            }
        }
    }
    match last {
        Some(why) => Err(HandoffError::NotAccepted(why)),
        None => Err(HandoffError::NoHost),
    }
}

/// The Windows arm of [`send_raise_to_picker`]: the same searched discovery over each
/// candidate's per-pid named pipe, with a "nobody listening" connect skipped rather than
/// counted as a refusal.
#[cfg(windows)]
pub fn send_raise_to_picker() -> Result<u32, HandoffError> {
    use crate::platform::windows::preview_pipe::SendError;
    let windows = crate::instance::live_color_picker_windows();
    if windows.is_empty() {
        return Err(HandoffError::NoHost);
    }
    let mut last: Option<String> = None;
    for pid in windows {
        match send_to_pipe(&crate::instance::color_picker_host_address(pid), &Request::Raise) {
            Ok(()) => return Ok(pid),
            Err(SendError::NoListener) => continue,
            Err(SendError::Failed(why)) => {
                log::info!("raise of the picker window at pid {pid} failed: {why}");
                last = Some(why);
            }
        }
    }
    match last {
        Some(why) => Err(HandoffError::NotAccepted(why)),
        None => Err(HandoffError::NoHost),
    }
}

/// No-transport stub: the palette viewer opens its own window there, exactly as it does
/// when nothing is listening.
#[cfg(not(any(unix, windows)))]
pub fn send_raise_to_picker() -> Result<u32, HandoffError> {
    Err(HandoffError::Unsupported)
}

/// The Windows arm of [`send_to_host`] (DRAGON-651): the same discovery scan and the same
/// preference order, over each candidate's per-pid preview pipe.
#[cfg(windows)]
pub fn send_to_host(req: &OpenRequest) -> Result<u32, HandoffError> {
    use crate::platform::windows::preview_pipe::SendError;
    let hosts = crate::instance::live_preview_hosts();
    if hosts.is_empty() {
        return Err(HandoffError::NoHost);
    }
    let mut last: Option<String> = None;
    let req = Request::Open(req.clone());
    for pid in hosts {
        match send_to_pipe(&crate::instance::preview_host_address(pid), &req) {
            Ok(()) => return Ok(pid),
            // A marker whose pipe does not answer isn't a host at all (an older build's
            // preview, or a failed bind) — skip without counting it as a refusal.
            Err(SendError::NoListener) => continue,
            Err(SendError::Failed(why)) => {
                log::info!("preview handoff to pid {pid} failed: {why}");
                last = Some(why);
            }
        }
    }
    match last {
        Some(why) => Err(HandoffError::NotAccepted(why)),
        None => Err(HandoffError::NoHost),
    }
}

/// No-transport stub (neither unix sockets nor Windows named pipes): a capture always
/// opens its own preview there. The CHILD side is the portable one on purpose — it sits
/// in the shared capture-finish path, so it must compile and answer honestly everywhere.
#[cfg(not(any(unix, windows)))]
pub fn send_to_host(_req: &OpenRequest) -> Result<u32, HandoffError> {
    Err(HandoffError::Unsupported)
}

/// Whether THIS build can host preview handoffs at all, so a caller can branch without
/// a `cfg`. `true` on unix (sockets) and on Windows (named pipes, DRAGON-651); `false`
/// only where neither transport exists, where the host half ([`PreviewHost`] /
/// [`start_host`]) isn't compiled and the child-side senders report
/// [`HandoffError::Unsupported`].
pub const fn host_supported() -> bool {
    cfg!(any(unix, windows))
}

/// One handoff attempt against ONE socket path: connect, send, and WAIT for the host's
/// `ok`. `Err(reason)` for every other outcome — including EOF (the host closed without
/// acking, i.e. it was exiting) and timeout (wedged host). Split out from
/// [`send_to_host`] so the whole exchange is testable against a socket in a temp dir,
/// with no dependency on the real runtime dir.
#[cfg(unix)]
fn send_to_socket(path: &str, req: &Request) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;

    let mut conn = UnixStream::connect(path).map_err(|e| format!("connect: {e}"))?;
    ensure_cloexec(&conn).map_err(|e| format!("cloexec: {e}"))?;
    let _ = conn.set_write_timeout(Some(WRITE_TIMEOUT));
    conn.set_read_timeout(Some(ACK_TIMEOUT)).map_err(|e| format!("timeout: {e}"))?;
    conn.write_all(req.encode().as_bytes()).map_err(|e| format!("write: {e}"))?;
    conn.flush().map_err(|e| format!("flush: {e}"))?;

    let reader = conn.try_clone().map_err(|e| format!("clone: {e}"))?;
    let line = read_line(reader).map_err(|e| format!("no ack: {e}"))?;
    match Reply::parse(&line) {
        Some(Reply::Accepted) => Ok(()),
        Some(Reply::Rejected(r)) => Err(format!("rejected ({})", r.as_str())),
        None => Err(format!("unrecognised reply {:?}", line.trim())),
    }
}

/// One handoff attempt against ONE pipe name — [`send_to_socket`]'s Windows twin
/// (DRAGON-651). The connect/send/read-ack syscalls are the plugin's
/// (`platform::windows::preview_pipe::exchange`, bounded by [`ACK_TIMEOUT`]); the reply
/// PARSE stays here so both transports read the same words the same way. The "nobody is
/// listening" case keeps its own variant because a pipe has no socket file whose absence
/// pre-answers it — the callers map it to [`HandoffError::NoHost`] / a skip.
#[cfg(windows)]
fn send_to_pipe(
    address: &str,
    req: &Request,
) -> Result<(), crate::platform::windows::preview_pipe::SendError> {
    use crate::platform::windows::preview_pipe::{SendError, exchange};
    let line = exchange(address, &req.encode(), ACK_TIMEOUT)?;
    match Reply::parse(&line) {
        Some(Reply::Accepted) => Ok(()),
        Some(Reply::Rejected(r)) => Err(SendError::Failed(format!("rejected ({})", r.as_str()))),
        None => Err(SendError::Failed(format!("unrecognised reply {:?}", line.trim()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> OpenRequest {
        OpenRequest {
            path: PathBuf::from("/home/u/Pictures/Screenshot 2026-07-27.png"),
            video: false,
            display_dims: Some((2560, 1440)),
            source_scale: 1.0,
            external: false,
            size: Some(1_234_567),
        }
    }

    #[test]
    fn request_round_trips_every_optional_shape() {
        for dims in [None, Some((1, 1)), Some((3840, 2160))] {
            for size in [None, Some(0), Some(u64::MAX)] {
                for video in [false, true] {
                    for external in [false, true] {
                        for scale in [1.0f32, 2.0, 1.25, 0.5] {
                            let req = OpenRequest {
                                path: PathBuf::from("/tmp/a.mp4"),
                                video,
                                display_dims: dims,
                                source_scale: scale,
                                external,
                                size,
                            };
                            let line = req.encode();
                            assert!(line.ends_with('\n'), "must be newline-terminated: {line:?}");
                            assert_eq!(OpenRequest::parse(&line), Ok(req.clone()), "round trip");
                            // Line readers hand us the line with the newline stripped.
                            assert_eq!(OpenRequest::parse(line.trim_end()), Ok(req));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn paths_with_spaces_newlines_and_non_utf8_survive() {
        // The exact reason `path` is hex: none of these survive whitespace splitting,
        // and the last one isn't even a `String`.
        //
        // The `mut` is used by the `cases.push` below, which is `cfg(unix)` only (a
        // non-UTF-8 path cannot be built from bytes on Windows). So off unix the binding
        // is honestly immutable, and the attribute says exactly where rather than
        // blanketing the file. Windows holds a ZERO-warning clippy bar under
        // `--all-targets`, which is where this surfaced.
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut cases = vec![
            PathBuf::from("/home/u/My Pictures/shot 1.png"),
            PathBuf::from("/tmp/we\nird.png"),
            PathBuf::from("/tmp/tab\there=now.png"),
            PathBuf::from("/tmp/émoji 🐉.png"),
        ];
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            cases.push(PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/\xff\xfe.png")));
        }
        for path in cases {
            let req = OpenRequest { path: path.clone(), ..sample() };
            let parsed = OpenRequest::parse(&req.encode()).expect("round trip");
            assert_eq!(parsed.path, path);
        }
    }

    #[test]
    fn version_mismatch_is_reported_as_such_not_as_garbage() {
        let line = sample().encode();
        let bumped = line.replacen(
            &format!("{MAGIC} {PROTOCOL_VERSION} "),
            &format!("{MAGIC} {} ", PROTOCOL_VERSION + 1),
            1,
        );
        assert_eq!(
            OpenRequest::parse(&bumped),
            Err(WireError::Version(PROTOCOL_VERSION + 1))
        );
        // The version is checked BEFORE the verb + fields, so a future line whose whole
        // shape differs still reports the version, never a confusing parse failure.
        assert_eq!(
            OpenRequest::parse(&format!("{MAGIC} 99 whatever a=b c=d")),
            Err(WireError::Version(99))
        );
        // ...and a non-numeric version is plain garbage.
        assert_eq!(OpenRequest::parse(&format!("{MAGIC} v2 open")), Err(WireError::Malformed));
    }

    #[test]
    fn request_parse_rejects_malformed() {
        let ok = sample().encode();
        let bad = [
            String::new(),
            "   \n".to_string(),
            "cck-recording 1 open".to_string(),          // wrong magic
            format!("{MAGIC} {PROTOCOL_VERSION}"),                        // no verb
            format!("{MAGIC} {PROTOCOL_VERSION} close path=2f61"),        // wrong verb
            format!("{MAGIC} {PROTOCOL_VERSION} open"),                   // no fields
            ok.replace("video=0 ", ""),                  // missing field
            ok.replace("path=", "pth="),                 // unknown key (and missing path)
            format!("{ok} extra=1").replace('\n', ""),   // unknown key alongside a full set
            ok.replace("video=0", "video=0 video=1"),    // duplicate key
            ok.replace("video=0", "video=2"),            // non-bit
            ok.replace("external=0", "external=yes"),    // non-bit
            ok.replace("dims=2560x1440", "dims=2560"),   // malformed dims
            ok.replace("dims=2560x1440", "dims=axb"),    // malformed dims
            ok.replace("scale=1", "scale=0"),            // non-positive scale
            ok.replace("scale=1", "scale=-2"),           // negative scale
            ok.replace("scale=1", "scale=NaN"),          // non-finite scale
            ok.replace("scale=1", "scale=inf"),          // non-finite scale
            ok.replace("size=1234567", "size=-1"),       // negative size
            ok.replace("size=1234567", "size=lots"),     // non-numeric size
            "video=0 path=2f61".to_string(),             // fields with no header
        ];
        for line in bad {
            assert_eq!(
                OpenRequest::parse(&line),
                Err(WireError::Malformed),
                "should reject {line:?}"
            );
        }
        // A hex path that isn't hex (odd length / non-hex digit) is malformed too.
        for hex in ["abc", "zz", "2f6g"] {
            let line = format!(
                "{MAGIC} {PROTOCOL_VERSION} open path={hex} video=0 dims=- scale=1 external=0 size=-"
            );
            assert_eq!(OpenRequest::parse(&line), Err(WireError::Malformed), "{hex}");
        }
    }

    /// DRAGON-587's verb, carrying DRAGON-613's destination and DRAGON-687's palette
    /// target: it round-trips for BOTH consumers, tagged and untagged, and its field set
    /// is as closed as `open`'s.
    #[test]
    fn the_color_verb_round_trips_and_is_strict() {
        for to in [ColorDest::Editor, ColorDest::PickerWindow] {
            for rgb in [[0, 0, 0], [255, 255, 255], [255, 136, 0], [1, 2, 3]] {
                let req = Request::Color { to, rgb, palette: None };
                let line = req.encode();
                assert!(line.ends_with('\n'));
                assert_eq!(Request::parse(&line), Ok(req.clone()), "round trip {to:?} {rgb:?}");
                assert_eq!(Request::parse(line.trim_end()), Ok(req));
            }
        }
        // The palette tag round-trips too, at both ends of its range.
        for nonce in [1u64, u64::MAX] {
            let req = Request::Color {
                to: ColorDest::PickerWindow,
                rgb: [255, 136, 0],
                palette: Some(nonce),
            };
            assert_eq!(Request::parse(&req.encode()), Ok(req), "nonce {nonce}");
        }
        assert_eq!(
            Request::Color { to: ColorDest::Editor, rgb: [255, 136, 0], palette: None }
                .encode()
                .trim_end(),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=editor rgb=ff8800 palette=-")
        );
        assert_eq!(
            Request::Color {
                to: ColorDest::PickerWindow,
                rgb: [255, 136, 0],
                palette: Some(7)
            }
            .encode()
            .trim_end(),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=picker rgb=ff8800 palette=7")
        );
        // Order-independent, exactly like `open`.
        assert_eq!(
            Request::parse(&format!(
                "{MAGIC} {PROTOCOL_VERSION} color palette=- rgb=ff8800 to=picker"
            )),
            Ok(Request::Color { to: ColorDest::PickerWindow, rgb: [255, 136, 0], palette: None })
        );
        for bad in [
            format!("{MAGIC} {PROTOCOL_VERSION} color"),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=picker palette=-"), // no colour
            format!("{MAGIC} {PROTOCOL_VERSION} color rgb=ff8800 palette=-"), // no destination
            format!("{MAGIC} {PROTOCOL_VERSION} color to=picker rgb=ff8800"), // no palette key
            format!("{MAGIC} {PROTOCOL_VERSION} color to= rgb=ff8800 palette=-"),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=nobody rgb=ff8800 palette=-"),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=Editor rgb=ff8800 palette=-"),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=picker rgb= palette=-"),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=picker rgb=ff88 palette=-"),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=picker rgb=ff8800ff palette=-"),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=picker rgb=gg8800 palette=-"),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=picker hex=ff8800 palette=-"),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=picker rgb=ff8800 rgb=000000 palette=-"),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=editor to=picker rgb=ff8800 palette=-"),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=picker rgb=ff8800 palette=- extra=1"),
            format!("{MAGIC} {PROTOCOL_VERSION} colour to=picker rgb=ff8800 palette=-"),
            // The palette tag's own strictness (DRAGON-687): a zero nonce is a constructed
            // line (the window never mints one), junk is junk, duplicates are duplicates,
            // and an EDITOR line carrying a target is malformed rather than ignored (an
            // editor has no palettes).
            format!("{MAGIC} {PROTOCOL_VERSION} color to=picker rgb=ff8800 palette=0"),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=picker rgb=ff8800 palette=abc"),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=picker rgb=ff8800 palette=-1"),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=picker rgb=ff8800 palette=1 palette=2"),
            format!("{MAGIC} {PROTOCOL_VERSION} color to=editor rgb=ff8800 palette=1"),
        ] {
            assert_eq!(Request::parse(&bad), Err(WireError::Malformed), "should reject {bad:?}");
        }
        // The version is still checked FIRST, before the verb.
        assert_eq!(
            Request::parse(&format!("{MAGIC} 99 color to=picker rgb=ff8800 palette=-")),
            Err(WireError::Version(99))
        );
    }

    /// DRAGON-613, and the case a user mid-upgrade actually IS: a v2 peer and a v3 peer.
    ///
    /// The old `color` line has no destination, so a new host must not half-understand it,
    /// and the version check must be what catches it rather than the field parser, so the
    /// peer learns WHY. Both directions land on `err version`, which every sender in this
    /// module reads as "not accepted" and answers by doing the job itself.
    #[test]
    fn a_v2_color_line_is_refused_by_version_not_misparsed() {
        let v2 = format!("{MAGIC} 2 color rgb=ff8800");
        assert_eq!(Request::parse(&v2), Err(WireError::Version(2)));
        // And the reverse direction: a v2 host reading our v3 line sees the same shape, so
        // pin that our line still carries a plain integer version a v2 parser can read.
        let v3 = Request::Color { to: ColorDest::PickerWindow, rgb: [255, 136, 0], palette: None }
            .encode();
        let mut it = v3.split_whitespace();
        assert_eq!(it.next(), Some(MAGIC));
        assert_eq!(it.next().and_then(|v| v.parse::<u32>().ok()), Some(PROTOCOL_VERSION));
    }

    /// DRAGON-680: the `raise` verb round-trips, carries nothing, and stays strict.
    ///
    /// The strictness is the part worth pinning. A fieldless verb has no key parser to
    /// reject an unknown key, so the only thing standing between it and silently accepting
    /// `raise rgb=ff8800` is the trailing-token check. A host that ignored the extra would
    /// be exactly the half-understood line the version discipline exists to prevent.
    #[test]
    fn the_raise_verb_round_trips_and_carries_nothing() {
        let line = Request::Raise.encode();
        assert_eq!(line, format!("{MAGIC} {PROTOCOL_VERSION} raise\n"));
        assert_eq!(Request::parse(&line), Ok(Request::Raise));
        assert_eq!(Request::parse(line.trim_end()), Ok(Request::Raise));
        for bad in [
            format!("{MAGIC} {PROTOCOL_VERSION} raise to=picker"),
            format!("{MAGIC} {PROTOCOL_VERSION} raise rgb=ff8800"),
            format!("{MAGIC} {PROTOCOL_VERSION} raise junk"),
        ] {
            assert_eq!(Request::parse(&bad), Err(WireError::Malformed), "should reject {bad:?}");
        }
        // The version is checked before the verb here too, so an older host answers a
        // raise with `err version` and the viewer opens its own window.
        assert_eq!(
            Request::parse(&format!("{MAGIC} 3 raise")),
            Err(WireError::Version(3))
        );
    }

    /// The destination words are the wire, so they are pinned literally: changing one
    /// silently breaks every mixed pair without changing the version.
    #[test]
    fn the_destination_words_round_trip() {
        for dest in [ColorDest::Editor, ColorDest::PickerWindow] {
            assert_eq!(ColorDest::parse(dest.as_str()), Some(dest));
        }
        assert_eq!(ColorDest::Editor.as_str(), "editor");
        assert_eq!(ColorDest::PickerWindow.as_str(), "picker");
        for junk in ["", "Editor", "PICKER", "window", "preview", "0", "1"] {
            assert_eq!(ColorDest::parse(junk), None, "{junk}");
        }
    }

    /// Both verbs share one parser entry point, and an `open` line still parses exactly as
    /// it did: the second verb must not have loosened the first.
    #[test]
    fn the_open_verb_is_unchanged_by_the_second_one() {
        let line = sample().encode();
        assert_eq!(Request::parse(&line), Ok(Request::Open(sample())));
        assert_eq!(OpenRequest::parse(&line), Ok(sample()));
        // And a `color` line is not an `open` one.
        let colour =
            Request::Color { to: ColorDest::Editor, rgb: [1, 2, 3], palette: None }.encode();
        assert_eq!(OpenRequest::parse(&colour), Err(WireError::Malformed));
    }

    #[test]
    fn hex_round_trips_and_rejects_junk() {
        for bytes in [&b""[..], &b"\x00\x01\xfe\xff"[..], &b"hello"[..]] {
            assert_eq!(hex_decode(&hex_encode(bytes)).as_deref(), Some(bytes));
        }
        assert_eq!(hex_encode(&[0x0a, 0xff]), "0aff");
        assert_eq!(hex_decode("0AFF"), Some(vec![0x0a, 0xff])); // uppercase tolerated
        assert_eq!(hex_decode("f"), None); // odd length
        assert_eq!(hex_decode("gg"), None); // non-hex
        assert_eq!(hex_decode("0 f"), None);
    }

    #[test]
    fn reply_round_trips_and_rejects_junk() {
        let replies = [
            Reply::Accepted,
            Reply::Rejected(RejectReason::Version),
            Reply::Rejected(RejectReason::Parse),
            Reply::Rejected(RejectReason::Busy),
        ];
        for r in replies {
            let line = r.encode();
            assert!(line.ends_with('\n'));
            assert_eq!(Reply::parse(&line), Some(r), "round trip {r:?}");
            assert_eq!(Reply::parse(line.trim_end()), Some(r));
        }
        // Anything unrecognised must NOT read as an acceptance.
        for junk in ["", "\n", "okay", "ok extra", "err", "err nope", "err busy extra", "1"] {
            assert_ne!(Reply::parse(junk), Some(Reply::Accepted), "{junk:?}");
        }
    }

    #[test]
    fn a_capped_line_cannot_grow_without_bound() {
        // MAX_LINE is the read cap; prove the constant is the one `read_line` uses by
        // feeding an over-long unterminated "line" and checking the truncation.
        #[cfg(unix)]
        {
            let huge = vec![b'a'; (MAX_LINE as usize) + 4096];
            let got = read_line(&huge[..]).expect("reads up to the cap");
            assert_eq!(got.len(), MAX_LINE as usize);
            assert_eq!(OpenRequest::parse(&got), Err(WireError::Malformed));
        }
    }

    // ── Socket-level tests (a real Unix socket pair in a temp dir; no compositor,
    // D-Bus, GPU or runtime-dir dependency) ─────────────────────────────────────

    #[cfg(unix)]
    fn temp_socket(tag: &str) -> String {
        let dir = std::env::temp_dir().join(format!(
            "cck-preview-ipc-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir.join("host.sock").to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn accepted_handoff_reports_success_to_the_child() {
        use std::os::unix::net::UnixListener;

        let path = temp_socket("accept");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind");
        assert!(
            ensure_cloexec(&listener).is_ok(),
            "listener must carry FD_CLOEXEC"
        );
        let (tx, rx) = std::sync::mpsc::channel::<Handoff>();
        // Detached, like the real host thread: it dies with the process.
        std::thread::spawn(move || accept_loop(&listener, &tx));

        let req = Request::Open(sample());
        let child_path = path.clone();
        let child_req = req.clone();
        let child = std::thread::spawn(move || send_to_socket(&child_path, &child_req));

        // The host's "update loop": take the request, then ack.
        let handoff = rx.recv_timeout(Duration::from_secs(5)).expect("handoff arrives");
        assert_eq!(handoff.request(), &req, "the request survives the wire");
        handoff.accept();

        assert!(child.join().unwrap().is_ok(), "child must see the ack");
        let _ = std::fs::remove_file(&path);
    }

    /// One sample colour message, so the round trip asserts the DESTINATION as well as the
    /// colour without repeating the literal at both ends of the socket.
    #[cfg(unix)]
    const COLOR_TO_EDITOR: Request =
        Request::Color { to: ColorDest::Editor, rgb: [255, 136, 0], palette: None };

    /// DRAGON-587 end to end: a picked colour crosses the SAME socket and is acknowledged
    /// the same way, so the editor's pipette reaches the editor that asked.
    #[cfg(unix)]
    #[test]
    fn a_picked_color_reaches_the_host_and_is_acked() {
        use std::os::unix::net::UnixListener;

        let path = temp_socket("color");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind");
        let (tx, rx) = std::sync::mpsc::channel::<Handoff>();
        std::thread::spawn(move || accept_loop(&listener, &tx));

        let child_path = path.clone();
        let child = std::thread::spawn(move || {
            send_to_socket(&child_path, &COLOR_TO_EDITOR)
        });

        let handoff = rx.recv_timeout(Duration::from_secs(5)).expect("the colour arrives");
        assert_eq!(handoff.request(), &COLOR_TO_EDITOR, "the exact colour and destination");
        handoff.accept();
        assert!(child.join().unwrap().is_ok(), "the picker must see the ack");
        let _ = std::fs::remove_file(&path);
    }

    /// A host with no document to put a colour on refuses it, and the picker must read that
    /// as "not delivered" so it shows its own result window instead of losing the pick.
    #[cfg(unix)]
    #[test]
    fn a_refused_color_is_not_a_delivery() {
        use std::os::unix::net::UnixListener;

        let path = temp_socket("color-refused");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind");
        let (tx, rx) = std::sync::mpsc::channel::<Handoff>();
        std::thread::spawn(move || accept_loop(&listener, &tx));

        let child_path = path.clone();
        let child = std::thread::spawn(move || {
            send_to_socket(
                &child_path,
                &Request::Color { to: ColorDest::Editor, rgb: [1, 2, 3], palette: None },
            )
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the colour arrives")
            .reject(RejectReason::Busy);
        assert!(child.join().unwrap().is_err(), "a rejection is never a delivery");
        let _ = std::fs::remove_file(&path);
    }

    /// THE failure mode that must never lose a capture: the host is on its way out as the
    /// child connects. Dropping the `Handoff` (no ack) closes the connection, and the
    /// child must report failure so it opens its own preview.
    #[cfg(unix)]
    #[test]
    fn host_exiting_mid_connect_makes_the_child_fall_back() {
        use std::os::unix::net::UnixListener;

        let path = temp_socket("exiting");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind");
        let (tx, rx) = std::sync::mpsc::channel::<Handoff>();
        std::thread::spawn(move || accept_loop(&listener, &tx));

        let child_path = path.clone();
        let child = std::thread::spawn(move || send_to_socket(&child_path, &Request::Open(sample())));

        let handoff = rx.recv_timeout(Duration::from_secs(5)).expect("handoff arrives");
        drop(handoff); // the host dies / gives up before acking

        let err = child.join().unwrap().expect_err("child must NOT think it was accepted");
        assert!(err.contains("no ack"), "{err}");
        let _ = std::fs::remove_file(&path);
    }

    /// A host that drops its RECEIVER (stopped hosting) is the same story one level up:
    /// the accept loop's send fails, the handoff drops unacked, the child falls back.
    #[cfg(unix)]
    #[test]
    fn dropped_receiver_makes_the_child_fall_back() {
        use std::os::unix::net::UnixListener;

        let path = temp_socket("dropped-rx");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind");
        let (tx, rx) = std::sync::mpsc::channel::<Handoff>();
        drop(rx); // the host has stopped hosting
        std::thread::spawn(move || accept_loop(&listener, &tx));

        let err = send_to_socket(&path, &Request::Open(sample())).expect_err("must not report acceptance");
        assert!(err.contains("no ack"), "{err}");
        let _ = std::fs::remove_file(&path);
    }

    /// An explicit rejection reaches the child immediately (no ACK_TIMEOUT wait).
    #[cfg(unix)]
    #[test]
    fn explicit_rejection_reaches_the_child() {
        use std::os::unix::net::UnixListener;

        let path = temp_socket("reject");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind");
        let (tx, rx) = std::sync::mpsc::channel::<Handoff>();
        std::thread::spawn(move || accept_loop(&listener, &tx));

        let child_path = path.clone();
        let child = std::thread::spawn(move || send_to_socket(&child_path, &Request::Open(sample())));
        rx.recv_timeout(Duration::from_secs(5))
            .expect("handoff arrives")
            .reject(RejectReason::Busy);

        let err = child.join().unwrap().expect_err("a rejection is not an acceptance");
        assert!(err.contains("busy"), "{err}");
        let _ = std::fs::remove_file(&path);
    }

    /// A version-mismatched child gets `err version` from the accept loop itself — the
    /// request never reaches the host's update loop.
    #[cfg(unix)]
    #[test]
    fn version_mismatch_is_refused_by_the_accept_loop() {
        use std::io::Write as _;
        use std::os::unix::net::{UnixListener, UnixStream};

        let path = temp_socket("version");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind");
        let (tx, rx) = std::sync::mpsc::channel::<Handoff>();
        std::thread::spawn(move || accept_loop(&listener, &tx));

        let mut conn = UnixStream::connect(&path).expect("connect");
        conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let future = sample().encode().replacen(
            &format!("{MAGIC} {PROTOCOL_VERSION} "),
            &format!("{MAGIC} {} ", PROTOCOL_VERSION + 1),
            1,
        );
        conn.write_all(future.as_bytes()).unwrap();
        conn.flush().unwrap();
        let line = read_line(&conn).expect("a reply");
        assert_eq!(Reply::parse(&line), Some(Reply::Rejected(RejectReason::Version)));
        assert!(
            rx.try_recv().is_err(),
            "a version-mismatched line must never reach the update loop"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Garbage on the wire is refused, and the accept loop KEEPS SERVING: the next
    /// (valid) child still gets through.
    #[cfg(unix)]
    #[test]
    fn garbage_is_refused_and_the_loop_keeps_serving() {
        use std::io::Write as _;
        use std::os::unix::net::{UnixListener, UnixStream};

        let path = temp_socket("garbage");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind");
        let (tx, rx) = std::sync::mpsc::channel::<Handoff>();
        std::thread::spawn(move || accept_loop(&listener, &tx));

        let mut conn = UnixStream::connect(&path).expect("connect");
        conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        conn.write_all(b"hello there\n").unwrap();
        conn.flush().unwrap();
        let line = read_line(&conn).expect("a reply");
        assert_eq!(Reply::parse(&line), Some(Reply::Rejected(RejectReason::Parse)));
        drop(conn);

        let child_path = path.clone();
        let child = std::thread::spawn(move || send_to_socket(&child_path, &Request::Open(sample())));
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the loop still serves")
            .accept();
        assert!(child.join().unwrap().is_ok());
        let _ = std::fs::remove_file(&path);
    }

    /// No socket at the path at all (the host is long gone, only a stale marker remains):
    /// connect fails fast and the child falls back.
    #[cfg(unix)]
    #[test]
    fn a_missing_socket_fails_fast() {
        let path = temp_socket("missing");
        let _ = std::fs::remove_file(&path);
        let err = send_to_socket(&path, &Request::Open(sample())).expect_err("nothing is listening");
        assert!(err.starts_with("connect:"), "{err}");
    }

    /// An accepted connection's fds must be CLOEXEC on BOTH sides, so a handoff can never
    /// be inherited by a spawned child (ffmpeg) and hold the ack channel open.
    #[cfg(unix)]
    #[test]
    fn both_ends_of_a_connection_are_cloexec() {
        use rustix::io::{FdFlags, fcntl_getfd};
        use std::os::unix::net::{UnixListener, UnixStream};

        let path = temp_socket("cloexec");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind");
        ensure_cloexec(&listener).unwrap();
        assert!(fcntl_getfd(&listener).unwrap().contains(FdFlags::CLOEXEC));

        let client = UnixStream::connect(&path).expect("connect");
        ensure_cloexec(&client).unwrap();
        assert!(fcntl_getfd(&client).unwrap().contains(FdFlags::CLOEXEC));

        let (server, _) = listener.accept().expect("accept");
        ensure_cloexec(&server).unwrap();
        assert!(fcntl_getfd(&server).unwrap().contains(FdFlags::CLOEXEC));
        let _ = std::fs::remove_file(&path);
    }

    /// The host's socket path is per-pid, lives beside that pid's preview marker, and is
    /// unlinked when the `PreviewHost` drops (a host that stopped hosting stops being
    /// discoverable at once).
    #[cfg(unix)]
    #[test]
    fn start_host_binds_a_per_pid_socket_and_unlinks_on_drop() {
        let host = match start_host() {
            Ok(h) => h,
            // A runtime dir we can't bind in (exotic sandbox) is a legitimate "no host";
            // don't fail the suite for it.
            Err(e) => {
                eprintln!("skipping: cannot bind in the runtime dir ({e})");
                return;
            }
        };
        let path = host.address().to_string();
        assert!(
            path.ends_with(&format!("cosmic-capture-kit.{}.preview.sock", std::process::id())),
            "{path}"
        );
        assert!(std::path::Path::new(&path).exists(), "socket must exist while hosting");

        // A real end-to-end handoff over the REAL host path.
        let req = Request::Open(sample());
        let send_path = path.clone();
        let send_req = req.clone();
        let child = std::thread::spawn(move || send_to_socket(&send_path, &send_req));
        let handoff = host.requests.recv_timeout(Duration::from_secs(5)).expect("handoff");
        assert_eq!(handoff.request(), &req);
        handoff.accept();
        assert!(child.join().unwrap().is_ok());

        drop(host);
        assert!(
            !std::path::Path::new(&path).exists(),
            "dropping the host must unlink its socket"
        );
    }

    // ── Pipe-level tests (DRAGON-651: the Windows transport, a real named pipe under a
    // test-only name; no compositor, GPU or runtime-dir dependency) ──────────────

    /// A test-only pipe name, per-pid + per-case so parallel tests never share one.
    #[cfg(windows)]
    fn test_pipe(tag: &str) -> String {
        format!(r"\\.\pipe\cck-preview-ipc-test-{}-{tag}", std::process::id())
    }

    #[cfg(windows)]
    use crate::platform::windows::preview_pipe::SendError;

    /// The whole Windows exchange end-to-end: bind the pipe host, send a colour, ack it —
    /// the pipe twin of `a_picked_color_reaches_the_host_and_is_acked`.
    #[cfg(windows)]
    #[test]
    fn a_picked_color_crosses_the_pipe_and_is_acked() {
        let host = start_host_at(test_pipe("color")).expect("bind the pipe");
        let addr = host.address().to_string();
        let child = std::thread::spawn(move || send_to_pipe(&addr, &COLOR_TO_EDITOR));
        let handoff = host.requests.recv_timeout(Duration::from_secs(5)).expect("the colour arrives");
        assert_eq!(handoff.request(), &COLOR_TO_EDITOR, "the exact colour and destination");
        handoff.accept();
        assert!(child.join().unwrap().is_ok(), "the sender must see the ack");
    }

    /// One sample colour message for the pipe tests, like the unix `COLOR_TO_EDITOR`.
    #[cfg(windows)]
    const COLOR_TO_EDITOR: Request =
        Request::Color { to: ColorDest::Editor, rgb: [255, 136, 0], palette: None };

    /// A refusal crosses the pipe as a refusal — never as a delivery.
    #[cfg(windows)]
    #[test]
    fn a_refused_color_on_the_pipe_is_not_a_delivery() {
        let host = start_host_at(test_pipe("refuse")).expect("bind the pipe");
        let addr = host.address().to_string();
        let child = std::thread::spawn(move || send_to_pipe(&addr, &COLOR_TO_EDITOR));
        host.requests
            .recv_timeout(Duration::from_secs(5))
            .expect("the colour arrives")
            .reject(RejectReason::Busy);
        match child.join().unwrap() {
            Err(SendError::Failed(why)) => assert!(why.contains("busy"), "{why}"),
            other => panic!("a rejection is never a delivery: {other:?}"),
        }
    }

    /// THE failure mode that must never lose a pick: the host drops the handoff unacked
    /// (mid-exit). The pipe closes, the sender reads no ack, and it must fall back.
    #[cfg(windows)]
    #[test]
    fn host_exiting_mid_pipe_connect_makes_the_sender_fall_back() {
        let host = start_host_at(test_pipe("exiting")).expect("bind the pipe");
        let addr = host.address().to_string();
        let child = std::thread::spawn(move || send_to_pipe(&addr, &Request::Open(sample())));
        let handoff = host.requests.recv_timeout(Duration::from_secs(5)).expect("handoff arrives");
        drop(handoff); // the host dies / gives up before acking
        match child.join().unwrap() {
            Err(SendError::Failed(why)) => assert!(why.contains("no ack"), "{why}"),
            other => panic!("the sender must NOT think it was accepted: {other:?}"),
        }
    }

    /// Nobody listening at the name: the connect's not-found IS the "no host" answer
    /// (there is no socket file to pre-check on Windows), and it fails fast.
    #[cfg(windows)]
    #[test]
    fn a_missing_pipe_fails_fast_as_no_listener() {
        match send_to_pipe(&test_pipe("missing"), &Request::Open(sample())) {
            Err(SendError::NoListener) => {}
            other => panic!("nothing is listening: {other:?}"),
        }
    }

    /// A version-mismatched sender is refused by the accept loop itself, `err version`,
    /// and the request never reaches the host's update loop.
    #[cfg(windows)]
    #[test]
    fn version_mismatch_is_refused_by_the_pipe_accept_loop() {
        let host = start_host_at(test_pipe("version")).expect("bind the pipe");
        let future = sample().encode().replacen(
            &format!("{MAGIC} {PROTOCOL_VERSION} "),
            &format!("{MAGIC} {} ", PROTOCOL_VERSION + 1),
            1,
        );
        let reply = crate::platform::windows::preview_pipe::exchange(
            host.address(),
            &future,
            Duration::from_secs(5),
        )
        .expect("a reply");
        assert_eq!(Reply::parse(&reply), Some(Reply::Rejected(RejectReason::Version)));
        assert!(
            host.requests.try_recv().is_err(),
            "a version-mismatched line must never reach the update loop"
        );
    }

    /// Dropping the host shuts the accept thread down and frees the NAME: a later sender
    /// finds nobody listening instead of a zombie listener nobody drains.
    #[cfg(windows)]
    #[test]
    fn dropping_the_pipe_host_frees_the_name() {
        let host = start_host_at(test_pipe("drop")).expect("bind the pipe");
        let addr = host.address().to_string();
        drop(host);
        // The wake unsticks the accept thread; give it a bounded moment to close out.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match send_to_pipe(&addr, &COLOR_TO_EDITOR) {
                Err(SendError::NoListener) => return, // the name is gone
                _ => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the pipe name must disappear after the host drops"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    /// Garbage on the pipe is refused (`err parse`) and the loop KEEPS SERVING: the next
    /// (valid) sender still gets through — the pipe twin of the unix garbage test.
    #[cfg(windows)]
    #[test]
    fn pipe_garbage_is_refused_and_the_loop_keeps_serving() {
        let host = start_host_at(test_pipe("garbage")).expect("bind the pipe");
        let reply = crate::platform::windows::preview_pipe::exchange(
            host.address(),
            "hello there\n",
            Duration::from_secs(5),
        )
        .expect("a reply");
        assert_eq!(Reply::parse(&reply), Some(Reply::Rejected(RejectReason::Parse)));

        let addr = host.address().to_string();
        let child = std::thread::spawn(move || send_to_pipe(&addr, &COLOR_TO_EDITOR));
        host.requests
            .recv_timeout(Duration::from_secs(5))
            .expect("the loop still serves")
            .accept();
        assert!(child.join().unwrap().is_ok());
    }
}
