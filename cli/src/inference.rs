//! **`cauldron inference`**: local models, registered by hand, and the
//! `inference` service that runs them on demand.
//!
//! `models add|list|remove` read and write the environment's registry
//! (`data/<env>/inference/models.json`, owner-only) directly: registering a
//! model needs no running service, copies nothing and downloads nothing. It
//! records the file's path, its identity (real path, device, inode, size,
//! modification and change times) and sha256, and the few header facts the
//! memory estimate uses; `verify` reads the file again. `status` and `unload` ask the
//! running service through its seam with its door key
//! (`data/<env>/inference/door.secret`), read for each call and never
//! printed; every dial goes through `env::egress::connect_tcp`.

use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use env::seams::{self, SeamState, Table};
use env::Environment;
use inference_registry::memory;
use inference_registry::{Config, Registry};
use serde_json::{json, Value};

const SERVICE: &str = "inference";

const USAGE: &str = "\
cauldron inference — local models, on demand, on this machine

    cauldron inference models add <name> <path> [--context <tokens>] [--env <env>]
        Register a .gguf model file you placed on this machine under <name> (a lower-case
        letter or digit, then up to 63 of those, `.`, `_` or `-`). Nothing is copied, moved
        or downloaded: its path, its identity (the real path with symlinks resolved,
        device, inode, size, modification and change times) and its sha256 (the whole file
        is read once) are recorded in data/<env>/inference/models.json. --context sets
        the context size it is served with (default: the settings' `context`, 65536).
        Prints the memory estimate against the budget.
    cauldron inference models list [--env <env>]
        Every registered model, with its memory estimate against the budget and whether
        its file's identity is still the one recorded (nothing is read).
    cauldron inference models verify <name> [--env <env>]
        Read the whole file again now and compare its sha256 with the one registered.
        Before every load the service compares the file's identity, and reads the whole
        file again only when that changed (or always, with `verify_every_load = true` in
        data/<env>/inference/config.toml); a file whose sha256 differs is never loaded.
    cauldron inference models remove <name> [--env <env>]
        Forget a model. The file is left where it is.
    cauldron inference status [--env <env>]
        The service (running or not), the loaded model (sha256, context, memory estimate,
        child pid, idle time and when it unloads), the llama-server it runs (path,
        version, build), crashes and the restart backoff, the settings.
    cauldron inference unload [--env <env>]
        Unload the model now (it loads again on the next request).
    cauldron inference help

  The `inference` service is off unless started (`cauldron --env <env> build inference`,
  `pin`, then `start inference`); the installer does not start it. It runs llama.cpp's
  `llama-server` (on PATH, or `llama_server = \"/path\"` in data/<env>/inference/config.toml),
  on 127.0.0.1 only, with one model at a time, loaded when first asked and unloaded after
  15 idle minutes (`idle_minutes`). A model whose file size plus KV cache plus a 2 GiB
  allowance is over the memory budget (64 GiB, `memory_budget_gib`) is refused with the
  arithmetic. It never offers a model tools. `cauldron ripples run <DM-n> --backend local`
  uses it.
";

pub(crate) fn verb_inference(args: &[String]) -> Result<i32> {
    match args.first().map(String::as_str) {
        Some("help") | Some("-h") | Some("--help") => {
            print!("{USAGE}");
            Ok(0)
        }
        Some("models") => match args.get(1).map(String::as_str) {
            Some("add") => add(&args[2..]),
            Some("list") | Some("ls") => list(&args[2..]),
            Some("remove") | Some("rm") => remove(&args[2..]),
            Some("verify") => verify(&args[2..]),
            other => {
                eprintln!("cauldron inference models: {} (add | list | remove | verify)", other.map_or("a verb is needed".to_string(), |o| format!("no verb `{o}`")));
                Ok(2)
            }
        },
        Some("status") => status(&args[1..]),
        Some("unload") => unload(&args[1..]),
        None => {
            eprint!("{USAGE}");
            Ok(2)
        }
        Some(other) => {
            eprintln!("cauldron inference: no verb `{other}` (models | status | unload | help)");
            Ok(2)
        }
    }
}

#[derive(Default)]
struct Args {
    env: Option<String>,
    context: Option<String>,
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
            bail!("unknown option `{s}` for `cauldron inference {verb}` (`cauldron inference help`)");
        }
        let mut value = || -> Result<String> {
            match &inline {
                Some(v) => Ok(v.clone()),
                None => it.next().cloned().with_context(|| format!("`cauldron inference {verb} {flag}` needs a value")),
            }
        };
        match flag {
            "--env" => a.env = Some(value()?),
            "--context" => a.context = Some(value()?),
            _ => a.pos.push(s.clone()),
        }
    }
    Ok(a)
}

