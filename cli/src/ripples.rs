//! **`cauldron ripples run | status`**: the dump ripple, by hand.
//!
//! In this release nothing is processed unless the owner asks: `run` sends
//! one dump to the `ripples` service of the environment (which must be
//! running, as must `mcp` and `threads`) and follows it to its outcome;
//! `status` shows the service, the run in progress, the last runs and
//! the spend. The owner approves a ripple's records with
//! `cauldron soup approve`, at a terminal.
//!
//! The service is found through the environment's seam table (its `ripples`
//! row must be live); `RUN` and `STATUS` carry the run key the service
//! minted at its start (`data/<env>/ripples/run.secret`, owner-only), read
//! for each call and never printed. Every dial goes through
//! `env::egress::connect_tcp`.

use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use env::seams::{self, SeamState, Table};
use env::Environment;
use serde_json::{json, Value};

const SERVICE: &str = "ripples";

const USAGE: &str = "\
cauldron ripples — turn a dump into knowledge-graph records, by hand

    cauldron ripples run <DM-n> --env <env> --cauldron <name> [--backend threads|local] [--model <model>] [--force-budget <tokens>]
        Process one dump: its text (a session dump: only what the person typed and what
        the assistant wrote as text) goes to a model, through the `threads` service on
        this machine's Claude login, as a bare completion with no tools, no MCP server and
        no settings, plugins, hooks or CLAUDE.md (`--setting-sources \"\"`, `--safe-mode`),
        with the cauldron's regions (at most 50)
        and nothing else from the cauldron (the whole prompt is
        services/ripples/package/dump/v1/prompt.md, filled in). The answer must meet the
        package's schema; its records land through the `mcp` service as new records, each
        citing the dump and marked `unreviewed` until the owner approves them, and the dump
        moves from `received` to `processed`. One call on Sonnet (or --model), and one retry
        on Opus when the answer is refused. Caps, counted from the run journal before every
        call: per dump, across every run of it, 3 calls and 200000 input tokens; per
        cauldron per UTC day, 1500000 input tokens or 30 dumps. A call is charged when it
        begins (one call, its estimated input tokens), before anything is sent, and settled
        to what it reported when it ends; a call cut short by a kill or a stop of the
        service stays charged. Over a cap the run stops as
        `deferred` and the dump stays `received`. --force-budget <tokens> lets this one run
        spend that many more input tokens (and 3 more calls) on the dump than it has spent;
        the daily cap still holds. A call given up (parked, timed out, ended without an
        answer) is cancelled in threads, and the outcome says whether threads confirmed it.
        A dump already processed is answered as it is; a removed dump is refused. Prints
        the outcome as JSON (the records, the tokens: input, output, cached, the calls and
        the models) and, on a failure, what to do. Needs the `ripples`, `mcp` and `threads`
        services running in the environment (threads from this release) and the cauldron
        granted to the MCP door with read, write and dump. Exit 0 processed, 3 deferred,
        1 failed.
        --backend local runs it on this machine instead, through the `inference` service
        (`cauldron inference help`): --model names a registered model (default: the only
        one registered), both calls use it (no Opus retry), and nothing leaves the
        machine. The same schema, caps, journal and `unreviewed` marking apply; its tokens
        cost no money, and the per-dump call cap still bounds a runaway loop. Needs the
        `ripples`, `mcp` and `inference` services running (not `threads`).
    cauldron ripples status [--env <env>] [--cauldron <name>] [--run <run>]
        The service (running or not), the run in progress, the last ten runs (each `done`
        with its outcome and tokens, `running`, or `interrupted`: cut short, safe to run
        again), the spend today per cauldron and per dump across its runs (a call that
        never ended, cut short by a kill or a stop, is counted and shown in
        `unsettled_calls`), any model call that may still be running in threads (with how to
        stop it), and the caps. With the
        service stopped, all of it is read from its journal (data/<env>/ripples/runs.jsonl).
    cauldron ripples eval --dumps <dir> --backends threads,local --out <dir> --env <env> --cauldron <name>
                          [--local-model <name>] [--threads-model <model>] [--repeat <n>]
                          [--max-calls <n> --yes]
        Backends side by side, with NOTHING written to any soup: each fixture dump (each .txt
        or .md file of --dumps, one dump each) gets the same prompt a run would build, with
        the cauldron's regions, on each backend; an answer that misses the schema or that
        the soup would refuse gets one retry on the same model; every schema-valid answer
        is judged by the mcp service's dry run, never landed. --repeat runs each dump that
        many times on the local backend (1 to 5; threads once). Writes into --out (new or
        empty): per dump a directory with one JSON file per run and side-by-side.md, and
        summary.md / summary.json with, per dump and run, schema validity (first try, after
        the retry), record count, records quoting the dump, regions used of those offered,
        duplicate handles, tokens, wall time and tokens per second. Judging the records is
        left to a person. The dump text is never copied into --out.
        Threads calls are the owner's Claude spend. With `threads` in --backends,
        --max-calls <n> (1 to 100) is required: the most threads calls the evaluation makes.
        The estimated spend (calls, input tokens, the model) is printed first, and nothing
        is sent unless --yes is given. Each threads call is charged before it is sent, in
        the evaluations' own ledger (data/<env>/ripples/eval.jsonl), held to the per-dump
        input cap and to a daily input-token cap of the evaluations' own, and never counted
        against the cauldron's caps for runs. A local-only evaluation costs no money and
        needs neither; its calls are not journaled. The evaluation runs in the service:
        Ctrl-C stops this command following it, not the evaluation, which ends when it is
        done or when the service is stopped (`cauldron --env <env> stop ripples`).
    cauldron ripples help

  The `ripples` service is off unless started (`cauldron --env <env> build ripples`,
  `pin`, then `start ripples`); the installer does not start it, and in this release it
  processes a dump only when this command asks.
