//! An egui widget that renders a `Terminal`'s VT100 screen and forwards
//! keyboard input to it.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Event, EventFilter, FontId, Key, Modifiers, Rect, Sense,
    Stroke, StrokeKind, TextFormat, Ui, pos2, text::LayoutJob, vec2,
};
use manageriliki_core::{Terminal, TerminalState, vt100};

const FONT_SIZE: f32 = 14.0;
const DEFAULT_FG: Color32 = Color32::from_rgb(0xd0, 0xd0, 0xd0);
const DEFAULT_BG: Color32 = Color32::from_rgb(0x16, 0x16, 0x1a);

const ANSI: [Color32; 16] = [
    Color32::from_rgb(0x1e, 0x1e, 0x1e),
    Color32::from_rgb(0xcd, 0x31, 0x31),
    Color32::from_rgb(0x0d, 0xbc, 0x79),
    Color32::from_rgb(0xe5, 0xe5, 0x10),
    Color32::from_rgb(0x24, 0x72, 0xc8),
    Color32::from_rgb(0xbc, 0x3f, 0xbc),
    Color32::from_rgb(0x11, 0xa8, 0xcd),
    Color32::from_rgb(0xe5, 0xe5, 0xe5),
    Color32::from_rgb(0x66, 0x66, 0x66),
    Color32::from_rgb(0xf1, 0x4c, 0x4c),
    Color32::from_rgb(0x23, 0xd1, 0x8b),
    Color32::from_rgb(0xf5, 0xf5, 0x43),
    Color32::from_rgb(0x3b, 0x8e, 0xea),
    Color32::from_rgb(0xd6, 0x70, 0xd6),
    Color32::from_rgb(0x29, 0xb8, 0xdb),
    Color32::from_rgb(0xff, 0xff, 0xff),
];

fn indexed(i: u8) -> Color32 {
    match i {
        0..=15 => ANSI[i as usize],
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        _ => {
            let g = 8 + (i - 232) * 10;
            Color32::from_rgb(g, g, g)
        }
    }
}

fn color(c: vt100::Color, default: Color32, bold: bool) -> Color32 {
    match c {
        vt100::Color::Default => default,
        // Classic terminals brighten the 8 base colors when bold.
        vt100::Color::Idx(i) if bold && i < 8 => ANSI[i as usize + 8],
        vt100::Color::Idx(i) => indexed(i),
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

/// Draws the terminal filling the available space and handles its input.
pub fn show(ui: &mut Ui, term: &Terminal, id: egui::Id) {
    let font = FontId::monospace(FONT_SIZE);
    let (cw, ch) = ui
        .ctx()
        .fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));

    let avail = ui.available_size();
    let cols = ((avail.x / cw).floor() as u16).max(10);
    let rows = ((avail.y / ch).floor() as u16).max(2);
    term.resize(rows, cols);

    let (rect, _) = ui.allocate_exact_size(avail, Sense::hover());
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Terminal"));
    if response.clicked() {
        response.request_focus();
    }
    ui.memory_mut(|m| {
        m.interested_in_focus(id, ui.layer_id());
        m.set_focus_lock_filter(
            id,
            EventFilter {
                tab: true,
                horizontal_arrows: true,
                vertical_arrows: true,
                escape: true,
            },
        );
    });
    let focused = ui.memory(|m| m.has_focus(id));
    if focused {
        handle_input(ui, term);
    }
    if response.hovered() {
        let lines: f32 = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::MouseWheel { unit, delta, .. } => Some(match unit {
                        egui::MouseWheelUnit::Point => delta.y / ch,
                        egui::MouseWheelUnit::Line => delta.y,
                        egui::MouseWheelUnit::Page => delta.y * rows as f32,
                    }),
                    _ => None,
                })
                .sum()
        });
        if lines.abs() >= 0.5 {
            term.scroll((lines * 3.0).round() as isize);
        }
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, DEFAULT_BG);
    let origin = rect.min;

    term.with_screen(|screen| {
        let (srows, scols) = screen.size();
        for row in 0..srows {
            let y = origin.y + row as f32 * ch;
            let mut job = LayoutJob::default();
            for col in 0..scols {
                let Some(cell) = screen.cell(row, col) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut fg = color(cell.fgcolor(), DEFAULT_FG, cell.bold());
                let mut bg = color(cell.bgcolor(), DEFAULT_BG, false);
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.dim() {
                    fg = fg.gamma_multiply(0.6);
                }
                if bg != DEFAULT_BG {
                    let w = if cell.is_wide() { 2.0 * cw } else { cw };
                    let x = origin.x + col as f32 * cw;
                    painter.rect_filled(
                        Rect::from_min_size(pos2(x, y), vec2(w, ch)),
                        CornerRadius::ZERO,
                        bg,
                    );
                }
                let text = if cell.has_contents() { cell.contents() } else { " " };
                job.append(
                    text,
                    0.0,
                    TextFormat {
                        font_id: font.clone(),
                        color: fg,
                        italics: cell.italic(),
                        underline: if cell.underline() { Stroke::new(1.0, fg) } else { Stroke::NONE },
                        ..Default::default()
                    },
                );
            }
            let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
            painter.galley(pos2(origin.x, y), galley, DEFAULT_FG);
        }

        if !screen.hide_cursor() && screen.scrollback() == 0 {
            let (r, c) = screen.cursor_position();
            let cursor = Rect::from_min_size(
                pos2(origin.x + c as f32 * cw, origin.y + r as f32 * ch),
                vec2(cw, ch),
            );
            if focused {
                painter.rect_filled(cursor, CornerRadius::ZERO, DEFAULT_FG.gamma_multiply(0.5));
            } else {
                painter.rect_stroke(cursor, CornerRadius::ZERO, Stroke::new(1.0, DEFAULT_FG), StrokeKind::Inside);
            }
        }
        if screen.scrollback() > 0 {
            painter.text(
                rect.right_top() + vec2(-6.0, 4.0),
                Align2::RIGHT_TOP,
                format!("scrollback −{}", screen.scrollback()),
                FontId::proportional(12.0),
                Color32::YELLOW,
            );
        }
    });

    let overlay = match term.state() {
        TerminalState::Connecting => Some(("Connecting…".to_string(), Color32::LIGHT_BLUE)),
        TerminalState::Closed(_) => Some(("Session closed".to_string(), Color32::LIGHT_RED)),
        TerminalState::Connected if !focused => {
            Some(("Click to type".to_string(), Color32::from_gray(140)))
        }
        TerminalState::Connected => None,
    };
    if let Some((text, col)) = overlay {
        painter.text(
            rect.right_bottom() + vec2(-8.0, -6.0),
            Align2::RIGHT_BOTTOM,
            text,
            FontId::proportional(13.0),
            col,
        );
    }
}

