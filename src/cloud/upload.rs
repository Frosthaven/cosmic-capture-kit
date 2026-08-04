//! The upload engine, PARENT side (DRAGON-482).
//!
//! Two jobs, and both belong to the process that still has the capture on screen:
//!
//! 1. [`stage_for_upload`] takes a snapshot of the file, so the editor is free to close,
//!    save somewhere else, or delete its temp while the transfer is still running.
//! 2. [`spawn_upload_child`] hands that snapshot to a DETACHED re-exec of ourselves
//!    (`--cloud-upload`), the same technique every other post-capture action uses. The
//!    child's whole life is [`super::child::run_cloud_upload`]; nothing about an upload
//!    blocks the editor, and nothing about the editor's exit stops an upload.
//!
//! The tray counter the child raises lives in [`tray`], mounted from here (see below).
//!
//! # Privacy
//!
//! The account id is in argv, and only the account id: it is random hex minted by
//! [`super::accounts::new_id`], so it identifies nothing about the user and is safe where
//! argv is world-readable. A TOKEN never goes near this path; the child loads its own from
//! [`super::secrets`]. Paths are described with [`crate::diag::path_shape`], never logged.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// The tray upload counter (stage A3).
///
/// Mounted HERE rather than declared in `cloud/mod.rs` because that file belongs to the
/// foundation stage and no later stage edits it (the module doc there says so). The
/// resulting path, `cloud::upload::tray`, reads correctly anyway: the counter exists only
/// for the duration of an upload and has no other caller. `#[path]` on a module outside an
/// inline block resolves against the DIRECTORY of this file, so the file itself sits at
/// `src/cloud/tray.rs`, beside its siblings.
#[path = "tray.rs"]
pub mod tray;

/// The marker every staged copy carries in its name.
///
/// **Load-bearing, not decoration** (the same rule as `preview::share::clipboard_temp_name`).
/// The staging directory is the session runtime dir, and with "Automatically save originals"
/// off the CAPTURE ITSELF lives there too. A staged copy named like its source would BE its
/// source, and the upload would be reading the file it is writing.
const UPLOAD_MARKER: &str = "cck-upload";

/// The default extension when a source has none, matching what a still is written as.
const DEFAULT_EXT: &str = "png";

/// How many uploads this process has staged. Part of the staged name, see [`staged_name`].
static STAGED: AtomicU32 = AtomicU32::new(0);

/// The file name a staged upload gets: `cck-upload-<pid>-<n>.<ext>`. Pure; unit-tested.
///
/// # Why this is NOT one fixed name
///
/// The precedent next door (`preview::snapshot_bake_source`) uses a fixed name on purpose,
/// because one document can only ever need one snapshot and a fixed name keeps the runtime
/// directory bounded. An upload breaks that premise: several can be in flight at once (the
/// feature is built for it, one child and one tray counter each), and a fixed name would
/// mean the second upload OVERWRITING the bytes the first is still reading, sending one
/// capture under another's name. So the name carries the process and a per-process
/// sequence, which is the same shape `share::clipboard::copy_text` and
/// `share::open::save_and_open` already use for their handoff temps.
///
/// Boundedness comes from the directory instead: the runtime dir is session-scoped (it is
/// cleared at logout), and the count is uploads-per-session, not per-percent.
///
/// Always a single path component (`file_name`, never a directory), so a caller's
/// `runtime_dir().join(..)` cannot be walked out of by a crafted source name.
pub fn staged_name(src: &Path, pid: u32, seq: u32) -> String {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| !e.is_empty() && !e.contains(['/', '\\', '.']))
        .unwrap_or(DEFAULT_EXT);
    format!("{UPLOAD_MARKER}-{pid}-{seq}.{ext}")
}

