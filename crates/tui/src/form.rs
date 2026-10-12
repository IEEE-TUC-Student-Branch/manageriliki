//! A small keyboard-driven form used for every dialog in the TUI.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

pub enum FieldKind {
    Text { value: String, masked: bool },
    Choice { options: Vec<String>, idx: usize },
    Toggle(bool),
}

pub struct Field {
    key: &'static str,
    label: &'static str,
    kind: FieldKind,
    /// The field is editable only when every `(key, allowed values)` holds.
    depends: Vec<(&'static str, &'static [&'static str])>,
}

pub enum FormResult {
    Pending,
    Submit,
    Cancel,
}

pub struct Form {
    pub title: String,
    pub hint: String,
    pub error: Option<String>,
    submit: &'static str,
    fields: Vec<Field>,
    focus: usize,
}

impl Form {
    pub fn new(title: impl Into<String>) -> Self {
        Form {
            title: title.into(),
            hint: String::new(),
            error: None,
            submit: "save",
            fields: Vec::new(),
            focus: 0,
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    pub fn submit_label(mut self, label: &'static str) -> Self {
        self.submit = label;
        self
    }

    pub fn text(mut self, key: &'static str, label: &'static str, value: impl Into<String>) -> Self {
        self.push(key, label, FieldKind::Text { value: value.into(), masked: false });
        self
    }

    pub fn secret(mut self, key: &'static str, label: &'static str) -> Self {
        self.push(key, label, FieldKind::Text { value: String::new(), masked: true });
        self
    }

    pub fn choice<S: Into<String>>(
        mut self,
        key: &'static str,
        label: &'static str,
        options: impl IntoIterator<Item = S>,
        selected: &str,
    ) -> Self {
        let options: Vec<String> = options.into_iter().map(Into::into).collect();
        let idx = options.iter().position(|o| o == selected).unwrap_or(0);
        self.push(key, label, FieldKind::Choice { options, idx });
        self
    }

    pub fn toggle(mut self, key: &'static str, label: &'static str, on: bool) -> Self {
        self.push(key, label, FieldKind::Toggle(on));
        self
    }

    /// Makes the most recently added field depend on another field's value.
    pub fn when(mut self, key: &'static str, values: &'static [&'static str]) -> Self {
        if let Some(f) = self.fields.last_mut() {
            f.depends.push((key, values));
        }
        self
    }

    fn push(&mut self, key: &'static str, label: &'static str, kind: FieldKind) {
        self.fields.push(Field { key, label, kind, depends: Vec::new() });
    }

    fn field(&self, key: &str) -> &Field {
        self.fields
            .iter()
            .find(|f| f.key == key)
            .unwrap_or_else(|| panic!("form has no field '{key}'"))
    }

    /// The field's value: text, the selected option, or "on"/"off".
    pub fn get(&self, key: &str) -> String {
        match &self.field(key).kind {
            FieldKind::Text { value, .. } => value.clone(),
            FieldKind::Choice { options, idx } => options[*idx].clone(),
            FieldKind::Toggle(on) => if *on { "on" } else { "off" }.to_string(),
        }
    }

    pub fn on(&self, key: &str) -> bool {
        matches!(self.field(key).kind, FieldKind::Toggle(true))
    }

    fn enabled(&self, idx: usize) -> bool {
        self.fields[idx]
            .depends
            .iter()
            .all(|(key, allowed)| allowed.contains(&self.get(key).as_str()))
    }

    fn move_focus(&mut self, forward: bool) {
        let n = self.fields.len();
        for step in 1..=n {
            let idx = if forward {
                (self.focus + step) % n
            } else {
                (self.focus + n - step) % n
            };
            if self.enabled(idx) {
                self.focus = idx;
                return;
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> FormResult {
        if self.fields.is_empty() {
            return FormResult::Cancel;
        }
        match key.code {
            KeyCode::Esc => return FormResult::Cancel,
            KeyCode::Enter => return FormResult::Submit,
            KeyCode::Tab | KeyCode::Down => self.move_focus(true),
            KeyCode::BackTab | KeyCode::Up => self.move_focus(false),
            code => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match (&mut self.fields[self.focus].kind, code) {
                    (FieldKind::Text { value, .. }, KeyCode::Char('u')) if ctrl => value.clear(),
                    (FieldKind::Text { value, .. }, KeyCode::Char(c)) if !ctrl => value.push(c),
                    (FieldKind::Text { value, .. }, KeyCode::Backspace) => {
                        value.pop();
                    }
                    (FieldKind::Choice { options, idx }, KeyCode::Right | KeyCode::Char(' ')) => {
                        *idx = (*idx + 1) % options.len()
                    }
                    (FieldKind::Choice { options, idx }, KeyCode::Left) => {
                        *idx = (*idx + options.len() - 1) % options.len()
                    }
                    (FieldKind::Toggle(on), KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) => {
                        *on = !*on
                    }
                    _ => {}
                }
            }
        }
        FormResult::Pending
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let label_w = self.fields.iter().map(|f| f.label.len()).max().unwrap_or(0) as u16 + 2;
        let height = self.fields.len() as u16 + 4 + u16::from(self.error.is_some()) * 2;
        let width = 72.min(area.width.saturating_sub(4));
        let popup = centered(area, width, height.min(area.height));
        frame.render_widget(Clear, popup);

        let block = Block::bordered()
            .title(format!(" {} ", self.title).bold())
            .title_bottom(
                Line::from(format!(" Enter {} · Esc cancel · Tab/↑↓ move · ←→/Space change ", self.submit)).dim(),
            )
            .border_style(Style::new().fg(Color::Cyan));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let value_w = (inner.width.saturating_sub(label_w + 1)) as usize;
        let mut lines: Vec<Line> = Vec::new();
        if !self.hint.is_empty() {
            lines.push(Line::from(self.hint.clone()).dim());
        } else {
            lines.push(Line::default());
        }
        for (i, f) in self.fields.iter().enumerate() {
            let focused = i == self.focus;
            let enabled = self.enabled(i);
            let value = match &f.kind {
                FieldKind::Text { value, masked } => {
                    let shown = if *masked { "•".repeat(value.chars().count()) } else { value.clone() };
                    let shown = if focused { format!("{shown}▏") } else { shown };
                    tail(&shown, value_w)
                }
                FieldKind::Choice { options, idx } => format!("‹ {} ›", options[*idx]),
                FieldKind::Toggle(on) => if *on { "[x]" } else { "[ ]" }.to_string(),
            };
            let label_style = if !enabled {
                Style::new().fg(Color::DarkGray)
            } else if focused {
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            let value_style = if !enabled {
                Style::new().fg(Color::DarkGray)
            } else if focused {
                Style::new().bg(Color::DarkGray)
            } else {
                Style::new()
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{:>w$} ", f.label, w = label_w as usize - 1), label_style),
                Span::styled(value, value_style),
            ]));
        }
        if let Some(err) = &self.error {
            lines.push(Line::default());
            lines.push(Line::from(err.clone()).fg(Color::Red));
        }
        // Scroll so the focused field (line 1 + index) stays visible.
        let focus_line = 1 + self.focus as u16;
        let scroll = focus_line.saturating_sub(inner.height.saturating_sub(1));
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll, 0)), inner);
    }
}

/// Keeps the end of `s` (where the cursor is) if it is wider than `width`.
fn tail(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n <= width || width < 2 {
        return s.to_string();
    }
    let rest: String = s.chars().skip(n - (width - 1)).collect();
    format!("…{rest}")
}

pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [_, v, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .areas(area);
    let [_, h, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width),
        Constraint::Fill(1),
    ])
    .areas(v);
    h
}
