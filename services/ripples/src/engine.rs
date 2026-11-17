//! **One run of the dump ripple**, step by step:
//!
//! 1. the dump, through the `mcp` service (`RDUMP`): a dump that is removed,
//!    or whose removal is unfinished, is refused; one already `processed` is
//!    answered as it is;
//! 2. the run id, `sha256` of the environment, the cauldron (its name and
//!    when it was made), the dump (its id and when it was received) and the
//!    package (`dump@v1`): the same dump always has the same run, so a run
//!    is never landed twice (`RRUN`, and the `mcp` service refuses a second
//!    landing of a run), and its spend adds up across attempts;
//! 3. the dump's text (a text dump whole; a session dump through its text
//!    view, user and assistant text only) and the cauldron's regions; the
//!    prompt (`crate::package`). No existing record is read or sent;
//! 4. the model call(s), within the caps, counted from the journal
//!    (`crate::journal`) BEFORE each call: per dump, across every run of it,
//!    at most [`Caps::calls`] calls and [`Caps::input_tokens`] input tokens
//!    (a run the owner gave `--force-budget N` may spend 3 more calls and N
//!    more tokens than the dump has spent); per cauldron per UTC day, at most
//!    [`Caps::day_tokens`] input tokens and [`Caps::day_dumps`] dumps. A
//!    call is charged when it BEGINS: its `call` line, written under the
//!    journal's lock together with the check of the caps and before anything
//!    is sent, counts as one call and its estimate (the prompt's characters
//!    divided by three, rounded up); when it ends it is settled to the
//!    larger of the tokens it reported and that estimate. A call a kill or a
//!    stop cut short stays charged. Over a cap the
//!    run stops as `deferred` (reason `budget` or `daily_cap`): nothing is
//!    sent, the dump stays `received`. The first call is Sonnet (or
//!    `--model`); an answer that misses the schema (checked here, with the
//!    schema itself), or that the door refuses, gets ONE retry on Opus with
//!    the refusal added to the prompt; a busy backend is tried again, within
//!    the caps. Every call runs in an empty private directory made for the
//!    run and removed after it;
//! 5. the batch to the `mcp` service (`RPUT`), which lands it and marks the
//!    dump `processed`.
//!
//! Every step is a line in the journal before the next one starts. A call
//! given up (a park, a turn that ended without an answer, the time limit)
//! is cancelled by the backend and the outcome says whether threads
//! confirmed it; a call a kill cut short is cancelled by [`settle`] when the
//! service next starts or runs.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::inference::{Failure, Inference, Request, Usage};
use crate::journal::Journal;
use crate::mcp::{Mcp, Refused};
use crate::package;

/// The caps (module docs, step 4).
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    /// Calls per dump, across every run of it.
    pub calls: u32,
    /// Input tokens per dump, across every run of it.
    pub input_tokens: u64,
    /// Input tokens per cauldron per UTC day.
    pub day_tokens: u64,
    /// Dumps per cauldron per UTC day.
    pub day_dumps: usize,
}

impl Default for Caps {
    /// 3 calls and 200,000 input tokens per dump; 1,500,000 input tokens or
    /// 30 dumps per cauldron per day.
    fn default() -> Caps {
        Caps { calls: 3, input_tokens: 200_000, day_tokens: 1_500_000, day_dumps: 30 }
    }
}

/// The most tokens one answer may have.
pub const MAX_OUTPUT_TOKENS: u32 = crate::threads::MAX_OUTPUT_TOKENS;

/// The model of the first call, unless asked otherwise.
pub const FIRST_MODEL: &str = "sonnet";
/// The model of the one retry.
pub const RETRY_MODEL: &str = "opus";
/// A session dump's text is read in pages of this many characters.
pub const PAGE_CHARS: u64 = 20_000;

/// What a run needs.
pub struct Ctx {
    pub env: env::Environment,
    pub mcp: Mcp,
    pub inference: Box<dyn Inference>,
    pub journal: Journal,
    pub caps: Caps,
    /// Where each run's private working directory is made (the system's
    /// temporary directory).
    pub scratch_root: PathBuf,
    /// The wait before trying a busy backend again.
    pub backoff: Duration,
}

/// What the owner asked.
#[derive(Debug, Clone)]
pub struct Ask {
    pub cauldron: String,
    pub dump: String,
    pub model: Option<String>,
    /// `--force-budget N`, given by the owner at the command line: this run
    /// may spend N more input tokens (and 3 more calls) than the dump has.
    pub force_budget: Option<u64>,
}

