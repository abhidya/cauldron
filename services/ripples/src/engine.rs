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
//!    sent, the dump stays `received`. The first call is Opus (or
//!    `--model`); an answer that misses the schema (checked here, with the
//!    schema itself), or that the door refuses, gets ONE retry on Opus with
//!    the refusal added to the prompt; a busy backend is tried again, within
//!    the caps. Every call runs in an empty private directory made for the
//!    run and removed after it. A local run (`--backend local`, model
//!    `local:<name>`) makes both calls on its local model, so nothing leaves
//!    the machine; its tokens cost no money, and the same caps still hold
//!    (the per-dump call cap is what bounds a runaway loop);
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

/// The most calls one run makes to a busy backend by hand (an automatic
/// run makes one, and waits for the next scan). A busy answer is charged
/// nothing and is not a call against the caps.
pub const BUSY_TRIES: u32 = 3;

/// The model of the first call, unless asked otherwise. Opus until further
/// notice: a ripple does its task logic (issues, contradictions, work items)
/// while it thinks, and that reasoning is not yet split out to a stronger
/// model over cheaper graph writers.
pub const FIRST_MODEL: &str = "opus";
/// The model of the one retry (Opus, the same as the first call by default).
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
    /// A region lock's time to live, renewed while a model call is open
    /// (`crate::regions::ttl_secs`).
    pub lock_ttl_secs: u64,
    /// How long ripples may hold one region of a cauldron within any hour
    /// (`crate::regions::hold_budget_secs`); a run that would hold a region
    /// past it is deferred (`hold_budget`) before it takes any lock.
    pub hold_budget_secs: u64,
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
    /// The cauldron's identity ([`cauldron_identity`]): what its daily cap
    /// is counted by.
    pub identity: String,
    /// Every identity its calls were journaled under ([`cauldron_identities`]).
    pub identities: Vec<String>,
}

/// **A cauldron's identity**, as its daily cap counts it: the binding its
/// automatic setting and MCP grant are bound to (the environment and the
/// cauldron's id; for a cauldron with no id yet, its `created_at` and its
/// directory's inode; `env::ripples_auto::Binding`), so a cauldron removed
/// and made again under the same name starts its day clean, and one renamed
/// or moved keeps what it spent. A name that is no cauldron there now is its
/// name.
pub fn cauldron_identity(e: &env::Environment, cauldron: &str) -> String {
    cauldron_identities(e, cauldron).remove(0)
}

/// [`cauldron_identity`], then the identity the cauldron's calls carried
/// before it was given its id (its `created_at` and inode), so the calls of
/// the day it was given one still count.
pub fn cauldron_identities(e: &env::Environment, cauldron: &str) -> Vec<String> {
    match e.find_cauldron(cauldron) {
        Ok(Some(c)) => env::ripples_auto::spend_identities(e, &c),
        _ => vec![format!("name:{cauldron}")],
    }
}

/// Is `model` a local model id (`local:<name>`, run by the `inference`
/// service on this machine)?
pub fn is_local(model: &str) -> bool {
    model.starts_with(crate::local::PREFIX)
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
    let code = if code_ok(&r.code) { r.code.as_str() } else { "refused" };
    v["code"] = json!(code);
    v["message"] = json!(fixed_message(code));
    v["detail"] = json!(r.message);
    v["what_to_do"] = json!(what_to_do);
    v
}

