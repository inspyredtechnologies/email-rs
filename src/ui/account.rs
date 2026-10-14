//! Add/Edit Account dialog (account type detection, provider presets, password
//! or OAuth sign-in, and a live connection test) and the File tab "backstage".

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};

use eframe::egui::{self, CentralPanel, Color32, Frame, Margin, Panel, RichText, Ui};

use crate::app::{Action, App};
use crate::backend::imap::{self as imap_backend, Secret};
use crate::backend::{ews, graph_mail};
use crate::model::{AccountConfig, AuthMethod, Protocol};
use crate::oauth::{self, ClientConfig};
use crate::theme::{self, BLUE, SUBTLE, icon};

/// What the user picks as "Account type".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Microsoft Graph with Microsoft sign-in.
    Microsoft365,
    /// EWS with a Windows/Exchange password.
    Exchange,
    /// IMAP/SMTP with password, Microsoft or Google sign-in.
    Imap,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Microsoft365 => "Microsoft 365 / Outlook.com",
            Kind::Exchange => "Exchange Server (on-premises)",
            Kind::Imap => "IMAP / SMTP",
        }
    }

    fn of(c: &AccountConfig) -> Kind {
        match c.protocol {
            Protocol::Graph => Kind::Microsoft365,
            Protocol::Ews => Kind::Exchange,
            Protocol::Imap => Kind::Imap,
        }
    }
}

/// Result of a connection test: the secret to keep, and (Exchange) the EWS URL found.
type TestResult = Result<(String, Option<String>), String>;

pub struct AccountDialog {
    editing: Option<AccountConfig>,
    name: String,
    email: String,
    username: String,
    password: String,
    kind: Kind,
    /// The user chose the type themselves; detection won't override it.
    kind_chosen: bool,
    auth: AuthMethod,
    client: ClientConfig,
    imap_host: String,
    imap_port: String,
    smtp_host: String,
    smtp_port: String,
    ews_url: String,
    advanced: bool,
    /// Domain the server fields were last auto-filled from.
    filled_for: String,
    /// Domain the account type was last detected for, and the pending detection.
    detected_for: String,
    detecting: Option<Receiver<Kind>>,
    error: Option<String>,
    testing: Option<Receiver<TestResult>>,
    cancel: Arc<AtomicBool>,
}

const MS_CONSUMER: [&str; 4] = ["outlook.com", "hotmail.com", "live.com", "msn.com"];
const KNOWN_IMAP: [&str; 10] =
    ["gmail.com", "googlemail.com", "yahoo.com", "ymail.com", "aol.com", "icloud.com", "me.com", "mac.com", "fastmail.com", "fastmail.fm"];

fn note(domain: &str, kind: Kind, auth: AuthMethod) -> Option<&'static str> {
    match (kind, auth) {
        (Kind::Microsoft365, _) => Some(
            "You'll sign in with your Microsoft account in your browser. Mail and calendar use Microsoft Graph, \
             so IMAP and SMTP don't need to be turned on.",
        ),
        (Kind::Exchange, _) => Some(
            "Uses Exchange Web Services with your network sign-in (DOMAIN\\user or your email address). \
             Leave the server blank to find it automatically.",
        ),
        (_, AuthMethod::Microsoft) => Some("You'll sign in with your Microsoft account in your browser (IMAP/SMTP must be enabled)."),
        (_, AuthMethod::Google) => Some("You'll sign in with your Google account in your browser."),
        (_, AuthMethod::Password) => match domain {
            "gmail.com" | "googlemail.com" => {
                Some("Gmail needs an app password (Google Account > Security > App passwords), or choose Google sign-in.")
            }
            "yahoo.com" | "ymail.com" => Some("Yahoo needs an app password: Account Security > Generate app password."),
            "aol.com" => Some("AOL needs an app password."),
            "icloud.com" | "me.com" | "mac.com" => Some("iCloud needs an app-specific password from appleid.apple.com."),
            "fastmail.com" | "fastmail.fm" => Some("Fastmail needs an app password: Settings > Privacy & Security > App passwords."),
            _ => None,
        },
    }
}

fn domain_of(email: &str) -> String {
    email.rsplit_once('@').map(|(_, d)| d.trim().to_lowercase()).unwrap_or_default()
}

fn auth_label(a: AuthMethod) -> &'static str {
    match a {
        AuthMethod::Password => "Password",
        AuthMethod::Microsoft => "Microsoft account",
        AuthMethod::Google => "Google account",
    }
}

