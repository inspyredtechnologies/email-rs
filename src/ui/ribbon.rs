//! The Office ribbon: a blue tab strip over grouped large/small command buttons.

use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Stroke, Ui, vec2};

use crate::app::{Action, App, RibbonTab};
use crate::mime::ReplyKind;
use crate::model::FolderKind;
use crate::theme::{self, BLUE, BORDER, RIBBON_BG, SUBTLE, icon};

const BODY_HEIGHT: f32 = 92.0;

pub fn show(app: &mut App, ui: &mut Ui) {
    tab_strip(app, ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), BODY_HEIGHT), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, RIBBON_BG);
    ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, BORDER));
    let mut body = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(6.0, 4.0))).layout(egui::Layout::left_to_right(egui::Align::Min)));
    match app.ribbon_tab {
        RibbonTab::Home => home(app, &mut body),
        RibbonTab::SendReceive => send_receive(app, &mut body),
        RibbonTab::Folder => folder(app, &mut body),
        RibbonTab::View => view(app, &mut body),
    }
}

fn tab_strip(app: &mut App, ui: &mut Ui) {
    let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    ui.painter().rect_filled(strip, 0.0, BLUE);
    let mut x = strip.left() + 4.0;
    let tabs: [(&str, Option<RibbonTab>); 5] = [
        ("File", None),
        ("Home", Some(RibbonTab::Home)),
        ("Send / Receive", Some(RibbonTab::SendReceive)),
        ("Folder", Some(RibbonTab::Folder)),
        ("View", Some(RibbonTab::View)),
    ];
    for (label, tab) in tabs {
        let galley = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(13.0), Color32::WHITE);
        let w = galley.size().x + 24.0;
        let rect = Rect::from_min_size(egui::pos2(x, strip.top() + 2.0), vec2(w, strip.height() - 2.0));
        let resp = ui.interact(rect, ui.id().with(("ribbon_tab", label)), Sense::click());
        let selected = tab.is_some_and(|t| t == app.ribbon_tab);
        let (fill, color) = if selected {
            (RIBBON_BG, BLUE)
        } else if resp.hovered() {
            (theme::BLUE_DARK, Color32::WHITE)
        } else {
            (Color32::TRANSPARENT, Color32::WHITE)
        };
        ui.painter().rect_filled(rect, 0.0, fill);
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(13.0), color);
        if resp.clicked() {
            match tab {
                Some(t) => app.ribbon_tab = t,
                None => app.backstage = true,
            }
        }
        x += w;
    }
}

/// A ribbon group: content on top, caption underneath, separator on the right.
fn group(ui: &mut Ui, caption: &str, add: impl FnOnce(&mut Ui)) {
    let top = ui.max_rect().top();
    let inner = ui.vertical(|ui| {
        ui.set_height(BODY_HEIGHT - 8.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.set_min_height(66.0);
            add(ui);
        });
    });
    let r = inner.response.rect;
    ui.painter().text(
        egui::pos2(r.center().x, top + BODY_HEIGHT - 12.0),
        Align2::CENTER_CENTER,
        caption,
        FontId::proportional(11.5),
        SUBTLE,
    );
    ui.add_space(4.0);
    let x = ui.cursor().left();
    ui.painter().vline(x, (top + 2.0)..=(top + BODY_HEIGHT - 10.0), Stroke::new(1.0, BORDER));
    ui.add_space(5.0);
}

/// Large ribbon button: 32px icon over a (possibly two-line) caption.
fn large(ui: &mut Ui, glyph: char, label: &str, enabled: bool) -> egui::Response {
    let font = FontId::proportional(12.5);
    let text_w = ui.painter().layout_no_wrap(label.to_string(), font.clone(), theme::TEXT).size().x;
    let width = (text_w + 12.0).clamp(48.0, 92.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 66.0), if enabled { Sense::click() } else { Sense::hover() });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if enabled && resp.hovered() {
            painter.rect(rect, 0.0, theme::HOVER, Stroke::new(1.0, Color32::from_rgb(0xA0, 0xC8, 0xE8)), egui::StrokeKind::Inside);
        }
        let (icon_c, text_c) = if enabled { (BLUE, theme::TEXT) } else { (Color32::from_gray(180), Color32::from_gray(160)) };
        painter.text(rect.center_top() + vec2(0.0, 20.0), Align2::CENTER_CENTER, glyph, FontId::new(26.0, theme::icon_family()), icon_c);
        let galley = painter.layout(label.to_string(), font, text_c, width - 4.0);
        let pos = egui::pos2(rect.center().x - galley.size().x / 2.0, rect.top() + 38.0);
        painter.galley(pos, galley, text_c);
    }
    resp.on_hover_cursor(if enabled { egui::CursorIcon::PointingHand } else { egui::CursorIcon::Default })
}

fn small(ui: &mut Ui, glyph: char, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| super::flat_button(ui, glyph, label)).inner
}

