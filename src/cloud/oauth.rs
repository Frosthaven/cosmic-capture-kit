//! OAuth 2.0 authorization-code-with-PKCE flow (DRAGON-482, stage A2).
//!
//! Three jobs, in the order a connected account meets them:
//!
//! 1. **Connect.** [`connect_interactive_with`] runs the browser flow: a loopback listener on a
//!    random port, an authorization URL, the user's browser, one redirect carrying a code,
//!    and a code-for-token exchange.
//! 2. **Keep.** The result is a [`TokenSet`], serialized as the JSON object
//!    [`super::secrets::store`] already takes. Nothing else in `cloud/` has to know the
//!    shape.
//! 3. **Use.** [`ensure_fresh`] hands a caller a live access token, refreshing first when
//!    the stored one is close to expiring, and persisting the rotation BEFORE it returns.
//!
//! # A dead end: the device-code flow (DRAGON-490)
//!
//! This file used to also carry RFC 8628's device-code flow (a short code and a page to
//! type it into, for a machine whose browser is not usable) alongside the browser flow above.
//! It shipped, then the owner tried it against real accounts: Google's device flow needs a
//! separate "TVs and Limited Input devices" client REGISTRATION that this app never had (a
//! desktop client id polled without a secret is refused with `invalid_client`), so it never
//! worked; Microsoft's worked but was redundant once the browser flow's QR/link handling
//! already covers "sign in from another device" better, and Dropbox never had one. Rather than
//! keep unproven, dead code around, DRAGON-490 removed it wholesale: [`ConnectedTokens`] is now
//! reached only through [`connect_interactive_with`].
//!
//! # What makes this flow safe, concretely
//!
//! * **PKCE S256, always** ([`pkce_challenge`]). The verifier is 32 bytes from the OS
//!   CSPRNG. Without it, any local process that can win the race to the loopback port has an
//!   authorization code it can redeem, because a desktop app has no client secret to prove
//!   with.
//! * **The redirect listener binds `127.0.0.1:0`.** Loopback only, so nothing off this
//!   machine can reach it, and a kernel-assigned port so two connects (or two copies of the
//!   app) cannot collide.
//! * **`state` is random and checked** ([`check_state`]). A request arriving at our port
//!   with someone else's code is refused rather than exchanged.
//! * **Only the first request LINE is read**, bounded at [`MAX_REQUEST_LINE`] bytes, with a
//!   read timeout. This listener is a one-shot redirect catcher, not an HTTP server, and the
//!   less of an attacker-supplied request it parses the better.
//! * **The pages it serves are fixed content.** [`SUCCESS_PAGE`], [`DENIED_PAGE`] and
//!   [`NOT_FOUND_PAGE`] reflect NOTHING from the request: no code, no state, no path, no
//!   error string. A browser page that echoes its query is a reflected-XSS hole, and the
//!   page has nothing useful to say with that text anyway. `SUCCESS_PAGE`/`DENIED_PAGE` are
//!   assembled once, lazily, from other fixed sources (their own copy text plus the app's
//!   icon, embedded at compile time); nothing from a request ever reaches the assembly.
//! * **Every wait has a deadline.** The browser flow gets [`BROWSER_DEADLINE`], and every HTTP
//!   request its own budget through [`super::http::CurlReq`], which will not construct
//!   without one.
//!
//! # Privacy
//!
//! No token, code or verifier reaches a log line or argv. [`super::http`] guarantees the argv
//! half (secrets AND urls ride a stdin config); this file guarantees the log half by never
//! formatting a credential into a message, and by running the one string that could carry
//! one, the authorization URL, through [`crate::diag::redact_oauth`] first.
//! [`TokenSet`]'s `Debug` is hand-written for the same reason: a derived one would print
//! both tokens the moment anyone writes `{tokens:?}` while chasing an unrelated bug.
//!
//! `state` is the exception, and it is deliberate: it is a parameter OF the authorization URL,
//! so it is in the string this file logs (redacted) and it is in the URL handed to the user's
//! browser, where it is also in history. That is fine and is what the parameter is for. It is
//! not a credential; it is a nonce whose only job is that a reply quoting it came from the
//! request WE started ([`check_state`]). Knowing it lets nobody redeem anything, which is what
//! the PKCE verifier, the one value here that never leaves this process, is for.
//!
//! # Errors, and the one prefix that means something
//!
//! `Result<T, String>` per the house rule, written for a human. ONE of those strings is also
//! read by machine: a failure that means "the provider has forgotten this account, the user
//! must connect it again" starts with [`RECONNECT_PREFIX`], so the SETTINGS PAGE can offer a
//! Reconnect button instead of a shrug. Test it with [`needs_reconnect`] rather than matching
//! the literal.
//!
//! The settings page is the only surface that reads it, and that is not an oversight. A
//! transfer failure never reaches the editor's toast: an upload runs in a detached child
//! (`super::child`), which posts a desktop BANNER, and a banner has no Reconnect button to
//! offer. So that path strips the prefix with [`reconnect_reason`] instead, leaving the
//! sentence that follows it, which already tells the user to connect the account again.

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::http::{CurlReq, Method};
use super::{AuthKind, ProviderSpec};

// ---------------------------------------------------------------------------
// Budgets and tunables. Every one of them bounds a wait; see the module doc.
// ---------------------------------------------------------------------------

/// How long the browser flow waits for the user to finish signing in, by default.
///
/// Was two minutes; owner testing (DRAGON-489 follow-up) found that too short for a real
/// first-time setup, choosing among several Google accounts, a consent screen, 2FA, and
/// re-reading the app's own instructions mid-flow. Five minutes gives real headroom while
/// still not leaving a listener bound for the life of the process on a truly abandoned
/// connect. [`connect_interactive_with`] takes its own value, which is what the settings page
/// uses if it ever wants a different one; the settings page also shows a live countdown
/// against this exact constant (`cloud_browser_step`), so a change here changes what the
/// user sees, not just the underlying timeout.
pub const BROWSER_DEADLINE: Duration = Duration::from_secs(300);

/// Refresh an access token once it is within this many seconds of expiring.
///
/// Two minutes of margin, because the token has to survive the WHOLE request it is about to
/// authorize, and that request may be a multi-chunk upload. A token that expires mid-upload
/// fails the upload, not just the request that carried it.
pub const EXPIRY_MARGIN_SECS: i64 = 120;

/// The budget for a single token endpoint call (exchange or refresh).
const TOKEN_BUDGET: Duration = Duration::from_secs(30);

/// How long a redirect connection has to send its request line before we drop it.
const REDIRECT_READ_BUDGET: Duration = Duration::from_secs(10);

/// How often the accept loop wakes to check its deadline.
const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// The most of a request line this listener will read.
///
/// A redirect line is a few hundred bytes. Eight kilobytes is generous for a provider that
/// returns an unusually long code, and small enough that a local process cannot make us
/// allocate by talking to our port.
pub const MAX_REQUEST_LINE: usize = 8 * 1024;

/// The path the loopback redirect URI uses.
///
/// Bare `/`, because that is the shortest thing a provider's exact-match check can disagree
/// with us about. Providers that accept a loopback redirect accept an arbitrary port on it;
/// none of them require a path.
const REDIRECT_PATH: &str = "/";

/// The prefix on an error that means the account must be connected again.
///
/// The provider has forgotten this authorization: the refresh token was revoked, expired, or
/// invalidated by a password change. Nothing the app retries can fix it, so the UI should
/// offer Reconnect rather than Try again. Read it with [`needs_reconnect`].
///
/// It is deliberately a readable sentence opener rather than a machine tag, because the
/// SAME string is shown to the user. A message that says `reconnect-needed: …` in a toast
/// would be worse than one that reads as English and happens to be matchable.
pub const RECONNECT_PREFIX: &str = "Reconnect needed: ";

/// Whether an error string means "this account has to be connected again". Pure;
/// unit-tested.
pub fn needs_reconnect(message: &str) -> bool {
    message.starts_with(RECONNECT_PREFIX)
}

/// Build a reconnect-flagged message. Every site that decides an authorization is dead goes
/// through here, so the prefix is spelled exactly once. Pure; unit-tested.
///
/// `reason` continues the sentence the prefix opens, so it starts lowercase and ends with a
/// full stop.
pub fn reconnect_message(reason: &str) -> String {
    format!("{RECONNECT_PREFIX}{reason}")
}

/// The message with [`RECONNECT_PREFIX`] taken off, for a surface that cannot act on it.
/// Pure; unit-tested.
///
/// The prefix earns its place where a Reconnect BUTTON can appear next to it. On a desktop
/// banner there is no button, so the words are a label for an affordance that is not there;
/// what is left after them is a whole sentence that already says to connect the account again.
/// A message without the prefix comes back unchanged, so this is safe to apply to anything.
pub fn reconnect_reason(message: &str) -> &str {
    message.strip_prefix(RECONNECT_PREFIX).unwrap_or(message)
}

// ---------------------------------------------------------------------------
// The pages the loopback listener serves. Constants, reflecting nothing.
// ---------------------------------------------------------------------------

/// The app's own icon, embedded a second time for these pages (the first is `about.rs`'s
/// `APP_ICON`; that one feeds an iced `Handle`, this one feeds raw HTML text, so the two
/// cannot share one constant). Same file, same asset, so the browser landing page carries the
/// same brand mark the app shows everywhere else.
static APP_ICON_SVG: &str = include_str!("../../res/icons/dev.frosthaven.CosmicCaptureKit.svg");

/// [`APP_ICON_SVG`] with its XML declaration and DOCTYPE stripped off. Pure; unit-tested.
///
/// Those two lines are legal only at the very start of a STANDALONE SVG/XML document; inlined
/// into the middle of an HTML page's body (rather than referenced as a separate `.svg` file,
/// which this loopback page cannot do without a second server route) they are, at best, inert
/// noise a lenient browser skips, and are dropped here so the served markup is clean either way.
fn inline_app_icon_svg() -> &'static str {
    match APP_ICON_SVG.find("<svg") {
        Some(i) => &APP_ICON_SVG[i..],
        // Defensive only: the asset is compiled in and always contains an `<svg` tag. An empty
        // string here means the page renders with no icon rather than panicking on a launch
        // path a user is actively waiting on.
        None => "",
    }
}

/// The landing pages' background (DRAGON-495). Named constants rather than literals buried in
/// the format string, so the three colours can be read (and asserted on) as a palette.
const PAGE_BG: &str = "#1d1d1f";

/// The heading colour: near-white, the old light page's background.
const PAGE_FG: &str = "#f5f5f7";

/// The body line's colour, dimmer than the heading but still readable prose (~7:1 on
/// [`PAGE_BG`]), not a wash. The dark-page counterpart of the `#55555a` the light page used.
const PAGE_MUTED: &str = "#a1a1a6";

/// The CARD's fill (DRAGON-495): ONE step lighter than [`PAGE_BG`], which is how a dark-mode
/// surface says "this is a card" without a shadow. A dark card is lifted by being lighter than
/// what is behind it; the light-mode habit of dropping a shadow under it does nothing here
/// (a shadow on a near-black page is invisible), and the owner ruled one out explicitly.
const CARD_BG: &str = "#2a2a2e";

/// The card's hairline edge: one more step up again, so the card keeps a defined boundary on a
/// display where the 13-unit gap between [`PAGE_BG`] and [`CARD_BG`] washes out.
const CARD_EDGE: &str = "#3a3a3f";

/// The shared look every loopback landing page wears: the app icon, centered both ways on the
/// page, with a heading and a line of body text under it. Self-contained (no external CSS, JS,
/// font or image request: this page is served from a local socket with nothing on the internet
/// to fetch from), the common shape of a "you're done, close this tab" OAuth landing page.
/// Pure; unit-tested.
///
/// **Dark, unconditionally** (DRAGON-495, owner report). The first build was light
/// (`#1d1d1f` text on an `#f5f5f7` page); this app is a dark-desktop tool and a white page
/// mid-flow reads as somebody else's page. The palette is that same pair INVERTED, so the page
/// stays in the family it started in: [`PAGE_BG`] behind [`PAGE_FG`], with [`PAGE_MUTED`] for
/// the body line under the heading (Apple's own secondary grey on dark, ~7:1 against the
/// background, so the sentence someone actually reads is never a wash). The app icon's own
/// marks are pastel (light blue, yellow, pink, cyan, violet) and were already carrying
/// themselves against a light page; on this one they gain contrast rather than lose it, so the
/// asset is used as-is.
///
/// `color-scheme:dark` rides along so the BROWSER's own chrome (scrollbar, form controls, the
/// paint before our CSS lands) is dark too. Without it a dark page still flashes a white
/// scrollbar gutter and a white first frame, which is most of what the owner would see on a
/// page this short-lived.
///
/// The inlined SVG carries its OWN `width="100%" height="100%"` (copied verbatim from the
/// asset, see [`inline_app_icon_svg`]), which is exactly why it sits inside a `<div class=icon>`
/// with a fixed pixel box rather than being sized by a CSS rule targeting the `<svg>` element
/// directly: a raw `<svg>` tag with no `class` of its own would not match any such rule, and
/// would then fill 100% of whatever the flex layout gives it, which is not a small icon.
fn landing_page(title: &str, heading: &str, lines: &[&str]) -> String {
    // One `<p>` per sentence (DRAGON-495, owner request): the two sentences say different
    // things (what happened, and what to do now), and as one wrapped paragraph on a page with
    // nothing else on it they read as a block to skim past. `.lines` keeps its own gap; the
    // body's flex `gap` would otherwise space them like separate sections.
    let body: String = lines.iter().map(|line| format!("<p>{line}</p>")).collect();
    format!(
        "<!doctype html><html lang=\"en\"><head>\
<meta charset=\"utf-8\"><title>{title}</title>\
<style>\
html{{color-scheme:dark;background:{PAGE_BG}}}\
html,body{{height:100%;margin:0}}\
body{{box-sizing:border-box;display:flex;align-items:center;justify-content:center;\
min-height:100vh;padding:32px;text-align:center;\
font-family:-apple-system,BlinkMacSystemFont,\"Segoe UI\",Roboto,Helvetica,Arial,sans-serif;\
background:{PAGE_BG};color:{PAGE_FG}}}\
.card{{display:flex;flex-direction:column;align-items:center;gap:20px;\
box-sizing:border-box;max-width:32rem;padding:48px 40px;border-radius:18px;\
background:{CARD_BG};border:1px solid {CARD_EDGE};box-shadow:none}}\
.icon{{width:96px;height:96px}}\
h1{{margin:0;font-size:1.375rem;font-weight:600}}\
.lines{{display:flex;flex-direction:column;gap:8px;align-items:center}}\
p{{margin:0;max-width:28rem;line-height:1.5;color:{PAGE_MUTED};font-size:0.95rem}}\
</style></head><body><div class=\"card\"><div class=\"icon\">{icon}</div><h1>{heading}</h1>\
<div class=\"lines\">{body}</div></div>\
</body></html>",
        icon = inline_app_icon_svg(),
    )
}

