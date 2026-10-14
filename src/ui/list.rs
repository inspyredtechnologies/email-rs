//! Message list: search box, All/Unread filter, and date-grouped three-line items.

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone};
use eframe::egui::{self, Align2, Color32, FontId, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

use crate::app::{Action, App};
use crate::mime::ReplyKind;
use crate::model::MessageMeta;
use crate::theme::{self, BLUE, BORDER, FLAG_RED, HOVER, SELECTED, SUBTLE, icon};

const ROW_HEIGHT: f32 = 64.0;

pub fn show(app: &mut App, ui: &mut Ui) {
    search_box(app, ui);
    filter_row(app, ui);
    ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0, BORDER));

    let now = Local::now();
    let messages: Vec<MessageMeta> = app.visible_messages().into_iter().cloned().collect();
    let selected = app.selected;
    let scroll_to = app.actions.iter().find_map(|a| if let Action::SelectMessage(u) = a { Some(*u) } else { None });

    egui::ScrollArea::vertical().id_salt("message_list").auto_shrink(false).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        if messages.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("We didn't find anything to show here.").color(SUBTLE));
            });
            return;
        }
        let mut last_group = String::new();
        for m in &messages {
            let group = group_label(m.date, now);
            if group != last_group {
                group_header(ui, &group);
                last_group = group;
            }
            let resp = message_row(ui, m, selected == Some(m.uid), now);
            if scroll_to == Some(m.uid) {
                resp.scroll_to_me(None);
            }
            handle_row(app, &resp, m);
        }
    });
}

fn search_box(app: &mut App, ui: &mut Ui) {
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
        ui.horizontal(|ui| {
            let edit = egui::TextEdit::singleline(&mut app.search)
                .hint_text("Search Current Mailbox (Ctrl+E)")
                .desired_width(ui.available_width() - 24.0)
                .margin(egui::Margin::symmetric(6, 4));
            let resp = ui.add(edit);
            if app.focus_search {
                resp.request_focus();
                app.focus_search = false;
            }
            if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                app.search.clear();
                resp.surrender_focus();
            }
            ui.label(icon(icon::SEARCH, 14.0).color(SUBTLE));
        });
    });
}

fn filter_row(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        for (label, unread) in [("All", false), ("Unread", true)] {
            let active = app.unread_only == unread;
            let text = if active { theme::bold(label, 13.5).color(BLUE) } else { RichText::new(label).color(SUBTLE) };
            let r = ui.add(egui::Label::new(text).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand);
            if active {
                let rect = r.rect;
                ui.painter().hline(rect.x_range(), rect.bottom() + 2.0, Stroke::new(2.0, BLUE));
            }
            if r.clicked() {
                app.unread_only = unread;
            }
            ui.add_space(8.0);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(10.0);
            ui.label(RichText::new("\u{2193}").color(SUBTLE));
            ui.label(RichText::new("By Date").color(SUBTLE));
        });
    });
    ui.add_space(4.0);
}

fn group_header(ui: &mut Ui, label: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    let p = ui.painter();
    p.text(rect.left_center() + vec2(10.0, 1.0), Align2::LEFT_CENTER, icon::CHEVRON_DOWN, FontId::new(9.0, theme::icon_family()), SUBTLE);
    p.text(rect.left_center() + vec2(24.0, 1.0), Align2::LEFT_CENTER, label, FontId::new(12.5, theme::bold_family()), theme::GROUP_HEADER);
}

fn message_row(ui: &mut Ui, m: &MessageMeta, selected: bool, now: DateTime<Local>) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let p = ui.painter_at(rect);
    let bg = if selected { SELECTED } else if resp.hovered() { HOVER } else { Color32::TRANSPARENT };
    p.rect_filled(rect, 0.0, bg);
    if !m.seen {
        p.rect_filled(Rect::from_min_size(rect.min, vec2(3.0, rect.height())), 0.0, BLUE);
    }
    p.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, Color32::from_rgb(0xEA, 0xEA, 0xEA)));

    let left = rect.left() + 14.0;
    let right = rect.right() - 10.0;
    let unread = !m.seen;
    let regular = FontId::proportional(13.5);
    let bold = FontId::new(13.5, theme::bold_family());

    // Line 1: sender, then attachment / flag icons on the right.
    let mut icons_x = right;
    let flag_color = if m.flagged { FLAG_RED } else if resp.hovered() { Color32::from_gray(170) } else { Color32::TRANSPARENT };
    p.text(pos2(icons_x, rect.top() + 13.0), Align2::RIGHT_CENTER, icon::FLAG, FontId::new(13.0, theme::icon_family()), flag_color);
    icons_x -= 20.0;
    if m.has_attachments {
        p.text(pos2(icons_x, rect.top() + 13.0), Align2::RIGHT_CENTER, icon::ATTACH, FontId::new(13.0, theme::icon_family()), SUBTLE);
        icons_x -= 18.0;
    }
    if m.answered {
        p.text(pos2(icons_x, rect.top() + 13.0), Align2::RIGHT_CENTER, icon::REPLY, FontId::new(12.0, theme::icon_family()), SUBTLE);
        icons_x -= 18.0;
    }
    let sender_font = if unread { bold.clone() } else { regular.clone() };
    clipped_text(&p, pos2(left, rect.top() + 13.0), m.sender_label(), sender_font, theme::TEXT, icons_x - left - 6.0);

    // Line 2: subject and date.
    let date = short_date(m.date, now);
    let date_color = if unread { BLUE } else { SUBTLE };
    let date_rect = p.text(pos2(right, rect.top() + 32.0), Align2::RIGHT_CENTER, &date, FontId::proportional(12.5), date_color);
    let subject = if m.subject.is_empty() { "(No subject)" } else { m.subject.as_str() };
    let (sfont, scolor) = if unread { (bold, BLUE) } else { (regular, theme::TEXT) };
    clipped_text(&p, pos2(left, rect.top() + 32.0), subject, sfont, scolor, date_rect.left() - left - 8.0);

    // Line 3: preview.
    clipped_text(&p, pos2(left, rect.top() + 50.0), &m.preview, FontId::proportional(12.5), SUBTLE, right - left);
    resp
}

