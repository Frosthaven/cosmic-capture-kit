//! Cloud Accounts settings page (DRAGON-482).
//!
//! The page a user connects a cloud drive on, and the small state machine that connecting
//! one needs. Everything here is either a PURE decision (which the tests at the bottom pin)
//! or a view; the effects live in `app::update::settings`'s `update_cloud`, which is the
//! only place that touches the network, the keyring or the accounts file.
//!
//! # What the page shows
//!
//! 1. An "Add cloud account" button, right-aligned in its row like every other settings
//!    control. It opens a dialog whose backdrop is INERT (the update dialog's shape, not the
//!    reset dialog's): the flow it starts hands control to a browser, and a stray click
//!    behind the card must not throw a half-finished sign-in away.
//! 2. Below it, one row per connected account: the provider's mark, the account's label, and
//!    where its uploads land, plus the per-account controls (Reconnect when the stored
//!    sign-in warrants it, Disconnect).
//!
//! # The folder browser, in one sentence (DRAGON-517)
//!
//! **Where the browser IS, is where the captures go.** The level shown above the list is this
//! account's destination: going into a folder chooses it, coming back out chooses the parent, and
//! Done is what writes the answer. There is no pick control, no per-row tick and no chosen-folder
//! state to hold, which is what three earlier passes at this step kept trying to reconcile.
//!
//! Two consequences worth knowing before editing anything here:
//!
//! * Opening the step WALKS back down to the account's saved folder
//!   (`cloud::providers::restore_browse`), because the browser has to be standing in the right
//!   place for that place to still be the destination.
//! * Nothing is written while browsing. Cancel therefore leaves the account exactly as it was
//!   found, and the one write that does still happen mid-dialog
//!   ([`crate::app::App::adopt_app_folder`]) is not about the browser at all.
//!
//! # What it never shows
//!
//! Anything about a token. A secret is never rendered, never copied to the clipboard, never
//! put in a tooltip, and never carried in a message. The connect flow stores tokens on the
//! background thread through [`crate::cloud::secrets`] and hands the UI back an account
//! record, which by construction holds no credential.
//!
//! # Why the provider list is not a `widget::dropdown`
//!
//! libcosmic's dropdown CAN draw an icon beside each name in both the closed control and the
//! open list, so that half of the requirement was never the problem. What it could not express
//! was a listed-but-unselectable entry, which the list needed while Proton Drive and iCloud
//! Drive were shown with a "Not available yet" caption. DRAGON-498 stopped showing them at all
//! (see [`crate::cloud::connectable_display_order`]: a picker offers what can be picked, and two
//! permanently dead rows read as a broken control), so that reason is gone. The popover stays,
//! because it is what draws the provider's MARK beside each name in the list the way the closed
//! control does, wearing the settings pill material; a dropdown's per-item icon support was
//! never the deciding factor and swapping the control now would be churn for its own sake.
//!
//! # Why nothing here uses `tokio::task::spawn_blocking` (DRAGON-497)
//!
//! Every background job this page starts runs on a DETACHED OS thread through
//! [`crate::app::off_thread`], never on the executor's blocking pool. This is not a style
//! choice, it is the difference between the settings window closing and the process wedging.
//! The mechanism, and the reason the pool cannot be used at all, is written down once in
//! `app::background`'s module doc (the helper lived here until DRAGON-499 gave the update
//! path the same treatment and the seam moved to a shared home).
//!
//! This page is the worst possible place for that wedge, because its longest job is one the
//! user is EXPECTED to walk away from: an abandoned sign-in keeps its loopback listener
//! polling for the full `oauth::BROWSER_DEADLINE` (five minutes), and
//! `CloudSettingsMsg::AddClose` says outright that a connect in flight cannot be aborted.
//! Closing the dialog stops caring about the reply; it does not stop the work. That is fine,
//! and it stays fine, precisely because the work is no longer something the exit path can
//! wait on.

use super::super::*;
use super::super::row::{
    Item, SectionSpec, action_button, danger_caption, standard_button_class,
    subtle_caption, warning_caption,
};
use crate::cloud::accounts::CloudAccount;
use crate::cloud::oauth::TokenSet;
use crate::cloud::{AuthKind, ProviderSpec};
// The browser step's copy button, its "Copied!" flash window and the accent icon-button style it
// wears all moved to `widgets::copy_button` in DRAGON-520, when the preview editor's upload meter
// grew a second copy control and the owner asked for ONE component rather than a lookalike. This
// step still owns its own `CloudAdd::copied_at` stamp, and its once-a-second
// `sub_cloud_browser_countdown` tick is still what redraws the view while the flash is up.
use crate::widgets::copy_button::{accent_icon_button_class, copied_recently, copy_button};

/// The destination shown for an account that never chose a folder INSIDE its app folder: the
/// app folder itself (DRAGON-498).
///
/// The provider's own name for that folder, not a description of it, so a row reads "OneDrive -
/// Cosmic Capture Kit" and points at something the user can actually go and open. It read "Main
/// folder" while an account's root was the whole drive, which is no longer a place this app can
/// write to on any provider.
pub const ROOT_DESTINATION: &str = crate::cloud::APP_FOLDER;

/// The provider list's width, matched by the closed control so the list opens exactly over
/// the button rather than growing sideways out of it.
const PROVIDER_MENU_W: f32 = 300.0;

/// The tallest the folder list gets before it scrolls inside the dialog.
const FOLDER_LIST_H: f32 = 220.0;

// ---------------------------------------------------------------------------
// Background work
// ---------------------------------------------------------------------------

/// How long the browser-URL relay waits for the sign-in page's address (DRAGON-497).
///
/// An OUTER ring, not an expectation: `connect_and_save` reports the URL within a
/// millisecond of the flow starting, so a relay still waiting when this elapses is one the
/// connect will never answer. It is the connect's own browser deadline for that reason,
/// which is the point past which the flow it belongs to has given up anyway.
///
/// There was no bound here at all before, just a bare `recv()`. The repo's rule is that a
/// wait gets its deadline in the same commit as the wait, and this one had been the single
/// unbounded channel wait in the app.
const URL_RELAY_BUDGET: std::time::Duration = crate::cloud::oauth::BROWSER_DEADLINE;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// The Cloud Accounts page's whole UI state, held by [`crate::app::SettingsState`].
///
/// The accounts list is CACHED here rather than read in the view: `accounts::list` is a file
/// read plus a TOML parse, and a view runs on every redraw. `Reload` refills it.
#[derive(Debug, Default)]
pub struct CloudPageState {
    /// The connected accounts, in file order.
    pub accounts: Vec<CloudAccount>,
    /// The ids whose stored sign-in warrants a Reconnect (see [`reconnect_warranted`]).
    /// Filled by the off-thread probe, so the keyring is never read on the UI thread.
    pub reconnect: Vec<String>,
    /// The add / reconnect dialog, or `None` when it is closed.
    pub add: Option<CloudAdd>,
    /// The account id awaiting a disconnect confirmation.
    pub pending_disconnect: Option<String>,
    /// The account id whose Disconnect is currently in flight, if any (DRAGON-489 follow-up):
    /// its row shows "Disconnecting..." and cannot be pressed again mid-flight. Set the
    /// moment `DisconnectConfirm` fires the async work (the confirm dialog itself closes
    /// immediately, so the ROW's own button is the only thing left on screen tracking this),
    /// cleared when `Disconnected` reports the SAME id back.
    ///
    /// A single slot rather than a set: two disconnects overlapping (a second account started
    /// before the first's result lands) is possible but rare, and the double-press this
    /// actually guards against, the same button pressed twice while its own network delete
    /// and keyring write are still in flight, only ever needs one slot.
    pub disconnecting: Option<String>,
    /// A page-level message (a failed disconnect, a failed save), shown under the button.
    pub notice: Option<String>,
    /// Bumped whenever the user cancels or closes the dialog; a background reply stamped
    /// with an older value is dropped. See `CloudSettingsMsg`'s doc for why a connect
    /// cannot simply be aborted.
    pub generation: u64,
}

impl CloudPageState {
    /// Re-read the accounts file. The cheap half of a refresh: no keyring, no network, so it
    /// is safe to call from the UI thread (the sign-in probe is the other half, and runs on
    /// a background task).
    ///
    /// Sorted for display through [`crate::cloud::accounts::sorted`] (DRAGON-495), the same
    /// comparator the preview editor's upload picker uses, so the two windows never disagree
    /// about the order of the same accounts.
    pub fn reload(&mut self) {
        self.accounts = crate::cloud::accounts::sorted(crate::cloud::accounts::list());
        // An id that is no longer listed cannot warrant anything.
        self.reconnect.retain(|id| self.accounts.iter().any(|a| &a.id == id));
    }

    /// Whether this account's row should offer Reconnect.
    pub fn warrants_reconnect(&self, id: &str) -> bool {
        self.reconnect.iter().any(|k| k == id)
    }

    /// Whether the add flow is currently waiting on the browser (DRAGON-489 follow-up).
    /// Drives `subscriptions.rs`'s once-a-second countdown tick, which cannot name
    /// `CloudAddStep` directly (`pages` is a private module of `settings`), so the predicate
    /// lives here instead.
    pub fn waiting_on_browser(&self) -> bool {
        matches!(self.add.as_ref().map(|a| &a.step), Some(CloudAddStep::Browser { .. }))
    }

    /// Whether a folder listing is in flight, so the path row's refresh glyph should be
    /// spinning (DRAGON-514). Drives `subscriptions.rs`'s ~30fps spin tick.
    ///
    /// Lives here for the same reason [`Self::waiting_on_browser`] does: `pages` is a private
    /// module of `settings`, so the subscription cannot reach into [`CloudAdd`] itself. An idle
    /// dialog (and a settings window with no dialog at all) ticks nothing.
    pub fn listing_folders(&self) -> bool {
        self.add.as_ref().is_some_and(|a| a.folder_loading)
    }
}

/// The add-account dialog's state.
#[derive(Debug)]
pub struct CloudAdd {
    /// Which provider is chosen, as an index into [`crate::cloud::registry`].
    pub provider: usize,
    /// Whether the provider list is open.
    pub menu_open: bool,
    /// Where the flow has got to.
    pub step: CloudAddStep,
    /// The account the connect saved, once tokens have landed. The folder step edits it.
    pub account: Option<CloudAccount>,
    /// The folders offered by the folder step (provider-side id, display name), all of them
    /// inside whatever level [`Self::trail`] currently names.
    pub folders: Vec<(String, String)>,
    /// How far INTO the app folder the list has been drilled (DRAGON-506): one (id, display
    /// name) per level below it, outermost first. Empty means the list is showing the app
    /// folder itself, which is where every dialog opens.
    ///
    /// The app folder is deliberately NOT an entry here. Its id lives in [`Self::root`] (and is
    /// `None` on the two providers whose token root IS it), and its NAME comes from the registry
    /// at render time, so a trail entry for it would be a second, staler copy of both. The
    /// breadcrumb draws it as the first crumb regardless; see [`breadcrumb`].
    pub trail: Vec<(String, String)>,
    /// The inline "new folder" row's typed name, or `None` while the row is closed
    /// (DRAGON-506). Buffered raw, like the account's name field: validation happens on submit,
    /// through [`create_refusal`].
    pub creating: Option<String>,
    /// A create is in flight, so the row shows it and cannot be submitted twice.
    pub create_busy: bool,
    /// The folder (id, display name) a delete confirmation is open for, or `None`
    /// (DRAGON-506). The folder itself rather than an index into [`Self::folders`], so a listing
    /// that lands while the confirmation is up cannot move the confirmation onto a different
    /// folder than the one it names.
    pub pending_delete: Option<(String, String)>,
    /// A delete is in flight.
    pub delete_busy: bool,
    /// The account's app folder, as the provider names it (id, display name), when the
    /// provider needed one found or created (DRAGON-498; Google Drive today). `None` where the
    /// token itself is confined to that folder, which is where "no folder chosen" already means
    /// the app folder.
    ///
    /// This is what the browser saves when it is showing the app folder ITSELF (an empty
    /// [`Self::trail`]): writing nothing there must not clear the folder to nothing on a
    /// provider where nothing means the whole drive. See [`viewed_destination`].
    pub root: Option<(String, String)>,
    /// Whether the browser has ever actually shown a level (DRAGON-517).
    ///
    /// Set by the first listing that LANDS, and never cleared: from then on [`Self::trail`] and
    /// [`Self::root`] describe a real place, so Done can write it as the account's destination.
    ///
    /// Until then there is no level to save, and Done must leave whatever the account already
    /// stores alone. Without this flag a step whose listing failed would read as "the browser is
    /// at the app folder", and a Drive account with a destination three levels down would have it
    /// silently reset by a user who opened the gear, saw an error and pressed Done.
    pub browsed: bool,
    /// A message from the folder listing, when it could not be fetched. Not fatal: the
    /// account is already saved and defaults to its own app folder.
    pub folder_note: Option<String>,
    /// The folder listing is in flight.
    pub folder_loading: bool,
    /// Whether ANY folder listing has landed yet THIS DIALOG SESSION, success or failure
    /// (owner report, live testing). Starts `false` in every constructor and flips to `true`
    /// for good the moment [`CloudSettingsMsg::FoldersLoaded`] or
    /// [`CloudSettingsMsg::FolderViewRestored`] lands, whichever the fetch was.
    ///
    /// The one thing [`breadcrumb_first_load`] reads besides [`Self::folder_loading`]: while
    /// this is still `false` AND a listing is in flight, the dialog just opened and has never
    /// shown a real breadcrumb, so `breadcrumb_row` shows "Loading..." instead. Every later
    /// fetch (a refresh press, a descent, an ascent) also sets `folder_loading`, but by then
    /// this is already `true`, so those read as the ordinary case: the real path, with just the
    /// refresh icon spinning.
    ///
    /// **Never reset in place.** It does not need to be: `add` itself is torn down and rebuilt
    /// fresh (`CloudAdd::new`/`reconnecting`/`edit_account`) on every open of this dialog
    /// (`AddOpen`, `Reconnect`, `EditAccount`) and dropped on every way it closes (`SetupDone`,
    /// `CancelSetupStep`, `AddClose`), so "first load this session" is already bounded by the
    /// struct's own lifetime; a stale `true` surviving a close would need this field to outlive
    /// its own `CloudAdd`, which nothing here does.
    pub has_loaded_once: bool,
    /// The path row's refresh glyph's rotation, in radians (DRAGON-514).
    ///
    /// Advanced by [`CloudSettingsMsg::FolderSpinTick`] and read by `folder_refresh_button`,
    /// which spins the icon while (and only while) [`Self::folder_loading`] is set. That spin is
    /// the ONLY indication an ORDINARY (not first-ever) listing is running: `breadcrumb_row`'s
    /// "Loading..." text is the one exception, gated on [`Self::has_loaded_once`] rather than
    /// living here.
    ///
    /// An angle rather than a start-instant, mirroring the app's other spinning glyph
    /// (`App::scan_spin`, the scanner's re-read button): it needs no reset when a listing
    /// starts, because where the wheel happens to be pointing carries no meaning.
    pub folder_spin: f32,
    /// Set when this dialog is RECONNECTING an existing account rather than adding one, so
    /// the new tokens land under the same id and the folder / visibility choices survive.
    pub reconnect_of: Option<CloudAccount>,
    /// When the Browser step began (DRAGON-489 follow-up), so `cloud_browser_step` can show a
    /// live countdown against [`crate::cloud::oauth::BROWSER_DEADLINE`]. `None` outside that
    /// step. Set once, in `start_cloud_connect`, the single place that step is entered.
    pub browser_started: Option<std::time::Instant>,
    /// Whether this dialog is adding a BRAND NEW account this session, as opposed to
    /// reconnecting or editing one that already existed before the dialog opened (DRAGON-489
    /// follow-up). This, not [`Self::reconnect_of`], is what [`CloudSettingsMsg::CancelFolderStep`]
    /// checks: only a fresh connect has a just-created account worth undoing on Cancel. Set
    /// `true` only by [`Self::new`].
    pub is_fresh_connect: bool,
    /// When the browser step's sign-in URL was last copied to the clipboard (DRAGON-489
    /// follow-up), so `cloud_browser_step` can flash the copy button to a "Copied!" tick for
    /// [`COPIED_FLASH`] and revert on its own. `None` until the first copy. Mirrors
    /// [`Self::browser_started`]: a timestamp the view reads against `Instant::now()`, needing
    /// no explicit clear because the Browser step's once-a-second tick already rebuilds the
    /// view (see `sub_cloud_browser_countdown`).
    pub copied_at: Option<std::time::Instant>,
}

impl CloudAdd {
    /// A fresh dialog for adding an account, starting on the first CONNECTABLE provider.
    pub fn new() -> Self {
        Self {
            provider: first_connectable(),
            menu_open: false,
            step: CloudAddStep::Provider,
            account: None,
            folders: Vec::new(),
            trail: Vec::new(),
            creating: None,
            create_busy: false,
            pending_delete: None,
            delete_busy: false,
            root: None,
            browsed: false,
            folder_note: None,
            folder_loading: false,
            has_loaded_once: false,
            folder_spin: 0.0,
            reconnect_of: None,
            browser_started: None,
            is_fresh_connect: true,
            copied_at: None,
        }
    }

    /// A dialog that reconnects `account`, with its provider pre-selected and locked.
    pub fn reconnecting(account: CloudAccount) -> Self {
        let provider = crate::cloud::registry()
            .iter()
            .position(|p| p.id == account.provider)
            .unwrap_or_else(first_connectable);
        Self { provider, reconnect_of: Some(account), is_fresh_connect: false, ..Self::new() }
    }

    /// A dialog that jumps straight to the setup step for an ALREADY-connected `account` (the
    /// owner's "change the folder setup again" ask, and since DRAGON-495 the RENAME path too),
    /// with no OAuth involved at all: the account is already authorized, so this only re-lists
    /// its folders and lets the user rename it or pick a different destination. Not a reconnect
    /// (`reconnect_of` stays `None`, since nothing here refreshes tokens) and not a fresh
    /// connect (`is_fresh_connect` stays `false`, since Cancel must not disconnect an account
    /// that already worked before this dialog opened).
    ///
    /// The name field is prefilled from the account's own label, which is simply the label
    /// already on the record this carries; a blank one (a legacy account, a hand-edited file)
    /// commits [`default_label`] rather than staying blank, exactly as a fresh connect does.
    ///
    /// **The trail starts EMPTY and is filled in when the restore lands** (DRAGON-517). The
    /// browser re-opens INSIDE the account's saved destination, but which folders those are is a
    /// question only the provider can answer, so the dialog opens on the app folder's crumb with
    /// the refresh glyph spinning and walks down as
    /// [`crate::app::App::start_edit_account`]'s reply arrives. See
    /// [`crate::cloud::providers::restore_browse`].
    pub fn edit_account(account: CloudAccount) -> Self {
        let provider = crate::cloud::registry()
            .iter()
            .position(|p| p.id == account.provider)
            .unwrap_or_else(first_connectable);
        let browse = account.spec().is_some_and(chooses_folder);
        Self {
            provider,
            step: CloudAddStep::Setup,
            account: Some(account),
            folder_loading: browse,
            is_fresh_connect: false,
            ..Self::new()
        }
    }

    /// The chosen provider's spec, or `None` if the index has somehow gone out of range.
    pub fn spec(&self) -> Option<&'static ProviderSpec> {
        crate::cloud::registry().get(self.provider)
    }

    /// The folder the list is currently showing the inside of, as the provider addresses one
    /// (DRAGON-506), or `None` for the app folder itself.
    ///
    /// This is exactly `providers::browse_folders`' `parent`, and `None` really does have to
    /// mean the app folder rather than "the root's id": only that call knows how to FIND or
    /// create the root, and only it should, once per dialog rather than once per level.
    pub fn level_parent(&self) -> Option<String> {
        self.trail.last().map(|(id, _)| id.clone())
    }

    /// The levels below the app folder, by display name, outermost first. What a chosen
    /// destination is stored as, and what the delete rules compare paths with.
    pub fn level_names(&self) -> Vec<&str> {
        self.trail.iter().map(|(_, name)| name.as_str()).collect()
    }
}

impl Default for CloudAdd {
    fn default() -> Self {
        Self::new()
    }
}

/// Where the add dialog has got to.
///
/// One enum rather than a set of booleans: the steps are mutually exclusive, and every
/// transition between them is a named pure function below, so a state the dialog can reach
/// is a state the tests have seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudAddStep {
    /// Choosing which drive to connect.
    Provider,
    /// Waiting for the user to sign in, in their own browser. `url` is the sign-in page's own
    /// address, filled in moments after the step begins (DRAGON-489): `None` for the first
    /// frame or two, before the background task reports it back. Shown as a clickable, copyable
    /// link rather than opened automatically: an automatic launch can fail silently (no default
    /// browser set, the wrong browser opens, a headless/remote session), and a failed automatic
    /// open with nothing else on screen is a dead end.
    Browser { url: Option<String> },
    /// The attempt ended without tokens.
    Failed {
        /// What to tell the user. Always a sentence, never a raw error dump.
        message: String,
        /// Whether another attempt could plausibly succeed. `false` for the two refusals
        /// that are facts about the build or the provider rather than about this attempt.
        retry: bool,
    },
    /// Tokens landed; setting the account up (its name, and where its uploads go).
    ///
    /// **Unconditional since DRAGON-495.** It was the FOLDER step, reached only where the
    /// provider could list folders, which meant a YouTube connect finished with no step at all
    /// and no way to name the account (the owner's report). Naming applies to every provider,
    /// so the step now always follows a fresh connect and the folder controls are what comes
    /// and goes inside it (see [`chooses_folder`]).
    Setup,
}

/// How the page treats a failure that came back from a cloud call.
///
/// Named `ReconnectHint` rather than `CloudFailure` (DRAGON-482) because this app already has
/// exactly ONE failure vocabulary, `diag::Failure`, and CLAUDE.md's rule is that there is
/// never a second one. A type spelled `Failure::` reads like the start of that second
/// vocabulary at every call site and in every grep, when what it actually answers is much
/// narrower: given a message, does the page offer Reconnect or Try again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconnectHint {
    /// The provider has forgotten this authorization: offer Reconnect, not Try again.
    NeedsReconnect(String),
    /// An ordinary failure. Another attempt is worth offering.
    Retry(String),
}

impl ReconnectHint {
    /// The sentence to show. Pure; unit-tested.
    ///
    /// The message is returned WHOLE, prefix included, because this one goes on the settings
    /// page next to a Reconnect button and the prefix is the button's label in sentence form.
    /// The surface with no such button (the upload child's banner) strips it instead; see
    /// `cloud::oauth::reconnect_reason`.
    pub fn message(&self) -> &str {
        match self {
            ReconnectHint::NeedsReconnect(m) | ReconnectHint::Retry(m) => m,
        }
    }
}

/// What pressing Connect should do for the chosen provider.
///
// ── Tombstone: `ConnectAction::NoClientId` (DRAGON-508) ──────────────────────────────────
//
// A third variant stood here from DRAGON-482 until DRAGON-508: "this build carries no sign-in
// id for this provider", carrying `resolve_client_id`'s sentence naming the environment
// variable to set. It had a whole path behind it: `provider_notes` drew a caption under the
// picker saying the build had no registration for the chosen provider, and `connect_pressable`
// deliberately kept Connect LIVE so a press could show the full instructions (the owner had
// reported an inert button as a broken dialog, which it was).
//
// It is gone because the state it described is no longer reachable from the picker. The picker
// now lists a provider if and only if its client id RESOLVES (`cloud::provider_available`), so
// a provider with no id is never offered, never chosen, and never pressed. The on-ramp that
// caption used to be moved with it, and became the dialog's EMPTY STATE (`cloud_empty_state`):
// one message and one link to the setup guide, shown when NOTHING resolves, instead of the
// same explanation repeated once per provider behind a control that could only fail.
//
// What is left is `Refused`, which now covers both ways a connect cannot start: a provider
// with no public API, and (reachable only through a reconnect on an account whose variable is
// no longer set) one this build cannot resolve an id for. Do not re-add a second variant to
// tell those two apart on screen: the sentence already differs, and the dialog's shape does
// not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectAction {
    /// Nothing, with the reason: the provider has no public API to connect to, or this build
    /// cannot resolve a client id for it.
    Refused(String),
    /// Run the flow.
    Run,
}

/// The setup guide this app points at when it has no cloud provider configured (DRAGON-508).
///
/// The documentation site's own copy of `CLOUD_ACCOUNTS.md` (the DRAGON-509 site: the page is
/// generated from that file, so the app and the repo cannot drift apart on what the steps are).
/// A compile-time constant, so nothing user-supplied or provider-supplied ever reaches the OS
/// opener through this path and no host check is needed.
pub const CLOUD_GUIDE_URL: &str = "https://cck.thedragon.dev/cloud-accounts/";

// ---------------------------------------------------------------------------
// The pure decisions
// ---------------------------------------------------------------------------

/// The registry index of the first provider the picker OFFERS.
///
/// The dialog opens on it rather than on index 0 so the first thing a user sees is something
/// they can press Connect on, however the registry is later reordered. Read through
/// [`crate::cloud::connectable_display_order`], so "first" keeps meaning the first entry of the
/// list the user is actually looking at: sorted by brand since DRAGON-495, and carrying only
/// what this build can actually connect since DRAGON-508.
///
/// Falls back to index 0 when the list is EMPTY (a build with no provider configured at all).
/// Nothing renders that provider in that case: the dialog shows [`shows_empty_state`]'s empty
/// state instead of the picker, so this is only ever the index a `CloudAdd` happens to hold.
pub fn first_connectable() -> usize {
    crate::cloud::connectable_display_order().first().map_or(0, |(index, _)| *index)
}

/// Whether the add dialog should show its EMPTY STATE instead of a provider picker. Pure;
/// unit-tested.
///
/// True only for a build where NOTHING resolves (`offered` is 0) on a fresh add. A RECONNECT is
/// excluded on purpose: it already knows its provider, it is fixing one specific account, and
/// the honest answer there is that account's own refusal sentence, not a general "this build
/// has no cloud providers" page.
pub fn shows_empty_state(reconnecting: bool, offered: usize) -> bool {
    !reconnecting && offered == 0
}

/// The captions the add dialog shows under the provider control, in order. Pure; unit-tested.
///
/// Everything here is a fact the user would otherwise discover by pressing a button and being
/// told no, which is the one thing this dialog has already been reported for. `env_value` is
/// injected exactly as [`connect_action`] takes it, so the whole thing is testable without
/// touching the environment.
///
/// A refused provider says ONLY why it cannot be connected, in [`connect_action`]'s own words,
/// and a connectable one says nothing at all. Unreachable from the picker, which lists only
/// what this build can connect (DRAGON-508): the one way in is a RECONNECT on an account whose
/// provider this build cannot resolve, either because the account names an unofficial provider
/// (a hand-edited file naming `proton`) or because the variable that supplied its id is no
/// longer set. Both are real, and both deserve the sentence rather than a dead dialog.
///
/// The second line this used to carry, a short "this build has no X app registration yet" for a
/// provider with no client id, went with `ConnectAction::NoClientId` (see its tombstone). It
/// was an on-ramp printed once per provider; the dialog's empty state is that on-ramp now, said
/// once.
///
/// A provider used to get a line here for "supports browser sign-in only", when it had no
/// device-code fallback. The device-code flow itself is gone now (DRAGON-489 follow-up:
/// Google's needed a client type this app doesn't register, and Microsoft's was redundant
/// with the link-based browser flow), so every provider is "browser sign-in only" and the
/// line would just be noise on every dialog; it stays gone.
pub fn provider_notes(spec: &ProviderSpec, env_value: Option<&str>) -> Vec<String> {
    match connect_action(spec, env_value) {
        ConnectAction::Refused(message) => vec![message],
        ConnectAction::Run => Vec::new(),
    }
}

/// Whether the dialog's Connect button takes a press. Pure; unit-tested.
///
/// **A dead button reads as broken, not as "not this provider".** The first build drew Connect
/// inert whenever the flow could not run, on the theory that an unpressable button was a
/// quieter way of saying no; the owner tested it and reported the dialog as broken, which is
/// exactly what an unlabelled dead control means to anyone who did not write it.
///
/// That lesson is why this function still exists, and why the answer moved (DRAGON-508). It was
/// answered by keeping a hopeless Connect LIVE so a press could at least explain itself. It is
/// answered now by never showing the user a hopeless Connect: the picker offers only providers
/// whose client id resolves ([`crate::cloud::provider_available`]), and a build with none shows
/// the empty state instead of the dialog's controls. So [`ConnectAction::Refused`] is inert
/// again, and it is once more the guard for a state the picker does not offer rather than a
/// path a user can walk into. It is reachable only through a RECONNECT, where the provider is
/// fixed and named, and where [`provider_notes`]'s line under the control has already said
/// everything a press could.
pub fn connect_pressable(action: &ConnectAction) -> bool {
    !matches!(action, ConnectAction::Refused(_))
}

