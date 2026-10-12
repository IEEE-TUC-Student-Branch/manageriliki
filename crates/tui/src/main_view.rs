use std::sync::{Arc, mpsc};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use manageriliki_core::{
    AuditEntry, CommandOutput, FlowControl, Identity, Manager, Parity, Result as CoreResult,
    Role, SecretUpdate, SerialConfig, Server, ServerInput, SshAuth, SshConfig, Status,
    StatusMonitor, Terminal, TransportKind, User, VncConfig,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap};

use crate::form::{Form, FormResult, centered};
use crate::term_view;

const BAUD_RATES: [u32; 10] = [1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 921600];
const ON: &[&str] = &["on"];

pub enum Outcome {
    Stay,
    Quit,
    Logout,
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Servers,
    Users,
    Audit,
}

enum Confirm {
    DeleteServer(i64),
    DeleteUser(i64),
    ResetHostKey(i64),
}

enum Modal {
    Server { id: Option<i64>, form: Form },
    User { id: Option<i64>, form: Form },
    Password { user_id: Option<i64>, form: Form },
    Command { server: Box<Server>, form: Form },
    Confirm { message: String, action: Confirm },
    Output { title: String, text: String, scroll: u16 },
    Help,
}

struct Job {
    rx: mpsc::Receiver<(String, CoreResult<CommandOutput>)>,
    pending: usize,
}

pub struct MainView {
    mgr: Manager,
    who: Identity,
    tab: Tab,
    servers: Vec<Server>,
    server_state: TableState,
    users: Vec<User>,
    user_state: TableState,
    audit: Vec<AuditEntry>,
    audit_state: TableState,
    monitor: StatusMonitor,
    modal: Option<Modal>,
    terminal: Option<Terminal>,
    job: Option<Job>,
    flash: Option<(String, bool)>,
}

impl MainView {
    pub fn new(mgr: Manager, who: Identity) -> Self {
        let mut view = MainView {
            mgr,
            who,
            tab: Tab::Servers,
            servers: Vec::new(),
            server_state: TableState::default().with_selected(0),
            users: Vec::new(),
            user_state: TableState::default().with_selected(0),
            audit: Vec::new(),
            audit_state: TableState::default().with_selected(0),
            monitor: StatusMonitor::start(Duration::from_secs(20), Arc::new(|| {})),
            modal: None,
            terminal: None,
            job: None,
            flash: None,
        };
        view.reload();
        view
    }

    fn is_admin(&self) -> bool {
        self.who.role == Role::Admin
    }

    fn info(&mut self, msg: impl Into<String>) {
        self.flash = Some((msg.into(), false));
    }

    fn error(&mut self, msg: impl ToString) {
        self.flash = Some((msg.to_string(), true));
    }

    fn reload(&mut self) {
        match self.mgr.refresh_identity(&self.who) {
            Ok(who) => self.who = who,
            Err(e) => return self.error(e),
        }
        match self.mgr.list_servers(&self.who) {
            Ok(servers) => {
                self.monitor.set_servers(servers.clone());
                self.servers = servers;
            }
            Err(e) => self.error(e),
        }
        if self.is_admin() {
            match self.mgr.list_users(&self.who) {
                Ok(users) => self.users = users,
                Err(e) => self.error(e),
            }
            match self.mgr.audit_log(&self.who, 500) {
                Ok(audit) => self.audit = audit,
                Err(e) => self.error(e),
            }
        } else {
            self.tab = Tab::Servers;
        }
        clamp(&mut self.server_state, self.servers.len());
        clamp(&mut self.user_state, self.users.len());
        clamp(&mut self.audit_state, self.audit.len());
    }

    fn selected_server(&self) -> Option<&Server> {
        self.server_state.selected().and_then(|i| self.servers.get(i))
    }

    fn selected_user(&self) -> Option<&User> {
        self.user_state.selected().and_then(|i| self.users.get(i))
    }

