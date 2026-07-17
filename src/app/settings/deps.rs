//! Unified capability + dependency model.
//!
//! The two things the app must be able to do are modelled as *capabilities*:
//! taking a screenshot and recording the screen. Each is satisfied when at least
//! one probed *capture method* supports it (`App::capture_methods`). This is the
//! stopgap that keeps the required checks honest as more compositor backends are
//! added later: teach the app a new method and the capability checks pick it up
//! for free, with no other edits.
//!
//! On top of that sit optional external tools (tesseract, pactl) that only enable
//! extra features. Everything is declared once in `App::dep` and renders two ways:
//!
//!   * `Dep::note` - a compact inline line for the relevant setting page, e.g.
//!     "[ok] Screen recording: At least one recording method is available."
//!   * `Dep::row`  - a row on the Health page (name + message + status icon).
//!
//! Required-but-missing is red, optional-but-missing is amber, present is green.

use super::*;
use super::row::{action_button, severity_caption, status_icon, Item, Severity};
#[cfg(target_os = "macos")]
use crate::platform::mac::tcc::TccStatus;
use std::borrow::Cow;

/// Whether a capability/dependency is essential (its absence breaks core function)
/// or optional (only a feature is lost). Drives the missing-state severity:
/// Required is an error (red), Optional is a warning (amber).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::app::settings) enum Requirement {
    Required,
    Optional,
}

/// The requirements the app surfaces. Reference one by id (e.g. on a setting page)
/// to render its note; `App::deps` lists them all for the Health page.
#[derive(Clone, Copy)]
pub(in crate::app::settings) enum DepId {
    /// Required: at least one capture method can take a screenshot.
    Screenshot,
    /// Required: at least one capture method can record.
    Recording,
    /// Required: the ffmpeg binary (recording, preview playback, audio capture).
    Ffmpeg,
    /// Required: the ffprobe binary (video metadata for preview playback).
    Ffprobe,
    /// Optional: OCR text recognition.
    Tesseract,
    /// Optional: OCR language data (the tesseract binary alone can't OCR).
    TesseractLang,
    /// Optional: specific audio device selection.
    Pactl,
    /// Optional: hardware video encoding (NVENC / VAAPI).
    HwEncoder,
    /// Required (macOS): the Screen Recording TCC grant — capture is blank without it.
    #[cfg(target_os = "macos")]
    ScreenRecording,
    /// Optional (macOS): the Microphone TCC grant — video-only recording still works.
    #[cfg(target_os = "macos")]
    Microphone,
}

/// A resolved requirement: its static metadata plus whether it is satisfied now.
pub(in crate::app::settings) struct Dep {
    name: &'static str,
    present: bool,
    requirement: Requirement,
    /// Message shown when present (reads after "name: ").
    ok: Cow<'static, str>,
    /// Message shown when missing (reads after "name: ").
    missing: Cow<'static, str>,
    /// An optional remediation action rendered as the row's control (a button)
    /// instead of the plain status icon — e.g. a macOS TCC row's "Open Settings" /
    /// "Request". `(label, message)`. `None` (the norm, and always on Linux) keeps
    /// the historical status-icon control.
    action: Option<(&'static str, Msg)>,
}

impl Dep {
    /// Effective severity given presence (present is ok; otherwise by requirement).
    fn severity(&self) -> Severity {
        match (self.present, self.requirement) {
            (true, _) => Severity::Ok,
            (false, Requirement::Required) => Severity::Error,
            (false, Requirement::Optional) => Severity::Warn,
        }
    }

    pub(in crate::app::settings) fn is_required(&self) -> bool {
        self.requirement == Requirement::Required
    }

    /// Whether the dependency is satisfied. The single source of truth for both the
    /// status note and whether dependent controls are gated, so they never disagree
    /// (and the `CCK_HEALTH_FORCE_WARN` review flag drives both).
    pub(in crate::app::settings) fn is_present(&self) -> bool {
        self.present
    }

    /// The active message (present vs missing), owned so renderings don't borrow self.
    fn message(&self) -> String {
        if self.present { self.ok.to_string() } else { self.missing.to_string() }
    }

