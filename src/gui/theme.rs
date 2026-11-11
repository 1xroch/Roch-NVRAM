// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared Roch colors, the Consolas typography, and the saved theme choice.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke, TextStyle, Visuals,
};

use roch_nvram::session::Tone;

pub const BOLD: &str = "bold";

/// One theme's colors. Light mode is white with black text, dark mode black
/// with white text; red accents are shared with the other Roch tools.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub window: Color32,
    pub surface: Color32,
    pub field: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub disabled: Color32,
    pub hover: Color32,
    pub selected: Color32,
    pub red: Color32,
    pub warning_bg: Color32,
    pub warning: Color32,
    /// Table foregrounds for settings that cannot be edited, changed compare
    /// rows, and queued settings.
    pub semantic_muted: Color32,
    pub semantic_warning: Color32,
    pub semantic_queued: Color32,
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub const LIGHT: Palette = Palette {
    window: hex(0xffffff),
    surface: hex(0xffffff),
    field: hex(0xffffff),
    border: hex(0xd0d0d0),
    text: hex(0x000000),
    muted: hex(0x000000),
    disabled: hex(0x62666d),
    hover: hex(0xe6e6e6),
    selected: hex(0xd0343a),
    red: hex(0xb91c1c),
    warning_bg: hex(0xfff1df),
    warning: hex(0x9a4b0c),
    semantic_muted: hex(0x62666d),
    semantic_warning: hex(0xa85a14),
    semantic_queued: hex(0xb91c1c),
};

pub const DARK: Palette = Palette {
    window: hex(0x000000),
    surface: hex(0x000000),
    field: hex(0x000000),
    border: hex(0x383838),
    text: hex(0xffffff),
    muted: hex(0xffffff),
    disabled: hex(0xa0a0a0),
    hover: hex(0x222222),
    selected: hex(0xd0343a),
    red: hex(0xff5a5f),
    warning_bg: hex(0x382719),
    warning: hex(0xf4aa65),
    semantic_muted: hex(0xa0a0a0),
    semantic_warning: hex(0xf4aa65),
    semantic_queued: hex(0xff5a5f),
};

pub fn palette(dark: bool) -> &'static Palette {
    if dark {
        &DARK
    } else {
        &LIGHT
    }
}

impl Palette {
    /// The color of a label with a semantic tone.
    pub fn tone(&self, tone: Tone) -> Color32 {
        match tone {
            Tone::Normal => self.text,
            Tone::Warning => self.warning,
            Tone::Queued => self.red,
            Tone::Muted => self.disabled,
        }
    }
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}

/// Consolas, as Roch Viewer uses, with Segoe UI Symbol behind it for arrows
/// and dashes. Both come with Windows; elsewhere egui's own fonts stand in.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let windows_fonts = std::env::var_os("WINDIR")
        .map(|dir| std::path::PathBuf::from(dir).join("Fonts"))
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows\Fonts"));
    let mut load = |name: &str, file: &str| -> bool {
        match std::fs::read(windows_fonts.join(file)) {
            Ok(bytes) => {
                fonts.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes)));
                true
            }
            Err(_) => false,
        }
    };
    let regular = load("consolas", "consola.ttf");
    let heavy = load("consolas-bold", "consolab.ttf");
    let symbol = load("segoe-symbol", "seguisym.ttf");

    let defaults_mono = fonts.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let mut base: Vec<String> = Vec::new();
    if regular {
        base.push("consolas".into());
    }
    if symbol {
        base.push("segoe-symbol".into());
    }
    base.extend(defaults_mono.iter().cloned());

    let mut heavy_family: Vec<String> = Vec::new();
    if heavy {
        heavy_family.push("consolas-bold".into());
    }
    heavy_family.extend(base.iter().cloned());

    fonts.families.insert(FontFamily::Proportional, base.clone());
    fonts.families.insert(FontFamily::Monospace, base);
    fonts.families.insert(FontFamily::Name(BOLD.into()), heavy_family);
    ctx.set_fonts(fonts);
}

