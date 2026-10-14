//! Core data types shared by the UI and the background mail worker.

use serde::{Deserialize, Serialize};

/// Connection settings for one mail account. The password (or OAuth refresh
/// token) lives in the Windows Credential Manager, never in this struct.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AccountConfig {
    pub id: String,
    pub display_name: String,
    pub email: String,
    pub username: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    #[serde(default)]
    pub auth: AuthMethod,
    /// How mail and calendar are reached.
    #[serde(default)]
    pub protocol: Protocol,
    /// Exchange Server: the EWS endpoint (found by Autodiscover when empty).
    #[serde(default)]
    pub ews_url: String,
}

impl AccountConfig {
    /// Whether the account has a calendar on the server (Microsoft Graph or EWS).
    pub fn has_server_calendar(&self) -> bool {
        match self.protocol {
            Protocol::Graph | Protocol::Ews => true,
            Protocol::Imap => self.auth == AuthMethod::Microsoft,
        }
    }
}

/// The protocol family an account uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    /// IMAP + SMTP (any provider; Microsoft accounts get their calendar via Graph).
    #[default]
    Imap,
    /// Microsoft Graph: Microsoft 365 / Exchange Online and Outlook.com.
    Graph,
    /// Exchange Web Services: Exchange Server (on-premises).
    Ews,
}

/// How the account signs in to IMAP/SMTP.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMethod {
    #[default]
    Password,
    /// OAuth 2.0: Outlook.com, Hotmail, Microsoft 365 / Exchange Online.
    Microsoft,
    /// OAuth 2.0: Gmail and Google Workspace.
    Google,
}

/// Outlook's well-known folders, detected from IMAP SPECIAL-USE attributes or names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum FolderKind {
    Inbox,
    Drafts,
    Sent,
    Deleted,
    Archive,
    Junk,
    #[default]
    Other,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Folder {
    /// Full IMAP mailbox name (e.g. `INBOX`, `[Gmail]/Sent Mail`); for Graph and
    /// EWS the display path (`Projects/Northwind`) with `/` as the delimiter.
    pub path: String,
    pub delimiter: Option<String>,
    pub kind: FolderKind,
    pub selectable: bool,
    pub unread: u32,
    pub total: u32,
    /// Graph / EWS folder id (empty for IMAP).
    #[serde(default)]
    pub remote_id: String,
}

impl Folder {
    /// The label Outlook shows in the folder pane.
    pub fn display_name(&self) -> String {
        match self.kind {
            FolderKind::Inbox => "Inbox".into(),
            FolderKind::Drafts => "Drafts".into(),
            FolderKind::Sent => "Sent Items".into(),
            FolderKind::Deleted => "Deleted Items".into(),
            FolderKind::Archive => "Archive".into(),
            FolderKind::Junk => "Junk Email".into(),
            FolderKind::Other => self.leaf_name().to_string(),
        }
    }

    pub fn leaf_name(&self) -> &str {
        match &self.delimiter {
            Some(d) if !d.is_empty() => self.path.rsplit(d.as_str()).next().unwrap_or(&self.path),
            _ => &self.path,
        }
    }

    /// Nesting depth in the folder tree (0 = top level).
    pub fn depth(&self) -> usize {
        match &self.delimiter {
            Some(d) if !d.is_empty() => self.path.matches(d.as_str()).count(),
            _ => 0,
        }
    }
}

/// Classify a mailbox from its LIST attributes and name.
pub fn classify_folder(path: &str, attributes: &[String]) -> FolderKind {
    let has = |a: &str| attributes.iter().any(|x| x.eq_ignore_ascii_case(a));
    if path.eq_ignore_ascii_case("INBOX") {
        return FolderKind::Inbox;
    }
    if has("\\Sent") {
        return FolderKind::Sent;
    }
    if has("\\Trash") {
        return FolderKind::Deleted;
    }
    if has("\\Drafts") {
        return FolderKind::Drafts;
    }
    if has("\\Junk") {
        return FolderKind::Junk;
    }
    if has("\\Archive") {
        return FolderKind::Archive;
    }
    let leaf = path.rsplit(['/', '.']).next().unwrap_or(path).to_ascii_lowercase();
    match leaf.as_str() {
        "sent" | "sent items" | "sent mail" | "sent messages" => FolderKind::Sent,
        "trash" | "deleted items" | "deleted messages" | "bin" => FolderKind::Deleted,
        "drafts" | "draft" => FolderKind::Drafts,
        "junk" | "junk email" | "junk e-mail" | "spam" | "bulk mail" => FolderKind::Junk,
        "archive" | "archives" => FolderKind::Archive,
        _ => FolderKind::Other,
    }
}

/// Header-level information about a message, enough to draw the message list.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MessageMeta {
    /// IMAP UID, or a local number assigned to a Graph / EWS message.
    pub uid: u32,
    /// Graph / EWS item id (empty for IMAP).
    #[serde(default)]
    pub remote_id: String,
    pub from_name: String,
    pub from_addr: String,
    pub to: String,
    pub subject: String,
    /// Unix timestamp (seconds).
    pub date: i64,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub has_attachments: bool,
    pub size: u32,
    pub preview: String,
}

impl MessageMeta {
    pub fn sender_label(&self) -> &str {
        if self.from_name.is_empty() { &self.from_addr } else { &self.from_name }
    }
}

/// A fully parsed message ready for the reading pane.
#[derive(Clone, Debug, Default)]
pub struct MessageBody {
    pub subject: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub date: i64,
    pub text: String,
    /// Original HTML body (inline images embedded as data: URIs), if the message has one.
    pub html: Option<String>,
    pub message_id: Option<String>,
    pub references: Vec<String>,
    pub reply_to: Option<String>,
    pub attachments: Vec<Attachment>,
    /// Meeting request or cancellation carried in a text/calendar part.
    pub invite: Option<crate::ics::Invite>,
}

#[derive(Clone, Debug)]
pub struct Attachment {
    pub name: String,
    pub data: Vec<u8>,
}

/// A message being composed in a compose window.
#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub account_id: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    /// What the user typed; `body_styles` holds one style per character.
    pub body: String,
    pub body_styles: Vec<crate::rich::Style>,
    /// Send as plain text only (Outlook's Format Text > Plain Text).
    pub plain_text: bool,
    /// Quoted original for replies/forwards, kept apart from the editable body.
    pub quote_text: String,
    pub quote_html: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub attachments: Vec<std::path::PathBuf>,
    /// iCalendar part for meeting requests/replies: (METHOD, iCalendar text).
    pub calendar: Option<(String, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_attribute_then_name() {
        assert_eq!(classify_folder("INBOX", &[]), FolderKind::Inbox);
        assert_eq!(classify_folder("[Gmail]/Sent Mail", &["\\Sent".into()]), FolderKind::Sent);
        assert_eq!(classify_folder("INBOX.Trash", &[]), FolderKind::Deleted);
        assert_eq!(classify_folder("Spam", &[]), FolderKind::Junk);
        assert_eq!(classify_folder("Projects", &[]), FolderKind::Other);
    }

    #[test]
    fn folder_tree_helpers() {
        let f = Folder {
            path: "Work/Clients/Acme".into(),
            delimiter: Some("/".into()),
            kind: FolderKind::Other,
            selectable: true,
            unread: 0,
            total: 0,
            ..Default::default()
        };
        assert_eq!(f.leaf_name(), "Acme");
        assert_eq!(f.depth(), 2);
        assert_eq!(f.display_name(), "Acme");
    }
}