    /// Compact inline note for a setting page: a status icon followed by
    /// "name: message", coloured by severity. Renders as a full-width row, no control.
    pub(in crate::app::settings) fn note<'a>(&self) -> Item<'a> {
        let sev = self.severity();
        let line = widget::row(vec![
            status_icon(sev),
            severity_caption(sev, format!("{}: {}", self.name, self.message())),
        ])
        .spacing(8.0)
        .align_y(Alignment::Center);
        Item::note(line)
    }

    /// The inline note, but only when the dependency has a problem - so settings pages
    /// stay quiet when everything is fine. The Health page still lists every entry
    /// (it uses `row`, not this).
    pub(in crate::app::settings) fn note_if_issue<'a>(&self) -> Option<Item<'a>> {
        (!self.present).then(|| self.note())
    }

    /// Health-page row: the name as the title, the message as the helper line, and —
    /// on the right — a remediation action button when the row carries one (a missing
    /// TCC grant), otherwise the severity status icon.
    pub(in crate::app::settings) fn row<'a>(&self) -> Item<'a> {
        let sev = self.severity();
        let control = match &self.action {
            Some((label, msg)) => action_button(label, msg.clone()),
            None => status_icon(sev),
        };
        Item::new(self.name, self.message(), control).status(sev)
    }
}

impl crate::app::App {
    /// A friendly label for the best available hardware video encoder, if any.
    fn hw_encoder_label(&self) -> Option<&str> {
        self.encoders()
            .iter()
            .find(|e| {
                e.id.contains("nvenc")
                    || e.id.contains("vaapi")
                    || e.id.contains("videotoolbox")
            })
            .map(|e| e.label.as_str())
    }

    /// Every capture backend's capabilities in this environment. Implement
    /// `platform::backend::CaptureBackend` (and add it to `backend::backends`) to
    /// teach the app a new compositor/OS; these checks pick it up for free.
    fn capture_caps(&self) -> Vec<crate::platform::backend::Caps> {
        crate::platform::backend::backends(self.pipewire_available, self.ffmpeg_available)
            .iter()
            .map(|b| b.caps())
            .collect()
    }

    /// Whether at least one capture backend can take a screenshot.
    fn can_screenshot(&self) -> bool {
        self.capture_caps().iter().any(|c| c.screenshot)
    }

    /// Whether at least one capture backend can record.
    fn can_record(&self) -> bool {
        self.capture_caps().iter().any(|c| c.record)
    }

    /// Re-derive the "Capture method" dropdown contents from `backends()`. Called
    /// when a runtime input to the backend caps changes (the portal probe); the
    /// rest (protocols, ffmpeg) is fixed for the session.
    pub(in crate::app) fn rebuild_capture_methods(&mut self) {
        self.screenshot_methods = crate::platform::backend::method_choices(
            self.pipewire_available,
            self.ffmpeg_available,
            |c| c.screenshot,
        );
        self.record_methods = crate::platform::backend::method_choices(
            self.pipewire_available,
            self.ffmpeg_available,
            |c| c.record,
        );
    }

