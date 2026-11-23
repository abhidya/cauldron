//! **A bare completion** (`ThreadSettings::completion`): a thread whose
//! turns are a text answer with nothing an agent has. The argv and the
//! environment, built from settings that try to loosen them, and then one
//! real turn through the daemon with a capturing fake `claude` (never a real
//! invocation): what that process was handed, on its argv, in its
//! environment and on its stdin.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use threads::build_manager;
use threads::config::Config;
use threads::settings::{Completion, ThreadSettings, ToolConfig};
use threads::spawn::{SpawnConfig, SpawnLogin, COMPLETION_DENIED_TOOLS, COMPLETION_ENV_KEPT};

/// Settings that ask for everything an agent has: a completion must drop it.
fn loose() -> ThreadSettings {
    let mut s = ThreadSettings::new("/scratch/empty", "sonnet");
    s.permission_mode = Some("bypassPermissions".into());
    s.tools = ToolConfig {
        clean_slate: false,
        tools: Some("Bash,Edit,Read".into()),
        agents_json: Some("{\"x\":{}}".into()),
        allowed_tools: vec!["Bash(*)".into()],
        disallowed_tools: vec!["Mine".into(), "Bash".into()],
    };
    s.add_dirs = vec!["/home-ish".into()];
    s.agent = Some("helper".into());
    s.plugin_dirs = vec!["/plugins/p".into()];
    s.completion = Some(Completion { max_output_tokens: 8000 });
    s
}

fn value_of(a: &[String], flag: &str) -> Vec<String> {
    a.windows(2).filter(|w| w[0] == flag).map(|w| w[1].clone()).collect()
}

/// Every property of the request, as argv.
fn assert_completion_argv(a: &[String], thread: &str) {
    for absent in [
        "--dangerously-skip-permissions",
        "--allow-dangerously-skip-permissions",
        "--allowedTools",
        "--agents",
        "--agent",
        "--add-dir",
        "--plugin-dir",
        "--resume",
        "--settings",
    ] {
        assert!(!a.iter().any(|x| x == absent), "{absent} on a completion's argv: {a:?}");
    }
    assert_eq!(value_of(a, "--permission-mode"), vec!["dontAsk"], "{a:?}");
    assert_eq!(value_of(a, "--tools"), vec![String::new()], "{a:?}");
    let denied = value_of(a, "--disallowedTools");
    for t in COMPLETION_DENIED_TOOLS {
        assert!(denied.iter().any(|d| d == t), "{t} not denied: {denied:?}");
    }
    assert!(!denied.iter().any(|d| d == "StructuredOutput"), "the answer's own tool is never denied");
    assert!(a.iter().any(|x| x == "--strict-mcp-config"), "{a:?}");
    let mcp = value_of(a, "--mcp-config");
    assert_eq!(mcp.len(), 1, "{a:?}");
    assert_eq!(serde_json::from_str::<Value>(&mcp[0]).unwrap(), json!({"mcpServers": {}}));
    assert_eq!(value_of(a, "--setting-sources"), vec![String::new()], "{a:?}");
    assert_eq!(a.iter().filter(|x| *x == "--safe-mode").count(), 1, "no CLAUDE.md, by the flag that says so: {a:?}");
    assert!(a.iter().any(|x| x == "--disable-slash-commands"), "{a:?}");
    assert!(a.iter().any(|x| x == "--no-session-persistence"), "{a:?}");
    assert_eq!(value_of(a, "--session-id"), vec![thread.to_string()], "{a:?}");
    assert_eq!(value_of(a, "--model"), vec!["sonnet"], "{a:?}");
}

#[test]
fn a_completion_argv_has_no_tool_server_setting_plugin_or_skip_mode() {
    let s = loose();
    let mut cfg = SpawnConfig::from_settings("uuid-c", &s, vec!["/plugins/scanned".into()]);
    cfg.mcp_config = Some("/would/be/the/facade.json".into());
    cfg.json_schema = Some("{\"type\":\"object\"}".into());
    for first in [true, false] {
        let a = cfg.build_args(first);
        assert_completion_argv(&a, "uuid-c");
        assert!(value_of(&a, "--disallowedTools").contains(&"Mine".to_string()), "the thread's own deny list is kept");
        let mut d = value_of(&a, "--disallowedTools");
        let n = d.len();
        d.sort();
        d.dedup();
        assert_eq!(d.len(), n, "each tool is denied once");
        assert_eq!(value_of(&a, "--json-schema"), vec!["{\"type\":\"object\"}"]);
        assert!(!a.iter().any(|x| x == "/would/be/the/facade.json"), "the MCP endpoint never reaches a completion: {a:?}");
    }
    // The interactive recipe never carries print-only flags either.
    let i = cfg.build_interactive_args(true, "/s.json");
    assert!(!i.iter().any(|x| x == "-p" || x == "--no-session-persistence" || x == "--json-schema"), "{i:?}");

    // Opposed: the same settings without `completion` keep the agent's argv.
    let mut agent = loose();
    agent.completion = None;
    let a = SpawnConfig::from_settings("uuid-a", &agent, vec!["/plugins/scanned".into()]).build_args(true);
    assert_eq!(value_of(&a, "--permission-mode"), vec!["bypassPermissions"]);
    assert!(a.iter().any(|x| x == "--plugin-dir") && a.iter().any(|x| x == "--allowedTools"));
    assert!(!a.iter().any(|x| x == "--strict-mcp-config" || x == "--setting-sources" || x == "--safe-mode" || x == "--no-session-persistence"));
    let d = SpawnConfig::from_settings("uuid-d", &ThreadSettings::new("/w", "sonnet"), vec![]).build_args(true);
    assert!(d.iter().any(|x| x == "--dangerously-skip-permissions"), "the default posture is unchanged for every other caller");
}

