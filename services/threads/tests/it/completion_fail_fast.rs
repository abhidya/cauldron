//! **A bare completion against a `claude` that refuses like the real one**
//! (`fixtures/claude-cli-check`, sourced by `fake-claude`).
//!
//! 1. A schema that says it is JSON Schema 2020-12 (`$schema`) reaches the
//!    child without that key: the real CLI refuses the 2020-12 meta-schema
//!    before it starts, so a completion carrying it never ran at all.
//! 2. A completion whose `claude` exits without an answer FAILS AT ONCE: one
//!    spawn, no `retry_scheduled`, `turn_failed` whose message carries the
//!    end of the child's stderr with the home directory and the working
//!    directory named by label, control characters removed, and the
//!    prompt kept on the queue as `failed`.
//! 3. Opposed: the same failure on a thread that is not a completion is
//!    re-delivered, as before.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use threads::build_manager;
use threads::config::Config;
use threads::settings::{Completion, ThreadSettings};
use threads::ThreadManager;

fn fake_claude() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude"))
}

/// The binary the daemon spawns: count the spawn, keep its argv, and then
/// either fail as a refused command line does (a prompt holding
/// `stderr-then-exit`: two stderr lines naming the home and working
/// directories, one with an ANSI colour, exit 1, nothing on stdout) or become
/// `fake-claude` with the same argv and stdin.
fn write_wrapper(tmp: &Path) -> PathBuf {
    let w = tmp.join("claude-wrapper");
    let script = format!(
        r#"#!/bin/bash
IN="$(/bin/cat)"
echo spawn >> '{spawns}'
: > '{argv}'
for a in "$@"; do printf '%s\n' "$a" >> '{argv}'; done
if [[ "$IN" == *stderr-then-exit* ]]; then
  printf '\033[31mClaude configuration file not found at: %s/.claude-x/.claude.json\033[0m\n' "$HOME" >&2
  printf 'Error: --json-schema is not a valid JSON Schema (seen from %s)\n' "$PWD" >&2
  exit 1
fi
printf '%s' "$IN" | exec '{fake}' "$@"
"#,
        spawns = tmp.join("spawns").display(),
        argv = tmp.join("argv").display(),
        fake = fake_claude().display(),
    );
    std::fs::write(&w, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&w, std::fs::Permissions::from_mode(0o755)).unwrap();
    w
}

fn spawns(tmp: &Path) -> usize {
    std::fs::read_to_string(tmp.join("spawns")).map(|s| s.lines().count()).unwrap_or(0)
}

fn events(m: &ThreadManager, id: &str) -> Vec<Value> {
    m.store
        .list_events(id, 0, 100_000, None)
        .unwrap()
        .iter()
        .map(|e| serde_json::to_value(&e.event).unwrap())
        .collect()
}

fn kinds(m: &ThreadManager, id: &str) -> Vec<String> {
    events(m, id).iter().map(|e| e["kind"].as_str().unwrap_or("").to_string()).collect()
}

