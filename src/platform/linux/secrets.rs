//! Linux keyring backend: the Secret Service (DRAGON-482).
//!
//! `org.freedesktop.secrets` over the session bus, which is what gnome-keyring,
//! KWallet's secrets bridge and `keepassxc` all provide. Hand-written blocking calls on
//! the zbus 5 connection this crate already depends on, in the same shape
//! `share/notify.rs` uses, rather than a keyring crate: the interface is four methods, and
//! a new dependency for four methods is not worth the lock churn.
//!
//! **This module makes no decisions.** The item naming, the account-id validation and the
//! fallback rule all live in `cloud::secrets`, compiled and unit-tested on every host
//! (CLAUDE.md's "put the DECISION in the shared tree, put the SYSCALL in the plugin"). What
//! is here is D-Bus and nothing else, which is also why it has no unit tests: every line of
//! it needs a session bus and a running secrets provider.
//!
//! # What it does when there is no keyring
//!
//! It returns an `Err` naming the reason, and the caller falls back to the file backend.
//! That is a supported configuration, not a failure: a headless session, a minimal WM, or
//! a user who declines to unlock their keyring are all normal.
//!
//! # Prompts are not driven
//!
//! A locked collection answers `Unlock` with a PROMPT object, which is a whole interactive
//! protocol (show the prompt, wait for its `Completed` signal, with a window handle we do
//! not have in a helper process). We do not drive it. A prompt means "this needs the user,
//! and we cannot ask right now", so we report it and let the fallback take over. Driving
//! prompts is a decision for whoever wants a keyring-unlock UX, not something to bolt on
//! here.
//!
//! # Everything is bounded
//!
//! zbus's blocking calls wait for a reply indefinitely, and a wedged secrets daemon would
//! then wedge us. Each operation therefore runs on its own thread against
//! [`SECRET_SERVICE_BUDGET`]. A timeout is reported like any other failure, so a keyring
//! that never answers degrades to the file backend instead of hanging a capture.

// Honestly dead until the cloud-accounts feature is wired (DRAGON-482 stage A1): the only
// caller of this backend is `cloud::secrets`, whose own callers arrive with the OAuth flow
// (A2) and the upload engine (A3). Scoped to this file; delete it once a real path stores a
// token. `not(test)` rather than a bare allow, so the module is still checked by any test
// build that reaches it.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashMap;
use std::time::Duration;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// The bus name, object path and interfaces of the Secret Service.
const DEST: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_IFACE: &str = "org.freedesktop.Secret.Service";
const COLLECTION_IFACE: &str = "org.freedesktop.Secret.Collection";
const ITEM_IFACE: &str = "org.freedesktop.Secret.Item";
/// The user's default collection, by its well-known alias.
const DEFAULT_COLLECTION: &str = "/org/freedesktop/secrets/aliases/default";

/// The `xdg:schema` attribute every well-behaved client sets, so a keyring app can group
/// and label our items instead of showing them as unknown blobs.
const SCHEMA: &str = "org.freedesktop.Secret.Generic";

/// How long any one Secret Service operation may take before we give up on it and let the
/// caller fall back. Generous enough for a keyring daemon that has to start up, short
/// enough that a wedged one never stalls a capture.
const SECRET_SERVICE_BUDGET: Duration = Duration::from_secs(5);

/// Store `secret` under (`service`, `account`), replacing any existing item.
pub fn store(service: &str, account: &str, secret: &str) -> Result<(), String> {
    let (service, account, secret) = (service.to_string(), account.to_string(), secret.to_string());
    with_budget("store", move || store_blocking(&service, &account, &secret))
}

/// Read the secret stored under (`service`, `account`). `Ok(None)` = no such item.
pub fn load(service: &str, account: &str) -> Result<Option<String>, String> {
    let (service, account) = (service.to_string(), account.to_string());
    with_budget("read", move || load_blocking(&service, &account))
}

/// Remove the item under (`service`, `account`). A missing item is success.
pub fn delete(service: &str, account: &str) -> Result<(), String> {
    let (service, account) = (service.to_string(), account.to_string());
    with_budget("delete", move || delete_blocking(&service, &account))
}

/// Run one Secret Service operation on its own thread, bounded by
/// [`SECRET_SERVICE_BUDGET`].
///
/// The thread is DETACHED on timeout rather than joined: it is blocked inside zbus and
/// cannot be interrupted, so waiting for it is exactly the hang this exists to prevent. It
/// holds only its own connection and ends when the daemon eventually answers or the
/// process exits.
fn with_budget<T: Send + 'static>(
    what: &'static str,
    op: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name(format!("cck-secrets-{what}"))
        .spawn(move || {
            let _ = tx.send(op());
        })
        .map_err(|e| format!("the keyring thread could not be started: {e}"))?;
    rx.recv_timeout(SECRET_SERVICE_BUDGET)
        .map_err(|_| format!("the keyring did not answer the {what} within {SECRET_SERVICE_BUDGET:?}"))?
}

/// The lookup attributes for one item. These ARE the item's identity to the Secret
/// Service: a search matches on them exactly, so they must be built in one place.
fn attributes(service: &str, account: &str) -> HashMap<String, String> {
    HashMap::from([
        ("service".to_string(), service.to_string()),
        ("account".to_string(), account.to_string()),
        ("xdg:schema".to_string(), SCHEMA.to_string()),
    ])
}

