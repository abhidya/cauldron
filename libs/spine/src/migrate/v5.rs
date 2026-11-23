//! The v5 REGIONS pool → [`SourceItem`] adapter — the import BY
//! REGENERATION.
//!
//! It reads `<pool_dir>/nodes/*.yaml` (every `.yaml` file there, in file
//! name order; nothing else under `nodes/`) and `<pool_dir>/manifest.yaml`
//! (for `workspace.slug` and `viewers` ONLY), as the v5 workspace tooling writes
//! them, READ-ONLY (`std::fs::read`, nothing else), and yields:
//!
//! - per node file, one [`SourceItem::Node`] carrying the v5 id VERBATIM in
//!   [`SourceNode::carried_id`] (the `IMPORT` form lands `Q-65` as `Q-65`),
//!   then one [`SourceItem::Edge`] per entry of its `edges:` (duplicate local
//!   triples merged — see [`TRANSFORM_DUPLICATE_TRIPLE`]);
//! - then, per manifest viewer ROLE KEY in ascending order, one viewer node
//!   (store-minted `V-n`, field `role`) and one `shows` edge per listed node
//!   with its `{cite, why}` as the edge's declared fields.
//!
//! The mapping:
//! - `id` → `source_id` AND `carried_id`; `type` → `node_type`; `state`
//!   verbatim except a `grammar_v6::DROPPED_STATES` pair, which v6 computes
//!   from edges (transform [`TRANSFORM_DROP_COMPUTED_STATE`]);
//! - every other top-level key except the record keys below (`handle`,
//!   `body`, `boundary`, `wake_when`, `deferred_by`, `retired`,
//!   `provisional`, `thresholds`, and anything unknown) → the same-named
//!   field, verbatim, so the destination grammar judges it (over cap, out of
//!   vocabulary, undeclared → that item is REJECTED, never silently dropped);
//! - `provenance.operator` → the v6 field `operator`;
//! - `meta` → `meta`, verbatim, with the IMPORT RECORD added under
//!   `meta.imported_from` (the v5 migration's own record key):
//!   `{board, v5_id, origin: "<id>@<board>", rev, created, created_by,
//!   field_revs?, schema_version, prior?, admits_by_edge, unresolved_edges}`
//!   — `v5_id` the source id, `admits_by_edge` every `in_region` edge's v5
//!   `admits:` span keyed by destination id (the AUTHORITATIVE home of the
//!   span; the edge's `note` keeps a copy as its free text) —
//!   no import timestamp (it would enter the hash identity through the
//!   bindings; the spine row's own stamp is the import time);
//! - an edge whose `to` is a node of THIS pool → an edge item, its one v5
//!   text attribute (`admits`/`sentence`/`note`/`label`) VERBATIM in the
//!   edge's `note`; two or more joined one per line `<key>: <value>`
//!   ([`TRANSFORM_EDGE_TEXT_JOINED`]);
//! - an edge whose `to` is anything else (`<thread>#<msg>`, `<id>@<board>`)
//!   → kept VERBATIM in `meta.imported_from.unresolved_edges`, AND yielded as
//!   an edge item whose `dst` is the raw text, so the driver disposes it as
//!   `endpoint-absent-from-source` and the manifest counts it. An `@board`
//!   suffix is NEVER stripped to resolve it: the frozen v3 board and this
//!   pool can share a name, and stripping makes `CON-24 derived_from
//!   CON-24` self-loops.
//! - `sealed` is never set (machine-owned; set only by the seal act).

