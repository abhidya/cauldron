//! Region locks from the ripple's side: a run holds the regions it
//! declared from before its model call until its records land or it gives
//! up; a curator writes elsewhere meanwhile and is refused, by name, in a
//! held region; a member written into a region never makes the run plan
//! again, a boundary that changed under the run makes it plan again once,
//! told what changed (or, when the model saw nothing change, send the same
//! records again with no call); two runs on disjoint regions hold and land at once; no lock is
//! held after the run ends any other way, nor after a kill once the service
//! starts again.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::support::*;

/// A dump's text that declares R-1 of the worked example only ("Publishing
/// the soup schema").
const SCHEMA_TEXT: &str = "the published schema needs a drift test";
/// One that declares R-2 only ("Importing records into a soup").
const IMPORT_TEXT: &str = "landing externally written records twice";

fn answer_in(region: &str, handle: &str) -> Value {
    json!({"records": [{"label": "n1", "type": "idea", "state": "idea", "handle": handle, "regions": [region]}]})
}

fn idea(label: &str, handle: &str, region: &str) -> Value {
    json!({"id": label, "type": "idea", "state": "idea", "handle": handle, "edges": [{"kind": "in_region", "to": region}]})
}

/// A session's `soup_put` straight to the door: the payload, or the
/// refusal's `(code, message, detail)`.
fn session_put(door: &Door, region: &str, handle: &str) -> Result<Value, (String, String, Value)> {
    let args = json!({"_ctx": {"client": "test"}, "cauldron": "kb", "remint": true, "records": [idea("x", handle, region)]});
    door.door.call(cauldron_mcp::catalogue::find("soup_put").unwrap(), args).map_err(|r| (r.code.to_string(), r.message.clone(), r.detail.clone().unwrap_or(Value::Null)))
}

fn table(s: &Scratch) -> Vec<Value> {
    let t = env::region_locks::load(&s.target().cauldron).unwrap();
    let now = env::region_locks::now_ms();
    t.live(now).into_iter().map(|r| serde_json::to_value(r).unwrap()).collect()
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !f() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Opens a scripted call's gate when dropped, so a test that fails while
/// the call is held never leaves the run waiting for it.
struct OpensOnDrop(std::path::PathBuf);

impl Drop for OpensOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, b"");
    }
}

fn events_of(s: &Scratch, run: &str) -> Vec<String> {
    s.journal().iter().filter(|l| l["run"] == run).map(|l| l["event"].as_str().unwrap().to_string()).collect()
}

/// **The curator writes elsewhere while the ripple thinks.** The run holds
/// R-1 from before its call; during the call (the scripted backend holds it
/// open until the test opens its gate, so the call cannot end under the
/// test on a loaded machine) a session's write into R-2 lands, one into R-1
/// is refused `region_locked` naming the run, since and until, with a
/// retry-after, and both answer while the call is still open (a refusal
/// that waited for the lock would never answer); the run then lands in R-1
/// and lets go in the same act, and the session's write into R-1 lands.
#[test]
fn rg1_a_curator_write_elsewhere_lands_during_a_running_ripple_and_one_into_its_region_is_refused() {
    let s = Scratch::new("rg1");
    let door = Door::start(&s);
    let id = door.dump(SCHEMA_TEXT);
    let gate = s.0.join("rg1.gate");
    let script = s.script("rg1", json!([{"output": answer_in("R-1", "a drift test for the schema"), "until_exists": gate}]));
    let out = std::thread::scope(|sc| {
        // Inside the scope: a failure here opens the gate before the scope
        // waits for the run.
        let _open = OpensOnDrop(gate.clone());
        let run = sc.spawn(|| {
            let ctx = engine(&s, &door, &script, Default::default());
            cauldron_ripples::engine::run(&ctx, &ask(&id))
        });
        wait_for("the call is open", || calls(&script).len() == 1);
        let held = table(&s);
        assert_eq!(held.len(), 1, "{held:?}");
        assert_eq!((held[0]["region"].clone(), held[0]["mode"].clone(), held[0]["holder"]["dump"].clone()), (json!("R-1"), json!("write"), json!(id)));
        session_put(&door, "R-2", "an import idea").expect("another region takes the write at once");
        let (code, msg, detail) = session_put(&door, "R-1", "a schema idea").expect_err("the held region refuses");
        assert_eq!(table(&s).len(), 1, "answered while the call still holds R-1");
        std::fs::write(&gate, b"").unwrap();
        assert_eq!(code, "region_locked", "{msg}");
        assert!(msg.contains("region `R-1` is locked by a ripple") && msg.contains(&format!("dump {id}")), "{msg}");
        assert!(detail["retry_after_secs"].as_i64().unwrap() >= 1, "{detail}");
        run.join().unwrap()
    });
    assert_eq!(out["outcome"], "processed", "{out}");
    let run = out["run"].as_str().unwrap();
    let ev = events_of(&s, run);
    assert_eq!(ev, ["started", "locked", "read", "call", "scrubbed", "called", "unlocked", "done"], "{ev:?}");
    assert!(table(&s).is_empty(), "let go in the act that landed the records");
    session_put(&door, "R-1", "a schema idea").expect("R-1 takes writes again");
    assert_eq!(s.ripple_nodes().len(), 1);
}

