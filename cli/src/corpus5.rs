//! **The older corpus format, converted to workspace-v6 records**
//! (`cauldron soup import-corpus`).
//!
//! The format is the terminal interface's seed corpus, `corpus.schema_version
//! 5` (`apps/tui/src/cauldron/corpus.rs`, `model.rs`):
//!
//! - a REGION is `{id, handle, boundary, schema_version?, narrows: [region ids]}`;
//! - a NODE is `{id, type, handle, state, body, in_region: [region ids],
//!   cite: "thread#msg"}`;
//! - a seed file wraps them as `{clusters: [...], corpus: {schema_version,
//!   regions, nodes}, cauldron: {...}, messages: {...}}`; a bare corpus is
//!   `{schema_version?, regions, nodes}`; and a directory holds one record per
//!   file (`.json`, `.yaml`, `.yml`), searched recursively.
//!
//! Two optional keys that the format cannot express are accepted on a node,
//! because an interim store kept them beside its records: `refs` (ids, or
//! `{to, kind?, note?}` objects; each becomes a `ref` edge, or the named
//! kind) and `cites` (a list in `meta.cites` form, carried as `meta.cites`).
//!
//! The mapping, record by record:
//!
//! - region -> `{type: region, handle, boundary, edges: narrows}`;
//! - node -> `{type, state, handle, body, operator, edges: in_region}`, where
//!   `cite "<thread>#<msg>"` becomes `operator {thread, msg}` (the format
//!   defines `cite` as the message the node cites; a `msg` that is not a
//!   number is carried as written);
//! - every record carries `meta.imported_from {format: "corpus-v5", id,
//!   file}`, plus `schema_version` when the record had one and `unmapped`
//!   holding, verbatim, every key the mapping does not know (each such key is
//!   also listed in the report);
//! - ids become batch LABELS `v5:<id>`, so the import's `--remint` gives every
//!   record a fresh id and prints the map (no label can equal an id the soup
//!   holds); every reference to an id of the input, and every `[[id]]` link in
//!   a body, is rewritten to that label, and each linked node also gets the
//!   `ref` edge the schema's rule asks for.
//!
//! What it cannot represent is LISTED in the report (`not_carried`), never
//! dropped silently:
//!
//! - clusters (`clusters`, with everything inside them): workspace-v6 has no
//!   cluster type and no edge for a cluster's claim on a region;
//! - a node whose `type` is not one of workspace-v6's ten content types;
//! - a reference (`narrows`, `in_region`, `refs`, a `[[link]]`) to an id that is
//!   not in the input: it would otherwise name whatever record the soup holds
//!   under that id;
//! - the seed's `messages` (the text a trace quotes; each node's own cite is
//!   carried as `operator`) and `cauldron` (the face's demonstration
//!   dashboards), and any other top-level key of a seed file.
//!
//! A state outside the type's vocabulary, a boundary over its cap, a node left
//! with no region, and every other grammar rule are the import door's to
//! judge, exactly as for `cauldron soup import`: one refused record and
//! nothing lands.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

/// The prefix that makes a corpus id a batch label.
pub const LABEL_PREFIX: &str = "v5:";

/// The ten content types of workspace-v6 (`schema/soup/workspace-v6/README.md`).
const CONTENT_TYPES: [&str; 10] =
    ["directive", "constraint", "danger", "issue", "decision", "question", "task", "milestone", "idea", "handoff"];

/// The edge kinds a node's `refs` entry may name.
const REF_KINDS: [&str; 6] = ["ref", "derived_from", "depends_on", "blocks", "supersedes", "resolved_by"];

const REGION_KEYS: [&str; 5] = ["id", "handle", "boundary", "schema_version", "narrows"];
const NODE_KEYS: [&str; 9] = ["id", "type", "handle", "state", "body", "in_region", "cite", "refs", "cites"];

/// The conversion of one input: the records to import and the report.
pub struct Converted {
    pub records: Vec<Value>,
    pub report: Value,
}

