//! **The import**: records written in the PUBLISHED shape
//! (`schema/soup/workspace-v6/`) landed as ONE write, all or nothing, after
//! the spine's own door has judged every one (`spine::migrate::records`).
//!
//! # How a real import opens the store, and why that is safe
//!
//! The spine's only production open is `spine::Cauldron::open`, which runs
//! the recovery pass and the tranche pass before it returns, and those WRITE
//! (roll forward, abort, sweep orphan locks, reset tranche deadlines). So an
//! import opens only a soup that is (1) held by no running soup daemon and
//! no other writer and (2) AT REST, so those passes have nothing to do and
//! the import's commit is the only write:
//!
//! 1. no live `soup` seam ([`crate::lock::refuse_live_soup`]), then the
//!    soup's writer lock taken exclusive and held to the end;
//! 2. `spine::offline::rest_check` finds nothing un-settled;
//! 3. not a batch that already landed ([`landed_before`]);
//! 4. only then `Cauldron::open`, whose own report must show it did nothing.
//!
//! A dry run never opens the live store with the writing open: each `*.db`
//! file is copied from a read-only connection into a private temporary
//! directory (`spine::offline::snapshot_copy`) and judged there.
//!
//! # A batch that already landed
//!
//! Every import writes its batch's content digest ([`batch_digest`]) into
//! its own transaction row's provenance, the END of the `dispatch` text of
//! its `agent` initiator ([`batch_mark`]). Before it writes, an import looks
//! for a DECIDED row carrying the same mark and, finding one, refuses as
//! `batch-already-landed` unless `allow_duplicate`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use spine::migrate::records::{import_records, RecordsOptions};
use spine::migrate::{Landing, MigrationPlan};
use spine::schema_def::PoolName;

use crate::error::chain;
use crate::lock::{refuse_live_soup, take_writer_lock};
use crate::reads::DEFAULT_POOL;
use crate::target::Target;
use crate::{refuse, Code, Error, Result};

/// How to import.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// The pool, already checked with [`crate::reads::checked_pool`]; `ws`
    /// when `None`.
    pub pool: Option<String>,
    /// Judge every record against a read-only snapshot; land nothing.
    pub dry_run: bool,
    /// Treat ids as batch-local and give each record the next free id.
    pub remint: bool,
    /// Land a batch even when the same batch already landed.
    pub allow_duplicate: bool,
    /// Who writes: the `thread` of the `agent` initiator.
    pub agent: String,
    /// What writes: the start of the `dispatch` text of the `agent`
    /// initiator (the batch's mark is appended).
    pub dispatch: String,
}

impl ImportOptions {
    pub fn pool_name(&self) -> PoolName {
        PoolName::new(self.pool.as_deref().unwrap_or(DEFAULT_POOL))
    }
}

/// What an import came to.
#[derive(Debug, Clone)]
pub enum ImportOutcome {
    /// The door judged the batch; `doc` says how (`outcome` is `validated`,
    /// `landed`, `refused` or `empty`) and `ok` is true for `validated` and
    /// `landed`.
    Judged { doc: Value, ok: bool },
    /// Refused before judging: the same batch already landed. `why` is the
    /// sentence `doc` also carries.
    AlreadyLanded { doc: Value, why: String },
}

fn store(err: impl std::fmt::Display) -> Error {
    Error::new(Code::Store, err.to_string())
}