/// A run about to start.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub ask: Ask,
    pub run: String,
    pub dump: Value,
    /// A text dump's text, read with the dump.
    pub text: Option<String>,
}

/// A model id is a plain word (`sonnet`, `opus`, `local:<name>`).
pub fn model_ok(m: &str) -> bool {
    !m.is_empty() && m.len() <= 64 && m.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// The run id of a dump (module docs, step 2).
pub fn run_id(env: &str, cauldron: &str, cauldron_created_at: &str, dump: &str, received_at: &str) -> String {
    let mut h = Sha256::new();
    for part in ["ripple-run", env, cauldron, cauldron_created_at, dump, received_at, package::PACKAGE, package::VERSION] {
        h.update(part.as_bytes());
        h.update([0u8]);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A tokens estimate for `chars` characters of prompt: a third, rounded up.
pub fn estimate(chars: usize) -> u64 {
    (chars as u64).div_ceil(3)
}

fn outcome(ask: &Ask, run: &str, what: &str) -> Value {
    json!({"run": run, "cauldron": ask.cauldron, "dump": ask.dump, "outcome": what})
}

fn refused_outcome(ask: &Ask, run: &str, r: &Refused, what_to_do: &str) -> Value {
    let mut v = outcome(ask, run, "failed");
    v["code"] = json!(r.code);
    v["message"] = json!(r.message);
    v["what_to_do"] = json!(what_to_do);
    v
}

/// What to do about a refusal from the `mcp` service, in a sentence.
fn advice(ctx: &Ctx, r: &Refused) -> String {
    let e = &ctx.env.name;
    match r.code.as_str() {
        "mcp_down" => format!("start the mcp service (`cauldron --env {e} start mcp`), then run this again"),
        "not_granted" => format!(
            "the cauldron must be granted to the MCP door with read, write and dump: run `cauldron mcp grant <cauldron> --env {e}` at a terminal (all three are the default), then run this again"
        ),
        "not_found" => "check the dump's id with `cauldron soup dumps`".to_string(),
        "dump_removed" => "nothing to do: a removed dump is never processed".to_string(),
        "unauthorized" => "the mcp service restarted while this run read its key: run the same command again".to_string(),
        _ => "read the message; nothing was written".to_string(),
    }
}

/// **Steps 1-2**: the dump and its run. `Err` is the run's outcome already
/// (the dump was processed, removed, or could not be read).
pub fn prepare(ctx: &Ctx, ask: &Ask) -> Result<Prepared, Value> {
    if let Some(m) = &ask.model {
        if !model_ok(m) {
            let mut v = outcome(ask, "", "failed");
            v["code"] = json!("usage");
            v["message"] = json!(format!("`{m}` is not a model id (sonnet, opus, …)"));
            return Err(v);
        }
    }
    let d = ctx.mcp.ask("RDUMP", &json!({"cauldron": ask.cauldron, "dump": ask.dump})).map_err(|r| refused_outcome(ask, "", &r, &advice(ctx, &r)))?;
    let dump = d["dump"].clone();
    let run = run_id(&ctx.env.name, &ask.cauldron, d["cauldron_created_at"].as_str().unwrap_or(""), &ask.dump, dump["received_at"].as_str().unwrap_or(""));
    match dump["state"].as_str() {
        Some("removed") | Some("removing") => {
            let r = Refused { code: "dump_removed".into(), message: format!("dump `{}` was removed (or its removal is unfinished); it is never processed", ask.dump), detail: Value::Null };
            return Err(refused_outcome(ask, &run, &r, &advice(ctx, &r)));
        }
        Some("processed") => {
            let mut v = outcome(ask, &run, "already_processed");
            v["records"] = dump["processed"]["records"].clone();
            v["processed_by"] = dump["processed"]["run"].clone();
            return Err(v);
        }
        _ => {}
    }
    Ok(Prepared { ask: ask.clone(), run, text: d["text"].as_str().map(str::to_string), dump })
}

/// A session dump's text view, whole, as `[role] text` turns; stops early
/// (answering what it has, and `true`) once it is past `limit` characters.
fn session_text(ctx: &Ctx, p: &Prepared, limit: usize) -> Result<(String, bool), Refused> {
    let (mut offset, mut turn_offset) = (0u64, 0u64);
    let mut out = String::new();
    loop {
        let page = ctx.mcp.ask(
            "RTEXT",
            &json!({"cauldron": p.ask.cauldron, "dump": p.ask.dump, "offset": offset, "turn_offset": turn_offset, "max_chars": PAGE_CHARS}),
        )?;
        for t in page["turns"].as_array().into_iter().flatten() {
            let text = t["text"].as_str().unwrap_or("");
            if t["from_char"].as_u64().unwrap_or(0) > 0 {
                out.push_str(text);
            } else {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(&format!("[{}] {text}", t["role"].as_str().unwrap_or("?")));
            }
        }
        if out.chars().count() > limit {
            return Ok((out, true));
        }
        if page["more"] != json!(true) {
            return Ok((out, false));
        }
        offset = page["next_offset"].as_u64().unwrap_or(offset);
        turn_offset = page["next_turn_offset"].as_u64().unwrap_or(0);
    }
}

/// **A run's private working directory**: an empty directory, owner-only,
/// under the system's temporary directory, made for one run and removed
/// with everything in it when this is dropped.
struct Workdir(PathBuf);

impl Workdir {
    fn make(root: &std::path::Path, run: &str) -> std::io::Result<Workdir> {
        use std::os::unix::fs::DirBuilderExt;
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let p = root.join(format!("cauldron-ripple-{}-{}-{nanos}", &run[..run.len().min(8)], std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&p)?;
        Ok(Workdir(p))
    }

    /// What is in it (a call should leave nothing).
    fn entries(&self) -> usize {
        std::fs::read_dir(&self.0).map(|d| d.count()).unwrap_or(0)
    }
}

impl Drop for Workdir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **Settle calls a run left running**: every `sent` call of the journal
/// whose end was never recorded (the service was killed during the call),
/// and every cancel that was not confirmed, is cancelled through the
/// backend now; each answer is a `cancel` line. Called when the service
/// starts and before each run, never while a run is in progress.
pub fn settle(ctx: &Ctx) -> Vec<Value> {
    let mut out = Vec::new();
    for sent in crate::journal::unsettled(&ctx.journal.lines()) {
        let report = ctx.inference.cancel(&sent);
        let mut extra = report.clone();
        extra["settled"] = json!(true);
        let _ = ctx.journal.write(sent["run"].as_str().unwrap_or(""), sent["cauldron"].as_str().unwrap_or(""), sent["dump"].as_str().unwrap_or(""), "cancel", extra);
        out.push(json!({"run": sent["run"], "dump": sent["dump"], "cancel": report}));
    }
    out
}

/// **Steps 3-5.** Answers the run's outcome (also the journal's `done`).
pub fn execute(ctx: &Ctx, p: &Prepared) -> Value {
    let (ask, run) = (&p.ask, p.run.as_str());
    let j = |event: &str, extra: Value| ctx.journal.write(run, &ask.cauldron, &ask.dump, event, extra);
    let first = ask.model.clone().unwrap_or_else(|| FIRST_MODEL.to_string());
    let mut started = json!({"model": first, "package": format!("{}@{}", package::PACKAGE, package::VERSION), "backend": ctx.inference.name()});
    if let Some(n) = ask.force_budget {
        started["force_budget"] = json!(n);
    }
    if let Err(e) = j("started", started) {
        let mut v = outcome(ask, run, "failed");
        v["code"] = json!("journal");
        v["message"] = json!(format!("cannot write the run journal: {e}"));
        return v;
    }
    let mut usage = Usage::default();
    let mut calls: u32 = 0;
    let mut models: Vec<String> = Vec::new();
    // What the backend reported during a call (`sent`, `cancel`), journaled
    // as it happens; the last cancel goes into the outcome.
    let cancelled: std::sync::Arc<std::sync::Mutex<Option<Value>>> = Default::default();
    let sink: crate::inference::Sink = {
        let (path, ids, cancelled) = (ctx.journal.path().to_path_buf(), (run.to_string(), ask.cauldron.clone(), ask.dump.clone()), cancelled.clone());
        std::sync::Arc::new(move |event: &str, v: Value| {
            let _ = crate::journal::Journal::at(&path).write(&ids.0, &ids.1, &ids.2, event, v.clone());
            if event == "cancel" {
                *cancelled.lock().unwrap_or_else(|p| p.into_inner()) = Some(v);
            }
        })
    };
    let done = |mut v: Value, usage: &Usage, calls: u32, models: &[String]| -> Value {
        v["usage"] = json!(usage);
        v["input_total"] = json!(usage.input_total());
        v["calls"] = json!(calls);
        v["models"] = json!(models);
        v["backend"] = json!(ctx.inference.name());
        if let Some(c) = cancelled.lock().unwrap_or_else(|p| p.into_inner()).clone() {
            if c["confirmed"] != json!(true) {
                let todo = v["what_to_do"].as_str().unwrap_or("").to_string();
                v["what_to_do"] = json!(format!("{todo} THE CALL MAY STILL BE RUNNING: {}", c["how_to_stop"].as_str().unwrap_or("")).trim().to_string());
            }
            v["cancel"] = c;
        }
        let mut extra = v.clone();
        if let Some(o) = extra.as_object_mut() {
            for k in ["run", "cauldron", "dump"] {
                o.remove(k);
            }
        }
        let _ = j("done", extra);
        v
    };
    let fail = |r: &Refused| refused_outcome(ask, run, r, &advice(ctx, r));

    // A run that already landed (cut short before the dump was marked) is
    // finished without calling the model again.
    match ctx.mcp.ask("RRUN", &json!({"cauldron": ask.cauldron, "run": run})) {
        Ok(r) if r["landed"] == json!(true) => {
            return match ctx.mcp.ask("RPUT", &put_args(p, "put", &first, json!([]))) {
                Ok(v) => {
                    let mut o = outcome(ask, run, "processed");
                    o["records"] = v["records"].clone();
                    o["resumed"] = json!(true);
                    done(o, &usage, calls, &models)
                }
                Err(r) => done(fail(&r), &usage, calls, &models),
            };
        }
        Ok(_) => {}
        Err(r) => return done(fail(&r), &usage, calls, &models),
    }

    // The dump's caps for this run, from what it spent before it.
    let (calls_before, tokens_before) = crate::journal::spent(&ctx.journal.lines(), run);
    let (calls_cap, tokens_cap) = match ask.force_budget {
        Some(n) => (calls_before + 3, tokens_before + n),
        None => (ctx.caps.calls, ctx.caps.input_tokens),
    };
    let budget_note = if ask.force_budget.is_some() {
        "this run's forced budget".to_string()
    } else {
        format!("this dump's cap ({} calls and {} input tokens per dump, counted across every run of it)", ctx.caps.calls, ctx.caps.input_tokens)
    };

    // Step 3: the text and the regions.
    let room = tokens_cap.saturating_sub(tokens_before);
    let limit = (room as usize).saturating_mul(3);
    let (text, cut) = match (&p.text, p.dump["kind"].as_str()) {
        (Some(t), _) => {
            let cut = t.chars().count() > limit;
            (t.clone(), cut)
        }
        (None, Some("session")) => match session_text(ctx, p, limit) {
            Ok(x) => x,
            Err(r) => return done(fail(&r), &usage, calls, &models),
        },
        (None, k) => {
            let r = Refused { code: "usage".into(), message: format!("dump `{}` has no text to read (kind {k:?})", ask.dump), detail: Value::Null };
            return done(fail(&r), &usage, calls, &models);
        }
    };
    let read = match ctx.mcp.ask("RREAD", &json!({"cauldron": ask.cauldron})) {
        Ok(v) => v,
        Err(r) => return done(fail(&r), &usage, calls, &models),
    };
    let empty = vec![];
    let regions = read["regions"].as_array().unwrap_or(&empty);
    if regions.is_empty() {
        let r = Refused {
            code: "no_regions".into(),
            message: format!("cauldron `{}` has no region; every record a ripple writes sits in one", ask.cauldron),
            detail: Value::Null,
        };
        let mut v = fail(&r);
        v["what_to_do"] = json!("add regions first (`cauldron soup import` of a regions file, e.g. the plugin's regions.example.json), then run this again");
        return done(v, &usage, calls, &models);
    }
    let kind = p.dump["kind"].as_str().unwrap_or("text");
    let prompt = package::prompt(&ask.dump, kind, regions, &text);
    let est = estimate(prompt.chars().count());
    let _ = j("read", json!({"prompt_chars": prompt.chars().count(), "estimated_input_tokens": est, "text_cut": cut, "regions": regions.len().min(package::MAX_REGIONS), "spent_before": {"calls": calls_before, "charged_input_tokens": tokens_before}}));
    if cut {
        let mut v = outcome(ask, run, "deferred");
        v["reason"] = json!("budget");
        v["message"] = json!(format!("this dump's text is longer than {budget_note} leaves room for ({room} input tokens); nothing was sent to a model"));
        v["what_to_do"] = json!("curate it by hand (`/cauldron:curate-dump`), or give this run more with `--force-budget <tokens>`; the dump stays `received`");
        return done(v, &usage, calls, &models);
    }

    // Step 4: the calls.
    let schema = package::schema();
    let mut model = first.clone();
    let mut retried = false;
    let mut feedback: Option<String> = None;
    loop {
        let this_prompt = match &feedback {
            None => prompt.clone(),
            Some(f) => format!("{prompt}\n\n## Your previous answer was refused\n\n{f}\n\nAnswer again, meeting the schema and the rules above."),
        };
        let this_est = estimate(this_prompt.chars().count());
        let workdir = match Workdir::make(&ctx.scratch_root, run) {
            Ok(w) => w,
            Err(e) => {
                let mut v = outcome(ask, run, "failed");
                v["code"] = json!("workdir");
                v["message"] = json!(format!("cannot make the run's private working directory: {e}"));
                v["what_to_do"] = json!("nothing was sent; check the system's temporary directory, then run this again");
                return done(v, &usage, calls, &models);
            }
        };
        // The caps, counted from the journal, and the call begun (its `call`
        // line, charged its estimate) in one step under the journal's lock:
        // no other run can count the same last slot.
        let call_id = format!("{}-{}-{}", std::process::id(), calls + 1, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        let begun = ctx.journal.begin(run, &ask.cauldron, &ask.dump, |lines| {
            let (spent_calls, spent_tokens) = crate::journal::spent(lines, run);
            if spent_calls >= calls_cap || spent_tokens + this_est > tokens_cap {
                let mut v = outcome(ask, run, "deferred");
                v["reason"] = json!("budget");
                v["message"] = json!(format!(
                    "the next call would pass {budget_note}: {spent_calls} calls and {spent_tokens} input tokens spent on this dump, about {this_est} more needed; nothing more was sent"
                ));
                v["what_to_do"] = json!("the dump stays `received`; curate it by hand (`/cauldron:curate-dump`), or run it again with `--force-budget <tokens>` to let one run spend that many more");
                return Err(v);
            }
            let date = crate::journal::today();
            let today = crate::journal::day(lines, &ask.cauldron, &date);
            let new_dump = !today.dumps.contains(&ask.dump);
            if today.tokens + this_est > ctx.caps.day_tokens || (new_dump && today.dumps.len() >= ctx.caps.day_dumps) {
                let mut v = outcome(ask, run, "deferred");
                v["reason"] = json!("daily_cap");
                v["message"] = json!(format!(
                    "the next call would pass cauldron `{}`'s daily cap ({} input tokens or {} dumps per UTC day; {} tokens and {} dumps spent on {date}); nothing more was sent",
                    ask.cauldron,
                    ctx.caps.day_tokens,
                    ctx.caps.day_dumps,
                    today.tokens,
                    today.dumps.len()
                ));
                v["what_to_do"] = json!("the dump stays `received`; run it again after midnight UTC, or curate it by hand (`/cauldron:curate-dump`)");
                return Err(v);
            }
            Ok(json!({"n": calls + 1, "call_id": call_id, "model": model, "estimated_input_tokens": this_est}))
        });
        match begun {
            Ok(Ok(())) => {}
            Ok(Err(v)) => return done(v, &usage, calls, &models),
            Err(e) => {
                let mut v = outcome(ask, run, "failed");
                v["code"] = json!("journal");
                v["message"] = json!(format!("cannot write the run journal: {e}"));
                v["what_to_do"] = json!("nothing was sent; check the service's data directory, then run this again");
                return done(v, &usage, calls, &models);
            }
        }
        calls += 1;
        models.push(model.clone());
        let req = Request {
            model: model.clone(),
            prompt: this_prompt,
            schema: schema.clone(),
            cwd: workdir.0.clone(),
            name: run.to_string(),
            max_output_tokens: MAX_OUTPUT_TOKENS,
            sink: Some(sink.clone()),
        };
        let answer = ctx.inference.call(&req);
        let left = workdir.entries();
        drop(workdir);
        let _ = j("scrubbed", json!({"entries_left_by_the_call": left}));
        let (call_usage, answered_by) = match &answer {
            Ok(r) => (r.usage.clone(), r.model.clone()),
            Err(f) => (f.usage(), model.clone()),
        };
        usage.add(&call_usage);
        // Nothing was sent when the backend refused the restricted request.
        let charged = if matches!(answer, Err(Failure::Unsupported(_)) | Err(Failure::Unavailable(_))) { call_usage.input_total() } else { call_usage.input_total().max(this_est) };
        let mut called = json!({"n": calls, "call_id": call_id, "model": model, "answered_by": answered_by, "usage": call_usage, "input_total": call_usage.input_total(), "charged": charged});
        if let Err(f) = &answer {
            called["failure"] = json!(f.code());
        }
        let _ = j("called", called);
        let refusal: String = match answer {
            Ok(reply) => match package::to_records(&reply.output) {
                Err(why) => why,
                Ok(records) => match ctx.mcp.ask("RPUT", &put_args(p, "put", &model, json!(records))) {
                    Ok(v) => {
                        fault_point("after-put");
                        let mut o = outcome(ask, run, "processed");
                        o["records"] = v["records"].clone();
                        if let Some(n) = reply.output.get("notes") {
                            o["notes"] = n.clone();
                        }
                        return done(o, &usage, calls, &models);
                    }
                    Err(r) if matches!(r.code.as_str(), "batch_refused" | "records_refused") => {
                        let _ = j("refused", json!({"code": r.code}));
                        format!("{}: {}", r.message, clip(&r.detail.to_string(), 4000))
                    }
                    Err(r) => return done(fail(&r), &usage, calls, &models),
                },
            },
            Err(Failure::Busy(m)) => {
                std::thread::sleep(ctx.backoff);
                let _ = m;
                continue;
            }
            Err(Failure::Parked(m)) => {
                let mut v = outcome(ask, run, "deferred");
                v["reason"] = json!("budget");
                v["message"] = json!(m);
                v["what_to_do"] = json!("threads held the call under its budget and the call was cancelled; the dump stays `received`. Run this again once the budget allows (`threads` decides).");
                return done(v, &usage, calls, &models);
            }
            Err(Failure::SchemaUnmet { message, .. }) => message,
            Err(Failure::Unavailable(m)) => {
                let mut v = outcome(ask, run, "failed");
                v["code"] = json!("inference_unavailable");
                v["message"] = json!(m);
                v["what_to_do"] = json!(format!("start threads in this environment (`cauldron --env {} start threads`) and check that it is logged in, then run this again", ctx.env.name));
                return done(v, &usage, calls, &models);
            }
            Err(Failure::Unsupported(m)) => {
                let mut v = outcome(ask, run, "failed");
                v["code"] = json!("threads_unsupported");
                v["message"] = json!(m);
                v["what_to_do"] = json!("run a threads service from the same cauldron release as ripples (0.1.10 or later), then run this again; nothing was sent");
                return done(v, &usage, calls, &models);
            }
            Err(Failure::Failed { message, .. }) => {
                let mut v = outcome(ask, run, "failed");
                v["code"] = json!("inference_failed");
                v["message"] = json!(message);
                v["what_to_do"] = json!("nothing was written and the dump stays `received`; run this again, or curate it by hand (`/cauldron:curate-dump`)");
                return done(v, &usage, calls, &models);
            }
        };
        // The one retry, on Opus, with the refusal.
        if retried {
            let mut v = outcome(ask, run, "failed");
            v["code"] = json!("answer_refused");
            v["message"] = json!(format!("the answer was refused again after the retry: {}", clip(&refusal, 1000)));
            v["what_to_do"] = json!("nothing was written and the dump stays `received`; curate it by hand (`/cauldron:curate-dump`)");
            return done(v, &usage, calls, &models);
        }
        retried = true;
        model = RETRY_MODEL.to_string();
        feedback = Some(refusal);
    }
}

/// Test-only: under isolation, `CAULDRON_RIPPLES_FAULT=<point>` kills the
/// process at that point (`after-put`: the batch landed and the dump was
/// marked, and the journal has no `done` yet).
fn fault_point(point: &str) {
    if env::egress::process_isolation().is_some() && std::env::var("CAULDRON_RIPPLES_FAULT").as_deref() == Ok(point) {
        std::process::abort();
    }
}

/// At most `n` characters of `s`.
fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `RPUT`'s arguments.
fn put_args(p: &Prepared, step: &str, model: &str, records: Value) -> Value {
    json!({
        "cauldron": p.ask.cauldron, "dump": p.ask.dump, "run": p.run, "step": step,
        "package": package::PACKAGE, "version": package::VERSION, "model": model, "records": records,
    })
}

/// [`prepare`] then [`execute`].
pub fn run(ctx: &Ctx, ask: &Ask) -> Value {
    match prepare(ctx, ask) {
        Ok(p) => execute(ctx, &p),
        Err(v) => v,
    }
}
