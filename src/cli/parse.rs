//! The command line: ONE pure parser, and the only flag-spelling table left (DRAGON-695).
//!
//! ## What this replaced, and why the old shape had to go
//!
//! Until DRAGON-695 the CLI was a flat bag of flags parsed inline in `main`, by scanning argv
//! for each spelling in turn (`args.iter().any(|a| a == "--region")`, twenty-odd times over).
//! It had three properties that were each a bug waiting to be filed:
//!
//! 1. **Unknown arguments FELL THROUGH.** Nothing ever said "I do not know that word". A typo,
//!    a flag from a newer build, a stray shell expansion: every one of them reached the end of
//!    `main` and opened a capture overlay. That is the worst possible answer to "I do not
//!    understand you", because it puts a fullscreen grab on the user's screen.
//! 2. **"Is this a bare launch?" was answered by THREE near-duplicate lists**, one per
//!    platform, each enumerating every flag that is NOT bare. Every feature that added a
//!    launch flag had to remember all three. DRAGON-586 and DRAGON-680 are both bugs from
//!    exactly that: the colour picker and then the palette viewer were missing from the lists,
//!    so with the tray on they read as bare, took the resident branch, found the daemon lock
//!    held, signalled "capture now" and exited. The user asked for a colour and got a region
//!    overlay.
//! 3. **A bare GUI launch meant "capture NOW"**, which is what made 2 damaging rather than
//!    merely untidy, and which is also the login-time phantom overlay this ticket fixes: a
//!    login item starts the binary bare, the daemon lock is already held by the tray that
//!    started a moment earlier, and the loser of that race asked the winner for a capture.
//!    Nobody pressed anything.
//!
//! So the grammar is now `cosmic-capture-kit <command> [options]`, every old public spelling
//! is gone (a HARD cutover, no aliases), and this module is the single table. `main` does
//! three things: it lets the internal plumbing flags through, it calls [`parse`], and it
//! dispatches. There is no fall-through: an argument we do not recognise is a usage error on
//! stderr with exit code 2, and NOTHING unknown can reach a surface.
//!
//! ## Pure, so the whole grammar is a unit test
//!
//! [`parse`] takes an argv and returns a [`Plan`] or a [`Usage`]. It touches no filesystem, no
//! settings store and no platform API, so the tests at the bottom of this file cover every
//! command, every modifier and every rejection on all three platforms. What is NOT decided
//! here is anything that needs the world: whether the `editor` file exists, whether a
//! recording is running, whether this build carries the control transport. Those stay in
//! `main` and in [`crate::cli::recording`], and they are the reason a [`Plan`] can still fail
//! after parsing cleanly.
//!
//! ## Exit codes
//!
//! * `0`: the command did what it said.
//! * `1`: we understood the command and it could not be carried out (no recording is running,
//!   no scanner is open, the file does not exist, this platform does not carry it).
//! * `2`: we did not understand the command line at all. Every [`Usage`] this module returns
//!   carries `2` except where a caller overrides it.

use std::path::PathBuf;

use crate::app::{ImmediateCapture, Kind, Mode};
use crate::daemon_ipc::Command;
use crate::recording_ui::AudioArmState;

/// The whole user-facing help, printed by `--help` / `-h` and by a bare launch from a
/// terminal. The owner's layout, reproduced exactly: the hidden `debug` namespace and the
/// internal plumbing flags are deliberately absent, and a test below pins that.
pub const HELP: &str = "\
cosmic-capture-kit <command> [options]

  scan                          Scan the screen for QR codes / text (region select)
      --action <refresh>        retake the screen snapshot in the scanner already open

  color                         Color tools
      --picker                  pick a color from the screen (magnifier overlay)
      --viewer                  open the palette viewer window
      --timer <0-99>            countdown before the pick (--picker only)

  capture                       Take a screenshot
      --region                  select a region
      --window                  select a window
      --monitor                 select a monitor
      --active-window           capture the active window immediately, no picker
      --active-monitor          capture the monitor under the cursor immediately
      --timer <0-99>            countdown before the capture (this launch only)
      --skip-editor             save, copy and notify instead of opening the editor

  record                        Start a screen recording, or control the one running
    starting one:
      --region                  select a region
      --window                  select a window
      --monitor                 select a monitor
      --sound <both|mic|system|none>   arm audio for this launch only
      --timer <0-99>            countdown before recording starts (this launch only)
      --skip-editor             save, copy and notify instead of opening the editor
    controlling the one running (no --region/--window/--monitor):
      --action <pause|save|cancel>     pause/resume, finish and save, or discard
      --tool <pointer|marker|magic-marker|erase>   live annotation tool; erase clears marks
      --sound <both|mic|system|none>   set audio channels on the live recording

  editor <file>                 Open an image or video in the editor
      --overlay                 fullscreen overlay instead of a window

  timer <0-99>                  Set the default countdown for every capture and record
                                (0 clears it; bare `timer` prints the current value)

  sound <both|mic|system|none>  Set the default audio channels for every recording
                                (bare `sound` prints the current value)

  settings                      Open the settings window
  permissions                   Permission checker (macOS only)
  tray                          Run the tray / menu bar resident

  --version, -V                 Print the version
  --help, -h                    Show this help";

/// The HIDDEN spelling of the all-in-one capture picker (DRAGON-695).
///
/// The picker itself is not going anywhere: it is what the PrintScreen / F13 hotkey and the
/// tray's Capture rows open. What it no longer has is a PUBLIC spelling, because "open the
/// picker" is a launcher's word and never something a user types at a shell (they would type
/// `capture --region`). It lives in the same bucket as `--preview-handoff`: recognised, never
/// advertised, and only ever written by our own spawn tables.
///
/// Its old spelling, `--all-in-one`, was public and appeared in the help.
pub const INTERNAL_ALL_IN_ONE: &str = "--internal-all-in-one";

/// A command line we could not act on: what to print, and what to exit with.
///
/// Not a bare `String` (the house error type) because the exit CODE is part of the answer and
/// a caller must not have to re-derive it from the wording. `2` means "the command line was
/// wrong"; `1` means "the command was fine and the world said no".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    /// One line for stderr, written for a human at a terminal. Never carries a path.
    pub message: String,
    /// The process exit code.
    pub code: i32,
}

impl Usage {
    /// A command line we did not understand: exit 2.
    fn bad(message: impl Into<String>) -> Self {
        Usage { message: message.into(), code: 2 }
    }
}

/// Which GUI a [`Plan::Launch`] opens. The capture-shaped arms carry no data; the
/// distinguishing fields live on [`Launch`] beside them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum What {
    /// `capture ...`: a screenshot.
    #[default]
    Screenshot,
    /// `record --region|--window|--monitor`: start a recording.
    Recording,
    /// `scan`: the QR / OCR scanner, which is region work by construction.
    Scanner,
    /// `color --picker`: the magnifier overlay that samples one pixel.
    ColorPicker,
    /// `color --viewer`: the picker's result window on its own, no overlay and no pick.
    PaletteViewer,
    /// `settings`.
    Settings,
    /// `permissions` (macOS only; every other platform refuses it in `main`).
    Permissions,
    /// `editor <file> [--overlay]`. The path is NOT checked here; `main` does that, because
    /// asking the filesystem is exactly what a pure parser may not do.
    Editor { file: PathBuf, overlay: bool },
}