/// A session's `soup_link` straight to the door.
fn session_link(door: &Door, from: &str, kind: &str, to: &str) -> Result<Value, (String, String, Value)> {
    let args = json!({"_ctx": {"client": "test"}, "cauldron": "kb", "links": [{"from": from, "kind": kind, "to": to}]});
    door.door.call(cauldron_mcp::catalogue::find("soup_link").unwrap(), args).map_err(|r| (r.code.to_string(), r.message.clone(), r.detail.clone().unwrap_or(Value::Null)))
}

/// A session's new region, narrowing `parent` (or none).
fn session_region(door: &Door, handle: &str, parent: Option<&str>) -> Value {
    let mut r = json!({"id": "nr", "type": "region", "handle": handle, "boundary": format!("Inside: anything about {handle}, and nothing else at all.")});
    if let Some(p) = parent {
        r["edges"] = json!([{"kind": "narrows", "to": p}]);
    }
    let args = json!({"_ctx": {"client": "test"}, "cauldron": "kb", "remint": true, "records": [r]});
    door.door.call(cauldron_mcp::catalogue::find("soup_put").unwrap(), args).expect("the region lands")
}

/// What the journal says the dump's run spent against its own cap.
fn spent(s: &Scratch, run: &str) -> (u32, u64) {
    cauldron_ripples::journal::spent(&s.journal(), run)
}

/// **Members added to a region the run never declared are not staleness.**
/// The run declares R-2; its answer places a record in R-1, which it did
/// not declare; during the call a session writes two records into R-1 (a
/// revision that counted members forced a re-plan from an identical prompt
/// here, and two such changes stranded the dump at its lifetime cap). The run lands
/// with one call, no re-plan, and holds nothing after.
#[test]
fn rg2_members_added_to_an_undeclared_region_during_the_call_do_not_stop_the_dump() {
    let s = Scratch::new("rg2");
    let door = Door::start(&s);
    let id = door.dump(IMPORT_TEXT);
    let script = s.script(
        "rg2",
        json!([
            {"output": answer_in("R-1", "outside the lock"), "sleep_ms": 800},
            {"output": answer_in("R-1", "outside the lock again"), "sleep_ms": 800},
            {"output": answer_in("R-1", "third"), "sleep_ms": 800}
        ]),
    );
    let out = std::thread::scope(|sc| {
        let run = sc.spawn(|| {
            let ctx = engine(&s, &door, &script, Default::default());
            cauldron_ripples::engine::run(&ctx, &ask(&id))
        });
        wait_for("the first call is open", || calls(&script).len() == 1);
        assert_eq!(table(&s)[0]["region"], "R-2", "the declared region is held");
        session_put(&door, "R-1", "change one").expect("R-1 is not held");
        session_put(&door, "R-1", "change two").expect("R-1 is not held");
        run.join().unwrap()
    });
    assert_eq!(out["outcome"], "processed", "{out}");
    assert_eq!(out["calls"], 1, "{out}");
    let ev = events_of(&s, out["run"].as_str().unwrap());
    assert!(!ev.contains(&"replan".to_string()), "{ev:?}");
    assert_eq!(calls(&script).len(), 1);
    assert!(table(&s).is_empty());
    assert_eq!(s.dump_state(&id)["state"], "processed");
}

