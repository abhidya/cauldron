//! **The evaluation**: backends side by side on a fixed set of fixture
//! dumps, with nothing written to any soup.
//!
//! `cauldron ripples eval --dumps <dir> --backends threads,local --out <dir>
//! --env <env> --cauldron <name>` runs, for each fixture dump (each `.txt` or
//! `.md` file of the directory, read as a text dump) and each backend, the
//! dump package's prompt with the cauldron's regions, exactly as a run would
//! build it, and then:
//!
//! - one call; an answer that misses the schema, or that the soup would
//!   refuse, gets ONE retry on the same model with the refusal (as a run's
//!   retry, but never another model: the evaluation is of the model named);
//! - every answer that meets the schema is put through the `mcp` service's
//!   dry run (`RCHECK`): the stage's limits and the soup's grammar, judged,
//!   never landed. No record is written and no dump is touched;
//! - the local backend may run each dump several times (`--repeat`, to see
//!   its variance); threads runs each dump once.
//!
//! **What is measured, mechanically** (quality is a person's to judge, or a
//! later step's): schema validity on the first try and after the retry; the
//! dry run's verdict; the record count and the types; **cite presence**: how
//! many records quote the dump (a quoted span of at least 12 characters, or
//! a run of 8 words, found in the dump's text); **region coverage**: the
//! regions used of those offered, and any id that is not one; **duplicate
//! handles**: handles that are the same once case, spacing and punctuation
//! are set aside; and per call the tokens, the wall time and tokens per
//! second (the local backend's own rate when it reports one).
//!
//! **What it writes** (into `--out`, which must be new or empty): per dump a
//! directory with one JSON file per backend run (the answer, the records in
//! the published shape, every attempt with its usage and timing, the
//! measures) and `side-by-side.md`, the runs' records and measures as
//! tables a person can read; at the top `summary.md` and `summary.json`
//! (one row per dump and backend run), `eval.json` (what was asked) and
//! `progress.jsonl`. The dump's text is never copied out.
//!
//! **Spend.** A threads call is the owner's Claude spend, and is accounted
//! in its own ledger (`eval.jsonl`, beside the run journal), never in the
//! run journal: it never counts against a cauldron's caps for runs, its
//! daily dump slots included. It is bounded by the evaluation's own
//! `max_calls` (required when threads is a backend), by the per-dump input
//! cap (a fixture whose prompt is over it is skipped), and by a daily
//! input-token cap of the evaluations' own (the same size as a cauldron's,
//! counted in the ledger). It is charged BEFORE it is sent, as a run's call
//! is: a `call` line written under the ledger's lock at its estimate (and
//! refused there when a bound would be passed), then its `sent` handle, and
//! its `called` line when it ends; one a kill or a stop cut short stays
//! charged, and is cancelled at the service's next start. [`plan`] answers
//! what an evaluation may spend before it starts. A local call costs no
//! money and is not journaled; its tokens are in the files.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Map, Value};

use crate::engine::{estimate, Ctx, Workdir, MAX_OUTPUT_TOKENS};
use crate::inference::{Failure, Request, Usage};
use crate::package;

/// The most fixture dumps one evaluation reads.
pub const MAX_DUMPS: usize = 50;
/// The most times a local backend runs one dump.
pub const MAX_REPEAT: u32 = 5;
/// The most calls one backend run makes when its backend is busy.
const BUSY_TRIES: u32 = 3;
/// The largest `max_calls`: every fixture, a call and a retry each.
pub const MAX_CALLS: u32 = 2 * MAX_DUMPS as u32;
/// What a retry adds to a call's estimate, at most: the refusal quoted
/// back (up to 4000 characters) and the line around it.
const RETRY_EXTRA_TOKENS: u64 = 1100;

/// What was asked.
#[derive(Debug, Clone)]
pub struct Spec {
    pub cauldron: String,
    pub dumps: PathBuf,
    pub out: PathBuf,
    /// `threads` and/or `local`, in order.
    pub backends: Vec<String>,
    /// The model of the threads runs (`sonnet` unless asked).
    pub threads_model: String,
    /// The model of the local runs (`local:<name>`).
    pub local_model: Option<String>,
    /// Runs of each dump on the local backend.
    pub repeat: u32,
    /// The most threads calls this evaluation makes (required when threads
    /// is a backend).
    pub max_calls: Option<u32>,
    /// The evaluation's id (a plain id).
    pub id: String,
}

