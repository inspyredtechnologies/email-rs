//! Parsing raw RFC 5322 messages and building replies / outgoing mail.

use chrono::{Local, TimeZone};
use mail_parser::{Address, MessageParser, MimeHeaders};

use crate::model::{AccountConfig, Attachment, Draft, MessageBody, MessageMeta};

fn format_address(addr: Option<&Address>) -> String {
    let Some(addr) = addr else { return String::new() };
    addr.iter()
        .map(|a| match (a.name(), a.address()) {
            (Some(n), Some(e)) if !n.is_empty() => format!("{n} <{e}>"),
            (_, Some(e)) => e.to_string(),
            (Some(n), None) => n.to_string(),
            (None, None) => String::new(),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Build list metadata from a header block (or a full message).
pub fn parse_meta(uid: u32, raw: &[u8]) -> MessageMeta {
    let parsed = MessageParser::default().parse(raw);
    let msg = parsed.as_ref();
    let from = msg.and_then(|m| m.from()).and_then(|a| a.first());
    let content_type = msg
        .and_then(|m| m.content_type())
        .map(|ct| format!("{}/{}", ct.ctype(), ct.subtype().unwrap_or("")))
        .unwrap_or_default()
        .to_ascii_lowercase();
    MessageMeta {
        uid,
        from_name: from.and_then(|a| a.name()).unwrap_or_default().to_string(),
        from_addr: from.and_then(|a| a.address()).unwrap_or_default().to_string(),
        to: format_address(msg.and_then(|m| m.to())),
        subject: msg.and_then(|m| m.subject()).unwrap_or_default().to_string(),
        date: msg.and_then(|m| m.date()).map(|d| d.to_timestamp()).unwrap_or(0),
        seen: false,
        flagged: false,
        answered: false,
        // From headers alone, multipart/mixed is the usual signal for attachments.
        has_attachments: content_type == "multipart/mixed",
        size: raw.len() as u32,
        preview: String::new(),
    }
}

/// Parse a message for the reading pane: plain text (HTML converted when there's
/// no text part), the original HTML when present, and attachments.
pub fn parse_body(raw: &[u8]) -> MessageBody {
    let Some(msg) = MessageParser::default().parse(raw) else {
        return MessageBody { text: String::from_utf8_lossy(raw).into_owned(), ..Default::default() };
    };
    let text = match msg.body_text(0) {
        Some(t) if !t.trim().is_empty() => t.into_owned(),
        _ => msg
            .body_html(0)
            .map(|h| html2text::from_read(h.as_bytes(), 100).unwrap_or_else(|_| h.into_owned()))
            .unwrap_or_default(),
    };
    // Only real HTML parts; mail-parser would otherwise synthesize HTML from text.
    let mut html = msg.html_part(0).filter(|p| p.is_text_html()).and_then(|_| msg.body_html(0)).map(|h| h.into_owned());
    let mut inline_used = Vec::new();
    if let Some(h) = html.as_mut() {
        let images: Vec<(String, String, Vec<u8>)> = msg
            .attachments()
            .filter_map(|p| {
                let cid = p.content_id()?.trim_matches(['<', '>']).to_string();
                let ct = p.content_type().map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or(""))).unwrap_or_default();
                Some((cid, ct, p.contents().to_vec()))
            })
            .collect();
        inline_used = crate::html::inline_cid_images(h, &images);
    }
    // Images shown inline in the HTML aren't listed as attachments, as in Outlook.
    let attachments = msg
        .attachments()
        .filter(|p| html.is_none() || !p.content_id().is_some_and(|c| inline_used.iter().any(|u| u == c.trim_matches(['<', '>']))))
        .map(|p| Attachment {
            name: p.attachment_name().unwrap_or("attachment").to_string(),
            data: p.contents().to_vec(),
        })
        .collect();
    MessageBody {
        subject: msg.subject().unwrap_or_default().to_string(),
        from: format_address(msg.from()),
        to: format_address(msg.to()),
        cc: format_address(msg.cc()),
        date: msg.date().map(|d| d.to_timestamp()).unwrap_or(0),
        text,
        html,
        message_id: msg.message_id().map(str::to_string),
        references: msg
            .references()
            .as_text_list()
            .map(|l| l.iter().map(|s| s.to_string()).collect())
            .or_else(|| msg.references().as_text().map(|s| vec![s.to_string()]))
            .unwrap_or_default(),
        reply_to: msg.reply_to().map(|a| format_address(Some(a))),
        attachments,
    }
}

/// Short single-line preview, like the second line of Outlook's message list.
pub fn preview_of(body: &MessageBody) -> String {
    let mut out = String::new();
    for word in body.text.split_whitespace() {
        if out.len() + word.len() > 160 {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Outlook's long date format, e.g. "Monday, October 13, 2026 9:05 AM".
pub fn long_date(ts: i64) -> String {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|d| d.format("%A, %B %-d, %Y %-I:%M %p").to_string())
        .unwrap_or_default()
}

fn prefixed(prefix: &str, subject: &str) -> String {
    let lower = subject.to_ascii_lowercase();
    let p = prefix.to_ascii_lowercase();
    if lower.starts_with(&p) { subject.to_string() } else { format!("{prefix} {subject}") }
}

/// Outlook-style quoted header block placed above the original text.
fn quote_block(orig: &MessageBody) -> String {
    let mut s = String::from("\n\n________________________________\n");
    s += &format!("From: {}\n", orig.from);
    s += &format!("Sent: {}\n", long_date(orig.date));
    s += &format!("To: {}\n", orig.to);
    if !orig.cc.is_empty() {
        s += &format!("Cc: {}\n", orig.cc);
    }
    s += &format!("Subject: {}\n\n", orig.subject);
    s += &orig.text;
    s
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Plain text as HTML, keeping line breaks.
pub fn text_to_html(text: &str) -> String {
    escape_html(text).replace("\r\n", "\n").replace('\n', "<br>")
}

/// The inside of an HTML document's `<body>`, without scripts.
fn body_inner(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<body").and_then(|i| lower[i..].find('>').map(|j| i + j + 1)).unwrap_or(0);
    let end = lower[start..].rfind("</body").map(|i| start + i).unwrap_or(html.len());
    let (mut inner, mut lower) = (html[start..end].to_string(), lower[start..end].to_string());
    while let Some(i) = lower.find("<script") {
        let j = lower[i..].find("</script>").map(|j| i + j + "</script>".len()).unwrap_or(lower.len());
        inner.replace_range(i..j, "");
        lower.replace_range(i..j, "");
    }
    inner
}

/// Outlook's HTML reply header (its `divRplyFwdMsg` block) followed by the original.
fn quote_html(orig: &MessageBody) -> String {
    let mut header = format!(
        "<b>From:</b> {}<br><b>Sent:</b> {}<br><b>To:</b> {}<br>",
        escape_html(&orig.from),
        escape_html(&long_date(orig.date)),
        escape_html(&orig.to)
    );
    if !orig.cc.is_empty() {
        header += &format!("<b>Cc:</b> {}<br>", escape_html(&orig.cc));
    }
    header += &format!("<b>Subject:</b> {}", escape_html(&orig.subject));
    let original = match &orig.html {
        Some(h) => body_inner(h),
        None => format!("<div>{}</div>", text_to_html(&orig.text)),
    };
    format!(
        "<div><br></div><hr style=\"display:inline-block;width:98%\" tabindex=\"-1\">\
         <div id=\"divRplyFwdMsg\" dir=\"ltr\"><font face=\"Calibri, sans-serif\" style=\"font-size:11pt\" color=\"#000000\">\
         {header}</font><div>&nbsp;</div></div>{original}"
    )
}

/// The HTML part: the user's formatted text in Outlook's default font, then the quote.
pub fn html_document(draft: &Draft) -> String {
    let quote = match &draft.quote_html {
        Some(q) => q.clone(),
        None => text_to_html(&draft.quote_text),
    };
    format!(
        "<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\"></head><body>\
         <div style=\"font-family:Calibri,Arial,Helvetica,sans-serif;font-size:11pt;color:#000000\">{}</div>{quote}</body></html>",
        crate::rich::to_html(&draft.body, &draft.body_styles)
    )
}

#[derive(Clone, Copy)]
pub enum ReplyKind {
    Reply,
    ReplyAll,
    Forward,
}

pub fn make_reply(account: &AccountConfig, orig: &MessageBody, kind: ReplyKind) -> Draft {
    let me = account.email.to_ascii_lowercase();
    let sender = orig.reply_to.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| orig.from.clone());
    let (to, cc) = match kind {
        ReplyKind::Reply => (sender, String::new()),
        ReplyKind::ReplyAll => {
            let others: Vec<&str> = orig
                .to
                .split("; ")
                .chain(orig.cc.split("; "))
                .filter(|a| !a.is_empty() && !a.to_ascii_lowercase().contains(&me))
                .collect();
            (sender, others.join("; "))
        }
        ReplyKind::Forward => (String::new(), String::new()),
    };
    let subject = match kind {
        ReplyKind::Forward => prefixed("FW:", &orig.subject),
        _ => prefixed("RE:", &orig.subject),
    };
    let mut references = orig.references.clone();
    if let Some(id) = &orig.message_id {
        references.push(id.clone());
    }
    let is_forward = matches!(kind, ReplyKind::Forward);
    Draft {
        account_id: account.id.clone(),
        to,
        cc,
        subject,
        quote_text: quote_block(orig),
        quote_html: Some(quote_html(orig)),
        in_reply_to: if is_forward { None } else { orig.message_id.clone() },
        references: if is_forward { Vec::new() } else { references },
        ..Default::default()
    }
}

/// Split an Outlook-style recipient field ("a@x.com; Bob <b@y.com>, c@z").
pub fn split_recipients(field: &str) -> Vec<String> {
    field
        .split([';', ','])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Build the RFC 5322 message to hand to SMTP and to store in Sent Items.
pub fn build_message(account: &AccountConfig, draft: &Draft) -> Result<lettre::Message, String> {
    use lettre::message::{Attachment as LAttachment, MultiPart, SinglePart, header::ContentType};

    let from = format!("{} <{}>", account.display_name, account.email)
        .parse()
        .map_err(|e| format!("Invalid sender address: {e}"))?;
    let mut builder = lettre::Message::builder().from(from).subject(draft.subject.clone());
    let mut any_recipient = false;
    for (field, kind) in [(&draft.to, 0), (&draft.cc, 1), (&draft.bcc, 2)] {
        for r in split_recipients(field) {
            let mbox = r.parse().map_err(|_| format!("Outlook does not recognize \"{r}\"."))?;
            any_recipient = true;
            builder = match kind {
                0 => builder.to(mbox),
                1 => builder.cc(mbox),
                _ => builder.bcc(mbox),
            };
        }
    }
    if !any_recipient {
        return Err("This message must have at least one recipient.".into());
    }
    if let Some(id) = &draft.in_reply_to {
        builder = builder.in_reply_to(id_with_brackets(id));
    }
    if !draft.references.is_empty() {
        let refs: Vec<String> = draft.references.iter().map(|r| id_with_brackets(r)).collect();
        builder = builder.references(refs.join(" "));
    }

    // Plain text alone, or text + HTML alternatives (what Outlook sends for HTML mail).
    let plain = format!("{}{}", draft.body, draft.quote_text);
    let alternative = || MultiPart::alternative_plain_html(plain.clone(), html_document(draft));
    let result = if draft.attachments.is_empty() {
        if draft.plain_text { builder.singlepart(SinglePart::plain(plain.clone())) } else { builder.multipart(alternative()) }
    } else {
        let mut mp = if draft.plain_text {
            MultiPart::mixed().singlepart(SinglePart::plain(plain.clone()))
        } else {
            MultiPart::mixed().multipart(alternative())
        };
        for path in &draft.attachments {
            let data = std::fs::read(path).map_err(|e| format!("Cannot attach {}: {e}", path.display()))?;
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let ct = ContentType::parse("application/octet-stream").unwrap();
            mp = mp.singlepart(LAttachment::new(name).body(data, ct));
        }
        builder.multipart(mp)
    };
    result.map_err(|e| e.to_string())
}

fn id_with_brackets(id: &str) -> String {
    if id.starts_with('<') { id.to_string() } else { format!("<{id}>") }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b"From: Alice Smith <alice@example.com>\r\n\
To: Me <me@example.com>, Bob <bob@example.com>\r\n\
Cc: carol@example.com\r\n\
Subject: Quarterly numbers\r\n\
Date: Mon, 12 Oct 2026 09:30:00 +0000\r\n\
Message-ID: <abc123@example.com>\r\n\
Content-Type: text/plain; charset=utf-8\r\n\r\n\
Hi team,\r\nHere are the numbers.\r\n";

    fn account() -> AccountConfig {
        AccountConfig {
            id: "a1".into(),
            display_name: "Me".into(),
            email: "me@example.com".into(),
            ..Default::default()
        }
    }

    #[test]
    fn parses_meta_and_body() {
        let meta = parse_meta(7, SAMPLE);
        assert_eq!(meta.uid, 7);
        assert_eq!(meta.from_name, "Alice Smith");
        assert_eq!(meta.subject, "Quarterly numbers");
        let body = parse_body(SAMPLE);
        assert!(body.text.contains("Here are the numbers."));
        assert_eq!(body.message_id.as_deref(), Some("abc123@example.com"));
        assert_eq!(preview_of(&body), "Hi team, Here are the numbers.");
    }

    #[test]
    fn reply_all_excludes_self_and_quotes() {
        let body = parse_body(SAMPLE);
        let d = make_reply(&account(), &body, ReplyKind::ReplyAll);
        assert_eq!(d.subject, "RE: Quarterly numbers");
        assert_eq!(d.to, "Alice Smith <alice@example.com>");
        assert_eq!(d.cc, "Bob <bob@example.com>; carol@example.com");
        assert_eq!(d.in_reply_to.as_deref(), Some("abc123@example.com"));
        assert!(d.body.is_empty());
        assert!(d.quote_text.contains("Subject: Quarterly numbers"));
        let quote = d.quote_html.as_deref().unwrap();
        assert!(quote.contains("<b>From:</b> Alice Smith &lt;alice@example.com&gt;<br>"));
        assert!(quote.contains("Hi team,<br>Here are the numbers."));
        let again = make_reply(&account(), &MessageBody { subject: d.subject.clone(), ..body.clone() }, ReplyKind::Reply);
        assert_eq!(again.subject, "RE: Quarterly numbers");
        let fw = make_reply(&account(), &body, ReplyKind::Forward);
        assert_eq!(fw.subject, "FW: Quarterly numbers");
        assert!(fw.to.is_empty() && fw.in_reply_to.is_none());
    }

    #[test]
    fn builds_outgoing_message() {
        let mut d = make_reply(&account(), &parse_body(SAMPLE), ReplyKind::Reply);
        d.body = "Thanks!".into();
        d.body_styles = vec![crate::rich::Style { bold: true, ..Default::default() }; 7];
        let msg = build_message(&account(), &d).unwrap();
        let raw = String::from_utf8(msg.formatted()).unwrap();
        assert!(raw.contains("In-Reply-To: <abc123@example.com>"));
        assert!(raw.contains("Subject: RE: Quarterly numbers"));
        assert!(raw.contains("Content-Type: multipart/alternative"));
        assert!(raw.contains("Content-Type: text/html; charset=utf-8"));

        // The HTML part round-trips through our own parser.
        let sent = parse_body(&msg.formatted());
        let html = sent.html.expect("html part");
        assert!(html.contains("<span style=\"font-weight:bold\">Thanks!</span>"));
        assert!(html.contains("divRplyFwdMsg"));
        assert!(sent.text.starts_with("Thanks!"));

        d.plain_text = true;
        let raw = String::from_utf8(build_message(&account(), &d).unwrap().formatted()).unwrap();
        assert!(!raw.contains("text/html"));
        d.to.clear();
        assert!(build_message(&account(), &d).is_err());
    }

    #[test]
    fn quotes_html_original_without_scripts() {
        let orig = MessageBody {
            from: "A <a@x>".into(),
            subject: "Hi".into(),
            html: Some("<html><head><title>t</title></head><BODY class=x><p>Hello</p><script>alert(1)</script></body></html>".into()),
            ..Default::default()
        };
        let q = quote_html(&orig);
        assert!(q.ends_with("<p>Hello</p>"));
        assert!(!q.contains("script") && !q.contains("<title>"));
    }
}
