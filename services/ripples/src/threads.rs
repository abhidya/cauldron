//! **The `threads` backend**: one structured-output call through the
//! `threads` service, which runs it as a Claude Code child (`claude -p`) on
//! the owner's own Claude login, with its accounts, budgets and retries.
//!
//! **The call is a bare completion, never an agent.** The thread is created
//! with threads' `completion` setting, so the `claude` it runs has no tool of
//! any kind (`--tools ""`, a deny list of every built-in tool and of threads'
//! own MCP server, and no allow list), no MCP server (`--strict-mcp-config`
//! with an empty config: threads mints no run token and writes no config
//! file for it), no settings file, plugin, hook, skill or CLAUDE.md
//! (`--setting-sources ""`, `--safe-mode`, `--disable-slash-commands`, no
//! `--plugin-dir`),
//! permission mode `dontAsk` (never a skip-permissions mode), no session
//! file (`--no-session-persistence`), a bounded answer
//! ([`MAX_OUTPUT_TOKENS`], as `CLAUDE_CODE_MAX_OUTPUT_TOKENS`), and an
//! environment stripped to what `claude` needs to run and log in. The
//! request also says so in threads' own older vocabulary (`tools.clean_slate`,
//! an empty `allowed_tools`, `disallowed_tools`, `permission_mode`, no
//! plugin roots or directories, every injection off), and the message is
//! sent `bare`, so the model reads exactly the prompt. One turn: the schema
//! gets no corrective turn in threads (`schema_retries: 0`); the engine's
//! own retry is a second, counted call.
//!
//! **A threads service that cannot do this is refused before anything is
//! sent.** An older threads ignores settings it does not know, so after
//! `threads/create` the backend reads the thread back (`threads/list`) and
//! goes on only when the stored settings carry the `completion` bound it
//! asked for and permission mode `dontAsk`; otherwise the call fails as
//! [`Failure::Unsupported`] with no message sent.
//!
//! The wire is the threads chassis's websocket (`ws://<addr>/ws`), JSON
//! frames, one request and one response each, every request bounded by a
//! deadline:
//!
//! 1. the server's `welcome`; when it says `auth_required`, an `auth` frame
//!    with the threads secret (`$THREADS_SECRET`, else `secret` in
//!    `$THREADS_HOME/config.toml` or `~/.threads/config.toml`);
//! 2. `threads/create` with the settings above, the run's private scratch
//!    directory as the working directory, and a budget key when
//!    `$CAULDRON_RIPPLES_BUDGET_KEY` names one (else threads' default key);
//!    then `threads/list` to confirm them;
//! 3. `threads/<id>/send` with the prompt as the text (`bare`) and the
//!    schema in the message's metadata (`cauldron.threads.json_schema`,
//!    `schema_retries: 0`); the thread and message ids go to the run's
//!    journal at once ([`Request::sink`], event `sent`);
//! 4. `threads/<id>/history`, read until the judged answer
//!    (`cauldron.threads/structured_output`: `ok`, or `unmet`), a park
//!    (`turn_parked`, `budget_tripped`), a turn that ended without an answer
//!    (`turn_failed`, `turn_interrupted`, `turn_killed`, `budgets_failed`,
//!    `retry_starving`, `limit_hit`), a re-delivery threads schedules
//!    (`retry_scheduled`: a park for a usage limit, a spent credit balance or
//!    an expired login, a failure for anything else; a ripple's call is one
//!    delivery) or the time limit
//!    (`$CAULDRON_RIPPLES_CALL_TIMEOUT_S`, default 900). Token counts are the
//!    `message_usage` events, each API message once, until a `result_turn`
//!    reports the turn's totals (`model_usage`), which replace them (an API
//!    message's `message_usage` is its first streamed snapshot, before its
//!    answer); the cost is the `result_turn`s' `total_cost_usd`.
//!
//! **Giving up cancels.** On every exit but an answer, the backend asks
//! threads to stop the turn (`threads/<id>/cancel`, which puts the message
//! back on the queue) and to withdraw the message (`queue_cancel`, so no
//! budget reset or retry runs it again), then reads the thread until it is
//! idle. The result (`confirmed`, or not with how to stop it) goes to the
//! journal (event `cancel`) and into the run's outcome. A run cut short by
//! a kill is cancelled the same way when the service next starts or runs
//! ([`Inference::cancel`], from the journal's `sent` line).
//!
//! Where threads is: `$CAULDRON_RIPPLES_THREADS_ADDR` (`host:port`), else
//! the address the environment's threads is launched on, found as the face
//! finds it (a registering threads' seam row, else `[services.threads]
//! addr` in the environment's `env.toml`, else the declaration's `[launch]
//! addr`). The dial goes through `env::egress::connect_tcp`.

