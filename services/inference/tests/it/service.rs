//! The service binary over the fake `llama-server`: load on demand, the
//! exact argv, one completion, the key, idle unload, the memory budget, the
//! runtime check, crashes and their backoff, cancel and timeout, and that
//! no child outlives the service.

use serde_json::{json, Value};

use crate::support::*;

fn answer() -> Value {
    json!({"records": [{"label": "n1", "type": "idea", "state": "idea", "handle": "a local idea", "regions": ["R-1"]}]})
}

fn ask(model: Option<&str>) -> Value {
    let mut a = json!({"system": "Answer with JSON only.", "user": "the dump text", "schema": {"type": "object"}, "max_output_tokens": 800, "timeout_s": 30, "request_id": "r-1"});
    if let Some(m) = model {
        a["model"] = json!(m);
    }
    a
}

/// i1: nothing loads until asked; the first request loads the registered
/// model with the exact argv, answers with tokens and timing; the key is
/// required; a request carrying tools is refused; the idle timer unloads
/// it and the child is gone.
#[test]
fn i1_on_demand_load_one_completion_and_idle_unload() {
    let s = Scratch::new("i1");
    s.fake(&Fake { load_ms: Some(300), ..Fake::default() }, "idle_seconds = 3\ncontext = 4096\n");
    let m = s.small("tiny");
    s.answers(json!([content(&answer())]));
    let svc = Service::start(&s);

    let st = svc.status(&s);
    assert_eq!(st["state"], "idle", "{st}");
    assert_eq!(st["model"], Value::Null);
    assert_eq!(st["runtime"]["ok"], true, "{st}");
    assert_eq!(st["runtime"]["build"], 12110);
    assert_eq!(st["registered"], json!(["tiny"]));
    assert!(s.fake_argv().is_empty(), "nothing was loaded at start");

    // The key.
    for line in ["STATUS {}".to_string(), format!("STATUS {}x {{}}", Service::key(&s)), format!("COMPLETE {}", "0".repeat(64))] {
        assert_eq!(svc.say(&line)["code"], "unauthorized", "{line}");
    }
    // No tools, ever.
    let mut t = ask(None);
    t["tools"] = json!([{"type": "function", "function": {"name": "exec_shell_command"}}]);
    let r = svc.verb(&s, "COMPLETE", t);
    assert_eq!(r["code"], "usage", "{r}");
    assert!(s.fake_argv().is_empty(), "a refused request loads nothing");

    let r = svc.verb(&s, "COMPLETE", ask(None));
    assert_eq!(r["ok"], true, "{r}\n{}", svc.log());
    assert_eq!(r["output"], answer());
    assert_eq!(r["model"], "tiny");
    assert_eq!(r["sha256"], m.sha256);
    assert_eq!(r["usage"], json!({"prompt_tokens": 1200, "completion_tokens": 300}));
    assert_eq!(r["tokens_per_second"], 60.0);
    assert_eq!(r["timings"]["predicted_ms"], 5000.0);
    assert!(r["load_ms"].as_u64().unwrap() >= 300, "{r}");
    assert_eq!(r["request_id"], "r-1");

    // The argv, exactly.
    let argv = s.fake_argv();
    let key = s.dir().join("llama.key");
    let port = argv[argv.iter().position(|a| a == "--port").unwrap() + 1].clone();
    let want: Vec<String> = [
        "--model", &m.path.display().to_string(), "--alias", "tiny",
        "--host", "127.0.0.1", "--port", &port, "--api-key-file", &key.display().to_string(),
        "--ctx-size", "4096", "--parallel", "1",
        "--no-webui", "--no-slots", "--offline", "--jinja",
        "--no-agent", "--reasoning", "off", "--cache-ram", "0",
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
    assert_eq!(argv, want);
    // The child's environment: none of the service's LLAMA_ARG_* (tools, an
    // agent) reached it.
    let names: Vec<String> = serde_json::from_str(&std::fs::read_to_string(s.fake_dir().join("env.json")).unwrap()).unwrap();
    assert!(!names.iter().any(|n| n.starts_with("LLAMA_") || n.starts_with("HF_")), "{names:?}");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&key).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::metadata(s.dir().join("door.secret")).unwrap().permissions().mode() & 0o777, 0o600);

    // The request the child got: authorized, schema-constrained, no tools.
    let reqs = s.fake_lines("requests.jsonl");
    assert!(reqs.iter().all(|r| r["authorized"] == true), "{reqs:?}");
    let c: Vec<&Value> = reqs.iter().filter(|r| r["path"] == "/v1/chat/completions").collect();
    assert_eq!(c.len(), 1);
    let b = &c[0]["body"];
    assert_eq!(b["response_format"], json!({"type": "json_schema", "json_schema": {"name": "answer", "strict": true, "schema": {"type": "object"}}}));
    assert_eq!(b["messages"], json!([{"role": "system", "content": "Answer with JSON only."}, {"role": "user", "content": "the dump text"}]));
    assert_eq!(b["max_tokens"], 800);
    assert_eq!(b["stream"], false);
    for k in ["tools", "tool_choice", "functions"] {
        assert!(b.get(k).is_none(), "{k} in {b}");
    }

    let st = svc.status(&s);
    assert_eq!(st["state"], "ready", "{st}");
    let child = st["model"]["child_pid"].as_u64().unwrap() as u32;
    assert_eq!(child, read_pid(&s.fake_dir().join("pid")));
    assert!(alive(child));
    assert_eq!(st["model"]["memory_estimate"]["context"], 4096);
    assert_eq!(st["model"]["memory_estimate"]["fits"], true);
    // 4 layers × 2 KV heads × (64 + 64) × 2 bytes × 4096 tokens.
    assert_eq!(st["model"]["memory_estimate"]["kv_bytes"], 4 * 2 * 128 * 2 * 4096);
    assert!(st["model"]["unloads_in_s"].as_u64().unwrap() <= 3);
    assert_eq!(st["served"], 1);

    // A second request: no load.
    let r2 = svc.verb(&s, "COMPLETE", ask(Some("tiny")));
    assert_eq!(r2["ok"], true, "{r2}");
    assert!(r2.get("load_ms").is_none(), "{r2}");
    assert_eq!(s.fake_lines("requests.jsonl").iter().filter(|r| r["path"] == "/v1/chat/completions").count(), 2);

    // Idle: unloaded, and the child is gone.
    assert!(eventually(10, || svc.status(&s)["state"] == "idle"), "{}", svc.status(&s));
    let st = svc.status(&s);
    assert_eq!(st["last_unload"]["reason"], "idle", "{st}");
    assert!(eventually(5, || !alive(child)), "the child is gone after an idle unload");
    assert!(!s.dir().join("llama.key").exists(), "its key is gone with it");
    assert!(!s.dir().join("child.json").exists());
}

