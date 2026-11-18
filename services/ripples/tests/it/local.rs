//! The `local` backend, in process and through the service binary, against
//! a fake `inference` seam (a loopback line server in this process that
//! answers `COMPLETE` and `CANCEL` as `cauldron-inference` does). No model,
//! no llama-server: what the backend sends, what it journals, that a local
//! run is held to the same schema, caps and marking, and that its retry
//! stays local.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::support::*;
use cauldron_ripples::engine::{self, Caps};

/// The fake inference seam: each `COMPLETE` takes the next answer (the last
/// again when they run out); every line is recorded, key checked.
pub struct FakeInference {
    pub addr: SocketAddr,
    pub seen: Arc<Mutex<Vec<(String, Value)>>>,
}

const SHA: &str = "fixture-sha256-of-the-model-file";

/// An `ok` answer carrying `output`.
pub fn ok(output: Value) -> Value {
    json!({"ok": true, "model": "qwen", "sha256": SHA, "output": output, "usage": {"prompt_tokens": 2100, "completion_tokens": 340},
           "timings": {"prompt_ms": 700.0, "predicted_ms": 5600.0}, "tokens_per_second": 60.7, "wall_ms": 6400, "finish_reason": "stop"})
}

impl FakeInference {
    pub fn start(s: &Scratch, answers: Vec<Value>) -> FakeInference {
        let dir = s.data().join("inference");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("door.secret"), "fake-door-key").unwrap();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        env::egress::grant_addr(addr).unwrap();
        let seen: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let (seen2, answers) = (seen.clone(), Arc::new(answers));
        let n = Arc::new(Mutex::new(0usize));
        std::thread::spawn(move || {
            for conn in l.incoming().flatten() {
                let (seen, answers, n) = (seen2.clone(), answers.clone(), n.clone());
                std::thread::spawn(move || {
                    let mut out = conn.try_clone().unwrap();
                    let mut line = String::new();
                    if BufReader::new(conn).read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut parts = line.trim().splitn(3, ' ');
                    let (verb, key, body) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or("{}"));
                    let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                    seen.lock().unwrap().push((verb.to_string(), body.clone()));
                    let a = if key != "fake-door-key" {
                        json!({"ok": false, "code": "unauthorized", "message": "no"})
                    } else if verb == "CANCEL" {
                        json!({"ok": true, "confirmed": true, "detail": format!("request {} was disconnected", body["request_id"].as_str().unwrap_or(""))})
                    } else {
                        let mut g = n.lock().unwrap();
                        let a = answers.get(*g).or(answers.last()).cloned().unwrap_or(Value::Null);
                        *g += 1;
                        a
                    };
                    // `{"hang_up": true}`: the line is read and no answer
                    // is written (the answer is lost).
                    if a["hang_up"] == true {
                        return;
                    }
                    let _ = writeln!(out, "{a}");
                });
            }
        });
        FakeInference { addr, seen }
    }

    pub fn completes(&self) -> Vec<Value> {
        self.seen.lock().unwrap().iter().filter(|(v, _)| v == "COMPLETE").map(|(_, b)| b.clone()).collect()
    }
}

/// An engine whose default backend is the scripted one and whose local
/// backend dials `fake`.
fn local_engine(s: &Scratch, door: &Door, fake: Option<&FakeInference>, caps: Caps) -> (engine::Ctx, std::path::PathBuf) {
    let script = s.script("default", json!([]));
    let mut ctx = engine(s, door, &script, caps);
    let mut local = cauldron_ripples::local::Local::new(&s.env());
    local.seam = Some(fake.map_or("127.0.0.1:1".to_string(), |f| f.addr.to_string()));
    ctx.inference = Box::new(cauldron_ripples::local::Routed { default: ctx.inference, local: Box::new(local) });
    (ctx, script)
}

fn ask_local(dump: &str) -> engine::Ask {
    let mut a = ask(dump);
    a.model = Some("local:qwen".into());
    a
}

