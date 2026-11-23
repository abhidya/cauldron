//! Region locks at the door: a ripple holds the regions it will write
//! into (`RLOCK`), every other writer into them is refused `region_locked`
//! at once with who holds them and until when, writes elsewhere land, two
//! holders of disjoint regions both land, the put takes the regions it
//! writes into, the head-revision check refuses a put whose regions'
//! boundaries changed (never one whose members did), and no lock outlives its expiry or its run.

use serde_json::{json, Value};
use soup_door::grants::Scope;

use crate::support::*;

fn key(s: &Scratch) -> String {
    std::fs::read_to_string(s.mcp_dir().join("ripples.secret")).unwrap().trim().to_string()
}

fn say(svc: &Service, s: &Scratch, verb: &str, args: Value) -> Value {
    serde_json::from_str(&seam_say(svc.seam, &format!("{verb} {} {args}", key(s)))).unwrap()
}

fn code(v: &Value) -> &str {
    assert_eq!(v["ok"], json!(false), "not refused: {v}");
    v["code"].as_str().unwrap()
}

fn idea(label: &str, handle: &str, region: &str) -> Value {
    json!({"id": label, "type": "idea", "state": "idea", "handle": handle, "edges": [{"kind": "in_region", "to": region}]})
}

fn put(run: &str, dump: &str, records: Value, revs: Option<&Value>) -> Value {
    let mut v = json!({"cauldron": "kb", "dump": dump, "run": run, "step": "put", "package": "dump", "version": "v1", "model": "sonnet", "records": records});
    if let Some(r) = revs {
        v["revs"] = r.clone();
    }
    v
}

fn lock(svc: &Service, s: &Scratch, run: &str, dump: &str, regions: &[&str], ttl: u64) -> Value {
    say(svc, s, "RLOCK", json!({"cauldron": "kb", "run": run, "dump": dump, "regions": regions, "ttl_secs": ttl}))
}

fn setup(tag: &str) -> (Scratch, Service, Value) {
    let s = Scratch::new(tag, &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &[Scope::Read, Scope::Write, Scope::Dump], &[]);
    let svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    ok(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "a note about the schema"}), &c));
    ok(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "a note about importing"}), &c));
    (s, svc, c)
}

/// A ripple holds R-1: a session's `soup_put` into R-2 lands at once, one
/// into R-1 is refused `region_locked` at once (not a wait), naming the
/// holder, since and until and a retry-after, with nothing written; a link
/// from R-2 to a node of R-1 is refused too (an edge names both ends); the
/// status and the regions listing show the lock; a second ripple is refused
/// R-1 and granted R-2, and both land at once; once the first is let go, the
/// session's write into R-1 lands.
#[test]
fn rl1_a_held_region_refuses_other_writers_at_once_and_every_other_region_takes_writes() {
    let (s, svc, c) = setup("rl1");
    let a = lock(&svc, &s, "run-a", "DM-1", &["R-1"], 120);
    assert_eq!(a["outcome"], "locked", "{a}");
    assert_eq!(a["lock"]["regions"], json!(["R-1"]));
    assert!(a["revs"]["R-1"].is_string() && a["revs"]["R-2"].is_string(), "{a}");
    assert!(a["regions"].as_array().unwrap().iter().all(|r| r["rev"].is_string()), "{a}");
    let before = s.node_count("kb");

    // Elsewhere: lands.
    ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [idea("x", "an import idea", "R-2")]}), &c));
    assert_eq!(s.node_count("kb"), before + 1);
    // The held region: refused at once.
    let t0 = std::time::Instant::now();
    let (code_, msg, detail) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [idea("y", "a schema idea", "R-1")]}), &c));
    assert!(t0.elapsed() < std::time::Duration::from_secs(5), "a refusal, not a wait");
    assert_eq!(code_, "region_locked", "{msg}");
    assert!(msg.contains("region `R-1` is locked by a ripple (run run-a, drawing records from dump DM-1)"), "{msg}");
    assert!(msg.contains("Nothing was written") && msg.contains("Try again in about"), "{msg}");
    assert_eq!(detail["locks"][0]["region"], "R-1", "{detail}");
    assert_eq!(detail["locks"][0]["holder"]["run"], "run-a");
    assert!(detail["locks"][0]["since"].is_string() && detail["locks"][0]["until"].is_string());
    assert!((1..=120).contains(&detail["retry_after_secs"].as_i64().unwrap()), "{detail}");
    assert_eq!(s.node_count("kb"), before + 1, "nothing of the refused write landed");
    // A link from R-2 into R-1 names R-1.
    let (code_, _, _) = refused(&svc.call("soup_link", json!({"cauldron": "kb", "links": [{"from": "T-1", "kind": "ref", "to": "M-1"}]}), &c));
    assert_eq!(code_, "region_locked");
    // Status and the listing show it.
    let st = ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &c));
    assert_eq!(st["region_locks"][0]["holder"]["run"], "run-a", "{st}");
    assert_eq!(st["region_locks"][0]["regions"], json!(["R-1"]));
    let regions = ok(&svc.call("soup_regions", json!({"cauldron": "kb"}), &c));
    let r1 = regions["regions"].as_array().unwrap().iter().find(|r| r["id"] == "R-1").unwrap().clone();
    assert_eq!(r1["locked"]["holder"]["run"], "run-a", "{regions}");
    assert!(regions["regions"].as_array().unwrap().iter().find(|r| r["id"] == "R-2").unwrap().get("locked").is_none());

    // A second ripple: refused R-1, granted R-2.
    let b = lock(&svc, &s, "run-b", "DM-2", &["R-1"], 120);
    assert_eq!(code(&b), "region_locked");
    assert!(b["message"].as_str().unwrap().contains("run run-a"), "{b}");
    let b = lock(&svc, &s, "run-b", "DM-2", &["R-2"], 120);
    assert_eq!(b["outcome"], "locked", "{b}");
    // Both hold at once, and both land.
    let pa = say(&svc, &s, "RPUT", put("run-a", "DM-1", json!([idea("n1", "from the schema note", "R-1")]), Some(&a["revs"])));
    assert_eq!(pa["outcome"], "landed", "{pa}");
    assert_eq!(pa["locks_released"], 1);
    let pb = say(&svc, &s, "RPUT", put("run-b", "DM-2", json!([idea("n1", "from the import note", "R-2")]), Some(&b["revs"])));
    assert_eq!(pb["outcome"], "landed", "{pb}");
    // Both let go: the session lands in R-1 now.
    assert!(ok(&svc.call("soup_status", json!({"cauldron": "kb"}), &c))["region_locks"].as_array().unwrap().is_empty());
    ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [idea("y", "a schema idea", "R-1")]}), &c));
    // The audit names the refusals.
    assert!(s.calls().iter().any(|l| l["tool"] == "soup_put" && l["outcome"] == "region_locked"), "{:?}", s.calls());
}