use super::{NamedTransform, SourceEdge, SourceItem, SourceNode};
use crate::error::{Result, SpineError};
use crate::grammar_v6::{DROPPED_STATES, VIEWER, VIEWER_ROLE};
use crate::schema_def::PoolName;
use crate::Cauldron;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The meta key the import record rides (the v5 precedent's own key).
pub const RECORD_KEY: &str = "imported_from";
/// The record key holding every `in_region` edge's v5 `admits:` span by
/// destination id (see [`admits_by_edge`]).
pub const ADMITS_BY_EDGE: &str = "admits_by_edge";
/// A viewer item's source identity prefix (`viewer:<role>`).
pub const VIEWER_SOURCE_PREFIX: &str = "viewer:";
/// The v5 edge text attributes, in the fixed join order.
pub const EDGE_TEXT_KEYS: [&str; 5] = ["admits", "note", "sentence", "label", "cite"];

pub const TRANSFORM_RECORD: &str = "v5-record-fields-into-imported-from";
pub const TRANSFORM_DROP_COMPUTED_STATE: &str = "drop-computed-state";
pub const TRANSFORM_PRIOR_RECORD: &str = "prior-imported-from-nested";
pub const TRANSFORM_EDGE_TEXT_JOINED: &str = "edge-text-joined";
/// Two or more v5 edge entries of ONE node with the same `(kind, to)` toward
/// a node of this pool. The store's edge identity is the triple (`pool.rs`
/// `INSERT ... ON CONFLICT (src, kind, dst_pool, dst) DO NOTHING`), so a
/// second clause for one triple in the landing would store NOTHING and its
/// text would vanish without a name. The adapter yields ONE edge item for
/// the triple, its note every entry's text joined one per line (labelled),
/// and names the merge here (lossy: false).
pub const TRANSFORM_DUPLICATE_TRIPLE: &str = "edge-duplicate-triple-merged";
/// A v6 required-at-mint edge the v5 node NEVER carried. A v6-only required
/// edge the v5 node never had becomes a recorded ISSUE on import (noted in
/// meta.imported_from and the manifest), not a rejection. The node imports, its record carries
/// one [`ISSUES_KEY`] entry per gap, and this transform names it on the
/// node's manifest entry. Not lossy: nothing of the source is dropped — the
/// node is in a DEGRADED state, and the door accepts it only through IMPORT
/// (`schema_def`'s required-at-mint waiver).
pub const TRANSFORM_REQUIRED_EDGE_ISSUE: &str = "v6-required-edge-never-had";
/// The record key of the import issues (`schema_def::IMPORT_ISSUES_KEY`).
pub const ISSUES_KEY: &str = crate::schema_def::IMPORT_ISSUES_KEY;
pub const TRANSFORM_VIEWERS: &str = "viewers-from-manifest";
pub const TRANSFORM_VIEWER_NOTE_DROPPED: &str = "viewer-note-dropped";

/// What [`read_pool`] produced.
#[derive(Debug, Clone, PartialEq)]
pub struct V5Source {
    /// The destination pool: the pool directory's basename.
    pub pool: PoolName,
    /// The manifest's `workspace.slug` — the record's `board`, never the
    /// pool name.
    pub board: String,
    pub items: Vec<SourceItem>,
}

/// Read one v5 regions pool into source items. READ-ONLY over the pool. A
/// file that is not a v5 node at all (unparseable YAML, not a mapping, no
/// string `id`/`type`, a non-string `state`, an edge entry with no string
/// `kind`/`to`) is a [`SpineError::Usage`] naming the file — the adapter
/// cannot make an item of it; everything a v5 node CAN say is handed to the
/// destination grammar, which rejects per item.
pub fn read_pool(pool_dir: &Path) -> Result<V5Source> {
    let pool = pool_dir
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| {
            SpineError::usage(format!(
                "migrate::v5: the pool directory `{}` has no basename to name the destination pool",
                pool_dir.display()
            ))
        })?;
    let pool = PoolName::new(pool);
    let manifest = read_yaml(&pool_dir.join("manifest.yaml"))?;
    let board = manifest
        .get("workspace")
        .and_then(|w| w.get("slug"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            SpineError::usage(format!(
                "migrate::v5: `{}` carries no string `workspace.slug`",
                pool_dir.join("manifest.yaml").display()
            ))
        })?
        .to_string();

    let nodes_dir = pool_dir.join("nodes");
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&nodes_dir)
        .map_err(|e| {
            SpineError::usage(format!(
                "migrate::v5: cannot list `{}`: {e}",
                nodes_dir.display()
            ))
        })?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("yaml"))
        .collect();
    files.sort();

    let mut docs: Vec<(std::path::PathBuf, Map<String, Value>)> = Vec::new();
    for f in files {
        let Value::Object(doc) = read_yaml(&f)? else {
            return Err(SpineError::usage(format!(
                "migrate::v5: `{}` is not a YAML mapping",
                f.display()
            )));
        };
        for key in ["id", "type"] {
            if !doc.get(key).is_some_and(Value::is_string) {
                return Err(SpineError::usage(format!(
                    "migrate::v5: `{}` carries no string `{key}`",
                    f.display()
                )));
            }
        }
        docs.push((f, doc));
    }
    let local: BTreeSet<String> = docs
        .iter()
        .map(|(_, d)| d["id"].as_str().expect("checked").to_string())
        .collect();

    let mut items = Vec::new();
    for (file, doc) in &docs {
        items.extend(node_items(file, doc, &pool, &board, &local)?);
    }
    items.extend(viewer_items(&manifest, &pool));
    Ok(V5Source { pool, board, items })
}