    /// Resolve one requirement to its current state. This match is the single source
    /// of every requirement's name, severity, and messages.
    pub(in crate::app::settings) fn dep(&self, id: DepId) -> Dep {
        let (name, present, requirement, ok, missing): (
            &'static str,
            bool,
            Requirement,
            Cow<'static, str>,
            Cow<'static, str>,
        ) = match id {
            DepId::Screenshot => (
                "Screenshot capture",
                self.can_screenshot(),
                Requirement::Required,
                "At least one capture method is available.".into(),
                "No capture method available.".into(),
            ),
            DepId::Recording => (
                "Screen recording",
                self.can_record(),
                Requirement::Required,
                "At least one recording method is available.".into(),
                "No recording method available.".into(),
            ),
            DepId::Ffmpeg => (
                "ffmpeg",
                self.ffmpeg_available,
                Requirement::Required,
                "Screen recording, video preview playback, and the audio meters are available.".into(),
                "Screen recording, video preview playback, the mic test and the audio meters are disabled. Install 'ffmpeg'.".into(),
            ),
            DepId::Ffprobe => (
                "ffprobe",
                self.ffprobe_available,
                Requirement::Required,
                "Recordings can be probed for preview playback.".into(),
                "Recordings can't be previewed. Install 'ffmpeg' (it provides ffprobe; some distros package it separately).".into(),
            ),
            DepId::Tesseract => (
                "tesseract",
                self.tesseract_available,
                Requirement::Optional,
                "Text recognition (OCR) is available.".into(),
                "Text scanning is disabled. Install 'tesseract' and a language pack (e.g. tesseract-data-eng).".into(),
            ),
            DepId::TesseractLang => (
                "tesseract language data",
                // Lazy: shells out to `tesseract --list-langs` once, on the first
                // Health/Scanner query — launch never pays for it. The binary being
                // missing also reads as missing data (installing it comes first).
                *self
                    .tesseract_langs
                    .get_or_init(crate::detect::tesseract_langs_available),
                Requirement::Optional,
                "OCR language data is installed.".into(),
                "The tesseract binary has no usable language data, so every OCR pass fails. Install a language pack (e.g. tesseract-data-eng).".into(),
            ),
            DepId::Pactl => {
                // On macOS there is no PulseAudio. Microphone selection works through
                // avfoundation (enumerated via ffmpeg, DRAGON-132), so it gates on
                // ffmpeg rather than pactl. There is deliberately no OUTPUT picker:
                // system audio is captured from the ScreenCaptureKit mix (which also
                // serves as the echo-cancellation reference) and playback follows the
                // system default output, so a device choice would change nothing.
                // Linux keeps its exact pactl wording.
                #[cfg(target_os = "macos")]
                let (name, present, ok, missing) = (
                    "audio device selection",
                    self.ffmpeg_available,
                    "A specific microphone can be selected. System audio is captured from \
                     the ScreenCaptureKit mix and playback uses the system default output, \
                     so there is no output device to choose.",
                    "Listing microphones needs ffmpeg; the system default microphone is \
                     used until it is available.",
                );
                #[cfg(not(target_os = "macos"))]
                let (name, present, ok, missing) = (
                    "pactl",
                    self.pactl_available,
                    "Specific input and output devices can be selected.",
                    "Only the system default audio devices are offered. Install pipewire-pulse \
                     (or pulseaudio) to choose specific input and output devices.",
                );
                (name, present, Requirement::Optional, ok.into(), missing.into())
            }
            DepId::HwEncoder => {
                let label = self.hw_encoder_label();
                // The post-driver-update NVML mismatch reads as a warning even though
                // NVENC is still listed (the persisted choice must survive the
                // transient state): recordings fall back until the module reloads.
                let missing: Cow<'static, str> = if self.nvenc_driver_mismatch {
                    "The NVIDIA driver was updated but the previous kernel module is \
                     still loaded, so hardware encoding (NVENC) is unavailable and \
                     recordings fall back to the next best encoder. Reboot or update \
                     your Nvidia packages."
                        .into()
                } else {
                    "No hardware encoder detected. Recordings use software encoding (libx264).".into()
                };
                (
                    "Hardware video encoder",
                    label.is_some() && !self.nvenc_driver_mismatch,
                    Requirement::Optional,
                    match label {
                        Some(l) => format!("Hardware accelerated recording is available ({l}).").into(),
                        None => Cow::Borrowed("Hardware accelerated recording is available."),
                    },
                    missing,
                )
            }
            #[cfg(target_os = "macos")]
            DepId::ScreenRecording => (
                "Screen Recording",
                crate::platform::mac::tcc::screen_capture_granted(),
                Requirement::Required,
                "Screen capture is authorised.".into(),
                "Screen capture is NOT authorised, so screenshots and recordings come out blank. \
                 Grant Screen Recording access, then relaunch the app."
                    .into(),
            ),
            #[cfg(target_os = "macos")]
            DepId::Microphone => {
                let status = crate::platform::mac::tcc::mic_status();
                // NotDetermined ≠ Denied: reflect the three states honestly. The
                // Granted branch's string is never shown (present ⇒ the ok line).
                let missing: Cow<'static, str> = match status {
                    TccStatus::NotDetermined =>
                        "Microphone access hasn't been requested yet. Video-only recording still \
                         works; grant it to include your voice.".into(),
                    _ =>
                        "Microphone access is denied. Video-only recording still works; enable it \
                         in System Settings to include your voice.".into(),
                };
                (
                    "Microphone",
                    status == TccStatus::Granted,
                    Requirement::Optional,
                    "Microphone audio can be recorded with videos.".into(),
                    missing,
                )
            }
        };
        // Review aids (inert unless the env var is set): CCK_HEALTH_FORCE_WARN forces every
        // Optional requirement to read as missing (amber); CCK_HEALTH_FORCE_DANGER forces
        // every Required capability to read as missing (red). Lets the warning and error UI
        // be reviewed without actually breaking anything.
        let forced_missing = match requirement {
            Requirement::Optional => std::env::var_os("CCK_HEALTH_FORCE_WARN").is_some(),
            Requirement::Required => std::env::var_os("CCK_HEALTH_FORCE_DANGER").is_some(),
        };
        let present = present && !forced_missing;
        // A remediation action button (macOS TCC rows only) when the grant is missing.
        #[cfg(target_os = "macos")]
        let action = self.tcc_row_action(id, present);
        #[cfg(not(target_os = "macos"))]
        let action: Option<(&'static str, Msg)> = None;
        Dep { name, present, requirement, ok, missing, action }
    }

