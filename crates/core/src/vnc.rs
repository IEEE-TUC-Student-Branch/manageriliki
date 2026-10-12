//! A VNC (RFB 3.3/3.7/3.8) client: None and VNC-password authentication,
//! Raw, CopyRect and ZRLE encodings, and desktop resizing.
//!
//! The protocol runs on a worker thread and decodes into a shared RGBA
//! framebuffer that frontends upload as a texture.

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use des::Des;
use des::cipher::{BlockCipherEncrypt, KeyInit};
use flate2::{Decompress, FlushDecompress};

use crate::terminal::{Notify, OnClose};
use crate::transport::{self, Link};

const ENC_RAW: i32 = 0;
const ENC_COPYRECT: i32 = 1;
const ENC_ZRLE: i32 = 16;
const ENC_DESKTOP_SIZE: i32 = -223;

pub(crate) type VncConnector = Box<dyn FnOnce() -> crate::Result<Box<dyn Link>> + Send>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VncState {
    Connecting,
    Connected,
    Closed(Option<String>),
}

/// RGBA pixels plus the region changed since the frontend last looked.
pub struct Framebuffer {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
    /// Bumped on every completed update.
    pub generation: u64,
    /// Bumped when the size changes (the frontend must re-upload fully).
    pub size_generation: u64,
    dirty: Option<(usize, usize, usize, usize)>,
}

impl Framebuffer {
    fn new(width: usize, height: usize) -> Self {
        Framebuffer {
            width,
            height,
            pixels: vec![0; width * height * 4],
            generation: 0,
            size_generation: 0,
            dirty: None,
        }
    }

    fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.pixels = vec![0; width * height * 4];
        self.size_generation += 1;
        self.mark(0, 0, width, height);
    }

    fn mark(&mut self, x: usize, y: usize, w: usize, h: usize) {
        self.dirty = Some(match self.dirty {
            None => (x, y, w, h),
            Some((dx, dy, dw, dh)) => {
                let (x0, y0) = (dx.min(x), dy.min(y));
                let (x1, y1) = ((dx + dw).max(x + w), (dy + dh).max(y + h));
                (x0, y0, x1 - x0, y1 - y0)
            }
        });
    }

    /// Returns and clears the changed region `(x, y, w, h)`.
    pub fn take_dirty(&mut self) -> Option<(usize, usize, usize, usize)> {
        self.dirty.take()
    }

    fn check_rect(&self, x: usize, y: usize, w: usize, h: usize) -> io::Result<()> {
        if x + w > self.width || y + h > self.height {
            return Err(proto_err(format!(
                "rectangle {w}x{h}+{x}+{y} is outside the {}x{} framebuffer",
                self.width, self.height
            )));
        }
        Ok(())
    }

    #[inline]
    fn put(&mut self, x: usize, y: usize, [r, g, b]: [u8; 3]) {
        let i = (y * self.width + x) * 4;
        self.pixels[i..i + 4].copy_from_slice(&[r, g, b, 255]);
    }
}

