//! `cauldron ripples` without its service: the help, the flags `run` needs,
//! a refusal that says how to start the service, and `status` read from the
//! journal (a run without its `done` line reads as interrupted).

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

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

/// The verb on a pseudo-terminal: `script -q /dev/null cauldron <args>`
/// (the in-test way to give a command a terminal). Everything it prints
/// arrives on stdout.
fn on_tty(root: &std::path::Path, args: &[&str]) -> (i32, String) {
    on_tty_answering(root, args, "y")
}

/// `args` at a terminal (`script(1)`), answering its question with `answer`.
fn on_tty_answering(root: &std::path::Path, args: &[&str], answer: &str) -> (i32, String) {
    use std::io::Write;
    let mut full = vec!["-q", "/dev/null", CAULDRON];
    full.extend_from_slice(args);
    let mut child = Command::new("/usr/bin/script")
        .args(&full)
        .env("CAULDRON_HOME", root.join("root"))
        .env("HOME", root.join("home"))
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // The answer goes in once the question is on the terminal (sent
    // earlier, the terminal may take the end of input first).
    let out = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let reader = {
        let (out, mut stdout) = (out.clone(), child.stdout.take().unwrap());
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 {
                    break;
                }
                out.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        })
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut stdin = child.stdin.take().unwrap();
    loop {
        let asked = String::from_utf8_lossy(&out.lock().unwrap()).contains("[y/N]");
        if asked || child.try_wait().unwrap().is_some() || std::time::Instant::now() > deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = stdin.write_all(format!("{answer}\n").as_bytes());
    let status = child.wait().unwrap();
    drop(stdin);
    reader.join().unwrap();
    let text = String::from_utf8_lossy(&out.lock().unwrap()).replace("\r\n", "\n");
    (status.code().unwrap_or(-1), text)
}

