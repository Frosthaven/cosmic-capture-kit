//! Proton Drive's [`ProviderOps`], spoken through Proton's official CLI (DRAGON-485).
//!
//! Every other file in this directory builds an HTTP request. This one builds an ARGV and
//! spawns a child, because Proton has no third-party API and its official tool is the supported
//! way in. `cloud::proton` is the seam that finds the tool and classifies its failures; this
//! file is the provider contract on top of it.
//!
//! # Two things are addressed differently here, and both are deliberate
//!
//! * **A folder id IS its path.** The tool takes paths, never opaque ids, so a `RemoteFolder`
//!   carries `/my-files/Cosmic Capture Kit/Shots` as its id, the way the Dropbox provider
//!   already carries a path. That is what makes the ordinary folder step work unchanged.
//! * **There is no token.** The tool owns the session, in the OS secret store, under its own
//!   identity. So `secrets` holds nothing for a Proton account and the `token` argument these
//!   methods receive is always empty; [`super::access_token`] is where that is decided.
//!
//! # Two destinations, and they are not two flavours of one thing
//!
//! An account here uploads either to a FOLDER under `/my-files` or to an ALBUM under `/albums`,
//! and which one is `CloudAccount::destination`. The two use different commands the whole way
//! down (`filesystem upload` vs `photo upload` + `album add-photo`, `filesystem list` vs
//! `album list`), because the tool treats them as different sections of the drive rather than as
//! one tree. `cloud::proton::ALBUMS` carries the model and the reason the picker's two tabs are
//! deliberately NOT unified.
//!
//! The photo path is the awkward one and the awkwardness is the tool's, not ours: `photo upload`
//! has no `--album` flag and reports no identifier for what it moved, so a capture is filed into
//! an album by re-addressing it under the timeline root by the NAME it was uploaded with. See
//! [`PHOTO_CONFLICT`] for what keeps that name unambiguous.
//!
//! # Progress: real, and it comes from `--verbose` (DRAGON-522)
//!
//! An upload reports a genuine percentage, counted from the tool's own log lines. Measured
//! against `proton-drive` 0.7.0 (`cli-drive@0.7.0+5174900c`), and the two dead ends are recorded
//! here so nobody re-runs them:
//!
//! * **The tool's drawn progress display is not the way in.** It is built by a factory that
//!   opens `if (!process.stdout.isTTY) return <no-op sink>`, so a piped stdout gets nothing, and
//!   both upload commands build it as `json ? undefined : progress(...)`, so `--json` disables
//!   it whatever stdout is. Under a pseudo-terminal WITHOUT `--json` it does draw parseable
//!   frames (`⠋ 0.00% <name> (9.27 MiB)`, redrawn with `\x1b[1A\x1b[2K\r`), but adopting them
//!   would mean giving up `--json`, and with it the machine-readable transfer summary
//!   [`transfer_landed`] reads AND the tool's own suppression of interactive conflict prompts.
//!   **The pty route is unnecessary; do not build it.**
//! * **`--verbose` needs neither.** It turns on the tool's console log handler
//!   (`enableConsoleLog: !!options.verbose`), which writes the ordinary log to stdout, line by
//!   line, fully piped, alongside the `--json` output. The two coexist: the transfer summary is
//!   still printed, as the last JSON object line, which is exactly what [`run`] keeps.
//!
//! What is counted, and why that shape: every block logs
//! `<time> INFO [upload] revision <ids>: block <n>:<token>: Uploaded` when it arrives, blocks are
//! 4 MiB ([`UPLOAD_BLOCK_BYTES`]), and the file's own size gives the denominator, since the tool
//! announces no total anywhere. [`uploaded_block_index`] is the line reader,
//! [`block_percent`] the fraction. Both destinations share it: the Photos upload goes through
//! the same SDK uploader and logs the same lines.
//!
//! So the meter is granular for a RECORDING and still a spinner for a screenshot, which is the
//! honest outcome rather than a limitation to work around: a screenshot is one block, so there
//! is no midpoint to report, and [`block_percent`] declines to invent one.
//!
//! **Nothing from those lines is ever logged.** They carry per-block verification tokens and, at
//! the tool's default level, full API addresses. They are parsed, counted and dropped.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use super::super::accounts::CloudAccount;
use super::super::proton::{self, Failure};
use super::{RemoteFile, RemoteFolder};

/// The unit struct the dispatch point hands back.
pub struct Proton;

/// How long a metadata call (a listing, a folder create, a trash, a share link) may take.
///
/// Generous, because the tool is a 118 MB self-contained binary whose start-up dominates a
/// small call, and because its first run after a login decrypts and caches keys. Bounded all
/// the same, per the house rule that nothing waits forever.
const META_BUDGET: Duration = Duration::from_secs(90);

/// The floor for an upload's budget, and the extra allowed per megabyte.
///
/// The same shape `super::upload_budget` uses for the HTTP providers: a small file must not
/// inherit a huge timeout, and a large one must not die under a timeout written for a small
/// one. Proton encrypts every block client-side before it goes out, so its per-megabyte cost is
/// higher than a plain PUT.
const UPLOAD_BASE: Duration = Duration::from_secs(120);
const UPLOAD_PER_MB: Duration = Duration::from_secs(4);

/// How often a running child is checked for having finished or for a cancel.
const POLL: Duration = Duration::from_millis(50);

/// The budget for an upload of `bytes`. Pure; unit-tested.
pub fn upload_budget(bytes: u64) -> Duration {
    let megabytes = bytes.div_ceil(1024 * 1024);
    UPLOAD_BASE + UPLOAD_PER_MB * u32::try_from(megabytes).unwrap_or(u32::MAX)
}

/// What one finished invocation produced.
struct Output {
    stdout: String,
    stderr: String,
    status: Option<i32>,
}

/// What a WATCHED (verbose) run counts as it reads the tool's log lines.
///
/// Two atomics rather than one, because the denominator can be wrong: the count is what the
/// meter divides, and the highest index seen is what stops a file with more blocks than its size
/// implies (a thumbnail, a revision the tool re-sliced) from reporting past 100.
#[derive(Default)]
struct BlockCounter {
    done: std::sync::atomic::AtomicU32,
    highest: std::sync::atomic::AtomicU32,
}

/// The upload side of a run: how many blocks are expected, and where to report progress.
///
/// Only the two upload commands pass one. Every other call site passes `None` and behaves
/// exactly as it did before verbose output existed, whole stdout included.
struct UploadWatch<'a> {
    /// Blocks this file is expected to take, from its size ([`block_count`]).
    total: u32,
    /// The provider contract's own reporter, called from the POLL loop rather than from the
    /// reader thread: `on_progress` is `&mut dyn FnMut` and belongs to the caller's thread.
    on_progress: &'a mut dyn FnMut(u8),
}