/// Style every widget for the given theme.
pub fn apply(ctx: &egui::Context, dark: bool) {
    let p = palette(dark);
    let mut visuals = if dark { Visuals::dark() } else { Visuals::light() };
    visuals.override_text_color = Some(p.text);
    visuals.window_fill = p.window;
    visuals.panel_fill = p.window;
    visuals.extreme_bg_color = p.field;
    visuals.faint_bg_color = p.surface;
    visuals.code_bg_color = p.surface;
    visuals.window_stroke = Stroke::new(1.0, p.border);
    visuals.window_corner_radius = CornerRadius::same(3);
    visuals.menu_corner_radius = CornerRadius::same(2);
    visuals.hyperlink_color = p.selected;
    visuals.warn_fg_color = p.warning;
    visuals.error_fg_color = p.red;
    visuals.selection.bg_fill = p.selected;
    visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.window_shadow = egui::epaint::Shadow::NONE;
    visuals.popup_shadow = egui::epaint::Shadow::NONE;
    visuals.striped = false;
    visuals.slider_trailing_fill = true;

    let radius = CornerRadius::same(3);
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = p.surface;
    widgets.noninteractive.weak_bg_fill = p.surface;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    for (state, fill, stroke) in [
        (&mut widgets.inactive, p.surface, p.border),
        (&mut widgets.hovered, p.hover, p.muted),
        (&mut widgets.active, p.selected, p.selected),
        (&mut widgets.open, p.hover, p.border),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::new(1.0, stroke);
        state.fg_stroke = Stroke::new(1.0, p.text);
        state.corner_radius = radius;
        state.expansion = 0.0;
    }
    widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    ctx.set_visuals(visuals);

    ctx.all_styles_mut(|style| {
        use FontFamily::Proportional;
        style.text_styles = [
            (TextStyle::Small, FontId::new(10.0, Proportional)),
            (TextStyle::Body, FontId::new(12.0, Proportional)),
            (TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
            (TextStyle::Button, FontId::new(12.0, Proportional)),
            (TextStyle::Heading, bold(13.0)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(6.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 3.0);
        style.spacing.interact_size.y = 27.0;
        style.spacing.combo_height = 300.0;
        style.spacing.window_margin = Margin::same(12);
        style.spacing.menu_margin = Margin::same(4);
        style.spacing.text_edit_width = 220.0;
    });
}

// ----- the saved choice ------------------------------------------------------

/// Where the original Qt build kept its settings, so a saved choice carries
/// over: `HKCU\Software\Roch Studio\Roch NVRAM`, value `darkMode`.
#[cfg(windows)]
const SETTINGS_KEY: &str = r"Software\Roch Studio\Roch NVRAM";

/// Fresh installs start light; a stored choice remains authoritative.
pub fn load_dark_mode() -> bool {
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(SETTINGS_KEY) else {
            return false;
        };
        if let Ok(text) = key.get_value::<String, _>("darkMode") {
            return matches!(text.trim().to_ascii_lowercase().as_str(), "true" | "1");
        }
        if let Ok(number) = key.get_value::<u32, _>("darkMode") {
            return number != 0;
        }
        false
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Store the choice where Qt's QSettings would have: as the string
/// "true"/"false", leaving every other value under the key alone.
pub fn save_dark_mode(dark: bool) {
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        if let Ok((key, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(SETTINGS_KEY) {
            let _ = key.set_value("darkMode", &if dark { "true" } else { "false" });
        }
    }
    #[cfg(not(windows))]
    let _ = dark;
}

/// Give the native caption the theme's colors. The window draws its own title
/// bar, so this only matters for the frame Windows 11 draws around it.
pub fn sync_title_bar(frame: &eframe::Frame, dark: bool) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = frame.window_handle() else { return };
        let RawWindowHandle::Win32(handle) = handle.as_raw() else { return };
        let hwnd = handle.hwnd.get() as *mut core::ffi::c_void;
        dwm::set(hwnd, dark);
    }
    #[cfg(not(windows))]
    let _ = (frame, dark);
}

#[cfg(windows)]
mod dwm {
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(hwnd: *mut core::ffi::c_void, attribute: u32, value: *const core::ffi::c_void, size: u32) -> i32;
    }

    fn colorref(rgb: u32) -> u32 {
        let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
        r | (g << 8) | (b << 16)
    }

    pub fn set(hwnd: *mut core::ffi::c_void, dark: bool) {
        let enabled: i32 = i32::from(dark);
        let caption = colorref(if dark { 0x000000 } else { 0xffffff });
        let border = colorref(if dark { 0x383838 } else { 0xd0d0d0 });
        let size = std::mem::size_of::<u32>() as u32;
        // SAFETY: hwnd is this process's live top-level window, and every
        // value points at a local of the size passed. Unsupported attributes
        // fail with an error code, which is ignored.
        unsafe {
            // 20 is supported by current Windows; older Windows 10 builds use 19.
            if DwmSetWindowAttribute(hwnd, 20, (&raw const enabled).cast(), size) != 0 {
                DwmSetWindowAttribute(hwnd, 19, (&raw const enabled).cast(), size);
            }
            DwmSetWindowAttribute(hwnd, 34, (&raw const border).cast(), size);
            DwmSetWindowAttribute(hwnd, 35, (&raw const caption).cast(), size);
        }
    }
}
