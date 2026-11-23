//! **`cauldron soup import-corpus`**: the older corpus format (the terminal
//! interface's seed corpus, schema version 5) landed as workspace-v6 records,
//! and its clusters as cauldron@1 `cluster` records, through the same door as
//! `cauldron soup import`.
//!
//! Fixtures (`fixtures/corpus-v5/`): `seed.json`, a seed file with a cluster
//! tree (a cluster claiming two regions, holding a task, and a child claiming
//! one region of the corpus and one that is not in it), a corpus of three
//! regions and five nodes (one of a type workspace-v6 does not have) and the
//! seed's messages; and `records/`, the same regions and nodes one per file.
//!
//! - **K1** a dry run validates and writes nothing, lists what is not carried
//!   (the cluster's task, the child's nesting, the claim of a region not in
//!   the corpus, the incident, the messages, the unknown key, a boundary
//!   naming old ids) and gives the id map by corpus id; the real import lands
//!   with new ids, `narrows`, `in_region`, `ref` edges, `operator`,
//!   `meta.imported_from`, `meta.cites`, a rewritten `[[link]]`, and the
//!   clusters with their claims, seen from the cluster and from the region.
//! - **K2** the directory form converts to the same regions and nodes.
//! - **K3** all or nothing: a record the door refuses (a state outside its
//!   type's vocabulary) refuses the batch, and nothing lands.
//! - **K4** the batch-already-landed guard, and `--allow-duplicate`.
//! - **K5** a project marker tracked by git cannot aim the write.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

const CAULDRON: &str = env!("CARGO_BIN_EXE_cauldron");

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/it/fixtures/corpus-v5").join(rel)
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
impl Scratch {
    fn new(tag: &str, cauldrons: &[&str]) -> Scratch {
        let p = std::env::temp_dir().join(format!(
            "cauldron-corpus-it-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        for d in ["root/data/dev", "home"] {
            std::fs::create_dir_all(p.join(d)).unwrap();
        }
        let s = Scratch(p.canonicalize().unwrap());
        if let Some(h) = std::env::var_os("HOME") {
            assert!(!s.0.starts_with(Path::new(&h).join(".cauldron")), "scratch under ~/.cauldron");
        }
        for c in cauldrons {
            let o = s.run_in(&s.0, &["create", c, "--env", "dev"]);
            assert_eq!(o.0, 0, "create {c}: {}", o.2);
        }
        s
    }
    fn soup(&self, c: &str) -> PathBuf {
        self.0.join("root/data/dev/cauldrons").join(c).join("soup")
    }
    fn run_in(&self, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        let o = Command::new(CAULDRON)
            .args(args)
            .current_dir(cwd)
            .env("CAULDRON_HOME", self.0.join("root"))
            .env("HOME", self.0.join("home"))
            .env_remove("CAULDRON_ENV")
            .env_remove("ENV")
            .env_remove("CAULDRON_CAULDRON")
            .stdin(Stdio::null())
            .output()
            .expect("spawn cauldron");
        (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
    }
    fn corpus(&self, c: &str, input: &Path, extra: &[&str]) -> (i32, Value, String) {
        let mut a = vec!["soup", "import-corpus", input.to_str().unwrap(), "--env", "dev", "--cauldron", c];
        a.extend_from_slice(extra);
        let (rc, out, err) = self.run_in(&self.0, &a);
        let v = serde_json::from_str(&out).unwrap_or_else(|e| panic!("not JSON ({e}): {out}\n{err}"));
        (rc, v, err)
    }
    fn get(&self, c: &str, id: &str) -> Value {
        let (rc, out, err) = self.run_in(&self.0, &["soup", "get", id, "--env", "dev", "--cauldron", c]);
        assert_eq!(rc, 0, "{err}");
        let v: Value = serde_json::from_str(&out).unwrap();
        v["records"].as_array().and_then(|a| a.first()).cloned().unwrap_or_else(|| panic!("no node {id}: {out}"))
    }
}

/// The bytes of every database file of a soup (side files excluded: SQLite
/// may create and remove `-wal`/`-shm` around a connection's life).
fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("db"))
        .map(|p| (p.clone(), std::fs::read(&p).unwrap()))
        .collect();
    out.sort();
    out
}

