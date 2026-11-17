//! **The `mcp` service beside the command line**: a forwarded import, the
//! refused recover, `cauldron mcp status`, and the stdio bridge.
//!
//! Every test runs the built `cauldron` and the real `cauldron-mcp` (built
//! here from this worktree by cargo, as `environments.rs` builds its probe)
//! under a scratch `CAULDRON_HOME`/`HOME`; this binary exports
//! `CAULDRON_ISOLATED=1`, so a `cauldron` child may dial only the scratch
//! service's listeners, which the test hands it (`crate::grants::owned`).
//!
//! - **F1** while the service holds a granted cauldron, `soup import` is
//!   sent to it and lands through it, printing the import's own JSON (plus
//!   `forwarded_to`); the batch-already-landed guard and `--allow-duplicate`
//!   hold across the two ways of landing; a refused record prints the
//!   refusal as a direct import does; `--dry-run` is never sent;
//!   `soup recover` is refused naming the service; `mcp status` shows it
//!   holding the cauldron. An ungranted cauldron imports directly while the
//!   service runs, and once the service stops, so does the granted one.
//! - **B1** the bridge: with the service down it answers `initialize` and
//!   `tools/list` and refuses every call as `service_down`, naming the start
//!   command; with it up it relays, with `_ctx` set from its own working
//!   directory and session (a caller's own `_ctx` replaced); the door secret
//!   never appears on its stdout or stderr; a service stopped mid-session
//!   turns the next call into `service_down`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use soup_door::grants::Scope;

const CAULDRON: &str = env!("CARGO_BIN_EXE_cauldron");
const README: &str = include_str!("../../../schema/soup/workspace-v6/README.md");

/// `cauldron-mcp`, built from THIS worktree by cargo once per test process,
/// at the path cargo reports.
fn service_bin() -> PathBuf {
    static BIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        let target = Path::new(CAULDRON).parent().unwrap().parent().unwrap();
        let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["build", "-p", "cauldron-mcp", "--bin", "cauldron-mcp", "--message-format=json-render-diagnostics", "--target-dir"])
            .arg(target)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .stderr(Stdio::inherit())
            .output()
            .expect("spawn cargo build -p cauldron-mcp");
        assert!(out.status.success(), "building cauldron-mcp for the test: {:?}", out.status);
        let exe = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == "cauldron-mcp")
            .find_map(|m| m["executable"].as_str().map(PathBuf::from))
            .expect("cargo reported no cauldron-mcp executable");
        assert!(exe.exists(), "cauldron-mcp at {}", exe.display());
        exe
    })
    .clone()
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Scratch {
    fn new(tag: &str, cauldrons: &[&str]) -> Scratch {
        let p = std::env::temp_dir().join(format!(
            "cauldron-mcp-svc-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        for d in ["root/data/dev/mcp", "home", "work/proj/sub"] {
            std::fs::create_dir_all(p.join(d)).unwrap();
        }
        let s = Scratch(p.canonicalize().unwrap());
        if let Some(h) = std::env::var_os("HOME") {
            assert!(!s.0.starts_with(Path::new(&h).join(".cauldron")), "scratch under ~/.cauldron");
        }
        for c in cauldrons {
            let o = s.run(&["create", c, "--env", "dev"], &[]);
            assert_eq!(o.rc, 0, "create {c}: {}", o.err);
        }
        s
    }
    fn root(&self) -> PathBuf {
        self.0.join("root")
    }
    fn data(&self) -> PathBuf {
        self.root().join("data").join("dev")
    }
    fn mcp_dir(&self) -> PathBuf {
        self.data().join("mcp")
    }
    fn soup(&self, c: &str) -> PathBuf {
        self.data().join("cauldrons").join(c).join("soup")
    }
    fn project(&self) -> PathBuf {
        self.0.join("work").join("proj")
    }
    fn env(&self) -> env::Environment {
        env::Environment {
            root: self.root(),
            root_source: env::paths::Source::CauldronHome,
            name: "dev".into(),
            name_source: env::paths::Source::CauldronEnv,
            data_root: self.data(),
        }
    }
    fn grant(&self, c: &str) {
        let p = soup_door::grants::canonical_project(&self.project()).unwrap();
        soup_door::grants::grant(&self.env(), c, &Scope::ALL, &[p], "tty").unwrap();
    }
    fn command(&self, args: &[&str], pids: &[u32]) -> Command {
        let mut c = Command::new(CAULDRON);
        c.args(args)
            .env("CAULDRON_HOME", self.root())
            .env("HOME", self.0.join("home"))
            .env(crate::grants::ALLOW_ENV, crate::grants::owned(&self.0, pids))
            .env_remove("CAULDRON_ENV")
            .env_remove("ENV")
            .env_remove("CAULDRON_CAULDRON")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(self.0.join("work"));
        c
    }
    fn run(&self, args: &[&str], pids: &[u32]) -> Out {
        let o = self.command(args, pids).stdin(Stdio::null()).output().expect("spawn cauldron");
        Out { rc: o.status.code().unwrap_or(-1), out: String::from_utf8_lossy(&o.stdout).into_owned(), err: String::from_utf8_lossy(&o.stderr).into_owned() }
    }
    fn import(&self, c: &str, file: &Path, extra: &[&str], pids: &[u32]) -> Out {
        let mut a = vec!["soup", "import", file.to_str().unwrap(), "--env", "dev", "--cauldron", c];
        a.extend_from_slice(extra);
        self.run(&a, pids)
    }
    fn write(&self, name: &str, v: &Value) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, serde_json::to_string(v).unwrap()).unwrap();
        p
    }
    fn node_count(&self, c: &str) -> i64 {
        let conn = spine::offline::read_only_connection(&self.soup(c).join("ws.db")).unwrap();
        conn.query_row("SELECT count(*) FROM nodes", [], |r| r.get(0)).unwrap()
    }
    fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(self.mcp_dir().join("calls.jsonl")).unwrap_or_default().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }
    fn secret(&self) -> String {
        std::fs::read_to_string(self.mcp_dir().join("door.secret")).unwrap().trim().to_string()
    }
}

