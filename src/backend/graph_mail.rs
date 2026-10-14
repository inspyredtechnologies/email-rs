//! Microsoft Graph backend for Microsoft 365 / Exchange Online and Outlook.com:
//! mail through the Graph mail API (no IMAP/SMTP needed) and the calendar.
//! One sign-in covers both (`oauth::GRAPH_MAIL_SCOPES`).

use std::collections::HashMap;

use base64::Engine;
use serde_json::{Value, json};

use super::{Auth, Backend, Res, SyncOutcome, merge_remote};
use crate::calendar::{Event as CalEvent, Response};
use crate::graph::{self, BASE, agent, read, read_bytes, seg};
use crate::model::{Draft, Folder, FolderKind, MessageMeta};
use crate::store::FolderCache;
use crate::{mime, oauth};

const FOLDER_FIELDS: &str = "id,displayName,childFolderCount,unreadItemCount,totalItemCount";
const MESSAGE_FIELDS: &str = "id,subject,from,toRecipients,receivedDateTime,isRead,flag,hasAttachments,bodyPreview";

pub struct GraphBackend {
    auth: Auth,
}

/// Folder names may contain '/', which we use as the path delimiter.
fn path_segment(name: &str) -> String {
    name.replace('/', "\u{2215}")
}

fn parse_time(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s).map(|d| d.timestamp()).unwrap_or(0)
}

fn address(v: &Value) -> (String, String) {
    let e = &v["emailAddress"];
    (e["name"].as_str().unwrap_or_default().to_string(), e["address"].as_str().unwrap_or_default().to_string())
}

/// A Graph message resource as list metadata.
pub fn parse_message(v: &Value) -> Option<MessageMeta> {
    let (from_name, from_addr) = address(&v["from"]);
    let to = v["toRecipients"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|r| match address(r) {
                    (n, a) if n.is_empty() || n == a => a,
                    (n, a) => format!("{n} <{a}>"),
                })
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default();
    Some(MessageMeta {
        remote_id: v["id"].as_str()?.to_string(),
        from_name,
        from_addr,
        to,
        subject: v["subject"].as_str().unwrap_or_default().to_string(),
        date: parse_time(v["receivedDateTime"].as_str().unwrap_or_default()),
        seen: v["isRead"].as_bool().unwrap_or(true),
        flagged: v["flag"]["flagStatus"].as_str() == Some("flagged"),
        has_attachments: v["hasAttachments"].as_bool().unwrap_or(false),
        preview: v["bodyPreview"].as_str().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" "),
        ..Default::default()
    })
}

impl GraphBackend {
    pub fn new(auth: Auth) -> Self {
        Self { auth }
    }

    fn token(&mut self) -> Res<String> {
        self.auth.access_token(Some(oauth::GRAPH_MAIL_SCOPES))
    }

    fn get(&mut self, url: &str) -> Res<Value> {
        let token = self.token()?;
        read(agent().get(url).header("Authorization", format!("Bearer {token}")).call())
    }

    /// GET a collection, following @odata.nextLink until `limit` items.
    fn get_all(&mut self, url: &str, limit: usize) -> Res<Vec<Value>> {
        let mut out = Vec::new();
        let mut next = Some(url.to_string());
        while let Some(url) = next.take() {
            let v = self.get(&url)?;
            out.extend(v["value"].as_array().cloned().unwrap_or_default());
            if out.len() < limit {
                next = v["@odata.nextLink"].as_str().map(str::to_string);
            }
        }
        out.truncate(limit);
        Ok(out)
    }

    fn send_json(&mut self, method: &str, url: &str, body: Value) -> Res<Value> {
        let token = self.token()?;
        let a = agent();
        let auth = format!("Bearer {token}");
        read(match method {
            "PATCH" => a.patch(url).header("Authorization", auth).send_json(body),
            _ => a.post(url).header("Authorization", auth).send_json(body),
        })
    }
}