/// Shown in the browser when the redirect carried a usable authorization code.
///
/// Assembled once, lazily, the first time either page is served (in practice, once per
/// process): the copy and the icon are both fixed, so there is nothing to gain from
/// recomputing it per request, and a `LazyLock` is what lets this stay a plain top-level value
/// callers pass around exactly like a `&str` constant, `SUCCESS_PAGE.as_str()` (or a `&` for
/// deref coercion) rather than a function every call site has to remember to invoke.
/// The copy is ONE SENTENCE PER LINE (DRAGON-495): what happened, then what to do next. See
/// [`landing_page`].
pub static SUCCESS_PAGE: LazyLock<String> = LazyLock::new(|| {
    landing_page(
        "Connected",
        "Connected",
        &[
            "Cosmic Capture Kit has the permission it asked for.",
            "You can close this tab and go back to the app.",
        ],
    )
});

/// Shown when the user declined, or when the redirect was not one we can use.
pub static DENIED_PAGE: LazyLock<String> = LazyLock::new(|| {
    landing_page(
        "Not connected",
        "Not connected",
        &["Nothing was connected.", "You can close this tab and try again from the app."],
    )
});

/// Served to anything hitting the port that is not the redirect: a browser asking for a
/// favicon, a prefetch, a port scanner. It says nothing about what the port is for. Left as a
/// plain literal (unlike the two above): this page is never meant to be looked at, so it does
/// not carry the app's icon or the shared landing-page styling.
pub const NOT_FOUND_PAGE: &str = "<!doctype html><html lang=\"en\"><head>\
<meta charset=\"utf-8\"><title>Not found</title></head><body><p>Not found.</p>\
</body></html>";

// ---------------------------------------------------------------------------
// The token model.
// ---------------------------------------------------------------------------

/// What a connected account's secret record holds, and the JSON object stored through
/// [`super::secrets::store`].
///
/// Serde field names ARE the on-disk contract: a rename orphans every token already stored,
/// exactly like a provider id. `expires_at` is absolute RFC 3339 rather than the `expires_in`
/// seconds the provider returns, because a duration is only meaningful next to the instant it
/// was received, and that instant is not in the file.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenSet {
    /// The bearer token API calls carry.
    pub access_token: String,
    /// The long-lived token a refresh spends. `None` for a provider or a grant that did not
    /// issue one, which means the account can be used until the access token dies and then
    /// has to be connected again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// When [`Self::access_token`] stops working, RFC 3339. `None` means the provider did
    /// not say, which [`needs_refresh`] treats as "no expiry we can plan around".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// The scopes the provider actually GRANTED, which can be narrower than the ones asked
    /// for. Kept so a later stage can tell "the upload failed" from "you never granted us
    /// write access".
    #[serde(default)]
    pub scopes: Vec<String>,
    /// The token type, in practice always `Bearer`. Stored rather than assumed, because the
    /// `Authorization` header is built from it.
    #[serde(default)]
    pub token_type: String,
}

/// Hand-written so a token cannot reach a log through `{:?}`.
///
/// A derived `Debug` prints both credentials in full. That is one careless `dbg!` away from
/// a customer's drive token sitting in a debug log we ask them to mail us, so the derive is
/// not used and this prints only the SHAPE: whether each token is present, and the
/// non-secret fields.
impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &redacted_shape(!self.access_token.is_empty()))
            .field("refresh_token", &redacted_shape(self.refresh_token.is_some()))
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .field("token_type", &self.token_type)
            .finish()
    }
}

/// What [`TokenSet`]'s `Debug` prints in place of a credential. Pure; unit-tested.
///
/// A SHAPE, never a length: "the refresh token is 512 characters" is a fact about the
/// credential, and this string exists precisely so no fact about the credential is printed.
fn redacted_shape(present: bool) -> &'static str {
    if present { "<redacted>" } else { "<none>" }
}

impl TokenSet {
    /// The JSON object [`super::secrets::store`] takes.
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self)
            .map_err(|e| format!("The sign-in details could not be prepared for storage: {e}"))
    }

    /// Read a stored secret back. Pure; unit-tested.
    ///
    /// The failure message says nothing about the VALUE, only that it could not be read: it
    /// can reach a log, and a parse error that quotes its input would quote a token.
    pub fn from_json(text: &str) -> Result<TokenSet, String> {
        serde_json::from_str(text)
            .map_err(|_| reconnect_message("this account's stored sign-in details could not be read."))
    }

}

/// The result of a successful connect, handed back to the UI stage that started it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedTokens {
    /// The [`ProviderSpec::id`] that was connected.
    pub provider: String,
    /// The tokens to persist, through [`super::secrets::store`].
    pub tokens: TokenSet,
}

// ---------------------------------------------------------------------------
// PKCE, randomness and encoding. All pure except the CSPRNG read.
// ---------------------------------------------------------------------------

/// Base64url without padding, RFC 4648 §5. Pure; unit-tested.
///
/// Hand-written rather than a new dependency: this is the only base64 in the app, it is
/// twenty lines, and the alternative is pulling a crate into a build that otherwise has no
/// use for one. The URL alphabet (`-` and `_`) and the dropped padding are what RFC 7636
/// requires of a code challenge.
pub fn base64url_nopad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[n as usize & 63] as char);
        }
    }
    out
}

/// The S256 code challenge for a verifier. Pure; unit-tested against RFC 7636's own vector.
///
/// `BASE64URL(SHA256(ASCII(verifier)))`. SHA-256 comes from the `sha2` crate rather than
/// being hand-rolled: a hash written for one call site is a hash nobody reviews.
pub fn pkce_challenge(verifier: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    base64url_nopad(&hasher.finalize())
}

/// `n` bytes from the OS CSPRNG as base64url text.
///
/// Used for the PKCE verifier (32 bytes, 43 characters, the length RFC 7636 asks for) and
/// for `state` (16 bytes). A failure here is refused rather than papered over with a weaker
/// source, the same call [`super::accounts::new_id`] makes.
fn random_token(n: usize) -> Result<String, String> {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes)
        .map_err(|_| "This computer's random number source is unavailable, so a secure sign-in could not be started.".to_string())?;
    Ok(base64url_nopad(&bytes))
}

/// Percent-encode a value for a URL query, RFC 3986. Pure; unit-tested.
///
/// Everything outside the unreserved set is encoded, which is stricter than necessary and
/// deliberately so: an authorization URL is assembled from a provider endpoint, our redirect
/// URI, a scope list and a random challenge, and a builder that has to reason about which
/// character is safe where is a builder that eventually gets it wrong.
///
/// POST bodies do NOT come through here. They go out as [`super::http::CurlReq::form_field`]
/// entries, which curl url-encodes itself, and which is also what keeps them out of argv.
pub fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char);
            }
            other => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// Decode a percent-encoded query value. Pure; unit-tested.
///
/// `+` becomes a space, because RFC 6749 §4.1.2 says the redirect's parameters are in
/// `application/x-www-form-urlencoded` form, where it does. A stray `%` or a truncated escape
/// is left alone rather than dropped: this parses attacker-reachable input, and the safe
/// answer to "that is not valid" is to hand back something that will fail the state check,
/// not to guess.
pub fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// The authorization URL.
// ---------------------------------------------------------------------------

/// Build the URL the user's browser is sent to. Pure; unit-tested.
///
/// Parameter order is fixed so the test can assert the whole string, and every value is
/// percent-encoded ([`percent_encode`]). `extra` carries the per-provider parameters that
/// decide whether a refresh token comes back at all; see [`authorize_extras`].
pub fn authorize_url(
    auth_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    scopes: &[&str],
    challenge: &str,
    state: &str,
    extra: &[(&str, &str)],
) -> String {
    let mut params: Vec<(&str, String)> = vec![
        ("client_id", client_id.to_string()),
        ("response_type", "code".to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("scope", scopes.join(" ")),
        ("state", state.to_string()),
        ("code_challenge", challenge.to_string()),
        ("code_challenge_method", "S256".to_string()),
    ];
    for (k, v) in extra {
        params.push((k, (*v).to_string()));
    }
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let joiner = if auth_endpoint.contains('?') { '&' } else { '?' };
    format!("{auth_endpoint}{joiner}{query}")
}

// ---------------------------------------------------------------------------
// The loopback redirect listener.
// ---------------------------------------------------------------------------

/// What arrived on the loopback port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectRequest {
    /// The provider returned an authorization code.
    Code {
        code: String,
        /// The `state` echoed back, if any. Checked by [`check_state`], never trusted.
        state: Option<String>,
    },
    /// The provider returned an error: the user declined, or the request was rejected.
    Denied {
        error: String,
        /// The `state` echoed back, if any. RFC 6749 §4.1.2.1 REQUIRES a provider to echo it
        /// on an error redirect too, so this is what tells a genuine refusal from one any
        /// local process could have written to our port. Checked, never trusted.
        state: Option<String>,
    },
    /// Anything else: a favicon request, a prefetch, a port scan, a malformed line. Answered
    /// with [`NOT_FOUND_PAGE`] and otherwise ignored, so one stray request cannot end a
    /// connect the user is still working through.
    Other,
}

/// Parse the FIRST line of an HTTP request into what it means for us. Pure; unit-tested.
///
/// Only `GET`, only a target beginning with [`REDIRECT_PATH`], and only the `code`, `state`
/// and `error` parameters. Everything else is [`RedirectRequest::Other`]. This is the whole
/// HTTP parser: there is no header parsing, no body, no keep-alive, because a one-shot
/// redirect catcher needs none of it and every line of parser is attack surface.
pub fn parse_redirect_request(line: &str) -> RedirectRequest {
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return RedirectRequest::Other;
    };
    if method != "GET" || !version.starts_with("HTTP/") || parts.next().is_some() {
        return RedirectRequest::Other;
    }
    let Some((path, query)) = target.split_once('?') else {
        return RedirectRequest::Other;
    };
    if path != REDIRECT_PATH {
        return RedirectRequest::Other;
    }
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else { continue };
        match key {
            "code" if code.is_none() => code = Some(percent_decode(value)),
            "state" if state.is_none() => state = Some(percent_decode(value)),
            "error" if error.is_none() => error = Some(percent_decode(value)),
            _ => {}
        }
    }
    // An error wins over a code: a response carrying both is malformed, and the safe reading
    // of a malformed authorization response is that it did not authorize anything.
    if let Some(error) = error {
        return RedirectRequest::Denied { error, state };
    }
    match code {
        Some(code) if !code.is_empty() => RedirectRequest::Code { code, state },
        _ => RedirectRequest::Other,
    }
}

/// Compare the `state` we sent with the one that came back. Pure; unit-tested.
///
/// A missing or mismatched state is refused. This is what stops a local process from
/// delivering an authorization code of its own to our port and having us exchange it, which
/// would bind the user's account to an attacker's authorization.
pub fn check_state(expected: &str, got: Option<&str>) -> Result<(), String> {
    match got {
        Some(got) if !expected.is_empty() && got == expected => Ok(()),
        _ => Err("The sign-in reply did not match the request this app started, so it was \
                  refused. Try connecting again."
            .to_string()),
    }
}

/// The user-facing message for an `error` parameter on the redirect. Pure; unit-tested.
///
/// The provider's own text is NEVER shown. The `error` value arrives over a loopback socket
/// any local process can write to, so treating it as display copy would let one put words in
/// our UI. Known codes get our sentence; anything else gets the generic one.
pub fn denied_message(error: &str) -> String {
    match error {
        "access_denied" => "The sign-in was declined, so nothing was connected.".to_string(),
        "invalid_scope" => {
            "The provider refused the permissions this app asked for, so nothing was connected."
                .to_string()
        }
        _ => "The provider did not complete the sign-in, so nothing was connected.".to_string(),
    }
}

