mod containers_view;
mod editors;
mod servers;
mod term_widget;
mod theme;
mod vnc_view;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use containers_view::ContainersView;
use editors::{GroupEditor, PasswordDialog, ServerEditor, UserEditor};
use eframe::egui::{self, Align, Align2, Color32, CornerRadius, FontId, Margin, RichText, Sense, Stroke, Ui, vec2};
use manageriliki_core::{
    AuditEntry, CommandOutput, Group, Identity, Manager, Result as CoreResult, Role, Server,
    StatusMonitor, Terminal, TransportKind, User,
};
use vnc_view::VncView;

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
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([820.0, 480.0]),
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
        theme::apply(&ctx);
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
                        ui.colored_label(theme::DANGER, e);
                    }
                    ui.add_space(4.0);
                    let w = ui.available_width();
                    if ui.add(theme::primary("Create admin").min_size(vec2(w, 34.0))).clicked() || enter {
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
                        ui.colored_label(theme::DANGER, e);
                    }
                    ui.add_space(4.0);
                    let w = ui.available_width();
                    if ui.add(theme::primary("Sign in").min_size(vec2(w, 34.0))).clicked() || enter {
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
            ui.add_space((ui.available_height() * 0.22).max(20.0));
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 190.0).max(0.0) / 2.0);
                logo(ui, 30.0);
                ui.label(RichText::new("manageriliki").size(26.0).strong().color(theme::TEXT));
            });
            ui.label(RichText::new("Local server management over SSH, serial and VNC").color(theme::MUTED));
            ui.add_space(18.0);
            theme::card().inner_margin(Margin::same(22)).show(ui, |ui| {
                ui.set_width(340.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(title).size(18.0).strong().color(theme::TEXT));
                    ui.label(RichText::new(subtitle).small().color(theme::MUTED));
                    ui.add_space(10.0);
                    body(ui);
                });
            });
        });
    });
}

fn field(ui: &mut Ui, label: &str, edit: egui::TextEdit) -> egui::Response {
    let label = ui.label(RichText::new(label).color(theme::SUBTLE));
    ui.add(edit.desired_width(f32::INFINITY).margin(vec2(8.0, 6.0))).labelled_by(label.id)
}

/// The app mark: two stacked server blades on an accent tile.
fn logo(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same((size * 0.25) as u8), theme::ACCENT);
    let inner = rect.shrink(size * 0.22);
    let h = inner.height() * 0.42;
    for top in [inner.top(), inner.bottom() - h] {
        let blade = egui::Rect::from_min_size(egui::pos2(inner.left(), top), vec2(inner.width(), h));
        p.rect_stroke(blade, CornerRadius::same(2), Stroke::new(1.6, theme::SIDEBAR), egui::StrokeKind::Inside);
        p.circle_filled(egui::pos2(blade.left() + h * 0.55, blade.center().y), 1.3, theme::SIDEBAR);
    }
}

/// A full-width sidebar navigation entry.
fn nav_item(ui: &mut Ui, label: &str, count: Option<usize>, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label));
    let fill = if selected {
        theme::SURFACE_2
    } else if response.hovered() {
        Color32::from_rgb(0x15, 0x18, 0x1d)
    } else {
        Color32::TRANSPARENT
    };
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(7), fill);
    if selected {
        let bar = egui::Rect::from_min_size(rect.left_top() + vec2(0.0, 8.0), vec2(3.0, rect.height() - 16.0));
        p.rect_filled(bar, CornerRadius::same(2), theme::ACCENT);
    }
    let color = if selected { theme::TEXT } else { theme::SUBTLE };
    p.text(rect.left_center() + vec2(14.0, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(14.0), color);
    if let Some(n) = count {
        p.text(rect.right_center() - vec2(12.0, 0.0), Align2::RIGHT_CENTER, n, FontId::proportional(12.0), theme::MUTED);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// ---- main UI ----

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Servers,
    Users,
    Audit,
    Settings,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Servers => "Servers",
            Page::Users => "Users",
            Page::Audit => "Audit log",
            Page::Settings => "Settings",
        }
    }
}

/// How the Servers page lays out the inventory.
#[derive(PartialEq, Clone, Copy)]
enum ServerLayout {
    /// Cards grouped by server group.
    Cards,
    /// A dense grouped table with an inspector for the selected server.
    Compact,
}