";

pub(crate) fn verb_ripples(args: &[String]) -> Result<i32> {
    match args.first().map(String::as_str) {
        Some("help") | Some("-h") | Some("--help") => {
            print!("{USAGE}");
            Ok(0)
        }
        Some("run") => run(&args[1..]),
        Some("status") => status(&args[1..]),
        Some("eval") => eval(&args[1..]),
        None => {
            eprint!("{USAGE}");
            Ok(2)
        }
        Some(other) => {
            eprintln!("cauldron ripples: no verb `{other}` (run | status | eval | help)");
            Ok(2)
        }
    }
}

#[derive(Default)]
struct Args {
    env: Option<String>,
    cauldron: Option<String>,
    model: Option<String>,
    backend: Option<String>,
    run: Option<String>,
    force_budget: Option<String>,
    dumps: Option<String>,
    out: Option<String>,
    backends: Option<String>,
    local_model: Option<String>,
    threads_model: Option<String>,
    repeat: Option<String>,
    max_calls: Option<String>,
    yes: bool,
    pos: Vec<String>,
}

fn parse(verb: &str, args: &[String], allow: &[&str]) -> Result<Args> {
    let mut a = Args::default();
    let mut it = args.iter();
    while let Some(s) = it.next() {
        let (flag, inline) = match s.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (s.as_str(), None),
        };
        if flag.starts_with('-') && !allow.contains(&flag) {
            bail!("unknown option `{s}` for `cauldron ripples {verb}` (`cauldron ripples help`)");
        }
        if flag == "--yes" {
            if inline.is_some() {
                bail!("`--yes` takes no value");
            }
            a.yes = true;
            continue;
        }
        let mut value = || -> Result<String> {
            match &inline {
                Some(v) => Ok(v.clone()),
                None => it.next().cloned().with_context(|| format!("`cauldron ripples {verb} {flag}` needs a value")),
            }
        };
        match flag {
            "--env" => a.env = Some(value()?),
            "--cauldron" => a.cauldron = Some(value()?),
            "--model" => a.model = Some(value()?),
            "--backend" => a.backend = Some(value()?),
            "--run" => a.run = Some(value()?),
            "--force-budget" => a.force_budget = Some(value()?),
            "--dumps" => a.dumps = Some(value()?),
            "--out" => a.out = Some(value()?),
            "--backends" => a.backends = Some(value()?),
            "--local-model" => a.local_model = Some(value()?),
            "--threads-model" => a.threads_model = Some(value()?),
            "--repeat" => a.repeat = Some(value()?),
            "--max-calls" => a.max_calls = Some(value()?),
            _ => a.pos.push(s.clone()),
        }
    }
    Ok(a)
}

