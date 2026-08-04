//! Where a connected account's TOKENS live (DRAGON-482).
//!
//! One portable seam, [`store`] / [`load`] / [`delete`], over four backends:
//!
//! | Platform | Backend | Body |
//! |---|---|---|
//! | Linux | Secret Service (`org.freedesktop.secrets`) over D-Bus | `platform/linux/secrets.rs` |
//! | macOS | Keychain generic password (SecItem) | `platform/mac/services/secrets.rs` |
//! | Windows | Credential Manager (`CredWriteW`) | `platform/windows/secrets.rs` |
//! | anywhere | a 0600 file under the app config dir | here, [`file_store`] and friends |
//!
//! The file backend is not only the fallback for a platform we have no keyring for. It is
//! also what a Linux box with no Secret Service provider running gets (a minimal WM, a
//! headless session, a locked keyring the user will not unlock), which is a real
//! configuration and not an error. So a keyring failure DEGRADES to the file rather than
//! failing the connect, with a warning that says which backend answered.
//!
//! # Decision here, syscall in the plugin
//!
//! The house split (CLAUDE.md). Everything that DECIDES lives in this file, compiled and
//! unit-tested on every host: the keyring item naming ([`item_service`], [`item_account`]),
//! the account-id validation that keeps a crafted id out of the filesystem
//! ([`is_valid_account_id`]), and the file name ([`secret_file_name`]). The three platform
//! modules do nothing but call their OS. That is what makes the macOS and Windows backends
//! reviewable from a Linux machine, which is where this was written.
//!
//! # Privacy
//!
//! No function here logs a secret, and none logs a path except through
//! [`crate::diag::path_shape`]. The account id IS logged: it is random hex minted by
//! [`super::accounts::new_id`] and identifies nothing about the user, and without it a
//! multi-account failure is undiagnosable.

/// The keyring SERVICE name every backend files its items under, and what the user sees in
/// their keyring app. One constant so the three platforms cannot drift: an item written by
/// the mac backend and looked up by a differently-spelled service name is a token silently
/// lost.
pub const ITEM_SERVICE: &str = "Cosmic Capture Kit";

/// The keyring service name. A function rather than a bare `const` reference at each call
/// site so the platform modules all reach it the same way.
pub fn item_service() -> &'static str {
    ITEM_SERVICE
}

/// The keyring ACCOUNT (item) name for a cloud account. Pure; unit-tested.
///
/// Prefixed rather than the bare id, because this app may keep other kinds of secret later
/// under the same service, and an unprefixed random-hex item would be indistinguishable
/// from them. The prefix is part of the on-disk contract: changing it orphans every token
/// already stored.
pub fn item_account(account_id: &str) -> String {
    format!("cloud:{account_id}")
}

/// Whether `id` is a well-formed account id. Pure; unit-tested.
///
/// Lowercase hex, 8 to 64 characters, which is exactly what [`super::accounts::new_id`]
/// mints. This is a SECURITY check, not a tidiness one: [`secret_file_name`] builds a file
/// name from the id, so an id containing `/` or `..` would let a crafted (or corrupted)
/// accounts file steer a write outside the app config directory. Rejecting the id is what
/// makes the file backend safe to build by concatenation.
pub fn is_valid_account_id(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// The file-backend file name for an account. Pure; unit-tested.
///
/// `None` for an id [`is_valid_account_id`] refuses, so a caller cannot get a path at all
/// for a bad id rather than getting one and having to remember to check.
pub fn secret_file_name(account_id: &str) -> Option<String> {
    is_valid_account_id(account_id).then(|| format!("{account_id}.json"))
}

/// The directory the file backend writes into: `cloud-secrets/` under the app config dir.
///
/// Its own directory rather than loose files beside `config.toml`, so the whole fallback
/// store can be found, backed up, or wiped as one thing.
pub fn file_dir() -> Option<std::path::PathBuf> {
    Some(crate::util::app_config_dir()?.join("cloud-secrets"))
}

/// The file backend's path for an account, or `None` when the id is not well-formed.
fn file_path(account_id: &str) -> Option<std::path::PathBuf> {
    Some(file_dir()?.join(secret_file_name(account_id)?))
}

/// Whether `text` is a JSON OBJECT, which is what a stored secret always is. Pure;
/// unit-tested.
///
/// The stored value is a token SET (access token, refresh token, expiry, scope), so it is
/// an object and never a bare string or an array. Checking that at the door is worth the
/// line: a caller that stores a raw token by mistake would only find out much later, when a
/// refresh tried to parse it back and failed with the account already "connected". Parsed
/// with `serde_json` rather than sniffed for a leading `{`, because "is this really JSON"
/// is exactly the question a sniff cannot answer.
pub fn is_json_object(text: &str) -> bool {
    matches!(
        serde_json::from_str::<serde_json::Value>(text),
        Ok(serde_json::Value::Object(_))
    )
}

/// Which backend answered. Logged (never the secret), and the settings page will show it
/// so a user can tell "in your keyring" from "in a file in your config folder".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// The platform keyring.
    Keyring,
    /// The 0600 file fallback.
    File,
}

