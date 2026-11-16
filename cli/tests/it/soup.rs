//! **`cauldron soup import | regions | get`** — the soup, offline.
//!
//! Every test runs the built `cauldron` under a scratch `CAULDRON_HOME`/`HOME`
//! (never `~/.cauldron`; this binary exports `CAULDRON_ISOLATED=1`), on a
//! cauldron made by `cauldron create`, whose soup is the engine's own (one
//! `ws` pool on grammar v6, `engine::ensure_soup_cauldron`).
//!
//! - **S1** round trip: the published README's worked example dry-runs
//!   (nothing lands), imports, and reads back record for record with `get`;
//!   `regions` lists both regions.
//! - **S2** one refused record refuses the batch: the reason is the door's,
//!   the other records are reported accepted, and the store's files are
//!   byte-identical.
//! - **S3** a carried-id collision without `--remint` refuses everything.
//! - **S4** `--remint` twice into one pool: two disjoint id sets, references
//!   and `[[links]]` inside each batch rewritten, a reference outside the
//!   batch (bare, and `@ws`) left pointing at the existing node; YAML
//!   directory input.
//! - **S5** a live soup daemon, an unreadable seam table, and a soup not at
//!   rest each refuse a real import and write nothing; a dry run with the
//!   daemon live is judged on a snapshot and writes nothing.
//! - **S6** the reads write nothing, and every verb needs `--env`.
//! - **S7** the acceptance run's shadow reproduction: under `--remint` a
//!   label equal to a held id refuses the batch (dry run and real), and the
//!   same batch with labels that are not ids lands with its bare reference
//!   on the existing node.
//! - **S8** `nodes`, `search` and `region`: shapes, filters, newest first,
//!   `--limit` with `more`, `--max-chars` with `truncated`, deterministic
//!   ranking.
//! - **S9** an import KILLED part way leaves the soup not at rest: nothing
//!   of it has landed, and every later import refuses, naming `cauldron soup
//!   recover`. `recover` refuses while a soup daemon is live or another
//!   writer holds the soup's writer lock, settles the soup otherwise (the
//!   killed import rolled back, or rolled forward whole if it had decided),
//!   is a no-op on a soup at rest, and the next import lands. Each outcome
//!   is stated in `interrupted_import` and in words on stderr.
//! - **S10** (only under `--features fault-injection`) an import stopped at
//!   a named commit stage by the spine's fault seam, so the decided case is
//!   deterministic: stopped after its decision, `recover` finishes it and
//!   says `landed` with the ids and "do NOT import that batch again", and
//!   the soup holds one copy; stopped before it, `recover` rolls it back,
//!   says importing again is safe, and doing so leaves one copy.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde_json::{json, Value};

const CAULDRON: &str = env!("CARGO_BIN_EXE_cauldron");
const README: &str = include_str!("../../../schema/soup/workspace-v6/README.md");

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
    fn data(&self) -> PathBuf {
        self.root().join("data").join("dev")
    }
    fn soup(&self, name: &str) -> PathBuf {
        self.data().join("cauldrons").join(name).join("soup")
    }
    fn file(&self, name: &str, body: &str) -> PathBuf {
        let p = self.0.join("in").join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }
}

