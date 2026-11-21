//! The evaluation with scripted backends: each fixture dump on each
//! backend, the retry, the dry run, the mechanical measures, the files side
//! by side, the spend journaled only for threads, and nothing written to
//! the soup.

use serde_json::{json, Value};

use crate::support::*;
use cauldron_ripples::engine::Caps;
use cauldron_ripples::eval::{self, Spec};

const A: &str = "The build cache goes stale after a rebase. We should hash the whole tree instead of the file list, and keep the old key for a week.";
const B: &str = "Plans for the release: cut on Friday, and nobody has decided who signs the notes.";

fn fixtures(s: &Scratch) -> std::path::PathBuf {
    let d = s.0.join("fixtures");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("a-cache.txt"), A).unwrap();
    std::fs::write(d.join("b plans.md"), B).unwrap();
    std::fs::write(d.join("ignored.json"), "{}").unwrap();
    d
}

fn rec(label: &str, ty: &str, state: &str, handle: &str, body: &str, regions: &[&str]) -> Value {
    json!({"label": label, "type": ty, "state": state, "handle": handle, "body": body, "regions": regions})
}

fn spec(s: &Scratch, dumps: &std::path::Path, out: &str) -> Spec {
    Spec {
        cauldron: "kb".into(),
        dumps: dumps.to_path_buf(),
        out: s.0.join(out),
        backends: vec!["threads".into(), "local".into()],
        threads_model: "sonnet".into(),
        local_model: Some("local:qwen".into()),
        repeat: 2,
        max_calls: Some(10),
        id: "eval-test-1".into(),
    }
}

