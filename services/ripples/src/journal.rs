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
/// **The evaluations' ledger**, beside the journal: an evaluation's threads
/// calls are charged here, with the same `call` / `sent` / `cancel` /
/// `called` lines, and never in [`FILE`], so they never count against a
/// cauldron's caps for runs (its daily dump slots above all).
pub const EVAL_FILE: &str = "eval.jsonl";

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

    /// The evaluations' ledger beside this journal ([`EVAL_FILE`]).
    pub fn eval_ledger(&self) -> Journal {
        Journal::at(&self.path.with_file_name(EVAL_FILE))
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
        // An evaluation's calls are spend, not runs.
        if l["eval"] == json!(true) {
            continue;
        }
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
            for k in ["model", "package", "backend", "automatic"] {
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
    /// The cauldron's identity its `call` line carries (`cauldron_id`,
    /// [`crate::engine::cauldron_identity`]); `None` on a line an earlier
    /// build wrote.
    pub identity: Option<String>,
    pub dump: String,
    /// When the call began (its `call` line), UTC.
    pub at: String,
    /// Charged input tokens: the estimate while the call is unsettled, then
    /// what its `called` line charged.
    pub tokens: u64,
    /// `false` while no `called` line ended it: a call in progress, or one
    /// a kill or a stop cut short, which stays charged at its estimate.
    pub settled: bool,
    /// A `voided` line names it: its answer was never judged on its merits,
    /// because another writer changed or held a region the answer went into
    /// (`region_changed`, `region_locked` at the put). It still counts
    /// against the cauldron's daily caps, and never against the dump's own
    /// ([`spent`]), so other writers' activity alone can never use up a
    /// dump's lifetime cap.
    pub voided: bool,
}

/// **Every model call, charged from the moment it began.** A `call` line
/// (written, under the journal's lock, before anything is sent) is one call
/// charged its `estimated_input_tokens`; the `called` line that ends it
/// (same run, same `call_id`; a line without one ends the newest open call
/// of its run) settles it to what it reports. A `called` line with no
/// `call` before it counts on its own.
pub fn charges(lines: &[Value]) -> Vec<Charge> {
    let mut out: Vec<Charge> = Vec::new();
    // The `call_id` of each charge, by index.
    let mut ids: Vec<Value> = Vec::new();
    let mut open: Vec<(usize, Value)> = Vec::new();
    let mut dropped: Vec<usize> = Vec::new();
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    for l in lines {
        // An evaluation's call journaled here by an earlier build: spend of
        // the evaluation, not of the cauldron's runs.
        if l["eval"] == json!(true) {
            continue;
        }
        match l["event"].as_str() {
            Some("call") => {
                open.push((out.len(), l["call_id"].clone()));
                out.push(Charge {
                    run: s(&l["run"]),
                    cauldron: s(&l["cauldron"]),
                    identity: l["cauldron_id"].as_str().map(str::to_string),
                    dump: s(&l["dump"]),
                    at: s(&l["at"]),
                    tokens: l["estimated_input_tokens"].as_u64().unwrap_or(0),
                    settled: false,
                    voided: false,
                });
                ids.push(l["call_id"].clone());
            }
            Some("called") => {
                let run = s(&l["run"]);
                let found = open.iter().rposition(|(i, id)| out[*i].run == run && (l["call_id"].is_null() || *id == l["call_id"]));
                // A call threads held before it ran (`held`) never was one.
                let held = l["held"] == json!(true);
                match found {
                    Some(k) => {
                        let (i, _) = open.remove(k);
                        if held {
                            dropped.push(i);
                        }
                        out[i].tokens = charged(l);
                        out[i].settled = true;
                    }
                    None if held => {}
                    None => {
                        out.push(Charge { run, cauldron: s(&l["cauldron"]), identity: None, dump: s(&l["dump"]), at: s(&l["at"]), tokens: charged(l), settled: true, voided: false });
                        ids.push(l["call_id"].clone());
                    }
                }
            }
            Some("voided") if !l["call_id"].is_null() => {
                let run = s(&l["run"]);
                if let Some(i) = (0..out.len()).rev().find(|i| out[*i].run == run && ids[*i] == l["call_id"]) {
                    out[i].voided = true;
                }
            }
            _ => {}
        }
    }
    let mut i = 0;
    out.retain(|_| {
        i += 1;
        !dropped.contains(&(i - 1))
    });
    out
}

/// **How long each region of `cauldron` was held by this journal's runs
/// within the `window_ms` before `now_ms`**: `{region: ms}`. A hold begins
/// at a run's `locked` line (its `regions`, the ones held `write`) and ends
/// at its next `locked`, `unlocked` or `done` line; one with no end yet runs
/// to `now_ms`. What a ripple's hourly hold budget per region is counted
/// from.
pub fn region_hold_ms(lines: &[Value], cauldron: &str, now_ms: i64, window_ms: i64) -> std::collections::BTreeMap<String, i64> {
    let ms = |l: &Value| chrono::DateTime::parse_from_rfc3339(l["at"].as_str().unwrap_or("")).map(|t| t.timestamp_millis()).ok();
    let from = now_ms - window_ms;
    let mut out: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    let mut open: std::collections::HashMap<String, (i64, Vec<String>)> = std::collections::HashMap::new();
    let mut close = |out: &mut std::collections::BTreeMap<String, i64>, (start, regions): (i64, Vec<String>), end: i64| {
        let span = (end.min(now_ms) - start.max(from)).max(0);
        for r in regions {
            *out.entry(r).or_insert(0) += span;
        }
    };
    for l in lines {
        if l["cauldron"].as_str() != Some(cauldron) {
            continue;
        }
        let (Some(run), Some(at)) = (l["run"].as_str(), ms(l)) else { continue };
        match l["event"].as_str() {
            Some("locked") | Some("unlocked") | Some("done") => {
                if let Some(h) = open.remove(run) {
                    close(&mut out, h, at);
                }
                if l["event"] == "locked" {
                    let regions: Vec<String> = l["regions"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
                    if !regions.is_empty() {
                        open.insert(run.to_string(), (at, regions));
                    }
                }
            }
            _ => {}
        }
    }
    for (_, h) in open.drain() {
        close(&mut out, h, now_ms);
    }
    out
}

/// **What a dump's run has spent, across every attempt**: `(calls, charged
/// input tokens)` of every call it began ([`charges`]), less the calls
/// voided by another writer's region change or hold ([`Charge::voided`]):
/// the dump's own caps count only answers judged on their merits.
pub fn spent(lines: &[Value], run: &str) -> (u32, u64) {
    charges(lines).iter().filter(|c| c.run == run && !c.voided).fold((0, 0), |(n, t), c| (n + 1, t + c.tokens))
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
/// that began that day ([`charges`]). With `identity` (the daily cap's
/// count), a call is the cauldron's when its line carries that identity,
/// whatever name it was made under; a line with no identity (an earlier
/// build's) is counted by its name. Without, by name.
pub fn day(lines: &[Value], cauldron: &str, identity: Option<&str>, date: &str) -> Day {
    let ids: Vec<String> = identity.into_iter().map(str::to_string).collect();
    day_of(lines, cauldron, &ids, date)
}

/// [`day`], a call counting as the cauldron's when its line carries any of
/// `identities` (the cauldron's identity now, and the one it had before it
/// was given its id); by name when `identities` is empty.
pub fn day_of(lines: &[Value], cauldron: &str, identities: &[String], date: &str) -> Day {
    let mut d = Day::default();
    let mine = |c: &Charge| match (identities.is_empty(), c.identity.as_deref()) {
        (false, Some(had)) => identities.iter().any(|w| w == had),
        _ => c.cauldron == cauldron,
    };
    for c in charges(lines).into_iter().filter(|c| mine(c) && c.at.starts_with(date)) {
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

/// **The region locks runs took and did not let go**: every run whose last
/// `locked` line (with regions) has no `unlocked` line after it, as `(run,
/// cauldron, dump)`, oldest first. A service killed while a run held its
/// lock leaves one; [`crate::engine::settle`] lets it go.
pub fn held_locks(lines: &[Value]) -> Vec<(String, String, String)> {
    let mut held: Vec<(String, String, String)> = Vec::new();
    for l in lines {
        let run = l["run"].as_str().unwrap_or("").to_string();
        match l["event"].as_str() {
            Some("locked") if l["regions"].as_array().is_some_and(|r| !r.is_empty()) => {
                held.retain(|(r, _, _)| *r != run);
                held.push((run, l["cauldron"].as_str().unwrap_or("").to_string(), l["dump"].as_str().unwrap_or("").to_string()));
            }
            Some("unlocked") => held.retain(|(r, _, _)| *r != run),
            _ => {}
        }
    }
    held
}
