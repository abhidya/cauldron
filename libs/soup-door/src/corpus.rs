//! **An older corpus's clusters, as `cauldron@1` records.**
//!
//! The older corpus format (the seed corpus, version 5) has clusters: an
//! `id`, a `name`, a `description`, the `regions` the cluster claims, and
//! what it holds (`tasks`, `questions`, `human_actions`, `repos`,
//! `directives`, `constraints`, `objectives`, `milestones`,
//! `closed_questions`, `ripples`, `links`), with `children`, clusters of
//! their own. [`cluster_records`] maps one such cluster (and each child) to
//! the records a batch lands it as:
//!
//! - one `cluster` record: `handle` the name, `boundary` the description,
//!   `meta.imported_from {format: "corpus-v5", id, file}`, and one `claims`
//!   edge to each region the cluster names that the same batch carries (by
//!   that region's batch label);
//! - in the same batch as the corpus's regions and nodes, under `--remint`
//!   (a corpus id is not a `CL-<n>`), so the import routes the record to the
//!   cluster pool, gives an older store that pool, and lands everything as
//!   one write, all or nothing.
//!
//! What a `cluster` record cannot hold is never dropped silently: each part
//! is one `not_carried` entry, with its reason, as the corpus importer lists
//! everything else it cannot carry:
//!
//! - every non-empty part a cluster holds (`tasks` and the rest above), by
//!   key and count: `cauldron@1` gives a cluster a handle, a boundary and
//!   its claims only;
//! - a claim of a region the batch does not carry;
//! - a child's nesting: a child is mapped as a cluster of its own (claiming
//!   its own regions), and the entry names its parent, since `cauldron@1` has
//!   no edge between clusters;
//! - a cluster whose name or description is outside the grammar's bounds
//!   (the region's: a handle of 1 to 235 characters, a boundary of 40 to
//!   1,000): no record, and the text is neither padded nor cut;
//! - a cluster with no `id`, `name` or `description`.
//!
//! # Used by
//!
//! `cauldron soup import-corpus` (`cli/src/corpus5.rs`) calls it for every
//! cluster of its input, with its own `label` (corpus id to batch label) and
//! the set of region ids it converts. A cluster's label is
//! `label("cluster:<id>")`, so the importer's id map by corpus id answers it
//! as `cluster:<id>`.

use serde_json::{json, Map, Value};
use spine::grammar_v6::{BOUNDARY_CAP, BOUNDARY_FLOOR, HANDLE_CAP, HANDLE_FLOOR};

/// The format name every `meta.imported_from` carries.
pub const FORMAT: &str = "corpus-v5";

/// The keys of a corpus cluster a `cluster` record carries (or maps).
const CARRIED: [&str; 5] = ["id", "name", "description", "regions", "children"];

/// **The records one corpus cluster lands as**, its children included (each
/// a cluster of its own), and every part they cannot carry pushed onto
/// `not_carried` (module docs). `at` names where the cluster was read
/// (`clusters.json:clusters[0]`); `label` turns a corpus id into its batch
/// label; `carried_region` says whether the batch carries a region of that
/// corpus id.
pub fn cluster_records(
    cluster: &Value,
    at: &str,
    label: &dyn Fn(&str) -> String,
    carried_region: &dyn Fn(&str) -> bool,
    not_carried: &mut Vec<Value>,
) -> Vec<Value> {
    let mut out = Vec::new();
    map(cluster, at, None, label, carried_region, not_carried, &mut out);
    out
}

fn chars(s: &str) -> u64 {
    s.chars().count() as u64
}

