//! **The supervisor of one `llama-server` child.**
//!
//! - **On demand.** Nothing is loaded at start. The first request for a
//!   model loads it: the runtime is checked ([`crate::runtime`]), the model
//!   must be registered and its file the one registered (its identity, and
//!   its sha256 when the identity changed), and its memory estimate
//!   ([`inference_registry::memory`]) must be within the budget, or the
//!   request is refused with the arithmetic. A request for another model
//!   unloads the first: never more than one model, so never more than the
//!   budget.
//! - **Idle unload.** A model no request has used for the idle time (15
//!   minutes by default) is unloaded: the child exits and its memory is the
//!   system's again.
//! - **One request at a time** (the child is started with one slot); another
//!   meanwhile is answered `busy`.
//! - **Crashes.** A child that exits when nobody asked it to is recorded
//!   (`crashes`, `last_crash` with its exit status and the end of its log)
//!   and the next load waits out a backoff: 1 s after the first crash, then
//!   2, 4, … up to 60 s; a request in the backoff is answered `busy` with
//!   when to try again. The count resets after a request is served.
//! - **No orphan.** The child runs under a guard ([`crate::guard`]) whose
//!   standard input is a pipe from this process: unload closes it, and so
//!   does this process's end, however it ends. The child's pid is also kept
//!   in `child.json`, and a child a previous start left behind is stopped
//!   when the service starts.
//! - **Loopback only.** The child listens on 127.0.0.1, on a port chosen
//!   free at each load, and answers only requests carrying a key minted for
//!   that load (`llama.key`, 0600), passed with `--api-key-file`, and
//!   removed when the child is unloaded or exits. llama-server itself
//!   leaves `/health` open without a key (it says only that it is up);
//!   `/v1/models` needs the key (build 12110).

use std::io::{BufRead, BufReader, Read};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use inference_registry::memory::{self, Estimate};
use inference_registry::{Config, FileId, Model, Registry};
use serde_json::{json, Value};

use crate::guard;
use crate::runtime::{self, Runtime};

/// The child's log, in the service's directory.
pub const LOG_FILE: &str = "llama-server.log";
/// The pids of the child and its guard while one runs.
pub const CHILD_FILE: &str = "child.json";
/// The key the child answers to, minted at each load.
pub const LLAMA_KEY_FILE: &str = "llama.key";
/// The longest backoff after crashes.
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Why a request was not answered.
#[derive(Debug, Clone)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
    pub detail: Value,
}

impl Refusal {
    pub fn new(code: &'static str, message: impl Into<String>) -> Refusal {
        Refusal { code, message: message.into(), detail: Value::Null }
    }

    pub fn with(code: &'static str, message: impl Into<String>, detail: Value) -> Refusal {
        Refusal { code, message: message.into(), detail }
    }

    pub fn json(&self) -> Value {
        let mut v = json!({"ok": false, "code": self.code, "message": self.message});
        if !self.detail.is_null() {
            v["detail"] = self.detail.clone();
        }
        v
    }
}

