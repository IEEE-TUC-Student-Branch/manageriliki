mod editors;
mod term_widget;
mod vnc_view;
#[cfg(test)]
mod tests;

use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, Ui};
use editors::{PasswordDialog, ServerEditor, UserEditor};
use vnc_view::VncView;
use manageriliki_core::{
    AuditEntry, CommandOutput, Identity, Manager, Result as CoreResult, Role, Server, Status,
    StatusMonitor, Terminal, TransportKind, User,
};

fn main() -> eframe::Result {
    let mgr = match Manager::open_default() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("manageriliki: {e}");
            std::process::exit(1);
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("manageriliki")
            .with_app_id("manageriliki")
            .with_inner_size([1180.0, 740.0])
            .with_min_inner_size([720.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "manageriliki",
        options,
        Box::new(move |cc| Ok(Box::new(GuiApp::new(mgr, cc.egui_ctx.clone())))),
    )
}

enum Screen {
    Setup { username: String, password: String, confirm: String, error: Option<String> },
    Login { username: String, password: String, error: Option<String> },
    Main(Box<MainUi>),
}

struct GuiApp {
    mgr: Manager,
    ctx: egui::Context,
    screen: Screen,
}

fn login_screen() -> Screen {
    Screen::Login { username: String::new(), password: String::new(), error: None }
}

impl GuiApp {
    fn new(mgr: Manager, ctx: egui::Context) -> Self {
        let screen = match mgr.needs_setup() {
            Ok(true) => Screen::Setup {
                username: "admin".into(),
                password: String::new(),
                confirm: String::new(),
                error: None,
            },
            Ok(false) => login_screen(),
            Err(e) => Screen::Login { username: String::new(), password: String::new(), error: Some(e.to_string()) },
        };
        GuiApp { mgr, ctx, screen }
    }
}

impl eframe::App for GuiApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let mut next = None;
        match &mut self.screen {
            Screen::Setup { username, password, confirm, error } => {
                auth_card(ui, "First-time setup", "Create the first administrator account.", |ui| {
                    field(ui, "Username", egui::TextEdit::singleline(username));
                    field(ui, "Password", egui::TextEdit::singleline(password).password(true));
                    let r = field(ui, "Confirm", egui::TextEdit::singleline(confirm).password(true));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if let Some(e) = error.as_ref() {
                        ui.colored_label(Color32::LIGHT_RED, e);
                    }
                    if ui.button("Create admin").clicked() || enter {
                        if password != confirm {
                            *error = Some("passwords do not match".into());
                        } else {
                            match self.mgr.create_initial_admin(username.trim(), password) {
                                Ok(who) => next = Some(who),
                                Err(e) => *error = Some(e.to_string()),
                            }
                        }
                    }
                });
            }
            Screen::Login { username, password, error } => {
                auth_card(ui, "Sign in", &format!("Data: {}", self.mgr.data_dir().display()), |ui| {
                    let u = field(ui, "Username", egui::TextEdit::singleline(username));
                    if username.is_empty() && !u.has_focus() && error.is_none() {
                        u.request_focus();
                    }
                    let r = field(ui, "Password", egui::TextEdit::singleline(password).password(true));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if let Some(e) = error.as_ref() {
                        ui.colored_label(Color32::LIGHT_RED, e);
                    }
                    if ui.button("Sign in").clicked() || enter {
                        match self.mgr.login(username, password) {
                            Ok(who) => next = Some(who),
                            Err(e) => {
                                *error = Some(e.to_string());
                                password.clear();
                            }
                        }
                    }
                });
            }
            Screen::Main(main) => {
                if main.show(ui) == MainOutcome::Logout {
                    self.mgr.logout(&main.who);
                    self.screen = login_screen();
                }
            }
        }
        if let Some(who) = next {
            self.screen = Screen::Main(Box::new(MainUi::new(self.mgr.clone(), who, self.ctx.clone())));
        }
    }
}

