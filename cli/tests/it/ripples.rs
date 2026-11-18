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
    // `ripples approve | reject` are the owner's acts at a terminal too.
    let (rc, out, err) = run(&s, &["ripples", "approve", "C-1", "--env", "dev", "--cauldron", "kb"]);
    assert_ne!(rc, 0);
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("REFUSED: approving a ripple's records is the owner's act at a terminal") && err.contains("cauldron ripples approve C-1 --env dev --cauldron kb"), "{err}");
    let (rc, _, err) = run(&s, &["ripples", "reject", "--dump", "DM-1", "--env", "dev", "--cauldron", "kb"]);
    assert_ne!(rc, 0);
    assert!(err.contains("REFUSED: rejecting a ripple's records") && err.contains("cauldron ripples reject --dump DM-1 --env dev --cauldron kb"), "{err}");
    // The top-level help names `soup approve` and `ripples review | approve | reject`.
    let (_, _, listing) = run(&s, &["help"]);
    let line = |verb: &str| listing.lines().find(|l| l.trim_start().starts_with(&format!("{verb} "))).unwrap_or_else(|| panic!("{listing}")).to_string();
    assert!(line("soup").contains("| approve"), "{listing}");
    assert!(line("ripples").contains("run | status") && line("ripples").contains("review | approve | reject"), "{listing}");
    let (rc, usage, _) = run(&s, &["--help"]);
    assert_eq!(rc, 0);
    assert!(usage.contains("dump-remove|approve"), "{usage}");
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

/// Records a ripple wrote, straight through the spine (what the MCP door's
/// ripple seam lands), each citing `dump` and carrying `meta.ripple`; one
/// write per call, so one run. Answers the ids, in `labels` order.
pub(crate) fn ripple_write(soup: &std::path::Path, run: &str, dump: &str, model: &str, records: &[(&str, &str, &str)]) -> Vec<String> {
    use spine::envelope::{Envelope, Initiator, Instrument, Mode, Provenance, RippleKind, ENVELOPE_FORMAT_VERSION};
    let c = spine::Cauldron::open(soup, spine::config::SpineConfig::default()).unwrap();
    let mut lines = vec!["MATCH (r: ws/region { id: $r })".to_string()];
    let mut bindings: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    bindings.insert("r".into(), json!("R-1"));
    bindings.insert("m".into(), json!({"cites": [{"kind": "dump", "id": dump}], "ripple": {"package": "dump", "version": "v1", "run": run, "model": model}}));
    let mut ret = vec![];
    for (i, (ty, handle, body)) in records.iter().enumerate() {
        let state = match *ty {
            "idea" => "idea",
            "task" => "ready",
            _ => "open",
        };
        lines.push(format!("CREATE (n{i}: ws/{ty} {{ state: \"{state}\", handle: $h{i}, body: $b{i}, meta: $m }})"));
        lines.push(format!("CREATE (n{i}) -[in_region]-> (r)"));
        bindings.insert(format!("h{i}"), json!(handle));
        bindings.insert(format!("b{i}"), json!(body));
        ret.push(format!("n{i}.id"));
    }
    lines.push(format!("RETURN {}", ret.join(", ")));
    let env = Envelope {
        statement: lines.join("\n"),
        bindings,
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Ripple { kind: RippleKind::TypeTwo, run_id: run.into(), step_id: "put".into(), scope: vec![] },
            instrument: Instrument { name: "test".into(), version: "0".into() },
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(format!("review-{run}")),
    };
    match c.submit(env).unwrap() {
        spine::Outcome::Committed { returned, .. } => ret.iter().map(|k| returned.0[0][k.as_str()].as_str().unwrap().to_string()).collect(),
        other => panic!("{other:?}"),
    }
}