/// Guess the account type for a domain: Microsoft 365, then Exchange Server, else IMAP.
fn detect(domain: String, ctx: egui::Context) -> Receiver<Kind> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let kind = if graph_mail::is_microsoft_365_domain(&domain) {
            Kind::Microsoft365
        } else if ews::looks_like_exchange(&domain) {
            Kind::Exchange
        } else {
            Kind::Imap
        };
        let _ = tx.send(kind);
        ctx.request_repaint();
    });
    rx
}

impl AccountDialog {
    pub fn new() -> Self {
        Self {
            editing: None,
            name: String::new(),
            email: String::new(),
            username: String::new(),
            password: String::new(),
            kind: Kind::Imap,
            kind_chosen: false,
            auth: AuthMethod::Password,
            client: ClientConfig::default(),
            imap_host: String::new(),
            imap_port: "993".into(),
            smtp_host: String::new(),
            smtp_port: "587".into(),
            ews_url: String::new(),
            advanced: false,
            filled_for: String::new(),
            detected_for: String::new(),
            detecting: None,
            error: None,
            testing: None,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn edit(c: &AccountConfig) -> Self {
        let domain = domain_of(&c.email);
        Self {
            editing: Some(c.clone()),
            name: c.display_name.clone(),
            email: c.email.clone(),
            username: c.username.clone(),
            kind: Kind::of(c),
            kind_chosen: true,
            auth: c.auth,
            client: oauth::load_client(c.auth),
            imap_host: c.imap_host.clone(),
            imap_port: c.imap_port.to_string(),
            smtp_host: c.smtp_host.clone(),
            smtp_port: c.smtp_port.to_string(),
            ews_url: c.ews_url.clone(),
            advanced: true,
            filled_for: domain.clone(),
            detected_for: domain,
            ..Self::new()
        }
    }

    fn set_servers(&mut self, imap: &str, imap_port: u16, smtp: &str, smtp_port: u16) {
        self.imap_host = imap.into();
        self.imap_port = imap_port.to_string();
        self.smtp_host = smtp.into();
        self.smtp_port = smtp_port.to_string();
    }

    fn set_kind(&mut self, kind: Kind) {
        self.kind = kind;
        match kind {
            Kind::Microsoft365 => self.set_auth(AuthMethod::Microsoft),
            Kind::Exchange => self.set_auth(AuthMethod::Password),
            Kind::Imap => {}
        }
    }

    /// Switch IMAP sign-in method, applying the servers that go with it.
    fn set_auth(&mut self, auth: AuthMethod) {
        self.auth = auth;
        self.client = oauth::load_client(auth);
        if self.kind != Kind::Imap {
            return;
        }
        let domain = domain_of(&self.email);
        match auth {
            AuthMethod::Microsoft => {
                let smtp = if MS_CONSUMER.contains(&domain.as_str()) { "smtp-mail.outlook.com" } else { "smtp.office365.com" };
                self.set_servers("outlook.office365.com", 993, smtp, 587);
            }
            AuthMethod::Google => self.set_servers("imap.gmail.com", 993, "smtp.gmail.com", 465),
            AuthMethod::Password => {}
        }
    }

    /// Fill type, sign-in method and IMAP servers from the email domain when it changes.
    fn autofill(&mut self) {
        let domain = domain_of(&self.email);
        if domain.is_empty() || domain == self.filled_for {
            return;
        }
        self.filled_for = domain.clone();
        self.username = self.email.trim().to_string();
        self.auth = AuthMethod::Password;
        match domain.as_str() {
            "gmail.com" | "googlemail.com" => {
                self.set_servers("imap.gmail.com", 993, "smtp.gmail.com", 465);
                // Prefer Google sign-in when a client is configured; app passwords work otherwise.
                if !oauth::load_client(AuthMethod::Google).client_id.is_empty() {
                    self.auth = AuthMethod::Google;
                    self.client = oauth::load_client(AuthMethod::Google);
                }
            }
            "yahoo.com" | "ymail.com" => self.set_servers("imap.mail.yahoo.com", 993, "smtp.mail.yahoo.com", 465),
            "aol.com" => self.set_servers("imap.aol.com", 993, "smtp.aol.com", 465),
            "icloud.com" | "me.com" | "mac.com" => self.set_servers("imap.mail.me.com", 993, "smtp.mail.me.com", 587),
            "fastmail.com" | "fastmail.fm" => self.set_servers("imap.fastmail.com", 993, "smtp.fastmail.com", 465),
            d => self.set_servers(&format!("imap.{d}"), 993, &format!("smtp.{d}"), 587),
        }
        if !self.kind_chosen {
            if MS_CONSUMER.contains(&domain.as_str()) {
                self.set_kind(Kind::Microsoft365);
                self.detected_for = domain;
            } else if KNOWN_IMAP.contains(&domain.as_str()) {
                self.set_kind(Kind::Imap);
                self.detected_for = domain;
            }
        }
    }

    fn config(&self) -> Result<AccountConfig, String> {
        let email = self.email.trim();
        if !email.contains('@') {
            return Err("Enter a valid email address.".into());
        }
        let port = |s: &str, what: &str| s.trim().parse::<u16>().map_err(|_| format!("The {what} port must be a number."));
        let (protocol, auth) = match self.kind {
            Kind::Microsoft365 => (Protocol::Graph, AuthMethod::Microsoft),
            Kind::Exchange => (Protocol::Ews, AuthMethod::Password),
            Kind::Imap => (Protocol::Imap, self.auth),
        };
        let imap = self.kind == Kind::Imap;
        if imap && (self.imap_host.trim().is_empty() || self.smtp_host.trim().is_empty()) {
            return Err("Enter the incoming and outgoing mail servers.".into());
        }
        Ok(AccountConfig {
            id: self.editing.as_ref().map(|c| c.id.clone()).unwrap_or_else(|| format!("acct-{}", chrono::Utc::now().timestamp_millis())),
            display_name: if self.name.trim().is_empty() { email.to_string() } else { self.name.trim().to_string() },
            email: email.to_string(),
            username: if self.username.trim().is_empty() { email.to_string() } else { self.username.trim().to_string() },
            imap_host: if imap { self.imap_host.trim().to_string() } else { String::new() },
            imap_port: if imap { port(&self.imap_port, "incoming")? } else { 0 },
            smtp_host: if imap { self.smtp_host.trim().to_string() } else { String::new() },
            smtp_port: if imap { port(&self.smtp_port, "outgoing")? } else { 0 },
            auth,
            protocol,
            ews_url: if self.kind == Kind::Exchange { self.ews_url.trim().to_string() } else { String::new() },
        })
    }

    /// Validate, then sign in and test the connection on a background thread.
    fn start(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let config = self.config()?;
        // Reuse the saved password / refresh token when editing without changing how we sign in.
        let saved_secret = self
            .editing
            .as_ref()
            .filter(|c| c.auth == config.auth && c.protocol == config.protocol)
            .and_then(|c| crate::store::get_password(&c.id).ok());
        if config.auth == AuthMethod::Password && self.password.is_empty() && saved_secret.is_none() {
            return Err("Enter your password.".into());
        }
        if config.auth != AuthMethod::Password {
            oauth::save_client(config.auth, &self.client);
        }
        let password = self.password.clone();
        let client = self.client.clone();
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let ctx = ctx.clone();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let result = test_account(&config, &password, saved_secret, &client, cancel);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.testing = Some(rx);
        Ok(())
    }
}

/// OAuth tokens for `scopes`: from the saved refresh token if it still works, else a browser sign-in.
fn oauth_tokens(config: &AccountConfig, saved: Option<String>, client: &ClientConfig, cancel: Arc<AtomicBool>, scopes: Option<&'static str>) -> Result<(String, String), String> {
    let method = config.auth;
    let refreshed = saved.and_then(|r| oauth::refresh_scoped(method, client, &r, scopes).ok().map(|t| (t, r)));
    let (tokens, old_refresh) = match refreshed {
        Some((t, r)) => (t, Some(r)),
        None => (oauth::sign_in_scoped(method, client, &config.email, cancel, scopes)?, None),
    };
    let keep = tokens.refresh_token.clone().or(old_refresh).unwrap_or_default();
    Ok((tokens.access_token, keep))
}

/// Sign in and check the account works. Returns the secret to keep in
/// Credential Manager and, for Exchange, the EWS URL that was used.
fn test_account(config: &AccountConfig, password: &str, saved: Option<String>, client: &ClientConfig, cancel: Arc<AtomicBool>) -> TestResult {
    let typed_or_saved = || if password.is_empty() { saved.clone().unwrap_or_default() } else { password.to_string() };
    match config.protocol {
        Protocol::Graph => {
            let (access, keep) = oauth_tokens(config, saved.clone(), client, cancel, Some(oauth::GRAPH_MAIL_SCOPES))?;
            graph_mail::check(&access)?;
            Ok((keep, None))
        }
        Protocol::Ews => {
            let pw = typed_or_saved();
            let url = ews::check(&config.email, &config.username, &pw, &config.ews_url)?;
            Ok((pw, Some(url)))
        }
        Protocol::Imap => {
            let (secret, keep) = match config.auth {
                AuthMethod::Password => {
                    let pw = typed_or_saved();
                    (Secret::Password(pw.clone()), pw)
                }
                _ => {
                    let (access, keep) = oauth_tokens(config, saved.clone(), client, cancel, None)?;
                    (Secret::Token(access), keep)
                }
            };
            let mut session = imap_backend::open_session(config, &secret)?;
            let _ = session.logout();
            match imap_backend::smtp_transport(config, &secret)?.test_connection() {
                Ok(true) => Ok((keep, None)),
                Ok(false) => Err(format!("Couldn't connect to the outgoing server {}.", config.smtp_host)),
                Err(e) => Err(format!("The outgoing server {} reported an error: {e}", config.smtp_host)),
            }
        }
    }
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(dlg) = app.account_dialog.as_mut() else { return };
    let mut close = false;
    let mut save: Option<(AccountConfig, String)> = None;

    if let Some(rx) = &dlg.detecting {
        if let Ok(kind) = rx.try_recv() {
            dlg.detecting = None;
            if !dlg.kind_chosen {
                dlg.set_kind(kind);
            }
        }
    }
    if let Some(rx) = &dlg.testing {
        if let Ok(result) = rx.try_recv() {
            dlg.testing = None;
            match result {
                Ok((secret, ews_url)) => {
                    if let Some(url) = ews_url {
                        dlg.ews_url = url;
                    }
                    match dlg.config() {
                        Ok(c) => save = Some((c, secret)),
                        Err(e) => dlg.error = Some(e),
                    }
                }
                Err(e) => {
                    dlg.error = Some(e);
                    dlg.advanced = true;
                }
            }
        }
    }

    let modal = egui::Modal::new(egui::Id::new("account_dialog")).show(ctx, |ui| {
        ui.set_width(520.0);
        let title = if dlg.editing.is_some() { "Account Settings" } else { "Add Account" };
        ui.label(RichText::new(title).size(20.0).color(BLUE));
        ui.add_space(4.0);
        if dlg.editing.is_none() {
            ui.label(RichText::new("Enter your email address to connect your account.").color(SUBTLE));
        }
        ui.add_space(10.0);
        let busy = dlg.testing.is_some();
        ui.add_enabled_ui(!busy, |ui| {
            egui::Grid::new("account_fields").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label("Your Name:");
                ui.add(egui::TextEdit::singleline(&mut dlg.name).hint_text("Example: Ellen Adams").desired_width(340.0));
                ui.end_row();
                ui.label("Email Address:");
                let r = ui.add(egui::TextEdit::singleline(&mut dlg.email).hint_text("Example: ellen@contoso.com").desired_width(340.0));
                if r.changed() && dlg.editing.is_none() {
                    dlg.autofill();
                }
                // Look the domain up once the address is complete.
                let domain = domain_of(&dlg.email);
                if r.lost_focus() && !dlg.kind_chosen && domain.contains('.') && domain != dlg.detected_for {
                    dlg.detected_for = domain.clone();
                    dlg.detecting = Some(detect(domain, ui.ctx().clone()));
                }
                ui.end_row();

                ui.label("Account type:");
                ui.horizontal(|ui| {
                    let mut kind = dlg.kind;
                    egui::ComboBox::from_id_salt("account_kind").selected_text(kind.label()).width(250.0).show_ui(ui, |ui| {
                        for k in [Kind::Microsoft365, Kind::Exchange, Kind::Imap] {
                            ui.selectable_value(&mut kind, k, k.label());
                        }
                    });
                    if kind != dlg.kind {
                        dlg.kind_chosen = true;
                        dlg.set_kind(kind);
                    }
                    if dlg.detecting.is_some() {
                        ui.spinner();
                        ui.label(RichText::new("Checking...").small().color(SUBTLE));
                    }
                });
                ui.end_row();

                // Builds that ship with a client registration never ask users for one.
                let builtin = |m: AuthMethod| oauth::builtin_client(m).is_some();
                match dlg.kind {
                    Kind::Microsoft365 => {
                        if !builtin(AuthMethod::Microsoft) {
                            ui.label("Client ID:");
                            ui.add(egui::TextEdit::singleline(&mut dlg.client.client_id).hint_text("From your app registration").desired_width(340.0));
                            ui.end_row();
                        }
                    }
                    Kind::Exchange => {
                        ui.label("User Name:");
                        ui.add(egui::TextEdit::singleline(&mut dlg.username).hint_text("DOMAIN\\user or your email address").desired_width(340.0));
                        ui.end_row();
                    }
                    Kind::Imap => {
                        ui.label("Sign in with:");
                        let mut auth = dlg.auth;
                        egui::ComboBox::from_id_salt("auth_method").selected_text(auth_label(auth)).width(340.0).show_ui(ui, |ui| {
                            for a in [AuthMethod::Password, AuthMethod::Microsoft, AuthMethod::Google] {
                                ui.selectable_value(&mut auth, a, auth_label(a));
                            }
                        });
                        if auth != dlg.auth {
                            dlg.set_auth(auth);
                        }
                        ui.end_row();
                        if dlg.auth != AuthMethod::Password && !builtin(dlg.auth) {
                            ui.label("Client ID:");
                            ui.add(egui::TextEdit::singleline(&mut dlg.client.client_id).hint_text("From your app registration").desired_width(340.0));
                            ui.end_row();
                            if dlg.auth == AuthMethod::Google {
                                ui.label("Client Secret:");
                                ui.add(egui::TextEdit::singleline(&mut dlg.client.client_secret).password(true).desired_width(340.0));
                                ui.end_row();
                            }
                        }
                    }
                }
                if dlg.kind == Kind::Exchange || (dlg.kind == Kind::Imap && dlg.auth == AuthMethod::Password) {
                    ui.label("Password:");
                    let hint = if dlg.editing.is_some() { "Leave blank to keep the saved password" } else { "" };
                    ui.add(egui::TextEdit::singleline(&mut dlg.password).password(true).hint_text(hint).desired_width(340.0));
                    ui.end_row();
                }
            });
            if let Some(n) = note(&domain_of(&dlg.email), dlg.kind, dlg.auth) {
                ui.add_space(6.0);
                ui.label(RichText::new(n).small().color(SUBTLE));
            }
            let uses_oauth = dlg.kind == Kind::Microsoft365 || (dlg.kind == Kind::Imap && dlg.auth != AuthMethod::Password);
            if uses_oauth && dlg.client.client_id.trim().is_empty() {
                ui.label(
                    RichText::new("Paste the client ID of your app registration (one-time setup, see README).").small().color(theme::FLAG_RED),
                );
            }
            ui.add_space(8.0);
            if dlg.kind != Kind::Microsoft365 {
                egui::CollapsingHeader::new("Server settings").open(dlg.advanced.then_some(true)).show(ui, |ui| {
                    if dlg.kind == Kind::Exchange {
                        ui.horizontal(|ui| {
                            ui.label("EWS URL:");
                            ui.add_sized(
                                [380.0, 22.0],
                                egui::TextEdit::singleline(&mut dlg.ews_url).hint_text("https://mail.contoso.com/EWS/Exchange.asmx"),
                            );
                        });
                        ui.label(RichText::new("Leave blank to find it with Autodiscover.").small().color(SUBTLE));
                        return;
                    }
                    egui::Grid::new("server_fields").num_columns(4).spacing([8.0, 8.0]).show(ui, |ui| {
                        ui.label("Incoming (IMAP):");
                        ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut dlg.imap_host));
                        ui.label("Port:");
                        ui.add_sized([50.0, 22.0], egui::TextEdit::singleline(&mut dlg.imap_port));
                        ui.end_row();
                        ui.label("Outgoing (SMTP):");
                        ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut dlg.smtp_host));
                        ui.label("Port:");
                        ui.add_sized([50.0, 22.0], egui::TextEdit::singleline(&mut dlg.smtp_port));
                        ui.end_row();
                        ui.label("User Name:");
                        ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut dlg.username));
                        ui.end_row();
                    });
                    ui.label(RichText::new("Ports 993/465 use SSL/TLS; 143/587 use STARTTLS.").small().color(SUBTLE));
                });
            }
        });
        if let Some(err) = &dlg.error {
            ui.add_space(8.0);
            ui.label(RichText::new(err).color(theme::FLAG_RED));
        }
        ui.add_space(14.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            if ui.button("  Cancel  ").clicked() {
                if busy {
                    // Stop waiting for the browser; the pending result is dropped.
                    dlg.cancel.store(true, Ordering::Relaxed);
                    dlg.testing = None;
                } else {
                    close = true;
                }
            }
            let oauth_kind = match dlg.kind {
                Kind::Microsoft365 => Some(AuthMethod::Microsoft),
                Kind::Imap if dlg.auth != AuthMethod::Password => Some(dlg.auth),
                _ => None,
            };
            let label = match (oauth_kind, dlg.editing.is_some()) {
                (Some(AuthMethod::Microsoft), _) => "  Sign in with Microsoft  ",
                (Some(_), _) => "  Sign in with Google  ",
                (None, true) => "  Save  ",
                (None, false) => "  Connect  ",
            };
            if ui.add_enabled(!busy, egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(BLUE)).clicked() {
                dlg.error = None;
                if let Err(e) = dlg.start(ui.ctx()) {
                    dlg.error = Some(e);
                }
            }
            if busy {
                ui.spinner();
                ui.label(if oauth_kind.is_some() { "Finish signing in in your browser..." } else { "Connecting..." });
            }
        });
    });
    if modal.should_close() && dlg.testing.is_none() {
        close = true;
    }
    if let Some((config, secret)) = save {
        app.account_dialog = None;
        app.save_account(config, Some(secret));
    } else if close {
        app.account_dialog = None;
    }
}