/// [`read_pool`]'s items alone.
pub fn source_items(pool_dir: &Path) -> Result<Vec<SourceItem>> {
    Ok(read_pool(pool_dir)?.items)
}

/// One viewer per role (a workspace constraint, `grammar_v6::LEFT_OUT`):
/// refuse, naming the role, when the destination already holds a `viewer`
/// whose `role` equals one this import would mint.
pub fn refuse_viewer_collision(cauldron: &Cauldron, source: &V5Source) -> Result<()> {
    let roles: BTreeSet<&str> = source
        .items
        .iter()
        .filter_map(|i| match i {
            SourceItem::Node(n) if n.node_type == VIEWER && n.pool == source.pool => {
                n.fields.get(VIEWER_ROLE).and_then(Value::as_str)
            }
            _ => None,
        })
        .collect();
    if roles.is_empty() || cauldron.grammar(&source.pool).is_none() {
        return Ok(());
    }
    let pool = cauldron.open_pool(&source.pool)?;
    let snapshot = pool.snapshot()?;
    for record in snapshot.nodes_of(VIEWER, None)? {
        if let Some(role) = record.fields.get(VIEWER_ROLE).and_then(|f| f.value.as_str()) {
            if roles.contains(role) {
                return Err(SpineError::usage(format!(
                    "migrate::import_v5_pool: pool `{}` already holds viewer `{}` for role \
                     `{role}`; this import would mint a second viewer for that role (one viewer \
                     per role, grammar_v6::LEFT_OUT) — nothing was dry-run",
                    source.pool, record.id.0
                )));
            }
        }
    }
    Ok(())
}

fn read_yaml(path: &Path) -> Result<Value> {
    let bytes = std::fs::read(path).map_err(|e| {
        SpineError::usage(format!("migrate::v5: cannot read `{}`: {e}", path.display()))
    })?;
    serde_yaml::from_slice::<Value>(&bytes).map_err(|e| {
        SpineError::usage(format!(
            "migrate::v5: `{}` is not YAML a v5 pool writes: {e}",
            path.display()
        ))
    })
}

/// The keys the adapter consumes itself; every other top-level key is a
/// field, verbatim.
const CONSUMED: [&str; 9] = [
    "schema_version",
    "id",
    "type",
    "state",
    "provenance",
    "rev",
    "field_revs",
    "meta",
    "edges",
];

fn transform(name: &str, detail: impl Into<String>, lossy: bool) -> NamedTransform {
    NamedTransform {
        name: name.to_string(),
        detail: detail.into(),
        lossy,
    }
}

