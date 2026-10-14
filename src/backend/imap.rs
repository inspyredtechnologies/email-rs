//! IMAP + SMTP backend (any provider). Microsoft accounts on IMAP also get
//! their calendar through Microsoft Graph with a separate token.

use std::collections::HashSet;
use std::net::TcpStream;

use imap::types::{Flag, NameAttribute, StatusAttribute, UnsolicitedResponse};
use native_tls::TlsStream;

use super::{Auth, Backend, Res, SyncOutcome, err};
use crate::calendar::{Event as CalEvent, Response};
use crate::model::{AccountConfig, AuthMethod, Draft, Folder, FolderKind, MessageMeta, classify_folder};
use crate::store::FolderCache;
use crate::{graph, mime, oauth};

pub type Session = imap::Session<TlsStream<TcpStream>>;

/// What to authenticate with: a password, or a current OAuth access token (XOAUTH2).
pub enum Secret {
    Password(String),
    Token(String),
}

/// Connect to the IMAP server and log in.
pub fn open_session(config: &AccountConfig, secret: &Secret) -> Res<Session> {
    let tls = native_tls::TlsConnector::new().map_err(err)?;
    let host = config.imap_host.as_str();
    let addr = (host, config.imap_port);
    let client = if config.imap_port == 143 { imap::connect_starttls(addr, host, &tls) } else { imap::connect(addr, host, &tls) }
        .map_err(|e| format!("Cannot connect to {host}: {e}"))?;
    let result = match secret {
        Secret::Password(pw) => client.login(&config.username, pw),
        Secret::Token(token) => {
            let auth = oauth::XOAuth2 { user: config.username.clone(), access_token: token.clone() };
            client.authenticate("XOAUTH2", &auth)
        }
    };
    result.map_err(|(e, _)| format!("Logon failed for {}: {e}", config.username))
}

/// SMTP transport for the account (implicit TLS on 465, STARTTLS otherwise).
pub fn smtp_transport(config: &AccountConfig, secret: &Secret) -> Res<lettre::SmtpTransport> {
    use lettre::transport::smtp::authentication::{Credentials, Mechanism};
    let host = config.smtp_host.as_str();
    let builder = if config.smtp_port == 465 { lettre::SmtpTransport::relay(host) } else { lettre::SmtpTransport::starttls_relay(host) }
        .map_err(err)?
        .port(config.smtp_port);
    Ok(match secret {
        Secret::Password(pw) => builder.credentials(Credentials::new(config.username.clone(), pw.clone())),
        Secret::Token(token) => builder
            .credentials(Credentials::new(config.username.clone(), token.clone()))
            .authentication(vec![Mechanism::Xoauth2]),
    }
    .build())
}

/// Compress UIDs into an IMAP sequence set, e.g. `1:4,7,9:10`.
pub fn uid_set(uids: &[u32]) -> String {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        out.push(if start == end { start.to_string() } else { format!("{start}:{end}") });
        i += 1;
    }
    out.join(",")
}

fn apply_flags(meta: &mut MessageMeta, flags: &[Flag]) {
    meta.seen = flags.contains(&Flag::Seen);
    meta.flagged = flags.contains(&Flag::Flagged);
    meta.answered = flags.contains(&Flag::Answered);
}

fn uids(msgs: &[MessageMeta]) -> Vec<u32> {
    msgs.iter().map(|m| m.uid).collect()
}

pub struct ImapBackend {
    auth: Auth,
    session: Option<Session>,
    selected: Option<String>,
}

impl ImapBackend {
    pub fn new(auth: Auth) -> Self {
        Self { auth, session: None, selected: None }
    }

    fn secret(&mut self) -> Res<Secret> {
        if self.auth.config.auth == AuthMethod::Password {
            return Ok(Secret::Password(self.auth.password()?));
        }
        Ok(Secret::Token(self.auth.access_token(None)?))
    }

    fn session(&mut self) -> Res<&mut Session> {
        // Servers drop OAuth sessions once the token expires; reconnect proactively.
        if self.session.is_some() && self.auth.config.auth != AuthMethod::Password && self.auth.token_expired() {
            if let Some(mut s) = self.session.take() {
                let _ = s.logout();
            }
        }
        if self.session.is_none() {
            self.auth.emitter.status(format!("Connecting to {}...", self.auth.config.imap_host));
            let secret = self.secret()?;
            self.session = Some(open_session(&self.auth.config, &secret)?);
            self.selected = None;
        }
        Ok(self.session.as_mut().unwrap())
    }

    fn select(&mut self, folder: &str) -> Res<u32> {
        let mb = self.session()?.select(folder).map_err(err)?;
        self.selected = Some(folder.to_string());
        Ok(mb.uid_validity.unwrap_or(0))
    }