/// **A boundary changed under the run: planned again once, told what
/// changed.** During the call a session adds a region narrowing R-1, the
/// region the answer places a record in: R-1's head revision moves, the put
/// is refused `region_changed`, and the second prompt differs from the first
/// (it lists the new region and says what changed). The voided first call
/// is not counted against the dump's own cap.
#[test]
fn rg2b_a_boundary_changed_under_the_run_replans_with_what_changed() {
    let s = Scratch::new("rg2b");
    let door = Door::start(&s);
    let id = door.dump(IMPORT_TEXT);
    let script = s.script(
        "rg2b",
        json!([
            {"output": answer_in("R-1", "placed before the split"), "sleep_ms": 800},
            {"output": answer_in("R-1", "placed after a fresh read"), "sleep_ms": 10}
        ]),
    );
    let out = std::thread::scope(|sc| {
        let run = sc.spawn(|| {
            let ctx = engine(&s, &door, &script, Default::default());
            cauldron_ripples::engine::run(&ctx, &ask(&id))
        });
        wait_for("the first call is open", || calls(&script).len() == 1);
        session_region(&door, "schema drift checks", Some("R-1"));
        run.join().unwrap()
    });
    assert_eq!(out["outcome"], "processed", "{out}");
    assert_eq!(out["calls"], 2, "{out}");
    let c = calls(&script);
    assert_ne!(c[0]["prompt"], c[1]["prompt"], "the re-plan is given what changed");
    let second = c[1]["prompt"].as_str().unwrap();
    assert!(second.contains("## The regions changed while you answered") && second.contains("- R-1: its place among the regions changed"), "{second}");
    assert!(second.contains("schema drift checks"), "the new region is listed");
    let run = out["run"].as_str().unwrap();
    assert!(events_of(&s, run).contains(&"voided".to_string()));
    assert_eq!(spent(&s, run).0, 1, "the voided call is not the dump's");
    assert!(table(&s).is_empty());
}

/// **A re-plan is never burned on a no-op.** During the call a session
/// re-parents R-1 under a third region: R-1's head revision moves, but
/// nothing the model was shown changed (the prompt shows each region's id,
/// handle and boundary). The run reads again, sends the same records with
/// the fresh revisions and makes no second call.
#[test]
fn rg2c_a_change_the_model_never_saw_resends_the_records_without_a_call() {
    let s = Scratch::new("rg2c");
    let door = Door::start(&s);
    let third = session_region(&door, "release engineering", None)["id_map"]["nr"].as_str().unwrap().to_string();
    let id = door.dump(IMPORT_TEXT);
    let script = s.script("rg2c", json!([{"output": answer_in("R-1", "placed once"), "sleep_ms": 800}, {"output": answer_in("R-1", "never asked")}]));
    let out = std::thread::scope(|sc| {
        let run = sc.spawn(|| {
            let ctx = engine(&s, &door, &script, Default::default());
            cauldron_ripples::engine::run(&ctx, &ask(&id))
        });
        wait_for("the first call is open", || calls(&script).len() == 1);
        session_link(&door, "R-1", "narrows", &third).expect("neither region is held");
        run.join().unwrap()
    });
    assert_eq!(out["outcome"], "processed", "{out}");
    assert_eq!(out["calls"], 1, "{out}");
    assert_eq!(calls(&script).len(), 1, "no second call");
    let run = out["run"].as_str().unwrap();
    let replan: Vec<Value> = s.journal().into_iter().filter(|l| l["run"] == run && l["event"] == "replan").collect();
    assert_eq!(replan.len(), 1);
    assert_eq!((replan[0]["prompt_changed"].clone(), replan[0]["call"].clone()), (json!(false), json!(false)), "{replan:?}");
}