fn node_items(
    file: &Path,
    doc: &Map<String, Value>,
    pool: &PoolName,
    board: &str,
    local: &BTreeSet<String>,
) -> Result<Vec<SourceItem>> {
    let id = doc["id"].as_str().expect("checked").to_string();
    let node_type = doc["type"].as_str().expect("checked").to_string();
    let mut transforms = Vec::new();

    let state = match doc.get("state") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            if DROPPED_STATES
                .iter()
                .any(|(t, st, _)| *t == node_type && *st == s.as_str())
            {
                transforms.push(transform(
                    TRANSFORM_DROP_COMPUTED_STATE,
                    format!("{node_type}: {s} (v6 computes it from edges; the edge is imported)"),
                    false,
                ));
                None
            } else {
                Some(s.clone())
            }
        }
        Some(other) => {
            return Err(SpineError::usage(format!(
                "migrate::v5: `{}` carries a non-string `state` ({other})",
                file.display()
            )))
        }
    };

    let mut fields: BTreeMap<String, Value> = BTreeMap::new();
    for (k, v) in doc {
        if !CONSUMED.contains(&k.as_str()) {
            fields.insert(k.clone(), v.clone());
        }
    }

    // ---- the import record ----
    let mut record = Map::new();
    record.insert("board".into(), Value::String(board.to_string()));
    record.insert("v5_id".into(), Value::String(id.clone()));
    record.insert("origin".into(), Value::String(format!("{id}@{board}")));
    if let Some(rev) = doc.get("rev") {
        record.insert("rev".into(), rev.clone());
    }
    match doc.get("provenance") {
        Some(Value::Object(prov)) => {
            let mut other = Map::new();
            for (k, v) in prov {
                match k.as_str() {
                    "operator" => {
                        fields.insert("operator".into(), v.clone());
                    }
                    "created" | "created_by" => {
                        record.insert(k.clone(), v.clone());
                    }
                    _ => {
                        other.insert(k.clone(), v.clone());
                    }
                }
            }
            if !other.is_empty() {
                record.insert("provenance".into(), Value::Object(other));
            }
        }
        None | Some(Value::Null) => {}
        Some(other) => {
            record.insert("provenance".into(), other.clone());
        }
    }
    if let Some(fr) = doc.get("field_revs") {
        record.insert("field_revs".into(), fr.clone());
    }
    if let Some(sv) = doc.get("schema_version") {
        record.insert("schema_version".into(), sv.clone());
    }

    // ---- edges ----
    // One pending edge item per (kind, to) toward a LOCAL node (duplicates
    // merged, TRANSFORM_DUPLICATE_TRIPLE), one per entry toward anything else.
    struct Pending {
        entry: usize,
        kind: String,
        to: String,
        parts: Vec<Vec<(String, Value)>>,
        merged: Vec<usize>,
    }
    let mut pending: Vec<Pending> = Vec::new();
    let mut by_triple: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut unresolved: Vec<Value> = Vec::new();
    // (to, admits) of every `in_region` entry, local or not, in source
    // order — the record's `admits_by_edge`.
    let mut admits: Vec<(String, Value)> = Vec::new();
    let entries = match doc.get("edges") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(other) => {
            return Err(SpineError::usage(format!(
                "migrate::v5: `{}` carries a non-list `edges` ({other})",
                file.display()
            )))
        }
    };
    for (i, entry) in entries.iter().enumerate() {
        let (Some(kind), Some(to)) = (
            entry.get("kind").and_then(Value::as_str),
            entry.get("to").and_then(Value::as_str),
        ) else {
            return Err(SpineError::usage(format!(
                "migrate::v5: `{}` edge {i} carries no string `kind`/`to`",
                file.display()
            )));
        };
        let parts = edge_text_parts(entry);
        if kind == "in_region" {
            if let Some(a) = entry.get("admits") {
                admits.push((to.to_string(), a.clone()));
            }
        }
        if local.contains(to) {
            let key = (kind.to_string(), to.to_string());
            if let Some(&at) = by_triple.get(&key) {
                pending[at].parts.push(parts);
                pending[at].merged.push(i);
                continue;
            }
            by_triple.insert(key, pending.len());
        } else {
            unresolved.push(entry.clone());
        }
        pending.push(Pending {
            entry: i,
            kind: kind.to_string(),
            to: to.to_string(),
            parts: vec![parts],
            merged: Vec::new(),
        });
    }
    let mut edge_items: Vec<SourceEdge> = Vec::new();
    for p in pending {
        let mut edge_transforms = Vec::new();
        let all: Vec<(String, Value)> = p.parts.iter().flatten().cloned().collect();
        let note = match all.len() {
            0 => None,
            1 if all[0].0 != "cite" && all[0].1.is_string() => {
                Some(all[0].1.as_str().expect("is_string").to_string())
            }
            _ => {
                edge_transforms.push(transform(
                    TRANSFORM_EDGE_TEXT_JOINED,
                    all.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(", "),
                    false,
                ));
                Some(labelled(&all).join("\n"))
            }
        };
        if !p.merged.is_empty() {
            edge_transforms.push(transform(
                TRANSFORM_DUPLICATE_TRIPLE,
                format!(
                    "{id} -[{}]-> {}: v5 edge entries {} merged into one edge",
                    p.kind,
                    p.to,
                    std::iter::once(p.entry)
                        .chain(p.merged.iter().copied())
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                false,
            ));
        }
        edge_items.push(SourceEdge {
            source_id: format!("{id}/edges/{}", p.entry),
            pool: pool.clone(),
            kind: p.kind,
            src: id.clone(),
            dst: p.to,
            note,
            fields: BTreeMap::new(),
            transforms: edge_transforms,
        });
    }
    record.insert(ADMITS_BY_EDGE.into(), admits_by_edge(admits));
    record.insert("unresolved_edges".into(), Value::Array(unresolved));
    let issues = never_had_required(&node_type, &entries);
    if !issues.is_empty() {
        transforms.push(transform(
            TRANSFORM_REQUIRED_EDGE_ISSUE,
            issues
                .iter()
                .map(|i| format!("{} >= {} (had 0)", i["edge_kind"].as_str().unwrap_or("?"), i["min"]))
                .collect::<Vec<_>>()
                .join(", "),
            false,
        ));
        record.insert(ISSUES_KEY.into(), Value::Array(issues));
    }

    let meta = match doc.get("meta") {
        None | Some(Value::Null) => Some(Map::new()),
        Some(Value::Object(m)) => Some(m.clone()),
        // Not an object: handed to the grammar verbatim, which refuses it
        // (meta is an object field) — the item is REJECTED by name.
        Some(other) => {
            fields.insert("meta".into(), other.clone());
            None
        }
    };
    if let Some(mut meta) = meta {
        if let Some(prior) = meta.remove(RECORD_KEY) {
            record.insert("prior".into(), prior);
            transforms.push(transform(
                TRANSFORM_PRIOR_RECORD,
                "a pre-existing meta.imported_from kept as meta.imported_from.prior",
                false,
            ));
        }
        meta.insert(RECORD_KEY.into(), Value::Object(record));
        fields.insert("meta".into(), Value::Object(meta));
        transforms.push(transform(
            TRANSFORM_RECORD,
            "rev, provenance.created, provenance.created_by, field_revs, schema_version, the source \
             id (v5_id) and board, every in_region admits span (admits_by_edge), and every \
             non-local edge entry into meta.imported_from",
            false,
        ));
    }

    let mut out = vec![SourceItem::Node(SourceNode {
        source_id: id.clone(),
        pool: pool.clone(),
        node_type,
        state,
        carried_id: Some(id),
        fields,
        transforms,
    })];
    out.extend(edge_items.into_iter().map(SourceItem::Edge));
    Ok(out)
}