use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::inference::{Failure, Inference, Reply, Request, Usage};

/// The metadata key threads reads a message's schema from.
pub const METADATA_KEY: &str = "cauldron.threads";
/// The event threads writes a judged structured answer as.
pub const STRUCTURED_EVENT: &str = "cauldron.threads/structured_output";
/// The corrective turns threads may take when an answer misses the schema:
/// none, so one call is one model turn.
pub const SCHEMA_RETRIES: u64 = 0;
/// The most tokens one answer may have.
pub const MAX_OUTPUT_TOKENS: u32 = 16_000;
/// The permission mode the call runs in: every tool use not pre-approved is
/// denied, and nothing is pre-approved.
pub const PERMISSION_MODE: &str = "dontAsk";
/// The tools the request denies by name (threads adds its own list too).
pub const DENIED_TOOLS: &[&str] = &[
    "Agent", "Task", "Bash", "BashOutput", "KillShell", "Edit", "MultiEdit", "Write", "Read", "NotebookEdit", "Glob", "Grep", "LS", "WebFetch", "WebSearch",
    "TodoWrite", "Skill", "SlashCommand", "ExitPlanMode", "mcp__threads",
];
/// threads' prompt injections, each turned off by name.
pub const INJECTIONS: &[&str] = &[
    "inject_first_turn_preamble",
    "inject_compaction_restatement",
    "inject_plugin_changes",
    "inject_system_notices",
    "inject_application_headers",
    "inject_thread_id",
    "inject_context_id",
    "inject_message_id",
    "inject_app_idx",
    "inject_timestamp",
    "inject_duration",
    "inject_queue_lead_in",
];
/// Turn endings that give no answer: the call is over.
const ENDED: &[&str] = &["turn_failed", "turn_interrupted", "turn_killed", "budgets_failed", "retry_starving", "limit_hit"];
/// The longest one request may wait for its response.
const REQUEST_DEADLINE: Duration = Duration::from_secs(30);
/// How long a cancel waits to see the thread idle (or the call's whole
/// time limit when that is shorter).
const CANCEL_CONFIRM: Duration = Duration::from_secs(15);

pub struct Threads {
    /// `host:port` of the threads chassis.
    pub addr: String,
    secret: Option<String>,
    budget_key: Option<String>,
    timeout: Duration,
}

/// Where the environment's threads listens (see the module docs).
pub fn resolve_addr(e: &env::Environment) -> Result<String> {
    if let Some(a) = std::env::var("CAULDRON_RIPPLES_THREADS_ADDR").ok().filter(|v| !v.trim().is_empty()) {
        return Ok(a.trim().to_string());
    }
    let d = env::decl::load(&e.service_dir(), "threads").with_context(|| format!("finding the `threads` declaration of environment `{}`", e.name))?;
    if d.registers {
        let t = env::seams::Table::load(&e.name, &e.data_root, &e.seams())?;
        return t.seams.get("threads").map(|s| s.addr.clone()).context("threads registers its address and has no seam row: it is not running");
    }
    if let Some(a) = env::decl::Manifest::load(&e.manifest()).ok().and_then(|m| m.hosted_addr("threads")) {
        return Ok(a);
    }
    d.launch.map(|l| l.addr).context("the `threads` declaration names no address")
}

