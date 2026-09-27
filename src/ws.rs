//! Minimal WebSocket server (RFC 6455) — for agent integration (agent.rs). Zero deps: SHA-1, base64 here.
//!
//! Only what's needed: HTTP Upgrade handshake (auth header check), reading masked client frames
//! (text, continuation, ping, close), writing server frames (unmasked). No extensions or compression.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Caps for a peer that hasn't authenticated yet (handshake) and for one message.
const HEADER_MAX: u64 = 16 << 10;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MESSAGE_MAX: usize = 64 << 20;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(wi);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, t);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn accept_key(key: &str) -> String {
    base64(&sha1(format!("{}{GUID}", key.trim()).as_bytes()))
}

/// Reads and answers the HTTP Upgrade request. `auth` = (header name, expected value) — 401 and drop
/// if different.
/// On success returns the read buffer (including bytes left over after the handshake).
/// The headers must arrive within `HANDSHAKE_TIMEOUT` and `HEADER_MAX` bytes (before auth is known).
pub fn handshake(stream: &TcpStream, auth: Option<(&str, &str)>) -> io::Result<BufReader<TcpStream>> {
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let mut r = BufReader::new(stream.try_clone()?);
    let mut budget = HEADER_MAX;
    let mut key = None;
    let mut authorized = auth.is_none();
    let mut upgrade = false;
    let mut protocol = None;
    let mut first = true;
    loop {
        let mut line = String::new();
        let n = (&mut r).take(budget).read_line(&mut line)? as u64;
        if n == 0 {
            let why = if budget == 0 { "handshake headers too large" } else { "closed during handshake" };
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, why));
        }
        budget -= n;
        let line = line.trim_end();
        if first {
            first = false;
            if !line.starts_with("GET ") {
                return Err(io::Error::other("not a GET"));
            }
            continue;
        }
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else { continue };
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
        match name.as_str() {
            "sec-websocket-key" => key = Some(value.to_string()),
            "upgrade" => upgrade = value.eq_ignore_ascii_case("websocket"),
            // If a subprotocol is requested, echo back the first — otherwise the node `ws` client disconnects
            // (Claude Code requests `mcp`, measured)
            "sec-websocket-protocol" => protocol = value.split(',').next().map(|p| p.trim().to_string()),
            n if auth.is_some_and(|(h, v)| h.eq_ignore_ascii_case(n) && v == value) => authorized = true,
            _ => {}
        }
    }
    let mut w = stream;
    let Some(key) = key.filter(|_| upgrade) else {
        w.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")?;
        return Err(io::Error::other("not a websocket upgrade"));
    };
    if !authorized {
        w.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")?;
        return Err(io::Error::other("unauthorized"));
    }
    let proto = protocol.map(|p| format!("Sec-WebSocket-Protocol: {p}\r\n")).unwrap_or_default();
    write!(
        w,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n{proto}\r\n",
        accept_key(&key)
    )?;
    stream.set_read_timeout(None)?;
    Ok(r)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Text(String),
    Ping(Vec<u8>),
    /// Close — the status code bytes (echoed back in the reply).
    Close(Vec<u8>),
    /// Binary·pong — not used here.
    Other,
}

/// Reads messages. Continuation fragments are gathered — also across control frames (ping) sent between
/// them, which are returned on their own without dropping what's gathered so far.
pub struct Reader<R> {
    inner: R,
    /// Server side: client frames must be masked (RFC 6455 5.1).
    need_mask: bool,
    data: Vec<u8>,
    kind: Option<u8>,
}

impl<R: Read> Reader<R> {
    /// Reads client frames (server side).
    pub fn server(inner: R) -> Self {
        Reader { inner, need_mask: true, data: Vec::new(), kind: None }
    }

    /// Reads server frames (client side — tests).
    #[cfg(test)]
    pub fn client(inner: R) -> Self {
        Reader { inner, need_mask: false, data: Vec::new(), kind: None }
    }

