//! Exchange Web Services backend for Exchange Server (on-premises): mail and
//! calendar over SOAP, with Autodiscover to find the EWS endpoint. HTTP goes
//! through WinHTTP so Windows handles Kerberos/NTLM and certificates.
//!
//! (EWS is being retired in Exchange Online; Microsoft 365 accounts use Graph.)

use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chrono::{DateTime, Local, TimeZone, Utc};
use roxmltree::{Document, Node};

use super::{Auth, Backend, Res, SyncOutcome, merge_remote};
use crate::calendar::{self, Event as CalEvent, Response};
use crate::model::{Draft, Folder, FolderKind, MessageMeta, classify_folder};
use crate::store::FolderCache;
use crate::winhttp;

const SOAP: &str = "http://schemas.xmlsoap.org/soap/envelope/";
const TYPES: &str = "http://schemas.microsoft.com/exchange/services/2006/types";
const MESSAGES: &str = "http://schemas.microsoft.com/exchange/services/2006/messages";

/// XML-escape text and attribute values.
pub fn x(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// A SOAP envelope for EWS (Exchange 2013 SP1 schema; works on 2013, 2016, 2019, SE).
pub fn envelope(body: &str, time_zone: Option<&str>) -> String {
    let tz = time_zone
        .map(|z| format!("<t:TimeZoneContext><t:TimeZoneDefinition Id=\"{}\"/></t:TimeZoneContext>", x(z)))
        .unwrap_or_default();
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <soap:Envelope xmlns:soap=\"{SOAP}\" xmlns:t=\"{TYPES}\" xmlns:m=\"{MESSAGES}\">\
         <soap:Header><t:RequestServerVersion Version=\"Exchange2013_SP1\"/>{tz}</soap:Header>\
         <soap:Body>{body}</soap:Body></soap:Envelope>"
    )
}

// ------------------------------------------------------------- XML helpers

fn el<'a>(n: Node<'a, 'a>, name: &str) -> Option<Node<'a, 'a>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

fn path<'a>(n: Node<'a, 'a>, names: &[&str]) -> Option<Node<'a, 'a>> {
    names.iter().try_fold(n, |cur, name| el(cur, name))
}

fn text(n: Node, name: &str) -> String {
    el(n, name).and_then(|c| c.text()).unwrap_or_default().to_string()
}

fn descendants<'a>(n: Node<'a, 'a>, name: &'a str) -> impl Iterator<Item = Node<'a, 'a>> {
    n.descendants().filter(move |c| c.is_element() && c.tag_name().name() == name)
}

fn id_of(n: Node, name: &str) -> String {
    el(n, name).and_then(|c| c.attribute("Id")).unwrap_or_default().to_string()
}

fn parse_doc(text: &str) -> Res<Document<'_>> {
    let doc = Document::parse(text).map_err(|e| format!("Exchange sent an unreadable response: {e}"))?;
    if let Some(fault) = descendants(doc.root(), "Fault").next() {
        let msg = text_deep(fault, "faultstring");
        return Err(format!("Exchange reported an error: {msg}"));
    }
    Ok(doc)
}

fn text_deep(n: Node, name: &str) -> String {
    descendants(n, name).next().and_then(|c| c.text()).unwrap_or_default().to_string()
}

/// Every `*ResponseMessage` element, in request order.
fn response_messages<'a>(doc: &'a Document<'a>) -> Vec<Node<'a, 'a>> {
    doc.descendants().filter(|n| n.is_element() && n.attribute("ResponseClass").is_some()).collect()
}

fn message_error(m: Node) -> Option<String> {
    (m.attribute("ResponseClass") == Some("Error")).then(|| {
        let code = text(m, "ResponseCode");
        let msg = text(m, "MessageText");
        if msg.is_empty() { code } else { format!("{msg} ({code})") }
    })
}

/// Fail if any response message is an error.
fn check_all(doc: &Document) -> Res<()> {
    match response_messages(doc).into_iter().find_map(message_error) {
        Some(e) => Err(format!("Exchange reported an error: {e}")),
        None => Ok(()),
    }
}

fn parse_utc(s: &str) -> i64 {
    DateTime::parse_from_rfc3339(s.trim()).map(|d| d.timestamp()).unwrap_or(0)
}

fn utc_string(ts: i64) -> String {
    Utc.timestamp_opt(ts, 0).single().map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string()).unwrap_or_default()
}

/// Local date-time without offset; read in the request's TimeZoneContext.
fn local_string(ts: i64) -> String {
    Local.timestamp_opt(ts, 0).single().map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string()).unwrap_or_default()
}

fn item_ids(msgs: &[MessageMeta]) -> String {
    msgs.iter().map(|m| format!("<t:ItemId Id=\"{}\"/>", x(&m.remote_id))).collect()
}

// --------------------------------------------------------------- parsing

/// FindFolder (Deep) results → (id, parent id, name, total, unread) for mail folders.
pub fn parse_folders(doc: &Document) -> Vec<(String, String, String, u32, u32)> {
    descendants(doc.root(), "Folder")
        .filter(|f| {
            let class = text(*f, "FolderClass");
            class.is_empty() || class == "IPF.Note" || class.starts_with("IPF.Note.")
        })
        .map(|f| {
            (
                id_of(f, "FolderId"),
                id_of(f, "ParentFolderId"),
                text(f, "DisplayName"),
                text(f, "TotalCount").parse().unwrap_or(0),
                text(f, "UnreadCount").parse().unwrap_or(0),
            )
        })
        .collect()
}

/// FindItem results → list metadata (newest first, as requested).
pub fn parse_items(doc: &Document) -> Vec<MessageMeta> {
    let Some(items) = descendants(doc.root(), "Items").next() else { return Vec::new() };
    items
        .children()
        .filter(|n| n.is_element())
        .filter_map(|item| {
            let id = id_of(item, "ItemId");
            if id.is_empty() {
                return None;
            }
            let from = path(item, &["From", "Mailbox"]);
            Some(MessageMeta {
                remote_id: id,
                subject: text(item, "Subject"),
                from_name: from.map(|m| text(m, "Name")).unwrap_or_default(),
                from_addr: from.map(|m| text(m, "EmailAddress")).unwrap_or_default(),
                to: text(item, "DisplayTo"),
                date: parse_utc(&text(item, "DateTimeReceived")),
                seen: text(item, "IsRead") != "false",
                flagged: path(item, &["Flag", "FlagStatus"]).and_then(|n| n.text()) == Some("Flagged"),
                has_attachments: text(item, "HasAttachments") == "true",
                size: text(item, "Size").parse().unwrap_or(0),
                preview: text(item, "Preview").split_whitespace().collect::<Vec<_>>().join(" "),
                ..Default::default()
            })
        })
        .collect()
}

