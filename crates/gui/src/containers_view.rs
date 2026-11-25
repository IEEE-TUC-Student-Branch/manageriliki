//! A view listing a server's containers (Docker, Podman, LXC, Kubernetes)
//! and the services of the selected one.

use std::sync::mpsc::{self, Receiver};
use std::thread;

use eframe::egui::{self, Color32, RichText, Ui};
use manageriliki_core::{
    Container, ContainerAction, ContainerList, ContainerService, Identity, Manager,
    Result as CoreResult, Runtime, Server,
};

const GREEN: Color32 = Color32::from_rgb(0x3c, 0xc8, 0x6e);
const RED: Color32 = Color32::from_rgb(0xe0, 0x50, 0x50);
const GRAY: Color32 = Color32::from_rgb(0x90, 0x90, 0x90);
const ORANGE: Color32 = Color32::from_rgb(0xf0, 0xa0, 0x60);

/// Runs `f` on a worker thread and repaints when it is done.
fn spawn<T: Send + 'static>(ctx: &egui::Context, f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    thread::spawn(move || {
        let _ = tx.send(f());
        ctx.request_repaint();
    });
    rx
}

/// A background result: nothing yet, loading, or done.
enum Task<T> {
    Idle,
    Loading(Receiver<T>),
    Done(T),
}

impl<T> Task<T> {
    /// Moves a finished result in; returns true when it just arrived.
    fn poll(&mut self) -> bool {
        if let Task::Loading(rx) = self {
            match rx.try_recv() {
                Ok(v) => {
                    *self = Task::Done(v);
                    return true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => *self = Task::Idle,
            }
        }
        false
    }

    fn loading(&self) -> bool {
        matches!(self, Task::Loading(_))
    }
}

fn same(a: &Container, b: &Container) -> bool {
    a.runtime == b.runtime && a.namespace == b.namespace && a.id == b.id && a.backend() == b.backend()
}

pub struct ContainersView {
    pub id: u64,
    pub server: Server,
    mgr: Manager,
    who: Identity,
    list: Task<CoreResult<ContainerList>>,
    filter: String,
    runtime: Option<Runtime>,
    selected: Option<Container>,
    services: Task<CoreResult<Vec<ContainerService>>>,
    logs: Task<CoreResult<String>>,
    action: Task<(String, CoreResult<()>)>,
    confirm: Option<ContainerAction>,
    message: Option<(String, bool)>,
}

impl ContainersView {
    pub fn new(id: u64, mgr: Manager, who: Identity, server: Server, ctx: &egui::Context) -> Self {
        let mut view = ContainersView {
            id,
            server,
            mgr,
            who,
            list: Task::Idle,
            filter: String::new(),
            runtime: None,
            selected: None,
            services: Task::Idle,
            logs: Task::Idle,
            action: Task::Idle,
            confirm: None,
            message: None,
        };
        view.refresh(ctx);
        view
    }

    fn refresh(&mut self, ctx: &egui::Context) {
        let (mgr, who, id) = (self.mgr.clone(), self.who.clone(), self.server.id);
        self.list = Task::Loading(spawn(ctx, move || mgr.list_containers(&who, id)));
    }

    fn load_services(&mut self, ctx: &egui::Context) {
        let Some(c) = self.selected.clone() else { return };
        let (mgr, who, id) = (self.mgr.clone(), self.who.clone(), self.server.id);
        self.services = Task::Loading(spawn(ctx, move || mgr.container_services(&who, id, &c)));
    }

    fn select(&mut self, c: Container, ctx: &egui::Context) {
        self.selected = Some(c);
        self.logs = Task::Idle;
        self.confirm = None;
        self.load_services(ctx);
    }

    fn run_action(&mut self, action: ContainerAction, ctx: &egui::Context) {
        let Some(c) = self.selected.clone() else { return };
        let (mgr, who, id) = (self.mgr.clone(), self.who.clone(), self.server.id);
        let label = format!("{} {}", c.action_label(action).to_lowercase(), c.display_name());
        self.action = Task::Loading(spawn(ctx, move || {
            (label, mgr.container_action(&who, id, &c, action))
        }));
    }

    fn load_logs(&mut self, ctx: &egui::Context) {
        let Some(c) = self.selected.clone() else { return };
        let (mgr, who, id) = (self.mgr.clone(), self.who.clone(), self.server.id);
        self.logs = Task::Loading(spawn(ctx, move || mgr.container_logs(&who, id, &c)));
    }