#[derive(Default)]
struct Input {
    regions: Vec<(Value, String)>,
    nodes: Vec<(Value, String)>,
    clusters: Vec<(Value, String)>,
    other: Vec<Value>,
    /// Entries of a directory input that were not read (a symlink).
    skipped: Vec<Value>,
    schema_version: Option<u64>,
}

/// Read `path` (a seed file, a corpus file, one record, or a directory of
/// record files) and convert it.
pub fn convert_path(path: &Path) -> Result<Converted> {
    let mut input = Input::default();
    if path.is_dir() {
        let mut files = Vec::new();
        walk(path, path, &mut files, &mut input.skipped)?;
        files.sort();
        if files.is_empty() {
            bail!("{} holds no .json, .yaml or .yml file", path.display());
        }
        for f in files {
            let rel = f.strip_prefix(path).unwrap_or(&f).display().to_string();
            read_into(&f, &rel, &mut input)?;
        }
    } else {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        read_into(path, &name, &mut input)?;
    }
    convert(input)
}

/// The record files under `dir`, recursively. A symlink (to a file or a
/// directory) is not followed, since it can point outside the input; it is
/// listed in `skipped`, so the report says so instead of losing it.
fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>, skipped: &mut Vec<Value>) -> Result<()> {
    let mut entries: Vec<PathBuf> = Vec::new();
    for e in std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
        entries.push(e?.path());
    }
    entries.sort();
    for p in entries {
        let ft = std::fs::symlink_metadata(&p)?.file_type();
        if ft.is_symlink() {
            let rel = p.strip_prefix(root).unwrap_or(&p).display().to_string();
            skipped.push(json!({"what": "file", "file": rel,
                "reason": "symlink-not-followed: a symbolic link in a directory input is not read, since it can point outside the input; copy the file in to carry it"}));
        } else if ft.is_dir() {
            walk(root, &p, out, skipped)?;
        } else if ft.is_file() && matches!(p.extension().and_then(|x| x.to_str()), Some("json" | "yaml" | "yml")) {
            out.push(p);
        }
    }
    Ok(())
}

/// `val` as an array, or a refusal naming the key: a corpus whose
/// `regions`, `nodes` or `clusters` is anything else is not the format, and
/// reading it as empty would drop what it holds without a word.
fn array_of(val: Value, file: &str, key: &str) -> Result<Vec<Value>> {
    match val {
        Value::Array(a) => Ok(a),
        other => {
            let kind = match other {
                Value::Object(_) => "an object",
                Value::String(_) => "a string",
                Value::Number(_) => "a number",
                Value::Bool(_) => "a boolean",
                _ => "null",
            };
            bail!("{file}: `{key}` is {kind}, not an array of records; nothing was imported")
        }
    }
}

fn parse_file(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    match path.extension().and_then(|x| x.to_str()) {
        Some("yaml" | "yml") => serde_yaml::from_str(&text).with_context(|| format!("{} is not YAML", path.display())),
        _ => serde_json::from_str(&text).with_context(|| format!("{} is not JSON", path.display())),
    }
}

/// What one JSON value in a file is.
fn read_into(path: &Path, file: &str, input: &mut Input) -> Result<()> {
    let v = parse_file(path)?;
    match v {
        Value::Array(items) => {
            for (i, item) in items.into_iter().enumerate() {
                classify(item, &format!("{file}[{i}]"), input)?;
            }
        }
        Value::Object(ref m) if m.contains_key("corpus") || m.contains_key("clusters") => {
            // A seed file.
            let Value::Object(m) = v else { unreachable!() };
            for (k, val) in m {
                match k.as_str() {
                    "corpus" => read_corpus(val, &format!("{file}:corpus"), input)?,
                    "clusters" => {
                        for (i, c) in array_of(val, file, "clusters")?.into_iter().enumerate() {
                            input.clusters.push((c, format!("{file}:clusters[{i}]")));
                        }
                    }
                    other => input.other.push(json!({"key": other, "file": file, "value": val})),
                }
            }
        }
        Value::Object(ref m) if m.contains_key("regions") || m.contains_key("nodes") => read_corpus(v, file, input)?,
        other => classify(other, file, input)?,
    }
    Ok(())
}

