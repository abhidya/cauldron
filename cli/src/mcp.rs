//! **`cauldron mcp grant | revoke | grants | status | stdio | config`**: which
//! cauldrons the owner has opened to the MCP door, the `mcp` service that
//! serves them, and the stdio bridge Claude Code launches
//! (`crate::mcp_bridge`).
//!
//! Every cauldron is closed to the MCP door until it is granted. A grant is
//! the owner's act: `grant` runs when its standard input and standard output
//! are both an interactive terminal, or without one only where the owner
//! opened `grant` to agents (`env::owner_acts`, through
//! `crate::owner_acts::act`), so an agent (whose shell has no terminal) that
//! reads a refusal naming the command cannot simply run it unless the owner
//! said it may. Revoking and listing
//! need no terminal. The store and its rules are `soup_door::grants`.
//!
//! The environment is the usual one: `--env` (or a leading `cauldron
//! --env`), else `$CAULDRON_ENV` / `$ENV`, else `prod`; every answer names
//! it. The stdio bridge is the exception: it does not read `$ENV`
//! (`crate::mcp_bridge`).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use env::Environment;
use serde_json::json;
use soup_door::grants::{self, Grants, Revoked, Scope};

const USAGE: &str = "\
cauldron mcp — which cauldrons are open to the MCP door

    cauldron mcp grant <cauldron> [--env <env>] [--scopes read,write,dump] [--project <dir>]...
        Open the cauldron to the MCP door for those scopes (default: all three): every
        Claude Code session this user runs on this machine against the environment can
        then reach it by naming it. Each --project only tells a session working in that
        directory (or below it) that this is the cauldron it means when it names none;
        --project does not limit which sessions can reach it. A cauldron that is already
        granted takes the new scopes and keeps its projects, adding these.
        Refused for a cauldron that does not exist or has no soup. A grant is the owner's
        act: it runs with its input and output on an interactive terminal, or without one
        only where the owner opened `grant` to agents (`cauldron owner-acts help`), and is
        refused (naming the command to run) anywhere else. The grant row and the answer
        say who granted (`by`: `tty` or `agent`).
    cauldron mcp revoke <cauldron> [--env <env>] [--project <dir>]
        Close the cauldron to the MCP door; with --project, only unbind that directory.
    cauldron mcp grants [--env <env>]
        Every grant of the environment, as JSON. A grant that names its cauldron only by
        name (written by a pre-release build, before a grant recorded which cauldron it
        was made for) is refused on its first call; `warnings` lists each, with the
        command that grants it again (`regrant`), and a line on stderr says the same.
    cauldron mcp status [--env <env>]
        The `mcp` service (live or not, its pid, its door and its version beside this
        cauldron's), the cauldrons it holds open for writing and what opening each
        recovered, the dumps stores it holds (`open_dumps`), and per cauldron its grant,
        what its writer lock says and its dumps store (`dumps`: held by the service or
        not, and how many dumps by state; never their text). When the
        running service is another version than this cauldron (an upgrade left the mcp pin
        on its old build), `version_skew` and `upgrade` say so and give the two commands
        that move it, and a line on stderr says the same. A cauldron whose grant names it
        only by name has `grant_note` and `regrant` (the command that grants it again),
        and a line on stderr says the same: every call to it is refused until then.
        JSON; writes nothing.
    cauldron mcp stdio [--env <env>] [--cauldron <name>]
        The bridge Claude Code launches (the plugin's .mcp.json names it): JSON-RPC, one
        message per line, on stdin and stdout, relayed to the service with the door secret.
        It adds the session's context to every tool call (working directory, the session
        id when Claude Code passes CLAUDE_CODE_SESSION_ID, and --cauldron or
        $CAULDRON_CAULDRON), replacing any `_ctx` a caller put in the arguments. With
        --cauldron it is pinned to that cauldron: a call whose `cauldron` argument names
        another is refused as `cauldron_pinned` and never relayed ($CAULDRON_CAULDRON
        only chooses a default). With the
        service down it still answers `initialize` and `tools/list`, and every tool call
        is refused as `service_down`, naming the command that starts it. Its environment
        is --env, else $CAULDRON_ENV, else prod: it does not read $ENV. When that
        environment cannot be used it still answers `initialize` and `tools/list`, and
        every tool call is refused as `no_environment`, saying why. When the running
        service is another version than this cauldron, its `initialize` instructions and
        a line on stderr say so with the commands that upgrade it, and a refusal of a tool
        or an argument the service does not know says the same.
    cauldron mcp config --cauldron <name> [--env <env>]
        The `.mcp.json` entry of an MCP server for that one cauldron, on stdout:
        `cauldron-<name>`, running this cauldron by its absolute path as
        `mcp stdio --env <env> --cauldron <name>`. Put it in a project's .mcp.json. A server
        started with --cauldron is pinned: every call reaches that cauldron, and a call
        naming another is refused as `cauldron_pinned`. Writes and grants nothing; the
        environment is --env, else $CAULDRON_ENV, else prod (not $ENV). The entry has no
        `env` block: it never carries CAULDRON_OWNER_ACTS (or any variable), whatever this
        shell has. Who may act for the owner is chosen with `cauldron owner-acts`, never by
        a project's .mcp.json, and the MCP review tools follow that choice, not a variable
        in a session's server.
    cauldron mcp help

  Every cauldron is closed until granted. Grants live in <root>/data/<env>/mcp/grants.json
  (owner-only). The environment is --env, else $CAULDRON_ENV, else $ENV, else prod
  (`stdio` skips $ENV). A project marker
  (`cauldron link`) never opens anything to the MCP door. The service is started like any
  other: `cauldron --env <env> start mcp`. While it holds a cauldron, `cauldron soup
  import` is sent to it and `cauldron soup recover` is refused.
";

/// `cauldron mcp <verb> …`.
pub(crate) fn verb_mcp(args: &[String]) -> Result<i32> {
    match args.first().map(String::as_str) {
        Some("help") | Some("-h") | Some("--help") => {
            print!("{USAGE}");
            Ok(0)
        }
        Some("grant") => grant(&args[1..]),
        Some("revoke") => revoke(&args[1..]),
        Some("grants") => list(&args[1..]),
        Some("status") => status(&args[1..]),
        Some("stdio") => crate::mcp_bridge::stdio(&args[1..]),
        Some("config") => config(&args[1..]),
        None => {
            eprint!("{USAGE}");
            Ok(2)
        }
        Some(other) => {
            eprintln!("cauldron mcp: no verb `{other}` (grant | revoke | grants | status | stdio | config | help)");
            Ok(2)
        }
    }
}

#[derive(Default)]
pub(crate) struct Args {
    pub(crate) env: Option<String>,
    pub(crate) cauldron: Option<String>,
    scopes: Option<String>,
    projects: Vec<PathBuf>,
    pub(crate) pos: Vec<String>,
}

pub(crate) fn parse(verb: &str, args: &[String], allow: &[&str]) -> Result<Args> {
    let mut a = Args::default();
    let mut it = args.iter();
    while let Some(s) = it.next() {
        let (flag, inline) = match s.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (s.as_str(), None),
        };
        if flag.starts_with('-') && !allow.contains(&flag) {
            bail!("unknown option `{s}` for `cauldron mcp {verb}` (`cauldron mcp help`)");
        }
        let mut value = || -> Result<String> {
            match &inline {
                Some(v) => Ok(v.clone()),
                None => it.next().cloned().with_context(|| format!("`cauldron mcp {verb} {flag}` needs a value")),
            }
        };
        match flag {
            "--env" => a.env = Some(value()?),
            "--scopes" => a.scopes = Some(value()?),
            "--cauldron" => a.cauldron = Some(value()?),
            "--project" => a.projects.push(PathBuf::from(value()?)),
            _ => a.pos.push(s.clone()),
        }
    }
    Ok(a)
}

