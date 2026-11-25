//! Containers on a server (Docker, Podman, LXC and Kubernetes), driven over
//! SSH with each runtime's own command-line tool.
//!
//! Nothing is installed on the server. The tools present are detected, and
//! when one refuses to run as the SSH user (e.g. not in the `docker` group,
//! or k3s' root-only kubeconfig), it is retried once with passwordless sudo.

use std::fmt;

use serde_json::Value;

use crate::error::{Result, invalid};
use crate::transport::{self, ExecResult, SshConnection};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Runtime {
    Docker,
    Podman,
    /// LXC containers managed by Incus, LXD, Proxmox (`pct`) or plain `lxc-*`.
    Lxc,
    /// Kubernetes pods, through `kubectl` (also k3s and MicroK8s).
    Kubernetes,
}

impl Runtime {
    pub const ALL: [Runtime; 4] = [Runtime::Docker, Runtime::Podman, Runtime::Lxc, Runtime::Kubernetes];

    pub fn label(self) -> &'static str {
        match self {
            Runtime::Docker => "Docker",
            Runtime::Podman => "Podman",
            Runtime::Lxc => "LXC",
            Runtime::Kubernetes => "Kubernetes",
        }
    }
}

impl fmt::Display for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// The command-line tool that manages a container.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Tool {
    Docker,
    Podman,
    Incus,
    Lxd,
    LxcClassic,
    Proxmox,
    /// The kubectl invocation, e.g. `kubectl` or `microk8s kubectl`.
    Kubectl(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    pub runtime: Runtime,
    /// Runtime id: container id, LXC name, Proxmox VMID or pod name.
    pub id: String,
    pub name: String,
    /// Kubernetes namespace; empty for the other runtimes.
    pub namespace: String,
    /// Image (Docker/Podman/Incus), container images (pods) or type.
    pub image: String,
    pub state: String,
    pub running: bool,
    /// Human-readable status, e.g. `Up 3 hours` or `2/2 ready, 1 restart`.
    pub status: String,
    /// Published ports or IP addresses.
    pub ports: String,
    tool: Tool,
    sudo: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
}

impl ContainerAction {
    pub const ALL: [ContainerAction; 3] =
        [ContainerAction::Start, ContainerAction::Stop, ContainerAction::Restart];

    pub fn as_str(self) -> &'static str {
        match self {
            ContainerAction::Start => "start",
            ContainerAction::Stop => "stop",
            ContainerAction::Restart => "restart",
        }
    }
}

impl Container {
    /// `namespace/name` for pods, the name otherwise.
    pub fn display_name(&self) -> String {
        if self.namespace.is_empty() {
            self.name.clone()
        } else {
            format!("{}/{}", self.namespace, self.name)
        }
    }

    /// The tool that manages it, e.g. `docker`, `incus`, `pct`, `k3s kubectl`.
    pub fn backend(&self) -> String {
        let tool = match &self.tool {
            Tool::Docker => "docker",
            Tool::Podman => "podman",
            Tool::Incus => "incus",
            Tool::Lxd => "lxd",
            Tool::LxcClassic => "lxc",
            Tool::Proxmox => "pct",
            Tool::Kubectl(k) => k,
        };
        if self.sudo { format!("{tool} (sudo)") } else { tool.to_string() }
    }

    /// Pods can only be restarted (deleted, so their controller recreates them).
    pub fn supports(&self, action: ContainerAction) -> bool {
        !matches!(self.tool, Tool::Kubectl(_)) || action == ContainerAction::Restart
    }

