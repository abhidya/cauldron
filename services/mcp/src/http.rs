//! **`POST /mcp` on loopback**, in front of shapewire's MCP carrier.
//!
//! # What is checked before any of a request's body is read
//!
//! Only the request line and the headers have been read when these run, in
//! this order, and a refusal is answered without reading a byte of the body
//! (the connection is then closed):
//!
//! 1. the door secret (`Authorization: Bearer <secret>`, compared in
//!    constant time), else **401**;
//! 2. `Host` is exactly `127.0.0.1:<port>` or `localhost:<port>`, else
//!    **403** (a page served from another name that resolves to loopback,
//!    the DNS-rebinding shape, carries its own name here);
//! 3. any `Origin` at all, **403** (a browser sends one; the bridge and
//!    every command-line client do not);
//! 4. a `Content-Length`, else **411** (every client of this door sends
//!    one), and at most [`BODY_LIMIT`], else **413** `too_large`.
//!
//! Only then is the body read: its length is first reserved from the
//! in-flight budget ([`INFLIGHT_BODY_BYTES`]; a request that cannot get its
//! share within [`BUDGET_WAIT`] is answered **503** `busy`), and the whole
//! body must arrive within [`BODY_TIMEOUT`] (else **408**). Then the
//! carrier's own transport checks (`Accept`, `Content-Type`,
//! `mcp-protocol-version`), the secret again through
//! [`shapewire::McpTools::verify`], and the JSON-RPC message. `GET` and
//! `DELETE` are 405: there is no event stream and no session to end.
//!
//! # The limits, and why these numbers
//!
//! - [`MAX_CONNECTIONS`] (64) connections are served at once. The next one
//!   waits in the kernel's accept queue until a slot frees; it is never
//!   read while it waits. One Claude Code session holds one connection per
//!   call in flight, so 64 is several busy sessions at once.
//! - [`HEADER_TIMEOUT`] (5 s): a connection must deliver a request's whole
//!   head within it, and an idle kept-alive connection is closed after it.
//!   A loopback client sends its head in microseconds; a slow one only
//!   holds a slot.
//! - [`READ_BUFFER`] (64 KiB) per connection, so what an unauthenticated
//!   connection can make the service hold is bounded by 64 x 64 KiB.
//! - [`BODY_LIMIT`] (16 MiB, `soup_door::catalogue::REQUEST_BODY_LIMIT`): a
//!   `soup_put` of several thousand records fits.
//! - [`INFLIGHT_BODY_BYTES`] (64 MiB): bodies being read or answered at
//!   once, four of the largest. A body's share is held until its answer is
//!   ready.
//! - [`BODY_TIMEOUT`] (10 s) for a whole body, [`BUDGET_WAIT`] (10 s) for a
//!   share of the budget: 16 MiB crosses loopback in milliseconds.
//! - [`WRITE_TIMEOUT`] (10 s): an answer the client does not read (a write
//!   that makes no progress for that long) closes the connection, and the
//!   log says so. A call is applied before its answer is written, so a
//!   write closed this way has landed whole or not at all.
//!
//! # `POST /mcp/dump-session`
//!
//! The second route carries `soup_dump_session` from the stdio bridge: the
//! `tools/call` message on the body's first line (at most
//! `SESSION_HEAD_LIMIT`, 64 KiB), then the session's transcript, verbatim.
//! The same three checks run first (secret, `Host`, `Origin`), then the
//! `Content-Length` (required, at most the 64 MiB transcript limit plus the
//! first line). Only the first line is read before the call is judged
//! (`_ctx`, the cauldron, the `dump` grant, the size); a refused call is
//! answered without reading a byte of the transcript. The transcript is
//! then streamed to the dumps store's blob store, never held whole in
//! memory, and must arrive within [`SESSION_BODY_TIMEOUT`]. At most
//! [`SESSION_SLOTS`] such requests are read at once; another waits up to
//! [`BUDGET_WAIT`] for a slot, then is answered **503** `busy`.
//!
//! The listener is bound to `127.0.0.1` only ([`bind`]); nothing here
//! binds another address.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use serde_json::{json, Value};
use shapewire::envelope::Identity;
use shapewire::mcp::http::{door, not_allowed, PATH};
use shapewire::mcp::rpc;
use tokio::sync::{watch, Semaphore};

