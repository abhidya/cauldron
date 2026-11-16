//! **`cauldron soup import | regions | region | nodes | search | get`** — a
//! cauldron's soup, offline, for an agent that is not the soup daemon.
//!
//! - `import` lands records written in the PUBLISHED shape
//!   (`schema/soup/workspace-v6/`) as ONE write, all or nothing, after the
//!   spine's own door has judged every one (`spine::migrate::records`).
//! - `regions`, `region`, `nodes`, `search` and `get` read the soup's pool
//!   through a connection that cannot write (and `nodes` the spine's commit
//!   order the same way), so an agent can see where records belong and what
//!   the soup already says. Every read is JSON, bounded by a `--limit` with
//!   a default, and takes `--max-chars`; what it cut it lists.
//!
//! # `import --dry-run` while a soup daemon runs
//!
//! This release runs no soup daemon, but a real import still refuses when
//! the environment's seam table shows a live one, because it opens the
//! store directly and a daemon may hold it. A dry
//! run is allowed, because it never touches the live store with anything
//! but the read-only connection `get` already uses: each `*.db` file is
//! copied with `VACUUM INTO` from that connection into a private temporary
//! directory (`spine::offline::snapshot_copy`), and the spine's writing open
//! and the door's validate mode run on the COPY. The answer is as of that
//! snapshot.
//!
//! # How `import` opens the store, and why that is safe
//!
//! The spine's only production open is `spine::Cauldron::open`, which runs
//! the recovery pass and the tranche pass before it returns, and those
//! WRITE (roll forward, abort, sweep orphan locks, reset tranche deadlines).
//! That is why the other verbs never open a soup that way
//! (`cauldrons.rs`). `import` has to write — the one write door is reached
//! only through a handle from that open — so it opens only a soup that is
//! (1) held by no running soup daemon and (2) AT REST, so those passes have
//! nothing to do and the import's commit is the only write:
//!
//! 1. the environment's seam table is read; a `soup` row whose process is
//!    alive refuses, and a table that cannot be read refuses (no proof);
//!    then the soup's writer lock (`<soup>/writer.lock`) is taken exclusive
//!    and held to the end, so no second import or `recover` writes beside
//!    it;
//! 2. `spine::offline::rest_check` reads `spine.db` read-only: not halted,
//!    the current spine schema, no un-cleared transaction row, no lock row,
//!    no live tranche, no quiesce, every pool with its seal column. Any
//!    reason refuses, naming it; an unreadable spine refuses;
//! 3. only then `Cauldron::open`, and its own report is checked: a pass
//!    that did anything after all (a writer that slipped in between) is
//!    reported and the import refuses before it submits.
//!
//! The import is then dry-run through the door's VALIDATE mode (no row, no
//! lock) and, unless `--dry-run`, landed as one statement under one spine
//! row. The spine's locks are tables in `spine.db`, arbitrated across
//! processes, so a daemon that starts in the middle meets an ordinary
//! concurrent writer: the worst case is a restart of the import's
//! attempt, never a half-landed batch.
//!
//! # An interrupted import, and `recover`
//!
//! Killing an import part way never lands part of it, but it can leave the
//! spine NOT AT REST: its transaction row un-cleared, its locks held. Every
//! later import then refuses at step 2, and with no soup daemon nothing
//! else would ever settle it. `cauldron soup recover` does: under the same
//! two proofs (no live soup daemon, the writer lock held exclusive) it runs
//! `Cauldron::open`, whose recovery pass aborts a row that was never
//! decided (nothing of that import lands) and rolls a decided row forward
//! (all of it lands, once), and reports which of the two happened, with
//! the ids that landed. The difference decides what the user does next:
//! a rolled-back import may be run again, a landed one must not be (with
//! `--remint` it would land every record a second time).
//!
//! Like `ctl`, every verb needs the environment NAMED (`--env`, a leading
//! `--env`, or `$CAULDRON_ENV`): it never defaults to prod. The cauldron is
//! always named too.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use env::seams::{self, Table};
use env::Environment;
use serde_json::{json, Value};
use spine::migrate::records::{import_records, RecordsOptions};
use spine::migrate::{Landing, MigrationPlan};
use spine::pool::{NodeId, Pool, QualifiedNodeId};
use spine::schema_def::PoolName;

/// The soup's one pool (`engine.rs` `SOUP_POOL`).
const DEFAULT_POOL: &str = "ws";
/// The soup's seam name in an environment's seam table.
const SOUP_SEAM: &str = "soup";

