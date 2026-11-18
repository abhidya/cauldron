//! **Clusters**: the soup's second pool (`cauldron`, grammar `cauldron@1`)
//! and the claims its clusters make on workspace regions, through the
//! command line.
//!
//! - **CL1** `cauldron create` makes both pools.
//! - **CL2** a store with only the workspace pool (as 0.1.4 to 0.1.14 make
//!   it) gets the cluster pool on its first cluster write, and on nothing
//!   else: a workspace-only import, a dry run and a claim leave it without.
//! - **CL3** with `$CAULDRON_OLDER_BIN` naming a 0.1.14 `cauldron`, a store
//!   that 0.1.14 made and this release then gave clusters still opens, reads
//!   and writes under 0.1.14, which does not see the clusters; this release
//!   reads both afterwards. Skipped (and says so) without the variable.
//! - **CL4** a batch holding a region, a cluster and the cluster's claim of
//!   that region lands as one write, all or nothing; a claim of a node that
//!   is not a region refuses the whole batch.
//! - **CL5** `cauldron soup claim` adds a claim, says so again without
//!   writing, releases it, and refuses an unknown cluster or a non-region.
//! - **CL6** the reads: `nodes --type cluster`, `region` (its `clusters`),
//!   `get CL-n` (its claims), `search` over cluster text, bounded by
//!   `--limit` and `--max-chars`.
//! - **CL7** the review flags are refused on a cluster record.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

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
    fn soup(&self, name: &str) -> PathBuf {
        self.root().join("data").join("dev").join("cauldrons").join(name).join("soup")
    }
    fn file(&self, name: &str, v: &Value) -> PathBuf {
        let p = self.0.join("in").join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, serde_json::to_string_pretty(v).unwrap()).unwrap();
        p
    }
}

fn scratch(tag: &str) -> Scratch {
    let p = std::env::temp_dir().join(format!(
        "cauldron-clusters-it-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    for d in ["root/data/dev", "home"] {
        std::fs::create_dir_all(p.join(d)).unwrap();
    }
    Scratch(p.canonicalize().unwrap())
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

fn run_with(bin: &str, s: &Scratch, args: &[&str]) -> Out {
    let o = Command::new(bin)
        .args(args)
        .env("CAULDRON_HOME", s.root())
        .env("HOME", s.0.join("home"))
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .env_remove("CAULDRON_CAULDRON")
        .stdin(Stdio::null())
        .output()
        .expect("spawn cauldron");
    Out { rc: o.status.code().unwrap_or(-1), out: String::from_utf8_lossy(&o.stdout).into_owned(), err: String::from_utf8_lossy(&o.stderr).into_owned() }
}

fn run(s: &Scratch, args: &[&str]) -> Out {
    run_with(CAULDRON, s, args)
}

fn soup(s: &Scratch, verb: &[&str]) -> Out {
    let mut args = vec!["soup"];
    args.extend_from_slice(verb);
    args.extend_from_slice(&["--env", "dev", "--cauldron", "kb"]);
    run(s, &args)
}

fn import(s: &Scratch, name: &str, records: Value, extra: &[&str]) -> Out {
    let p = s.file(name, &records);
    let mut v = vec!["import", p.to_str().unwrap()];
    v.extend_from_slice(extra);
    soup(s, &v)
}

const BOUNDARY: &str = "Inside: building, signing and publishing releases. Outside: what a release contains.";
const CLUSTER_BOUNDARY: &str = "Inside: everything that gets a release to the people who use it, end to end.";

fn region(label: &str, handle: &str) -> Value {
    json!({"id": label, "type": "region", "handle": handle, "boundary": BOUNDARY})
}

fn cluster(label: &str, handle: &str, claims: &[&str]) -> Value {
    let edges: Vec<Value> = claims.iter().map(|r| json!({"kind": "claims", "to": r})).collect();
    json!({"id": label, "type": "cluster", "handle": handle, "boundary": CLUSTER_BOUNDARY, "edges": edges})
}

fn pools(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".db"))
        .collect();
    v.sort();
    v
}

