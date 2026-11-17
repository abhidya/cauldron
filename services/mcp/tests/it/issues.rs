//! The consumer's issues, through the door: each reproduction the CLI tests
//! run (`cli/tests/it/soup.rs`, the `i<n>_` tests), as `soup_put` and the
//! read tools answer it, so both doors say the same thing.

use serde_json::{json, Value};
use soup_door::grants::Scope;
use soup_door::reads::{self, ReadOpts};

use crate::support::*;

/// A scratch with the README example landed, granted every scope, and the
/// service started: (scratch, service, ctx).
fn ready(tag: &str) -> (Scratch, Service, Value) {
    let s = Scratch::new(tag, &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &Scope::ALL, &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    (s, svc, c)
}

/// Issue #19: a refused `remint` batch answers no `id_map` and no `new_id`.
#[test]
fn i19_a_refused_remint_answers_no_id_map() {
    let (s, svc, c) = ready("i19");
    let held = s.node_count("kb");
    let bad = vec![
        json!({"id": "CASE-1", "type": "idea", "state": "no-such-state", "handle": "A case that will not land",
               "edges": [{"kind": "in_region", "to": "R-1", "note": "the published schema"}]}),
        json!({"id": "PAT-1", "type": "decision", "state": "decided", "handle": "A pattern drawn from the case",
               "edges": [{"kind": "in_region", "to": "R-1", "note": "the published schema"}, {"kind": "derived_from", "to": "CASE-1"}]}),
    ];
    // A real put is refused as `records_refused`; its detail is the import's
    // own answer.
    let (code, _, d) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": bad}), &c));
    assert_eq!(code, "records_refused");
    assert!(d.get("id_map").is_none(), "{d}");
    assert!(d["records"].as_array().unwrap().iter().all(|r| r.get("new_id").is_none()), "{d}");
    // A dry run answers (not a refusal), and says `refused` the same way.
    let d = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "dry_run": true, "records": bad}), &c));
    assert_eq!(d["outcome"], "refused", "{d}");
    assert!(d.get("id_map").is_none(), "{d}");
    assert_eq!(s.node_count("kb"), held);
    // Opposed: a batch that lands answers its map.
    let mut good = bad.clone();
    good[0]["state"] = json!("idea");
    let d = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": good}), &c));
    assert_eq!(d["outcome"], "landed", "{d}");
    assert_eq!(d["id_map"]["PAT-1"], "DEC-2", "{d}");
}

/// The keys every refused batch carries (#20).
const REFUSED_SHAPE: [&str; 11] =
    ["env", "cauldron", "pool", "dry_run", "remint", "outcome", "refusal", "accepted", "refused", "records", "batch_digest"];

fn assert_refused_shape(d: &Value, refusal: &str) {
    for k in REFUSED_SHAPE {
        assert!(d.get(k).is_some(), "a refused batch without `{k}`: {d}");
    }
    assert_eq!(d["outcome"], "refused", "{d}");
    assert_eq!(d["refusal"], refusal, "{d}");
}

/// Issue #20: `batch_already_landed`'s detail has the shape of every refused
/// batch, as `records_refused`'s does.
#[test]
fn i20_both_refusals_carry_one_shape() {
    let (_s, svc, c) = ready("i20");
    let one = vec![json!({"id": "t", "type": "task", "state": "ready", "handle": "A task put twice", "edges": [{"kind": "in_region", "to": "R-1"}]})];
    let d = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": one}), &c));
    assert!(d.get("refusal").is_none(), "{d}");
    let (code, _, d) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": one}), &c));
    assert_eq!(code, "batch_already_landed");
    assert_refused_shape(&d, "batch-already-landed");
    assert_eq!((d["accepted"].as_u64(), d["refused"].as_u64(), d["records"].clone()), (Some(0), Some(1), json!([])), "{d}");
    let bad = vec![json!({"id": "b", "type": "task", "state": "no-such-state", "handle": "A bad task", "edges": [{"kind": "in_region", "to": "R-1"}]})];
    let (code, _, d) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": bad}), &c));
    assert_eq!(code, "records_refused");
    assert_refused_shape(&d, "records-refused");
    let d = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "dry_run": true, "records": bad}), &c));
    assert_refused_shape(&d, "records-refused");
}