    /// What an action does, for buttons and confirmations.
    pub fn action_label(&self, action: ContainerAction) -> &'static str {
        match (&self.tool, action) {
            (Tool::Kubectl(_), ContainerAction::Restart) => "Delete pod",
            (_, ContainerAction::Start) => "Start",
            (_, ContainerAction::Stop) => "Stop",
            (_, ContainerAction::Restart) => "Restart",
        }
    }

    fn bin(&self, name: &str) -> String {
        if self.sudo { format!("sudo -n {name}") } else { name.to_string() }
    }

    fn cli(&self) -> String {
        self.bin(match &self.tool {
            Tool::Docker => "docker",
            Tool::Podman => "podman",
            Tool::Incus => "incus",
            Tool::Lxd => "lxc",
            Tool::LxcClassic => "lxc-attach",
            Tool::Proxmox => "pct",
            Tool::Kubectl(k) => k,
        })
    }

    /// Runs a POSIX shell script inside an LXC container.
    fn lxc_exec(&self, script: &str) -> String {
        let (id, script) = (quote(&self.id), quote(script));
        match self.tool {
            Tool::Incus | Tool::Lxd => format!("{} exec {id} -- sh -c {script}", self.cli()),
            Tool::LxcClassic => format!("{} -n {id} -- sh -c {script}", self.bin("lxc-attach")),
            Tool::Proxmox => format!("{} exec {id} -- sh -c {script}", self.cli()),
            _ => unreachable!("not an LXC container"),
        }
    }

    pub(crate) fn action_command(&self, action: ContainerAction) -> Result<String> {
        let id = quote(&self.id);
        let verb = action.as_str();
        Ok(match &self.tool {
            Tool::Docker | Tool::Podman | Tool::Incus | Tool::Lxd => {
                format!("{} {verb} {id}", self.cli())
            }
            Tool::LxcClassic => match action {
                ContainerAction::Start => format!("{} -n {id}", self.bin("lxc-start")),
                ContainerAction::Stop => format!("{} -n {id}", self.bin("lxc-stop")),
                ContainerAction::Restart => format!("{} -r -n {id}", self.bin("lxc-stop")),
            },
            Tool::Proxmox => {
                let verb = match action {
                    ContainerAction::Start => "start",
                    ContainerAction::Stop => "shutdown",
                    ContainerAction::Restart => "reboot",
                };
                format!("{} {verb} {id}", self.cli())
            }
            Tool::Kubectl(_) if action == ContainerAction::Restart => format!(
                "{} delete pod -n {} {id} --wait=false",
                self.cli(),
                quote(&self.namespace)
            ),
            Tool::Kubectl(_) => {
                return Err(invalid("pods cannot be started or stopped; scale their controller instead"));
            }
        })
    }

    pub(crate) fn logs_command(&self) -> String {
        let id = quote(&self.id);
        match &self.tool {
            Tool::Docker | Tool::Podman => format!("{} logs --tail 200 {id} 2>&1", self.cli()),
            Tool::Kubectl(_) => format!(
                "{} logs -n {} {id} --all-containers --prefix --tail=200 2>&1",
                self.cli(),
                quote(&self.namespace)
            ),
            _ => format!(
                "{} 2>&1",
                self.lxc_exec(
                    "journalctl -n 200 --no-pager 2>/dev/null || \
                     tail -n 200 /var/log/syslog /var/log/messages 2>/dev/null"
                )
            ),
        }
    }

    pub(crate) fn shell_command(&self) -> String {
        const SHELL: &str = "command -v bash >/dev/null 2>&1 && exec bash || exec sh";
        let id = quote(&self.id);
        match &self.tool {
            Tool::Docker | Tool::Podman => format!("{} exec -it {id} sh -c {}", self.cli(), quote(SHELL)),
            Tool::Incus | Tool::Lxd => {
                format!("{} exec -t {id} -- sh -c {}", self.cli(), quote(SHELL))
            }
            Tool::LxcClassic => self.lxc_exec(SHELL),
            Tool::Proxmox => format!("{} enter {id}", self.cli()),
            Tool::Kubectl(_) => format!(
                "{} exec -it -n {} {id} -- sh -c {}",
                self.cli(),
                quote(&self.namespace),
                quote(SHELL)
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceKind {
    /// A published port.
    Port,
    /// A process running in the container.
    Process,
    /// A systemd unit inside a system container.
    Unit,
    /// A container of a pod.
    PodContainer,
    /// A Kubernetes Service that selects the pod.
    KubeService,
}

impl ServiceKind {
    pub fn label(self) -> &'static str {
        match self {
            ServiceKind::Port => "port",
            ServiceKind::Process => "process",
            ServiceKind::Unit => "service",
            ServiceKind::PodContainer => "container",
            ServiceKind::KubeService => "k8s service",
        }
    }
}

/// Something a container provides or runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerService {
    pub kind: ServiceKind,
    pub name: String,
    pub state: String,
    /// Whether the state is healthy (running / active / ready).
    pub ok: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Default)]
pub struct ContainerList {
    pub containers: Vec<Container>,
    /// Runtimes whose tools were found on the server.
    pub runtimes: Vec<Runtime>,
    /// Tools that were found but failed, e.g. a stopped Docker daemon.
    pub warnings: Vec<String>,
}

/// Runs a shell command on the server.
pub(crate) trait Exec {
    fn exec(&self, command: &str) -> Result<ExecResult>;
}

/// Directories that non-interactive SSH sessions often leave out of PATH.
const PATH_PREFIX: &str =
    "PATH=\"$PATH:/usr/local/sbin:/usr/local/bin:/usr/sbin:/sbin:/snap/bin\"; export PATH; ";

