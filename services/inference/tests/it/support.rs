//! The harness: a scratch root with environment `dev`; a fake
//! `llama-server` (a shell script that runs this test binary's
//! `fake::fake_llama_server`); a registered model file (a GGUF header and
//! some zeros); and the real `cauldron-inference` binary started as
//! `cauldron start inference` would. No model, no network, nothing in the
//! real root.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use inference_registry::gguf::Meta;
use serde_json::{json, Value};

pub const BIN: &str = env!("CARGO_BIN_EXE_cauldron-inference");

pub struct Scratch(pub PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        if std::env::var_os("KEEP_SCRATCH").is_none() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// How the fake behaves (see `fake`).
#[derive(Default, Clone)]
pub struct Fake {
    pub version: Option<String>,
    pub help_drop: Option<String>,
    pub load_ms: Option<u64>,
    pub exit_on_start: Option<i32>,
    pub ignore_term: bool,
}

impl Scratch {
    pub fn new(tag: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!(
            "cauldron-inference-it-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        for d in ["root/data/dev/inference", "home", "fake", "models"] {
            std::fs::create_dir_all(p.join(d)).unwrap();
        }
        Scratch(p.canonicalize().unwrap())
    }

    pub fn root(&self) -> PathBuf {
        self.0.join("root")
    }
    pub fn data(&self) -> PathBuf {
        self.root().join("data").join("dev")
    }
    pub fn dir(&self) -> PathBuf {
        self.data().join("inference")
    }
    /// Where the fake records what it saw.
    pub fn fake_dir(&self) -> PathBuf {
        self.0.join("fake")
    }

    /// Write the fake's script, and settings pointing at it.
    pub fn fake(&self, f: &Fake, settings: &str) -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let mut vars = vec![format!("FAKE_LLAMA_DIR='{}'", self.fake_dir().display())];
        if let Some(v) = &f.version {
            vars.push(format!("FAKE_LLAMA_VERSION='{v}'"));
        }
        if let Some(v) = &f.help_drop {
            vars.push(format!("FAKE_LLAMA_HELP_DROP='{v}'"));
        }
        if let Some(v) = f.load_ms {
            vars.push(format!("FAKE_LLAMA_LOAD_MS={v}"));
        }
        if let Some(v) = f.exit_on_start {
            vars.push(format!("FAKE_LLAMA_EXIT_ON_START={v}"));
        }
        if f.ignore_term {
            vars.push("FAKE_LLAMA_IGNORE_TERM=1".into());
        }
        let script = self.0.join("bin").join("llama-server");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        let names: Vec<String> = vars.iter().map(|v| v.split('=').next().unwrap().to_string()).collect();
        let body = format!(
            "#!/bin/sh\nFAKE_LLAMA_ARGV=\"$(printf '%s\\n' \"$@\")\"\n{}\nexport FAKE_LLAMA_ARGV {}\nexec '{}' fake::fake_llama_server --exact --nocapture --test-threads 1 -q\n",
            vars.join("\n"),
            names.join(" "),
            exe.display()
        );
        std::fs::write(&script, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(self.dir().join("config.toml"), format!("llama_server = \"{}\"\n{settings}", script.display())).unwrap();
        script
    }

    /// The fake's next answers.
    pub fn answers(&self, a: Value) {
        std::fs::write(self.fake_dir().join("answers.json"), a.to_string()).unwrap();
    }

    /// Register a model file named `name`: a GGUF header with these facts
    /// and `pad` bytes of stand-in weights.
    pub fn model(&self, name: &str, entries: &[(&str, Meta)], pad: u64) -> inference_registry::Model {
        let p = self.0.join("models").join(format!("{name}.gguf"));
        inference_registry::gguf::write_header(&p, entries, pad).unwrap();
        let m = inference_registry::Registry::describe(name, &p, None).unwrap();
        let mut r = inference_registry::Registry::load(&self.data()).unwrap();
        r.add(m.clone()).unwrap();
        r.save(&self.data()).unwrap();
        m
    }

    /// A small model: 4 layers, 2 KV heads, 64-wide keys and values.
    pub fn small(&self, name: &str) -> inference_registry::Model {
        self.model(
            name,
            &[
                ("general.architecture", Meta::Str("llama".into())),
                ("llama.block_count", Meta::Int(4)),
                ("llama.attention.head_count", Meta::Int(8)),
                ("llama.attention.head_count_kv", Meta::Int(2)),
                ("llama.embedding_length", Meta::Int(512)),
            ],
            4096,
        )
    }

    pub fn fake_argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.fake_dir().join("argv.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn fake_lines(&self, file: &str) -> Vec<Value> {
        std::fs::read_to_string(self.fake_dir().join(file)).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }
}

/// Is process `pid` alive?
pub fn alive(pid: u32) -> bool {
    Command::new("/bin/kill").arg("-0").arg(pid.to_string()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

pub fn kill(pid: u32, sig: &str) {
    let _ = Command::new("/bin/kill").arg(format!("-{sig}")).arg(pid.to_string()).stderr(Stdio::null()).status();
}

/// Wait up to `secs` for `f`.
pub fn eventually(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    f()
}

/// The service, started as `cauldron start inference` starts it.
pub struct Service {
    pub child: Child,
    pub seam: SocketAddr,
    pub log: PathBuf,
}

impl Drop for Service {
    fn drop(&mut self) {
        kill(self.child.id(), "TERM");
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Service {
    pub fn start(s: &Scratch) -> Service {
        let log = s.0.join(format!("inference-{}.log", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let out = std::fs::File::create(&log).unwrap();
        let child = Command::new(BIN)
            .env("CAULDRON_SERVICE", "inference")
            .env("CAULDRON_ENV", "dev")
            .env("CAULDRON_HOME", s.root())
            .env("CAULDRON_DATA_ROOT", s.data())
            .env("CAULDRON_SERVICE_DATA_DIR", s.dir())
            .env("CAULDRON_SEAMS", s.data().join("seams.json"))
            .env("CAULDRON_BOOT_TOKEN", format!("boot-{}", std::process::id()))
            .env("CAULDRON_VERSION", "test")
            .env("CAULDRON_COMMIT", "0000000000000000000000000000000000000000")
            .env("CAULDRON_DIRTY", "0")
            .env("CAULDRON_BIND", "127.0.0.1:0")
            .env("CAULDRON_BINARY", BIN)
            .env("CAULDRON_PROBE", "seam")
            .env("HOME", s.0.join("home"))
            .env("CAULDRON_INFERENCE_LOAD_TIMEOUT_S", "20")
            .env("LLAMA_ARG_TOOLS", "all")
            .env("LLAMA_ARG_AGENT", "1")
            .stdin(Stdio::null())
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .unwrap();
        let pid = child.id() as u64;
        let deadline = Instant::now() + Duration::from_secs(60);
        let seam: SocketAddr = loop {
            let t: Value = std::fs::read_to_string(s.data().join("seams.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
            if t["seams"]["inference"]["pid"].as_u64() == Some(pid) {
                break t["seams"]["inference"]["addr"].as_str().unwrap().parse().unwrap();
            }
            assert!(Instant::now() < deadline, "the service never registered: {}", std::fs::read_to_string(&log).unwrap_or_default());
            std::thread::sleep(Duration::from_millis(50));
        };
        Service { child, seam, log }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    pub fn key(s: &Scratch) -> String {
        std::fs::read_to_string(s.dir().join("door.secret")).unwrap().trim().to_string()
    }

    /// One seam line; the answer's JSON.
    pub fn say(&self, line: &str) -> Value {
        let mut c = TcpStream::connect(self.seam).unwrap();
        writeln!(c, "{line}").unwrap();
        let mut out = String::new();
        BufReader::new(c).read_line(&mut out).unwrap();
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out}"))
    }

    pub fn verb(&self, s: &Scratch, verb: &str, args: Value) -> Value {
        self.say(&format!("{verb} {} {args}", Service::key(s)))
    }

    pub fn status(&self, s: &Scratch) -> Value {
        self.verb(s, "STATUS", json!({}))
    }

    /// Wait for the process to exit (up to `secs`).
    pub fn wait_exit(&mut self, secs: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }
}

/// An answer for the fake: `content` as the model's text.
pub fn content(v: &Value) -> Value {
    json!({"content": v.to_string()})
}

pub fn read_pid(p: &Path) -> u32 {
    std::fs::read_to_string(p).unwrap().trim().parse().unwrap()
}
