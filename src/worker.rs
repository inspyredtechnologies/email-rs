//! One background thread per account: talks IMAP/SMTP and owns the on-disk cache.
//! The UI sends `Command`s and receives `Event`s; it never blocks on the network.

use std::collections::{HashMap, HashSet};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::Instant;

use imap::types::{Flag, NameAttribute, StatusAttribute, UnsolicitedResponse};
use native_tls::TlsStream;

use crate::calendar::{self, Response};
use crate::model::{AccountConfig, AuthMethod, Draft, Folder, FolderKind, MessageBody, MessageMeta, classify_folder};
use crate::store::{self, FolderCache};
use crate::{mime, oauth};

/// How many of the newest messages per folder are kept in the local cache.
const WINDOW: usize = 500;
/// How many of the newest messages get their body prefetched (for previews).
const PREFETCH: usize = 25;
const PREFETCH_MAX_SIZE: u32 = 300 * 1024;

pub type Session = imap::Session<TlsStream<TcpStream>>;

pub enum Command {
    /// Refresh the folder list and every folder (Outlook's F9).
    SyncAll,
    SyncFolder(String),
    FetchBody { folder: String, uid: u32 },
    SetSeen { folder: String, uids: Vec<u32>, seen: bool },
    SetFlagged { folder: String, uid: u32, flagged: bool },
    Move { folder: String, uids: Vec<u32>, dest: String },
    /// Move to Deleted Items, or delete permanently when already there.
    Delete { folder: String, uids: Vec<u32> },
    Send(Box<Draft>),
    /// Microsoft accounts: fetch calendar events between two UTC timestamps.
    CalendarSync { from: i64, to: i64 },
    /// Create (empty id) or update an event in the account's calendar.
    CalendarSave(Box<calendar::Event>),
    CalendarDelete(String),
    CalendarRespond { id: String, response: Response },
}

pub enum Event {
    Folders { account: String, folders: Vec<Folder> },
    Messages { account: String, folder: String, uid_validity: u32, messages: Vec<MessageMeta> },
    Body { account: String, folder: String, uid: u32, body: Arc<MessageBody> },
    Status { account: String, text: String },
    Error { account: String, text: String },
    SendFailed { draft: Box<Draft>, error: String },
    CalendarEvents { account: String, events: Vec<calendar::Event> },
    CalendarError { account: String, text: String },
    /// The calendar needs a one-time interactive sign-in (Graph consent).
    CalendarNeedsConnect { account: String },
}

pub struct WorkerHandle {
    tx: Sender<Command>,
}

impl WorkerHandle {
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

pub fn spawn(config: AccountConfig, events: Sender<Event>, ctx: eframe::egui::Context) -> WorkerHandle {
    let (tx, rx) = channel();
    thread::Builder::new()
        .name(format!("imap-{}", config.email))
        .spawn(move || Worker::new(config, events, ctx).run(rx))
        .expect("spawn worker thread");
    WorkerHandle { tx }
}

struct Worker {
    config: AccountConfig,
    events: Sender<Event>,
    ctx: eframe::egui::Context,
    session: Option<Session>,
    selected: Option<String>,
    folders: Vec<Folder>,
    caches: HashMap<String, FolderCache>,
    /// Current OAuth access token (OAuth accounts only).
    token: Option<oauth::Tokens>,
    /// Microsoft Graph access token for the calendar.
    graph_token: Option<oauth::Tokens>,
    /// Last synced calendar range, re-synced after changes.
    cal_range: Option<(i64, i64)>,
}

type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Credential Manager entry holding an account's Graph (calendar) refresh token.
pub fn graph_key(account_id: &str) -> String {
    format!("{account_id}#graph")
}

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

impl Worker {
    fn new(config: AccountConfig, events: Sender<Event>, ctx: eframe::egui::Context) -> Self {
        let folders = store::load_folders(&config.id);
        Self { config, events, ctx, session: None, selected: None, folders, caches: HashMap::new(), token: None, graph_token: None, cal_range: None }
    }

    fn emit(&self, ev: Event) {
        let _ = self.events.send(ev);
        self.ctx.request_repaint();
    }

    fn status(&self, text: impl Into<String>) {
        self.emit(Event::Status { account: self.config.id.clone(), text: text.into() });
    }