impl Exec for SshConnection {
    fn exec(&self, command: &str) -> Result<ExecResult> {
        transport::ssh_exec(self, &format!("{PATH_PREFIX}{command}"))
    }
}

/// Quotes a word for a POSIX shell.
pub(crate) fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn needs_root(stderr: &str) -> bool {
    let e = stderr.to_lowercase();
    ["permission denied", "root", "not permitted", "insufficient permission", "access denied"]
        .iter()
        .any(|p| e.contains(p))
}

/// Runs `tool args`, retrying with `sudo -n` when the tool lacks privileges.
/// Returns the output and whether sudo was used.
fn run_tool(x: &dyn Exec, tool: &str, args: &str, is_root: bool) -> Result<(ExecResult, bool)> {
    let out = x.exec(&format!("{tool} {args}"))?;
    if out.exit_status != 0 && !is_root && needs_root(&out.stderr) {
        let retry = x.exec(&format!("sudo -n {tool} {args}"))?;
        if retry.exit_status == 0 {
            return Ok((retry, true));
        }
    }
    Ok((out, false))
}

fn failure(what: &str, out: &ExecResult) -> String {
    let msg = out.stderr.trim();
    let msg = if msg.is_empty() { out.stdout.trim() } else { msg };
    let first = msg.lines().next().unwrap_or("").trim();
    format!("{what}: {}", if first.is_empty() { format!("exit status {}", out.exit_status) } else { first.to_string() })
}

const DETECT: &str = "for t in docker podman incus lxc lxc-ls pct kubectl k3s microk8s; do \
    command -v \"$t\" >/dev/null 2>&1 && echo \"$t\"; done; [ \"$(id -u)\" = 0 ] && echo '#root'; true";

const DOCKER_PS: &str =
    "ps -a --format '{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.State}}\t{{.Status}}\t{{.Ports}}'";

pub(crate) fn list(x: &dyn Exec) -> Result<ContainerList> {
    let found = x.exec(DETECT)?.stdout;
    let has = |t: &str| found.lines().any(|l| l.trim() == t);
    let root = has("#root");
    let mut list = ContainerList::default();

    fn add(list: &mut ContainerList, runtime: Runtime, result: std::result::Result<Vec<Container>, String>) {
        if !list.runtimes.contains(&runtime) {
            list.runtimes.push(runtime);
        }
        match result {
            Ok(c) => list.containers.extend(c),
            Err(w) => list.warnings.push(w),
        }
    }

    for (bin, tool, runtime) in [("docker", Tool::Docker, Runtime::Docker), ("podman", Tool::Podman, Runtime::Podman)] {
        if has(bin) {
            let (out, sudo) = run_tool(x, bin, DOCKER_PS, root)?;
            let r = if out.exit_status == 0 {
                Ok(parse_docker_ps(&out.stdout, runtime, &tool, sudo))
            } else {
                Err(failure(bin, &out))
            };
            add(&mut list, runtime, r);
        }
    }

    // Incus and LXD (whose client is called `lxc`).
    for (bin, tool) in [("incus", Tool::Incus), ("lxc", Tool::Lxd)] {
        if has(bin) {
            let (out, sudo) = run_tool(x, bin, "list --format json", root)?;
            let r = if out.exit_status == 0 {
                parse_incus(&out.stdout, &tool, sudo).map_err(|e| format!("{bin}: {e}"))
            } else {
                Err(failure(bin, &out))
            };
            add(&mut list, Runtime::Lxc, r);
        }
    }

    // Proxmox also ships the lxc-* tools; pct is the authoritative view there.
    if has("pct") {
        let (out, sudo) = run_tool(x, "pct", "list", root)?;
        let r = if out.exit_status == 0 {
            Ok(parse_pct(&out.stdout, sudo))
        } else {
            Err(failure("pct", &out))
        };
        add(&mut list, Runtime::Lxc, r);
    } else if has("lxc-ls") {
        let (out, sudo) = run_tool(x, "lxc-ls", "-f -F NAME,STATE,IPV4", root)?;
        let r = if out.exit_status == 0 {
            Ok(parse_lxc_ls(&out.stdout, sudo))
        } else {
            Err(failure("lxc-ls", &out))
        };
        add(&mut list, Runtime::Lxc, r);
    }

    let kubectl = if has("kubectl") {
        Some("kubectl")
    } else if has("k3s") {
        Some("k3s kubectl")
    } else if has("microk8s") {
        Some("microk8s kubectl")
    } else {
        None
    };
    if let Some(k) = kubectl {
        let (out, sudo) = run_tool(x, k, "get pods -A -o json --request-timeout=10s", root)?;
        let r = if out.exit_status == 0 {
            parse_pods(&out.stdout, k, sudo).map_err(|e| format!("{k}: {e}"))
        } else {
            Err(failure(k, &out))
        };
        add(&mut list, Runtime::Kubernetes, r);
    }

    list.runtimes.sort();
    Ok(list)
}