#[derive(PartialEq)]
enum MainOutcome {
    Stay,
    Logout,
}

enum Confirm {
    DeleteServer(i64),
    DeleteUser(i64),
    DeleteGroup(i64),
    ResetHostKey(i64),
}

enum SessionView {
    Terminal(Terminal),
    Vnc(VncView),
    Containers(Box<ContainersView>),
}

/// An open session, docked as a tab next to the current page.
struct Session {
    id: u64,
    /// Short tag shown on the tab, e.g. `SSH`.
    kind: &'static str,
    label: String,
    view: SessionView,
}

impl Session {
    #[cfg(test)]
    fn terminal(&self) -> Option<&Terminal> {
        match &self.view {
            SessionView::Terminal(t) => Some(t),
            _ => None,
        }
    }

    #[cfg(test)]
    fn vnc(&self) -> Option<&VncView> {
        match &self.view {
            SessionView::Vnc(v) => Some(v),
            _ => None,
        }
    }
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
    page: Page,
    layout: ServerLayout,
    /// The session tab in front, or `None` for the page.
    focus: Option<u64>,
    servers: Vec<Server>,
    users: Vec<User>,
    groups: Vec<Group>,
    new_group: String,
    audit: Vec<AuditEntry>,
    filter: String,
    /// Server shown in the compact layout's inspector.
    selected: Option<i64>,
    monitor: StatusMonitor,
    sessions: Vec<Session>,
    next_session_id: u64,
    server_editor: Option<ServerEditor>,
    user_editor: Option<UserEditor>,
    group_editor: Option<GroupEditor>,
    password_dialog: Option<PasswordDialog>,
    confirm: Option<(String, Confirm)>,
    command: Option<CommandDialog>,
    message: Option<(String, bool, Instant)>,
    collapsed: HashSet<String>,
}

impl MainUi {
    fn new(mgr: Manager, who: Identity, ctx: egui::Context) -> Self {
        let repaint = ctx.clone();
        let mut ui = MainUi {
            monitor: StatusMonitor::start(Duration::from_secs(20), Arc::new(move || repaint.request_repaint())),
            mgr,
            who,
            ctx,
            page: Page::Servers,
            layout: ServerLayout::Cards,
            focus: None,
            servers: Vec::new(),
            users: Vec::new(),
            groups: Vec::new(),
            new_group: String::new(),
            audit: Vec::new(),
            filter: String::new(),
            selected: None,
            sessions: Vec::new(),
            next_session_id: 0,
            server_editor: None,
            user_editor: None,
            group_editor: None,
            password_dialog: None,
            confirm: None,
            command: None,
            message: None,
            collapsed: HashSet::new(),
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
            match self.mgr.list_groups(&self.who) {
                Ok(g) => self.groups = g,
                Err(e) => self.error(e),
            }
            match self.mgr.audit_log(&self.who, 1000) {
                Ok(a) => self.audit = a,
                Err(e) => self.error(e),
            }
        } else {
            self.page = Page::Servers;
        }
    }

    fn dock(&mut self, kind: &'static str, label: String, view: SessionView) {
        self.next_session_id += 1;
        self.sessions.push(Session { id: self.next_session_id, kind, label, view });
        self.focus = Some(self.next_session_id);
    }

    fn open_session(&mut self, server: &Server, kind: TransportKind) {
        let repaint = self.ctx.clone();
        let notify = Arc::new(move || repaint.request_repaint());
        match self.mgr.open_terminal(&self.who, server.id, kind, 30, 100, notify) {
            Ok(term) => {
                let tag = if kind == TransportKind::Serial { "SERIAL" } else { "SSH" };
                self.dock(tag, server.name.clone(), SessionView::Terminal(term));
            }
            Err(e) => self.error(e),
        }
    }

    fn open_vnc(&mut self, server: &Server) {
        let repaint = self.ctx.clone();
        match self.mgr.open_vnc(&self.who, server.id, Arc::new(move || repaint.request_repaint())) {
            Ok(session) => {
                let id = self.next_session_id + 1;
                self.dock("VNC", server.name.clone(), SessionView::Vnc(VncView::new(id, session)));
            }
            Err(e) => self.error(e),
        }
    }

