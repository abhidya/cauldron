//! The consumer's interface table (issue #10, "The interface we want"), row
//! by row: each tool called exactly as the table writes it, with no
//! `cauldron` argument (the cauldron is resolved from the session's project,
//! as the table asks) and no argument the table does not name, and the
//! reply read for the fields the table names.

use serde_json::{json, Value};
use soup_door::grants::Scope;

use crate::support::*;

const BOUNDARY: &str = "Inside: everything that gets a release to the people who use it, end to end.";

/// The worked example in `kb`, granted to the project `p` (so a call that
/// names no cauldron reaches it), and the service.
fn setup(tag: &str) -> (Scratch, Service, Value) {
    let s = Scratch::new(tag, &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &[Scope::Read, Scope::Write], &[&s.project("p")]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    (s, svc, c)
}

/// The cauldron was chosen by the session's project, not named.
fn by_project(doc: &Value) {
    let from = doc["resolved_from"].as_str().unwrap_or_default();
    assert!(from.contains("project"), "resolved_from: {doc}");
}

/// `soup_search {query, region?, type?, limit, max_chars}` → ranked
/// `[{id, type, handle, regions, score}]`.
#[test]
fn if1_soup_search_as_the_table_writes_it() {
    let (_s, svc, c) = setup("if1");
    let all = ok(&svc.call("soup_search", json!({"query": "import", "limit": 10, "max_chars": 4000}), &c));
    by_project(&all);
    let hits = all["results"].as_array().expect("ranked results");
    assert!(!hits.is_empty(), "{all}");
    for h in hits {
        for k in ["id", "type", "handle", "regions", "score"] {
            assert!(h.get(k).is_some(), "a hit lacks `{k}`: {h}");
        }
    }
    let scores: Vec<f64> = hits.iter().map(|h| h["score"].as_f64().unwrap()).collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]), "best first: {scores:?}");
    // The optional two narrow it.
    let narrow = ok(&svc.call("soup_search", json!({"query": "import", "region": "R-2", "type": "task", "limit": 10, "max_chars": 4000}), &c));
    let hits = narrow["results"].as_array().unwrap();
    assert!(!hits.is_empty(), "{narrow}");
    assert!(hits.iter().all(|h| h["type"] == "task" && h["regions"].as_array().unwrap().contains(&json!("R-2"))), "{narrow}");
}

/// `soup_get {ids[], max_chars}` → full records.
#[test]
fn if2_soup_get_as_the_table_writes_it() {
    let (_s, svc, c) = setup("if2");
    let got = ok(&svc.call("soup_get", json!({"ids": ["T-1", "DEC-1"], "max_chars": 8000}), &c));
    by_project(&got);
    let recs = got["records"].as_array().unwrap();
    assert_eq!(recs.len(), 2, "{got}");
    let dec = recs.iter().find(|r| r["id"] == "DEC-1").unwrap();
    for k in ["type", "state", "handle", "edges", "meta"] {
        assert!(dec.get(k).is_some(), "a whole record has `{k}`: {dec}");
    }
}

/// `soup_region {id, depth}` → the region, its `narrows` parents and
/// children, member handles, and the clusters that claim it.
#[test]
fn if3_soup_region_as_the_table_writes_it() {
    let (_s, svc, c) = setup("if3");
    // A child region of R-2, and a cluster claiming R-2.
    let put = ok(&svc.call(
        "soup_put",
        json!({"remint": true, "records": [
            {"id": "child", "type": "region", "handle": "Import errors", "boundary": "Inside: what an import says when it refuses.",
             "edges": [{"kind": "narrows", "to": "R-2"}]},
            {"id": "k", "type": "cluster", "handle": "Shipping", "boundary": BOUNDARY, "edges": [{"kind": "claims", "to": "R-2"}]}]}),
        &c,
    ));
    let child = put["id_map"]["child"].as_str().unwrap().to_string();
    let r = ok(&svc.call("soup_region", json!({"id": "R-2", "depth": 1}), &c));
    by_project(&r);
    assert_eq!(r["region"]["id"], "R-2", "{r}");
    assert!(r["parents"].is_array(), "{r}");
    assert_eq!(r["children"][0]["id"], json!(child), "{r}");
    let members = r["members"].as_array().unwrap();
    assert!(!members.is_empty() && members.iter().all(|m| m["handle"].is_string()), "{r}");
    assert_eq!(r["clusters"][0]["handle"], "Shipping", "{r}");
}

