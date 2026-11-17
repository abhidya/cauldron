//! **The command line's way to the `mcp` service**: find it, read its door
//! secret, and send it one JSON-RPC message. Used by the stdio bridge
//! (`cauldron mcp stdio`), by `cauldron soup import` when the service holds
//! the soup, and by `cauldron mcp status`.
//!
//! - The service is found through the environment's seam table: its `mcp`
//!   row must be LIVE (the same handshake `cauldron status` makes), and its
//!   seam's `MCP` verb answers the door's address (`http://127.0.0.1:<port>/mcp`).
//! - The secret is `<root>/data/<env>/mcp/door.secret`, minted by the
//!   service at every start (owner-only). It is read for each request and
//!   never printed, logged or put in an error message.
//! - Every dial goes through `env::egress::connect_tcp`, loopback only.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use env::seams::{self, SeamState, Table};
use env::Environment;

/// The service's name: its seam row, its declaration, its data dir.
pub const SERVICE: &str = "mcp";

/// The door's one route (shapewire's MCP carrier).
pub const PATH: &str = "/mcp";

/// How long a request may take: a large `soup_put` judges every record.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// A running service: its pid, its seam, and its door.
#[derive(Debug, Clone)]
pub struct Found {
    pub pid: u32,
    pub seam: String,
    pub door: SocketAddr,
}

/// Why the service could not be reached.
#[derive(Debug, Clone)]
pub enum Absent {
    /// The seam table has no `mcp` row.
    NoRow,
    /// The row is there and its probe is not live (the state, and why).
    NotLive(String, String),
    /// The seam table cannot be read, or the seam did not say where the door is.
    Broken(String),
}

impl Absent {
    pub fn say(&self) -> String {
        match self {
            Absent::NoRow => "it is not running (no `mcp` row in the seam table)".to_string(),
            Absent::NotLive(state, why) => format!("it is not reachable ({state}: {why})"),
            Absent::Broken(why) => format!("it cannot be reached ({why})"),
        }
    }
}

/// The command that starts the service in `env`.
pub fn start_command(env: &str) -> String {
    format!("cauldron --env {env} start {SERVICE}")
}

/// Find the environment's running service.
pub fn find(e: &Environment) -> std::result::Result<Found, Absent> {
    let table = Table::load(&e.name, &e.data_root, &e.seams()).map_err(|err| Absent::Broken(format!("{err:#}")))?;
    let Some(row) = table.seams.get(SERVICE) else { return Err(Absent::NoRow) };
    let v = seams::probe(row);
    if v.state != SeamState::Live {
        return Err(Absent::NotLive(v.state.as_str().to_string(), v.detail));
    }
    let url = seam_ask(&row.addr, "MCP").map_err(|err| Absent::Broken(format!("asking its seam for the door: {err:#}")))?;
    let door = url
        .strip_prefix("http://")
        .and_then(|u| u.strip_suffix(PATH))
        .and_then(|a| a.parse::<SocketAddr>().ok())
        .filter(|a| a.ip().is_loopback())
        .ok_or_else(|| Absent::Broken(format!("its seam answered `{url}`, which is not a loopback door")))?;
    Ok(Found { pid: row.pid, seam: row.addr.clone(), door })
}

/// One line to the service's seam, one line back.
pub fn seam_ask(addr: &str, line: &str) -> Result<String> {
    let sa: SocketAddr = addr.parse().with_context(|| format!("the seam address `{addr}`"))?;
    let s = env::egress::connect_tcp("mcp seam", &sa, Duration::from_secs(2))?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut w = s.try_clone()?;
    w.write_all(format!("{line}\n").as_bytes())?;
    w.flush()?;
    let mut out = String::new();
    BufReader::new(s).read_line(&mut out)?;
    let out = out.trim().to_string();
    if out.is_empty() {
        bail!("{addr} said nothing to `{line}`");
    }
    Ok(out)
}

/// Where the door secret is.
pub fn secret_path(e: &Environment) -> PathBuf {
    soup_door::grants::store_dir(e).join("door.secret")
}

/// The door secret, as the service minted it at its last start.
fn secret(e: &Environment) -> Result<String> {
    let p = secret_path(e);
    let s = std::fs::read_to_string(&p).with_context(|| format!("reading the door secret {}", p.display()))?;
    let s = s.trim().to_string();
    if s.is_empty() {
        bail!("the door secret {} is empty", p.display());
    }
    Ok(s)
}

/// An HTTP answer: its status and its body.
#[derive(Debug, Clone)]
pub struct Answer {
    pub status: u16,
    pub body: String,
}

/// The most the door reads in one request (`soup_door::catalogue`).
pub const BODY_LIMIT: usize = soup_door::catalogue::REQUEST_BODY_LIMIT;

/// `n` bytes as MiB, one decimal.
pub fn mib(n: usize) -> String {
    format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0))
}

/// The JSON-RPC body of one `tools/call`.
pub fn call_body(tool: &str, arguments: serde_json::Value) -> String {
    serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": arguments}}).to_string()
}

