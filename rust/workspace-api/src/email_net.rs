/*
 * Broodlink workspace-api — Email network layer (IMAP/SMTP).
 * Phase 2: real connections. Blocking `imap`/`lettre` calls are run on the
 * blocking pool via spawn_blocking; connections are opened per operation
 * (pooling is a later optimization). Credentials come from ws_email_accounts.
 */

use imap_proto::types::Address;
use lettre::message::{header::ContentType, Mailbox, Message, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{SmtpTransport, Transport};
use serde_json::{json, Value};

#[derive(Clone)]
pub struct MailCreds {
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_user: String,
    pub imap_pass: String,
    pub imap_starttls: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_user: String,
    pub smtp_pass: String,
}

pub type ImapSession = imap::Session<native_tls::TlsStream<std::net::TcpStream>>;

/// Open + authenticate an IMAP session over implicit TLS (port 993 etc.).
/// (STARTTLS on plain :143 is not supported in this cut — Fastmail/Gmail/most
/// providers use implicit TLS, which this covers.)
pub fn imap_session(c: &MailCreds) -> imap::error::Result<ImapSession> {
    let tls = native_tls::TlsConnector::builder().build().map_err(|e| {
        imap::error::Error::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            e.to_string(),
        ))
    })?;
    let client = imap::connect(
        (c.imap_host.as_str(), c.imap_port),
        c.imap_host.as_str(),
        &tls,
    )?;
    client.login(&c.imap_user, &c.imap_pass).map_err(|(e, _)| e)
}

fn smtp_transport(c: &MailCreds) -> Result<SmtpTransport, lettre::transport::smtp::Error> {
    let creds = Credentials::new(c.smtp_user.clone(), c.smtp_pass.clone());
    let builder = if c.smtp_port == 587 {
        SmtpTransport::starttls_relay(&c.smtp_host)?
    } else {
        SmtpTransport::relay(&c.smtp_host)?
    };
    Ok(builder.port(c.smtp_port).credentials(creds).build())
}

/// Test IMAP + SMTP login. Blocking; call via spawn_blocking.
pub fn test_blocking(c: MailCreds) -> Value {
    let imap = match imap_session(&c) {
        Ok(mut s) => {
            let _ = s.logout();
            json!({ "ok": true })
        }
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    };

    let smtp = if c.smtp_host.is_empty() {
        None
    } else {
        Some(match smtp_transport(&c).and_then(|t| t.test_connection()) {
            Ok(true) => json!({ "ok": true }),
            Ok(false) => json!({ "ok": false, "error": "server did not accept connection" }),
            Err(e) => json!({ "ok": false, "error": e.to_string() }),
        })
    };

    let ok = imap["ok"].as_bool().unwrap_or(false)
        && smtp
            .as_ref()
            .map(|s| s["ok"].as_bool().unwrap_or(false))
            .unwrap_or(true);

    let mut out = json!({ "ok": ok, "imap": imap });
    if let Some(s) = smtp {
        out["smtp"] = s;
    }
    out
}

// ---------------------------------------------------------------------------
// Decoding helpers
// ---------------------------------------------------------------------------

fn s8(c: &Option<&[u8]>) -> String {
    c.map(|b| String::from_utf8_lossy(b).to_string())
        .unwrap_or_default()
}

fn addr_one(a: &Address) -> (String, String) {
    let name = s8(&a.name);
    let mailbox = s8(&a.mailbox);
    let host = s8(&a.host);
    let email = if !mailbox.is_empty() && !host.is_empty() {
        format!("{mailbox}@{host}")
    } else {
        mailbox
    };
    (name, email)
}

