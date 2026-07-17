//! `RecordingMsg` sub-enum split out of the former flat `Msg` (see app/mod.rs).

#[derive(Debug, Clone)]
pub enum RecordingMsg {
    /// Settings: audio→video sync offset (ms).
    SetAudioSyncOffset(String),
    /// Settings: toggle auto A/V sync calibration.
    SetAudioSyncAuto(bool),
    /// Stop the in-progress recording (finalize + save).
    StopRecording,
    /// Pause the in-progress recording, or resume it when paused — nothing is
    /// captured in between (DRAGON-111).
    TogglePause,
    /// Cancel the recording and discard its file (no save, no notification).
    CancelRecording,
    /// Poll the recording worker for completion.
    RecordingPoll,
    /// Refresh the live audio levels for the on-button meters (when not recording).
    MeterTick,
    /// Toggle recording microphone / system audio (video mode only).
    ToggleMic,
    ToggleSystemAudio,
    /// Drain the system-tray menu clicks (fired on a timer while the tray is up).
    TrayPoll,
}
