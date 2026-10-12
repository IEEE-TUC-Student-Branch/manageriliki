//! The application facade used by every frontend. Every operation takes the
//! caller's `Identity`, re-checks their account and role against the database,
//! and records security-relevant actions in the audit log.

use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread;

use rusqlite::Connection;

use crate::auth;
use crate::db::{self, ServerRow};
use crate::error::{Error, Result, invalid};
use crate::models::*;
use crate::terminal::{Connector, Notify, Terminal};
use crate::transport::{self, Link, SerialLink, SshLink};
use crate::vault::Vault;

/// A logged-in user. Permissions are re-validated on every call, so changes
/// to the account (role, disabled) take effect immediately.
#[derive(Clone, Debug)]
pub struct Identity {
    pub user_id: i64,
    pub username: String,
    pub role: Role,
}

#[derive(Clone, Debug)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_status: i32,
}

#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

struct Inner {
    conn: Mutex<Connection>,
    vault: Vault,
    data_dir: PathBuf,
}

impl Manager {
    /// Data directory: `$MANAGERILIKI_DATA_DIR`, else `~/.local/share/manageriliki`.
    pub fn default_data_dir() -> Result<PathBuf> {
        if let Some(dir) = std::env::var_os("MANAGERILIKI_DATA_DIR") {
            return Ok(PathBuf::from(dir));
        }
        directories::ProjectDirs::from("", "", "manageriliki")
            .map(|d| d.data_dir().to_path_buf())
            .ok_or_else(|| invalid("cannot determine a data directory; set MANAGERILIKI_DATA_DIR"))
    }

    pub fn open_default() -> Result<Self> {
        Self::open(&Self::default_data_dir()?)
    }

