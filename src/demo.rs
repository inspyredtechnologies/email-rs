//! Offline sample mailbox for `--demo`, so the UI can be explored without an account.

use crate::model::{AccountConfig, Folder, FolderKind, MessageBody, MessageMeta};

pub const ACCOUNT_ID: &str = "demo";

pub fn account() -> AccountConfig {
    AccountConfig {
        id: ACCOUNT_ID.into(),
        display_name: "Alex Wilber".into(),
        email: "alex@contoso.example".into(),
        ..Default::default()
    }
}

fn folder(path: &str, kind: FolderKind, unread: u32, total: u32) -> Folder {
    Folder { path: path.into(), delimiter: Some("/".into()), kind, selectable: true, unread, total }
}

pub fn folders() -> Vec<Folder> {
    vec![
        folder("INBOX", FolderKind::Inbox, 4, 9),
        folder("Drafts", FolderKind::Drafts, 0, 0),
        folder("Sent", FolderKind::Sent, 0, 1),
        folder("Trash", FolderKind::Deleted, 0, 0),
        folder("Archive", FolderKind::Archive, 0, 0),
        folder("Junk", FolderKind::Junk, 0, 0),
        folder("Projects", FolderKind::Other, 0, 0),
        folder("Projects/Northwind", FolderKind::Other, 0, 0),
    ]
}

/// (folder, meta, body text, attachments)
pub fn messages(now: i64) -> Vec<(String, MessageMeta, MessageBody)> {
    let hour = 3600;
    let day = 24 * hour;
    let rows: [(&str, &str, &str, &str, i64, bool, bool, &str); 10] = [
        ("INBOX", "Megan Bowen", "megan@contoso.example", "Q4 planning offsite agenda", 2 * hour, false, true,
         "Hi Alex,\n\nAttached is the draft agenda for the Q4 planning offsite. Could you review the product section and send me your edits by Thursday?\n\nThanks,\nMegan"),
        ("INBOX", "Nestor Wilke", "nestor@contoso.example", "RE: Build pipeline is green again", 5 * hour, false, false,
         "The flaky integration test was a timezone issue in the fixture. Fixed in main; all pipelines are green again.\n\nNestor"),
        ("INBOX", "Contoso IT", "it@contoso.example", "Scheduled maintenance this weekend", 9 * hour, false, false,
         "Email and file services will be unavailable Saturday from 10 PM to 2 AM while we upgrade storage.\n\n- Contoso IT"),
        ("INBOX", "Lee Gu", "lee@contoso.example", "Lunch on Friday?", day + 3 * hour, true, false,
         "Want to try the new ramen place on Friday? I can book for 12:30."),
        ("INBOX", "Isaiah Langer", "isaiah@contoso.example", "Northwind contract redlines", 2 * day, true, false,
         "Legal returned the redlines on the Northwind contract. Two open items: payment terms and the SLA credit cap. Can we talk tomorrow?"),
        ("INBOX", "Diego Siciliani", "diego@contoso.example", "Customer feedback summary - September", 6 * day, true, false,
         "Top themes this month: faster search, offline mode, and better calendar sharing. Full report on the team site."),
        ("INBOX", "Patti Fernandez", "patti@contoso.example", "Welcome to the team!", 12 * day, true, false,
         "Welcome aboard, Alex! Your onboarding buddy is Lee. Let me know if you need anything."),
        ("INBOX", "Microsoft Outlook", "no-reply@contoso.example", "Microsoft Outlook Test Message", 40 * day, true, false,
         "This is an email message sent automatically by Microsoft Outlook while testing the settings for your account."),
        ("INBOX", "Megan Bowen", "megan@contoso.example", "Design review", 40 * 60, false, false,
         "Let's walk through the Q4 designs before the offsite. Please come with feedback on the mockups."),
        ("Sent", "Alex Wilber", "alex@contoso.example", "RE: Lunch on Friday?", day + 2 * hour, true, false,
         "Sounds great, 12:30 works."),
    ];
    rows.iter()
        .enumerate()
        .map(|(i, (folder, name, addr, subject, age, seen, has_att, text))| {
            let date = now - age;
            let meta = MessageMeta {
                uid: i as u32 + 1,
                from_name: name.to_string(),
                from_addr: addr.to_string(),
                to: "Alex Wilber <alex@contoso.example>".into(),
                subject: subject.to_string(),
                date,
                seen: *seen,
                flagged: i == 4,
                answered: false,
                has_attachments: *has_att,
                size: text.len() as u32,
                preview: text.split_whitespace().collect::<Vec<_>>().join(" "),
            };
            let body = MessageBody {
                subject: subject.to_string(),
                from: format!("{name} <{addr}>"),
                to: meta.to.clone(),
                date,
                text: text.to_string(),
                html: (i == 5).then(|| FEEDBACK_HTML.to_string()),
                invite: (i == 8).then(design_review_invite),
                message_id: Some(format!("demo-{i}@contoso.example")),
                attachments: if *has_att {
                    vec![crate::model::Attachment { name: "Q4 Offsite Agenda.txt".into(), data: b"9:00 Welcome\n10:00 Product\n".to_vec() }]
                } else {
                    Vec::new()
                },
                ..Default::default()
            };
            (folder.to_string(), meta, body)
        })
        .collect()
}

