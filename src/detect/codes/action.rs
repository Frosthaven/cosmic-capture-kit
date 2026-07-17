//! The click-action layer: classify a decoded payload into a typed [`MarkAction`] and
//! build the URI / contact / calendar strings a click acts on.
//!
//! Everything here is pure string handling; the only barcode dependency is rxing's
//! result parser, used by [`classify`] to recognise the payload kind.

/// What clicking a mark does. Resolved from the QR/barcode payload via rxing's
/// result parser; anything we can't act on falls back to [`MarkAction::Copy`], and
/// each action also copies if it can't be completed (e.g. no NetworkManager).
#[derive(Clone, Debug)]
pub enum MarkAction {
    /// Open a URI with the desktop's default handler (URLs, `mailto:`/`tel:`/`sms:`,
    /// `geo:` rewritten to a Google Maps URL, …).
    Open(String),
    /// Copy the value/text to the clipboard.
    Copy(String),
    /// Join a Wi-Fi network via NetworkManager (falls back to copying the password).
    Wifi {
        ssid: String,
        password: String,
        encryption: String,
    },
    /// Write `content` to a temp `.ext` file and open it (a contact `.vcf` /
    /// calendar `.ics`); falls back to copying the content.
    SaveOpen {
        ext: &'static str,
        content: String,
    },
}

/// Classify a decoded code into an action + a hover summary, using rxing's result
/// parser (URL / MEBKM / MECARD / vCard / WIFI / geo / tel / email / sms / calendar
/// / …). Openable kinds → [`MarkAction::Open`]; Wi-Fi → join; contact/event → save &
/// open; everything else (text, ISBN, VIN, product) → copy. The summary leads with
/// the action + its target, then the full raw payload after a blank line.
pub(super) fn classify(res: &rxing::RXingResult) -> (MarkAction, String) {
    use rxing::client::result::ParsedClientResult as P;
    let raw = res.getText().to_string();
    match rxing::client::result::parseRXingResult(res) {
        P::URIResult(u) => {
            let uri = u.getURI().trim().to_string();
            if uri.is_empty() {
                copy(&raw)
            } else {
                (MarkAction::Open(uri.clone()), summary("Open", &uri, &raw))
            }
        }
        P::GeoResult(g) => {
            // Documented Google Maps URLs format (comma encoded as %2C).
            let url = format!(
                "https://www.google.com/maps/search/?api=1&query={}%2C{}",
                g.getLatitude(),
                g.getLongitude()
            );
            (MarkAction::Open(url.clone()), summary("Open map", &url, &raw))
        }
        P::TelResult(t) => {
            let n = t.getNumber().trim();
            if n.is_empty() {
                copy(&raw)
            } else {
                let uri = if t.getTelURI().is_empty() {
                    format!("tel:{n}")
                } else {
                    t.getTelURI().to_string()
                };
                (MarkAction::Open(uri), summary("Call", n, &raw))
            }
        }
        P::EmailResult(e) => {
            let to = e
                .getTos()
                .first()
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            if to.is_empty() {
                copy(&raw)
            } else {
                let uri = build_mailto(&to, e.getSubject(), e.getBody());
                (MarkAction::Open(uri), summary("Email", &to, &raw))
            }
        }
        P::SMSResult(s) => {
            let n = s.getNumbers().first().cloned().unwrap_or_default();
            if n.trim().is_empty() {
                copy(&raw)
            } else {
                // Build the URI ourselves: rxing's getSMSURI() emits a non-standard
                // `;via=?body=…` with an unencoded body. RFC 5724 is `sms:NUM?body=…`.
                let uri = build_sms(s.getNumbers(), s.getBody());
                (MarkAction::Open(uri), summary("Text", n.trim(), &raw))
            }
        }
        P::WiFiResult(w) => {
            let ssid = w.getSsid().trim().to_string();
            if ssid.is_empty() {
                copy(&raw)
            } else {
                let action = MarkAction::Wifi {
                    ssid: ssid.clone(),
                    password: w.getPassword().to_string(),
                    encryption: w.getNetworkEncryption().to_string(),
                };
                (action, summary("Join Wi-Fi", &ssid, &raw))
            }
        }
        P::AddressBookResult(a) => {
            let name = a.getNames().first().cloned().unwrap_or_else(|| "contact".into());
            let vcf = build_vcard(
                a.getNames(),
                a.getPhoneNumbers(),
                a.getEmails(),
                a.getOrg(),
                a.getTitle(),
                a.getURLs(),
                a.getAddresses(),
                a.getNote(),
            );
            let action = MarkAction::SaveOpen { ext: "vcf", content: vcf };
            (action, summary("Add contact", &name, &raw))
        }
        P::CalendarEventResult(c) => {
            let sum = c.getSummary().trim();
            let ics = build_ics(
                c.getSummary(),
                c.getStartTimestamp(),
                c.getEndTimestamp(),
                c.getLocation(),
                c.getDescription(),
            );
            let action = MarkAction::SaveOpen { ext: "ics", content: ics };
            let title = if sum.is_empty() { "event" } else { sum };
            (action, summary("Add event", title, &raw))
        }
        // rxing found no structured type (Text, ISBN, VIN, product, …). Salvage a
        // leading URL — e.g. the bare-URL prefix of a dual-format MEBKM payload, or
        // "url + trailing text" — otherwise copy.
        _ => match raw.split_whitespace().next().and_then(as_openable_url) {
            Some(url) => (MarkAction::Open(url.clone()), summary("Open", &url, &raw)),
            None => copy(&raw),
        },
    }
}