/// Run the tool with `args`, bounded by `budget`, aborting early if `should_cancel` says so.
///
/// The one place a child is spawned, so the console-window suppression, the pipe wiring, the
/// deadline and the kill-on-cancel are decided once. `operation` is the user-facing phrase that
/// goes into a failure message.
///
/// **Both pipes are drained by their own threads** (DRAGON-522). They used to be read once, at
/// the end, through `wait_with_output`: fine while every invocation printed one small JSON
/// object, and a deadlock waiting to happen the moment one printed more than a pipe buffer, which
/// is exactly what `--verbose` does. A reader thread per pipe also makes progress possible at
/// all, since a line has to be seen while the child is still running for it to mean anything.
///
/// `watch` turns a run into a WATCHED one: stdout lines are counted through
/// [`uploaded_block_index`] as they arrive, and the poll loop below reports the percentage. A
/// watched run keeps only the last JSON OBJECT line as its `stdout`, because the rest of what
/// verbose prints is the tool's log and none of the parsers want it. Nothing from those lines is
/// ever logged: they carry per-block tokens and full API addresses, so they are parsed, counted
/// and dropped (see this module's privacy note).
fn run(
    args: &[&str],
    budget: Duration,
    operation: &str,
    should_cancel: &dyn Fn() -> bool,
    mut watch: Option<UploadWatch<'_>>,
) -> Result<Output, String> {
    use std::io::{BufRead, BufReader, Read};
    use std::sync::atomic::Ordering;

    /// The key that identifies the transfer SUMMARY among the JSON a watched run prints.
    ///
    /// A watched run's stdout carries the tool's log as well as its answer, and one of those log
    /// lines is a metric event that ends in a JSON object of its own. Keeping "the last line that
    /// looks like JSON" would eventually keep the wrong one, so the summary is recognised by a
    /// field only it has.
    const SUMMARY_KEY: &str = "transferredItems";

    let mut command = crate::util::quiet_command(crate::util::proton_drive_path());
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|_| {
        format!(
            "This app could not run the {} tool, so it could not {operation}.",
            proton::TOOL_NAME
        )
    })?;
    let watched = watch.is_some();
    let counter = std::sync::Arc::new(BlockCounter::default());
    let stdout_slot = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let stdout_reader = child.stdout.take().map(|stdout| {
        let slot = std::sync::Arc::clone(&stdout_slot);
        let counter = std::sync::Arc::clone(&counter);
        std::thread::spawn(move || {
            let mut kept = String::new();
            let mut reader = BufReader::new(stdout);
            let mut raw = Vec::new();
            // Read BYTES and convert per line, never `lines()`: that iterator ends at the first
            // line that is not valid UTF-8, which would leave the rest of the pipe unread and
            // wedge the child against a full buffer until the deadline killed it.
            while let Ok(read) = reader.read_until(b'\n', &mut raw) {
                if read == 0 {
                    break;
                }
                let line = String::from_utf8_lossy(&raw);
                let line = line.trim_end_matches(['\n', '\r']);
                if !watched {
                    // The unwatched shape: every byte, exactly as `wait_with_output` gave it,
                    // because a listing's JSON array spans several lines.
                    kept.push_str(line);
                    kept.push('\n');
                } else if let Some(index) = uploaded_block_index(line) {
                    counter.done.fetch_add(1, Ordering::Relaxed);
                    counter.highest.fetch_max(index, Ordering::Relaxed);
                } else {
                    // The one line a watched run keeps: the transfer summary. Everything else it
                    // has read is the tool's own log, and it is dropped here rather than carried
                    // anywhere (see this module's privacy note).
                    let trimmed = line.trim();
                    if trimmed.starts_with('{') && trimmed.contains(SUMMARY_KEY) {
                        kept = trimmed.to_string();
                    }
                }
                raw.clear();
            }
            if let Ok(mut g) = slot.lock() {
                *g = kept;
            }
        })
    });
    let stderr_slot = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let stderr_reader = child.stderr.take().map(|mut stderr| {
        let slot = std::sync::Arc::clone(&stderr_slot);
        std::thread::spawn(move || {
            // Lossy, like the `wait_with_output` this replaced: `read_to_string` would REFUSE a
            // stderr with one bad byte in it, and this text is only ever fed to
            // `classify_failure`, which reads it for known phrases.
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            if let Ok(mut g) = slot.lock() {
                *g = String::from_utf8_lossy(&bytes).into_owned();
            }
        })
    });
    let deadline = Instant::now() + budget;
    let mut last_percent: Option<u8> = None;
    let status = loop {
        if should_cancel() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(super::canceled_err());
        }
        if let Some(watch) = watch.as_mut() {
            // The reader thread counts; this reports. A percentage is only ever handed on when
            // it CHANGES, so a still transfer costs the meter nothing.
            let done = counter.done.load(Ordering::Relaxed);
            let total = watch.total.max(counter.highest.load(Ordering::Relaxed));
            if let Some(percent) = block_percent(done, total)
                && Some(percent) != last_percent
            {
                last_percent = Some(percent);
                (watch.on_progress)(percent);
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            Ok(None) => {
                // Past its budget and still running. Kill it rather than leaving a child
                // holding a half-finished transfer, and report it as a retryable failure.
                let _ = child.kill();
                let _ = child.wait();
                return Err(proton::failure_message(Failure::Transient, operation));
            }
            Err(_) => return Err(proton::failure_message(Failure::Other, operation)),
        }
    };
    // The child is over, so both pipes are at EOF and both readers are about to end. Joining is
    // what makes the slots complete; neither can block on anything else, since a reader only
    // ever reads its own pipe (DRAGON-118: this wait has an end by construction, not by hope).
    for reader in [stdout_reader, stderr_reader].into_iter().flatten() {
        let _ = reader.join();
    }
    Ok(Output {
        stdout: stdout_slot.lock().map(|g| g.clone()).unwrap_or_default(),
        stderr: stderr_slot.lock().map(|g| g.clone()).unwrap_or_default(),
        status: status.code(),
    })
}

/// Run and insist on success, turning a failure into this app's own copy.
fn run_ok(
    args: &[&str],
    budget: Duration,
    operation: &str,
    should_cancel: &dyn Fn() -> bool,
) -> Result<String, String> {
    let out = run(args, budget, operation, should_cancel, None)?;
    if out.status == Some(0) {
        return Ok(out.stdout);
    }
    let failure = proton::classify_failure(out.status, &out.stderr);
    // The tool's own text can name a path or an item, so it never reaches a log; the category
    // and the exit code are what a diagnosis actually runs on.
    log::debug!(
        "cloud accounts: proton-drive {operation} failed, exit {:?}, classified {failure:?}",
        out.status
    );
    Err(proton::failure_message(failure, operation))
}

/// Never cancelled, for the calls with no cancel to honour.
fn never() -> bool {
    false
}

/// The size the tool's SDK slices an upload into, in bytes.
///
/// 4 MiB, read out of the tool itself rather than measured: its own block-index helper is
/// `blockIndex: Math.floor(offset / 4194304) + 1`, and its per-block crypto metric reports
/// exactly `bytesProcessed: 4194304` for every full block. A 62.95 MiB file produced exactly 16
/// `Uploaded` lines, which is this constant.
const UPLOAD_BLOCK_BYTES: u64 = 4 * 1024 * 1024;

/// How many blocks a file of `bytes` uploads as. Pure; unit-tested.
///
/// The denominator the meter divides by. An empty file is ONE block rather than zero, so the
/// fraction is never over zero; an absurd size saturates rather than wrapping.
pub fn block_count(bytes: u64) -> u32 {
    u32::try_from(bytes.div_ceil(UPLOAD_BLOCK_BYTES)).unwrap_or(u32::MAX).max(1)
}

/// Drop ANSI colour sequences from one line of the tool's verbose output. Pure; unit-tested.
///
/// The tool colours its console log unconditionally (`\x1b[1;34m` … `\x1b[0m` per line; the
/// colour choice is by LEVEL, and unlike its progress display it is not gated on a TTY at all),
/// so the escapes are in the pipe. Written by hand rather than by regex: it is a two-state scan
/// and this crate has no regex dependency.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // `ESC [ <params> <final byte>`: everything up to and including the first ASCII letter
        // belongs to the escape. A truncated escape at end of line simply ends the scan.
        for c in chars.by_ref() {
            if c.is_ascii_alphabetic() {
                break;
            }
        }
    }
    out
}