fn read_corpus(v: Value, file: &str, input: &mut Input) -> Result<()> {
    let Value::Object(m) = v else { bail!("{file}: a corpus is an object with `regions` and `nodes`") };
    for (k, val) in m {
        match k.as_str() {
            "schema_version" => {
                let n = val.as_u64().with_context(|| format!("{file}: schema_version is not a number"))?;
                if n != 5 {
                    bail!("{file}: corpus schema_version {n}; this importer reads the version-5 corpus format only");
                }
                input.schema_version = Some(n);
            }
            "regions" => {
                for (i, r) in array_of(val, file, "regions")?.into_iter().enumerate() {
                    input.regions.push((r, format!("{file}.regions[{i}]")));
                }
            }
            "nodes" => {
                for (i, n) in array_of(val, file, "nodes")?.into_iter().enumerate() {
                    input.nodes.push((n, format!("{file}.nodes[{i}]")));
                }
            }
            other => input.other.push(json!({"key": other, "file": file, "value": val})),
        }
    }
    Ok(())
}

/// One record in a file of its own: a node has a `type`, a region a
/// `boundary`, a cluster a `name` and a `description`.
fn classify(v: Value, file: &str, input: &mut Input) -> Result<()> {
    let Some(m) = v.as_object() else { bail!("{file}: a record is a JSON object") };
    if m.contains_key("type") {
        input.nodes.push((v, file.to_string()));
    } else if m.contains_key("boundary") {
        input.regions.push((v, file.to_string()));
    } else if m.contains_key("name") && m.contains_key("description") {
        input.clusters.push((v, file.to_string()));
    } else {
        bail!("{file}: neither a region (no `boundary`), a node (no `type`) nor a cluster (no `name` and `description`)");
    }
    Ok(())
}

fn label(id: &str) -> String {
    format!("{LABEL_PREFIX}{id}")
}

fn str_field<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    m.get(k).and_then(Value::as_str)
}