/// A single token that's a clickable URL: bare `www.` → `https://`, or a known
/// openable scheme. `None` if it isn't one (so we don't hand junk to the opener).
fn as_openable_url(token: &str) -> Option<String> {
    let t = token.trim();
    if t.is_empty() || t.contains(char::is_whitespace) {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("www.") {
        return Some(format!("https://{t}"));
    }
    let i = t.find(':')?;
    let scheme = &lower[..i];
    let ok = !scheme.is_empty()
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        && matches!(
            scheme,
            "http" | "https" | "ftp" | "ftps" | "mailto" | "tel" | "sms" | "magnet" | "bitcoin"
        );
    ok.then(|| t.to_string())
}

/// Copy action with a "Copy: …" summary.
fn copy(raw: &str) -> (MarkAction, String) {
    (MarkAction::Copy(raw.to_string()), summary("Copy", raw, raw))
}

/// `<verb>: <primary>` then, on a blank line, the full raw payload (omitted when it's
/// the same as `primary`, e.g. a bare URL).
fn summary(verb: &str, primary: &str, raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() || raw == primary.trim() {
        format!("{verb}: {primary}")
    } else {
        format!("{verb}: {primary}\n\n{raw}")
    }
}

/// `mailto:` with optional subject/body (RFC 6068).
fn build_mailto(to: &str, subject: &str, body: &str) -> String {
    let mut q = Vec::new();
    if !subject.is_empty() {
        q.push(format!("subject={}", percent_encode(subject)));
    }
    if !body.is_empty() {
        q.push(format!("body={}", percent_encode(body)));
    }
    if q.is_empty() {
        format!("mailto:{to}")
    } else {
        format!("mailto:{to}?{}", q.join("&"))
    }
}

/// `sms:` with optional body (RFC 5724): `sms:NUM[,NUM…]?body=…`. The non-standard
/// `via` field is dropped — most SMS handlers ignore it.
fn build_sms(numbers: &[String], body: &str) -> String {
    let nums: Vec<&str> = numbers
        .iter()
        .map(|n| n.trim())
        .filter(|n| !n.is_empty())
        .collect();
    let mut uri = format!("sms:{}", nums.join(","));
    if !body.is_empty() {
        uri.push_str(&format!("?body={}", percent_encode(body)));
    }
    uri
}

/// vCard 3.0 from the parsed contact fields (covers MECARD / vCard / BIZCARD input).
#[allow(clippy::too_many_arguments)]
fn build_vcard(
    names: &[String],
    phones: &[String],
    emails: &[String],
    org: &str,
    title: &str,
    urls: &[String],
    addresses: &[String],
    note: &str,
) -> String {
    let esc = |s: &str| s.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,");
    let mut v = String::from("BEGIN:VCARD\r\nVERSION:3.0\r\n");
    if let Some(n) = names.first() {
        v.push_str(&format!("FN:{}\r\nN:{};;;;\r\n", esc(n), esc(n)));
    }
    for p in phones {
        v.push_str(&format!("TEL:{}\r\n", esc(p)));
    }
    for e in emails {
        v.push_str(&format!("EMAIL:{}\r\n", esc(e)));
    }
    if !org.is_empty() {
        v.push_str(&format!("ORG:{}\r\n", esc(org)));
    }
    if !title.is_empty() {
        v.push_str(&format!("TITLE:{}\r\n", esc(title)));
    }
    for u in urls {
        v.push_str(&format!("URL:{}\r\n", esc(u)));
    }
    for a in addresses {
        v.push_str(&format!("ADR:;;{};;;;\r\n", esc(a)));
    }
    if !note.is_empty() {
        v.push_str(&format!("NOTE:{}\r\n", esc(note)));
    }
    v.push_str("END:VCARD\r\n");
    v
}

/// iCalendar VEVENT from the parsed calendar fields. rxing returns timestamps as
/// seconds since the epoch; ≤0 means unset.
fn build_ics(summary: &str, start_secs: i64, end_secs: i64, location: &str, description: &str) -> String {
    let esc = |s: &str| s.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,");
    let fmt = |secs: i64| -> Option<String> {
        (secs > 0)
            .then(|| chrono::DateTime::from_timestamp(secs, 0))
            .flatten()
            .map(|dt| dt.format("%Y%m%dT%H%M%SZ").to_string())
    };
    let mut s = String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//cosmic-capture-kit//EN\r\nBEGIN:VEVENT\r\n",
    );
    s.push_str(&format!("SUMMARY:{}\r\n", esc(summary)));
    if let Some(dt) = fmt(start_secs) {
        s.push_str(&format!("DTSTART:{dt}\r\n"));
    }
    if let Some(dt) = fmt(end_secs) {
        s.push_str(&format!("DTEND:{dt}\r\n"));
    }
    if !location.is_empty() {
        s.push_str(&format!("LOCATION:{}\r\n", esc(location)));
    }
    if !description.is_empty() {
        s.push_str(&format!("DESCRIPTION:{}\r\n", esc(description)));
    }
    s.push_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
    s
}