fn addr_list(list: &Option<Vec<Address>>) -> String {
    list.as_ref()
        .map(|v| {
            v.iter()
                .map(|a| addr_one(a).1)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Folder role detection
// ---------------------------------------------------------------------------

pub fn folders_blocking(c: MailCreds) -> imap::error::Result<Vec<String>> {
    let mut s = imap_session(&c)?;
    let names = s.list(Some(""), Some("*"))?;
    let mut out: Vec<String> = names.iter().map(|n| n.name().to_string()).collect();
    let _ = s.logout();
    if !out.iter().any(|f| f.eq_ignore_ascii_case("INBOX")) {
        out.insert(0, "INBOX".into());
    }
    Ok(out)
}

/// Resolve a logical role (sent/trash/archive/drafts/junk) to an actual folder
/// name on the server, via SPECIAL-USE attributes then common-name fallback.
fn resolve_role(s: &mut ImapSession, role: &str) -> Option<String> {
    let names = s.list(Some(""), Some("*")).ok()?;
    let (attr, candidates): (&str, &[&str]) = match role {
        "sent" => (
            "\\Sent",
            &["Sent", "Sent Items", "[Gmail]/Sent Mail", "INBOX.Sent"],
        ),
        "trash" => (
            "\\Trash",
            &["Trash", "Bin", "Deleted Items", "[Gmail]/Trash"],
        ),
        "archive" => ("\\Archive", &["Archive", "All Mail", "[Gmail]/All Mail"]),
        "drafts" => ("\\Drafts", &["Drafts", "Draft", "[Gmail]/Drafts"]),
        "junk" => ("\\Junk", &["Junk", "Spam", "[Gmail]/Spam"]),
        _ => ("", &[]),
    };
    for n in names.iter() {
        if n.attributes()
            .iter()
            .any(|a| format!("{a:?}").contains(attr.trim_start_matches('\\')))
        {
            return Some(n.name().to_string());
        }
    }
    for cand in candidates {
        if let Some(n) = names.iter().find(|n| n.name().eq_ignore_ascii_case(cand)) {
            return Some(n.name().to_string());
        }
    }
    candidates.first().map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

pub fn list_blocking(
    c: MailCreds,
    folder: String,
    limit: usize,
    offset: usize,
    filter: String,
) -> imap::error::Result<(Vec<Value>, usize)> {
    let mut s = imap_session(&c)?;
    s.select(&folder)?;
    let query = match filter.as_str() {
        "unread" => "UNSEEN",
        "unanswered" => "UNANSWERED",
        "favorites" => "FLAGGED",
        _ => "ALL",
    };
    let mut uids: Vec<u32> = s.uid_search(query)?.into_iter().collect();
    uids.sort_unstable();
    let total = uids.len();
    uids.reverse(); // newest first
    let window: Vec<u32> = uids.into_iter().skip(offset).take(limit).collect();
    if window.is_empty() {
        let _ = s.logout();
        return Ok((Vec::new(), total));
    }
    let set = window
        .iter()
        .map(|u| u.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let fetches = s.uid_fetch(set, "(UID FLAGS ENVELOPE RFC822.SIZE)")?;

    let mut by_uid: std::collections::HashMap<u32, Value> = std::collections::HashMap::new();
    for f in fetches.iter() {
        let uid = match f.uid {
            Some(u) => u,
            None => continue,
        };
        let flags: Vec<String> = f.flags().iter().map(|fl| format!("{fl:?}")).collect();
        let is_read = f
            .flags()
            .iter()
            .any(|fl| matches!(fl, imap::types::Flag::Seen));
        let is_answered = f
            .flags()
            .iter()
            .any(|fl| matches!(fl, imap::types::Flag::Answered));
        let is_flagged = f
            .flags()
            .iter()
            .any(|fl| matches!(fl, imap::types::Flag::Flagged));
        let env = f.envelope();
        let subject = env.map(|e| s8(&e.subject)).unwrap_or_default();
        let (from_name, from_address) = env
            .and_then(|e| e.from.as_ref())
            .and_then(|v| v.first())
            .map(addr_one)
            .unwrap_or_default();
        let to = env.map(|e| addr_list(&e.to)).unwrap_or_default();
        let cc = env.map(|e| addr_list(&e.cc)).unwrap_or_default();
        let date = env.map(|e| s8(&e.date)).unwrap_or_default();
        let date_epoch = mailparse::dateparse(&date).unwrap_or(0);
        by_uid.insert(
            uid,
            json!({
                "uid": uid,
                "message_id": env.map(|e| s8(&e.message_id)).unwrap_or_default(),
                "subject": subject,
                "from_name": from_name,
                "from_address": from_address,
                "to": to,
                "cc": cc,
                "date": date,
                "date_display": date,
                "date_epoch": date_epoch,
                "size": f.size.unwrap_or(0),
                "is_read": is_read,
                "is_answered": is_answered,
                "is_flagged": is_flagged,
                "flags": flags,
                "has_attachments": false,
                "tags": [],
                "is_spam_verdict": false,
                "cached_summary": Value::Null,
            }),
        );
    }
    let _ = s.logout();
    // Preserve newest-first ordering from the search.
    let ordered: Vec<Value> = window.iter().filter_map(|u| by_uid.remove(u)).collect();
    Ok((ordered, total))
}

// ---------------------------------------------------------------------------
// Read a single message
// ---------------------------------------------------------------------------

pub fn read_blocking(
    c: MailCreds,
    folder: String,
    uid: u32,
    mark_seen: bool,
) -> imap::error::Result<Option<Value>> {
    let mut s = imap_session(&c)?;
    s.select(&folder)?;
    let fetches = s.uid_fetch(uid.to_string(), "(UID RFC822)")?;
    let body = match fetches
        .iter()
        .next()
        .and_then(|f| f.body().map(|b| b.to_vec()))
    {
        Some(b) => b,
        None => {
            let _ = s.logout();
            return Ok(None);
        }
    };
    if mark_seen {
        let _ = s.uid_store(uid.to_string(), "+FLAGS (\\Seen)");
    }
    let _ = s.logout();

    let parsed = mailparse::parse_mail(&body).map_err(|e| {
        imap::error::Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            e.to_string(),
        ))
    })?;

    let hdr = |name: &str| -> String {
        use mailparse::MailHeaderMap;
        parsed.headers.get_first_value(name).unwrap_or_default()
    };

    let mut text = String::new();
    let mut html = String::new();
    let mut attachments: Vec<Value> = Vec::new();
    collect_parts(&parsed, &mut text, &mut html, &mut attachments);

    Ok(Some(json!({
        "uid": uid,
        "message_id": hdr("Message-ID"),
        "subject": hdr("Subject"),
        "from_name": "",
        "from_address": hdr("From"),
        "to": hdr("To"),
        "cc": hdr("Cc"),
        "date": hdr("Date"),
        "in_reply_to": hdr("In-Reply-To"),
        "references": hdr("References"),
        "body": text,
        "body_html": html,
        "attachments": attachments,
        "cached_summary": Value::Null,
        "cached_ai_reply": Value::Null,
        "thread_turns": [],
    })))
}

fn collect_parts(
    part: &mailparse::ParsedMail,
    text: &mut String,
    html: &mut String,
    attachments: &mut Vec<Value>,
) {
    use mailparse::MailHeaderMap;
    let ctype = &part.ctype.mimetype;
    let disposition = part.get_content_disposition();
    let is_attachment = matches!(
        disposition.disposition,
        mailparse::DispositionType::Attachment
    ) || disposition.params.contains_key("filename");

    if is_attachment {
        let filename = disposition
            .params
            .get("filename")
            .cloned()
            .or_else(|| {
                part.headers
                    .get_first_value("Content-Type")
                    .and_then(|_| part.ctype.params.get("name").cloned())
            })
            .unwrap_or_else(|| "attachment".into());
        let size = part.get_body_raw().map(|b| b.len()).unwrap_or(0);
        attachments.push(json!({
            "index": attachments.len(),
            "filename": filename,
            "content_type": ctype,
            "size": size,
            "is_inline": false,
        }));
        return;
    }

    if part.subparts.is_empty() {
        if ctype == "text/plain" {
            if let Ok(b) = part.get_body() {
                text.push_str(&b);
            }
        } else if ctype == "text/html" {
            if let Ok(b) = part.get_body() {
                html.push_str(&b);
            }
        }
    } else {
        for sub in &part.subparts {
            collect_parts(sub, text, html, attachments);
        }
    }
}

// ---------------------------------------------------------------------------
// Flags / move / delete
// ---------------------------------------------------------------------------

pub fn store_flag_blocking(
    c: MailCreds,
    folder: String,
    uid: u32,
    flag: &str,
    set: bool,
) -> imap::error::Result<()> {
    let mut s = imap_session(&c)?;
    s.select(&folder)?;
    let op = if set { "+FLAGS" } else { "-FLAGS" };
    s.uid_store(uid.to_string(), format!("{op} ({flag})"))?;
    let _ = s.logout();
    Ok(())
}

pub fn move_to_role_blocking(
    c: MailCreds,
    folder: String,
    uid: u32,
    role: &str,
) -> imap::error::Result<()> {
    let mut s = imap_session(&c)?;
    s.select(&folder)?;
    let dest = resolve_role(&mut s, role).unwrap_or_else(|| "Archive".into());
    s.uid_mv(uid.to_string(), dest)?;
    let _ = s.logout();
    Ok(())
}

pub fn move_to_blocking(
    c: MailCreds,
    folder: String,
    uid: u32,
    dest: String,
) -> imap::error::Result<()> {
    let mut s = imap_session(&c)?;
    s.select(&folder)?;
    s.uid_mv(uid.to_string(), dest)?;
    let _ = s.logout();
    Ok(())
}

pub fn delete_permanent_blocking(
    c: MailCreds,
    folder: String,
    uid: u32,
) -> imap::error::Result<()> {
    let mut s = imap_session(&c)?;
    s.select(&folder)?;
    s.uid_store(uid.to_string(), "+FLAGS (\\Deleted)")?;
    s.expunge()?;
    let _ = s.logout();
    Ok(())
}

// ---------------------------------------------------------------------------
// Send / draft
// ---------------------------------------------------------------------------

pub struct OutgoingMail {
    pub from: String,
    pub to: String,
    pub cc: Option<String>,
    pub subject: String,
    pub body: String,
    pub body_html: Option<String>,
}

fn build_message(m: &OutgoingMail) -> Result<Message, String> {
    let from: Mailbox = m.from.parse().map_err(|e| format!("bad from: {e}"))?;
    let mut builder = Message::builder().from(from).subject(&m.subject);
    for addr in m.to.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        builder = builder.to(addr.parse().map_err(|e| format!("bad to '{addr}': {e}"))?);
    }
    if let Some(cc) = &m.cc {
        for addr in cc.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            builder = builder.cc(addr.parse().map_err(|e| format!("bad cc '{addr}': {e}"))?);
        }
    }
    let msg = match &m.body_html {
        Some(html) => builder
            .multipart(
                MultiPart::alternative()
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_PLAIN)
                            .body(m.body.clone()),
                    )
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_HTML)
                            .body(html.clone()),
                    ),
            )
            .map_err(|e| e.to_string())?,
        None => builder.body(m.body.clone()).map_err(|e| e.to_string())?,
    };
    Ok(msg)
}

