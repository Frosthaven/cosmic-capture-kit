//! The NON-secret half of a connected cloud account (DRAGON-482).
//!
//! Stored as TOML in its OWN file, `cloud_accounts.toml`, beside `config.toml` under
//! [`crate::util::app_config_dir`]. Deliberately not part of `Persisted`:
//!
//! * `config.toml` is written whole on every settings save, from an `App` that holds a
//!   snapshot of it. An account added by one process would be clobbered by any other
//!   process saving a setting, and the accounts list is exactly the kind of thing a
//!   second window can change while the first is open.
//! * The account list has its own lifecycle (add, edit one field, remove) and is read by
//!   paths that never load the settings at all, such as the `--cloud-upload` child.
//!
//! The SECRETS never come here. [`super::secrets`] holds the tokens, keyed by
//! [`CloudAccount::id`]. This file is ordinary config a user may open, copy between
//! machines or paste into a support mail, and it must stay safe to do that with.
//!
//! # Tolerance
//!
//! An account naming a provider this build does not know is KEPT, listed and saved back
//! unchanged. A newer build (or a hand edit) must not lose a user's account just because
//! this one cannot use it; [`CloudAccount::spec`] returns `None` and the UI says so.

use serde::{Deserialize, Serialize};

/// The visibility a video-hosting upload carries (DRAGON-493): YouTube's own `privacyStatus`
/// speaks its own vocabulary; this is the small, portable vocabulary the editor's dropdown and
/// this file speak instead, so neither has to know a provider's own spelling. Only offered
/// where [`super::ProviderCaps::visibility`] is true (today: YouTube); every other account's
/// [`CloudAccount::visibility`] stays `None` and is never read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// Anyone can find and watch it.
    Public,
    /// Reachable only by a direct link: not searchable, not listed on the uploading account's
    /// public channel or profile.
    Unlisted,
    /// Only the uploading account (and, per provider, whoever else it names) can watch it.
    Private,
}

impl Visibility {
    /// What an account uses when it has never chosen one. Pure; unit-tested.
    ///
    /// Unlisted, not Public: a visibility nobody has actively picked yet should not be the ONE
    /// choice that broadcasts the upload. Public is still one click away in the dropdown; it is
    /// just never the silent default.
    pub fn default_choice() -> Visibility {
        Visibility::Unlisted
    }
}

#[cfg(test)]
mod visibility_tests {
    use super::*;

    /// The silent default is the private-leaning end of the scale, never Public: a visibility
    /// nobody actively picked must not be the one choice that broadcasts the upload.
    #[test]
    fn the_default_choice_is_unlisted_not_public() {
        assert_eq!(Visibility::default_choice(), Visibility::Unlisted);
    }

    /// The on-disk spelling is lowercase and stable: `cloud_accounts.toml` is ordinary config a
    /// user may open or hand-edit, and a rename here would silently reset every account's
    /// chosen visibility back to "never chosen" the next time the file is read.
    ///
    /// Wrapped in a one-field struct rather than serialized bare: TOML's root must be a table,
    /// so a bare scalar (which is all `Visibility` is) cannot be a document by itself, the same
    /// shape it actually appears in (a field on [`CloudAccount`], never the whole file).
    #[test]
    fn the_wire_spelling_is_lowercase_and_stable() {
        #[derive(Serialize)]
        struct Wrapper {
            v: Visibility,
        }
        assert_eq!(toml::to_string(&Wrapper { v: Visibility::Public }).unwrap().trim(), "v = \"public\"");
        assert_eq!(toml::to_string(&Wrapper { v: Visibility::Unlisted }).unwrap().trim(), "v = \"unlisted\"");
        assert_eq!(toml::to_string(&Wrapper { v: Visibility::Private }).unwrap().trim(), "v = \"private\"");
    }
}

