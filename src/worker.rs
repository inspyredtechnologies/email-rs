//! One background thread per account. It owns the on-disk cache and talks to
//! the server through the account's `Backend` (IMAP, Microsoft Graph or EWS).
//! The UI sends `Command`s and receives `Event`s; it never blocks on the network.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use crate::backend::{self, Backend, Emitter, Res};
use crate::calendar::{self, Response};
use crate::mime;
use crate::model::{AccountConfig, Draft, Folder, FolderKind, MessageBody, MessageMeta};
use crate::store::{self, FolderCache};

pub use crate::backend::graph_key;

/// How many of the newest messages per folder are kept in the local cache.
const WINDOW: usize = 500;
/// How many of the newest messages get their body prefetched (for previews).
const PREFETCH: usize = 25;
const PREFETCH_MAX_SIZE: u32 = 300 * 1024;

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
    /// Fetch calendar events between two UTC timestamps.
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
        .name(format!("mail-{}", config.email))
        .spawn(move || {
            let emitter = Emitter { account: config.id.clone(), tx: events, ctx };
            Worker::new(config, emitter).run(rx)
        })
        .expect("spawn worker thread");
    WorkerHandle { tx }
}

struct Worker {
    config: AccountConfig,
    em: Emitter,
    backend: Box<dyn Backend>,
    folders: Vec<Folder>,
    caches: HashMap<String, FolderCache>,
    /// Last synced calendar range, re-synced after changes.
    cal_range: Option<(i64, i64)>,
}

impl Worker {
    fn new(config: AccountConfig, em: Emitter) -> Self {
        let folders = store::load_folders(&config.id);
        let backend = backend::create(config.clone(), em.clone());
        Self { config, em, backend, folders, caches: HashMap::new(), cal_range: None }
    }