/// GetItem results for calendar items → events.
pub fn parse_calendar(doc: &Document, account: &str) -> Vec<CalEvent> {
    descendants(doc.root(), "CalendarItem")
        .filter_map(|c| {
            let id = id_of(c, "ItemId");
            if id.is_empty() || text(c, "IsCancelled") == "true" {
                return None;
            }
            let all_day = text(c, "IsAllDayEvent") == "true";
            let (mut start, mut end) = (parse_utc(&text(c, "Start")), parse_utc(&text(c, "End")));
            if all_day {
                // All-day items are pinned to dates; land them on local midnights.
                let first = calendar::local_date(start);
                let last = calendar::local_date(end - 1).max(first);
                start = calendar::day_bounds(first).0;
                end = calendar::day_bounds(last).1;
            }
            let attendees: Vec<String> = ["RequiredAttendees", "OptionalAttendees"]
                .iter()
                .filter_map(|k| el(c, k))
                .flat_map(|list| descendants(list, "EmailAddress").filter_map(|e| e.text().map(str::to_string)).collect::<Vec<_>>())
                .collect();
            let is_meeting = text(c, "IsMeeting") == "true" || !attendees.is_empty();
            let response = if !is_meeting {
                Response::None
            } else {
                match text(c, "MyResponseType").as_str() {
                    "Organizer" => Response::Organizer,
                    "Accept" => Response::Accepted,
                    "Tentative" => Response::Tentative,
                    "Decline" => Response::Declined,
                    "NoResponseReceived" => Response::NotResponded,
                    _ => Response::None,
                }
            };
            let organizer = path(c, &["Organizer", "Mailbox"]);
            Some(CalEvent {
                id,
                calendar: account.to_string(),
                uid: text(c, "UID"),
                subject: text(c, "Subject"),
                location: text(c, "Location"),
                body: text(c, "Body"),
                start,
                end: end.max(start),
                all_day,
                organizer_name: organizer.map(|m| text(m, "Name")).unwrap_or_default(),
                organizer: organizer.map(|m| text(m, "EmailAddress")).unwrap_or_default(),
                attendees,
                response,
                sequence: 0,
                recurring: !matches!(text(c, "CalendarItemType").as_str(), "" | "Single"),
            })
        })
        .collect()
}

// ----------------------------------------------------------- Autodiscover

/// POX Autodiscover request body.
fn autodiscover_request(email: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <Autodiscover xmlns=\"http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006\">\
         <Request><EMailAddress>{}</EMailAddress>\
         <AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema>\
         </Request></Autodiscover>",
        x(email)
    )
}

pub enum Discovered {
    Ews(String),
    RedirectAddr(String),
    RedirectUrl(String),
}

/// Pick the EWS URL (external preferred) or a redirect from an Autodiscover response.
pub fn parse_autodiscover(xml: &str) -> Option<Discovered> {
    let doc = Document::parse(xml).ok()?;
    let account = descendants(doc.root(), "Account").next()?;
    match text_deep(account, "Action").as_str() {
        "redirectAddr" => return Some(Discovered::RedirectAddr(text_deep(account, "RedirectAddr"))),
        "redirectUrl" => return Some(Discovered::RedirectUrl(text_deep(account, "RedirectUrl"))),
        _ => {}
    }
    let protocols: Vec<Node> = descendants(account, "Protocol").collect();
    ["EXPR", "EXCH"].iter().find_map(|kind| {
        protocols
            .iter()
            .find(|p| text(**p, "Type") == *kind && !text(**p, "EwsUrl").is_empty())
            .map(|p| Discovered::Ews(text(*p, "EwsUrl")))
    })
}

fn autodiscover_urls(domain: &str) -> Vec<String> {
    vec![
        format!("https://{domain}/autodiscover/autodiscover.xml"),
        format!("https://autodiscover.{domain}/autodiscover/autodiscover.xml"),
    ]
}

/// Autodiscover endpoints to try for an address: the two HTTPS URLs, then
/// wherever `http://autodiscover.<domain>` redirects (the HTTP-redirect method).
fn candidates(client: &winhttp::Client, email: &str) -> Res<std::collections::VecDeque<String>> {
    let domain = email.rsplit_once('@').map(|(_, d)| d.to_lowercase()).ok_or("Enter a valid email address.")?;
    let mut urls: std::collections::VecDeque<String> = autodiscover_urls(&domain).into();
    if let Ok(r) = client.request("GET", &format!("http://autodiscover.{domain}/autodiscover/autodiscover.xml"), None, b"", false) {
        if let Some(loc) = r.location.filter(|l| l.starts_with("https://") && (300..400).contains(&r.status)) {
            urls.push_back(loc);
        }
    }
    Ok(urls)
}

/// Find the EWS endpoint for a mailbox with POX Autodiscover, following up to a
/// few address or URL redirects.
pub fn autodiscover(email: &str, user: &str, password: &str) -> Res<String> {
    let client = winhttp::Client::new(Some((user, password)), 20)?;
    let mut email = email.to_string();
    let mut queue = candidates(&client, &email)?;
    let mut last_err = String::from("No Autodiscover service answered.");
    let mut tries = 0;
    while let Some(url) = queue.pop_front() {
        tries += 1;
        if tries > 8 {
            break;
        }
        match client.request("POST", &url, Some("text/xml; charset=utf-8"), autodiscover_request(&email).as_bytes(), true) {
            Ok(r) if r.status == 200 => match parse_autodiscover(&String::from_utf8_lossy(&r.body)) {
                Some(Discovered::Ews(ews)) => return Ok(ews),
                Some(Discovered::RedirectAddr(addr)) if addr.contains('@') => {
                    email = addr;
                    queue = candidates(&client, &email)?;
                }
                Some(Discovered::RedirectUrl(u)) if u.starts_with("https://") => queue.push_front(u),
                _ => last_err = format!("{url} didn't list an Exchange Web Services address."),
            },
            Ok(r) if r.status == 401 => last_err = "Exchange rejected the user name or password.".into(),
            Ok(r) => last_err = format!("{url} answered HTTP {}.", r.status),
            Err(e) => last_err = e,
        }
    }
    Err(format!("Couldn't find your Exchange server automatically. {last_err} Enter the EWS URL under Server settings."))
}

/// Without credentials: does the domain answer Autodiscover with an auth challenge,
/// as Exchange does? Used to suggest the account type.
pub fn looks_like_exchange(domain: &str) -> bool {
    let Ok(client) = winhttp::Client::new(None, 6) else { return false };
    autodiscover_urls(domain).iter().any(|u| {
        client
            .request("POST", u, Some("text/xml; charset=utf-8"), autodiscover_request(&format!("probe@{domain}")).as_bytes(), true)
            .is_ok_and(|r| r.status == 401)
    })
}

