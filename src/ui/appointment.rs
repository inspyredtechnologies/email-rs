//! Appointment / Meeting window: Outlook's event inspector, opened in its own
//! OS window like the compose window.

use chrono::{Duration, NaiveDate};
use eframe::egui::{self, CentralPanel, Color32, FontId, Frame, Margin, Panel, RichText, Stroke, Ui, vec2};

use crate::app::{Action, App};
use crate::calendar::{self, Event, LOCAL, Response};
use crate::theme::{self, BLUE, BORDER, RIBBON_BG, SUBTLE, icon};

pub struct Editor {
    pub id: u64,
    pub event: Event,
    pub is_new: bool,
    pub meeting: bool,
    /// Someone else's meeting: details are read-only, respond instead.
    pub read_only: bool,
    pub attendees: String,
    start_date: NaiveDate,
    start_min: u32,
    end_date: NaiveDate,
    end_min: u32,
    pub error: Option<String>,
    pub open: bool,
    confirm_delete: bool,
}

impl Editor {
    pub fn new(id: u64, event: Event, is_new: bool, meeting: bool) -> Self {
        let read_only = !is_new && event.is_meeting() && event.response != Response::Organizer && !event.organizer.is_empty();
        let (start_date, end_date) = (calendar::local_date(event.start), calendar::local_date(event.end));
        // All-day events end at midnight of the next day; show the last day instead.
        let end_date = if event.all_day && end_date > start_date { end_date - Duration::days(1) } else { end_date };
        Self {
            id,
            attendees: event.attendees.join("; "),
            start_min: calendar::local_minutes(event.start),
            end_min: calendar::local_minutes(event.end),
            start_date,
            end_date,
            meeting: meeting || event.is_meeting(),
            read_only,
            is_new,
            event,
            error: None,
            open: true,
            confirm_delete: false,
        }
    }

    /// The event as edited, or a validation error.
    fn build(&self) -> Result<Event, String> {
        let mut e = self.event.clone();
        if e.all_day {
            e.start = calendar::day_bounds(self.start_date).0;
            e.end = calendar::day_bounds(self.end_date).1;
        } else {
            e.start = calendar::local_ts(self.start_date, self.start_min);
            e.end = calendar::local_ts(self.end_date, self.end_min);
        }
        if e.end < e.start {
            return Err("The end time you entered occurs before the start time.".into());
        }
        e.attendees = if self.meeting {
            crate::mime::split_recipients(&self.attendees)
                .iter()
                .map(|a| a.rsplit_once('<').map(|(_, r)| r.trim_end_matches('>').trim().to_string()).unwrap_or_else(|| a.clone()))
                .collect()
        } else {
            Vec::new()
        };
        if self.meeting && e.attendees.is_empty() {
            return Err("Add at least one attendee, or remove the meeting attendees to save an appointment.".into());
        }
        if e.attendees.iter().any(|a| !a.contains('@')) {
            return Err("One or more attendees aren't valid email addresses.".into());
        }
        Ok(e)
    }
}

fn viewport_id(id: u64) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(("appointment", id))
}

pub fn show_all(app: &mut App, ctx: &egui::Context) {
    let calendars: Vec<(String, String)> = super::calendar::calendars(app).into_iter().map(|(id, label, _)| (id, label)).collect();
    let mut actions = Vec::new();
    for ed in app.cal.editors.iter_mut() {
        let kind = if ed.meeting { "Meeting" } else { "Appointment" };
        let subject = if ed.event.subject.trim().is_empty() { "Untitled" } else { ed.event.subject.trim() };
        let builder = egui::ViewportBuilder::default()
            .with_title(format!("{subject} - {kind}"))
            .with_inner_size([720.0, 560.0])
            .with_min_inner_size([520.0, 400.0])
            .with_icon(theme::app_icon());
        ctx.show_viewport_immediate(viewport_id(ed.id), builder, |ui, class| {
            window(ed, &calendars, ui, &mut actions);
            if class != egui::ViewportClass::EmbeddedWindow && ui.input(|i| i.viewport().close_requested()) {
                ed.open = false;
            }
        });
    }
    app.actions.extend(actions);
    app.cal.editors.retain(|e| e.open);
}

