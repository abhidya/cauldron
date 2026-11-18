//! **`cauldron ripples run | status | on | off`**: the dump ripple, by hand
//! or, for a cauldron whose owner turned it on, automatically.
//!
//! Nothing is processed unless the owner asks: `run` sends
//! one dump to the `ripples` service of the environment (which must be
//! running, as must `mcp` and `threads`) and follows it to its outcome;
//! `status` shows the service, the run in progress, the last runs and
//! the spend and automatic processing per cauldron; `on` (the owner's act,
//! at a terminal) turns automatic processing on for one cauldron, from now
//! on, and `off` (from anywhere) turns it off. The owner approves a ripple's records with
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
use env::ripples_auto as ra;
use serde_json::{json, Value};
use std::io::IsTerminal;

const SERVICE: &str = "ripples";

const USAGE: &str = "\
cauldron ripples — turn a dump into knowledge-graph records, by hand or automatically

    cauldron ripples run <DM-n> --env <env> --cauldron <name> [--backend threads|local] [--model <model>] [--force-budget <tokens>]
                         [--debug-keep-answer]
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
        the models) and, on a failure, what to do (`detail`: what the backend or the soup
        said, shown here and written nowhere). The journal keeps codes, counts, ids and
        fixed reasons only, never the model's answer or the dump's text.
        --debug-keep-answer keeps this run's model answers and refusals, unscrubbed, in one
        owner-only file under data/<env>/ripples/debug/, whose path is printed; for a run by
        hand only (automatic processing never keeps them). Needs the `ripples`, `mcp` and `threads`
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
    cauldron ripples on <cauldron> --env <env> [--backend threads|local] [--model <model>]
                        [--include-sessions] [--no-scrub-local]
        Turn automatic processing on for one cauldron: from this moment, each dump that lands
        in it is processed without a further command, as `run` would process it (the same
        call, caps, journal and `unreviewed` marking). Only dumps received AFTER this moment
        are taken; older ones only by hand (`run`). Text dumps only, unless
        --include-sessions (session transcripts too: their text view). It authorises
        automatic model spend and automatic writes, so it requires an interactive terminal,
        like `cauldron mcp grant`: it prints what will be sent where and the caps, asks
        [y/N], and writes the setting only on yes. The setting belongs to that cauldron,
        never to its name: `cauldron remove` and `cauldron rename` turn it off, and a
        setting left behind or copied from another cauldron or environment is void (runs
        nothing; `status` says why).
        --backend threads (the default): the scrubbed text goes through the `threads`
        service on this machine's Claude login. --backend local: to the `inference` service
        on this machine (--model: a registered model; default the only one); nothing leaves
        the machine, and the text is scrubbed too unless --no-scrub-local. The scrubber
        replaces credential-shaped text (known key formats, private keys, tokens, passwords
        and keys in assignments, on command lines, in URLs, Authorization and Cookie
        headers, long random strings) with `<redacted:kind>` in what is sent and in the
        answer; it reduces the risk and is not a guarantee, and if it cannot finish in its
        time nothing is sent. Needs the `ripples` service running (it is not started for
        you; one per environment).
    cauldron ripples off <cauldron> --env <env>
        Turn it off, from anywhere: no further model call starts for that cauldron, and a run
        in progress stops before its next call and writes nothing more. Dumps then wait
        `received`; turning it on again takes only dumps received after that moment.
    cauldron ripples help

  The `ripples` service is off unless started (`cauldron --env <env> build ripples`,
  `pin`, then `start ripples`); the installer does not start it. Automatic processing
  is off for every cauldron until its owner runs `cauldron ripples on`; an upgrade never
  turns it on. `status` shows, per cauldron, whether it is on, off or void, since when,
  the backend and the queue (waiting, running, deferred with the reason, retrying,
  failed with the reason). The journals keep codes, counts, ids and fixed reasons,
  never the dump's text or the model's answer.
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
        Some("on") => turn_on(&args[1..]),
        Some("off") => turn_off(&args[1..]),
        None => {
            eprint!("{USAGE}");
            Ok(2)
        }
        Some(other) => {
            eprintln!("cauldron ripples: no verb `{other}` (run | status | eval | on | off | help)");
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
    include_sessions: bool,
    no_scrub_local: bool,
    debug_keep_answer: bool,
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
        if matches!(flag, "--yes" | "--include-sessions" | "--no-scrub-local" | "--debug-keep-answer") {
            if inline.is_some() {
                bail!("`{flag}` takes no value");
            }
            match flag {
                "--yes" => a.yes = true,
                "--include-sessions" => a.include_sessions = true,
                "--debug-keep-answer" => a.debug_keep_answer = true,
                _ => a.no_scrub_local = true,
            }
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
    if let Some(p) = v["debug_file"].as_str() {
        eprintln!("cauldron ripples: --debug-keep-answer: the model's answers and the refusals, unscrubbed, are in {p} (owner-only); delete it when done");
    }
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
            if let Some(d) = v["detail"].as_str() {
                eprintln!("cauldron ripples: detail (shown here only, written nowhere): {d}");
            }
            Ok(1)
        }
    }
}