// ---------------------------------------------------------------- backend

pub struct EwsBackend {
    auth: Auth,
    client: Option<winhttp::Client>,
    url: String,
    time_zone: Option<String>,
    /// Older servers reject some properties; drop them after the first refusal.
    rich_find: bool,
}

impl EwsBackend {
    pub fn new(auth: Auth) -> Self {
        let url = auth.config.ews_url.clone();
        Self { auth, client: None, url, time_zone: winhttp::windows_time_zone(), rich_find: true }
    }

    /// For tests: talk to `url` with an already-built client.
    #[cfg(test)]
    fn with_client(auth: Auth, client: winhttp::Client, url: &str) -> Self {
        Self { client: Some(client), url: url.to_string(), ..Self::new(auth) }
    }

    fn user(&self) -> String {
        let c = &self.auth.config;
        if c.username.is_empty() { c.email.clone() } else { c.username.clone() }
    }

    /// POST a SOAP body and return the response text.
    fn call(&mut self, body: &str, with_tz: bool) -> Res<String> {
        if self.client.is_none() || self.url.is_empty() {
            let password = self.auth.password()?;
            if self.url.is_empty() {
                self.auth.emitter.status("Finding your Exchange server...");
                self.url = autodiscover(&self.auth.config.email, &self.user(), &password)?;
            }
            self.auth.emitter.status("Connecting to Exchange...");
            self.client = Some(winhttp::Client::new(Some((&self.user(), &password)), 60)?);
        }
        let tz = if with_tz { self.time_zone.clone() } else { None };
        let xml = envelope(body, tz.as_deref());
        let r = self.client.as_ref().unwrap().request("POST", &self.url, Some("text/xml; charset=utf-8"), xml.as_bytes(), true)?;
        let text = String::from_utf8_lossy(&r.body).into_owned();
        match r.status {
            200 | 500 => Ok(text), // EWS reports SOAP faults with 500.
            401 => Err("Exchange rejected the user name or password. Open File > Account Settings to update them.".into()),
            s => Err(format!("Exchange answered HTTP {s}.")),
        }
    }

    fn get_folders(&mut self, ids: &[&str]) -> Res<Vec<Option<String>>> {
        let ids_xml: String = ids.iter().map(|id| format!("<t:DistinguishedFolderId Id=\"{id}\"/>")).collect();
        let body = format!("<m:GetFolder><m:FolderShape><t:BaseShape>IdOnly</t:BaseShape></m:FolderShape><m:FolderIds>{ids_xml}</m:FolderIds></m:GetFolder>");
        let text = self.call(&body, false)?;
        let doc = parse_doc(&text)?;
        Ok(response_messages(&doc)
            .into_iter()
            .map(|m| descendants(m, "FolderId").next().and_then(|f| f.attribute("Id")).map(str::to_string))
            .collect())
    }

    fn find_items(&mut self, folder_id: &str, window: usize) -> Res<String> {
        let extra = if self.rich_find { "<t:FieldURI FieldURI=\"item:Flag\"/><t:FieldURI FieldURI=\"item:Preview\"/>" } else { "" };
        let body = format!(
            "<m:FindItem Traversal=\"Shallow\"><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties>\
             <t:FieldURI FieldURI=\"item:Subject\"/><t:FieldURI FieldURI=\"message:From\"/><t:FieldURI FieldURI=\"item:DisplayTo\"/>\
             <t:FieldURI FieldURI=\"item:DateTimeReceived\"/><t:FieldURI FieldURI=\"message:IsRead\"/>\
             <t:FieldURI FieldURI=\"item:HasAttachments\"/><t:FieldURI FieldURI=\"item:Size\"/>{extra}\
             </t:AdditionalProperties></m:ItemShape>\
             <m:IndexedPageItemView MaxEntriesReturned=\"{window}\" Offset=\"0\" BasePoint=\"Beginning\"/>\
             <m:SortOrder><t:FieldOrder Order=\"Descending\"><t:FieldURI FieldURI=\"item:DateTimeReceived\"/></t:FieldOrder></m:SortOrder>\
             <m:ParentFolderIds><t:FolderId Id=\"{}\"/></m:ParentFolderIds></m:FindItem>",
            x(folder_id)
        );
        let text = self.call(&body, false)?;
        let doc = parse_doc(&text)?;
        match check_all(&doc) {
            Err(e) if self.rich_find && (e.contains("InvalidPropertyRequest") || e.contains("SchemaValidation")) => {
                self.rich_find = false;
                self.find_items(folder_id, window)
            }
            Err(e) => Err(e),
            Ok(()) => Ok(text),
        }
    }

    fn update_items(&mut self, msgs: &[MessageMeta], field: &str, value_xml: &str) -> Res<()> {
        let changes: String = msgs
            .iter()
            .map(|m| {
                format!(
                    "<t:ItemChange><t:ItemId Id=\"{}\"/><t:Updates><t:SetItemField><t:FieldURI FieldURI=\"{field}\"/>\
                     <t:Message>{value_xml}</t:Message></t:SetItemField></t:Updates></t:ItemChange>",
                    x(&m.remote_id)
                )
            })
            .collect();
        let body = format!(
            "<m:UpdateItem MessageDisposition=\"SaveOnly\" ConflictResolution=\"AlwaysOverwrite\" SuppressReadReceipts=\"true\">\
             <m:ItemChanges>{changes}</m:ItemChanges></m:UpdateItem>"
        );
        let text = self.call(&body, false)?;
        check_all(&parse_doc(&text)?)
    }

    fn calendar_item_xml(e: &CalEvent) -> String {
        let (start, end) = if e.all_day { (local_string(e.start), local_string(e.end)) } else { (utc_string(e.start), utc_string(e.end)) };
        let attendees: String = e.attendees.iter().map(|a| format!("<t:Attendee><t:Mailbox><t:EmailAddress>{}</t:EmailAddress></t:Mailbox></t:Attendee>", x(a))).collect();
        let attendees = if attendees.is_empty() { String::new() } else { format!("<t:RequiredAttendees>{attendees}</t:RequiredAttendees>") };
        format!(
            "<t:Subject>{}</t:Subject><t:Body BodyType=\"Text\">{}</t:Body><t:Start>{start}</t:Start><t:End>{end}</t:End>\
             <t:IsAllDayEvent>{}</t:IsAllDayEvent><t:Location>{}</t:Location>{attendees}",
            x(&e.subject),
            x(&e.body),
            e.all_day,
            x(&e.location)
        )
    }
}

