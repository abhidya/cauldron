//! Clusters at the door: `soup_put` routes `cluster` records to the soup's
//! cluster pool in the same all-or-nothing write as workspace records (and
//! gives a store made before clusters that pool on its first cluster write),
//! `soup_claim` adds and releases a claim idempotently under `write`, a
//! claim into a region a ripple holds is refused as a link is, every record
//! guard holds for clusters, and a ripple writes neither a cluster nor a
//! claim.

use serde_json::{json, Value};
use soup_door::grants::Scope;

use crate::support::*;

const BOUNDARY: &str = "Inside: everything that gets a release to the people who use it, end to end.";

fn cluster(label: &str, handle: &str, claims: &[&str]) -> Value {
    let edges: Vec<Value> = claims.iter().map(|r| json!({"kind": "claims", "to": r})).collect();
    json!({"id": label, "type": "cluster", "handle": handle, "boundary": BOUNDARY, "edges": edges})
}

fn region(label: &str) -> Value {
    json!({"id": label, "type": "region", "handle": "Packaging", "boundary": "Inside: how a release is packaged. Outside: how it is built."})
}

fn has_pool(s: &Scratch) -> bool {
    s.soup("kb").join("cauldron.db").is_file()
}

fn claims_of(s: &Scratch, cluster: &str) -> Vec<String> {
    let conn = spine::offline::read_only_connection(&s.soup("kb").join("cauldron.db")).unwrap();
    let mut st = conn.prepare("SELECT dst FROM edges WHERE src = ?1 AND kind = 'claims' ORDER BY dst").unwrap();
    st.query_map([cluster], |r| r.get::<_, String>(0)).unwrap().map(Result::unwrap).collect()
}

fn setup(tag: &str, scopes: &[Scope]) -> (Scratch, Service, Value) {
    let s = Scratch::new(tag, &["kb"]);
    s.import("kb", &example());
    s.grant("kb", scopes, &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    (s, svc, c)
}

fn claim(svc: &Service, c: &Value, args: Value) -> Value {
    let mut a = args;
    a["cauldron"] = json!("kb");
    svc.call("soup_claim", a, c)
}

/// A store made before clusters (`ws` only): a dry run of a cluster write is
/// judged on a copy and adds no pool; the first real one adds the pool and
/// lands a new region, a cluster and its claims of the new region and of a
/// held one in ONE write; a batch whose claim names a non-region refuses
/// whole and writes nothing.
#[test]
fn ck1_soup_put_routes_clusters_and_lands_them_with_regions_in_one_write() {
    let (s, svc, c) = setup("ck1", &[Scope::Read, Scope::Write]);
    assert!(!has_pool(&s), "the fixture is a store made before clusters");
    let batch = json!([region("r-new"), cluster("k1", "Shipping", &["r-new", "R-1"])]);
    let dry = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "dry_run": true, "records": batch}), &c));
    assert_eq!(dry["outcome"], "validated", "{dry}");
    assert!(!has_pool(&s), "a dry run writes no pool");
    let before = s.node_count("kb");
    let landed = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": batch}), &c));
    assert_eq!(landed["outcome"], "landed", "{landed}");
    assert_eq!(landed["id_map"]["k1"], "CL-1", "{landed}");
    let new_region = landed["id_map"]["r-new"].as_str().unwrap().to_string();
    assert!(has_pool(&s));
    assert_eq!(s.node_count("kb"), before + 1, "the region in ws");
    let mut want = vec!["R-1".to_string(), new_region];
    want.sort();
    assert_eq!(claims_of(&s, "CL-1"), want);
    // One spine row for the whole batch.
    assert_eq!(landed["landed"]["seq"].as_u64().is_some(), true);
    // A claim of a node that is not a region refuses the whole batch.
    let (code, _, detail) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [region("r2"), cluster("k2", "Bad", &["T-1"])]}), &c));
    assert_eq!(code, "records_refused", "{detail}");
    assert_eq!(s.node_count("kb"), before + 1, "nothing of the refused batch landed");
    // A cluster carries its citation, the session's included.
    let rec = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "cite": {"kind": "url", "url": "https://example.com/plan"}, "records": [cluster("k3", "Docs", &[])]}), &c));
    assert_eq!(rec["outcome"], "landed", "{rec}");
}

