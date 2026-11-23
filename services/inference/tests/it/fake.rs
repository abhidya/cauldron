//! **A fake `llama-server`**: this test binary, run as one test
//! (`fake::fake_llama_server`) by a small shell script the service is
//! pointed at as its `llama_server`. With no `FAKE_LLAMA_DIR` the test does
//! nothing, so in a normal run it passes at once.
//!
//! The script sets, before it `exec`s this binary:
//!
//! - `FAKE_LLAMA_DIR`: where it records `argv.json` (its arguments),
//!   `pid`, `requests.jsonl` (each request: method, path, whether the
//!   bearer key matched `--api-key-file`, the JSON body) and `events.jsonl`
//!   (`disconnected` when a client went away before the answer);
//! - `FAKE_LLAMA_ARGV`: its arguments, one per line;
//! - optionally `FAKE_LLAMA_VERSION` (what `--version` prints),
//!   `FAKE_LLAMA_HELP_DROP` (a flag left out of `--help`),
//!   `FAKE_LLAMA_LOAD_MS` (`/health` answers 503 that long),
//!   `FAKE_LLAMA_EXIT_ON_START` (exit with that code at once),
//!   `FAKE_LLAMA_IGNORE_TERM=1` (TERM does not stop it).
//!
//! Each completion takes the next entry of `<dir>/answers.json` (the last
//! one again when they run out): `{"content": "<text>"}`, `{"status": 400,
//! "error": "<message>"}`, `{"exit": <code>}`, any of them with `"sleep_ms"`
//! first.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Every flag the real `--help` lists that the service looks for.
const HELP: &str = "\
----- common params -----
-h,    --help, --usage                  print usage and exit
--version                               show version and build info
-c,    --ctx-size N                     size of the prompt context
-m,    --model FNAME                    model path to load
-j,    --json-schema SCHEMA             JSON schema to constrain generations
-cram, --cache-ram N                    set the maximum cache size in MiB
-np,   --parallel N                     number of server slots
--host HOST                             IP addresses to listen on
--port PORT                             port to listen (default: 9931)
--offline                               Offline mode
--alias STRING                          set alias for model name
-ag,   --agent, -no-ag, --no-agent      whether to enable built-in tools
--ui,  --webui, --no-ui, --no-webui     whether to enable the Web UI
--api-key KEY                           API key
--api-key-file FNAME                    path to file containing API keys
--slots, --no-slots                     expose slots monitoring endpoint
--jinja, --no-jinja                     whether to use jinja template engine for chat
-rea,  --reasoning [on|off|auto]        Use reasoning/thinking in the chat
";

fn record(dir: &Path, file: &str, v: Value) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(file)) {
        let _ = writeln!(f, "{v}");
    }
}