/// HTML demo message: a styled report with a table, and one remote image
/// (blocked until "download pictures" is clicked).
const FEEDBACK_HTML: &str = r#"<html><head><style>
  .hero { background:#0072C6; color:#fff; padding:18px 22px; }
  .hero h1 { margin:0; font-weight:300; font-size:24px; }
  table { border-collapse:collapse; width:100%; margin:14px 0; }
  th, td { border-bottom:1px solid #e1e1e1; padding:8px 10px; text-align:left; }
  th { background:#f3f3f3; font-weight:600; }
  .up { color:#107C10; font-weight:600; } .down { color:#C4312B; font-weight:600; }
</style></head><body>
<div class="hero"><h1>Customer feedback &mdash; September</h1><div>Monthly summary for the product team</div></div>
<p>Hi team, here are the top themes from 1,284 pieces of feedback this month:</p>
<table>
  <tr><th>Theme</th><th>Mentions</th><th>Trend</th></tr>
  <tr><td>Faster search</td><td>412</td><td class="up">&#9650; 18%</td></tr>
  <tr><td>Offline mode</td><td>305</td><td class="up">&#9650; 7%</td></tr>
  <tr><td>Calendar sharing</td><td>221</td><td class="down">&#9660; 4%</td></tr>
</table>
<p><img src="https://example.com/feedback-chart.png" alt="[Chart: feedback by week]" width="480" height="160"></p>
<p>Full report on the <a href="https://example.com/feedback">team site</a>. Questions? <a href="mailto:diego@contoso.example">Email Diego</a>.</p>
<p style="color:#666;font-size:12px">Contoso Research &bull; You're receiving this because you're on the product team.</p>
</body></html>"#;

fn at(days_from_today: i64, minutes: u32) -> i64 {
    let day = chrono::Local::now().date_naive() + chrono::Duration::days(days_from_today);
    crate::calendar::local_ts(day, minutes)
}

/// The meeting request attached to the "Design review" message: tomorrow 10-11 AM.
fn design_review_invite() -> crate::ics::Invite {
    use crate::ics::{Invite, Method, Person};
    Invite {
        method: Method::Request,
        uid: "demo-design-review@contoso.example".into(),
        sequence: 0,
        summary: "Design review".into(),
        location: "Conf Room 4 / Teams".into(),
        description: "Let's walk through the Q4 designs before the offsite.".into(),
        start: at(1, 10 * 60),
        end: at(1, 11 * 60),
        all_day: false,
        organizer: Some(Person { name: "Megan Bowen".into(), email: "megan@contoso.example".into() }),
        attendees: vec![Person { name: "Alex Wilber".into(), email: "alex@contoso.example".into() }],
        recurring: false,
    }
}

/// Local calendar for the demo, around today (never saved to disk).
pub fn events() -> Vec<crate::calendar::Event> {
    use crate::calendar::{Event, LOCAL, Response};
    let ev = |id: &str, subject: &str, location: &str, start: i64, end: i64, all_day: bool, response: Response| Event {
        id: id.into(),
        calendar: LOCAL.into(),
        uid: format!("{id}@demo"),
        subject: subject.into(),
        location: location.into(),
        start,
        end,
        all_day,
        organizer: if response == Response::None { String::new() } else { "megan@contoso.example".into() },
        organizer_name: if response == Response::None { String::new() } else { "Megan Bowen".into() },
        response,
        ..Default::default()
    };
    let (s, e) = crate::calendar::day_bounds(chrono::Local::now().date_naive() + chrono::Duration::days(3));
    vec![
        ev("standup", "Team standup", "Teams", at(0, 9 * 60), at(0, 9 * 60 + 15), false, Response::Accepted),
        ev("1on1", "1:1 with Megan", "Megan's office", at(0, 14 * 60), at(0, 14 * 60 + 30), false, Response::Accepted),
        ev("call", "Customer call: Northwind", "Teams", at(0, 14 * 60 + 15), at(0, 15 * 60), false, Response::Tentative),
        ev("focus", "Focus time", "", at(-1, 13 * 60), at(-1, 15 * 60), false, Response::None),
        ev("lunch", "Lunch with Lee", "Ramen place", at(2, 12 * 60 + 30), at(2, 13 * 60 + 30), false, Response::None),
        ev("offsite", "Q4 planning offsite", "Seattle", s, e, true, Response::Accepted),
        ev("dentist", "Dentist", "", at(-2, 8 * 60), at(-2, 9 * 60), false, Response::None),
    ]
}
