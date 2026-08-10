//! Cloud accounts (DRAGON-482): the FOUNDATION layer.
//!
//! What this module is for: a capture can be handed to a cloud drive the user has
//! connected, and the link to it copied back. That whole feature lands in stages, and
//! this file is stage A1: the types, the storage, the transport and the wiring. There is
//! no OAuth here, no provider API call, no upload engine and no UI. The stages that add
//! those own their own files, which is the point of the empty modules declared at the
//! bottom: after A1 the SHARED tree (main.rs, state, shortcuts, settings nav, icons,
//! diag) does not have to change again, so later stages touch only `cloud/`.
//!
//! # The shape
//!
//! * [`ProviderSpec`] is a static description of one cloud drive: what it is called, what
//!   it can do ([`ProviderCaps`]), and how a user connects to it ([`AuthKind`]). The set
//!   of them is [`registry`], a compile-time table. Nothing about a provider is
//!   discovered at runtime.
//! * [`accounts`] stores the NON-secret half of a connected account (which provider,
//!   what the user called it, which folder, which visibility) in its own file.
//! * [`secrets`] stores the SECRET half (tokens) in the platform keyring, with a file
//!   fallback. The two are deliberately separate stores: the account list is ordinary
//!   config a user may read or copy between machines, and the tokens are not.
//! * [`http`] is the transport, curl with the secrets kept out of argv.
//!
//! # Why a capability table rather than per-provider branches
//!
//! The five drives disagree about almost everything: two of them have no official API at
//! all, one can expire a share link and the others cannot, one takes a visibility choice
//! and the others do not. Writing that as `if provider == "dropbox"` in the UI would spread
//! the disagreement across every screen. [`ProviderCaps`] states it ONCE, and every later
//! stage reads the caps rather than the id. A provider that gains an ability changes one
//! row in [`registry`].
//!
//! # Errors
//!
//! `Result<T, String>` throughout, per the house rule. The message names what failed and
//! why, because these strings reach the user through the editor's toast and the settings
//! page.
//!
//! # Privacy
//!
//! Nothing in this module may log a token, a share URL, a remote file id, or a `Path`.
//! `diag::path_shape` is how a path informs a log line, and [`crate::diag::redact_oauth`]
//! is how anything that might carry a token does. That rule is why [`http`] feeds curl
//! its secrets over a stdin config instead of argv: argv is world-readable on Linux.

// The staging `#![allow(dead_code)]` that covered stages A1 to B1 is GONE, as its own comment
// said it must be by the end of B2: the feature is wired, so from here on the attribute would
// hide real rot rather than work in progress. The five items it was covering that no wired
// path calls (`Method::Patch`, `CurlReq::output_to`, `CurlReq::host`, `ledger::remove`,
// `TokenSet::auth_header`) were deleted with it rather than kept behind per-item attributes.
// Anything genuinely dead on ONE platform only gets a targeted `cfg_attr`, per CLAUDE.md.

pub mod accounts;
pub mod http;
pub mod secrets;

// ── Tombstone: the auto-delete feature and its `ledger` module (DRAGON-505) ──────────────
//
// `pub mod ledger;` stood here from DRAGON-482 until DRAGON-505 removed the whole
// auto-delete feature: a per-account "delete uploads after N hours" window, a
// pending-deletions file (flock'd, temp-and-rename, `.corrupt` rescue) recording each
// upload's deadline, and a six-hourly sweep thread in all three resident daemons that
// deleted whatever had come due.
//
// WHY it is gone, and why it should not come back in this shape:
//
// * The feature was only ever meant to exist where a provider could be TOLD to expire a
//   file itself. No provider offers server-side file TTL: Google Drive, Dropbox and
//   OneDrive were each re-checked against current API docs on 2026-08-03 and none of them
//   has one. `ProviderCaps::server_scheduled_delete` was therefore false for every row in
//   `registry()`, which made it a capability nothing could ever claim.
// * What shipped instead was a client-side substitute, and its guarantee was too weak to
//   keep: deletions only happened while OUR daemon happened to be running. An uninstall, a
//   laptop left off, or a user who simply stopped taking captures left the files in the
//   drive forever, while the settings copy had promised they would go. A promise that
//   holds only when the promising process is alive is not a retention policy.
// * Removed at the owner's direction. DRAGON-505 carries the history; the DRAGON-482 and
//   DRAGON-493 logs in `docs/archive/` carry the original design.
//
// `providers::delete_file` deliberately SURVIVED all of this. Two live paths need it, and
// neither is a deadline: the DRAGON-496 cancel that loses the race to a committed upload,
// and the DRAGON-507 undo during the meter's finish-hold. Both delete a file the user just
// asked about, right then, with the app plainly running. That is the shape of delete this
// app can honestly offer.

// The stage mount points. Each is a module doc and nothing else, so the stage that fills
// it adds a body without touching any file another stage owns.
pub mod child;
pub mod oauth;
pub mod providers;
pub mod upload;

/// Proton Drive (DRAGON-485), which connects through Proton's own official `proton-drive`
/// command-line tool rather than through an API of ours.
///
/// It sits beside [`oauth`] for the same reason that does: it is the whole way one provider is
/// reached, not one provider's REST calls. Where the other four speak OAuth over [`http`], this
/// one spawns a child process, so this module is the seam that finds the tool, decides whether
/// it is usable, and turns its exit codes into this app's `Result<T, String>` vocabulary.
pub mod proton;

/// Cross-process upload progress/cancellation (DRAGON-490): the marker files that let a
/// detached upload child and the editor that started it exchange progress, outcome and a
/// cancel request without either one holding a handle to the other.
pub mod session;

/// The ONE folder this app ever writes to, on every provider (DRAGON-498).
///
/// # Why every provider is sandboxed the same way
///
/// A screenshot tool asking for a user's whole drive is asking for far more than it needs, and
/// the three drives each offer a way to say so: Google's `drive.file` scope sees only what this
/// app created, Microsoft's `Files.ReadWrite.AppFolder` scope addresses only the app folder
/// Graph provisions for the registration, and a Dropbox app registered with **App folder**
/// access has a token whose ROOT already is that folder. Different mechanisms, one user-visible
/// result: everything this app uploads lands in a folder of this name, and nothing outside it is
/// reachable at all.
///
/// So this string is what the UI calls that folder wherever it names it, and, for the one
/// provider that has to make its own (Drive; see `providers::gdrive::find_or_create_root`), the
/// name it creates it under. The other two are named by the app REGISTRATION rather than by us:
/// Graph auto-provisions `Apps/<the registration's display name>` and Dropbox the same, which is
/// why `CLOUD_ACCOUNTS.md` asks for the registration to carry this name too.
///
/// Never a user-editable path. Manual paths were the previous model's Dropbox-only escape hatch,
/// and they are gone on purpose: a typed path is a way back out of the sandbox for the one
/// provider whose folders happen to be addressable as strings, which would make "least privilege
/// everywhere" a promise with an exception in it.
pub const APP_FOLDER: &str = "Cosmic Capture Kit";

/// The container OneDrive and Dropbox both auto-provision an app's folder INSIDE (DRAGON-501).
///
/// Not our name and not our choice: Graph puts the registration's app folder at
/// `Apps/<registration name>`, and a Dropbox app-folder registration lands in the same place, so
/// a user who goes looking for their uploads finds them one level down rather than at the top of
/// the drive. Named once here because it is the same fact twice, and stated in the provider's own
/// English wording, which is what both providers' documentation uses and what a row can honestly
/// promise without knowing the account's display language.
const APPS_CONTAINER: &str = "Apps";

// ---------------------------------------------------------------------------
// Baked client ids (DRAGON-508).
// ---------------------------------------------------------------------------

/// The client id a build with no app registration has: none.
///
/// Named rather than a bare `""` so a grep finds every entry an official build has to fill,
/// and so the empty string reads as a decision rather than an oversight.
const NO_CLIENT_ID: &str = "";

/// The client secret a build with no app registration has: none. [`NO_CLIENT_ID`]'s sibling,
/// named for the same reason.
const NO_CLIENT_SECRET: &str = "";