struct Out {
    rc: i32,
    out: String,
    err: String,
}
impl Out {
    fn json(&self) -> Value {
        serde_json::from_str(&self.out).unwrap_or_else(|e| panic!("not JSON ({e}): {}\n{}", self.out, self.err))
    }
}

fn example() -> Vec<Value> {
    let at = README.find("## A worked example").unwrap();
    let start = README[at..].find("```json").unwrap() + at + "```json".len();
    let end = README[start..].find("```").unwrap() + start;
    serde_json::from_str(&README[start..end]).unwrap()
}

fn task(label: &str, handle: &str) -> Value {
    json!({"id": label, "type": "task", "state": "ready", "handle": handle, "edges": [{"kind": "in_region", "to": "R-1"}]})
}

// ------------------------------------------------------------ the service

struct Service {
    child: Child,
    door: SocketAddr,
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Service {
    /// Started as `cauldron start mcp` starts it.
    fn start(s: &Scratch) -> Service {
        let log = std::fs::File::create(s.0.join(format!("service-{}.log", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()))).unwrap();
        let bin = service_bin();
        let child = Command::new(&bin)
            .env("CAULDRON_SERVICE", "mcp")
            .env("CAULDRON_ENV", "dev")
            .env("CAULDRON_HOME", s.root())
            .env("CAULDRON_DATA_ROOT", s.data())
            .env("CAULDRON_SERVICE_DATA_DIR", s.mcp_dir())
            .env("CAULDRON_SEAMS", s.data().join("seams.json"))
            .env("CAULDRON_BOOT_TOKEN", format!("boot-{}", std::process::id()))
            .env("CAULDRON_VERSION", "test")
            .env("CAULDRON_COMMIT", "0000000000000000000000000000000000000000")
            .env("CAULDRON_DIRTY", "0")
            .env("CAULDRON_BIND", "127.0.0.1:0")
            .env("CAULDRON_BINARY", &bin)
            .env("CAULDRON_PROBE", "seam")
            .env("HOME", s.0.join("home"))
            .env_remove("CAULDRON_CAULDRON")
            .env_remove("SPINE_FAULT")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("spawn cauldron-mcp");
        let pid = child.id() as u64;
        let deadline = Instant::now() + Duration::from_secs(60);
        let seam: SocketAddr = loop {
            let t: Value = std::fs::read_to_string(s.data().join("seams.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
            if t["seams"]["mcp"]["pid"].as_u64() == Some(pid) {
                break t["seams"]["mcp"]["addr"].as_str().unwrap().parse().unwrap();
            }
            assert!(Instant::now() < deadline, "the service never registered");
            std::thread::sleep(Duration::from_millis(50));
        };
        let mut c = TcpStream::connect(seam).unwrap();
        writeln!(c, "MCP").unwrap();
        let mut url = String::new();
        BufReader::new(c).read_line(&mut url).unwrap();
        let door = url.trim().strip_prefix("http://").and_then(|u| u.strip_suffix("/mcp")).unwrap().parse().unwrap();
        Service { child, door }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// One `tools/call` straight to the door, as the test's own client.
    fn call(&self, s: &Scratch, tool: &str, mut args: Value) -> Value {
        args["_ctx"] = json!({"client": "test", "cwd": s.project().display().to_string()});
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}).to_string();
        let mut c = TcpStream::connect(self.door).unwrap();
        write!(
            c,
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.door.port(),
            s.secret(),
            body.len()
        )
        .unwrap();
        let mut raw = String::new();
        c.read_to_string(&mut raw).unwrap();
        let v: Value = serde_json::from_str(raw.split_once("\r\n\r\n").unwrap().1).unwrap();
        v["result"].clone()
    }

    /// SIGTERM, and wait for it to exit.
    fn stop(mut self) {
        Command::new("/bin/kill").args(["-TERM", &self.child.id().to_string()]).status().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the service ignored SIGTERM");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn lock_free(s: &Scratch, c: &str) -> bool {
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(s.soup(c).join("writer.lock")).unwrap();
    f.try_lock().is_ok()
}

// --------------------------------------------------------------------- F1

#[test]
fn f1_an_import_is_sent_to_the_service_that_holds_the_soup() {
    let s = Scratch::new("f1", &["kb", "other"]);
    let ex = s.write("example.json", &json!(example()));
    assert_eq!(s.import("kb", &ex, &[], &[]).rc, 0);
    assert_eq!(s.import("other", &ex, &[], &[]).rc, 0);
    // The shape of a direct import's answer, before the service exists.
    let direct = s.import("kb", &s.write("d.json", &json!([task("t", "Direct before the service")])), &["--remint"], &[]);
    assert_eq!(direct.rc, 0, "{}", direct.err);
    let direct_keys: Vec<String> = direct.json().as_object().unwrap().keys().cloned().collect();
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    // Not held until the service's first write.
    assert!(lock_free(&s, "kb"));
    let r = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "Opens the writer")]}));
    assert_eq!(r["isError"], json!(false), "{r}");
    assert!(!lock_free(&s, "kb"), "the service holds the soup");

    // A real import: sent to the service, landed through it, same shape.
    let before = s.node_count("kb");
    let batch = s.write("batch.json", &json!([task("a", "Forwarded task a"), task("b", "Forwarded task b")]));
    let o = s.import("kb", &batch, &["--remint"], &pids);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    let d = o.json();
    assert_eq!(d["outcome"], "landed", "{d}");
    assert_eq!(d["forwarded_to"], json!({"service": "mcp", "pid": svc.pid()}));
    let mut keys: Vec<String> = d.as_object().unwrap().keys().filter(|k| *k != "forwarded_to").cloned().collect();
    let mut want = direct_keys.clone();
    keys.sort();
    want.sort();
    assert_eq!(keys, want, "the forwarded answer has the direct import's keys");
    assert_eq!(d["judged_on"], "the soup");
    assert!(o.err.contains(&format!("the `mcp` service (pid {})", svc.pid())), "{}", o.err);
    assert_eq!(s.node_count("kb"), before + 2);
    let last = s.calls().into_iter().filter(|l| l["tool"] == "soup_put").last().unwrap();
    assert_eq!((last["client"].clone(), last["outcome"].clone()), (json!("cli"), json!("landed")), "{last}");

    // The same batch again: refused as a direct import refuses it, through
    // the service; --allow-duplicate lands it.
    let o = s.import("kb", &batch, &["--remint"], &pids);
    assert_eq!(o.rc, 1, "{}\n{}", o.out, o.err);
    let d = o.json();
    assert_eq!((d["refusal"].clone(), d["forwarded_to"]["pid"].clone()), (json!("batch-already-landed"), json!(svc.pid())), "{d}");
    assert_eq!(d["landed_before"][0]["records"], 2);
    assert!(o.err.contains("REFUSED (batch-already-landed)") && o.err.contains("--allow-duplicate"), "{}", o.err);
    assert_eq!(s.node_count("kb"), before + 2);
    let o = s.import("kb", &batch, &["--remint", "--allow-duplicate"], &pids);
    assert_eq!((o.rc, o.json()["outcome"].clone()), (0, json!("landed")), "{}", o.err);
    assert_eq!(s.node_count("kb"), before + 4);
    // A batch landed DIRECTLY earlier is known to the service too.
    let o = s.import("kb", &s.write("d.json", &json!([task("t", "Direct before the service")])), &["--remint"], &pids);
    assert_eq!(o.json()["refusal"], "batch-already-landed", "{}", o.out);

    // A refused record: the refusal, printed as a direct import prints it.
    let o = s.import("kb", &s.write("bad.json", &json!([task("x", "Fine"), {"id": "y", "type": "task", "state": "no-such-state", "handle": "Bad state"}])), &["--remint"], &pids);
    assert_eq!(o.rc, 1, "{}\n{}", o.out, o.err);
    let d = o.json();
    assert_eq!((d["outcome"].clone(), d["accepted"].clone(), d["refused"].clone()), (json!("refused"), json!(1), json!(1)), "{d}");
    assert!(d["records"].as_array().unwrap().iter().any(|r| r["id"] == "y" && r["disposition"] == "refused"), "{d}");
    assert_eq!(s.node_count("kb"), before + 4);

    // A dry run is judged on a snapshot, never sent.
    let o = s.import("kb", &s.write("dry.json", &json!([task("t", "Dry")])), &["--remint", "--dry-run"], &pids);
    assert_eq!((o.rc, o.json()["outcome"].clone()), (0, json!("validated")), "{}", o.err);
    assert!(o.json().get("forwarded_to").is_none());

    // recover is refused, naming the service.
    let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "kb"], &pids);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains(&format!("the `mcp` service (pid {})", svc.pid())) && o.err.contains("cauldron mcp status"), "{}", o.err);

    // status: the service, and what it holds.
    let o = s.run(&["mcp", "status", "--env", "dev"], &pids);
    assert_eq!(o.rc, 0, "{}", o.err);
    let st = o.json();
    assert_eq!((st["service"]["state"].clone(), st["service"]["pid"].clone()), (json!("live"), json!(svc.pid())), "{st}");
    assert_eq!(st["open"][0]["cauldron"], "kb", "{st}");
    assert_eq!(st["open"][0]["recovered_at_open"]["interrupted"], "none");
    let kb = st["cauldrons"].as_array().unwrap().iter().find(|c| c["cauldron"] == "kb").unwrap();
    assert_eq!((kb["held_by_service"].clone(), kb["writer_lock_says"].clone()), (json!(true), json!(format!("mcp {}", svc.pid()))), "{st}");
    assert_eq!(kb["granted"], json!(["read", "write", "dump"]));
    let other = st["cauldrons"].as_array().unwrap().iter().find(|c| c["cauldron"] == "other").unwrap();
    assert_eq!((other["held_by_service"].clone(), other["granted"].clone()), (json!(false), Value::Null), "{st}");

    // An ungranted cauldron: the service holds nothing, the import is direct.
    let o = s.import("other", &s.write("o.json", &json!([task("t", "Direct into the ungranted one")])), &["--remint"], &pids);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert!(o.json().get("forwarded_to").is_none(), "{}", o.out);

    // The service stopped: the import is direct again, and status says so.
    let pid = svc.pid();
    svc.stop();
    assert!(lock_free(&s, "kb"));
    let o = s.import("kb", &s.write("after.json", &json!([task("t", "Direct after the service")])), &["--remint"], &[]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert!(o.json().get("forwarded_to").is_none(), "{}", o.out);
    assert!(!o.err.contains(&format!("pid {pid}")), "{}", o.err);
    let st = s.run(&["mcp", "status", "--env", "dev"], &[]).json();
    assert_eq!(st["service"]["state"], "not running", "{st}");
    assert_eq!(st["service"]["start"], "cauldron --env dev start mcp");
    let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "kb"], &[]);
    assert_eq!(o.rc, 0, "recover works again once the service let go: {}", o.err);
}

