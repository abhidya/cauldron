//! **`soup_changes`**: what landed in a soup since a cursor, read through
//! connections that cannot write (never through the writer thread).
//!
//! The stream is the spine's committed-commit counter: every write that
//! lands bumps it by one and stamps its transaction row with the new value
//! (`committed_at`). A cursor (`v1.<soup instance>.<counter>`, see
//! [`crate::cursor`]) names a point in that stream; the changes after it are
//! the rows whose `committed_at` is above it, oldest first.
//!
//! Each change says what the write did, from the pool files' own `*_tx`
//! columns: `create` (it made nodes; `ids` are those nodes), `link` (it made
//! edges between nodes that already existed; `ids` are the edges' sources)
//! or `update` (it changed nodes it did not make). At most
//! [`IDS_PER_CHANGE`] ids are listed per change; `count` is how many there
//! were.
//!
//! Refusals: a cursor that is not this door's form, or that belongs to
//! another cauldron (or to one removed and made again under the same name),
//! is `cursor_unknown`; one whose counter is above the soup's is
//! `cursor_stale`. With no `since`, the answer is the current cursor and no
//! changes: never a dump of the soup.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{json, Map, Value};
use shapewire::envelope::Refusal;
use soup_door::Target;

use crate::cursor;

/// How many changes one call answers when it does not say.
pub const DEFAULT_LIMIT: usize = 100;
/// The most one call answers, whatever it asks.
pub const MAX_LIMIT: usize = 1000;
/// The most ids listed for one change.
pub const IDS_PER_CHANGE: usize = 50;

/// The hint a cut list carries: how to read on from where it stopped.
pub const NEXT: &str = "more changes landed after these: call soup_changes again with `since` set to this answer's `cursor`";

fn store(t: &Target, e: &dyn std::fmt::Display) -> Refusal {
    Refusal::new("store", format!("reading cauldron `{}`'s change stream: {e}", t.cauldron.name))
}

fn unknown(t: &Target, since: &str, why: &str) -> Refusal {
    Refusal::new(
        "cursor_unknown",
        format!(
            "`{since}` is not a cursor of cauldron `{}` as it stands ({why}). Nothing was read. Call soup_changes \
             without `since` (or soup_status) for the current cursor, and read the soup itself for what is \
             already there.",
            t.cauldron.name
        ),
    )
}

/// The counter `since` names, checked against this cauldron and its head.
pub fn parse_since(t: &Target, since: &str, head: u64) -> Result<u64, Refusal> {
    let mut parts = since.trim().splitn(3, '.');
    let (Some("v1"), Some(inst), Some(n)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(unknown(t, since, "a cursor has the form v1.<instance>.<counter>"));
    };
    if inst != cursor::instance(&t.cauldron) {
        return Err(unknown(
            t,
            since,
            "it was answered by another cauldron, or by one of the same name that has since been removed and made again",
        ));
    }
    let n: u64 = n.parse().map_err(|_| unknown(t, since, "its counter is not a number"))?;
    if n > head {
        return Err(Refusal::new(
            "cursor_stale",
            format!(
                "`{since}` is ahead of cauldron `{}`'s soup, whose cursor is `{}`: the soup has not seen that many \
                 writes. Nothing was read. Call soup_changes without `since` for the current cursor.",
                t.cauldron.name,
                cursor::cursor(&t.cauldron, head)
            ),
        ));
    }
    Ok(n)
}

/// One transaction row of the stream.
struct Row {
    counter: u64,
    hash: String,
    at: String,
    by: Option<String>,
}

/// The answer: `{env, cauldron, since, cursor, head, changes, more}` (+
/// `next` when the list stopped short). `cursor` is where to read on from.
pub fn changes(t: &Target, since: Option<&str>, limit: Option<usize>) -> Result<Value, Refusal> {
    let soup = t.soup();
    let head = cursor::counter_offline(&soup).map_err(|e| store(t, &e))?;
    let head_cursor = cursor::cursor(&t.cauldron, head);
    let Some(since) = since else {
        return Ok(json!({
            "env": t.env.name,
            "cauldron": t.cauldron.name,
            "since": Value::Null,
            "cursor": head_cursor,
            "head": head_cursor,
            "changes": [],
            "more": false,
            "note": "no `since`: this is the current cursor only; pass it as `since` later to read what landed after it",
        }));
    };
    let from = parse_since(t, since, head)?;
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let rows = rows_after(&soup, from, head, limit + 1).map_err(|e| store(t, &e))?;
    let more = rows.len() > limit;
    let rows = &rows[..rows.len().min(limit)];
    let pools = pool_files(&soup).map_err(|e| store(t, &e))?;
    let mut list = Vec::new();
    for r in rows {
        list.push(change(&pools, r).map_err(|e| store(t, &e))?);
    }
    let cursor = match (more, rows.last()) {
        (true, Some(last)) => cursor::cursor(&t.cauldron, last.counter),
        _ => head_cursor.clone(),
    };
    let mut doc = json!({
        "env": t.env.name,
        "cauldron": t.cauldron.name,
        "since": since,
        "cursor": cursor,
        "head": head_cursor,
        "changes": list,
        "more": more,
    });
    if more {
        doc["next"] = json!(NEXT);
    }
    Ok(doc)
}