/// A launch that ends in `app::run`, described in portable terms.
///
/// Deliberately NOT `app::Startup`: that struct carries per-platform fields and a preview
/// handoff wire object, neither of which a parser has any business building, and it has no
/// `PartialEq` for a test to assert against. `main` maps this onto it in one place.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Launch {
    /// Which surface this launch opens.
    pub what: What,
    /// The interactive picker mode, when there is one. `None` on a capture whose target is
    /// resolved without a picker, and on every non-capture launch.
    pub mode: Option<Mode>,
    /// A picker-free capture: the active window, or the monitor under the cursor.
    pub immediate: Option<ImmediateCapture>,
    /// `--timer <0-99>`: an EXACT countdown for this launch only. Never written back.
    pub timer: Option<u64>,
    /// `--skip-editor`: save, copy and notify instead of opening the editor.
    pub skip_editor: bool,
    /// `--sound <channels>`: arm exactly these channels for this launch only. Never written
    /// back; only a user's own later toggle persists.
    pub sound: Option<AudioArmState>,
}

impl Launch {
    /// The capture KIND this launch runs as, or `None` for a launch that captures nothing.
    /// One place, so `main` never re-derives it from [`Self::what`].
    pub fn kind(&self) -> Option<Kind> {
        match self.what {
            What::Screenshot => Some(Kind::Image),
            What::Recording => Some(Kind::Video),
            What::Scanner => Some(Kind::Scanner),
            _ => None,
        }
    }

    /// Whether this launch puts a capture-shaped surface on screen, which is what the macOS
    /// startup guard is armed for (DRAGON-413) and what the debug log tags `capture`.
    ///
    /// The colour picker counts: it mints the same per-output overlays a capture does. The
    /// palette viewer does NOT, for the same reason `settings` does not: it opens one ordinary
    /// window, and arming a 90 second self-exit budget against a window the user is reading
    /// would close it under them.
    pub fn opens_overlays(&self) -> bool {
        matches!(
            self.what,
            What::Screenshot | What::Recording | What::Scanner | What::ColorPicker
        )
    }
}

/// The hidden `debug` namespace: the harnesses and one-shot tools that are ours, not the
/// user's. Never in [`HELP`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugPlan {
    /// `debug test <name> [args]`. An empty name prints the harness's own usage.
    Test { name: String, args: Vec<String> },
    /// `debug inspect <file>`: print a capture's embedded metadata.
    Inspect(PathBuf),
    /// `debug make-sync-clip [path]`: write the A/V-sync reference clip.
    MakeSyncClip(Option<String>),
    /// `debug calibrate-sync <file> [--apply]`: measure a recording of that clip.
    CalibrateSync { file: Option<PathBuf>, apply: bool },
    /// `debug cloud-upload <file> --account <id> [--auto-share]`: the detached upload runner,
    /// reachable by hand. The editor's own spawns keep the internal `--cloud-upload` argv
    /// (`share::reexec`), which `main` reads before this parser ever runs.
    CloudUpload { file: PathBuf, account: String, auto_share: bool },
}

/// What a command line asks this process to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// `--help` / `-h`, and a bare launch from a terminal.
    Help,
    /// `--version` / `-V`.
    Version,
    /// No arguments at all. `main` decides what that means, because the answer depends on
    /// whether there is a terminal attached and on the persisted `resident` setting, neither
    /// of which a pure parser may read.
    Bare,
    /// `tray`: run the resident, whatever the `resident` setting says.
    Tray,
    /// `timer [0-99]`: write the persisted default countdown, or (`None`) print it.
    SetTimer(Option<u64>),
    /// `sound [both|mic|system|none]`: write the persisted default arms, or (`None`) print
    /// them.
    SetSound(Option<AudioArmState>),
    /// `scan --action refresh`: make the scanner already open re-read the screen.
    ScanRefresh,
    /// `record` with control flags only: drive the recording already running. In APPLY order
    /// (sound, then tool, then action), which is the order the help lists them in and the only
    /// one that makes a combined invocation predictable: setting the arms before finishing is
    /// meaningful, finishing before setting them is not.
    RecordControl(Vec<Command>),
    /// The hidden `debug` namespace.
    Debug(DebugPlan),
    /// A GUI launch.
    Launch(Launch),
}

/// **Pure**, unit-tested: what this argv asks for.
///
/// `argv[0]` is skipped: the program's own path is not an argument, and on macOS it is an
/// absolute path inside a bundle that could contain anything.
///
/// `--help` and `--version` are answered from ANYWHERE in the line, before the command word is
/// even looked at, because that is what a user expects from `<command> --help` and because
/// neither can be wrong. Help wins over version when both are present.
pub fn parse<S: AsRef<str>>(args: &[S]) -> Result<Plan, Usage> {
    let rest: Vec<&str> = args.iter().skip(1).map(AsRef::as_ref).collect();
    if rest.iter().any(|a| *a == "--help" || *a == "-h") {
        return Ok(Plan::Help);
    }
    if rest.iter().any(|a| *a == "--version" || *a == "-V") {
        return Ok(Plan::Version);
    }
    let Some((command, opts)) = rest.split_first() else {
        return Ok(Plan::Bare);
    };
    match *command {
        "scan" => parse_scan(opts),
        "color" => parse_color(opts),
        "capture" => parse_capture(opts),
        "record" => parse_record(opts),
        "editor" => parse_editor(opts),
        "timer" => parse_timer(opts),
        "sound" => parse_sound(opts),
        "settings" => {
            no_extra("settings", opts)?;
            Ok(Plan::Launch(Launch { what: What::Settings, ..Launch::default() }))
        }
        "permissions" => {
            no_extra("permissions", opts)?;
            Ok(Plan::Launch(Launch { what: What::Permissions, ..Launch::default() }))
        }
        "tray" => {
            no_extra("tray", opts)?;
            Ok(Plan::Tray)
        }
        // The PRE-DRAGON-695 spelling of `tray`, accepted for the upgrade window and absent
        // from the help. It is not a courtesy: the XDG autostart `.desktop`, the Windows HKCU
        // `Run` value and the macOS login item all launch the binary with the word they were
        // registered with, and installing a new build does not rewrite them. Refusing it would
        // mean the first login after an upgrade exits 2 and the user's tray is simply gone.
        // See `instance::LEGACY_RESIDENT_ARG`.
        w if w == crate::instance::LEGACY_RESIDENT_ARG => {
            no_extra(crate::instance::LEGACY_RESIDENT_ARG, opts)?;
            Ok(Plan::Tray)
        }
        "debug" => parse_debug(opts),
        // The hidden picker spelling. It takes the same two modifiers `capture` does, because
        // the daemon's "(no editor)" hotkey twins are exactly this plus `--skip-editor`.
        w if w == INTERNAL_ALL_IN_ONE => {
            let mut launch = Launch { what: What::Screenshot, ..Launch::default() };
            let mut it = Walk::new(INTERNAL_ALL_IN_ONE, opts);
            while let Some(arg) = it.next_arg() {
                match arg {
                    "--timer" => launch.timer = Some(it.timer_value()?),
                    "--skip-editor" => launch.skip_editor = true,
                    other => return Err(it.unknown(other)),
                }
            }
            Ok(Plan::Launch(launch))
        }
        other => Err(Usage::bad(format!(
            "cosmic-capture-kit: `{other}` is not a command. Run `cosmic-capture-kit --help` \
             for the list."
        ))),
    }
}