fn auth_card(ui: &mut Ui, title: &str, subtitle: &str, body: impl FnOnce(&mut Ui)) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.25).max(20.0));
            ui.heading(RichText::new("manageriliki").size(28.0).strong());
            ui.label(RichText::new("Local server management").weak());
            ui.add_space(16.0);
            egui::Frame::group(ui.style()).inner_margin(16.0).show(ui, |ui| {
                ui.set_width(340.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(title).size(18.0).strong());
                    ui.label(RichText::new(subtitle).weak().small());
                    ui.add_space(8.0);
                    body(ui);
                });
            });
        });
    });
}

/// A filled dot for a known status, a hollow one while unknown.
fn status_dot(ui: &mut Ui, color: Option<Color32>) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    match color {
        Some(c) => {
            ui.painter().circle_filled(rect.center(), 5.0, c);
        }
        None => {
            ui.painter().circle_stroke(rect.center(), 4.5, egui::Stroke::new(1.0, Color32::GRAY));
        }
    }
    response
}

fn field(ui: &mut Ui, label: &str, edit: egui::TextEdit) -> egui::Response {
    let label = ui.label(label);
    ui.add(edit.desired_width(f32::INFINITY)).labelled_by(label.id)
}

// ---- main UI ----

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Servers,
    Users,
    Audit,
}

#[derive(PartialEq)]
enum MainOutcome {
    Stay,
    Logout,
}

enum Confirm {
    DeleteServer(i64),
    DeleteUser(i64),
    ResetHostKey(i64),
}

struct Session {
    id: u64,
    term: Terminal,
    open: bool,
}

#[derive(PartialEq, Clone)]
enum Scope {
    One,
    Group,
    All,
}

struct CommandDialog {
    server: Server,
    command: String,
    scope: Scope,
    rx: Option<mpsc::Receiver<(String, CoreResult<CommandOutput>)>>,
    results: Vec<(String, CoreResult<CommandOutput>)>,
    pending: usize,
}

struct MainUi {
    mgr: Manager,
    who: Identity,
    ctx: egui::Context,
    tab: Tab,
    servers: Vec<Server>,
    users: Vec<User>,
    audit: Vec<AuditEntry>,
    filter: String,
    monitor: StatusMonitor,
    sessions: Vec<Session>,
    displays: Vec<VncView>,
    next_session_id: u64,
    server_editor: Option<ServerEditor>,
    user_editor: Option<UserEditor>,
    password_dialog: Option<PasswordDialog>,
    confirm: Option<(String, Confirm)>,
    command: Option<CommandDialog>,
    message: Option<(String, bool, Instant)>,
}

impl MainUi {
    fn new(mgr: Manager, who: Identity, ctx: egui::Context) -> Self {
        let repaint = ctx.clone();
        let mut ui = MainUi {
            monitor: StatusMonitor::start(Duration::from_secs(20), Arc::new(move || repaint.request_repaint())),
            mgr,
            who,
            ctx,
            tab: Tab::Servers,
            servers: Vec::new(),
            users: Vec::new(),
            audit: Vec::new(),
            filter: String::new(),
            sessions: Vec::new(),
            displays: Vec::new(),
            next_session_id: 0,
            server_editor: None,
            user_editor: None,
            password_dialog: None,
            confirm: None,
            command: None,
            message: None,
        };
        ui.reload();
        ui
    }

    fn is_admin(&self) -> bool {
        self.who.role == Role::Admin
    }

    fn can_connect(&self, server: &Server) -> bool {
        self.who.role >= Role::Operator && self.who.role >= server.required_role
    }