/// What pressing Connect should do. Pure; unit-tested.
///
/// `env_value` is the provider's `client_id_env` as the process sees it, injected rather
/// than read here so the decision is testable without touching the environment (the house
/// `foo_with` seam). The refusal is decided BEFORE anything is opened: a browser window
/// that appears only to fail is worse than a sentence that says why it will.
///
/// The id question is asked through the SAME resolution `crate::cloud::provider_available`
/// asks (`oauth::resolve_client_id`, DRAGON-508), so a provider the picker offers is always one
/// this can run, and there is no way for the list and the button to disagree.
pub fn connect_action(spec: &ProviderSpec, env_value: Option<&str>) -> ConnectAction {
    let AuthKind::OAuthPkce { client_id_env, baked_client_id, .. } = spec.auth else {
        return ConnectAction::Refused(format!(
            "{} has no public API for other apps to upload through, so it cannot be \
             connected.",
            spec.display_name
        ));
    };
    match crate::cloud::oauth::resolve_client_id(
        env_value,
        baked_client_id,
        spec.display_name,
        client_id_env,
    ) {
        Ok(_) => ConnectAction::Run,
        Err(message) => ConnectAction::Refused(message),
    }
}

/// Whether this provider's setup step offers a destination folder at all. Pure; unit-tested.
///
/// Straight from the capability table, as every screen must be: a provider whose API can list
/// folders gets the list, and one that cannot (YouTube) gets a step carrying only the account's
/// name.
///
/// # The typed-path axis, and why it is gone (DRAGON-498)
///
/// This used to return a `FolderStep { browse, typed_path }`, where `typed_path` was keyed on
/// the provider id (`spec.id == "dropbox"`) and put a free-text field on the step, because a
/// Dropbox folder IS a path string and could therefore be typed. That whole axis is deleted,
/// not disabled: with every provider confined to one app folder, a typed path can only name a
/// subfolder the list already offers, and a text box that looks like it can address the rest of
/// a drive it cannot reach is worse than no text box. It was also the one place a user could
/// name a destination this app had never listed, which is exactly the escape hatch the uniform
/// model closes. Nothing keyed on a provider id is left on this page.
pub fn chooses_folder(spec: &ProviderSpec) -> bool {
    spec.caps.folder_browse
}

/// The step to move to once tokens land. Pure; unit-tested.
///
/// `None` closes the dialog, and a RECONNECT is the ONLY thing that produces it: the account
/// already has its name and its destination, and re-asking is a chance to lose either.
///
/// **A fresh connect always gets the step** (DRAGON-495). It used to be skipped for a provider
/// with no folder concept, on the reasoning that there was nothing left to ask; that stopped
/// being true when the step gained the account's NAME, which every provider has. YouTube is the
/// case that made it visible: it connected and closed, leaving a row nobody could rename.
///
/// `spec` is kept in the signature although nothing reads it today: the decision is
/// per-provider by nature (the step's CONTENTS still are, see [`chooses_folder`]), and a caller
/// that already has the spec in hand should not have to rediscover that when the next provider
/// needs its own arm here.
pub fn step_after_tokens(spec: &ProviderSpec, is_reconnect: bool) -> Option<CloudAddStep> {
    let _ = spec;
    if is_reconnect {
        return None;
    }
    Some(CloudAddStep::Setup)
}

/// Classify a failure that came back from a cloud call. Pure; unit-tested.
///
/// The one place [`crate::cloud::oauth::RECONNECT_PREFIX`] is read on this page, so the
/// connect step, the folder step and the account rows all agree on what "the provider has
/// forgotten this authorization" looks like.
pub fn classify_failure(message: &str) -> ReconnectHint {
    if crate::cloud::oauth::needs_reconnect(message) {
        ReconnectHint::NeedsReconnect(message.to_string())
    } else {
        ReconnectHint::Retry(message.to_string())
    }
}

/// The step a failed connect lands on. Pure; unit-tested.
///
/// Every failure the flow itself can produce (denied, timed out, no network, a provider
/// error) is worth another attempt, so `retry` is true; the two refusals that are NOT
/// ([`ConnectAction::Refused`] and [`ConnectAction::NoClientId`]) never reach here, because
/// they are decided before the flow starts.
pub fn failure_step(message: &str) -> CloudAddStep {
    CloudAddStep::Failed { message: classify_failure(message).message().to_string(), retry: true }
}

/// The label a freshly connected account gets. Pure; unit-tested.
///
/// `reported_user` is whatever identity the token exchange handed back. Today that is always
/// `None`: [`crate::cloud::oauth::ConnectedTokens`] carries the provider and the tokens and
/// nothing else, and none of the three providers' token responses are parsed for an identity
/// (asking for one would mean asking for an identity SCOPE we deliberately do not request).
/// The argument exists so the day one is available, one call site changes and this decision
/// does not have to be rediscovered.
pub fn derive_label(reported_user: Option<&str>, display_name: &str, date: &str) -> String {
    match reported_user.map(str::trim).filter(|u| !u.is_empty()) {
        Some(user) => user.to_string(),
        None => format!("{display_name} ({date})"),
    }
}

/// The date part of an account's `added_at` stamp. Pure; unit-tested.
///
/// `added_at` is RFC 3339 (see [`CloudAccount`]), whose first ten characters ARE the date in
/// exactly the shape [`derive_label`] wants. Anything else (an empty stamp, a hand-edited file)
/// has no date to offer and reports `None`, so the caller supplies today's instead. Checked
/// rather than sliced blindly: this string comes off disk, and `YYYY-MM-DD` is the only shape
/// a name should be built from.
fn added_on(added_at: &str) -> Option<&str> {
    let date = added_at.get(..10)?;
    let bytes = date.as_bytes();
    let shaped = bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes.iter().enumerate().all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit());
    shaped.then_some(date)
}

/// The name an account carries when the user has not chosen one. Pure; unit-tested.
///
/// The SAME default the connect flow assigns ([`derive_label`]), rebuilt from what the account
/// itself remembers, so clearing the name field restores the name the account was born with
/// rather than leaving a blank row. `today` is injected (the house `foo_with` seam) and is used
/// only by an account whose stamp says nothing; a real one dates its default from when it was
/// actually added, so the default is stable however long after the fact it is asked for.
///
/// An account whose provider is not in the registry (a file written by a newer build) falls
/// back to its raw provider id, which is what [`CloudAccount::display_label`] does with the
/// same case.
pub fn default_label(account: &CloudAccount, today: &str) -> String {
    let display = account.spec().map_or(account.provider.as_str(), |spec| spec.display_name);
    derive_label(None, display, added_on(&account.added_at).unwrap_or(today))
}

/// The label to SAVE from the name field. Pure; unit-tested.
///
/// A blank field is not a name (DRAGON-495). It used to be stored as an empty string, on the
/// reasoning that [`CloudAccount::display_label`] renders an empty label as the provider's own
/// name anyway; that is true, and it is still the wrong thing to WRITE, because two accounts on
/// the same provider then become two identical rows with nothing to tell them apart. So a blank
/// field commits `default` (the provider default from [`default_label`]) instead.
///
/// Surrounding whitespace is trimmed off a real name too: a trailing space is invisible in the
/// field and would otherwise be the only difference between two accounts.
pub fn commit_label(typed: &str, default: &str) -> String {
    let typed = typed.trim();
    if !typed.is_empty() {
        return typed.to_string();
    }
    default.trim().to_string()
}

/// The post-connect step's heading. Pure; unit-tested.
///
/// The step asks two things at most, and only one of them is universal, so the heading names
/// whichever the provider actually has. "Configure your cloud storage" on a YouTube step (which
/// has no folder to choose) would promise a folder-configuration screen the dialog cannot
/// deliver.
pub fn setup_title(chooses_folder: bool) -> &'static str {
    if chooses_folder {
        "Configure your cloud storage"
    } else {
        "Name this account"
    }
}

/// The sentence under the post-connect step's heading. Pure; unit-tested.
///
/// Same split as [`setup_title`]: the folder providers explain where a capture lands, and a
/// provider with no destination to choose explains what the name is FOR, since otherwise a step
/// carrying one text field and nothing else looks like a step that failed to load.
///
/// The folder sentence names the SANDBOX (DRAGON-498), because that is the fact a user cannot
/// see from the list alone: the choice is not "anywhere in my drive", it is "the Cosmic Capture
/// Kit folder, or something inside it", and this app cannot reach past it on any provider.
///
/// **The owner's wording, verbatim**, latest revision. Earlier revisions named the real save
/// PATH (DRAGON-501), then named the folder via provider metadata (DRAGON-503, one commit
/// earlier than this one). This one is a fixed, generic phrase, "the App's Folder", with no
/// per-provider interpolation at all: [`crate::cloud::app_folder_name`] still exists and still
/// backs the folder ROW and the sandbox note below, it just no longer feeds this sentence.
///
/// The sandbox half of the old sentence moved out to [`setup_sandbox_note`], which is a
/// different KIND of statement (a limit, not an instruction) and now wears a different tone.
pub fn setup_body(spec: &ProviderSpec, chooses_folder: bool) -> String {
    if chooses_folder {
        return "Captures uploaded to this account are placed in the App's Folder.".to_string();
    }
    format!(
        "This {} account is connected. Give it a name you will recognise when you pick where \
         to upload.",
        spec.display_name
    )
}

/// What this provider calls the folder this app writes to. Pure; unit-tested.
///
/// Straight from [`crate::cloud::app_folder_name`], with the app's own constant as the fallback
/// for a provider that has no folder at all. [`setup_body`] used to call this for its folder
/// sentence; the owner's latest copy for that sentence is a fixed generic phrase with no
/// per-provider interpolation, so the only caller left is the test module's assertion that this
/// still agrees with [`crate::cloud::APP_FOLDER`] for every registry provider today. Kept (not
/// deleted) so that assertion, and any sentence that wants a real per-provider folder name again,
/// still has a single place to read it from.
#[cfg_attr(not(test), allow(dead_code))]
fn app_folder_name(spec: &ProviderSpec) -> &'static str {
    crate::cloud::app_folder_name(spec.id).unwrap_or(crate::cloud::APP_FOLDER)
}

/// The limit stated under [`setup_body`], for a step whose folder list renders (DRAGON-503, the
/// owner's wording). Pure; unit-tested.
///
/// Its own line, in the subtle tone, because it is not an instruction: the body says what to do
/// and this says what this app cannot reach whatever the user does. It took the place of
/// `folder_scope_note`, a Google-only caption explaining that `drive.file` let this app see only
/// folders it had created. That caption was written when Google was the ONLY sandboxed provider;
/// with every provider confined to one app folder (DRAGON-498) a warning on one row of three read
/// as Google being uniquely restricted, when the truth it described is now true everywhere. So
/// the fact is stated once, for every provider that has a folder, and the asymmetry is gone.
pub fn setup_sandbox_note(spec: &ProviderSpec, chooses_folder: bool) -> Option<String> {
    chooses_folder.then(|| {
        format!("This app cannot see or touch anything else in your {} storage.", spec.display_name)
    })
}

// DRAGON-514: `FOLDER_LOADING` ("Reading your folders...") lived here and is GONE, with no
// replacement string anywhere. The owner's rule is no loading TEXT at all: a sentence that
// appears and disappears is a layout change plus a thing to read, for news the user can already
// see. The refresh icon itself spins instead (see `folder_refresh_button`), which is the same
// control that started the listing, in the same place, at the same size, so nothing moves.

/// What the folder browser's REFRESH control is doing (DRAGON-503, re-aimed by DRAGON-514).
///
/// One control, three faces, because they are the same answer to "is the folder list current?"
/// and only one of them can be true at a time. It was a bare `Option<&str>` status line
/// (`setup_status`), which could only ever say "reading" or say nothing, so a list that had
/// finished left the slot empty and the user with no way to ask for it again.
///
/// It used to live in the dialog's FOOTER slot. It now rides the path row, immediately before
/// the create button (DRAGON-514), so the two controls that act on the level named beside them
/// are together, and the footer is back to Cancel and Done alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderSlot {
    /// A listing is in flight: the icon SPINS, in the subtle tone, and is not pressable, so a
    /// second listing cannot be started on top of it. No text, no second control, and no change
    /// to the row's size while it runs.
    Loading,
    /// The list is as fetched: a still, pressable refresh, for a user who has just made a folder
    /// at the provider and wants it offered here without closing the step and re-opening it.
    Idle,
    /// The last listing failed. The same still button, offering to TRY AGAIN rather than to
    /// refresh, since there is nothing on screen yet to refresh. The reason itself rides the
    /// existing error path (the step's `folder_note` warning), not this control.
    Failed,
}

/// What the folder browser's refresh control shows, or `None` when there is no folder list at
/// all. Pure; unit-tested.
///
/// Only a provider whose folders can be listed has anything here, so this is keyed on the folder
/// capability first: a folderless step must never claim to be reading folders it does not have
/// (DRAGON-495 made that step reachable for the first time), and must not offer to re-read them
/// either.
///
/// Loading OUTRANKS a failure, because a retry in flight is the newer fact: the note explaining
/// the old failure is cleared with it, and the spin is what says a retry is under way.
pub fn folder_slot(chooses_folder: bool, folder_loading: bool, failed: bool) -> Option<FolderSlot> {
    if !chooses_folder {
        return None;
    }
    Some(match (folder_loading, failed) {
        (true, _) => FolderSlot::Loading,
        (false, true) => FolderSlot::Failed,
        (false, false) => FolderSlot::Idle,
    })
}

/// Whether [`breadcrumb_row`] is in its FIRST-EVER-load state for this dialog session: the
/// dialog (or reconnect, or edit) just opened and no listing has landed yet, success or
/// failure. Pure; unit-tested.
///
/// True for exactly the one fetch [`CloudAdd::has_loaded_once`]'s doc names, never a later one:
/// that flag flips permanently the moment any fetch lands, so a refresh press or a descent into
/// a subfolder, both of which also set [`CloudAdd::folder_loading`], read as an ORDINARY
/// loading state (the real breadcrumb path, spinning refresh icon) rather than this one (owner
/// report, live testing: only the very first load should replace the path with "Loading...").
pub fn breadcrumb_first_load(folder_loading: bool, has_loaded_once: bool) -> bool {
    folder_loading && !has_loaded_once
}

// ── Tombstone: `level_is_chosen(level_id, root_id, chosen)` (DRAGON-517) ─────────────────────
//
// It answered "is the level the browser is inside the account's chosen destination?", so the path
// row's tick could be ON or OFF while the browser was somewhere the account did not upload to. It
// is gone because the question is gone: **the level the browser is showing IS the destination**
// (DRAGON-517), so the answer was always about to become a constant `true`, and a derived
// predicate that can only ever say one thing is a facade over state that no longer exists.
//
// The tick it drew was KEPT at the time, unconditional: `level_control` marked the level because
// the level is where uploads land, not because a separate pick happened to name it. The tick
// itself is gone now too (owner report, live testing): the label alone already states the fact,
// and DRAGON-517's "no per-row selection" model left no obvious second place to put a mark
// instead (see [`level_control`]). What used to be the SELECTION half of the retired rule (Drive
// stores the app folder's real id, the other two store nothing) survives whole in
// [`viewed_destination`], which is the one place a destination is now built.

/// The destination the browser's current level names: the folder's provider-side id, and the
/// path stored for it. Pure; unit-tested.
///
/// **The view IS the selection** (DRAGON-517). There is no chosen-folder state to consult: what
/// the browser is showing is what Done writes onto the account, so this takes the browser's own
/// two pieces of state and nothing else. It is deliberately not given the folder LISTING, which is
/// what makes a refresh incapable of moving the destination.
///
/// `trail` is the levels below the app folder, outermost first, and `root` is the app folder as
/// the provider names it (`CloudAdd::root`).
///
/// Two shapes come out, and the split is the DRAGON-498 rule this inherited from the deleted pick
/// handler:
///
/// * Inside a folder: that folder's own id, and the whole PATH below the app folder, because a
///   bare name cannot say WHICH "2026" is meant and [`crate::cloud::save_path`] renders every
///   level of it on the account's row.
/// * At the app folder itself: the root's id and name where the provider has one (Google Drive,
///   where the app folder is an ordinary folder this app made), and nothing at all on the two
///   providers whose token root already IS that folder. Writing nothing on Drive would mean the
///   whole of My Drive, which is the one destination the uniform model does not allow.
pub fn viewed_destination(
    trail: &[(String, String)],
    root: Option<&(String, String)>,
) -> (Option<String>, Option<String>) {
    if let Some((id, _)) = trail.last() {
        let levels: Vec<&str> = trail.iter().map(|(_, name)| name.as_str()).collect();
        return (Some(id.clone()), destination_path(&levels, None));
    }
    match root {
        Some((id, name)) => (Some(id.clone()), Some(name.clone())),
        None => (None, None),
    }
}

/// Whether an account should ADOPT the app folder it has just been told about. Pure;
/// unit-tested.
///
/// The DRAGON-498 rule, and all that is left of the old "what stays selected once a listing
/// lands" machinery: an account that has never named a destination takes the app folder as its
/// destination the first time this app learns the folder's id.
///
/// **It is not about the browser.** The browser's own level is written by Done
/// ([`viewed_destination`]); this covers the account that never gets there, because the user
/// connected it and then closed the settings window. On Google Drive an account with no
/// `folder_id` uploads to the whole of My Drive, so leaving it unadopted is captures landing loose
/// in someone's drive.
///
/// Only where the app folder HAS an id to store, which the caller checks by having a root at all:
/// on OneDrive and Dropbox the token root already is that folder, so `None` there already means
/// exactly this and there is nothing to write.
pub fn adopts_app_folder(stored_id: Option<&str>, stored_name: Option<&str>) -> bool {
    stored_id.map(str::trim).is_none_or(str::is_empty)
        && stored_name.map(str::trim).is_none_or(str::is_empty)
}

/// The glyph box for the path row's trailing controls (refresh and create).
///
/// 16px, matching what libcosmic's `button::icon` gives a SYMBOLIC handle
/// (`widget/button/icon.rs`: `icon_size: if handle.symbolic { 16 } else { 24 }`, and
/// `icons::handle` marks every lucide glyph symbolic). Pinned here because the loading
/// SPINNER is not a `button::icon` (it cannot be: rotation lives on iced's `svg` widget), so
/// the two faces would otherwise be sized by two different rules and the row would resize
/// every time a listing started.
///
/// **Re-checked directly against libcosmic's own layout code** (owner report, live testing: the
/// breadcrumb row's height was visibly shifting). This specific pairing is NOT what was wrong:
/// `Length::Fixed` ignores a rotated SVG's own intrinsic size (`iced::widget::Svg::layout`, the
/// `Length::Fixed` arm of `Limits::resolve` never reads the `rotated_size` it is handed), and
/// `button::icon`'s inner content is the identical `iced::widget::Svg` type at the identical
/// `Length::Fixed(16)`, wrapped in the same padding a bare `container` applies (`Padding::fit`
/// only ever SHRINKS a padding to fit, never grows one, so a zero outer padding stays zero).
/// The two faces really do land on the same 20x20 box. The actual bug was one level up, the
/// crumb-vs-icon-button height mismatch [`row_control_h`]'s doc and [`folder_refresh_button`]'s
/// now cover.
const PATH_ICON: f32 = 16.0;

/// The gap inside the path row's trailing PAIR: refresh, then create (DRAGON-514).
///
/// Deliberately smaller than the space between the crumbs and the pair (which is a
/// `Length::Fill` spacer): these two act on the level named beside them and read as one group,
/// which is the whole reason the refresh left the dialog's footer.
const PATH_ACTION_GAP: f32 = 6.0;

// ── Tombstone: `surviving_selection(chosen, chosen_depth, listing_depth, root, listed)`
// (DRAGON-517) ───────────────────────────────────────────────────────────────────────────────
//
// It decided which folder stayed SELECTED once a listing landed: keep a chosen folder the listing
// can still see, fall back to the app folder for one that had been deleted, and only ever judge a
// destination at the listing's own depth (DRAGON-503 + DRAGON-506). Every one of those cases
// existed because the selection could sit somewhere the browser was not looking.
//
// It is gone with that possibility. The browser's level IS the destination now, so a listing has
// no selection to preserve or discard: a refresh re-reads the folders inside the level and cannot
// move the level, and a level that has been deleted elsewhere walks the breadcrumb up one
// ([`listing_failure`], which already did exactly that and now moves the destination with it).
//
// Its one arm that was NOT about the browser survives as [`adopts_app_folder`]: an account that
// has never named a destination still takes the app folder the first time we learn its id.

// The WALK that re-opens the browser inside an account's saved destination
// (`cloud::providers::restore_browse`, DRAGON-517) is deliberately NOT here, unlike every other
// decision on this page. It composes `browse_folders` calls, level after level, so it lives beside
// that function, with its lister injected (`restore_browse_with`) so the whole walk is unit-tested
// there with no provider at all. This page only asks for it and assigns the answer; see
// [`folder_restore_task`].

/// The most crumbs the breadcrumb row draws before it elides the middle.
///
/// A dialog is a fixed width and a folder name is not, so a trail deep enough to run past the
/// card's edge has to lose something; what it loses is the levels a user is least likely to want,
/// the ones in the middle. Four keeps the app folder, the parent and the current level visible
/// with one to spare, which covers every trail anyone actually builds in a captures folder.
const MAX_CRUMBS: usize = 4;

const _: () = assert!(
    MAX_CRUMBS >= 3,
    "DRAGON-506: the breadcrumb must always fit the app folder, the elision and the level being \
     looked at, or the row cannot say where the user is"
);

/// One face of the breadcrumb row above the folder list (DRAGON-506).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Crumb {
    /// The app folder, always the first crumb and always pressable: it is the one level every
    /// provider has and the one every dialog opens on.
    Root,
    /// The levels between what is drawn, replaced by an ellipsis. INERT, because it names no
    /// single level to go to; the crumbs on either side of it do.
    Elided,
    /// A level of the trail, by its index (0 is the first folder descended into).
    Level(usize),
}

/// The crumbs to draw for a trail `depth` levels below the app folder. Pure; unit-tested.
///
/// Always starts at [`Crumb::Root`] and always ends at the level being looked at, because those
/// are the two a user reads: where this app is confined to, and where they are now. A trail too
/// long for [`MAX_CRUMBS`] loses its middle to one [`Crumb::Elided`], never its ends.
pub fn breadcrumb(depth: usize) -> Vec<Crumb> {
    let mut crumbs = vec![Crumb::Root];
    // `depth < MAX_CRUMBS` and not `depth + 1 <= MAX_CRUMBS`: the same test, and the one clippy
    // will accept. The count being compared is the root plus the levels, hence the off-by-one
    // that reads oddly here.
    if depth < MAX_CRUMBS {
        crumbs.extend((0..depth).map(Crumb::Level));
        return crumbs;
    }
    crumbs.push(Crumb::Elided);
    // The root and the elision have taken two of the slots; the rest go to the DEEPEST levels.
    crumbs.extend((depth - (MAX_CRUMBS - 2)..depth).map(Crumb::Level));
    crumbs
}

/// The longest a single crumb's text gets before it is shortened.
///
/// The elision in [`breadcrumb`] bounds how MANY crumbs the row draws; nothing bounds how wide
/// one of them is, and a folder name can be [`crate::cloud::providers::FOLDER_NAME_MAX`]
/// characters long. Four crumbs of this width still fit the card, and a name long enough to be
/// cut is one nobody reads to the end of anyway.
const MAX_CRUMB_CHARS: usize = 18;

/// One crumb's text, shortened if it has to be. Pure; unit-tested.
///
/// Cut by CHARACTERS, never by bytes: a byte slice through an accented name panics, and this
/// string comes from the user's own folder names. The ellipsis is one character for the same
/// reason the elided crumb's is, and it counts toward the budget, so the result is never wider
/// than the limit.
pub fn crumb_label(name: &str) -> String {
    if name.chars().count() <= MAX_CRUMB_CHARS {
        return name.to_string();
    }
    let kept: String = name.chars().take(MAX_CRUMB_CHARS - 1).collect();
    format!("{kept}\u{2026}")
}

/// The trail depth a crumb press moves to, or `None` when the press does nothing. Pure;
/// unit-tested.
///
/// `None` for two cases that must not start a round trip: the elision, which names no level, and
/// the crumb for the level already being shown, since pressing "you are here" is not navigation.
/// Everything else truncates the trail to that many levels, so a press on the app folder is a
/// press on `Some(0)`.
pub fn crumb_target(crumb: Crumb, depth: usize) -> Option<usize> {
    let target = match crumb {
        Crumb::Elided => return None,
        Crumb::Root => 0,
        Crumb::Level(index) => index + 1,
    };
    (target < depth).then_some(target)
}

/// The two lines the folder list shows when the level a user is standing in has nothing in it
/// (DRAGON-520, a second line added on the owner's later request). Subtle, and centred on BOTH
/// axes in the well's own space, so it reads as the absence of rows rather than as one: see the
/// render site (search `EMPTY_LEVEL_NOTE`) for why that means the note replaces the scrollable
/// rather than riding inside it.
pub const EMPTY_LEVEL_NOTE: [&str; 2] = [
    "No subfolders in this view.",
    "Add a folder with + or select done to use this folder.",
];

/// Whether the folder list shows [`EMPTY_LEVEL_NOTE`]. Pure; unit-tested.
///
/// An empty well is ambiguous: a level with no subfolders, a listing that has not landed yet and a
/// level whose only row is the one being typed all look the same. So the line appears for exactly
/// ONE of them, the settled empty level, and every other state keeps the well quiet:
///
/// * `loading`: the answer is not in yet. The refresh glyph is already spinning, and a line that
///   said "nothing here" and then filled with folders would have been a wrong answer confidently
///   given.
/// * `creating`: the inline new-folder row is open, so the list is not empty, it is being added
///   to. A create in flight keeps that row open, which is why this one flag covers both.
/// * `deleting`: a confirmation is up or a delete is in flight. The row it names is still on
///   screen, and the level's contents are about to change anyway.
pub fn shows_empty_level_note(
    folders: usize,
    loading: bool,
    creating: bool,
    deleting: bool,
) -> bool {
    folders == 0 && !loading && !creating && !deleting
}

/// What a failed folder listing does to the breadcrumb (DRAGON-506).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListingFailure {
    /// Say so on the step, and leave the list where it is. Every ordinary failure.
    Note,
    /// The folder being listed is not there any more: drop the last crumb and list its parent
    /// instead.
    AscendOne,
}

/// How the step answers a listing that failed. Pure; unit-tested.
///
/// **A level deleted somewhere else is not a broken dialog.** A user can delete a folder in their
/// browser while this step is open, and the next listing of it then reports that it is gone. The
/// honest answer is the one a file manager gives: step back out to the parent, which certainly
/// still exists, and show that instead. Failing the whole step would leave them looking at a
/// folder that no longer exists with no way back except closing it.
///
/// `reason` is the listing's own `Err`, and the only thing read out of it is
/// [`crate::cloud::providers::is_missing_folder`]'s marker, never the wording: user copy changes,
/// and a rule keyed on it would quietly stop working.
///
/// At depth 0 there is nothing to ascend to, so the answer is always a note. It is also not a
/// state the providers produce: the app folder is created on demand, so a listing of it reports
/// empty rather than missing.
///
/// **Ascending now moves the DESTINATION too** (DRAGON-517), because the level the browser shows
/// is the level Done saves. That is the honest outcome, and it is the reason a browser can never
/// be left pointing inside a folder that is gone: the same rule that keeps the view valid keeps
/// the destination valid.
pub fn listing_failure(depth: usize, reason: &str) -> ListingFailure {
    if depth > 0 && crate::cloud::providers::is_missing_folder(reason) {
        return ListingFailure::AscendOne;
    }
    ListingFailure::Note
}

/// The destination an account stores for a folder at this level: its path BELOW the app folder.
/// Pure; unit-tested.
///
/// `levels` are the folders drilled through, outermost first, and `child` is a folder listed
/// INSIDE them, or `None` for the level itself. `None` comes back only for the app folder, where
/// the caller applies the root rule instead (Drive stores the root's own id and name; the other
/// two store nothing, because there nothing already means the app folder).
///
/// A path rather than a bare name, because a name alone cannot say WHICH "2026" was picked once
/// the list drills down, and [`crate::cloud::save_path`] renders every level of it on the
/// account's row.
pub fn destination_path(levels: &[&str], child: Option<&str>) -> Option<String> {
    let mut parts: Vec<&str> = levels.to_vec();
    parts.extend(child);
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// Why a typed folder name cannot be created here, or `None` when it can. Pure; unit-tested.
///
/// Two checks, in this order. First the provider-side rules
/// ([`crate::cloud::providers::remote_folder_name`]), which every provider shares and which are
/// about what a name can BE. Then the one rule this level knows and the providers do not: a name
/// already taken by a folder in the list on screen. Two folders of one name is a picker nobody
/// can use, and Drive is happy to make one, so refusing here is what keeps the three providers
/// behaving the same way.
///
/// `siblings` are the display names at this level. Compared case-INSENSITIVELY: two of the three
/// providers treat names that way themselves, and "screenshots" beside "Screenshots" would be
/// two rows a user cannot tell apart on any of them.
pub fn create_refusal(typed: &str, siblings: &[&str]) -> Option<String> {
    let name = match crate::cloud::providers::remote_folder_name(typed) {
        Ok(name) => name,
        Err(message) => return Some(message),
    };
    siblings
        .iter()
        .any(|existing| existing.trim().eq_ignore_ascii_case(&name))
        .then(|| "There is already a folder with that name here.".to_string())
}

/// Why a listed folder cannot be deleted, or `None` when it can. Pure; unit-tested.
///
/// **Both refusals are about the account's own destination**, and both are decided before any
/// request is made, because a provider would carry either one out without complaint:
///
/// * The folder IS where this account uploads to. Deleting it would leave the account pointed at
///   something that no longer exists, and the next capture would fail with nothing on screen
///   explaining why.
/// * The folder CONTAINS that destination. Same outcome one level removed, and worse, because
///   the listing that would notice the destination had gone is a listing of a folder that is
///   itself gone, so nothing would ever move the selection back. Answered by path prefix, which
///   is what makes it answerable at all without walking the provider's tree.
///
/// The app folder is refused too, and it is refused by the caller rather than here: it is never a
/// row in the list, only ever the level the list is inside, so there is no press to check. Its
/// own rule is stated in [`crate::cloud::providers::delete_folder`]'s doc.
///
/// Paths are compared case-INSENSITIVELY, since Dropbox stores a lowercased path and OneDrive
/// treats names that way, so a case difference between two spellings of one path is not two
/// folders.
pub fn delete_refusal(candidate: &str, chosen: Option<&str>) -> Option<&'static str> {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return Some("This is the folder this app uploads to, so it cannot be deleted here.");
    }
    let chosen = chosen.map(str::trim).filter(|c| !c.is_empty())?;
    if chosen.eq_ignore_ascii_case(candidate) {
        return Some("This account uploads here, so pick another folder first.");
    }
    let inside = chosen.len() > candidate.len()
        && chosen[..candidate.len()].eq_ignore_ascii_case(candidate)
        && chosen.as_bytes().get(candidate.len()) == Some(&b'/');
    inside.then_some("This account uploads to a folder inside this one, so pick another folder first.")
}