/// A left-to-right walk over one command's options, carrying the command name so every
/// rejection can say which command rejected it.
///
/// Not an iterator adaptor: several options take a VALUE, and consuming that value is the
/// walk's job rather than each call site's, so the "a flag's value is never mistaken for the
/// next flag" rule is written once.
struct Walk<'a> {
    command: &'a str,
    opts: &'a [&'a str],
    at: usize,
}

impl<'a> Walk<'a> {
    fn new(command: &'a str, opts: &'a [&'a str]) -> Self {
        Walk { command, opts, at: 0 }
    }

    /// The next option word, or `None` at the end.
    fn next_arg(&mut self) -> Option<&'a str> {
        let out = self.opts.get(self.at).copied();
        self.at += 1;
        out
    }

    /// The VALUE of the option just returned, or `None` when the line ends or the next word is
    /// another flag. A missing value is never silently defaulted: the caller turns `None` into
    /// a usage error naming what it expected.
    fn value(&mut self) -> Option<&'a str> {
        match self.opts.get(self.at) {
            Some(v) if !v.starts_with('-') => {
                self.at += 1;
                Some(*v)
            }
            _ => None,
        }
    }

    /// `--timer <0-99>`, the same rule wherever it appears.
    fn timer_value(&mut self) -> Result<u64, Usage> {
        let cmd = self.command;
        let raw = self.value();
        parse_timer_value(cmd, "--timer", raw)
    }

    /// `--sound <both|mic|system|none>`, the same rule wherever it appears.
    fn sound_value(&mut self) -> Result<AudioArmState, Usage> {
        let cmd = self.command;
        let raw = self.value();
        parse_sound_value(cmd, "--sound", raw)
    }

    /// An option this command does not have.
    fn unknown(&self, arg: &str) -> Usage {
        let cmd = self.command;
        Usage::bad(format!(
            "{cmd}: `{arg}` is not an option of this command. Run `cosmic-capture-kit --help`."
        ))
    }
}

/// The shared `--timer` value rule: a plain number from 0 to 99. Anything else is rejected
/// rather than clamped, because a clamp would silently give the user a countdown they did not
/// ask for and a typo is far more likely than a genuine wish for 900 seconds.
fn parse_timer_value(command: &str, flag: &str, raw: Option<&str>) -> Result<u64, Usage> {
    match raw.and_then(|v| v.parse::<u64>().ok()) {
        Some(secs) if secs <= 99 => Ok(secs),
        _ => Err(Usage::bad(format!(
            "{command}: {flag} expects a whole number of seconds from 0 to 99 (got {})",
            raw.unwrap_or("nothing")
        ))),
    }
}

/// The shared `--sound` / `sound` value rule. A bad value REJECTS rather than guessing: unlike
/// a countdown, silently keeping the persisted arms could record a microphone the user just
/// asked to silence.
fn parse_sound_value(command: &str, flag: &str, raw: Option<&str>) -> Result<AudioArmState, Usage> {
    match raw.and_then(AudioArmState::parse_flag) {
        Some(state) => Ok(state),
        None => Err(Usage::bad(format!(
            "{command}: {flag} expects one of both|mic|system|none (got {})",
            raw.unwrap_or("nothing")
        ))),
    }
}

/// A command that takes no options at all.
fn no_extra(command: &str, opts: &[&str]) -> Result<(), Usage> {
    match opts.first() {
        None => Ok(()),
        Some(extra) => Err(Usage::bad(format!(
            "{command}: takes no options, but got `{extra}`. Run `cosmic-capture-kit --help`."
        ))),
    }
}

/// `scan` / `scan --action refresh`.
fn parse_scan(opts: &[&str]) -> Result<Plan, Usage> {
    let mut refresh = false;
    let mut it = Walk::new("scan", opts);
    while let Some(arg) = it.next_arg() {
        match arg {
            "--action" => match it.value() {
                Some("refresh") => refresh = true,
                other => {
                    return Err(Usage::bad(format!(
                        "scan: --action expects refresh (got {})",
                        other.unwrap_or("nothing")
                    )));
                }
            },
            other => return Err(it.unknown(other)),
        }
    }
    if refresh {
        return Ok(Plan::ScanRefresh);
    }
    // The scanner is region work by construction, and `App::init` pins Region for the
    // Scanner kind, so the mode is deliberately left unset here rather than stated twice.
    Ok(Plan::Launch(Launch { what: What::Scanner, ..Launch::default() }))
}

/// `color --picker [--timer n]` / `color --viewer`.
fn parse_color(opts: &[&str]) -> Result<Plan, Usage> {
    let mut picker = false;
    let mut viewer = false;
    let mut timer = None;
    let mut it = Walk::new("color", opts);
    while let Some(arg) = it.next_arg() {
        match arg {
            "--picker" => picker = true,
            "--viewer" => viewer = true,
            // The picker counts down before it samples (DRAGON-663 wired the countdown;
            // DRAGON-699 gives it this exact one-shot spelling). Same value rule, same
            // "this launch only, never written back" contract as `capture --timer`.
            "--timer" => timer = Some(it.timer_value()?),
            other => return Err(it.unknown(other)),
        }
    }
    match (picker, viewer) {
        (true, false) => {
            Ok(Plan::Launch(Launch { what: What::ColorPicker, timer, ..Launch::default() }))
        }
        // The viewer opens a window and samples nothing, so a countdown would count down
        // to nothing. Refused by name rather than ignored: silently dropping a flag the
        // user typed is the fall-through this parser exists to prevent.
        (false, true) if timer.is_some() => {
            Err(Usage::bad("color: --timer applies to --picker only"))
        }
        (false, true) => {
            Ok(Plan::Launch(Launch { what: What::PaletteViewer, ..Launch::default() }))
        }
        (false, false) => Err(Usage::bad(
            "color: expected --picker or --viewer. Run `cosmic-capture-kit --help`.",
        )),
        (true, true) => Err(Usage::bad("color: --picker and --viewer cannot be combined")),
    }
}