/// Issue #18: `soup_put` names the unknown edge key, as the import does.
#[test]
fn i18_an_unknown_edge_key_beside_a_note_is_named() {
    let (_s, svc, c) = ready("i18");
    let rec = json!({"id": "n1", "type": "decision", "state": "decided", "handle": "A decision with a weighted region edge",
                     "edges": [{"kind": "in_region", "to": "R-1", "note": "the published schema", "weight": 1}]});
    let d = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "dry_run": true, "records": [rec.clone()]}), &c));
    let why = d["records"][0]["reasons"][0]["detail"].as_str().unwrap().to_string();
    assert!(why.contains("`weight` is not a field of `in_region` edges"), "{d}");
    let (code, _, d) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [rec]}), &c));
    assert_eq!(code, "records_refused");
    assert_eq!(d["records"][0]["reasons"][0]["detail"].as_str(), Some(why.as_str()), "{d}");
}

/// Issue #21: the read tools list a region or viewer without `state`.
#[test]
fn i21_a_region_or_viewer_is_listed_without_state() {
    let (_s, svc, c) = ready("i21");
    let d = ok(&svc.call("soup_nodes", json!({"cauldron": "kb", "type": "region"}), &c));
    assert!(!d["nodes"].as_array().unwrap().is_empty());
    assert!(d["nodes"].as_array().unwrap().iter().all(|n| n.get("state").is_none()), "{d}");
    let d = ok(&svc.call("soup_nodes", json!({"cauldron": "kb", "type": "viewer"}), &c));
    assert!(d["nodes"].as_array().unwrap().iter().all(|n| n.get("state").is_none()), "{d}");
    let d = ok(&svc.call("soup_search", json!({"cauldron": "kb", "query": "schema"}), &c));
    let r1 = d["results"].as_array().unwrap().iter().find(|n| n["id"] == "R-1").cloned().expect("R-1 matches");
    assert!(r1.get("state").is_none(), "{d}");
    // Opposed: a node of a type with states keeps it.
    let d = ok(&svc.call("soup_nodes", json!({"cauldron": "kb", "type": "issue"}), &c));
    assert!(d["nodes"].as_array().unwrap().iter().all(|n| n["state"].is_string()), "{d}");
}

/// Issue #15: `soup_put` refuses a bad citation as `bad-cite`, named by
/// record, in a dry run and for real; a bad `cite` argument refuses every
/// record it was added to; a good one lands.
#[test]
fn i15_a_bad_citation_refuses_its_record() {
    let (s, svc, c) = ready("i15");
    let held = s.node_count("kb");
    let rec = |label: &str, meta: Value| {
        json!({"id": label, "type": "idea", "state": "idea", "handle": format!("A case labelled {label}"), "meta": meta,
               "edges": [{"kind": "in_region", "to": "R-1", "note": "the published schema"}]})
    };
    let bad = vec![rec("c1", json!({"cites": [{"kind": "fiel", "path": "notes/run.log"}]})), rec("c2", json!({"cites": [{"kind": "commit", "repo": "cauldron"}]}))];
    let d = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "dry_run": true, "records": bad}), &c));
    assert_eq!(d["outcome"], "refused", "{d}");
    let (code, _, real) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": bad}), &c));
    assert_eq!(code, "records_refused");
    for d in [&d, &real] {
        for (i, label) in ["c1", "c2"].iter().enumerate() {
            assert_eq!(d["records"][i]["reasons"][0]["code"], "bad-cite", "{d}");
            assert!(d["records"][i]["reasons"][0]["detail"].as_str().unwrap().starts_with(&format!("`{label}`: meta.cites[0]")), "{d}");
        }
    }
    // A `cite` argument rides in every record's `meta.cites`, so a bad one
    // refuses each record it reached.
    let one = vec![rec("g", json!({"kind": "case"}))];
    let (code, _, d) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "cite": {"kind": "url"}, "records": one}), &c));
    assert_eq!(code, "records_refused");
    assert_eq!(d["records"][0]["reasons"][0]["code"], "bad-cite", "{d}");
    assert_eq!(s.node_count("kb"), held);
    // Opposed: a good citation, and a good `cite`, land.
    let d = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "cite": {"kind": "url", "url": "https://example.com/spec"},
                                             "records": [rec("g", json!({"cites": [{"kind": "dump", "id": "DM-3"}]}))]}), &c));
    assert_eq!(d["outcome"], "landed", "{d}");
}