fn handle_input(ui: &Ui, term: &Terminal) {
    let app_cursor = term.with_screen(|s| s.application_cursor());
    let bracketed = term.with_screen(|s| s.bracketed_paste());
    let events = ui.input(|i| i.events.clone());
    let mut out: Vec<u8> = Vec::new();
    for event in events {
        match event {
            Event::Text(text) => out.extend_from_slice(text.as_bytes()),
            Event::Paste(text) if bracketed => {
                out.extend_from_slice(format!("\x1b[200~{text}\x1b[201~").as_bytes())
            }
            Event::Paste(text) => out.extend_from_slice(text.replace('\n', "\r").as_bytes()),
            // egui turns Ctrl+C / Ctrl+X into clipboard events; a terminal wants the control codes.
            Event::Copy => out.push(0x03),
            Event::Cut => out.push(0x18),
            Event::Key { key, pressed: true, modifiers, .. } => {
                if let Some(bytes) = encode_key(key, modifiers, app_cursor) {
                    out.extend_from_slice(&bytes);
                }
            }
            _ => {}
        }
    }
    if !out.is_empty() {
        term.scroll(isize::MIN / 2);
        term.write(out);
    }
}

fn encode_key(key: Key, m: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let cursor = |c: char| {
        let prefix = if app_cursor { "\x1bO" } else { "\x1b[" };
        Some(format!("{prefix}{c}").into_bytes())
    };
    let csi = |s: &str| Some(format!("\x1b[{s}").into_bytes());
    // Alt+letter arrives as a key event without text; send it ESC-prefixed (meta).
    let name = key.name();
    if m.alt
        && !m.ctrl
        && let [c @ b'A'..=b'Z'] = name.as_bytes()
    {
        let c = if m.shift { *c } else { c.to_ascii_lowercase() };
        return Some(vec![0x1b, c]);
    }
    let mut bytes = if m.ctrl {
        match name.as_bytes() {
            [c @ b'A'..=b'Z'] => Some(vec![c - b'A' + 1]),
            _ => match key {
                Key::OpenBracket => Some(vec![0x1b]),
                Key::Backslash => Some(vec![0x1c]),
                Key::CloseBracket => Some(vec![0x1d]),
                Key::Space => Some(vec![0]),
                _ => None,
            },
        }
    } else {
        match key {
            Key::Enter => Some(b"\r".to_vec()),
            Key::Backspace => Some(vec![0x7f]),
            Key::Tab if m.shift => csi("Z"),
            Key::Tab => Some(b"\t".to_vec()),
            Key::Escape => Some(vec![0x1b]),
            Key::ArrowUp => cursor('A'),
            Key::ArrowDown => cursor('B'),
            Key::ArrowRight => cursor('C'),
            Key::ArrowLeft => cursor('D'),
            Key::Home => cursor('H'),
            Key::End => cursor('F'),
            Key::Insert => csi("2~"),
            Key::Delete => csi("3~"),
            Key::PageUp => csi("5~"),
            Key::PageDown => csi("6~"),
            Key::F1 => Some(b"\x1bOP".to_vec()),
            Key::F2 => Some(b"\x1bOQ".to_vec()),
            Key::F3 => Some(b"\x1bOR".to_vec()),
            Key::F4 => Some(b"\x1bOS".to_vec()),
            Key::F5 => csi("15~"),
            Key::F6 => csi("17~"),
            Key::F7 => csi("18~"),
            Key::F8 => csi("19~"),
            Key::F9 => csi("20~"),
            Key::F10 => csi("21~"),
            Key::F11 => csi("23~"),
            Key::F12 => csi("24~"),
            _ => None,
        }
    }?;
    if m.alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}