fn print(v: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn estimate_json(m: &inference_registry::Model, c: &Config) -> Value {
    let e = memory::estimate(m.size_bytes, &m.facts, m.context_under(c), c.memory_budget_bytes);
    json!({"total": memory::gib(e.total_bytes), "fits": e.fits, "how": e.how})
}

/// A line every few GiB while a large file is hashed.
fn progress_lines() {
    inference_registry::report_hash_progress(|n| eprintln!("cauldron inference: … {} read", memory::gib(n)));
}

fn add(args: &[String]) -> Result<i32> {
    let a = parse("models add", args, &["--env", "--context"])?;
    let [name, path] = a.pos.as_slice() else {
        bail!("`cauldron inference models add` takes a name and the model file's path");
    };
    let e = crate::mcp::environment(a.env.as_deref())?;
    let context = match &a.context {
        None => None,
        Some(c) => Some(c.parse::<u32>().ok().with_context(|| format!("`--context` takes a whole number of tokens, not `{c}`"))?),
    };
    let path = PathBuf::from(path);
    let path = if path.is_absolute() { path } else { std::env::current_dir()?.join(path) };
    let reg = Registry::load(&e.data_root)?;
    if reg.get(name).is_some() {
        bail!("REFUSED: a model named `{name}` is already registered in `{}`; `cauldron inference models remove {name} --env {}` first", e.name, e.name);
    }
    let cfg = Config::load(&e.data_root)?;
    eprintln!("cauldron inference: reading {} for its sha256 (the whole file, once; nothing is copied)…", path.display());
    progress_lines();
    let m = Registry::describe(name, &path, context).map_err(|err| anyhow::anyhow!("REFUSED: {err:#}. Nothing was registered."))?;
    let est = estimate_json(&m, &cfg);
    // Read again and written under the registry's lock: another add or
    // remove that finished while this file was hashed is kept.
    Registry::update(&e.data_root, |r| r.add(m.clone())).map_err(|err| anyhow::anyhow!("REFUSED: {err:#}. Nothing was registered."))?;
    print(&json!({"registered": m, "memory_estimate": est, "registry": inference_registry::dir(&e.data_root).join(inference_registry::MODELS_FILE)}))?;
    if est["fits"] == json!(false) {
        eprintln!("cauldron inference: registered, but loading it would be refused: {}. Register it again with a smaller `--context`, or raise `memory_budget_gib`.", est["how"].as_str().unwrap_or(""));
    } else {
        eprintln!("cauldron inference: registered `{name}`; it loads when a request first names it.");
    }
    Ok(0)
}

fn list(args: &[String]) -> Result<i32> {
    let a = parse("models list", args, &["--env"])?;
    let e = crate::mcp::environment(a.env.as_deref())?;
    let reg = Registry::load(&e.data_root)?;
    let cfg = Config::load(&e.data_root)?;
    let models: Vec<Value> = reg
        .models
        .iter()
        .map(|m| {
            json!({"name": m.name, "path": m.path, "size": memory::gib(m.size_bytes), "sha256": m.sha256, "added_at": m.added_at,
                   "context": m.context_under(&cfg), "architecture": m.facts.architecture,
                   "unchanged": matches!(m.differences(), Ok(ref d) if d.is_empty()),
                   "identity_differs": match m.differences() { Ok(d) => json!(d), Err(e) => json!([format!("{e:#}")]) },
                   "memory_estimate": estimate_json(m, &cfg)})
        })
        .collect();
    print(&json!({"env": e.name, "models": models, "memory_budget": memory::gib(cfg.memory_budget_bytes)}))?;
    Ok(0)
}

fn verify(args: &[String]) -> Result<i32> {
    let a = parse("models verify", args, &["--env"])?;
    let [name] = a.pos.as_slice() else {
        bail!("`cauldron inference models verify` takes one model name");
    };
    let e = crate::mcp::environment(a.env.as_deref())?;
    let reg = Registry::load(&e.data_root)?;
    let Some(m) = reg.get(name) else {
        bail!("REFUSED: no model named `{name}` is registered in `{}`", e.name);
    };
    eprintln!("cauldron inference: reading {} whole for its sha256…", m.path.display());
    progress_lines();
    match m.verify(true) {
        Ok(v) => {
            Registry::record_identity(&e.data_root, &m.name, &m.sha256, &v.file)?;
            print(&json!({"model": m.name, "verified": true, "sha256": m.sha256, "file": v.file}))?;
            Ok(0)
        }
        Err(err) => {
            print(&json!({"model": m.name, "verified": false, "problem": format!("{err:#}")}))?;
            eprintln!("cauldron inference: {err:#}");
            Ok(1)
        }
    }
}

fn remove(args: &[String]) -> Result<i32> {
    let a = parse("models remove", args, &["--env"])?;
    let [name] = a.pos.as_slice() else {
        bail!("`cauldron inference models remove` takes one model name");
    };
    let e = crate::mcp::environment(a.env.as_deref())?;
    let m = Registry::update(&e.data_root, |r| r.remove(name)).map_err(|err| anyhow::anyhow!("REFUSED: {err:#}"))?;
    print(&json!({"removed": m.name, "file_left_at": m.path}))?;
    Ok(0)
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

fn ask(e: &Environment, addr: SocketAddr, verb: &str, args: &Value) -> Result<Value> {
    let p = inference_registry::dir(&e.data_root).join("door.secret");
    let key = std::fs::read_to_string(&p).with_context(|| format!("reading the door key {} (the service mints it when it starts)", p.display()))?;
    let s = env::egress::connect_tcp("inference seam", &addr, Duration::from_secs(2))?;
    s.set_read_timeout(Some(Duration::from_secs(60)))?;
    let mut w = s.try_clone()?;
    w.write_all(format!("{verb} {} {args}\n", key.trim()).as_bytes())?;
    w.flush()?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    serde_json::from_str(line.trim()).with_context(|| format!("the inference service answered `{}`", line.trim()))
}

fn start_hint(e: &str) -> String {
    format!("Start it: `cauldron --env {e} build inference`, then `cauldron --env {e} pin inference <the version it printed>`, then `cauldron --env {e} start inference`.")
}

fn status(args: &[String]) -> Result<i32> {
    let a = parse("status", args, &["--env"])?;
    let e = crate::mcp::environment(a.env.as_deref())?;
    match find(&e) {
        Ok(addr) => {
            let v = ask(&e, addr, "STATUS", &json!({}))?;
            print(&v)?;
            Ok(if v["ok"] == json!(true) { 0 } else { 1 })
        }
        Err(why) => {
            let reg = Registry::load(&e.data_root)?;
            let cfg = Config::load(&e.data_root)?;
            print(&json!({
                "ok": true,
                "service": {"running": false, "why": why},
                "registered": reg.models.iter().map(|m| m.name.clone()).collect::<Vec<_>>(),
                "settings": {"memory_budget": memory::gib(cfg.memory_budget_bytes), "idle_seconds": cfg.idle_seconds, "context": cfg.context, "llama_server": cfg.llama_server},
            }))?;
            eprintln!("cauldron inference: {why}. {}", start_hint(&e.name));
            Ok(0)
        }
    }
}

fn unload(args: &[String]) -> Result<i32> {
    let a = parse("unload", args, &["--env"])?;
    let e = crate::mcp::environment(a.env.as_deref())?;
    match find(&e) {
        Ok(addr) => {
            let v = ask(&e, addr, "UNLOAD", &json!({}))?;
            print(&v)?;
            Ok(if v["ok"] == json!(true) { 0 } else { 1 })
        }
        Err(why) => {
            eprintln!("cauldron inference: {why}; nothing is loaded.");
            Ok(0)
        }
    }
}