/// WHICH KIND of place an account uploads to (DRAGON-485).
///
/// Almost every provider has exactly one: a folder. Proton Drive has two, because a Proton
/// drive keeps photos in a section of its own whose containers are flat ALBUMS rather than
/// nested folders, and an album is not a folder that happens to hold pictures. So an account
/// records which of the two it was pointed at, and [`CloudAccount::folder_id`] /
/// [`CloudAccount::folder_name`] then name whichever one that is.
///
/// **One pair of fields, not two.** An account has exactly one destination at a time, so
/// storing a folder AND an album would be storing one answer plus a stale one; this enum is
/// what says which of them the pair currently means.
///
/// The capability is a registry row ([`super::ProviderSpec::album_root`]), never a provider id,
/// so a screen asks the table whether this axis exists at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Destination {
    /// The ordinary file tree: the app folder, or a folder inside it.
    Files,
    /// A photo album.
    Photos,
}

impl Destination {
    /// What an account uses when it has never chosen one. Pure; unit-tested.
    ///
    /// Files, and it is the whole migration story: every account written before this field
    /// existed has no value here, and every provider that has only folders can never have
    /// another, so "absent" and "files" have to be the same thing.
    pub fn default_choice() -> Destination {
        Destination::Files
    }
}

/// One connected account, as stored on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudAccount {
    /// The account's own id, and the key [`super::secrets`] stores its tokens under.
    /// Random hex from the OS CSPRNG (see [`new_id`]), never derived from the user's
    /// email or drive: this string ends up in a keyring item name.
    pub id: String,
    /// The [`super::ProviderSpec::id`] this account belongs to. An unknown value is kept
    /// (see the module doc).
    pub provider: String,
    /// What the user calls this account in the picker.
    #[serde(default)]
    pub label: String,
    /// The destination folder's provider-side id, when one was chosen. `None` means this
    /// app's own folder on that provider ([`super::APP_FOLDER`]), which is the only other
    /// place an upload can land: since DRAGON-498 every provider is confined to that folder,
    /// so this is either it or something INSIDE it, never the drive at large.
    ///
    /// On Google Drive the app folder is an ordinary folder this app made, so an account
    /// pointed at it stores its id here like any other; on OneDrive and Dropbox the token's
    /// own root IS that folder, so `None` addresses it with no id to store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<String>,
    /// The destination folder's display name, so the settings row can name it without a
    /// network round trip. Cosmetic: [`Self::folder_id`] is what an upload uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_name: Option<String>,
    /// When the account was connected, RFC 3339. Display only, and the tiebreaker for a
    /// stable list order.
    #[serde(default)]
    pub added_at: String,
    /// The visibility chosen for uploads to this account (DRAGON-493), for a provider whose
    /// [`super::ProviderCaps::visibility`] is true. `None` means "never chosen yet"; an upload
    /// then falls back to [`Visibility::default_choice`]. Persisted PER ACCOUNT, the same way
    /// [`Self::folder_id`] already is: the editor's Upload
    /// dropdown writes this the moment a user picks a different visibility, so switching
    /// accounts in that dropdown recalls what was last chosen for THAT one. `None` (never
    /// read) for every provider without the capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    /// Which KIND of destination [`Self::folder_id`] / [`Self::folder_name`] name
    /// (DRAGON-485), for the one provider that has two of them. `None` means
    /// [`Destination::Files`], which is what every account written before this field existed
    /// holds and the only thing a folders-only provider can mean; read it through
    /// [`Self::destination_kind`] rather than matching the `Option` at a call site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<Destination>,
}