    /// Collects finished command results into the output dialog.
    pub fn tick(&mut self) {
        let Some(job) = &mut self.job else { return };
        while let Ok((name, result)) = job.rx.try_recv() {
            job.pending -= 1;
            let chunk = match result {
                Ok(out) => {
                    let mut s = format!("━━ {name} (exit {}) ━━\n{}", out.exit_status, out.stdout);
                    if !out.stderr.is_empty() {
                        s.push_str(&format!("[stderr]\n{}", out.stderr));
                    }
                    s
                }
                Err(e) => format!("━━ {name} (failed) ━━\n{e}\n"),
            };
            if let Some(Modal::Output { text, .. }) = &mut self.modal {
                if text.starts_with("Running") {
                    text.clear();
                }
                text.push_str(&chunk);
                text.push('\n');
            }
        }
        if job.pending == 0 {
            self.job = None;
        }
    }

    pub fn on_paste(&mut self, text: &str) {
        if let Some(term) = &self.terminal {
            if term.with_screen(|s| s.bracketed_paste()) {
                term.write(format!("\x1b[200~{text}\x1b[201~"));
            } else {
                term.write(text.replace('\n', "\r"));
            }
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        if let Some(term) = &self.terminal {
            if term.is_closed() || term_view::is_detach_key(&key) {
                self.terminal = None;
                self.reload();
            } else if key.modifiers.contains(KeyModifiers::SHIFT) && key.code == KeyCode::PageUp {
                term.scroll(10);
            } else if key.modifiers.contains(KeyModifiers::SHIFT) && key.code == KeyCode::PageDown {
                term.scroll(-10);
            } else if let Some(bytes) =
                term_view::encode_key(&key, term.with_screen(|s| s.application_cursor()))
            {
                term.scroll(isize::MIN / 2); // typing jumps back to the live screen
                term.write(bytes);
            }
            return Outcome::Stay;
        }
        if let Some(modal) = self.modal.take() {
            self.modal = self.on_modal_key(modal, key);
            return Outcome::Stay;
        }
        self.flash = None;

        match key.code {
            KeyCode::Char('q') => return Outcome::Quit,
            KeyCode::Char('o') => {
                self.mgr.logout(&self.who);
                return Outcome::Logout;
            }
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Char('w') => {
                self.modal = Some(Modal::Password {
                    user_id: None,
                    form: Form::new("Change my password")
                        .secret("old", "Current password")
                        .secret("new", "New password")
                        .secret("confirm", "Confirm"),
                })
            }
            KeyCode::Char('r') => {
                self.reload();
                self.monitor.refresh();
                self.info("refreshed");
            }
            KeyCode::Char('1') => self.switch_tab(Tab::Servers),
            KeyCode::Char('2') if self.is_admin() => self.switch_tab(Tab::Users),
            KeyCode::Char('3') if self.is_admin() => self.switch_tab(Tab::Audit),
            KeyCode::Tab if self.is_admin() => self.switch_tab(match self.tab {
                Tab::Servers => Tab::Users,
                Tab::Users => Tab::Audit,
                Tab::Audit => Tab::Servers,
            }),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            _ => match self.tab {
                Tab::Servers => self.on_servers_key(key),
                Tab::Users => self.on_users_key(key),
                Tab::Audit => {}
            },
        }
        Outcome::Stay
    }

    fn switch_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.reload();
    }

    fn move_selection(&mut self, delta: i32) {
        let (state, len) = match self.tab {
            Tab::Servers => (&mut self.server_state, self.servers.len()),
            Tab::Users => (&mut self.user_state, self.users.len()),
            Tab::Audit => (&mut self.audit_state, self.audit.len()),
        };
        if len == 0 {
            return;
        }
        let cur = state.selected().unwrap_or(0) as i32;
        state.select(Some((cur + delta).clamp(0, len as i32 - 1) as usize));
    }