/// Import `records` into the target's soup (see the module docs for every
/// proof a real import makes first).
pub fn import(t: &Target, records: &[Value], o: &ImportOptions) -> Result<ImportOutcome> {
    // Before any lock or proof: a batch carrying a review flag is refused
    // whatever holds the soup (`submit` checks again).
    crate::review::refuse_review_flags(records)?;
    let (e, c) = (&t.env, &t.cauldron);
    let soup = c.soup();
    let pool = o.pool_name();
    let digest = batch_digest(&pool, records);

    let _copy;
    let _writer_lock;
    let cauldron = if o.dry_run {
        let dir = ScratchDir::new()?;
        spine::offline::snapshot_copy(&soup, &dir.0).map_err(|err| {
            store(format!("cannot take a read-only snapshot of cauldron `{}`'s soup to judge the dry run on: {err}", c.name))
        })?;
        let opened = spine::Cauldron::open(&dir.0, spine::config::SpineConfig::default())
            .map_err(|err| store(format!("opening the snapshot of cauldron `{}`'s soup: {err}", c.name)))?;
        // The same duplicate-batch judgement the real import makes, on the copy.
        if !o.allow_duplicate {
            let earlier = landed_before(&dir.0, &c.name, &digest)?;
            if !earlier.is_empty() {
                return Ok(already_landed(t, &pool, &digest, o, records.len(), earlier));
            }
        }
        _copy = dir;
        opened
    } else {
        // 1. No running soup daemon may hold it, provably; and no other
        //    writer may be writing it (held to the end of the import).
        refuse_live_soup(e)?;
        _writer_lock = take_writer_lock(c, "import")?;
        // 2. At rest, so the open-time passes have nothing to do.
        let unrest = spine::offline::rest_check(&soup).map_err(|err| {
            store(format!("REFUSED: cannot read {}'s spine, so cannot prove it is at rest: {err}", c.name))
        })?;
        if !unrest.is_empty() {
            refuse!(
                Code::NotAtRest,
                "REFUSED: cauldron `{}`'s soup is not at rest, and an outside import writes only to a soup at rest: {}. \
                 An import that was interrupted (killed, or its machine stopped) leaves a soup like this. \
                 Run `cauldron soup recover --env {} --cauldron {}` first and read what it prints: it either \
                 FINISHES the interrupted import (it landed in full: do NOT import that batch again; under \
                 --remint every record would land twice) or ROLLS IT BACK (none of it landed: importing it again is safe). \
                 Nothing was written.",
                c.name,
                unrest.join("; "),
                e.name,
                c.name
            );
        }
        // 3. Not a batch that already landed (unless allowed).
        if !o.allow_duplicate {
            let earlier = landed_before(&soup, &c.name, &digest)?;
            if !earlier.is_empty() {
                return Ok(already_landed(t, &pool, &digest, o, records.len(), earlier));
            }
        }
        // 4. The spine's own open, checked: it must have done nothing.
        let mut cauldron = spine::Cauldron::open(&soup, spine::config::SpineConfig::default())
            .map_err(|err| store(format!("opening cauldron `{}`'s soup: {err}", c.name)))?;
        // A region a ripple holds is refused here as at the door
        // (`region_lock`).
        cauldron.set_region_guard(crate::region_lock::Guard::new(c));
        let rec = cauldron.recovered_at_open();
        if !rec.rolled_forward.is_empty() || !rec.cleared_only.is_empty() || !rec.aborted.is_empty() || !cauldron.tranches_at_open().tranches.is_empty() {
            refuse!(
                Code::NotAtRest,
                "REFUSED: another writer touched cauldron `{}`'s soup between the rest check and the open (the open \
                 recovered work); nothing was imported. Try again once it is quiet.",
                c.name
            );
        }
        cauldron
    };
    let (doc, ok) = submit(&cauldron, t, records, o, &digest)?;
    Ok(ImportOutcome::Judged { doc, ok })
}