    /// One message.
    pub fn read(&mut self) -> io::Result<Frame> {
        let r = &mut self.inner;
        loop {
            let mut h = [0u8; 2];
            r.read_exact(&mut h)?;
            let fin = h[0] & 0x80 != 0;
            let op = h[0] & 0x0F;
            let masked = h[1] & 0x80 != 0;
            if self.need_mask && !masked {
                return Err(io::Error::other("unmasked client frame"));
            }
            let mut len = u64::from(h[1] & 0x7F);
            if len == 126 {
                let mut b = [0u8; 2];
                r.read_exact(&mut b)?;
                len = u64::from(u16::from_be_bytes(b));
            } else if len == 127 {
                let mut b = [0u8; 8];
                r.read_exact(&mut b)?;
                len = u64::from_be_bytes(b);
            }
            if len > (MESSAGE_MAX - self.data.len().min(MESSAGE_MAX)) as u64 {
                return Err(io::Error::other("message too large"));
            }
            let mut mask = [0u8; 4];
            if masked {
                r.read_exact(&mut mask)?;
            }
            let mut payload = vec![0u8; len as usize];
            r.read_exact(&mut payload)?;
            if masked {
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= mask[i % 4];
                }
            }
            match op {
                0x8 => {
                    payload.truncate(2);
                    return Ok(Frame::Close(payload));
                }
                0x9 => return Ok(Frame::Ping(payload)),
                0xA => return Ok(Frame::Other),
                0x0 => self.data.extend(payload),
                op => {
                    self.kind = Some(op);
                    self.data = payload;
                }
            }
            if fin {
                let data = std::mem::take(&mut self.data);
                return Ok(match self.kind.take() {
                    Some(0x2) => Frame::Other,
                    _ => Frame::Text(String::from_utf8_lossy(&data).into_owned()),
                });
            }
        }
    }
}

/// Server → client frame (unmasked).
pub fn write(w: &mut impl Write, opcode: u8, payload: &[u8]) -> io::Result<()> {
    // One buffer, one write — header and payload in one segment
    let n = payload.len();
    let mut frame = Vec::with_capacity(n + 10);
    frame.push(0x80 | opcode);
    if n < 126 {
        frame.push(n as u8);
    } else if n <= u16::MAX as usize {
        frame.push(126);
        frame.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        frame.push(127);
        frame.extend_from_slice(&(n as u64).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    w.write_all(&frame)?;
    w.flush()
}

pub fn write_text(w: &mut impl Write, s: &str) -> io::Result<()> {
    write(w, 0x1, s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_base64_and_accept_key() {
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
        // Example from RFC 6455 1.3
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn frames_round_trip_with_mask_and_fragments() {
        // Client frames: masked "Hel" + continuation "lo"
        let mask = [1u8, 2, 3, 4];
        let masked = |p: &[u8]| p.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]).collect::<Vec<u8>>();
        let mut bytes = vec![0x01, 0x80 | 3];
        bytes.extend(mask);
        bytes.extend(masked(b"Hel"));
        bytes.extend([0x80, 0x80 | 2]);
        bytes.extend(mask);
        bytes.extend(masked(b"lo"));
        assert_eq!(Reader::server(bytes.as_slice()).read().unwrap(), Frame::Text("Hello".into()));
        // Server frame length formats
        let mut out = Vec::new();
        write_text(&mut out, &"x".repeat(300)).unwrap();
        assert_eq!(&out[..4], &[0x81, 126, 1, 44]);
        assert_eq!(Reader::client(out.as_slice()).read().unwrap(), Frame::Text("x".repeat(300)));
        let mut ping = vec![0x89, 0x80, 0, 0, 0, 0];
        assert_eq!(Reader::server(ping.as_slice()).read().unwrap(), Frame::Ping(Vec::new()));
        ping[0] = 0x88;
        assert_eq!(Reader::server(ping.as_slice()).read().unwrap(), Frame::Close(Vec::new()));
        // A ping between fragments keeps what's gathered
        let mut bytes = vec![0x01, 0x80 | 3];
        bytes.extend(mask);
        bytes.extend(masked(b"Hel"));
        bytes.extend([0x89, 0x80 | 1]);
        bytes.extend(mask);
        bytes.extend(masked(b"p"));
        bytes.extend([0x80, 0x80 | 2]);
        bytes.extend(mask);
        bytes.extend(masked(b"lo"));
        let mut r = Reader::server(bytes.as_slice());
        assert_eq!(r.read().unwrap(), Frame::Ping(b"p".to_vec()));
        assert_eq!(r.read().unwrap(), Frame::Text("Hello".into()));
        // Unmasked client frames are refused
        assert!(Reader::server([0x81u8, 1, b'x'].as_slice()).read().is_err());
    }
}
