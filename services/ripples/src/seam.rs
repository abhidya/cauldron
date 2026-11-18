//! **The service's cauldron seam** (the `CAULDRON_BIND` line protocol),
//! registered with the warnings attestation as every cauldron service does.
//! Its own verbs, each answering one JSON line:
//!
//! - `RUN <key> {cauldron, dump, backend?, model?, force_budget?}`: start
//!   one run (`backend`: `threads`, the default, or `local`, the `inference`
//!   service on this machine, whose `model` is a registered model's name,
//!   or the only one registered when none is given; `force_budget`: the
//!   input tokens the owner let this run spend beyond the dump's cap, from
//!   `cauldron ripples run --force-budget`; `keep_answer: true`, from
//!   `cauldron ripples run --debug-keep-answer`: the model's answers and
//!   the refusals kept in one private file, its path in the outcome,
//!   `crate::engine::Opts::keep_answer`). A dump already
//!   processed (or removed, or unreadable) is answered at once (`done:
//!   true`, `result`); otherwise the run starts on its own thread and the
//!   answer names it (`done: false`, `run`). One run at a time: another
//!   `RUN` meanwhile is refused as `busy`.
//! - `STATUS <key> {run?, cauldron?}`: with `run`, that run (`running`, or
//!   its outcome); without, the service, the run in progress, the last ten
//!   runs (of `cauldron` when given; each `done`, `running` or
//!   `interrupted`), the spend today (per cauldron, against the daily cap)
//!   and per dump (across its runs, against the dump's cap), the
//!   evaluations' spend today (their own ledger, never the caps for runs),
//!   any call that may still be running in threads, and the caps.
//! - `EVAL <key> {cauldron, dumps, out, backends, threads_model?,
//!   local_model?, repeat?, max_calls?, plan?, confirmed?}`: start an
//!   evaluation (`crate::eval`): each fixture dump of the directory `dumps`
//!   on each backend, nothing written to the soup, the files into the new or
//!   empty directory `out` (both absolute). With the threads backend,
//!   `max_calls` (its bound) is required, and it starts only with
//!   `confirmed: true`; `plan: true` answers what it may spend
//!   (`crate::eval::plan`) and starts nothing. It takes the run slot (one
//!   run or evaluation at a time) and is followed like a run: `STATUS {run:
//!   <the eval id>}`.
//! - `DUMP <key> {cauldron, dump}`: the `mcp` service's word that a dump
//!   landed; wakes automatic processing (`crate::auto`) when it is on for
//!   that cauldron. Answers `{ok, automatic: on|off}`; a lost word costs
//!   only the wait until the next scan.
//! - `PING`.
//!
//! `STATUS` also shows automatic processing (`automatic`: per cauldron
//! with a setting, on or off, since when, the backend, the queue, and
//! `held` while the threads hold holds it; and `threads_hold`: since when,
//! the cause, the next attempt, which dump set it).
//!
//! `RUN`, `STATUS` and `DUMP` answer only a line carrying the run key
//! (`crate::key`), which `cauldron ripples` reads.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use env::seams::ServiceEnv;
use env::warnings::{Conn, Export, Service, ServesWarnings, Verb};
use serde_json::{json, Value};

use crate::engine::{self, Ask, Ctx};
use crate::journal;
use crate::key::Key;

/// What the seam's verbs share.
pub struct State {
    pub ctx: Ctx,
    pub key: Key,
    /// The run in progress: `(run, cauldron, dump)`.
    active: Mutex<Option<(String, String, String)>>,
    /// Is the run in progress an automatic one ([`State::take_slot`])?
    active_auto: std::sync::atomic::AtomicBool,
    /// Outcomes of the runs this process finished, by run.
    results: Mutex<HashMap<String, Value>>,
    pub started_at: String,
    /// Wakes the automatic worker (`crate::auto`), once it runs.
    wake: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

impl State {
    /// Take the one run slot for `run` (an automatic run); `false` when a
    /// run holds it.
    pub fn take_slot(&self, run: &str, cauldron: &str, dump: &str) -> bool {
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if active.is_some() {
            return false;
        }
        *active = Some((run.to_string(), cauldron.to_string(), dump.to_string()));
        self.active_auto.store(true, std::sync::atomic::Ordering::SeqCst);
        true
    }

