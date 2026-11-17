//! The service binary: its seam (the run key), a run from `RUN` to its
//! outcome, `STATUS`, and runs killed at two points then run again: each
//! dump ends with exactly one record set.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::support::*;

#[test]
fn s1_a_run_through_the_service_and_its_status() {
    let s = Scratch::new("s1");
    let door = Door::start(&s);
    let id = door.dump("the cache key should hash the whole tree");
    let script = s.script("s1", json!([{"output": ideas(3), "usage": usage(12000, 900)}]));
    let svc = Service::start(&s, &door, &[("CAULDRON_RIPPLES_INFERENCE", format!("scripted:{}", script.display()))]);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(s.ripples_dir().join("run.secret")).unwrap().permissions().mode() & 0o777, 0o600);
    // Without the key, nothing runs.
    for line in [format!("RUN {}", json!({"cauldron": "kb", "dump": id})), format!("RUN nope {}", json!({"cauldron": "kb", "dump": id})), "STATUS".to_string()] {
        let v = svc.say(&line);
        assert_eq!(v["code"], "unauthorized", "{line}: {v}");
    }
    assert!(calls(&script).is_empty());
    let out = svc.run(&s, &id);
    assert_eq!(out["outcome"], "processed", "{out}\n{}", svc.log());
    assert_eq!(out["records"].as_array().unwrap().len(), 3);
    let k = Service::key(&s);
    let st = svc.say(&format!("STATUS {k} {}", json!({"cauldron": "kb"})));
    assert_eq!(st["service"]["backend"], "scripted");
    assert_eq!(st["runs"].as_array().unwrap().len(), 1, "{st}");
    let r = &st["runs"][0];
    assert_eq!((r["state"].clone(), r["outcome"].clone(), r["calls"].clone(), r["usage"]["input_tokens"].clone()), (json!("done"), json!("processed"), json!(1), json!(12000)));
    assert_eq!((st["today"]["calls"].clone(), st["today"]["usage"]["output_tokens"].clone()), (json!(1), json!(900)));
    let caps = &st["caps"];
    assert_eq!(
        (caps["calls_per_dump"].clone(), caps["input_tokens_per_dump"].clone(), caps["input_tokens_per_cauldron_per_day"].clone(), caps["dumps_per_cauldron_per_day"].clone()),
        (json!(3), json!(200000), json!(1500000), json!(30))
    );
    // Spend today, per cauldron, and per dump across its runs.
    assert_eq!(st["today"]["per_cauldron"]["kb"]["calls"], 1, "{st}");
    assert_eq!(st["today"]["per_cauldron"]["kb"]["dumps"], 1, "{st}");
    assert!(st["today"]["per_cauldron"]["kb"]["charged_input_tokens"].as_u64().unwrap() >= 12010, "{st}");
    assert_eq!((st["per_dump"][0]["dump"].clone(), st["per_dump"][0]["calls"].clone()), (json!(id), json!(1)), "{st}");
    assert_eq!(st["possibly_running"], json!([]));
    // A force budget that is not a positive number is refused.
    let bad = svc.say(&format!("RUN {k} {}", json!({"cauldron": "kb", "dump": id, "force_budget": 0})));
    assert_eq!(bad["code"], "usage", "{bad}");
    // Again: answered at once, nothing more.
    let again = svc.run(&s, &id);
    assert_eq!(again["outcome"], "already_processed");
    assert_eq!(s.ripple_nodes().len(), 3);
    assert_eq!(calls(&script).len(), 1);
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !f() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Killed during the model call: the journal shows the run interrupted, and
/// the call it left in flight is cancelled when the service starts again;
/// the next run calls again and lands one record set.
#[test]
fn s2_killed_during_the_model_call() {
    let s = Scratch::new("s2");
    let door = Door::start(&s);
    let id = door.dump("a note to process");
    let slow = s.script("s2a", json!([{"output": ideas(2), "sleep_ms": 60000, "sent": true}]));
    let mut svc = Service::start(&s, &door, &[("CAULDRON_RIPPLES_INFERENCE", format!("scripted:{}", slow.display()))]);
    let k = Service::key(&s);
    let a = svc.say(&format!("RUN {k} {}", json!({"cauldron": "kb", "dump": id})));
    let run = a["run"].as_str().unwrap().to_string();
    // A second run meanwhile is refused.
    assert_eq!(svc.say(&format!("RUN {k} {}", json!({"cauldron": "kb", "dump": id})))["code"], "busy");
    // Killed once the backend has reported the call in flight (`sent` is
    // journaled), not when it was merely asked: the scripted backend logs
    // the call before it reports `sent`.
    wait_for("the call is in flight", || s.journal().iter().any(|l| l["run"] == run.as_str() && l["event"] == "sent"));
    svc.child.kill().unwrap();
    svc.child.wait().unwrap();
    assert!(s.ripple_nodes().is_empty());
    assert_eq!(s.dump_state(&id)["state"], "received");
    let fast = s.script("s2b", json!([{"output": ideas(2)}]));
    let svc = Service::start(&s, &door, &[("CAULDRON_RIPPLES_INFERENCE", format!("scripted:{}", fast.display()))]);
    let k = Service::key(&s);
    let st = svc.say(&format!("STATUS {k} {}", json!({"run": run})));
    assert_eq!(st["state"], "interrupted", "{st}");
    let lines: Vec<Value> = s.journal().into_iter().filter(|l| l["run"] == run.as_str()).collect();
    let events: Vec<&str> = lines.iter().map(|l| l["event"].as_str().unwrap()).collect();
    assert_eq!(events, ["started", "read", "call", "sent", "cancel"], "the start settled the call the kill left");
    assert_eq!((lines[4]["settled"].clone(), lines[4]["confirmed"].clone()), (json!(true), json!(true)));
    let st = svc.say(&format!("STATUS {k} {}", json!({})));
    assert_eq!(st["possibly_running"], json!([]));
    // The call the kill cut short is charged, and shown as never settled.
    assert_eq!((st["per_dump"][0]["calls"].clone(), st["per_dump"][0]["unsettled_calls"].clone()), (json!(1), json!(1)), "{st}");
    let out = svc.run(&s, &id);
    assert_eq!((out["outcome"].clone(), out["run"].clone()), (json!("processed"), json!(run)), "the same run id");
    assert_eq!(s.ripple_nodes().len(), 2);
    assert_eq!(svc.run(&s, &id)["outcome"], "already_processed");
    assert_eq!(s.ripple_nodes().len(), 2, "one record set");
}