async fn eventually(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = std::time::Instant::now() + Duration::from_secs(secs) + first_exec::FIRST_EXEC_BUDGET;
    while std::time::Instant::now() < end {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

fn thread(m: &ThreadManager, work: &Path, completion: bool) -> String {
    std::fs::create_dir_all(work).unwrap();
    let mut s = ThreadSettings::new(work.to_string_lossy(), "sonnet");
    if completion {
        s.completion = Some(Completion { max_output_tokens: 4000 });
    }
    s.account = serde_json::from_value(json!({"named": "aa-one"})).unwrap();
    m.create_thread(None, s).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_completion_loads_its_schema_and_fails_at_once_with_the_stderr_tail() {
    let tmp = tempfile::tempdir().unwrap();
    let tmp = tmp.path();
    std::env::set_var("CAULDRON_HOME", tmp.join("cauldron"));
    std::env::set_var("THREADS_CLAUDE_BIN", write_wrapper(tmp));
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.join("claude-home"));
    std::env::set_var("FAKE_STATE_DIR", tmp);
    threads::clock::testing::ticking();

    let login = tmp.join("aa-login");
    std::fs::create_dir_all(&login).unwrap();
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert("aa-one".to_string(), login.to_string_lossy().to_string());
    let m = build_manager(Config {
        data_dir: Some(tmp.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        probe_interval_busy_secs: 0,
        ..Default::default()
    })
    .unwrap();
    let (_addr, h) = threads::chassis::serve(Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap()).await.unwrap();
    std::mem::forget(h);

    // ── 1. The 2020-12 `$schema` never reaches the child ──
    let id = thread(&m, &tmp.join("work-schema"), true);
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "description": "fake:valid",
        "type": "object",
        "properties": {"verdict": {"enum": ["merge", "block"]}},
        "required": ["verdict"]
    });
    let meta = json!({"cauldron.threads": {"json_schema": schema, "schema_retries": 0}});
    m.send(&id, "decide".into(), threads::SendIntent::Immediate, meta, true).unwrap();
    let judged = |m: &ThreadManager| {
        events(m, &id).into_iter().find(|e| e["kind"] == "custom" && e["event_type"] == "cauldron.threads/structured_output").map(|e| e["data"].clone())
    };
    assert!(eventually(20, || judged(&m).is_some() || kinds(&m, &id).iter().any(|k| k == "turn_failed")).await, "{:?}", kinds(&m, &id));
    let argv: Vec<String> = std::fs::read_to_string(tmp.join("argv")).unwrap().lines().map(str::to_string).collect();
    let sent = argv.windows(2).find(|w| w[0] == "--json-schema").map(|w| w[1].clone()).expect("a --json-schema");
    let sent: Value = serde_json::from_str(&sent).unwrap();
    assert!(sent.get("$schema").is_none(), "the CLI cannot load the 2020-12 meta-schema: {sent}");
    assert_eq!(sent["required"], json!(["verdict"]), "the rest of the schema rides unchanged");
    let d = judged(&m).unwrap_or_else(|| panic!("no judged answer: {:?}", events(&m, &id)));
    assert_eq!(d["status"], "ok", "{d}");
    assert_eq!(spawns(tmp), 1);

    // ── 2. A completion whose claude exits without an answer fails at once ──
    let work = tmp.join("work-fail");
    let id = thread(&m, &work, true);
    m.send(&id, "stderr-then-exit".into(), threads::SendIntent::Immediate, Value::Null, true).unwrap();
    assert!(eventually(20, || kinds(&m, &id).iter().any(|k| k == "turn_failed")).await, "no turn_failed: {:?}", kinds(&m, &id));
    // Long enough for any re-delivery the ladder would have scheduled.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(spawns(tmp), 2, "one spawn for this completion, never a second");
    let k = kinds(&m, &id);
    assert!(!k.iter().any(|k| k == "retry_scheduled" || k == "delivery_resumed"), "{k:?}");
    let failed = events(&m, &id).into_iter().find(|e| e["kind"] == "turn_failed").unwrap();
    let msg = failed["message"].as_str().unwrap();
    assert!(msg.contains("a completion is not re-delivered"), "{msg}");
    assert!(msg.contains("Error: --json-schema is not a valid JSON Schema"), "the stderr's last line: {msg}");
    assert!(msg.contains("~/.claude-x/.claude.json"), "the home directory by its label: {msg}");
    assert!(msg.contains("(seen from <cwd>)"), "the working directory by its label: {msg}");
    let home = std::env::var("HOME").unwrap();
    assert!(!msg.contains(&home) && !msg.contains(&*work.to_string_lossy()), "{msg}");
    assert!(!msg.contains('\u{1b}'), "no control characters: {msg:?}");
    assert!(msg.chars().count() <= 200 + threads::retry::STDERR_TAIL_CHARS, "{msg}");
    let rows = m.store.failed_deliveries(&id).unwrap();
    assert_eq!(rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), vec!["stderr-then-exit"], "the prompt is kept as failed");
    assert_ne!(m.thread_state(&id).unwrap().state, "retrying");

    // ── 3. Opposed: an agent thread with the same failure is re-delivered ──
    let id = thread(&m, &tmp.join("work-agent"), false);
    m.send(&id, "stderr-then-exit".into(), threads::SendIntent::Immediate, Value::Null, true).unwrap();
    assert!(eventually(20, || kinds(&m, &id).iter().any(|k| k == "retry_scheduled")).await, "{:?}", kinds(&m, &id));
    assert!(!kinds(&m, &id).iter().any(|k| k == "turn_failed"), "an agent turn keeps its ladder");
    let _ = m.cancel(&id);
}

// Fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