/// The running service's seam, or why not.
fn find(e: &Environment) -> std::result::Result<SocketAddr, String> {
    let t = Table::load(&e.name, &e.data_root, &e.seams()).map_err(|err| format!("the seam table cannot be read: {err:#}"))?;
    let Some(row) = t.seams.get(SERVICE) else {
        return Err(format!("the `{SERVICE}` service is not running in environment `{}`", e.name));
    };
    // Loopback, or nothing is dialled (the probe included): the key goes
    // on every line.
    let addr = inference_registry::loopback_addr(&format!("the `{SERVICE}` seam (seam table)"), &row.addr)?;
    let v = seams::probe(row);
    if v.state != SeamState::Live {
        return Err(format!("the `{SERVICE}` service is not reachable ({}: {})", v.state.as_str(), v.detail));
    }
    Ok(addr)
}

fn start_hint(e: &str) -> String {
    format!(
        "Start it: `cauldron --env {e} build ripples`, then `cauldron --env {e} pin ripples <the version it printed>`, then \
         `cauldron --env {e} start ripples` (the `mcp` and `threads` services must run too)."
    )
}

/// One line to the service with the run key; the JSON answer.
fn ask(e: &Environment, addr: SocketAddr, verb: &str, args: &Value) -> Result<Value> {
    let p = e.data_root.join(SERVICE).join("run.secret");
    let key = std::fs::read_to_string(&p).with_context(|| format!("reading the run key {} (the service mints it when it starts)", p.display()))?;
    let s = env::egress::connect_tcp("ripples seam", &addr, Duration::from_secs(2))?;
    s.set_read_timeout(Some(Duration::from_secs(60)))?;
    let mut w = s.try_clone()?;
    w.write_all(format!("{verb} {} {args}\n", key.trim()).as_bytes())?;
    w.flush()?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    serde_json::from_str(line.trim()).with_context(|| format!("the ripples service answered `{}`", line.trim()))
}

