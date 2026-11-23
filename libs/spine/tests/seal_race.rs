//! The seal act's side paths, proven at N = 2.
//!
//! Two arbitrations the seal act relies on were argued but never raced:
//!
//! - G1 **two racing opens of a pool file from before the `sealed` column**
//!   both upgrade it, and
//!   the file ends with exactly ONE `sealed` column (`pool.rs`,
//!   `upgrade_sealed_column`'s recheck after a failed `ALTER`). The race is
//!   made to FIRE by the armed build's `fault::upgrade_window` hook: each
//!   opener is held between its column check and its `ALTER` until BOTH are
//!   there (a two-party rendezvous), so both decide to `ALTER` and exactly
//!   one can win. Armed build only — the hook does not exist without
//!   `fault-injection`. Its opposite (a pool that already has the column
//!   opens with NO `ALTER`) runs in both builds, by `PRAGMA schema_version`.
//! - G2b **a SEAL racing a body SET on one node** — interleaved BOTH ways —
//!   always ends in one of the two serial orders and never in a sealed node
//!   whose body changed after its seal. No seam: the interleaving is
//!   CONSTRUCTED with the lock table (the `lock_behaviour.rs` idiom): a
//!   foreign row parks a WRITE on the node's body, both statements are
//!   submitted from their own threads, and the test waits until BOTH are
//!   enrolled and queued behind the park — so both matched and compiled
//!   against the same unsealed record — before it releases the park. The
//!   lower sequence number is granted first; the other is re-judged under
//!   its grant (exec.rs phase 10). Runs in both builds. The racers open
//!   with `Cauldron::open_unrecovered`: a recovering `open` aborts the
//!   park's un-decided row and frees its lock, so nothing would ever queue
//!   behind it.
//!
//! Each race test prints its interleaving evidence (`G1 interleaving:` /
//! `G2b interleaving:`) and asserts it, so a run in which the race did not
//! happen FAILS rather than passing quietly.

#![allow(dead_code)]

use spine::config::SpineConfig;
use spine::delta::CompiledPayload;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::grammar_v6;
use spine::locks::{self, Claim, ClaimSet, LockMode, Resource};
use spine::pool::{NodeId, Pool};
use spine::schema_def::{grammar_fault_tag, FieldKind, PoolGrammar, PoolName};
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use spine::witness::Witness;
use spine::{Cauldron, Outcome, ReadRequest};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

test_scratch::install!();

const POOL: &str = "ws-v6";

fn grammar() -> PoolGrammar {
    grammar_v6::workspace_v6(POOL)
}

// ---------------------------------------------------------------------------
// G1 — two racing opens of an old pool file both upgrade it
// ---------------------------------------------------------------------------