fn map(
    cluster: &Value,
    at: &str,
    parent: Option<&str>,
    label: &dyn Fn(&str) -> String,
    carried_region: &dyn Fn(&str) -> bool,
    not_carried: &mut Vec<Value>,
    out: &mut Vec<Value>,
) {
    let Some(m) = cluster.as_object() else {
        not_carried.push(json!({"what": "cluster", "file": at, "reason": "cluster-not-an-object: a corpus cluster is a JSON object"}));
        return;
    };
    let text = |k: &str| m.get(k).and_then(Value::as_str);
    let id = text("id").unwrap_or("");
    // Each child is mapped, whatever becomes of this one.
    for (i, ch) in m.get("children").and_then(Value::as_array).into_iter().flatten().enumerate() {
        map(ch, &format!("{at}.children[{i}]"), Some(id), label, carried_region, not_carried, out);
    }
    let (Some(name), Some(description)) = (text("name"), text("description")) else {
        not_carried.push(json!({"what": "cluster", "id": m.get("id"), "file": at,
            "reason": "cluster-incomplete: a corpus cluster needs an `id`, a `name` and a `description`; this one is not imported"}));
        return;
    };
    if id.is_empty() || id.contains('@') || id.contains('#') {
        not_carried.push(json!({"what": "cluster", "id": m.get("id"), "file": at,
            "reason": "cluster-id-unusable: its id is empty or holds `@` or `#`, so it cannot be a batch label; it is not imported"}));
        return;
    }
    let (h, b) = (chars(name), chars(description));
    if !(HANDLE_FLOOR..=HANDLE_CAP).contains(&h) || !(BOUNDARY_FLOOR..=BOUNDARY_CAP).contains(&b) {
        not_carried.push(json!({"what": "cluster", "id": id, "file": at, "name_chars": h, "description_chars": b,
            "reason": format!(
                "cluster-out-of-bounds: a cluster's handle (its name) is {HANDLE_FLOOR} to {HANDLE_CAP} characters and its boundary \
                 (its description) {BOUNDARY_FLOOR} to {BOUNDARY_CAP}, as a region's; the text is not padded or cut, so the cluster is not imported"
            )}));
        return;
    }
    let mut edges = Vec::new();
    for r in m.get("regions").and_then(Value::as_array).into_iter().flatten() {
        let r = r.as_str().unwrap_or_default();
        if carried_region(r) {
            edges.push(json!({"kind": "claims", "to": label(r)}));
        } else {
            not_carried.push(json!({"what": "claim", "from": id, "to": r, "file": at,
                "reason": "target-not-in-input: the cluster claims a region this import does not carry, so that claim is not imported"}));
        }
    }
    if let Some(p) = parent {
        not_carried.push(json!({"what": "cluster-nesting", "id": id, "parent": p, "file": at,
            "reason": "no-cluster-nesting-in-cauldron-v1: a child cluster is imported as a cluster of its own; cauldron@1 has no edge between clusters, so it is not recorded as its parent's"}));
    }
    let mut holds = Map::new();
    for (k, v) in m {
        if CARRIED.contains(&k.as_str()) {
            continue;
        }
        match v {
            Value::Array(a) if a.is_empty() => {}
            Value::Array(a) => {
                holds.insert(k.clone(), json!(a.len()));
                not_carried.push(json!({"what": "cluster-part", "cluster": id, "key": k, "entries": a.len(), "file": at,
                    "reason": format!("not-in-cauldron-v1: a cluster record holds a handle, a boundary and its claims only, so the cluster's `{k}` is not imported")}));
            }
            _ => {
                not_carried.push(json!({"what": "cluster-part", "cluster": id, "key": k, "file": at,
                    "reason": format!("unknown-key: `{k}` is not part of a corpus cluster; it is not imported")}));
            }
        }
    }
    let mut imported = json!({"format": FORMAT, "id": id, "file": at});
    if !holds.is_empty() {
        imported["not_carried"] = Value::Object(holds);
    }
    let mut rec = json!({
        "id": label(&format!("cluster:{id}")),
        "type": spine::grammar_cauldron::CLUSTER,
        "handle": name,
        "boundary": description,
        "meta": {"imported_from": imported},
    });
    if !edges.is_empty() {
        rec["edges"] = Value::Array(edges);
    }
    out.push(rec);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{import, ImportOptions, ImportOutcome};

    /// The demo seed this repository ships, in the older corpus format.
    const SEED: &str = include_str!("../../soup/demo/cauldron-build/clusters.json");

    fn label(id: &str) -> String {
        format!("v5:{id}")
    }

    struct Tmp(std::path::PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A cauldron as 0.1.4 to 0.1.14 made it: `ws` only.
    fn older_store(tag: &str) -> (Tmp, crate::Target) {
        let dir = std::env::temp_dir().join(format!(
            "soup-door-corpus-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let soup = dir.join("cauldrons").join("kb").join("soup");
        std::fs::create_dir_all(&soup).unwrap();
        std::fs::write(dir.join("cauldrons").join("kb").join("cauldron.toml"), "name = \"kb\"\ncreated_at = \"2026-01-01T00:00:00Z\"\nenabled = true\n").unwrap();
        drop(spine::Cauldron::create(&soup, &[spine::grammar_v6::workspace_v6("ws")], spine::config::SpineConfig::default()).unwrap());
        let e = env::Environment {
            root: dir.clone(),
            root_source: env::paths::Source::CauldronHome,
            name: "dev".into(),
            name_source: env::paths::Source::CauldronEnv,
            data_root: dir.clone(),
        };
        let c = e.find_cauldron("kb").unwrap().expect("found");
        (Tmp(dir), crate::Target { env: e, cauldron: c, resolved_from: None })
    }

    fn boundary() -> String {
        "Inside: everything about shipping a release to the people who use it.".to_string()
    }

    /// One cluster: its record, its claims by batch label, every part it
    /// cannot carry listed by key, a claim of a region the batch lacks
    /// listed, and a child mapped as a cluster of its own with its nesting
    /// listed.
    #[test]
    fn a_cluster_maps_to_one_record_with_its_claims_and_lists_what_it_cannot_carry() {
        let c = json!({
            "id": "X-1", "name": "Shipping", "description": boundary(), "regions": ["R-1", "R-9"],
            "tasks": [{"id": "T-1"}, {"id": "T-2"}], "questions": [], "repos": [{"name": "cauldron"}],
            "children": [{"id": "X-2", "name": "Signing", "description": boundary(), "regions": ["R-1"], "milestones": [{"id": "M-1"}]}]
        });
        let mut nc = Vec::new();
        let recs = cluster_records(&c, "clusters.json:clusters[0]", &label, &|r| r == "R-1", &mut nc);
        assert_eq!(recs.len(), 2, "the child and the cluster: {recs:?}");
        let parent = recs.iter().find(|r| r["id"] == "v5:cluster:X-1").unwrap();
        assert_eq!(parent["type"], "cluster");
        assert_eq!((parent["handle"].as_str(), parent["boundary"].as_str()), (Some("Shipping"), Some(boundary().as_str())));
        assert_eq!(parent["edges"], json!([{"kind": "claims", "to": "v5:R-1"}]));
        assert_eq!(parent["meta"]["imported_from"]["id"], "X-1");
        assert_eq!(parent["meta"]["imported_from"]["format"], FORMAT);
        assert_eq!(parent["meta"]["imported_from"]["not_carried"], json!({"tasks": 2, "repos": 1}));
        let child = recs.iter().find(|r| r["id"] == "v5:cluster:X-2").unwrap();
        assert_eq!(child["edges"], json!([{"kind": "claims", "to": "v5:R-1"}]));
        let whats: Vec<(String, String)> = nc.iter().map(|e| (e["what"].as_str().unwrap().to_string(), e["reason"].as_str().unwrap().split(':').next().unwrap().to_string())).collect();
        for want in [
            ("claim", "target-not-in-input"),
            ("cluster-part", "not-in-cauldron-v1"),
            ("cluster-nesting", "no-cluster-nesting-in-cauldron-v1"),
        ] {
            assert!(whats.iter().any(|(w, r)| (w.as_str(), r.as_str()) == want), "{want:?} in {nc:?}");
        }
        assert_eq!(nc.iter().filter(|e| e["what"] == "cluster-part").count(), 3, "tasks, repos, and the child's milestones: {nc:?}");
        assert!(nc.iter().all(|e| e["key"] != "questions"), "an empty part is nothing to report");
    }

    /// A cluster whose text is outside the grammar's bounds, or that lacks a
    /// name, is listed and makes no record; nothing is padded or cut.
    #[test]
    fn a_cluster_outside_the_bounds_is_listed_not_padded() {
        let mut nc = Vec::new();
        let short = json!({"id": "X-3", "name": "Short", "description": "too short", "regions": []});
        let long = json!({"id": "X-4", "name": "n".repeat(236), "description": boundary()});
        let none = json!({"id": "X-5", "description": boundary()});
        for c in [short, long, none] {
            assert!(cluster_records(&c, "f", &label, &|_| true, &mut nc).is_empty(), "{c}");
        }
        let reasons: Vec<&str> = nc.iter().map(|e| e["reason"].as_str().unwrap().split(':').next().unwrap()).collect();
        assert_eq!(reasons, ["cluster-out-of-bounds", "cluster-out-of-bounds", "cluster-incomplete"]);
    }

    /// The demo seed's clusters, mapped and landed with the seed's regions
    /// in ONE import on a store made before clusters: the store gets the
    /// cluster pool, every cluster lands with its claims, and each region
    /// names the clusters that claim it.
    #[test]
    fn the_demo_seed_s_clusters_land_with_their_claims_in_one_write() {
        let seed: Value = serde_json::from_str(SEED).unwrap();
        let regions = seed["corpus"]["regions"].as_array().unwrap();
        let region_ids: std::collections::BTreeSet<String> = regions.iter().map(|r| r["id"].as_str().unwrap().to_string()).collect();
        let mut records: Vec<Value> = regions
            .iter()
            .map(|r| json!({"id": label(r["id"].as_str().unwrap()), "type": "region", "handle": r["handle"], "boundary": r["boundary"]}))
            .collect();
        let mut nc = Vec::new();
        let mut clusters = 0;
        for (i, c) in seed["clusters"].as_array().unwrap().iter().enumerate() {
            let recs = cluster_records(c, &format!("clusters.json:clusters[{i}]"), &label, &|r| region_ids.contains(r), &mut nc);
            clusters += recs.len();
            records.extend(recs);
        }
        assert_eq!(clusters, 9, "five clusters and four children: {nc:?}");
        assert!(nc.iter().any(|e| e["what"] == "cluster-part" && e["key"] == "tasks"), "{nc:?}");
        let (_tmp, t) = older_store("seed");
        assert!(!crate::clusters::has_pool(&t.cauldron));
        let o = ImportOptions { remint: true, agent: "test".into(), dispatch: "test".into(), ..Default::default() };
        let doc = match import(&t, &records, &o).unwrap() {
            ImportOutcome::Judged { doc, ok: true } => doc,
            other => panic!("{other:?}"),
        };
        assert_eq!(doc["outcome"], "landed", "{doc}");
        assert!(crate::clusters::has_pool(&t.cauldron), "the older store got the cluster pool");
        let map = &doc["id_map"];
        let x1 = map["v5:cluster:X-1"].as_str().unwrap();
        let r1 = map["v5:R-1"].as_str().unwrap();
        assert!(x1.starts_with("CL-"), "{doc}");
        assert_eq!(crate::clusters::count(&t.cauldron).unwrap(), 9);
        let mut cut = crate::cut::Cut::new(None);
        let claimants = crate::clusters::claimants(&t.cauldron, r1, &mut cut).unwrap();
        assert!(claimants.iter().any(|c| c["id"] == x1), "R-1 names X-1's new id: {claimants:?}");
        let (got, missing) = crate::clusters::get(&t.cauldron, &[x1.to_string()], &mut cut).unwrap();
        assert!(missing.is_empty());
        assert_eq!(got[0]["claims"][0]["region"], json!(r1), "{got:?}");
        assert_eq!(got[0]["meta"]["imported_from"]["id"], "X-1");
    }
}