struct Shared {
    state: Mutex<VncState>,
    fb: Mutex<Framebuffer>,
    name: Mutex<String>,
    clipboard: Mutex<Option<String>>,
    stop: Arc<AtomicBool>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A live VNC connection. Dropping it disconnects.
pub struct VncSession {
    pub title: String,
    shared: Arc<Shared>,
    out: Sender<Vec<u8>>,
}

impl VncSession {
    pub(crate) fn spawn(
        title: String,
        password: Option<String>,
        notify: Notify,
        connect: VncConnector,
        on_close: OnClose,
    ) -> VncSession {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared {
            state: Mutex::new(VncState::Connecting),
            fb: Mutex::new(Framebuffer::new(0, 0)),
            name: Mutex::new(String::new()),
            clipboard: Mutex::new(None),
            stop: stop.clone(),
        });
        let (out, out_rx) = mpsc::channel::<Vec<u8>>();
        let (w_shared, w_out) = (shared.clone(), out.clone());
        thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let link = connect().map_err(|e| e.to_string())?;
                let mut reader = transport::pump(link, out_rx, stop);
                run(&mut reader, &w_out, password.as_deref(), &w_shared, &notify)
                    .map_err(|e| match e.kind() {
                        io::ErrorKind::UnexpectedEof => "the VNC server closed the connection".to_string(),
                        _ => e.to_string(),
                    })
            })();
            // A deliberate disconnect also ends with EOF; don't report it as an error.
            let err = result.err().filter(|_| !w_shared.stop.load(Ordering::Relaxed));
            on_close(err.as_deref());
            *lock(&w_shared.state) = VncState::Closed(err);
            notify();
        });
        VncSession { title, shared, out }
    }

    pub fn state(&self) -> VncState {
        lock(&self.shared.state).clone()
    }

    /// The desktop name the server reported.
    pub fn desktop_name(&self) -> String {
        lock(&self.shared.name).clone()
    }

    pub fn with_framebuffer<R>(&self, f: impl FnOnce(&mut Framebuffer) -> R) -> R {
        f(&mut lock(&self.shared.fb))
    }

    /// Text the remote side put on its clipboard since the last call.
    pub fn take_clipboard(&self) -> Option<String> {
        lock(&self.shared.clipboard).take()
    }

    fn send(&self, msg: Vec<u8>) {
        // Input before the handshake finishes would corrupt it.
        if self.state() == VncState::Connected {
            let _ = self.out.send(msg);
        }
    }

    /// `buttons` is the RFB mask: 1 left, 2 middle, 4 right, 8 wheel up, 16 wheel down.
    pub fn pointer(&self, x: u16, y: u16, buttons: u8) {
        let mut m = vec![5, buttons];
        m.extend_from_slice(&x.to_be_bytes());
        m.extend_from_slice(&y.to_be_bytes());
        self.send(m);
    }

    /// Sends an X11 keysym press or release (see [`keysym`]).
    pub fn key(&self, keysym: u32, down: bool) {
        let mut m = vec![4, down as u8, 0, 0];
        m.extend_from_slice(&keysym.to_be_bytes());
        self.send(m);
    }

    /// Replaces the remote clipboard (RFB only carries Latin-1 text).
    pub fn set_clipboard(&self, text: &str) {
        let latin1: Vec<u8> = text.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect();
        let mut m = vec![6, 0, 0, 0];
        m.extend_from_slice(&(latin1.len() as u32).to_be_bytes());
        m.extend_from_slice(&latin1);
        self.send(m);
    }

    pub fn ctrl_alt_del(&self) {
        for (sym, down) in [
            (keysym::CONTROL_L, true),
            (keysym::ALT_L, true),
            (keysym::DELETE, true),
            (keysym::DELETE, false),
            (keysym::ALT_L, false),
            (keysym::CONTROL_L, false),
        ] {
            self.key(sym, down);
        }
    }
}

impl Drop for VncSession {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

/// X11 keysyms for keys without a printable character.
pub mod keysym {
    pub const BACKSPACE: u32 = 0xff08;
    pub const TAB: u32 = 0xff09;
    pub const RETURN: u32 = 0xff0d;
    pub const ESCAPE: u32 = 0xff1b;
    pub const HOME: u32 = 0xff50;
    pub const LEFT: u32 = 0xff51;
    pub const UP: u32 = 0xff52;
    pub const RIGHT: u32 = 0xff53;
    pub const DOWN: u32 = 0xff54;
    pub const PAGE_UP: u32 = 0xff55;
    pub const PAGE_DOWN: u32 = 0xff56;
    pub const END: u32 = 0xff57;
    pub const INSERT: u32 = 0xff63;
    pub const DELETE: u32 = 0xffff;
    /// F1; F2..F12 follow consecutively.
    pub const F1: u32 = 0xffbe;
    pub const SHIFT_L: u32 = 0xffe1;
    pub const CONTROL_L: u32 = 0xffe3;
    pub const ALT_L: u32 = 0xffe9;
    pub const SUPER_L: u32 = 0xffeb;

