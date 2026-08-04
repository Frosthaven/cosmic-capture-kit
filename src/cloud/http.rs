//! The cloud transport: `curl`, with the secrets kept out of argv (DRAGON-482).
//!
//! Modelled on [`crate::update`]'s `fetch_text`, which is the app's existing proof that a
//! curl shell-out is the right weight here: curl is present on macOS, on Windows 10+ and
//! on virtually every Linux, it is already a documented runtime dependency, and it saves
//! the whole async-TLS-client dependency tree for a handful of requests.
//!
//! # The one hard rule: a secret never reaches argv
//!
//! On Linux every process on the machine can read `/proc/<pid>/cmdline`. On macOS and
//! Windows the same is true for anything the user runs. So an `Authorization: Bearer …`
//! header, a refresh token, an authorization code or a PKCE verifier passed as a curl
//! ARGUMENT is readable by any other program while the request is in flight, and lands in
//! any process accounting the machine keeps.
//!
//! Every header, every form field AND THE URL therefore go through `curl --config -`, a
//! config file fed on STDIN and written from memory: it never touches the filesystem and it
//! never touches argv. What is left in argv is only what [`CurlReq::argv`] builds, and that
//! is deliberately the shortest list that works: the method, the two timeouts, the file
//! paths, and `--config -` itself. `argv_carries_no_secret` pins it.
//!
//! The URL joined that list in the DRAGON-482 fix round. A URL looks like addressing rather
//! than a credential, and for most requests it is, but two of them are not: Google's
//! resumable session URI carries an `upload_id` that authorizes writing to the user's drive,
//! and Microsoft's carries its own `tempauth` token. Rather than split "safe URLs" from
//! "pre-authorized URLs", which is a builder where the wrong method eventually gets called,
//! ALL urls take the safe path, so the invariant is uniform and one test can state it.
//!
//! Nothing here distinguishes a "secret" header from an ordinary one, and that is on
//! purpose, for the same reason. ALL headers and ALL form fields take the safe path.
//!
//! # The other rules
//!
//! * **https only, and only to a host [`crate::cloud::registry`] names.** [`check_url`] is
//!   pure and unit-tested, and the allowlist is derived from the provider table rather than
//!   maintained beside it ([`crate::cloud::registry_hosts`]), so a host nothing in the app
//!   knows about is unreachable by construction.
//! * **Every request carries an explicit timeout budget, passed by the caller.** There is
//!   no default: a caller that has not thought about how long this request may take has
//!   not finished writing it. `--max-time` is what enforces it, the same bound
//!   `fetch_text` relies on.
//! * **Redirects are NOT followed.** A redirect can leave the allowlisted host, and this
//!   client sends credentials. If a provider ever needs one, add it as an explicit opt-in
//!   with the allowlist re-checked on the new host, never as a default.
//! * **One retry, and only when repeating could help.** See [`should_retry`].
//!
//! # Size
//!
//! The config is written to a PIPE, so it must stay small (well under the OS pipe buffer)
//! or a large config and a large response could deadlock against each other. That is why a
//! file body goes through `--data-binary @path` in argv rather than through the config: an
//! upload payload is exactly the thing that must not travel down that pipe.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long past a request's own `--max-time` we wait for curl to actually exit.
///
/// `--max-time` is supposed to end the transfer, so anything still running after it plus this
/// grace is not slow, it is stuck: a wedged resolver, a TLS handshake in a library that is not
/// watching its own clock. See [`run_bounded`].
const REAP_GRACE: Duration = Duration::from_secs(10);

/// How often the reap loop wakes to check whether curl has exited.
const REAP_POLL: Duration = Duration::from_millis(20);

/// The HTTP methods this client speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

impl Method {
    /// The wire name, which is also what `-X` takes. Pure; unit-tested.
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
        }
    }
}

/// What to send as the request body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// Nothing. Form fields (if any) still apply.
    None,
    /// A document sent verbatim, through the stdin config (`data = "…"`). Goes the safe
    /// route because a JSON body is exactly where a token turns up.
    Text(String),
    /// A file streamed as the body (`--data-binary @path`). The PATH is in argv, which is
    /// fine: a path is not a credential. It never reaches a log except through
    /// [`crate::diag::path_shape`].
    File(PathBuf),
}

/// One request, built and then sent.
///
/// Construct with [`CurlReq::new`], which is where the allowlist and the timeout are
/// enforced, so a `CurlReq` that exists is a request that was allowed to exist.
#[derive(Debug, Clone)]
pub struct CurlReq {
    method: Method,
    url: String,
    /// The host [`check_url`] resolved. Kept so a log line can name WHERE a request went
    /// without carrying the path or the query, which is where ids and tokens live.
    host: String,
    /// Total budget for the whole request, from the caller.
    timeout: Duration,
    /// Headers, every one of them destined for the stdin config.
    headers: Vec<(String, String)>,
    /// `application/x-www-form-urlencoded` fields, likewise.
    form: Vec<(String, String)>,
    body: Body,
    /// Where curl dumps the response headers, when a caller asked for them. `None` (the
    /// default) means headers are not captured at all and nothing extra is written.
    header_file: Option<PathBuf>,
    /// Whether a failure that could plausibly succeed on a second attempt gets one.
    retry: bool,
}

impl CurlReq {
    /// Start a request to `url`, which must be https and must name a host in
    /// `allowed_hosts` (in practice [`crate::cloud::registry_hosts`]).
    ///
    /// `timeout` is the WHOLE budget for the request and has no default on purpose: see
    /// the module doc.
    pub fn new(
        method: Method,
        url: &str,
        allowed_hosts: &[&str],
        timeout: Duration,
    ) -> Result<CurlReq, String> {
        let host = check_url(url, allowed_hosts)?;
        if timeout.is_zero() {
            return Err("A cloud request needs a time budget.".to_string());
        }
        Ok(CurlReq {
            method,
            url: url.to_string(),
            host,
            timeout,
            headers: Vec::new(),
            form: Vec::new(),
            body: Body::None,
            header_file: None,
            retry: true,
        })
    }