impl Backend for EwsBackend {
    fn folders(&mut self) -> Res<Vec<Folder>> {
        let known = self.get_folders(&["msgfolderroot", "inbox", "drafts", "sentitems", "deleteditems", "junkemail"])?;
        let root = known.first().cloned().flatten().ok_or("Exchange didn't return the mailbox's folder root.")?;
        let kinds: HashMap<String, FolderKind> = known
            .iter()
            .skip(1)
            .zip([FolderKind::Inbox, FolderKind::Drafts, FolderKind::Sent, FolderKind::Deleted, FolderKind::Junk])
            .filter_map(|(id, kind)| id.clone().map(|id| (id, kind)))
            .collect();
        let body = "<m:FindFolder Traversal=\"Deep\"><m:FolderShape><t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties>\
             <t:FieldURI FieldURI=\"folder:DisplayName\"/><t:FieldURI FieldURI=\"folder:ParentFolderId\"/>\
             <t:FieldURI FieldURI=\"folder:TotalCount\"/><t:FieldURI FieldURI=\"folder:UnreadCount\"/>\
             <t:FieldURI FieldURI=\"folder:FolderClass\"/></t:AdditionalProperties></m:FolderShape>\
             <m:ParentFolderIds><t:DistinguishedFolderId Id=\"msgfolderroot\"/></m:ParentFolderIds></m:FindFolder>";
        let text = self.call(body, false)?;
        let doc = parse_doc(&text)?;
        check_all(&doc)?;
        Ok(build_tree(&root, &kinds, parse_folders(&doc)))
    }

    fn sync(&mut self, folder: &Folder, cache: &mut FolderCache, window: usize) -> Res<SyncOutcome> {
        let text = self.find_items(&folder.remote_id, window)?;
        let doc = parse_doc(&text)?;
        let total = descendants(doc.root(), "RootFolder").next().and_then(|r| r.attribute("TotalItemsInView")).and_then(|t| t.parse().ok());
        let listing = parse_items(&doc);
        let total = total.unwrap_or(listing.len() as u32);
        let removed = merge_remote(cache, listing);
        Ok(SyncOutcome { total, removed })
    }

    fn fetch(&mut self, _folder: &Folder, msgs: &[MessageMeta]) -> Res<Vec<(u32, Vec<u8>)>> {
        let mut out = Vec::new();
        for chunk in msgs.chunks(10) {
            let body = format!(
                "<m:GetItem><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape><t:IncludeMimeContent>true</t:IncludeMimeContent>\
                 </m:ItemShape><m:ItemIds>{}</m:ItemIds></m:GetItem>",
                item_ids(chunk)
            );
            let text = self.call(&body, false)?;
            let doc = parse_doc(&text)?;
            // One response message per requested id, in order; skip items that vanished.
            for (m, resp) in chunk.iter().zip(response_messages(&doc)) {
                if message_error(resp).is_some() {
                    continue;
                }
                if let Some(mime) = descendants(resp, "MimeContent").next().and_then(|n| n.text()) {
                    let cleaned: String = mime.chars().filter(|c| !c.is_whitespace()).collect();
                    if let Ok(raw) = B64.decode(cleaned) {
                        out.push((m.uid, raw));
                    }
                }
            }
        }
        Ok(out)
    }

    fn set_seen(&mut self, _folder: &Folder, msgs: &[MessageMeta], seen: bool) -> Res<()> {
        self.update_items(msgs, "message:IsRead", &format!("<t:IsRead>{seen}</t:IsRead>"))
    }

    fn set_flagged(&mut self, _folder: &Folder, msg: &MessageMeta, flagged: bool) -> Res<()> {
        let status = if flagged { "Flagged" } else { "NotFlagged" };
        self.update_items(std::slice::from_ref(msg), "item:Flag", &format!("<t:Flag><t:FlagStatus>{status}</t:FlagStatus></t:Flag>"))
    }

    fn move_to(&mut self, _folder: &Folder, msgs: &[MessageMeta], dest: &Folder) -> Res<()> {
        let body = format!(
            "<m:MoveItem><m:ToFolderId><t:FolderId Id=\"{}\"/></m:ToFolderId><m:ItemIds>{}</m:ItemIds></m:MoveItem>",
            x(&dest.remote_id),
            item_ids(msgs)
        );
        let text = self.call(&body, false)?;
        check_all(&parse_doc(&text)?)
    }

    fn delete_permanently(&mut self, _folder: &Folder, msgs: &[MessageMeta]) -> Res<()> {
        let body = format!("<m:DeleteItem DeleteType=\"SoftDelete\"><m:ItemIds>{}</m:ItemIds></m:DeleteItem>", item_ids(msgs));
        let text = self.call(&body, false)?;
        check_all(&parse_doc(&text)?)
    }

    /// Exchange sends the MIME message and saves it to Sent Items.
    fn send(&mut self, draft: &Draft, _folders: &[Folder]) -> Res<()> {
        // Keep Bcc in the MIME: Exchange takes recipients from the headers.
        let msg = crate::mime::build_message(&self.auth.config, draft, true)?;
        let body = format!(
            "<m:CreateItem MessageDisposition=\"SendAndSaveCopy\"><m:SavedItemFolderId><t:DistinguishedFolderId Id=\"sentitems\"/>\
             </m:SavedItemFolderId><m:Items><t:Message><t:MimeContent CharacterSet=\"UTF-8\">{}</t:MimeContent></t:Message>\
             </m:Items></m:CreateItem>",
            B64.encode(msg.formatted())
        );
        let text = self.call(&body, false)?;
        check_all(&parse_doc(&text)?).map_err(|e| format!("Sending reported error: {e}"))
    }

    fn reset(&mut self) {
        self.client = None;
    }