fn run(args: &[String]) -> Result<i32> {
    let a = parse("run", args, &["--env", "--cauldron", "--model", "--backend", "--force-budget", "--debug-keep-answer"])?;
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
    if a.debug_keep_answer {
        req["keep_answer"] = json!(true);
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
                "automatic": automatic_offline(&e, a.cauldron.as_deref()),
            }))?;
            eprintln!("cauldron ripples: {why}. {}", start_hint(&e.name));
            Ok(0)
        }
    }
}

/// Automatic processing per cauldron with a setting, read with the service
/// stopped: the setting, the automatic journal, and the dumps read from the
/// cauldron's store (`env::ripples_auto::queue`).
fn automatic_offline(e: &Environment, cauldron: Option<&str>) -> Value {
    let root = &e.data_root;
    let lines = ra::read(root);
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let hold = ra::threads_hold(&lines);
    let list: Vec<Value> = ra::all(e)
        .into_iter()
        .filter(|x| cauldron.is_none_or(|c| x.name == c))
        .map(|x| {
            let rs = if x.void.is_none() { received_dumps(e, &x.settings.cauldron) } else { None };
            // The service is not running: an open attempt was cut short.
            let mut q = ra::queue(&x, &e.name, rs.as_deref(), &ra::dump_states(&lines, &x.settings), &now, None);
            hold.mark(&mut q, &x.settings, &now);
            if x.runs() {
                q["service"] = json!("not running: nothing is processed until it is started");
            }
            q
        })
        .collect();
    json!({"threads_hold": hold.view(&now), "cauldrons": list, "off_elsewhere": "every cauldron not listed here is off", "journal": ra::journal_path(root).display().to_string()})
}

/// A cauldron's dumps still `received`, read from its store.
fn received_dumps(e: &Environment, cauldron: &str) -> Option<Vec<Value>> {
    let c = e.find_cauldron(cauldron).ok().flatten()?;
    let o = soup_door::dumps::ListOpts { state: Some("received".into()), kind: None, limit: Some(usize::MAX) };
    soup_door::dumps::list(&c, &o).ok().and_then(|d| d["dumps"].as_array().cloned())
}

/// The highest dump serial `cauldron` holds (any state, removed ones
/// included): the `since` rule's mark.
fn highest_dump(c: &env::Cauldron) -> Result<u64> {
    let o = soup_door::dumps::ListOpts { state: None, kind: None, limit: Some(usize::MAX) };
    let d = soup_door::dumps::list(c, &o)?;
    Ok(d["dumps"].as_array().into_iter().flatten().map(|x| ra::serial(x["id"].as_str().unwrap_or(""))).max().unwrap_or(0))
}

fn one(verb: &str, a: &Args) -> Result<String> {
    match a.pos.as_slice() {
        [c] => Ok(c.clone()),
        _ => bail!("`cauldron ripples {verb}` takes ONE cauldron name (got {}); `cauldron ripples help`", a.pos.len()),
    }
}

