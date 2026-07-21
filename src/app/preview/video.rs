//! Video preview: a first-frame poster of the recording plus inline playback (a
//! streaming `ffmpeg` worker), the timeline editor, and frame stepping — alongside
//! the shared Save / Save As / Copy / Cancel bar.
//!
//! This is the in-overlay **micro editor**. Built (DRAGON-114): the transport's
//! three-lane timeline (`timeline.rs`) — video track, L/R soundtrack lanes, seeking
//! arm — with razor cuts, segment select/delete (hard cuts, ripple), and undo/redo
//! shared with the covermark history. Planned next: crossfade-vs-hard-cut per seam.
//! Editor state hangs off [`VideoPreview`] so it grows without touching the image
//! path; `position` is the playhead (SOURCE seconds everywhere outside the widget),
//! and [`playback::decode_frame_at`] is the frame-accurate seek the tools reuse.

use super::playback::{self, Playback};
use super::layers::{Layer, LayerKey, LayerStack, PixelFrame};
use super::timeline::{PlayPos, Timeline, TimelineCanvas};
use super::*;
use crate::app::VideoMeta;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Playful "finalizing your recording" lines shown under the spinner while a recording
/// finalizes + its poster is extracted. Picked at random when the preview opens.
pub(super) const PREVIEW_VIDEO_LOADING_MESSAGES: [&str; 20] = [
    "Finalizing your recording",
    "Wrapping up the video",
    "Muxing audio and video",
    "Rendering the final cut",
    "Rolling the credits",
    "Sweeping the cutting-room floor",
    "Syncing the soundtrack",
    "Splicing the reels together",
    "Packing it into the file",
    "Smoothing out the frames",
    "Setting the scene",
    "Cueing up your clip",
    "Threading the projector",
    "Bottling the footage",
    "Letting the tape rewind",
    "Polishing the playback",
    "Lining up the frames",
    "Tidying up the timeline",
    "Almost ready to roll",
    "Developing the reel",
];

/// The video preview's payload: the poster, inline playback state, and the scrub
/// playhead. The micro-editor's future state (timelines, segments, undo, crossfade)
/// extends this struct.
pub struct VideoPreview {
    /// The extracted first-frame poster, or `None` once extraction has run but produced
    /// no frame (the view then shows a film card instead).
    pub poster: Option<widget::image::Handle>,
    /// Whether poster extraction has finished — `false` shows the spinner.
    pub extracted: bool,
    /// Probed facts (dims/fps/duration/audio); `None` if ffprobe failed (no playback).
    pub(super) meta: Option<VideoMeta>,
    /// The live playback worker while playing; `None` when paused/stopped.
    pub(super) playback: Option<Playback>,
    /// The frame shown during playback / after a scrub (overrides the poster), drawn via
    /// the wgpu shader so rapid updates don't churn iced's image atlas.
    pub(super) frame: Option<Arc<PixelFrame>>,
    /// Playhead position in seconds — kept across pause for resume + the scrubber.
    pub(super) position: f32,
    /// Whether a single-frame scrub/step decode is in flight (coalesces rapid seeks).
    pub(super) seeking: bool,
    /// The latest requested scrub while one was in flight `(position, accurate)`.
    pub(super) pending_seek: Option<(f32, bool)>,
    /// The timeline editor's cut/delete state (kept source spans + selection +
    /// razor mode). Established when the probe lands (needs the duration);
    /// undo/redo snapshots ride the preview's shared edit history.
    pub(super) timeline: Option<Timeline>,
    /// Per-bucket L/R soundtrack peaks for the timeline's audio lanes, once
    /// extracted (kicked off with the poster; `None` for silent recordings).
    pub(super) waveform: Option<Arc<Vec<(f32, f32)>>>,
}

impl VideoPreview {
    /// A freshly-opened video preview, still finalizing / extracting its poster.
    pub fn loading() -> Self {
        Self {
            poster: None,
            extracted: false,
            meta: None,
            playback: None,
            frame: None,
            position: 0.0,
            seeking: false,
            pending_seek: None,
            timeline: None,
            waveform: None,
        }
    }

    /// Whether a recording is actively streaming frames.
    pub(super) fn is_playing(&self) -> bool {
        self.playback.is_some()
    }

    /// The source frame rate (falls back to 30 before probing completes).
    pub(super) fn fps(&self) -> f32 {
        self.meta.map(|m| m.fps).unwrap_or(30.0)
    }
}