    fn on_servers_key(&mut self, key: KeyEvent) {
        let selected = self.selected_server().cloned();
        let admin_key = matches!(key.code, KeyCode::Char('a' | 'e' | 'd' | 'h'));
        if (admin_key && !self.is_admin()) || self.who.role < Role::Operator {
            return;
        }
        match (key.code, selected) {
            (KeyCode::Char('a'), _) => {
                self.modal = Some(Modal::Server { id: None, form: server_form(None) })
            }
            (KeyCode::Char('e'), Some(s)) => {
                self.modal = Some(Modal::Server { id: Some(s.id), form: server_form(Some(&s)) })
            }
            (KeyCode::Char('d'), Some(s)) => {
                self.modal = Some(Modal::Confirm {
                    message: format!("Delete server '{}'?", s.name),
                    action: Confirm::DeleteServer(s.id),
                })
            }
            (KeyCode::Char('h'), Some(s)) => {
                self.modal = Some(Modal::Confirm {
                    message: format!(
                        "Forget the trusted SSH host key of '{}'? The next connection will trust whatever key the server presents.",
                        s.name
                    ),
                    action: Confirm::ResetHostKey(s.id),
                })
            }
            (KeyCode::Enter | KeyCode::Char('s'), Some(s)) => self.open_session(&s, TransportKind::Ssh),
            (KeyCode::Char('c'), Some(s)) => self.open_session(&s, TransportKind::Serial),
            (KeyCode::Char('v'), Some(s)) => {
                if s.vnc.is_some() {
                    self.info(format!("VNC displays need graphics: open '{}' in manageriliki-gui", s.name));
                } else {
                    self.error(format!("{} has no VNC display configured", s.name));
                }
            }
            (KeyCode::Char('x'), Some(s)) => {
                let mut scopes = vec![format!("only {}", s.name)];
                if !s.group.is_empty() {
                    scopes.push(format!("group {}", s.group));
                }
                scopes.push("all servers".into());
                let first = scopes[0].clone();
                self.modal = Some(Modal::Command {
                    form: Form::new("Run command over SSH")
                        .text("command", "Command", "")
                        .choice("scope", "Run on", scopes, &first),
                    server: Box::new(s),
                })
            }
            _ => {}
        }
    }

    fn on_users_key(&mut self, key: KeyEvent) {
        let selected = self.selected_user().cloned();
        let roles = Role::ALL.map(|r| r.as_str());
        match (key.code, selected) {
            (KeyCode::Char('a'), _) => {
                self.modal = Some(Modal::User {
                    id: None,
                    form: Form::new("New user")
                        .text("username", "Username", "")
                        .secret("password", "Password")
                        .choice("role", "Role", roles, "operator"),
                })
            }
            (KeyCode::Char('e'), Some(u)) => {
                self.modal = Some(Modal::User {
                    id: Some(u.id),
                    form: Form::new(format!("Edit user {}", u.username))
                        .choice("role", "Role", roles, u.role.as_str())
                        .toggle("disabled", "Disabled", u.disabled),
                })
            }
            (KeyCode::Char('p'), Some(u)) => {
                self.modal = Some(Modal::Password {
                    user_id: Some(u.id),
                    form: Form::new(format!("Reset password for {}", u.username))
                        .secret("new", "New password")
                        .secret("confirm", "Confirm"),
                })
            }
            (KeyCode::Char('d'), Some(u)) => {
                self.modal = Some(Modal::Confirm {
                    message: format!("Delete user '{}'?", u.username),
                    action: Confirm::DeleteUser(u.id),
                })
            }
            _ => {}
        }
    }

    fn open_session(&mut self, server: &Server, kind: TransportKind) {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let notify = Arc::new(|| {});
        match self.mgr.open_terminal(&self.who, server.id, kind, rows.saturating_sub(1), cols, notify) {
            Ok(term) => self.terminal = Some(term),
            Err(e) => self.error(e),
        }
    }

