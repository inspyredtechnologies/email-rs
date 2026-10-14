//! Calendar events: the model shared by the local calendar, Microsoft Graph
//! calendars and meeting invites, plus storage and day-view layout helpers.

use chrono::{Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};

/// Calendar id of the calendar stored on this PC; other calendars use the account id.
pub const LOCAL: &str = "local";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    /// Not a meeting, or no response recorded.
    #[default]
    None,
    Organizer,
    Accepted,
    Tentative,
    Declined,
    NotResponded,
}

impl Response {
    /// iCalendar PARTSTAT value.
    pub fn partstat(self) -> &'static str {
        match self {
            Response::Accepted | Response::Organizer => "ACCEPTED",
            Response::Tentative => "TENTATIVE",
            Response::Declined => "DECLINED",
            _ => "NEEDS-ACTION",
        }
    }

    /// Subject prefix Outlook uses for responses.
    pub fn verb(self) -> &'static str {
        match self {
            Response::Accepted => "Accepted",
            Response::Tentative => "Tentative",
            Response::Declined => "Declined",
            _ => "",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Event {
    /// Local id, or the Microsoft Graph event id.
    pub id: String,
    /// `LOCAL` or the account id it syncs with.
    pub calendar: String,
    /// iCalendar UID (Graph: iCalUId), used to match invites to events.
    pub uid: String,
    pub subject: String,
    pub location: String,
    pub body: String,
    /// UTC timestamps; all-day events run from local midnight to local midnight.
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub organizer_name: String,
    pub organizer: String,
    /// Attendee email addresses.
    pub attendees: Vec<String>,
    pub response: Response,
    pub sequence: i64,
    pub recurring: bool,
}

impl Event {
    pub fn is_meeting(&self) -> bool {
        !self.attendees.is_empty() || self.response != Response::None
    }

    /// Whether this event shows on (or overlaps) the local `day`.
    pub fn on_day(&self, day: NaiveDate) -> bool {
        let (s, e) = day_bounds(day);
        self.start < e && (self.end > s || (self.end == self.start && self.start >= s))
    }
}

/// Short random-enough id for new local events and iCalendar UIDs.
pub fn new_id() -> String {
    let mut buf = [0u8; 12];
    let _ = getrandom::fill(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// UTC timestamps of local midnight at the start and end of `day`.
pub fn day_bounds(day: NaiveDate) -> (i64, i64) {
    let at = |d: NaiveDate| Local.from_local_datetime(&d.and_time(NaiveTime::MIN)).earliest().map(|t| t.timestamp()).unwrap_or(0);
    (at(day), at(day + Duration::days(1)))
}

/// UTC timestamp of a local date + minutes after midnight.
pub fn local_ts(day: NaiveDate, minutes: u32) -> i64 {
    let t = day.and_time(NaiveTime::MIN) + Duration::minutes(minutes as i64);
    Local.from_local_datetime(&t).earliest().map(|d| d.timestamp()).unwrap_or(0)
}

pub fn local_date(ts: i64) -> NaiveDate {
    Local.timestamp_opt(ts, 0).single().map(|d| d.date_naive()).unwrap_or_default()
}

/// Minutes after local midnight.
pub fn local_minutes(ts: i64) -> u32 {
    use chrono::Timelike;
    Local.timestamp_opt(ts, 0).single().map(|d| d.hour() * 60 + d.minute()).unwrap_or(0)
}

/// Sunday of the week containing `day` (US Outlook default).
pub fn week_start(day: NaiveDate) -> NaiveDate {
    day - Duration::days(day.weekday().num_days_from_sunday() as i64)
}

pub fn month_start(day: NaiveDate) -> NaiveDate {
    day.with_day(1).unwrap_or(day)
}

/// For events on one day (start, end), assign each a lane so overlapping
/// events sit side by side. Returns (lane, lane count of its overlap group).
pub fn lanes(spans: &[(i64, i64)]) -> Vec<(usize, usize)> {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&i| (spans[i].0, -(spans[i].1)));
    let mut out = vec![(0, 1); spans.len()];
    let mut group: Vec<usize> = Vec::new();
    let mut lane_ends: Vec<i64> = Vec::new();
    let mut group_end = i64::MIN;
    let flush = |group: &mut Vec<usize>, lane_ends: &mut Vec<i64>, out: &mut Vec<(usize, usize)>| {
        for &g in group.iter() {
            out[g].1 = lane_ends.len();
        }
        group.clear();
        lane_ends.clear();
    };
    for i in order {
        let (s, e) = spans[i];
        let e = e.max(s + 1);
        if s >= group_end {
            flush(&mut group, &mut lane_ends, &mut out);
        }
        let lane = match lane_ends.iter().position(|&end| end <= s) {
            Some(l) => l,
            None => {
                lane_ends.push(0);
                lane_ends.len() - 1
            }
        };
        lane_ends[lane] = e;
        out[i].0 = lane;
        group.push(i);
        group_end = group_end.max(e);
    }
    flush(&mut group, &mut lane_ends, &mut out);
    out
}

// ---------------------------------------------------------------- storage

fn path(calendar: &str) -> std::path::PathBuf {
    crate::store::data_dir().join("calendar").join(format!("{calendar}.json"))
}

pub fn load(calendar: &str) -> Vec<Event> {
    std::fs::read(path(calendar)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save(calendar: &str, events: &[Event]) {
    let p = path(calendar);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec(events) {
        let tmp = p.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(tmp, p);
        }
    }
}

pub fn forget(calendar: &str) {
    let _ = std::fs::remove_file(path(calendar));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lanes_split_overlaps_only() {
        // 9-10 and 9:30-11 overlap; 11-12 is on its own.
        let h = 3600;
        let out = lanes(&[(9 * h, 10 * h), (9 * h + 1800, 11 * h), (11 * h, 12 * h)]);
        assert_eq!(out, vec![(0, 2), (1, 2), (0, 1)]);
        // A lane frees up once its event ends: three events, two lanes.
        let out = lanes(&[(0, 2 * h), (0, h), (h, 2 * h)]);
        assert_eq!(out, vec![(0, 2), (1, 2), (1, 2)]);
    }

    #[test]
    fn week_and_day_helpers() {
        let tue = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
        assert_eq!(week_start(tue), NaiveDate::from_ymd_opt(2026, 10, 11).unwrap());
        assert_eq!(month_start(tue).day(), 1);
        let ts = local_ts(tue, 9 * 60 + 30);
        assert_eq!((local_date(ts), local_minutes(ts)), (tue, 570));
        let ev = Event { start: ts, end: ts + 3600, ..Default::default() };
        assert!(ev.on_day(tue) && !ev.on_day(tue.succ_opt().unwrap()));
    }
}