/// The delete confirmation's heading. One sentence for every provider: what differs is what
/// deleting DOES, and that belongs in the body where there is room to say it.
pub const DELETE_TITLE: &str = "Delete this folder?";

/// The delete confirmation's body. Pure; unit-tested.
///
/// It names the folder, says that everything inside it goes too, and is keyed on
/// `recoverable` (`ProviderCaps::delete_recoverable`) rather than on a provider id, because the
/// two sentences are the honest answer for two different APIs: Graph moves the item to the
/// OneDrive recycle bin, where the user can put it back, while Drive deletes permanently and
/// Dropbox's restore window depends on the account's plan. Telling a Drive user their folder can
/// be restored would be the worst copy on this page.
pub fn delete_confirm_body(folder: &str, provider_name: &str, recoverable: bool) -> String {
    if recoverable {
        return format!(
            "{folder} and everything inside it will be moved to your {provider_name} recycle \
             bin. You can put it back from there if you need it."
        );
    }
    format!(
        "{folder} and everything inside it will be permanently deleted from your \
         {provider_name}. This cannot be undone."
    )
}

/// The tooltip on a connected account's gear (the owner's exact copy, DRAGON-495).
///
/// ONE sentence for every provider, deliberately not a per-provider one naming whichever of
/// rename / folder that provider actually offers: the button opens the account's setup step,
/// and what that step contains is the step's business, not the tooltip's.
pub const CONFIGURE_TOOLTIP: &str = "Configure this account";

/// Whether a stored sign-in warrants a Reconnect on the account's row. Pure; unit-tested.
///
/// Two states warrant it, and only these two: the tokens are GONE (nothing to sign in with),
/// or the access token is expiring and there is no refresh token to renew it from. Anything
/// else is a live account, and a Reconnect offered on one of those would read as a fault the
/// user has to act on.
pub fn reconnect_warranted(stored: Option<&TokenSet>, now: chrono::DateTime<chrono::Utc>) -> bool {
    let Some(tokens) = stored else { return true };
    tokens.refresh_token.is_none()
        && crate::cloud::oauth::needs_refresh(tokens.expires_at.as_deref(), now)
}

// A `destination_label(account)` used to sit here, answering the folder's NAME (its
// `folder_name`, or `ROOT_DESTINATION` when nothing was chosen). DRAGON-501 replaced it with
// `cloud::save_path`, which answers the real PATH: a name alone told a user "Screenshots" without
// saying where that is, and on the two providers whose folder is auto-provisioned it is two levels
// down. Its rule (a blank or missing name means the app folder itself, and never an empty
// destination) moved into the builder rather than being dropped, along with the collapse that
// makes Drive's stored root name read as the root; see `save_path`'s doc.

/// A connected account's row text: the provider, then where its uploads land, if anywhere.
/// Pure; unit-tested.
///
/// `destination` is `None` for a provider with NO folder concept, and then the row is just the
/// provider's name with NO dangling separator. This is the DRAGON-495 follow-up bug: every
/// row named a destination unconditionally, so a YouTube row read "YouTube - Main folder",
/// naming a folder that cannot exist. YouTube uploads to a channel; `caps.folder_browse` is
/// false for it, and [`chooses_folder`] is the SAME predicate the setup step already uses to
/// decide whether to show folder controls at all. Keying the row on the capability rather than
/// on a provider id is what keeps the row and the step from disagreeing.
///
/// The rule STANDS with DRAGON-501, which only changed what a destination is: the caller now
/// passes [`crate::cloud::save_path`]'s real path instead of a bare folder name, and that
/// function answers `None` for exactly the providers this arm was written for.
///
/// `sep` is what sits between the two halves: the visible subtitle passes `" - "`, the string
/// the global settings search indexes passes a plain `" "`, and BOTH have to disappear along
/// with the destination. Taking the separator as an argument is what keeps that rule in one
/// place instead of repeating the `Option` match at each call site.
pub fn account_subtitle(provider_name: &str, destination: Option<&str>, sep: &str) -> String {
    match destination {
        Some(dest) => format!("{provider_name}{sep}{dest}"),
        None => provider_name.to_string(),
    }
}

// A `folder_scope_note(provider_id)` used to sit here: a caption shown on Google's step alone,
// explaining that the `drive.file` scope let this app see only folders it had created. DRAGON-503
// deleted it rather than reworded it. It was written when Drive was the only sandboxed provider,
// and since DRAGON-498 every provider is confined to one app folder, so a caution on one row of
// three said the opposite of the truth: it read as Google being uniquely restricted, when what it
// described now holds everywhere. The fact it carried is stated for every folder provider by
// `setup_sandbox_note`. It was also the last thing on this page keyed on a provider id.

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// Wrap one of the page's own messages. Every button on this page goes through it, so the
/// three-deep `Msg::Settings(SettingsMsg::Cloud(..))` nesting is spelled once.
fn cm(msg: CloudSettingsMsg) -> Msg {
    Msg::Settings(SettingsMsg::Cloud(msg))
}

impl crate::app::App {
    pub(in crate::app::settings) fn cloud_sections(&self) -> Vec<SectionSpec<'_>> {
        let cloud = &self.settings.cloud;
        // `.flush()` (DRAGON-495): nothing ever follows this button, so the unit + reset slots
        // every ordinary settings row reserves left it ~60px short of the section's right edge,
        // visibly out of line with the account rows below it (which are `full_width` and never
        // reserved them). See `row::Item::flush`.
        let mut add_items = vec![
            // DRAGON-514: no description. It read "Upload a capture to a drive you have
            // connected, and copy the link to it.", which explains the FEATURE to someone
            // already on the page that configures it, under a section titled "Cloud Accounts",
            // above a button that says what to do. The empty `desc` is this file's idiom for a
            // row with no helper line, not a blank-string hack: `settings::mod`'s row builder
            // branches on `it.desc.is_empty()` and lays out the title alone, so nothing is
            // reserved and no gap is left (see also `about.rs` / `audio.rs`, which do the same).
            // The title stays searchable either way.
            Item::new(
                "Connect a cloud drive",
                "",
                action_button("Add cloud account", cm(CloudSettingsMsg::AddOpen)),
            )
            .flush(),
        ];
        if let Some(notice) = cloud.notice.as_deref() {
            add_items.push(Item::note(danger_caption(notice)));
        }
        let mut secs = vec![SectionSpec { title: "Cloud Accounts", items: add_items }];

        let mut rows: Vec<Item<'_>> = Vec::new();
        if cloud.accounts.is_empty() {
            // Ordinary body text, NOT `subdued_caption`: this line IS the section's content
            // when there is nothing else in it, and the owner reported the subdued grey as too
            // hard to read. Subdued is for a caption BESIDE something, where the thing it
            // annotates carries the eye. Here there is nothing else to look at.
            rows.push(Item::note(widget::text::body("No cloud accounts are connected yet.")));
        }
        for account in &cloud.accounts {
            rows.push(self.cloud_account_row(account));
        }
        // "Connected Accounts", title case, matching "Cloud Accounts" directly above it
        // (DRAGON-514, owner's call). This page's own first section is the precedent that
        // settles it; the two headings sit one above the other and were disagreeing.
        secs.push(SectionSpec { title: "Connected Accounts", items: rows });
        secs
    }

    /// One connected account's identity row: the provider's mark, the label, where its
    /// uploads land, and the per-account actions.
    ///
    /// A full-width row rather than the standard title/desc/control shape, because the
    /// standard one has no place for an icon and the provider's mark is what makes a list of
    /// three accounts readable at a glance. `full_width` keeps the title and description
    /// searchable, so the global settings search still finds an account by name.
    fn cloud_account_row<'a>(&'a self, account: &'a CloudAccount) -> Item<'a> {
        let label = account.display_label();
        let provider_name = account
            .spec()
            .map(|s| s.display_name.to_string())
            .unwrap_or_else(|| account.provider.clone());
        // The REAL SAVE PATH, not a folder name (DRAGON-501): "OneDrive - /Apps/Cosmic Capture
        // Kit/Screenshots" says where to go and look, where "OneDrive - Screenshots" left the
        // user to guess. ONLY a provider that actually has folders names one (see
        // `account_subtitle`), and an unknown provider (no spec) names none either: both fall
        // out of `save_path` answering `None`, which is keyed on the same registry table
        // `chooses_folder` reads and is pinned to agree with it in `cloud::registry_tests`.
        let destination = crate::cloud::save_path(&account.provider, account.folder_name.as_deref());
        let icon_name = account.spec().map_or("cloud-symbolic", |s| s.icon);
        let text_block = widget::column::with_capacity(2)
            .push(widget::text::body(label.clone()).font(cosmic::font::bold()))
            .push(widget::text::caption(account_subtitle(
                &provider_name,
                destination.as_deref(),
                " - ",
            )))
            .spacing(2.0)
            .width(Length::Fill);
        let mut actions = widget::row::with_capacity(3).spacing(8.0).align_y(Alignment::Center);
        if self.settings.cloud.warrants_reconnect(&account.id) {
            actions = actions
                .push(action_button("Reconnect", cm(CloudSettingsMsg::Reconnect(account.id.clone()))));
        }
        // The gear re-opens the setup step for this account with no reconnect: rename it, and
        // (where the provider has folders) change where its uploads land. UNCONDITIONAL since
        // DRAGON-495: it used to be gated on the provider having a folder concept, which left
        // YouTube rows with no edit affordance at all and therefore no way to be renamed. Every
        // account has a name, so every row has the button.
        actions = actions.push(edit_account_btn(account.id.clone()));
        // Accent, not destructive-red (DRAGON-489 follow-up, owner's pick): `Suggested` is
        // the built-in accent-filled class already worn by every affirmative action in this
        // file (Connect, Try again, Done), so this is the direct swap rather than a
        // hand-rolled style. While THIS account's disconnect is in flight, the button reads
        // "Disconnecting..." and drops its press handler so it cannot double-fire.
        let disconnecting = self.settings.cloud.disconnecting.as_deref() == Some(account.id.as_str());
        let disconnect_button = widget::button::suggested(if disconnecting {
            "Disconnecting..."
        } else {
            "Disconnect"
        });
        let disconnect_button = if disconnecting {
            disconnect_button
        } else {
            disconnect_button.on_press(cm(CloudSettingsMsg::Disconnect(account.id.clone())))
        };
        actions = actions.push(crate::widgets::arrow_cursor::arrow_cursor(disconnect_button));
        let row = widget::row::with_capacity(3)
            .push(crate::widgets::icons::sized(icon_name, 24.0))
            .push(text_block)
            .push(actions)
            .spacing(12.0)
            .align_y(Alignment::Center)
            .width(Length::Fill);
        Item::full_width(label, account_subtitle(&provider_name, destination.as_deref(), " "), row)
    }

    /// The add / reconnect dialog, stacked over the settings window.
    ///
    /// The backdrop is INERT (the update dialog's shape, DRAGON-177, not the reset dialog's):
    /// an outside click must not dismiss this one. The flow hands control to a browser, so a
    /// click on the settings window behind the card is exactly what happens when a user
    /// comes back from signing in, and dismissing there would throw the sign-in away.
    pub(in crate::app::settings) fn cloud_add_modal<'a>(
        &'a self,
        window: Element<'a, Msg>,
    ) -> Element<'a, Msg> {
        let Some(add) = self.settings.cloud.add.as_ref() else {
            return window;
        };
        let Some(spec) = add.spec() else {
            return window;
        };
        let card = match &add.step {
            CloudAddStep::Provider => self.cloud_provider_step(add, spec),
            CloudAddStep::Browser { url } => {
                let remaining = add
                    .browser_started
                    .map(|started| {
                        crate::cloud::oauth::BROWSER_DEADLINE.saturating_sub(started.elapsed()).as_secs()
                    })
                    .unwrap_or(crate::cloud::oauth::BROWSER_DEADLINE.as_secs());
                // The flash is read from a `CloudAdd` timestamp at render time, the same way
                // `remaining` is, so the copy button's "Copied!" tick reverts on the next
                // once-a-second tick with no state to clear.
                cloud_browser_step(spec, url.as_deref(), remaining, copied_recently(add.copied_at))
            }
            CloudAddStep::Failed { message, retry } => cloud_failed_step(spec, message, *retry),
            CloudAddStep::Setup => cloud_setup_step(add, spec),
        };
        let stacked = stack_dialog(window, card.max_width(560.0).into(), None);
        // The folder delete confirmation (DRAGON-506) is stacked over the step that raised it,
        // by handing the whole composition back in as this one's "window". The step underneath
        // keeps its INERT backdrop and this one gets a DISMISSING one, which is the same split
        // the add and disconnect dialogs already make: backing out of a destructive question is
        // always safe, and losing a half-finished sign-in to a stray click is not.
        let Some((_, name)) = add.pending_delete.as_ref() else {
            return stacked;
        };
        let card = widget::dialog()
            .title(DELETE_TITLE)
            .body(delete_confirm_body(name, spec.display_name, spec.caps.delete_recoverable))
            .primary_action(crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::destructive("Delete")
                    .on_press(cm(CloudSettingsMsg::FolderDeleteConfirm)),
            ))
            .secondary_action(crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::text("Cancel").on_press(cm(CloudSettingsMsg::FolderDeleteCancel)),
            ));
        stack_dialog(
            stacked,
            card.max_width(480.0).into(),
            Some(cm(CloudSettingsMsg::FolderDeleteCancel)),
        )
    }

    /// The disconnect confirmation. The RESET dialog's shape: a destructive primary, a text
    /// secondary, and a backdrop that DOES dismiss, because backing out of a destructive
    /// question is always safe.
    pub(in crate::app::settings) fn cloud_disconnect_modal<'a>(
        &'a self,
        window: Element<'a, Msg>,
    ) -> Element<'a, Msg> {
        let Some(id) = self.settings.cloud.pending_disconnect.as_ref() else {
            return window;
        };
        let label = self
            .settings
            .cloud
            .accounts
            .iter()
            .find(|a| &a.id == id)
            .map_or_else(|| "this account".to_string(), CloudAccount::display_label);
        let card = widget::dialog()
            .title("Disconnect this account?")
            .body(format!(
                "{label} will be removed from this computer, along with its sign-in. Files \
                 already uploaded stay where they are."
            ))
            .primary_action(crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::destructive("Disconnect")
                    .on_press(cm(CloudSettingsMsg::DisconnectConfirm(id.clone()))),
            ))
            .secondary_action(crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::text("Cancel").on_press(cm(CloudSettingsMsg::DisconnectCancel)),
            ));
        stack_dialog(
            window,
            card.max_width(480.0).into(),
            Some(cm(CloudSettingsMsg::DisconnectCancel)),
        )
    }

    /// The first step: choose a provider, then Connect.
    fn cloud_provider_step<'a>(
        &'a self,
        add: &'a CloudAdd,
        spec: &'static ProviderSpec,
    ) -> widget::Dialog<'a, Msg> {
        // Nothing to pick from at all (DRAGON-508): a build made from source, with no client id
        // baked in and none exported. The dialog says so once and points at the guide, rather
        // than drawing a picker whose every entry fails the same way.
        let offered = crate::cloud::connectable_display_order().len();
        if shows_empty_state(add.reconnect_of.is_some(), offered) {
            return cloud_empty_state();
        }
        // A reconnect has nothing to choose: the account already names its provider, and
        // changing it would make the new tokens belong to a different drive.
        let control: Element<'a, Msg> = if add.reconnect_of.is_some() {
            widget::row::with_capacity(2)
                .push(crate::widgets::icons::sized(spec.icon, 20.0))
                .push(widget::text::body(spec.display_name))
                .spacing(10.0)
                .align_y(Alignment::Center)
                .into()
        } else {
            provider_picker(add, spec)
        };
        let env_value = std::env::var(client_id_env(spec)).ok();
        let mut column = widget::column::with_capacity(3).push(control).spacing(8.0);
        for note in provider_notes(spec, env_value.as_deref()) {
            // NOT `subdued_caption`: that tone is a 78%-toward-background wash meant for an
            // inert icon at rest, not a sentence the user has to actually read (the missing
            // app-registration notice and its siblings were unreadably faint on the owner's
            // report). Same caption size, normal readable color.
            column = column.push(widget::text::caption(note));
        }
        let action = connect_action(spec, env_value.as_deref());
        // Pressable unless the provider has no public API at all; see `connect_pressable` for
        // why a build with no client id gets a LIVE button rather than a dead one.
        let button = widget::button::suggested("Connect");
        let connect: Element<'a, Msg> =
            crate::widgets::arrow_cursor::arrow_cursor(if connect_pressable(&action) {
                button.on_press(cm(CloudSettingsMsg::Connect))
            } else {
                button
            });
        let title = if add.reconnect_of.is_some() {
            "Reconnect this account"
        } else {
            "Add cloud account"
        };
        widget::dialog()
            .title(title)
            .body("Choose a drive, then sign in to it in your browser.")
            .control(column)
            .primary_action(connect)
            .secondary_action(crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::text("Cancel").on_press(cm(CloudSettingsMsg::AddClose)),
            ))
    }
}

/// The add dialog when this build has no cloud provider configured at all (DRAGON-508).
///
/// **What this replaced.** Every provider used to be listed whether or not this build could
/// connect it, each one repeating the same missing-registration caption under a Connect button
/// that could only explain itself when pressed. The list was four ways of saying one thing. So
/// the one thing is said once, here, and the picker went back to offering only what it can
/// offer.
///
/// The copy stays short and plain: what the state is, and where the steps are. It deliberately
/// names no provider and no environment variable, because the guide covers all of them and a
/// dialog is the wrong place for a per-provider setup procedure.
///
/// The guide opens through the app's ordinary open-URI seam, on a compile-time constant
/// ([`CLOUD_GUIDE_URL`]), so nothing user- or provider-supplied can ride this path to the OS
/// opener. `Close` is the primary action because it is the only one that finishes the dialog;
/// reading the guide is the step that happens elsewhere, in a browser, and the dialog has
/// nothing to wait for while it does.
///
/// The link is the accent `rich_text` span this file already uses for the browser step's
/// sign-in link, not a second kind of link control, so the two read as the same affordance.
fn cloud_empty_state<'a>() -> widget::Dialog<'a, Msg> {
    let accent = theme::accent(&cosmic::theme::active());
    let link = cosmic::iced::widget::rich_text([
        cosmic::iced::widget::span("Open the setup guide").color(accent).link(()),
    ])
    .on_link_click(|()| cm(CloudSettingsMsg::OpenGuide));
    widget::dialog()
        .title("No cloud drives are set up")
        .body(
            "This build has no cloud providers configured, so there is nothing to connect \
             yet. Setting one up means registering your own app with the provider, which is \
             free and takes a few minutes. The guide has the steps for each one.",
        )
        .control(link)
        .primary_action(crate::widgets::arrow_cursor::arrow_cursor(
            widget::button::suggested("Close").on_press(cm(CloudSettingsMsg::AddClose)),
        ))
}

/// The provider name for the environment variable that overrides its client id, or an empty
/// name for a provider that has none (which [`connect_action`] then reports as refused).
fn client_id_env(spec: &ProviderSpec) -> &'static str {
    match spec.auth {
        AuthKind::OAuthPkce { client_id_env, .. } => client_id_env,
        AuthKind::Unofficial => "",
    }
}

/// The closed provider control plus, when it is open, the list.
///
/// The popover WRAPPER is unconditional and only the POPUP comes and goes, the same shape
/// the preview editor's menus use: wrapping conditionally changes the widget tag at this
/// position, and iced answers a tag change by rebuilding the subtree.
fn provider_picker<'a>(add: &'a CloudAdd, spec: &'static ProviderSpec) -> Element<'a, Msg> {
    let face = widget::row::with_capacity(3)
        .push(crate::widgets::icons::sized(spec.icon, 20.0))
        .push(widget::text::body(spec.display_name).width(Length::Fill))
        .push(crate::widgets::icons::sized("pan-down-symbolic", 16.0))
        .spacing(10.0)
        .align_y(Alignment::Center)
        .width(Length::Fill);
    let anchor = crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::custom(face)
            .class(standard_button_class())
            .width(Length::Fixed(PROVIDER_MENU_W))
            .padding([8.0, 12.0])
            .on_press(cm(CloudSettingsMsg::AddProviderMenu(!add.menu_open))),
    );
    let mut picker = widget::popover(anchor);
    if add.menu_open {
        picker = picker
            .popup(provider_menu(add.provider))
            .position(widget::popover::Position::Bottom)
            .on_close(cm(CloudSettingsMsg::AddProviderMenu(false)));
    }
    picker.into()
}

/// The open provider list: every CONNECTABLE provider in BRAND order, each with its mark
/// before its name.
///
/// Brand order since DRAGON-495 ([`crate::cloud::registry_display_order`]), not registry order:
/// the registry grows by appending, so its order says when a provider was added rather than
/// anything a reader can use. `index` stays the REGISTRY index, because that is what
/// `AddProviderSelected` carries.
///
/// **Connectable only since DRAGON-498** ([`crate::cloud::connectable_display_order`]). Proton
/// Drive and iCloud Drive used to sit here unpressable, each captioned "Not available yet", so
/// a user could see they had not been forgotten; the owner's call is that a picker should offer
/// what can be picked. Every row here is therefore pressable, which is why this loop has no
/// caption line and no selectable guard left in it. The registry still carries both providers
/// for every prose answer that needs them.
fn provider_menu<'a>(selected: usize) -> Element<'a, Msg> {
    let offered = crate::cloud::connectable_display_order();
    let mut list = widget::column::with_capacity(offered.len()).spacing(2.0);
    for (index, spec) in offered {
        let content = widget::row::with_capacity(2)
            .push(crate::widgets::icons::sized(spec.icon, 20.0))
            .push(widget::text::body(spec.display_name).width(Length::Fill))
            .spacing(10.0)
            .align_y(Alignment::Center)
            .width(Length::Fill);
        let entry = widget::button::custom(content)
            .class(cosmic::theme::Button::MenuItem)
            .selected(index == selected)
            .width(Length::Fill)
            .padding([6.0, 10.0])
            .on_press(cm(CloudSettingsMsg::AddProviderSelected(index)));
        list = list.push(crate::widgets::arrow_cursor::arrow_cursor(entry));
    }
    menu_surface(list, PROVIDER_MENU_W)
}

/// The opaque menu surface the provider list sits on: the component base at full alpha, a
/// 1px divider outline and panel rounding. The same look the preview editor's dropdown menus
/// wear (its builder lives on the editor's toolbar type, which settings cannot reach).
fn menu_surface<'a>(content: impl Into<Element<'a, Msg>>, width: f32) -> Element<'a, Msg> {
    widget::container(content)
        .width(Length::Fixed(width))
        .padding(4.0)
        .class(cosmic::theme::Container::custom(move |t| {
            let c = t.cosmic();
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(c.background.component.base.into())),
                border: Border {
                    radius: theme::rounding(t).s.into(),
                    width: 1.0,
                    color: c.background.divider.into(),
                },
                ..Default::default()
            }
        }))
        .into()
}

/// Wrap the folder list in a bordered, rounded WELL so its extent is visible (the owner
/// reported the bare list as "floating", with no edge to it). The same item-card look every
/// other framed surface in the app wears: the component base fill, a 1px divider outline, and
/// PANEL (`s`) rounding read from [`theme::rounding`] so the corners follow the user's
/// Appearance roundness setting live rather than a hardcoded radius. This is `menu_container`'s
/// shape (`preview::chrome`) at Fill width instead of a fixed one, matching the file's own
/// `menu_surface`.
fn folder_list_well<'a>(content: impl Into<Element<'a, Msg>>) -> Element<'a, Msg> {
    widget::container(content)
        .width(Length::Fill)
        .padding(4.0)
        .class(cosmic::theme::Container::custom(move |t| {
            let c = t.cosmic();
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(c.background.component.base.into())),
                border: Border {
                    radius: theme::rounding(t).s.into(),
                    width: 1.0,
                    color: c.background.divider.into(),
                },
                ..Default::default()
            }
        }))
        .into()
}

/// The gear button on a connected account's row that RE-OPENS the setup step (the owner's
/// "change the folder setup again" ask, DRAGON-489 follow-up; the RENAME affordance too since
/// DRAGON-495). Icon-only in the shared pill material like
/// [`super::super::row::open_folder_btn`], since it sits beside the bigger Disconnect button as
/// a small affordance, not a second full-width action. The glyph is `emblem-system-symbolic`,
/// the same gear the capture overlay's Settings button wears, so "gear opens configuration"
/// stays one icon across the app.
///
/// **One button, not two** (DRAGON-495). Rename could have arrived as its own pencil beside the
/// gear, but the step this already opens is exactly where the name is edited, and a row with
/// three icon buttons before the Disconnect starts reading as a toolbar. The tooltip
/// ([`CONFIGURE_TOOLTIP`]) is the owner's own copy.
fn edit_account_btn<'a>(id: String) -> Element<'a, Msg> {
    // DRAGON-514: the gear takes the ROW's shared height rather than sizing itself. See
    // `row_control_h` for what was wrong before and why this is a height rather than a smaller
    // glyph. The vertical padding is then whatever centres the glyph in that height, which is
    // why it is derived rather than typed.
    let h = row_control_h();
    let pad_y = ((h - GEAR_GLYPH) / 2.0).max(0.0);
    // The tooltip rides on a `widget::tooltip` wrapper (the `row::reset_button` pattern), not
    // the button builder's own `.tooltip()`, which `button::custom`'s `Button` does not carry.
    let btn = widget::button::custom(crate::widgets::icons::sized("emblem-system-symbolic", GEAR_GLYPH))
        .class(standard_button_class())
        .padding([pad_y, GEAR_PAD_X])
        .height(Length::Fixed(h))
        .on_press(cm(CloudSettingsMsg::EditAccount(id)));
    widget::tooltip(
        crate::widgets::arrow_cursor::arrow_cursor(btn),
        widget::text(CONFIGURE_TOOLTIP).size(12),
        widget::tooltip::Position::Top,
    )
    .into()
}

/// The gear's own glyph box, kept at the size it has always been: this control was never too
/// big because of its ICON (DRAGON-514, and the owner's own "not sure if we need to reduce the
/// icon height or something else" — it was something else).
const GEAR_GLYPH: f32 = 20.0;

/// The gear's horizontal padding. Unchanged, and horizontal only: the VERTICAL padding is
/// derived from [`row_control_h`] so the glyph centres in the shared height.
const GEAR_PAD_X: f32 = 8.0;

/// **The height every control in a connected account's row shares** (DRAGON-514), and, since,
/// the SAME height [`breadcrumb_row`] pins every crumb-ish child to (owner report, live
/// testing: that row's height was visibly shifting).
///
/// Not a number of ours, and that is the point. libcosmic's `button::text` builder declares
/// `height: Length::Fixed(theme.space_l())` (`widget/button/text.rs`), and `button::standard`
/// and `button::suggested` are both that builder, so Reconnect and Disconnect are `space_l`
/// tall by construction: 32px on the default spacing, 24 compact, 48 spacious.
///
/// `button::custom` is NOT that builder. It is `Length::Shrink`, sized by its own content, so
/// the gear was `GEAR_GLYPH + 2 × 8` = 36px beside a 32px Disconnect. The two controls were
/// never disagreeing about icon size; they were being sized by two different rules, one of
/// which follows the theme's spacing and one of which does not. Shrinking the glyph would have
/// matched them at ONE spacing setting and broken them at the other two.
///
/// `breadcrumb_row` hit the identical shape of bug, one call deeper: its pressable crumbs are
/// `widget::button::text` (this same `space_l`-by-construction builder), while its unpressable
/// crumb ([`level_control`], a plain `container`), its elided crumb (bare text, no button at
/// all) and its trailing refresh/create icon buttons are all `Length::Shrink`. A trail with
/// nothing below the app folder has NO pressable crumb (the sole, last crumb always routes to
/// `level_control`), so the row sat at its shorter Shrink height; opening any subfolder turned
/// the app-folder crumb into a pressable `button::text` and jumped the row to `space_l`. That
/// read as "the row's height shifts when the folder list refreshes/populates", because opening
/// a folder is exactly what starts its listing AND exactly what adds a pressable crumb. See
/// `folder_refresh_button`'s and `PATH_ICON`'s docs for the DIFFERENT (and, checked directly
/// against libcosmic's layout code, not actually buggy) risk this is not: the loading spinner
/// agreeing in SIZE with the settled refresh/create buttons.
///
/// Reading the token here means the row has one sizing source. Anything else added to these
/// rows should take its height from this rather than from its own padding.
fn row_control_h() -> f32 {
    cosmic::theme::active().cosmic().space_l() as f32
}