/// Which target flag a capture / record line named, and where. The position is kept so a line
/// naming two targets can say which two.
fn target_flag(arg: &str) -> Option<(Option<Mode>, Option<ImmediateCapture>)> {
    match arg {
        "--region" => Some((Some(Mode::Region), None)),
        "--window" => Some((Some(Mode::Window), None)),
        "--monitor" => Some((Some(Mode::Monitor), None)),
        "--active-window" => Some((None, Some(ImmediateCapture::ActiveWindow))),
        "--active-monitor" => Some((None, Some(ImmediateCapture::ActiveMonitor))),
        _ => None,
    }
}

/// `capture --<target> [--timer n] [--skip-editor]`.
fn parse_capture(opts: &[&str]) -> Result<Plan, Usage> {
    let mut launch = Launch { what: What::Screenshot, ..Launch::default() };
    let mut named: Vec<&str> = Vec::new();
    let mut it = Walk::new("capture", opts);
    while let Some(arg) = it.next_arg() {
        if let Some((mode, immediate)) = target_flag(arg) {
            named.push(arg);
            launch.mode = mode;
            launch.immediate = immediate;
            continue;
        }
        match arg {
            "--timer" => launch.timer = Some(it.timer_value()?),
            "--skip-editor" => launch.skip_editor = true,
            other => return Err(it.unknown(other)),
        }
    }
    match named.len() {
        1 => Ok(Plan::Launch(launch)),
        0 => Err(Usage::bad(
            "capture: expected exactly one of --region, --window, --monitor, --active-window, \
             --active-monitor",
        )),
        _ => Err(Usage::bad(format!(
            "capture: expected exactly one target, but got {}",
            named.join(" ")
        ))),
    }
}

/// `record`, both halves: a target flag starts one, control flags drive the one running.
///
/// The two halves are told apart by the TARGET, and nothing else. That is deliberate: "start a
/// recording of this thing" and "do this to the recording already running" are different
/// enough that mixing them can only be a mistake, so a line carrying both is refused rather
/// than resolved in some order.
fn parse_record(opts: &[&str]) -> Result<Plan, Usage> {
    let mut launch = Launch { what: What::Recording, ..Launch::default() };
    let mut named: Vec<&str> = Vec::new();
    let mut sound: Option<AudioArmState> = None;
    let mut tool: Option<Command> = None;
    let mut action: Option<Command> = None;
    let mut controls: Vec<&str> = Vec::new();
    let mut it = Walk::new("record", opts);
    while let Some(arg) = it.next_arg() {
        if let Some((mode, immediate)) = target_flag(arg) {
            // The picker-free targets are screenshot-only: there is no "record the active
            // window with no picker" launch, and inventing one here would be a feature.
            if immediate.is_some() {
                return Err(Usage::bad(format!(
                    "record: {arg} is a capture target, not a recording one (use --region, \
                     --window or --monitor)"
                )));
            }
            named.push(arg);
            launch.mode = mode;
            continue;
        }
        match arg {
            "--sound" => sound = Some(it.sound_value()?),
            "--timer" => launch.timer = Some(it.timer_value()?),
            "--skip-editor" => launch.skip_editor = true,
            "--action" => {
                controls.push("--action");
                action = Some(match it.value() {
                    Some("pause") => Command::TogglePause,
                    Some("save") => Command::Stop,
                    Some("cancel") => Command::Cancel,
                    other => {
                        return Err(Usage::bad(format!(
                            "record: --action expects one of pause|save|cancel (got {})",
                            other.unwrap_or("nothing")
                        )));
                    }
                });
            }
            "--tool" => {
                controls.push("--tool");
                tool = Some(match it.value() {
                    Some("pointer") => Command::InkTool(crate::ink::InkTool::Pointer),
                    Some("marker") => Command::InkTool(crate::ink::InkTool::Marker),
                    Some("magic-marker") => Command::InkTool(crate::ink::InkTool::MagicMarker),
                    Some("erase") => Command::InkClear,
                    other => {
                        return Err(Usage::bad(format!(
                            "record: --tool expects one of pointer|marker|magic-marker|erase \
                             (got {})",
                            other.unwrap_or("nothing")
                        )));
                    }
                });
            }
            other => return Err(it.unknown(other)),
        }
    }
    if named.len() > 1 {
        return Err(Usage::bad(format!(
            "record: expected exactly one target, but got {}",
            named.join(" ")
        )));
    }
    if named.is_empty() {
        // The control half. `--timer` and `--skip-editor` describe a recording that has not
        // started yet, so they are meaningless here and saying so is kinder than ignoring
        // them.
        if launch.timer.is_some() || launch.skip_editor {
            return Err(Usage::bad(
                "record: --timer and --skip-editor apply to STARTING a recording, so they need \
                 a target (--region, --window or --monitor)",
            ));
        }
        if sound.is_none() && controls.is_empty() {
            return Err(Usage::bad(
                "record: expected a target (--region, --window, --monitor) to start a \
                 recording, or a control (--action, --tool, --sound) for the one running",
            ));
        }
        // Apply order: sound, tool, action. See `Plan::RecordControl`.
        let mut cmds: Vec<Command> = Vec::new();
        if let Some(s) = sound {
            cmds.push(Command::SetAudioArms(s));
        }
        cmds.extend(tool);
        cmds.extend(action);
        return Ok(Plan::RecordControl(cmds));
    }
    if !controls.is_empty() {
        return Err(Usage::bad(format!(
            "record: {} controls the recording already running, so it cannot be combined with \
             {}",
            controls.join(" and "),
            named.join(" ")
        )));
    }
    launch.sound = sound;
    Ok(Plan::Launch(launch))
}

/// `editor <file> [--overlay]`.
fn parse_editor(opts: &[&str]) -> Result<Plan, Usage> {
    let mut file: Option<&str> = None;
    let mut overlay = false;
    let mut it = Walk::new("editor", opts);
    while let Some(arg) = it.next_arg() {
        match arg {
            "--overlay" => overlay = true,
            other if other.starts_with('-') => return Err(it.unknown(other)),
            other if file.is_none() => file = Some(other),
            other => {
                return Err(Usage::bad(format!(
                    "editor: opens one file at a time, so `{other}` is one file too many"
                )));
            }
        }
    }
    match file {
        Some(f) => Ok(Plan::Launch(Launch {
            what: What::Editor { file: PathBuf::from(f), overlay },
            ..Launch::default()
        })),
        None => Err(Usage::bad("editor: expected a file to open")),
    }
}

/// `timer` / `timer <0-99>`.
fn parse_timer(opts: &[&str]) -> Result<Plan, Usage> {
    match opts {
        [] => Ok(Plan::SetTimer(None)),
        [value] => Ok(Plan::SetTimer(Some(parse_timer_value("timer", "timer", Some(value))?))),
        [_, extra, ..] => Err(Usage::bad(format!(
            "timer: takes one value, but got `{extra}` as well"
        ))),
    }
}