    fn calendar_view(&mut self, from: i64, to: i64) -> Res<Vec<CalEvent>> {
        let body = format!(
            "<m:FindItem Traversal=\"Shallow\"><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape></m:ItemShape>\
             <m:CalendarView MaxEntriesReturned=\"1000\" StartDate=\"{}\" EndDate=\"{}\"/>\
             <m:ParentFolderIds><t:DistinguishedFolderId Id=\"calendar\"/></m:ParentFolderIds></m:FindItem>",
            utc_string(from),
            utc_string(to)
        );
        let text = self.call(&body, false)?;
        let doc = parse_doc(&text)?;
        check_all(&doc)?;
        let ids: Vec<String> = descendants(doc.root(), "ItemId").filter_map(|n| n.attribute("Id").map(str::to_string)).collect();
        let mut events = Vec::new();
        for chunk in ids.chunks(100) {
            let ids_xml: String = chunk.iter().map(|id| format!("<t:ItemId Id=\"{}\"/>", x(id))).collect();
            let body = format!(
                "<m:GetItem><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape><t:BodyType>Text</t:BodyType><t:AdditionalProperties>\
                 <t:FieldURI FieldURI=\"item:Subject\"/><t:FieldURI FieldURI=\"item:Body\"/>\
                 <t:FieldURI FieldURI=\"calendar:UID\"/><t:FieldURI FieldURI=\"calendar:Start\"/><t:FieldURI FieldURI=\"calendar:End\"/>\
                 <t:FieldURI FieldURI=\"calendar:IsAllDayEvent\"/><t:FieldURI FieldURI=\"calendar:Location\"/>\
                 <t:FieldURI FieldURI=\"calendar:IsMeeting\"/><t:FieldURI FieldURI=\"calendar:IsCancelled\"/>\
                 <t:FieldURI FieldURI=\"calendar:CalendarItemType\"/><t:FieldURI FieldURI=\"calendar:MyResponseType\"/>\
                 <t:FieldURI FieldURI=\"calendar:Organizer\"/><t:FieldURI FieldURI=\"calendar:RequiredAttendees\"/>\
                 <t:FieldURI FieldURI=\"calendar:OptionalAttendees\"/></t:AdditionalProperties></m:ItemShape>\
                 <m:ItemIds>{ids_xml}</m:ItemIds></m:GetItem>"
            );
            let text = self.call(&body, false)?;
            events.extend(parse_calendar(&parse_doc(&text)?, &self.auth.config.id));
        }
        Ok(events)
    }

    fn calendar_save(&mut self, e: &CalEvent) -> Res<()> {
        let invites = if e.attendees.is_empty() { "SendToNone" } else { "SendToAllAndSaveCopy" };
        let body = if e.id.is_empty() {
            format!(
                "<m:CreateItem SendMeetingInvitations=\"{invites}\"><m:SavedItemFolderId><t:DistinguishedFolderId Id=\"calendar\"/>\
                 </m:SavedItemFolderId><m:Items><t:CalendarItem>{}</t:CalendarItem></m:Items></m:CreateItem>",
                Self::calendar_item_xml(e)
            )
        } else {
            let (start, end) = if e.all_day { (local_string(e.start), local_string(e.end)) } else { (utc_string(e.start), utc_string(e.end)) };
            let set = |uri: &str, inner: String| {
                format!("<t:SetItemField><t:FieldURI FieldURI=\"{uri}\"/><t:CalendarItem>{inner}</t:CalendarItem></t:SetItemField>")
            };
            let mut updates = vec![
                set("item:Subject", format!("<t:Subject>{}</t:Subject>", x(&e.subject))),
                set("item:Body", format!("<t:Body BodyType=\"Text\">{}</t:Body>", x(&e.body))),
                set("calendar:Start", format!("<t:Start>{start}</t:Start>")),
                set("calendar:End", format!("<t:End>{end}</t:End>")),
                set("calendar:IsAllDayEvent", format!("<t:IsAllDayEvent>{}</t:IsAllDayEvent>", e.all_day)),
                set("calendar:Location", format!("<t:Location>{}</t:Location>", x(&e.location))),
            ];
            if !e.attendees.is_empty() {
                let list: String = e.attendees.iter().map(|a| format!("<t:Attendee><t:Mailbox><t:EmailAddress>{}</t:EmailAddress></t:Mailbox></t:Attendee>", x(a))).collect();
                updates.push(set("calendar:RequiredAttendees", format!("<t:RequiredAttendees>{list}</t:RequiredAttendees>")));
            }
            format!(
                "<m:UpdateItem ConflictResolution=\"AlwaysOverwrite\" SendMeetingInvitationsOrCancellations=\"{}\">\
                 <m:ItemChanges><t:ItemChange><t:ItemId Id=\"{}\"/><t:Updates>{}</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>",
                if e.attendees.is_empty() { "SendToNone" } else { "SendToAllAndSaveCopy" },
                x(&e.id),
                updates.concat()
            )
        };
        let text = self.call(&body, true)?;
        check_all(&parse_doc(&text)?)
    }

    fn calendar_delete(&mut self, id: &str) -> Res<()> {
        let body = format!(
            "<m:DeleteItem DeleteType=\"MoveToDeletedItems\" SendMeetingCancellations=\"SendToAllAndSaveCopy\">\
             <m:ItemIds><t:ItemId Id=\"{}\"/></m:ItemIds></m:DeleteItem>",
            x(id)
        );
        let text = self.call(&body, false)?;
        check_all(&parse_doc(&text)?)
    }

    fn calendar_respond(&mut self, id: &str, response: Response) -> Res<()> {
        let kind = match response {
            Response::Accepted => "AcceptItem",
            Response::Tentative => "TentativelyAcceptItem",
            Response::Declined => "DeclineItem",
            _ => return Ok(()),
        };
        let body = format!(
            "<m:CreateItem MessageDisposition=\"SendAndSaveCopy\"><m:Items><t:{kind}><t:ReferenceItemId Id=\"{}\"/></t:{kind}>\
             </m:Items></m:CreateItem>",
            x(id)
        );
        let text = self.call(&body, false)?;
        check_all(&parse_doc(&text)?)
    }
}

/// Folder paths from the parent chain (top-level folders sit directly under the root).
fn build_tree(root: &str, kinds: &HashMap<String, FolderKind>, raw: Vec<(String, String, String, u32, u32)>) -> Vec<Folder> {
    let by_id: HashMap<&str, (&str, &str)> = raw.iter().map(|(id, parent, name, _, _)| (id.as_str(), (parent.as_str(), name.as_str()))).collect();
    let full_path = |id: &str| -> Option<String> {
        let mut parts = Vec::new();
        let mut cur = id;
        for _ in 0..32 {
            let (parent, name) = by_id.get(cur)?;
            parts.push(name.replace('/', "\u{2215}"));
            if *parent == root {
                parts.reverse();
                return Some(parts.join("/"));
            }
            cur = parent;
        }
        None
    };
    raw.iter()
        .filter_map(|(id, _, name, total, unread)| {
            let path = full_path(id)?;
            let kind = kinds.get(id).copied().unwrap_or_else(|| match classify_folder(name, &[]) {
                FolderKind::Archive => FolderKind::Archive,
                _ => FolderKind::Other,
            });
            Some(Folder { path, delimiter: Some("/".into()), kind, selectable: true, unread: *unread, total: *total, remote_id: id.clone() })
        })
        .collect()
}

