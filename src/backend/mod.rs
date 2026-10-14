//! Mail/calendar protocol backends behind one interface, so the worker's
//! caching, previews and UI events work the same for IMAP, Microsoft Graph
//! (Microsoft 365 / Outlook.com) and EWS (Exchange Server).

pub mod ews;
pub mod graph_mail;
pub mod imap;

use std::sync::mpsc::Sender;
use std::time::Instant;

use crate::calendar::{Event as CalEvent, Response};
use crate::model::{AccountConfig, AuthMethod, Draft, Folder, MessageMeta, Protocol};
use crate::oauth;
use crate::store::{self, FolderCache};
use crate::worker::Event;

pub type Res<T> = Result<T, String>;

pub fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

const NO_CALENDAR: &str = "This account has no calendar on the server.";

/// What a folder sync found on the server.
pub struct SyncOutcome {
    /// Messages in the folder on the server (may exceed the cached window).
    pub total: u32,
    /// Cached message numbers that are gone from the server (or the window).
    pub removed: Vec<u32>,
}

/// One account's connection to its server. All methods block; they run on the
/// account's worker thread.
pub trait Backend {
    /// The folder list with total/unread counts.
    fn folders(&mut self) -> Res<Vec<Folder>>;
    /// Bring `cache` up to date with the newest `window` messages of `folder`.
    fn sync(&mut self, folder: &Folder, cache: &mut FolderCache, window: usize) -> Res<SyncOutcome>;
    /// Full RFC 5322 source of messages, as (message number, raw bytes).
    fn fetch(&mut self, folder: &Folder, msgs: &[MessageMeta]) -> Res<Vec<(u32, Vec<u8>)>>;
    fn set_seen(&mut self, folder: &Folder, msgs: &[MessageMeta], seen: bool) -> Res<()>;
    fn set_flagged(&mut self, folder: &Folder, msg: &MessageMeta, flagged: bool) -> Res<()>;
    fn move_to(&mut self, folder: &Folder, msgs: &[MessageMeta], dest: &Folder) -> Res<()>;
    fn delete_permanently(&mut self, folder: &Folder, msgs: &[MessageMeta]) -> Res<()>;
    /// Send, and make sure a copy lands in Sent Items.
    fn send(&mut self, draft: &Draft, folders: &[Folder]) -> Res<()>;
    /// Drop any connection after an error; the next call reconnects.
    fn reset(&mut self) {}

    fn calendar_view(&mut self, _from: i64, _to: i64) -> Res<Vec<CalEvent>> {
        Err(NO_CALENDAR.into())
    }
    /// Create (empty id) or update an event.
    fn calendar_save(&mut self, _event: &CalEvent) -> Res<()> {
        Err(NO_CALENDAR.into())
    }
    fn calendar_delete(&mut self, _id: &str) -> Res<()> {
        Err(NO_CALENDAR.into())
    }
    fn calendar_respond(&mut self, _id: &str, _response: Response) -> Res<()> {
        Err(NO_CALENDAR.into())
    }
}

/// The backend for an account's protocol.
pub fn create(config: AccountConfig, emitter: Emitter) -> Box<dyn Backend> {
    let auth = Auth::new(config.clone(), emitter.clone());
    match config.protocol {
        Protocol::Imap => Box::new(imap::ImapBackend::new(auth)),
        Protocol::Graph => Box::new(graph_mail::GraphBackend::new(auth)),
        Protocol::Ews => Box::new(ews::EwsBackend::new(auth)),
    }
}

/// Sends events to the UI and wakes it up.
#[derive(Clone)]
pub struct Emitter {
    pub account: String,
    pub tx: Sender<Event>,
    pub ctx: eframe::egui::Context,
}

impl Emitter {
    pub fn emit(&self, ev: Event) {
        let _ = self.tx.send(ev);
        self.ctx.request_repaint();
    }

    pub fn status(&self, text: impl Into<String>) {
        self.emit(Event::Status { account: self.account.clone(), text: text.into() });
    }
}

/// Credential Manager entry holding an IMAP account's Graph (calendar) refresh token.
pub fn graph_key(account_id: &str) -> String {
    format!("{account_id}#graph")
}

/// Passwords and OAuth tokens for one account.
pub struct Auth {
    pub config: AccountConfig,
    pub emitter: Emitter,
    /// Access token from the account's stored refresh token.
    token: Option<oauth::Tokens>,
    /// IMAP + Microsoft accounts: separate Graph token for the calendar.
    calendar_token: Option<oauth::Tokens>,
}

impl Auth {
    pub fn new(config: AccountConfig, emitter: Emitter) -> Self {
        Self { config, emitter, token: None, calendar_token: None }
    }