/// Extract the poster + probe metadata off-thread, resolving to [`PreviewMsg::PosterReady`].
///
/// The probe + extraction are time-boxed on an inner thread: if ffprobe/ffmpeg ever wedges
/// on an edge-case file, the preview must still leave its spinner (DRAGON-106) rather than
/// wait forever — on timeout we resolve with no poster/meta (the view falls back to a film
/// card). `PosterReady` therefore ALWAYS fires.
pub(super) fn poster_task(path: PathBuf) -> Task<cosmic::Action<Msg>> {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let (itx, irx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let meta = Playback::probe(&path);
            let poster = extract_poster(&path, meta);
            let _ = itx.send((poster, meta));
        });
        let result = irx
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap_or((None, None));
        let _ = tx.send(result);
    });
    Task::perform(rx, |res| {
        let (poster, meta) = res.unwrap_or((None, None));
        cosmic::Action::App(Msg::Preview(PreviewMsg::PosterReady(poster, meta)))
    })
}

/// Extract the timeline's waveform buckets off-thread → [`PreviewMsg::WaveformReady`]
/// (an empty vector when the soundtrack won't decode — the lanes then stay flat).
pub(super) fn waveform_task(path: PathBuf) -> Task<cosmic::Action<Msg>> {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(super::timeline::extract_waveform(&path).unwrap_or_default());
    });
    Task::perform(rx, |res| {
        cosmic::Action::App(Msg::Preview(PreviewMsg::WaveformReady(Arc::new(
            res.unwrap_or_default(),
        ))))
    })
}

/// Build the single-frame scrub/step decode task → [`PreviewMsg::SeekFrameReady`].
fn seek_frame_task(path: PathBuf, meta: VideoMeta, t: f32, accurate: bool) -> Task<cosmic::Action<Msg>> {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(playback::decode_frame_at(&path, meta, t, accurate));
    });
    Task::perform(rx, |res| {
        let frame = res
            .ok()
            .flatten()
            .map(|f| PixelFrame::new(f.rgba, f.w, f.h));
        cosmic::Action::App(Msg::Preview(PreviewMsg::SeekFrameReady(frame)))
    })
}

/// Decode the recording's first frame as a poster via ffmpeg (piped PNG), scaled to the
/// same size playback uses so there's no size jump when Play starts/stops. `None` if
/// ffmpeg fails or the output won't decode. `-hwaccel auto` matches playback — it is
/// SAFE unconditionally (never fatal; falls back to software decode by design, see
/// `playback.rs`'s module doc), and keeps the poster on the same decode path.
fn extract_poster(path: &Path, meta: Option<VideoMeta>) -> Option<widget::image::Handle> {
    let scale = meta
        .map(|m| playback::scaled_dims(m.w, m.h))
        .map(|(w, h)| format!("scale={w}:{h}"));
    let mut cmd = crate::util::ffmpeg_command();
    cmd.args(["-v", "error", "-hwaccel", "auto"]);
    cmd.args(["-i"]).arg(path).args(["-frames:v", "1"]);
    if let Some(vf) = &scale {
        cmd.args(["-vf", vf]);
    }
    cmd.args(["-f", "image2pipe", "-vcodec", "png", "pipe:1"]);
    let out = cmd.output().ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    let img = ::image::load_from_memory(&out.stdout).ok()?;
    // into_rgba8 moves the buffer instead of cloning it.
    let rgba = img.into_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Some(widget::image::Handle::from_rgba(w, h, rgba.into_raw()))
}

/// The timeline's right-click context menu: "Cut here" (split at the clicked
/// instant) and — when there's more than one segment — "Delete segment" (the
/// segment under the click, selected at open). Styled like the overlay's
/// right-click text/code menus.
fn timeline_menu(t: f32, can_delete: bool) -> Element<'static, Msg> {
    let item = |label: &'static str, msg: PreviewMsg| {
        crate::widgets::arrow_cursor::arrow_cursor(
            widget::button::custom(widget::text(label).size(14))
                .padding(cosmic::iced::Padding::from([6.0, 14.0]))
                .width(Length::Fill)
                .on_press(Msg::Preview(msg))
                .class(cosmic::theme::Button::Text),
        )
    };
    let mut items = vec![item("Cut here", PreviewMsg::TimelineCut(t))];
    if can_delete {
        items.push(item("Delete segment", PreviewMsg::TimelineDelete));
    }
    widget::container(widget::column(items))
        .width(Length::Fixed(150.0))
        .padding(4)
        .class(cosmic::theme::Container::Custom(Box::new(|t| {
            let c = t.cosmic();
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(c.background.component.base.into())),
                text_color: Some(c.background.component.on.into()),
                border: Border {
                    radius: crate::app::theme::rounding(t).s.into(),
                    width: 1.0,
                    color: c.background.component.divider.into(),
                },
                ..Default::default()
            }
        })))
        .into()
}