/// File tab: Outlook's full-window backstage with account management.
pub fn backstage(app: &mut App, ui: &mut Ui) {
    Panel::left("backstage_nav")
        .exact_size(160.0)
        .resizable(false)
        .frame(Frame::NONE.fill(BLUE).inner_margin(Margin::symmetric(0, 12)))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let back = ui.add(egui::Label::new(theme::icon(icon::BACK, 22.0).color(Color32::WHITE)).sense(egui::Sense::click()));
            if back.clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                app.backstage = false;
            }
            ui.add_space(16.0);
            Frame::NONE.fill(theme::BLUE_DARK).inner_margin(Margin::symmetric(20, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Info").size(15.0).color(Color32::WHITE));
            });
            let exit = Frame::NONE
                .inner_margin(Margin::symmetric(20, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("Exit").size(15.0).color(Color32::WHITE));
                })
                .response
                .interact(egui::Sense::click());
            if exit.clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });

    CentralPanel::default().frame(Frame::NONE.fill(theme::WHITE).inner_margin(Margin::symmetric(36, 24))).show(ui, |ui| {
        ui.label(RichText::new("Account Information").size(28.0).color(theme::TEXT));
        ui.add_space(18.0);
        let accounts: Vec<(String, String)> = app.accounts.iter().map(|a| (a.config.id.clone(), a.config.email.clone())).collect();
        if accounts.is_empty() {
            ui.label(RichText::new("No accounts are set up.").color(SUBTLE));
        }
        for (id, email) in &accounts {
            Frame::NONE.stroke(egui::Stroke::new(1.0, theme::BORDER)).inner_margin(Margin::same(12)).show(ui, |ui| {
                ui.set_width(520.0);
                ui.horizontal(|ui| {
                    super::reading::avatar(ui, email, 36.0);
                    ui.vertical(|ui| {
                        ui.label(theme::bold(email.as_str(), 14.0));
                        ui.label(RichText::new("IMAP/SMTP").small().color(SUBTLE));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if super::flat_button(ui, icon::DELETE, "Remove").clicked() {
                            app.confirm_remove = Some(id.clone());
                        }
                        if super::flat_button(ui, icon::SETTINGS, "Account Settings").clicked() {
                            app.actions.push(Action::EditAccount(id.clone()));
                        }
                    });
                });
            });
            ui.add_space(8.0);
        }
        ui.add_space(8.0);
        if super::flat_button(ui, icon::ADD, "Add Account").clicked() {
            app.actions.push(Action::AddAccount);
        }
    });

    if let Some(id) = app.confirm_remove.clone() {
        let modal = egui::Modal::new(egui::Id::new("confirm_remove")).show(ui.ctx(), |ui| {
            ui.set_max_width(400.0);
            ui.label(theme::bold("Remove Account", 15.0));
            ui.add_space(6.0);
            ui.label("The offline cache for this account will be deleted from this computer. Messages on the server are not affected.");
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("  Cancel  ").clicked() {
                    return Some(false);
                }
                if ui.button("  Remove  ").clicked() {
                    return Some(true);
                }
                None
            })
            .inner
        });
        match modal.inner {
            Some(true) => {
                app.actions.push(Action::RemoveAccount(id));
                app.confirm_remove = None;
            }
            Some(false) => app.confirm_remove = None,
            None if modal.should_close() => app.confirm_remove = None,
            None => {}
        }
    }
}