/// **Two boundary changes: the dump waits, and is not stranded.** A change
/// during each of the two calls defers the dump (`region_changed`) with
/// nothing held and nothing landed; both calls are voided, so with a cap of
/// ONE call per dump the next run still gets its call and lands.
#[test]
fn rg2d_two_boundary_changes_defer_the_dump_without_spending_its_cap() {
    let s = Scratch::new("rg2d");
    let door = Door::start(&s);
    let id = door.dump(IMPORT_TEXT);
    let script = s.script(
        "rg2d",
        json!([
            {"output": answer_in("R-1", "outside the lock"), "sleep_ms": 800},
            {"output": answer_in("R-1", "outside the lock again"), "sleep_ms": 800},
            {"output": answer_in("R-1", "at last")}
        ]),
    );
    let caps = cauldron_ripples::engine::Caps { calls: 2, ..Default::default() };
    let out = std::thread::scope(|sc| {
        let run = sc.spawn(|| {
            let ctx = engine(&s, &door, &script, caps);
            cauldron_ripples::engine::run(&ctx, &ask(&id))
        });
        wait_for("the first call is open", || calls(&script).len() == 1);
        session_region(&door, "schema drift checks", Some("R-1"));
        wait_for("the second call is open", || calls(&script).len() == 2);
        session_region(&door, "schema version numbers", Some("R-1"));
        run.join().unwrap()
    });
    assert_eq!((out["outcome"].clone(), out["reason"].clone()), (json!("deferred"), json!("region_changed")), "{out}");
    assert_eq!(s.dump_state(&id)["state"], "received");
    assert!(table(&s).is_empty(), "nothing held after the run gave up");
    assert_eq!(s.ripple_nodes().len(), 0);
    let run = out["run"].as_str().unwrap().to_string();
    assert_eq!(spent(&s, &run).0, 0, "both calls voided: {:?}", s.journal());
    // The next attempt, with nothing changing, under a cap of one call.
    let caps = cauldron_ripples::engine::Caps { calls: 1, ..Default::default() };
    let out = cauldron_ripples::engine::run(&engine(&s, &door, &script, caps), &ask(&id));
    assert_eq!(out["outcome"], "processed", "{out}");
    assert_eq!(s.ripple_nodes().len(), 1);
    // The daily cap still counts every call.
    let today = cauldron_ripples::journal::day(&s.journal(), "kb", None, &cauldron_ripples::journal::today());
    assert_eq!(today.calls, 3);
}

/// **Member churn across scans**: two member changes into the region the
/// answer goes into, then the next scan's run of the same dump. With a
/// revision that counted members the dump ended `deferred budget` with
/// every call spent; now it lands at the first run, with one call.
#[test]
fn rg2e_member_churn_across_scans_lands_the_dump() {
    let s = Scratch::new("rg2e");
    let door = Door::start(&s);
    let id = door.dump(IMPORT_TEXT);
    let script = s.script(
        "rg2e",
        json!([
            {"output": answer_in("R-1", "outside the lock"), "sleep_ms": 800},
            {"output": answer_in("R-1", "outside the lock again"), "sleep_ms": 800},
            {"output": answer_in("R-1", "third"), "sleep_ms": 800},
            {"output": answer_in("R-1", "fourth"), "sleep_ms": 800}
        ]),
    );
    let out = std::thread::scope(|sc| {
        let run = sc.spawn(|| cauldron_ripples::engine::run(&engine(&s, &door, &script, Default::default()), &ask(&id)));
        wait_for("first call", || calls(&script).len() == 1);
        session_put(&door, "R-1", "change one").unwrap();
        session_put(&door, "R-1", "change two").unwrap();
        run.join().unwrap()
    });
    eprintln!("churn round1 outcome={} reason={} calls={}", out["outcome"], out["reason"], out["calls"]);
    assert_eq!((out["outcome"].clone(), out["calls"].clone()), (json!("processed"), json!(1)), "{out}");
    let again = cauldron_ripples::engine::run(&engine(&s, &door, &script, Default::default()), &ask(&id));
    eprintln!("churn round2 outcome={} calls={}", again["outcome"], again["calls"]);
    assert_eq!(again["outcome"], "already_processed", "{again}");
    eprintln!("churn total backend calls={}", calls(&script).len());
    assert_eq!(calls(&script).len(), 1);
}

