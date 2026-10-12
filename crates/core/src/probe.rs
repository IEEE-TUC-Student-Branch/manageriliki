use std::collections::HashMap;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::models::Server;
use crate::terminal::Notify;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Unknown,
    /// Reachable; the value is the TCP connect latency in milliseconds (0 for serial).
    Online(u128),
    Offline(String),
}

/// Checks reachability: a TCP connect to the SSH port, or the serial device's
/// presence when the server has no SSH configuration.
pub fn probe(server: &Server, timeout: Duration) -> Status {
    if let Some(ssh) = &server.ssh {
        let addr = match (ssh.host.as_str(), ssh.port).to_socket_addrs() {
            Ok(mut addrs) => match addrs.next() {
                Some(a) => a,
                None => return Status::Offline("no address".into()),
            },
            Err(e) => return Status::Offline(e.to_string()),
        };
        let start = Instant::now();
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(_) => Status::Online(start.elapsed().as_millis()),
            Err(e) => Status::Offline(e.to_string()),
        }
    } else if let Some(serial) = &server.serial {
        if Path::new(&serial.device).exists() {
            Status::Online(0)
        } else {
            Status::Offline(format!("{} not present", serial.device))
        }
    } else {
        Status::Unknown
    }
}

/// Periodically probes a set of servers in the background.
pub struct StatusMonitor {
    shared: Arc<Shared>,
}

struct Shared {
    servers: Mutex<Vec<Server>>,
    statuses: Mutex<HashMap<i64, Status>>,
    /// Set to request an immediate probe round; cleared by the worker.
    wake: Mutex<bool>,
    cv: Condvar,
    stop: AtomicBool,
}

impl StatusMonitor {
    pub fn start(interval: Duration, notify: Notify) -> Self {
        let shared = Arc::new(Shared {
            servers: Mutex::new(Vec::new()),
            statuses: Mutex::new(HashMap::new()),
            wake: Mutex::new(false),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let worker = shared.clone();
        thread::spawn(move || {
            loop {
                {
                    let wake = worker.wake.lock().unwrap_or_else(|e| e.into_inner());
                    let (mut wake, _) = worker
                        .cv
                        .wait_timeout_while(wake, interval, |w| !*w)
                        .unwrap_or_else(|e| e.into_inner());
                    *wake = false;
                }
                if worker.stop.load(Ordering::Relaxed) {
                    return;
                }
                let servers = worker.servers.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let results: Vec<(i64, Status)> = thread::scope(|s| {
                    let handles: Vec<_> = servers
                        .iter()
                        .map(|srv| s.spawn(move || (srv.id, probe(srv, Duration::from_millis(1500)))))
                        .collect();
                    handles.into_iter().filter_map(|h| h.join().ok()).collect()
                });
                *worker.statuses.lock().unwrap_or_else(|e| e.into_inner()) =
                    results.into_iter().collect();
                notify();
            }
        });
        StatusMonitor { shared }
    }

    /// Replaces the probed set and triggers an immediate round.
    pub fn set_servers(&self, servers: Vec<Server>) {
        *self.shared.servers.lock().unwrap_or_else(|e| e.into_inner()) = servers;
        self.refresh();
    }

    pub fn refresh(&self) {
        *self.shared.wake.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.shared.cv.notify_all();
    }

    pub fn get(&self, id: i64) -> Status {
        self.shared
            .statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned()
            .unwrap_or(Status::Unknown)
    }
}

impl Drop for StatusMonitor {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.refresh();
    }
}