/// `sound` / `sound <both|mic|system|none>`.
fn parse_sound(opts: &[&str]) -> Result<Plan, Usage> {
    match opts {
        [] => Ok(Plan::SetSound(None)),
        [value] => Ok(Plan::SetSound(Some(parse_sound_value("sound", "sound", Some(value))?))),
        [_, extra, ..] => Err(Usage::bad(format!(
            "sound: takes one value, but got `{extra}` as well"
        ))),
    }
}

/// The hidden `debug` namespace.
fn parse_debug(opts: &[&str]) -> Result<Plan, Usage> {
    let Some((verb, rest)) = opts.split_first() else {
        return Err(Usage::bad(
            "debug: expected one of test, inspect, make-sync-clip, calibrate-sync, cloud-upload",
        ));
    };
    match *verb {
        // Everything after the name belongs to the harness, flags included, so this one does
        // NOT walk the options: `debug test encode --loops 3` must reach `run_test` whole.
        "test" => Ok(Plan::Debug(DebugPlan::Test {
            name: rest.first().copied().unwrap_or("").to_string(),
            args: rest.iter().skip(1).map(|s| (*s).to_string()).collect(),
        })),
        "inspect" => match rest {
            [file] => Ok(Plan::Debug(DebugPlan::Inspect(PathBuf::from(*file)))),
            [] => Err(Usage::bad("debug inspect: expected a file")),
            [_, extra, ..] => Err(Usage::bad(format!(
                "debug inspect: takes one file, but got `{extra}` as well"
            ))),
        },
        "make-sync-clip" => match rest {
            [] => Ok(Plan::Debug(DebugPlan::MakeSyncClip(None))),
            [path] => Ok(Plan::Debug(DebugPlan::MakeSyncClip(Some((*path).to_string())))),
            [_, extra, ..] => Err(Usage::bad(format!(
                "debug make-sync-clip: takes one path, but got `{extra}` as well"
            ))),
        },
        "calibrate-sync" => {
            let mut file: Option<PathBuf> = None;
            let mut apply = false;
            let mut it = Walk::new("debug calibrate-sync", rest);
            while let Some(arg) = it.next_arg() {
                match arg {
                    "--apply" => apply = true,
                    other if other.starts_with('-') => return Err(it.unknown(other)),
                    other if file.is_none() => file = Some(PathBuf::from(other)),
                    other => {
                        return Err(Usage::bad(format!(
                            "debug calibrate-sync: takes one file, but got `{other}` as well"
                        )));
                    }
                }
            }
            Ok(Plan::Debug(DebugPlan::CalibrateSync { file, apply }))
        }
        "cloud-upload" => {
            let mut file: Option<PathBuf> = None;
            let mut account: Option<String> = None;
            let mut auto_share = false;
            let mut it = Walk::new("debug cloud-upload", rest);
            while let Some(arg) = it.next_arg() {
                match arg {
                    "--account" => match it.value() {
                        Some(id) => account = Some(id.to_string()),
                        None => {
                            return Err(Usage::bad(
                                "debug cloud-upload: --account expects a connected account id",
                            ));
                        }
                    },
                    "--auto-share" => auto_share = true,
                    other if other.starts_with('-') => return Err(it.unknown(other)),
                    other if file.is_none() => file = Some(PathBuf::from(other)),
                    other => {
                        return Err(Usage::bad(format!(
                            "debug cloud-upload: uploads one file, but got `{other}` as well"
                        )));
                    }
                }
            }
            match (file, account) {
                (Some(file), Some(account)) => {
                    Ok(Plan::Debug(DebugPlan::CloudUpload { file, account, auto_share }))
                }
                (None, _) => Err(Usage::bad("debug cloud-upload: expected a file to upload")),
                (_, None) => Err(Usage::bad("debug cloud-upload: expected --account <id>")),
            }
        }
        other => Err(Usage::bad(format!(
            "debug: `{other}` is not a debug command (test, inspect, make-sync-clip, \
             calibrate-sync, cloud-upload)"
        ))),
    }
}

#[cfg(test)]
fn argv(rest: &[&str]) -> Vec<String> {
    std::iter::once("cosmic-capture-kit")
        .chain(rest.iter().copied())
        .map(String::from)
        .collect()
}

#[cfg(test)]
fn plan(rest: &[&str]) -> Plan {
    parse(&argv(rest)).unwrap_or_else(|e| panic!("{rest:?} should parse: {}", e.message))
}

#[cfg(test)]
fn refused(rest: &[&str]) -> Usage {
    parse(&argv(rest)).err().unwrap_or_else(|| panic!("{rest:?} should have been refused"))
}

/// The rule that made this rework worth doing: nothing we do not understand may ever reach a
/// surface.
#[cfg(test)]
mod nothing_unknown_falls_through_tests {
    use super::*;

    #[test]
    fn an_unknown_command_is_a_usage_error() {
        for word in ["shoot", "Capture", "CAPTURE", "--region", "-r", "screenshot"] {
            let e = refused(&[word]);
            assert_eq!(e.code, 2, "{word}");
            assert!(e.message.contains(word), "{word}: {}", e.message);
        }
    }

    /// Every OLD public spelling is dead. This is the hard-cutover pin: if any of these ever
    /// parses again, the grammar has grown a second vocabulary.
    #[test]
    fn every_retired_spelling_is_refused() {
        for old in [
            "--region",
            "--window",
            "--monitor",
            "--all-in-one",
            "--active-window",
            "--active-monitor",
            "--no-editor",
            "--image",
            "--video",
            "--scan",
            "--scanner",
            "--countdown",
            "--audio",
            "--color-picker",
            "--palette-viewer",
            "--settings",
            "--permissions",
            "--preview",
            "--pause-recording",
            "--finish-recording",
            "--cancel-recording",
            "--toggle-mic",
            "--toggle-system-audio",
            "--inspect",
            "--make-sync-clip",
            "--calibrate-sync",
        ] {
            assert_eq!(refused(&[old]).code, 2, "{old} must not parse any more");
        }
        // `--test` is the one that could plausibly still be typed by us out of habit, and it
        // is gone too: the harness answers to `debug test` now.
        assert_eq!(refused(&["--test", "help"]).code, 2);
    }

    /// An unknown OPTION is refused by the command that does not have it, rather than ignored.
    #[test]
    fn an_unknown_option_names_its_command() {
        let e = refused(&["capture", "--region", "--sound", "mic"]);
        assert_eq!(e.code, 2);
        assert!(e.message.starts_with("capture:"), "{}", e.message);
        assert!(e.message.contains("--sound"), "{}", e.message);
    }
}

#[cfg(test)]
mod help_and_version_tests {
    use super::*;

    #[test]
    fn help_is_answered_from_anywhere() {
        for line in [
            vec!["--help"],
            vec!["-h"],
            vec!["capture", "--help"],
            vec!["record", "--region", "-h"],
            vec!["nonsense", "--help"],
        ] {
            assert_eq!(plan(&line), Plan::Help, "{line:?}");
        }
    }

