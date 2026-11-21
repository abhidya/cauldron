//! R7.S2 (DEC-37): `s` — set this machine's login — is the one LOCAL act, as
//! on the Python board (`threads_tui/local.py`): a `--dry-run` preflight,
//! then either recording the name (already live) or the real `--yes` switch
//! handed to the operator's terminal. The tool is a STUB here
//! (`TUI_POOL_ACCOUNTS_TOOL`); nothing in this file may touch the real
//! machine login. One test, because the env it sets is process-wide.

// REL-6a3 rule B: every child this test binary spawns inherits CAULDRON_ISOLATED=1.
test_isolation::install!();

use std::os::unix::fs::PermissionsExt;

use tokio::sync::mpsc;
use tui::pool::action::Action;
use tui::pool::model::Tone;
use tui::pool::{client, perform, Outcome};

fn stub(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    let p = dir.join("claude-accounts");
    let log = dir.join("calls.log");
    std::fs::write(
        &p,
        format!("#!/bin/sh\necho \"$*\" >> '{}'\n{body}\n", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_file(&log);
    p
}

fn calls(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("calls.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn s_runs_the_preflight_then_hands_over_or_records() {
    std::env::remove_var("CLAUDE_CONFIG_DIR");
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    // A handle to nowhere: the local act never uses the socket. V1-POOLTEST:
    // "nowhere" is the egress module's DEAD_PORT (1, privileged, the one port
    // an isolated process may name without a grant), not port 9, which nobody
    // granted — the isolation gate rightly refused it.
    let (tx, _rx) = mpsc::channel(8);
    let h = client::spawn(format!("ws://127.0.0.1:{}/ws", env::egress::DEAD_PORT), tx);
    let a = Action::SetLive("beta".into());

    // 1. The preflight reaches its dry-run stop → the real switch is handed
    //    to the terminal, and nothing else ran.
    let tool = stub(dir, r#"case "$*" in *--dry-run*) echo "--dry-run: stopping here";; esac"#);
    std::env::set_var("TUI_POOL_ACCOUNTS_TOOL", &tool);
    let out = perform(&h, &a).await;
    let t = tool.to_string_lossy().to_string();
    assert_eq!(out, Outcome::NeedsTerminal(vec![t.clone(), "set-live-login".into(), "beta".into(), "--yes".into()]));
    assert_eq!(calls(dir), ["set-live-login beta --dry-run"]);

    // 2. Already live → the no-flag run records the name; no switch.
    stub(dir, r#"echo "already-live: beta""#);
    let out = perform(&h, &a).await;
    assert!(matches!(&out, Outcome::Done(n, Tone::Good) if n.contains("already is")), "{out:?}");
    assert_eq!(calls(dir), ["set-live-login beta --dry-run", "set-live-login beta"]);

    // 3. A clean exit that never reached the dry-run stop checked nothing.
    stub(dir, "echo hello");
    let out = perform(&h, &a).await;
    assert!(matches!(&out, Outcome::Done(n, Tone::Bad) if n.contains("never reached")), "{out:?}");
    assert_eq!(calls(dir), ["set-live-login beta --dry-run"]);

    // 4. CLAUDE_CONFIG_DIR set → refused before the tool runs at all.
    stub(dir, r#"echo "--dry-run: stopping here""#);
    std::env::set_var("CLAUDE_CONFIG_DIR", "");
    let out = perform(&h, &a).await;
    std::env::remove_var("CLAUDE_CONFIG_DIR");
    assert!(matches!(&out, Outcome::Done(n, Tone::Bad) if n.contains("CLAUDE_CONFIG_DIR")), "{out:?}");
    assert!(calls(dir).is_empty(), "{:?}", calls(dir));

    // 5. Started from a project holding a planted `json.py`, with variables
    //    that would make a shell or python load a file: the helper runs from
    //    the store (or `/`), never that directory, and inherits none of them.
    let project = dir.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("json.py"), "raise SystemExit('planted')\n").unwrap();
    let where_ = dir.join("where");
    stub(dir, &format!(r#"echo "$(pwd -P)|${{PYTHONPATH:-}}|${{PYTHONSTARTUP:-}}|${{BASH_ENV:-}}|${{ENV:-}}|$PATH" > '{}'; echo "--dry-run: stopping here""#, where_.display()));
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(&project).unwrap();
    for (k, v) in [("PYTHONPATH", &project), ("PYTHONSTARTUP", &project.join("json.py")), ("BASH_ENV", &project.join("json.py")), ("ENV", &project.join("json.py"))] {
        std::env::set_var(k, v);
    }
    let path_before = std::env::var_os("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!(".:{}", path_before.to_string_lossy()));
    let out = perform(&h, &a).await;
    std::env::set_current_dir(&back).unwrap();
    std::env::set_var("PATH", &path_before);
    for k in ["PYTHONPATH", "PYTHONSTARTUP", "BASH_ENV", "ENV"] {
        std::env::remove_var(k);
    }
    assert!(matches!(&out, Outcome::NeedsTerminal(_)), "{out:?}");
    let seen = std::fs::read_to_string(&where_).unwrap();
    let f: Vec<&str> = seen.trim_end().split('|').collect();
    let project_real = std::fs::canonicalize(&project).unwrap();
    assert_ne!(std::path::Path::new(f[0]), project_real, "the helper ran from the project: {seen}");
    assert_eq!(&f[1..5], ["", "", "", ""], "a variable that loads a file reached the helper: {seen}");
    assert!(f[5].split(':').all(|d| d.starts_with('/')), "a relative PATH entry reached the helper: {seen}");
}