/// A compile-time value, or [`NO_CLIENT_ID`] when the variable was not set at build time.
///
/// A `const fn` rather than a `match` repeated at each constant, so all four read as one line
/// each and the "unset means empty" rule is written once.
const fn baked_or_empty(value: Option<&'static str>) -> &'static str {
    match value {
        Some(v) => v,
        None => NO_CLIENT_ID,
    }
}

// **The BAKED_ prefix is load-bearing, and it is not the same name as the runtime override.**
// The runtime variables (`CCK_GDRIVE_CLIENT_ID` and friends) are what a user with their own
// registration sets in their own shell, and a developer running `cargo build` in that shell
// must not get those ids compiled INTO the binary they are about to hand someone. Distinct
// names keep the two apart by construction: nothing a user is told to export can ever be
// baked, and the only things that set a `CCK_BAKED_*` variable are the build workflows
// (`.github/workflows/release.yml` and `build-only.yml`), which read them from repository
// configuration.
//
// **A note for forks.** These resolve to empty here, which is the correct and working state
// for a build made from source: the app then shows the setup guide rather than pretending it
// has a registration. If you ship your own build, please register your own app ids with each
// provider and bake those. The official ids are one project's quota and one project's
// reputation with each provider; a fork consuming them costs the users of both builds.
//
// **A caching gotcha, worth knowing before you chase a mystery.** Cargo does not rebuild a
// crate just because an `option_env!` variable changed value, so a warm `target/` (a CI cache,
// a local rebuild) can keep an older baked id. A changed value needs a rebuild that actually
// recompiles this crate.

/// Google Drive's client id, baked at build time (DRAGON-508).
const BAKED_GDRIVE_CLIENT_ID: &str = baked_or_empty(option_env!("CCK_BAKED_GDRIVE_CLIENT_ID"));

/// Google Drive's client SECRET, baked at build time. Google's "Desktop app" client type is
/// the one client type here that issues one; see [`AuthKind::OAuthPkce::client_secret_env`].
const BAKED_GDRIVE_CLIENT_SECRET: &str =
    baked_or_empty(option_env!("CCK_BAKED_GDRIVE_CLIENT_SECRET"));

/// OneDrive's client id, baked at build time. A public client: no secret to bake.
const BAKED_ONEDRIVE_CLIENT_ID: &str = baked_or_empty(option_env!("CCK_BAKED_ONEDRIVE_CLIENT_ID"));

/// Dropbox's client id, baked at build time. A public client: no secret to bake.
const BAKED_DROPBOX_CLIENT_ID: &str = baked_or_empty(option_env!("CCK_BAKED_DROPBOX_CLIENT_ID"));

// **YouTube has NO baked constant, on purpose** (DRAGON-508), and that is a quota decision
// rather than an oversight. The YouTube Data API gives a project 10,000 units per day, and a
// single `videos.insert` upload costs about 1,600 of them. One shared, baked project would
// therefore give EVERY user of the official build about six uploads a day between them, and
// the seventh person to record something that day would get a quota error with nothing they
// could do about it. A per-user registration has the whole 10,000 to itself, so
// self-registration is the intended stance here until a Google quota-extension audit is
// granted. `CCK_YOUTUBE_CLIENT_ID` / `CCK_YOUTUBE_CLIENT_SECRET` still work exactly as they
// always did, and `CLOUD_ACCOUNTS.md`'s YouTube section is what the app points a user at.

/// What one provider can actually DO. Read this, never the provider id: a screen that
/// branches on the id has to be revisited every time a provider is added, and a screen
/// that branches on a capability does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCaps {
    /// The provider's API can list folders, so the user can pick a destination folder
    /// rather than accepting the app's own. Without it, uploads land in one fixed place.
    pub folder_browse: bool,
    /// The provider can turn an uploaded file into a shareable link. Without it, an
    /// upload is a file transfer and nothing more: there is no link to copy back.
    pub share_links: bool,
    /// A share link can carry an expiry date the provider enforces. Distinct from
    /// [`Self::share_links`]: several providers make links and none of them expire.
    pub link_expiry: bool,
    /// A folder this app deletes lands somewhere the user can put it back from, rather than
    /// being removed for good (DRAGON-506).
    ///
    /// The one thing the delete confirmation's copy is allowed to key on, because the honest
    /// sentence really is different per provider and the difference is a FACT about the API we
    /// call, not a preference: Graph's `DELETE /items/{id}` moves the item to the OneDrive
    /// recycle bin, while Drive's `files.delete` is documented as "permanently deletes a file
    /// owned by the user without moving it to the trash" and Dropbox's `delete_v2` is treated
    /// the same way here, since restoring from its deleted-files history is a provider feature
    /// this app cannot promise on a given plan. So a screen asks THIS, never a provider id.
    ///
    /// `false` for a provider with no folders at all, where nothing here is ever read.
    pub delete_recoverable: bool,
    // `server_scheduled_delete` and `client_delete` stood here until DRAGON-505, and went
    // with the auto-delete feature that was their only reader (see this file's tombstone).
    /// The largest single file the provider accepts through the path we use, if it states
    /// one. `None` means "no documented limit we have to pre-check"; it does NOT mean
    /// unlimited, only that the failure comes back from the API rather than from us.
    pub max_file_bytes: Option<u64>,
    /// The provider is a sensible destination for a STILL (a screenshot/scan). Always paired
    /// with [`Self::accepts_videos`] for a general file-storage drive, which is what makes
    /// this NOT the same axis as media-hosting: a video-hosting provider (YouTube) is meant
    /// for one kind only, so this is `false` there (DRAGON-493). Read by the preview editor's
    /// Upload dropdown to offer only the providers relevant to the document actually open,
    /// never by branching on a provider id.
    pub accepts_images: bool,
    /// The provider is a sensible destination for a RECORDING (a screen capture video).
    /// `false` for a provider that exists ONLY for stills, if one is ever added; `true` for
    /// every provider today, file-storage and video-hosting alike (DRAGON-493).
    pub accepts_videos: bool,
    /// The provider lets us set a visibility (public / unlisted / private, or whichever subset
    /// it actually offers) on what we upload, distinct from [`Self::share_links`]: a
    /// file-storage drive makes a share permission on an existing file, while a video-hosting
    /// provider's uploads carry their OWN visibility as upload metadata, with no separate
    /// "make a link" call at all. `false` for every provider today except the two built
    /// specifically to host video (DRAGON-493). Paired with [`super::accounts::CloudAccount`]'s
    /// own `visibility` field the same way [`Self::folder_browse`] is paired with
    /// [`super::accounts::CloudAccount::folder_id`]: the capability says whether the control
    /// exists, the account field is what was last chosen.
    pub visibility: bool,
}

/// How a user connects an account.
///
/// Two variants and not one, because the second is a real state a provider can be in and
/// hiding it would be worse than showing it. Proton Drive and iCloud have no official
/// public API for third-party uploads. They still appear in [`registry`] so the settings
/// page can NAME them and say why they are not connectable, rather than leaving a user to
/// wonder whether we simply forgot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    /// OAuth 2.0 authorization code with PKCE, the flow every official provider here
    /// offers. The endpoint values are filled in by [`registry`], each verified against the
    /// provider's own documentation; [`oauth`] is what speaks the flow.
    OAuthPkce {
        /// The authorization endpoint the user's browser is sent to.
        auth_url: &'static str,
        /// The token endpoint the code and the refresh token are exchanged at.
        token_url: &'static str,
        /// The scopes requested. Kept as narrow as the feature allows: an upload target
        /// does not need read access to a user's whole drive.
        scopes: &'static [&'static str],
        /// The environment variable that overrides [`Self::OAuthPkce::baked_client_id`],
        /// so a build (or a user with their own app registration) can supply its own.
        client_id_env: &'static str,
        /// The client id BAKED INTO this build at compile time, or empty (DRAGON-508). A
        /// public client id is not a secret in the OAuth sense (PKCE is what protects the
        /// exchange), but it is still per-build configuration, hence the env override beside
        /// it. See the `BAKED_*` constants above for where the value comes from and why the
        /// official release workflow is the only thing that fills it in.
        baked_client_id: &'static str,
        /// The environment variable carrying this provider's OAuth client SECRET, for the
        /// one provider whose native client type issues one (DRAGON-489 follow-up).
        ///
        /// Google's "Desktop app" OAuth client type issues a client secret alongside the
        /// client id, and Google's own installed-app docs have the token exchange send it,
        /// even while noting it "is not treated as a secret" in this context (it ships in
        /// the app same as the client id, since PKCE is what actually protects the
        /// exchange). Microsoft's "Mobile and desktop applications" client type and
        /// Dropbox's "Allow public clients" setting issue none, so their exchange sends
        /// nothing extra; leaving a public-client provider's field here `None` is what keeps
        /// their request byte-identical to before this field existed. `None` for a provider
        /// with no secret to send; see `oauth::resolve_client_secret`.
        client_secret_env: Option<&'static str>,
        /// The client secret BAKED INTO this build at compile time, or empty (DRAGON-508).
        /// The secret half of [`Self::OAuthPkce::baked_client_id`], on the same chain: the
        /// runtime variable named by [`Self::OAuthPkce::client_secret_env`] wins, then this,
        /// then nothing extra is sent. Empty for every provider that declares no secret.
        baked_client_secret: &'static str,
    },
    /// Connected through an EXTERNAL TOOL the provider itself publishes, rather than through an
    /// API of ours (DRAGON-485; Proton Drive today).
    ///
    /// The provider has no third-party API, so there is no client id to resolve and no browser
    /// flow for [`oauth`] to run. What there is instead is a first-party command-line tool that
    /// owns the sign-in, the session and the transfers, which this app finds and spawns.
    ///
    /// **Availability is a RUNTIME question here, not a build-time one**, and that is the whole
    /// reason this is its own variant rather than an `OAuthPkce` with empty endpoints. Every
    /// other provider either has a registration baked in or does not, decided once at compile
    /// time by [`provider_available`]. This one depends on whether the user has installed the
    /// tool, which can change between two openings of the settings page, so the picker probes
    /// for it and offers install guidance instead of hiding the row.
    ExternalTool {
        /// Where the user gets it, opened when the row is selected and the tool is not there.
        /// Never opened by a package that BUNDLES the tool (the Flatpak, DRAGON-566): there
        /// the guidance names a packaging fault instead; see
        /// `proton::install_press_downloads`.
        download_url: &'static str,
        /// What to call it in the "install this" line, spelled as the command a user types.
        tool_name: &'static str,
    },
    /// No official API and no official tool either. The provider is listed and named, and
    /// connecting is refused with an explanation.
    ///
    /// iCloud Drive alone since DRAGON-485. Proton Drive was here until Proton shipped the
    /// official CLI that [`Self::ExternalTool`] now reaches it through.
    Unofficial,
}

impl AuthKind {
    /// Whether an account with this auth kind can be connected at all. Pure; unit-tested.
    ///
    /// A named predicate rather than a `matches!` at each call site, because the settings
    /// page, the connect action and the upload path all have to agree on it.
    ///
    /// **Test-only since DRAGON-508**, and kept deliberately rather than deleted. It used to be
    /// the picker's filter; the picker now asks the stronger question ([`provider_available`]:
    /// does a client id resolve), which this no longer adds anything to at a call site. What it
    /// still states precisely is "this provider can be connected at all", which is the invariant
    /// three test modules pin against the registry (every connectable provider has ops, has
    /// endpoints, claims no capability it cannot reach). Deleting it would leave those tests
    /// spelling the `matches!` in three places.
    ///
    /// [`Self::ExternalTool`] counts as connectable, because it can be: the tool signs the user
    /// in. Whether it is INSTALLED is a different question, asked at runtime; see that variant.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_connectable(&self) -> bool {
        matches!(self, AuthKind::OAuthPkce { .. } | AuthKind::ExternalTool { .. })
    }

    /// Whether this provider is reached by spawning a tool rather than by calling an API. Pure;
    /// unit-tested.
    ///
    /// Named so the picker and the connect path ask one question instead of each matching the
    /// variant, and so a second external-tool provider needs no new call sites.
    pub fn is_external_tool(&self) -> bool {
        matches!(self, AuthKind::ExternalTool { .. })
    }
}

