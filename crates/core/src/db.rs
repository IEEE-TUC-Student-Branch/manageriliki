//! SQLite persistence. Functions here do no permission checks; see `Manager`.

use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::{Error, Result};
use crate::models::{AuditEntry, Role, SerialConfig, Server, SshConfig, User};

const MIGRATIONS: &[&str] = &[r#"
    CREATE TABLE users (
        id            INTEGER PRIMARY KEY,
        username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
        password_hash TEXT NOT NULL,
        role          TEXT NOT NULL,
        disabled      INTEGER NOT NULL DEFAULT 0,
        created_at    TEXT NOT NULL,
        last_login    TEXT
    );
    CREATE TABLE servers (
        id            INTEGER PRIMARY KEY,
        name          TEXT NOT NULL UNIQUE COLLATE NOCASE,
        grp           TEXT NOT NULL DEFAULT '',
        notes         TEXT NOT NULL DEFAULT '',
        ssh           TEXT,
        serial        TEXT,
        required_role TEXT NOT NULL,
        secret        BLOB,
        host_key_fp   TEXT
    );
    CREATE TABLE audit (
        id       INTEGER PRIMARY KEY,
        ts       TEXT NOT NULL,
        username TEXT NOT NULL,
        action   TEXT NOT NULL,
        target   TEXT NOT NULL,
        detail   TEXT NOT NULL
    );
"#];

pub fn open(path: &Path) -> Result<Connection> {
    let mut conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrate(&mut conn)?;
    Ok(conn)
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let version = version as usize;
    if version > MIGRATIONS.len() {
        return Err(Error::Invalid(format!(
            "database schema v{version} is newer than this program supports"
        )));
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

fn role_col(row: &Row, idx: &str) -> rusqlite::Result<Role> {
    let s: String = row.get(idx)?;
    Ok(Role::parse(&s).unwrap_or(Role::Viewer))
}

fn json_col<T: serde::de::DeserializeOwned>(row: &Row, idx: &str) -> rusqlite::Result<Option<T>> {
    let s: Option<String> = row.get(idx)?;
    Ok(s.and_then(|s| serde_json::from_str(&s).ok()))
}

fn to_json<T: serde::Serialize>(v: &Option<T>) -> Option<String> {
    v.as_ref().map(|v| serde_json::to_string(v).expect("config serializes"))
}

// ---- users ----

const USER_COLS: &str = "id, username, role, disabled, created_at, last_login";

fn user_from_row(row: &Row) -> rusqlite::Result<User> {
    Ok(User {
        id: row.get("id")?,
        username: row.get("username")?,
        role: role_col(row, "role")?,
        disabled: row.get("disabled")?,
        created_at: row.get("created_at")?,
        last_login: row.get("last_login")?,
    })
}

pub fn count_users(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)
}

pub fn count_active_admins(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM users WHERE role = 'admin' AND disabled = 0",
        [],
        |r| r.get(0),
    )?)
}

pub fn list_users(conn: &Connection) -> Result<Vec<User>> {
    let mut stmt = conn.prepare(&format!("SELECT {USER_COLS} FROM users ORDER BY username"))?;
    let users = stmt.query_map([], user_from_row)?.collect::<rusqlite::Result<_>>()?;
    Ok(users)
}

pub fn get_user(conn: &Connection, id: i64) -> Result<User> {
    conn.query_row(
        &format!("SELECT {USER_COLS} FROM users WHERE id = ?1"),
        [id],
        user_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("user #{id}")))
}

/// Returns the user and their password hash.
pub fn find_user_for_login(conn: &Connection, username: &str) -> Result<Option<(User, String)>> {
    Ok(conn
        .query_row(
            &format!("SELECT {USER_COLS}, password_hash FROM users WHERE username = ?1"),
            [username],
            |r| Ok((user_from_row(r)?, r.get("password_hash")?)),
        )
        .optional()?)
}

pub fn password_hash(conn: &Connection, id: i64) -> Result<String> {
    Ok(conn.query_row("SELECT password_hash FROM users WHERE id = ?1", [id], |r| {
        r.get(0)
    })?)
}

pub fn insert_user(conn: &Connection, username: &str, hash: &str, role: Role) -> Result<i64> {
    conn.execute(
        "INSERT INTO users (username, password_hash, role, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![username, hash, role.as_str(), Utc::now()],
    )
    .map_err(|e| unique_violation(e, format!("user '{username}' already exists")))?;
    Ok(conn.last_insert_rowid())
}

pub fn update_user(conn: &Connection, id: i64, role: Role, disabled: bool) -> Result<()> {
    conn.execute(
        "UPDATE users SET role = ?2, disabled = ?3 WHERE id = ?1",
        params![id, role.as_str(), disabled],
    )?;
    Ok(())
}