/// What turning it on means, in words: what is sent where, the caps, and
/// how to turn it off.
fn what_it_means(s: &ra::Settings, env_name: &str, service: &str) -> String {
    let kinds = if s.include_sessions {
        "each text dump (its whole text) and each session dump (only what the person typed and what the assistant wrote as text: no tool output, thinking or shell output)"
    } else {
        "each text dump (its whole text); session dumps are not taken (--include-sessions was not given) and wait for `cauldron ripples run`"
    };
    let where_to = if s.backend == "local" {
        format!(
            "goes to the `inference` service on this machine, model `{}`, as one structured call (and one retry on the same model when the answer is refused). Nothing leaves this machine. The text {} scrubbed first.",
            s.model.as_deref().unwrap_or("(the only registered model)"),
            if s.scrub_local { "is" } else { "is NOT (--no-scrub-local)" }
        )
    } else {
        format!(
            "goes through the `threads` service, on this machine's Claude login, to Anthropic as one bare completion with no tools, no MCP server and no settings, plugins, hooks or CLAUDE.md, on `{}` (one retry on `opus` when the answer is refused). The text is scrubbed first: credential-shaped text is replaced by `<redacted:kind>`. The scrubber reduces the risk and is not a guarantee. threads keeps a copy of each call's prompt and answer in its own store.",
            s.model.as_deref().unwrap_or("sonnet")
        )
    };
    format!(
        "Turning automatic processing on for cauldron `{c}` in environment `{env_name}` means, for dumps received after DM-{since}:
         
         What is sent where: {kinds}, with the cauldron's regions (id, handle, boundary; at most 50) and the package's instructions, {where_to} No existing record is sent.
         What is written: the answer's records, each scrubbed, citing the dump and marked `unreviewed` until you approve them at a terminal (`cauldron soup approve`); the dump becomes `processed` with the run and the record ids.
         Caps, charged before each call: per dump, 3 calls and 200000 input tokens across every run of it; per cauldron per UTC day, 1500000 input tokens or 30 dumps. Over a cap a dump stays `received` with `deferred: budget` and is tried again after midnight UTC.
         Failures: a failed attempt is retried after 2, then 8 minutes; after 3 failed attempts the dump is parked `failed` with the reason, and only `cauldron ripples run` runs it again.
         The `ripples` service: {service}
         Turn it off, from anywhere: cauldron ripples off {c} --env {env_name}
         See it: cauldron ripples status --env {env_name} --cauldron {c}",
        c = s.cauldron,
        since = s.since_dump,
    )
}