/// A fixture dump: its label (from its file name) and its text.
#[derive(Debug, Clone)]
pub struct Fixture {
    pub label: String,
    pub file: String,
    pub text: String,
}

/// A file stem as a plain label.
fn label_of(stem: &str) -> String {
    let l: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).take(60).collect();
    if l.is_empty() {
        "dump".into()
    } else {
        l
    }
}

/// **The fixtures** of `dir`: each `.txt` or `.md` file, by name.
pub fn fixtures(dir: &Path) -> Result<Vec<Fixture>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("cannot read the fixture directory {}: {e}", dir.display()))?;
    let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_file() && matches!(p.extension().and_then(|x| x.to_str()), Some("txt") | Some("md"))).collect();
    files.sort();
    if files.is_empty() {
        return Err(format!("{} holds no fixture dump (a .txt or .md file, one dump each)", dir.display()));
    }
    if files.len() > MAX_DUMPS {
        return Err(format!("{} holds {} fixture dumps; an evaluation reads at most {MAX_DUMPS}", dir.display(), files.len()));
    }
    let mut out: Vec<Fixture> = Vec::new();
    for p in files {
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        if text.trim().is_empty() {
            return Err(format!("{} is empty", p.display()));
        }
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("dump");
        let mut label = label_of(stem);
        if out.iter().any(|f| f.label == label) {
            label = format!("{label}-{}", out.len() + 1);
        }
        out.push(Fixture { label, file: p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string(), text });
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────── measures ──

fn norm_words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(|w| w.to_lowercase()).collect()
}

fn norm(s: &str) -> String {
    norm_words(s).join(" ")
}

/// Quoted spans of `s` ("…", “…”, `…`), at least 12 characters.
fn quotes(s: &str) -> Vec<String> {
    let mut out = vec![];
    for (open, close) in [('"', '"'), ('\u{201c}', '\u{201d}'), ('`', '`')] {
        let mut rest = s;
        while let Some(i) = rest.find(open) {
            let after = &rest[i + open.len_utf8()..];
            let Some(j) = after.find(close) else { break };
            let q = &after[..j];
            if q.chars().count() >= 12 {
                out.push(q.to_string());
            }
            rest = &after[j + close.len_utf8()..];
        }
    }
    out
}

/// **Does `record` quote `dump`?** A quoted span of at least 12 characters
/// found in the dump (case and spacing aside), or a run of 8 words the dump
/// also has.
pub fn cites(record: &Value, dump_norm: &str, dump_grams: &std::collections::HashSet<String>) -> bool {
    let text = format!("{} {}", record["handle"].as_str().unwrap_or(""), record["body"].as_str().unwrap_or(""));
    if quotes(&text).iter().map(|q| norm(q)).any(|q| !q.is_empty() && dump_norm.contains(&q)) {
        return true;
    }
    let w = norm_words(&text);
    w.windows(8).any(|g| dump_grams.contains(&g.join(" ")))
}