#[test]
fn a_completion_environment_keeps_only_what_claude_needs() {
    let cfg = SpawnConfig::from_settings("uuid-e", &loose(), vec![]);
    let inherited = ["PATH", "HOME", "USER", "TMPDIR", "LANG", "SSH_AUTH_SOCK", "PWD", "OLDPWD", "THREADS_SECRET", "CAULDRON_HOME", "HOSTNAME", "EDITOR", "THREADS_RUN_TOKEN"]
        .map(String::from);
    let e = cfg.env_from(&SpawnLogin::pool_account("/logins/a").unwrap(), inherited.clone());
    let get = |k: &str| e.iter().rev().find(|(ek, _)| ek == k).map(|(_, v)| v.clone());
    assert_eq!(get("CLAUDE_CODE_MAX_OUTPUT_TOKENS"), Some(Some("8000".into())));
    assert_eq!(get("CLAUDE_CONFIG_DIR"), Some(Some("/logins/a".into())));
    for k in ["DISABLE_TELEMETRY", "DISABLE_ERROR_REPORTING", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "CLAUDE_CODE_DISABLE_AUTO_MEMORY"] {
        assert_eq!(get(k), Some(Some("1".into())), "{k}");
    }
    assert_eq!(get("ANTHROPIC_API_KEY"), Some(None));
    assert_eq!(get("THREADS_RUN_TOKEN"), Some(None));
    for k in &inherited {
        if COMPLETION_ENV_KEPT.contains(&k.as_str()) {
            assert_eq!(get(k), None, "{k} is kept as inherited");
        } else {
            assert_eq!(get(k), Some(None), "{k} must be removed");
        }
    }
    // Opposed: an agent's environment removes nothing it inherits.
    let mut agent = loose();
    agent.completion = None;
    let e = SpawnConfig::from_settings("uuid-f", &agent, vec![]).env_from(&SpawnLogin::machine_default(), inherited);
    assert!(!e.iter().any(|(k, _)| k == "SSH_AUTH_SOCK" || k == "CLAUDE_CODE_MAX_OUTPUT_TOKENS"));
}

#[test]
fn a_completion_setting_is_absent_unless_named_and_round_trips() {
    let plain = serde_json::to_value(ThreadSettings::new("/w", "sonnet")).unwrap();
    assert!(plain.get("completion").is_none(), "{plain}");
    let s: ThreadSettings = serde_json::from_value(json!({"cwd": "/w", "model": "sonnet", "completion": {"max_output_tokens": 4000}})).unwrap();
    assert_eq!(s.completion, Some(Completion { max_output_tokens: 4000 }));
    assert_eq!(serde_json::to_value(&s).unwrap()["completion"], json!({"max_output_tokens": 4000}));
    assert!(Completion { max_output_tokens: 0 }.check().is_err());
    assert!(Completion { max_output_tokens: Completion::MAX_OUTPUT_TOKENS + 1 }.check().is_err());
    assert!(Completion { max_output_tokens: 1 }.check().is_ok());
}

fn capture_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude-capture"))
}