/// The import ISSUES of one v5 node: one entry
/// `{kind: "required_edge_missing", edge_kind, min, had: 0}` per v6
/// required-at-mint requirement of `node_type` the source carries NO edge
/// entry toward at all (any `to`, local or not — "never had"):
/// - the ONE-OF (`grammar_v6::mint_alternatives`, a content type): no entry
///   of ANY alternative's edge kinds — recorded under the first
///   alternative's first kind (`in_region`, min 1);
/// - each AND-form requirement (`grammar_v6::required_edges`, e.g.
///   `milestone serves >= 1`) with no entry of its kind.
/// A node that carried such an edge is NOT an issue, whatever became of the
/// edge's endpoint: the door judges it exactly as before.
fn never_had_required(node_type: &str, entries: &[Value]) -> Vec<Value> {
    let had = |k: &str| entries.iter().any(|e| e.get("kind").and_then(Value::as_str) == Some(k));
    let mut out = Vec::new();
    let alternatives = crate::grammar_v6::mint_alternatives(node_type);
    if let Some((first, min)) = alternatives.first().and_then(|a| a.edges.iter().next()) {
        let none = alternatives.iter().all(|a| a.edges.keys().all(|k| !had(k)));
        if none {
            out.push(issue(first, *min));
        }
    }
    for (k, min) in crate::grammar_v6::required_edges(node_type) {
        if !had(&k) {
            out.push(issue(&k, min));
        }
    }
    out
}

fn issue(edge_kind: &str, min: u32) -> Value {
    serde_json::json!({
        "kind": crate::schema_def::ISSUE_REQUIRED_EDGE_MISSING,
        "edge_kind": edge_kind,
        "min": min,
        "had": 0,
    })
}