/// Sign-in check for a new Exchange account: find the server if needed, then read the Inbox.
/// Returns the EWS URL to save.
pub fn check(email: &str, user: &str, password: &str, ews_url: &str) -> Res<String> {
    let url = if ews_url.trim().is_empty() { autodiscover(email, user, password)? } else { ews_url.trim().to_string() };
    let client = winhttp::Client::new(Some((user, password)), 30)?;
    let body = envelope(
        "<m:GetFolder><m:FolderShape><t:BaseShape>IdOnly</t:BaseShape></m:FolderShape>\
         <m:FolderIds><t:DistinguishedFolderId Id=\"inbox\"/></m:FolderIds></m:GetFolder>",
        None,
    );
    let r = client.request("POST", &url, Some("text/xml; charset=utf-8"), body.as_bytes(), true)?;
    match r.status {
        200 | 500 => {
            let text = String::from_utf8_lossy(&r.body).into_owned();
            check_all(&parse_doc(&text)?)?;
            Ok(url)
        }
        401 => Err("Exchange rejected the user name or password.".into()),
        s => Err(format!("{url} answered HTTP {s}. Check the EWS URL.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(body: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"{SOAP}\"><s:Body \
             xmlns:m=\"{MESSAGES}\" xmlns:t=\"{TYPES}\">{body}</s:Body></s:Envelope>"
        )
    }

    #[test]
    fn builds_folder_tree() {
        let xml = wrap(
            "<m:FindFolderResponse><m:ResponseMessages><m:FindFolderResponseMessage ResponseClass=\"Success\">\
             <m:ResponseCode>NoError</m:ResponseCode><m:RootFolder TotalItemsInView=\"4\"><t:Folders>\
             <t:Folder><t:FolderId Id=\"INBOX\"/><t:ParentFolderId Id=\"ROOT\"/><t:FolderClass>IPF.Note</t:FolderClass>\
               <t:DisplayName>Inbox</t:DisplayName><t:TotalCount>12</t:TotalCount><t:UnreadCount>3</t:UnreadCount></t:Folder>\
             <t:Folder><t:FolderId Id=\"P\"/><t:ParentFolderId Id=\"INBOX\"/><t:FolderClass>IPF.Note</t:FolderClass>\
               <t:DisplayName>Projects/2026</t:DisplayName><t:TotalCount>1</t:TotalCount><t:UnreadCount>0</t:UnreadCount></t:Folder>\
             <t:CalendarFolder><t:FolderId Id=\"CAL\"/><t:ParentFolderId Id=\"ROOT\"/><t:FolderClass>IPF.Appointment</t:FolderClass>\
               <t:DisplayName>Calendar</t:DisplayName></t:CalendarFolder>\
             <t:Folder><t:FolderId Id=\"C\"/><t:ParentFolderId Id=\"ROOT\"/><t:FolderClass>IPF.Contact</t:FolderClass>\
               <t:DisplayName>Contacts</t:DisplayName></t:Folder>\
             <t:Folder><t:FolderId Id=\"A\"/><t:ParentFolderId Id=\"ROOT\"/><t:DisplayName>Archive</t:DisplayName></t:Folder>\
             </t:Folders></m:RootFolder></m:FindFolderResponseMessage></m:ResponseMessages></m:FindFolderResponse>",
        );
        let doc = Document::parse(&xml).unwrap();
        let kinds = HashMap::from([("INBOX".to_string(), FolderKind::Inbox)]);
        let folders = build_tree("ROOT", &kinds, parse_folders(&doc));
        let paths: Vec<(&str, FolderKind)> = folders.iter().map(|f| (f.path.as_str(), f.kind)).collect();
        assert_eq!(paths, vec![("Inbox", FolderKind::Inbox), ("Inbox/Projects\u{2215}2026", FolderKind::Other), ("Archive", FolderKind::Archive)]);
        assert_eq!((folders[0].total, folders[0].unread, folders[0].remote_id.as_str()), (12, 3, "INBOX"));
        assert_eq!(folders[1].depth(), 1);
    }

    #[test]
    fn parses_find_item() {
        let xml = wrap(
            "<m:FindItemResponse><m:ResponseMessages><m:FindItemResponseMessage ResponseClass=\"Success\">\
             <m:ResponseCode>NoError</m:ResponseCode><m:RootFolder IndexedPagingOffset=\"1\" TotalItemsInView=\"731\" IncludesLastItemInRange=\"false\">\
             <t:Items><t:Message><t:ItemId Id=\"AAMk1\" ChangeKey=\"CQAA\"/><t:Subject>Budget &amp; plan</t:Subject>\
             <t:DateTimeReceived>2026-10-14T00:12:34Z</t:DateTimeReceived><t:Size>4521</t:Size><t:HasAttachments>true</t:HasAttachments>\
             <t:DisplayTo>Alex Wilber; Bob</t:DisplayTo><t:Preview>Hi   all,\n see attached</t:Preview>\
             <t:Flag><t:FlagStatus>Flagged</t:FlagStatus></t:Flag>\
             <t:From><t:Mailbox><t:Name>Megan Bowen</t:Name><t:EmailAddress>megan@contoso.com</t:EmailAddress></t:Mailbox></t:From>\
             <t:IsRead>false</t:IsRead></t:Message>\
             <t:MeetingRequest><t:ItemId Id=\"AAMk2\"/><t:Subject>Invite</t:Subject><t:IsRead>true</t:IsRead></t:MeetingRequest>\
             </t:Items></m:RootFolder></m:FindItemResponseMessage></m:ResponseMessages></m:FindItemResponse>",
        );
        let doc = Document::parse(&xml).unwrap();
        let items = parse_items(&doc);
        assert_eq!(items.len(), 2);
        let m = &items[0];
        assert_eq!((m.remote_id.as_str(), m.subject.as_str(), m.from_name.as_str()), ("AAMk1", "Budget & plan", "Megan Bowen"));
        assert_eq!((m.date, m.size, m.to.as_str()), (1_791_936_754, 4521, "Alex Wilber; Bob"));
        assert!(!m.seen && m.flagged && m.has_attachments);
        assert_eq!(m.preview, "Hi all, see attached");
        assert!(items[1].seen);
    }

    #[test]
    fn parses_calendar_items_and_errors() {
        let xml = wrap(
            "<m:GetItemResponse><m:ResponseMessages><m:GetItemResponseMessage ResponseClass=\"Success\"><m:Items>\
             <t:CalendarItem><t:ItemId Id=\"E1\"/><t:Subject>Design review</t:Subject><t:Body BodyType=\"Text\">Agenda</t:Body>\
             <t:UID>040000008200E00074C5B7101A82E008</t:UID><t:Start>2026-10-15T17:00:00Z</t:Start><t:End>2026-10-15T18:00:00Z</t:End>\
             <t:IsAllDayEvent>false</t:IsAllDayEvent><t:Location>Room 4</t:Location><t:IsMeeting>true</t:IsMeeting>\
             <t:IsCancelled>false</t:IsCancelled><t:CalendarItemType>Occurrence</t:CalendarItemType><t:MyResponseType>Tentative</t:MyResponseType>\
             <t:Organizer><t:Mailbox><t:Name>Megan</t:Name><t:EmailAddress>megan@contoso.com</t:EmailAddress></t:Mailbox></t:Organizer>\
             <t:RequiredAttendees><t:Attendee><t:Mailbox><t:EmailAddress>alex@contoso.com</t:EmailAddress></t:Mailbox></t:Attendee></t:RequiredAttendees>\
             </t:CalendarItem>\
             <t:CalendarItem><t:ItemId Id=\"E2\"/><t:Subject>Focus</t:Subject><t:Start>2026-10-16T01:00:00Z</t:Start>\
             <t:End>2026-10-16T02:00:00Z</t:End><t:IsMeeting>false</t:IsMeeting><t:MyResponseType>Organizer</t:MyResponseType>\
             <t:CalendarItemType>Single</t:CalendarItemType></t:CalendarItem>\
             </m:Items></m:GetItemResponseMessage>\
             <m:GetItemResponseMessage ResponseClass=\"Error\"><m:MessageText>The specified object was not found in the store.</m:MessageText>\
             <m:ResponseCode>ErrorItemNotFound</m:ResponseCode></m:GetItemResponseMessage></m:ResponseMessages></m:GetItemResponse>",
        );
        let doc = Document::parse(&xml).unwrap();
        let events = parse_calendar(&doc, "acct");
        assert_eq!(events.len(), 2);
        let e = &events[0];
        assert_eq!((e.subject.as_str(), e.location.as_str(), e.body.as_str()), ("Design review", "Room 4", "Agenda"));
        assert_eq!(e.start, Utc.with_ymd_and_hms(2026, 10, 15, 17, 0, 0).unwrap().timestamp());
        assert_eq!((e.response, e.recurring, e.attendees.len()), (Response::Tentative, true, 1));
        // A plain appointment you own is not a meeting.
        assert_eq!((events[1].response, events[1].recurring), (Response::None, false));
        assert_eq!(check_all(&doc).unwrap_err(), "Exchange reported an error: The specified object was not found in the store. (ErrorItemNotFound)");
    }

    #[test]
    fn parses_autodiscover() {
        let xml = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
            <Autodiscover xmlns=\"http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006\">\
            <Response xmlns=\"http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a\">\
            <Account><AccountType>email</AccountType><Action>settings</Action>\
            <Protocol><Type>EXCH</Type><EwsUrl>https://ex01.corp.contoso.com/EWS/Exchange.asmx</EwsUrl></Protocol>\
            <Protocol><Type>EXPR</Type><EwsUrl>https://mail.contoso.com/EWS/Exchange.asmx</EwsUrl></Protocol>\
            </Account></Response></Autodiscover>";
        assert!(matches!(parse_autodiscover(xml), Some(Discovered::Ews(u)) if u == "https://mail.contoso.com/EWS/Exchange.asmx"));
        let redirect = "<Autodiscover xmlns=\"x\"><Response><Account><Action>redirectAddr</Action>\
            <RedirectAddr>alex@corp.contoso.com</RedirectAddr></Account></Response></Autodiscover>";
        assert!(matches!(parse_autodiscover(redirect), Some(Discovered::RedirectAddr(a)) if a == "alex@corp.contoso.com"));
    }

    /// Hits a real Exchange endpoint over TLS; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_exchange_endpoint_and_probe() {
        let err = check("nobody@contoso.com", "nobody@contoso.com", "wrong", "https://outlook.office365.com/EWS/Exchange.asmx").unwrap_err();
        println!("EWS check error: {err}");
        assert!(err.contains("rejected") || err.contains("sign-in method") || err.contains("HTTP"), "{err}");
        assert!(!looks_like_exchange("example.com"));
    }

    /// Canned EWS answers keyed by operation; every request must be well-formed XML.
    fn mock_answer(request: &str) -> String {
        let doc = Document::parse(request).expect("request is well-formed XML");
        let op = descendants(doc.root(), "Body").next().unwrap().first_element_child().unwrap();
        let ok = |name: &str, inner: &str| {
            format!("<m:{name}ResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode>{inner}</m:{name}ResponseMessage>")
        };
        let body = match op.tag_name().name() {
            "GetFolder" => {
                let ids: Vec<String> = descendants(op, "DistinguishedFolderId").map(|n| n.attribute("Id").unwrap().to_uppercase()).collect();
                ids.iter().map(|id| ok("GetFolder", &format!("<m:Folders><t:Folder><t:FolderId Id=\"{id}\"/></t:Folder></m:Folders>"))).collect()
            }
            "FindFolder" => ok(
                "FindFolder",
                "<m:RootFolder TotalItemsInView=\"2\"><t:Folders>\
                 <t:Folder><t:FolderId Id=\"INBOX\"/><t:ParentFolderId Id=\"MSGFOLDERROOT\"/><t:FolderClass>IPF.Note</t:FolderClass>\
                 <t:DisplayName>Inbox</t:DisplayName><t:TotalCount>2</t:TotalCount><t:UnreadCount>1</t:UnreadCount></t:Folder>\
                 <t:Folder><t:FolderId Id=\"DELETEDITEMS\"/><t:ParentFolderId Id=\"MSGFOLDERROOT\"/><t:FolderClass>IPF.Note</t:FolderClass>\
                 <t:DisplayName>Deleted Items</t:DisplayName><t:TotalCount>0</t:TotalCount><t:UnreadCount>0</t:UnreadCount></t:Folder>\
                 </t:Folders></m:RootFolder>",
            ),
            "FindItem" if descendants(op, "CalendarView").next().is_some() => {
                ok("FindItem", "<m:RootFolder><t:Items><t:CalendarItem><t:ItemId Id=\"EV1\"/></t:CalendarItem></t:Items></m:RootFolder>")
            }
            "FindItem" => {
                assert_eq!(descendants(op, "FolderId").next().unwrap().attribute("Id"), Some("INBOX"));
                ok(
                    "FindItem",
                    "<m:RootFolder TotalItemsInView=\"2\"><t:Items>\
                     <t:Message><t:ItemId Id=\"M2\"/><t:Subject>Second</t:Subject><t:DateTimeReceived>2026-10-14T02:00:00Z</t:DateTimeReceived>\
                     <t:IsRead>false</t:IsRead><t:Size>900</t:Size></t:Message>\
                     <t:Message><t:ItemId Id=\"M1\"/><t:Subject>First &lt;hi&gt;</t:Subject><t:DateTimeReceived>2026-10-14T01:00:00Z</t:DateTimeReceived>\
                     <t:IsRead>true</t:IsRead><t:Size>800</t:Size></t:Message></t:Items></m:RootFolder>",
                )
            }
            "GetItem" if descendants(op, "IncludeMimeContent").next().is_some() => {
                let mime = B64.encode("From: a@b.c\r\nSubject: Second\r\n\r\nHello from Exchange\r\n");
                ok("GetItem", &format!("<m:Items><t:Message><t:MimeContent CharacterSet=\"UTF-8\">{mime}</t:MimeContent></t:Message></m:Items>"))
            }
            "GetItem" => ok(
                "GetItem",
                "<m:Items><t:CalendarItem><t:ItemId Id=\"EV1\"/><t:Subject>Standup</t:Subject><t:UID>U1</t:UID>\
                 <t:Start>2026-10-14T23:00:00Z</t:Start><t:End>2026-10-14T23:15:00Z</t:End><t:IsMeeting>false</t:IsMeeting>\
                 <t:CalendarItemType>Single</t:CalendarItemType></t:CalendarItem></m:Items>",
            ),
            "UpdateItem" => {
                assert_eq!(text_deep(op, "IsRead"), "true");
                ok("UpdateItem", "")
            }
            "MoveItem" => {
                assert_eq!(descendants(op, "FolderId").next().unwrap().attribute("Id"), Some("DELETEDITEMS"));
                ok("MoveItem", "")
            }
            "CreateItem" => {
                let mime = B64.decode(text_deep(op, "MimeContent")).unwrap();
                let mime = String::from_utf8(mime).unwrap();
                assert!(mime.contains("Bcc: secret@contoso.com"), "Bcc kept for Exchange: {mime}");
                ok("CreateItem", "")
            }
            other => panic!("unexpected EWS operation {other}"),
        };
        wrap(&format!("<m:Response><m:ResponseMessages>{body}</m:ResponseMessages></m:Response>"))
    }

    /// A local HTTP server that answers `n` EWS requests from `mock_answer`.
    fn mock_server(n: usize) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/EWS/Exchange.asmx", listener.local_addr().unwrap().port());
        let handle = std::thread::spawn(move || {
            let mut ops = Vec::new();
            for _ in 0..n {
                let (mut s, _) = listener.accept().unwrap();
                let mut data = Vec::new();
                let mut buf = [0u8; 16384];
                let body = loop {
                    let k = s.read(&mut buf).unwrap();
                    data.extend_from_slice(&buf[..k]);
                    let text = String::from_utf8_lossy(&data).into_owned();
                    if let Some(split) = text.find("\r\n\r\n") {
                        let len: usize = text[..split]
                            .lines()
                            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                            .unwrap_or(0);
                        if data.len() >= split + 4 + len {
                            break text[split + 4..split + 4 + len].to_string();
                        }
                    }
                };
                let answer = mock_answer(&body);
                ops.push(Document::parse(&body).unwrap().descendants().find(|n| n.tag_name().name() == "Body").unwrap().first_element_child().unwrap().tag_name().name().to_string());
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}", answer.len());
                s.write_all(resp.as_bytes()).unwrap();
            }
            ops
        });
        (url, handle)
    }

    #[test]
    fn talks_to_a_mock_exchange_server() {
        let (url, server) = mock_server(9);
        let config = crate::model::AccountConfig {
            id: "ews-test".into(),
            email: "alex@contoso.com".into(),
            display_name: "Alex".into(),
            protocol: crate::model::Protocol::Ews,
            ..Default::default()
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let emitter = super::super::Emitter { account: config.id.clone(), tx, ctx: Default::default() };
        let client = winhttp::Client::new(None, 10).unwrap();
        let mut b = EwsBackend::with_client(Auth::new(config, emitter), client, &url);

        // Folder tree (GetFolder + FindFolder).
        let folders = b.folders().unwrap();
        let inbox = folders.iter().find(|f| f.kind == FolderKind::Inbox).unwrap().clone();
        let trash = folders.iter().find(|f| f.kind == FolderKind::Deleted).unwrap().clone();
        assert_eq!((inbox.path.as_str(), inbox.unread), ("Inbox", 1));

        // Sync (FindItem): newest first, numbered locally.
        let mut cache = FolderCache::default();
        let outcome = b.sync(&inbox, &mut cache, 500).unwrap();
        assert_eq!(outcome.total, 2);
        let subjects: Vec<&str> = cache.messages.iter().map(|m| m.subject.as_str()).collect();
        assert_eq!(subjects, vec!["Second", "First <hi>"]);

        // Body (GetItem with MIME), read flag (UpdateItem), delete (MoveItem to Deleted Items).
        let second = cache.messages[0].clone();
        let fetched = b.fetch(&inbox, std::slice::from_ref(&second)).unwrap();
        assert_eq!(fetched[0].0, second.uid);
        assert!(String::from_utf8_lossy(&fetched[0].1).contains("Hello from Exchange"));
        b.set_seen(&inbox, std::slice::from_ref(&second), true).unwrap();
        b.move_to(&inbox, std::slice::from_ref(&second), &trash).unwrap();

        // Send (CreateItem with MIME, Bcc kept).
        let draft = Draft { to: "bob@contoso.com".into(), bcc: "secret@contoso.com".into(), subject: "Hi".into(), body: "Hello".into(), plain_text: true, ..Default::default() };
        b.send(&draft, &folders).unwrap();

        // Calendar (FindItem CalendarView + GetItem).
        let events = b.calendar_view(0, i64::from(u32::MAX)).unwrap();
        assert_eq!((events[0].subject.as_str(), events[0].uid.as_str(), events[0].response), ("Standup", "U1", Response::None));

        let ops = server.join().unwrap();
        assert_eq!(ops, ["GetFolder", "FindFolder", "FindItem", "GetItem", "UpdateItem", "MoveItem", "CreateItem", "FindItem", "GetItem"]);
    }

    #[test]
    fn soap_fault_and_escaping() {
        let fault = format!(
            "<s:Envelope xmlns:s=\"{SOAP}\"><s:Body><s:Fault><faultcode>a:ErrorSchemaValidation</faultcode>\
             <faultstring>The request failed schema validation.</faultstring></s:Fault></s:Body></s:Envelope>"
        );
        assert_eq!(parse_doc(&fault).unwrap_err(), "Exchange reported an error: The request failed schema validation.");
        assert_eq!(x("a<b & \"c\""), "a&lt;b &amp; &quot;c&quot;");
        let env = envelope("<m:GetFolder/>", Some("AUS Eastern Standard Time"));
        let doc = Document::parse(&env).unwrap();
        assert_eq!(text_deep(doc.root(), "TimeZoneDefinition"), "");
        assert!(env.contains("<t:TimeZoneDefinition Id=\"AUS Eastern Standard Time\"/>"));
    }
}