    fn run(mut self, rx: Receiver<Command>) {
        while let Ok(cmd) = rx.recv() {
            let cmd = match self.calendar(cmd) {
                Ok(()) => continue,
                Err(other) => other,
            };
            let result = match cmd {
                Command::SyncAll => self.sync_all(),
                Command::SyncFolder(f) => self.sync_folder(&f).map(|_| self.emit_folders()),
                Command::FetchBody { folder, uid } => self.fetch_body(&folder, uid),
                Command::SetSeen { folder, uids, seen } => self.set_seen(&folder, &uids, seen),
                Command::SetFlagged { folder, uid, flagged } => self.set_flagged(&folder, uid, flagged),
                Command::Move { folder, uids, dest } => self.move_messages(&folder, &uids, &dest),
                Command::Delete { folder, uids } => self.delete(&folder, &uids),
                Command::Send(draft) => {
                    if let Err(e) = self.send_mail(&draft) {
                        self.emit(Event::SendFailed { draft, error: e });
                    }
                    Ok(())
                }
                _ => Ok(()),
            };
            if let Err(e) = result {
                // Assume the connection is unusable; reconnect on the next command.
                self.session = None;
                self.selected = None;
                self.emit(Event::Error { account: self.config.id.clone(), text: e });
            }
        }
        if let Some(mut s) = self.session.take() {
            let _ = s.logout();
        }
    }

    // ------------------------------------------------------------- calendar

    /// Handle calendar commands; anything else is handed back for the mail loop.
    /// Calendar failures are reported without touching the IMAP connection.
    fn calendar(&mut self, cmd: Command) -> Result<(), Command> {
        let result = match cmd {
            Command::CalendarSync { from, to } => {
                self.cal_range = Some((from, to));
                self.calendar_sync()
            }
            Command::CalendarSave(event) => self.graph_token().and_then(|t| {
                if event.id.is_empty() { crate::graph::create(&t, &event) } else { crate::graph::update(&t, &event) }
            }),
            Command::CalendarDelete(id) => self.graph_token().and_then(|t| crate::graph::delete(&t, &id)),
            Command::CalendarRespond { id, response } => self.graph_token().and_then(|t| crate::graph::respond(&t, &id, response)),
            other => return Err(other),
        };
        // After a change, refresh so the UI shows the server's view.
        let result = result.and_then(|_| if self.cal_range.is_some() { self.calendar_sync() } else { Ok(()) });
        if let Err(text) = result {
            self.emit(Event::CalendarError { account: self.config.id.clone(), text });
        }
        Ok(())
    }

    fn calendar_sync(&mut self) -> Res<()> {
        let Some((from, to)) = self.cal_range else { return Ok(()) };
        let token = self.graph_token()?;
        let events = crate::graph::calendar_view(&token, &self.config.id, from, to)?;
        calendar::save(&self.config.id, &events);
        self.emit(Event::CalendarEvents { account: self.config.id.clone(), events });
        Ok(())
    }

    /// Graph access token. Uses the calendar's own refresh token, or tries the
    /// mail sign-in's (works if the user already consented to calendar access).
    fn graph_token(&mut self) -> Res<String> {
        if let Some(t) = self.graph_token.as_ref().filter(|t| Instant::now() < t.expires_at) {
            return Ok(t.access_token.clone());
        }
        if self.config.auth != AuthMethod::Microsoft {
            return Err("Calendar sync is available for Microsoft accounts only.".into());
        }
        let client = oauth::load_client(AuthMethod::Microsoft);
        let key = graph_key(&self.config.id);
        let refresh_token = store::get_password(&key).or_else(|_| store::get_password(&self.config.id))?;
        match oauth::refresh_scoped(AuthMethod::Microsoft, &client, &refresh_token, Some(oauth::GRAPH_CALENDAR_SCOPES)) {
            Ok(tokens) => {
                if let Some(r) = &tokens.refresh_token {
                    store::set_password(&key, r)?;
                }
                let access = tokens.access_token.clone();
                self.graph_token = Some(tokens);
                Ok(access)
            }
            Err(e) => {
                self.emit(Event::CalendarNeedsConnect { account: self.config.id.clone() });
                Err(format!("Calendar access needs a one-time sign-in. {e}"))
            }
        }
    }

    fn token_expired(&self) -> bool {
        self.token.as_ref().is_none_or(|t| Instant::now() >= t.expires_at)
    }

    /// Password, or a fresh access token obtained with the stored refresh token.
    fn secret(&mut self) -> Res<Secret> {
        let stored = store::get_password(&self.config.id)?;
        let method = self.config.auth;
        if method == AuthMethod::Password {
            return Ok(Secret::Password(stored));
        }
        if self.token_expired() {
            self.status("Signing in...");
            let tokens = oauth::refresh(method, &oauth::load_client(method), &stored)?;
            // Microsoft rotates refresh tokens; keep the newest one.
            if let Some(r) = &tokens.refresh_token {
                store::set_password(&self.config.id, r)?;
            }
            self.token = Some(tokens);
        }
        Ok(Secret::Token(self.token.as_ref().unwrap().access_token.clone()))
    }

