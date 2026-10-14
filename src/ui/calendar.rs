//! Calendar module: date navigator and calendar list in the left pane, and
//! Day / Work Week / Week / Month views in the main area.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;

use chrono::{Datelike, Duration, Local, NaiveDate};
use eframe::egui::{self, Align2, Color32, FontId, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

use crate::app::{Action, App};
use crate::calendar::{self, Event, LOCAL, Response};
use crate::theme::{self, BLUE, BORDER, SUBTLE, icon};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CalView {
    Day,
    WorkWeek,
    Week,
    Month,
}

pub struct CalendarState {
    pub view: CalView,
    /// The selected date; the visible range is derived from it.
    pub date: NaiveDate,
    /// First day of the month shown in the date navigator.
    pub nav_month: NaiveDate,
    pub local: Vec<Event>,
    /// Server calendars (Microsoft Graph / Exchange), by account id.
    pub remote: HashMap<String, Vec<Event>>,
    /// Calendars unchecked in "My Calendars".
    pub hidden: HashSet<String>,
    pub selected: Option<(String, String)>,
    /// Selected empty time slot: (date, minutes after midnight).
    pub slot: Option<(NaiveDate, u32)>,
    pub needs_connect: HashSet<String>,
    pub connecting: Option<(String, Receiver<Result<(), String>>)>,
    pub errors: HashMap<String, String>,
    /// Range last requested from each server calendar.
    pub synced: HashMap<String, (i64, i64)>,
    pub editors: Vec<super::appointment::Editor>,
    pub next_editor: u64,
    pub scroll_to_morning: bool,
}

impl CalendarState {
    pub fn new() -> Self {
        let today = Local::now().date_naive();
        Self {
            view: CalView::WorkWeek,
            date: today,
            nav_month: calendar::month_start(today),
            local: Vec::new(),
            remote: HashMap::new(),
            hidden: HashSet::new(),
            selected: None,
            slot: None,
            needs_connect: HashSet::new(),
            connecting: None,
            errors: HashMap::new(),
            synced: HashMap::new(),
            editors: Vec::new(),
            next_editor: 1,
            scroll_to_morning: true,
        }
    }

    /// Days shown by the current view.
    pub fn days(&self) -> Vec<NaiveDate> {
        let (first, n) = match self.view {
            CalView::Day => (self.date, 1),
            CalView::WorkWeek => (calendar::week_start(self.date) + Duration::days(1), 5),
            CalView::Week => (calendar::week_start(self.date), 7),
            CalView::Month => (calendar::week_start(calendar::month_start(self.date)), 42),
        };
        (0..n).map(|i| first + Duration::days(i)).collect()
    }

    pub fn step(&mut self, forward: bool) {
        let sign = if forward { 1 } else { -1 };
        self.date = match self.view {
            CalView::Day => self.date + Duration::days(sign),
            CalView::WorkWeek | CalView::Week => self.date + Duration::days(7 * sign),
            CalView::Month => {
                let m = calendar::month_start(self.date);
                if forward { (m + Duration::days(32)).with_day(1).unwrap() } else { (m - Duration::days(1)).with_day(1).unwrap() }
            }
        };
        self.nav_month = calendar::month_start(self.date);
    }

    pub fn go_to(&mut self, date: NaiveDate) {
        self.date = date;
        self.nav_month = calendar::month_start(date);
    }

    pub fn find(&self, cal: &str, id: &str) -> Option<&Event> {
        let list = if cal == LOCAL { Some(&self.local) } else { self.remote.get(cal) };
        list?.iter().find(|e| e.id == id)
    }

    /// Event matching an iCalendar UID in any calendar (Microsoft first).
    pub fn find_uid(&self, uid: &str) -> Option<&Event> {
        self.remote.values().flatten().chain(&self.local).find(|e| !uid.is_empty() && e.uid == uid)
    }
}

/// (calendar id, label, color) for "My Calendars": the local one plus each account with a server calendar.
pub fn calendars(app: &App) -> Vec<(String, String, Color32)> {
    let mut out = vec![(LOCAL.to_string(), "Calendar".to_string(), PALETTE[0])];
    for (i, a) in app.accounts.iter().filter(|a| a.config.has_server_calendar()).enumerate() {
        out.push((a.config.id.clone(), format!("Calendar - {}", a.config.email), PALETTE[(i + 1) % PALETTE.len()]));
    }
    out
}

const PALETTE: [Color32; 4] = [
    Color32::from_rgb(0x00, 0x78, 0xD4),
    Color32::from_rgb(0x10, 0x7C, 0x10),
    Color32::from_rgb(0x88, 0x17, 0x98),
    Color32::from_rgb(0xCA, 0x50, 0x10),
];

fn light(c: Color32) -> Color32 {
    let mix = |x: u8| (x as u16 + (255 - x as u16) * 4 / 5) as u8;
    Color32::from_rgb(mix(c.r()), mix(c.g()), mix(c.b()))
}

/// Visible events with their calendar color, declined meetings left out.
fn visible(app: &App) -> Vec<(Event, Color32)> {
    let cals = calendars(app);
    let color_of = |id: &str| cals.iter().find(|c| c.0 == id).map(|c| c.2).unwrap_or(PALETTE[0]);
    let cal = &app.cal;
    cal.local
        .iter()
        .chain(cal.remote.iter().filter(|(id, _)| cals.iter().any(|c| &c.0 == *id)).flat_map(|(_, v)| v))
        .filter(|e| !cal.hidden.contains(&e.calendar) && e.response != Response::Declined)
        .map(|e| (e.clone(), color_of(&e.calendar)))
        .collect()
}

/// Number of visible events in the current view's date range (for the status bar).
pub fn visible_count(app: &App) -> usize {
    let days = app.cal.days();
    let (from, to) = (calendar::day_bounds(days[0]).0, calendar::day_bounds(*days.last().unwrap()).1);
    visible(app).iter().filter(|(e, _)| e.start < to && e.end > from).count()
}

fn time_label(minutes: u32) -> String {
    let (h, m) = (minutes / 60, minutes % 60);
    let h12 = if h % 12 == 0 { 12 } else { h % 12 };
    let ampm = if h < 12 { "AM" } else { "PM" };
    if m == 0 { format!("{h12} {ampm}") } else { format!("{h12}:{m:02} {ampm}") }
}

pub fn time_of(minutes: u32) -> String {
    let (h, m) = (minutes / 60, minutes % 60);
    let h12 = if h % 12 == 0 { 12 } else { h % 12 };
    format!("{h12}:{m:02} {}", if h < 12 { "AM" } else { "PM" })
}

// ------------------------------------------------------------ month grid

/// A small month calendar. Highlights `range`, marks today, returns a clicked date.
pub fn month_grid(ui: &mut Ui, month: NaiveDate, range: (NaiveDate, NaiveDate), selected: Option<NaiveDate>) -> Option<NaiveDate> {
    let today = Local::now().date_naive();
    let (cw, ch) = (28.0, 22.0);
    let mut clicked = None;
    let (rect, _) = ui.allocate_exact_size(vec2(cw * 7.0, ch * 7.0), Sense::hover());
    let p = ui.painter();
    for (i, d) in ["S", "M", "T", "W", "T", "F", "S"].iter().enumerate() {
        p.text(pos2(rect.left() + cw * (i as f32 + 0.5), rect.top() + ch / 2.0), Align2::CENTER_CENTER, d, FontId::proportional(11.5), SUBTLE);
    }
    let first = calendar::week_start(month);
    for i in 0..42 {
        let day = first + Duration::days(i);
        let cell = Rect::from_min_size(pos2(rect.left() + cw * (i % 7) as f32, rect.top() + ch * (1 + i / 7) as f32), vec2(cw, ch));
        let resp = ui.interact(cell, ui.id().with(("mg", month, i)), Sense::click());
        let p = ui.painter();
        if day >= range.0 && day <= range.1 {
            p.rect_filled(cell.shrink(1.0), 0.0, theme::SELECTED);
        }
        if resp.hovered() {
            p.rect_stroke(cell.shrink(1.0), 0.0, Stroke::new(1.0, BLUE), egui::StrokeKind::Inside);
        }
        let mut color = if day.month() == month.month() { theme::TEXT } else { Color32::from_gray(160) };
        if day == today {
            p.rect_filled(cell.shrink(2.0), 0.0, BLUE);
            color = Color32::WHITE;
        } else if Some(day) == selected {
            p.rect_stroke(cell.shrink(1.0), 0.0, Stroke::new(1.5, BLUE), egui::StrokeKind::Inside);
        }
        p.text(cell.center(), Align2::CENTER_CENTER, day.day().to_string(), FontId::proportional(12.5), color);
        if resp.clicked() {
            clicked = Some(day);
        }
    }
    clicked
}

/// Month name with ‹ › arrows; returns the new month when changed.
pub fn month_header(ui: &mut Ui, month: NaiveDate) -> NaiveDate {
    let mut out = month;
    ui.horizontal(|ui| {
        if ui.small_button("\u{2039}").clicked() {
            out = (month - Duration::days(1)).with_day(1).unwrap();
        }
        ui.add_sized([150.0, 20.0], egui::Label::new(theme::bold(month.format("%B %Y").to_string(), 13.0)));
        if ui.small_button("\u{203A}").clicked() {
            out = (month + Duration::days(32)).with_day(1).unwrap();
        }
    });
    out
}

// -------------------------------------------------------------- left pane

pub fn nav_pane(app: &mut App, ui: &mut Ui) {
    ui.add_space(4.0);
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 0)).show(ui, |ui| {
        app.cal.nav_month = month_header(ui, app.cal.nav_month);
        let days = app.cal.days();
        let range = (days[0], *days.last().unwrap());
        if let Some(d) = month_grid(ui, app.cal.nav_month, range, Some(app.cal.date)) {
            app.cal.date = d;
        }
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.label(theme::icon(icon::CHEVRON_DOWN, 9.0).color(SUBTLE));
            ui.label(theme::bold("My Calendars", 13.5));
        });
        for (id, label, color) in calendars(app) {
            ui.horizontal(|ui| {
                let mut shown = !app.cal.hidden.contains(&id);
                let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                ui.painter().rect_filled(r, 2.0, color);
                if ui.checkbox(&mut shown, label).changed() {
                    if shown {
                        app.cal.hidden.remove(&id);
                    } else {
                        app.cal.hidden.insert(id.clone());
                    }
                }
            });
            if id == LOCAL {
                continue;
            }
            let connecting = app.cal.connecting.as_ref().is_some_and(|(a, _)| *a == id);
            if connecting {
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    ui.spinner();
                    ui.label(RichText::new("Finish signing in in your browser...").small().color(SUBTLE));
                });
            } else if app.cal.needs_connect.contains(&id) {
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    if ui.link("Connect this calendar...").on_hover_text("One-time Microsoft sign-in for calendar access").clicked() {
                        app.actions.push(Action::ConnectCalendar(id.clone()));
                    }
                });
            } else if let Some(err) = app.cal.errors.get(&id) {
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    ui.add(egui::Label::new(RichText::new(err).small().color(theme::FLAG_RED)).wrap());
                });
            }
        }
    });
}