impl Backend {
    /// A short name for a log line or a settings row.
    pub fn label(self) -> &'static str {
        match self {
            Backend::Keyring => "system keyring",
            Backend::File => "config folder file",
        }
    }
}

/// Store `secret_json` for `account_id`, replacing anything already there.
///
/// Tries the platform keyring first and falls back to the file on ANY keyring failure. The
/// fallback is logged at `warn` with the reason, because a user whose tokens quietly moved
/// out of their keyring deserves to be able to find out why.
pub fn store(account_id: &str, secret_json: &str) -> Result<(), String> {
    if !is_valid_account_id(account_id) {
        return Err("That cloud account has an invalid id.".to_string());
    }
    if !is_json_object(secret_json) {
        // Deliberately says nothing about the value itself, not even its length: this
        // message can reach a log.
        return Err("That cloud account's sign-in details are not in the expected form.".to_string());
    }
    match keyring_store(account_id, secret_json) {
        Ok(()) => {
            log::debug!("cloud secrets: stored {account_id} in the {}", Backend::Keyring.label());
            // A previous run may have fallen back to the file. Now that the keyring has it,
            // the file copy is a second place the token lives, so it goes.
            let _ = file_delete(account_id);
            Ok(())
        }
        Err(e) => {
            log::warn!(
                "cloud secrets: the system keyring refused to store {account_id} ({e}); \
                 falling back to a file in the config folder"
            );
            file_store(account_id, secret_json)
        }
    }
}

/// Load the stored secret for `account_id`.
///
/// `Ok(None)` means nothing is stored, which is an ordinary answer (a disconnected
/// account, a fresh install with an accounts file restored from a backup). `Err` means a
/// backend failed in a way the caller should surface.
pub fn load(account_id: &str) -> Result<Option<String>, String> {
    if !is_valid_account_id(account_id) {
        return Err("That cloud account has an invalid id.".to_string());
    }
    match keyring_load(account_id) {
        Ok(Some(secret)) => Ok(Some(secret)),
        // Not in the keyring, or no keyring at all: the file is the other place it can be.
        Ok(None) => file_load(account_id),
        Err(e) => {
            log::warn!(
                "cloud secrets: the system keyring could not be read for {account_id} ({e}); \
                 trying the config folder file"
            );
            file_load(account_id)
        }
    }
}

/// Forget the secret for `account_id`, from BOTH backends.
///
/// Both, always, and not "whichever one has it": a token that moved between backends over
/// the life of an install can exist in both, and a disconnect that leaves one behind is a
/// credential the user believes is gone. Absence is success.
pub fn delete(account_id: &str) -> Result<(), String> {
    if !is_valid_account_id(account_id) {
        return Err("That cloud account has an invalid id.".to_string());
    }
    let keyring = keyring_delete(account_id);
    let file = file_delete(account_id);
    if let Err(e) = &keyring {
        log::warn!("cloud secrets: the system keyring could not drop {account_id} ({e})");
    }
    // The file half is the one this process fully controls, so its failure is the one that
    // is reported. A keyring that will not delete is warned about above and does not block
    // the disconnect, since the alternative is an account the user cannot remove.
    file
}

// ---------------------------------------------------------------------------
// The keyring backends: one portable fn per operation, branching by cfg, so no
// caller above carries a platform gate.
// ---------------------------------------------------------------------------

