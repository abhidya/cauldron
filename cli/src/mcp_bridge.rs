//! **`cauldron mcp stdio`**: the bridge Claude Code launches as the plugin's
//! MCP server.
//!
//! Claude Code speaks MCP's stdio transport: one JSON-RPC message per line on
//! the bridge's stdin, one per line back on its stdout. The bridge relays
//! each message to the environment's `mcp` service (`POST /mcp` on loopback,
//! with the door secret, see [`crate::mcp_door`]) and writes the service's
//! answer back as one line. Messages are answered as they finish, each on
//! its own thread, so one slow write does not hold up a read.
//!
//! **The context.** On every `tools/call` the bridge sets
//! `arguments._ctx` to what it knows and the service cannot:
//!
//! - `client`: `claude-code`;
//! - `cwd`: the bridge's working directory (Claude Code starts it in the
//!   session's project);
//! - `session_id`: `$CLAUDE_CODE_SESSION_ID`, when Claude Code sets it and
//!   it is a plain token;
//! - `cauldron_var`: `--cauldron`, else `$CAULDRON_CAULDRON`.
//!
//! Whatever `_ctx` a caller put in the arguments is REPLACED, never merged:
//! a model cannot name another directory, client or session through a tool
//! argument. (The service reads `_ctx` only from requests that carry the
//! door secret, which only the bridge and the owner's own CLI can read.)
//!
//! **The service down.** When the service is not running (no live seam row,
//! or its door refuses the connection), the bridge answers `initialize`,
//! `ping` and `tools/list` itself from the compiled catalogue
//! (`soup_door::catalogue`), so a session starts and sees every tool, and
//! answers every `tools/call` with the typed refusal `service_down` naming
//! the command that starts it. Nothing is read from a soup in the bridge:
//! there is one way to a soup, through the service. A service that goes
//! away while a call is in flight (the connection lost after the request
//! went out: killed, or crashed, mid-wait or mid-write) is answered the
//! same way, `service_down` with the start command; a call that writes is
//! also told that whether it landed is not known, and to look for it with
//! `soup_changes` since the session's last cursor before writing it again.
//!
//! **The environment** is `--env`, else `$CAULDRON_ENV`, else `prod`. Unlike
//! the other verbs, the bridge does NOT read `$ENV`: a session's shell often
//! carries a generic `ENV=development` that names no cauldron environment,
//! and the bridge must not refuse to start over it. When the environment
//! cannot be used (a bad name, or no such environment), the bridge still
//! answers `initialize`, `ping` and `tools/list`, and refuses every
//! `tools/call` as `no_environment`, saying why and how the environment is
//! chosen, so the session shows the reason instead of a failed server.
//!
//! **The secret** is read from the environment's `mcp/door.secret` for each
//! request and written only to the door's socket: never to stdout, stderr
//! or an error message.
//!
//! **`soup_dump_session`** is the one call the bridge does not pass on as it
//! is. The bridge runs as the user, beside the session, so it is the one that
//! finds the transcript: the session is the call's `session_id`, else this
//! session (`$CLAUDE_CODE_SESSION_ID`); the id must be a plain id (no path
//! separator, never `.` or `..`); and the transcript is looked for only as
//! `<config>/projects/*/<id>.jsonl`, `<config>` being `$CLAUDE_CONFIG_DIR`,
//! else `~/.claude`. No link is followed under `<config>/projects`: a linked
//! project directory or file, a file with a second name, or one whose real
//! path is not in the projects store is `transcript_link`, and a FIFO,
//! socket or device in its place is `transcript_unreadable`; in both cases
//! nothing of it is read. None there is `session_unknown`; a store or file
//! that cannot be read is `transcript_unreadable`, never "absent". A transcript
//! over 64 MiB is `too_large`, and nothing is sent. Otherwise the call goes
//! to the service's `/mcp/dump-session` route with the transcript's bytes
//! streamed after it: as the transcript stood when the bridge looked at it,
//! and when it has grown since (the session is still writing it), up to its
//! last whole line. The service reads no path.

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use env::Environment;
use serde_json::{json, Map, Value};
use shapewire::envelope::{codes, Identity, Refusal};
use shapewire::mcp::{answer, parse, Caller, Incoming, McpTools, ToolDef};
use soup_door::catalogue::{self, CTX_KEY};

/// The `client` the bridge names on every call.
pub const CLIENT: &str = "claude-code";

/// The variable Claude Code sets to the session's id.
pub const SESSION_VAR: &str = "CLAUDE_CODE_SESSION_ID";

/// The tool whose transcript the bridge finds and sends itself.
pub const DUMP_SESSION: &str = "soup_dump_session";

/// The refusal code for a session with no transcript on this machine.
pub const SESSION_UNKNOWN: &str = "session_unknown";

/// The refusal code for a transcript store or file that cannot be read.
pub const TRANSCRIPT_UNREADABLE: &str = "transcript_unreadable";

/// The refusal code for a transcript reached through a link (a linked
/// file or project directory, a second name, or a path leading outside the
/// projects store). Nothing of it is read.
pub const TRANSCRIPT_LINK: &str = "transcript_link";

/// The refusal code for a call made while the service is down.
pub const SERVICE_DOWN: &str = "service_down";

/// The refusal code for every call when the bridge's environment cannot be
/// used.
pub const NO_ENVIRONMENT: &str = "no_environment";

/// What the bridge knows about its caller, set on every call as `_ctx`.
#[derive(Debug, Clone, PartialEq)]
pub struct Ctx(pub Map<String, Value>);