/// **Two runs on disjoint regions hold at once and both land**; a third
/// declaring a region the first holds is deferred `region_locked` with the
/// holder's retry-after and holds nothing. (The service runs one dump at a
/// time; two runs here are two engines over one door, which is what the
/// locks have to keep apart.)
#[test]
fn rg3_two_runs_on_disjoint_regions_hold_at_once_and_both_land() {
    let s = Scratch::new("rg3");
    let door = Door::start(&s);
    let (a, b, c) = (door.dump(SCHEMA_TEXT), door.dump(IMPORT_TEXT), door.dump(SCHEMA_TEXT));
    let sa = s.script("rg3a", json!([{"output": answer_in("R-1", "from the schema note"), "sleep_ms": 1200}]));
    let sb = s.script("rg3b", json!([{"output": answer_in("R-2", "from the import note"), "sleep_ms": 1200}]));
    let sc_ = s.script("rg3c", json!([{"output": answer_in("R-1", "never asked")}]));
    let (oa, ob, oc) = std::thread::scope(|sc| {
        let ra = sc.spawn(|| cauldron_ripples::engine::run(&engine(&s, &door, &sa, Default::default()), &ask(&a)));
        let rb = sc.spawn(|| cauldron_ripples::engine::run(&engine(&s, &door, &sb, Default::default()), &ask(&b)));
        wait_for("both calls are open", || calls(&sa).len() == 1 && calls(&sb).len() == 1);
        let held: Vec<Value> = table(&s).iter().map(|r| r["region"].clone()).collect();
        assert_eq!(held.len(), 2, "both hold at once: {held:?}");
        let oc = cauldron_ripples::engine::run(&engine(&s, &door, &sc_, Default::default()), &ask(&c));
        (ra.join().unwrap(), rb.join().unwrap(), oc)
    });
    assert_eq!(oa["outcome"], "processed", "{oa}");
    assert_eq!(ob["outcome"], "processed", "{ob}");
    assert_eq!((oc["outcome"].clone(), oc["reason"].clone()), (json!("deferred"), json!("region_locked")), "{oc}");
    assert!(oc["retry_after_secs"].as_i64().unwrap() >= 1, "{oc}");
    assert!(calls(&sc_).is_empty(), "no call was made for the refused run");
    assert_eq!(s.dump_state(&c)["state"], "received");
    assert!(table(&s).is_empty());
    assert_eq!(s.ripple_nodes().len(), 2);
}

/// **No lock is held after a park, a budget deferral or a failure.** Each
/// run ends with its lock let go (`unlocked`, before `done`), and a run at
/// its cap never takes one.
#[test]
fn rg4_a_park_a_cap_or_a_failure_lets_the_lock_go() {
    let s = Scratch::new("rg4");
    let door = Door::start(&s);
    for (n, entry) in [json!({"fail": "parked", "cause": "threshold"}), json!({"fail": "failed", "message": "scripted"})].into_iter().enumerate() {
        let id = door.dump(SCHEMA_TEXT);
        let script = s.script(&format!("rg4-{n}"), json!([entry]));
        let out = cauldron_ripples::engine::run(&engine(&s, &door, &script, Default::default()), &ask(&id));
        assert_ne!(out["outcome"], "processed", "{out}");
        let ev = events_of(&s, out["run"].as_str().unwrap());
        assert!(ev.contains(&"locked".to_string()) && ev.ends_with(&["unlocked".to_string(), "done".to_string()]), "{ev:?}");
        assert!(table(&s).is_empty(), "{out}");
    }
    // At the dump's cap of calls: deferred before any lock.
    let id = door.dump(SCHEMA_TEXT);
    let script = s.script("rg4-cap", json!([{"output": answer_in("R-1", "never")}]));
    let caps = cauldron_ripples::engine::Caps { calls: 0, ..Default::default() };
    let out = cauldron_ripples::engine::run(&engine(&s, &door, &script, caps), &ask(&id));
    assert_eq!((out["outcome"].clone(), out["reason"].clone()), (json!("deferred"), json!("budget")), "{out}");
    let ev = events_of(&s, out["run"].as_str().unwrap());
    assert!(!ev.contains(&"locked".to_string()), "{ev:?}");
    assert!(table(&s).is_empty());
    assert!(calls(&script).is_empty());
}

/// **Renewed while the call is open.** With a time to live of 1 s, the call
/// is held open by the test's gate (never a guessed duration). Once the
/// lock has been renewed (its `until` moved past where it started) and more
/// than its time to live has passed since the call opened, it still refuses
/// a session's write; then the gate opens, and once the run lands the lock
/// is let go.
#[test]
fn rg5_the_lock_is_renewed_while_the_call_is_open() {
    let s = Scratch::new("rg5");
    let door = Door::start(&s);
    let id = door.dump(SCHEMA_TEXT);
    let gate = s.0.join("rg5.gate");
    let script = s.script("rg5", json!([{"output": answer_in("R-1", "after a long call"), "until_exists": gate}]));
    let out = std::thread::scope(|sc| {
        // Inside the scope: a failure here opens the gate before the scope
        // waits for the run.
        let _open = OpensOnDrop(gate.clone());
        let run = sc.spawn(|| {
            let mut ctx = engine(&s, &door, &script, Default::default());
            ctx.lock_ttl_secs = 1;
            cauldron_ripples::engine::run(&ctx, &ask(&id))
        });
        wait_for("the call is open", || calls(&script).len() == 1);
        let opened = Instant::now();
        let first = table(&s)[0]["until_ms"].clone();
        wait_for("renewed, and past its first time to live", || {
            opened.elapsed() > Duration::from_millis(1800) && table(&s).first().is_some_and(|r| r["until_ms"] != first)
        });
        let (code, _, _) = session_put(&door, "R-1", "while the call is open").expect_err("still held");
        assert_eq!(code, "region_locked");
        std::fs::write(&gate, b"").unwrap();
        run.join().unwrap()
    });
    assert_eq!(out["outcome"], "processed", "{out}");
    let ev = events_of(&s, out["run"].as_str().unwrap());
    assert!(!ev.contains(&"lock_lost".to_string()), "{ev:?}");
    assert!(table(&s).is_empty());
}