/// The block index one verbose line reports as ARRIVED at the provider, or `None` for every
/// other line. Pure; unit-tested (DRAGON-522).
///
/// The shape, captured from a real transfer:
///
/// ```text
/// <ISO time> INFO [upload] revision <ids>: block 3:<token>: Uploaded
/// ```
///
/// Three things make a line count, and all three are needed:
///
/// * The `[upload]` logger, so an `[api]` or `[metric]` line about something else never does.
/// * A `block <n>:` field, which is the index and the only number in the line worth having.
/// * A trailing `Uploaded`. The SAME block also logs `Upload started` when its request goes out,
///   and the file logs `Upload succeeded` at the end; counting either would double the number or
///   report progress for bytes still in flight.
///
/// **Nothing from the line is kept but the index.** The token between the index and the verdict
/// is per-block verification material, and the rule in `cloud`'s module doc covers it: parse,
/// count, drop, never log.
pub fn uploaded_block_index(line: &str) -> Option<u32> {
    let line = strip_ansi(line);
    let line = line.trim_end();
    if !line.contains("[upload]") || !line.ends_with("Uploaded") {
        return None;
    }
    let rest = line.split(" block ").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The percentage after `done` of `total` blocks, or `None` when there is nothing GENUINE to
/// report yet. Pure; unit-tested (DRAGON-522).
///
/// `None` for the two ends, and both matter. Before the first block lands there is no evidence
/// of anything, and the meter's spinner is the honest face
/// (`cloud::upload::tray::is_genuine_progress` refuses 0 anyway, so reporting it would be a
/// no-op that only looked like progress). At the far end the answer is capped at 99: the last
/// block's line is written when its request returns, and the file is not in the drive until the
/// tool commits the revision after it, so 100 belongs to the caller's own end-of-upload report
/// and not to a block count.
pub fn block_percent(done: u32, total: u32) -> Option<u8> {
    if done == 0 {
        return None;
    }
    // `max(done)` below is what makes a nonsense denominator harmless: a total of zero, or one
    // smaller than the number of blocks that have actually arrived, divides by the count instead
    // and reports the top of the range rather than dividing by zero or overshooting.
    let percent = u64::from(done) * 100 / u64::from(total.max(done));
    u8::try_from(percent).ok().map(|p| p.clamp(1, 99))
}

/// One entry of a `filesystem list --json` or `album list --json` array, as much of it as this
/// app reads.
///
/// The tool serializes the SDK's own node objects, whose `name` is a result wrapper rather than
/// a bare string: a name is decrypted and its signature verified, and either can fail, so the
/// shape is `{"ok": true, "value": "Shots"}`. A name that did not verify has no usable text at
/// all, which is why [`parse_folders`] skips those entries rather than showing an empty row.
#[derive(Debug, serde::Deserialize)]
struct ListedNode {
    #[serde(default)]
    uid: String,
    #[serde(default)]
    name: Option<ListedName>,
    #[serde(default, rename = "type")]
    node_type: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum ListedName {
    /// The ordinary shape: a verified name.
    Wrapped { ok: bool, value: String },
    /// A bare string, accepted so a future simplification of the tool's output does not break
    /// listing outright. Tolerated on READ only; nothing here ever writes this shape.
    Plain(String),
}

impl ListedName {
    /// The usable name, or `None` when the tool could not vouch for it. Pure; unit-tested.
    fn text(&self) -> Option<&str> {
        match self {
            ListedName::Wrapped { ok: true, value } if !value.is_empty() => Some(value),
            ListedName::Plain(value) if !value.is_empty() => Some(value),
            _ => None,
        }
    }
}

/// Read a `filesystem list --json` array into this app's folders. Pure; unit-tested.
///
/// `parent` is the path that was listed, so each child's id can be built from it: the tool
/// addresses by path, so a folder's id is its full path.
///
/// **Non-folders are dropped and unverifiable names are dropped.** The first because the
/// destination picker only ever shows folders, and the second because a name whose signature
/// did not check out cannot be turned into a path the tool would accept, and showing it would
/// invite a click that could only fail.
pub fn parse_folders(body: &str, parent: &str) -> Result<Vec<RemoteFolder>, String> {
    let nodes: Vec<ListedNode> = serde_json::from_str(body.trim())
        .map_err(|_| "Proton Drive returned a folder listing this app could not read.".to_string())?;
    let mut folders = Vec::new();
    for node in nodes {
        if !node.node_type.eq_ignore_ascii_case("folder") {
            continue;
        }
        let Some(name) = node.name.as_ref().and_then(ListedName::text) else {
            log::debug!("cloud accounts: skipping a Proton folder whose name did not verify");
            continue;
        };
        let _ = &node.uid;
        folders.push(RemoteFolder { id: format!("{parent}/{name}"), name: name.to_string() });
    }
    Ok(folders)
}

/// The node type an `album list` entry carries.
const ALBUM_TYPE: &str = "album";

/// Read an `album list --json` array into this app's rows. Pure; unit-tested.
///
/// The same wire shape [`parse_folders`] reads, with two differences that matter:
///
/// * An album's id is its **UID**, wrapped as a path (`cloud::proton::album_path`), NOT its
///   name. A name lookup walks every album, and two albums may share a name; the uid is exact
///   and is what every later command takes.
/// * Nothing is filtered by parent, because there is no parent. An album list is the whole set.
///
/// Entries whose name did not verify are dropped for the same reason folders' are: a row a user
/// cannot read is a row whose press could only confuse them. An entry with no uid is dropped
/// too, since there would be nothing to address it by.
pub fn parse_albums(body: &str) -> Result<Vec<RemoteFolder>, String> {
    let nodes: Vec<ListedNode> = serde_json::from_str(body.trim())
        .map_err(|_| "Proton Drive returned an album list this app could not read.".to_string())?;
    let mut albums = Vec::new();
    for node in nodes {
        if !node.node_type.eq_ignore_ascii_case(ALBUM_TYPE) {
            continue;
        }
        let Some(name) = node.name.as_ref().and_then(ListedName::text) else {
            log::debug!("cloud accounts: skipping a Proton album whose name did not verify");
            continue;
        };
        if node.uid.trim().is_empty() {
            log::debug!("cloud accounts: skipping a Proton album with no identifier");
            continue;
        }
        albums.push(RemoteFolder {
            id: proton::album_path(&node.uid),
            name: name.to_string(),
        });
    }
    Ok(albums)
}

/// Read the ONE node object `album create --json` prints. Pure; unit-tested.
///
/// A single object rather than the array [`parse_albums`] reads, because the tool prints a
/// created node with its own printer. The uid is what makes this call worth parsing at all: it
/// is the handle every later command takes, and asking for it back by name afterwards would
/// reintroduce exactly the ambiguity the uid exists to avoid.
///
/// The NAME is allowed to be missing here, unlike in a listing: the caller typed it, so a
/// created album whose name did not come back verified still has a name this app knows. `typed`
/// is that fallback.
pub fn parse_created_album(body: &str, typed: &str) -> Result<RemoteFolder, String> {
    let node: ListedNode = serde_json::from_str(body.trim())
        .map_err(|_| "Proton Drive did not say which album it made.".to_string())?;
    if node.uid.trim().is_empty() {
        return Err("Proton Drive did not say which album it made.".to_string());
    }
    let name = node
        .name
        .as_ref()
        .and_then(ListedName::text)
        .unwrap_or(typed)
        .to_string();
    Ok(RemoteFolder { id: proton::album_path(&node.uid), name })
}

/// Whether an `album add-photo --json` reply says every photo was added. Pure; unit-tested.
///
/// The tool prints one `{"uid": …, "ok": true|false}` per photo. **The `error` beside a failed
/// one is a JavaScript `Error`, whose message is not enumerable, so it serializes as `{}`**:
/// there is no reason to read out of this reply, only a verdict. That is why this answers a
/// bool and the caller supplies the sentence.
///
/// An EMPTY reply is a failure, not a success: one photo went in, so exactly one result should
/// come back, and nothing coming back means the tool did not do what was asked.
pub fn album_add_succeeded(body: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct NodeResult {
        #[serde(default)]
        ok: bool,
    }
    let Ok(results) = serde_json::from_str::<Vec<NodeResult>>(body.trim()) else {
        return false;
    };
    !results.is_empty() && results.iter().all(|r| r.ok)
}

/// What `photo upload --json` and `filesystem upload --json` print: a transfer SUMMARY, and no
/// identifier for anything transferred.
///
/// This is the whole reason the Photos upload takes two steps and re-addresses the photo by
/// name; see [`Proton::upload`].
#[derive(Debug, serde::Deserialize)]
struct TransferSummary {
    #[serde(default, rename = "transferredItems")]
    transferred: u32,
    #[serde(default, rename = "skippedItems")]
    skipped: u32,
    #[serde(default, rename = "failedItems")]
    failed: u32,
}

/// Whether a transfer summary says the file is now in the drive. Pure; unit-tested.
///
/// **A SKIP counts.** With the conflict strategy this app passes, a photo whose name is already
/// in the timeline is skipped rather than uploaded again, and the honest reading of that is that
/// the photo of that name IS there, which is exactly what the next step needs to be true. A
/// failure never counts, whatever else the summary says.
///
/// A summary that could not be read at all answers `false`: the tool's exit code said the run
/// succeeded, but nothing in it said the file arrived, and guessing yes would mean filing a
/// photo that may not exist into an album.
pub fn transfer_landed(body: &str) -> bool {
    let Ok(summary) = serde_json::from_str::<TransferSummary>(body.trim()) else {
        return false;
    };
    summary.failed == 0 && (summary.transferred > 0 || summary.skipped > 0)
}

/// Where the public link actually sits in a `sharing set-url --json` reply.
///
/// **The reply is the node's WHOLE sharing state, not a link** (DRAGON-522). `sharing set-url`
/// prints what the SDK's `shareNode` returns, which is
/// `{"protonInvitations":[…],"nonProtonInvitations":[…],"members":[…],"urlAccess":{…},
/// "editorsCanShare":false}`, and the public link is `urlAccess.url` inside it. The url access
/// object is the same one `sharing status` reports, so this key is the one to read for either.
const URL_ACCESS: &str = "urlAccess";

/// The keys a URL-shaped value has ever been seen under, innermost first.
///
/// `url` is what the tool writes today, in both of the places the SDK builds a url access object
/// (a link just created, and one read back and decrypted). The rest are tolerated on READ only,
/// because the naming moved between pre-release builds and a link that arrives under an older
/// name is still a link.
const URL_KEYS: &[&str] = &["url", "publicLink", "publicUrl", "shareUrl", "link"];

/// Pull the public link out of a `sharing set-url --json` object. Pure; unit-tested.
///
/// **The link is NESTED, and reading only the top level is what DRAGON-522 was reported for.**
/// Uploads landed, the tool created the link (its own log says
/// `Sharing via URL access with role viewer`), and this function then answered "no usable share
/// link" for every single one, because it looked for `url` at the top of an object whose top
/// level carries invitations and members. Nothing reached the clipboard and the editor's meter
/// offered no copy control, since both read the link out of the upload's done state and the done
/// state had none to carry.
///
/// So [`URL_ACCESS`] is looked in FIRST, and the top level is kept as a fallback for a future
/// build that flattens the reply. What is NOT tolerated either way is a value that is not an
/// https URL, because this string becomes a click target.
pub fn parse_share_url(body: &str) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_str(body.trim())
        .map_err(|_| "Proton Drive did not return a usable share link.".to_string())?;
    // The url access object first, then the top level: a reply that carries both cannot disagree
    // about which one is the link the tool just made.
    let scopes = [value.get(URL_ACCESS), Some(&value)];
    for scope in scopes.into_iter().flatten() {
        for key in URL_KEYS {
            if let Some(text) = scope.get(key).and_then(serde_json::Value::as_str)
                && text.starts_with("https://")
            {
                return Ok(text.to_string());
            }
        }
    }
    Err("Proton Drive did not return a usable share link.".to_string())
}

/// The drive path an account's uploads land in.
fn account_root(acct: &CloudAccount) -> String {
    let leaf = super::super::app_folder_name("proton").unwrap_or(super::super::APP_FOLDER);
    let stored = acct.folder_name.as_deref();
    let mut segments = vec![super::super::APP_FOLDER];
    segments.extend(super::super::folder_path_segments(stored, leaf));
    proton::drive_path(&segments)
}

/// The album an account uploads to, or `None` when it uploads to a folder instead.
///
/// One reader of the two stored halves ([`crate::cloud::uploads_to_album`] for the KIND, the
/// account's `folder_id` for the album's path), so the upload and the settings row cannot
/// disagree about where a capture goes. An account switched to Photos that has somehow not
/// recorded an album falls back to the app's own album by NAME, which is the one the setup step
/// finds or creates and therefore the one that exists.
fn account_album(acct: &CloudAccount) -> Option<String> {
    if !crate::cloud::uploads_to_album(acct) {
        return None;
    }
    let stored = acct.folder_id.as_deref().map(str::trim).filter(|id| !id.is_empty());
    Some(stored.map_or_else(
        || proton::album_path(crate::cloud::APP_FOLDER),
        std::string::ToString::to_string,
    ))
}

/// How a `photo upload` answers a name that is already in the timeline.
///
/// **`skip`, and the choice is load-bearing.** `photo upload` reports a transfer SUMMARY with no
/// identifier for what it moved, so the only handle on the new photo is the NAME it was given,
/// and the very next step addresses `/photos/<that name>`. Under the tool's other strategy,
/// `keep-both`, a collision renames the arriving photo to something this app never learns, and
/// the name it then addresses would resolve to the OLDER photo, which would be filed into the
/// album instead. Under `skip` the photo of that name is the one already there, which is what
/// the name resolves to, so the two agree by construction.
const PHOTO_CONFLICT: &str = "skip";

/// The flag that makes an upload REPORT (DRAGON-522).
///
/// `--verbose` turns on the tool's console log handler (`enableConsoleLog: !!options.verbose`),
/// which writes its ordinary log to stdout, line by line, with no TTY involved. That is where
/// the per-block `Uploaded` lines this app counts come from. It goes AFTER the subcommand words,
/// like every other option the tool takes.
const VERBOSE: &str = "--verbose";

/// Run one upload command with progress, and hand back its transfer summary.
///
/// The one place [`UploadWatch`] is built, so the two destinations (a folder and an album) cannot
/// end up reporting differently. `bytes` is the local file's size, which is the only place the
/// denominator can come from: the tool announces no block count anywhere.
fn run_upload(
    args: &[&str],
    bytes: u64,
    operation: &str,
    on_progress: &mut dyn FnMut(u8),
    should_cancel: &dyn Fn() -> bool,
) -> Result<String, String> {
    let watch = UploadWatch { total: block_count(bytes), on_progress };
    let out = run(args, upload_budget(bytes), operation, should_cancel, Some(watch))?;
    if out.status == Some(0) {
        return Ok(out.stdout);
    }
    let failure = proton::classify_failure(out.status, &out.stderr);
    log::debug!(
        "cloud accounts: proton-drive {operation} failed, exit {:?}, classified {failure:?}",
        out.status
    );
    Err(proton::failure_message(failure, operation))
}

impl super::ProviderOps for Proton {
    fn upload(
        &self,
        acct: &CloudAccount,
        _token: &str,
        file: &Path,
        on_progress: &mut dyn FnMut(u8),
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<RemoteFile, String> {
        let local = file.to_string_lossy().into_owned();
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| "That capture has no file name to upload.".to_string())?;
        let bytes = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
        // The opening 0, as every provider makes: not progress, and `is_genuine_progress`
        // refuses it, so the meter starts on its spinner and stays there until a real block
        // lands. See this file's module doc for what does the reporting after that.
        on_progress(0);
        let uploaded = match account_album(acct) {
            // **The Photos destination takes TWO commands, and it has to** (DRAGON-485). There
            // is no `--album` flag on `photo upload`, and its `--json` output is a transfer
            // summary with no identifier in it, so the photo is filed by re-addressing it
            // under the timeline root by the name it was uploaded with. `PHOTO_CONFLICT` is
            // what makes that name unambiguous.
            Some(album) => {
                let summary = run_upload(
                    &[
                        "photo",
                        "upload",
                        &local,
                        "--conflict-strategy",
                        PHOTO_CONFLICT,
                        "--json",
                        VERBOSE,
                    ],
                    bytes,
                    "upload this capture",
                    on_progress,
                    should_cancel,
                )?;
                if !transfer_landed(&summary) {
                    return Err(proton::failure_message(Failure::Other, "upload this capture"));
                }
                let photo = proton::photo_path(&name);
                let added = run_ok(
                    &["album", "add-photo", &album, &photo, "--json"],
                    META_BUDGET,
                    "add this capture to your album",
                    should_cancel,
                )?;
                if !album_add_succeeded(&added) {
                    // The photo IS in the drive, in the timeline, and saying otherwise would be
                    // a lie. What failed is the destination the user actually chose, so this is
                    // reported rather than quietly treated as a success.
                    return Err(proton::failure_message(
                        Failure::Other,
                        "add this capture to your album",
                    ));
                }
                photo
            }
            None => {
                let parent = account_root(acct);
                let summary = run_upload(
                    &["filesystem", "upload", &local, &parent, "--json", VERBOSE],
                    bytes,
                    "upload this capture",
                    on_progress,
                    should_cancel,
                )?;
                if !transfer_landed(&summary) {
                    // The same check the Photos side has always made, and it earns its place
                    // here for the same reason: a run that exits 0 having transferred nothing
                    // (every item skipped as unsupported, a summary that could not be read) must
                    // not report a file this app then hands to a share link.
                    return Err(proton::failure_message(Failure::Other, "upload this capture"));
                }
                format!("{parent}/{name}")
            }
        };
        on_progress(100);
        Ok(RemoteFile { id: uploaded, web_url: None })
    }

    fn list_albums(&self, _token: &str) -> Result<Vec<RemoteFolder>, String> {
        // NOT `filesystem list`: the tool's listing command has no arm for the albums root and
        // refuses it outright. `album list` takes no path at all, because albums are flat.
        let body = run_ok(&["album", "list", "--json"], META_BUDGET, "list your albums", &never)?;
        parse_albums(&body)
    }

    fn create_album(&self, _token: &str, name: &str) -> Result<RemoteFolder, String> {
        // `album create` takes a NAME, not a path: there is nowhere for an album to be created
        // inside, which is the same flatness the picker draws.
        let body = run_ok(
            &["album", "create", name, "--json"],
            META_BUDGET,
            "create that album",
            &never,
        )?;
        parse_created_album(&body, name)
    }

    fn delete_album(&self, _token: &str, album_id: &str) -> Result<(), String> {
        // `--force` because nothing here is interactive, and `--save` because the photos in an
        // album are the user's photos: deleting a collection must not delete what is in it.
        // The confirmation's copy promises exactly that; see `pages::cloud::delete_confirm_body`.
        //
        // **`--json` prints nothing at all for this command**, so success is the exit code and
        // there is deliberately nothing here to parse.
        run_ok(
            &["album", "delete", album_id, "--force", "--save", "--json"],
            META_BUDGET,
            "delete that album",
            &never,
        )
        .map(drop)
    }

    fn create_share_link(&self, _token: &str, file_id: &str) -> Result<String, String> {
        let body = run_ok(
            &["sharing", "set-url", file_id, "--json"],
            META_BUDGET,
            "make a share link",
            &never,
        )?;
        parse_share_url(&body)
    }

    fn list_folders(&self, _token: &str, parent: Option<&str>) -> Result<Vec<RemoteFolder>, String> {
        let root;
        let path = match parent {
            Some(path) => path,
            None => {
                root = proton::drive_path(&[super::super::APP_FOLDER]);
                &root
            }
        };
        let body = run_ok(
            &["filesystem", "list", path, "--type", "folder", "--json"],
            META_BUDGET,
            "list your folders",
            &never,
        )?;
        parse_folders(&body, path)
    }

    fn create_folder(
        &self,
        _token: &str,
        parent: Option<&str>,
        name: &str,
    ) -> Result<RemoteFolder, String> {
        let root;
        let path = match parent {
            Some(path) => path,
            None => {
                root = proton::drive_path(&[super::super::APP_FOLDER]);
                &root
            }
        };
        run_ok(
            &["filesystem", "create-folder", path, name, "--json"],
            META_BUDGET,
            "create that folder",
            &never,
        )?;
        Ok(RemoteFolder { id: format!("{path}/{name}"), name: name.to_string() })
    }

    fn delete_folder(&self, _token: &str, folder_id: &str) -> Result<(), String> {
        // TRASH, not delete: Proton's trash is restorable, which is what this provider's
        // `delete_recoverable` capability promises the user in the confirmation.
        run_ok(
            &["filesystem", "trash", folder_id, "--json"],
            META_BUDGET,
            "delete that folder",
            &never,
        )
        .map(drop)
    }

    fn ensure_root_folder(&self, token: &str) -> Result<Option<RemoteFolder>, String> {
        let root = proton::drive_path(&[super::super::APP_FOLDER]);
        // Listing it is the cheapest existence check the tool offers, and it is the same call
        // the folder step is about to make anyway.
        if run(
            &["filesystem", "list", &root, "--json"],
            META_BUDGET,
            "open your Proton Drive folder",
            &never,
            None,
        )?
        .status
            == Some(0)
        {
            return Ok(Some(RemoteFolder {
                id: root,
                name: super::super::APP_FOLDER.to_string(),
            }));
        }
        self.create_folder(token, Some(&proton::drive_path(&[])), super::super::APP_FOLDER)
            .map(Some)
    }

    fn delete_file(&self, _token: &str, file_id: &str) -> Result<(), String> {
        run_ok(
            &["filesystem", "trash", file_id, "--json"],
            META_BUDGET,
            "remove that file",
            &never,
        )
        .map(drop)
    }

    fn revoke(&self, _token: &str) -> Result<(), String> {
        // Signs out and clears the tool's cached credentials and caches. The session belongs to
        // the tool, so this is the only way to end it, and it ends the ONLY session the tool can
        // hold; see the single-account cap in `cloud::proton`.
        run_ok(&["auth", "logout"], META_BUDGET, "sign out of Proton Drive", &never).map(drop)
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn a_small_upload_gets_the_floor() {
        assert_eq!(upload_budget(0), UPLOAD_BASE);
        assert_eq!(upload_budget(1), UPLOAD_BASE + UPLOAD_PER_MB);
    }

    #[test]
    fn a_large_upload_gets_more() {
        assert!(upload_budget(500 * 1024 * 1024) > upload_budget(1024 * 1024));
    }

    /// The budget must never overflow into something tiny for an absurd size.
    #[test]
    fn an_absurd_size_still_produces_a_large_budget() {
        assert!(upload_budget(u64::MAX) > UPLOAD_BASE);
    }
}

#[cfg(test)]
mod upload_progress_tests {
    use super::*;

    /// One real line, with its token shortened. The colour codes are the tool's own: it colours
    /// its console log by level whether or not anything is a terminal.
    fn uploaded(index: u32) -> String {
        format!(
            "\u{1b}[1;34m2026-08-05T00:26:05.098Z INFO [upload] revision VOL==~NODE==~REV==: \
             block {index}:eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJpYXQiOjE3ODU4ODk1: \
             Uploaded\u{1b}[0m"
        )
    }

    #[test]
    fn a_block_arrival_reports_its_index() {
        assert_eq!(uploaded_block_index(&uploaded(1)), Some(1));
        assert_eq!(uploaded_block_index(&uploaded(16)), Some(16));
        // Uncoloured, in case the tool ever stops painting its log.
        assert_eq!(
            uploaded_block_index(
                "2026-08-05T00:26:05.098Z INFO [upload] revision R: block 7:tok: Uploaded"
            ),
            Some(7)
        );
    }

    /// **The same block logs twice**, and only the second one means the bytes are at the
    /// provider. Counting `Upload started` would double every number and report progress for
    /// requests still in flight.
    #[test]
    fn only_an_arrival_counts_not_a_departure() {
        let started = uploaded(3).replace("Uploaded", "Upload started");
        assert_eq!(uploaded_block_index(&started), None);
        // The end-of-file line, which names no block and must not be mistaken for one.
        assert_eq!(
            uploaded_block_index(
                "2026-08-05T00:26:07.072Z INFO [upload] revision R: Upload succeeded"
            ),
            None
        );
    }

    /// Every other logger writes to the same stream, including one whose message ENDS in a JSON
    /// blob. None of them is a block.
    #[test]
    fn lines_from_other_loggers_are_not_blocks() {
        for line in [
            "2026-08-05T00:26:02.050Z INFO [api] POST https://drive-api.proton.me/drive/blocks: 200 (310ms)",
            "2026-08-05T00:26:01.681Z INFO [metric] performance {\"eventName\":\"performance\",\"bytesProcessed\":4194304}",
            "2026-08-05T00:25:59.353Z INFO [interface] Getting my files root folder",
            "{\"transferredItems\":1,\"transferredBytes\":9722608,\"skippedItems\":0,\"failedItems\":0}",
            "Transfer summary:",
            "",
        ] {
            assert_eq!(uploaded_block_index(line), None, "{line}");
        }
    }

    /// The denominator: the file's own size, in 4 MiB blocks. A capture too small to fill one
    /// block is still one block, so the fraction is never over zero.
    #[test]
    fn a_files_block_count_comes_from_its_size() {
        assert_eq!(block_count(0), 1);
        assert_eq!(block_count(1), 1);
        assert_eq!(block_count(4 * 1024 * 1024), 1);
        assert_eq!(block_count(4 * 1024 * 1024 + 1), 2);
        // The live 62.95 MiB probe produced exactly 16 arrival lines.
        assert_eq!(block_count(66_010_000), 16);
        assert_eq!(block_count(u64::MAX), u32::MAX);
    }

    /// **A screenshot has no midpoint, and this must not invent one.** One block means the only
    /// values are "nothing yet" and "the upload is over", and the meter's spinner is the honest
    /// face for the whole of it.
    #[test]
    fn a_single_block_upload_never_reports_a_midpoint() {
        assert_eq!(block_percent(0, 1), None);
        // Even the block's own arrival is capped below 100: the file is not in the drive until
        // the tool commits the revision after it.
        assert_eq!(block_percent(1, 1), Some(99));
        assert!(!crate::cloud::upload::tray::is_genuine_progress(0));
    }

    /// A recording is where this pays off: sixteen blocks, sixteen honest steps.
    #[test]
    fn a_many_block_upload_climbs() {
        let steps: Vec<u8> = (1..=16).filter_map(|done| block_percent(done, 16)).collect();
        assert_eq!(steps.len(), 16);
        assert!(steps.windows(2).all(|w| w[0] <= w[1]), "{steps:?} must not go backwards");
        assert_eq!(steps.first(), Some(&6));
        assert_eq!(steps.last(), Some(&99), "the last block is not the committed file");
        for percent in &steps {
            assert!(
                crate::cloud::upload::tray::is_genuine_progress(*percent),
                "{percent} must count as real progress or the meter keeps spinning"
            );
        }
    }

    /// More blocks than the size predicted (a thumbnail, a re-sliced revision) must not report
    /// past the end: the caller widens the denominator with the highest index it has seen, and
    /// this clamps whatever is left.
    #[test]
    fn more_blocks_than_expected_never_overshoot() {
        assert_eq!(block_percent(20, 16), Some(99));
        assert_eq!(block_percent(1, 0), Some(99));
    }

    /// The escapes are in the pipe, so the reader has to see through them. Nothing else about a
    /// line is touched.
    #[test]
    fn colour_escapes_are_seen_through() {
        assert_eq!(strip_ansi("\u{1b}[1;34mplain\u{1b}[0m"), "plain");
        assert_eq!(strip_ansi("no escapes here"), "no escapes here");
        // A truncated escape at the end of a line ends the scan rather than eating the line.
        assert_eq!(strip_ansi("tail\u{1b}[1;3"), "tail");
    }
}

#[cfg(test)]
mod listing_tests {
    use super::*;

    #[test]
    fn folders_are_read_with_their_paths_as_ids() {
        let body = r#"[
            {"uid":"a","name":{"ok":true,"value":"Shots"},"type":"folder"},
            {"uid":"b","name":{"ok":true,"value":"2026"},"type":"folder"}
        ]"#;
        let folders = parse_folders(body, "/my-files/Cosmic Capture Kit").expect("reads");
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[0].name, "Shots");
        assert_eq!(folders[0].id, "/my-files/Cosmic Capture Kit/Shots");
    }

    /// A destination picker shows folders, so a file in the listing is not one of them.
    #[test]
    fn files_are_dropped() {
        let body = r#"[
            {"uid":"a","name":{"ok":true,"value":"shot.png"},"type":"file"},
            {"uid":"b","name":{"ok":true,"value":"Shots"},"type":"folder"}
        ]"#;
        let folders = parse_folders(body, "/my-files").expect("reads");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "Shots");
    }

    /// A name whose signature did not verify cannot be turned into a path the tool would
    /// accept, so offering it would invite a click that could only fail.
    #[test]
    fn a_name_that_did_not_verify_is_dropped() {
        let body = r#"[
            {"uid":"a","name":{"ok":false,"value":""},"type":"folder"},
            {"uid":"b","name":{"ok":true,"value":"Shots"},"type":"folder"}
        ]"#;
        let folders = parse_folders(body, "/my-files").expect("reads");
        assert_eq!(folders.len(), 1, "only the verified folder is offered");
    }

    #[test]
    fn a_bare_string_name_is_tolerated() {
        let body = r#"[{"uid":"a","name":"Shots","type":"folder"}]"#;
        let folders = parse_folders(body, "/my-files").expect("reads");
        assert_eq!(folders[0].name, "Shots");
    }

    #[test]
    fn an_empty_listing_is_not_an_error() {
        assert!(parse_folders("[]", "/my-files").expect("reads").is_empty());
        assert!(parse_folders("[\n]\n", "/my-files").expect("reads").is_empty());
    }

    #[test]
    fn unreadable_output_is_an_error_not_an_empty_list() {
        for bad in ["", "not json", "{\"nope\":1}"] {
            assert!(parse_folders(bad, "/my-files").is_err(), "{bad:?}");
        }
    }
}