    /// The remediation action for a macOS TCC health row, given its resolved presence.
    /// The per-state choice itself is pure ([`screen_tcc_action`] / [`mic_tcc_action`]);
    /// this just feeds it the live probes and maps the result to a label + message.
    /// `None` for every non-TCC dep. Based on `present` so the CCK_HEALTH_FORCE_*
    /// review flags surface the button too.
    #[cfg(target_os = "macos")]
    fn tcc_row_action(&self, id: DepId, present: bool) -> Option<(&'static str, Msg)> {
        use crate::platform::mac::tcc::{mic_status, PrivacyPane};
        let (action, pane, request) = match id {
            DepId::ScreenRecording => (
                // `mac_first_run_seen` is the only record of whether the one-shot
                // prompt was spent (CGPreflight can't tell NotDetermined from
                // Denied). Read from disk only when the row is actually missing.
                screen_tcc_action(present, crate::state::load().mac_first_run_seen),
                PrivacyPane::ScreenCapture,
                SettingsMsg::RequestScreenTcc,
            ),
            DepId::Microphone => (
                mic_tcc_action(mic_status(), present),
                PrivacyPane::Microphone,
                SettingsMsg::RequestMicTcc,
            ),
            _ => return None,
        };
        action.map(|a| match a {
            TccAction::Request => ("Request", Msg::Settings(request)),
            TccAction::OpenSettings => {
                ("Open Settings", Msg::Settings(SettingsMsg::OpenTccPane(pane)))
            }
        })
    }

    /// Every requirement, in display order (required capabilities first).
    pub(in crate::app::settings) fn deps(&self) -> Vec<Dep> {
        let mut ids: Vec<DepId> = Vec::new();
        // macOS: the Screen Recording TCC grant leads the Required list — capture is
        // blank without it, so it's the first thing to check.
        #[cfg(target_os = "macos")]
        ids.push(DepId::ScreenRecording);
        ids.extend([
            DepId::Screenshot,
            DepId::Recording,
            DepId::Ffmpeg,
            DepId::Ffprobe,
            DepId::Tesseract,
            DepId::TesseractLang,
            DepId::Pactl,
            DepId::HwEncoder,
        ]);
        // macOS: the Microphone TCC grant is an Optional feature (video-only still works).
        #[cfg(target_os = "macos")]
        ids.push(DepId::Microphone);
        ids.into_iter().map(|id| self.dep(id)).collect()
    }

