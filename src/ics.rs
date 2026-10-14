//! iCalendar (RFC 5545) for meeting invites: parsing incoming REQUEST/CANCEL
//! messages and writing REQUEST/REPLY/CANCEL (iMIP, RFC 6047).
//!
//! Times are resolved to UTC. TZID times use the message's own VTIMEZONE rules
//! (which Outlook and Exchange always include), falling back to IANA zone names,
//! then to this PC's local time.

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc, Weekday};

#[derive(Clone, Debug, Default)]
struct Prop {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Prop {
    fn param(&self, key: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug, Default)]
struct Component {
    name: String,
    props: Vec<Prop>,
    children: Vec<Component>,
}

impl Component {
    fn prop(&self, name: &str) -> Option<&Prop> {
        self.props.iter().find(|p| p.name == name)
    }
    fn text(&self, name: &str) -> String {
        self.prop(name).map(|p| unescape(&p.value)).unwrap_or_default()
    }
}

/// Join folded lines (continuations start with a space or tab).
fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let (Some(rest), Some(last)) = (line.strip_prefix([' ', '\t']), lines.last_mut()) {
            last.push_str(rest);
        } else if !line.is_empty() {
            lines.push(line.to_string());
        }
    }
    lines
}

/// `NAME;P1=v;P2="a:b":value` → Prop. Quoted parameter values may contain `:` and `;`.
fn parse_line(line: &str) -> Option<Prop> {
    let mut in_quotes = false;
    let mut split = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                split = Some(i);
                break;
            }
            _ => {}
        }
    }
    let (head, value) = line.split_at(split?);
    let mut parts = Vec::new();
    let (mut cur, mut q) = (String::new(), false);
    for c in head.chars() {
        match c {
            '"' => q = !q,
            ';' if !q => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    let name = parts.remove(0).to_ascii_uppercase();
    let params = parts
        .into_iter()
        .filter_map(|p| p.split_once('=').map(|(k, v)| (k.to_ascii_uppercase(), v.to_string())))
        .collect();
    Some(Prop { name, params, value: value[1..].to_string() })
}

fn parse_components(text: &str) -> Option<Component> {
    let mut stack: Vec<Component> = vec![Component { name: "ROOT".into(), ..Default::default() }];
    for line in unfold(text) {
        let Some(p) = parse_line(&line) else { continue };
        match p.name.as_str() {
            "BEGIN" => stack.push(Component { name: p.value.to_ascii_uppercase(), ..Default::default() }),
            "END" if stack.len() > 1 => {
                let done = stack.pop().unwrap();
                stack.last_mut().unwrap().children.push(done);
            }
            _ => stack.last_mut().unwrap().props.push(p),
        }
    }
    stack.into_iter().next()?.children.into_iter().find(|c| c.name == "VCALENDAR")
}

pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace("\r\n", "\\n").replace('\n', "\\n")
}

// ------------------------------------------------------------------ times

/// One STANDARD or DAYLIGHT block of a VTIMEZONE.
struct Observance {
    start: NaiveDateTime,
    offset_to: i64,
    /// Yearly rule: month and (nth, weekday); nth = -1 means last.
    rule: Option<(u32, i32, Weekday)>,
}

fn parse_offset(s: &str) -> Option<i64> {
    let sign = if s.starts_with('-') { -1 } else { 1 };
    let digits = s.trim_start_matches(['+', '-']);
    let h: i64 = digits.get(0..2)?.parse().ok()?;
    let m: i64 = digits.get(2..4)?.parse().ok()?;
    Some(sign * (h * 3600 + m * 60))
}

fn parse_weekday(s: &str) -> Option<Weekday> {
    Some(match s {
        "SU" => Weekday::Sun,
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        _ => return None,
    })
}

/// The nth (or last when n < 0) `weekday` of a month.
fn nth_weekday(year: i32, month: u32, n: i32, weekday: Weekday) -> Option<NaiveDate> {
    if n > 0 {
        NaiveDate::from_weekday_of_month_opt(year, month, weekday, n as u8)
    } else {
        let next = if month == 12 { NaiveDate::from_ymd_opt(year + 1, 1, 1) } else { NaiveDate::from_ymd_opt(year, month + 1, 1) }?;
        let mut d = next.pred_opt()?;
        while d.weekday() != weekday {
            d = d.pred_opt()?;
        }
        Some(d)
    }
}