/// i2: the memory budget, an unknown model, a changed file, and a runtime
/// that is missing, too old, or lacks a flag: each refused with what to do,
/// and no child is started.
#[test]
fn i2_refusals_before_any_child() {
    use inference_registry::gguf::Meta;
    let s = Scratch::new("i2");
    s.fake(&Fake::default(), "memory_budget_gib = 1\ncontext = 65536\n");
    // 80 layers × 8 KV heads × (128 + 128) × 2 bytes × 65536 tokens = 20 GiB.
    s.model(
        "big",
        &[
            ("general.architecture", Meta::Str("llama".into())),
            ("llama.block_count", Meta::Int(80)),
            ("llama.attention.head_count", Meta::Int(64)),
            ("llama.attention.head_count_kv", Meta::Int(8)),
            ("llama.attention.key_length", Meta::Int(128)),
            ("llama.attention.value_length", Meta::Int(128)),
        ],
        1024,
    );
    let svc = Service::start(&s);
    let r = svc.verb(&s, "COMPLETE", ask(Some("big")));
    assert_eq!(r["code"], "over_budget", "{r}");
    let msg = r["message"].as_str().unwrap();
    for part in ["weights", "KV cache 20.00 GiB", "80 layers × 8 KV heads × (128 + 128) × 2 bytes", "× 65536 tokens", "2.00 GiB for compute buffers", "OVER the memory budget of 1.00 GiB"] {
        assert!(msg.contains(part), "`{part}` in: {msg}");
    }
    assert_eq!(r["detail"]["fits"], false);
    assert!(s.fake_argv().is_empty(), "nothing was started");

    let r = svc.verb(&s, "COMPLETE", ask(Some("nope")));
    assert_eq!(r["code"], "unknown_model", "{r}");
    assert!(r["message"].as_str().unwrap().contains("registered: big"), "{r}");

    // Two registered and none named: which one is not guessed.
    s.small("tiny");
    let r = svc.verb(&s, "COMPLETE", ask(None));
    assert_eq!(r["code"], "unknown_model", "{r}");
    assert!(r["message"].as_str().unwrap().contains("--model"), "{r}");

    // A file changed since it was registered.
    std::fs::write(s.0.join("models").join("tiny.gguf"), b"GGUF and something else").unwrap();
    let r = svc.verb(&s, "COMPLETE", ask(Some("tiny")));
    assert_eq!(r["code"], "model_changed", "{r}");

    // A file swapped for another of the same size, its times copied with
    // `touch -r`, renamed over the registered one: read again, refused by
    // its sha256, never loaded.
    let reg = s.small("swap");
    let other = s.0.join("models").join("other.gguf");
    inference_registry::gguf::write_header(
        &other,
        &[
            ("general.architecture", Meta::Str("llama".into())),
            ("llama.block_count", Meta::Int(4)),
            ("llama.attention.head_count", Meta::Int(8)),
            ("llama.attention.head_count_kv", Meta::Int(3)),
            ("llama.embedding_length", Meta::Int(512)),
        ],
        4096,
    )
    .unwrap();
    assert_eq!(std::fs::metadata(&other).unwrap().len(), reg.size_bytes);
    assert!(std::process::Command::new("/usr/bin/touch").arg("-r").arg(&reg.path).arg(&other).status().unwrap().success());
    std::fs::rename(&other, &reg.path).unwrap();
    let r = svc.verb(&s, "COMPLETE", ask(Some("swap")));
    assert_eq!(r["code"], "model_changed", "{r}");
    let msg = r["message"].as_str().unwrap();
    assert!(msg.contains("its sha256 is now") && msg.contains(&reg.sha256) && msg.contains("cauldron inference models add swap"), "{msg}");
    assert!(s.fake_argv().is_empty(), "nothing was started");

    // A registry entry whose header facts are out of range (written by
    // hand, or by another version): refused by name before any estimate.
    let mut r = inference_registry::Registry::load(&s.data()).unwrap();
    let mut m = r.remove("big").unwrap();
    m.name = "wild".into();
    m.facts.block_count = Some(1 << 40);
    r.add(m).unwrap();
    r.save(&s.data()).unwrap();
    let t0 = std::time::Instant::now();
    let r = svc.verb(&s, "COMPLETE", ask(Some("wild")));
    assert_eq!(r["code"], "model_header", "{r}");
    assert!(r["message"].as_str().unwrap().contains("`block_count` is 1099511627776"), "{r}");
    assert!(t0.elapsed() < std::time::Duration::from_secs(10));
    assert!(s.fake_argv().is_empty(), "nothing was started");
    drop(svc);

    // The runtime: too old, lacking a flag, missing.
    let s = Scratch::new("i2rt");
    s.small("tiny");
    for (fake, want) in [
        (Fake { version: Some("version: 4000 (0000000)".into()), ..Fake::default() }, "needs build 6000 or later"),
        (Fake { help_drop: Some("--json-schema".into()), ..Fake::default() }, "does not list --json-schema"),
    ] {
        s.fake(&fake, "");
        let svc = Service::start(&s);
        assert_eq!(svc.status(&s)["runtime"]["ok"], false);
        let r = svc.verb(&s, "COMPLETE", ask(None));
        assert_eq!(r["code"], "runtime", "{r}");
        assert!(r["message"].as_str().unwrap().contains(want), "{r}");
        assert!(s.fake_argv().is_empty());
    }
    std::fs::write(s.dir().join("config.toml"), format!("llama_server = \"{}\"\n", s.0.join("nowhere").join("llama-server").display())).unwrap();
    let svc = Service::start(&s);
    let r = svc.verb(&s, "COMPLETE", ask(None));
    assert_eq!(r["code"], "runtime", "{r}");
    assert!(r["message"].as_str().unwrap().contains("not a file"), "{r}");
    let st = svc.status(&s);
    assert_eq!(st["runtime"]["ok"], false, "{st}");
}