const USAGE: &str = "\
cauldron soup — a cauldron's soup, offline

    cauldron soup import <file-or-dir> --cauldron <name> [--env <env>] [--dry-run] [--remint]
                         [--agent <label>]
        Land records written in the published shape (schema/soup/workspace-v6/README.md):
        one file holding a list (or one record), or a directory of one-record files; JSON,
        or YAML for .yaml/.yml. Every record is judged by the soup's own door; the JSON
        printed gives each one's disposition (accepted, or refused with the grammar's
        reason). Without --dry-run the whole set lands as ONE write when every record is
        accepted, and nothing lands otherwise. Ids are carried as written and a collision
        refuses the import; --remint treats them as batch-local, gives each record the
        next free id for its type, rewrites references and [[links]] inside the batch, and
        prints the old -> new map. Under --remint a label may not equal an id the soup
        already holds (refused, `label-shadows-existing`), so a bare id that is not a label
        always names the existing node; `<ID>@<pool>` names it explicitly. An edge to an id
        outside the batch must name a node the soup already holds. A real import is refused
        while a soup daemon runs in the environment, while another import or recover is
        writing to the same soup, and while the soup is not at rest (an earlier import was
        interrupted: see `recover`); --dry-run is then judged against a read-only snapshot.
        --agent records who wrote (default `outside-agent`).
    cauldron soup recover --cauldron <name> [--env <env>]
        Settle a soup that an interrupted import left not at rest: an import that never
        reached its commit decision is rolled back (none of it lands), one that did is
        finished (all of it lands, once). Refused while a soup daemon runs in the
        environment or another import or recover is writing to the soup; a soup already at
        rest is not touched. Prints (JSON on stdout, the same in words on stderr) which of
        the two happened: `interrupted_import` is `landed` (with how many records and their
        ids: do NOT import that batch again, or every record lands twice), `rolled-back`
        (none of it landed: importing it again is safe) or `none` (nothing to recover).
        Also says what still stands in the way if anything does.
    cauldron soup regions --cauldron <name> [--env <env>] [--max-chars N]
        Every region: id, handle, boundary, flags, the regions it narrows or relates to,
        and how many nodes sit in it.
    cauldron soup region <id> --cauldron <name> [--env <env>] [--depth N] [--limit N] [--max-chars N]
        One region: its record, the regions it narrows (parents) and that narrow it
        (children) to --depth levels (default 1), the regions it relates to either way, and
        its direct members (id, type, state, handle), newest first, at most --limit (50).
    cauldron soup nodes --cauldron <name> [--env <env>] [--region <id>] [--type <t>]
                        [--state <s>] [--limit N] [--max-chars N]
        Nodes as {id, type, state, handle, regions}, newest first (by the spine's commit
        order; one write's nodes by id prefix, then serial, highest first), at most --limit
        (default 50); --region keeps a region's direct members.
    cauldron soup search <query> --cauldron <name> [--env <env>] [--region <id>] [--type <t>]
                         [--limit N] [--max-chars N]
        Lexical search over handles and bodies: every word of the query a term, found
        case-insensitively as a substring; ranked by how many terms match, then handle hits over body hits, then newest.
        Prints {id, type, state, handle, regions, score, snippet}, at most --limit (20);
        the snippet is --max-chars long (default 160) around the first hit.
    cauldron soup get <id>... --cauldron <name> [--env <env>] [--max-chars N]
        Those nodes as records in the published shape, with their outgoing edges.
    cauldron soup help

  Every verb prints JSON on stdout and exits non-zero on a refusal. Reads open the store
  so that they cannot write. --max-chars cuts any longer text field, and the output's
  `truncated` lists what was cut; a list cut by --limit says how many `more` there are.
  `--pool <p>` names another pool than `ws`.
";

/// `cauldron soup <verb> …`.
pub(crate) fn verb_soup(args: &[String]) -> Result<i32> {
    match args.first().map(String::as_str) {
        Some("help") | Some("-h") | Some("--help") => {
            print!("{USAGE}");
            Ok(0)
        }
        Some("import") => import(&args[1..]),
        Some("regions") => regions(&args[1..]),
        Some("region") => region(&args[1..]),
        Some("nodes") => nodes(&args[1..]),
        Some("search") => search(&args[1..]),
        Some("get") => get(&args[1..]),
        Some("recover") => recover(&args[1..]),
        None => {
            eprint!("{USAGE}");
            Ok(2)
        }
        Some(other) => {
            eprintln!("cauldron soup: no verb `{other}` (import | recover | regions | region | nodes | search | get | help)");
            Ok(2)
        }
    }
}

#[derive(Default)]
struct Args {
    env: Option<String>,
    cauldron: Option<String>,
    pool: Option<String>,
    agent: Option<String>,
    max_chars: Option<usize>,
    region: Option<String>,
    node_type: Option<String>,
    state: Option<String>,
    limit: Option<usize>,
    depth: Option<usize>,
    dry_run: bool,
    remint: bool,
    pos: Vec<String>,
}

fn parse(verb: &str, args: &[String], allow: &[&str]) -> Result<Args> {
    let mut a = Args::default();
    let mut it = args.iter();
    let value = |it: &mut std::slice::Iter<'_, String>, flag: &str| -> Result<String> {
        it.next().cloned().with_context(|| format!("`cauldron soup {verb} {flag}` needs a value"))
    };
    while let Some(s) = it.next() {
        let (flag, inline) = match s.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (s.as_str(), None),
        };
        if flag.starts_with('-') && !allow.contains(&flag) {
            bail!("unknown option `{s}` for `cauldron soup {verb}`");
        }
        let take = |it: &mut std::slice::Iter<'_, String>| -> Result<String> {
            match &inline {
                Some(v) => Ok(v.clone()),
                None => value(it, flag),
            }
        };
        match flag {
            "--env" => a.env = Some(take(&mut it)?),
            "--cauldron" | "-c" => a.cauldron = Some(take(&mut it)?),
            "--pool" => a.pool = Some(take(&mut it)?),
            "--agent" => a.agent = Some(take(&mut it)?),
            "--max-chars" => a.max_chars = Some(count(flag, &take(&mut it)?)?),
            "--limit" => a.limit = Some(count(flag, &take(&mut it)?)?),
            "--depth" => a.depth = Some(count(flag, &take(&mut it)?)?),
            "--region" => a.region = Some(take(&mut it)?),
            "--type" => a.node_type = Some(take(&mut it)?),
            "--state" => a.state = Some(take(&mut it)?),
            "--dry-run" => a.dry_run = true,
            "--remint" => a.remint = true,
            _ => a.pos.push(s.clone()),
        }
    }
    Ok(a)
}

fn count(flag: &str, v: &str) -> Result<usize> {
    v.parse().with_context(|| format!("{flag} takes a count, not `{v}`"))
}

/// The environment, named (never the default — a forgotten `--env` must not
/// land in prod), and the named cauldron in it.
fn target(verb: &str, a: &Args) -> Result<(Environment, env::Cauldron)> {
    let env_name = match &a.env {
        Some(n) => n.clone(),
        None => match Environment::resolve() {
            Ok(e) if e.name_source != env::paths::Source::Default => e.name,
            _ => bail!("`cauldron soup {verb}` needs --env <name>: it never defaults to prod"),
        },
    };
    let e = Environment::resolve()?.for_name(&env_name)?;
    if !e.data_root.is_dir() {
        bail!("REFUSED: there is no environment `{}` ({} does not exist)", e.name, e.data_root.display());
    }
    let name = match &a.cauldron {
        Some(n) => n.clone(),
        None => match env::cauldron::requested_cauldron()? {
            Some((n, _)) => n,
            None => bail!("`cauldron soup {verb}` needs --cauldron <name>"),
        },
    };
    let Some(c) = e.find_cauldron(&name)? else {
        bail!(
            "REFUSED: there is no cauldron `{name}` in environment `{}`. `cauldron list --env {}` shows its cauldrons.",
            e.name,
            e.name
        );
    };
    let soup = c.soup();
    if !soup.join(spine::spine_db::SPINE_FILE).is_file() {
        bail!("REFUSED: cauldron `{name}` has no soup yet ({} holds no spine.db)", soup.display());
    }
    Ok((e, c))
}

fn pool_name(a: &Args) -> PoolName {
    PoolName::new(a.pool.as_deref().unwrap_or(DEFAULT_POOL))
}