/// The record's `admits_by_edge`: every `in_region`
/// entry's v5 `admits:` span VERBATIM, keyed by the edge's destination id —
/// `{"R-7": "<span>", ...}`. If one node carries two `in_region` entries to
/// ONE destination (measured 0 of 537 on a real v5 pool; the v5 schema
/// does not forbid it), an object cannot key both, so the whole value is the
/// list form `[{to, admits}, ...]` in source order instead — never a dropped
/// span.
fn admits_by_edge(admits: Vec<(String, Value)>) -> Value {
    let distinct: BTreeSet<&str> = admits.iter().map(|(t, _)| t.as_str()).collect();
    if distinct.len() == admits.len() {
        Value::Object(admits.into_iter().collect())
    } else {
        Value::Array(
            admits
                .into_iter()
                .map(|(to, a)| serde_json::json!({"to": to, "admits": a}))
                .collect(),
        )
    }
}

/// An edge entry's text attributes: the five known keys in their fixed
/// order, then any other key but `kind`/`to`, by name.
fn edge_text_parts(entry: &Value) -> Vec<(String, Value)> {
    let Some(obj) = entry.as_object() else {
        return Vec::new();
    };
    let mut parts: Vec<(String, Value)> = EDGE_TEXT_KEYS
        .iter()
        .filter_map(|k| obj.get(*k).map(|v| (k.to_string(), v.clone())))
        .collect();
    for (k, v) in obj {
        if k != "kind" && k != "to" && !EDGE_TEXT_KEYS.contains(&k.as_str()) {
            parts.push((k.clone(), v.clone()));
        }
    }
    parts
}

/// `<key>: <value>` per part — a string verbatim, anything else (and `cite`
/// always) JSON-encoded.
fn labelled(parts: &[(String, Value)]) -> Vec<String> {
    parts
        .iter()
        .map(|(k, v)| match v {
            Value::String(s) if k != "cite" => format!("{k}: {s}"),
            other => format!("{k}: {other}"),
        })
        .collect()
}

fn viewer_items(manifest: &Value, pool: &PoolName) -> Vec<SourceItem> {
    let Some(viewers) = manifest.get("viewers").and_then(Value::as_object) else {
        return Vec::new();
    };
    // serde_json's map is ordered by key, so this IS ascending role order;
    // sorted explicitly anyway so the order never rides a library default.
    let mut roles: Vec<&String> = viewers.keys().collect();
    roles.sort();
    let mut out = Vec::new();
    for role in roles {
        let spec = &viewers[role.as_str()];
        let source_id = format!("{VIEWER_SOURCE_PREFIX}{role}");
        let mut transforms = vec![transform(
            TRANSFORM_VIEWERS,
            format!("manifest viewers.{role}"),
            false,
        )];
        if let Some(note) = spec.get("note") {
            transforms.push(transform(
                TRANSFORM_VIEWER_NOTE_DROPPED,
                format!("viewers.{role}.note has no v6 home: {note}"),
                true,
            ));
        }
        if let Some(obj) = spec.as_object() {
            for k in obj.keys() {
                if k != "nodes" && k != "note" {
                    transforms.push(transform(
                        TRANSFORM_VIEWER_NOTE_DROPPED,
                        format!("viewers.{role}.{k} has no v6 home: {}", obj[k]),
                        true,
                    ));
                }
            }
        }
        out.push(SourceItem::Node(SourceNode {
            source_id: source_id.clone(),
            pool: pool.clone(),
            node_type: VIEWER.to_string(),
            state: None,
            carried_id: None,
            fields: BTreeMap::from([(VIEWER_ROLE.to_string(), Value::String(role.clone()))]),
            transforms,
        }));
        let listed = spec
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (j, entry) in listed.iter().enumerate() {
            let dst = entry
                .get("id")
                .map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))
                .unwrap_or_default();
            let fields: BTreeMap<String, Value> = entry
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter(|(k, _)| k.as_str() != "id")
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .unwrap_or_default();
            out.push(SourceItem::Edge(SourceEdge {
                source_id: format!("{source_id}/shows/{j}"),
                pool: pool.clone(),
                kind: "shows".to_string(),
                src: source_id.clone(),
                dst,
                note: None,
                fields,
                transforms: Vec::new(),
            }));
        }
    }
    out
}
