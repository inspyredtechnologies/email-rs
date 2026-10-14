//! Microsoft Graph calendar API (Outlook.com and Microsoft 365 calendars).
//!
//! Reads use `calendarView`, which expands recurring series into instances.
//! All times are exchanged in UTC (`Prefer: outlook.timezone="UTC"`).

use std::time::Duration;

use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
use serde_json::{Value, json};

use crate::calendar::{self, Event, Response};

const BASE: &str = "https://graph.microsoft.com/v1.0";

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into()
}

/// Turn a Graph response into JSON, or an error with Graph's message.
fn read(resp: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Value, String> {
    let resp = resp.map_err(|e| format!("Couldn't reach Microsoft Graph: {e}"))?;
    let status = resp.status();
    let text = resp.into_body().read_to_string().map_err(|e| e.to_string())?;
    if status.is_success() {
        return Ok(if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).map_err(|e| e.to_string())? });
    }
    let msg = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| text.chars().take(200).collect());
    Err(format!("Microsoft Graph error {}: {msg}", status.as_u16()))
}

fn utc_string(ts: i64) -> String {
    Utc.timestamp_opt(ts, 0).single().map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string()).unwrap_or_default()
}

/// Parse Graph's `dateTimeTimeZone` (already UTC because of the Prefer header).
fn parse_time(v: &Value, all_day: bool) -> Option<i64> {
    let s = v["dateTime"].as_str()?;
    let t = NaiveDateTime::parse_from_str(s.get(0..19)?, "%Y-%m-%dT%H:%M:%S").ok()?;
    if all_day {
        // All-day events are pinned to a date; land them on local midnight of that date.
        let date = if t.time() == chrono::NaiveTime::MIN { t.date() } else { calendar::local_date(t.and_utc().timestamp()) };
        return Some(calendar::day_bounds(date).0);
    }
    Some(t.and_utc().timestamp())
}