fn edge_to(n: &Value, kind: &str) -> Vec<String> {
    n["edges"].as_array().unwrap().iter().filter(|e| e["kind"] == kind).map(|e| e["to"].as_str().unwrap().to_string()).collect()
}

#[test]
fn k1_a_seed_lands_with_its_clusters_and_says_what_it_does_not_carry() {
    let s = Scratch::new("k1", &["kb"]);
    let before = files(&s.soup("kb"));
    let (rc, d, err) = s.corpus("kb", &fixture("seed.json"), &["--dry-run"]);
    assert_eq!(rc, 0, "{d}\n{err}");
    assert_eq!(d["outcome"], "validated", "{d}");
    assert_eq!(files(&s.soup("kb")), before, "a dry run wrote to the soup");
    let corpus = &d["corpus"];
    assert_eq!(corpus["carried"]["regions"], 3);
    assert_eq!(corpus["carried"]["nodes"], 4);
    assert_eq!(corpus["carried"]["clusters"], 2);
    let nc = corpus["not_carried"].as_array().unwrap();
    let has = |what: &str, reason: &str| nc.iter().any(|e| e["what"] == what && e["reason"].as_str().unwrap().starts_with(reason));
    assert!(nc.iter().all(|e| !e["reason"].as_str().unwrap().starts_with("cluster-not-in-workspace-v6")), "{nc:?}");
    assert!(has("cluster-part", "not-in-cauldron-v1"), "the cluster's task is listed: {nc:?}");
    assert!(has("cluster-nesting", "no-cluster-nesting-in-cauldron-v1"), "{nc:?}");
    let absent: Vec<&Value> = nc.iter().filter(|e| e["what"] == "claim").collect();
    assert_eq!(absent.len(), 1, "{nc:?}");
    assert_eq!((absent[0]["from"].clone(), absent[0]["to"].clone()), (json!("files"), json!("R-7")));
    assert!(absent[0]["reason"].as_str().unwrap().starts_with("target-not-in-input"), "{nc:?}");
    assert!(has("node", "type-not-in-workspace-v6"), "the incident is listed: {nc:?}");
    assert!(has("seed-key", "seed-messages"), "{nc:?}");
    assert!(has("field", "unknown-key"), "{nc:?}");
    assert!(has("boundary-text", "old-ids-in-text"), "{nc:?}");
    assert!(err.contains("not carried"), "{err}");
    let map = corpus["id_map"].as_object().unwrap();
    assert_eq!(map.len(), 9, "every carried record has a new id: {map:?}");

    let (rc, d, err) = s.corpus("kb", &fixture("seed.json"), &[]);
    assert_eq!(rc, 0, "{d}\n{err}");
    assert_eq!(d["outcome"], "landed", "{d}");
    let map = d["corpus"]["id_map"].clone();
    let id = |old: &str| map[old].as_str().unwrap_or_else(|| panic!("{old} not in {map}")).to_string();
    let r2 = s.get("kb", &id("R-2"));
    assert_eq!(edge_to(&r2, "narrows"), vec![id("R-1")]);
    let dec = s.get("kb", &id("DEC-1"));
    assert_eq!(dec["operator"], json!({"thread": "notes", "msg": 4}));
    assert_eq!(dec["meta"]["imported_from"]["id"], "DEC-1");
    assert_eq!(dec["meta"]["imported_from"]["format"], "corpus-v5");
    assert_eq!(edge_to(&dec, "in_region"), vec![id("R-2")]);
    assert_eq!(edge_to(&dec, "ref"), vec![id("IS-2")], "the [[link]] has its ref edge");
    assert!(dec["body"].as_str().unwrap().contains(&format!("[[{}]]", id("IS-2"))), "{dec}");
    let q = s.get("kb", &id("Q-1"));
    assert_eq!(edge_to(&q, "ref"), vec![id("DEC-1")]);
    assert_eq!(q["meta"]["cites"], json!([{"kind": "url", "url": "https://example.com/corpus-notes"}]));
    let c = s.get("kb", &id("C-1"));
    assert_eq!(c["meta"]["imported_from"]["unmapped"], json!({"tags": ["later"]}));
    let r1 = s.get("kb", &id("R-1"));
    assert_eq!(r1["meta"]["imported_from"]["schema_version"], 5);

    // The clusters landed in the same write, with their claims, seen both ways.
    let import = s.get("kb", &id("cluster:import"));
    assert_eq!((import["type"].clone(), import["handle"].clone()), (json!("cluster"), json!("Import")), "{import}");
    assert_eq!(import["meta"]["imported_from"]["not_carried"], json!({"tasks": 1}), "{import}");
    let claimed = |c: &Value| -> Vec<String> {
        let mut v: Vec<String> = c["claims"].as_array().unwrap().iter().map(|x| x["region"].as_str().unwrap().to_string()).collect();
        v.sort();
        v
    };
    let mut want = vec![id("R-1"), id("R-2")];
    want.sort();
    assert_eq!(claimed(&import), want, "{import}");
    let files_cl = s.get("kb", &id("cluster:files"));
    assert_eq!(claimed(&files_cl), vec![id("R-2")], "the claim of R-7, not in the corpus, is not made: {files_cl}");
    let claimants = |r: &str| -> Vec<String> {
        let (rc, out, err) = s.run_in(&s.0, &["soup", "region", r, "--env", "dev", "--cauldron", "kb"]);
        assert_eq!(rc, 0, "{err}");
        let v: Value = serde_json::from_str(&out).unwrap();
        let mut l: Vec<String> = v["clusters"].as_array().unwrap_or_else(|| panic!("no clusters: {v}")).iter().map(|c| c["id"].as_str().unwrap().to_string()).collect();
        l.sort();
        l
    };
    assert_eq!(claimants(&id("R-1")), vec![id("cluster:import")]);
    let mut both = vec![id("cluster:import"), id("cluster:files")];
    both.sort();
    assert_eq!(claimants(&id("R-2")), both);
    assert!(claimants(&id("R-3")).is_empty());
}