use crate::door::Tools;
use soup_door::catalogue::{SESSION_HEAD_LIMIT, SESSION_PATH};

/// The fixed port in `prod`, where the bridge and the faces read no seam.
/// Every other environment's port is the kernel's choice, published
/// through the seam's `MCP` verb.
pub const PROD_PORT: u16 = 3654;

/// The largest request body the door reads, from a request that already
/// carried the secret (see the module docs).
pub const BODY_LIMIT: usize = soup_door::catalogue::REQUEST_BODY_LIMIT;

/// The bytes of request bodies held at once, across every connection.
pub const INFLIGHT_BODY_BYTES: usize = 4 * BODY_LIMIT;

/// Connections served at once; the next waits in the accept queue.
pub const MAX_CONNECTIONS: usize = 64;

/// A request's head must arrive within this, and an idle connection is
/// closed after it.
pub const HEADER_TIMEOUT: Duration = Duration::from_secs(5);

/// A request's whole body must arrive within this.
pub const BODY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a request waits for its share of [`INFLIGHT_BODY_BYTES`].
pub const BUDGET_WAIT: Duration = Duration::from_secs(10);

/// A connection whose answer cannot be written for this long (its client
/// stopped reading) is closed, so it cannot hold a connection slot forever.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Each connection's read buffer (the head must fit in it).
pub const READ_BUFFER: usize = 64 * 1024;

/// How many bytes of a transcript arrive between two reads of its grant.
const SESSION_GRANT_CHECK: u64 = 4 * 1024 * 1024;

/// A session dump's transcript must arrive within this (64 MiB crosses
/// loopback in well under a second; a slow disk is given room).
pub const SESSION_BODY_TIMEOUT: Duration = Duration::from_secs(120);

/// Session dumps read at once.
pub const SESSION_SLOTS: usize = 2;

/// The loopback host this door binds, and the only one.
pub const HOST: &str = "127.0.0.1";

/// Bind the door: `127.0.0.1`, on [`PROD_PORT`] in `prod` and an ephemeral
/// port elsewhere (shapewire's fixed-port rule).
pub fn bind(env: &str) -> anyhow::Result<std::net::TcpListener> {
    let configured = (env == shapewire::FACES_ENV).then_some(PROD_PORT);
    shapewire::seam::listen(HOST, configured, env).map_err(|e| anyhow::anyhow!("{e}"))
}

struct Http {
    tools: Tools,
    identity: Identity,
    port: u16,
    budget: Arc<Semaphore>,
    sessions: Arc<Semaphore>,
}

/// The router: `POST /mcp` and `POST /mcp/dump-session`, and 405 for
/// `GET`/`DELETE` there.
pub fn router(tools: Tools, identity: Identity, addr: SocketAddr) -> Router {
    let state = Arc::new(Http {
        tools,
        identity,
        port: addr.port(),
        budget: Arc::new(Semaphore::new(INFLIGHT_BODY_BYTES)),
        sessions: Arc::new(Semaphore::new(SESSION_SLOTS)),
    });
    Router::new()
        .route(PATH, axum::routing::post(post).get(not_allowed).delete(not_allowed))
        .route(SESSION_PATH, axum::routing::post(session).get(not_allowed).delete(not_allowed))
        .with_state(state)
}