/// The measures of one answer (`output`, as the model gave it) for a dump
/// with text `dump` against the regions offered.
pub fn measures(output: &Value, dump: &str, offered: &[String]) -> Value {
    let records: Vec<Value> = output.get("records").and_then(Value::as_array).cloned().unwrap_or_default();
    let dump_norm = norm(dump);
    let words = norm_words(dump);
    let grams: std::collections::HashSet<String> = words.windows(8).map(|g| g.join(" ")).collect();
    let citing = records.iter().filter(|r| cites(r, &dump_norm, &grams)).count();
    let mut used: Vec<String> = records.iter().flat_map(|r| r["regions"].as_array().cloned().unwrap_or_default()).filter_map(|x| x.as_str().map(str::to_string)).collect();
    used.sort();
    used.dedup();
    let unknown: Vec<&String> = used.iter().filter(|u| !offered.contains(u)).collect();
    let known = used.len() - unknown.len();
    let mut seen: Map<String, Value> = Map::new();
    for r in &records {
        let h = norm(r["handle"].as_str().unwrap_or(""));
        let n = seen.get(&h).and_then(Value::as_u64).unwrap_or(0);
        seen.insert(h, json!(n + 1));
    }
    let dups: Vec<Value> = seen.iter().filter(|(_, n)| n.as_u64().unwrap_or(0) > 1).map(|(h, n)| json!({"handle": h, "times": n})).collect();
    let mut types: Map<String, Value> = Map::new();
    for r in &records {
        let t = r["type"].as_str().unwrap_or("?").to_string();
        let n = types.get(&t).and_then(Value::as_u64).unwrap_or(0);
        types.insert(t, json!(n + 1));
    }
    let frac = |a: usize, b: usize| if b == 0 { Value::Null } else { json!(((a as f64 / b as f64) * 1000.0).round() / 1000.0) };
    json!({
        "records": records.len(),
        "types": types,
        "cites": {"records_quoting_the_dump": citing, "fraction": frac(citing, records.len())},
        "regions": {"offered": offered.len(), "used": used, "used_known": known, "coverage": frac(known, offered.len()), "unknown": unknown},
        "duplicate_handles": dups,
    })
}

// ─────────────────────────────────────────────────────────────────── runs ──

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn write_json(p: &Path, v: &Value) -> Result<(), String> {
    std::fs::write(p, format!("{}\n", serde_json::to_string_pretty(v).unwrap_or_default())).map_err(|e| format!("{}: {e}", p.display()))
}

fn progress(out: &Path, v: Value) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(out.join("progress.jsonl")) {
        let mut v = v;
        v["at"] = json!(now());
        let _ = writeln!(f, "{v}");
    }
}

/// The out directory: new, or empty; owner-only (0700) either way.
fn prepare_out(out: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    if !out.is_absolute() {
        return Err(format!("the out directory must be an absolute path, not {}", out.display()));
    }
    if out.exists() {
        let n = std::fs::read_dir(out).map_err(|e| format!("{}: {e}", out.display()))?.count();
        if n > 0 {
            return Err(format!("{} is not empty; give a new directory (an evaluation never overwrites one)", out.display()));
        }
    }
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(out).map_err(|e| format!("creating {}: {e}", out.display()))?;
    // An empty directory given already keeps its mode through `create`:
    // set it, so answers drawn from dumps are the owner's alone.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o700)).map_err(|e| format!("making {} owner-only: {e}", out.display()))
}

