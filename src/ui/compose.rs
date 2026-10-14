//! Compose window: each draft opens in its own OS window, like Outlook's
//! Message inspector, with the big Send button beside the address fields.

use eframe::egui::{self, Align2, CentralPanel, Color32, FontId, Frame, Margin, Panel, RichText, Sense, Stroke, Ui, vec2};

use crate::app::{Action, App, Composer};
use crate::theme::{self, BLUE, BORDER, RIBBON_BG, icon};

fn viewport_id(id: u64) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(("compose", id))
}

pub fn show_all(app: &mut App, ctx: &egui::Context) {
    let accounts: Vec<(String, String)> =
        app.accounts.iter().map(|a| (a.config.id.clone(), a.config.email.clone())).collect();
    let mut to_send = Vec::new();
    for c in app.composers.iter_mut() {
        let subject = if c.draft.subject.trim().is_empty() { "Untitled" } else { c.draft.subject.trim() };
        let builder = egui::ViewportBuilder::default()
            .with_title(format!("{subject} - Message (Plain Text)"))
            .with_inner_size([780.0, 640.0])
            .with_min_inner_size([520.0, 380.0]);
        ctx.show_viewport_immediate(viewport_id(c.id), builder, |ui, class| {
            if window(c, &accounts, ui) {
                to_send.push(c.id);
            }
            // An embedded window would see the main window's close request instead of its own.
            let embedded = class == egui::ViewportClass::EmbeddedWindow;
            if !embedded && ui.input(|i| i.viewport().close_requested()) {
                if is_dirty(c) {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    c.confirm_discard = true;
                } else {
                    c.open = false;
                }
            }
        });
    }
    app.actions.extend(to_send.into_iter().map(Action::Send));
}

fn is_dirty(c: &Composer) -> bool {
    let d = &c.draft;
    [&d.to, &d.cc, &d.bcc, &d.subject, &d.body].iter().any(|s| !s.trim().is_empty()) || !d.attachments.is_empty()
}