    /// Add a request header. It travels in the stdin config, always.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// Add a form field, url-encoded by curl. It travels in the stdin config, always.
    pub fn form_field(mut self, name: &str, value: &str) -> Self {
        self.form.push((name.to_string(), value.to_string()));
        self
    }

    /// Set the request body.
    pub fn body(mut self, body: Body) -> Self {
        self.body = body;
        self
    }

    /// Turn the single retry off, for a request that must not be repeated even when
    /// repeating it looks safe (anything that is not idempotent server-side).
    pub fn no_retry(mut self) -> Self {
        self.retry = false;
        self
    }

    /// Also capture the RESPONSE headers, readable afterwards with
    /// [`CurlResponse::header`] (DRAGON-482).
    ///
    /// Opt-in, and off by default, because almost nothing needs it: an API that answers in
    /// JSON says everything in the body. The exception that forced this is Google Drive's
    /// resumable upload, which returns its session URI ONLY in the `Location` header, and
    /// its per-chunk progress only in `Range`.
    ///
    /// # Why `-D <file>` and not `--write-out '%header{...}'`
    ///
    /// `%header{}` would be tidier: one more `write-out` field, no file, no cleanup. It
    /// needs **curl 7.83** (March 2022), and this app ships where that is not a safe
    /// assumption. Windows 10 bundles curl **7.55**, Ubuntu 22.04 LTS (a target this repo
    /// names explicitly, see CLAUDE.md's feature-flag section) ships **7.81**, and on those
    /// the field is not expanded, so the failure would be a silently empty header rather
    /// than an error: an upload that mysteriously cannot start, on exactly the machines
    /// least likely to be in front of a developer. `-D` has been in curl since long before
    /// any version we could meet, and it captures EVERY header rather than one named in
    /// advance.
    ///
    /// The dump file is written into the session runtime dir, is deleted on every path out
    /// of [`Self::send`] (including the error and panic-free early returns), and is never
    /// logged. Its PATH lands in argv, which is fine and deliberate for the same reason
    /// `-o` and `--data-binary @` already do: a path is not a credential. **The header
    /// VALUES can be**, so treat them as secret: a Drive session URI is a capability to
    /// write to the user's drive.
    pub fn capture_headers(mut self) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        /// Keeps two concurrent requests from sharing one dump file.
        static SEQ: AtomicU64 = AtomicU64::new(0);
        self.header_file = Some(PathBuf::from(crate::util::runtime_dir()).join(format!(
            "cck-cloud-head-{}-{}.txt",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        )));
        self
    }

    /// The exact argument vector handed to curl. Pure; unit-tested, and the test that
    /// matters is [`argv_carries_no_secret`](self).
    ///
    /// Only the method, the timeouts, the file paths, and `--config -`. Every other option,
    /// including the URL, all headers and all form fields, is in the config.
    pub fn argv(&self) -> Vec<String> {
        let secs = self.timeout.as_secs().max(1);
        let mut argv = vec![
            "-X".to_string(),
            self.method.as_str().to_string(),
            "--max-time".to_string(),
            secs.to_string(),
            "--connect-timeout".to_string(),
            connect_timeout_secs(secs).to_string(),
        ];
        if let Some(dump) = &self.header_file {
            argv.push("-D".to_string());
            argv.push(dump.to_string_lossy().into_owned());
        }
        if let Body::File(p) = &self.body {
            argv.push("--data-binary".to_string());
            argv.push(format!("@{}", p.to_string_lossy()));
        }
        // Last, so it reads as "and the rest comes from stdin".
        argv.push("--config".to_string());
        argv.push("-".to_string());
        argv
    }

    /// The curl config fed on stdin: every header, every form field, the text body, and
    /// the options that keep curl quiet and make it report its status code. Pure;
    /// unit-tested.
    ///
    /// `write-out = "%{http_code}"` is what makes [`split_status`] work: curl appends the
    /// final status to stdout, so one capture carries both the body and the code. `-f` is
    /// deliberately NOT used: it collapses every HTTP error into one exit code, and this
    /// client has to tell a 401 (refresh the token) from a 429 (back off) from a 500
    /// (retry).
    ///
    /// The `url` directive is here rather than in argv so a pre-authorized URL (Google's
    /// `upload_id`, Microsoft's `tempauth`) is not readable from `/proc/<pid>/cmdline`; see
    /// the module doc. [`check_url`] has already refused any URL carrying a control
    /// character, and [`config_escape`] handles the rest, so the directive cannot be broken
    /// out of.
    pub fn config(&self) -> String {
        let mut out = String::new();
        out.push_str("silent\n");
        out.push_str("show-error\n");
        out.push_str("write-out = \"%{http_code}\"\n");
        out.push_str(&format!("url = \"{}\"\n", config_escape(&self.url)));
        for (name, value) in &self.headers {
            out.push_str(&format!("header = \"{}\"\n", config_escape(&format!("{name}: {value}"))));
        }
        for (name, value) in &self.form {
            out.push_str(&format!(
                "data-urlencode = \"{}\"\n",
                config_escape(&format!("{name}={value}"))
            ));
        }
        if let Body::Text(text) = &self.body {
            out.push_str(&format!("data = \"{}\"\n", config_escape(text)));
        }
        out
    }

    /// Send the request, blocking, with at most one retry (see [`should_retry`]).
    ///
    /// Never call this from the UI thread: the budget is seconds, not milliseconds.
    pub fn send(&self) -> Result<CurlResponse, String> {
        let first = self.send_once()?;
        if self.retry && should_retry(first.status) {
            log::debug!(
                "cloud http: {} {} came back {}; retrying once",
                self.method.as_str(),
                self.host,
                first.status
            );
            return self.send_once();
        }
        Ok(first)
    }

    /// One attempt. Split out so [`Self::send`] holds the retry RULE and nothing else.
    fn send_once(&self) -> Result<CurlResponse, String> {
        // Armed BEFORE curl is spawned, so every way out of this function removes the dump:
        // the two early error returns, the no-status return, and the success path (which has
        // already read and removed it, making this a no-op). A header dump left behind would
        // be a session URI sitting in the runtime dir.
        let _dump = HeaderDumpGuard(self.header_file.as_ref());
        if let Some(path) = &self.header_file {
            create_header_dump(path)?;
        }
        // Quiet spawn (DRAGON-236): curl is console-subsystem, so a bare spawn flashes a
        // console window on Windows. Byte-identical off Windows.
        let child = crate::util::quiet_command("curl")
            .args(self.argv())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("The curl program could not be started: {e}"))?;
        let deadline = Instant::now() + self.timeout + REAP_GRACE;
        let out = run_bounded(child, self.config().as_bytes(), deadline)?;
        if out.killed {
            log::warn!(
                "cloud http: {} {} outlived its {}s budget and was stopped",
                self.method.as_str(),
                self.host,
                self.timeout.as_secs()
            );
            return Err("The cloud service stopped answering, so the request was ended."
                .to_string());
        }
        let Some((body, status)) = split_status(&out.stdout) else {
            // No status means curl never got far enough to report one (it could not start
            // the transfer at all). Its stderr is the only thing that says why, and it can
            // name a path, so it goes through the redactors before anywhere near a log.
            let why = crate::diag::redact_oauth(&crate::diag::redact_paths(
                &String::from_utf8_lossy(&out.stderr),
            ));
            let why = why.trim();
            log::warn!("cloud http: {} {} produced no status ({why})", self.method.as_str(), self.host);
            return Err("The cloud service could not be reached.".to_string());
        };
        log::debug!("cloud http: {} {} -> {status}", self.method.as_str(), self.host);
        Ok(CurlResponse { status, body, headers: self.take_headers() })
    }

    /// Read and REMOVE the header dump, if one was asked for.
    ///
    /// Removed unconditionally, whether or not the read worked, so a failed request cannot
    /// leave a file behind. Nothing here is logged: a captured header can be a credential.
    fn take_headers(&self) -> Vec<(String, String)> {
        let Some(path) = &self.header_file else { return Vec::new() };
        let dump = std::fs::read_to_string(path).unwrap_or_default();
        let _ = std::fs::remove_file(path);
        parse_headers(&dump)
    }
}

