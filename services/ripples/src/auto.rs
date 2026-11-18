//! **Automatic processing**: the service runs a dump without being asked,
//! for a cauldron whose owner turned it on (`cauldron ripples on`, at a
//! terminal; the setting is `env::ripples_auto`). Off unless turned on.
//!
//! # What wakes it
//!
//! - **The word from the `mcp` service**: when a dump lands through it (an
//!   MCP `soup_dump` or `soup_dump_session`, or `cauldron soup dump` while
//!   the `mcp` service holds the store), its side effect sends a `DUMP` line
//!   to this service's seam, which wakes the worker at once.
//! - **The scan**: when the service starts, and every
//!   `CAULDRON_RIPPLES_SCAN_MS` (60 s by default), the worker asks the `mcp`
//!   service for each turned-on cauldron's dumps still `received`
//!   (`RDUMPS`). That finds what the word missed: a dump that landed while
//!   this service was down, a word that was lost, and a dump `cauldron soup
//!   dump` wrote straight to the store (no side effect runs then).
//!
//! # What it runs
//!
//! Only a dump received after the cauldron's processing was turned on
//! (`since`: a dump id above the highest one held then), and a session
//! dump only when `--include-sessions` was given. One run at a time (the
//! service's one run slot, which `cauldron ripples run` also takes), the
//! oldest due dump first, one dump per cauldron per pass, the cauldron
//! served least recently first, so no cauldron waits behind another's
//! backlog, and none behind another whose calls threads keeps parking. A run is the same run `cauldron ripples
//! run` makes ([`crate::engine`]): the same run id, so it lands once; the
//! same caps, charged before each call; the same journal. Before every call
//! and before the put it reads the setting again: turned off, it stops
//! there (`deferred`, reason `off`) and nothing more is sent or written.
//!
//! # What becomes of a dump
//!
//! Every attempt is a line in the automatic journal (`auto.jsonl`) before
//! it starts, and its outcome a line after; [`env::ripples_auto`] reads them.
//!
//! - **processed**: records landed (or there was nothing worth recording),
//!   the dump is `processed`, with the run and the record ids.
//! - **deferred**: a cap was reached (the dump's own, or the cauldron's
//!   daily cap): the dump stays `received` with `meta.auto` `{state:
//!   deferred, reason: budget, ...}` and is tried again after midnight UTC
//!   (when the daily window resets; a dump at its own cap is then deferred
//!   again with nothing sent, until the owner runs it by hand with
//!   `--force-budget`). Or threads parked the call (an account over its
//!   usage threshold or limit, a spent credit balance, an expired login, a
//!   budget): `meta.auto` `{state: deferred, reason: threads_parked, park:
//!   <cause>, ...}`. That is not a failed attempt, and a park at admission
//!   (no tokens reported) is charged nothing and is not counted as a call.
//!   It sets the **threads hold** ([`threads_hold`]): no automatic run on
//!   threads, of any cauldron, starts until the wait is over (the retry
//!   wait, growing fourfold with each park in a row, at most an hour), so
//!   a parked account costs one withdrawn call per wait, never one per dump.
//!   `cauldron ripples status` shows the hold (`automatic.threads_hold`:
//!   since when, the cause, the next attempt, which dump set it) and marks
//!   each cauldron it holds (`held`).
//!   Or the backend was busy (another request held it, as a run by hand or
//!   another client of the local inference service does, or it was not
//!   ready yet): `meta.auto` `{state: deferred, reason: busy, ...}`, charged
//!   nothing, not a failed attempt, and tried again at the next scan.
//! - **retrying**: the attempt failed (the model could not be reached, the
//!   call failed, the service was killed or stopped during it): the dump
//!   stays `received` with `meta.auto` `{state: retrying, failed_attempts,
//!   last_code, retry_after}` and is tried again after a wait that grows
//!   fourfold each time (`CAULDRON_RIPPLES_RETRY_MS`, 2 minutes by default,
//!   then 8, then 32).
//! - **failed**: after [`env::ripples_auto::MAX_ATTEMPTS`] failed attempts,
//!   or at once when trying again cannot help (the answer was refused after
//!   its retry, the cauldron has no region, threads is too old), the dump is
//!   parked in state `failed` with `meta.failure` (the code, the message,
//!   the attempts). It is never tried automatically again; `cauldron
//!   ripples run` still runs it by hand.
//! - **skipped**: removed, or turned off during the run.
//!
//! Every attempt, retry and failure is bounded: at most three failed
//! attempts per dump, each held to the dump's caps (3 calls across every
//! run of it), so a dump that keeps failing costs at most what one dump may.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use env::ripples_auto::{self as ra, Settings};
use serde_json::{json, Value};