/// Two threads, two connections, one column-less pool file. Each opener is
/// HELD in the window between "the column is absent" and its `ALTER` until
/// the other is there too, so both `ALTER`; one wins, the other's fails and
/// its recheck finds the winner's column. Both opens succeed, the file has
/// exactly one `sealed` column, and the hook counted two openers in the
/// window and one lost `ALTER` — the race happened. Then (opposed) two more
/// racing opens of the now-upgraded file enter the window ZERO times.
// RED (G1): the recheck in `upgrade_sealed_column` removed
// (the failed `ALTER` returned as the hard error directly) — "both racing
// opens must succeed": Err(Hard { "pool `ws-v6` predates the seal flag and
// the `nodes.sealed` column could not be added: duplicate column name:
// sealed" }). (Opposed) the column check's early return removed (every open
// ALTERs and leans on the recheck) — the first race still passes (that is
// what makes the recheck load-bearing), the second: "an upgraded pool's
// opens never enter the ALTER window": left 4 right 2.
#[cfg(feature = "fault-injection")]
#[test]
fn g1_two_racing_opens_of_an_old_pool_both_upgrade_it_once() {
    use spine::fault::upgrade_window::{self, Event};
    use std::sync::{Arc, Condvar, Mutex};

    let file = old_pool_fixture("g1-race");
    let conn = rusqlite::Connection::open(&file).expect("pool");
    assert_eq!(sealed_columns(&conn), 0, "the fixture is the old shape (sentinel)");
    drop(conn);

    // (in the window, lost ALTERs) for THIS file only, plus a rendezvous
    // that releases when two openers are in the window — bounded, so a run
    // in which only one ever arrives fails loudly instead of hanging.
    #[derive(Default)]
    struct Seen {
        in_window: u32,
        lost: u32,
    }
    let seen = Arc::new((Mutex::new(Seen::default()), Condvar::new()));
    let rendezvous = Arc::new(Mutex::new(true));
    {
        let (seen, rendezvous, target) = (seen.clone(), rendezvous.clone(), file.clone());
        upgrade_window::install(move |path: &Path, event: Event| {
            if path != target.as_path() {
                return;
            }
            let (lock, cv) = &*seen;
            let mut s = lock.lock().unwrap();
            match event {
                Event::BeforeAlter => {
                    s.in_window += 1;
                    cv.notify_all();
                    if *rendezvous.lock().unwrap() {
                        let (s2, timeout) = cv
                            .wait_timeout_while(s, Duration::from_secs(10), |s| s.in_window < 2)
                            .unwrap();
                        assert!(
                            !timeout.timed_out(),
                            "only {} opener(s) ever reached the ALTER window",
                            s2.in_window
                        );
                    }
                }
                Event::AlterLostToARacer => s.lost += 1,
            }
        });
    }
    let _clear = ClearHook;

    let race = |file: &PathBuf| -> Vec<spine::error::Result<Pool>> {
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let f = file.clone();
                std::thread::spawn(move || Pool::open(&f))
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("an opener panicked")).collect()
    };

    let results = race(&file);
    for (i, r) in results.iter().enumerate() {
        assert!(r.is_ok(), "both racing opens must succeed; opener {i}: {:?}", r.as_ref().err());
    }
    drop(results);
    let (in_window, lost) = {
        let s = seen.0.lock().unwrap();
        (s.in_window, s.lost)
    };
    eprintln!("G1 interleaving: openers in the ALTER window together = {in_window}, ALTERs lost to the racer = {lost}");
    assert_eq!(in_window, 2, "both openers found the column absent and reached the ALTER");
    assert_eq!(lost, 1, "exactly one ALTER lost, and the recheck absorbed it");
    let conn = rusqlite::Connection::open(&file).expect("pool");
    assert_eq!(sealed_columns(&conn), 1, "the file ends with exactly one `sealed` column");
    drop(conn);

    // OPPOSED: the upgraded file's racing opens never reach the window —
    // a pool that has the column is not ALTERed (no rendezvous: a broken
    // build that does ALTER must fail on the count, not hang).
    *rendezvous.lock().unwrap() = false;
    let results = race(&file);
    assert!(results.iter().all(|r| r.is_ok()), "re-opens of the upgraded file");
    drop(results);
    let s = seen.0.lock().unwrap();
    assert_eq!(s.in_window, 2, "an upgraded pool's opens never enter the ALTER window");
    assert_eq!(s.lost, 1);
}

#[cfg(feature = "fault-injection")]
struct ClearHook;
#[cfg(feature = "fault-injection")]
impl Drop for ClearHook {
    fn drop(&mut self) {
        spine::fault::upgrade_window::clear();
    }
}

/// G1's opposite in BOTH builds: opening a pool that already has the column
/// (a fresh pool, and an old one once upgraded) changes no schema — SQLite's
/// own `PRAGMA schema_version`, which every `ALTER` bumps, is unchanged.
/// Paired with a sentinel: the one open that DOES upgrade moves it.
// RED (G1 sentinel): the `upgrade_sealed_column` call in
// `Pool::open` removed — "sentinel: the upgrading open DID alter the
// schema": left 12 right 12. NOT caught here: an open that ALTERs every
// time and absorbs the duplicate-column failure leaves schema_version where
// it was (the ALTER fails); the armed test above catches that one by count.
#[test]
fn g1_opening_a_pool_that_has_the_column_alters_nothing() {
    let file = old_pool_fixture("g1-noalter");
    let v0 = schema_version(&file);
    drop(Pool::open(&file).expect("the upgrading open"));
    let v1 = schema_version(&file);
    assert_ne!(v0, v1, "sentinel: the upgrading open DID alter the schema");
    for i in 0..2 {
        drop(Pool::open(&file).expect("re-open"));
        assert_eq!(schema_version(&file), v1, "re-open {i} of an upgraded pool altered nothing");
    }
    let (c, _g) = fresh("g1-fresh");
    drop(c);
    let fresh_file = Pool::path_in(&scratch_path("g1-fresh"), &PoolName::new(POOL));
    let f0 = schema_version(&fresh_file);
    drop(Pool::open(&fresh_file).expect("open a fresh pool"));
    assert_eq!(schema_version(&fresh_file), f0, "a fresh pool's open altered nothing");
}