/// l1: a local run lands cited, unreviewed records like any run; what it
/// sends is the package's prompt and a schema, no tools; the journal names
/// the backend, the model, the file's sha256, the tokens and the rate.
#[test]
fn l1_a_local_run_is_held_to_the_same_package_and_journals_what_ran() {
    let s = Scratch::new("l1");
    let door = Door::start(&s);
    let id = door.dump("the cache should hash the whole tree; rebases make it stale");
    let fake = FakeInference::start(&s, vec![ok(ideas(2))]);
    let (ctx, script) = local_engine(&s, &door, Some(&fake), Caps::default());
    let out = engine::run(&ctx, &ask_local(&id));
    assert_eq!(out["outcome"], "processed", "{out}");
    assert_eq!(out["backend"], "local");
    assert_eq!(out["models"], json!(["local:qwen"]));
    assert_eq!(out["usage"]["input_tokens"], 2100);
    assert_eq!(out["usage"]["output_tokens"], 340);
    assert_eq!(out["usage"]["cost_usd"], 0.0);
    let records: Vec<String> = out["records"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
    assert_eq!(s.ripple_nodes(), records);
    let got = soup_door::reads::get(&s.target(), &records, &Default::default()).unwrap();
    for r in got["records"].as_array().unwrap() {
        assert_eq!(r["unreviewed"], json!(true));
        assert!(r["meta"]["cites"].as_array().unwrap().contains(&json!({"kind": "dump", "id": id})));
        assert_eq!(r["meta"]["ripple"]["model"], "local:qwen");
    }
    assert_eq!(s.dump_state(&id)["state"], "processed");
    assert!(calls(&script).is_empty(), "the default backend was not called");

    // What was sent.
    let c = fake.completes();
    assert_eq!(c.len(), 1);
    let keys: Vec<&String> = c[0].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["max_output_tokens", "model", "request_id", "schema", "system", "timeout_s", "user"]);
    assert_eq!(c[0]["model"], "qwen");
    assert!(c[0]["user"].as_str().unwrap().starts_with("You turn one dump into records") && c[0]["user"].as_str().unwrap().contains("rebases make it stale"));
    assert_eq!(c[0]["max_output_tokens"], cauldron_ripples::engine::MAX_OUTPUT_TOKENS);
    assert_eq!(c[0]["schema"], cauldron_ripples::local::grammar_schema(&cauldron_ripples::package::schema()));

    // The journal.
    let j = s.journal();
    let events: Vec<&str> = j.iter().map(|l| l["event"].as_str().unwrap()).collect();
    assert_eq!(events, ["started", "read", "call", "sent", "local", "scrubbed", "called", "done"]);
    assert_eq!(j[0]["backend"], "local");
    assert_eq!(j[0]["model"], "local:qwen");
    let l = &j[4];
    assert_eq!((l["model"].clone(), l["sha256"].clone(), l["prompt_tokens"].clone(), l["completion_tokens"].clone(), l["tokens_per_second"].clone()), (json!("local:qwen"), json!(SHA), json!(2100), json!(340), json!(60.7)));
    assert_eq!(l["cost_usd"], 0.0);
    assert_eq!(j[6]["answered_by"], "local:qwen");
    assert_eq!(j[7]["backend"], "local");
    assert!(!std::fs::read_to_string(s.ripples_dir().join("runs.jsonl")).unwrap().contains("rebases"), "no dump text in the journal");
}