/// Draw single-line text, truncated with an ellipsis to fit `max_width`.
fn clipped_text(p: &egui::Painter, pos: egui::Pos2, text: &str, font: FontId, color: Color32, max_width: f32) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width.max(10.0));
    let galley = p.layout_job(job);
    p.galley(pos2(pos.x, pos.y - galley.size().y / 2.0), galley, color);
}

fn handle_row(app: &mut App, resp: &egui::Response, m: &MessageMeta) {
    let flag_hit = resp
        .interact_pointer_pos()
        .is_some_and(|p| p.x > resp.rect.right() - 30.0 && p.y < resp.rect.top() + 24.0);
    if resp.clicked() {
        if flag_hit {
            app.actions.push(Action::ToggleFlag(m.uid));
        } else {
            app.actions.push(Action::SelectMessage(m.uid));
        }
    }
    if resp.secondary_clicked() && app.selected != Some(m.uid) {
        app.actions.push(Action::SelectMessage(m.uid));
    }
    let actions = &mut app.actions;
    resp.context_menu(|ui| {
        ui.set_min_width(170.0);
        let mut item = |ui: &mut Ui, label: &str, action: Action| {
            if ui.button(label).clicked() {
                actions.push(action);
                ui.close();
            }
        };
        item(ui, "Reply", Action::Respond(ReplyKind::Reply));
        item(ui, "Reply All", Action::Respond(ReplyKind::ReplyAll));
        item(ui, "Forward", Action::Respond(ReplyKind::Forward));
        ui.separator();
        if m.seen {
            item(ui, "Mark as Unread", Action::SetRead(false));
        } else {
            item(ui, "Mark as Read", Action::SetRead(true));
        }
        item(ui, if m.flagged { "Clear Flag" } else { "Follow Up" }, Action::ToggleFlag(m.uid));
        ui.separator();
        item(ui, "Delete", Action::Delete);
        item(ui, "Archive...", Action::Archive);
    });
}

fn local(ts: i64) -> DateTime<Local> {
    Local.timestamp_opt(ts, 0).single().unwrap_or_else(Local::now)
}

/// Outlook's "Arrange by Date" group names.
pub fn group_label(ts: i64, now: DateTime<Local>) -> String {
    let d = local(ts).date_naive();
    let today = now.date_naive();
    let days = (today - d).num_days();
    let into_week = today.weekday().num_days_from_sunday() as i64;
    match days {
        i64::MIN..=0 => "Today".into(),
        1 => "Yesterday".into(),
        _ if days <= into_week => local(ts).format("%A").to_string(),
        _ if days <= into_week + 7 => "Last Week".into(),
        _ if days <= into_week + 14 => "Two Weeks Ago".into(),
        _ if days <= into_week + 21 => "Three Weeks Ago".into(),
        _ if d.year() == today.year() && d.month() == today.month() => "Earlier this Month".into(),
        _ => {
            let first_this_month = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap();
            let last_month = first_this_month.pred_opt().unwrap();
            if d.year() == last_month.year() && d.month() == last_month.month() {
                "Last Month".into()
            } else {
                "Older".into()
            }
        }
    }
}

/// Date column: time for today, weekday + time this week, otherwise M/D/YYYY.
pub fn short_date(ts: i64, now: DateTime<Local>) -> String {
    let dt = local(ts);
    let days = (now.date_naive() - dt.date_naive()).num_days();
    if days <= 0 {
        dt.format("%-I:%M %p").to_string()
    } else if days < 7 {
        dt.format("%a %-I:%M %p").to_string()
    } else {
        dt.format("%-m/%-d/%Y").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_like_outlook() {
        // Tuesday, October 13, 2026 at noon.
        let now = Local.with_ymd_and_hms(2026, 10, 13, 12, 0, 0).unwrap();
        let at = |y, mo, d| Local.with_ymd_and_hms(y, mo, d, 9, 0, 0).unwrap().timestamp();
        assert_eq!(group_label(at(2026, 10, 13), now), "Today");
        assert_eq!(group_label(at(2026, 10, 12), now), "Yesterday");
        assert_eq!(group_label(at(2026, 10, 11), now), "Sunday");
        assert_eq!(group_label(at(2026, 10, 6), now), "Last Week");
        assert_eq!(group_label(at(2026, 9, 29), now), "Two Weeks Ago");
        assert_eq!(group_label(at(2026, 9, 2), now), "Last Month");
        assert_eq!(group_label(at(2025, 1, 2), now), "Older");
        assert_eq!(short_date(at(2026, 10, 13), now), "9:00 AM");
        assert_eq!(short_date(at(2026, 10, 12), now), "Mon 9:00 AM");
        assert_eq!(short_date(at(2026, 9, 2), now), "9/2/2026");
    }
}
