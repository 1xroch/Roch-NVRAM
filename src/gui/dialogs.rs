// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! Message boxes, questions, and the busy overlay.

use eframe::egui::{self, Align, Id, Key, Layout, RichText, Ui};

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
        ui.set_min_width(360.0);
        ui.set_max_width(640.0);
        let accent = match dialog.level {
            Level::Information => palette.text,
            Level::Warning => palette.warning,
            Level::Critical => palette.red,
        };
        ui.label(RichText::new(&dialog.title).font(bold(13.0)).color(accent));
        ui.add_space(6.0);
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(&dialog.text).color(palette.text)).wrap().selectable(true));
        });
        ui.add_space(10.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if question {
                let no = ui.button("No");
                let yes = ui.button("Yes");
                if yes.clicked() {
                    answer = Some(true);
                }
                if no.clicked() {
                    answer = Some(false);
                }
            } else {
                let ok = ui.button("OK");
                if ok.clicked() || ui.input(|input| input.key_pressed(Key::Enter)) {
                    answer = Some(false);
                }
            }
        });
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
