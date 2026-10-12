//! An interactive session whose output is fed through a VT100 emulator, so
//! any frontend can render the resulting screen grid.

use std::sync::mpsc::{self, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use crate::error::Result;
use crate::transport::{Link, ReadOutcome};

const SCROLLBACK: usize = 2000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalState {
    Connecting,
    Connected,
    Closed(Option<String>),
}

enum Input {
    Data(Vec<u8>),
    Resize(u16, u16),
}

pub type Notify = Arc<dyn Fn() + Send + Sync>;

/// Establishes the link; runs on the session's worker thread.
pub(crate) type Connector = Box<dyn FnOnce() -> Result<Box<dyn Link>> + Send>;

/// Runs once when the session ends, with the error that ended it, if any.
pub(crate) type OnClose = Box<dyn FnOnce(Option<&str>) + Send>;

pub struct Terminal {
    pub title: String,
    parser: Arc<Mutex<vt100::Parser>>,
    state: Arc<Mutex<TerminalState>>,
    tx: Option<Sender<Input>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Terminal {
    /// Spawns a worker that runs `connect` and then pumps data until the link
    /// closes or the terminal is dropped. `on_close` runs exactly once.
    pub(crate) fn spawn(
        title: String,
        rows: u16,
        cols: u16,
        notify: Notify,
        connect: Connector,
        on_close: OnClose,
    ) -> Terminal {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));
        let state = Arc::new(Mutex::new(TerminalState::Connecting));
        let (tx, rx) = mpsc::channel::<Input>();

        let (w_parser, w_state) = (parser.clone(), state.clone());
        thread::spawn(move || {
            let result = (|| -> std::result::Result<(), String> {
                let mut link = connect().map_err(|e| e.to_string())?;
                *lock(&w_state) = TerminalState::Connected;
                notify();
                let (r, c) = lock(&w_parser).screen().size();
                link.resize(r, c).map_err(|e| e.to_string())?;

                let mut buf = vec![0u8; 16 * 1024];
                loop {
                    let mut busy = false;
                    match link.read(&mut buf).map_err(|e| e.to_string())? {
                        ReadOutcome::Data(n) => {
                            lock(&w_parser).process(&buf[..n]);
                            notify();
                            busy = true;
                        }
                        ReadOutcome::Eof => return Ok(()),
                        ReadOutcome::Idle => {}
                    }
                    // Wait briefly for input when idle so keystrokes stay snappy.
                    let first = if busy {
                        rx.try_recv().map_err(|e| e == TryRecvError::Disconnected)
                    } else {
                        rx.recv_timeout(Duration::from_millis(15))
                            .map_err(|e| e == RecvTimeoutError::Disconnected)
                    };
                    let mut next = first;
                    loop {
                        match next {
                            Ok(Input::Data(d)) => link.write_all(&d).map_err(|e| e.to_string())?,
                            Ok(Input::Resize(r, c)) => link.resize(r, c).map_err(|e| e.to_string())?,
                            Err(true) => return Ok(()), // terminal dropped
                            Err(false) => break,
                        }
                        next = rx.try_recv().map_err(|e| e == TryRecvError::Disconnected);
                    }
                }
            })();
            let err = result.err();
            let note = match &err {
                Some(e) => format!("\r\n\x1b[1;31m[connection error] {e}\x1b[0m\r\n"),
                None => "\r\n\x1b[2m[session closed]\x1b[0m\r\n".to_string(),
            };
            lock(&w_parser).process(note.as_bytes());
            on_close(err.as_deref());
            *lock(&w_state) = TerminalState::Closed(err);
            notify();
        });

        Terminal {
            title,
            parser,
            state,
            tx: Some(tx),
        }
    }

    pub fn state(&self) -> TerminalState {
        lock(&self.state).clone()
    }

    pub fn is_closed(&self) -> bool {
        matches!(self.state(), TerminalState::Closed(_))
    }

    pub fn write(&self, data: impl Into<Vec<u8>>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Input::Data(data.into()));
        }
    }

    /// Resizes the emulated screen and the remote PTY (no-op if unchanged).
    pub fn resize(&self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(10));
        let mut parser = lock(&self.parser);
        if parser.screen().size() == (rows, cols) {
            return;
        }
        parser.screen_mut().set_size(rows, cols);
        drop(parser);
        if let Some(tx) = &self.tx {
            let _ = tx.send(Input::Resize(rows, cols));
        }
    }

    /// Scrolls the view back by `delta` lines (negative scrolls forward).
    pub fn scroll(&self, delta: isize) {
        let mut parser = lock(&self.parser);
        let current = parser.screen().scrollback() as isize;
        parser
            .screen_mut()
            .set_scrollback((current + delta).max(0) as usize);
    }

    pub fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> R {
        f(lock(&self.parser).screen())
    }

    /// Ends the session; the worker exits on its next poll.
    pub fn close(&mut self) {
        self.tx = None;
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.close();
    }
}
