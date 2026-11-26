//! Full-screen view of a server's containers (Docker, Podman, LXC,
//! Kubernetes) and the services of the selected one.

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use manageriliki_core::{
    Container, ContainerAction, ContainerList, ContainerService, Identity, Manager, Result as CoreResult, Runtime,
    Server,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};

use crate::form::centered;

/// Wait this long after the selection settles before fetching its services.
const SELECT_DELAY: Duration = Duration::from_millis(350);

pub enum Outcome {
    Stay,
    Close,
    Shell(Container),
}

fn spawn<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx
}

fn same(a: &Container, b: &Container) -> bool {
    a.runtime == b.runtime && a.namespace == b.namespace && a.id == b.id && a.backend() == b.backend()
}

struct Logs {
    title: String,
    text: Option<String>,
    scroll: u16,
}

pub struct ContainersScreen {
    mgr: Manager,
    who: Identity,
    pub server: Server,
    list: Option<CoreResult<ContainerList>>,
    list_rx: Option<Receiver<CoreResult<ContainerList>>>,
    runtime: Option<Runtime>,
    table: TableState,
    /// Container the services belong to, and when the selection last changed.
    services_for: Option<Container>,
    selected_at: Option<Instant>,
    services: Option<CoreResult<Vec<ContainerService>>>,
    services_rx: Option<Receiver<(Container, CoreResult<Vec<ContainerService>>)>>,
    services_scroll: usize,
    action_rx: Option<Receiver<(String, CoreResult<()>)>>,
    logs: Option<Logs>,
    logs_rx: Option<Receiver<CoreResult<String>>>,
    confirm: Option<(Container, ContainerAction)>,
    flash: Option<(String, bool)>,
}

impl ContainersScreen {
    pub fn new(mgr: Manager, who: Identity, server: Server) -> Self {
        let mut screen = ContainersScreen {
            mgr,
            who,
            server,
            list: None,
            list_rx: None,
            runtime: None,
            table: TableState::default().with_selected(0),
            services_for: None,
            selected_at: None,
            services: None,
            services_rx: None,
            services_scroll: 0,
            action_rx: None,
            logs: None,
            logs_rx: None,
            confirm: None,
            flash: None,
        };
        screen.refresh();
        screen
    }

    fn refresh(&mut self) {
        let (mgr, who, id) = (self.mgr.clone(), self.who.clone(), self.server.id);
        self.list_rx = Some(spawn(move || mgr.list_containers(&who, id)));
    }

    fn visible(&self) -> Vec<&Container> {
        match &self.list {
            Some(Ok(list)) => list.containers.iter().filter(|c| self.runtime.is_none_or(|r| c.runtime == r)).collect(),
            _ => Vec::new(),
        }
    }

    fn selected(&self) -> Option<Container> {
        self.table.selected().and_then(|i| self.visible().get(i).map(|c| (*c).clone()))
    }

    fn selection_changed(&mut self) {
        self.selected_at = Some(Instant::now());
    }

    fn load_services(&mut self, c: Container) {
        let (mgr, who, id) = (self.mgr.clone(), self.who.clone(), self.server.id);
        self.services_for = Some(c.clone());
        self.services = None;
        self.services_scroll = 0;
        self.services_rx = Some(spawn(move || {
            let r = mgr.container_services(&who, id, &c);
            (c, r)
        }));
    }