impl CloudAccount {
    /// The provider this account belongs to, if this build knows it.
    pub fn spec(&self) -> Option<&'static super::ProviderSpec> {
        super::provider(&self.provider)
    }

    /// Which kind of destination this account uploads to. Pure; unit-tested.
    ///
    /// The ONE reader of [`Self::destination`], so "absent means files" is decided once rather
    /// than at every call site that has to know where an upload goes.
    pub fn destination_kind(&self) -> Destination {
        self.destination.unwrap_or_else(Destination::default_choice)
    }

    /// The name to show for this account: the user's label, or a fallback that still says
    /// something useful when the label is empty (an account connected before labels, or a
    /// hand-edited file). Pure; unit-tested.
    pub fn display_label(&self) -> String {
        if !self.label.trim().is_empty() {
            return self.label.trim().to_string();
        }
        match self.spec() {
            Some(p) => p.display_name.to_string(),
            None => self.provider.clone(),
        }
    }

    /// The BRAND this account is filed under: its provider's display name, or the raw
    /// provider id for one this build does not know. Pure; unit-tested.
    ///
    /// The raw id rather than a placeholder for the unknown case, for the same reason
    /// [`Self::display_label`] falls back to it: an account written by a newer build is kept,
    /// so it has to sort somewhere, and its own id is the only honest name available.
    pub fn brand_name(&self) -> String {
        match self.spec() {
            Some(p) => p.display_name.to_string(),
            None => self.provider.clone(),
        }
    }
}

/// The order connected accounts are LISTED in, everywhere they are listed. Pure; unit-tested.
///
/// Brand first, then the user's own name for the account within that brand (DRAGON-495, owner
/// request): the accounts file's own order is the order they happened to be connected in,
/// which is meaningless to anyone looking for one of them. Both keys are case-folded through
/// [`super::brand_key`]'s rule, since "work" and "Work" are the same word to a reader.
///
/// The id breaks a remaining tie, so two identically named accounts on one provider still have
/// ONE order rather than one that depends on the sort's stability and the file's order. It is
/// never shown, so it never has to be a sensible tiebreak, only a deterministic one.
///
/// **One comparator, three surfaces** (the settings list, the preview editor's upload picker,
/// and anything either of them grows). A second ordering rule somewhere would mean the same
/// two accounts swapping places between two windows of the same app.
pub fn compare(a: &CloudAccount, b: &CloudAccount) -> std::cmp::Ordering {
    super::brand_key(&a.brand_name())
        .cmp(&super::brand_key(&b.brand_name()))
        .then_with(|| {
            super::brand_key(&a.display_label()).cmp(&super::brand_key(&b.display_label()))
        })
        .then_with(|| a.id.cmp(&b.id))
}

/// `accounts` in display order (see [`compare`]).
///
/// A function taking and returning the list, rather than a sort in [`list`]: `list` is also
/// what every WRITE path re-reads before saving, and sorting there would silently rewrite the
/// file into a new order on the next unrelated edit. The file stays in connection order; only
/// what a user looks at is sorted.
pub fn sorted(mut accounts: Vec<CloudAccount>) -> Vec<CloudAccount> {
    accounts.sort_by(compare);
    accounts
}

/// The on-disk document. A named table rather than a bare array so the file can gain
/// top-level keys later without breaking every already-written file.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct AccountFile {
    /// Every connected account, in the order they were added.
    accounts: Vec<CloudAccount>,
}

/// The accounts file, beside `config.toml`.
pub fn path() -> Option<std::path::PathBuf> {
    Some(crate::util::app_config_dir()?.join("cloud_accounts.toml"))
}

/// A fresh account id: 128 bits of OS randomness as lowercase hex.
///
/// The OS CSPRNG rather than a timestamp or a counter, because the id names a keyring item
/// and two accounts colliding would mean one silently reading the other's tokens. 32 hex
/// characters is also what [`super::secrets`]'s id validator accepts, which is what keeps a
/// crafted id out of the file-fallback path.
pub fn new_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // The OS refusing randomness is not something we can paper over with a weaker
        // source, and it is not something the caller can act on either. Say so and let the
        // id be rejected downstream rather than minting a guessable one.
        log::error!("cloud accounts: the OS random source is unavailable; no account id was minted");
        return String::new();
    }
    let mut out = String::with_capacity(32);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Every connected account, in file order. An unreadable or malformed file yields an EMPTY