/// Above this size a staged copy goes to the DISK-backed cache directory instead of the
/// session runtime directory.
///
/// `$XDG_RUNTIME_DIR` is a tmpfs, so it is RAM, and the systemd default caps it at 10% of
/// physical memory (`util::transient_recording_dir`'s doc spells this out; it is why
/// recordings never buffer there). A still is a few megabytes and belongs in the runtime
/// dir, where the session's own lifetime tidies it up. A RECORDING can be gigabytes, and
/// copying one into RAM to upload it could ENOSPC the whole session. The threshold sits
/// above any plausible still and below any plausible recording; both destinations work for
/// either, so its exact value is not load-bearing.
const RUNTIME_DIR_MAX_BYTES: u64 = 32 * 1024 * 1024;

const _: () = assert!(
    RUNTIME_DIR_MAX_BYTES >= 16 * 1024 * 1024 && RUNTIME_DIR_MAX_BYTES <= 128 * 1024 * 1024,
    "DRAGON-482: below this band every ordinary screenshot is copied to disk for nothing; \
     above it a screen recording is staged into the runtime tmpfs, which is RAM"
);

/// Whether a staged copy of `bytes` should go to the DISK-backed directory rather than the
/// runtime tmpfs. Pure; unit-tested. An unknown size (a `metadata` that failed) answers
/// `true`, because the expensive mistake is the one that fills RAM.
pub fn stage_on_disk(bytes: Option<u64>) -> bool {
    bytes.is_none_or(|b| b > RUNTIME_DIR_MAX_BYTES)
}

/// Copy `src` into a staging directory and return the copy.
///
/// The copy is what the child uploads, and it OUTLIVES this process on purpose: the child
/// is detached, so the bytes have to belong to something the editor closing cannot take
/// away. It is also what the upload notification's click reveals when the provider makes no
/// share link, which is why it is not deleted at the end of the transfer.
///
/// Where it lands depends on how big it is ([`stage_on_disk`]). Both directories are
/// self-tidying: the runtime dir is cleared at logout, and the cache dir is swept by age
/// (`util::sweep_transient_recordings`), so neither can grow without bound even though this
/// function never deletes anything.
pub fn stage_for_upload(src: &Path) -> Result<PathBuf, String> {
    let seq = STAGED.fetch_add(1, Ordering::Relaxed);
    let name = staged_name(src, std::process::id(), seq);
    let size = std::fs::metadata(src).ok().map(|m| m.len());
    let dir = match stage_on_disk(size) {
        // `None` (no cache dir on this OS) falls back to the runtime dir: a staged copy in
        // RAM is still better than an upload that cannot start.
        true => crate::util::transient_recording_dir()
            .unwrap_or_else(|| PathBuf::from(crate::util::runtime_dir())),
        false => PathBuf::from(crate::util::runtime_dir()),
    };
    let dst = dir.join(name);
    match std::fs::copy(src, &dst) {
        Ok(_) => {
            log::debug!("cloud upload: staged a copy of the capture for a detached upload");
            Ok(dst)
        }
        Err(e) => {
            // The path is DESCRIBED, never printed: `path_shape` is enough to tell "the
            // runtime dir is full" from "the capture is gone" without the user's filesystem.
            log::warn!(
                "cloud upload: could not stage the capture ({} -> {}): {e}",
                crate::diag::path_shape(src),
                crate::diag::path_shape(&dst)
            );
            Err("The capture could not be prepared for upload.".to_string())
        }
    }
}

