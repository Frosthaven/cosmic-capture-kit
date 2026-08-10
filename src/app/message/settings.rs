//! `SettingsMsg` sub-enum split out of the former flat `Msg` (see app/mod.rs).

use crate::app::DirTarget;
use crate::shortcuts::{Action, Shortcut};
use cosmic::widget::color_picker::ColorPickerUpdate;

/// One GLOBAL capture hotkey this app advertises: what it is called, what it launches, and
/// (on the two platforms with a resident daemon) which persisted spec string a settings row
/// edits.
///
/// macOS/Windows (DRAGON-295): each slot is an independent spec string persisted separately
/// (see the `capture_*_hotkey` fields), and the daemon registers each that is set.
///
/// **Compiled on EVERY platform since DRAGON-589**, and the widening is the point. Linux
/// cannot register any of these itself, so its Global tab shows the same seven actions with
/// the COMMAND that runs each one instead of a chord editor. Both pages need one list, in one
/// order, under one set of names: a second Linux-only table would be a second vocabulary for
/// the same seven verbs, and the two would drift the first time a slot was added. What stays
/// gated to macOS and Windows is the `SettingsMsg` variants that EDIT a slot, so Linux's
/// message enum is byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureHotkeySlot {
    /// "Capture All In One" — opens the full region/window/monitor picker overlay
    /// (the former single "Start Capture" hotkey).
    AllInOne,
    /// "Capture Active Window" — immediately captures the frontmost window, no picker.
    ActiveWindow,
    /// "Capture Active Monitor" — immediately captures the monitor under the cursor, no picker.
    ActiveMonitor,
    /// DRAGON-428: "Capture All In One (no editor)" — the same picker overlay, but the
    /// capture is saved, copied and notified without the preview editor opening.
    AllInOneNoEditor,
    /// DRAGON-428: "Capture Active Window (no editor)".
    ActiveWindowNoEditor,
    /// DRAGON-428: "Capture Active Monitor (no editor)".
    ActiveMonitorNoEditor,
    /// DRAGON-582: "Color Picker" — opens the colour picker tool. LAST in the list, so
    /// the six capture slots keep their order and their indices (the pure duplicate
    /// check speaks in indices, and the two daemons' registration tables are zipped
    /// against the same order).
    ColorPicker,
}

impl CaptureHotkeySlot {
    /// Every slot, in the order the settings page lists them AND the order both daemons
    /// register them (`load_specs` on Windows, the `Spawn` slot list on macOS). One order,
    /// three consumers: change it here and the rows, the registration and the DRAGON-452
    /// duplicate check move together.
    pub const ALL: [Self; 7] = [
        Self::AllInOne,
        Self::ActiveWindow,
        Self::ActiveMonitor,
        Self::AllInOneNoEditor,
        Self::ActiveWindowNoEditor,
        Self::ActiveMonitorNoEditor,
        Self::ColorPicker,
    ];

    /// This slot's position in [`Self::ALL`] — the index the pure
    /// [`crate::shortcuts::capture_hotkey_conflict`] check speaks in.
    // Honestly dead on Linux: the duplicate check exists to stop two DAEMON-registered chords
    // colliding, and Linux registers none (its Global rows are commands, which cannot collide).
    // The body stays portable because the answer is the same everywhere.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub fn index(self) -> usize {
        match self {
            Self::AllInOne => 0,
            Self::ActiveWindow => 1,
            Self::ActiveMonitor => 2,
            Self::AllInOneNoEditor => 3,
            Self::ActiveWindowNoEditor => 4,
            Self::ActiveMonitorNoEditor => 5,
            Self::ColorPicker => 6,
        }
    }

    /// The row label, and the name used when telling the user which shortcut lost its
    /// binding (DRAGON-452). Matches the daemons' slot names word for word.
    pub fn label(self) -> &'static str {
        match self {
            Self::AllInOne => "Capture All In One",
            Self::ActiveWindow => "Capture Active Window",
            Self::ActiveMonitor => "Capture Active Monitor",
            Self::AllInOneNoEditor => "Capture All In One (no editor)",
            Self::ActiveWindowNoEditor => "Capture Active Window (no editor)",
            Self::ActiveMonitorNoEditor => "Capture Active Monitor (no editor)",
            Self::ColorPicker => "Color Picker",
        }
    }

    /// The argv that PERFORMS this slot's action, which is what a resident daemon spawns and
    /// what a desktop-level shortcut has to run (DRAGON-589).
    ///
    /// The "(no editor)" twins are `<capture flag> --no-editor`: `--no-editor` is a MODIFIER
    /// rather than a mode, so it composes with the flag its twin uses alone.
    ///
    /// Both daemons keep their own copy of this table, because argv is their only channel to
    /// the child and neither can reach into the settings layer: macOS `Spawn::args`, Windows
    /// its slot list. Those two must stay word-for-word this, each pinned by its own test.
    /// This copy is the one a HUMAN is shown, and it is in the shared tree so `cargo test`
    /// proves the spelling on any host.
    // Honestly dead on macOS and Windows, and the Windows cross-check is what said so. There
    // the resident daemon BINDS these keys, so the Global Capture row is a chord editor that
    // never prints a command, and each daemon spawns from its OWN copy of this table. The body
    // still stays portable and ungated: it is the copy the tests below check, and those run on
    // every host, which is the only reason a spelling drift is catchable at all from Linux.
    #[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
    pub fn flags(self) -> &'static [&'static str] {
        match self {
            Self::AllInOne => &["--all-in-one"],
            Self::ActiveWindow => &["--active-window"],
            Self::ActiveMonitor => &["--active-monitor"],
            Self::AllInOneNoEditor => &["--all-in-one", "--no-editor"],
            Self::ActiveWindowNoEditor => &["--active-window", "--no-editor"],
            Self::ActiveMonitorNoEditor => &["--active-monitor", "--no-editor"],
            Self::ColorPicker => &["--color-picker"],
        }
    }

    /// Which picker-free IMMEDIATE capture this slot performs, if it performs one at all.
    ///
    /// The distinction decides whether a build can offer the action AT ALL (DRAGON-589). An
    /// immediate capture needs the compositor to say which window or monitor is active and
    /// then hand over its pixels; a sandboxed session cannot ask, so those actions are absent
    /// there rather than merely unbindable. `None` means the action needs no such answer:
    /// All In One opens the ordinary picker overlay and the colour picker samples a snapshot,
    /// both of which work wherever a capture works.
    ///
    /// The availability question itself is `capture_flow::immediate_capture_available`; this
    /// is only the mapping from a hotkey slot to the capture it asks for.
    /// Pure, unit-tested: is this one of DRAGON-428's "(no editor)" twins, the slots that
    /// save, copy and notify without ever opening the preview editor?
    ///
    /// It matters because an editor-less capture has NO WINDOW OF OURS to deliver a
    /// clipboard write through; see `global_capture_items` for what that costs in a
    /// sandbox.
    pub fn no_editor(self) -> bool {
        matches!(
            self,
            Self::AllInOneNoEditor | Self::ActiveWindowNoEditor | Self::ActiveMonitorNoEditor
        )
    }

    pub fn immediate(self) -> Option<crate::app::ImmediateCapture> {
        use crate::app::ImmediateCapture;
        match self {
            Self::ActiveWindow | Self::ActiveWindowNoEditor => Some(ImmediateCapture::ActiveWindow),
            Self::ActiveMonitor | Self::ActiveMonitorNoEditor => {
                Some(ImmediateCapture::ActiveMonitor)
            }
            Self::AllInOne | Self::AllInOneNoEditor | Self::ColorPicker => None,
        }
    }
}