/// l2: an answer that misses the schema is retried once on the SAME local
/// model (never Opus), with the refusal.
#[test]
fn l2_the_retry_of_a_local_run_stays_local() {
    let s = Scratch::new("l2");
    let door = Door::start(&s);
    let id = door.dump("a note about caching");
    let bad = json!({"records": [{"label": "n1", "type": "decision", "state": "idea", "handle": "x", "regions": ["R-1"]}]});
    let fake = FakeInference::start(&s, vec![ok(bad), json!({"ok": false, "code": "not_json", "message": "the answer does not meet the schema: it is not JSON", "detail": {"usage": {"prompt_tokens": 2000, "completion_tokens": 16000}}})]);
    let (ctx, script) = local_engine(&s, &door, Some(&fake), Caps::default());
    let out = engine::run(&ctx, &ask_local(&id));
    assert_eq!(out["outcome"], "failed", "{out}");
    assert_eq!(out["code"], "answer_refused");
    assert_eq!(out["models"], json!(["local:qwen", "local:qwen"]));
    let c = fake.completes();
    assert_eq!(c.len(), 2);
    assert_eq!(c[1]["model"], "qwen", "the retry is local");
    assert!(c[1]["user"].as_str().unwrap().contains("Your previous answer was refused"));
    assert!(calls(&script).is_empty());
    assert_eq!(out["usage"]["output_tokens"], 16340, "the unmet answer's tokens are counted");
    assert!(s.ripple_nodes().is_empty());
    assert_eq!(s.dump_state(&id)["state"], "received");
}

/// l3: a local backend that is always busy (another request holds it) is
/// tried 3 times by a run, which then ends `deferred` (reason `busy`); a
/// busy answer reached no model, so it is charged nothing and the dump's
/// caps are untouched. A run makes at most those 3 tries.
#[test]
fn l3_a_busy_local_backend_is_tried_a_bounded_number_of_times_and_charged_nothing() {
    let s = Scratch::new("l3");
    let door = Door::start(&s);
    let id = door.dump("a note");
    let fake = FakeInference::start(&s, vec![json!({"ok": false, "code": "busy", "message": "another request is in progress"})]);
    let (ctx, _) = local_engine(&s, &door, Some(&fake), Caps::default());
    let out = engine::run(&ctx, &ask_local(&id));
    assert_eq!((out["outcome"].clone(), out["reason"].clone()), (json!("deferred"), json!("busy")), "{out}");
    assert_eq!(fake.completes().len(), engine::BUSY_TRIES as usize);
    assert_eq!(cauldron_ripples::journal::spent(&s.journal(), out["run"].as_str().unwrap()), (0, 0));
    let again = engine::run(&ctx, &ask_local(&id));
    assert_eq!((again["outcome"].clone(), again["reason"].clone()), (json!("deferred"), json!("busy")));
    assert_eq!(fake.completes().len(), 2 * engine::BUSY_TRIES as usize, "a run's tries are bounded");
}

/// l4: no inference service: failed, with how to start it; nothing sent,
/// nothing written.
#[test]
fn l4_no_inference_service() {
    let s = Scratch::new("l4");
    let door = Door::start(&s);
    let id = door.dump("a note");
    let (ctx, _) = local_engine(&s, &door, None, Caps::default());
    let out = engine::run(&ctx, &ask_local(&id));
    assert_eq!((out["outcome"].clone(), out["code"].clone()), (json!("failed"), json!("inference_unavailable")), "{out}");
    assert!(out["what_to_do"].as_str().unwrap().contains("start inference"), "{out}");
    assert!(s.ripple_nodes().is_empty());
    assert_eq!(s.dump_state(&id)["state"], "received");
    let called = s.journal().into_iter().find(|l| l["event"] == "called").unwrap();
    assert_eq!(called["charged"], 0, "nothing was sent, so nothing is charged");
}