#[test]
fn fake_llama_server() {
    let Some(dir) = std::env::var_os("FAKE_LLAMA_DIR").map(PathBuf::from) else { return };
    let argv: Vec<String> = std::env::var("FAKE_LLAMA_ARGV").unwrap_or_default().lines().map(str::to_string).collect();
    if argv.iter().any(|a| a == "--version") {
        eprintln!("{}", std::env::var("FAKE_LLAMA_VERSION").unwrap_or_else(|_| "version: 0.9.0 (build 12110)".into()));
        eprintln!("built with a test harness");
        std::process::exit(0);
    }
    if argv.iter().any(|a| a == "--help") {
        let drop = std::env::var("FAKE_LLAMA_HELP_DROP").unwrap_or_default();
        for l in HELP.lines() {
            if drop.is_empty() || !l.contains(&drop) {
                println!("{l}");
            }
        }
        std::process::exit(0);
    }
    std::fs::write(dir.join("argv.json"), json!(argv).to_string()).unwrap();
    std::fs::write(dir.join("pid"), std::process::id().to_string()).unwrap();
    let names: Vec<String> = std::env::vars().map(|(k, _)| k).collect();
    std::fs::write(dir.join("env.json"), json!(names).to_string()).unwrap();
    if let Some(code) = std::env::var("FAKE_LLAMA_EXIT_ON_START").ok().and_then(|c| c.parse::<i32>().ok()) {
        eprintln!("fake llama-server: failing to load on purpose");
        std::process::exit(code);
    }
    if std::env::var("FAKE_LLAMA_IGNORE_TERM").as_deref() == Ok("1") {
        // TERM caught and dropped: only a kill stops it now.
        let mut s = signal_hook::iterator::Signals::new([signal_hook::consts::SIGTERM]).unwrap();
        std::thread::spawn(move || for _ in s.forever() {});
    }
    let flag = |f: &str| argv.iter().position(|a| a == f).and_then(|i| argv.get(i + 1)).cloned();
    let host = flag("--host").unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = flag("--port").and_then(|p| p.parse().ok()).expect("--port");
    let key = flag("--api-key-file").and_then(|p| std::fs::read_to_string(p).ok()).map(|k| k.trim().to_string()).unwrap_or_default();
    let load_ms: u64 = std::env::var("FAKE_LLAMA_LOAD_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let l = TcpListener::bind((host.as_str(), port)).expect("bind");
    eprintln!("fake llama-server: listening on {host}:{port}");
    let started = Instant::now();
    let n = std::sync::Arc::new(AtomicUsize::new(0));
    for conn in l.incoming().flatten() {
        let (dir, key, n) = (dir.clone(), key.clone(), n.clone());
        let ready = started.elapsed() >= Duration::from_millis(load_ms);
        std::thread::spawn(move || serve(conn, &dir, &key, ready, &n));
    }
}

fn respond(mut s: &TcpStream, status: u16, body: &Value) -> std::io::Result<()> {
    let b = body.to_string();
    write!(s, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}", b.len())?;
    s.flush()
}

fn serve(conn: TcpStream, dir: &Path, key: &str, ready: bool, n: &AtomicUsize) {
    let mut r = BufReader::new(conn.try_clone().unwrap());
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    let (mut auth, mut len) = (String::new(), 0usize);
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
            break;
        }
        let (k, v) = h.split_once(':').unwrap_or(("", ""));
        match k.trim().to_ascii_lowercase().as_str() {
            "authorization" => auth = v.trim().to_string(),
            "content-length" => len = v.trim().parse().unwrap_or(0),
            _ => {}
        }
    }
    let mut body = vec![0u8; len];
    let _ = r.read_exact(&mut body);
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let authed = !key.is_empty() && auth == format!("Bearer {key}");
    record(dir, "requests.jsonl", json!({"method": method, "path": path, "authorized": authed, "body": body}));
    if !authed {
        let _ = respond(&conn, 401, &json!({"error": {"message": "Invalid API Key"}}));
        return;
    }
    if path == "/health" {
        let _ = if ready { respond(&conn, 200, &json!({"status": "ok"})) } else { respond(&conn, 503, &json!({"error": {"message": "Loading model"}})) };
        return;
    }
    if path != "/v1/chat/completions" {
        let _ = respond(&conn, 404, &json!({"error": {"message": "not found"}}));
        return;
    }
    let answers: Vec<Value> = std::fs::read_to_string(dir.join("answers.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    // Counted across children: the completions recorded before this one.
    let _ = n.fetch_add(1, Ordering::SeqCst);
    let i = std::fs::read_to_string(dir.join("requests.jsonl")).unwrap_or_default().lines().filter(|l| l.contains("\"/v1/chat/completions\"")).count().saturating_sub(1);
    let a = answers.get(i).or(answers.last()).cloned().unwrap_or(json!({"content": "{\"records\": []}"}));
    if let Some(ms) = a["sleep_ms"].as_u64() {
        // Wait, watching for the client to go away.
        let _ = conn.set_read_timeout(Some(Duration::from_millis(50)));
        let deadline = Instant::now() + Duration::from_millis(ms);
        let mut probe = [0u8; 1];
        while Instant::now() < deadline {
            match (&conn).read(&mut probe) {
                Ok(0) => {
                    record(dir, "events.jsonl", json!({"event": "disconnected", "request": i}));
                    return;
                }
                _ => {}
            }
        }
    }
    if let Some(code) = a["exit"].as_i64() {
        eprintln!("fake llama-server: exiting during a request on purpose");
        std::process::exit(code as i32);
    }
    if let Some(st) = a["status"].as_u64() {
        let _ = respond(&conn, st as u16, &json!({"error": {"message": a["error"], "type": "invalid_request_error"}}));
        return;
    }
    let content = a["content"].as_str().unwrap_or("{}").to_string();
    let _ = respond(
        &conn,
        200,
        &json!({
            "choices": [{"index": 0, "finish_reason": a.get("finish_reason").cloned().unwrap_or(json!("stop")), "message": {"role": "assistant", "content": content}}],
            "usage": {"prompt_tokens": 1200, "completion_tokens": 300, "total_tokens": 1500},
            "timings": {"prompt_n": 1200, "prompt_ms": 400.0, "prompt_per_second": 3000.0, "predicted_n": 300, "predicted_ms": 5000.0, "predicted_per_second": 60.0},
            "model": body["model"],
        }),
    );
}