/// Half-hour time picker.
fn time_combo(ui: &mut Ui, id: (&str, u64), minutes: &mut u32) {
    egui::ComboBox::from_id_salt(id).width(96.0).selected_text(super::calendar::time_of(*minutes)).show_ui(ui, |ui| {
        for m in (0..24 * 60).step_by(30) {
            ui.selectable_value(minutes, m, super::calendar::time_of(m));
        }
    });
}

/// Date button that drops down a month calendar.
fn date_button(ui: &mut Ui, id: (&str, u64), date: &mut NaiveDate) {
    let resp = ui.add(egui::Button::new(date.format("%a %-m/%-d/%Y").to_string()).min_size(vec2(120.0, 24.0)));
    let month_key = ui.id().with(id).with("month");
    egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        let month = ui.data(|d| d.get_temp::<NaiveDate>(month_key)).unwrap_or_else(|| calendar::month_start(*date));
        let month = super::calendar::month_header(ui, month);
        ui.data_mut(|d| d.insert_temp(month_key, month));
        if let Some(d) = super::calendar::month_grid(ui, month, (*date, *date), Some(*date)) {
            *date = d;
            ui.close();
        }
    });
}

fn window(ed: &mut Editor, calendars: &[(String, String)], ui: &mut Ui, actions: &mut Vec<Action>) {
    let ro = ed.read_only;
    Panel::top("appt_ribbon").frame(Frame::NONE.fill(RIBBON_BG)).show(ui, |ui| {
        let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), egui::Sense::hover());
        ui.painter().rect_filled(strip, 0.0, BLUE);
        let tab = egui::Rect::from_min_size(strip.min + vec2(6.0, 2.0), vec2(96.0, 26.0));
        ui.painter().rect_filled(tab, 0.0, RIBBON_BG);
        let label = if ed.meeting { "Meeting" } else { "Appointment" };
        ui.painter().text(tab.center(), egui::Align2::CENTER_CENTER, label, FontId::proportional(13.0), BLUE);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            if !ro {
                let save_label = match (ed.meeting, ed.is_new) {
                    (true, true) => "Send",
                    (true, false) => "Send Update",
                    _ => "Save & Close",
                };
                if super::flat_button(ui, if ed.meeting { icon::SEND } else { icon::CALENDAR }, save_label).clicked() {
                    match ed.build() {
                        Ok(e) => {
                            actions.push(Action::SaveEvent(Box::new(e), ed.is_new));
                            ed.open = false;
                        }
                        Err(err) => ed.error = Some(err),
                    }
                }
            } else {
                for (glyph, label, resp) in [
                    (icon::READ, "Accept", Response::Accepted),
                    (icon::FLAG, "Tentative", Response::Tentative),
                    (icon::DELETE, "Decline", Response::Declined),
                ] {
                    if super::flat_button(ui, glyph, label).clicked() {
                        actions.push(Action::RespondEvent(Box::new(ed.event.clone()), resp));
                        ed.open = false;
                    }
                }
            }
            if !ed.is_new && super::flat_button(ui, icon::DELETE, "Delete").clicked() {
                ed.confirm_delete = true;
            }
            if !ro && !ed.meeting && super::flat_button(ui, icon::PEOPLE, "Invite Attendees").clicked() {
                ed.meeting = true;
            }
        });
        ui.add_space(4.0);
        ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0, BORDER));
    });

    CentralPanel::default().frame(Frame::NONE.fill(theme::WHITE).inner_margin(Margin::symmetric(14, 10))).show(ui, |ui| {
        if let Some(err) = &ed.error {
            Frame::NONE
                .fill(Color32::from_rgb(0xFF, 0xF4, 0xCE))
                .inner_margin(Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(err);
                });
            ui.add_space(6.0);
        }
        if ro {
            let who = if ed.event.organizer_name.is_empty() { &ed.event.organizer } else { &ed.event.organizer_name };
            let status = match ed.event.response {
                Response::Accepted => "You accepted this meeting.",
                Response::Tentative => "You tentatively accepted this meeting.",
                _ => "Please respond.",
            };
            ui.label(RichText::new(format!("{status}  Organizer: {who}")).color(SUBTLE));
            ui.add_space(6.0);
        }
        let field_w = ui.available_width() - 100.0;
        egui::Grid::new(("appt_grid", ed.id)).num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label("Title");
            ui.add_enabled(!ro, egui::TextEdit::singleline(&mut ed.event.subject).desired_width(field_w));
            ui.end_row();
            if ed.meeting {
                ui.label("Required");
                ui.add_enabled(
                    !ro,
                    egui::TextEdit::singleline(&mut ed.attendees).hint_text("name@example.com; ...").desired_width(field_w),
                );
                ui.end_row();
            }
            ui.label("Start time");
            ui.add_enabled_ui(!ro, |ui| {
                ui.horizontal(|ui| {
                    let before = calendar::local_ts(ed.start_date, ed.start_min);
                    date_button(ui, ("start", ed.id), &mut ed.start_date);
                    if !ed.event.all_day {
                        time_combo(ui, ("start_t", ed.id), &mut ed.start_min);
                    }
                    ui.checkbox(&mut ed.event.all_day, "All day event");
                    // Moving the start keeps the duration, as in Outlook.
                    let delta = calendar::local_ts(ed.start_date, ed.start_min) - before;
                    if delta != 0 {
                        let end = calendar::local_ts(ed.end_date, ed.end_min) + delta;
                        ed.end_date = calendar::local_date(end);
                        ed.end_min = calendar::local_minutes(end);
                    }
                });
            });
            ui.end_row();
            ui.label("End time");
            ui.add_enabled_ui(!ro, |ui| {
                ui.horizontal(|ui| {
                    date_button(ui, ("end", ed.id), &mut ed.end_date);
                    if !ed.event.all_day {
                        time_combo(ui, ("end_t", ed.id), &mut ed.end_min);
                    }
                });
            });
            ui.end_row();
            ui.label("Location");
            ui.add_enabled(!ro, egui::TextEdit::singleline(&mut ed.event.location).desired_width(field_w));
            ui.end_row();
            ui.label("Calendar");
            let current = calendars.iter().find(|(id, _)| *id == ed.event.calendar).map(|(_, l)| l.clone()).unwrap_or_default();
            ui.add_enabled_ui(ed.is_new, |ui| {
                egui::ComboBox::from_id_salt(("appt_cal", ed.id)).selected_text(current).width(field_w.min(360.0)).show_ui(ui, |ui| {
                    for (id, label) in calendars {
                        ui.selectable_value(&mut ed.event.calendar, id.clone(), label);
                    }
                });
            });
            ui.end_row();
        });
        if ed.meeting && ed.event.calendar == LOCAL && !ro {
            ui.label(
                RichText::new("Invitations are emailed from your mail account; responses arrive in your Inbox.").small().color(SUBTLE),
            );
        }
        ui.add_space(8.0);
        ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0, BORDER));
        ui.add_space(8.0);
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.add_enabled(
                !ro,
                egui::TextEdit::multiline(&mut ed.event.body)
                    .frame(Frame::NONE)
                    .desired_width(f32::INFINITY)
                    .desired_rows(10)
                    .font(FontId::proportional(14.0)),
            );
        });
    });

    if ed.confirm_delete {
        let what = if ed.meeting && ed.event.response == Response::Organizer { "Cancel this meeting and notify attendees?" } else { "Delete this item?" };
        let modal = egui::Modal::new(egui::Id::new(("appt_delete", ed.id))).show(ui.ctx(), |ui| {
            ui.set_max_width(360.0);
            ui.label(theme::bold("Outlook RS", 15.0));
            ui.add_space(6.0);
            ui.label(what);
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("  Cancel  ").clicked() {
                    return Some(false);
                }
                if ui.button("  Delete  ").clicked() {
                    return Some(true);
                }
                None
            })
            .inner
        });
        match modal.inner {
            Some(true) => {
                actions.push(Action::DeleteEvent(Box::new(ed.event.clone())));
                ed.open = false;
                ed.confirm_delete = false;
            }
            Some(false) => ed.confirm_delete = false,
            None if modal.should_close() => ed.confirm_delete = false,
            None => {}
        }
    }
}
