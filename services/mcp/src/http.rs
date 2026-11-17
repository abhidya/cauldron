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

/// Each connection's read buffer (the head must fit in it).
pub const READ_BUFFER: usize = 64 * 1024;

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
}

/// The router: `POST /mcp`, and 405 for `GET`/`DELETE` there.
pub fn router(tools: Tools, identity: Identity, addr: SocketAddr) -> Router {
    let state = Arc::new(Http { tools, identity, port: addr.port(), budget: Arc::new(Semaphore::new(INFLIGHT_BODY_BYTES)) });
    Router::new().route(PATH, axum::routing::post(post).get(not_allowed).delete(not_allowed)).with_state(state)
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
            let conn = b.serve_connection(TokioIo::new(stream), svc);
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

async fn post(State(h): State<Arc<Http>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let headers = &parts.headers;
    // 1-3: who, from where. Nothing of the body has been read.
    let auth = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    if !h.tools.0.admits(auth) {
        let mut r = refuse(
            StatusCode::UNAUTHORIZED,
            rpc::UNAUTHORIZED,
            "unauthorized: this door needs `Authorization: Bearer <secret>` (the environment's mcp/door.secret)",
            None,
        );
        r.headers_mut().insert("www-authenticate", HeaderValue::from_static("Bearer realm=\"cauldron-mcp\""));
        return r;
    }
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let allowed = [format!("{HOST}:{}", h.port), format!("localhost:{}", h.port)];
    if !allowed.iter().any(|a| a == host) {
        return refuse(StatusCode::FORBIDDEN, rpc::SERVER, "forbidden: the Host header must name this loopback door", None);
    }
    if headers.contains_key(header::ORIGIN) {
        return refuse(StatusCode::FORBIDDEN, rpc::SERVER, "forbidden: a request with an Origin (a web page) is never served here", None);
    }
    // 4: how much. A body without a declared length is not read at all.
    let Some(len) = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok()) else {
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