fn sealed_columns(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM pragma_table_info('nodes') WHERE name = 'sealed'", [], |r| r.get(0))
        .expect("table_info")
}

fn schema_version(file: &Path) -> i64 {
    rusqlite::Connection::open(file)
        .expect("pool")
        .query_row("PRAGMA schema_version", [], |r| r.get(0))
        .expect("schema_version")
}

/// A pool file as it was before the `sealed` column: rebuilt from the pool's OWN `nodes`
/// DDL with the `sealed` line cut (the `seal.rs` fixture), holding one of
/// every v6 type.
fn old_pool_fixture(tag: &str) -> PathBuf {
    let (c, g) = fresh(tag);
    seed_every_type(&c, &g);
    drop(c);
    let file = Pool::path_in(&scratch_path(tag), &PoolName::new(POOL));
    let conn = rusqlite::Connection::open(&file).expect("pool");
    let ddl: String = conn
        .query_row("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'nodes'", [], |r| r.get(0))
        .expect("the nodes DDL");
    let cut = ddl.find(",\n  sealed").unwrap_or_else(|| panic!("the DDL names `sealed`: {ddl}"));
    let old = format!("{}\n)", &ddl[..cut]).replacen("CREATE TABLE nodes", "CREATE TABLE nodes_v0", 1);
    conn.execute_batch(&format!(
        "PRAGMA foreign_keys = OFF; BEGIN; {old};
         INSERT INTO nodes_v0 SELECT id, node_type, state, rev, schema_version, created_tx, updated_tx, created, updated FROM nodes;
         DROP TABLE nodes; ALTER TABLE nodes_v0 RENAME TO nodes;
         CREATE INDEX idx_nodes_type_state ON nodes(node_type, state); COMMIT;"
    ))
    .expect("rewrite the pool as it was before the sealed column");
    assert_eq!(sealed_columns(&conn), 0, "the fixture is the old shape");
    file
}

// ---------------------------------------------------------------------------
// G2b — SEAL racing a body SET on one node, interleaved both ways
// ---------------------------------------------------------------------------

/// SEAL is granted first, the SET second. Both matched and compiled against
/// the same UNSEALED record (both enrolled behind the park before either
/// could land), so the SET passed the door's judgement once — and is
/// re-judged under its grant against the sealed record and refuses
/// `sealed_body`. The serial order "SEAL lands, then the SET refuses": the
/// node is sealed, its body is the pre-race body, its rev moved once.
// RED (G2b-a): exec.rs phase 10's re-compile handed the
// STALE match (`&matched` for `&re_matched`) — the SET was never re-judged,
// reached the landing after its decided flip, and the landing's net refused:
// "the SET loses to the SEAL: expected a structured refusal, got Hard {
// SPINE INTERNAL INVARIANT VIOLATED ... }". (G2b-b) the same plus the
// landing's sealed-body net removed in `apply_part` — the WRONG outcome
// itself: "the SET loses to the SEAL: expected a refusal, got Committed {
// .. seq: Seq(15) .. }". (Seal's WRITE claim removed alone stays GREEN: the
// SEAL then holds the match's READ, which the SET's WRITE still queues
// behind — G2a in `seal.rs` guards the claim itself.)
#[test]
fn g2b_seal_granted_first_then_the_racing_body_set_refuses_sealed_body() {
    let (c, g) = fresh("g2b-seal-first");
    let have = seed_every_type(&c, &g);
    let t = have["task"].clone();
    let before_body = body_of(&c, "task", &t);
    let (_, rev0) = sealed_rev(&c, "task", &t);
    let dir = scratch_path("g2b-seal-first");
    let edit = unique("racing edit");

    let (first, second) = race_on_body(
        &dir,
        &t,
        Racer::Seal,
        Racer::SetBody(edit.clone()),
    );
    first.unwrap_or_else(|e| panic!("the SEAL, granted first, lands: {e:?}"));
    let w = refused(second, "the SET loses to the SEAL");
    assert_sealed_body(&w, &t);

    assert_eq!(sealed_rev(&c, "task", &t), (true, rev0 + 1), "sealed, one bump (the seal's)");
    assert_eq!(body_of(&c, "task", &t), before_body, "the body is the pre-race body");
    assert_ne!(before_body, json(&edit), "sentinel: the racing edit would have changed the body");
}