/// How one provider wants the loopback redirect URI built.
///
/// The providers genuinely disagree, and getting this wrong is a connect that fails at the
/// provider with `redirect_uri_mismatch`, which tells the user nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedirectPolicy {
    /// The host that goes in the URI. Either `127.0.0.1` or `localhost`, and the difference
    /// is not cosmetic: they are different registered URIs at every provider.
    pub host: &'static str,
    /// Ports to try, in order. EMPTY means "any free port", which is only safe where the
    /// provider ignores the port when matching.
    pub ports: &'static [u16],
}

/// The ports Dropbox connects on.
///
/// Dropbox matches the redirect URI EXACTLY, port included, and has no loopback exemption.
/// So unlike the other two, its ports cannot be ephemeral: these exact URIs have to be
/// registered in the Dropbox App Console, and the app binds the first one that is free. A
/// small pool rather than one port, because a single hard-coded port is one already-running
/// program away from a connect that can never succeed.
///
/// **Registering these is part of setting `CCK_DROPBOX_CLIENT_ID`**: each must be entered as
/// `http://localhost:<port>/`, with the trailing slash, exactly as [`Redirect::bind`] builds
/// it.
pub const DROPBOX_REDIRECT_PORTS: &[u16] = &[47821, 47822, 47823, 47824];

/// How a provider's redirect URI must be built. Pure; unit-tested.
///
/// * **Google** documents the loopback flow as "start an HTTP listener on a random available
///   port" and prefers the IP literal over `localhost`, warning that `localhost` can trip
///   client firewalls.
/// * **Microsoft** ignores the port when matching a loopback redirect (RFC 8252 §7.3, stated
///   outright in its redirect-URI doc) and also recommends the IP literal.
/// * **Dropbox** does neither. Its reference says the redirect "must be the exact URI
///   registered in the App Console", it names only `localhost` as the plain-http exemption,
///   and `127.0.0.1` appears nowhere in its documentation. So Dropbox gets `localhost` and a
///   fixed pool.
pub fn redirect_policy(provider_id: &str) -> RedirectPolicy {
    match provider_id {
        "dropbox" => RedirectPolicy { host: "localhost", ports: DROPBOX_REDIRECT_PORTS },
        _ => RedirectPolicy { host: "127.0.0.1", ports: &[] },
    }
}

/// A bound loopback listener plus the redirect URI that names it.
struct Redirect {
    listener: TcpListener,
    uri: String,
}

impl Redirect {
    /// Bind a loopback port per `policy` and derive the redirect URI from it.
    ///
    /// Always binds the loopback INTERFACE (`127.0.0.1`), whatever the policy's host spells:
    /// the host is what the provider matches the URI against, and the interface is what the
    /// socket listens on. Binding anything routable would expose the catcher to the network.
    /// IPv4 only, because Microsoft states `[::1]` is not supported.
    fn bind(policy: RedirectPolicy) -> Result<Redirect, String> {
        let listener = if policy.ports.is_empty() {
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| {
                format!("This computer would not open a local port to receive the sign-in reply: {e}")
            })?
        } else {
            let mut bound = None;
            for port in policy.ports {
                if let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, *port)) {
                    bound = Some(listener);
                    break;
                }
            }
            bound.ok_or_else(|| {
                format!(
                    "None of the local ports this cloud service accepts ({}) were free, so the \
                     sign-in could not be started. Close whatever is using them and try again.",
                    policy
                        .ports
                        .iter()
                        .map(u16::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?
        };
        let port = listener
            .local_addr()
            .map_err(|e| format!("The local sign-in port could not be read back: {e}"))?
            .port();
        Ok(Redirect { uri: format!("http://{}:{port}{REDIRECT_PATH}", policy.host), listener })
    }

    /// Wait for the one request that matters, or until `deadline`.
    ///
    /// The first request that carries the state WE MINTED ends the wait, one way or the other:
    /// a code connects the account, and a refusal ends the flow with [`denied_message`]. The
    /// listener is dropped with `self`.
    ///
    /// # Why an unauthenticated request cannot end the flow (DRAGON-482)
    ///
    /// Everything else is answered and IGNORED, and "everything else" specifically includes a
    /// redirect carrying an `error`, or a code with the wrong `state`. That is a fix, not a
    /// relaxation. This port is on loopback, so any process running as the user can connect to
    /// it, and it was reachable the whole time the user was signing in. A single
    /// `GET /?error=access_denied` from such a process used to end the connect with "the
    /// sign-in was declined" while the user was still looking at the consent screen: a
    /// zero-effort denial of service on connecting an account, and one that reads as the
    /// provider's fault.
    ///
    /// A GENUINE refusal is still honoured, because RFC 6749 §4.1.2.1 requires the provider to
    /// echo `state` on the error redirect exactly as it does on the success one. So the rule is
    /// uniform and easy to state: **the state check gates every outcome**, and anything that
    /// cannot produce it only ever gets a page. The cost is that a provider which violates the
    /// RFC by dropping `state` from an error redirect makes the user wait out the deadline
    /// instead of seeing "declined" at once, which is the right way round for the trade.
    fn wait(&self, expected_state: &str, deadline: Instant) -> Result<String, String> {
        let _ = self.listener.set_nonblocking(true);
        while Instant::now() < deadline {
            match self.listener.accept() {
                Ok((mut stream, _peer)) => {
                    // Back to blocking, with a read timeout: the connection is already
                    // accepted, and a byte-at-a-time bounded read is simplest that way.
                    let _ = stream.set_nonblocking(false);
                    let Some(line) = read_request_line(&mut stream, deadline) else {
                        respond(&mut stream, "400 Bad Request", NOT_FOUND_PAGE);
                        continue;
                    };
                    match parse_redirect_request(&line) {
                        RedirectRequest::Code { code, state } => {
                            if check_state(expected_state, state.as_deref()).is_err() {
                                respond(&mut stream, "400 Bad Request", DENIED_PAGE.as_str());
                                log::debug!(
                                    "cloud oauth: ignored a redirect carrying the wrong state"
                                );
                                continue;
                            }
                            respond(&mut stream, "200 OK", SUCCESS_PAGE.as_str());
                            return Ok(code);
                        }
                        RedirectRequest::Denied { error, state } => {
                            respond(&mut stream, "200 OK", DENIED_PAGE.as_str());
                            if check_state(expected_state, state.as_deref()).is_err() {
                                log::debug!(
                                    "cloud oauth: ignored a refusal carrying the wrong state"
                                );
                                continue;
                            }
                            // The CODE is logged, never a description: it is a small closed
                            // vocabulary and it is the whole diagnosis.
                            log::debug!("cloud oauth: the sign-in was refused ({error})");
                            return Err(denied_message(&error));
                        }
                        RedirectRequest::Other => {
                            respond(&mut stream, "404 Not Found", NOT_FOUND_PAGE);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(ACCEPT_POLL);
                }
                Err(e) => {
                    return Err(format!("The local sign-in port stopped working: {e}"));
                }
            }
        }
        Err("The sign-in was not completed in time, so nothing was connected.".to_string())
    }
}

/// Read at most the first request line, bounded in both bytes and time.
///
/// One byte at a time so we stop exactly at the newline and never pull a body or a header
/// into memory. The line is short; the syscall count is not worth optimizing against the
/// guarantee that nothing past it is read.
///
/// # Why the overall deadline is passed in (DRAGON-482)
///
/// [`REDIRECT_READ_BUDGET`] alone bounds ONE read, not the line. A connection dripping one byte
/// every few seconds keeps every individual read inside its budget, so the line takes up to
/// [`MAX_REQUEST_LINE`] × the budget to finish: eight thousand reads, which is hours. And the
/// accept loop cannot notice, because it is blocked in here. So every read is clamped to
/// whatever is left of the FLOW'S deadline as well, and a drip simply gets nothing.
fn read_request_line(stream: &mut TcpStream, deadline: Instant) -> Option<String> {
    let line_deadline = (Instant::now() + REDIRECT_READ_BUDGET).min(deadline);
    let mut line: Vec<u8> = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    while line.len() < MAX_REQUEST_LINE {
        let left = line_deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            // A zero timeout means "no timeout" to the socket API, so it is checked here
            // rather than handed on.
            return None;
        }
        let _ = stream.set_read_timeout(Some(left));
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                if byte[0] != b'\r' {
                    line.push(byte[0]);
                }
            }
            Err(_) => return None,
        }
    }
    String::from_utf8(line).ok()
}

/// Write one of the constant pages back and close.
fn respond(stream: &mut TcpStream, status: &str, page: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        page.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(page.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

// ---------------------------------------------------------------------------
// Provider lookup and client ids.
// ---------------------------------------------------------------------------

/// One provider's OAuth details, unpacked from the registry.
///
/// A named struct rather than a tuple because most of its fields are `&str` and a tuple of
/// several strings is a bug waiting for someone to reorder it.
struct Endpoints {
    spec: &'static ProviderSpec,
    auth_url: &'static str,
    token_url: &'static str,
    scopes: &'static [&'static str],
    client_id_env: &'static str,
    baked_client_id: &'static str,
    client_secret_env: Option<&'static str>,
    baked_client_secret: &'static str,
}

/// The OAuth details for a provider id, or a message saying why there are none.
fn endpoints(provider_id: &str) -> Result<Endpoints, String> {
    let spec = super::provider(provider_id)
        .ok_or_else(|| "This build does not know that cloud service.".to_string())?;
    match spec.auth {
        AuthKind::OAuthPkce {
            auth_url,
            token_url,
            scopes,
            client_id_env,
            baked_client_id,
            client_secret_env,
            baked_client_secret,
        } => Ok(Endpoints {
            spec,
            auth_url,
            token_url,
            scopes,
            client_id_env,
            baked_client_id,
            client_secret_env,
            baked_client_secret,
        }),
        AuthKind::Unofficial => Err(format!(
            "{} has no public API for other apps to upload through, so it cannot be connected.",
            spec.display_name
        )),
    }
}

impl Endpoints {
    /// The client id for this provider, reading the environment override.
    fn client_id(&self) -> Result<String, String> {
        let from_env = std::env::var(self.client_id_env).ok();
        resolve_client_id(
            from_env.as_deref(),
            self.baked_client_id,
            self.spec.display_name,
            self.client_id_env,
        )
    }

    /// The client secret for this provider, if it declares one AND something supplies it
    /// (DRAGON-489 follow-up). `None` for a provider with no `client_secret_env` (Microsoft,
    /// Dropbox) without ever touching the environment for one, and `None` again for one that
    /// declares a variable nothing has set and carries no baked secret either.
    fn client_secret(&self) -> Option<String> {
        let env_name = self.client_secret_env?;
        resolve_client_secret(std::env::var(env_name).ok().as_deref(), self.baked_client_secret)
    }
}

/// Resolve an OAuth client secret: the runtime value, else the baked one, else nothing. Pure;
/// unit-tested.
///
/// The same chain as [`resolved_client_id`], for the same reason (DRAGON-508), but not a
/// parallel UX: every user needs a client id for every provider, so a missing one gets guided
/// copy naming the variable to set. A missing secret is a much narrower, single-provider case,
/// so this just resolves quietly and the exchange sends nothing extra when there is none.
pub fn resolve_client_secret(env_value: Option<&str>, baked: &str) -> Option<String> {
    runtime_then_baked(env_value, baked)
}

/// The chain itself: the runtime value if it is more than whitespace, else the baked one if it
/// is, else nothing. Pure; unit-tested through both of its callers.
///
/// ONE body for the id and the secret, because they are the same rule twice and a second copy
/// is a second thing to get wrong. Whitespace counts as absent at both levels, so an
/// exported-but-blank variable falls through to the baked value rather than blocking it.
fn runtime_then_baked(env_value: Option<&str>, baked: &str) -> Option<String> {
    let from_env = env_value.map(str::trim).unwrap_or("");
    if !from_env.is_empty() {
        return Some(from_env.to_string());
    }
    let baked = baked.trim();
    if baked.is_empty() { None } else { Some(baked.to_string()) }
}

/// Which client id this build uses for a provider, if it has one at all. Pure; unit-tested.
///
/// **The resolution chain, highest first** (DRAGON-508):
///
/// 1. The RUNTIME environment variable (`CCK_<PROVIDER>_CLIENT_ID`). It wins outright, so a
///    user with their own app registration can always override, official build or not.
/// 2. The value BAKED IN at compile time, from the distinct `CCK_BAKED_*` variable the release
///    workflow supplies. See `cloud`'s `BAKED_*` constants for why the two names must differ.
/// 3. Nothing. An empty answer is a real, supported state and not a bug: a build made from
///    source has no registration, and the app answers that by not offering the provider at all
///    (`cloud::provider_available`) and pointing at the setup guide instead.
///
/// Whitespace is not a value at either level, so an exported-but-blank variable falls through
/// to the baked id rather than blocking it.
///
/// The one place the order is decided, so the picker's visibility rule and the connect flow
/// can never disagree about whether a provider is configured.
pub fn resolved_client_id(env_value: Option<&str>, baked_client_id: &str) -> Option<String> {
    runtime_then_baked(env_value, baked_client_id)
}

/// [`resolved_client_id`], or a sentence saying what to set. Pure; unit-tested.
///
/// The failure half is reachable in one place now that the picker hides an unconfigured
/// provider (DRAGON-508): a RECONNECT on an account whose provider this build can no longer
/// resolve, which is a real state (the account was connected with a variable that is no longer
/// set). It stays worth a full sentence for exactly that reason.
pub fn resolve_client_id(
    env_value: Option<&str>,
    baked_client_id: &str,
    display_name: &str,
    env_name: &str,
) -> Result<String, String> {
    resolved_client_id(env_value, baked_client_id).ok_or_else(|| {
        // Written as INSTRUCTIONS, not as a diagnosis (DRAGON-482). This sentence is the whole
        // answer a user gets when a connect cannot start, so it has to end with something they
        // can DO, in the order they would do it. The earlier draft said the provider "cannot be
        // connected" and stopped there, which is a dead end wearing an explanation.
        format!(
            "{display_name} needs an app registration, and this build does not have one. To \
             connect it: create your own {display_name} app registration, set the {env_name} \
             environment variable to its client id, then start this app again."
        )
    })
}

/// The extra authorization parameters a provider needs to return a REFRESH token. Pure;
/// unit-tested.
///
/// This is the one place per-provider OAuth dialect lives, and it is small on purpose. Each
/// entry exists because without it the provider issues an access token only, and the account
/// silently stops working an hour after it is connected.
pub fn authorize_extras(provider_id: &str) -> &'static [(&'static str, &'static str)] {
    match provider_id {
        // Google issues a refresh token only for an offline-access grant, and only on the
        // FIRST consent unless consent is re-prompted. A reconnect is exactly the case where
        // the previous refresh token is gone, so the prompt is forced every time.
        "gdrive" => &[("access_type", "offline"), ("prompt", "consent")],
        // The SAME Google authorize endpoint as gdrive, so the SAME extras: YouTube is its own
        // registry row (DRAGON-493) but not its own OAuth dialect.
        "youtube" => &[("access_type", "offline"), ("prompt", "consent")],
        // Microsoft decides this with the `offline_access` scope, which is in the registry
        // row, so there is nothing extra on the URL.
        "onedrive" => &[],
        // Dropbox issues short-lived tokens plus a refresh token only when asked in exactly
        // this way. Without it the token lasts four hours and cannot be renewed.
        "dropbox" => &[("token_access_type", "offline")],
        _ => &[],
    }
}