fn print(v: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

// ------------------------------------------------------------------- import

fn import(args: &[String]) -> Result<i32> {
    let a = parse("import", args, &["--env", "--cauldron", "-c", "--pool", "--agent", "--dry-run", "--remint"])?;
    let [input] = a.pos.as_slice() else {
        bail!("`cauldron soup import` takes ONE file or directory (got {}); `cauldron soup help`", a.pos.len());
    };
    let records = read_records(Path::new(input))?;
    let (e, c) = target("import", &a)?;
    let soup = c.soup();

    // A dry run never opens the live store with the spine's writing open:
    // it is judged on a snapshot copy (see the module docs), so it needs no
    // proof that no daemon holds the soup, nor that the soup is at rest.
    let _copy;
    let mut writer_lock = None;
    let cauldron = if a.dry_run {
        let dir = ScratchDir::new()?;
        spine::offline::snapshot_copy(&soup, &dir.0).map_err(|err| {
            anyhow::anyhow!("cannot take a read-only snapshot of cauldron `{}`'s soup to judge the dry run on: {err}", c.name)
        })?;
        let opened = spine::Cauldron::open(&dir.0, spine::config::SpineConfig::default())
            .map_err(|err| anyhow::anyhow!("opening the snapshot of cauldron `{}`'s soup: {err}", c.name))?;
        _copy = dir;
        opened
    } else {
        // 1. No running soup daemon may hold it, provably; and no other
        //    import or recover may be writing it (held to the end of `import`).
        refuse_live_soup(&e)?;
        writer_lock = Some(take_writer_lock(&c, "import")?);
        // 2. At rest, so the open-time passes have nothing to do.
        let unrest = spine::offline::rest_check(&soup).map_err(|err| {
            anyhow::anyhow!("REFUSED: cannot read {}'s spine, so cannot prove it is at rest: {err}", c.name)
        })?;
        if !unrest.is_empty() {
            bail!(
                "REFUSED: cauldron `{}`'s soup is not at rest, and an outside import writes only to a soup at rest: {}. \
                 An import that was interrupted (killed, or its machine stopped) leaves a soup like this. \
                 Run `cauldron soup recover --env {} --cauldron {}` first and read what it prints: it either \
                 FINISHES the interrupted import (it landed in full: do NOT import that batch again, or every \
                 record lands twice) or ROLLS IT BACK (none of it landed: importing it again is safe). \
                 Nothing was written.",
                c.name,
                unrest.join("; "),
                e.name,
                c.name
            );
        }
        // 3. The spine's own open, checked: it must have done nothing.
        let cauldron = spine::Cauldron::open(&soup, spine::config::SpineConfig::default())
            .map_err(|err| anyhow::anyhow!("opening cauldron `{}`'s soup: {err}", c.name))?;
        let rec = cauldron.recovered_at_open();
        if !rec.rolled_forward.is_empty() || !rec.cleared_only.is_empty() || !rec.aborted.is_empty() || !cauldron.tranches_at_open().tranches.is_empty() {
            bail!(
                "REFUSED: another writer touched cauldron `{}`'s soup between the rest check and the open (the open \
                 recovered work); nothing was imported. Try again once it is quiet.",
                c.name
            );
        }
        cauldron
    };

    let agent = a.agent.clone().unwrap_or_else(|| "outside-agent".to_string());
    let mut plan = MigrationPlan::new(spine::envelope::Initiator::Agent {
        thread: agent,
        dispatch: format!("cauldron soup import {}", Path::new(input).file_name().and_then(|n| n.to_str()).unwrap_or("records")),
    });
    // Two imports of the same text are two writes, never one replayed.
    plan.idempotency_nonce = Some(format!(
        "soup-import:{}:{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    ));
    let opts = RecordsOptions { pool: pool_name(&a), remint: a.remint, commit: !a.dry_run };
    let run = import_records(&cauldron, &plan, &records, &opts).map_err(|err| anyhow::anyhow!("{err}"))?;

    let accepted = run.records.iter().filter(|r| r.disposition == "accepted").count();
    let refused = run.records.len() - accepted;
    let (outcome, landed, landing_refusal) = match &run.landing {
        None if a.dry_run && refused == 0 => ("validated", Value::Null, Value::Null),
        None => ("refused", Value::Null, Value::Null),
        Some(Landing::Committed { tx, seq, deduplicated, .. }) => {
            ("landed", json!({"tx": tx.to_hex(), "seq": seq, "deduplicated": deduplicated}), Value::Null)
        }
        Some(Landing::NothingAccepted) => ("empty", Value::Null, Value::Null),
        Some(Landing::Refused { witnesses }) => ("refused", Value::Null, serde_json::to_value(witnesses)?),
    };
    let mut doc = json!({
        "env": e.name,
        "cauldron": c.name,
        "pool": opts.pool.as_str(),
        "dry_run": a.dry_run,
        "remint": a.remint,
        "judged_on": if a.dry_run { "a read-only snapshot of the soup, taken now; nothing was written" } else { "the soup" },
        "outcome": outcome,
        "accepted": accepted,
        "refused": refused,
        "records": serde_json::to_value(&run.records)?,
    });
    if a.remint {
        doc["id_map"] = serde_json::to_value(&run.id_map)?;
    }
    if !landed.is_null() {
        doc["landed"] = landed;
    }
    if !landing_refusal.is_null() {
        doc["landing_refused"] = landing_refusal;
    }
    print(&doc)?;
    drop(writer_lock);
    Ok(if outcome == "landed" || outcome == "validated" { 0 } else { 1 })
}

// ------------------------------------------------------------------ recover

/// The file a writer of this soup locks EXCLUSIVE for as long as it writes:
/// `<soup>/writer.lock`. `import` and `recover` both take it, non-blocking,
/// after the soup-daemon check; the kernel drops it when the holder exits,
/// however it exits, so a killed import never leaves it held.
const WRITER_LOCK: &str = "writer.lock";

/// **The soup's writer lock, taken EXCLUSIVE and non-blocking.** Held until
/// the returned file is dropped. Another `cauldron soup import` or
/// `cauldron soup recover` holding it makes this refuse.
fn take_writer_lock(c: &env::Cauldron, verb: &str) -> Result<std::fs::File> {
    let path = c.soup().join(WRITER_LOCK);
    let fh = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    match fh.try_lock() {
        Ok(()) => Ok(fh),
        Err(std::fs::TryLockError::WouldBlock) => bail!(
            "REFUSED: another `cauldron soup import` or `cauldron soup recover` is writing to cauldron `{}`'s soup \
             right now ({} is held), so `cauldron soup {verb}` did not touch it. Nothing was written. Try again \
             when it has finished.",
            c.name,
            path.display()
        ),
        Err(std::fs::TryLockError::Error(err)) => Err(err).with_context(|| format!("locking {}", path.display())),
    }
}

/// **`cauldron soup recover`**: settle a soup an interrupted import left
/// not at rest. An import killed part way (Ctrl-C, a crash, a machine that
/// stopped) leaves its transaction row un-cleared and its locks held; the
/// spine's own recovery pass, which `spine::Cauldron::open` runs, settles
/// that from the row alone: a row that was never decided is ABORTED
/// (nothing of it landed, nothing lands), a decided row is ROLLED FORWARD
/// (all of it lands, exactly once), and orphaned locks are released.
///
/// It runs that open only when it can prove no other process holds the
/// store: no live soup daemon in the environment (the seam table, as for
/// `import`), and the soup's writer lock, which every import holds while it
/// writes, taken exclusive. A soup already at rest is not opened at all.
fn recover(args: &[String]) -> Result<i32> {
    let a = parse("recover", args, &["--env", "--cauldron", "-c"])?;
    if !a.pos.is_empty() {
        bail!("`cauldron soup recover` takes no arguments (got `{}`); `cauldron soup help`", a.pos.join(" "));
    }
    let (e, c) = target("recover", &a)?;
    let soup = c.soup();
    refuse_live_soup(&e)?;
    let _lock = take_writer_lock(&c, "recover")?;
    let before = spine::offline::rest_check(&soup)
        .map_err(|err| anyhow::anyhow!("REFUSED: cannot read {}'s spine: {err}", c.name))?;
    let mut doc = json!({
        "env": e.name,
        "cauldron": c.name,
        "before": before,
    });
    let nodes_cmd = format!("cauldron soup nodes --env {} --cauldron {}", e.name, c.name);
    if before.is_empty() {
        doc["outcome"] = json!("at-rest");
        doc["interrupted_import"] = json!("none");
        doc["note"] = json!("the soup was already at rest; the spine was not opened and no data was written");
        doc["instruction"] = json!(format!(
            "Nothing to recover: no interrupted import is waiting to be settled. This cannot tell you whether an \
             import you stopped earlier landed (one stopped after it finished landed in full, one stopped before \
             it began writing landed nothing, and an earlier recover printed its own outcome), so check with \
             `{nodes_cmd}` (newest first) before importing that batch again."
        ));
        say(&doc);
        print(&doc)?;
        return Ok(0);
    }
    let cauldron = spine::Cauldron::open(&soup, spine::config::SpineConfig::default())
        .map_err(|err| anyhow::anyhow!("recovering cauldron `{}`'s soup: {err}", c.name))?;
    let rec = cauldron.recovered_at_open();
    let hexes = |v: &[spine::envelope::TxHash]| v.iter().map(|h| h.to_hex()).collect::<Vec<_>>();
    doc["rolled_forward"] = json!(hexes(&rec.rolled_forward));
    doc["cleared"] = json!(hexes(&rec.cleared_only));
    doc["aborted"] = json!(hexes(&rec.aborted));
    doc["live_tranches"] = json!(cauldron.tranches_at_open().tranches.len());
    // A row rolled forward, or found landed and only cleared, is an import
    // that LANDED; a row aborted (here, or by another recovery) landed
    // nothing.
    let landed_tx: Vec<String> = hexes(&rec.rolled_forward).into_iter().chain(hexes(&rec.cleared_only)).collect();
    let rolled_back = rec.aborted.len() + rec.aborted_elsewhere.len();
    drop(cauldron);
    let after = spine::offline::rest_check(&soup)
        .map_err(|err| anyhow::anyhow!("cannot read {}'s spine after recovery: {err}", c.name))?;
    let settled = after.is_empty();
    doc["after"] = json!(after);
    doc["outcome"] = json!(if settled { "recovered" } else { "not-at-rest" });

    let landed = landed_nodes(&soup, &landed_tx)
        .map_err(|err| anyhow::anyhow!("cannot read which records landed in {}'s soup: {err}", c.name))?;
    let mut steps = Vec::new();
    match (landed_tx.is_empty(), rolled_back) {
        (false, 0) => doc["interrupted_import"] = json!("landed"),
        (true, n) if n > 0 => doc["interrupted_import"] = json!("rolled-back"),
        (false, _) => doc["interrupted_import"] = json!("landed-and-rolled-back"),
        (true, _) => doc["interrupted_import"] = json!("none"),
    }
    if !landed_tx.is_empty() {
        doc["landed_records"] = json!(landed.len());
        doc["landed_ids"] = landed_summary(&landed);
        steps.push(format!(
            "The interrupted import LANDED: recover finished it, and its {} record(s) are now in the soup ({}). \
             Do NOT import that batch again: every record would land a second time, and this release cannot \
             delete nodes. An import run with --remint printed no id map; `{nodes_cmd}` lists the landed nodes \
             newest first.",
            landed.len(),
            id_ranges_text(&landed)
        ));
    }
    if rolled_back > 0 {
        steps.push(
            "The interrupted import was ROLLED BACK: none of its records landed. It is safe to import that batch \
             again."
                .to_string(),
        );
    }
    if landed_tx.is_empty() && rolled_back == 0 {
        steps.push("No interrupted import was found to finish or roll back.".to_string());
    }
    if !settled {
        steps.push(format!(
            "The soup is still NOT at rest ({}); do not import anything until a recover reports `recovered`.",
            doc["after"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("; ")).unwrap_or_default()
        ));
    }
    doc["instruction"] = json!(steps.join(" "));
    say(&doc);
    print(&doc)?;
    Ok(if settled { 0 } else { 1 })
}

/// The human text of a `recover` result, on stderr (stdout carries the
/// JSON): the outcome and its instruction.
fn say(doc: &Value) {
    eprintln!(
        "cauldron soup recover: {} (interrupted import: {})\n{}",
        doc["outcome"].as_str().unwrap_or("?"),
        doc["interrupted_import"].as_str().unwrap_or("?"),
        doc["instruction"].as_str().unwrap_or("")
    );
}

/// Every node, `(pool, id)`, that one of the transactions `tx` (hex hashes)
/// created, read through read-only connections to each pool file of the
/// soup at `soup`; sorted by pool, id prefix, then serial.
fn landed_nodes(soup: &Path, tx: &[String]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    if tx.is_empty() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(soup).with_context(|| format!("listing {}", soup.display()))? {
        let p = entry.with_context(|| format!("listing {}", soup.display()))?.path();
        let stem = p.file_stem().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if p.extension().and_then(|x| x.to_str()) != Some("db") || stem == "spine" {
            continue;
        }
        let conn = spine::offline::read_only_connection(&p).map_err(|err| anyhow::anyhow!("{err}"))?;
        let mut stmt = conn.prepare("SELECT id FROM nodes WHERE created_tx = ?1")?;
        for h in tx {
            for id in stmt.query_map([h], |r| r.get::<_, String>(0))? {
                out.push((stem.clone(), id?));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| prefix(&a.1).cmp(prefix(&b.1))).then_with(|| serial(&a.1).cmp(&serial(&b.1))).then_with(|| a.1.cmp(&b.1)));
    Ok(out)
}

/// Per pool and id prefix: how many landed, the first and last id, and
/// whether the serials between them are all present (a contiguous range).
fn landed_groups(landed: &[(String, String)]) -> Vec<(String, String, usize, String, String, bool)> {
    let mut groups: Vec<(String, String, usize, String, String, bool)> = Vec::new();
    for (pool, id) in landed {
        match groups.last_mut() {
            Some(g) if g.0 == *pool && g.1 == prefix(id) => {
                g.5 &= serial(id) == serial(&g.4) + 1;
                g.2 += 1;
                g.4 = id.clone();
            }
            _ => groups.push((pool.clone(), prefix(id).to_string(), 1, id.clone(), id.clone(), true)),
        }
    }
    groups
}

/// The JSON form of [`landed_groups`].
fn landed_summary(landed: &[(String, String)]) -> Value {
    Value::Array(
        landed_groups(landed)
            .into_iter()
            .map(|(pool, prefix, count, first, last, contiguous)| {
                json!({"pool": pool, "prefix": prefix, "count": count, "first": first, "last": last, "contiguous": contiguous})
            })
            .collect(),
    )
}

/// The text form of [`landed_groups`]: `IS-2..IS-5001 (5000) in ws`.
fn id_ranges_text(landed: &[(String, String)]) -> String {
    if landed.is_empty() {
        return "no node ids found for it".to_string();
    }
    landed_groups(landed)
        .into_iter()
        .map(|(pool, _, count, first, last, contiguous)| {
            let span = if count == 1 { first } else { format!("{first}..{last}") };
            let gaps = if contiguous { "" } else { ", not contiguous" };
            format!("{span} ({count}{gaps}) in {pool}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A running soup daemon in the environment may hold any of its cauldrons,
/// so its live `soup` row refuses a real import; a seam table that cannot be
/// read refuses too (no proof). Reads the seam table only.
fn refuse_live_soup(e: &Environment) -> Result<()> {
    let table = Table::load(&e.name, &e.data_root, &e.seams()).with_context(|| {
        format!("REFUSED: cannot read environment `{}`'s seam table, so cannot prove no soup daemon holds the cauldron", e.name)
    })?;
    if let Some(s) = table.seams.get(SOUP_SEAM) {
        if !seams::probe(s).process_gone(s) {
            bail!(
                "REFUSED: environment `{}`'s soup daemon is running (pid {}, {}) and may hold this cauldron's \
                 soup. `cauldron soup import` and `cauldron soup recover` write only to a soup no daemon holds, \
                 because they open the store directly. Nothing was written. While it runs, `import --dry-run` \
                 still judges a batch against a read-only snapshot, and the reads (`soup regions | region | \
                 nodes | search | get`) still work.",
                e.name,
                s.pid,
                s.addr
            );
        }
    }
    Ok(())
}

/// A private temporary directory, removed with everything in it on drop.
struct ScratchDir(PathBuf);
impl ScratchDir {
    fn new() -> Result<ScratchDir> {
        let p = std::env::temp_dir().join(format!(
            "cauldron-soup-dry-run-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).with_context(|| format!("creating {}", p.display()))?;
        Ok(ScratchDir(p))
    }
}
impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Records from one file (a list, or one record) or a directory of
/// one-record files (`*.json`, `*.yaml`, `*.yml`, in name order).
fn read_records(path: &Path) -> Result<Vec<Value>> {
    if path.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(path)
            .with_context(|| format!("listing {}", path.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| matches!(p.extension().and_then(|x| x.to_str()), Some("json" | "yaml" | "yml")))
            .collect();
        files.sort();
        if files.is_empty() {
            bail!("{} holds no .json, .yaml or .yml record files", path.display());
        }
        let mut out = Vec::new();
        for f in files {
            match read_file(&f)? {
                Value::Array(list) => out.extend(list),
                one => out.push(one),
            }
        }
        return Ok(out);
    }
    match read_file(path)? {
        Value::Array(list) => Ok(list),
        one => Ok(vec![one]),
    }
}

fn read_file(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    match path.extension().and_then(|x| x.to_str()) {
        Some("yaml" | "yml") => {
            serde_yaml::from_str::<Value>(&text).with_context(|| format!("{} is not YAML", path.display()))
        }
        _ => serde_json::from_str::<Value>(&text).with_context(|| format!("{} is not JSON", path.display())),
    }
}

// -------------------------------------------------------------------- reads

/// The pool, through a connection that cannot write.
fn read_pool(c: &env::Cauldron, pool: &PoolName) -> Result<Pool> {
    let file = Pool::path_in(&c.soup(), pool);
    Pool::open_read_only(&file).map_err(|err| anyhow::anyhow!("reading cauldron `{}`'s pool `{pool}`: {err}", c.name))
}

const REGION_FLAGS: [&str; 5] = ["retired", "provisional", "deferred_by", "thresholds", "wake_when"];

fn regions(args: &[String]) -> Result<i32> {
    let a = parse("regions", args, &["--env", "--cauldron", "-c", "--pool", "--max-chars"])?;
    if let Some(p) = a.pos.first() {
        bail!("`cauldron soup regions` takes no ids (got `{p}`); `soup get` reads nodes");
    }
    let (e, c) = target("regions", &a)?;
    let pool = pool_name(&a);
    let handle = read_pool(&c, &pool)?;
    let snap = handle.snapshot().map_err(|err| anyhow::anyhow!("{err}"))?;
    let mut out = Vec::new();
    let mut cut = Cut::new(a.max_chars);
    for r in snap.nodes_of("region", None).map_err(|err| anyhow::anyhow!("{err}"))? {
        let field = |k: &str| r.fields.get(k).map(|f| f.value.clone());
        let text = |cut: &mut Cut, k: &str| r.fields.get(k).map(|f| cut.value(&r.id.0, k, &f.value));
        let mut flags = serde_json::Map::new();
        for k in REGION_FLAGS {
            if let Some(v) = field(k) {
                flags.insert(k.to_string(), v);
            }
        }
        let edges = snap.edges_from(&r.id, None).map_err(|err| anyhow::anyhow!("{err}"))?;
        let of = |kind: &str| -> Vec<String> {
            edges.iter().filter(|x| x.kind == kind).map(|x| target_text(&pool, &x.dst)).collect()
        };
        let members = snap
            .edges_to(&QualifiedNodeId { pool: pool.clone(), id: r.id.clone() }, Some("in_region"))
            .map_err(|err| anyhow::anyhow!("{err}"))?
            .len();
        out.push(json!({
            "id": r.id.0,
            "handle": text(&mut cut, "handle"),
            "boundary": text(&mut cut, "boundary"),
            "flags": flags,
            "narrows": of("narrows"),
            "relates": of("relates"),
            "members": members,
        }));
    }
    let mut doc = json!({"env": e.name, "cauldron": c.name, "pool": pool.as_str(), "regions": out});
    cut.report(&mut doc);
    print(&doc)?;
    Ok(0)
}

fn get(args: &[String]) -> Result<i32> {
    let a = parse("get", args, &["--env", "--cauldron", "-c", "--pool", "--max-chars"])?;
    if a.pos.is_empty() {
        bail!("`cauldron soup get` needs at least one id");
    }
    let (e, c) = target("get", &a)?;
    let pool = pool_name(&a);
    let handle = read_pool(&c, &pool)?;
    let snap = handle.snapshot().map_err(|err| anyhow::anyhow!("{err}"))?;
    let mut records = Vec::new();
    let mut missing = Vec::new();
    let mut cut = Cut::new(a.max_chars);
    for id in &a.pos {
        let Some(n) = snap.node(&NodeId(id.clone())).map_err(|err| anyhow::anyhow!("{err}"))? else {
            missing.push(id.clone());
            continue;
        };
        let mut rec = serde_json::Map::new();
        rec.insert("id".into(), json!(n.id.0));
        rec.insert("type".into(), json!(n.node_type));
        if let Some(s) = &n.state {
            rec.insert("state".into(), json!(s));
        }
        for (k, f) in &n.fields {
            rec.insert(k.clone(), cut.value(&n.id.0, k, &f.value));
        }
        let mut edges = Vec::new();
        for x in snap.edges_from(&n.id, None).map_err(|err| anyhow::anyhow!("{err}"))? {
            let mut m = serde_json::Map::new();
            m.insert("kind".into(), json!(x.kind));
            m.insert("to".into(), json!(target_text(&pool, &x.dst)));
            if let Some(note) = &x.note {
                m.insert("note".into(), json!(note));
            }
            let fields: BTreeMap<String, Value> = x.fields.clone();
            for (k, v) in fields {
                m.insert(k, v);
            }
            edges.push(Value::Object(m));
        }
        if !edges.is_empty() {
            rec.insert("edges".into(), Value::Array(edges));
        }
        records.push(Value::Object(rec));
    }
    let mut doc = json!({"env": e.name, "cauldron": c.name, "pool": pool.as_str(), "records": records});
    if !missing.is_empty() {
        doc["missing"] = json!(missing);
    }
    cut.report(&mut doc);
    print(&doc)?;
    Ok(if missing.is_empty() { 0 } else { 1 })
}

/// An edge's far end as the published shape writes it: the bare id in the
/// same pool, `<ID>@<pool>` in another.
fn target_text(pool: &PoolName, dst: &QualifiedNodeId) -> String {
    if &dst.pool == pool {
        dst.id.0.clone()
    } else {
        format!("{}@{}", dst.id.0, dst.pool)
    }
}

// ------------------------------------------------------- truncation, shared

/// `--max-chars`: cuts any longer string value and remembers what it cut,
/// for the output's `truncated` list (`{id, field, chars}`, `chars` the
/// whole length).
struct Cut {
    max: Option<usize>,
    cut: Vec<Value>,
}

impl Cut {
    fn new(max: Option<usize>) -> Cut {
        Cut { max, cut: Vec::new() }
    }

    fn value(&mut self, id: &str, field: &str, v: &Value) -> Value {
        match (self.max, v) {
            (Some(max), Value::String(s)) => Value::String(self.text(id, field, s, max)),
            _ => v.clone(),
        }
    }

    fn text(&mut self, id: &str, field: &str, s: &str, max: usize) -> String {
        let chars = s.chars().count();
        if chars <= max {
            return s.to_string();
        }
        self.cut.push(json!({"id": id, "field": field, "chars": chars}));
        s.chars().take(max).collect()
    }

    fn report(self, doc: &mut Value) {
        if !self.cut.is_empty() {
            doc["truncated"] = Value::Array(self.cut);
        }
    }
}

// ---------------------------------------------------- the catalog of nodes

/// One node as the listing reads carry it.
struct Entry {
    id: String,
    node_type: String,
    state: Option<String>,
    handle: String,
    body: String,
    regions: Vec<String>,
    /// The committed-commit counter at the commit that created it; `None`
    /// when the spine has no landed row for it (not expected; sorted first).
    order: Option<i64>,
}

/// Every node of the pool, newest first: by the commit that created it
/// (the spine's order, never a timestamp); nodes of one write by id prefix,
/// then serial, highest first. Read through connections that cannot write.
fn catalog(c: &env::Cauldron, pool: &PoolName, snap: &spine::pool::Snapshot<'_>, types: &[String]) -> Result<Vec<Entry>> {
    let order = spine::offline::commit_order(&c.soup())
        .map_err(|err| anyhow::anyhow!("reading cauldron `{}`'s commit order: {err}", c.name))?;
    let mut out = Vec::new();
    for t in types {
        for n in snap.nodes_of(t, None).map_err(|err| anyhow::anyhow!("{err}"))? {
            let text = |k: &str| n.fields.get(k).and_then(|f| f.value.as_str()).unwrap_or("").to_string();
            let regions = snap
                .edges_from(&n.id, Some("in_region"))
                .map_err(|err| anyhow::anyhow!("{err}"))?
                .iter()
                .map(|x| target_text(pool, &x.dst))
                .collect();
            let created = snap.created_tx(&n.id).map_err(|err| anyhow::anyhow!("{err}"))?;
            out.push(Entry {
                id: n.id.0.clone(),
                node_type: n.node_type.clone(),
                state: n.state.clone(),
                handle: text("handle"),
                body: text("body"),
                regions,
                order: created.and_then(|h| order.get(&h.to_hex()).copied()),
            });
        }
    }
    out.sort_by(|a, b| {
        let newest = |e: &Entry| e.order.map_or(i64::MAX, |o| o);
        newest(b).cmp(&newest(a)).then_with(|| prefix(&a.id).cmp(prefix(&b.id))).then_with(|| serial(&b.id).cmp(&serial(&a.id))).then_with(|| a.id.cmp(&b.id))
    });
    Ok(out)
}

/// The prefix of `<PREFIX>-<n>`, the whole id for anything else.
fn prefix(id: &str) -> &str {
    id.rsplit_once('-').map_or(id, |(p, _)| p)
}

/// The serial of `<PREFIX>-<n>`, 0 for anything else.
fn serial(id: &str) -> u64 {
    id.rsplit_once('-').and_then(|(_, n)| n.parse().ok()).unwrap_or(0)
}

/// The node types to read: `--type` (checked against the pool's grammar),
/// or every type the grammar declares.
fn types_of(pool: &Pool, asked: Option<&str>) -> Result<Vec<String>> {
    let all: Vec<String> = pool.grammar().node_types.keys().cloned().collect();
    match asked {
        None => Ok(all),
        Some(t) if all.iter().any(|x| x == t) => Ok(vec![t.to_string()]),
        Some(t) => bail!("pool `{}` has no node type `{t}` (it has: {})", pool.name(), all.join(", ")),
    }
}

/// `--region`, checked: it must name a region the pool holds.
fn region_filter(snap: &spine::pool::Snapshot<'_>, asked: Option<&str>) -> Result<Option<String>> {
    let Some(r) = asked else { return Ok(None) };
    match snap.node(&NodeId(r.to_string())).map_err(|err| anyhow::anyhow!("{err}"))? {
        Some(n) if n.node_type == "region" => Ok(Some(r.to_string())),
        Some(n) => bail!("`{r}` is a {}, not a region", n.node_type),
        None => bail!("there is no region `{r}`; `cauldron soup regions` lists them"),
    }
}

/// A list cut to `limit`, with how many were left out.
fn bounded<T>(mut v: Vec<T>, limit: usize) -> (Vec<T>, usize) {
    let more = v.len().saturating_sub(limit);
    v.truncate(limit);
    (v, more)
}

const NODES_LIMIT: usize = 50;
const SEARCH_LIMIT: usize = 20;
const MEMBERS_LIMIT: usize = 50;
const SNIPPET_CHARS: usize = 160;

fn nodes(args: &[String]) -> Result<i32> {
    let a = parse(
        "nodes",
        args,
        &["--env", "--cauldron", "-c", "--pool", "--region", "--type", "--state", "--limit", "--max-chars"],
    )?;
    if let Some(p) = a.pos.first() {
        bail!("`cauldron soup nodes` takes no ids (got `{p}`); `soup get` reads nodes by id");
    }
    let (e, c) = target("nodes", &a)?;
    let pool = pool_name(&a);
    let handle = read_pool(&c, &pool)?;
    let snap = handle.snapshot().map_err(|err| anyhow::anyhow!("{err}"))?;
    let types = types_of(&handle, a.node_type.as_deref())?;
    let region = region_filter(&snap, a.region.as_deref())?;
    let all: Vec<Entry> = catalog(&c, &pool, &snap, &types)?
        .into_iter()
        .filter(|n| a.state.as_deref().is_none_or(|s| n.state.as_deref() == Some(s)))
        .filter(|n| region.as_ref().is_none_or(|r| n.regions.contains(r)))
        .collect();
    let total = all.len();
    let (shown, more) = bounded(all, a.limit.unwrap_or(NODES_LIMIT));
    let mut cut = Cut::new(a.max_chars);
    let list: Vec<Value> = shown
        .iter()
        .map(|n| {
            json!({
                "id": n.id,
                "type": n.node_type,
                "state": n.state,
                "handle": cut.value(&n.id, "handle", &json!(n.handle)),
                "regions": n.regions,
            })
        })
        .collect();
    let mut doc = json!({"env": e.name, "cauldron": c.name, "pool": pool.as_str(), "total": total, "nodes": list});
    if more > 0 {
        doc["more"] = json!(more);
    }
    cut.report(&mut doc);
    print(&doc)?;
    Ok(0)
}

/// Lower-case one char to one char, so char positions in the folded text
/// are positions in the original.
fn fold(s: &str) -> Vec<char> {
    s.chars().map(|ch| ch.to_lowercase().next().unwrap_or(ch)).collect()
}

/// Where `term` first occurs in `hay`, as a char index.
fn find(hay: &[char], term: &[char]) -> Option<usize> {
    if term.is_empty() || term.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - term.len()).find(|&i| hay[i..i + term.len()] == *term)
}

/// The query's terms: its words (runs of letters and digits), folded,
/// without repeats, in order.
fn terms(query: &str) -> Vec<Vec<char>> {
    let mut out: Vec<Vec<char>> = Vec::new();
    for w in query.split(|ch: char| !ch.is_alphanumeric()).filter(|w| !w.is_empty()) {
        let t = fold(w);
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// `max` chars of `text` around char `at`, with `…` where it was cut.
fn snippet(text: &str, at: usize, max: usize) -> (String, bool) {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return (text.to_string(), false);
    }
    let start = at.saturating_sub(max / 4).min(chars.len() - max);
    let mut s: String = chars[start..start + max].iter().collect();
    if start > 0 {
        s.insert(0, '…');
    }
    if start + max < chars.len() {
        s.push('…');
    }
    (s, true)
}

fn search(args: &[String]) -> Result<i32> {
    let a = parse("search", args, &["--env", "--cauldron", "-c", "--pool", "--region", "--type", "--limit", "--max-chars"])?;
    let query = a.pos.join(" ");
    let wanted = terms(&query);
    if wanted.is_empty() {
        bail!("`cauldron soup search` needs a query with at least one word");
    }
    let (e, c) = target("search", &a)?;
    let pool = pool_name(&a);
    let handle = read_pool(&c, &pool)?;
    let snap = handle.snapshot().map_err(|err| anyhow::anyhow!("{err}"))?;
    let types = types_of(&handle, a.node_type.as_deref())?;
    let region = region_filter(&snap, a.region.as_deref())?;
    let phrase: Vec<char> = wanted.join(&' ');
    // (matched terms, score, catalog position, entry, first body hit)
    let mut hits = Vec::new();
    for (pos, n) in catalog(&c, &pool, &snap, &types)?.into_iter().enumerate() {
        if region.as_ref().is_some_and(|r| !n.regions.contains(r)) {
            continue;
        }
        let (h, b) = (fold(&n.handle), fold(&n.body));
        let (mut matched, mut score, mut first) = (0usize, 0usize, None::<usize>);
        for t in &wanted {
            let in_h = find(&h, t).is_some();
            let in_b = find(&b, t);
            if in_h || in_b.is_some() {
                matched += 1;
            }
            score += 3 * usize::from(in_h) + usize::from(in_b.is_some());
            if let Some(at) = in_b {
                first = Some(first.map_or(at, |f: usize| f.min(at)));
            }
        }
        if matched == 0 {
            continue;
        }
        if wanted.len() > 1 && (find(&h, &phrase).is_some() || find(&b, &phrase).is_some()) {
            score += 2;
        }
        hits.push((matched, 10 * matched + score, pos, n, first));
    }
    // Most terms matched, then the score, then newest (catalog order).
    hits.sort_by(|x, y| y.0.cmp(&x.0).then(y.1.cmp(&x.1)).then(x.2.cmp(&y.2)));
    let total = hits.len();
    let (shown, more) = bounded(hits, a.limit.unwrap_or(SEARCH_LIMIT));
    let max = a.max_chars.unwrap_or(SNIPPET_CHARS);
    let mut cut = Cut::new(a.max_chars);
    let list: Vec<Value> = shown
        .into_iter()
        .map(|(_, score, _, n, first)| {
            let (snip, partial) = snippet(&n.body, first.unwrap_or(0), max);
            if partial {
                cut.cut.push(json!({"id": n.id, "field": "body", "chars": n.body.chars().count(), "as": "snippet"}));
            }
            json!({
                "id": n.id,
                "type": n.node_type,
                "state": n.state,
                "handle": cut.value(&n.id, "handle", &json!(n.handle)),
                "regions": n.regions,
                "score": score,
                "snippet": snip,
            })
        })
        .collect();
    let mut doc = json!({
        "env": e.name, "cauldron": c.name, "pool": pool.as_str(), "query": query,
        "terms": wanted.iter().map(|t| t.iter().collect::<String>()).collect::<Vec<_>>(),
        "total": total, "results": list,
    });
    if more > 0 {
        doc["more"] = json!(more);
    }
    cut.report(&mut doc);
    print(&doc)?;
    Ok(0)
}

fn region(args: &[String]) -> Result<i32> {
    let a = parse("region", args, &["--env", "--cauldron", "-c", "--pool", "--depth", "--limit", "--max-chars"])?;
    let [id] = a.pos.as_slice() else {
        bail!("`cauldron soup region` takes ONE region id (got {}); `cauldron soup regions` lists them", a.pos.len());
    };
    let (e, c) = target("region", &a)?;
    let pool = pool_name(&a);
    let handle = read_pool(&c, &pool)?;
    let snap = handle.snapshot().map_err(|err| anyhow::anyhow!("{err}"))?;
    let id = region_filter(&snap, Some(id))?.expect("asked");
    let err = |e: spine::error::SpineError| anyhow::anyhow!("{e}");
    let mut cut = Cut::new(a.max_chars);
    let depth = a.depth.unwrap_or(1);
    let qualified = |id: &str| QualifiedNodeId { pool: pool.clone(), id: NodeId(id.to_string()) };
    let members_of = |id: &str| -> Result<usize> { Ok(snap.edges_to(&qualified(id), Some("in_region")).map_err(err)?.len()) };

    // A neighbouring region, short: id, handle, member count.
    let brief = |cut: &mut Cut, rid: &str, extra: Option<usize>| -> Result<Value> {
        let h = snap
            .node(&NodeId(rid.to_string()))
            .map_err(err)?
            .and_then(|n| n.fields.get("handle").map(|f| f.value.clone()))
            .unwrap_or(Value::Null);
        let mut v = json!({"id": rid, "handle": cut.value(rid, "handle", &h), "members": members_of(rid)?});
        if let Some(d) = extra {
            v["depth"] = json!(d);
        }
        Ok(v)
    };

    // Up the narrows graph (parents) and down it (children), breadth first,
    // each region once, to `depth` levels.
    let walk = |cut: &mut Cut, up: bool| -> Result<Vec<Value>> {
        let mut seen = std::collections::BTreeSet::from([id.clone()]);
        let mut frontier = vec![id.clone()];
        let mut out = Vec::new();
        for level in 1..=depth {
            let mut next = Vec::new();
            for r in &frontier {
                let mut far: Vec<String> = if up {
                    snap.edges_from(&NodeId(r.clone()), Some("narrows")).map_err(err)?.iter().map(|x| target_text(&pool, &x.dst)).collect()
                } else {
                    snap.edges_to(&qualified(r), Some("narrows")).map_err(err)?.iter().map(|x| x.src.0.clone()).collect()
                };
                far.sort();
                for f in far {
                    if seen.insert(f.clone()) {
                        out.push(brief(cut, &f, Some(level))?);
                        next.push(f);
                    }
                }
            }
            frontier = next;
        }
        Ok(out)
    };
    let parents = walk(&mut cut, true)?;
    let children = walk(&mut cut, false)?;

    let mut relates: Vec<String> = snap
        .edges_from(&NodeId(id.clone()), Some("relates"))
        .map_err(err)?
        .iter()
        .map(|x| target_text(&pool, &x.dst))
        .chain(snap.edges_to(&qualified(&id), Some("relates")).map_err(err)?.iter().map(|x| x.src.0.clone()))
        .collect();
    relates.sort();
    relates.dedup();
    let relates = relates.iter().map(|r| brief(&mut cut, r, None)).collect::<Result<Vec<_>>>()?;

    let r = snap.node(&NodeId(id.clone())).map_err(err)?.expect("checked above");
    let mut flags = serde_json::Map::new();
    for k in REGION_FLAGS {
        if let Some(f) = r.fields.get(k) {
            flags.insert(k.to_string(), f.value.clone());
        }
    }
    let field = |cut: &mut Cut, k: &str| r.fields.get(k).map(|f| cut.value(&id, k, &f.value));
    let record = json!({
        "id": id,
        "handle": field(&mut cut, "handle"),
        "boundary": field(&mut cut, "boundary"),
        "flags": flags,
    });

    let types = types_of(&handle, None)?;
    let members: Vec<Entry> = catalog(&c, &pool, &snap, &types)?.into_iter().filter(|n| n.regions.contains(&id)).collect();
    let total = members.len();
    let (shown, more) = bounded(members, a.limit.unwrap_or(MEMBERS_LIMIT));
    let members: Vec<Value> = shown
        .iter()
        .map(|n| json!({"id": n.id, "type": n.node_type, "state": n.state, "handle": cut.value(&n.id, "handle", &json!(n.handle))}))
        .collect();
    let mut doc = json!({
        "env": e.name, "cauldron": c.name, "pool": pool.as_str(),
        "region": record, "parents": parents, "children": children, "relates": relates,
        "members_total": total, "members": members,
    });
    if more > 0 {
        doc["more"] = json!(more);
    }
    cut.report(&mut doc);
    print(&doc)?;
    Ok(0)
}