    fn open_containers(&mut self, server: &Server) {
        let existing = self.sessions.iter().find(|s| match &s.view {
            SessionView::Containers(v) => v.server.id == server.id,
            _ => false,
        });
        if let Some(s) = existing {
            self.focus = Some(s.id);
            return;
        }
        let id = self.next_session_id + 1;
        let view = ContainersView::new(id, self.mgr.clone(), self.who.clone(), server.clone(), &self.ctx);
        self.dock("CONTAINERS", server.name.clone(), SessionView::Containers(Box::new(view)));
    }

    fn open_container_shell(&mut self, server_id: i64, container: &manageriliki_core::Container) {
        let repaint = self.ctx.clone();
        let notify = Arc::new(move || repaint.request_repaint());
        match self.mgr.open_container_shell(&self.who, server_id, container, 30, 100, notify) {
            Ok(term) => {
                let server = self.servers.iter().find(|s| s.id == server_id).map_or("", |s| s.name.as_str());
                let label = format!("{server} › {}", container.display_name());
                self.dock("SHELL", label, SessionView::Terminal(term));
            }
            Err(e) => self.error(e),
        }
    }

    fn open_command(&mut self, server: &Server, scope: Scope) {
        self.command = Some(CommandDialog {
            server: server.clone(),
            command: String::new(),
            scope,
            rx: None,
            results: Vec::new(),
            pending: 0,
        });
    }

    fn show(&mut self, ui: &mut Ui) -> MainOutcome {
        let mut outcome = MainOutcome::Stay;
        let ctx = ui.ctx().clone();

        egui::Panel::left("sidebar")
            .resizable(false)
            .exact_size(224.0)
            .frame(egui::Frame::new().fill(theme::SIDEBAR).inner_margin(Margin::symmetric(12, 16)))
            .show(ui, |ui| {
                if self.sidebar(ui) {
                    outcome = MainOutcome::Logout;
                }
            });

        egui::Panel::bottom("status")
            .frame(egui::Frame::new().fill(theme::SIDEBAR).inner_margin(Margin::symmetric(20, 5)))
            .show(ui, |ui| self.status_bar(ui));

        if !self.sessions.is_empty() {
            egui::Panel::top("tabs")
                .frame(egui::Frame::new().fill(theme::SIDEBAR).inner_margin(Margin { left: 8, right: 8, top: 6, bottom: 0 }))
                .show(ui, |ui| self.tab_strip(ui));
        }

        let focused = self.focus.and_then(|id| self.sessions.iter().position(|s| s.id == id));
        let frame = match focused {
            Some(_) => egui::Frame::new().fill(theme::SIDEBAR).inner_margin(Margin::same(6)),
            None => egui::Frame::new().fill(theme::BG).inner_margin(Margin::symmetric(28, 20)),
        };
        egui::CentralPanel::default().frame(frame).show(ui, |ui| match focused {
            Some(i) => self.session_body(ui, i),
            None => match self.page {
                Page::Servers => self.servers_page(ui),
                Page::Users => self.users_page(ui),
                Page::Audit => self.audit_page(ui),
                Page::Settings => self.settings_page(ui),
            },
        });

        self.dialogs(&ctx);
        outcome
    }