impl Backend for GraphBackend {
    fn folders(&mut self) -> Res<Vec<Folder>> {
        // Well-known folders are addressed by name; map their ids to Outlook's folder kinds.
        let mut kinds = HashMap::new();
        for (name, kind) in [
            ("inbox", FolderKind::Inbox),
            ("drafts", FolderKind::Drafts),
            ("sentitems", FolderKind::Sent),
            ("deleteditems", FolderKind::Deleted),
            ("junkemail", FolderKind::Junk),
            ("archive", FolderKind::Archive),
        ] {
            if let Ok(v) = self.get(&format!("{BASE}/me/mailFolders/{name}?$select=id")) {
                if let Some(id) = v["id"].as_str() {
                    kinds.insert(id.to_string(), kind);
                }
            }
        }
        let mut folders = Vec::new();
        // Depth-first so children follow their parent.
        let mut stack: Vec<(Option<String>, String)> = vec![(None, String::new())];
        while let Some((parent, prefix)) = stack.pop() {
            let url = match &parent {
                None => format!("{BASE}/me/mailFolders?$top=250&$select={FOLDER_FIELDS}"),
                Some(id) => format!("{BASE}/me/mailFolders/{}/childFolders?$top=250&$select={FOLDER_FIELDS}", seg(id)),
            };
            for f in self.get_all(&url, usize::MAX)? {
                let Some(id) = f["id"].as_str() else { continue };
                let name = path_segment(f["displayName"].as_str().unwrap_or("Folder"));
                let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
                if f["childFolderCount"].as_u64().unwrap_or(0) > 0 {
                    stack.push((Some(id.to_string()), path.clone()));
                }
                folders.push(Folder {
                    path,
                    delimiter: Some("/".into()),
                    kind: kinds.get(id).copied().unwrap_or(FolderKind::Other),
                    selectable: true,
                    unread: f["unreadItemCount"].as_u64().unwrap_or(0) as u32,
                    total: f["totalItemCount"].as_u64().unwrap_or(0) as u32,
                    remote_id: id.to_string(),
                });
            }
        }
        Ok(folders)
    }

    fn sync(&mut self, folder: &Folder, cache: &mut FolderCache, window: usize) -> Res<SyncOutcome> {
        let id = seg(&folder.remote_id);
        let url = format!(
            "{BASE}/me/mailFolders/{id}/messages?$top={}&$orderby=receivedDateTime%20desc&$select={MESSAGE_FIELDS}",
            window.min(250)
        );
        let listing: Vec<MessageMeta> = self.get_all(&url, window)?.iter().filter_map(parse_message).collect();
        let removed = merge_remote(cache, listing);
        let counts = self.get(&format!("{BASE}/me/mailFolders/{id}?$select=totalItemCount"))?;
        let total = counts["totalItemCount"].as_u64().unwrap_or(cache.messages.len() as u64) as u32;
        Ok(SyncOutcome { total, removed })
    }

    fn fetch(&mut self, _folder: &Folder, msgs: &[MessageMeta]) -> Res<Vec<(u32, Vec<u8>)>> {
        let mut out = Vec::new();
        for m in msgs {
            let token = self.token()?;
            let url = format!("{BASE}/me/messages/{}/$value", seg(&m.remote_id));
            out.push((m.uid, read_bytes(agent().get(&url).header("Authorization", format!("Bearer {token}")).call())?));
        }
        Ok(out)
    }

    fn set_seen(&mut self, _folder: &Folder, msgs: &[MessageMeta], seen: bool) -> Res<()> {
        for m in msgs {
            self.send_json("PATCH", &format!("{BASE}/me/messages/{}", seg(&m.remote_id)), json!({ "isRead": seen }))?;
        }
        Ok(())
    }

    fn set_flagged(&mut self, _folder: &Folder, msg: &MessageMeta, flagged: bool) -> Res<()> {
        let status = if flagged { "flagged" } else { "notFlagged" };
        self.send_json("PATCH", &format!("{BASE}/me/messages/{}", seg(&msg.remote_id)), json!({ "flag": { "flagStatus": status } }))
            .map(|_| ())
    }

    fn move_to(&mut self, _folder: &Folder, msgs: &[MessageMeta], dest: &Folder) -> Res<()> {
        for m in msgs {
            let url = format!("{BASE}/me/messages/{}/move", seg(&m.remote_id));
            self.send_json("POST", &url, json!({ "destinationId": dest.remote_id }))?;
        }
        Ok(())
    }

    fn delete_permanently(&mut self, _folder: &Folder, msgs: &[MessageMeta]) -> Res<()> {
        for m in msgs {
            let token = self.token()?;
            let url = format!("{BASE}/me/messages/{}", seg(&m.remote_id));
            read(agent().delete(&url).header("Authorization", format!("Bearer {token}")).call())?;
        }
        Ok(())
    }

