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
//! ## The one rule: never lose the user's capture
//!
//! A handoff is an OPTIMISATION, never a dependency. EVERY failure mode — no host, a
//! host that is exiting, a wedged host, a version-mismatched host, a refused connect, a
//! platform with no transport — surfaces as an `Err` from [`send_to_host`], and the
//! caller's ONLY correct response is to open its own preview exactly as it does today.
//! Nothing here may ever consume a capture without a positive acknowledgement.
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
//! child -> host: cck-preview 1 open path=<hex> video=<0|1> dims=<WxH|-> scale=<f32> external=<0|1> size=<u64|->
//! host  -> child: ok
//!                 err version | err parse | err busy
//! ```
//!
//! * **Versioned**: the `1` is [`PROTOCOL_VERSION`], checked BEFORE the verb, so a
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
//!    preview mint routes through), storing the [`PreviewHost`] in `App::preview_host`. It
//!    binds BEFORE `instance::set_preview_marker(true)`, so a marker a child discovers
//!    always implies a bound socket, and it is idempotent — re-mints keep the one listener.
//!    `App::finish_session` drops it (its `Drop` unlinks the socket) as the process ends.
//! 2. `App::sub_preview_handoff` (`app/subscriptions.rs`) is a 100ms tick gated on
//!    `self.preview_host.is_some()`, emitting `Msg::Capture(CaptureMsg::HandoffPoll)`. The
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
//! Unix (Linux + macOS) get the real transport; both already share the runtime-dir
//! marker convention, so the code is one arm, not two. Windows has no unix sockets: it
//! keeps honest stubs ([`start_host`] returns `Unsupported`, [`send_to_host`] returns
//! `Err(HandoffError::Unsupported)`), so every Windows capture opens its own preview
//! exactly as it does today — byte-identical behaviour. A Windows transport would mirror
//! `daemon_ipc`'s named-pipe split (`daemon_ipc::pipe_name`) behind these same seams.

// The HOST half of this protocol — the request PARSER, the reply writer, the listener and
// everything they reach — is `#[cfg(unix)]`, because unix sockets are the transport. On a
// platform with no transport (Windows) the child side is all that survives, so the parsing
// side is legitimately unreferenced there. Gated honestly per-platform rather than with a
// blanket allow: on unix (Linux + macOS) nothing here is exempt, so a genuinely dead item
// still shows up.
#![cfg_attr(not(unix), allow(dead_code))]

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
pub const PROTOCOL_VERSION: u32 = 1;

/// Longest request/reply line accepted, in bytes. A path is the only unbounded field and
/// hex-doubles, so 64 KiB is ~32 KiB of path — far past any real one, while keeping a
/// garbage (or hostile) sender from making either side allocate without bound.
const MAX_LINE: u64 = 64 * 1024;

/// How long a child waits for the host's `ok` before giving up and opening its own
/// preview. Generous next to the host's ~100ms drain tick, but BOUNDED: a wedged host
/// must cost a capture a short pause, never the capture itself.
pub const ACK_TIMEOUT: Duration = Duration::from_secs(3);

/// How long the child will spend writing its (single, small) request line.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the HOST waits for a connected peer to finish its request line before
/// dropping the connection. Keeps one stalled connector from pinning the accept thread
/// and starving real handoffs.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(2);

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
    // Windows paths are UTF-16; their lossy UTF-8 form round-trips every real path and
    // this arm has no transport behind it anyway (see the module doc's Platforms note).
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
#[cfg(unix)]
fn ensure_cloexec<F: std::os::fd::AsFd>(fd: &F) -> std::io::Result<()> {
    use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};
    let flags = fcntl_getfd(fd)?;
    if !flags.contains(FdFlags::CLOEXEC) {
        fcntl_setfd(fd, flags | FdFlags::CLOEXEC)?;
    }
    Ok(())
}

/// The host's still-open connection back to a waiting child — the ACK half of a
/// [`Handoff`].
///
/// **Dropping an `Ack` without [`accept`](Ack::accept)ing rejects the handoff**: the fd
/// closes, the child reads EOF and opens its own preview. That default is what makes the
/// host-is-exiting race safe (a dropped channel receiver, a dropped `Handoff`, a
/// panicking update, or plain process death all land here), so there is deliberately no
/// `Drop` impl to get wrong — closing the socket IS the rejection.
#[cfg(unix)]
pub struct Ack(Option<std::os::unix::net::UnixStream>);

#[cfg(unix)]
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
#[cfg(unix)]
pub struct Handoff {
    request: OpenRequest,
    ack: Ack,
}