/// i3: a child that crashes during a request, and one that fails to load:
/// recorded with the end of its log, the next load waits out the backoff,
/// then loads again; a served request resets the count.
#[test]
fn i3_crash_backoff_and_restart() {
    let s = Scratch::new("i3");
    s.fake(&Fake::default(), "");
    s.small("tiny");
    s.answers(json!([{"exit": 3}, content(&answer())]));
    let svc = Service::start(&s);
    let r = svc.verb(&s, "COMPLETE", ask(None));
    assert_eq!(r["code"], "crashed", "{r}");
    assert_eq!(r["detail"]["exit"], 3);
    let st = svc.status(&s);
    assert_eq!(st["state"], "idle", "{st}");
    assert_eq!(st["crashes_in_a_row"], 1);
    assert_eq!(st["last_crash"]["exit"], 3);
    assert_eq!(st["last_crash"]["during"], "serving");
    assert!(st["last_crash"]["log_tail"].as_str().unwrap().contains("exiting during a request on purpose"), "{st}");
    assert!(st["backoff_until"].is_string(), "{st}");
    let first_pid = read_pid(&s.fake_dir().join("pid"));
    assert!(!alive(first_pid));

    // In the backoff: busy, with when.
    let r = svc.verb(&s, "COMPLETE", ask(None));
    assert_eq!(r["code"], "busy", "{r}");
    assert!(r["detail"]["retry_after_s"].as_u64().unwrap() >= 1);
    // After it: loaded again, served, and the count reset.
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let r = svc.verb(&s, "COMPLETE", ask(None));
    assert_eq!(r["ok"], true, "{r}");
    assert_ne!(read_pid(&s.fake_dir().join("pid")), first_pid, "a new child");
    let st = svc.status(&s);
    assert_eq!(st["crashes_in_a_row"], 0, "{st}");
    drop(svc);

    // A child that exits while loading.
    let s = Scratch::new("i3load");
    s.fake(&Fake { exit_on_start: Some(5), ..Fake::default() }, "");
    s.small("tiny");
    let svc = Service::start(&s);
    let r = svc.verb(&s, "COMPLETE", ask(None));
    assert_eq!(r["code"], "load_failed", "{r}");
    assert!(r["message"].as_str().unwrap().contains("exit 5") && r["message"].as_str().unwrap().contains("failing to load on purpose"), "{r}");
    let st = svc.status(&s);
    assert_eq!(st["state"], "idle");
    assert_eq!(st["crashes_in_a_row"], 1, "a load that fails at start is a crash, counted once: {st}");
    assert_eq!(st["last_crash"]["during"], "loading", "{st}");
    assert!(st["backoff_until"].is_string(), "{st}");
}