fn secret() -> Option<String> {
    if let Some(s) = std::env::var("THREADS_SECRET").ok().filter(|s| !s.trim().is_empty()) {
        return Some(s.trim().to_string());
    }
    let home = std::env::var_os("THREADS_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".threads")))?;
    let text = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let v: toml::Table = text.parse().ok()?;
    v.get("secret").and_then(|s| s.as_str()).filter(|s| !s.is_empty()).map(str::to_string)
}

/// **The thread settings of a ripple's call** (module docs): exactly what is
/// sent on `threads/create`.
pub fn settings(cwd: &str, model: &str, max_output_tokens: u32, budget_key: Option<&str>) -> Value {
    let mut s = json!({
        "cwd": cwd,
        "model": model,
        "completion": {"max_output_tokens": max_output_tokens},
        "permission_mode": PERMISSION_MODE,
        "tools": {"clean_slate": true, "tools": null, "agents_json": null, "allowed_tools": [], "disallowed_tools": DENIED_TOOLS},
        "plugin_roots": [],
        "plugin_dirs": [],
        "add_dirs": [],
        "append_system_prompt": null,
        "agent": null,
    });
    for k in INJECTIONS {
        s[*k] = json!(false);
    }
    if let Some(k) = budget_key {
        s["budget_key"] = json!(k);
    }
    s
}

impl Threads {
    /// A backend at `addr` (`host:port`).
    pub fn new(addr: &str, secret: Option<String>, budget_key: Option<String>, timeout: Duration) -> Threads {
        Threads { addr: addr.to_string(), secret, budget_key, timeout }
    }

    pub fn from_env(e: &env::Environment) -> Result<Threads> {
        Ok(Threads {
            addr: resolve_addr(e)?,
            secret: secret(),
            budget_key: std::env::var("CAULDRON_RIPPLES_BUDGET_KEY").ok().filter(|k| !k.trim().is_empty()),
            timeout: Duration::from_secs(std::env::var("CAULDRON_RIPPLES_CALL_TIMEOUT_S").ok().and_then(|v| v.parse().ok()).unwrap_or(900)),
        })
    }

    /// The longest one request waits: [`REQUEST_DEADLINE`], or the call's
    /// whole time limit when that is shorter.
    fn wait(&self) -> Duration {
        REQUEST_DEADLINE.min(self.timeout)
    }

    /// How to stop a call that may still be running in threads.
    fn how_to_stop(&self, thread: &str) -> String {
        format!(
            "threads at {} may still be running the ripple's thread {thread}: the ripples service tries the cancel again when it next \
             starts or runs; to stop it now, cancel that thread from a threads face (the TUI), or stop the threads service",
            self.addr
        )
    }

    /// **Stop a call and confirm it stopped**: interrupt the turn, withdraw
    /// the message, then read the thread until it is idle. Answers
    /// `{thread, message_id, confirmed, detail, how_to_stop?}`.
    fn stop(&self, thread: &str, message_id: Option<u64>) -> Value {
        let mut out = json!({"thread": thread, "message_id": message_id, "confirmed": false});
        let mut c = match Conn::open(&self.addr, self.secret.as_deref(), self.wait()) {
            Ok(c) => c,
            Err(f) => {
                out["detail"] = json!(format!("threads could not be reached to cancel: {}", f.message()));
                out["how_to_stop"] = json!(self.how_to_stop(thread));
                return out;
            }
        };
        let mut notes = Vec::new();
        if let Err(f) = c.request(&format!("threads/{thread}/cancel"), json!({})) {
            notes.push(format!("cancel: {}", f.message()));
        }
        if let Some(m) = message_id {
            match c.request(&format!("threads/{thread}/queue_cancel"), json!({"message_id": m})) {
                Ok(v) => notes.push(format!("withdrawn: {}", v["changed"])),
                Err(f) => notes.push(format!("queue_cancel: {}", f.message())),
            }
        }
        let deadline = Instant::now() + CANCEL_CONFIRM.min(self.timeout);
        let mut idle_seen = 0;
        loop {
            match c.request("threads/list", json!({"limit": 1000})) {
                Ok(v) => match v["threads"].as_array().and_then(|a| a.iter().find(|t| t["thread_id"] == thread)) {
                    Some(t) if t["executing"] != json!(true) && t["status"] == "idle" => {
                        idle_seen += 1;
                        // Idle twice, a second apart: no turn started again.
                        if idle_seen >= 2 {
                            out["confirmed"] = json!(true);
                            break;
                        }
                    }
                    Some(t) => {
                        idle_seen = 0;
                        notes.push(format!("still {}", t["status"].as_str().unwrap_or("?")));
                    }
                    None => {
                        notes.push("the thread is not listed".into());
                        break;
                    }
                },
                Err(f) => {
                    notes.push(format!("list: {}", f.message()));
                    break;
                }
            }
            if Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(1000));
        }
        notes.dedup();
        out["detail"] = json!(notes.join("; "));
        if out["confirmed"] != json!(true) {
            out["how_to_stop"] = json!(self.how_to_stop(thread));
        }
        out
    }
}