// --------------------------------------------------------------- main area

fn title(cal: &CalendarState) -> String {
    let days = cal.days();
    match cal.view {
        CalView::Day => cal.date.format("%A, %B %-d, %Y").to_string(),
        CalView::Month => cal.date.format("%B %Y").to_string(),
        _ => {
            let (a, b) = (days[0], *days.last().unwrap());
            if a.month() == b.month() {
                format!("{} {}\u{2013}{}, {}", a.format("%B"), a.day(), b.day(), b.year())
            } else if a.year() == b.year() {
                format!("{} \u{2013} {}, {}", a.format("%B %-d"), b.format("%B %-d"), b.year())
            } else {
                format!("{} \u{2013} {}", a.format("%B %-d, %Y"), b.format("%B %-d, %Y"))
            }
        }
    }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 12, top: 8, bottom: 6 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            if ui.button(RichText::new("\u{2039}").size(16.0)).on_hover_text("Back").clicked() {
                app.cal.step(false);
            }
            if ui.button(RichText::new("\u{203A}").size(16.0)).on_hover_text("Forward").clicked() {
                app.cal.step(true);
            }
            ui.add_space(6.0);
            ui.label(RichText::new(title(&app.cal)).size(20.0));
        });
    });
    match app.cal.view {
        CalView::Month => month_view(app, ui),
        _ => time_grid(app, ui),
    }
}