// ---------------------------------------------------------------------------
// Token endpoint calls.
// ---------------------------------------------------------------------------

/// A provider's token endpoint response, success or failure, in one shape.
///
/// Both halves are optional because the same endpoint returns both, and a device poll
/// returns the failure half repeatedly as its ordinary "keep waiting" answer.
#[derive(Debug, Default, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
    token_type: Option<String>,
    error: Option<String>,
    /// The provider's own text for the failure. Never shown to the user (see
    /// [`token_error_message`]); read only by [`oauth_error_fields`] for the debug log.
    error_description: Option<String>,
}

/// Turn a token endpoint body into a [`TokenSet`]. Pure; unit-tested.
///
/// `now` is passed in rather than read, so the expiry arithmetic is testable, and
/// `previous_refresh` is carried forward when the response omits one: Google returns a
/// refresh token on the first consent and never again, so a refresh that dropped it would
/// disconnect the account on the NEXT refresh rather than this one.
pub fn parse_token_response(
    body: &str,
    now: chrono::DateTime<chrono::Utc>,
    previous_refresh: Option<&str>,
) -> Result<TokenSet, String> {
    let parsed: TokenResponse = serde_json::from_str(body)
        .map_err(|_| "The provider's sign-in reply could not be read.".to_string())?;
    if let Some(error) = parsed.error.as_deref() {
        return Err(token_error_message(error));
    }
    let access_token = parsed
        .access_token
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "The provider's sign-in reply carried no access token.".to_string())?;
    let expires_at = parsed
        .expires_in
        .filter(|s| *s > 0)
        .and_then(|s| now.checked_add_signed(chrono::TimeDelta::seconds(s)))
        .map(|t| t.to_rfc3339());
    let refresh_token = parsed
        .refresh_token
        .filter(|t| !t.is_empty())
        .or_else(|| previous_refresh.filter(|t| !t.is_empty()).map(str::to_string));
    Ok(TokenSet {
        access_token,
        refresh_token,
        expires_at,
        scopes: parsed
            .scope
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        token_type: parsed.token_type.unwrap_or_else(|| "Bearer".to_string()),
    })
}

/// The user-facing message for an OAuth `error` code. Pure; unit-tested.
///
/// `invalid_grant` is the one that earns [`RECONNECT_PREFIX`]: it is what every provider here
/// returns when a refresh token has been revoked, has expired, or was invalidated by a
/// password change, and no amount of retrying changes it. The provider's own
/// `error_description` is never shown, because it is written for a developer and often names
/// internal error codes.
pub fn token_error_message(error: &str) -> String {
    match error {
        "invalid_grant" => reconnect_message(
            "the cloud service no longer accepts this account's saved sign-in. Connect it again.",
        ),
        "invalid_scope" => {
            "The cloud service refused the permissions this app asked for.".to_string()
        }
        "invalid_client" => {
            "The cloud service did not recognise this app's sign-in id.".to_string()
        }
        "unauthorized_client" => {
            "The cloud service will not allow this app to sign in this way.".to_string()
        }
        // The last two of RFC 6749 section 5.2's token-endpoint error set. Both mean the
        // REQUEST was wrong, not the account, so reconnecting cannot fix them: they point at
        // an app bug (a malformed body, or asking for a grant type the endpoint does not
        // serve), which is why neither is a reconnect.
        "invalid_request" => {
            "This app sent a sign-in request the cloud service could not accept.".to_string()
        }
        "unsupported_grant_type" => {
            "The cloud service does not support the sign-in method this app used.".to_string()
        }
        // Google-specific, not in RFC 6749: its OAuth servers return HTTP 403 with this code
        // when an app exceeds its authorization or token grant rate limits (see Google's
        // "OAuth Application Rate Limits", support.google.com/cloud/answer/9028764). It is
        // transient, so the user can retry, and it is NOT a reconnect.
        "rate_limit_exceeded" => {
            "The cloud service is receiving too many sign-in attempts. Wait a little while, then \
             try again."
                .to_string()
        }
        _ => "The cloud service refused the sign-in.".to_string(),
    }
}

/// The `error` and `error_description` fields of a failed token response, for the debug log.
/// Pure; unit-tested.
///
/// Both are PROTOCOL detail, never user content: the code names WHY the provider refused
/// (`invalid_grant`, `rate_limit_exceeded`, ...) and the description is the provider's own
/// developer-facing text about THIS app's OAuth request. Neither says anything about what the
/// user captured, so both are safe for the debug log (the description is still never shown to
/// the USER; see [`token_error_message`]). A body that is not JSON, or carries no `error`,
/// yields `(None, None)`.
pub fn oauth_error_fields(body: &str) -> (Option<String>, Option<String>) {
    match serde_json::from_str::<TokenResponse>(body) {
        Ok(parsed) => (parsed.error, parsed.error_description),
        Err(_) => (None, None),
    }
}

/// POST to a token endpoint and return the response body.
///
/// `no_retry` on every call: an authorization code is single-use, a device poll has its own
/// pacing, and a refresh can rotate the token, so an automatic second attempt is at best
/// wasted and at worst spends a credential twice.
fn post_token(token_url: &str, form: &[(&str, &str)]) -> Result<super::http::CurlResponse, String> {
    let hosts = super::registry_hosts();
    let mut req = CurlReq::new(Method::Post, token_url, &hosts, TOKEN_BUDGET)?.no_retry();
    for (k, v) in form {
        req = req.form_field(k, v);
    }
    req.send()
}

/// Append a `client_secret` form field only when there is one to send. Pure; unit-tested.
///
/// Shared by every token-endpoint call that needs it: [`exchange_code`] (the initial
/// authorization_code exchange) and [`ensure_fresh`]'s refresh call. Google's "Desktop app"
/// client type requires `client_secret` on EVERY call for that client, not only the initial
/// exchange; the refresh call went without it for a while, which made every refresh fail with
/// `invalid_request` the first time an access token actually expired (about an hour in). A
/// provider with no `client_secret_env` (Microsoft, Dropbox) passes `None` here and the form
/// is unchanged, which is what keeps their requests byte-identical.
fn with_client_secret<'a>(
    mut form: Vec<(&'a str, &'a str)>,
    secret: Option<&'a str>,
) -> Vec<(&'a str, &'a str)> {
    if let Some(secret) = secret {
        form.push(("client_secret", secret));
    }
    form
}

