//! Building + spawning the capture ffmpeg invocations: the media-clock raw-frame
//! recorder ([`spawn_ffmpeg_media_clock`]), the media-clock zero-copy muxer for
//! pre-encoded video ([`spawn_ffmpeg_encoded_media_clock`]), and the throughput
//! benchmark ([`bench_encoder`]). DRAGON-127 retired the legacy wallclock+CFR
//! recorder (`spawn_ffmpeg`) and zero-copy muxer (`spawn_ffmpeg_encoded`) these
//! used to sit alongside — the media-clock pipeline is the only recording path now.

use std::io::Write;
use std::process::{Child, Command, Stdio};

use super::*;

/// Append the video filter/upload selection plus the encoder (`-c:v …`) args shared by
/// the raw-frame ffmpeg builders, returning the video map label to wire into `-map`.
/// VAAPI uploads to a GPU surface via a `filter_complex` whose labelled output we map
/// (mapping the raw input would bypass the upload); other plans map the input video.
/// `prescale` is an optional filter (e.g. `scale=…`) that runs FIRST — before any
/// plan filter/upload — so an oversized capture downscales inside ffmpeg's threaded,
/// SIMD swscale instead of on the capture path (where a per-frame CPU resize was
/// measured collapsing a 5120-wide recording to ~1 real frame per second).
fn apply_video_args(cmd: &mut Command, plan: &EncodePlan, prescale: Option<&str>) -> &'static str {
    let vmap = match (&plan.vf, prescale) {
        (Some(vf), Some(pre)) => {
            cmd.args(["-filter_complex", &format!("[0:v]{pre},{vf}[v]")]);
            "[v]"
        }
        (Some(vf), None) => {
            cmd.args(["-filter_complex", &format!("[0:v]{vf}[v]")]);
            "[v]"
        }
        (None, Some(pre)) => {
            cmd.args(["-filter_complex", &format!("[0:v]{pre}[v]")]);
            "[v]"
        }
        (None, None) => "0:v:0",
    };
    for a in &plan.codec {
        cmd.arg(a);
    }
    vmap
}

/// One encoder's benchmark outcome: achieved throughput plus what it COST.
/// `fps` alone misranks hardware encoders — a paced fixed-function pipeline
/// (VideoToolbox through ffmpeg's vtenc wrapper) tops out on per-frame overhead
/// while barely touching the CPU, whereas x264 races ahead by burning every
/// core the recorded app would otherwise have. `cores` is average CPU cores
/// consumed over the run (ffmpeg's own `-benchmark` utime+stime over rtime).
#[derive(Clone, Copy, Default, PartialEq)]
pub struct BenchScore {
    pub fps: f32,
    pub cores: f32,
}