#[cfg(test)]
mod share_link_tests {
    use super::*;

    /// **The reply the tool actually prints** (DRAGON-522), field for field: the node's whole
    /// sharing state, with the link nested in the url access object. Reading only the top level
    /// is what made every Proton share link silently vanish.
    #[test]
    fn the_link_is_read_out_of_the_url_access_object() {
        let body = r#"{
            "protonInvitations":[],
            "nonProtonInvitations":[],
            "members":[],
            "urlAccess":{
                "uid":"vol~share",
                "creationTime":"2026-08-04T23:59:00.000Z",
                "role":"viewer",
                "url":"https://drive.proton.me/urls/ABC#key",
                "numberOfInitializedDownloads":0,
                "creatorEmail":"someone@proton.me"
            },
            "editorsCanShare":false
        }"#;
        assert_eq!(
            parse_share_url(body).expect("reads"),
            "https://drive.proton.me/urls/ABC#key"
        );
    }

    /// A reply with no url access object is a node shared with PEOPLE and not by link, so there
    /// is nothing to copy and the caller must be told rather than handed an invitation's field.
    #[test]
    fn a_reply_with_no_public_link_is_refused() {
        let body = r#"{"protonInvitations":[],"members":[],"editorsCanShare":false}"#;
        assert!(parse_share_url(body).is_err());
        // Present but empty, which is what an unshared node reports.
        assert!(parse_share_url(r#"{"urlAccess":null}"#).is_err());
        assert!(parse_share_url(r#"{"urlAccess":{}}"#).is_err());
    }

    #[test]
    fn the_link_is_read_from_any_of_the_known_keys() {
        for key in ["url", "publicLink", "publicUrl", "shareUrl", "link"] {
            // Nested, the shape the tool writes.
            let nested = format!(r#"{{"urlAccess":{{"{key}":"https://drive.proton.me/urls/ABC#key"}}}}"#);
            // And flat, tolerated for a future build that stops wrapping it.
            let flat = format!(r#"{{"{key}":"https://drive.proton.me/urls/ABC#key"}}"#);
            for body in [nested, flat] {
                assert_eq!(
                    parse_share_url(&body).expect("reads"),
                    "https://drive.proton.me/urls/ABC#key",
                    "key {key} in {body}"
                );
            }
        }
    }

    /// The host the registry allows for this provider has to be the host the tool's own links
    /// are on, or `child::run_cloud_upload` drops a link that parsed perfectly well. The two
    /// halves of the DRAGON-522 handoff are checked together for exactly that reason.
    #[test]
    fn the_parsed_link_is_one_this_app_will_offer() {
        let body = r#"{"urlAccess":{"url":"https://drive.proton.me/urls/ABC#key"}}"#;
        let url = parse_share_url(body).expect("reads");
        assert!(
            crate::cloud::web_url_allowed("proton", &url),
            "the registry must allow the host the tool puts its links on: {url}"
        );
    }

    /// This string becomes a click target, so anything that is not an https URL is refused
    /// rather than passed along.
    #[test]
    fn a_value_that_is_not_an_https_url_is_refused() {
        for body in [
            r#"{"url":"javascript:alert(1)"}"#,
            r#"{"url":"http://drive.proton.me/x"}"#,
            r#"{"url":""}"#,
            r#"{"other":"https://drive.proton.me/x"}"#,
            // The same refusals one level in, where the tool actually writes the value.
            r#"{"urlAccess":{"url":"javascript:alert(1)"}}"#,
            r#"{"urlAccess":{"url":"http://drive.proton.me/x"}}"#,
            r#"{"urlAccess":{"url":""}}"#,
            r#"{"urlAccess":{"role":"viewer"}}"#,
            "{}",
            "nonsense",
        ] {
            assert!(parse_share_url(body).is_err(), "{body}");
        }
    }
}

#[cfg(test)]
mod album_listing_tests {
    use super::*;

    /// An album's id is its UID under the ALBUMS root, never its name: a name lookup walks
    /// every album and two albums may share one, so a name-shaped id would address the wrong
    /// album as soon as a user made two called "Trips".
    #[test]
    fn albums_are_read_with_their_uids_as_ids() {
        let body = r#"[
            {"uid":"vol~a","name":{"ok":true,"value":"Holidays"},"type":"album"},
            {"uid":"vol~b","name":{"ok":true,"value":"Trips"},"type":"album"}
        ]"#;
        let albums = parse_albums(body).expect("reads");
        assert_eq!(albums.len(), 2);
        assert_eq!(albums[0].name, "Holidays");
        assert_eq!(albums[0].id, "/albums/vol~a");
        assert_eq!(albums[1].id, "/albums/vol~b");
    }

    /// The tool's own streaming array (one object per line, opened and closed on their own
    /// lines) is what actually arrives, so the parser has to take it as it comes.
    #[test]
    fn the_streaming_array_shape_is_read_as_it_arrives() {
        let body = "[\n{\"uid\":\"vol~a\",\"name\":{\"ok\":true,\"value\":\"Holidays\"},\"type\":\"album\"}\n]\n";
        assert_eq!(parse_albums(body).expect("reads").len(), 1);
        // And an empty iterable, which the tool writes as an array with nothing between.
        assert!(parse_albums("[\n\n]\n").expect("reads").is_empty());
    }

    /// A photo in the reply is not an album, and a picker of albums must not offer one.
    #[test]
    fn non_albums_are_dropped() {
        let body = r#"[
            {"uid":"vol~a","name":{"ok":true,"value":"shot.png"},"type":"photo"},
            {"uid":"vol~b","name":{"ok":true,"value":"Holidays"},"type":"album"}
        ]"#;
        let albums = parse_albums(body).expect("reads");
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].name, "Holidays");
    }

    /// A row whose name did not verify cannot be read, and a row with no uid cannot be
    /// addressed. Either way the press could only confuse or fail, so neither is offered.
    #[test]
    fn an_unreadable_or_unaddressable_album_is_dropped() {
        let body = r#"[
            {"uid":"vol~a","name":{"ok":false,"value":""},"type":"album"},
            {"uid":"","name":{"ok":true,"value":"Nameless"},"type":"album"},
            {"uid":"vol~c","name":{"ok":true,"value":"Holidays"},"type":"album"}
        ]"#;
        let albums = parse_albums(body).expect("reads");
        assert_eq!(albums.len(), 1, "only the readable, addressable album is offered");
        assert_eq!(albums[0].id, "/albums/vol~c");
    }

    #[test]
    fn unreadable_output_is_an_error_not_an_empty_list() {
        for bad in ["", "not json", "{\"nope\":1}"] {
            assert!(parse_albums(bad).is_err(), "{bad:?}");
        }
    }

    /// A created album is ONE object, not an array, and its uid is the whole reason this call is
    /// parsed at all: asking for it back by name afterwards would reintroduce exactly the
    /// ambiguity the uid avoids.
    #[test]
    fn a_created_album_is_read_with_its_new_uid() {
        let body = r#"{"uid":"vol~new","name":{"ok":true,"value":"Holidays"},"type":"album"}"#;
        let album = parse_created_album(body, "Holidays").expect("reads");
        assert_eq!(album.id, "/albums/vol~new");
        assert_eq!(album.name, "Holidays");
    }

    /// The typed name is the fallback when the reply's own name did not verify: the user typed
    /// it, so this app knows it, unlike a name in a listing.
    #[test]
    fn a_created_album_falls_back_to_the_typed_name() {
        let body = r#"{"uid":"vol~new","name":{"ok":false,"value":""},"type":"album"}"#;
        assert_eq!(parse_created_album(body, "Holidays").expect("reads").name, "Holidays");
    }

    /// With no uid there is nothing to address the album by, so this fails rather than handing
    /// back a row every later command would refuse.
    #[test]
    fn a_created_album_with_no_uid_is_an_error() {
        for bad in [r#"{"name":{"ok":true,"value":"Holidays"}}"#, r#"{"uid":""}"#, "nonsense"] {
            assert!(parse_created_album(bad, "Holidays").is_err(), "{bad}");
        }
    }
}

