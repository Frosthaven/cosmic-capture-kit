//! Finalize pass: bake the live per-channel audio toggles into the recorded temp
//! file (amix + per-channel mute timeline) and mux the metadata into `out_path`.
//! Video is stream-copied so A/V sync is preserved exactly.
//!
//! DRAGON-127 retired the legacy wallclock+CFR+segments recorder, and with it the
//! per-channel A/V-sync delay-shift (`FinalizeDelays`, `shift_filter`) this pass
//! used to apply: the media-clock owned pipeline's FIFO output IS the final audio
//! (no wall-clock/segment-offset mapping needed), so finalize is now just
//! intervals → gate filters → amix → mux.

use std::path::PathBuf;
use std::process::Stdio;

/// How long the ink finalize may write NOTHING to its output before it is declared wedged
/// and killed (DRAGON-716).
///
/// Deliberately the same thirty seconds the old constant cap used, so that every pass which
/// already finished inside thirty seconds of continuous progress behaves exactly as it did.
/// Only the QUESTION changed: this is a stall budget, not a length limit. See
/// [`crate::record::wait_or_kill_progressing`] for why the old shape destroyed long
/// recordings.
///
/// It must stay at least as generous as the LIVE muxer's liveness budget, which the assert
/// below pins. A finalize output grows less regularly than a recording muxer's does: it is
/// re-encoding rather than passing packets through, and `-movflags +faststart` rewrites the
/// file at the end to move the moov atom, a phase during which the size need not climb at
/// all. Anything tighter than the live budget would be claiming this pass writes MORE
/// steadily than the one that just streamed to disk, which is backwards.
const INK_FINALIZE_STALL: std::time::Duration = std::time::Duration::from_secs(30);

const _: () = assert!(
    INK_FINALIZE_STALL.as_secs() >= crate::record::MUXER_LIVENESS_SECS,
    "the finalize stall budget must not be tighter than the live muxer's, or a re-encode \
     pausing over its faststart rewrite is killed as wedged"
);

/// Produce `out_path` from the temp capture, gating each audio channel to the
/// intervals it was toggled on. Video is stream-copied (no re-encode → no sync
/// drift); only the (cheap) audio is re-encoded.
// Only the LINUX workers call this directly; macOS and Windows go through
// `finalize_with_ink` because their window recordings can carry an overlay. It is also every
// e2e test's entry point, so it is live in any test build.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) fn finalize_with_intervals(
    temp: &std::path::Path,
    out_path: &std::path::Path,
    mic_off: &[(f64, f64)],
    sys_off: &[(f64, f64)],
    is_hevc: bool,
    metadata: &str,
) -> Result<PathBuf, String> {
    finalize_with_ink(temp, out_path, mic_off, sys_off, is_hevc, metadata, None)
}

/// [`finalize_with_intervals`] with the live annotation ink composited in (DRAGON-691
/// amendment 2), for a WINDOW recording on a platform that captures the window's own buffer.
///
/// # The one place a delivered recording lets go of its temp (DRAGON-748)
///
/// Every worker on every platform ends its session with a tail call to this function, or to
/// [`finalize_with_intervals`], which delegates here. So this is the SINGLE seam that knows
/// "the finalize succeeded and `out_path` is the delivered recording". On that arm, and only
/// on that arm, the pre-finalize temp is unlinked, best-effort.
///
/// **The invariant that makes this safe**: the GUI app already removed this file
/// unconditionally as soon as the worker posted `done` (`App::update_recording`'s
/// `RecordingPoll` arm), on every healthy recording that has ever shipped. Nothing downstream
/// can be relying on the temp surviving a successful finalize. The preview opens the DELIVERED
/// path carried in `done` and never the temp, and the "save originals" settings choose which
/// DIRECTORY that delivered file lands in, not whether the capture temp is kept. All this does
/// is move an existing deletion into the code that owns the file, which buys three things the
/// app's copy could not:
///
/// * the three `debug test` rec benches (`windows-record`, `mac-rec-bench`, `linux-rec-bench`)
///   call the worker directly, clean their OUTPUT and never had a deleter for the temp at all,
///   so their temps sat in the system temp dir for ever. That is recorded screen content, so
///   it is a privacy point and not only a disk one: 65 of them had accumulated on one box.
/// * a capture process that dies between a successful finalize and the app's poll no longer
///   leaves a temp behind for the NEXT session's sweep to rename into a stray
///   `<stamp>-recovered.mkv`, for a take that was in fact delivered.
/// * the app's own unlink becomes the belt and braces its comment always claimed it was.
///
/// The FAILURE arms are deliberately untouched. A finalize that did not deliver leaves the
/// temp exactly where it was, which is what the DRAGON-118 stop-tail salvage and
/// `recover.rs`'s rename-never-delete both depend on.
///
/// **With `ink: None` this is byte-for-byte the command finalize has always built**, and that
/// is a guarantee rather than an intention: an un-inked recording must not pay for a feature
/// it did not use, and the video must keep its stream copy. `finalize_ink_args` is the pure
/// statement of the diff and a test pins the `None` arm against the historical list.
///
/// With ink, exactly two things change:
///
/// * a second input appears, `rawvideo` RGBA on the child's stdin, and an `overlay` is
///   appended to the SAME `-filter_complex` the audio already uses;
/// * `-c:v copy` becomes an encode, because a filtered stream cannot be copied.
///
/// The AUDIO chain, its two mute gates and its `amix` are untouched, and the audio is still
/// encoded exactly once. That is the whole reason the overlay joins this pass instead of
/// running as a second one: the alternative shape, baking only the inked spans and
/// concatenating, would splice the audio, and the mixer's continuity guarantee is what keeps
/// `amix` from desynchronising (CLAUDE.md's recording invariants).
///
/// The ink is fed from media 0 to the last instant anything is on screen, then the pipe is
/// closed and `overlay`'s `eof_action=pass` lets the rest of the recording through untouched.
pub(crate) fn finalize_with_ink(
    temp: &std::path::Path,
    out_path: &std::path::Path,
    mic_off: &[(f64, f64)],
    sys_off: &[(f64, f64)],
    is_hevc: bool,
    metadata: &str,
    ink: Option<&InkJob<'_>>,
) -> Result<PathBuf, String> {
    let delivered = finalize_pass(temp, out_path, mic_off, sys_off, is_hevc, metadata, ink);
    // DRAGON-728: the pass is over however it ended, so the progress slot stops claiming a
    // phase is running. Here rather than at each of `finalize_pass`'s returns, for the same
    // reason the temp's deletion is here: this is the ONE seam every worker's tail call goes
    // through, and a phase left published would have the preview showing a stale readout for
    // as long as the app kept polling.
    super::finalize_progress::clear();
    // The success arm, and nothing else. See this function's doc for why the temp is ours to
    // drop here and why every failure arm leaves it alone.
    if delivered.is_ok() && finalize_owns_temp(temp) {
        let _ = std::fs::remove_file(temp);
    }
    delivered
}

