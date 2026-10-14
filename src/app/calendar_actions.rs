//! Calendar actions: creating, editing and deleting events, meeting invites
//! and responses (by email or through Microsoft Graph), Microsoft calendar sync,
//! and the one-time calendar connection.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use chrono::Local;

use super::{App, Module};
use crate::calendar::{self, Event, LOCAL, Response};
use crate::ics::{self, EventSpec, Person};
use crate::model::{AccountConfig, AuthMethod, Draft};
use crate::worker::{Command, Event as WorkerEvent};
use crate::{mime, oauth, store, ui};

/// "Thursday, October 15, 2026 10:00 AM–11:00 AM" (or "... (all day)").
pub fn when_text(start: i64, end: i64, all_day: bool) -> String {
    let date = calendar::local_date(start).format("%A, %B %-d, %Y");
    if all_day {
        return format!("{date} (all day)");
    }
    let t = |ts| ui::calendar::time_of(calendar::local_minutes(ts));
    if calendar::local_date(end) == calendar::local_date(start) {
        format!("{date} {}\u{2013}{}", t(start), t(end))
    } else {
        format!("{date} {} \u{2013} {} {}", t(start), calendar::local_date(end).format("%A, %B %-d, %Y"), t(end))
    }
}

impl App {
    pub(super) fn load_calendars(&mut self) {
        self.cal.local = calendar::load(LOCAL);
        for id in self.microsoft_accounts() {
            self.cal.remote.insert(id.clone(), calendar::load(&id));
        }
    }

    fn microsoft_accounts(&self) -> Vec<String> {
        self.accounts.iter().filter(|a| a.config.auth == AuthMethod::Microsoft).map(|a| a.config.id.clone()).collect()
    }

    fn save_local(&self) {
        // The demo never touches the real calendar file.
        if !self.demo {
            calendar::save(LOCAL, &self.cal.local);
        }
    }

    /// Each frame: finish a pending calendar connection, and keep Microsoft
    /// calendars synced for the visible range while the calendar is open.
    pub(super) fn calendar_tick(&mut self) {
        if let Some((acct, rx)) = &self.cal.connecting {
            if let Ok(result) = rx.try_recv() {
                let acct = acct.clone();
                self.cal.connecting = None;
                match result {
                    Ok(()) => {
                        self.cal.needs_connect.remove(&acct);
                        self.cal.errors.remove(&acct);
                        self.cal.synced.remove(&acct);
                    }
                    Err(e) => {
                        self.cal.errors.insert(acct, e);
                    }
                }
            }
        }
        if self.module != Module::Calendar {
            return;
        }
        let days = self.cal.days();
        let from = calendar::day_bounds(days[0]).0;
        let to = calendar::day_bounds(*days.last().unwrap()).1;
        for acct in self.microsoft_accounts() {
            if self.cal.needs_connect.contains(&acct) || !self.workers.contains_key(&acct) {
                continue;
            }
            let covered = self.cal.synced.get(&acct).is_some_and(|(a, b)| *a <= from && *b >= to);
            if !covered {
                // Fetch a month either side so paging around stays instant.
                let window = (from - 31 * 86_400, to + 31 * 86_400);
                self.cal.synced.insert(acct.clone(), window);
                self.send_cmd(&acct, Command::CalendarSync { from: window.0, to: window.1 });
            }
        }
    }

    pub(super) fn calendar_event(&mut self, ev: WorkerEvent) {
        match ev {
            WorkerEvent::CalendarEvents { account, events } => {
                self.cal.errors.remove(&account);
                self.cal.needs_connect.remove(&account);
                self.cal.remote.insert(account, events);
            }
            WorkerEvent::CalendarError { account, text } => {
                self.cal.errors.insert(account, text);
            }
            WorkerEvent::CalendarNeedsConnect { account } => {
                self.cal.needs_connect.insert(account.clone());
                self.cal.synced.remove(&account);
            }
            _ => {}
        }
    }

    /// The account to send meeting mail from: the organizer's, else the current one.
    fn mail_account_for(&self, email: &str) -> Option<AccountConfig> {
        self.accounts
            .iter()
            .find(|a| a.config.email.eq_ignore_ascii_case(email))
            .or_else(|| self.current_account())
            .or_else(|| self.accounts.first())
            .map(|a| a.config.clone())
    }

    fn me(&self) -> Option<AccountConfig> {
        self.current_account().or_else(|| self.accounts.first()).map(|a| a.config.clone())
    }

