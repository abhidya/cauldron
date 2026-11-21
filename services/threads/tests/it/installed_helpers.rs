//! **The account helpers run from an INSTALLED layout** (issue #30).
//!
//! `add-account` and `claude-accounts` are copied, as the installer lays them
//! down, to `<root>/share/cauldron/scripts/threads/`, with no source checkout
//! anywhere near them, and run against a FAKE `threads` that records each call.
//! They must run that `threads` (the one the daemon names with
//! `CAULDRON_THREADS_BIN`, or the one cauldron pins), never `cargo`, and only
//! against the store they are told (`CLAUDE_ACCOUNTS_HOME`).
//!
//! SAFETY: every run has a scratch `HOME`, a cleared environment and a PATH
//! whose first directory holds fakes of `security`, `cargo`, `claude` and `git`
//! that record the call and fail, so nothing here can reach a keychain, a
//! build, a Claude account or a repository; each case asserts none was called.
//! The fake `threads` only appends to a log (and prints an empty inventory).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Layout {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    store: PathBuf,
    fakes: PathBuf,
    threads: PathBuf,
    log: PathBuf,
    forbidden: PathBuf,
}

fn exe(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Layout {
    fn new() -> Layout {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        let root = t.join("root");
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts");
        for name in ["add-account", "claude-accounts", "claude-login-browser"] {
            let to = root.join(threads::helpers::INSTALLED_DIR).join(name);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(src.join(name), &to).unwrap();
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (home, store, fakes) = (t.join("home"), t.join("root/data/prod/threads/store"), t.join("fakes"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&store).unwrap();
        let (log, forbidden) = (t.join("threads.log"), t.join("forbidden.log"));
        for name in ["security", "cargo", "claude", "git"] {
            exe(&fakes.join(name), &format!("#!/bin/sh\necho \"{name} $*\" >> '{}'\nexit 97\n", forbidden.display()));
        }
        let threads = t.join("fake-threads/threads");
        exe(
            &threads,
            &format!(
                "#!/bin/sh\necho \"$THREADS_HOME|$*\" >> '{}'\ncase \"$*\" in 'accounts health --json') echo '{{\"slots\": []}}' ;; esac\nexit 0\n",
                log.display()
            ),
        );
        Layout { _tmp: tmp, root, home, store, fakes, threads, log, forbidden }
    }

    fn script(&self, name: &str) -> PathBuf {
        self.root.join(threads::helpers::INSTALLED_DIR).join(name)
    }

    /// Run an installed helper with a cleared environment plus `vars`.
    fn run(&self, name: &str, args: &[&str], vars: &[(&str, &Path)]) -> (i32, String) {
        self.run_from(&self.home, "", name, args, vars)
    }

    /// [`Layout::run`] from `cwd`, with `path_head` put first on PATH.
    fn run_from(&self, cwd: &Path, path_head: &str, name: &str, args: &[&str], vars: &[(&str, &Path)]) -> (i32, String) {
        let mut c = Command::new("/bin/bash");
        c.arg(self.script(name)).args(args).env_clear().current_dir(cwd);
        c.env("HOME", &self.home);
        c.env("PATH", format!("{path_head}{}:/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin", self.fakes.display()));
        c.env("USER", "someone");
        for (k, v) in vars {
            c.env(k, v);
        }
        let o = c.output().unwrap();
        (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log).unwrap_or_default().lines().map(str::to_string).collect()
    }

    fn forbidden(&self) -> String {
        std::fs::read_to_string(&self.forbidden).unwrap_or_default()
    }
}

/// `add-account` (capture) and `claude-accounts list` run the `threads` the
/// daemon names, against the named store, and nothing else.
#[test]
fn installed_helpers_run_the_threads_they_are_given_against_the_named_store() {
    let l = Layout::new();
    let store = l.store.display().to_string();

    let (rc, out) = l.run("add-account", &["work-2"], &[("CAULDRON_THREADS_BIN", &l.threads), ("CLAUDE_ACCOUNTS_HOME", &l.store)]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(l.calls(), [format!("{store}|accounts add work-2 --clone-session")], "{out}");
    assert!(l.store.join("accounts/.gitignore").is_file(), "the store keeps its catch-all ignore rule");

    let (rc, out) = l.run("claude-accounts", &["list"], &[("CAULDRON_THREADS_BIN", &l.threads), ("CLAUDE_ACCOUNTS_HOME", &l.store)]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(l.calls().last().unwrap(), &format!("{store}|accounts health --json"), "{out}");
    assert_eq!(l.forbidden(), "", "no keychain, cargo, claude or git call");
}

/// With no `CAULDRON_THREADS_BIN`, an installed copy runs the threads image
/// the cauldron installed beside it pins, never a build.
#[test]
fn an_installed_copy_finds_the_threads_cauldron_pins() {
    let l = Layout::new();
    let label = "pinned-release+prod";
    let image = l.root.join("data/shared/builds/threads").join(label).join("threads");
    std::fs::create_dir_all(image.parent().unwrap()).unwrap();
    std::fs::copy(&l.threads, &image).unwrap();
    std::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o755)).unwrap();
    let status = format!(
        r#"{{"environment": {{"root": "{}"}}, "services": [{{"service": "threads", "pinned": "{label}"}}]}}"#,
        l.root.display()
    );
    exe(&l.root.join("bin/cauldron"), &format!("#!/bin/sh\n[ \"$*\" = 'status --json threads' ] || exit 64\necho '{status}'\n"));

    let (rc, out) = l.run("add-account", &["work-3"], &[("CLAUDE_ACCOUNTS_HOME", &l.store)]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(l.calls(), [format!("{}|accounts add work-3 --clone-session", l.store.display())], "{out}");
    let (rc, out) = l.run("claude-accounts", &["list"], &[("CLAUDE_ACCOUNTS_HOME", &l.store)]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(l.calls().len(), 2, "{out}");
    assert_eq!(l.forbidden(), "");
}

/// Refused, with nothing run: an installed copy told no store, and one with
/// no threads to run (no `CAULDRON_THREADS_BIN`, no cauldron beside it). No
/// `cargo` either way.
#[test]
fn an_installed_copy_refuses_without_a_store_or_a_threads_and_never_builds() {
    let l = Layout::new();
    let (rc, out) = l.run("add-account", &["work-2"], &[("CAULDRON_THREADS_BIN", &l.threads)]);
    assert_ne!(rc, 0, "{out}");
    assert!(out.contains("CLAUDE_ACCOUNTS_HOME"), "{out}");
    let (rc, out) = l.run("claude-accounts", &["list"], &[("CAULDRON_THREADS_BIN", &l.threads)]);
    assert_ne!(rc, 0, "{out}");
    assert!(out.contains("account store") && out.contains("CLAUDE_ACCOUNTS_HOME"), "{out}");

    let (rc, out) = l.run("add-account", &["work-2"], &[("CLAUDE_ACCOUNTS_HOME", &l.store)]);
    assert_ne!(rc, 0, "{out}");
    assert!(out.contains("no threads binary to run"), "{out}");
    let rel = Path::new("relative/threads");
    let (rc, out) = l.run("add-account", &["work-2"], &[("CLAUDE_ACCOUNTS_HOME", &l.store), ("CAULDRON_THREADS_BIN", rel)]);
    assert_ne!(rc, 0, "{out}");
    assert!(out.contains("is not an absolute path"), "{out}");

    assert!(l.calls().is_empty(), "nothing ran: {:?}", l.calls());
    assert_eq!(l.forbidden(), "", "no cargo, keychain, claude or git call");
}

/// **Nothing planted where a helper is started is used.** Each helper is
/// started from a project directory holding a `json.py` and programs named
/// `security`, `claude`, `git`, `cauldron` and `open`, with `.` first on PATH
/// and `PYTHONPATH`/`PYTHONSTARTUP` naming that directory: the reproduction
/// of a planted module taking the keychain secret. Every python the helpers
/// run is isolated (`-I`) and PATH keeps only absolute entries, so none of
/// it runs, while the helpers still do their work.
#[test]
fn a_module_or_program_planted_where_a_helper_starts_is_never_used() {
    let l = Layout::new();
    let project = l.home.join("project");
    let marker = l.home.join("planted-ran");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("json.py"),
        format!("open({:?}, 'a').write('json.py imported\\n')\nfrom importlib import import_module\n", marker.display().to_string()),
    )
    .unwrap();
    std::fs::write(project.join("sitecustomize.py"), format!("open({:?}, 'a').write('sitecustomize imported\\n')\n", marker.display().to_string())).unwrap();
    for name in ["security", "claude", "git", "cauldron", "open", "python3"] {
        exe(&project.join(name), &format!("#!/bin/sh\necho 'planted {name} ran' >> '{}'\nexit 0\n", marker.display()));
    }
    let py_vars: [(&str, &Path); 2] = [("PYTHONPATH", &project), ("PYTHONSTARTUP", &project.join("json.py"))];
    let planted = || std::fs::read_to_string(&marker).unwrap_or_default();

    // claude-accounts: `list` renders through python.
    let mut vars = vec![("CAULDRON_THREADS_BIN", l.threads.as_path()), ("CLAUDE_ACCOUNTS_HOME", l.store.as_path())];
    vars.extend(py_vars);
    let (rc, out) = l.run_from(&project, ".:", "claude-accounts", &["list"], &vars);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("claude-accounts login"), "the listing still renders: {out}");
    assert_eq!(planted(), "", "claude-accounts used something planted in its directory");

    // add-account: an installed copy reads `cauldron status --json` through
    // python to find its threads.
    let label = "pinned-release+prod";
    let image = l.root.join("data/shared/builds/threads").join(label).join("threads");
    std::fs::create_dir_all(image.parent().unwrap()).unwrap();
    std::fs::copy(&l.threads, &image).unwrap();
    std::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o755)).unwrap();
    let status = format!(
        r#"{{"environment": {{"root": "{}"}}, "services": [{{"service": "threads", "pinned": "{label}"}}]}}"#,
        l.root.display()
    );
    exe(&l.root.join("bin/cauldron"), &format!("#!/bin/sh\n[ \"$*\" = 'status --json threads' ] || exit 64\necho '{status}'\n"));
    let mut vars = vec![("CLAUDE_ACCOUNTS_HOME", l.store.as_path())];
    vars.extend(py_vars);
    let (rc, out) = l.run_from(&project, ".:", "add-account", &["work-9"], &vars);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(l.calls().last().unwrap(), &format!("{}|accounts add work-9 --clone-session", l.store.display()), "{out}");
    assert_eq!(planted(), "", "add-account used something planted in its directory");
    assert_eq!(l.forbidden(), "", "no keychain, cargo, claude or git call");

    // claude-login-browser hands a URL to a browser, so it is not run here
    // (that would open one); every program it starts is named by an absolute
    // path, and its PATH keeps only absolute entries.
    let browser = std::fs::read_to_string(l.script("claude-login-browser")).unwrap();
    let code: Vec<&str> = browser.lines().map(str::trim_start).filter(|t| !t.starts_with('#')).collect();
    assert!(code.iter().any(|t| t.starts_with("PATH=\"${_safe_path:-")), "claude-login-browser keeps only absolute PATH entries");
    for t in &code {
        if let Some(rest) = t.strip_prefix("exec ") {
            assert!(rest.starts_with('/'), "claude-login-browser execs a program found through PATH: {t}");
        }
    }

    // Every python every shipped helper runs is isolated.
    for name in ["add-account", "claude-accounts", "claude-login-browser"] {
        let text = std::fs::read_to_string(l.script(name)).unwrap();
        for line in text.lines().map(str::trim_start).filter(|t| !t.starts_with('#')) {
            for (i, _) in line.match_indices("python3 ") {
                let after = &line[i + "python3 ".len()..];
                let invocation = after.starts_with('-') && !after.starts_with("-I");
                assert!(!invocation, "{name}: python3 run without -I: {line}");
            }
        }
    }
}