fn print(v: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// How a finished run's outcome exits, with a line on stderr.
fn finish(v: &Value) -> Result<i32> {
    print(v)?;
    let o = v["outcome"].as_str().unwrap_or("");
    let n = v["records"].as_array().map_or(0, Vec::len);
    match o {
        "processed" | "already_processed" => {
            eprintln!(
                "cauldron ripples: {} {o}: {n} record(s){}; they read as `unreviewed` until the owner approves them at a terminal (`cauldron soup approve`).",
                v["dump"].as_str().unwrap_or("?"),
                if v["calls"].is_u64() { format!(", {} model call(s), {} input and {} output tokens", v["calls"], v["input_total"], v["usage"]["output_tokens"]) } else { String::new() }
            );
            Ok(0)
        }
        "deferred" => {
            eprintln!("cauldron ripples: deferred: {}. {}", v["reason"].as_str().unwrap_or(""), v["what_to_do"].as_str().unwrap_or(""));
            Ok(3)
        }
        _ => {
            eprintln!("cauldron ripples: failed ({}): {} What to do: {}", v["code"].as_str().unwrap_or("?"), v["message"].as_str().unwrap_or(""), v["what_to_do"].as_str().unwrap_or("read the message"));
            Ok(1)
        }
    }
}

fn run(args: &[String]) -> Result<i32> {
    let a = parse("run", args, &["--env", "--cauldron", "--model", "--backend", "--force-budget"])?;
    let (Some(env_name), Some(cauldron)) = (a.env.as_deref(), a.cauldron.as_deref()) else {
        bail!("`cauldron ripples run` needs --env and --cauldron, given as flags (`cauldron ripples help`)");
    };
    let [dump] = a.pos.as_slice() else {
        bail!("`cauldron ripples run` takes one dump id (DM-<n>)");
    };
    if let Some(b) = a.backend.as_deref().filter(|b| !matches!(*b, "threads" | "local")) {
        bail!("`--backend` is `threads` (the default) or `local`, not `{b}`");
    }
    let force_budget = match &a.force_budget {
        None => None,
        Some(n) => match n.parse::<u64>() {
            Ok(n) if n > 0 => Some(n),
            _ => bail!("`--force-budget` takes a whole number of input tokens above 0, not `{n}`"),
        },
    };
    let e = crate::mcp::environment(Some(env_name))?;
    let addr = match find(&e) {
        Ok(a) => a,
        Err(why) => bail!("REFUSED: {why}. Nothing was run. {}", start_hint(&e.name)),
    };
    let mut req = json!({"cauldron": cauldron, "dump": dump});
    if let Some(m) = &a.model {
        req["model"] = json!(m);
    }
    if let Some(b) = &a.backend {
        req["backend"] = json!(b);
    }
    if let Some(n) = force_budget {
        req["force_budget"] = json!(n);
    }
    let first = ask(&e, addr, "RUN", &req)?;
    if first["ok"] != json!(true) {
        print(&first)?;
        eprintln!("cauldron ripples: refused ({}): {}", first["code"].as_str().unwrap_or("?"), first["message"].as_str().unwrap_or(""));
        return Ok(1);
    }
    if first["done"] == json!(true) {
        return finish(&first["result"]);
    }
    let run = first["run"].as_str().unwrap_or_default().to_string();
    eprintln!("cauldron ripples: run {run} started for {dump} of `{cauldron}`; following it (`cauldron ripples status --env {} --run {run}` shows it too)", e.name);
    let mut said = 0;
    let started = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let st = match find(&e) {
            Ok(addr) => ask(&e, addr, "STATUS", &json!({"run": run}))?,
            Err(why) => bail!("the ripples service went away while the run was in progress ({why}); `cauldron ripples status --env {}` shows what the journal says", e.name),
        };
        match st["state"].as_str() {
            Some("done") => return finish(&st["result"]),
            Some("running") => {
                let steps = st["steps"].as_array().cloned().unwrap_or_default();
                for s in steps.iter().skip(said) {
                    eprintln!("cauldron ripples: {} {}", s["event"].as_str().unwrap_or(""), s["model"].as_str().unwrap_or(""));
                }
                said = said.max(steps.len());
            }
            _ => {
                print(&st)?;
                eprintln!("cauldron ripples: the run is {} after {} s; run the same command again to finish it (it lands once)", st["state"].as_str().unwrap_or("?"), started.elapsed().as_secs());
                return Ok(1);
            }
        }
    }
}