/// l5: which model a local run means, and settling a local call a kill left.
#[test]
fn l5_the_model_a_local_run_means_and_settling_a_left_call() {
    let s = Scratch::new("l5");
    let e = s.env();
    let r = cauldron_ripples::seam::local_model(&e, None).unwrap_err();
    assert!(r["message"].as_str().unwrap().contains("no model is registered"), "{r}");
    let mut reg = inference_registry::Registry::default();
    let model = |n: &str| inference_registry::Model {
        name: n.into(),
        path: "/nowhere/m.gguf".into(),
        size_bytes: 1,
        mtime: 0,
        sha256: SHA.into(),
        added_at: "2026-01-01T00:00:00Z".into(),
        context: None,
        facts: Default::default(),
        file: None,
    };
    reg.add(model("qwen")).unwrap();
    reg.save(&s.data()).unwrap();
    assert_eq!(cauldron_ripples::seam::local_model(&e, None).unwrap(), "local:qwen");
    assert_eq!(cauldron_ripples::seam::local_model(&e, Some("gpt".into())).unwrap(), "local:gpt");
    assert_eq!(cauldron_ripples::seam::local_model(&e, Some("local:gpt".into())).unwrap(), "local:gpt");
    assert_eq!(cauldron_ripples::seam::local_model(&e, Some("Bad Name".into())).unwrap_err()["code"], "usage");
    reg.add(model("gpt")).unwrap();
    reg.save(&s.data()).unwrap();
    assert!(cauldron_ripples::seam::local_model(&e, None).unwrap_err()["message"].as_str().unwrap().contains("--model"));

    let door = Door::start(&s);
    let fake = FakeInference::start(&s, vec![]);
    let (ctx, _) = local_engine(&s, &door, Some(&fake), Caps::default());
    ctx.journal.write("run-x", "kb", "DM-9", "sent", json!({"backend": "local", "model": "local:qwen", "request_id": "ripple-left-1"})).unwrap();
    let settled = engine::settle(&ctx);
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["cancel"]["confirmed"], true);
    let cancels: Vec<Value> = fake.seen.lock().unwrap().iter().filter(|(v, _)| v == "CANCEL").map(|(_, b)| b.clone()).collect();
    assert_eq!(cancels, [json!({"request_id": "ripple-left-1"})]);
    assert!(engine::settle(&ctx).is_empty(), "settled once");
}

