# manageriliki

Manage the servers on your local network from one Linux machine, over **SSH**
or **RS232 serial**, through a **terminal UI** or a **native desktop GUI**,
with built-in **user accounts and roles**.

## Features

- Server inventory with groups, notes, and live reachability status
- Interactive SSH shells and serial consoles with full VT100 emulation
  (colors, `top`, `vim`, …), resizing, and scrollback
- Run a command over SSH on one server, a whole group, or every server at once
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
| `x`                 | Run a command (this server, its group, or all)  |
| `a` `e` `d`         | Add / edit / delete                             |
| `h`                 | Forget a server's trusted host key              |
| `p`                 | Reset a user's password (Users tab)             |
| `w`                 | Change my password                              |
| `o` / `q`           | Log out / quit                                  |
| Ctrl+] or F12       | Leave a session                                 |
| Shift+PgUp/PgDn     | Scroll back in a session                        |

## Layout

```
crates/core   storage, accounts, vault, audit, SSH/serial transports, terminal sessions
crates/tui    ratatui frontend
crates/gui    egui/eframe frontend
```

## Tests

```sh
cargo test --workspace
```

The GUI test runs headlessly. To also save screenshots and exercise a real
SSH session:

```sh
MGR_SCREENSHOT_DIR=/tmp/shots MGR_TEST_SSH=host:port:user:/path/to/key \
  cargo test -p manageriliki-gui
```