/// Benchmark an encoder `backend` at `w`×`h` and `bitrate_kbps`: encode black
/// frames as fast as the pipe accepts for ~`secs`, returning the achieved fps
/// (encode throughput) and the CPU cost. All-zero if the backend is unavailable
/// or errors immediately.
///
/// Pass `capture_cost = false` for encoder-only throughput; [`bench_encoder_pipeline`]'s
/// `capture_cost = true` adds the real capture-thread per-frame cost so the number reflects
/// a whole recording (DRAGON-168).
///
/// Benchmark an encoder `backend` at the ENCODE dims `w`×`h`, optionally including the
/// real per-frame CAPTURE cost (`capture_cost`) so the achieved fps reflects the WHOLE
/// macOS recording pipeline, not the encoder alone (DRAGON-168). On macOS a real
/// recording pays, per frame at these same dims (the SCK stream now delivers AT the
/// encode size — GPU-scaled server-side — so the pipe/transport frame size the bench uses
/// already matches reality): a BGRA→RGBA swizzle over the whole frame on the capture
/// thread, then the stdin write the encoder-only bench already measures. Modeling the
/// swizzle here is what makes the bench's verdict predict a real recording of the selected
/// monitor: a config that can't sustain the configured fps once the swizzle is counted
/// shows a lower number, instead of an encoder-only figure a real recording never reaches.
pub fn bench_encoder_pipeline(
    backend: &str,
    w: u32,
    h: u32,
    bitrate_kbps: u32,
    presets: &Presets,
    secs: f64,
    capture_cost: bool,
) -> BenchScore {
    let (w, h) = fit_within_encoder_max(w, h);
    let Some(plan) = EncodePlan::for_backend(backend, w, h, presets) else {
        return BenchScore::default();
    };
    let mut cmd = crate::util::ffmpeg_command();
    // `-benchmark` prints `bench: utime=…s stime=…s rtime=…s` at info level on
    // exit — the CPU-cost source. `-nostats` keeps the per-frame progress line
    // out of stderr so the drain stays tiny.
    cmd.args(["-hide_banner", "-loglevel", "info", "-nostats", "-benchmark", "-y"]);
    for (k, v) in &plan.env {
        cmd.env(k, v);
    }
    for a in &plan.pre {
        cmd.arg(a);
    }
    let in_fmt = if plan.nv12 { "nv12" } else { "rgba" };
    cmd.args(["-f", "rawvideo", "-pix_fmt", in_fmt])
        .arg("-s")
        .arg(format!("{w}x{h}"))
        .args(["-i", "-"]);
    let vmap = apply_video_args(&mut cmd, &plan, None);
    // Match the real recording's rate control so the throughput is representative.
    let kbps = bitrate_kbps.max(100);
    if plan.quality_rc {
        cmd.args(["-maxrate", &format!("{kbps}k"), "-bufsize", &format!("{}k", kbps * 2)]);
    } else {
        cmd.args([
            "-b:v", &format!("{kbps}k"),
            "-maxrate", &format!("{kbps}k"),
            "-bufsize", &format!("{}k", kbps * 2),
        ]);
    }
    cmd.args(["-map", vmap, "-f", "null", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let Ok(mut child) = cmd.spawn() else {
        return BenchScore::default();
    };
    let Some(mut stdin) = child.stdin.take() else {
        return BenchScore::default();
    };
    // Drain stderr on its own thread so a chatty encoder can never fill the pipe
    // and deadlock against our stdin writes; the captured text carries the
    // `bench:` line parsed below.
    let stderr = child.stderr.take();
    let drain = std::thread::spawn(move || {
        use std::io::Read;
        let mut s = String::new();
        if let Some(mut e) = stderr {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let out_bytes =
        if plan.nv12 { (w * h * 3 / 2) as usize } else { (w * h * 4) as usize };
    let mut frame = vec![0u8; out_bytes];
    // The synthetic capture source the swizzle reads FROM (tightly-packed BGRA at the
    // encode/delivery dims — the same footprint SCK now delivers, DRAGON-168). Only
    // allocated/used when modeling the capture cost.
    let bgra_src: Vec<u8> = if capture_cost { vec![0u8; (w * h * 4) as usize] } else { Vec::new() };
    let start = std::time::Instant::now();
    let mut n = 0u64;
    while start.elapsed().as_secs_f64() < secs {
        if capture_cost {
            // The real capture-thread per-frame work: BGRA→RGBA swizzle over the whole
            // frame (mirrors `record::sck::copy_screen_frame_rgba`'s inner pixel loop). For
            // the NV12 (hardware) path the frame writer additionally converts RGBA→NV12, so
            // do both, ending with the bytes the encoder actually reads.
            let px = (w * h) as usize;
            let mut rgba = vec![0u8; px * 4];
            for i in 0..px {
                let s = i * 4;
                rgba[s] = bgra_src[s + 2];
                rgba[s + 1] = bgra_src[s + 1];
                rgba[s + 2] = bgra_src[s];
                rgba[s + 3] = bgra_src[s + 3];
            }
            if plan.nv12 {
                crate::encode::rgba_to_nv12(&rgba, w as usize, h as usize, &mut frame);
            } else {
                frame.copy_from_slice(&rgba);
            }
        }
        if stdin.write_all(&frame).is_err() {
            break;
        }
        n += 1;
    }
    drop(stdin);
    let _ = child.wait();
    let dt = start.elapsed().as_secs_f64();
    let stderr_text = drain.join().unwrap_or_default();
    let fps = if n == 0 || dt <= 0.0 {
        0.0
    } else {
        (n as f64 / dt) as f32
    };
    BenchScore { fps, cores: bench_cores(&stderr_text).unwrap_or(0.0) }
}

/// Average CPU cores from ffmpeg's `-benchmark` stderr line
/// (`bench: utime=30.257s stime=9.290s rtime=9.918s`): (utime+stime)/rtime.
/// `None` when the line is absent/garbled (the caller shows the fps alone).
fn bench_cores(stderr: &str) -> Option<f32> {
    let line = stderr.lines().rev().find(|l| l.contains("utime="))?;
    let field = |key: &str| -> Option<f64> {
        line.split_once(key)?.1.split('s').next()?.parse().ok()
    };
    let (utime, stime, rtime) = (field("utime=")?, field("stime=")?, field("rtime=")?);
    (rtime > 0.0).then(|| ((utime + stime) / rtime) as f32)
}

/// The ffmpeg args for one FIFO-fed audio input in the media-clock pipeline
/// (DRAGON-125): a `-thread_queue_size`/`-f f32le`/`-ar 48000` demuxer input,
/// `-ac channels` instead of a fixed channel count (mono mic / stereo system),
/// reading a FIFO the pump owns and feeds instead of a live pulse device. No
/// `-itsoffset` here — the A/V-sync offset is applied once at finalize
/// (DRAGON-119), never on a live/FIFO-fed input (ffmpeg 8's threaded scheduler
/// deterministically stalls on a live `-itsoffset` ≥ ~200ms).
fn fifo_input_args(path: &std::path::Path, channels: u8) -> Vec<String> {
    vec![
        "-thread_queue_size".into(), "1024".into(),
        "-f".into(), "f32le".into(),
        "-ar".into(), "48000".into(),
        "-ac".into(), channels.to_string(),
        "-i".into(), path.to_string_lossy().into_owned(),
    ]
}

/// Build the media-clock capture ffmpeg [`Command`] (unspawned) — the pure half of
/// [`spawn_ffmpeg_media_clock`], split out so its argv shape is unit-testable without
/// spawning a real process (`Command::get_args()` is a stable, side-effect-free
/// introspection call).
#[allow(clippy::too_many_arguments)]
fn build_media_clock_command(
    w: u32,
    h: u32,
    ew: u32,
    eh: u32,
    fps: u32,
    plan: &EncodePlan,
    bitrate_kbps: u32,
    out_path: &std::path::Path,
    mic_fifo: &std::path::Path,
    sys_fifo: &std::path::Path,
) -> Command {
    let mut cmd = crate::util::ffmpeg_command();
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    for (k, v) in &plan.env {
        cmd.env(k, v);
    }
    for a in &plan.pre {
        cmd.arg(a);
    }
    // The rawvideo input pixel format: NV12 on the hardware-feed path, otherwise the
    // plan's RGBA-feed byte order (`"rgba"` for every Linux/Windows worker and the mac
    // default; the macOS SCK worker sets `"bgra"` so it feeds ScreenCaptureKit's native
    // BGRA buffers WITHOUT a per-pixel swizzle — DRAGON-316). The default keeps every
    // historical Linux/Windows command line byte-identical.
    let in_fmt = if plan.nv12 { "nv12" } else { plan.input_pix_fmt };
    // Index-stamped video input (DRAGON-125): frame k's PTS = k/fps, assigned by
    // `-r fps` on the input instead of wallclock arrival — the pump's
    // `due_video_ticks` already paces frame delivery to exactly one slot per 1/fps
    // of MEDIA time, so the input is CFR by construction and needs no wallclock
    // timestamping (contrast `spawn_ffmpeg`'s `-use_wallclock_as_timestamps 1`).
    cmd.args(["-thread_queue_size", "1024", "-f", "rawvideo", "-pix_fmt", in_fmt])
        .arg("-s")
        .arg(format!("{w}x{h}"))
        .args(["-r", &fps.to_string()]);
    if plan.nv12 {
        cmd.args([
            "-colorspace", "bt709",
            "-color_primaries", "bt709",
            "-color_trc", "bt709",
            "-color_range", "tv",
        ]);
    }
    cmd.args(["-i", "-"]);
    // Both audio inputs are plain FIFOs the pump already owns/feeds (mixer-rendered
    // PCM) — no pulse inputs, no clean-mic/relay setup here (the caller owns that).
    // Input index order matches spawn_ffmpeg exactly: 0 video, 1 mic (mono), 2
    // system (stereo).
    cmd.args(fifo_input_args(mic_fifo, 1));
    cmd.args(fifo_input_args(sys_fifo, 2));
    let prescale = ((w, h) != (ew, eh)).then(|| format!("scale={ew}:{eh}:flags=bilinear"));
    let vmap = apply_video_args(&mut cmd, plan, prescale.as_deref());
    if plan.color_tags {
        cmd.args([
            "-colorspace", "bt709",
            "-color_primaries", "bt709",
            "-color_trc", "bt709",
            "-color_range", "tv",
        ]);
    }
    let kbps = bitrate_kbps.max(100);
    if plan.quality_rc {
        cmd.args(["-maxrate", &format!("{kbps}k"), "-bufsize", &format!("{}k", kbps * 2)]);
    } else {
        cmd.args([
            "-b:v", &format!("{kbps}k"),
            "-maxrate", &format!("{kbps}k"),
            "-bufsize", &format!("{}k", kbps * 2),
        ]);
    }
    cmd.args(["-map", vmap, "-map", "1:a:0", "-map", "2:a:0"]);
    cmd.args([
        "-c:a", "aac", "-b:a", "192k",
        "-metadata:s:a:0", "title=mic", "-metadata:s:a:1", "title=system",
        "-shortest",
    ]);
    // NO -fps_mode cfr / -r pair here (contrast spawn_ffmpeg): the input is already
    // CFR by construction (see above), so ffmpeg's default passthrough vsync is
    // correct: adding a replacement flag would fight a stream that's already
    // exactly what cfr would produce, for no benefit.
    cmd.args(["-flush_packets", "1"]);
    // stderr is deliberately NOT set here (DRAGON-432): `spawn_recording_muxer` pipes it and
    // attaches the drain, so the raw-frame and zero-copy muxers cannot drift apart.
    cmd.arg(out_path).stdin(Stdio::piped()).stdout(Stdio::null());
    cmd
}

/// Spawn the capture ffmpeg for the media-clock OWNED recording pipeline
/// (DRAGON-125, chunk B1 — the ONLY recording path since DRAGON-127): the raw-video
/// input is INDEX-stamped (frame k's PTS = k/fps) instead of wallclock-stamped, the
/// output carries no `-fps_mode cfr -r {fps}` pair, and both audio inputs are plain
/// FIFOs the caller (`record::pump`) already owns and feeds — no pulse inputs, no
/// clean-mic tap/monitor-capture setup happens here (the caller owns that; see
/// `crate::audio::clean_mic::setup_clean_mic_tap` /
/// `crate::audio::capture::MonitorCapture::start`). `mic_fifo` carries mono PCM,
/// `sys_fifo` stereo.
#[allow(clippy::too_many_arguments)]
pub fn spawn_ffmpeg_media_clock(
    w: u32,
    h: u32,
    ew: u32,
    eh: u32,
    fps: u32,
    plan: &EncodePlan,
    bitrate_kbps: u32,
    out_path: &std::path::Path,
    mic_fifo: &std::path::Path,
    sys_fifo: &std::path::Path,
) -> Result<Child, String> {
    spawn_recording_muxer(
        &mut build_media_clock_command(
            w, h, ew, eh, fps, plan, bitrate_kbps, out_path, mic_fifo, sys_fifo,
        ),
        out_path,
        mic_fifo,
        sys_fifo,
    )
}

/// Spawn a built recording-muxer command, TETHERED to this process and recorded in the
/// crash-recovery ledger (DRAGON-421).
///
/// Both recording muxer spawns (raw-frame and zero-copy) go through here so neither the
/// tether nor the ledger depends on a worker remembering to do it — there are four workers
/// and every one of them must get this right. See [`crate::util::spawn_tethered`] for the
/// per-platform guarantee and [`crate::record::recover`] for what the ledger buys.
fn spawn_recording_muxer(
    cmd: &mut Command,
    out_path: &std::path::Path,
    mic_fifo: &std::path::Path,
    sys_fifo: &std::path::Path,
) -> Result<Child, String> {
    // DRAGON-432: capture the muxer's stderr. Set HERE rather than in the builders because
    // this is the one place BOTH recording muxers pass through (the raw-frame one and the
    // feature-gated zero-copy one), so the drain cannot be wired up for one and forgotten
    // for the other. It was `Stdio::inherit()`, which on a Windows GUI-subsystem launch and
    // on a Linux hotkey launch inherits nothing at all — ffmpeg's explanation of why a
    // recording failed was written straight into a black hole.
    cmd.stderr(Stdio::piped());
    let mut child = crate::util::spawn_tethered(cmd)
        .map_err(|e| format!("could not start ffmpeg (is it installed?): {e}"))?;
    // MUST happen before anything writes to this child's stdin. A piped stderr that nobody
    // drains fills its pipe and blocks ffmpeg's writes, deadlocking against our frame feed —
    // see `ffmpeg_log::attach`, which owns that contract.
    crate::record::ffmpeg_log::attach(&mut child);
    crate::record::recover::note_muxer(child.id(), out_path, mic_fifo, sys_fifo);
    Ok(child)
}

/// Build the media-clock zero-copy muxer [`Command`] (unspawned) — the pure half of
/// [`spawn_ffmpeg_encoded_media_clock`], mirroring how [`build_media_clock_command`]
/// splits out of [`spawn_ffmpeg_media_clock`] for the same reason (argv shape
/// unit-testable without spawning a process).
/// How far apart the zero-copy muxer's streams may drift before libavformat flushes,
/// in microseconds. See the long note at its use site in
/// [`build_media_clock_encoded_command`] for why this is a band with two live edges.
#[cfg(feature = "zero-copy")]
pub(crate) const INTERLEAVE_DELTA_US: u64 = 1_000_000;

// Both edges of that band, pinned at COMPILE time (the same treatment
// `RENDER_LAG_SECS` gets against `REANCHOR_THRESHOLD_SECS`), because each one cost a
// recording in a different way and neither is visible to any other test here:
// at libavformat's 10s default the watchdog killed the muxer before it wrote a packet
// (no file at all), and at 1us the streams decoupled far enough that the stop tail
// ended the file with the audio still queued (a playable, correctly sized, MUTE file).
#[cfg(feature = "zero-copy")]
const _: () = assert!(
    INTERLEAVE_DELTA_US * 10 <= crate::record::MUXER_LIVENESS_SECS * 1_000_000,
    "the interleave window must stay an order of magnitude inside the muxer-liveness \
     budget, or ffmpeg is killed as wedged before it writes its first packet"
);
#[cfg(feature = "zero-copy")]
const _: () = assert!(
    INTERLEAVE_DELTA_US >= 100_000,
    "too small decouples the streams and the stop tail strands a whole audio track"
);

#[cfg(feature = "zero-copy")]
fn build_media_clock_encoded_command(
    hevc: bool,
    fps: u32,
    out_path: &std::path::Path,
    mic_fifo: &std::path::Path,
    sys_fifo: &std::path::Path,
) -> Command {
    let mut cmd = crate::util::ffmpeg_command();
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    let demux = if hevc { "hevc" } else { "h264" };
    cmd.args(["-fflags", "+genpts"])
        .args(["-thread_queue_size", "1024", "-r", &fps.to_string()])
        .args(["-f", demux, "-i", "-"]);
    // Both audio inputs are plain FIFOs the pump already owns/feeds (mixer-rendered
    // PCM) — no pulse inputs, no clean-mic/relay setup here (the caller owns that),
    // mirroring what `build_media_clock_command` did to the raw-frame path's audio
    // inputs. Input index order matches `spawn_ffmpeg_encoded` exactly: 0 video
    // (pre-encoded elementary stream), 1 mic (mono), 2 system (stereo).
    cmd.args(fifo_input_args(mic_fifo, 1));
    cmd.args(fifo_input_args(sys_fifo, 2));
    cmd.args(["-map", "0:v:0", "-map", "1:a:0", "-map", "2:a:0"]);
    cmd.args(["-c:v", "copy"]);
    if hevc {
        cmd.args(["-tag:v", "hvc1"]);
    }
    cmd.args([
        "-c:a", "aac", "-b:a", "192k",
        "-metadata:s:a:0", "title=mic", "-metadata:s:a:1", "title=system",
    ]);
    // DRAGON-457: NO `-shortest` here, and its absence is the fix — on this path it
    // silently threw away EVERY audio packet.
    //
    // ffmpeg 8 implements `-shortest` with an output SYNC QUEUE. The raw-frame sibling
    // encodes all three streams, so its queue behaves; here video is `-c:v copy` from a
    // pre-encoded elementary stream while both audio tracks go through the AAC encoders,
    // and the queue discards the audio outright. ffmpeg's own accounting, muxing the same
    // 11.9s of h264 plus both audio FIFOs on the owner's box:
    //
    //   with `-shortest`:     video 358 packets muxed
    //                         audio  8 frames ENCODED, 0 PACKETS MUXED  (both tracks)
    //   without `-shortest`:  video 358 packets muxed
    //                         audio 563 frames encoded, 564 packets muxed (576000 samples)
    //
    // `-shortest_buf_duration` does not rescue it; the packets are dropped, not buffered.
    // The failure is invisible downstream: finalize's `amix` faithfully produced nothing
    // from nothing, so the recording arrived playable, correctly sized, and MUTE.
    //
    // Losing it costs us nothing here. `-shortest` existed to trim the video back to the
    // audio's end, and this pipeline already guarantees the relationship the other way
    // round: we own the clock, and the stop tail deliberately writes covering video ticks
    // until video ends >= the audio's media end (see `record::zero_copy`'s stop tail and
    // CLAUDE.md's stop-tail rule). What remains is at most a few frames of video past the
    // last audio sample, which is the side the invariant was always willing to pay.
    cmd.args(["-flush_packets", "1"]);
    // DRAGON-457: WITHOUT this, nothing reaches the file until an input ends.
    //
    // libavformat holds the interleave queue until it has a packet from every stream, or
    // until the queue spans `max_interleave_delta` — which defaults to TEN SECONDS. The
    // raw-frame sibling never notices, because there ffmpeg encodes the video itself and
    // so produces video and audio packets in lockstep. Here input 0 is a pre-encoded
    // elementary stream with no container timestamps, parsed by the raw `h264`/`hevc`
    // demuxer, and the queue condition is not met promptly enough — so ffmpeg sat on the
    // WHOLE recording, wrote its 928-byte header and nothing else, and `MuxerWatchdog`
    // (`MUXER_LIVENESS_SECS`, 12s, and rightly) killed it as wedged. That is why
    // zero-copy had never once produced a file on the owner's box.
    //
    // The value is a BAND, not a minimum, and both edges are real:
    //
    // * Too large (the 10s default) and the watchdog kills the muxer before its first
    //   packet — no recording at all.
    // * Too small and the streams decouple: at 1µs, ffmpeg wrote video far ahead of
    //   audio, and when the stop tail closed video stdin, `-shortest` ended the file
    //   while the audio was still queued. That produced a clean, playable, 19s
    //   recording WITH NO AUDIO TRACK — the worst kind of failure, since it looks
    //   delivered. Measured on the owner's box.
    //
    // One second sits an order of magnitude inside the watchdog budget while keeping the
    // streams close enough that the stop tail cannot strand a whole track.
    // `interleave_window_sits_inside_the_muxer_liveness_budget` pins that relationship.
    cmd.args(["-max_interleave_delta", &INTERLEAVE_DELTA_US.to_string()]);
    // stderr is deliberately NOT set here (DRAGON-432): `spawn_recording_muxer` pipes it and
    // attaches the drain, so the raw-frame and zero-copy muxers cannot drift apart.
    cmd.arg(out_path).stdin(Stdio::piped()).stdout(Stdio::null());
    cmd
}

/// Spawn the muxer ffmpeg for the media-clock OWNED zero-copy pipeline (DRAGON-127):
/// video is ALREADY encoded (H.264/HEVC Annex-B written to stdin by the in-process
/// GPU encoder), so ffmpeg only stream-copies it (`-c:v copy`, the `hvc1` tag for
/// HEVC) and muxes the two audio tracks, which are plain FIFOs the caller
/// (`record::pump`) already owns and feeds — no pulse inputs, no clean-mic
/// tap/monitor-capture setup happens here. `mic_fifo` carries mono PCM, `sys_fifo`
/// stereo, matching [`spawn_ffmpeg_media_clock`]'s own shape for the raw-frame path.
#[cfg(feature = "zero-copy")]
pub fn spawn_ffmpeg_encoded_media_clock(
    hevc: bool,
    fps: u32,
    out_path: &std::path::Path,
    mic_fifo: &std::path::Path,
    sys_fifo: &std::path::Path,
) -> Result<Child, String> {
    spawn_recording_muxer(
        &mut build_media_clock_encoded_command(hevc, fps, out_path, mic_fifo, sys_fifo),
        out_path,
        mic_fifo,
        sys_fifo,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_input_args_carry_no_itsoffset_and_the_right_channel_count() {
        let mic = fifo_input_args(std::path::Path::new("/tmp/mic.pcm"), 1);
        let sys = fifo_input_args(std::path::Path::new("/tmp/sys.pcm"), 2);
        assert!(!mic.iter().any(|a| a == "-itsoffset"));
        assert!(!sys.iter().any(|a| a == "-itsoffset"));
        assert_eq!(
            mic,
            ["-thread_queue_size", "1024", "-f", "f32le", "-ar", "48000", "-ac", "1", "-i", "/tmp/mic.pcm"]
        );
        assert_eq!(
            sys,
            ["-thread_queue_size", "1024", "-f", "f32le", "-ar", "48000", "-ac", "2", "-i", "/tmp/sys.pcm"]
        );
    }

    /// A minimal software-encode plan, standing in for whatever `EncodePlan::resolve`
    /// would pick — the media-clock command's shape doesn't depend on which plan.
    fn test_plan() -> EncodePlan {
        EncodePlan {
            pre: Vec::new(),
            codec: vec!["-c:v".into(), "libx264".into()],
            env: Vec::new(),
            vf: None,
            color_tags: false,
            nv12: false,
            input_pix_fmt: "rgba",
            quality_rc: true,
        }
    }

    fn media_clock_args(plan: &EncodePlan) -> Vec<String> {
        build_media_clock_command(
            1920, 1080, 1920, 1080, 30, plan, 8000,
            std::path::Path::new("/tmp/out.mkv"),
            std::path::Path::new("/tmp/mic.pcm"),
            std::path::Path::new("/tmp/sys.pcm"),
        )
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
    }

    #[test]
    fn media_clock_command_video_input_is_index_stamped_not_wallclock() {
        let args = media_clock_args(&test_plan());
        assert!(
            !args.iter().any(|a| a == "-use_wallclock_as_timestamps"),
            "the media-clock video input must not be wallclock-stamped: {args:?}"
        );
        // Exactly one -r pair, and it sits BEFORE the rawvideo `-i -` (i.e. it's an
        // INPUT option, not an output one).
        let r_positions: Vec<usize> =
            args.iter().enumerate().filter(|(_, a)| a.as_str() == "-r").map(|(i, _)| i).collect();
        assert_eq!(r_positions, vec![args.iter().position(|a| a == "-r").unwrap()], "{args:?}");
        assert_eq!(args[r_positions[0] + 1], "30");
        let first_dash_i = args.iter().position(|a| a == "-i").unwrap();
        assert!(r_positions[0] < first_dash_i, "-r must precede the video -i: {args:?}");
        assert_eq!(args[first_dash_i + 1], "-", "the video input stays stdin");
    }

    #[test]
    fn media_clock_command_output_has_no_cfr_pair_and_no_itsoffset() {
        let args = media_clock_args(&test_plan());
        assert!(
            !args.iter().any(|a| a == "-fps_mode"),
            "the media-clock output must not force cfr — the input is already CFR: {args:?}"
        );
        assert!(!args.iter().any(|a| a == "-itsoffset"), "{args:?}");
        assert!(args.iter().any(|a| a == "-shortest"), "{args:?}");
        assert!(args.windows(2).any(|w| w == ["-flush_packets", "1"]), "{args:?}");
    }

    #[test]
    fn media_clock_command_audio_inputs_are_fifos_not_pulse() {
        let args = media_clock_args(&test_plan());
        assert!(
            !args.iter().any(|a| a == "pulse"),
            "the media-clock command must carry no live pulse input: {args:?}"
        );
        assert!(args.iter().any(|a| a == "/tmp/mic.pcm"), "{args:?}");
        assert!(args.iter().any(|a| a == "/tmp/sys.pcm"), "{args:?}");
    }

    /// The rawvideo input pixel format defaults to `rgba` (locking the byte-identical
    /// Linux/Windows command line), and honours a plan that overrides `input_pix_fmt`
    /// to `bgra` (the macOS SCK worker's swizzle-free feed, DRAGON-316) WITHOUT changing
    /// any other argument. The `-pix_fmt` value sits on the rawvideo INPUT (before the
    /// video `-i -`).
    #[test]
    fn media_clock_command_input_pix_fmt_defaults_rgba_and_honours_bgra() {
        // Default plan: `-pix_fmt rgba` on the rawvideo input.
        let rgba_args = media_clock_args(&test_plan());
        let pf = rgba_args.iter().position(|a| a == "-pix_fmt").expect("has -pix_fmt");
        assert_eq!(rgba_args[pf + 1], "rgba", "default input pix_fmt is rgba: {rgba_args:?}");
        // The `-pix_fmt` sits BEFORE the video `-i -` (an input option).
        let video_i = rgba_args.iter().position(|a| a == "-i").expect("has -i");
        assert!(pf < video_i, "-pix_fmt must precede the video -i: {rgba_args:?}");

        // A plan overriding input_pix_fmt to bgra flips ONLY the -pix_fmt value; every
        // other argument stays byte-identical to the rgba plan.
        let mut bgra_plan = test_plan();
        bgra_plan.input_pix_fmt = "bgra";
        let bgra_args = media_clock_args(&bgra_plan);
        assert_eq!(bgra_args[pf + 1], "bgra", "bgra plan feeds -pix_fmt bgra: {bgra_args:?}");
        // Prove nothing else moved: the two arg vectors differ ONLY at the pix_fmt value.
        assert_eq!(rgba_args.len(), bgra_args.len());
        for (i, (a, b)) in rgba_args.iter().zip(bgra_args.iter()).enumerate() {
            if i == pf + 1 {
                continue;
            }
            assert_eq!(a, b, "arg {i} differs beyond the pix_fmt value: {a} vs {b}");
        }
    }

    /// An NV12 (hardware-feed) plan ignores `input_pix_fmt` entirely — the feed is `nv12`
    /// regardless of the RGBA-path byte order, so a stray override can never mislabel the
    /// NV12 stream.
    #[test]
    fn media_clock_command_nv12_plan_ignores_input_pix_fmt() {
        let mut plan = test_plan();
        plan.nv12 = true;
        plan.input_pix_fmt = "bgra"; // must be ignored on the nv12 path
        let args = media_clock_args(&plan);
        let pf = args.iter().position(|a| a == "-pix_fmt").expect("has -pix_fmt");
        assert_eq!(args[pf + 1], "nv12", "nv12 feed ignores input_pix_fmt: {args:?}");
    }

    #[cfg(feature = "zero-copy")]
    fn media_clock_encoded_args(hevc: bool) -> Vec<String> {
        build_media_clock_encoded_command(
            hevc, 30,
            std::path::Path::new("/tmp/out.mkv"),
            std::path::Path::new("/tmp/mic.pcm"),
            std::path::Path::new("/tmp/sys.pcm"),
        )
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
    }

    #[cfg(feature = "zero-copy")]
    #[test]
    fn media_clock_encoded_command_audio_inputs_are_fifos_not_pulse() {
        let args = media_clock_encoded_args(false);
        assert!(
            !args.iter().any(|a| a == "pulse"),
            "the media-clock zero-copy command must carry no live pulse input: {args:?}"
        );
        assert!(args.iter().any(|a| a == "/tmp/mic.pcm"), "{args:?}");
        assert!(args.iter().any(|a| a == "/tmp/sys.pcm"), "{args:?}");
        assert!(!args.iter().any(|a| a == "-itsoffset"), "{args:?}");
    }

    #[cfg(feature = "zero-copy")]
    #[test]
    fn media_clock_encoded_command_video_is_copied_and_hevc_tags_only_when_hevc() {
        let h264 = media_clock_encoded_args(false);
        assert!(h264.windows(2).any(|w| w == ["-c:v", "copy"]), "{h264:?}");
        assert!(!h264.iter().any(|a| a == "hvc1"), "{h264:?}");
        let hevc = media_clock_encoded_args(true);
        assert!(hevc.windows(2).any(|w| w == ["-tag:v", "hvc1"]), "{hevc:?}");
    }

    /// DRAGON-457: the zero-copy muxer must not be left on libavformat's default
    /// ten-second interleave window.
    ///
    /// It is a shape test because the failure it guards is invisible in one: ffmpeg
    /// buffered the entire recording, wrote its header and nothing else, and the
    /// `MuxerWatchdog` killed it at 12s as wedged — so zero-copy produced no file at all
    /// on the owner's box. The value must stay SMALL and NON-ZERO; `0` means the
    /// opposite ("buffer until every stream has a packet") and reinstates the wedge.
    #[cfg(feature = "zero-copy")]
    #[test]
    fn media_clock_encoded_command_does_not_sit_on_the_default_interleave_window() {
        for hevc in [false, true] {
            let args = media_clock_encoded_args(hevc);
            let i = args
                .iter()
                .position(|a| a == "-max_interleave_delta")
                .unwrap_or_else(|| panic!("no -max_interleave_delta (hevc={hevc}): {args:?}"));
            let v: u64 = args[i + 1].parse().expect("numeric");
            assert_eq!(v, INTERLEAVE_DELTA_US, "hevc={hevc}: {args:?}");
        }
    }

    /// The raw-frame path stays exactly as it was: it encodes every stream itself, so
    /// neither the interleave window nor the `-shortest` sync queue ever bit it. Pinned
    /// so the two fixes above cannot be "tidied" into the shared shape and change a
    /// command that works.
    #[test]
    fn the_raw_frame_media_clock_command_is_left_alone() {
        let args = media_clock_args(&test_plan());
        assert!(
            !args.iter().any(|a| a == "-max_interleave_delta"),
            "the raw-frame muxer must keep its historical argv: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "-shortest"),
            "the raw-frame muxer KEEPS -shortest; only the zero-copy one drops it: {args:?}"
        );
    }

    /// DRAGON-457: `-shortest` must never come back to the zero-copy muxer.
    ///
    /// On this path ffmpeg 8's `-shortest` sync queue discarded every audio packet — the
    /// AAC encoders ran, and 0 packets were muxed — so recordings arrived playable,
    /// correctly sized and completely silent. Nothing downstream can detect that: the
    /// finalize `amix` produces a valid (empty) result from empty inputs. This test is
    /// the only thing standing between that and a re-added flag.
    #[cfg(feature = "zero-copy")]
    #[test]
    fn the_zero_copy_muxer_never_carries_shortest() {
        for hevc in [false, true] {
            let args = media_clock_encoded_args(hevc);
            assert!(
                !args.iter().any(|a| a == "-shortest"),
                "-shortest drops ALL audio on the stream-copy path (hevc={hevc}): {args:?}"
            );
        }
    }

    #[test]
    fn bench_cores_parses_the_benchmark_line() {
        // The real shape ffmpeg 8 prints (maxrss follows on its own line).
        let err = "frame I info\nbench: utime=30.257s stime=9.290s rtime=9.918s\nbench: maxrss=329859072KiB\n";
        let cores = bench_cores(err).expect("parse");
        assert!((cores - 3.988).abs() < 0.01, "got {cores}");
        // Absent or garbled lines yield None, never a bogus number.
        assert_eq!(bench_cores("no bench line here"), None);
        assert_eq!(bench_cores("bench: utime=1.0s stime=1.0s rtime=0.000s"), None);
    }
}