/// One backend run of one fixture: up to two answers, each judged.
#[allow(clippy::too_many_arguments)]
fn one_run(ctx: &Ctx, spec: &Spec, f: &Fixture, backend: &str, model: &str, k: u32, regions: &[Value], offered: &[String]) -> Value {
    let base_prompt = package::prompt(&format!("fixture:{}", f.label), "text", regions, &f.text);
    let est = estimate(base_prompt.chars().count());
    let mut result = json!({"dump": f.label, "file": f.file, "backend": backend, "model": model, "repeat": k, "estimated_input_tokens": est});
    if est > ctx.caps.input_tokens {
        result["skipped"] = json!(format!("the prompt is about {est} input tokens, over the per-dump cap of {}", ctx.caps.input_tokens));
        return result;
    }
    let call_id = format!("{}-{}-{backend}-{k}", spec.id, f.label);
    let ledger = ctx.journal.eval_ledger();
    let dump_id = format!("fixture:{}", f.label);
    let sink: crate::inference::Sink = {
        let (path, call_id, cauldron, dump, eval_id) = (ledger.path().to_path_buf(), call_id.clone(), spec.cauldron.clone(), dump_id.clone(), spec.id.clone());
        std::sync::Arc::new(move |event: &str, v: Value| {
            // A local call's own line stays out of the ledger; a threads
            // call's handle and cancel go in, so a call a kill left is
            // settled at the next start.
            if event == "local" {
                return;
            }
            let mut v = v;
            v["eval_id"] = json!(eval_id);
            let _ = crate::journal::Journal::at(&path).write(&call_id, &cauldron, &dump, event, v);
        })
    };
    let local_line: std::sync::Arc<std::sync::Mutex<Option<Value>>> = Default::default();
    let sink: crate::inference::Sink = {
        let (inner, keep) = (sink.clone(), local_line.clone());
        std::sync::Arc::new(move |event: &str, v: Value| {
            if event == "local" {
                *keep.lock().unwrap_or_else(|p| p.into_inner()) = Some(v.clone());
            }
            inner(event, v);
        })
    };
    let schema = package::schema();
    let mut attempts: Vec<Value> = vec![];
    let mut feedback: Option<String> = None;
    let mut last_output = Value::Null;
    let mut records: Option<Vec<Value>> = None;
    let mut busy = 0;
    let mut answers = 0;
    while answers < 2 {
        let prompt = match &feedback {
            None => base_prompt.clone(),
            Some(why) => format!("{base_prompt}\n\n## Your previous answer was refused\n\n{why}\n\nAnswer again, meeting the schema and the rules above."),
        };
        let this_est = estimate(prompt.chars().count());
        let n = attempts.len() + 1;
        if backend == "threads" {
            // Charged before it is sent, under the ledger's lock, or not sent.
            let max = spec.max_calls.unwrap_or(0);
            let begun = ledger.begin(&call_id, &spec.cauldron, &dump_id, |lines| {
                let made = lines.iter().filter(|l| l["event"] == "call" && l["eval_id"] == json!(spec.id)).count() as u32;
                if made >= max {
                    return Err(format!("the evaluation's bound of {max} threads calls (`--max-calls`) is reached; nothing more was sent"));
                }
                let day = crate::journal::day(lines, &spec.cauldron, &crate::journal::today());
                if day.tokens + this_est > ctx.caps.day_tokens {
                    return Err(format!("the next call would pass the evaluations' own daily cap of {} input tokens for cauldron `{}` ({} spent today); nothing more was sent", ctx.caps.day_tokens, spec.cauldron, day.tokens));
                }
                Ok(json!({"eval_id": spec.id, "call_id": n, "model": model, "estimated_input_tokens": this_est}))
            });
            match begun {
                Ok(Ok(())) => {}
                Ok(Err(why)) => {
                    result["stopped"] = json!(why);
                    break;
                }
                Err(e) => {
                    result["error"] = json!(format!("cannot write the evaluations' ledger {}: {e}; nothing was sent", ledger.path().display()));
                    break;
                }
            }
        }
        let workdir = match Workdir::make(&ctx.scratch_root, &call_id) {
            Ok(w) => w,
            Err(e) => {
                result["error"] = json!(format!("cannot make a private working directory: {e}"));
                break;
            }
        };
        *local_line.lock().unwrap_or_else(|p| p.into_inner()) = None;
        let req = Request { model: model.to_string(), prompt, schema: schema.clone(), cwd: workdir.0.clone(), name: call_id.clone(), max_output_tokens: MAX_OUTPUT_TOKENS, sink: Some(sink.clone()) };
        let t0 = Instant::now();
        let answer = ctx.inference.call(&req);
        let wall_ms = t0.elapsed().as_millis() as u64;
        drop(workdir);
        let usage: Usage = match &answer {
            Ok(r) => r.usage.clone(),
            Err(f) => f.usage(),
        };
        if backend == "threads" {
            let charged = if matches!(answer, Err(Failure::Unsupported(_)) | Err(Failure::Unavailable(_))) { usage.input_total() } else { usage.input_total().max(this_est) };
            let _ = ledger.write(&call_id, &spec.cauldron, &dump_id, "called", json!({"eval_id": spec.id, "call_id": n, "model": model, "usage": usage, "input_total": usage.input_total(), "charged": charged}));
        }
        let local = local_line.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let tps = local.as_ref().and_then(|l| l["tokens_per_second"].as_f64()).or_else(|| (wall_ms > 0 && usage.output_tokens > 0).then(|| usage.output_tokens as f64 * 1000.0 / wall_ms as f64));
        let mut a = json!({"n": attempts.len() + 1, "wall_ms": wall_ms, "usage": usage, "tokens_per_second": tps});
        if let Some(l) = &local {
            a["sha256"] = l["sha256"].clone();
            a["load_ms"] = l["load_ms"].clone();
        }
        let refusal: String = match answer {
            Err(Failure::Busy(m)) if busy + 1 < BUSY_TRIES => {
                busy += 1;
                a["failure"] = json!({"code": "busy", "message": m});
                attempts.push(a);
                std::thread::sleep(ctx.backoff);
                continue;
            }
            Err(Failure::SchemaUnmet { message, .. }) => {
                a["schema_valid"] = json!(false);
                a["failure"] = json!({"code": "schema_unmet", "message": message});
                message
            }
            Err(f) => {
                a["failure"] = json!({"code": f.code(), "message": f.message()});
                attempts.push(a);
                result["error"] = json!(format!("{}: {}", f.code(), f.message()));
                break;
            }
            Ok(reply) => {
                last_output = reply.output.clone();
                match package::to_records(&reply.output) {
                    Err(why) => {
                        a["schema_valid"] = json!(false);
                        a["refused"] = json!(why);
                        why
                    }
                    Ok(recs) => {
                        a["schema_valid"] = json!(true);
                        let check = ctx.mcp.ask(
                            "RCHECK",
                            &json!({"cauldron": spec.cauldron, "run": call_id, "step": "check", "package": package::PACKAGE, "version": package::VERSION, "model": model, "records": recs}),
                        );
                        match check {
                            Ok(v) => {
                                a["dry_run"] = json!({"ok": true, "outcome": v["outcome"]});
                                records = Some(recs);
                                attempts.push(a);
                                break;
                            }
                            Err(r) if matches!(r.code.as_str(), "batch_refused" | "records_refused") => {
                                a["dry_run"] = json!({"ok": false, "code": r.code, "detail": r.detail});
                                format!("{}: {}", r.message, r.detail.to_string().chars().take(4000).collect::<String>())
                            }
                            Err(r) => {
                                a["dry_run"] = json!({"ok": false, "code": r.code, "message": r.message});
                                attempts.push(a);
                                result["error"] = json!(format!("the dry run could not be made: {} ({})", r.message, r.code));
                                break;
                            }
                        }
                    }
                }
            }
        };
        attempts.push(a);
        answers += 1;
        feedback = Some(refusal);
    }
    let valid_first = attempts.iter().find(|a| a.get("schema_valid").is_some()).map(|a| a["schema_valid"] == json!(true) && a["dry_run"]["ok"] == json!(true));
    result["first_try_valid"] = json!(valid_first);
    result["valid"] = json!(records.is_some());
    result["valid_after_retry"] = json!(records.is_some() && valid_first == Some(false));
    let mut total = Usage::default();
    for a in &attempts {
        if let Ok(u) = serde_json::from_value::<Usage>(a["usage"].clone()) {
            total.add(&u);
        }
    }
    result["usage"] = json!(total);
    result["wall_ms"] = json!(attempts.iter().filter_map(|a| a["wall_ms"].as_u64()).sum::<u64>());
    result["calls"] = json!(attempts.len());
    result["attempts"] = json!(attempts);
    result["measures"] = measures(&last_output, &f.text, offered);
    if let Some(n) = last_output.get("notes") {
        result["notes"] = n.clone();
    }
    result["answer"] = last_output;
    result["records"] = json!(records);
    result
}