fn observances(tz: &Component) -> Vec<Observance> {
    tz.children
        .iter()
        .filter(|c| c.name == "STANDARD" || c.name == "DAYLIGHT")
        .filter_map(|c| {
            let start = NaiveDateTime::parse_from_str(&c.prop("DTSTART")?.value, "%Y%m%dT%H%M%S").ok()?;
            let offset_to = parse_offset(&c.prop("TZOFFSETTO")?.value)?;
            let rule = c.prop("RRULE").and_then(|r| {
                let get = |k: &str| r.value.split(';').find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(str::to_string));
                let month: u32 = get("BYMONTH")?.parse().ok()?;
                let byday = get("BYDAY")?;
                let (n, wd) = byday.split_at(byday.len() - 2);
                let n: i32 = if n.is_empty() { 1 } else { n.parse().ok()? };
                Some((month, n, parse_weekday(wd)?))
            });
            Some(Observance { start, offset_to, rule })
        })
        .collect()
}

/// UTC offset in effect at local time `t` according to VTIMEZONE observances.
fn vtimezone_offset(obs: &[Observance], t: NaiveDateTime) -> Option<i64> {
    let onset = |o: &Observance, year: i32| -> Option<NaiveDateTime> {
        match o.rule {
            Some((month, n, wd)) => Some(nth_weekday(year, month, n, wd)?.and_time(o.start.time())),
            None => Some(o.start),
        }
    };
    obs.iter()
        .filter_map(|o| {
            // The latest onset of this observance at or before t.
            [t.year(), t.year() - 1]
                .iter()
                .filter_map(|y| onset(o, *y))
                .filter(|d| *d <= t && *d >= o.start)
                .max()
                .map(|d| (d, o.offset_to))
        })
        .max_by_key(|(d, _)| *d)
        .map(|(_, off)| off)
        .or_else(|| obs.first().map(|o| o.offset_to))
}

/// Resolve a DTSTART/DTEND property to (UTC timestamp, is_date_only).
fn resolve_time(p: &Prop, cal: &Component) -> Option<(i64, bool)> {
    let v = p.value.trim();
    if p.param("VALUE").is_some_and(|x| x.eq_ignore_ascii_case("DATE")) || v.len() == 8 {
        let d = NaiveDate::parse_from_str(v, "%Y%m%d").ok()?;
        let local = Local.from_local_datetime(&d.and_time(NaiveTime::MIN)).earliest()?;
        return Some((local.timestamp(), true));
    }
    if let Some(utc) = v.strip_suffix('Z') {
        let t = NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S").ok()?;
        return Some((t.and_utc().timestamp(), false));
    }
    let t = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()?;
    if let Some(tzid) = p.param("TZID") {
        let tzid = tzid.trim_matches('"');
        let vtz = cal.children.iter().find(|c| c.name == "VTIMEZONE" && c.prop("TZID").is_some_and(|z| z.value == tzid));
        if let Some(offset) = vtz.and_then(|z| vtimezone_offset(&observances(z), t)) {
            return Some((t.and_utc().timestamp() - offset, false));
        }
        if let Ok(zone) = tzid.parse::<chrono_tz::Tz>() {
            return zone.from_local_datetime(&t).earliest().map(|d| (d.timestamp(), false));
        }
    }
    // Floating time (or an unknown zone): this PC's local time.
    Local.from_local_datetime(&t).earliest().map(|d| (d.timestamp(), false))
}

/// ISO 8601 duration like `PT1H30M` or `P1D` in seconds.
fn parse_duration(s: &str) -> Option<i64> {
    let (sign, s) = match s.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, s.trim_start_matches('+')),
    };
    let s = s.strip_prefix('P')?;
    let (mut total, mut num, mut in_time) = (0i64, String::new(), false);
    for c in s.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            unit => {
                let n: i64 = std::mem::take(&mut num).parse().ok()?;
                total += n * match (unit, in_time) {
                    ('W', _) => 604_800,
                    ('D', _) => 86_400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
            }
        }
    }
    Some(sign * total)
}

// --------------------------------------------------------------- invites

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Request,
    Cancel,
    Reply,
    Publish,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    pub name: String,
    pub email: String,
}

impl Person {
    fn from_prop(p: &Prop) -> Person {
        let email = p.value.trim();
        let email = email.strip_prefix("mailto:").or_else(|| email.strip_prefix("MAILTO:")).unwrap_or(email);
        Person { name: p.param("CN").unwrap_or("").trim_matches('"').to_string(), email: email.to_string() }
    }

    pub fn label(&self) -> String {
        if self.name.is_empty() { self.email.clone() } else { format!("{} <{}>", self.name, self.email) }
    }
}

#[derive(Clone, Debug)]
pub struct Invite {
    pub method: Method,
    pub uid: String,
    pub sequence: i64,
    pub summary: String,
    pub location: String,
    pub description: String,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub organizer: Option<Person>,
    pub attendees: Vec<Person>,
    pub recurring: bool,
}

