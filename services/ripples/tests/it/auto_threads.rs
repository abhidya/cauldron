//! **Automatic processing through the threads backend**, as the live run
//! met it: the real service binary on its default backend, pointed at a fake
//! threads chassis (`threads::fake_gen`), and the real mcp door in this
//! process. No model is called.
//!
//! - A call that fails fast (a `claude` that exits at once, as one refusing
//!   its `--json-schema` does): retried, then parked `failed`, with fixed
//!   words only. The failure's text is built the way threads builds it, from
//!   a fake `claude` run for each call (its stderr carries a planted word)
//!   through threads' own `retry::stderr_tail`; then every file under the
//!   test's root is scanned for that word.
//! - Token counts of an automatic run are the turn's totals.
//! - A call threads parks at admission (every account over its usage
//!   threshold) is a deferral: not a failed attempt, charged nothing, and
//!   one withdrawn call per wait for every cauldron on threads, not one per
//!   dump.
//! - The hold shows in `STATUS`, and after it the cauldron served least
//!   recently is tried first, so one whose calls keep being parked does not
//!   starve another.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use env::ripples_auto as ra;
use serde_json::{json, Value};

use crate::auto::{auto_lines, on, on_for, outcomes, wait_for};
use crate::support::*;
use crate::threads::{answer, ev, fake_gen, usage_ev, Fake, Gen, Mode};

fn start(s: &Scratch, door: &Door, f: &Fake, retry_ms: u64) -> Service {
    let svc = Service::start(
        s,
        door,
        &[
            ("CAULDRON_RIPPLES_THREADS_ADDR", f.addr.to_string()),
            ("CAULDRON_RIPPLES_SCAN_MS", "200".to_string()),
            ("CAULDRON_RIPPLES_RETRY_MS", retry_ms.to_string()),
        ],
    );
    env::egress::grant_addr(svc.seam).unwrap();
    svc
}

/// A `claude` that refuses its command line before it starts, as the real
/// one refused a 2020-12 `$schema`: a warning naming the home directory,
/// the refusal (with `$MARK` in it, and the working directory), exit 1.
fn refusing_claude(s: &Scratch) -> PathBuf {
    let p = s.0.join("refusing-claude");
    std::fs::write(
        &p,
        "#!/bin/bash\n\
         printf '\\033[33mClaude configuration file not found at: %s/.claude.json\\033[0m\\n' \"$HOME\" >&2\n\
         printf 'Error: --json-schema is not a valid JSON Schema: %s (from %s)\\n' \"$MARK\" \"$PWD\" >&2\n\
         exit 1\n",
    )
    .unwrap();
    p
}

/// Every file under `dir` that holds `word`, as bytes.
fn files_holding(dir: &Path, word: &str) -> Vec<PathBuf> {
    let mut hits = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    let mut seen = 0;
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            let t = e.file_type().unwrap();
            if t.is_dir() {
                stack.push(p);
            } else if t.is_file() {
                seen += 1;
                let b = std::fs::read(&p).unwrap_or_default();
                if b.windows(word.len()).any(|w| w == word.as_bytes()) {
                    hits.push(p);
                }
            }
        }
    }
    assert!(seen > 10, "the scan saw the root's files ({seen})");
    hits
}