    pub fn open(data_dir: &Path) -> Result<Self> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(data_dir)?;
        let conn = db::open(&data_dir.join("manageriliki.db"))?;
        let vault = Vault::open(&data_dir.join("vault.key"))?;
        Ok(Manager {
            inner: Arc::new(Inner {
                conn: Mutex::new(conn),
                vault,
                data_dir: data_dir.to_path_buf(),
            }),
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.inner.data_dir
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        self.inner.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn audit(&self, who: &str, action: &str, target: &str, detail: &str) {
        // Auditing must never break the operation it describes.
        let _ = db::audit(&self.db(), who, action, target, detail);
    }

    fn authorize(&self, who: &Identity, min: Role) -> Result<User> {
        let user = db::get_user(&self.db(), who.user_id).map_err(|_| Error::InvalidCredentials)?;
        if user.disabled {
            return Err(Error::Disabled);
        }
        if user.role < min {
            return Err(Error::Forbidden(min));
        }
        Ok(user)
    }

    /// Re-reads the caller's account, e.g. to refresh a role shown in the UI.
    pub fn refresh_identity(&self, who: &Identity) -> Result<Identity> {
        let user = self.authorize(who, Role::Viewer)?;
        Ok(Identity {
            user_id: user.id,
            username: user.username,
            role: user.role,
        })
    }

    // ---- accounts ----

    /// True until the first (admin) account has been created.
    pub fn needs_setup(&self) -> Result<bool> {
        Ok(db::count_users(&self.db())? == 0)
    }

    pub fn create_initial_admin(&self, username: &str, password: &str) -> Result<Identity> {
        auth::validate_username(username)?;
        auth::validate_password(password)?;
        let hash = auth::hash_password(password)?;
        let conn = self.db();
        if db::count_users(&conn)? != 0 {
            return Err(invalid("setup has already been completed"));
        }
        let id = db::insert_user(&conn, username, &hash, Role::Admin)?;
        db::touch_last_login(&conn, id)?;
        db::audit(&conn, username, "setup", username, "created initial admin")?;
        Ok(Identity {
            user_id: id,
            username: username.to_string(),
            role: Role::Admin,
        })
    }

    pub fn login(&self, username: &str, password: &str) -> Result<Identity> {
        let found = db::find_user_for_login(&self.db(), username.trim())?;
        let Some((user, hash)) = found else {
            auth::verify_dummy(password);
            self.audit(username, "login.fail", username, "unknown user");
            return Err(Error::InvalidCredentials);
        };
        if !auth::verify_password(password, &hash) {
            self.audit(&user.username, "login.fail", &user.username, "bad password");
            return Err(Error::InvalidCredentials);
        }
        if user.disabled {
            self.audit(&user.username, "login.fail", &user.username, "account disabled");
            return Err(Error::Disabled);
        }
        let conn = self.db();
        db::touch_last_login(&conn, user.id)?;
        db::audit(&conn, &user.username, "login", &user.username, "")?;
        Ok(Identity {
            user_id: user.id,
            username: user.username,
            role: user.role,
        })
    }

    pub fn logout(&self, who: &Identity) {
        self.audit(&who.username, "logout", &who.username, "");
    }

    pub fn list_users(&self, who: &Identity) -> Result<Vec<User>> {
        self.authorize(who, Role::Admin)?;
        db::list_users(&self.db())
    }

    pub fn create_user(
        &self,
        who: &Identity,
        username: &str,
        password: &str,
        role: Role,
    ) -> Result<User> {
        self.authorize(who, Role::Admin)?;
        auth::validate_username(username)?;
        auth::validate_password(password)?;
        let hash = auth::hash_password(password)?;
        let conn = self.db();
        let id = db::insert_user(&conn, username, &hash, role)?;
        db::audit(&conn, &who.username, "user.create", username, role.as_str())?;
        db::get_user(&conn, id)
    }

    /// Changes a user's role and enabled state. The last active admin cannot
    /// be demoted or disabled, and admins cannot disable themselves.
    pub fn update_user(&self, who: &Identity, id: i64, role: Role, disabled: bool) -> Result<()> {
        self.authorize(who, Role::Admin)?;
        let conn = self.db();
        let target = db::get_user(&conn, id)?;
        if id == who.user_id && disabled {
            return Err(invalid("you cannot disable your own account"));
        }
        let loses_admin =
            target.role == Role::Admin && !target.disabled && (role != Role::Admin || disabled);
        if loses_admin && db::count_active_admins(&conn)? <= 1 {
            return Err(invalid("cannot demote or disable the last active admin"));
        }
        db::update_user(&conn, id, role, disabled)?;
        let detail = format!("role={role} disabled={disabled}");
        db::audit(&conn, &who.username, "user.update", &target.username, &detail)?;
        Ok(())
    }

    pub fn reset_password(&self, who: &Identity, id: i64, new_password: &str) -> Result<()> {
        self.authorize(who, Role::Admin)?;
        auth::validate_password(new_password)?;
        let hash = auth::hash_password(new_password)?;
        let conn = self.db();
        let target = db::get_user(&conn, id)?;
        db::set_password_hash(&conn, id, &hash)?;
        db::audit(&conn, &who.username, "user.password_reset", &target.username, "")?;
        Ok(())
    }

    pub fn change_own_password(&self, who: &Identity, old: &str, new: &str) -> Result<()> {
        self.authorize(who, Role::Viewer)?;
        let current = db::password_hash(&self.db(), who.user_id)?;
        if !auth::verify_password(old, &current) {
            return Err(invalid("current password is incorrect"));
        }
        auth::validate_password(new)?;
        let hash = auth::hash_password(new)?;
        let conn = self.db();
        db::set_password_hash(&conn, who.user_id, &hash)?;
        db::audit(&conn, &who.username, "user.password_change", &who.username, "")?;
        Ok(())
    }

    pub fn delete_user(&self, who: &Identity, id: i64) -> Result<()> {
        self.authorize(who, Role::Admin)?;
        if id == who.user_id {
            return Err(invalid("you cannot delete your own account"));
        }
        let conn = self.db();
        let target = db::get_user(&conn, id)?;
        if target.role == Role::Admin && !target.disabled && db::count_active_admins(&conn)? <= 1 {
            return Err(invalid("cannot delete the last active admin"));
        }
        db::delete_user(&conn, id)?;
        db::audit(&conn, &who.username, "user.delete", &target.username, "")?;
        Ok(())
    }

    // ---- servers ----

    pub fn list_servers(&self, who: &Identity) -> Result<Vec<Server>> {
        self.authorize(who, Role::Viewer)?;
        db::list_servers(&self.db())
    }

    pub fn get_server(&self, who: &Identity, id: i64) -> Result<Server> {
        self.authorize(who, Role::Viewer)?;
        db::get_server(&self.db(), id)
    }

    /// Creates (`id == None`) or updates a server. Returns its id.
    pub fn save_server(&self, who: &Identity, id: Option<i64>, input: ServerInput) -> Result<i64> {
        self.authorize(who, Role::Admin)?;
        let input = normalize_server(input)?;

        let conn = self.db();
        let had_secret = match id {
            Some(id) => db::get_server(&conn, id)?.has_secret,
            None => false,
        };
        let uses_secret = matches!(
            input.ssh.as_ref().map(|s| &s.auth),
            Some(SshAuth::Password | SshAuth::Key { .. })
        );
        let password_auth = matches!(input.ssh.as_ref().map(|s| &s.auth), Some(SshAuth::Password));
        let will_have_secret = match &input.secret {
            SecretUpdate::Keep => had_secret,
            SecretUpdate::Clear => false,
            SecretUpdate::Set(s) => !s.is_empty(),
        };
        if password_auth && !will_have_secret {
            return Err(invalid("password authentication needs a password"));
        }

        let row = ServerRow {
            name: &input.name,
            group: &input.group,
            notes: &input.notes,
            ssh: &input.ssh,
            serial: &input.serial,
            required_role: input.required_role,
        };
        let id = match id {
            Some(id) => {
                db::update_server(&conn, id, &row)?;
                id
            }
            None => db::insert_server(&conn, &row)?,
        };
        match (&input.secret, uses_secret) {
            (_, false) | (SecretUpdate::Clear, _) => db::set_secret(&conn, id, None)?,
            (SecretUpdate::Set(s), true) if s.is_empty() => db::set_secret(&conn, id, None)?,
            (SecretUpdate::Set(s), true) => {
                db::set_secret(&conn, id, Some(&self.inner.vault.encrypt(s)?))?
            }
            (SecretUpdate::Keep, true) => {}
        }
        db::audit(&conn, &who.username, "server.save", &input.name, "")?;
        Ok(id)
    }

    pub fn delete_server(&self, who: &Identity, id: i64) -> Result<()> {
        self.authorize(who, Role::Admin)?;
        let conn = self.db();
        let server = db::get_server(&conn, id)?;
        db::delete_server(&conn, id)?;
        db::audit(&conn, &who.username, "server.delete", &server.name, "")?;
        Ok(())
    }

    /// Forgets the trusted host key so the next connection re-learns it.
    pub fn reset_host_key(&self, who: &Identity, id: i64) -> Result<()> {
        self.authorize(who, Role::Admin)?;
        let conn = self.db();
        let server = db::get_server(&conn, id)?;
        db::set_host_key(&conn, id, None)?;
        db::audit(&conn, &who.username, "hostkey.reset", &server.name, "")?;
        Ok(())
    }

    // ---- connections ----

    fn connectable(&self, who: &Identity, server_id: i64) -> Result<Server> {
        let user = self.authorize(who, Role::Operator)?;
        let server = db::get_server(&self.db(), server_id)?;
        if user.role < server.required_role {
            return Err(Error::Forbidden(server.required_role));
        }
        Ok(server)
    }

    fn secret(&self, server_id: i64) -> Result<Option<String>> {
        db::get_secret(&self.db(), server_id)?
            .map(|s| self.inner.vault.decrypt(&s))
            .transpose()
    }

    /// Trust-on-first-use host key verification.
    fn check_host_key(&self, who: &str, server: &Server, fp: &str) -> Result<()> {
        let conn = self.db();
        match db::get_server(&conn, server.id)?.host_key_fp {
            None => {
                db::set_host_key(&conn, server.id, Some(fp))?;
                db::audit(&conn, who, "hostkey.trust", &server.name, fp)?;
                Ok(())
            }
            Some(known) if known == fp => Ok(()),
            Some(known) => {
                let detail = format!("expected {known}, got {fp}");
                db::audit(&conn, who, "hostkey.mismatch", &server.name, &detail)?;
                Err(Error::HostKeyMismatch {
                    server: server.name.clone(),
                    expected: known,
                    actual: fp.to_string(),
                })
            }
        }
    }

    fn ssh_connect(&self, who: &str, server: &Server) -> Result<transport::SshConnection> {
        let cfg = server
            .ssh
            .as_ref()
            .ok_or_else(|| invalid(format!("{} has no SSH configuration", server.name)))?;
        let secret = self.secret(server.id)?;
        transport::ssh_connect(cfg, secret.as_deref(), |fp| {
            self.check_host_key(who, server, fp)
        })
    }

    /// Opens an interactive terminal. Connecting happens in the background;
    /// watch `Terminal::state()`. `notify` is called whenever the screen changes.
    pub fn open_terminal(
        &self,
        who: &Identity,
        server_id: i64,
        kind: TransportKind,
        rows: u16,
        cols: u16,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Terminal> {
        let server = self.connectable(who, server_id)?;
        let connect: Connector = match kind {
            TransportKind::Ssh => {
                if server.ssh.is_none() {
                    return Err(invalid(format!("{} has no SSH configuration", server.name)));
                }
                let (mgr, who, server) = (self.clone(), who.username.clone(), server.clone());
                Box::new(move || {
                    let conn = mgr.ssh_connect(&who, &server)?;
                    Ok(Box::new(SshLink::open_shell(conn, rows, cols)?) as Box<dyn Link>)
                })
            }
            TransportKind::Serial => {
                let cfg = server
                    .serial
                    .clone()
                    .ok_or_else(|| invalid(format!("{} has no serial configuration", server.name)))?;
                Box::new(move || Ok(Box::new(SerialLink::open(&cfg)?) as Box<dyn Link>))
            }
        };

        self.audit(&who.username, "session.open", &server.name, &kind.to_string());
        let (mgr, username, name) = (self.clone(), who.username.clone(), server.name.clone());
        let on_close = Box::new(move |err: Option<&str>| {
            mgr.audit(&username, "session.close", &name, err.unwrap_or(""));
        });
        let title = format!("{} ({kind})", server.name);
        Ok(Terminal::spawn(title, rows, cols, notify as Notify, connect, on_close))
    }

    /// Runs a command over SSH and waits for it to finish. Blocking: call it
    /// from a worker thread in interactive frontends.
    pub fn run_command(&self, who: &Identity, server_id: i64, command: &str) -> Result<CommandOutput> {
        let server = self.connectable(who, server_id)?;
        self.audit(&who.username, "command.run", &server.name, command);
        let conn = self.ssh_connect(&who.username, &server)?;
        let out = transport::ssh_exec(&conn, command)?;
        Ok(CommandOutput {
            stdout: out.stdout,
            stderr: out.stderr,
            exit_status: out.exit_status,
        })
    }

    /// Runs `command` on several servers in parallel. Each result arrives on
    /// the returned channel as `(server name, result)`; `notify` fires after each.
    pub fn run_command_many(
        &self,
        who: &Identity,
        servers: &[Server],
        command: &str,
        notify: Notify,
    ) -> mpsc::Receiver<(String, Result<CommandOutput>)> {
        let (tx, rx) = mpsc::channel();
        for server in servers {
            let (mgr, who, tx, notify) = (self.clone(), who.clone(), tx.clone(), notify.clone());
            let (id, name, command) = (server.id, server.name.clone(), command.to_string());
            thread::spawn(move || {
                let _ = tx.send((name, mgr.run_command(&who, id, &command)));
                notify();
            });
        }
        rx
    }

    // ---- audit ----

    pub fn audit_log(&self, who: &Identity, limit: usize) -> Result<Vec<AuditEntry>> {
        self.authorize(who, Role::Admin)?;
        db::list_audit(&self.db(), limit)
    }
}

fn normalize_server(mut input: ServerInput) -> Result<ServerInput> {
    input.name = input.name.trim().to_string();
    input.group = input.group.trim().to_string();
    if input.name.is_empty() {
        return Err(invalid("server name is required"));
    }
    if input.ssh.is_none() && input.serial.is_none() {
        return Err(invalid("configure SSH, serial, or both"));
    }
    if let Some(ssh) = &mut input.ssh {
        ssh.host = ssh.host.trim().to_string();
        ssh.username = ssh.username.trim().to_string();
        if ssh.host.is_empty() || ssh.username.is_empty() || ssh.port == 0 {
            return Err(invalid("SSH needs a host, port and username"));
        }
        if let SshAuth::Key { path } = &mut ssh.auth {
            *path = path.trim().to_string();
            if path.is_empty() {
                return Err(invalid("key authentication needs a key file path"));
            }
        }
    }
    if let Some(serial) = &mut input.serial {
        serial.device = serial.device.trim().to_string();
        if serial.device.is_empty() || serial.baud == 0 {
            return Err(invalid("serial needs a device and baud rate"));
        }
        if !(5..=8).contains(&serial.data_bits) || !(1..=2).contains(&serial.stop_bits) {
            return Err(invalid("data bits must be 5-8 and stop bits 1-2"));
        }
    }
    // Viewers can never connect, so the floor is operator.
    input.required_role = input.required_role.max(Role::Operator);
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_manager(tag: &str) -> (Manager, PathBuf) {
        let dir = std::env::temp_dir().join(format!("mgr-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (Manager::open(&dir).unwrap(), dir)
    }

    #[test]
    fn setup_login_and_roles() {
        let (m, dir) = temp_manager("roles");
        assert!(m.needs_setup().unwrap());
        let admin = m.create_initial_admin("admin", "adminpass1").unwrap();
        assert!(!m.needs_setup().unwrap());
        assert!(m.create_initial_admin("other", "adminpass1").is_err());

        assert!(matches!(m.login("admin", "nope"), Err(Error::InvalidCredentials)));
        assert!(matches!(m.login("ghost", "adminpass1"), Err(Error::InvalidCredentials)));
        m.login("ADMIN", "adminpass1").unwrap();

        let viewer = m.create_user(&admin, "val", "viewerpass", Role::Viewer).unwrap();
        let v = m.login("val", "viewerpass").unwrap();
        assert!(m.list_servers(&v).is_ok());
        assert!(matches!(m.list_users(&v), Err(Error::Forbidden(Role::Admin))));
        assert!(matches!(
            m.run_command(&v, 1, "true"),
            Err(Error::Forbidden(Role::Operator))
        ));

        // Disabling takes effect for an already logged-in identity.
        m.update_user(&admin, viewer.id, Role::Viewer, true).unwrap();
        assert!(matches!(m.list_servers(&v), Err(Error::Disabled)));
        assert!(matches!(m.login("val", "viewerpass"), Err(Error::Disabled)));

        // The last admin is protected.
        assert!(m.update_user(&admin, admin.user_id, Role::Operator, false).is_err());
        assert!(m.update_user(&admin, admin.user_id, Role::Admin, true).is_err());
        assert!(m.delete_user(&admin, admin.user_id).is_err());

        m.change_own_password(&admin, "adminpass1", "adminpass2").unwrap();
        assert!(m.login("admin", "adminpass2").is_ok());
        assert!(m.audit_log(&admin, 100).unwrap().len() > 5);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn server_crud_and_secrets() {
        let (m, dir) = temp_manager("servers");
        let admin = m.create_initial_admin("admin", "adminpass1").unwrap();

        let mut input = ServerInput {
            name: "web1".into(),
            ssh: Some(SshConfig {
                host: "10.0.0.5".into(),
                auth: SshAuth::Password,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(m.save_server(&admin, None, input.clone()).is_err(), "password required");
        input.secret = SecretUpdate::Set("hunter22".into());
        let id = m.save_server(&admin, None, input.clone()).unwrap();
        assert!(m.get_server(&admin, id).unwrap().has_secret);
        assert_eq!(m.secret(id).unwrap().as_deref(), Some("hunter22"));
        assert!(m.save_server(&admin, None, input.clone()).is_err(), "duplicate name");

        // Switching to agent auth drops the stored secret.
        let mut edit = ServerInput::from_server(&m.get_server(&admin, id).unwrap());
        edit.ssh.as_mut().unwrap().auth = SshAuth::Agent;
        edit.serial = Some(SerialConfig::default());
        m.save_server(&admin, Some(id), edit).unwrap();
        let s = m.get_server(&admin, id).unwrap();
        assert!(!s.has_secret);
        assert_eq!(s.serial, Some(SerialConfig::default()));

        // Trust on first use, then reject a different key.
        m.check_host_key("admin", &s, "SHA256:aaa").unwrap();
        m.check_host_key("admin", &s, "SHA256:aaa").unwrap();
        assert!(matches!(
            m.check_host_key("admin", &s, "SHA256:bbb"),
            Err(Error::HostKeyMismatch { .. })
        ));
        m.reset_host_key(&admin, id).unwrap();
        m.check_host_key("admin", &s, "SHA256:bbb").unwrap();

        m.delete_server(&admin, id).unwrap();
        assert!(m.list_servers(&admin).unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
