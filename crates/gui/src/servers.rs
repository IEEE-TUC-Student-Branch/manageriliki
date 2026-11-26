//! The Servers page: grouped cards, or a compact grouped table with an
//! inspector for the selected server.

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Margin, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2,
    vec2,
};
use manageriliki_core::{Role, Server, SshAuth, Status, TransportKind};

use crate::editors::ServerEditor;
use crate::theme;
use crate::{Confirm, MainUi, Scope, ServerLayout};

const CARD_MIN_WIDTH: f32 = 300.0;
const CARD_GAP: f32 = 12.0;
const ROW_HEIGHT: f32 = 32.0;

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Ssh,
    Serial,
    Vnc,
    Containers,
    Run,
}

/// The actions a server supports, most direct first.
fn actions(s: &Server) -> Vec<Action> {
    let mut out = Vec::new();
    if s.ssh.is_some() {
        out.push(Action::Ssh);
    }
    if s.serial.is_some() {
        out.push(Action::Serial);
    }
    if s.vnc.is_some() {
        out.push(Action::Vnc);
    }
    if s.ssh.is_some() {
        out.extend([Action::Containers, Action::Run]);
    }
    out
}

fn short_label(a: Action) -> &'static str {
    match a {
        Action::Ssh => "SSH",
        Action::Serial => "Serial",
        Action::Vnc => "VNC",
        Action::Containers => "Containers",
        Action::Run => "Run…",
    }
}

fn long_label(a: Action) -> &'static str {
    match a {
        Action::Ssh => "Open SSH shell",
        Action::Serial => "Serial console",
        Action::Vnc => "VNC display",
        Action::Containers => "Containers",
        Action::Run => "Run command…",
    }
}

/// Servers by group, in list order, with ungrouped servers last.
fn by_group(servers: &[Server]) -> Vec<(String, Vec<Server>)> {
    let mut groups: Vec<(String, Vec<Server>)> = Vec::new();
    for s in servers {
        match groups.iter_mut().find(|(g, _)| *g == s.group) {
            Some((_, list)) => list.push(s.clone()),
            None => groups.push((s.group.clone(), vec![s.clone()])),
        }
    }
    groups.sort_by_key(|(g, _)| g.is_empty());
    groups
}

fn group_title(group: &str) -> &str {
    if group.is_empty() { "Ungrouped" } else { group }
}

impl MainUi {
    fn visible_servers(&self) -> Vec<Server> {
        let filter = self.filter.to_lowercase();
        self.servers
            .iter()
            .filter(|s| {
                filter.is_empty()
                    || s.name.to_lowercase().contains(&filter)
                    || s.group.to_lowercase().contains(&filter)
                    || s.ssh.as_ref().is_some_and(|c| c.host.to_lowercase().contains(&filter))
            })
            .cloned()
            .collect()
    }

    fn run_action(&mut self, s: &Server, a: Action) {
        match a {
            Action::Ssh => self.open_session(s, TransportKind::Ssh),
            Action::Serial => self.open_session(s, TransportKind::Serial),
            Action::Vnc => self.open_vnc(s),
            Action::Containers => self.open_containers(s),
            Action::Run => self.open_command(s, Scope::One),
        }
    }

