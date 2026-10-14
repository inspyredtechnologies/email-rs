//! Compose window: each draft opens in its own OS window, like Outlook's
//! Message inspector, with the big Send button beside the address fields.

use eframe::egui::text::LayoutJob;
use eframe::egui::{self, Align2, CentralPanel, Color32, FontFamily, FontId, Frame, Margin, Panel, RichText, Sense, Stroke, TextFormat, Ui, vec2};

use crate::app::{Action, App, Composer};
use crate::rich::{self, Size, Style};
use crate::theme::{self, BLUE, BORDER, RIBBON_BG, SUBTLE, icon};

/// Outlook's standard font colors and highlight colors.
const FONT_COLORS: [(&str, [u8; 3]); 10] = [
    ("Dark Red", [0xC0, 0x00, 0x00]),
    ("Red", [0xFF, 0x00, 0x00]),
    ("Orange", [0xFF, 0xC0, 0x00]),
    ("Yellow", [0xFF, 0xFF, 0x00]),
    ("Light Green", [0x92, 0xD0, 0x50]),
    ("Green", [0x00, 0xB0, 0x50]),
    ("Light Blue", [0x00, 0xB0, 0xF0]),
    ("Blue", [0x00, 0x70, 0xC0]),
    ("Dark Blue", [0x00, 0x20, 0x60]),
    ("Purple", [0x70, 0x30, 0xA0]),
];
const HIGHLIGHTS: [(&str, [u8; 3]); 5] = [
    ("Yellow", [0xFF, 0xFF, 0x00]),
    ("Bright Green", [0x00, 0xFF, 0x00]),
    ("Turquoise", [0x00, 0xFF, 0xFF]),
    ("Pink", [0xFF, 0x00, 0xFF]),
    ("Gray", [0xC0, 0xC0, 0xC0]),
];

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

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
            .with_title(format!("{subject} - Message ({})", if c.draft.plain_text { "Plain Text" } else { "HTML" }))
            .with_inner_size([780.0, 640.0])
            .with_min_inner_size([520.0, 380.0])
            .with_icon(theme::app_icon());
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
            ui.separator();
            // Format Text: HTML or plain text (formatting is kept, just not sent, in plain text).
            ui.label(RichText::new("Format:").color(SUBTLE));
            ui.radio_value(&mut c.draft.plain_text, false, "HTML");
            ui.radio_value(&mut c.draft.plain_text, true, "Plain Text");
        });
        if !c.draft.plain_text {
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                format_toolbar(c, ui);
            });
        }
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
            body_editor(c, ui, focus_body);
            // The quoted original (shown as text; the HTML version is what gets sent).
            if !c.draft.quote_text.is_empty() {
                ui.add(egui::Label::new(RichText::new(c.draft.quote_text.trim_start()).color(SUBTLE).size(13.5)).wrap());
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
            ui.label(theme::bold("E-Mail RS", 15.0));
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

fn text_format(s: &Style) -> TextFormat {
    let size = match s.size {
        Size::Small => 12.0,
        Size::Normal => 14.5,
        Size::Large => 18.5,
        Size::Huge => 24.0,
    };
    let family = if s.bold { theme::bold_family() } else { FontFamily::Proportional };
    let color = s.color.map(rgb).unwrap_or(theme::TEXT);
    let line = |on: bool| if on { Stroke::new(1.0, color) } else { Stroke::NONE };
    TextFormat {
        font_id: FontId::new(size, family),
        color,
        italics: s.italic,
        underline: line(s.underline),
        strikethrough: line(s.strike),
        background: s.highlight.map(rgb).unwrap_or(Color32::TRANSPARENT),
        ..Default::default()
    }
}

/// Lay out `text` with one `Style` per character.
fn styled_job(text: &str, styles: &[Style], typing: Style, wrap_width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    if text.is_empty() {
        job.append("", 0.0, text_format(&typing));
        return job;
    }
    let style_at = |i: usize| styles.get(i).copied().unwrap_or_default();
    let mut run_start = 0; // byte offset
    let mut run_style = style_at(0);
    for (ci, (bi, _)) in text.char_indices().enumerate() {
        let s = style_at(ci);
        if s != run_style {
            job.append(&text[run_start..bi], 0.0, text_format(&run_style));
            run_start = bi;
            run_style = s;
        }
    }
    job.append(&text[run_start..], 0.0, text_format(&run_style));
    job
}

/// The message body: a multiline editor that renders per-character formatting.
fn body_editor(c: &mut Composer, ui: &mut Ui, focus: bool) {
    let char_count = c.draft.body.chars().count();
    c.draft.body_styles.resize(char_count, Style::default());
    let before = c.draft.body.clone();
    let (typing, plain) = (c.typing, c.draft.plain_text);
    let styles = &c.draft.body_styles;
    let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
        let text = buf.as_str();
        let job = if plain {
            styled_job(text, &[], Style::default(), wrap_width)
        } else if text == before {
            styled_job(text, styles, typing, wrap_width)
        } else {
            // Mid-edit this frame: preview the styles the edit will produce.
            styled_job(text, &rich::reconcile(&before, styles, text, typing), typing, wrap_width)
        };
        ui.ctx().fonts_mut(|f| f.layout_job(job))
    };
    let min_height = if c.draft.quote_text.is_empty() { ui.available_height() } else { 220.0 };
    let output = egui::TextEdit::multiline(&mut c.draft.body)
        .id(egui::Id::new(("compose_body", c.id)))
        .frame(Frame::NONE)
        .desired_width(f32::INFINITY)
        .min_size(vec2(ui.available_width(), min_height))
        .layouter(&mut layouter)
        .show(ui);

    let edited = c.draft.body != before;
    if edited {
        c.draft.body_styles = rich::reconcile(&before, &c.draft.body_styles, &c.draft.body, c.typing);
    }
    if output.response.has_focus() {
        if let Some(range) = output.cursor_range {
            let [a, b] = range.sorted_cursors();
            let sel = (a.index.0, b.index.0);
            // Moving the caret (not typing) picks up the formatting of the text before it, like Word.
            if sel != c.selection && sel.0 == sel.1 && !edited {
                c.typing = sel.0.checked_sub(1).and_then(|i| c.draft.body_styles.get(i).copied()).unwrap_or_default();
            }
            c.selection = sel;
        }
        if !plain {
            let shortcut = |key| ui.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, key)));
            if shortcut(egui::Key::B) {
                toggle(c, |s| s.bold, |s, v| s.bold = v);
            }
            if shortcut(egui::Key::I) {
                toggle(c, |s| s.italic, |s, v| s.italic = v);
            }
            if shortcut(egui::Key::U) {
                toggle(c, |s| s.underline, |s, v| s.underline = v);
            }
        }
    }
    if focus || std::mem::take(&mut c.refocus) {
        output.response.request_focus();
    }
}