/// list rather than an error: a caller asking "what accounts are there" can always be
/// answered, and the settings page is where a broken file is worth reporting.
pub fn list() -> Vec<CloudAccount> {
    let Some(path) = path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    match toml::from_str::<AccountFile>(&text) {
        Ok(f) => f.accounts,
        Err(e) => {
            // The path is described, never printed (CLAUDE.md's privacy rule).
            log::warn!(
                "cloud accounts: could not parse the accounts file ({}): {e}",
                crate::diag::path_shape(&path)
            );
            Vec::new()
        }
    }
}

/// One account by id.
pub fn get(id: &str) -> Option<CloudAccount> {
    list().into_iter().find(|a| a.id == id)
}

/// Write the whole list back, replacing the file.
pub fn save(accounts: &[CloudAccount]) -> Result<(), String> {
    let path = path().ok_or_else(|| "The app config folder could not be located.".to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| {
            format!("The app config folder could not be created: {e}")
        })?;
    }
    let file = AccountFile { accounts: accounts.to_vec() };
    let text = toml::to_string(&file)
        .map_err(|e| format!("The cloud accounts could not be written: {e}"))?;
    std::fs::write(&path, text).map_err(|e| {
        format!("The cloud accounts file could not be saved: {e}")
    })
}

/// Add an account (or replace one with the same id, which is what a re-connect is).
pub fn upsert(account: CloudAccount) -> Result<(), String> {
    let mut all = list();
    match all.iter_mut().find(|a| a.id == account.id) {
        Some(slot) => *slot = account,
        None => all.push(account),
    }
    save(&all)
}

/// Remove an account from the list. Returns whether one was there to remove.
///
/// The account's SECRETS are not touched here: dropping the tokens is
/// [`super::secrets::delete`]'s job, and the disconnect path calls both. Splitting them
/// keeps this file a pure list edit, and lets a caller re-write the list without ever
/// risking the keyring.
pub fn remove(id: &str) -> Result<bool, String> {
    let mut all = list();
    let before = all.len();
    all.retain(|a| a.id != id);
    if all.len() == before {
        return Ok(false);
    }
    save(&all)?;
    Ok(true)
}

#[cfg(test)]
mod account_file_tests {
    use super::*;

    fn sample() -> CloudAccount {
        CloudAccount {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            provider: "gdrive".to_string(),
            label: "Work Drive".to_string(),
            folder_id: Some("folder-1".to_string()),
            folder_name: Some("Screenshots".to_string()),
            added_at: "2026-08-02T10:00:00+00:00".to_string(),
            visibility: None,
            destination: None,
        }
    }

    /// The round trip through the real serializer, since that is what the file IS. Every
    /// field has to survive: a dropped `folder_id` would silently move a user's uploads.
    #[test]
    fn an_account_survives_a_toml_round_trip() {
        let file = AccountFile { accounts: vec![sample()] };
        let text = toml::to_string(&file).expect("serialize");
        let back: AccountFile = toml::from_str(&text).expect("parse");
        assert_eq!(back.accounts, vec![sample()]);
    }

    /// The optional fields are OMITTED when unset (TOML cannot write a literal none), and
    /// an omitted key reads back as `None` rather than failing the parse.
    #[test]
    fn unset_options_are_omitted_and_read_back_as_none() {
        let bare = CloudAccount {
            folder_id: None,
            folder_name: None,
            ..sample()
        };
        let text = toml::to_string(&AccountFile { accounts: vec![bare.clone()] }).expect("serialize");
        assert!(!text.contains("folder_id"), "an unset option must not be written: {text}");
        let back: AccountFile = toml::from_str(&text).expect("parse");
        assert_eq!(back.accounts, vec![bare]);
    }