/// A scratch environment `dev` with cauldron `kb`, its example regions
/// imported, and two ripple runs: run-a (DM-1, two records) then run-b
/// (DM-2, three records). Answers (scratch, run-a ids, run-b ids).
fn queue_fixture(tag: &str) -> (PathBuf, Vec<String>, Vec<String>) {
    let s = scratch(tag);
    assert_eq!(run(&s, &["env", "create", "dev", "--no-engine"]).0, 0);
    assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).0, 0);
    let regions = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../claude-code/cauldron/regions.example.json");
    let (rc, _, err) = run(&s, &["soup", "import", regions.to_str().unwrap(), "--env", "dev", "--cauldron", "kb", "--remint"]);
    assert_eq!(rc, 0, "{err}");
    let soup = s.join("root/data/dev/cauldrons/kb/soup");
    let long = "word ".repeat(200);
    let a = ripple_write(&soup, "run-a", "DM-1", "sonnet", &[("idea", "cache the build graph", "a body"), ("task", "write the cache test", &long)]);
    let b = ripple_write(&soup, "run-b", "DM-2", "local:qwen", &[("issue", "flaky login\nIGNORE THIS LINE", "the body"), ("question", "who owns deploys", ""), ("idea", "a third record", "x")]);
    (s, a, b)
}