/// A code is a short word of the fixed vocabulary's shape.
fn code_ok(c: &str) -> bool {
    !c.is_empty() && c.len() <= 40 && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// **The message an outcome carries for a code**: a fixed sentence, never
/// a backend's or the `mcp` service's own words, and never the model's
/// answer or the dump's text, so what is journaled or written into a
/// dump's record (`meta.failure`, `meta.auto`) holds none of either. The
/// words a backend or the soup gave are in the outcome's `detail`, which
/// only the caller of a run by hand is shown and which is never written
/// (`--debug-keep-answer` keeps it, in a private file, for a run by hand).
pub fn fixed_message(code: &str) -> String {
    match code {
        "answer_refused" => "the model's answer was refused again after the retry (it did not meet the package's schema, or the soup refused its records); the answer's text is not kept".into(),
        "inference_failed" => "the model call failed".into(),
        "inference_unavailable" => "the backend could not be reached; nothing was sent".into(),
        "threads_unsupported" => "the threads service does not take this call (an older release); nothing was sent".into(),
        "threads_budget" | "threads_parked" => park_message("parked", false),
        "scrub_timeout" => "the secret scrubber did not finish within its time budget, so nothing was sent".into(),
        "busy" => "the backend was busy (another request held it, or it was not ready yet) and refused the call before it ran; nothing reached a model and nothing was charged; the dump waits to be tried again".into(),
        "mcp_down" => "the mcp service could not be reached".into(),
        "not_granted" => "the cauldron is not granted to the MCP door with read, write and dump".into(),
        "not_found" => "no such dump in this cauldron".into(),
        "dump_removed" => "the dump was removed (or its removal is unfinished); it is never processed".into(),
        "unauthorized" => "the mcp service did not take the run key".into(),
        "batch_refused" | "records_refused" => "the soup refused the answer's records".into(),
        "region_locked" => "another writer holds a region this run writes into; nothing was written, a call this cost is not counted against the dump's own cap, and the dump waits and is tried again once it is let go".into(),
        "region_changed" => "the boundary of a region the run planned against changed while it was planning, again after it planned once more; nothing was written, the calls this cost are not counted against the dump's own cap, and the dump waits to be tried again".into(),
        "hold_budget" => "the regions this dump's text points at have been held by ripples for their whole hourly budget; nothing was sent, no lock was taken, and the dump waits to be tried again once the budget frees up".into(),
        "mcp_too_old" => "the running mcp service is older than this ripples service and does not know the verbs it uses; nothing was written".into(),
        other => format!("the mcp service refused the request ({other})"),
    }
}

/// **The message of a call threads parked**, by its cause (a word of
/// [`crate::inference::PARK_CAUSES`]): fixed sentences, never threads' own
/// words (which name accounts).
pub fn park_message(cause: &str, held: bool) -> String {
    let why = match cause {
        "threshold" => "every account it may use is over its usage threshold",
        "usage_limit" => "an account reached its usage limit",
        "credit_balance" => "the account's credit balance is spent",
        "login_expired" => "the account's login has expired",
        "budget" => "one of its budgets tripped",
        _ => "it holds calls for now",
    };
    let sent = if held { "nothing reached a model and nothing was charged" } else { "the call was cut short and what it spent is charged" };
    format!("threads parked the call: {why}; the call was withdrawn, {sent}; it is not a failure and the dump waits to be tried again")
}

/// **What of an outcome is written down** (the run journal's `done` line,
/// the automatic journal, a dump's record): everything but the words a
/// backend or the soup gave (`detail`) and the model's `notes`.
pub fn journal_view(v: &Value) -> Value {
    let mut x = v.clone();
    if let Some(o) = x.as_object_mut() {
        for k in ["detail", "notes"] {
            o.remove(k);
        }
    }
    x
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
        "mcp_too_old" => format!("upgrade the mcp service to this release (`scripts/install --replace-running` from your checkout, or `cauldron --env {e} build mcp --env-copy` and then `cauldron --env {e} pin mcp <the label that build printed>`), then run this again"),
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
    let identities = cauldron_identities(&ctx.env, &ask.cauldron);
    let identity = identities[0].clone();
    Ok(Prepared { ask: ask.clone(), run, text: d["text"].as_str().map(str::to_string), dump, identity, identities })
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
pub(crate) struct Workdir(pub(crate) PathBuf);

impl Workdir {
    pub(crate) fn make(root: &std::path::Path, run: &str) -> std::io::Result<Workdir> {
        use std::os::unix::fs::DirBuilderExt;
        // The clock alone does not tell two calls apart (macOS gives it in
        // microseconds, so concurrent runs of one dump in one process could
        // pick one name): a count of the directories this process made does.
        static MADE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let p = root.join(format!("cauldron-ripple-{}-{}-{nanos}-{n}", &run[..run.len().min(8)], std::process::id()));
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
/// backend now; each answer is a `cancel` line. Every region lock a run
/// took and never let go is let go first (an `unlocked` line). Called when the service
/// starts and before each run, never while a run is in progress.
pub fn settle(ctx: &Ctx) -> Vec<Value> {
    let mut out = Vec::new();
    // The region locks a run took and never let go (the service was killed
    // while it held them): let go now. One the `mcp` service cannot be
    // asked about is tried again next time, and expires meanwhile.
    for (run, cauldron, dump) in crate::journal::held_locks(&ctx.journal.lines()) {
        if let Ok(u) = ctx.mcp.ask("RUNLOCK", &json!({"cauldron": cauldron, "run": run})) {
            let _ = ctx.journal.write(&run, &cauldron, &dump, "unlocked", json!({"released": u["released"], "settled": true}));
            out.push(json!({"run": run, "dump": dump, "unlocked": u["released"]}));
        }
    }
    // The runs' journal, then the evaluations' ledger.
    for j in [Journal::at(ctx.journal.path()), ctx.journal.eval_ledger()] {
        for sent in crate::journal::unsettled(&j.lines()) {
            let report = ctx.inference.cancel(&sent);
            let mut extra = report.clone();
            extra["settled"] = json!(true);
            let _ = j.write(sent["run"].as_str().unwrap_or(""), sent["cauldron"].as_str().unwrap_or(""), sent["dump"].as_str().unwrap_or(""), "cancel", extra);
            out.push(json!({"run": sent["run"], "dump": sent["dump"], "cancel": report}));
        }
    }
    out
}

/// How a run is held beyond its [`Ask`]: what an automatic run adds.
#[derive(Default, Clone, Copy)]
pub struct Opts<'a> {
    /// Started by automatic processing, not by the owner.
    pub automatic: bool,
    /// Asked before every model call (under the journal's lock, with the
    /// caps) and before the batch is put: `Some(why)` stops the run there as
    /// `deferred` with reason `off`, and nothing more is sent or written
    /// (`cauldron ripples off` takes effect before the next call).
    pub gate: Option<&'a (dyn Fn() -> Option<String> + Sync)>,
    /// Send a local model the text unscrubbed (`cauldron ripples on
    /// --no-scrub-local`). What goes through threads is always scrubbed, and
    /// the answer always is.
    pub no_scrub_local: bool,
    /// `cauldron ripples run --debug-keep-answer` (by hand only, never an
    /// automatic run): the model's answers and the refusals, which are
    /// otherwise written nowhere, kept in one private file
    /// (`<data root>/ripples/debug/`, 0600), its path in the outcome.
    pub keep_answer: bool,
    /// The secret scrubber's time for this run, instead of
    /// [`crate::scrub::budget_for`] the text's size (tests).
    pub scrub_budget: Option<Duration>,
}

/// The run's outcome when the scrubber ran out of time: failed
/// `scrub_timeout`, nothing sent (or, for the answer, nothing written).
fn scrub_timeout(ask: &Ask, run: &str, x: &crate::scrub::Exhausted, what: &str) -> Value {
    let mut v = outcome(ask, run, "failed");
    v["code"] = json!("scrub_timeout");
    v["message"] = json!(fixed_message("scrub_timeout"));
    v["scrub"] = json!({"bytes": x.bytes, "budget_ms": x.budget_ms, "of": what});
    v["what_to_do"] = json!(if what == "sent" {
        "nothing was sent to a model and the dump stays `received`; curate it by hand (`/cauldron:curate-dump`)"
    } else {
        "the answer was not written and the dump stays `received`; curate it by hand (`/cauldron:curate-dump`)"
    });
    v
}

/// Write what `--debug-keep-answer` kept; its path.
fn keep_answer(ctx: &Ctx, opts: &Opts<'_>, run: &str, kept: &std::sync::Mutex<Vec<Value>>) -> Option<String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if !opts.keep_answer || opts.automatic {
        return None;
    }
    let items = std::mem::take(&mut *kept.lock().unwrap_or_else(|p| p.into_inner()));
    let dir = ctx.env.data_root.join(crate::SERVICE).join("debug");
    env::private::make_dir(&ctx.env.data_root.join(crate::SERVICE)).ok()?;
    env::private::make_dir(&dir).ok()?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let path = dir.join(format!("answer-{}-{nanos}.json", &run[..run.len().min(12)]));
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(env::private::FILE_MODE).open(&path).ok()?;
    let doc = json!({"run": run, "note": "kept by `--debug-keep-answer`: the model's answers and the refusals, unscrubbed; delete this file when done", "items": items});
    f.write_all(serde_json::to_string_pretty(&doc).ok()?.as_bytes()).ok()?;
    f.sync_all().ok()?;
    Some(path.display().to_string())
}

fn gated(ask: &Ask, run: &str, why: String) -> Value {
    let mut v = outcome(ask, run, "deferred");
    v["reason"] = json!("off");
    v["message"] = json!(why);
    v["what_to_do"] = json!("nothing more was sent or written and the dump stays `received`; turn automatic processing on again, or run it by hand (`cauldron ripples run`)");
    v
}

/// **Steps 3-5.** Answers the run's outcome (also the journal's `done`).
pub fn execute(ctx: &Ctx, p: &Prepared) -> Value {
    execute_with(ctx, p, &Opts::default())
}

/// [`execute`], held by `opts`.
pub fn execute_with(ctx: &Ctx, p: &Prepared, opts: &Opts<'_>) -> Value {
    let (ask, run) = (&p.ask, p.run.as_str());
    let j = |event: &str, extra: Value| ctx.journal.write(run, &ask.cauldron, &ask.dump, event, extra);
    let first = ask.model.clone().unwrap_or_else(|| FIRST_MODEL.to_string());
    let backend = ctx.inference.name_for(&first);
    let mut started = json!({"model": first, "package": format!("{}@{}", package::PACKAGE, package::VERSION), "backend": backend});
    if let Some(n) = ask.force_budget {
        started["force_budget"] = json!(n);
    }
    if opts.automatic {
        started["automatic"] = json!(true);
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
    // `--debug-keep-answer` (a run by hand only): each answer and refusal,
    // kept in a private file when the run ends; nowhere otherwise.
    let kept: std::sync::Mutex<Vec<Value>> = Default::default();
    let keep = |what: &str, v: Value| {
        if opts.keep_answer {
            kept.lock().unwrap_or_else(|p| p.into_inner()).push(json!({"what": what, "value": v}));
        }
    };
    // The run's region lock (`crate::regions`): held from before the first
    // model call until the records land (the `mcp` service lets it go in
    // the act that lands them) or the run ends any other way, when it is let
    // go here, before the outcome is written.
    let lock_held = std::cell::Cell::new(false);
    let done = |mut v: Value, usage: &Usage, calls: u32, models: &[String]| -> Value {
        if lock_held.get() {
            match ctx.mcp.ask("RUNLOCK", &json!({"cauldron": ask.cauldron, "run": run})) {
                Ok(u) => {
                    let _ = j("unlocked", json!({"released": u["released"]}));
                    lock_held.set(false);
                }
                // Left for the next start or run to let go (`settle`); it
                // expires meanwhile.
                Err(r) => {
                    let _ = j("unlock_failed", json!({"code": r.code}));
                }
            }
        }
        v["usage"] = json!(usage);
        v["input_total"] = json!(usage.input_total());
        v["calls"] = json!(calls);
        v["models"] = json!(models);
        v["backend"] = json!(backend);
        if let Some(path) = keep_answer(ctx, opts, run, &kept) {
            v["debug_file"] = json!(path);
        }
        if let Some(c) = cancelled.lock().unwrap_or_else(|p| p.into_inner()).clone() {
            if c["confirmed"] != json!(true) {
                let todo = v["what_to_do"].as_str().unwrap_or("").to_string();
                v["what_to_do"] = json!(format!("{todo} THE CALL MAY STILL BE RUNNING: {}", c["how_to_stop"].as_str().unwrap_or("")).trim().to_string());
            }
            v["cancel"] = c;
        }
        let mut extra = journal_view(&v);
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
    // An `mcp` service from before region locks answers without revisions:
    // it would take no lock and check no revision at the put, so nothing is
    // written through it (the service is upgraded with the command line).
    if !read.get("revs").is_some_and(Value::is_object) {
        let r = Refused {
            code: "mcp_too_old".into(),
            message: "the running mcp service answered the regions without their revisions: it is from before region locks, and would neither hold a region nor check one at the put; nothing was sent or written".into(),
            detail: Value::Null,
        };
        return done(fail(&r), &usage, calls, &models);
    }
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
    // Too long for the dump's cap: deferred before the text is scrubbed, so
    // a dump that is not sent costs no scrubbing either.
    if cut {
        let _ = j("read", json!({"text_cut": true, "regions": regions.len().min(package::MAX_REGIONS), "spent_before": {"calls": calls_before, "charged_input_tokens": tokens_before}}));
        let mut v = outcome(ask, run, "deferred");
        v["reason"] = json!("budget");
        v["message"] = json!(format!("this dump's text is longer than {budget_note} leaves room for ({room} input tokens); nothing was sent to a model"));
        v["what_to_do"] = json!("curate it by hand (`/cauldron:curate-dump`), or give this run more with `--force-budget <tokens>`; the dump stays `received`");
        return done(v, &usage, calls, &models);
    }
    // The regions this run expects to write into, held from now until its
    // records land or it gives up (`crate::regions`). The prompt is built
    // from the regions as they were read at the moment the lock was taken,
    // and the put carries their revisions.
    // A run already at a cap holds nothing: deferred before any lock (the
    // check that charges is the one under the journal's lock, below).
    if let Some(v) = over_caps(ctx, p, &ctx.journal.lines(), calls_cap, tokens_cap, &budget_note, estimate(text.chars().count())) {
        return done(v, &usage, calls, &models);
    }
    let declared = crate::regions::declare(&text, regions);
    // A region ripples already held for its hourly budget is not held
    // again: the run is deferred before it takes any lock, so no text can
    // keep a region busy hour after hour.
    if let Some(v) = over_hold_budget(ctx, p, &declared) {
        let _ = j("hold_budget", json!({"regions": v["regions"]}));
        return done(v, &usage, calls, &models);
    }
    let (regions, mut revs) = if declared.is_empty() {
        (regions.clone(), read.get("revs").filter(|v| v.is_object()).cloned())
    } else {
        match take_lock(ctx, p, &declared) {
            Ok(v) => {
                lock_held.set(true);
                let _ = j("locked", json!({"regions": v["lock"]["regions"], "above": v["lock"]["above"], "until": v["lock"]["until"]}));
                (v["regions"].as_array().cloned().unwrap_or_default(), Some(v["revs"].clone()))
            }
            Err(r) if r.code == "region_locked" => {
                let _ = j("lock_refused", json!({"regions": declared}));
                return done(region_deferred(ask, run, "region_locked", &r), &usage, calls, &models);
            }
            Err(r) => return done(fail(&r), &usage, calls, &models),
        }
    };
    fault_point("after-lock");
    // The secret scrubber (`crate::scrub`) on what is sent: always through
    // threads, and to a local model unless the owner said otherwise. The
    // stored dump is never changed: this is the run's copy. Held to a time
    // budget in proportion to the text; out of time, nothing is sent (it
    // fails closed, whatever the backend).
    let scrub_sent = !is_local(&first) || !opts.no_scrub_local;
    let scrub_regions = |regions: &[Value]| -> Result<Vec<Value>, crate::scrub::Exhausted> {
        let mut regions = regions.to_vec();
        if !scrub_sent {
            return Ok(regions);
        }
        let bytes = regions.iter().map(crate::scrub::value_bytes).sum::<usize>();
        let deadline = std::time::Instant::now() + opts.scrub_budget.unwrap_or_else(|| crate::scrub::budget_for(bytes));
        let mut counts = Default::default();
        for r in regions.iter_mut() {
            for k in ["handle", "boundary"] {
                crate::scrub::add(&mut counts, &crate::scrub::scrub_value_within(&mut r[k], deadline)?);
            }
        }
        if !counts.is_empty() {
            let _ = j("redacted", json!({"in": "sent", "of": "regions", "counts": counts}));
        }
        Ok(regions)
    };
    let text = if scrub_sent {
        let deadline = std::time::Instant::now() + opts.scrub_budget.unwrap_or_else(|| crate::scrub::budget_for(text.len()));
        match crate::scrub::scrub_within(&text, deadline) {
            Ok((t, counts)) => {
                if !counts.is_empty() {
                    let _ = j("redacted", json!({"in": "sent", "counts": counts}));
                }
                t
            }
            Err(x) => return done(scrub_timeout(ask, run, &x, "sent"), &usage, calls, &models),
        }
    } else {
        text
    };
    let regions = match scrub_regions(&regions) {
        Ok(r) => r,
        Err(x) => return done(scrub_timeout(ask, run, &x, "sent"), &usage, calls, &models),
    };
    let mut prompt = package::prompt(&ask.dump, kind, &regions, &text);
    // The regions as the prompt shows them, and, after a re-plan, what
    // changed in them while the model answered (said in the next prompt).
    let mut shown = regions.clone();
    let mut changes: Option<String> = None;
    let est = estimate(prompt.chars().count());
    let _ = j("read", json!({"prompt_chars": prompt.chars().count(), "estimated_input_tokens": est, "text_cut": cut, "regions": regions.len().min(package::MAX_REGIONS), "declared": declared, "spent_before": {"calls": calls_before, "charged_input_tokens": tokens_before}}));

    // Step 4: the calls.
    let schema = package::schema();
    let mut model = first.clone();
    let mut retried = false;
    let mut feedback: Option<String> = None;
    let mut refused_by: &str;
    let mut busy = 0u32;
    // A put refused because a region changed after the run read it is
    // planned again ONCE, from a fresh read; a second time, the dump waits.
    let mut replanned = false;
    'calls: loop {
        let base = match &changes {
            None => prompt.clone(),
            Some(c) => format!("{prompt}\n\n{c}"),
        };
        let this_prompt = match &feedback {
            None => base,
            Some(f) => format!("{base}\n\n## Your previous answer was refused\n\n{f}\n\nAnswer again, meeting the schema and the rules above."),
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
            if let Some(why) = opts.gate.and_then(|g| g()) {
                return Err(gated(ask, run, why));
            }
            if let Some(v) = over_caps(ctx, p, lines, calls_cap, tokens_cap, &budget_note, this_est) {
                return Err(v);
            }
            Ok(json!({"n": calls + 1, "call_id": call_id, "model": model, "estimated_input_tokens": this_est, "cauldron_id": p.identity}))
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
        let answer = with_renewal(&ctx.mcp, &ask.cauldron, run, lock_held.get().then_some(ctx.lock_ttl_secs), &j, || ctx.inference.call(&req));
        let left = workdir.entries();
        drop(workdir);
        let _ = j("scrubbed", json!({"entries_left_by_the_call": left}));
        let (call_usage, answered_by) = match &answer {
            Ok(r) => (r.usage.clone(), r.model.clone()),
            Err(f) => (f.usage(), model.clone()),
        };
        usage.add(&call_usage);
        // Nothing was sent when the backend refused the restricted request,
        // and nothing reached a model when threads held the call before it
        // ran (a park with no tokens reported: an account over its usage
        // threshold, at admission). Such a call is `held`: charged nothing,
        // and not counted as a call against any cap.
        // A busy backend (another request holds it, or it is not ready)
        // refused the call before it ran: held too.
        let busy_now = matches!(answer, Err(Failure::Busy(_)));
        let held = (busy_now || matches!(answer, Err(Failure::Parked { .. }))) && call_usage.input_total() == 0 && call_usage.output_tokens == 0;
        let charged = if held || matches!(answer, Err(Failure::Unsupported(_)) | Err(Failure::Unavailable(_))) { call_usage.input_total() } else { call_usage.input_total().max(this_est) };
        let mut called = json!({"n": calls, "call_id": call_id, "model": model, "answered_by": answered_by, "usage": call_usage, "input_total": call_usage.input_total(), "charged": charged});
        if let Err(f) = &answer {
            called["failure"] = json!(f.code());
        }
        if held {
            called["held"] = json!(true);
        }
        if busy_now {
            called["busy"] = json!(true);
        }
        let _ = j("called", called);
        match &answer {
            Ok(reply) => keep("answer", reply.output.clone()),
            Err(f) => keep("failure", json!({"code": f.code(), "message": f.message()})),
        }
        let refusal: String = match answer {
            Ok(reply) => match package::to_records(&reply.output) {
                Err(why) => {
                    refused_by = "schema";
                    why
                }
                Ok(_) if opts.gate.and_then(|g| g()).is_some() => {
                    let why = opts.gate.and_then(|g| g()).unwrap_or_default();
                    return done(gated(ask, run, why), &usage, calls, &models);
                }
                Ok(records) => match scrub_answer(&j, records, opts.scrub_budget) {
                    Err(x) => return done(scrub_timeout(ask, run, &x, "answer"), &usage, calls, &models),
                    // The put, sent again (with no new call) when nothing
                    // the model was shown changed.
                    Ok(records) => loop {
                        match ctx.mcp.ask("RPUT", &with_revs(put_args(p, "put", &model, records.clone()), &revs)) {
                            Ok(v) => {
                                if lock_held.get() {
                                    lock_held.set(false);
                                    let _ = j("unlocked", json!({"released": v["locks_released"], "by": "put"}));
                                }
                                fault_point("after-put");
                                let mut o = outcome(ask, run, "processed");
                                o["records"] = v["records"].clone();
                                if let Some(n) = reply.output.get("notes") {
                                    o["notes"] = n.clone();
                                }
                                return done(o, &usage, calls, &models);
                            }
                            Err(r) if r.code == "region_changed" && !replanned => {
                                replanned = true;
                                // Read the regions again (under the lock again,
                                // when the run holds one).
                                let fresh = if declared.is_empty() { ctx.mcp.ask("RREAD", &json!({"cauldron": ask.cauldron})) } else { take_lock(ctx, p, &declared) };
                                let v = match fresh {
                                    Ok(v) => v,
                                    Err(r) if r.code == "region_locked" => {
                                        let _ = j("voided", json!({"call_id": call_id, "n": calls, "why": "region_locked"}));
                                        return done(region_deferred(ask, run, "region_locked", &r), &usage, calls, &models);
                                    }
                                    Err(r) => return done(fail(&r), &usage, calls, &models),
                                };
                                if !declared.is_empty() {
                                    lock_held.set(true);
                                    let _ = j("locked", json!({"regions": v["lock"]["regions"], "above": v["lock"]["above"], "until": v["lock"]["until"], "again": true}));
                                }
                                revs = v.get("revs").filter(|x| x.is_object()).cloned();
                                let again = match scrub_regions(&v["regions"].as_array().cloned().unwrap_or_default()) {
                                    Ok(r) => r,
                                    Err(x) => return done(scrub_timeout(ask, run, &x, "sent"), &usage, calls, &models),
                                };
                                let fresh_prompt = package::prompt(&ask.dump, kind, &again, &text);
                                if fresh_prompt == prompt {
                                    // Nothing the model was shown changed: the
                                    // same records, with the fresh revisions,
                                    // and no call.
                                    let _ = j("replan", json!({"changed": r.detail["changed"], "prompt_changed": false, "call": false}));
                                    continue;
                                }
                                let _ = j("replan", json!({"changed": r.detail["changed"], "prompt_changed": true, "call": true}));
                                let _ = j("voided", json!({"call_id": call_id, "n": calls, "why": "region_changed"}));
                                changes = Some(package::what_changed(&shown, &again, &r.detail["changed"]));
                                shown = again;
                                prompt = fresh_prompt;
                                feedback = None;
                                continue 'calls;
                            }
                            Err(r) if r.code == "region_changed" || r.code == "region_locked" => {
                                let _ = j("refused", json!({"code": r.code}));
                                let _ = j("voided", json!({"call_id": call_id, "n": calls, "why": r.code}));
                                return done(region_deferred(ask, run, &r.code, &r), &usage, calls, &models);
                            }
                            Err(r) if matches!(r.code.as_str(), "batch_refused" | "records_refused") => {
                                let _ = j("refused", json!({"code": r.code}));
                                refused_by = "soup";
                                break format!("{}: {}", r.message, clip(&r.detail.to_string(), 4000));
                            }
                            Err(r) => return done(fail(&r), &usage, calls, &models),
                        }
                    },
                },
            },
            Err(Failure::Busy(m)) => {
                busy += 1;
                // An automatic run gives the backend back at once and waits
                // for the next scan; one by hand tries a few times first.
                if opts.automatic || busy >= BUSY_TRIES {
                    let mut v = outcome(ask, run, "deferred");
                    v["reason"] = json!("busy");
                    v["message"] = json!(fixed_message("busy"));
                    v["detail"] = json!(m);
                    v["what_to_do"] = json!(if opts.automatic {
                        "the dump stays `received`; automatic processing tries it again at its next scan"
                    } else {
                        "the dump stays `received`; run this again in a moment (another request held the backend, or it was not ready yet)"
                    });
                    return done(v, &usage, calls, &models);
                }
                // The wait keeps the run's lock renewed: it is held through
                // it (at most `BUSY_TRIES - 1` waits of `backoff`).
                with_renewal(&ctx.mcp, &ask.cauldron, run, lock_held.get().then_some(ctx.lock_ttl_secs), &j, || std::thread::sleep(ctx.backoff));
                continue;
            }
            Err(Failure::Parked { message: m, cause, .. }) => {
                let mut v = outcome(ask, run, "deferred");
                v["reason"] = json!("threads_parked");
                v["held_by"] = json!("threads");
                v["park"] = json!(cause);
                v["reached_model"] = json!(!held);
                v["message"] = json!(park_message(cause, held));
                v["detail"] = json!(m);
                v["what_to_do"] = json!("the dump stays `received`; run this again once threads takes calls again (`cauldron threads status` shows the accounts); automatic processing tries again by itself");
                return done(v, &usage, calls, &models);
            }
            Err(Failure::SchemaUnmet { message, .. }) => {
                refused_by = "schema";
                message
            }
            Err(Failure::Unavailable(m)) => {
                let mut v = outcome(ask, run, "failed");
                v["code"] = json!("inference_unavailable");
                v["message"] = json!(fixed_message("inference_unavailable"));
                v["detail"] = json!(m);
                v["what_to_do"] = json!(if is_local(&model) {
                    format!("start the inference service in this environment (`cauldron --env {} start inference`), then run this again; nothing was sent", ctx.env.name)
                } else {
                    format!("start threads in this environment (`cauldron --env {} start threads`) and check that it is logged in, then run this again", ctx.env.name)
                });
                return done(v, &usage, calls, &models);
            }
            Err(Failure::Unsupported(m)) => {
                let mut v = outcome(ask, run, "failed");
                v["code"] = json!("threads_unsupported");
                v["message"] = json!(fixed_message("threads_unsupported"));
                v["detail"] = json!(m);
                v["what_to_do"] = json!("run a threads service from the same cauldron release as ripples (0.1.10 or later), then run this again; nothing was sent");
                return done(v, &usage, calls, &models);
            }
            Err(Failure::Failed { message, .. }) => {
                let mut v = outcome(ask, run, "failed");
                v["code"] = json!("inference_failed");
                v["message"] = json!(fixed_message("inference_failed"));
                v["detail"] = json!(message);
                v["what_to_do"] = json!("nothing was written and the dump stays `received`; run this again, or curate it by hand (`/cauldron:curate-dump`)");
                return done(v, &usage, calls, &models);
            }
        };
        // The one retry, on Opus, with the refusal.
        if retried {
            let mut v = outcome(ask, run, "failed");
            v["code"] = json!("answer_refused");
            v["refused_by"] = json!(refused_by);
            v["message"] = json!(fixed_message("answer_refused"));
            v["detail"] = json!(format!("the answer was refused again after the retry: {}", clip(&refusal, 1000)));
            keep("refusal", json!(refusal));
            v["what_to_do"] = json!("nothing was written and the dump stays `received`; curate it by hand (`/cauldron:curate-dump`)");
            return done(v, &usage, calls, &models);
        }
        retried = true;
        keep("refusal", json!(refusal));
        // A local run stays local: its retry is the same model.
        model = if is_local(&first) { first.clone() } else { RETRY_MODEL.to_string() };
        // The refusal quotes the answer: scrubbed like the rest of the
        // prompt before it is sent back.
        feedback = Some(if scrub_sent { crate::scrub::scrub(&refusal).0 } else { refusal });
    }
}

/// Test-only: under isolation, `CAULDRON_RIPPLES_FAULT=<point>` kills the
/// process at that point (`after-lock`: the run holds its region lock and
/// no call was made; `after-put`: the batch landed and the dump was
/// marked, and the journal has no `done` yet).
fn fault_point(point: &str) {
    if env::egress::process_isolation().is_some() && std::env::var("CAULDRON_RIPPLES_FAULT").as_deref() == Ok(point) {
        std::process::abort();
    }
}

/// The answer's records with the secret scrubber run on every string, the
/// counts journaled (`redacted`, `in: answer`) when anything was replaced.
fn scrub_answer(j: &dyn Fn(&str, Value) -> std::io::Result<()>, records: Vec<Value>, budget: Option<Duration>) -> Result<Value, crate::scrub::Exhausted> {
    let mut v = json!(records);
    let deadline = std::time::Instant::now() + budget.unwrap_or_else(|| crate::scrub::budget_for(crate::scrub::value_bytes(&v)));
    let counts = crate::scrub::scrub_value_within(&mut v, deadline)?;
    if !counts.is_empty() {
        let _ = j("redacted", json!({"in": "answer", "counts": counts}));
    }
    Ok(v)
}

/// At most `n` characters of `s`.
fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// **The caps, counted from the journal's `lines`** (module docs, step 4):
/// the run's `deferred` outcome when a call of about `this_est` input tokens
/// would pass one, else `None`.
fn over_caps(ctx: &Ctx, p: &Prepared, lines: &[Value], calls_cap: u32, tokens_cap: u64, budget_note: &str, this_est: u64) -> Option<Value> {
    let (ask, run) = (&p.ask, p.run.as_str());
    let (spent_calls, spent_tokens) = crate::journal::spent(lines, run);
    if spent_calls >= calls_cap || spent_tokens + this_est > tokens_cap {
        let mut v = outcome(ask, run, "deferred");
        v["reason"] = json!("budget");
        v["message"] = json!(format!(
            "the next call would pass {budget_note}: {spent_calls} calls and {spent_tokens} input tokens spent on this dump, about {this_est} more needed; nothing more was sent"
        ));
        v["what_to_do"] = json!("the dump stays `received`; curate it by hand (`/cauldron:curate-dump`), or run it again with `--force-budget <tokens>` to let one run spend that many more");
        return Some(v);
    }
    let date = crate::journal::today();
    let today = crate::journal::day_of(lines, &ask.cauldron, &p.identities, &date);
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
        return Some(v);
    }
    None
}

/// **Hold `regions` for the run** (`RLOCK`): the lock, and the regions and
/// their revisions read at that moment.
fn take_lock(ctx: &Ctx, p: &Prepared, regions: &[String]) -> Result<Value, Refused> {
    ctx.mcp.ask(
        "RLOCK",
        &json!({"cauldron": p.ask.cauldron, "run": p.run, "dump": p.ask.dump, "regions": regions, "ttl_secs": ctx.lock_ttl_secs}),
    )
}

/// Run `f` (a model call), renewing the run's lock every third of its time
/// to live (`held`: the time to live, when the run holds a lock) while it
/// runs. A renewal that finds the
/// lock gone is journaled (`lock_lost`); the put's revision check decides.
fn with_renewal<T>(mcp: &Mcp, cauldron: &str, run: &str, held: Option<u64>, j: &(dyn Fn(&str, Value) -> std::io::Result<()> + Sync), f: impl FnOnce() -> T) -> T {
    let Some(ttl) = held else {
        return f();
    };
    let period = Duration::from_millis((ttl * 1000 / 3).max(200));
    let stop = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|sc| {
        sc.spawn(|| {
            let mut next = std::time::Instant::now() + period;
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                if std::time::Instant::now() >= next {
                    next = std::time::Instant::now() + period;
                    match mcp.ask("RRENEW", &json!({"cauldron": cauldron, "run": run, "ttl_secs": ttl})) {
                        Ok(v) if v["renewed"] == json!(0) => {
                            let _ = j("lock_lost", json!({}));
                        }
                        Ok(_) => {}
                        Err(r) => {
                            let _ = j("renew_failed", json!({"code": r.code}));
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let out = f();
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        out
    })
}

/// One hour, in milliseconds: the window of the hold budget.
const HOUR_MS: i64 = 3_600_000;

/// **The hourly hold budget per region** (`Ctx::hold_budget_secs`): the
/// run's `deferred` outcome (reason `hold_budget`) when ripples already
/// held one of `declared` for the whole budget within the last hour, as this
/// service's journal counts it (`crate::journal::region_hold_ms`), else
/// `None`. Checked before any lock is taken and before any call.
fn over_hold_budget(ctx: &Ctx, p: &Prepared, declared: &[String]) -> Option<Value> {
    if declared.is_empty() {
        return None;
    }
    let lines = ctx.journal.lines();
    let now = chrono::Utc::now().timestamp_millis();
    let budget = ctx.hold_budget_secs as i64 * 1000;
    let used = crate::journal::region_hold_ms(&lines, &p.ask.cauldron, now, HOUR_MS);
    let spent: Vec<String> = declared.iter().filter(|r| used.get(*r).copied().unwrap_or(0) >= budget).cloned().collect();
    if spent.is_empty() {
        return None;
    }
    // When every one of them is under its budget again, if nothing more is
    // held meanwhile: a minute at a time, at most the hour.
    let retry_ms = (1..=60)
        .map(|m| m * 60_000)
        .find(|d| {
            let u = crate::journal::region_hold_ms(&lines, &p.ask.cauldron, now + d, HOUR_MS);
            spent.iter().all(|r| u.get(r).copied().unwrap_or(0) < budget)
        })
        .unwrap_or(HOUR_MS);
    let mut v = outcome(&p.ask, &p.run, "deferred");
    v["reason"] = json!("hold_budget");
    v["message"] = json!(fixed_message("hold_budget"));
    v["regions"] = json!(spent);
    v["hold_budget_secs_per_hour"] = json!(ctx.hold_budget_secs);
    v["retry_after_secs"] = json!(retry_ms / 1000);
    v["what_to_do"] = json!("the dump stays `received`; automatic processing tries it again once the budget frees up, or run it again later, or curate it by hand (`/cauldron:curate-dump`)");
    Some(v)
}

/// The run's outcome when a region held it back: `deferred`, with the
/// holder's retry-after when it gave one.
fn region_deferred(ask: &Ask, run: &str, reason: &str, r: &Refused) -> Value {
    let mut v = outcome(ask, run, "deferred");
    v["reason"] = json!(reason);
    v["message"] = json!(fixed_message(reason));
    v["detail"] = json!(r.message);
    if let Some(n) = r.detail.get("retry_after_secs") {
        v["retry_after_secs"] = n.clone();
    }
    v["what_to_do"] = json!(if reason == "region_locked" {
        "the dump stays `received`; it is tried again once that region is let go (automatic processing does so by itself), or run this again later"
    } else {
        "the dump stays `received`; automatic processing tries it again at its next scan, or run this again"
    });
    v
}

/// `args` with the revisions the run read, when it has them.
fn with_revs(mut args: Value, revs: &Option<Value>) -> Value {
    if let Some(r) = revs {
        args["revs"] = r.clone();
    }
    args
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