type Ws = tungstenite::WebSocket<TcpStream>;

struct Conn {
    ws: Ws,
    next: u64,
    /// The longest one request waits.
    wait: Duration,
}

impl Conn {
    fn open(addr: &str, secret: Option<&str>, wait: Duration) -> Result<Conn, Failure> {
        let unavailable = |m: String| Failure::Unavailable(m);
        let sa: SocketAddr = addr.parse().map_err(|e| unavailable(format!("the threads address `{addr}`: {e}")))?;
        let stream = env::egress::connect_tcp("threads chassis (ripples)", &sa, Duration::from_secs(5))
            .map_err(|e| unavailable(format!("cannot reach threads at {addr}: {e}")))?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
        let (ws, _) = tungstenite::client(format!("ws://{addr}/ws"), stream).map_err(|e| unavailable(format!("the websocket handshake with threads at {addr}: {e}")))?;
        let mut c = Conn { ws, next: 0, wait };
        let deadline = Instant::now() + wait;
        let welcome = c.frame(deadline)?;
        if welcome["type"] != "welcome" {
            return Err(unavailable(format!("threads said `{}` before its welcome", welcome["type"])));
        }
        if welcome["auth_required"] == json!(true) {
            let Some(s) = secret else {
                return Err(unavailable("threads asks for its secret, and none is set ($THREADS_SECRET or its config.toml)".into()));
            };
            c.send(&json!({"type": "auth", "secret": s}))?;
            let ack = c.frame(deadline)?;
            if ack["type"] != "auth_ack" || ack["ok"] != json!(true) {
                return Err(unavailable("threads refused its secret".into()));
            }
        }
        Ok(c)
    }

    fn send(&mut self, v: &Value) -> Result<(), Failure> {
        self.ws.send(tungstenite::Message::Text(v.to_string())).map_err(|e| Failure::Unavailable(format!("writing to threads: {e}")))
    }

    /// The next text frame, waiting no later than `deadline` (a server that
    /// is silent, or that sends only other frames, cannot hold a call).
    fn frame(&mut self, deadline: Instant) -> Result<Value, Failure> {
        loop {
            if Instant::now() > deadline {
                return Err(Failure::Failed { message: "threads did not answer in time".into(), usage: Usage::default() });
            }
            match self.ws.read() {
                Ok(tungstenite::Message::Text(t)) => {
                    return serde_json::from_str(&t).map_err(|e| Failure::Failed { message: format!("threads sent a frame that is not JSON: {e}"), usage: Usage::default() })
                }
                Ok(tungstenite::Message::Close(_)) => return Err(Failure::Unavailable("threads closed the connection".into())),
                Ok(_) => continue,
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
                Err(e) => return Err(Failure::Unavailable(format!("reading from threads: {e}"))),
            }
        }
    }