/// The environment a child (and the runtime check) gets: the few variables
/// a program needs to run, and nothing else, so no `LLAMA_ARG_*` (tools,
/// MCP servers, a tunnel, a download) or `HF_*` variable of this process
/// reaches `llama-server`.
pub fn child_env() -> Vec<(String, String)> {
    ["PATH", "HOME", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE", env::egress::ISOLATED_ENV].iter().filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v))).collect()
}

/// The backoff after `n` crashes in a row.
pub fn backoff(n: u32) -> Duration {
    if n == 0 {
        return Duration::ZERO;
    }
    Duration::from_secs(1u64 << (n - 1).min(6)).min(BACKOFF_MAX)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A child's exit, once known: `Some(code)` (128 + signal for a signal).
type Exit = Arc<(Mutex<Option<i32>>, Condvar)>;

struct Loaded {
    gen: u64,
    model: Model,
    estimate: Estimate,
    context: u32,
    guard_pid: u32,
    child_pid: u32,
    port: u16,
    api_key: String,
    stdin: Option<ChildStdin>,
    loaded_at: String,
    load_ms: Option<u64>,
    ready: bool,
    stopping: bool,
    exit: Exit,
    argv: Vec<String>,
}

/// What a request needs of the loaded child.
#[derive(Clone)]
pub struct Endpoint {
    pub addr: SocketAddr,
    pub api_key: String,
    pub model: Model,
    pub gen: u64,
    /// Set when this request loaded the model: how long that took.
    pub load_ms: Option<u64>,
    exit: Exit,
}

impl Endpoint {
    /// The child's exit status, if it exited.
    pub fn exited(&self) -> Option<i32> {
        *self.exit.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

struct InFlight {
    request_id: String,
    stream: TcpStream,
    started_at: String,
    cancelled: bool,
}

struct Inner {
    loaded: Option<Loaded>,
    gen: u64,
    crashes: u32,
    last_crash: Option<Value>,
    backoff_until: Option<(Instant, String)>,
    last_unload: Option<Value>,
    last_used: Instant,
    served: u64,
    in_flight: Option<InFlight>,
    runtime: Option<Result<Runtime, String>>,
    /// The load an `unload` ended last, and why: a request that loses its
    /// child to it is told so, not that the child crashed.
    unloaded: Option<(u64, String)>,
}

pub struct Supervisor {
    pub data_root: PathBuf,
    pub dir: PathBuf,
    /// The guard: this binary, run with `--guard`.
    pub guard_exe: PathBuf,
    inner: Mutex<Inner>,
    /// Held for the whole of a request (load included).
    request: Mutex<()>,
    /// Itself, weakly: a watcher thread reports to the supervisor that
    /// started it, and does not keep a dropped one alive.
    me: std::sync::Weak<Supervisor>,
}

impl Supervisor {
    pub fn new(data_root: &Path, guard_exe: &Path) -> Arc<Supervisor> {
        Arc::new_cyclic(|me| Supervisor {
            me: me.clone(),
            data_root: data_root.to_path_buf(),
            dir: inference_registry::dir(data_root),
            guard_exe: guard_exe.to_path_buf(),
            inner: Mutex::new(Inner {
                loaded: None,
                gen: 0,
                crashes: 0,
                last_crash: None,
                backoff_until: None,
                last_unload: None,
                last_used: Instant::now(),
                served: 0,
                in_flight: None,
                runtime: None,
                unloaded: None,
            }),
            request: Mutex::new(()),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn config(&self) -> Result<Config, Refusal> {
        Config::load(&self.data_root).map_err(|e| Refusal::new("settings", format!("{e:#}")))
    }

    /// Check the runtime the settings name now, and keep the answer for
    /// `status`.
    pub fn check_runtime(&self) -> Result<Runtime, String> {
        let r = match Config::load(&self.data_root) {
            Ok(c) => runtime::check(c.llama_server.as_deref()),
            Err(e) => Err(format!("{e:#}")),
        };
        self.lock().runtime = Some(r.clone());
        r
    }

    /// **Stop a child a previous start of this service left running**
    /// (`child.json`): its guard and itself, if they are still the programs
    /// recorded. Answers what was done.
    pub fn reap_leftover(&self) -> Option<Value> {
        // Nothing is loaded at start, so no key is current.
        let _ = std::fs::remove_file(self.dir.join(LLAMA_KEY_FILE));
        let p = self.dir.join(CHILD_FILE);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
        let _ = std::fs::remove_file(&p);
        let mut stopped = vec![];
        for (k, prog, seen) in [("child_pid", v["program"].as_str(), v["command"].as_str()), ("guard_pid", v["guard"].as_str(), None)] {
            let (Some(pid), Some(prog)) = (v[k].as_u64().map(|x| x as u32), prog) else { continue };
            // Still the program recorded (as `ps` saw it once it was ready,
            // or by its file name), so a reused pid is never signalled.
            let same = |c: String| match seen {
                Some(s) => c == s,
                None => Path::new(&c).file_name() == Path::new(prog).file_name(),
            };
            if guard::alive(pid) && command_of(pid).is_some_and(same) {
                guard::signal(pid, "TERM");
                let deadline = Instant::now() + guard::GRACE;
                while guard::alive(pid) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(50));
                }
                if guard::alive(pid) {
                    guard::signal(pid, "KILL");
                }
                stopped.push(json!({k: pid}));
            }
        }
        Some(json!({"found": v, "stopped": stopped}))
    }

    /// **Unload** the model, if one is loaded: close the guard's input, wait
    /// for the child to exit (the guard's grace, then a kill).
    pub fn unload(&self, reason: &str) -> Option<Value> {
        let (exit, child, guard_pid, name) = {
            let mut g = self.lock();
            let l = g.loaded.as_mut()?;
            l.stopping = true;
            drop(l.stdin.take());
            let out = (l.exit.clone(), l.child_pid, l.guard_pid, l.model.name.clone());
            g.unloaded = Some((l.gen, reason.to_string()));
            g.loaded = None;
            out
        };
        let deadline = Instant::now() + guard::GRACE + Duration::from_secs(5);
        let (m, cv) = &*exit;
        let mut st = m.lock().unwrap_or_else(|p| p.into_inner());
        while st.is_none() && Instant::now() < deadline {
            st = cv.wait_timeout(st, Duration::from_millis(100)).unwrap_or_else(|p| p.into_inner()).0;
        }
        let code = *st;
        drop(st);
        if code.is_none() {
            for pid in [child, guard_pid] {
                guard::signal(pid, "KILL");
            }
        }
        let _ = std::fs::remove_file(self.dir.join(CHILD_FILE));
        // The key answered only that child; the next load mints another.
        let _ = std::fs::remove_file(self.dir.join(LLAMA_KEY_FILE));
        let v = json!({"model": name, "reason": reason, "at": now(), "exit": code, "child_pid": child});
        self.lock().last_unload = Some(v.clone());
        tracing::info!(model = %name, reason, "unloaded");
        Some(v)
    }

    /// Unload when the model has been idle for the settings' idle time and
    /// no request is in progress.
    pub fn idle_tick(&self) {
        let Ok(_req) = self.request.try_lock() else { return };
        let idle = match Config::load(&self.data_root) {
            Ok(c) => Duration::from_secs(c.idle_seconds),
            Err(_) => return,
        };
        let due = {
            let g = self.lock();
            g.loaded.as_ref().is_some_and(|l| l.ready) && g.in_flight.is_none() && g.last_used.elapsed() >= idle
        };
        if due {
            self.unload("idle");
        }
    }

    /// **The endpoint of `name`, loading it if needed.** Called with the
    /// request lock held.
    fn ensure(&self, name: &str) -> Result<Endpoint, Refusal> {
        {
            let g = self.lock();
            if let Some(l) = g.loaded.as_ref() {
                if l.model.name == name && l.ready && l.exit.0.lock().unwrap_or_else(|p| p.into_inner()).is_none() {
                    return Ok(Endpoint { addr: SocketAddr::from(([127, 0, 0, 1], l.port)), api_key: l.api_key.clone(), model: l.model.clone(), gen: l.gen, load_ms: None, exit: l.exit.clone() });
                }
            }
        }
        // Another model, or a child that exited: never two at once.
        self.unload("another model was asked for");
        if let Some((until, at)) = self.lock().backoff_until.clone() {
            if Instant::now() < until {
                let wait = until.saturating_duration_since(Instant::now()).as_secs() + 1;
                return Err(Refusal::with(
                    "busy",
                    format!("llama-server crashed; the next load waits until {at} (about {wait} s). Try again then; `cauldron inference status` shows the crash"),
                    json!({"retry_after_s": wait}),
                ));
            }
        }
        let cfg = self.config()?;
        let rt = self.check_runtime().map_err(|m| Refusal::new("runtime", m))?;
        let reg = Registry::load(&self.data_root).map_err(|e| Refusal::new("settings", format!("{e:#}")))?;
        let Some(model) = reg.get(name).cloned() else {
            let known: Vec<&str> = reg.models.iter().map(|m| m.name.as_str()).collect();
            return Err(Refusal::new(
                "unknown_model",
                format!("no model named `{name}` is registered (registered: {}); `cauldron inference models add <name> <path>` registers one", if known.is_empty() { "none".into() } else { known.join(", ") }),
            ));
        };
        // The file is the one registered: its identity (canonical path,
        // device, inode, size, modification and change times), and when any
        // of it differs (or the settings ask for it every time) its sha256,
        // read again whole.
        let verified = model.verify(cfg.verify_every_load).map_err(|e| Refusal::new("model_changed", format!("{e:#}")))?;
        if !verified.rehashed_because.is_empty() {
            tracing::info!(model = %name, because = %verified.rehashed_because.join(", "), "the model file was read again; its sha256 is the one registered");
            if let Err(e) = Registry::record_identity(&self.data_root, name, &model.sha256, &verified.file) {
                tracing::warn!(model = %name, error = %format!("{e:#}"), "the file's new identity could not be recorded; it is read again at the next load");
            }
        }
        // The registry's header facts are bounded at registration; one
        // written by hand or by another version is checked again here.
        model.facts.check().map_err(|e| Refusal::new("model_header", format!("model `{name}`: {e:#}; remove it and register it again")))?;
        let context = model.context_under(&cfg);
        let est = memory::estimate(model.size_bytes, &model.facts, context, cfg.memory_budget_bytes);
        if !est.fits {
            return Err(Refusal::with("over_budget", format!("model `{name}` was not loaded: {}", est.how), json!(est)));
        }
        self.start_child(&rt, &model, &verified.file, context, est, &cfg)
    }

    fn start_child(&self, rt: &Runtime, model: &Model, file: &FileId, context: u32, est: Estimate, cfg: &Config) -> Result<Endpoint, Refusal> {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let started = Instant::now();
        let fail = |m: String| Refusal::new("load_failed", m);
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.dir).map_err(|e| fail(format!("creating {}: {e}", self.dir.display())))?;
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").map_err(|e| fail(format!("no free loopback port: {e}")))?;
            l.local_addr().map_err(|e| fail(e.to_string()))?.port()
        };
        let key = crate::key::random_hex();
        let key_path = self.dir.join(LLAMA_KEY_FILE);
        {
            let tmp = self.dir.join(format!(".{LLAMA_KEY_FILE}.{}", std::process::id()));
            let _ = std::fs::remove_file(&tmp);
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).map_err(|e| fail(e.to_string()))?;
            f.write_all(format!("{key}\n").as_bytes()).map_err(|e| fail(e.to_string()))?;
            std::fs::rename(&tmp, &key_path).map_err(|e| fail(e.to_string()))?;
        }
        let argv = argv(rt, model, &file.real_path, context, port, &key_path);
        let log_path = self.dir.join(LOG_FILE);
        let log = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&log_path).map_err(|e| fail(format!("{}: {e}", log_path.display())))?;
        let _ = writeln!(&log, "\n=== {} load of `{}`: {}", now(), model.name, argv.join(" "));
        let mut guard: Child = Command::new(&self.guard_exe)
            .arg("--guard")
            .arg("--")
            .args(&argv)
            .env_clear()
            .envs(child_env())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|e| fail(format!("the guard ({}) could not be started: {e}", self.guard_exe.display())))?;
        let guard_pid = guard.id();
        let stdin = guard.stdin.take();
        let mut first = String::new();
        if let Some(out) = guard.stdout.take() {
            let mut r = BufReader::new(out);
            let _ = r.read_line(&mut first);
            // Drain the rest so the guard never blocks on it.
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut r, &mut std::io::sink());
            });
        }
        let Some(child_pid) = first.trim().strip_prefix("pid ").and_then(|p| p.parse::<u32>().ok()) else {
            drop(stdin);
            let _ = guard.wait();
            return Err(fail(format!("llama-server could not be started: {}", first.trim().trim_start_matches("error ").trim())));
        };
        let exit: Exit = Arc::new((Mutex::new(None), Condvar::new()));
        let gen = {
            let mut g = self.lock();
            g.gen += 1;
            let gen = g.gen;
            g.loaded = Some(Loaded {
                gen,
                model: model.clone(),
                estimate: est,
                context,
                guard_pid,
                child_pid,
                port,
                api_key: key.clone(),
                stdin,
                loaded_at: now(),
                load_ms: None,
                ready: false,
                stopping: false,
                exit: exit.clone(),
                argv: argv.clone(),
            });
            gen
        };
        let _ = std::fs::write(
            self.dir.join(CHILD_FILE),
            json!({"child_pid": child_pid, "guard_pid": guard_pid, "program": rt.path, "guard": self.guard_exe, "model": model.name, "at": now()}).to_string(),
        );
        // The watcher: the guard's exit is the child's.
        {
            let (exit, me) = (exit.clone(), self.weak());
            std::thread::spawn(move || {
                use std::os::unix::process::ExitStatusExt;
                let code = match guard.wait() {
                    Ok(s) => s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)),
                    Err(_) => -1,
                };
                *exit.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(code);
                exit.1.notify_all();
                if let Some(me) = me.upgrade() {
                    me.child_exited(gen, code);
                }
            });
        }
        tracing::info!(model = %model.name, port, child_pid, "loading");
        // Ready when /health answers 200.
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let deadline = started + Duration::from_secs(cfg_load_timeout(cfg));
        loop {
            if let Some(code) = *exit.0.lock().unwrap_or_else(|p| p.into_inner()) {
                // Counted as a crash exactly once, whether the watcher saw
                // the exit first or this loop did: `child_exited` acts only
                // while this load is still the loaded one.
                self.child_exited(gen, code);
                return Err(Refusal::with(
                    "load_failed",
                    format!("llama-server exited while loading `{}` (exit {code}); the end of its log: {}", model.name, tail(&log_path, 1500)),
                    json!({"exit": code, "log": log_path}),
                ));
            }
            if health(&addr, &key) {
                break;
            }
            if Instant::now() > deadline {
                self.unload("the load did not finish in time");
                return Err(fail(format!("llama-server did not become ready within {} s; the end of its log: {}", cfg_load_timeout(cfg), tail(&log_path, 1500))));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // The file llama-server opened is the one verified: had it been
        // swapped between the check and the open, its identity would differ.
        let swapped = match FileId::now(&file.real_path) {
            Ok(now) => now.differences(file),
            Err(e) => vec![format!("{e:#}")],
        };
        if !swapped.is_empty() {
            self.unload("the model file changed while it loaded");
            return Err(Refusal::new(
                "model_changed",
                format!("model `{}`: {} changed while it loaded ({}); it was unloaded. Run the request again; a file that is not the one registered is refused", model.name, file.real_path.display(), swapped.join(", ")),
            ));
        }
        let load_ms = started.elapsed().as_millis() as u64;
        let _ = std::fs::write(
            self.dir.join(CHILD_FILE),
            json!({"child_pid": child_pid, "guard_pid": guard_pid, "program": rt.path, "command": command_of(child_pid), "guard": self.guard_exe, "model": model.name, "at": now()}).to_string(),
        );
        let mut g = self.lock();
        g.last_used = Instant::now();
        if let Some(l) = g.loaded.as_mut().filter(|l| l.gen == gen) {
            l.ready = true;
            l.load_ms = Some(load_ms);
        }
        tracing::info!(model = %model.name, load_ms, "loaded");
        Ok(Endpoint { addr, api_key: key, model: model.clone(), gen, load_ms: Some(load_ms), exit })
    }

    fn weak(&self) -> std::sync::Weak<Supervisor> {
        self.me.clone()
    }

    /// The watcher's report: a child that exited when nobody asked is a
    /// crash.
    fn child_exited(&self, gen: u64, code: i32) {
        let mut g = self.lock();
        let Some(l) = g.loaded.as_ref().filter(|l| l.gen == gen) else { return };
        if l.stopping {
            return;
        }
        let model = l.model.name.clone();
        let was_ready = l.ready;
        let child = l.child_pid;
        g.loaded = None;
        g.crashes += 1;
        let wait = backoff(g.crashes);
        let until = chrono::Utc::now() + chrono::Duration::milliseconds(wait.as_millis() as i64);
        g.backoff_until = Some((Instant::now() + wait, until.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)));
        let log = self.dir.join(LOG_FILE);
        g.last_crash = Some(json!({
            "model": model, "exit": code, "at": now(), "during": if was_ready { "serving" } else { "loading" },
            "crashes_in_a_row": g.crashes, "backoff_s": wait.as_secs(), "log_tail": tail(&log, 800),
        }));
        drop(g);
        // The guard ends after its child, unless it was killed itself: then
        // the child is stopped here.
        if guard::alive(child) {
            tracing::warn!(model = %model, child, "the guard ended and its child did not; stopping the child");
            guard::signal(child, "TERM");
            let deadline = Instant::now() + guard::GRACE;
            while guard::alive(child) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            if guard::alive(child) {
                guard::signal(child, "KILL");
            }
        }
        let _ = std::fs::remove_file(self.dir.join(CHILD_FILE));
        let _ = std::fs::remove_file(self.dir.join(LLAMA_KEY_FILE));
        tracing::warn!(model = %model, exit = code, "llama-server exited unasked");
    }

    /// **One completion**: see [`crate::complete`].
    pub fn complete(&self, req: &crate::complete::Ask) -> Result<Value, Refusal> {
        let Ok(_req) = self.request.try_lock() else {
            let g = self.lock();
            let who = g.in_flight.as_ref().map(|f| format!(" (request {} since {})", f.request_id, f.started_at)).unwrap_or_default();
            return Err(Refusal::new("busy", format!("another request is in progress{who}; one at a time")));
        };
        let started = Instant::now();
        let ep = self.ensure(&req.model)?;
        let addr = ep.addr;
        let stream = crate::http::connect(&addr, Duration::from_secs(req.timeout_s)).map_err(|e| Refusal::new("failed", format!("cannot reach llama-server at {addr}: {e}")))?;
        {
            let mut g = self.lock();
            g.in_flight = Some(InFlight {
                request_id: req.request_id.clone(),
                stream: stream.try_clone().map_err(|e| Refusal::new("failed", e.to_string()))?,
                started_at: now(),
                cancelled: false,
            });
        }
        let body = crate::complete::body(req, &ep.model.name);
        let res = crate::http::send(&stream, &addr, "POST", "/v1/chat/completions", Some(&ep.api_key), Some(body.to_string().as_bytes()));
        let _ = stream.shutdown(std::net::Shutdown::Both);
        let cancelled = {
            let mut g = self.lock();
            g.last_used = Instant::now();
            g.in_flight.take().is_some_and(|f| f.cancelled)
        };
        let wall_ms = started.elapsed().as_millis() as u64;
        let out = match res {
            Err(_) if cancelled => Err(Refusal::new("cancelled", "the request was cancelled; llama-server was disconnected, which stops its generation")),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                Err(Refusal::new("timeout", format!("no answer within {} s; the request was given up and llama-server was disconnected, which stops its generation", req.timeout_s)))
            }
            Err(e) => {
                std::thread::sleep(Duration::from_millis(200));
                let unloaded = self.lock().unloaded.clone().filter(|(gen, _)| *gen == ep.gen).map(|(_, why)| why);
                match ep.exited() {
                    Some(code) if unloaded.is_some() => Err(Refusal::with(
                        "unloaded",
                        format!("the model was unloaded during the request (why: {}); llama-server was stopped on purpose. Run the request again", unloaded.unwrap_or_default()),
                        json!({"exit": code}),
                    )),
                    Some(code) => Err(Refusal::with("crashed", format!("llama-server exited during the request (exit {code}); `cauldron inference status` shows the end of its log"), json!({"exit": code}))),
                    None => Err(Refusal::new("failed", format!("the request to llama-server failed: {e}"))),
                }
            }
            Ok(r) => crate::complete::answer(req, &ep, r, wall_ms),
        };
        if out.is_ok() {
            let mut g = self.lock();
            g.served += 1;
            g.crashes = 0;
            g.backoff_until = None;
        }
        out
    }

    /// **Cancel** request `id` if it is the one in flight.
    pub fn cancel(&self, id: &str) -> Value {
        let mut g = self.lock();
        match g.in_flight.as_mut() {
            Some(f) if f.request_id == id => {
                f.cancelled = true;
                let _ = f.stream.shutdown(std::net::Shutdown::Both);
                json!({"ok": true, "confirmed": true, "detail": format!("request {id} was disconnected from llama-server, which stops its generation")})
            }
            _ => json!({"ok": true, "confirmed": true, "detail": format!("request {id} is not running")}),
        }
    }

    /// **The status**: the runtime, the model (loaded or not), its memory
    /// estimate, the idle timer, the child's pid, crashes and the backoff.
    pub fn status(&self) -> Value {
        let cfg = Config::load(&self.data_root);
        let reg = Registry::load(&self.data_root);
        let g = self.lock();
        let idle = cfg.as_ref().map(|c| c.idle_seconds).unwrap_or(inference_registry::DEFAULT_IDLE_MINUTES * 60);
        let model = g.loaded.as_ref().map(|l| {
            let idle_for = g.last_used.elapsed().as_secs();
            json!({
                "name": l.model.name, "path": l.model.path, "sha256": l.model.sha256, "size_bytes": l.model.size_bytes,
                "state": if l.ready { "ready" } else { "loading" }, "context": l.context,
                "memory_estimate": l.estimate, "child_pid": l.child_pid, "guard_pid": l.guard_pid, "port": l.port,
                "loaded_at": l.loaded_at, "load_ms": l.load_ms, "argv": l.argv,
                "idle_s": idle_for, "unloads_in_s": if g.in_flight.is_some() { None } else { Some(idle.saturating_sub(idle_for)) },
            })
        });
        let runtime = match &g.runtime {
            None => json!({"checked": false}),
            Some(Ok(r)) => json!({"checked": true, "ok": true, "path": r.path, "version": r.version, "build": r.build, "optional_flags": r.optional}),
            Some(Err(m)) => json!({"checked": true, "ok": false, "problem": m}),
        };
        json!({
            "ok": true,
            "state": match &g.loaded { Some(l) if l.ready => "ready", Some(_) => "loading", None => "idle" },
            "model": model,
            "runtime": runtime,
            "settings": match &cfg {
                Ok(c) => json!({"memory_budget_bytes": c.memory_budget_bytes, "memory_budget": memory::gib(c.memory_budget_bytes), "idle_seconds": c.idle_seconds, "context": c.context, "llama_server": c.llama_server}),
                Err(e) => json!({"error": format!("{e:#}")}),
            },
            "registered": match &reg { Ok(r) => json!(r.models.iter().map(|m| m.name.clone()).collect::<Vec<_>>()), Err(e) => json!({"error": format!("{e:#}")}) },
            "in_flight": g.in_flight.as_ref().map(|f| json!({"request_id": f.request_id, "started_at": f.started_at})),
            "served": g.served,
            "crashes_in_a_row": g.crashes,
            "last_crash": g.last_crash,
            "backoff_until": g.backoff_until.as_ref().filter(|(i, _)| Instant::now() < *i).map(|(_, at)| at.clone()),
            "last_unload": g.last_unload,
        })
    }
}