/// `soup_claim`: claims, says so again without writing, releases, and says
/// so again; refuses an unknown cluster, a region the soup does not hold
/// and a node that is not a region; every call is audited.
#[test]
fn ck2_soup_claim_is_idempotent_and_audited() {
    let (s, svc, c) = setup("ck2", &[Scope::Read, Scope::Write]);
    // No cluster pool yet: nothing to claim with, and no pool made.
    let (code, msg, _) = refused(&claim(&svc, &c, json!({"cluster": "CL-1", "region": "R-1"})));
    assert_eq!(code, "not_found", "{msg}");
    assert!(!has_pool(&s));
    ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [cluster("k1", "Shipping", &[])]}), &c));
    let a = ok(&claim(&svc, &c, json!({"cluster": "CL-1", "region": "R-1", "note": "all of it"})));
    assert_eq!((a["outcome"].as_str(), a["written"].as_bool()), (Some("claimed"), Some(true)), "{a}");
    assert!(a["cursor"].as_str().unwrap().starts_with("v1."), "{a}");
    assert_eq!(claims_of(&s, "CL-1"), ["R-1"]);
    let again = ok(&claim(&svc, &c, json!({"cluster": "CL-1", "region": "R-1"})));
    assert_eq!((again["outcome"].as_str(), again["written"].as_bool()), (Some("already-claimed"), Some(false)), "{again}");
    assert_eq!(again["note"], "all of it");
    let r = ok(&claim(&svc, &c, json!({"cluster": "CL-1", "region": "R-1", "release": true})));
    assert_eq!(r["outcome"], "released", "{r}");
    assert!(claims_of(&s, "CL-1").is_empty());
    let r = ok(&claim(&svc, &c, json!({"cluster": "CL-1", "region": "R-1", "release": true})));
    assert_eq!((r["outcome"].as_str(), r["written"].as_bool()), (Some("not-claimed"), Some(false)), "{r}");
    for (args, want) in [
        (json!({"cluster": "CL-9", "region": "R-1"}), "not_found"),
        (json!({"cluster": "CL-1", "region": "R-99"}), "not_found"),
        (json!({"cluster": "CL-1", "region": "T-1"}), "usage"),
        (json!({"cluster": "CL-1"}), "usage"),
        (json!({"cluster": "CL-1", "region": "R-1", "release": true, "note": "x"}), "usage"),
        (json!({"cluster": "CL-1", "region": "R-1", "lease": true}), "usage"),
    ] {
        let (code, msg, _) = refused(&claim(&svc, &c, args.clone()));
        assert_eq!(code, want, "{args}: {msg}");
    }
    let calls: Vec<Value> = s.calls().into_iter().filter(|l| l["tool"] == "soup_claim").collect();
    let outcomes: Vec<&str> = calls.iter().map(|l| l["outcome"].as_str().unwrap()).collect();
    assert!(outcomes.starts_with(&["not_found", "claimed", "already-claimed", "released", "not-claimed"]), "{outcomes:?}");
    assert!(calls[1]["tx"].is_string(), "the claim's write is named in the audit: {}", calls[1]);
}

/// A claim needs `write`; a read-only grant refuses it.
#[test]
fn ck3_soup_claim_needs_the_write_scope() {
    let (_s, svc, c) = setup("ck3", &[Scope::Read]);
    let (code, _, _) = refused(&claim(&svc, &c, json!({"cluster": "CL-1", "region": "R-1"})));
    assert_eq!(code, "not_granted");
}

/// The owner's review flags are refused on a cluster record as on any
/// other, before anything is written.
#[test]
fn ck4_a_cluster_record_cannot_carry_a_review_flag() {
    let (s, svc, c) = setup("ck4", &[Scope::Read, Scope::Write]);
    for flag in ["approved", "rejected"] {
        let mut k = cluster("k1", "Shipping", &[]);
        k["meta"] = json!({flag: {"at": "2026-01-01", "by": "owner"}});
        let (code, msg, _) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [k]}), &c));
        assert_eq!(code, "usage", "{msg}");
        assert!(msg.contains("review flags"), "{msg}");
    }
    assert!(!has_pool(&s), "a refused batch adds no pool");
}

fn key(s: &Scratch) -> String {
    std::fs::read_to_string(s.mcp_dir().join("ripples.secret")).unwrap().trim().to_string()
}