    /// Handles a key for the open dialog; returns the dialog to keep showing.
    fn on_modal_key(&mut self, modal: Modal, key: KeyEvent) -> Option<Modal> {
        match modal {
            Modal::Help => None,
            Modal::Output { title, text, scroll } => {
                let scroll = match key.code {
                    KeyCode::Up | KeyCode::Char('k') => scroll.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => scroll + 1,
                    KeyCode::PageUp => scroll.saturating_sub(15),
                    KeyCode::PageDown => scroll + 15,
                    _ => {
                        self.job = None;
                        return None;
                    }
                };
                Some(Modal::Output { title, text, scroll })
            }
            Modal::Confirm { message, action } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let result = match action {
                        Confirm::DeleteServer(id) => self.mgr.delete_server(&self.who, id),
                        Confirm::DeleteUser(id) => self.mgr.delete_user(&self.who, id),
                        Confirm::ResetHostKey(id) => self.mgr.reset_host_key(&self.who, id),
                    };
                    match result {
                        Ok(()) => self.info("done"),
                        Err(e) => self.error(e),
                    }
                    self.reload();
                    None
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => None,
                _ => Some(Modal::Confirm { message, action }),
            },
            Modal::Server { id, mut form } => self.form_step(&mut form, key, |v, form| {
                let input = read_server_form(form)?;
                v.mgr.save_server(&v.who, id, input).map_err(|e| e.to_string())?;
                v.info("server saved");
                Ok(())
            })
            .then_some(Modal::Server { id, form }),
            Modal::User { id, mut form } => self.form_step(&mut form, key, |v, form| {
                let role = Role::parse(&form.get("role")).unwrap_or(Role::Viewer);
                match id {
                    None => v
                        .mgr
                        .create_user(&v.who, form.get("username").trim(), &form.get("password"), role)
                        .map(|_| ()),
                    Some(id) => v.mgr.update_user(&v.who, id, role, form.on("disabled")),
                }
                .map_err(|e| e.to_string())?;
                v.info("user saved");
                Ok(())
            })
            .then_some(Modal::User { id, form }),
            Modal::Password { user_id, mut form } => self.form_step(&mut form, key, |v, form| {
                if form.get("new") != form.get("confirm") {
                    return Err("passwords do not match".into());
                }
                match user_id {
                    None => v.mgr.change_own_password(&v.who, &form.get("old"), &form.get("new")),
                    Some(id) => v.mgr.reset_password(&v.who, id, &form.get("new")),
                }
                .map_err(|e| e.to_string())?;
                v.info("password changed");
                Ok(())
            })
            .then_some(Modal::Password { user_id, form }),
            Modal::Command { server, mut form } => {
                match form.handle_key(key) {
                    FormResult::Pending => return Some(Modal::Command { server, form }),
                    FormResult::Cancel => return None,
                    FormResult::Submit => {}
                }
                let command = form.get("command");
                if command.trim().is_empty() {
                    form.error = Some("enter a command".into());
                    return Some(Modal::Command { server, form });
                }
                let scope = form.get("scope");
                let targets: Vec<Server> = self
                    .servers
                    .iter()
                    .filter(|s| s.ssh.is_some())
                    .filter(|s| match scope.as_str() {
                        "all servers" => true,
                        sc if sc.starts_with("group ") => s.group == server.group,
                        _ => s.id == server.id,
                    })
                    .cloned()
                    .collect();
                if targets.is_empty() {
                    form.error = Some("no SSH-enabled servers in that scope".into());
                    return Some(Modal::Command { server, form });
                }
                let rx = self.mgr.run_command_many(&self.who, &targets, &command, Arc::new(|| {}));
                self.job = Some(Job { rx, pending: targets.len() });
                Some(Modal::Output {
                    title: format!("$ {command}"),
                    text: format!("Running on {} server(s)…", targets.len()),
                    scroll: 0,
                })
            }
        }
    }

    /// Feeds a key to a form; on submit runs `save`. Returns true to keep the
    /// dialog open (still editing, or the save failed).
    fn form_step(
        &mut self,
        form: &mut Form,
        key: KeyEvent,
        save: impl FnOnce(&mut Self, &Form) -> std::result::Result<(), String>,
    ) -> bool {
        match form.handle_key(key) {
            FormResult::Pending => true,
            FormResult::Cancel => false,
            FormResult::Submit => match save(self, form) {
                Ok(()) => {
                    self.reload();
                    false
                }
                Err(e) => {
                    form.error = Some(e);
                    true
                }
            },
        }
    }

    // ---- drawing ----

    pub fn draw(&mut self, frame: &mut Frame) {
        if let Some(term) = &self.terminal {
            term_view::render(frame, frame.area(), term);
            return;
        }
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        let mut titles = vec!["1 Servers"];
        if self.is_admin() {
            titles.extend(["2 Users", "3 Audit"]);
        }
        let [tabs_area, who_area] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(40)]).areas(header);
        frame.render_widget(
            Tabs::new(titles)
                .select(self.tab as usize)
                .highlight_style(Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD | Modifier::REVERSED))
                .divider("│"),
            tabs_area,
        );
        frame.render_widget(
            Line::from(format!("{} ({}) ", self.who.username, self.who.role))
                .right_aligned()
                .fg(Color::Cyan),
            who_area,
        );

        match self.tab {
            Tab::Servers => self.draw_servers(frame, body),
            Tab::Users => self.draw_users(frame, body),
            Tab::Audit => self.draw_audit(frame, body),
        }

        let footer_line = match &self.flash {
            Some((msg, true)) => Line::from(format!(" {msg}")).fg(Color::Red),
            Some((msg, false)) => Line::from(format!(" {msg}")).fg(Color::Green),
            None => Line::from(self.key_hints()).dim(),
        };
        frame.render_widget(footer_line, footer);

        if let Some(modal) = &self.modal {
            draw_modal(frame, modal);
        }
    }

    fn key_hints(&self) -> String {
        let hints = match (self.tab, self.who.role) {
            (Tab::Servers, Role::Admin) => {
                " Enter/s ssh · c serial · x run · a add · e edit · d delete · h reset hostkey · r refresh · ? help · q quit"
            }
            (Tab::Servers, Role::Operator) => {
                " Enter/s ssh · c serial · x run command · r refresh · w password · o logout · ? help · q quit"
            }
            (Tab::Servers, Role::Viewer) => " r refresh · w password · o logout · ? help · q quit",
            (Tab::Users, _) => " a add · e edit role/disabled · p reset password · d delete · ? help · q quit",
            (Tab::Audit, _) => " ↑↓ scroll · r refresh · ? help · q quit",
        };
        hints.to_string()
    }

    fn draw_servers(&mut self, frame: &mut Frame, area: Rect) {
        let [table_area, detail_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(7)]).areas(area);
        let rows = self.servers.iter().map(|s| {
            let (dot, color) = match self.monitor.get(s.id) {
                Status::Online(_) => ("●", Color::Green),
                Status::Offline(_) => ("●", Color::Red),
                Status::Unknown => ("○", Color::DarkGray),
            };
            Row::new(vec![
                Cell::from(dot).fg(color),
                Cell::from(s.name.clone()).bold(),
                Cell::from(s.group.clone()),
                Cell::from(
                    s.ssh
                        .as_ref()
                        .map(|c| format!("{}@{}:{}", c.username, c.host, c.port))
                        .unwrap_or_else(|| "—".into()),
                ),
                Cell::from(s.serial.as_ref().map(|c| c.summary()).unwrap_or_else(|| "—".into())),
                Cell::from(s.required_role.as_str()),
            ])
        });
        let table = Table::new(
            rows,
            [
                Constraint::Length(2),
                Constraint::Fill(2),
                Constraint::Fill(1),
                Constraint::Fill(3),
                Constraint::Fill(3),
                Constraint::Length(9),
            ],
        )
        .header(Row::new(["", "Name", "Group", "SSH", "Serial", "Connect"]).bold().underlined())
        .row_highlight_style(Style::new().bg(Color::DarkGray))
        .block(Block::bordered().title(format!(" Servers ({}) ", self.servers.len())));
        frame.render_stateful_widget(table, table_area, &mut self.server_state);

        let detail = match self.selected_server() {
            None => vec![Line::from("No servers yet.").dim(), Line::from(if self.is_admin() {
                "Press 'a' to add one."
            } else {
                "Ask an admin to add servers."
            })
            .dim()],
            Some(s) => {
                let status = match self.monitor.get(s.id) {
                    Status::Online(ms) if s.ssh.is_some() => format!("online ({ms} ms)"),
                    Status::Online(_) => "device present".into(),
                    Status::Offline(e) => format!("offline: {e}"),
                    Status::Unknown => "unknown".into(),
                };
                let mut lines = vec![Line::from(vec![Span::raw("Status: ").bold(), Span::raw(status)])];
                if let Some(ssh) = &s.ssh {
                    let auth = match &ssh.auth {
                        SshAuth::Key { path } => format!("key {path}"),
                        a => a.label().to_string(),
                    };
                    lines.push(Line::from(vec![
                        Span::raw("SSH: ").bold(),
                        Span::raw(format!(
                            "{}@{}:{} · auth {auth} · host key {}",
                            ssh.username,
                            ssh.host,
                            ssh.port,
                            s.host_key_fp.as_deref().unwrap_or("not yet trusted")
                        )),
                    ]));
                }
                if let Some(serial) = &s.serial {
                    lines.push(Line::from(vec![
                        Span::raw("Serial: ").bold(),
                        Span::raw(format!("{} · flow {:?}", serial.summary(), serial.flow_control)),
                    ]));
                }
                if let Some(vnc) = &s.vnc {
                    lines.push(Line::from(vec![
                        Span::raw("VNC: ").bold(),
                        Span::raw(format!(
                            "{}{} · open it in manageriliki-gui",
                            vnc.summary(s.ssh.as_ref()),
                            if s.has_vnc_secret { " · password stored" } else { "" }
                        )),
                    ]));
                }
                if !s.notes.is_empty() {
                    lines.push(Line::from(vec![Span::raw("Notes: ").bold(), Span::raw(s.notes.clone())]));
                }
                lines
            }
        };
        frame.render_widget(
            Paragraph::new(detail).wrap(Wrap { trim: true }).block(Block::bordered().title(" Details ")),
            detail_area,
        );
    }

    fn draw_users(&mut self, frame: &mut Frame, area: Rect) {
        let rows = self.users.iter().map(|u| {
            Row::new(vec![
                Cell::from(u.username.clone()).bold(),
                Cell::from(u.role.as_str()),
                if u.disabled { Cell::from("disabled").fg(Color::Red) } else { Cell::from("active").fg(Color::Green) },
                Cell::from(
                    u.last_login
                        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "never".into()),
                ),
                Cell::from(u.created_at.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string()),
            ])
        });
        let table = Table::new(
            rows,
            [
                Constraint::Fill(2),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(18),
                Constraint::Length(12),
            ],
        )
        .header(Row::new(["Username", "Role", "State", "Last login", "Created"]).bold().underlined())
        .row_highlight_style(Style::new().bg(Color::DarkGray))
        .block(Block::bordered().title(format!(" Users ({}) ", self.users.len())));
        frame.render_stateful_widget(table, area, &mut self.user_state);
    }

    fn draw_audit(&mut self, frame: &mut Frame, area: Rect) {
        let rows = self.audit.iter().map(|a| {
            let color = if a.action.contains("fail") || a.action.contains("mismatch") {
                Color::Red
            } else {
                Color::Reset
            };
            Row::new(vec![
                Cell::from(a.ts.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string()),
                Cell::from(a.username.clone()),
                Cell::from(a.action.clone()).fg(color),
                Cell::from(a.target.clone()),
                Cell::from(a.detail.clone()),
            ])
        });
        let table = Table::new(
            rows,
            [
                Constraint::Length(20),
                Constraint::Length(14),
                Constraint::Length(20),
                Constraint::Fill(1),
                Constraint::Fill(2),
            ],
        )
        .header(Row::new(["Time", "User", "Action", "Target", "Detail"]).bold().underlined())
        .row_highlight_style(Style::new().bg(Color::DarkGray))
        .block(Block::bordered().title(" Audit log (latest 500) "));
        frame.render_stateful_widget(table, area, &mut self.audit_state);
    }
}

