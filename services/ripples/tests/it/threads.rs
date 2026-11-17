//! The `threads` backend against a fake threads chassis (a websocket in
//! this process that speaks the frames the real one does): the welcome and
//! the secret, `threads/create` with the restricted request, the read-back
//! that refuses an older threads, `threads/<id>/send`, `threads/<id>/history`
//! read to the judged answer, and the cancel of every call given up. No
//! model is called.

use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::support::*;
use cauldron_ripples::inference::{Failure, Inference, Request};
use cauldron_ripples::threads::{Threads, DENIED_TOOLS, INJECTIONS};

/// How the fake behaves.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// The current threads: keeps every setting it was sent.
    Current,
    /// An older threads: drops the settings it does not know (`completion`)
    /// and says nothing.
    Older,
    /// Stays busy after a cancel.
    Stuck,
    /// Sends a stream of unrelated frames and never answers a request.
    Chatty,
}

/// A fake chassis: answers every request, records them, and hands out
/// `events` on the first history read after the send.
struct Fake {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Value>>>,
}

impl Fake {
    fn methods(&self) -> Vec<String> {
        self.seen.lock().unwrap().iter().filter_map(|f| f["method"].as_str().map(|m| m.replace("t-fake", "<t>"))).collect()
    }

    fn frame(&self, method: &str) -> Value {
        self.seen.lock().unwrap().iter().find(|f| f["method"].as_str().is_some_and(|m| m.ends_with(method))).cloned().unwrap_or(Value::Null)
    }
}

fn fake_with(secret: Option<&'static str>, events: Vec<Value>, mode: Mode) -> Fake {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    env::egress::grant_addr(addr).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let stored: Arc<Mutex<Value>> = Arc::new(Mutex::new(Value::Null));
    let cancelled = Arc::new(Mutex::new(false));
    std::thread::spawn(move || {
        for stream in l.incoming().flatten() {
            let (log, events, stored, cancelled) = (log.clone(), events.clone(), stored.clone(), cancelled.clone());
            std::thread::spawn(move || {
                let mut ws = tungstenite::accept(stream).unwrap();
                let send = |ws: &mut tungstenite::WebSocket<std::net::TcpStream>, v: Value| ws.send(tungstenite::Message::Text(v.to_string())).is_ok();
                send(&mut ws, json!({"type": "welcome", "proto": 1, "daemon": "threads", "daemon_version": "0", "boot_id": "b", "auth_required": secret.is_some()}));
                let mut handed = false;
                loop {
                    let Ok(tungstenite::Message::Text(t)) = ws.read() else { return };
                    let f: Value = serde_json::from_str(&t).unwrap();
                    log.lock().unwrap().push(f.clone());
                    if f["type"] == "auth" {
                        let ok = Some(f["secret"].as_str().unwrap_or("")) == secret;
                        send(&mut ws, json!({"type": "auth_ack", "ok": ok}));
                        if !ok {
                            return;
                        }
                        continue;
                    }
                    if mode == Mode::Chatty {
                        for i in 0.. {
                            if !send(&mut ws, json!({"type": "shape", "n": i})) {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(20));
                        }
                    }
                    let m = f["method"].as_str().unwrap_or("");
                    let payload = if m == "threads/create" {
                        let mut s = f["params"]["settings"].clone();
                        if mode == Mode::Older {
                            s.as_object_mut().unwrap().remove("completion");
                        }
                        *stored.lock().unwrap() = s;
                        json!({"thread_id": "t-fake", "promise_id": "p", "visible": true})
                    } else if m == "threads/list" {
                        let idle = *cancelled.lock().unwrap() && mode != Mode::Stuck;
                        json!({"threads": [
                            {"thread_id": "t-other", "status": "busy", "executing": true, "settings": {}},
                            {"thread_id": "t-fake", "status": if idle { "idle" } else { "busy" }, "executing": !idle, "settings": stored.lock().unwrap().clone()}
                        ]})
                    } else if m.ends_with("/cancel") {
                        *cancelled.lock().unwrap() = true;
                        json!({"cancelled": true, "thread_id": "t-fake"})
                    } else if m.ends_with("/queue_cancel") {
                        json!({"promise_id": "q", "visible": true, "changed": true})
                    } else if m.ends_with("/send") {
                        json!({"accepted": true, "thread_id": "t-fake", "message_id": 7})
                    } else if m.ends_with("/history") && !handed {
                        handed = true;
                        json!({"thread_id": "t-fake", "events": events, "next_after": events.len()})
                    } else {
                        json!({"thread_id": "t-fake", "events": [], "next_after": events.len()})
                    };
                    send(&mut ws, json!({"type": "response", "correlate": f["id"], "outcome": {"type": "success", "payload": payload}}));
                }
            });
        }
    });
    Fake { addr, seen }
}

