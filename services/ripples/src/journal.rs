//! **The run journal**: `<data root>/ripples/runs.jsonl`, one JSON line per
//! step of every run, each appended in one write and synced before the run
//! goes on, so a run cut short (killed, stopped, crashed) is visible as one
//! that never reached `done`. A line never holds dump text or the prompt:
//! ids, counts, models, token counts and outcomes only.
//!
//! Every line is `{at, run, cauldron, dump, pid, event, ...}`. The events, in
//! a run's order: `started` (the model asked for, the package, the backend,
//! a `force_budget` the owner gave), `read` (the prompt's characters and its
//! token estimate), per model call `call` (written before anything is sent,
//! with the call's estimate: from here the call is charged), `sent` (the backend's handle of
//! the call in flight: the threads thread and message), `cancel` (when the
//! call was given up: whether threads confirmed it stopped) and `called`
//! (with that call's tokens and what the caps count it as, `charged`),
//! `refused` (a batch the door refused), `scrubbed` (the run's private
//! working directory removed), then `done` with the outcome (`processed`,
//! `already_processed`, `deferred`, `failed`), the records, and the run's
//! token totals.
//!
//! **The journal is what the caps are counted from** ([`charges`],
//! [`spent`], [`day`]): a dump's run id is the same at every attempt, so its
//! calls and tokens add up across runs and across restarts of the service.
//! A call is charged when it BEGINS (its `call` line, at its estimate) and
//! settled when it ends (`called`); a call a kill or a stop cut short never
//! gets its `called` line and stays charged.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Map, Value};

pub const FILE: &str = "runs.jsonl";

pub struct Journal {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Journal {
    pub fn new(dir: &Path) -> Journal {
        Journal::at(&dir.join(FILE))
    }

    /// The journal in the file `path`.
    pub fn at(path: &Path) -> Journal {
        Journal { path: path.to_path_buf(), lock: Mutex::new(()) }
    }

