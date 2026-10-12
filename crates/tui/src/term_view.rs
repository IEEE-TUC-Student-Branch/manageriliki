//! Renders a remote session's VT100 screen and encodes keys for it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use manageriliki_core::{Terminal, TerminalState, vt100};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

pub fn is_detach_key(key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    key.code == KeyCode::F(12) || (ctrl && matches!(key.code, KeyCode::Char(']') | KeyCode::Char('5')))
}

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

pub fn render(frame: &mut Frame, area: Rect, term: &Terminal) {
    let [title_area, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    term.resize(body.height, body.width);

    let state = match term.state() {
        TerminalState::Connecting => "connecting…".to_string(),
        TerminalState::Connected => "connected".to_string(),
        TerminalState::Closed(None) => "closed — press any key".to_string(),
        TerminalState::Closed(Some(e)) => format!("error: {e} — press any key"),
    };
    let scrolled = term.with_screen(|s| s.scrollback());
    let scroll_note = if scrolled > 0 { format!(" [scrollback -{scrolled}]") } else { String::new() };
    frame.render_widget(
        Line::from(format!(
            " {} · {state}{scroll_note} · Ctrl+] or F12 to close · Shift+PgUp/PgDn scroll",
            term.title
        ))
        .style(Style::new().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD)),
        title_area,
    );

    term.with_screen(|screen| {
        let buf = frame.buffer_mut();
        let (rows, cols) = screen.size();
        for row in 0..rows.min(body.height) {
            for col in 0..cols.min(body.width) {
                let Some(cell) = screen.cell(row, col) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let Some(out) = buf.cell_mut((body.x + col, body.y + row)) else { continue };
                let mut style = Style::new().fg(color(cell.fgcolor())).bg(color(cell.bgcolor()));
                if cell.bold() {
                    style = style.bold();
                }
                if cell.dim() {
                    style = style.dim();
                }
                if cell.italic() {
                    style = style.italic();
                }
                if cell.underline() {
                    style = style.underlined();
                }
                if cell.inverse() {
                    style = style.reversed();
                }
                let symbol = if cell.has_contents() { cell.contents() } else { " " };
                out.set_symbol(symbol).set_style(style);
            }
        }
        if !screen.hide_cursor() && screen.scrollback() == 0 {
            let (r, c) = screen.cursor_position();
            frame.set_cursor_position(Position::new(body.x + c, body.y + r));
        }
    });
}

/// Translates a key press into the bytes a terminal would send.
pub fn encode_key(key: &KeyEvent, application_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let csi = |s: &str| Some(format!("\x1b[{s}").into_bytes());
    let cursor = |c: char| {
        if application_cursor {
            Some(format!("\x1bO{c}").into_bytes())
        } else {
            Some(format!("\x1b[{c}").into_bytes())
        }
    };
    let mut bytes = match key.code {
        KeyCode::Char(c) if ctrl => {
            let b = match c {
                'a'..='z' => c as u8 - b'a' + 1,
                'A'..='Z' => c as u8 - b'A' + 1,
                '@' | ' ' | '2' => 0,
                '[' | '3' => 0x1b,
                '\\' | '4' => 0x1c,
                '^' | '6' => 0x1e,
                '_' | '7' | '/' => 0x1f,
                _ => return None,
            };
            Some(vec![b])
        }
        KeyCode::Char(c) => Some(c.to_string().into_bytes()),
        KeyCode::Enter => Some(b"\r".to_vec()),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Tab => Some(b"\t".to_vec()),
        KeyCode::BackTab => csi("Z"),
        KeyCode::Esc => Some(vec![0x1b]),
        KeyCode::Up => cursor('A'),
        KeyCode::Down => cursor('B'),
        KeyCode::Right => cursor('C'),
        KeyCode::Left => cursor('D'),
        KeyCode::Home => cursor('H'),
        KeyCode::End => cursor('F'),
        KeyCode::Insert => csi("2~"),
        KeyCode::Delete => csi("3~"),
        KeyCode::PageUp => csi("5~"),
        KeyCode::PageDown => csi("6~"),
        KeyCode::F(n @ 1..=4) => Some(format!("\x1bO{}", (b'P' + n - 1) as char).into_bytes()),
        KeyCode::F(n) => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                12 => 24,
                _ => return None,
            };
            csi(&format!("{code}~"))
        }
        _ => None,
    }?;
    if alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn encodes_common_keys() {
        let none = KeyModifiers::NONE;
        assert_eq!(encode_key(&key(KeyCode::Char('c'), KeyModifiers::CONTROL), false), Some(vec![3]));
        assert_eq!(encode_key(&key(KeyCode::Up, none), false), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Up, none), true), Some(b"\x1bOA".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Char('x'), KeyModifiers::ALT), false), Some(b"\x1bx".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::F(5), none), false), Some(b"\x1b[15~".to_vec()));
        assert!(is_detach_key(&key(KeyCode::Char(']'), KeyModifiers::CONTROL)));
    }
}