/// `soup_put {records[], cite}` → validated write of regions and nodes,
/// with per-record errors. Written as the table writes it: records whose
/// ids the batch names, one `cite`.
#[test]
fn if4_soup_put_as_the_table_writes_it() {
    let (s, svc, c) = setup("if4");
    let before = s.node_count("kb");
    let cite = json!({"kind": "url", "url": "https://example.com/plan"});
    let records = json!([
        {"id": "R-3", "type": "region", "handle": "Release notes", "boundary": "Inside: what each release says it changed."},
        {"id": "T-2", "type": "task", "state": "ready", "handle": "Write the notes", "edges": [{"kind": "in_region", "to": "R-3"}]},
    ]);
    let d = ok(&svc.call("soup_put", json!({"records": records, "cite": cite}), &c));
    by_project(&d);
    assert_eq!(d["outcome"], "landed", "{d}");
    assert_eq!(s.node_count("kb"), before + 2);
    let got = ok(&svc.call("soup_get", json!({"ids": ["T-2"]}), &c));
    assert_eq!(got["records"][0]["meta"]["cites"][0], cite, "{got}");
    // Per-record errors: the bad record is named with its reasons, and
    // nothing lands.
    let bad = json!([
        {"id": "T-3", "type": "task", "state": "ready", "handle": "Fine", "edges": [{"kind": "in_region", "to": "R-1"}]},
        {"id": "T-4", "type": "task", "state": "no-such-state", "handle": "Not fine", "edges": [{"kind": "in_region", "to": "R-1"}]},
    ]);
    let (code, _, detail) = refused(&svc.call("soup_put", json!({"records": bad, "cite": cite}), &c));
    assert_eq!(code, "records_refused", "{detail}");
    let b = detail["records"].as_array().unwrap().iter().find(|r| r["id"] == "T-4").expect("named by id").clone();
    assert_eq!(b["disposition"], "refused", "{detail}");
    assert!(b["reasons"].as_array().is_some_and(|r| !r.is_empty()), "{detail}");
    assert_eq!(s.node_count("kb"), before + 2);
    // Not an upsert: a record the soup holds is never written over (a
    // change is a new record that supersedes it), and the refusal says so.
    let again = json!([{"id": "T-2", "type": "task", "state": "done", "handle": "Write the notes", "edges": [{"kind": "in_region", "to": "R-3"}]}]);
    let (code, _, detail) = refused(&svc.call("soup_put", json!({"records": again, "cite": cite}), &c));
    assert_eq!(code, "records_refused", "{detail}");
    assert!(detail["records"][0]["hint"].as_str().unwrap().contains("supersedes"), "{detail}");
    // A label without remint says what remint is for.
    let (_, _, detail) = refused(&svc.call("soup_put", json!({"records": [{"id": "t", "type": "task", "state": "ready", "handle": "A label", "edges": [{"kind": "in_region", "to": "R-1"}]}]}), &c));
    assert!(detail["records"][0]["hint"].as_str().unwrap().contains("remint"), "{detail}");
}

