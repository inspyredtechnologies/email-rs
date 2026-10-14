//! Application state, the action queue the UI feeds, and worker event handling.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::mime::{self, ReplyKind};
use crate::model::{AccountConfig, Draft, Folder, FolderKind, MessageBody, MessageMeta};
use crate::worker::{self, Command, Event, WorkerHandle};
use crate::{demo, store, theme, ui};

/// Outlook's default automatic Send/Receive interval is 30 minutes; we poll a bit more often.
const AUTO_SYNC: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Module {
    Mail,
    Calendar,
    People,
    Tasks,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RibbonTab {
    Home,
    SendReceive,
    Folder,
    View,
}

pub struct AccountState {
    pub config: AccountConfig,
    pub folders: Vec<Folder>,
    pub messages: HashMap<String, Vec<MessageMeta>>,
    pub uid_validity: HashMap<String, u32>,
    pub status: String,
    pub error: Option<String>,
}

impl AccountState {
    fn new(config: AccountConfig) -> Self {
        Self {
            config,
            folders: Vec::new(),
            messages: HashMap::new(),
            uid_validity: HashMap::new(),
            status: String::new(),
            error: None,
        }
    }

    pub fn folder_of_kind(&self, kind: FolderKind) -> Option<&Folder> {
        self.folders.iter().find(|f| f.kind == kind)
    }
}

pub struct Composer {
    pub id: u64,
    pub draft: Draft,
    pub show_bcc: bool,
    pub error: Option<String>,
    pub confirm_discard: bool,
    pub open: bool,
}

pub enum Action {
    SelectFolder { account: String, folder: String },
    SelectMessage(u32),
    NewMail,
    Respond(ReplyKind),
    Delete,
    Archive,
    MoveTo(String),
    SetRead(bool),
    ToggleFlag(u32),
    SendReceive,
    UpdateFolder,
    MarkAllRead,
    AddAccount,
    EditAccount(String),
    RemoveAccount(String),
    SaveAttachment(usize),
    Send(u64),
}

pub struct Screenshot {
    pub path: PathBuf,
    pub compose: bool,
}

pub struct App {
    pub accounts: Vec<AccountState>,
    workers: HashMap<String, WorkerHandle>,
    events_tx: Sender<Event>,
    events_rx: Receiver<Event>,
    pub module: Module,
    pub ribbon_tab: RibbonTab,
    pub backstage: bool,
    /// (account id, folder path)
    pub current: Option<(String, String)>,
    pub selected: Option<u32>,
    pub bodies: HashMap<(String, String, u32), Arc<MessageBody>>,
    pub search: String,
    pub focus_search: bool,
    pub unread_only: bool,
    pub show_reading_pane: bool,
    pub show_folder_pane: bool,
    pub composers: Vec<Composer>,
    next_composer: u64,
    pub account_dialog: Option<ui::account::AccountDialog>,
    pub confirm_remove: Option<String>,
    pub alert: Option<String>,
    pub actions: Vec<Action>,
    pub demo: bool,
    last_sync: Instant,
    ctx: egui::Context,
    screenshot: Option<Screenshot>,
    frames: u64,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, demo_mode: bool, screenshot: Option<Screenshot>) -> Self {
        theme::install(&cc.egui_ctx);
        let (events_tx, events_rx) = channel();
        let mut app = App {
            accounts: Vec::new(),
            workers: HashMap::new(),
            events_tx,
            events_rx,
            module: Module::Mail,
            ribbon_tab: RibbonTab::Home,
            backstage: false,
            current: None,
            selected: None,
            bodies: HashMap::new(),
            search: String::new(),
            focus_search: false,
            unread_only: false,
            show_reading_pane: true,
            show_folder_pane: true,
            composers: Vec::new(),
            next_composer: 1,
            account_dialog: None,
            confirm_remove: None,
            alert: None,
            actions: Vec::new(),
            demo: demo_mode,
            last_sync: Instant::now(),
            ctx: cc.egui_ctx.clone(),
            screenshot,
            frames: 0,
        };

        if demo_mode {
            app.load_demo();
        } else {
            for config in store::load_accounts() {
                let mut acct = AccountState::new(config.clone());
                acct.folders = store::load_folders(&config.id);
                for f in &acct.folders {
                    let cache = store::load_folder_cache(&config.id, &f.path);
                    acct.uid_validity.insert(f.path.clone(), cache.uid_validity);
                    acct.messages.insert(f.path.clone(), cache.messages);
                }
                app.accounts.push(acct);
                app.start_worker(config);
            }
            if app.accounts.is_empty() {
                // First run: Outlook opens straight into the Add Account wizard.
                app.account_dialog = Some(ui::account::AccountDialog::new());
            }
        }
        if let Some(acct) = app.accounts.first() {
            let inbox = acct.folder_of_kind(FolderKind::Inbox).map(|f| f.path.clone()).unwrap_or_else(|| "INBOX".into());
            app.current = Some((acct.config.id.clone(), inbox));
        }
        if app.screenshot.as_ref().is_some_and(|s| s.compose) {
            // Embed the compose window in the main one so a single capture shows it.
            cc.egui_ctx.set_embed_viewports(true);
            app.actions.push(Action::Respond(ReplyKind::Reply));
        }
        app
    }

    fn load_demo(&mut self) {
        let mut acct = AccountState::new(demo::account());
        acct.folders = demo::folders();
        let now = chrono::Utc::now().timestamp();
        for (folder, meta, body) in demo::messages(now) {
            self.bodies.insert((demo::ACCOUNT_ID.into(), folder.clone(), meta.uid), Arc::new(body));
            acct.messages.entry(folder).or_default().push(meta);
        }
        acct.status = "All folders are up to date.".into();
        self.accounts.push(acct);
        self.selected = Some(1);
    }

    fn start_worker(&mut self, config: AccountConfig) {
        let id = config.id.clone();
        let handle = worker::spawn(config, self.events_tx.clone(), self.ctx.clone());
        handle.send(Command::SyncAll);
        self.workers.insert(id, handle);
    }

    fn send_cmd(&self, account: &str, cmd: Command) {
        if let Some(w) = self.workers.get(account) {
            w.send(cmd);
        }
    }

    pub fn account(&self, id: &str) -> Option<&AccountState> {
        self.accounts.iter().find(|a| a.config.id == id)
    }

    fn account_mut(&mut self, id: &str) -> Option<&mut AccountState> {
        self.accounts.iter_mut().find(|a| a.config.id == id)
    }

    pub fn current_account(&self) -> Option<&AccountState> {
        self.current.as_ref().and_then(|(a, _)| self.account(a))
    }

    pub fn current_folder(&self) -> Option<&Folder> {
        let (_, path) = self.current.as_ref()?;
        self.current_account()?.folders.iter().find(|f| &f.path == path)
    }

    /// The message list after search and the All/Unread filter, newest first.
    pub fn visible_messages(&self) -> Vec<&MessageMeta> {
        let Some((acct, folder)) = &self.current else { return Vec::new() };
        let Some(list) = self.account(acct).and_then(|a| a.messages.get(folder)) else { return Vec::new() };
        let needle = self.search.trim().to_lowercase();
        let mut out: Vec<&MessageMeta> = list
            .iter()
            .filter(|m| !self.unread_only || !m.seen)
            .filter(|m| {
                needle.is_empty()
                    || [&m.subject, &m.from_name, &m.from_addr, &m.preview, &m.to]
                        .iter()
                        .any(|s| s.to_lowercase().contains(&needle))
            })
            .collect();
        out.sort_by(|a, b| b.date.cmp(&a.date).then(b.uid.cmp(&a.uid)));
        out
    }

    pub fn selected_meta(&self) -> Option<&MessageMeta> {
        let uid = self.selected?;
        let (acct, folder) = self.current.as_ref()?;
        self.account(acct)?.messages.get(folder)?.iter().find(|m| m.uid == uid)
    }

    pub fn selected_body(&self) -> Option<Arc<MessageBody>> {
        let (acct, folder) = self.current.as_ref()?;
        self.bodies.get(&(acct.clone(), folder.clone(), self.selected?)).cloned()
    }

    fn meta_mut(&mut self, uid: u32) -> Option<&mut MessageMeta> {
        let (acct, folder) = self.current.clone()?;
        self.account_mut(&acct)?.messages.get_mut(&folder)?.iter_mut().find(|m| m.uid == uid)
    }

    // ---------------------------------------------------------------- events

    fn process_events(&mut self) {
        while let Ok(ev) = self.events_rx.try_recv() {
            match ev {
                Event::Folders { account, folders } => {
                    if let Some(a) = self.account_mut(&account) {
                        a.folders = folders;
                    }
                    // A brand-new account has no folder yet; land on its Inbox.
                    if self.current.as_ref().is_some_and(|(a, f)| *a == account && self.account(a).is_some_and(|s| !s.folders.iter().any(|x| &x.path == f))) {
                        if let Some(inbox) = self.account(&account).and_then(|a| a.folder_of_kind(FolderKind::Inbox)) {
                            self.current = Some((account.clone(), inbox.path.clone()));
                        }
                    }
                }
                Event::Messages { account, folder, uid_validity, messages } => {
                    if let Some(a) = self.account_mut(&account) {
                        a.uid_validity.insert(folder.clone(), uid_validity);
                        a.messages.insert(folder.clone(), messages);
                    }
                    let viewing = self.current.as_ref().is_some_and(|(a, f)| *a == account && *f == folder);
                    if viewing && self.selected.is_some() && self.selected_meta().is_none() {
                        self.selected = None;
                    }
                }
                Event::Body { account, folder, uid, body } => {
                    self.bodies.insert((account, folder, uid), body);
                }
                Event::Status { account, text } => {
                    if let Some(a) = self.account_mut(&account) {
                        a.status = text;
                        a.error = None;
                    }
                }
                Event::Error { account, text } => {
                    if let Some(a) = self.account_mut(&account) {
                        a.status = "Disconnected".into();
                        a.error = Some(text);
                    }
                }
                Event::SendFailed { draft, error } => {
                    let id = self.open_composer(*draft);
                    if let Some(c) = self.composers.iter_mut().find(|c| c.id == id) {
                        c.error = Some(error);
                    }
                }
            }
        }
    }

    // --------------------------------------------------------------- actions

    pub fn open_composer(&mut self, draft: Draft) -> u64 {
        let id = self.next_composer;
        self.next_composer += 1;
        self.composers.push(Composer { id, draft, show_bcc: false, error: None, confirm_discard: false, open: true });
        id
    }

    fn select_message(&mut self, uid: u32) {
        self.selected = Some(uid);
        let Some((acct, folder)) = self.current.clone() else { return };
        let key = (acct.clone(), folder.clone(), uid);
        if !self.bodies.contains_key(&key) {
            let uidv = self.account(&acct).and_then(|a| a.uid_validity.get(&folder).copied()).unwrap_or(0);
            match store::load_body(&acct, &folder, uidv, uid) {
                Some(raw) => {
                    self.bodies.insert(key, Arc::new(mime::parse_body(&raw)));
                }
                None => self.send_cmd(&acct, Command::FetchBody { folder: folder.clone(), uid }),
            }
        }
        if self.selected_meta().is_some_and(|m| !m.seen) {
            self.set_read(true);
        }
    }

    fn set_read(&mut self, read: bool) {
        let Some(uid) = self.selected else { return };
        let Some((acct, folder)) = self.current.clone() else { return };
        let changed = match self.meta_mut(uid) {
            Some(m) if m.seen != read => {
                m.seen = read;
                true
            }
            _ => false,
        };
        if !changed {
            return;
        }
        if let Some(f) = self.account_mut(&acct).and_then(|a| a.folders.iter_mut().find(|f| f.path == folder)) {
            f.unread = if read { f.unread.saturating_sub(1) } else { f.unread + 1 };
        }
        self.send_cmd(&acct, Command::SetSeen { folder, uids: vec![uid], seen: read });
    }

    /// Remove the selected message from the current list (optimistically) and
    /// select the next one, as Outlook does after Delete/Move.
    fn take_selected(&mut self, dest: Option<&str>) -> Option<(String, String, u32)> {
        let uid = self.selected?;
        let (acct, folder) = self.current.clone()?;
        let visible: Vec<u32> = self.visible_messages().iter().map(|m| m.uid).collect();
        let pos = visible.iter().position(|u| *u == uid);
        let next = pos.and_then(|p| visible.get(p + 1).or_else(|| p.checked_sub(1).and_then(|q| visible.get(q)))).copied();
        let demo = self.demo;
        let a = self.account_mut(&acct)?;
        let list = a.messages.get_mut(&folder)?;
        let idx = list.iter().position(|m| m.uid == uid)?;
        let meta = list.remove(idx);
        if let Some(f) = a.folders.iter_mut().find(|f| f.path == folder) {
            f.total = f.total.saturating_sub(1);
            if !meta.seen {
                f.unread = f.unread.saturating_sub(1);
            }
        }
        // Real accounts get the destination list from the server; the demo fakes it.
        if let (true, Some(dest)) = (demo, dest) {
            a.messages.entry(dest.to_string()).or_default().push(meta);
            if let Some(f) = a.folders.iter_mut().find(|f| f.path == dest) {
                f.total += 1;
            }
            if let Some(body) = self.bodies.get(&(acct.clone(), folder.clone(), uid)).cloned() {
                self.bodies.insert((acct.clone(), dest.to_string(), uid), body);
            }
        }
        self.selected = None;
        if let Some(n) = next {
            self.select_message(n);
        }
        Some((acct, folder, uid))
    }

    fn move_selected(&mut self, dest: String) {
        if let Some((acct, folder, uid)) = self.take_selected(Some(&dest)) {
            self.send_cmd(&acct, Command::Move { folder, uids: vec![uid], dest });
        }
    }

    fn respond(&mut self, kind: ReplyKind) {
        let Some(acct) = self.current_account().map(|a| a.config.clone()) else { return };
        if self.selected.is_none() {
            return;
        }
        match self.selected_body() {
            Some(body) => {
                self.open_composer(mime::make_reply(&acct, &body, kind));
            }
            None => self.alert = Some("The message is still downloading. Try again in a moment.".into()),
        }
    }

    fn send_composer(&mut self, id: u64) {
        let Some(c) = self.composers.iter_mut().find(|c| c.id == id) else { return };
        let Some(acct) = self.accounts.iter().find(|a| a.config.id == c.draft.account_id) else {
            c.error = Some("Choose an account to send from.".into());
            return;
        };
        // Validate up front so recipient errors show in the compose window, like Outlook.
        if let Err(e) = mime::build_message(&acct.config, &c.draft) {
            c.error = Some(e);
            return;
        }
        let draft = c.draft.clone();
        c.open = false;
        if self.demo {
            self.alert = Some("Demo mode: the message was not sent.".into());
        } else {
            let account = draft.account_id.clone();
            self.send_cmd(&account, Command::Send(Box::new(draft)));
        }
    }

    pub fn save_account(&mut self, config: AccountConfig, password: Option<String>) {
        if let Some(pw) = password {
            if let Err(e) = store::set_password(&config.id, &pw) {
                self.alert = Some(e);
                return;
            }
        }
        match self.account_mut(&config.id) {
            Some(a) => a.config = config.clone(),
            None => self.accounts.push(AccountState::new(config.clone())),
        }
        let saved: Vec<AccountConfig> =
            self.accounts.iter().filter(|a| a.config.id != demo::ACCOUNT_ID).map(|a| a.config.clone()).collect();
        store::save_accounts(&saved);
        // Dropping the old handle ends that worker thread.
        self.workers.remove(&config.id);
        if self.current.is_none() {
            self.current = Some((config.id.clone(), "INBOX".into()));
        }
        self.start_worker(config);
    }

    fn remove_account(&mut self, id: &str) {
        self.workers.remove(id);
        self.accounts.retain(|a| a.config.id != id);
        self.bodies.retain(|(a, _, _), _| a != id);
        let saved: Vec<AccountConfig> =
            self.accounts.iter().filter(|a| a.config.id != demo::ACCOUNT_ID).map(|a| a.config.clone()).collect();
        store::save_accounts(&saved);
        store::forget_account(id);
        if self.current.as_ref().is_some_and(|(a, _)| a == id) {
            self.current = self.accounts.first().map(|a| (a.config.id.clone(), "INBOX".into()));
            self.selected = None;
        }
    }

    fn save_attachment(&mut self, index: usize) {
        let Some(body) = self.selected_body() else { return };
        let Some(att) = body.attachments.get(index) else { return };
        if let Some(path) = rfd::FileDialog::new().set_title("Save Attachment").set_file_name(&att.name).save_file() {
            if let Err(e) = std::fs::write(&path, &att.data) {
                self.alert = Some(format!("Cannot save the attachment: {e}"));
            }
        }
    }

    fn apply_actions(&mut self) {
        for action in std::mem::take(&mut self.actions) {
            match action {
                Action::SelectFolder { account, folder } => {
                    if self.current.as_ref() != Some(&(account.clone(), folder.clone())) {
                        self.current = Some((account.clone(), folder.clone()));
                        self.selected = None;
                        self.search.clear();
                        self.send_cmd(&account, Command::SyncFolder(folder));
                    }
                    self.module = Module::Mail;
                }
                Action::SelectMessage(uid) => self.select_message(uid),
                Action::NewMail => {
                    let account_id = self.current.as_ref().map(|(a, _)| a.clone()).unwrap_or_default();
                    self.open_composer(Draft { account_id, ..Default::default() });
                }
                Action::Respond(kind) => self.respond(kind),
                Action::Delete => {
                    let trash = self.current_account().and_then(|a| a.folder_of_kind(FolderKind::Deleted)).map(|f| f.path.clone());
                    let in_trash = self.current_folder().is_some_and(|f| f.kind == FolderKind::Deleted);
                    let dest = if in_trash { None } else { trash };
                    if let Some((acct, folder, uid)) = self.take_selected(dest.as_deref()) {
                        self.send_cmd(&acct, Command::Delete { folder, uids: vec![uid] });
                    }
                }
                Action::Archive => {
                    match self.current_account().and_then(|a| a.folder_of_kind(FolderKind::Archive)).map(|f| f.path.clone()) {
                        Some(dest) => self.move_selected(dest),
                        None => self.alert = Some("This account doesn't have an Archive folder.".into()),
                    }
                }
                Action::MoveTo(dest) => self.move_selected(dest),
                Action::SetRead(read) => self.set_read(read),
                Action::ToggleFlag(uid) => {
                    let Some((acct, folder)) = self.current.clone() else { continue };
                    if let Some(m) = self.meta_mut(uid) {
                        m.flagged = !m.flagged;
                        let flagged = m.flagged;
                        self.send_cmd(&acct, Command::SetFlagged { folder, uid, flagged });
                    }
                }
                Action::SendReceive => {
                    self.last_sync = Instant::now();
                    for w in self.workers.values() {
                        w.send(Command::SyncAll);
                    }
                }
                Action::UpdateFolder => {
                    if let Some((acct, folder)) = self.current.clone() {
                        self.send_cmd(&acct, Command::SyncFolder(folder));
                    }
                }
                Action::MarkAllRead => {
                    let Some((acct, folder)) = self.current.clone() else { continue };
                    let Some(a) = self.account_mut(&acct) else { continue };
                    let mut uids = Vec::new();
                    for m in a.messages.get_mut(&folder).into_iter().flatten().filter(|m| !m.seen) {
                        m.seen = true;
                        uids.push(m.uid);
                    }
                    if let Some(f) = a.folders.iter_mut().find(|f| f.path == folder) {
                        f.unread = 0;
                    }
                    if !uids.is_empty() {
                        self.send_cmd(&acct, Command::SetSeen { folder, uids, seen: true });
                    }
                }
                Action::AddAccount => self.account_dialog = Some(ui::account::AccountDialog::new()),
                Action::EditAccount(id) => {
                    if let Some(a) = self.account(&id) {
                        self.account_dialog = Some(ui::account::AccountDialog::edit(&a.config));
                    }
                }
                Action::RemoveAccount(id) => self.remove_account(&id),
                Action::SaveAttachment(i) => self.save_attachment(i),
                Action::Send(id) => self.send_composer(id),
            }
        }
        self.composers.retain(|c| c.open);
    }

    // -------------------------------------------------------------- keyboard

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        let typing = ctx.memory(|m| m.focused().is_some());
        let shortcut = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
        let ctrl_shift = Modifiers::COMMAND | Modifiers::SHIFT;

        if shortcut(Modifiers::COMMAND, Key::N) {
            self.actions.push(Action::NewMail);
        }
        if shortcut(Modifiers::COMMAND, Key::E) {
            self.focus_search = true;
        }
        if shortcut(Modifiers::NONE, Key::F9) {
            self.actions.push(Action::SendReceive);
        }
        if typing {
            return;
        }
        if shortcut(ctrl_shift, Key::R) {
            self.actions.push(Action::Respond(ReplyKind::ReplyAll));
        }
        if shortcut(Modifiers::COMMAND, Key::R) {
            self.actions.push(Action::Respond(ReplyKind::Reply));
        }
        if shortcut(Modifiers::COMMAND, Key::F) {
            self.actions.push(Action::Respond(ReplyKind::Forward));
        }
        if shortcut(Modifiers::COMMAND, Key::Q) {
            self.actions.push(Action::SetRead(true));
        }
        if shortcut(Modifiers::COMMAND, Key::U) {
            self.actions.push(Action::SetRead(false));
        }
        if shortcut(Modifiers::NONE, Key::Delete) || shortcut(Modifiers::COMMAND, Key::D) {
            self.actions.push(Action::Delete);
        }
        if shortcut(Modifiers::NONE, Key::Insert) {
            if let Some(uid) = self.selected {
                self.actions.push(Action::ToggleFlag(uid));
            }
        }
        let down = shortcut(Modifiers::NONE, Key::ArrowDown);
        let up = shortcut(Modifiers::NONE, Key::ArrowUp);
        if down || up {
            let visible: Vec<u32> = self.visible_messages().iter().map(|m| m.uid).collect();
            let pos = self.selected.and_then(|s| visible.iter().position(|u| *u == s));
            let next = match (pos, down) {
                (None, _) => visible.first(),
                (Some(p), true) => visible.get(p + 1),
                (Some(p), false) => p.checked_sub(1).and_then(|q| visible.get(q)),
            };
            if let Some(uid) = next {
                self.actions.push(Action::SelectMessage(*uid));
            }
        }
    }

    // ------------------------------------------------------------ screenshot

    /// `--screenshot <file>`: render a few frames, save a PNG, and exit.
    fn screenshot_logic(&mut self, ctx: &egui::Context) {
        let Some(shot) = &self.screenshot else { return };
        self.frames += 1;
        if self.frames == 10 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            if let Err(e) = crate::save_png(&shot.path, &image) {
                eprintln!("screenshot failed: {e}");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.process_events();
        self.handle_shortcuts(&ctx);
        if self.last_sync.elapsed() >= AUTO_SYNC {
            self.actions.push(Action::SendReceive);
        }
        ctx.request_repaint_after(AUTO_SYNC);

        ui::draw(self, ui);
        ui::compose::show_all(self, &ctx);
        ui::account::show(self, &ctx);
        ui::show_alert(self, &ctx);

        self.apply_actions();
        self.screenshot_logic(&ctx);
    }
}