    /// Graph sends the MIME message and saves it to Sent Items itself.
    fn send(&mut self, draft: &Draft, _folders: &[Folder]) -> Res<()> {
        // Keep Bcc in the MIME: Graph takes recipients from the headers.
        let msg = mime::build_message(&self.auth.config, draft, true)?;
        let body = base64::engine::general_purpose::STANDARD.encode(msg.formatted());
        let token = self.token()?;
        read(
            agent()
                .post(&format!("{BASE}/me/sendMail"))
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "text/plain")
                .send(body),
        )
        .map(|_| ())
        .map_err(|e| format!("Sending reported error: {e}"))
    }

    fn calendar_view(&mut self, from: i64, to: i64) -> Res<Vec<CalEvent>> {
        let token = self.token()?;
        graph::calendar_view(&token, &self.auth.config.id, from, to)
    }

    fn calendar_save(&mut self, event: &CalEvent) -> Res<()> {
        let token = self.token()?;
        if event.id.is_empty() { graph::create(&token, event) } else { graph::update(&token, event) }
    }

    fn calendar_delete(&mut self, id: &str) -> Res<()> {
        let token = self.token()?;
        graph::delete(&token, id)
    }

    fn calendar_respond(&mut self, id: &str, response: Response) -> Res<()> {
        let token = self.token()?;
        graph::respond(&token, id, response)
    }
}

/// Whether an email domain's mailboxes are in Exchange Online (Microsoft 365 or
/// Outlook.com), per Exchange Online's Autodiscover v2: it answers with its own
/// EWS URL (or redirects within Exchange Online) for hosted domains, and sends
/// every other domain to that domain's own Autodiscover. A placeholder address
/// is used, so the user's address isn't sent. No sign-in needed.
///
/// (Tenant lookups such as the OpenID configuration aren't usable here: they
/// also resolve for domains like gmail.com that are merely claimed by some tenant.)
pub fn is_microsoft_365_domain(domain: &str) -> bool {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(std::time::Duration::from_secs(8)))
        .build()
        .into();
    let url = format!("https://outlook.office365.com/autodiscover/autodiscover.json/v1.0/probe@{}?Protocol=EWS", seg(domain));
    let Ok(resp) = agent.get(&url).call() else { return false };
    let in_exchange_online = |u: &str| u.starts_with("https://outlook.office365.com/");
    match resp.status().as_u16() {
        200 => resp
            .into_body()
            .read_to_string()
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["Url"].as_str().map(in_exchange_online))
            .unwrap_or(false),
        301 | 302 | 307 | 308 => resp.headers().get("location").and_then(|l| l.to_str().ok()).is_some_and(in_exchange_online),
        _ => false,
    }
}

/// Sign-in check for a new Graph account: can we read the Inbox?
pub fn check(token: &str) -> Res<()> {
    read(agent().get(&format!("{BASE}/me/mailFolders/inbox?$select=id")).header("Authorization", format!("Bearer {token}")).call())
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hits Microsoft's live endpoints; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_graph_and_domain_detection() {
        let err = check("not-a-token").unwrap_err();
        assert!(err.starts_with("Microsoft Graph error 401"), "{err}");
        for hosted in ["microsoft.com", "outlook.com", "hotmail.com"] {
            assert!(is_microsoft_365_domain(hosted), "{hosted}");
        }
        for other in ["example.com", "gmail.com", "fastmail.com"] {
            assert!(!is_microsoft_365_domain(other), "{other}");
        }
    }

    #[test]
    fn parses_graph_message() {
        let v: Value = serde_json::from_str(
            r#"{"id":"AAMkAGI2=","subject":"Q4 plan","receivedDateTime":"2026-10-14T00:12:34Z","isRead":false,
                "hasAttachments":true,"bodyPreview":"Hi team,\r\n here is   the plan",
                "flag":{"flagStatus":"flagged"},
                "from":{"emailAddress":{"name":"Megan Bowen","address":"megan@contoso.com"}},
                "toRecipients":[{"emailAddress":{"name":"Alex","address":"alex@contoso.com"}},
                                {"emailAddress":{"name":"bob@contoso.com","address":"bob@contoso.com"}}]}"#,
        )
        .unwrap();
        let m = parse_message(&v).unwrap();
        assert_eq!(m.remote_id, "AAMkAGI2=");
        assert_eq!((m.from_name.as_str(), m.from_addr.as_str()), ("Megan Bowen", "megan@contoso.com"));
        assert_eq!(m.to, "Alex <alex@contoso.com>; bob@contoso.com");
        assert_eq!(m.date, 1_791_936_754);
        assert!(!m.seen && m.flagged && m.has_attachments);
        assert_eq!(m.preview, "Hi team, here is the plan");
        assert_eq!(path_segment("A/B"), "A\u{2215}B");
        assert_eq!(seg("AA/kz+="), "AA%2Fkz%2B=");
    }
}