/// Exchange an authorization code for tokens.
///
/// `client_secret` is `Some` only for a provider whose native client type issues one
/// (Google's "Desktop app" type; see [`AuthKind::OAuthPkce::client_secret_env`]'s doc).
fn exchange_code(
    token_url: &str,
    client_id: &str,
    client_secret: Option<&str>,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<TokenSet, String> {
    let form = with_client_secret(
        vec![
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
        ],
        client_secret,
    );
    let response = post_token(token_url, &form)?;
    let body = response.text();
    if !response.is_success() {
        return Err(response_error(&body, "The sign-in could not be completed."));
    }
    parse_token_response(&body, chrono::Utc::now(), None)
}

/// Read a provider's `error` out of a failed response body, or fall back to `fallback`.
/// Pure; unit-tested.
///
/// A body that is not JSON, or that carries no `error`, must not become user copy: it can be
/// an HTML error page or a proxy's message, and it can be long. The fallback sentence is used
/// instead.
pub fn response_error(body: &str, fallback: &str) -> String {
    match serde_json::from_str::<TokenResponse>(body) {
        Ok(parsed) => match parsed.error.as_deref() {
            Some(error) => token_error_message(error),
            None => fallback.to_string(),
        },
        Err(_) => fallback.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Connecting: the browser flow.
// ---------------------------------------------------------------------------

/// Connect an account through the user's browser, blocking until it finishes or times out,
/// with the deadline and the browser opener injected.
///
/// **Never call this on the UI thread**: it waits minutes by design. The settings page runs
/// it on a background task and delivers the result as a message.
///
/// The injected `open` is what makes the flow testable without launching a browser, and what
/// lets a caller substitute its own handling: the settings page passes one that only reports
/// the URL back to the UI (DRAGON-489 follow-up), which shows it as a clickable link plus a
/// QR code and lets the user open it themselves, rather than a browser launch that can fail
/// silently with nothing else on screen to fall back to. The house `foo_with` seam
/// (CLAUDE.md).
pub fn connect_interactive_with(
    provider_id: &str,
    browser_deadline: Duration,
    open: &mut dyn FnMut(&str),
) -> Result<ConnectedTokens, String> {
    let ends = endpoints(provider_id)?;
    let spec = ends.spec;
    let client_id = ends.client_id()?;
    let verifier = random_token(32)?;
    let challenge = pkce_challenge(&verifier);
    let state = random_token(16)?;
    let redirect = Redirect::bind(redirect_policy(spec.id))?;

    let url = authorize_url(
        ends.auth_url,
        &client_id,
        &redirect.uri,
        ends.scopes,
        &challenge,
        &state,
        authorize_extras(spec.id),
    );
    // The URL carries the challenge and the state. It is not a credential by itself, but it
    // is exactly the shape `redact_oauth` exists for, so it goes through it before a log ever
    // sees it.
    log::debug!("cloud oauth: the sign-in page is ready for {} ({})", spec.id, crate::diag::redact_oauth(&url));
    open(&url);

    let deadline = Instant::now() + browser_deadline;
    let code = redirect.wait(&state, deadline)?;
    let client_secret = ends.client_secret();
    let tokens = exchange_code(
        ends.token_url,
        &client_id,
        client_secret.as_deref(),
        &code,
        &verifier,
        &redirect.uri,
    )?;
    log::debug!("cloud oauth: connected a {} account", spec.id);
    Ok(ConnectedTokens { provider: spec.id.to_string(), tokens })
}

// ---------------------------------------------------------------------------
// Staying fresh: expiry, single flight, refresh.
// ---------------------------------------------------------------------------

/// Whether a stored token should be refreshed before use. Pure; unit-tested.
///
/// Three answers, and the two edge cases matter:
///
/// * `None` means the provider never said when it expires, so there is nothing to plan
///   around and the token is used until a call fails. Refreshing on a schedule we invented
///   would spend a refresh token for no reason.
/// * An UNPARSEABLE stamp is treated as expired. A refresh is cheap and usually succeeds; a
///   request with a dead token is a user-visible failure. The asymmetry is the whole
///   argument.
pub fn needs_refresh(expires_at: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> bool {
    let Some(stamp) = expires_at else { return false };
    match chrono::DateTime::parse_from_rfc3339(stamp) {
        Ok(t) => t.with_timezone(&chrono::Utc) <= now + chrono::TimeDelta::seconds(EXPIRY_MARGIN_SECS),
        Err(_) => true,
    }
}

/// The per-account refresh gates, minted on demand and never removed.
///
/// Never removed on purpose: an account is connected once and used for the life of the
/// process, so the map holds a handful of entries, and reclaiming them would need a second
/// lock ordering to do safely. A `HashMap` that only grows to the number of connected
/// accounts is not a leak worth a bug.
fn gates() -> &'static Mutex<HashMap<String, Arc<Mutex<()>>>> {
    static GATES: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    GATES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The single-flight gate for one account. Unit-tested (identity, and that it serializes).
///
/// Two callers asking about the SAME account get the same gate, so only one of them refreshes
/// and the other waits and then finds the fresh token already stored. Two callers asking about
/// DIFFERENT accounts get different gates and do not block each other, which matters as soon
/// as an upload and a folder listing run at once.
///
/// Without this, parallel calls race: both see an expired token, both spend the refresh
/// token, and a provider that ROTATES refresh tokens (Microsoft does) invalidates the first
/// response's token with the second request, leaving the account permanently disconnected.
pub fn account_gate(account_id: &str) -> Arc<Mutex<()>> {
    let mut map = match gates().lock() {
        Ok(map) => map,
        // A poisoned gate map still describes the right accounts: the panic that poisoned it
        // happened in a caller, not in the map. Recovering beats refusing every refresh for
        // the rest of the process.
        Err(poison) => poison.into_inner(),
    };
    Arc::clone(map.entry(account_id.to_string()).or_default())
}

/// How long [`ensure_fresh`] waits for ANOTHER PROCESS to finish refreshing the same account.
///
/// A refresh is one token endpoint call whose own budget is [`TOKEN_BUDGET`] (30s) plus a
/// small store write, so 45s covers a slow one with room to spare. Past that the holder is not
/// slow, it is wedged, and nothing in this app is allowed to wait unboundedly (DRAGON-118).
/// The failure is then a sentence the user can act on, not a hang.
pub const CROSS_PROCESS_GATE_BUDGET: Duration = Duration::from_secs(45);

/// The cross-process refresh lock for one account, held for the life of the value.
///
/// The lock is the open FILE HANDLE, not the file's existence, so it is released by the
/// kernel when this drops, when the process exits, and when the process is killed. That is
/// what makes it safe where a lock file would go stale forever after one crash.
struct RefreshLock {
    /// `None` when there is nowhere to put a lock file (no config directory). The in-process
    /// gate is then the only one, which is what this feature had before and is still better
    /// than refusing every refresh.
    ///
    /// Never READ, and that is the point: holding it IS the lock, and dropping it releases
    /// it. An RAII handle has no accessor to write, so the attribute is the honest way to say
    /// so rather than inventing a getter nobody calls.
    #[allow(dead_code)]
    file: Option<std::fs::File>,
}

/// The lock file for one account, creating its directory.
///
/// `None` for an id [`super::secrets::is_valid_account_id`] refuses: the id becomes a FILE
/// NAME, so the same check that protects the secrets directory protects this one, and it is
/// the same one function rather than a second copy of the rule.
fn refresh_lock_path(account_id: &str) -> Option<PathBuf> {
    if !super::secrets::is_valid_account_id(account_id) {
        return None;
    }
    let dir = crate::util::app_config_dir()?.join("cloud-locks");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!("{account_id}.lock")))
}

/// Take the cross-process refresh lock for `account_id`, waiting at most `budget`.
///
/// The lock itself is [`super::lock_file_waiting`], the one implementation of "an exclusive
/// lock on a file" in `cloud/`. Unit-tested (that it excludes,
/// that it does not block a DIFFERENT account, and that it gives up rather than waiting
/// forever).
fn take_refresh_lock(account_id: &str, budget: Duration) -> Result<RefreshLock, String> {
    let Some(path) = refresh_lock_path(account_id) else {
        return Ok(RefreshLock { file: None });
    };
    match super::lock_file_waiting(&path, budget) {
        Ok(file) => Ok(RefreshLock { file: Some(file) }),
        Err(()) => {
            log::warn!(
                "cloud oauth: another process has been renewing {account_id} for over {}s",
                budget.as_secs()
            );
            Err("Something else on this computer is still renewing this cloud account's \
                 sign-in. Try again in a moment."
                .to_string())
        }
    }
}

/// A live access token for `account_id`, refreshing first if it is close to expiring.
///
/// The contract callers depend on, in order:
///
/// 1. **Single flight per account, across PROCESSES.** Concurrent calls in one process
///    serialize on [`account_gate`]; concurrent PROCESSES serialize on a per-account file lock
///    ([`take_refresh_lock`]), taken inside it. Both are needed. This app's whole model is
///    short-lived processes, so an editor listing folders while a detached upload child
///    refreshes is the ORDINARY case, not a corner one, and the in-process mutex says nothing
///    about it. Whichever loses re-reads the store and finds the fresh token already there.
/// 2. **Persist before returning.** A rotated refresh token is written through
///    [`super::secrets::store`] BEFORE the access token is handed back, so the window in which
///    the provider has rotated and the disk has not is one store write wide. It is not zero:
///    a process killed between the provider's reply and that write loses the rotated token,
///    and the account then needs connecting again. Nothing short of writing the token before
///    it is known would close that, so what this promises is the narrow window, not none.
/// 3. **A dead authorization is distinguishable.** The error starts with [`RECONNECT_PREFIX`]
///    ([`needs_reconnect`]), so the UI can offer Reconnect.
///
/// **Never call this on the UI thread**: it can make a network request.
pub fn ensure_fresh(account_id: &str) -> Result<String, String> {
    let gate = account_gate(account_id);
    // Held across the refresh, which IS the in-process single flight. Poison-tolerant for the
    // same reason as the map above.
    let _flight = match gate.lock() {
        Ok(guard) => guard,
        Err(poison) => poison.into_inner(),
    };
    // The cross-process half, INSIDE the in-process one so only ever one thread per process
    // queues for it. Two providers that rotate refresh tokens (Microsoft does) invalidate the
    // first reply's token with the second request, which leaves the account permanently
    // disconnected; a sibling process racing us does that just as effectively as a sibling
    // thread.
    let _cross = take_refresh_lock(account_id, CROSS_PROCESS_GATE_BUDGET)?;

    // Read INSIDE the gate: a caller that waited here may find the other one already
    // refreshed, in which case there is nothing left to do.
    let stored = super::secrets::load(account_id)?.ok_or_else(|| {
        reconnect_message("this account's sign-in is no longer stored on this computer.")
    })?;
    let tokens = TokenSet::from_json(&stored)?;
    if !needs_refresh(tokens.expires_at.as_deref(), chrono::Utc::now()) {
        return Ok(tokens.access_token);
    }

    let account = super::accounts::get(account_id)
        .ok_or_else(|| "That cloud account is no longer set up.".to_string())?;
    let ends = endpoints(&account.provider)?;
    let Some(refresh_token) = tokens.refresh_token.clone() else {
        return Err(reconnect_message(&format!(
            "this {} account has no saved renewal, so it has to be connected again.",
            ends.spec.display_name
        )));
    };
    let client_id = ends.client_id()?;
    // See `with_client_secret`'s doc: this call needs the same secret the initial exchange
    // does, and used to go without it.
    let client_secret = ends.client_secret();
    let form = with_client_secret(
        vec![
            ("grant_type", "refresh_token"),
            ("client_id", client_id.as_str()),
            ("refresh_token", refresh_token.as_str()),
        ],
        client_secret.as_deref(),
    );
    let response = post_token(ends.token_url, &form)?;
    let body = response.text();
    if !response.is_success() {
        let message = response_error(&body, "This cloud account's sign-in could not be renewed.");
        // Log the raw OAuth error code (and the provider's description), which is what tells us
        // WHICH refusal this is: the derived `needs_reconnect` boolean collapses every
        // unmapped code into the same catch-all, so without the code a multi-device or
        // rate-limit refusal is indistinguishable from any other. Both fields are protocol
        // detail, not user content (see `oauth_error_fields`).
        //
        // `debug!`, not `warn!`, on purpose: `warn!`/`error!` print to STDERR on every run
        // regardless of `CCK_DEBUG_LOG` (diag.rs's module doc: "Stderr behaviour is
        // UNCHANGED"), and a token refresh failing is an ORDINARY, routine event, not a
        // defect, the same classification `oauth.rs`'s `the sign-in was refused (...)` line
        // already gives a user declining consent. It still happens on every account that
        // idles long enough for Google to expire or revoke its token, so at `warn!` it would
        // print for a completely healthy install the first ordinary time that happens. `debug!`
        // keeps it silent everywhere by default and still lands in the shared file the moment
        // `CCK_DEBUG_LOG=1` is set (`diag::FILE_LEVEL` is `Debug` for our own records), which is
        // exactly the "silent unless asked for" shape this diagnostic needs.
        let (oauth_error, oauth_desc) = oauth_error_fields(&body);
        log::debug!(
            "cloud oauth: renewing {account_id} failed with HTTP {} (oauth error: {}, \
             description: {}, reconnect needed: {})",
            response.status,
            oauth_error.as_deref().unwrap_or("<none>"),
            oauth_desc.as_deref().unwrap_or("<none>"),
            needs_reconnect(&message)
        );
        return Err(message);
    }
    let refreshed = parse_token_response(&body, chrono::Utc::now(), Some(&refresh_token))?;
    // Before returning, always: see the contract above.
    super::secrets::store(account_id, &refreshed.to_json()?)?;
    log::debug!("cloud oauth: renewed the sign-in for {account_id}");
    Ok(refreshed.access_token)
}

#[cfg(test)]
mod pkce_tests {
    use super::*;

    /// RFC 4648 §10's own vectors, in the URL alphabet without padding.
    #[test]
    fn base64url_matches_the_rfc_vectors() {
        assert_eq!(base64url_nopad(b""), "");
        assert_eq!(base64url_nopad(b"f"), "Zg");
        assert_eq!(base64url_nopad(b"fo"), "Zm8");
        assert_eq!(base64url_nopad(b"foo"), "Zm9v");
        assert_eq!(base64url_nopad(b"foob"), "Zm9vYg");
        assert_eq!(base64url_nopad(b"fooba"), "Zm9vYmE");
        assert_eq!(base64url_nopad(b"foobar"), "Zm9vYmFy");
        // The URL alphabet: these two bytes are `+` and `/` in standard base64, and the whole
        // point of base64url is that they are not.
        assert_eq!(base64url_nopad(&[0xfb, 0xff, 0xfe]), "-__-");
        // And no padding, ever: RFC 7636 forbids it in a code challenge.
        assert!(!base64url_nopad(b"any length at all").contains('='));
    }

    /// **The PKCE vector from RFC 7636 Appendix B.** If this ever fails, every connect fails
    /// at the provider with an opaque error, so it is pinned against the spec rather than
    /// against our own output.
    #[test]
    fn the_s256_challenge_matches_rfc_7636() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(pkce_challenge(verifier), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    /// A fresh verifier is the length and alphabet RFC 7636 §4.1 requires (43 to 128
    /// characters of unreserved ASCII), and is different every time.
    #[test]
    fn a_fresh_verifier_is_well_formed_and_unique() {
        let a = random_token(32).expect("the OS random source");
        let b = random_token(32).expect("the OS random source");
        assert_eq!(a.len(), 43, "32 bytes of base64url");
        assert!((43..=128).contains(&a.len()));
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~'));
        assert_ne!(a, b);
        // The state is shorter but the same shape.
        assert_eq!(random_token(16).expect("random").len(), 22);
    }
}

#[cfg(test)]
mod encoding_tests {
    use super::*;

    #[test]
    fn percent_encoding_keeps_only_the_unreserved_set() {
        assert_eq!(percent_encode("abcXYZ019-._~"), "abcXYZ019-._~");
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("https://a/b?c=d&e"), "https%3A%2F%2Fa%2Fb%3Fc%3Dd%26e");
        // A scope list is the real case: the separator must not survive as a literal space.
        assert_eq!(percent_encode("Files.ReadWrite offline_access"), "Files.ReadWrite%20offline_access");
        // Non-ASCII goes out as its UTF-8 bytes.
        assert_eq!(percent_encode("é"), "%C3%A9");
    }

    #[test]
    fn percent_decoding_reverses_it() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("4%2F0AY0e-g7"), "4/0AY0e-g7");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("plain-token_123"), "plain-token_123");
        // A truncated or invalid escape is left alone rather than guessed at.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("a%zzb"), "a%zzb");
        // The round trip is what matters for a real code.
        let code = "4/0AeaYSHDx-_ab/cd+ef";
        assert_eq!(percent_decode(&percent_encode(code)), code);
    }
}

#[cfg(test)]
mod authorize_url_tests {
    use super::*;

    /// The whole URL is asserted, because a missing or misspelled parameter fails at the
    /// provider with a message the user cannot act on.
    #[test]
    fn the_authorization_url_carries_every_pkce_parameter() {
        let url = authorize_url(
            "https://provider.example/auth",
            "client-123",
            "http://127.0.0.1:41234/",
            &["scope.one", "scope.two"],
            "CHALLENGE",
            "STATE",
            &[("access_type", "offline")],
        );
        assert_eq!(
            url,
            "https://provider.example/auth?client_id=client-123&response_type=code\
             &redirect_uri=http%3A%2F%2F127.0.0.1%3A41234%2F&scope=scope.one%20scope.two\
             &state=STATE&code_challenge=CHALLENGE&code_challenge_method=S256\
             &access_type=offline"
        );
    }

    /// An endpoint that already carries a query gets `&`, not a second `?`.
    #[test]
    fn an_endpoint_with_a_query_is_extended_not_broken() {
        let url = authorize_url(
            "https://provider.example/auth?tenant=common",
            "c",
            "http://127.0.0.1:1/",
            &[],
            "ch",
            "st",
            &[],
        );
        assert!(url.starts_with("https://provider.example/auth?tenant=common&client_id=c"));
        assert_eq!(url.matches('?').count(), 1);
    }

    /// Every connectable provider asks for something that yields a REFRESH token. Without
    /// it an account works for an hour and then silently stops, which is the single worst
    /// failure this feature can have.
    #[test]
    fn every_provider_asks_for_offline_access() {
        for spec in super::super::registry().iter().filter(|p| p.auth.is_connectable()) {
            let extras = authorize_extras(spec.id);
            let scopes = match spec.auth {
                AuthKind::OAuthPkce { scopes, .. } => scopes,
                AuthKind::Unofficial => &[],
            };
            let offline = extras.iter().any(|(k, v)| {
                (*k == "access_type" && *v == "offline")
                    || (*k == "token_access_type" && *v == "offline")
            }) || scopes.contains(&"offline_access");
            assert!(offline, "{} would never get a refresh token", spec.id);
        }
    }
}