/// Every node, field and edge row of the workspace pool, as text.
fn ws_rows(soup: &Path) -> Vec<String> {
    let conn = spine::offline::read_only_connection(&soup.join("ws.db")).unwrap();
    let mut out = Vec::new();
    for sql in [
        "SELECT id || '|' || node_type || '|' || rev || '|' || updated_tx FROM nodes ORDER BY id",
        "SELECT node_id || '|' || field || '|' || value || '|' || rev FROM node_fields ORDER BY node_id, field",
        "SELECT src || '|' || kind || '|' || dst_pool || '|' || dst FROM edges ORDER BY src, kind, dst",
    ] {
        let mut st = conn.prepare(sql).unwrap();
        out.extend(st.query_map([], |r| r.get::<_, String>(0)).unwrap().map(Result::unwrap));
    }
    out
}

/// A cauldron as 0.1.4 to 0.1.14 laid it out: its soup holds `spine.db`
/// and `ws.db` only. Made by this release's `create`, less the cluster pool
/// (empty and never written, so nothing else in the store refers to it).
fn older_store(s: &Scratch) {
    let o = run(s, &["create", "kb", "--env", "dev"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let soup = s.soup("kb");
    for f in ["cauldron.db", "cauldron.db-wal", "cauldron.db-shm"] {
        let _ = std::fs::remove_file(soup.join(f));
    }
    assert_eq!(pools(&soup), ["spine.db", "ws.db"]);
}

#[test]
fn cl1_create_makes_the_workspace_and_the_cluster_pool() {
    let s = scratch("cl1");
    let o = run(&s, &["create", "kb", "--env", "dev"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert_eq!(pools(&s.soup("kb")), ["cauldron.db", "spine.db", "ws.db"]);
}

#[test]
fn cl2_an_older_store_gets_the_cluster_pool_on_its_first_cluster_write_only() {
    let s = scratch("cl2");
    older_store(&s);
    let soup_dir = s.soup("kb");
    // A workspace-only import writes no cluster pool.
    let o = import(&s, "ws.json", json!([region("r1", "Releases")]), &["--remint"]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    assert_eq!(pools(&soup_dir), ["spine.db", "ws.db"]);
    // A dry run of a cluster write judges it on a copy and adds nothing here.
    let o = import(&s, "dry.json", json!([cluster("k1", "Shipping", &["R-1"])]), &["--remint", "--dry-run"]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    assert_eq!(o.json()["outcome"], "validated");
    assert_eq!(pools(&soup_dir), ["spine.db", "ws.db"]);
    // A claim, with no cluster pool, has no cluster to claim with.
    let o = soup(&s, &["claim", "CL-1", "R-1"]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("holds no clusters yet"), "{}", o.err);
    assert_eq!(pools(&soup_dir), ["spine.db", "ws.db"]);
    // The first cluster write adds the pool, and lands.
    let ws_before = ws_rows(&soup_dir);
    let o = import(&s, "k.json", json!([cluster("k1", "Shipping", &["R-1"])]), &["--remint"]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    assert_eq!(o.json()["id_map"], json!({"k1": "CL-1"}));
    assert_eq!(pools(&soup_dir), ["cauldron.db", "spine.db", "ws.db"]);
    assert!(!soup_dir.join(".adding-cauldron").exists());
    assert_eq!(ws_rows(&soup_dir), ws_before, "nothing in the workspace pool is rewritten");
    let conn = spine::offline::read_only_connection(&soup_dir.join("cauldron.db")).unwrap();
    let claims: i64 = conn.query_row("SELECT count(*) FROM edges WHERE kind = 'claims' AND dst_pool = 'ws' AND dst = 'R-1'", [], |r| r.get(0)).unwrap();
    assert_eq!(claims, 1);
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(soup_dir.join("cauldron.db")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode & 0o077, 0, "the new pool file is its owner's only ({mode:o})");
}

/// CL3: the older reader, a real 0.1.14 build named by `$CAULDRON_OLDER_BIN`.
#[test]
fn cl3_a_store_with_clusters_opens_reads_and_writes_under_the_older_release() {
    let Some(older) = std::env::var_os("CAULDRON_OLDER_BIN") else {
        eprintln!("cl3: skipped: set CAULDRON_OLDER_BIN to a 0.1.14 `cauldron` to run it");
        return;
    };
    let older = older.to_string_lossy().into_owned();
    let s = scratch("cl3");
    let o = run_with(&older, &s, &["create", "kb", "--env", "dev"]);
    assert_eq!(o.rc, 0, "the older create: {}", o.err);
    assert_eq!(pools(&s.soup("kb")), ["spine.db", "ws.db"], "the older release makes one pool");
    let o = import(&s, "a.json", json!([region("r1", "Releases"), cluster("k1", "Shipping", &["r1"])]), &["--remint"]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let older_soup = |verb: &[&str]| -> Out {
        let mut a = vec!["soup"];
        a.extend_from_slice(verb);
        a.extend_from_slice(&["--env", "dev", "--cauldron", "kb"]);
        run_with(&older, &s, &a)
    };
    // It reads: the region, and no cluster anywhere.
    let o = older_soup(&["regions"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert_eq!(o.json()["regions"].as_array().unwrap().len(), 1);
    let o = older_soup(&["nodes"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert!(o.json()["nodes"].as_array().unwrap().iter().all(|n| n["type"] != "cluster"), "{}", o.out);
    let o = older_soup(&["get", "CL-1"]);
    assert_eq!(o.json()["missing"], json!(["CL-1"]), "{}", o.out);
    // It writes.
    let q = s.file("q.json", &json!([{"id": "q1", "type": "question", "state": "open", "handle": "does the older writer still write?", "edges": [{"kind": "in_region", "to": "R-1"}]}]));
    let o = older_soup(&["import", q.to_str().unwrap(), "--remint"]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    assert_eq!(o.json()["outcome"], "landed");
    // And this release reads both afterwards.
    let o = soup(&s, &["get", "CL-1", "Q-1"]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    assert_eq!(o.json()["records"].as_array().unwrap().len(), 2, "{}", o.out);
}

fn land(s: &Scratch, name: &str, records: Value) -> Value {
    let o = import(s, name, records, &["--remint"]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    o.json()
}

#[test]
fn cl4_a_region_a_cluster_and_its_claim_land_as_one_write_or_not_at_all() {
    let s = scratch("cl4");
    assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).rc, 0);
    let doc = land(&s, "a.json", json!([region("r1", "Releases"), cluster("k1", "Shipping", &["r1"])]));
    assert_eq!(doc["id_map"], json!({"k1": "CL-1", "r1": "R-1"}));
    assert!(doc["landed"]["seq"].is_u64(), "{doc}");
    // A claim of a non-region refuses the whole batch: neither record lands.
    let bad = json!([
        region("r2", "Packaging"),
        {"id": "q1", "type": "question", "state": "open", "handle": "which?", "edges": [{"kind": "in_region", "to": "R-1"}]},
        cluster("k2", "Bad", &["r2", "q1"]),
    ]);
    let o = import(&s, "bad.json", bad, &["--remint"]);
    assert_eq!(o.rc, 1, "{}{}", o.out, o.err);
    let d = o.json();
    assert_eq!(d["outcome"], "refused");
    let k2 = d["records"].as_array().unwrap().iter().find(|r| r["id"] == "k2").unwrap();
    assert_eq!(k2["disposition"], "refused", "{d}");
    let conn = spine::offline::read_only_connection(&s.soup("kb").join("ws.db")).unwrap();
    let n: i64 = conn.query_row("SELECT count(*) FROM nodes", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1, "only R-1");
    // Carried ids: a cluster carried as CL-5 with its claim, bare and @ws.
    let doc = land(&s, "b.json", json!([{"id": "CL-5", "type": "cluster", "handle": "Docs", "boundary": CLUSTER_BOUNDARY, "edges": [{"kind": "claims", "to": "R-1@ws"}]}]));
    assert_eq!(doc["outcome"], "landed", "{doc}");
}

#[test]
fn cl5_soup_claim_claims_releases_and_says_so_again_without_writing() {
    let s = scratch("cl5");
    assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).rc, 0);
    land(&s, "a.json", json!([region("r1", "Releases"), region("r2", "Packaging"), cluster("k1", "Shipping", &[])]));
    let o = soup(&s, &["claim", "CL-1", "R-2", "--note", "the install half"]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    assert_eq!(o.json()["outcome"], "claimed");
    assert_eq!(o.json()["note"], "the install half");
    let o = soup(&s, &["claim", "CL-1", "R-2"]);
    assert_eq!((o.rc, o.json()["outcome"].clone(), o.json()["written"].clone()), (0, json!("already-claimed"), json!(false)));
    let o = soup(&s, &["claim", "CL-1", "R-2", "--release"]);
    assert_eq!(o.json()["outcome"], "released", "{}", o.out);
    let o = soup(&s, &["claim", "CL-1", "R-2", "--release"]);
    assert_eq!(o.json()["outcome"], "not-claimed", "{}", o.out);
    for (args, says) in [
        (vec!["claim", "CL-9", "R-1"], "there is no cluster `CL-9`"),
        (vec!["claim", "CL-1", "R-9"], "there is no region `R-9`"),
        (vec!["claim", "CL-1"], "takes a cluster and a region"),
        (vec!["claim", "CL-1", "R-1", "--release", "--note", "x"], "a release takes no note"),
    ] {
        let o = soup(&s, &args);
        assert_ne!(o.rc, 0, "{args:?}");
        assert!(o.err.contains(says), "{args:?}: {}", o.err);
    }
}

#[test]
fn cl7_the_review_flags_are_refused_on_a_cluster_record() {
    let s = scratch("cl7");
    assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).rc, 0);
    let mut k = cluster("k1", "Shipping", &[]);
    k["meta"] = json!({"approved": {"at": "2026-01-01", "by": "owner"}});
    let o = import(&s, "k.json", json!([k]), &["--remint"]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("review flags"), "{}", o.err);
}

#[test]
fn cl6_the_reads_answer_clusters_bounded_like_every_read() {
    let s = scratch("cl6");
    assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).rc, 0);
    land(&s, "a.json", json!([region("r1", "Releases"), region("r2", "Packaging"), cluster("k1", "Shipping lane", &["r1", "r2"]), cluster("k2", "Schema work", &["r1"])]));
    let o = soup(&s, &["nodes", "--type", "cluster"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let d = o.json();
    assert_eq!((d["pool"].clone(), d["total"].clone()), (json!("cauldron"), json!(2)), "{d}");
    assert_eq!(d["nodes"][1], json!({"id": "CL-1", "type": "cluster", "handle": "Shipping lane", "regions": ["R-1", "R-2"]}));
    let d = soup(&s, &["nodes", "--type", "cluster", "--region", "R-2", "--max-chars", "4"]).json();
    assert_eq!(d["nodes"], json!([{"id": "CL-1", "type": "cluster", "handle": "Ship", "regions": ["R-1", "R-2"]}]), "{d}");
    assert_eq!(d["truncated"][0]["field"], "handle");
    let d = soup(&s, &["region", "R-1"]).json();
    assert_eq!(d["clusters"], json!([{"id": "CL-1", "handle": "Shipping lane"}, {"id": "CL-2", "handle": "Schema work"}]), "{d}");
    let o = soup(&s, &["get", "CL-2"]);
    assert_eq!(o.rc, 0, "{}", o.out);
    assert_eq!(o.json()["records"][0]["claims"][0]["region"], "R-1");
    let d = soup(&s, &["search", "schema", "work"]).json();
    assert_eq!(d["results"][0]["id"], "CL-2", "{d}");
    let d = soup(&s, &["search", "release", "--type", "cluster", "--limit", "1"]).json();
    assert_eq!((d["results"].as_array().unwrap().len(), d["more"].clone()), (1, json!(1)), "{d}");
    // The workspace-only reads are as before: no type lists no cluster.
    let d = soup(&s, &["nodes"]).json();
    assert!(d["nodes"].as_array().unwrap().iter().all(|n| n["type"] == "region"), "{d}");
}

#[test]
fn cl8_soup_changes_reports_cluster_writes_and_waits() {
    let s = scratch("cl8");
    assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).rc, 0);
    let cur = soup(&s, &["changes"]).json()["cursor"].as_str().unwrap().to_string();
    land(&s, "a.json", json!([region("r1", "Releases"), cluster("k1", "Shipping", &[])]));
    assert_eq!(soup(&s, &["claim", "CL-1", "R-1"]).rc, 0);
    assert_eq!(soup(&s, &["claim", "CL-1", "R-1", "--release"]).rc, 0);
    let d = soup(&s, &["changes", "--since", &cur]).json();
    let ops: Vec<&str> = d["changes"].as_array().unwrap().iter().map(|c| c["op"].as_str().unwrap()).collect();
    assert_eq!(ops, ["create", "link", "unlink"], "{d}");
    let head = d["cursor"].as_str().unwrap().to_string();
    // A wait that times out answers the same cursor.
    let t0 = std::time::Instant::now();
    let d = soup(&s, &["changes", "--since", &head, "--wait", "1"]).json();
    assert!(t0.elapsed() >= std::time::Duration::from_millis(900), "{:?}", t0.elapsed());
    assert_eq!((d["cursor"].as_str(), d["waited"]["ended"].as_str()), (Some(head.as_str()), Some("timeout")), "{d}");
    // A wait ends when a write lands.
    let waiter = {
        let (bin, root, home, since) = (CAULDRON.to_string(), s.root(), s.0.join("home"), head.clone());
        std::thread::spawn(move || {
            Command::new(bin)
                .args(["soup", "changes", "--since", &since, "--wait", "30", "--env", "dev", "--cauldron", "kb"])
                .env("CAULDRON_HOME", root)
                .env("HOME", home)
                .env_remove("CAULDRON_ENV")
                .env_remove("ENV")
                .env_remove("CAULDRON_CAULDRON")
                .stdin(Stdio::null())
                .output()
                .unwrap()
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(700));
    let t1 = std::time::Instant::now();
    assert_eq!(soup(&s, &["claim", "CL-1", "R-1"]).rc, 0);
    let o = waiter.join().unwrap();
    let d: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(d["waited"]["ended"], "landed", "{d}");
    assert_eq!(d["changes"][0]["op"], "link", "{d}");
    assert!(t1.elapsed() < std::time::Duration::from_secs(5), "{:?}", t1.elapsed());
    let o = soup(&s, &["changes", "--wait", "2"]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("needs --since"), "{}", o.err);
    let o = soup(&s, &["changes", "--since", &head, "--wait", "61"]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("0 to 60"), "{}", o.err);
}

// ------------------------------------------------- a claim and its region

/// How many nodes of pool file `pool` carry this handle.
fn handled(soup: &Path, pool: &str, handle: &str) -> i64 {
    let f = soup.join(format!("{pool}.db"));
    if !f.is_file() {
        return 0;
    }
    spine::offline::read_only_connection(&f)
        .unwrap()
        .query_row("SELECT count(*) FROM node_fields WHERE field = 'handle' AND value = json_quote(?1)", [handle], |r| r.get(0))
        .unwrap()
}

fn claims_rows(soup: &Path) -> i64 {
    let f = soup.join("cauldron.db");
    if !f.is_file() {
        return 0;
    }
    spine::offline::read_only_connection(&f).unwrap().query_row("SELECT count(*) FROM edges WHERE kind = 'claims'", [], |r| r.get(0)).unwrap()
}

fn region_ids(soup: &Path) -> std::collections::BTreeSet<String> {
    let conn = spine::offline::read_only_connection(&soup.join("ws.db")).unwrap();
    let mut st = conn.prepare("SELECT id FROM nodes WHERE node_type = 'region'").unwrap();
    st.query_map([], |r| r.get::<_, String>(0)).unwrap().map(Result::unwrap).collect()
}

/// **No read names a claim whose region it cannot read**: every region a
/// cluster listing, a cluster record, a search hit or a region's claimants
/// name is a region the workspace pool holds, and every claim a record
/// carries has its region's handle. `regions_before` is read BEFORE the
/// reads (so a region that lands meanwhile cannot excuse a dangling claim).
fn assert_no_dangling_claim(s: &Scratch, regions_before: &std::collections::BTreeSet<String>, why: &str) {
    let o = soup(s, &["nodes", "--type", "cluster"]);
    assert_eq!(o.rc, 0, "{why}: {}", o.err);
    let listed = o.json();
    let mut ids = Vec::new();
    for n in listed["nodes"].as_array().unwrap() {
        ids.push(n["id"].as_str().unwrap().to_string());
        for r in n["regions"].as_array().unwrap() {
            assert!(regions_before.contains(r.as_str().unwrap()), "{why}: nodes --type cluster names {r}, which is not readable: {listed}");
        }
    }
    if !ids.is_empty() {
        let mut args = vec!["get"];
        args.extend(ids.iter().map(String::as_str));
        let d = soup(s, &args).json();
        for rec in d["records"].as_array().unwrap() {
            for c in rec["claims"].as_array().unwrap() {
                assert!(regions_before.contains(c["region"].as_str().unwrap()), "{why}: get names a claim of an unreadable region: {rec}");
                assert!(c["handle"].is_string(), "{why}: a claim without its region's handle: {rec}");
            }
            for e in rec["edges"].as_array().into_iter().flatten() {
                let to = e["to"].as_str().unwrap().trim_end_matches("@ws");
                assert!(regions_before.contains(to), "{why}: get lists an edge to an unreadable region: {rec}");
            }
        }
    }
    let d = soup(s, &["search", "shipping", "--type", "cluster"]).json();
    for h in d["results"].as_array().unwrap() {
        for r in h["regions"].as_array().unwrap() {
            assert!(regions_before.contains(r.as_str().unwrap()), "{why}: search names {r}: {d}");
        }
    }
}

/// CL9: a store whose cluster pool holds a claim of a region the workspace
/// pool does not (what a write cut between its two landings left, when the
/// claim's pool landed first): every read leaves the claim out, and the
/// claim of a region that is there still reads.
#[test]
fn cl9_a_claim_of_a_region_that_is_not_there_is_left_out_of_every_read() {
    let s = scratch("cl9");
    assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).rc, 0);
    land(&s, "a.json", json!([region("r1", "Releases"), cluster("k1", "Shipping", &["r1"])]));
    let soup_dir = s.soup("kb");
    {
        let conn = rusqlite::Connection::open(soup_dir.join("cauldron.db")).unwrap();
        let tx: String = conn.query_row("SELECT created_tx FROM nodes WHERE id = 'CL-1'", [], |r| r.get(0)).unwrap();
        conn.execute(
            "INSERT INTO edges (src, kind, dst_pool, dst, note, fields, created_tx, created) VALUES ('CL-1', 'claims', 'ws', 'R-9', NULL, '{}', ?1, '2026-01-01T00:00:00Z')",
            [tx],
        )
        .unwrap();
    }
    assert_eq!(claims_rows(&soup_dir), 2, "the torn claim is planted");
    let before = region_ids(&soup_dir);
    assert!(!before.contains("R-9"));
    assert_no_dangling_claim(&s, &before, "planted");
    let d = soup(&s, &["get", "CL-1"]).json();
    assert_eq!(d["records"][0]["claims"], json!([{"region": "R-1", "handle": "Releases"}]), "{d}");
    let d = soup(&s, &["nodes", "--type", "cluster"]).json();
    assert_eq!(d["nodes"][0]["regions"], json!(["R-1"]), "{d}");
    let d = soup(&s, &["region", "R-1"]).json();
    assert_eq!(d["clusters"], json!([{"id": "CL-1", "handle": "Shipping"}]), "{d}");
}

/// A real mixed import (a new region, a cluster, and its claims of that
/// region and of R-1), stopped by the spine's fault seam at `stage` with
/// `var` (`SPINE_FAULT` aborts there; `SPINE_PAUSE` holds there until the
/// latch appears).
#[cfg(feature = "fault-injection")]
fn mixed_import(s: &Scratch, var: &[(&str, String)]) -> Command {
    let batch = s.file("mixed.json", &json!([region("rx", "Torn region"), cluster("kx", "Shipping torn", &["rx", "R-1"])]));
    let mut c = Command::new(CAULDRON);
    c.args(["soup", "import", batch.to_str().unwrap(), "--env", "dev", "--cauldron", "kb", "--remint"])
        .env("CAULDRON_HOME", s.root())
        .env("HOME", s.0.join("home"))
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .env_remove("CAULDRON_CAULDRON")
        .stdin(Stdio::null());
    for (k, v) in var {
        c.env(k, v);
    }
    c
}

/// CL10: the crash matrix. A mixed batch killed at each of the four commit
/// stages, on a store made with the cluster pool and on one made before it:
/// before recovery no read names a claim whose region it cannot read (killed
/// between its landings, the region has landed and the claim has not), and
/// recovery settles the pair as one unit: both, with both claims, or
/// neither. Importing the batch again is refused once it landed and lands
/// once it was rolled back.
#[cfg(feature = "fault-injection")]
#[test]
fn cl10_a_mixed_batch_killed_at_any_stage_is_never_torn_to_readers_and_recovers_whole() {
    for older in [false, true] {
        for stage in ["row-appended", "decided", "between-landings", "before-cleared"] {
            let why = format!("{stage}, {}", if older { "older store" } else { "new store" });
            let s = scratch("cl10");
            if older {
                older_store(&s);
            } else {
                assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).rc, 0);
            }
            land(&s, "a.json", json!([region("r1", "Releases")]));
            let soup_dir = s.soup("kb");
            let o = mixed_import(&s, &[("SPINE_FAULT", stage.to_string())]).output().unwrap();
            assert_ne!(o.status.code(), Some(0), "{why}: the seam did not stop the import");
            let before = region_ids(&soup_dir);
            assert_no_dangling_claim(&s, &before, &why);
            if stage == "between-landings" {
                assert_eq!(handled(&soup_dir, "ws", "Torn region"), 1, "{why}: the region lands first");
                assert_eq!(handled(&soup_dir, "cauldron", "Shipping torn"), 0, "{why}: the claim's pool lands second");
            }
            let o = run(&s, &["soup", "recover", "--env", "dev", "--cauldron", "kb"]);
            assert_eq!(o.rc, 0, "{why}: {}{}", o.out, o.err);
            let landed = stage != "row-appended";
            let want = i64::from(landed);
            assert_eq!(handled(&soup_dir, "ws", "Torn region"), want, "{why}: the region");
            assert_eq!(handled(&soup_dir, "cauldron", "Shipping torn"), want, "{why}: the cluster");
            assert_eq!(claims_rows(&soup_dir), 2 * want, "{why}: the claims");
            assert_no_dangling_claim(&s, &region_ids(&soup_dir), &why);
            let o = mixed_import(&s, &[]).output().unwrap();
            let d: Value = serde_json::from_slice(&o.stdout).unwrap_or(Value::Null);
            if landed {
                assert_eq!(d["refusal"], "batch-already-landed", "{why}: {d}");
            } else {
                assert_eq!(d["outcome"], "landed", "{why}: {d}");
            }
            assert_eq!(handled(&soup_dir, "cauldron", "Shipping torn"), 1, "{why}: one copy");
        }
    }
}

/// CL11: the window held open. A mixed import held by the fault seam's
/// pause between its two landings (a live writer, not a dead one): while
/// it is held, the reads, `soup changes` and `soup region` name no claim of
/// a region they cannot read, and the change cursor has not moved; once
/// released, the region, the cluster and both claims read together.
#[cfg(feature = "fault-injection")]
#[test]
fn cl11_readers_inside_a_held_window_see_no_dangling_claim() {
    let s = scratch("cl11");
    assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).rc, 0);
    land(&s, "a.json", json!([region("r1", "Releases")]));
    let soup_dir = s.soup("kb");
    let cur = soup(&s, &["changes"]).json()["cursor"].as_str().unwrap().to_string();
    let latch = s.0.join("latch");
    let reached = s.0.join("latch.reached");
    let mut child = mixed_import(
        &s,
        &[
            ("SPINE_PAUSE", "between-landings".to_string()),
            ("SPINE_PAUSE_LATCH", latch.to_string_lossy().into_owned()),
            ("SPINE_PAUSE_CEILING_MS", "60000".to_string()),
        ],
    )
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    let t0 = std::time::Instant::now();
    while !reached.exists() {
        assert!(t0.elapsed() < std::time::Duration::from_secs(30), "the import never reached the window");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    for round in 0..3 {
        let before = region_ids(&soup_dir);
        assert_no_dangling_claim(&s, &before, &format!("held, round {round}"));
        let d = soup(&s, &["region", "R-1"]).json();
        assert_eq!(d["clusters"], json!([]), "{d}");
        let d = soup(&s, &["changes", "--since", &cur]).json();
        assert_eq!(d["changes"], json!([]), "a half-landed write is not a change yet: {d}");
    }
    std::fs::write(&latch, b"go").unwrap();
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    let d = soup(&s, &["nodes", "--type", "cluster"]).json();
    assert_eq!(d["nodes"][0]["regions"].as_array().unwrap().len(), 2, "{d}");
    let d = soup(&s, &["changes", "--since", &cur]).json();
    assert_eq!(d["changes"].as_array().unwrap().len(), 1, "{d}");
    assert_no_dangling_claim(&s, &region_ids(&soup_dir), "released");
}

/// CL12: the command line bounds a new edge note at 1,000 characters as the
/// service does (`soup claim --note`, an import's record edges), writing
/// nothing, and a longer stored note reads back cut, the cut named.
#[test]
fn cl12_edge_notes_are_bounded_on_the_command_line_too() {
    let s = scratch("cl12");
    assert_eq!(run(&s, &["create", "kb", "--env", "dev"]).rc, 0);
    land(&s, "a.json", json!([region("r1", "Releases"), cluster("k1", "Shipping", &[])]));
    let long = "n".repeat(1_001);
    let o = soup(&s, &["claim", "CL-1", "R-1", "--note", &long]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("at most 1000") && o.err.contains("Nothing was written"), "{}", o.err);
    assert_eq!(claims_rows(&s.soup("kb")), 0);
    let mut k = cluster("k2", "Docs", &["R-1"]);
    k["edges"][0]["note"] = json!(long);
    let o = import(&s, "k.json", json!([k]), &["--remint"]);
    assert_ne!(o.rc, 0, "{}", o.out);
    assert!(o.err.contains("record `k2`'s edge 0"), "{}", o.err);
    assert_eq!(soup(&s, &["claim", "CL-1", "R-1", "--note", "short"]).rc, 0);
    {
        let conn = rusqlite::Connection::open(s.soup("kb").join("cauldron.db")).unwrap();
        conn.execute("UPDATE edges SET note = ?1 WHERE src = 'CL-1'", [&"m".repeat(5_000)]).unwrap();
    }
    let d = soup(&s, &["get", "CL-1"]).json();
    assert_eq!(d["records"][0]["claims"][0]["note"].as_str().unwrap().len(), 1_000, "{d}");
    assert!(d["truncated"].as_array().unwrap().iter().any(|t| t["field"] == "claims/R-1/note"), "{d}");
    let d = soup(&s, &["region", "R-1"]).json();
    assert_eq!(d["clusters"][0]["note"].as_str().unwrap().len(), 1_000, "{d}");
}

/// CL13: on a store made before clusters, an import whose cluster the
/// grammar refuses adds no cluster pool; one that lands a cluster adds it.
#[test]
fn cl13_a_refused_first_cluster_import_adds_no_pool() {
    let s = scratch("cl13");
    older_store(&s);
    let soup_dir = s.soup("kb");
    let mut k = cluster("k1", "Shipping", &[]);
    k["boundary"] = json!("too short");
    let o = import(&s, "k.json", json!([region("r1", "Releases"), k]), &["--remint"]);
    assert_eq!(o.rc, 1, "{}{}", o.out, o.err);
    let d = o.json();
    assert_eq!((d["outcome"].as_str(), d["dry_run"].as_bool()), (Some("refused"), Some(false)), "{d}");
    assert_eq!(pools(&soup_dir), ["spine.db", "ws.db"], "no pool added");
    assert!(!soup_dir.join(".adding-cauldron").exists());
    land(&s, "ok.json", json!([region("r1", "Releases"), cluster("k1", "Shipping", &["r1"])]));
    assert_eq!(pools(&soup_dir), ["cauldron.db", "spine.db", "ws.db"]);
}
