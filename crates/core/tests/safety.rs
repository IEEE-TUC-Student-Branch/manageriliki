//! Security invariants that must never regress. Everything here runs offline:
//! access checks are expected to fail *before* any network connection is made,
//! so the servers point at TEST-NET addresses that are never reachable.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use manageriliki_core::{Error, Identity, Manager, Role, SecretUpdate, ServerInput, SshAuth, SshConfig, VncConfig};

const ADMIN_PW: &str = "Adm1n-Passw0rd!";
const SSH_SECRET: &str = "ssh-secret-Zq7w4n9X";
const VNC_SECRET: &str = "vncP4ss!";

struct Env {
    m: Manager,
    dir: PathBuf,
    admin: Identity,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn env(tag: &str) -> Env {
    let dir = std::env::temp_dir().join(format!("mgr-safety-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let m = Manager::open(&dir).unwrap();
    let admin = m.create_initial_admin("admin", ADMIN_PW).unwrap();
    Env { m, dir, admin }
}

fn user(e: &Env, name: &str, role: Role) -> Identity {
    let pw = format!("{name}-password");
    e.m.create_user(&e.admin, name, &pw, role).unwrap();
    e.m.login(name, &pw).unwrap()
}

fn server(name: &str, required_role: Role) -> ServerInput {
    ServerInput {
        name: name.into(),
        ssh: Some(SshConfig { host: "192.0.2.1".into(), auth: SshAuth::Password, ..Default::default() }),
        vnc: Some(VncConfig { via_ssh: true, ..Default::default() }),
        required_role,
        secret: SecretUpdate::Set(SSH_SECRET.into()),
        vnc_secret: SecretUpdate::Set(VNC_SECRET.into()),
        ..Default::default()
    }
}

/// Every byte the app has written to its data directory (DB, WAL, key file).
fn data_bytes(dir: &Path) -> Vec<u8> {
    let mut all = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            all.extend(fs::read(path).unwrap());
        }
    }
    all
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle.as_bytes())
}

fn forbidden<T: std::fmt::Debug>(r: manageriliki_core::Result<T>) -> bool {
    matches!(r, Err(Error::Forbidden(_)))
}

#[test]
fn data_files_are_private() {
    let e = env("perms");
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&e.dir), 0o700, "data directory must be owner-only");
    assert_eq!(mode(&e.dir.join("vault.key")), 0o600, "vault key must be owner-only");
}

#[test]
fn vault_key_is_never_regenerated() {
    let e = env("vaultkey");
    let key = fs::read(e.dir.join("vault.key")).unwrap();
    assert_eq!(key.len(), 32);
    drop(Manager::open(&e.dir).unwrap());
    assert_eq!(fs::read(e.dir.join("vault.key")).unwrap(), key, "reopening must not replace the key");

    fs::write(e.dir.join("vault.key"), b"too short").unwrap();
    assert!(Manager::open(&e.dir).is_err(), "a corrupt key file must be rejected, not replaced");
}

#[test]
fn secrets_and_passwords_never_stored_in_plaintext() {
    let e = env("plaintext");
    let id = e.m.save_server(&e.admin, None, server("db-01", Role::Operator)).unwrap();
    let s = e.m.get_server(&e.admin, id).unwrap();
    assert!(s.has_secret && s.has_vnc_secret);
    // Also exercise the other password paths.
    let op = user(&e, "olga", Role::Operator);
    e.m.change_own_password(&op, "olga-password", "olga-new-password").unwrap();
    e.m.reset_password(&e.admin, op.user_id, "olga-reset-password").unwrap();

    let bytes = data_bytes(&e.dir);
    for plain in [SSH_SECRET, VNC_SECRET, ADMIN_PW, "olga-password", "olga-new-password", "olga-reset-password"] {
        assert!(!contains(&bytes, plain), "{plain:?} found in plaintext in the data directory");
    }
    assert!(contains(&bytes, "$argon2id$"), "passwords must be argon2id hashes");
}

#[test]
fn secrets_are_not_exposed_through_the_api() {
    let e = env("api");
    let id = e.m.save_server(&e.admin, None, server("db-01", Role::Operator)).unwrap();
    let dump = format!("{:?}{:?}", e.m.get_server(&e.admin, id).unwrap(), e.m.list_servers(&e.admin).unwrap());
    assert!(!dump.contains(SSH_SECRET) && !dump.contains(VNC_SECRET), "server views leak a secret");
    let audit = format!("{:?}", e.m.audit_log(&e.admin, 1000).unwrap());
    assert!(!audit.contains(SSH_SECRET) && !audit.contains(VNC_SECRET), "audit log leaks a secret");
}

#[test]
fn login_failures_are_uniform_and_audited() {
    let e = env("login");
    assert!(matches!(e.m.login("admin", "wrong-password"), Err(Error::InvalidCredentials)));
    assert!(matches!(e.m.login("nobody", "wrong-password"), Err(Error::InvalidCredentials)));
    assert!(matches!(e.m.login("admin", ""), Err(Error::InvalidCredentials)));
    let fails = e.m.audit_log(&e.admin, 100).unwrap().into_iter().filter(|a| a.action == "login.fail").count();
    assert_eq!(fails, 3, "every failed login must be audited");

    let audit = format!("{:?}", e.m.audit_log(&e.admin, 100).unwrap());
    assert!(!audit.contains("wrong-password"), "attempted passwords must not be logged");
}