fn convert(input: Input) -> Result<Converted> {
    let mut not_carried: Vec<Value> = Vec::new();
    // Every id of the input, once.
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    let mut node_type_ok: BTreeSet<String> = BTreeSet::new();
    for (r, at) in input.regions.iter().chain(input.nodes.iter()) {
        let id = r.get("id").and_then(Value::as_str).with_context(|| format!("{at}: a record without a string `id`"))?;
        if id.is_empty() || id.contains('@') || id.contains('#') {
            bail!("{at}: id {id:?} cannot be a batch label (empty, or holds `@` or `#`)");
        }
        if let Some(first) = ids.insert(id.to_string(), at.clone()) {
            bail!("the id {id} is in the input twice: {first} and {at}. Ids are unique in a corpus; remove one");
        }
    }
    for (n, _) in &input.nodes {
        let t = n.get("type").and_then(Value::as_str).unwrap_or("");
        if CONTENT_TYPES.contains(&t) {
            node_type_ok.insert(n["id"].as_str().unwrap_or("").to_string());
        }
    }
    let region_ids: BTreeSet<String> = input.regions.iter().filter_map(|(r, _)| r["id"].as_str().map(String::from)).collect();
    // A reference resolves to a label only if its target is converted.
    let carried = |id: &str| region_ids.contains(id) || node_type_ok.contains(id);

    let mut records = Vec::new();
    let (mut n_regions, mut n_nodes, mut n_edges, mut n_link_refs) = (0usize, 0usize, 0usize, 0usize);

    for (r, at) in &input.regions {
        let m = r.as_object().unwrap();
        let id = str_field(m, "id").unwrap();
        let mut meta = json!({"imported_from": {"format": "corpus-v5", "id": id, "file": at}});
        if let Some(sv) = m.get("schema_version") {
            meta["imported_from"]["schema_version"] = sv.clone();
        }
        let unmapped = unmapped(m, &REGION_KEYS, id, at, &mut not_carried);
        if !unmapped.is_empty() {
            meta["imported_from"]["unmapped"] = Value::Object(unmapped);
        }
        let mut edges = Vec::new();
        for p in m.get("narrows").and_then(Value::as_array).cloned().unwrap_or_default() {
            let p = p.as_str().unwrap_or_default().to_string();
            if region_ids.contains(&p) {
                edges.push(json!({"kind": "narrows", "to": label(&p)}));
                n_edges += 1;
            } else {
                not_carried.push(json!({"what": "edge", "from": id, "kind": "narrows", "to": p, "file": at,
                    "reason": "target-not-in-input: the region it narrows is not in the input, and a bare id would name whatever the soup holds under it"}));
            }
        }
        // An `Outside:` clause names neighbours by id; the text is not
        // rewritten (it is prose), so the old ids it names are listed.
        if let Some(b) = str_field(m, "boundary") {
            let named: Vec<&String> = region_ids.iter().filter(|r| mentions(b, r)).collect();
            if !named.is_empty() {
                not_carried.push(json!({"what": "boundary-text", "id": id, "names": named, "file": at,
                    "reason": "old-ids-in-text: the boundary names regions by their corpus ids; the text is kept as written, so read those ids through corpus.id_map"}));
            }
        }
        let mut rec = json!({"id": label(id), "type": "region", "meta": meta});
        if let Some(h) = m.get("handle") {
            rec["handle"] = h.clone();
        }
        if let Some(b) = m.get("boundary") {
            rec["boundary"] = b.clone();
        }
        if !edges.is_empty() {
            rec["edges"] = Value::Array(edges);
        }
        records.push(rec);
        n_regions += 1;
    }

    for (n, at) in &input.nodes {
        let m = n.as_object().unwrap();
        let id = str_field(m, "id").unwrap();
        let t = str_field(m, "type").unwrap_or("");
        if !CONTENT_TYPES.contains(&t) {
            not_carried.push(json!({"what": "node", "id": id, "type": m.get("type"), "handle": m.get("handle"), "file": at,
                "reason": format!("type-not-in-workspace-v6: `{t}` is not one of workspace-v6's ten content types ({}); it is not mapped to another type by guess", CONTENT_TYPES.join(", "))}));
            continue;
        }
        let mut meta = json!({"imported_from": {"format": "corpus-v5", "id": id, "file": at}});
        let unmapped = unmapped(m, &NODE_KEYS, id, at, &mut not_carried);
        if !unmapped.is_empty() {
            meta["imported_from"]["unmapped"] = Value::Object(unmapped);
        }
        let mut rec = json!({"id": label(id), "type": t});
        for k in ["handle", "state"] {
            if let Some(v) = m.get(k) {
                if !(k == "state" && v.as_str() == Some("")) {
                    rec[k] = v.clone();
                }
            }
        }
        let mut edges = Vec::new();
        for r in m.get("in_region").and_then(Value::as_array).cloned().unwrap_or_default() {
            let r = r.as_str().unwrap_or_default().to_string();
            if region_ids.contains(&r) {
                edges.push(json!({"kind": "in_region", "to": label(&r)}));
                n_edges += 1;
            } else {
                not_carried.push(json!({"what": "edge", "from": id, "kind": "in_region", "to": r, "file": at,
                    "reason": "target-not-in-input: the region is not in the input, and a bare id would name whatever the soup holds under it"}));
            }
        }
        for entry in m.get("refs").and_then(Value::as_array).cloned().unwrap_or_default() {
            let (to, kind, note) = match &entry {
                Value::String(s) => (s.clone(), "ref".to_string(), None),
                Value::Object(o) => (
                    o.get("to").and_then(Value::as_str).unwrap_or_default().to_string(),
                    o.get("kind").and_then(Value::as_str).unwrap_or("ref").to_string(),
                    o.get("note").cloned(),
                ),
                _ => (String::new(), String::new(), None),
            };
            if to.is_empty() || !REF_KINDS.contains(&kind.as_str()) {
                not_carried.push(json!({"what": "ref", "from": id, "value": entry, "file": at,
                    "reason": format!("bad-ref: a `refs` entry is an id or {{to, kind?, note?}} with kind one of {}", REF_KINDS.join(", "))}));
                continue;
            }
            if !carried(&to) {
                not_carried.push(json!({"what": "edge", "from": id, "kind": kind, "to": to, "file": at,
                    "reason": "target-not-in-input: the target is not converted from the input, and a bare id would name whatever the soup holds under it"}));
                continue;
            }
            let mut e = json!({"kind": kind, "to": label(&to)});
            if let Some(note) = note {
                e["note"] = note;
            }
            edges.push(e);
            n_edges += 1;
        }
        if let Some(body) = str_field(m, "body").filter(|b| !b.is_empty()) {
            let (rewritten, linked, dangling) = rewrite_links(body, &carried);
            for to in linked {
                let already = edges.iter().any(|e| e["kind"] == "ref" && e["to"] == json!(label(&to)));
                if !already {
                    edges.push(json!({"kind": "ref", "to": label(&to)}));
                    n_edges += 1;
                    n_link_refs += 1;
                }
            }
            for to in dangling {
                not_carried.push(json!({"what": "link", "from": id, "to": to, "file": at,
                    "reason": "target-not-in-input: the body links [[id]] to a record that is not converted from the input; the link text is kept as written and no edge is made"}));
            }
            rec["body"] = json!(rewritten);
        } else if let Some(b) = m.get("body").filter(|b| !b.is_string()) {
            rec["body"] = b.clone();
        }
        if let Some(cite) = m.get("cite").filter(|c| !c.is_null()) {
            match cite.as_str().and_then(|c| c.split_once('#')) {
                Some((thread, msg)) if !thread.is_empty() && !msg.is_empty() => {
                    let msg = msg.parse::<u64>().map(Value::from).unwrap_or_else(|_| json!(msg));
                    rec["operator"] = json!({"thread": thread, "msg": msg});
                }
                _ => {
                    meta["imported_from"]["cite"] = cite.clone();
                    not_carried.push(json!({"what": "cite", "from": id, "value": cite, "file": at,
                        "reason": "cite-not-thread-msg: not `<thread>#<msg>`, so it is not carried as `operator`; it is kept in meta.imported_from.cite"}));
                }
            }
        }
        if let Some(cites) = m.get("cites") {
            meta["cites"] = cites.clone();
        }
        rec["meta"] = meta;
        if !edges.is_empty() {
            rec["edges"] = Value::Array(edges);
        }
        records.push(rec);
        n_nodes += 1;
    }

    for (c, at) in &input.clusters {
        not_carried.push(cluster_entry(c, at));
    }
    not_carried.extend(input.skipped.iter().cloned());
    for o in &input.other {
        let key = o["key"].as_str().unwrap_or("");
        let reason = match key {
            "messages" => "seed-messages: the text of the messages a trace quotes is not a record; each node's own cite is carried as `operator`".to_string(),
            "cauldron" => "seed-dashboards: the face's demonstration dashboards (Status and Config tabs) are not knowledge records".to_string(),
            _ => format!("unknown-key: `{key}` is not part of the corpus format"),
        };
        let size = match &o["value"] {
            Value::Object(m) => m.len(),
            Value::Array(a) => a.len(),
            _ => 1,
        };
        not_carried.push(json!({"what": "seed-key", "key": key, "entries": size, "file": o["file"], "reason": reason}));
    }
    if records.is_empty() {
        bail!(
            "nothing in the input converts to a workspace-v6 record (it holds {} region(s), {} node(s), {} cluster(s)); see what cannot be carried: {}",
            input.regions.len(),
            input.nodes.len(),
            input.clusters.len(),
            serde_json::to_string(&not_carried)?
        );
    }
    let report = json!({
        "format": "corpus-v5",
        "schema_version": input.schema_version.unwrap_or(5),
        "label_prefix": LABEL_PREFIX,
        "carried": {"regions": n_regions, "nodes": n_nodes, "edges": n_edges, "ref_edges_added_for_links": n_link_refs},
        "not_carried": not_carried,
    });
    Ok(Converted { records, report })
}