/// `soup_link {from, kind, to}` → a typed node-to-node ref: the flat form
/// is one link; the list form still lands several; both at once is refused,
/// naming the clash.
#[test]
fn if5_soup_link_as_the_table_writes_it() {
    let (s, svc, c) = setup("if5");
    let d = ok(&svc.call("soup_link", json!({"from": "DEC-1", "kind": "ref", "to": "T-1"}), &c));
    by_project(&d);
    assert_eq!(d["outcome"], "linked", "{d}");
    assert_eq!(d["links"], json!([{"from": "DEC-1", "kind": "ref", "to": "T-1", "status": "linked"}]), "{d}");
    assert!(d["cursor"].as_str().unwrap().starts_with("v1."), "{d}");
    let got = ok(&svc.call("soup_get", json!({"ids": ["DEC-1"]}), &c));
    let edges = got["records"][0]["edges"].as_array().unwrap();
    assert!(edges.iter().any(|e| e["kind"] == "ref" && e["to"] == "T-1"), "{got}");
    // With a note, flat.
    let d = ok(&svc.call("soup_link", json!({"from": "DEC-1", "kind": "ref", "to": "Q-1", "note": "evidence"}), &c));
    assert_eq!(d["outcome"], "linked", "{d}");
    // The list form, as before.
    let d = ok(&svc.call("soup_link", json!({"links": [{"from": "M-1", "kind": "ref", "to": "T-1"}, {"from": "M-1", "kind": "ref", "to": "Q-1"}]}), &c));
    assert_eq!(d["links"].as_array().unwrap().len(), 2, "{d}");
    // Both forms in one call: refused as usage, naming the keys; nothing lands.
    let head = s.node_count("kb");
    let (code, msg, _) = refused(&svc.call("soup_link", json!({"from": "T-1", "kind": "ref", "to": "M-1", "links": [{"from": "T-1", "kind": "ref", "to": "Q-1"}]}), &c));
    assert_eq!(code, "usage", "{msg}");
    assert!(msg.contains("not both") && msg.contains("`from`") && msg.contains("`kind`") && msg.contains("`to`"), "{msg}");
    let got = ok(&svc.call("soup_get", json!({"ids": ["T-1"]}), &c));
    assert!(!got["records"][0]["edges"].as_array().unwrap().iter().any(|e| e["kind"] == "ref"), "nothing landed: {got}");
    assert_eq!(s.node_count("kb"), head);
    // A flat call missing a part is refused, and so is an unknown kind,
    // which lists the kinds there are.
    let (code, msg, _) = refused(&svc.call("soup_link", json!({"from": "T-1", "to": "M-1"}), &c));
    assert_eq!(code, "usage", "{msg}");
    let (code, _, detail) = refused(&svc.call("soup_link", json!({"from": "DEC-1", "kind": "evidence", "to": "IS-1"}), &c));
    assert_eq!(code, "links_refused", "{detail}");
    let kinds = &detail["links"][0]["refused"]["kinds"];
    assert!(kinds.as_array().unwrap().contains(&json!("ref")), "{detail}");
    // No call at all is still refused.
    let (code, _, _) = refused(&svc.call("soup_link", json!({}), &c));
    assert_eq!(code, "usage");
}

/// `soup_claim {cluster, region}`.
#[test]
fn if6_soup_claim_as_the_table_writes_it() {
    let (_s, svc, c) = setup("if6");
    let put = ok(&svc.call("soup_put", json!({"remint": true, "records": [{"id": "k", "type": "cluster", "handle": "Shipping", "boundary": BOUNDARY}]}), &c));
    let k = put["id_map"]["k"].as_str().unwrap().to_string();
    let d = ok(&svc.call("soup_claim", json!({"cluster": k, "region": "R-1"}), &c));
    by_project(&d);
    assert_eq!(d["outcome"], "claimed", "{d}");
}

/// `soup_changes {since}` → changes plus a cursor.
#[test]
fn if7_soup_changes_as_the_table_writes_it() {
    let (_s, svc, c) = setup("if7");
    let status = ok(&svc.call("soup_status", json!({}), &c));
    let since = status["cursor"].as_str().unwrap().to_string();
    ok(&svc.call("soup_link", json!({"from": "DEC-1", "kind": "ref", "to": "T-1"}), &c));
    let d = ok(&svc.call("soup_changes", json!({"since": since}), &c));
    by_project(&d);
    let changes = d["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "{d}");
    assert_eq!(changes[0]["op"], "link", "{d}");
    let next = d["cursor"].as_str().unwrap();
    assert_ne!(next, since);
    let again = ok(&svc.call("soup_changes", json!({"since": next}), &c));
    assert!(again["changes"].as_array().unwrap().is_empty(), "{again}");
}

/// `soup_status` → which cauldron and env resolved, counts, cursor; and,
/// as every read, it takes `max_chars`.
#[test]
fn if8_soup_status_as_the_table_writes_it() {
    let (_s, svc, c) = setup("if8");
    let d = ok(&svc.call("soup_status", json!({}), &c));
    by_project(&d);
    assert_eq!(d["cauldron"], "kb", "{d}");
    assert!(d["env"].is_string(), "{d}");
    assert!(d["counts"]["task"].as_u64().is_some() && d["total"].as_u64().is_some(), "{d}");
    assert!(d["cursor"].as_str().unwrap().starts_with("v1."), "{d}");
    let b = ok(&svc.call("soup_status", json!({"max_chars": 50_000}), &c));
    assert_eq!(b["cursor"], d["cursor"], "{b}");
    let tiny = ok(&svc.call("soup_status", json!({"max_chars": 256}), &c));
    assert!(tiny.to_string().chars().count() <= 256, "bounded: {tiny}");
}