/// Removes a header dump file on every path out of a send. See [`CurlReq::send_once`].
struct HeaderDumpGuard<'a>(Option<&'a PathBuf>);

impl Drop for HeaderDumpGuard<'_> {
    fn drop(&mut self) {
        if let Some(path) = self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Create the header dump file OURSELVES, owner-only, before curl is spawned (DRAGON-482).
///
/// curl's `-D` truncates a file that already exists rather than replacing it, so the mode set
/// here is the mode the dump keeps for its whole life. That matters because the dump holds
/// response headers, and a response header can be a credential: Google's `Location` on a
/// resumable upload authorizes writing to the user's drive. Letting curl create the file would
/// give it the umask's mode, which on a default umask is world-readable.
///
/// `create_new` is the other half. It refuses if the path exists AT ALL, including as a
/// symlink, so nothing that can write to the runtime directory can plant a link and have curl
/// truncate whatever it points at. A stale file from a previous process that happened to reuse
/// our pid is removed first; the `create_new` still refuses to follow anything that reappears
/// in between, so the removal cannot open a hole.
///
/// The mode is set on unix only. On Windows the file inherits the ACL of the per-user runtime
/// directory, which is not world-readable to begin with, so `create_new` alone is the whole
/// job there.
fn create_header_dump(path: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    opts.open(path).map(drop).map_err(|e| {
        format!(
            "A temporary file for the cloud reply could not be created ({}): {e}",
            crate::diag::path_shape(path)
        )
    })
}

/// What a bounded curl run produced.
struct CurlOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// Whether the deadline expired and curl had to be killed. A killed curl is a TRANSPORT
    /// failure, never a status: whatever is in `stdout` is a partial answer at best.
    killed: bool,
}

/// Feed curl its config, drain both pipes, and reap it against `deadline` (DRAGON-482).
///
/// Modelled on [`crate::record::wait_or_kill`], and here for the same reason DRAGON-118 put it
/// there: nothing may wait unboundedly. `--max-time` is supposed to end the transfer on its
/// own, so this is the ring OUTSIDE that one, for a curl that is not watching its own clock (a
/// wedged resolver, a TLS handshake stuck in a library that never checks the timer). Without
/// it one such curl hangs an upload child until its 30-minute backstop.
///
/// The two pipes are drained on their OWN threads, started before the reap loop. A child that
/// has filled its stdout pipe blocks in `write` and never exits, so a `try_wait` loop over an
/// undrained pipe would turn every deadline into the full grace period and then a kill.
fn run_bounded(
    mut child: std::process::Child,
    config: &[u8],
    deadline: Instant,
) -> Result<CurlOutput, String> {
    // Write the config and CLOSE the pipe: curl reads the config to EOF before it does
    // anything, so a stdin left open is a hang. Best effort on the write itself, since a child
    // that died early gives EPIPE and its exit status is the better diagnosis.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(config);
    }
    let stdout_reader = child.stdout.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });
    let stderr_reader = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });

    let mut killed = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(e) => return Err(format!("The curl program could not be run: {e}")),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            killed = true;
            break;
        }
        std::thread::sleep(REAP_POLL);
    }
    // Joined AFTER the child is gone, so both reads have hit EOF and neither join can block.
    let join = |reader: Option<std::thread::JoinHandle<Vec<u8>>>| {
        reader.and_then(|h| h.join().ok()).unwrap_or_default()
    };
    Ok(CurlOutput { stdout: join(stdout_reader), stderr: join(stderr_reader), killed })
}