fn token_ok(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl Ctx {
    /// From the process: its working directory, `$CLAUDE_CODE_SESSION_ID`,
    /// and `cauldron` (the flag) else `$CAULDRON_CAULDRON`.
    pub fn from_process(cauldron: Option<&str>) -> Ctx {
        Ctx::from_parts(
            std::env::current_dir().ok().filter(|d| d.is_absolute()).map(|d| d.display().to_string()),
            std::env::var(SESSION_VAR).ok(),
            cauldron.map(str::to_string).or_else(|| std::env::var("CAULDRON_CAULDRON").ok()),
        )
    }

    pub fn from_parts(cwd: Option<String>, session: Option<String>, cauldron: Option<String>) -> Ctx {
        let mut m = Map::new();
        m.insert("client".into(), json!(CLIENT));
        if let Some(c) = cwd {
            m.insert("cwd".into(), json!(c));
        }
        if let Some(s) = session.map(|s| s.trim().to_string()).filter(|s| token_ok(s, 128)) {
            m.insert("session_id".into(), json!(s));
        }
        if let Some(c) = cauldron.map(|c| c.trim().to_string()).filter(|c| !c.is_empty()) {
            m.insert("cauldron_var".into(), json!(c));
        }
        Ctx(m)
    }

    /// Set `_ctx` on a `tools/call` message, replacing any the caller sent.
    /// Any other message is left as it is.
    pub fn apply(&self, msg: &mut Value) {
        if msg.get("method").and_then(Value::as_str) != Some("tools/call") {
            return;
        }
        let Some(params) = msg.get_mut("params").and_then(Value::as_object_mut) else { return };
        let args = params.entry("arguments").or_insert_with(|| json!({}));
        if args.is_null() {
            *args = json!({});
        }
        // Arguments that are not an object are left for the service to refuse.
        if let Some(a) = args.as_object_mut() {
            a.insert(CTX_KEY.into(), Value::Object(self.0.clone()));
        }
    }
}

/// `cauldron mcp stdio [--env <env>] [--cauldron <name>]`.
pub fn stdio(args: &[String]) -> Result<i32> {
    let a = crate::mcp::parse("stdio", args, &["--env", "--cauldron"])?;
    if let Some(p) = a.pos.first() {
        bail!("`cauldron mcp stdio` takes no arguments (got `{p}`); `cauldron mcp help`");
    }
    // `--env`, else `$CAULDRON_ENV`, else `prod`; never `$ENV` (module
    // docs). Recording the name as this process's `--env` makes the
    // resolution below read it ahead of both variables. It fails only for a
    // bad name (refused just below, with the reason) or when a leading
    // `cauldron --env` already recorded it, which `$CAULDRON_ENV` mirrors.
    let name = a
        .env
        .clone()
        .or_else(|| std::env::var("CAULDRON_ENV").ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "prod".to_string());
    let _ = env::paths::set_env_flag(&name);
    let e = crate::mcp::environment(Some(&name)).map_err(|err| Unusable { name: name.clone(), why: format!("{err:#}") });
    if let Err(u) = &e {
        eprintln!("cauldron mcp stdio: {}", u.say());
    }
    let bridge = Arc::new(Bridge { e, ctx: Ctx::from_process(a.cauldron.as_deref()), out: Mutex::new(std::io::stdout()) });
    let mut threads = Vec::new();
    for line in std::io::stdin().lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(err) => {
                eprintln!("cauldron mcp stdio: reading stdin: {err}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let b = bridge.clone();
        threads.push(std::thread::spawn(move || b.handle(&line)));
        threads.retain(|t| !t.is_finished());
    }
    for t in threads {
        let _ = t.join();
    }
    Ok(0)
}

struct Bridge {
    e: std::result::Result<Environment, Unusable>,
    ctx: Ctx,
    out: Mutex<std::io::Stdout>,
}

/// The environment the bridge was asked for, and why it cannot be used.
#[derive(Debug, Clone)]
struct Unusable {
    name: String,
    why: String,
}

impl Unusable {
    fn say(&self) -> String {
        format!(
            "environment `{}` cannot be used: {}. The bridge's environment is `cauldron mcp stdio --env <env>`, else \
             $CAULDRON_ENV, else prod; $ENV is not read. Set CAULDRON_ENV where Claude Code is started (or pass --env \
             in the plugin's server command), or create the environment with `cauldron env create <env>`",
            self.name, self.why
        )
    }
}

impl Bridge {
    fn write(&self, v: &Value) {
        let mut out = self.out.lock().unwrap_or_else(|p| p.into_inner());
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    }

    fn handle(&self, line: &str) {
        let mut msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(err) => {
                self.write(&shapewire::mcp::error_body(Value::Null, shapewire::mcp::rpc::PARSE, format!("parse error: {err}"), None));
                return;
            }
        };
        // One request per line: a JSON-RPC batch (an array), or anything
        // else that is not an object, is answered here, never relayed.
        if !msg.is_object() {
            self.write(&shapewire::mcp::error_body(
                Value::Null,
                shapewire::mcp::rpc::INVALID_REQUEST,
                "invalid request: send one JSON-RPC message per line; a batch (an array) is not supported",
                None,
            ));
            return;
        }
        self.ctx.apply(&mut msg);
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let e = match &self.e {
            Ok(e) => e,
            Err(u) => {
                if let Some(v) = answer_unusable(u, &msg) {
                    self.write(&v);
                }
                return;
            }
        };
        let is_dump_session = msg.get("method").and_then(Value::as_str) == Some("tools/call")
            && msg.pointer("/params/name").and_then(Value::as_str) == Some(DUMP_SESSION);
        if is_dump_session && !id.is_null() {
            self.write(&dump_session(e, &self.ctx, &msg));
            return;
        }
        match relay(e, &msg) {
            Relayed::Answer(v) => {
                let mut v = back_off(e, &msg, v);
                skew(e, &msg, &mut v);
                self.write(&v)
            }
            Relayed::Nothing => {}
            Relayed::Down(why) => {
                if let Some(v) = answer_down(&e.name, &why, &msg) {
                    self.write(&v);
                }
            }
            Relayed::Lost(why) => {
                if let Some(v) = answer_lost(&e.name, &why, &msg) {
                    self.write(&v);
                }
            }
            Relayed::Failed(why) => {
                if !id.is_null() {
                    self.write(&shapewire::mcp::error_body(id, shapewire::mcp::rpc::SERVER, why, None));
                }
            }
        }
    }
}

enum Relayed {
    /// The service's answer, to write as it is.
    Answer(Value),
    /// A notification the service accepted: nothing to write.
    Nothing,
    /// The service is not there: the bridge answers itself.
    Down(String),
    /// The service went away while the call was in flight (its connection
    /// was lost after the request went out): the bridge answers itself, and
    /// says whether the call may have written.
    Lost(String),
    /// The service is there and the relay failed.
    Failed(String),
}

/// The waited part of a `soup_changes` answer that was not given a slot
/// (`waited.ended: "not-waited"`) and carries no change: `(retry_after_secs,
/// asked_secs)`.
fn not_waited(v: &Value) -> Option<(u64, u64)> {
    let doc = v.pointer("/result/structuredContent")?;
    // An answer that already carries changes is the answer: nothing to wait for.
    let empty = |k: &str| doc.get(k).and_then(Value::as_array).is_none_or(|a| a.is_empty());
    if !empty("changes") || !empty("dumps") {
        return None;
    }
    let w = doc.get("waited")?;
    (w.get("ended")?.as_str()? == "not-waited").then(|| (w.get("retry_after_secs").and_then(Value::as_u64).unwrap_or(1).max(1), w.get("asked_secs").and_then(Value::as_u64).unwrap_or(0)))
}

/// **A `soup_changes` wait the service had no slot for is waited here.**
/// The service answers a wait past its slots at once, with
/// `waited.ended: "not-waited"` and `retry_after_secs`. The bridge does not
/// hand that to the session (which would call again at once, and again):
/// it sleeps `retry_after_secs`, asks again, and doubles the sleep each time
/// it is told the same, until the call's own `wait_secs` has passed; then it
/// answers the last answer as it is. A wait it was given a slot for, and any
/// other answer, is written as it came.
fn back_off(e: &Environment, msg: &Value, first: Value) -> Value {
    let Some((mut retry, asked)) = not_waited(&first) else { return first };
    if msg.pointer("/params/name").and_then(Value::as_str) != Some("soup_changes") {
        return first;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(asked);
    let mut last = first;
    let mut tries = 0u32;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return last;
        }
        std::thread::sleep(left.min(std::time::Duration::from_secs(retry)));
        let left = deadline.saturating_duration_since(std::time::Instant::now()).as_secs();
        if left == 0 {
            return last;
        }
        // Ask again for what is left of the wait.
        let mut again = msg.clone();
        again["params"]["arguments"]["wait_secs"] = json!(left);
        tries += 1;
        match relay(e, &again) {
            Relayed::Answer(v) => match not_waited(&v) {
                Some((r, _)) => {
                    retry = r.max(retry.saturating_mul(2)).min(asked.max(1));
                    last = v;
                }
                None => return v,
            },
            // Anything else (the service went away meanwhile): the answer
            // already in hand stands.
            _ => return last,
        }
        if tries > 64 {
            return last;
        }
    }
}