    #[test]
    fn version_is_answered_from_anywhere_but_help_wins() {
        assert_eq!(plan(&["--version"]), Plan::Version);
        assert_eq!(plan(&["-V"]), Plan::Version);
        assert_eq!(plan(&["timer", "--version"]), Plan::Version);
        assert_eq!(plan(&["--version", "--help"]), Plan::Help);
    }

    #[test]
    fn a_bare_argv_is_bare() {
        assert_eq!(plan(&[]), Plan::Bare);
        assert_eq!(parse::<String>(&[]).unwrap(), Plan::Bare);
    }

    /// The help lists every public command and NOTHING private. The `debug` namespace and the
    /// internal spellings are ours, and a user who reads them starts filing tickets about
    /// them.
    #[test]
    fn the_help_lists_the_public_grammar_and_nothing_else() {
        for command in [
            "scan",
            "color",
            "capture",
            "record",
            "editor",
            "timer",
            "sound",
            "settings",
            "permissions",
            "tray",
        ] {
            assert!(HELP.contains(command), "the help must name `{command}`");
        }
        assert!(!HELP.contains("debug"), "the debug namespace is hidden");
        assert!(!HELP.contains(INTERNAL_ALL_IN_ONE), "the internal picker spelling is hidden");
        assert!(!HELP.contains("--preview-handoff"), "internal plumbing stays out of the help");
        // The house rule: no em-dashes and no en-dashes in anything the user reads.
        assert!(!HELP.contains('\u{2014}') && !HELP.contains('\u{2013}'), "no dashes in copy");
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    fn launch(rest: &[&str]) -> Launch {
        match plan(rest) {
            Plan::Launch(l) => l,
            other => panic!("{rest:?} should be a launch, got {other:?}"),
        }
    }

    #[test]
    fn each_target_reaches_its_own_mode() {
        assert_eq!(launch(&["capture", "--region"]).mode, Some(Mode::Region));
        assert_eq!(launch(&["capture", "--window"]).mode, Some(Mode::Window));
        assert_eq!(launch(&["capture", "--monitor"]).mode, Some(Mode::Monitor));
        assert_eq!(
            launch(&["capture", "--active-window"]).immediate,
            Some(ImmediateCapture::ActiveWindow)
        );
        assert_eq!(
            launch(&["capture", "--active-monitor"]).immediate,
            Some(ImmediateCapture::ActiveMonitor)
        );
        for line in [
            vec!["capture", "--region"],
            vec!["capture", "--active-window"],
            vec!["capture", "--active-monitor"],
        ] {
            assert_eq!(launch(&line).kind(), Some(Kind::Image), "{line:?}");
        }
    }

    /// Exactly one target, and both failures are usage errors rather than a guess.
    #[test]
    fn a_capture_needs_exactly_one_target() {
        assert_eq!(refused(&["capture"]).code, 2);
        assert_eq!(refused(&["capture", "--timer", "3"]).code, 2);
        let e = refused(&["capture", "--region", "--monitor"]);
        assert_eq!(e.code, 2);
        assert!(e.message.contains("--region --monitor"), "{}", e.message);
        assert_eq!(refused(&["capture", "--active-window", "--window"]).code, 2);
    }

    #[test]
    fn the_modifiers_compose_with_every_target() {
        let l = launch(&["capture", "--window", "--timer", "7", "--skip-editor"]);
        assert_eq!(l.mode, Some(Mode::Window));
        assert_eq!(l.timer, Some(7));
        assert!(l.skip_editor);
        // Order does not matter.
        assert_eq!(launch(&["capture", "--skip-editor", "--timer", "0", "--monitor"]).timer, Some(0));
    }

    #[test]
    fn the_timer_range_is_enforced_not_clamped() {
        assert_eq!(launch(&["capture", "--region", "--timer", "99"]).timer, Some(99));
        for bad in ["100", "-1", "3s", "", "seven"] {
            assert_eq!(refused(&["capture", "--region", "--timer", bad]).code, 2, "{bad}");
        }
        // A missing value must not swallow the next flag.
        let e = refused(&["capture", "--region", "--timer", "--skip-editor"]);
        assert_eq!(e.code, 2);
        assert!(e.message.contains("nothing"), "{}", e.message);
    }

    /// The hidden picker spelling takes the same two modifiers, because the daemon's
    /// "(no editor)" hotkey twin is exactly this plus `--skip-editor`.
    #[test]
    fn the_internal_picker_spelling_takes_the_capture_modifiers() {
        let l = launch(&[INTERNAL_ALL_IN_ONE]);
        assert_eq!(l.what, What::Screenshot);
        assert_eq!(l.mode, None, "the picker starts on the persisted mode, not a forced one");
        assert!(!l.skip_editor);
        assert!(launch(&[INTERNAL_ALL_IN_ONE, "--skip-editor"]).skip_editor);
        assert_eq!(launch(&[INTERNAL_ALL_IN_ONE, "--timer", "5"]).timer, Some(5));
        assert_eq!(refused(&[INTERNAL_ALL_IN_ONE, "--region"]).code, 2);
    }
}

#[cfg(test)]
mod record_tests {
    use super::*;

    fn launch(rest: &[&str]) -> Launch {
        match plan(rest) {
            Plan::Launch(l) => l,
            other => panic!("{rest:?} should be a launch, got {other:?}"),
        }
    }

    fn controls(rest: &[&str]) -> Vec<Command> {
        match plan(rest) {
            Plan::RecordControl(c) => c,
            other => panic!("{rest:?} should be controls, got {other:?}"),
        }
    }

    #[test]
    fn a_target_starts_a_recording() {
        for (flag, mode) in [
            ("--region", Mode::Region),
            ("--window", Mode::Window),
            ("--monitor", Mode::Monitor),
        ] {
            let l = launch(&["record", flag]);
            assert_eq!(l.mode, Some(mode), "{flag}");
            assert_eq!(l.kind(), Some(Kind::Video), "{flag}");
        }
        let l = launch(&["record", "--monitor", "--sound", "system", "--timer", "3", "--skip-editor"]);
        assert_eq!(l.sound, Some(AudioArmState::SystemOnly));
        assert_eq!(l.timer, Some(3));
        assert!(l.skip_editor);
    }

    /// There is no picker-free recording, so those two targets are refused by name rather
    /// than read as "no target" and turned into a control command.
    #[test]
    fn the_picker_free_targets_are_capture_only() {
        for flag in ["--active-window", "--active-monitor"] {
            let e = refused(&["record", flag]);
            assert_eq!(e.code, 2, "{flag}");
            assert!(e.message.contains(flag), "{}", e.message);
        }
    }

