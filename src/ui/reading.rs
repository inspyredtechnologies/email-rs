//! Reading pane: response buttons, sender card, subject, attachments, body.

use chrono::{Local, TimeZone};
use eframe::egui::{self, Align2, Color32, FontId, RichText, Sense, Stroke, Ui, vec2};

use crate::app::{Action, App};
use crate::mime::ReplyKind;
use crate::theme::{self, BORDER, SUBTLE, icon};

pub fn show(app: &mut App, ui: &mut Ui) {
    let Some(meta) = app.selected_meta().cloned() else {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new("Select an item to read").size(15.0).color(SUBTLE));
        });
        return;
    };
    let body = app.selected_body();

    egui::Frame::NONE.inner_margin(egui::Margin { left: 18, right: 18, top: 10, bottom: 4 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (glyph, label, kind) in [
                (icon::REPLY, "Reply", ReplyKind::Reply),
                (icon::REPLY_ALL, "Reply All", ReplyKind::ReplyAll),
                (icon::FORWARD, "Forward", ReplyKind::Forward),
            ] {
                if super::flat_button(ui, glyph, label).clicked() {
                    app.actions.push(Action::Respond(kind));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let date = Local
                    .timestamp_opt(meta.date, 0)
                    .single()
                    .map(|d| d.format("%a %-m/%-d/%Y %-I:%M %p").to_string())
                    .unwrap_or_default();
                ui.label(RichText::new(date).color(SUBTLE));
            });
        });
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            avatar(ui, meta.sender_label(), 44.0);
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let from = if meta.from_name.is_empty() {
                    meta.from_addr.clone()
                } else {
                    format!("{} <{}>", meta.from_name, meta.from_addr)
                };
                ui.label(RichText::new(from).size(15.5));
                let to = body.as_ref().map(|b| b.to.clone()).unwrap_or_else(|| meta.to.clone());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("To").color(SUBTLE));
                    ui.label(to);
                });
                if let Some(cc) = body.as_ref().map(|b| b.cc.clone()).filter(|c| !c.is_empty()) {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Cc").color(SUBTLE));
                        ui.label(cc);
                    });
                }
            });
        });
        ui.add_space(8.0);
        let subject = if meta.subject.is_empty() { "(No subject)".to_string() } else { meta.subject.clone() };
        ui.add(egui::Label::new(RichText::new(subject).size(18.0)).wrap());
        if meta.flagged {
            ui.label(RichText::new(format!("{}  Follow up.", '\u{2691}')).color(theme::FLAG_RED).small());
        }

        if let Some(b) = &body {
            if !b.attachments.is_empty() {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    for (i, att) in b.attachments.iter().enumerate() {
                        if attachment_chip(ui, &att.name, att.data.len()).on_hover_text("Click to save").clicked() {
                            app.actions.push(Action::SaveAttachment(i));
                        }
                    }
                });
            }
        }
        ui.add_space(8.0);
    });
    ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0, BORDER));

    // HTML mail goes to the native web view, which is laid over the rest of the pane.
    if let (Some(b), false) = (&body, app.html_unavailable) {
        if let Some(html) = &b.html {
            let (acct, folder) = app.current.clone().unwrap_or_default();
            let key = (acct, folder, meta.uid);
            if crate::html::has_remote_content(html) && !app.allow_remote.contains(&key) {
                pictures_infobar(app, ui);
            }
            let rect = ui.available_rect_before_wrap();
            ui.allocate_rect(rect, Sense::hover());
            app.html_slot = Some(crate::app::HtmlSlot { rect, key, body: b.clone() });
            return;
        }
    }

    egui::ScrollArea::vertical().id_salt(("reading", meta.uid)).auto_shrink(false).show(ui, |ui| {
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(18, 14)).show(ui, |ui| {
            match &body {
                Some(b) => {
                    ui.add(egui::Label::new(RichText::new(&b.text).size(14.0)).selectable(true).wrap());
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Downloading message...").color(SUBTLE));
                    });
                }
            }
        });
    });
}

/// Outlook's privacy infobar for messages with blocked remote pictures.
fn pictures_infobar(app: &mut App, ui: &mut Ui) {
    let resp = egui::Frame::NONE
        .fill(Color32::from_rgb(0xF4, 0xF4, 0xF4))
        .inner_margin(egui::Margin::symmetric(18, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Click here to download pictures. To help protect your privacy, \
                         Outlook RS prevented automatic download of some pictures in this message.",
                    )
                    .small(),
                )
                .wrap(),
            );
        })
        .response
        .interact(Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        app.actions.push(Action::AllowRemoteContent);
    }
    ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0, BORDER));
}

pub fn avatar(ui: &mut Ui, name: &str, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, theme::avatar_color(name));
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, theme::initials(name), FontId::proportional(size * 0.38), Color32::WHITE);
}

fn attachment_chip(ui: &mut Ui, name: &str, size: usize) -> egui::Response {
    let label = format!("{name}\n{}", human_size(size));
    let galley = ui.painter().layout_no_wrap(label, FontId::proportional(12.5), theme::TEXT);
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 44.0, 42.0), Sense::click());
    let fill = if resp.hovered() { theme::HOVER } else { theme::WHITE };
    ui.painter().rect(rect, 0.0, fill, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
    ui.painter().text(rect.left_center() + vec2(18.0, 0.0), Align2::CENTER_CENTER, icon::ATTACH, FontId::new(18.0, theme::icon_family()), theme::BLUE);
    ui.painter().galley(rect.left_center() + vec2(36.0, -galley.size().y / 2.0), galley, theme::TEXT);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn human_size(bytes: usize) -> String {
    match bytes {
        b if b < 1024 => format!("{b} bytes"),
        b if b < 1024 * 1024 => format!("{} KB", b.div_ceil(1024)),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}