    /// One request; its success payload, or the error as a failure.
    fn request(&mut self, method: &str, params: Value) -> Result<Value, Failure> {
        self.next += 1;
        let id = format!("r{}", self.next);
        self.send(&json!({"type": "request", "id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + self.wait;
        loop {
            let f = self.frame(deadline).map_err(|f| match f {
                Failure::Failed { usage, .. } => Failure::Failed { message: format!("threads did not answer `{method}` within {} s", self.wait.as_secs()), usage },
                other => other,
            })?;
            if f["type"] != "response" || f["correlate"] != json!(id) {
                continue;
            }
            let o = &f["outcome"];
            return match o["type"].as_str() {
                Some("success") => Ok(o["payload"].clone()),
                _ => {
                    let msg = format!("threads refused `{method}`: {} ({})", o["error"].as_str().unwrap_or("?"), o["code"].as_str().unwrap_or("?"));
                    if o["code"].as_str().is_some_and(|c| c.contains("busy") || c.contains("overload")) {
                        Err(Failure::Busy(msg))
                    } else {
                        Err(Failure::Failed { message: msg, usage: Usage::default() })
                    }
                }
            };
        }
    }
}

/// The most characters of a failure's text a call's outcome carries.
const FAILURE_TEXT_CHARS: usize = 1200;

/// A failure's text as a run's outcome may carry it: the home directory
/// named `~`, control characters removed, at most [`FAILURE_TEXT_CHARS`]
/// characters (the END kept: that is where an error is).
fn scrubbed(text: &str) -> String {
    let mut t = text.to_string();
    if let Some(h) = std::env::var("HOME").ok().map(|h| h.trim_end_matches('/').to_string()).filter(|h| h.len() > 1) {
        t = t.replace(&h, "~");
    }
    let t: String = t.chars().map(|c| if c.is_control() && c != '\n' { ' ' } else { c }).collect();
    let t = t.trim();
    let n = t.chars().count();
    if n > FAILURE_TEXT_CHARS {
        format!("…{}", t.chars().skip(n - FAILURE_TEXT_CHARS).collect::<String>())
    } else {
        t.to_string()
    }
}

/// Does the stored settings row hold the restricted request as asked?
fn restricted(row: &Value, max_output_tokens: u32) -> bool {
    let s = &row["settings"];
    s["completion"]["max_output_tokens"] == json!(max_output_tokens) && s["permission_mode"] == PERMISSION_MODE && s["tools"]["clean_slate"] == json!(true)
}

impl Inference for Threads {
    fn name(&self) -> &'static str {
        "threads"
    }

    fn call(&self, r: &Request) -> Result<Reply, Failure> {
        let mut c = Conn::open(&self.addr, self.secret.as_deref(), self.wait())?;
        let settings = settings(&r.cwd.display().to_string(), &r.model, r.max_output_tokens, self.budget_key.as_deref());
        let created = c.request("threads/create", json!({"name": format!("ripple {}", &r.name[..r.name.len().min(12)]), "settings": settings}))?;
        let Some(thread) = created["thread_id"].as_str().map(str::to_string) else {
            return Err(Failure::Failed { message: format!("threads/create answered no thread_id: {created}"), usage: Usage::default() });
        };
        // Refuse a threads that did not take the restricted request, before
        // anything is sent.
        let listed = c.request("threads/list", json!({"limit": 1000}))?;
        let row = listed["threads"].as_array().and_then(|a| a.iter().find(|t| t["thread_id"] == thread.as_str()).cloned());
        if !row.as_ref().is_some_and(|row| restricted(row, r.max_output_tokens)) {
            return Err(Failure::Unsupported(format!(
                "the threads service at {} does not support the restricted request a ripple needs (a bare completion with no tools, \
                 no MCP server and no settings: the `completion` setting of threads in cauldron 0.1.10 or later); it did not keep \
                 the settings it was sent. Nothing was sent to a model",
                self.addr
            )));
        }
        let sent = c.request(
            &format!("threads/{thread}/send"),
            json!({"text": r.prompt, "bare": true, "metadata": {METADATA_KEY: {"json_schema": r.schema, "schema_retries": SCHEMA_RETRIES}}}),
        )?;
        let message_id = sent["message_id"].as_u64();
        r.note("sent", json!({"thread": thread, "message_id": message_id, "addr": self.addr}));
        let deadline = Instant::now() + self.timeout;
        let mut after: u64 = 0;
        let mut usage = Usage::default();
        let mut seen = std::collections::BTreeSet::new();
        // The token totals of the turn's results (`model_usage`), once one
        // has come; they replace the `message_usage` counts.
        let mut from_results: Option<Usage> = None;
        let mut model = r.model.clone();
        // Every exit but an answer stops the turn first.
        let give_up = |f: Failure| -> Failure {
            let report = self.stop(&thread, message_id);
            r.note("cancel", report);
            f
        };
        loop {
            let page = match c.request(&format!("threads/{thread}/history"), json!({"after_seq": after, "limit": 200})) {
                Ok(p) => p,
                Err(f) => return Err(give_up(f)),
            };
            for e in page["events"].as_array().into_iter().flatten() {
                if let Some(s) = e["seq"].as_u64() {
                    after = after.max(s);
                }
                let ev = &e["event"];
                let kind = ev["kind"].as_str().unwrap_or("");
                match kind {
                    "message_usage" => {
                        let key = ev["api_message_id"].as_str().map(str::to_string).unwrap_or_else(|| format!("seq{}", e["seq"]));
                        if seen.insert(key) && from_results.is_none() {
                            let n = |k: &str| ev[k].as_u64().unwrap_or(0);
                            usage.add(&Usage {
                                input_tokens: n("input_tokens"),
                                output_tokens: n("output_tokens"),
                                cache_read_tokens: n("cache_read_tokens"),
                                cache_creation_tokens: n("cache_creation_tokens"),
                                cost_usd: None,
                            });
                        }
                        if let Some(m) = ev["model"].as_str() {
                            model = m.to_string();
                        }
                    }
                    "result_turn" => {
                        if let Some(c) = ev["total_cost_usd"].as_f64() {
                            usage.cost_usd = Some(usage.cost_usd.unwrap_or(0.0) + c);
                        }
                        // The turn's own totals, when it reports them, are the
                        // tokens: a `message_usage` is the API message as first
                        // streamed, before its answer was written.
                        if let Some(models) = ev["model_usage"].as_object().filter(|m| !m.is_empty()) {
                            let total = from_results.get_or_insert_with(Usage::default);
                            for m in models.values() {
                                let n = |k: &str| m[k].as_u64().unwrap_or(0);
                                total.add(&Usage {
                                    input_tokens: n("inputTokens"),
                                    output_tokens: n("outputTokens"),
                                    cache_read_tokens: n("cacheReadInputTokens"),
                                    cache_creation_tokens: n("cacheCreationInputTokens"),
                                    cost_usd: None,
                                });
                            }
                            usage.input_tokens = total.input_tokens;
                            usage.output_tokens = total.output_tokens;
                            usage.cache_read_tokens = total.cache_read_tokens;
                            usage.cache_creation_tokens = total.cache_creation_tokens;
                        }
                    }
                    "custom" if ev["event_type"] == STRUCTURED_EVENT => {
                        let d = &ev["data"];
                        match d["status"].as_str() {
                            Some("ok") => return Ok(Reply { output: d["value"].clone(), usage, model }),
                            Some("unmet") => return Err(give_up(Failure::SchemaUnmet { message: format!("the answer did not meet the schema: {}", d["errors"]), usage, model })),
                            _ => {}
                        }
                    }
                    "turn_parked" => {
                        return Err(give_up(Failure::Parked(format!(
                            "threads parked the call ({}): {}",
                            ev["cause"].as_str().unwrap_or("?"),
                            ev["reason"].as_str().unwrap_or("")
                        ))))
                    }
                    "budget_tripped" => return Err(give_up(Failure::Parked(format!("threads' budget `{}` tripped", ev["budget_key"].as_str().unwrap_or("?"))))),
                    // A ripple's call is one delivery. A threads that would
                    // run it again (an older one, or a limit on the account)
                    // ends the call here: re-running the same command line
                    // repeats its failure, and the caller is waiting.
                    "retry_scheduled" => {
                        let kind = ev["error_kind"].as_str().unwrap_or("?");
                        let why = scrubbed(ev["reason"].as_str().unwrap_or(""));
                        if matches!(kind, "usage_limit" | "credit_balance" | "login_expired") {
                            return Err(give_up(Failure::Parked(format!("threads would run the call again later ({kind}): {why}"))));
                        }
                        return Err(give_up(Failure::Failed {
                            message: format!("the call ended in threads without an answer ({kind}); threads would run it again, which a ripple's call never does: {why}"),
                            usage,
                        }));
                    }
                    k if ENDED.contains(&k) => {
                        return Err(give_up(Failure::Failed {
                            message: format!("the call ended in threads without an answer ({k}): {}", scrubbed(ev["message"].as_str().or(ev["reason"].as_str()).unwrap_or(""))),
                            usage,
                        }))
                    }
                    _ => {}
                }
            }
            if let Some(n) = page["next_after"].as_u64() {
                after = after.max(n);
            }
            if Instant::now() > deadline {
                return Err(give_up(Failure::Failed { message: format!("no answer from threads within {} s", self.timeout.as_secs()), usage }));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn cancel(&self, sent: &Value) -> Value {
        match sent["thread"].as_str() {
            Some(t) => self.stop(t, sent["message_id"].as_u64()),
            None => json!({"confirmed": true, "detail": "no thread was named; nothing to cancel"}),
        }
    }
}