    /// The visibility choice (DRAGON-493) round-trips the same way `folder_id` already
    /// does: `Some` survives, and `None` is omitted from the file rather than written as a
    /// literal absence, so an account connected before this field existed still parses.
    #[test]
    fn visibility_round_trips_and_is_omitted_when_unset() {
        let chosen = CloudAccount { visibility: Some(Visibility::Unlisted), ..sample() };
        let text = toml::to_string(&AccountFile { accounts: vec![chosen.clone()] }).expect("serialize");
        assert!(text.contains("visibility"), "a chosen visibility must be written: {text}");
        let back: AccountFile = toml::from_str(&text).expect("parse");
        assert_eq!(back.accounts, vec![chosen]);

        let unset = CloudAccount { visibility: None, ..sample() };
        let text = toml::to_string(&AccountFile { accounts: vec![unset.clone()] }).expect("serialize");
        assert!(!text.contains("visibility"), "an unset visibility must not be written: {text}");
        let back: AccountFile = toml::from_str(&text).expect("parse");
        assert_eq!(back.accounts, vec![unset]);
    }

    /// **An account file written before DRAGON-505 still loads** (DRAGON-505). The
    /// auto-delete feature is gone and `auto_delete_hours` went with it, but a user who has
    /// ever set a window has that key sitting in their `cloud_accounts.toml` right now. It
    /// is a leftover, not a schema break: serde ignores keys it does not know, so the
    /// account parses whole and the next save simply writes it back out without the key.
    ///
    /// Pinned as its own test rather than trusted to the generic unknown-key case above,
    /// because this exact key is on real users' disks and the failure it guards against
    /// (every connected account vanishing from the settings page after an update) is the
    /// worst thing this file can do.
    #[test]
    fn an_account_written_before_auto_delete_was_removed_still_loads() {
        let text = "\
[[accounts]]
id = \"cccccccccccccccccccccccccccccccc\"
provider = \"gdrive\"
label = \"Work Drive\"
folder_id = \"folder-1\"
auto_delete_hours = 24
added_at = \"2026-08-02T10:00:00+00:00\"
";
        let parsed: AccountFile = toml::from_str(text).expect("the retired key is ignored");
        assert_eq!(parsed.accounts.len(), 1, "the account must not be lost with the key");
        let account = &parsed.accounts[0];
        assert_eq!(account.provider, "gdrive");
        assert_eq!(account.label, "Work Drive", "every other field survives intact");
        assert_eq!(account.folder_id.as_deref(), Some("folder-1"), "the destination survives");
        // And writing it back drops the retired key rather than preserving it forever.
        let written = toml::to_string(&parsed).expect("serialize");
        assert!(!written.contains("auto_delete_hours"), "the key is not written back: {written}");
    }

    /// The destination KIND round-trips the way `visibility` already does: `Some` survives, and
    /// `None` is omitted rather than written, so an account connected before this field existed
    /// parses unchanged and one on a folders-only provider never gains the key at all.
    #[test]
    fn the_destination_kind_round_trips_and_is_omitted_when_unset() {
        let photos = CloudAccount { destination: Some(Destination::Photos), ..sample() };
        let text = toml::to_string(&AccountFile { accounts: vec![photos.clone()] }).expect("ser");
        assert!(text.contains("destination = \"photos\""), "{text}");
        let back: AccountFile = toml::from_str(&text).expect("parse");
        assert_eq!(back.accounts, vec![photos]);

        let unset = CloudAccount { destination: None, ..sample() };
        let text = toml::to_string(&AccountFile { accounts: vec![unset.clone()] }).expect("ser");
        assert!(!text.contains("destination"), "an unset kind must not be written: {text}");
        assert_eq!(toml::from_str::<AccountFile>(&text).expect("parse").accounts, vec![unset]);
    }

    /// **An account written before the destination field existed still loads, and still uploads
    /// to its folder.** This is the whole migration story: there is no rewrite, no default
    /// written on read, and no way for an existing account to be quietly re-pointed.
    #[test]
    fn an_account_written_before_the_destination_field_still_uploads_to_its_folder() {
        let text = "\
[[accounts]]
id = \"dddddddddddddddddddddddddddddddd\"
provider = \"proton\"
label = \"Personal\"
folder_name = \"Shots\"
added_at = \"2026-08-02T10:00:00+00:00\"
";
        let parsed: AccountFile = toml::from_str(text).expect("an absent kind still parses");
        assert_eq!(parsed.accounts.len(), 1);
        assert_eq!(parsed.accounts[0].destination, None);
        assert_eq!(parsed.accounts[0].destination_kind(), Destination::Files);
        assert_eq!(Destination::default_choice(), Destination::Files);
    }