pub(crate) fn services(x: &dyn Exec, c: &Container) -> Result<Vec<ContainerService>> {
    match &c.tool {
        Tool::Docker | Tool::Podman => docker_services(x, c),
        Tool::Kubectl(_) => pod_services(x, c),
        _ => lxc_services(x, c),
    }
}

fn docker_services(x: &dyn Exec, c: &Container) -> Result<Vec<ContainerService>> {
    let mut services: Vec<ContainerService> = c
        .ports
        .split(", ")
        .filter(|p| !p.trim().is_empty())
        .map(|p| ContainerService {
            kind: ServiceKind::Port,
            name: p.trim().to_string(),
            state: "published".into(),
            ok: c.running,
            detail: String::new(),
        })
        .collect();
    if !c.running {
        return Ok(services);
    }
    let fields = if c.tool == Tool::Docker { "-o pid,user,args" } else { "pid user args" };
    let out = x.exec(&format!("{} top {} {fields}", c.cli(), quote(&c.id)))?;
    if out.exit_status != 0 {
        return Err(invalid(failure("top", &out)));
    }
    services.extend(parse_processes(&out.stdout));
    Ok(services)
}

const LXC_SERVICES: &str = "if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then \
    echo '#systemd'; systemctl list-units --type=service --all --no-legend --no-pager --plain; \
    else echo '#ps'; ps -eo pid,user,args 2>/dev/null || ps; fi";

fn lxc_services(x: &dyn Exec, c: &Container) -> Result<Vec<ContainerService>> {
    if !c.running {
        return Ok(Vec::new());
    }
    let out = x.exec(&c.lxc_exec(LXC_SERVICES))?;
    if out.exit_status != 0 {
        return Err(invalid(failure("listing services", &out)));
    }
    let mut lines = out.stdout.lines();
    let marker = lines.find(|l| l.starts_with('#')).unwrap_or("");
    let rest: String = lines.collect::<Vec<_>>().join("\n");
    Ok(if marker == "#systemd" { parse_units(&rest) } else { parse_processes(&rest) })
}

fn pod_services(x: &dyn Exec, c: &Container) -> Result<Vec<ContainerService>> {
    let (k, ns) = (c.cli(), quote(&c.namespace));
    let pod = x.exec(&format!("{k} get pod -n {ns} {} -o json --request-timeout=10s", quote(&c.id)))?;
    if pod.exit_status != 0 {
        return Err(invalid(failure("kubectl get pod", &pod)));
    }
    let svcs = x.exec(&format!("{k} get services -n {ns} -o json --request-timeout=10s"))?;
    if svcs.exit_status != 0 {
        return Err(invalid(failure("kubectl get services", &svcs)));
    }
    parse_pod_services(&pod.stdout, &svcs.stdout)
}

// ---- parsers ----

fn parse_docker_ps(out: &str, runtime: Runtime, tool: &Tool, sudo: bool) -> Vec<Container> {
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 5 || f[0].is_empty() {
                return None;
            }
            let (state, status) = (f[3].to_string(), f[4].to_string());
            Some(Container {
                runtime,
                id: f[0].to_string(),
                name: f[1].to_string(),
                namespace: String::new(),
                image: f[2].to_string(),
                running: state.eq_ignore_ascii_case("running") || status.starts_with("Up"),
                state,
                status,
                ports: f.get(5).map(|p| p.trim().to_string()).unwrap_or_default(),
                tool: tool.clone(),
                sudo,
            })
        })
        .collect()
}

fn str_at<'a>(v: &'a Value, path: &[&str]) -> &'a str {
    let mut v = v;
    for p in path {
        v = &v[*p];
    }
    v.as_str().unwrap_or("")
}