/// i4: cancel and timeout disconnect llama-server (which stops its
/// generation); a second request meanwhile is `busy`; text that is not
/// JSON, and a prompt over the context, are refused by name.
#[test]
fn i4_cancel_timeout_busy_and_bad_answers() {
    let s = Scratch::new("i4");
    s.fake(&Fake::default(), "");
    s.small("tiny");
    s.answers(json!([{"sleep_ms": 8000, "content": "{}"}, {"sleep_ms": 8000, "content": "{}"}, {"content": "Sure! Here are the records"}, {"status": 400, "error": "the request exceeds the available context size, try increasing it"}]));
    let svc = std::sync::Arc::new(Service::start(&s));
    let key = Service::key(&s);
    let (svc2, key2) = (svc.clone(), key.clone());
    let started = std::time::Instant::now();
    let t = std::thread::spawn(move || svc2.say(&format!("COMPLETE {key2} {}", ask(None))));
    assert!(eventually(10, || !svc.status(&s)["in_flight"].is_null()));
    let b = svc.verb(&s, "COMPLETE", ask(None));
    assert_eq!(b["code"], "busy", "{b}");
    let c = svc.verb(&s, "CANCEL", json!({"request_id": "r-1"}));
    assert_eq!(c["confirmed"], true, "{c}");
    let r = t.join().unwrap();
    assert_eq!(r["code"], "cancelled", "{r}");
    assert!(started.elapsed().as_secs() < 6, "cancel did not wait for the answer");
    assert!(eventually(10, || s.fake_lines("events.jsonl").iter().any(|e| e["event"] == "disconnected" && e["request"] == 0)), "llama-server saw the disconnect");

    let mut a = ask(None);
    a["timeout_s"] = json!(1);
    let r = svc.verb(&s, "COMPLETE", a);
    assert_eq!(r["code"], "timeout", "{r}");
    assert!(eventually(10, || s.fake_lines("events.jsonl").iter().any(|e| e["event"] == "disconnected" && e["request"] == 1)));

    let r = svc.verb(&s, "COMPLETE", ask(None));
    assert_eq!(r["code"], "not_json", "{r}");
    assert_eq!(r["detail"]["usage"]["completion_tokens"], 300);
    assert!(r["detail"]["text"].as_str().unwrap().starts_with("Sure!"));
    let r = svc.verb(&s, "COMPLETE", ask(None));
    assert_eq!(r["code"], "too_long", "{r}");
    assert_eq!(svc.verb(&s, "CANCEL", json!({"request_id": "gone"}))["detail"], "request gone is not running");

    // UNLOAD during a request: the request is told it was unloaded, not
    // that llama-server crashed, and no crash is counted.
    s.answers(json!([{"sleep_ms": 6000, "content": "{}"}]));
    let (svc2, key2) = (svc.clone(), key.clone());
    let t = std::thread::spawn(move || svc2.say(&format!("COMPLETE {key2} {}", ask(None))));
    assert!(eventually(10, || !svc.status(&s)["in_flight"].is_null()));
    let u = svc.verb(&s, "UNLOAD", json!({}));
    assert_eq!(u["ok"], true, "{u}");
    let r = t.join().unwrap();
    assert_eq!(r["code"], "unloaded", "{r}");
    assert!(r["message"].as_str().unwrap().contains("unloaded during the request") && !r["message"].as_str().unwrap().contains("crash"), "{r}");
    let st = svc.status(&s);
    assert_eq!((st["crashes_in_a_row"].clone(), st["last_crash"].clone()), (json!(0), Value::Null), "{st}");
}