#[test]
fn f2_an_import_too_large_for_the_door_is_refused_before_it_is_sent() {
    let s = Scratch::new("f2", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let r = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "Opens the writer")]}));
    assert_eq!(r["isError"], json!(false), "{r}");
    let before = s.node_count("kb");
    let puts = s.calls().into_iter().filter(|l| l["tool"] == "soup_put").count();
    // About 18 MiB of records: more than the door reads in one request.
    let body = "x".repeat(450);
    let records: Vec<Value> = (0..40_000)
        .map(|i| json!({"id": format!("t{i}"), "type": "task", "state": "ready", "handle": format!("Large import task {i}"), "body": body,
            "edges": [{"kind": "in_region", "to": "R-1"}]}))
        .collect();
    let big = s.write("huge.json", &json!(records));
    assert!(std::fs::metadata(&big).unwrap().len() > soup_door::catalogue::REQUEST_BODY_LIMIT as u64);
    let o = s.import("kb", &big, &["--remint"], &pids);
    assert_eq!(o.rc, 1, "{}\n{}", o.out, o.err);
    for want in ["16.0 MiB", "40000 records", "lands all of it or none of it", "Nothing was written", "cauldron mcp revoke kb --env dev", "stop mcp"] {
        assert!(o.err.contains(want), "missing `{want}`: {}", o.err);
    }
    assert!(!o.err.contains("Connection reset"), "{}", o.err);
    assert_eq!(s.node_count("kb"), before, "part of the batch landed");
    assert_eq!(s.calls().into_iter().filter(|l| l["tool"] == "soup_put").count(), puts, "the batch was sent");
}

