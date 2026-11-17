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
//! - **G5** `cauldron rename` moves the directory (its soup and dumps with
//!   it), rewrites the manifest's name, carries the grant to the new name
//!   with its projects, `granted_at` and the cauldron's identity, so it still
//!   opens the renamed cauldron and never one made later under the old name;
//!   a grant left under the new name by a cauldron that is gone is dropped.
//!   It says the `.cauldron` marker is not updated.
//! - **G6** `rename` and `remove` are refused while the `mcp` service holds
//!   the cauldron's soup or dumps store (naming `cauldron mcp revoke`), for
//!   the legacy cauldron, for a name taken, and while its soup holds a live
//!   tranche; nothing moves and the grant is untouched. Each is opposed by
//!   the same verb going through once the cause is gone.
//! - **G7** `remove` without `--yes` is a dry run (rc 2, nothing moved, the
//!   grant kept, and it says the grant would be dropped); with `--yes` it
//!   moves the cauldron to `cauldrons/.removed/` and drops its grant, and a
//!   cauldron made again under the name is not opened by anything.
//! - **G8** a grant that names its cauldron only by name (the pre-release
//!   shape) is named BEFORE any call is refused: `mcp grants` lists it under
//!   `warnings` and `mcp status` on its cauldron (`grant_note`), each with
//!   the command that grants it again, and each says so on stderr; a bound
//!   grant beside it is not named, and nothing is written.

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

// ------------------------------------------------------- rename and remove

fn env_dev(s: &Scratch) -> env::Environment {
    env::Environment {
        root: s.root(),
        root_source: env::paths::Source::CauldronHome,
        name: "dev".into(),
        name_source: env::paths::Source::Default,
        data_root: s.root().join("data").join("dev"),
    }
}

fn cauldron_dir(s: &Scratch, name: &str) -> PathBuf {
    s.root().join("data").join("dev").join("cauldrons").join(name)
}

/// The `key = "value"` of a cauldron's `cauldron.toml`.
fn manifest_field(s: &Scratch, name: &str, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(cauldron_dir(s, name).join("cauldron.toml")).ok()?;
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}

fn store_json(s: &Scratch) -> Value {
    serde_json::from_str(&std::fs::read_to_string(s.store()).unwrap()).unwrap()
}

/// Can the MCP door reach `name` with the read scope (`Grants::authorize`,
/// what the service asks on every call)? `Err` carries the refusal code.
fn opens(s: &Scratch, name: &str) -> Result<(), String> {
    let e = env_dev(s);
    let g = soup_door::grants::Grants::load(&e).map_err(|e| format!("{:?}", e.code))?;
    g.authorize(&e, name, soup_door::grants::Scope::Read).map(|_| ()).map_err(|e| format!("{:?}", e.code))
}

/// One dump in `name`'s dumps store, through the CLI.
fn dump_into(s: &Scratch, name: &str) {
    let f = s.0.join(format!("note-{name}.txt"));
    std::fs::write(&f, "a note kept as a dump\n").unwrap();
    let o = run(s, &["soup", "dump", f.to_str().unwrap(), "--env", "dev", "--cauldron", name]);
    assert_eq!(o.rc, 0, "dump: {}\n{}", o.out, o.err);
}

fn dump_count(s: &Scratch, name: &str) -> usize {
    let o = run(s, &["soup", "dumps", "--env", "dev", "--cauldron", name]);
    assert_eq!(o.rc, 0, "dumps: {}\n{}", o.out, o.err);
    let v: Value = serde_json::from_str(&o.out).unwrap_or_else(|e| panic!("not JSON ({e}): {}", o.out));
    v["dumps"].as_array().map(|a| a.len()).unwrap_or_else(|| panic!("no dumps list: {v}"))
}