/// Is `s` a `${NAME}` placeholder nobody substituted?
fn unsubstituted(s: &str) -> bool {
    let s = s.trim();
    s.starts_with("${") && s.ends_with('}')
}

fn relay(e: &Environment, msg: &Value) -> Relayed {
    let found = match crate::mcp_door::find(e) {
        Ok(f) => f,
        Err(why) => return Relayed::Down(why.say()),
    };
    let body = msg.to_string();
    let mut tries = 0;
    loop {
        tries += 1;
        let a = match crate::mcp_door::post(e, found.door, &body) {
            Ok(a) => a,
            Err(err) if crate::mcp_door::is_lost(&err) => return Relayed::Lost(format!("{err:#}")),
            Err(err) if is_refused(&err) => return Relayed::Down(format!("its door refused the connection ({err:#})")),
            Err(err) => return Relayed::Failed(format!("relaying to the mcp service: {err:#}")),
        };
        if a.status == 202 {
            return Relayed::Nothing;
        }
        // A restarted service mints a new secret: read it again once.
        if a.status == 401 && tries == 1 {
            continue;
        }
        let Ok(mut v) = serde_json::from_str::<Value>(&a.body) else {
            return Relayed::Failed(format!("the mcp service answered HTTP {} with no JSON", a.status));
        };
        // A transport refusal has no request id: give it the request's.
        if v.get("error").is_some() && v.get("id").is_none_or(Value::is_null) {
            v["id"] = msg.get("id").cloned().unwrap_or(Value::Null);
        }
        if msg.get("id").is_none_or(Value::is_null) {
            return Relayed::Nothing;
        }
        return Relayed::Answer(v);
    }
}