    pub(super) fn new_appointment(&mut self, start: Option<i64>, all_day: bool, meeting: bool) {
        // Default to the next half hour, like Outlook.
        let start = start.unwrap_or_else(|| {
            let now = Local::now().timestamp();
            (now / 1800 + 1) * 1800
        });
        // New items go to the first connected Microsoft calendar, else the local one.
        let calendar = self
            .microsoft_accounts()
            .into_iter()
            .find(|a| !self.cal.needs_connect.contains(a) && self.workers.contains_key(a))
            .unwrap_or_else(|| LOCAL.to_string());
        let me = self.me().unwrap_or_default();
        let event = Event {
            calendar,
            uid: calendar::new_id(),
            start: if all_day { calendar::day_bounds(calendar::local_date(start)).0 } else { start },
            end: if all_day { calendar::day_bounds(calendar::local_date(start)).1 } else { start + 1800 },
            all_day,
            organizer: me.email,
            organizer_name: me.display_name,
            response: if meeting { Response::Organizer } else { Response::None },
            ..Default::default()
        };
        self.open_editor(event, true, meeting);
    }

    fn open_editor(&mut self, event: Event, is_new: bool, meeting: bool) {
        let id = self.cal.next_editor;
        self.cal.next_editor += 1;
        self.cal.editors.push(ui::appointment::Editor::new(id, event, is_new, meeting));
    }

    pub(super) fn open_event(&mut self, cal: &str, id: &str) {
        if let Some(e) = self.cal.find(cal, id).cloned() {
            self.open_editor(e, false, false);
        }
    }

    fn upsert(list: &mut Vec<Event>, e: Event) {
        match list.iter_mut().find(|x| x.id == e.id && !e.id.is_empty()) {
            Some(x) => *x = e,
            None => list.push(e),
        }
    }

    pub(super) fn save_event(&mut self, mut e: Event, is_new: bool) {
        if e.calendar == LOCAL {
            if e.id.is_empty() {
                e.id = calendar::new_id();
            }
            if !e.attendees.is_empty() {
                // We organize meetings created here: email the invitations.
                e.response = Response::Organizer;
                if !is_new {
                    e.sequence += 1;
                }
                self.send_invite("REQUEST", &e);
            }
            Self::upsert(&mut self.cal.local, e);
            self.save_local();
        } else {
            // Graph creates/updates it (and emails attendees); show it right away.
            let account = e.calendar.clone();
            Self::upsert(self.cal.remote.entry(account.clone()).or_default(), e.clone());
            self.send_cmd(&account, Command::CalendarSave(Box::new(e)));
        }
    }

    pub(super) fn delete_event(&mut self, e: Event) {
        if self.cal.selected.as_ref() == Some(&(e.calendar.clone(), e.id.clone())) {
            self.cal.selected = None;
        }
        if e.calendar == LOCAL {
            if e.response == Response::Organizer && !e.attendees.is_empty() {
                let cancel = Event { sequence: e.sequence + 1, ..e.clone() };
                self.send_invite("CANCEL", &cancel);
            }
            self.cal.local.retain(|x| x.id != e.id);
            self.save_local();
        } else {
            if let Some(list) = self.cal.remote.get_mut(&e.calendar) {
                list.retain(|x| x.id != e.id);
            }
            self.send_cmd(&e.calendar, Command::CalendarDelete(e.id));
        }
    }

    /// Respond to a meeting in a calendar (from the appointment window).
    pub(super) fn respond_event(&mut self, e: Event, response: Response) {
        if e.calendar == LOCAL {
            let organizer = Person { name: e.organizer_name.clone(), email: e.organizer.clone() };
            self.reply_by_email(&e, &organizer, response);
            self.record_local_response(e, response);
        } else {
            if let Some(x) = self.cal.remote.get_mut(&e.calendar).and_then(|l| l.iter_mut().find(|x| x.id == e.id)) {
                x.response = response;
            }
            self.send_cmd(&e.calendar, Command::CalendarRespond { id: e.id, response });
        }
    }

    fn record_local_response(&mut self, mut e: Event, response: Response) {
        self.cal.local.retain(|x| x.uid != e.uid);
        if response != Response::Declined {
            e.response = response;
            e.calendar = LOCAL.into();
            if e.id.is_empty() {
                e.id = calendar::new_id();
            }
            self.cal.local.push(e);
        }
        self.save_local();
    }

    /// Accept / Tentative / Decline on an invite in the reading pane.
    pub(super) fn respond_invite(&mut self, response: Response) {
        let Some(inv) = self.selected_body().and_then(|b| b.invite.clone()) else { return };
        // Exchange puts invitations on the Microsoft calendar itself: respond there.
        if let Some(e) = self.cal.remote.values().flatten().find(|e| e.uid == inv.uid).cloned() {
            self.respond_event(e, response);
            return;
        }
        let Some(organizer) = inv.organizer.clone() else {
            self.alert = Some("This invitation has no organizer to respond to.".into());
            return;
        };
        let e = Event {
            uid: inv.uid.clone(),
            subject: inv.summary.clone(),
            location: inv.location.clone(),
            body: inv.description.clone(),
            start: inv.start,
            end: inv.end,
            all_day: inv.all_day,
            organizer: organizer.email.clone(),
            organizer_name: organizer.name.clone(),
            attendees: inv.attendees.iter().map(|p| p.email.clone()).collect(),
            sequence: inv.sequence,
            recurring: inv.recurring,
            ..Default::default()
        };
        self.reply_by_email(&e, &organizer, response);
        self.record_local_response(e, response);
    }