/// DRAGON-589: the global capture slots as ONE list, now that Linux reads the same labels,
/// order and flags the two daemons do.
#[cfg(test)]
mod capture_hotkey_slot_tests {
    use super::*;

    /// Every slot appears in `ALL` exactly once, at the index `index()` claims. The pure
    /// duplicate check speaks in those indices, and both daemons zip their registration
    /// tables against this order.
    #[test]
    fn every_slot_is_listed_once_at_its_own_index() {
        assert_eq!(CaptureHotkeySlot::ALL.len(), 7);
        for (i, slot) in CaptureHotkeySlot::ALL.into_iter().enumerate() {
            assert_eq!(slot.index(), i, "{slot:?}");
            assert_eq!(
                CaptureHotkeySlot::ALL.iter().filter(|s| **s == slot).count(),
                1,
                "{slot:?} listed twice"
            );
        }
    }

    /// The flags a desktop shortcut runs, pinned exactly. These are the strings `main` parses,
    /// so a typo here is a shortcut that opens a capture overlay instead of doing the thing.
    #[test]
    fn each_slot_spells_the_flags_main_parses() {
        use CaptureHotkeySlot as S;
        assert_eq!(S::AllInOne.flags(), ["--all-in-one"]);
        assert_eq!(S::ActiveWindow.flags(), ["--active-window"]);
        assert_eq!(S::ActiveMonitor.flags(), ["--active-monitor"]);
        assert_eq!(S::ColorPicker.flags(), ["--color-picker"]);
        // A "(no editor)" twin is its original plus the modifier, in that order, and never
        // anything else: the daemons spawn exactly this and the two must not drift.
        for (twin, original) in [
            (S::AllInOneNoEditor, S::AllInOne),
            (S::ActiveWindowNoEditor, S::ActiveWindow),
            (S::ActiveMonitorNoEditor, S::ActiveMonitor),
        ] {
            assert_eq!(twin.flags(), [original.flags()[0], "--no-editor"], "{twin:?}");
            assert_eq!(original.flags().len(), 1, "{original:?}");
        }
    }

    /// A slot never has an empty flag list and never a short-form flag: `main` matches these
    /// by exact token, and an empty argv would launch the bare picker.
    #[test]
    fn no_slot_launches_with_nothing() {
        for slot in CaptureHotkeySlot::ALL {
            assert!(!slot.flags().is_empty(), "{slot:?}");
            assert!(!slot.label().is_empty(), "{slot:?}");
            for f in slot.flags() {
                assert!(f.starts_with("--"), "{slot:?} flag {f} must be a long flag");
            }
        }
    }

    /// Only the four active-window / active-monitor slots ask the compositor what is active,
    /// and a twin asks the same question its original does. This is the mapping the
    /// "absent in this build" rule reads, so a wrong answer either hides a working action or
    /// advertises one that cannot run.
    #[test]
    fn exactly_the_active_target_slots_are_immediate_captures() {
        use crate::app::ImmediateCapture as I;
        use CaptureHotkeySlot as S;
        assert_eq!(S::ActiveWindow.immediate(), Some(I::ActiveWindow));
        assert_eq!(S::ActiveWindowNoEditor.immediate(), Some(I::ActiveWindow));
        assert_eq!(S::ActiveMonitor.immediate(), Some(I::ActiveMonitor));
        assert_eq!(S::ActiveMonitorNoEditor.immediate(), Some(I::ActiveMonitor));
        assert_eq!(S::AllInOne.immediate(), None);
        assert_eq!(S::AllInOneNoEditor.immediate(), None);
        assert_eq!(S::ColorPicker.immediate(), None);
        // And the two axes agree: a slot is immediate exactly when its capture flag is one of
        // the two `main` treats as picker-free.
        for slot in S::ALL {
            let by_flag = slot.flags()[0] == "--active-window" || slot.flags()[0] == "--active-monitor";
            assert_eq!(slot.immediate().is_some(), by_flag, "{slot:?}");
        }
    }
}