/// **Version skew, made visible.** The service answering is another build
/// than this bridge when the owner upgraded cauldron and the environment's
/// `mcp` pin still names the old one. On `initialize`, the service's version
/// (`serverInfo.version`) is compared with the bridge's: when they differ,
/// the session's instructions say so, with the commands that upgrade it,
/// and so does a line on stderr (Claude Code's log of this server). A
/// refusal that says the service does not know a tool or an argument the
/// call used is annotated the same way ([`crate::mcp_door::annotate_skew`]).
fn skew(e: &Environment, msg: &Value, v: &mut Value) {
    match msg.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            let Some(sv) = v.pointer("/result/serverInfo/version").and_then(Value::as_str).map(str::to_string) else { return };
            if let Some(note) = crate::mcp_door::skew_note(&sv, &e.name) {
                eprintln!("cauldron mcp stdio: {note}.");
                if let Some(r) = v.get_mut("result").and_then(Value::as_object_mut) {
                    let before = r.get("instructions").and_then(Value::as_str).unwrap_or("").to_string();
                    let sep = if before.is_empty() { "" } else { " " };
                    r.insert("instructions".into(), json!(format!("{before}{sep}VERSION SKEW: {note}.")));
                }
            }
        }
        Some("tools/call") if crate::mcp_door::is_unknown_to_service(v) => {
            if let Some(sv) = crate::mcp_door::find(e).ok().and_then(|f| crate::mcp_door::service_version(e, f.door)) {
                crate::mcp_door::annotate_skew(v, &sv, &e.name);
            }
        }
        _ => {}
    }
}

fn is_refused(err: &anyhow::Error) -> bool {
    err.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|io| matches!(io.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset))
    })
}

/// The tools the bridge answers with itself (the service is down, or the
/// environment cannot be used): the same catalogue, and one refusal for
/// every call.
struct Down {
    refusal: Refusal,
    instructions: String,
}

impl Down {
    /// The service is down.
    fn service(env: &str, why: &str) -> Down {
        Down {
            refusal: Refusal::new(
                SERVICE_DOWN,
                format!(
                    "the `mcp` service of environment `{env}` is not running, so no cauldron can be reached: {why}. \
                     Nothing was read or written. The owner starts it from a terminal with `{}`.",
                    crate::mcp_door::start_command(env)
                ),
            ),
            instructions: format!(
                "This is cauldron's MCP door to the soups of environment `{env}`, answered by its stdio bridge because the \
                 `mcp` service is not running: every tool call is refused as `service_down` until the owner starts it \
                 (`{}`). Every cauldron is closed to the door until its owner grants it from a terminal (`cauldron mcp \
                 grant`). A refusal is a tool result with isError and a typed code: read it; it is never an absence.",
                crate::mcp_door::start_command(env)
            ),
        }
    }

    /// The service went away while the call `tool` was in flight. The same
    /// code as the service being down (`service_down`, with the command that
    /// starts it); a call that writes is told plainly that whether it landed
    /// is not known, and how to find out.
    fn lost(env: &str, why: &str, tool: Option<&str>) -> Down {
        let mut d = Down::service(env, why);
        let start = crate::mcp_door::start_command(env);
        let writes = tool.and_then(catalogue::find).is_some_and(|t| {
            matches!(t.needs, catalogue::Needs::Scope(soup_door::grants::Scope::Write) | catalogue::Needs::Scope(soup_door::grants::Scope::Dump))
        });
        let message = if writes {
            let again = if tool == Some("soup_put") {
                " A soup_put sent again unchanged is refused as `batch_already_landed` if the first one landed."
            } else {
                ""
            };
            format!(
                "the `mcp` service of environment `{env}` stopped answering while this call was in flight ({why}): it was \
                 stopped or it crashed. WHETHER THIS WRITE LANDED IS NOT KNOWN: a write lands whole or not at all, never in \
                 part, and the service went away before it said which. The owner starts the service again from a terminal \
                 with `{start}`. Then, before writing it again, call soup_changes with `since` set to the last cursor this \
                 session had (from soup_status, soup_put, soup_link, soup_claim, soup_dump or an earlier soup_changes) and \
                 look for this write there, or read back what it would have written (soup_get, soup_dumps).{again}"
            )
        } else {
            format!(
                "the `mcp` service of environment `{env}` stopped answering while this call was in flight ({why}): it was \
                 stopped or it crashed. This call only reads, so nothing was written. The owner starts the service again \
                 from a terminal with `{start}`; then call again."
            )
        };
        d.refusal = Refusal::new(SERVICE_DOWN, message);
        d
    }

    /// The environment cannot be used.
    fn unusable(u: &Unusable) -> Down {
        Down {
            refusal: Refusal::new(NO_ENVIRONMENT, format!("{}. Nothing was read or written.", u.say())),
            instructions: format!(
                "This is cauldron's MCP door, answered by its stdio bridge, which cannot reach any cauldron: {}. Every \
                 tool call is refused as `no_environment` until that is fixed. A refusal is a tool result with isError \
                 and a typed code: read it; it is never an absence.",
                u.say()
            ),
        }
    }

    fn refusal(&self) -> Refusal {
        self.refusal.clone()
    }
}

impl McpTools for Down {
    fn verify(&self, _authorization: Option<&str>) -> std::result::Result<Caller, Refusal> {
        Ok(Caller { thread_id: "mcp-bridge".into(), run_id: String::new(), root: String::new(), depth: 0, turn: String::new() })
    }

    fn tools(&self) -> Vec<ToolDef> {
        catalogue::defs()
    }

    fn instructions(&self, _caller: &Caller) -> Option<String> {
        Some(self.instructions.clone())
    }

    fn call<'a>(
        &'a self,
        _caller: &'a Caller,
        name: &'a str,
        _arguments: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::result::Result<Value, Refusal>> + Send + 'a>> {
        let r = if catalogue::find(name).is_some() { self.refusal() } else { Refusal::new(codes::UNKNOWN_METHOD, name.to_string()) };
        Box::pin(async move { Err(r) })
    }
}

