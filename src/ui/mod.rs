//! Main window layout: ribbon, folder pane, message list, reading pane,
//! navigation bar and status bar, arranged like Outlook Classic.

pub mod account;
pub mod compose;
pub mod folders;
pub mod list;
pub mod reading;
pub mod ribbon;

use eframe::egui::{self, CentralPanel, Color32, Frame, Margin, Panel, RichText, Stroke, Ui};

use crate::app::{App, Module};
use crate::theme::{self, BLUE, BORDER, RIBBON_BG, SUBTLE, icon};

pub fn draw(app: &mut App, ui: &mut Ui) {
    update_title(app, ui.ctx());
    if app.backstage {
        account::backstage(app, ui);
        return;
    }

    Panel::top("ribbon").frame(Frame::NONE.fill(RIBBON_BG)).show(ui, |ui| ribbon::show(app, ui));
    Panel::bottom("status_bar")
        .frame(Frame::NONE.fill(Color32::from_rgb(0xF0, 0xF0, 0xF0)).inner_margin(Margin::symmetric(10, 3)))
        .show(ui, |ui| status_bar(app, ui));
    Panel::bottom("nav_bar")
        .frame(Frame::NONE.fill(theme::WHITE).inner_margin(Margin::symmetric(8, 4)).stroke(Stroke::new(1.0, BORDER)))
        .show(ui, |ui| nav_bar(app, ui));

    if app.show_folder_pane {
        Panel::left("folder_pane")
            .default_size(230.0)
            .min_size(150.0)
            .frame(Frame::NONE.fill(theme::WHITE).inner_margin(Margin::symmetric(0, 6)))
            .show(ui, |ui| folders::show(app, ui));
    }

    let white = Frame::NONE.fill(theme::WHITE);
    match app.module {
        Module::Mail if app.show_reading_pane => {
            Panel::left("message_list")
                .default_size(400.0)
                .min_size(280.0)
                .frame(white)
                .show(ui, |ui| list::show(app, ui));
            CentralPanel::default().frame(white).show(ui, |ui| reading::show(app, ui));
        }
        Module::Mail => {
            CentralPanel::default().frame(white).show(ui, |ui| list::show(app, ui));
        }
        other => {
            let name = match other {
                Module::Calendar => "Calendar",
                Module::People => "People",
                _ => "Tasks",
            };
            CentralPanel::default().frame(white).show(ui, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new(format!("{name} isn't part of this build yet. Mail is fully functional.")).color(SUBTLE));
                });
            });
        }
    }
}

fn update_title(app: &App, ctx: &egui::Context) {
    let folder = app.current_folder().map(|f| f.display_name()).unwrap_or_else(|| "Inbox".into());
    let title = match app.current_account() {
        Some(a) => format!("{folder} - {} - Outlook RS", a.config.email),
        None => "Outlook RS".to_string(),
    };
    let id = egui::Id::new("window_title");
    if ctx.data(|d| d.get_temp::<String>(id)).as_deref() != Some(title.as_str()) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
        ctx.data_mut(|d| d.insert_temp(id, title));
    }
}

fn status_bar(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        let (total, unread) = app.current_folder().map(|f| (f.total, f.unread)).unwrap_or((0, 0));
        let shown = app.visible_messages().len();
        let filtered = app.unread_only || !app.search.trim().is_empty();
        ui.label(RichText::new(format!("Items: {}", if filtered { shown as u32 } else { total })).small());
        ui.add_space(12.0);
        ui.label(RichText::new(format!("Unread: {unread}")).small());
        if filtered {
            ui.add_space(12.0);
            ui.label(RichText::new("FILTER APPLIED").small().color(BLUE));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let Some(acct) = app.current_account() else {
                ui.label(RichText::new("No accounts").small());
                return;
            };
            if let Some(err) = acct.error.clone() {
                let r = ui.add(
                    egui::Label::new(RichText::new(format!("{}  Disconnected", '\u{26A0}')).small().color(theme::FLAG_RED))
                        .sense(egui::Sense::click()),
                );
                if r.on_hover_text(&err).clicked() {
                    app.alert = Some(err);
                }
            } else {
                let text = if acct.status.is_empty() { "Connected".to_string() } else { acct.status.clone() };
                ui.label(RichText::new(text).small());
            }
        });
    });
}

fn nav_bar(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        for (module, label, glyph) in [
            (Module::Mail, "Mail", icon::MAIL),
            (Module::Calendar, "Calendar", icon::CALENDAR),
            (Module::People, "People", icon::PEOPLE),
            (Module::Tasks, "Tasks", icon::TASKS),
        ] {
            let selected = app.module == module;
            let color = if selected { BLUE } else { theme::TEXT };
            let r = ui
                .horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    ui.label(icon(glyph, 15.0).color(color));
                    let text = if selected { theme::bold(label, 13.5).color(color) } else { RichText::new(label).color(color) };
                    ui.label(text);
                })
                .response
                .interact(egui::Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if r.clicked() {
                app.module = module;
            }
        }
    });
}

pub fn show_alert(app: &mut App, ctx: &egui::Context) {
    let Some(text) = app.alert.clone() else { return };
    let modal = egui::Modal::new(egui::Id::new("alert")).show(ctx, |ui| {
        ui.set_max_width(420.0);
        ui.label(theme::bold("Outlook RS", 15.0));
        ui.add_space(6.0);
        ui.label(text);
        ui.add_space(10.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| ui.button("   OK   ").clicked()).inner
    });
    if modal.inner || modal.should_close() {
        app.alert = None;
    }
}

/// Paint a flat Outlook-style button with an icon and caption; returns its response.
pub fn flat_button(ui: &mut Ui, glyph: char, label: &str) -> egui::Response {
    let enabled = ui.is_enabled();
    let color = if enabled { theme::TEXT } else { Color32::from_gray(170) };
    let icon_color = if enabled { BLUE } else { Color32::from_gray(170) };
    let galley = ui.painter().layout_no_wrap(label.to_string(), egui::FontId::proportional(13.0), color);
    let size = egui::vec2(galley.size().x + 30.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        if resp.hovered() && enabled {
            ui.painter().rect(rect, 0.0, theme::HOVER, Stroke::new(1.0, Color32::from_rgb(0xA0, 0xC8, 0xE8)), egui::StrokeKind::Inside);
        }
        ui.painter().text(
            rect.left_center() + egui::vec2(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            glyph,
            egui::FontId::new(14.0, theme::icon_family()),
            icon_color,
        );
        ui.painter().galley(rect.left_center() + egui::vec2(24.0, -galley.size().y / 2.0), galley, color);
    }
    resp
}