/// **Pure**, unit-tested (DRAGON-748): whether finalize OWNS `temp` and may delete it once it
/// has delivered the recording.
///
/// The answer is the NAME's shape, asked through the very predicate
/// `recover::sweep_unclaimed_temps` uses to decide which abandoned wreckage is ours to touch,
/// so the two can never disagree about what a recording temp is. A path our own
/// [`crate::record::recording_temp_path`] minted is a file this pass has just consumed and
/// nothing else will ever read again.
///
/// Anything else is a file the caller merely POINTED us at, and deleting one of those would be
/// a surprise rather than a service. That is not hypothetical: the ffmpeg-backed e2e tests hand
/// this pass an ordinary fixture and then keep using it. `av_sync_tests` re-reads its memoised
/// fixture after finalizing, to prove the video was stream-copied frame for frame, and shares
/// that one fixture across three tests; `ink_bake_e2e_tests` finalizes the SAME fixture twice,
/// once plain and once inked, to prove baking ink leaves the audio alone. Neither path is a
/// recording temp, and neither test should have to be rewritten to accommodate a cleanup that
/// was never about it.
fn finalize_owns_temp(temp: &std::path::Path) -> bool {
    temp.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(super::recover::is_recording_temp)
}

/// The finalize pass itself: build the command, run it, and report the result. Split out from
/// [`finalize_with_ink`] (DRAGON-748) purely so that the temp's deletion has ONE success arm to
/// hang off, rather than the three this body returns through (the no-ink `finalize_result`, the
/// hardware ink attempt, and the software ink fallback). The behaviour here is unchanged.
fn finalize_pass(
    temp: &std::path::Path,
    out_path: &std::path::Path,
    mic_off: &[(f64, f64)],
    sys_off: &[(f64, f64)],
    is_hevc: bool,
    metadata: &str,
    ink: Option<&InkJob<'_>>,
) -> Result<PathBuf, String> {
    let mic_f = chan_filter(mic_off);
    let sys_f = chan_filter(sys_off);
    let mut fc = format!(
        "[0:a:0]{mic_f}[m];[0:a:1]{sys_f}[s];\
         [m][s]amix=inputs=2:duration=longest:normalize=0[a]"
    );
    if ink.is_some() {
        // `eof_action=pass` and `repeatlast=0`: once the ink stream ends the rest of the
        // recording passes through with nothing drawn on it, rather than the last ink frame
        // being held over it for ever.
        fc.push_str(";[0:v:0][1:v]overlay=0:0:eof_action=pass:repeatlast=0[v]");
    }

    // Embed how this was recorded (read back by `debug inspect`). The App wrote
    // `codec=auto` before the encode plan existed — annotate the outcome here,
    // where the plan's actual pick is known.
    let metadata = annotate_codec(metadata, is_hevc);

    let Some(job) = ink else {
        // Copying HEVC into mp4 needs the hvc1 tag for QuickTime/Apple players (h264
        // doesn't). We chose the encoder, so `is_hevc` is known from the plan — no
        // ffprobe needed.
        let mut cmd = build_finalize_cmd(temp, out_path, &fc, &metadata, is_hevc, None, &[]);
        // DRAGON-728: a named wait and no number. This pass stream-copies the video and
        // re-encodes only the (cheap) audio, and nothing in this process can count it: see
        // `finalize_progress`'s module doc for the measurement showing `-loglevel error`
        // leaves ffmpeg's own progress line unprinted, and for why adding `-stats` is not
        // ours to do.
        super::finalize_progress::publish(super::finalize_progress::Progress {
            phase: super::finalize_progress::Phase::Muxing,
            done: 0,
            total: 0,
        });
        // `output()` waits for exit, exactly as `status()` did: no new unbounded wait is
        // introduced here, and this one was already unbounded (it is not a stop tail; the
        // process is not being torn down around it).
        let out = cmd
            .output()
            .map_err(|e| format!("could not start ffmpeg finalize: {e}"))?;
        return finalize_result(out.status, &out.stderr, out_path);
    };

    // DRAGON-716: the recording's OWN encoder tier first, software only as a fallback.
    // Trying hardware and falling back on failure is what makes reusing the tier safe: the
    // worst case is one wasted attempt and then exactly the pass this code has always run,
    // so a finalize can never come out worse than it did before the tier reuse existed.
    if let Some(hw) = ink_hw_video_args(job.encoder, is_hevc, job.kbps) {
        match run_ink_pass(temp, out_path, &fc, &metadata, is_hevc, job, &hw, 1) {
            Ok(p) => return Ok(p),
            Err(e) => log::warn!(
                "ink bake: the {} finalize re-encode failed ({e}); retrying in software, \
                 which is what this pass did before it learned to reuse the recording's \
                 encoder (DRAGON-716)",
                job.encoder
            ),
        }
    }
    run_ink_pass(temp, out_path, &fc, &metadata, is_hevc, job, &ink_sw_video_args(), 2)
}

/// Build the finalize invocation. `ink` carries the overlay input's size and frame rate when
/// there is one, and `video` the `-c:v …` args the caller chose.
///
/// Split out (DRAGON-716) so the ink path can build the SAME command twice, once per encoder
/// attempt. With `ink: None` the argv is byte-identical to what finalize has always built.
fn build_finalize_cmd(
    temp: &std::path::Path,
    out_path: &std::path::Path,
    fc: &str,
    metadata: &str,
    is_hevc: bool,
    ink: Option<((u32, u32), u32)>,
    video: &[String],
) -> std::process::Command {
    let mut cmd = crate::util::ffmpeg_command();
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
        .arg("-i")
        .arg(temp);
    if let Some((size, fps)) = ink {
        cmd.args(["-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", &format!("{}x{}", size.0, size.1)])
            .args(["-r", &fps.max(1).to_string()])
            .args(["-i", "pipe:0"]);
    }
    cmd.args(["-filter_complex", fc]);
    for a in finalize_ink_args(ink.is_some().then_some(video), is_hevc) {
        cmd.arg(a);
    }
    cmd.args(["-metadata", &format!("comment={metadata}")])
        .args(["-metadata", "title=Cosmic Capture Kit"]);
    cmd.args(["-movflags", "+faststart"])
        .arg(out_path)
        .stdin(if ink.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::null())
        // DRAGON-432: was `inherit`, which on a Windows GUI-subsystem launch and a Linux
        // hotkey launch inherits nothing — the finalize re-encode's explanation of why it
        // failed went nowhere. `output()` both captures AND drains it, so unlike a
        // hand-rolled pipe there is no way to fill it and deadlock.
        .stderr(Stdio::piped());
    cmd
}