/// The environment (`--env`, else the process's), which must exist.
pub(crate) fn environment(flag: Option<&str>) -> Result<Environment> {
    let e = Environment::resolve()?;
    let e = match flag {
        Some(n) => e.for_name(n)?,
        None => e,
    };
    if !e.data_root.is_dir() {
        bail!("REFUSED: there is no environment `{}` ({} does not exist)", e.name, e.data_root.display());
    }
    Ok(e)
}

fn one_cauldron<'a>(verb: &str, a: &'a Args) -> Result<&'a str> {
    match a.pos.as_slice() {
        [c] => Ok(c),
        _ => bail!("`cauldron mcp {verb}` takes ONE cauldron name (got {}); `cauldron mcp help`", a.pos.len()),
    }
}

// -------------------------------------------------------------------- grant

fn grant(args: &[String]) -> Result<i32> {
    let a = parse("grant", args, &["--env", "--scopes", "--project"])?;
    let cauldron = one_cauldron("grant", &a)?;
    let e = environment(a.env.as_deref())?;
    let scopes = match &a.scopes {
        Some(s) => Scope::parse_list(s)?,
        None => Scope::ALL.to_vec(),
    };
    let projects = a.projects.iter().map(|p| grants::canonical_project(p)).collect::<soup_door::Result<Vec<_>>>()?;
    grants::require_cauldron(&e, cauldron)?;
    // Checked last, so the command the refusal names is one that would work.
    let allowed = crate::owner_acts::act(
        env::owner_acts::Act::Grant,
        &e,
        cauldron,
        &format!(
            "REFUSED: a grant is the owner's act at a terminal. `cauldron mcp grant` runs only with its input and \
             output on an interactive terminal, unless the owner has let agents grant (owner_acts), so an agent or a \
             script cannot open a cauldron to the MCP door by itself. Nothing was granted. Ask the user to run, in a \
             terminal on this machine:\n    {}",
            grants::grant_command(&e.name, cauldron, &scopes, &projects)
        ),
    )?;
    let (g, created) = grants::grant(&e, cauldron, &scopes, &projects, allowed.by().as_str())?;
    let defaults = if g.projects.is_empty() {
        "No --project was given, so a session reaches it only by naming it.".to_string()
    } else {
        "Sessions working in its projects use it when they name no cauldron; --project does not limit which sessions \
         can reach it."
            .to_string()
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "outcome": if created { "granted" } else { "updated" },
            "by": allowed.by().as_str(),
            "env": e.name,
            "grant": g,
            "store": grants::store_path(&e).display().to_string(),
            "note": format!(
                "cauldron `{cauldron}` is open to the MCP door for {}: every Claude Code session this user runs on this \
                 machine against environment `{}` can reach it by naming it. {defaults} `cauldron mcp revoke {cauldron} \
                 --env {}` closes it.",
                grants::scopes_text(&g.scopes),
                e.name,
                e.name
            ),
        }))?
    );
    Ok(0)
}

