//! The harness: a scratch cauldron root with one cauldron (`kb`, the
//! schema README's worked example: two regions), granted to the MCP door;
//! the mcp door's ripple seam run IN THIS PROCESS (`cauldron_mcp::ripple`)
//! behind a loopback line listener; the scripted inference backend; and the
//! real `cauldron-ripples` binary started as `cauldron start ripples` would.
//! No model is called and nothing touches the real root.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use soup_door::grants::Scope;

pub const BIN: &str = env!("CARGO_BIN_EXE_cauldron-ripples");
const README: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../schema/soup/workspace-v6/README.md"));

pub struct Scratch(pub PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        if std::env::var_os("KEEP_SCRATCH").is_none() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

impl Scratch {
    /// Environment `dev`, cauldron `kb` with the worked example, granted
    /// read, write and dump.
    pub fn new(tag: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!(
            "cauldron-ripples-it-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        for d in ["root/data/dev/mcp", "root/data/dev/ripples", "home"] {
            std::fs::create_dir_all(p.join(d)).unwrap();
        }
        let s = Scratch(p.canonicalize().unwrap());
        let dir = s.data().join("cauldrons").join("kb");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cauldron.toml"), format!("name = \"kb\"\ncreated_at = \"{}\"\nenabled = true\n", chrono::Utc::now().to_rfc3339())).unwrap();
        drop(spine::Cauldron::create(&dir.join("soup"), &[spine::grammar_v6::workspace_v6("ws")], spine::config::SpineConfig::default()).unwrap());
        let o = soup_door::import::ImportOptions { remint: true, agent: "fixture".into(), dispatch: "fixture".into(), ..Default::default() };
        let records = example();
        match soup_door::import::import(&s.target(), &records, &o).unwrap() {
            soup_door::import::ImportOutcome::Judged { ok: true, .. } => {}
            other => panic!("fixture: {other:?}"),
        }
        s.grant(&[Scope::Read, Scope::Write, Scope::Dump]);
        s
    }

    pub fn root(&self) -> PathBuf {
        self.0.join("root")
    }
    pub fn data(&self) -> PathBuf {
        self.root().join("data").join("dev")
    }
    pub fn ripples_dir(&self) -> PathBuf {
        self.data().join("ripples")
    }

    /// Where a run's private working directories are made (made here).
    pub fn scratch_root(&self) -> PathBuf {
        let p = self.data().join("tmp");
        std::fs::create_dir_all(&p).unwrap();
        p
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

    pub fn target(&self) -> soup_door::Target {
        let e = self.env();
        let cauldron = e.find_cauldron("kb").unwrap().unwrap();
        soup_door::Target { env: e, cauldron, resolved_from: None }
    }

    pub fn grant(&self, scopes: &[Scope]) {
        soup_door::grants::grant(&self.env(), "kb", scopes, &[], "tty").unwrap();
    }

    /// The nodes of the soup a ripple wrote (by the spine's record).
    pub fn ripple_nodes(&self) -> Vec<String> {
        let t = self.target();
        let txs: Vec<String> = soup_door::reads::ripple_commits(&t.cauldron).unwrap().into_iter().collect();
        soup_door::ripple::created_by(&t.cauldron, &txs).unwrap()
    }

    pub fn dump_state(&self, id: &str) -> Value {
        soup_door::dumps::summary(&self.target().cauldron, id).unwrap()
    }

    /// The evaluations' ledger.
    pub fn eval_ledger(&self) -> Vec<Value> {
        cauldron_ripples::journal::read(&self.ripples_dir().join(cauldron_ripples::journal::EVAL_FILE))
    }

    pub fn journal(&self) -> Vec<Value> {
        std::fs::read_to_string(self.ripples_dir().join("runs.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// A scripted backend's script file, with these entries.
    pub fn script(&self, name: &str, entries: Value) -> PathBuf {
        let p = self.0.join(format!("{name}.json"));
        std::fs::write(&p, entries.to_string()).unwrap();
        p
    }
}

/// What a scripted backend was asked, call by call (a line still being
/// written is not counted yet).
pub fn calls(script: &Path) -> Vec<Value> {
    std::fs::read_to_string(format!("{}.calls.jsonl", script.display()))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The README's worked example: two regions and nine nodes.
pub fn example() -> Vec<Value> {
    let at = README.find("## A worked example").unwrap();
    let start = README[at..].find("```json").unwrap() + at + "```json".len();
    let end = README[start..].find("```").unwrap() + start;
    serde_json::from_str(&README[start..end]).unwrap()
}

// ------------------------------------------------------------- the door

/// The mcp door, in this process, answering its ripple seam on loopback.
pub struct Door {
    pub door: Arc<cauldron_mcp::door::Door>,
    pub seam: SocketAddr,
}

impl Door {
    pub fn start(s: &Scratch) -> Door {
        let dir = s.data().join("mcp");
        let secret = cauldron_mcp::secret::mint(&dir).unwrap();
        let key = cauldron_mcp::secret::mint_named(&dir, cauldron_mcp::secret::RIPPLES_FILE).unwrap();
        let door = cauldron_mcp::door::Door::new(s.env(), secret, "test".into(), &dir);
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let seam = l.local_addr().unwrap();
        env::egress::grant_addr(seam).unwrap();
        let d = door.clone();
        std::thread::spawn(move || {
            for conn in l.incoming().flatten() {
                let (d, key) = (d.clone(), key.clone());
                std::thread::spawn(move || {
                    let mut out = conn.try_clone().unwrap();
                    for line in BufReader::new(conn).lines() {
                        let Ok(line) = line else { return };
                        let (verb, rest) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
                        let a = cauldron_mcp::ripple::answer(&d, &key, verb, rest);
                        if writeln!(out, "{a}").is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Door { door, seam }
    }

    fn tool(&self, name: &str, args: Value) -> Value {
        let mut args = args;
        args["_ctx"] = json!({"client": "test"});
        self.door.call(cauldron_mcp::catalogue::find(name).unwrap(), args).unwrap_or_else(|r| panic!("{name}: {r:?}"))
    }

    /// A text dump; its id.
    pub fn dump(&self, text: &str) -> String {
        self.tool("soup_dump", json!({"cauldron": "kb", "text": text}))["dump_id"].as_str().unwrap().to_string()
    }

    /// A session dump of `transcript`; its id.
    pub fn dump_session(&self, transcript: &[u8]) -> String {
        let mut sd = self
            .door
            .session_begin(json!({"_ctx": {"client": "test"}, "cauldron": "kb", "session_id": "sess-fixture"}), transcript.len() as u64)
            .unwrap();
        sd.blob.write_all(transcript).unwrap();
        self.door.session_finish(sd, Ok(())).unwrap()["dump_id"].as_str().unwrap().to_string()
    }

    /// Let the dumps store go (so a CLI-side removal can take it).
    pub fn release_dumps(&self) {
        self.door.dumps.close("kb", "test");
    }
}

/// A transcript with user and assistant text, thinking, tool calls and tool
/// output.
pub fn transcript() -> Vec<u8> {
    let mut out = Vec::new();
    let mut push = |v: Value| {
        out.extend_from_slice(v.to_string().as_bytes());
        out.push(b'\n');
    };
    push(json!({"type": "user", "message": {"role": "user", "content": "the build cache keeps going stale after a rebase"}}));
    push(json!({"type": "assistant", "message": {"role": "assistant", "content": [
        {"type": "thinking", "thinking": "HIDDEN-THOUGHT-MARKER"},
        {"type": "text", "text": "I will read the cache key code."},
        {"type": "tool_use", "id": "call-1", "name": "Read", "input": {"file_path": "/w/cache.rs"}}]}}));
    push(json!({"type": "user", "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": "call-1", "content": "TOOL-OUTPUT-MARKER fn key() {}"}]}}));
    push(json!({"type": "assistant", "message": {"role": "assistant", "content": "The key ignores the rebase; we should hash the tree instead."}}));
    out
}

// ------------------------------------------------------- answers to script

/// An answer of `n` ideas in R-1.
pub fn ideas(n: usize) -> Value {
    json!({"records": (1..=n).map(|i| json!({"label": format!("n{i}"), "type": "idea", "state": "idea",
        "handle": format!("idea {i} drawn from the dump"), "regions": ["R-1"]})).collect::<Vec<_>>()})
}

pub fn usage(input: u64, output: u64) -> Value {
    json!({"input_tokens": input, "output_tokens": output, "cache_read_tokens": 10, "cache_creation_tokens": 0})
}

// ------------------------------------------------------------ the service

/// The ripples service, started as `cauldron start ripples` starts it.
pub struct Service {
    pub child: Child,
    pub seam: SocketAddr,
    pub log: PathBuf,
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Service {
    pub fn start(s: &Scratch, door: &Door, vars: &[(&str, String)]) -> Service {
        let log = s.0.join(format!("ripples-{}.log", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let out = std::fs::File::create(&log).unwrap();
        let child = Service::command(s, door, vars).stdout(out.try_clone().unwrap()).stderr(out).spawn().unwrap();
        Service::registered(s, child, log)
    }

    /// The service's command, its environment set, its output not yet.
    pub fn command(s: &Scratch, door: &Door, vars: &[(&str, String)]) -> Command {
        let mut grants = vec![format!("{}@{}", door.seam, std::process::id())];
        if let Some((_, t)) = vars.iter().find(|(k, _)| *k == "CAULDRON_RIPPLES_THREADS_ADDR") {
            grants.push(format!("{t}@{}", std::process::id()));
        }
        let mut cmd = Command::new(BIN);
        cmd.env("CAULDRON_SERVICE", "ripples")
            // A run's working directory goes under the system's temporary
            // directory; a test's goes under its own scratch, so one cut
            // short by a kill is not left behind.
            .env("TMPDIR", s.scratch_root())
            .env("CAULDRON_ENV", "dev")
            .env("CAULDRON_HOME", s.root())
            .env("CAULDRON_DATA_ROOT", s.data())
            .env("CAULDRON_SERVICE_DATA_DIR", s.ripples_dir())
            .env("CAULDRON_SEAMS", s.data().join("seams.json"))
            .env("CAULDRON_BOOT_TOKEN", format!("boot-{}", std::process::id()))
            .env("CAULDRON_VERSION", "test")
            .env("CAULDRON_COMMIT", "0000000000000000000000000000000000000000")
            .env("CAULDRON_DIRTY", "0")
            .env("CAULDRON_BIND", "127.0.0.1:0")
            .env("CAULDRON_BINARY", BIN)
            .env("CAULDRON_PROBE", "seam")
            .env("HOME", s.0.join("home"))
            .env("CAULDRON_RIPPLES_MCP_SEAM", door.seam.to_string())
            .env("CAULDRON_ISOLATED_PORTS", grants.join(","))
            .env("CAULDRON_RIPPLES_BACKOFF_MS", "10")
            .env_remove("CAULDRON_RIPPLES_INFERENCE")
            .env_remove("CAULDRON_RIPPLES_FAULT")
            .env_remove("THREADS_SECRET")
            .envs(vars.iter().map(|(k, v)| (*k, v.as_str())))
            .stdin(Stdio::null());
        cmd
    }

    fn registered(s: &Scratch, child: std::process::Child, log: PathBuf) -> Service {
        let pid = child.id() as u64;
        let deadline = Instant::now() + Duration::from_secs(60);
        let seam: SocketAddr = loop {
            let t: Value = std::fs::read_to_string(s.data().join("seams.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
            if t["seams"]["ripples"]["pid"].as_u64() == Some(pid) {
                break t["seams"]["ripples"]["addr"].as_str().unwrap().parse().unwrap();
            }
            assert!(Instant::now() < deadline, "the service never registered: {}", std::fs::read_to_string(&log).unwrap_or_default());
            std::thread::sleep(Duration::from_millis(50));
        };
        Service { child, seam, log }
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    pub fn key(s: &Scratch) -> String {
        std::fs::read_to_string(s.ripples_dir().join("run.secret")).unwrap().trim().to_string()
    }

    /// One seam line; the answer's JSON.
    pub fn say(&self, line: &str) -> Value {
        let mut c = TcpStream::connect(self.seam).unwrap();
        writeln!(c, "{line}").unwrap();
        let mut out = String::new();
        BufReader::new(c).read_line(&mut out).unwrap();
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out}"))
    }

    /// `RUN`, then `STATUS` until the run is done; its outcome.
    pub fn run(&self, s: &Scratch, dump: &str) -> Value {
        let k = Service::key(s);
        let a = self.say(&format!("RUN {k} {}", json!({"cauldron": "kb", "dump": dump})));
        assert_eq!(a["ok"], json!(true), "{a}");
        if a["done"] == json!(true) {
            return a["result"].clone();
        }
        let run = a["run"].as_str().unwrap().to_string();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let st = self.say(&format!("STATUS {k} {}", json!({"run": run})));
            if st["state"] == "done" {
                return st["result"].clone();
            }
            assert!(Instant::now() < deadline, "the run never ended: {st}\n{}", self.log());
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// An in-process engine over `door`, with the scripted backend.
pub fn engine(s: &Scratch, door: &Door, script: &Path, caps: cauldron_ripples::engine::Caps) -> cauldron_ripples::engine::Ctx {
    let mut mcp = cauldron_ripples::mcp::Mcp::new(&s.env());
    mcp.seam = Some(door.seam.to_string());
    cauldron_ripples::engine::Ctx {
        env: s.env(),
        mcp,
        inference: Box::new(cauldron_ripples::inference::Scripted::new(script)),
        journal: cauldron_ripples::journal::Journal::new(&s.ripples_dir()),
        caps,
        scratch_root: s.scratch_root(),
        backoff: Duration::from_millis(5),
    }
}

pub fn ask(dump: &str) -> cauldron_ripples::engine::Ask {
    cauldron_ripples::engine::Ask { cauldron: "kb".into(), dump: dump.into(), model: None, force_budget: None }
}