/// One cloud drive, described at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderSpec {
    /// The stable id stored in `cloud_accounts.toml`. NEVER change one:
    /// a persisted account names its provider by this string, and a rename would orphan
    /// every account already connected.
    pub id: &'static str,
    /// The provider's name as the user knows it.
    pub display_name: &'static str,
    /// The app icon name for the provider's mark (see `widgets::icons`).
    pub icon: &'static str,
    // `support_note` (DRAGON-566) lived here until the owner's 2026-08-07 decision: a
    // per-row disclosure line, carried only by Proton ("Not officially supported by
    // Proton."), because Proton has no third-party API and does not endorse this
    // integration (it runs through Proton's MIT-licensed CLI). The owner reviewed
    // Proton's third-party branding terms (quoted in DRAGON-566) and accepted the risk;
    // the disclosure and the neutral row glyph were removed at owner direction, not by
    // oversight. Proton's stance itself is unchanged; if a disclosure is ever needed
    // again, re-add the field here so screens keep reading the table, never a provider
    // id.
    /// What it can do.
    pub caps: ProviderCaps,
    /// How a user connects it.
    pub auth: AuthKind,
    /// Where this provider keeps the folder this app writes to, as the path a user would follow
    /// in that provider's own file browser: one segment per level, outermost first (DRAGON-501).
    ///
    /// EMPTY for a provider with no folder concept at all (YouTube uploads to a channel) and for
    /// the two with no API, which is what makes "this account has no save path to show" a fact of
    /// the table rather than a guess a screen has to make. [`save_path`] is the one reader.
    ///
    /// **The leaf is stated per row on purpose, never assumed to be [`APP_FOLDER`] everywhere.**
    /// Drive's leaf IS that constant, and must stay it: Drive has no app-folder mechanism, so this
    /// app creates an ordinary folder of that name itself
    /// (`providers::gdrive::ensure_root_folder`), and a row naming anything else would name a
    /// folder the uploader never made. OneDrive's and Dropbox's leaves belong to the PROVIDER: it
    /// derives the folder's name from the app REGISTRATION's display name, and they read the same
    /// as Drive's today only because `CLOUD_ACCOUNTS.md` asks both registrations to carry this
    /// app's name. Renaming one registration is then an edit to one row here and to nothing else.
    pub app_folder_path: &'static [&'static str],
    /// Where this provider keeps PHOTO ALBUMS, if it has a second, album-shaped destination
    /// beside its folders (DRAGON-485). One segment per level, outermost first, exactly like
    /// [`Self::app_folder_path`].
    ///
    /// EMPTY for every provider whose only destination is a folder, which is all of them except
    /// Proton Drive. That emptiness is the CAPABILITY: a screen asks whether this row has an
    /// album root, never whether the account's provider happens to be the one that does, so the
    /// setup step's Files/Photos tabs appear for exactly the providers that have both.
    ///
    /// **Albums are FLAT and folders are not**, which is why this is a separate field rather
    /// than a second entry in the path above: there is nothing to descend into, so the picker
    /// built on it selects rather than navigates. `cloud::proton`'s [`proton::PHOTOS`] doc
    /// carries the whole of that reasoning and the instruction not to unify the two models.
    pub album_root: &'static [&'static str],
    /// The hosts a SHARE or web-view link for an uploaded file may be on (DRAGON-482).
    ///
    /// A share link is provider output, and it becomes a notification's click target and a
    /// Windows toast's protocol-activation launch value: a value we do not check is a value
    /// that decides what page opens on the user's machine. Empty for a provider that makes
    /// no links.
    pub web_hosts: &'static [&'static str],
}

/// Every provider this app knows about, in the order the settings page offers them.
///
/// The OAuth endpoints, scopes and client ids were filled in by stage A2, each verified
/// against the provider's current documentation; the URL beside every group says which page.
///
/// # Where every `baked_client_id` comes from
///
/// A client id is per-BUILD configuration, not source. Each row names the `BAKED_*` constant
/// above it, which is [`NO_CLIENT_ID`] in a build made from source and the official
/// registration's id in a build the release workflow made. A row whose id resolves to nothing
/// is not listed in the picker at all (see [`connectable_display_order`]); the app shows the
/// setup guide instead, and a user with their own registration supplies theirs through the
/// row's `client_id_env`. See `oauth::resolve_client_id` for the whole chain.
pub fn registry() -> &'static [ProviderSpec] {
    &[
        ProviderSpec {
            id: "gdrive",
            display_name: "Google Drive",
            icon: "brand-gdrive-symbolic",
            caps: ProviderCaps {
                folder_browse: true,
                share_links: true,
                // Drive's sharing permissions carry an expiration date, but only on paid
                // Workspace domains, so it cannot be offered as a general capability.
                link_expiry: false,
                // `files.delete` is documented as permanent: it "permanently deletes a file
                // owned by the user without moving it to the trash". There is nothing for the
                // user to restore afterwards, so the confirmation says so.
                delete_recoverable: false,
                max_file_bytes: None,
                // A general file-storage drive: either kind of capture belongs here.
                accepts_images: true,
                accepts_videos: true,
                visibility: false,
            },
            // Endpoints: developers.google.com/identity/protocols/oauth2/native-app, also
            // what accounts.google.com/.well-known/openid-configuration advertises.
            auth: AuthKind::OAuthPkce {
                auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
                token_url: "https://oauth2.googleapis.com/token",
                // LEAST PRIVILEGE, and it costs something real: `drive.file` grants access
                // only to files this app created or the user explicitly handed us, which is
                // why `providers::gdrive::list_folders` can only ever show folders this app
                // made. The alternative, `.../auth/drive`, is a RESTRICTED scope: it reads
                // the user's whole Drive and needs a Google verification review. A
                // screenshot uploader has no business holding it.
                scopes: &["https://www.googleapis.com/auth/drive.file"],
                client_id_env: "CCK_GDRIVE_CLIENT_ID",
                baked_client_id: BAKED_GDRIVE_CLIENT_ID,
                // Google's "Desktop app" client type issues a client secret; see the field's
                // own doc for why this is the one provider that needs one at all.
                client_secret_env: Some("CCK_GDRIVE_CLIENT_SECRET"),
                baked_client_secret: BAKED_GDRIVE_CLIENT_SECRET,
            },
            // One ordinary folder at the top of My Drive, because Drive offers no app-folder
            // mechanism a user is meant to find. THE SAME NAME `gdrive::ensure_root_folder`
            // creates, single-sourced through the constant so the row and the folder cannot drift.
            app_folder_path: &[APP_FOLDER],
            // No album destination: Drive has folders and nothing else.
            album_root: &[],
            // A Drive `webViewLink` is on `drive.google.com`; a Docs-converted item is on
            // `docs.google.com`. Nothing else is a Drive file view.
            web_hosts: &["drive.google.com", "docs.google.com"],
        },
        ProviderSpec {
            id: "onedrive",
            display_name: "OneDrive",
            icon: "brand-onedrive-symbolic",
            caps: ProviderCaps {
                folder_browse: true,
                share_links: true,
                link_expiry: true,
                // Graph's `DELETE /me/drive/items/{id}` moves the item to the OneDrive recycle
                // bin, where the user can put it back. The one provider here that can promise
                // that, and the reason the confirmation's copy is capability-keyed.
                delete_recoverable: true,
                max_file_bytes: None,
                accepts_images: true,
                accepts_videos: true,
                visibility: false,
            },
            // Endpoints: learn.microsoft.com/entra/identity-platform/v2-protocols,
            // cross-checked against the live metadata at
            // login.microsoftonline.com/common/v2.0/.well-known/openid-configuration.
            //
            // `common` rather than a tenant id or `consumers`, because it is the ONE issuer
            // that accepts both a personal Microsoft account and a work or school account,
            // and a screenshot tool has no way to know which kind a user has.
            auth: AuthKind::OAuthPkce {
                auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
                token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
                // Bare names resolve against Microsoft Graph when no resource is named, so
                // `Files.ReadWrite.AppFolder` IS
                // `https://graph.microsoft.com/Files.ReadWrite.AppFolder`.
                // `offline_access` is not optional: Microsoft returns a refresh token ONLY
                // when it is asked for by name. `openid`/`profile` are deliberately absent,
                // since nothing here needs to know who the user is.
                //
                // **`.AppFolder`, not the whole drive** (DRAGON-498). `Files.ReadWrite` grants
                // read/write over every file the user has; this one grants it over the app
                // folder Graph provisions for this registration and nothing else, which is the
                // whole of what an uploader needs. See [`APP_FOLDER`] for the uniform model,
                // and `providers::onedrive`'s module doc for the `special/approot` addressing
                // that goes with it. An account connected under the OLD scope keeps a token
                // that cannot reach `approot`: Graph answers 403, which `providers::api_failure`
                // already turns into a RECONNECT (`oauth::RECONNECT_PREFIX`), so the row offers
                // the one action that fixes it.
                scopes: &["Files.ReadWrite.AppFolder", "offline_access"],
                client_id_env: "CCK_ONEDRIVE_CLIENT_ID",
                baked_client_id: BAKED_ONEDRIVE_CLIENT_ID,
                // Microsoft's "Mobile and desktop applications" client type is a public
                // client: no secret.
                client_secret_env: None,
                baked_client_secret: NO_CLIENT_SECRET,
            },
            // Graph provisions `Apps/<the registration's display name>` and hands it back as
            // `special/approot`. The leaf is the REGISTRATION's, not ours; see the field's doc.
            app_folder_path: &[APPS_CONTAINER, APP_FOLDER],
            album_root: &[],
            // A personal-account share link is a `1drv.ms` short link or an
            // `onedrive.live.com` view; a work/school one is on the tenant's SharePoint host,
            // hence the leading-dot SUFFIX, which a lookalike `evilsharepoint.com` cannot
            // match.
            web_hosts: &["1drv.ms", "onedrive.live.com", ".sharepoint.com"],
        },
        ProviderSpec {
            id: "dropbox",
            display_name: "Dropbox",
            icon: "brand-dropbox-symbolic",
            caps: ProviderCaps {
                folder_browse: true,
                share_links: true,
                // Dropbox link expiry is a paid-plan feature, same reasoning as Drive.
                link_expiry: false,
                // `files/delete_v2` removes the folder. Dropbox does keep deleted files for a
                // window and can restore them, but that window and the restore UI depend on
                // the account's plan, so this app does not promise it in a confirmation the
                // user is about to act on.
                delete_recoverable: false,
                max_file_bytes: None,
                accepts_images: true,
                accepts_videos: true,
                visibility: false,
            },
            // Endpoints: dropbox.com/developers/documentation/http/documentation, the
            // "OAuth 2" section. Note the authorize endpoint is the ONLY call that goes to
            // www.dropbox.com; everything else is api/content.dropboxapi.com. (The reference
            // page's own curl examples write `api.dropbox.com` for the token endpoint, which
            // contradicts its normative URL line. The normative one is used here.)
            auth: AuthKind::OAuthPkce {
                auth_url: "https://www.dropbox.com/oauth2/authorize",
                token_url: "https://api.dropboxapi.com/oauth2/token",
                // Dropbox scopes are per-operation, so this is exactly the four the provider
                // ops use: write a file, read a folder listing, make a share link, read an
                // existing one. `files.content.read` is deliberately absent: nothing here
                // downloads.
                scopes: &[
                    "files.content.write",
                    "files.metadata.read",
                    "sharing.write",
                    "sharing.read",
                ],
                client_id_env: "CCK_DROPBOX_CLIENT_ID",
                baked_client_id: BAKED_DROPBOX_CLIENT_ID,
                // Dropbox's "Allow public clients" setting is a public client: no secret.
                client_secret_env: None,
                baked_client_secret: NO_CLIENT_SECRET,
            },
            // An App-folder registration's token has that folder as its own root, and Dropbox
            // files it under `Apps/<the app's name>` in the user's account, same as Graph.
            app_folder_path: &[APPS_CONTAINER, APP_FOLDER],
            album_root: &[],
            // A shared link is on `www.dropbox.com`; `db.tt` is Dropbox's own short-link
            // domain, which older links and some clients still produce.
            web_hosts: &["www.dropbox.com", "dropbox.com", "db.tt"],
        },
        ProviderSpec {
            id: "youtube",
            display_name: "YouTube",
            icon: "brand-youtube-symbolic",
            // The first VIDEO-HOSTING provider (DRAGON-493), not a file-storage drive: a still
            // has no business on a video platform, so `accepts_images` is false where every
            // drive above is true. `share_links` is true with no separate "make a link" call
            // needed at all: the watch URL a video gets IS its own share link
            // (`providers::youtube::create_share_link` just formats it from the video id).
            // `visibility` is the one genuinely new capability this provider adds: the upload
            // itself carries a public/unlisted/private choice as metadata, which
            // `CloudAccount::visibility` persists per account. 256GB / 12h are YouTube's
            // documented ceilings for a normal account; a real quota/verification/channel-tier
            // limit can still be lower, so this is a pre-check ceiling, not a promise.
            caps: ProviderCaps {
                folder_browse: false,
                share_links: true,
                link_expiry: false,
                // No folders at all, so nothing here is ever read; `false` is the only honest
                // value for a provider with nothing to delete a folder from.
                delete_recoverable: false,
                max_file_bytes: Some(256 * 1024 * 1024 * 1024),
                accepts_images: false,
                accepts_videos: true,
                visibility: true,
            },
            // Endpoints: the SAME Google OAuth endpoints Drive uses (developers.google.com/
            // identity/protocols/oauth2/native-app). YouTube Data API v3 lives in the same
            // Google Cloud project/OAuth client shape, it just needs its own scope requested
            // and its own API enabled in that project. A from-source build can reuse its
            // existing Drive client id/secret here (CLOUD_ACCOUNTS.md says so), or register a
            // separate one; either way this is its OWN registry row with its OWN env vars, so
            // connecting YouTube is always a separate Connect / separate stored account from
            // Drive, never implied by having Drive connected.
            auth: AuthKind::OAuthPkce {
                auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
                token_url: "https://oauth2.googleapis.com/token",
                // `youtube.force-ssl`, not the narrower `youtube.upload`: the upload-only scope
                // is refused by `videos.delete`, and this app has to be able to take a video
                // back off the channel. Two live paths do exactly that, and both are the user
                // asking, in the moment: the DRAGON-496 cancel that arrives after the upload
                // has already committed, and the DRAGON-507 undo during the meter's
                // finish-hold. Without delete access, pressing either would leave the video up.
                scopes: &["https://www.googleapis.com/auth/youtube.force-ssl"],
                client_id_env: "CCK_YOUTUBE_CLIENT_ID",
                // NOTHING is baked here, in an official build or any other, and the block of
                // `BAKED_*` constants above says why: one shared project's YouTube Data API
                // quota is about six uploads a day between every user of the build. This row
                // is self-registration only until a quota-extension audit says otherwise, so
                // YouTube is absent from the picker unless the user sets the variable below.
                baked_client_id: NO_CLIENT_ID,
                // Same reasoning as Google Drive's own row: Google's "Desktop app" client type
                // issues a secret even for a PKCE flow, and the installed-app docs still send
                // it.
                client_secret_env: Some("CCK_YOUTUBE_CLIENT_SECRET"),
                baked_client_secret: NO_CLIENT_SECRET,
            },
            // NO path: an upload becomes a video on a channel, which is not a place in a folder
            // tree. A row for this account names no destination at all (DRAGON-495/501), rather
            // than inventing a folder that cannot exist.
            app_folder_path: &[],
            // A channel is not a folder and it is not an album either.
            album_root: &[],
            // `youtu.be` is YouTube's own short-link domain; `www.youtube.com/watch?v=<id>` is
            // the ordinary long form `providers::youtube::create_share_link` builds. Neither is
            // reported back by the API (there is no `webViewLink`-equivalent field), so both are
            // named here for `web_url_allowed` to recognise, in case a share link is ever typed
            // or pasted back through a path that checks it.
            web_hosts: &["www.youtube.com", "youtu.be"],
        },
        ProviderSpec {
            id: "proton",
            display_name: "Proton Drive",
            icon: "brand-proton-drive-symbolic",
            // The icon above resolves to Proton's OFFICIAL mark again, and this row carries
            // no disclosure line any more: the owner reviewed Proton's third-party branding
            // terms (quoted in DRAGON-566) and accepted the risk on 2026-08-07, so the
            // DRAGON-566 neutral glyph and `support_note` were removed at owner direction,
            // not by oversight (see the tombstone on `ProviderSpec` and
            // `res/icons/brands/proton-drive.svg`). Proton still neither offers a
            // third-party API nor endorses this integration, which runs through their
            // MIT-licensed CLI.
            // Every row here was established by READING the official tool's source rather than
            // by trying an account (DRAGON-485); `cloud::proton`'s module doc records which
            // command backs each one.
            caps: ProviderCaps {
                // `filesystem list <path> --json` lists a level, so the ordinary folder step
                // works with no Proton-shaped special case.
                folder_browse: true,
                // `sharing set-url <path> --json` creates or updates a public link.
                share_links: true,
                // The tool DOES take an `--expiration`, so this is the one cap here that is
                // false out of caution rather than out of absence: it could not be confirmed
                // that Proton offers link expiry on a free plan, and the house rule (see Drive
                // and Dropbox above, both false for exactly this reason) is that a capability
                // is not promised until it is known to hold for everyone. Flip it with
                // evidence, not with the flag's existence.
                link_expiry: false,
                // `filesystem trash` moves an item to Proton's trash and `filesystem restore`
                // brings it back. Proton is the SECOND provider here that can honestly promise
                // that, after OneDrive, which is why the delete confirmation's copy keys on
                // this capability rather than on a provider id.
                delete_recoverable: true,
                max_file_bytes: None,
                // A general file-storage drive: either kind of capture belongs here.
                accepts_images: true,
                accepts_videos: true,
                visibility: false,
            },
            auth: AuthKind::ExternalTool {
                download_url: proton::DOWNLOAD_URL,
                tool_name: proton::TOOL_NAME,
            },
            // The tool addresses everything by path, and `/my-files` is the root of a user's own
            // files; the leaf is an ordinary folder this app creates, exactly as on Google
            // Drive, since Proton has no app-folder mechanism either.
            app_folder_path: &[proton::MY_FILES, APP_FOLDER],
            // **The only non-empty one** (DRAGON-485). A Proton drive keeps its albums in a
            // top-level section of their own, separate from the photo timeline, and they are
            // FLAT: the leaf is the album itself, chosen per account, so only the section is
            // named here.
            album_root: &[proton::ALBUMS],
            // A Proton share link is on the Drive web app's own host.
            web_hosts: &["drive.proton.me"],
        },
        ProviderSpec {
            id: "icloud",
            display_name: "iCloud Drive",
            icon: "brand-icloud-symbolic",
            caps: ProviderCaps {
                folder_browse: false,
                share_links: false,
                link_expiry: false,
                delete_recoverable: false,
                max_file_bytes: None,
                accepts_images: false,
                accepts_videos: false,
                visibility: false,
            },
            auth: AuthKind::Unofficial,
            app_folder_path: &[],
            album_root: &[],
            web_hosts: &[],
        },
    ]
}