/// `text` names `id` as a whole word (not inside a longer id).
fn mentions(text: &str, id: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    text.match_indices(id).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + id.len()..].chars().next();
        !before.is_some_and(word) && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The keys of `m` outside `known`, verbatim, each listed in the report.
fn unmapped(m: &Map<String, Value>, known: &[&str], id: &str, at: &str, not_carried: &mut Vec<Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in m {
        if !known.contains(&k.as_str()) {
            out.insert(k.clone(), v.clone());
            not_carried.push(json!({"what": "field", "id": id, "key": k, "file": at,
                "reason": "unknown-key: not part of the corpus format; carried verbatim in meta.imported_from.unmapped, not as a field"}));
        }
    }
    out
}

/// A cluster, and what it holds, as one `not_carried` entry.
fn cluster_entry(c: &Value, at: &str) -> Value {
    let mut holds = Map::new();
    let mut children = Vec::new();
    if let Some(m) = c.as_object() {
        for (k, v) in m {
            match (k.as_str(), v) {
                ("children", Value::Array(a)) => {
                    for (i, ch) in a.iter().enumerate() {
                        children.push(cluster_entry(ch, &format!("{at}.children[{i}]")));
                    }
                }
                ("id" | "name" | "description" | "regions", _) => {}
                (_, Value::Array(a)) if !a.is_empty() => {
                    holds.insert(k.clone(), json!(a.len()));
                }
                _ => {}
            }
        }
    }
    let mut e = json!({
        "what": "cluster",
        "id": c.get("id"),
        "name": c.get("name"),
        "claims_regions": c.get("regions").cloned().unwrap_or(json!([])),
        "holds": holds,
        "file": at,
        "reason": "cluster-not-in-workspace-v6: workspace-v6 has no cluster type and no edge for a cluster's claim on a region, so the cluster, its claims and everything it holds are not imported",
    });
    if !children.is_empty() {
        e["children"] = Value::Array(children);
    }
    e
}