/// Fit a `w`×`h` frame within the available area, preserving aspect and never upscaling
/// — returns the on-screen pixel size. Stills use this: their decode IS source
/// resolution, so growing past 1:1 would only blur.
pub(super) fn fit_dims(w: u32, h: u32, avail_w: f32, avail_h: f32) -> (f32, f32) {
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    let scale = (avail_w / w).min(avail_h / h).min(1.0);
    (w * scale, h * scale)
}

/// Format seconds as a fixed-width `HH:MM:SS` (for the monospaced transport readout, so its
/// width never jitters as digits change).
/// Fit a `w`×`h` frame to FILL the available area (aspect preserved, upscaling
/// allowed) — VIDEO display uses this: the preview stream decodes at a
/// smoothness-capped size ([`playback::scaled_dims`], ≤720p), so filling the
/// media-fitted box means scaling UP for large recordings. Display softness
/// only — the bake and the saved file always work at source resolution.
pub(super) fn contain_dims(w: u32, h: u32, avail_w: f32, avail_h: f32) -> (f32, f32) {
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    let scale = (avail_w / w).min(avail_h / h).max(0.0);
    (w * scale, h * scale)
}

fn fmt_hms(secs: f32) -> String {
    let s = secs.max(0.0).round() as u32;
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

impl App {
    /// Play/pause toggle for a video preview. Pausing stops the stream but keeps the
    /// paused frame + playhead; playing (re)starts an `ffmpeg` stream from the playhead
    /// (or the start, if we were at the end). No-op for an image preview.
    pub(super) fn toggle_playback(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(PreviewState { kind: PreviewKind::Video(vid), path, .. }) = &mut self.preview
        else {
            return Task::none();
        };
        if vid.playback.is_some() {
            // Pause: dropping the worker kills ffmpeg; the last frame stays shown.
            vid.playback = None;
            return Task::none();
        }
        let (Some(path), Some(meta)) = (path.clone(), vid.meta) else {
            return Task::none();
        };
        // Restart from the top if the playhead is at (or past) the end — the
        // EDITED end when the timeline has cuts; a playhead sitting in a
        // deleted gap (undo/delete can strand it there) snaps forward first.
        let start = match &vid.timeline {
            Some(tl) => {
                if vid.position >= tl.end() - 0.05 {
                    tl.first_start()
                } else {
                    tl.play_start(vid.position)
                }
            }
            None if vid.position >= meta.duration - 0.05 => 0.0,
            None => vid.position,
        };
        vid.position = start;
        vid.playback = Some(Playback::start(path, meta, start));
        Task::none()
    }

    /// While playing: present the frame due now (paced by the worker's shared A/V
    /// epoch), then wind down at end-of-stream. When the worker reports decode has
    /// fallen hopelessly behind the clock (a machine that can't decode this file at
    /// realtime), restart the stream at the clock — the picture jumps forward to meet
    /// the realtime audio instead of drifting ever further out of sync. No-op for an
    /// image preview.
    pub(super) fn playback_tick(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(PreviewState { kind: PreviewKind::Video(vid), path, .. }) = &mut self.preview
        else {
            return Task::none();
        };
        let (frame, catchup, finished) = {
            let Some(pb) = &mut vid.playback else {
                return Task::none();
            };
            (pb.poll(), pb.wants_catchup(), pb.finished())
        };
        if let Some(f) = frame {
            vid.frame = Some(PixelFrame::new(f.rgba, f.w, f.h));
            vid.position = f.pos;
        }
        // Timeline gap-skip: the playhead reached a deleted range — hard-cut to
        // the next kept span by restarting the stream there (the same kill +
        // respawn a catch-up jump uses, so audio follows for free); past the
        // last kept span the playback is done, whatever the file still holds.
        if let Some(tl) = vid.timeline.as_ref().filter(|t| t.edited()) {
            match tl.play_pos(vid.position) {
                PlayPos::Inside => {}
                PlayPos::Jump(next) => {
                    if let (Some(path), Some(meta)) = (path.clone(), vid.meta) {
                        vid.position = next;
                        vid.playback = Some(Playback::start(path, meta, next));
                    }
                    return Task::none();
                }
                PlayPos::Ended => {
                    vid.playback = None;
                    vid.position = tl.end();
                    return Task::none();
                }
            }
        }
        if let (Some(t), Some(path), Some(meta)) = (catchup, path.clone(), vid.meta) {
            let t = t.clamp(0.0, meta.duration);
            vid.position = t;
            vid.playback = Some(Playback::start(path, meta, t));
            return Task::none();
        }
        if finished {
            // End of stream: stop, park the playhead at the end (so Play restarts from the
            // top) and leave the last frame shown — no flash back to the poster.
            vid.playback = None;
            if let Some(m) = vid.meta {
                vid.position = m.duration;
            }
        }
        Task::none()
    }

    /// Scrub to an absolute time (seek bar): pause, move the playhead, and decode a
    /// preview frame there (fast, keyframe-approximate). No-op for an image preview.
    pub(super) fn seek(&mut self, t: f32) -> Task<cosmic::Action<Msg>> {
        self.request_frame(Some(t), false)
    }

    /// Step the playhead by `delta` frames (`,`/`.`): pause and decode that exact frame
    /// (frame-accurate). No-op for an image preview.
    pub(super) fn frame_step(&mut self, delta: i32) -> Task<cosmic::Action<Msg>> {
        let fps = match &self.preview {
            Some(PreviewState { kind: PreviewKind::Video(vid), .. }) => {
                vid.meta.map(|m| m.fps).unwrap_or(30.0)
            }
            _ => return Task::none(),
        };
        let pos = match &self.preview {
            Some(PreviewState { kind: PreviewKind::Video(vid), .. }) => vid.position,
            _ => return Task::none(),
        };
        self.request_frame(Some(pos + (delta as f32) / fps), true)
    }

    /// Shared scrub/step body: stop playback, move to `t` (clamped), and decode a single
    /// frame — coalescing if one is already in flight so a drag doesn't flood ffmpeg.
    fn request_frame(&mut self, t: Option<f32>, accurate: bool) -> Task<cosmic::Action<Msg>> {
        let Some(PreviewState { kind: PreviewKind::Video(vid), path, .. }) = &mut self.preview
        else {
            return Task::none();
        };
        let (Some(path), Some(meta)) = (path.clone(), vid.meta) else {
            return Task::none();
        };
        if let Some(t) = t {
            vid.position = t.clamp(0.0, meta.duration);
        }
        vid.playback = None; // scrubbing pauses
        if vid.seeking {
            // A decode is in flight; remember the latest target and let it pick this up.
            vid.pending_seek = Some((vid.position, accurate));
            return Task::none();
        }
        vid.seeking = true;
        seek_frame_task(path, meta, vid.position, accurate)
    }

    /// A scrubbed/stepped frame arrived: show it, then service any seek that was
    /// requested while this one was decoding.
    pub(super) fn on_seek_frame(
        &mut self,
        frame: Option<Arc<PixelFrame>>,
    ) -> Task<cosmic::Action<Msg>> {
        let Some(PreviewState { kind: PreviewKind::Video(vid), path, .. }) = &mut self.preview
        else {
            return Task::none();
        };
        if let Some(f) = frame {
            vid.frame = Some(f);
        }
        let Some((t, accurate)) = vid.pending_seek.take() else {
            vid.seeking = false;
            return Task::none();
        };
        let (Some(path), Some(meta)) = (path.clone(), vid.meta) else {
            vid.seeking = false;
            return Task::none();
        };
        vid.position = t;
        seek_frame_task(path, meta, t, accurate)
    }

    /// Ruler click/drag: scrub the playhead. `t` is SOURCE seconds (the widget
    /// already mapped the click through the edited timeline, so it always
    /// lands in kept content). Selection is untouched — that's a LANE click
    /// ([`Self::timeline_select`]).
    pub(super) fn timeline_seek(&mut self, t: f32) -> Task<cosmic::Action<Msg>> {
        if let Some(PreviewState { kind: PreviewKind::Video(vid), .. }) = &mut self.preview
            && let Some(tl) = &mut vid.timeline
        {
            tl.menu = None;
        }
        self.seek(t)
    }

    /// Lane click with the pointer tool: update the selection without moving
    /// the playhead. `t` = the clicked SOURCE time when a segment was hit
    /// (`None` = away from any segment); plain replaces (or deselects on a
    /// miss), ctrl toggles, shift range-selects from the anchor.
    pub(super) fn timeline_select(
        &mut self,
        t: Option<f32>,
        ctrl: bool,
        shift: bool,
    ) -> Task<cosmic::Action<Msg>> {
        if let Some(PreviewState { kind: PreviewKind::Video(vid), .. }) = &mut self.preview
            && let Some(tl) = &mut vid.timeline
        {
            tl.menu = None;
            match t.and_then(|t| tl.span_at_source(t)) {
                Some(i) if ctrl => tl.select_toggle(i),
                Some(i) if shift => tl.select_range_to(i),
                Some(i) => tl.select_only(Some(i)),
                // Away from any segment: a plain click deselects all; a
                // missed modifier click leaves the selection alone.
                None if !ctrl && !shift => tl.select_only(None),
                None => {}
            }
        }
        Task::none()
    }

    /// Pointer box-select (EDITED seconds): select the segments the box
    /// swept; `additive` (ctrl/shift held) keeps the current selection.
    pub(super) fn timeline_box_select(
        &mut self,
        a: f32,
        b: f32,
        additive: bool,
    ) -> Task<cosmic::Action<Msg>> {
        if let Some(PreviewState { kind: PreviewKind::Video(vid), .. }) = &mut self.preview
            && let Some(tl) = &mut vid.timeline
        {
            tl.menu = None;
            tl.select_edited_range(a, b, additive);
        }
        Task::none()
    }

    /// Razor (or context-menu "Cut here"): split the segment at source time `t`.
    /// Only a cut that actually took (not in a gap / too close to an edge)
    /// enters undo history.
    pub(super) fn timeline_cut(&mut self, t: f32) -> Task<cosmic::Action<Msg>> {
        if let Some(PreviewState { kind: PreviewKind::Video(vid), edit, .. }) = &mut self.preview
            && let Some(tl) = &mut vid.timeline
        {
            tl.menu = None;
            let prev = tl.spans.clone();
            if tl.cut_at_source(t) {
                edit.push_timeline(prev);
            }
        }
        Task::none()
    }

    /// Arm/disarm the timeline's razor (cut) tool — the pointer/razor toggle.
    pub(super) fn timeline_set_razor(&mut self, on: bool) -> Task<cosmic::Action<Msg>> {
        if let Some(PreviewState { kind: PreviewKind::Video(vid), .. }) = &mut self.preview
            && let Some(tl) = &mut vid.timeline
        {
            tl.razor = on;
            tl.menu = None;
        }
        Task::none()
    }

    /// Right-click on the timeline: select the segment under the click (so the
    /// menu's "Delete segment" acts on what was clicked, and the ring shows it)
    /// and open the context menu at that point. A right-click INSIDE a
    /// multi-selection keeps it — the menu then acts on the whole group.
    pub(super) fn timeline_menu_open(&mut self, t: f32, x: f32, y: f32) -> Task<cosmic::Action<Msg>> {
        if let Some(PreviewState { kind: PreviewKind::Video(vid), .. }) = &mut self.preview
            && let Some(tl) = &mut vid.timeline
        {
            let hit = tl.span_at_source(t);
            if !hit.is_some_and(|i| tl.selected.contains(&i)) {
                tl.select_only(hit);
            }
            tl.menu = Some((t, x, y));
        }
        Task::none()
    }

    /// Dismiss the timeline context menu without acting.
    pub(super) fn timeline_menu_close(&mut self) -> Task<cosmic::Action<Msg>> {
        if let Some(PreviewState { kind: PreviewKind::Video(vid), .. }) = &mut self.preview
            && let Some(tl) = &mut vid.timeline
        {
            tl.menu = None;
        }
        Task::none()
    }

    /// Delete the selected segments (one undoable edit); the segments after
    /// them slide left inherently. A paused playhead stranded in a new gap
    /// re-decodes at the seam so the shown frame is never deleted content (a
    /// PLAYING stream skips on its own — the tick's gap-jump).
    pub(super) fn timeline_delete_selected(&mut self) -> Task<cosmic::Action<Msg>> {
        let seam = {
            let Some(PreviewState { kind: PreviewKind::Video(vid), edit, .. }) =
                &mut self.preview
            else {
                return Task::none();
            };
            let Some(tl) = &mut vid.timeline else {
                return Task::none();
            };
            tl.menu = None;
            let prev = tl.spans.clone();
            if !tl.delete_selected() {
                return Task::none();
            }
            edit.push_timeline(prev);
            match (vid.playback.is_some(), tl.play_pos(vid.position)) {
                (false, PlayPos::Jump(next)) => Some(next),
                (false, PlayPos::Ended) => Some((tl.end() - 0.05).max(tl.first_start())),
                _ => None,
            }
        };
        match seam {
            Some(t) => self.seek(t),
            None => Task::none(),
        }
    }

    /// Store the extracted waveform buckets (empty = extraction failed / silent;
    /// the lanes then stay flat).
    pub(super) fn on_waveform(&mut self, peaks: Arc<Vec<(f32, f32)>>) -> Task<cosmic::Action<Msg>> {
        if let Some(PreviewState { kind: PreviewKind::Video(vid), .. }) = &mut self.preview
            && !peaks.is_empty()
        {
            vid.waveform = Some(peaks);
        }
        Task::none()
    }

    /// Stop any in-progress playback (kills the ffmpeg worker), e.g. before an action
    /// exits or hides the overlay. No-op for an image preview / when not playing.
    pub(super) fn stop_preview_playback(&mut self) {
        if let Some(PreviewState { kind: PreviewKind::Video(vid), .. }) = &mut self.preview {
            vid.playback = None;
        }
        // Closing the overlay: resume any other media we paused on its behalf (no-op if none).
        self.preview_duck = None;
    }

    /// The loaded-video view: the current playback/scrub frame (else the poster, else a
    /// film card), an optional scrub bar, and a Play/Pause group plus the shared bar.
    pub(super) fn video_loaded_view<'a>(
        &'a self,
        preview: &'a PreviewState,
        vid: &'a VideoPreview,
        tb: Tb,
    ) -> Element<'a, Msg> {
        // The media-hugging viewport (falls back to the full box before the probe
        // lands), so the overlay's toolbars sit tight above/below the recording.
        let (avail_w, avail_h) = self.preview_viewport(preview);
        let cm_frame = preview.edit.cm_raster.frame();
        let content: Element<'a, Msg> = if let Some(frame) = &vid.frame {
            // A live/paused/scrubbed frame: ONE LayerStack draws the video frame and
            // (when applied) the covermark together, each in its OWN persistent texture
            // slot — no atlas churn, at the same fit as the poster. This is the fix for
            // the old shared-texture defect: two separate same-type shader primitives
            // (the video frame and the covermark) shared ONE pipeline texture, so during
            // playback with a covermark applied both prepares wrote it and both draws
            // sampled whichever upload happened last.
            let (dw, dh) = contain_dims(frame.w, frame.h, avail_w, avail_h);
            let mut layers = vec![Layer { key: LayerKey::VIDEO, frame: frame.clone() }];
            if let Some(cm) = cm_frame {
                layers.push(Layer { key: LayerKey::COVERMARK, frame: cm.clone() });
            }
            let shader = cosmic::iced::widget::shader::Shader::new(LayerStack::new(layers))
                .width(Length::Fixed(dw))
                .height(Length::Fixed(dh));
            widget::container(Element::new(shader))
                .center_x(Length::Fill)
                .into()
        } else {
            // No live frame: the poster (or a film card), with the covermark applied.
            self.video_still_content(preview, vid, avail_w, avail_h, cm_frame)
        };
        let (play_icon, play_tip) = if vid.is_playing() {
            ("media-playback-pause-symbolic", "Pause  (P)")
        } else {
            ("media-playback-start-symbolic", "Play  (P)")
        };
        // Left: do-not-train + covermark tools. (Save / Save As / Copy, size + Delete,
        // appearance, and Close live on top; play now lives with the seek bar.)
        // `Vec<Element<'static, _>>` is a subtype of `Vec<Element<'a, _>>` (Element
        // is covariant in its lifetime), so this is a plain re-binding.
        let left: Vec<Element<'a, Msg>> = self.edit_tools(preview, tb);
        let toolbar = toolbar_row(left, Vec::new(), Vec::new());

        // The transport strip — a tool row (play on the left; the seek time,
        // the pointer/razor toggle, and segment delete on the right) stacked
        // over the full-width timeline editor (measurement ruler + video and
        // L/R audio lanes, with the danger-red seek arm). It renders in its
        // OWN bar between the canvas and the action toolbar (compose_preview
        // slots + styles it; every sizing path reserves its height via
        // `preview_transport_h`), shown once the duration is known.
        let transport: Option<Element<'a, Msg>> =
            vid.meta.filter(|m| m.duration > 0.0).map(|meta| {
                // Borderless play button (no group box) — it's part of the
                // transport, not a toolbar.
                let play =
                    tb.tool_button(play_icon, play_tip, PreviewMsg::Play, widget::tooltip::Position::Top);
                let Some(tl) = &vid.timeline else {
                    // Probe landed without a timeline (shouldn't happen — both are
                    // set together): fall back to the plain seek slider.
                    let remaining = (meta.duration - vid.position).max(0.0);
                    let slider =
                        widget::slider(0.0..=meta.duration, vid.position.min(meta.duration), |t| {
                            Msg::Preview(PreviewMsg::Seek(t))
                        })
                        .step(0.05f32);
                    return widget::row(vec![
                        play,
                        slider.width(Length::Fill).into(),
                        widget::text(fmt_hms(remaining))
                            .size(12)
                            .font(cosmic::font::mono())
                            .into(),
                    ])
                    .spacing(8.0)
                    .align_y(Alignment::Center)
                    .into();
                };
                let lanes = cosmic::widget::Canvas::new(TimelineCanvas {
                    timeline: tl,
                    position: vid.position,
                    fps: meta.fps,
                    waveform: vid.waveform.as_deref().map(|w| w.as_slice()),
                })
                .width(Length::Fill)
                .height(Length::Fixed(super::timeline::RULER_H + super::timeline::LANES_H));
                // Right-click context menu: "Cut here" at the clicked instant,
                // "Delete segment" for the segment under the click (which the
                // open selected). Anchored at the click point; an outside click
                // dismisses.
                let lanes: Element<'a, Msg> = match tl.menu {
                    Some((t, mx, my)) => widget::popover(Element::new(lanes))
                        .popup(timeline_menu(t, tl.spans.len() > 1))
                        .position(widget::popover::Position::Point(cosmic::iced::Point::new(
                            mx, my,
                        )))
                        .on_close(Msg::Preview(PreviewMsg::TimelineMenuClose))
                        .into(),
                    None => Element::new(lanes),
                };
                // The now-playing time (EDITED position / duration, in the
                // editor's one fixed-width HH:MM:SS:FF timecode format — same
                // as the ruler labels); it sits right of the play button. The
                // pointer/razor tool toggle — a segmented pair matching the
                // region toolbar's mode selector — leads the row; delete acts
                // on the selection from the far right.
                let time = widget::text(format!(
                    "{} / {}",
                    super::timeline::fmt_timecode(tl.source_to_edited(vid.position), meta.fps),
                    super::timeline::fmt_timecode(tl.edited_duration(), meta.fps),
                ))
                .size(12)
                .font(cosmic::font::mono());
                let tools = widget::row(vec![
                    tb.seg_toggle(
                        "input-mouse-symbolic",
                        !tl.razor,
                        PreviewMsg::TimelineRazor(false),
                        "Pointer: click to seek, click a segment to select",
                        true,
                        false,
                    ),
                    tb.seg_toggle(
                        "edit-cut-symbolic",
                        tl.razor,
                        PreviewMsg::TimelineRazor(true),
                        "Razor: click the timeline to cut it",
                        false,
                        true,
                    ),
                ]);
                let del = tb.history_button(
                    "edit-delete-symbolic",
                    "Delete selected segments  (Del)",
                    PreviewMsg::TimelineDelete,
                    !tl.selected.is_empty(),
                    widget::tooltip::Position::Top,
                );
                let top = widget::row(vec![
                    tools.into(),
                    play,
                    time.into(),
                    toolbar_split(),
                    del,
                ])
                .spacing(8.0)
                .width(Length::Fill)
                .align_y(Alignment::Center);
                widget::column(vec![top.into(), lanes])
                    .spacing(6.0)
                    .width(Length::Fill)
                    .into()
            });
        compose_preview(
            preview.surface.is_window(),
            self.overlay_control_width(preview),
            self.edit_toolbar(preview, tb),
            content,
            transport,
            toolbar,
            tb.glass,
        )
    }

    /// The video canvas content when NOTHING is playing: the poster (fitted like the live
    /// frames so nothing jumps at Play), or a film card when the poster is missing — with the
    /// covermark stacked over it through the persistent-texture shader (in-place upload,
    /// alpha-blended, no atlas churn). Windows OVERLAY exception (DRAGON-235): iced's
    /// raster-image pipeline does not composite on the premultiplied transparent surface, so a
    /// sized poster is drawn through the SAME LayerStack shader instead — with the covermark
    /// folded into that one stack, so only a single LayerStack ever lives on the surface (two
    /// would fight over slot pruning). The opaque windowed surface, Linux and macOS compile
    /// only the portable `widget::image` paths below (byte-identical there).
    fn video_still_content(
        &self,
        preview: &PreviewState,
        vid: &VideoPreview,
        avail_w: f32,
        avail_h: f32,
        cm_frame: Option<&std::sync::Arc<PixelFrame>>,
    ) -> Element<'static, Msg> {
        #[cfg(windows)]
        if !preview.surface.is_window()
            && let Some(poster) = &vid.poster
            && let Some(m) = vid.meta
            && let Some(pf) = super::layers::rgba_handle_frame(poster)
        {
            let (sw, sh) = playback::scaled_dims(m.w, m.h);
            let (dw, dh) = contain_dims(sw, sh, avail_w, avail_h);
            let mut layers = vec![Layer { key: LayerKey::VIDEO, frame: pf }];
            if let Some(cm) = cm_frame {
                layers.push(Layer { key: LayerKey::COVERMARK, frame: cm.clone() });
            }
            let shader = cosmic::iced::widget::shader::Shader::new(LayerStack::new(layers))
                .width(Length::Fixed(dw))
                .height(Length::Fixed(dh));
            return widget::container(Element::new(shader)).center_x(Length::Fill).into();
        }
        let content: Element<'static, Msg> = if let Some(poster) = &vid.poster {
            // The poster was extracted at the playback scale (≤720p): size it to
            // FILL the fitted box exactly like the live frames, so nothing jumps
            // at Play and large recordings aren't shown as a small patch.
            if let Some(m) = vid.meta {
                let (sw, sh) = playback::scaled_dims(m.w, m.h);
                let (dw, dh) = contain_dims(sw, sh, avail_w, avail_h);
                widget::container(
                    widget::image(poster.clone())
                        .content_fit(cosmic::iced::ContentFit::Fill)
                        .width(Length::Fixed(dw))
                        .height(Length::Fixed(dh)),
                )
                .center_x(Length::Fill)
                .into()
            } else {
                // Probe failed → the poster's pixel size is unknown; show it plain.
                widget::container(
                    widget::image(poster.clone()).content_fit(cosmic::iced::ContentFit::ScaleDown),
                )
                .center_x(Length::Fill)
                .max_height(avail_h)
                .into()
            }
        } else {
            // No poster — a film card with the recording's filename.
            let name = preview
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "recording.mp4".to_string());
            let icon =
                widget::icon::Icon::from(widget::icon::from_name("video-x-generic-symbolic").size(96));
            widget::container(
                widget::column(vec![icon.into(), widget::text(name).size(16).into()])
                    .spacing(12.0)
                    .align_x(Alignment::Center),
            )
            .center_x(Length::Fill)
            .max_height(avail_h)
            .into()
        };
        // Covermark ops preview for the POSTER / film-card path (the Windows overlay poster
        // above already folded the covermark into its own LayerStack): stack a covermark-only
        // LayerStack over `content` at the content's display size (the bake composites the
        // same rasters at source resolution, so this is faithful).
        if let Some(frame) = cm_frame
            && let Some(meta) = vid.meta
        {
            let (sw, sh) = playback::scaled_dims(meta.w, meta.h);
            let (dw, dh) = contain_dims(sw, sh, avail_w, avail_h);
            // The covermark overlay draws through the persistent-texture shader (in-place
            // upload, alpha-blended over the frame) — no atlas churn, so no blink on edit.
            let layers = LayerStack::new(vec![Layer { key: LayerKey::COVERMARK, frame: frame.clone() }]);
            let shader = cosmic::iced::widget::shader::Shader::new(layers)
                .width(Length::Fixed(dw))
                .height(Length::Fixed(dh));
            let overlay = widget::container(Element::new(shader)).center_x(Length::Fill);
            cosmic::iced::widget::stack(vec![content, overlay.into()]).into()
        } else {
            content
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_dims_downscales_preserving_aspect_on_the_limiting_axis() {
        // 16:9 source into a box half its width but full-height: width is the limiting
        // axis, so both dims scale by 0.5.
        assert_eq!(fit_dims(1920, 1080, 960.0, 1080.0), (960.0, 540.0));
    }

    #[test]
    fn fit_dims_picks_whichever_axis_is_more_constrained() {
        // Portrait source into a landscape box: height is the limiting axis (0.5625)
        // even though the width ratio (1.777..) would allow more.
        let (w, h) = fit_dims(1080, 1920, 1920.0, 1080.0);
        assert!((w - 607.5).abs() < 0.01, "w = {w}");
        assert_eq!(h, 1080.0);
    }

    #[test]
    fn contain_dims_fills_the_box_in_both_directions() {
        // Upscales a small (≤720p preview-stream) frame to fill the fitted box…
        let (w, h) = contain_dims(1280, 720, 2560.0, 2000.0);
        assert_eq!((w, h), (2560.0, 1440.0));
        // …and downscales an oversized one, both keeping aspect exactly.
        let (w, h) = contain_dims(3840, 2160, 1920.0, 2000.0);
        assert_eq!((w, h), (1920.0, 1080.0));
    }

    #[test]
    fn fit_dims_never_upscales_past_source_size() {
        // A tiny frame in a huge available area stays at its native size (scale capped
        // at 1.0), matching the poster's ScaleDown.
        assert_eq!(fit_dims(100, 100, 1000.0, 1000.0), (100.0, 100.0));
    }

    #[test]
    fn fmt_hms_pads_to_a_fixed_two_digit_width() {
        assert_eq!(fmt_hms(0.0), "00:00:00");
        assert_eq!(fmt_hms(9.0), "00:00:09");
    }

    #[test]
    fn fmt_hms_rolls_seconds_into_minutes_and_minutes_into_hours() {
        assert_eq!(fmt_hms(65.0), "00:01:05");
        assert_eq!(fmt_hms(3661.0), "01:01:01");
    }

    #[test]
    fn fmt_hms_rounds_to_the_nearest_second() {
        // 59.6s rounds to 60s, which rolls into the next minute rather than clipping to
        // "00:00:59" or truncating oddly.
        assert_eq!(fmt_hms(59.6), "00:01:00");
    }
}