    fn capabilities(&mut self) -> Res<(bool, bool)> {
        let caps = self.session()?.capabilities().map_err(err)?;
        Ok((caps.has_str("MOVE"), caps.has_str("UIDPLUS")))
    }

    fn expunge_uids(&mut self, uids: &[u32], uidplus: bool) -> Res<()> {
        let set = uid_set(uids);
        let s = self.session()?;
        s.uid_store(&set, "+FLAGS.SILENT (\\Deleted)").map_err(err)?;
        if uidplus { s.uid_expunge(&set).map(|_| ()) } else { s.expunge().map(|_| ()) }.map_err(err)
    }
}

impl Drop for ImapBackend {
    fn drop(&mut self) {
        if let Some(mut s) = self.session.take() {
            let _ = s.logout();
        }
    }
}

impl Backend for ImapBackend {
    fn folders(&mut self) -> Res<Vec<Folder>> {
        let session = self.session()?;
        let names = session.list(Some(""), Some("*")).map_err(err)?;
        let mut folders = Vec::new();
        for n in names.iter() {
            let attrs: Vec<String> = n
                .attributes()
                .iter()
                .map(|a| match a {
                    NameAttribute::NoInferiors => "\\Noinferiors".to_string(),
                    NameAttribute::NoSelect => "\\Noselect".to_string(),
                    NameAttribute::Marked => "\\Marked".to_string(),
                    NameAttribute::Unmarked => "\\Unmarked".to_string(),
                    NameAttribute::Custom(c) => c.to_string(),
                })
                .collect();
            let selectable = !attrs.iter().any(|a| a.eq_ignore_ascii_case("\\Noselect") || a.eq_ignore_ascii_case("\\NonExistent"));
            folders.push(Folder {
                path: n.name().to_string(),
                delimiter: n.delimiter().map(str::to_string),
                kind: classify_folder(n.name(), &attrs),
                selectable,
                ..Default::default()
            });
        }
        drop(names);
        // Only the first folder of each special kind keeps the Outlook name.
        let mut seen_kinds = HashSet::new();
        for f in &mut folders {
            if f.kind != FolderKind::Other && !seen_kinds.insert(f.kind) {
                f.kind = FolderKind::Other;
            }
        }
        // STATUS results arrive on the unsolicited channel in imap 2.4.
        for f in folders.iter().filter(|f| f.selectable) {
            let _ = session.status(&f.path, "(MESSAGES UNSEEN)");
        }
        while let Ok(resp) = session.unsolicited_responses.try_recv() {
            if let UnsolicitedResponse::Status { mailbox, attributes } = resp {
                if let Some(f) = folders.iter_mut().find(|f| f.path == mailbox) {
                    for a in attributes {
                        match a {
                            StatusAttribute::Messages(n) => f.total = n,
                            StatusAttribute::Unseen(n) => f.unread = n,
                            _ => {}
                        }
                    }
                }
            }
        }
        Ok(folders)
    }

    fn sync(&mut self, folder: &Folder, cache: &mut FolderCache, window: usize) -> Res<SyncOutcome> {
        let uid_validity = self.select(&folder.path)?;
        let mut removed = Vec::new();
        if cache.uid_validity != uid_validity {
            removed.extend(cache.messages.iter().map(|m| m.uid));
            *cache = FolderCache { uid_validity, ..Default::default() };
        }

        let mut all: Vec<u32> = self.session()?.uid_search("ALL").map_err(err)?.into_iter().collect();
        all.sort_unstable();
        let window_uids: Vec<u32> = all[all.len().saturating_sub(window)..].to_vec();
        let window_set: HashSet<u32> = window_uids.iter().copied().collect();

        // Messages expunged on the server (or now outside the window) leave the cache.
        removed.extend(cache.messages.iter().map(|m| m.uid).filter(|u| !window_set.contains(u)));
        cache.messages.retain(|m| window_set.contains(&m.uid));
        let known: HashSet<u32> = cache.messages.iter().map(|m| m.uid).collect();
        let new: Vec<u32> = window_uids.iter().copied().filter(|u| !known.contains(u)).collect();

        for chunk in new.chunks(100) {
            let fetches = self
                .session()?
                .uid_fetch(uid_set(chunk), "(UID FLAGS RFC822.SIZE INTERNALDATE BODY.PEEK[HEADER])")
                .map_err(err)?;
            for f in fetches.iter() {
                let (Some(uid), Some(header)) = (f.uid, f.header()) else { continue };
                let mut meta = mime::parse_meta(uid, header);
                if meta.date == 0 {
                    meta.date = f.internal_date().map(|d| d.timestamp()).unwrap_or(0);
                }
                meta.size = f.size.unwrap_or(meta.size);
                apply_flags(&mut meta, f.flags());
                cache.messages.push(meta);
            }
        }

        // Flag changes made elsewhere (other clients, webmail) for already-known messages.
        if !known.is_empty() {
            let known_vec: Vec<u32> = known.iter().copied().collect();
            let fetches = self.session()?.uid_fetch(uid_set(&known_vec), "(UID FLAGS)").map_err(err)?;
            for f in fetches.iter() {
                if let Some(m) = f.uid.and_then(|uid| cache.messages.iter_mut().find(|m| m.uid == uid)) {
                    apply_flags(m, f.flags());
                }
            }
        }
        Ok(SyncOutcome { total: all.len() as u32, removed })
    }