fn cfg_load_timeout(_c: &Config) -> u64 {
    std::env::var("CAULDRON_INFERENCE_LOAD_TIMEOUT_S").ok().and_then(|v| v.parse().ok()).filter(|n| *n > 0).unwrap_or(inference_registry::LOAD_TIMEOUT_S).min(inference_registry::LOAD_TIMEOUT_S)
}

/// **The child's argv** for `model` (the program first).
/// `real` is the model file's canonical path, as verified: the child opens
/// that file, not whatever a symlink points at by then.
pub fn argv(rt: &Runtime, model: &Model, real: &Path, context: u32, port: u16, key: &Path) -> Vec<String> {
    let mut a: Vec<String> = vec![rt.path.display().to_string()];
    let mut push = |xs: &[&str]| a.extend(xs.iter().map(|s| s.to_string()));
    push(&["--model", &real.display().to_string(), "--alias", &model.name]);
    push(&["--host", "127.0.0.1", "--port", &port.to_string(), "--api-key-file", &key.display().to_string()]);
    push(&["--ctx-size", &context.to_string(), "--parallel", "1"]);
    push(&["--no-webui", "--no-slots", "--offline", "--jinja"]);
    if rt.has("--no-agent") {
        push(&["--no-agent"]);
    }
    if rt.has("--reasoning") {
        push(&["--reasoning", "off"]);
    }
    if rt.has("--cache-ram") {
        push(&["--cache-ram", "0"]);
    }
    a
}

/// Is the child at `addr` ready (`GET /health` answers 200)?
fn health(addr: &SocketAddr, key: &str) -> bool {
    let Ok(s) = crate::http::connect(addr, Duration::from_secs(2)) else { return false };
    crate::http::send(&s, addr, "GET", "/health", Some(key), None).is_ok_and(|r| r.status == 200)
}

/// The last `n` characters of the file at `p`, on one line.
pub fn tail(p: &Path, n: u64) -> String {
    let Ok(mut f) = std::fs::File::open(p) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    use std::io::Seek;
    let _ = f.seek(std::io::SeekFrom::Start(len.saturating_sub(n)));
    let mut s = String::new();
    let _ = f.read_to_string(&mut s);
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The command of process `pid`, from `ps`.
fn command_of(pid: u32) -> Option<String> {
    let o = Command::new("/bin/ps").args(["-o", "comm=", "-p", &pid.to_string()]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}
