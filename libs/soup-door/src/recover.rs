//! **Recover**: settle a soup an interrupted import left not at rest.
//!
//! An import killed part way (Ctrl-C, a crash, a machine that stopped)
//! leaves its transaction row un-cleared and its locks held; the spine's own
//! recovery pass, which `spine::Cauldron::open` runs, settles that from the
//! row alone: a row that was never decided is ABORTED (nothing of it
//! landed, nothing lands), a decided row is ROLLED FORWARD (all of it lands,
//! exactly once), and orphaned locks are released.
//!
//! It runs that open only when it can prove no other process holds the
//! store: no live soup daemon in the environment, and the soup's writer
//! lock taken exclusive. A soup already at rest is not opened at all.

use std::path::Path;

use serde_json::{json, Value};

use crate::lock::{refuse_live_soup, take_writer_lock};
use crate::reads::{prefix, serial};
use crate::target::Target;
use crate::{Code, Error, Result};

/// What a recover came to: its JSON, and whether the soup is now at rest.
#[derive(Debug, Clone)]
pub struct RecoverReport {
    pub doc: Value,
    pub settled: bool,
}

fn store(err: impl std::fmt::Display) -> Error {
    Error::new(Code::Store, err.to_string())
}

/// Recover the target's soup. `doc["interrupted_import"]` is `landed` (with
/// how many records and their ids), `rolled-back`, `landed-and-rolled-back`
/// or `none`; `doc["instruction"]` says in words what to do next.
pub fn recover(t: &Target) -> Result<RecoverReport> {
    let (e, c) = (&t.env, &t.cauldron);
    let soup = c.soup();
    refuse_live_soup(e)?;
    let _lock = take_writer_lock(c, "recover")?;
    let before = spine::offline::rest_check(&soup).map_err(|err| store(format!("REFUSED: cannot read {}'s spine: {err}", c.name)))?;
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
        return Ok(RecoverReport { doc, settled: true });
    }
    let cauldron = spine::Cauldron::open(&soup, spine::config::SpineConfig::default())
        .map_err(|err| store(format!("recovering cauldron `{}`'s soup: {err}", c.name)))?;
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
    let after = spine::offline::rest_check(&soup).map_err(|err| store(format!("cannot read {}'s spine after recovery: {err}", c.name)))?;
    let settled = after.is_empty();
    doc["after"] = json!(after);
    doc["outcome"] = json!(if settled { "recovered" } else { "not-at-rest" });

    let landed = landed_nodes(&soup, &landed_tx).map_err(|err| store(format!("cannot read which records landed in {}'s soup: {err}", c.name)))?;
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
             Do NOT import that batch again: under --remint every record would land a second time under new \
             ids, and this release cannot delete nodes. An import run with --remint printed no id map; `{nodes_cmd}` lists the landed nodes \
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
    Ok(RecoverReport { doc, settled })
}

/// The human text of a recover result: the outcome and its instruction.
pub fn say(doc: &Value) -> String {
    format!(
        "{} (interrupted import: {})\n{}",
        doc["outcome"].as_str().unwrap_or("?"),
        doc["interrupted_import"].as_str().unwrap_or("?"),
        doc["instruction"].as_str().unwrap_or("")
    )
}

/// Every node, `(pool, id)`, that one of the transactions `tx` (hex hashes)
/// created, read through read-only connections to each pool file of the
/// soup at `soup`; sorted by pool, id prefix, then serial.
pub fn landed_nodes(soup: &Path, tx: &[String]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    if tx.is_empty() {
        return Ok(out);
    }
    let listing = |err: std::io::Error| Error::caused(Code::Store, format!("listing {}", soup.display()), &err);
    let sql = |err: &dyn std::error::Error| Error::new(Code::Store, err.to_string());
    for entry in std::fs::read_dir(soup).map_err(listing)? {
        let p = entry.map_err(listing)?.path();
        let stem = p.file_stem().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if p.extension().and_then(|x| x.to_str()) != Some("db") || stem == "spine" {
            continue;
        }
        let conn = spine::offline::read_only_connection(&p).map_err(store)?;
        let mut stmt = conn.prepare("SELECT id FROM nodes WHERE created_tx = ?1").map_err(|e| sql(&e))?;
        for h in tx {
            for id in stmt.query_map([h], |r| r.get::<_, String>(0)).map_err(|e| sql(&e))? {
                out.push((stem.clone(), id.map_err(|e| sql(&e))?));
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

/// The JSON form of the landed groups.
pub fn landed_summary(landed: &[(String, String)]) -> Value {
    Value::Array(
        landed_groups(landed)
            .into_iter()
            .map(|(pool, prefix, count, first, last, contiguous)| {
                json!({"pool": pool, "prefix": prefix, "count": count, "first": first, "last": last, "contiguous": contiguous})
            })
            .collect(),
    )
}

/// The text form of the landed groups: `IS-2..IS-5001 (5000) in ws`.
pub fn id_ranges_text(landed: &[(String, String)]) -> String {
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