// --------------------------------------------------------------------- B1

/// A bridge, driven line by line as Claude Code drives it.
struct Bridge {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    err: PathBuf,
    seen: String,
}

impl Bridge {
    fn start(s: &Scratch, cwd: &Path, extra: &[&str], vars: &[(&str, &str)], pids: &[u32]) -> Bridge {
        let mut a = vec!["--env", "dev"];
        a.extend_from_slice(extra);
        Bridge::start_bare(s, cwd, &a, vars, pids)
    }

    /// `cauldron mcp stdio <args>`, with no `--env` added.
    fn start_bare(s: &Scratch, cwd: &Path, args: &[&str], vars: &[(&str, &str)], pids: &[u32]) -> Bridge {
        let err = s.0.join(format!("bridge-{}.err", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let mut a = vec!["mcp", "stdio"];
        a.extend_from_slice(args);
        let mut c = s.command(&a, pids);
        c.current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(std::fs::File::create(&err).unwrap());
        for (k, v) in vars {
            c.env(k, v);
        }
        let mut child = c.spawn().expect("spawn the bridge");
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Bridge { child, stdin, stdout, err, seen: String::new() }
    }

    /// Send one message; read one answer line (None for a notification).
    fn ask(&mut self, msg: Value) -> Option<Value> {
        let w = self.stdin.as_mut().unwrap();
        writeln!(w, "{msg}").unwrap();
        w.flush().unwrap();
        msg.get("id")?;
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        self.seen.push_str(&line);
        let v: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("not one JSON line ({e}): {line}"));
        assert_eq!(v["id"], msg["id"], "{v}");
        Some(v)
    }

    fn call(&mut self, id: u64, tool: &str, args: Value) -> Value {
        self.ask(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}})).unwrap()["result"].clone()
    }

    /// Close stdin; the bridge exits; everything it wrote.
    fn finish(mut self) -> (i32, String, String) {
        drop(self.stdin.take());
        let mut rest = String::new();
        self.stdout.read_to_string(&mut rest).unwrap();
        self.seen.push_str(&rest);
        let st = self.child.wait().unwrap();
        (st.code().unwrap_or(-1), self.seen.clone(), std::fs::read_to_string(&self.err).unwrap_or_default())
    }
}