/// **Killed while it holds its lock, at each point, then started again.**
/// Killed during the model call, and killed right after it took its lock:
/// the row stays in the soup (it is the store's) until the service starts
/// again, whose first act lets it go; the dump then lands exactly once.
/// Killed after the put: the lock went with the landing.
#[test]
fn rg6_killed_while_holding_its_lock_the_restart_lets_it_go_and_the_dump_lands_once() {
    let s = Scratch::new("rg6");
    let door = Door::start(&s);
    for (n, fault) in [("call", None), ("after-lock", Some("after-lock")), ("after-put", Some("after-put"))] {
        let id = door.dump(SCHEMA_TEXT);
        let slow = s.script(&format!("rg6-{n}"), json!([{"output": answer_in("R-1", "from a killed run"), "sleep_ms": if fault.is_none() { 60000 } else { 10 }}]));
        let mut vars = vec![("CAULDRON_RIPPLES_INFERENCE", format!("scripted:{}", slow.display()))];
        if let Some(f) = fault {
            vars.push(("CAULDRON_RIPPLES_FAULT", f.to_string()));
        }
        let mut svc = Service::start(&s, &door, &vars);
        let k = Service::key(&s);
        let a = svc.say(&format!("RUN {k} {}", json!({"cauldron": "kb", "dump": id})));
        assert_eq!(a["ok"], json!(true), "{n}: {a}");
        if fault.is_none() {
            wait_for("the call is open", || calls(&slow).len() == 1);
            svc.child.kill().unwrap();
        }
        svc.child.wait().unwrap();
        if n == "after-put" {
            assert!(table(&s).is_empty(), "{n}: the landing let it go");
        } else {
            assert_eq!(table(&s).len(), 1, "{n}: the row outlives the process");
            let (code, _, _) = session_put(&door, "R-1", &format!("while the dead run {n} holds it")).expect_err("held until let go");
            assert_eq!(code, "region_locked");
        }
        let before = s.ripple_nodes().len();
        let fast = s.script(&format!("rg6-{n}-again"), json!([{"output": answer_in("R-1", "from the run again")}]));
        let svc = Service::start(&s, &door, &[("CAULDRON_RIPPLES_INFERENCE", format!("scripted:{}", fast.display()))]);
        wait_for("the start let the lock go", || table(&s).is_empty());
        let out = svc.run(&s, &id);
        if n == "after-put" {
            assert_eq!(out["outcome"], "already_processed", "{n}: {out}");
            assert_eq!(s.ripple_nodes().len(), before, "{n}: nothing landed twice");
        } else {
            assert_eq!(out["outcome"], "processed", "{n}: {out}");
            assert_eq!(s.ripple_nodes().len(), before + 1, "{n}: one record set");
        }
        assert!(table(&s).is_empty(), "{n}");
        session_put(&door, "R-1", &format!("after the run {n}")).expect("R-1 takes writes");
        drop(svc);
    }
}

/// Hold `region` for another run, straight in the soup's table (as an
/// `RLOCK` of another engine would).
fn hold_for(s: &Scratch, run: &str, region: &str, ttl_ms: i64) {
    let c = s.target().cauldron;
    let mut t = env::region_locks::load(&c).unwrap();
    let h = env::region_locks::Holder { kind: "ripple".into(), run: run.into(), dump: Some("DM-99".into()) };
    t.acquire(&h, "placing the records drawn from dump DM-99", "ws", &[region.to_string()], &[], env::region_locks::now_ms(), ttl_ms).unwrap();
    env::region_locks::save(&c, &t).unwrap();
}