    /// "Remove from Calendar" on a cancellation.
    pub(super) fn remove_canceled_invite(&mut self) {
        let Some(inv) = self.selected_body().and_then(|b| b.invite.clone()) else { return };
        self.cal.local.retain(|x| x.uid != inv.uid);
        self.save_local();
        if let Some(e) = self.cal.remote.values().flatten().find(|e| e.uid == inv.uid).cloned() {
            self.delete_event(e);
        }
    }

    /// iMIP REPLY to the organizer (RFC 6047).
    fn reply_by_email(&mut self, e: &Event, organizer: &Person, response: Response) {
        let Some(acct) = self.me() else { return };
        let me = Person { name: acct.display_name.clone(), email: acct.email.clone() };
        let spec = EventSpec {
            uid: &e.uid,
            sequence: e.sequence,
            summary: &e.subject,
            location: &e.location,
            description: "",
            start: e.start,
            end: e.end,
            all_day: e.all_day,
            organizer,
            attendees: vec![(&me, response.partstat())],
        };
        let draft = Draft {
            account_id: acct.id.clone(),
            to: organizer.label(),
            subject: format!("{}: {}", response.verb(), e.subject),
            body: format!("{} has {} this meeting.", me.label(), response.verb().to_lowercase()),
            plain_text: true,
            calendar: Some(("REPLY".into(), ics::write("REPLY", &spec))),
            ..Default::default()
        };
        self.dispatch(draft);
    }

    /// iMIP REQUEST or CANCEL from the organizer to every attendee.
    fn send_invite(&mut self, method: &str, e: &Event) {
        let Some(acct) = self.mail_account_for(&e.organizer) else {
            self.alert = Some("Add a mail account to send meeting invitations.".into());
            return;
        };
        let organizer = Person { name: acct.display_name.clone(), email: acct.email.clone() };
        let people: Vec<Person> = e.attendees.iter().map(|a| Person { name: String::new(), email: a.clone() }).collect();
        let spec = EventSpec {
            uid: &e.uid,
            sequence: e.sequence,
            summary: &e.subject,
            location: &e.location,
            description: &e.body,
            start: e.start,
            end: e.end,
            all_day: e.all_day,
            organizer: &organizer,
            attendees: people.iter().map(|p| (p, "NEEDS-ACTION")).collect(),
        };
        let mut body = format!("When: {}\n", when_text(e.start, e.end, e.all_day));
        if !e.location.is_empty() {
            body += &format!("Where: {}\n", e.location);
        }
        if !e.body.is_empty() {
            body += &format!("\n{}\n", e.body);
        }
        let subject = if method == "CANCEL" { format!("Canceled: {}", e.subject) } else { e.subject.clone() };
        let draft = Draft {
            account_id: acct.id.clone(),
            to: e.attendees.join("; "),
            subject,
            body,
            plain_text: true,
            calendar: Some((method.into(), ics::write(method, &spec))),
            ..Default::default()
        };
        self.dispatch(draft);
    }

    /// Send meeting mail through the account's worker (or skip it in the demo).
    fn dispatch(&mut self, draft: Draft) {
        if self.demo {
            self.alert = Some("Demo mode: the meeting email was not sent.".into());
            return;
        }
        let Some(acct) = self.account(&draft.account_id).map(|a| a.config.clone()) else { return };
        match mime::build_message(&acct, &draft) {
            Ok(_) => self.send_cmd(&acct.id, Command::Send(Box::new(draft))),
            Err(e) => self.alert = Some(e),
        }
    }

    /// One-time browser sign-in granting calendar access (Microsoft Graph).
    pub(super) fn connect_calendar(&mut self, account: String) {
        let Some(email) = self.account(&account).map(|a| a.config.email.clone()) else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.ctx.clone();
        let id = account.clone();
        std::thread::spawn(move || {
            let client = oauth::load_client(AuthMethod::Microsoft);
            let result = oauth::sign_in_scoped(
                AuthMethod::Microsoft,
                &client,
                &email,
                Arc::new(AtomicBool::new(false)),
                Some(oauth::GRAPH_CALENDAR_SCOPES),
            )
            .and_then(|t| store::set_password(&crate::worker::graph_key(&id), t.refresh_token.as_deref().unwrap_or_default()));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.cal.errors.remove(&account);
        self.cal.connecting = Some((account, rx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_when() {
        let day = chrono::NaiveDate::from_ymd_opt(2026, 10, 15).unwrap();
        let (s, e) = (calendar::local_ts(day, 600), calendar::local_ts(day, 660));
        assert_eq!(when_text(s, e, false), "Thursday, October 15, 2026 10:00 AM\u{2013}11:00 AM");
        assert_eq!(when_text(s, e, true), "Thursday, October 15, 2026 (all day)");
    }
}