fn status(args: &[String]) -> Result<i32> {
    let a = parse("status", args, &["--env", "--cauldron", "--run"])?;
    let e = crate::mcp::environment(a.env.as_deref())?;
    let mut req = json!({});
    if let Some(c) = &a.cauldron {
        req["cauldron"] = json!(c);
    }
    if let Some(r) = &a.run {
        req["run"] = json!(r);
    }
    match find(&e) {
        Ok(addr) => {
            let v = ask(&e, addr, "STATUS", &req)?;
            print(&v)?;
            Ok(if v["ok"] == json!(true) { 0 } else { 1 })
        }
        Err(why) => {
            // Stopped: what its journal says. A run without its `done` line
            // was cut short.
            let path = e.data_root.join(SERVICE).join("runs.jsonl");
            let lines: Vec<Value> = std::fs::read_to_string(&path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
            let eval_path = e.data_root.join(SERVICE).join("eval.jsonl");
            let eval_lines: Vec<Value> = std::fs::read_to_string(&eval_path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
            let mut runs: Vec<Value> = Vec::new();
            for l in &lines {
                if a.cauldron.as_deref().is_some_and(|c| l["cauldron"] != c) || a.run.as_deref().is_some_and(|r| l["run"] != r) || l["eval"] == json!(true) {
                    continue;
                }
                if l["event"] == "started" || !runs.iter().any(|r| r["run"] == l["run"]) {
                    runs.retain(|r| r["run"] != l["run"]);
                    runs.push(json!({"run": l["run"], "cauldron": l["cauldron"], "dump": l["dump"], "started_at": l["at"], "state": "interrupted"}));
                }
                let r = runs.iter_mut().find(|r| r["run"] == l["run"]).expect("pushed above");
                r["last_event"] = l["event"].clone();
                if l["event"] == "done" {
                    r["state"] = json!("done");
                    for k in ["outcome", "records", "usage", "input_total", "calls", "models", "reason", "code", "message", "what_to_do"] {
                        if let Some(x) = l.get(k) {
                            r[k] = x.clone();
                        }
                    }
                }
            }
            let keep = runs.len().saturating_sub(10);
            runs.drain(..keep);
            print(&json!({
                "ok": true,
                "service": {"running": false, "why": why},
                "runs": runs,
                "spend": spend(&lines, a.cauldron.as_deref()),
                "evaluations_spend": spend(&eval_lines, a.cauldron.as_deref())["today"].clone(),
                "journal": path.display().to_string(),
                "evaluations_ledger": eval_path.display().to_string(),
            }))?;
            eprintln!("cauldron ripples: {why}. {}", start_hint(&e.name));
            Ok(0)
        }
    }
}

/// What the journal says was spent: today per cauldron, and per dump
/// across its runs, as the service's own `STATUS` counts it
/// (`cauldron_ripples::journal::charges`): a call is charged when it begins
/// (its `call` line, at its `estimated_input_tokens`) and settled by its
/// `called` line (same run and `call_id`; a line without one ends the newest
/// open call of its run); one with no end is `unsettled` and stays charged.
/// And calls that may still be running.
fn spend(lines: &[Value], cauldron: Option<&str>) -> Value {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let charged = |l: &Value| l["charged"].as_u64().unwrap_or_else(|| l["input_total"].as_u64().unwrap_or(0));
    // (the call's first line, tokens, settled)
    let mut calls: Vec<(Value, u64, bool)> = Vec::new();
    // (An evaluation's line in the run journal, from an earlier build, is
    // not a run's spend.)
    for l in lines.iter().filter(|l| cauldron.is_none_or(|c| l["cauldron"] == c) && l["eval"] != json!(true)) {
        match l["event"].as_str() {
            Some("call") => calls.push((l.clone(), l["estimated_input_tokens"].as_u64().unwrap_or(0), false)),
            Some("called") => {
                match calls.iter_mut().rev().find(|(b, _, settled)| !settled && b["run"] == l["run"] && (l["call_id"].is_null() || b["call_id"] == l["call_id"])) {
                    Some(c) => {
                        c.1 = charged(l);
                        c.2 = true;
                    }
                    None => calls.push((l.clone(), charged(l), true)),
                }
            }
            _ => {}
        }
    }
    let mut per_cauldron = serde_json::Map::new();
    let mut per_dump: Vec<Value> = Vec::new();
    for (l, tokens, settled) in &calls {
        let unsettled = u64::from(!settled);
        if l["at"].as_str().is_some_and(|a| a.starts_with(&today)) {
            let c = per_cauldron
                .entry(l["cauldron"].as_str().unwrap_or("?").to_string())
                .or_insert_with(|| json!({"calls": 0, "charged_input_tokens": 0, "unsettled_calls": 0, "dumps": []}));
            c["calls"] = json!(c["calls"].as_u64().unwrap_or(0) + 1);
            c["charged_input_tokens"] = json!(c["charged_input_tokens"].as_u64().unwrap_or(0) + tokens);
            c["unsettled_calls"] = json!(c["unsettled_calls"].as_u64().unwrap_or(0) + unsettled);
            if let Some(d) = c["dumps"].as_array_mut() {
                if !d.contains(&l["dump"]) {
                    d.push(l["dump"].clone());
                }
            }
        }
        let i = match per_dump.iter().position(|r| r["run"] == l["run"]) {
            Some(i) => i,
            None => {
                per_dump.push(json!({"cauldron": l["cauldron"], "dump": l["dump"], "run": l["run"], "calls": 0, "charged_input_tokens": 0, "unsettled_calls": 0}));
                per_dump.len() - 1
            }
        };
        let mut r = per_dump.remove(i);
        r["calls"] = json!(r["calls"].as_u64().unwrap_or(0) + 1);
        r["charged_input_tokens"] = json!(r["charged_input_tokens"].as_u64().unwrap_or(0) + tokens);
        r["unsettled_calls"] = json!(r["unsettled_calls"].as_u64().unwrap_or(0) + unsettled);
        r["last_at"] = l["at"].clone();
        per_dump.push(r);
    }
    for c in per_cauldron.values_mut() {
        c["dumps"] = json!(c["dumps"].as_array().map_or(0, Vec::len));
    }
    // A `sent` call with no later end, or a cancel threads did not confirm.
    let mut open: Vec<Value> = Vec::new();
    for l in lines.iter().filter(|l| cauldron.is_none_or(|c| l["cauldron"] == c)) {
        match l["event"].as_str() {
            Some("sent") => {
                open.retain(|x| x["run"] != l["run"]);
                open.push(json!({"run": l["run"], "dump": l["dump"], "thread": l["thread"]}));
            }
            Some("called") if !open.iter().any(|x| x["run"] == l["run"] && x.get("how_to_stop").is_some()) => open.retain(|x| x["run"] != l["run"]),
            Some("cancel") => {
                open.retain(|x| x["run"] != l["run"]);
                if l["confirmed"] != json!(true) {
                    open.push(json!({"run": l["run"], "dump": l["dump"], "thread": l["thread"], "how_to_stop": l["how_to_stop"]}));
                }
            }
            _ => {}
        }
    }
    per_dump.reverse();
    per_dump.truncate(20);
    json!({"today": {"date": today, "per_cauldron": per_cauldron}, "per_dump": per_dump, "possibly_running": open,
           "caps": {"calls_per_dump": 3, "input_tokens_per_dump": 200000, "input_tokens_per_cauldron_per_day": 1500000, "dumps_per_cauldron_per_day": 30,
                    "counted": "a call is charged when it begins, as one call and its estimate, and settled when it ends; a call cut short by a kill or a stop stays charged at its estimate (`unsettled_calls`)"}})
}

/// An absolute path for `p` (relative to the working directory).
fn absolute(p: &str) -> Result<std::path::PathBuf> {
    let p = std::path::PathBuf::from(p);
    Ok(if p.is_absolute() { p } else { std::env::current_dir()?.join(p) })
}

fn eval(args: &[String]) -> Result<i32> {
    let a = parse("eval", args, &["--env", "--cauldron", "--dumps", "--out", "--backends", "--local-model", "--threads-model", "--repeat", "--max-calls", "--yes"])?;
    let (Some(env_name), Some(cauldron)) = (a.env.as_deref(), a.cauldron.as_deref()) else {
        bail!("`cauldron ripples eval` needs --env and --cauldron (the cauldron's regions are offered, and its dry run judges the answers)");
    };
    let (Some(dumps), Some(out), Some(backends)) = (a.dumps.as_deref(), a.out.as_deref(), a.backends.as_deref()) else {
        bail!("`cauldron ripples eval` needs --dumps <dir>, --backends threads,local and --out <dir> (`cauldron ripples help`)");
    };
    if !a.pos.is_empty() {
        bail!("`cauldron ripples eval` takes no positional argument (got `{}`)", a.pos.join(" "));
    }
    let backends: Vec<String> = backends.split(',').map(|b| b.trim().to_string()).filter(|b| !b.is_empty()).collect();
    if backends.is_empty() || backends.iter().any(|b| !matches!(b.as_str(), "threads" | "local")) {
        bail!("`--backends` is a comma-separated list of `threads` and `local`");
    }
    let repeat = match &a.repeat {
        None => None,
        Some(n) => match n.parse::<u64>() {
            Ok(n) if (1..=5).contains(&n) => Some(n),
            _ => bail!("`--repeat` is 1 to 5, not `{n}`"),
        },
    };
    let threads = backends.iter().any(|b| b == "threads");
    let max_calls = match &a.max_calls {
        None if threads => bail!("REFUSED: `--backends` names threads, which is your Claude spend: give `--max-calls <n>` (1 to 100), the most threads calls the evaluation may make. Nothing was sent."),
        None => None,
        Some(n) => match n.parse::<u64>() {
            Ok(n) if (1..=100).contains(&n) => Some(n),
            _ => bail!("`--max-calls` is 1 to 100, not `{n}`"),
        },
    };
    let (dumps, out) = (absolute(dumps)?, absolute(out)?);
    if !dumps.is_dir() {
        bail!("REFUSED: {} is not a directory of fixture dumps", dumps.display());
    }
    if out.exists() && std::fs::read_dir(&out).map(|mut d| d.next().is_some()).unwrap_or(true) {
        bail!("REFUSED: {} is not empty; give a new directory (an evaluation never overwrites one)", out.display());
    }
    let e = crate::mcp::environment(Some(env_name))?;
    let addr = match find(&e) {
        Ok(a) => a,
        Err(why) => bail!("REFUSED: {why}. Nothing was run. {}", start_hint(&e.name)),
    };
    let mut req = json!({"cauldron": cauldron, "dumps": dumps, "out": out, "backends": backends});
    for (k, v) in [("local_model", &a.local_model), ("threads_model", &a.threads_model)] {
        if let Some(v) = v {
            req[k] = json!(v);
        }
    }
    if let Some(n) = repeat {
        req["repeat"] = json!(n);
    }
    if let Some(n) = max_calls {
        req["max_calls"] = json!(n);
    }
    if threads {
        // What it may spend, before anything is sent.
        let mut p = req.clone();
        p["plan"] = json!(true);
        let plan = ask(&e, addr, "EVAL", &p)?;
        if plan["ok"] != json!(true) {
            print(&plan)?;
            eprintln!("cauldron ripples: refused ({}): {}", plan["code"].as_str().unwrap_or("?"), plan["message"].as_str().unwrap_or(""));
            return Ok(1);
        }
        let t = &plan["plan"]["threads"];
        eprintln!(
            "cauldron ripples: threads calls: at most {} on `{}` (--max-calls {}), about {} input tokens in all, and up to {} output tokens a call. This is your Claude spend, charged in the evaluations' ledger, not against the cauldron's caps for runs.",
            t["calls_at_most"], t["model"].as_str().unwrap_or("?"), t["max_calls"], t["estimated_input_tokens_at_most"], t["output_tokens_per_call_at_most"]
        );
        if let Some(sk) = t["skipped_over_the_per_dump_cap"].as_array().filter(|x| !x.is_empty()) {
            eprintln!("cauldron ripples: {} fixture(s) over the per-dump input cap are skipped on threads.", sk.len());
        }
        if !a.yes {
            print(&plan)?;
            eprintln!("cauldron ripples: nothing was sent. Run the same command with --yes to start it.");
            return Ok(2);
        }
        req["confirmed"] = json!(true);
    }
    let first = ask(&e, addr, "EVAL", &req)?;
    if first["ok"] != json!(true) {
        print(&first)?;
        eprintln!("cauldron ripples: refused ({}): {}", first["code"].as_str().unwrap_or("?"), first["message"].as_str().unwrap_or(""));
        return Ok(1);
    }
    let id = first["run"].as_str().unwrap_or_default().to_string();
    eprintln!("cauldron ripples: evaluation {id} started; writing into {}. Ctrl-C stops following it, not the evaluation (stop the ripples service to stop it).", out.display());
    let mut said = 0;
    loop {
        std::thread::sleep(Duration::from_millis(1000));
        let lines: Vec<String> = std::fs::read_to_string(out.join("progress.jsonl")).unwrap_or_default().lines().map(str::to_string).collect();
        for l in lines.iter().skip(said) {
            eprintln!("cauldron ripples: {l}");
        }
        said = said.max(lines.len());
        let st = match find(&e) {
            Ok(addr) => ask(&e, addr, "STATUS", &json!({"run": id}))?,
            Err(why) => bail!("the ripples service went away during the evaluation ({why}); what it finished is in {}", out.display()),
        };
        match st["state"].as_str() {
            Some("running") => continue,
            Some("done") => {
                let r = &st["result"];
                print(r)?;
                return if r["outcome"] == json!("done") {
                    eprintln!("cauldron ripples: evaluation {id} done; read {}", out.join("summary.md").display());
                    Ok(0)
                } else {
                    eprintln!("cauldron ripples: evaluation {id} failed: {}", r["message"].as_str().unwrap_or(""));
                    Ok(1)
                };
            }
            _ => {
                print(&st)?;
                eprintln!("cauldron ripples: the evaluation is {}; what it finished is in {}", st["state"].as_str().unwrap_or("?"), out.display());
                return Ok(1);
            }
        }
    }
}
