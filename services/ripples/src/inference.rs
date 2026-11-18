//! **What runs a model call.** The [`Inference`] trait is the one seam
//! between a ripple and a model: one structured-output request in, one
//! answer (or a typed failure) out, with its token counts. Two backends:
//!
//! - [`crate::threads::Threads`] (the default): the `threads` service, on
//!   the owner's own Claude login;
//! - [`Scripted`]: answers read from a file, for tests. It makes no model
//!   call of any kind, and is refused outside an isolated process;
//! - [`crate::local::Local`]: the `inference` service on this machine, for a
//!   model id `local:<name>` (`cauldron ripples run --backend local`).
//!
//! A model id is a plain string (`sonnet`, `opus`, `local:<name>`); the
//! process's backend routes `local:` ids to the local one
//! ([`crate::local::Routed`]) and every other id to the default.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// A place a backend reports what it did during a call (`sent`: the
/// handle of a call in flight; `cancel`: how stopping it went), so the run's
/// journal holds it the moment it happens.
pub type Sink = std::sync::Arc<dyn Fn(&str, Value) + Send + Sync>;

/// One structured-output call.
#[derive(Clone)]
pub struct Request {
    /// The model id (`sonnet`, `opus`).
    pub model: String,
    /// The whole prompt: everything the model is sent beyond the backend's
    /// own harness.
    pub prompt: String,
    /// The JSON schema the answer must meet.
    pub schema: Value,
    /// The working directory the backend runs its call in: an empty private
    /// directory made for the run.
    pub cwd: PathBuf,
    /// A name for the call (the run id), for the backend's own records.
    pub name: String,
    /// The most tokens the answer may have.
    pub max_output_tokens: u32,
    /// Where the backend reports `sent` and `cancel` (see [`Sink`]).
    pub sink: Option<Sink>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request").field("model", &self.model).field("prompt_chars", &self.prompt.chars().count()).field("cwd", &self.cwd).field("name", &self.name).finish()
    }
}

impl Request {
    /// Report `event` to the sink, if any.
    pub fn note(&self, event: &str, v: Value) {
        if let Some(s) = &self.sink {
            s(event, v);
        }
    }
}

/// Token counts of one call (or of a run, summed).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_creation_tokens: u64,
    /// What the backend says it cost, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl Usage {
    /// Every input token the model read: fresh, cached and cache-written.
    pub fn input_total(&self) -> u64 {
        self.input_tokens + self.cache_read_tokens + self.cache_creation_tokens
    }

    pub fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cache_read_tokens += o.cache_read_tokens;
        self.cache_creation_tokens += o.cache_creation_tokens;
        self.cost_usd = match (self.cost_usd, o.cost_usd) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        };
    }
}

/// A call's answer.
#[derive(Debug, Clone)]
pub struct Reply {
    /// The structured answer, already judged against the schema by the
    /// backend where it can.
    pub output: Value,
    pub usage: Usage,
    /// The model that answered, as the backend names it.
    pub model: String,
}

/// Why a call gave no answer.
#[derive(Debug, Clone)]
pub enum Failure {
    /// Busy or overloaded: worth trying again (within the run's caps).
    Busy(String),
    /// Held by the backend (threads parked it: an account over its usage
    /// threshold or limit, a spent credit balance, an expired login, a
    /// budget): the run waits. `cause` is a word of [`PARK_CAUSES`]; `usage`
    /// is what the call spent before it was held (none when threads parked
    /// it at admission, before any `claude` ran).
    Parked { message: String, cause: &'static str, usage: Usage },
    /// An answer came, and did not meet the schema after the backend's own
    /// corrective turn.
    SchemaUnmet { message: String, usage: Usage, model: String },
    /// The backend cannot be reached.
    Unavailable(String),
    /// The backend cannot make the call as restricted as a ripple's must
    /// be (an older threads): nothing was sent.
    Unsupported(String),
    /// Anything else.
    Failed { message: String, usage: Usage },
}

/// **Why threads held a call**, as a fixed word: `threshold` (every account
/// it may use is over its usage threshold), `usage_limit` (an account hit
/// its limit), `credit_balance`, `login_expired`, `budget` (a threads
/// budget tripped), or `parked` (any other cause).
pub const PARK_CAUSES: &[&str] = &["threshold", "usage_limit", "credit_balance", "login_expired", "budget", "parked"];

/// The word of [`PARK_CAUSES`] for a cause as threads names it.
pub fn park_cause(threads_word: &str) -> &'static str {
    match threads_word {
        "threshold" => "threshold",
        "usage_limit" | "rate_limit" | "limit" => "usage_limit",
        "credit_balance" => "credit_balance",
        "login_expired" => "login_expired",
        "budget" => "budget",
        _ => "parked",
    }
}

impl Failure {
    pub fn code(&self) -> &'static str {
        match self {
            Failure::Busy(_) => "busy",
            Failure::Parked { .. } => "parked",
            Failure::SchemaUnmet { .. } => "schema_unmet",
            Failure::Unavailable(_) => "unavailable",
            Failure::Unsupported(_) => "unsupported",
            Failure::Failed { .. } => "failed",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Failure::Busy(m) | Failure::Unavailable(m) | Failure::Unsupported(m) => m,
            Failure::Parked { message, .. } => message,
            Failure::SchemaUnmet { message, .. } | Failure::Failed { message, .. } => message,
        }
    }

    /// The tokens it spent anyway.
    pub fn usage(&self) -> Usage {
        match self {
            Failure::SchemaUnmet { usage, .. } | Failure::Failed { usage, .. } | Failure::Parked { usage, .. } => usage.clone(),
            _ => Usage::default(),
        }
    }
}

