use crate::models::Role;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SSH error: {0}")]
    Ssh(#[from] ssh2::Error),
    #[error("serial port error: {0}")]
    Serial(#[from] serialport::Error),
    #[error("invalid username or password")]
    InvalidCredentials,
    #[error("permission denied: requires the {0} role")]
    Forbidden(Role),
    #[error("account is disabled")]
    Disabled,
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("credential vault error: {0}")]
    Vault(String),
    #[error(
        "host key mismatch for {server}: expected {expected}, got {actual}. \
         If the server was reinstalled, an admin can reset its stored host key."
    )]
    HostKeyMismatch {
        server: String,
        expected: String,
        actual: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn invalid(msg: impl Into<String>) -> Error {
    Error::Invalid(msg.into())
}