    /// The keysym for a printable character.
    pub fn from_char(c: char) -> u32 {
        match c as u32 {
            cp @ 0x20..=0x7e | cp @ 0xa0..=0xff => cp,
            cp => 0x0100_0000 | cp,
        }
    }
}

// ---- protocol ----

fn proto_err(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn read_u8(r: &mut impl Read) -> io::Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_u16(r: &mut impl Read) -> io::Result<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_be_bytes(b))
}

fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

fn read_bytes(r: &mut impl Read, n: usize) -> io::Result<Vec<u8>> {
    let mut v = vec![0u8; n];
    r.read_exact(&mut v)?;
    Ok(v)
}

fn read_string(r: &mut impl Read) -> io::Result<String> {
    let len = read_u32(r)? as usize;
    if len > 1 << 20 {
        return Err(proto_err("server sent an oversized string"));
    }
    // RFB strings are Latin-1. Some servers (e.g. QEMU) include a trailing
    // NUL; drop control characters so names and errors display cleanly.
    Ok(read_bytes(r, len)?.into_iter().map(char::from).filter(|c| !c.is_control()).collect())
}

/// VNC authentication: DES-encrypt the challenge with the password
/// (truncated to 8 bytes, each byte bit-reversed) as the key.
fn vnc_auth_response(password: &str, challenge: &[u8; 16]) -> [u8; 16] {
    let mut key = [0u8; 8];
    for (k, b) in key.iter_mut().zip(password.bytes()) {
        *k = b.reverse_bits();
    }
    let cipher = Des::new_from_slice(&key).expect("DES key is 8 bytes");
    let mut out = *challenge;
    for block in out.as_chunks_mut::<8>().0 {
        cipher.encrypt_block(block.into());
    }
    out
}

fn update_request(incremental: bool, w: u16, h: u16) -> Vec<u8> {
    let mut m = vec![3, incremental as u8, 0, 0, 0, 0];
    m.extend_from_slice(&w.to_be_bytes());
    m.extend_from_slice(&h.to_be_bytes());
    m
}

/// Performs the handshake and returns `(width, height, desktop name)`.
fn handshake(
    r: &mut impl Read,
    w: &Sender<Vec<u8>>,
    password: Option<&str>,
) -> io::Result<(u16, u16, String)> {
    let send = |m: Vec<u8>| w.send(m).map_err(|_| proto_err("connection closed"));

    let version = read_bytes(r, 12)?;
    let text = String::from_utf8_lossy(&version);
    let parsed = text
        .strip_prefix("RFB ")
        .and_then(|v| v.trim_end().split_once('.'))
        .and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)));
    let Some((major, minor)) = parsed else {
        return Err(proto_err(format!("not a VNC server (greeting {:?})", text.trim_end())));
    };
    let minor = match (major, minor) {
        (3, m) if m >= 8 => 8,
        (3, 7) => 7,
        (3, _) => 3,
        (m, _) if m > 3 => 8,
        _ => return Err(proto_err(format!("unsupported RFB version {major}.{minor}"))),
    };
    send(format!("RFB 003.{minor:03}\n").into_bytes())?;

    let has_password = password.is_some_and(|p| !p.is_empty());
    let security = if minor == 3 {
        match read_u32(r)? {
            0 => return Err(proto_err(format!("server refused: {}", read_string(r)?))),
            t => t as u8,
        }
    } else {
        let n = read_u8(r)?;
        if n == 0 {
            return Err(proto_err(format!("server refused: {}", read_string(r)?)));
        }
        let types = read_bytes(r, n as usize)?;
        let chosen = if types.contains(&2) && has_password {
            2
        } else if types.contains(&1) {
            1
        } else if types.contains(&2) {
            return Err(proto_err("the VNC server requires a password; set one in the server's settings"));
        } else {
            return Err(proto_err(format!(
                "unsupported VNC security types {types:?}; only None (1) and VNC password (2) are \
                 supported. Disable TLS/VeNCrypt on the server or use the SSH tunnel option."
            )));
        };
        send(vec![chosen])?;
        chosen
    };

    match security {
        1 => {}
        2 => {
            let Some(password) = password.filter(|p| !p.is_empty()) else {
                return Err(proto_err("the VNC server requires a password; set one in the server's settings"));
            };
            let mut challenge = [0u8; 16];
            r.read_exact(&mut challenge)?;
            send(vnc_auth_response(password, &challenge).to_vec())?;
        }
        t => return Err(proto_err(format!("unsupported VNC security type {t}"))),
    }
    if (security == 2 || minor == 8)
        && read_u32(r)? != 0 {
            let reason = if minor == 8 { read_string(r)? } else { String::new() };
            let reason = reason.trim();
            return Err(proto_err(if reason.is_empty() || reason.eq_ignore_ascii_case("authentication failed") {
                "VNC authentication failed (wrong password?)".to_string()
            } else {
                format!("VNC authentication failed: {reason}")
            }));
        }

    send(vec![1])?; // ClientInit: shared session
    let width = read_u16(r)?;
    let height = read_u16(r)?;
    read_bytes(r, 16)?; // server pixel format; we set our own below
    let name = read_string(r)?;

    // 32 bpp true colour, little-endian 0x00RRGGBB.
    send(vec![0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0])?;
    let encodings = [ENC_ZRLE, ENC_COPYRECT, ENC_RAW, ENC_DESKTOP_SIZE];
    let mut m = vec![2, 0];
    m.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
    for e in encodings {
        m.extend_from_slice(&e.to_be_bytes());
    }
    send(m)?;
    send(update_request(false, width, height))?;
    Ok((width, height, name))
}