/// A dialog's title, with the provider's icon just before the text (the owner's original
/// request). NOT `Dialog::icon()` (DRAGON-489 follow-up regression): that builder puts the
/// icon in its OWN column beside the dialog's ENTIRE content block (title + body +
/// controls), which silently narrows the content column and right-shifts everything in it
/// relative to the card's true width, the "everything is margined to the right" bug the
/// owner hit in both this step and the folder step. Built as a plain row and fed in as a
/// `.control(...)` element instead, with `Dialog::title`/`Dialog::icon` left unset, so the
/// content column gets the card's full width. 20px matches the inline provider icons used
/// elsewhere in this file (the provider menu, the provider chip), not the oversized 32px the
/// regression used.
fn provider_title_row(spec: &'static ProviderSpec, title: String) -> Element<'static, Msg> {
    widget::row::with_capacity(2)
        .push(crate::widgets::icons::sized(spec.icon, 20.0))
        .push(widget::text::title3(title))
        .spacing(8.0)
        .align_y(Alignment::Center)
        .into()
}

/// Waiting for the user to sign in. A Cancel is always offered: the flow's own deadline is
/// two minutes, and a user who has changed their mind must not have to wait it out.
///
/// **The header is `provider_title_row`** (the provider's own icon + "Signing in to X"), the
/// SAME header the provider-choice and folder steps use. An earlier pass swapped this for the
/// app's own logo, on the mistaken belief that "the connection screen" meant this in-app
/// dialog; the owner's actual ask was the loopback HTML page the browser shows on the OTHER
/// end of the flow (`cloud::oauth`'s `SUCCESS_PAGE`/`DENIED_PAGE`), which carries the logo
/// instead. This dialog stays exactly as `provider_title_row`'s own doc describes it.
///
/// The whole step is ONE plain LEFT-aligned column: header, then the "Click here" sentence,
/// then the countdown directly below it. There is no QR code (DRAGON-489 follow-up): the OAuth
/// redirect is a loopback URL (`127.0.0.1` / `localhost`) that only resolves on the machine
/// running this app, so a phone that scanned a code and signed in there would be redirected to
/// itself, where nothing is listening. Signing in from another device would need a
/// publicly-reachable relay, which is out of scope; the link is the one honest way in.
///
/// **Two-phase, on `url`** (DRAGON-489). The step opens with `url` = `None`, for the frame or
/// two before the background task reports the sign-in page's own address back; all it can
/// show then is that it is waiting. Once the URL arrives (`Some`), it adds the "Click here"
/// sentence with a clickable accent link (opening the page) plus a copy icon: the way in,
/// since the page is no longer opened automatically (an automatic launch can fail silently: no
/// default browser set, the wrong browser opens, a headless/remote session).
///
/// **`copied` flashes the copy button** (DRAGON-489 follow-up). For [`COPIED_FLASH`] after a
/// press it swaps the copy glyph to a success-green tick and its tooltip to "Copied!", then
/// reverts on its own via the once-a-second tick (no state to clear).
fn cloud_browser_step<'a>(
    spec: &'static ProviderSpec,
    url: Option<&'a str>,
    remaining_secs: u64,
    copied: bool,
) -> widget::Dialog<'a, Msg> {
    // ONE plain left-aligned column: the provider-icon header, then the instruction / link /
    // waiting lines stacked below it. No centered sub-column: the QR image (the one thing that
    // needed centering) is gone.
    let mut content = widget::column::with_capacity(5).spacing(12.0).width(Length::Fill);
    content = content.push(provider_title_row(spec, format!("Signing in to {}", spec.display_name)));
    if url.is_none() {
        content = content.push(widget::text::body(format!(
            "Sign in to {} using the link below, then come back to this window.",
            spec.display_name
        )));
    }
    if let Some(url) = url {
        let theme = cosmic::theme::active();
        let accent = theme::accent(&theme);
        // "Click here" is the clickable span; the rest of the sentence rides in a separate,
        // plain-colour `rich_text` span, and the copy icon sits between the two as a sibling
        // in the row (a `Span` carries no room for an inline widget). This one sentence takes
        // the place of the generic header sentence above (owner report: showing both read as
        // a bug).
        let click_span = cosmic::iced::widget::span("Click here").color(accent).link(());
        let click_link = {
            let url = url.to_string();
            cosmic::iced::widget::rich_text([click_span])
                .on_link_click(move |()| cm(CloudSettingsMsg::OpenBrowserUrl(url.clone())))
        };
        // The app's ONE copy control (`widgets::copy_button`, shared with the upload meter since
        // DRAGON-520): while `copied` its glyph becomes a success-green tick and its tooltip
        // reads "Copied!", reverting on its own once the flash window passes.
        // ABOVE the QR code, so the tooltip drops UPWARD and never lands on the QR image below it.
        let copy = copy_button(
            copied,
            2,
            widget::tooltip::Position::Top,
            cm(CloudSettingsMsg::CopyBrowserUrl(url.to_string())),
        );
        content = content.push(
            widget::row::with_capacity(3)
                .push(click_link)
                .push(copy)
                // NOT `subdued_caption`: the same "78%-toward-background, unreadable for real
                // content" mistake already fixed twice elsewhere in this file (the
                // app-registration notes, the provider-menu "not available yet" caption).
                .push(widget::text::body("to sign in with your browser."))
                .spacing(4.0)
                .align_y(Alignment::Center),
        );
    }
    // Directly under the link sentence, left-aligned like the rest (owner request), not above,
    // so the step never looks "already finished" before the link even renders (the first frame
    // or two, while `url` is still `None`). The countdown reads against the real
    // `BROWSER_DEADLINE`, via `browser_started` at the call site, so it's never just a
    // decorative number. `subtle_caption`, not `subdued_caption`: this line is secondary (a
    // background countdown, not the sentence someone actually acts on), so it dims, but only
    // half of `subdued`'s wash, which read as unreadable for real content elsewhere in this file.
    //
    // A DELIBERATE BREAK above it (DRAGON-503, owner report: the two lines sat too close). The
    // countdown is a different kind of statement from the sign-in link over it, a background fact
    // rather than the thing to act on, and at the column's own 12px it read as a fourth line of
    // the same block. One more `space_xs`, the SAME token this column spaces by, doubles that gap
    // into a section break without introducing a second spacing value to keep in step.
    let gap = cosmic::theme::active().cosmic().space_xs() as f32;
    content = content.push(widget::Space::new().height(Length::Fixed(gap)));
    content = content.push(subtle_caption(format!("Waiting for the browser ({remaining_secs})...")));

    widget::dialog()
        // No `.icon()`, no `.title()`, no `.body()`: the whole left-aligned header is fed in as
        // the `.control(...)`, so the content column gets the card's full width (see
        // `provider_title_row`'s doc for the `Dialog::icon()` regression this avoids).
        .control(content)
        .secondary_action(crate::widgets::arrow_cursor::arrow_cursor(
            widget::button::text("Cancel").on_press(cm(CloudSettingsMsg::AddClose)),
        ))
}

/// The failure step. `retry` decides whether there is a primary action at all: a build with
/// no sign-in id, and a provider with no API, are not going to behave differently on a
/// second press, and offering one would say otherwise.
fn cloud_failed_step<'a>(
    spec: &'static ProviderSpec,
    message: &'a str,
    retry: bool,
) -> widget::Dialog<'a, Msg> {
    let dialog = widget::dialog()
        .title(format!("{} could not be connected", spec.display_name))
        .body(message)
        .secondary_action(crate::widgets::arrow_cursor::arrow_cursor(
            widget::button::text("Close").on_press(cm(CloudSettingsMsg::AddClose)),
        ));
    if !retry {
        return dialog;
    }
    dialog.primary_action(crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::suggested("Try again").on_press(cm(CloudSettingsMsg::Connect)),
    ))
}

/// The post-connect setup step: the account's NAME, and (where the provider has one) where its
/// uploads land. The account is ALREADY saved by the time this shows, so every way out of it
/// (pick a folder, type one, rename, close) leaves a working account.
///
/// **Shown for every provider** (DRAGON-495). It was the folder step, reached only where there
/// were folders to choose, so a YouTube connect ended with no step and no chance to name the
/// account. The heading and the sentence under it come from [`setup_title`] / [`setup_body`],
/// which is what keeps a folderless step from asking where uploads should go.
fn cloud_setup_step<'a>(add: &'a CloudAdd, spec: &'static ProviderSpec) -> widget::Dialog<'a, Msg> {
    let browse = chooses_folder(spec);
    let mut control = widget::column::with_capacity(7).spacing(10.0).width(Length::Fill);
    control = control.push(provider_title_row(spec, setup_title(browse).to_string()));
    control = control.push(widget::text::body(setup_body(spec, browse)));
    if let Some(note) = setup_sandbox_note(spec, browse) {
        // `subtle_caption`, the SAME quiet-but-legible tone as the footer status below
        // (DRAGON-495, owner request: one quiet-text style per dialog). It states a limit rather
        // than an instruction, so it steps back from the body text above it without falling into
        // the subdued wash this file has been reported for twice.
        control = control.push(subtle_caption(note));
    }
    if browse {
        // The path row carries the level's own name, mark and tick, and the refresh/create pair
        // (DRAGON-514). Everything the list used to duplicate at its top lives there now.
        control = control.push(breadcrumb_row(
            add,
            folder_slot(browse, add.folder_loading, add.folder_note.is_some()),
        ));
        let mut list = widget::column::with_capacity(add.folders.len() + 1).spacing(2.0);
        if let Some(typed) = add.creating.as_deref() {
            // At the TOP of the list, where the folder itself will appear once the level is
            // re-listed, and at the list's own inset. See `new_folder_row`.
            list = list.push(new_folder_row(typed, add.create_busy));
        }
        // DRAGON-514: the list's FIRST entry used to be the folder the list is INSIDE, repeated
        // above its own children, and it was the only way to choose that folder. The owner's
        // report was that it reads as a child of itself. It is gone; the path row names the level
        // (`level_control`), and the list is exactly the children of the level named above it.
        //
        // DRAGON-517: a row carries NO tick and no pick. Going into a folder is what makes it the
        // destination, so the one press a row has is the one it always had, and the mark that
        // said "this one is chosen" now sits on the level a user is actually in.
        for (index, (_, name)) in add.folders.iter().enumerate() {
            list = list.push(folder_entry(
                name,
                Some(index),
                cm(CloudSettingsMsg::FolderCrumbIn(index)),
            ));
        }
        // A "Reading your folders..." line USED to sit here, directly above the list, in the
        // SUBDUED tone (DRAGON-489); it then moved to the dialog's footer as a
        // `tertiary_action` (DRAGON-495) because it pushed the list down as it came and went.
        // DRAGON-514 deleted it outright, in both places: there is no loading TEXT anywhere in
        // this browser now, and the refresh icon in the path row spins instead.
        //
        // The settled-empty level says so, in the well's own space (DRAGON-520). See
        // `shows_empty_level_note` for the three states that keep it quiet instead. The note
        // REPLACES the scrollable rather than riding inside it (as it used to as an extra row):
        // a scrollable lays its content out with an unbounded height budget, so it can overflow
        // and scroll, which leaves nothing for `align_y` to centre a row against. The note's own
        // container is pinned to the well's exact height instead, which is what gives centering
        // on both axes real room to apply.
        let well_content: Element<'_, Msg> = if shows_empty_level_note(
            add.folders.len(),
            add.folder_loading,
            add.creating.is_some(),
            add.pending_delete.is_some() || add.delete_busy,
        ) {
            let lines: Vec<Element<'_, Msg>> =
                EMPTY_LEVEL_NOTE.iter().map(|line| subtle_caption(*line)).collect();
            widget::container(widget::column(lines).spacing(2.0).align_x(Alignment::Center))
                .width(Length::Fill)
                .height(Length::Fixed(FOLDER_LIST_H))
                .padding([0.0, FOLDER_ROW_PAD[1]])
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .into()
        } else {
            widget::scrollable(list).height(Length::Fixed(FOLDER_LIST_H)).width(Length::Fill).into()
        };
        control = control.push(folder_list_well(well_content));
    }
    // A TYPED-PATH field used to follow the list here, for Dropbox alone, with a sentence
    // explaining that typing one overrode whatever had been picked. Both are gone with the axis
    // they belonged to (DRAGON-498, see `chooses_folder`): with every provider confined to one
    // app folder there is no path left to type that the list does not already offer, and the
    // override rule was the page's most confusing behaviour by the owner's own report.

    // The account's NAME (DRAGON-489 follow-up; on EVERY provider's step since DRAGON-495, and
    // reachable again afterwards from the row's gear). Prefilled with the label the account
    // already carries: for a fresh connect that is the default `connect_and_save` gave it
    // (`derive_label`'s "Provider (date)"), for a rename it is whatever the user last chose.
    // Clearing it is allowed while typing and saves the provider DEFAULT rather than an empty
    // label; see `commit_label` for why an empty one is the wrong thing to write.
    control = control.push(widget::text::body("What should this account be called?"));
    control = control.push(
        widget::text_input(
            "Name this account",
            add.account.as_ref().map(|a| a.label.clone()).unwrap_or_default(),
        )
        .on_input(|text| cm(CloudSettingsMsg::AccountLabelInput(text)))
        .width(Length::Fill),
    );
    if let Some(note) = add.folder_note.as_deref() {
        control = control.push(warning_caption(note));
    }
    widget::dialog()
        // No `.icon()`, no `.title()`, no `.body()`: see `provider_title_row`'s doc (the
        // same "everything margined to the right" fix as the browser step).
        .control(control)
        // No separate "Skip": DRAGON-489 follow-up removed it once the step made plain where a
        // capture would land by default. Doing nothing and pressing Done produces exactly what
        // Skip used to (the account's own app folder, DRAGON-498, which is the level the browser
        // opens on when nothing else is saved), so nothing is lost, one less redundant button.
        //
        // Cancel DOES still belong here (owner report: there was no way back out of this
        // step at all): see `CloudSettingsMsg::CancelSetupStep`'s doc for why it disconnects
        // a fresh connect but only closes on a reconnect.
        .secondary_action(crate::widgets::arrow_cursor::arrow_cursor(
            widget::button::text("Cancel").on_press(cm(CloudSettingsMsg::CancelSetupStep)),
        ))
        // `on_press_maybe`, not a wired `on_press` plus a colour change (owner report, live
        // testing): while `add.folder_loading` is up (the same flag `folder_slot` and
        // `folder_refresh_button` key off, the refresh icon spinning), Done writes the account
        // against whatever the LAST listing landed, which is exactly the folder the browser is
        // about to move out from under it. Withholding the message is what actually stops the
        // press, not merely tints the button; the library already renders a `None` `on_press`
        // in its own disabled style, the same "no handler wired" idiom this file already uses
        // elsewhere (`app/preview/chrome.rs`'s `.on_press_maybe(enabled.then_some(..))`).
        .primary_action(crate::widgets::arrow_cursor::arrow_cursor(
            widget::button::suggested("Done")
                .on_press_maybe((!add.folder_loading).then_some(cm(CloudSettingsMsg::SetupDone))),
        ))
    // DRAGON-503 put a footer slot here (`tertiary_action`), carrying either the
    // "Reading your folders..." status or the refresh button. DRAGON-514 emptied it: the status
    // is deleted outright and the refresh moved up to the path row, beside the create button,
    // where both act on the level they are read next to. The footer is Cancel and Done again.
}

/// The folder browser's REFRESH control (DRAGON-503, relocated and given a spinning face by
/// DRAGON-514): re-read this account's folders, for a user who has just made one at the
/// provider.
///
/// **Where it is.** In the PATH row, immediately before the create button, with only
/// [`PATH_ACTION_GAP`] between them: the two controls that act on the level the row names,
/// grouped as one pair at the trailing edge. It used to sit in the dialog's footer slot
/// (`tertiary_action`), a place the eye reaches after Cancel and Done, describing a list two
/// rows above it.
///
/// **The three faces are [`FolderSlot`]'s**, and the LOADING one is the whole point: the icon
/// spins in place, in the SUBTLE tone, and that is the only indication a listing is running.
/// No text appears, nothing is added to the row, and nothing moves, which is exactly what the
/// deleted "Reading your folders..." line could not manage. The spinning face has NO `on_press`,
/// so a second listing cannot be started on top of the first, and the missing press is honest
/// rather than a control that accepts a click and drops it.
///
/// The angle comes in from `CloudAdd::folder_spin`, advanced by
/// [`CloudSettingsMsg::FolderSpinTick`] while (and only while) a listing is in flight. This is
/// the scanner's mechanism exactly (`overlay::toolbar`'s re-read button, the app's other
/// spinning glyph): [`crate::widgets::icons::sized_rotated`] goes through iced's `svg` widget,
/// because rotation lives there and not on `icon::Icon`.
///
/// `view-refresh-symbolic` is the clockwise "reload" glyph every other refresh control in the
/// app already wears (see `widgets::icons`, where the scanner's counter-clockwise re-read is
/// kept deliberately distinct from it), so no new icon is registered for this.
///
/// `Failed` swaps only the tooltip: after a failure there is no list on screen to refresh, and
/// "Try again" is what the button actually does then.
///
/// **`height` is [`row_control_h`]**, the same shared height every crumb-ish child in
/// [`breadcrumb_row`] carries, for a REAL guarantee rather than a trusted-to-agree one. The
/// glyph box itself was already sound before this (`PATH_ICON`'s doc has the full account: a
/// `Length::Fixed` size ignores a rotated SVG's own intrinsic size, and `button::icon`'s inner
/// content is the identical `iced::widget::Svg` type at the identical `Length::Fixed`, so the
/// bare `container` here and the pressable `button::icon` below it were never actually
/// disagreeing). What COULD disagree, and is now pinned shut too, is this control's footprint
/// against the REST of the row: a bare `container`/a `button::icon` are both `Length::Shrink`
/// by default, sized to `PATH_ICON` plus its own padding, a few px shorter than a pressable
/// crumb's `button::text` (`space_l` by construction, see [`row_control_h`]'s doc). Explicit
/// `height` on every face removes any dependency on those two rules coincidentally landing on
/// the same number.
/// The SUBTLE tone, for an SVG glyph rather than text: the spinning refresh icon's own tint, and
/// (owner report, the FIRST-EVER-load state) the leading folder icon's while [`breadcrumb_row`]
/// is showing "Loading..." in its place. One closure so the two cannot drift to two different
/// subtle greys.
fn subtle_svg_class() -> cosmic::theme::Svg {
    cosmic::theme::Svg::custom(|theme| cosmic::widget::svg::Style { color: Some(theme::subtle(theme)) })
}

fn folder_refresh_button<'a>(slot: FolderSlot, spin: f32, height: f32) -> Element<'a, Msg> {
    if slot == FolderSlot::Loading {
        // Sized to the glyph the pressable faces wear, so the row's height and the control's
        // footprint are identical in all three states and the spin cannot nudge the `+` beside
        // it sideways.
        let glyph =
            crate::widgets::icons::sized_rotated("view-refresh-symbolic", PATH_ICON, spin, subtle_svg_class());
        // The same padding the pressable faces carry, applied to the bare glyph: a container,
        // not a button, because there is nothing to press while this is up. `height` (not left
        // to `Length::Shrink`) is the shared row height; see this function's doc.
        return widget::container(glyph)
            .padding(2)
            .height(Length::Fixed(height))
            .align_y(Alignment::Center)
            .into();
    }
    crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::icon(crate::widgets::icons::handle("view-refresh-symbolic"))
            .class(accent_icon_button_class(false))
            .padding(2)
            .height(Length::Fixed(height))
            .tooltip(if slot == FolderSlot::Failed { "Try again" } else { "Refresh folder list" })
            .on_press(cm(CloudSettingsMsg::FolderRefresh)),
    )
}

// DRAGON-514: `footer_status` lived here — the dialog's leading-footer status line, which
// carried "Reading your folders..." vertically centred with Cancel and Done. It is deleted with
// the only string it ever showed. Two hard-won notes are kept, because the slot itself still
// exists and the next person to put something in it needs them:
//
//   * `tertiary_action` IS the leading-footer slot. `widget::Dialog` builds one button row as
//     `[tertiary, <horizontal space>, secondary, primary]` (libcosmic `widget/dialog.rs`), so a
//     tertiary element is left-aligned while the two real buttons keep their right alignment.
//   * **Never give anything in that row a `Length::Fill` HEIGHT.** That was the DRAGON-495 bug,
//     reported live: while the status was up, the whole dialog stretched to the full window
//     height. `Row::push` does `self.height = self.height.enclose(child.height)` and
//     `Length::enclose(Shrink, Fill)` is `Fill`, so declaring Fill on ANY child promotes the
//     ROW's declared height; `Column::push` then promotes the dialog's content column, and
//     `Container::new` copies its content's `size_hint().fluid()`, so the CARD ends up
//     Fill-height and `stack_dialog` stretches it over the window. This happens at BUILD time,
//     before any layout pass runs, which is why reasoning about `layout::flex` missed it. Use
//     `Length::Fixed` (the buttons' own `space_l`); `enclose` only ever promotes to Fill.

/// One row of the folder list: a folder inside the level the path row names.
///
/// # One press, one meaning (DRAGON-517)
///
/// A row's press GOES INTO the folder, and going into a folder is what makes it this account's
/// destination. There is nothing else to press, because there is nothing else to choose.
///
/// It used to be two answers on one row: a press that descended, plus a tick showing whether the
/// folder happened to be the account's pick from some earlier visit, with the pick itself made
/// from the path row above. That is the model this ticket replaced. The owner's reading is the
/// simpler one, and it is the one a phone's folder picker uses: where you are is where it goes.
///
/// A row still carries a DELETE (`deletable` is `Some(index)`), which is why the trailing block
/// survives the tick's removal: see [`folder_row_actions`] for what keeps every row ending at the
/// same inset now that only one of its two slots is ever filled.
///
/// `Button::MenuItem` without `.selected(..)`: the flag was always a no-op here (that class's
/// style function never reads it, unlike `Button::ListItem`'s, which is why the tick existed at
/// all), and there is no per-row selection left for it to carry even if a future libcosmic
/// started honouring it.
fn folder_entry<'a>(name: &str, deletable: Option<usize>, msg: Msg) -> Element<'a, Msg> {
    let row = widget::row::with_capacity(2)
        .push(crate::widgets::icons::sized("folder-open-symbolic", FOLDER_ROW_ICON))
        .push(widget::text::body(name.to_string()).width(Length::Fill));
    let entry = crate::widgets::arrow_cursor::arrow_cursor(
        widget::button::custom(row.spacing(10.0).align_y(Alignment::Center).width(Length::Fill))
            .class(cosmic::theme::Button::MenuItem)
            .width(Length::Fill)
            .padding(FOLDER_ROW_PAD)
            .on_press(msg),
    );
    widget::row::with_capacity(2)
        .push(entry)
        .push(folder_row_actions(deletable))
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .into()
}

/// The glyph box for a folder row's leading folder mark.
const FOLDER_ROW_ICON: f32 = 18.0;

/// A folder row's own padding, `[vertical, horizontal]`. Shared with the inline create row
/// (DRAGON-514), which is why it is a constant rather than a literal on the button: the create
/// row used to be a full-width bar with no inset at all, so it lined up with nothing.
const FOLDER_ROW_PAD: [f32; 2] = [6.0, 10.0];

/// The glyph box for BOTH of a row's trailing actions. One constant, because a tick and a bin
/// that are different sizes in the same column is exactly what DRAGON-514 was reported for.
const FOLDER_ACTION_ICON: f32 = 16.0;

/// The padding around a trailing action's glyph, matching the row's own horizontal inset on the
/// outside edge so every row ends at the same place as the text above and below it.
const FOLDER_ACTION_PAD: f32 = 4.0;

/// **The fixed width of a folder row's trailing-actions block** (DRAGON-514).
///
/// Two slots, always both reserved, whether or not either is filled. This is the whole
/// mechanism that keeps a row with a tick, a row with a bin, and a row with both ending at the
/// same inset from the browser's edge: the block cannot change width, so the name column cannot
/// change width either, so nothing shifts as a selection moves between rows.
const FOLDER_ACTIONS_W: f32 =
    2.0 * (FOLDER_ACTION_ICON + 2.0 * FOLDER_ACTION_PAD) + FOLDER_ROW_PAD[1];

const _: () = assert!(
    FOLDER_ACTIONS_W > 2.0 * FOLDER_ACTION_ICON + FOLDER_ROW_PAD[1],
    "DRAGON-514: the trailing block must reserve BOTH glyphs and their padding, or a row with \
     one action ends at a different inset from a row with two, which is what it was reported for"
);
const _: () = assert!(
    FOLDER_ROW_PAD[1] > 0.0,
    "DRAGON-514: a zero row inset is the unpadded full-width bar the inline create row was \
     reported as; the create row shares this constant so the two cannot drift apart again"
);

/// Which of a folder row's two trailing slots are FILLED. Pure; unit-tested.
///
/// `[tick, bin]`, and **the tick is now always empty** (DRAGON-517): selection follows the view,
/// so a folder in the LIST is by definition not the level the browser is in, and a row has no
/// pick for a mark to report. The slot is still RESERVED, which is why this answers a pair rather
/// than a bare bool: the block's width is [`FOLDER_ACTIONS_W`], fixed, and an unfilled slot is
/// drawn as a `Space` of the same footprint, so a row's name column and its right edge do not
/// move when the bin is the only thing in it.
///
/// The block's WIDTH does not depend on this at all: this decides what is DRAWN, never how much
/// room it takes.
fn folder_row_slots(deletable: bool) -> [bool; 2] {
    [false, deletable]
}

/// A folder row's trailing actions: the delete bin, in ONE fixed-width block sized for TWO
/// (DRAGON-514, emptied of its tick by DRAGON-517).
///
/// **Why this exists as its own builder.** The bin and the (now deleted) selection tick used to be
/// built in different places and in different shapes: the tick was a bare glyph INSIDE the row
/// button (so it wore the button's 10px inset), while the bin was a `button::icon` in
/// [`standard_button_class`] with 4px padding OUTSIDE it (so it wore a filled background box and a
/// different inset). The owner's screenshot is the result: two controls in one column at two
/// sizes, on two backgrounds, with the row's right edge landing in a different place depending on
/// which of them a row happened to have. One container, one glyph box, one padding, and they
/// cannot drift again.
///
/// **The two-slot reservation stays even though only one slot can be filled.** It is what keeps
/// every row ending at the same inset as the path row's own controls above the list, and it is one
/// constant rather than a width that changes with a row's state: see [`FOLDER_ACTIONS_W`] and
/// [`folder_row_slots`]. An unfilled slot is a `Space` of exactly the glyph's footprint rather
/// than nothing at all.
///
/// **The bin is a PLAIN glyph** (owner's call): no background box, drawn in the ordinary
/// foreground tone, so it reads as part of the row rather than as a button that wandered into the
/// list. It is still pressable, and still sits outside the row button, because a button inside a
/// button gets no press of its own: the outer one captures first.
///
/// **It is always live, even for a folder that cannot be deleted.** A dead icon reads as broken
/// rather than as "not this row", which is the lesson `connect_pressable` records from the
/// owner's own report of this dialog; pressing one of those puts the REASON on the step's notice
/// line instead (see [`delete_refusal`] and [`CloudSettingsMsg::FolderDelete`]).
fn folder_row_actions<'a>(deletable: Option<usize>) -> Element<'a, Msg> {
    // One slot's footprint, used for both the drawn glyph and the empty placeholders.
    let slot = FOLDER_ACTION_ICON + 2.0 * FOLDER_ACTION_PAD;
    let blank = || -> Element<'a, Msg> { widget::Space::new().width(Length::Fixed(slot)).into() };
    let [_tick_slot, show_bin] = folder_row_slots(deletable.is_some());
    // The leading slot is the retired selection tick (DRAGON-517). It is RESERVED and never
    // filled, which is what [`folder_row_slots`] answers and what keeps the bin in the column it
    // has always been in.
    let tick: Element<'a, Msg> = blank();
    let bin: Element<'a, Msg> = match deletable.filter(|_| show_bin) {
        Some(index) => widget::tooltip(
            crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::custom(
                    crate::widgets::icons::sized("edit-delete-symbolic", FOLDER_ACTION_ICON).class(
                        cosmic::theme::Svg::Custom(std::rc::Rc::new(|t: &cosmic::Theme| {
                            cosmic::widget::svg::Style { color: Some(theme::foreground(t)) }
                        })),
                    ),
                )
                // `Button::Icon`, NOT `standard_button_class`: the flat variant paints no
                // background of its own, which is what makes this read as a glyph beside the
                // tick instead of as a boxed control.
                .class(cosmic::theme::Button::Icon)
                .padding(FOLDER_ACTION_PAD)
                .on_press(cm(CloudSettingsMsg::FolderDelete(index))),
            ),
            widget::text("Delete this folder").size(12),
            widget::tooltip::Position::Top,
        )
        .into(),
        None => blank(),
    };
    widget::container(
        widget::row::with_capacity(2).push(tick).push(bin).align_y(Alignment::Center),
    )
    // The row's own horizontal inset on the OUTSIDE edge, so the actions stop where a row's
    // text would (`FOLDER_ROW_PAD`), and the fixed width that makes every row agree.
    .padding([0.0, FOLDER_ROW_PAD[1], 0.0, 0.0])
    .width(Length::Fixed(FOLDER_ACTIONS_W))
    .align_y(Alignment::Center)
    .into()
}

