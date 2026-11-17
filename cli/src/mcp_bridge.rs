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
//! there is one way to a soup, through the service.
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
        match relay(e, &msg) {
            Relayed::Answer(v) => self.write(&v),
            Relayed::Nothing => {}
            Relayed::Down(why) => {
                if let Some(v) = answer_down(&e.name, &why, &msg) {
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
    /// The service is there and the relay failed.
    Failed(String),
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