/// What came back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurlResponse {
    /// The final HTTP status, or 0 when curl could not complete the transfer.
    pub status: u16,
    /// The response body.
    pub body: Vec<u8>,
    /// The response headers, in the order they arrived, and EMPTY unless the request asked
    /// for them with [`CurlReq::capture_headers`]. Read them with [`Self::header`] rather
    /// than by scanning, so the case-insensitivity is not re-implemented per call site.
    pub headers: Vec<(String, String)>,
}

impl CurlResponse {
    /// Whether the status is a 2xx. Pure; unit-tested.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// One response header by name, case-insensitively. Pure; unit-tested.
    ///
    /// The FIRST match, which is what every header this app reads has exactly one of
    /// (`Location`, `Range`). A header legitimately sent more than once (`Set-Cookie`) is
    /// still all present in [`Self::headers`] for a caller that wants them.
    ///
    /// **A header value can be a credential.** Google's `Location` on a resumable upload is
    /// a URL that authorizes writing to the user's drive. Never log one.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The body as text, lossily. Callers parse JSON from this. Pure; unit-tested.
    ///
    /// Lossy rather than fallible: a provider reply is JSON in practice, and a body that is
    /// not valid UTF-8 is one the JSON parse is going to reject anyway. Turning it into an
    /// error here would only move the same failure one step earlier with less to say.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// The hostname of an `https://` URL, or `None` for anything else. Pure; unit-tested.
///
/// `None` for a non-https scheme, for an empty host, and for an authority carrying
/// userinfo (`https://a@b/`). Userinfo is REFUSED rather than parsed: it has no legitimate
/// use here, it is the classic way to make a URL read as one host and resolve to another,
/// and refusing it costs nothing.
pub fn host_of(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    // A port is not part of the host for allowlist purposes; the scheme pins 443 anyway.
    let host = authority.split(':').next().unwrap_or("");
    if host.is_empty() { None } else { Some(host) }
}

/// Check a URL against the allowlist and return its host. Pure; unit-tested.
///
/// The failure messages are user-facing (they can reach a toast) and deliberately do NOT
/// echo the URL back: a rejected URL is the one most likely to be worth not repeating.
pub fn check_url(url: &str, allowed_hosts: &[&str]) -> Result<String, String> {
    if url.chars().any(|c| c.is_control() || c == ' ') {
        return Err("That cloud address is not valid.".to_string());
    }
    let host = host_of(url).ok_or_else(|| {
        "Cloud requests can only be made over a secure https address.".to_string()
    })?;
    let allowed = allowed_hosts
        .iter()
        .any(|h| h.eq_ignore_ascii_case(host) && !h.is_empty());
    if !allowed {
        return Err("That cloud address is not one this app is allowed to contact.".to_string());
    }
    Ok(host.to_ascii_lowercase())
}

/// Escape a value for a curl config double-quoted string. Pure; unit-tested.
///
/// curl's config parser understands `\\`, `\"`, `\t`, `\n`, `\r` and `\v` inside double
/// quotes. Everything else is literal. A newline that reached the config unescaped would
/// end the line and let the rest be read as another config DIRECTIVE, which is the one
/// injection this file has to be careful about, so it is escaped rather than stripped.
pub fn config_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{0b}' => out.push_str("\\v"),
            other => out.push(other),
        }
    }
    out
}

/// Split curl's stdout into the response body and the trailing status code that
/// `write-out = "%{http_code}"` appends. Pure; unit-tested.
///
/// `%{http_code}` is always exactly three ASCII digits, including `000` when the transfer
/// never produced a response, so the split is by LENGTH and cannot be confused by a body
/// that happens to end in digits.
pub fn split_status(stdout: &[u8]) -> Option<(Vec<u8>, u16)> {
    if stdout.len() < 3 {
        return None;
    }
    let (body, tail) = stdout.split_at(stdout.len() - 3);
    if !tail.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let status: u16 = std::str::from_utf8(tail).ok()?.parse().ok()?;
    Some((body.to_vec(), status))
}