fn init(b: &mut Bridge) -> Value {
    let i = b.ask(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}})).unwrap();
    assert!(b.ask(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
    i
}

fn refused(r: &Value) -> (String, String) {
    assert_eq!(r["isError"], json!(true), "not refused: {r}");
    let x = &r["structuredContent"]["refused"];
    (x["code"].as_str().unwrap().to_string(), x["message"].as_str().unwrap().to_string())
}

#[test]
fn b1_the_bridge_relays_with_its_own_context_and_answers_when_the_service_is_down() {
    let s = Scratch::new("b1", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    s.grant("kb");
    let names = soup_door::catalogue::TOOLS.iter().map(|t| t.name).collect::<Vec<_>>();
    let sub = s.project().join("sub");

    // DOWN: initialize and tools/list answered by the bridge; every call
    // refused as service_down, naming the start command.
    let mut b = Bridge::start(&s, &sub, &[], &[], &[]);
    let i = init(&mut b);
    assert_eq!(i["result"]["protocolVersion"], "2025-06-18", "{i}");
    assert!(i["result"]["instructions"].as_str().unwrap().contains("cauldron --env dev start mcp"), "{i}");
    let list = b.ask(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    let listed: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(listed, names);
    let (code, msg) = refused(&b.call(2, "soup_status", json!({})));
    assert_eq!(code, "service_down");
    assert!(msg.contains("`cauldron --env dev start mcp`"), "{msg}");
    let (rc, _, err) = b.finish();
    assert_eq!(rc, 0, "{err}");

    // UP: relayed, with the bridge's own context.
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let secret = s.secret();
    let mut b = Bridge::start(&s, &sub, &[], &[("CLAUDE_CODE_SESSION_ID", "sess-b1")], &pids);
    let i = init(&mut b);
    assert!(!i["result"]["instructions"].as_str().unwrap().contains("not running"), "{i}");
    let list = b.ask(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), names.len());
    assert!(list["result"]["tools"].as_array().unwrap().iter().all(|t| t["inputSchema"]["properties"].get("_ctx").is_none()));
    // The project is found from the bridge's working directory (a
    // subdirectory of the granted project), whatever `_ctx` the caller sent.
    let st = b.call(2, "soup_status", json!({"_ctx": {"client": "evil", "cwd": "/", "session_id": "forged"}}));
    assert_eq!(st["isError"], json!(false), "{st}");
    let want = format!("project:{}", s.project().display());
    assert_eq!(st["structuredContent"]["resolved_from"], json!(want), "{st}");
    // A write: the session from the bridge's environment is cited and audited.
    let put = b.call(3, "soup_put", json!({"remint": true, "records": [task("t", "Through the bridge")], "_ctx": {"client": "evil"}}));
    assert_eq!(put["isError"], json!(false), "{put}");
    let id = put["structuredContent"]["id_map"]["t"].as_str().unwrap().to_string();
    let got = b.call(4, "soup_get", json!({"ids": [id]}));
    let cites = &got["structuredContent"]["records"][0]["meta"]["cites"];
    assert!(cites.as_array().unwrap().contains(&json!({"kind": "session", "id": "sess-b1"})), "{got}");
    let line = s.calls().into_iter().filter(|l| l["tool"] == "soup_put").last().unwrap();
    assert_eq!((line["client"].clone(), line["session"].clone()), (json!("claude-code"), json!("sess-b1")), "{line}");
    assert!(s.calls().iter().all(|l| l["client"] != "evil" && l["session"] != "forged"));
    // The service's own refusals reach the caller as they are.
    let (code, _) = refused(&b.call(5, "soup_get", json!({"ids": "R-1"})));
    assert_eq!(code, "usage");
    let unknown = b.ask(json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "soup_grant", "arguments": {}}})).unwrap();
    assert_eq!(unknown["error"]["code"], json!(-32602), "{unknown}");
    // Stopped mid-session: the next call is service_down.
    svc.stop();
    let (code, _) = refused(&b.call(7, "soup_status", json!({})));
    assert_eq!(code, "service_down");
    let (rc, out, err) = b.finish();
    assert_eq!(rc, 0, "{err}");
    assert!(!out.contains(&secret) && !err.contains(&secret), "the door secret reached the bridge's output");

    // --cauldron names the cauldron outside any granted project.
    let svc = Service::start(&s);
    let secret2 = s.secret();
    let mut b = Bridge::start(&s, &s.0.join("work"), &["--cauldron", "kb"], &[], &[svc.pid()]);
    init(&mut b);
    let st = b.call(1, "soup_status", json!({}));
    assert_eq!(st["structuredContent"]["resolved_from"], "CAULDRON_CAULDRON", "{st}");
    let (_, out, err) = b.finish();
    assert!(!out.contains(&secret2) && !err.contains(&secret2));
    let mut b = Bridge::start(&s, &s.0.join("work"), &[], &[], &[svc.pid()]);
    init(&mut b);
    let (code, _) = refused(&b.call(1, "soup_status", json!({})));
    assert_eq!(code, "no_cauldron_for_project");
    b.finish();
    svc.stop();
}