/// The revision check, on the head revision (a region's boundary and
/// identity): a member a curator writes into R-1 after the read does NOT
/// move it, so the put lands; a region narrowing R-1 does, and the put is
/// refused `region_changed` (which region, read and now), nothing written;
/// read again, it lands. A put without `revs` (an older ripples service) is
/// not checked. A malformed `revs` is usage.
#[test]
fn rl2_a_region_changed_after_the_run_read_it_refuses_the_put_until_it_reads_again() {
    let (s, svc, c) = setup("rl2");
    let read = say(&svc, &s, "RREAD", json!({"cauldron": "kb"}));
    let revs = read["revs"].clone();
    assert_eq!(code(&say(&svc, &s, "RPUT", put("run-z", "DM-1", json!([idea("n1", "x", "R-1")]), Some(&json!(["R-1"]))))), "usage");
    assert!(revs["R-1"].is_string() && revs["R-2"].is_string() && revs.get("~unplaced").is_none(), "{read}");
    // A curator writes a member into R-1 after the read: not staleness.
    ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [idea("x", "the curator's idea", "R-1")]}), &c));
    assert_eq!(say(&svc, &s, "RREAD", json!({"cauldron": "kb"}))["revs"], revs, "a member moves no head revision");
    let v = say(&svc, &s, "RPUT", put("run-m", "DM-1", json!([idea("n1", "beside a new member", "R-1")]), Some(&revs)));
    assert_eq!(v["outcome"], "landed", "{v}");
    // A region narrowing R-2 after the read: R-2's boundary context moved.
    let child = json!({"id": "c", "type": "region", "handle": "Importing in bulk", "boundary": "Inside: anything about importing many records at once, in bulk.", "edges": [{"kind": "narrows", "to": "R-2"}]});
    ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [child]}), &c));
    let before = s.node_count("kb");
    let v = say(&svc, &s, "RPUT", put("run-r", "DM-2", json!([idea("n1", "stale", "R-2")]), Some(&revs)));
    assert_eq!(code(&v), "region_changed", "{v}");
    assert_eq!(v["detail"]["changed"][0]["region"], "R-2");
    assert_ne!(v["detail"]["changed"][0]["read"], v["detail"]["changed"][0]["now"]);
    assert_eq!(s.node_count("kb"), before, "nothing written");
    // Read again: lands.
    let fresh = say(&svc, &s, "RREAD", json!({"cauldron": "kb"}))["revs"].clone();
    assert_ne!(fresh["R-2"], revs["R-2"]);
    let v = say(&svc, &s, "RPUT", put("run-r", "DM-2", json!([idea("n1", "fresh", "R-2")]), Some(&fresh)));
    assert_eq!(v["outcome"], "landed", "{v}");
}