/// The Cloud Accounts settings page's own messages (DRAGON-482).
///
/// A sub-enum under [`SettingsMsg::Cloud`] rather than eight more flat variants: the page
/// owns a small state machine (an add dialog, a provider choice, a disconnect
/// confirmation) that nothing else in settings shares, and keeping it in one type is what
/// lets stage B2 add to it without editing the outer enum every time.
///
/// Stage A1 declared the vocabulary and handled every variant as a documented no-op; the
/// settings-page stage filled the bodies in and added the variants the live flow needs
/// (the provider list's open state, the connect outcome, the folder step, and the two
/// refresh messages). A device-code fallback flow lived here too for a while (DRAGON-482)
/// but was removed (DRAGON-489 follow-up): Google's needed a client type this app doesn't
/// register, and the QR-code/link browser flow already covers "sign in from another device"
/// better than a typed code.
///
/// # The generation number
///
/// Five variants carry a `u64` generation: [`Self::Connected`], [`Self::BrowserUrl`],
/// [`Self::FoldersLoaded`], and DRAGON-506's [`Self::FolderCreated`] and
/// [`Self::FolderDeleted`]. A connect runs on a background thread that CANNOT be aborted
/// (the loopback listener owns its own deadline), so "Cancel" cannot stop it, only stop
/// caring about it. The page bumps its generation whenever the user cancels or closes the
/// dialog, and a reply stamped with an older one is dropped. Without it, a cancelled
/// sign-in finished in the browser would silently add an account minutes later.
///
/// The folder-manager replies carry it for the same reason and for one more: every navigation
/// between levels bumps the generation too, so a listing (or a create, or a delete) started at
/// one level can never land on the list of another.
#[derive(Debug, Clone)]
pub enum CloudSettingsMsg {
    /// Open the "add an account" dialog.
    AddOpen,
    /// Close it without connecting.
    AddClose,
    /// Choose which provider to connect, by index into `cloud::registry()`. An index, not
    /// an id, because it comes straight from a dropdown; the handler resolves it against
    /// the registry and ignores one that is out of range.
    AddProviderSelected(usize),
    /// Open (`true`) or close (`false`) the provider list in the add dialog.
    ///
    /// The list is a popover the page builds itself rather than a `widget::dropdown`,
    /// because two of the five providers are listed but NOT selectable, which a dropdown
    /// cannot express. So its open state is ours to hold.
    AddProviderMenu(bool),
    /// Start the OAuth flow for the chosen provider, in the browser. A no-op for a
    /// provider whose `AuthKind` is `Unofficial`, which the page also says on the row.
    Connect,
    /// The interactive (browser) flow now knows its sign-in page's own address: the
    /// generation, and the URL (DRAGON-489). Reported moments after the Browser step begins.
    /// The page does NOT open it automatically (DRAGON-489 follow-up: an automatic launch can
    /// fail silently, no default browser set, the wrong browser opens, a headless/remote
    /// session, with nothing else on screen to fall back to); instead it shows a clickable
    /// accent link plus a scannable QR code, and the user picks whichever suits them,
    /// including finishing on their phone.
    BrowserUrl(u64, String),
    /// Copy the browser sign-in URL to the clipboard.
    CopyBrowserUrl(String),
    /// Open the browser sign-in URL: the browser step's own click-through, since the page is
    /// no longer opened automatically (DRAGON-489 follow-up).
    ///
    /// This needs no host check: the URL is built entirely by our own code from the hardcoded
    /// per-provider `auth_url` constant (see `cloud::AuthKind::OAuthPkce`), so it is already
    /// trusted before it ever reaches the UI, unlike provider-supplied network input.
    OpenBrowserUrl(String),
    /// The external-tool probe finished (DRAGON-485): whether Proton's own `proton-drive`
    /// command-line tool is installed and can run.
    ///
    /// A RUNTIME answer, unlike every other provider's availability, which is decided once at
    /// compile time by whether a client id was baked in. The user can install the tool while
    /// this app is running, so the probe runs whenever the add dialog or its provider list is
    /// opened, and the picker's row draws whatever the latest answer is (see
    /// `pages::cloud::tool_entry`). Bounded by `cloud::proton::PROBE_BUDGET` and run off the
    /// UI thread, because it starts a process.
    ToolProbed(crate::cloud::proton::Availability),
    /// Open an external-tool provider's DOWNLOAD page, by index into `cloud::registry()`
    /// (DRAGON-485): what a press on a picker row does while the tool is not installed.
    ///
    /// An index rather than a URL, for the same reason [`Self::OpenGuide`] carries nothing at
    /// all: the handler reads the address out of the registry row's own
    /// `AuthKind::ExternalTool { download_url, .. }`, a compile-time constant, so no URL from a
    /// message or from a provider can ever ride this path to the OS opener.
    OpenToolDownload(usize),
    /// Open the cloud accounts setup guide in the user's browser (DRAGON-508): the add
    /// dialog's empty-state link, shown when this build has no provider configured at all.
    ///
    /// Carries NO payload on purpose. The URL is the compile-time
    /// `pages::cloud::CLOUD_GUIDE_URL` constant, read by the handler, so there is no way for a
    /// URL from anywhere else to reach the OS opener along this path.
    OpenGuide,
    /// A connect attempt finished: the generation, and either the account it saved (tokens
    /// already stored through `cloud::secrets`) or a message saying why not.
    Connected(u64, Result<crate::cloud::accounts::CloudAccount, String>),
    /// The folder setup for the account being configured finished: the generation, and either
    /// the account's app folder plus what is inside it (`cloud::providers::FolderSetup`) or a
    /// message.
    ///
    /// Carried the folder list alone until DRAGON-498. It carries the ROOT with it now because
    /// finding (or creating) that folder is part of the same round trip, and because on Google
    /// Drive the root is a real folder id the account has to remember: a listing without it
    /// would leave "the app folder" unaddressable.
    FoldersLoaded(u64, Result<crate::cloud::providers::FolderSetup, String>),
    /// The folder browser OPENED, walked back down to the account's saved destination
    /// (DRAGON-517): the generation, and either the view to show (the app folder, the levels
    /// below it the walk resolved, and what is inside the deepest one) or a message.
    ///
    /// Separate from [`Self::FoldersLoaded`], which carries ONE level, because opening the
    /// browser can take several: an account remembers its destination as a path of display names,
    /// and the ids the breadcrumb needs for each level of it are not on disk. The walk is one
    /// detached job (`cloud::providers::restore_browse`) so the step spins its refresh glyph once
    /// and lands once, however deep the saved folder is.
    ///
    /// `Err` means not one level came back, which is the same "the folder list could not be read"
    /// path a failed listing takes. A walk that got partway carries its reason INSIDE the view
    /// instead, because there is still something to show.
    FolderViewRestored(u64, Result<crate::cloud::providers::RestoredBrowse, String>),
    /// Switch the setup step's destination TAB (DRAGON-485), for the one provider that has two
    /// kinds of destination.
    ///
    /// **The tab IS the destination**: pressing Done writes whichever one is showing, so this
    /// is not a view toggle over one answer, it is the choice itself. Nothing is written here,
    /// exactly as browsing writes nothing (DRAGON-517); switching tabs starts that tab's own
    /// listing and Cancel still leaves the account as it was found.
    ///
    /// Carries the account's own `Destination` rather than a UI-only enum, so there is no
    /// mapping between two spellings of one choice for anything to get wrong.
    SetupTab(crate::cloud::accounts::Destination),
    /// Choose one of the listed ALBUMS as this account's destination, by index (DRAGON-485).
    ///
    /// The Photos tab's answer to [`Self::FolderCrumbIn`], and deliberately a different
    /// message: a folder is chosen by being descended INTO, and an album, being flat, has
    /// nowhere to descend to, so a press selects instead of navigating. See
    /// `cloud::proton::ALBUMS` for why the two models stay apart.
    ///
    /// Nothing is written here either; [`Self::SetupDone`] is what commits it.
    AlbumPicked(usize),
    /// The account's ALBUMS landed (DRAGON-485): the generation, and either the app's own
    /// album plus every album this account has (`cloud::providers::FolderSetup`, whose `root`
    /// carries the default) or a message.
    ///
    /// Separate from [`Self::FoldersLoaded`] because the two answers are consumed differently:
    /// a folder listing fills a level and may walk the breadcrumb up when the level has gone,
    /// and an album listing fills a flat set and may move the SELECTION when the chosen album
    /// has gone. Folding them together would mean one handler doing both under a flag.
    AlbumsLoaded(u64, Result<crate::cloud::providers::FolderSetup, String>),
    /// The app's own album was provisioned on the way to saving a Photos destination
    /// (DRAGON-522): the generation, and either the album or a message.
    ///
    /// **The one path that CREATES a default album.** Listing no longer does
    /// (`cloud::providers::needs_default_album`): opening the Photos tab, refreshing it, or
    /// editing an account whose album is something else all used to leave a "Cosmic Capture Kit"
    /// album behind, which was the report. Provisioning is now what happens when the destination
    /// genuinely resolves to the default and the default is not there, and confirming Photos with
    /// nothing chosen is exactly that case: this message is its reply, and its handler finishes
    /// the Done that started it.
    DefaultAlbumMade(u64, Result<crate::cloud::providers::RemoteFolder, String>),
    /// Re-run the folder listing for the account the setup step is configuring (DRAGON-503).
    ///
    /// The refresh button that replaces the "Reading your folders" line once a listing has
    /// landed: a user who just made a subfolder at the provider needs a way to see it without
    /// closing the step and re-opening it. It stamps a fresh generation like every other
    /// connect-adjacent flow, so a slower earlier listing cannot land on top of this one.
    FolderRefresh,
    // ── Tombstone: `FolderPicked(Option<usize>)` (DRAGON-517) ────────────────────────────────
    //
    // "Put my captures in this folder": an index into the listing, or `None` for the level the
    // browser was inside. It was the OTHER press the folder step had, alongside the one that
    // navigated, and it is gone with the distinction. Where the browser IS, is where the captures
    // go, so [`Self::FolderCrumbIn`] and [`Self::FolderCrumbTo`] are the whole of choosing now,
    // and [`Self::SetupDone`] is what writes the answer.
    //
    // A `FolderPathInput` (a typed destination path, for the one provider that addresses folders
    // by path) had already been deleted from beside it in DRAGON-498, when every provider became
    // confined to one app folder.
    /// Go INTO one of the listed folders, by index (DRAGON-506), which since DRAGON-517 is also
    /// how a destination is chosen: the level the browser is showing is where this account's
    /// captures land, so descending into a folder selects it.
    ///
    /// Nothing is written here. The account's record is updated once, by [`Self::SetupDone`], so
    /// browsing costs no file writes and Cancel really does leave the account as it was found.
    FolderCrumbIn(usize),
    /// Go back OUT to a level already in the breadcrumb, by crumb index (DRAGON-506): 0 is the
    /// app folder, 1 the first folder descended into, and so on. The elision a deep trail draws
    /// sends nothing, since it names no single level to go to.
    ///
    /// The mirror of [`Self::FolderCrumbIn`] under DRAGON-517: coming back out moves the
    /// destination out with the view.
    FolderCrumbTo(usize),
    /// Open the inline "new folder" row at the level being viewed (DRAGON-506).
    FolderCreateOpen,
    /// Close it without creating anything.
    FolderCreateCancel,
    /// The new folder's name field changed. Buffered raw, exactly as
    /// [`Self::AccountLabelInput`] is, so a user clearing it sees an empty field; the name is
    /// validated when they submit.
    FolderCreateInput(String),
    /// Create the folder now (the tick beside the field, or Enter in it).
    FolderCreateSubmit,
    /// A create finished: the generation, and either the folder the provider made or a message
    /// saying why not.
    FolderCreated(u64, Result<crate::cloud::providers::RemoteFolder, String>),
    /// Ask to delete one of the listed folders, by index (DRAGON-506). Opens a confirmation,
    /// because a folder takes everything inside it when it goes.
    FolderDelete(usize),
    /// The user backed out of the delete.
    FolderDeleteCancel,
    /// The user confirmed it. This is the one that calls the provider.
    FolderDeleteConfirm,
    /// A delete finished: the generation, and either success or a message. Either way the level
    /// is re-listed, so what is on screen is what the provider actually has.
    FolderDeleted(u64, Result<(), String>),
    /// The account name field on the setup step changed (DRAGON-489 follow-up). Buffered
    /// straight onto the in-progress account and applied at [`Self::SetupDone`], which since
    /// DRAGON-498 is the only thing that step still has left to commit.
    ///
    /// Buffered RAW, empty string included, so a user clearing the field to retype it sees an
    /// empty field. An empty name becomes the provider default on the way to disk instead
    /// (DRAGON-495; see the page's `commit_label`).
    AccountLabelInput(String),
    /// Finish the setup step: write the account's name, and (where the provider has folders) the
    /// level the browser is showing, onto the account and close.
    ///
    /// **The destination is written HERE and nowhere else** (DRAGON-517). It used to be written
    /// the instant a folder was picked, because picking was a separate act from browsing; with the
    /// browser's level as the destination, every crumb press would otherwise be a file write.
    ///
    /// Named `FolderDone` until DRAGON-495, when the step stopped being about folders alone
    /// (it is now shown for every provider, and carries the account's name).
    SetupDone,
    /// Re-read the accounts file into the page (and re-probe the stored sign-ins). Sent
    /// when the settings window opens and after anything that changes the list.
    Reload,
    /// The off-thread sign-in probe finished: the ids of the accounts whose stored tokens
    /// warrant a Reconnect. Off-thread because reading a token means reading the platform
    /// keyring, which is a D-Bus round trip on Linux.
    HealthLoaded(Vec<String>),
    /// Connect an already-listed account again, by id, because its authorization is gone
    /// (see `cloud::oauth::RECONNECT_PREFIX`). Opens the same dialog as adding one, with
    /// the provider fixed.
    Reconnect(String),
    /// Re-open the setup step for an already-connected account, by id (the gear on its row,
    /// DRAGON-489 follow-up). No OAuth: the account is already authorized, so this only
    /// re-lists its folders and lets the user RENAME it or pick a different destination. Opens
    /// the add dialog jumped straight to the Setup step (see `CloudAdd::edit_account`).
    ///
    /// Named `EditFolder` until DRAGON-495. It is offered on EVERY row now, including a
    /// provider with no folders at all (YouTube), because renaming is the half of the step
    /// every account has, and gating the button on folders left those rows unrenameable.
    EditAccount(String),
    /// Disconnect an account, by id: asks first (see [`Self::DisconnectConfirm`]), because
    /// it drops the stored tokens and cannot be undone.
    Disconnect(String),
    /// The user confirmed the disconnect, by id. This is the one that removes the account
    /// from `cloud::accounts` AND its tokens from `cloud::secrets`.
    DisconnectConfirm(String),
    /// The user backed out of the disconnect.
    DisconnectCancel,
    /// The disconnect finished: the account id (so the page can clear ONLY that id's
    /// in-flight "Disconnecting..." marker, DRAGON-489 follow-up), and either success or a
    /// message. `Err` is the LOCAL removal failing (a revoke that the provider refused is
    /// best-effort and never reported here), which the page shows as a notice, because an
    /// account still listed after a disconnect needs explaining.
    Disconnected(String, Result<(), String>),
    /// Cancel pressed on the post-connect setup step (DRAGON-489 follow-up).
    /// By this point the OAuth handshake already completed and saved a real, connected
    /// account (`connect_and_save`), so unlike every earlier step this one cannot just close
    /// the dialog: for a FRESH connect (`CloudAdd::reconnect_of` is `None`) that would leave a
    /// stray connected-but-unconfigured account behind, so this disconnects it too. For a
    /// RECONNECT of an existing account, cancelling just closes the dialog and keeps the
    /// (now freshly refreshed) tokens; the account already existed before this flow started
    /// and Cancel must not delete it.
    ///
    /// Since DRAGON-517 it also DISCARDS wherever the folder browser was left: the destination is
    /// written by [`Self::SetupDone`] alone, so an account opened from the gear, browsed around
    /// and then cancelled keeps the folder it had. That is what Cancel has always claimed to do,
    /// and could not while a pick wrote itself to disk on the spot.
    ///
    /// Named `CancelFolderStep` until DRAGON-495; the step it cancels is now the whole
    /// post-connect setup, not the folder choice alone.
    CancelSetupStep,
    /// A tick while the Browser step is showing (DRAGON-489 follow-up): nothing in state
    /// changes here, it exists purely to force the view to rebuild once a second so
    /// `cloud_browser_step`'s countdown (read from `CloudAdd::browser_started` and
    /// `Instant::now()` at render time) stays live.
    CloudBrowserTick,
    /// A tick while a folder listing is in flight (DRAGON-514): advance the path row's refresh
    /// glyph by one step, so it SPINS. That spin is the only indication the browser gives that
    /// a listing is running; the "Reading your folders..." line it replaced is deleted.
    ///
    /// Unlike [`Self::CloudBrowserTick`] this one does change state (`CloudAdd::folder_spin`),
    /// because the angle it advances has nowhere else to live. It stops with the listing.
    FolderSpinTick,
}