#[cfg(test)]
mod redirect_parse_tests {
    use super::*;

    #[test]
    fn a_redirect_with_a_code_is_recognised() {
        let got = parse_redirect_request("GET /?code=4%2F0Ab_cd&state=xyz HTTP/1.1");
        assert_eq!(
            got,
            RedirectRequest::Code { code: "4/0Ab_cd".to_string(), state: Some("xyz".to_string()) }
        );
    }

    /// A refusal carries its `state` through, because that is what tells a real one from a
    /// request any local process could have written to the port (see [`Redirect::wait`]).
    #[test]
    fn a_declined_sign_in_is_recognised() {
        let got = parse_redirect_request("GET /?error=access_denied&state=xyz HTTP/1.0");
        assert_eq!(
            got,
            RedirectRequest::Denied {
                error: "access_denied".to_string(),
                state: Some("xyz".to_string()),
            }
        );
        // A refusal with no state at all parses, and is what `wait` then ignores.
        assert_eq!(
            parse_redirect_request("GET /?error=access_denied HTTP/1.1"),
            RedirectRequest::Denied { error: "access_denied".to_string(), state: None }
        );
    }

    /// A response carrying BOTH is malformed. The safe reading is that nothing was
    /// authorized, so the error wins and no code is exchanged.
    #[test]
    fn an_error_beats_a_code() {
        let got = parse_redirect_request("GET /?code=abc&error=access_denied&state=s HTTP/1.1");
        assert_eq!(
            got,
            RedirectRequest::Denied {
                error: "access_denied".to_string(),
                state: Some("s".to_string()),
            }
        );
    }

    /// **The parser's whole job is to say no.** Anything that is not our redirect is
    /// `Other`, which is answered with a constant 404 and otherwise ignored.
    #[test]
    fn anything_that_is_not_our_redirect_is_ignored() {
        for line in [
            "",
            "GET /favicon.ico HTTP/1.1",
            "GET / HTTP/1.1",                       // no query at all
            "GET /?state=xyz HTTP/1.1",             // no code and no error
            "GET /?code= HTTP/1.1",                 // an empty code is not a code
            "POST /?code=abc HTTP/1.1",             // only GET
            "GET /other?code=abc HTTP/1.1",         // only our path
            "GET /?code=abc",                       // no version
            "GET /?code=abc HTTP/1.1 extra",        // a fourth field
            "CONNECT example.com:443 HTTP/1.1",
            "\u{1}\u{2}\u{3}",
        ] {
            assert_eq!(parse_redirect_request(line), RedirectRequest::Other, "{line:?}");
        }
    }

    /// The first occurrence of a parameter wins, so a duplicate appended by anything in the
    /// middle cannot override the real one.
    #[test]
    fn a_duplicated_parameter_cannot_override_the_first() {
        let got = parse_redirect_request("GET /?code=real&code=fake&state=s HTTP/1.1");
        assert_eq!(
            got,
            RedirectRequest::Code { code: "real".to_string(), state: Some("s".to_string()) }
        );
    }
}

#[cfg(test)]
mod redirect_policy_tests {
    use super::*;

    /// **Dropbox is the odd one out, and forgetting it is a connect that always fails.** Its
    /// redirect URI is matched exactly, port included, so it gets a fixed pool; the other two
    /// documented an ephemeral port and get one.
    #[test]
    fn dropbox_needs_fixed_ports_and_the_others_do_not() {
        let dropbox = redirect_policy("dropbox");
        assert_eq!(dropbox.host, "localhost", "127.0.0.1 is undocumented at Dropbox");
        assert!(!dropbox.ports.is_empty(), "Dropbox cannot use an ephemeral port");
        for id in ["gdrive", "onedrive"] {
            let policy = redirect_policy(id);
            assert_eq!(policy.host, "127.0.0.1", "{id} prefers the IP literal");
            assert!(policy.ports.is_empty(), "{id} accepts any free port");
        }
    }

    /// The pool is several distinct ports in the dynamic range, so one busy port cannot make
    /// Dropbox permanently unconnectable.
    #[test]
    fn the_dropbox_port_pool_is_usable() {
        assert!(DROPBOX_REDIRECT_PORTS.len() >= 2, "one port is a single point of failure");
        let mut sorted = DROPBOX_REDIRECT_PORTS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), DROPBOX_REDIRECT_PORTS.len(), "a duplicate port is a wasted slot");
        for port in DROPBOX_REDIRECT_PORTS {
            assert!(*port >= 1024, "{port} is privileged");
        }
    }

    /// The URI a bound listener reports is the shape that has to be registered, ending in the
    /// path the parser insists on. A drift here is `redirect_uri_mismatch` at the provider.
    #[test]
    fn a_bound_redirect_uri_has_the_registered_shape() {
        let redirect = Redirect::bind(redirect_policy("gdrive")).expect("a loopback port");
        assert!(redirect.uri.starts_with("http://127.0.0.1:"), "{}", redirect.uri);
        assert!(redirect.uri.ends_with(REDIRECT_PATH), "{}", redirect.uri);
        // No query component: Microsoft refuses a redirect URI with one for personal
        // accounts, and our own parser splits on the first `?`.
        assert!(!redirect.uri.contains('?'));
    }
}

#[cfg(test)]
mod state_tests {
    use super::*;

    /// **The CSRF case.** A code delivered to our port by anything that does not know the
    /// state we minted is refused, so a local process cannot bind the user's account to its
    /// own authorization.
    #[test]
    fn only_the_state_we_sent_is_accepted() {
        assert!(check_state("abc123", Some("abc123")).is_ok());
        assert!(check_state("abc123", Some("abc124")).is_err());
        assert!(check_state("abc123", None).is_err());
        assert!(check_state("abc123", Some("")).is_err());
        // An empty expectation must never match, or a bug that loses the state would open
        // exactly the hole the state exists to close.
        assert!(check_state("", Some("")).is_err());
        assert!(check_state("", None).is_err());
    }

    /// The refusal says what to do and quotes nothing from the request.
    #[test]
    fn the_refusal_quotes_nothing() {
        let err = check_state("expected", Some("ATTACKER-STATE")).expect_err("refused");
        assert!(!err.contains("ATTACKER-STATE"));
        assert!(!err.contains("expected"));
    }
}

#[cfg(test)]
mod page_tests {
    use super::*;

    /// **The reflected-content case.** The three pages are fixed content, and none of them has
    /// a placeholder anything could be substituted into. A page that echoed the query would
    /// reflect an attacker's string into a browser on a loopback origin.
    #[test]
    fn the_served_pages_reflect_nothing() {
        for page in [SUCCESS_PAGE.as_str(), DENIED_PAGE.as_str(), NOT_FOUND_PAGE] {
            assert!(!page.contains("{}"), "a page has a substitution point");
            assert!(!page.contains("{ }"));
            assert!(page.starts_with("<!doctype html>"));
            // No script at all: these pages have nothing to do.
            assert!(!page.contains("<script"));
        }
        // And they are distinguishable, so a user can tell what happened.
        assert_ne!(SUCCESS_PAGE.as_str(), DENIED_PAGE.as_str());
    }

    /// The two landing pages carry the app's own icon, inline (no external image request a
    /// loopback page could not satisfy anyway), and it is a real, complete SVG rather than a
    /// truncated fragment or the stripped-off XML prolog leaking through.
    #[test]
    fn the_landing_pages_carry_the_app_icon_inline() {
        for page in [SUCCESS_PAGE.as_str(), DENIED_PAGE.as_str()] {
            assert!(page.contains("<svg"), "the icon did not make it into the page");
            assert!(page.contains("</svg>"), "the icon markup is not closed");
            // The stripped XML declaration/DOCTYPE must not survive into the served page.
            assert!(!page.contains("<?xml"));
            assert!(!page.contains("<!DOCTYPE"));
        }
        // `NOT_FOUND_PAGE` deliberately carries none of this: it is never meant to be looked at.
        assert!(!NOT_FOUND_PAGE.contains("<svg"));
    }

    /// **The pages are DARK** (DRAGON-495). Both landing pages paint the dark background, put
    /// light text on it, and declare `color-scheme:dark` so the browser's own chrome (and the
    /// frame before our CSS lands) matches. The light palette they shipped with must be gone
    /// from both, not merely overridden further down the stylesheet.
    #[test]
    fn the_landing_pages_are_dark() {
        for page in [SUCCESS_PAGE.as_str(), DENIED_PAGE.as_str()] {
            assert!(page.contains("color-scheme:dark"), "the browser chrome is not told");
            assert!(page.contains(&format!("background:{PAGE_BG}")), "no dark background");
            assert!(page.contains(&format!("color:{PAGE_FG}")), "no light text");
            assert!(page.contains(&format!("color:{PAGE_MUTED}")), "no body-text colour");
            // The light page's own colours, gone rather than shadowed further down the sheet.
            assert!(!page.contains("background:#f5f5f7"), "the light background survives");
            assert!(!page.contains("color:#1d1d1f"), "the dark-on-light text survives");
            assert!(!page.contains("#55555a"), "the light body colour survives");
        }
    }

    /// **One sentence per line** (DRAGON-495, owner request). Each landing page says two
    /// things, what happened and what to do next, and each gets its own block rather than
    /// wrapping into one paragraph.
    #[test]
    fn each_sentence_gets_its_own_line() {
        for page in [SUCCESS_PAGE.as_str(), DENIED_PAGE.as_str()] {
            assert_eq!(page.matches("<p>").count(), 2, "not two separate lines: {page}");
            // Two lines, not one paragraph that merely contains a full stop: the sentences
            // must not share a block.
            assert!(!page.contains(". You can close"), "the sentences share one block");
        }
        assert!(SUCCESS_PAGE.contains("<p>You can close this tab and go back to the app.</p>"));
        assert!(DENIED_PAGE.contains("<p>Nothing was connected.</p>"));
    }