    fn session(&mut self) -> Res<&mut Session> {
        // Servers drop OAuth sessions once the token expires; reconnect proactively.
        if self.session.is_some() && self.config.auth != AuthMethod::Password && self.token_expired() {
            if let Some(mut s) = self.session.take() {
                let _ = s.logout();
            }
        }
        if self.session.is_none() {
            self.status(format!("Connecting to {}...", self.config.imap_host));
            let secret = self.secret()?;
            self.session = Some(open_session(&self.config, &secret)?);
            self.selected = None;
        }
        Ok(self.session.as_mut().unwrap())
    }

    fn select(&mut self, folder: &str) -> Res<u32> {
        let mb = self.session()?.select(folder).map_err(err)?;
        self.selected = Some(folder.to_string());
        Ok(mb.uid_validity.unwrap_or(0))
    }

    fn cache(&mut self, folder: &str) -> &mut FolderCache {
        let id = self.config.id.clone();
        self.caches.entry(folder.to_string()).or_insert_with(|| store::load_folder_cache(&id, folder))
    }

    fn publish_folder(&mut self, folder: &str) {
        let account = self.config.id.clone();
        let cache = self.cache(folder);
        cache.messages.sort_by(|a, b| b.date.cmp(&a.date).then(b.uid.cmp(&a.uid)));
        store::save_folder_cache(&account, folder, cache);
        let ev = Event::Messages {
            account,
            folder: folder.to_string(),
            uid_validity: cache.uid_validity,
            messages: cache.messages.clone(),
        };
        self.emit(ev);
    }

    fn emit_folders(&self) {
        store::save_folders(&self.config.id, &self.folders);
        self.emit(Event::Folders { account: self.config.id.clone(), folders: self.folders.clone() });
    }

    fn folder_mut(&mut self, path: &str) -> Option<&mut Folder> {
        self.folders.iter_mut().find(|f| f.path == path)
    }

    fn sync_all(&mut self) -> Res<()> {
        self.status("Updating folder list...");
        self.refresh_folder_list()?;
        let mut order: Vec<String> = self.folders.iter().filter(|f| f.selectable).map(|f| f.path.clone()).collect();
        // Inbox first, like Outlook.
        order.sort_by_key(|p| !p.eq_ignore_ascii_case("INBOX"));
        let n = order.len();
        for (i, path) in order.iter().enumerate() {
            self.status(format!("Receiving: {} ({} of {n})", path, i + 1));
            self.sync_folder(path)?;
        }
        self.emit_folders();
        self.status("All folders are up to date.");
        Ok(())
    }

    fn refresh_folder_list(&mut self) -> Res<()> {
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
                unread: 0,
                total: 0,
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
        self.folders = folders;
        self.emit_folders();
        Ok(())
    }

    fn sync_folder(&mut self, folder: &str) -> Res<()> {
        if self.folders.is_empty() {
            self.refresh_folder_list()?;
        }
        let uid_validity = self.select(folder)?;
        let account = self.config.id.clone();
        {
            let cache = self.cache(folder);
            if cache.uid_validity != uid_validity {
                *cache = FolderCache { uid_validity, messages: Vec::new() };
            }
        }

        let session = self.session()?;
        let mut all: Vec<u32> = session.uid_search("ALL").map_err(err)?.into_iter().collect();
        all.sort_unstable();
        let window: Vec<u32> = all[all.len().saturating_sub(WINDOW)..].to_vec();
        let window_set: HashSet<u32> = window.iter().copied().collect();

        // Messages expunged on the server (or now outside the window) leave the cache.
        let cache = self.cache(folder);
        let removed: Vec<u32> = cache.messages.iter().map(|m| m.uid).filter(|u| !window_set.contains(u)).collect();
        cache.messages.retain(|m| window_set.contains(&m.uid));
        for uid in removed {
            store::delete_body(&account, folder, uid_validity, uid);
        }
        let known: HashSet<u32> = cache.messages.iter().map(|m| m.uid).collect();
        let new: Vec<u32> = window.iter().copied().filter(|u| !known.contains(u)).collect();

        // New headers.
        let mut fresh = Vec::new();
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
                fresh.push(meta);
            }
        }