/// Draw one compose window. Returns true when the user pressed Send.
fn window(c: &mut Composer, accounts: &[(String, String)], ui: &mut Ui) -> bool {
    let mut send = false;
    // On first show, put the cursor in To for new mail and in the body for replies.
    let focus_key = egui::Id::new(("compose_focus", c.id));
    let first_frame = ui.ctx().data_mut(|d| {
        let first = d.get_temp::<bool>(focus_key).is_none();
        d.insert_temp(focus_key, true);
        first
    });
    let focus_to = first_frame && c.draft.to.is_empty();
    let focus_body = first_frame && !c.draft.to.is_empty();

    Panel::top("compose_ribbon").frame(Frame::NONE.fill(RIBBON_BG)).show(ui, |ui| {
        let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
        ui.painter().rect_filled(strip, 0.0, BLUE);
        let tab = egui::Rect::from_min_size(strip.min + vec2(6.0, 2.0), vec2(80.0, 26.0));
        ui.painter().rect_filled(tab, 0.0, RIBBON_BG);
        ui.painter().text(tab.center(), Align2::CENTER_CENTER, "Message", FontId::proportional(13.0), BLUE);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            if super::flat_button(ui, icon::ATTACH, "Attach File").clicked() {
                if let Some(files) = rfd::FileDialog::new().set_title("Insert File").pick_files() {
                    c.draft.attachments.extend(files);
                }
            }
            let bcc_label = if c.show_bcc { "Hide Bcc" } else { "Bcc" };
            if super::flat_button(ui, icon::PEOPLE, bcc_label).clicked() {
                c.show_bcc = !c.show_bcc;
            }
            if super::flat_button(ui, icon::DELETE, "Discard").clicked() {
                if is_dirty(c) {
                    c.confirm_discard = true;
                } else {
                    c.open = false;
                }
            }
        });
        ui.add_space(4.0);
        let y = ui.cursor().top();
        ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, BORDER));
    });

    CentralPanel::default().frame(Frame::NONE.fill(theme::WHITE).inner_margin(Margin::symmetric(12, 10))).show(ui, |ui| {
        if let Some(err) = &c.error {
            Frame::NONE
                .fill(Color32::from_rgb(0xFF, 0xF4, 0xCE))
                .stroke(Stroke::new(1.0, Color32::from_rgb(0xE8, 0xD3, 0x8B)))
                .inner_margin(Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(format!("This message has not been sent. {err}"));
                });
            ui.add_space(6.0);
        }

        ui.horizontal_top(|ui| {
            if send_button(ui).on_hover_text("Send (Ctrl+Enter)").clicked() {
                send = true;
            }
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                let field_w = ui.available_width() - 76.0;
                ui.horizontal(|ui| {
                    field_label(ui, "From");
                    let current = accounts.iter().find(|(id, _)| *id == c.draft.account_id).map(|(_, e)| e.clone()).unwrap_or_default();
                    egui::ComboBox::from_id_salt("from")
                        .selected_text(current)
                        .width(field_w)
                        .show_ui(ui, |ui| {
                            for (id, email) in accounts {
                                ui.selectable_value(&mut c.draft.account_id, id.clone(), email);
                            }
                        });
                });
                let to = address_row(ui, "To...", &mut c.draft.to, field_w);
                if focus_to {
                    to.request_focus();
                }
                address_row(ui, "Cc...", &mut c.draft.cc, field_w);
                if c.show_bcc {
                    address_row(ui, "Bcc...", &mut c.draft.bcc, field_w);
                }
            });
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(84.0);
            field_label(ui, "Subject");
            ui.add(egui::TextEdit::singleline(&mut c.draft.subject).desired_width(ui.available_width()).margin(Margin::symmetric(6, 4)));
        });
        if !c.draft.attachments.is_empty() {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_space(84.0);
                field_label(ui, "Attached");
                let mut remove = None;
                for (i, path) in c.draft.attachments.iter().enumerate() {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let size = std::fs::metadata(path).map(|m| m.len() as usize).unwrap_or(0);
                    if ui.button(format!("{name} ({})  \u{2715}", super::reading::human_size(size))).on_hover_text("Remove").clicked() {
                        remove = Some(i);
                    }
                }
                if let Some(i) = remove {
                    c.draft.attachments.remove(i);
                }
            });
        }
        ui.add_space(8.0);
        ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0, BORDER));
        ui.add_space(8.0);
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let edit = egui::TextEdit::multiline(&mut c.draft.body)
                .frame(egui::Frame::NONE)
                .desired_width(f32::INFINITY)
                .desired_rows(20)
                .font(FontId::proportional(14.5));
            let resp = ui.add_sized(ui.available_size(), edit);
            if focus_body {
                resp.request_focus();
            }
        });
    });

    let ctrl_enter = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Enter);
    let alt_s = egui::KeyboardShortcut::new(egui::Modifiers::ALT, egui::Key::S);
    if ui.input_mut(|i| i.consume_shortcut(&ctrl_enter) || i.consume_shortcut(&alt_s)) {
        send = true;
    }

    if c.confirm_discard {
        let modal = egui::Modal::new(egui::Id::new(("discard", c.id))).show(ui.ctx(), |ui| {
            ui.set_max_width(360.0);
            ui.label(theme::bold("Outlook RS", 15.0));
            ui.add_space(6.0);
            ui.label("Discard this message? Your changes will be lost.");
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("  Cancel  ").clicked() {
                    return Some(false);
                }
                if ui.button("  Discard  ").clicked() {
                    return Some(true);
                }
                None
            })
            .inner
        });
        match modal.inner {
            Some(true) => {
                c.open = false;
                c.confirm_discard = false;
            }
            Some(false) => c.confirm_discard = false,
            None if modal.should_close() => c.confirm_discard = false,
            None => {}
        }
    }
    send
}

fn field_label(ui: &mut Ui, text: &str) {
    ui.add_sized([62.0, 24.0], egui::Label::new(RichText::new(text).color(theme::TEXT)));
}

fn address_row(ui: &mut Ui, label: &str, value: &mut String, width: f32) -> egui::Response {
    ui.horizontal(|ui| {
        let _ = ui.add_sized([62.0, 24.0], egui::Button::new(label).stroke(Stroke::new(1.0, BORDER)));
        ui.add(egui::TextEdit::singleline(value).desired_width(width).margin(Margin::symmetric(6, 4)))
    })
    .inner
}

fn send_button(ui: &mut Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(68.0, 90.0), Sense::click());
    let fill = if resp.hovered() { theme::HOVER } else { theme::WHITE };
    ui.painter().rect(rect, 0.0, fill, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
    ui.painter().text(rect.center() - vec2(0.0, 12.0), Align2::CENTER_CENTER, icon::SEND, FontId::new(26.0, theme::icon_family()), BLUE);
    ui.painter().text(rect.center() + vec2(0.0, 22.0), Align2::CENTER_CENTER, "Send", FontId::proportional(13.5), theme::TEXT);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}