/// The path row above the folder list (DRAGON-506): where in the app folder the list is, the mark
/// saying that this IS where uploads land, and the pair that refreshes the level or makes a new
/// folder in it.
///
/// Crumbs are separated by a plain `/`, not a chevron glyph. That is the same separator
/// [`crate::cloud::save_path`] renders on the account's own row, so the two say the same thing in
/// the same shape, and it needs no new icon.
///
/// # The header IS the level, and the level IS the destination (DRAGON-514, DRAGON-517)
///
/// The LAST crumb is the level being looked at ([`breadcrumb`] guarantees it ends there), and it
/// carries the level's name alone now: no folder mark (that leads the whole trail instead,
/// DRAGON-520) and no tick (owner report; see [`level_control`] for why it left with no
/// replacement). DRAGON-514 made this crumb the control that CHOSE that level; DRAGON-517 removed
/// the choosing, because the level shown is the destination by definition. So the crumb is no
/// longer pressable, and its name alone states where this account's captures go, a fact about the
/// view and not about a stored pick.
///
/// The crumbs BEFORE it keep their old job of navigating up, and under this model that is also
/// how the destination moves back OUT: a press on the app folder's crumb both lists it and makes
/// it where uploads land. The elision keeps its plain text, because it names no single level
/// ([`crumb_target`]).
///
/// # Every crumb-ish child shares [`row_control_h`] (owner report: the row's height visibly
/// shifted)
///
/// A pressable crumb ([`widget::button::text`]) is `space_l` tall by construction; the
/// unpressable level ([`level_control`]), the elided crumb and the trailing refresh/create pair
/// are all `Length::Shrink`, naturally shorter. Left alone, a trail with no pressable crumb
/// (nothing below the app folder) sat at the shorter height, and opening any subfolder jumped it
/// to `space_l` the moment a pressable crumb appeared. `row_control_h`'s own doc has the full
/// account, including the DIFFERENT risk this is not (the loading spinner vs the settled refresh
/// button, [`folder_refresh_button`]'s doc and [`PATH_ICON`]'s: those already agree, checked
/// directly against libcosmic's layout code).
///
/// # The FIRST-EVER load, and only that one, says "Loading..." (owner report, live testing)
///
/// [`breadcrumb_first_load`] is the whole decision: while it is true (nothing has landed yet
/// this dialog session AND a fetch is in flight) the crumb trail is replaced by one subtle
/// "Loading..." caption and the leading folder icon is tinted subtle too, rather than drawing a
/// path built from data that does not exist yet. Every LATER fetch this session (a refresh
/// press, a descent, an ascent) still sets [`CloudAdd::folder_loading`], but by then
/// [`CloudAdd::has_loaded_once`] is already `true`, so those keep the ordinary look: the real
/// path, with just the trailing refresh icon spinning.
fn breadcrumb_row<'a>(add: &'a CloudAdd, slot: Option<FolderSlot>) -> Element<'a, Msg> {
    let depth = add.trail.len();
    let names = add.level_names();
    let crumbs = breadcrumb(depth);
    // The level's own crumb. `breadcrumb` always ends at the level being looked at, which is what
    // makes "the last one" the right test rather than a second depth calculation here.
    let last = crumbs.len().saturating_sub(1);
    // Every crumb-ish child (pressable crumbs, the level, the elided crumb, refresh/create) is
    // pinned to this ONE height below; see the doc above for why.
    let control_h = row_control_h();
    // The FIRST-EVER-load state for this dialog session (owner report, live testing): see this
    // function's doc and [`CloudAdd::has_loaded_once`]'s.
    let first_load = breadcrumb_first_load(add.folder_loading, add.has_loaded_once);
    let mut row = widget::row::with_capacity(depth * 2 + 5).spacing(4.0).align_y(Alignment::Center);
    // **The folder mark leads the whole trail** (DRAGON-520, owner request). It used to ride
    // inside [`level_control`], attached to the LAST crumb, which read as "this one is a folder"
    // rather than as "the row below is a folder path": the glyph moved along the row as the user
    // descended, and on a deep trail it sat behind the elision. Anchored at the start it names the
    // whole crumb trail once and never moves. [`level_control`] itself now carries no glyph at
    // all, folder mark or tick: the tick it used to wear is gone on the owner's own report.
    //
    // Tinted SUBTLE during the first-ever load too (owner report, live testing): the icon leads
    // whatever is shown in its place, the real path or "Loading...".
    let folder_glyph = crate::widgets::icons::sized("folder-open-symbolic", FOLDER_ROW_ICON);
    row = row.push(if first_load { folder_glyph.class(subtle_svg_class()) } else { folder_glyph });
    if first_load {
        // Nothing has EVER been shown yet this session, so there is no real path to draw: one
        // subtle status word takes the whole crumb trail's place, rather than a breadcrumb built
        // from data that was never fetched. `folder_refresh_button`'s own Loading face already
        // renders subtle (see its doc), so nothing in the trailing pair needs touching here.
        row = row.push(subtle_caption("Loading..."));
    } else {
        for (position, crumb) in crumbs.into_iter().enumerate() {
            if position > 0 {
                row = row.push(subtle_caption("/"));
            }
            let label = match crumb {
                Crumb::Root => ROOT_DESTINATION,
                // Not "..." typed as three dots: one ellipsis character is one glyph to lay out
                // and reads as an elision rather than as a pause.
                Crumb::Elided => "\u{2026}",
                Crumb::Level(index) => names.get(index).copied().unwrap_or_default(),
            };
            // Shortened so a long folder name cannot push the row past the card's edge; the
            // elision above bounds how MANY crumbs there are, and this bounds how wide one is.
            let label = crumb_label(label);
            // The level itself: the destination mark, not a navigation crumb.
            if position == last {
                row = row.push(level_control(label, control_h));
                continue;
            }
            match crumb_target(crumb, depth) {
                Some(target) => {
                    row = row.push(crate::widgets::arrow_cursor::arrow_cursor(
                        widget::button::text(label)
                            .padding([2.0, 6.0])
                            // Overrides `button::text`'s own `Length::Fixed(space_l)` default
                            // (see this function's doc): every crumb-ish child reads the SAME
                            // height.
                            .height(Length::Fixed(control_h))
                            .on_press(cm(CloudSettingsMsg::FolderCrumbTo(target))),
                    ));
                }
                // The elided crumb: no press, so no button, but the SAME box every other crumb
                // wears, for the same reason (see this function's doc).
                None => {
                    row = row.push(
                        widget::container(widget::text::body(label))
                            .padding([2.0, 6.0])
                            .height(Length::Fixed(control_h))
                            .align_y(Alignment::Center),
                    );
                }
            }
        }
    }
    row = row.push(widget::Space::new().width(Length::Fill));
    // The trailing PAIR (DRAGON-514): refresh, then create, as one group at the row's end.
    //
    // Refresh came from the dialog's footer slot, where it sat after Cancel and Done and
    // described a list two rows above it. Both of these act on the level this row names, so they
    // belong on this row, next to each other, with only `PATH_ACTION_GAP` between them. Refresh
    // leads, because it is the passive one: re-reading what is there before adding to it.
    //
    // **Create opens an INLINE row rather than a dialog, and the delete opens a dialog rather
    // than an inline confirm. That asymmetry is the point.** A card over this card is what a
    // destructive question earns: it stops everything and demands an answer. Making a folder is
    // the opposite kind of act, forward-moving and undone by the bin beside it, so it stays in
    // the list, exactly where the folder itself will appear once the level is re-listed, and
    // costs no full-screen interruption.
    let create = widget::tooltip(
        crate::widgets::arrow_cursor::arrow_cursor(
            widget::button::icon(crate::widgets::icons::handle("list-add-symbolic"))
                .class(accent_icon_button_class(false))
                .padding(2)
                // Same shared height as every crumb-ish child (see this function's doc); a bare
                // `button::icon` is `Length::Shrink` otherwise, a few px shorter than a pressable
                // crumb.
                .height(Length::Fixed(control_h))
                .on_press(cm(CloudSettingsMsg::FolderCreateOpen)),
        ),
        widget::text("New folder here").size(12),
        widget::tooltip::Position::Top,
    );
    let mut actions = widget::row::with_capacity(2).spacing(PATH_ACTION_GAP).align_y(Alignment::Center);
    if let Some(slot) = slot {
        actions = actions.push(folder_refresh_button(slot, add.folder_spin, control_h));
    }
    row.push(actions.push(create)).into()
}

/// The path row's LEVEL mark (DRAGON-514, made a statement rather than a control by DRAGON-517,
/// then stripped of its own tick on the owner's later report): the name of the folder the
/// browser is inside, this account's destination by definition.
///
/// Shaped like the list row it replaced, deliberately: the level's own name, at the same padding
/// as the navigation crumbs beside it. A user who read the old first-row-of-the-list as "this
/// folder" reads this the same way, one line higher and without a duplicate of itself underneath.
///
/// **The folder glyph is no longer part of it** (DRAGON-520, owner request): it leads the whole
/// breadcrumb from the row's start now, where it names the trail rather than one crumb of it. See
/// [`breadcrumb_row`].
///
/// **The tick is gone too** (owner report, live testing). DRAGON-517 had already made it
/// unconditional, since the level shown always IS the destination, which left it only confirming
/// a fact the label beside it already states. The owner's ask was for a mark that makes it
/// obvious you're inside a folder while browsing subfolders, ideally on the folder LIST rather
/// than here; there is no row in that list to put one on without relitigating a decision this
/// file already settled twice, DRAGON-514 removed the one row that used to repeat the current
/// level (it read as "a child of itself") and DRAGON-517 removed the per-row selection state a
/// mark would otherwise need to read. So the checkmark is simply gone, and "where you are" is
/// left to the label here, which was always legible on its own before DRAGON-514 first put a
/// control on it.
///
/// **It is not pressable.** DRAGON-514 made this the control that selected the level, which was
/// the best available answer while a selection could sit somewhere the browser was not.
/// DRAGON-517 removed that possibility: the level shown IS the destination, so pressing "you are
/// here" would do nothing.
///
/// A plain container rather than a `button::custom` for exactly that reason: a control with no
/// press is a thing users click at. The padding matches the navigation crumbs beside it so the
/// row's baseline does not step.
///
/// **`height` is [`row_control_h`], not left to `Length::Shrink`** (owner report, live testing:
/// see [`breadcrumb_row`]'s doc). A plain `container` shrinks to its own text, a few px shorter
/// than a pressable crumb's `button::text` (`space_l` by construction), so the row's height used
/// to depend on whether the level being shown happened to be the trail's only crumb.
fn level_control<'a>(label: String, height: f32) -> Element<'a, Msg> {
    widget::container(widget::text::body(label))
        .padding([2.0, 6.0])
        .height(Length::Fixed(height))
        .align_y(Alignment::Center)
        .into()
}

/// The inline "new folder" row (DRAGON-506): the name field, a tick that creates it, and an x
/// that gives up.
///
/// The field carries `on_submit` as well as the tick, so Enter finishes it without reaching for
/// the mouse. While the create is in flight the controls are replaced by a status caption rather
/// than greyed out, which stops a second create being submitted on top of the first.
///
/// **It is inset exactly like a folder row** (DRAGON-514, owner's report): [`FOLDER_ROW_PAD`],
/// the same constant `folder_entry`'s button carries, so the field's folder mark lines up with
/// the marks above and below it. Before this it was a bare full-width bar with no padding of its
/// own, which made it read as a strip laid across the list rather than as the row the new folder
/// is about to become, and made the list appear to jump when the real row replaced it.
fn new_folder_row<'a>(typed: &'a str, busy: bool) -> Element<'a, Msg> {
    let mut row = widget::row::with_capacity(4)
        .push(crate::widgets::icons::sized("folder-open-symbolic", FOLDER_ROW_ICON))
        .spacing(6.0)
        .align_y(Alignment::Center)
        .width(Length::Fill);
    if busy {
        return padded_folder_row(
            row.push(widget::text::body(typed.to_string()).width(Length::Fill))
                .push(subtle_caption("Creating...")),
        );
    }
    row = row.push(
        widget::text_input("Folder name", typed)
            .on_input(|text| cm(CloudSettingsMsg::FolderCreateInput(text)))
            .on_submit(|_| cm(CloudSettingsMsg::FolderCreateSubmit))
            .width(Length::Fill),
    );
    for (icon, tooltip, message) in [
        ("emblem-ok-symbolic", "Create this folder", CloudSettingsMsg::FolderCreateSubmit),
        ("window-close-symbolic", "Cancel", CloudSettingsMsg::FolderCreateCancel),
    ] {
        row = row.push(widget::tooltip(
            crate::widgets::arrow_cursor::arrow_cursor(
                widget::button::icon(crate::widgets::icons::handle(icon))
                    .class(accent_icon_button_class(false))
                    .padding(2)
                    .on_press(cm(message)),
            ),
            widget::text(tooltip).size(12),
            widget::tooltip::Position::Top,
        ));
    }
    padded_folder_row(row)
}

/// Give a row the folder list's own inset, so anything added to the list lines up with the
/// rows already in it (DRAGON-514). One helper rather than a repeated literal: the create row
/// drifting away from the folder rows is what this ticket was reported for.
fn padded_folder_row<'a>(row: impl Into<Element<'a, Msg>>) -> Element<'a, Msg> {
    widget::container(row.into()).padding(FOLDER_ROW_PAD).width(Length::Fill).into()
}

// `stack_dialog` (this page's two dialogs, and every other one in the settings window) now
// lives in `settings::dialog`, reached through this file's `use super::super::*` glob. It
// moved there in DRAGON-502: its scrim grew a drag strip, and that strip has to be on EVERY
// dialog's scrim or the settings window stays pinned behind whichever one kept its own copy.

// ---------------------------------------------------------------------------
// The update body
// ---------------------------------------------------------------------------

impl crate::app::App {
    /// The Cloud Accounts page's update body: every `CloudSettingsMsg`, in one place.
    ///
    /// It lives beside the page rather than in `app::update::settings`, the way
    /// `update_preview` lives beside the preview editor: every arm here is a step of ONE
    /// flow, and the transitions are the pure functions above.
    ///
    /// **Nothing in this body blocks.** A connect, a folder listing, a revoke and a keyring
    /// read are all network or IPC, so each runs on the blocking pool and comes back as a
    /// message. The settings update loop must never wait on any of them.
    pub(in crate::app) fn update_cloud(
        &mut self,
        message: CloudSettingsMsg,
    ) -> Task<cosmic::Action<Msg>> {
        match message {
            CloudSettingsMsg::AddOpen => {
                self.settings.cloud.notice = None;
                self.settings.cloud.add = Some(CloudAdd::new());
                Task::none()
            }
            CloudSettingsMsg::AddClose => {
                // A connect already in flight CANNOT be aborted: the loopback listener owns
                // its own deadline and the provider's page is already open. So closing stops
                // CARING about it: the generation moves on and the eventual reply is
                // dropped, tokens and all. Discarding an authorization the user walked away
                // from is the honest end; adding an account minutes after they cancelled is
                // not.
                self.settings.cloud.generation += 1;
                self.settings.cloud.add = None;
                Task::none()
            }
            CloudSettingsMsg::AddProviderSelected(index) => {
                if let Some(add) = self.settings.cloud.add.as_mut() {
                    // An index the registry does not have changes nothing: the list is built
                    // from the registry, so this can only happen if the two ever disagree.
                    if index < crate::cloud::registry().len() {
                        add.provider = index;
                    }
                    add.menu_open = false;
                }
                Task::none()
            }
            CloudSettingsMsg::AddProviderMenu(open) => {
                if let Some(add) = self.settings.cloud.add.as_mut() {
                    add.menu_open = open;
                }
                Task::none()
            }
            CloudSettingsMsg::Connect => self.start_cloud_connect(),
            CloudSettingsMsg::BrowserUrl(generation, url) => {
                // Fill the URL in only on a step that is still waiting for the browser. The
                // URL is reported the instant the page opens, so this all but always lands
                // during the Browser step, but the guard means a late reply can never drag a
                // step that has already moved on (a completed sign-in now on the folder step)
                // back to waiting.
                if generation == self.settings.cloud.generation
                    && let Some(add) = self.settings.cloud.add.as_mut()
                    && matches!(add.step, CloudAddStep::Browser { .. })
                {
                    add.step = CloudAddStep::Browser { url: Some(url) };
                }
                Task::none()
            }
            CloudSettingsMsg::CopyBrowserUrl(url) => {
                crate::platform::services::copy_text(&url);
                // Stamp the copy so the button flashes "Copied!" (a green tick) for
                // `COPIED_FLASH`; the Browser-step tick reverts it with no explicit clear.
                if let Some(add) = self.settings.cloud.add.as_mut() {
                    add.copied_at = Some(std::time::Instant::now());
                }
                Task::none()
            }
            CloudSettingsMsg::OpenBrowserUrl(url) => {
                // No host check needed: this URL is our own code's output, built from the
                // hardcoded per-provider `auth_url`, so it is trusted before it reaches the UI
                // rather than provider-supplied network input. See
                // `CloudSettingsMsg::OpenBrowserUrl`.
                crate::platform::services::open_uri(&url);
                Task::none()
            }
            CloudSettingsMsg::OpenGuide => {
                // A compile-time constant, never a value that travelled through a message or
                // came back from a provider, so there is nothing here to check.
                crate::platform::services::open_uri(CLOUD_GUIDE_URL);
                Task::none()
            }
            CloudSettingsMsg::Connected(generation, result) => {
                self.finish_cloud_connect(generation, result)
            }
            CloudSettingsMsg::FoldersLoaded(generation, result) => {
                if generation != self.settings.cloud.generation {
                    return Task::none();
                }
                let Some(add) = self.settings.cloud.add.as_mut() else {
                    return Task::none();
                };
                add.folder_loading = false;
                // A fetch COMPLETED, whichever branch below it lands in, including the
                // ascend-and-retry one: `breadcrumb_first_load` only cares whether the FIRST
                // one has landed, not whether it succeeded (owner report, live testing; see
                // `CloudAdd::has_loaded_once`'s doc).
                add.has_loaded_once = true;
                let setup = match result {
                    // Not fatal: the account is already saved and already has a destination
                    // (its app folder), so the listing failing costs the user a choice, not an
                    // account. Unless the LEVEL itself is gone, which is not a failure of this
                    // step at all; see `listing_failure`.
                    Err(message) => {
                        let recovery = listing_failure(add.trail.len(), &message);
                        if recovery == ListingFailure::Note {
                            // Stripped on the way to the note: the missing-folder marker is
                            // machinery and must never reach the user, and a message carrying
                            // it can still land here (a provider that reported it for the app
                            // folder, which none does today, has nowhere to ascend to).
                            let shown = crate::cloud::providers::missing_folder_reason(&message);
                            add.folder_note = Some(classify_failure(shown).message().to_string());
                            return Task::none();
                        }
                        add.trail.pop();
                        add.folders.clear();
                        add.folder_note = None;
                        return self.relist_current_level();
                    }
                    Ok(setup) => setup,
                };
                // A listing that ARRIVED clears whatever the last one said went wrong, which is
                // also what returns the refresh control from Failed to a plain refresh button.
                add.folder_note = None;
                add.folders = setup.folders.into_iter().map(|f| (f.id, f.name)).collect();
                // Only a ROOT listing reports a root, and only a root listing may adopt one: a
                // descent deliberately does not re-find the app folder (`browse_folders`), so
                // writing `setup.root` unconditionally would clear Drive's known root id every
                // time the user went one level in.
                if add.trail.is_empty() {
                    add.root = setup.root.map(|r| (r.id, r.name));
                }
                // The browser is standing somewhere real, so Done has a level to write
                // (DRAGON-517). Nothing else here touches the account's destination: the level is
                // the destination, and this message cannot change the level. That is what makes
                // a refresh incapable of losing where the captures go, which is what
                // `surviving_selection` used to have to work for.
                add.browsed = true;
                self.adopt_app_folder()
            }
            CloudSettingsMsg::FolderViewRestored(generation, result) => {
                if generation != self.settings.cloud.generation {
                    return Task::none();
                }
                let Some(add) = self.settings.cloud.add.as_mut() else {
                    return Task::none();
                };
                add.folder_loading = false;
                // Same reasoning as `FoldersLoaded`: a fetch completed, success or failure.
                add.has_loaded_once = true;
                let view = match result {
                    // Not one level came back, so there is nothing to show and nothing to save.
                    // The account keeps whatever destination it already had (`CloudAdd::browsed`
                    // stays false), and the step says why on its notice line, exactly as an
                    // ordinary failed listing does.
                    Err(message) => {
                        add.folder_note = Some(classify_failure(&message).message().to_string());
                        return Task::none();
                    }
                    Ok(view) => view,
                };
                let pairs = |folders: Vec<crate::cloud::providers::RemoteFolder>| {
                    folders.into_iter().map(|f| (f.id, f.name)).collect::<Vec<_>>()
                };
                add.root = view.root.map(|r| (r.id, r.name));
                add.trail = pairs(view.trail);
                add.folders = pairs(view.folders);
                // A walk that a provider cut short opens at the app folder and says why; one that
                // simply could not find a folder any more opens at the deepest level that does
                // exist and lets the breadcrumb speak for itself. See
                // `cloud::providers::restore_browse_with`.
                add.folder_note =
                    view.note.map(|reason| classify_failure(&reason).message().to_string());
                add.browsed = true;
                self.adopt_app_folder()
            }
            CloudSettingsMsg::FolderRefresh => self.relist_current_level(),
            CloudSettingsMsg::FolderCrumbIn(index) => {
                let Some(add) = self.settings.cloud.add.as_mut() else {
                    return Task::none();
                };
                let Some(folder) = add.folders.get(index).cloned() else {
                    return Task::none();
                };
                add.trail.push(folder);
                // The old level's folders go NOW rather than when the new ones land: leaving
                // them on screen under the new breadcrumb would say the folder contains things
                // it does not.
                add.folders.clear();
                add.creating = None;
                self.relist_current_level()
            }
            CloudSettingsMsg::FolderCrumbTo(depth) => {
                let Some(add) = self.settings.cloud.add.as_mut() else {
                    return Task::none();
                };
                if depth >= add.trail.len() {
                    return Task::none();
                }
                add.trail.truncate(depth);
                add.folders.clear();
                add.creating = None;
                self.relist_current_level()
            }
            CloudSettingsMsg::FolderCreateOpen => {
                if let Some(add) = self.settings.cloud.add.as_mut() {
                    add.creating = Some(String::new());
                    add.folder_note = None;
                }
                Task::none()
            }
            CloudSettingsMsg::FolderCreateCancel => {
                if let Some(add) = self.settings.cloud.add.as_mut() {
                    add.creating = None;
                    add.create_busy = false;
                }
                Task::none()
            }
            CloudSettingsMsg::FolderCreateInput(text) => {
                // Buffered RAW, as the account's name field is: a user clearing it to retype
                // must see an empty field. `create_refusal` runs on submit instead.
                if let Some(add) = self.settings.cloud.add.as_mut() {
                    add.creating = Some(text);
                }
                Task::none()
            }
            CloudSettingsMsg::FolderCreateSubmit => self.create_cloud_folder(),
            CloudSettingsMsg::FolderCreated(generation, result) => {
                // **The busy flag is cleared even for a reply this page no longer cares about.**
                // The work really did finish, and the generation guard is about not letting a
                // stale answer touch the LIST; leaving the flag set would strand the inline row
                // on "Creating..." with no way back if a refresh or a navigation had superseded
                // it in between. Same reasoning in `FolderDeleted`.
                if let Some(add) = self.settings.cloud.add.as_mut() {
                    add.create_busy = false;
                }
                if generation != self.settings.cloud.generation {
                    return Task::none();
                }
                let Some(add) = self.settings.cloud.add.as_mut() else {
                    return Task::none();
                };
                match result {
                    Ok(_) => {
                        // The row closes and the LEVEL is re-listed rather than the new folder
                        // being pushed onto the list here: the provider decides the id, the
                        // final name and the sort position, and a locally invented row would be
                        // a second, possibly-disagreeing copy of all three.
                        add.creating = None;
                        self.relist_current_level()
                    }
                    Err(message) => {
                        // The typed name STAYS, so a name that was refused can be edited rather
                        // than retyped from nothing.
                        add.folder_note = Some(classify_failure(&message).message().to_string());
                        Task::none()
                    }
                }
            }
            CloudSettingsMsg::FolderDelete(index) => {
                let Some(add) = self.settings.cloud.add.as_mut() else {
                    return Task::none();
                };
                let Some((id, name)) = add.folders.get(index).cloned() else {
                    return Task::none();
                };
                // **Refused BEFORE anything is asked**, and refused out loud: the reason goes on
                // the step's own notice line, which is why the trash stays a live button on
                // every row (see `folder_entry`). The candidate is compared as a PATH, since a
                // destination further down is inside this folder and would go with it.
                let levels = add.level_names();
                let candidate = destination_path(&levels, Some(&name));
                let leaf = add
                    .spec()
                    .and_then(|spec| crate::cloud::app_folder_name(spec.id))
                    .unwrap_or(ROOT_DESTINATION);
                let chosen = add
                    .account
                    .as_ref()
                    .and_then(|a| a.folder_name.clone())
                    .map(|stored| crate::cloud::folder_path_segments(Some(&stored), leaf).join("/"));
                if let Some(reason) =
                    delete_refusal(candidate.as_deref().unwrap_or_default(), chosen.as_deref())
                {
                    add.folder_note = Some(reason.to_string());
                    return Task::none();
                }
                add.folder_note = None;
                add.pending_delete = Some((id, name));
                Task::none()
            }
            CloudSettingsMsg::FolderDeleteCancel => {
                if let Some(add) = self.settings.cloud.add.as_mut() {
                    add.pending_delete = None;
                }
                Task::none()
            }
            CloudSettingsMsg::FolderDeleteConfirm => self.delete_cloud_folder(),
            CloudSettingsMsg::FolderDeleted(generation, result) => {
                if let Some(add) = self.settings.cloud.add.as_mut() {
                    add.delete_busy = false;
                }
                if generation != self.settings.cloud.generation {
                    return Task::none();
                }
                let Some(add) = self.settings.cloud.add.as_mut() else {
                    return Task::none();
                };
                if let Err(message) = result {
                    add.folder_note = Some(classify_failure(&message).message().to_string());
                }
                // Re-listed either way: on success to lose the row, and on failure because the
                // one thing certain after a delete that reported an error is that the list on
                // screen is a guess.
                self.relist_current_level()
            }
            CloudSettingsMsg::AccountLabelInput(text) => {
                // Buffered straight onto the in-progress account: applied at `SetupDone`,
                // discarded if the dialog is cancelled instead.
                //
                // Buffered RAW, including an empty string: a user clearing the field to retype
                // it must see an empty field, not the default snapping back under their cursor.
                // `commit_label` is applied on the way to disk instead (`saved_account`), which
                // is the only place an empty name would actually matter.
                if let Some(add) = self.settings.cloud.add.as_mut()
                    && let Some(account) = add.account.as_mut()
                {
                    account.label = text;
                }
                Task::none()
            }
            CloudSettingsMsg::SetupDone => {
                // **This is where a destination is written** (DRAGON-517). It used to be written
                // the moment a folder was picked, because picking was a separate act from
                // browsing; with the level itself as the destination, every crumb press would
                // otherwise be a file write, and Cancel would have nothing left to cancel.
                let save = self.save_setup();
                self.settings.cloud.generation += 1;
                self.settings.cloud.add = None;
                save
            }
            CloudSettingsMsg::Reload => {
                self.settings.cloud.notice = None;
                self.settings.cloud.reload();
                self.probe_cloud_signins()
            }
            CloudSettingsMsg::HealthLoaded(ids) => {
                self.settings.cloud.reconnect = ids;
                Task::none()
            }
            CloudSettingsMsg::Reconnect(id) => {
                let account = self.settings.cloud.accounts.iter().find(|a| a.id == id).cloned();
                if let Some(account) = account {
                    self.settings.cloud.notice = None;
                    self.settings.cloud.add = Some(CloudAdd::reconnecting(account));
                }
                Task::none()
            }
            CloudSettingsMsg::EditAccount(id) => self.start_edit_account(id),
            CloudSettingsMsg::Disconnect(id) => {
                self.settings.cloud.pending_disconnect = Some(id);
                Task::none()
            }
            CloudSettingsMsg::DisconnectCancel => {
                self.settings.cloud.pending_disconnect = None;
                Task::none()
            }
            CloudSettingsMsg::DisconnectConfirm(id) => {
                self.settings.cloud.pending_disconnect = None;
                self.settings.cloud.disconnecting = Some(id.clone());
                self.disconnect_cloud_account(id)
            }
            CloudSettingsMsg::Disconnected(id, result) => {
                // Only clear THIS id's marker: a second disconnect for a different account
                // may have started in the meantime and taken the one slot.
                if self.settings.cloud.disconnecting.as_deref() == Some(id.as_str()) {
                    self.settings.cloud.disconnecting = None;
                }
                if let Err(message) = result {
                    self.settings.cloud.notice = Some(message);
                }
                self.settings.cloud.reload();
                self.probe_cloud_signins()
            }
            CloudSettingsMsg::CancelSetupStep => {
                let add = self.settings.cloud.add.take();
                let fresh_account =
                    add.and_then(|a| if a.is_fresh_connect { a.account } else { None });
                match fresh_account {
                    Some(account) => {
                        self.settings.cloud.disconnecting = Some(account.id.clone());
                        self.disconnect_cloud_account(account.id)
                    }
                    None => Task::none(),
                }
            }
            CloudSettingsMsg::CloudBrowserTick => Task::none(),
            CloudSettingsMsg::FolderSpinTick => {
                // One step of the refresh glyph's rotation (DRAGON-514). The same constant and
                // the same wrap the scanner's spinning re-read uses
                // (`update::capture`'s `ScanSpinTick`): a full turn in 30 ticks, at the ~30fps
                // the subscription ticks, so one revolution a second.
                const STEP: f32 = std::f32::consts::TAU / 30.0;
                if let Some(add) = self.settings.cloud.add.as_mut() {
                    add.folder_spin = (add.folder_spin + STEP) % std::f32::consts::TAU;
                }
                Task::none()
            }
        }
    }