fn run(
    r: &mut impl Read,
    w: &Sender<Vec<u8>>,
    password: Option<&str>,
    shared: &Shared,
    notify: &Notify,
) -> io::Result<()> {
    let (width, height, name) = handshake(r, w, password)?;
    lock(&shared.fb).resize(width as usize, height as usize);
    *lock(&shared.name) = name;
    *lock(&shared.state) = VncState::Connected;
    notify();

    let mut zrle = ZrleDecoder::new();
    loop {
        match read_u8(r)? {
            0 => {
                read_u8(r)?;
                let rects = read_u16(r)?;
                for _ in 0..rects {
                    let x = read_u16(r)? as usize;
                    let y = read_u16(r)? as usize;
                    let rw = read_u16(r)? as usize;
                    let rh = read_u16(r)? as usize;
                    let encoding = read_u32(r)? as i32;
                    match encoding {
                        ENC_RAW => {
                            let data = read_bytes(r, rw * rh * 4)?;
                            let mut fb = lock(&shared.fb);
                            fb.check_rect(x, y, rw, rh)?;
                            for (i, px) in data.as_chunks::<4>().0.iter().enumerate() {
                                fb.put(x + i % rw, y + i / rw, [px[2], px[1], px[0]]);
                            }
                            fb.mark(x, y, rw, rh);
                        }
                        ENC_COPYRECT => {
                            let sx = read_u16(r)? as usize;
                            let sy = read_u16(r)? as usize;
                            let mut fb = lock(&shared.fb);
                            fb.check_rect(x, y, rw, rh)?;
                            fb.check_rect(sx, sy, rw, rh)?;
                            let stride = fb.width * 4;
                            let src: Vec<u8> = (0..rh)
                                .flat_map(|row| {
                                    let start = (sy + row) * stride + sx * 4;
                                    fb.pixels[start..start + rw * 4].to_vec()
                                })
                                .collect();
                            for row in 0..rh {
                                let start = (y + row) * stride + x * 4;
                                fb.pixels[start..start + rw * 4]
                                    .copy_from_slice(&src[row * rw * 4..(row + 1) * rw * 4]);
                            }
                            fb.mark(x, y, rw, rh);
                        }
                        ENC_ZRLE => {
                            let len = read_u32(r)? as usize;
                            let data = read_bytes(r, len)?;
                            let mut fb = lock(&shared.fb);
                            fb.check_rect(x, y, rw, rh)?;
                            zrle.decode(&data, &mut fb, x, y, rw, rh)?;
                            fb.mark(x, y, rw, rh);
                        }
                        ENC_DESKTOP_SIZE => lock(&shared.fb).resize(rw, rh),
                        e => return Err(proto_err(format!("server used unrequested encoding {e}"))),
                    }
                }
                let (fw, fh) = {
                    let mut fb = lock(&shared.fb);
                    fb.generation += 1;
                    (fb.width as u16, fb.height as u16)
                };
                notify();
                w.send(update_request(true, fw, fh)).map_err(|_| proto_err("connection closed"))?;
            }
            1 => {
                // SetColourMapEntries: unused in true-colour mode; skip.
                read_u8(r)?;
                read_u16(r)?;
                let n = read_u16(r)? as usize;
                read_bytes(r, n * 6)?;
            }
            2 => {} // Bell
            3 => {
                read_bytes(r, 3)?;
                let text = read_string(r)?;
                *lock(&shared.clipboard) = Some(text);
                notify();
            }
            t => return Err(proto_err(format!("unknown server message type {t}"))),
        }
    }
}