use crate::engine::{self, Ask, Opts};
use crate::seam::State;

/// The scan's period, unless `CAULDRON_RIPPLES_SCAN_MS` says otherwise.
pub const SCAN: Duration = Duration::from_secs(60);
/// The first retry's wait, unless `CAULDRON_RIPPLES_RETRY_MS` says
/// otherwise; each later one is four times the one before.
pub const RETRY: Duration = Duration::from_secs(120);

fn ms_var(name: &str, default: Duration, floor: Duration) -> Duration {
    let v = std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok()).map(Duration::from_millis).unwrap_or(default);
    // Short waits are for tests only.
    if env::egress::process_isolation().is_some() {
        v.max(Duration::from_millis(10))
    } else {
        v.max(floor)
    }
}

/// The scan's period.
pub fn scan_period() -> Duration {
    ms_var("CAULDRON_RIPPLES_SCAN_MS", SCAN, Duration::from_secs(5))
}

/// The wait before failed attempt `n` (1, 2, …) is tried again.
pub fn retry_wait(n: u32) -> Duration {
    let base = ms_var("CAULDRON_RIPPLES_RETRY_MS", RETRY, Duration::from_secs(60));
    base.saturating_mul(4u32.saturating_pow(n.saturating_sub(1).min(8)))
}

/// **The wait after the `n`th park in a row** (1, 2, …): the retry wait of
/// a failed attempt, growing fourfold, never more than 30 first waits (an
/// hour by default): threads parks a call when an account is over its usage
/// threshold, which lasts until its window resets.
pub fn hold_wait(n: u32) -> Duration {
    let base = ms_var("CAULDRON_RIPPLES_RETRY_MS", RETRY, Duration::from_secs(60));
    retry_wait(n).min(base.saturating_mul(30))
}

/// **The threads hold** ([`ra::threads_hold`]): after threads parks an
/// automatic call, no automatic run of any cauldron on the threads backend
/// starts until the park's `retry_after`, so a parked account costs one
/// call per wait, not one per dump. Read from the automatic journal (so it
/// holds across restarts): `(until, parks)`.
pub fn threads_hold(lines: &[Value]) -> (Option<String>, u32) {
    let h = ra::threads_hold(lines);
    (h.until, h.parks)
}

/// Now, UTC, in the journal's form.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn later(d: Duration) -> String {
    (chrono::Utc::now() + chrono::Duration::from_std(d).unwrap_or(chrono::Duration::zero())).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The next midnight, UTC: when the daily cap's window resets.
pub fn next_midnight() -> String {
    let d = chrono::Utc::now().date_naive() + chrono::Days::new(1);
    format!("{}T00:00:00.000Z", d.format("%Y-%m-%d"))
}

/// **Start the worker**: it scans now, then on every word ([`State::wake`])
/// and every [`scan_period`].
pub fn start(state: Arc<State>) -> std::io::Result<()> {
    let (tx, rx): (Sender<()>, Receiver<()>) = channel();
    state.set_wake(tx.clone());
    let _ = tx.send(());
    std::thread::Builder::new().name("ripples-auto".into()).spawn(move || {
        let period = scan_period();
        loop {
            match rx.recv_timeout(period) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            while rx.try_recv().is_ok() {}
            scan(&state);
        }
    })?;
    Ok(())
}

/// Test-only: under isolation, `CAULDRON_RIPPLES_FAULT=<point>` kills the
/// process at that point (`auto-scan`: the dumps were listed, nothing run;
/// `auto-attempt`: the attempt is journaled, the run not started;
/// `auto-ran`: the run ended, its outcome not journaled).
fn fault_point(point: &str) {
    if env::egress::process_isolation().is_some() && std::env::var("CAULDRON_RIPPLES_FAULT").as_deref() == Ok(point) {
        std::process::abort();
    }
}