/// A dump's text that points at no region of the worked example.
const PLAIN_TEXT: &str = "lunch on friday, and a call with the landlord";

/// **A run that declares nothing holds nothing during its call, and takes
/// its regions at the put.** The text points at no region, so no lock is
/// taken before the call and a session writes into R-1 meanwhile; the
/// answer goes into R-1, which the put takes and writes in one act, and
/// nothing is held after. Then the same with R-1 held by another run at the
/// put: refused `region_locked`, nothing written, the call voided (not the
/// dump's), the dump waits.
#[test]
fn rg7_a_run_that_declares_nothing_locks_at_the_put_and_never_lands_unlocked() {
    let s = Scratch::new("rg7");
    let door = Door::start(&s);
    let id = door.dump(PLAIN_TEXT);
    let script = s.script("rg7", json!([{"output": answer_in("R-1", "placed at the put"), "sleep_ms": 600}]));
    let out = std::thread::scope(|sc| {
        let run = sc.spawn(|| cauldron_ripples::engine::run(&engine(&s, &door, &script, Default::default()), &ask(&id)));
        wait_for("the call is open", || calls(&script).len() == 1);
        assert!(table(&s).is_empty(), "nothing is held during the call");
        session_put(&door, "R-1", "the curator writes meanwhile").expect("R-1 is not held");
        run.join().unwrap()
    });
    assert_eq!(out["outcome"], "processed", "{out}");
    let run = out["run"].as_str().unwrap();
    assert!(!events_of(&s, run).contains(&"locked".to_string()));
    let read = s.journal().into_iter().find(|l| l["run"] == run && l["event"] == "read" && l.get("declared").is_some()).unwrap();
    assert_eq!(read["declared"], json!([]), "{read}");
    assert!(table(&s).is_empty());

    // R-1 held by another run when the put comes.
    let id2 = door.dump(PLAIN_TEXT);
    let script2 = s.script("rg7b", json!([{"output": answer_in("R-1", "refused at the put")}]));
    hold_for(&s, "another-run", "R-1", 60_000);
    let before = s.ripple_nodes().len();
    let out = cauldron_ripples::engine::run(&engine(&s, &door, &script2, Default::default()), &ask(&id2));
    assert_eq!((out["outcome"].clone(), out["reason"].clone()), (json!("deferred"), json!("region_locked")), "{out}");
    assert_eq!(s.ripple_nodes().len(), before, "nothing landed unlocked");
    let run2 = out["run"].as_str().unwrap();
    assert!(events_of(&s, run2).contains(&"voided".to_string()));
    assert_eq!(cauldron_ripples::journal::spent(&s.journal(), run2).0, 0, "the call is not the dump's");
    assert_eq!(s.dump_state(&id2)["state"], "received");
}

/// **A declared run whose answer goes into a region it did not declare**,
/// held by another run: refused at the put, nothing written, deferred.
#[test]
fn rg8_an_undeclared_target_held_by_another_is_refused_at_the_put() {
    let s = Scratch::new("rg8");
    let door = Door::start(&s);
    let id = door.dump(IMPORT_TEXT);
    let script = s.script("rg8", json!([{"output": answer_in("R-1", "into a region it did not declare"), "sleep_ms": 400}]));
    let out = std::thread::scope(|sc| {
        let run = sc.spawn(|| cauldron_ripples::engine::run(&engine(&s, &door, &script, Default::default()), &ask(&id)));
        wait_for("the call is open", || calls(&script).len() == 1);
        assert_eq!(table(&s).iter().map(|r| r["region"].clone()).collect::<Vec<_>>(), vec![json!("R-2")]);
        hold_for(&s, "another-run", "R-1", 60_000);
        run.join().unwrap()
    });
    assert_eq!((out["outcome"].clone(), out["reason"].clone()), (json!("deferred"), json!("region_locked")), "{out}");
    assert_eq!(s.ripple_nodes().len(), 0);
    let live: Vec<Value> = table(&s);
    assert_eq!(live.len(), 1, "only the other run's row: {live:?}");
    assert_eq!(live[0]["holder"]["run"], "another-run");
}