/// ZRLE uses one zlib stream for the whole connection.
struct ZrleDecoder {
    inflater: Decompress,
    buf: Vec<u8>,
}

impl ZrleDecoder {
    fn new() -> Self {
        ZrleDecoder { inflater: Decompress::new(true), buf: Vec::new() }
    }

    fn inflate(&mut self, input: &[u8]) -> io::Result<()> {
        self.buf.clear();
        let mut consumed = 0;
        loop {
            if self.buf.capacity() - self.buf.len() < 64 * 1024 {
                self.buf.reserve(256 * 1024);
            }
            let (in_before, out_before) = (self.inflater.total_in(), self.inflater.total_out());
            self.inflater
                .decompress_vec(&input[consumed..], &mut self.buf, FlushDecompress::Sync)
                .map_err(|e| proto_err(format!("ZRLE inflate failed: {e}")))?;
            consumed += (self.inflater.total_in() - in_before) as usize;
            let progressed = self.inflater.total_in() != in_before || self.inflater.total_out() != out_before;
            // Done once all input is consumed and the output buffer was not the limit.
            if (consumed >= input.len() && self.buf.len() < self.buf.capacity()) || !progressed {
                return Ok(());
            }
        }
    }

    fn decode(&mut self, data: &[u8], fb: &mut Framebuffer, x: usize, y: usize, w: usize, h: usize) -> io::Result<()> {
        self.inflate(data)?;
        let mut c = Cursor { data: &self.buf, pos: 0 };
        for ty in (y..y + h).step_by(64) {
            let th = 64.min(y + h - ty);
            for tx in (x..x + w).step_by(64) {
                let tw = 64.min(x + w - tx);
                decode_tile(&mut c, fb, tx, ty, tw, th)?;
            }
        }
        Ok(())
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn u8(&mut self) -> io::Result<u8> {
        let b = *self.data.get(self.pos).ok_or_else(|| proto_err("truncated ZRLE data"))?;
        self.pos += 1;
        Ok(b)
    }

    /// A compressed pixel: 3 bytes (B, G, R) for our 32 bpp / depth 24 format.
    fn cpixel(&mut self) -> io::Result<[u8; 3]> {
        let s = self.data.get(self.pos..self.pos + 3).ok_or_else(|| proto_err("truncated ZRLE data"))?;
        self.pos += 3;
        Ok([s[2], s[1], s[0]])
    }

    fn run_length(&mut self) -> io::Result<usize> {
        let mut len = 1;
        loop {
            let b = self.u8()?;
            len += b as usize;
            if b != 255 {
                return Ok(len);
            }
        }
    }
}

fn decode_tile(c: &mut Cursor, fb: &mut Framebuffer, x: usize, y: usize, w: usize, h: usize) -> io::Result<()> {
    let sub = c.u8()?;
    match sub {
        0 => {
            for row in 0..h {
                for col in 0..w {
                    let p = c.cpixel()?;
                    fb.put(x + col, y + row, p);
                }
            }
        }
        1 => {
            let p = c.cpixel()?;
            for row in 0..h {
                for col in 0..w {
                    fb.put(x + col, y + row, p);
                }
            }
        }
        2..=16 => {
            let palette = (0..sub).map(|_| c.cpixel()).collect::<io::Result<Vec<_>>>()?;
            let bits = match sub {
                2 => 1,
                3..=4 => 2,
                _ => 4,
            };
            let mask = (1u8 << bits) - 1;
            for row in 0..h {
                let mut byte = 0u8;
                let mut left = 0;
                for col in 0..w {
                    if left == 0 {
                        byte = c.u8()?;
                        left = 8;
                    }
                    left -= bits;
                    let idx = ((byte >> left) & mask) as usize;
                    let p = *palette.get(idx).ok_or_else(|| proto_err("bad ZRLE palette index"))?;
                    fb.put(x + col, y + row, p);
                }
            }
        }
        128 | 130..=255 => {
            let palette = if sub == 128 {
                Vec::new()
            } else {
                (0..sub - 128).map(|_| c.cpixel()).collect::<io::Result<Vec<_>>>()?
            };
            let total = w * h;
            let mut i = 0;
            while i < total {
                let (p, len) = if sub == 128 {
                    let p = c.cpixel()?;
                    (p, c.run_length()?)
                } else {
                    let idx = c.u8()?;
                    let p = *palette
                        .get((idx & 0x7f) as usize)
                        .ok_or_else(|| proto_err("bad ZRLE palette index"))?;
                    (p, if idx & 0x80 != 0 { c.run_length()? } else { 1 })
                };
                if i + len > total {
                    return Err(proto_err("ZRLE run overflows tile"));
                }
                for k in i..i + len {
                    fb.put(x + k % w, y + k / w, p);
                }
                i += len;
            }
        }
        s => return Err(proto_err(format!("invalid ZRLE subencoding {s}"))),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    #[test]
    fn vnc_auth_matches_reference() {
        // Reference from `openssl enc -des-ecb -K 0e86ceceeef64e26` (the
        // bit-reversed bytes of "password") over an all-zero block.
        let resp = vnc_auth_response("password", &[0u8; 16]);
        assert_eq!(&resp[..8], &resp[8..], "ECB: identical blocks give identical output");
        assert_eq!(hex(&resp[..8]), "ff97502e9422f089");
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn decodes_zrle_tiles_across_one_stream() {
        let mut fb = Framebuffer::new(80, 2);
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        let mut z = ZrleDecoder::new();

        // Rect 1 (80x2 → tiles 64x2 and 16x2): solid red, then plain RLE blue×20 + green×12.
        let mut tiles = vec![1, 0, 0, 255];
        tiles.extend([128, 255, 0, 0, 19, 0, 255, 0, 11]);
        enc.write_all(&tiles).unwrap();
        enc.flush().unwrap();
        let chunk1 = enc.get_ref().clone();
        z.decode(&chunk1, &mut fb, 0, 0, 80, 2).unwrap();
        let px = |fb: &Framebuffer, x: usize, y: usize| fb.pixels[(y * 80 + x) * 4..(y * 80 + x) * 4 + 3].to_vec();
        assert_eq!(px(&fb, 0, 0), [255, 0, 0]);
        assert_eq!(px(&fb, 63, 1), [255, 0, 0]);
        assert_eq!(px(&fb, 64, 0), [0, 0, 255]);
        assert_eq!(px(&fb, 79, 0), [0, 0, 255]); // 20 blue fills row 0 (16) + 4 of row 1
        assert_eq!(px(&fb, 67, 1), [0, 0, 255]);
        assert_eq!(px(&fb, 68, 1), [0, 255, 0]);

        // Rect 2 continues the same zlib stream: 2-colour packed palette, 1 bit/pixel.
        let start = enc.get_ref().len();
        enc.write_all(&[2, 0, 0, 0, 255, 255, 255, 0b0101_0000]).unwrap();
        enc.flush().unwrap();
        let chunk2 = enc.get_ref()[start..].to_vec();
        z.decode(&chunk2, &mut fb, 0, 0, 4, 1).unwrap();
        assert_eq!(px(&fb, 0, 0), [0, 0, 0]);
        assert_eq!(px(&fb, 1, 0), [255, 255, 255]);
        assert_eq!(px(&fb, 3, 0), [255, 255, 255]);
    }
}