fn say(svc: &Service, s: &Scratch, verb: &str, args: Value) -> Value {
    serde_json::from_str(&seam_say(svc.seam, &format!("{verb} {} {args}", key(s)))).unwrap()
}

/// A ripple holds R-1: a claim of R-1 is refused `region_locked` at once
/// with nothing written, as a link into it is; a claim of R-2 lands; once
/// the ripple lets go, the claim of R-1 lands. A ripple's own batch may hold
/// no cluster and no claim, and says why.
#[test]
fn ck5_a_held_region_refuses_a_claim_and_a_ripple_writes_no_cluster() {
    let (s, svc, c) = setup("ck5", &[Scope::Read, Scope::Write, Scope::Dump]);
    ok(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "a note about the schema"}), &c));
    ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [cluster("k1", "Shipping", &[])]}), &c));
    let a = say(&svc, &s, "RLOCK", json!({"cauldron": "kb", "run": "run-a", "dump": "DM-1", "regions": ["R-1"], "ttl_secs": 120}));
    assert_eq!(a["outcome"], "locked", "{a}");
    let t0 = std::time::Instant::now();
    let (code, msg, detail) = refused(&claim(&svc, &c, json!({"cluster": "CL-1", "region": "R-1"})));
    assert!(t0.elapsed() < std::time::Duration::from_secs(5), "a refusal, not a wait");
    assert_eq!(code, "region_locked", "{msg}");
    assert!(msg.contains("R-1") && msg.contains("run-a"), "{msg}");
    assert!(detail["retry_after_secs"].as_i64().is_some(), "{detail}");
    assert!(claims_of(&s, "CL-1").is_empty(), "nothing was written");
    ok(&claim(&svc, &c, json!({"cluster": "CL-1", "region": "R-2"})));
    // A ripple's batch: a cluster, and a claims edge, each refused by name.
    let put = json!({"cauldron": "kb", "dump": "DM-1", "run": "run-a", "step": "put", "package": "dump", "version": "v1", "model": "sonnet", "revs": a["revs"],
        "records": [cluster("n1", "From a dump", &["R-1"]),
                    {"id": "n2", "type": "idea", "state": "idea", "handle": "an idea", "edges": [{"kind": "in_region", "to": "R-1"}, {"kind": "claims", "to": "R-1"}]}]});
    let p = say(&svc, &s, "RPUT", put);
    assert_eq!(p["ok"], json!(false), "{p}");
    let text = p.to_string();
    assert!(text.contains("a ripple writes no cluster and no claim in this release"), "{p}");
    assert!(text.contains("`cluster`") && text.contains("a `claims` edge"), "{p}");
    let r = say(&svc, &s, "RUNLOCK", json!({"cauldron": "kb", "run": "run-a"}));
    assert_ne!(r["ok"], json!(false), "{r}");
    ok(&claim(&svc, &c, json!({"cluster": "CL-1", "region": "R-1"})));
    assert_eq!(claims_of(&s, "CL-1"), ["R-1", "R-2"]);
}