/// The provider with this id, if we know it. `None` for an account written by a newer
/// build, or by hand: see [`accounts`] for why that account is KEPT rather than dropped.
pub fn provider(id: &str) -> Option<&'static ProviderSpec> {
    registry().iter().find(|p| p.id == id)
}

/// Where an account's uploads actually land, written as the path a user could follow in that
/// provider's own drive (DRAGON-501). Pure; unit-tested.
///
/// **The ONE builder.** Every surface that names a destination reads this: the settings row's
/// second line, and the post-connect step's summary sentence. They cannot disagree about where a
/// capture goes, because there is only one place the answer is written.
///
/// `subfolder` is the account's stored `folder_name` (`accounts::CloudAccount::folder_name`),
/// exactly as it came off disk, and since DRAGON-506 it can name a folder ANY number of levels
/// down: the setup step's list drills into subfolders, so a destination is remembered as its path
/// relative to the app folder and every level of it is rendered here. [`folder_path_segments`] is
/// where that string is split, so this function and the setup step cannot disagree about what a
/// level is.
///
/// Three inputs mean the same thing, THE APP FOLDER ITSELF, and all three answer with the bare
/// path:
///
/// * `None`, the two providers whose token root already IS the app folder, so nothing is stored.
/// * A blank or whitespace name, which is a hand-edited file rather than a folder.
/// * A name equal to the path's own LEAF, which is how Drive records "the root was picked": there
///   the app folder is an ordinary folder, so picking it stores its real id and its real name.
///   The name is the only signal a row has at render time (an account remembers its folder's id,
///   never the root's), which is the same collapse the row's old `destination_label` made and is
///   why a subfolder a user happens to name after the app folder reads as the root. Cosmetic, and
///   the alternative is a network round trip to render a settings row.
///
/// `None` for a provider with no folder concept ([`ProviderSpec::app_folder_path`] empty:
/// YouTube, and the two with no API) and for a provider this build has never heard of. The caller
/// then names no destination at all, which is the DRAGON-495 rule this extends: a YouTube row is
/// the provider's name and nothing else, never a folder that cannot exist.
pub fn save_path(provider_id: &str, subfolder: Option<&str>) -> Option<String> {
    let segments = provider(provider_id)?.app_folder_path;
    let leaf = segments.last()?;
    let mut path = String::new();
    for segment in segments {
        path.push('/');
        path.push_str(segment);
    }
    for name in folder_path_segments(subfolder, leaf) {
        path.push('/');
        path.push_str(name);
    }
    Some(path)
}

/// Whether this provider offers an ALBUM destination beside its folders (DRAGON-485). Pure;
/// unit-tested.
///
/// The capability behind [`ProviderSpec::album_root`], named so a screen asks one question
/// rather than testing a slice for emptiness in four places. `false` for every provider whose
/// only destination is a folder, which is the setup step's ordinary case: no tabs, one browser.
pub fn has_albums(spec: &ProviderSpec) -> bool {
    !spec.album_root.is_empty()
}

/// Where an ALBUM destination is, written as the path a user could follow. Pure; unit-tested.
///
/// [`save_path`]'s sibling for the other kind of destination, and deliberately a separate
/// function rather than an argument to it: an album is not a level of the file tree, so nothing
/// about it goes through [`folder_path_segments`], and there is no root-collapse rule to apply
/// because there is no root. `album` is the account's stored `folder_name`, which in this mode
/// names the album; a blank or absent one reads as [`APP_FOLDER`], the album this app finds or
/// creates on first use.
///
/// **A DISPLAY path, built from the album's NAME**, which is what a reader can act on. Not to be
/// confused with `proton::album_path`, which addresses an album by its UID because that is what
/// the tool needs and what an account stores in its `folder_id`. Two functions because they
/// answer two questions; the name is what a row shows and the uid is what a command takes.
///
/// `None` for a provider with no album root at all, which is every provider but one.
pub fn album_save_path(provider_id: &str, album: Option<&str>) -> Option<String> {
    let segments = provider(provider_id)?.album_root;
    if segments.is_empty() {
        return None;
    }
    let mut path = String::new();
    for segment in segments {
        path.push('/');
        path.push_str(segment);
    }
    let leaf = album.map(str::trim).filter(|name| !name.is_empty()).unwrap_or(APP_FOLDER);
    path.push('/');
    path.push_str(leaf);
    Some(path)
}