/// Percent-encode a string for a URI query component (RFC 3986 unreserved set).
fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A decoded result carrying just `text` (the payload classify parses); points and
    /// raw bytes are irrelevant to the action layer.
    fn result(text: &str) -> rxing::RXingResult {
        rxing::RXingResult::new(text, Vec::new(), Vec::new(), rxing::BarcodeFormat::QR_CODE)
    }

    #[test]
    fn percent_encode_keeps_unreserved() {
        assert_eq!(percent_encode("AZaz09-_.~"), "AZaz09-_.~");
    }

    #[test]
    fn percent_encode_escapes_space_and_reserved() {
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("a&b=c?d/e"), "a%26b%3Dc%3Fd%2Fe");
        assert_eq!(percent_encode("Hello, World!"), "Hello%2C%20World%21");
    }

    #[test]
    fn percent_encode_escapes_unicode_utf8() {
        // 'é' = U+00E9 -> UTF-8 bytes 0xC3 0xA9.
        assert_eq!(percent_encode("é"), "%C3%A9");
    }

    #[test]
    fn classify_url_opens() {
        let (action, _) = classify(&result("https://example.com"));
        assert!(matches!(action, MarkAction::Open(ref u) if u == "https://example.com"));
    }

    #[test]
    fn classify_email_opens() {
        let (action, _) = classify(&result("mailto:foo@bar.com"));
        assert!(matches!(action, MarkAction::Open(ref u) if u.starts_with("mailto:")));
    }

    #[test]
    fn classify_phone_opens() {
        let (action, _) = classify(&result("tel:+15551234567"));
        assert!(matches!(action, MarkAction::Open(ref u) if u.starts_with("tel:")));
    }

    #[test]
    fn classify_wifi_joins() {
        let (action, _) = classify(&result("WIFI:T:WPA;S:MyNet;P:secretpass;;"));
        match action {
            MarkAction::Wifi { ssid, password, .. } => {
                assert_eq!(ssid, "MyNet");
                assert_eq!(password, "secretpass");
            }
            other => panic!("expected Wifi, got {other:?}"),
        }
    }

    #[test]
    fn classify_plain_text_copies() {
        let (action, _) = classify(&result("just some plain words"));
        assert!(matches!(action, MarkAction::Copy(ref s) if s == "just some plain words"));
    }
}