/// Run ONE ink finalize attempt with `video` as its codec args: spawn, feed the overlay
/// frames, close the pipe, reap.
///
/// Called at most twice (DRAGON-716), hardware then software. Re-feeding the frames on the
/// second attempt costs a repeat of the raster, which is deterministic and cheap next to the
/// encode, and is the price of not holding gigabytes of RGBA in memory just in case.
/// `attempt` is which of those two this is, and it exists for the progress readout: the
/// fallback feeds the same frames again from zero, so the bar has to restart rather than sit
/// full for the whole of the slower pass (DRAGON-728).
// One command's worth of finalize inputs, threaded rather than gathered into a struct that
// would exist only to satisfy the lint.
#[allow(clippy::too_many_arguments)]
fn run_ink_pass(
    temp: &std::path::Path,
    out_path: &std::path::Path,
    fc: &str,
    metadata: &str,
    is_hevc: bool,
    job: &InkJob<'_>,
    video: &[String],
    attempt: u32,
) -> Result<PathBuf, String> {
    // Start from NO output file, every attempt (DRAGON-716). The reap below watches this path
    // grow and keeps a high-water mark, so it needs the size to be monotonic for the life of
    // one child. A failed hardware attempt leaves a partial file here, and the software retry's
    // ffmpeg TRUNCATES it back to zero on open: without this removal the retry's readings would
    // sit under the dead attempt's mark for most of the encode, the stall clock would never
    // reset, and the fallback that exists to guarantee "never worse than before" would itself
    // be killed at the budget. Cheap, and it also means a doomed pass cannot leave a half file
    // sitting where a finished recording belongs.
    let _ = std::fs::remove_file(out_path);
    let mut cmd =
        build_finalize_cmd(temp, out_path, fc, metadata, is_hevc, Some((job.video, job.fps)), video);
    // The INK path cannot use `output()`: that reads stdout and stderr to EOF before it waits,
    // and this child is waiting on US for its second input, so nothing would ever reach EOF.
    // Feed it, close the pipe, then reap.
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start ffmpeg finalize: {e}"))?;
    // The stderr ring, attached immediately: a piped stderr nobody drains fills its 64 KB
    // buffer and deadlocks ffmpeg, which is the one way this pass could hang for ever.
    super::ffmpeg_log::attach(&mut child);
    let pid = child.id();
    let frames = crate::record::ink_bake::frame_count(&job.bake.log, job.fps)
        .unwrap_or(job.video_frames)
        .min(job.video_frames);
    // DRAGON-728: the FEED is the phase the user waits through, and it is the one phase in
    // this pass that can be counted. The denominator is the whole video, never `frames`: this
    // command re-encodes every frame of the recording whether or not the ink reaches the end,
    // so a drawing that stops a minute into a ten minute take must not read 100% with nine
    // minutes still to go.
    let baking = |done: u64| super::finalize_progress::Progress {
        phase: super::finalize_progress::Phase::BakingInk { attempt },
        done,
        total: job.video_frames,
    };
    super::finalize_progress::publish(baking(0));
    // The denominator the user's percentage is counted against, at the START of the wait.
    // The existing `fed N of M` line only lands once the feed is over, which is exactly the
    // span a report of "it looked stuck" is about.
    log::debug!(
        "ink bake: attempt {attempt} is feeding {frames} overlay frames over a {} frame video",
        job.video_frames
    );
    if let Some(mut sink) = child.stdin.take() {
        let written = crate::record::ink_bake::write_frames(
            job.bake,
            job.video,
            job.fps,
            frames,
            &mut sink,
            |done| super::finalize_progress::publish(baking(done)),
        );
        // Dropping the handle is what delivers EOF, and it has to happen before the reap or
        // ffmpeg waits for an input that is never coming. Explicit, because the borrow would
        // otherwise live to the end of the scope.
        drop(sink);
        log::debug!("ink bake: fed {written} of {frames} overlay frames to finalize");
    }
    // The overlay is fed; ffmpeg is still encoding whatever is left of the video with nothing
    // drawn on it, and then rewriting the moov atom for `+faststart`. Nothing here can count
    // that, so the phase says what it is doing and hands the spinner back rather than leaving
    // a bar parked on its last value (DRAGON-728).
    super::finalize_progress::publish(super::finalize_progress::Progress {
        phase: super::finalize_progress::Phase::FinishingInk { attempt },
        done: frames,
        total: job.video_frames,
    });
    // BOUNDED, unlike the no-ink path above, and for a reason that is new here: this child is
    // being fed by us, so a wedge is reachable in a way it is not when ffmpeg simply reads a
    // file.
    //
    // DRAGON-716: bounded by having STOPPED WRITING, not by a wall clock. This was
    // `wait_or_kill(child, 30s)`, a CONSTANT cap over a workload that scales with the
    // recording: the ink arm re-encodes the whole video (see `ink_hw_video_args`), measured
    // at 22.1s for a 2 minute 1080p take in software on a quiet 16-core desktop. So the cap
    // was a LENGTH LIMIT, and past it the encode was SIGKILLed and the recording lost, after
    // the user had already watched the spinner for the full budget. Same number, different
    // question; see `wait_or_kill_progressing`. It still matters with a hardware tier: that
    // is 4.5x faster here, not instant, and the software fallback below it is the slow one.
    let status = super::wait_or_kill_progressing(&mut child, INK_FINALIZE_STALL, || {
        std::fs::metadata(out_path).map(|m| m.len()).unwrap_or(0)
    })
    .map_err(|e| format!("ffmpeg finalize could not be reaped: {e}"))?;
    let hint = super::ffmpeg_log::take_hint(pid);
    if status.success() {
        return Ok(out_path.to_path_buf());
    }
    Err(super::ffmpeg_log::with_hint(
        format!("ffmpeg finalize exited with {status}"),
        hint,
    ))
}