    /// Navigation and account controls. Returns true to log out.
    fn sidebar(&mut self, ui: &mut Ui) -> bool {
        let mut logout = false;
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            logo(ui, 26.0);
            ui.label(RichText::new("manageriliki").strong().size(15.0).color(theme::TEXT));
        });
        ui.add_space(20.0);

        let mut pages = vec![(Page::Servers, Some(self.servers.len()))];
        if self.is_admin() {
            pages.extend([(Page::Users, Some(self.users.len())), (Page::Audit, None), (Page::Settings, None)]);
        }
        ui.spacing_mut().item_spacing.y = 2.0;
        for (page, count) in pages {
            let selected = self.page == page && self.focus.is_none();
            if nav_item(ui, page.title(), count, selected).clicked() {
                self.focus = None;
                if self.page != page {
                    self.page = page;
                    self.reload();
                }
            }
        }

        ui.with_layout(egui::Layout::bottom_up(Align::Min), |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 8.0) / 2.0;
                if ui.add(theme::ghost("Password").min_size(vec2(w, 28.0))).clicked() {
                    self.password_dialog = Some(PasswordDialog::own());
                }
                if ui.add(theme::ghost("Log out").min_size(vec2(w, 28.0))).clicked() {
                    logout = true;
                }
            });
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                ui.painter().circle_filled(rect.center(), 15.0, Color32::from_rgb(0x23, 0x30, 0x3f));
                let initial = self.who.username.chars().next().unwrap_or('?').to_uppercase().to_string();
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, initial, FontId::proportional(13.0), theme::ACCENT_TEXT);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(RichText::new(&self.who.username).strong().color(theme::TEXT));
                    let role = match self.who.role {
                        Role::Admin => "Administrator",
                        Role::Operator => "Operator",
                        Role::Viewer => "Viewer",
                    };
                    ui.label(RichText::new(role).small().color(theme::MUTED));
                });
            });
            theme::rule(ui);
        });
        logout
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            match &self.message {
                Some((msg, err, at)) if at.elapsed() < Duration::from_secs(8) => {
                    ui.colored_label(if *err { theme::DANGER } else { theme::SUCCESS }, msg);
                    self.ctx.request_repaint_after(Duration::from_secs(1));
                }
                _ => {
                    ui.label(
                        RichText::new(format!("{} server(s) · {} open session(s)", self.servers.len(), self.sessions.len()))
                            .small()
                            .color(theme::MUTED),
                    );
                }
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("Data: {}", self.mgr.data_dir().display())).small().color(theme::MUTED));
            });
        });
    }

    /// The page tab followed by one tab per open session.
    fn tab_strip(&mut self, ui: &mut Ui) {
        let mut focus = None;
        let mut close = None;
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if tab(ui, None, self.page.title(), self.focus.is_none(), false).0 {
                    focus = Some(None);
                }
                for s in &self.sessions {
                    let (clicked, closed) = tab(ui, Some(s.kind), &s.label, self.focus == Some(s.id), true);
                    if clicked {
                        focus = Some(Some(s.id));
                    }
                    if closed {
                        close = Some(s.id);
                    }
                }
            });
        });
        if let Some(f) = focus {
            self.focus = f;
        }
        if let Some(id) = close {
            // Dropping a session disconnects it.
            let idx = self.sessions.iter().position(|s| s.id == id).expect("tab exists");
            self.sessions.remove(idx);
            if self.focus == Some(id) {
                self.focus = self.sessions.get(idx.saturating_sub(1)).filter(|_| idx > 0).map(|s| s.id);
            }
        }
    }

    fn session_body(&mut self, ui: &mut Ui, idx: usize) {
        let session = &mut self.sessions[idx];
        let id = egui::Id::new(("session", session.id));
        let mut shell = None;
        match &mut session.view {
            SessionView::Terminal(term) => term_widget::show(ui, term, id.with("term")),
            SessionView::Vnc(view) => view.show(ui),
            SessionView::Containers(view) => {
                egui::Frame::new().inner_margin(Margin::same(10)).show(ui, |ui| {
                    if let Some(c) = view.show(ui) {
                        shell = Some((view.server.id, c));
                    }
                });
            }
        }
        if let Some((server_id, c)) = shell {
            self.open_container_shell(server_id, &c);
        }
    }

    fn page_header(ui: &mut Ui, title: &str, subtitle: &str, actions: impl FnOnce(&mut Ui)) {
        ui.horizontal(|ui| {
            theme::page_title(ui, title, subtitle);
            ui.with_layout(egui::Layout::right_to_left(Align::Center), actions);
        });
        ui.add_space(16.0);
    }

    fn users_page(&mut self, ui: &mut Ui) {
        let mut add = false;
        Self::page_header(ui, "Users", &format!("{} account(s)", self.users.len()), |ui| {
            add = ui.add(theme::primary("+ Add user")).clicked();
        });
        if add {
            self.user_editor = Some(UserEditor::new(None));
        }
        let users = self.users.clone();
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                egui::Grid::new("users").striped(true).num_columns(6).spacing([22.0, 10.0]).show(ui, |ui| {
                    for h in ["Username", "Role", "State", "Last login", "Created", ""] {
                        ui.label(theme::caption(h));
                    }
                    ui.end_row();
                    for u in &users {
                        let you = if u.id == self.who.user_id { " (you)" } else { "" };
                        ui.label(RichText::new(format!("{}{you}", u.username)).strong().color(theme::TEXT));
                        ui.label(u.role.as_str());
                        if u.disabled {
                            ui.colored_label(theme::OFFLINE_TEXT, "disabled");
                        } else {
                            ui.colored_label(theme::ONLINE_TEXT, "active");
                        }
                        ui.label(u.last_login.map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_else(|| "never".into()));
                        ui.label(u.created_at.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string());
                        ui.horizontal(|ui| {
                            if ui.add(theme::ghost("Edit").small()).clicked() {
                                self.user_editor = Some(UserEditor::new(Some(u)));
                            }
                            if ui.add(theme::ghost("Reset password").small()).clicked() {
                                self.password_dialog = Some(PasswordDialog::reset(u));
                            }
                            if ui.add_enabled(u.id != self.who.user_id, theme::danger("Delete").small()).clicked() {
                                self.confirm = Some((format!("Delete user “{}”?", u.username), Confirm::DeleteUser(u.id)));
                            }
                        });
                        ui.end_row();
                    }
                });
            });
        });
    }

    fn audit_page(&mut self, ui: &mut Ui) {
        let mut refresh = false;
        Self::page_header(ui, "Audit log", "Latest 1000 entries", |ui| {
            refresh = ui.add(theme::ghost("⟳ Refresh")).clicked();
        });
        if refresh {
            self.reload();
        }
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                egui::Grid::new("audit").striped(true).num_columns(5).spacing([22.0, 6.0]).show(ui, |ui| {
                    for h in ["Time", "User", "Action", "Target", "Detail"] {
                        ui.label(theme::caption(h));
                    }
                    ui.end_row();
                    for a in &self.audit {
                        ui.label(theme::mono(a.ts.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string()));
                        ui.label(&a.username);
                        let bad = a.action.contains("fail") || a.action.contains("mismatch");
                        ui.label(if bad { RichText::new(&a.action).color(theme::OFFLINE_TEXT) } else { RichText::new(&a.action) });
                        ui.label(&a.target);
                        ui.label(theme::mono(&a.detail));
                        ui.end_row();
                    }
                });
            });
        });
    }

    fn add_group(&mut self) {
        match self.mgr.create_group(&self.who, &self.new_group) {
            Ok(g) => {
                self.info(format!("group “{}” created", g.name));
                self.new_group.clear();
                self.reload();
            }
            Err(e) => self.error(e),
        }
    }

    fn settings_page(&mut self, ui: &mut Ui) {
        Self::page_header(ui, "Settings", "Configuration shared by everyone using this data directory", |_| {});
        let groups = self.groups.clone();
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width().min(720.0));
            ui.label(RichText::new("Server groups").strong().size(16.0).color(theme::TEXT));
            ui.label(RichText::new("Groups you create here are offered in the server form, and “Run on group…” targets every server in one.").color(theme::MUTED));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.new_group).hint_text("New group name").desired_width(260.0).margin(vec2(8.0, 5.0)));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let can_add = !self.new_group.trim().is_empty();
                if (ui.add_enabled(can_add, theme::primary("+ Add group")).clicked() || enter) && can_add {
                    self.add_group();
                }
            });
            ui.add_space(10.0);
            if groups.is_empty() {
                ui.label(RichText::new("No groups yet.").color(theme::MUTED));
                return;
            }
            egui::Grid::new("groups").striped(true).num_columns(3).spacing([28.0, 8.0]).show(ui, |ui| {
                for h in ["Group", "Servers", ""] {
                    ui.label(theme::caption(h));
                }
                ui.end_row();
                for g in &groups {
                    ui.label(RichText::new(&g.name).strong().color(theme::TEXT));
                    ui.label(g.server_count.to_string());
                    ui.horizontal(|ui| {
                        if ui.add(theme::ghost("Rename").small()).clicked() {
                            self.group_editor = Some(GroupEditor::new(g));
                        }
                        if ui.add(theme::danger("Delete").small()).clicked() {
                            let message = match g.server_count {
                                0 => format!("Delete group “{}”?", g.name),
                                n => format!("Delete group “{}”? Its {n} server(s) are kept, without a group.", g.name),
                            };
                            self.confirm = Some((message, Confirm::DeleteGroup(g.id)));
                        }
                    });
                    ui.end_row();
                }
            });
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(editor) = &mut self.server_editor {
            match editor.show(ctx, &self.groups) {
                editors::DialogResult::Open => {}
                editors::DialogResult::Cancel => self.server_editor = None,
                editors::DialogResult::Submit => match editor.to_input() {
                    Ok(input) => match self.mgr.save_server(&self.who, editor.id, input) {
                        Ok(id) => {
                            self.server_editor = None;
                            self.selected = Some(id);
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

        if let Some(editor) = &mut self.group_editor {
            match editor.show(ctx) {
                editors::DialogResult::Open => {}
                editors::DialogResult::Cancel => self.group_editor = None,
                editors::DialogResult::Submit => match self.mgr.rename_group(&self.who, editor.id, &editor.name) {
                    Ok(()) => {
                        self.group_editor = None;
                        self.info("group renamed");
                        self.reload();
                    }
                    Err(e) => editor.error = Some(e.to_string()),
                },
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
                ui.set_max_width(400.0);
                ui.label(RichText::new("Are you sure?").size(17.0).strong().color(theme::TEXT));
                ui.add_space(4.0);
                ui.label(message);
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui.add(theme::danger("Yes")).clicked() {
                        decision = Some(true);
                    }
                    if ui.add(theme::ghost("Cancel")).clicked() {
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
                        Confirm::DeleteGroup(id) => self.mgr.delete_group(&self.who, id),
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
                            .margin(vec2(8.0, 5.0))
                            .desired_width(ui.available_width() - 70.0),
                    );
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let can_run = dialog.rx.is_none() && !dialog.command.trim().is_empty();
                    if (ui.add_enabled(can_run, theme::primary("Run")).clicked() || enter) && can_run {
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
                                    .color(if ok { theme::ONLINE_TEXT } else { theme::OFFLINE_TEXT });
                                egui::CollapsingHeader::new(header).id_salt(name).default_open(true).show(ui, |ui| {
                                    if !out.stdout.is_empty() {
                                        ui.label(RichText::new(out.stdout.trim_end()).monospace());
                                    }
                                    if !out.stderr.is_empty() {
                                        ui.label(RichText::new(out.stderr.trim_end()).monospace().color(theme::OFFLINE_TEXT));
                                    }
                                });
                            }
                            Err(e) => {
                                ui.colored_label(theme::DANGER, format!("{name} — failed: {e}"));
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

/// One tab in the strip. Returns (clicked, close clicked).
fn tab(ui: &mut Ui, kind: Option<&str>, label: &str, selected: bool, closable: bool) -> (bool, bool) {
    let mut clicked = false;
    let mut closed = false;
    let fill = if selected { theme::BG } else { Color32::TRANSPARENT };
    let radius = CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 };
    let response = egui::Frame::new().fill(fill).corner_radius(radius).inner_margin(Margin { left: 10, right: 6, top: 4, bottom: 4 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let color = if selected { theme::TEXT } else { theme::SUBTLE };
            let mut text = egui::text::LayoutJob::default();
            if let Some(kind) = kind {
                text.append(kind, 0.0, egui::TextFormat { font_id: FontId::proportional(10.5), color: theme::MUTED, ..Default::default() });
                text.append(" ", 0.0, egui::TextFormat::default());
            }
            text.append(label, 0.0, egui::TextFormat { font_id: FontId::proportional(13.0), color, ..Default::default() });
            if ui.add(egui::Button::new(text).frame(false)).clicked() {
                clicked = true;
            }
            if closable && ui.add(egui::Button::new(RichText::new("×").color(theme::MUTED)).frame(false)).on_hover_text("Close").clicked() {
                closed = true;
            }
        });
    });
    if selected {
        let r = response.response.rect;
        ui.painter().hline(r.x_range().shrink(4.0), r.top() + 1.0, Stroke::new(2.0, theme::ACCENT));
    }
    (clicked, closed)
}