/// **A call that fails fast is retried, then parked, and its stderr is
/// written nowhere.** First as threads 0.1.10 ends it (`turn_failed` at
/// once, with the stderr tail), then as an older threads does (a scheduled
/// re-delivery, the tail in its reason): each dump gets exactly three calls,
/// then is parked `failed` with the fixed code and sentence. A run by hand
/// shows the tail to its caller (so the planted word did reach ripples), and
/// no file under the root holds it.
#[test]
fn at1_a_call_that_fails_fast_is_retried_then_parked_and_its_stderr_is_written_nowhere() {
    let s = Scratch::new("at1");
    let door = Door::start(&s);
    let by_hand = door.dump("a note from before it was turned on");
    on(&s, ra::serial(&by_hand), false);
    let mark = format!("PLANTED-STDERR-WORD-{}", std::process::id());
    let older = Arc::new(AtomicBool::new(false));
    let tails = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let gen: Gen = {
        let (claude, mark, older, tails) = (refusing_claude(&s), mark.clone(), older.clone(), tails.clone());
        let (home, tmp) = (s.0.join("home").display().to_string(), s.scratch_root().display().to_string());
        Arc::new(move |_n, settings: &Value| {
            let cwd = settings["cwd"].as_str().unwrap_or("").to_string();
            let out = std::process::Command::new("/bin/bash")
                .arg(&claude)
                .env("MARK", &mark)
                .env("HOME", &home)
                .current_dir(if Path::new(&cwd).is_dir() { cwd.as_str() } else { "/" })
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(1));
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            // What threads hands its caller (`runtime::scrub_prefixes`).
            let prefixes = [(cwd.as_str(), "<cwd>"), (tmp.as_str(), "$TMPDIR"), (home.as_str(), "~")];
            let tail = threads::retry::stderr_tail(&stderr, &prefixes);
            let message = threads::retry::completion_failure_message(&threads::retry::TransientKind::Deterministic, "", tail.as_deref(), &prefixes);
            assert!(message.contains(&mark) && !message.contains(&home), "{message}");
            tails.lock().unwrap().push(message.clone());
            let end = if older.load(Ordering::SeqCst) {
                json!({"kind": "retry_scheduled", "error_kind": "claude_error", "reason": message, "attempt": 2})
            } else {
                json!({"kind": "turn_failed", "error_kind": "claude_error", "message": message})
            };
            vec![usage_ev(1, "m1", 40, 0), ev(2, end)]
        })
    };
    let f = fake_gen(None, gen, Mode::Current);
    let svc = start(&s, &door, &f, 100);

    let first = door.dump("the cache key should hash the whole tree");
    wait_for("parked after three calls", 60, || s.dump_state(&first)["state"] == "failed" && outcomes(&s, &first).len() == 3);
    assert_eq!(outcomes(&s, &first), ["retry", "retry", "parked"], "{}", svc.log());
    assert_eq!(f.sends(), 3, "exactly three calls, then none");
    older.store(true, Ordering::SeqCst);
    let second = door.dump("the release checklist misses the plugin schema");
    wait_for("parked after three calls", 60, || s.dump_state(&second)["state"] == "failed" && outcomes(&s, &second).len() == 3);
    assert_eq!(outcomes(&s, &second), ["retry", "retry", "parked"], "{}", svc.log());
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(f.sends(), 6, "no call after a dump is parked");
    for d in [&first, &second] {
        let failure = &s.dump_state(d)["failure"];
        assert_eq!((failure["code"].clone(), failure["attempts"].clone()), (json!("inference_failed"), json!(3)), "{failure}");
        assert_eq!(failure["message"], json!("the model call failed"), "{failure}");
    }
    let parked: Vec<Value> = auto_lines(&s).into_iter().filter(|l| l["event"] == "outcome").collect();
    assert!(parked.iter().all(|l| l["code"] == "inference_failed" && l["message"] == "the model call failed"), "{parked:?}");

    // By hand, the caller is shown the tail: the word did reach ripples.
    let out = svc.run(&s, &by_hand);
    assert_eq!(out["code"], "inference_failed", "{out}");
    assert!(out["detail"].as_str().unwrap_or("").contains(&mark), "{out}");
    assert_eq!(tails.lock().unwrap().len(), 7);

    drop(svc);
    let hits = files_holding(&s.0, &mark);
    assert!(hits.is_empty(), "the stderr word is written in {hits:?}");
}

/// **An automatic run's tokens are the turn's totals** (`model_usage` of
/// the turn's result), not the first streamed snapshot of each message: in
/// the run journal's `called` and `done` lines, and in what the caps charge.
#[test]
fn at2_an_automatic_runs_tokens_are_the_turns_totals() {
    let s = Scratch::new("at2");
    let door = Door::start(&s);
    on(&s, 0, false);
    let mu = json!({"claude-sonnet-x": {"inputTokens": 2, "outputTokens": 386, "cacheReadInputTokens": 2323, "cacheCreationInputTokens": 2215, "costUSD": 0.013}});
    let gen: Gen = Arc::new(move |_, _| {
        vec![
            usage_ev(1, "msg-a", 2, 24),
            ev(2, json!({"kind": "result_turn", "subtype": "success", "is_error": false, "total_cost_usd": 0.013, "model_usage": mu})),
            answer(3, 1, "ok", ideas(1)),
        ]
    });
    let f = fake_gen(None, gen, Mode::Current);
    let svc = start(&s, &door, &f, 100);
    let id = door.dump("the cache key ignores rebases");
    wait_for("processed", 30, || s.dump_state(&id)["state"] == "processed" && !outcomes(&s, &id).is_empty());
    assert_eq!(outcomes(&s, &id), ["done"], "{}", svc.log());
    let lines = s.journal();
    let called = lines.iter().find(|l| l["event"] == "called").unwrap();
    assert_eq!(called["usage"]["output_tokens"], json!(386), "{called}");
    assert_eq!(called["input_total"], json!(2 + 2323 + 2215), "{called}");
    assert!(called["charged"].as_u64().unwrap() >= 2 + 2323 + 2215, "{called}");
    let done = lines.iter().find(|l| l["event"] == "done").unwrap();
    assert_eq!((done["usage"]["output_tokens"].clone(), done["usage"]["cache_read_tokens"].clone()), (json!(386), json!(2323)), "{done}");
    let started = lines.iter().find(|l| l["event"] == "started").unwrap();
    assert_eq!(started["automatic"], json!(true), "{started}");
}