/// Where THIS ACCOUNT's uploads land, whichever kind of destination it holds. Pure;
/// unit-tested.
///
/// **The ONE builder every surface that names a destination should call** (DRAGON-485). It was
/// [`save_path`] until a provider gained a second kind of destination; that function still
/// answers the folder question and is still what this delegates to, but a screen must not have
/// to remember that an account can be pointed at an album instead, which is exactly the kind of
/// per-provider knowledge the capability table exists to keep out of the UI.
///
/// `None` for a provider with no destination to name at all (YouTube, and a provider this build
/// has never heard of), which is [`save_path`]'s own rule unchanged.
pub fn account_save_path(account: &accounts::CloudAccount) -> Option<String> {
    if uploads_to_album(account) {
        return album_save_path(&account.provider, account.folder_name.as_deref());
    }
    save_path(&account.provider, account.folder_name.as_deref())
}

/// Whether this account's uploads go to an ALBUM rather than to a folder. Pure; unit-tested.
///
/// **The capability is checked FIRST, and that ordering is the point.** `cloud_accounts.toml`
/// is ordinary config a user may hand-edit, so `destination = "photos"` can appear on an
/// account whose provider has no albums at all. That has to read as the folder the account
/// really still has, not as a destination nothing could deliver to; the stored kind selects
/// between two real answers, it never invents one.
///
/// The one question the upload path and the settings row both ask, so a capture cannot land
/// somewhere other than where the row says it will.
pub fn uploads_to_album(account: &accounts::CloudAccount) -> bool {
    account.destination_kind() == accounts::Destination::Photos
        && account.spec().is_some_and(has_albums)
}

/// The levels a stored destination sits BELOW the app folder, outermost first. Pure;
/// unit-tested.
///
/// `stored` is an account's `folder_name` exactly as it came off disk, and since DRAGON-506 that
/// can name a folder any number of levels down: the setup step's list drills into subfolders, so
/// a destination is remembered as its path RELATIVE to the app folder ("Screenshots/2026"),
/// joined with `/`. A destination one level down is a one-segment path, which is what every
/// account written before DRAGON-506 holds, so nothing on disk had to change.
///
/// **The argument stays one string rather than becoming a slice.** The account stores one string,
/// so a slice would only move the split to every caller and give this app two places that decide
/// what a segment is. Splitting HERE keeps one parser: blank segments are dropped (a hand-edited
/// file, a trailing slash) and each is trimmed.
///
/// The single-segment COLLAPSE is [`save_path`]'s old rule, kept and narrowed: a lone segment
/// equal to the path's own leaf is how Google Drive records "the app folder itself was picked"
/// (there it is an ordinary folder, so picking it stores its real name), and it answers with no
/// segments at all. It applies only to a ONE-segment name, because "Cosmic Capture Kit/2026" is a
/// real subfolder path and its first segment is not a way of saying "the root".
pub fn folder_path_segments<'a>(stored: Option<&'a str>, leaf: &str) -> Vec<&'a str> {
    let Some(stored) = stored else { return Vec::new() };
    let parts: Vec<&str> =
        stored.split('/').map(str::trim).filter(|part| !part.is_empty()).collect();
    if parts.len() == 1 && parts[0].eq_ignore_ascii_case(leaf) {
        return Vec::new();
    }
    parts
}

/// What this provider CALLS the folder our uploads land in: the LEAF of
/// [`ProviderSpec::app_folder_path`]. Pure; unit-tested.
///
/// Separate from [`save_path`] because the two answer different questions. A row names the place
/// to go and find the folder ("/Apps/Cosmic Capture Kit"), reading this field directly. The
/// post-connect step's body sentence used to read this field too, through its own private
/// wrapper, but the owner's latest wording for that sentence is a fixed, generic phrase with no
/// per-provider name in it at all.
///
/// `None` for a provider with no folder at all.
pub fn app_folder_name(provider_id: &str) -> Option<&'static str> {
    provider(provider_id)?.app_folder_path.last().copied()
}

/// The sort key a BRAND is ordered by, wherever one is listed. Pure; unit-tested.
///
/// Case-folded, because "iCloud Drive" and "Dropbox" are the same word to a reader whatever
/// their capitalisation, and a byte comparison would file every capital ahead of every
/// lowercase letter. The one key, so the provider picker and the account lists cannot end up
/// ordering the same brand differently (DRAGON-495).
pub fn brand_key(display_name: &str) -> String {
    display_name.to_lowercase()
}

/// Every provider, in the order a LIST should offer them: by brand name, carrying each one's
/// [`registry`] index. Pure; unit-tested.
///
/// The index rides along because it is what the picker's own message carries
/// (`CloudSettingsMsg::AddProviderSelected`), and it must keep meaning "the position in
/// `registry()`" however the list is arranged on screen. Sorting the DISPLAY without moving
/// the registry itself is also what keeps the registry free to stay in its own order, which
/// other things (the host allowlist, the docs) read positionally.
///
/// Sorted rather than hand-ordered: the registry grows by appending, and an appended provider
/// landing at the bottom of the list regardless of its name is exactly the "why is iCloud
/// above Dropbox" the owner reported (DRAGON-495).
pub fn registry_display_order() -> Vec<(usize, &'static ProviderSpec)> {
    let mut ordered: Vec<(usize, &'static ProviderSpec)> =
        registry().iter().enumerate().collect();
    ordered.sort_by_key(|(_, spec)| brand_key(spec.display_name));
    ordered
}

/// Whether THIS BUILD can connect `spec` at all: its client id resolves, from the runtime
/// override or from the value baked in at compile time. Pure; unit-tested.
///
/// **This is the ONE RULE the add-account picker is built on** (DRAGON-508). A provider is
/// offered if and only if pressing Connect on it could actually reach a sign-in page. There is
/// no flag beside it, no build feature, and no provider id named anywhere in UI code.
///
/// It subsumes the older, separate rule. Proton Drive and iCloud Drive have no official API
/// ([`AuthKind::Unofficial`]) and therefore no client id to resolve, so they fall out here
/// exactly as they used to fall out of an `is_connectable` filter, by the same test rather
/// than by a second one. It also covers the case that rule could not see: an OAuth provider
/// this build carries no registration for (every provider in a build made from source, and
/// YouTube in every build; see the `BAKED_*` block) is equally unpickable, and used to be
/// listed anyway with a Connect that could only explain itself.
///
/// `env_value` is the provider's `client_id_env` as the process sees it, injected rather than
/// read here (the house `foo_with` seam), so the decision is testable without touching
/// process-wide state.
pub fn provider_available(spec: &ProviderSpec, env_value: Option<&str>) -> bool {
    match spec.auth {
        AuthKind::OAuthPkce { baked_client_id, .. } => {
            oauth::resolved_client_id(env_value, baked_client_id).is_some()
        }
        // **Always offered, even with the tool absent** (DRAGON-485). This function answers
        // "can THIS BUILD connect it", and for an external-tool provider the build always can:
        // there is no registration to bake, so nothing about the build decides it. What decides
        // it is whether the user has installed the tool, which is a runtime probe the PICKER
        // makes so it can offer install guidance on the row. Excluding the row here would hide
        // the one provider whose missing piece the user can actually go and fix.
        AuthKind::ExternalTool { .. } => true,
        AuthKind::Unofficial => false,
    }
}

/// Every provider a user can actually CONNECT IN THIS BUILD, in display order.
///
/// [`registry_display_order`] is every provider we know of, which is what a table of coverage
/// wants. This is what a PICKER wants, and the two are different questions. The REGISTRY keeps
/// every entry regardless, because it is still the answer to "does this app support Proton"
/// everywhere that question is asked in prose (the README's feature table, `providers::ops`'s
/// "nobody can" refusal); only the picker stops listing what it cannot offer.
///
/// Reads the environment, so the pure half is [`connectable_display_order_with`], which is
/// what the tests drive.
pub fn connectable_display_order() -> Vec<(usize, &'static ProviderSpec)> {
    connectable_display_order_with(|name| std::env::var(name).ok())
}

/// [`connectable_display_order`] with the environment injected. Pure; unit-tested.
///
/// `env` is asked for one variable name at a time, exactly as the process would be, so a test
/// can describe a build ("Drive baked, nothing else") without setting a real variable and
/// making itself order-dependent against every other test in the binary.
///
/// **An EMPTY answer is a real, supported state**, not a bug: a build made from source with no
/// variables set can offer nothing, and the add dialog answers that with its own empty state
/// pointing at the setup guide rather than with a list of buttons that all fail.
pub fn connectable_display_order_with(
    env: impl Fn(&str) -> Option<String>,
) -> Vec<(usize, &'static ProviderSpec)> {
    registry_display_order()
        .into_iter()
        .filter(|(_, spec)| match spec.auth {
            AuthKind::OAuthPkce { client_id_env, .. } => {
                provider_available(spec, env(client_id_env).as_deref())
            }
            // No environment variable to consult: an external-tool provider is always listed,
            // and the row itself says whether the tool is there. See [`provider_available`].
            AuthKind::ExternalTool { .. } => true,
            AuthKind::Unofficial => false,
        })
        .collect()
}

/// Every https host [`registry`] names, de-duplicated, in registry order.
///
/// This is what [`http`] allows requests to reach. Deriving it from the table rather than
/// keeping a second list is what makes the allowlist impossible to forget: a provider
/// whose endpoints A2 fills in becomes reachable by that edit alone, and a host nothing in
/// the table names is never reachable at all. Pure; unit-tested.
pub fn registry_hosts() -> Vec<&'static str> {
    let mut hosts: Vec<&'static str> = Vec::new();
    for p in registry() {
        let AuthKind::OAuthPkce { auth_url, token_url, .. } = p.auth else {
            continue;
        };
        for url in [auth_url, token_url] {
            if let Some(h) = http::host_of(url)
                && !hosts.contains(&h)
            {
                hosts.push(h);
            }
        }
    }
    hosts
}

// ---------------------------------------------------------------------------
// Cross-process file locks (DRAGON-482).
// ---------------------------------------------------------------------------

/// How often a waiting lock retries.
const LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// Open `path` and take an EXCLUSIVE cross-process lock on it, or fail at once.
///
/// **The lock is the open HANDLE, not the file's existence.** It is released by the kernel
/// when the returned `File` drops, when the process exits, and when the process is killed,
/// which is what makes this safe where a lock FILE would go stale forever after one crash.
///
/// Unix uses `flock`, the same idiom [`crate::instance`] uses for the settings and daemon
/// locks. Windows opens with a share mode of ZERO, which is Windows' own way of saying "no
/// other process may open this file until my handle closes"; that IS the lock, and it needs no
/// platform-plugin code because `std` exposes it. Neither is a directory-wide lock: one file,
/// one lock, and the caller picks the file.
///
/// Two locks taken in the SAME process still exclude each other, on both, because each `open`
/// makes its own file description / handle. That is what lets this be unit-tested.
#[cfg(unix)]
fn lock_file_exclusive(path: &std::path::Path) -> Result<std::fs::File, ()> {
    use rustix::fs::{FlockOperation, flock};
    // Not truncating: nothing is ever written into a lock file, and truncating would be one
    // more way for a failed attempt to disturb the holder.
    let file = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|_| ())?;
    flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|_| ())?;
    Ok(file)
}

