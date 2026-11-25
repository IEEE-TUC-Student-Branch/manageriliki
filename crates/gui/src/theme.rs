//! The app's dark palette and small styled building blocks shared by every page.

use eframe::egui::{
    self, Color32, CornerRadius, FontId, Margin, RichText, Sense, Stroke, TextStyle, Ui, vec2,
};
use manageriliki_core::{Server, Status};

pub const SIDEBAR: Color32 = Color32::from_rgb(0x0c, 0x0e, 0x11);
pub const BG: Color32 = Color32::from_rgb(0x11, 0x13, 0x17);
pub const SURFACE: Color32 = Color32::from_rgb(0x18, 0x1b, 0x20);
pub const SURFACE_2: Color32 = Color32::from_rgb(0x1e, 0x23, 0x2a);
pub const BORDER: Color32 = Color32::from_rgb(0x26, 0x2a, 0x31);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x2c, 0x31, 0x3a);
pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe8, 0xeb);
pub const SUBTLE: Color32 = Color32::from_rgb(0xb4, 0xba, 0xc3);
pub const MUTED: Color32 = Color32::from_rgb(0x9a, 0xa1, 0xab);
pub const ACCENT: Color32 = Color32::from_rgb(0x5a, 0xa2, 0xff);
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(0xa9, 0xcf, 0xff);
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(0x1d, 0x2a, 0x3a);
pub const ACCENT_BORDER: Color32 = Color32::from_rgb(0x2d, 0x45, 0x60);
pub const SELECTED: Color32 = Color32::from_rgb(0x1b, 0x26, 0x33);
pub const ONLINE: Color32 = Color32::from_rgb(0x3f, 0xb9, 0x50);
pub const ONLINE_TEXT: Color32 = Color32::from_rgb(0x6f, 0xd2, 0x83);
pub const OFFLINE: Color32 = Color32::from_rgb(0xf0, 0x88, 0x3e);
pub const OFFLINE_TEXT: Color32 = Color32::from_rgb(0xf5, 0xa7, 0x6a);
pub const DANGER: Color32 = Color32::from_rgb(0xff, 0x9a, 0x9a);
pub const DANGER_BORDER: Color32 = Color32::from_rgb(0x5a, 0x2a, 0x2a);
pub const SUCCESS: Color32 = ONLINE_TEXT;

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.text_styles.insert(TextStyle::Heading, FontId::proportional(22.0));
        style.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        style.text_styles.insert(TextStyle::Button, FontId::proportional(13.5));
        style.text_styles.insert(TextStyle::Small, FontId::proportional(11.5));
        style.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.0));

        let s = &mut style.spacing;
        s.item_spacing = vec2(8.0, 6.0);
        s.button_padding = vec2(10.0, 5.0);
        s.interact_size.y = 26.0;
        s.window_margin = Margin::same(16);
        s.menu_margin = Margin::same(6);

        let v = &mut style.visuals;
        v.panel_fill = BG;
        v.window_fill = SURFACE;
        v.extreme_bg_color = SIDEBAR;
        v.faint_bg_color = Color32::from_rgb(0x15, 0x18, 0x1c);
        v.code_bg_color = SURFACE_2;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_corner_radius = CornerRadius::same(10);
        v.menu_corner_radius = CornerRadius::same(8);
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = Color32::from_rgb(0x24, 0x49, 0x6f);
        v.selection.stroke = Stroke::new(1.0, ACCENT_TEXT);
        v.error_fg_color = DANGER;
        v.warn_fg_color = OFFLINE_TEXT;

        let radius = CornerRadius::same(6);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = SURFACE;
        w.noninteractive.weak_bg_fill = SURFACE;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.corner_radius = radius;
        w.inactive.bg_fill = Color32::from_rgb(0x23, 0x27, 0x2e);
        w.inactive.weak_bg_fill = SURFACE_2;
        w.inactive.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
        w.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(0xd0, 0xd4, 0xda));
        w.inactive.corner_radius = radius;
        w.hovered.bg_fill = Color32::from_rgb(0x2a, 0x30, 0x39);
        w.hovered.weak_bg_fill = Color32::from_rgb(0x26, 0x2c, 0x35);
        w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x3a, 0x41, 0x4c));
        w.hovered.fg_stroke = Stroke::new(1.5, TEXT);
        w.hovered.corner_radius = radius;
        w.active.bg_fill = Color32::from_rgb(0x2d, 0x34, 0x40);
        w.active.weak_bg_fill = Color32::from_rgb(0x2d, 0x34, 0x40);
        w.active.bg_stroke = Stroke::new(1.0, ACCENT_BORDER);
        w.active.fg_stroke = Stroke::new(1.5, TEXT);
        w.active.corner_radius = radius;
        w.open = w.hovered;
    });
}