    fn run(mut self, rx: Receiver<Command>) {
        while let Ok(cmd) = rx.recv() {
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
                        self.em.emit(Event::SendFailed { draft, error: e });
                    }
                    Ok(())
                }
                cal => {
                    // Calendar failures are reported without resetting the mail connection.
                    if let Err(text) = self.calendar(cal) {
                        self.em.emit(Event::CalendarError { account: self.config.id.clone(), text });
                    }
                    Ok(())
                }
            };
            if let Err(e) = result {
                // Assume the connection is unusable; reconnect on the next command.
                self.backend.reset();
                self.em.emit(Event::Error { account: self.config.id.clone(), text: e });
            }
        }
    }

    // ------------------------------------------------------------- calendar

    fn calendar(&mut self, cmd: Command) -> Res<()> {
        match cmd {
            Command::CalendarSync { from, to } => self.cal_range = Some((from, to)),
            Command::CalendarSave(event) => self.backend.calendar_save(&event)?,
            Command::CalendarDelete(id) => self.backend.calendar_delete(&id)?,
            Command::CalendarRespond { id, response } => self.backend.calendar_respond(&id, response)?,
            _ => return Ok(()),
        }
        // Refresh so the UI shows the server's view.
        let Some((from, to)) = self.cal_range else { return Ok(()) };
        let events = self.backend.calendar_view(from, to)?;
        calendar::save(&self.config.id, &events);
        self.em.emit(Event::CalendarEvents { account: self.config.id.clone(), events });
        Ok(())
    }

    // ---------------------------------------------------------------- cache

    fn cache(&mut self, folder: &str) -> &mut FolderCache {
        let id = self.config.id.clone();
        self.caches.entry(folder.to_string()).or_insert_with(|| store::load_folder_cache(&id, folder))
    }

    fn publish_folder(&mut self, folder: &str) {
        let account = self.config.id.clone();
        let cache = self.cache(folder);
        cache.messages.sort_by(|a, b| b.date.cmp(&a.date).then(b.uid.cmp(&a.uid)));
        store::save_folder_cache(&account, folder, cache);
        let ev = Event::Messages { account, folder: folder.to_string(), uid_validity: cache.uid_validity, messages: cache.messages.clone() };
        self.em.emit(ev);
    }

    fn emit_folders(&self) {
        store::save_folders(&self.config.id, &self.folders);
        self.em.emit(Event::Folders { account: self.config.id.clone(), folders: self.folders.clone() });
    }

    fn folder_mut(&mut self, path: &str) -> Option<&mut Folder> {
        self.folders.iter_mut().find(|f| f.path == path)
    }

    fn folder(&mut self, path: &str) -> Res<Folder> {
        if self.folders.is_empty() {
            self.refresh_folder_list()?;
        }
        self.folders.iter().find(|f| f.path == path).cloned().ok_or_else(|| format!("The folder \"{path}\" no longer exists."))
    }

    /// Cached metadata for the given message numbers.
    fn metas(&mut self, folder: &str, uids: &[u32]) -> Vec<MessageMeta> {
        self.cache(folder).messages.iter().filter(|m| uids.contains(&m.uid)).cloned().collect()
    }

    // ---------------------------------------------------------------- mail

    fn refresh_folder_list(&mut self) -> Res<()> {
        self.folders = self.backend.folders()?;
        self.emit_folders();
        Ok(())
    }

    fn sync_all(&mut self) -> Res<()> {
        self.em.status("Updating folder list...");
        self.refresh_folder_list()?;
        let mut order: Vec<(String, String)> =
            self.folders.iter().filter(|f| f.selectable).map(|f| (f.path.clone(), f.display_name())).collect();
        // Inbox first, like Outlook.
        let inbox = self.folders.iter().find(|f| f.kind == FolderKind::Inbox).map(|f| f.path.clone());
        order.sort_by_key(|(p, _)| Some(p) != inbox.as_ref());
        let n = order.len();
        for (i, (path, name)) in order.iter().enumerate() {
            self.em.status(format!("Receiving: {name} ({} of {n})", i + 1));
            self.sync_folder(path)?;
        }
        self.emit_folders();
        self.em.status("All folders are up to date.");
        Ok(())
    }

    fn sync_folder(&mut self, path: &str) -> Res<()> {
        let folder = self.folder(path)?;
        let account = self.config.id.clone();
        let mut cache = std::mem::take(self.cache(path));
        let old_uid_validity = cache.uid_validity;
        let outcome = self.backend.sync(&folder, &mut cache, WINDOW);
        let unread = cache.messages.iter().filter(|m| !m.seen).count() as u32;
        let uid_validity = cache.uid_validity;
        *self.cache(path) = cache;
        let outcome = outcome?;
        for uid in outcome.removed {
            store::delete_body(&account, path, old_uid_validity, uid);
        }
        self.publish_folder(path);
        // Unread counts beyond the cache window come from the folder list; within it we know exactly.
        if let Some(f) = self.folder_mut(path) {
            f.total = outcome.total;
            if outcome.total as usize <= WINDOW {
                f.unread = unread;
            }
        }
        self.prefetch_previews(&folder, uid_validity)
    }

    /// Download bodies of the newest small messages so the list can show previews.
    fn prefetch_previews(&mut self, folder: &Folder, uid_validity: u32) -> Res<()> {
        let account = self.config.id.clone();
        let path = folder.path.clone();
        let mut wanted = Vec::new();
        for m in self.cache(&path).messages.iter_mut().take(PREFETCH) {
            if !m.preview.is_empty() {
                continue;
            }
            if let Some(raw) = store::load_body(&account, &path, uid_validity, m.uid) {
                m.preview = mime::preview_of(&mime::parse_body(&raw));
            } else if m.size <= PREFETCH_MAX_SIZE {
                wanted.push(m.clone());
            }
        }
        if !wanted.is_empty() {
            let fetched = self.backend.fetch(folder, &wanted)?;
            let cache = self.cache(&path);
            for (uid, raw) in fetched {
                store::save_body(&account, &path, uid_validity, uid, &raw);
                let body = mime::parse_body(&raw);
                if let Some(m) = cache.messages.iter_mut().find(|m| m.uid == uid) {
                    m.preview = mime::preview_of(&body);
                    m.has_attachments = !body.attachments.is_empty();
                }
            }
        }
        self.publish_folder(&path);
        Ok(())
    }

    fn fetch_body(&mut self, path: &str, uid: u32) -> Res<()> {
        let folder = self.folder(path)?;
        let account = self.config.id.clone();
        let uid_validity = self.cache(path).uid_validity;
        let raw = match store::load_body(&account, path, uid_validity, uid) {
            Some(raw) => raw,
            None => {
                self.em.status("Downloading message...");
                let metas = self.metas(path, &[uid]);
                let raw = self
                    .backend
                    .fetch(&folder, &metas)?
                    .into_iter()
                    .find(|(u, _)| *u == uid)
                    .map(|(_, raw)| raw)
                    .ok_or("The message could not be found on the server. It may have been moved or deleted.")?;
                store::save_body(&account, path, uid_validity, uid, &raw);
                self.em.status("Connected");
                raw
            }
        };
        let body = mime::parse_body(&raw);
        if let Some(m) = self.cache(path).messages.iter_mut().find(|m| m.uid == uid) {
            m.preview = mime::preview_of(&body);
            m.has_attachments = !body.attachments.is_empty();
        }
        self.em.emit(Event::Body { account, folder: path.to_string(), uid, body: Arc::new(body) });
        Ok(())
    }

    fn set_seen(&mut self, path: &str, uids: &[u32], seen: bool) -> Res<()> {
        let folder = self.folder(path)?;
        let metas = self.metas(path, uids);
        self.backend.set_seen(&folder, &metas, seen)?;
        let mut delta: i64 = 0;
        for m in self.cache(path).messages.iter_mut().filter(|m| uids.contains(&m.uid)) {
            if m.seen != seen {
                delta += if seen { -1 } else { 1 };
                m.seen = seen;
            }
        }
        if let Some(f) = self.folder_mut(path) {
            f.unread = (f.unread as i64 + delta).max(0) as u32;
        }
        self.publish_folder(path);
        self.emit_folders();
        Ok(())
    }

    fn set_flagged(&mut self, path: &str, uid: u32, flagged: bool) -> Res<()> {
        let folder = self.folder(path)?;
        let Some(meta) = self.metas(path, &[uid]).pop() else { return Ok(()) };
        self.backend.set_flagged(&folder, &meta, flagged)?;
        if let Some(m) = self.cache(path).messages.iter_mut().find(|m| m.uid == uid) {
            m.flagged = flagged;
        }
        self.publish_folder(path);
        Ok(())
    }

    /// Drop messages from the cache (after a move or delete).
    fn forget(&mut self, path: &str, uids: &[u32]) {
        let account = self.config.id.clone();
        let cache = self.cache(path);
        let uid_validity = cache.uid_validity;
        let unread_removed = cache.messages.iter().filter(|m| uids.contains(&m.uid) && !m.seen).count() as u32;
        cache.messages.retain(|m| !uids.contains(&m.uid));
        for uid in uids {
            store::delete_body(&account, path, uid_validity, *uid);
        }
        if let Some(f) = self.folder_mut(path) {
            f.unread = f.unread.saturating_sub(unread_removed);
            f.total = f.total.saturating_sub(uids.len() as u32);
        }
        self.publish_folder(path);
    }

    fn move_messages(&mut self, path: &str, uids: &[u32], dest: &str) -> Res<()> {
        if path == dest || uids.is_empty() {
            return Ok(());
        }
        let (folder, dest_folder) = (self.folder(path)?, self.folder(dest)?);
        let metas = self.metas(path, uids);
        self.backend.move_to(&folder, &metas, &dest_folder)?;
        self.forget(path, uids);
        self.sync_folder(dest)?;
        self.emit_folders();
        Ok(())
    }

    fn delete(&mut self, path: &str, uids: &[u32]) -> Res<()> {
        let trash = self.folders.iter().find(|f| f.kind == FolderKind::Deleted).map(|f| f.path.clone());
        match trash {
            Some(t) if t != path => self.move_messages(path, uids, &t),
            _ => {
                let folder = self.folder(path)?;
                let metas = self.metas(path, uids);
                self.backend.delete_permanently(&folder, &metas)?;
                self.forget(path, uids);
                self.emit_folders();
                Ok(())
            }
        }
    }

    fn send_mail(&mut self, draft: &Draft) -> Res<()> {
        self.em.status("Sending...");
        if self.folders.is_empty() {
            let _ = self.refresh_folder_list();
        }
        let folders = self.folders.clone();
        self.backend.send(draft, &folders)?;
        if let Some(sent) = self.folders.iter().find(|f| f.kind == FolderKind::Sent).map(|f| f.path.clone()) {
            let _ = self.sync_folder(&sent);
        }
        self.em.status("Message sent.");
        Ok(())
    }
}