fn parse_event(v: &Value, account: &str) -> Option<Event> {
    if v["isCancelled"].as_bool() == Some(true) {
        return None;
    }
    let all_day = v["isAllDay"].as_bool().unwrap_or(false);
    let response = match v["responseStatus"]["response"].as_str().unwrap_or("none") {
        "organizer" => Response::Organizer,
        "accepted" => Response::Accepted,
        "tentativelyAccepted" => Response::Tentative,
        "declined" => Response::Declined,
        "notResponded" => Response::NotResponded,
        _ => Response::None,
    };
    let attendees = v["attendees"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x["emailAddress"]["address"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Some(Event {
        id: v["id"].as_str()?.to_string(),
        calendar: account.to_string(),
        uid: v["iCalUId"].as_str().unwrap_or_default().to_string(),
        subject: v["subject"].as_str().unwrap_or_default().to_string(),
        location: v["location"]["displayName"].as_str().unwrap_or_default().to_string(),
        body: v["bodyPreview"].as_str().unwrap_or_default().to_string(),
        start: parse_time(&v["start"], all_day)?,
        end: parse_time(&v["end"], all_day)?,
        all_day,
        organizer_name: v["organizer"]["emailAddress"]["name"].as_str().unwrap_or_default().to_string(),
        organizer: v["organizer"]["emailAddress"]["address"].as_str().unwrap_or_default().to_string(),
        attendees,
        response,
        sequence: 0,
        recurring: v["type"].as_str().is_some_and(|t| t != "singleInstance"),
    })
}

/// Events (with recurrences expanded) between two UTC timestamps.
pub fn calendar_view(token: &str, account: &str, from: i64, to: i64) -> Result<Vec<Event>, String> {
    let agent = agent();
    let select = "id,iCalUId,subject,location,bodyPreview,start,end,isAllDay,isCancelled,organizer,attendees,responseStatus,type";
    let mut url = format!(
        "{BASE}/me/calendarView?startDateTime={}Z&endDateTime={}Z&$top=250&$select={select}",
        utc_string(from),
        utc_string(to)
    );
    let mut events = Vec::new();
    loop {
        let v = read(
            agent
                .get(&url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Prefer", "outlook.timezone=\"UTC\", outlook.body-content-type=\"text\"")
                .call(),
        )?;
        events.extend(v["value"].as_array().into_iter().flatten().filter_map(|e| parse_event(e, account)));
        match v["@odata.nextLink"].as_str() {
            Some(next) => url = next.to_string(),
            None => break,
        }
    }
    Ok(events)
}

/// Graph JSON for creating or updating an event.
fn event_json(e: &Event) -> Value {
    let time = |ts: i64| {
        if e.all_day {
            // All-day events must start and end at midnight in the event's zone.
            let date: NaiveDate = calendar::local_date(ts);
            let zone = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into());
            json!({ "dateTime": format!("{}T00:00:00", date.format("%Y-%m-%d")), "timeZone": zone })
        } else {
            json!({ "dateTime": utc_string(ts), "timeZone": "UTC" })
        }
    };
    let attendees: Vec<Value> =
        e.attendees.iter().map(|a| json!({ "emailAddress": { "address": a }, "type": "required" })).collect();
    json!({
        "subject": e.subject,
        "body": { "contentType": "text", "content": e.body },
        "start": time(e.start),
        "end": time(e.end),
        "isAllDay": e.all_day,
        "location": { "displayName": e.location },
        "attendees": attendees,
    })
}

/// Create an event. Graph sends invitations to attendees itself.
pub fn create(token: &str, e: &Event) -> Result<(), String> {
    read(agent().post(&format!("{BASE}/me/events")).header("Authorization", format!("Bearer {token}")).send_json(event_json(e)))
        .map(|_| ())
}

pub fn update(token: &str, e: &Event) -> Result<(), String> {
    read(
        agent()
            .patch(&format!("{BASE}/me/events/{}", e.id))
            .header("Authorization", format!("Bearer {token}"))
            .send_json(event_json(e)),
    )
    .map(|_| ())
}

/// Delete an event. For a meeting you organize, Graph sends the cancellation.
pub fn delete(token: &str, id: &str) -> Result<(), String> {
    read(agent().delete(&format!("{BASE}/me/events/{id}")).header("Authorization", format!("Bearer {token}")).call()).map(|_| ())
}

/// Accept, tentatively accept or decline, notifying the organizer.
pub fn respond(token: &str, id: &str, response: Response) -> Result<(), String> {
    let action = match response {
        Response::Accepted => "accept",
        Response::Tentative => "tentativelyAccept",
        Response::Declined => "decline",
        _ => return Ok(()),
    };
    read(
        agent()
            .post(&format!("{BASE}/me/events/{id}/{action}"))
            .header("Authorization", format!("Bearer {token}"))
            .send_json(json!({ "sendResponse": true })),
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hits the real Graph endpoint; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn graph_reports_auth_errors() {
        let err = calendar_view("not-a-token", "a", 0, 86_400).unwrap_err();
        assert!(err.starts_with("Microsoft Graph error 401"), "{err}");
    }

    #[test]
    fn parses_graph_events() {
        let v: Value = serde_json::from_str(
            r#"{"id":"AAMk1","iCalUId":"040000008200","subject":"Standup","isAllDay":false,"isCancelled":false,
                "type":"occurrence","bodyPreview":"Daily",
                "start":{"dateTime":"2026-10-14T16:00:00.0000000","timeZone":"UTC"},
                "end":{"dateTime":"2026-10-14T16:15:00.0000000","timeZone":"UTC"},
                "location":{"displayName":"Teams"},
                "organizer":{"emailAddress":{"name":"Megan","address":"megan@contoso.example"}},
                "attendees":[{"emailAddress":{"address":"alex@contoso.example"}}],
                "responseStatus":{"response":"tentativelyAccepted"}}"#,
        )
        .unwrap();
        let e = parse_event(&v, "acct-1").unwrap();
        assert_eq!(e.start, Utc.with_ymd_and_hms(2026, 10, 14, 16, 0, 0).unwrap().timestamp());
        assert_eq!(e.end - e.start, 900);
        assert_eq!((e.response, e.recurring, e.calendar.as_str()), (Response::Tentative, true, "acct-1"));
        assert_eq!(e.attendees, vec!["alex@contoso.example".to_string()]);

        let mut all_day = v.clone();
        all_day["isAllDay"] = Value::Bool(true);
        all_day["start"]["dateTime"] = "2026-10-20T00:00:00.0000000".into();
        all_day["end"]["dateTime"] = "2026-10-21T00:00:00.0000000".into();
        let e = parse_event(&all_day, "a").unwrap();
        assert_eq!(calendar::local_date(e.start), NaiveDate::from_ymd_opt(2026, 10, 20).unwrap());
        assert_eq!(e.end - e.start, calendar::day_bounds(calendar::local_date(e.start)).1 - e.start);

        let mut cancelled = v;
        cancelled["isCancelled"] = Value::Bool(true);
        assert!(parse_event(&cancelled, "a").is_none());
    }
}