    fn fetch(&mut self, folder: &Folder, msgs: &[MessageMeta]) -> Res<Vec<(u32, Vec<u8>)>> {
        if self.selected.as_deref() != Some(folder.path.as_str()) {
            self.select(&folder.path)?;
        }
        let fetches = self.session()?.uid_fetch(uid_set(&uids(msgs)), "(UID BODY.PEEK[])").map_err(err)?;
        Ok(fetches.iter().filter_map(|f| Some((f.uid?, f.body()?.to_vec()))).collect())
    }

    fn set_seen(&mut self, folder: &Folder, msgs: &[MessageMeta], seen: bool) -> Res<()> {
        self.select(&folder.path)?;
        let op = if seen { "+FLAGS.SILENT (\\Seen)" } else { "-FLAGS.SILENT (\\Seen)" };
        self.session()?.uid_store(uid_set(&uids(msgs)), op).map_err(err).map(|_| ())
    }

    fn set_flagged(&mut self, folder: &Folder, msg: &MessageMeta, flagged: bool) -> Res<()> {
        self.select(&folder.path)?;
        let op = if flagged { "+FLAGS.SILENT (\\Flagged)" } else { "-FLAGS.SILENT (\\Flagged)" };
        self.session()?.uid_store(msg.uid.to_string(), op).map_err(err).map(|_| ())
    }

    fn move_to(&mut self, folder: &Folder, msgs: &[MessageMeta], dest: &Folder) -> Res<()> {
        self.select(&folder.path)?;
        let (can_move, uidplus) = self.capabilities()?;
        let set = uid_set(&uids(msgs));
        if can_move {
            self.session()?.uid_mv(&set, &dest.path).map_err(err)
        } else {
            self.session()?.uid_copy(&set, &dest.path).map_err(err)?;
            self.expunge_uids(&uids(msgs), uidplus)
        }
    }

    fn delete_permanently(&mut self, folder: &Folder, msgs: &[MessageMeta]) -> Res<()> {
        self.select(&folder.path)?;
        let (_, uidplus) = self.capabilities()?;
        self.expunge_uids(&uids(msgs), uidplus)
    }

    fn send(&mut self, draft: &Draft, folders: &[Folder]) -> Res<()> {
        use lettre::Transport;
        let config = self.auth.config.clone();
        let msg = mime::build_message(&config, draft, false)?;
        let secret = self.secret()?;
        smtp_transport(&config, &secret)?.send(&msg).map_err(|e| format!("Sending reported error: {e}"))?;

        // Gmail and Microsoft 365 file sent mail themselves; others need an APPEND.
        let auto_saves = ["gmail.com", "googlemail.com", "office365.com", "outlook.com"].iter().any(|h| config.smtp_host.ends_with(h));
        if let (false, Some(sent)) = (auto_saves, folders.iter().find(|f| f.kind == FolderKind::Sent)) {
            let raw = msg.formatted();
            if let Err(e) = self.session().and_then(|s| s.append_with_flags(&sent.path, &raw, &[Flag::Seen]).map_err(err)) {
                self.auth.emitter.status(format!("Sent, but could not save a copy to Sent Items: {e}"));
            }
        }
        Ok(())
    }

    fn reset(&mut self) {
        self.session = None;
        self.selected = None;
    }

    fn calendar_view(&mut self, from: i64, to: i64) -> Res<Vec<CalEvent>> {
        let token = self.auth.calendar_token()?;
        graph::calendar_view(&token, &self.auth.config.id, from, to)
    }

    fn calendar_save(&mut self, event: &CalEvent) -> Res<()> {
        let token = self.auth.calendar_token()?;
        if event.id.is_empty() { graph::create(&token, event) } else { graph::update(&token, event) }
    }

    fn calendar_delete(&mut self, id: &str) -> Res<()> {
        graph::delete(&self.auth.calendar_token()?, id)
    }

    fn calendar_respond(&mut self, id: &str, response: Response) -> Res<()> {
        graph::respond(&self.auth.calendar_token()?, id, response)
    }
}

#[cfg(test)]
mod tests {
    use super::uid_set;

    #[test]
    fn compresses_uid_sets() {
        assert_eq!(uid_set(&[5, 1, 2, 3, 9, 10, 7]), "1:3,5,7,9:10");
        assert_eq!(uid_set(&[42]), "42");
        assert_eq!(uid_set(&[]), "");
    }
}