fn clamp(state: &mut TableState, len: usize) {
    match (state.selected(), len) {
        (_, 0) => state.select(None),
        (None, _) => state.select(Some(0)),
        (Some(i), n) if i >= n => state.select(Some(n - 1)),
        _ => {}
    }
}

fn draw_modal(frame: &mut Frame, modal: &Modal) {
    let area = frame.area();
    match modal {
        Modal::Server { form, .. }
        | Modal::User { form, .. }
        | Modal::Password { form, .. }
        | Modal::Command { form, .. } => form.render(frame, area),
        Modal::Confirm { message, .. } => {
            let popup = centered(area, 60.min(area.width), 6);
            frame.render_widget(Clear, popup);
            frame.render_widget(
                Paragraph::new(vec![Line::from(message.clone()), Line::default(), Line::from("y = yes · n = no").dim()])
                    .wrap(Wrap { trim: true })
                    .block(Block::bordered().title(" Confirm ").border_style(Style::new().fg(Color::Yellow))),
                popup,
            );
        }
        Modal::Output { title, text, scroll } => {
            let popup = centered(area, area.width.saturating_sub(6), area.height.saturating_sub(4));
            frame.render_widget(Clear, popup);
            frame.render_widget(
                Paragraph::new(text.clone())
                    .scroll((*scroll, 0))
                    .block(
                        Block::bordered()
                            .title(format!(" {title} "))
                            .title_bottom(Line::from(" ↑↓/PgUp/PgDn scroll · any other key closes ").dim())
                            .border_style(Style::new().fg(Color::Cyan)),
                    ),
                popup,
            );
        }
        Modal::Help => {
            let text = vec![
                Line::from("Global").bold(),
                Line::from("  1/2/3, Tab   switch tabs (Users/Audit are admin-only)"),
                Line::from("  ↑↓ j k       move selection        r  refresh"),
                Line::from("  w            change my password    o  log out     q  quit"),
                Line::default(),
                Line::from("Servers").bold(),
                Line::from("  Enter, s     SSH shell             c  serial console"),
                Line::from("  v            VNC display (opens in manageriliki-gui)"),
                Line::from("  x            run a command on this server, its group, or all"),
                Line::from("  a / e / d    add / edit / delete (admin)"),
                Line::from("  h            forget trusted SSH host key (admin)"),
                Line::default(),
                Line::from("Sessions").bold(),
                Line::from("  Ctrl+] or F12        close the session"),
                Line::from("  Shift+PgUp/PgDn      scroll back"),
                Line::default(),
                Line::from("Users (admin)").bold(),
                Line::from("  a add · e edit role/disabled · p reset password · d delete"),
            ];
            let popup = centered(area, 72.min(area.width), text.len() as u16 + 2);
            frame.render_widget(Clear, popup);
            frame.render_widget(
                Paragraph::new(text).block(Block::bordered().title(" Help — any key closes ")),
                popup,
            );
        }
    }
}