    /// The on-disk spelling is lowercase and stable, for the same reason `visibility`'s is: a
    /// rename here would silently move every Photos account back to its folder.
    #[test]
    fn the_destination_wire_spelling_is_lowercase_and_stable() {
        #[derive(Serialize)]
        struct Wrapper {
            d: Destination,
        }
        assert_eq!(toml::to_string(&Wrapper { d: Destination::Files }).unwrap().trim(), "d = \"files\"");
        assert_eq!(toml::to_string(&Wrapper { d: Destination::Photos }).unwrap().trim(), "d = \"photos\"");
    }

    /// An account naming a provider this build does not know must LOAD, not vanish. This
    /// is the forward-compatibility promise: a newer build's account survives a downgrade,
    /// and a save from this build writes it back untouched.
    #[test]
    fn an_unknown_provider_is_kept_and_written_back() {
        let text = "\
[[accounts]]
id = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"
provider = \"a-drive-from-the-future\"
label = \"Later\"
added_at = \"2026-08-02T10:00:00+00:00\"
";
        let parsed: AccountFile = toml::from_str(text).expect("an unknown provider still parses");
        assert_eq!(parsed.accounts.len(), 1);
        let account = &parsed.accounts[0];
        assert!(account.spec().is_none(), "this build must not claim to know it");
        // It labels itself from the raw provider id rather than rendering blank.
        let unlabelled = CloudAccount { label: String::new(), ..account.clone() };
        assert_eq!(unlabelled.display_label(), "a-drive-from-the-future");
        // And it survives a write.
        let written = toml::to_string(&parsed).expect("serialize");
        let back: AccountFile = toml::from_str(&written).expect("parse");
        assert_eq!(back.accounts, parsed.accounts);
    }

    /// A file with keys this build does not know (a newer schema) must not fail the whole
    /// parse and take every account with it.
    #[test]
    fn unknown_keys_do_not_lose_the_file() {
        let text = "\
sync_interval_mins = 15

[[accounts]]
id = \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"
provider = \"dropbox\"
label = \"Personal\"
quota_bytes = 999
";
        let parsed: AccountFile = toml::from_str(text).expect("unknown keys are ignored");
        assert_eq!(parsed.accounts.len(), 1);
        assert_eq!(parsed.accounts[0].provider, "dropbox");
    }

    /// The label shown falls back through user label, then provider name, then the raw id,
    /// so a row is never blank.
    #[test]
    fn the_display_label_is_never_empty() {
        assert_eq!(sample().display_label(), "Work Drive");
        let no_label = CloudAccount { label: "   ".to_string(), ..sample() };
        assert_eq!(no_label.display_label(), "Google Drive");
    }
}

#[cfg(test)]
mod id_tests {
    use super::*;

    /// The id is what keys a keyring item, so it must be hex, long enough not to collide,
    /// and different every time.
    #[test]
    fn a_fresh_id_is_hex_and_unique() {
        let a = new_id();
        let b = new_id();
        assert_eq!(a.len(), 32, "128 bits as hex");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(a, b, "two ids from the OS CSPRNG must not match");
        // And it is accepted by the validator that guards the secrets file fallback.
        assert!(super::super::secrets::is_valid_account_id(&a));
    }
}

#[cfg(test)]
mod ordering_tests {
    use super::*;

    fn acct(provider: &str, label: &str, id: &str) -> CloudAccount {
        CloudAccount {
            id: id.to_string(),
            provider: provider.to_string(),
            label: label.to_string(),
            folder_id: None,
            folder_name: None,
            added_at: "2026-08-02T10:00:00+00:00".to_string(),
            visibility: None,
            destination: None,
        }
    }