pub fn set_password_hash(conn: &Connection, id: i64, hash: &str) -> Result<()> {
    conn.execute(
        "UPDATE users SET password_hash = ?2 WHERE id = ?1",
        params![id, hash],
    )?;
    Ok(())
}

pub fn touch_last_login(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE users SET last_login = ?2 WHERE id = ?1",
        params![id, Utc::now()],
    )?;
    Ok(())
}

pub fn delete_user(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM users WHERE id = ?1", [id])?;
    Ok(())
}

// ---- servers ----

const SERVER_COLS: &str =
    "id, name, grp, notes, ssh, serial, required_role, secret IS NOT NULL AS has_secret, host_key_fp";

fn server_from_row(row: &Row) -> rusqlite::Result<Server> {
    Ok(Server {
        id: row.get("id")?,
        name: row.get("name")?,
        group: row.get("grp")?,
        notes: row.get("notes")?,
        ssh: json_col::<SshConfig>(row, "ssh")?,
        serial: json_col::<SerialConfig>(row, "serial")?,
        required_role: role_col(row, "required_role")?,
        has_secret: row.get("has_secret")?,
        host_key_fp: row.get("host_key_fp")?,
    })
}

pub fn list_servers(conn: &Connection) -> Result<Vec<Server>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SERVER_COLS} FROM servers ORDER BY grp, name"
    ))?;
    let servers = stmt.query_map([], server_from_row)?.collect::<rusqlite::Result<_>>()?;
    Ok(servers)
}

pub fn get_server(conn: &Connection, id: i64) -> Result<Server> {
    conn.query_row(
        &format!("SELECT {SERVER_COLS} FROM servers WHERE id = ?1"),
        [id],
        server_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("server #{id}")))
}

pub struct ServerRow<'a> {
    pub name: &'a str,
    pub group: &'a str,
    pub notes: &'a str,
    pub ssh: &'a Option<SshConfig>,
    pub serial: &'a Option<SerialConfig>,
    pub required_role: Role,
}

pub fn insert_server(conn: &Connection, s: &ServerRow) -> Result<i64> {
    conn.execute(
        "INSERT INTO servers (name, grp, notes, ssh, serial, required_role)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            s.name,
            s.group,
            s.notes,
            to_json(s.ssh),
            to_json(s.serial),
            s.required_role.as_str()
        ],
    )
    .map_err(|e| unique_violation(e, format!("a server named '{}' already exists", s.name)))?;
    Ok(conn.last_insert_rowid())
}

pub fn update_server(conn: &Connection, id: i64, s: &ServerRow) -> Result<()> {
    conn.execute(
        "UPDATE servers SET name = ?2, grp = ?3, notes = ?4, ssh = ?5, serial = ?6,
         required_role = ?7 WHERE id = ?1",
        params![
            id,
            s.name,
            s.group,
            s.notes,
            to_json(s.ssh),
            to_json(s.serial),
            s.required_role.as_str()
        ],
    )
    .map_err(|e| unique_violation(e, format!("a server named '{}' already exists", s.name)))?;
    Ok(())
}

pub fn get_secret(conn: &Connection, id: i64) -> Result<Option<Vec<u8>>> {
    Ok(conn.query_row("SELECT secret FROM servers WHERE id = ?1", [id], |r| {
        r.get(0)
    })?)
}

pub fn set_secret(conn: &Connection, id: i64, secret: Option<&[u8]>) -> Result<()> {
    conn.execute(
        "UPDATE servers SET secret = ?2 WHERE id = ?1",
        params![id, secret],
    )?;
    Ok(())
}

pub fn set_host_key(conn: &Connection, id: i64, fp: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE servers SET host_key_fp = ?2 WHERE id = ?1",
        params![id, fp],
    )?;
    Ok(())
}

pub fn delete_server(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM servers WHERE id = ?1", [id])?;
    Ok(())
}

// ---- audit ----

pub fn audit(conn: &Connection, username: &str, action: &str, target: &str, detail: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO audit (ts, username, action, target, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![Utc::now(), username, action, target, detail],
    )?;
    Ok(())
}

pub fn list_audit(conn: &Connection, limit: usize) -> Result<Vec<AuditEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, ts, username, action, target, detail FROM audit ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit as i64], |r| {
            Ok(AuditEntry {
                id: r.get(0)?,
                ts: r.get::<_, DateTime<Utc>>(1)?,
                username: r.get(2)?,
                action: r.get(3)?,
                target: r.get(4)?,
                detail: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn unique_violation(e: rusqlite::Error, msg: String) -> Error {
    match e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
        {
            Error::Invalid(msg)
        }
        e => Error::Db(e),
    }
}
