//! The harness: a scratch cauldron root, the real `cauldron-mcp` binary
//! started under it as `cauldron start` would start it, and a minimal HTTP
//! client for `POST /mcp`. Nothing here touches the real root: every test
//! makes its own root under the temp directory, and the service's port is
//! the kernel's choice on loopback.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use soup_door::grants::Scope;
use soup_door::import::{ImportOptions, ImportOutcome};
use soup_door::Target;

pub const BIN: &str = env!("CARGO_BIN_EXE_cauldron-mcp");
const README: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../schema/soup/workspace-v6/README.md"));

/// A scratch root with environment `dev` and the named cauldrons.
pub struct Scratch(pub PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        if std::env::var_os("KEEP_SCRATCH").is_none() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

impl Scratch {
    pub fn new(tag: &str, cauldrons: &[&str]) -> Scratch {
        let p = std::env::temp_dir().join(format!(
            "cauldron-mcp-it-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        for d in ["root/data/dev/mcp", "home", "work"] {
            std::fs::create_dir_all(p.join(d)).unwrap();
        }
        let s = Scratch(p.canonicalize().unwrap());
        if let Some(h) = std::env::var_os("HOME") {
            assert!(!s.0.starts_with(Path::new(&h).join(".cauldron")), "scratch under the real root");
        }
        for c in cauldrons {
            s.create(c);
        }
        s
    }

    pub fn root(&self) -> PathBuf {
        self.0.join("root")
    }
    pub fn data(&self) -> PathBuf {
        self.root().join("data").join("dev")
    }
    pub fn mcp_dir(&self) -> PathBuf {
        self.data().join("mcp")
    }
    pub fn soup(&self, c: &str) -> PathBuf {
        self.data().join("cauldrons").join(c).join("soup")
    }
    /// A project directory under the scratch root.
    pub fn project(&self, name: &str) -> PathBuf {
        let p = self.0.join("work").join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// A cauldron as `cauldron create` makes one: its directory, its
    /// `cauldron.toml`, and a soup with one `ws` pool on grammar v6.
    pub fn create(&self, name: &str) {
        let dir = self.data().join("cauldrons").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cauldron.toml"), format!("name = \"{name}\"\ncreated_at = \"{}\"\nenabled = true\n", chrono::Utc::now().to_rfc3339())).unwrap();
        let c = spine::Cauldron::create(&dir.join("soup"), &[spine::grammar_v6::workspace_v6("ws")], spine::config::SpineConfig::default()).unwrap();
        drop(c);
    }

    pub fn env(&self) -> env::Environment {
        env::Environment {
            root: self.root(),
            root_source: env::paths::Source::CauldronHome,
            name: "dev".into(),
            name_source: env::paths::Source::CauldronEnv,
            data_root: self.data(),
        }
    }

    pub fn target(&self, c: &str) -> Target {
        let e = self.env();
        let cauldron = e.find_cauldron(c).unwrap().expect("the cauldron exists");
        Target { env: e, cauldron, resolved_from: None }
    }

    /// The owner's grant (the CLI's TTY rule is the CLI's; this is the store).
    pub fn grant(&self, c: &str, scopes: &[Scope], projects: &[&Path]) {
        let projects: Vec<String> = projects.iter().map(|p| soup_door::grants::canonical_project(p).unwrap()).collect();
        soup_door::grants::grant(&self.env(), c, scopes, &projects, "tty").unwrap();
    }

    pub fn revoke(&self, c: &str) {
        soup_door::grants::revoke(&self.env(), c, None).unwrap();
    }

    /// Land records directly, as `cauldron soup import --remint` would.
    pub fn import(&self, c: &str, records: &[Value]) -> Value {
        let o = ImportOptions { remint: true, agent: "fixture".into(), dispatch: "fixture".into(), ..ImportOptions::default() };
        match soup_door::import::import(&self.target(c), records, &o).unwrap() {
            ImportOutcome::Judged { doc, ok } => {
                assert!(ok, "fixture refused: {doc}");
                doc
            }
            ImportOutcome::AlreadyLanded { why, .. } => panic!("{why}"),
        }
    }

    /// How many nodes the soup holds.
    pub fn node_count(&self, c: &str) -> i64 {
        let conn = spine::offline::read_only_connection(&self.soup(c).join("ws.db")).unwrap();
        conn.query_row("SELECT count(*) FROM nodes", [], |r| r.get(0)).unwrap()
    }

    pub fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(self.mcp_dir().join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

/// The README's worked example: two regions and nine nodes.
pub fn example() -> Vec<Value> {
    let at = README.find("## A worked example").expect("README has its worked example");
    let start = README[at..].find("```json").unwrap() + at + "```json".len();
    let end = README[start..].find("```").unwrap() + start;
    serde_json::from_str(&README[start..end]).expect("the worked example is JSON")
}

/// One region and `n` tasks in it, under batch-local labels.
pub fn load_batch(n: usize, tag: &str) -> Vec<Value> {
    let mut v = vec![json!({"id": "load-region", "type": "region", "handle": format!("Load {tag}"),
        "boundary": "Inside: the load test's tasks. Outside: everything else."})];
    v.extend((1..=n).map(|i| {
        json!({"id": format!("load-task-{i}"), "type": "task", "state": "ready", "handle": format!("{tag} task {i}"),
            "body": "A task of the load test, with a body long enough to be cut when a reply is bounded.",
            "edges": [{"kind": "in_region", "to": "load-region", "note": "the load test"}]})
    }));
    v
}

// --------------------------------------------------------------- the service

/// The service, started as `cauldron start mcp` starts it.
pub struct Service {
    pub child: Child,
    pub door: SocketAddr,
    pub bearer: String,
    pub log: PathBuf,
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn seams(s: &Scratch) -> Value {
    std::fs::read_to_string(s.data().join("seams.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null)
}

impl Service {
    pub fn start(s: &Scratch) -> Service {
        Service::start_with(s, &[])
    }

    /// Started with these extra variables (e.g. the spine's fault seam).
    pub fn start_with(s: &Scratch, vars: &[(&str, &str)]) -> Service {
        let log = s.0.join(format!("service-{}.log", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let out = std::fs::File::create(&log).unwrap();
        let token = format!("boot-{}", std::process::id());
        let child = Command::new(BIN)
            .env("CAULDRON_SERVICE", "mcp")
            .env("CAULDRON_ENV", "dev")
            .env("CAULDRON_HOME", s.root())
            .env("CAULDRON_DATA_ROOT", s.data())
            .env("CAULDRON_SERVICE_DATA_DIR", s.mcp_dir())
            .env("CAULDRON_SEAMS", s.data().join("seams.json"))
            .env("CAULDRON_BOOT_TOKEN", &token)
            .env("CAULDRON_VERSION", "test")
            .env("CAULDRON_COMMIT", "0000000000000000000000000000000000000000")
            .env("CAULDRON_DIRTY", "0")
            .env("CAULDRON_BIND", "127.0.0.1:0")
            .env("CAULDRON_BINARY", BIN)
            .env("CAULDRON_PROBE", "seam")
            .env("HOME", s.0.join("home"))
            .env_remove("CAULDRON_CAULDRON")
            .env_remove("SPINE_FAULT")
            .envs(vars.iter().copied())
            .stdin(Stdio::null())
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .expect("spawn cauldron-mcp");
        let pid = child.id() as u64;
        let deadline = Instant::now() + Duration::from_secs(60);
        let seam: SocketAddr = loop {
            let t = seams(s);
            let row = &t["seams"]["mcp"];
            if row["pid"].as_u64() == Some(pid) {
                break row["addr"].as_str().unwrap().parse().unwrap();
            }
            assert!(Instant::now() < deadline, "the service never registered: {}", std::fs::read_to_string(&log).unwrap_or_default());
            std::thread::sleep(Duration::from_millis(50));
        };
        let url = seam_say(seam, "MCP");
        let door: SocketAddr = url.strip_prefix("http://").and_then(|u| u.strip_suffix("/mcp")).expect("MCP answers the door's url").parse().unwrap();
        let bearer = format!("Bearer {}", std::fs::read_to_string(s.mcp_dir().join("door.secret")).unwrap().trim());
        Service { child, door, bearer, log }
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// SIGTERM, and wait for the process to exit.
    pub fn stop(mut self) -> std::process::ExitStatus {
        Command::new("/bin/kill").args(["-TERM", &self.child.id().to_string()]).status().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(st) = self.child.try_wait().unwrap() {
                return st;
            }
            assert!(Instant::now() < deadline, "the service ignored SIGTERM");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// One `tools/call`; answers the JSON-RPC `result`.
    pub fn call(&self, tool: &str, args: Value, ctx: &Value) -> Value {
        let mut args = args;
        args["_ctx"] = ctx.clone();
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}});
        let r = self.post(&body.to_string(), &[]);
        assert_eq!(r.status, 200, "{}", r.body);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        v.get("result").cloned().unwrap_or_else(|| panic!("no result: {v}"))
    }

    /// A raw `POST /mcp` with the secret, the MCP headers and `extra`
    /// (an `extra` header replaces a default of the same name).
    pub fn post(&self, body: &str, extra: &[(&str, &str)]) -> Resp {
        let mut h: Vec<(String, String)> = vec![
            ("Host".into(), format!("127.0.0.1:{}", self.door.port())),
            ("Authorization".into(), self.bearer.clone()),
            ("Accept".into(), "application/json, text/event-stream".into()),
            ("Content-Type".into(), "application/json".into()),
        ];
        for (k, v) in extra {
            h.retain(|(hk, _)| !hk.eq_ignore_ascii_case(k));
            if !v.is_empty() {
                h.push((k.to_string(), v.to_string()));
            }
        }
        http(self.door, "POST", &h, body)
    }
}

impl Service {
    /// `soup_dump_session` as the stdio bridge sends it: the call on the
    /// first line, then the transcript's bytes.
    pub fn dump_session(&self, args: Value, ctx: &Value, transcript: &[u8]) -> Resp {
        let mut args = args;
        args["_ctx"] = ctx.clone();
        let call = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "soup_dump_session", "arguments": args}});
        let mut body = call.to_string().into_bytes();
        body.push(b'\n');
        body.extend_from_slice(transcript);
        let h = vec![
            ("Host".to_string(), format!("127.0.0.1:{}", self.door.port())),
            ("Authorization".to_string(), self.bearer.clone()),
            ("Content-Type".to_string(), "application/octet-stream".to_string()),
        ];
        http_bytes(self.door, "POST", "/mcp/dump-session", &h, &body)
    }

    /// [`Service::dump_session`]'s JSON-RPC `result`.
    pub fn dump_session_result(&self, args: Value, ctx: &Value, transcript: &[u8]) -> Value {
        let r = self.dump_session(args, ctx, transcript);
        assert_eq!(r.status, 200, "{}", r.body);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["id"], 7, "{v}");
        v.get("result").cloned().unwrap_or_else(|| panic!("no result: {v}"))
    }
}

/// HTTP/1.1 to `path` with a raw body and `Connection: close`.
pub fn http_bytes(addr: SocketAddr, method: &str, path: &str, headers: &[(String, String)], body: &[u8]) -> Resp {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(600))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()));
    s.write_all(req.as_bytes()).unwrap();
    // The service may answer (a refusal) before reading the whole body.
    let _ = s.write_all(body);
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    Resp { status, head: head.to_string(), body: body.to_string() }
}