/// After a whole-reply bound cut the list: `cursor` is moved back to the
/// last change kept, so reading on from it misses nothing.
pub fn after_budget(t: &Target, doc: &mut Value) {
    let dropped = doc["truncated"]["dropped_from"]["changes"].as_u64().unwrap_or(0);
    if dropped == 0 {
        return;
    }
    let Some(last) = doc["changes"].as_array().and_then(|c| c.last()).and_then(|c| c["seq"].as_u64()) else { return };
    doc["cursor"] = json!(cursor::cursor(&t.cauldron, last));
    doc["more"] = json!(true);
    doc["next"] = json!(NEXT);
}

fn rows_after(soup: &Path, from: u64, head: u64, n: usize) -> Result<Vec<Row>, String> {
    let conn = spine::offline::read_only_connection(&soup.join(spine::spine_db::SPINE_FILE)).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT committed_at, hash, ts, initiator FROM transactions
             WHERE committed_at IS NOT NULL AND committed_at > ?1 AND committed_at <= ?2
             ORDER BY committed_at LIMIT ?3",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite_params(from, head, n), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        let (counter, hash, at, initiator) = row.map_err(|e| e.to_string())?;
        let by = serde_json::from_str::<Value>(&initiator).ok().and_then(|v| v.get("thread").and_then(Value::as_str).map(str::to_string));
        out.push(Row { counter: counter.max(0) as u64, hash, at, by });
    }
    Ok(out)
}

fn rusqlite_params(from: u64, head: u64, n: usize) -> [i64; 3] {
    [from as i64, head as i64, n as i64]
}

/// Every pool file of the soup (`<pool>.db`, not `spine.db`), by name.
fn pool_files(soup: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(soup).map_err(|e| format!("listing {}: {e}", soup.display()))? {
        let p = entry.map_err(|e| e.to_string())?.path();
        let stem = p.file_stem().and_then(|n| n.to_str()).unwrap_or("");
        if p.extension().and_then(|x| x.to_str()) == Some("db") && stem != "spine" {
            out.push(p);
        }
    }
    out.sort();
    Ok(out)
}

fn change(pools: &[std::path::PathBuf], r: &Row) -> Result<Value, String> {
    let mut created: Vec<Value> = Vec::new();
    let mut linked: BTreeSet<(usize, String)> = BTreeSet::new();
    let mut updated: Vec<Value> = Vec::new();
    let mut edges = 0usize;
    for (pi, p) in pools.iter().enumerate() {
        let conn = spine::offline::read_only_connection(p).map_err(|e| e.to_string())?;
        let node = |sql: &str| -> Result<Vec<Value>, String> {
            let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([&r.hash], |row| {
                    Ok(json!({"id": row.get::<_, String>(0)?, "type": row.get::<_, String>(1)?, "handle": handle(row.get::<_, Option<String>>(2)?)}))
                })
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
        };
        created.extend(node(
            "SELECT n.id, n.node_type, f.value FROM nodes n LEFT JOIN node_fields f ON f.node_id = n.id AND f.field = 'handle'
             WHERE n.created_tx = ?1 ORDER BY n.rowid",
        )?);
        updated.extend(node(
            "SELECT n.id, n.node_type, f.value FROM nodes n LEFT JOIN node_fields f ON f.node_id = n.id AND f.field = 'handle'
             WHERE n.updated_tx = ?1 AND n.created_tx != ?1 ORDER BY n.rowid",
        )?);
        let mut stmt = conn.prepare("SELECT src FROM edges WHERE created_tx = ?1").map_err(|e| e.to_string())?;
        for src in stmt.query_map([&r.hash], |row| row.get::<_, String>(0)).map_err(|e| e.to_string())? {
            edges += 1;
            linked.insert((pi, src.map_err(|e| e.to_string())?));
        }
    }
    let (op, ids): (&str, Vec<Value>) = if !created.is_empty() {
        ("create", created)
    } else if !linked.is_empty() {
        let mut v = Vec::new();
        for (pi, src) in &linked {
            let conn = spine::offline::read_only_connection(&pools[*pi]).map_err(|e| e.to_string())?;
            let found = conn
                .query_row(
                    "SELECT n.node_type, f.value FROM nodes n LEFT JOIN node_fields f ON f.node_id = n.id AND f.field = 'handle' WHERE n.id = ?1",
                    [src],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .ok();
            let (ty, h) = found.unwrap_or_default();
            v.push(json!({"id": src, "type": ty, "handle": handle(h)}));
        }
        ("link", v)
    } else {
        ("update", updated)
    };
    let count = ids.len();
    let mut c = Map::new();
    c.insert("seq".into(), json!(r.counter));
    c.insert("at".into(), json!(r.at));
    c.insert("op".into(), json!(op));
    c.insert("tx".into(), json!(r.hash));
    if let Some(by) = &r.by {
        c.insert("by".into(), json!(by));
    }
    c.insert("count".into(), json!(count));
    if edges > 0 {
        c.insert("edges".into(), json!(edges));
    }
    c.insert("ids".into(), Value::Array(ids.into_iter().take(IDS_PER_CHANGE).collect()));
    Ok(Value::Object(c))
}

/// A `handle` field's stored JSON text, as the string it holds.
fn handle(stored: Option<String>) -> Value {
    stored.and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null)
}