// ------------------------------------------------------------------- config

/// The name of the MCP server `cauldron mcp config` writes for `cauldron`.
pub(crate) fn server_name(cauldron: &str) -> String {
    format!("cauldron-{cauldron}")
}

/// `cauldron mcp config --cauldron <name> [--env <env>]`: the `.mcp.json`
/// entry of a server pinned to that one cauldron, on stdout and nothing
/// else (so it can be written to a file as it is); notes go to stderr. The
/// command is this `cauldron`'s absolute path, so the server starts whatever
/// PATH Claude Code has. The environment is the bridge's rule (`--env`, else
/// `$CAULDRON_ENV`, else `prod`; never `$ENV`), and the entry names it
/// outright. Writes nothing, and grants nothing: an ungranted cauldron's
/// note names the owner's `cauldron mcp grant` command.
fn config(args: &[String]) -> Result<i32> {
    let a = parse("config", args, &["--env", "--cauldron"])?;
    if let Some(p) = a.pos.first() {
        bail!("`cauldron mcp config` takes no arguments (got `{p}`); name the cauldron with --cauldron <name>");
    }
    let Some(cauldron) = a.cauldron.as_deref().map(str::trim).filter(|c| !c.is_empty()) else {
        bail!("`cauldron mcp config` needs --cauldron <name>: the one cauldron the server is pinned to");
    };
    let name = a
        .env
        .clone()
        .or_else(|| std::env::var("CAULDRON_ENV").ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "prod".to_string());
    let e = environment(Some(&name))?;
    if e.find_cauldron(cauldron)?.is_none() {
        bail!("REFUSED: environment `{}` has no cauldron `{cauldron}` (`cauldron --env {} list`). Nothing was printed.", e.name, e.name);
    }
    let me = std::env::current_exe().context("finding this cauldron's own path")?;
    if !me.is_absolute() {
        bail!("this cauldron's own path is not absolute ({}); run it by its absolute path", me.display());
    }
    // The entry is the command and its arguments, and never an `env` block:
    // a project's `.mcp.json` is shared (often committed), and the owner's
    // choice of who may act for them is made with `cauldron owner-acts`,
    // never carried in by a variable here (the `mcp` service, which decides
    // the review tools, would not read it from a session's server anyway).
    let doc = json!({"mcpServers": {server_name(cauldron): {
        "command": me.display().to_string(),
        "args": ["mcp", "stdio", "--env", e.name, "--cauldron", cauldron],
    }}});
    println!("{}", serde_json::to_string_pretty(&doc)?);
    eprintln!(
        "cauldron mcp config: the `{}` server above serves cauldron `{cauldron}` of environment `{}` only: a call naming \
         another cauldron is refused (`cauldron_pinned`). Put it in the project's .mcp.json (merge it into `mcpServers` \
         if the file has one already).",
        server_name(cauldron),
        e.name
    );
    match Grants::load(&e)?.get(cauldron) {
        Some(_) => {}
        None => eprintln!(
            "cauldron mcp config: `{cauldron}` is not granted yet, so every call will be refused as `not_granted` until \
             you run, at a terminal:\n    {}",
            grants::grant_command(&e.name, cauldron, &Scope::ALL, &[])
        ),
    }
    Ok(0)
}

