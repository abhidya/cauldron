//! `cauldron ripples` without its service: the help, the flags `run` needs,
//! a refusal that says how to start the service, and `status` read from the
//! journal (a run without its `done` line reads as interrupted).

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

const CAULDRON: &str = env!("CARGO_BIN_EXE_cauldron");

fn scratch(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "cauldron-ripples-cli-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(p.join("home")).unwrap();
    p.canonicalize().unwrap()
}

fn run(root: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let o = Command::new(CAULDRON)
        .args(args)
        .env("CAULDRON_HOME", root.join("root"))
        .env("HOME", root.join("home"))
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
fn rp1_ripples_without_its_service() {
    let s = scratch("rp1");
    assert_eq!(run(&s, &["env", "create", "dev", "--no-engine"]).0, 0);
    let (rc, out, _) = run(&s, &["ripples", "help"]);
    assert_eq!(rc, 0);
    assert!(out.contains("cauldron ripples run <DM-n> --env <env> --cauldron <name>") && out.contains("unreviewed"), "{out}");
    // `run` needs both flags.
    let (rc, _, err) = run(&s, &["ripples", "run", "DM-1", "--env", "dev"]);
    assert_ne!(rc, 0);
    assert!(err.contains("--env and --cauldron"), "{err}");
    // No service: refused, with how to start it.
    let (rc, _, err) = run(&s, &["ripples", "run", "DM-1", "--env", "dev", "--cauldron", "kb"]);
    assert_ne!(rc, 0);
    assert!(err.contains("not running") && err.contains("start ripples"), "{err}");
    // `--backend` is threads or local, checked first.
    let (rc, _, err) = run(&s, &["ripples", "run", "DM-1", "--env", "dev", "--cauldron", "kb", "--backend", "cloud"]);
    assert_ne!(rc, 0);
    assert!(err.contains("`--backend` is `threads` (the default) or `local`"), "{err}");
    let (_, out, _) = run(&s, &["ripples", "help"]);
    assert!(out.contains("--backend local runs it on this machine") && out.contains("nothing leaves the"), "{out}");
    // `eval`: its flags, checked before the service is looked for.
    let (rc, _, err) = run(&s, &["ripples", "eval", "--env", "dev", "--cauldron", "kb", "--dumps", "/nowhere", "--out", "/nowhere-out", "--backends", "threads,cloud"]);
    assert_ne!(rc, 0);
    assert!(err.contains("`--backends` is a comma-separated list"), "{err}");
    let (rc, _, err) = run(&s, &["ripples", "eval", "--env", "dev", "--cauldron", "kb", "--out", "x", "--backends", "local"]);
    assert_ne!(rc, 0);
    assert!(err.contains("needs --dumps"), "{err}");
    let fx = s.join("fx");
    std::fs::create_dir_all(&fx).unwrap();
    let fxs = fx.display().to_string();
    let outs = s.join("out").display().to_string();
    // Threads is paid: its bound is required, before anything else.
    let (rc, _, err) = run(&s, &["ripples", "eval", "--env", "dev", "--cauldron", "kb", "--dumps", &fxs, "--out", &outs, "--backends", "threads,local"]);
    assert_ne!(rc, 0);
    assert!(err.contains("--max-calls") && err.contains("Nothing was sent"), "{err}");
    let (rc, _, err) = run(&s, &["ripples", "eval", "--env", "dev", "--cauldron", "kb", "--dumps", &fxs, "--out", &outs, "--backends", "threads", "--max-calls", "0"]);
    assert_ne!(rc, 0);
    assert!(err.contains("`--max-calls` is 1 to 100"), "{err}");
    let (rc, _, err) = run(&s, &["ripples", "eval", "--env", "dev", "--cauldron", "kb", "--dumps", &fxs, "--out", &outs, "--backends", "threads,local", "--max-calls", "4", "--yes"]);
    assert_ne!(rc, 0);
    assert!(err.contains("not running") && err.contains("start ripples"), "{err}");
    // Local only needs neither.
    let (rc, _, err) = run(&s, &["ripples", "eval", "--env", "dev", "--cauldron", "kb", "--dumps", &fxs, "--out", &outs, "--backends", "local"]);
    assert_ne!(rc, 0);
    assert!(err.contains("not running"), "{err}");
    let (_, out, _) = run(&s, &["ripples", "help"]);
    assert!(out.contains("cauldron ripples eval --dumps <dir> --backends threads,local --out <dir>") && out.contains("NOTHING written"), "{out}");
    assert!(out.contains("[--max-calls <n> --yes]") && out.contains("is sent unless --yes"), "{out}");
    assert!(out.contains("Ctrl-C stops this command following it, not the evaluation"), "{out}");
    // `--force-budget` is a positive number of tokens, checked first.
    let (rc, _, err) = run(&s, &["ripples", "run", "DM-1", "--env", "dev", "--cauldron", "kb", "--force-budget", "lots"]);
    assert_ne!(rc, 0);
    assert!(err.contains("--force-budget"), "{err}");
    // Approving is the owner's act at a terminal: this test has none, so it
    // is refused before the service is looked for, with the command to run;
    // `ripples approve` is gone.
    let (rc, out, err) = run(&s, &["soup", "approve", "C-1", "C-2", "--env", "dev", "--cauldron", "kb"]);
    assert_ne!(rc, 0);
    assert!(out.is_empty(), "{out}");
    assert!(
        err.contains("REFUSED: approving a ripple's records is the owner's act at a terminal")
            && err.contains("no option skips this")
            && err.contains("cauldron soup approve C-1 C-2 --env dev --cauldron kb"),
        "{err}"
    );
    let (rc, _, err) = run(&s, &["soup", "approve", "C-1", "--env", "dev", "--cauldron", "kb", "--unapprove"]);
    assert_ne!(rc, 0);
    assert!(err.contains("cannot unapprove records by itself") && err.contains("--unapprove"), "{err}");
    let (rc, _, err) = run(&s, &["soup", "approve", "C-1", "--env", "dev"]);
    assert_ne!(rc, 0);
    assert!(err.contains("only as flags"), "{err}");
    let (rc, _, err) = run(&s, &["ripples", "approve", "C-1", "--env", "dev", "--cauldron", "kb"]);
    assert_eq!(rc, 2);
    assert!(err.contains("no verb `approve`"), "{err}");
    // The top-level help names `soup approve` and no `ripples approve`.
    let (_, _, listing) = run(&s, &["help"]);
    let line = |verb: &str| listing.lines().find(|l| l.trim_start().starts_with(&format!("{verb} "))).unwrap_or_else(|| panic!("{listing}")).to_string();
    assert!(line("soup").contains("| approve"), "{listing}");
    assert!(!line("ripples").contains("| approve") && line("ripples").contains("run | status"), "{listing}");
    let (rc, usage, _) = run(&s, &["--help"]);
    assert_eq!(rc, 0);
    assert!(usage.contains("dump-remove|approve") && !usage.contains("ripples approve"), "{usage}");
    // `status` reads the journal.
    let dir = s.join("root/data/dev/ripples");
    std::fs::create_dir_all(&dir).unwrap();
    let lines = [
        r#"{"at":"2026-01-01T00:00:00Z","run":"run-a","cauldron":"kb","dump":"DM-1","pid":1,"event":"started","model":"sonnet"}"#,
        r#"{"at":"2026-01-01T00:00:01Z","run":"run-a","cauldron":"kb","dump":"DM-1","pid":1,"event":"call","n":1,"model":"sonnet","estimated_input_tokens":5000}"#,
        r#"{"at":"2026-01-01T00:00:02Z","run":"run-a","cauldron":"kb","dump":"DM-1","pid":1,"event":"sent","thread":"t-left","message_id":3}"#,
        r#"{"at":"2026-01-01T00:01:00Z","run":"run-b","cauldron":"kb","dump":"DM-2","pid":1,"event":"started","model":"sonnet"}"#,
        r#"{"at":"2026-01-01T00:01:05Z","run":"run-b","cauldron":"kb","dump":"DM-2","pid":1,"event":"called","n":1,"input_total":9000,"charged":9100}"#,
        r#"{"at":"2026-01-01T00:01:09Z","run":"run-b","cauldron":"kb","dump":"DM-2","pid":1,"event":"done","outcome":"processed","records":["C-1"],"calls":1,"usage":{"input_tokens":9000,"output_tokens":400}}"#,
    ];
    std::fs::write(dir.join("runs.jsonl"), lines.join("\n") + "\n").unwrap();
    let (rc, out, err) = run(&s, &["ripples", "status", "--env", "dev"]);
    assert_eq!(rc, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["service"]["running"], false);
    let runs = v["runs"].as_array().unwrap();
    assert_eq!((runs[0]["run"].as_str(), runs[0]["state"].as_str(), runs[0]["last_event"].as_str()), (Some("run-a"), Some("interrupted"), Some("sent")));
    assert_eq!((runs[1]["state"].as_str(), runs[1]["outcome"].as_str(), runs[1]["usage"]["input_tokens"].as_u64()), (Some("done"), Some("processed"), Some(9000)));
    // Spend per dump across its runs, and the call the kill left in flight.
    let spend = &v["spend"];
    assert_eq!((spend["per_dump"][0]["dump"].as_str(), spend["per_dump"][0]["charged_input_tokens"].as_u64()), (Some("DM-2"), Some(9100)), "{spend}");
    assert_eq!(spend["possibly_running"][0]["thread"].as_str(), Some("t-left"), "{spend}");
    // The call the kill cut short is charged its estimate, and shown as unsettled.
    let a = &spend["per_dump"][1];
    assert_eq!(
        (a["dump"].as_str(), a["calls"].as_u64(), a["charged_input_tokens"].as_u64(), a["unsettled_calls"].as_u64()),
        (Some("DM-1"), Some(1), Some(5000), Some(1)),
        "{spend}"
    );
    assert_eq!(spend["per_dump"][0]["unsettled_calls"].as_u64(), Some(0), "{spend}");
    assert_eq!(spend["caps"]["input_tokens_per_cauldron_per_day"].as_u64(), Some(1_500_000));
    let _ = std::fs::remove_dir_all(&s);
}
