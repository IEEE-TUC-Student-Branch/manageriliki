//! Byte-stream links to servers over SSH or a serial line.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use base64::Engine;
use ssh2::{Channel, HashType, Session};

use crate::error::{Error, Result, invalid};
use crate::models::{FlowControl, Parity, SerialConfig, SshAuth, SshConfig};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

pub enum ReadOutcome {
    Data(usize),
    Idle,
    Eof,
}

/// A bidirectional, non-blocking link carrying terminal traffic.
pub trait Link: Send {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<ReadOutcome>;
    fn write_all(&mut self, data: &[u8]) -> io::Result<()>;
    fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()>;
}

// ---- SSH ----

/// An authenticated SSH session whose host key has been verified.
pub struct SshConnection {
    pub session: Session,
}

/// Connects and authenticates. `check_host_key` is called with the server's
/// fingerprint before any credentials are sent; returning an error aborts.
pub fn ssh_connect(
    cfg: &SshConfig,
    secret: Option<&str>,
    check_host_key: impl FnOnce(&str) -> Result<()>,
) -> Result<SshConnection> {
    let addr = (cfg.host.as_str(), cfg.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| invalid(format!("cannot resolve {}", cfg.host)))?;
    let tcp = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;

    let mut session = Session::new()?;
    session.set_tcp_stream(tcp);
    session.set_timeout(15_000);
    session.handshake()?;

    let hash = session
        .host_key_hash(HashType::Sha256)
        .ok_or_else(|| invalid("server did not present a host key"))?;
    let fingerprint = format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(hash)
    );
    check_host_key(&fingerprint)?;

    match &cfg.auth {
        SshAuth::Agent => session.userauth_agent(&cfg.username)?,
        SshAuth::Password => {
            let password = secret.ok_or_else(|| invalid("no password is stored for this server"))?;
            session.userauth_password(&cfg.username, password)?
        }
        SshAuth::Key { path } => {
            session.userauth_pubkey_file(&cfg.username, None, &expand_tilde(path), secret)?
        }
    }
    if !session.authenticated() {
        return Err(invalid("SSH authentication failed"));
    }
    Ok(SshConnection { session })
}

fn expand_tilde(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => directories::BaseDirs::new()
            .map(|d| d.home_dir().join(rest))
            .unwrap_or_else(|| PathBuf::from(path)),
        None => PathBuf::from(path),
    }
}

pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_status: i32,
}

pub fn ssh_exec(conn: &SshConnection, command: &str) -> Result<ExecResult> {
    let mut channel = conn.session.channel_session()?;
    channel.exec(command)?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    channel.read_to_end(&mut stdout)?;
    channel.stderr().read_to_end(&mut stderr)?;
    channel.wait_close()?;
    Ok(ExecResult {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        exit_status: channel.exit_status()?,
    })
}

pub struct SshLink {
    session: Session,
    channel: Channel,
}

impl SshLink {
    /// Opens an interactive shell with a PTY of the given size.
    pub fn open_shell(conn: SshConnection, rows: u16, cols: u16) -> Result<Self> {
        let session = conn.session;
        let mut channel = session.channel_session()?;
        channel.request_pty("xterm-256color", None, Some((cols as u32, rows as u32, 0, 0)))?;
        channel.shell()?;
        session.set_blocking(false);
        Ok(SshLink { session, channel })
    }
}

/// Retries a libssh2 call that reports EAGAIN on a non-blocking session.
fn again<T>(mut f: impl FnMut() -> std::result::Result<T, ssh2::Error>) -> io::Result<T> {
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                let e = io::Error::from(e);
                if e.kind() != io::ErrorKind::WouldBlock {
                    return Err(e);
                }
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

impl Link for SshLink {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<ReadOutcome> {
        match self.channel.read(buf) {
            Ok(0) if self.channel.eof() => Ok(ReadOutcome::Eof),
            Ok(0) => Ok(ReadOutcome::Idle),
            Ok(n) => Ok(ReadOutcome::Data(n)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if self.channel.eof() {
                    Ok(ReadOutcome::Eof)
                } else {
                    Ok(ReadOutcome::Idle)
                }
            }
            Err(e) => Err(e),
        }
    }

    fn write_all(&mut self, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            match self.channel.write(data) {
                Ok(n) => data = &data[n..],
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2))
                }
                Err(e) => return Err(e),
            }
        }
        // Deliberately no flush(): libssh2's channel flush discards buffered
        // *incoming* data, and writes are sent immediately anyway.
        Ok(())
    }

    fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        again(|| {
            self.channel
                .request_pty_size(cols as u32, rows as u32, None, None)
        })
    }
}

impl Drop for SshLink {
    fn drop(&mut self) {
        self.session.set_blocking(true);
        let _ = self.channel.close();
    }
}

// ---- serial ----

pub struct SerialLink {
    port: Box<dyn serialport::SerialPort>,
}

impl SerialLink {
    pub fn open(cfg: &SerialConfig) -> Result<Self> {
        let data_bits = match cfg.data_bits {
            5 => serialport::DataBits::Five,
            6 => serialport::DataBits::Six,
            7 => serialport::DataBits::Seven,
            8 => serialport::DataBits::Eight,
            n => return Err(invalid(format!("unsupported data bits: {n}"))),
        };
        let stop_bits = match cfg.stop_bits {
            1 => serialport::StopBits::One,
            2 => serialport::StopBits::Two,
            n => return Err(invalid(format!("unsupported stop bits: {n}"))),
        };
        let parity = match cfg.parity {
            Parity::None => serialport::Parity::None,
            Parity::Odd => serialport::Parity::Odd,
            Parity::Even => serialport::Parity::Even,
        };
        let flow = match cfg.flow_control {
            FlowControl::None => serialport::FlowControl::None,
            FlowControl::Software => serialport::FlowControl::Software,
            FlowControl::Hardware => serialport::FlowControl::Hardware,
        };
        let port = serialport::new(&cfg.device, cfg.baud)
            .data_bits(data_bits)
            .stop_bits(stop_bits)
            .parity(parity)
            .flow_control(flow)
            .timeout(Duration::from_millis(10))
            .open()
            .map_err(|e| match e.kind() {
                serialport::ErrorKind::Io(io::ErrorKind::PermissionDenied) => invalid(format!(
                    "permission denied opening {} (is your user in the 'uucp' or 'dialout' group?)",
                    cfg.device
                )),
                _ => Error::Serial(e),
            })?;
        Ok(SerialLink { port })
    }
}

impl Link for SerialLink {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<ReadOutcome> {
        match self.port.read(buf) {
            Ok(0) => Ok(ReadOutcome::Idle),
            Ok(n) => Ok(ReadOutcome::Data(n)),
            Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => {
                Ok(ReadOutcome::Idle)
            }
            Err(e) => Err(e),
        }
    }

    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.port.write_all(data)?;
        self.port.flush()
    }

    fn resize(&mut self, _rows: u16, _cols: u16) -> io::Result<()> {
        Ok(())
    }
}

/// Serial devices detected on this machine (e.g. /dev/ttyUSB0).
pub fn serial_ports() -> io::Result<Vec<String>> {
    serialport::available_ports()
        .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
        .map_err(io::Error::other)
}