fn parse_incus(out: &str, tool: &Tool, sudo: bool) -> std::result::Result<Vec<Container>, String> {
    let v: Value = serde_json::from_str(out).map_err(|e| format!("unexpected output: {e}"))?;
    let items = v.as_array().ok_or("unexpected output: not a list")?;
    Ok(items
        .iter()
        .map(|i| {
            let mut addrs = Vec::new();
            if let Some(nets) = i["state"]["network"].as_object() {
                for (iface, net) in nets {
                    if iface == "lo" {
                        continue;
                    }
                    for a in net["addresses"].as_array().into_iter().flatten() {
                        if str_at(a, &["scope"]) == "global" {
                            addrs.push(str_at(a, &["address"]).to_string());
                        }
                    }
                }
            }
            let status = str_at(i, &["status"]).to_string();
            let kind = str_at(i, &["type"]);
            let desc = str_at(i, &["config", "image.description"]);
            let image = match (desc.is_empty(), kind) {
                (false, "virtual-machine") => format!("{desc} (VM)"),
                (false, _) => desc.to_string(),
                (true, k) => k.to_string(),
            };
            Container {
                runtime: Runtime::Lxc,
                id: str_at(i, &["name"]).to_string(),
                name: str_at(i, &["name"]).to_string(),
                namespace: String::new(),
                image,
                running: status.eq_ignore_ascii_case("running"),
                state: status.to_lowercase(),
                status,
                ports: addrs.join(", "),
                tool: tool.clone(),
                sudo,
            }
        })
        .collect())
}

fn parse_lxc_ls(out: &str, sudo: bool) -> Vec<Container> {
    out.lines()
        .skip(1)
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let name = f.next()?.to_string();
            let state = f.next().unwrap_or("").to_string();
            let ips = f.collect::<Vec<_>>().join(" ");
            Some(Container {
                runtime: Runtime::Lxc,
                id: name.clone(),
                name,
                namespace: String::new(),
                image: "lxc".into(),
                running: state.eq_ignore_ascii_case("running"),
                state: state.to_lowercase(),
                status: state,
                ports: if ips == "-" { String::new() } else { ips },
                tool: Tool::LxcClassic,
                sudo,
            })
        })
        .collect()
}

fn parse_pct(out: &str, sudo: bool) -> Vec<Container> {
    out.lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 3 {
                return None;
            }
            let state = f[1].to_string();
            Some(Container {
                runtime: Runtime::Lxc,
                id: f[0].to_string(),
                name: f[f.len() - 1].to_string(),
                namespace: String::new(),
                image: format!("CT {}", f[0]),
                running: state == "running",
                status: if f.len() > 3 { format!("{state}, locked: {}", f[2]) } else { state.clone() },
                state,
                ports: String::new(),
                tool: Tool::Proxmox,
                sudo,
            })
        })
        .collect()
}

fn parse_pods(out: &str, kubectl: &str, sudo: bool) -> std::result::Result<Vec<Container>, String> {
    let v: Value = serde_json::from_str(out).map_err(|e| format!("unexpected output: {e}"))?;
    let items = v["items"].as_array().ok_or("unexpected output: no items")?;
    Ok(items
        .iter()
        .map(|p| {
            let statuses = p["status"]["containerStatuses"].as_array().cloned().unwrap_or_default();
            let total = p["spec"]["containers"].as_array().map_or(0, |c| c.len());
            let ready = statuses.iter().filter(|s| s["ready"].as_bool() == Some(true)).count();
            let restarts: u64 = statuses.iter().filter_map(|s| s["restartCount"].as_u64()).sum();
            let waiting = statuses
                .iter()
                .find_map(|s| s["state"]["waiting"]["reason"].as_str())
                .map(str::to_string);
            let phase = str_at(p, &["status", "phase"]);
            let state = if !p["metadata"]["deletionTimestamp"].is_null() {
                "Terminating".to_string()
            } else {
                waiting.unwrap_or_else(|| phase.to_string())
            };
            let images: Vec<&str> = p["spec"]["containers"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| str_at(c, &["image"]))
                .collect();
            let node = str_at(p, &["spec", "nodeName"]);
            let mut status = format!("{ready}/{total} ready");
            if restarts > 0 {
                status.push_str(&format!(", {restarts} restart{}", if restarts == 1 { "" } else { "s" }));
            }
            if !node.is_empty() {
                status.push_str(&format!(", on {node}"));
            }
            Container {
                runtime: Runtime::Kubernetes,
                id: str_at(p, &["metadata", "name"]).to_string(),
                name: str_at(p, &["metadata", "name"]).to_string(),
                namespace: str_at(p, &["metadata", "namespace"]).to_string(),
                image: images.join(", "),
                running: phase == "Running" && state == "Running",
                state,
                status,
                ports: str_at(p, &["status", "podIP"]).to_string(),
                tool: Tool::Kubectl(kubectl.to_string()),
                sudo,
            }
        })
        .collect())
}