/// Killed after the batch landed and the dump was marked, before the
/// journal's `done`: the next run calls no model and lands nothing.
#[test]
fn s3_killed_after_the_put() {
    let s = Scratch::new("s3");
    let door = Door::start(&s);
    let id = door.dump("another note");
    let script = s.script("s3", json!([{"output": ideas(2)}]));
    let mut svc = Service::start(
        &s,
        &door,
        &[("CAULDRON_RIPPLES_INFERENCE", format!("scripted:{}", script.display())), ("CAULDRON_RIPPLES_FAULT", "after-put".into())],
    );
    let k = Service::key(&s);
    let a = svc.say(&format!("RUN {k} {}", json!({"cauldron": "kb", "dump": id})));
    let run = a["run"].as_str().unwrap().to_string();
    let status = svc.child.wait().unwrap();
    assert!(!status.success(), "the fault point stops the process");
    assert_eq!(s.ripple_nodes().len(), 2);
    assert_eq!(s.dump_state(&id)["state"], "processed");
    let none = s.script("s3b", json!([]));
    let svc = Service::start(&s, &door, &[("CAULDRON_RIPPLES_INFERENCE", format!("scripted:{}", none.display()))]);
    let k = Service::key(&s);
    assert_eq!(svc.say(&format!("STATUS {k} {}", json!({"run": run})))["state"], "interrupted");
    let out = svc.run(&s, &id);
    assert_eq!(out["outcome"], "already_processed", "{out}");
    assert!(calls(&none).is_empty());
    assert_eq!(s.ripple_nodes().len(), 2, "one record set");
}

/// **A call a kill cuts short is charged.** The service is killed during
/// the model call three times; each call stays charged (one call, its
/// estimate), so the fourth run is `deferred` at the cap of 3 with no call,
/// and `STATUS` shows 3 calls today and on the dump, all 3 unsettled.
#[test]
fn s4_calls_killed_mid_way_count_against_the_cap() {
    let s = Scratch::new("s4");
    let door = Door::start(&s);
    let id = door.dump("a note whose calls are killed");
    let slow = s.script("s4", json!(vec![json!({"output": ideas(1), "sleep_ms": 60000, "sent": true}); 6]));
    let vars = [("CAULDRON_RIPPLES_INFERENCE", format!("scripted:{}", slow.display()))];
    for i in 1..=3 {
        let mut svc = Service::start(&s, &door, &vars);
        let k = Service::key(&s);
        let a = svc.say(&format!("RUN {k} {}", json!({"cauldron": "kb", "dump": id})));
        assert_eq!(a["ok"], json!(true), "kill {i}: {a}");
        wait_for("the call is in flight", || s.journal().iter().filter(|l| l["event"] == "sent").count() == i);
        svc.child.kill().unwrap();
        svc.child.wait().unwrap();
    }
    assert_eq!(calls(&slow).len(), 3);
    let svc = Service::start(&s, &door, &vars);
    let out = svc.run(&s, &id);
    assert_eq!((out["outcome"].clone(), out["reason"].clone(), out["calls"].clone()), (json!("deferred"), json!("budget"), json!(0)), "{out}");
    assert_eq!(calls(&slow).len(), 3, "no fourth call: the cap of 3 holds");
    let k = Service::key(&s);
    let st = svc.say(&format!("STATUS {k} {}", json!({"cauldron": "kb"})));
    assert_eq!((st["today"]["calls"].clone(), st["today"]["unsettled_calls"].clone()), (json!(3), json!(3)), "{st}");
    assert_eq!(st["today"]["per_cauldron"]["kb"]["calls"], 3, "{st}");
    let row = &st["per_dump"][0];
    assert_eq!((row["dump"].clone(), row["calls"].clone(), row["unsettled_calls"].clone()), (json!(id), json!(3), json!(3)), "{st}");
    assert!(row["charged_input_tokens"].as_u64().unwrap() > 0, "{st}");
    assert_eq!(st["possibly_running"], json!([]), "each start cancelled the call the kill left");
    assert!(s.ripple_nodes().is_empty());
}