#[cfg(windows)]
fn lock_file_exclusive(path: &std::path::Path) -> Result<std::fs::File, ()> {
    use std::os::windows::fs::OpenOptionsExt as _;
    std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        // Same reason as the unix arm above, which had said so since it was written while
        // this one silently left the behaviour undefined: nothing is ever written into a
        // lock file, and truncating would be one more way for a failed attempt to disturb
        // the holder. Two arms of one function must not disagree about that.
        .truncate(false)
        .share_mode(0)
        .open(path)
        .map_err(|_| ())
}

/// No cross-process lock on a platform that is neither. The file is still opened so callers
/// have the same shape; this app targets Linux, macOS and Windows, so this arm exists to keep
/// the code portable rather than to be used.
#[cfg(not(any(unix, windows)))]
fn lock_file_exclusive(path: &std::path::Path) -> Result<std::fs::File, ()> {
    std::fs::File::options().read(true).write(true).create(true).open(path).map_err(|_| ())
}

/// [`lock_file_exclusive`], retrying until `budget` runs out.
///
/// `Err(())` means somebody else still holds it, and the caller turns that into its own
/// sentence. There is a budget at all because DRAGON-118's rule reaches here: a holder that
/// died in a way the OS did not notice, or one that is simply wedged, must not become a hang.
fn lock_file_waiting(
    path: &std::path::Path,
    budget: std::time::Duration,
) -> Result<std::fs::File, ()> {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if let Ok(file) = lock_file_exclusive(path) {
            return Ok(file);
        }
        if std::time::Instant::now() >= deadline {
            return Err(());
        }
        std::thread::sleep(LOCK_POLL);
    }
}

/// Whether `host` matches an entry in `allowed`. Pure; unit-tested.
///
/// An entry that starts with a dot is a SUFFIX (`.sharepoint.com` matches
/// `contoso-my.sharepoint.com` but not `evilsharepoint.com`, which is exactly why the dot is
/// part of the entry rather than implied). Anything else is an exact, case-insensitive match.
/// The same shape `providers::onedrive::is_upload_session_host` uses, lifted here so the two
/// URL questions this app asks about a provider are answered by one rule.
fn host_matches(host: &str, allowed: &[&str]) -> bool {
    if host.is_empty() {
        return false;
    }
    let host = host.to_ascii_lowercase();
    allowed.iter().any(|entry| match entry.strip_prefix('.') {
        Some(bare) => host.ends_with(&format!(".{bare}")) || host == bare,
        None => host == *entry,
    })
}

/// Whether a provider-supplied URL is one this app may hand to the OS opener, given the set of
/// hosts that provider legitimately uses. Pure; unit-tested.
///
/// Two conditions, and neither is optional:
///
/// * **https.** [`http::host_of`] answers `None` for anything else, which also rules out
///   `file://`, `javascript:` and the shell-handler schemes a desktop registers, any of which
///   would turn "open the link" into "run something".
/// * **A host the OWNING provider uses.** Not the union of every provider's hosts: a Dropbox
///   account's share link has no business opening a Microsoft page, and the union would let
///   one compromised provider reply point at another.
///
/// A URL that fails is not an error to show the user. The caller drops the OPEN affordance and
/// falls back to the local file (or to plain text), which is why this answers a bool.
fn url_allowed(url: &str, allowed: &[&str]) -> bool {
    http::host_of(url).is_some_and(|host| host_matches(host, allowed))
}