/// **Hostile dump text cannot keep regions held.** A dump made of every
/// word of every region declares at most half of them (one of the worked
/// example's two); with a hold budget of one second per region per hour,
/// the first such run holds its region and lands, and the next ones are
/// deferred `hold_budget` before any lock or call, naming when to retry.
#[test]
fn rg9_hostile_text_declares_at_most_half_and_the_hold_budget_defers_the_rest() {
    let s = Scratch::new("rg9");
    let door = Door::start(&s);
    let regions: Vec<Value> = example().into_iter().filter(|r| r["type"] == "region").collect();
    let hostile: String = regions.iter().map(|r| format!("{} {}", r["handle"].as_str().unwrap(), r["boundary"].as_str().unwrap())).collect::<Vec<_>>().join(" ").repeat(10);
    assert!(cauldron_ripples::regions::declare(&hostile, &regions).len() <= 1);
    let script = s.script("rg9", json!([{"output": answer_in("R-1", "from hostile text"), "sleep_ms": 1200}, {"output": answer_in("R-1", "never")}, {"output": answer_in("R-1", "never")}]));
    let mut ctx = engine(&s, &door, &script, Default::default());
    ctx.hold_budget_secs = 1;
    let first = cauldron_ripples::engine::run(&ctx, &ask(&door.dump(&hostile)));
    assert_eq!(first["outcome"], "processed", "{first}");
    for _ in 0..2 {
        let id = door.dump(&hostile);
        let out = cauldron_ripples::engine::run(&ctx, &ask(&id));
        assert_eq!((out["outcome"].clone(), out["reason"].clone()), (json!("deferred"), json!("hold_budget")), "{out}");
        assert!(out["retry_after_secs"].as_u64().unwrap() >= 60, "{out}");
        assert!(!events_of(&s, out["run"].as_str().unwrap()).contains(&"locked".to_string()));
        assert!(table(&s).is_empty());
    }
    assert_eq!(calls(&script).len(), 1, "no call for a deferred run");
    // Text that declares nothing is not held to the budget: it holds nothing.
    let plain = cauldron_ripples::engine::run(&ctx, &ask(&door.dump(PLAIN_TEXT)));
    assert_eq!(plain["outcome"], "processed", "{plain}");
}

/// **An `mcp` service from before region locks**: one that answers the
/// regions without their revisions (here: the real door, with `revs`
/// taken out of every `RREAD` answer). The run fails `mcp_too_old` before
/// any call, with the upgrade named, and nothing is put.
#[test]
fn rg10_an_mcp_service_from_before_region_locks_is_never_written_through() {
    use std::io::{BufRead, BufReader, Write};
    let s = Scratch::new("rg10");
    let door = Door::start(&s);
    let id = door.dump(SCHEMA_TEXT);
    let real = door.seam;
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let old = l.local_addr().unwrap();
    env::egress::grant_addr(old).unwrap();
    let heard: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let h = heard.clone();
    std::thread::spawn(move || {
        for conn in l.incoming().flatten() {
            let h = h.clone();
            std::thread::spawn(move || {
                let mut out = conn.try_clone().unwrap();
                let mut up = std::net::TcpStream::connect(real).unwrap();
                let mut back = BufReader::new(up.try_clone().unwrap());
                for line in BufReader::new(conn).lines() {
                    let Ok(line) = line else { return };
                    let verb = line.split(' ').next().unwrap_or("").to_string();
                    h.lock().unwrap().push(verb.clone());
                    writeln!(up, "{line}").unwrap();
                    let mut a = String::new();
                    back.read_line(&mut a).unwrap();
                    if verb == "RREAD" {
                        let mut v: Value = serde_json::from_str(&a).unwrap();
                        v.as_object_mut().unwrap().remove("revs");
                        a = format!("{v}\n");
                    }
                    if out.write_all(a.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    let script = s.script("rg10", json!([{"output": answer_in("R-1", "never asked")}]));
    let mut ctx = engine(&s, &door, &script, Default::default());
    ctx.mcp.seam = Some(old.to_string());
    let out = cauldron_ripples::engine::run(&ctx, &ask(&id));
    assert_eq!((out["outcome"].clone(), out["code"].clone()), (json!("failed"), json!("mcp_too_old")), "{out}");
    assert!(out["what_to_do"].as_str().unwrap().contains("upgrade the mcp service"), "{out}");
    assert!(calls(&script).is_empty(), "no call");
    let verbs = heard.lock().unwrap().clone();
    assert!(!verbs.iter().any(|v| v == "RPUT" || v == "RLOCK"), "{verbs:?}");
    assert_eq!(s.dump_state(&id)["state"], "received");
}