/// Parses `ps`-style output with PID, USER and ARGS columns after a header.
fn parse_processes(out: &str) -> Vec<ContainerService> {
    out.lines()
        .skip(1)
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let pid = f.next()?;
            let user = f.next().unwrap_or("");
            let args = f.collect::<Vec<_>>().join(" ");
            if args.is_empty() {
                return None;
            }
            let exe = args.split_whitespace().next().unwrap_or("");
            let name = exe.rsplit('/').next().unwrap_or(exe).to_string();
            Some(ContainerService {
                kind: ServiceKind::Process,
                name,
                state: format!("pid {pid} · {user}"),
                ok: true,
                detail: args,
            })
        })
        .collect()
}

/// Parses `systemctl list-units --plain --no-legend` output.
fn parse_units(out: &str) -> Vec<ContainerService> {
    out.lines()
        .filter_map(|line| {
            let line = line.trim_start_matches(['●', '*', ' ']);
            let mut f = line.split_whitespace();
            let unit = f.next()?;
            let (load, active, sub) = (f.next()?, f.next()?, f.next()?);
            if load == "not-found" {
                return None;
            }
            Some(ContainerService {
                kind: ServiceKind::Unit,
                name: unit.trim_end_matches(".service").to_string(),
                state: format!("{active} ({sub})"),
                ok: active == "active",
                detail: f.collect::<Vec<_>>().join(" "),
            })
        })
        .collect()
}