/// **`cauldron ripples on`**.
fn turn_on(args: &[String]) -> Result<i32> {
    let a = parse("on", args, &["--env", "--backend", "--model", "--include-sessions", "--no-scrub-local"])?;
    let cauldron = one("on", &a)?;
    let Some(env_name) = a.env.as_deref() else {
        bail!("`cauldron ripples on` needs --env, given as a flag (`cauldron ripples help`)");
    };
    let backend = a.backend.clone().unwrap_or_else(|| "threads".into());
    if !matches!(backend.as_str(), "threads" | "local") {
        bail!("`--backend` is `threads` (the default) or `local`, not `{backend}`");
    }
    if a.no_scrub_local && backend != "local" {
        bail!("`--no-scrub-local` goes with `--backend local`: what goes through threads is always scrubbed");
    }
    if let Some(m) = &a.model {
        let ok = if backend == "local" { inference_registry::name_ok(m.strip_prefix("local:").unwrap_or(m)) } else { !m.is_empty() && m.len() <= 64 && m.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')) };
        if !ok {
            bail!("`{m}` is not a model id for the {backend} backend");
        }
    }
    let e = crate::mcp::environment(Some(env_name))?;
    soup_door::grants::require_cauldron(&e, &cauldron)?;
    let c = e.find_cauldron(&cauldron)?.with_context(|| format!("no cauldron `{cauldron}` in environment `{}`", e.name))?;
    let mut cmd = format!("cauldron ripples on {cauldron} --env {}", e.name);
    if backend != "threads" {
        cmd.push_str(&format!(" --backend {backend}"));
    }
    if let Some(m) = &a.model {
        cmd.push_str(&format!(" --model {m}"));
    }
    if a.include_sessions {
        cmd.push_str(" --include-sessions");
    }
    if a.no_scrub_local {
        cmd.push_str(" --no-scrub-local");
    }
    // Checked last, so the command the refusal names is one that would work.
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        bail!(
            "REFUSED: turning automatic processing on is the owner's act at a terminal: it authorises model spend and \
             writes into the cauldron with no further command. `cauldron ripples on` requires an interactive terminal \
             (its input and output), shows what it means there and asks for a yes, and no option skips this. \
             Nothing was changed. Ask the user to run, in a terminal on this machine:\n    {cmd}"
        );
    }
    // The cauldron's id, given now if it has none yet: the setting is bound
    // to it.
    soup_door::grants::adopt(&e, &c)?;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let before = ra::effective(&e, &cauldron)?;
    // Already on for this very cauldron: the same `since`, so nothing older
    // is taken; else (off, or a void setting of a cauldron that is gone)
    // from now.
    let (since_at, since_dump, kept) = match &before {
        Some(b) if b.runs() => (b.settings.since_at.clone(), b.settings.since_dump, true),
        _ => (now.clone(), highest_dump(&c)?, false),
    };
    let s = ra::Settings {
        cauldron: cauldron.clone(),
        on: true,
        since_at,
        since_dump,
        backend,
        model: a.model.clone(),
        include_sessions: a.include_sessions,
        scrub_local: !a.no_scrub_local,
        changed_at: now,
        off_at: None,
        bound_to: Some(ra::Binding::of(&e, &c)),
    };
    let service = match find(&e) {
        Ok(_) => "running; it takes dumps from the moment you say yes.".to_string(),
        Err(why) => format!("{why}. Nothing is processed until it runs: {}", start_hint(&e.name)),
    };
    let granted = soup_door::grants::Grants::load(&e).ok().and_then(|g| g.get(&cauldron).map(|x| x.scopes.len() >= 3)).unwrap_or(false);
    let mut text = what_it_means(&s, &e.name, &service);
    if !granted {
        text.push_str(&format!("\nThe cauldron must be granted to the MCP door with read, write and dump for this to run: `cauldron mcp grant {cauldron} --env {}`.", e.name));
    }
    // What it means first; then the owner's yes, at the terminal; only then
    // is anything written.
    println!("{text}\n");
    print!("Turn automatic processing on for cauldron `{cauldron}`? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        println!("cauldron ripples: not turned on; nothing was changed.");
        return Ok(1);
    }
    // Already on for this cauldron, and the setting not changed meanwhile:
    // the same `since` (read again, after the wait at the prompt).
    let s = match ra::effective(&e, &cauldron)? {
        Some(b) if b.runs() && kept => ra::Settings { since_at: b.settings.since_at, since_dump: b.settings.since_dump, ..s },
        _ => s,
    };
    ra::store(&e.data_root, &s)?;
    print(&json!({
        "outcome": if kept { "updated" } else { "on" },
        "env": e.name,
        "setting": s,
        "store": ra::path(&e.data_root, &cauldron).display().to_string(),
    }))?;
    println!(
        "Automatic processing is ON for cauldron `{cauldron}` in environment `{}`, for dumps received after DM-{} (from {}). Turn it off, from anywhere: cauldron ripples off {cauldron} --env {}",
        e.name, s.since_dump, s.since_at, e.name
    );
    Ok(0)
}

/// **`cauldron ripples off`**: from anywhere; takes effect before the next
/// model call.
fn turn_off(args: &[String]) -> Result<i32> {
    let a = parse("off", args, &["--env"])?;
    let cauldron = one("off", &a)?;
    let Some(env_name) = a.env.as_deref() else {
        bail!("`cauldron ripples off` needs --env, given as a flag (`cauldron ripples help`)");
    };
    let e = crate::mcp::environment(Some(env_name))?;
    if !ra::name_ok(&cauldron) {
        bail!("REFUSED: `{cauldron}` is not a usable cauldron name");
    }
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let doc = match ra::turn_off(&e.data_root, &cauldron, &now)? {
        Some(_) => {
            let s = ra::load(&e.data_root, &cauldron)?;
            json!({"outcome": "off", "env": e.name, "cauldron": cauldron, "setting": s,
                   "note": format!("automatic processing is off for cauldron `{cauldron}`: no further model call starts, a run in progress stops before its next call, and dumps wait `received` (`cauldron ripples run` still processes one by hand)")})
        }
        _ => json!({"outcome": "already_off", "env": e.name, "cauldron": cauldron, "note": format!("automatic processing was not on for cauldron `{cauldron}`; nothing changed")}),
    };
    print(&doc)?;
    Ok(0)
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
                let open = calls.iter().rposition(|(b, _, settled)| !settled && b["run"] == l["run"] && (l["call_id"].is_null() || b["call_id"] == l["call_id"]));
                match open {
                    // A call threads held before it ran (`held`) never was one.
                    Some(i) if l["held"] == json!(true) => {
                        calls.remove(i);
                    }
                    Some(i) => {
                        calls[i].1 = charged(l);
                        calls[i].2 = true;
                    }
                    None if l["held"] == json!(true) => {}
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