    fn info(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), false, Instant::now()));
    }

    fn error(&mut self, msg: impl ToString) {
        self.message = Some((msg.to_string(), true, Instant::now()));
    }

    fn reload(&mut self) {
        match self.mgr.refresh_identity(&self.who) {
            Ok(who) => self.who = who,
            Err(e) => return self.error(e),
        }
        match self.mgr.list_servers(&self.who) {
            Ok(s) => {
                self.monitor.set_servers(s.clone());
                self.servers = s;
            }
            Err(e) => self.error(e),
        }
        if self.is_admin() {
            match self.mgr.list_users(&self.who) {
                Ok(u) => self.users = u,
                Err(e) => self.error(e),
            }
            match self.mgr.audit_log(&self.who, 1000) {
                Ok(a) => self.audit = a,
                Err(e) => self.error(e),
            }
        } else {
            self.tab = Tab::Servers;
        }
    }

    fn open_session(&mut self, server: &Server, kind: TransportKind) {
        let repaint = self.ctx.clone();
        let notify = Arc::new(move || repaint.request_repaint());
        match self.mgr.open_terminal(&self.who, server.id, kind, 30, 100, notify) {
            Ok(term) => {
                self.next_session_id += 1;
                self.sessions.push(Session { id: self.next_session_id, term, open: true });
            }
            Err(e) => self.error(e),
        }
    }

    fn open_vnc(&mut self, server: &Server) {
        let repaint = self.ctx.clone();
        match self.mgr.open_vnc(&self.who, server.id, Arc::new(move || repaint.request_repaint())) {
            Ok(session) => {
                self.next_session_id += 1;
                self.displays.push(VncView::new(self.next_session_id, session));
            }
            Err(e) => self.error(e),
        }
    }

    fn show(&mut self, ui: &mut Ui) -> MainOutcome {
        let mut outcome = MainOutcome::Stay;
        let ctx = ui.ctx().clone();

        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("manageriliki").strong().size(16.0));
                ui.separator();
                let mut tab = self.tab;
                ui.selectable_value(&mut tab, Tab::Servers, "Servers");
                if self.is_admin() {
                    ui.selectable_value(&mut tab, Tab::Users, "Users");
                    ui.selectable_value(&mut tab, Tab::Audit, "Audit log");
                }
                if tab != self.tab {
                    self.tab = tab;
                    self.reload();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Log out").clicked() {
                        outcome = MainOutcome::Logout;
                    }
                    if ui.button("Change password").clicked() {
                        self.password_dialog = Some(PasswordDialog::own());
                    }
                    ui.label(
                        RichText::new(format!("{} · {}", self.who.username, self.who.role))
                            .color(ui.visuals().hyperlink_color),
                    );
                });
            });
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                match &self.message {
                    Some((msg, err, at)) if at.elapsed() < Duration::from_secs(8) => {
                        let color = if *err { Color32::LIGHT_RED } else { Color32::LIGHT_GREEN };
                        ui.colored_label(color, msg);
                        ctx.request_repaint_after(Duration::from_secs(1));
                    }
                    _ => {
                        ui.label(
                            RichText::new(format!(
                                "{} server(s) · {} open session(s)",
                                self.servers.len(),
                                self.sessions.len() + self.displays.len()
                            ))
                            .weak(),
                        );
                    }
                }
            });
        });

        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Servers => self.servers_tab(ui),
            Tab::Users => self.users_tab(ui),
            Tab::Audit => self.audit_tab(ui),
        });

        self.session_windows(&ctx);
        self.dialogs(&ctx);
        outcome
    }

    fn servers_tab(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.heading("Servers");
            ui.add_space(12.0);
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter by name, group or host").desired_width(240.0));
            if ui.button("⟳ Refresh").clicked() {
                self.reload();
                self.monitor.refresh();
            }
            if self.is_admin() && ui.button("+ Add server").clicked() {
                self.server_editor = Some(ServerEditor::new(None));
            }
        });
        ui.add_space(6.0);

        let filter = self.filter.to_lowercase();
        let visible: Vec<Server> = self
            .servers
            .iter()
            .filter(|s| {
                filter.is_empty()
                    || s.name.to_lowercase().contains(&filter)
                    || s.group.to_lowercase().contains(&filter)
                    || s.ssh.as_ref().is_some_and(|c| c.host.to_lowercase().contains(&filter))
            })
            .cloned()
            .collect();
        if visible.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new(if self.servers.is_empty() {
                if self.is_admin() { "No servers yet. Click “Add server” to create one." } else { "No servers yet. Ask an admin to add some." }
            } else {
                "No servers match the filter."
            }).weak());
            return;
        }

        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("servers").striped(true).num_columns(7).spacing([14.0, 8.0]).show(ui, |ui| {
                for h in ["", "Name", "Group", "SSH", "Serial", "VNC", "Actions"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for s in &visible {
                    let (color, tip) = match self.monitor.get(s.id) {
                        Status::Online(ms) if s.ssh.is_some() => (Some(Color32::from_rgb(0x3c, 0xc8, 0x6e)), format!("online, {ms} ms")),
                        Status::Online(_) => (Some(Color32::from_rgb(0x3c, 0xc8, 0x6e)), "serial device present".into()),
                        Status::Offline(e) => (Some(Color32::from_rgb(0xe0, 0x50, 0x50)), format!("offline: {e}")),
                        Status::Unknown => (None, "checking…".into()),
                    };
                    status_dot(ui, color).on_hover_text(tip);
                    let name = ui.label(RichText::new(&s.name).strong());
                    let mut details = String::new();
                    if !s.notes.is_empty() {
                        details.push_str(&format!("{}\n", s.notes));
                    }
                    details.push_str(&format!("Connect: {} and above", s.required_role));
                    if let Some(fp) = &s.host_key_fp {
                        details.push_str(&format!("\nHost key: {fp}"));
                    }
                    name.on_hover_text(details);
                    ui.label(&s.group);
                    ui.label(s.ssh.as_ref().map(|c| format!("{}@{}:{} ({})", c.username, c.host, c.port, c.auth.label())).unwrap_or_else(|| "—".into()));
                    ui.label(s.serial.as_ref().map(|c| c.summary()).unwrap_or_else(|| "—".into()));
                    ui.label(s.vnc.as_ref().map(|c| c.summary(s.ssh.as_ref())).unwrap_or_else(|| "—".into()));
                    ui.horizontal(|ui| {
                        let connect = self.can_connect(s);
                        if ui.add_enabled(connect && s.ssh.is_some(), egui::Button::new("SSH")).clicked() {
                            self.open_session(s, TransportKind::Ssh);
                        }
                        if ui.add_enabled(connect && s.serial.is_some(), egui::Button::new("Serial")).clicked() {
                            self.open_session(s, TransportKind::Serial);
                        }
                        if ui.add_enabled(connect && s.vnc.is_some(), egui::Button::new("VNC")).clicked() {
                            self.open_vnc(s);
                        }
                        if ui.add_enabled(connect && s.ssh.is_some(), egui::Button::new("Run…")).clicked() {
                            self.command = Some(CommandDialog {
                                server: s.clone(),
                                command: String::new(),
                                scope: Scope::One,
                                rx: None,
                                results: Vec::new(),
                                pending: 0,
                            });
                        }
                        if self.is_admin() {
                            ui.menu_button("More…", |ui| {
                                if ui.button("Edit").clicked() {
                                    self.server_editor = Some(ServerEditor::new(Some(s)));
                                }
                                if ui.add_enabled(s.host_key_fp.is_some(), egui::Button::new("Forget host key")).clicked() {
                                    self.confirm = Some((
                                        format!("Forget the trusted SSH host key of “{}”? The next connection will trust whatever key the server presents.", s.name),
                                        Confirm::ResetHostKey(s.id),
                                    ));
                                }
                                if ui.button(RichText::new("Delete").color(Color32::LIGHT_RED)).clicked() {
                                    self.confirm = Some((format!("Delete server “{}”?", s.name), Confirm::DeleteServer(s.id)));
                                }
                            });
                        }
                    });
                    ui.end_row();
                }
            });
        });
    }

    fn users_tab(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.heading("Users");
            if ui.button("+ Add user").clicked() {
                self.user_editor = Some(UserEditor::new(None));
            }
        });
        ui.add_space(6.0);
        let users = self.users.clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("users").striped(true).num_columns(6).spacing([18.0, 8.0]).show(ui, |ui| {
                for h in ["Username", "Role", "State", "Last login", "Created", "Actions"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for u in &users {
                    let you = if u.id == self.who.user_id { " (you)" } else { "" };
                    ui.label(RichText::new(format!("{}{you}", u.username)).strong());
                    ui.label(u.role.as_str());
                    if u.disabled {
                        ui.colored_label(Color32::LIGHT_RED, "disabled");
                    } else {
                        ui.colored_label(Color32::LIGHT_GREEN, "active");
                    }
                    ui.label(u.last_login.map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_else(|| "never".into()));
                    ui.label(u.created_at.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string());
                    ui.horizontal(|ui| {
                        if ui.button("Edit").clicked() {
                            self.user_editor = Some(UserEditor::new(Some(u)));
                        }
                        if ui.button("Reset password").clicked() {
                            self.password_dialog = Some(PasswordDialog::reset(u));
                        }
                        if ui.add_enabled(u.id != self.who.user_id, egui::Button::new(RichText::new("Delete").color(Color32::LIGHT_RED))).clicked() {
                            self.confirm = Some((format!("Delete user “{}”?", u.username), Confirm::DeleteUser(u.id)));
                        }
                    });
                    ui.end_row();
                }
            });
        });
    }

    fn audit_tab(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.heading("Audit log");
            if ui.button("⟳ Refresh").clicked() {
                self.reload();
            }
            ui.label(RichText::new("latest 1000 entries").weak());
        });
        ui.add_space(6.0);
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("audit").striped(true).num_columns(5).spacing([18.0, 4.0]).show(ui, |ui| {
                for h in ["Time", "User", "Action", "Target", "Detail"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for a in &self.audit {
                    ui.label(RichText::new(a.ts.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string()).monospace());
                    ui.label(&a.username);
                    let bad = a.action.contains("fail") || a.action.contains("mismatch");
                    ui.label(if bad { RichText::new(&a.action).color(Color32::LIGHT_RED) } else { RichText::new(&a.action) });
                    ui.label(&a.target);
                    ui.label(RichText::new(&a.detail).monospace());
                    ui.end_row();
                }
            });
        });
    }

    fn session_windows(&mut self, ctx: &egui::Context) {
        for session in &mut self.sessions {
            let id = egui::Id::new(("session", session.id));
            egui::Window::new(&session.term.title)
                .id(id)
                .open(&mut session.open)
                .default_size([820.0, 480.0])
                .min_size([320.0, 160.0])
                .resizable(true)
                .collapsible(true)
                .show(ctx, |ui| {
                    term_widget::show(ui, &session.term, id.with("term"));
                });
        }
        // Dropping a Terminal disconnects it.
        self.sessions.retain(|s| s.open);

        for view in &mut self.displays {
            let mut open = view.open;
            egui::Window::new(view.session.title.clone())
                .id(egui::Id::new(("display", view.id)))
                .open(&mut open)
                .default_size([960.0, 600.0])
                .min_size([320.0, 200.0])
                .resizable(true)
                .collapsible(true)
                .show(ctx, |ui| view.show(ui));
            view.open = open;
        }
        self.displays.retain(|d| d.open);
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(editor) = &mut self.server_editor {
            match editor.show(ctx) {
                editors::DialogResult::Open => {}
                editors::DialogResult::Cancel => self.server_editor = None,
                editors::DialogResult::Submit => match editor.to_input() {
                    Ok(input) => match self.mgr.save_server(&self.who, editor.id, input) {
                        Ok(_) => {
                            self.server_editor = None;
                            self.info("server saved");
                            self.reload();
                        }
                        Err(e) => editor.error = Some(e.to_string()),
                    },
                    Err(e) => editor.error = Some(e),
                },
            }
        }

        if let Some(editor) = &mut self.user_editor {
            match editor.show(ctx) {
                editors::DialogResult::Open => {}
                editors::DialogResult::Cancel => self.user_editor = None,
                editors::DialogResult::Submit => {
                    let result = match editor.id {
                        None => self.mgr.create_user(&self.who, editor.username.trim(), &editor.password, editor.role).map(|_| ()),
                        Some(id) => self.mgr.update_user(&self.who, id, editor.role, editor.disabled),
                    };
                    match result {
                        Ok(()) => {
                            self.user_editor = None;
                            self.info("user saved");
                            self.reload();
                        }
                        Err(e) => editor.error = Some(e.to_string()),
                    }
                }
            }
        }

        if let Some(dialog) = &mut self.password_dialog {
            match dialog.show(ctx) {
                editors::DialogResult::Open => {}
                editors::DialogResult::Cancel => self.password_dialog = None,
                editors::DialogResult::Submit => {
                    let result = if dialog.new != dialog.confirm {
                        Err("passwords do not match".to_string())
                    } else {
                        match dialog.user_id {
                            None => self.mgr.change_own_password(&self.who, &dialog.old, &dialog.new),
                            Some(id) => self.mgr.reset_password(&self.who, id, &dialog.new),
                        }
                        .map_err(|e| e.to_string())
                    };
                    match result {
                        Ok(()) => {
                            self.password_dialog = None;
                            self.info("password changed");
                        }
                        Err(e) => dialog.error = Some(e),
                    }
                }
            }
        }

        if let Some((message, _)) = &self.confirm {
            let message = message.clone();
            let mut decision = None;
            let modal = egui::Modal::new(egui::Id::new("confirm")).show(ctx, |ui| {
                ui.set_max_width(380.0);
                ui.heading("Confirm");
                ui.label(message);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Yes").color(Color32::LIGHT_RED)).clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
            if modal.should_close() && decision.is_none() {
                decision = Some(false);
            }
            if let Some(yes) = decision {
                let (_, action) = self.confirm.take().expect("confirm is open");
                if yes {
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
                }
            }
        }

        self.command_window(ctx);
    }

    fn command_window(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.command else { return };
        if let Some(rx) = &dialog.rx {
            while let Ok(result) = rx.try_recv() {
                dialog.results.push(result);
                dialog.pending -= 1;
            }
            if dialog.pending == 0 {
                dialog.rx = None;
            }
        }

        let mut open = true;
        let mut run = false;
        egui::Window::new("Run command over SSH")
            .open(&mut open)
            .default_size([640.0, 420.0])
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Run on:");
                    ui.radio_value(&mut dialog.scope, Scope::One, &dialog.server.name);
                    if !dialog.server.group.is_empty() {
                        ui.radio_value(&mut dialog.scope, Scope::Group, format!("group “{}”", dialog.server.group));
                    }
                    ui.radio_value(&mut dialog.scope, Scope::All, "all servers");
                });
                ui.horizontal(|ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut dialog.command)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("e.g. uptime")
                            .desired_width(ui.available_width() - 70.0),
                    );
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let can_run = dialog.rx.is_none() && !dialog.command.trim().is_empty();
                    if (ui.add_enabled(can_run, egui::Button::new("Run")).clicked() || enter) && can_run {
                        run = true;
                    }
                });
                if dialog.pending > 0 {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("waiting for {} server(s)…", dialog.pending));
                    });
                }
                ui.separator();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for (name, result) in &dialog.results {
                        match result {
                            Ok(out) => {
                                let ok = out.exit_status == 0;
                                let header = RichText::new(format!("{name} — exit {}", out.exit_status))
                                    .color(if ok { Color32::LIGHT_GREEN } else { Color32::LIGHT_RED });
                                egui::CollapsingHeader::new(header).id_salt(name).default_open(true).show(ui, |ui| {
                                    if !out.stdout.is_empty() {
                                        ui.label(RichText::new(out.stdout.trim_end()).monospace());
                                    }
                                    if !out.stderr.is_empty() {
                                        ui.label(RichText::new(out.stderr.trim_end()).monospace().color(Color32::from_rgb(0xf0, 0xa0, 0x60)));
                                    }
                                });
                            }
                            Err(e) => {
                                ui.colored_label(Color32::LIGHT_RED, format!("{name} — failed: {e}"));
                            }
                        }
                    }
                });
            });

        if run {
            let targets: Vec<Server> = self
                .servers
                .iter()
                .filter(|s| s.ssh.is_some() && self.who.role >= s.required_role)
                .filter(|s| match dialog.scope {
                    Scope::One => s.id == dialog.server.id,
                    Scope::Group => s.group == dialog.server.group,
                    Scope::All => true,
                })
                .cloned()
                .collect();
            let repaint = self.ctx.clone();
            dialog.results.clear();
            dialog.pending = targets.len();
            dialog.rx = Some(self.mgr.run_command_many(&self.who, &targets, dialog.command.trim(), Arc::new(move || repaint.request_repaint())));
        }
        if !open {
            self.command = None;
        }
    }
}