/// Issue #16: the read tools mark and hide a superseded node, as the CLI
/// reads do, and the whole-reply budget bounds the answer as before.
#[test]
fn i16_reads_mark_and_hide_a_superseded_node() {
    let s = Scratch::new("i16", &["kb"]);
    s.import("kb", &example());
    let t = s.target("kb");
    let d = s.import("kb", &[json!({"id": "first", "type": "decision", "state": "decided", "handle": "Supersede test: the first version of the rule",
                                   "edges": [{"kind": "in_region", "to": "R-1", "note": "the published schema"}]})]);
    let old = d["id_map"]["first"].as_str().unwrap().to_string();
    let d = s.import("kb", &[json!({"id": "second", "type": "decision", "state": "decided", "handle": "Supersede test: the second version of the rule",
                                   "edges": [{"kind": "in_region", "to": "R-1", "note": "the published schema"},
                                             {"kind": "supersedes", "to": old, "note": "replaces the first version"}]})]);
    let new = d["id_map"]["second"].as_str().unwrap().to_string();
    s.grant("kb", &[Scope::Read], &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    let big = 50_000;
    let has = |list: &Value, id: &str| list.as_array().unwrap().iter().any(|n| n["id"] == id);

    let d = ok(&svc.call("soup_get", json!({"cauldron": "kb", "ids": [old]}), &c));
    assert_eq!(d["records"][0]["superseded_by"], json!([new]), "{d}");
    for (tool, args, list) in [
        ("soup_nodes", json!({"cauldron": "kb", "type": "decision", "max_chars": big}), "nodes"),
        ("soup_search", json!({"cauldron": "kb", "query": "supersede test", "max_chars": big}), "results"),
        ("soup_region", json!({"cauldron": "kb", "id": "R-1", "max_chars": big}), "members"),
    ] {
        let d = ok(&svc.call(tool, args.clone(), &c));
        assert!(!has(&d[list], &old) && has(&d[list], &new), "{tool}: {d}");
        assert_eq!(d["superseded_hidden"], 1, "{tool}: {d}");
        let mut all = args.clone();
        all["include_superseded"] = json!(true);
        let d = ok(&svc.call(tool, all, &c));
        assert!(has(&d[list], &old), "{tool}: {d}");
        assert_eq!(d["superseded_hidden"], 0, "{tool}: {d}");
        // The same answer as the library the CLI prints.
        let o = ReadOpts { include_superseded: true, ..ReadOpts::default() };
        let lib = match tool {
            "soup_nodes" => reads::nodes(&t, &ReadOpts { node_type: Some("decision".into()), ..o }).unwrap(),
            "soup_search" => reads::search(&t, "supersede test", &o).unwrap(),
            _ => reads::region(&t, "R-1", &o).unwrap(),
        };
        assert_eq!(d[list], lib[list], "{tool}");
    }
    // `include_superseded` is a flag.
    let (code, _, _) = refused(&svc.call("soup_nodes", json!({"cauldron": "kb", "include_superseded": "yes"}), &c));
    assert_eq!(code, "usage");
    // The whole-reply budget still bounds the answer, and keeps the count.
    let d = ok(&svc.call("soup_nodes", json!({"cauldron": "kb", "max_chars": 1000}), &c));
    assert!(serde_json::to_string(&d).unwrap().chars().count() <= 1000, "{d}");
    assert!(d["truncated"]["items_dropped"].as_u64().unwrap() > 0, "{d}");
    assert_eq!(d["superseded_hidden"], 1, "{d}");
}