/// Whether `url` may be offered as `provider_id`'s share or web-view link. Pure; unit-tested.
///
/// Checked on the way OUT of an upload, because this value becomes a desktop notification's
/// click target and, on Windows, a toast's protocol-activation launch value. An unknown
/// provider allows nothing.
pub fn web_url_allowed(provider_id: &str, url: &str) -> bool {
    provider(provider_id).is_some_and(|spec| url_allowed(url, spec.web_hosts))
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    // `only_proton_carries_the_unofficial_disclosure` pinned the DRAGON-566
    // `support_note` field until the owner's 2026-08-07 decision removed the field with
    // the disclosure it carried (Proton's branding terms reviewed, risk accepted; see
    // the tombstone on `ProviderSpec`). The test went with the field: there is no
    // disclosure vocabulary left to pin.

    /// Ids are the on-disk vocabulary for a connected account, so they must be
    /// unique and stable. The exact list is asserted so a rename fails HERE rather than
    /// silently orphaning a user's connected account.
    #[test]
    fn provider_ids_are_unique_and_stable() {
        let ids: Vec<&str> = registry().iter().map(|p| p.id).collect();
        assert_eq!(ids, ["gdrive", "onedrive", "dropbox", "youtube", "proton", "icloud"]);
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "provider ids must be unique");
        for id in ids {
            assert_eq!(provider(id).map(|p| p.id), Some(id));
        }
        assert!(provider("not-a-provider").is_none());
    }

    /// **The provider LIST is ordered by brand** (DRAGON-495), not by the registry's own
    /// order, which only says when each provider was added to the table. Every provider is
    /// still present exactly once, and each keeps its REGISTRY index, which is what the
    /// picker's message carries.
    #[test]
    fn the_display_order_is_by_brand_and_loses_nobody() {
        let ordered = registry_display_order();
        assert_eq!(ordered.len(), registry().len(), "the display order dropped a provider");
        let names: Vec<&str> = ordered.iter().map(|(_, spec)| spec.display_name).collect();
        assert_eq!(
            names,
            ["Dropbox", "Google Drive", "iCloud Drive", "OneDrive", "Proton Drive", "YouTube"],
            "not in brand order"
        );
        // Case-folded, which is what puts "iCloud Drive" between Google and OneDrive rather
        // than after every capitalised name.
        for (index, spec) in ordered {
            assert_eq!(registry()[index].id, spec.id, "the registry index no longer matches");
        }
    }

    /// Build an environment reader from a list of (variable, value) pairs, so a test can
    /// describe a BUILD without setting a real variable. A real `set_var` would make these
    /// tests order-dependent against every other test in the binary, and would read whatever
    /// the developer happens to have exported.
    fn fake_env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| (*value).to_string())
        }
    }

    /// **The picker's list is the client ids that RESOLVE** (DRAGON-508), still in brand order,
    /// still carrying the REGISTRY index that `AddProviderSelected` sends back.
    ///
    /// The environment here supplies three of the four OAuth providers, which is the shape of
    /// an official build: Drive, OneDrive and Dropbox arrive with ids, YouTube does not (its
    /// quota makes a shared registration useless, see the `BAKED_*` block), iCloud has no API
    /// to resolve an id for, and Proton needs no id at all because its tool owns the sign-in.
    #[test]
    fn the_picker_offers_only_what_this_build_can_connect() {
        let offered = connectable_display_order_with(fake_env(&[
            ("CCK_GDRIVE_CLIENT_ID", "g-id"),
            ("CCK_ONEDRIVE_CLIENT_ID", "o-id"),
            ("CCK_DROPBOX_CLIENT_ID", "d-id"),
        ]));
        let names: Vec<&str> = offered.iter().map(|(_, spec)| spec.display_name).collect();
        assert_eq!(
            names,
            ["Dropbox", "Google Drive", "OneDrive", "Proton Drive"],
            "brand order: everything with an id, plus the external-tool provider"
        );
        for (index, spec) in &offered {
            assert!(spec.auth.is_connectable(), "{} cannot be connected", spec.id);
            assert_eq!(registry()[*index].id, spec.id, "the registry index no longer matches");
        }
        // YouTube is an ordinary OAuth provider that this build simply has no id for, and it
        // is hidden by the SAME rule that hides the two with no API. One rule, not two.
        assert!(!offered.iter().any(|(_, spec)| spec.id == "youtube"));
        // iCloud has no API and no tool, so nothing can ever offer it.
        assert!(!offered.iter().any(|(_, spec)| spec.id == "icloud"));
        assert!(registry().iter().any(|p| p.id == "icloud"));
        // **Proton is offered whatever the environment says** (DRAGON-485), because no
        // environment variable decides it: the question is whether the user has installed
        // Proton's tool, which is a RUNTIME probe the picker makes so the row can carry install
        // guidance. Hiding it here would hide the one provider whose missing piece is fixable.
        assert!(offered.iter().any(|(_, spec)| spec.id == "proton"));
    }

    /// **The rule itself**, one provider at a time: resolvable, unresolvable, external-tool,
    /// and the one with nothing at all.
    #[test]
    fn a_provider_is_available_exactly_when_its_client_id_resolves() {
        let spec = |id: &str| provider(id).expect("a registry provider");
        // Supplied at runtime: available.
        assert!(provider_available(spec("gdrive"), Some("an-app-id")));
        assert!(provider_available(spec("youtube"), Some("an-app-id")));
        // Nothing supplied, and nothing baked into a build made from source: not available.
        assert!(!provider_available(spec("gdrive"), None));
        assert!(!provider_available(spec("youtube"), None));
        // Whitespace is not an id.
        assert!(!provider_available(spec("dropbox"), Some("   ")));
        // A provider with no API at all can never be available, whatever is in the environment.
        assert!(!provider_available(spec("icloud"), None));
        assert!(!provider_available(spec("icloud"), Some("an-app-id")));
        // An EXTERNAL-TOOL provider is always available to the build, and no environment value
        // changes that: what decides it is whether the tool is installed, which is asked at
        // runtime and not here.
        assert!(provider_available(spec("proton"), None));
        assert!(provider_available(spec("proton"), Some("an-app-id")));
    }

    /// **The registration-less build is a supported state** (DRAGON-508): a build made from
    /// source with no variables set offers no OAUTH provider at all, and the add dialog answers
    /// with its empty state rather than with a list of buttons that can only fail. Pinned so
    /// nothing quietly starts listing a provider it has no registration for.
    #[test]
    fn a_build_with_no_ids_at_all_offers_nothing() {
        let offered = connectable_display_order_with(fake_env(&[]));
        // Everything that needs a REGISTRATION falls out. Proton is the deliberate exception
        // and the only one left: it needs no registration, so an unconfigured build can still
        // connect it once the user installs the tool (DRAGON-485).
        let names: Vec<&str> = offered.iter().map(|(_, spec)| spec.id).collect();
        assert_eq!(names, ["proton"], "an unconfigured build offers only the external-tool one");
        // And the registry is untouched by any of this.
        assert_eq!(registry_display_order().len(), registry().len());
    }

    /// YouTube is the one OAuth provider an OFFICIAL build still cannot bake (its API quota
    /// would be about six uploads a day shared between everyone), so its row carries no baked
    /// id and the user's own variable is the only way it appears. Pinned here because it is a
    /// deliberate product decision that reads like an omission.
    #[test]
    fn youtube_carries_no_baked_client_id() {
        let AuthKind::OAuthPkce { baked_client_id, .. } =
            provider("youtube").expect("youtube is registered").auth
        else {
            panic!("youtube must be an OAuthPkce provider");
        };
        assert_eq!(baked_client_id, NO_CLIENT_ID, "a baked YouTube id shares one daily quota");
    }

    /// The sort key is the one both surfaces use, so a brand cannot order differently in the
    /// provider picker than in the account list.
    #[rstest::rstest]
    #[case("Google Drive", "google drive")]
    #[case("iCloud Drive", "icloud drive")]
    #[case("DROPBOX", "dropbox")]
    fn the_brand_key_is_case_folded(#[case] name: &str, #[case] expected: &str) {
        assert_eq!(brand_key(name), expected);
    }

    /// Every provider is nameable and iconed: the settings page lists all of them,
    /// including the two that cannot be connected, so a blank row is never acceptable.
    #[test]
    fn every_provider_is_named_and_iconed() {
        for p in registry() {
            assert!(!p.display_name.is_empty(), "{} has no display name", p.id);
            assert!(
                crate::widgets::icons::is_bundled(p.icon),
                "{}'s icon `{}` is not a bundled glyph",
                p.id,
                p.icon
            );
        }
    }

    /// The two unofficial providers claim NOTHING. A cap set to true there would be a
    /// promise the connect path cannot keep, and the settings page reads caps to decide
    /// what to offer.
    #[test]
    fn an_unofficial_provider_claims_no_capability() {
        for p in registry().iter().filter(|p| p.auth == AuthKind::Unofficial) {
            assert!(!p.auth.is_connectable(), "{} must not be connectable", p.id);
            let c = p.caps;
            assert!(
                !c.folder_browse
                    && !c.share_links
                    && !c.link_expiry
                    && !c.accepts_images
                    && !c.accepts_videos
                    && !c.visibility,
                "{} claims a capability it has no API for",
                p.id
            );
        }
    }

    /// **The least-privilege scopes** (DRAGON-498). Each of these is the narrow one, and each
    /// has a wider sibling that would be easier and is refused: `Files.ReadWrite` reads the
    /// user's whole OneDrive, `.../auth/drive` their whole Google Drive. Pinned by name so
    /// widening one is a deliberate edit to a test that says why it must not be.
    #[test]
    fn no_provider_asks_for_more_of_a_drive_than_its_own_folder() {
        let scopes = |id: &str| match provider(id).expect("a known provider").auth {
            AuthKind::OAuthPkce { scopes, .. } => scopes,
            AuthKind::ExternalTool { .. } | AuthKind::Unofficial => &[][..],
        };
        assert_eq!(scopes("onedrive"), ["Files.ReadWrite.AppFolder", "offline_access"]);
        assert_eq!(scopes("gdrive"), ["https://www.googleapis.com/auth/drive.file"]);
        for (id, forbidden) in [
            ("onedrive", "Files.ReadWrite"),
            ("gdrive", "https://www.googleapis.com/auth/drive"),
        ] {
            assert!(
                !scopes(id).contains(&forbidden),
                "{id} asks for {forbidden}, which reaches the user's whole drive"
            );
        }
        // Dropbox's sandbox is the app registration's ACCESS TYPE (App folder), not a scope,
        // so its four per-operation scopes are unchanged by this and stay exactly as narrow.
        assert_eq!(
            scopes("dropbox"),
            ["files.content.write", "files.metadata.read", "sharing.write", "sharing.read"]
        );
    }

    /// **The save path and the folder capability are the same fact** (DRAGON-501). A provider
    /// whose folders can be listed has somewhere those folders are, and a provider with nowhere
    /// to list has no path to show. The settings row keys on the path and the setup step keys on
    /// the capability, so a row added with one and not the other would show a step that offers
    /// folders under a destination nothing names, or a destination for a provider that has none.
    #[test]
    fn every_provider_that_browses_folders_has_a_path_to_show() {
        for p in registry() {
            assert_eq!(
                p.caps.folder_browse,
                !p.app_folder_path.is_empty(),
                "{}'s folder capability and its save path disagree",
                p.id
            );
            for segment in p.app_folder_path {
                assert!(!segment.trim().is_empty(), "{} has a blank path segment", p.id);
                assert!(!segment.contains('/'), "{}'s segments must be one level each", p.id);
            }
        }
        // Drive's leaf is SINGLE-SOURCED with the folder `gdrive::ensure_root_folder` creates:
        // this app makes that folder itself, so a different name here would name a folder no
        // upload ever went to. The other two take their leaf from the app registration, which is
        // the provider's to decide, so nothing pins those to this constant.
        assert_eq!(provider("gdrive").expect("a known provider").app_folder_path, [APP_FOLDER]);
    }

    /// **The album axis is a registry row, not a provider id** (DRAGON-485), which is what lets
    /// the setup step ask one question to decide whether it draws tabs at all.
    #[test]
    fn only_one_provider_declares_an_album_root() {
        let with_albums: Vec<&str> =
            registry().iter().filter(|p| has_albums(p)).map(|p| p.id).collect();
        assert_eq!(with_albums, ["proton"], "the album axis is one row, and it says so here");
        for p in registry() {
            for segment in p.album_root {
                assert!(!segment.trim().is_empty(), "{} has a blank album segment", p.id);
                assert!(!segment.contains('/'), "{}'s album root is one level each", p.id);
            }
            // A provider with albums must ALSO have folders: the tabs are a choice between two
            // destinations, and a step with a Photos tab and no Files tab is not a thing this
            // page can draw.
            if has_albums(p) {
                assert!(p.caps.folder_browse, "{} offers albums but no folders", p.id);
            }
        }
    }

    /// The album path is built from the album's NAME, because that is what a reader can act on;
    /// the uid-shaped path the tool takes is `proton::album_path`, a different function for a
    /// different question.
    #[test]
    fn an_album_destination_reads_as_a_path_a_user_can_follow() {
        assert_eq!(album_save_path("proton", Some("Holidays")).as_deref(), Some("/albums/Holidays"));
        // A blank or absent album is the app's own, which is the one found or created on first
        // use, so a hand-edited file never renders a nameless destination.
        for blank in [None, Some(""), Some("   ")] {
            assert_eq!(
                album_save_path("proton", blank).as_deref(),
                Some("/albums/Cosmic Capture Kit"),
                "{blank:?}"
            );
        }
        // Every other provider has no album root and therefore no album path.
        for folders_only in ["gdrive", "onedrive", "dropbox", "youtube", "icloud"] {
            assert_eq!(album_save_path(folders_only, Some("Holidays")), None, "{folders_only}");
        }
        assert_eq!(album_save_path("a-drive-from-the-future", None), None);
    }

    /// **Absent means Files**, which is the whole migration story: every account written before
    /// the destination field existed keeps naming its folder.
    #[test]
    fn an_account_with_no_stored_kind_still_names_its_folder() {
        let account = accounts::CloudAccount {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            provider: "proton".to_string(),
            label: String::new(),
            folder_id: None,
            folder_name: Some("Shots".to_string()),
            added_at: String::new(),
            visibility: None,
            destination: None,
        };
        assert!(!uploads_to_album(&account));
        assert_eq!(
            account_save_path(&account).as_deref(),
            Some("/my-files/Cosmic Capture Kit/Shots")
        );
    }

    /// A Photos account names its ALBUM instead, and the row and the upload read the same
    /// answer.
    #[test]
    fn a_photos_account_names_its_album() {
        let account = accounts::CloudAccount {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            provider: "proton".to_string(),
            label: String::new(),
            folder_id: Some("/albums/vol~a".to_string()),
            folder_name: Some("Holidays".to_string()),
            added_at: String::new(),
            visibility: None,
            destination: Some(accounts::Destination::Photos),
        };
        assert!(uploads_to_album(&account));
        assert_eq!(account_save_path(&account).as_deref(), Some("/albums/Holidays"));
    }

    /// **The capability is checked FIRST**, and this is the case that proves why:
    /// `cloud_accounts.toml` is ordinary config a user can hand-edit, so `destination =
    /// "photos"` can land on a provider that has no albums at all. It has to read as the folder
    /// the account really still has, never as a destination nothing could deliver to.
    #[test]
    fn a_hand_edited_photos_kind_on_a_folders_only_provider_still_names_its_folder() {
        let account = accounts::CloudAccount {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            provider: "gdrive".to_string(),
            label: String::new(),
            folder_id: Some("folder-1".to_string()),
            folder_name: Some("Shots".to_string()),
            added_at: String::new(),
            visibility: None,
            destination: Some(accounts::Destination::Photos),
        };
        assert!(!uploads_to_album(&account), "a provider with no albums can never upload to one");
        assert_eq!(
            account_save_path(&account).as_deref(),
            Some("/Cosmic Capture Kit/Shots"),
            "the row names the folder the account really has"
        );
    }

    /// Link expiry without share links would be meaningless, and so would a promise about
    /// deleting a folder from a provider that has none (DRAGON-506).
    #[test]
    fn capability_pairs_stay_consistent() {
        for p in registry() {
            if p.caps.link_expiry {
                assert!(p.caps.share_links, "{} expires links it cannot make", p.id);
            }
            if p.caps.delete_recoverable {
                assert!(
                    p.caps.folder_browse,
                    "{} promises a way back from deleting a folder it has none of",
                    p.id
                );
            }
        }
    }

    /// **The delete confirmation's copy is keyed on this and on nothing else** (DRAGON-506).
    /// The three folder providers really do differ, and the difference is a fact about the API
    /// this app calls, not a preference: Graph's DELETE moves an item to the recycle bin, while
    /// Drive's `files.delete` is documented as permanent and Dropbox's restore window depends on
    /// the account's plan. Pinned per provider so a row cannot quietly start promising a way
    /// back that its API does not give.
    #[test]
    fn only_the_provider_that_recycles_a_deleted_folder_says_so() {
        let recoverable = |id: &str| provider(id).expect("a known provider").caps.delete_recoverable;
        assert!(recoverable("onedrive"), "Graph moves a deleted item to the recycle bin");
        assert!(!recoverable("gdrive"), "Drive's files.delete is permanent");
        assert!(!recoverable("dropbox"), "Dropbox's restore window is plan-dependent");
        // A provider with no folders never answers this question at all.
        assert!(!recoverable("youtube"));
    }

    /// The allowlist is derived, not maintained. Until A2 fills the endpoints there are no
    /// hosts, which is the honest answer: `http` then refuses every request rather than
    /// reaching somewhere unreviewed.
    #[test]
    fn hosts_come_only_from_the_registry() {
        for h in registry_hosts() {
            assert!(!h.is_empty(), "an empty host would allow anything");
        }
        // No duplicates: two providers on one host must yield one entry.
        let hosts = registry_hosts();
        let mut sorted = hosts.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), hosts.len());
    }
}