/// **Serve `app` on `listener`** until `stop` resolves: at most
/// [`MAX_CONNECTIONS`] at once, each with [`HEADER_TIMEOUT`] and
/// [`READ_BUFFER`]. On `stop`, no new connection is accepted, every open
/// one finishes the request it is answering and is closed, and this returns
/// when they all have.
pub async fn serve(listener: tokio::net::TcpListener, app: Router, stop: impl Future<Output = ()>) {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let (closing_tx, closing_rx) = watch::channel(false);
    tokio::pin!(stop);
    loop {
        let slot = tokio::select! {
            _ = &mut stop => break,
            s = slots.clone().acquire_owned() => match s { Ok(s) => s, Err(_) => break },
        };
        let stream = tokio::select! {
            _ = &mut stop => break,
            r = listener.accept() => match r {
                Ok((s, _)) => s,
                Err(e) => {
                    // Out of descriptors, or a connection reset before it was
                    // accepted: wait a moment rather than spin.
                    tracing::warn!("accepting a connection: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
        };
        let _ = stream.set_nodelay(true);
        let svc = TowerToHyperService::new(app.clone());
        let mut closing = closing_rx.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let mut b = hyper::server::conn::http1::Builder::new();
            b.timer(TokioTimer::new()).header_read_timeout(HEADER_TIMEOUT).max_buf_size(READ_BUFFER).keep_alive(true);
            let conn = b.serve_connection(TokioIo::new(WriteTimeout::new(stream, WRITE_TIMEOUT)), svc);
            tokio::pin!(conn);
            let mut asked = false;
            loop {
                tokio::select! {
                    _ = conn.as_mut() => break,
                    changed = closing.changed(), if !asked => {
                        asked = true;
                        if changed.is_ok() {
                            conn.as_mut().graceful_shutdown();
                        }
                    }
                }
            }
        });
    }
    let _ = closing_tx.send(true);
    // Every connection holds a slot until it is closed: holding them all
    // means every connection has finished.
    let _ = slots.acquire_many(MAX_CONNECTIONS as u32).await;
}

/// A connection whose writes fail once one has made no progress for
/// `limit`: hyper has a timeout for reading a head, none for writing an
/// answer, and a client that stops reading would otherwise hold its
/// connection slot for as long as it likes.
struct WriteTimeout<S> {
    io: S,
    limit: Duration,
    stalled: Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
}

impl<S> WriteTimeout<S> {
    fn new(io: S, limit: Duration) -> Self {
        WriteTimeout { io, limit, stalled: None }
    }

    /// `r` from the inner write: progress clears the stall, a pending write
    /// starts (or keeps) its clock, and a stall past the limit fails.
    fn judge<T>(&mut self, cx: &mut std::task::Context<'_>, r: std::task::Poll<std::io::Result<T>>) -> std::task::Poll<std::io::Result<T>> {
        use std::task::Poll;
        if r.is_ready() {
            self.stalled = None;
            return r;
        }
        let limit = self.limit;
        let clock = self.stalled.get_or_insert_with(|| Box::pin(tokio::time::sleep(limit)));
        if clock.as_mut().poll(cx).is_ready() {
            tracing::info!("closing a connection: the client stopped reading its answer ({} s without progress)", limit.as_secs_f64());
            return Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "the client stopped reading its answer")));
        }
        Poll::Pending
    }
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for WriteTimeout<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for WriteTimeout<S> {
    fn poll_write(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &[u8]) -> std::task::Poll<std::io::Result<usize>> {
        let r = std::pin::Pin::new(&mut self.io).poll_write(cx, buf);
        self.judge(cx, r)
    }

    fn poll_flush(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        let r = std::pin::Pin::new(&mut self.io).poll_flush(cx);
        self.judge(cx, r)
    }

    fn poll_shutdown(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

fn refuse(status: StatusCode, code: i64, message: &str, data: Option<Value>) -> Response {
    let mut error = json!({"code": code, "message": message});
    if let Some(d) = data {
        error["data"] = d;
    }
    let body = json!({"jsonrpc": "2.0", "id": Value::Null, "error": error});
    (
        status,
        [(header::CONTENT_TYPE, HeaderValue::from_static("application/json")), (header::CONNECTION, HeaderValue::from_static("close"))],
        body.to_string(),
    )
        .into_response()
}

fn mib(n: usize) -> String {
    format!("{} MiB", n / (1024 * 1024))
}

/// Read exactly `len` bytes of body into one buffer of that size (so a
/// body costs its length once, not a growing buffer's doublings).
async fn read_body(body: axum::body::Body, len: usize) -> Option<Vec<u8>> {
    use http_body_util::BodyExt;
    let mut body = body;
    let mut out = Vec::with_capacity(len);
    while let Some(frame) = body.frame().await {
        let Ok(data) = frame.ok()?.into_data() else { continue };
        if out.len() + data.len() > len {
            return None;
        }
        out.extend_from_slice(&data);
    }
    (out.len() == len).then_some(out)
}

/// Checks 1-3 of the module docs: who, from where. `Some` is the refusal.
fn guard(h: &Http, headers: &axum::http::HeaderMap) -> Option<Response> {
    let auth = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    if !h.tools.0.admits(auth) {
        let mut r = refuse(
            StatusCode::UNAUTHORIZED,
            rpc::UNAUTHORIZED,
            "unauthorized: this door needs `Authorization: Bearer <secret>` (the environment's mcp/door.secret)",
            None,
        );
        r.headers_mut().insert("www-authenticate", HeaderValue::from_static("Bearer realm=\"cauldron-mcp\""));
        return Some(r);
    }
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let allowed = [format!("{HOST}:{}", h.port), format!("localhost:{}", h.port)];
    if !allowed.iter().any(|a| a == host) {
        return Some(refuse(StatusCode::FORBIDDEN, rpc::SERVER, "forbidden: the Host header must name this loopback door", None));
    }
    if headers.contains_key(header::ORIGIN) {
        return Some(refuse(StatusCode::FORBIDDEN, rpc::SERVER, "forbidden: a request with an Origin (a web page) is never served here", None));
    }
    None
}

fn content_length(headers: &axum::http::HeaderMap) -> Option<u64> {
    headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok())
}

async fn post(State(h): State<Arc<Http>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let headers = &parts.headers;
    // 1-3: who, from where. Nothing of the body has been read.
    if let Some(r) = guard(&h, headers) {
        return r;
    }
    // 4: how much. A body without a declared length is not read at all.
    let Some(len) = content_length(headers) else {
        return refuse(StatusCode::LENGTH_REQUIRED, rpc::SERVER, "length_required: this door reads only a body with a Content-Length", None);
    };
    if len > BODY_LIMIT as u64 {
        return refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            rpc::SERVER,
            &format!(
                "too_large: this request's body is {len} bytes, and the door reads at most {BODY_LIMIT} ({}) in one request. \
                 Nothing was read or written. Send fewer records per call.",
                mib(BODY_LIMIT)
            ),
            Some(json!({"code": "too_large", "limit_bytes": BODY_LIMIT, "content_length": len})),
        );
    }
    let len = len as usize;
    // The body's share of the in-flight budget, held until the answer is
    // ready.
    let share = match tokio::time::timeout(BUDGET_WAIT, h.budget.clone().acquire_many_owned(len as u32)).await {
        Ok(Ok(p)) => p,
        _ => {
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                rpc::SERVER,
                &format!(
                    "busy: other requests are holding the {} of request bodies this door reads at once. Nothing was \
                     read or written. Try again.",
                    mib(INFLIGHT_BODY_BYTES)
                ),
                Some(json!({"code": "busy", "inflight_limit_bytes": INFLIGHT_BODY_BYTES})),
            )
        }
    };
    let bytes = match tokio::time::timeout(BODY_TIMEOUT, read_body(body, len)).await {
        Ok(Some(b)) => b,
        Ok(None) => return refuse(StatusCode::BAD_REQUEST, rpc::SERVER, "bad_request: the body did not match its Content-Length", None),
        Err(_) => {
            return refuse(
                StatusCode::REQUEST_TIMEOUT,
                rpc::SERVER,
                &format!("timeout: the body did not arrive within {} s. Nothing was written.", BODY_TIMEOUT.as_secs()),
                None,
            )
        }
    };
    let answer = door(&h.tools, &h.identity, headers, &bytes).await;
    drop(bytes);
    drop(share);
    answer
}