/// `[[ID]]` links in `body`: those whose target is converted are rewritten to
/// the target's label; the rest are left as written. Returns the new body,
/// the converted targets, and the others.
fn rewrite_links(body: &str, carried: &dyn Fn(&str) -> bool) -> (String, Vec<String>, Vec<String>) {
    let mut out = String::with_capacity(body.len());
    let (mut linked, mut dangling) = (Vec::new(), Vec::new());
    let mut rest = body;
    while let Some(open) = rest.find("[[") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        match after.find("]]") {
            Some(close) if !after[..close].contains('[') && !after[..close].is_empty() => {
                let target = &after[..close];
                if carried(target) {
                    out.push_str(&format!("[[{}]]", label(target)));
                    if !linked.iter().any(|t| t == target) {
                        linked.push(target.to_string());
                    }
                } else {
                    out.push_str(&format!("[[{target}]]"));
                    if !dangling.iter().any(|t| t == target) {
                        dangling.push(target.to_string());
                    }
                }
                rest = &after[close + 2..];
            }
            _ => {
                out.push_str("[[");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    (out, linked, dangling)
}

/// The report's `id_map`, by corpus id, from the import's `id_map` (labels
/// to new ids).
pub fn corpus_id_map(import_id_map: &Value) -> Value {
    let mut out = Map::new();
    if let Some(m) = import_id_map.as_object() {
        for (k, v) in m {
            if let Some(id) = k.strip_prefix(LABEL_PREFIX) {
                out.insert(id.to_string(), v.clone());
            }
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(v: Value) -> Converted {
        let mut input = Input::default();
        read_value(v, "t.json", &mut input);
        convert(input).unwrap()
    }
    fn read_value(v: Value, file: &str, input: &mut Input) {
        let dir = std::env::temp_dir().join(format!("corpus5-unit-{}-{}", std::process::id(), rand_tag()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(file);
        std::fs::write(&p, serde_json::to_string(&v).unwrap()).unwrap();
        read_into(&p, file, input).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
    fn rand_tag() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::SeqCst)
    }
    fn region(id: &str, narrows: &[&str]) -> Value {
        json!({"id": id, "handle": format!("Region {id}"), "boundary": "Inside: anything about the test topic here. Outside: nothing else.", "narrows": narrows})
    }

    #[test]
    fn a_seed_maps_regions_nodes_edges_and_lists_clusters() {
        let c = conv(json!({
            "clusters": [{"id": "build", "name": "Build", "description": "d", "regions": ["R-1"], "tasks": [{"id": "T-9"}],
                          "children": [{"id": "ui", "name": "UI", "description": "d", "regions": ["R-2"]}]}],
            "corpus": {"schema_version": 5, "regions": [region("R-1", &[]), region("R-2", &["R-1"])],
                       "nodes": [{"id": "DEC-1", "type": "decision", "state": "decided", "handle": "A call.", "body": "See [[IS-2]] and [[Q-77]].",
                                  "in_region": ["R-2"], "cite": "build#4"},
                                 {"id": "IS-2", "type": "issue", "state": "open", "handle": "A defect.", "in_region": ["R-1", "R-9"]},
                                 {"id": "INC-1", "type": "incident", "state": "closed", "handle": "Something broke.", "in_region": ["R-1"]}]},
            "messages": {"build#4": {"date": "d", "text": "t"}},
        }));
        let by: BTreeMap<String, &Value> = c.records.iter().map(|r| (r["id"].as_str().unwrap().to_string(), r)).collect();
        assert_eq!(by.len(), 4, "two regions and two nodes; the incident is not carried");
        assert_eq!(by["v5:R-2"]["edges"], json!([{"kind": "narrows", "to": "v5:R-1"}]));
        let dec = by["v5:DEC-1"];
        assert_eq!(dec["operator"], json!({"thread": "build", "msg": 4}));
        assert_eq!(dec["body"], "See [[v5:IS-2]] and [[Q-77]].");
        assert!(dec["edges"].as_array().unwrap().contains(&json!({"kind": "ref", "to": "v5:IS-2"})));
        assert_eq!(dec["meta"]["imported_from"]["id"], "DEC-1");
        assert_eq!(by["v5:IS-2"]["edges"], json!([{"kind": "in_region", "to": "v5:R-1"}]), "the edge to R-9, not in the input, is not made");
        let nc = c.report["not_carried"].as_array().unwrap();
        let reasons: Vec<&str> = nc.iter().map(|e| e["reason"].as_str().unwrap().split(':').next().unwrap()).collect();
        for want in ["type-not-in-workspace-v6", "target-not-in-input", "cluster-not-in-workspace-v6", "seed-messages"] {
            assert!(reasons.contains(&want), "{want} not listed: {reasons:?}");
        }
        let cl = nc.iter().find(|e| e["what"] == "cluster").unwrap();
        assert_eq!(cl["claims_regions"], json!(["R-1"]));
        assert_eq!(cl["holds"], json!({"tasks": 1}));
        assert_eq!(cl["children"][0]["id"], "ui");
        assert!(nc.iter().any(|e| e["what"] == "link" && e["to"] == "Q-77"));
    }

    #[test]
    fn refs_cites_and_unknown_keys_are_carried_and_reported() {
        let c = conv(json!([
            region("R-1", &[]),
            {"id": "DEC-1", "type": "decision", "state": "decided", "handle": "A call.", "in_region": ["R-1"],
             "refs": ["DEC-2", {"to": "DEC-2", "kind": "derived_from", "note": "drawn from"}, {"to": "X", "kind": "likes"}],
             "cites": [{"kind": "url", "url": "https://example.com/spec"}], "tags": ["a"]},
            {"id": "DEC-2", "type": "decision", "state": "decided", "handle": "Another.", "in_region": ["R-1"], "cite": "notes#first"},
        ]));
        let d1 = c.records.iter().find(|r| r["id"] == "v5:DEC-1").unwrap();
        assert!(d1["edges"].as_array().unwrap().contains(&json!({"kind": "ref", "to": "v5:DEC-2"})));
        assert!(d1["edges"].as_array().unwrap().contains(&json!({"kind": "derived_from", "to": "v5:DEC-2", "note": "drawn from"})));
        assert_eq!(d1["meta"]["cites"], json!([{"kind": "url", "url": "https://example.com/spec"}]));
        assert_eq!(d1["meta"]["imported_from"]["unmapped"], json!({"tags": ["a"]}));
        let d2 = c.records.iter().find(|r| r["id"] == "v5:DEC-2").unwrap();
        assert_eq!(d2["operator"], json!({"thread": "notes", "msg": "first"}));
        let nc = c.report["not_carried"].as_array().unwrap();
        assert!(nc.iter().any(|e| e["what"] == "field" && e["key"] == "tags"));
        assert!(nc.iter().any(|e| e["what"] == "ref" && e["reason"].as_str().unwrap().starts_with("bad-ref")));
    }

    #[test]
    fn a_duplicate_id_and_another_corpus_version_are_refused() {
        let mut input = Input::default();
        read_value(json!([region("R-1", &[]), region("R-1", &[])]), "d.json", &mut input);
        assert!(convert(input).err().unwrap().to_string().contains("in the input twice"));
        let mut input = Input::default();
        let dir = std::env::temp_dir().join(format!("corpus5-unit-v-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("c.json"), r#"{"schema_version": 4, "regions": [], "nodes": []}"#).unwrap();
        assert!(read_into(&dir.join("c.json"), "c.json", &mut input).err().unwrap().to_string().contains("version-5 corpus format only"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing in the input is dropped without a word: a symlinked record
    /// file in a directory input is listed, and a `regions`, `nodes` or
    /// `clusters` that is not an array refuses the import.
    #[test]
    fn a_symlink_is_listed_and_a_collection_that_is_not_an_array_is_refused() {
        let dir = std::env::temp_dir().join(format!("corpus5-unit-l-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("in/sub")).unwrap();
        std::fs::write(dir.join("in/r.json"), serde_json::to_string(&region("R-1", &[])).unwrap()).unwrap();
        std::fs::write(dir.join("linked-target.json"), serde_json::to_string(&region("R-2", &[])).unwrap()).unwrap();
        std::os::unix::fs::symlink("../../linked-target.json", dir.join("in/sub/link.json")).unwrap();
        let c = convert_path(&dir.join("in")).unwrap();
        let nc = c.report["not_carried"].as_array().unwrap();
        let e = nc.iter().find(|e| e["what"] == "file").expect("the symlink is listed");
        assert_eq!(e["file"], "sub/link.json");
        assert!(e["reason"].as_str().unwrap().starts_with("symlink-not-followed"));
        for (key, body) in [
            ("regions", r#"{"schema_version": 5, "regions": {"R-1": {}}, "nodes": []}"#),
            ("nodes", r#"{"schema_version": 5, "regions": [], "nodes": "none"}"#),
            ("clusters", r#"{"clusters": {"a": 1}, "corpus": {"schema_version": 5, "regions": [], "nodes": []}}"#),
        ] {
            std::fs::write(dir.join("bad.json"), body).unwrap();
            let err = convert_path(&dir.join("bad.json")).err().unwrap().to_string();
            assert!(err.contains(&format!("`{key}` is")) && err.contains("not an array"), "{key}: {err}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_boundary_naming_an_old_id_is_listed() {
        assert!(mentions("Outside: the schema (R-3).", "R-3"));
        assert!(!mentions("Outside: the schema (R-31).", "R-3"));
        assert!(!mentions("Outside: SUBR-3 here", "R-3"));
    }

    #[test]
    fn the_id_map_is_given_by_corpus_id() {
        assert_eq!(corpus_id_map(&json!({"v5:R-1": "R-7", "v5:DEC-1": "DEC-30"})), json!({"R-1": "R-7", "DEC-1": "DEC-30"}));
    }
}