    /// Draws the view into `ui` (e.g. a docked tab). Returns a container to open a shell in.
    pub fn show(&mut self, ui: &mut Ui) -> Option<Container> {
        self.poll(&ui.ctx().clone());
        self.body(ui)
    }

    /// Collects finished background work.
    fn poll(&mut self, ctx: &egui::Context) {
        if self.list.poll() {
            // Keep the selection pointing at the refreshed container.
            if let (Task::Done(Ok(list)), Some(sel)) = (&self.list, &self.selected) {
                self.selected = list.containers.iter().find(|c| same(c, sel)).cloned();
                if self.selected.is_some() {
                    self.load_services(ctx);
                } else {
                    self.services = Task::Idle;
                }
            }
        }
        self.services.poll();
        self.logs.poll();
        if self.action.poll() {
            if let Task::Done((label, result)) = &self.action {
                self.message = Some(match result {
                    Ok(()) => (format!("{label}: done"), false),
                    Err(e) => (e.to_string(), true),
                });
            }
            self.refresh(ctx);
        }
    }

    fn body(&mut self, ui: &mut Ui) -> Option<Container> {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.list.loading(), egui::Button::new("⟳ Refresh")).clicked() {
                self.refresh(&ctx);
            }
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter").desired_width(180.0));
            if let Task::Done(Ok(list)) = &self.list {
                ui.selectable_value(&mut self.runtime, None, format!("All ({})", list.containers.len()));
                for r in &list.runtimes {
                    let n = list.containers.iter().filter(|c| c.runtime == *r).count();
                    ui.selectable_value(&mut self.runtime, Some(*r), format!("{r} ({n})"));
                }
            }
            if self.list.loading() {
                ui.spinner();
            }
        });

        let list = match &self.list {
            Task::Done(Ok(list)) => list,
            Task::Done(Err(e)) => {
                ui.colored_label(Color32::LIGHT_RED, e.to_string());
                return None;
            }
            _ => {
                ui.label(RichText::new("Looking for Docker, Podman, LXC and Kubernetes…").weak());
                return None;
            }
        };
        for w in &list.warnings {
            ui.colored_label(ORANGE, format!("⚠ {w}"));
        }
        if list.runtimes.is_empty() {
            ui.add_space(10.0);
            ui.label(RichText::new(
                "No container runtime found on this server (looked for docker, podman, incus, lxc, pct, lxc-ls, kubectl, k3s and microk8s).",
            ).weak());
            return None;
        }

        let filter = self.filter.to_lowercase();
        let visible: Vec<Container> = list
            .containers
            .iter()
            .filter(|c| self.runtime.is_none_or(|r| c.runtime == r))
            .filter(|c| {
                filter.is_empty()
                    || c.display_name().to_lowercase().contains(&filter)
                    || c.image.to_lowercase().contains(&filter)
                    || c.state.to_lowercase().contains(&filter)
            })
            .cloned()
            .collect();

        let mut clicked = None;
        let list_height = (ui.available_height() * 0.45).max(120.0);
        egui::ScrollArea::both().id_salt("list").max_height(list_height).auto_shrink([false, true]).show(ui, |ui| {
            if visible.is_empty() {
                ui.label(RichText::new("No containers.").weak());
                return;
            }
            egui::Grid::new(("containers-grid", self.id)).striped(true).num_columns(7).spacing([14.0, 6.0]).show(ui, |ui| {
                for h in ["", "Name", "Runtime", "Image", "State", "Status", "Ports / addresses"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for c in &visible {
                    dot(ui, if c.running { GREEN } else if c.state == "exited" || c.state == "stopped" { GRAY } else { RED });
                    let selected = self.selected.as_ref().is_some_and(|s| same(s, c));
                    if ui.selectable_label(selected, RichText::new(c.display_name()).strong()).clicked() {
                        clicked = Some(c.clone());
                    }
                    ui.label(c.runtime.label()).on_hover_text(format!("via {}", c.backend()));
                    ui.label(&c.image);
                    ui.label(&c.state);
                    ui.label(&c.status);
                    ui.label(RichText::new(&c.ports).monospace());
                    ui.end_row();
                }
            });
        });
        if let Some(c) = clicked {
            self.select(c, &ctx);
        }

        ui.separator();
        self.details(ui)
    }

    fn details(&mut self, ui: &mut Ui) -> Option<Container> {
        let ctx = ui.ctx().clone();
        let Some(c) = self.selected.clone() else {
            ui.label(RichText::new("Select a container to see its services.").weak());
            return None;
        };
        let mut shell = None;
        let busy = self.action.loading();
        ui.horizontal(|ui| {
            ui.label(RichText::new(c.display_name()).strong().size(16.0));
            ui.label(RichText::new(format!("{} · {}", c.runtime, c.backend())).weak());
            ui.add_space(10.0);
            for action in ContainerAction::ALL {
                if !c.supports(action) {
                    continue;
                }
                let enabled = !busy && match action {
                    ContainerAction::Start => !c.running,
                    ContainerAction::Stop => c.running,
                    ContainerAction::Restart => true,
                };
                if ui.add_enabled(enabled, egui::Button::new(c.action_label(action))).clicked() {
                    if action == ContainerAction::Start {
                        self.run_action(action, &ctx);
                    } else {
                        self.confirm = Some(action);
                    }
                }
            }
            if ui.add_enabled(!self.logs.loading(), egui::Button::new("Logs")).clicked() {
                self.load_logs(&ctx);
            }
            if ui.add_enabled(c.running, egui::Button::new("Shell")).clicked() {
                shell = Some(c.clone());
            }
            if ui.add_enabled(!self.services.loading(), egui::Button::new("⟳")).on_hover_text("Reload services").clicked() {
                self.load_services(&ctx);
            }
            if busy {
                ui.spinner();
            }
        });

        if let Some(action) = self.confirm {
            ui.horizontal(|ui| {
                ui.colored_label(ORANGE, format!("{} “{}”?", c.action_label(action), c.display_name()));
                if ui.button(RichText::new("Yes").color(Color32::LIGHT_RED)).clicked() {
                    self.confirm = None;
                    self.run_action(action, &ctx);
                }
                if ui.button("Cancel").clicked() {
                    self.confirm = None;
                }
            });
        }
        if let Some((msg, err)) = &self.message {
            ui.colored_label(if *err { Color32::LIGHT_RED } else { Color32::LIGHT_GREEN }, msg);
        }

        let show_logs = !matches!(self.logs, Task::Idle);
        egui::ScrollArea::vertical().id_salt("services").auto_shrink([false, false])
            .max_height(if show_logs { ui.available_height() * 0.5 } else { f32::INFINITY })
            .show(ui, |ui| match &self.services {
                Task::Loading(_) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("loading services…");
                    });
                }
                Task::Done(Err(e)) => {
                    ui.colored_label(Color32::LIGHT_RED, e.to_string());
                }
                Task::Done(Ok(s)) if s.is_empty() => {
                    ui.label(RichText::new(if c.running { "No services found." } else { "Not running." }).weak());
                }
                Task::Done(Ok(services)) => {
                    egui::Grid::new(("services-grid", self.id)).striped(true).num_columns(5).spacing([14.0, 4.0]).show(ui, |ui| {
                        for h in ["", "Kind", "Name", "State", "Detail"] {
                            ui.label(RichText::new(h).strong());
                        }
                        ui.end_row();
                        for s in services {
                            dot(ui, if s.ok { GREEN } else { RED });
                            ui.label(RichText::new(s.kind.label()).weak());
                            ui.label(RichText::new(&s.name).strong());
                            ui.label(&s.state);
                            ui.label(RichText::new(&s.detail).monospace());
                            ui.end_row();
                        }
                    });
                }
                Task::Idle => {}
            });

        if show_logs {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Logs (last 200 lines)").strong());
                if ui.small_button("Close").clicked() {
                    self.logs = Task::Idle;
                }
            });
            egui::ScrollArea::both().id_salt("logs").auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                match &self.logs {
                    Task::Loading(_) => {
                        ui.spinner();
                    }
                    Task::Done(Ok(text)) if text.trim().is_empty() => {
                        ui.label(RichText::new("(no output)").weak());
                    }
                    Task::Done(Ok(text)) => {
                        ui.label(RichText::new(text.trim_end()).monospace());
                    }
                    Task::Done(Err(e)) => {
                        ui.colored_label(Color32::LIGHT_RED, e.to_string());
                    }
                    Task::Idle => {}
                }
            });
        }
        shell
    }
}

fn dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}