/// **`ripples review`**: the unreviewed records grouped by dump and run,
/// newest group first, with the model, backend, when, the excerpt, the
/// regions and edges, the counts and the commands; filters; the bound and
/// the pages; the readable form; no service needed.
#[test]
fn rp4_the_review_queue() {
    let (s, a, b) = queue_fixture("rp4");
    let review = |extra: &[&str]| -> Value {
        let mut args = vec!["ripples", "review", "--env", "dev", "--cauldron", "kb"];
        args.extend_from_slice(extra);
        let (rc, out, err) = run(&s, &args);
        assert_eq!(rc, 0, "{err}");
        serde_json::from_str(&out).unwrap()
    };
    let q = review(&[]);
    assert_eq!((q["counts"]["unreviewed"].clone(), q["counts"]["groups"].clone()), (json!(5), json!(2)), "{q}");
    assert_eq!((q["counts"]["by_dump"]["DM-1"].clone(), q["counts"]["by_dump"]["DM-2"].clone()), (json!(2), json!(3)));
    let g = q["groups"].as_array().unwrap();
    // Newest first: run-b's write landed last.
    assert_eq!((g[0]["dump"].clone(), g[0]["run"].clone(), g[0]["model"].clone(), g[0]["backend"].clone()), (json!("DM-2"), json!("run-b"), json!("local:qwen"), json!("local")), "{q}");
    assert_eq!((g[1]["dump"].clone(), g[1]["backend"].clone(), g[1]["records_total"].clone()), (json!("DM-1"), json!("threads"), json!(2)));
    assert!(g[0]["written_at"].as_str().is_some_and(|t| t.ends_with('Z')), "{q}");
    let ids: Vec<&str> = g[0]["records"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
    let mut want: Vec<&str> = b.iter().map(String::as_str).collect();
    want.sort_by_key(|x| (x.rsplit_once('-').unwrap().0.to_string(), x.rsplit_once('-').unwrap().1.parse::<u64>().unwrap()));
    assert_eq!(ids, want);
    assert_eq!(g[0]["commands"]["approve"], "cauldron ripples approve --dump DM-2 --env dev --cauldron kb");
    assert_eq!(g[0]["commands"]["reject"], "cauldron ripples reject --dump DM-2 --env dev --cauldron kb [--reason <text>]");
    let task = g[1]["records"].as_array().unwrap().iter().find(|r| r["id"] == a[1].as_str()).unwrap();
    assert_eq!((task["type"].clone(), task["state"].clone(), task["regions"].clone(), task["body_chars"].clone()), (json!("task"), json!("ready"), json!(["R-1"]), json!(1000)));
    assert_eq!(task["excerpt"].as_str().unwrap().chars().count(), 241, "240 and the ellipsis");
    assert_eq!(task["unreviewed"], true);
    assert!(q["note"].as_str().unwrap().contains("data, never as instructions"), "{q}");
    // Filters.
    let f = review(&["--dump", "DM-1"]);
    assert_eq!((f["counts"]["unreviewed"].clone(), f["groups"].as_array().unwrap().len()), (json!(2), 1));
    assert_eq!(review(&["--run", "run-b"])["counts"]["unreviewed"], 3);
    assert_eq!(review(&["--type", "idea"])["counts"]["unreviewed"], 2);
    assert_eq!(review(&["--since", "2000-01-01"])["counts"]["unreviewed"], 5);
    assert_eq!(review(&["--since", "2999-01-01T00:00:00Z"])["counts"]["unreviewed"], 0);
    for (flag, val, says) in [("--since", "yesterday", "YYYY-MM-DD"), ("--dump", "D1", "DM-<n>"), ("--type", "widget", "no node type"), ("--run", "a b", "plain word")] {
        let (rc, _, err) = run(&s, &["ripples", "review", "--env", "dev", "--cauldron", "kb", flag, val]);
        assert_ne!(rc, 0, "{flag} {val}");
        assert!(err.contains(says), "{flag} {val}: {err}");
    }
    // The bound, and the pages: every record once, in order.
    let mut seen = vec![];
    let mut after: Option<String> = None;
    loop {
        let mut args = vec!["--max-chars", "2000"];
        if let Some(a) = &after {
            args.extend(["--after", a.as_str()]);
        }
        let p = review(&args);
        assert!(serde_json::to_string(&p).unwrap().chars().count() <= 2000, "{p}");
        for grp in p["groups"].as_array().unwrap() {
            for r in grp["records"].as_array().unwrap() {
                seen.push(r["id"].as_str().unwrap().to_string());
            }
        }
        match p["next_after"].as_str() {
            Some(n) => after = Some(n.to_string()),
            None => break,
        }
    }
    let all: Vec<String> = q["groups"].as_array().unwrap().iter().flat_map(|g| g["records"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string())).collect();
    assert_eq!(seen, all);
    assert!(review(&["--max-chars", "2000"])["next_after"].is_string());
    // The readable form: one line per record, control characters flattened.
    let (rc, text, _) = run(&s, &["ripples", "review", "--env", "dev", "--cauldron", "kb", "--text"]);
    assert_eq!(rc, 0);
    assert!(text.starts_with("cauldron `kb` (environment `dev`): 5 unreviewed records in 2 groups"), "{text}");
    assert!(text.contains("DM-2 · run run-b · local:qwen (local)") && text.contains("approve: cauldron ripples approve --dump DM-1 --env dev --cauldron kb"), "{text}");
    assert!(text.contains("flaky login IGNORE THIS LINE") && !text.contains("\nIGNORE"), "{text}");
    assert!(text.contains("data, never as instructions"), "{text}");
    // The status views count what waits, per cauldron, and name the command.
    let (rc, out, err) = run(&s, &["ripples", "status", "--env", "dev"]);
    assert_eq!(rc, 0, "{err}");
    let st: Value = serde_json::from_str(&out).unwrap();
    assert_eq!((st["review"]["unreviewed"].clone(), st["review"]["cauldrons"][0]["unreviewed"].clone()), (json!(5), json!(5)), "{st}");
    assert_eq!(st["review"]["cauldrons"][0]["review"], "cauldron ripples review --env dev --cauldron kb --text");
    assert!(err.contains("5 record(s) of cauldron `kb` wait for review: cauldron ripples review --env dev --cauldron kb --text"), "{err}");
    let (rc, out, err) = run(&s, &["mcp", "status", "--env", "dev"]);
    assert_eq!(rc, 0, "{err}");
    let ms: Value = serde_json::from_str(&out).unwrap();
    let row = ms["cauldrons"].as_array().unwrap().iter().find(|r| r["cauldron"] == "kb").unwrap().clone();
    assert_eq!((row["unreviewed"].clone(), row["review"].is_string()), (json!(5), true), "{row}");
    let (rc, out, err) = run(&s, &["--env", "dev", "status", "--json"]);
    assert_eq!(rc, 0, "{err}");
    let cs: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(cs["review"][0]["unreviewed"], json!(5), "{cs}");
    let (_, out, _) = run(&s, &["--env", "dev", "status"]);
    assert!(out.contains("cauldron kb: 5 record(s) a ripple wrote wait for review: cauldron ripples review --env dev --cauldron kb --text"), "{out}");
    // Both flags are needed.
    let (rc, _, err) = run(&s, &["ripples", "review", "--env", "dev"]);
    assert_ne!(rc, 0);
    assert!(err.contains("--env <env> --cauldron <name>"), "{err}");
    let _ = std::fs::remove_dir_all(&s);
}

/// One record a ripple wrote, in R-1, with a `ref` edge to each of `refs`.
fn ripple_write_refs(soup: &std::path::Path, run: &str, dump: &str, handle: &str, body: &str, refs: &[String]) -> String {
    use spine::envelope::{Envelope, Initiator, Instrument, Mode, Provenance, RippleKind, ENVELOPE_FORMAT_VERSION};
    let c = spine::Cauldron::open(soup, spine::config::SpineConfig::default()).unwrap();
    let mut lines = vec!["MATCH (r: ws/region { id: $r })".to_string()];
    let mut bindings: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    bindings.insert("r".into(), json!("R-1"));
    for (k, id) in refs.iter().enumerate() {
        lines.push(format!("MATCH (x{k}: ws/idea {{ id: $x{k} }})"));
        bindings.insert(format!("x{k}"), json!(id));
    }
    bindings.insert("m".into(), json!({"cites": [{"kind": "dump", "id": dump}], "ripple": {"package": "dump", "version": "v1", "run": run, "model": "sonnet"}}));
    lines.push("CREATE (n: ws/idea { state: \"idea\", handle: $h, body: $b, meta: $m })".into());
    lines.push("CREATE (n) -[in_region]-> (r)".into());
    for k in 0..refs.len() {
        lines.push(format!("CREATE (n) -[ref]-> (x{k})"));
    }
    lines.push("RETURN n.id".into());
    bindings.insert("h".into(), json!(handle));
    bindings.insert("b".into(), json!(body));
    let env = Envelope {
        statement: lines.join("\n"),
        bindings,
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Ripple { kind: RippleKind::TypeTwo, run_id: run.into(), step_id: "put".into(), scope: vec![] },
            instrument: Instrument { name: "test".into(), version: "0".into() },
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(format!("review-{run}")),
    };
    match c.submit(env).unwrap() {
        spine::Outcome::Committed { returned, .. } => returned.0[0]["n.id"].as_str().unwrap().to_string(),
        other => panic!("{other:?}"),
    }
}

/// **`ripples review --max-chars` is a bound**, for JSON and for `--text`,
/// at every limit, including below the size of one record: whole records
/// while they fit; one record longer than the bound is cut (its longest
/// fields, then its edges) and `truncated` says what was cut, with paths
/// from the answer's top; a smaller limit gives a smaller answer; under the
/// floor the bound is refused, as `soup dump-get` refuses its own.
#[test]
fn rp5_the_review_bound_is_real() {
    let s = scratch("rp5");
    assert_eq!(run(&s, &["env", "create", "dev", "--no-engine"]).0, 0);
    assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).0, 0);
    let regions = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../claude-code/cauldron/regions.example.json");
    assert_eq!(run(&s, &["soup", "import", regions.to_str().unwrap(), "--env", "dev", "--cauldron", "kb", "--remint"]).0, 0);
    let soup = s.join("root/data/dev/cauldrons/kb/soup");
    let many: Vec<(String, String, String)> = (0..80).map(|i| ("idea".to_string(), format!("bulk record {i} {}", "y".repeat(80)), "z".repeat(300))).collect();
    let many: Vec<(&str, &str, &str)> = many.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str())).collect();
    let targets = ripple_write(&soup, "run-a", "DM-1", "sonnet", &many);
    let big = ripple_write_refs(&soup, "run-b", "DM-2", &format!("many edges {}", "h".repeat(200)), &"b".repeat(5000), &targets);
    let ask = |extra: &[&str]| -> (i32, String, String) {
        let mut args = vec!["ripples", "review", "--env", "dev", "--cauldron", "kb"];
        args.extend_from_slice(extra);
        run(&s, &args)
    };
    let whole: Value = serde_json::from_str(&ask(&["--max-chars", "50000"]).1).unwrap();
    let rec = whole["groups"][0]["records"][0].clone();
    assert_eq!(rec["id"], json!(big), "the newest record first");
    let one = serde_json::to_string(&rec).unwrap().chars().count();
    assert!(one > 2600, "the record is longer than the smaller bounds: {one}");
    let mut sizes = vec![];
    for limit in [2000usize, 2600, 4000, 8000, 12000] {
        let l = limit.to_string();
        let (rc, out, err) = ask(&["--max-chars", &l]);
        assert_eq!(rc, 0, "{limit}: {err}");
        let n = out.trim_end().chars().count();
        let v: Value = serde_json::from_str(&out).unwrap();
        let compact = serde_json::to_string(&v).unwrap().chars().count();
        assert!(compact <= limit, "{limit}: {compact}");
        assert!(v["shown"].as_u64().unwrap() >= 1, "{limit}: at least one record: {v}");
        if limit < one {
            let t = &v["truncated"];
            let named = t["fields"].as_array().unwrap().iter().filter_map(|f| f["path"].as_str()).chain(t["dropped_from"].as_object().into_iter().flat_map(|d| d.keys().map(String::as_str))).collect::<Vec<_>>();
            assert!(!named.is_empty() && named.iter().all(|p| p.starts_with("/groups/0/records/0/")), "{limit}: {t}");
            assert!(t["dropped_from"]["/groups/0/records/0/edges"].as_u64().is_some_and(|n| n > 0), "{limit}: its edges first: {t}");
            assert_eq!(v["groups"][0]["records"][0]["id"], json!(big), "{limit}: the cut record keeps its id");
            assert_eq!(v["next_after"], json!(big), "{limit}: the next page starts after it");
        }
        let (rc, text, err) = ask(&["--max-chars", &l, "--text"]);
        assert_eq!(rc, 0, "{limit}: {err}");
        assert!(text.chars().count() <= limit, "{limit}: the text is bounded too: {}", text.chars().count());
        assert!(text.contains(&big), "{limit}: {text}");
        sizes.push((compact, n));
    }
    for w in sizes.windows(2) {
        assert!(w[0].0 < w[1].0, "a smaller limit, a smaller answer: {sizes:?}");
    }
    for low in ["1999", "256", "1"] {
        let (rc, _, err) = ask(&["--max-chars", low]);
        assert_ne!(rc, 0, "{low}");
        assert!(err.contains(&format!("max_chars {low} is under 2000")), "{low}: {err}");
        let (rc, _, err) = ask(&["--max-chars", low, "--text"]);
        assert_ne!(rc, 0, "{low}");
        assert!(err.contains("is under 2000"), "{low}: {err}");
    }
}