/// Which window-capture border a colour-picker edit targets (DRAGON-191).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderColorTarget {
    /// The ACTIVE (focused / single-window) border. Its colour is an `Option` (a Reset
    /// clears it back to "follow the system accent").
    Active,
    /// The INACTIVE (unfocused) border. Always a concrete colour.
    Inactive,
}

#[derive(Debug, Clone)]
pub enum SettingsMsg {
    /// Settings: toggle whether captures include the mouse cursor.
    SetCaptureCursor(bool),
    /// Settings: toggle whether window captures keep their transparency.
    SetCaptureTransparency(bool),
    /// Settings: toggle whether region/monitor captures include the wallpaper.
    SetCaptureWallpaper(bool),
    /// Settings (DRAGON-191): single-window capture focus appearance (dropdown index
    /// 0 = Active, 1 = Inactive).
    SetWindowFocusAppearance(usize),
    /// Settings: the MASTER "Enable single window aesthetic effects" toggle. OFF
    /// delivers the bare captured frame and hides every other single-window aesthetic
    /// row; the individual preferences stay persisted.
    SetWindowRecompositing(bool),
    /// Settings (DRAGON-209): the region selection box thickness (slider, 1-8 px).
    SetSelectionBoxThickness(u32),
    /// Settings (DRAGON-191): the ACTIVE window-capture border width (slider, 0-10 px).
    SetActiveBorderWidth(u32),
    /// Settings (DRAGON-191): the INACTIVE window-capture border width (slider, 0-10 px).
    SetInactiveBorderWidth(u32),
    /// Settings (DRAGON-191): reset the ACTIVE window-capture border to defaults (colour
    /// back to "follow accent" = None, width back to the default).
    ResetActiveBorder,
    /// Settings (DRAGON-191): reset the INACTIVE window-capture border to defaults
    /// (default colour + default width).
    ResetInactiveBorder,
    /// Settings (DRAGON-191): toggle the reconstructed drop shadow on window captures.
    SetWindowDropShadow(bool),
    /// Settings (DRAGON-191): open (`true`) / close (`false`) the border colour-picker
    /// sidebar, carrying which border it edits.
    ToggleBorderColorEditor(BorderColorTarget, bool),
    /// Settings (DRAGON-191): drive the border colour picker (hex/RGB input, hue, save /
    /// reset / cancel). Save/Reset persist + apply the colour and close the panel.
    BorderColorPicker(ColorPickerUpdate),
    // Transparency multiplier parked (linear-light over() makes it redundant):
    // /// Settings: window-capture transparency multiplier (0..1).
    // SetWindowTransparencyMultiplier(f32),
    /// Settings: toggle the transparent margin around window captures.
    SetWindowPadding(bool),
    /// Settings: set the window-capture padding width (px), from its text input.
    SetWindowPaddingPx(String),
    /// Settings: toggle freeze-pixels (takes effect next launch).
    SetFreeze(bool),
    /// Settings: toggle staying resident (the tray/menu-bar RESIDENT process). Emitted
    /// by the "System tray icon" row on macOS (menu-bar daemon), Linux (ksni
    /// tray resident, DRAGON-173), and Windows (Win32 tray daemon, DRAGON-237); a no-op on
    /// any platform without a resident, so the variant is gated to the three that construct it.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    SetResident(bool),
    /// Settings (DRAGON-296): toggle "Automatically start on login" — persist
    /// `autostart_on_login` and reconcile the OS login item (registered iff the tray is on
    /// AND this is on). Emitted by the row directly below "System tray icon", which is
    /// hidden while the tray is off. Gated to the three OSes with a resident (same set as
    /// `SetResident`), so Linux/other `SettingsMsg` shapes stay byte-identical elsewhere.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    SetAutostartOnLogin(bool),
    /// Settings (DRAGON-618): a Flatpak's Background-portal autostart request has settled.
    ///
    /// Linux only, because it is the only platform whose registration is ASYNCHRONOUS. A
    /// Flatpak cannot write the host's autostart directory, so it asks
    /// `org.freedesktop.portal.Background` instead, and the portal may put a confirmation
    /// dialog in front of the user. The payload is what autostart is registered as AFTER the
    /// request, which is NOT what was asked for when the user declines; `Err` is a request
    /// that never landed. `autostart::settled_toggle` turns either into the value the toggle
    /// shows, so it can never claim a registration that did not happen.
    #[cfg(target_os = "linux")]
    AutostartPortalSettled(Result<bool, String>),
    /// Settings: region selection overlay dim opacity.
    SetRegionOpacity(f32),
    /// Settings (DRAGON-582): colour-picker overlay dim opacity.
    SetColorPickerOpacity(f32),
    /// Settings: active (countdown/recording) overlay dim + line opacity.
    SetActiveOpacity(f32),
    /// Settings: post-capture preview overlay dim opacity.
    SetPreviewOpacity(f32),
    /// Settings: recording frame rate text field.
    SetRecordFps(String),
    /// Settings: recording target bitrate (Kbps).
    SetRecordBitrate(String),
    /// Settings: recording max-resolution preset index.
    SetRecordResPreset(usize),
    /// Settings: custom recording max width / height.
    SetRecordMaxWidth(String),
    SetRecordMaxHeight(String),
    /// Settings: pick the NVENC preset (index into `NVENC_PRESETS`).
    SetNvencPreset(usize),
    /// Settings: pick the x264 preset (index into `X264_PRESETS`).
    SetX264Preset(usize),
    /// Settings: pick the VAAPI compression level (index into `VAAPI_CL_VALUES`).
    SetVaapiPreset(usize),
    /// Settings: toggle experimental GPU zero-copy capture.
    #[cfg(feature = "zero-copy")]
    SetRecordZeroCopy(bool),
    /// Settings: pick the video codec (index into `CODEC_VALUES`).
    SetRecordCodec(usize),
    /// Settings: pick the preferred encoder (index into `encoders`).
    SetPreferredEncoder(usize),
    /// Windows (DRAGON-238): the off-thread encoder probe finished — store the list.
    /// Kicked when the settings window opens so the video page never blocks on the
    /// `ffmpeg -encoders` + hardware probe-encodes; fills the process-wide cache.
    #[cfg(windows)]
    EncodersProbed(Vec<crate::encode::EncoderInfo>),
    /// DRAGON-564: the off-thread tool-version probe finished - store what each
    /// Health-row binary reported. Kicked when the settings window opens
    /// (`App::kick_tool_version_probe`) so the Health page never blocks on spawning
    /// ffmpeg / ffprobe / tesseract / pactl for their version banners.
    ToolVersionsProbed(Vec<crate::util::ToolVersion>),
    /// Settings: pick which monitor the encoder benchmark tests (index into the
    /// enumerated `bench_monitors`).
    SetBenchMonitor(usize),
    /// Settings: run the encoder benchmark against the selected monitor's true dims.
    RunBenchmark,
    /// Refresh the benchmark progress/results while it runs.
    BenchPoll,
    /// Settings: recording save directory.
    SetRecordDir(String),
    /// Settings: recording capture method (a stable `platform::backend` id).
    SetRecordBackend(String),
    /// Settings: screenshot capture method (a stable `platform::backend` id).
    SetScreenshotBackend(String),
    /// Settings: clear the saved ScreenCast portal permission (restore token).
    ResetScreencastPermission,
    /// Settings: screenshot save directory.
    SetScreenshotDir(String),
    /// Settings: the "Custom text" covermark's text.
    SetCovermarkText(String),
    /// Settings: toggle push-to-talk (mic muted while recording unless the hotkey is held).
    SetPushToTalk(bool),
    /// Settings (DRAGON-174): toggle hiding the floating toolbar on full-screen
    /// captures (when it can't fit outside the recording area).
    SetHideToolbarFullscreen(bool),
    /// Settings: switch the General page's in-page tab (Settings vs Appearance;
    /// DRAGON-138). In the settings domain — it's General-page view state, not
    /// window chrome like the nav-rail `SetConfigTab`.
    SetGeneralTab(cosmic::widget::segmented_button::Entity),
    /// Settings: switch the Capture Modes page's in-page tab (Scanner / Screenshots /
    /// Screen Recordings; DRAGON-140). Same domain rationale as `SetGeneralTab`.
    SetCaptureTab(cosmic::widget::segmented_button::Entity),
    /// Settings: switch the Audio & Video page's in-page tab (Audio / Video;
    /// DRAGON-141). Same domain rationale as `SetGeneralTab`, but it also drives the
    /// live mic sensitivity meter's lifecycle (gated on the Audio tab being active).
    SetAudioVideoTab(cosmic::widget::segmented_button::Entity),
    /// Settings: switch the Keyboard Shortcuts page's in-page tab (Capture /
    /// Recording / Preview; DRAGON-142). Same domain rationale as `SetGeneralTab`.
    SetShortcutsTab(cosmic::widget::segmented_button::Entity),
    /// Settings: preview editor appearance (windowed vs overlay).
    SetPreviewWindowed(bool),
    /// Settings: draw the group captions under the preview editor's top toolbar
    /// (DRAGON-478). Also changes the top bar's HEIGHT, so an open editor re-fits.
    SetPreviewToolbarLabels(bool),
    /// Settings → Health → Debug: write the opt-in debug log (DRAGON-419). Takes effect
    /// immediately, with no restart — a customer turns it on, reproduces the failure once,
    /// and mails the file.
    SetDebugLogging(bool),
    /// Copy a PATH shown in a settings row to the clipboard, carrying the text exactly as the
    /// row shows it. The handler stamps `SettingsState::health_copied`, and the button flashes
    /// its "Copied!" tick.
    ///
    /// Named for Health because that page had the only such line when it landed (DRAGON-540),
    /// and now shared by every row that renders a path through `settings::row::path_line`:
    /// Health's tool locations, Scanner's tesseract language folder, and the preview editor's
    /// covermark folder. The flash is keyed on the copied TEXT rather than on which row asked,
    /// which is exactly why one message and one stamp serve all three.
    CopyHealthLocation(String),
    /// Settings: a tick while a path copy is still inside its flash window.
    /// Nothing in state changes here. Like `CloudSettingsMsg::CloudBrowserTick`, it exists
    /// purely to rebuild the view so the tick reverts by the clock, with no explicit clear.
    HealthCopyTick,
    /// Settings → Health → Debug: open the macOS permission-checker window
    /// (`app::permissions`) from within Settings, rather than only via `--permissions`
    /// or missing-grant routing. Focuses it if already open. macOS-only, mirroring
    /// `PermissionsMsg`'s own cfg pattern; the row that fires this only renders on
    /// macOS, the sole platform with TCC grants to manage.
    #[cfg(target_os = "macos")]
    OpenPermissionsWindow,
    /// Settings: the image editor puts its edits on the clipboard as it closes (DRAGON-467).
    SetPreviewCopyOnExit(bool),
    /// Settings: write every screenshot to the configured save folder as it is taken
    /// (DRAGON-467). Off routes it through the session runtime directory instead.
    SetPreviewSaveOriginals(bool),
    /// Settings: ask before closing the image editor over unsaved edits (DRAGON-467).
    SetPreviewAskToSave(bool),
    /// Settings: the VIDEO editor's own copy-on-exit (DRAGON-467) — the three below mirror
    /// the three above for recordings, and are deliberately separate messages writing
    /// separate fields, so a video document can differ from an image one.
    SetPreviewVideoCopyOnExit(bool),
    /// Settings: the VIDEO editor's own save-originals (DRAGON-467), over `record_dir`.
    SetPreviewVideoSaveOriginals(bool),
    /// Settings: the VIDEO editor's own ask-to-save (DRAGON-467).
    SetPreviewVideoAskToSave(bool),
    /// Settings: open the folder picker for a save directory, then apply it.
    PickDir(DirTarget),
    DirPicked(DirTarget, Option<std::path::PathBuf>),
    /// Settings: toggle real-time noise reduction on the captured mic.
    SetNoiseReduction(bool),
    /// Settings: pick the microphone input device (0 = System / automatic).
    SetMicDevice(usize),
    /// Settings: toggle echo cancellation (cancel speaker bleed into the mic).
    SetEchoCancellation(bool),
    /// Settings: pick the speaker output device used as the echo reference
    /// (0 = System / automatic). Only constructed by the Linux Output picker;
    /// macOS has no output section (DRAGON-132).
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    SetSpeakerDevice(usize),
    /// Settings: toggle input-sensitivity threshold between automatic and manual.
    SetInputSensitivityAuto(bool),
    /// Settings: set the manual input-sensitivity (voice gate) threshold (0..1).
    SetInputSensitivity(f32),
    /// Settings: toggle Automatic Gain Control.
    SetAutoGain(bool),
    /// Settings: toggle Advanced Voice Activity (neural VAD gating).
    SetAdvancedVad(bool),
    /// Settings: open the live microphone test dialog (starts mic capture).
    OpenMicTest,
    /// Close the microphone test dialog (stops mic capture).
    CloseMicTest,
    /// Repaint tick for the live mic-test waveform (snapshots fresh samples).
    MicTestTick,
    /// Settings: toggle muting other apps' audio while a video preview plays.
    SetMuteOthersDuringPreview(bool),
    /// Settings: toggle ducking the recorded system audio under mic speech.
    SetDuckSystemAudio(bool),
    /// Appearance (DRAGON-139): follow the system theme (ON) vs use the overrides
    /// below (OFF). When turned back ON the live theme reverts to following the
    /// system; the override values are kept but ignored.
    SetUseSystemAppearance(bool),
    /// Appearance override: base mode (0 automatic / 1 dark / 2 light).
    SetAppearanceMode(u8),
    /// Appearance override: accent colour (`Some` = a palette swatch's sRGB, `None`
    /// = keep the base theme's own accent). Applied live.
    SetAppearanceAccent(Option<[f32; 3]>),
    /// Appearance override: corner-rounding style (0 round / 1 slightly / 2 square).
    SetAppearanceRoundness(u8),
    /// Appearance (DRAGON-289): toggle "Automatic Contrast Boost" — adapt the selected
    /// accent for optimal contrast (ON) vs use the exact picked colour everywhere (OFF).
    /// Applied live. Only meaningful while System Default is off.
    SetAppearanceContrastBoost(bool),
    /// Appearance: open (`true`) / close (`false`) the hand-rolled custom-accent
    /// colour-picker sidebar panel.
    ToggleAccentEditor(bool),
    /// Appearance: drive the custom-accent colour picker (hex/RGB input, hue, save /
    /// reset / cancel). Save/Reset persist + apply the accent and close the panel.
    AccentPicker(ColorPickerUpdate),
    /// Keyboard Shortcuts: start capturing a new binding for `action` (the next key
    /// press is consumed as its shortcut). Press again or Esc to cancel.
    BeginRebind(Action),
    /// Keyboard Shortcuts: set `action`'s binding (from a captured key, or a reset).
    SetShortcut(Action, Shortcut),
    /// Keyboard Shortcuts: clear `action`'s binding (the "x" button).
    UnbindShortcut(Action),
    /// Keyboard Shortcuts (macOS/Windows): edit one of the resident daemon's three global
    /// capture hotkey specs (DRAGON-295 — the `slot` picks which: All In One / Active Window
    /// / Active Monitor), e.g. "PrintScreen", "Cmd+Shift+2". Persisted; when the daemon is
    /// running, it is restarted so the new keys take effect. Gated to the two OSes with a
    /// daemon-owned global hotkey so Linux's `SettingsMsg` stays byte-identical (Linux's
    /// capture key is a COSMIC custom shortcut, not ours).
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    SetCaptureHotkey(CaptureHotkeySlot, String),
    /// Keyboard Shortcuts (macOS/Windows): start (or cancel) RECORDING one of the three
    /// global capture hotkeys (DRAGON-295 — the `slot` picks which) — the next chord the
    /// user presses is captured, serialized to a daemon spec, and applied via
    /// `SetCaptureHotkey`. Mirrors `BeginRebind` for the in-app rows: press the button to
    /// begin, press again or Esc to cancel.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    BeginCaptureHotkeyRebind(CaptureHotkeySlot),
    /// Keyboard Shortcuts (macOS/Windows): while the "Start Capture" chord recorder is armed,
    /// a ~1s timer sends this so the running daemon SUSPENDS its global hotkey (the key
    /// then reaches this app to be recorded). Fire-and-forget signal; the daemon
    /// auto-resumes after the pings stop.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    SuspendDaemonHotkeyPing,
    /// Health (macOS): open a System Settings > Privacy & Security pane for a TCC
    /// permission (Screen Recording / Microphone) — the honest recovery for a denied
    /// grant (a re-request prompt only fires once per code identity). macOS-only so
    /// Linux's `SettingsMsg` stays byte-identical.
    #[cfg(target_os = "macos")]
    OpenTccPane(crate::platform::mac::tcc::PrivacyPane),
    /// Health (macOS): request Microphone TCC access — fires the one-shot OS prompt
    /// when the status is NotDetermined. macOS-only (see `OpenTccPane`).
    #[cfg(target_os = "macos")]
    RequestMicTcc,
    /// Health (macOS): request Screen Recording TCC access — fires the one-shot OS
    /// prompt and marks it spent (`mac_first_run_seen`), after which the row's
    /// action falls back to `OpenTccPane`. Only offered while the prompt hasn't
    /// been fired this TCC lifetime. macOS-only (see `OpenTccPane`).
    #[cfg(target_os = "macos")]
    RequestScreenTcc,
    /// About (DRAGON-175): start (or restart) a background update check — fired on
    /// the "Check for updates" button and when the settings window opens.
    CheckForUpdates,
    /// About: a background update check finished; carries the resolved status,
    /// cached in `App::update_status` (drives the nav tint + About page rows).
    UpdateChecked(crate::update::UpdateStatus),
    /// About (macOS): start the one-click download + verify + install of the
    /// available update. A no-op unless the status is `Available` with an artifact.
    /// Only constructed by the macOS About page (Linux has no one-click yet);
    /// compiled (and type-checked) everywhere on purpose, like `Msg::Permissions`.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    InstallUpdate,
    /// About (macOS): the one-click install finished. `Staged` means the swap
    /// helper is armed and the app must now quit so the swap + relaunch can run;
    /// `Failed` surfaces the reason inline. Only constructed on macOS (see
    /// `InstallUpdate`).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    UpdateInstallDone(crate::update::InstallOutcome),
    /// About (DRAGON-177): toggle "Notify me when an update is available" — the
    /// persisted `notify_updates` setting that gates the launch-time update dialog.
    SetNotifyUpdates(bool),
    /// Land the settings window on the About page (a post-update relaunch, or a
    /// settings child spawned with `CCK_SETTINGS_TAB=about`), so the new
    /// version's "What's new" notes are immediately visible.
    ShowAboutPage,
    /// About (DRAGON-605): the About page became active on a build whose
    /// [`crate::update::notes_source`] is `OwnFetch` (a Flatpak), so fetch the release
    /// notes on their own. A build WITH an update channel already has them from its
    /// check and the arm is a no-op there. Sent only on About activation, once per
    /// session; never from a daemon, a capture launch or a settings-window mint.
    FetchReleaseNotes,
    /// About (DRAGON-605): the notes-only fetch resolved. `None` covers every failure
    /// (no network, no curl, malformed manifest, no notes in the release) and leaves the
    /// page exactly as it was: this build cannot update, so an update error would name a
    /// feature it does not have.
    ReleaseNotesFetched(Option<crate::update::ReleaseNotes>),
    /// Launch dialog (DRAGON-177): the "Don't remind me again" checkbox in the
    /// update dialog changed; carries its new state (applied to `notify_updates`
    /// when a dialog button is clicked).
    UpdateDialogRemindToggled(bool),
    /// Launch dialog (DRAGON-177): "Update Now" pressed — apply the checkbox to
    /// `notify_updates`, dismiss the dialog, and run the platform update flow (the
    /// macOS one-click install / the Linux release-page link).
    UpdateDialogNow,
    /// Launch dialog (DRAGON-177): "Update Later" pressed — apply the checkbox to
    /// `notify_updates` and dismiss the dialog for this session (no update action).
    UpdateDialogLater,
    /// Cloud Accounts page (DRAGON-482). One variant carrying that page's own sub-enum;
    /// see [`CloudSettingsMsg`] for why the page keeps its messages together.
    Cloud(CloudSettingsMsg),
}