#[test]
fn k2_a_directory_of_one_record_files_converts_to_the_same_records() {
    let s = Scratch::new("k2", &["a", "b"]);
    let (rc, file, err) = s.corpus("a", &fixture("seed.json"), &["--dry-run"]);
    assert_eq!(rc, 0, "{err}");
    let (rc, dir, err) = s.corpus("b", &fixture("records"), &["--dry-run"]);
    assert_eq!(rc, 0, "{dir}\n{err}");
    // The seed's clusters are the only difference: the directory holds none.
    for k in ["regions", "nodes"] {
        assert_eq!(file["corpus"]["carried"][k], dir["corpus"]["carried"][k], "{k}");
    }
    assert_eq!((file["corpus"]["carried"]["clusters"].clone(), dir["corpus"]["carried"]["clusters"].clone()), (json!(2), json!(0)));
    let without_clusters = |m: &Value| -> Value {
        Value::Object(m.as_object().unwrap().iter().filter(|(k, _)| !k.starts_with("cluster:")).map(|(k, v)| (k.clone(), v.clone())).collect())
    };
    assert_eq!(without_clusters(&file["corpus"]["id_map"]), dir["corpus"]["id_map"]);
    let strip = |v: &Value| -> Vec<(Value, Value)> {
        let mut l: Vec<(Value, Value)> = v["records"].as_array().unwrap().iter().filter(|r| !r["id"].as_str().unwrap_or("").starts_with("v5:cluster:")).map(|r| (r["id"].clone(), r["disposition"].clone())).collect();
        l.sort_by_key(|(id, _)| id.to_string());
        l
    };
    assert_eq!(strip(&file), strip(&dir));
    // No clusters or messages in the directory: only the incident and the
    // field-level and boundary notes are listed.
    let nc = dir["corpus"]["not_carried"].as_array().unwrap();
    assert!(nc.iter().all(|e| e["what"] != "cluster" && e["what"] != "seed-key"), "{nc:?}");
    assert!(nc.iter().any(|e| e["what"] == "node" && e["id"] == "INC-1"));
}