/// **What an evaluation may spend, before it starts**: the fixtures read,
/// the cauldron's regions read (no model is called and nothing is
/// written), and for the threads backend the most calls it may make (a
/// call and a retry for each fixture within the per-dump cap, up to
/// `max_calls`) and the most input tokens those calls are estimated at.
pub fn plan(ctx: &Ctx, spec: &Spec) -> Result<Value, String> {
    let fx = fixtures(&spec.dumps)?;
    let read = ctx.mcp.ask("RREAD", &json!({"cauldron": spec.cauldron})).map_err(|r| format!("reading the cauldron's regions: {} ({})", r.message, r.code))?;
    let regions: Vec<Value> = read["regions"].as_array().cloned().unwrap_or_default();
    if regions.is_empty() {
        return Err(format!("cauldron `{}` has no region; the prompt offers the model the cauldron's regions", spec.cauldron));
    }
    let mut out = json!({"eval_plan": true, "dumps": fx.len(), "backends": spec.backends, "nothing_sent": true});
    if spec.backends.iter().any(|b| b == "threads") {
        let max = spec.max_calls.unwrap_or(0);
        let (mut calls, mut tokens, mut skipped) = (0u32, 0u64, vec![]);
        for f in &fx {
            let est = estimate(package::prompt(&format!("fixture:{}", f.label), "text", &regions, &f.text).chars().count());
            if est > ctx.caps.input_tokens {
                skipped.push(json!({"dump": f.label, "estimated_input_tokens": est}));
                continue;
            }
            for extra in [0, RETRY_EXTRA_TOKENS] {
                if calls < max {
                    calls += 1;
                    tokens += est + extra;
                }
            }
        }
        out["threads"] = json!({
            "model": spec.threads_model, "max_calls": max, "calls_at_most": calls,
            "estimated_input_tokens_at_most": tokens, "output_tokens_per_call_at_most": MAX_OUTPUT_TOKENS,
            "skipped_over_the_per_dump_cap": skipped,
            "accounted": "in the evaluations' ledger, charged before each call is sent; never against the cauldron's caps for runs",
        });
    }
    if spec.backends.iter().any(|b| b == "local") {
        out["local"] = json!({"model": spec.local_model, "calls_at_most": fx.len() as u32 * spec.repeat * 2, "cost": "no money; this machine's time"});
    }
    Ok(out)
}