        // Flag changes made elsewhere (other clients, webmail) for already-known messages.
        let mut flag_updates = Vec::new();
        if !known.is_empty() {
            let known_vec: Vec<u32> = known.iter().copied().collect();
            let fetches = self.session()?.uid_fetch(uid_set(&known_vec), "(UID FLAGS)").map_err(err)?;
            for f in fetches.iter() {
                if let Some(uid) = f.uid {
                    let mut probe = MessageMeta { uid, ..empty_meta() };
                    apply_flags(&mut probe, f.flags());
                    flag_updates.push(probe);
                }
            }
        }

        let cache = self.cache(folder);
        cache.messages.extend(fresh);
        for p in flag_updates {
            if let Some(m) = cache.messages.iter_mut().find(|m| m.uid == p.uid) {
                m.seen = p.seen;
                m.flagged = p.flagged;
                m.answered = p.answered;
            }
        }
        let unread = cache.messages.iter().filter(|m| !m.seen).count() as u32;
        let total = all.len() as u32;
        self.publish_folder(folder);

        // Unread counts beyond the cache window come from STATUS; within it we know exactly.
        if let Some(f) = self.folder_mut(folder) {
            f.total = total;
            if all.len() <= WINDOW {
                f.unread = unread;
            }
        }

        self.prefetch_previews(folder, uid_validity)
    }

    /// Download bodies of the newest small messages so the list can show previews.
    fn prefetch_previews(&mut self, folder: &str, uid_validity: u32) -> Res<()> {
        let account = self.config.id.clone();
        let mut wanted = Vec::new();
        for m in self.cache(folder).messages.iter_mut().take(PREFETCH) {
            if !m.preview.is_empty() {
                continue;
            }
            if let Some(raw) = store::load_body(&account, folder, uid_validity, m.uid) {
                m.preview = mime::preview_of(&mime::parse_body(&raw));
            } else if m.size <= PREFETCH_MAX_SIZE {
                wanted.push(m.uid);
            }
        }
        if !wanted.is_empty() {
            let fetches = self.session()?.uid_fetch(uid_set(&wanted), "(UID BODY.PEEK[])").map_err(err)?;
            let mut previews = Vec::new();
            for f in fetches.iter() {
                if let (Some(uid), Some(raw)) = (f.uid, f.body()) {
                    store::save_body(&account, folder, uid_validity, uid, raw);
                    let body = mime::parse_body(raw);
                    previews.push((uid, mime::preview_of(&body), !body.attachments.is_empty()));
                }
            }
            let cache = self.cache(folder);
            for (uid, preview, has_att) in previews {
                if let Some(m) = cache.messages.iter_mut().find(|m| m.uid == uid) {
                    m.preview = preview;
                    m.has_attachments = has_att;
                }
            }
        }
        self.publish_folder(folder);
        Ok(())
    }

    fn fetch_body(&mut self, folder: &str, uid: u32) -> Res<()> {
        let uid_validity = self.select(folder)?;
        let account = self.config.id.clone();
        let raw = match store::load_body(&account, folder, uid_validity, uid) {
            Some(raw) => raw,
            None => {
                self.status("Downloading message...");
                let fetches = self.session()?.uid_fetch(uid.to_string(), "(UID BODY.PEEK[])").map_err(err)?;
                let raw = fetches
                    .iter()
                    .find_map(|f| f.body().map(<[u8]>::to_vec))
                    .ok_or("The message could not be found on the server. It may have been moved or deleted.")?;
                store::save_body(&account, folder, uid_validity, uid, &raw);
                self.status("Connected");
                raw
            }
        };
        let body = mime::parse_body(&raw);
        if let Some(m) = self.cache(folder).messages.iter_mut().find(|m| m.uid == uid) {
            m.preview = mime::preview_of(&body);
            m.has_attachments = !body.attachments.is_empty();
        }
        self.emit(Event::Body { account, folder: folder.to_string(), uid, body: Arc::new(body) });
        Ok(())
    }

    fn set_seen(&mut self, folder: &str, uids: &[u32], seen: bool) -> Res<()> {
        self.select(folder)?;
        let op = if seen { "+FLAGS.SILENT (\\Seen)" } else { "-FLAGS.SILENT (\\Seen)" };
        self.session()?.uid_store(uid_set(uids), op).map_err(err)?;
        let mut delta: i64 = 0;
        for m in self.cache(folder).messages.iter_mut().filter(|m| uids.contains(&m.uid)) {
            if m.seen != seen {
                delta += if seen { -1 } else { 1 };
                m.seen = seen;
            }
        }
        if let Some(f) = self.folder_mut(folder) {
            f.unread = (f.unread as i64 + delta).max(0) as u32;
        }
        self.publish_folder(folder);
        self.emit_folders();
        Ok(())
    }

    fn set_flagged(&mut self, folder: &str, uid: u32, flagged: bool) -> Res<()> {
        self.select(folder)?;
        let op = if flagged { "+FLAGS.SILENT (\\Flagged)" } else { "-FLAGS.SILENT (\\Flagged)" };
        self.session()?.uid_store(uid.to_string(), op).map_err(err)?;
        if let Some(m) = self.cache(folder).messages.iter_mut().find(|m| m.uid == uid) {
            m.flagged = flagged;
        }
        self.publish_folder(folder);
        Ok(())
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

    fn forget(&mut self, folder: &str, uids: &[u32]) {
        let account = self.config.id.clone();
        let cache = self.cache(folder);
        let uid_validity = cache.uid_validity;
        let unread_removed = cache.messages.iter().filter(|m| uids.contains(&m.uid) && !m.seen).count() as u32;
        cache.messages.retain(|m| !uids.contains(&m.uid));
        for uid in uids {
            store::delete_body(&account, folder, uid_validity, *uid);
        }
        if let Some(f) = self.folder_mut(folder) {
            f.unread = f.unread.saturating_sub(unread_removed);
            f.total = f.total.saturating_sub(uids.len() as u32);
        }
        self.publish_folder(folder);
    }

    fn move_messages(&mut self, folder: &str, uids: &[u32], dest: &str) -> Res<()> {
        if folder == dest || uids.is_empty() {
            return Ok(());
        }
        self.select(folder)?;
        let (can_move, uidplus) = self.capabilities()?;
        let set = uid_set(uids);
        if can_move {
            self.session()?.uid_mv(&set, dest).map_err(err)?;
        } else {
            self.session()?.uid_copy(&set, dest).map_err(err)?;
            self.expunge_uids(uids, uidplus)?;
        }
        self.forget(folder, uids);
        self.sync_folder(dest)?;
        self.emit_folders();
        Ok(())
    }

    fn delete(&mut self, folder: &str, uids: &[u32]) -> Res<()> {
        let trash = self.folders.iter().find(|f| f.kind == FolderKind::Deleted).map(|f| f.path.clone());
        match trash {
            Some(t) if t != folder => self.move_messages(folder, uids, &t),
            _ => {
                self.select(folder)?;
                let (_, uidplus) = self.capabilities()?;
                self.expunge_uids(uids, uidplus)?;
                self.forget(folder, uids);
                self.emit_folders();
                Ok(())
            }
        }
    }

    fn send_mail(&mut self, draft: &Draft) -> Res<()> {
        use lettre::Transport;

        self.status("Sending...");
        let msg = mime::build_message(&self.config, draft)?;
        let secret = self.secret()?;
        let transport = smtp_transport(&self.config, &secret)?;
        transport.send(&msg).map_err(|e| format!("Sending reported error: {e}"))?;

        // Gmail and Microsoft 365 file sent mail themselves; others need an APPEND.
        let auto_saves = ["gmail.com", "googlemail.com", "office365.com", "outlook.com"]
            .iter()
            .any(|h| self.config.smtp_host.ends_with(h));
        let sent = self.folders.iter().find(|f| f.kind == FolderKind::Sent).map(|f| f.path.clone());
        if let (false, Some(sent)) = (auto_saves, sent) {
            let raw = msg.formatted();
            if let Err(e) = self.session().and_then(|s| s.append_with_flags(&sent, &raw, &[Flag::Seen]).map_err(err)) {
                self.status(format!("Sent, but could not save a copy to Sent Items: {e}"));
            }
        }
        if let Some(sent) = self.folders.iter().find(|f| f.kind == FolderKind::Sent).map(|f| f.path.clone()) {
            let _ = self.sync_folder(&sent);
        }
        self.status("Message sent.");
        Ok(())
    }
}

fn empty_meta() -> MessageMeta {
    MessageMeta {
        uid: 0,
        from_name: String::new(),
        from_addr: String::new(),
        to: String::new(),
        subject: String::new(),
        date: 0,
        seen: false,
        flagged: false,
        answered: false,
        has_attachments: false,
        size: 0,
        preview: String::new(),
    }
}

fn apply_flags(meta: &mut MessageMeta, flags: &[Flag]) {
    meta.seen = flags.contains(&Flag::Seen);
    meta.flagged = flags.contains(&Flag::Flagged);
    meta.answered = flags.contains(&Flag::Answered);
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