/// The SET is granted first, the SEAL second: both land, in that order — the
/// draft's last edit, then its seal. The node is sealed, its body is the
/// racing edit, its rev moved twice; and a later edit refuses.
// RED (G2b opposed): the door refusal in `check_settable`
// made unconditional for `body` — the SET refused at the door and never
// queued: "no racer ever queued on Body { node: NodeId(\"T-1\") }". With the
// test above (whose G2b-b red is always-land) both constant answers for the
// racing SET fail.
#[test]
fn g2b_body_set_granted_first_then_the_racing_seal_lands_on_the_edited_body() {
    let (c, g) = fresh("g2b-set-first");
    let have = seed_every_type(&c, &g);
    let t = have["task"].clone();
    let (_, rev0) = sealed_rev(&c, "task", &t);
    let dir = scratch_path("g2b-set-first");
    let edit = unique("racing edit");

    let (first, second) = race_on_body(
        &dir,
        &t,
        Racer::SetBody(edit.clone()),
        Racer::Seal,
    );
    first.unwrap_or_else(|e| panic!("the SET, granted first, lands: {e:?}"));
    second.unwrap_or_else(|e| panic!("the SEAL, granted second, lands: {e:?}"));

    assert_eq!(sealed_rev(&c, "task", &t), (true, rev0 + 2), "one bump for the SET, one for the SEAL");
    assert_eq!(body_of(&c, "task", &t), json(&edit), "the seal froze the edited body");
    let w = refused(set_body(&c, "task", &t, &unique("after the seal")), "a later edit");
    assert_sealed_body(&w, &t);
    assert_eq!(body_of(&c, "task", &t), json(&edit), "and the body is still the edited one");
}

#[derive(Clone, Debug)]
enum Racer {
    Seal,
    SetBody(String),
}

impl Racer {
    fn submit(&self, c: &Cauldron, t: &NodeId) -> spine::error::Result<Outcome> {
        match self {
            Racer::Seal => seal(c, &[("task", t)]),
            Racer::SetBody(b) => set_body(c, "task", t, b),
        }
    }
}

/// THE CONSTRUCTED INTERLEAVING. Park a WRITE on `body:<t>` under a foreign
/// row; submit `first` from its own thread and wait until it is ENROLLED
/// and QUEUED behind the park; then `second`, likewise; assert both are
/// queued, ungranted, with first's number below second's — so neither has
/// landed and both compiled against the same record — print that queue;
/// then release the park. Returns (first's outcome, second's outcome).
fn race_on_body(
    dir: &Path,
    t: &NodeId,
    first: Racer,
    second: Racer,
) -> (spine::error::Result<Outcome>, spine::error::Result<Outcome>) {
    let body = Resource::Body { node: t.clone() };
    let pool = PoolName::new(POOL);
    let mut held = ClaimSet::new();
    held.insert(Claim { pool: pool.clone(), resource: body.clone(), mode: LockMode::Write });
    let park_seq = park(dir, &held);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let mut known = vec![park_seq];

    let spawn = |r: Racer| {
        let (d, t) = (dir.to_path_buf(), t.clone());
        std::thread::spawn(move || {
            // `open_unrecovered`, never `open`: `open`
            // runs recovery, whose un-decided arm ABORTS every un-decided
            // row it meets — the park included (and the first racer's
            // row, when the second opens) — so the park would be gone
            // before any racer queued behind it. `lock_behaviour.rs` opens
            // its second handle the same way, for the same reason.
            let c = Cauldron::open_unrecovered(&d, patient()).expect("open the cauldron in the racer");
            r.submit(&c, &t)
        })
    };
    let h1 = spawn(first);
    let s1 = await_new_claimant(&spine, &pool, &body, &mut known);
    let h2 = spawn(second);
    let s2 = await_new_claimant(&spine, &pool, &body, &mut known);

    let q = locks::queue(&spine, &pool, &body).expect("queue");
    let holder = q.iter().filter(|e| e.seq == park_seq && e.granted).count();
    let q1 = q.iter().filter(|e| e.seq == s1 && !e.granted).count();
    let q2 = q.iter().filter(|e| e.seq == s2 && !e.granted).count();
    eprintln!(
        "G2b interleaving: park seq {park_seq} granted; first seq {s1} queued x{q1}; second seq {s2} queued x{q2}; \
         queue {:?}",
        q.iter().map(|e| (e.seq.0, e.mode, e.granted)).collect::<Vec<_>>()
    );
    assert_eq!(holder, 1, "the park holds the body");
    assert!(q1 >= 1 && q2 >= 1, "both racers are enrolled and waiting: {q:?}");
    assert!(s1 < s2, "the first racer holds the lower number");
    assert!(!h1.is_finished() && !h2.is_finished(), "neither racer has returned while the park holds");

    unpark(dir, park_seq);
    (h1.join().expect("first racer"), h2.join().expect("second racer"))
}