    /// Every line of this journal.
    pub fn lines(&self) -> Vec<Value> {
        read(&self.path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one line, synced. A journal that cannot be written is an
    /// error the run stops on: a run nobody can see is not run.
    pub fn write(&self, run: &str, cauldron: &str, dump: &str, event: &str, extra: Value) -> std::io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut line = json!({
            "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "run": run, "cauldron": cauldron, "dump": dump, "pid": std::process::id(), "event": event,
        });
        if let (Some(m), Value::Object(x)) = (line.as_object_mut(), extra) {
            m.extend(x);
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&self.path)?;
        f.write_all(format!("{line}\n").as_bytes())?;
        f.sync_all()
    }
}

impl Journal {
    /// **Begin a model call**: under an exclusive lock on the journal's
    /// lock file (`runs.jsonl.lock`), so no other run, in this process or
    /// another, counts the caps between this read and this write: read
    /// every line, let `check` decide from them (`Err` is the caller's
    /// answer and nothing is written), and append the `call` line with what
    /// `check` gave. That line charges the call ([`charges`]).
    pub fn begin<T>(&self, run: &str, cauldron: &str, dump: &str, check: impl FnOnce(&[Value]) -> Result<Value, T>) -> std::io::Result<Result<(), T>> {
        use std::os::unix::fs::OpenOptionsExt;
        let lock_path = PathBuf::from(format!("{}.lock", self.path.display()));
        let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(&lock_path)?;
        f.lock()?;
        let extra = match check(&self.lines()) {
            Ok(x) => x,
            Err(t) => return Ok(Err(t)),
        };
        self.write(run, cauldron, dump, "call", extra)?;
        drop(f);
        Ok(Ok(()))
    }
}

/// Every line of the journal at `path` (a missing file is none; a line
/// that does not parse is skipped).
pub fn read(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// The runs, one view each, newest last: `{run, cauldron, dump, started_at,
/// last_event, last_at, pid, state, outcome?, ...done fields}`. A run whose
/// last line is not `done` is `running` when `live` says its process is
/// still running it, else `interrupted`.
pub fn runs(lines: &[Value], live: &dyn Fn(&str, u64) -> bool) -> Vec<Value> {
    let mut order: Vec<String> = Vec::new();
    let mut views: std::collections::HashMap<String, Map<String, Value>> = std::collections::HashMap::new();
    for l in lines {
        let Some(run) = l["run"].as_str() else { continue };
        // A new attempt of a run already done starts a new view.
        let fresh = l["event"] == "started";
        if fresh || !views.contains_key(run) {
            if let Some(i) = order.iter().position(|r| r == run) {
                order.remove(i);
            }
            order.push(run.to_string());
            let mut v = Map::new();
            v.insert("run".into(), json!(run));
            v.insert("cauldron".into(), l["cauldron"].clone());
            v.insert("dump".into(), l["dump"].clone());
            v.insert("started_at".into(), l["at"].clone());
            views.insert(run.to_string(), v);
        }
        let v = views.get_mut(run).expect("inserted above");
        v.insert("last_event".into(), l["event"].clone());
        v.insert("last_at".into(), l["at"].clone());
        v.insert("pid".into(), l["pid"].clone());
        if l["event"] == "started" {
            for k in ["model", "package", "backend"] {
                if let Some(x) = l.get(k) {
                    v.insert(k.into(), x.clone());
                }
            }
        }
        if l["event"] == "done" {
            for (k, x) in l.as_object().into_iter().flatten() {
                if !["at", "run", "cauldron", "dump", "pid", "event"].contains(&k.as_str()) {
                    v.insert(k.clone(), x.clone());
                }
            }
        }
    }
    order
        .into_iter()
        .filter_map(|r| views.remove(&r))
        .map(|mut v| {
            let state = if v.get("last_event") == Some(&json!("done")) {
                "done"
            } else if live(v["run"].as_str().unwrap_or(""), v.get("pid").and_then(Value::as_u64).unwrap_or(0)) {
                "running"
            } else {
                "interrupted"
            };
            v.insert("state".into(), json!(state));
            Value::Object(v)
        })
        .collect()
}

/// What one `called` line counts against the caps: the larger of the
/// tokens the backend reported and the estimate made before the call (a
/// call cut short reports none, and may have spent them).
pub fn charged(l: &Value) -> u64 {
    l["charged"].as_u64().unwrap_or_else(|| l["input_total"].as_u64().unwrap_or(0))
}

/// One model call as the caps count it.
#[derive(Debug, Clone, PartialEq)]
pub struct Charge {
    pub run: String,
    pub cauldron: String,
    pub dump: String,
    /// When the call began (its `call` line), UTC.
    pub at: String,
    /// Charged input tokens: the estimate while the call is unsettled, then
    /// what its `called` line charged.
    pub tokens: u64,
    /// `false` while no `called` line ended it: a call in progress, or one
    /// a kill or a stop cut short, which stays charged at its estimate.
    pub settled: bool,
}

/// **Every model call, charged from the moment it began.** A `call` line
/// (written, under the journal's lock, before anything is sent) is one call
/// charged its `estimated_input_tokens`; the `called` line that ends it
/// (same run, same `call_id`; a line without one ends the newest open call
/// of its run) settles it to what it reports. A `called` line with no
/// `call` before it counts on its own.
pub fn charges(lines: &[Value]) -> Vec<Charge> {
    let mut out: Vec<Charge> = Vec::new();
    let mut open: Vec<(usize, Value)> = Vec::new();
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    for l in lines {
        match l["event"].as_str() {
            Some("call") => {
                open.push((out.len(), l["call_id"].clone()));
                out.push(Charge {
                    run: s(&l["run"]),
                    cauldron: s(&l["cauldron"]),
                    dump: s(&l["dump"]),
                    at: s(&l["at"]),
                    tokens: l["estimated_input_tokens"].as_u64().unwrap_or(0),
                    settled: false,
                });
            }
            Some("called") => {
                let run = s(&l["run"]);
                let found = open.iter().rposition(|(i, id)| out[*i].run == run && (l["call_id"].is_null() || *id == l["call_id"]));
                match found {
                    Some(k) => {
                        let (i, _) = open.remove(k);
                        out[i].tokens = charged(l);
                        out[i].settled = true;
                    }
                    None => out.push(Charge { run, cauldron: s(&l["cauldron"]), dump: s(&l["dump"]), at: s(&l["at"]), tokens: charged(l), settled: true }),
                }
            }
            _ => {}
        }
    }
    out
}

/// **What a dump's run has spent, across every attempt**: `(calls, charged
/// input tokens)` of every call it began ([`charges`]).
pub fn spent(lines: &[Value], run: &str) -> (u32, u64) {
    charges(lines).iter().filter(|c| c.run == run).fold((0, 0), |(n, t), c| (n + 1, t + c.tokens))
}

/// One day's spend of one cauldron.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Day {
    pub calls: u32,
    /// Charged input tokens ([`charges`]).
    pub tokens: u64,
    /// The dumps that had a call.
    pub dumps: std::collections::BTreeSet<String>,
}

/// **The spend of `cauldron` on `date`** (UTC, `YYYY-MM-DD`): every call
/// that began that day ([`charges`]).
pub fn day(lines: &[Value], cauldron: &str, date: &str) -> Day {
    let mut d = Day::default();
    for c in charges(lines).into_iter().filter(|c| c.cauldron == cauldron && c.at.starts_with(date)) {
        d.calls += 1;
        d.tokens += c.tokens;
        d.dumps.insert(c.dump);
    }
    d
}

/// Today, UTC, as `YYYY-MM-DD`.
pub fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// **Per dump, what it has spent**, newest first: `{cauldron, dump, run,
/// calls, charged_input_tokens, unsettled_calls, last_at}`.
/// `unsettled_calls` are calls with no recorded end (in progress, or cut
/// short by a kill or a stop): each is in `calls`, charged its estimate.
pub fn per_dump(lines: &[Value], cauldron: Option<&str>) -> Vec<Value> {
    let mut order: Vec<String> = Vec::new();
    let mut by: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for c in charges(lines).into_iter().filter(|c| cauldron.is_none_or(|x| c.cauldron == x)) {
        let e = by
            .entry(c.run.clone())
            .or_insert_with(|| json!({"cauldron": c.cauldron, "dump": c.dump, "run": c.run, "calls": 0, "charged_input_tokens": 0, "unsettled_calls": 0}));
        e["calls"] = json!(e["calls"].as_u64().unwrap_or(0) + 1);
        e["charged_input_tokens"] = json!(e["charged_input_tokens"].as_u64().unwrap_or(0) + c.tokens);
        if !c.settled {
            e["unsettled_calls"] = json!(e["unsettled_calls"].as_u64().unwrap_or(0) + 1);
        }
        e["last_at"] = json!(c.at);
        order.retain(|r| r != &c.run);
        order.push(c.run);
    }
    order.into_iter().rev().filter_map(|r| by.remove(&r)).collect()
}

/// **Calls that may still be running**: each `sent` line with no later
/// `called` or `cancel` line of the same run, and each `cancel` that was not
/// confirmed with no later confirmed one. A run in progress has one; a run
/// cut short by a kill keeps it until the call is cancelled.
pub fn unsettled(lines: &[Value]) -> Vec<Value> {
    let mut open: Vec<(String, Value)> = Vec::new();
    for l in lines {
        let Some(run) = l["run"].as_str() else { continue };
        match l["event"].as_str() {
            Some("sent") => {
                open.retain(|(r, _)| r != run);
                open.push((run.to_string(), l.clone()));
            }
            Some("called") => {
                // A call that ended and was not given up (an answer): settled
                // unless its cancel failed.
                if !open.iter().any(|(r, x)| r == run && x["event"] == "cancel") {
                    open.retain(|(r, _)| r != run);
                }
            }
            Some("cancel") => {
                let sent = open.iter().find(|(r, _)| r == run).map(|(_, x)| x.clone());
                open.retain(|(r, _)| r != run);
                if l["confirmed"] != json!(true) {
                    let mut x = sent.unwrap_or_else(|| l.clone());
                    x["event"] = json!("cancel");
                    x["thread"] = l["thread"].clone();
                    x["message_id"] = l["message_id"].clone();
                    x["how_to_stop"] = l["how_to_stop"].clone();
                    open.push((run.to_string(), x));
                }
            }
            _ => {}
        }
    }
    open.into_iter().map(|(_, x)| x).collect()
}