/// **The put takes the regions it writes into**, declared or not: a run
/// that holds nothing puts into R-1 while another run holds R-1, and is
/// refused `region_locked` at the put (`detail.at`), nothing written; into
/// R-2 it lands; a put whose record goes into a region held by ANOTHER
/// holder through the child it narrows (`link`) is refused too.
#[test]
fn rl2b_a_put_into_a_region_another_holds_is_refused_even_undeclared() {
    let (s, svc, _c) = setup("rl2b");
    let revs = say(&svc, &s, "RREAD", json!({"cauldron": "kb"}))["revs"].clone();
    assert_eq!(lock(&svc, &s, "run-holder", "DM-1", &["R-1"], 60)["outcome"], "locked");
    let before = s.node_count("kb");
    let v = say(&svc, &s, "RPUT", put("run-free", "DM-2", json!([idea("n1", "into a held region", "R-1")]), Some(&revs)));
    assert_eq!(code(&v), "region_locked", "{v}");
    assert_eq!(v["detail"]["at"], "put", "{v}");
    assert!(v["message"].as_str().unwrap().contains("region `R-1` is locked"), "{v}");
    assert_eq!(s.node_count("kb"), before, "nothing written");
    let v = say(&svc, &s, "RPUT", put("run-free", "DM-2", json!([idea("n1", "into a free region", "R-2")]), Some(&revs)));
    assert_eq!(v["outcome"], "landed", "{v}");
    // Without `revs` (an older ripples service) the put is held by the
    // other holder's rows all the same.
    let v = say(&svc, &s, "RPUT", put("run-old", "DM-1", json!([idea("n1", "from an older ripples", "R-1")]), None));
    assert_eq!(code(&v), "region_locked", "{v}");
}

/// No lock outlives its expiry: after its `until` a held region takes a
/// session's write, renewing revives nothing, and a later `RLOCK` by another
/// run is granted. A renewal before expiry moves `until` forward. `RUNLOCK`
/// lets go at once. A region that is no region is refused.
#[test]
fn rl3_a_lock_expires_renewal_never_revives_it_and_runlock_lets_go() {
    let (s, svc, c) = setup("rl3");
    let a = lock(&svc, &s, "run-e", "DM-1", &["R-1"], 2);
    assert_eq!(a["outcome"], "locked", "{a}");
    let r = say(&svc, &s, "RRENEW", json!({"cauldron": "kb", "run": "run-e", "ttl_secs": 2}));
    assert_eq!(r["renewed"], 1, "{r}");
    let (code_, _, _) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [idea("x", "held", "R-1")]}), &c));
    assert_eq!(code_, "region_locked");
    std::thread::sleep(std::time::Duration::from_millis(2_300));
    ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [idea("x", "after the expiry", "R-1")]}), &c));
    let r = say(&svc, &s, "RRENEW", json!({"cauldron": "kb", "run": "run-e", "ttl_secs": 60}));
    assert_eq!(r["renewed"], 0, "an expired lock is never revived: {r}");
    assert_eq!(lock(&svc, &s, "run-f", "DM-2", &["R-1"], 60)["outcome"], "locked");
    let u = say(&svc, &s, "RUNLOCK", json!({"cauldron": "kb", "run": "run-f"}));
    assert_eq!(u["released"], 1, "{u}");
    ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [idea("y", "after the release", "R-1")]}), &c));
    assert_eq!(code(&lock(&svc, &s, "run-g", "DM-2", &["DIR-1"], 60)), "usage");
    assert_eq!(code(&lock(&svc, &s, "run-g", "DM-2", &[], 60)), "usage");
    assert_eq!(code(&say(&svc, &s, "RLOCK", json!({"cauldron": "kb", "run": "run-g", "regions": ["R-1"], "ttl_secs": 0}))), "usage");
}

