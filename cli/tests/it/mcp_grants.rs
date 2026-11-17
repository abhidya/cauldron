//! **`cauldron mcp grant | revoke | grants`**: the cauldrons open to the MCP
//! door, kept in `<root>/data/<env>/mcp/grants.json`.
//!
//! Every test runs the built `cauldron` under a scratch `CAULDRON_HOME`/`HOME`
//! (never `~/.cauldron`; this binary exports `CAULDRON_ISOLATED=1`). A
//! terminal is given to `grant` by running it under `script -q /dev/null`,
//! which runs a command on a pseudo-terminal.
//!
//! - **G1** nothing is granted by default; without a terminal `grant` refuses,
//!   names the exact command to run, and writes nothing; on a terminal it
//!   grants, the store is owner-only, and `grants` lists it.
//! - **G2** a cauldron that does not exist, or a project that is not a
//!   directory, is refused and nothing is written.
//! - **G3** granting again adds projects and takes the new scopes; `revoke
//!   --project` removes only that binding (also for a directory since
//!   deleted); `revoke` removes the whole grant; revoking twice is harmless.
//! - **G4** two processes granting at once, 50 times each, lose no write.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

const CAULDRON: &str = env!("CARGO_BIN_EXE_cauldron");

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
impl Scratch {
    fn root(&self) -> PathBuf {
        self.0.join("root")
    }
    fn store(&self) -> PathBuf {
        self.root().join("data").join("dev").join("mcp").join("grants.json")
    }
    fn dir(&self, name: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}

/// `root/`, `home/`, and an environment `dev` with one cauldron per name.
fn scratch(tag: &str, cauldrons: &[&str]) -> Scratch {
    let p = std::env::temp_dir().join(format!(
        "cauldron-mcp-it-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    for d in ["root", "home"] {
        std::fs::create_dir_all(p.join(d)).unwrap();
    }
    let p = p.canonicalize().unwrap();
    if let Some(h) = std::env::var_os("HOME") {
        assert!(!p.starts_with(Path::new(&h).join(".cauldron")), "scratch under ~/.cauldron: {}", p.display());
    }
    let s = Scratch(p);
    std::fs::create_dir_all(s.root().join("data").join("dev")).unwrap();
    for c in cauldrons {
        let o = run(&s, &["create", c, "--env", "dev"]);
        assert_eq!(o.rc, 0, "create {c}: {}", o.err);
    }
    s
}

struct Out {
    rc: i32,
    out: String,
    err: String,
}
impl Out {
    fn json(&self) -> Value {
        // Under a pseudo-terminal, output lines end in \r\n, and `script`
        // may echo the ^D it sends when its own input ends.
        let text = self.out.replace("\r\n", "\n");
        let start = text.find('{').unwrap_or_else(|| panic!("no JSON: {}\n{}", self.out, self.err));
        serde_json::from_str(&text[start..]).unwrap_or_else(|e| panic!("not JSON ({e}): {}\n{}", self.out, self.err))
    }
}

fn command(s: &Scratch, program: &str, args: &[&str]) -> Command {
    let mut c = Command::new(program);
    c.args(args)
        .env("CAULDRON_HOME", s.root())
        .env("HOME", s.0.join("home"))
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .env_remove("CAULDRON_CAULDRON")
        .stdin(Stdio::null());
    c
}

fn out(c: &mut Command) -> Out {
    let o = c.output().expect("spawn");
    Out {
        rc: o.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&o.stdout).into_owned(),
        err: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

/// The verb with no terminal (stdin is /dev/null, stdout a pipe).
fn run(s: &Scratch, args: &[&str]) -> Out {
    out(&mut command(s, CAULDRON, args))
}

/// The verb on a pseudo-terminal: `script -q /dev/null cauldron <args>`.
/// Everything it prints arrives on stdout.
fn on_tty(s: &Scratch, args: &[&str]) -> Out {
    let mut full = vec!["-q", "/dev/null", CAULDRON];
    full.extend_from_slice(args);
    out(&mut command(s, "/usr/bin/script", &full))
}

fn grants(s: &Scratch) -> Vec<Value> {
    let o = run(s, &["mcp", "grants", "--env", "dev"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    o.json()["grants"].as_array().unwrap().clone()
}

fn projects(g: &Value) -> Vec<String> {
    g["projects"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect()
}

#[test]
fn g1_a_grant_needs_a_terminal_and_nothing_is_granted_before_it() {
    use std::os::unix::fs::PermissionsExt;
    let s = scratch("g1", &["kb"]);
    let proj = s.dir("proj");
    let p = proj.to_str().unwrap();
    assert!(grants(&s).is_empty(), "something was granted by default");

    // No terminal: refused, naming the exact command, and nothing written.
    let o = run(&s, &["mcp", "grant", "kb", "--env", "dev", "--project", p]);
    assert_ne!(o.rc, 0, "granted without a terminal: {}", o.out);
    assert!(o.err.contains("a grant is the owner's act at a terminal"), "{}", o.err);
    assert!(o.err.contains(&format!("cauldron mcp grant kb --env dev --scopes read,write,dump --project {p}")), "{}", o.err);
    assert!(o.out.is_empty(), "{}", o.out);
    assert!(!s.store().exists(), "a refused grant wrote the store");
    // Input from a terminal but output to a file is not enough either.
    let sink = s.0.join("sink");
    let line = format!("'{CAULDRON}' mcp grant kb --env dev > '{}' 2>&1", sink.display());
    let o = out(&mut command(&s, "/usr/bin/script", &["-q", "/dev/null", "/bin/sh", "-c", &line]));
    assert_eq!(o.rc, 1, "{}", o.out);
    assert!(std::fs::read_to_string(&sink).unwrap().contains("owner's act at a terminal"));
    assert!(!s.store().exists());

    // On a terminal: granted, owner-only, and listed.
    let o = on_tty(&s, &["mcp", "grant", "kb", "--env", "dev", "--project", p]);
    assert_eq!(o.rc, 0, "{}", o.out);
    let d = o.json();
    assert_eq!(d["outcome"], "granted");
    assert_eq!(d["grant"]["scopes"], serde_json::json!(["read", "write", "dump"]));
    assert_eq!(projects(&d["grant"]), [p]);
    assert_eq!(d["grant"]["by"], "tty");
    assert_eq!(std::fs::metadata(s.store()).unwrap().permissions().mode() & 0o777, 0o600);
    let g = grants(&s);
    assert_eq!(g.len(), 1);
    assert_eq!(g[0]["cauldron"], "kb");
}

#[test]
fn g2_an_unknown_cauldron_or_project_is_refused() {
    let s = scratch("g2", &["kb"]);
    let o = on_tty(&s, &["mcp", "grant", "nope", "--env", "dev"]);
    assert_ne!(o.rc, 0, "{}", o.out);
    assert!(o.out.contains("no cauldron `nope`"), "{}", o.out);
    // Refused before the terminal check, so it is refused the same without one.
    let o = run(&s, &["mcp", "grant", "nope", "--env", "dev"]);
    assert!(o.err.contains("no cauldron `nope`"), "{}", o.err);
    let o = on_tty(&s, &["mcp", "grant", "kb", "--env", "dev", "--project", s.0.join("missing").to_str().unwrap()]);
    assert_ne!(o.rc, 0, "{}", o.out);
    assert!(o.out.contains("cannot be resolved"), "{}", o.out);
    let o = on_tty(&s, &["mcp", "grant", "kb", "--env", "dev", "--scopes", "read,admin"]);
    assert_ne!(o.rc, 0, "{}", o.out);
    let o = on_tty(&s, &["mcp", "grant", "kb", "--env", "nowhere"]);
    assert_ne!(o.rc, 0, "{}", o.out);
    assert!(!s.store().exists(), "a refused grant wrote the store");
}

#[test]
fn g3_regrant_adds_projects_and_revoke_removes_one_binding_or_the_grant() {
    let s = scratch("g3", &["kb"]);
    let (a, b) = (s.dir("a"), s.dir("b"));
    let (pa, pb) = (a.to_str().unwrap(), b.to_str().unwrap());
    assert_eq!(on_tty(&s, &["mcp", "grant", "kb", "--env", "dev", "--project", pa]).rc, 0);
    let o = on_tty(&s, &["mcp", "grant", "kb", "--env", "dev", "--scopes", "read", "--project", pb]);
    assert_eq!(o.rc, 0, "{}", o.out);
    assert_eq!(o.json()["outcome"], "updated");
    let g = grants(&s);
    assert_eq!(projects(&g[0]), [pa, pb]);
    assert_eq!(g[0]["scopes"], serde_json::json!(["read"]));

    // One binding, by a path to it that is not the canonical one.
    let o = run(&s, &["mcp", "revoke", "kb", "--env", "dev", "--project", &format!("{pa}/../a")]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert_eq!(o.json()["outcome"], "unbound");
    assert_eq!(projects(&grants(&s)[0]), [pb]);
    // A directory since deleted can still be unbound by its stored path.
    std::fs::remove_dir(&b).unwrap();
    let o = run(&s, &["mcp", "revoke", "kb", "--env", "dev", "--project", pb]);
    assert_eq!(o.json()["outcome"], "unbound", "{}", o.out);
    assert!(projects(&grants(&s)[0]).is_empty());
    let o = run(&s, &["mcp", "revoke", "kb", "--env", "dev", "--project", pa]);
    assert_eq!(o.rc, 1);
    assert_eq!(o.json()["outcome"], "no-such-project");

    // The whole grant, then again.
    let o = run(&s, &["mcp", "revoke", "kb", "--env", "dev"]);
    assert_eq!(o.json()["outcome"], "revoked", "{}", o.out);
    assert!(grants(&s).is_empty());
    let o = run(&s, &["mcp", "revoke", "kb", "--env", "dev"]);
    assert_eq!((o.rc, o.json()["outcome"].as_str()), (0, Some("not-granted")));
}

#[test]
fn g4_concurrent_grants_lose_no_write() {
    const EACH: usize = 50;
    let s = scratch("g4", &["kb", "other"]);
    let mut dirs = Vec::new();
    for w in 0..2 {
        for i in 0..EACH {
            dirs.push((w, s.dir(&format!("p{w}-{i}")).to_str().unwrap().to_string()));
        }
    }
    let s = std::sync::Arc::new(s);
    let workers: Vec<_> = (0..2)
        .map(|w| {
            let s = s.clone();
            let mine: Vec<String> = dirs.iter().filter(|(x, _)| *x == w).map(|(_, d)| d.clone()).collect();
            std::thread::spawn(move || {
                // Worker 0 grants `kb`, worker 1 alternates, so the two write
                // the same grant as well as different ones.
                for (i, d) in mine.iter().enumerate() {
                    let c = if w == 0 || i % 2 == 0 { "kb" } else { "other" };
                    let o = on_tty(&s, &["mcp", "grant", c, "--env", "dev", "--project", d]);
                    assert_eq!(o.rc, 0, "worker {w} grant {i}: {}", o.out);
                }
            })
        })
        .collect();
    for w in workers {
        w.join().expect("a worker failed");
    }
    let g = grants(&s);
    let all: Vec<String> = g.iter().flat_map(projects).collect();
    assert_eq!(all.len(), 2 * EACH, "a write was lost: {} of {} projects", all.len(), 2 * EACH);
    for (_, d) in &dirs {
        assert!(all.contains(d), "{d} was lost");
    }
}
