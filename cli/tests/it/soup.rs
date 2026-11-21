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
//!   says importing again is safe, and doing so leaves one copy. A
//!   re-import of the landed batch is refused as `batch-already-landed`.
//! - **S11** `--pool` must be a pool name, never a path, on every verb.
//! - **S12** a batch identical to one that landed is refused as
//!   `batch-already-landed` (file or directory, dry run or real, with or
//!   without `--remint`), naming when and how many; a different batch or
//!   another cauldron is not; `--allow-duplicate` lands a second copy.
//! - **S13** `cauldron link` writes a `.cauldron` project marker after
//!   checking what it names; soup verbs run in the project (or below) then
//!   need neither `--env` nor `--cauldron`; a flag or variable always wins;
//!   an explicit env that differs from the marker's refuses; a broken marker
//!   refuses; a directory named `.cauldron` is never a marker.
//! - **S18** every verb's stdout, stderr and exit code over a fixed session
//!   equal a recorded copy (`golden/soup.json`), so the soup logic can move
//!   between crates without changing a byte of what a caller reads.

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
        // The same batch a second time is refused as already landed (S12)
        // unless asked for: here it is, to check the two id sets.
        let extra: &[&str] = if round == 0 { &["--remint"] } else { &["--remint", "--allow-duplicate"] };
        let o = import(&s, "kb", &dir, extra);
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
    // And NOT following it is refused: the batch recovery finished is known
    // as landed, so a re-import under --remint lands nothing.
    let o = import(&s, "kb", &batch, &["--remint"]);
    assert_eq!(o.rc, 1, "re-imported a batch recover had landed: {}\n{}", o.out, o.err);
    assert_eq!(o.json()["refusal"], "batch-already-landed", "{}", o.out);
    assert_eq!(o.json()["landed_before"][0]["records"], 41, "{}", o.out);
    assert_eq!(node_count(&soup), held + 41, "a refused re-import landed something");
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

// --------------------------------------------------------------------- S11