    #[test]
    fn the_controls_map_onto_the_relay_words() {
        assert_eq!(controls(&["record", "--action", "pause"]), vec![Command::TogglePause]);
        assert_eq!(controls(&["record", "--action", "save"]), vec![Command::Stop]);
        assert_eq!(controls(&["record", "--action", "cancel"]), vec![Command::Cancel]);
        assert_eq!(
            controls(&["record", "--tool", "pointer"]),
            vec![Command::InkTool(crate::ink::InkTool::Pointer)]
        );
        assert_eq!(
            controls(&["record", "--tool", "marker"]),
            vec![Command::InkTool(crate::ink::InkTool::Marker)]
        );
        assert_eq!(
            controls(&["record", "--tool", "magic-marker"]),
            vec![Command::InkTool(crate::ink::InkTool::MagicMarker)]
        );
        assert_eq!(controls(&["record", "--tool", "erase"]), vec![Command::InkClear]);
        assert_eq!(
            controls(&["record", "--sound", "mic"]),
            vec![Command::SetAudioArms(AudioArmState::MicrophoneOnly)]
        );
    }

    /// Combined controls apply in ONE order, whatever order they were typed in: sound, then
    /// tool, then action. Finishing the recording last is the only arrangement where the
    /// other two mean anything.
    #[test]
    fn combined_controls_apply_in_a_fixed_order() {
        let expected = vec![
            Command::SetAudioArms(AudioArmState::Both),
            Command::InkTool(crate::ink::InkTool::Marker),
            Command::Stop,
        ];
        assert_eq!(
            controls(&["record", "--sound", "both", "--tool", "marker", "--action", "save"]),
            expected
        );
        assert_eq!(
            controls(&["record", "--action", "save", "--tool", "marker", "--sound", "both"]),
            expected
        );
    }

    #[test]
    fn starting_and_controlling_cannot_be_combined() {
        for line in [
            vec!["record", "--region", "--action", "pause"],
            vec!["record", "--monitor", "--tool", "erase"],
            vec!["record", "--window", "--action", "save", "--tool", "marker"],
        ] {
            let e = parse(&argv(&line)).expect_err(&format!("{line:?}"));
            assert_eq!(e.code, 2, "{line:?}");
        }
    }

    /// `--sound` is the ONE flag that belongs to both halves, which is exactly why it must not
    /// be what decides which half a line is: the target does.
    #[test]
    fn sound_belongs_to_both_halves_and_decides_neither() {
        assert_eq!(launch(&["record", "--region", "--sound", "none"]).sound, Some(AudioArmState::None));
        assert_eq!(
            controls(&["record", "--sound", "none"]),
            vec![Command::SetAudioArms(AudioArmState::None)]
        );
    }

    #[test]
    fn a_start_only_modifier_needs_a_target() {
        for line in [vec!["record", "--timer", "3"], vec!["record", "--skip-editor"]] {
            let e = parse(&argv(&line)).expect_err(&format!("{line:?}"));
            assert_eq!(e.code, 2, "{line:?}");
            assert!(e.message.contains("target"), "{}", e.message);
        }
    }

    #[test]
    fn a_bare_record_says_what_it_wanted() {
        let e = refused(&["record"]);
        assert_eq!(e.code, 2);
        assert!(e.message.contains("--region"), "{}", e.message);
        assert!(e.message.contains("--action"), "{}", e.message);
    }

    #[test]
    fn bad_control_values_are_refused_by_name() {
        for line in [
            vec!["record", "--action", "stop"],
            vec!["record", "--action"],
            vec!["record", "--tool", "pen"],
            vec!["record", "--tool"],
            vec!["record", "--sound", "sytem"],
            vec!["record", "--sound"],
        ] {
            assert_eq!(parse(&argv(&line)).expect_err(&format!("{line:?}")).code, 2, "{line:?}");
        }
    }
}

#[cfg(test)]
mod scan_and_color_tests {
    use super::*;