    fn names(accounts: &[CloudAccount]) -> Vec<String> {
        accounts.iter().map(|a| format!("{}/{}", a.brand_name(), a.display_label())).collect()
    }

    /// **The whole of the ordering ask** (DRAGON-495): brand first, then the user's own name
    /// within a brand. The file's own order (the order they happened to be connected in) is
    /// what this replaces, so the input here is deliberately shuffled against the answer.
    #[test]
    fn accounts_sort_by_brand_then_by_name() {
        let listed = vec![
            acct("onedrive", "Work", "44"),
            acct("gdrive", "Personal", "22"),
            acct("dropbox", "Team", "11"),
            acct("gdrive", "Archive", "33"),
        ];
        assert_eq!(
            names(&sorted(listed)),
            vec![
                "Dropbox/Team".to_string(),
                "Google Drive/Archive".to_string(),
                "Google Drive/Personal".to_string(),
                "OneDrive/Work".to_string(),
            ]
        );
    }

    /// Case never decides the order: "work" and "Work" are the same word to a reader, and a
    /// byte comparison would file every capital ahead of every lowercase letter.
    #[rstest::rstest]
    #[case("apple", "Banana", std::cmp::Ordering::Less)]
    #[case("Apple", "banana", std::cmp::Ordering::Less)]
    #[case("ZEBRA", "apple", std::cmp::Ordering::Greater)]
    #[case("work", "Work", std::cmp::Ordering::Equal)]
    fn names_compare_case_insensitively(
        #[case] left: &str,
        #[case] right: &str,
        #[case] expected: std::cmp::Ordering,
    ) {
        let ordering = compare(&acct("gdrive", left, "11"), &acct("gdrive", right, "11"));
        assert_eq!(ordering, expected, "{left} vs {right}");
    }

    /// A TIE on both keys still has exactly one answer, decided by the id: without it the
    /// order of two identically named accounts would depend on the sort's stability and so on
    /// the file's own order, which is what this ordering exists to stop mattering.
    #[test]
    fn an_identical_pair_is_broken_by_id_not_by_luck() {
        let first = acct("gdrive", "Work", "aa");
        let second = acct("gdrive", "work", "bb");
        assert_eq!(compare(&first, &second), std::cmp::Ordering::Less);
        assert_eq!(compare(&second, &first), std::cmp::Ordering::Greater);
        // Same id as well as same name: the one case that is genuinely equal.
        assert_eq!(compare(&first, &first), std::cmp::Ordering::Equal);
        // And the result does not depend on the order they arrive in.
        let one = sorted(vec![first.clone(), second.clone()]);
        let other = sorted(vec![second, first]);
        assert_eq!(one, other);
    }

    /// An account whose provider this build does not know still sorts, under its raw provider
    /// id: it is KEPT in the list (see the module doc), so it has to have a place in it.
    #[test]
    fn an_unknown_provider_sorts_under_its_own_id() {
        let unknown = acct("aaa-unknown-drive", "Mine", "11");
        assert_eq!(unknown.brand_name(), "aaa-unknown-drive");
        let ordered = sorted(vec![acct("gdrive", "Work", "22"), unknown]);
        // "aaa-unknown-drive" sorts before "Google Drive", by the same case-folded rule.
        assert_eq!(names(&ordered)[0], "aaa-unknown-drive/Mine");
    }

    /// An account with no label of its own falls back to its provider's name in BOTH the
    /// display and the sort, so it files with its brand rather than at the top under "".
    #[test]
    fn an_unlabelled_account_sorts_under_its_provider_name() {
        let unlabelled = acct("onedrive", "", "11");
        assert_eq!(unlabelled.display_label(), "OneDrive");
        let ordered = sorted(vec![unlabelled, acct("dropbox", "Zed", "22")]);
        assert_eq!(names(&ordered), vec!["Dropbox/Zed".to_string(), "OneDrive/OneDrive".to_string()]);
    }
}