    fn connect_hint(&self) -> &'static str {
        if self.who.role < Role::Operator { "Viewers cannot connect" } else { "Only admins may connect to this server" }
    }

    pub(crate) fn servers_page(&mut self, ui: &mut Ui) {
        self.servers_header(ui);
        let visible = self.visible_servers();
        if visible.is_empty() {
            ui.add_space(30.0);
            let msg = match (self.servers.is_empty(), self.is_admin()) {
                (true, true) => "No servers yet. Click “Add server” to create one.",
                (true, false) => "No servers yet. Ask an admin to add some.",
                (false, _) => "No servers match the filter.",
            };
            ui.label(RichText::new(msg).color(theme::MUTED));
            return;
        }
        match self.layout {
            ServerLayout::Cards => self.cards(ui, &visible),
            ServerLayout::Compact => self.compact(ui, &visible),
        }
    }

    fn servers_header(&mut self, ui: &mut Ui) {
        let (mut online, mut offline) = (0, 0);
        for s in &self.servers {
            match self.monitor.get(s.id) {
                Status::Online(_) => online += 1,
                Status::Offline(_) => offline += 1,
                Status::Unknown => {}
            }
        }
        let summary =
            if self.servers.is_empty() { String::new() } else { format!("{online} online · {offline} offline") };
        ui.horizontal(|ui| {
            theme::page_title(ui, "Servers", &summary);
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if self.is_admin() && ui.add(theme::primary("+ Add server")).clicked() {
                    self.server_editor = Some(ServerEditor::new(None));
                }
                if ui.add(theme::ghost("⟳ Refresh")).clicked() {
                    self.reload();
                    self.monitor.refresh();
                }
                // Right-to-left: added last appears first.
                ui.spacing_mut().item_spacing.x = 2.0;
                for (layout, label) in [(ServerLayout::Compact, "Compact"), (ServerLayout::Cards, "Cards")] {
                    let on = self.layout == layout;
                    let b = egui::Button::selectable(on, label)
                        .fill(if on { theme::SURFACE_2 } else { Color32::TRANSPARENT })
                        .stroke(Stroke::new(1.0, if on { theme::BORDER_STRONG } else { Color32::TRANSPARENT }));
                    if ui.add(b).clicked() {
                        self.layout = layout;
                    }
                }
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("Filter by name, group or host")
                        .margin(vec2(8.0, 5.0))
                        .desired_width(240.0),
                );
            });
        });
        ui.add_space(18.0);
    }

    // ---- cards ----

    fn cards(&mut self, ui: &mut Ui, visible: &[Server]) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (group, servers) in by_group(visible) {
                ui.horizontal(|ui| {
                    ui.label(theme::caption(group_title(&group)));
                    let n = servers.len();
                    ui.label(
                        RichText::new(format!("{n} server{}", if n == 1 { "" } else { "s" }))
                            .small()
                            .color(theme::MUTED),
                    );
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        let target = servers.iter().find(|s| s.ssh.is_some() && self.can_connect(s));
                        if let (false, Some(target)) = (group.is_empty(), target)
                            && ui.add(theme::ghost("Run on group…").small()).clicked()
                        {
                            self.open_command(target, Scope::Group);
                        }
                        theme::rule(ui);
                    });
                });
                ui.add_space(6.0);
                let avail = ui.available_width();
                let cols = (((avail + CARD_GAP) / (CARD_MIN_WIDTH + CARD_GAP)).floor() as usize).max(1);
                let width = (avail - CARD_GAP * (cols - 1) as f32) / cols as f32;
                for row in servers.chunks(cols) {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = CARD_GAP;
                        for s in row {
                            self.server_card(ui, s, width);
                        }
                    });
                    ui.add_space(CARD_GAP - 6.0);
                }
                ui.add_space(22.0);
            }
        });
    }

    fn server_card(&mut self, ui: &mut Ui, s: &Server, width: f32) {
        let (dot, color, label) = theme::status(s, &self.monitor.get(s.id));
        let offline_reason = match self.monitor.get(s.id) {
            Status::Offline(e) => Some(e),
            _ => None,
        };
        // The card sits in a horizontal row; lay its contents out top to bottom.
        theme::card().show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_width(width - 34.0);
                ui.horizontal_top(|ui| {
                    let d = theme::dot(ui, dot);
                    if let Some(e) = &offline_reason {
                        d.on_hover_text(e);
                    }
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(RichText::new(&s.name).strong().size(15.0).color(theme::TEXT))
                            .on_hover_text(details(s));
                        ui.label(theme::mono(theme::address(s)));
                    });
                    ui.with_layout(egui::Layout::right_to_left(Align::Min), |ui| {
                        if self.is_admin() {
                            ui.menu_button(RichText::new("•••").color(theme::MUTED), |ui| self.admin_menu(ui, s))
                                .response
                                .on_hover_text("More actions");
                        }
                        let status = ui.label(RichText::new(label).small().color(color));
                        if let Some(e) = &offline_reason {
                            status.on_hover_text(e);
                        }
                    });
                });
                if !s.notes.is_empty() {
                    ui.label(RichText::new(&s.notes).small().color(theme::MUTED));
                }
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let connect = self.can_connect(s);
                    for (i, a) in actions(s).into_iter().enumerate() {
                        let b = if i == 0 { theme::soft(short_label(a)) } else { theme::ghost(short_label(a)) };
                        if ui.add_enabled(connect, b).on_disabled_hover_text(self.connect_hint()).clicked() {
                            self.run_action(s, a);
                        }
                    }
                });
            })
        });
    }

    fn admin_menu(&mut self, ui: &mut Ui, s: &Server) {
        if ui.button("Edit…").clicked() {
            self.server_editor = Some(ServerEditor::new(Some(s)));
        }
        if ui.add_enabled(s.host_key_fp.is_some(), egui::Button::new("Forget host key")).clicked() {
            self.confirm = Some((
                format!(
                    "Forget the trusted SSH host key of “{}”? The next connection will trust whatever key the server presents.",
                    s.name
                ),
                Confirm::ResetHostKey(s.id),
            ));
        }
        ui.separator();
        if ui.button(RichText::new("Delete").color(theme::DANGER)).clicked() {
            self.confirm = Some((format!("Delete server “{}”?", s.name), Confirm::DeleteServer(s.id)));
        }
    }

    // ---- compact ----

    fn compact(&mut self, ui: &mut Ui, visible: &[Server]) {
        // Rows in display order (groups expanded), for keyboard navigation.
        let groups = by_group(visible);
        let rows: Vec<&Server> =
            groups.iter().filter(|(g, _)| !self.collapsed.contains(g)).flat_map(|(_, list)| list.iter()).collect();
        if !visible.iter().any(|s| Some(s.id) == self.selected) {
            self.selected = rows.first().or(visible.first().as_ref()).map(|s| s.id);
        }
        let typing = ui.ctx().memory(|m| m.focused().is_some());
        if !typing && self.confirm.is_none() && self.server_editor.is_none() {
            let pos = rows.iter().position(|s| Some(s.id) == self.selected);
            let (down, up, enter) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if let Some(p) = pos {
                if down && p + 1 < rows.len() {
                    self.selected = Some(rows[p + 1].id);
                }
                if up && p > 0 {
                    self.selected = Some(rows[p - 1].id);
                }
                if enter {
                    let s = rows[p].clone();
                    if let Some(a) = actions(&s).first().copied().filter(|_| self.can_connect(&s)) {
                        self.run_action(&s, a);
                    }
                }
            }
        }
        let selected = visible.iter().find(|s| Some(s.id) == self.selected).cloned();

        egui::Panel::right("inspector")
            .resizable(false)
            .exact_size(340.0)
            .show_separator_line(false)
            .frame(egui::Frame::new().inner_margin(Margin { left: 16, right: 0, top: 0, bottom: 0 }))
            .show(ui, |ui| {
                if let Some(s) = &selected {
                    theme::card().inner_margin(Margin::same(18)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.set_min_height(ui.available_height());
                        self.inspector(ui, s);
                    });
                }
            });

        egui::CentralPanel::default().frame(egui::Frame::new()).show(ui, |ui| {
            theme::card().inner_margin(Margin::symmetric(8, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.table_header(ui);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for (group, servers) in &groups {
                        let collapsed = self.collapsed.contains(group);
                        ui.add_space(4.0);
                        if group_header(ui, group_title(group), servers.len(), collapsed).clicked() {
                            if collapsed {
                                self.collapsed.remove(group);
                            } else {
                                self.collapsed.insert(group.clone());
                            }
                        }
                        if !collapsed {
                            for s in servers {
                                self.table_row(ui, s);
                            }
                        }
                    }
                });
            });
        });
    }

    /// Column x offsets (relative to the row) for name, address, links and status.
    fn columns(width: f32) -> [f32; 4] {
        [28.0, 28.0 + width * 0.24, 28.0 + width * 0.56, width - 12.0]
    }

    fn table_header(&self, ui: &mut Ui) {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
        let [name, addr, links, status] = Self::columns(rect.width());
        let p = ui.painter();
        let font = FontId::proportional(11.0);
        for (x, text, align) in [
            (name, "NAME", Align2::LEFT_CENTER),
            (addr, "ADDRESS", Align2::LEFT_CENTER),
            (links, "LINKS", Align2::LEFT_CENTER),
            (status, "STATUS", Align2::RIGHT_CENTER),
        ] {
            p.text(pos2(rect.left() + x, rect.center().y), align, text, font.clone(), theme::MUTED);
        }
        p.hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, theme::BORDER));
    }

    fn table_row(&mut self, ui: &mut Ui, s: &Server) {
        let selected = self.selected == Some(s.id);
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
        response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &s.name));
        let (dot, color, label) = theme::status(s, &self.monitor.get(s.id));
        let [name_x, addr_x, links_x, status_x] = Self::columns(rect.width());
        let p = ui.painter_at(rect);
        if selected {
            p.rect(
                rect.shrink(1.0),
                CornerRadius::same(6),
                theme::SELECTED,
                Stroke::new(1.0, theme::ACCENT_BORDER),
                StrokeKind::Inside,
            );
        } else if response.hovered() {
            p.rect_filled(rect, CornerRadius::same(6), theme::faint_hover());
        }
        let y = rect.center().y;
        match dot {
            Some(c) => {
                p.circle_filled(pos2(rect.left() + 14.0, y), 4.0, c);
            }
            None => {
                p.circle_stroke(pos2(rect.left() + 14.0, y), 3.5, Stroke::new(1.0, theme::MUTED));
            }
        }
        let clip =
            |from: f32, to: f32| Rect::from_x_y_ranges(rect.left() + from..=rect.left() + to - 8.0, rect.y_range());
        p.with_clip_rect(clip(name_x, addr_x)).text(
            pos2(rect.left() + name_x, y),
            Align2::LEFT_CENTER,
            &s.name,
            FontId::proportional(13.5),
            theme::TEXT,
        );
        p.with_clip_rect(clip(addr_x, links_x)).text(
            pos2(rect.left() + addr_x, y),
            Align2::LEFT_CENTER,
            theme::address(s),
            FontId::monospace(12.0),
            theme::MUTED,
        );
        let mut x = rect.left() + links_x;
        let links = [(s.ssh.is_some(), "SSH"), (s.serial.is_some(), "Serial"), (s.vnc.is_some(), "VNC")];
        for (_, text) in links.into_iter().filter(|(on, _)| *on) {
            let galley = p.layout_no_wrap(text.to_string(), FontId::proportional(11.0), theme::SUBTLE);
            let chip = Rect::from_min_size(pos2(x, y - 9.0), vec2(galley.size().x + 12.0, 18.0));
            p.rect(
                chip,
                CornerRadius::same(4),
                theme::SURFACE_2,
                Stroke::new(1.0, theme::BORDER_STRONG),
                StrokeKind::Inside,
            );
            p.galley(pos2(x + 6.0, y - galley.size().y / 2.0), galley, theme::SUBTLE);
            x = chip.right() + 4.0;
        }
        p.text(pos2(rect.left() + status_x, y), Align2::RIGHT_CENTER, label, FontId::proportional(12.0), color);

        if response.clicked() {
            self.selected = Some(s.id);
        }
        if response.double_clicked()
            && self.can_connect(s)
            && let Some(a) = actions(s).first().copied()
        {
            self.run_action(s, a);
        }
        response.context_menu(|ui| {
            let connect = self.can_connect(s);
            for a in actions(s) {
                if ui.add_enabled(connect, egui::Button::new(long_label(a))).clicked() {
                    self.run_action(s, a);
                }
            }
            if self.is_admin() {
                ui.separator();
                self.admin_menu(ui, s);
            }
        });
    }

    fn inspector(&mut self, ui: &mut Ui, s: &Server) {
        let status = self.monitor.get(s.id);
        let (dot, color, label) = theme::status(s, &status);
        ui.horizontal(|ui| {
            theme::dot(ui, dot);
            ui.label(RichText::new(&s.name).size(18.0).strong().color(theme::TEXT));
            if !s.group.is_empty() {
                theme::chip(ui, &s.group);
            }
        });
        let label = match &status {
            Status::Offline(e) => format!("offline: {e}"),
            _ => label,
        };
        ui.label(RichText::new(label).small().color(color));
        if !s.notes.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new(&s.notes).color(theme::SUBTLE));
        }
        ui.add_space(12.0);

        let connect = self.can_connect(s);
        let acts = actions(s);
        let w = ui.available_width();
        if let Some((&first, rest)) = acts.split_first() {
            let primary = theme::primary(long_label(first)).min_size(vec2(w, 36.0));
            if ui.add_enabled(connect, primary).on_disabled_hover_text(self.connect_hint()).clicked() {
                self.run_action(s, first);
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                let half = (w - 8.0) / 2.0;
                for &a in rest {
                    let b = theme::ghost(long_label(a)).min_size(vec2(half, 32.0));
                    if ui.add_enabled(connect, b).on_disabled_hover_text(self.connect_hint()).clicked() {
                        self.run_action(s, a);
                    }
                }
            });
        }
        ui.add_space(14.0);

        egui::Grid::new(("inspector-details", s.id)).num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
            let row = |ui: &mut Ui, k: &str, v: RichText| {
                ui.label(RichText::new(k).small().color(theme::MUTED));
                ui.label(v);
                ui.end_row();
            };
            if let Some(ssh) = &s.ssh {
                row(ui, "SSH", theme::mono(format!("{}@{}:{}", ssh.username, ssh.host, ssh.port)).color(theme::TEXT));
                let auth = match &ssh.auth {
                    SshAuth::Key { path } => format!("key {path}"),
                    a => a.label().to_string(),
                };
                row(ui, "Auth", RichText::new(auth).small());
                let fp = s.host_key_fp.as_deref().unwrap_or("not yet trusted");
                row(ui, "Host key", theme::mono(fp));
            }
            if let Some(serial) = &s.serial {
                row(
                    ui,
                    "Serial",
                    theme::mono(format!("{} · flow {:?}", serial.summary(), serial.flow_control)).color(theme::TEXT),
                );
            }
            if let Some(vnc) = &s.vnc {
                let pw = if s.has_vnc_secret { " · password stored" } else { "" };
                row(ui, "VNC", theme::mono(format!("{}{pw}", vnc.summary(s.ssh.as_ref()))).color(theme::TEXT));
            }
            let who = if s.required_role == Role::Admin { "admins only" } else { "operators & admins" };
            row(ui, "Who may connect", RichText::new(who).small());
        });

        if self.is_admin() {
            ui.with_layout(egui::Layout::bottom_up(Align::Min), |ui| {
                ui.horizontal(|ui| {
                    if ui.add(theme::ghost("Edit…")).clicked() {
                        self.server_editor = Some(ServerEditor::new(Some(s)));
                    }
                    if ui.add_enabled(s.host_key_fp.is_some(), theme::ghost("Forget host key")).clicked() {
                        self.confirm = Some((
                            format!("Forget the trusted SSH host key of “{}”? The next connection will trust whatever key the server presents.", s.name),
                            Confirm::ResetHostKey(s.id),
                        ));
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(theme::danger("Delete")).clicked() {
                            self.confirm = Some((format!("Delete server “{}”?", s.name), Confirm::DeleteServer(s.id)));
                        }
                    });
                });
                theme::rule(ui);
            });
        }
    }
}

