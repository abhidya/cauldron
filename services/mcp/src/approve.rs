//! **The owner's approval of a ripple's records**: `cauldron soup approve`
//! at a terminal marks records a ripple wrote as reviewed (each unreviewed
//! record named gets `meta.approved: {at, by: "owner"}`), or with
//! `--unapprove` back to unreviewed (`meta.approved: null`), all in ONE
//! statement on the cauldron's writer.
//!
//! **Only the owner's command line can ask.** The request comes over this
//! service's seam as `APPROVE <key> {cauldron, ids, unapprove?}`, and the key
//! is the approval key this service mints into `approve.secret` at every
//! start ([`crate::secret::APPROVE_FILE`]). The command line reads it only
//! after checking that its input and output are an interactive terminal, as
//! `cauldron mcp grant` does. No MCP tool approves, so no session can; the
//! `ripples` service holds only the ripples key, which this verb refuses; and
//! the spine's flag rule refuses `meta.approved` to every ripple anyway. The
//! limit, as for every key here: a process of this user that reads the
//! private file can send the line. Every approval is audited in
//! `calls.jsonl` (client `cli`, tool `soup_approve` or `soup_unapprove`,
//! the counts and the transaction, never text).

use std::collections::BTreeMap;

use serde_json::{json, Value};
use shapewire::envelope::Refusal;
use soup_door::reads::{self, DEFAULT_POOL};
use soup_door::Target;
use spine::envelope::{Envelope, Initiator, Instrument, Mode, Provenance, ENVELOPE_FORMAT_VERSION};
use spine::pool::{NodeId, Pool};
use spine::schema_def::PoolName;

use crate::refusal;

fn store(e: impl std::fmt::Display) -> Refusal {
    Refusal::new("store", e.to_string())
}

/// Who an approval is written as.
pub const BY: &str = "owner";