    pub fn tick(&mut self) {
        if let Some(rx) = &self.list_rx
            && let Ok(result) = rx.try_recv()
        {
            self.list_rx = None;
            self.list = Some(result);
            let len = self.visible().len();
            match (self.table.selected(), len) {
                (_, 0) => self.table.select(None),
                (Some(i), n) if i >= n => self.table.select(Some(n - 1)),
                (None, _) => self.table.select(Some(0)),
                _ => {}
            }
            // Reload services for the selection (it may have changed state).
            self.services_for = None;
            self.selection_changed();
        }

        if let Some(at) = self.selected_at
            && at.elapsed() >= SELECT_DELAY
        {
            self.selected_at = None;
            match self.selected() {
                Some(c) if !self.services_for.as_ref().is_some_and(|s| same(s, &c)) => self.load_services(c),
                Some(_) => {}
                None => {
                    self.services_for = None;
                    self.services = None;
                }
            }
        }

        if let Some(rx) = &self.services_rx
            && let Ok((c, result)) = rx.try_recv()
        {
            self.services_rx = None;
            if self.services_for.as_ref().is_some_and(|s| same(s, &c)) {
                self.services = Some(result);
            }
        }

        if let Some(rx) = &self.action_rx
            && let Ok((label, result)) = rx.try_recv()
        {
            self.action_rx = None;
            self.flash = Some(match result {
                Ok(()) => (format!("{label}: done"), false),
                Err(e) => (e.to_string(), true),
            });
            self.refresh();
        }

        if let Some(rx) = &self.logs_rx
            && let Ok(result) = rx.try_recv()
        {
            self.logs_rx = None;
            if let Some(logs) = &mut self.logs {
                let text = match result {
                    Ok(t) if t.trim().is_empty() => "(no output)".to_string(),
                    Ok(t) => t,
                    Err(e) => format!("failed: {e}"),
                };
                // Start at the end, where the newest lines are.
                logs.scroll = (text.lines().count() as u16).saturating_sub(10);
                logs.text = Some(text);
            }
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        if let Some(logs) = &mut self.logs {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => logs.scroll = logs.scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => logs.scroll += 1,
                KeyCode::PageUp => logs.scroll = logs.scroll.saturating_sub(15),
                KeyCode::PageDown => logs.scroll += 15,
                KeyCode::Home | KeyCode::Char('g') => logs.scroll = 0,
                _ => {
                    self.logs = None;
                    self.logs_rx = None;
                }
            }
            return Outcome::Stay;
        }
        if let Some((c, action)) = self.confirm.take() {
            if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                self.run_action(c, action);
            }
            return Outcome::Stay;
        }
        self.flash = None;

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Close,
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::PageUp => self.services_scroll = self.services_scroll.saturating_sub(5),
            KeyCode::PageDown => self.services_scroll += 5,
            KeyCode::Char('r') => {
                self.refresh();
                if let Some(c) = self.selected() {
                    self.load_services(c);
                }
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let runtimes = match &self.list {
                    Some(Ok(list)) => list.runtimes.clone(),
                    _ => Vec::new(),
                };
                let mut choices: Vec<Option<Runtime>> = vec![None];
                choices.extend(runtimes.into_iter().map(Some));
                let pos = choices.iter().position(|c| *c == self.runtime).unwrap_or(0);
                let next = if key.code == KeyCode::Tab {
                    (pos + 1) % choices.len()
                } else {
                    (pos + choices.len() - 1) % choices.len()
                };
                self.runtime = choices[next];
                self.table.select(if self.visible().is_empty() { None } else { Some(0) });
                self.selection_changed();
            }
            _ => {
                let Some(c) = self.selected() else { return Outcome::Stay };
                match key.code {
                    KeyCode::Enter | KeyCode::Char('l') => self.open_logs(c),
                    KeyCode::Char('s') if c.running => return Outcome::Shell(c),
                    KeyCode::Char('s') => self.flash = Some((format!("{} is not running", c.display_name()), true)),
                    KeyCode::Char('u') => self.request(c, ContainerAction::Start),
                    KeyCode::Char('x') => self.request(c, ContainerAction::Stop),
                    KeyCode::Char('R') => self.request(c, ContainerAction::Restart),
                    _ => {}
                }
            }
        }
        Outcome::Stay
    }

    pub fn show_error(&mut self, e: impl ToString) {
        self.flash = Some((e.to_string(), true));
    }

    fn step(&mut self, delta: i32) {
        let len = self.visible().len();
        if len > 0 {
            let cur = self.table.selected().unwrap_or(0) as i32;
            self.table.select(Some((cur + delta).clamp(0, len as i32 - 1) as usize));
            self.selection_changed();
        }
    }

    fn request(&mut self, c: Container, action: ContainerAction) {
        if !c.supports(action) {
            self.flash = Some(("pods can only be deleted (R); their controller recreates them".into(), true));
        } else if self.action_rx.is_some() {
            self.flash = Some(("another action is still running".into(), true));
        } else if action == ContainerAction::Start {
            self.run_action(c, action);
        } else {
            self.confirm = Some((c, action));
        }
    }

    fn run_action(&mut self, c: Container, action: ContainerAction) {
        let (mgr, who, id) = (self.mgr.clone(), self.who.clone(), self.server.id);
        let label = format!("{} {}", c.action_label(action).to_lowercase(), c.display_name());
        self.flash = Some((format!("{label}…"), false));
        self.action_rx = Some(spawn(move || {
            let r = mgr.container_action(&who, id, &c, action);
            (label, r)
        }));
    }

    fn open_logs(&mut self, c: Container) {
        let (mgr, who, id) = (self.mgr.clone(), self.who.clone(), self.server.id);
        self.logs = Some(Logs { title: format!("logs: {}", c.display_name()), text: None, scroll: 0 });
        self.logs_rx = Some(spawn(move || mgr.container_logs(&who, id, &c)));
    }

    // ---- drawing ----

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let [header, list_area, services_area, footer] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Fill(1), Constraint::Length(1)])
                .areas(area);

        let mut head = vec![format!(" Containers on {} ", self.server.name).bold().cyan()];
        if let Some(Ok(list)) = &self.list {
            let mut push = |label: String, on: bool| {
                head.push(" ".into());
                head.push(if on { label.reversed().cyan() } else { label.into() });
            };
            push(format!(" All {} ", list.containers.len()), self.runtime.is_none());
            for r in &list.runtimes {
                let n = list.containers.iter().filter(|c| c.runtime == *r).count();
                push(format!(" {r} {n} "), self.runtime == Some(*r));
            }
        }
        if self.list_rx.is_some() {
            head.push("  loading…".dim());
        }
        frame.render_widget(Line::from(head), header);

        self.draw_list(frame, list_area);
        self.draw_services(frame, services_area);

        let footer_line = match (&self.confirm, &self.flash) {
            (Some((c, action)), _) => {
                Line::from(format!(" {} {}? y = yes, any other key cancels", c.action_label(*action), c.display_name()))
                    .fg(Color::Yellow)
            }
            (None, Some((msg, true))) => Line::from(format!(" {msg}")).fg(Color::Red),
            (None, Some((msg, false))) => Line::from(format!(" {msg}")).fg(Color::Green),
            (None, None) => Line::from(
                " ↑↓ select · Tab runtime · Enter/l logs · s shell · u start · x stop · R restart · PgUp/PgDn services · r refresh · Esc back",
            )
            .dim(),
        };
        frame.render_widget(footer_line, footer);

        if let Some(logs) = &self.logs {
            let popup = centered(area, area.width.saturating_sub(4), area.height.saturating_sub(2));
            frame.render_widget(Clear, popup);
            frame.render_widget(
                Paragraph::new(logs.text.clone().unwrap_or_else(|| "loading…".into())).scroll((logs.scroll, 0)).block(
                    Block::bordered()
                        .title(format!(" {} ", logs.title))
                        .title_bottom(Line::from(" ↑↓/PgUp/PgDn scroll · g top · any other key closes ").dim())
                        .border_style(Style::new().fg(Color::Cyan)),
                ),
                popup,
            );
        }
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(" Containers ");
        let list = match &self.list {
            Some(Ok(list)) => list,
            Some(Err(e)) => {
                frame.render_widget(Paragraph::new(e.to_string()).red().wrap(Wrap { trim: true }).block(block), area);
                return;
            }
            None => {
                frame.render_widget(
                    Paragraph::new("Looking for Docker, Podman, LXC and Kubernetes…").dim().block(block),
                    area,
                );
                return;
            }
        };
        if list.runtimes.is_empty() {
            frame.render_widget(
                Paragraph::new(
                    "No container runtime found (looked for docker, podman, incus, lxc, pct, lxc-ls, kubectl, k3s and microk8s).",
                )
                .dim()
                .wrap(Wrap { trim: true })
                .block(block),
                area,
            );
            return;
        }

        let warnings: Vec<Line> = list.warnings.iter().map(|w| Line::from(format!("⚠ {w}")).yellow()).collect();
        let [table_area, warn_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(warnings.len() as u16)]).areas(area);
        let rows: Vec<Row> = self
            .visible()
            .into_iter()
            .map(|c| {
                let color = if c.running {
                    Color::Green
                } else if matches!(c.state.as_str(), "exited" | "stopped" | "created") {
                    Color::DarkGray
                } else {
                    Color::Red
                };
                Row::new(vec![
                    Cell::from("●").fg(color),
                    Cell::from(c.display_name()).bold(),
                    Cell::from(c.runtime.label()),
                    Cell::from(c.image.clone()),
                    Cell::from(c.state.clone()),
                    Cell::from(c.status.clone()),
                    Cell::from(c.ports.clone()),
                ])
            })
            .collect();
        let count = rows.len();
        let table = Table::new(
            rows,
            [
                Constraint::Length(2),
                Constraint::Fill(3),
                Constraint::Length(10),
                Constraint::Fill(3),
                Constraint::Length(12),
                Constraint::Fill(2),
                Constraint::Fill(3),
            ],
        )
        .header(Row::new(["", "Name", "Runtime", "Image", "State", "Status", "Ports / addresses"]).bold().underlined())
        .row_highlight_style(Style::new().bg(Color::DarkGray))
        .block(Block::bordered().title(format!(" Containers ({count}) ")));
        frame.render_stateful_widget(table, table_area, &mut self.table);
        frame.render_widget(Paragraph::new(warnings), warn_area);
    }

    fn draw_services(&mut self, frame: &mut Frame, area: Rect) {
        let Some(c) = self.services_for.clone().or_else(|| self.selected()) else {
            frame.render_widget(Block::bordered().title(" Services "), area);
            return;
        };
        let title = format!(" Services of {} ({} via {}) ", c.display_name(), c.runtime, c.backend());
        let block = Block::bordered().title(title);
        let services = match &self.services {
            Some(Ok(s)) if s.is_empty() => {
                let msg = if c.running { "No services found." } else { "Not running." };
                frame.render_widget(Paragraph::new(msg).dim().block(block), area);
                return;
            }
            Some(Ok(s)) => s,
            Some(Err(e)) => {
                frame.render_widget(Paragraph::new(e.to_string()).red().wrap(Wrap { trim: true }).block(block), area);
                return;
            }
            None => {
                frame.render_widget(Paragraph::new("loading…").dim().block(block), area);
                return;
            }
        };
        self.services_scroll = self.services_scroll.min(services.len().saturating_sub(1));
        let rows = services.iter().skip(self.services_scroll).map(|s| {
            Row::new(vec![
                Cell::from("●").fg(if s.ok { Color::Green } else { Color::Red }),
                Cell::from(s.kind.label()).dim(),
                Cell::from(s.name.clone()).bold(),
                Cell::from(s.state.clone()),
                Cell::from(s.detail.clone()),
            ])
        });
        let table = Table::new(
            rows,
            [
                Constraint::Length(2),
                Constraint::Length(12),
                Constraint::Fill(2),
                Constraint::Length(20),
                Constraint::Fill(4),
            ],
        )
        .header(Row::new(["", "Kind", "Name", "State", "Detail"]).bold().underlined())
        .block(block);
        frame.render_widget(table, area);
    }
}