#[test]
fn k3_one_refused_record_lands_nothing() {
    let s = Scratch::new("k3", &["kb"]);
    let dir = s.0.join("bad");
    std::fs::create_dir_all(&dir).unwrap();
    for f in ["R-1.json", "DEC-1.json", "R-2.json", "IS-2.json", "R-3.json"] {
        std::fs::copy(fixture("records").join(f), dir.join(f)).unwrap();
    }
    std::fs::write(
        dir.join("CON-1.json"),
        r#"{"id": "CON-1", "type": "constraint", "state": "active", "handle": "Imports land whole or not at all.", "in_region": ["R-1"]}"#,
    )
    .unwrap();
    let before = files(&s.soup("kb"));
    let (rc, d, err) = s.corpus("kb", &dir, &[]);
    assert_eq!(rc, 1, "{d}\n{err}");
    assert_eq!((d["outcome"].clone(), d["refusal"].clone()), (json!("refused"), json!("records-refused")), "{d}");
    assert!(d["records"].as_array().unwrap().iter().any(|r| r["id"] == "v5:CON-1" && r["disposition"] == "refused"), "{d}");
    assert!(d["corpus"].get("id_map").is_none(), "a refused batch has no id map");
    assert_eq!(files(&s.soup("kb")), before, "a refused batch wrote to the soup");
}

#[test]
fn k4_the_same_batch_again_is_refused_unless_allowed() {
    let s = Scratch::new("k4", &["kb"]);
    assert_eq!(s.corpus("kb", &fixture("records"), &[]).0, 0);
    let (rc, d, _) = s.corpus("kb", &fixture("records"), &[]);
    assert_eq!((rc, d["refusal"].clone()), (1, json!("batch-already-landed")), "{d}");
    let (rc, d, _) = s.corpus("kb", &fixture("records"), &["--dry-run"]);
    assert_eq!((rc, d["refusal"].clone()), (1, json!("batch-already-landed")), "{d}");
    let (rc, d, err) = s.corpus("kb", &fixture("records"), &["--allow-duplicate"]);
    assert_eq!((rc, d["outcome"].clone()), (0, json!("landed")), "{err}");
}

#[test]
fn k5_a_tracked_marker_cannot_aim_the_write() {
    if Command::new("git").arg("--version").output().map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("k5: git is not available; skipped");
        return;
    }
    let s = Scratch::new("k5", &["kb"]);
    let proj = s.0.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let git = |args: &[&str]| {
        let o = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-C"])
            .arg(&proj)
            .args(args)
            .env("HOME", s.0.join("home"))
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    };
    git(&["init", "-q"]);
    assert_eq!(s.run_in(&proj, &["link", "kb", "--env", "dev"]).0, 0);
    git(&["add", "-f", ".cauldron"]);
    git(&["commit", "-qm", "marker"]);
    let before = files(&s.soup("kb"));
    let input = fixture("records");
    let (rc, _, err) = s.run_in(&proj, &["soup", "import-corpus", input.to_str().unwrap()]);
    assert_ne!(rc, 0);
    assert!(err.contains("REFUSED") && err.contains("tracked by git"), "{err}");
    assert_eq!(files(&s.soup("kb")), before);
    // A dry run through the same marker is allowed, and says where its target came from.
    let (rc, out, err) = s.run_in(&proj, &["soup", "import-corpus", input.to_str().unwrap(), "--dry-run"]);
    assert_eq!(rc, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v["resolved_from"].as_str().unwrap().starts_with("marker:"), "{v}");
    assert_eq!(v["outcome"], "validated");
}
