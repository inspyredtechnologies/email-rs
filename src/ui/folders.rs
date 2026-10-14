//! Folder pane: Favorites, then each account's folder tree with unread counts.

use eframe::egui::{self, Align2, Color32, FontId, RichText, Sense, Ui, vec2};

use crate::app::{Action, App};
use crate::model::{Folder, FolderKind};
use crate::theme::{self, BLUE, HOVER, SELECTED, SUBTLE};

const ROW_HEIGHT: f32 = 24.0;

pub fn show(app: &mut App, ui: &mut Ui) {
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;

        // Favorites mirror the first account's key folders, like a default Outlook profile.
        if let Some(acct) = app.accounts.first() {
            section_header(ui, "Favorites");
            let id = acct.config.id.clone();
            let favs: Vec<Folder> = [FolderKind::Inbox, FolderKind::Sent, FolderKind::Deleted]
                .iter()
                .filter_map(|k| acct.folder_of_kind(*k).cloned())
                .collect();
            for f in favs {
                folder_row(app, ui, &id, &f, 0);
            }
            ui.add_space(8.0);
        }

        let accounts: Vec<(String, String, Vec<Folder>)> = app
            .accounts
            .iter()
            .map(|a| (a.config.id.clone(), a.config.email.clone(), ordered(&a.folders)))
            .collect();
        for (id, email, folders) in accounts {
            section_header(ui, &email);
            if folders.is_empty() {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.label(RichText::new("Updating folders...").color(SUBTLE).italics());
                });
            }
            for f in &folders {
                let depth = if f.kind == FolderKind::Other { f.depth() } else { 0 };
                folder_row(app, ui, &id, f, depth);
            }
            ui.add_space(8.0);
        }
    });
}

/// Outlook order: well-known folders first, then everything else alphabetically
/// (which keeps children right after their parents).
fn ordered(folders: &[Folder]) -> Vec<Folder> {
    let mut special: Vec<Folder> = folders.iter().filter(|f| f.kind != FolderKind::Other).cloned().collect();
    special.sort_by_key(|f| f.kind);
    let mut rest: Vec<Folder> = folders.iter().filter(|f| f.kind == FolderKind::Other).cloned().collect();
    rest.sort_by_key(|f| f.path.to_lowercase());
    special.extend(rest);
    special
}

fn section_header(ui: &mut Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(theme::icon(theme::icon::CHEVRON_DOWN, 9.0).color(SUBTLE));
        ui.label(theme::bold(text, 13.5));
    });
    ui.add_space(2.0);
}

fn folder_row(app: &mut App, ui: &mut Ui, account: &str, f: &Folder, depth: usize) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    let is_current = app.current.as_ref().is_some_and(|(a, p)| a == account && *p == f.path);
    if is_current {
        ui.painter().rect_filled(rect, 0.0, SELECTED);
    } else if resp.hovered() && f.selectable {
        ui.painter().rect_filled(rect, 0.0, HOVER);
    }
    let x = rect.left() + 26.0 + depth as f32 * 14.0;
    let name = f.display_name();
    // Drafts and Junk show the total in [brackets]; others show unread in blue.
    let (badge, bold) = match f.kind {
        FolderKind::Drafts | FolderKind::Junk if f.total > 0 => (format!("[{}]", f.total), false),
        _ if f.unread > 0 => (f.unread.to_string(), true),
        _ => (String::new(), false),
    };
    let font = if bold { FontId::new(13.5, theme::bold_family()) } else { FontId::proportional(13.5) };
    let color = if f.selectable { theme::TEXT } else { SUBTLE };
    let name_rect = ui.painter().text(egui::pos2(x, rect.center().y), Align2::LEFT_CENTER, name, font, color);
    if !badge.is_empty() {
        ui.painter().text(
            egui::pos2(name_rect.right() + 8.0, rect.center().y),
            Align2::LEFT_CENTER,
            badge,
            FontId::new(13.0, theme::bold_family()),
            if bold { BLUE } else { Color32::from_rgb(0x55, 0x55, 0x55) },
        );
    }
    if resp.clicked() && f.selectable {
        app.actions.push(Action::SelectFolder { account: account.to_string(), folder: f.path.clone() });
    }
}