    #[test]
    fn a_bare_scan_opens_the_scanner() {
        match plan(&["scan"]) {
            Plan::Launch(l) => {
                assert_eq!(l.what, What::Scanner);
                assert_eq!(l.mode, None, "the Scanner kind pins Region in `App::init`");
                assert_eq!(l.kind(), Some(Kind::Scanner));
                assert!(l.opens_overlays());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_only_scan_action_is_refresh() {
        assert_eq!(plan(&["scan", "--action", "refresh"]), Plan::ScanRefresh);
        for bad in [
            vec!["scan", "--action"],
            vec!["scan", "--action", "reload"],
            vec!["scan", "--refresh"],
            vec!["scan", "--region"],
        ] {
            assert_eq!(parse(&argv(&bad)).expect_err(&format!("{bad:?}")).code, 2, "{bad:?}");
        }
    }

    #[test]
    fn color_takes_exactly_one_tool() {
        match plan(&["color", "--picker"]) {
            Plan::Launch(l) => {
                assert_eq!(l.what, What::ColorPicker);
                assert!(l.opens_overlays(), "the picker mints capture-shaped overlays");
                assert_eq!(l.kind(), None, "it captures nothing");
            }
            other => panic!("{other:?}"),
        }
        match plan(&["color", "--viewer"]) {
            Plan::Launch(l) => {
                assert_eq!(l.what, What::PaletteViewer);
                assert!(!l.opens_overlays(), "the viewer is one ordinary window");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(refused(&["color"]).code, 2);
        assert_eq!(refused(&["color", "--picker", "--viewer"]).code, 2);
        assert_eq!(refused(&["color", "--pick"]).code, 2);
    }

    /// DRAGON-699: the picker takes an exact one-shot countdown, spelled and bounded the
    /// same way `capture --timer` is, and order-free like every other command's flags.
    #[test]
    fn the_picker_takes_a_timer() {
        for line in [vec!["color", "--picker", "--timer", "5"],
                     vec!["color", "--timer", "5", "--picker"]] {
            match plan(&line) {
                Plan::Launch(l) => {
                    assert_eq!(l.what, What::ColorPicker, "{line:?}");
                    assert_eq!(l.timer, Some(5), "{line:?}");
                }
                other => panic!("{line:?}: {other:?}"),
            }
        }
        // `--timer 0` is a real value (suppress the persisted default for this pick).
        match plan(&["color", "--picker", "--timer", "0"]) {
            Plan::Launch(l) => assert_eq!(l.timer, Some(0)),
            other => panic!("{other:?}"),
        }
        // The shared value rule: missing and out-of-range values are refused, not clamped.
        assert_eq!(refused(&["color", "--picker", "--timer"]).code, 2);
        assert_eq!(refused(&["color", "--picker", "--timer", "100"]).code, 2);
    }

    /// The viewer samples nothing, so its countdown flag is refused by name, not dropped.
    #[test]
    fn the_viewer_refuses_a_timer() {
        let e = refused(&["color", "--viewer", "--timer", "5"]);
        assert_eq!(e.code, 2);
        assert!(e.message.contains("--picker only"), "{}", e.message);
    }
}

#[cfg(test)]
mod editor_tests {
    use super::*;

    #[test]
    fn a_file_is_required_and_the_overlay_is_optional() {
        match plan(&["editor", "/tmp/a.png"]) {
            Plan::Launch(l) => {
                assert_eq!(l.what, What::Editor { file: PathBuf::from("/tmp/a.png"), overlay: false });
                assert!(!l.opens_overlays(), "the editor is a window launch");
            }
            other => panic!("{other:?}"),
        }
        match plan(&["editor", "--overlay", "/tmp/a.mp4"]) {
            Plan::Launch(l) => {
                assert_eq!(l.what, What::Editor { file: PathBuf::from("/tmp/a.mp4"), overlay: true });
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(refused(&["editor"]).code, 2);
        assert_eq!(refused(&["editor", "--overlay"]).code, 2);
        assert_eq!(refused(&["editor", "a.png", "b.png"]).code, 2);
    }

    /// A path that begins with a dash would be read as a flag, and that is the right answer:
    /// the alternative is a typo silently opening a file nobody named.
    #[test]
    fn a_dash_led_word_is_a_flag_not_a_file() {
        assert_eq!(refused(&["editor", "--window"]).code, 2);
    }
}

#[cfg(test)]
mod defaults_tests {
    use super::*;

    #[test]
    fn timer_writes_or_prints() {
        assert_eq!(plan(&["timer"]), Plan::SetTimer(None));
        assert_eq!(plan(&["timer", "0"]), Plan::SetTimer(Some(0)));
        assert_eq!(plan(&["timer", "99"]), Plan::SetTimer(Some(99)));
        for bad in ["100", "-3", "3.5", "three", "1e2"] {
            assert_eq!(refused(&["timer", bad]).code, 2, "{bad}");
        }
        assert_eq!(refused(&["timer", "3", "4"]).code, 2);
    }

    #[test]
    fn sound_writes_or_prints() {
        assert_eq!(plan(&["sound"]), Plan::SetSound(None));
        for (word, state) in [
            ("both", AudioArmState::Both),
            ("mic", AudioArmState::MicrophoneOnly),
            ("system", AudioArmState::SystemOnly),
            ("none", AudioArmState::None),
        ] {
            assert_eq!(plan(&["sound", word]), Plan::SetSound(Some(state)), "{word}");
        }
        assert_eq!(refused(&["sound", "microfone"]).code, 2);
        assert_eq!(refused(&["sound", "mic", "system"]).code, 2);
    }
}

#[cfg(test)]
mod window_command_tests {
    use super::*;

    #[test]
    fn the_three_window_commands_take_no_options() {
        match plan(&["settings"]) {
            Plan::Launch(l) => assert_eq!(l.what, What::Settings),
            other => panic!("{other:?}"),
        }
        match plan(&["permissions"]) {
            Plan::Launch(l) => assert_eq!(l.what, What::Permissions),
            other => panic!("{other:?}"),
        }
        assert_eq!(plan(&["tray"]), Plan::Tray);
        for line in [
            vec!["settings", "--region"],
            vec!["permissions", "--check"],
            vec!["tray", "--background"],
        ] {
            assert_eq!(parse(&argv(&line)).expect_err(&format!("{line:?}")).code, 2, "{line:?}");
        }
    }

    /// The tray word is the one every launcher passes, and it is a constant rather than a
    /// literal here so an argv-building launcher and this parser cannot drift.
    #[test]
    fn the_tray_command_is_the_word_the_launchers_pass() {
        assert_eq!(plan(&[crate::instance::TRAY_ARG]), Plan::Tray);
    }

    /// The upgrade window: an autostart entry written by a pre-DRAGON-695 build still says
    /// `resident`, and installing this build does not rewrite it. That word has to keep
    /// raising the tray, or the first login after an upgrade loses it.
    #[test]
    fn the_pre_rename_word_still_raises_the_tray() {
        assert_eq!(plan(&["resident"]), Plan::Tray);
        assert_eq!(plan(&[crate::instance::LEGACY_RESIDENT_ARG]), Plan::Tray);
        // It is a bridge, not a second supported spelling: it is not OFFERED anywhere, and it
        // takes no options either. (The help's `tray` line says the word "resident" in prose,
        // which is the thing the command runs, so the check is on the command column.)
        assert!(!HELP.lines().any(|l| l.trim_start().starts_with("resident")));
        assert_eq!(refused(&["resident", "--now"]).code, 2);
    }
}

#[cfg(test)]
mod debug_namespace_tests {
    use super::*;

    #[test]
    fn the_harness_gets_its_arguments_whole() {
        assert_eq!(
            plan(&["debug", "test", "help"]),
            Plan::Debug(DebugPlan::Test { name: "help".into(), args: vec![] })
        );
        // Flags after the name belong to the harness, not to us.
        assert_eq!(
            plan(&["debug", "test", "encode", "--loops", "3"]),
            Plan::Debug(DebugPlan::Test {
                name: "encode".into(),
                args: vec!["--loops".into(), "3".into()],
            })
        );
        // A bare `debug test` reaches the harness with an empty name, which is what makes it
        // print its own list rather than a usage error from here.
        assert_eq!(
            plan(&["debug", "test"]),
            Plan::Debug(DebugPlan::Test { name: String::new(), args: vec![] })
        );
    }

    #[test]
    fn the_one_shot_tools_parse() {
        assert_eq!(
            plan(&["debug", "inspect", "/tmp/a.png"]),
            Plan::Debug(DebugPlan::Inspect(PathBuf::from("/tmp/a.png")))
        );
        assert_eq!(plan(&["debug", "make-sync-clip"]), Plan::Debug(DebugPlan::MakeSyncClip(None)));
        assert_eq!(
            plan(&["debug", "make-sync-clip", "/tmp/ref.mp4"]),
            Plan::Debug(DebugPlan::MakeSyncClip(Some("/tmp/ref.mp4".into())))
        );
        assert_eq!(
            plan(&["debug", "calibrate-sync", "/tmp/rec.mp4", "--apply"]),
            Plan::Debug(DebugPlan::CalibrateSync {
                file: Some(PathBuf::from("/tmp/rec.mp4")),
                apply: true,
            })
        );
        assert_eq!(
            plan(&["debug", "cloud-upload", "/tmp/a.png", "--account", "ab12", "--auto-share"]),
            Plan::Debug(DebugPlan::CloudUpload {
                file: PathBuf::from("/tmp/a.png"),
                account: "ab12".into(),
                auto_share: true,
            })
        );
    }

    #[test]
    fn the_debug_namespace_still_refuses_nonsense() {
        for line in [
            vec!["debug"],
            vec!["debug", "sing"],
            vec!["debug", "inspect"],
            vec!["debug", "cloud-upload", "/tmp/a.png"],
            vec!["debug", "cloud-upload", "--account", "ab12"],
        ] {
            assert_eq!(parse(&argv(&line)).expect_err(&format!("{line:?}")).code, 2, "{line:?}");
        }
    }
}