/// Toggle a on/off attribute on the selection, or on the typing style when nothing is selected.
fn toggle(c: &mut Composer, get: fn(&Style) -> bool, set: fn(&mut Style, bool)) {
    let (a, b) = c.selection;
    if a < b {
        let on = !rich::all(&c.draft.body_styles, a..b, get);
        rich::apply(&mut c.draft.body_styles, a..b, |s| set(s, on));
    } else {
        let on = !get(&c.typing);
        set(&mut c.typing, on);
    }
    c.refocus = true;
}

/// Apply a formatting change to the selection (or to the typing style).
fn restyle(c: &mut Composer, f: impl Fn(&mut Style)) {
    let (a, b) = c.selection;
    if a < b {
        rich::apply(&mut c.draft.body_styles, a..b, &f);
    } else {
        f(&mut c.typing);
    }
    c.refocus = true;
}

fn swatch(ui: &mut Ui, color: Color32, tip: &str) -> bool {
    ui.add(egui::Button::new("").fill(color).min_size(vec2(18.0, 18.0))).on_hover_text(tip).clicked()
}

/// Outlook's Basic Text group: bold/italic/underline/strike, size, color, highlight, lists.
fn format_toolbar(c: &mut Composer, ui: &mut Ui) {
    let (a, b) = c.selection;
    let current = if a < b { c.draft.body_styles.get(a).copied().unwrap_or_default() } else { c.typing };
    let active = |get: fn(&Style) -> bool| if a < b { rich::all(&c.draft.body_styles, a..b, get) } else { get(&c.typing) };
    let (bold, italic, underline, strike) = (active(|s| s.bold), active(|s| s.italic), active(|s| s.underline), active(|s| s.strike));

    ui.spacing_mut().item_spacing.x = 3.0;
    if ui.selectable_label(bold, theme::bold("B", 14.0)).on_hover_text("Bold (Ctrl+B)").clicked() {
        toggle(c, |s| s.bold, |s, v| s.bold = v);
    }
    if ui.selectable_label(italic, RichText::new("I").italics().size(14.0)).on_hover_text("Italic (Ctrl+I)").clicked() {
        toggle(c, |s| s.italic, |s, v| s.italic = v);
    }
    if ui.selectable_label(underline, RichText::new("U").underline().size(14.0)).on_hover_text("Underline (Ctrl+U)").clicked() {
        toggle(c, |s| s.underline, |s, v| s.underline = v);
    }
    if ui.selectable_label(strike, RichText::new("ab").strikethrough().size(14.0)).on_hover_text("Strikethrough").clicked() {
        toggle(c, |s| s.strike, |s, v| s.strike = v);
    }
    ui.separator();

    let mut size = current.size;
    egui::ComboBox::from_id_salt(("font_size", c.id)).width(48.0).selected_text(size.label()).show_ui(ui, |ui| {
        for s in Size::ALL {
            ui.selectable_value(&mut size, s, s.label());
        }
    });
    if size != current.size {
        restyle(c, |s| s.size = size);
    }

    let color_btn = ui.button(RichText::new("A").color(current.color.map(rgb).unwrap_or(theme::TEXT)).strong()).on_hover_text("Font Color");
    egui::Popup::menu(&color_btn).show(|ui| {
        if ui.button("Automatic").clicked() {
            restyle(c, |s| s.color = None);
            ui.close();
        }
        ui.horizontal(|ui| {
            for (name, col) in FONT_COLORS {
                if swatch(ui, rgb(col), name) {
                    restyle(c, |s| s.color = Some(col));
                    ui.close();
                }
            }
        });
    });
    let hl_btn = ui
        .button(RichText::new("ab").background_color(current.highlight.map(rgb).unwrap_or(rgb([0xFF, 0xFF, 0x00]))))
        .on_hover_text("Text Highlight Color");
    egui::Popup::menu(&hl_btn).show(|ui| {
        ui.horizontal(|ui| {
            for (name, col) in HIGHLIGHTS {
                if swatch(ui, rgb(col), name) {
                    restyle(c, |s| s.highlight = Some(col));
                    ui.close();
                }
            }
        });
        if ui.button("No Color").clicked() {
            restyle(c, |s| s.highlight = None);
            ui.close();
        }
    });
    ui.separator();

    for (label, numbered, tip) in [("\u{2022} \u{2261}", false, "Bullets"), ("1. \u{2261}", true, "Numbering")] {
        if ui.button(label).on_hover_text(tip).clicked() {
            rich::toggle_list(&mut c.draft.body, &mut c.draft.body_styles, a..b, numbered);
            c.refocus = true;
        }
    }
    if ui.button("Clear").on_hover_text("Clear All Formatting").clicked() {
        restyle(c, |s| *s = Style::default());
    }
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