fn parse_pod_services(pod: &str, services: &str) -> Result<Vec<ContainerService>> {
    let bad = |e: serde_json::Error| invalid(format!("unexpected kubectl output: {e}"));
    let pod: Value = serde_json::from_str(pod).map_err(bad)?;
    let svcs: Value = serde_json::from_str(services).map_err(bad)?;
    let mut out = Vec::new();

    let statuses = pod["status"]["containerStatuses"].as_array().cloned().unwrap_or_default();
    for c in pod["spec"]["containers"].as_array().into_iter().flatten() {
        let name = str_at(c, &["name"]);
        let st = statuses.iter().find(|s| str_at(s, &["name"]) == name);
        let (state, ok) = match st.map(|s| &s["state"]) {
            Some(s) if s["running"].is_object() => ("running".to_string(), st.unwrap()["ready"].as_bool() == Some(true)),
            Some(s) if s["waiting"].is_object() => (str_at(s, &["waiting", "reason"]).to_string(), false),
            Some(s) if s["terminated"].is_object() => (str_at(s, &["terminated", "reason"]).to_string(), false),
            _ => ("pending".to_string(), false),
        };
        let ports: Vec<String> = c["ports"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| format!("{}/{}", p["containerPort"], p["protocol"].as_str().unwrap_or("TCP")))
            .collect();
        let restarts = st.and_then(|s| s["restartCount"].as_u64()).unwrap_or(0);
        let mut detail = str_at(c, &["image"]).to_string();
        if !ports.is_empty() {
            detail.push_str(&format!(" · ports {}", ports.join(", ")));
        }
        if restarts > 0 {
            detail.push_str(&format!(" · {restarts} restarts"));
        }
        out.push(ContainerService { kind: ServiceKind::PodContainer, name: name.to_string(), state, ok, detail });
    }

    let labels = pod["metadata"]["labels"].as_object();
    for s in svcs["items"].as_array().into_iter().flatten() {
        let Some(selector) = s["spec"]["selector"].as_object().filter(|m| !m.is_empty()) else { continue };
        let selects = selector.iter().all(|(k, v)| labels.and_then(|l| l.get(k)) == Some(v));
        if !selects {
            continue;
        }
        let ports: Vec<String> = s["spec"]["ports"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| {
                let mut s = format!("{}/{}", p["port"], p["protocol"].as_str().unwrap_or("TCP"));
                match &p["targetPort"] {
                    Value::Null => {}
                    t if *t == p["port"] => {}
                    Value::String(name) => s.push_str(&format!("→{name}")),
                    t => s.push_str(&format!("→{t}")),
                }
                if let Some(n) = p["nodePort"].as_u64() {
                    s.push_str(&format!(" (node {n})"));
                }
                s
            })
            .collect();
        let ip = str_at(s, &["spec", "clusterIP"]);
        let external: Vec<String> = s["status"]["loadBalancer"]["ingress"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| i["ip"].as_str().or(i["hostname"].as_str()).unwrap_or("").to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let mut detail = format!("{ip} · {}", ports.join(", "));
        if !external.is_empty() {
            detail.push_str(&format!(" · external {}", external.join(", ")));
        }
        out.push(ContainerService {
            kind: ServiceKind::KubeService,
            name: str_at(s, &["metadata", "name"]).to_string(),
            state: str_at(s, &["spec", "type"]).to_string(),
            ok: true,
            detail,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// Answers commands by substring match and records what was run.
    struct Fake {
        rules: Vec<(&'static str, i32, &'static str, &'static str)>,
        log: RefCell<Vec<String>>,
    }

    impl Exec for Fake {
        fn exec(&self, command: &str) -> Result<ExecResult> {
            self.log.borrow_mut().push(command.to_string());
            for (pat, code, out, err) in &self.rules {
                if command.contains(pat) {
                    return Ok(ExecResult { stdout: out.to_string(), stderr: err.to_string(), exit_status: *code });
                }
            }
            Ok(ExecResult { stdout: String::new(), stderr: "not found".into(), exit_status: 127 })
        }
    }

    const PODS: &str = r#"{"items":[{"metadata":{"name":"web-7d9","namespace":"shop","labels":{"app":"web","tier":"fe"}},
        "spec":{"nodeName":"n1","containers":[{"name":"nginx","image":"nginx:1.27","ports":[{"containerPort":80,"protocol":"TCP"}]},
                                               {"name":"sidecar","image":"busybox"}]},
        "status":{"phase":"Running","podIP":"10.42.0.7","containerStatuses":[
            {"name":"nginx","ready":true,"restartCount":0,"state":{"running":{}}},
            {"name":"sidecar","ready":false,"restartCount":3,"state":{"waiting":{"reason":"CrashLoopBackOff"}}}]}}]}"#;

    const SVCS: &str = r#"{"items":[
        {"metadata":{"name":"web"},"spec":{"type":"LoadBalancer","clusterIP":"10.43.1.2","selector":{"app":"web"},
            "ports":[{"port":443,"protocol":"TCP","targetPort":8443,"nodePort":30443}]},
         "status":{"loadBalancer":{"ingress":[{"ip":"192.168.1.50"}]}}},
        {"metadata":{"name":"db"},"spec":{"type":"ClusterIP","clusterIP":"10.43.1.3","selector":{"app":"db"},"ports":[{"port":5432}]}},
        {"metadata":{"name":"kubernetes"},"spec":{"type":"ClusterIP","clusterIP":"10.43.0.1","ports":[{"port":443}]}}]}"#;

    #[test]
    fn detects_runtimes_and_falls_back_to_sudo() {
        let fake = Fake {
            rules: vec![
                ("for t in", 0, "docker\nlxc-ls\nkubectl\n", ""),
                ("sudo -n docker ps", 0, "abc123\tweb\tnginx:1.27\trunning\tUp 3 hours\t0.0.0.0:8080->80/tcp, :::8080->80/tcp\n", ""),
                ("docker ps", 1, "", "permission denied while trying to connect to the Docker daemon socket"),
                ("lxc-ls", 0, "NAME   STATE   IPV4\nbox1   RUNNING 10.0.3.5, 10.0.3.6\nbox2   STOPPED -\n", ""),
                ("kubectl get pods", 1, "", "The connection to the server localhost:8080 was refused"),
            ],
            log: RefCell::new(Vec::new()),
        };
        let list = list(&fake).unwrap();
        assert_eq!(list.runtimes, vec![Runtime::Docker, Runtime::Lxc, Runtime::Kubernetes]);
        assert_eq!(list.warnings.len(), 1, "{:?}", list.warnings);
        assert!(list.warnings[0].starts_with("kubectl: The connection"));
        // kubectl's failure doesn't look like a privilege problem: no sudo retry.
        assert!(!fake.log.borrow().iter().any(|c| c.contains("sudo -n kubectl")));

        let web = &list.containers[0];
        assert_eq!((web.name.as_str(), web.running, web.sudo), ("web", true, true));
        assert_eq!(web.backend(), "docker (sudo)");
        assert_eq!(web.action_command(ContainerAction::Restart).unwrap(), "sudo -n docker restart 'abc123'");

        let box1 = &list.containers[1];
        assert_eq!((box1.name.as_str(), box1.running), ("box1", true));
        assert_eq!(box1.ports, "10.0.3.5, 10.0.3.6");
        assert_eq!(box1.action_command(ContainerAction::Restart).unwrap(), "lxc-stop -r -n 'box1'");
        assert!(!list.containers[2].running);
        assert_eq!(list.containers[2].ports, "");
    }

    #[test]
    fn docker_services_include_ports_and_processes() {
        let c = parse_docker_ps(
            "abc\tweb\tnginx\trunning\tUp 1 hour\t0.0.0.0:8080->80/tcp, :::8080->80/tcp\n",
            Runtime::Docker,
            &Tool::Docker,
            false,
        )
        .remove(0);
        let fake = Fake {
            rules: vec![("docker top 'abc' -o pid,user,args", 0,
                "PID  USER  COMMAND\n101  root  nginx: master process nginx -g daemon off;\n140  101   nginx: worker process\n", "")],
            log: RefCell::new(Vec::new()),
        };
        let s = services(&fake, &c).unwrap();
        assert_eq!(s.len(), 4);
        assert_eq!((s[0].kind, s[0].name.as_str()), (ServiceKind::Port, "0.0.0.0:8080->80/tcp"));
        assert_eq!((s[2].kind, s[2].name.as_str(), s[2].state.as_str()), (ServiceKind::Process, "nginx:", "pid 101 · root"));
    }

    #[test]
    fn lxc_services_from_systemd() {
        let c = parse_incus(
            r#"[{"name":"db","status":"Running","type":"container","config":{"image.description":"Debian 12"},
                 "state":{"network":{"lo":{"addresses":[{"family":"inet","address":"127.0.0.1","scope":"local"}]},
                 "eth0":{"addresses":[{"family":"inet","address":"10.1.1.4","scope":"global"}]}}}}]"#,
            &Tool::Incus,
            false,
        )
        .unwrap()
        .remove(0);
        assert_eq!((c.image.as_str(), c.ports.as_str(), c.running), ("Debian 12", "10.1.1.4", true));
        let fake = Fake {
            rules: vec![("incus exec 'db' -- sh -c", 0,
                "#systemd\ncron.service loaded active running Regular background program processing daemon\n\
                 ● postgresql.service loaded failed failed PostgreSQL RDBMS\nfoo.service not-found inactive dead foo.service\n", "")],
            log: RefCell::new(Vec::new()),
        };
        let s = services(&fake, &c).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].name.as_str(), s[0].state.as_str(), s[0].ok), ("cron", "active (running)", true));
        assert_eq!((s[1].name.as_str(), s[1].ok), ("postgresql", false));
        assert_eq!(s[1].detail, "PostgreSQL RDBMS");
    }

    #[test]
    fn pods_and_their_services() {
        let pods = parse_pods(PODS, "k3s kubectl", false).unwrap();
        let p = &pods[0];
        assert_eq!(p.display_name(), "shop/web-7d9");
        assert_eq!(p.state, "CrashLoopBackOff");
        assert!(!p.running);
        assert_eq!(p.status, "1/2 ready, 3 restarts, on n1");
        assert!(!p.supports(ContainerAction::Stop) && p.supports(ContainerAction::Restart));
        assert_eq!(p.logs_command(), "k3s kubectl logs -n 'shop' 'web-7d9' --all-containers --prefix --tail=200 2>&1");

        let pod = serde_json::from_str::<Value>(PODS).unwrap()["items"][0].to_string();
        let s = parse_pod_services(&pod, SVCS).unwrap();
        assert_eq!(s.len(), 3, "{s:?}");
        assert_eq!((s[0].name.as_str(), s[0].ok), ("nginx", true));
        assert_eq!(s[0].detail, "nginx:1.27 · ports 80/TCP");
        assert_eq!((s[1].state.as_str(), s[1].ok), ("CrashLoopBackOff", false));
        assert_eq!((s[2].kind, s[2].name.as_str(), s[2].state.as_str()), (ServiceKind::KubeService, "web", "LoadBalancer"));
        assert_eq!(s[2].detail, "10.43.1.2 · 443/TCP→8443 (node 30443) · external 192.168.1.50");
    }

    #[test]
    fn proxmox_and_quoting() {
        let cts = parse_pct("VMID       Status     Lock         Name\n100        running                 dns\n101        stopped    backup       old'one\n", false);
        assert_eq!(cts.len(), 2);
        assert_eq!((cts[0].id.as_str(), cts[0].name.as_str(), cts[0].running), ("100", "dns", true));
        assert_eq!(cts[1].status, "stopped, locked: backup");
        assert_eq!(cts[0].action_command(ContainerAction::Stop).unwrap(), "pct shutdown '100'");
        assert_eq!(cts[0].shell_command(), "pct enter '100'");
        assert_eq!(quote("old'one"), r"'old'\''one'");
    }
}