/// The reads: `soup_region.clusters` from the claims' reverse edges,
/// `soup_nodes {type: cluster}`, `soup_get CL-n` with its claims,
/// `soup_search` over cluster handles and boundaries, `soup_status` counts,
/// and `soup_changes` with the cluster's creation, a claim (`link`) and a
/// release (`unlink`) on the one cursor, every answer bounded.
#[test]
fn ck6_the_reads_answer_clusters_and_the_change_feed_reports_their_writes() {
    let (_s, svc, c) = setup("ck6", &[Scope::Read, Scope::Write]);
    let call = |tool: &str, mut a: Value| -> Value {
        a["cauldron"] = json!("kb");
        ok(&svc.call(tool, a, &c))
    };
    let start = call("soup_status", json!({}))["cursor"].as_str().unwrap().to_string();
    let r = call("soup_region", json!({"id": "R-1"}));
    assert_eq!(r["clusters"], json!([]), "{r}");
    call("soup_put", json!({"remint": true, "records": [cluster("k1", "Shipping lane", &["R-1"]), cluster("k2", "Schema work", &["R-1", "R-2"])]}));
    call("soup_claim", json!({"cluster": "CL-1", "region": "R-2", "note": "only the install half"}));
    call("soup_claim", json!({"cluster": "CL-2", "region": "R-2", "release": true}));
    let r = call("soup_region", json!({"id": "R-1"}));
    assert_eq!(r["clusters"], json!([{"id": "CL-1", "handle": "Shipping lane"}, {"id": "CL-2", "handle": "Schema work"}]), "{r}");
    let r = call("soup_region", json!({"id": "R-2"}));
    assert_eq!(r["clusters"], json!([{"id": "CL-1", "handle": "Shipping lane", "note": "only the install half"}]), "{r}");
    let n = call("soup_nodes", json!({"type": "cluster"}));
    assert_eq!(n["total"], 2, "{n}");
    assert_eq!(n["nodes"][0]["type"], "cluster");
    let ids: Vec<&str> = n["nodes"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["CL-2", "CL-1"], "newest first (one write: by serial, highest first)");
    let n = call("soup_nodes", json!({"type": "cluster", "region": "R-2"}));
    assert_eq!(n["nodes"].as_array().unwrap().len(), 1, "{n}");
    let n = call("soup_nodes", json!({"type": "cluster", "limit": 1}));
    assert_eq!((n["nodes"].as_array().unwrap().len(), n["more"].as_u64()), (1, Some(1)), "{n}");
    let g = call("soup_get", json!({"ids": ["CL-1", "R-1", "CL-9"]}));
    let k = g["records"].as_array().unwrap().iter().find(|x| x["id"] == "CL-1").unwrap();
    assert_eq!(k["type"], "cluster");
    assert_eq!(k["boundary"], BOUNDARY);
    assert_eq!(k["claims"].as_array().unwrap().len(), 2, "{k}");
    assert_eq!((k["claims"][0]["region"].clone(), k["claims"][1]["region"].clone()), (json!("R-1"), json!("R-2")), "{k}");
    assert_eq!(k["claims"][1]["note"], "only the install half", "{k}");
    assert!(k["claims"][0]["handle"].is_string(), "each claim names its region's handle: {k}");
    assert!(k["edges"].as_array().unwrap().iter().any(|e| e["to"] == "R-1@ws"), "{k}");
    assert_eq!(g["missing"], json!(["CL-9"]));
    let f = call("soup_search", json!({"query": "shipping lane"}));
    assert_eq!(f["results"][0]["id"], "CL-1", "{f}");
    assert_eq!(f["results"][0]["regions"], json!(["R-1", "R-2"]), "{f}");
    let f = call("soup_search", json!({"query": "release end", "type": "cluster"}));
    assert!(f["results"].as_array().unwrap().iter().all(|x| x["type"] == "cluster") && f["total"] == 2, "{f}");
    let st = call("soup_status", json!({}));
    assert_eq!((st["clusters"].clone(), st["cluster_schema"].clone()), (json!(2), json!("cauldron@1")), "{st}");
    assert!(st["counts"].get("cluster").is_none(), "`counts` and `total` are the workspace nodes, as before clusters: {st}");
    assert_eq!(st["total"], st["counts"].as_object().unwrap().values().map(|v| v.as_u64().unwrap()).sum::<u64>(), "{st}");
    assert_eq!(g["cluster_pool"], "cauldron", "{g}");
    assert_eq!(f["cluster_pool"], "cauldron", "{f}");
    assert_eq!(st["schema"], "workspace@6");
    let ch = call("soup_changes", json!({"since": start}));
    let ops: Vec<(String, String)> = ch["changes"].as_array().unwrap().iter().map(|x| (x["op"].as_str().unwrap().to_string(), x["ids"][0]["id"].as_str().unwrap_or("").to_string())).collect();
    assert_eq!(ops, [("create".into(), "CL-1".into()), ("link".into(), "CL-1".into()), ("unlink".into(), "CL-2".into())], "{ch}");
    assert_eq!(ch["changes"][0]["count"], 2);
    // Bounded like every read.
    let b = call("soup_search", json!({"query": "release", "max_chars": 300}));
    assert!(serde_json::to_string(&b).unwrap().len() <= 300 + 200, "{b}");
    assert!(b["truncated"].is_object(), "{b}");
}