    pub fn password(&self) -> Res<String> {
        store::get_password(&self.config.id)
    }

    pub fn token_expired(&self) -> bool {
        self.token.as_ref().is_none_or(|t| Instant::now() >= t.expires_at)
    }

    /// A current access token, refreshing with the stored refresh token for
    /// `scopes` (None = the provider's IMAP/SMTP scopes).
    pub fn access_token(&mut self, scopes: Option<&'static str>) -> Res<String> {
        if self.token_expired() {
            self.emitter.status("Signing in...");
            let stored = self.password()?;
            let method = self.config.auth;
            let tokens = oauth::refresh_scoped(method, &oauth::load_client(method), &stored, scopes)
                .map_err(|e| format!("{e} Open File > Account Settings and sign in again."))?;
            // Microsoft rotates refresh tokens; keep the newest one.
            if let Some(r) = &tokens.refresh_token {
                store::set_password(&self.config.id, r)?;
            }
            self.token = Some(tokens);
        }
        Ok(self.token.as_ref().unwrap().access_token.clone())
    }

    /// IMAP + Microsoft accounts: a Graph token for the calendar. Uses the
    /// calendar's own refresh token, or tries the mail sign-in's.
    pub fn calendar_token(&mut self) -> Res<String> {
        if let Some(t) = self.calendar_token.as_ref().filter(|t| Instant::now() < t.expires_at) {
            return Ok(t.access_token.clone());
        }
        if self.config.auth != AuthMethod::Microsoft {
            return Err(NO_CALENDAR.into());
        }
        let client = oauth::load_client(AuthMethod::Microsoft);
        let key = graph_key(&self.config.id);
        let refresh_token = store::get_password(&key).or_else(|_| self.password())?;
        match oauth::refresh_scoped(AuthMethod::Microsoft, &client, &refresh_token, Some(oauth::GRAPH_CALENDAR_SCOPES)) {
            Ok(tokens) => {
                if let Some(r) = &tokens.refresh_token {
                    store::set_password(&key, r)?;
                }
                let access = tokens.access_token.clone();
                self.calendar_token = Some(tokens);
                Ok(access)
            }
            Err(e) => {
                self.emitter.emit(Event::CalendarNeedsConnect { account: self.config.id.clone() });
                Err(format!("Calendar access needs a one-time sign-in. {e}"))
            }
        }
    }
}

/// For Graph and EWS: merge a server listing (newest first, `remote_id` set)
/// into the cache. Known items keep their local number and cached preview;
/// new ones get the next number; items no longer listed are removed.
pub fn merge_remote(cache: &mut FolderCache, listing: Vec<MessageMeta>) -> Vec<u32> {
    if cache.uid_validity == 0 {
        cache.uid_validity = 1;
    }
    let mut next = cache.next_uid.max(cache.messages.iter().map(|m| m.uid).max().unwrap_or(0) + 1);
    let mut old: std::collections::HashMap<String, MessageMeta> =
        cache.messages.drain(..).map(|m| (m.remote_id.clone(), m)).collect();
    for mut m in listing {
        match old.remove(&m.remote_id) {
            Some(prev) => {
                m.uid = prev.uid;
                if m.preview.is_empty() {
                    m.preview = prev.preview;
                }
            }
            None => {
                m.uid = next;
                next += 1;
            }
        }
        cache.messages.push(m);
    }
    cache.next_uid = next;
    old.into_values().map(|m| m.uid).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: &str, preview: &str) -> MessageMeta {
        MessageMeta { remote_id: id.into(), preview: preview.into(), ..Default::default() }
    }

    #[test]
    fn merge_keeps_numbers_and_drops_missing() {
        let mut cache = FolderCache::default();
        assert!(merge_remote(&mut cache, vec![meta("a", "pa"), meta("b", "")]).is_empty());
        let uid_of = |c: &FolderCache, id: &str| c.messages.iter().find(|m| m.remote_id == id).map(|m| m.uid);
        let (a, b) = (uid_of(&cache, "a").unwrap(), uid_of(&cache, "b").unwrap());
        assert_ne!(a, b);
        assert_eq!(cache.uid_validity, 1);

        // "b" deleted on the server, "c" arrives; "a" keeps its number and preview.
        let removed = merge_remote(&mut cache, vec![meta("c", ""), meta("a", "")]);
        assert_eq!(removed, vec![b]);
        assert_eq!(uid_of(&cache, "a"), Some(a));
        assert_eq!(cache.messages.iter().find(|m| m.remote_id == "a").unwrap().preview, "pa");
        let c = uid_of(&cache, "c").unwrap();
        assert!(c > a && c > b, "new items never reuse numbers");
    }
}