    /// Start a connect, in the browser or with a device code.
    ///
    /// The refusal ([`ConnectAction`]) is decided HERE, before anything opens: a browser
    /// window that appears only to fail tells the user nothing.
    fn start_cloud_connect(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some((spec, existing)) = self
            .settings
            .cloud
            .add
            .as_ref()
            .and_then(|add| add.spec().map(|spec| (spec, add.reconnect_of.clone())))
        else {
            return Task::none();
        };
        let refusal = match connect_action(spec, std::env::var(client_id_env(spec)).ok().as_deref())
        {
            ConnectAction::Refused(message) => Some(message),
            ConnectAction::Run => None,
        };
        if let Some(message) = refusal {
            if let Some(add) = self.settings.cloud.add.as_mut() {
                add.step = CloudAddStep::Failed { message, retry: false };
            }
            return Task::none();
        }
        self.settings.cloud.generation += 1;
        let generation = self.settings.cloud.generation;
        if let Some(add) = self.settings.cloud.add.as_mut() {
            add.menu_open = false;
            add.browser_started = Some(std::time::Instant::now());
            add.step = CloudAddStep::Browser { url: None };
        }
        let provider_id = spec.id.to_string();
        let display_name = spec.display_name;
        // The browser flow reports its sign-in URL through a callback, mid-flight, so it
        // travels on a channel and a SECOND task delivers it as its own message. A plain
        // `std::sync::mpsc` on the blocking pool rather than an async channel: the sender is
        // called from blocking code, and a recv that ends when the sender drops is exactly
        // the lifetime wanted.
        let (url_tx, url_rx) = std::sync::mpsc::channel::<String>();
        let connect = Task::perform(
            off_thread(move || {
                connect_and_save(&provider_id, display_name, existing, &mut |url| {
                    let _ = url_tx.send(url.to_string());
                })
            }),
            move |result| {
                let result =
                    result.unwrap_or_else(|| Err("The sign-in could not be started.".to_string()));
                cosmic::Action::App(cm(CloudSettingsMsg::Connected(generation, result)))
            },
        );
        // A second worker waits on the URL channel and delivers whatever arrives as its own
        // message, so the sign-in URL reaches the UI without the connect task itself carrying
        // it. A `recv` that ends when the sender drops is exactly the lifetime wanted: the
        // connect closure owns the sender, so the moment it returns the channel closes and any
        // pending `recv` resolves. [`URL_RELAY_BUDGET`] is the outer ring on that (DRAGON-497),
        // since "until the other worker finishes" is a lifetime, not a bound.
        let browser_url = Task::perform(
            off_thread(move || url_rx.recv_timeout(URL_RELAY_BUDGET).ok()),
            move |url| match url.flatten() {
                Some(url) => {
                    cosmic::Action::App(cm(CloudSettingsMsg::BrowserUrl(generation, url)))
                }
                // The flow ended before it ever opened a page; the connect task's own reply is
                // what says why.
                None => cosmic::Action::App(Msg::WindowChrome(WindowChromeMsg::Ignore)),
            },
        );
        Task::batch([connect, browser_url])
    }

    /// A connect came back. The account is already saved (tokens and all) by the time this
    /// runs, so the only decision left is whether anything remains to ask the user.
    fn finish_cloud_connect(
        &mut self,
        generation: u64,
        result: Result<CloudAccount, String>,
    ) -> Task<cosmic::Action<Msg>> {
        if generation != self.settings.cloud.generation {
            log::debug!("cloud accounts: dropping the reply from a cancelled sign-in");
            return Task::none();
        }
        let account = match result {
            Ok(account) => account,
            Err(message) => {
                log::debug!("cloud accounts: a sign-in did not complete");
                if let Some(add) = self.settings.cloud.add.as_mut() {
                    add.step = failure_step(&message);
                }
                return Task::none();
            }
        };
        self.settings.cloud.reload();
        let is_reconnect =
            self.settings.cloud.add.as_ref().is_some_and(|add| add.reconnect_of.is_some());
        let next = account.spec().and_then(|spec| step_after_tokens(spec, is_reconnect));
        let Some(step) = next else {
            // Nothing left to choose: close, and re-probe so the row's Reconnect (which is
            // what a reconnect was fixing) clears.
            self.settings.cloud.generation += 1;
            self.settings.cloud.add = None;
            return self.probe_cloud_signins();
        };
        let browse = account.spec().is_some_and(chooses_folder);
        if let Some(add) = self.settings.cloud.add.as_mut() {
            add.step = step;
            add.folder_loading = browse;
            add.account = Some(account.clone());
        }
        let probe = self.probe_cloud_signins();
        if !browse {
            return probe;
        }
        Task::batch([probe, folder_restore_task(account, generation)])
    }

    /// Re-open the setup step for an already-connected account (the gear on its row,
    /// DRAGON-489 follow-up; the RENAME path since DRAGON-495). No OAuth:
    /// [`CloudAdd::edit_account`] jumps straight to the setup step, and this re-lists the
    /// account's folders the same way [`Self::finish_cloud_connect`] does once tokens land. A
    /// fresh generation is stamped (as every connect-adjacent flow does) so a stale reply from
    /// an earlier flow can never fill this dialog's list.
    ///
    /// A provider with no folders (YouTube) reaches this too, and simply has no listing to
    /// fetch: the step it opens carries the name field alone.
    fn start_edit_account(&mut self, id: String) -> Task<cosmic::Action<Msg>> {
        let Some(account) = self.settings.cloud.accounts.iter().find(|a| a.id == id).cloned()
        else {
            return Task::none();
        };
        self.settings.cloud.notice = None;
        let browse = account.spec().is_some_and(chooses_folder);
        self.settings.cloud.generation += 1;
        let generation = self.settings.cloud.generation;
        self.settings.cloud.add = Some(CloudAdd::edit_account(account.clone()));
        // A provider with no destination at all (YouTube) has no list to fetch: the dialog
        // shows the name field alone.
        if !browse {
            return Task::none();
        }
        folder_restore_task(account, generation)
    }

    /// List the level the setup step is currently showing (DRAGON-506).
    ///
    /// **The one way a folder listing is ever started after the dialog opens.** The refresh
    /// button, a descent, a walk back up the breadcrumb, a create that landed and a delete that
    /// finished all end here, so no two of them can decide the level, the generation or the
    /// loading flag differently.
    ///
    /// A fresh generation is stamped, as every connect-adjacent flow on this page does: a slower
    /// earlier listing must not land on top of the one this starts, which matters far more now
    /// that the level can change under it.
    ///
    /// **A listing already in flight is SUPERSEDED, never waited for.** This used to refuse to
    /// start while one was running, which was right while the refresh button was the only way in
    /// (and the footer hides that button while it is loading anyway) and became wrong the moment
    /// a press could change the LEVEL: a descent that declined to re-list would leave the older
    /// listing to land under the new breadcrumb, filling the list with the parent's contents and
    /// claiming they were the child's. Bumping the generation is what makes superseding safe: the
    /// abandoned reply is dropped on arrival, and this one's own reply clears the loading flag.
    fn relist_current_level(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(account) = self.settings.cloud.add.as_ref().and_then(|add| add.account.clone())
        else {
            return Task::none();
        };
        // Nothing to list at all: a provider with no folders (YouTube) reaches the setup step
        // and simply has no list on it.
        if !account.spec().is_some_and(chooses_folder) {
            return Task::none();
        }
        self.settings.cloud.generation += 1;
        let generation = self.settings.cloud.generation;
        let mut parent = None;
        if let Some(add) = self.settings.cloud.add.as_mut() {
            add.folder_loading = true;
            add.folder_note = None;
            parent = add.level_parent();
        }
        folder_level_task(account, parent, generation)
    }

    /// Create the folder named in the inline row, at the level being viewed (DRAGON-506).
    ///
    /// Everything refusable is refused HERE, before a request: the name's own rules and the one
    /// this level knows, a name already taken (see [`create_refusal`]). The reason goes on the
    /// step's notice line, which is the same place a failed create's does, so a user does not
    /// have to learn two places to look.
    fn create_cloud_folder(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(add) = self.settings.cloud.add.as_ref() else {
            return Task::none();
        };
        let (Some(typed), Some(account)) = (add.creating.clone(), add.account.clone()) else {
            return Task::none();
        };
        // A create already in flight: one press must not start two, which would be two folders.
        if add.create_busy {
            return Task::none();
        }
        let siblings: Vec<&str> = add.folders.iter().map(|(_, name)| name.as_str()).collect();
        if let Some(reason) = create_refusal(&typed, &siblings) {
            if let Some(add) = self.settings.cloud.add.as_mut() {
                add.folder_note = Some(reason);
            }
            return Task::none();
        }
        let parent = add.level_parent();
        self.settings.cloud.generation += 1;
        let generation = self.settings.cloud.generation;
        if let Some(add) = self.settings.cloud.add.as_mut() {
            add.create_busy = true;
            add.folder_note = None;
            // The generation bump above has ABANDONED any listing in flight, so the footer must
            // stop claiming to be reading one; the create's own reply re-lists the level. Same
            // reasoning in `delete_cloud_folder`.
            add.folder_loading = false;
        }
        Task::perform(
            off_thread(move || {
                crate::cloud::providers::create_folder(&account, parent.as_deref(), &typed)
            }),
            move |result| {
                let result =
                    result.unwrap_or_else(|| Err("The folder could not be created.".to_string()));
                cosmic::Action::App(cm(CloudSettingsMsg::FolderCreated(generation, result)))
            },
        )
    }

    /// Delete the folder the confirmation names (DRAGON-506).
    ///
    /// The refusals were applied when the confirmation was OPENED
    /// (`CloudSettingsMsg::FolderDelete`), which is where they belong: a question the user has
    /// already answered is the wrong place to tell them it should not have been asked.
    fn delete_cloud_folder(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(add) = self.settings.cloud.add.as_ref() else {
            return Task::none();
        };
        let (Some((folder, _)), Some(account)) = (add.pending_delete.clone(), add.account.clone())
        else {
            return Task::none();
        };
        if add.delete_busy {
            return Task::none();
        }
        self.settings.cloud.generation += 1;
        let generation = self.settings.cloud.generation;
        if let Some(add) = self.settings.cloud.add.as_mut() {
            add.pending_delete = None;
            add.delete_busy = true;
            add.folder_note = None;
            add.folder_loading = false;
        }
        Task::perform(
            off_thread(move || crate::cloud::providers::delete_folder(&account, &folder)),
            move |result| {
                let result =
                    result.unwrap_or_else(|| Err("The folder could not be deleted.".to_string()));
                cosmic::Action::App(cm(CloudSettingsMsg::FolderDeleted(generation, result)))
            },
        )
    }

    /// Write a destination onto the account being configured and save it.
    ///
    /// One caller since DRAGON-517: [`Self::adopt_app_folder`]. Browsing no longer writes
    /// anything, and Done writes through [`Self::save_setup`], which saves the name in the same
    /// upsert. This stays a separate path because the adoption happens mid-dialog, with the name
    /// field still being edited, and must not commit a half-typed label; `saved_account` is what
    /// keeps that decision in one place for both.
    fn set_cloud_destination(
        &mut self,
        folder_id: Option<String>,
        folder_name: Option<String>,
    ) -> Task<cosmic::Action<Msg>> {
        let Some(add) = self.settings.cloud.add.as_mut() else {
            return Task::none();
        };
        let Some(account) = add.account.as_mut() else {
            return Task::none();
        };
        account.folder_id = folder_id;
        account.folder_name = folder_name;
        // The in-progress account keeps whatever the name field currently holds (a half-typed
        // or momentarily empty one); only the copy going to disk is committed. See
        // `saved_account`.
        let saved = saved_account(account);
        if let Err(e) = crate::cloud::accounts::upsert(saved) {
            self.settings.cloud.notice = Some(e);
        }
        self.settings.cloud.reload();
        Task::none()
    }

    /// Persist the setup step: the account's name, and the level the browser is showing
    /// (DRAGON-517).
    ///
    /// ONE write, because they are one record. The name has always been buffered until here
    /// (DRAGON-489 follow-up, committed through [`commit_label`] on the way, DRAGON-495); the
    /// DESTINATION joined it when the browser's level became the destination, so a Cancel now
    /// really does leave the account exactly as it was found.
    ///
    /// **The folder is written only when there is a folder to write.** Two guards, and both
    /// exist to stop this clearing a destination it never learned:
    ///
    /// * The provider must HAVE folders ([`chooses_folder`]). A YouTube step is a name field and
    ///   nothing else, and its account's folder fields are not this step's business.
    /// * The browser must have actually shown a level (`CloudAdd::browsed`). An opening listing
    ///   that failed leaves no root and no trail, which [`viewed_destination`] would read as "the
    ///   app folder with no id" and write onto a Drive account as "the whole of My Drive".
    fn save_setup(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(add) = self.settings.cloud.add.as_ref() else {
            return Task::none();
        };
        let Some(mut account) = add.account.clone() else {
            return Task::none();
        };
        if add.browsed && account.spec().is_some_and(chooses_folder) {
            let (folder_id, folder_name) = viewed_destination(&add.trail, add.root.as_ref());
            account.folder_id = folder_id;
            account.folder_name = folder_name;
        }
        if let Err(e) = crate::cloud::accounts::upsert(saved_account(&account)) {
            self.settings.cloud.notice = Some(e);
        }
        self.settings.cloud.reload();
        Task::none()
    }

    /// Take the app folder as this account's destination, when it has never named one
    /// (DRAGON-498, re-homed by DRAGON-517).
    ///
    /// Run whenever a listing teaches the dialog what the app folder's id IS, which is the only
    /// moment the answer can change. [`adopts_app_folder`] is the rule; the root being `Some` is
    /// the other half of it, since the two providers whose token root already is that folder have
    /// no id to write and `None` there already means exactly this place.
    ///
    /// **Not the same thing as saving the browser's level.** That is [`Self::save_setup`], and it
    /// happens on Done. This covers the account that never reaches Done: a user who connects a
    /// Drive account and then closes the settings window would otherwise be left with a
    /// `folder_id` of `None`, and on Drive that is the whole of My Drive rather than the app
    /// folder.
    fn adopt_app_folder(&mut self) -> Task<cosmic::Action<Msg>> {
        let Some(add) = self.settings.cloud.add.as_ref() else {
            return Task::none();
        };
        let (Some((id, name)), Some(account)) = (add.root.as_ref(), add.account.as_ref()) else {
            return Task::none();
        };
        if !adopts_app_folder(account.folder_id.as_deref(), account.folder_name.as_deref()) {
            return Task::none();
        }
        let (id, name) = (id.clone(), name.clone());
        self.set_cloud_destination(Some(id), Some(name))
    }

    /// Re-read every account's stored sign-in on the blocking pool, and report which ones
    /// warrant a Reconnect. Off the UI thread because a keyring read is a D-Bus round trip
    /// on Linux, and there is one per account.
    fn probe_cloud_signins(&self) -> Task<cosmic::Action<Msg>> {
        let ids: Vec<String> =
            self.settings.cloud.accounts.iter().map(|a| a.id.clone()).collect();
        if ids.is_empty() {
            return Task::none();
        }
        Task::perform(
            off_thread(move || {
                let now = chrono::Utc::now();
                ids.into_iter()
                    .filter(|id| {
                        let stored = crate::cloud::secrets::load(id)
                            .ok()
                            .flatten()
                            .and_then(|json| TokenSet::from_json(&json).ok());
                        reconnect_warranted(stored.as_ref(), now)
                    })
                    .collect::<Vec<String>>()
            }),
            |ids| cosmic::Action::App(cm(CloudSettingsMsg::HealthLoaded(ids.unwrap_or_default()))),
        )
    }

    /// Revoke (best effort), then drop the tokens and the account.
    fn disconnect_cloud_account(&mut self, id: String) -> Task<cosmic::Action<Msg>> {
        let account = self.settings.cloud.accounts.iter().find(|a| a.id == id).cloned();
        Task::perform(
            async move {
                let result = off_thread({
                    let id = id.clone();
                    move || disconnect_account(account.as_ref(), &id)
                })
                .await
                .unwrap_or_else(|| Err("The account could not be disconnected.".to_string()));
                (id, result)
            },
            |(id, result)| cosmic::Action::App(cm(CloudSettingsMsg::Disconnected(id, result))),
        )
    }
}

/// The copy of an in-progress account that goes to DISK: the same record with its name
/// committed through [`commit_label`] (DRAGON-495).
///
/// A copy rather than an edit in place, so a momentarily empty name field stays empty while the
/// user retypes it and only the SAVED record carries the fallback. Every `upsert` on this page
/// goes through here, so the two save paths ([`crate::app::App::set_cloud_destination`] and
/// [`crate::app::App::save_setup`]) cannot disagree about what an empty field means.
///
/// Today's date is read here rather than injected, because the only thing it can affect is an
/// account whose own `added_at` is unusable; [`default_label`] is where that decision lives and
/// takes the date as an argument, which is what keeps it testable.
fn saved_account(account: &CloudAccount) -> CloudAccount {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut saved = account.clone();
    saved.label = commit_label(&account.label, &default_label(account, &today));
    saved
}

/// Open the folder browser INSIDE an account's saved destination, off the UI thread
/// (DRAGON-517).
///
/// One helper for the TWO ways the setup step is reached, a fresh connect and the gear on a row,
/// because they must ask the same question: an account whose folders were listed one way and
/// adopted the other would get a different default depending on how the step was opened. A fresh
/// connect stores no destination yet, so its walk is one listing of the app folder and stops,
/// which is exactly the `folder_setup_task` this replaced (DRAGON-498).
///
/// The account's stored destination is a PATH of display names plus the destination's own id, so
/// the walk is [`crate::cloud::providers::restore_browse`]'s: list a level, find the segment,
/// descend. It is ONE detached job however many levels that takes, so the step spins its refresh
/// glyph once and lands once. `generation` stamps the reply so a late one cannot fill a dialog
/// that has moved on.
///
/// On a DETACHED thread through [`crate::app::off_thread`], like every other job this page starts
/// (DRAGON-497; see the module doc). Putting it on the blocking pool would quietly hand the
/// settings close its wedge back: an account whose provider is slow to answer is exactly the case
/// a user closes the window on.
fn folder_restore_task(account: CloudAccount, generation: u64) -> Task<cosmic::Action<Msg>> {
    let leaf = crate::cloud::app_folder_name(&account.provider).unwrap_or(ROOT_DESTINATION);
    // Split HERE, through the one splitter `cloud::save_path` also reads, so the browser and the
    // account's row cannot disagree about what a level of a stored path is.
    let segments: Vec<String> = crate::cloud::folder_path_segments(account.folder_name.as_deref(), leaf)
        .into_iter()
        .map(str::to_string)
        .collect();
    let destination = account.folder_id.clone();
    Task::perform(
        off_thread(move || {
            let names: Vec<&str> = segments.iter().map(String::as_str).collect();
            crate::cloud::providers::restore_browse(&account, &names, destination.as_deref())
        }),
        move |result| {
            let result =
                result.unwrap_or_else(|| Err("The folder list could not be read.".to_string()));
            cosmic::Action::App(cm(CloudSettingsMsg::FolderViewRestored(generation, result)))
        },
    )
}

/// Fetch ONE level of an account's folder tree, off the UI thread (DRAGON-506).
///
/// `parent` is the folder to look inside, `None` being the app folder. Every call goes through
/// [`crate::app::App::relist_current_level`], the only caller that knows which level is on screen:
/// a refresh, a descent, a walk back up the breadcrumb, a create that landed and a delete that
/// finished. Opening the dialog is the one thing that does NOT come here, because it may have
/// several levels to walk; see [`folder_restore_task`].
fn folder_level_task(
    account: CloudAccount,
    parent: Option<String>,
    generation: u64,
) -> Task<cosmic::Action<Msg>> {
    Task::perform(
        off_thread(move || crate::cloud::providers::browse_folders(&account, parent.as_deref())),
        move |result| {
            let result =
                result.unwrap_or_else(|| Err("The folder list could not be read.".to_string()));
            cosmic::Action::App(cm(CloudSettingsMsg::FoldersLoaded(generation, result)))
        },
    )
}

/// Run the OAuth flow and persist what it returns. **Blocking**; runs on the blocking pool.
///
/// The order is deliberate: the tokens are stored FIRST and the account row second, so an
/// account can never exist without its sign-in. The reverse order would leave a listed
/// account that cannot upload and cannot even say why.
///
/// `existing` is the account being RECONNECTED, if any. It keeps its id, so the new tokens
/// land on the same keyring item and its folder and visibility choices survive.
fn connect_and_save(
    provider_id: &str,
    display_name: &'static str,
    existing: Option<CloudAccount>,
    on_browser_url: &mut dyn FnMut(&str),
) -> Result<CloudAccount, String> {
    // Report the sign-in URL back to the UI rather than opening it (DRAGON-489 follow-up): the
    // browser step shows it as a clickable link, and the user opens it
    // themselves. An earlier pass opened it automatically here too, but an automatic launch can
    // fail silently (no default browser set, the wrong browser opens, a headless/remote
    // session) with nothing else on screen to fall back to, so the decision is now the user's,
    // not this closure's.
    let connected = crate::cloud::oauth::connect_interactive_with(
        provider_id,
        crate::cloud::oauth::BROWSER_DEADLINE,
        &mut |url| on_browser_url(url),
    )?;
    // The ONE place this page holds a secret, and it holds it only as far as the store.
    let secret = connected.tokens.to_json()?;
    let account = match existing {
        Some(account) => account,
        None => {
            let id = crate::cloud::accounts::new_id();
            if id.is_empty() {
                return Err("This computer's random source is unavailable, so the account \
                            could not be created."
                    .to_string());
            }
            CloudAccount {
                id,
                provider: connected.provider.clone(),
                // The token exchange reports no identity (see `derive_label`), so this is
                // the fallback until one is available.
                label: derive_label(
                    None,
                    display_name,
                    &chrono::Local::now().format("%Y-%m-%d").to_string(),
                ),
                folder_id: None,
                folder_name: None,
                added_at: chrono::Utc::now().to_rfc3339(),
                visibility: None,
            }
        }
    };
    crate::cloud::secrets::store(&account.id, &secret)?;
    crate::cloud::accounts::upsert(account.clone())?;
    log::debug!("cloud accounts: connected a {} account", account.provider);
    Ok(account)
}

/// Disconnect one account. **Blocking**; runs on the blocking pool.
///
/// The revoke is best effort by design: a provider that refuses, or an authorization already
/// gone, must not block the removal, because the dead account is the one a user most wants
/// off their machine. The two LOCAL removals are what the result reports.
fn disconnect_account(account: Option<&CloudAccount>, id: &str) -> Result<(), String> {
    if let Some(account) = account
        && let Err(e) = crate::cloud::providers::revoke(account)
    {
        log::debug!("cloud accounts: the provider did not revoke the sign-in ({e})");
    }
    let dropped_secret = crate::cloud::secrets::delete(id);
    let removed = crate::cloud::accounts::remove(id);
    removed?;
    dropped_secret
}

#[cfg(test)]
mod provider_list_tests {
    use super::*;

    /// Describe a BUILD without touching the real environment, exactly as
    /// `cloud::registry_tests` does: a real `set_var` would read whatever the developer has
    /// exported and would make these tests order-dependent against the rest of the binary.
    fn fake_env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| (*value).to_string())
        }
    }

    /// **The picker lists what can be picked, and nothing else** (DRAGON-498, one rule since
    /// DRAGON-508). Every row it offers is a provider a press of Connect can actually START,
    /// which now means one whose client id resolves. The registry still knows all six, which is
    /// what the coverage answers elsewhere read.
    #[test]
    fn the_picker_lists_only_providers_this_build_can_connect() {
        assert_eq!(crate::cloud::registry().len(), 6);
        let offered = crate::cloud::connectable_display_order_with(fake_env(&[
            ("CCK_GDRIVE_CLIENT_ID", "g-id"),
            ("CCK_DROPBOX_CLIENT_ID", "d-id"),
        ]));
        let ids: Vec<&str> = offered.iter().map(|(_, spec)| spec.id).collect();
        assert_eq!(ids, ["dropbox", "gdrive"], "only the two with ids, in brand order");
        for (index, spec) in &offered {
            assert_ne!(spec.auth, AuthKind::Unofficial, "{} cannot be connected", spec.id);
            // Every row carries its REGISTRY index, which is what `AddProviderSelected` sends
            // back and what `CloudAdd::provider` then resolves against.
            assert_eq!(crate::cloud::registry()[*index].id, spec.id);
            // And a row that is offered is one Connect can press: the list and the button ask
            // the same question, so they cannot drift.
            assert_eq!(
                connect_action(spec, Some("an-app-id")),
                ConnectAction::Run,
                "{} is offered but refuses",
                spec.id
            );
        }
        for hidden in ["proton", "icloud", "onedrive", "youtube"] {
            assert!(
                !offered.iter().any(|(_, spec)| spec.id == hidden),
                "{hidden} has no client id here and must not be offered"
            );
            assert!(crate::cloud::provider(hidden).is_some(), "{hidden} left the registry");
        }
    }

    /// **The empty state's trigger** (DRAGON-508): a fresh add with nothing to offer shows it,
    /// and everything else shows the picker. A reconnect never does, however unconfigured the
    /// build is: it is fixing one named account, and its provider's own sentence is the honest
    /// answer there.
    #[test]
    fn the_empty_state_is_for_a_fresh_add_with_nothing_to_offer() {
        assert!(shows_empty_state(false, 0), "a build with no providers says so");
        assert!(!shows_empty_state(false, 1), "one provider is a picker, not an empty state");
        assert!(!shows_empty_state(false, 4));
        assert!(!shows_empty_state(true, 0), "a reconnect keeps its own provider's refusal");
        assert!(!shows_empty_state(true, 4));
    }

    /// The guide link is a compile-time constant pointing at the documentation site's copy of
    /// `CLOUD_ACCOUNTS.md`, and it has to stay an https URL: it goes to the OS opener.
    #[test]
    fn the_guide_url_is_a_documentation_page() {
        assert!(CLOUD_GUIDE_URL.starts_with("https://"), "{CLOUD_GUIDE_URL}");
        assert!(CLOUD_GUIDE_URL.contains("cloud-accounts"), "{CLOUD_GUIDE_URL}");
    }

    /// The dialog opens on something a user can actually press Connect on, whenever there IS
    /// something. With nothing offered the index is meaningless and unread (the empty state
    /// draws instead), which is what the fallback exists for.
    #[test]
    fn the_dialog_opens_on_a_connectable_provider() {
        let offered = crate::cloud::connectable_display_order();
        let index = first_connectable();
        if let Some((first, _)) = offered.first() {
            assert_eq!(index, *first, "the dialog must open on the list's first row");
            assert!(crate::cloud::registry()[index].auth.is_connectable());
        } else {
            assert_eq!(index, 0, "with nothing offered the fallback is the first registry row");
        }
    }

    /// A fresh dialog starts on the provider step with nothing chosen yet; a reconnecting one
    /// starts on the SAME provider as the account it is fixing.
    #[test]
    fn a_reconnect_dialog_locks_onto_the_accounts_provider() {
        let fresh = CloudAdd::new();
        assert_eq!(fresh.step, CloudAddStep::Provider);
        assert!(fresh.reconnect_of.is_none());
        let account = CloudAccount {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            provider: "dropbox".to_string(),
            label: "Personal".to_string(),
            folder_id: None,
            folder_name: None,
            added_at: String::new(),
            visibility: None,
        };
        let add = CloudAdd::reconnecting(account);
        assert_eq!(add.spec().map(|s| s.id), Some("dropbox"));
        assert!(add.reconnect_of.is_some());
    }

    /// The gear's `edit_account` dialog (DRAGON-489 follow-up) jumps straight to the setup step
    /// for an already-connected account, and is NEITHER a fresh connect (so Cancel must not
    /// disconnect it) NOR a reconnect (nothing here refreshes tokens). It carries the account's
    /// current destination so the folder list can check-mark it, and a browsable provider opens
    /// already loading its list.
    #[test]
    fn an_edit_account_dialog_opens_on_the_setup_step_without_reconnecting() {
        let account = CloudAccount {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            provider: "gdrive".to_string(),
            label: "Work".to_string(),
            folder_id: Some("folder-1".to_string()),
            folder_name: Some("Screenshots".to_string()),
            added_at: String::new(),
            visibility: None,
        };
        let add = CloudAdd::edit_account(account.clone());
        assert_eq!(add.step, CloudAddStep::Setup);
        assert!(add.reconnect_of.is_none(), "not a reconnect");
        assert!(!add.is_fresh_connect, "not a fresh connect: Cancel must not disconnect it");
        assert_eq!(add.account.as_ref().map(|a| a.id.as_str()), Some(account.id.as_str()));
        // gdrive browses, so the dialog opens already listing its folders.
        assert!(add.folder_loading, "a browsable provider starts loading its folder list");
        // Nothing has landed yet, so this opening fetch is the FIRST-EVER load
        // (`breadcrumb_first_load`): the breadcrumb shows "Loading..." right when the dialog
        // opens, not the real path.
        assert!(!add.has_loaded_once, "a fresh edit-account dialog has never loaded a listing");
        assert!(breadcrumb_first_load(add.folder_loading, add.has_loaded_once));
        assert_eq!(add.account.and_then(|a| a.folder_id), Some("folder-1".to_string()));
    }
}