const HOUR_H: f32 = 44.0;
const GUTTER: f32 = 58.0;
const WORK: std::ops::Range<u32> = 8 * 60..17 * 60;

fn day_header(ui: &Ui, rect: Rect, day: NaiveDate) {
    let today = day == Local::now().date_naive();
    let p = ui.painter();
    let color = if today { BLUE } else { theme::TEXT };
    p.text(rect.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, day.day().to_string(), FontId::proportional(22.0), color);
    p.text(rect.left_center() + vec2(40.0, 1.0), Align2::LEFT_CENTER, day.format("%A").to_string(), FontId::proportional(12.5), color);
    if today {
        p.hline(rect.x_range(), rect.top() + 1.0, Stroke::new(3.0, BLUE));
    }
    p.vline(rect.left(), rect.y_range(), Stroke::new(1.0, BORDER));
}

/// An event block: tinted fill, colored left bar, subject and location.
fn paint_event(ui: &Ui, rect: Rect, e: &Event, color: Color32, selected: bool) {
    let p = ui.painter_at(rect);
    let fill = if e.response == Response::Tentative || e.response == Response::NotResponded { Color32::WHITE } else { light(color) };
    p.rect_filled(rect, 2.0, fill);
    p.rect_filled(Rect::from_min_size(rect.min, vec2(4.0, rect.height())), 0.0, color);
    let border = if selected { Stroke::new(2.0, theme::TEXT) } else { Stroke::new(1.0, color.gamma_multiply(0.5)) };
    p.rect_stroke(rect, 2.0, border, egui::StrokeKind::Inside);
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = rect.width() - 10.0;
    let subject = if e.subject.is_empty() { "(No subject)" } else { &e.subject };
    job.append(subject, 0.0, egui::TextFormat::simple(FontId::new(12.5, theme::bold_family()), theme::TEXT));
    if !e.location.is_empty() {
        job.append(&format!("\n{}", e.location), 0.0, egui::TextFormat::simple(FontId::proportional(11.5), SUBTLE));
    }
    let galley = ui.painter().layout_job(job);
    p.galley(rect.min + vec2(8.0, 2.0), galley, theme::TEXT);
}