fn home(app: &mut App, ui: &mut Ui) {
    let has_sel = app.selected.is_some();
    let has_acct = app.current.is_some();
    let a = &mut app.actions;

    group(ui, "New", |ui| {
        if large(ui, icon::MAIL, "New Email", has_acct).on_hover_text("New Email (Ctrl+N)").clicked() {
            a.push(Action::NewMail);
        }
    });
    group(ui, "Delete", |ui| {
        if large(ui, icon::DELETE, "Delete", has_sel).on_hover_text("Delete (Del)").clicked() {
            a.push(Action::Delete);
        }
        if large(ui, icon::ARCHIVE, "Archive", has_sel).clicked() {
            a.push(Action::Archive);
        }
    });
    group(ui, "Respond", |ui| {
        if large(ui, icon::REPLY, "Reply", has_sel).on_hover_text("Reply (Ctrl+R)").clicked() {
            a.push(Action::Respond(ReplyKind::Reply));
        }
        if large(ui, icon::REPLY_ALL, "Reply All", has_sel).on_hover_text("Reply All (Ctrl+Shift+R)").clicked() {
            a.push(Action::Respond(ReplyKind::ReplyAll));
        }
        if large(ui, icon::FORWARD, "Forward", has_sel).on_hover_text("Forward (Ctrl+F)").clicked() {
            a.push(Action::Respond(ReplyKind::Forward));
        }
    });

    // Destination folders for the Move menu.
    let current = app.current.clone();
    let targets: Vec<(String, String, usize)> = app
        .current_account()
        .map(|acct| {
            acct.folders
                .iter()
                .filter(|f| f.selectable && current.as_ref().is_none_or(|(_, p)| *p != f.path))
                .map(|f| (f.path.clone(), f.display_name(), if f.kind == FolderKind::Other { f.depth() } else { 0 }))
                .collect()
        })
        .unwrap_or_default();
    let unread_sel = app.selected_meta().map(|m| !m.seen);
    let flagged_sel = app.selected_meta().map(|m| (m.uid, m.flagged));
    let a = &mut app.actions;

    group(ui, "Move", |ui| {
        let resp = large(ui, icon::MOVE, "Move", has_sel);
        egui::Popup::menu(&resp).show(|ui| {
            ui.set_min_width(180.0);
            for (path, name, depth) in &targets {
                if ui.button(format!("{}{name}", "    ".repeat(*depth))).clicked() {
                    a.push(Action::MoveTo(path.clone()));
                    ui.close();
                }
            }
        });
    });
    group(ui, "Tags", |ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let label = if unread_sel == Some(true) { "Read" } else { "Unread/ Read" };
            if small(ui, icon::READ, label, has_sel).on_hover_text("Ctrl+Q read, Ctrl+U unread").clicked() {
                a.push(Action::SetRead(unread_sel == Some(true)));
            }
            let follow = if flagged_sel.is_some_and(|(_, f)| f) { "Clear Flag" } else { "Follow Up" };
            if small(ui, icon::FLAG, follow, has_sel).on_hover_text("Insert").clicked() {
                if let Some((uid, _)) = flagged_sel {
                    a.push(Action::ToggleFlag(uid));
                }
            }
        });
    });
    let unread_only = &mut app.unread_only;
    group(ui, "Find", |ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            if small(ui, icon::SEARCH, "Search", true).on_hover_text("Ctrl+E").clicked() {
                app.focus_search = true;
            }
            let resp = small(ui, icon::CHEVRON_DOWN, "Filter Email", true);
            egui::Popup::menu(&resp).show(|ui| {
                ui.checkbox(unread_only, "Unread");
                if ui.button("Clear Filter").clicked() {
                    *unread_only = false;
                    ui.close();
                }
            });
        });
    });
}

fn send_receive(app: &mut App, ui: &mut Ui) {
    group(ui, "Send & Receive", |ui| {
        if large(ui, icon::SYNC, "Send/Receive All Folders", !app.accounts.is_empty()).on_hover_text("F9").clicked() {
            app.actions.push(Action::SendReceive);
        }
        if large(ui, icon::SYNC, "Update Folder", app.current.is_some()).clicked() {
            app.actions.push(Action::UpdateFolder);
        }
    });
}

fn folder(app: &mut App, ui: &mut Ui) {
    let has_unread = app.current_folder().is_some_and(|f| f.unread > 0);
    group(ui, "Clean Up", |ui| {
        if large(ui, icon::READ, "Mark All as Read", has_unread).clicked() {
            app.actions.push(Action::MarkAllRead);
        }
    });
    group(ui, "Actions", |ui| {
        if large(ui, icon::SYNC, "Update Folder", app.current.is_some()).clicked() {
            app.actions.push(Action::UpdateFolder);
        }
    });
}

fn view(app: &mut App, ui: &mut Ui) {
    group(ui, "Layout", |ui| {
        let resp = large(ui, icon::MAIL, "Folder Pane", true);
        let folder_pane = &mut app.show_folder_pane;
        egui::Popup::menu(&resp).show(|ui| {
            ui.radio_value(folder_pane, true, "Normal");
            ui.radio_value(folder_pane, false, "Off");
        });
        let resp = large(ui, icon::READ, "Reading Pane", true);
        let reading = &mut app.show_reading_pane;
        egui::Popup::menu(&resp).show(|ui| {
            ui.radio_value(reading, true, "Right");
            ui.radio_value(reading, false, "Off");
        });
    });
}