#[cfg(test)]
mod connect_action_tests {
    use super::*;

    fn spec(id: &str) -> &'static ProviderSpec {
        crate::cloud::provider(id).expect("a registry provider")
    }

    /// An unofficial provider is refused before anything is opened, and the sentence names
    /// it rather than dumping an error.
    #[test]
    fn an_unofficial_provider_is_refused_by_name() {
        let ConnectAction::Refused(message) = connect_action(spec("proton"), None) else {
            panic!("proton must be refused");
        };
        assert!(message.contains("Proton Drive"), "{message}");
    }

    /// With no client id anywhere, the message names the provider AND the variable that
    /// supplies one, which is the whole point of catching this before the browser opens. Since
    /// DRAGON-508 this is a `Refused` like any other: the picker never offers such a provider,
    /// so the one way here is a reconnect on an account whose variable is no longer set.
    #[test]
    fn a_missing_client_id_names_the_variable() {
        let ConnectAction::Refused(message) = connect_action(spec("gdrive"), None) else {
            panic!("a build with no client id must not start a flow");
        };
        assert!(message.contains("Google Drive"), "{message}");
        assert!(message.contains("CCK_GDRIVE_CLIENT_ID"), "{message}");
    }

    /// An id supplied through the environment is what makes a source build workable, so it
    /// has to reach `Run`.
    #[test]
    fn an_environment_client_id_lets_the_flow_run() {
        assert_eq!(connect_action(spec("dropbox"), Some("an-app-id")), ConnectAction::Run);
        // Whitespace is not an id.
        assert!(matches!(connect_action(spec("dropbox"), Some("   ")), ConnectAction::Refused(_)));
    }

    /// The refusal has to be GUIDANCE. It is the only thing a user sees when a connect cannot
    /// start, so it names the provider, names the exact variable, and says what to do with it.
    #[test]
    fn the_refusal_tells_the_user_what_to_do() {
        let ConnectAction::Refused(message) = connect_action(spec("dropbox"), None) else {
            panic!("a build with no client id must refuse");
        };
        assert!(message.contains("Dropbox"), "{message}");
        assert!(message.contains("CCK_DROPBOX_CLIENT_ID"), "{message}");
        assert!(message.contains("set the"), "the message must say what to set: {message}");
        assert!(!message.contains('—'), "house style: no em-dashes in user copy");
    }

    /// **Connect is pressable exactly when a press can start a flow** (DRAGON-482 review,
    /// re-answered by DRAGON-508).
    ///
    /// The regression the original rule existed for: Connect was drawn inert on a build with no
    /// client id, which put the explaining sentence behind a button that could not be pressed,
    /// and the owner read the result as a broken dialog. The rule now holds by keeping such a
    /// provider OUT of the picker entirely, so an inert Connect is reachable only where the
    /// dialog has already said, in full, why (a reconnect).
    #[test]
    fn connect_is_pressable_only_where_a_press_can_run() {
        assert!(connect_pressable(&ConnectAction::Run));
        assert!(!connect_pressable(&ConnectAction::Refused("no api".to_string())));
        // …and through the real registry: with an id supplied, every OAuth provider is
        // pressable, and neither unofficial one ever is.
        for provider in crate::cloud::registry() {
            assert_eq!(
                connect_pressable(&connect_action(provider, Some("an-app-id"))),
                provider.auth.is_connectable(),
                "`{}` disagrees about being pressable",
                provider.id
            );
            // With NO id, nothing is pressable, which is the state the picker now hides.
            assert!(
                !connect_pressable(&connect_action(provider, None)),
                "`{}` offers a press that cannot run",
                provider.id
            );
        }
    }

    /// The caption under the control is the refusal's own sentence, and only a refused provider
    /// has one. The short "this build has no X app registration yet" line that used to sit here
    /// went with `ConnectAction::NoClientId`; the dialog's empty state carries that on-ramp now,
    /// once instead of once per provider.
    #[test]
    fn a_note_appears_only_where_a_connect_would_refuse() {
        // Dropbox with no id: one line, and it is the guidance sentence.
        let dropbox = provider_notes(spec("dropbox"), None);
        assert_eq!(dropbox.len(), 1, "{dropbox:?}");
        assert!(dropbox[0].contains("Dropbox"), "{dropbox:?}");
        assert!(dropbox[0].contains("CCK_DROPBOX_CLIENT_ID"), "{dropbox:?}");

        // Google, same shape.
        let google = provider_notes(spec("gdrive"), None);
        assert_eq!(google.len(), 1, "{google:?}");
        assert!(google[0].contains("Google Drive"), "{google:?}");

        // With an id supplied, either provider says nothing at all.
        assert!(provider_notes(spec("gdrive"), Some("an-app-id")).is_empty());
        assert!(provider_notes(spec("dropbox"), Some("an-app-id")).is_empty());
    }

    /// An unconnectable provider says ONE thing, and it is that it cannot be connected. The
    /// app-registration line would be noise on a provider that has no registration to make.
    ///
    /// Unreachable from the picker since DRAGON-498 (it lists connectable providers only), so
    /// this pins the guard rather than a screen: the one way in is a hand-edited accounts file
    /// naming `proton`, whose row's Reconnect would open the dialog on that spec.
    #[test]
    fn an_unofficial_provider_says_only_that_it_cannot_be_connected() {
        for id in ["proton", "icloud"] {
            let notes = provider_notes(spec(id), None);
            assert_eq!(notes.len(), 1, "{notes:?}");
            assert!(notes[0].contains("no public API"), "{notes:?}");
            assert!(notes[0].contains(spec(id).display_name), "{notes:?}");
            assert!(!notes[0].contains("app registration"), "{notes:?}");
            // An id in the environment changes nothing: there is nothing to connect to.
            assert_eq!(provider_notes(spec(id), Some("an-app-id")), notes);
        }
    }

    /// The caption and the button cannot drift apart: a note under the control means the
    /// button is inert, and an inert button means there is a note saying why. A dead control
    /// with nothing beside it is the shape the owner reported as broken, and it must stay
    /// unreachable in both directions.
    #[test]
    fn a_note_and_an_inert_connect_always_travel_together() {
        for provider in crate::cloud::registry() {
            for env_value in [None, Some("an-app-id")] {
                let notes = provider_notes(provider, env_value);
                let pressable = connect_pressable(&connect_action(provider, env_value));
                assert_eq!(
                    notes.is_empty(),
                    pressable,
                    "`{}` draws a dead button with no explanation, or an explanation with a \
                     live button",
                    provider.id
                );
            }
        }
    }
}

#[cfg(test)]
mod step_tests {
    use super::*;

    fn spec(id: &str) -> &'static ProviderSpec {
        crate::cloud::provider(id).expect("a registry provider")
    }

    /// Adding an account ends on the setup step; reconnecting one does NOT, because the
    /// account already has its name and its destination, and re-asking would be a chance to
    /// lose either.
    #[test]
    fn a_reconnect_skips_the_setup_step() {
        assert_eq!(step_after_tokens(spec("gdrive"), false), Some(CloudAddStep::Setup));
        assert_eq!(step_after_tokens(spec("gdrive"), true), None);
    }

    /// **DRAGON-495, the whole of item 2.** A connectable provider with no folder concept at
    /// all (YouTube) used to skip the step entirely on a fresh connect, on the reasoning that
    /// there was nothing to ask about. The step now carries the account's NAME, which every
    /// provider has, so it is shown for every provider: skipping it is what left a YouTube
    /// account nobody could rename.
    #[test]
    fn every_fresh_connect_gets_the_setup_step_even_with_no_folders() {
        for provider in crate::cloud::registry() {
            assert_eq!(
                step_after_tokens(provider, false),
                Some(CloudAddStep::Setup),
                "`{}` skips the setup step on a fresh connect",
                provider.id
            );
            assert_eq!(step_after_tokens(provider, true), None, "`{}` re-asks", provider.id);
        }
        // The case that reported the bug, named on its own so a regression says so plainly.
        assert_eq!(step_after_tokens(spec("youtube"), false), Some(CloudAddStep::Setup));
    }

    /// The folder step's shape per provider, which since DRAGON-498 is the capability table and
    /// nothing else: every file-storage drive lists the folders inside its app folder, and
    /// YouTube has no folder concept at all (a video upload has no destination FOLDER to
    /// choose), the same non-shape Proton and iCloud have for the unrelated reason of having no
    /// API at all.
    #[test]
    fn the_folder_step_follows_the_capability_table_alone() {
        for id in ["gdrive", "onedrive", "dropbox"] {
            assert!(chooses_folder(spec(id)), "{id} lists folders");
        }
        for id in ["youtube", "proton", "icloud"] {
            assert!(!chooses_folder(spec(id)), "{id} has no folder to choose");
        }
        // **Nothing is keyed on a provider id any more.** Dropbox used to be the one arm that
        // was (`typed_path`), and its removal is what makes this a pure read of the caps.
        for spec in crate::cloud::registry() {
            assert_eq!(chooses_folder(spec), spec.caps.folder_browse, "{}", spec.id);
        }
    }

    /// The step says what it can actually deliver: a folder-configuring heading only where there
    /// are folders, and a naming step everywhere else (DRAGON-495). "Configure your cloud
    /// storage" on a YouTube step would promise a folder screen the dialog cannot deliver.
    #[rstest::rstest]
    #[case("gdrive", "Configure your cloud storage")]
    #[case("dropbox", "Configure your cloud storage")]
    #[case("onedrive", "Configure your cloud storage")]
    #[case("youtube", "Name this account")]
    fn the_setup_step_asks_only_what_the_provider_can_answer(
        #[case] id: &str,
        #[case] expected: &str,
    ) {
        let browse = chooses_folder(spec(id));
        assert_eq!(setup_title(browse), expected);
        let body = setup_body(spec(id), browse);
        // The folderless body still names the provider; the folder body no longer does (the owner's
        // newer copy speaks only of "this account", the destination is the same one folder however
        // the account was reached).
        if !browse {
            assert!(body.contains(spec(id).display_name), "the sentence names the provider: {body}");
        }
        assert_eq!(
            body.to_lowercase().contains("folder"),
            browse,
            "the sentence mentions folders exactly when there are folders: {body}"
        );
        // **The sandbox is stated, not implied** (DRAGON-498): a folder step that only said
        // "the folder you choose here" left a user with no way to know the choice is confined
        // to one folder, which is the whole shape of the feature. The LIMIT half of that moved
        // to its own line in DRAGON-503; the body still names the folder, though the owner's
        // latest copy names it generically ("the App's Folder") rather than by the registry's
        // per-provider literal.
        if browse {
            assert!(body.contains("App's Folder"), "the app folder is named: {body}");
        }
        assert!(!body.contains('—'), "house style: no em-dashes in user copy");
    }

    /// **The owner's wording, verbatim**, latest revision, for every provider that has a folder:
    /// the body states where captures land, as a fixed generic phrase ("the App's Folder", no
    /// per-provider name), and the limit is its own line. Pinned as whole sentences, because this
    /// copy was supplied word for word and a later edit that "improves" it should have to say so
    /// here.
    #[rstest::rstest]
    #[case("gdrive", "Google Drive")]
    #[case("onedrive", "OneDrive")]
    #[case("dropbox", "Dropbox")]
    fn the_folder_step_carries_the_owners_copy(#[case] id: &str, #[case] name: &str) {
        let spec = spec(id);
        assert_eq!(
            setup_body(spec, true),
            "Captures uploaded to this account are placed in the App's Folder."
        );
        assert_eq!(
            setup_sandbox_note(spec, true),
            Some(format!("This app cannot see or touch anything else in your {name} storage."))
        );
        for line in [setup_body(spec, true), setup_sandbox_note(spec, true).unwrap()] {
            assert!(!line.contains('—'), "house style: no em-dashes in user copy: {line}");
        }
        // `app_folder_name` no longer feeds the body sentence (it went generic), but it still
        // agrees with the app-wide constant for every provider today, which is what keeps it
        // worth keeping around rather than deleting.
        assert_eq!(app_folder_name(spec), crate::cloud::APP_FOLDER);
    }

    /// A step with no folder list makes no promise about folders: no limit line, and the body it
    /// already had (DRAGON-495) is untouched by the DRAGON-503 copy.
    #[test]
    fn a_folderless_step_says_nothing_about_storage() {
        let youtube = spec("youtube");
        assert_eq!(setup_sandbox_note(youtube, chooses_folder(youtube)), None);
        assert_eq!(
            setup_body(youtube, false),
            "This YouTube account is connected. Give it a name you will recognise when you pick \
             where to upload."
        );
    }

    /// The gear is offered on EVERY row, and says the same thing on each (DRAGON-495): it used
    /// to be gated on the provider having folders, which is what left a YouTube row with no way
    /// to be renamed at all.
    #[test]
    fn the_row_offers_configuration_for_every_provider() {
        assert_eq!(CONFIGURE_TOOLTIP, "Configure this account");
        // The gate that used to hide the button is gone from the row; the only thing that
        // still varies per provider is what the step CONTAINS.
        assert!(!chooses_folder(spec("youtube")));
        assert_eq!(CloudAdd::edit_account(account_of("youtube")).step, CloudAddStep::Setup);
        assert_eq!(CloudAdd::edit_account(account_of("gdrive")).step, CloudAddStep::Setup);
        // A folderless provider has no listing to wait for, so its dialog does not open
        // "loading"; a browsable one does.
        assert!(!CloudAdd::edit_account(account_of("youtube")).folder_loading);
        assert!(CloudAdd::edit_account(account_of("gdrive")).folder_loading);
    }

    /// The refresh control only ever speaks for a provider that HAS folders (DRAGON-495): the
    /// step is reachable for one that has none, and it must neither claim to be reading them nor
    /// offer to read them again. The three live faces are DRAGON-503's, now drawn in the path row
    /// rather than the dialog's footer (DRAGON-514).
    ///
    /// **There is no text face.** The states are spinning, still-and-pressable, and
    /// still-and-worded-as-a-retry; the loading STRING this used to carry is deleted.
    #[rstest::rstest]
    // A browsable provider, through the whole cycle: opening (the icon spins), landed (a still
    // refresh button), failed (the same button, worded as a retry).
    #[case("gdrive", true, false, Some(FolderSlot::Loading))]
    #[case("gdrive", false, false, Some(FolderSlot::Idle))]
    #[case("gdrive", false, true, Some(FolderSlot::Failed))]
    // A retry in flight is the newer fact: loading outranks the failure it is trying to clear.
    #[case("gdrive", true, true, Some(FolderSlot::Loading))]
    // A folderless provider has no control in any combination.
    #[case("youtube", true, false, None)]
    #[case("youtube", false, false, None)]
    #[case("youtube", false, true, None)]
    fn the_refresh_control_tracks_the_folder_listing(
        #[case] id: &str,
        #[case] loading: bool,
        #[case] failed: bool,
        #[case] expected: Option<FolderSlot>,
    ) {
        assert_eq!(folder_slot(chooses_folder(spec(id)), loading, failed), expected);
    }

    /// **Only the very first fetch this dialog session says "Loading..."** (owner report, live
    /// testing). `folder_loading` alone cannot tell a first load from a refresh or a descent,
    /// since all three set it; `has_loaded_once` is the tiebreaker, and it must win in both
    /// directions: a fetch in flight before anything has landed says so, and one in flight
    /// AFTER something already landed (a refresh, a descent, an ascent) does not.
    #[rstest::rstest]
    #[case(true, false, true)] // in flight, nothing has ever landed: the first load
    #[case(true, true, false)] // in flight, something already landed: an ordinary reload
    #[case(false, false, false)] // idle, nothing has landed: not loading at all
    #[case(false, true, false)] // idle, already landed: the ordinary settled state
    fn only_the_first_ever_fetch_reads_as_the_first_load(
        #[case] folder_loading: bool,
        #[case] has_loaded_once: bool,
        #[case] expected: bool,
    ) {
        assert_eq!(breadcrumb_first_load(folder_loading, has_loaded_once), expected);
    }

    /// **A listing in flight is a SPIN and nothing else** (DRAGON-514). The one state that used
    /// to put a sentence on screen is now the one state with no press, and the difference
    /// between it and the other two is a rotation, not a control appearing.
    #[test]
    fn only_the_loading_face_spins_and_none_of_them_carry_text() {
        let faces = [FolderSlot::Loading, FolderSlot::Idle, FolderSlot::Failed];
        // Exactly one of the three is the animated, unpressable one.
        assert_eq!(faces.iter().filter(|f| **f == FolderSlot::Loading).count(), 1);
        // The listing state is reachable for a browsable provider and never for a folderless
        // one, which is what keeps a YouTube step from spinning at a list it does not have.
        assert_eq!(folder_slot(true, true, false), Some(FolderSlot::Loading));
        assert_eq!(folder_slot(false, true, false), None);
    }

    /// **The tick that drives the spin is gated on the listing** (DRAGON-514), so an idle dialog
    /// costs nothing. This is the predicate `subscriptions::sub_cloud_folder_spin` reads.
    #[test]
    fn the_spin_ticks_only_while_a_listing_is_in_flight() {
        let mut state = CloudPageState::default();
        assert!(!state.listing_folders(), "no dialog open at all");
        state.add = Some(CloudAdd::edit_account(account_of("gdrive")));
        assert!(state.listing_folders(), "a browsable dialog opens mid-listing");
        if let Some(add) = state.add.as_mut() {
            add.folder_loading = false;
        }
        assert!(!state.listing_folders(), "the listing landed, so the tick stops");
        // A folderless provider's dialog never starts one.
        state.add = Some(CloudAdd::edit_account(account_of("youtube")));
        assert!(!state.listing_folders());
    }

    /// **Confirm persists the level the browser is showing** (DRAGON-517), and that is the whole
    /// of what a destination is now. The two shapes are the DRAGON-498 rule this inherited from
    /// the deleted pick handler: Drive's app folder is a real folder with a real id, and on the
    /// other two the token root already IS that folder, so there is nothing to write.
    #[test]
    fn confirm_saves_whatever_level_the_browser_is_showing() {
        let root = ("root-1".to_string(), crate::cloud::APP_FOLDER.to_string());
        let level = |names: &[&str]| -> Vec<(String, String)> {
            names.iter().map(|n| (format!("id-{n}"), (*n).to_string())).collect()
        };
        // At the app folder, on a provider that names one: its own id and name, never nothing.
        // Writing nothing on Drive would mean the whole of My Drive.
        assert_eq!(
            viewed_destination(&[], Some(&root)),
            (Some("root-1".to_string()), Some(crate::cloud::APP_FOLDER.to_string()))
        );
        // At the app folder on Dropbox / OneDrive: nothing, because nothing already means it.
        assert_eq!(viewed_destination(&[], None), (None, None));
        // Inside a folder: that folder's id, and the whole PATH below the app folder, because a
        // bare name cannot say which "2026" was meant.
        assert_eq!(
            viewed_destination(&level(&["Screenshots"]), Some(&root)),
            (Some("id-Screenshots".to_string()), Some("Screenshots".to_string()))
        );
        assert_eq!(
            viewed_destination(&level(&["Work", "Clients", "Acme"]), Some(&root)),
            (Some("id-Acme".to_string()), Some("Work/Clients/Acme".to_string()))
        );
        // The root plays no part once the browser is inside something, so the two providers'
        // answers are identical there.
        assert_eq!(
            viewed_destination(&level(&["Work", "2026"]), None),
            viewed_destination(&level(&["Work", "2026"]), Some(&root))
        );
    }

    /// **A refresh cannot move the destination** (DRAGON-517), which is what retired
    /// `surviving_selection` and the whole "which folder stays selected once a listing lands"
    /// question with it.
    ///
    /// The guarantee is structural: [`viewed_destination`] is not given the folder LISTING at all,
    /// so re-reading a level, finding new folders in it, or finding it empty cannot change what a
    /// save writes. Only navigating can, because only navigating changes the trail.
    #[test]
    fn a_refresh_cannot_move_the_destination_because_it_is_the_view() {
        let root = ("root-1".to_string(), crate::cloud::APP_FOLDER.to_string());
        let trail = vec![("id-Work".to_string(), "Work".to_string())];
        let before = viewed_destination(&trail, Some(&root));
        // Whatever a listing turns out to hold, the level it was a listing OF is unchanged, and
        // that level is the answer.
        assert_eq!(viewed_destination(&trail, Some(&root)), before);
        assert_eq!(before, (Some("id-Work".to_string()), Some("Work".to_string())));
        // Navigating is the one thing that does move it, in both directions.
        assert_ne!(viewed_destination(&[], Some(&root)), before, "back out to the app folder");
        let deeper = vec![trail[0].clone(), ("id-2026".to_string(), "2026".to_string())];
        assert_eq!(
            viewed_destination(&deeper, Some(&root)),
            (Some("id-2026".to_string()), Some("Work/2026".to_string()))
        );
    }

    /// **An account that never named a destination adopts the app folder** (DRAGON-498), and this
    /// is all that is left of the old listing-time selection rules.
    ///
    /// It is not about the browser: the browser's level is written by Done. This covers the
    /// account that never gets there, because a Drive account with no `folder_id` uploads to the
    /// whole of My Drive.
    #[test]
    fn an_account_with_no_destination_takes_the_app_folder() {
        assert!(adopts_app_folder(None, None), "a freshly connected account");
        assert!(adopts_app_folder(Some("   "), Some("")), "a hand-edited file says nothing too");
        // Anything actually stored is left alone, id or name.
        assert!(!adopts_app_folder(Some("folder-1"), Some("Screenshots")));
        assert!(!adopts_app_folder(Some("folder-1"), None));
        assert!(!adopts_app_folder(None, Some("Screenshots")), "a path with no id is a choice");
    }

    /// A stored destination is split into levels through the one splitter
    /// [`crate::cloud::save_path`] also reads, so the account's row and the browser walking back
    /// down to it cannot disagree about what a level is.
    #[test]
    fn the_stored_path_is_what_says_how_deep_a_destination_is() {
        let depth = |stored: Option<&str>| {
            crate::cloud::folder_path_segments(stored, crate::cloud::APP_FOLDER).len()
        };
        assert_eq!(depth(None), 0, "the app folder itself");
        assert_eq!(depth(Some(crate::cloud::APP_FOLDER)), 0, "Drive's way of storing the root");
        assert_eq!(depth(Some("Screenshots")), 1);
        assert_eq!(depth(Some("Screenshots/2026")), 2);
        assert_eq!(depth(Some("Work/Clients/Acme")), 3);
    }

    fn account_of(provider: &str) -> CloudAccount {
        CloudAccount {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            provider: provider.to_string(),
            label: "Mine".to_string(),
            folder_id: None,
            folder_name: None,
            added_at: "2026-08-02T10:00:00+00:00".to_string(),
            visibility: None,
        }
    }

    /// **Every folder provider gets the SAME limit line** (DRAGON-503). Google used to get a
    /// caption of its own about the `drive.file` scope, which read as Google being uniquely
    /// restricted once DRAGON-498 confined all three to one app folder. Nothing on this page is
    /// keyed on a provider id any more, and this is the test that says so.
    #[test]
    fn no_provider_is_singled_out_for_a_caution() {
        let note = |id: &str| setup_sandbox_note(spec(id), chooses_folder(spec(id)));
        for id in ["gdrive", "onedrive", "dropbox"] {
            let line = note(id).unwrap_or_else(|| panic!("{id} states its limit"));
            assert!(line.contains(spec(id).display_name), "{line}");
            assert!(line.contains("cannot see or touch"), "{line}");
        }
        assert_eq!(note("youtube"), None);
        // The shape is one sentence per provider, differing ONLY in the provider's name.
        assert_eq!(
            note("gdrive").unwrap().replace("Google Drive", "X"),
            note("dropbox").unwrap().replace("Dropbox", "X")
        );
    }

    /// A failure from the flow itself is always worth retrying, and a dead authorization is
    /// recognised as such wherever it comes from.
    #[test]
    fn a_failed_connect_offers_another_attempt() {
        let step = failure_step("The sign-in was declined.");
        assert_eq!(
            step,
            CloudAddStep::Failed {
                message: "The sign-in was declined.".to_string(),
                retry: true,
            }
        );
        let dead = crate::cloud::oauth::reconnect_message("the sign-in was withdrawn.");
        assert_eq!(
            classify_failure(&dead),
            ReconnectHint::NeedsReconnect(dead.clone()),
            "the reconnect prefix must be recognised"
        );
        assert_eq!(
            classify_failure("The network is unreachable."),
            ReconnectHint::Retry("The network is unreachable.".to_string())
        );
        // Either way the message shown is the provider-independent sentence, unchanged.
        assert_eq!(classify_failure(&dead).message(), dead);
    }
}

#[cfg(test)]
mod account_row_tests {
    use super::*;

    fn account() -> CloudAccount {
        CloudAccount {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            provider: "gdrive".to_string(),
            label: "Work Drive".to_string(),
            folder_id: Some("folder-1".to_string()),
            folder_name: Some("Screenshots".to_string()),
            added_at: "2026-08-02T10:00:00+00:00".to_string(),
            visibility: None,
        }
    }

    /// The label falls back to the drive's name plus the date, which is what tells two
    /// accounts on the same provider apart in a picker.
    #[test]
    fn a_label_is_derived_from_whatever_is_known() {
        assert_eq!(derive_label(Some("ada@example.test"), "Google Drive", "2026-08-02"), "ada@example.test");
        assert_eq!(
            derive_label(None, "Google Drive", "2026-08-02"),
            "Google Drive (2026-08-02)"
        );
        // A blank identity is not an identity.
        assert_eq!(derive_label(Some("  "), "Dropbox", "2026-08-02"), "Dropbox (2026-08-02)");
    }

    /// A row never shows an empty destination: no folder chosen means the account's own app
    /// folder, and the row names it (DRAGON-498) rather than calling it "Main folder", which
    /// used to mean the whole drive and no longer describes anywhere this app can write. Since
    /// DRAGON-501 it names it as the real PATH, which is [`crate::cloud::save_path`]'s job; this
    /// pins the ACCOUNT SHAPES the row feeds it, the ones that come off disk.
    #[test]
    fn the_destination_is_never_blank() {
        let row = |a: &CloudAccount| crate::cloud::save_path(&a.provider, a.folder_name.as_deref());
        // The sample account is a Google Drive one with a chosen subfolder.
        assert_eq!(row(&account()).as_deref(), Some("/Cosmic Capture Kit/Screenshots"));
        let root = CloudAccount { folder_id: None, folder_name: None, ..account() };
        assert_eq!(row(&root).as_deref(), Some("/Cosmic Capture Kit"));
        let blank = CloudAccount { folder_name: Some("   ".to_string()), ..account() };
        assert_eq!(row(&blank), row(&root));
        // The folder list's root entry is still the app folder's plain NAME, because it sits
        // among sibling folder names in a picker rather than describing a destination.
        assert_eq!(ROOT_DESTINATION, crate::cloud::APP_FOLDER);
        // A Drive account that PICKED that root stores the folder's own id and name; it must
        // read the same as one that picked nothing, not name the folder twice.
        let drive_root = CloudAccount {
            folder_id: Some("1CckFolder".to_string()),
            folder_name: Some(crate::cloud::APP_FOLDER.to_string()),
            ..account()
        };
        assert_eq!(row(&drive_root), row(&root));
        // And the two auto-provisioned providers carry their `Apps` level into the row.
        let onedrive = CloudAccount { provider: "onedrive".to_string(), ..account() };
        assert_eq!(row(&onedrive).as_deref(), Some("/Apps/Cosmic Capture Kit/Screenshots"));
    }

    /// A provider with no folder concept names no destination, and leaves no dangling
    /// separator where one would have been. The live report was a YouTube row reading
    /// "YouTube - Main folder", a folder that cannot exist: YouTube uploads to a channel.
    #[test]
    fn a_folderless_provider_names_no_destination() {
        // The visible subtitle and the searched text both lose their separator with it.
        assert_eq!(account_subtitle("YouTube", None, " - "), "YouTube");
        assert_eq!(account_subtitle("YouTube", None, " "), "YouTube");
        // A provider that HAS folders still names one, in both forms.
        assert_eq!(
            account_subtitle("Google Drive", Some("Screenshots"), " - "),
            "Google Drive - Screenshots"
        );
        // Built from the constant, not from the words it happens to hold: DRAGON-498 changed
        // that default from "Main folder" to the app folder's own name, and this test is about
        // the SEPARATOR rule, which must not fail the next time the destination is renamed.
        assert_eq!(
            account_subtitle("Google Drive", Some(ROOT_DESTINATION), " "),
            format!("Google Drive {ROOT_DESTINATION}")
        );
        // And the CAPABILITY, not a provider id, is what the row keys on: these are the two
        // sides the row has to tell apart, read straight from the registry.
        let youtube = crate::cloud::provider("youtube").expect("youtube is a listed provider");
        assert!(!youtube.caps.folder_browse);
        assert!(!chooses_folder(youtube));
        let gdrive = crate::cloud::provider("gdrive").expect("gdrive is a listed provider");
        assert!(chooses_folder(gdrive));
    }