fn event_tooltip(ui: &mut Ui, e: &Event) {
    ui.label(theme::bold(if e.subject.is_empty() { "(No subject)" } else { &e.subject }, 13.5));
    let when = if e.all_day {
        "All day".to_string()
    } else {
        format!("{} \u{2013} {}", time_of(calendar::local_minutes(e.start)), time_of(calendar::local_minutes(e.end)))
    };
    ui.label(when);
    if !e.location.is_empty() {
        ui.label(&e.location);
    }
    if !e.organizer.is_empty() && e.response != Response::Organizer {
        ui.label(RichText::new(format!("Organizer: {}", if e.organizer_name.is_empty() { &e.organizer } else { &e.organizer_name })).small());
    }
    if e.recurring {
        ui.label(RichText::new("Recurring").small().color(SUBTLE));
    }
}

/// Day, Work Week and Week views.
fn time_grid(app: &mut App, ui: &mut Ui) {
    let days = app.cal.days();
    let events = visible(app);
    let n = days.len() as f32;
    let full = ui.available_rect_before_wrap();
    let col_w = ((full.width() - GUTTER - 14.0) / n).max(40.0);
    let col_x = |i: usize, left: f32| left + GUTTER + col_w * i as f32;

    // Day headers.
    let (hdr, _) = ui.allocate_exact_size(vec2(full.width(), 42.0), Sense::hover());
    for (i, d) in days.iter().enumerate() {
        day_header(ui, Rect::from_min_size(pos2(col_x(i, hdr.left()), hdr.top()), vec2(col_w, hdr.height())), *d);
    }

    // All-day band.
    let all_day: Vec<Vec<&(Event, Color32)>> =
        days.iter().map(|d| events.iter().filter(|(e, _)| e.all_day && e.on_day(*d)).collect()).collect();
    let rows = all_day.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let (band, _) = ui.allocate_exact_size(vec2(full.width(), rows as f32 * 22.0 + 6.0), Sense::hover());
    ui.painter().hline(band.x_range(), band.top(), Stroke::new(1.0, BORDER));
    ui.painter().hline(band.x_range(), band.bottom(), Stroke::new(1.0, BORDER));
    for (i, list) in all_day.iter().enumerate() {
        for (row, (e, color)) in list.iter().enumerate() {
            let r = Rect::from_min_size(pos2(col_x(i, band.left()) + 2.0, band.top() + 3.0 + row as f32 * 22.0), vec2(col_w - 4.0, 20.0));
            let selected = app.cal.selected.as_ref() == Some(&(e.calendar.clone(), e.id.clone()));
            paint_event(ui, r, e, *color, selected);
            let resp = ui.interact(r, ui.id().with(("allday", &e.calendar, &e.id, i)), Sense::click());
            handle_event_click(app, &resp, e);
            resp.on_hover_ui_at_pointer(|ui| event_tooltip(ui, e));
        }
    }

    // Scrollable hours.
    let mut scroll = egui::ScrollArea::vertical().id_salt("cal_grid").auto_shrink(false);
    if std::mem::take(&mut app.cal.scroll_to_morning) {
        scroll = scroll.vertical_scroll_offset(8.0 * HOUR_H - 4.0);
    }
    scroll.show(ui, |ui| {
        let (area, resp) = ui.allocate_exact_size(vec2(full.width() - 14.0, 24.0 * HOUR_H), Sense::click());
        let y_of = |min: f32| area.top() + min / 60.0 * HOUR_H;
        let p = ui.painter();
        let now = Local::now();
        for (i, d) in days.iter().enumerate() {
            let x = col_x(i, area.left());
            let weekend = d.weekday().number_from_monday() > 5;
            // Outlook shades non-working hours.
            let shade = Color32::from_rgb(0xF3, 0xF6, 0xFA);
            if weekend {
                p.rect_filled(Rect::from_min_max(pos2(x, area.top()), pos2(x + col_w, area.bottom())), 0.0, shade);
            } else {
                p.rect_filled(Rect::from_min_max(pos2(x, area.top()), pos2(x + col_w, y_of(WORK.start as f32))), 0.0, shade);
                p.rect_filled(Rect::from_min_max(pos2(x, y_of(WORK.end as f32)), pos2(x + col_w, area.bottom())), 0.0, shade);
            }
            p.vline(x, area.y_range(), Stroke::new(1.0, BORDER));
        }
        if let Some((d, m)) = app.cal.slot {
            if let Some(i) = days.iter().position(|x| *x == d) {
                let x = col_x(i, area.left());
                p.rect_filled(Rect::from_min_max(pos2(x + 1.0, y_of(m as f32)), pos2(x + col_w, y_of(m as f32 + 30.0))), 0.0, theme::SELECTED);
            }
        }
        for h in 0..24 {
            let y = y_of(h as f32 * 60.0);
            p.hline((area.left() + GUTTER)..=area.right(), y, Stroke::new(1.0, Color32::from_gray(0xDD)));
            p.hline((area.left() + GUTTER)..=area.right(), y + HOUR_H / 2.0, Stroke::new(1.0, Color32::from_gray(0xEE)));
            p.text(pos2(area.left() + GUTTER - 6.0, y + 3.0), Align2::RIGHT_TOP, time_label(h * 60), FontId::proportional(11.5), SUBTLE);
        }

        // Events, side by side where they overlap.
        let mut hit: Vec<(Rect, Event)> = Vec::new();
        for (i, d) in days.iter().enumerate() {
            let (ds, de) = calendar::day_bounds(*d);
            let todays: Vec<&(Event, Color32)> = events.iter().filter(|(e, _)| !e.all_day && e.on_day(*d)).collect();
            let spans: Vec<(i64, i64)> = todays.iter().map(|(e, _)| (e.start.max(ds), e.end.min(de).max(e.start.max(ds) + 900))).collect();
            for ((e, color), ((s, en), (lane, lanes))) in todays.iter().zip(spans.iter().zip(calendar::lanes(&spans))) {
                let w = (col_w - 8.0) / lanes as f32;
                let top = y_of((s - ds) as f32 / 60.0);
                let bottom = y_of((en - ds) as f32 / 60.0).max(top + 18.0);
                let r = Rect::from_min_max(pos2(col_x(i, area.left()) + 2.0 + w * lane as f32, top + 1.0), pos2(col_x(i, area.left()) + w * (lane + 1) as f32, bottom - 1.0));
                let selected = app.cal.selected.as_ref() == Some(&(e.calendar.clone(), e.id.clone()));
                paint_event(ui, r, e, *color, selected);
                hit.push((r, e.clone()));
            }
            if *d == now.date_naive() {
                let y = y_of(calendar::local_minutes(now.timestamp()) as f32);
                let x = col_x(i, area.left());
                ui.painter().hline(x..=x + col_w, y, Stroke::new(2.0, theme::FLAG_RED));
                ui.painter().circle_filled(pos2(x, y), 4.0, theme::FLAG_RED);
            }
        }

        // Clicks: on an event select/open it, on empty space pick a slot / create one.
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
        let target = pointer.and_then(|pt| hit.iter().rev().find(|(r, _)| r.contains(pt)).map(|(_, e)| e.clone()));
        match (&target, pointer) {
            (Some(e), _) => {
                handle_event_click(app, &resp, e);
                resp.on_hover_ui_at_pointer(|ui| event_tooltip(ui, e));
            }
            (None, Some(pt)) if resp.clicked() || resp.double_clicked() => {
                let i = ((pt.x - area.left() - GUTTER) / col_w).floor();
                if i >= 0.0 && (i as usize) < days.len() {
                    let minutes = (((pt.y - area.top()) / HOUR_H * 60.0) as u32 / 30 * 30).min(23 * 60 + 30);
                    let day = days[i as usize];
                    app.cal.slot = Some((day, minutes));
                    app.cal.selected = None;
                    if resp.double_clicked() {
                        app.actions.push(Action::NewAppointment { start: Some(calendar::local_ts(day, minutes)), all_day: false, meeting: false });
                    }
                }
            }
            _ => {}
        }
    });
}