/// The most pages of [`PAGE`] dumps one listing reads.
pub const PAGES: usize = 40;
/// The dumps one `RDUMPS` answers.
pub const PAGE: usize = 500;

/// The dumps still `received` in `s`'s cauldron above its `since`, or why
/// they cannot be read: page after page (`RDUMPS` answers at most 500,
/// oldest first), so dumps that wait for good (deferred, skipped after
/// `off`) never hide a newer one; at most [`PAGES`] pages.
pub fn received(state: &State, s: &Settings) -> Result<Vec<Value>, String> {
    let mut out: Vec<Value> = Vec::new();
    let mut after = s.since_dump;
    // A smaller page, for tests only.
    let page_size = if env::egress::process_isolation().is_some() { std::env::var("CAULDRON_RIPPLES_PAGE").ok().and_then(|v| v.parse().ok()).unwrap_or(PAGE) } else { PAGE };
    for _ in 0..PAGES {
        let v = state.ctx.mcp.ask("RDUMPS", &json!({"cauldron": s.cauldron, "after": after, "limit": page_size})).map_err(|r| r.to_string())?;
        let page = v["dumps"].as_array().cloned().unwrap_or_default();
        let last = page.iter().map(|d| ra::serial(d["id"].as_str().unwrap_or(""))).max().unwrap_or(after);
        out.extend(page);
        if v["more"] != json!(true) || last <= after {
            break;
        }
        after = last;
    }
    Ok(out)
}

/// **One scan**: passes over every turned-on cauldron, one due dump each,
/// the cauldron served least recently first ([`ra::serve_order`]), until
/// no cauldron has one (or the run slot is taken).
pub fn scan(state: &State) {
    let data_root = state.ctx.env.data_root.clone();
    loop {
        let mut ran = false;
        let mut entries: Vec<ra::Entry> = ra::all(&state.ctx.env).into_iter().filter(ra::Entry::runs).collect();
        let at_start = ra::read(&data_root);
        ra::serve_order(&mut entries, &at_start);
        // Whether the threads hold holds is decided once per pass, so a hold
        // that ends during a pass goes to the cauldron first in the order,
        // not to whichever was being looked at when it ended.
        let held_at_start = ra::threads_hold(&at_start).holding(&now());
        for s in entries.into_iter().map(|x| x.settings) {
            let rs = match received(state, &s) {
                Ok(r) => r,
                Err(why) => {
                    tracing::info!(cauldron = %s.cauldron, %why, "automatic processing could not list the cauldron's dumps; the next scan tries again");
                    continue;
                }
            };
            fault_point("auto-scan");
            let lines = ra::read(&data_root);
            if s.backend != "local" && (held_at_start || ra::threads_hold(&lines).holding(&now())) {
                continue;
            }
            let states = ra::dump_states(&lines, &s);
            let Some(dump) = ra::due(&s, &rs, &states, &now()) else { continue };
            match run_one(state, &s, &dump, &states) {
                Step::Ran => ran = true,
                Step::Busy | Step::JournalDown => return,
                Step::Unreachable => {}
            }
        }
        if !ran {
            return;
        }
    }
}

enum Step {
    Ran,
    /// The run slot is taken (a run by hand): the end of that run wakes the
    /// worker again.
    Busy,
    /// The `mcp` service could not be asked: nothing was tried.
    Unreachable,
    /// The automatic journal could not be written: nothing more is tried
    /// until the next scan (a run whose attempt cannot be journaled never
    /// starts, so nothing loops and nothing is spent).
    JournalDown,
}

/// What an outcome means for the dump: `(class, retry_after)`.
fn classify(v: &Value, failed_before: u32, parks_before: u32) -> (&'static str, Option<String>) {
    let code = v["code"].as_str().unwrap_or("");
    match v["outcome"].as_str().unwrap_or("") {
        "processed" | "already_processed" => ("done", None),
        "deferred" if v["reason"] == "off" => ("skip", None),
        // Not a failure: threads parked the call. The wait is the service's
        // threads hold ([`threads_hold`]), set from this outcome's line.
        "deferred" if v["held_by"] == "threads" => ("deferred", Some(later(hold_wait(parks_before + 1)))),
        // The backend was busy (a run by hand, or another client, held it):
        // not a failure, charged nothing; the next scan tries again.
        "deferred" if v["reason"] == "busy" => ("deferred", Some(later(scan_period()))),
        "deferred" => ("deferred", Some(next_midnight())),
        _ => match code {
            "dump_removed" | "not_found" => ("skip", None),
            // Trying again cannot help: the owner has to look.
            "answer_refused" | "usage" | "no_regions" | "threads_unsupported" | "dump_processed" | "scrub_timeout" => ("parked", None),
            _ if failed_before + 1 >= ra::MAX_ATTEMPTS => ("parked", None),
            _ => ("retry", Some(later(retry_wait(failed_before + 1)))),
        },
    }
}