/// Approve `ids`, or with `unapprove` take the approval back (run on the
/// writer thread).
pub fn approve(c: &spine::Cauldron, t: &Target, ids: &[String], unapprove: bool) -> Result<Value, Refusal> {
    let by = BY;
    let pool = PoolName::new(DEFAULT_POOL);
    let handle = Pool::open_read_only(&Pool::path_in(&t.soup(), &pool)).map_err(store)?;
    let snap = handle.snapshot().map_err(store)?;
    let ripples = reads::ripple_commits(&t.cauldron).map_err(refusal)?;
    let mut take: Vec<(String, String)> = Vec::new();
    let mut skipped = Vec::new();
    for id in ids {
        if take.iter().any(|(x, _)| x == id) {
            continue;
        }
        let Some(n) = snap.node(&NodeId(id.clone())).map_err(store)? else {
            skipped.push(json!({"id": id, "why": "no such node in this cauldron"}));
            continue;
        };
        let by_ripple = snap.created_tx(&n.id).map_err(store)?.is_some_and(|h| ripples.contains(&h.to_hex()));
        if !by_ripple {
            skipped.push(json!({"id": id, "why": "not written by a ripple: there is nothing to approve"}));
        } else if !unapprove && reads::approved(&n.fields) {
            skipped.push(json!({"id": id, "why": "already approved"}));
        } else if unapprove && !reads::approved(&n.fields) {
            skipped.push(json!({"id": id, "why": "not approved: it already reads as unreviewed"}));
        } else {
            take.push((id.clone(), n.node_type.clone()));
        }
    }
    let mut doc = json!({"env": t.env.name, "cauldron": t.cauldron.name, "pool": pool.as_str(), "skipped": skipped});
    let (key, word) = if unapprove { ("unapproved", "unapprove") } else { ("approved", "approve") };
    if take.is_empty() {
        doc[key] = json!([]);
        doc["outcome"] = json!(format!("nothing-to-{word}"));
        return Ok(doc);
    }
    let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut lines = Vec::new();
    let mut expects = Vec::new();
    let mut sets = Vec::new();
    let mut params: BTreeMap<String, Value> = BTreeMap::new();
    params.insert("approved".into(), if unapprove { Value::Null } else { json!({"at": at, "by": by}) });
    for (i, (id, ty)) in take.iter().enumerate() {
        lines.push(format!("MATCH (n{i}: {DEFAULT_POOL}/{ty} {{ id: $id_{i} }})"));
        expects.push(format!("EXPECT COUNT(n{i}) = 1"));
        sets.push(format!("SET n{i}.meta.approved = $approved"));
        params.insert(format!("id_{i}"), json!(id));
    }
    let env = Envelope {
        statement: lines.into_iter().chain(expects).chain(sets).collect::<Vec<_>>().join("\n"),
        bindings: params,
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent { thread: by.to_string(), dispatch: format!("cauldron soup {word}") },
            instrument: Instrument { name: "cauldron-mcp".into(), version: env!("CARGO_PKG_VERSION").into() },
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(format!(
            "owner-{word}:{}:{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        )),
    };
    match c.submit(env) {
        Ok(spine::Outcome::Committed { tx, seq, deduplicated, .. }) => {
            doc[key] = json!(take.iter().map(|(id, _)| id).collect::<Vec<_>>());
            doc["outcome"] = json!(key);
            doc["landed"] = json!({"tx": tx.to_hex(), "seq": seq, "deduplicated": deduplicated});
            doc["cursor"] = crate::writer::cursor_after(c, t);
            Ok(doc)
        }
        Ok(spine::Outcome::Validated { .. }) => Err(store(format!("the {word} was validated and not committed"))),
        Err(spine::error::SpineError::Refused { witnesses }) => {
            doc["witnesses"] = json!(witnesses);
            Err(Refusal::with_detail("records_refused", format!("the soup refused the {word}; nothing was written"), doc))
        }
        Err(e) => Err(store(format!("writing the {word}: {e}"))),
    }
}

/// **Answer one `APPROVE` line**: `rest` is `<key> <json>`. Always one JSON
/// line, never an error; every call after the key is audited.
pub fn answer(door: &crate::door::Door, key: &crate::secret::Secret, rest: &str) -> String {
    let fail = |r: &Refusal| {
        let mut v = json!({"ok": false, "code": r.code, "message": r.message});
        if let Some(d) = &r.detail {
            v["detail"] = d.clone();
        }
        v.to_string()
    };
    let (given, body) = rest.split_once(' ').unwrap_or((rest, ""));
    if given.is_empty() || !key.is(given) {
        tracing::warn!("an APPROVE line without the approval key was refused");
        return fail(&Refusal::new(
            "unauthorized",
            "approving is the owner's act: this verb answers only `cauldron soup approve`, run at a terminal, which \
             carries the key this service minted into its directory (approve.secret) at its last start. No MCP \
             session and no ripple can approve. Nothing was written.",
        ));
    }
    let args: serde_json::Map<String, Value> = match serde_json::from_str::<Value>(body.trim()) {
        Ok(Value::Object(m)) => m,
        _ => return fail(&Refusal::new("usage", "APPROVE takes one JSON object after the key. Nothing was written.")),
    };
    let cauldron = args.get("cauldron").and_then(Value::as_str).unwrap_or("").to_string();
    let unapprove = args.get("unapprove").and_then(Value::as_bool).unwrap_or(false);
    let out = (|| -> Result<Value, Refusal> {
        if cauldron.trim().is_empty() {
            return Err(Refusal::new("usage", "`cauldron` must be a non-empty string. Nothing was written."));
        }
        let ids: Vec<String> = match args.get("ids") {
            Some(Value::Array(a)) if !a.is_empty() && a.len() <= 500 && a.iter().all(|x| x.as_str().is_some_and(|s| !s.trim().is_empty())) => {
                a.iter().filter_map(Value::as_str).map(str::to_string).collect()
            }
            _ => return Err(Refusal::new("usage", "`ids` must be a list of 1 to 500 record ids. Nothing was written.")),
        };
        let t = door.owner_target(&cauldron)?;
        let who = crate::writer::Who { client: "cli".into(), thread: BY.into(), session: None };
        door.writers.with(&t, who, Box::new(move |c, t| approve(c, t, &ids, unapprove)))
    })();
    let mut line = crate::audit::Line {
        client: Some("cli".into()),
        cauldron: (!cauldron.is_empty()).then_some(cauldron),
        tool: if unapprove { "soup_unapprove".into() } else { "soup_approve".into() },
        ..crate::audit::Line::default()
    };
    match &out {
        Ok(v) => {
            line.outcome = v.get("outcome").and_then(Value::as_str).unwrap_or("ok").to_string();
            line.tx = v["landed"].get("tx").and_then(Value::as_str).map(str::to_string);
            let n = |k: &str| v.get(k).and_then(Value::as_array).map_or(0, Vec::len);
            line.counts = json!({"approved": n("approved"), "unapproved": n("unapproved"), "skipped": n("skipped")});
        }
        Err(r) => line.outcome = r.code.to_string(),
    }
    door.audit(line);
    match out {
        Ok(mut v) => {
            v["ok"] = json!(true);
            v.to_string()
        }
        Err(r) => fail(&r),
    }
}