/// **Judge and (unless a dry run) land `records`** through `cauldron`, a
/// handle the caller opened under the proofs above, as ONE statement under
/// one spine row. Answers the import's JSON and whether it is `validated`
/// or `landed`. No duplicate-batch check is made here.
pub fn submit(cauldron: &spine::Cauldron, t: &Target, records: &[Value], o: &ImportOptions, digest: &str) -> Result<(Value, bool)> {
    // The review flags are the owner's: no record write sets or clears one.
    crate::review::refuse_review_flags(records)?;
    let (e, c) = (&t.env, &t.cauldron);
    let mut plan = MigrationPlan::new(spine::envelope::Initiator::Agent {
        thread: o.agent.clone(),
        dispatch: format!("{} [{}]", o.dispatch, batch_mark(digest, records.len())),
    });
    // Two imports of the same text are two writes, never one replayed.
    plan.idempotency_nonce = Some(format!(
        "soup-import:{}:{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    ));
    let opts = RecordsOptions { pool: o.pool_name(), remint: o.remint, commit: !o.dry_run };
    let run = import_records(cauldron, &plan, records, &opts).map_err(crate::region_lock::spine_error)?;

    let accepted = run.records.iter().filter(|r| r.disposition == "accepted").count();
    let refused = run.records.len() - accepted;
    let (outcome, landed, landing_refusal) = match &run.landing {
        None if o.dry_run && refused == 0 => ("validated", Value::Null, Value::Null),
        None => ("refused", Value::Null, Value::Null),
        Some(Landing::Committed { tx, seq, deduplicated, .. }) => {
            ("landed", json!({"tx": tx.to_hex(), "seq": seq, "deduplicated": deduplicated}), Value::Null)
        }
        Some(Landing::NothingAccepted) => ("empty", Value::Null, Value::Null),
        Some(Landing::Refused { witnesses }) => ("refused", Value::Null, to_value(witnesses)?),
    };
    // The re-minted ids mean something only for a batch that landed or
    // would land as it stands: on any other outcome they were never taken,
    // and the next import may give them to other records, so neither the
    // map nor a record's `new_id` is printed.
    let ids_hold = outcome == "landed" || outcome == "validated";
    let mut records_doc = to_value(&run.records)?;
    if !ids_hold {
        for r in records_doc.as_array_mut().into_iter().flatten() {
            if let Some(m) = r.as_object_mut() {
                m.remove("new_id");
            }
        }
    }
    let mut doc = json!({
        "env": e.name,
        "cauldron": c.name,
        "pool": opts.pool.as_str(),
        "dry_run": o.dry_run,
        "remint": o.remint,
        "judged_on": if o.dry_run { "a read-only snapshot of the soup, taken now; nothing was written" } else { "the soup" },
        "outcome": outcome,
        "accepted": accepted,
        "refused": refused,
        "records": records_doc,
        "batch_digest": digest,
    });
    if o.remint && ids_hold {
        doc["id_map"] = to_value(&run.id_map)?;
    }
    if !landed.is_null() {
        doc["landed"] = landed;
    }
    if !landing_refusal.is_null() {
        doc["landing_refused"] = landing_refusal;
    }
    // Every refused batch says why in one word, in the same place:
    // `records-refused` (the records name their reasons), `landing-refused`
    // (the store refused the write: `landing_refused`) or, from
    // [`already_landed`], `batch-already-landed`.
    if outcome == "refused" {
        doc["refusal"] = json!(if doc.get("landing_refused").is_some() { "landing-refused" } else { "records-refused" });
    }
    Ok((doc, outcome == "landed" || outcome == "validated"))
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value> {
    serde_json::to_value(v).map_err(|err| Error::new(Code::Store, chain(&err)))
}

// ------------------------------------------------- a batch that already landed

/// **The batch's content digest**: SHA-256 over the pool name and the
/// records exactly as read (before any remint), each object's keys sorted,
/// so the same records give the same digest whether they came as one JSON
/// file or a directory of YAML files.
pub fn batch_digest(pool: &PoolName, records: &[Value]) -> String {
    use sha2::{Digest, Sha256};
    fn canonical(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let sorted: BTreeMap<&String, Value> = m.iter().map(|(k, v)| (k, canonical(v))).collect();
                Value::Object(sorted.into_iter().map(|(k, v)| (k.clone(), v)).collect())
            }
            Value::Array(a) => Value::Array(a.iter().map(canonical).collect()),
            other => other.clone(),
        }
    }
    let mut h = Sha256::new();
    h.update(pool.as_str().as_bytes());
    h.update([0u8]);
    h.update(serde_json::to_string(&canonical(&Value::Array(records.to_vec()))).unwrap_or_default().as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The mark an import writes into its transaction row's provenance (the end
/// of the `dispatch` text of its `agent` initiator). No new table or
/// column: a store from an earlier release opens unchanged, and its earlier
/// imports simply carry no mark.
pub fn batch_mark(digest: &str, records: usize) -> String {
    format!("batch sha256:{digest}, {records} records")
}

/// Every earlier landing of the batch whose digest is `digest` in the spine
/// under `dir` (read-only): a DECIDED transaction row (one that landed, or
/// that recovery finished) whose `dispatch` text ENDS with this batch's
/// mark. Each as `{seq, landed_at, records}`, oldest first. `cauldron` is
/// only for the message.
pub fn landed_before(dir: &Path, cauldron: &str, digest: &str) -> Result<Vec<Value>> {
    let conn = spine::offline::read_only_connection(&dir.join(spine::spine_db::SPINE_FILE)).map_err(|err| {
        store(format!("REFUSED: cannot read {cauldron}'s spine to check for an earlier landing of this batch: {err}"))
    })?;
    let rows_err = |err: &dyn std::error::Error| Error::caused(Code::Store, "reading the spine's transaction rows", err);
    let needle = format!("batch sha256:{digest}, ");
    let mut stmt = conn
        .prepare("SELECT seq, ts, initiator FROM transactions WHERE decided = 1 AND instr(initiator, ?1) > 0 ORDER BY seq")
        .map_err(|e| rows_err(&e))?;
    let rows = stmt
        .query_map([&needle], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
        .map_err(|e| rows_err(&e))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| rows_err(&e))?;
    // The SQL only narrows the rows; the match is made here, on the mark an
    // import itself writes: the END of the `dispatch` text, exactly
    // ` [batch sha256:<digest>, <n> records]`. A mark copied into an agent
    // label (`thread`) or into a file name earlier in `dispatch` never counts.
    Ok(rows
        .into_iter()
        .filter_map(|(seq, ts, initiator)| {
            let v: Value = serde_json::from_str(&initiator).ok()?;
            let dispatch = v.get("dispatch")?.as_str()?;
            let (_, tail) = dispatch.rsplit_once(" [batch sha256:")?;
            let n = tail.strip_prefix(digest)?.strip_prefix(", ")?.strip_suffix(" records]")?;
            let records = (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).then(|| n.parse::<u64>().ok()).flatten()?;
            Some(json!({"seq": seq, "landed_at": ts, "records": records}))
        })
        .collect())
}

/// The refusal `batch-already-landed` for `earlier` (non-empty, from
/// [`landed_before`]) of a batch of `records` records: its JSON and its
/// sentence. The JSON has the shape of every refused batch (see
/// [`submit`]): `outcome`, `refusal`, `accepted`, `refused` and `records`
/// (empty: no record was judged), plus `landed_before` and `why`.
pub fn already_landed(t: &Target, pool: &PoolName, digest: &str, o: &ImportOptions, records: usize, earlier: Vec<Value>) -> ImportOutcome {
    let first = &earlier[0];
    let why = format!(
        "this exact batch (the same records, before any --remint, into pool `{pool}`) already landed in cauldron `{}` \
         at {} as spine transaction {} ({} records){}. Under --remint, importing it again would land every \
         record a second time under new ids, and this release cannot delete nodes; without --remint, its carried \
         ids would collide and the import would be refused. Nothing was written. If a second copy is really wanted, run the same \
         command with --allow-duplicate.",
        t.cauldron.name,
        first["landed_at"].as_str().unwrap_or("?"),
        first["seq"],
        first["records"].as_u64().map(|n| n.to_string()).unwrap_or_else(|| "?".to_string()),
        if earlier.len() > 1 { format!(", and {} more time(s) after that", earlier.len() - 1) } else { String::new() },
    );
    let doc = json!({
        "env": t.env.name,
        "cauldron": t.cauldron.name,
        "pool": pool.as_str(),
        "dry_run": o.dry_run,
        "remint": o.remint,
        "outcome": "refused",
        "refusal": "batch-already-landed",
        "accepted": 0,
        "refused": records,
        "records": [],
        "batch_digest": digest,
        "landed_before": earlier,
        "why": why,
    });
    ImportOutcome::AlreadyLanded { doc, why }
}

// ------------------------------------------------------------ reading input

/// Records from one file (a list, or one record) or a directory of
/// one-record files (`*.json`, `*.yaml`, `*.yml`, in name order).
pub fn read_records(path: &Path) -> Result<Vec<Value>> {
    if path.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(|err| Error::caused(Code::Usage, format!("listing {}", path.display()), &err))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| matches!(p.extension().and_then(|x| x.to_str()), Some("json" | "yaml" | "yml")))
            .collect();
        files.sort();
        if files.is_empty() {
            refuse!(Code::Usage, "{} holds no .json, .yaml or .yml record files", path.display());
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
    let text = std::fs::read_to_string(path).map_err(|err| Error::caused(Code::Usage, format!("reading {}", path.display()), &err))?;
    match path.extension().and_then(|x| x.to_str()) {
        Some("yaml" | "yml") => serde_yaml::from_str::<Value>(&text)
            .map_err(|err| Error::new(Code::Usage, format!("{} is not YAML: {}", path.display(), chain(&err)))),
        _ => serde_json::from_str::<Value>(&text)
            .map_err(|err| Error::new(Code::Usage, format!("{} is not JSON: {}", path.display(), chain(&err)))),
    }
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
        // The copy of the soup is the owner's only, like the soup.
        env::private::make_dir(&p).map_err(|err| Error::caused(Code::Store, format!("creating {}", p.display()), &err))?;
        Ok(ScratchDir(p))
    }
}
impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