// ------------------------------------------------------------------- revoke

fn revoke(args: &[String]) -> Result<i32> {
    let a = parse("revoke", args, &["--env", "--project"])?;
    let cauldron = one_cauldron("revoke", &a)?;
    if a.projects.len() > 1 {
        bail!("`cauldron mcp revoke` takes at most one --project");
    }
    if !soup_door::marker::name_ok(cauldron) {
        bail!("REFUSED: `{cauldron}` is not a usable cauldron name ([a-z0-9_-], 1-32 characters, starting with [a-z0-9])");
    }
    let e = environment(a.env.as_deref())?;
    let project = a.projects.first().map(PathBuf::as_path);
    let (doc, rc) = match grants::revoke(&e, cauldron, project)? {
        Revoked::Grant(g) => (json!({"outcome": "revoked", "env": e.name, "cauldron": cauldron, "revoked": g}), 0),
        Revoked::Project { project, grant } => (
            json!({"outcome": "unbound", "env": e.name, "cauldron": cauldron, "project": project, "grant": grant}),
            0,
        ),
        Revoked::NotGranted => (
            json!({"outcome": "not-granted", "env": e.name, "cauldron": cauldron,
                   "note": format!("cauldron `{cauldron}` had no grant in environment `{}`; nothing changed", e.name)}),
            0,
        ),
        Revoked::NoSuchProject(g) => (
            json!({"outcome": "no-such-project", "env": e.name, "cauldron": cauldron, "grant": g,
                   "note": format!("{} is not one of the grant's projects; nothing changed", project.map(Path::display).map(|d| d.to_string()).unwrap_or_default())}),
            1,
        ),
    };
    println!("{}", serde_json::to_string_pretty(&doc)?);
    Ok(rc)
}

// ------------------------------------------------------------------- grants

fn list(args: &[String]) -> Result<i32> {
    let a = parse("grants", args, &["--env"])?;
    if let Some(p) = a.pos.first() {
        bail!("`cauldron mcp grants` takes no arguments (got `{p}`)");
    }
    let e = environment(a.env.as_deref())?;
    let g = Grants::load(&e)?;
    let mut doc = json!({
        "env": e.name,
        "store": grants::store_path(&e).display().to_string(),
        "grants": g.grants,
    });
    let warnings = grants::name_only(&e, &g);
    for w in &warnings {
        eprintln!("cauldron: WARNING: {}", w["note"].as_str().unwrap_or_default());
    }
    if !warnings.is_empty() {
        doc["warnings"] = json!(warnings);
    }
    println!("{}", serde_json::to_string_pretty(&doc)?);
    Ok(0)
}

// ------------------------------------------------------------------- status

/// A cauldron's dumps store for `mcp status`: whether the service holds it
/// (`held` names what it holds; `None`: it cannot say), and how many dumps
/// it keeps by state, read without writing. Never a dump's text.
fn dumps_status(c: &env::Cauldron, held: Option<&[String]>) -> serde_json::Value {
    let mut d = match soup_door::dumps::counts(c) {
        Ok(v) => v,
        Err(err) => json!({"store": "unreadable", "error": err.to_string()}),
    };
    d["held_by_service"] = match held {
        Some(h) => json!(h.contains(&c.name)),
        None => serde_json::Value::Null,
    };
    d
}

