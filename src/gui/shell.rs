// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! The shared Roch window shell: the 30 px title strip with the theme,
//! minimize and close controls, the 24 px footer with the social links, and
//! edge resizing for the frameless window.

use eframe::egui::{
    self, pos2, vec2, Align, Color32, CursorIcon, Layout, Pos2, Rect, ResizeDirection, Sense, Stroke, TextureHandle,
    Ui, ViewportCommand,
};

use super::theme::{bold, Palette};

pub const TITLE_HEIGHT: f32 = 30.0;
pub const FOOTER_HEIGHT: f32 = 24.0;

pub const SOCIAL_LINKS: [(&str, &str); 3] = [
    ("YouTube", "https://www.youtube.com/@MateoPcTech"),
    ("X", "https://x.com/MateoPCTech"),
    ("Discord", "https://discord.gg/KfzExpKQHB"),
];

/// What the title strip asked for this frame.
#[derive(Default)]
pub struct TitleActions {
    pub toggle_theme: bool,
}

fn caption_button(ui: &mut Ui, width: f32, tooltip: &str, hover: Color32) -> (egui::Response, Rect) {
    let (rect, response) = ui.allocate_exact_size(vec2(width, TITLE_HEIGHT), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, hover);
    }
    (response.on_hover_text(tooltip), rect)
}

/// A solid crescent, the light-mode "switch to dark" glyph.
fn paint_moon(ui: &Ui, center: Pos2, color: Color32, background: Color32) {
    let painter = ui.painter();
    painter.circle_filled(center, 7.0, color);
    painter.circle_filled(center + vec2(4.5, -3.0), 6.0, background);
}

/// A sun, the dark-mode "switch to light" glyph.
fn paint_sun(ui: &Ui, center: Pos2, color: Color32) {
    let painter = ui.painter();
    painter.circle_filled(center, 3.6, color);
    for step in 0..8 {
        let angle = step as f32 * std::f32::consts::TAU / 8.0;
        let direction = vec2(angle.cos(), angle.sin());
        painter.line_segment([center + direction * 5.6, center + direction * 8.0], Stroke::new(1.6, color));
    }
}

pub fn title_bar(ui: &mut Ui, palette: &Palette, title: &str, icon: Option<&TextureHandle>, dark: bool) -> TitleActions {
    let mut actions = TitleActions::default();
    let bar = ui.max_rect();

    // The strip left of the caption buttons moves the window and maximises it
    // on a double click. It must not reach under the buttons: an OS move loop
    // started there would swallow their clicks.
    const CAPTION_WIDTH: f32 = 36.0 + 44.0 + 44.0;
    let strip = Rect::from_min_max(bar.min, pos2(bar.right() - CAPTION_WIDTH, bar.bottom()));
    let drag = ui.interact(strip, ui.scope_id().with("title-drag"), Sense::click_and_drag());
    if drag.double_clicked() {
        let maximized = ui.input(|input| input.viewport().maximized.unwrap_or(false));
        ui.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    } else if drag.drag_started_by(egui::PointerButton::Primary) {
        ui.send_viewport_cmd(ViewportCommand::StartDrag);
    }

    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(10.0);
        let (mark, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
        if let Some(icon) = icon {
            ui.painter().image(icon.id(), mark, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        ui.add_space(10.0);
        ui.label(egui::RichText::new(title).font(bold(12.0)).color(palette.text));

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let (close, rect) = caption_button(ui, 44.0, "Close", Color32::from_rgb(0xc4, 0x2b, 0x1c));
            let color = if close.hovered() { Color32::WHITE } else { palette.text };
            let c = rect.center();
            let stroke = Stroke::new(1.2, color);
            ui.painter().line_segment([c + vec2(-5.0, -5.0), c + vec2(5.0, 5.0)], stroke);
            ui.painter().line_segment([c + vec2(-5.0, 5.0), c + vec2(5.0, -5.0)], stroke);
            if close.clicked() {
                ui.send_viewport_cmd(ViewportCommand::Close);
            }

            let (minimize, rect) = caption_button(ui, 44.0, "Minimize", palette.hover);
            let c = rect.center();
            ui.painter().line_segment([c + vec2(-5.0, 0.0), c + vec2(5.0, 0.0)], Stroke::new(1.2, palette.text));
            if minimize.clicked() {
                ui.send_viewport_cmd(ViewportCommand::Minimized(true));
            }

            let target = if dark { "Switch to light mode" } else { "Switch to dark mode" };
            let (theme, rect) = caption_button(ui, 36.0, target, palette.hover);
            let background = if theme.hovered() { palette.hover } else { palette.window };
            if dark {
                paint_sun(ui, rect.center(), palette.text);
            } else {
                paint_moon(ui, rect.center(), Color32::BLACK, background);
            }
            if theme.clicked() {
                actions.toggle_theme = true;
            }
        });
    });
    actions
}

pub fn footer(ui: &mut Ui, palette: &Palette, message: &str) {
    ui.horizontal_centered(|ui| {
        ui.add_space(10.0);
        ui.spacing_mut().item_spacing.x = 5.0;
        for (index, (name, address)) in SOCIAL_LINKS.iter().enumerate() {
            if index > 0 {
                ui.label(egui::RichText::new("|").color(palette.muted));
            }
            let link = ui
                .add(egui::Label::new(egui::RichText::new(*name).font(bold(12.0)).color(palette.selected)).sense(Sense::click()))
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text(*address);
            if link.clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(*address));
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(10.0);
            ui.add(egui::Label::new(egui::RichText::new(message).color(palette.muted)).truncate())
                .on_hover_text(message);
        });
    });
}

/// Edge and corner resizing for the frameless window.
pub fn resize_edges(ctx: &egui::Context) {
    let (maximized, rect, pointer, pressed) = ctx.input(|input| {
        (
            input.viewport().maximized.unwrap_or(false),
            input.content_rect(),
            input.pointer.hover_pos(),
            input.pointer.primary_pressed(),
        )
    });
    if maximized {
        return;
    }
    let Some(pointer) = pointer else { return };
    const EDGE: f32 = 5.0;
    let left = pointer.x < rect.left() + EDGE;
    let right = pointer.x >= rect.right() - EDGE;
    let top = pointer.y < rect.top() + EDGE;
    let bottom = pointer.y >= rect.bottom() - EDGE;
    let direction = match (left, right, top, bottom) {
        (true, _, true, _) => ResizeDirection::NorthWest,
        (_, true, true, _) => ResizeDirection::NorthEast,
        (true, _, _, true) => ResizeDirection::SouthWest,
        (_, true, _, true) => ResizeDirection::SouthEast,
        (true, ..) => ResizeDirection::West,
        (_, true, ..) => ResizeDirection::East,
        (_, _, true, _) => ResizeDirection::North,
        (_, _, _, true) => ResizeDirection::South,
        _ => return,
    };
    let cursor = match direction {
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::ResizeNwSe,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::ResizeNeSw,
        ResizeDirection::West | ResizeDirection::East => CursorIcon::ResizeHorizontal,
        ResizeDirection::North | ResizeDirection::South => CursorIcon::ResizeVertical,
    };
    ctx.set_cursor_icon(cursor);
    if pressed {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
    }
}