/// Parse a curl `-D` header dump into name/value pairs. Pure; unit-tested.
///
/// The dump is the raw header block(s) curl received, so this has to survive more than one
/// response in a single file. That is not hypothetical: curl sends `Expect: 100-continue`
/// for a large body, so an upload's dump routinely begins with a whole `HTTP/1.1 100
/// Continue` block before the real one. **The LAST status line wins**: every `HTTP/`
/// line clears what came before it, so the headers returned always belong to the final
/// response. Getting that backwards would read `Location` out of an interim block that does
/// not have one.
///
/// Also handled, because a dump is real network input and not our own output:
///
/// * **Obs-fold continuation lines** (a header wrapped onto a following line starting with a
///   space or tab) are joined onto the previous value with a single space. Deprecated by RFC
///   7230 and still emitted by older servers and proxies.
/// * **Repeated names** are all kept, in order; [`CurlResponse::header`] takes the first.
/// * A line with no colon is skipped rather than guessed at.
///
/// Names and values are trimmed but NOT lower-cased: the original casing is what a log or a
/// debug view should show, and the case-insensitivity lives in the lookup instead.
pub fn parse_headers(dump: &str) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = Vec::new();
    for raw in dump.lines() {
        // `lines()` splits on \n; the \r of a CRLF pair is still on the end.
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.is_empty() {
            continue;
        }
        if line.starts_with("HTTP/") {
            // A new response begins. Anything gathered so far belonged to an interim one.
            headers.clear();
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some((_, value)) = headers.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    headers
}

/// Whether a request that came back with `status` gets its one retry. Pure; unit-tested.
///
/// Retry a transport failure (`0`, curl's `000`: a dropped connection, a DNS blip, a
/// timeout) and any 5xx, because both are conditions on the far side that a second attempt
/// can genuinely find changed.
///
/// Never a 4xx, INCLUDING 429. A 4xx is a decision the server made about this exact
/// request: repeating it cannot change the answer, and repeating a 401 or a 429 is how an
/// account gets rate-limited or locked. Backing off from a 429 is a scheduling decision for
/// the caller, not a retry.
pub fn should_retry(status: u16) -> bool {
    status == 0 || status >= 500
}

/// The connect budget derived from the total budget. Pure; unit-tested.
///
/// Capped at 10s so a long total budget (an upload) still fails FAST when the host is
/// simply unreachable, and floored at 1s so a short budget still has time to connect.
pub fn connect_timeout_secs(total_secs: u64) -> u64 {
    total_secs.clamp(1, 10)
}

#[cfg(test)]
mod url_tests {
    use super::*;

    #[test]
    fn only_https_urls_have_a_host() {
        assert_eq!(host_of("https://oauth2.example.com/token"), Some("oauth2.example.com"));
        assert_eq!(host_of("https://api.example.com"), Some("api.example.com"));
        assert_eq!(host_of("https://api.example.com:8443/v1"), Some("api.example.com"));
        assert_eq!(host_of("https://api.example.com?a=b"), Some("api.example.com"));
        // Not https, no host: plain http, a file, and the empty placeholder the registry
        // carries until stage A2 fills the endpoints in.
        assert_eq!(host_of("http://api.example.com/"), None);
        assert_eq!(host_of("file:///etc/passwd"), None);
        assert_eq!(host_of(""), None);
        assert_eq!(host_of("https://"), None);
        assert_eq!(host_of("https:///path"), None);
    }

    /// The spoofing shape: `https://<allowed>@<attacker>/` reads as the allowed host and
    /// resolves to the attacker. Refused outright rather than parsed.
    #[test]
    fn userinfo_is_refused_not_parsed() {
        assert_eq!(host_of("https://accounts.google.com@evil.example/"), None);
        assert_eq!(host_of("https://user:pass@api.example.com/"), None);
        assert!(check_url("https://good.example@evil.example/x", &["good.example"]).is_err());
    }

    #[test]
    fn the_allowlist_is_the_only_way_through() {
        let allowed = ["api.example.com", "oauth2.example.com"];
        assert_eq!(check_url("https://api.example.com/v1/files", &allowed), Ok("api.example.com".into()));
        // Case-insensitive, as hostnames are.
        assert_eq!(check_url("https://API.Example.COM/v1", &allowed), Ok("api.example.com".into()));
        // A host that merely LOOKS like an allowed one is not one.
        for bad in [
            "https://api.example.com.evil.test/v1",
            "https://evil.test/api.example.com",
            "https://sub.api.example.com/v1",
            "http://api.example.com/v1",
        ] {
            assert!(check_url(bad, &allowed).is_err(), "{bad} must be refused");
        }
        // An EMPTY allowlist (the state until stage A2 fills the endpoints in) refuses
        // everything, which is the honest answer rather than a hole.
        assert!(check_url("https://api.example.com/v1", &[]).is_err());
        // And an empty entry can never match an empty-ish host.
        assert!(check_url("https://api.example.com/v1", &[""]).is_err());
    }

    /// A URL is never echoed back in a rejection: the query string is where ids and codes
    /// live, and an error message reaches a toast.
    #[test]
    fn a_rejection_never_quotes_the_url() {
        let err = check_url("https://evil.example/steal?code=SECRET123", &["ok.example"])
            .expect_err("refused");
        assert!(!err.contains("SECRET123"), "the reason leaked the query: {err}");
        assert!(!err.contains("evil.example"), "the reason leaked the host: {err}");
    }

    /// A control character in a URL would end the argument (and, if a URL ever reached the
    /// config, the config line). Refused up front.
    #[test]
    fn control_characters_are_refused() {
        assert!(check_url("https://ok.example/a\nheader = \"x\"", &["ok.example"]).is_err());
        assert!(check_url("https://ok.example/a b", &["ok.example"]).is_err());
    }
}

#[cfg(test)]
mod argv_tests {
    use super::*;

    fn req() -> CurlReq {
        CurlReq::new(
            Method::Post,
            "https://oauth2.example.com/token",
            &["oauth2.example.com"],
            Duration::from_secs(30),
        )
        .expect("an allowed url")
    }

    /// **THE privacy test for this module.** Every way a caller can hand this client
    /// something secret is exercised at once, and none of it may appear in argv. The
    /// marker is a single distinctive token so a leak through ANY of the five routes fails
    /// here rather than in production.
    #[test]
    fn argv_carries_no_secret() {
        const MARKER: &str = "cck-secret-marker-77f3";
        let url = format!("https://oauth2.example.com/token?upload_id={MARKER}");
        let r = CurlReq::new(Method::Post, &url, &["oauth2.example.com"], Duration::from_secs(30))
            .expect("an allowed url")
            .header("Authorization", &format!("Bearer {MARKER}"))
            .header("X-Api-Key", MARKER)
            .form_field("refresh_token", MARKER)
            .form_field("code_verifier", MARKER)
            .body(Body::Text(format!("{{\"access_token\":\"{MARKER}\"}}")));
        let argv = r.argv().join(" ");
        assert!(!argv.contains(MARKER), "a secret reached argv: {argv}");
        // The URL is a secret carrier too (DRAGON-482): a session URI IS a capability. Not
        // one byte of it may be in argv, so the whole path is asserted absent, not just the
        // marker inside it.
        assert!(!argv.contains("oauth2.example.com"), "the url reached argv: {argv}");
        assert!(!argv.contains("/token"), "the url path reached argv: {argv}");
        // …and it really is being sent, through the config, so this is not passing by
        // simply dropping the values.
        let config = r.config();
        assert_eq!(config.matches(MARKER).count(), 6, "every secret must ride the config");
        assert!(config.contains("header = \"Authorization: Bearer "));
        assert!(config.contains("data-urlencode = \"refresh_token="));
        assert!(config.contains("url = \"https://oauth2.example.com/token?upload_id="));
    }

    /// **The pre-authorized-URL case, stated on its own.** Google's resumable session URI
    /// carries an `upload_id` that authorizes writing to the user's drive, and Microsoft's
    /// carries a `tempauth` token. Either in argv is a credential readable from
    /// `/proc/<pid>/cmdline` for the life of a multi-minute upload.
    #[test]
    fn a_session_uri_never_reaches_argv() {
        for url in [
            "https://oauth2.example.com/upload/drive/v3/files?uploadType=resumable&upload_id=AEnB2Uo-SECRET",
            "https://oauth2.example.com/v1/uploads/01ABC?tempauth=eyJ0eXAiOiJKV1QSECRET",
        ] {
            let r = CurlReq::new(
                Method::Put,
                url,
                &["oauth2.example.com"],
                Duration::from_secs(60),
            )
            .expect("an allowed url");
            let argv = r.argv().join(" ");
            assert!(!argv.contains("SECRET"), "a session URI reached argv: {argv}");
            assert!(!argv.contains("upload"), "a session URI reached argv: {argv}");
            // And it is still what curl fetches, through the config.
            assert!(r.config().contains(&format!("url = \"{url}\"")), "{}", r.config());
        }
    }

    /// The argv shape itself: method, timeouts, `--config -`. Anything else added here is a
    /// new chance for a secret to escape, so the list is asserted exactly.
    #[test]
    fn argv_is_only_the_method_timeouts_and_config() {
        let argv = req().argv();
        assert_eq!(
            argv,
            vec!["-X", "POST", "--max-time", "30", "--connect-timeout", "10", "--config", "-"]
        );
    }

    /// The one thing that legitimately puts a PATH in argv: a file body. A path is not a
    /// credential, and this is what keeps an upload payload out of the stdin pipe.
    #[test]
    fn a_file_body_is_the_only_other_argv_entry() {
        let r = req().body(Body::File(PathBuf::from("/tmp/cck-capture.png")));
        let argv = r.argv();
        assert!(argv.windows(2).any(|w| w == ["--data-binary", "@/tmp/cck-capture.png"]));
        // A file body is NOT also written into the config: it would defeat the point.
        assert!(!r.config().contains("data ="));
    }

    /// A caller must state a budget, and it reaches curl as the bound that enforces it.
    #[test]
    fn every_request_carries_its_budget() {
        assert!(
            CurlReq::new(Method::Get, "https://ok.example/x", &["ok.example"], Duration::ZERO)
                .is_err(),
            "a request with no budget must be refused"
        );
        let r = CurlReq::new(
            Method::Get,
            "https://ok.example/x",
            &["ok.example"],
            Duration::from_secs(600),
        )
        .expect("allowed");
        let argv = r.argv();
        assert!(argv.windows(2).any(|w| w == ["--max-time", "600"]));
        // A long total budget still fails fast on an unreachable host.
        assert!(argv.windows(2).any(|w| w == ["--connect-timeout", "10"]));
    }

    #[test]
    fn the_connect_budget_is_clamped_at_both_ends() {
        assert_eq!(connect_timeout_secs(1), 1);
        assert_eq!(connect_timeout_secs(5), 5);
        assert_eq!(connect_timeout_secs(10), 10);
        assert_eq!(connect_timeout_secs(600), 10);
        assert_eq!(connect_timeout_secs(0), 1);
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;
    use rstest::rstest;

    /// A newline in a header value would end the config line and let the remainder be read
    /// as a fresh curl DIRECTIVE (its own `-o`, its own `url`). Escaped, not stripped, so
    /// the value still arrives intact.
    #[test]
    fn a_newline_cannot_inject_a_config_directive() {
        let injected = "Bearer x\noutput = \"/tmp/stolen\"\n";
        let out = config_escape(injected);
        assert!(!out.contains('\n'), "a raw newline survived: {out:?}");
        assert!(out.contains("\\n"));
        assert!(out.contains("output = \\\"/tmp/stolen\\\""), "the value must survive: {out}");
    }

    /// Every character curl's config parser treats specially inside double quotes, one case
    /// each. A table, because the failure mode of a missed one is silent: the value arrives
    /// mangled and the request fails at the provider with nothing to point at.
    #[rstest]
    #[case(r#"a"b"#, r#"a\"b"#)]
    #[case(r"a\b", r"a\\b")]
    #[case("a\tb", r"a\tb")]
    #[case("a\rb", r"a\rb")]
    #[case("a\nb", r"a\nb")]
    #[case("a\u{0b}b", r"a\vb")]
    #[case("plain", "plain")]
    #[case("", "")]
    fn quotes_and_backslashes_escape(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(config_escape(input), expected);
    }

    /// The options that make the response readable: quiet output, the URL, and the status
    /// code appended to stdout. Losing `write-out` would make every status parse fail, and
    /// losing `url` would make curl fetch nothing at all.
    #[test]
    fn the_config_always_asks_for_the_status_code() {
        let r = CurlReq::new(Method::Get, "https://ok.example/x", &["ok.example"], Duration::from_secs(5))
            .expect("allowed");
        let c = r.config();
        assert!(c.contains("write-out = \"%{http_code}\""));
        assert!(c.contains("url = \"https://ok.example/x\""));
        assert!(c.contains("silent\n"));
        assert!(c.contains("show-error\n"));
        // No `-f`: this client has to tell a 401 from a 429 from a 500.
        assert!(!c.contains("fail"));
    }
}

#[cfg(test)]
mod response_tests {
    use super::*;

    #[test]
    fn the_status_splits_off_the_end_of_stdout() {
        assert_eq!(split_status(b"{\"ok\":true}200"), Some((b"{\"ok\":true}".to_vec(), 200)));
        // A body that itself ends in digits: the split is by length, so it is unaffected.
        assert_eq!(split_status(b"12345404"), Some((b"12345".to_vec(), 404)));
        // No body at all (a 204).
        assert_eq!(split_status(b"204"), Some((Vec::new(), 204)));
        // curl could not complete the transfer.
        assert_eq!(split_status(b"000"), Some((Vec::new(), 0)));
        // Nothing usable.
        assert_eq!(split_status(b""), None);
        assert_eq!(split_status(b"ab"), None);
        assert_eq!(split_status(b"body-with-no-code"), None);
    }

    /// The retry rule, stated as the property it protects: a 4xx is never repeated.
    #[test]
    fn only_a_transport_failure_or_a_5xx_is_retried() {
        assert!(should_retry(0), "a dead transport is worth one more try");
        for s in [500, 502, 503, 504] {
            assert!(should_retry(s), "{s} must retry");
        }
        for s in [200, 201, 204, 301, 400, 401, 403, 404, 409, 413, 429] {
            assert!(!should_retry(s), "{s} must NOT retry");
        }
    }

    #[test]
    fn success_is_exactly_the_2xx_band() {
        for s in [200, 201, 204, 299] {
            assert!(response(s).is_success());
        }
        for s in [0, 199, 300, 400, 500] {
            assert!(!response(s).is_success());
        }
    }

    /// The method names are the wire spelling `-X` takes, asserted exactly: a typo here
    /// fails at the provider with a message about an unsupported method.
    #[test]
    fn every_method_spells_its_wire_name() {
        assert_eq!(Method::Get.as_str(), "GET");
        assert_eq!(Method::Post.as_str(), "POST");
        assert_eq!(Method::Put.as_str(), "PUT");
        assert_eq!(Method::Delete.as_str(), "DELETE");
        for m in [Method::Get, Method::Post, Method::Put, Method::Delete] {
            assert!(m.as_str().chars().all(|c| c.is_ascii_uppercase()), "{m:?}");
        }
    }

    /// The body reads back as text, and a body that is not valid UTF-8 comes back lossily
    /// rather than panicking or erroring: this is real network input.
    #[test]
    fn the_body_reads_back_as_text_however_bad_it_is() {
        let ok = CurlResponse { status: 200, body: b"{\"a\":1}".to_vec(), headers: Vec::new() };
        assert_eq!(ok.text(), "{\"a\":1}");
        assert_eq!(response(204).text(), "", "an empty body is an empty string");
        let bad = CurlResponse { status: 200, body: vec![0xff, 0xfe, b'x'], headers: Vec::new() };
        assert!(bad.text().ends_with('x'), "the readable tail must survive: {:?}", bad.text());
    }

    /// A response with no captured headers, for the status-only cases.
    fn response(status: u16) -> CurlResponse {
        CurlResponse { status, body: Vec::new(), headers: Vec::new() }
    }
}

#[cfg(test)]
mod header_capture_tests {
    use super::*;

    /// **The case that forced this feature.** Google's resumable initiation replies with a
    /// `100 Continue` block first (curl sends `Expect: 100-continue` for a large body), then
    /// the real response. The `Location` must come from the LAST block; reading the first
    /// would find no session URI at all.
    #[test]
    fn the_last_response_block_wins() {
        let dump = "HTTP/1.1 100 Continue\r\n\r\n\
                    HTTP/1.1 200 OK\r\n\
                    Location: https://www.googleapis.com/upload/drive/v3/files?upload_id=ABC123\r\n\
                    Content-Type: application/json\r\n\r\n";
        let headers = parse_headers(dump);
        assert_eq!(headers.len(), 2, "only the final block's headers: {headers:?}");
        let response = CurlResponse { status: 200, body: Vec::new(), headers };
        assert_eq!(
            response.header("Location"),
            Some("https://www.googleapis.com/upload/drive/v3/files?upload_id=ABC123")
        );
    }

    /// An interim block carrying a header of its own must not leak into the final answer.
    #[test]
    fn an_interim_blocks_headers_are_discarded() {
        let dump = "HTTP/1.1 100 Continue\r\n\
                    Location: https://interim.example/wrong\r\n\r\n\
                    HTTP/1.1 308 Resume Incomplete\r\n\
                    Range: bytes=0-8388607\r\n\r\n";
        let response =
            CurlResponse { status: 308, body: Vec::new(), headers: parse_headers(dump) };
        assert_eq!(response.header("location"), None, "the interim header survived");
        assert_eq!(response.header("Range"), Some("bytes=0-8388607"));
    }

    /// Lookup is case-insensitive in both directions, because a server picks its own casing
    /// and HTTP says it does not matter.
    #[test]
    fn header_lookup_ignores_case() {
        let dump = "HTTP/2 200\r\nlOcAtIoN: https://x.example/s\r\nX-Goog-Thing: 7\r\n\r\n";
        let response =
            CurlResponse { status: 200, body: Vec::new(), headers: parse_headers(dump) };
        for spelling in ["Location", "location", "LOCATION", "LoCaTiOn"] {
            assert_eq!(response.header(spelling), Some("https://x.example/s"), "{spelling}");
        }
        assert_eq!(response.header("x-goog-thing"), Some("7"));
        assert_eq!(response.header("Missing"), None);
        // The original casing is preserved in the list itself.
        assert_eq!(response.headers[0].0, "lOcAtIoN");
    }

    /// Obs-fold continuation lines are joined rather than dropped or treated as a header of
    /// their own. Deprecated, still emitted by older proxies.
    #[test]
    fn folded_header_lines_are_joined() {
        let dump = "HTTP/1.1 200 OK\r\n\
                    X-Long: first part\r\n\
                    \tsecond part\r\n\
                    \x20 third part\r\n\
                    Content-Type: text/plain\r\n\r\n";
        let headers = parse_headers(dump);
        assert_eq!(headers.len(), 2, "a folded line is not its own header: {headers:?}");
        assert_eq!(headers[0], ("X-Long".to_string(), "first part second part third part".to_string()));
        assert_eq!(headers[1].0, "Content-Type");
        // A fold with nothing to attach to is ignored rather than panicking.
        assert_eq!(parse_headers("HTTP/1.1 200 OK\r\n  orphan\r\n"), Vec::new());
    }

    /// A repeated header keeps every value in order, and the lookup takes the first.
    #[test]
    fn a_repeated_header_keeps_every_value() {
        let dump = "HTTP/1.1 200 OK\r\n\
                    Set-Cookie: a=1\r\n\
                    Set-Cookie: b=2\r\n\r\n";
        let response =
            CurlResponse { status: 200, body: Vec::new(), headers: parse_headers(dump) };
        assert_eq!(response.headers.len(), 2);
        assert_eq!(response.header("set-cookie"), Some("a=1"), "the first match");
        let all: Vec<&str> = response
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(all, vec!["a=1", "b=2"]);
    }

    /// Junk is skipped, not guessed at. A dump is real network input.
    #[test]
    fn a_malformed_dump_yields_nothing_usable() {
        assert_eq!(parse_headers(""), Vec::new());
        assert_eq!(parse_headers("HTTP/1.1 204 No Content\r\n\r\n"), Vec::new());
        // A line with no colon is not a header.
        assert_eq!(parse_headers("HTTP/1.1 200 OK\r\ngarbage line\r\n"), Vec::new());
        // A value may be empty, and a value may itself contain colons (a URL).
        let headers = parse_headers("HTTP/1.1 200 OK\r\nEmpty:\r\nUrl: https://a.test:8443/x\r\n");
        assert_eq!(headers[0], ("Empty".to_string(), String::new()));
        assert_eq!(headers[1].1, "https://a.test:8443/x");
        // Bare LF, as a lenient server might send.
        assert_eq!(parse_headers("HTTP/1.1 200 OK\nA: b\n").len(), 1);
    }

    /// Capturing is OPT-IN: an ordinary request adds nothing to argv and reports no headers,
    /// so nothing that already exists changes behaviour.
    #[test]
    fn capturing_is_opt_in_and_adds_exactly_one_argv_pair() {
        let plain = CurlReq::new(
            Method::Post,
            "https://ok.example/x",
            &["ok.example"],
            Duration::from_secs(5),
        )
        .expect("allowed");
        assert!(!plain.argv().contains(&"-D".to_string()), "no dump without asking");

        let capturing = plain.clone().capture_headers();
        let argv = capturing.argv();
        let at = argv.iter().position(|a| a == "-D").expect("the dump flag");
        // The path is in argv, which is fine: a path is not a credential. The VALUES are.
        assert!(argv[at + 1].contains("cck-cloud-head-"), "{:?}", argv[at + 1]);
        // And nothing else about the request moved.
        assert_eq!(capturing.config(), plain.config());
        assert_eq!(argv.len(), plain.argv().len() + 2);
    }

    /// Two capturing requests never share a dump file, so concurrent uploads cannot read
    /// each other's headers.
    #[test]
    fn each_capturing_request_gets_its_own_dump_file() {
        let req = || {
            CurlReq::new(Method::Get, "https://ok.example/x", &["ok.example"], Duration::from_secs(5))
                .expect("allowed")
                .capture_headers()
        };
        let (a, b) = (req(), req());
        assert_ne!(a.header_file, b.header_file);
    }

    /// The dump is removed even when it is never read, which is what the guard is for.
    #[test]
    fn the_dump_file_is_always_cleaned_up() {
        let req = CurlReq::new(
            Method::Get,
            "https://ok.example/x",
            &["ok.example"],
            Duration::from_secs(5),
        )
        .expect("allowed")
        .capture_headers();
        let path = req.header_file.clone().expect("a dump path");
        std::fs::write(&path, "HTTP/1.1 200 OK\r\nA: b\r\n").expect("write a dump");
        // Reading it takes it away.
        assert_eq!(req.take_headers(), vec![("A".to_string(), "b".to_string())]);
        assert!(!path.exists(), "take_headers left the dump behind");
        // And a request that never ran leaves nothing either.
        std::fs::write(&path, "HTTP/1.1 200 OK\r\n").expect("write a dump");
        drop(HeaderDumpGuard(Some(&path)));
        assert!(!path.exists(), "the guard left the dump behind");
        // A missing dump is not an error.
        assert_eq!(req.take_headers(), Vec::new());
    }
}
