use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Account roles, ordered by privilege.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Role {
    /// Can see the inventory and server status.
    Viewer,
    /// Can additionally open sessions and run commands.
    Operator,
    /// Can additionally manage servers, users and read the audit log.
    Admin,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Viewer, Role::Operator, Role::Admin];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Operator => "operator",
            Role::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub role: Role,
    pub disabled: bool,
    pub created_at: DateTime<Utc>,
    pub last_login: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum SshAuth {
    /// Use keys loaded in the running ssh-agent.
    Agent,
    /// Password stored (encrypted) in the vault.
    Password,
    /// Private key file; an optional passphrase is stored in the vault.
    Key { path: String },
}

impl SshAuth {
    pub fn label(&self) -> &'static str {
        match self {
            SshAuth::Agent => "agent",
            SshAuth::Password => "password",
            SshAuth::Key { .. } => "key",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SshAuth,
}

impl Default for SshConfig {
    fn default() -> Self {
        SshConfig {
            host: String::new(),
            port: 22,
            username: "root".into(),
            auth: SshAuth::Agent,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Parity {
    None,
    Odd,
    Even,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowControl {
    None,
    Software,
    Hardware,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerialConfig {
    pub device: String,
    pub baud: u32,
    pub data_bits: u8,
    pub parity: Parity,
    pub stop_bits: u8,
    pub flow_control: FlowControl,
}

impl SerialConfig {
    /// Short form such as `/dev/ttyUSB0 9600 8N1`.
    pub fn summary(&self) -> String {
        let parity = match self.parity {
            Parity::None => 'N',
            Parity::Odd => 'O',
            Parity::Even => 'E',
        };
        format!(
            "{} {} {}{}{}",
            self.device, self.baud, self.data_bits, parity, self.stop_bits
        )
    }
}

impl Default for SerialConfig {
    fn default() -> Self {
        SerialConfig {
            device: "/dev/ttyUSB0".into(),
            baud: 9600,
            data_bits: 8,
            parity: Parity::None,
            stop_bits: 1,
            flow_control: FlowControl::None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Server {
    pub id: i64,
    pub name: String,
    pub group: String,
    pub notes: String,
    pub ssh: Option<SshConfig>,
    pub serial: Option<SerialConfig>,
    /// Minimum role needed to connect to this server (never below operator).
    pub required_role: Role,
    /// Whether a password / key passphrase is stored for this server.
    pub has_secret: bool,
    /// Trusted SSH host key fingerprint, recorded on first connection.
    pub host_key_fp: Option<String>,
}

/// What to do with a server's stored secret when saving it.
#[derive(Clone, Debug, Default)]
pub enum SecretUpdate {
    #[default]
    Keep,
    Clear,
    Set(String),
}

#[derive(Clone, Debug)]
pub struct ServerInput {
    pub name: String,
    pub group: String,
    pub notes: String,
    pub ssh: Option<SshConfig>,
    pub serial: Option<SerialConfig>,
    pub required_role: Role,
    pub secret: SecretUpdate,
}

impl ServerInput {
    pub fn from_server(s: &Server) -> Self {
        ServerInput {
            name: s.name.clone(),
            group: s.group.clone(),
            notes: s.notes.clone(),
            ssh: s.ssh.clone(),
            serial: s.serial.clone(),
            required_role: s.required_role,
            secret: SecretUpdate::Keep,
        }
    }
}

impl Default for ServerInput {
    fn default() -> Self {
        ServerInput {
            name: String::new(),
            group: String::new(),
            notes: String::new(),
            ssh: Some(SshConfig::default()),
            serial: None,
            required_role: Role::Operator,
            secret: SecretUpdate::Keep,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportKind {
    Ssh,
    Serial,
}

impl fmt::Display for TransportKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TransportKind::Ssh => "ssh",
            TransportKind::Serial => "serial",
        })
    }
}

#[derive(Clone, Debug)]
pub struct AuditEntry {
    pub id: i64,
    pub ts: DateTime<Utc>,
    pub username: String,
    pub action: String,
    pub target: String,
    pub detail: String,
}