/// The bridge's own answer to `msg` while the service is down (`None` for a
/// notification).
fn answer_down(env: &str, why: &str, msg: &Value) -> Option<Value> {
    answer_locally(Down::service(env, why), msg)
}

/// The bridge's own answer to `msg` when the service went away while it was
/// in flight (`None` for a notification).
fn answer_lost(env: &str, why: &str, msg: &Value) -> Option<Value> {
    let tool = (msg.get("method").and_then(Value::as_str) == Some("tools/call")).then(|| msg.pointer("/params/name").and_then(Value::as_str)).flatten();
    answer_locally(Down::lost(env, why, tool), msg)
}

/// The bridge's own answer to `msg` when its environment cannot be used.
fn answer_unusable(u: &Unusable, msg: &Value) -> Option<Value> {
    answer_locally(Down::unusable(u), msg)
}

fn answer_locally(tools: Down, msg: &Value) -> Option<Value> {
    let identity = Identity { daemon: "cauldron-mcp".into(), version: env!("CARGO_PKG_VERSION").into(), proto: 1, boot_id: "bridge".into() };
    let caller = tools.verify(None).ok()?;
    let incoming = match parse(msg.to_string().as_bytes()) {
        Ok(i) => i,
        Err(e) => return Some(e.body()),
    };
    if matches!(incoming, Incoming::Notification { .. }) {
        return None;
    }
    let reply = block_on(answer(&tools, &identity, &caller, incoming, || format!("bridge-{}", std::process::id())));
    reply.body
}