#[cfg(test)]
mod save_path_tests {
    use super::*;

    /// **The paths as a user reads them** (DRAGON-501), pinned as literal strings rather than
    /// rebuilt from the constants. These ARE the deliverable: the second line of an account's row
    /// promises a place the user can go and open, so the leading slash, the `Apps` level that only
    /// two providers have, and the subfolder join are all part of what is being asserted. If
    /// [`APP_FOLDER`] is ever renamed this table is the list of sentences that change with it.
    #[rstest::rstest]
    #[case("gdrive", None, Some("/Cosmic Capture Kit"))]
    #[case("gdrive", Some("Screenshots"), Some("/Cosmic Capture Kit/Screenshots"))]
    #[case("onedrive", None, Some("/Apps/Cosmic Capture Kit"))]
    #[case("onedrive", Some("Screenshots"), Some("/Apps/Cosmic Capture Kit/Screenshots"))]
    #[case("dropbox", None, Some("/Apps/Cosmic Capture Kit"))]
    #[case("dropbox", Some("Recordings"), Some("/Apps/Cosmic Capture Kit/Recordings"))]
    // NESTED destinations (DRAGON-506): the setup step's list drills into subfolders, so a
    // stored name can be a path, and every level of it has to reach the row. A row that named
    // only the leaf would send a user looking in the wrong folder.
    #[case("gdrive", Some("Screenshots/2026"), Some("/Cosmic Capture Kit/Screenshots/2026"))]
    #[case(
        "onedrive",
        Some("Work/Clients/Acme"),
        Some("/Apps/Cosmic Capture Kit/Work/Clients/Acme")
    )]
    // The folderless provider, with and without a name it should ignore: an upload becomes a
    // video on a channel, so there is no path either way.
    #[case("youtube", None, None)]
    #[case("youtube", Some("Screenshots"), None)]
    // Proton addresses by path from its own files root, so its app folder is a real place
    // (DRAGON-485), with and without a subfolder below it.
    #[case("proton", None, Some("/my-files/Cosmic Capture Kit"))]
    #[case("proton", Some("Screenshots/2026"), Some("/my-files/Cosmic Capture Kit/Screenshots/2026"))]
    // The one with no API at all, and a provider written by a newer build.
    #[case("icloud", None, None)]
    #[case("a-drive-from-the-future", Some("Screenshots"), None)]
    #[case("", None, None)]
    fn a_save_path_names_the_real_folder(
        #[case] provider_id: &str,
        #[case] subfolder: Option<&str>,
        #[case] expected: Option<&str>,
    ) {
        assert_eq!(save_path(provider_id, subfolder).as_deref(), expected);
    }

    /// The three ways of saying THE APP FOLDER ITSELF all answer with the bare path. The third is
    /// the one that matters in the field: Drive stores its root's real name when a user picks it,
    /// so without the collapse a Drive row would read `/Cosmic Capture Kit/Cosmic Capture Kit`.
    #[test]
    fn the_app_folder_is_never_named_twice() {
        let bare = save_path("gdrive", None);
        assert_eq!(bare.as_deref(), Some("/Cosmic Capture Kit"));
        assert_eq!(save_path("gdrive", Some("   ")), bare);
        assert_eq!(save_path("gdrive", Some(APP_FOLDER)), bare);
        // Case is not what distinguishes a folder from the root it sits in.
        assert_eq!(save_path("gdrive", Some("cosmic capture kit")), bare);
        // On the two whose leaf is the provider's, the same collapse applies to THEIR leaf, and
        // the `Apps` level above it is never mistaken for it.
        assert_eq!(save_path("onedrive", Some(APP_FOLDER)), save_path("onedrive", None));
        assert_eq!(
            save_path("onedrive", Some(APPS_CONTAINER)).as_deref(),
            Some("/Apps/Cosmic Capture Kit/Apps"),
            "only the leaf is the root; a folder named Apps inside it is an ordinary folder"
        );
        // Surrounding whitespace is trimmed off a real name, so a stray space cannot become a
        // second, different-looking destination.
        assert_eq!(save_path("dropbox", Some("  Screenshots  ")), save_path("dropbox", Some("Screenshots")));
    }

    /// **The collapse is for a LONE segment only** (DRAGON-506). A nested destination whose
    /// first level happens to be named after the app folder is a real subfolder path, and
    /// swallowing that level would name a folder the capture is not in.
    #[test]
    fn a_nested_path_keeps_every_level_it_has() {
        assert_eq!(
            save_path("gdrive", Some("Cosmic Capture Kit/2026")).as_deref(),
            Some("/Cosmic Capture Kit/Cosmic Capture Kit/2026")
        );
        // The segments are the levels, split in one place so the row and the setup step agree.
        assert_eq!(folder_path_segments(Some("Work/Clients/Acme"), APP_FOLDER), ["Work", "Clients", "Acme"]);
        assert_eq!(folder_path_segments(Some(APP_FOLDER), APP_FOLDER), Vec::<&str>::new());
        assert_eq!(folder_path_segments(None, APP_FOLDER), Vec::<&str>::new());
        // A hand-edited file's stray or doubled separators do not become empty levels, and each
        // level is trimmed the way a single name already was.
        assert_eq!(folder_path_segments(Some("/Work//2026/"), APP_FOLDER), ["Work", "2026"]);
        assert_eq!(folder_path_segments(Some(" Work / 2026 "), APP_FOLDER), ["Work", "2026"]);
        assert_eq!(
            save_path("dropbox", Some("/Work//2026/")).as_deref(),
            Some("/Apps/Cosmic Capture Kit/Work/2026")
        );
        assert_eq!(folder_path_segments(Some("///"), APP_FOLDER), Vec::<&str>::new());
    }

    /// Every path is absolute and has no empty or doubled separator, whatever the provider. A row
    /// shows this string verbatim, so a malformed one is a malformed sentence.
    #[test]
    fn a_path_is_absolute_and_well_formed() {
        for spec in registry() {
            for sub in [None, Some("Screenshots")] {
                let Some(path) = save_path(spec.id, sub) else {
                    continue;
                };
                assert!(path.starts_with('/'), "{}: {path}", spec.id);
                assert!(!path.contains("//"), "{}: {path}", spec.id);
                assert!(!path.ends_with('/'), "{}: {path}", spec.id);
                // Every segment of the provider's own app-folder path is in there, in order.
                // NOT `contains(APP_FOLDER)`: two of the three leaves belong to the app
                // registration, and this must keep passing the day one of them is renamed.
                let joined = spec.app_folder_path.join("/");
                assert!(path.starts_with(&format!("/{joined}")), "{}: {path}", spec.id);
            }
        }
    }
}

#[cfg(test)]
mod provider_url_tests {
    use super::*;

    /// The matcher's two forms, and the reason the suffix entries carry their leading dot.
    #[test]
    fn a_suffix_entry_needs_its_dot_and_an_exact_entry_is_exact() {
        assert!(host_matches("contoso-my.sharepoint.com", &[".sharepoint.com"]));
        assert!(host_matches("sharepoint.com", &[".sharepoint.com"]), "the bare domain counts");
        assert!(!host_matches("evilsharepoint.com", &[".sharepoint.com"]), "the dot is the point");
        assert!(host_matches("DRIVE.Google.COM", &["drive.google.com"]), "hosts are case-blind");
        assert!(!host_matches("evil.drive.google.com", &["drive.google.com"]), "exact is exact");
        assert!(!host_matches("drive.google.com.evil.test", &["drive.google.com"]));
        assert!(!host_matches("", &["drive.google.com"]), "an empty host matches nothing");
        assert!(!host_matches("drive.google.com", &[]), "an empty list allows nothing");
    }

    /// **The scheme half.** Anything that is not https is refused before the host is even
    /// looked at, which is what stops "open the link" from becoming "run something".
    #[test]
    fn only_an_https_url_can_be_opened() {
        for bad in [
            "http://drive.google.com/file/d/1",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ms-settings:privacy",
            "drive.google.com/file/d/1",
            "",
        ] {
            assert!(!web_url_allowed("gdrive", bad), "{bad} must be refused");
        }
        assert!(web_url_allowed("gdrive", "https://drive.google.com/file/d/1AbC/view"));
    }

    /// **The cross-provider case, which is the whole reason these lists are per-provider.**
    /// A reply from one provider must never be able to send the user to another's host, and
    /// the app-wide transport allowlist would happily accept every one of these.
    #[test]
    fn one_providers_url_is_not_anothers() {
        assert!(web_url_allowed("onedrive", "https://1drv.ms/i/s!AbC"));
        assert!(web_url_allowed("onedrive", "https://contoso-my.sharepoint.com/personal/x"));
        assert!(web_url_allowed("dropbox", "https://www.dropbox.com/s/abc/shot.png"));
        assert!(web_url_allowed("dropbox", "https://db.tt/abc"));
        // Each one on the others' hosts.
        assert!(!web_url_allowed("gdrive", "https://1drv.ms/i/s!AbC"));
        assert!(!web_url_allowed("dropbox", "https://drive.google.com/file/d/1"));
        assert!(!web_url_allowed("onedrive", "https://www.dropbox.com/s/abc"));
        // And the hosts this app talks to for OTHER reasons are still not link hosts.
        assert!(!web_url_allowed("gdrive", "https://www.googleapis.com/drive/v3/files/1"));
        assert!(!web_url_allowed("onedrive", "https://graph.microsoft.com/v1.0/me"));
    }

    /// A provider this build does not know allows NOTHING. That is the same answer
    /// `providers::ops` gives, and it is the safe one: an account written by a newer build
    /// has no host list here to check against.
    #[test]
    fn an_unknown_or_unofficial_provider_allows_nothing() {
        for id in ["a-drive-from-the-future", "proton", "icloud", ""] {
            assert!(!web_url_allowed(id, "https://drive.google.com/file/d/1"), "{id}");
        }
    }

    /// Every provider that claims a capability carries the hosts that capability produces.
    /// A row added without them would silently refuse every link the provider makes, which
    /// looks like a broken share rather than a missing table entry.
    #[test]
    fn the_host_lists_cover_the_capabilities_claimed() {
        for spec in registry() {
            if spec.caps.share_links {
                assert!(!spec.web_hosts.is_empty(), "{} makes links on no host", spec.id);
            }
            if !spec.auth.is_connectable() {
                assert!(spec.web_hosts.is_empty(), "{}", spec.id);
            }
            for host in spec.web_hosts {
                assert!(!host.is_empty(), "an empty entry in {}'s list allows nothing", spec.id);
                assert!(host.contains('.'), "{host} is not a host name");
            }
        }
    }
}