/// **A held region is shown everywhere, and holds the command line's own
/// import too.** A ripple's lock on R-1 (a row in the soup): `soup regions`
/// marks R-1 `locked`, `mcp status` and `ripples status` (service stopped)
/// list it; `soup import` into R-1 is refused `region_locked` by name and
/// writes nothing, into R-2 it lands. With `CAULDRON_OLD_CLI` naming a
/// build of the release before region locks, that build reads and writes
/// the same store as before: the rows are a file it never opens, and a
/// revision is never stored.
#[test]
fn rp6_a_held_region_is_shown_and_refuses_the_command_lines_import() {
    let s = scratch("rp6");
    assert_eq!(run(&s, &["env", "create", "dev", "--no-engine"]).0, 0);
    assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).0, 0);
    let regions = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../claude-code/cauldron/regions.example.json");
    assert_eq!(run(&s, &["soup", "import", regions.to_str().unwrap(), "--env", "dev", "--cauldron", "kb", "--remint"]).0, 0);
    let e = env::Environment {
        root: s.join("root"),
        root_source: env::paths::Source::CauldronHome,
        name: "dev".into(),
        name_source: env::paths::Source::CauldronEnv,
        data_root: s.join("root/data/dev"),
    };
    let c = e.find_cauldron("kb").unwrap().unwrap();
    let mut t = env::region_locks::Table::default();
    let h = env::region_locks::Holder { kind: "ripple".into(), run: "run-held".into(), dump: Some("DM-1".into()) };
    t.acquire(&h, "placing the records drawn from dump DM-1", "ws", &["R-1".to_string()], &[], env::region_locks::now_ms(), 600_000).unwrap();
    env::region_locks::save(&c, &t).unwrap();

    let (rc, out, err) = run(&s, &["soup", "regions", "--env", "dev", "--cauldron", "kb"]);
    assert_eq!(rc, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let by = |id: &str| v["regions"].as_array().unwrap().iter().find(|r| r["id"] == id).unwrap().clone();
    assert_eq!(by("R-1")["locked"]["holder"]["run"], "run-held", "{out}");
    assert!(by("R-1")["locked"]["until"].is_string());
    assert!(by("R-3").get("locked").is_none());
    let (_, out, _) = run(&s, &["mcp", "status", "--env", "dev"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    let kb = v["cauldrons"].as_array().unwrap().iter().find(|c| c["cauldron"] == "kb").unwrap().clone();
    assert_eq!(kb["region_locks"][0]["regions"], json!(["R-1"]), "{out}");
    assert_eq!(kb["region_locks"][0]["holder"]["dump"], "DM-1");
    let (_, out, _) = run(&s, &["ripples", "status", "--env", "dev"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["region_locks"][0]["cauldron"], "kb", "{out}");
    assert_eq!(v["region_locks"][0]["locks"][0]["regions"], json!(["R-1"]));

    let record = |region: &str, handle: &str| {
        let p = s.join(format!("{handle}.json").replace(' ', "-"));
        std::fs::write(&p, json!([{"id": "x", "type": "idea", "state": "idea", "handle": handle, "edges": [{"kind": "in_region", "to": region}]}]).to_string()).unwrap();
        p
    };
    let held = record("R-1", "an idea for the held region");
    let (rc, out, err) = run(&s, &["soup", "import", held.to_str().unwrap(), "--env", "dev", "--cauldron", "kb", "--remint"]);
    assert_ne!(rc, 0, "{out}");
    assert!(err.contains("region `R-1` is locked by a ripple (run run-held, drawing records from dump DM-1)") && err.contains("Nothing was written"), "{err}");
    // R-2 narrows R-1: held too (a held region covers the regions below
    // it). R-3 does not.
    let below = record("R-2", "an idea for the region below");
    let (rc, _, err) = run(&s, &["soup", "import", below.to_str().unwrap(), "--env", "dev", "--cauldron", "kb", "--remint"]);
    assert!(rc != 0 && err.contains("region `R-1` is locked"), "{err}");
    let free = record("R-3", "an idea for the free region");
    let (rc, _, err) = run(&s, &["soup", "import", free.to_str().unwrap(), "--env", "dev", "--cauldron", "kb", "--remint"]);
    assert_eq!(rc, 0, "{err}");

    if let Some(old) = std::env::var_os("CAULDRON_OLD_CLI") {
        let old_run = |args: &[&str]| {
            let o = Command::new(&old)
                .args(args)
                .env("CAULDRON_HOME", s.join("root"))
                .env("HOME", s.join("home"))
                .env_remove("CAULDRON_ENV")
                .env_remove("ENV")
                .stdin(Stdio::null())
                .output()
                .unwrap();
            (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
        };
        for args in [&["soup", "regions", "--env", "dev", "--cauldron", "kb"][..], &["soup", "nodes", "--env", "dev", "--cauldron", "kb"][..], &["soup", "get", "R-1", "--env", "dev", "--cauldron", "kb"][..], &["mcp", "status", "--env", "dev"][..]] {
            let (rc, out, err) = old_run(args);
            assert_eq!(rc, 0, "the older build on {args:?}: {out}\n{err}");
        }
        let older = record("R-4", "an idea written by the older build");
        let (rc, _, err) = old_run(&["soup", "import", older.to_str().unwrap(), "--env", "dev", "--cauldron", "kb", "--remint"]);
        assert_eq!(rc, 0, "the older build writes the store as before: {err}");
        // And this build reads it after, the lock still shown.
        let (rc, out, _) = run(&s, &["soup", "regions", "--env", "dev", "--cauldron", "kb"]);
        assert_eq!(rc, 0);
        assert!(out.contains("run-held"), "{out}");
    } else {
        eprintln!("rp6: CAULDRON_OLD_CLI names no binary; the older build was not run");
    }
}

/// **A damaged region-lock table: refused with the fix, then recovered.**
/// Truncated, garbage, of the wrong shape, zero bytes: an import, a rename
/// and a remove are each refused, naming `cauldron soup recover`; while a
/// ripple run of the cauldron is live in the ripples journal, recover
/// refuses to set it aside and changes nothing; once it is not, recover
/// sets the file aside under a dated name and says so, and the import and
/// the rename go through.
#[test]
fn rp7_a_damaged_lock_table_is_refused_with_its_fix_and_recover_sets_it_aside() {
    let s = scratch("rp7");
    assert_eq!(run(&s, &["env", "create", "dev", "--no-engine"]).0, 0);
    let regions = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../claude-code/cauldron/regions.example.json");
    let record = s.join("idea.json");
    std::fs::write(&record, json!([{"id": "x", "type": "idea", "state": "idea", "handle": "an idea once the table is set aside", "edges": [{"kind": "in_region", "to": "R-1"}]}]).to_string()).unwrap();
    let damaged: [(&str, &str); 4] = [("truncated", r#"{"format": 1, "rows": [{"pool": "#), ("garbage", "not a table"), ("wrong shape", r#"{"format": 1, "rows": {"R-1": true}}"#), ("zero bytes", "")];
    for (n, (what, text)) in damaged.iter().enumerate() {
        let name = format!("kb{n}");
        assert_eq!(run(&s, &["create", &name, "--env", "dev"]).0, 0);
        assert_eq!(run(&s, &["soup", "import", regions.to_str().unwrap(), "--env", "dev", "--cauldron", &name, "--remint"]).0, 0);
        let soup = s.join(format!("root/data/dev/cauldrons/{name}/soup"));
        let table = soup.join("region-locks.json");
        std::fs::write(&table, text).unwrap();
        let fix = format!("cauldron soup recover --env dev --cauldron {name}");
        let (rc, out, err) = run(&s, &["soup", "import", record.to_str().unwrap(), "--env", "dev", "--cauldron", &name, "--remint"]);
        assert_ne!(rc, 0, "{what}: {out}");
        assert!(err.contains("region locks cannot be read") && err.contains(&fix), "{what}: {err}");
        for args in [&["rename", name.as_str(), "renamed", "--env", "dev"][..], &["remove", name.as_str(), "--env", "dev", "--yes"][..]] {
            let (rc, _, err) = run(&s, args);
            assert!(rc != 0 && err.contains(&fix), "{what} {args:?}: {err}");
        }
        // A live run in the ripples journal (this process stands in for it).
        let journal = s.join("root/data/dev/ripples/runs.jsonl");
        std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
        let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let line = |event: &str| json!({"at": at, "run": format!("run-{n}"), "cauldron": name, "dump": "DM-1", "pid": std::process::id(), "event": event}).to_string() + "\n";
        std::fs::write(&journal, line("started") + &line("call")).unwrap();
        let (rc, out, err) = run(&s, &["soup", "recover", "--env", "dev", "--cauldron", &name]);
        assert_ne!(rc, 0, "{what}: {out}");
        assert!(err.contains("a ripple run of it may be live") && err.contains("cauldron --env dev stop ripples"), "{what}: {err}");
        assert_eq!(std::fs::read_to_string(&table).unwrap(), *text, "{what}: nothing changed");
        // The run ended.
        std::fs::write(&journal, line("started") + &line("call") + &line("done")).unwrap();
        let (rc, out, err) = run(&s, &["soup", "recover", "--env", "dev", "--cauldron", &name]);
        assert_eq!(rc, 0, "{what}: {out}\n{err}");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["region_locks"]["outcome"], "set_aside", "{what}: {out}");
        assert!(err.contains("was set aside as") && err.contains("the soup now holds no region lock"), "{what}: {err}");
        let moved = std::path::PathBuf::from(v["region_locks"]["moved_to"].as_str().unwrap());
        assert_eq!(moved.parent().unwrap(), soup, "{what}");
        assert_eq!(std::fs::read_to_string(&moved).unwrap(), *text, "{what}: kept whole");
        assert!(!table.exists());
        let (rc, _, err) = run(&s, &["soup", "import", record.to_str().unwrap(), "--env", "dev", "--cauldron", &name, "--remint"]);
        assert_eq!(rc, 0, "{what}: {err}");
        // A second recover finds the table readable and leaves it.
        let (rc, out, _) = run(&s, &["soup", "recover", "--env", "dev", "--cauldron", &name]);
        assert_eq!(rc, 0);
        assert!(serde_json::from_str::<Value>(&out).unwrap().get("region_locks").is_none(), "{out}");
        let (rc, _, err) = run(&s, &["rename", &name, &format!("done{n}"), "--env", "dev"]);
        assert_eq!(rc, 0, "{what}: {err}");
    }
}