fn fake(secret: Option<&'static str>, events: Vec<Value>) -> Fake {
    fake_with(secret, events, Mode::Current)
}

fn ev(seq: u64, event: Value) -> Value {
    json!({"seq": seq, "message_id": 7, "ts": "2026-01-01T00:00:00Z", "event": event})
}

fn usage_ev(seq: u64, id: &str, input: u64, output: u64) -> Value {
    ev(seq, json!({"kind": "message_usage", "model": "claude-sonnet-x", "input_tokens": input, "output_tokens": output, "cache_read_tokens": 100, "cache_creation_tokens": 0, "api_message_id": id}))
}

fn answer(seq: u64, attempt: u64, status: &str, value: Value) -> Value {
    ev(seq, json!({"kind": "custom", "event_type": "cauldron.threads/structured_output",
        "data": {"turn": 1, "for_message": 7, "attempt": attempt, "schema_sha": "s", "status": status, "value": value, "errors": []}}))
}

type Notes = Arc<Mutex<Vec<(String, Value)>>>;

fn req_noting() -> (Request, Notes) {
    let notes: Notes = Arc::default();
    let n = notes.clone();
    let r = Request {
        model: "sonnet".into(),
        prompt: "the prompt".into(),
        schema: cauldron_ripples::package::schema(),
        cwd: std::path::PathBuf::from("/scratch/ripple-workdir"),
        name: "run-of-a-test".into(),
        max_output_tokens: 16_000,
        sink: Some(Arc::new(move |e: &str, v: Value| n.lock().unwrap().push((e.to_string(), v)))),
    };
    (r, notes)
}

fn req() -> Request {
    req_noting().0
}