    /// Let the slot go after `run`, keeping its outcome.
    pub fn finish(&self, run: &str, result: Value) {
        if !run.is_empty() {
            self.results.lock().unwrap_or_else(|p| p.into_inner()).insert(run.to_string(), result);
        }
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        self.active_auto.store(false, std::sync::atomic::Ordering::SeqCst);
        *active = None;
    }

    /// Where [`State::wake`] sends.
    pub fn set_wake(&self, tx: std::sync::mpsc::Sender<()>) {
        *self.wake.lock().unwrap_or_else(|p| p.into_inner()) = Some(tx);
    }

    /// Wake the automatic worker, if it runs.
    pub fn wake(&self) {
        if let Some(tx) = self.wake.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            let _ = tx.send(());
        }
    }

    /// `DUMP`: a word from the `mcp` service that a dump landed.
    pub fn dump(&self, rest: &str) -> Value {
        let a = match self.args(rest) {
            Ok(a) => a,
            Err(v) => return v,
        };
        let cauldron = a.get("cauldron").and_then(Value::as_str).unwrap_or("");
        let on = env::ripples_auto::is_on(&self.ctx.env, cauldron);
        if on {
            self.wake();
        }
        json!({"ok": true, "automatic": if on { "on" } else { "off" }})
    }

    pub fn new(ctx: Ctx, key: Key) -> Arc<State> {
        Arc::new(State {
            ctx,
            key,
            active: Mutex::new(None),
            results: Mutex::new(HashMap::new()),
            started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            wake: Mutex::new(None),
            active_auto: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn refuse(code: &str, message: impl Into<String>) -> Value {
        json!({"ok": false, "code": code, "message": message.into()})
    }

    fn args(&self, rest: &str) -> Result<serde_json::Map<String, Value>, Value> {
        let (given, body) = rest.split_once(' ').unwrap_or((rest, ""));
        if given.is_empty() || !self.key.is(given) {
            return Err(State::refuse("unauthorized", "this verb answers only a line carrying the ripples run key (run.secret)"));
        }
        match serde_json::from_str::<Value>(body.trim()) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Err(State::refuse("usage", "one JSON object follows the key")),
        }
    }

    /// `RUN`.
    pub fn run(self: &Arc<State>, rest: &str) -> Value {
        let a = match self.args(rest) {
            Ok(a) => a,
            Err(v) => return v,
        };
        let s = |k: &str| a.get(k).and_then(Value::as_str).filter(|v| !v.trim().is_empty()).map(str::to_string);
        let (Some(cauldron), Some(dump)) = (s("cauldron"), s("dump")) else {
            return State::refuse("usage", "RUN needs `cauldron` and `dump`");
        };
        let force_budget = match a.get("force_budget") {
            None | Some(Value::Null) => None,
            Some(v) => match v.as_u64().filter(|n| *n > 0) {
                Some(n) => Some(n),
                None => return State::refuse("usage", "`force_budget` is a whole number of input tokens above 0"),
            },
        };
        let model = match s("backend").as_deref() {
            None | Some("threads") => s("model"),
            Some("local") => match local_model(&self.ctx.env, s("model")) {
                Ok(m) => Some(m),
                Err(v) => return v,
            },
            Some(other) => return State::refuse("usage", format!("`backend` is `threads` or `local`, not `{other}`")),
        };
        let keep_answer = a.get("keep_answer") == Some(&json!(true));
        let ask = Ask { cauldron, dump, model, force_budget };
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((run, c, d)) = active.as_ref() {
            if self.active_auto.load(std::sync::atomic::Ordering::SeqCst) {
                let mut v = State::refuse(
                    "busy",
                    format!(
                        "automatic processing holds the run slot (dump {d} of cauldron {c}); one run at a time. Try again in a moment: the slot is let go between dumps. Or turn automatic processing off for that cauldron (`cauldron ripples off {c} --env {}`).",
                        self.ctx.env.name
                    ),
                );
                v["held_by"] = json!("automatic");
                return v;
            }
            return State::refuse("busy", format!("run {run} (dump {d} of cauldron {c}) is running; one run at a time. Try again when it ends; `cauldron ripples status` follows it."));
        }
        // Nothing runs while a call an earlier run left may still be spending.
        engine::settle(&self.ctx);
        let p = match engine::prepare(&self.ctx, &ask) {
            Ok(p) => p,
            Err(result) => return json!({"ok": true, "done": true, "result": result}),
        };
        *active = Some((p.run.clone(), ask.cauldron.clone(), ask.dump.clone()));
        drop(active);
        let me = self.clone();
        let run = p.run.clone();
        std::thread::Builder::new()
            .name("ripple-run".into())
            .spawn(move || {
                let result = engine::execute_with(&me.ctx, &p, &engine::Opts { keep_answer, ..engine::Opts::default() });
                tracing::info!(run = %p.run, outcome = %result["outcome"], "a ripple run ended");
                me.results.lock().unwrap_or_else(|p| p.into_inner()).insert(p.run.clone(), result);
                *me.active.lock().unwrap_or_else(|p| p.into_inner()) = None;
                // Automatic processing may have waited for the slot.
                me.wake();
            })
            .map(|_| json!({"ok": true, "done": false, "run": run}))
            .unwrap_or_else(|e| State::refuse("internal", format!("cannot start the run: {e}")))
    }

    /// `EVAL`.
    pub fn eval(self: &Arc<State>, rest: &str) -> Value {
        let a = match self.args(rest) {
            Ok(a) => a,
            Err(v) => return v,
        };
        let s = |k: &str| a.get(k).and_then(Value::as_str).filter(|v| !v.trim().is_empty()).map(str::to_string);
        let (Some(cauldron), Some(dumps), Some(out)) = (s("cauldron"), s("dumps"), s("out")) else {
            return State::refuse("usage", "EVAL needs `cauldron`, `dumps` and `out`");
        };
        let (dumps, out) = (std::path::PathBuf::from(dumps), std::path::PathBuf::from(out));
        if !dumps.is_absolute() || !out.is_absolute() {
            return State::refuse("usage", "`dumps` and `out` are absolute paths");
        }
        let backends: Vec<String> = a.get("backends").and_then(Value::as_array).map(|b| b.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
        if backends.is_empty() || backends.iter().any(|b| !matches!(b.as_str(), "threads" | "local")) {
            return State::refuse("usage", "`backends` is a list of `threads` and `local`");
        }
        let threads_model = s("threads_model").unwrap_or_else(|| engine::FIRST_MODEL.to_string());
        if !engine::model_ok(&threads_model) || engine::is_local(&threads_model) {
            return State::refuse("usage", format!("`{threads_model}` is not a threads model id (sonnet, opus, …)"));
        }
        let local_model = if backends.iter().any(|b| b == "local") {
            match local_model(&self.ctx.env, s("local_model")) {
                Ok(m) => Some(m),
                Err(v) => return v,
            }
        } else {
            None
        };
        let repeat = match a.get("repeat") {
            None | Some(Value::Null) => 1,
            Some(v) => match v.as_u64().filter(|n| (1..=crate::eval::MAX_REPEAT as u64).contains(n)) {
                Some(n) => n as u32,
                None => return State::refuse("usage", format!("`repeat` is 1 to {}", crate::eval::MAX_REPEAT)),
            },
        };
        let threads = backends.iter().any(|b| b == "threads");
        let max_calls = match a.get("max_calls") {
            None | Some(Value::Null) => None,
            Some(v) => match v.as_u64().filter(|n| (1..=crate::eval::MAX_CALLS as u64).contains(n)) {
                Some(n) => Some(n as u32),
                None => return State::refuse("usage", format!("`max_calls` is 1 to {}", crate::eval::MAX_CALLS)),
            },
        };
        if threads && max_calls.is_none() {
            return State::refuse("usage", format!("the threads backend is the owner's Claude spend: an evaluation with it needs `max_calls` (1 to {}), its bound; nothing was sent", crate::eval::MAX_CALLS));
        }
        let id = crate::eval::new_id();
        let spec = crate::eval::Spec { cauldron: cauldron.clone(), dumps, out: out.clone(), backends, threads_model, local_model, repeat, max_calls, id: id.clone() };
        if a.get("plan") == Some(&json!(true)) {
            return match crate::eval::plan(&self.ctx, &spec) {
                Ok(p) => json!({"ok": true, "plan": p}),
                Err(m) => State::refuse("usage", m),
            };
        }
        if threads && a.get("confirmed") != Some(&json!(true)) {
            return State::refuse(
                "unconfirmed",
                "the threads backend is the owner's Claude spend: ask for the evaluation's `plan` first, then send it again with `confirmed: true` (`cauldron ripples eval … --yes`); nothing was sent",
            );
        }
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((run, c, d)) = active.as_ref() {
            return State::refuse("busy", format!("run {run} ({d} of cauldron {c}) is running; one run or evaluation at a time"));
        }
        engine::settle(&self.ctx);
        *active = Some((id.clone(), cauldron, "eval".into()));
        drop(active);
        let me = self.clone();
        let eval_id = id.clone();
        std::thread::Builder::new()
            .name("ripple-eval".into())
            .spawn(move || {
                let result = crate::eval::run(&me.ctx, &spec);
                tracing::info!(eval = %eval_id, outcome = %result["outcome"], "an evaluation ended");
                me.results.lock().unwrap_or_else(|p| p.into_inner()).insert(eval_id, result);
                *me.active.lock().unwrap_or_else(|p| p.into_inner()) = None;
                // Automatic processing may have waited for the slot.
                me.wake();
            })
            .map(|_| json!({"ok": true, "done": false, "run": id, "out": out}))
            .unwrap_or_else(|e| State::refuse("internal", format!("cannot start the evaluation: {e}")))
    }

    /// `STATUS`.
    pub fn status(&self, rest: &str) -> Value {
        let a = match self.args(rest) {
            Ok(a) => a,
            Err(v) => return v,
        };
        let active = self.active.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let me = std::process::id() as u64;
        let live = |run: &str, pid: u64| pid == me && active.as_ref().is_some_and(|(r, _, _)| r == run);
        let lines = journal::read(self.ctx.journal.path());
        if let Some(run) = a.get("run").and_then(Value::as_str) {
            if active.as_ref().is_some_and(|(r, _, _)| r == run) {
                let steps: Vec<Value> = lines.iter().filter(|l| l["run"] == run).map(|l| json!({"at": l["at"], "event": l["event"], "model": l["model"]})).collect();
                return json!({"ok": true, "run": run, "state": "running", "steps": steps});
            }
            if let Some(r) = self.results.lock().unwrap_or_else(|p| p.into_inner()).get(run) {
                return json!({"ok": true, "run": run, "state": "done", "result": r});
            }
            return match journal::runs(&lines, &live).into_iter().rev().find(|v| v["run"] == run) {
                Some(v) => json!({"ok": true, "run": run, "state": v["state"], "view": v}),
                None => State::refuse("not_found", format!("no run {run} in the journal")),
            };
        }
        let cauldron = a.get("cauldron").and_then(Value::as_str);
        let mut runs: Vec<Value> = journal::runs(&lines, &live).into_iter().filter(|v| cauldron.is_none_or(|c| v["cauldron"] == c)).collect();
        let keep = runs.len().saturating_sub(10);
        runs.drain(..keep);
        let running = active.as_ref().map(|(r, _, _)| r.clone());
        let ledger = journal::read(self.ctx.journal.eval_ledger().path());
        let owners = env::ripples_auto::Owners::of(&self.ctx.env);
        let unsettled: Vec<Value> = journal::unsettled(&lines)
            .into_iter()
            .chain(journal::unsettled(&ledger))
            .filter(|x| running.as_deref() != x["run"].as_str() && cauldron.is_none_or(|c| x["cauldron"] == c))
            .map(|x| json!({"run": x["run"], "cauldron": x["cauldron"], "dump": x["dump"], "thread": x["thread"], "how_to_stop": x["how_to_stop"]}))
            .collect();
        json!({
            "ok": true,
            "service": {"pid": me, "started_at": self.started_at, "backend": self.ctx.inference.name(), "env": self.ctx.env.name},
            "running": active.map(|(r, c, d)| json!({"run": r, "cauldron": c, "dump": d})),
            "runs": runs,
            "today": today(&lines, cauldron, &owners),
            "evaluations_today": today(&ledger, cauldron, &owners),
            "per_dump": journal::per_dump(&lines, cauldron).into_iter().take(20).collect::<Vec<_>>(),
            "possibly_running": unsettled,
            "caps": caps_json(&self.ctx.caps),
            "journal": self.ctx.journal.path().display().to_string(),
            "automatic": self.automatic(cauldron),
        })
    }
}

impl State {
    /// Automatic processing, per cauldron with a setting (or `cauldron`
    /// only): `env::ripples_auto::queue`, its dumps read through the `mcp`
    /// service.
    pub fn automatic(&self, cauldron: Option<&str>) -> Value {
        let root = &self.ctx.env.data_root;
        let lines = env::ripples_auto::read(root);
        let now = crate::auto::now();
        let active = self.active.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let hold = env::ripples_auto::threads_hold(&lines);
        let list: Vec<Value> = env::ripples_auto::all(&self.ctx.env)
            .into_iter()
            .filter(|x| cauldron.is_none_or(|c| x.name == c))
            .map(|x| {
                let rs = if x.void.is_none() { crate::auto::received(self, &x.settings).ok() } else { None };
                let running = active.as_ref().filter(|(_, c, _)| *c == x.name).map(|(_, _, d)| d.as_str());
                let mut q = env::ripples_auto::queue(&x, &self.ctx.env.name, rs.as_deref(), &env::ripples_auto::dump_states(&lines, &x.settings), &now, running);
                hold.mark(&mut q, &x.settings, &now);
                // The one run slot taken by another run (by hand, an
                // evaluation, or another cauldron's): why its dumps wait.
                if let Some((r, c, d)) = active.as_ref().filter(|(_, c, _)| *c != x.name || !self.active_auto.load(std::sync::atomic::Ordering::SeqCst)) {
                    if q["waiting"].as_array().is_some_and(|w| !w.is_empty()) {
                        q["waits_for"] = json!({"run_slot": {"run": r, "cauldron": c, "dump": d}, "note": "one run at a time: the waiting dumps start when that run ends (or at the next scan)"});
                    }
                }
                q
            })
            .collect();
        json!({
            "threads_hold": hold.view(&now),
            "cauldrons": list,
            "off_elsewhere": "every cauldron not listed here is off: nothing is processed automatically unless its owner turned it on (`cauldron ripples on`)",
            "journal": env::ripples_auto::journal_path(root).display().to_string(),
        })
    }
}

/// **The model id of a local run**: `local:<name>` for the name given (or
/// already prefixed), else for the only registered model; a refusal that
/// says what is registered otherwise.
pub fn local_model(e: &env::Environment, given: Option<String>) -> Result<String, Value> {
    let prefix = crate::local::PREFIX;
    if let Some(m) = given {
        let name = m.strip_prefix(prefix).unwrap_or(&m).to_string();
        if !inference_registry::name_ok(&name) {
            return Err(State::refuse("usage", format!("`{m}` is not a registered model's name (`cauldron inference models list`)")));
        }
        return Ok(format!("{prefix}{name}"));
    }
    let reg = inference_registry::Registry::load(&e.data_root).map_err(|err| State::refuse("usage", format!("{err:#}")))?;
    match reg.default_model() {
        Ok(m) => Ok(format!("{prefix}{}", m.name)),
        Err(err) => Err(State::refuse("usage", format!("{err:#}"))),
    }
}

/// The caps, as `STATUS` shows them.
pub fn caps_json(c: &crate::engine::Caps) -> Value {
    json!({
        "calls_per_dump": c.calls,
        "input_tokens_per_dump": c.input_tokens,
        "input_tokens_per_cauldron_per_day": c.day_tokens,
        "dumps_per_cauldron_per_day": c.day_dumps,
        "counted": "from the journal, across every run of a dump; a call is charged when it begins, as one call and its estimate, and settled when it ends to the larger of its reported input tokens and that estimate; a call cut short by a kill or a stop stays charged at its estimate (`unsettled_calls`)",
    })
}

/// Today's (UTC) spend, from the journal: every cauldron's (or `cauldron`'s)
/// calls, charged input tokens and dumps, grouped by the cauldron's
/// identity (`env::ripples_auto::Owners`) and shown under its current name,
/// so a rename does not split a day's spend and a cauldron made again under
/// an old name does not inherit it. `cauldron` is a current name.
pub fn today(lines: &[Value], cauldron: Option<&str>, owners: &env::ripples_auto::Owners) -> Value {
    let day = journal::today();
    let charges = journal::charges(lines);
    let mut u = crate::inference::Usage::default();
    let mut per = serde_json::Map::new();
    let owner = |c: &journal::Charge| owners.of_line(c.identity.as_deref(), &c.cauldron);
    let wanted = |o: &env::ripples_auto::Owner| cauldron.is_none_or(|x| !o.gone && o.name == x);
    let mut dumps: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> = Default::default();
    for c in charges.iter().filter(|c| c.at.starts_with(&day)) {
        let o = owner(c);
        if !wanted(&o) {
            continue;
        }
        let e = per.entry(o.key.clone()).or_insert_with(|| json!({"calls": 0, "charged_input_tokens": 0, "dumps": 0, "cauldron_id": o.id, "gone": o.gone, "names": []}));
        e["calls"] = json!(e["calls"].as_u64().unwrap_or(0) + 1);
        e["charged_input_tokens"] = json!(e["charged_input_tokens"].as_u64().unwrap_or(0) + c.tokens);
        if let Some(n) = e["names"].as_array_mut() {
            if !n.contains(&json!(c.cauldron)) {
                n.push(json!(c.cauldron));
            }
        }
        let d = dumps.entry(o.key.clone()).or_default();
        d.insert(c.dump.clone());
        e["dumps"] = json!(d.len());
    }
    let mine = |c: &journal::Charge| c.at.starts_with(&day) && wanted(&owner(c));
    let calls = charges.iter().filter(|c| mine(c)).count();
    let unsettled = charges.iter().filter(|c| mine(c) && !c.settled).count();
    let runs: std::collections::HashSet<&str> = charges.iter().filter(|c| mine(c)).map(|c| c.run.as_str()).collect();
    for l in lines.iter().filter(|l| l["event"] == "called" && l["at"].as_str().is_some_and(|a| a.starts_with(&day))) {
        if cauldron.is_some() && !l["run"].as_str().is_some_and(|r| runs.contains(r)) {
            continue;
        }
        if let Ok(x) = serde_json::from_value::<crate::inference::Usage>(l["usage"].clone()) {
            u.add(&x);
        }
    }
    json!({"date": day, "calls": calls, "unsettled_calls": unsettled, "usage": u, "input_total": u.input_total(), "per_cauldron": per})
}

/// One running instance, as cauldron's line protocol sees it.
pub struct RipplesSeam {
    pub se: ServiceEnv,
    pub addr: SocketAddr,
    pub started_at: String,
    pub state: Arc<State>,
}

impl ServesWarnings for RipplesSeam {
    const SHAPE: &'static str = env::warnings::SHAPE;

    fn warnings(&self, asked: &str) -> Result<Export> {
        if !asked.is_empty() && asked != Self::SHAPE {
            bail!("this service serves the `{}` shape, not `{asked}`", Self::SHAPE);
        }
        Ok(shapewire::warnings::COUNTER.export(&self.se.service, Self::SHAPE, &self.started_at))
    }
}

impl Service for RipplesSeam {
    const VERBS: &'static [Verb<RipplesSeam>] = &[Verb::new("RUN", RipplesSeam::v_run), Verb::new("STATUS", RipplesSeam::v_status), Verb::new("EVAL", RipplesSeam::v_eval), Verb::new("PING", RipplesSeam::v_ping), Verb::new("DUMP", RipplesSeam::v_dump)];

    fn service_env(&self) -> &ServiceEnv {
        &self.se
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl RipplesSeam {
    fn v_run(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        c.line(&self.state.run(rest).to_string())?;
        Ok(())
    }

    fn v_status(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        c.line(&self.state.status(rest).to_string())?;
        Ok(())
    }

    fn v_eval(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        c.line(&self.state.eval(rest).to_string())?;
        Ok(())
    }

    fn v_dump(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        c.line(&self.state.dump(rest).to_string())?;
        Ok(())
    }

    fn v_ping(&self, c: &mut Conn<'_>, _rest: &str) -> Result<()> {
        c.line("PONG")?;
        Ok(())
    }
}

/// The `tracing` layer that counts every WARN and ERROR by call site.
pub struct WarningsLayer;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for WarningsLayer {
    fn on_event(&self, e: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        shapewire::warnings::COUNTER.record_event(e);
    }
}