/// Parse the first VEVENT of an iCalendar object.
pub fn parse_invite(text: &str) -> Option<Invite> {
    let cal = parse_components(text)?;
    let method = match cal.prop("METHOD").map(|m| m.value.to_ascii_uppercase()).as_deref() {
        Some("REQUEST") => Method::Request,
        Some("CANCEL") => Method::Cancel,
        Some("REPLY") => Method::Reply,
        Some("PUBLISH") | None => Method::Publish,
        Some(_) => Method::Other,
    };
    let ev = cal.children.iter().find(|c| c.name == "VEVENT")?;
    let (start, all_day) = resolve_time(ev.prop("DTSTART")?, &cal)?;
    let end = match (ev.prop("DTEND"), ev.prop("DURATION")) {
        (Some(p), _) => resolve_time(p, &cal)?.0,
        (None, Some(d)) => start + parse_duration(&d.value)?,
        (None, None) => start + if all_day { 86_400 } else { 0 },
    };
    Some(Invite {
        method,
        uid: ev.text("UID"),
        sequence: ev.prop("SEQUENCE").and_then(|p| p.value.trim().parse().ok()).unwrap_or(0),
        summary: ev.text("SUMMARY"),
        location: ev.text("LOCATION"),
        description: ev.text("DESCRIPTION"),
        start,
        end: end.max(start),
        all_day,
        organizer: ev.prop("ORGANIZER").map(Person::from_prop),
        attendees: ev.props.iter().filter(|p| p.name == "ATTENDEE").map(Person::from_prop).collect(),
        recurring: ev.prop("RRULE").is_some(),
    })
}

// ----------------------------------------------------------------- writing

fn utc_stamp(ts: i64) -> String {
    Utc.timestamp_opt(ts, 0).single().map(|d| d.format("%Y%m%dT%H%M%SZ").to_string()).unwrap_or_default()
}

fn date_stamp(ts: i64) -> String {
    Local.timestamp_opt(ts, 0).single().map(|d| d.format("%Y%m%d").to_string()).unwrap_or_default()
}

/// Fold to 75-octet lines (RFC 5545 §3.1), splitting only on char boundaries.
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut len = 0;
    for c in line.chars() {
        if len + c.len_utf8() > 75 {
            out.push_str("\r\n ");
            len = 1;
        }
        out.push(c);
        len += c.len_utf8();
    }
    out.push_str("\r\n");
    out
}

pub struct EventSpec<'a> {
    pub uid: &'a str,
    pub sequence: i64,
    pub summary: &'a str,
    pub location: &'a str,
    pub description: &'a str,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub organizer: &'a Person,
    /// Attendees with their PARTSTAT (e.g. NEEDS-ACTION, ACCEPTED).
    pub attendees: Vec<(&'a Person, &'a str)>,
}