fn patient() -> SpineConfig {
    SpineConfig {
        lock_wait_ceiling: Duration::from_secs(30),
        lock_poll_interval: Duration::from_millis(3),
        ..SpineConfig::default()
    }
}

/// A foreign holder: a real pending row at a real sequence number and a real
/// grant (`lock_behaviour.rs`'s `park`).
fn park(dir: &Path, held: &ClaimSet) -> Seq {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let env = commit("-- parking holder: seal race", &[]);
    let hash = env.hash_identity().expect("hash");
    let seq = spine
        .append_pending(&env, &hash, &CompiledPayload { parts: Vec::new(), minted: BTreeMap::new() })
        .expect("parking row");
    locks::acquire(&spine, seq, held, &patient()).expect("park");
    seq
}

fn unpark(dir: &Path, seq: Seq) {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    locks::release_at_clear(&spine, seq).expect("release");
    spine
        .mark_cleared(seq, &serde_json::json!({ "parking_holder": "released" }))
        .expect("close the parking row");
}

/// Wait (bounded) for a door call's ungranted claim on `resource` at a
/// sequence number not yet seen: the state that says "it asked and did not
/// get it". No sleep race.
fn await_new_claimant(spine: &SpineDb, pool: &PoolName, resource: &Resource, known: &mut Vec<Seq>) -> Seq {
    let started = Instant::now();
    loop {
        let q = locks::queue(spine, pool, resource).expect("queue");
        if let Some(e) = q.iter().find(|e| !known.contains(&e.seq) && !e.granted) {
            known.push(e.seq);
            return e.seq;
        }
        assert!(started.elapsed() < Duration::from_secs(25), "no racer ever queued on {resource:?}: {q:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

// ---------------------------------------------------------------------------
// helpers (copied from `seal.rs`, which follows `grammar_v6_data_door.rs`)
// ---------------------------------------------------------------------------

#[track_caller]
fn assert_sealed_body(w: &Witness, node: &NodeId) {
    assert_eq!(grammar_fault_tag(w), Some("sealed_body"), "the frozen tag: {w:?}");
    match w {
        Witness::GrammarViolation { pool, message, .. } => {
            assert_eq!(pool, &PoolName::new(POOL), "{w:?}");
            assert!(message.contains(&format!("{POOL}/{node}")), "names the node: {message}");
            assert!(message.contains("supersed"), "names the repair: {message}");
        }
        other => panic!("expected GrammarViolation, got {other:?}"),
    }
}

/// One statement: a MATCH by identifier and a SEAL per named node.
fn seal(c: &Cauldron, nodes: &[(&str, &NodeId)]) -> spine::error::Result<Outcome> {
    let mut stmt = String::new();
    let mut params = Vec::new();
    for (i, (t, id)) in nodes.iter().enumerate() {
        stmt.push_str(&format!("MATCH (s{i}: {POOL}/{t} {{ id: $s{i} }}) "));
        params.push((format!("s{i}"), json(&id.0)));
    }
    for i in 0..nodes.len() {
        stmt.push_str(&format!("SEAL (s{i}) "));
    }
    let params: Vec<(&str, serde_json::Value)> = params.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    c.submit(commit(stmt.trim_end(), &params))
}

fn set_body(c: &Cauldron, t: &str, id: &NodeId, body: &str) -> spine::error::Result<Outcome> {
    c.submit(commit(
        &format!("MATCH (a: {POOL}/{t} {{ id: $a }}) SET a.body = $b"),
        &[("a", json(&id.0)), ("b", json(body))],
    ))
}

fn body_of(c: &Cauldron, t: &str, id: &NodeId) -> serde_json::Value {
    let rows = read(c, &format!("MATCH (n: {POOL}/{t} {{ id: $id }}) RETURN n.body AS b"), &[("id", json(&id.0))]);
    assert_eq!(rows.len(), 1, "`{t}` {id} reads back");
    rows[0]["b"].clone()
}

/// The flag and the whole-node rev, read through the read door.
fn sealed_rev(c: &Cauldron, t: &str, id: &NodeId) -> (bool, i64) {
    let rows = read(
        c,
        &format!("MATCH (n: {POOL}/{t} {{ id: $id }}) RETURN n.sealed AS s, REV(n) AS r"),
        &[("id", json(&id.0))],
    );
    assert_eq!(rows.len(), 1, "`{t}` {id} reads back");
    (
        rows[0]["s"].as_bool().unwrap_or_else(|| panic!("`sealed` is a boolean: {:?}", rows[0])),
        rows[0]["r"].as_i64().unwrap_or_else(|| panic!("rev is an integer: {:?}", rows[0])),
    )
}

/// Every node in the pool, by type, with its flag and rev.
fn all_nodes(c: &Cauldron, g: &PoolGrammar) -> BTreeMap<NodeId, (bool, i64)> {
    let mut out = BTreeMap::new();
    for t in g.node_types.keys() {
        for r in read(c, &format!("MATCH (n: {POOL}/{t}) RETURN n.id AS id, n.sealed AS s, REV(n) AS r"), &[]) {
            out.insert(
                NodeId(r["id"].as_str().unwrap().to_string()),
                (r["s"].as_bool().expect("sealed is a boolean"), r["r"].as_i64().expect("rev")),
            );
        }
    }
    out
}

fn sealed_set(c: &Cauldron, g: &PoolGrammar) -> BTreeSet<NodeId> {
    all_nodes(c, g).into_iter().filter(|(_, (s, _))| *s).map(|(id, _)| id).collect()
}

fn scratch_path(tag: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-seal-race-{tag}-{}", std::process::id()))
}

fn scratch(tag: &str) -> PathBuf {
    let dir = scratch_path(tag);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn fresh(tag: &str) -> (Cauldron, PoolGrammar) {
    let g = grammar();
    let c = Cauldron::create(&scratch(tag), &[g.clone()], SpineConfig::default()).expect("create the cauldron");
    (c, g)
}

fn json(v: impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(v).expect("json")
}

fn unique(what: &str) -> String {
    static SERIAL: AtomicU32 = AtomicU32::new(0);
    format!("{what} {:06}", SERIAL.fetch_add(1, Ordering::SeqCst))
}

fn filler(n: u64) -> String {
    let n = n as usize;
    let mut s = unique("x");
    s.truncate(n);
    while s.chars().count() < n {
        s.push('y');
    }
    s
}

fn valid_fields(g: &PoolGrammar, t: &str) -> BTreeMap<String, serde_json::Value> {
    let mut out = BTreeMap::new();
    for (f, fdef) in &g.node_types[t].fields {
        if !fdef.required {
            continue;
        }
        let v = match fdef.kind {
            FieldKind::Text => {
                let floor = fdef.min_chars.unwrap_or(1).max(1);
                let cap = fdef.max_chars.unwrap_or(u64::MAX);
                json(filler(floor.max(20).min(cap)))
            }
            FieldKind::Object => {
                serde_json::json!({ "thread": "00000000-0000-4000-8000-000000000001", "msg": 1 })
            }
            FieldKind::Integer => json(1),
            FieldKind::Boolean => json(true),
            FieldKind::Any => json("any"),
        };
        out.insert(f.clone(), v);
    }
    out
}

fn targets_of(g: &PoolGrammar, kind: &str, src: &str) -> Vec<String> {
    let kdef = &g.edge_kinds[kind];
    g.node_types.keys().filter(|t| kdef.permits(&g.kind, t, true, src)).cloned().collect()
}

fn required_for_mint(g: &PoolGrammar, t: &str) -> BTreeMap<String, u32> {
    let tdef = &g.node_types[t];
    let mut m = tdef.required_edges_at_mint.clone();
    if let Some(first) = tdef.required_at_mint_any_of.first() {
        m.extend(first.edges.clone());
    }
    m
}

fn targets_ready(g: &PoolGrammar, t: &str, have: &BTreeMap<String, NodeId>) -> bool {
    required_for_mint(g, t).keys().all(|k| targets_of(g, k, t).iter().any(|d| have.contains_key(d)))
}

fn mint(
    c: &Cauldron,
    g: &PoolGrammar,
    t: &str,
    have: &BTreeMap<String, NodeId>,
    overrides: &[(&str, serde_json::Value)],
    omit: Option<&String>,
) -> spine::error::Result<NodeId> {
    let mut fields = valid_fields(g, t);
    if g.node_types[t].fields.contains_key("handle") {
        fields.insert("handle".into(), json(unique(t)));
    }
    for (f, v) in overrides {
        fields.insert((*f).to_string(), v.clone());
    }
    let mut params: Vec<(String, serde_json::Value)> = Vec::new();
    let mut props: Vec<String> = Vec::new();
    for (i, (f, v)) in fields.iter().enumerate() {
        props.push(format!("{f}: $f{i}"));
        params.push((format!("f{i}"), v.clone()));
    }
    if let Some(s) = g.node_types[t].states.first() {
        props.push(format!("state: \"{s}\""));
    }
    let mut matches = String::new();
    let mut edges = String::new();
    for (j, (k, _)) in required_for_mint(g, t).iter().enumerate() {
        if Some(k) == omit {
            continue;
        }
        let dt = targets_of(g, k, t)
            .into_iter()
            .find(|d| have.contains_key(d))
            .unwrap_or_else(|| panic!("no minted target for `{t}` -{k}->"));
        matches.push_str(&format!("MATCH (t{j}: {POOL}/{dt} {{ id: $t{j} }}) "));
        params.push((format!("t{j}"), json(&have[&dt].0)));
        edges.push_str(&format!("CREATE (n) -[{k}]-> (t{j}) "));
    }
    let stmt = format!("{matches}CREATE (n: {POOL}/{t} {{ {} }}) {edges}RETURN n.id", props.join(", "));
    let params: Vec<(&str, serde_json::Value)> = params.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    c.submit(commit(&stmt, &params)).map(|o| returned_id(o, "n.id"))
}

/// One node of every type (dependency order), plus nothing else.
fn seed_every_type(c: &Cauldron, g: &PoolGrammar) -> BTreeMap<String, NodeId> {
    let types: Vec<String> = g.node_types.keys().cloned().collect();
    let mut have: BTreeMap<String, NodeId> = BTreeMap::new();
    while have.len() < types.len() {
        let before = have.len();
        for t in &types {
            if have.contains_key(t) || !targets_ready(g, t, &have) {
                continue;
            }
            let id = mint(c, g, t, &have, &[], None).unwrap_or_else(|e| panic!("seed `{t}`: {e:?}"));
            have.insert(t.clone(), id);
        }
        assert!(have.len() > before, "no type could mint; stuck with {have:?}");
    }
    have
}

fn commit(statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
    let mut b = Bindings::new();
    for (k, v) in params {
        b.insert((*k).to_string(), v.clone());
    }
    Envelope {
        statement: statement.to_string(),
        bindings: b,
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent { thread: "seal-race".into(), dispatch: "seal-race".into() },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(unique("nonce")),
    }
}

fn returned_id(outcome: Outcome, column: &str) -> NodeId {
    match outcome {
        Outcome::Committed { returned, .. } => NodeId(returned.0[0][column].as_str().expect("id").to_string()),
        other => panic!("expected Committed, got {other:?}"),
    }
}

#[track_caller]
fn refused<T: std::fmt::Debug>(r: spine::error::Result<T>, what: &str) -> Witness {
    match r {
        Err(e) => only_witness(&e),
        Ok(v) => panic!("{what}: expected a refusal, got {v:?}"),
    }
}

#[track_caller]
fn only_witness(err: &SpineError) -> Witness {
    match err {
        SpineError::Refused { witnesses } => {
            assert_eq!(witnesses.len(), 1, "one refusal, one witness: {witnesses:?}");
            witnesses[0].clone()
        }
        other => panic!("expected a structured refusal, got {other:?}"),
    }
}

fn read(c: &Cauldron, statement: &str, params: &[(&str, serde_json::Value)]) -> Vec<BTreeMap<String, serde_json::Value>> {
    let mut b = Bindings::new();
    for (k, v) in params {
        b.insert((*k).to_string(), v.clone());
    }
    let rows = c
        .read(&ReadRequest {
            statement: statement.to_string(),
            bindings: b,
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect("read")
        .rows
        .0;
    rows.into_iter().map(|r| serde_json::from_value(serde_json::to_value(r).unwrap()).unwrap()).collect()
}