/// The file name of a backend run.
fn run_file(backend: &str, k: u32, repeat: u32) -> String {
    if repeat > 1 {
        format!("{backend}-{k}.json")
    } else {
        format!("{backend}.json")
    }
}

/// **Run the evaluation** (module docs); answers the summary, also written
/// to `summary.json`.
pub fn run(ctx: &Ctx, spec: &Spec) -> Value {
    let fail = |m: String| json!({"eval": spec.id, "outcome": "failed", "message": m});
    if spec.backends.is_empty() || spec.backends.iter().any(|b| !matches!(b.as_str(), "threads" | "local")) {
        return fail(format!("`backends` is a list of `threads` and `local`, not {:?}", spec.backends));
    }
    if spec.backends.iter().any(|b| b == "local") && spec.local_model.is_none() {
        return fail("the local backend needs a model (`--local-model <name>`, or register exactly one)".into());
    }
    if !(1..=MAX_REPEAT).contains(&spec.repeat) {
        return fail(format!("`repeat` is 1 to {MAX_REPEAT}"));
    }
    if spec.backends.iter().any(|b| b == "threads") && !spec.max_calls.is_some_and(|n| (1..=MAX_CALLS).contains(&n)) {
        return fail(format!("the threads backend is the owner's Claude spend: the evaluation needs its bound, `max_calls` (1 to {MAX_CALLS})"));
    }
    let fx = match fixtures(&spec.dumps) {
        Ok(f) => f,
        Err(m) => return fail(m),
    };
    if let Err(m) = prepare_out(&spec.out) {
        return fail(m);
    }
    let read = match ctx.mcp.ask("RREAD", &json!({"cauldron": spec.cauldron})) {
        Ok(v) => v,
        Err(r) => return fail(format!("reading the cauldron's regions: {} ({})", r.message, r.code)),
    };
    let regions: Vec<Value> = read["regions"].as_array().cloned().unwrap_or_default();
    if regions.is_empty() {
        return fail(format!("cauldron `{}` has no region; the prompt offers the model the cauldron's regions", spec.cauldron));
    }
    let offered: Vec<String> = regions.iter().take(package::MAX_REGIONS).filter_map(|r| r["id"].as_str().map(str::to_string)).collect();
    let _ = write_json(
        &spec.out.join("eval.json"),
        &json!({"eval": spec.id, "started_at": now(), "cauldron": spec.cauldron, "dumps_dir": spec.dumps, "dumps": fx.iter().map(|f| json!({"label": f.label, "file": f.file, "chars": f.text.chars().count()})).collect::<Vec<_>>(),
                "backends": spec.backends, "threads_model": spec.threads_model, "local_model": spec.local_model, "repeat_local": spec.repeat,
                "package": format!("{}@{}", package::PACKAGE, package::VERSION), "regions_offered": offered.len(), "nothing_written": "every answer was judged by the mcp service's dry run (RCHECK); no record landed and no dump was touched"}),
    );
    let mut rows: Vec<Value> = vec![];
    for f in &fx {
        let dir = spec.out.join(&f.label);
        {
            use std::os::unix::fs::DirBuilderExt;
            let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir);
        }
        let mut runs: Vec<Value> = vec![];
        for b in &spec.backends {
            let (model, reps) = if b == "local" { (spec.local_model.clone().unwrap_or_default(), spec.repeat) } else { (spec.threads_model.clone(), 1) };
            for k in 1..=reps {
                progress(&spec.out, json!({"event": "call", "dump": f.label, "backend": b, "model": model, "repeat": k}));
                let r = one_run(ctx, spec, f, b, &model, k, &regions, &offered);
                let _ = write_json(&dir.join(run_file(b, k, reps)), &r);
                progress(&spec.out, json!({"event": "done", "dump": f.label, "backend": b, "repeat": k, "valid": r["valid"], "records": r["measures"]["records"], "error": r.get("error"), "skipped": r.get("skipped")}));
                rows.push(row(&r));
                runs.push(r);
            }
        }
        let _ = std::fs::write(dir.join("side-by-side.md"), side_by_side(f, &runs, &offered));
    }
    let summary = json!({"eval": spec.id, "outcome": "done", "finished_at": now(), "out": spec.out, "dumps": fx.len(), "rows": rows});
    let _ = write_json(&spec.out.join("summary.json"), &summary);
    let _ = std::fs::write(spec.out.join("summary.md"), summary_md(spec, &rows));
    progress(&spec.out, json!({"event": "finished", "summary": spec.out.join("summary.md")}));
    summary
}