/// A Claude Code transcript as a fixture: `n` JSONL lines, each recording
/// `cwd`, with a little of everything a real one holds.
pub fn transcript(n: usize, cwd: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..n {
        let line = json!({"type": if i % 2 == 0 { "user" } else { "assistant" }, "cwd": cwd, "sessionId": "sess-fixture",
            "message": {"role": if i % 2 == 0 { "user" } else { "assistant" }, "content": format!("line {i}: some words, \"quoted\", é")}});
        out.extend_from_slice(line.to_string().as_bytes());
        out.push(b'\n');
    }
    out
}

/// SHA-256 of `b`, hex.
pub fn sha256_hex(b: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(b))
}

/// The dumps store of `c`.
pub fn dumps_dir(s: &Scratch, c: &str) -> PathBuf {
    s.data().join("cauldrons").join(c).join("dumps")
}

/// One line to the seam, one line back.
pub fn seam_say(addr: SocketAddr, line: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    writeln!(s, "{line}").unwrap();
    let mut out = String::new();
    BufReader::new(s).read_line(&mut out).unwrap();
    out.trim().to_string()
}

pub struct Resp {
    pub status: u16,
    pub head: String,
    pub body: String,
}

/// HTTP/1.1 with `Connection: close`, read to the end.
pub fn http(addr: SocketAddr, method: &str, headers: &[(String, String)], body: &str) -> Resp {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(600))).unwrap();
    let mut req = format!("{method} /mcp HTTP/1.1\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()));
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    Resp { status, head: head.to_string(), body: body.to_string() }
}

/// The payload of a successful call.
pub fn ok(result: &Value) -> Value {
    assert_eq!(result["isError"], json!(false), "refused: {result}");
    result["structuredContent"].clone()
}

/// The refusal of a refused call: `(code, message, detail)`.
pub fn refused(result: &Value) -> (String, String, Value) {
    assert_eq!(result["isError"], json!(true), "not refused: {result}");
    let r = &result["structuredContent"]["refused"];
    (r["code"].as_str().unwrap().to_string(), r["message"].as_str().unwrap().to_string(), r["detail"].clone())
}

/// A `_ctx` from `cwd`.
pub fn ctx(cwd: &Path) -> Value {
    json!({"client": "test", "cwd": cwd.display().to_string()})
}