/// Store in the platform keyring.
fn keyring_store(account_id: &str, secret_json: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        crate::platform::linux::secrets::store(item_service(), &item_account(account_id), secret_json)
    }
    #[cfg(target_os = "macos")]
    {
        crate::platform::mac::secrets::store(item_service(), &item_account(account_id), secret_json)
    }
    #[cfg(windows)]
    {
        crate::platform::windows::secrets::store(item_service(), &item_account(account_id), secret_json)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (account_id, secret_json);
        Err("This platform has no keyring.".to_string())
    }
}

/// Read from the platform keyring. `Ok(None)` = no such item.
fn keyring_load(account_id: &str) -> Result<Option<String>, String> {
    #[cfg(target_os = "linux")]
    {
        crate::platform::linux::secrets::load(item_service(), &item_account(account_id))
    }
    #[cfg(target_os = "macos")]
    {
        crate::platform::mac::secrets::load(item_service(), &item_account(account_id))
    }
    #[cfg(windows)]
    {
        crate::platform::windows::secrets::load(item_service(), &item_account(account_id))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = account_id;
        Ok(None)
    }
}

/// Drop from the platform keyring. Absence is success.
fn keyring_delete(account_id: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        crate::platform::linux::secrets::delete(item_service(), &item_account(account_id))
    }
    #[cfg(target_os = "macos")]
    {
        crate::platform::mac::secrets::delete(item_service(), &item_account(account_id))
    }
    #[cfg(windows)]
    {
        crate::platform::windows::secrets::delete(item_service(), &item_account(account_id))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = account_id;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The file backend. Portable, and the honest last resort.
// ---------------------------------------------------------------------------

/// Create `cloud-secrets/` and make sure it is owner-only.
///
/// The MODE matters as much as the files inside it: a directory another user can list tells
/// them which accounts exist and, more usefully, gives them somewhere to plant a symlink that
/// [`file_store`]'s write would follow. `create_dir_all` cannot set a mode, so the mode is
/// applied afterwards, and it is applied on EVERY store rather than only at creation: a
/// restore from a backup archive, or a copy between machines, routinely flattens directory
/// modes to the umask default, and a directory that was 0700 last week is not evidence about
/// today.
///
/// Unix only. On Windows the app config directory already lives under the per-user profile,
/// whose default ACL is not world-readable, and there is no portable mode to set.
fn ensure_secret_dir(dir: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("The cloud secret folder could not be created: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Write the secret to `cloud-secrets/<id>.json`, owner-read-write only.
///
/// # Why a temp file and a rename
///
/// Three properties at once, none of which a straight open-and-truncate has:
///
/// * **The mode is never wrong, even for a moment.** The temp file is created with
///   `O_EXCL` and mode 0600, so the bytes are written into a file no other user could ever
///   open. Truncating the DESTINATION in place would inherit whatever mode it already had,
///   which is the bug this replaced: a config folder restored from a backup archive comes back
///   0644, and the next token refresh would have written a fresh token into a world-readable
///   file and left it that way.
/// * **A crash cannot leave half a token.** `rename` is atomic within a directory, so a reader
///   sees either the old secret or the new one.
/// * **A planted symlink cannot be written through.** `create_new` refuses an existing path of
///   any kind, and `rename` replaces the destination rather than following it.
///
/// The mode is re-asserted on the destination after the rename. That is belt and braces for a
/// filesystem that does not honour the creation mode (a mode-mangling mount), and it costs one
/// syscall on a path taken once per token refresh.
fn file_store(account_id: &str, secret_json: &str) -> Result<(), String> {
    use std::io::Write as _;
    let path = file_path(account_id)
        .ok_or_else(|| "The cloud secret could not be stored: no config folder.".to_string())?;
    if let Some(dir) = path.parent() {
        ensure_secret_dir(dir)?;
    }
    // Named by pid so two processes refreshing the same account cannot share a temp file; the
    // rename is what makes the last writer win cleanly rather than the two interleaving.
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts.open(&temp).map_err(|e| {
        format!(
            "The cloud secret could not be written ({}): {e}",
            crate::diag::path_shape(&temp)
        )
    })?;
    let written = f
        .write_all(secret_json.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("The cloud secret could not be written: {e}"));
    drop(f);
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&temp, &path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!(
            "The cloud secret could not be saved ({}): {e}",
            crate::diag::path_shape(&path)
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    log::debug!("cloud secrets: stored {account_id} in the {}", Backend::File.label());
    Ok(())
}

/// Read the secret file, if there is one. A missing file is `Ok(None)`.
fn file_load(account_id: &str) -> Result<Option<String>, String> {
    let Some(path) = file_path(account_id) else {
        return Err("That cloud account has an invalid id.".to_string());
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!(
            "The cloud secret could not be read ({}): {e}",
            crate::diag::path_shape(&path)
        )),
    }
}

/// Remove the secret file. A missing file is success.
fn file_delete(account_id: &str) -> Result<(), String> {
    let Some(path) = file_path(account_id) else {
        return Err("That cloud account has an invalid id.".to_string());
    };
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!(
            "The cloud secret could not be removed ({}): {e}",
            crate::diag::path_shape(&path)
        )),
    }
}

#[cfg(test)]
mod naming_tests {
    use super::*;

    /// The item naming is an on-disk contract shared by three platform backends. Pinned
    /// exactly, because a drift between them is a token written in one place and looked up
    /// in another, which fails as "your account is disconnected" and nothing else.
    #[test]
    fn the_keyring_item_naming_is_fixed() {
        assert_eq!(item_service(), "Cosmic Capture Kit");
        assert_eq!(item_account("0123456789abcdef"), "cloud:0123456789abcdef");
    }
}

#[cfg(test)]
mod account_id_tests {
    use super::*;

    /// The ids [`super::super::accounts::new_id`] actually mints are accepted.
    #[test]
    fn a_minted_id_is_valid() {
        assert!(is_valid_account_id("0123456789abcdef0123456789abcdef"));
        assert!(is_valid_account_id("00000000"));
    }

    /// **The security case.** The id becomes a FILE NAME, so anything that could steer a
    /// write out of the secrets directory has to be refused before a path is ever built.
    /// Traversal, separators, NUL, and the Windows drive form are all covered.
    #[test]
    fn an_id_that_could_escape_the_directory_is_refused() {
        for bad in [
            "../../etc/passwd",
            "..",
            "a/b",
            "a\\b",
            "C:windows",
            "abc\0def",
            "abcdefg.json",
            "ABCDEF0123456789", // uppercase is not what we mint, so it is not accepted
            "zzzzzzzz",         // outside hex
            "abc",              // too short to be a real id
            "",
        ] {
            assert!(!is_valid_account_id(bad), "{bad:?} must be refused");
            assert!(secret_file_name(bad).is_none(), "{bad:?} must not yield a file name");
        }
        // A 65-character id is refused too: the bound exists so a pathological length can
        // never reach the filesystem.
        assert!(!is_valid_account_id(&"a".repeat(65)));
        assert!(is_valid_account_id(&"a".repeat(64)));
    }

    /// A valid id yields exactly `<id>.json`, with no directory component of its own.
    #[test]
    fn a_valid_id_yields_a_flat_file_name() {
        let name = secret_file_name("0123456789abcdef").expect("valid");
        assert_eq!(name, "0123456789abcdef.json");
        assert!(!name.contains('/') && !name.contains('\\'));
    }

    /// The public seam rejects a bad id at every entry point, so no backend is ever asked
    /// about one.
    #[test]
    fn the_seam_refuses_a_bad_id_before_touching_a_backend() {
        assert!(store("../x", "{}").is_err());
        assert!(load("../x").is_err());
        assert!(delete("../x").is_err());
    }
}

#[cfg(test)]
mod secret_shape_tests {
    use super::*;

    /// A token SET is an object. Anything else is a caller mistake caught at the door.
    #[test]
    fn only_a_json_object_is_a_secret() {
        assert!(is_json_object("{}"));
        assert!(is_json_object(r#"{"access_token":"x","expires_in":3600}"#));
        assert!(is_json_object("  { \"a\": 1 }  "));
        for bad in [
            "",
            "not json",
            "\"a-bare-token\"",   // the mistake this catches
            "[{\"a\":1}]",        // an array of objects is not an object
            "123",
            "null",
            "{\"unterminated\": ",
        ] {
            assert!(!is_json_object(bad), "{bad:?} must not pass as a secret");
        }
    }

    /// The refusal must not describe the value: this message can reach a log.
    #[test]
    fn a_malformed_secret_is_refused_without_quoting_it() {
        let err = store("0123456789abcdef", "SECRET-TOKEN-VALUE")
            .expect_err("a bare token is not a secret set");
        assert!(!err.contains("SECRET"), "the refusal quoted the value: {err}");
    }
}

#[cfg(test)]
mod file_backend_tests {
    use super::*;

    /// The file backend round-trips, and delete is idempotent. Runs against the real
    /// filesystem, which is safe here: `util::is_dev_process` routes a cargo-run process's
    /// config dir into a sandbox (`tests/log_isolation.rs` proves it end to end), so this
    /// never touches the developer's own config folder.
    #[test]
    fn the_file_backend_round_trips_and_forgets() {
        // A fixed id per test, so parallel tests cannot collide on one file.
        let id = "00ff00ff00ff00ff00ff00ff00ff0001";
        let _ = file_delete(id);
        assert_eq!(file_load(id).expect("a missing secret is not an error"), None);
        file_store(id, "{\"access_token\":\"x\"}").expect("store");
        assert_eq!(file_load(id).expect("load"), Some("{\"access_token\":\"x\"}".to_string()));
        // A shorter secret must not leave the tail of a longer one behind.
        file_store(id, "{}").expect("overwrite");
        assert_eq!(file_load(id).expect("load"), Some("{}".to_string()));
        file_delete(id).expect("delete");
        assert_eq!(file_load(id).expect("load"), None);
        // Deleting again is success, not an error: absence is what was asked for.
        file_delete(id).expect("delete is idempotent");
    }

    /// **The permissions case.** A token file readable by other users on the machine is
    /// the whole failure this backend has to avoid, and the mode is set at OPEN so there
    /// is no window where it is not.
    #[cfg(unix)]
    #[test]
    fn the_secret_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let id = "00ff00ff00ff00ff00ff00ff00ff0002";
        let _ = file_delete(id);
        file_store(id, "{\"refresh_token\":\"x\"}").expect("store");
        let path = file_path(id).expect("a path for a valid id");
        let mode = std::fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the secret file must be owner read/write only");
        let _ = file_delete(id);
    }

    /// **The restored-from-backup case** (DRAGON-482). A config folder that came back out of
    /// a backup archive, or off a FAT stick, arrives 0644/0755. A refresh writing into that
    /// file in place would have left a working drive token world-readable, so the store
    /// replaces the file rather than truncating it, and re-asserts both modes.
    #[cfg(unix)]
    #[test]
    fn a_flattened_secret_file_and_folder_are_corrected() {
        use std::os::unix::fs::PermissionsExt as _;
        let id = "00ff00ff00ff00ff00ff00ff00ff0003";
        let _ = file_delete(id);
        file_store(id, "{\"access_token\":\"first\"}").expect("store");
        let path = file_path(id).expect("a path for a valid id");
        let dir = file_dir().expect("a secrets dir");
        // Flatten both, exactly as a restore would.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        file_store(id, "{\"access_token\":\"second\"}").expect("re-store");
        let mode = std::fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "a flattened secret file must be corrected");
        let dir_mode = std::fs::metadata(&dir).expect("metadata").permissions().mode();
        assert_eq!(dir_mode & 0o777, 0o700, "a flattened secrets folder must be corrected");
        // And the new value really landed: the rename replaced, it did not append.
        assert_eq!(file_load(id).expect("load"), Some("{\"access_token\":\"second\"}".to_string()));
        let _ = file_delete(id);
    }

    /// The write goes through a temp file, so nothing is left behind when it finishes. A
    /// stray `<id>.tmp-<pid>` beside the secret would be a second copy of the token.
    #[test]
    fn the_store_leaves_no_temp_file_behind() {
        let id = "00ff00ff00ff00ff00ff00ff00ff0004";
        let _ = file_delete(id);
        file_store(id, "{\"access_token\":\"x\"}").expect("store");
        let path = file_path(id).expect("a path for a valid id");
        let temp = path.with_extension(format!("tmp-{}", std::process::id()));
        assert!(!temp.exists(), "the temp file outlived the store");
        assert!(path.exists(), "the secret was not renamed into place");
        let _ = file_delete(id);
    }
}