/// l6: `RUN {backend: local}` through the service binary.
#[test]
fn l6_a_local_run_through_the_service() {
    let s = Scratch::new("l6");
    let door = Door::start(&s);
    let id = door.dump("the cache key should hash the whole tree");
    let fake = FakeInference::start(&s, vec![ok(ideas(1))]);
    let script = s.script("l6", json!([]));
    let grants = format!("{}@{pid},{}@{pid}", door.seam, fake.addr, pid = std::process::id());
    let svc = Service::start(
        &s,
        &door,
        &[
            ("CAULDRON_RIPPLES_INFERENCE", format!("scripted:{}", script.display())),
            ("CAULDRON_RIPPLES_INFERENCE_SEAM", fake.addr.to_string()),
            ("CAULDRON_ISOLATED_PORTS", grants),
        ],
    );
    let k = Service::key(&s);
    let bad = svc.say(&format!("RUN {k} {}", json!({"cauldron": "kb", "dump": id, "backend": "cloud"})));
    assert_eq!(bad["code"], "usage", "{bad}");
    let none = svc.say(&format!("RUN {k} {}", json!({"cauldron": "kb", "dump": id, "backend": "local"})));
    assert_eq!(none["code"], "usage", "no model registered and none named: {none}");
    let a = svc.say(&format!("RUN {k} {}", json!({"cauldron": "kb", "dump": id, "backend": "local", "model": "qwen"})));
    assert_eq!(a["ok"], true, "{a}");
    let run = a["run"].as_str().unwrap().to_string();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let out = loop {
        let st = svc.say(&format!("STATUS {k} {}", json!({"run": run})));
        if st["state"] == "done" {
            break st["result"].clone();
        }
        assert!(std::time::Instant::now() < deadline, "{st}\n{}", svc.log());
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!(out["outcome"], "processed", "{out}\n{}", svc.log());
    assert_eq!(out["backend"], "local");
    assert_eq!(out["models"], json!(["local:qwen"]));
    assert!(calls(&script).is_empty());
    assert_eq!(fake.completes().len(), 1);
}

/// l7: the local backend dials only loopback. A non-loopback seam, from
/// the test variable or from the seam table, is refused before anything is
/// dialled, at the call (charged 0) and at the service's start.
#[test]
fn l7_only_a_loopback_inference_seam_is_dialled() {
    let s = Scratch::new("l7");
    let door = Door::start(&s);
    let id = door.dump("a note");
    let script = s.script("default", json!([]));
    // The override, set on the backend.
    let mut ctx = engine(&s, &door, &script, Caps::default());
    let mut local = cauldron_ripples::local::Local::new(&s.env());
    local.seam = Some("192.0.2.7:443".into());
    ctx.inference = Box::new(cauldron_ripples::local::Routed { default: ctx.inference, local: Box::new(local) });
    let out = engine::run(&ctx, &ask_local(&id));
    assert_eq!(out["code"], "inference_unavailable", "{out}");
    assert!(out.to_string().contains("not a loopback address"), "{out}");
    assert_eq!(s.journal().into_iter().find(|l| l["event"] == "called").unwrap()["charged"], 0);
    assert!(!s.journal().iter().any(|l| l["event"] == "sent" && l["backend"] == "local" && l["request_id"].is_null()));

    // The seam table: an `inference` row that is not loopback (a copy of
    // the row a ripples service registers, its address changed).
    drop(Service::start(&s, &door, &[]));
    let seams = s.data().join("seams.json");
    let mut t: Value = serde_json::from_str(&std::fs::read_to_string(&seams).unwrap()).unwrap();
    let mut row = t["seams"]["ripples"].clone();
    assert!(row.is_object(), "a row to copy: {t}");
    row["service"] = json!("inference");
    row["addr"] = json!("192.0.2.7:443");
    t["seams"]["inference"] = row;
    std::fs::write(&seams, t.to_string()).unwrap();
    let id2 = door.dump("another note");
    let mut ctx = engine(&s, &door, &script, Caps::default());
    ctx.inference = Box::new(cauldron_ripples::local::Routed { default: ctx.inference, local: Box::new(cauldron_ripples::local::Local::new(&s.env())) });
    let out = engine::run(&ctx, &ask_local(&id2));
    assert_eq!(out["code"], "inference_unavailable", "{out}");
    assert!(out.to_string().contains("(seam table)") && out.to_string().contains("not a loopback address"), "{out}");

    // At start: the service refuses the variable naming another host.
    let mut child = Service::command(&s, &door, &[("CAULDRON_RIPPLES_INFERENCE_SEAM", "192.0.2.7:443".into())])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("the service started with a non-loopback inference seam");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let mut err = String::new();
    std::io::Read::read_to_string(child.stderr.as_mut().unwrap(), &mut err).unwrap();
    assert!(!status.success());
    assert!(err.contains("CAULDRON_RIPPLES_INFERENCE_SEAM") && err.contains("not a loopback address"), "{err}");
}

/// l8: a COMPLETE whose answer is lost after it was sent is given up AND
/// cancelled (the service may still be generating), and charged; not
/// reported as "start the inference service".
#[test]
fn l8_a_lost_answer_is_cancelled_not_called_unavailable() {
    let s = Scratch::new("l8");
    let door = Door::start(&s);
    let id = door.dump("a note");
    let fake = FakeInference::start(&s, vec![json!({"hang_up": true})]);
    let (ctx, _) = local_engine(&s, &door, Some(&fake), Caps { calls: 1, ..Caps::default() });
    let out = engine::run(&ctx, &ask_local(&id));
    assert_ne!(out["code"], "inference_unavailable", "{out}");
    assert!(out.to_string().contains("was given up") && out.to_string().contains("cancelled"), "{out}");
    let cancels: Vec<Value> = fake.seen.lock().unwrap().iter().filter(|(v, _)| v == "CANCEL").map(|(_, b)| b.clone()).collect();
    assert_eq!(cancels.len(), 1, "the call was cancelled: {cancels:?}");
    let j = s.journal();
    assert!(j.iter().any(|l| l["event"] == "cancel" && l["backend"] == "local" && l["confirmed"] == true), "{j:?}");
    let called = j.iter().find(|l| l["event"] == "called").unwrap();
    assert!(called["charged"].as_u64().unwrap() > 0, "a call given up after it was sent is charged: {called}");
}