#[test]
fn b2_the_bridge_does_not_read_env_and_answers_when_its_environment_cannot_be_used() {
    let s = Scratch::new("b2", &["kb"]);
    let cwd = s.0.join("work");
    // `$ENV` names the scratch's real environment, `dev`; the bridge ignores
    // it and looks for `prod`, which this root does not have. It still
    // starts, lists every tool and says why each call is refused.
    let mut b = Bridge::start_bare(&s, &cwd, &[], &[("ENV", "dev")], &[]);
    let i = init(&mut b);
    assert!(i["result"]["instructions"].as_str().unwrap().contains("$ENV is not read"), "{i}");
    let list = b.ask(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 12, "{list}");
    let (code, msg) = refused(&b.call(2, "soup_status", json!({"cauldron": "kb"})));
    assert_eq!(code, "no_environment", "{msg}");
    assert!(msg.contains("environment `prod`") && msg.contains("$CAULDRON_ENV") && msg.contains("$ENV is not read"), "{msg}");
    let (rc, _, err) = b.finish();
    assert_eq!(rc, 0, "{err}");
    // A common `ENV=Development` (not even a usable name) does not stop a
    // bridge whose environment is named: the service is down, and it says so.
    let mut b = Bridge::start_bare(&s, &cwd, &[], &[("ENV", "Development"), ("CAULDRON_ENV", "dev")], &[]);
    init(&mut b);
    let (code, msg) = refused(&b.call(1, "soup_status", json!({"cauldron": "kb"})));
    assert_eq!(code, "service_down", "{msg}");
    assert!(msg.contains("environment `dev`"), "{msg}");
    assert_eq!(b.finish().0, 0);
}