/// What a finalize pass needs to composite one session's ink.
///
/// Borrowed rather than owned: the plan belongs to the app, and the worker reads it once at
/// the end of the session.
pub(crate) struct InkJob<'a> {
    /// The strokes and the window's rect timeline.
    pub bake: &'a crate::record::ink_bake::InkBake,
    /// The ENCODED frame size, which is what the overlay is rasterized at.
    pub video: (u32, u32),
    /// The recording's frame rate.
    pub fps: u32,
    /// How many frames the video holds, the ceiling on what the bake will feed.
    pub video_frames: u64,
    /// The stable id of the encoder tier the RECORDING itself ran on
    /// ([`crate::encode::EncodePlan::encoder_id`]), so the bake's unavoidable re-encode can
    /// run on the SAME hardware instead of always falling to software (DRAGON-716).
    ///
    /// Read off the plan the worker already built, never re-resolved: re-walking the probe
    /// ladder would re-run every hardware probe and is not even guaranteed to give the same
    /// answer twice (see `encoder_id`'s own doc).
    pub encoder: &'static str,
    /// The video bitrate that recording used, in kbps, so a bitrate-targeted tier re-encodes
    /// to the same size envelope it was recorded at rather than to some ffmpeg default.
    pub kbps: u32,
}

/// **Pure**, unit-tested (DRAGON-716): the `-c:v …` arguments the ink finalize re-encodes
/// with when the recording ran on a HARDWARE tier, or `None` when it must use software.
///
/// # Why the tier is reused at all
///
/// The ink overlay forces a full re-encode of the video (a filtered stream cannot be
/// stream-copied), and doing that in software made the user wait out the whole encode after
/// pressing stop: measured at 22.1s for a 2 minute 1080p take on a quiet 16-core desktop,
/// against 3.6s for the un-inked stream copy. The recording itself had just been produced by
/// a hardware encoder on the same machine, so the finalize falling to `libx264` was not a
/// safety property, it was an oversight. Same job on the same box, back to back: `libx264
/// -preset veryfast -crf 18` 57.0s, `h264_nvenc` 12.7s.
///
/// Quality is not a new decision either. The tier was already trusted to produce the
/// recording, so this re-encodes on that tier, at the bitrate that recording used, in the
/// rate-control SHAPE `encode::command` gives it: constant-quality for the one tier whose
/// plan sets `quality_rc`, a bitrate target for the rest.
///
/// # What is deliberately NOT carried over
///
/// The capture plans' realtime tunings (`-realtime true`, `-quality speed`, `-preset
/// veryfast`, `-tune zerolatency`) exist so a LIVE encoder never falls behind the screen.
/// A finalize has no such deadline, so leaving them off spends a little more time for a
/// better picture, on a pass that is already bounded by its own stall budget.
///
/// # Media Foundation needs two more arguments, and NEITHER of them is a tuning
///
/// DRAGON-755, found on this arm's first-ever execution anywhere. `mf` alone among the tiers
/// does not name an encoder, it names Windows' WRAPPER over whatever encoder MFT the machine
/// exposes, so which MFT answers is itself an argument. This arm asked for neither the
/// hardware MFT nor the format that MFT takes, and the owner's inked 5120x1440 take died
/// inside its first frames with `ffmpeg finalize exited with exit code: 0xdfaba7bb`. The
/// DRAGON-716 fallback held and the software pass delivered the recording, so nothing was
/// lost, but the tier reuse had bought exactly nothing.
///
/// Measured headlessly on the owner's box (AMD, ffmpeg 9.0 with `--enable-mediafoundation`),
/// through the real `build_finalize_cmd` argv over a recording-shaped temp and a synthetic
/// RGBA overlay stream. Four rows, and only the last one runs:
///
/// | `-hw_encoding` | `-pix_fmt` | which MFT answers | result |
/// |---|---|---|---|
/// | absent | `yuv420p` | `HEVCVideoExtensionEncoder` (software) | `failed processing input: 80004005`, exit `0xdfaba7bb` |
/// | absent | `nv12` | `HEVCVideoExtensionEncoder` (software) | the same E_FAIL at the first frame |
/// | `true` | `yuv420p` | `AMDh265Encoder` (hardware) | `format negotiation failed (1/0)` |
/// | `true` | `nv12` | `AMDh265Encoder` (hardware) | encodes, HEVC, ink composited |
///
/// So both are load-bearing and neither covers the other. Two things follow that are worth
/// stating rather than re-deriving:
///
/// * **`-hw_encoding true` is not tuning, it is WHICH ENCODER.** Without it ffmpeg enumerates
///   only software MFTs, so `h264_mf` quietly re-encoded on Microsoft's `H264 Encoder MFT`,
///   on the CPU. That row does not fail, which is worse: it is this pass claiming to reuse the
///   recording's hardware while doing the one thing the tier reuse exists to stop. HEVC has no
///   working software MFT to fall on, which is the only reason the defect was ever visible.
/// * **The NV12 feed is DRAGON-708's measurement, re-met from the other side.** A hardware MFT
///   advertises NV12 and nothing else, so the moment the hardware is forced, `yuv420p` is
///   refused outright. That ticket learned it on the PROBE and wrote it down; the finalize arm
///   was written later and inherited the table's shared `-pix_fmt yuv420p` line instead.
///
/// A box with no hardware MFT now fails attempt 1 and takes the software fallback, which is
/// the DRAGON-716 design working as intended. It is also close to unreachable: a session whose
/// `encoder_id` is `mf` got that id from `encode::plan::mf_plan`, whose probe forces the
/// hardware too, so the tier could not have been selected on such a box in the first place.
///
/// **What is still deliberately left off**, so nobody adds it to be tidy: the `mf` plan's
/// `-rate_control pc_vbr`, `-scenario display_remoting` and `-g 60`. The bitrate triple below
/// already IS peak-constrained VBR, the scenario is a live screen-content hint, and no other
/// hardware arm here sets a GOP length either. Each one is another option an untested vendor
/// MFT could refuse, and this arm has exactly one QA box in the fleet.
///
/// # Why `vaapi` is not here
///
/// VAAPI needs `-vaapi_device`, a `LIBVA_DRIVER_NAME` in the environment and a
/// `format=nv12,hwupload` in the filtergraph, i.e. a different GRAPH shape from the overlay
/// chain this pass already builds, not just a different `-c:v`. It is also unreachable
/// where it would matter: the only platforms that bake ink are macOS and Windows, and
/// `vaapi_device()` walks `/dev/dri`. Software is the honest answer for it, and the caller's
/// fallback makes that answer safe rather than merely acceptable.
///
/// `None` here is never a failure: every caller falls back to [`ink_sw_video_args`], which
/// is what this pass has always used.
fn ink_hw_video_args(encoder_id: &str, is_hevc: bool, kbps: u32) -> Option<Vec<String>> {
    let kbps = kbps.max(100);
    let mut v: Vec<String> = Vec::new();
    let mut push = |args: &[&str]| v.extend(args.iter().map(|s| (*s).to_string()));
    // The bitrate target `encode::command` builds for a `quality_rc: false` plan, repeated
    // in the same order so the two read alike.
    let bitrate = |push: &mut dyn FnMut(&[&str])| {
        push(&["-b:v", &format!("{kbps}k")]);
        push(&["-maxrate", &format!("{kbps}k")]);
        push(&["-bufsize", &format!("{}k", kbps * 2)]);
    };
    // What the encoder is asked to produce. Every arm but one wants YUV420P, the format an
    // ordinary player expects, and the `mf` arm's departure from it is a hardware constraint
    // rather than a preference (DRAGON-755; the doc above has the measurement).
    let mut pix_fmt = "yuv420p";
    match encoder_id {
        // The one tier whose plan is `quality_rc: true`: constant-quality VBR with the
        // bitrate acting as a peak cap. `-cq 19` rather than the recording's 23 because this
        // is a re-encode of an already-encoded source, and a generation of loss is worth
        // spending a few bits to avoid. The same reason the software arm has always used
        // `-crf 18` where the recording plan uses 23.
        "nvenc" => {
            push(if is_hevc { &["-c:v", "hevc_nvenc"] } else { &["-c:v", "h264_nvenc"] });
            push(&["-rc", "vbr", "-cq", "19", "-b:v", "0"]);
            push(&["-maxrate", &format!("{kbps}k")]);
            push(&["-bufsize", &format!("{}k", kbps * 2)]);
        }
        "videotoolbox" => {
            push(if is_hevc {
                &["-c:v", "hevc_videotoolbox"]
            } else {
                &["-c:v", "h264_videotoolbox"]
            });
            bitrate(&mut push);
        }
        "amf" => {
            push(if is_hevc { &["-c:v", "hevc_amf"] } else { &["-c:v", "h264_amf"] });
            bitrate(&mut push);
        }
        "qsv" => {
            push(if is_hevc { &["-c:v", "hevc_qsv"] } else { &["-c:v", "h264_qsv"] });
            bitrate(&mut push);
        }
        "mf" => {
            push(if is_hevc { &["-c:v", "hevc_mf"] } else { &["-c:v", "h264_mf"] });
            // DRAGON-755, and BOTH of these or the arm does not run at all. The same
            // constant the `mf` plan and its probe carry, so the three cannot drift into
            // asking Media Foundation three different questions.
            push(&crate::encode::MF_HW_ENCODING);
            pix_fmt = "nv12";
            bitrate(&mut push);
        }
        // "vaapi" (see the doc above), "software", and anything a future ladder adds that
        // nobody has taught this table about. Falling through to software is always safe.
        _ => return None,
    }
    push(&["-pix_fmt", pix_fmt]);
    // Copying HEVC into mp4 needs the hvc1 tag for QuickTime and Apple players, exactly as
    // the un-inked stream-copy arm does. The software arm never needs it because libx264
    // cannot produce HEVC; a hardware arm can, so the tag comes back here.
    if is_hevc {
        push(&["-tag:v", "hvc1"]);
    }
    Some(v)
}