    /// The palette is a real dark one, not a light page with one value swapped: the background
    /// is the DARKEST of the three and both text colours sit above it, which is the whole
    /// readability claim. The CARD sits between the page and the text: lighter than the page
    /// (that is what makes it read as a card, DRAGON-495) and darker than anything written on
    /// it.
    #[test]
    fn the_palette_puts_light_text_on_a_dark_background() {
        let luma = |hex: &str| -> u32 {
            let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).expect("a hex colour");
            (v >> 16) + ((v >> 8) & 0xff) + (v & 0xff)
        };
        assert!(luma(PAGE_BG) < luma(PAGE_MUTED), "the body text does not lift off the page");
        assert!(luma(PAGE_MUTED) < luma(PAGE_FG), "the heading is not the brightest thing");
        // And the background really is dark, not merely darker than the text.
        assert!(luma(PAGE_BG) < 3 * 64, "the background is not a dark one");
        // The card lifts off the page, its edge lifts off the card, and both stay under the
        // body text so nothing competes with what is written on them.
        assert!(luma(PAGE_BG) < luma(CARD_BG), "the card does not lift off the page");
        assert!(luma(CARD_BG) < luma(CARD_EDGE), "the card has no defined edge");
        assert!(luma(CARD_EDGE) < luma(PAGE_MUTED), "the card's edge competes with the text");
    }

    /// **The content sits in a CARD** (DRAGON-495, owner request): a rounded, contained surface
    /// on the dark page, with NO drop shadow (a shadow on a near-black page is invisible, and
    /// the owner ruled one out).
    #[test]
    fn the_content_sits_in_a_rounded_shadowless_card() {
        for page in [SUCCESS_PAGE.as_str(), DENIED_PAGE.as_str()] {
            assert!(page.contains(r#"<div class="card">"#), "no card wraps the content");
            assert!(page.contains(&format!("background:{CARD_BG}")), "the card has no fill");
            assert!(page.contains("border-radius:18px"), "the card is not rounded");
            assert!(page.contains("box-shadow:none"), "the card does not refuse a shadow");
            // The card contains everything: icon, heading and both lines, in that one block.
            let card = page.split(r#"<div class="card">"#).nth(1).expect("the card opens");
            assert!(card.contains("<svg"), "the icon is outside the card");
            assert!(card.contains("<h1>"), "the heading is outside the card");
            assert_eq!(card.matches("<p>").count(), 2, "the lines are outside the card");
        }
    }

    /// [`inline_app_icon_svg`] drops exactly the two lines that are illegal mid-document, and
    /// nothing else: the SVG element itself survives whole.
    #[test]
    fn inlining_the_icon_strips_only_the_xml_prolog() {
        let inlined = inline_app_icon_svg();
        assert!(inlined.starts_with("<svg"), "{}", &inlined[..inlined.len().min(40)]);
        assert!(!inlined.contains("<?xml"));
        assert!(!inlined.contains("<!DOCTYPE"));
        assert!(inlined.ends_with("</svg>\n") || inlined.ends_with("</svg>"));
        // Nothing about the element itself was touched: same content length as the source file
        // minus exactly the stripped prefix.
        let prefix_len = APP_ICON_SVG.len() - inlined.len();
        assert_eq!(&APP_ICON_SVG[prefix_len..], inlined);
    }

    /// The denial copy is ours, never the provider's. The `error` value arrives over a
    /// socket any local process can write to.
    #[test]
    fn a_denial_never_shows_the_providers_text() {
        let injected = "<script>alert(1)</script> your account is fine, paste your password";
        let message = denied_message(injected);
        assert!(!message.contains("script"));
        assert!(!message.contains("password"));
        assert_eq!(message, denied_message("something-else-entirely"));
        // The two known codes get their own sentence.
        assert!(denied_message("access_denied").contains("declined"));
        assert!(denied_message("invalid_scope").contains("permissions"));
    }
}

#[cfg(test)]
mod token_parse_tests {
    use super::*;

    fn at(stamp: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(stamp).expect("a test stamp").with_timezone(&chrono::Utc)
    }

    /// A real-shaped success response (Google's, with invented values).
    #[test]
    fn a_token_response_becomes_a_token_set() {
        let body = r#"{
            "access_token": "ya29.a0-EXAMPLE",
            "expires_in": 3599,
            "refresh_token": "1//0e-EXAMPLE",
            "scope": "https://www.googleapis.com/auth/drive.file",
            "token_type": "Bearer"
        }"#;
        let set = parse_token_response(body, at("2026-08-02T12:00:00+00:00"), None).expect("parse");
        assert_eq!(set.access_token, "ya29.a0-EXAMPLE");
        assert_eq!(set.refresh_token.as_deref(), Some("1//0e-EXAMPLE"));
        assert_eq!(set.token_type, "Bearer");
        assert_eq!(set.scopes, vec!["https://www.googleapis.com/auth/drive.file"]);
        // 3599 seconds after the stamp above.
        assert_eq!(set.expires_at.as_deref(), Some("2026-08-02T12:59:59+00:00"));
    }

    /// **The refresh-token carry-forward.** Google returns a refresh token once, at first
    /// consent, and never again. A refresh that dropped it would leave the account working
    /// now and dead at the next renewal, which is the hardest kind of bug to attribute.
    #[test]
    fn a_refresh_without_a_new_token_keeps_the_old_one() {
        let body = r#"{"access_token":"new-access","expires_in":3600,"token_type":"Bearer"}"#;
        let set = parse_token_response(body, at("2026-08-02T12:00:00+00:00"), Some("old-refresh"))
            .expect("parse");
        assert_eq!(set.refresh_token.as_deref(), Some("old-refresh"));
        // A rotated one replaces it (Microsoft rotates on every refresh).
        let rotated = r#"{"access_token":"a","refresh_token":"rotated","expires_in":3600}"#;
        let set = parse_token_response(rotated, at("2026-08-02T12:00:00+00:00"), Some("old-refresh"))
            .expect("parse");
        assert_eq!(set.refresh_token.as_deref(), Some("rotated"));
    }

    /// Missing or unusable pieces are refused rather than stored as a half-account.
    #[test]
    fn an_unusable_response_is_refused() {
        let now = at("2026-08-02T12:00:00+00:00");
        assert!(parse_token_response("not json", now, None).is_err());
        assert!(parse_token_response("{}", now, None).is_err());
        assert!(parse_token_response(r#"{"access_token":""}"#, now, None).is_err());
        // No expiry stated is fine: the token is used until it fails.
        let set = parse_token_response(r#"{"access_token":"a"}"#, now, None).expect("parse");
        assert_eq!(set.expires_at, None);
        assert_eq!(set.token_type, "Bearer", "the default when the provider omits it");
    }

    /// **The reconnect signal.** `invalid_grant` is the only error that earns the prefix,
    /// because it is the only one a user can act on by reconnecting.
    #[test]
    fn only_a_dead_authorization_asks_for_a_reconnect() {
        let err = token_error_message("invalid_grant");
        assert!(needs_reconnect(&err), "invalid_grant must be a reconnect: {err}");
        for other in [
            "invalid_scope",
            "invalid_client",
            "unauthorized_client",
            "invalid_request",
            "unsupported_grant_type",
            "rate_limit_exceeded",
            "server_error",
            "",
        ] {
            assert!(!needs_reconnect(&token_error_message(other)), "{other} must not be");
        }
        // And it survives the whole parse path, which is how a caller actually meets it.
        let body = r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#;
        let err = parse_token_response(body, at("2026-08-02T12:00:00+00:00"), None)
            .expect_err("an error body is not a token set");
        assert!(needs_reconnect(&err));
        // The provider's own description is never shown.
        assert!(!err.contains("revoked."));
    }

    /// **The nameable refusals get their own copy.** Each of the newly mapped codes must give a
    /// specific sentence, not fall through to the generic catch-all, and none may masquerade as
    /// a reconnect (only `invalid_grant` earns that).
    #[test]
    fn the_named_refusals_have_specific_copy() {
        let generic = token_error_message("something_unmapped");
        for code in ["invalid_request", "unsupported_grant_type", "rate_limit_exceeded"] {
            let msg = token_error_message(code);
            assert_ne!(msg, generic, "{code} must not use the catch-all copy");
            assert!(!needs_reconnect(&msg), "{code} is not a reconnect: {msg}");
            assert!(!msg.contains('\u{2014}'), "no em-dash in {code}: {msg}");
        }
        // The rate-limit case tells the user to wait and retry, since it is transient.
        assert!(token_error_message("rate_limit_exceeded").contains("try again"));
        // An unmapped code still lands on the generic sentence.
        assert_eq!(generic, "The cloud service refused the sign-in.");
    }

    /// **The diagnostic seam.** `oauth_error_fields` pulls the raw code and description out of a
    /// failure body for the debug log, and yields nothing for a body that is not a JSON error.
    #[test]
    fn oauth_error_fields_reads_the_code_and_description() {
        let body = r#"{"error":"rate_limit_exceeded","error_description":"Rate limit exceeded."}"#;
        let (code, desc) = oauth_error_fields(body);
        assert_eq!(code.as_deref(), Some("rate_limit_exceeded"));
        assert_eq!(desc.as_deref(), Some("Rate limit exceeded."));
        // A code with no description is fine; only the code is required.
        let (code, desc) = oauth_error_fields(r#"{"error":"invalid_request"}"#);
        assert_eq!(code.as_deref(), Some("invalid_request"));
        assert_eq!(desc, None);
        // A non-error body (a success reply, or not JSON at all) carries neither.
        assert_eq!(oauth_error_fields(r#"{"access_token":"a"}"#), (None, None));
        assert_eq!(oauth_error_fields("<html>502</html>"), (None, None));
    }

    /// A non-JSON failure body (an HTML error page, a proxy notice) never becomes user copy.
    #[test]
    fn a_failure_body_that_is_not_json_falls_back() {
        let html = "<html><body>502 Bad Gateway from squid/5.7 at proxy.corp.example</body></html>";
        assert_eq!(response_error(html, "The fallback."), "The fallback.");
        assert_eq!(response_error("{}", "The fallback."), "The fallback.");
        assert!(needs_reconnect(&response_error(r#"{"error":"invalid_grant"}"#, "x")));
    }
}

#[cfg(test)]
mod token_privacy_tests {
    use super::*;

    /// **The privacy test for this module.** `{:?}` on a token set must not print either
    /// credential: `Debug` is how a token reaches a log by accident.
    #[test]
    fn debug_never_prints_a_token() {
        let set = TokenSet {
            access_token: "cck-access-marker-91be".to_string(),
            refresh_token: Some("cck-refresh-marker-91be".to_string()),
            expires_at: Some("2026-08-02T12:59:59+00:00".to_string()),
            scopes: vec!["drive.file".to_string()],
            token_type: "Bearer".to_string(),
        };
        let shown = format!("{set:?}");
        assert!(!shown.contains("91be"), "a token reached Debug: {shown}");
        // It still says whether each one is THERE, which is the diagnostic value.
        assert!(shown.contains("<redacted>"));
        assert!(shown.contains("2026-08-02T12:59:59+00:00"));
        let empty = TokenSet::default();
        assert!(format!("{empty:?}").contains("<none>"));
    }

    /// The replacement string says PRESENT or ABSENT and nothing else. Not a length: how
    /// long a credential is, is a fact about the credential, and this string exists so that
    /// no fact about it is printed.
    #[test]
    fn the_redacted_shape_says_only_whether_there_is_one() {
        assert_eq!(redacted_shape(true), "<redacted>");
        assert_eq!(redacted_shape(false), "<none>");
        assert_ne!(redacted_shape(true), redacted_shape(false), "the two must be tellable apart");
        for shown in [redacted_shape(true), redacted_shape(false)] {
            assert!(!shown.chars().any(|c| c.is_ascii_digit()), "{shown} leaks a measurement");
        }
    }

    /// The stored form round-trips through exactly what `secrets::store` accepts.
    #[test]
    fn a_token_set_round_trips_through_the_secret_store_shape() {
        let set = TokenSet {
            access_token: "a".to_string(),
            refresh_token: Some("r".to_string()),
            expires_at: Some("2026-08-02T12:59:59+00:00".to_string()),
            scopes: vec!["one".to_string(), "two".to_string()],
            token_type: "Bearer".to_string(),
        };
        let json = set.to_json().expect("serialize");
        assert!(super::super::secrets::is_json_object(&json), "the store only takes an object");
        assert_eq!(TokenSet::from_json(&json).expect("parse"), set);
        // The token type is stored rather than assumed, and survives the round trip: the
        // provider says what it is, and every one here says `Bearer`.
        let bare = TokenSet { token_type: String::new(), ..set };
        let json = bare.to_json().expect("serialize");
        assert_eq!(TokenSet::from_json(&json).expect("parse"), bare);
    }

    /// A stored secret that cannot be read asks for a reconnect rather than a retry: there
    /// is nothing to retry.
    #[test]
    fn unreadable_stored_details_ask_for_a_reconnect() {
        let err = TokenSet::from_json("not json at all").expect_err("refused");
        assert!(needs_reconnect(&err));
    }
}

#[cfg(test)]
mod expiry_tests {
    use super::*;

    fn at(stamp: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(stamp).expect("a test stamp").with_timezone(&chrono::Utc)
    }

    /// The margin is the point: a token that dies in ninety seconds is refreshed NOW, because
    /// the request it is about to authorize may be a multi-chunk upload.
    #[test]
    fn a_token_is_refreshed_before_it_actually_expires() {
        let now = at("2026-08-02T12:00:00+00:00");
        assert!(!needs_refresh(Some("2026-08-02T12:59:59+00:00"), now), "an hour left");
        assert!(!needs_refresh(Some("2026-08-02T12:02:01+00:00"), now), "just outside the margin");
        assert!(needs_refresh(Some("2026-08-02T12:02:00+00:00"), now), "exactly at the margin");
        assert!(needs_refresh(Some("2026-08-02T12:01:00+00:00"), now), "inside the margin");
        assert!(needs_refresh(Some("2026-08-02T11:00:00+00:00"), now), "already expired");
        // A different offset is the same instant, and must read the same.
        assert!(!needs_refresh(Some("2026-08-02T13:59:59+01:00"), now));
    }

    /// The two edge cases the doc argues about, pinned so neither drifts.
    #[test]
    fn an_unknown_expiry_is_left_alone_and_an_unreadable_one_is_not() {
        let now = at("2026-08-02T12:00:00+00:00");
        assert!(!needs_refresh(None, now), "no expiry means nothing to plan around");
        assert!(needs_refresh(Some("whenever"), now), "an unreadable stamp is treated as expired");
        assert!(needs_refresh(Some(""), now));
    }
}

#[cfg(test)]
mod single_flight_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// One gate per account, and the same gate every time. Identity is what makes the
    /// serialization below possible at all.
    #[test]
    fn one_gate_per_account() {
        let a1 = account_gate("00000000000000000000000000000001");
        let a2 = account_gate("00000000000000000000000000000001");
        let b = account_gate("00000000000000000000000000000002");
        assert!(Arc::ptr_eq(&a1, &a2), "one account must share one gate");
        assert!(!Arc::ptr_eq(&a1, &b), "two accounts must not block each other");
    }

    /// **The race this exists to stop.** Eight threads all want the same account's token at
    /// once; only one may be inside the gate at a time. Without it they all refresh, and a
    /// provider that rotates refresh tokens invalidates the survivors, disconnecting the
    /// account for good.
    #[test]
    fn only_one_refresh_per_account_runs_at_a_time() {
        let id = "00000000000000000000000000000003";
        let inside = Arc::new(AtomicUsize::new(0));
        let worst = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let (inside, worst) = (Arc::clone(&inside), Arc::clone(&worst));
            handles.push(std::thread::spawn(move || {
                let gate = account_gate(id);
                let _flight = gate.lock().unwrap_or_else(|p| p.into_inner());
                let n = inside.fetch_add(1, Ordering::SeqCst) + 1;
                worst.fetch_max(n, Ordering::SeqCst);
                // Long enough that an unguarded version would certainly overlap.
                std::thread::sleep(Duration::from_millis(20));
                inside.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.join().expect("a worker panicked");
        }
        assert_eq!(worst.load(Ordering::SeqCst), 1, "two refreshes overlapped");
    }

    /// Different accounts genuinely do NOT serialize, which is what keeps an upload on one
    /// account from waiting behind a folder listing on another.
    #[test]
    fn different_accounts_do_not_block_each_other() {
        let held = account_gate("00000000000000000000000000000004");
        let _guard = held.lock().unwrap_or_else(|p| p.into_inner());
        let other = account_gate("00000000000000000000000000000005");
        assert!(other.try_lock().is_ok(), "another account's gate must be free");
    }
}

#[cfg(test)]
mod client_id_tests {
    use super::*;

    /// **The chain, pinned end to end** (DRAGON-508): runtime beats baked beats nothing.
    ///
    /// The middle rung is what an official build has and a build from source does not, and the
    /// top rung is what keeps a user's own registration working in either. Getting the order
    /// wrong the other way would mean a baked id silently overriding what a user exported,
    /// which is the one thing this must never do.
    #[test]
    fn runtime_beats_baked_beats_empty() {
        // Runtime wins over a baked id that is also present.
        assert_eq!(resolved_client_id(Some("from-env"), "baked"), Some("from-env".to_string()));
        // Baked is used when nothing is exported.
        assert_eq!(resolved_client_id(None, "baked"), Some("baked".to_string()));
        // Neither is nothing at all.
        assert_eq!(resolved_client_id(None, ""), None);
        // Whitespace is not a value at EITHER level: a variable exported as blank falls
        // through to the baked id rather than blocking it, and a blank baked id is no id.
        assert_eq!(resolved_client_id(Some("   "), "baked"), Some("baked".to_string()));
        assert_eq!(resolved_client_id(Some(""), "baked"), Some("baked".to_string()));
        assert_eq!(resolved_client_id(None, "  "), None);
        assert_eq!(resolved_client_id(Some("  "), "  "), None);
        // Surrounding whitespace is trimmed off a real value.
        assert_eq!(resolved_client_id(Some(" id "), ""), Some("id".to_string()));
        assert_eq!(resolved_client_id(None, " id "), Some("id".to_string()));
    }

    /// The `Result` face of the same chain, which is what the connect flow calls.
    #[test]
    fn the_environment_overrides_the_baked_id() {
        assert_eq!(
            resolve_client_id(Some("from-env"), "baked", "Google Drive", "CCK_GDRIVE_CLIENT_ID"),
            Ok("from-env".to_string())
        );
        assert_eq!(
            resolve_client_id(None, "baked", "Google Drive", "CCK_GDRIVE_CLIENT_ID"),
            Ok("baked".to_string())
        );
        // Whitespace is not a client id.
        assert_eq!(
            resolve_client_id(Some("   "), "baked", "Google Drive", "CCK_GDRIVE_CLIENT_ID"),
            Ok("baked".to_string())
        );
    }

    /// **The source-build state.** No baked id and no override is a real configuration, not a
    /// bug, so the message says what to set rather than failing opaquely. Since DRAGON-508 the
    /// picker hides such a provider outright, and this sentence is what the one remaining way
    /// in (a reconnect on an account whose variable is no longer set) answers with.
    #[test]
    fn no_client_id_anywhere_says_what_to_set() {
        let err = resolve_client_id(None, "", "Dropbox", "CCK_DROPBOX_CLIENT_ID")
            .expect_err("there is no id");
        assert!(err.contains("CCK_DROPBOX_CLIENT_ID"), "the message must name the variable: {err}");
        assert!(err.contains("Dropbox"));
        assert!(!needs_reconnect(&err), "there is nothing to reconnect to");
        assert!(resolve_client_id(Some(""), "  ", "Dropbox", "CCK_DROPBOX_CLIENT_ID").is_err());
    }
}

#[cfg(test)]
mod client_secret_tests {
    use super::*;

    /// A present, non-blank value resolves; surrounding whitespace is trimmed the same way a
    /// client id's is.
    #[test]
    fn resolves_a_present_nonblank_value() {
        assert_eq!(resolve_client_secret(Some("shh"), ""), Some("shh".to_string()));
        assert_eq!(resolve_client_secret(Some("  shh  "), ""), Some("shh".to_string()));
    }

    /// Absent or blank is `None`, not an error: unlike a missing client id, a missing secret
    /// is not a build-wide problem, so there is nothing here to fail loudly about.
    #[test]
    fn absent_or_blank_resolves_to_none() {
        assert_eq!(resolve_client_secret(None, ""), None);
        assert_eq!(resolve_client_secret(Some(""), ""), None);
        assert_eq!(resolve_client_secret(Some("   "), ""), None);
    }

    /// **The secret is on the SAME chain as the id** (DRAGON-508): runtime beats baked beats
    /// nothing. An official build bakes Google's, and a user's own exported secret still wins
    /// over it, which is what lets somebody run the official build against their own project.
    #[test]
    fn the_secret_follows_the_same_chain_as_the_id() {
        assert_eq!(resolve_client_secret(Some("mine"), "baked"), Some("mine".to_string()));
        assert_eq!(resolve_client_secret(None, "baked"), Some("baked".to_string()));
        assert_eq!(resolve_client_secret(Some("  "), "baked"), Some("baked".to_string()));
        assert_eq!(resolve_client_secret(None, "  "), None);
        assert_eq!(resolve_client_secret(None, " baked "), Some("baked".to_string()));
    }

    /// **The declarative split itself.** Google is the one provider whose native client type
    /// issues a secret; Microsoft's and Dropbox's public client types issue none, so their
    /// `Endpoints` never even look at the environment for one, regardless of what is set
    /// there. Read straight off the registry, not through a real environment read, so this
    /// stays deterministic without touching process-wide state.
    #[test]
    fn only_google_declares_a_client_secret_env() {
        let google = crate::cloud::provider("gdrive").expect("gdrive is registered");
        let AuthKind::OAuthPkce { client_secret_env, .. } = google.auth else {
            panic!("gdrive must be an OAuthPkce provider");
        };
        assert_eq!(client_secret_env, Some("CCK_GDRIVE_CLIENT_SECRET"));

        for id in ["onedrive", "dropbox"] {
            let spec = crate::cloud::provider(id).unwrap_or_else(|| panic!("{id} is registered"));
            let AuthKind::OAuthPkce { client_secret_env, .. } = spec.auth else {
                panic!("{id} must be an OAuthPkce provider");
            };
            assert_eq!(client_secret_env, None, "{id} must not declare a client secret");
        }
    }

    /// **The regression this whole file exists for.** `with_client_secret` is what both
    /// `exchange_code` and `ensure_fresh`'s refresh call build their form through: a `None`
    /// must leave the form untouched (Microsoft, Dropbox), and a `Some` must add exactly one
    /// `client_secret` field (Google), on EVERY call, refresh included. The refresh call once
    /// skipped this, which surfaced as `invalid_request` the first time a token actually
    /// needed renewing.
    #[test]
    fn with_client_secret_appends_only_when_present() {
        let base = vec![("grant_type", "refresh_token"), ("client_id", "abc")];
        assert_eq!(with_client_secret(base.clone(), None), base, "no secret, no new field");
        assert_eq!(
            with_client_secret(base.clone(), Some("shh")),
            vec![("grant_type", "refresh_token"), ("client_id", "abc"), ("client_secret", "shh")],
        );
    }
}

#[cfg(test)]
mod loopback_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Send one raw request line to the listener and read whatever comes back, so the
    /// listener's write completes before the next connection is made.
    fn knock(port: u16, line: &str) {
        let Ok(mut stream) = TcpStream::connect((Ipv4Addr::LOCALHOST, port)) else { return };
        let _ = stream.write_all(format!("{line}\r\n\r\n").as_bytes());
        let _ = stream.flush();
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut sink = Vec::new();
        let _ = stream.read_to_end(&mut sink);
    }

    fn port_of(redirect: &Redirect) -> u16 {
        redirect.listener.local_addr().expect("a bound address").port()
    }

    /// **The local denial-of-service this fixes** (DRAGON-482). The redirect port is on
    /// loopback for the whole time the user is signing in, so any process running as the user
    /// can write to it. A single forged `error=access_denied`, or a code with someone else's
    /// state, used to END the connect while the consent screen was still on screen. Now they
    /// are answered and ignored, and the real redirect that arrives afterwards still works.
    #[test]
    fn a_forged_error_cannot_end_the_flow() {
        let redirect = Redirect::bind(redirect_policy("gdrive")).expect("a loopback port");
        let port = port_of(&redirect);
        let sender = std::thread::spawn(move || {
            for forged in [
                "GET /?error=access_denied&state=WRONG-STATE HTTP/1.1",
                "GET /?error=access_denied HTTP/1.1",
                "GET /?error=server_error&state= HTTP/1.1",
                "GET /?code=ATTACKERS-CODE&state=WRONG-STATE HTTP/1.1",
                "GET /?code=ATTACKERS-CODE HTTP/1.1",
                "GET /favicon.ico HTTP/1.1",
            ] {
                knock(port, forged);
            }
            // …and then the browser finally follows the real redirect.
            knock(port, "GET /?code=THE-REAL-CODE&state=the-state HTTP/1.1");
        });
        let got = redirect.wait("the-state", Instant::now() + Duration::from_secs(20));
        sender.join().expect("the sender thread");
        assert_eq!(got, Ok("THE-REAL-CODE".to_string()));
    }

    /// The other half of the same rule: a refusal that DOES carry our state is the user
    /// declining, and that still ends the flow at once. RFC 6749 §4.1.2.1 requires the
    /// provider to echo `state` on the error redirect, which is what makes this separable.
    #[test]
    fn a_real_refusal_still_ends_the_flow() {
        let redirect = Redirect::bind(redirect_policy("gdrive")).expect("a loopback port");
        let port = port_of(&redirect);
        let sender = std::thread::spawn(move || {
            knock(port, "GET /?error=access_denied&state=the-state HTTP/1.1");
        });
        let got = redirect.wait("the-state", Instant::now() + Duration::from_secs(20));
        sender.join().expect("the sender thread");
        let err = got.expect_err("a real denial ends the flow");
        assert!(err.contains("declined"), "{err}");
    }

    /// **The slowloris case** (DRAGON-482). One byte every 50ms keeps every individual read
    /// inside [`REDIRECT_READ_BUDGET`], so a per-read timeout alone would let one connection
    /// hold the accept loop for `MAX_REQUEST_LINE` × 10s, which is hours, and the flow's own
    /// deadline could never fire because the loop that checks it is blocked. Clamping each
    /// read to what is LEFT of the deadline is what bounds it.
    #[test]
    fn a_byte_drip_cannot_outlive_the_deadline() {
        let redirect = Redirect::bind(redirect_policy("gdrive")).expect("a loopback port");
        let port = port_of(&redirect);
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let dripper = {
            let stop = std::sync::Arc::clone(&stop);
            std::thread::spawn(move || {
                let Ok(mut stream) = TcpStream::connect((Ipv4Addr::LOCALHOST, port)) else {
                    return;
                };
                while !stop.load(Ordering::Relaxed) {
                    if stream.write_all(b"G").is_err() {
                        break;
                    }
                    let _ = stream.flush();
                    std::thread::sleep(Duration::from_millis(50));
                }
            })
        };
        let started = Instant::now();
        let got = redirect.wait("the-state", started + Duration::from_millis(600));
        let took = started.elapsed();
        stop.store(true, Ordering::Relaxed);
        let _ = dripper.join();
        assert!(got.is_err(), "a drip must never be read as a code");
        assert!(took < Duration::from_secs(5), "the drip outlived the deadline: {took:?}");
    }
}

#[cfg(test)]
mod refresh_lock_tests {
    use super::*;

    /// **The cross-process single flight.** Two holders must never have one account's refresh
    /// lock at once, one account's lock must not block another's, and a wait must end.
    ///
    /// Two `flock`s in ONE process still exclude each other: the lock belongs to the open file
    /// description, and each open makes a new one. So this is the real thing, not a stand-in.
    /// It runs against the real filesystem, which is safe here for the same reason the secrets
    /// tests are: `util::is_dev_process` routes a cargo-run process's config dir into a
    /// sandbox.
    #[test]
    fn the_refresh_lock_excludes_one_account_and_gives_up() {
        let id = "00ff00ff00ff00ff00ff00ff00ffaa01";
        if refresh_lock_path(id).is_none() {
            // No config directory at all: the lock degrades to the in-process gate by design,
            // and there is nothing here to assert.
            return;
        }
        let held = take_refresh_lock(id, Duration::from_secs(1)).expect("the first holder");
        let denied = take_refresh_lock(id, Duration::from_millis(150));
        assert!(denied.is_err(), "two holders got one account's refresh lock");
        // The refusal is a sentence the user can act on, not a hang and not a code.
        let message = denied.err().expect("a message");
        assert!(message.ends_with('.'), "{message}");
        assert!(!crate::diag::redact_oauth(&message).contains("<redacted>"), "{message}");

        // A DIFFERENT account is not blocked by it: an upload and a folder listing on two
        // accounts have to run at once.
        let other = take_refresh_lock("00ff00ff00ff00ff00ff00ff00ffaa02", Duration::from_millis(150));
        assert!(other.is_ok(), "one account's refresh blocked another's");

        drop(held);
        assert!(
            take_refresh_lock(id, Duration::from_millis(500)).is_ok(),
            "the lock was not released with its holder"
        );
    }

    /// An id that could steer a write out of the lock directory never yields a path, which is
    /// the same check (and the same function) the secrets store uses.
    #[test]
    fn a_crafted_account_id_gets_no_lock_file() {
        for bad in ["../../etc/passwd", "a/b", "", "abc", "ZZZZZZZZ"] {
            assert!(refresh_lock_path(bad).is_none(), "{bad:?} must not yield a lock path");
        }
    }

    /// The budget is the DRAGON-118 rule applied here: long enough for a real refresh to
    /// finish, short enough that a wedged sibling is not forever.
    #[test]
    fn the_gate_budget_outlasts_a_refresh_without_being_forever() {
        assert!(CROSS_PROCESS_GATE_BUDGET > TOKEN_BUDGET, "a real refresh must fit inside it");
        assert!(CROSS_PROCESS_GATE_BUDGET <= Duration::from_secs(120), "but it cannot be a hang");
    }
}