fn server_form(server: Option<&Server>) -> Form {
    let input = server.map(ServerInput::from_server).unwrap_or_default();
    let ssh = input.ssh.clone().unwrap_or_default();
    let serial = input.serial.clone().unwrap_or_default();
    let key_path = match &ssh.auth {
        SshAuth::Key { path } => path.clone(),
        _ => "~/.ssh/id_ed25519".into(),
    };
    let mut bauds: Vec<String> = BAUD_RATES.iter().map(u32::to_string).collect();
    if !BAUD_RATES.contains(&serial.baud) {
        bauds.push(serial.baud.to_string());
    }
    let vnc = input.vnc.clone().unwrap_or_default();
    let hint = match server {
        Some(s) if s.has_secret || s.has_vnc_secret => "Stored passwords are kept when left blank.",
        _ => "",
    };
    Form::new(if server.is_some() { "Edit server" } else { "New server" })
        .hint(hint)
        .text("name", "Name", input.name)
        .text("group", "Group", input.group)
        .text("notes", "Notes", input.notes)
        .choice("role", "Who may connect", ["operator", "admin"], input.required_role.as_str())
        .toggle("ssh", "SSH", input.ssh.is_some())
        .text("host", "Host", ssh.host)
        .when("ssh", ON)
        .text("port", "Port", ssh.port.to_string())
        .when("ssh", ON)
        .text("user", "Username", ssh.username)
        .when("ssh", ON)
        .choice("auth", "Auth", ["agent", "password", "key"], ssh.auth.label())
        .when("ssh", ON)
        .text("key", "Key file", key_path)
        .when("ssh", ON)
        .when("auth", &["key"])
        .secret("secret", "Password/passphrase")
        .when("ssh", ON)
        .when("auth", &["password", "key"])
        .toggle("serial", "Serial (RS232)", input.serial.is_some())
        .text("device", "Device", serial.device)
        .when("serial", ON)
        .choice("baud", "Baud", bauds, &serial.baud.to_string())
        .when("serial", ON)
        .choice("bits", "Data bits", ["8", "7", "6", "5"], &serial.data_bits.to_string())
        .when("serial", ON)
        .choice("parity", "Parity", ["none", "odd", "even"], parity_str(serial.parity))
        .when("serial", ON)
        .choice("stop", "Stop bits", ["1", "2"], &serial.stop_bits.to_string())
        .when("serial", ON)
        .choice("flow", "Flow control", ["none", "software", "hardware"], flow_str(serial.flow_control))
        .when("serial", ON)
        .toggle("vnc", "VNC display", input.vnc.is_some())
        .choice("vnc_via", "Connection", ["direct", "ssh tunnel"], if vnc.via_ssh { "ssh tunnel" } else { "direct" })
        .when("vnc", ON)
        .text("vnc_host", "VNC host (blank=auto)", vnc.host)
        .when("vnc", ON)
        .text("vnc_port", "VNC port", vnc.port.to_string())
        .when("vnc", ON)
        .secret("vnc_secret", "VNC password")
        .when("vnc", ON)
}

