// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! Message boxes, questions, and the busy overlay.

use eframe::egui::{self, Align, Color32, Id, Key, Layout, RichText, Ui};

use super::theme::{bold, Palette};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Information,
    Warning,
    Critical,
}

/// One modal message. `question` ones have Yes/No and carry what Yes does.
pub struct Dialog<A> {
    pub level: Level,
    pub title: String,
    pub text: String,
    /// `Some` for a Yes/No question: what to do on Yes. No is the default.
    pub on_yes: Option<A>,
    /// What to do on No (or Escape) for a question.
    pub on_no: Option<A>,
    /// The Yes and No button labels of a question.
    pub buttons: (&'static str, &'static str),
}

/// The answer to the topmost dialog, when one was given this frame.
pub enum Answer<A> {
    Closed,
    Yes(A),
    No(Option<A>),
}

pub fn show<A>(ui: &mut Ui, palette: &Palette, index: usize, dialog: &mut Dialog<A>) -> Option<Answer<A>> {
    let mut answer = None;
    let question = dialog.on_yes.is_some();
    let modal = egui::Modal::new(Id::unique(("roch-dialog", index))).frame(modal_frame(ui, palette)).show(ui.ctx(), |ui| {
        ui.set_min_width(MIN_WIDTH);
        ui.set_max_width(MAX_WIDTH);
        let accent = match dialog.level {
            Level::Information => palette.text,
            Level::Warning => palette.warning,
            Level::Critical => palette.red,
        };
        ui.label(RichText::new(&dialog.title).font(bold(13.0)).color(accent));
        ui.add_space(6.0);
        // The hairline under the title is painted at the end, once the body has
        // set the dialog's width.
        let rule_y = ui.cursor().top();
        ui.add_space(11.0);
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(&dialog.text).color(palette.text)).wrap().selectable(true));
        });
        ui.add_space(14.0);
        // A row exactly as wide as the text above it and one button high. A bare
        // right-to-left layout here takes every pixel left in the window, which
        // stretched the dialog and floated the buttons in the middle of it.
        let row = egui::vec2(ui.min_rect().width(), ui.spacing().interact_size.y + 4.0);
        ui.allocate_ui_with_layout(row, Layout::right_to_left(Align::Center), |ui| {
            // Right to left: the last button added sits leftmost, so Yes comes
            // before No as it always has.
            if question {
                if button(ui, palette, dialog.buttons.1, false).clicked() {
                    answer = Some(false);
                }
                if button(ui, palette, dialog.buttons.0, true).clicked() {
                    answer = Some(true);
                }
            } else if button(ui, palette, "OK", true).clicked() || ui.input(|input| input.key_pressed(Key::Enter)) {
                answer = Some(false);
            }
        });
        ui.painter().hline(ui.min_rect().x_range(), rule_y, egui::Stroke::new(1.0, palette.border));
    });
    if modal.should_close() && answer.is_none() {
        answer = Some(false);
    }
    if ui.input(|input| input.key_pressed(Key::Escape)) && answer.is_none() {
        answer = Some(false);
    }
    match answer {
        None => None,
        Some(true) => Some(Answer::Yes(dialog.on_yes.take().expect("a question"))),
        Some(false) if question => Some(Answer::No(dialog.on_no.take())),
        Some(false) => Some(Answer::Closed),
    }
}

const MIN_WIDTH: f32 = 360.0;
const MAX_WIDTH: f32 = 560.0;

/// A dialog button. The primary one is filled with the accent, like Import.
fn button(ui: &mut Ui, palette: &Palette, text: &str, primary: bool) -> egui::Response {
    let mut label = RichText::new(text).font(bold(12.0));
    if primary {
        label = label.color(Color32::WHITE);
    }
    let mut button = egui::Button::new(label).min_size(egui::vec2(84.0, 0.0));
    if primary {
        button = button.fill(palette.selected).stroke(egui::Stroke::new(1.0, palette.selected));
    }
    ui.add(button)
}

/// Grey the window out while SCEWIN runs, with what it is doing.
pub fn busy(ui: &mut Ui, palette: &Palette, message: &str) {
    egui::Modal::new(Id::unique("roch-busy")).frame(modal_frame(ui, palette)).show(ui.ctx(), |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(18.0).color(palette.selected));
            ui.label(RichText::new(message).color(palette.text));
        });
    });
    ui.ctx().set_cursor_icon(egui::CursorIcon::Wait);
}

/// The frame every dialog uses: the window color, a hairline border, and room
/// around the contents.
pub fn modal_frame(ui: &Ui, palette: &Palette) -> egui::Frame {
    egui::Frame::popup(ui.style())
        .fill(palette.window)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .inner_margin(egui::Margin::same(16))
        .corner_radius(egui::CornerRadius::same(4))
}