/// **A park at admission is a deferral, held for every cauldron.** threads
/// parks the first calls (every account over its usage threshold), then
/// takes calls again. Five dumps: while it parks, one withdrawn call per
/// wait (the wait growing), never one per dump; each park is `deferred`
/// with reason `threads_parked` and cause `threshold`, never a failed
/// attempt, and charged nothing; once threads takes calls, all five are
/// processed, each in one call.
#[test]
fn at3_a_park_at_admission_is_a_deferral_with_one_call_per_wait() {
    let s = Scratch::new("at3");
    let door = Door::start(&s);
    on(&s, 0, false);
    let parks = 2;
    let n = Arc::new(AtomicUsize::new(0));
    let gen: Gen = {
        let n = n.clone();
        Arc::new(move |_, _| {
            if n.fetch_add(1, Ordering::SeqCst) < parks {
                vec![ev(1, json!({"kind": "turn_parked", "cause": "threshold", "reason": "every in-scope account is over a threshold (an-account: weekly_all at 96%)", "message_ids": [7]}))]
            } else {
                vec![usage_ev(1, "m1", 900, 60), answer(2, 1, "ok", ideas(1))]
            }
        })
    };
    let f = fake_gen(None, gen, Mode::Current);
    // A first wait of 1.5 s, then 6 s: long beside the 200 ms scan.
    let svc = start(&s, &door, &f, 1500);
    let ids: Vec<String> = (0..5).map(|i| door.dump(&format!("note {i} about the cache key"))).collect();
    wait_for("the first park", 20, || auto_lines(&s).iter().any(|l| l["event"] == "outcome"));
    let parked_at = Instant::now();
    std::thread::sleep(Duration::from_millis(900));
    assert_eq!(f.sends(), 1, "while held, no other dump is sent: {}", svc.log());
    let first = auto_lines(&s).into_iter().find(|l| l["event"] == "outcome").unwrap();
    assert_eq!((first["class"].clone(), first["reason"].clone(), first["park"].clone()), (json!("deferred"), json!("threads_parked"), json!("threshold")), "{first}");
    assert!(first["message"].as_str().unwrap().contains("usage threshold") && !first["message"].as_str().unwrap().contains("an-account"), "{first}");
    let parked = first["dump"].as_str().unwrap().to_string();
    let a = &s.dump_state(&parked)["auto"];
    assert_eq!((a["state"].clone(), a["reason"].clone(), a["park"].clone()), (json!("deferred"), json!("threads_parked"), json!("threshold")), "{a}");
    assert!(a["message"].as_str().unwrap().contains("not a failure"), "{a}");
    let k = Service::key(&s);
    let st = svc.say(&format!("STATUS {k} {}", json!({"cauldron": "kb"})));
    let q = &st["automatic"]["cauldrons"][0];
    assert_eq!(q["deferred"][0]["reason"], json!("threads_parked"), "{st}");
    assert_eq!(q["retrying"], json!([]), "a park is not a failed attempt: {st}");

    wait_for("all five processed", 60, || ids.iter().all(|d| s.dump_state(d)["state"] == "processed"));
    let held_for = parked_at.elapsed();
    assert_eq!(f.sends(), parks + 5, "one call per wait while parked, then one per dump");
    assert!(held_for >= Duration::from_millis(1500 + 6000 - 1000), "the second wait is longer: {held_for:?}");
    let outs: Vec<Value> = auto_lines(&s).into_iter().filter(|l| l["event"] == "outcome").collect();
    let deferred: Vec<&Value> = outs.iter().filter(|l| l["class"] == "deferred").collect();
    assert_eq!(deferred.len(), parks, "{outs:?}");
    assert!(outs.iter().all(|l| l["failed_attempts"] == json!(0)), "no park counts as a failed attempt: {outs:?}");
    // The parks were charged nothing and are not calls against the caps.
    let lines = s.journal();
    let per = cauldron_ripples::journal::per_dump(&lines, Some("kb"));
    assert_eq!(per.len(), 5);
    assert!(per.iter().all(|d| d["calls"] == json!(1) && d["charged_input_tokens"].as_u64().unwrap() >= 900), "{per:?}");
    let held: Vec<&Value> = lines.iter().filter(|l| l["event"] == "called" && l["held"] == json!(true)).collect();
    assert_eq!(held.len(), parks);
    assert!(held.iter().all(|l| l["charged"] == json!(0)), "{held:?}");
}