#[test]
fn ev1_backends_side_by_side_with_nothing_written() {
    let s = Scratch::new("ev1");
    let door = Door::start(&s);
    let dumps = fixtures(&s);
    let quoting = rec("n1", "decision", "decided", "hash the whole tree for the cache key", "The dump says \"hash the whole tree instead of the file list\".", &["R-1"]);
    let plain = rec("n2", "idea", "idea", "keep the old cache key for a week", "", &["R-1"]);
    let script = s.script(
        "ev1",
        json!([
            // a / threads: a region that is not one (the dry run refuses), then good.
            {"output": {"records": [rec("n1", "idea", "idea", "x", "", &["R-9"])]}, "usage": usage(3000, 100)},
            {"output": {"records": [quoting.clone(), plain.clone()]}, "usage": usage(3100, 400)},
            // a / local, run 1: good.
            {"output": {"records": [plain.clone()]}, "usage": {"input_tokens": 2000, "output_tokens": 90, "cost_usd": 0.0}},
            // a / local, run 2: misses the schema, then good.
            {"fail": "schema_unmet", "message": "not JSON", "usage": {"input_tokens": 2000, "output_tokens": 16000}},
            {"output": {"records": [quoting.clone()]}, "usage": {"input_tokens": 2100, "output_tokens": 200}},
            // b / threads: two records with the same handle, in two regions.
            {"output": {"records": [rec("n1", "question", "open", "Who signs the release notes?", "", &["R-1"]), rec("n2", "task", "ready", "who signs the release notes", "", &["R-2"])]}, "usage": usage(2500, 300)},
            // b / local, run 1: nothing worth recording.
            {"output": {"records": [], "notes": "nothing decided"}, "usage": {"input_tokens": 1900, "output_tokens": 20}},
            // b / local, run 2: busy, then good.
            {"fail": "busy", "message": "another request"},
            {"output": {"records": [rec("n1", "issue", "open", "nobody has decided who signs the notes", "", &["R-2"])]}, "usage": {"input_tokens": 1900, "output_tokens": 60}},
        ]),
    );
    let ctx = engine(&s, &door, &script, Caps::default());
    let sp = spec(&s, &dumps, "out");
    let summary = eval::run(&ctx, &sp);
    assert_eq!(summary["outcome"], "done", "{summary}");
    assert_eq!(summary["dumps"], 2);
    let rows = summary["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 6, "2 dumps × (threads + local × 2)");

    // Nothing written to the soup, nothing marked.
    assert!(s.ripple_nodes().is_empty());
    let models: Vec<Value> = calls(&script).iter().map(|c| c["model"].clone()).collect();
    assert_eq!(models, ["sonnet", "sonnet", "local:qwen", "local:qwen", "local:qwen", "sonnet", "local:qwen", "local:qwen", "local:qwen"].map(|m| json!(m)));

    let out = s.0.join("out");
    let read = |p: &str| -> Value { serde_json::from_str(&std::fs::read_to_string(out.join(p)).unwrap()).unwrap() };
    // a / threads: refused by the dry run, then valid on the retry.
    let a_t = read("a-cache/threads.json");
    assert_eq!((a_t["first_try_valid"].clone(), a_t["valid"].clone(), a_t["valid_after_retry"].clone()), (json!(false), json!(true), json!(true)), "{a_t}");
    assert_eq!(a_t["attempts"][0]["schema_valid"], true);
    assert_eq!(a_t["attempts"][0]["dry_run"]["code"], "batch_refused");
    assert_eq!(a_t["attempts"][1]["dry_run"]["ok"], true);
    assert_eq!(a_t["usage"]["input_tokens"], 6100);
    assert_eq!(a_t["measures"]["records"], 2);
    assert_eq!(a_t["measures"]["cites"]["records_quoting_the_dump"], 1, "the quote, not the paraphrase: {a_t}");
    assert_eq!(a_t["measures"]["regions"]["coverage"], 0.5);
    assert_eq!(a_t["records"].as_array().unwrap().len(), 2);
    assert!(calls(&script)[1]["prompt"].as_str().unwrap().contains("Your previous answer was refused"));
    // a / local: two runs, the second retried after a schema miss.
    let a_l1 = read("a-cache/local-1.json");
    assert_eq!((a_l1["first_try_valid"].clone(), a_l1["calls"].clone()), (json!(true), json!(1)));
    assert_eq!(a_l1["measures"]["cites"]["records_quoting_the_dump"], 0);
    let a_l2 = read("a-cache/local-2.json");
    assert_eq!((a_l2["first_try_valid"].clone(), a_l2["valid_after_retry"].clone()), (json!(false), json!(true)), "{a_l2}");
    assert_eq!(a_l2["attempts"][0]["failure"]["code"], "schema_unmet");
    assert_eq!(a_l2["usage"]["output_tokens"], 16200);
    // b: the label from a file name with a space; duplicates; an empty answer; busy.
    let b_t = read("b-plans/threads.json");
    assert_eq!(b_t["measures"]["duplicate_handles"], json!([{"handle": "who signs the release notes", "times": 2}]));
    assert_eq!(b_t["measures"]["regions"]["used_known"], 2);
    assert_eq!(b_t["measures"]["types"], json!({"question": 1, "task": 1}));
    let b_l1 = read("b-plans/local-1.json");
    assert_eq!((b_l1["valid"].clone(), b_l1["measures"]["records"].clone(), b_l1["notes"].clone()), (json!(true), json!(0), json!("nothing decided")));
    let b_l2 = read("b-plans/local-2.json");
    assert_eq!((b_l2["calls"].clone(), b_l2["attempts"][0]["failure"]["code"].clone(), b_l2["valid"].clone()), (json!(2), json!("busy"), json!(true)));

    // The files a person reads.
    let md = std::fs::read_to_string(out.join("summary.md")).unwrap();
    assert!(md.contains("| dump | backend | run | valid 1st try |") && md.contains("| a-cache | threads | 1 | no | yes | 2 | 1 | 1 / 2 |"), "{md}");
    let side = std::fs::read_to_string(out.join("a-cache/side-by-side.md")).unwrap();
    assert!(side.contains("## threads `sonnet` (run 1)") && side.contains("## local `local:qwen` (run 2)") && side.contains("hash the whole tree for the cache key"), "{side}");
    assert!(!side.contains("keep the old key for a week."), "the dump's text is not copied out");
    assert!(read("eval.json")["nothing_written"].as_str().unwrap().contains("no record landed"));
    assert!(std::fs::read_to_string(out.join("progress.jsonl")).unwrap().lines().count() >= 13);
    assert!(!out.join("ignored").exists());

    // The spend: threads calls in the evaluations' ledger, each charged by
    // a `call` line before it was sent; local ones not at all; nothing in
    // the run journal, so nothing against the cauldron's caps for runs.
    let j = s.journal();
    assert!(j.is_empty(), "the run journal is untouched: {j:?}");
    assert_eq!(cauldron_ripples::journal::day(&j, "kb", None, &cauldron_ripples::journal::today()), Default::default());
    let l = s.eval_ledger();
    let called: Vec<&Value> = l.iter().filter(|l| l["event"] == "called").collect();
    assert_eq!(called.len(), 3, "{l:?}");
    assert!(called.iter().all(|l| l["eval_id"] == "eval-test-1" && l["model"] == "sonnet" && l["cauldron"] == "kb"));
    let events: Vec<&str> = l.iter().map(|l| l["event"].as_str().unwrap()).collect();
    assert_eq!(events, ["call", "called", "call", "called", "call", "called"], "charged before each call");
    let d = cauldron_ripples::journal::day(&l, "kb", None, &cauldron_ripples::journal::today());
    assert_eq!(d.calls, 3);
    assert_eq!(d.tokens, called.iter().map(|l| l["charged"].as_u64().unwrap()).sum::<u64>(), "settled to what each `called` line charged");
    assert!(d.tokens >= 3000 + 3100 + 2500);

    // A second evaluation into the same directory is refused.
    let again = eval::run(&ctx, &sp);
    assert_eq!(again["outcome"], "failed");
    assert!(again["message"].as_str().unwrap().contains("not empty"), "{again}");
    let mut empty = spec(&s, &s.0.join("nothing"), "out2");
    std::fs::create_dir_all(&empty.dumps).unwrap();
    assert!(eval::run(&ctx, &empty)["message"].as_str().unwrap().contains("no fixture dump"));
    empty.local_model = None;
    assert!(eval::run(&ctx, &empty)["message"].as_str().unwrap().contains("needs a model"));
    let mut unbounded = spec(&s, &dumps, "out3");
    unbounded.max_calls = None;
    assert!(eval::run(&ctx, &unbounded)["message"].as_str().unwrap().contains("max_calls"), "threads needs its bound");
    assert!(!s.0.join("out3").exists(), "refused before anything");
}

#[test]
fn ev2_measures_by_hand() {
    let offered = vec!["R-1".to_string(), "R-2".to_string(), "R-3".to_string()];
    let m = eval::measures(
        &json!({"records": [
            {"handle": "Cut on Friday", "body": "", "type": "decision", "regions": ["R-1", "R-7"]},
            {"handle": "cut on friday!", "body": "said `nobody has decided who signs`", "type": "question", "regions": ["R-1"]},
        ]}),
        B,
        &offered,
    );
    assert_eq!(m["records"], 2);
    assert_eq!(m["cites"]["records_quoting_the_dump"], 1);
    assert_eq!(m["cites"]["fraction"], 0.5);
    assert_eq!(m["regions"]["unknown"], json!(["R-7"]));
    assert_eq!(m["regions"]["coverage"], 0.333);
    assert_eq!(m["duplicate_handles"], json!([{"handle": "cut on friday", "times": 2}]));
    assert_eq!(eval::measures(&json!("not an answer"), B, &offered)["records"], 0);
}

#[test]
fn ev3_an_evaluation_through_the_service() {
    let s = Scratch::new("ev3");
    let door = Door::start(&s);
    let dumps = fixtures(&s);
    let good = json!({"output": {"records": [rec("n1", "idea", "idea", "hash the tree", "", &["R-1"])]}, "usage": usage(1000, 50)});
    let script = s.script("ev3", json!([good.clone(), good.clone()]));
    let svc = Service::start(&s, &door, &[("CAULDRON_RIPPLES_INFERENCE", format!("scripted:{}", script.display()))]);
    let k = Service::key(&s);
    let out = s.0.join("svc-out");
    let bad = svc.say(&format!("EVAL {k} {}", json!({"cauldron": "kb", "dumps": dumps, "out": "relative", "backends": ["threads"]})));
    assert_eq!(bad["code"], "usage", "{bad}");
    let bad = svc.say(&format!("EVAL {k} {}", json!({"cauldron": "kb", "dumps": dumps, "out": out, "backends": ["cloud"]})));
    assert_eq!(bad["code"], "usage", "{bad}");
    // Threads without its bound, then unconfirmed: refused, nothing sent.
    let bad = svc.say(&format!("EVAL {k} {}", json!({"cauldron": "kb", "dumps": dumps, "out": out, "backends": ["threads"]})));
    assert_eq!(bad["code"], "usage", "{bad}");
    assert!(bad["message"].as_str().unwrap().contains("max_calls"), "{bad}");
    let req = json!({"cauldron": "kb", "dumps": dumps, "out": out, "backends": ["threads"], "max_calls": 3});
    let bad = svc.say(&format!("EVAL {k} {req}"));
    assert_eq!(bad["code"], "unconfirmed", "{bad}");
    // The plan: what it may spend, before anything is sent.
    let mut p = req.clone();
    p["plan"] = json!(true);
    let plan = svc.say(&format!("EVAL {k} {p}"));
    assert_eq!(plan["ok"], true, "{plan}");
    let t = &plan["plan"]["threads"];
    assert_eq!((t["model"].clone(), t["max_calls"].clone(), t["calls_at_most"].clone()), (json!("opus"), json!(3), json!(3)), "the threads model follows the ripple default: {plan}");
    assert!(t["estimated_input_tokens_at_most"].as_u64().unwrap() > 0);
    assert!(calls(&script).is_empty() && !out.exists() && s.eval_ledger().is_empty(), "nothing sent or written");
    let mut go = req.clone();
    go["confirmed"] = json!(true);
    let a = svc.say(&format!("EVAL {k} {go}"));
    assert_eq!(a["ok"], true, "{a}");
    let id = a["run"].as_str().unwrap().to_string();
    assert!(id.starts_with("eval-"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let result = loop {
        let st = svc.say(&format!("STATUS {k} {}", json!({"run": id})));
        if st["state"] == "done" {
            break st["result"].clone();
        }
        assert!(std::time::Instant::now() < deadline, "{st}\n{}", svc.log());
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!(result["outcome"], "done", "{result}");
    assert_eq!(result["rows"].as_array().unwrap().len(), 2);
    assert!(out.join("summary.md").exists() && out.join("a-cache").join("threads.json").exists());
    assert!(s.ripple_nodes().is_empty());
    let st = svc.say(&format!("STATUS {k} {}", json!({"cauldron": "kb"})));
    assert_eq!(st["runs"], json!([]), "an evaluation is not a run: {st}");
    assert_eq!(st["today"]["calls"], 0, "not the cauldron's runs' spend: {st}");
    assert_eq!(st["evaluations_today"]["calls"], 2, "the evaluations' own: {st}");
}

/// ev4: an evaluation's own bound stops it sending; its call is charged
/// before it is sent; and it never takes a cauldron's daily dump slots.
#[test]
fn ev4_the_bound_charge_before_send_and_the_dump_cap_untouched() {
    let s = Scratch::new("ev4");
    let door = Door::start(&s);
    let dumps = fixtures(&s);
    let good = json!({"output": {"records": [rec("n1", "idea", "idea", "hash the tree", "", &["R-1"])]}, "usage": usage(1000, 50)});
    // The first call answers slowly, so its charge can be seen first.
    let mut slow = good.clone();
    slow["sleep_ms"] = json!(1500);
    let script = s.script("ev4", json!([slow, good.clone(), good.clone(), good.clone()]));
    // One dump a day for the cauldron's runs: a threads evaluation of two
    // fixtures must leave that slot free.
    let caps = Caps { day_dumps: 1, ..Caps::default() };
    let ctx = std::sync::Arc::new(engine(&s, &door, &script, caps));
    let mut sp = spec(&s, &dumps, "out");
    sp.backends = vec!["threads".into()];
    sp.max_calls = Some(1);
    // An empty directory given with an open mode is made owner-only.
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(&sp.out).unwrap();
    std::fs::set_permissions(&sp.out, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (c2, sp2) = (ctx.clone(), sp.clone());
    let t = std::thread::spawn(move || eval::run(&c2, &sp2));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !s.eval_ledger().iter().any(|l| l["event"] == "call") {
        assert!(std::time::Instant::now() < deadline, "no charge was written");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let l = s.eval_ledger();
    assert!(!l.iter().any(|l| l["event"] == "called"), "charged before the answer: {l:?}");
    assert!(l[0]["estimated_input_tokens"].as_u64().unwrap() > 0);
    let summary = t.join().unwrap();
    assert_eq!(summary["outcome"], "done", "{summary}");
    for d in [s.0.join("out"), s.0.join("out/a-cache")] {
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700, "{}", d.display());
    }
    // The bound: one call, then the rest stopped with nothing sent.
    assert_eq!(calls(&script).len(), 1);
    let b = serde_json::from_str::<Value>(&std::fs::read_to_string(s.0.join("out/b-plans/threads.json")).unwrap()).unwrap();
    assert!(b["stopped"].as_str().unwrap().contains("--max-calls"), "{b}");
    assert_eq!(s.eval_ledger().iter().filter(|l| l["event"] == "call").count(), 1);
    // A real run the same day is not deferred by the evaluation.
    let id = door.dump("the cache key should hash the whole tree");
    let out = cauldron_ripples::engine::run(&ctx, &ask(&id));
    assert_ne!(out["reason"], "daily_cap", "{out}");
    assert_eq!(out["outcome"], "processed", "{out}");
}