/// The binary the daemon spawns: record the environment's names, its
/// working directory (resolved, and what is in it) and stdin, then become
/// `fake-claude-capture` (argv, token, `--mcp-config` file).
fn write_wrapper(tmp: &Path) -> PathBuf {
    let w = tmp.join("claude-wrapper");
    let (envf, stdin, cwd) = (tmp.join("child.env"), tmp.join("child.stdin"), tmp.join("child.cwd"));
    std::fs::write(
        &w,
        format!(
            "#!/bin/bash\n/usr/bin/env > '{}'\n/bin/pwd -P > '{}'\n/bin/ls -A >> '{}'\n/bin/cat > '{}'\nexec '{}' \"$@\" < '{}'\n",
            envf.display(),
            cwd.display(),
            cwd.display(),
            stdin.display(),
            capture_fixture().display(),
            stdin.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&w, std::fs::Permissions::from_mode(0o755)).unwrap();
    w
}

/// **One turn of a completion thread, through the daemon**: the process it
/// spawns has the completion argv, no run token, no MCP config file, an
/// environment of kept names only (a planted variable is gone), its cwd the
/// thread's, and stdin exactly the text sent (no preamble, no header).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_completion_turn_spawns_claude_with_nothing_attached() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("CAULDRON_HOME", tmp.path().join("cauldron"));
    std::env::set_var("THREADS_CLAUDE_BIN", write_wrapper(tmp.path()));
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
    std::env::set_var("PLANTED_NAME_OF_THE_OWNER", "planted-value");
    std::env::set_var("THREADS_RUN_TOKEN", "inherited-token");
    threads::clock::testing::ticking();

    let login = tmp.path().join("aa-login");
    std::fs::create_dir_all(&login).unwrap();
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert("aa-one".to_string(), login.to_string_lossy().to_string());
    let m = build_manager(Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
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

    let work = tmp.path().join("empty-work");
    std::fs::create_dir_all(&work).unwrap();
    let mut s = loose();
    s.cwd = work.to_string_lossy().to_string();
    s.account = serde_json::from_value(json!({"named": "aa-one"})).unwrap();
    let id = m.create_thread(None, s).unwrap();
    let cap = tmp.path().join("cap");
    let text = format!("capture={} the dump text, exactly", cap.display());
    m.send(&id, text.clone(), threads::SendIntent::Immediate, Value::Null, false).unwrap();
    let done = PathBuf::from(format!("{}.done", cap.display()));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20) + first_exec::FIRST_EXEC_BUDGET;
    while !done.exists() {
        assert!(tokio::time::Instant::now() < deadline, "the turn never ran the fixture");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let p = |ext: &str| PathBuf::from(format!("{}.{ext}", cap.display()));
    let argv: Vec<String> = std::fs::read_to_string(p("argv")).unwrap().lines().map(str::to_string).collect();
    assert_completion_argv(&argv, &id);
    assert!(!p("token").exists(), "a completion turn holds no run token");
    assert!(!p("mcp").exists(), "no MCP config file");
    let env_text = std::fs::read_to_string(tmp.path().join("child.env")).unwrap();
    let names: Vec<&str> = env_text.lines().filter_map(|l| l.split_once('=').map(|x| x.0)).collect();
    let set_by_daemon = [
        "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
        "DISABLE_TELEMETRY",
        "DISABLE_ERROR_REPORTING",
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
        "CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS",
        "CLAUDE_ASYNC_AGENT_STALL_TIMEOUT_MS",
        "CLAUDE_CONFIG_DIR",
    ];
    // What bash itself sets for the wrapper script.
    let shell_own = ["PWD", "SHLVL", "_", "OLDPWD"];
    for n in &names {
        assert!(
            COMPLETION_ENV_KEPT.contains(n) || set_by_daemon.contains(n) || shell_own.contains(n),
            "`{n}` reached a completion's child: {names:?}"
        );
    }
    assert!(env_text.contains("CLAUDE_CODE_MAX_OUTPUT_TOKENS=8000"), "{env_text}");
    assert!(env_text.lines().any(|l| l == format!("CLAUDE_CONFIG_DIR={}", login.display())), "the pool login: {env_text}");
    assert!(!env_text.contains("planted-value") && !env_text.contains("inherited-token"));
    assert!(env_text.lines().any(|l| l == format!("PWD={}", work.display()) || l.starts_with("PWD=/private") && l.ends_with("empty-work")), "cwd: {env_text}");
    // The working directory: the thread's, empty when `claude` started, and
    // neither it nor any directory above it, up to the scratch root, holds a
    // CLAUDE.md (nor CLAUDE.local.md, nor .claude/CLAUDE.md).
    let cwd_text = std::fs::read_to_string(tmp.path().join("child.cwd")).unwrap();
    let mut lines = cwd_text.lines();
    let cwd = PathBuf::from(lines.next().unwrap());
    assert_eq!(cwd, work.canonicalize().unwrap(), "{cwd_text}");
    assert_eq!(lines.count(), 0, "the working directory was empty: {cwd_text}");
    let root = tmp.path().canonicalize().unwrap();
    assert!(cwd.starts_with(&root), "{cwd:?} under {root:?}");
    let mut seen = 0;
    for dir in cwd.ancestors().take_while(|d| d.starts_with(&root)) {
        for name in ["CLAUDE.md", "CLAUDE.local.md", ".claude/CLAUDE.md"] {
            assert!(!dir.join(name).exists(), "{} reaches the completion's working directory", dir.join(name).display());
        }
        seen += 1;
    }
    assert_eq!(seen, 2, "the working directory and the scratch root were both checked");
    // (threads ends what it writes to stdin with one newline)
    assert_eq!(std::fs::read_to_string(tmp.path().join("child.stdin")).unwrap(), format!("{text}\n"), "the model sees exactly the text sent");
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
