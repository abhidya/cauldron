//! **`cauldron mcp grant | revoke | grants | status | stdio`**: which
//! cauldrons the owner has opened to the MCP door, the `mcp` service that
//! serves them, and the stdio bridge Claude Code launches
//! (`crate::mcp_bridge`).
//!
//! Every cauldron is closed to the MCP door until it is granted. A grant is
//! the owner's act at a terminal: `grant` runs only when its standard input
//! and standard output are both an interactive terminal, and there is no
//! flag that skips that, so an agent (whose shell has no terminal) that reads
//! a refusal naming the command cannot simply run it. Revoking and listing
//! need no terminal. The store and its rules are `soup_door::grants`.
//!
//! The environment is the usual one: `--env` (or a leading `cauldron
//! --env`), else `$CAULDRON_ENV` / `$ENV`, else `prod`; every answer names
//! it. The stdio bridge is the exception: it does not read `$ENV`
//! (`crate::mcp_bridge`).

use std::io::IsTerminal;
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
        act at a terminal: it runs only with its input and output on an interactive
        terminal, and is refused (naming the command to run) anywhere else.
    cauldron mcp revoke <cauldron> [--env <env>] [--project <dir>]
        Close the cauldron to the MCP door; with --project, only unbind that directory.
    cauldron mcp grants [--env <env>]
        Every grant of the environment, as JSON.
    cauldron mcp status [--env <env>]
        The `mcp` service (live or not, its pid and its door), the cauldrons it holds open
        for writing and what opening each recovered, and per cauldron its grant and what
        its writer lock says. JSON; writes nothing.
    cauldron mcp stdio [--env <env>] [--cauldron <name>]
        The bridge Claude Code launches (the plugin's .mcp.json names it): JSON-RPC, one
        message per line, on stdin and stdout, relayed to the service with the door secret.
        It adds the session's context to every tool call (working directory, the session
        id when Claude Code passes CLAUDE_CODE_SESSION_ID, and --cauldron or
        $CAULDRON_CAULDRON), replacing any `_ctx` a caller put in the arguments. With the
        service down it still answers `initialize` and `tools/list`, and every tool call
        is refused as `service_down`, naming the command that starts it. Its environment
        is --env, else $CAULDRON_ENV, else prod: it does not read $ENV. When that
        environment cannot be used it still answers `initialize` and `tools/list`, and
        every tool call is refused as `no_environment`, saying why.
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
        None => {
            eprint!("{USAGE}");
            Ok(2)
        }
        Some(other) => {
            eprintln!("cauldron mcp: no verb `{other}` (grant | revoke | grants | status | stdio | help)");
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
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        bail!(
            "REFUSED: a grant is the owner's act at a terminal. `cauldron mcp grant` runs only with its input and \
             output on an interactive terminal, so an agent or a script cannot open a cauldron to the MCP door by \
             itself, and no option skips this. Nothing was granted. Ask the user to run, in a terminal on this \
             machine:\n    {}",
            grants::grant_command(&e.name, cauldron, &scopes, &projects)
        );
    }
    let (g, created) = grants::grant(&e, cauldron, &scopes, &projects, "tty")?;
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
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "env": e.name,
            "store": grants::store_path(&e).display().to_string(),
            "grants": g.grants,
        }))?
    );
    Ok(0)
}

// ------------------------------------------------------------------- status

/// **`cauldron mcp status`**: the service, what it holds open, and per
/// cauldron its grant and its writer lock. Reads only: the seam table, the
/// service's seam (`OPEN`), the grants and each lock file's text. It never
/// takes a writer lock, so it cannot get in an import's way.
fn status(args: &[String]) -> Result<i32> {
    let a = parse("status", args, &["--env"])?;
    if let Some(p) = a.pos.first() {
        bail!("`cauldron mcp status` takes no arguments (got `{p}`)");
    }
    let e = environment(a.env.as_deref())?;
    let (service, open) = match crate::mcp_door::find(&e) {
        Ok(f) => {
            let open = crate::mcp_door::seam_ask(&f.seam, "OPEN")
                .ok()
                .and_then(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
                .unwrap_or_else(|| json!([]));
            (
                json!({"state": "live", "pid": f.pid, "seam": f.seam, "door": format!("http://{}{}", f.door, crate::mcp_door::PATH)}),
                open,
            )
        }
        Err(why) => (
            json!({"state": "not running", "detail": why.say(), "start": crate::mcp_door::start_command(&e.name)}),
            json!([]),
        ),
    };
    let held: Vec<String> = open.as_array().into_iter().flatten().filter_map(|o| o["cauldron"].as_str().map(str::to_string)).collect();
    let g = Grants::load(&e)?;
    let mut cauldrons = Vec::new();
    for c in e.cauldrons()? {
        if !c.soup().join(spine::spine_db::SPINE_FILE).exists() {
            continue;
        }
        let grant = g.get(&c.name);
        cauldrons.push(json!({
            "cauldron": c.name,
            "granted": grant.map(|gr| gr.scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>()),
            "projects": grant.map(|gr| gr.projects.clone()).unwrap_or_default(),
            "held_by_service": held.contains(&c.name),
            "writer_lock_says": soup_door::lock::lock_says(&c),
            "dumps_lock_says": soup_door::dumps::lock_says(&c),
        }));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "env": e.name,
            "service": service,
            "open": open,
            "cauldrons": cauldrons,
            "note": "`writer_lock_says` is the text the last holder wrote into <soup>/writer.lock (and `dumps_lock_says` \
                     into <cauldron>/dumps/writer.lock); the lock itself, not the text, says who holds it now, and \
                     `held_by_service` is the service's own answer for the soup",
        }))?
    );
    Ok(0)
}