/// **A model-call backend.**
pub trait Inference: Send + Sync {
    /// The backend's name, recorded in the journal (`threads`, `scripted`,
    /// `local`).
    fn name(&self) -> &'static str;
    /// The name of the backend that runs `model` (a router answers per
    /// model; any other backend, its own name).
    fn name_for(&self, model: &str) -> &'static str {
        let _ = model;
        self.name()
    }
    /// One call.
    fn call(&self, r: &Request) -> Result<Reply, Failure>;
    /// Stop a call that may still be running, from what the backend
    /// reported as `sent`; answers `{confirmed, detail, how_to_stop?}`.
    fn cancel(&self, _sent: &Value) -> Value {
        json!({"confirmed": true, "detail": "this backend leaves nothing running"})
    }
}

/// The variable that chooses the backend: `threads` (the default) or
/// `scripted:<file>` (tests; refused outside an isolated process).
pub const BACKEND_ENV: &str = "CAULDRON_RIPPLES_INFERENCE";

/// The backend this process uses: the default one ([`default_from_env`])
/// for every model, and the local one for `local:<name>`.
pub fn from_env(e: &env::Environment) -> anyhow::Result<Box<dyn Inference>> {
    Ok(Box::new(crate::local::Routed { default: default_from_env(e)?, local: Box::new(crate::local::Local::from_env(e)?) }))
}

/// The default backend: `threads`, or `scripted:<file>` in tests.
pub fn default_from_env(e: &env::Environment) -> anyhow::Result<Box<dyn Inference>> {
    match std::env::var(BACKEND_ENV).ok().filter(|v| !v.trim().is_empty()) {
        None => Ok(Box::new(crate::threads::Threads::from_env(e)?)),
        Some(v) if v == "threads" => Ok(Box::new(crate::threads::Threads::from_env(e)?)),
        Some(v) => match v.strip_prefix("scripted:") {
            Some(path) => {
                if env::egress::process_isolation().is_none() {
                    anyhow::bail!("{BACKEND_ENV}=scripted:… is for tests: it is refused outside an isolated process");
                }
                Ok(Box::new(Scripted::new(Path::new(path))))
            }
            None => anyhow::bail!("{BACKEND_ENV} is `threads` or `scripted:<file>`, not `{v}`"),
        },
    }
}

/// **The scripted backend**: each call takes the next entry of a JSON list
/// in a file, and appends what it was asked to `<file>.calls.jsonl`, so a
/// test can read the prompt and the model of every call. An entry is
/// `{"output": …, "usage": {…}, "model": "…", "sleep_ms": n}` or `{"fail":
/// "busy"|"parked"|"schema_unmet"|"unavailable"|"unsupported"|"failed", "message": "…", "cause": "…" (a park's),
/// "usage": {…}, "sleep_ms": n}`; `"sent": true` on an entry reports a call
/// handle (event `sent`) before it answers. No entry left is `failed`.
pub struct Scripted {
    path: PathBuf,
    next: Mutex<usize>,
}

impl Scripted {
    pub fn new(path: &Path) -> Scripted {
        Scripted { path: path.to_path_buf(), next: Mutex::new(0) }
    }
}

impl Inference for Scripted {
    fn name(&self) -> &'static str {
        "scripted"
    }

    fn call(&self, r: &Request) -> Result<Reply, Failure> {
        use std::io::Write;
        let n = {
            let mut g = self.next.lock().unwrap_or_else(|p| p.into_inner());
            *g += 1;
            *g - 1
        };
        let log = PathBuf::from(format!("{}.calls.jsonl", self.path.display()));
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&r.cwd).map(|m| m.permissions().mode() & 0o777).ok();
            let entries = std::fs::read_dir(&r.cwd).map(|d| d.count()).ok();
            let _ = writeln!(
                f,
                "{}",
                json!({"n": n, "model": r.model, "prompt": r.prompt, "schema_title": r.schema["title"], "cwd": r.cwd, "cwd_mode": mode, "cwd_entries": entries, "max_output_tokens": r.max_output_tokens})
            );
            let _ = f.sync_all();
        }
        let script: Vec<Value> = std::fs::read_to_string(&self.path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let Some(e) = script.get(n) else {
            return Err(Failure::Failed { message: format!("the script has no entry {n}"), usage: Usage::default() });
        };
        if e["sent"] == json!(true) {
            r.note("sent", json!({"thread": format!("scripted-{n}"), "message_id": n}));
        }
        if let Some(ms) = e["sleep_ms"].as_u64() {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
        let usage: Usage = serde_json::from_value(e["usage"].clone()).unwrap_or_default();
        let message = e["message"].as_str().unwrap_or("scripted").to_string();
        let model = e["model"].as_str().map(str::to_string).unwrap_or_else(|| r.model.clone());
        match e["fail"].as_str() {
            None => Ok(Reply { output: e["output"].clone(), usage, model }),
            Some("busy") => Err(Failure::Busy(message)),
            Some("parked") => Err(Failure::Parked { message, cause: park_cause(e["cause"].as_str().unwrap_or("")), usage }),
            Some("schema_unmet") => Err(Failure::SchemaUnmet { message, usage, model }),
            Some("unavailable") => Err(Failure::Unavailable(message)),
            Some("unsupported") => Err(Failure::Unsupported(message)),
            Some(_) => Err(Failure::Failed { message, usage }),
        }
    }
}