#[cfg(unix)]
impl Handoff {
    /// The capture being handed over.
    pub fn request(&self) -> &OpenRequest {
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
/// Hold one on `App` for as long as any preview window is open. `Drop` unlinks the
/// socket, so a host that has stopped hosting stops being discoverable immediately (the
/// accept thread itself is detached and dies with the process — a one-shot-model process
/// exits when its last preview closes, so there is nothing to join).
#[cfg(unix)]
pub struct PreviewHost {
    path: String,
    /// Inbound handoffs. Drain with `try_recv` from the host's update loop; see the
    /// module doc's integration recipe.
    pub requests: std::sync::mpsc::Receiver<Handoff>,
}

#[cfg(unix)]
impl PreviewHost {
    /// The socket path this host is listening on.
    pub fn socket_path(&self) -> &str {
        &self.path
    }
}

#[cfg(unix)]
impl Drop for PreviewHost {
    fn drop(&mut self) {
        // Best-effort: an unlink failure only leaves a stale socket, which
        // `instance::sweep_stale_markers` clears on the next launch.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Begin hosting preview handoffs for THIS process: bind `{runtime}/cosmic-capture-kit.<pid>.preview.sock`
/// and start the accept thread.
///
/// A leftover socket at our own path (a previous process with the same pid, SIGKILLed)
/// is unlinked first — binding over it is the only way to reclaim the name, and a live
/// process cannot be holding our own pid's path.
///
/// Bind BEFORE `instance::set_preview_marker(true)` so a marker a child discovers always
/// implies a socket that already exists.
#[cfg(unix)]
pub fn start_host() -> std::io::Result<PreviewHost> {
    use std::os::unix::net::UnixListener;

    let path = crate::instance::preview_socket_path(std::process::id());
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
    Ok(PreviewHost { path, requests })
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
        match OpenRequest::parse(&line) {
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
    /// This platform has no preview-handoff transport (Windows today). Constructed ONLY by
    /// the non-unix [`send_to_host`] stub, so on unix it is legitimately unreachable — the
    /// honest platform gate rather than a blanket module allow.
    #[cfg_attr(unix, allow(dead_code))]
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
#[cfg(unix)]
pub fn host_pid() -> Option<u32> {
    crate::instance::live_preview_host()
}

/// Windows stub: with no transport there is never a host to find, so the child's fast-out
/// answers honestly without a `cfg` at the call site (its companion is
/// [`host_supported`]).
#[cfg(not(unix))]
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
    for pid in hosts {
        let path = crate::instance::preview_socket_path(pid);
        // A marker with no socket isn't a host at all (an older build's preview, or a
        // failed bind) — skip without counting it as a refusal.
        if !std::path::Path::new(&path).exists() {
            continue;
        }
        match send_to_socket(&path, req) {
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

/// Windows stub: there is no unix socket, so a capture always opens its own preview,
/// exactly as it does today. The CHILD side is the portable one on purpose — it sits in
/// the shared capture-finish path, so it must compile and answer honestly everywhere.
#[cfg(not(unix))]
pub fn send_to_host(_req: &OpenRequest) -> Result<u32, HandoffError> {
    Err(HandoffError::Unsupported)
}

/// Whether THIS build can host preview handoffs at all, so a caller can branch without
/// a `cfg`. `false` on Windows (no unix sockets — see the module doc's Platforms note),
/// where the host half ([`PreviewHost`] / [`start_host`]) isn't compiled: host-side
/// integration is therefore `#[cfg(unix)]`, while the child-side [`send_to_host`] is
/// portable and simply reports [`HandoffError::Unsupported`].
pub const fn host_supported() -> bool {
    cfg!(unix)
}

/// One handoff attempt against ONE socket path: connect, send, and WAIT for the host's
/// `ok`. `Err(reason)` for every other outcome — including EOF (the host closed without
/// acking, i.e. it was exiting) and timeout (wedged host). Split out from
/// [`send_to_host`] so the whole exchange is testable against a socket in a temp dir,
/// with no dependency on the real runtime dir.
#[cfg(unix)]
fn send_to_socket(path: &str, req: &OpenRequest) -> Result<(), String> {
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
            format!("{MAGIC} 1"),                        // no verb
            format!("{MAGIC} 1 close path=2f61"),        // wrong verb
            format!("{MAGIC} 1 open"),                   // no fields
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

        let req = sample();
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
        let child = std::thread::spawn(move || send_to_socket(&child_path, &sample()));

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

        let err = send_to_socket(&path, &sample()).expect_err("must not report acceptance");
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
        let child = std::thread::spawn(move || send_to_socket(&child_path, &sample()));
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
        let child = std::thread::spawn(move || send_to_socket(&child_path, &sample()));
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
        let err = send_to_socket(&path, &sample()).expect_err("nothing is listening");
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
        let path = host.socket_path().to_string();
        assert!(
            path.ends_with(&format!("cosmic-capture-kit.{}.preview.sock", std::process::id())),
            "{path}"
        );
        assert!(std::path::Path::new(&path).exists(), "socket must exist while hosting");

        // A real end-to-end handoff over the REAL host path.
        let req = sample();
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
}