/// **The hold is shown, and it starves no cauldron.** Two cauldrons on
/// threads; every call for `aa` (which sorts first) is parked, every call
/// for `kb` answered. While the hold holds, `STATUS` names it (since when,
/// the cause, the next attempt, the dump that set it) and marks both
/// cauldrons held. When it is over, `kb`, served less recently, is tried
/// first and processed, though `aa` still has a due dump and keeps parking.
#[test]
fn at4_the_hold_is_shown_and_the_cauldron_served_least_recently_goes_first() {
    let s = Scratch::new("at4");
    s.make_cauldron("aa");
    let door = Door::start(&s);
    on_for(&s, "aa", 0, false);
    on(&s, 0, false);
    let root = s.data();
    // Whose call it is: the newest attempt in the automatic journal.
    let gen: Gen = Arc::new(move |_, _| {
        let last = ra::read(&root).into_iter().rev().find(|l| l["event"] == "attempt").map(|l| l["cauldron"].clone()).unwrap_or(Value::Null);
        if last == "aa" {
            vec![ev(1, json!({"kind": "turn_parked", "cause": "threshold", "reason": "over a threshold", "message_ids": [7]}))]
        } else {
            vec![usage_ev(1, "m1", 900, 60), answer(2, 1, "ok", ideas(1))]
        }
    });
    let f = fake_gen(None, gen, Mode::Current);
    let svc = start(&s, &door, &f, 800);
    let a1 = door.dump_in("aa", "a note in aa");
    wait_for("aa parked", 20, || auto_lines(&s).iter().any(|l| l["event"] == "outcome" && l["cauldron"] == "aa"));
    let kb1 = door.dump("a note in kb, behind the hold");
    let k = Service::key(&s);
    let st = svc.say(&format!("STATUS {k} {}", json!({})));
    let hold = &st["automatic"]["threads_hold"];
    let first = auto_lines(&s).into_iter().find(|l| l["event"] == "outcome").unwrap();
    if hold["state"] == "holding" {
        assert_eq!((hold["park"].clone(), hold["reason"].clone(), hold["set_by"]["cauldron"].clone(), hold["set_by"]["dump"].clone()), (json!("threshold"), json!("threads_parked"), json!("aa"), json!(a1.clone())), "{st}");
        assert_eq!((hold["since"].clone(), hold["next_attempt"].clone()), (first["at"].clone(), first["retry_after"].clone()), "{st}");
        let row = |c: &str| st["automatic"]["cauldrons"].as_array().unwrap().iter().find(|q| q["cauldron"] == c).cloned().unwrap();
        assert_eq!(row("aa")["held"]["by"], "threads_hold", "{st}");
        assert_eq!(row("kb")["held"]["dumps"], json!([kb1.clone()]), "{st}");
    } else {
        panic!("the hold was over before STATUS was asked (a slow machine): {st}");
    }
    // After the hold, kb goes first, though aa's dump is due again.
    let t = Instant::now();
    // Processed, and its outcome line written (it follows the put).
    while s.dump_state(&kb1)["state"] != "processed" || !auto_lines(&s).iter().any(|l| l["event"] == "outcome" && l["cauldron"] == "kb") {
        assert!(t.elapsed() < Duration::from_secs(30), "kb never processed: {:?}\n{}", auto_lines(&s), svc.log());
        std::thread::sleep(Duration::from_millis(50));
    }
    let outs: Vec<Value> = auto_lines(&s).into_iter().filter(|l| l["event"] == "outcome").collect();
    let kb_at = outs.iter().position(|l| l["cauldron"] == "kb").unwrap();
    assert_eq!(kb_at, 1, "kb was served right after the first hold, before aa probed again: {:?}\n{}", auto_lines(&s).iter().map(|l| (l["event"].clone(), l["cauldron"].clone(), l["at"].clone(), l["retry_after"].clone(), l["bound"].clone())).collect::<Vec<_>>(), svc.log());
    assert_eq!(s.dump_state_in("aa", &a1)["state"], "received");
    drop(svc);
}