#[test]
fn g5_rename_carries_the_soup_the_dumps_and_the_grant() {
    let s = scratch("g5", &["kb", "other"]);
    let proj = s.dir("proj");
    let p = proj.to_str().unwrap();
    assert_eq!(on_tty(&s, &["mcp", "grant", "kb", "--env", "dev", "--project", p]).rc, 0);
    dump_into(&s, "kb");
    let created = manifest_field(&s, "kb", "created_at").expect("created_at");
    let before = store_json(&s)["grants"][0].clone();
    assert_eq!(before["cauldron_created_at"], created.as_str(), "the grant was not bound at grant time");
    // A grant left under the new name by a cauldron that is gone, recording
    // no identity: it would open the renamed cauldron by name.
    let mut v = store_json(&s);
    v["grants"].as_array_mut().unwrap().push(serde_json::json!({
        "cauldron": "kb2", "scopes": ["read"], "projects": [], "granted_at": "2026-01-01T00:00:00Z", "by": "tty"
    }));
    std::fs::write(s.store(), serde_json::to_string_pretty(&v).unwrap()).unwrap();

    let o = run(&s, &["rename", "kb", "kb2", "--env", "dev"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert!(o.out.contains("renamed cauldron `kb` to `kb2`"), "{}", o.out);
    assert!(o.out.contains("its MCP grant now names `kb2`"), "{}", o.out);
    assert!(o.out.contains("`.cauldron` marker") && o.out.contains("names `kb` is not updated"), "{}", o.out);
    assert!(!cauldron_dir(&s, "kb").exists(), "the old directory is still there");
    assert!(cauldron_dir(&s, "kb2").join("soup").join("spine.db").is_file(), "the soup did not move");
    assert_eq!(manifest_field(&s, "kb2", "name").as_deref(), Some("kb2"));
    assert_eq!(manifest_field(&s, "kb2", "created_at").as_deref(), Some(created.as_str()), "the identity changed");
    // The dumps moved with it.
    assert!(cauldron_dir(&s, "kb2").join("dumps").is_dir(), "the dumps store did not move");
    assert_eq!(dump_count(&s, "kb2"), 1);

    // One grant under the new name: the carried one, as it was.
    let g = store_json(&s)["grants"].as_array().unwrap().clone();
    let named: Vec<&Value> = g.iter().filter(|x| x["cauldron"] == "kb2").collect();
    assert_eq!(named.len(), 1, "the stale grant under the new name was kept: {g:?}");
    assert!(!g.iter().any(|x| x["cauldron"] == "kb"), "a grant still names `kb`: {g:?}");
    for k in ["scopes", "projects", "granted_at", "by", "cauldron_created_at"] {
        assert_eq!(named[0][k], before[k], "{k} changed");
    }
    assert_eq!(opens(&s, "kb2"), Ok(()), "the grant does not open the renamed cauldron");
    // A cauldron made again under the old name is not opened by it.
    assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).rc, 0);
    assert_eq!(opens(&s, "kb"), Err("NotGranted".into()));
    // Resolution by project follows the new name.
    let e = env_dev(&s);
    let r = soup_door::grants::Grants::load(&e).unwrap().resolve("dev", Some(&proj), None, None).unwrap();
    assert_eq!(r.cauldron, "kb2");

    // Opposed: a cauldron with no grant renames, and says so.
    let o = run(&s, &["rename", "other", "other2", "--env", "dev"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert!(o.out.contains("it has no MCP grant"), "{}", o.out);
}

#[test]
fn g6_rename_and_remove_are_refused_while_something_holds_the_cauldron() {
    let s = scratch("g6", &["kb", "taken"]);
    assert_eq!(on_tty(&s, &["mcp", "grant", "kb", "--env", "dev"]).rc, 0);
    dump_into(&s, "kb");
    let store_before = std::fs::read(s.store()).unwrap();
    let unchanged = |s: &Scratch| {
        assert!(cauldron_dir(s, "kb").join("cauldron.toml").is_file(), "the cauldron moved");
        assert_eq!(manifest_field(s, "kb", "name").as_deref(), Some("kb"));
        assert_eq!(std::fs::read(s.store()).unwrap(), store_before, "the grant store changed");
        assert!(!s.root().join("data/dev/cauldrons/.removed").exists(), "something was put away");
    };

    // The `mcp` service holds the soup, then the dumps store: the locks are
    // held here as the service holds them (flock, with its text).
    for lock in [cauldron_dir(&s, "kb").join("soup/writer.lock"), cauldron_dir(&s, "kb").join("dumps/writer.lock")] {
        let fh = std::fs::OpenOptions::new().create(true).truncate(true).write(true).open(&lock).unwrap();
        fh.try_lock().unwrap();
        std::fs::write(&lock, "mcp 4242\n").unwrap();
        for args in [&["rename", "kb", "kb2", "--env", "dev"][..], &["remove", "kb", "--env", "dev", "--yes"][..]] {
            let o = run(&s, args);
            assert_ne!(o.rc, 0, "{args:?} went through while {} was held: {}", lock.display(), o.out);
            assert!(o.err.contains("the `mcp` service (pid 4242) holds cauldron `kb`'s"), "{}", o.err);
            assert!(o.err.contains("cauldron mcp revoke kb --env dev"), "{}", o.err);
            unchanged(&s);
        }
        // Held by another writer (no `mcp <pid>` text): refused, not named as the service.
        std::fs::write(&lock, "").unwrap();
        let o = run(&s, &["rename", "kb", "kb2", "--env", "dev"]);
        assert!(o.rc != 0 && o.err.contains("another writer"), "{}\n{}", o.out, o.err);
        unchanged(&s);
        drop(fh);
    }

    // A live tranche in its soup.
    let db = cauldron_dir(&s, "kb").join("soup/spine.db");
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute(
            "INSERT INTO tranches (id, attempt, seq, state, lock_mode, holder_service, key_hash, regions, began_at, \
             last_heartbeat, deadline) VALUES ('T-test', 1, 1, 'open', 'whole', 'test', 'k', '[]', 0, 0, 0)",
            [],
        )
        .unwrap();
    }
    for args in [&["rename", "kb", "kb2", "--env", "dev"][..], &["remove", "kb", "--env", "dev", "--yes"][..]] {
        let o = run(&s, args);
        assert!(o.rc != 0 && o.err.contains("live tranche"), "{args:?}: {}\n{}", o.out, o.err);
        unchanged(&s);
    }
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute("UPDATE tranches SET state = 'landed'", []).unwrap();
    }

    // A name taken, the environment's own name, a bad name.
    let o = run(&s, &["rename", "kb", "taken", "--env", "dev"]);
    assert!(o.rc != 0 && o.err.contains("already exists"), "{}", o.err);
    let o = run(&s, &["rename", "kb", "dev", "--env", "dev"]);
    assert!(o.rc != 0 && o.err.contains("own name"), "{}", o.err);
    let o = run(&s, &["rename", "kb", "Bad Name", "--env", "dev"]);
    assert_ne!(o.rc, 0, "{}", o.out);
    unchanged(&s);

    // The legacy cauldron (its data in the environment's data root).
    std::fs::create_dir_all(s.root().join("data/dev/soup/cauldron")).unwrap();
    let o = run(&s, &["rename", "dev", "x", "--env", "dev"]);
    assert!(o.rc != 0 && o.err.contains("legacy cauldron"), "{}", o.err);
    let o = run(&s, &["remove", "dev", "--env", "dev", "--yes"]);
    assert!(o.rc != 0 && o.err.contains("legacy cauldron"), "{}", o.err);
    assert!(s.root().join("data/dev/soup/cauldron").is_dir(), "the legacy data moved");
    unchanged(&s);

    // Opposed: with every cause gone, the same rename goes through.
    let o = run(&s, &["rename", "kb", "kb2", "--env", "dev"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert_eq!(opens(&s, "kb2"), Ok(()));
}

#[test]
fn g7_remove_is_a_dry_run_without_yes_and_never_deletes() {
    let s = scratch("g7", &["kb"]);
    assert_eq!(on_tty(&s, &["mcp", "grant", "kb", "--env", "dev"]).rc, 0);
    dump_into(&s, "kb");
    let store_before = std::fs::read(s.store()).unwrap();

    let o = run(&s, &["remove", "kb", "--env", "dev"]);
    assert_eq!(o.rc, 2, "{}\n{}", o.out, o.err);
    assert!(o.out.contains("DRY RUN"), "{}", o.out);
    assert!(o.out.contains("MCP grant      dropped"), "{}", o.out);
    assert!(cauldron_dir(&s, "kb").join("cauldron.toml").is_file(), "a dry run moved it");
    assert_eq!(std::fs::read(s.store()).unwrap(), store_before, "a dry run changed the grants");
    assert_eq!(opens(&s, "kb"), Ok(()));

    let o = run(&s, &["remove", "kb", "--env", "dev", "--yes"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert!(o.out.contains("its MCP grant was dropped"), "{}", o.out);
    assert!(o.out.contains("names `kb` is not updated"), "{}", o.out);
    assert!(!cauldron_dir(&s, "kb").exists());
    let removed: Vec<PathBuf> = std::fs::read_dir(s.root().join("data/dev/cauldrons/.removed"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(removed.len(), 1, "{removed:?}");
    let name = removed[0].file_name().unwrap().to_str().unwrap().to_string();
    assert!(name.starts_with("kb-") && name.ends_with('Z'), "{name}");
    // Never deleted: the soup and the dumps are there, as they were.
    assert!(removed[0].join("soup/spine.db").is_file());
    assert!(removed[0].join("dumps").is_dir());
    assert!(store_json(&s)["grants"].as_array().unwrap().is_empty(), "the grant was kept");
    // A cauldron made again under the name is closed.
    assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).rc, 0);
    assert_eq!(opens(&s, "kb"), Err("NotGranted".into()));
    // Opposed: removing a cauldron that does not exist is refused.
    let o = run(&s, &["remove", "nope", "--env", "dev", "--yes"]);
    assert!(o.rc != 0 && o.err.contains("no cauldron `nope`"), "{}", o.err);
}

#[test]
fn g8_a_name_only_grant_is_named_before_its_first_call_with_its_regrant() {
    let s = scratch("g8", &["kb", "kb2"]);
    let e = env_dev(&s);
    let proj = s.dir("a dir");
    let p = soup_door::grants::canonical_project(&proj).unwrap();
    let scopes = [soup_door::grants::Scope::Read, soup_door::grants::Scope::Dump];
    soup_door::grants::grant(&e, "kb", &scopes, std::slice::from_ref(&p), "tty").unwrap();
    soup_door::grants::grant(&e, "kb2", &scopes, &[], "tty").unwrap();
    // `kb`'s grant as a pre-release build wrote it: no cauldron identity.
    let mut store = store_json(&s);
    for g in store["grants"].as_array_mut().unwrap() {
        if g["cauldron"] == "kb" {
            g.as_object_mut().unwrap().remove("cauldron_created_at");
        }
    }
    std::fs::write(s.store(), serde_json::to_string_pretty(&store).unwrap()).unwrap();
    let before = std::fs::read(s.store()).unwrap();
    let regrant = format!("cauldron mcp grant kb --env dev --scopes read,dump --project '{}'", p);

    let o = run(&s, &["mcp", "grants", "--env", "dev"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let w = o.json()["warnings"].clone();
    assert_eq!(w.as_array().map(Vec::len), Some(1), "{w}");
    assert_eq!((w[0]["cauldron"].as_str(), w[0]["regrant"].as_str()), (Some("kb"), Some(regrant.as_str())), "{w}");
    assert!(o.err.contains("WARNING") && o.err.contains("only by name") && o.err.contains(&regrant), "{}", o.err);
    assert!(!o.err.contains("kb2"), "{}", o.err);

    let o = run(&s, &["mcp", "status", "--env", "dev"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let d = o.json();
    let row = |name: &str| d["cauldrons"].as_array().unwrap().iter().find(|c| c["cauldron"] == name).cloned().unwrap();
    assert!(row("kb")["grant_note"].as_str().is_some_and(|n| n.contains("refused until it is granted again")), "{d}");
    assert_eq!(row("kb")["regrant"].as_str(), Some(regrant.as_str()));
    assert!(row("kb2").get("grant_note").is_none(), "{d}");
    assert!(o.err.contains("WARNING") && o.err.contains(&regrant), "{}", o.err);
    // The refusal it warns of.
    assert_eq!(opens(&s, "kb"), Err("NotGranted".into()));
    assert_eq!(opens(&s, "kb2"), Ok(()));
    assert_eq!(std::fs::read(s.store()).unwrap(), before, "nothing was written");
}