/// One summary row.
fn row(r: &Value) -> Value {
    let m = &r["measures"];
    json!({
        "dump": r["dump"], "backend": r["backend"], "model": r["model"], "repeat": r["repeat"],
        "first_try_valid": r["first_try_valid"], "valid": r["valid"], "valid_after_retry": r["valid_after_retry"],
        "records": m["records"], "records_quoting_the_dump": m["cites"]["records_quoting_the_dump"], "regions_used": m["regions"]["used_known"],
        "regions_offered": m["regions"]["offered"], "unknown_regions": m["regions"]["unknown"], "duplicate_handles": m["duplicate_handles"].as_array().map_or(0, Vec::len),
        "input_tokens": r["usage"]["input_tokens"], "output_tokens": r["usage"]["output_tokens"], "calls": r["calls"], "wall_ms": r["wall_ms"],
        "tokens_per_second": r["attempts"].as_array().and_then(|a| a.last()).map(|a| a["tokens_per_second"].clone()).unwrap_or(Value::Null),
        "error": r.get("error").or(r.get("skipped")).or(r.get("stopped")).cloned().unwrap_or(Value::Null),
    })
}

fn cell(v: &Value) -> String {
    match v {
        Value::Null => "–".into(),
        Value::Bool(true) => "yes".into(),
        Value::Bool(false) => "no".into(),
        Value::String(s) => s.replace('|', "\\|").replace('\n', " "),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() => format!("{f:.1}"),
            _ => n.to_string(),
        },
        other => other.to_string().replace('|', "\\|"),
    }
}

