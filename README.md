# manageriliki

Manage the servers on your local network from one Linux machine, over **SSH**,
**RS232 serial** or **VNC**, through a **terminal UI** or a **native desktop GUI**,
with built-in **user accounts and roles**.

## Features

- Server inventory with groups, notes, and live reachability status
- Interactive SSH shells and serial consoles with full VT100 emulation
  (colors, `top`, `vim`, …), resizing, and scrollback
- Built-in VNC viewer (GUI) for graphical desktops, VM consoles and BMC/KVM
  displays, either direct or **tunneled through the server's SSH** (encrypted,
  and reaches VNC servers that only listen on localhost)
- Run a command over SSH on one server, a whole group, or every server at once
- Container view per server for **Docker**, **Podman**, **LXC** (Incus, LXD,
  Proxmox `pct`, plain `lxc-*`) and **Kubernetes** (kubectl, k3s, MicroK8s):
  state, image, ports, and the services of each container, plus start / stop /
  restart, logs and a shell inside it
- Accounts with three roles, enforced in the core library on every call:

  | Role     | See servers | Connect / run commands | Manage servers, users, audit log |
  |----------|:-----------:|:----------------------:|:--------------------------------:|
  | viewer   | ✓           |                        |                                  |
  | operator | ✓           | ✓                      |                                  |
  | admin    | ✓           | ✓                      | ✓                                |

  Each server can also be restricted to admins only.
- SSH host keys are pinned on first connection; a changed key is refused
  (an admin can reset it after a legitimate reinstall)
- Stored SSH passwords and key passphrases are encrypted (XChaCha20-Poly1305)
- An audit log of logins, failed logins, session open/close, commands run,
  and every change to servers and users

## Build

Requires Rust (pinned in `mise.toml`; run `mise install`) and the system
`libssh2`/`openssl` development files (present on Arch by default).

```sh
cargo build --release
./target/release/manageriliki-tui   # terminal UI
./target/release/manageriliki-gui   # desktop GUI
```

On first start, both frontends ask you to create the initial admin account.

## Data

Everything is stored in `~/.local/share/manageriliki/` (override with
`MANAGERILIKI_DATA_DIR`):

- `manageriliki.db`: SQLite database with users (argon2id password hashes),
  servers, and the audit log
- `vault.key`: the key that encrypts stored server secrets (mode 0600)

The TUI and GUI share the same data and can run at the same time.

**Security note:** roles are enforced by the application. Anyone who can read
the data directory as a Linux user can bypass them, and can decrypt stored
secrets if they can read `vault.key` too. For a shared multi-user setup, run
the program under a dedicated system account that owns the data directory.
Prefer ssh-agent or key-file authentication over stored passwords.

## VNC

Supported: RFB 3.3/3.7/3.8 with no authentication or a VNC password, and the
Raw, CopyRect and ZRLE encodings, which cover TigerVNC, x11vnc, wayvnc,
QEMU/libvirt and most BMCs. TLS/VeNCrypt and Apple's auth are not supported;
use the SSH tunnel instead, which encrypts everything. Direct VNC is
unencrypted, which is acceptable on a trusted LAN. VNC passwords are limited to
8 characters by the protocol.

In the GUI, click a display to give it keyboard focus. Ctrl+V sends your
clipboard to the remote clipboard and pastes it, and remote clipboard changes
are copied locally. Use "View only" to watch without sending input. The TUI
can configure VNC but can't display it.

Examples of server-side setup:

```sh
x11vnc -localhost -display :0           # X11 desktop; use the SSH tunnel
wayvnc 127.0.0.1 5900                   # Wayland (wlroots) desktop
qemu-system-x86_64 ... -vnc 127.0.0.1:1 # VM console on port 5901
```

## Containers

Open **Containers** on a server (GUI button, or `t` in the TUI). Everything
runs over the server's SSH with the runtimes' own CLIs; nothing is installed
on the server. The tools found on `PATH` (plus `/usr/sbin`, `/sbin`,
`/snap/bin`) are used:

| Runtime    | Tools                                  | Services shown for a container                |
|------------|----------------------------------------|-----------------------------------------------|
| Docker     | `docker`                               | published ports and running processes (`top`) |
| Podman     | `podman`                               | published ports and running processes (`top`) |
| LXC        | `incus`, `lxc` (LXD), `pct`, `lxc-ls`  | systemd services inside, else processes       |
| Kubernetes | `kubectl`, `k3s kubectl`, `microk8s kubectl` | the pod's containers and the Services selecting it |

If a tool is refused for lack of privileges (not in the `docker` group, k3s'
root-only kubeconfig, …), it is retried once with `sudo -n`, so passwordless
sudo for that tool is enough. Kubernetes pods can't be started or stopped;
"Delete pod" makes their controller recreate them. Container actions, log
views and shells are recorded in the audit log. Connecting to containers needs
the same role as connecting to the server.

## Serial ports

Your Linux user needs access to the device. On Arch that means the `uucp`
group (`dialout` on Debian/Ubuntu):

```sh
sudo usermod -aG uucp "$USER"   # then log out and back in
```

## TUI keys

| Key                 | Action                                          |
|---------------------|-------------------------------------------------|
| `1` `2` `3` / Tab   | Servers / Users / Audit tabs                    |
| Enter or `s`        | SSH shell                                       |
| `c`                 | Serial console                                  |
| `v`                 | VNC (shows a hint: the viewer is in the GUI)    |
| `t`                 | Containers (Docker, Podman, LXC, Kubernetes)    |
| `x`                 | Run a command (this server, its group, or all)  |
| `a` `e` `d`         | Add / edit / delete                             |
| `h`                 | Forget a server's trusted host key              |
| `p`                 | Reset a user's password (Users tab)             |
| `w`                 | Change my password                              |
| `o` / `q`           | Log out / quit                                  |
| Ctrl+] or F12       | Leave a session                                 |

In the containers view: Tab cycles the runtime filter, Enter/`l` shows logs,
`s` opens a shell, `u` / `x` / `R` start / stop / restart, PgUp/PgDn scroll
the services, Esc goes back.
| Shift+PgUp/PgDn     | Scroll back in a session                        |

## Layout

```
crates/core   storage, accounts, vault, audit, SSH/serial transports, terminal sessions, containers
crates/tui    ratatui frontend
crates/gui    egui/eframe frontend
```

## Tests

```sh
cargo test --workspace
```

The GUI test runs headlessly. Live tests against real servers are opt-in:

| Variable                          | Enables                                        |
|-----------------------------------|------------------------------------------------|
| `MGR_TEST_SSH=host:port:user:key` | SSH session (GUI) and VNC-over-SSH tunnel      |
| `MGR_TEST_VNC=host:port[:pass]`   | VNC connect, auth and wrong-password checks    |
| `MGR_TEST_VNC_ECHO=1`             | Typing test; needs a guest that echoes keys    |
| `MGR_SCREENSHOT_DIR=dir`          | Save rendered GUI screenshots                  |

```sh
MGR_TEST_VNC=127.0.0.1:5901 cargo test -p manageriliki-core --test live_vnc -- --nocapture
MGR_TEST_VNC=127.0.0.1:5901 MGR_SCREENSHOT_DIR=/tmp/shots cargo test -p manageriliki-gui
```

## CI

GitHub Actions runs on every push to `main` and every pull request:

- **CI** (`.github/workflows/ci.yml`): `cargo fmt --check`, clippy with
  warnings as errors, debug and release builds against the committed
  `Cargo.lock`, the full test suite (GUI screenshots are uploaded as an
  artifact) and a strict docs build.
- **Security** (`.github/workflows/security.yml`, also weekly):
  `cargo-deny` for RustSec advisories, licenses and crate sources
  (`deny.toml`), a gitleaks scan of the full history (`.gitleaks.toml`), and a
  check that every crate keeps `#![forbid(unsafe_code)]`.

`crates/core/tests/safety.rs` holds the security invariants: owner-only data
files, no plaintext secrets or passwords on disk or in the API, the role
matrix, per-server required roles, revalidation of logged-in sessions, and
hostile input handling. Run it locally with
`cargo test -p manageriliki-core --test safety`.