/// A session bus connection plus a `plain` session.
///
/// The `plain` algorithm sends the secret unencrypted over the bus, which is the same
/// trust boundary as the rest of this app's D-Bus traffic: the session bus is a per-user
/// socket. The alternative (`dh-ietf1024-sha256-aes128-cbc-pkcs7`) buys protection only
/// against something already able to read that socket, which by then can read the
/// clipboard and screenshot the desktop too.
fn open_session() -> Result<(zbus::blocking::Connection, OwnedObjectPath), String> {
    let conn = zbus::blocking::Connection::session()
        .map_err(|e| format!("no session bus: {e}"))?;
    let reply = conn
        .call_method(
            Some(DEST),
            SERVICE_PATH,
            Some(SERVICE_IFACE),
            "OpenSession",
            &("plain", Value::new("")),
        )
        .map_err(|e| format!("the secret service did not open a session: {e}"))?;
    let (_output, session): (OwnedValue, OwnedObjectPath) = reply
        .body()
        .deserialize()
        .map_err(|e| format!("the secret service returned an unreadable session: {e}"))?;
    Ok((conn, session))
}

/// `Service.SearchItems` -> (unlocked, locked).
fn search(
    conn: &zbus::blocking::Connection,
    service: &str,
    account: &str,
) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>), String> {
    let reply = conn
        .call_method(
            Some(DEST),
            SERVICE_PATH,
            Some(SERVICE_IFACE),
            "SearchItems",
            &attributes(service, account),
        )
        .map_err(|e| format!("the secret service could not be searched: {e}"))?;
    reply
        .body()
        .deserialize()
        .map_err(|e| format!("the secret service returned an unreadable search result: {e}"))
}

/// Whether an object path is the D-Bus "no object" sentinel, which is how the Secret
/// Service says "no prompt is needed".
fn is_no_prompt(path: &OwnedObjectPath) -> bool {
    path.as_str() == "/"
}

fn store_blocking(service: &str, account: &str, secret: &str) -> Result<(), String> {
    let (conn, session) = open_session()?;
    let label = format!("{service}: {account}");
    let props: HashMap<&str, Value> = HashMap::from([
        ("org.freedesktop.Secret.Item.Label", Value::new(label)),
        (
            "org.freedesktop.Secret.Item.Attributes",
            Value::new(attributes(service, account)),
        ),
    ]);
    // The Secret struct is (session, parameters, value, content_type). `plain` takes no
    // parameters, so that field is empty.
    let secret_struct = (
        session,
        Vec::<u8>::new(),
        secret.as_bytes().to_vec(),
        "text/plain",
    );
    let reply = conn
        .call_method(
            Some(DEST),
            DEFAULT_COLLECTION,
            Some(COLLECTION_IFACE),
            "CreateItem",
            // replace = true: re-connecting an account must overwrite its tokens, never
            // leave a second item that a later search could return instead.
            &(props, secret_struct, true),
        )
        .map_err(|e| format!("the secret service refused to store the item: {e}"))?;
    let (item, prompt): (OwnedObjectPath, OwnedObjectPath) = reply
        .body()
        .deserialize()
        .map_err(|e| format!("the secret service returned an unreadable result: {e}"))?;
    if !is_no_prompt(&prompt) || item.as_str() == "/" {
        // A prompt here means the collection is locked. See the module doc: we report it
        // rather than driving it, and the caller falls back.
        return Err("the keyring is locked and would need to be unlocked first".to_string());
    }
    Ok(())
}

fn load_blocking(service: &str, account: &str) -> Result<Option<String>, String> {
    let (conn, session) = open_session()?;
    let (unlocked, locked) = search(&conn, service, account)?;
    let Some(item) = unlocked.first().or_else(|| locked.first()) else {
        return Ok(None);
    };
    if unlocked.is_empty() {
        // Found, but behind a lock. One attempt to unlock without a prompt (some providers
        // unlock silently); anything that wants the user is reported.
        let reply = conn
            .call_method(
                Some(DEST),
                SERVICE_PATH,
                Some(SERVICE_IFACE),
                "Unlock",
                &vec![item.clone()],
            )
            .map_err(|e| format!("the keyring could not be unlocked: {e}"))?;
        let (opened, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = reply
            .body()
            .deserialize()
            .map_err(|e| format!("the keyring returned an unreadable unlock result: {e}"))?;
        if opened.is_empty() || !is_no_prompt(&prompt) {
            return Err("the keyring is locked and would need to be unlocked first".to_string());
        }
    }
    let reply = conn
        .call_method(
            Some(DEST),
            item.as_str(),
            Some(ITEM_IFACE),
            "GetSecret",
            &session,
        )
        .map_err(|e| format!("the stored item could not be read: {e}"))?;
    let (_session, _params, value, _content_type): (
        OwnedObjectPath,
        Vec<u8>,
        Vec<u8>,
        String,
    ) = reply
        .body()
        .deserialize()
        .map_err(|e| format!("the stored item was unreadable: {e}"))?;
    String::from_utf8(value)
        .map(Some)
        .map_err(|_| "the stored item was not valid text".to_string())
}

fn delete_blocking(service: &str, account: &str) -> Result<(), String> {
    let conn = zbus::blocking::Connection::session()
        .map_err(|e| format!("no session bus: {e}"))?;
    let (unlocked, locked) = search(&conn, service, account)?;
    // Both lists: a token that exists in a locked collection is still a token, and leaving
    // it behind on a disconnect is the failure this loop exists to avoid.
    for item in unlocked.into_iter().chain(locked) {
        conn.call_method(Some(DEST), item.as_str(), Some(ITEM_IFACE), "Delete", &())
            .map_err(|e| format!("a stored item could not be removed: {e}"))?;
    }
    Ok(())
}
