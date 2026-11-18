//! **A minimal HTTP/1.1 client** for the one server this service talks to:
//! its own `llama-server` child on loopback. One request per connection
//! (`Connection: close`), a JSON body, a bearer key; the answer's status and
//! body (length-delimited, chunked, or to the end). The connection is made
//! through `env::egress::connect_tcp`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// The largest body this client reads.
pub const BODY_MAX: usize = 64 << 20;

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Connect to `addr` (loopback), with `timeout` for the connect and every
/// read and write after it.
pub fn connect(addr: &SocketAddr, timeout: Duration) -> std::io::Result<TcpStream> {
    // The child is only ever on loopback; any other address is refused
    // before a connection is made.
    if !addr.ip().is_loopback() {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("llama-server at {addr}: not a loopback address; the inference service only dials its child on this machine")));
    }
    let s = env::egress::connect_tcp("llama-server (inference service)", addr, Duration::from_secs(5).min(timeout))?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    Ok(s)
}

/// Send one request on `s` and read the answer.
pub fn send(s: &TcpStream, addr: &SocketAddr, method: &str, path: &str, bearer: Option<&str>, body: Option<&[u8]>) -> std::io::Result<Response> {
    let mut w = s.try_clone()?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAccept: application/json\r\n");
    if let Some(k) = bearer {
        head.push_str(&format!("Authorization: Bearer {k}\r\n"));
    }
    if let Some(b) = body {
        head.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", b.len()));
    }
    head.push_str("\r\n");
    w.write_all(head.as_bytes())?;
    if let Some(b) = body {
        w.write_all(b)?;
    }
    w.flush()?;
    read_response(s)
}

fn bad(m: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, m.into())
}

fn read_response(s: &TcpStream) -> std::io::Result<Response> {
    let mut r = BufReader::new(s);
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the server closed the connection without answering"));
    }
    let status: u16 = line.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or_else(|| bad(format!("not an HTTP status line: {:?}", line.trim())))?;
    let (mut length, mut chunked) = (None, false);
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 {
            return Err(bad("the headers end early"));
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            if k == "content-length" {
                length = v.parse::<usize>().ok();
            } else if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            }
        }
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size = String::new();
            r.read_line(&mut size)?;
            let n = usize::from_str_radix(size.trim().split(';').next().unwrap_or(""), 16).map_err(|_| bad("a bad chunk size"))?;
            if n == 0 {
                break;
            }
            if body.len() + n > BODY_MAX {
                return Err(bad("the answer is too large"));
            }
            let at = body.len();
            body.resize(at + n, 0);
            r.read_exact(&mut body[at..])?;
            let mut crlf = [0u8; 2];
            r.read_exact(&mut crlf)?;
        }
    } else if let Some(n) = length {
        if n > BODY_MAX {
            return Err(bad("the answer is too large"));
        }
        body.resize(n, 0);
        r.read_exact(&mut body)?;
    } else {
        r.take(BODY_MAX as u64 + 1).read_to_end(&mut body)?;
        if body.len() > BODY_MAX {
            return Err(bad("the answer is too large"));
        }
    }
    Ok(Response { status, body })
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_loopback_is_dialled() {
        for a in ["192.0.2.7:80", "0.0.0.0:80", "[::]:80"] {
            let e = super::connect(&a.parse().unwrap(), std::time::Duration::from_secs(1)).unwrap_err();
            assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "{a}");
            assert!(e.to_string().contains("not a loopback address"), "{e}");
        }
    }
}