/// Run a future that never waits (every future here is ready at once).
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bridge backs off only from a `not-waited` answer that carries no
    /// change: one that already holds changes is the answer.
    #[test]
    fn only_an_empty_not_waited_answer_is_waited_again() {
        let answer = |changes: Value, ended: &str| json!({"result": {"structuredContent": {"changes": changes, "dumps": [],
            "waited": {"asked_secs": 20, "ended": ended, "retry_after_secs": 3}}}});
        assert_eq!(not_waited(&answer(json!([]), "not-waited")), Some((3, 20)));
        assert_eq!(not_waited(&answer(json!([{"seq": 4}]), "not-waited")), None);
        assert_eq!(not_waited(&answer(json!([]), "timeout")), None);
    }

    #[test]
    fn the_bridge_replaces_any_ctx_a_caller_sends() {
        let ctx = Ctx::from_parts(Some("/work/p".into()), Some("sess-1".into()), Some(" kb ".into()));
        let mut m = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "soup_status",
            "arguments": {"cauldron": "x", "_ctx": {"client": "evil", "cwd": "/elsewhere", "session_id": "forged"}}}});
        ctx.apply(&mut m);
        assert_eq!(m["params"]["arguments"]["_ctx"], json!({"client": "claude-code", "cwd": "/work/p", "session_id": "sess-1", "cauldron_var": "kb"}));
        assert_eq!(m["params"]["arguments"]["cauldron"], "x");
        // No arguments, null arguments: an object with the context.
        for args in [None, Some(Value::Null)] {
            let mut m = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "soup_status"}});
            if let Some(a) = args {
                m["params"]["arguments"] = a;
            }
            ctx.apply(&mut m);
            assert_eq!(m["params"]["arguments"]["_ctx"]["client"], "claude-code");
        }
        // Not a call: untouched.
        let mut m = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {"_ctx": 1}});
        let before = m.clone();
        ctx.apply(&mut m);
        assert_eq!(m, before);
        // A session id that is not a plain token is left out, never passed on.
        let c = Ctx::from_parts(None, Some("../../x".into()), None);
        assert_eq!(Value::Object(c.0), json!({"client": "claude-code"}));
    }

    #[test]
    fn with_the_service_down_the_bridge_lists_every_tool_and_refuses_every_call() {
        let init = answer_down("dev", "not running", &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}})).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(init["result"]["capabilities"], json!({"tools": {}}));
        let list = answer_down("dev", "not running", &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, catalogue::TOOLS.iter().map(|t| t.name).collect::<Vec<_>>());
        let call = answer_down("dev", "not running", &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "soup_status", "arguments": {}}})).unwrap();
        assert_eq!(call["result"]["isError"], json!(true));
        assert_eq!(call["result"]["structuredContent"]["refused"]["code"], SERVICE_DOWN);
        assert!(call["result"]["structuredContent"]["refused"]["message"].as_str().unwrap().contains("cauldron --env dev start mcp"));
        let unknown = answer_down("dev", "x", &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "nope"}})).unwrap();
        assert_eq!(unknown["error"]["code"], json!(-32602));
        assert!(answer_down("dev", "x", &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "cauldron-bridge-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(p.join("cc").join("projects").join("-work-a")).unwrap();
        std::fs::create_dir_all(p.join("outside").join("-work-a")).unwrap();
        std::fs::write(p.join("cc/projects/-work-a/sess-1.jsonl"), b"{\"inside\":1}\n").unwrap();
        std::fs::write(p.join("outside/-work-a/sess-1.jsonl"), b"OUTSIDE\n").unwrap();
        std::fs::write(p.join("outside/plain.txt"), b"OUTSIDE\n").unwrap();
        p.canonicalize().unwrap()
    }

    /// A link put in place of the transcript after it was found, and a
    /// project directory swapped for a link to a directory holding a file of
    /// the same name: both are refused at the open, nothing read. A device
    /// is not a regular file.
    #[test]
    fn a_link_swapped_in_between_the_look_and_the_open_is_refused() {
        let root = scratch("swap");
        let config = root.join("cc");
        let file = config.join("projects/-work-a/sess-1.jsonl");
        assert_eq!(locate_transcript(&config, "sess-1"), Ok(Some(file.clone())));
        assert!(open_transcript(&config, &file).is_ok(), "the plain file opens");

        // The file swapped for a link after the look.
        let found = locate_transcript(&config, "sess-1").unwrap().unwrap();
        std::fs::rename(&file, root.join("kept.jsonl")).unwrap();
        std::os::unix::fs::symlink(root.join("outside/plain.txt"), &file).unwrap();
        assert!(matches!(open_transcript(&config, &found), Err(Transcript::Link(_))), "a linked file is not followed");
        std::fs::remove_file(&file).unwrap();
        std::fs::rename(root.join("kept.jsonl"), &file).unwrap();

        // The project directory swapped for a link after the look.
        let found = locate_transcript(&config, "sess-1").unwrap().unwrap();
        let slug = config.join("projects/-work-a");
        std::fs::rename(&slug, root.join("kept-dir")).unwrap();
        std::os::unix::fs::symlink(root.join("outside/-work-a"), &slug).unwrap();
        match open_transcript(&config, &found) {
            Err(Transcript::Link(why)) => assert!(why.contains("leads outside"), "{why}"),
            other => panic!("a linked project directory was followed: {other:?}"),
        }
        // And seen as a link by the look itself.
        assert!(matches!(locate_transcript(&config, "sess-1"), Err(Transcript::Link(_))));

        // A device in the file's place (the look refuses a non-regular file,
        // and so does the open on its own handle).
        assert!(matches!(open_transcript(&config, std::path::Path::new("/dev/null")), Err(Transcript::NotRegular(_))));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An older service's `initialize`: the session's instructions and a
    /// stderr line name both versions and the upgrade; the same version
    /// leaves the answer as the service gave it.
    #[test]
    fn an_older_service_s_initialize_tells_the_session_about_the_skew() {
        let e = Environment {
            root: "/nonexistent-root".into(),
            root_source: env::paths::Source::CauldronHome,
            name: "prod".into(),
            name_source: env::paths::Source::CauldronEnv,
            data_root: "/nonexistent-root/data/prod".into(),
        };
        let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        let answer = |v: &str| json!({"jsonrpc": "2.0", "id": 1, "result": {"serverInfo": {"name": "cauldron-mcp", "version": v}, "instructions": "The door."}});
        let mut old = answer("0.0.1");
        skew(&e, &msg, &mut old);
        let i = old["result"]["instructions"].as_str().unwrap();
        assert!(i.starts_with("The door. VERSION SKEW: "), "{i}");
        for want in ["is cauldron 0.0.1", crate::mcp_door::CLI_VERSION, "scripts/install --replace-running", "cauldron --env prod build mcp --env-copy"] {
            assert!(i.contains(want), "{want}: {i}");
        }
        let mut same = answer(crate::mcp_door::CLI_VERSION);
        let before = same.clone();
        skew(&e, &msg, &mut same);
        assert_eq!(same, before);
    }

    #[test]
    fn with_an_unusable_environment_the_bridge_still_lists_every_tool_and_says_why() {
        let u = Unusable { name: "prod".into(), why: "there is no environment `prod`".into() };
        let init = answer_unusable(&u, &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}})).unwrap();
        assert!(init["result"]["instructions"].as_str().unwrap().contains("$ENV is not read"), "{init}");
        let list = answer_unusable(&u, &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), catalogue::TOOLS.len());
        let call = answer_unusable(&u, &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "soup_status", "arguments": {}}})).unwrap();
        let r = &call["result"]["structuredContent"]["refused"];
        assert_eq!(r["code"], NO_ENVIRONMENT);
        assert!(r["message"].as_str().unwrap().contains("there is no environment `prod`") && r["message"].as_str().unwrap().contains("CAULDRON_ENV"), "{r}");
    }
}

// ------------------------------------------------------- soup_dump_session

fn refused_answer(id: Value, code: &'static str, message: String) -> Value {
    shapewire::mcp::result_body(id, shapewire::mcp::tool_refused(&Refusal::new(code, message)))
}

/// The Claude Code config directory: `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn claude_config_dir() -> Option<std::path::PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        Some(d) => Some(std::path::PathBuf::from(d)),
        None => std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|h| std::path::PathBuf::from(h).join(".claude")),
    }
}

/// Why a transcript cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transcript {
    /// Something that could hold it could not be read (`transcript_unreadable`).
    Unreadable(String),
    /// It is reached through a link, has a second name, or leads outside
    /// the projects store (`transcript_link`). Nothing of it was read.
    Link(String),
    /// It is not a regular file: a FIFO, a socket, a device or a directory
    /// (`transcript_unreadable`). Nothing of it was read.
    NotRegular(String),
}