/// The model of an automatic run of `s`, or the refusal.
fn model_of(state: &State, s: &Settings) -> Result<Option<String>, Value> {
    match s.backend.as_str() {
        "local" => crate::seam::local_model(&state.ctx.env, s.model.clone()).map(Some),
        _ => Ok(s.model.clone()),
    }
}

fn run_one(state: &State, s: &Settings, dump: &str, states: &std::collections::BTreeMap<(String, String), ra::DumpAuto>) -> Step {
    let data_root = state.ctx.env.data_root.clone();
    let failed_before = states.get(&(s.cauldron.clone(), dump.to_string())).map(|d| d.failed_attempts).unwrap_or(0);
    let parks_before = threads_hold(&ra::read(&data_root)).1;
    let backend = if s.backend == "local" { "local" } else { "threads" };
    let attempt = format!("{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0));
    let line = |event: &str, extra: Value| {
        let mut l = json!({"at": now(), "cauldron": s.cauldron, "bound": s.bound_key(), "dump": dump, "event": event, "attempt": attempt, "pid": std::process::id()});
        if let (Some(m), Value::Object(x)) = (l.as_object_mut(), extra) {
            m.extend(x);
        }
        match ra::append(&data_root, &l) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(%e, "cannot write the automatic journal; automatic processing waits for the next scan");
                false
            }
        }
    };
    // The attempt's line, or the run does not start.
    let journal_down = |run: &str| {
        state.finish(run, json!({"outcome": "failed", "code": "journal", "message": "the automatic journal could not be written; nothing was run"}));
        Step::JournalDown
    };
    let (result, run): (Value, String) = match model_of(state, s) {
        Err(refusal) => {
            if !state.take_slot("automatic", &s.cauldron, dump) {
                return Step::Busy;
            }
            if !line("attempt", json!({"n": failed_before + 1, "retry_after_if_cut": later(retry_wait(failed_before + 1))})) {
                return journal_down("");
            }
            let mut v = refusal;
            v["outcome"] = json!("failed");
            v["what_to_do"] = json!(format!("register a local model (`cauldron inference models add`), or turn automatic processing on again with the backend you mean (`cauldron ripples on {} --env {}`)", s.cauldron, state.ctx.env.name));
            (v, String::new())
        }
        Ok(model) => 'arm: {
            let ask = Ask { cauldron: s.cauldron.clone(), dump: dump.to_string(), model, force_budget: None };
            let p = match engine::prepare(&state.ctx, &ask) {
                Ok(p) => p,
                Err(v) if matches!(v["code"].as_str(), Some("mcp_down" | "not_granted" | "unauthorized")) => {
                    tracing::info!(cauldron = %s.cauldron, %dump, code = %v["code"], "automatic processing could not read the dump; the next scan tries again");
                    return Step::Unreachable;
                }
                Err(v) => {
                    if !state.take_slot("automatic", &s.cauldron, dump) {
                        return Step::Busy;
                    }
                    if !line("attempt", json!({"n": failed_before + 1, "retry_after_if_cut": later(retry_wait(failed_before + 1))})) {
                        return journal_down("");
                    }
                    let run = v["run"].as_str().unwrap_or("").to_string();
                    break 'arm (v, run);
                }
            };
            let run = p.run.clone();
            if !state.take_slot(&run, &s.cauldron, dump) {
                return Step::Busy;
            }
            // Nothing runs while a call an earlier run left may still be spending.
            engine::settle(&state.ctx);
            if !line("attempt", json!({"n": failed_before + 1, "run": run, "retry_after_if_cut": later(retry_wait(failed_before + 1))})) {
                return journal_down(&run);
            }
            fault_point("auto-attempt");
            let since_at = s.since_at.clone();
            let cauldron = s.cauldron.clone();
            let e = state.ctx.env.clone();
            // Off, turned off and on again, or no longer the cauldron it was
            // turned on for (removed, renamed, made again): stop here.
            let gate = move || -> Option<String> {
                match ra::effective(&e, &cauldron) {
                    Ok(Some(x)) if x.runs() && x.settings.since_at == since_at => None,
                    Ok(Some(x)) if x.void.is_some() => Some(format!("automatic processing stopped for cauldron `{cauldron}` during the run: its setting no longer belongs to the cauldron of that name")),
                    _ => Some(format!("automatic processing was turned off for cauldron `{cauldron}` (`cauldron ripples off`) during the run")),
                }
            };
            let v = engine::execute_with(&state.ctx, &p, &Opts { automatic: true, gate: Some(&gate), no_scrub_local: !s.scrub_local, keep_answer: false, scrub_budget: None });
            (v, run)
        }
    };
    fault_point("auto-ran");
    let (class, retry_after) = classify(&result, failed_before, parks_before);
    let failed_attempts = failed_before + u32::from(matches!(class, "retry" | "parked") && result["outcome"] == "failed");
    let note_run = if run.is_empty() { "automatic".to_string() } else { run.clone() };
    let base = json!({"code": result["code"], "message": clip(result["message"].as_str().unwrap_or(""), 600)});
    let written = match class {
        "parked" => {
            let mut f = base.clone();
            f["attempts"] = json!(failed_attempts);
            f["what_to_do"] = json!(format!(
                "automatic processing gave up on this dump; run it by hand once the cause is fixed: `cauldron ripples run {dump} --env {} --cauldron {}`",
                state.ctx.env.name, s.cauldron
            ));
            state.ctx.mcp.ask("RFAIL", &json!({"cauldron": s.cauldron, "dump": dump, "run": note_run, "failure": f})).map(|_| ())
        }
        "deferred" => {
            let a = if result["reason"] == "busy" {
                json!({"state": "deferred", "reason": "busy", "message": clip(result["message"].as_str().unwrap_or(""), 600), "retry_after": retry_after})
            } else if result["held_by"] == "threads" {
                json!({"state": "deferred", "reason": "threads_parked", "park": result["park"], "message": clip(result["message"].as_str().unwrap_or(""), 600), "retry_after": retry_after})
            } else {
                json!({"state": "deferred", "reason": "budget", "detail": result["reason"], "message": clip(result["message"].as_str().unwrap_or(""), 600), "retry_after": retry_after})
            };
            state.ctx.mcp.ask("RNOTE", &json!({"cauldron": s.cauldron, "dump": dump, "run": note_run, "auto": a})).map(|_| ())
        }
        "retry" => {
            let mut a = base.clone();
            a["state"] = json!("retrying");
            a["last_code"] = result["code"].clone();
            a["failed_attempts"] = json!(failed_attempts);
            a["of"] = json!(ra::MAX_ATTEMPTS);
            a["retry_after"] = json!(retry_after);
            state.ctx.mcp.ask("RNOTE", &json!({"cauldron": s.cauldron, "dump": dump, "run": note_run, "auto": a})).map(|_| ())
        }
        _ => Ok(()),
    };
    if let Err(r) = written {
        tracing::info!(%dump, code = %r.code, "the dump's own record of automatic processing could not be written; the automatic journal has it");
    }
    let journaled = line(
        "outcome",
        json!({
            "class": class, "run": run, "backend": backend, "outcome": result["outcome"], "code": result["code"], "reason": result["reason"],
            "held_by": result["held_by"], "park": result["park"],
            "message": clip(result["message"].as_str().unwrap_or(""), 600), "what_to_do": result["what_to_do"],
            "records": result["records"], "retry_after": retry_after, "failed_attempts": failed_attempts,
        }),
    );
    tracing::info!(cauldron = %s.cauldron, %dump, %class, outcome = %result["outcome"], "an automatic run ended");
    state.finish(&run, engine::journal_view(&result));
    if !journaled {
        return Step::JournalDown;
    }
    Step::Ran
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