/// **`cauldron mcp status`**: the service, what it holds open, and per
/// cauldron its grant, its writer lock and its dumps store. Reads only: the
/// seam table, the service's seam (`OPEN`, `DUMPS`), the dumps stores'
/// counts through connections that cannot write, its version (its `initialize` answer), the
/// grants and each lock file's text. It never
/// takes a writer lock, so it cannot get in an import's way.
fn status(args: &[String]) -> Result<i32> {
    let a = parse("status", args, &["--env"])?;
    if let Some(p) = a.pos.first() {
        bail!("`cauldron mcp status` takes no arguments (got `{p}`)");
    }
    let e = environment(a.env.as_deref())?;
    let (service, open, open_dumps) = match crate::mcp_door::find(&e) {
        Ok(f) => {
            let open = crate::mcp_door::seam_ask(&f.seam, "OPEN")
                .ok()
                .and_then(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
                .unwrap_or_else(|| json!([]));
            // A service from before `DUMPS` cannot say: null, not "none".
            let open_dumps = crate::mcp_door::seam_ask(&f.seam, "DUMPS")
                .ok()
                .and_then(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
                .filter(serde_json::Value::is_array)
                .unwrap_or(serde_json::Value::Null);
            let mut svc = json!({"state": "live", "pid": f.pid, "seam": f.seam, "door": format!("http://{}{}", f.door, crate::mcp_door::PATH),
                "cauldron_version": crate::mcp_door::CLI_VERSION});
            // Its version against this cauldron's: an upgrade that left the
            // environment's `mcp` pin on the old build shows here.
            match crate::mcp_door::service_version(&e, f.door) {
                Some(sv) => {
                    if let Some(note) = crate::mcp_door::skew_note(&sv, &e.name) {
                        eprintln!("cauldron: VERSION SKEW: {note}.");
                        svc["version_skew"] = json!(note);
                        svc["upgrade"] = json!(crate::mcp_door::upgrade_commands(&e.name));
                    }
                    svc["version"] = json!(sv);
                }
                None => svc["version"] = json!(null),
            }
            (svc, open, open_dumps)
        }
        Err(why) => (
            json!({"state": "not running", "detail": why.say(), "start": crate::mcp_door::start_command(&e.name)}),
            json!([]),
            json!([]),
        ),
    };
    let dumps_held: Option<Vec<String>> = open_dumps
        .as_array()
        .map(|a| a.iter().filter_map(|o| o["cauldron"].as_str().map(str::to_string)).collect());
    let held: Vec<String> = open.as_array().into_iter().flatten().filter_map(|o| o["cauldron"].as_str().map(str::to_string)).collect();
    let g = Grants::load(&e)?;
    let name_only = grants::name_only(&e, &g);
    for w in &name_only {
        eprintln!("cauldron: WARNING: {}", w["note"].as_str().unwrap_or_default());
    }
    let mut cauldrons = Vec::new();
    for c in e.cauldrons()? {
        if !c.soup().join(spine::spine_db::SPINE_FILE).exists() {
            continue;
        }
        let grant = g.get(&c.name);
        let mut row = json!({
            "cauldron": c.name,
            "granted": grant.map(|gr| gr.scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>()),
            "projects": grant.map(|gr| gr.projects.clone()).unwrap_or_default(),
            "held_by_service": held.contains(&c.name),
            "writer_lock_says": soup_door::lock::lock_says(&c),
            "dumps_lock_says": soup_door::dumps::lock_says(&c),
            "dumps": dumps_status(&c, dumps_held.as_deref()),
            // The regions a writer holds now: a write into one is refused
            // `region_locked` until it is let go or expires.
            "region_locks": env::region_locks::view_of(&c),
            // Who may act for the owner on it without a terminal.
            "owner_acts": crate::owner_acts::of_cauldron(&e, &c),
        });
        if let Some(w) = crate::review::waiting(&e, &c) {
            row["unreviewed"] = w["unreviewed"].clone();
            if let Some(r) = w.get("review") {
                row["review"] = r.clone();
            }
        }
        if let Some(w) = name_only.iter().find(|w| w["cauldron"] == c.name.as_str()) {
            row["grant_note"] = w["note"].clone();
            row["regrant"] = w["regrant"].clone();
        }
        cauldrons.push(row);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "env": e.name,
            "service": service,
            "open": open,
            "open_dumps": open_dumps,
            "owner_acts": crate::owner_acts::summary(&e),
            "cauldrons": cauldrons,
            "note": "`writer_lock_says` is the text the last holder wrote into <soup>/writer.lock (and `dumps_lock_says` \
                     into <cauldron>/dumps/writer.lock); the lock itself, not the text, says who holds it now, and \
                     `held_by_service` is the service's own answer for the soup (`dumps.held_by_service` for the dumps \
                     store; null when the running service is too old to say)",
        }))?
    );
    Ok(0)
}