/// A clickable group row with a disclosure triangle.
fn group_header(ui: &mut Ui, title: &str, count: usize, collapsed: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::CollapsingHeader, true, title));
    let p = ui.painter();
    let c = pos2(rect.left() + 14.0, rect.center().y);
    let points = if collapsed {
        vec![c + vec2(-2.5, -4.0), c + vec2(3.0, 0.0), c + vec2(-2.5, 4.0)]
    } else {
        vec![c + vec2(-4.0, -2.5), c + vec2(4.0, -2.5), c + vec2(0.0, 3.0)]
    };
    p.add(egui::Shape::convex_polygon(points, theme::SUBTLE, Stroke::NONE));
    let title_rect =
        p.text(pos2(rect.left() + 28.0, c.y), Align2::LEFT_CENTER, title, FontId::proportional(13.5), theme::SUBTLE);
    p.text(pos2(title_rect.right() + 8.0, c.y), Align2::LEFT_CENTER, count, FontId::proportional(12.0), theme::MUTED);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Tooltip text for a server name.
fn details(s: &Server) -> String {
    let mut out = String::new();
    if !s.notes.is_empty() {
        out.push_str(&format!("{}\n", s.notes));
    }
    out.push_str(&format!("Connect: {} and above", s.required_role));
    if let Some(fp) = &s.host_key_fp {
        out.push_str(&format!("\nHost key: {fp}"));
    }
    out
}