/// `POST /mcp` with the secret: one JSON-RPC message, one answer. A body
/// larger than the door reads ([`BODY_LIMIT`]) is refused here, before
/// anything is sent.
pub fn post(e: &Environment, door: SocketAddr, body: &str) -> Result<Answer> {
    if body.len() > BODY_LIMIT {
        bail!(
            "this request is {} bytes ({}), over the {} the mcp door reads in one request (`too_large`); nothing was \
             sent, so nothing was written. Send fewer records per call.",
            body.len(),
            mib(body.len()),
            mib(BODY_LIMIT)
        );
    }
    let secret = secret(e)?;
    let mut s = env::egress::connect_tcp("mcp door", &door, Duration::from_secs(5)).with_context(|| format!("connecting to the mcp door at {door}"))?;
    s.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    s.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json, text/event-stream\r\n\
         Content-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n",
        path = PATH,
        port = door.port(),
        len = body.len()
    );
    // The secret goes on the wire and nowhere else.
    s.write_all(head.as_bytes())?;
    s.write_all(b"Authorization: Bearer ")?;
    s.write_all(secret.as_bytes())?;
    s.write_all(b"\r\n\r\n")?;
    s.write_all(body.as_bytes())?;
    s.flush()?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).with_context(|| format!("reading the mcp door's answer from {door}"))?;
    parse_http(&raw)
}

/// `POST <path>` with the secret: `head` (one JSON-RPC message, no
/// newline) on the first line, then `len` bytes of `content`, streamed, never
/// held whole. When the door answers before taking every byte (it refused
/// the call from its first line), its answer is still read.
pub fn post_stream(e: &Environment, door: SocketAddr, path: &str, head_line: &str, content: &mut dyn Read, len: u64) -> Result<Answer> {
    let secret = secret(e)?;
    let mut s = env::egress::connect_tcp("mcp door", &door, Duration::from_secs(5)).with_context(|| format!("connecting to the mcp door at {door}"))?;
    s.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    s.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    let total = head_line.len() as u64 + 1 + len;
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/octet-stream\r\n\
         Content-Length: {total}\r\nConnection: close\r\n",
        port = door.port(),
    );
    s.write_all(head.as_bytes())?;
    s.write_all(b"Authorization: Bearer ")?;
    s.write_all(secret.as_bytes())?;
    s.write_all(b"\r\n\r\n")?;
    let sent = (|| -> std::io::Result<u64> {
        s.write_all(head_line.as_bytes())?;
        s.write_all(b"\n")?;
        let n = std::io::copy(&mut content.take(len), &mut s)?;
        s.flush()?;
        Ok(n)
    })();
    let mut raw = Vec::new();
    let read = s.read_to_end(&mut raw);
    if raw.is_empty() {
        let n = sent.with_context(|| format!("sending to the mcp door at {door}"))?;
        read.with_context(|| format!("reading the mcp door's answer from {door}"))?;
        if n != len {
            bail!("the content ended after {n} of {len} bytes");
        }
    }
    parse_http(&raw)
}

fn parse_http(raw: &[u8]) -> Result<Answer> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").context("the mcp door's answer has no end of headers")?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut body = raw[split + 4..].to_vec();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .with_context(|| format!("the mcp door's answer has no status line: {}", head.lines().next().unwrap_or("")))?;
    let header = |name: &str| {
        head.lines()
            .skip(1)
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim().to_string())
    };
    if header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        body = dechunk(&body)?;
    } else if let Some(n) = header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        body.truncate(n);
    }
    Ok(Answer { status, body: String::from_utf8_lossy(&body).into_owned() })
}

fn dechunk(mut b: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = b.windows(2).position(|w| w == b"\r\n").context("a chunk with no size line")?;
        let size = std::str::from_utf8(&b[..eol]).ok().and_then(|s| usize::from_str_radix(s.split(';').next().unwrap_or("").trim(), 16).ok()).context("a chunk size that is not hex")?;
        b = &b[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if b.len() < size {
            bail!("a chunk shorter than its size");
        }
        out.extend_from_slice(&b[..size]);
        b = b.get(size + 2..).unwrap_or(&[]);
    }
}

/// One `tools/call` to the door: the JSON-RPC `result` (a tool result, with
/// `isError` on a refusal), or an error for anything that is not one.
pub fn call(e: &Environment, door: SocketAddr, tool: &str, arguments: serde_json::Value) -> Result<serde_json::Value> {
    let a = post(e, door, &call_body(tool, arguments))?;
    let v: serde_json::Value = serde_json::from_str(&a.body).with_context(|| format!("the mcp door answered HTTP {} with no JSON", a.status))?;
    if let Some(r) = v.get("result") {
        return Ok(r.clone());
    }
    bail!("the mcp door answered HTTP {}: {}", a.status, v.get("error").map(|e| e.to_string()).unwrap_or_else(|| v.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_read_by_its_length_or_its_chunks() {
        let a = parse_http(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}junk").unwrap();
        assert_eq!((a.status, a.body.as_str()), (200, "{}"));
        let a = parse_http(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{\"a\r\n4\r\n\":1}\r\n0\r\n\r\n").unwrap();
        assert_eq!(a.body, "{\"a\":1}");
        let a = parse_http(b"HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\n\r\n").unwrap();
        assert_eq!((a.status, a.body.as_str()), (202, ""));
        assert!(parse_http(b"garbage").is_err());
    }
}