/// The lock survives a restart of this service (it is a row in the store),
/// and a hierarchy holds: a ripple holding a child region refuses a write
/// into the child and a re-parenting of its parent, and takes a write into
/// the sibling and into the parent itself.
#[test]
fn rl4_a_held_child_region_refuses_its_parents_move_and_survives_a_restart() {
    let s = Scratch::new("rl4", &["kb"]);
    let mut records = example();
    records.push(json!({"id": "R-9", "type": "region", "handle": "Schema field names",
        "boundary": "Inside: the names of the schema's fields. Outside: anything else about the schema goes to R-1.",
        "edges": [{"kind": "narrows", "to": "R-1"}]}));
    records.push(json!({"id": "R-8", "type": "region", "handle": "Schema versions",
        "boundary": "Inside: how the schema's versions are numbered. Outside: anything else about the schema goes to R-1.",
        "edges": [{"kind": "narrows", "to": "R-1"}]}));
    s.import("kb", &records);
    s.grant("kb", &[Scope::Read, Scope::Write, Scope::Dump], &[]);
    let mut svc = Service::start(&s);
    let c = ctx(&s.project("p"));
    ok(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "a note"}), &c));
    let regions = ok(&svc.call("soup_regions", json!({"cauldron": "kb"}), &c));
    let id_of = |h: &str| regions["regions"].as_array().unwrap().iter().find(|r| r["handle"] == h).unwrap()["id"].as_str().unwrap().to_string();
    let (child, sibling) = (id_of("Schema field names"), id_of("Schema versions"));
    let a = lock(&svc, &s, "run-h", "DM-1", &[&child], 120);
    assert_eq!(a["lock"]["above"], json!(["R-1"]), "{a}");
    drop(svc.stop());
    svc = Service::start(&s);
    let (code_, _, _) = refused(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [idea("x", "a field name", &child)]}), &c));
    assert_eq!(code_, "region_locked", "held across the restart");
    // Re-parenting the parent (`write` on it) against the holder's `link`.
    let (code_, msg, _) = refused(&svc.call("soup_link", json!({"cauldron": "kb", "links": [{"from": "R-1", "kind": "narrows", "to": "R-2"}]}), &c));
    assert_eq!(code_, "region_locked", "the parent re-parented: {msg}");
    assert!(msg.contains("has a region below it locked"), "{msg}");
    ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [idea("x", "a version idea", &sibling)]}), &c));
    ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [idea("y", "a schema idea", "R-1")]}), &c));
}

/// Killed after the records landed and before the run's locks were let
/// go: the restarted door finds rows of a run that landed, and they hold
/// nothing; the next `RLOCK` sweeps them; the run lands once.
#[cfg(feature = "fault-injection")]
#[test]
fn rl5_killed_between_the_landing_and_the_release_no_lock_is_stuck() {
    let s = Scratch::new("rl5", &["kb"]);
    s.import("kb", &example());
    s.grant("kb", &[Scope::Read, Scope::Write, Scope::Dump], &[]);
    let mut svc = Service::start_with(&s, &[("CAULDRON_MCP_FAULT", "ripple-landed-held")]);
    let c = ctx(&s.project("p"));
    ok(&svc.call("soup_dump", json!({"cauldron": "kb", "text": "a note"}), &c));
    let a = lock(&svc, &s, "run-k", "DM-1", &["R-1"], 600);
    let before = s.node_count("kb");
    let line = format!("RPUT {} {}", key(&s), put("run-k", "DM-1", json!([idea("n1", "from the note", "R-1")]), Some(&a["revs"])));
    let mut st = std::net::TcpStream::connect(svc.seam).unwrap();
    use std::io::{BufRead, Write};
    writeln!(st, "{line}").unwrap();
    let mut out = String::new();
    let _ = std::io::BufReader::new(st).read_line(&mut out);
    assert!(out.trim().is_empty(), "the service answered: {out}");
    assert!(!svc.child.wait().unwrap().success());
    assert_eq!(s.node_count("kb"), before + 1, "the records landed");
    let table: Value = serde_json::from_str(&std::fs::read_to_string(s.soup("kb").join("region-locks.json")).unwrap()).unwrap();
    assert_eq!(table["rows"].as_array().unwrap().len(), 1, "the row is still in the table: {table}");
    let svc = Service::start(&s);
    // The row holds nothing: its run landed.
    ok(&svc.call("soup_put", json!({"cauldron": "kb", "remint": true, "records": [idea("x", "a curator's idea", "R-1")]}), &c));
    assert_eq!(lock(&svc, &s, "run-l", "DM-1", &["R-1"], 60)["outcome"], "locked");
    let table: Value = serde_json::from_str(&std::fs::read_to_string(s.soup("kb").join("region-locks.json")).unwrap()).unwrap();
    assert!(table["rows"].as_array().unwrap().iter().all(|r| r["holder"]["run"] == "run-l"), "swept: {table}");
    let v = say(&svc, &s, "RPUT", put("run-k", "DM-1", json!([]), None));
    assert_eq!(v["outcome"], "already_landed", "{v}");
    assert_eq!(s.node_count("kb"), before + 2, "the run landed once");
}
