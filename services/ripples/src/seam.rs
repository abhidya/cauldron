//! **The service's cauldron seam** (the `CAULDRON_BIND` line protocol),
//! registered with the warnings attestation as every cauldron service does.
//! Its own verbs, each answering one JSON line:
//!
//! - `RUN <key> {cauldron, dump, model?, force_budget?}`: start one run
//!   (`force_budget`: the input tokens the owner let this run spend beyond
//!   the dump's cap, from `cauldron ripples run --force-budget`). A dump already
//!   processed (or removed, or unreadable) is answered at once (`done:
//!   true`, `result`); otherwise the run starts on its own thread and the
//!   answer names it (`done: false`, `run`). One run at a time: another
//!   `RUN` meanwhile is refused as `busy`.
//! - `STATUS <key> {run?, cauldron?}`: with `run`, that run (`running`, or
//!   its outcome); without, the service, the run in progress, the last ten
//!   runs (of `cauldron` when given; each `done`, `running` or
//!   `interrupted`), the spend today (per cauldron, against the daily cap)
//!   and per dump (across its runs, against the dump's cap), any call that
//!   may still be running in threads, and the caps.
//! - `PING`.
//!
//! `RUN` and `STATUS` answer only a line carrying the run key
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
    /// Outcomes of the runs this process finished, by run.
    results: Mutex<HashMap<String, Value>>,
    pub started_at: String,
}

impl State {
    pub fn new(ctx: Ctx, key: Key) -> Arc<State> {
        Arc::new(State {
            ctx,
            key,
            active: Mutex::new(None),
            results: Mutex::new(HashMap::new()),
            started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
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
        let ask = Ask { cauldron, dump, model: s("model"), force_budget };
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((run, c, d)) = active.as_ref() {
            return State::refuse("busy", format!("run {run} (dump {d} of cauldron {c}) is running; one run at a time. `cauldron ripples status` follows it."));
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
                let result = engine::execute(&me.ctx, &p);
                tracing::info!(run = %p.run, outcome = %result["outcome"], "a ripple run ended");
                me.results.lock().unwrap_or_else(|p| p.into_inner()).insert(p.run.clone(), result);
                *me.active.lock().unwrap_or_else(|p| p.into_inner()) = None;
            })
            .map(|_| json!({"ok": true, "done": false, "run": run}))
            .unwrap_or_else(|e| State::refuse("internal", format!("cannot start the run: {e}")))
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
        let unsettled: Vec<Value> = journal::unsettled(&lines)
            .into_iter()
            .filter(|x| running.as_deref() != x["run"].as_str() && cauldron.is_none_or(|c| x["cauldron"] == c))
            .map(|x| json!({"run": x["run"], "cauldron": x["cauldron"], "dump": x["dump"], "thread": x["thread"], "how_to_stop": x["how_to_stop"]}))
            .collect();
        json!({
            "ok": true,
            "service": {"pid": me, "started_at": self.started_at, "backend": self.ctx.inference.name(), "env": self.ctx.env.name},
            "running": active.map(|(r, c, d)| json!({"run": r, "cauldron": c, "dump": d})),
            "runs": runs,
            "today": today(&lines, cauldron),
            "per_dump": journal::per_dump(&lines, cauldron).into_iter().take(20).collect::<Vec<_>>(),
            "possibly_running": unsettled,
            "caps": caps_json(&self.ctx.caps),
            "journal": self.ctx.journal.path().display().to_string(),
        })
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
/// calls, charged input tokens and dumps.
pub fn today(lines: &[Value], cauldron: Option<&str>) -> Value {
    let day = journal::today();
    let charges = journal::charges(lines);
    let mut names: Vec<String> = charges.iter().map(|c| c.cauldron.clone()).collect();
    names.sort();
    names.dedup();
    let mut u = crate::inference::Usage::default();
    let mut per = serde_json::Map::new();
    for c in names.iter().filter(|c| cauldron.is_none_or(|x| x == c.as_str())) {
        let d = journal::day(lines, c, &day);
        if d.calls == 0 {
            continue;
        }
        per.insert(c.clone(), json!({"calls": d.calls, "charged_input_tokens": d.tokens, "dumps": d.dumps.len()}));
    }
    let mine = |c: &journal::Charge| c.at.starts_with(&day) && cauldron.is_none_or(|x| c.cauldron == x);
    let calls = charges.iter().filter(|c| mine(c)).count();
    let unsettled = charges.iter().filter(|c| mine(c) && !c.settled).count();
    for l in lines.iter().filter(|l| l["event"] == "called" && l["at"].as_str().is_some_and(|a| a.starts_with(&day))) {
        if cauldron.is_some_and(|c| l["cauldron"] != c) {
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
    const VERBS: &'static [Verb<RipplesSeam>] = &[Verb::new("RUN", RipplesSeam::v_run), Verb::new("STATUS", RipplesSeam::v_status), Verb::new("PING", RipplesSeam::v_ping)];

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