    /// The worst severity across all requirements (drives the nav health icon).
    pub(in crate::app::settings) fn health_level(&self) -> Severity {
        self.deps()
            .iter()
            .map(Dep::severity)
            .max()
            .unwrap_or(Severity::Ok)
    }

    /// Refresh the Health nav entry's stored icon to the current overall severity
    /// (the severity glyph tinted to its colour). The collapsed rail builds its icon
    /// live each frame, but the expanded `nav_bar` renders the icon stored on the
    /// model; the stored `Icon` carries its own `Svg::Custom` colour filter, which the
    /// segmented widget honours over the nav's themed colour, so the expanded entry
    /// shows green/amber/red too. Call when health-affecting state changes (settings
    /// open, portal probe).
    pub(in crate::app) fn update_health_nav_icon(&mut self) {
        let sev = self.health_level();
        let icon = cosmic::widget::icon::from_name(sev.icon_name())
            .icon()
            .class(cosmic::theme::Svg::custom(move |theme| {
                cosmic::widget::svg::Style { color: Some(sev.color(theme)) }
            }));
        self.settings.nav.icon_set(self.settings.health, icon);
    }
}

/// A TCC health row's remediation, chosen purely from probe state (so the choice is
/// unit-testable); `App::tcc_row_action` maps it to the concrete label + message.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TccAction {
    /// The one-shot OS permission prompt is still available — offer it.
    Request,
    /// The prompt has been spent (or access denied) — deep-link to System Settings.
    OpenSettings,
}

/// Screen Recording remediation. `Request` only while the one-shot prompt hasn't
/// been fired this TCC lifetime (`request_spent` = the persisted `mac_first_run_seen`
/// — the preflight API can't distinguish NotDetermined from Denied); after that a
/// re-request just silently returns the standing decision, so System Settings is the
/// only honest action.
#[cfg(target_os = "macos")]
fn screen_tcc_action(present: bool, request_spent: bool) -> Option<TccAction> {
    if present {
        None
    } else if request_spent {
        Some(TccAction::OpenSettings)
    } else {
        Some(TccAction::Request)
    }
}

/// Microphone remediation per TCC status: NotDetermined → the one-shot prompt;
/// Denied → System Settings; Granted → none (unless a CCK_HEALTH_FORCE_* review
/// flag forces the row missing, where Settings keeps the amber row actionable).
#[cfg(target_os = "macos")]
fn mic_tcc_action(status: TccStatus, present: bool) -> Option<TccAction> {
    match status {
        TccStatus::NotDetermined => Some(TccAction::Request),
        TccStatus::Denied => Some(TccAction::OpenSettings),
        TccStatus::Granted => (!present).then_some(TccAction::OpenSettings),
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn screen_granted_offers_no_action() {
        assert_eq!(screen_tcc_action(true, false), None);
        assert_eq!(screen_tcc_action(true, true), None);
    }

    #[test]
    fn screen_missing_with_unspent_prompt_offers_request() {
        assert_eq!(screen_tcc_action(false, false), Some(TccAction::Request));
    }

    #[test]
    fn screen_missing_with_spent_prompt_opens_settings() {
        assert_eq!(screen_tcc_action(false, true), Some(TccAction::OpenSettings));
    }

    #[test]
    fn mic_not_determined_offers_request() {
        assert_eq!(
            mic_tcc_action(TccStatus::NotDetermined, false),
            Some(TccAction::Request)
        );
    }

    #[test]
    fn mic_denied_opens_settings() {
        assert_eq!(
            mic_tcc_action(TccStatus::Denied, false),
            Some(TccAction::OpenSettings)
        );
    }

    #[test]
    fn mic_granted_offers_no_action() {
        assert_eq!(mic_tcc_action(TccStatus::Granted, true), None);
    }

    #[test]
    fn mic_granted_but_forced_missing_still_has_a_control() {
        // The CCK_HEALTH_FORCE_WARN review flag makes a granted row read as missing;
        // the amber row must still carry a working control.
        assert_eq!(
            mic_tcc_action(TccStatus::Granted, false),
            Some(TccAction::OpenSettings)
        );
    }
}