fn parity_str(p: Parity) -> &'static str {
    match p {
        Parity::None => "none",
        Parity::Odd => "odd",
        Parity::Even => "even",
    }
}

fn flow_str(f: FlowControl) -> &'static str {
    match f {
        FlowControl::None => "none",
        FlowControl::Software => "software",
        FlowControl::Hardware => "hardware",
    }
}

fn read_server_form(form: &Form) -> std::result::Result<ServerInput, String> {
    let ssh = if form.on("ssh") {
        let port = form.get("port").trim().parse::<u16>().map_err(|_| "port must be 1-65535")?;
        let auth = match form.get("auth").as_str() {
            "password" => SshAuth::Password,
            "key" => SshAuth::Key { path: form.get("key") },
            _ => SshAuth::Agent,
        };
        Some(SshConfig { host: form.get("host"), port, username: form.get("user"), auth })
    } else {
        None
    };
    let serial = if form.on("serial") {
        Some(SerialConfig {
            device: form.get("device"),
            baud: form.get("baud").parse().map_err(|_| "invalid baud rate")?,
            data_bits: form.get("bits").parse().map_err(|_| "invalid data bits")?,
            parity: match form.get("parity").as_str() {
                "odd" => Parity::Odd,
                "even" => Parity::Even,
                _ => Parity::None,
            },
            stop_bits: form.get("stop").parse().map_err(|_| "invalid stop bits")?,
            flow_control: match form.get("flow").as_str() {
                "software" => FlowControl::Software,
                "hardware" => FlowControl::Hardware,
                _ => FlowControl::None,
            },
        })
    } else {
        None
    };
    let vnc = if form.on("vnc") {
        Some(VncConfig {
            host: form.get("vnc_host"),
            port: form.get("vnc_port").trim().parse().map_err(|_| "VNC port must be 1-65535")?,
            via_ssh: form.get("vnc_via") == "ssh tunnel",
        })
    } else {
        None
    };
    let secret = form.get("secret");
    let vnc_secret = form.get("vnc_secret");
    Ok(ServerInput {
        name: form.get("name"),
        group: form.get("group"),
        notes: form.get("notes"),
        ssh,
        serial,
        vnc,
        required_role: Role::parse(&form.get("role")).unwrap_or(Role::Operator),
        secret: if secret.is_empty() { SecretUpdate::Keep } else { SecretUpdate::Set(secret) },
        vnc_secret: if vnc_secret.is_empty() { SecretUpdate::Keep } else { SecretUpdate::Set(vnc_secret) },
    })
}