#[test]
fn weak_credentials_and_bad_usernames_rejected() {
    let e = env("weak");
    assert!(e.m.create_user(&e.admin, "bob", "short", Role::Viewer).is_err());
    for name in ["", "a b", "x'; DROP TABLE users;--", "../etc", &"a".repeat(33)] {
        assert!(e.m.create_user(&e.admin, name, "long-enough-pw", Role::Viewer).is_err(), "{name:?} accepted");
    }
    assert!(e.m.create_initial_admin("second", "long-enough-pw").is_err(), "setup can only run once");
}

#[test]
fn role_matrix() {
    let e = env("roles");
    let id = e.m.save_server(&e.admin, None, server("db-01", Role::Operator)).unwrap();
    let group = e.m.create_group(&e.admin, "web").unwrap();
    let viewer = user(&e, "vera", Role::Viewer);
    let operator = user(&e, "otto", Role::Operator);

    for who in [&viewer, &operator] {
        assert!(forbidden(e.m.list_users(who)));
        assert!(forbidden(e.m.create_user(who, "mallory", "mallory-password", Role::Admin)));
        assert!(forbidden(e.m.update_user(who, who.user_id, Role::Admin, false)), "self-promotion");
        assert!(forbidden(e.m.reset_password(who, e.admin.user_id, "hijacked-password")));
        assert!(forbidden(e.m.delete_user(who, e.admin.user_id)));
        assert!(forbidden(e.m.save_server(who, None, server("evil", Role::Viewer))));
        assert!(forbidden(e.m.save_server(who, Some(id), server("db-01", Role::Viewer))));
        assert!(forbidden(e.m.delete_server(who, id)));
        assert!(forbidden(e.m.reset_host_key(who, id)));
        assert!(forbidden(e.m.create_group(who, "evil")));
        assert!(forbidden(e.m.rename_group(who, group.id, "evil")));
        assert!(forbidden(e.m.delete_group(who, group.id)));
        assert!(forbidden(e.m.audit_log(who, 10)));
        // Read-only inventory is fine for everyone.
        assert!(e.m.list_servers(who).is_ok() && e.m.list_groups(who).is_ok());
    }

    // Viewers can never connect anywhere.
    assert!(forbidden(e.m.run_command(&viewer, id, "id")));
    assert!(forbidden(e.m.list_containers(&viewer, id)));
}

#[test]
fn per_server_required_role_checked_before_connecting() {
    let e = env("required");
    let id = e.m.save_server(&e.admin, None, server("vault-host", Role::Admin)).unwrap();
    let operator = user(&e, "otto", Role::Operator);
    assert!(matches!(e.m.run_command(&operator, id, "id"), Err(Error::Forbidden(Role::Admin))));
    assert!(matches!(e.m.list_containers(&operator, id), Err(Error::Forbidden(Role::Admin))));

    // The floor is operator: a server can't be opened up to viewers.
    let id = e.m.save_server(&e.admin, None, server("open-host", Role::Viewer)).unwrap();
    assert_eq!(e.m.get_server(&e.admin, id).unwrap().required_role, Role::Operator);
}

#[test]
fn identities_are_revalidated_on_every_call() {
    let e = env("revalidate");
    let id = e.m.save_server(&e.admin, None, server("db-01", Role::Operator)).unwrap();
    let operator = user(&e, "otto", Role::Operator);

    // A forged identity claiming admin gets the role of the real account, not its own.
    let forged = Identity { role: Role::Admin, ..operator.clone() };
    assert!(forbidden(e.m.list_users(&forged)));
    let ghost = Identity { user_id: 9999, username: "ghost".into(), role: Role::Admin };
    assert!(matches!(e.m.list_servers(&ghost), Err(Error::InvalidCredentials)));

    // Demotion applies immediately to a logged-in session.
    e.m.update_user(&e.admin, operator.user_id, Role::Viewer, false).unwrap();
    assert!(forbidden(e.m.run_command(&operator, id, "id")));

    // Disabling and deleting lock the session out entirely.
    e.m.update_user(&e.admin, operator.user_id, Role::Viewer, true).unwrap();
    assert!(matches!(e.m.list_servers(&operator), Err(Error::Disabled)));
    e.m.delete_user(&e.admin, operator.user_id).unwrap();
    assert!(matches!(e.m.list_servers(&operator), Err(Error::InvalidCredentials)));
}

#[test]
fn admin_cannot_lock_everyone_out() {
    let e = env("lockout");
    assert!(e.m.update_user(&e.admin, e.admin.user_id, Role::Operator, false).is_err());
    assert!(e.m.update_user(&e.admin, e.admin.user_id, Role::Admin, true).is_err());
    assert!(e.m.delete_user(&e.admin, e.admin.user_id).is_err());
    assert!(e.m.login("admin", ADMIN_PW).is_ok());
}

#[test]
fn hostile_names_are_stored_verbatim() {
    let e = env("hostile");
    let names = ["'; DROP TABLE servers;--", "\" OR 1=1 --", "$(reboot)", "`id`", "a\\b%_*"];
    for (i, name) in names.iter().enumerate() {
        e.m.create_group(&e.admin, &format!("g{i} {name}")).unwrap();
        let mut input = server(name, Role::Operator);
        input.group = format!("g{i} {name}");
        let id = e.m.save_server(&e.admin, None, input).unwrap();
        assert_eq!(e.m.get_server(&e.admin, id).unwrap().name, *name);
    }
    assert_eq!(e.m.list_servers(&e.admin).unwrap().len(), names.len());
    assert_eq!(e.m.list_groups(&e.admin).unwrap().len(), names.len());
    assert!(e.m.login("admin", ADMIN_PW).is_ok(), "users table intact");
}
