//! Core library for manageriliki: storage, accounts, credentials and the
//! SSH / serial transports, container runtimes shared by the TUI and GUI frontends.

#![forbid(unsafe_code)]

mod auth;
mod containers;
mod db;
mod error;
mod manager;
mod models;
mod probe;
mod terminal;
mod transport;
mod vault;
mod vnc;

pub use containers::{Container, ContainerAction, ContainerList, ContainerService, Runtime, ServiceKind};
pub use error::{Error, Result};
pub use manager::{CommandOutput, Identity, Manager};
pub use models::*;
pub use probe::{Status, StatusMonitor, probe};
pub use terminal::{Notify, Terminal, TerminalState};
pub use transport::serial_ports;
pub use vnc::{Framebuffer, VncSession, VncState, keysym};
pub use vt100;