fn summary_md(spec: &Spec, rows: &[Value]) -> String {
    let mut s = format!(
        "# Ripples evaluation `{}`\n\nCauldron `{}`; fixture dumps from `{}`; backends {}; threads model `{}`; local model `{}`, {} run(s) a dump.\nNothing was written to the soup: every schema-valid answer was judged by the mcp service's dry run.\n\n",
        spec.id,
        spec.cauldron,
        spec.dumps.display(),
        spec.backends.join(", "),
        spec.threads_model,
        spec.local_model.as_deref().unwrap_or("–"),
        spec.repeat
    );
    s.push_str("| dump | backend | run | valid 1st try | valid | records | quoting the dump | regions used / offered | unknown regions | duplicate handles | input tok | output tok | calls | wall s | tok/s | problem |\n");
    s.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for r in rows {
        let wall = r["wall_ms"].as_u64().map(|m| format!("{:.1}", m as f64 / 1000.0)).unwrap_or_else(|| "–".into());
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} / {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            cell(&r["dump"]),
            cell(&r["backend"]),
            cell(&r["repeat"]),
            cell(&r["first_try_valid"]),
            cell(&r["valid"]),
            cell(&r["records"]),
            cell(&r["records_quoting_the_dump"]),
            cell(&r["regions_used"]),
            cell(&r["regions_offered"]),
            r["unknown_regions"].as_array().map_or("–".into(), |a| if a.is_empty() { "none".into() } else { a.iter().map(cell).collect::<Vec<_>>().join(", ") }),
            cell(&r["duplicate_handles"]),
            cell(&r["input_tokens"]),
            cell(&r["output_tokens"]),
            cell(&r["calls"]),
            wall,
            cell(&r["tokens_per_second"]),
            cell(&r["error"]),
        ));
    }
    s.push_str("\nEach dump's directory holds `side-by-side.md` (the records of every run, to read and judge) and one JSON file per run (the answer, every attempt, the measures).\n");
    s
}

fn side_by_side(f: &Fixture, runs: &[Value], offered: &[String]) -> String {
    let mut s = format!("# `{}` side by side\n\nFixture `{}` ({} characters; its text is not copied here). Regions offered: {}.\n\n", f.label, f.file, f.text.chars().count(), offered.join(", "));
    for r in runs {
        let m = &r["measures"];
        s.push_str(&format!("## {} `{}` (run {})\n\n", cell(&r["backend"]), cell(&r["model"]), cell(&r["repeat"])));
        if let Some(e) = r.get("error").or(r.get("skipped")).or(r.get("stopped")) {
            s.push_str(&format!("**Problem:** {}\n\n", cell(e)));
        }
        s.push_str(&format!(
            "valid on the first try: {} · valid: {} · records: {} · quoting the dump: {} · regions used: {} · unknown regions: {} · duplicate handles: {} · tokens in/out: {}/{} · calls: {} · wall: {} ms\n\n",
            cell(&r["first_try_valid"]),
            cell(&r["valid"]),
            cell(&m["records"]),
            cell(&m["cites"]["records_quoting_the_dump"]),
            m["regions"]["used"].as_array().map_or("–".into(), |a| a.iter().map(cell).collect::<Vec<_>>().join(", ")),
            m["regions"]["unknown"].as_array().map_or("–".into(), |a| if a.is_empty() { "none".into() } else { a.iter().map(cell).collect::<Vec<_>>().join(", ") }),
            m["duplicate_handles"].as_array().map_or(0, Vec::len),
            cell(&r["usage"]["input_tokens"]),
            cell(&r["usage"]["output_tokens"]),
            cell(&r["calls"]),
            cell(&r["wall_ms"]),
        ));
        let recs = r["answer"]["records"].as_array().cloned().unwrap_or_default();
        if recs.is_empty() {
            s.push_str("_No records._");
            if let Some(n) = r.get("notes") {
                s.push_str(&format!(" Notes: {}", cell(n)));
            }
            s.push_str("\n\n");
            continue;
        }
        s.push_str("| label | type | state | handle | regions | derived from | body (start) |\n|---|---|---|---|---|---|---|\n");
        for x in recs {
            let list = |k: &str| x[k].as_array().map_or(String::new(), |a| a.iter().map(cell).collect::<Vec<_>>().join(", "));
            let body: String = x["body"].as_str().unwrap_or("").chars().take(200).collect();
            s.push_str(&format!("| {} | {} | {} | {} | {} | {} | {} |\n", cell(&x["label"]), cell(&x["type"]), cell(&x["state"]), cell(&x["handle"]), list("regions"), list("derived_from"), cell(&json!(body))));
        }
        if let Some(n) = r.get("notes") {
            s.push_str(&format!("\nNotes: {}\n", cell(n)));
        }
        s.push('\n');
    }
    s
}

/// An evaluation id, from the clock.
pub fn new_id() -> String {
    format!("eval-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S%3f"))
}