fn answer(id: Value, out: Result<Value, shapewire::envelope::Refusal>) -> Response {
    use shapewire::mcp::{result_body, tool_refused, tool_result};
    let payload = match out {
        Ok(v) => tool_result(v),
        Err(r) => tool_refused(&r),
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, HeaderValue::from_static("application/json")), (header::CONNECTION, HeaderValue::from_static("close"))],
        result_body(id, payload).to_string(),
    )
        .into_response()
}

/// `POST /mcp/dump-session` (module docs).
async fn session(State(h): State<Arc<Http>>, req: Request) -> Response {
    use http_body_util::BodyExt;
    use std::io::Write;
    let (parts, body) = req.into_parts();
    if let Some(r) = guard(&h, &parts.headers) {
        return r;
    }
    let Some(len) = content_length(&parts.headers) else {
        return refuse(StatusCode::LENGTH_REQUIRED, rpc::SERVER, "length_required: this door reads only a body with a Content-Length", None);
    };
    let most = soup_door::dumps::TRANSCRIPT_LIMIT + SESSION_HEAD_LIMIT as u64 + 1;
    if len > most {
        return refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            rpc::SERVER,
            &format!(
                "too_large: this session dump is {len} bytes, and a transcript is at most {} ({}). Nothing was read or written.",
                soup_door::dumps::TRANSCRIPT_LIMIT,
                mib(soup_door::dumps::TRANSCRIPT_LIMIT as usize)
            ),
            Some(json!({"code": "too_large", "limit_bytes": soup_door::dumps::TRANSCRIPT_LIMIT, "content_length": len})),
        );
    }
    let _slot = match tokio::time::timeout(BUDGET_WAIT, h.sessions.clone().acquire_owned()).await {
        Ok(Ok(p)) => p,
        _ => {
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                rpc::SERVER,
                &format!("busy: {SESSION_SLOTS} session dumps are being read already. Nothing was read or written. Try again."),
                Some(json!({"code": "busy"})),
            )
        }
    };
    // The call's line, and nothing more is judged until it has been.
    let mut body = body;
    let mut head: Vec<u8> = Vec::new();
    let read_head = async {
        while !head.contains(&b'\n') {
            if head.len() > SESSION_HEAD_LIMIT {
                return false;
            }
            match body.frame().await {
                Some(Ok(f)) => {
                    if let Ok(d) = f.into_data() {
                        head.extend_from_slice(&d);
                    }
                }
                _ => return false,
            }
        }
        true
    };
    match tokio::time::timeout(BODY_TIMEOUT, read_head).await {
        Ok(true) => {}
        Ok(false) => return refuse(StatusCode::BAD_REQUEST, rpc::SERVER, "bad_request: the body's first line is not a call within 64 KiB", None),
        Err(_) => return refuse(StatusCode::REQUEST_TIMEOUT, rpc::SERVER, "timeout: the call did not arrive. Nothing was written.", None),
    }
    let nl = head.iter().position(|b| *b == b'\n').expect("read to a newline");
    if nl > SESSION_HEAD_LIMIT || (nl as u64) + 1 > len {
        return refuse(StatusCode::BAD_REQUEST, rpc::SERVER, "bad_request: the body's first line is not a call within 64 KiB", None);
    }
    let rest = head.split_off(nl + 1);
    head.truncate(nl);
    let (id, arguments) = match shapewire::mcp::parse(&head) {
        Ok(shapewire::mcp::Incoming::Request { id, method, params })
            if method == "tools/call" && params.get("name").and_then(Value::as_str) == Some("soup_dump_session") =>
        {
            (id, params.get("arguments").cloned().unwrap_or(Value::Null))
        }
        Ok(_) => {
            return refuse(StatusCode::BAD_REQUEST, rpc::INVALID_REQUEST, "bad_request: this route takes only a `tools/call` of soup_dump_session", None)
        }
        Err(e) => return refuse(StatusCode::BAD_REQUEST, e.code, &e.message, None),
    };
    let transcript = len - nl as u64 - 1;
    let door = h.tools.0.clone();
    let begun = tokio::task::spawn_blocking(move || door.session_begin(arguments, transcript)).await;
    let mut s = match begun {
        Ok(Ok(s)) => s,
        Ok(Err(r)) => return answer(id, Err(r)),
        Err(e) => return answer(id, Err(shapewire::envelope::Refusal::new("store", format!("the call failed inside the service: {e}")))),
    };
    let too_large = || crate::door::too_large_transcript(transcript);
    let door = h.tools.0.clone();
    let stream = async {
        if s.blob.write_all(&rest).is_err() {
            return Err(too_large());
        }
        // The grant is read again every few MiB: a revoke stops the reading.
        let mut check_at = SESSION_GRANT_CHECK;
        while s.blob.bytes() < transcript {
            if s.blob.bytes() >= check_at {
                door.session_still_allowed(&s)?;
                check_at = s.blob.bytes() + SESSION_GRANT_CHECK;
            }
            match body.frame().await {
                Some(Ok(f)) => {
                    if let Ok(d) = f.into_data() {
                        if let Err(e) = s.blob.write_all(&d) {
                            return Err(if e.kind() == std::io::ErrorKind::FileTooLarge || s.blob.bytes() + d.len() as u64 > transcript {
                                too_large()
                            } else {
                                shapewire::envelope::Refusal::new("store", format!("writing the transcript: {e}"))
                            });
                        }
                    }
                }
                _ => break,
            }
        }
        Ok(())
    };
    let arrived = match tokio::time::timeout(SESSION_BODY_TIMEOUT, stream).await {
        Ok(r) => r,
        Err(_) => Err(shapewire::envelope::Refusal::new(
            "timeout",
            format!("the transcript did not arrive within {} s. Nothing was written.", SESSION_BODY_TIMEOUT.as_secs()),
        )),
    };
    let door = h.tools.0.clone();
    let out = tokio::task::spawn_blocking(move || door.session_finish(s, arrived))
        .await
        .unwrap_or_else(|e| Err(shapewire::envelope::Refusal::new("store", format!("the call failed inside the service: {e}"))));
    answer(id, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::time::Instant;
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    /// Write `buf` through `w` until it is all written or a write fails.
    async fn write_all<W: AsyncWrite + Unpin>(w: &mut W, mut buf: &[u8]) -> std::io::Result<u64> {
        let mut n = 0u64;
        while !buf.is_empty() {
            let k = std::future::poll_fn(|cx| Pin::new(&mut *w).poll_write(cx, buf)).await?;
            n += k as u64;
            buf = &buf[k..];
        }
        Ok(n)
    }

    /// A peer that never reads: the socket's buffers fill (a Unix socket
    /// pair's are a few KiB, so nothing here depends on how much loopback
    /// TCP would buffer), the next write makes no progress, and it fails
    /// `TimedOut` once the limit has passed, not before.
    #[tokio::test]
    async fn a_write_that_makes_no_progress_fails_once_the_limit_has_passed() {
        let (a, _peer) = tokio::net::UnixStream::pair().unwrap();
        let limit = Duration::from_millis(400);
        let mut w = WriteTimeout::new(a, limit);
        let t = Instant::now();
        let mut sent = 0u64;
        let err = loop {
            match write_all(&mut w, &[7u8; 64 * 1024]).await {
                Ok(n) => sent += n,
                Err(e) => break e,
            }
            assert!(t.elapsed() < Duration::from_secs(10), "64 KiB chunks kept being taken: {sent} bytes");
        };
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");
        assert!(t.elapsed() >= limit, "failed before the limit: {:?}", t.elapsed());
        assert!(t.elapsed() < limit + Duration::from_secs(3), "failed long after the limit: {:?}", t.elapsed());
    }

    /// Opposed: a slow peer that keeps reading, a little at a time, for
    /// three times the limit: every write that makes progress restarts the
    /// clock, so nothing fails.
    #[tokio::test]
    async fn a_slow_reader_that_keeps_reading_is_never_timed_out() {
        let (a, mut peer) = tokio::net::UnixStream::pair().unwrap();
        let limit = Duration::from_millis(400);
        let reader = tokio::spawn(async move {
            let mut got = 0u64;
            let mut buf = [0u8; 4096];
            loop {
                tokio::time::sleep(limit / 4).await;
                let mut rb = ReadBuf::new(&mut buf);
                match std::future::poll_fn(|cx| Pin::new(&mut peer).poll_read(cx, &mut rb)).await {
                    Ok(()) if rb.filled().is_empty() => return got,
                    Ok(()) => got += rb.filled().len() as u64,
                    Err(_) => return got,
                }
            }
        });
        let mut w = WriteTimeout::new(a, limit);
        let t = Instant::now();
        let mut sent = 0u64;
        while t.elapsed() < limit * 3 {
            sent += write_all(&mut w, &[1u8; 1024]).await.expect("a write with progress failed");
        }
        drop(w);
        let got = reader.await.unwrap();
        assert!(sent > 0 && got > 0, "sent {sent}, read {got}");
    }
}