/// **`ripples on | off`, and `status` with the service stopped.** `on`
/// needs a terminal and names the command to run otherwise; at one, it
/// stores the setting owner-only with the `since` mark at the newest dump
/// and says what is sent where and the caps; `off` works from anywhere;
/// `status` shows the setting and the queue.
#[test]
fn rp2_on_off_and_the_queue() {
    use std::os::unix::fs::PermissionsExt;
    let s = scratch("rp2");
    assert_eq!(run(&s, &["env", "create", "dev", "--no-engine"]).0, 0);
    assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).0, 0, "create");
    let note = s.join("note.txt");
    std::fs::write(&note, "a note from before it was turned on").unwrap();
    let (rc, out, err) = run(&s, &["soup", "dump", note.to_str().unwrap(), "--env", "dev", "--cauldron", "kb"]);
    assert_eq!(rc, 0, "{out}{err}");
    // No terminal: refused, with the exact command, nothing written.
    let (rc, out, err) = run(&s, &["ripples", "on", "kb", "--env", "dev", "--include-sessions"]);
    assert_ne!(rc, 0);
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("owner's act at a terminal") && err.contains("no option skips this") && err.contains("cauldron ripples on kb --env dev --include-sessions"), "{err}");
    let setting = s.join("root/data/dev/ripples/auto/kb.json");
    assert!(!setting.exists());
    // Checked before the terminal.
    let (rc, _, err) = run(&s, &["ripples", "on", "kb", "--env", "dev", "--no-scrub-local"]);
    assert_ne!(rc, 0);
    assert!(err.contains("`--no-scrub-local` goes with `--backend local`"), "{err}");
    let (rc, _, err) = run(&s, &["ripples", "on", "nope", "--env", "dev"]);
    assert_ne!(rc, 0, "{err}");
    // At a terminal, answered no: what it means is shown, nothing written.
    let (rc, out) = on_tty_answering(&s, &["ripples", "on", "kb", "--env", "dev"], "n");
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("What is sent where") && out.contains("[y/N]") && out.contains("nothing was changed"), "{out}");
    assert!(!setting.exists());
    // Answered yes: on, owner-only, since the newest dump; what it means
    // shown before the question.
    let (rc, out) = on_tty(&s, &["ripples", "on", "kb", "--env", "dev"]);
    assert!(out.find("What is sent where").unwrap() < out.find("[y/N]").unwrap(), "{out}");
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("Automatic processing is ON for cauldron `kb`") && out.contains("after DM-1"), "{out}");
    assert!(out.contains("What is sent where") && out.contains("`threads` service") && out.contains("scrubbed") && out.contains("not a guarantee"), "{out}");
    assert!(out.contains("3 calls and 200000 input tokens") && out.contains("1500000 input tokens or 30 dumps"), "{out}");
    assert!(out.contains("cauldron ripples off kb --env dev") && out.contains("session dumps are not taken"), "{out}");
    assert!(out.contains("not running") && out.contains("start ripples"), "{out}");
    assert_eq!(std::fs::metadata(&setting).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::metadata(setting.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&setting).unwrap()).unwrap();
    assert_eq!((v["on"].clone(), v["since_dump"].clone(), v["backend"].clone(), v["include_sessions"].clone()), (json!(true), json!(1), json!("threads"), json!(false)));
    // A dump after it: waiting, with the service stopped.
    std::fs::write(&note, "a note from after it was turned on").unwrap();
    assert_eq!(run(&s, &["soup", "dump", note.to_str().unwrap(), "--env", "dev", "--cauldron", "kb"]).0, 0);
    let (rc, out, err) = run(&s, &["ripples", "status", "--env", "dev", "--cauldron", "kb"]);
    assert_eq!(rc, 0, "{err}");
    let st: Value = serde_json::from_str(&out).unwrap();
    let a = &st["automatic"]["cauldrons"][0];
    assert_eq!((a["automatic"].clone(), a["waiting"].clone(), a["since"]["after_dump"].clone()), (json!("on"), json!(["DM-2"]), json!("DM-1")), "{st}");
    // On again (another backend option): the `since` mark is kept.
    let (rc, out) = on_tty(&s, &["ripples", "on", "kb", "--env", "dev", "--include-sessions"]);
    assert_eq!(rc, 0, "{out}");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&setting).unwrap()).unwrap();
    assert_eq!((v["since_dump"].clone(), v["include_sessions"].clone()), (json!(1), json!(true)));
    // Off, from anywhere (no terminal).
    let (rc, out, err) = run(&s, &["ripples", "off", "kb", "--env", "dev"]);
    assert_eq!(rc, 0, "{err}");
    assert!(out.contains("\"outcome\": \"off\""), "{out}");
    let (rc, out, _) = run(&s, &["ripples", "off", "kb", "--env", "dev"]);
    assert_eq!(rc, 0);
    assert!(out.contains("already_off"), "{out}");
    let (_, out, _) = run(&s, &["ripples", "status", "--env", "dev"]);
    let st: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(st["automatic"]["cauldrons"][0]["automatic"], "off", "{st}");
    // Turned on again: only dumps after that moment.
    let (rc, _) = on_tty(&s, &["ripples", "on", "kb", "--env", "dev"]);
    assert_eq!(rc, 0);
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&setting).unwrap()).unwrap();
    assert_eq!(v["since_dump"], 2);
    let (_, out, _) = run(&s, &["ripples", "help"]);
    assert!(out.contains("cauldron ripples on <cauldron> --env <env>") && out.contains("cauldron ripples off <cauldron> --env <env>") && out.contains("an upgrade never"), "{out}");
    let _ = std::fs::remove_dir_all(&s);
}