/// **Pure**, unit-tested: the SOFTWARE `-c:v …` arguments for the ink finalize, which is
/// what this pass used unconditionally before DRAGON-716 and is still both the answer for a
/// tier with no hardware arm and the fallback when a hardware attempt FAILS.
///
/// Byte-identical to the historical list, and a test pins it that way: the guarantee is that
/// a finalize can never come out worse than it did before the tier reuse existed.
fn ink_sw_video_args() -> Vec<String> {
    // The preview editor's re-encode flags (`edit::VIDEO_REENCODE`), repeated rather than
    // shared because that constant lives in the app and this is the record path; the two
    // are the same choice for the same reason and are worth keeping in step by hand.
    ["-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

/// **Pure**, unit-tested: the codec and mapping arguments finalize passes, with and without an
/// ink overlay.
///
/// Split out so the ONE thing that must never regress is testable without running ffmpeg: an
/// un-inked recording still STREAM-COPIES its video. The ink arm is the only place in the
/// record path that encodes video at finalize, and it does so because a filtered stream cannot
/// be copied, not by choice.
fn finalize_ink_args(ink_video: Option<&[String]>, is_hevc: bool) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    let mut push = |args: &[&str]| v.extend(args.iter().map(|s| (*s).to_string()));
    if let Some(video) = ink_video {
        push(&["-map", "[v]", "-map", "[a]"]);
        // The video codec args are chosen by the caller now (DRAGON-716): the recording's own
        // hardware tier when it has a finalize arm ([`ink_hw_video_args`]), and
        // [`ink_sw_video_args`] otherwise or after a hardware attempt failed. Any `-tag:v
        // hvc1` an HEVC arm needs is already inside that list, since only the arm that
        // picked the encoder knows whether it produced HEVC.
        v.extend(video.iter().cloned());
        let mut push = |args: &[&str]| v.extend(args.iter().map(|s| (*s).to_string()));
        push(&["-c:a", "aac", "-b:a", "192k"]);
        return v;
    }
    push(&["-map", "0:v:0", "-map", "[a]"]);
    push(&["-c:v", "copy", "-c:a", "aac", "-b:a", "192k"]);
    if is_hevc {
        push(&["-tag:v", "hvc1"]);
    }
    v
}

/// The shared tail of both paths: turn an exit status plus captured stderr into the finalize
/// result, logging the tail when it failed.
fn finalize_result(
    status: std::process::ExitStatus,
    stderr: &[u8],
    out_path: &std::path::Path,
) -> Result<PathBuf, String> {
    if status.success() {
        return Ok(out_path.to_path_buf());
    }
    let text = String::from_utf8_lossy(stderr);
    let mut tail = super::ffmpeg_log::StderrTail::new();
    for line in text.lines() {
        tail.push_line(line);
    }
    if tail.len() > 0 {
        log::error!(
            "DRAGON-432: ffmpeg finalize failed; stderr tail:\n{}",
            tail.log_block(super::ffmpeg_log::LOG_TAIL_LINES)
        );
    }
    let lines: Vec<&str> = tail.lines().collect();
    Err(super::ffmpeg_log::with_hint(
        format!("ffmpeg finalize exited with {status}"),
        super::ffmpeg_log::likely_error_line(&lines),
    ))
}

#[cfg(test)]
mod finalize_owns_temp_tests {
    use super::*;
    use std::path::Path;

    /// The shape [`crate::record::recording_temp_path`] mints is the shape finalize deletes.
    ///
    /// Asked of a path that function really produced, never of a hand-typed literal: a change
    /// to the minting rule must not be able to leave this test happily agreeing with a shape
    /// nothing produces any more, because the only symptom would be the leak coming back
    /// silently.
    #[test]
    fn a_temp_this_crate_minted_is_finalizes_to_delete() {
        for out in [
            "/home/u/Videos/2026-08-21-16-11-53-493.mp4",
            "/tmp/cck-winrec-4242.mp4",
            "recording.mp4",
        ] {
            let temp = crate::record::recording_temp_path(Path::new(out));
            assert!(
                finalize_owns_temp(&temp),
                "the temp minted for {out} ({}) must be finalize's to delete",
                temp.display()
            );
        }
    }

    /// A file the caller merely POINTED at is never deleted, however successful the pass was.
    #[test]
    fn a_file_the_caller_merely_pointed_at_is_never_deleted() {
        for other in [
            // The two ffmpeg-backed e2e fixtures. Both are handed to this pass and then read
            // again, so deleting either would break tests that are not about temps at all.
            "/tmp/cck-av-sync-fixture-1234.mkv",
            "/tmp/cck-inkbake-audio-1234.mkv",
            // A DELIVERED recording is never its own temp.
            "/home/u/Videos/2026-08-21-16-11-53-493.mp4",
            // A recovered take, which `recover.rs` renamed precisely so it would survive.
            "/home/u/Videos/2026-08-21-16-11-53-493-recovered.mkv",
            // Hidden, but not one of ours.
            "/tmp/.hidden.mkv",
            // The bare suffix with no stem in front of it.
            "/tmp/.recording.mkv",
        ] {
            assert!(!finalize_owns_temp(Path::new(other)), "{other} is not finalize's to delete");
        }
    }
}

#[cfg(test)]
mod finalize_ink_args_tests {
    use super::*;

    /// **The guarantee**: a recording with no ink builds exactly the arguments it always did,
    /// and its video is STREAM-COPIED. A feature nobody used must cost nothing, and a video
    /// re-encode is the most expensive thing this pass could accidentally start doing.
    #[test]
    fn a_recording_with_no_ink_still_stream_copies_its_video() {
        assert_eq!(
            finalize_ink_args(None, false),
            vec!["-map", "0:v:0", "-map", "[a]", "-c:v", "copy", "-c:a", "aac", "-b:a", "192k"]
        );
        // And HEVC still gets its QuickTime tag.
        assert_eq!(
            finalize_ink_args(None, true),
            vec![
                "-map", "0:v:0", "-map", "[a]", "-c:v", "copy", "-c:a", "aac", "-b:a", "192k",
                "-tag:v", "hvc1"
            ]
        );
    }

    /// The ink arm maps the OVERLAY's output and encodes the video, because a filtered stream
    /// cannot be copied. The audio arguments are the same ones either way, which is what keeps
    /// the audio path out of this change entirely.
    #[test]
    fn the_ink_arm_encodes_the_video_and_leaves_the_audio_alone() {
        let sw = ink_sw_video_args();
        let args = finalize_ink_args(Some(&sw), false);
        assert!(args.starts_with(&["-map".into(), "[v]".into(), "-map".into(), "[a]".into()]));
        assert!(args.windows(2).any(|w| w[0] == "-c:v" && w[1] == "libx264"));
        assert!(!args.iter().any(|a| a == "copy"), "a filtered stream cannot be copied");
        // The audio arguments are identical to the no-ink arm's.
        let audio = |v: &[String]| {
            v.windows(4)
                .find(|w| w[0] == "-c:a")
                .map(|w| w.to_vec())
                .expect("audio args")
        };
        assert_eq!(audio(&args), audio(&finalize_ink_args(None, false)));
    }

    /// The SOFTWARE arm re-encodes to h264, so it must NOT carry the hvc1 tag whatever the
    /// recording was: that tag on an h264 stream is a lie players act on.
    #[test]
    fn the_software_arm_never_carries_the_hevc_tag() {
        let sw = ink_sw_video_args();
        assert!(!finalize_ink_args(Some(&sw), true).iter().any(|a| a == "hvc1"));
    }

    /// **The never-worse-than-before guarantee** (DRAGON-716): the software arm is byte for
    /// byte the list this pass used unconditionally before it learned to reuse the recording's
    /// encoder. It is both the answer for a tier with no hardware arm and the fallback after a
    /// hardware attempt fails, so if this drifts, a fallback silently stops being a fallback.
    #[test]
    fn the_software_arm_is_the_historical_list() {
        assert_eq!(
            ink_sw_video_args(),
            vec!["-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p"]
        );
    }
}

/// The ink finalize's ENCODER choice (DRAGON-716).
///
/// The rule these pin: the bake re-encodes on the tier that produced the recording, and
/// anything this table does not know about falls to software rather than guessing.
#[cfg(test)]
mod ink_hw_video_args_tests {
    use super::*;

    fn args(id: &str, is_hevc: bool) -> Vec<String> {
        ink_hw_video_args(id, is_hevc, 8000).unwrap_or_else(|| panic!("{id} should have an arm"))
    }

    fn has_pair(v: &[String], a: &str, b: &str) -> bool {
        v.windows(2).any(|w| w[0] == a && w[1] == b)
    }

    /// Every tier reachable on a platform that BAKES (macOS and Windows) gets a hardware arm,
    /// named for the codec the recording actually produced.
    #[test]
    fn each_baking_platforms_tier_maps_to_its_own_encoder() {
        assert!(has_pair(&args("nvenc", false), "-c:v", "h264_nvenc"));
        assert!(has_pair(&args("nvenc", true), "-c:v", "hevc_nvenc"));
        assert!(has_pair(&args("videotoolbox", false), "-c:v", "h264_videotoolbox"));
        assert!(has_pair(&args("videotoolbox", true), "-c:v", "hevc_videotoolbox"));
        assert!(has_pair(&args("amf", false), "-c:v", "h264_amf"));
        assert!(has_pair(&args("qsv", false), "-c:v", "h264_qsv"));
        assert!(has_pair(&args("mf", false), "-c:v", "h264_mf"));
    }

    /// A hardware arm CAN produce HEVC, unlike the software one, so the QuickTime tag comes
    /// back with it. Without this an HEVC mp4 does not play in Apple players.
    #[test]
    fn an_hevc_hardware_arm_tags_hvc1_and_an_h264_one_does_not() {
        for id in ["nvenc", "videotoolbox", "amf", "qsv", "mf"] {
            assert!(args(id, true).iter().any(|a| a == "hvc1"), "{id} hevc must tag");
            assert!(!args(id, false).iter().any(|a| a == "hvc1"), "{id} h264 must not tag");
        }
    }

    /// NVENC is the one tier whose capture plan is `quality_rc: true`, so it gets
    /// constant-quality VBR with the bitrate as a peak cap, and never a `-b:v` target.
    #[test]
    fn nvenc_uses_constant_quality_and_the_bitrate_only_caps_it() {
        let v = args("nvenc", false);
        assert!(has_pair(&v, "-rc", "vbr"));
        assert!(has_pair(&v, "-cq", "19"));
        assert!(has_pair(&v, "-b:v", "0"), "quality RC means no bitrate TARGET");
        assert!(has_pair(&v, "-maxrate", "8000k"));
        assert!(has_pair(&v, "-bufsize", "16000k"));
    }

    /// Every other tier's plan is `quality_rc: false`, so it re-encodes to the same bitrate
    /// envelope the recording used rather than to some ffmpeg default.
    #[test]
    fn the_bitrate_tiers_target_the_recordings_own_bitrate() {
        for id in ["videotoolbox", "amf", "qsv", "mf"] {
            let v = args(id, false);
            assert!(has_pair(&v, "-b:v", "8000k"), "{id}");
            assert!(has_pair(&v, "-maxrate", "8000k"), "{id}");
            assert!(has_pair(&v, "-bufsize", "16000k"), "{id}");
        }
    }

    /// Software, VAAPI and anything a future ladder adds fall through to `None`, which the
    /// caller turns into the historical software pass. VAAPI is not an oversight: it needs a
    /// device, an env var and a `hwupload` in the graph, i.e. a different graph SHAPE, and it
    /// cannot be reached on the two platforms that bake anyway.
    #[test]
    fn tiers_without_a_safe_arm_fall_through_to_software() {
        assert_eq!(ink_hw_video_args("software", false, 8000), None);
        assert_eq!(ink_hw_video_args("vaapi", false, 8000), None);
        assert_eq!(ink_hw_video_args("something-new", false, 8000), None);
    }

    /// The capture plans' REALTIME tunings must not ride along. They exist so a live encoder
    /// never falls behind the screen, and a finalize has no such deadline.
    #[test]
    fn the_realtime_capture_tunings_are_left_behind() {
        for id in ["nvenc", "videotoolbox", "amf", "qsv", "mf"] {
            let v = args(id, false);
            for banned in ["-realtime", "-tune", "zerolatency", "speed"] {
                assert!(!v.iter().any(|a| a == banned), "{id} must not carry {banned}");
            }
        }
    }

    /// A nonsense bitrate cannot produce a nonsense argument: the floor matches the one
    /// `encode::command` applies to the recording itself.
    #[test]
    fn the_bitrate_has_the_same_floor_the_recording_uses() {
        let v = ink_hw_video_args("videotoolbox", false, 0).expect("an arm");
        assert!(has_pair(&v, "-b:v", "100k"));
        assert!(has_pair(&v, "-bufsize", "200k"));
    }

    /// Both arms always name a pixel format, so neither can inherit one from the overlay's
    /// RGBA input and produce a file ordinary players refuse.
    #[test]
    fn every_arm_pins_the_output_pixel_format() {
        for id in ["nvenc", "videotoolbox", "amf", "qsv", "mf"] {
            let v = args(id, false);
            let pf = v.windows(2).find(|w| w[0] == "-pix_fmt").expect("an arm names a format");
            assert!(!pf[1].is_empty(), "{id}");
        }
        assert!(has_pair(&ink_sw_video_args(), "-pix_fmt", "yuv420p"));
    }

    /// Every arm but Media Foundation asks for YUV420P, and that has to stay a per-arm
    /// answer rather than a shared trailing line (DRAGON-755). It was shared, `mf` inherited
    /// it, and a hardware MFT refused the whole invocation with `format negotiation failed`.
    #[test]
    fn only_the_media_foundation_arm_departs_from_yuv420p() {
        for id in ["nvenc", "videotoolbox", "amf", "qsv"] {
            assert!(has_pair(&args(id, false), "-pix_fmt", "yuv420p"), "{id}");
            assert!(has_pair(&args(id, true), "-pix_fmt", "yuv420p"), "{id} hevc");
        }
        assert!(has_pair(&args("mf", false), "-pix_fmt", "nv12"));
        assert!(has_pair(&args("mf", true), "-pix_fmt", "nv12"));
        assert!(!args("mf", false).iter().any(|a| a == "yuv420p"));
    }

    /// **DRAGON-755**: the Media Foundation arm forces the HARDWARE MFT, from the one
    /// constant the plan and the probe already share.
    ///
    /// This is not a tuning and it is not optional. Without it ffmpeg enumerates software
    /// MFTs only, so `hevc_mf` fails at its first frame and `h264_mf` quietly re-encodes on
    /// the CPU, which is the tier reuse claiming a hardware pass it is not making. No other
    /// arm carries it, because no other tier is a wrapper that has to be told which encoder
    /// it is wrapping.
    #[test]
    fn the_media_foundation_arm_forces_the_hardware_mft() {
        for hevc in [false, true] {
            let v = args("mf", hevc);
            assert!(has_pair(&v, "-hw_encoding", "true"), "hevc={hevc}");
        }
        assert_eq!(crate::encode::MF_HW_ENCODING, ["-hw_encoding", "true"]);
        for id in ["nvenc", "videotoolbox", "amf", "qsv"] {
            assert!(!args(id, false).iter().any(|a| a == "-hw_encoding"), "{id}");
        }
    }

    /// The `mf` PLAN's live-capture options stay out of the finalize arm, exactly as the
    /// other tiers' realtime tunings do (DRAGON-755). Each is an option an untested vendor
    /// MFT could refuse, and the arm gains nothing by carrying it.
    #[test]
    fn the_media_foundation_plans_capture_options_stay_behind() {
        let v = args("mf", false);
        for banned in ["-rate_control", "pc_vbr", "-scenario", "display_remoting", "-g"] {
            assert!(!v.iter().any(|a| a == banned), "the mf arm must not carry {banned}");
        }
    }
}

/// One channel's finalize filter chain: gate it to its on-intervals. A no-op chain
/// is `anull` (a valid single filter for the `[0:a:N]…[x]` graph link).
fn chan_filter(off: &[(f64, f64)]) -> String {
    match vol_filter(off) {
        Some(f) => f,
        None => "anull".to_string(),
    }
}

/// The mute-gate stage for a channel given its off-intervals, or `None` (passthrough)
/// when it's always on. Full mute when always off, else a timeline `enable` that mutes
/// the gaps.
fn vol_filter(off: &[(f64, f64)]) -> Option<String> {
    if off.is_empty() {
        None
    } else if off.len() == 1 && off[0].0 <= 0.0 && off[0].1 >= 1.0e9 {
        Some("volume=0".to_string())
    } else {
        let expr = off
            .iter()
            .map(|(s, e)| format!("between(t,{s:.3},{e:.3})"))
            .collect::<Vec<_>>()
            .join("+");
        Some(format!("volume=0:enable='{expr}'"))
    }
}

/// Annotate a metadata line's `codec=auto` with the codec the encode plan
/// actually resolved to (decided only once the first frame fixed the dims) —
/// `codec=auto(H.264)` / `codec=auto(HEVC)`. Explicit codec choices pass
/// through untouched: they already say what they are.
fn annotate_codec(metadata: &str, is_hevc: bool) -> String {
    metadata.replace(
        "codec=auto",
        if is_hevc { "codec=auto(HEVC)" } else { "codec=auto(H.264)" },
    )
}

/// Per-channel "off" intervals from toggle points already mapped to seconds on the
/// audio stream's timeline (a channel is muted during these; elsewhere it passes
/// at full volume; `on_at_start` is the channel's state at t0; a trailing off
/// interval runs open-ended to EOF, encoded as `1e9`). The interval-building core:
/// `record::pump` (DRAGON-125) reuses this exact function to turn its accumulated
/// `TrackGain` automation (already in MEDIA seconds — no wall-clock mapping
/// needed) into the same interval shape, rather than re-deriving the same
/// on/off/trailing-open logic a second time.
pub(crate) fn off_intervals_from_pts(mut pts: Vec<(f64, bool)>, on_at_start: bool) -> Vec<(f64, f64)> {
    pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut intervals = Vec::new();
    let mut on = on_at_start;
    let mut off_start = if on_at_start { None } else { Some(0.0) };
    for (t, new_on) in pts {
        if new_on && !on {
            if let Some(s) = off_start.take() {
                intervals.push((s, t));
            }
        } else if !new_on && on {
            off_start = Some(t);
        }
        on = new_on;
    }
    if let Some(s) = off_start {
        intervals.push((s, 1.0e9)); // open-ended to EOF
    }
    intervals
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn annotate_codec_resolves_auto_and_leaves_explicit_choices() {
        let meta = "Cosmic Capture Kit | encoder=nvenc | codec=auto | fps=30";
        assert_eq!(
            annotate_codec(meta, false),
            "Cosmic Capture Kit | encoder=nvenc | codec=auto(H.264) | fps=30"
        );
        assert_eq!(
            annotate_codec(meta, true),
            "Cosmic Capture Kit | encoder=nvenc | codec=auto(HEVC) | fps=30"
        );
        // An explicit codec passes through untouched.
        let explicit = "Cosmic Capture Kit | codec=hevc | fps=30";
        assert_eq!(annotate_codec(explicit, true), explicit);
    }

    // ── off_intervals_from_pts: the interval-building core (media seconds) ────
    #[test]
    fn starts_on_with_no_events_is_never_muted() {
        assert_eq!(off_intervals_from_pts(vec![], true), Vec::<(f64, f64)>::new());
    }

    #[test]
    fn starts_off_with_no_events_is_muted_to_eof() {
        assert_eq!(off_intervals_from_pts(vec![], false), vec![(0.0, 1.0e9)]);
    }

    #[test]
    fn off_then_on_yields_one_bounded_interval() {
        let pts = vec![(1.0, false), (3.0, true)];
        assert_eq!(off_intervals_from_pts(pts, true), vec![(1.0, 3.0)]);
    }

    #[test]
    fn trailing_off_is_open_ended_to_eof() {
        let pts = vec![(2.0, false)];
        assert_eq!(off_intervals_from_pts(pts, true), vec![(2.0, 1.0e9)]);
    }

    #[test]
    fn starts_off_on_off_on_yields_two_intervals() {
        let pts = vec![(1.0, true), (2.0, false), (3.0, true)];
        assert_eq!(off_intervals_from_pts(pts, false), vec![(0.0, 1.0), (2.0, 3.0)]);
    }

    #[test]
    fn redundant_off_while_already_off_is_ignored() {
        // Already off at start; an "off" toggle changes nothing.
        let pts = vec![(1.0, false)];
        assert_eq!(off_intervals_from_pts(pts, false), vec![(0.0, 1.0e9)]);
    }

    #[test]
    fn out_of_order_events_are_sorted_by_time() {
        // Same toggles as `off_then_on` but supplied out of order.
        let pts = vec![(3.0, true), (1.0, false)];
        assert_eq!(off_intervals_from_pts(pts, true), vec![(1.0, 3.0)]);
    }

    // ── chan_filter: gate-only filtergraph shapes (no delay-shift any more) ───
    #[rstest]
    #[case(&[], "anull")]
    #[case(&[(0.0, 1.0e9)], "volume=0")]
    #[case(&[(1.0, 3.0)], "volume=0:enable='between(t,1.000,3.000)'")]
    fn chan_filter_shapes(#[case] off: &[(f64, f64)], #[case] want: &str) {
        assert_eq!(chan_filter(off), want);
    }
}