fn handle_event_click(app: &mut App, resp: &egui::Response, e: &Event) {
    if resp.clicked() || resp.double_clicked() {
        app.cal.selected = Some((e.calendar.clone(), e.id.clone()));
        app.cal.slot = None;
    }
    if resp.double_clicked() {
        app.actions.push(Action::OpenEvent(e.calendar.clone(), e.id.clone()));
    }
}

fn month_view(app: &mut App, ui: &mut Ui) {
    let days = app.cal.days();
    let events = visible(app);
    let month = app.cal.date.month();
    let full = ui.available_rect_before_wrap();
    let (hdr, _) = ui.allocate_exact_size(vec2(full.width(), 24.0), Sense::hover());
    let cw = (full.width() - 8.0) / 7.0;
    for (i, name) in ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"].iter().enumerate() {
        ui.painter().text(pos2(hdr.left() + cw * i as f32 + 8.0, hdr.center().y), Align2::LEFT_CENTER, *name, FontId::proportional(12.5), SUBTLE);
    }
    let area = ui.available_rect_before_wrap();
    let chh = (area.height() / 6.0).max(60.0);
    let today = Local::now().date_naive();
    for (i, d) in days.iter().enumerate() {
        let cell = Rect::from_min_size(pos2(area.left() + cw * (i % 7) as f32, area.top() + chh * (i / 7) as f32), vec2(cw, chh));
        let resp = ui.interact(cell, ui.id().with(("month_cell", *d)), Sense::click());
        let p = ui.painter();
        let fill = if *d == app.cal.date { theme::SELECTED } else if d.month() != month { Color32::from_rgb(0xF7, 0xF7, 0xF7) } else { Color32::WHITE };
        p.rect(cell, 0.0, fill, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
        let label = if d.day() == 1 { d.format("%b %-d").to_string() } else { d.day().to_string() };
        if *d == today {
            p.rect_filled(Rect::from_min_size(cell.min + vec2(3.0, 3.0), vec2(if d.day() == 1 { 44.0 } else { 22.0 }, 18.0)), 2.0, BLUE);
            p.text(cell.min + vec2(7.0, 4.0), Align2::LEFT_TOP, label, FontId::new(12.5, theme::bold_family()), Color32::WHITE);
        } else {
            let c = if d.month() == month { theme::TEXT } else { Color32::from_gray(150) };
            p.text(cell.min + vec2(7.0, 4.0), Align2::LEFT_TOP, label, FontId::proportional(12.5), c);
        }
        let mut list: Vec<&(Event, Color32)> = events.iter().filter(|(e, _)| e.on_day(*d)).collect();
        list.sort_by_key(|(e, _)| (!e.all_day, e.start));
        let fit = (((chh - 26.0) / 18.0).floor() as usize).max(1);
        for (row, (e, color)) in list.iter().take(if list.len() > fit { fit - 1 } else { fit }).enumerate() {
            let r = Rect::from_min_size(cell.min + vec2(3.0, 24.0 + row as f32 * 18.0), vec2(cw - 6.0, 16.0));
            let selected = app.cal.selected.as_ref() == Some(&(e.calendar.clone(), e.id.clone()));
            let p = ui.painter_at(r);
            if e.all_day {
                p.rect_filled(r, 2.0, light(*color));
                p.rect_filled(Rect::from_min_size(r.min, vec2(3.0, r.height())), 0.0, *color);
            } else {
                p.circle_filled(r.left_center() + vec2(5.0, 0.0), 3.0, *color);
            }
            if selected {
                p.rect_stroke(r, 2.0, Stroke::new(1.5, theme::TEXT), egui::StrokeKind::Inside);
            }
            let text = if e.all_day { e.subject.clone() } else { format!("{} {}", time_of(calendar::local_minutes(e.start)), e.subject) };
            p.text(r.left_center() + vec2(11.0, 0.0), Align2::LEFT_CENTER, text, FontId::proportional(11.5), theme::TEXT);
            let er = ui.interact(r, ui.id().with(("month_ev", &e.calendar, &e.id, *d)), Sense::click());
            handle_event_click(app, &er, e);
            er.on_hover_ui_at_pointer(|ui| event_tooltip(ui, e));
        }
        if list.len() > fit {
            let more = list.len() - (fit - 1);
            let pos = cell.min + vec2(8.0, 24.0 + (fit - 1) as f32 * 18.0 + 8.0);
            ui.painter().text(pos, Align2::LEFT_CENTER, format!("+{more} more"), FontId::proportional(11.5), BLUE);
        }
        if resp.clicked() {
            app.cal.date = *d;
        }
        if resp.double_clicked() {
            app.cal.go_to(*d);
            app.cal.view = CalView::Day;
        }
    }
    ui.allocate_rect(Rect::from_min_size(area.min, vec2(cw * 7.0, chh * 6.0)), Sense::hover());
}