pub fn send_blocking(c: MailCreds, mail: OutgoingMail) -> Value {
    let msg = match build_message(&mail) {
        Ok(m) => m,
        Err(e) => return json!({ "success": false, "error": e }),
    };
    let transport = match smtp_transport(&c) {
        Ok(t) => t,
        Err(e) => return json!({ "success": false, "error": e.to_string() }),
    };
    if let Err(e) = transport.send(&msg) {
        return json!({ "success": false, "error": e.to_string() });
    }
    // Best-effort append to Sent.
    let raw = msg.formatted();
    let mut sent_folder = Value::Null;
    if let Ok(mut s) = imap_session(&c) {
        if let Some(dest) = resolve_role(&mut s, "sent") {
            if s.append(&dest, &raw).is_ok() {
                sent_folder = json!(dest);
            }
        }
        let _ = s.logout();
    }
    json!({ "success": true, "queued": false, "message": "sent", "sent_folder": sent_folder })
}

pub fn draft_blocking(c: MailCreds, mail: OutgoingMail) -> Value {
    let msg = match build_message(&mail) {
        Ok(m) => m,
        Err(e) => return json!({ "success": false, "error": e }),
    };
    let raw = msg.formatted();
    match imap_session(&c) {
        Ok(mut s) => {
            let dest = resolve_role(&mut s, "drafts").unwrap_or_else(|| "Drafts".into());
            let res = s.append(&dest, &raw);
            let _ = s.logout();
            match res {
                Ok(_) => json!({ "success": true, "message": "draft saved" }),
                Err(e) => json!({ "success": false, "error": e.to_string() }),
            }
        }
        Err(e) => json!({ "success": false, "error": e.to_string() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live Fastmail check. Skips unless FM_USER/FM_PASS are set.
    /// Run: FM_USER=.. FM_PASS=.. cargo test -p workspace-api live_mail -- --nocapture
    #[test]
    fn live_mail() {
        let (u, p) = match (std::env::var("FM_USER"), std::env::var("FM_PASS")) {
            (Ok(u), Ok(p)) => (u, p),
            _ => {
                eprintln!("skip: FM_USER/FM_PASS not set");
                return;
            }
        };
        let c = MailCreds {
            imap_host: "imap.fastmail.com".into(),
            imap_port: 993,
            imap_user: u.clone(),
            imap_pass: p.clone(),
            imap_starttls: false,
            smtp_host: "smtp.fastmail.com".into(),
            smtp_port: 465,
            smtp_user: u,
            smtp_pass: p,
        };
        let r = test_blocking(c);
        println!("RESULT: {r}");
        assert_eq!(r["ok"], true, "expected IMAP+SMTP login to succeed");
    }

    /// Encrypt the password, decrypt it, then log in — proves the at-rest
    /// encryption path doesn't corrupt the credential.
    #[test]
    fn live_encrypted_roundtrip() {
        let mut c = match creds() {
            Some(c) => c,
            None => return,
        };
        let cipher = crate::crypto::Cipher::load_or_create("/tmp/ws_test_key").unwrap();
        let enc = cipher.encrypt(&c.imap_pass);
        assert!(enc.starts_with("enc:v1:"));
        c.imap_pass = cipher.decrypt(&enc);
        c.smtp_pass = c.imap_pass.clone();
        let r = test_blocking(c);
        println!("ENC ROUNDTRIP: {r}");
        assert_eq!(
            r["ok"], true,
            "login with decrypted-from-ciphertext password should work"
        );
    }

    fn creds() -> Option<MailCreds> {
        let (u, p) = match (std::env::var("FM_USER"), std::env::var("FM_PASS")) {
            (Ok(u), Ok(p)) => (u, p),
            _ => return None,
        };
        Some(MailCreds {
            imap_host: "imap.fastmail.com".into(),
            imap_port: 993,
            imap_user: u.clone(),
            imap_pass: p.clone(),
            imap_starttls: false,
            smtp_host: "smtp.fastmail.com".into(),
            smtp_port: 465,
            smtp_user: u,
            smtp_pass: p,
        })
    }

    #[test]
    fn live_list_read() {
        let c = match creds() {
            Some(c) => c,
            None => return,
        };
        let (emails, total) = list_blocking(c.clone(), "INBOX".into(), 5, 0, "all".into()).unwrap();
        println!("LIST total={total} returned={}", emails.len());
        if let Some(first) = emails.first() {
            println!("  first: uid={} subject={}", first["uid"], first["subject"]);
            let uid = first["uid"].as_u64().unwrap() as u32;
            let msg = read_blocking(c, "INBOX".into(), uid, false)
                .unwrap()
                .unwrap();
            let body = msg["body"].as_str().unwrap_or("");
            println!("  read subject={} body_len={}", msg["subject"], body.len());
        }
    }

    #[test]
    fn live_send() {
        let c = match creds() {
            Some(c) => c,
            None => return,
        };
        let me = std::env::var("FM_USER").unwrap();
        let mail = OutgoingMail {
            from: me.clone(),
            to: me,
            cc: None,
            subject: "Broodlink workspace-api send test".into(),
            body: "This is a live SMTP send test from the ported email feature.".into(),
            body_html: None,
        };
        let r = send_blocking(c, mail);
        println!("SEND: {r}");
        assert_eq!(r["success"], true, "expected SMTP send to succeed");
    }
}