/// i5: no child outlives the service: TERM stops it first (a child that
/// ignores TERM is killed after the grace); a killed service's child is
/// stopped by its guard; and a child whose guard was killed too is stopped
/// by the next start.
#[test]
fn i5_no_orphan_child() {
    let s = Scratch::new("i5");
    s.fake(&Fake::default(), "");
    s.small("tiny");
    s.answers(json!([content(&answer())]));

    // TERM.
    let mut svc = Service::start(&s);
    assert_eq!(svc.verb(&s, "COMPLETE", ask(None))["ok"], true);
    let child = svc.status(&s)["model"]["child_pid"].as_u64().unwrap() as u32;
    kill(svc.pid(), "TERM");
    assert!(svc.wait_exit(20), "the service exits on TERM");
    assert!(!alive(child), "its child was stopped before it exited");
    drop(svc);

    // KILL: the guard sees its input end.
    let mut svc = Service::start(&s);
    assert_eq!(svc.verb(&s, "COMPLETE", ask(None))["ok"], true);
    let child = svc.status(&s)["model"]["child_pid"].as_u64().unwrap() as u32;
    kill(svc.pid(), "KILL");
    assert!(svc.wait_exit(10));
    assert!(eventually(15, || !alive(child)), "a killed service's child is stopped by its guard");
    drop(svc);

    // The guard alone killed: the service stops the child itself.
    let svc = Service::start(&s);
    assert_eq!(svc.verb(&s, "COMPLETE", ask(None))["ok"], true);
    let st = svc.status(&s);
    let (child, guard) = (st["model"]["child_pid"].as_u64().unwrap() as u32, st["model"]["guard_pid"].as_u64().unwrap() as u32);
    kill(guard, "KILL");
    assert!(eventually(15, || !alive(child)), "the service stopped the child its killed guard left");
    assert_eq!(svc.status(&s)["last_crash"]["during"], "serving");
    drop(svc);

    // The service and its guard both killed (the guard held stopped so it
    // cannot react first): the next start stops what was left.
    let mut svc = Service::start(&s);
    assert_eq!(svc.verb(&s, "COMPLETE", ask(None))["ok"], true);
    let st = svc.status(&s);
    let (child, guard) = (st["model"]["child_pid"].as_u64().unwrap() as u32, st["model"]["guard_pid"].as_u64().unwrap() as u32);
    kill(guard, "STOP");
    kill(svc.pid(), "KILL");
    kill(guard, "KILL");
    assert!(svc.wait_exit(10));
    assert!(alive(child), "an orphan, for now");
    drop(svc);
    let svc = Service::start(&s);
    assert!(eventually(15, || !alive(child)), "the next start stopped the orphan: {}", svc.log());
    assert!(svc.log().contains("found and stopped"), "{}", svc.log());
    drop(svc);

    // A child that ignores TERM is killed after the grace.
    let s = Scratch::new("i5term");
    s.fake(&Fake { ignore_term: true, ..Fake::default() }, "");
    s.small("tiny");
    s.answers(json!([content(&answer())]));
    let mut svc = Service::start(&s);
    assert_eq!(svc.verb(&s, "COMPLETE", ask(None))["ok"], true);
    let child = svc.status(&s)["model"]["child_pid"].as_u64().unwrap() as u32;
    let r = svc.verb(&s, "UNLOAD", json!({}));
    assert_eq!(r["unloaded"]["reason"], "asked", "{r}");
    assert!(!alive(child), "killed after the grace");
    assert_eq!(svc.status(&s)["state"], "idle");
    kill(svc.pid(), "TERM");
    assert!(svc.wait_exit(20));
}
