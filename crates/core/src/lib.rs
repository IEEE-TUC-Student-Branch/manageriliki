//! Core library for manageriliki: storage, accounts, credentials and the
//! SSH / serial transports shared by the TUI and GUI frontends.

mod auth;
mod db;
mod error;
mod manager;
mod models;
mod probe;
mod terminal;
mod transport;
mod vault;

pub use error::{Error, Result};
pub use manager::{CommandOutput, Identity, Manager};
pub use models::*;
pub use probe::{Status, StatusMonitor, probe};
pub use terminal::{Notify, Terminal, TerminalState};
pub use transport::serial_ports;
pub use vt100;