    /// The page's cached list decides which rows offer Reconnect, and an id that has been
    /// disconnected must not keep warranting one.
    #[test]
    fn a_removed_account_stops_warranting_a_reconnect() {
        let mut page = CloudPageState { accounts: vec![account()], ..Default::default() };
        page.reconnect = vec![account().id, "a-stale-id".to_string()];
        assert!(page.warrants_reconnect(&account().id));
        assert!(page.warrants_reconnect("a-stale-id"));
        // `reload` re-reads the file (empty in a test sandbox), so both drop out.
        page.reload();
        assert!(!page.warrants_reconnect("a-stale-id"));
    }
}

#[cfg(test)]
mod account_name_tests {
    use super::*;

    fn account(provider: &str, label: &str, added_at: &str) -> CloudAccount {
        CloudAccount {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            provider: provider.to_string(),
            label: label.to_string(),
            folder_id: None,
            folder_name: None,
            added_at: added_at.to_string(),
            visibility: None,
        }
    }

    /// **A blank name is never SAVED blank** (DRAGON-495). The field may be emptied while
    /// typing, but what reaches the file is the provider default, because two accounts on one
    /// provider with no names are two identical rows.
    #[rstest::rstest]
    #[case("Work Drive", "Google Drive (2026-08-02)", "Work Drive")]
    #[case("  Work Drive  ", "Google Drive (2026-08-02)", "Work Drive")]
    #[case("", "Google Drive (2026-08-02)", "Google Drive (2026-08-02)")]
    #[case("   ", "Google Drive (2026-08-02)", "Google Drive (2026-08-02)")]
    fn a_blank_name_commits_the_provider_default(
        #[case] typed: &str,
        #[case] default: &str,
        #[case] expected: &str,
    ) {
        assert_eq!(commit_label(typed, default), expected);
    }

    /// The default a blank field falls back to is the name the connect flow would have given
    /// the account, rebuilt from the account's own record: its provider's display name and the
    /// date it was added. So clearing the field restores the name it started with rather than
    /// stamping today's date on an account connected months ago.
    #[test]
    fn the_default_name_is_the_one_the_account_was_born_with() {
        let acct = account("gdrive", "", "2026-08-02T10:00:00+00:00");
        assert_eq!(default_label(&acct, "2026-12-25"), "Google Drive (2026-08-02)");
        // It matches what `connect_and_save` itself would have derived on that day.
        assert_eq!(default_label(&acct, "2026-12-25"), derive_label(None, "Google Drive", "2026-08-02"));
        // An account with no usable stamp falls back to the injected date.
        let undated = account("dropbox", "", "");
        assert_eq!(default_label(&undated, "2026-12-25"), "Dropbox (2026-12-25)");
        let malformed = account("dropbox", "", "not-a-date-at-all");
        assert_eq!(default_label(&malformed, "2026-12-25"), "Dropbox (2026-12-25)");
        // A provider this build does not know still sorts and names by SOMETHING.
        let unknown = account("a-drive-from-the-future", "", "2026-08-02T10:00:00+00:00");
        assert_eq!(default_label(&unknown, "2026-12-25"), "a-drive-from-the-future (2026-08-02)");
    }

    /// Only a real `YYYY-MM-DD` prefix counts as a date. Anything else has nothing to offer a
    /// name, and a sliced-up garbage string in the middle of an account's label would be worse
    /// than today's date.
    #[rstest::rstest]
    #[case("2026-08-02T10:00:00+00:00", Some("2026-08-02"))]
    #[case("2026-08-02", Some("2026-08-02"))]
    #[case("", None)]
    #[case("2026/08/02T10:00", None)]
    #[case("not-a-date", None)]
    #[case("20260802T100000Z", None)]
    fn only_a_real_date_prefix_is_read_as_one(#[case] stamp: &str, #[case] expected: Option<&str>) {
        assert_eq!(added_on(stamp), expected);
    }

    /// A rename writes the label and NOTHING else: the record that goes to disk is the one the
    /// dialog was opened with, its label committed, every other field untouched. Tokens live in
    /// the keyring and are not part of this record at all, which is the structural half of the
    /// same guarantee.
    #[test]
    fn a_rename_touches_nothing_but_the_label() {
        let before = CloudAccount {
            folder_id: Some("folder-1".to_string()),
            folder_name: Some("Screenshots".to_string()),
            visibility: Some(crate::cloud::accounts::Visibility::Unlisted),
            ..account("gdrive", "Renamed", "2026-08-02T10:00:00+00:00")
        };
        let after = saved_account(&before);
        assert_eq!(after.label, "Renamed");
        assert_eq!(CloudAccount { label: before.label.clone(), ..after }, before);
    }
}

#[cfg(test)]
mod stored_signin_tests {
    use super::*;

    fn at(offset_secs: i64) -> String {
        (chrono::Utc::now() + chrono::Duration::seconds(offset_secs)).to_rfc3339()
    }

    /// Missing tokens always warrant a reconnect: there is nothing left to sign in with.
    #[test]
    fn missing_tokens_warrant_a_reconnect() {
        assert!(reconnect_warranted(None, chrono::Utc::now()));
    }

    /// A refresh token means the account can renew itself, whatever the access token's
    /// expiry says, so the row must NOT nag.
    #[test]
    fn a_renewable_account_does_not_warrant_one() {
        let tokens = TokenSet {
            access_token: "a".to_string(),
            refresh_token: Some("r".to_string()),
            expires_at: Some(at(-3600)),
            scopes: Vec::new(),
            token_type: "Bearer".to_string(),
        };
        assert!(!reconnect_warranted(Some(&tokens), chrono::Utc::now()));
    }

    /// No refresh token AND an expiring access token is the one state a user has to act on.
    /// A long-lived access token with no refresh is still usable, so it does not warrant one
    /// yet.
    #[test]
    fn an_unrenewable_expiring_account_warrants_one() {
        let expiring = TokenSet {
            access_token: "a".to_string(),
            refresh_token: None,
            expires_at: Some(at(30)),
            scopes: Vec::new(),
            token_type: "Bearer".to_string(),
        };
        assert!(reconnect_warranted(Some(&expiring), chrono::Utc::now()));
        let live = TokenSet { expires_at: Some(at(7200)), ..expiring };
        assert!(!reconnect_warranted(Some(&live), chrono::Utc::now()));
    }
}

// A `field_tests` module here (DRAGON-482) covered two fields this page no longer has. The
// typed-folder normaliser (`typed_folder`, "/Screenshots/" and "Screenshots" both meaning one
// stored path) went with its field in DRAGON-498: with every provider confined to one app
// folder there is no path for a user to type. The auto-delete field went to the preview
// editor in DRAGON-493 and then away entirely in DRAGON-505, with the feature behind it. The
// module is gone rather than left empty.

#[cfg(test)]
mod url_relay_tests {
    use super::*;

    /// The relay that carries the sign-in URL to the UI has a deadline, so it cannot outlive
    /// the flow it belongs to even if that flow never reports a URL at all.
    ///
    /// The teardown half of DRAGON-497 (that this page's workers are detached at all) is
    /// pinned in `app::background`, where the seam now lives.
    #[test]
    fn the_browser_url_relay_is_bounded_by_the_connect_it_belongs_to() {
        assert_eq!(URL_RELAY_BUDGET, crate::cloud::oauth::BROWSER_DEADLINE);
        assert!(!URL_RELAY_BUDGET.is_zero(), "a zero budget would race every real connect");
    }
}

#[cfg(test)]
mod breadcrumb_tests {
    use super::*;

    /// **The LAST crumb is always the level being looked at** (DRAGON-514). `breadcrumb_row`
    /// relies on exactly this to know which crumb becomes the level's own mark rather than a
    /// navigation link, and it is the reason that mark needs no second depth calculation of its
    /// own. Load-bearing twice over since DRAGON-517: the level the crumbs end at is also the
    /// account's destination, so a row whose last crumb were not the current level would be
    /// ticking somewhere the captures do not go.
    #[test]
    fn the_last_crumb_is_always_the_current_level() {
        for depth in 0..40 {
            let crumbs = breadcrumb(depth);
            let last = *crumbs.last().expect("a breadcrumb is never empty");
            // The level's crumb is never a navigation target: `crumb_target` answers `None` for
            // it, which is exactly what made it plain text before it became the selector.
            assert_eq!(crumb_target(last, depth), None, "depth {depth} made its own level a link");
            match depth {
                0 => assert_eq!(last, Crumb::Root, "at the app folder the level IS the root"),
                _ => assert_eq!(last, Crumb::Level(depth - 1), "depth {depth}"),
            }
            // And it is never the elision, which names no single level and stays plain text.
            assert_ne!(last, Crumb::Elided, "depth {depth}");
        }
    }

    /// **The mark follows the VIEW** (DRAGON-517), which is what replaced `level_is_chosen`, the
    /// per-provider rule that used to decide whether the path row's level was the account's pick.
    ///
    /// There is no pick left to disagree with: the level the browser is showing IS where uploads
    /// land, so the mark on the last crumb (a plain label now; the tick it used to carry is gone
    /// on the owner's report) is a statement rather than a state, and the thing that still differs
    /// per provider is what that level SAVES ([`viewed_destination`], below).
    ///
    /// What this pins is that the two really are the same level. `level_control` labels whichever
    /// crumb `breadcrumb` ends on, and `viewed_destination` reads whichever level `trail` ends on;
    /// if those two could ever be different levels the label would be naming the wrong folder.
    #[test]
    fn the_marked_level_is_where_uploads_land() {
        let root = ("root-id".to_string(), crate::cloud::APP_FOLDER.to_string());
        for depth in 0..8 {
            let trail: Vec<(String, String)> =
                (0..depth).map(|i| (format!("f{i}"), format!("Level {i}"))).collect();
            let marked = *breadcrumb(trail.len()).last().expect("a breadcrumb is never empty");
            let (saved, _) = viewed_destination(&trail, Some(&root));
            match trail.last() {
                // Inside a folder: the crumb marked is that folder's, and that folder's id is
                // what a save writes.
                Some((id, _)) => {
                    assert_eq!(marked, Crumb::Level(depth - 1), "depth {depth}");
                    assert_eq!(saved.as_deref(), Some(id.as_str()), "depth {depth}");
                }
                // At the app folder: the root crumb is marked, and the root is what is saved.
                None => {
                    assert_eq!(marked, Crumb::Root);
                    assert_eq!(saved.as_deref(), Some("root-id"));
                }
            }
        }
    }

    /// **What a folder row draws in its trailing block, and what it reserves** (DRAGON-514,
    /// emptied of its tick by DRAGON-517).
    ///
    /// A row can no longer mark itself: a folder in the LIST is by definition not the level the
    /// browser is in, so the leading slot is never filled on any row. It is still RESERVED, and
    /// that is the half of the original report this test still guards: the block's width is
    /// [`FOLDER_ACTIONS_W`], fixed, with an unfilled slot drawn as a `Space` of the same
    /// footprint, so the bin sits in one column and every row ends at one inset.
    #[test]
    fn a_rows_trailing_actions_are_drawn_by_state_and_sized_by_neither() {
        assert_eq!(folder_row_slots(false), [false, false], "a folder that cannot be deleted");
        assert_eq!(folder_row_slots(true), [false, true], "an ordinary child");
        // The tick slot is dead on EVERY row, which is the DRAGON-517 half: there is no argument
        // that can light it, because there is no per-row selection left to report.
        for deletable in [false, true] {
            let [tick, _] = folder_row_slots(deletable);
            assert!(!tick, "a listed folder must never mark itself");
        }
        // Both slots occupy the SAME square, the other half of the original report: a tick and a
        // bin at two different sizes, on two different backgrounds, in one column.
        let slot = FOLDER_ACTION_ICON + 2.0 * FOLDER_ACTION_PAD;
        assert!(slot > FOLDER_ACTION_ICON, "a slot is the glyph plus its padding");
        assert!((FOLDER_ACTIONS_W - (2.0 * slot + FOLDER_ROW_PAD[1])).abs() < f32::EPSILON);
    }

    /// The row always says two things: what this app is confined to, and where the user is now.
    /// A trail short enough to fit draws every level between them.
    #[rstest::rstest]
    #[case(0, vec![Crumb::Root])]
    #[case(1, vec![Crumb::Root, Crumb::Level(0)])]
    #[case(2, vec![Crumb::Root, Crumb::Level(0), Crumb::Level(1)])]
    #[case(3, vec![Crumb::Root, Crumb::Level(0), Crumb::Level(1), Crumb::Level(2)])]
    fn a_shallow_trail_draws_every_level(#[case] depth: usize, #[case] expected: Vec<Crumb>) {
        assert_eq!(breadcrumb(depth), expected);
    }

    /// **A deep trail loses its MIDDLE, never its ends.** A dialog is a fixed width and a folder
    /// name is not, so something has to give; what gives is the levels a user is least likely to
    /// be looking for.
    #[test]
    fn a_deep_trail_elides_the_middle_and_keeps_both_ends() {
        assert_eq!(
            breadcrumb(4),
            vec![Crumb::Root, Crumb::Elided, Crumb::Level(2), Crumb::Level(3)]
        );
        assert_eq!(
            breadcrumb(9),
            vec![Crumb::Root, Crumb::Elided, Crumb::Level(7), Crumb::Level(8)]
        );
        // However deep it goes, the row stays the same width, starts at the app folder and ends
        // at the level being looked at.
        for depth in 0..40 {
            let crumbs = breadcrumb(depth);
            assert!(crumbs.len() <= MAX_CRUMBS, "depth {depth} drew {} crumbs", crumbs.len());
            assert_eq!(crumbs.first(), Some(&Crumb::Root), "depth {depth}");
            if depth > 0 {
                assert_eq!(crumbs.last(), Some(&Crumb::Level(depth - 1)), "depth {depth}");
            }
            // Exactly one elision, and only when there was something to elide.
            let elided = crumbs.iter().filter(|c| **c == Crumb::Elided).count();
            assert_eq!(elided, usize::from(depth >= MAX_CRUMBS), "depth {depth}");
        }
    }

    /// A long folder name is cut so it cannot push the row past the card's edge, and cut by
    /// CHARACTERS: a byte slice through an accented name panics, and these are the user's own
    /// folder names.
    #[test]
    fn a_long_crumb_is_shortened_and_never_split_mid_character() {
        assert_eq!(crumb_label("Screenshots"), "Screenshots");
        let exact = "x".repeat(MAX_CRUMB_CHARS);
        assert_eq!(crumb_label(&exact), exact, "a name that fits is left alone");
        let long = crumb_label(&"x".repeat(MAX_CRUMB_CHARS + 1));
        assert_eq!(long.chars().count(), MAX_CRUMB_CHARS, "the ellipsis counts toward the budget");
        assert!(long.ends_with('\u{2026}'), "{long}");
        // Multi-byte throughout, including one outside the basic plane.
        for name in ["é".repeat(40), "\u{1f4c1}".repeat(40), "Réunions clients Acme 2026".to_string()] {
            let cut = crumb_label(&name);
            assert!(cut.chars().count() <= MAX_CRUMB_CHARS, "{cut}");
        }
        assert_eq!(crumb_label(""), "");
    }

    /// A crumb press truncates the trail to that many levels, and the two crumbs that are not
    /// navigation send nothing: the elision names no single level, and the level already on
    /// screen is not somewhere to go.
    #[rstest::rstest]
    #[case(Crumb::Root, 0, None)]
    #[case(Crumb::Root, 3, Some(0))]
    #[case(Crumb::Level(0), 3, Some(1))]
    #[case(Crumb::Level(1), 3, Some(2))]
    #[case(Crumb::Level(2), 3, None)]
    #[case(Crumb::Elided, 9, None)]
    fn a_crumb_press_goes_back_to_that_level_and_no_further(
        #[case] crumb: Crumb,
        #[case] depth: usize,
        #[case] expected: Option<usize>,
    ) {
        assert_eq!(crumb_target(crumb, depth), expected);
    }

    /// Every crumb the row actually draws is either a level a press can reach or one of the two
    /// inert ones, and a reachable target is always SHALLOWER than where the user is. A press
    /// that could deepen the trail would be a breadcrumb that is not a way back.
    #[test]
    fn no_drawn_crumb_can_take_the_user_deeper() {
        for depth in 0..12 {
            for crumb in breadcrumb(depth) {
                let Some(target) = crumb_target(crumb, depth) else { continue };
                assert!(target < depth, "depth {depth}, crumb {crumb:?} -> {target}");
            }
        }
    }
}

#[cfg(test)]
mod folder_navigation_tests {
    use super::*;

    /// The destination stored for a folder is its PATH below the app folder, every level of it.
    /// A name alone cannot say which "2026" was picked once the list drills down, and it is what
    /// `cloud::save_path` renders on the account's row.
    #[test]
    fn a_chosen_folder_is_stored_as_its_whole_path() {
        assert_eq!(destination_path(&[], Some("Screenshots")).as_deref(), Some("Screenshots"));
        assert_eq!(
            destination_path(&["Screenshots"], Some("2026")).as_deref(),
            Some("Screenshots/2026")
        );
        assert_eq!(
            destination_path(&["Work", "Clients"], Some("Acme")).as_deref(),
            Some("Work/Clients/Acme")
        );
        // The level itself, which is what the list's first row picks.
        assert_eq!(destination_path(&["Work", "Clients"], None).as_deref(), Some("Work/Clients"));
        // The app folder, where the caller applies the root rule instead: Drive stores the
        // root's own id and name, the other two store nothing.
        assert_eq!(destination_path(&[], None), None);
    }

    /// The stored path and the row's rendering are the same fact: what is written here is what
    /// `cloud::save_path` reads back, so a nested destination round-trips into a path a user can
    /// actually follow in their drive.
    #[test]
    fn a_stored_path_round_trips_into_the_row_the_user_reads() {
        let stored = destination_path(&["Screenshots"], Some("2026")).expect("a path");
        assert_eq!(
            crate::cloud::save_path("gdrive", Some(&stored)).as_deref(),
            Some("/Cosmic Capture Kit/Screenshots/2026")
        );
        assert_eq!(
            crate::cloud::save_path("onedrive", Some(&stored)).as_deref(),
            Some("/Apps/Cosmic Capture Kit/Screenshots/2026")
        );
    }

    /// **A level deleted somewhere else is not a broken dialog** (DRAGON-506): the step steps
    /// back out to the parent, which certainly still exists, rather than failing with the user
    /// looking at a folder that is gone and no way back but closing.
    #[test]
    fn a_level_that_is_gone_walks_the_breadcrumb_up_one() {
        let missing = crate::cloud::providers::missing_folder_err();
        assert!(crate::cloud::providers::is_missing_folder(&missing), "{missing}");
        assert_eq!(listing_failure(1, &missing), ListingFailure::AscendOne);
        assert_eq!(listing_failure(4, &missing), ListingFailure::AscendOne);
        // At the top of the tree there is nowhere to ascend to, and no provider reports it
        // there anyway: the app folder is created on demand, so its listing is empty, never
        // missing.
        assert_eq!(listing_failure(0, &missing), ListingFailure::Note);
        // Every ordinary failure stays a note at every depth, including one that merely reads
        // like a missing folder. The MARKER decides, never the wording, because user copy
        // changes and a rule keyed on it would quietly stop working.
        for reason in [
            "The cloud service could not list folders.",
            "The cloud service could not find what it needed to list folders.",
            "that folder is no longer there",
            "",
        ] {
            for depth in 0..4 {
                assert_eq!(listing_failure(depth, reason), ListingFailure::Note, "{reason:?}");
            }
        }
    }
}

#[cfg(test)]
mod folder_create_tests {
    use super::*;

    /// A typed name is held to the provider rules first, and then to the one this level knows:
    /// a name already on screen. Two folders of one name is a picker nobody can use, and Drive
    /// would happily make one, so refusing here is what keeps the three providers behaving the
    /// same way.
    #[test]
    fn a_new_folder_name_is_refused_before_anything_is_asked() {
        let siblings = ["Screenshots", "Recordings"];
        assert_eq!(create_refusal("Notes", &siblings), None);
        assert_eq!(create_refusal("  Notes  ", &siblings), None, "trimmed, then judged");
        // The provider rules, borrowed whole rather than restated.
        assert!(create_refusal("", &siblings).is_some());
        assert!(create_refusal("   ", &siblings).is_some());
        assert!(create_refusal("a/b", &siblings).is_some());
        assert!(create_refusal("..", &siblings).is_some());
        // The duplicate rule, case-insensitively: two of the three providers treat names that
        // way themselves, and two rows a user cannot tell apart is the same problem on all three.
        for taken in ["Screenshots", "screenshots", "  SCREENSHOTS "] {
            let refusal = create_refusal(taken, &siblings).expect("taken");
            assert!(refusal.contains("already a folder"), "{refusal}");
        }
        // An empty level takes any legal name.
        assert_eq!(create_refusal("Screenshots", &[]), None);
    }

    /// Every refusal is a sentence a dialog can show, and none of them quotes the name back:
    /// this string reaches the step's notice line, and that line can reach a log.
    #[test]
    fn a_create_refusal_is_a_sentence_that_quotes_nothing() {
        for typed in ["", "secret/plan", "Screenshots"] {
            let Some(refusal) = create_refusal(typed, &["Screenshots"]) else { continue };
            assert!(refusal.ends_with('.'), "{refusal}");
            assert!(!refusal.contains("secret"), "{refusal}");
        }
    }

    /// **A create that fails has exactly one destination: the step's notice line** (DRAGON-520).
    ///
    /// Every reason a create can end badly goes through [`classify_failure`] on the way there, so
    /// this pins that none of them is dropped or arrives empty, and that a reason carrying the
    /// reconnect marker still reaches the user as the reconnect it is. The DRAGON-520 report read
    /// as a swallowed error and was not one: Drive's create SUCCEEDED at the wrong parent (see
    /// `cloud::providers::create_parent_with`), so there was no `Err` for this path to show.
    #[test]
    fn every_way_a_create_can_fail_reaches_the_notice_line() {
        let reasons = [
            // The provider refused it.
            "The cloud service could not create that folder.".to_string(),
            // A provider that would not say where its app folder is (DRAGON-520).
            "OneDrive could not say where this app's own folder is, so the new folder had \
             nowhere to go."
                .to_string(),
            // The name was taken.
            "That folder name is already taken here.".to_string(),
            // The detached job died without answering, which `create_cloud_folder` turns into
            // this rather than into silence.
            "The folder could not be created.".to_string(),
        ];
        for reason in &reasons {
            let shown = classify_failure(reason).message().to_string();
            assert!(!shown.is_empty(), "a create failure reached the step with nothing to say");
            assert_eq!(&shown, reason, "the reason was rewritten on its way to the user");
        }
        // A refusal that needs a reconnect keeps that classification, so the step offers the
        // reconnect rather than a plain retry.
        let stale =
            crate::cloud::oauth::reconnect_message("the cloud service refused this account.");
        assert!(matches!(classify_failure(&stale), ReconnectHint::NeedsReconnect(_)));
    }
}

#[cfg(test)]
mod empty_level_tests {
    use super::*;

    /// The empty-state line appears for the settled empty level and for nothing else
    /// (DRAGON-520). Each of the other three states has a row or a spinner on screen already, and
    /// a line saying "nothing here" beside one of those is simply wrong.
    #[rstest::rstest]
    // folders, loading, creating, deleting
    #[case(0, false, false, false, true)]
    // Rows on screen: never.
    #[case(1, false, false, false, false)]
    #[case(9, false, false, false, false)]
    // A listing in flight has not answered yet.
    #[case(0, true, false, false, false)]
    // The inline create row IS the list's content while it is open, in flight or not.
    #[case(0, false, true, false, false)]
    // A delete confirmation is up, or one is running.
    #[case(0, false, false, true, false)]
    // Two at once still keeps it quiet.
    #[case(0, true, true, true, false)]
    fn the_empty_line_is_shown_only_for_a_settled_empty_level(
        #[case] folders: usize,
        #[case] loading: bool,
        #[case] creating: bool,
        #[case] deleting: bool,
        #[case] shown: bool,
    ) {
        assert_eq!(shows_empty_level_note(folders, loading, creating, deleting), shown);
    }

    /// The copy itself: two lines, the first naming what's true and the second what to do about
    /// it. Neither carries a folder name, because this line can reach a log and names are the
    /// user's content.
    #[test]
    fn the_empty_line_says_what_to_do_and_names_nothing() {
        for line in EMPTY_LEVEL_NOTE {
            assert!(line.ends_with('.'), "{line}");
            assert!(!line.contains('—'), "house style: no em-dashes in user copy: {line}");
        }
        assert!(
            EMPTY_LEVEL_NOTE.iter().any(|line| line.contains('+')),
            "it must point at the create control: {EMPTY_LEVEL_NOTE:?}"
        );
    }
}

#[cfg(test)]
mod folder_delete_tests {
    use super::*;

    fn spec(id: &str) -> &'static ProviderSpec {
        crate::cloud::provider(id).expect("a registry provider")
    }

    /// **Both refusals are about the account's own destination**, and both are decided before
    /// any request: a provider would carry either one out without complaint, and the user would
    /// find out at their next capture.
    #[rstest::rstest]
    // Nothing chosen at all, or a destination somewhere else entirely: deletable.
    #[case("Old", None, false)]
    #[case("Old", Some("Screenshots"), false)]
    #[case("Old", Some("Screenshots/2026"), false)]
    // The folder IS the destination.
    #[case("Screenshots", Some("Screenshots"), true)]
    #[case("Work/2026", Some("Work/2026"), true)]
    // The folder CONTAINS the destination, at any depth.
    #[case("Work", Some("Work/2026"), true)]
    #[case("Work", Some("Work/Clients/Acme"), true)]
    #[case("Work/Clients", Some("Work/Clients/Acme"), true)]
    // A name that merely STARTS with the candidate is a different folder, and the separator is
    // what tells them apart. Without that check, deleting "Work" would be refused because of a
    // destination in "Workshop".
    #[case("Work", Some("Workshop"), false)]
    #[case("Work", Some("Workshop/2026"), false)]
    // Case is not what distinguishes two folders: Dropbox stores a lowercased path and OneDrive
    // treats names case-insensitively.
    #[case("Work", Some("work/2026"), true)]
    #[case("WORK", Some("work"), true)]
    fn a_folder_holding_the_destination_cannot_be_deleted(
        #[case] candidate: &str,
        #[case] chosen: Option<&str>,
        #[case] refused: bool,
    ) {
        assert_eq!(delete_refusal(candidate, chosen).is_some(), refused);
    }

    /// The app folder is never deletable, and it reaches this rule as the empty path: it is the
    /// one level with nothing above it, so a destination check cannot be what saves it.
    #[test]
    fn the_app_folder_is_refused_whatever_the_account_points_at() {
        for chosen in [None, Some("Screenshots"), Some("")] {
            let refusal = delete_refusal("", chosen).expect("the app folder is not deletable");
            assert!(refusal.contains("this app uploads to"), "{refusal}");
        }
        // A blank destination is not a destination, so it protects nothing.
        assert_eq!(delete_refusal("Screenshots", Some("   ")), None);
    }

    /// **The confirmation's copy is keyed on the capability, not on a provider id**, because the
    /// two sentences are the honest answer for two different APIs. Telling a Drive user their
    /// folder can be restored would be the worst copy on this page.
    #[test]
    fn the_confirmation_says_what_this_provider_actually_does() {
        assert_eq!(DELETE_TITLE, "Delete this folder?");
        let permanent = delete_confirm_body("Screenshots", "Google Drive", false);
        assert_eq!(
            permanent,
            "Screenshots and everything inside it will be permanently deleted from your Google \
             Drive. This cannot be undone."
        );
        let recycled = delete_confirm_body("Screenshots", "OneDrive", true);
        assert_eq!(
            recycled,
            "Screenshots and everything inside it will be moved to your OneDrive recycle bin. \
             You can put it back from there if you need it."
        );
        // Both name the folder and both say the contents go too, which is the fact a user has
        // to have before they press Delete.
        for body in [&permanent, &recycled] {
            assert!(body.starts_with("Screenshots"), "{body}");
            assert!(body.contains("everything inside it"), "{body}");
            assert!(!body.contains('\u{2014}'), "no em-dashes: {body}");
        }
        // And they do not blur into each other: only one of them offers a way back.
        assert!(permanent.contains("cannot be undone"));
        assert!(!permanent.contains("recycle"));
        assert!(recycled.contains("recycle bin"));
        assert!(!recycled.contains("permanently"));
    }

    /// The copy each provider gets is the one its own API earns, read from the registry rather
    /// than chosen here.
    #[test]
    fn each_provider_gets_the_copy_its_api_earns() {
        let body = |id: &str| {
            let s = spec(id);
            delete_confirm_body("Screenshots", s.display_name, s.caps.delete_recoverable)
        };
        assert!(body("gdrive").contains("permanently deleted from your Google Drive"));
        assert!(body("dropbox").contains("permanently deleted from your Dropbox"));
        assert!(body("onedrive").contains("moved to your OneDrive recycle bin"));
    }
}