/// Dot color, text color and label for a server's reachability.
pub fn status(server: &Server, status: &Status) -> (Option<Color32>, Color32, String) {
    match status {
        Status::Online(ms) if server.ssh.is_some() => (Some(ONLINE), ONLINE_TEXT, format!("online · {ms} ms")),
        Status::Online(_) => (Some(ONLINE), ONLINE_TEXT, "device present".into()),
        Status::Offline(_) => (Some(OFFLINE), OFFLINE_TEXT, "offline".into()),
        Status::Unknown => (None, MUTED, "checking…".into()),
    }
}

/// The address a server is best known by: SSH, else serial, else VNC.
pub fn address(s: &Server) -> String {
    if let Some(c) = &s.ssh {
        format!("{}@{}:{}", c.username, c.host, c.port)
    } else if let Some(c) = &s.serial {
        c.summary()
    } else if let Some(c) = &s.vnc {
        c.summary(None)
    } else {
        "—".into()
    }
}

/// A filled dot for a known status, a hollow one while unknown.
pub fn dot(ui: &mut Ui, color: Option<Color32>) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    match color {
        Some(c) => {
            ui.painter().circle_filled(rect.center(), 4.5, c);
        }
        None => {
            ui.painter().circle_stroke(rect.center(), 4.0, Stroke::new(1.0, MUTED));
        }
    }
    response
}

pub fn primary(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).color(SIDEBAR).strong()).fill(ACCENT).stroke(Stroke::NONE)
}

pub fn soft(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).color(ACCENT_TEXT)).fill(ACCENT_SOFT).stroke(Stroke::new(1.0, ACCENT_BORDER))
}

pub fn ghost(text: &str) -> egui::Button<'static> {
    egui::Button::new(text.to_owned()).fill(Color32::TRANSPARENT).stroke(Stroke::new(1.0, BORDER_STRONG))
}

pub fn danger(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).color(DANGER)).fill(Color32::TRANSPARENT).stroke(Stroke::new(1.0, DANGER_BORDER))
}

/// A small rounded tag, e.g. a group name or a link type.
pub fn chip(ui: &mut Ui, text: &str) {
    egui::Frame::new()
        .fill(SURFACE_2)
        .stroke(Stroke::new(1.0, BORDER_STRONG))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(7, 1))
        .show(ui, |ui| ui.label(RichText::new(text).small().color(SUBTLE)));
}

/// Upper-case section caption.
pub fn caption(text: &str) -> RichText {
    RichText::new(text.to_uppercase()).small().strong().color(SUBTLE)
}

pub fn mono(text: impl Into<String>) -> RichText {
    RichText::new(text).monospace().size(12.0).color(MUTED)
}

pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(16))
}

/// A page title with a muted subtitle line under it.
pub fn page_title(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(RichText::new(title).heading().strong().color(TEXT));
        if !subtitle.is_empty() {
            ui.label(RichText::new(subtitle).color(MUTED));
        }
    });
}

/// A full-width horizontal rule in the border color, filling the rest of a row.
pub fn rule(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().max(0.0), 12.0), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, BORDER));
}

/// Background for a hovered list row.
pub fn faint_hover() -> Color32 {
    Color32::from_rgb(0x1a, 0x1e, 0x24)
}