/// A VCALENDAR with one VEVENT, for METHOD REQUEST, CANCEL or REPLY.
pub fn write(method: &str, e: &EventSpec) -> String {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "PRODID:-//Outlook RS//EN".into(),
        "VERSION:2.0".into(),
        format!("METHOD:{method}"),
        "BEGIN:VEVENT".into(),
        format!("UID:{}", e.uid),
        format!("SEQUENCE:{}", e.sequence),
        format!("DTSTAMP:{}", utc_stamp(Utc::now().timestamp())),
    ];
    if e.all_day {
        lines.push(format!("DTSTART;VALUE=DATE:{}", date_stamp(e.start)));
        lines.push(format!("DTEND;VALUE=DATE:{}", date_stamp(e.end)));
    } else {
        lines.push(format!("DTSTART:{}", utc_stamp(e.start)));
        lines.push(format!("DTEND:{}", utc_stamp(e.end)));
    }
    lines.push(format!("SUMMARY:{}", escape(e.summary)));
    if !e.location.is_empty() {
        lines.push(format!("LOCATION:{}", escape(e.location)));
    }
    if !e.description.is_empty() && method != "REPLY" {
        lines.push(format!("DESCRIPTION:{}", escape(e.description)));
    }
    let cn = |p: &Person| if p.name.is_empty() { String::new() } else { format!(";CN=\"{}\"", p.name.replace('"', "'")) };
    lines.push(format!("ORGANIZER{}:mailto:{}", cn(e.organizer), e.organizer.email));
    for (p, partstat) in &e.attendees {
        let rsvp = if method == "REQUEST" { ";RSVP=TRUE" } else { "" };
        lines.push(format!("ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT={partstat}{rsvp}{}:mailto:{}", cn(p), p.email));
    }
    if method == "CANCEL" {
        lines.push("STATUS:CANCELLED".into());
    }
    lines.push("END:VEVENT".into());
    lines.push("END:VCALENDAR".into());
    lines.iter().map(|l| fold(l)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed-down invite as Outlook sends it, with a Pacific VTIMEZONE.
    const OUTLOOK_INVITE: &str = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nPRODID:Microsoft Exchange Server 2010\r\nVERSION:2.0\r\n\
BEGIN:VTIMEZONE\r\nTZID:Pacific Standard Time\r\n\
BEGIN:STANDARD\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:-0700\r\nTZOFFSETTO:-0800\r\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=11\r\nEND:STANDARD\r\n\
BEGIN:DAYLIGHT\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:-0800\r\nTZOFFSETTO:-0700\r\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=2SU;BYMONTH=3\r\nEND:DAYLIGHT\r\n\
END:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nORGANIZER;CN=Megan Bowen:mailto:megan@contoso.example\r\n\
ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;CN=Alex Wilber:mailto:alex@contoso.example\r\n\
DESCRIPTION;LANGUAGE=en-US:Let's review the\\nQ4 designs\\, please come prepared.\r\n\
UID:040000008200E00074C5B7101A82E008\r\nSUMMARY;LANGUAGE=en-US:Design review\r\n\
DTSTART;TZID=Pacific Standard Time:20261015T100000\r\nDTEND;TZID=Pacific Standard Time:20261015T110000\r\n\
SEQUENCE:2\r\nLOCATION;LANGUAGE=en-US:Conf Room 4 / Teams\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn parses_outlook_invite_with_vtimezone() {
        let inv = parse_invite(OUTLOOK_INVITE).unwrap();
        assert_eq!(inv.method, Method::Request);
        assert_eq!(inv.summary, "Design review");
        assert_eq!(inv.location, "Conf Room 4 / Teams");
        assert_eq!(inv.description, "Let's review the\nQ4 designs, please come prepared.");
        assert_eq!(inv.sequence, 2);
        assert_eq!(inv.organizer.as_ref().unwrap().email, "megan@contoso.example");
        assert_eq!(inv.attendees[0].name, "Alex Wilber");
        // October 15 is in Pacific daylight time (UTC-7): 10:00 PDT = 17:00 UTC.
        assert_eq!(inv.start, Utc.with_ymd_and_hms(2026, 10, 15, 17, 0, 0).unwrap().timestamp());
        assert_eq!(inv.end - inv.start, 3600);
        // A December time falls in standard time (UTC-8).
        let winter = OUTLOOK_INVITE.replace("20261015T100000", "20261210T100000").replace("20261015T110000", "20261210T110000");
        assert_eq!(parse_invite(&winter).unwrap().start, Utc.with_ymd_and_hms(2026, 12, 10, 18, 0, 0).unwrap().timestamp());
    }

    #[test]
    fn handles_iana_zones_durations_and_folding() {
        let text = "BEGIN:VCALENDAR\nMETHOD:CANCEL\nBEGIN:VEVENT\nUID:x\nSUMMARY:Long\n  title\n\
DTSTART;TZID=Europe/Berlin:20260115T090000\nDURATION:PT1H30M\nEND:VEVENT\nEND:VCALENDAR\n";
        let inv = parse_invite(text).unwrap();
        assert_eq!(inv.method, Method::Cancel);
        assert_eq!(inv.summary, "Long title");
        assert_eq!(inv.start, Utc.with_ymd_and_hms(2026, 1, 15, 8, 0, 0).unwrap().timestamp());
        assert_eq!(inv.end - inv.start, 5400);
        assert_eq!(parse_duration("P1W2DT3H"), Some(604_800 + 2 * 86_400 + 3 * 3600));
    }

    #[test]
    fn writes_reply_that_parses_back() {
        let me = Person { name: "Alex Wilber".into(), email: "alex@contoso.example".into() };
        let org = Person { name: "Megan Bowen".into(), email: "megan@contoso.example".into() };
        let start = Utc.with_ymd_and_hms(2026, 10, 15, 17, 0, 0).unwrap().timestamp();
        let spec = EventSpec {
            uid: "u1",
            sequence: 2,
            summary: "Design review; Q4, final",
            location: "Room 4",
            description: "",
            start,
            end: start + 3600,
            all_day: false,
            organizer: &org,
            attendees: vec![(&me, "ACCEPTED")],
        };
        let ics = write("REPLY", &spec);
        assert!(ics.contains("METHOD:REPLY\r\n"));
        assert!(ics.contains("DTSTART:20261015T170000Z\r\n"));
        // Long lines are folded; check the logical (unfolded) line.
        assert!(unfold(&ics).iter().any(|l| l.ends_with("PARTSTAT=ACCEPTED;CN=\"Alex Wilber\":mailto:alex@contoso.example")));
        assert!(ics.lines().all(|l| l.len() <= 75));
        let back = parse_invite(&ics).unwrap();
        assert_eq!((back.method, back.summary.as_str(), back.start), (Method::Reply, "Design review; Q4, final", start));
    }
}