/// **Every property of the request a ripple makes**, as the fake endpoint
/// received it: a bare completion with no tools, no MCP, no settings, no
/// plugins or hooks, never a skip mode, one turn, the schema, a bounded
/// answer, the prompt sent bare; the thread read back before the send.
#[test]
fn t1_the_request_is_a_bare_completion_and_its_tokens_are_counted_once() {
    let f = fake(
        Some("s3cret-for-the-fake"),
        vec![
            usage_ev(1, "msg-a", 7000, 400),
            usage_ev(2, "msg-a", 7000, 400),
            usage_ev(3, "msg-b", 1000, 50),
            ev(4, json!({"kind": "result_turn", "subtype": "success", "is_error": false, "total_cost_usd": 0.05})),
            answer(5, 1, "ok", ideas(1)),
        ],
    );
    let t = Threads::new(&f.addr.to_string(), Some("s3cret-for-the-fake".into()), Some("ripples:kb".into()), Duration::from_secs(30));
    let (r, notes) = req_noting();
    let out = t.call(&r).unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(out.output, ideas(1));
    assert_eq!((out.usage.input_tokens, out.usage.output_tokens, out.usage.cache_read_tokens), (8000, 450, 200));
    assert_eq!(out.usage.cost_usd, Some(0.05));
    assert_eq!(out.model, "claude-sonnet-x");
    let seen = f.seen.lock().unwrap().clone();
    assert_eq!(seen[0], json!({"type": "auth", "secret": "s3cret-for-the-fake"}));
    assert_eq!(f.methods(), ["threads/create", "threads/list", "threads/<t>/send", "threads/<t>/history"], "the read-back comes before the send");

    let s = &f.frame("threads/create")["params"]["settings"];
    assert_eq!(s["model"], "sonnet");
    assert_eq!(s["cwd"], "/scratch/ripple-workdir", "the run's private directory");
    assert_eq!(s["completion"], json!({"max_output_tokens": 16000}), "a bare completion with a bounded answer");
    assert_eq!(s["permission_mode"], "dontAsk", "never a skip-permissions mode");
    assert_eq!(s["tools"]["clean_slate"], json!(true), "no built-in tool");
    assert_eq!(s["tools"]["allowed_tools"], json!([]), "an explicit empty allow list");
    assert_eq!((s["tools"]["tools"].clone(), s["tools"]["agents_json"].clone()), (Value::Null, Value::Null));
    let denied: Vec<&str> = s["tools"]["disallowed_tools"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
    for t in ["Bash", "Edit", "Write", "Read", "WebFetch", "WebSearch", "Agent", "Task", "Skill", "mcp__threads"] {
        assert!(denied.contains(&t), "{t} denied: {denied:?}");
    }
    assert_eq!(denied.len(), DENIED_TOOLS.len());
    assert_eq!((s["plugin_roots"].clone(), s["plugin_dirs"].clone(), s["add_dirs"].clone()), (json!([]), json!([]), json!([])), "no plugins, no other directory");
    assert_eq!((s["append_system_prompt"].clone(), s["agent"].clone()), (Value::Null, Value::Null));
    for k in INJECTIONS {
        assert_eq!(s[*k], json!(false), "{k}");
    }
    assert_eq!(s["budget_key"], "ripples:kb");
    assert!(s.get("max_budget_usd").is_none() && s.get("fallback_model").is_none());

    let send = &f.frame("/send")["params"];
    assert_eq!(send["text"], "the prompt");
    assert_eq!(send["bare"], json!(true), "no preamble or header");
    assert_eq!(send["metadata"]["cauldron.threads"]["json_schema"], cauldron_ripples::package::schema(), "structured output by JSON schema");
    assert_eq!(send["metadata"]["cauldron.threads"]["schema_retries"], 0, "one turn: no corrective turn");
    // The call in flight was reported before its answer; nothing to cancel.
    let notes = notes.lock().unwrap().clone();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!((notes[0].0.as_str(), notes[0].1["thread"].clone(), notes[0].1["message_id"].clone()), ("sent", json!("t-fake"), json!(7)));
}

/// **The turn's totals are the tokens.** threads records one
/// `message_usage` per API message, as first streamed (24 output tokens of
/// an answer that wrote 386); the `result_turn`'s `model_usage` holds the
/// turn's real totals, and they replace the per-message counts.
#[test]
fn t9_the_result_totals_replace_the_first_streamed_counts() {
    let mu = json!({"claude-sonnet-x": {"inputTokens": 2, "outputTokens": 386, "cacheReadInputTokens": 2323, "cacheCreationInputTokens": 2215, "costUSD": 0.013}});
    let f = fake(
        None,
        vec![
            usage_ev(1, "msg-a", 2, 24),
            ev(2, json!({"kind": "result_turn", "subtype": "success", "is_error": false, "total_cost_usd": 0.013, "model_usage": mu})),
            answer(3, 1, "ok", ideas(1)),
        ],
    );
    let t = Threads::new(&f.addr.to_string(), None, None, Duration::from_secs(30));
    let out = t.call(&req()).unwrap_or_else(|e| panic!("{e:?}"));
    let u = &out.usage;
    assert_eq!((u.input_tokens, u.output_tokens, u.cache_read_tokens, u.cache_creation_tokens), (2, 386, 2323, 2215), "{u:?}");
    assert_eq!(u.cost_usd, Some(0.013));
}

/// An older threads keeps none of it: refused before anything is sent.
#[test]
fn t4_an_older_threads_is_refused_before_anything_is_sent() {
    let f = fake_with(None, vec![answer(1, 1, "ok", ideas(1))], Mode::Older);
    let t = Threads::new(&f.addr.to_string(), None, None, Duration::from_secs(30));
    match t.call(&req()) {
        Err(Failure::Unsupported(m)) => assert!(m.contains("does not support the restricted request") && m.contains("Nothing was sent"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(f.methods(), ["threads/create", "threads/list"], "no send");
}

/// Every exit but an answer cancels the call (`cancel`, then the message
/// withdrawn) and confirms the thread idle.
#[test]
fn t2_unmet_parked_and_unreachable_and_every_give_up_cancels() {
    // Unmet on the one turn: no corrective turn is waited for.
    let f = fake(None, vec![usage_ev(1, "m1", 10, 1), answer(2, 1, "unmet", json!({}))]);
    let t = Threads::new(&f.addr.to_string(), None, None, Duration::from_secs(30));
    match t.call(&req()) {
        Err(Failure::SchemaUnmet { usage, .. }) => assert_eq!(usage.input_tokens, 10),
        other => panic!("{other:?}"),
    }
    // Parked: cancelled, withdrawn, confirmed.
    let f = fake(None, vec![ev(1, json!({"kind": "turn_parked", "cause": "budget", "reason": "over", "message_ids": [7]}))]);
    let t = Threads::new(&f.addr.to_string(), None, None, Duration::from_secs(30));
    let (r, notes) = req_noting();
    assert!(matches!(t.call(&r), Err(Failure::Parked(_))));
    let m = f.methods();
    let (c, q) = (m.iter().position(|x| x == "threads/<t>/cancel").unwrap(), m.iter().position(|x| x == "threads/<t>/queue_cancel").unwrap());
    assert!(c < q, "{m:?}");
    assert_eq!(f.frame("/queue_cancel")["params"]["message_id"], 7);
    let notes = notes.lock().unwrap().clone();
    let cancel = &notes.iter().find(|(e, _)| e == "cancel").unwrap().1;
    assert_eq!((cancel["confirmed"].clone(), cancel["thread"].clone()), (json!(true), json!("t-fake")), "{cancel}");
    // The secret asked for and not held.
    let f = fake(Some("x"), vec![]);
    let t = Threads::new(&f.addr.to_string(), None, None, Duration::from_secs(30));
    assert!(matches!(t.call(&req()), Err(Failure::Unavailable(_))));
    // Nothing listening (port 1, which nothing may bind).
    let t = Threads::new("127.0.0.1:1", None, None, Duration::from_secs(5));
    assert!(matches!(t.call(&req()), Err(Failure::Unavailable(_))));
}

/// A turn that ends without an answer ends the call at once, cancelled.
#[test]
fn t5_a_turn_that_ends_without_an_answer_is_terminal() {
    for kind in ["turn_failed", "turn_interrupted", "turn_killed", "budgets_failed", "retry_starving", "limit_hit"] {
        let f = fake(None, vec![usage_ev(1, "m1", 50, 1), ev(2, json!({"kind": kind, "message": "ended"}))]);
        let t = Threads::new(&f.addr.to_string(), None, None, Duration::from_secs(60));
        let started = Instant::now();
        match t.call(&req()) {
            Err(Failure::Failed { message, usage }) => {
                assert!(message.contains(kind), "{message}");
                assert_eq!(usage.input_tokens, 50);
            }
            other => panic!("{kind}: {other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(20), "{kind} waited {:?}", started.elapsed());
        assert!(f.methods().contains(&"threads/<t>/cancel".to_string()), "{kind}");
    }
}

/// A re-delivery threads schedules ends the call at once, cancelled: a
/// failure carrying the reason (the home directory named `~`), or a park for
/// a limit on the account. Never a wait for the time limit.
#[test]
fn t8_a_scheduled_re_delivery_ends_the_call() {
    let home = std::env::var("HOME").unwrap();
    for (kind, parked) in [("claude_error", false), ("transient", false), ("usage_limit", true)] {
        let reason = format!("unclassified error: Error: refused at {home}/x");
        let f = fake(None, vec![usage_ev(1, "m1", 7, 1), ev(2, json!({"kind": "retry_scheduled", "error_kind": kind, "reason": reason, "attempt": 2}))]);
        let t = Threads::new(&f.addr.to_string(), None, None, Duration::from_secs(60));
        let started = Instant::now();
        match (t.call(&req()), parked) {
            (Err(Failure::Failed { message, usage }), false) => {
                assert!(message.contains(kind) && message.contains("Error: refused at ~/x"), "{message}");
                assert!(!message.contains(&home), "{message}");
                assert_eq!(usage.input_tokens, 7);
            }
            (Err(Failure::Parked(m)), true) => assert!(m.contains(kind), "{m}"),
            (other, _) => panic!("{kind}: {other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(20), "{kind} waited {:?}", started.elapsed());
        assert!(f.methods().contains(&"threads/<t>/cancel".to_string()), "{kind}");
        assert!(f.methods().contains(&"threads/<t>/queue_cancel".to_string()), "{kind}: the message is withdrawn");
    }
}

/// The time limit cancels; a thread that stays busy is reported as
/// possibly still running, with how to stop it.
#[test]
fn t6_a_timeout_cancels_and_a_cancel_that_does_not_take_says_how_to_stop() {
    let f = fake(None, vec![usage_ev(1, "m1", 5, 1)]);
    let t = Threads::new(&f.addr.to_string(), None, None, Duration::from_secs(2));
    let (r, notes) = req_noting();
    match t.call(&r) {
        Err(Failure::Failed { message, .. }) => assert!(message.contains("no answer from threads within 2 s"), "{message}"),
        other => panic!("{other:?}"),
    }
    let n = notes.lock().unwrap().clone();
    assert_eq!(n.iter().find(|(e, _)| e == "cancel").unwrap().1["confirmed"], json!(true));

    let f = fake_with(None, vec![], Mode::Stuck);
    let t = Threads::new(&f.addr.to_string(), None, None, Duration::from_secs(2));
    let (r, notes) = req_noting();
    assert!(matches!(t.call(&r), Err(Failure::Failed { .. })));
    let n = notes.lock().unwrap().clone();
    let c = &n.iter().find(|(e, _)| e == "cancel").unwrap().1;
    assert_eq!(c["confirmed"], json!(false), "{c}");
    assert!(c["how_to_stop"].as_str().unwrap().contains("may still be running") && c["how_to_stop"].as_str().unwrap().contains("stop the threads service"), "{c}");
    // The backend's own cancel, from a `sent` handle (a kill's leftover).
    let again = t.cancel(&json!({"thread": "t-fake", "message_id": 7}));
    assert_eq!(again["confirmed"], json!(false));
}

/// A server that keeps sending other frames cannot hold a call past its
/// deadline.
#[test]
fn t7_a_chatty_server_cannot_hold_a_call() {
    let f = fake_with(None, vec![], Mode::Chatty);
    let t = Threads::new(&f.addr.to_string(), None, None, Duration::from_secs(2));
    let started = Instant::now();
    assert!(matches!(t.call(&req()), Err(Failure::Failed { .. })));
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

/// The service with its default backend (threads), pointed at the fake: a
/// run end to end.
#[test]
fn t3_the_service_runs_a_dump_through_the_threads_backend() {
    let s = Scratch::new("t3");
    let door = Door::start(&s);
    let id = door.dump("the cache key ignores rebases");
    let f = fake(Some("fake-secret"), vec![usage_ev(1, "m1", 9000, 600), answer(2, 1, "ok", ideas(2))]);
    let svc = Service::start(&s, &door, &[("CAULDRON_RIPPLES_THREADS_ADDR", f.addr.to_string()), ("THREADS_SECRET", "fake-secret".into())]);
    let out = svc.run(&s, &id);
    assert_eq!(out["outcome"], "processed", "{out}\n{}", svc.log());
    assert_eq!((out["backend"].clone(), out["usage"]["input_tokens"].clone()), (json!("threads"), json!(9000)));
    assert_eq!(s.ripple_nodes().len(), 2);
    let send = f.frame("/send");
    assert!(send["params"]["text"].as_str().unwrap().contains("the cache key ignores rebases"));
    // The working directory: private, made for the call, gone after it.
    let cwd = f.frame("threads/create")["params"]["settings"]["cwd"].as_str().unwrap().to_string();
    assert!(cwd.contains("cauldron-ripple-") && !std::path::Path::new(&cwd).exists(), "{cwd}");
    assert!(!cwd.starts_with(&s.ripples_dir().display().to_string()), "never the service's directory: {cwd}");
}