#[test]
fn s11_pool_must_be_a_pool_name_never_a_path() {
    let s = scratch("s11", &["kb"]);
    let input = write_json(&s, "example.json", &example());
    // A file that a path-shaped `--pool` would otherwise reach, outside the soup.
    let outside = s.0.join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::copy(s.soup("kb").join("ws.db"), outside.join("stolen.db")).unwrap();
    let abs = outside.join("stolen");
    let abs = abs.to_str().unwrap();
    let before = db_bytes(&s.soup("kb"));
    for bad in [abs, "ws/../ws", "../ws", "WS", "1ws", "w_s", "", "spine"] {
        for verb in [&["soup", "nodes"][..], &["soup", "regions"][..], &["soup", "get", "IS-1"][..], &["soup", "search", "x"][..]] {
            let mut args = verb.to_vec();
            args.extend_from_slice(&["--env", "dev", "--cauldron", "kb", "--pool", bad]);
            let o = run(&s, &args);
            assert_eq!(o.rc, 1, "{verb:?} --pool {bad:?} was accepted: {}\n{}", o.out, o.err);
            assert!(o.err.contains("REFUSED") && o.err.contains("--pool"), "{verb:?} --pool {bad:?}: {}", o.err);
            assert!(o.out.is_empty(), "{verb:?} --pool {bad:?} printed a result: {}", o.out);
        }
        let mut args = vec!["soup", "import", input.to_str().unwrap(), "--env", "dev", "--cauldron", "kb"];
        args.push("--pool");
        args.push(bad);
        let o = run(&s, &args);
        assert_eq!(o.rc, 1, "import --pool {bad:?} was accepted: {}", o.out);
        assert!(o.err.contains("REFUSED") && o.err.contains("--pool"), "import --pool {bad:?}: {}", o.err);
    }
    // The `=` spelling is held to the same grammar.
    let o = run(&s, &["soup", "nodes", "--env", "dev", "--cauldron", "kb", &format!("--pool={abs}")]);
    assert_eq!(o.rc, 1, "--pool=<path> was accepted: {}", o.out);
    assert_eq!(db_bytes(&s.soup("kb")), before, "a refused --pool changed the store");
    // A well-formed name is still accepted: the default pool, spelled out.
    let o = run(&s, &["soup", "nodes", "--env", "dev", "--cauldron", "kb", "--pool", "ws"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert_eq!(o.json()["pool"], "ws");
}

// --------------------------------------------------------------------- S12

#[test]
fn s12_a_batch_that_already_landed_is_refused_unless_allowed() {
    let s = scratch("s12", &["kb", "other"]);
    let soup = s.soup("kb");
    // The example without its viewer (one viewer per role, so a second copy
    // of it would be refused for that alone), its ids as batch labels.
    let records = relabel(example().into_iter().filter(|r| r["type"] != "viewer").collect(), "b-");
    let input = write_json(&s, "batch.json", &records);
    let o = import(&s, "kb", &input, &["--remint"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    let first = o.json();
    assert_eq!(first["outcome"], "landed");
    let digest = first["batch_digest"].as_str().unwrap().to_string();
    assert_eq!(digest.len(), 64, "{first}");
    let held = node_count(&soup);

    // The same records again, as the same file, as a YAML directory, in a
    // dry run and for real, with and without --remint: refused by name,
    // saying when it landed and how many records it held; nothing written.
    for (i, r) in records.iter().enumerate() {
        s.file(&format!("dir/{i:02}.yaml"), &serde_yaml::to_string(r).unwrap());
    }
    let dir = s.0.join("in").join("dir");
    let before = db_bytes(&soup);
    for (src, extra) in [
        (&input, &["--remint"][..]),
        (&input, &["--remint", "--dry-run"][..]),
        (&dir, &["--remint"][..]),
        (&input, &[][..]),
    ] {
        let o = import(&s, "kb", src, extra);
        assert_eq!(o.rc, 1, "{src:?} {extra:?} was not refused: {}", o.out);
        let d = o.json();
        assert_eq!(d["outcome"], "refused", "{d}");
        assert_eq!(d["refusal"], "batch-already-landed", "{d}");
        assert_eq!(d["batch_digest"], digest.as_str(), "{d}");
        let earlier = d["landed_before"].as_array().unwrap();
        assert_eq!(earlier.len(), 1, "{d}");
        assert_eq!(earlier[0]["seq"], first["landed"]["seq"], "{d}");
        assert_eq!(earlier[0]["records"], records.len(), "{d}");
        assert!(earlier[0]["landed_at"].as_str().unwrap().contains('T'), "{d}");
        assert!(o.err.contains("batch-already-landed") && o.err.contains("--allow-duplicate"), "{}", o.err);
        assert_eq!(db_bytes(&soup), before, "a refused re-import changed the store ({extra:?})");
    }

    // A different batch is not refused; nor is this one in another cauldron.
    let mut changed = records.clone();
    changed[0]["handle"] = json!("A different first handle");
    let o = import(&s, "kb", &write_json(&s, "changed.json", &changed), &["--remint", "--dry-run"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert_eq!(o.json()["outcome"], "validated");
    let o = import(&s, "other", &input, &["--remint"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);

    // --allow-duplicate lands the second copy, under new ids; a third try
    // names both earlier landings.
    let o = import(&s, "kb", &input, &["--remint", "--allow-duplicate"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert_eq!(o.json()["outcome"], "landed");
    assert_eq!(node_count(&soup), held + records.len() as i64);
    let o = import(&s, "kb", &input, &["--remint"]);
    assert_eq!(o.rc, 1);
    assert_eq!(o.json()["landed_before"].as_array().unwrap().len(), 2, "{}", o.out);
}

// --------------------------------------------------------------------- S13

/// `run`, in directory `cwd`, with extra environment variables.
fn run_in(s: &Scratch, cwd: &Path, vars: &[(&str, &str)], args: &[&str]) -> Out {
    let mut c = Command::new(CAULDRON);
    c.args(args)
        .current_dir(cwd)
        .env("CAULDRON_HOME", s.root())
        .env("HOME", s.0.join("home"))
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .env_remove("CAULDRON_CAULDRON")
        .stdin(Stdio::null());
    for (k, v) in vars {
        c.env(k, v);
    }
    let o = c.output().expect("spawn cauldron");
    Out {
        rc: o.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&o.stdout).into_owned(),
        err: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

#[test]
fn s13_a_linked_project_directory_resolves_to_its_cauldron() {
    let s = scratch("s13", &["kb", "other"]);
    let proj = s.0.join("proj");
    let sub = proj.join("src").join("deep");
    std::fs::create_dir_all(&sub).unwrap();
    let marker = proj.join(".cauldron");

    // Unlinked: the verbs still ask for --env, and say how to link.
    let o = run_in(&s, &sub, &[], &["soup", "nodes"]);
    assert_ne!(o.rc, 0, "{}", o.out);
    assert!(o.err.contains("needs --env") && o.err.contains("cauldron link"), "{}", o.err);

    // `link` checks what it names and writes nothing on a refusal.
    let o = run_in(&s, &proj, &[], &["link", "nope", "--env", "dev"]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("no cauldron `nope`"), "{}", o.err);
    let o = run_in(&s, &proj, &[], &["link", "kb"]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("needs --env"), "{}", o.err);
    let o = run_in(&s, &proj, &[], &["link", "kb", "--env", "nowhere"]);
    assert_ne!(o.rc, 0);
    assert!(!marker.exists(), "a refused link wrote a marker");

    // Linked.
    let o = run_in(&s, &sub, &[], &["link", "kb", "--env", "dev", "--dir", proj.to_str().unwrap()]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert_eq!(o.json()["outcome"], "linked");
    let text = std::fs::read_to_string(&marker).unwrap();
    assert!(text.contains("env = dev\n") && text.contains("cauldron = kb\n"), "{text}");
    let o = run_in(&s, &proj, &[], &["link", "kb", "--env", "dev"]);
    assert_eq!(o.json()["outcome"], "unchanged");

    // `--show` from a directory below: both from the marker.
    let o = run_in(&s, &sub, &[], &["link", "--show"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let d = o.json();
    assert_eq!((d["env"].as_str(), d["cauldron"].as_str()), (Some("dev"), Some("kb")), "{d}");
    assert!(d["env_from"].as_str().unwrap().contains("project marker"), "{d}");
    assert_eq!(d["marker"], marker.to_str().unwrap());
    assert_eq!(d["cauldron_exists"], true);

    // Soup verbs need neither flag inside the project; an import lands.
    let input = write_json(&s, "example.json", &example());
    let o = run_in(&s, &sub, &[], &["soup", "import", input.to_str().unwrap()]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    let d = o.json();
    assert_eq!((d["env"].as_str(), d["cauldron"].as_str(), d["outcome"].as_str()), (Some("dev"), Some("kb"), Some("landed")), "{d}");
    assert!(o.err.contains("project marker") && o.err.contains(marker.to_str().unwrap()), "{}", o.err);
    let o = run_in(&s, &sub, &[], &["soup", "get", "DEC-1"]);
    assert_eq!(o.rc, 0, "{}", o.err);

    // Explicit always wins: --cauldron, $CAULDRON_CAULDRON (env still from the
    // marker), and both flags (marker not even read, so no note).
    let o = run_in(&s, &sub, &[], &["soup", "nodes", "--cauldron", "other"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert_eq!(o.json()["cauldron"], "other");
    assert_eq!(o.json()["total"], 0, "the marker's cauldron was read instead");
    let o = run_in(&s, &sub, &[("CAULDRON_CAULDRON", "other")], &["soup", "nodes"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert_eq!(o.json()["cauldron"], "other");
    let o = run_in(&s, &sub, &[], &["soup", "nodes", "--env", "dev", "--cauldron", "other"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert!(!o.err.contains("marker"), "{}", o.err);

    // A different explicit env with the marker's cauldron: refused, never
    // looked for elsewhere; nor by the variable.
    std::fs::create_dir_all(s.root().join("data").join("prod")).unwrap();
    for (vars, args) in [
        (&[][..], &["soup", "nodes", "--env", "prod"][..]),
        (&[("CAULDRON_ENV", "prod")][..], &["soup", "nodes"][..]),
    ] {
        let o = run_in(&s, &sub, vars, args);
        assert_ne!(o.rc, 0, "{}", o.out);
        assert!(o.err.contains("REFUSED") && o.err.contains("in environment `dev`") && o.err.contains("`prod`"), "{}", o.err);
    }

    // A variable naming no usable environment is an error, named, and the
    // marker is not used in its place.
    for bad in ["PROD", "shared"] {
        let o = run_in(&s, &sub, &[("CAULDRON_ENV", bad)], &["soup", "nodes"]);
        assert_ne!(o.rc, 0, "{bad}: {}", o.out);
        assert!(o.err.contains("$CAULDRON_ENV") && !o.err.contains("project marker"), "{bad}: {}", o.err);
    }

    // Re-pointing needs --replace.
    let o = run_in(&s, &proj, &[], &["link", "other", "--env", "dev"]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("--replace"), "{}", o.err);
    let o = run_in(&s, &proj, &[], &["link", "other", "--env", "dev", "--replace"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert_eq!(run_in(&s, &sub, &[], &["soup", "nodes"]).json()["cauldron"], "other");

    // A nearer marker that does not parse refuses, naming itself.
    let broken = sub.join(".cauldron");
    std::fs::write(&broken, "cauldron: kb\n").unwrap();
    let o = run_in(&s, &sub, &[], &["soup", "nodes"]);
    assert_ne!(o.rc, 0, "{}", o.out);
    assert!(o.err.contains(broken.to_str().unwrap()), "{}", o.err);

    // A directory named `.cauldron` (as the data root is) is not a marker,
    // and cannot be linked into.
    let home_like = s.0.join("homelike");
    std::fs::create_dir_all(home_like.join(".cauldron")).unwrap();
    let o = run_in(&s, &home_like, &[], &["soup", "nodes"]);
    assert!(o.err.contains("needs --env"), "{}", o.err);
    let o = run_in(&s, &home_like, &[], &["link", "kb", "--env", "dev"]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("not a file"), "{}", o.err);
}

#[test]
fn s14_link_never_writes_through_a_symbolic_link() {
    use std::os::unix::fs::symlink;
    let s = scratch("s14", &["kb"]);
    let victim = s.0.join("victim");
    std::fs::create_dir_all(&victim).unwrap();

    // A dangling link: nothing is created where it points, with or without --replace.
    let repo = s.0.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    symlink(victim.join("created-by-link"), repo.join(".cauldron")).unwrap();
    for extra in [&[][..], &["--replace"][..]] {
        let mut args = vec!["link", "kb", "--env", "dev"];
        args.extend_from_slice(extra);
        let o = run_in(&s, &repo, &[], &args);
        assert_ne!(o.rc, 0, "{}", o.out);
        assert!(o.err.contains("symbolic link") && o.err.contains(repo.join(".cauldron").to_str().unwrap()), "{}", o.err);
        assert!(!victim.join("created-by-link").exists(), "link wrote through a dangling symlink");
    }
    assert!(std::fs::symlink_metadata(repo.join(".cauldron")).unwrap().file_type().is_symlink());

    // A live link to someone's file: refused, and --replace does not overwrite it.
    let repo2 = s.0.join("repo2");
    std::fs::create_dir_all(&repo2).unwrap();
    std::fs::write(victim.join("precious.txt"), "precious\n").unwrap();
    symlink(victim.join("precious.txt"), repo2.join(".cauldron")).unwrap();
    for extra in [&[][..], &["--replace"][..]] {
        let mut args = vec!["link", "kb", "--env", "dev", "--dir", repo2.to_str().unwrap()];
        args.extend_from_slice(extra);
        let o = run_in(&s, &s.0, &[], &args);
        assert_ne!(o.rc, 0, "{}", o.out);
        assert!(o.err.contains("symbolic link"), "{}", o.err);
        assert_eq!(std::fs::read_to_string(victim.join("precious.txt")).unwrap(), "precious\n");
    }

    // The reader does not follow either link, and says explicit flags work.
    for dir in [&repo, &repo2] {
        let o = run_in(&s, dir, &[], &["soup", "nodes"]);
        assert_ne!(o.rc, 0, "{}", o.out);
        assert!(o.err.contains("symbolic link") && o.err.contains("--env and --cauldron"), "{}", o.err);
        let o = run_in(&s, dir, &[], &["soup", "nodes", "--env", "dev", "--cauldron", "kb"]);
        assert_eq!(o.rc, 0, "{}", o.err);
    }

    // --replace replaces a regular file, through a new file renamed into place.
    let repo3 = s.0.join("repo3");
    std::fs::create_dir_all(&repo3).unwrap();
    std::fs::write(repo3.join(".cauldron"), "not a marker\n").unwrap();
    let o = run_in(&s, &repo3, &[], &["link", "kb", "--env", "dev"]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("--replace"), "{}", o.err);
    let o = run_in(&s, &repo3, &[], &["link", "kb", "--env", "dev", "--replace"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert_eq!(o.json()["outcome"], "linked");
    let text = std::fs::read_to_string(repo3.join(".cauldron")).unwrap();
    assert!(text.contains("env = dev\n") && text.contains("cauldron = kb\n"), "{text}");
    let left: Vec<_> = std::fs::read_dir(&repo3).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(left, vec![std::ffi::OsString::from(".cauldron")], "a temporary file was left behind");
}

#[test]
fn s15_both_flags_need_no_current_directory() {
    let s = scratch("s15", &["kb"]);
    let input = write_json(&s, "example.json", &example());
    let gone = s.0.join("gone");
    std::fs::create_dir_all(&gone).unwrap();
    // A child whose working directory is removed before it reads it.
    let spawn = |args: &[&str]| -> Out {
        let mut c = Command::new("/bin/sh");
        c.arg("-c")
            .arg(r#"cd "$1" && rmdir "$1" && shift && exec "$@""#)
            .arg("sh")
            .arg(&gone)
            .arg(CAULDRON)
            .args(args)
            .env("CAULDRON_HOME", s.root())
            .env("HOME", s.0.join("home"))
            .env_remove("CAULDRON_ENV")
            .env_remove("ENV")
            .env_remove("CAULDRON_CAULDRON")
            .stdin(Stdio::null());
        let o = c.output().expect("spawn sh");
        Out { rc: o.status.code().unwrap_or(-1), out: String::from_utf8_lossy(&o.stdout).into_owned(), err: String::from_utf8_lossy(&o.stderr).into_owned() }
    };
    let o = spawn(&["soup", "import", input.to_str().unwrap(), "--env", "dev", "--cauldron", "kb"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert!(!gone.exists());
    std::fs::create_dir_all(&gone).unwrap();
    let o = spawn(&["soup", "nodes", "--env", "dev", "--cauldron", "kb"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert_eq!(o.json()["total"], example().len());
    // With a value missing, the marker is needed, and the refusal says why.
    std::fs::create_dir_all(&gone).unwrap();
    let o = spawn(&["soup", "nodes", "--env", "dev"]);
    assert_ne!(o.rc, 0, "{}", o.out);
    assert!(o.err.contains("current directory") && o.err.contains("--cauldron"), "{}", o.err);
}

#[test]
fn s16_a_marker_is_reported_and_a_tracked_marker_cannot_aim_a_write() {
    if Command::new("git").arg("--version").output().map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("s16: git is not available; skipped");
        return;
    }
    let s = scratch("s16", &["kb"]);
    let input = write_json(&s, "example.json", &example());
    let git = |dir: &Path, args: &[&str]| {
        let o = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-C"])
            .arg(dir)
            .args(args)
            .env("HOME", s.0.join("home"))
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    };

    // `link` in a git work tree hints at .gitignore until the marker is ignored.
    let proj = s.0.join("proj");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    git(&proj, &["init", "-q"]);
    let o = run_in(&s, &proj, &[], &["link", "kb", "--env", "dev"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert!(o.err.contains("hint") && o.err.contains(".gitignore"), "{}", o.err);
    std::fs::write(proj.join(".gitignore"), ".cauldron\n").unwrap();
    let o = run_in(&s, &proj, &[], &["link", "kb", "--env", "dev"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert!(!o.err.contains("hint"), "{}", o.err);
    let o = run_in(&s, &s.0, &[], &["link", "kb", "--env", "dev", "--dir", s.0.join("home").to_str().unwrap()]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert!(!o.err.contains("hint"), "outside a work tree: {}", o.err);

    // An ignored marker aims writes, and every JSON says where the target came from.
    let marker = proj.join(".cauldron");
    let from = format!("marker:{}", marker.display());
    let o = run_in(&s, &proj.join("src"), &[], &["soup", "import", input.to_str().unwrap()]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert_eq!(o.json()["resolved_from"], from.as_str());
    assert_eq!(run_in(&s, &proj, &[], &["link", "--show"]).json()["writes_refused"], Value::Null);
    let o = run_in(&s, &proj.join("src"), &[], &["soup", "import", input.to_str().unwrap()]);
    assert_eq!(o.rc, 1, "{}", o.err);
    assert_eq!((o.json()["refusal"].as_str(), o.json()["resolved_from"].as_str()), (Some("batch-already-landed"), Some(from.as_str())));
    for args in [&["soup", "nodes"][..], &["soup", "regions"][..], &["soup", "get", "DEC-1"][..], &["soup", "search", "soup"][..]] {
        let o = run_in(&s, &proj.join("src"), &[], args);
        assert_eq!(o.rc, 0, "{args:?}: {}", o.err);
        assert_eq!(o.json()["resolved_from"], from.as_str(), "{args:?}");
    }
    let o = run_in(&s, &proj.join("src"), &[], &["soup", "nodes", "--env", "dev", "--cauldron", "kb"]);
    assert!(o.json().get("resolved_from").is_none(), "{}", o.out);

    // Committed, the marker no longer aims a write; a dry run, a read and
    // explicit flags still work.
    std::fs::write(proj.join(".gitignore"), "").unwrap();
    git(&proj, &["add", ".cauldron"]);
    git(&proj, &["commit", "-qm", "marker"]);
    let mut fresh = example()[0].clone();
    fresh["id"] = json!("R-9");
    let other = write_json(&s, "other.json", &[fresh]);
    let before = tree(&s.soup("kb"));
    for args in [&["soup", "import", other.to_str().unwrap()][..], &["soup", "recover"][..]] {
        let o = run_in(&s, &proj.join("src"), &[], args);
        assert_ne!(o.rc, 0, "{args:?}: {}", o.out);
        assert!(o.err.contains("REFUSED") && o.err.contains("tracked by git") && o.err.contains(marker.to_str().unwrap()), "{}", o.err);
        assert!(o.err.contains("--env dev --cauldron kb"), "{}", o.err);
    }
    assert_eq!(tree(&s.soup("kb")), before, "a refused write touched the soup");
    let o = run_in(&s, &proj.join("src"), &[], &["soup", "import", other.to_str().unwrap(), "--dry-run"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert_eq!(o.json()["resolved_from"], from.as_str());
    let o = run_in(&s, &proj.join("src"), &[], &["soup", "nodes"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let o = run_in(&s, &proj, &[], &["link", "kb", "--env", "dev"]);
    assert!(o.err.contains("tracked by git"), "{}", o.err);
    let o = run_in(&s, &proj.join("src"), &[], &["link", "--show"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert!(o.json()["writes_refused"].as_str().is_some_and(|w| w.contains("tracked by git")), "{}", o.out);
    let o = run_in(&s, &proj.join("src"), &[], &["soup", "import", other.to_str().unwrap(), "--env", "dev", "--cauldron", "kb"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert!(o.json().get("resolved_from").is_none());
}

#[test]
fn s17_a_mark_in_an_agent_label_or_file_name_is_not_a_landing() {
    let s = scratch("s17", &["kb"]);
    let ex = example();
    let b = write_json(&s, "b.json", &ex[..2]);
    let o = import(&s, "kb", &b, &["--dry-run"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let digest = o.json()["batch_digest"].as_str().unwrap().to_string();
    let mark = format!("[batch sha256:{digest}, 2 records]");

    // A different batch whose --agent label and file name both carry b's mark.
    let a = write_json(&s, &format!("a {mark}.json"), &ex[..1]);
    let label = format!("bot {mark}");
    let o = import(&s, "kb", &a, &["--agent", &label]);
    assert_eq!((o.rc, o.json()["outcome"].as_str()), (0, Some("landed")), "{}", o.err);

    // b itself has never landed, so it is not refused. It collides on R-1 under
    // carried ids, so --remint; the guard is judged before the door.
    let o = import(&s, "kb", &b, &["--remint", "--dry-run"]);
    assert_ne!(o.json()["refusal"], "batch-already-landed", "{}", o.out);
    let mut fresh = ex[1].clone();
    fresh["id"] = json!("R-7");
    let c = write_json(&s, "c.json", &[fresh]);
    let o = import(&s, "kb", &c, &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    // And a real landing is still found by its own mark.
    let o = import(&s, "kb", &c, &[]);
    assert_eq!((o.rc, o.json()["refusal"].as_str()), (1, Some("batch-already-landed")), "{}", o.out);
    assert_eq!(o.json()["landed_before"][0]["records"], 1);
}

// ------------------------------------------------- the consumer's issues

/// Issue #19: a refused `--remint` import printed an `id_map` (and each
/// record's `new_id`) for ids that were never taken; the next import then
/// gave one of them to another record. Now a refused batch prints neither,
/// and a batch that lands or validates still does.
#[test]
fn i19_a_refused_remint_prints_no_id_map() {
    let s = scratch("i19", &["kb"]);
    assert_eq!(import(&s, "kb", &write_json(&s, "example.json", &example()), &[]).rc, 0);
    // His reproduction: a case that the grammar refuses, and a pattern drawn
    // from it (refused with it as `target-refused`).
    let refused = vec![
        json!({"id": "CASE-1", "type": "idea", "state": "no-such-state", "handle": "A case that will not land",
               "edges": [{"kind": "in_region", "to": "R-1", "note": "the published schema"}]}),
        json!({"id": "PAT-1", "type": "decision", "state": "decided", "handle": "A pattern drawn from the case",
               "edges": [{"kind": "in_region", "to": "R-1", "note": "the published schema"}, {"kind": "derived_from", "to": "CASE-1"}]}),
    ];
    let input = write_json(&s, "refused.json", &refused);
    for extra in [&["--remint"][..], &["--remint", "--dry-run"][..]] {
        let o = import(&s, "kb", &input, extra);
        assert_eq!(o.rc, 1, "{extra:?}: {}", o.out);
        let d = o.json();
        assert_eq!(d["outcome"], "refused", "{d}");
        assert!(d.get("id_map").is_none(), "a refused batch printed an id_map: {d}");
        assert!(d["records"].as_array().unwrap().iter().all(|r| r.get("new_id").is_none()), "a refused record carries a new_id: {d}");
        assert_eq!(d["refused"], 2, "{d}");
    }
    // Opposed: the same batch without the bad state validates and lands,
    // and both answers carry the map and each record's new id.
    let mut good = refused.clone();
    good[0]["state"] = json!("idea");
    let input = write_json(&s, "good.json", &good);
    for (extra, outcome) in [(&["--remint", "--dry-run"][..], "validated"), (&["--remint"][..], "landed")] {
        let o = import(&s, "kb", &input, extra);
        assert_eq!(o.rc, 0, "{extra:?}: {}\n{}", o.out, o.err);
        let d = o.json();
        assert_eq!(d["outcome"], outcome, "{d}");
        assert_eq!(d["id_map"], json!({"CASE-1": "C-2", "PAT-1": "DEC-2"}), "{d}");
        assert_eq!(d["records"][1]["new_id"], "DEC-2", "{d}");
    }
}

/// The keys every refused batch prints, whatever refused it (#20).
const REFUSED_SHAPE: [&str; 11] =
    ["env", "cauldron", "pool", "dry_run", "remint", "outcome", "refusal", "accepted", "refused", "records", "batch_digest"];

fn assert_refused_shape(d: &Value, refusal: &str) {
    for k in REFUSED_SHAPE {
        assert!(d.get(k).is_some(), "a refused batch without `{k}`: {d}");
    }
    assert_eq!(d["outcome"], "refused", "{d}");
    assert_eq!(d["refusal"], refusal, "{d}");
    assert!(d["accepted"].is_u64() && d["refused"].is_u64() && d["records"].is_array(), "{d}");
}

/// Issue #20: `batch-already-landed` printed another JSON shape than every
/// other import (no `accepted`, `refused` or `records`). Now every refused
/// batch prints the same keys, and says what refused it in `refusal`.
#[test]
fn i20_a_batch_already_landed_prints_the_shape_of_every_refused_batch() {
    let s = scratch("i20", &["kb"]);
    let input = write_json(&s, "example.json", &example());
    assert_eq!(import(&s, "kb", &input, &[]).rc, 0);
    // His reproduction: the same batch again, for real and as a dry run.
    for extra in [&[][..], &["--dry-run"][..]] {
        let o = import(&s, "kb", &input, extra);
        assert_eq!(o.rc, 1, "{extra:?}: {}", o.out);
        let d = o.json();
        assert_refused_shape(&d, "batch-already-landed");
        assert_eq!((d["accepted"].as_u64(), d["refused"].as_u64()), (Some(0), Some(example().len() as u64)), "{d}");
        assert_eq!(d["records"], json!([]), "{d}");
        assert!(d["landed_before"].is_array() && d["why"].is_string(), "{d}");
    }
    // The other refusal, a record the grammar refuses: the same keys.
    let bad = write_json(&s, "bad.json", &[json!({"id": "b", "type": "task", "state": "no-such-state", "handle": "A bad task",
                                                     "edges": [{"kind": "in_region", "to": "R-1"}]})]);
    for extra in [&["--remint"][..], &["--remint", "--dry-run"][..]] {
        let o = import(&s, "kb", &bad, extra);
        assert_eq!(o.rc, 1, "{extra:?}: {}", o.out);
        let d = o.json();
        assert_refused_shape(&d, "records-refused");
        assert_eq!(d["records"][0]["id"], "b", "{d}");
    }
    // Opposed: a batch that lands carries no `refusal`.
    let good = write_json(&s, "good.json", &[json!({"id": "g", "type": "task", "state": "ready", "handle": "A good task",
                                                      "edges": [{"kind": "in_region", "to": "R-1"}]})]);
    let d = import(&s, "kb", &good, &["--remint"]).json();
    assert_eq!(d["outcome"], "landed", "{d}");
    assert!(d.get("refusal").is_none(), "{d}");
}

/// Issue #18: an edge with a `note` and an undeclared key (`weight`) was
/// refused as carrying "both `note` and declared fields", though `weight` is
/// declared for no edge. Now the reason names the key.
#[test]
fn i18_an_unknown_edge_key_beside_a_note_is_named() {
    let s = scratch("i18", &["kb"]);
    assert_eq!(import(&s, "kb", &write_json(&s, "example.json", &example()), &[]).rc, 0);
    // His reproduction.
    let rec = |edge: Value| json!({"id": "n1", "type": "decision", "state": "decided", "handle": "A decision with a weighted region edge", "edges": [edge]});
    let input = write_json(&s, "weight.json", &[rec(json!({"kind": "in_region", "to": "R-1", "note": "the published schema", "weight": 1}))]);
    for extra in [&["--remint", "--dry-run"][..], &["--remint"][..]] {
        let o = import(&s, "kb", &input, extra);
        assert_eq!(o.rc, 1, "{extra:?}: {}", o.out);
        let r = &o.json()["records"][0]["reasons"][0];
        assert_eq!(r["code"], "record-shape", "{r}");
        assert_eq!(r["edge"], 0, "{r}");
        let why = r["detail"].as_str().unwrap();
        assert!(why.contains("`weight` is not a field of `in_region` edges"), "{why}");
        assert!(!why.contains("both `note` and declared fields"), "{why}");
    }
    // Without the note the door names it too, as before.
    let input = write_json(&s, "weight-only.json", &[rec(json!({"kind": "in_region", "to": "R-1", "weight": 1}))]);
    let o = import(&s, "kb", &input, &["--remint", "--dry-run"]);
    assert_eq!(o.rc, 1);
    assert!(o.out.contains("edge kind `in_region` declares no field `weight`"), "{}", o.out);
    // The note-or-fields reason is kept for a kind that declares fields.
    let viewer = json!({"id": "v", "type": "viewer", "role": "reviewer",
                        "edges": [{"kind": "shows", "to": "DEC-1", "note": "x", "why": "the decision under review", "cite": {"thread": "t", "msg": 1}}]});
    let o = import(&s, "kb", &write_json(&s, "viewer.json", &[viewer]), &["--remint", "--dry-run"]);
    assert_eq!(o.rc, 1);
    assert!(o.json()["records"][0]["reasons"][0]["detail"].as_str().unwrap().contains("carries both `note` and declared fields"), "{}", o.out);
    // Opposed: the same record with only its note validates.
    let input = write_json(&s, "note.json", &[rec(json!({"kind": "in_region", "to": "R-1", "note": "the published schema"}))]);
    let o = import(&s, "kb", &input, &["--remint", "--dry-run"]);
    assert_eq!(o.rc, 0, "{}", o.out);
}

/// Issue #21: `nodes` printed `state: null` for regions, which carry no
/// state at all. Now a region or viewer is listed without the key, in
/// `nodes`, `search` and a region's members alike.
#[test]
fn i21_a_region_or_viewer_is_listed_without_state() {
    let s = scratch("i21", &["kb"]);
    assert_eq!(import(&s, "kb", &write_json(&s, "example.json", &example()), &[]).rc, 0);
    // His reproduction.
    let d = read(&s, "nodes", &["--type", "region"]);
    let regions = d["nodes"].as_array().unwrap();
    assert_eq!(regions.len(), 2, "{d}");
    assert!(regions.iter().all(|n| n.get("state").is_none()), "{d}");
    // Every stateless type, in every list read; every other node keeps it.
    let all = read(&s, "nodes", &[]);
    for n in all["nodes"].as_array().unwrap() {
        let stateless = n["type"] == "region" || n["type"] == "viewer";
        assert_eq!(n.get("state").is_none(), stateless, "{n}");
        if !stateless {
            assert!(n["state"].is_string(), "{n}");
        }
    }
    let hits = read(&s, "search", &["schema"]);
    let r1 = hits["results"].as_array().unwrap().iter().find(|n| n["id"] == "R-1").expect("R-1 matches `schema`");
    assert!(r1.get("state").is_none(), "{hits}");
    assert!(hits["results"].as_array().unwrap().iter().any(|n| n["state"].is_string()), "{hits}");
    let members = read(&s, "region", &["R-1"])["members"].clone();
    assert!(members.as_array().unwrap().iter().all(|n| n["state"].is_string()), "{members}");
}

/// An idea in R-1 with `meta`.
fn cited(label: &str, meta: Value) -> Value {
    json!({"id": label, "type": "idea", "state": "idea", "handle": format!("A case labelled {label}"), "meta": meta,
           "edges": [{"kind": "in_region", "to": "R-1", "note": "the published schema"}]})
}

/// Issue #15: `meta.cites` was never checked, so a misspelled kind or a
/// missing key landed silently. Now the import refuses such a record as
/// `bad-cite`, naming it, in a dry run and a real import; every citation the
/// README's table describes (with extra keys) still lands.
#[test]
fn i15_a_bad_citation_refuses_its_record() {
    let s = scratch("i15", &["kb"]);
    let soup = s.soup("kb");
    // The README's own example carries a `file` citation and lands.
    assert_eq!(import(&s, "kb", &write_json(&s, "example.json", &example()), &[]).rc, 0);
    // His reproduction: a misspelled kind, and a commit without its sha.
    let bad = vec![
        cited("c1", json!({"kind": "case", "cites": [{"kind": "fiel", "path": "notes/run.log"}]})),
        cited("c2", json!({"kind": "case", "cites": [{"kind": "commit", "repo": "cauldron"}]})),
    ];
    let input = write_json(&s, "bad.json", &bad);
    let before = db_bytes(&soup);
    for extra in [&["--remint", "--dry-run"][..], &["--remint"][..]] {
        let o = import(&s, "kb", &input, extra);
        assert_eq!(o.rc, 1, "{extra:?}: {}", o.out);
        let d = o.json();
        assert_eq!(d["refusal"], "records-refused", "{d}");
        for (i, (label, says)) in [("c1", "`fiel` is not a citation kind"), ("c2", "(commit) lacks `sha`")].iter().enumerate() {
            let r = &d["records"][i];
            assert_eq!(r["id"], *label, "{d}");
            assert_eq!(r["reasons"][0]["code"], "bad-cite", "{d}");
            let why = r["reasons"][0]["detail"].as_str().unwrap();
            assert!(why.starts_with(&format!("`{label}`: meta.cites[0]")) && why.contains(says), "{why}");
        }
        assert_eq!(db_bytes(&soup), before, "{extra:?} wrote");
    }
    // Opposed: every kind of the table, with its keys and extra ones, lands.
    let good = vec![
        cited("g1", json!({"cites": [{"kind": "session", "id": "s-1", "at": "turn 4"}]})),
        cited("g2", json!({"cites": [{"kind": "file", "path": "src/lib.rs", "line": 40, "repo": "cauldron", "commit": "main"}]})),
        cited("g3", json!({"cites": [{"kind": "commit", "repo": "cauldron", "sha": "main"}]})),
        cited("g4", json!({"cites": [{"kind": "url", "url": "https://example.com/spec"}]})),
        cited("g5", json!({"cites": [{"kind": "thread", "thread": "t-1", "msg": 12}]})),
        cited("g6", json!({"cites": [{"kind": "dump", "id": "DM-3"}]})),
        cited("g7", json!({"kind": "case"})),
    ];
    let o = import(&s, "kb", &write_json(&s, "good.json", &good), &["--remint"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert_eq!(o.json()["accepted"], 7);
}

/// Issue #16: after DEC-3 superseded DEC-2, every read showed DEC-2 as a
/// plain `decided` node. Now `get` answers it with `superseded_by`, and
/// `nodes`, `search` and `region` leave it out, saying how many they left
/// out, unless `--include-superseded`.
#[test]
fn i16_reads_mark_and_hide_a_superseded_node() {
    let s = scratch("i16", &["kb"]);
    assert_eq!(import(&s, "kb", &write_json(&s, "example.json", &example()), &[]).rc, 0);
    // His reproduction: a decision, then a second one superseding it.
    let first = json!({"id": "DEC-2", "type": "decision", "state": "decided", "handle": "Supersede test: the first version of the rule",
                       "edges": [{"kind": "in_region", "to": "R-1", "note": "the published schema"}]});
    let second = json!({"id": "DEC-3", "type": "decision", "state": "decided", "handle": "Supersede test: the second version of the rule",
                        "edges": [{"kind": "in_region", "to": "R-1", "note": "the published schema"},
                                  {"kind": "supersedes", "to": "DEC-2", "note": "replaces the first version"}]});
    // Before the supersede, nothing is hidden and nothing is marked.
    assert_eq!(import(&s, "kb", &write_json(&s, "first.json", &[first.clone()]), &[]).rc, 0);
    let d = read(&s, "nodes", &["--type", "decision"]);
    assert_eq!((d["total"].as_u64(), d["superseded_hidden"].as_u64()), (Some(2), Some(0)), "{d}");
    assert!(read(&s, "get", &["DEC-2"])["records"][0].get("superseded_by").is_none());
    assert_eq!(import(&s, "kb", &write_json(&s, "second.json", &[second]), &[]).rc, 0);

    // get: always the node, marked.
    let d = read(&s, "get", &["DEC-2", "DEC-3"]);
    assert_eq!(d["records"][0]["id"], "DEC-2", "{d}");
    assert_eq!(d["records"][0]["state"], "decided", "{d}");
    assert_eq!(d["records"][0]["superseded_by"], json!(["DEC-3"]), "{d}");
    assert!(d["records"][1].get("superseded_by").is_none(), "{d}");
    // nodes, search, region: hidden and counted.
    let ids_of = |list: &Value| -> Vec<String> { list.as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap().to_string()).collect() };
    let d = read(&s, "nodes", &["--type", "decision"]);
    assert_eq!(ids_of(&d["nodes"]), ["DEC-3", "DEC-1"], "{d}");
    assert_eq!((d["total"].as_u64(), d["superseded_hidden"].as_u64()), (Some(2), Some(1)), "{d}");
    let d = read(&s, "search", &["supersede", "test"]);
    assert!(!ids_of(&d["results"]).contains(&"DEC-2".to_string()), "{d}");
    assert!(ids_of(&d["results"]).contains(&"DEC-3".to_string()), "{d}");
    assert_eq!(d["superseded_hidden"], 1, "{d}");
    let d = read(&s, "region", &["R-1"]);
    assert!(!ids_of(&d["members"]).contains(&"DEC-2".to_string()), "{d}");
    assert_eq!((d["members_total"].as_u64(), d["superseded_hidden"].as_u64()), (Some(5), Some(1)), "{d}");
    // A filter that leaves the superseded node out hides nothing.
    assert_eq!(read(&s, "nodes", &["--type", "issue"])["superseded_hidden"], 0);
    // --include-superseded lists it, marked, and hides nothing.
    for (verb, extra, list) in [
        ("nodes", &["--type", "decision"][..], "nodes"),
        ("search", &["supersede", "test"][..], "results"),
        ("region", &["R-1"][..], "members"),
    ] {
        let mut args = extra.to_vec();
        args.push("--include-superseded");
        let d = read(&s, verb, &args);
        assert_eq!(d["superseded_hidden"], 0, "{verb}: {d}");
        let dec2 = d[list].as_array().unwrap().iter().find(|n| n["id"] == "DEC-2").unwrap_or_else(|| panic!("{verb}: {d}"));
        assert_eq!(dec2["superseded_by"], json!(["DEC-3"]), "{verb}: {d}");
        assert!(d[list].as_array().unwrap().iter().filter(|n| n["id"] != "DEC-2").all(|n| n.get("superseded_by").is_none()), "{d}");
    }
    // --limit and --max-chars bound what is listed, as before.
    let d = read(&s, "nodes", &["--type", "decision", "--limit", "1", "--max-chars", "10"]);
    assert_eq!((d["total"].as_u64(), d["more"].as_u64(), d["superseded_hidden"].as_u64()), (Some(2), Some(1), Some(1)), "{d}");
    assert_eq!(d["nodes"][0]["handle"].as_str().unwrap().chars().count(), 10, "{d}");
    // The flag is refused where it means nothing.
    let o = run(&s, &["soup", "get", "DEC-2", "--include-superseded", "--env", "dev", "--cauldron", "kb"]);
    assert_ne!(o.rc, 0, "{}", o.out);
}

// ---------------------------------------------------------------- golden

/// **The output of every soup verb, byte for byte, against a recorded
/// copy** (`golden/soup.json`): stdout, stderr and the exit code of a fixed
/// session that imports, refuses, reads, bounds and resolves through a
/// project marker. Only what differs from run to run is replaced before the
/// comparison: the scratch directory, transaction hashes and timestamps.
/// `SOUP_GOLDEN_WRITE=1` rewrites the recorded copy; the copy was first
/// recorded from the binary in which the soup logic still lived in the CLI.
#[test]
fn s18_every_verb_prints_exactly_the_recorded_output() {
    let s = scratch("golden", &["kb", "other"]);
    let example = write_json(&s, "example.json", &example());
    let later = write_json(
        &s,
        "later.json",
        &[
            json!({"id": "r", "type": "region", "handle": "Schema drift checks",
                   "boundary": "Inside: tests that hold a copy of the schema to its source. Outside: the schema itself (R-1).",
                   "edges": [{"kind": "narrows", "to": "R-1"}]}),
            json!({"id": "a", "type": "issue", "state": "open", "handle": "The plugin's schema copy drifted from the published schema",
                   "body": "A drift test compares the copy with its source.",
                   "edges": [{"kind": "in_region", "to": "r"}]}),
        ],
    );
    let bad = write_json(&s, "bad.json", &[json!({"id": "IS-90", "type": "nonsense", "handle": "x"})]);
    let proj = s.0.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let (ex, la, ba) = (example.to_str().unwrap(), later.to_str().unwrap(), bad.to_str().unwrap());
    let k: &[&str] = &["--env", "dev", "--cauldron", "kb"];
    let with = |v: &[&str]| -> Vec<String> { v.iter().chain(k.iter()).map(|x| x.to_string()).collect() };
    let plain = |v: &[&str]| -> Vec<String> { v.iter().map(|x| x.to_string()).collect() };
    let session: Vec<(Option<&Path>, Vec<String>)> = vec![
        (None, with(&["soup", "import", ex, "--dry-run"])),
        (None, with(&["soup", "import", ex])),
        (None, with(&["soup", "import", ex])),
        (None, with(&["soup", "import", ex, "--dry-run", "--remint"])),
        (None, with(&["soup", "import", la, "--remint", "--agent", "golden"])),
        (None, with(&["soup", "import", ba])),
        (None, with(&["soup", "import", ba, "--dry-run"])),
        (None, with(&["soup", "regions"])),
        (None, with(&["soup", "regions", "--max-chars", "12"])),
        (None, with(&["soup", "region", "R-1"])),
        (None, with(&["soup", "region", "R-1", "--depth", "2", "--limit", "1", "--max-chars", "8"])),
        (None, with(&["soup", "region", "IS-1"])),
        (None, with(&["soup", "region", "R-99"])),
        (None, with(&["soup", "nodes"])),
        (None, with(&["soup", "nodes", "--type", "issue", "--state", "open"])),
        (None, with(&["soup", "nodes", "--region", "R-2", "--limit", "2", "--max-chars", "10"])),
        (None, with(&["soup", "nodes", "--type", "nope"])),
        (None, with(&["soup", "search", "schema", "drift"])),
        (None, with(&["soup", "search", "schema", "--limit", "2", "--max-chars", "20"])),
        (None, with(&["soup", "search", "--type", "issue", "copy"])),
        (None, with(&["soup", "search", "!!"])),
        (None, with(&["soup", "get", "IS-1", "R-1", "R-3", "NOPE-1"])),
        (None, with(&["soup", "get", "IS-1", "--max-chars", "10"])),
        (None, with(&["soup", "get", "IS-1", "--pool", "../x"])),
        (None, with(&["soup", "get", "IS-1", "--pool", "spine"])),
        (None, with(&["soup", "recover"])),
        (None, with(&["soup", "recover", "extra"])),
        (None, plain(&["soup", "nodes", "--env", "dev", "--cauldron", "nope"])),
        (None, plain(&["soup", "nodes", "--env", "nowhere", "--cauldron", "kb"])),
        (Some(&s.0), plain(&["soup", "nodes", "--cauldron", "kb"])),
        (None, with(&["soup", "nodes", "--bogus"])),
        (None, with(&["soup", "nodes", "--limit", "many"])),
        (None, with(&["soup", "frobnicate"])),
        (None, plain(&["soup"])),
        (None, plain(&["soup", "help"])),
        (None, plain(&["link", "kb", "--env", "dev", "--dir", proj.to_str().unwrap()])),
        (Some(&proj), plain(&["soup", "nodes", "--limit", "1"])),
        (Some(&proj), plain(&["soup", "nodes", "--limit", "1", "--cauldron", "other"])),
        (Some(&proj), plain(&["soup", "import", ex, "--dry-run"])),
        (Some(&proj), plain(&["soup", "recover"])),
        (Some(&proj), plain(&["soup", "nodes", "--env", "other-env"])),
        (Some(&proj), plain(&["link", "--show"])),
    ];
    let root = s.0.display().to_string();
    let mut volatile: Vec<String> = Vec::new();
    let mut doc = Vec::new();
    for (cwd, args) in &session {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let o = match cwd {
            Some(d) => run_in(&s, d, &[], &argv),
            None => run(&s, &argv),
        };
        // What legitimately differs per run: hashes and timestamps.
        if let Ok(v) = serde_json::from_str::<Value>(&o.out) {
            if let Some(tx) = v["landed"]["tx"].as_str() {
                volatile.push(tx.to_string());
            }
            for l in v["landed_before"].as_array().into_iter().flatten() {
                if let Some(ts) = l["landed_at"].as_str() {
                    volatile.push(ts.to_string());
                }
            }
        }
        doc.push(json!({
            "args": argv.iter().map(|a| a.replace(&root, "<S>")).collect::<Vec<_>>(),
            "cwd": cwd.map(|d| d.display().to_string().replace(&root, "<S>")),
            "rc": o.rc, "stdout": o.out, "stderr": o.err,
        }));
    }
    let mut text = serde_json::to_string_pretty(&doc).unwrap().replace(&root, "<S>");
    volatile.sort_by_key(|v| std::cmp::Reverse(v.len()));
    for (i, v) in volatile.iter().enumerate() {
        text = text.replace(v.as_str(), &format!("<V{i}>"));
    }
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/it/golden/soup.json");
    if std::env::var_os("SOUP_GOLDEN_WRITE").is_some() {
        std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
        std::fs::write(&golden, &text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&golden).expect("golden/soup.json (record it with SOUP_GOLDEN_WRITE=1)");
    if want != text {
        let (w, t): (Vec<Value>, Vec<Value>) = (serde_json::from_str(&want).unwrap(), serde_json::from_str(&text).unwrap());
        for (a, b) in w.iter().zip(t.iter()) {
            assert_eq!(a, b, "a soup verb's output changed");
        }
        assert_eq!(w.len(), t.len(), "the session changed length");
        panic!("golden text differs only in formatting");
    }
}

// ---------------------------------------------------------------- dumps

/// User-role lines Claude Code records that the person did not type: a
/// `!` command and its output, a slash command's wrapper, a local command's
/// output, system reminders, an attachment, a compaction summary, a tool
/// result. Each holds `PLANTED` and none is a text turn.
fn planted_lines(i: usize) -> Vec<Value> {
    let user = |content: Value| json!({"type": "user", "message": {"role": "user", "content": content}});
    vec![
        user(json!(format!("<bash-input>cat .env PLANTED_SHELL_IN_{i}</bash-input>"))),
        user(json!(format!("<bash-stdout>TOKEN=PLANTED_SHELL_OUT_{i}</bash-stdout><bash-stderr>PLANTED_SHELL_ERR_{i}</bash-stderr>"))),
        user(json!(format!("<command-name>/deploy</command-name>\n<command-args>PLANTED_ARGS_{i}</command-args>"))),
        user(json!(format!("<local-command-stdout>PLANTED_LOCAL_{i}</local-command-stdout>"))),
        user(json!([{"type": "text", "text": format!("<system-reminder>PLANTED_REMINDER_{i}</system-reminder>")}])),
        json!({"type": "attachment", "attachment": {"type": "file", "content": format!("PLANTED_ATTACHMENT_{i}")}}),
        json!({"type": "user", "isCompactSummary": true, "message": {"role": "user", "content": format!("PLANTED_SUMMARY_{i}")}}),
        json!({"type": "user", "toolUseResult": {"stdout": format!("PLANTED_TOOL_{i}")},
               "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "x", "content": format!("PLANTED_TOOL_{i}")}]}}),
    ]
}

/// A session dump made in `s`'s cauldron `c` as the service makes one,
/// from a fixture transcript with tool calls between its text turns.
fn session_dump(s: &Scratch, c: &str) -> (env::Cauldron, Vec<u8>) {
    use std::io::Write;
    let e = env::Environment {
        root: s.root(),
        root_source: env::paths::Source::CauldronHome,
        name: "dev".into(),
        name_source: env::paths::Source::CauldronEnv,
        data_root: s.data(),
    };
    let cauldron = e.find_cauldron(c).unwrap().expect("the cauldron");
    let mut t = Vec::new();
    for i in 0..30 {
        for v in [
            json!({"type": "user", "message": {"role": "user", "content": format!("question {i}")}}),
            json!({"type": "assistant", "message": {"role": "assistant", "content": [
                {"type": "text", "text": format!("answer {i}")},
                {"type": "tool_use", "id": format!("call-{i}"), "name": "Read", "input": {}}]}}),
            json!({"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": format!("call-{i}"), "content": "TOOL OUTPUT"}]}}),
        ] {
            t.extend_from_slice(v.to_string().as_bytes());
            t.push(b'\n');
        }
        // Every tenth exchange, what Claude Code records as the user's that
        // the person did not type, each with a planted secret.
        if i % 10 == 3 {
            for v in planted_lines(i) {
                t.extend_from_slice(v.to_string().as_bytes());
                t.push(b'\n');
            }
        }
    }
    let store = soup_door::dumps::Store::open(&cauldron, "test 1").unwrap();
    let mut w = store.blob(soup_door::dumps::Kind::Session).unwrap();
    w.write_all(&t).unwrap();
    let by = soup_door::dumps::By { thread: "test".into(), dispatch: "test".into(), instrument: "test".into(), version: "0".into() };
    let make = |blob: &soup_door::dumps::Blob| {
        Ok(soup_door::dumps::NewDump {
            env: "dev".into(),
            content: soup_door::dumps::Content::Session { session_id: "sess-one".into(), cwd: None, lines: 90 },
            blob: blob.clone(),
            title: Some("A session".into()),
            cite: None,
            refs: vec![],
            client: "test".into(),
            session: None,
        })
    };
    let (_, landed) = store.commit(w, || Ok(()), make, &by, |e| e).unwrap();
    assert_eq!(landed.id, "DM-1");
    (cauldron, t)
}

/// `cauldron soup dump-get --view text` is the text view the MCP door
/// answers (the same library call), page for page; `--turn-offset` and
/// `--raw` refuse where they do not apply.
#[test]
fn d4_dump_get_view_text_pages_the_turns_as_the_door_does() {
    let s = scratch("textview", &["kb"]);
    let (c, transcript) = session_dump(&s, "kb");
    let k = ["--env", "dev", "--cauldron", "kb"];
    let (mut off, mut turns, mut tool_calls) = (0u64, 0usize, 0u64);
    let mut left_out = std::collections::BTreeMap::new();
    loop {
        let o = off.to_string();
        let mut args = vec!["soup", "dump-get", "DM-1", "--view", "text", "--max-chars", "1000", "--offset", &o];
        args.extend_from_slice(&k);
        let out = run(&s, &args);
        assert_eq!(out.rc, 0, "{}", out.err);
        let page = out.json();
        assert_eq!(page, soup_door::dumps::get_text(&c, "DM-1", off, 0, 1000).unwrap(), "the CLI's page is the library's");
        assert!(page.to_string().chars().count() <= 1000);
        assert!(!out.out.contains("TOOL OUTPUT"));
        assert!(!out.out.contains("PLANTED"), "a planted secret is shown: {}", out.out);
        turns += page["turns"].as_array().unwrap().len();
        tool_calls += page["elided"]["tool_calls"].as_u64().unwrap();
        for f in ["shell", "commands", "local_command", "system_reminders", "attachments", "generated"] {
            *left_out.entry(f).or_insert(0) += page["elided"][f].as_u64().unwrap();
        }
        assert!(page.get("next_turn_offset").is_none(), "no turn here is longer than a page");
        match page["next_offset"].as_u64() {
            Some(n) => off = n,
            None => break,
        }
    }
    assert_eq!((turns, tool_calls), (60, 30));
    // Three planted groups, each counted by kind.
    let want: std::collections::BTreeMap<&str, u64> =
        [("shell", 6), ("commands", 3), ("local_command", 3), ("system_reminders", 3), ("attachments", 3), ("generated", 3)].into();
    assert_eq!(left_out, want);
    assert!(transcript.windows(7).filter(|w| w == b"PLANTED").count() >= 27, "the stored transcript holds them all");
    for (bad, why) in [
        (vec!["soup", "dump-get", "DM-1", "--view", "text", "--raw"], "--raw with the text view"),
        (vec!["soup", "dump-get", "DM-1", "--turn-offset", "2"], "--turn-offset without the text view"),
        (vec!["soup", "dump-get", "DM-1", "--view", "words"], "an unknown view"),
        (vec!["soup", "dump-get", "DM-1", "--view", "text", "--offset", "3"], "an offset inside a line"),
        (vec!["soup", "dump-get", "DM-1", "--view", "text", "--max-chars", "999"], "a text page under the floor"),
        (vec!["soup", "dump-get", "DM-1", "--max-chars", "256"], "a raw page under the floor"),
    ] {
        let mut args = bad.clone();
        args.extend_from_slice(&k);
        let o = run(&s, &args);
        assert_ne!(o.rc, 0, "{why}: {}", o.out);
        assert!(o.out.is_empty(), "{why}: a refusal prints no JSON");
    }
    // The raw view is unchanged.
    let mut args = vec!["soup", "dump-get", "DM-1", "--max-chars", "1000"];
    args.extend_from_slice(&k);
    let raw = run(&s, &args).json();
    assert!(raw.to_string().chars().count() <= 1000);
    assert!(transcript.starts_with(raw["text"].as_str().unwrap().as_bytes()) && raw["next_offset"].is_u64());
}

// ---------------------------------------------------------------- modes

/// Every directory and file under `root`, with its mode (links skipped).
fn modes_under(root: &Path) -> Vec<(String, bool, u32)> {
    use std::os::unix::fs::PermissionsExt;
    let mut out = vec![];
    let mut todo = vec![root.to_path_buf()];
    while let Some(d) = todo.pop() {
        let md = std::fs::symlink_metadata(&d).unwrap();
        out.push((d.display().to_string(), true, md.permissions().mode() & 0o777));
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let md = std::fs::symlink_metadata(e.path()).unwrap();
            if md.is_dir() {
                todo.push(e.path());
            } else if md.is_file() {
                out.push((e.path().display().to_string(), false, md.permissions().mode() & 0o777));
            }
        }
    }
    out.sort();
    out
}

fn all_private(root: &Path) -> bool {
    modes_under(root).iter().all(|(_, dir, m)| *m == if *dir { 0o700 } else { 0o600 })
}

fn loosen(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for (p, dir, _) in modes_under(root) {
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(if dir { 0o755 } else { 0o644 })).unwrap();
    }
}

/// **A cauldron's data is its owner's only.** `cauldron create`, even under
/// a umask that takes nothing away, makes every directory of the cauldron
/// 0700 and every file 0600; `cauldron list` and the reads work as before.
/// A store made before this (0755/0644) is left alone by reads and dry
/// runs, which write nothing, and tightened by the next real import.
#[test]
fn p1_a_cauldron_s_data_is_private_whatever_the_umask() {
    let s = scratch("modes", &[]);
    let mut c = Command::new("/bin/sh");
    c.arg("-c")
        .arg("umask 000; exec \"$0\" \"$@\"")
        .arg(CAULDRON)
        .args(["create", "kb", "--env", "dev"])
        .env("CAULDRON_HOME", s.root())
        .env("HOME", s.0.join("home"))
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .env_remove("CAULDRON_CAULDRON")
        .stdin(Stdio::null());
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let dir = s.data().join("cauldrons").join("kb");
    let made = modes_under(&dir);
    assert!(made.iter().filter(|(_, d, _)| !d).count() >= 3, "cauldron.toml, spine.db, ws.db: {made:?}");
    assert!(all_private(&dir), "a new cauldron is not private: {made:?}");
    // Its readers, as before.
    let list = run(&s, &["list", "--env", "dev"]);
    assert!(list.rc == 0 && list.out.contains("kb"), "{}", list.err);
    assert_eq!(read(&s, "nodes", &[])["total"], 0);

    // A store made before this release.
    loosen(&dir);
    assert!(!all_private(&dir));
    let before = modes_under(&dir);
    let input = write_json(&s, "example.json", &example());
    assert_eq!(read(&s, "regions", &[])["regions"], json!([]));
    assert_eq!(import(&s, "kb", &input, &["--dry-run"]).rc, 0);
    assert_eq!(modes_under(&dir), before, "a read or a dry run changed a mode");
    let o = import(&s, "kb", &input, &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert!(all_private(&dir), "the import did not tighten the store: {:?}", modes_under(&dir));
    assert!(read(&s, "nodes", &[])["total"].as_u64().unwrap() > 0);
}

/// **`cauldron rename` keeps a cauldron its owner's only.** Under a umask
/// that takes nothing away, the rewritten `cauldron.toml` and the lock files
/// the rename opens are 0600 and every directory 0700; a store an older
/// release left loose (0755/0644) is private once it is renamed.
#[test]
fn p2_a_renamed_cauldron_is_private_whatever_the_umask() {
    let s = scratch("modes-rename", &[]);
    let loose = |args: &[&str]| {
        let mut c = Command::new("/bin/sh");
        c.arg("-c")
            .arg("umask 000; exec \"$0\" \"$@\"")
            .arg(CAULDRON)
            .args(args)
            .env("CAULDRON_HOME", s.root())
            .env("HOME", s.0.join("home"))
            .env_remove("CAULDRON_ENV")
            .env_remove("ENV")
            .env_remove("CAULDRON_CAULDRON")
            .stdin(Stdio::null());
        let o = c.output().unwrap();
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    };
    loose(&["create", "um0", "--env", "dev"]);
    loose(&["rename", "um0", "um1", "--env", "dev"]);
    let dir = s.data().join("cauldrons").join("um1");
    let made = modes_under(&dir);
    for f in ["cauldron.toml", "tui/open.lock", "soup/writer.lock"] {
        assert!(made.iter().any(|(p, d, _)| !d && p.ends_with(f)), "{f} is not there: {made:?}");
    }
    assert!(all_private(&dir), "a renamed cauldron is not private: {made:?}");

    // A store an older release left loose is private once renamed.
    loosen(&dir);
    assert!(!all_private(&dir));
    loose(&["rename", "um1", "um2", "--env", "dev"]);
    let dir = s.data().join("cauldrons").join("um2");
    assert!(all_private(&dir), "a loose store renamed is not private: {:?}", modes_under(&dir));
    let o = run(&s, &["soup", "nodes", "--env", "dev", "--cauldron", "um2"]);
    assert_eq!((o.rc, o.json()["total"].clone()), (0, json!(0)), "{}", o.err);
}

// ---------------------------------------------------------------- the plugin's example regions

/// The plugin ships `regions.example.json` for a new soup, which starts with
/// no regions: against a fresh cauldron it dry-runs `validated` under
/// `--remint` (its ids are labels), and lands as four regions.
#[test]
fn e1_the_plugin_s_example_regions_validate_against_a_fresh_soup() {
    let s = scratch("regions-example", &["kb"]);
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../claude-code/cauldron/regions.example.json");
    let dry = import(&s, "kb", &file, &["--dry-run", "--remint"]);
    assert_eq!(dry.rc, 0, "{}\n{}", dry.out, dry.err);
    let d = dry.json();
    assert_eq!((d["outcome"].clone(), d["accepted"].clone(), d["refused"].clone()), (json!("validated"), json!(4), json!(0)), "{d}");
    // Without --remint its labels are not ids, and it is refused.
    assert_ne!(import(&s, "kb", &file, &["--dry-run"]).rc, 0);
    let real = import(&s, "kb", &file, &["--remint"]);
    assert_eq!(real.rc, 0, "{}", real.err);
    assert_eq!(real.json()["id_map"], json!({"r-code": "R-1", "r-tests": "R-2", "r-product": "R-3", "r-process": "R-4"}));
    assert_eq!(read(&s, "regions", &[])["regions"].as_array().unwrap().len(), 4);
}

/// A record a ripple wrote reads as `unreviewed: true` in `nodes`, `search`
/// and `get`, and `--review pending|reviewed` filters on it; the CLI's items
/// equal the library's (the door's). A bad value is refused.
#[test]
fn rv1_unreviewed_records_are_marked_and_filtered_by_the_cli() {
    use spine::envelope::{Envelope, Initiator, Instrument, Mode, Provenance, RippleKind, ENVELOPE_FORMAT_VERSION};
    let s = scratch("review", &["kb"]);
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../claude-code/cauldron/regions.example.json");
    assert_eq!(import(&s, "kb", &file, &["--remint"]).rc, 0);
    assert_eq!(read(&s, "nodes", &["--review", "pending"])["total"], 0);
    let c = spine::Cauldron::open(&s.soup("kb"), spine::config::SpineConfig::default()).unwrap();
    let env = Envelope {
        statement: "MATCH (r: ws/region { id: $r }) CREATE (i: ws/issue { state: \"open\", handle: $h }) CREATE (i) -[in_region]-> (r) RETURN i.id".into(),
        bindings: [("r".to_string(), json!("R-1")), ("h".to_string(), json!("flaky build cache"))].into_iter().collect(),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Ripple { kind: RippleKind::TypeTwo, run_id: "run-cli".into(), step_id: "put".into(), scope: vec![] },
            instrument: Instrument { name: "test".into(), version: "0".into() },
        },
        mode: Mode::Commit,
        idempotency_nonce: Some("cli-review".into()),
    };
    assert!(matches!(c.submit(env).unwrap(), spine::Outcome::Committed { .. }));
    drop(c);
    let p = read(&s, "nodes", &["--review", "pending"]);
    assert_eq!(p["total"], 1, "{p}");
    assert_eq!((p["nodes"][0]["id"].clone(), p["nodes"][0]["unreviewed"].clone()), (json!("IS-1"), json!(true)));
    let r = read(&s, "nodes", &["--review", "reviewed"]);
    assert_eq!(r["total"], 4);
    assert!(!r.to_string().contains("unreviewed"));
    let all = read(&s, "nodes", &[]);
    assert_eq!(all["total"], 5);
    let found = read(&s, "search", &["cache", "--review", "pending"]);
    assert_eq!(found["results"][0]["unreviewed"], json!(true));
    let got = get(&s, "kb", &["IS-1"]).json();
    assert_eq!(got["records"][0]["unreviewed"], json!(true));
    let bad = run(&s, &["soup", "nodes", "--review", "maybe", "--env", "dev", "--cauldron", "kb"]);
    assert_ne!(bad.rc, 0);
    assert!(bad.err.contains("pending"), "{}", bad.err);
}

// ------------------------------------------------------- link, unlink, cite

fn edges_in(s: &Scratch, id: &str) -> Vec<(String, String)> {
    let g = get(s, "kb", &[id]).json();
    g["records"][0]["edges"].as_array().unwrap().iter().map(|e| (e["kind"].as_str().unwrap().to_string(), e["to"].as_str().unwrap().to_string())).collect()
}

/// `cauldron soup link | unlink | cite`, written directly (issue #24, and
/// #23's missing CLI twin of `soup_link`): an edge added and removed (`get`
/// stops showing it, `changes` lists an `unlink`), a repeat `not-linked`
/// with nothing written, a record's last `in_region` and a `supersedes`
/// refused with their reasons on stdout and exit 1, a citation added and
/// removed by index and by match with every other meta key kept, a bad one
/// refused; and `cauldron link help` points at `soup link`.
#[test]
fn s19_link_unlink_and_cite_from_a_shell() {
    let s = scratch("s19", &["kb"]);
    assert_eq!(import(&s, "kb", &write_json(&s, "ex.json", &example()), &[]).rc, 0);
    let kb = ["--env", "dev", "--cauldron", "kb"];
    let soup = |args: &[&str]| {
        let mut a = vec!["soup"];
        a.extend_from_slice(args);
        a.extend_from_slice(&kb);
        run(&s, &a)
    };
    let since = soup(&["changes"]).json()["cursor"].as_str().unwrap().to_string();
    let o = soup(&["link", "T-1", "ref", "M-1", "--note", "evidence"]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert_eq!(o.json()["outcome"], "linked", "{}", o.out);
    assert!(edges_in(&s, "T-1").contains(&("ref".into(), "M-1".into())));
    let o = soup(&["unlink", "T-1", "ref", "M-1"]);
    assert_eq!((o.rc, o.json()["outcome"].clone()), (0, json!("unlinked")), "{}\n{}", o.out, o.err);
    assert!(!edges_in(&s, "T-1").contains(&("ref".into(), "M-1".into())), "get no longer shows it");
    let ops: Vec<String> = soup(&["changes", "--since", &since]).json()["changes"].as_array().unwrap().iter().map(|c| c["op"].as_str().unwrap().to_string()).collect();
    assert_eq!(ops, ["link", "unlink"]);
    let o = soup(&["unlink", "T-1", "ref", "M-1"]);
    assert_eq!((o.rc, o.json()["outcome"].clone(), o.json()["written"].clone()), (0, json!("not-linked"), json!(false)), "{}", o.out);
    // Refused by name.
    let o = soup(&["unlink", "T-1", "in_region", "R-2"]);
    assert_eq!(o.rc, 1, "{}\n{}", o.out, o.err);
    assert_eq!(o.json()["links"][0]["refused"]["reason"], "required-edge", "{}", o.out);
    assert!(o.err.contains("links_refused"), "{}", o.err);
    assert!(edges_in(&s, "T-1").contains(&("in_region".into(), "R-2".into())));
    let o = soup(&["unlink", "CL-1", "claims", "R-1"]);
    assert_eq!(o.rc, 1);
    assert_eq!(o.json()["links"][0]["refused"]["reason"], "claims-is-soup-claim", "{}", o.out);
    let o = soup(&["link", "T-1", "ref"]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("takes a record, an edge kind and a record"), "{}", o.err);
    // Citations.
    let o = soup(&["cite", "DEC-1", "--add", r#"{"kind":"url","url":"https://example.com/d"}"#]);
    assert_eq!((o.rc, o.json()["outcome"].clone()), (0, json!("cited")), "{}\n{}", o.out, o.err);
    let o = soup(&["cite", "DEC-1", "--add", r#"{"kind":"file","path":"src/lib.rs","line":3}"#]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let meta = get(&s, "kb", &["DEC-1"]).json()["records"][0]["meta"].clone();
    assert_eq!(meta["cites"].as_array().unwrap().len(), 2, "{meta}");
    assert_eq!(meta["kind"], "pattern", "{meta}");
    let o = soup(&["cite", "DEC-1", "--add", r#"{"kind":"file"}"#]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("bad-cite"), "{}", o.err);
    let o = soup(&["cite", "DEC-1", "--remove", r#"{"kind":"file"}"#]);
    assert_eq!((o.rc, o.json()["outcome"].clone()), (0, json!("uncited")), "{}\n{}", o.out, o.err);
    let o = soup(&["cite", "DEC-1", "--remove", "0"]);
    assert_eq!((o.rc, o.json()["removed"]["url"].clone()), (0, json!("https://example.com/d")), "{}", o.out);
    let o = soup(&["cite", "DEC-1", "--remove", "0"]);
    assert_ne!(o.rc, 0, "past the end: {}", o.out);
    let meta = get(&s, "kb", &["DEC-1"]).json()["records"][0]["meta"].clone();
    assert_eq!(meta["cites"], json!([]), "{meta}");
    assert_eq!(meta["kind"], "pattern");
    // The two `link`s told apart.
    let o = run(&s, &["link", "help"]);
    assert!(o.out.contains("cauldron soup link") && o.out.contains("does not connect two records"), "{}", o.out);
}