/// Launch the detached upload child for `staged`.
///
/// `session_id` is [`super::session::new_session_id`]'s output, minted by the CALLER
/// (DRAGON-490) so it can start watching for this exact session's progress/outcome
/// (`super::session::state_path`) the moment this returns `true`, rather than learning the
/// child's identity back from it. Pass `""` for a caller that has no cross-process watcher to
/// wire up (none exists in this codebase, but it keeps this function's contract honest: the
/// child treats an absent/blank session id as "nobody is watching" and simply never writes
/// the sidecar).
///
/// Returns whether the child was SPAWNED, which is the only thing this side can observe:
/// the transfer's own outcome happens after we let go, and reaches the user through the
/// child's tray counter and its notification. Same contract as `share::reexec::spawn_self`
/// everywhere else.
pub fn spawn_upload_child(staged: &Path, account_id: &str, auto_share: bool, session_id: &str) -> bool {
    use crate::share::reexec::{CLOUD_ACCOUNT, CLOUD_AUTO_SHARE, CLOUD_SESSION, CLOUD_UPLOAD, spawn_self};
    let mut extra = vec![CLOUD_ACCOUNT, account_id];
    if auto_share {
        extra.push(CLOUD_AUTO_SHARE);
    }
    if !session_id.is_empty() {
        extra.push(CLOUD_SESSION);
        extra.push(session_id);
    }
    let spawned = spawn_self(CLOUD_UPLOAD, staged, &extra);
    if spawned {
        // The account id is random hex and says nothing about the user; the file is not
        // named at all. Enough to follow one upload through the log.
        log::debug!("cloud upload: handed off to a detached child for account {account_id}");
    }
    spawned
}

#[cfg(test)]
mod staging_tests {
    use super::*;

    /// The marker is what keeps a staged copy from colliding with the capture it was made
    /// from, so it has to be in the name, and the name has to be one path component.
    #[test]
    fn a_staged_name_is_marked_and_is_a_single_component() {
        let name = staged_name(Path::new("/home/jane/Pictures/shot.png"), 42, 0);
        assert!(name.contains(UPLOAD_MARKER), "{name} carries no marker");
        assert!(!name.contains('/') && !name.contains('\\'), "{name} is not one component");
        assert_eq!(Path::new(&name).file_name().map(|n| n == name.as_str()), Some(true));
        // It can never equal the source's own name, which is the collision this exists for.
        assert_ne!(name, "shot.png");
    }

    /// The extension survives, because the provider (and the user's drive) reads it to know
    /// what the file is. A source with no usable extension gets the still default rather
    /// than an extensionless upload.
    #[test]
    fn the_extension_is_carried_across() {
        assert!(staged_name(Path::new("/tmp/a.png"), 1, 0).ends_with(".png"));
        assert!(staged_name(Path::new("/tmp/a.mp4"), 1, 0).ends_with(".mp4"));
        assert!(staged_name(Path::new("/tmp/a.JPEG"), 1, 0).ends_with(".JPEG"));
        // No extension, or one that would smuggle a path separator, falls back.
        assert!(staged_name(Path::new("/tmp/capture"), 1, 0).ends_with(".png"));
        assert!(staged_name(Path::new("/tmp/"), 1, 0).ends_with(".png"));
    }

    /// A still stages into the session runtime dir (a tmpfs); anything big enough to be a
    /// recording stages to disk instead, because the runtime dir is RAM and a multi-gigabyte
    /// copy there could take the whole session down with it.
    #[test]
    fn a_big_capture_stages_to_disk_rather_than_into_ram() {
        assert!(!stage_on_disk(Some(0)), "an empty file is not a recording");
        assert!(!stage_on_disk(Some(4 * 1024 * 1024)), "a still belongs in the runtime dir");
        assert!(!stage_on_disk(Some(RUNTIME_DIR_MAX_BYTES)), "the boundary itself is small");
        assert!(stage_on_disk(Some(RUNTIME_DIR_MAX_BYTES + 1)));
        assert!(stage_on_disk(Some(2 * 1024 * 1024 * 1024)), "a recording must not go to RAM");
        // An unknown size resolves toward the safe answer, not the convenient one.
        assert!(stage_on_disk(None));
    }

    /// **The reason this is not a fixed name.** Two uploads from one editor must stage to
    /// two files, or the second overwrites the bytes the first is still sending.
    #[test]
    fn two_uploads_from_one_process_never_share_a_file() {
        let src = Path::new("/tmp/shot.png");
        let a = staged_name(src, 7, 0);
        let b = staged_name(src, 7, 1);
        assert_ne!(a, b, "a second upload must not reuse the first's staging file");
        // And two PROCESSES uploading the same capture do not collide either.
        assert_ne!(staged_name(src, 7, 0), staged_name(src, 8, 0));
    }
}