/// **Find `session`'s transcript** under `<config>/projects/*/<session>.jsonl`.
/// `Ok(None)`: the store was read and holds none (or does not exist).
/// `Err`: something that could hold it could not be read, and nothing
/// readable was found elsewhere; or a candidate is a link (a linked project
/// directory, or a linked file) or not a regular file, which refuses the
/// whole look whatever else is there. Every component under the projects
/// store is looked at with `lstat`, so no link is followed. Two copies (the
/// session moved directory): the larger. `session` must already be a plain
/// id. The projects store itself (`<config>/projects`) may be a link; it is
/// taken as where it leads.
pub fn locate_transcript(config: &std::path::Path, session: &str) -> std::result::Result<Option<std::path::PathBuf>, Transcript> {
    debug_assert!(soup_door::dumps::session_id_ok(session));
    let file = format!("{session}.jsonl");
    let projects = config.join("projects");
    let unreadable = |p: &std::path::Path, e: std::io::Error| Transcript::Unreadable(format!("{}: {e}", p.display()));
    let entries = match std::fs::read_dir(&projects) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(unreadable(&projects, e)),
    };
    let mut found: Option<(u64, std::path::PathBuf)> = None;
    let mut skipped: Option<Transcript> = None;
    for entry in entries {
        let entry = match entry {
            Ok(x) => x,
            Err(e) => {
                skipped.get_or_insert(unreadable(&projects, e));
                continue;
            }
        };
        let slug = entry.path();
        let candidate = slug.join(&file);
        // The entry's own type: `file_type` does not follow a link.
        let kind = match entry.file_type() {
            Ok(k) => k,
            Err(e) => {
                skipped.get_or_insert(unreadable(&slug, e));
                continue;
            }
        };
        if kind.is_symlink() {
            // A linked project directory that leads to a file of this name:
            // refused. Only the name is looked up; nothing is opened.
            if std::fs::symlink_metadata(&candidate).is_ok() {
                return Err(Transcript::Link(format!("the project directory {} is a link", slug.display())));
            }
            continue;
        }
        if !kind.is_dir() {
            continue;
        }
        match std::fs::symlink_metadata(&candidate) {
            Ok(md) if md.file_type().is_symlink() => return Err(Transcript::Link(format!("{} is a link", candidate.display()))),
            Ok(md) if md.file_type().is_file() => {
                if found.as_ref().is_none_or(|(n, _)| md.len() > *n) {
                    found = Some((md.len(), candidate));
                }
            }
            Ok(_) => return Err(Transcript::NotRegular(format!("{} is not a regular file", candidate.display()))),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => {}
            Err(e) => {
                skipped.get_or_insert(unreadable(&candidate, e));
            }
        }
    }
    match (found, skipped) {
        (Some((_, p)), _) => Ok(Some(p)),
        (None, Some(e)) => Err(e),
        (None, None) => Ok(None),
    }
}

/// **Open the transcript [`locate_transcript`] found**, as it stands now:
/// with no-follow (a link put in its place since it was found is not
/// followed, and opening a FIFO does not wait), then on the open handle a
/// regular file with one name, and its canonical path a file in a project
/// directory of the canonical projects store whose device and inode are the
/// open handle's. Anything else is refused and nothing is read.
pub fn open_transcript(config: &std::path::Path, path: &std::path::Path) -> std::result::Result<std::fs::File, Transcript> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let unreadable = |p: &std::path::Path, e: std::io::Error| Transcript::Unreadable(format!("{}: {e}", p.display()));
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) => Transcript::Link(format!("{} is a link", path.display())),
            _ => unreadable(path, e),
        })?;
    let md = f.metadata().map_err(|e| unreadable(path, e))?;
    if !md.file_type().is_file() {
        return Err(Transcript::NotRegular(format!("{} is not a regular file", path.display())));
    }
    if md.nlink() > 1 {
        return Err(Transcript::Link(format!("{} has more than one name (a hard link)", path.display())));
    }
    let projects = config.join("projects");
    let root = std::fs::canonicalize(&projects).map_err(|e| unreadable(&projects, e))?;
    let real = std::fs::canonicalize(path).map_err(|e| unreadable(path, e))?;
    if real.parent().and_then(std::path::Path::parent) != Some(root.as_path()) {
        return Err(Transcript::Link(format!("{} leads outside {}", path.display(), projects.display())));
    }
    let now = std::fs::symlink_metadata(&real).map_err(|e| unreadable(&real, e))?;
    if now.dev() != md.dev() || now.ino() != md.ino() {
        return Err(Transcript::Link(format!("{} was replaced while it was opened", path.display())));
    }
    Ok(f)
}