/// **"On" belongs to one cauldron, never to a name.** `cauldron remove`
/// turns it off and says so, and a cauldron made again under the name
/// starts off; `cauldron rename` turns it off and prints the `on` command
/// for the new name; renaming back does not turn it on; a setting left
/// behind or copied (to another cauldron's name, or into another
/// environment) is void: `status` says the setting belonged to a cauldron
/// that is gone and how to clear it, and a dump's receipt says it is not
/// taken.
#[test]
fn rp3_on_follows_no_remove_rename_or_copy() {
    let s = scratch("rp3");
    let auto = |env: &str, name: &str| s.join(format!("root/data/{env}/ripples/auto/{name}.json"));
    let status = |cauldron: &str| -> Value {
        let (rc, out, err) = run(&s, &["ripples", "status", "--env", "dev", "--cauldron", cauldron]);
        assert_eq!(rc, 0, "{err}");
        serde_json::from_str(&out).unwrap()
    };
    let dump = |cauldron: &str| -> Value {
        let note = s.join("note.txt");
        std::fs::write(&note, "a note").unwrap();
        let (rc, out, err) = run(&s, &["soup", "dump", note.to_str().unwrap(), "--env", "dev", "--cauldron", cauldron]);
        assert_eq!(rc, 0, "{out}{err}");
        serde_json::from_str(&out).unwrap()
    };
    assert_eq!(run(&s, &["env", "create", "dev", "--no-engine"]).0, 0);
    assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).0, 0);
    assert_eq!(on_tty(&s, &["ripples", "on", "kb", "--env", "dev", "--include-sessions"]).0, 0);
    let saved = std::fs::read_to_string(auto("dev", "kb")).unwrap();
    assert_eq!(dump("kb")["automatic"]["takes_this_dump"], true);
    // Remove: off, and said (in the dry run too).
    let (rc, out, _) = run(&s, &["remove", "kb", "--env", "dev"]);
    assert_eq!(rc, 2);
    assert!(out.contains("automatic processing turned off for `kb`"), "{out}");
    let (rc, out, err) = run(&s, &["remove", "kb", "--env", "dev", "--yes"]);
    assert_eq!(rc, 0, "{err}");
    assert!(out.contains("automatic ripples processing was on for `kb`; it is now OFF"), "{out}");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(auto("dev", "kb")).unwrap()).unwrap();
    assert_eq!(v["on"], false);
    // Made again: off, and its dumps are not taken.
    assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).0, 0);
    assert_eq!(dump("kb")["automatic"]["on"], false);
    assert_eq!(status("kb")["automatic"]["cauldrons"][0]["automatic"], "off");
    // A setting left behind "on" (written back by hand): void, not taken.
    std::fs::write(auto("dev", "kb"), &saved).unwrap();
    let r = dump("kb");
    assert_eq!((r["automatic"]["on"].clone(), r["automatic"]["takes_this_dump"].clone()), (json!(false), json!(false)), "{r}");
    let q = &status("kb")["automatic"]["cauldrons"][0];
    assert_eq!(q["automatic"], "void", "{q}");
    assert!(q["why"].as_str().unwrap().contains("which is gone") && q["what_to_do"].as_str().unwrap().contains("cauldron ripples off kb --env dev"), "{q}");
    // Cleared with `off`, from anywhere.
    let (rc, out, _) = run(&s, &["ripples", "off", "kb", "--env", "dev"]);
    assert_eq!(rc, 0);
    assert!(out.contains("\"outcome\": \"off\""), "{out}");
    assert_eq!(status("kb")["automatic"]["cauldrons"][0]["automatic"], "off");
    // Rename: off, with the command for the new name.
    assert_eq!(on_tty(&s, &["ripples", "on", "kb", "--env", "dev", "--include-sessions"]).0, 0);
    let (rc, out, err) = run(&s, &["rename", "kb", "kb2", "--env", "dev"]);
    assert_eq!(rc, 0, "{err}");
    assert!(out.contains("it is now OFF") && out.contains("cauldron ripples on kb2 --env dev --include-sessions"), "{out}");
    assert_eq!(dump("kb2")["automatic"]["on"], false);
    assert!(status("kb2")["automatic"]["cauldrons"].as_array().unwrap().is_empty());
    // Renamed back: still off.
    let (rc, out, err) = run(&s, &["rename", "kb2", "kb", "--env", "dev"]);
    assert_eq!(rc, 0, "{err}");
    assert!(!out.contains("now OFF"), "nothing was on: {out}");
    assert_eq!(dump("kb")["automatic"]["on"], false);
    assert_eq!(status("kb")["automatic"]["cauldrons"][0]["automatic"], "off");
    // Copied to another cauldron's name: void there.
    assert_eq!(on_tty(&s, &["ripples", "on", "kb", "--env", "dev"]).0, 0);
    assert_eq!(run(&s, &["create", "other", "--env", "dev"]).0, 0);
    std::fs::copy(auto("dev", "kb"), auto("dev", "other")).unwrap();
    assert_eq!(dump("other")["automatic"]["takes_this_dump"], false);
    assert_eq!(status("other")["automatic"]["cauldrons"][0]["automatic"], "void");
    assert_eq!(dump("kb")["automatic"]["takes_this_dump"], true, "kb's own setting still holds");
    // Copied into another environment's cauldron of the same name: void.
    assert_eq!(run(&s, &["env", "create", "prod", "--no-engine"]).0, 0);
    assert_eq!(run(&s, &["create", "kb", "--env", "prod"]).0, 0);
    std::fs::create_dir_all(auto("prod", "kb").parent().unwrap()).unwrap();
    std::fs::copy(auto("dev", "kb"), auto("prod", "kb")).unwrap();
    let note = s.join("note.txt");
    let (rc, out, err) = run(&s, &["soup", "dump", note.to_str().unwrap(), "--env", "prod", "--cauldron", "kb"]);
    assert_eq!(rc, 0, "{err}");
    let r: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(r["automatic"]["takes_this_dump"], false, "{r}");
    assert!(r["automatic"]["void"].as_str().unwrap().contains("environment `dev`"), "{r}");
    let _ = std::fs::remove_dir_all(&s);
}