/// `root/`, `home/`, and an environment `dev` with one cauldron per name.
fn scratch(tag: &str, cauldrons: &[&str]) -> Scratch {
    let p = std::env::temp_dir().join(format!(
        "cauldron-soup-it-{tag}-{}-{}",
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
    std::fs::create_dir_all(s.data()).unwrap();
    for c in cauldrons {
        let o = run(&s, &["create", c, "--env", "dev"]);
        assert_eq!(o.rc, 0, "create {c}: {}", o.err);
        assert!(s.soup(c).join("ws.db").is_file(), "create made no ws pool");
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
        serde_json::from_str(&self.out).unwrap_or_else(|e| panic!("not JSON ({e}): {}\n{}", self.out, self.err))
    }
}

fn run(s: &Scratch, args: &[&str]) -> Out {
    let o = Command::new(CAULDRON)
        .args(args)
        .env("CAULDRON_HOME", s.root())
        .env("HOME", s.0.join("home"))
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .env_remove("CAULDRON_CAULDRON")
        .stdin(Stdio::null())
        .output()
        .expect("spawn cauldron");
    Out {
        rc: o.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&o.stdout).into_owned(),
        err: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

fn import(s: &Scratch, cauldron: &str, input: &Path, extra: &[&str]) -> Out {
    let mut args = vec!["soup", "import", input.to_str().unwrap(), "--env", "dev", "--cauldron", cauldron];
    args.extend_from_slice(extra);
    run(s, &args)
}

fn get(s: &Scratch, cauldron: &str, ids: &[&str]) -> Out {
    let mut args = vec!["soup", "get"];
    args.extend_from_slice(ids);
    args.extend_from_slice(&["--env", "dev", "--cauldron", cauldron]);
    run(s, &args)
}

/// The README's worked example, the exact block it publishes.
fn example() -> Vec<Value> {
    let at = README.find("## A worked example").expect("README has its worked example");
    let start = README[at..].find("```json").unwrap() + at + "```json".len();
    let end = README[start..].find("```").unwrap() + start;
    serde_json::from_str(&README[start..end]).expect("the worked example is JSON")
}

fn write_json(s: &Scratch, name: &str, v: &[Value]) -> PathBuf {
    s.file(name, &serde_json::to_string_pretty(v).unwrap())
}

/// Every entry under `dir`: relative path -> (len, mtime ns).
fn tree(dir: &Path) -> BTreeMap<String, (u64, i64)> {
    fn walk(base: &Path, d: &Path, out: &mut BTreeMap<String, (u64, i64)>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let m = std::fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(base).unwrap().display().to_string();
            out.insert(rel, (if m.is_dir() { 0 } else { m.len() }, m.mtime() * 1_000_000_000 + m.mtime_nsec()));
            if m.is_dir() {
                walk(base, &p, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// The bytes of every database file of a soup (side files excluded: SQLite
/// may create and remove `-wal`/`-shm` around a connection's life).
fn db_bytes(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("db"))
        .map(|p| (p.file_name().unwrap().to_str().unwrap().to_string(), std::fs::read(&p).unwrap()))
        .collect()
}

/// A record as `get` prints it, compared with what was written: edges as a
/// set (the store orders them by kind and target).
fn same_record(written: &Value, read: &Value) -> bool {
    let norm = |v: &Value| {
        let mut v = v.clone();
        if let Some(edges) = v.get_mut("edges").and_then(Value::as_array_mut) {
            edges.sort_by_key(|e| e.to_string());
        }
        v
    };
    norm(written) == norm(read)
}

fn ids(v: &[Value]) -> Vec<String> {
    v.iter().map(|r| r["id"].as_str().unwrap().to_string()).collect()
}

// ---------------------------------------------------------------------- S1

#[test]
fn s1_the_published_example_round_trips() {
    let s = scratch("s1", &["kb"]);
    let records = example();
    assert_eq!(records.len(), 11, "the README's example changed size");
    let input = write_json(&s, "example.json", &records);
    let all = ids(&records);
    let all: Vec<&str> = all.iter().map(String::as_str).collect();

    // A dry run judges everything and lands nothing.
    let o = import(&s, "kb", &input, &["--dry-run"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    let d = o.json();
    assert_eq!(d["outcome"], "validated");
    assert_eq!(d["accepted"], 11);
    let o = get(&s, "kb", &all);
    assert_eq!(o.rc, 1, "a dry run landed something: {}", o.out);
    assert_eq!(o.json()["missing"].as_array().unwrap().len(), 11);

    // The import lands all eleven as one write.
    let o = import(&s, "kb", &input, &[]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    let d = o.json();
    assert_eq!(d["outcome"], "landed", "{d}");
    assert_eq!(d["landed"]["seq"], 1, "not ONE spine row: {d}");

    // Read back, record for record.
    let o = get(&s, "kb", &all);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    let back = o.json()["records"].as_array().unwrap().clone();
    assert_eq!(back.len(), 11);
    for (w, r) in records.iter().zip(back.iter()) {
        assert!(same_record(w, r), "round trip changed a record:\nwrote {w}\nread  {r}");
    }

    let o = run(&s, &["soup", "regions", "--env", "dev", "--cauldron", "kb"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let r = o.json();
    let regions = r["regions"].as_array().unwrap();
    assert_eq!(ids(regions), ["R-1", "R-2"]);
    assert_eq!(regions[1]["relates"], json!(["R-1"]));
    assert!(regions[0]["boundary"].as_str().unwrap().starts_with("Inside:"));
    assert_eq!(regions[0]["members"], 4, "R-1 holds DIR-1, M-1, DEC-1, CON-1");

    // --max-chars cuts and says so.
    let o = run(&s, &["soup", "get", "IS-1", "--max-chars", "10", "--env", "dev", "--cauldron", "kb"]);
    let d = o.json();
    assert_eq!(d["records"][0]["body"].as_str().unwrap().chars().count(), 10);
    assert!(d["truncated"].as_array().unwrap().iter().any(|t| t["field"] == "body"), "{d}");
}

// ---------------------------------------------------------------------- S2

#[test]
fn s2_one_refused_record_leaves_the_store_untouched() {
    let s = scratch("s2", &["kb"]);
    let mut records = example();
    // DEC-1's handle one character over the cap.
    let dec = records.iter_mut().find(|r| r["id"] == "DEC-1").unwrap();
    dec["handle"] = json!("x".repeat(236));
    let input = write_json(&s, "bad.json", &records);
    let before = db_bytes(&s.soup("kb"));
    for extra in [&[][..], &["--remint"][..]] {
        let o = import(&s, "kb", &input, extra);
        assert_eq!(o.rc, 1, "accepted a 236-character handle: {}", o.out);
        let d = o.json();
        assert_eq!(d["outcome"], "refused");
        let dec = d["records"].as_array().unwrap().iter().find(|r| r["id"] == "DEC-1").unwrap();
        assert_eq!(dec["disposition"], "refused");
        assert!(dec["reasons"][0]["witnesses"].to_string().contains("handle"), "the door's reason is missing: {dec}");
        // The others were judged, and are reported accepted.
        let ok = d["records"].as_array().unwrap().iter().filter(|r| r["disposition"] == "accepted").count();
        assert!(ok >= 9, "{d}");
        assert_eq!(db_bytes(&s.soup("kb")), before, "a refused import changed the store ({extra:?})");
    }
    // A record that is not even a record is refused by shape, the same way.
    let input = write_json(&s, "shape.json", &[json!({"type": "issue", "handle": "no id"})]);
    let o = import(&s, "kb", &input, &[]);
    assert_eq!(o.rc, 1);
    assert_eq!(o.json()["records"][0]["reasons"][0]["code"], "record-shape");
    assert_eq!(db_bytes(&s.soup("kb")), before);
}

// ---------------------------------------------------------------------- S3

#[test]
fn s3_a_collision_without_remint_refuses_everything() {
    let s = scratch("s3", &["kb"]);
    let input = write_json(&s, "example.json", &example());
    assert_eq!(import(&s, "kb", &input, &[]).rc, 0);
    let before = db_bytes(&s.soup("kb"));
    let batch = vec![
        json!({"id": "T-9", "type": "task", "state": "ready", "handle": "A new task",
               "edges": [{"kind": "in_region", "to": "R-2", "note": "landing externally written records in a soup"}]}),
        json!({"id": "DEC-1", "type": "decision", "state": "decided", "handle": "A second DEC-1",
               "edges": [{"kind": "in_region", "to": "R-1"}]}),
    ];
    let o = import(&s, "kb", &write_json(&s, "clash.json", &batch), &[]);
    assert_eq!(o.rc, 1, "{}", o.out);
    let d = o.json();
    assert_eq!(d["outcome"], "refused");
    assert!(d["records"][1]["reasons"].to_string().contains("duplicate_node_id"), "{d}");
    assert_eq!(db_bytes(&s.soup("kb")), before, "a collision landed part of the batch");
    assert_eq!(get(&s, "kb", &["T-9"]).rc, 1, "T-9 landed beside a collision");
}

// ---------------------------------------------------------------------- S4

#[test]
fn s4_remint_twice_gives_two_disjoint_sets_with_their_own_references() {
    let s = scratch("s4", &["kb"]);
    // A region the soup already holds, outside both batches.
    let home = vec![json!({"id": "R-1", "type": "region", "handle": "Existing home",
        "boundary": "Inside: anything a test batch points at from outside. Outside: nothing else is here."})];
    assert_eq!(import(&s, "kb", &write_json(&s, "home.json", &home), &[]).rc, 0);

    // The example minus its viewer (one viewer per role), its ids turned
    // into labels that are not ids (`b-R-1`; a label equal to a held id is
    // refused, S7), plus records that sit in the EXISTING R-1 by reference:
    // bare, and `R-1@ws`. One-record YAML files.
    let mut batch: Vec<Value> = relabel(example().into_iter().filter(|r| r["type"] != "viewer").collect(), "b-");
    batch.push(json!({"id": "b-C-7", "type": "idea", "state": "idea", "handle": "Points outside the batch",
        "body": "Drawn from [[b-IS-1]].",
        "edges": [{"kind": "in_region", "to": "R-1", "note": "anything a test batch points at"},
                  {"kind": "ref", "to": "b-IS-1"}]}));
    batch.push(json!({"id": "b-C-8", "type": "idea", "state": "idea", "handle": "Points outside the batch explicitly",
        "edges": [{"kind": "in_region", "to": "R-1@ws", "note": "anything a test batch points at"}]}));
    for (i, r) in batch.iter().enumerate() {
        s.file(&format!("batch/{i:02}.yaml"), &serde_yaml::to_string(r).unwrap());
    }
    let dir = s.0.join("in").join("batch");

    let mut maps = Vec::new();
    for round in 0..2 {
        let o = import(&s, "kb", &dir, &["--remint"]);
        assert_eq!(o.rc, 0, "round {round}: {}\n{}", o.out, o.err);
        let d = o.json();
        assert_eq!(d["outcome"], "landed", "{d}");
        let map: BTreeMap<String, String> = serde_json::from_value(d["id_map"].clone()).unwrap();
        assert_eq!(map.len(), batch.len());
        maps.push(map);
    }
    // Index each map by the example's own id (`b-IS-1` -> `IS-1`).
    let maps: Vec<BTreeMap<String, String>> = maps
        .into_iter()
        .map(|m| m.into_iter().map(|(k, v)| (k.trim_start_matches("b-").to_string(), v)).collect())
        .collect();
    let (a, b) = (&maps[0], &maps[1]);
    let a_ids: std::collections::BTreeSet<&String> = a.values().collect();
    assert!(b.values().all(|id| !a_ids.contains(id)), "the two imports share ids: {a:?} {b:?}");
    // Next free serial per type: R-1 existed, so the first batch's regions are R-2, R-3.
    assert_eq!(a["R-1"], "R-2");
    assert_eq!(b["R-1"], "R-4");
    assert_eq!(a["IS-1"], "IS-1");
    assert_eq!(b["IS-1"], "IS-2");

    for map in &maps {
        let o = get(&s, "kb", &[&map["IS-1"], &map["M-1"], &map["C-7"], &map["R-2"], &map["C-8"]]);
        assert_eq!(o.rc, 0, "{}", o.err);
        let r = o.json()["records"].as_array().unwrap().clone();
        let edge = |rec: &Value, kind: &str| -> Vec<String> {
            rec["edges"].as_array().unwrap().iter().filter(|e| e["kind"] == kind).map(|e| e["to"].as_str().unwrap().to_string()).collect()
        };
        assert_eq!(edge(&r[0], "ref"), [map["DEC-1"].clone()], "IS-1's ref was not rewritten");
        assert_eq!(edge(&r[0], "in_region"), [map["R-2"].clone()]);
        assert!(r[0]["body"].as_str().unwrap().contains(&format!("[[{}]]", map["DEC-1"])), "body link not rewritten: {}", r[0]);
        assert_eq!(edge(&r[1], "serves"), [map["DIR-1"].clone()], "M-1's serves was not rewritten");
        assert_eq!(edge(&r[2], "in_region"), ["R-1"], "the reference outside the batch moved");
        assert_eq!(edge(&r[2], "ref"), [map["IS-1"].clone()]);
        assert!(r[2]["body"].as_str().unwrap().contains(&format!("[[{}]]", map["IS-1"])));
        assert_eq!(edge(&r[3], "relates"), [map["R-1"].clone()], "a region's relates was not rewritten");
        assert_eq!(edge(&r[4], "in_region"), ["R-1"], "the explicit `R-1@ws` moved");
    }
}

/// The records with every id `X` turned into the label `<prefix>X`, and
/// every edge and `[[link]]` between them rewritten to match.
fn relabel(records: Vec<Value>, prefix: &str) -> Vec<Value> {
    let ids: Vec<String> = ids(&records);
    records
        .into_iter()
        .map(|mut r| {
            r["id"] = json!(format!("{prefix}{}", r["id"].as_str().unwrap()));
            if let Some(edges) = r.get_mut("edges").and_then(Value::as_array_mut) {
                for e in edges {
                    let to = e["to"].as_str().unwrap().to_string();
                    if ids.contains(&to) {
                        e["to"] = json!(format!("{prefix}{to}"));
                    }
                }
            }
            if let Some(body) = r.get("body").and_then(Value::as_str).map(str::to_string) {
                let mut b = body;
                for id in &ids {
                    b = b.replace(&format!("[[{id}]]"), &format!("[[{prefix}{id}]]"));
                }
                r["body"] = json!(b);
            }
            r
        })
        .collect()
}

// ---------------------------------------------------------------------- S5

/// A live process this test owns, killed by its own pid on drop.
struct Sleeper(Child);
impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn s5_a_live_daemon_an_unreadable_seam_table_or_a_restless_soup_refuses() {
    let s = scratch("s5", &["kb"]);
    let input = write_json(&s, "example.json", &example());
    let soup = s.soup("kb");

    // A live soup row in the seam table.
    let sleeper = Sleeper(Command::new("/bin/sleep").arg("60").spawn().unwrap());
    let data_root = s.data().canonicalize().unwrap().display().to_string();
    let row = json!({
        "service": "soup", "addr": "127.0.0.1:1", "pid": sleeper.0.id(),
        "boot_token": "t", "version": "v", "commit": "", "dirty": false, "binary": "",
        "env": "dev", "data_root": data_root, "data_dir": "",
        "started_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
        "probe": "seam", "registered": true, "pid_source": "self",
    });
    let table = json!({"table_version": 1, "env": "dev", "data_root": data_root, "seams": {"soup": row}});
    let seams = s.data().join("seams.json");
    std::fs::write(&seams, serde_json::to_string_pretty(&table).unwrap()).unwrap();
    let before = tree(&soup);
    let o = import(&s, "kb", &input, &[]);
    assert_ne!(o.rc, 0, "imported with the soup daemon live: {}", o.out);
    for why in ["soup daemon is running", "--dry-run", "Nothing was written"] {
        assert!(o.err.contains(why), "the refusal does not say `{why}`: {}", o.err);
    }
    assert_eq!(tree(&soup), before, "a refusal touched the soup");
    // A dry run with the daemon live is judged on a read-only snapshot: it
    // answers, and the live soup is untouched (no file, length or mtime).
    let o = import(&s, "kb", &input, &["--dry-run"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    let d = o.json();
    assert_eq!(d["outcome"], "validated", "{d}");
    assert!(d["judged_on"].as_str().unwrap().contains("snapshot"), "{d}");
    assert_eq!(tree(&soup), before, "a dry run with the daemon live touched the soup");
    // OPPOSED: the process gone, it imports for real.
    drop(sleeper);
    let o = import(&s, "kb", &input, &["--dry-run"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);

    // A seam table that cannot be read proves nothing: refused.
    std::fs::write(&seams, "{ not json").unwrap();
    let o = import(&s, "kb", &input, &[]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("cannot prove"), "{}", o.err);
    std::fs::remove_file(&seams).unwrap();

    // A soup not at rest (a live tranche the daemon owns): refused, named.
    {
        let c = rusqlite::Connection::open(soup.join("spine.db")).unwrap();
        c.execute(
            "INSERT INTO tranches (id, attempt, seq, state, lock_mode, holder_service, key_hash, regions, \
             began_at, last_heartbeat, deadline) VALUES ('tr-test', 1, 99, 'open', 'whole', 'ripples', 'k', '[]', 1, 1, 1)",
            [],
        )
        .unwrap();
    }
    let before = db_bytes(&soup);
    let o = import(&s, "kb", &input, &[]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("not at rest") && o.err.contains("tranche"), "{}", o.err);
    assert_eq!(db_bytes(&soup), before);
}

// ---------------------------------------------------------------------- S6

#[test]
fn s6_reads_write_nothing_and_every_verb_needs_an_env() {
    let s = scratch("s6", &["kb"]);
    let input = write_json(&s, "example.json", &example());
    assert_eq!(import(&s, "kb", &input, &[]).rc, 0);
    let before = tree(&s.root());
    assert_eq!(run(&s, &["soup", "regions", "--env", "dev", "--cauldron", "kb"]).rc, 0);
    assert_eq!(get(&s, "kb", &["R-1", "V-1", "NOPE-1"]).rc, 1, "a missing id is not a success");
    assert_eq!(run(&s, &["soup", "nodes", "--env", "dev", "--cauldron", "kb"]).rc, 0);
    assert_eq!(run(&s, &["soup", "search", "schema", "--env", "dev", "--cauldron", "kb"]).rc, 0);
    assert_eq!(run(&s, &["soup", "region", "R-1", "--env", "dev", "--cauldron", "kb"]).rc, 0);
    assert_eq!(tree(&s.root()), before, "a read wrote something");

    for args in [
        &["soup", "regions", "--cauldron", "kb"][..],
        &["soup", "nodes", "--cauldron", "kb"][..],
        &["soup", "search", "schema", "--cauldron", "kb"][..],
        &["soup", "region", "R-1", "--cauldron", "kb"][..],
        &["soup", "get", "R-1", "--cauldron", "kb"][..],
        &["soup", "import", input.to_str().unwrap(), "--cauldron", "kb"][..],
    ] {
        let o = run(&s, args);
        assert_ne!(o.rc, 0, "{args:?} ran without an env");
        assert!(o.err.contains("never defaults to prod"), "{args:?}: {}", o.err);
    }
    let o = run(&s, &["soup", "regions", "--env", "dev", "--cauldron", "nope"]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("no cauldron `nope`"), "{}", o.err);
}

// ---------------------------------------------------------------------- S7

#[test]
fn s7_a_remint_label_equal_to_a_held_id_refuses_the_batch() {
    let s = scratch("s7", &["kb"]);
    assert_eq!(import(&s, "kb", &write_json(&s, "example.json", &example()), &[]).rc, 0);
    let before = db_bytes(&s.soup("kb"));

    // The acceptance run's reproduction: a new decision labelled `DEC-1`
    // and an issue whose bare `DEC-1` means the decision the soup holds.
    // Before the rule the issue's ref landed on the NEW decision, silently.
    let shadow = vec![
        json!({"id": "DEC-1", "type": "decision", "state": "decided", "handle": "A new decision labelled like a held id",
               "edges": [{"kind": "in_region", "to": "R-2"}]}),
        json!({"id": "n2", "type": "issue", "state": "open", "handle": "An issue meant to cite the held decision",
               "body": "Meant: the held [[DEC-1]].",
               "edges": [{"kind": "in_region", "to": "R-2"}, {"kind": "ref", "to": "DEC-1"}]}),
    ];
    let input = write_json(&s, "shadow.json", &shadow);
    for extra in [&["--remint", "--dry-run"][..], &["--remint"][..]] {
        let o = import(&s, "kb", &input, extra);
        assert_eq!(o.rc, 1, "a shadowing label was accepted ({extra:?}): {}", o.out);
        let d = o.json();
        assert_eq!(d["outcome"], "refused");
        let r = d["records"].as_array().unwrap();
        assert_eq!(r[0]["reasons"][0]["code"], "label-shadows-existing", "{d}");
        assert!(r[0]["reasons"][0]["detail"].as_str().unwrap().contains("Validate every record"), "names the held node: {d}");
        assert_eq!(r[1]["disposition"], "refused", "the record leaning on the shadow landed: {d}");
        assert_eq!(db_bytes(&s.soup("kb")), before, "a refused batch changed the store ({extra:?})");
    }

    // The same batch with a label that is not an id lands, and the issue's
    // bare `DEC-1` is the held decision.
    let mut fixed = shadow.clone();
    fixed[0]["id"] = json!("n1");
    let o = import(&s, "kb", &write_json(&s, "fixed.json", &fixed), &["--remint"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    let d = o.json();
    assert_eq!(d["id_map"]["n1"], "DEC-2");
    let issue = d["id_map"]["n2"].as_str().unwrap().to_string();
    let rec = get(&s, "kb", &[&issue]).json()["records"][0].clone();
    let refs: Vec<&str> = rec["edges"].as_array().unwrap().iter().filter(|e| e["kind"] == "ref").map(|e| e["to"].as_str().unwrap()).collect();
    assert_eq!(refs, ["DEC-1"], "the bare reference left the held node: {rec}");
    assert!(rec["body"].as_str().unwrap().contains("[[DEC-1]]"), "{rec}");

    // Without --remint ids are carried, and a held id is the door's
    // duplicate refusal (S3), not this one.
    let o = import(&s, "kb", &input, &["--dry-run"]);
    assert_eq!(o.rc, 1);
    assert!(o.out.contains("duplicate_node_id"), "{}", o.out);
}

// ---------------------------------------------------------------------- S8

fn read(s: &Scratch, verb: &str, extra: &[&str]) -> Value {
    let mut args = vec!["soup", verb];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--env", "dev", "--cauldron", "kb"]);
    let o = run(s, &args);
    assert_eq!(o.rc, 0, "{args:?}: {}\n{}", o.out, o.err);
    o.json()
}

#[test]
fn s8_nodes_search_and_region_place_and_deduplicate() {
    let s = scratch("s8", &["kb"]);
    assert_eq!(import(&s, "kb", &write_json(&s, "example.json", &example()), &[]).rc, 0);
    // A second, later write: a child region of R-1 and two issues in it.
    let later = vec![
        json!({"id": "r", "type": "region", "handle": "Schema drift checks",
               "boundary": "Inside: tests that hold a copy of the schema to its source. Outside: the schema itself (R-1).",
               "edges": [{"kind": "narrows", "to": "R-1"}]}),
        json!({"id": "a", "type": "issue", "state": "open", "handle": "The plugin's schema copy drifted from the published schema",
               "body": "A drift test compares the copy with its source.",
               "edges": [{"kind": "in_region", "to": "r"}]}),
        json!({"id": "b", "type": "issue", "state": "resolved", "handle": "A copy went stale",
               "body": "The schema copy in the plugin was older than the source; the drift check caught it.",
               "edges": [{"kind": "in_region", "to": "r"}]}),
    ];
    let o = import(&s, "kb", &write_json(&s, "later.json", &later), &["--remint"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    let m = o.json()["id_map"].clone();
    let (r3, a, b) = (m["r"].as_str().unwrap(), m["a"].as_str().unwrap(), m["b"].as_str().unwrap());
    assert_eq!(r3, "R-3");

    // nodes: newest first (the later write's three, highest serial first),
    // with every field the shape names.
    let d = read(&s, "nodes", &[]);
    let list = d["nodes"].as_array().unwrap();
    assert_eq!(d["total"], 14, "11 example records + 3: {d}");
    assert_eq!(ids(&list[..3]), [b, a, r3], "not newest first (one write: prefix, then serial down): {d}");
    for k in ["id", "type", "state", "handle", "regions"] {
        assert!(list[1].get(k).is_some(), "nodes lacks `{k}`: {}", list[1]);
    }
    assert_eq!(list[1]["regions"], json!([r3]));
    // Filters, and the limit.
    let d = read(&s, "nodes", &["--type", "issue"]);
    assert_eq!(ids(d["nodes"].as_array().unwrap()), [b, a, "IS-1"]);
    let d = read(&s, "nodes", &["--type", "issue", "--state", "open"]);
    assert_eq!(ids(d["nodes"].as_array().unwrap()), [a, "IS-1"]);
    let d = read(&s, "nodes", &["--region", "R-2"]);
    assert_eq!(ids(d["nodes"].as_array().unwrap()), ["IS-1", "Q-1", "T-1"], "R-2's direct members, one write");
    let d = read(&s, "nodes", &["--limit", "2", "--max-chars", "10"]);
    assert_eq!(d["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(d["more"], 12);
    assert_eq!(d["nodes"][1]["handle"].as_str().unwrap().chars().count(), 10);
    assert!(d["truncated"].as_array().unwrap().iter().any(|t| t["id"] == a && t["field"] == "handle"), "{d}");
    let o = run(&s, &["soup", "nodes", "--type", "nope", "--env", "dev", "--cauldron", "kb"]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("no node type `nope`"), "{}", o.err);

    // search: a handle hit outranks a body-only hit; ranking is stable.
    let d = read(&s, "search", &["schema", "copy"]);
    let res = d["results"].as_array().unwrap();
    assert_eq!(d["terms"], json!(["schema", "copy"]));
    assert_eq!(res[0]["id"], a, "both terms in the handle ranks first: {d}");
    assert_eq!(res[1]["id"], b, "both terms in the body only, second: {d}");
    for k in ["id", "type", "handle", "regions", "score", "snippet"] {
        assert!(res[0].get(k).is_some(), "search lacks `{k}`: {}", res[0]);
    }
    assert!(res[0]["score"].as_u64() > res[1]["score"].as_u64());
    assert_eq!(read(&s, "search", &["schema", "copy"]), d, "search is not deterministic");
    let d = read(&s, "search", &["drift", "--max-chars", "20", "--region", r3]);
    let res = d["results"].as_array().unwrap();
    assert_eq!(ids(res), [a, b], "{d}");
    assert!(res.iter().all(|x| x["snippet"].as_str().unwrap().chars().count() <= 22), "{d}");
    assert!(d["truncated"].as_array().unwrap().iter().any(|t| t["field"] == "body" && t["as"] == "snippet"), "{d}");
    let d = read(&s, "search", &["SCHEMA", "--type", "decision", "--limit", "1"]);
    assert_eq!(ids(d["results"].as_array().unwrap()), ["DEC-1"], "case-insensitive, type-narrowed: {d}");
    let d = read(&s, "search", &["zebra"]);
    assert_eq!(d["total"], 0);

    // region: the record, parents, children, relates both ways, members.
    let d = read(&s, "region", &["R-1"]);
    assert_eq!(d["region"]["id"], "R-1");
    assert!(d["region"]["boundary"].as_str().unwrap().starts_with("Inside:"));
    assert_eq!(d["children"], json!([{"id": r3, "handle": "Schema drift checks", "members": 2, "depth": 1}]));
    assert_eq!(d["parents"], json!([]));
    assert_eq!(ids(d["relates"].as_array().unwrap()), ["R-2"], "R-2 relates R-1, read from R-1's side");
    assert_eq!(d["members_total"], 4);
    assert_eq!(ids(d["members"].as_array().unwrap()), ["CON-1", "DEC-1", "DIR-1", "M-1"]);
    let d = read(&s, "region", &[r3, "--limit", "1", "--max-chars", "12"]);
    assert_eq!(d["parents"], json!([{"id": "R-1", "handle": "Publishing t", "members": 4, "depth": 1}]));
    assert_eq!(d["members_total"], 2);
    assert_eq!(d["more"], 1);
    assert_eq!(ids(d["members"].as_array().unwrap()), [b]);
    assert!(d["truncated"].as_array().unwrap().iter().any(|t| t["id"] == r3 && t["field"] == "boundary"), "{d}");
    let o = run(&s, &["soup", "region", "DEC-1", "--env", "dev", "--cauldron", "kb"]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("not a region"), "{}", o.err);
}

// ---------------------------------------------------------------------- S9

/// One region and `n` tasks in it, under batch-local labels (imported with
/// `--remint`).
fn load_batch(n: usize) -> Vec<Value> {
    let mut v = vec![json!({"id": "load-region", "type": "region", "handle": "Load",
        "boundary": "Inside: the load test's tasks. Outside: everything else."})];
    v.extend((1..=n).map(|i| {
        json!({"id": format!("load-task-{i}"), "type": "task", "state": "ready", "handle": format!("load task {i}"),
            "edges": [{"kind": "in_region", "to": "load-region", "note": "the load test"}]})
    }));
    v
}

/// A read-only connection to one of a soup's files.
fn ro(path: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn node_count(soup: &Path) -> i64 {
    ro(&soup.join("ws.db")).query_row("SELECT count(*) FROM nodes", [], |r| r.get(0)).unwrap()
}

/// `(decided, cleared)` of the first un-cleared transaction row, if any.
fn uncleared_row(soup: &Path) -> Option<(bool, bool)> {
    let c = rusqlite::Connection::open_with_flags(
        soup.join("spine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    c.busy_timeout(std::time::Duration::from_millis(50)).ok()?;
    c.query_row("SELECT decided, cleared FROM transactions WHERE cleared = 0", [], |r| {
        Ok((r.get::<_, i64>(0)? != 0, r.get::<_, i64>(1)? != 0))
    })
    .ok()
}

/// A seam table with a live `soup` row (the pid of a process this test owns).
fn live_soup_row(s: &Scratch, pid: u32) -> PathBuf {
    let data_root = s.data().canonicalize().unwrap().display().to_string();
    let row = json!({
        "service": "soup", "addr": "127.0.0.1:1", "pid": pid,
        "boot_token": "t", "version": "v", "commit": "", "dirty": false, "binary": "",
        "env": "dev", "data_root": data_root, "data_dir": "",
        "started_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
        "probe": "seam", "registered": true, "pid_source": "self",
    });
    let table = json!({"table_version": 1, "env": "dev", "data_root": data_root, "seams": {"soup": row}});
    let seams = s.data().join("seams.json");
    std::fs::write(&seams, serde_json::to_string_pretty(&table).unwrap()).unwrap();
    seams
}

fn recover(s: &Scratch) -> Out {
    run(s, &["soup", "recover", "--env", "dev", "--cauldron", "kb"])
}

#[test]
fn s9_a_killed_import_is_settled_by_recover_and_the_next_import_lands() {
    let s = scratch("s9", &["kb"]);
    let soup = s.soup("kb");
    let o = import(&s, "kb", &write_json(&s, "example.json", &example()), &[]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    let held = node_count(&soup);

    // THE KILL: a real import, killed the moment its transaction row is on
    // disk (the stage the long lock acquisition of a big batch holds open).
    let big = write_json(&s, "big.json", &load_batch(1000));
    let mut child = Command::new(CAULDRON)
        .args(["soup", "import", big.to_str().unwrap(), "--env", "dev", "--cauldron", "kb", "--remint"])
        .env("CAULDRON_HOME", s.root())
        .env("HOME", s.0.join("home"))
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .env_remove("CAULDRON_CAULDRON")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn cauldron");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let stuck = loop {
        if let Some(row) = uncleared_row(&soup) {
            child.kill().unwrap();
            child.wait().unwrap();
            break row;
        }
        if let Some(st) = child.try_wait().unwrap() {
            panic!("the import finished ({st}) before its row was seen; nothing was killed");
        }
        assert!(std::time::Instant::now() < deadline, "the import never wrote its row");
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    let (decided, _) = uncleared_row(&soup).expect("the killed import's row is still un-cleared");
    assert_eq!(decided, stuck.0);
    assert_eq!(node_count(&soup), held, "a killed import landed part of its batch");

    // THE WEDGE: every real import refuses, names `recover`, writes nothing.
    let small = write_json(&s, "small.json", &load_batch(3));
    let before = db_bytes(&soup);
    let o = import(&s, "kb", &small, &["--remint"]);
    assert_ne!(o.rc, 0, "imported into a soup not at rest: {}", o.out);
    for why in [
        "not at rest",
        "not cleared",
        "cauldron soup recover --env dev --cauldron kb",
        "do NOT import that batch again",
        "importing it again is safe",
        "Nothing was written",
    ] {
        assert!(o.err.contains(why), "the refusal does not say `{why}`: {}", o.err);
    }
    assert_eq!(db_bytes(&soup), before);

    // NEVER BESIDE A LIVE HOLDER: a live soup daemon, then another writer
    // holding the writer lock, each refuse `recover`, which writes nothing.
    let sleeper = Sleeper(Command::new("/bin/sleep").arg("60").spawn().unwrap());
    let seams = live_soup_row(&s, sleeper.0.id());
    let o = recover(&s);
    assert_ne!(o.rc, 0, "recovered beside a live soup daemon: {}", o.out);
    assert!(o.err.contains("soup daemon is running"), "{}", o.err);
    assert_eq!(db_bytes(&soup), before);
    std::fs::remove_file(&seams).unwrap();
    drop(sleeper);
    {
        let lock = std::fs::OpenOptions::new().write(true).open(soup.join("writer.lock")).unwrap();
        lock.try_lock().expect("the killed import's lock died with it");
        let o = recover(&s);
        assert_ne!(o.rc, 0, "recovered beside another writer: {}", o.out);
        assert!(o.err.contains("is writing to cauldron `kb`'s soup"), "{}", o.err);
        let o = import(&s, "kb", &small, &["--remint"]);
        assert_ne!(o.rc, 0);
        assert!(o.err.contains("is writing to cauldron `kb`'s soup"), "{}", o.err);
        assert_eq!(db_bytes(&soup), before);
    }

    // RECOVER: the killed import is rolled back (never decided: none of it
    // lands) or, had it decided, rolled forward whole; never a part.
    let o = recover(&s);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    let d = o.json();
    assert_eq!(d["outcome"], "recovered", "{d}");
    assert_eq!(d["after"], json!([]), "{d}");
    if decided {
        assert_eq!(d["rolled_forward"].as_array().unwrap().len(), 1, "{d}");
        assert_eq!(node_count(&soup), held + 1001, "a decided import rolls forward whole");
        assert_landed_outcome(&o, 1001);
    } else {
        assert_eq!(d["aborted"].as_array().unwrap().len(), 1, "{d}");
        assert_eq!(d["rolled_forward"], json!([]), "{d}");
        assert_eq!(node_count(&soup), held, "nothing of the killed import landed");
        assert_rolled_back_outcome(&o);
    }
    assert_eq!(uncleared_row(&soup), None);

    // At rest: `recover` opens nothing and writes nothing.
    let before = db_bytes(&soup);
    let o = recover(&s);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert_eq!(o.json()["outcome"], "at-rest");
    assert_eq!(o.json()["interrupted_import"], "none");
    assert!(o.err.contains("Nothing to recover"), "{}", o.err);
    assert_eq!(db_bytes(&soup), before);

    // And the next import lands.
    let after = node_count(&soup);
    let o = import(&s, "kb", &small, &["--remint"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert_eq!(o.json()["outcome"], "landed");
    assert_eq!(node_count(&soup), after + 4);
}

/// `recover` said the interrupted import LANDED, in its JSON and in words,
/// with the count and ids, and told the user not to import it again.
fn assert_landed_outcome(o: &Out, records: usize) {
    let d = o.json();
    assert_eq!(d["interrupted_import"], "landed", "{d}");
    assert_eq!(d["landed_records"], json!(records), "{d}");
    let counted: u64 = d["landed_ids"].as_array().unwrap().iter().map(|g| g["count"].as_u64().unwrap()).sum();
    assert_eq!(counted, records as u64, "{d}");
    for text in [d["instruction"].as_str().unwrap(), o.err.as_str()] {
        assert!(text.contains("LANDED"), "{text}");
        assert!(text.contains("Do NOT import that batch again"), "{text}");
        assert!(!text.contains("safe to import"), "{text}");
    }
    assert!(o.err.contains("interrupted import: landed"), "{}", o.err);
}

/// `recover` said the interrupted import was ROLLED BACK and that running
/// it again is safe.
fn assert_rolled_back_outcome(o: &Out) {
    let d = o.json();
    assert_eq!(d["interrupted_import"], "rolled-back", "{d}");
    assert!(d.get("landed_records").is_none(), "{d}");
    for text in [d["instruction"].as_str().unwrap(), o.err.as_str()] {
        assert!(text.contains("ROLLED BACK"), "{text}");
        assert!(text.contains("safe to import that batch again"), "{text}");
        assert!(!text.contains("Do NOT import"), "{text}");
    }
}

/// How many nodes of the soup carry this handle.
#[cfg(feature = "fault-injection")]
fn with_handle(soup: &Path, handle: &str) -> i64 {
    ro(&soup.join("ws.db"))
        .query_row("SELECT count(*) FROM node_fields WHERE field = 'handle' AND value = json_quote(?1)", [handle], |r| r.get(0))
        .unwrap()
}

/// A real `--remint` import of `input`, stopped by the spine's fault seam at
/// the named commit stage (the process aborts there, as a kill would).
#[cfg(feature = "fault-injection")]
fn import_stopped_at(s: &Scratch, input: &Path, stage: &str) -> Out {
    let o = Command::new(CAULDRON)
        .args(["soup", "import", input.to_str().unwrap(), "--env", "dev", "--cauldron", "kb", "--remint"])
        .env("CAULDRON_HOME", s.root())
        .env("HOME", s.0.join("home"))
        .env("SPINE_FAULT", stage)
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .env_remove("CAULDRON_CAULDRON")
        .stdin(Stdio::null())
        .output()
        .expect("spawn cauldron");
    Out {
        rc: o.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&o.stdout).into_owned(),
        err: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

// --------------------------------------------------------------------- S10

/// **S10** an import stopped AFTER its commit decision (the decided state a
/// real Ctrl-C a moment into a big import leaves) is FINISHED by `recover`,
/// which says so — `landed`, the count, the ids, "do NOT import that batch
/// again" — and a user who follows that ends with exactly one copy; the
/// carried-id import of the same batch then refuses on collision. An import
/// stopped BEFORE its decision is rolled back, `recover` says it is safe to
/// run again, and running it again also ends with exactly one copy.
/// Deterministic: the spine's fault seam stops the import at the named stage
/// (built only under `--features fault-injection`).
#[cfg(feature = "fault-injection")]
#[test]
fn s10_recover_says_whether_the_import_landed_and_following_it_leaves_one_copy() {
    // DECIDED: rolled forward, landed; do not import again.
    let s = scratch("s10d", &["kb"]);
    let soup = s.soup("kb");
    let o = import(&s, "kb", &write_json(&s, "example.json", &example()), &[]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    let held = node_count(&soup);
    let batch = write_json(&s, "batch.json", &load_batch(40));
    let o = import_stopped_at(&s, &batch, "decided");
    assert_ne!(o.rc, 0, "the seam did not stop the import: {}\n{}", o.out, o.err);
    assert_eq!(uncleared_row(&soup), Some((true, false)), "the stopped import left no decided row");
    assert_eq!(node_count(&soup), held, "nothing lands before recovery");

    let o = import(&s, "kb", &batch, &["--remint"]);
    assert_ne!(o.rc, 0, "imported into a soup not at rest: {}", o.out);
    assert!(o.err.contains("cauldron soup recover") && o.err.contains("do NOT import that batch again"), "{}", o.err);

    let o = recover(&s);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert_eq!(o.json()["outcome"], "recovered");
    assert_landed_outcome(&o, 41);
    let d = o.json();
    let tasks = d["landed_ids"].as_array().unwrap().iter().find(|g| g["count"] == 40).unwrap_or_else(|| panic!("no group of 40 tasks: {d}"));
    assert_eq!((tasks["count"].as_u64(), tasks["contiguous"].as_bool()), (Some(40), Some(true)), "{d}");
    assert!(o.err.contains(&format!("{}..{}", tasks["first"].as_str().unwrap(), tasks["last"].as_str().unwrap())), "{}", o.err);
    assert_eq!(node_count(&soup), held + 41, "a decided import rolls forward whole");
    // Following the printed instruction means no re-import: one copy.
    for i in [1, 20, 40] {
        assert_eq!(with_handle(&soup, &format!("load task {i}")), 1);
    }
    let o = recover(&s);
    assert_eq!(o.json()["interrupted_import"], "none", "{}", o.out);

    // ROW APPENDED, never decided: rolled back; importing again is safe.
    let s = scratch("s10a", &["kb"]);
    let soup = s.soup("kb");
    let held = node_count(&soup);
    let batch = write_json(&s, "batch.json", &load_batch(40));
    let o = import_stopped_at(&s, &batch, "row-appended");
    assert_ne!(o.rc, 0, "the seam did not stop the import: {}\n{}", o.out, o.err);
    assert_eq!(uncleared_row(&soup), Some((false, false)));
    let o = recover(&s);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert_rolled_back_outcome(&o);
    assert_eq!(node_count(&soup), held, "nothing of a rolled-back import lands");
    let o = import(&s, "kb", &batch, &["--remint"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert_eq!(node_count(&soup), held + 41);
    for i in [1, 20, 40] {
        assert_eq!(with_handle(&soup, &format!("load task {i}")), 1);
    }
}