/// How many bytes of a transcript to send: all of it, unless it grew since
/// `len` was taken and does not end on a line there, then up to its last
/// whole line.
fn whole_lines(f: &mut std::fs::File, len: u64) -> std::io::Result<u64> {
    use std::io::{Read, Seek, SeekFrom};
    if len == 0 || f.metadata()?.len() <= len {
        return Ok(len);
    }
    let mut end = len;
    let mut buf = vec![0u8; 64 * 1024];
    while end > 0 {
        let start = end.saturating_sub(buf.len() as u64);
        let n = (end - start) as usize;
        f.seek(SeekFrom::Start(start))?;
        f.read_exact(&mut buf[..n])?;
        if let Some(i) = buf[..n].iter().rposition(|b| *b == b'\n') {
            return Ok(start + i as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

/// **`soup_dump_session`, the bridge's half** (module docs). Answers the
/// JSON-RPC message to write back.
fn dump_session(e: &Environment, ctx: &Ctx, msg: &Value) -> Value {
    use std::io::{Seek, SeekFrom};
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let found = match crate::mcp_door::find(e) {
        Ok(f) => f,
        Err(why) => return answer_down(&e.name, &why.say(), msg).unwrap_or(Value::Null),
    };
    let mut msg = msg.clone();
    let Some(args) = msg.pointer_mut("/params/arguments").and_then(Value::as_object_mut) else {
        return refused_answer(id, "usage", "the arguments of soup_dump_session must be an object. Nothing was read or written.".into());
    };
    let session = match args.get("session_id") {
        // A `${...}` left as it was written (a command whose variable was
        // not substituted) names no session: this session's id is used.
        Some(Value::String(s)) if !s.trim().is_empty() && !unsubstituted(s) => Some(s.trim().to_string()),
        None | Some(Value::Null) | Some(Value::String(_)) => ctx.0.get("session_id").and_then(Value::as_str).map(str::to_string),
        Some(_) => return refused_answer(id, "usage", "`session_id` of soup_dump_session must be a string. Nothing was read or written.".into()),
    };
    let Some(session) = session else {
        return refused_answer(
            id,
            SESSION_UNKNOWN,
            "no session was named and this session's id is not known here: pass `session_id` (`/cauldron:dump-session` passes \
             it for you). Nothing was read or written."
                .into(),
        );
    };
    if !soup_door::dumps::session_id_ok(&session) {
        return refused_answer(
            id,
            "usage",
            format!("`{}` is not a session id: an id is 1-128 characters of letters, digits, `.`, `_` and `-`, not starting with `.`; never a path. Nothing was read or written.", session.chars().take(80).collect::<String>()),
        );
    }
    let Some(config) = claude_config_dir() else {
        return refused_answer(id, TRANSCRIPT_UNREADABLE, "neither $CLAUDE_CONFIG_DIR nor $HOME is set, so there is no Claude Code store to look in. Nothing was read or written.".into());
    };
    let refuse_unusable = |u: Transcript| match u {
        Transcript::Unreadable(why) => refused_answer(
            id.clone(),
            TRANSCRIPT_UNREADABLE,
            format!("session `{session}`'s transcript could not be looked for or read: {why}. Nothing was written."),
        ),
        Transcript::NotRegular(why) => refused_answer(
            id.clone(),
            TRANSCRIPT_UNREADABLE,
            format!("session `{session}`'s transcript is not a plain file: {why}. Nothing was read or written."),
        ),
        Transcript::Link(why) => refused_answer(
            id.clone(),
            TRANSCRIPT_LINK,
            format!(
                "a link was found where session `{session}`'s transcript is looked for: {why}. The bridge reads a transcript only \
                 as a plain file in a project directory of {}/projects, and follows no link. Nothing was read or written.",
                config.display()
            ),
        ),
    };
    let path = match locate_transcript(&config, &session) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return refused_answer(
                id,
                SESSION_UNKNOWN,
                format!(
                    "no Claude Code transcript of session `{session}` is on this machine (looked in {}/projects). Nothing was read or written.",
                    config.display()
                ),
            )
        }
        Err(u) => return refuse_unusable(u),
    };
    let mut f = match open_transcript(&config, &path) {
        Ok(f) => f,
        Err(u) => return refuse_unusable(u),
    };
    let unreadable = |e: std::io::Error| refused_answer(id.clone(), TRANSCRIPT_UNREADABLE, format!("session `{session}`'s transcript could not be read: {e}. Nothing was written."));
    let len = match f.metadata().and_then(|m| { let l = m.len(); whole_lines(&mut f, l) }) {
        Ok(l) => l,
        Err(err) => return unreadable(err),
    };
    if len > soup_door::dumps::TRANSCRIPT_LIMIT {
        return refused_answer(
            id,
            "too_large",
            format!(
                "session `{session}`'s transcript is {len} bytes, over the {} bytes (64 MiB) one session dump holds. Nothing was sent or written.",
                soup_door::dumps::TRANSCRIPT_LIMIT
            ),
        );
    }
    args.insert("session_id".into(), json!(session));
    let line = msg.to_string();
    if line.len() > soup_door::catalogue::SESSION_HEAD_LIMIT {
        return refused_answer(id, "usage", "the call is too long (its title?). Nothing was read or written.".into());
    }
    for tries in 1..=2 {
        if let Err(err) = f.seek(SeekFrom::Start(0)) {
            return unreadable(err);
        }
        let a = match crate::mcp_door::post_stream(e, found.door, soup_door::catalogue::SESSION_PATH, &line, &mut f, len) {
            Ok(a) => a,
            Err(err) if crate::mcp_door::is_lost(&err) => return answer_lost(&e.name, &format!("{err:#}"), &msg).unwrap_or(Value::Null),
            Err(err) if is_refused(&err) => return answer_down(&e.name, &format!("its door refused the connection ({err:#})"), &msg).unwrap_or(Value::Null),
            Err(err) => return shapewire::mcp::error_body(id, shapewire::mcp::rpc::SERVER, format!("sending the transcript to the mcp service: {err:#}"), None),
        };
        if a.status == 401 && tries == 1 {
            continue;
        }
        let mut v = match serde_json::from_str::<Value>(&a.body) {
            Ok(mut v) => {
                if v.get("error").is_some() && v.get("id").is_none_or(Value::is_null) {
                    v["id"] = id.clone();
                }
                v
            }
            Err(_) => shapewire::mcp::error_body(id.clone(), shapewire::mcp::rpc::SERVER, format!("the mcp service answered HTTP {} with no JSON", a.status), None),
        };
        // A service older than session dumps has no such route (404).
        if a.status == 404 || crate::mcp_door::is_unknown_to_service(&v) {
            if let Some(sv) = crate::mcp_door::service_version(e, found.door) {
                if a.status == 404 && crate::mcp_door::skew_note(&sv, &e.name).is_some() {
                    v = shapewire::mcp::error_body(id.clone(), shapewire::mcp::rpc::INVALID_PARAMS, format!("unknown tool: {DUMP_SESSION}"), None);
                }
                crate::mcp_door::annotate_skew(&mut v, &sv, &e.name);
            }
        }
        return v;
    }
    shapewire::mcp::error_body(id, shapewire::mcp::rpc::SERVER, "the mcp service refused the door secret twice", None)
}