/// Readers hammering while mixed batches land: one session puts batch after
/// batch, each a new region, a cluster and its claims of that region and of
/// R-1 (one write over both pools); three others list the clusters, get
/// them and search them as fast as they can. No answer ever names a claim
/// of a region it could not read: every claim a record carries has its
/// region's handle, and every region a listing names answers soup_region.
#[test]
fn ck7_readers_never_see_a_claim_before_its_region_while_mixed_batches_land() {
    let (_s, svc, c) = setup("ck7", &[Scope::Read, Scope::Write]);
    let done = std::sync::atomic::AtomicBool::new(false);
    let seen = std::sync::atomic::AtomicUsize::new(0);
    let refused = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|sc| {
        sc.spawn(|| {
            for i in 0..25 {
                let batch = json!([region(&format!("r{i}")), cluster(&format!("k{i}"), "Shipping", &[&format!("r{i}"), "R-1"])]);
                let r = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": batch}), &c));
                assert_eq!(r["outcome"], "landed", "{r}");
            }
            done.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        for _ in 0..3 {
            sc.spawn(|| {
                // Every read answers: a refusal of any kind (a `store`
                // refusal included, which a read opening a pool file while a
                // write was checkpointed into it used to give) is counted and
                // fails the test.
                let call = |tool: &str, mut a: Value| -> Value {
                    a["cauldron"] = json!("kb");
                    let r = svc.call(tool, a.clone(), &c);
                    if r["isError"] == json!(true) {
                        refused.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        eprintln!("ck7: {tool} refused: {r}");
                    }
                    ok(&r)
                };
                while !done.load(std::sync::atomic::Ordering::SeqCst) {
                    let n = call("soup_nodes", json!({"type": "cluster", "limit": 1000}));
                    let ids: Vec<Value> = n["nodes"].as_array().unwrap().iter().map(|x| x["id"].clone()).collect();
                    if ids.is_empty() {
                        continue;
                    }
                    let g = call("soup_get", json!({"ids": ids, "max_chars": 1_000_000}));
                    for k in g["records"].as_array().unwrap() {
                        for cl in k["claims"].as_array().unwrap() {
                            assert!(cl["handle"].is_string(), "a claim read before its region: {k}");
                            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                    for x in n["nodes"].as_array().unwrap().iter().take(3) {
                        for r in x["regions"].as_array().unwrap() {
                            call("soup_region", json!({"id": r}));
                        }
                    }
                    let f = call("soup_search", json!({"query": "shipping", "type": "cluster", "limit": 100}));
                    assert!(f["results"].is_array(), "{f}");
                }
            });
        }
    });
    assert!(seen.load(std::sync::atomic::Ordering::SeqCst) > 0, "the readers read claims");
    assert_eq!(refused.load(std::sync::atomic::Ordering::SeqCst), 0, "a read was refused while writes landed");
    eprintln!("ck7: {} claims read, no read refused", seen.into_inner());
}

/// A claim's note, and every edge note, is bounded at the door at 1,000
/// characters for a new write (soup_claim, soup_put, soup_link), refused
/// whole with nothing written. A longer note already stored (written before
/// the bound) is neither refused nor rewritten: every read shows it cut to
/// the bound and names the cut in `truncated`, an `already-claimed` answer
/// echoes it cut, and a bounded read stays within its `max_chars`.
#[test]
fn ck8_edge_notes_are_bounded_at_the_door_and_cut_on_the_way_out() {
    let (s, svc, c) = setup("ck8", &[Scope::Read, Scope::Write]);
    let call = |tool: &str, mut a: Value| -> Value {
        a["cauldron"] = json!("kb");
        svc.call(tool, a, &c)
    };
    ok(&call("soup_put", json!({"remint": true, "records": [cluster("k1", "Shipping", &[])]})));
    let huge = "n".repeat(300_000);
    let (code, msg, _) = refused(&call("soup_claim", json!({"cluster": "CL-1", "region": "R-1", "note": huge})));
    assert_eq!(code, "usage", "{msg}");
    assert!(msg.contains("300000 characters") && msg.contains("at most 1000") && msg.contains("Nothing was written"), "{msg}");
    assert!(msg.len() < 1_000, "the refusal does not carry the note back");
    assert!(claims_of(&s, "CL-1").is_empty());
    let at_bound = "b".repeat(1_000);
    let a = ok(&call("soup_claim", json!({"cluster": "CL-1", "region": "R-1", "note": at_bound})));
    assert_eq!(a["outcome"], "claimed", "{a}");
    // Record edges and links are bounded the same way.
    let mut k2 = cluster("k2", "Docs", &["R-1"]);
    k2["edges"][0]["note"] = json!("x".repeat(1_001));
    let (code, msg, _) = refused(&call("soup_put", json!({"remint": true, "records": [k2]})));
    assert_eq!(code, "usage", "{msg}");
    assert!(msg.contains("record `k2`'s edge 0"), "{msg}");
    let (code, msg, _) = refused(&call("soup_link", json!({"links": [{"from": "T-1", "kind": "ref", "to": "R-1", "note": "y".repeat(1_001)}]})));
    assert_eq!(code, "usage", "{msg}");
    // A note stored before the bound: planted straight into the pool file.
    {
        let conn = rusqlite::Connection::open(s.soup("kb").join("cauldron.db")).unwrap();
        conn.execute("UPDATE edges SET note = ?1 WHERE src = 'CL-1' AND kind = 'claims'", [&huge]).unwrap();
    }
    let g = ok(&call("soup_get", json!({"ids": ["CL-1"]})));
    let k = &g["records"][0];
    assert_eq!(k["claims"][0]["note"].as_str().unwrap().chars().count(), 1_000, "{}", &g.to_string()[..400]);
    assert!(g.to_string().contains("claims/R-1/note"), "the cut is named: {}", &g.to_string()[..600]);
    let r = ok(&call("soup_region", json!({"id": "R-1"})));
    assert_eq!(r["clusters"][0]["note"].as_str().unwrap().chars().count(), 1_000);
    let again = ok(&call("soup_claim", json!({"cluster": "CL-1", "region": "R-1"})));
    assert_eq!(again["outcome"], "already-claimed");
    assert_eq!(again["note"].as_str().unwrap().chars().count(), 1_000, "the echo is cut");
    assert!(again["truncated"].is_array(), "{}", &again.to_string()[..300]);
    for tool in ["soup_get", "soup_region"] {
        let a = if tool == "soup_get" { json!({"ids": ["CL-1"], "max_chars": 2_000}) } else { json!({"id": "R-1", "max_chars": 2_000}) };
        let b = ok(&call(tool, a));
        assert!(serde_json::to_string(&b).unwrap().chars().count() <= 2_000, "{tool} over its bound");
    }
}

/// The first cluster write of a store made before clusters adds the cluster
/// pool only when the batch lands a cluster: a batch the grammar refuses
/// (a boundary too short, a claim of a region nobody holds) adds nothing,
/// not even the pool, and says what it was judged on; a refused claim names
/// both pools it looked in.
#[test]
fn ck9_a_refused_first_cluster_write_adds_no_pool() {
    let (s, svc, c) = setup("ck9", &[Scope::Read, Scope::Write]);
    assert!(!has_pool(&s));
    let mut short = cluster("k1", "Shipping", &[]);
    short["boundary"] = json!("too short");
    let (code, _, detail) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [short]}), &c));
    assert_eq!(code, "records_refused", "{detail}");
    assert_eq!(detail["dry_run"], json!(false), "{detail}");
    assert!(detail["judged_on"].as_str().unwrap().contains("the pool was not added"), "{detail}");
    assert!(!has_pool(&s), "a refused batch adds no pool");
    let (code, _, detail) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [cluster("k2", "Shipping", &["R-77"])]}), &c));
    assert_eq!(code, "records_refused");
    let why = detail.to_string();
    assert!(why.contains("pool `cauldron` nor of pool `ws`"), "{why}");
    assert!(!has_pool(&s));
    // Partial: the region lands; the refused cluster adds no pool.
    let mut bad = cluster("k3", "Shipping", &[]);
    bad["boundary"] = json!("too short");
    let p = ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "partial": true, "records": [region("r9"), bad]}), &c));
    assert_eq!(p["outcome"], "landed", "{p}");
    assert!(!has_pool(&s), "a partial batch landing no cluster adds no pool");
    // And a batch that lands a cluster still adds it.
    ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [cluster("k4", "Shipping", &["R-1"])]}), &c));
    assert!(has_pool(&s));
}