#[cfg(test)]
mod photo_upload_tests {
    use super::*;

    /// Every photo went in. The tool prints one result per photo and we send exactly one.
    #[test]
    fn an_all_ok_reply_is_a_success() {
        assert!(album_add_succeeded(r#"[{"uid":"vol~a","ok":true}]"#));
    }

    /// **An empty reply is a failure**, not a vacuous success: one photo went in, so exactly one
    /// result should come back, and nothing coming back means the tool did not do what was
    /// asked.
    #[test]
    fn an_empty_or_failed_reply_is_not_a_success() {
        assert!(!album_add_succeeded("[]"));
        assert!(!album_add_succeeded("[\n\n]\n"));
        assert!(!album_add_succeeded(r#"[{"uid":"vol~a","ok":false,"error":{}}]"#));
        assert!(!album_add_succeeded(r#"[{"uid":"a","ok":true},{"uid":"b","ok":false}]"#));
        assert!(!album_add_succeeded("nonsense"));
        assert!(!album_add_succeeded(""));
    }

    /// **A SKIP counts as landed.** With the conflict strategy this app passes, a photo whose
    /// name is already in the timeline is skipped rather than re-uploaded, and the honest
    /// reading of that is that the photo of that name IS there, which is exactly what the next
    /// step needs to be true.
    #[test]
    fn a_transfer_lands_when_it_uploaded_or_skipped() {
        assert!(transfer_landed(r#"{"transferredItems":1,"skippedItems":0,"failedItems":0}"#));
        assert!(transfer_landed(r#"{"transferredItems":0,"skippedItems":1,"failedItems":0}"#));
    }

    /// A failure never counts, whatever else the summary says, and a summary that could not be
    /// read at all is not a landing either: filing a photo that may not exist into an album is
    /// worse than reporting that the upload could not be confirmed.
    #[test]
    fn a_failure_or_an_unreadable_summary_never_counts_as_landed() {
        assert!(!transfer_landed(r#"{"transferredItems":1,"skippedItems":0,"failedItems":1}"#));
        assert!(!transfer_landed(r#"{"transferredItems":0,"skippedItems":0,"failedItems":0}"#));
        assert!(!transfer_landed("nonsense"));
        assert!(!transfer_landed(""));
    }

    /// The conflict strategy is load-bearing, not a preference: `keep-both` renames the arriving
    /// photo to a name this app never learns, and the name it then addresses would resolve to
    /// the OLDER photo, which would be the one filed into the album.
    #[test]
    fn the_photo_conflict_strategy_keeps_the_name_unambiguous() {
        assert_eq!(PHOTO_CONFLICT, "skip");
    }
}

#[cfg(test)]
mod destination_tests {
    use super::*;
    use crate::cloud::accounts::Destination;

    fn account(destination: Option<Destination>, folder_id: Option<&str>) -> CloudAccount {
        CloudAccount {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            provider: "proton".to_string(),
            label: String::new(),
            folder_id: folder_id.map(str::to_string),
            folder_name: None,
            added_at: String::new(),
            visibility: None,
            destination,
        }
    }

    /// An account with no stored kind, which is every account written before the field existed,
    /// uploads to a FOLDER.
    #[test]
    fn an_account_with_no_stored_kind_uploads_to_a_folder() {
        assert_eq!(account(None, None).destination_kind(), Destination::Files);
        assert_eq!(account_album(&account(None, Some("/albums/x"))), None);
    }

    /// The stored album is what an upload addresses.
    #[test]
    fn a_photos_account_uploads_to_its_stored_album() {
        let acct = account(Some(Destination::Photos), Some("/albums/vol~a"));
        assert_eq!(account_album(&acct).as_deref(), Some("/albums/vol~a"));
    }

    /// A Photos account that somehow has no album recorded falls back to the app's OWN album by
    /// name, which is the one the setup step finds or creates and therefore the one that exists.
    /// Silently uploading to the file tree instead would put captures somewhere the settings row
    /// does not name.
    #[test]
    fn a_photos_account_with_no_album_falls_back_to_the_apps_own() {
        for stored in [None, Some(""), Some("   ")] {
            let acct = account(Some(Destination::Photos), stored);
            assert_eq!(
                account_album(&acct).as_deref(),
                Some("/albums/Cosmic Capture Kit"),
                "{stored:?}"
            );
        }
    }

    /// **A Proton upload never leaves the meter's indeterminate face**, and that is the honest
    /// outcome rather than a gap: the tool disables its progress display when stdout is not a
    /// TTY, which is exactly how this file spawns it, so the only values `on_progress` ever
    /// sees are the synthetic 0 `providers::upload` makes before dispatching, this file's own
    /// 0, and its 100 at the end.
    ///
    /// Pinned here rather than trusted, because the meter's spinner is what a user actually sees
    /// for the whole of a Proton upload, and a provider that reported a fake midpoint would
    /// silently turn it into a bar that jumps 0 to 100.
    #[test]
    fn a_proton_upload_never_leaves_the_indeterminate_face() {
        let mut indeterminate = true;
        for percent in [0u8, 0, 100] {
            indeterminate = crate::cloud::upload::tray::still_indeterminate(indeterminate, percent);
        }
        assert!(
            indeterminate,
            "every value this provider reports is synthetic, so the meter must keep spinning"
        );
        // And none of them is genuine evidence of progress on its own.
        for percent in [0u8, 100] {
            assert!(!crate::cloud::upload::tray::is_genuine_progress(percent), "{percent}");
        }
    }
}
