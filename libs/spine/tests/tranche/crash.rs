//! CRASH RECOVERY: a
//! daemon killed at every named point of a tranche's life restarts into the
//! record the crash table predicts. Every kill here is a REAL PROCESS DEATH —
//! `spine_victim`'s `tranche` job, aborted by the seam at a named stage
//! (`kill::kill_at_with`, SIGABRT asserted) — and every reopen is the
//! healing `Cauldron::open`: the ordinary pass, THE TRANCHE PASS, then the
//! parent's own first `tranche_tick`, which is the one thing that lands.
//!
//! Tests carry a stable label prefix (`t19_<n>_…` for the tranche suite's
//! numbered cases, `g_…` for the guards). The kill tests are compiled only under
//! `fault-injection`; the default build proves each new stage name REFUSES
//! by name out of process (the production victim's exit 3) and runs the
//! two in-process guards (the halt, the orphan sweep).
//!
//! What a parent reads after a kill, and in which order: the DISK as the
//! victim left it (direct SQL, before any open — `pre_*`), then what the
//! healing open found and did (`recovered_at_open`, `tranches_at_open`),
//! then the record through the door (heartbeat, store, reads), then the
//! first tick and the record after it.

// The helpers below serve the kill tests, which exist only under
// `fault-injection`; the default build compiles the vocabulary test and the
// two in-process guards and would otherwise warn on every unused helper.
#![cfg_attr(not(feature = "fault-injection"), allow(dead_code))]

use serde_json::{json, Value};
use spine::config::SpineConfig;
use spine::envelope::{
    Envelope, Initiator, Instrument, Mode, Provenance, RippleKind, ENVELOPE_FORMAT_VERSION,
};
use spine::error::SpineError;
use spine::pool::Pool;
use spine::schema_def::PoolName;
use spine::spine_db::{Seq, SPINE_FILE};
use spine::tranche::{
    ChildOutcome, QuietLevels, TickReport, TrancheActor, TrancheLockMode, TrancheRecord,
    TrancheStage, TrancheState,
};
use spine::{Cauldron, Outcome, ReadRequest};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[path = "../util/kill.rs"]
mod kill;

const QUIET: QuietLevels = QuietLevels::BusNotConfigured;
const SERVICE: &str = "runner";
const ALPHA: &str = "fixture-alpha";
const BETA: &str = "fixture-beta";
/// The victim's keys (`spine_victim.rs` `tranche::KEY_T0`, `KEY_T`).
const KEY_T0: &str = "0101010101010101010101010101010101010101010101010101010101010101";
const KEY_T: &str = "0202020202020202020202020202020202020202020202020202020202020202";
/// The tranche timeout both sides use, so `boot + timeout` is computable.
const TIMEOUT_MS: i64 = 30_000;
const CEILING_MS: u64 = 2_000;

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-crash-{name}-{}", std::process::id()))
}

/// The SAME config the victim builds from its environment (`TIMEOUT_MS`,
/// `CEILING_MS`): the parent's reopen must compute the same deadline.
fn cfg() -> SpineConfig {
    let mut c = SpineConfig::default();
    c.busy_timeout = Duration::from_secs(5);
    c.lock_wait_ceiling = Duration::from_millis(CEILING_MS);
    c.tranche.timeout = Duration::from_millis(TIMEOUT_MS as u64);
    c.tranche.heartbeat_interval = Duration::from_millis(TIMEOUT_MS as u64 / 3);
    c
}

fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn instrument() -> Instrument {
    Instrument {
        name: "runner".into(),
        version: "test-build".into(),
    }
}

fn actor(t: &str, k: &str) -> TrancheActor {
    TrancheActor {
        tranche: t.into(),
        key: k.into(),
        holder_service: SERVICE.into(),
    }
}

fn bindings(params: &[(&str, Value)]) -> spine::envelope::Bindings {
    params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

/// A type-one child under run `T`.
fn child(step: &str, statement: &str, params: &[(&str, Value)]) -> Envelope {
    Envelope {
        statement: statement.to_string(),
        bindings: bindings(params),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Ripple {
                kind: RippleKind::TypeOne,
                run_id: "T".into(),
                step_id: step.to_string(),
                scope: vec![],
            },
            instrument: instrument(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(step.to_string()),
    }
}

/// An ordinary (keyless) writer — the agent at the door.
fn keyless(c: &Cauldron, nonce: &str, statement: &str, params: &[(&str, Value)]) -> spine::error::Result<Outcome> {
    c.submit(Envelope {
        statement: statement.to_string(),
        bindings: bindings(params),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent {
                thread: "crash".into(),
                dispatch: nonce.into(),
            },
            instrument: instrument(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(nonce.to_string()),
    })
}

const WRITER: &str = r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" }) RETURN d.id"#;

fn req(statement: &str, params: &[(&str, Value)]) -> ReadRequest {
    ReadRequest {
        statement: statement.to_string(),
        bindings: bindings(params),
        format_version: ENVELOPE_FORMAT_VERSION,
    }
}

fn committed(c: &Cauldron, statement: &str, params: &[(&str, Value)]) -> Vec<BTreeMap<String, Value>> {
    c.read(&req(statement, params)).expect("committed read").rows.0
}

fn heartbeat_result(c: &Cauldron, t: &str, k: &str) -> spine::error::Result<TrancheRecord> {
    c.tranche_heartbeat(&actor(t, k), &QUIET, wall_ms())
}

fn heartbeat(c: &Cauldron, t: &str, k: &str, now: i64) -> TrancheRecord {
    c.tranche_heartbeat(&actor(t, k), &QUIET, now).unwrap_or_else(|e| panic!("heartbeat {t}: {e:?}"))
}

fn tick(c: &Cauldron, now: i64) -> TickReport {
    match c.tranche_tick(&QUIET, now) {
        Ok(r) => r,
        Err(SpineError::Busy { .. }) => TickReport::default(),
        Err(e) => panic!("tick: {e:?}"),
    }
}

fn state_of(c: &Cauldron, t: &str) -> TrancheState {
    c.tranche_store(t, None).expect("store").state
}

/// Wait (bounded, without ticking) for `t` to be `landed` — the lander the
/// FIRST tick spawned finishing on its own thread. On the bound, the panic
/// names the record's `landing_error`, so a lander that stopped short says
/// why (a forced red's shape: a part applied twice is an invariant
/// violation the lander records as `internal`).
fn wait_landed(c: &Cauldron, dir: &Path, t: &str, what: &str) {
    let started = Instant::now();
    while state_of(c, t) != TrancheState::Landed {
        if started.elapsed() >= Duration::from_secs(10) {
            let err: Option<String> = spine_sql(dir)
                .query_row("SELECT landing_error FROM tranches WHERE id = ?1 AND attempt = 1", [t], |r| r.get(0))
                .unwrap();
            panic!("{what}: never landed: {:?}; landing_error = {}", state_of(c, t), err.unwrap_or_else(|| "none".into()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The holder's `commit` on a thread, ticking here until it answers.
fn land(c: &Cauldron, t: &str, k: &str) -> spine::error::Result<spine::tranche::TrancheCommitted> {
    let (c2, a) = (c.clone(), actor(t, k));
    let h = std::thread::spawn(move || c2.tranche_commit(&a, wall_ms()));
    let started = Instant::now();
    while !h.is_finished() {
        tick(c, wall_ms());
        assert!(started.elapsed() < Duration::from_secs(20), "the landing never finished");
        std::thread::sleep(Duration::from_millis(20));
    }
    h.join().expect("the commit thread")
}

/// `land` on an explicit clock: the commit and every tick are stamped
/// `now`, so a scheduler stall between them cannot lapse a deadline the
/// caller set from the same clock (see g_j3). After the commit's flip
/// the heartbeat is not enforced, and the lander needs no clock
/// advance on its success path.
fn land_at(c: &Cauldron, t: &str, k: &str, now: i64) -> spine::error::Result<spine::tranche::TrancheCommitted> {
    let (c2, a) = (c.clone(), actor(t, k));
    let h = std::thread::spawn(move || c2.tranche_commit(&a, now));
    let started = Instant::now();
    while !h.is_finished() {
        tick(c, now);
        assert!(started.elapsed() < Duration::from_secs(20), "the landing never finished");
        std::thread::sleep(Duration::from_millis(20));
    }
    h.join().expect("the commit thread")
}

// ---------------------------------------------------------------------------
// reading the disk — before the open, and after
// ---------------------------------------------------------------------------

fn spine_sql(dir: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("open spine.db");
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn
}

fn pool_sql(dir: &Path, pool: &str) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(Pool::path_in(dir, &PoolName::new(pool))).expect("open the pool");
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn
}

/// Every row of every table of a database, as sorted strings — the POOL
/// CONTENTS the crash table's cells name, compared across an open to prove
/// "nothing at open" (the pools are identical between open and the first
/// tick).
fn dump(conn: &rusqlite::Connection) -> Vec<String> {
    let mut tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    tables.sort();
    let mut out = Vec::new();
    for t in tables {
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
        let n = stmt.column_count();
        let rows: Vec<String> = stmt
            .query_map([], |r| {
                let mut cells = Vec::with_capacity(n);
                for i in 0..n {
                    cells.push(format!("{:?}", r.get::<_, rusqlite::types::Value>(i)?));
                }
                Ok(format!("{t}|{}", cells.join("|")))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        out.extend(rows);
    }
    out.sort();
    out
}

fn pools(dir: &Path) -> Vec<String> {
    let mut all = dump(&pool_sql(dir, ALPHA));
    all.extend(dump(&pool_sql(dir, BETA)));
    all
}

/// One `tranches` row, every column, as bytes-for-comparison.
fn tranche_row(dir: &Path, t: &str) -> String {
    let conn = spine_sql(dir);
    let mut stmt = conn.prepare("SELECT * FROM tranches WHERE id = ?1 AND attempt = 1").unwrap();
    let n = stmt.column_count();
    stmt.query_row([t], |r| {
        let mut cells = Vec::with_capacity(n);
        for i in 0..n {
            cells.push(format!("{:?}", r.get::<_, rusqlite::types::Value>(i)?));
        }
        Ok(cells.join("|"))
    })
    .unwrap()
}

fn tranche_cols(dir: &Path, t: &str) -> (String, bool, i64, i64, bool, u32) {
    spine_sql(dir)
        .query_row(
            "SELECT state, decided, last_heartbeat, deadline, draining, staged FROM tranches WHERE id = ?1 AND attempt = 1",
            [t],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)? != 0,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)? != 0,
                    r.get::<_, i64>(5)? as u32,
                ))
            },
        )
        .unwrap()
}

fn anchor_seq(dir: &Path, t: &str) -> i64 {
    spine_sql(dir)
        .query_row("SELECT seq FROM tranches WHERE id = ?1 AND attempt = 1", [t], |r| r.get(0))
        .unwrap()
}

fn lock_rows(dir: &Path, seq: i64) -> i64 {
    spine_sql(dir)
        .query_row("SELECT COUNT(*) FROM locks WHERE seq = ?1", [seq], |r| r.get(0))
        .unwrap()
}

/// `(decided, cleared)` of one `transactions` row.
fn tx_flags(dir: &Path, seq: i64) -> (bool, bool) {
    spine_sql(dir)
        .query_row("SELECT decided, cleared FROM transactions WHERE seq = ?1", [seq], |r| {
            Ok((r.get::<_, i64>(0)? != 0, r.get::<_, i64>(1)? != 0))
        })
        .unwrap()
}

/// The RESERVED child rows of `T` (ordinal set): `(ordinal, decided,
/// cleared, committed_at)`.
fn reserved_rows(dir: &Path, t: &str) -> Vec<(i64, bool, bool, Option<i64>)> {
    let conn = spine_sql(dir);
    let mut stmt = conn
        .prepare(
            "SELECT tranche_ordinal, decided, cleared, committed_at FROM transactions
             WHERE tranche_id = ?1 AND tranche_ordinal IS NOT NULL ORDER BY tranche_ordinal",
        )
        .unwrap();
    stmt.query_map([t], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)? != 0,
            r.get::<_, i64>(2)? != 0,
            r.get::<_, Option<i64>>(3)?,
        ))
    })
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

fn landed_children(dir: &Path, t: &str) -> u32 {
    spine_sql(dir)
        .query_row("SELECT landed_children FROM tranches WHERE id = ?1 AND attempt = 1", [t], |r| r.get::<_, i64>(0))
        .unwrap() as u32
}

/// Child `ordinal`'s hash off `tranche_children` (the reserved row's hash).
fn child_hash(dir: &Path, t: &str, ordinal: u32) -> String {
    spine_sql(dir)
        .query_row(
            "SELECT hash FROM tranche_children WHERE tranche_id = ?1 AND tranche_attempt = 1 AND ordinal = ?2",
            rusqlite::params![t, ordinal],
            |r| r.get(0),
        )
        .unwrap()
}

/// Is part `part` of `hash` marked applied in `pool` (`applied_parts`)?
fn has_marker(dir: &Path, pool: &str, hash: &str, part: u32) -> bool {
    pool_sql(dir, pool)
        .query_row(
            "SELECT COUNT(*) FROM applied_parts WHERE tx_hash = ?1 AND part = ?2",
            rusqlite::params![hash, part],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
        == 1
}

/// The node child `ordinal`'s staged payload CREATES (its first
/// `create_node` delta's id), off `tranche_children`.
fn created_node_of(dir: &Path, t: &str, ordinal: u32) -> String {
    let payload: String = spine_sql(dir)
        .query_row(
            "SELECT payload FROM tranche_children WHERE tranche_id = ?1 AND tranche_attempt = 1 AND ordinal = ?2",
            rusqlite::params![t, ordinal],
            |r| r.get(0),
        )
        .unwrap();
    let v: Value = serde_json::from_str(&payload).unwrap();
    v["parts"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|p| p["deltas"].as_array().cloned().unwrap_or_default())
        .find(|d| d["op"] == json!("create_node"))
        .and_then(|d| d["id"].as_str().map(str::to_string))
        .unwrap_or_else(|| panic!("child {ordinal} of {t} creates no node: {payload}"))
}

fn touched(c: &Cauldron, t: &str) -> Vec<(String, String, String, bool)> {
    c.tranche_touched(t, 1).expect("touched")
}

/// The author's `docs_written` count cell, read off the pool file — the
/// cell a part applied twice would double (t19_30's red).
fn docs_written(dir: &Path, author: &str) -> i64 {
    pool_sql(dir, ALPHA)
        .query_row(
            "SELECT COALESCE((SELECT value FROM count_cells WHERE node_id = ?1 AND cell = 'docs_written'), 0)",
            [author],
            |r| r.get(0),
        )
        .unwrap()
}

fn tickets(c: &Cauldron) -> usize {
    committed(c, "MATCH (t: fixture-beta/ticket) RETURN t.id", &[]).len()
}

fn doc_state(c: &Cauldron, id: &str) -> Option<String> {
    committed(c, "MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.state", &[("id", json!(id))])
        .first()
        .and_then(|r| r["d.state"].as_str().map(str::to_string))
}

/// `<pool>/<id>` → `<id>`.
fn bare(id: &str) -> String {
    id.split_once('/').map(|(_, i)| i.to_string()).unwrap_or_else(|| id.to_string())
}

// ---------------------------------------------------------------------------
// the kill, and the healing open
// ---------------------------------------------------------------------------

/// KILL THE VICTIM at `stage` inside `scenario` (`spine_victim`'s `tranche`
/// job) — a real process death, SIGABRT asserted, the stage named — and
/// hand back what it said before it died.
#[cfg(feature = "fault-injection")]
fn kill(dir: &Path, scenario: &str, stage: &str, extra: &[(&str, &str)]) -> std::process::Output {
    let timeout = TIMEOUT_MS.to_string();
    let ceiling = CEILING_MS.to_string();
    let mut env: Vec<(&str, &str)> = vec![
        ("SPINE_VICTIM_TRANCHE", scenario),
        ("SPINE_VICTIM_TIMEOUT_MS", timeout.as_str()),
        ("SPINE_VICTIM_CEILING_MS", ceiling.as_str()),
    ];
    env.extend_from_slice(extra);
    kill::kill_at_with("tranche", dir, stage, &env)
}

#[cfg(feature = "fault-injection")]
fn said(out: &std::process::Output, key: &str) -> String {
    kill::said(out, key).unwrap_or_else(|| {
        panic!(
            "the victim never said `{key}`.\nstdout:\n{}\nstderr:\n{}",
            kill::stdout_of(out),
            kill::stderr_of(out)
        )
    })
}

/// THE HEALING OPEN, bracketed by the wall clock so `boot` is bounded:
/// `(cauldron, before_ms, after_ms)`.
fn reopen(dir: &Path) -> (Cauldron, i64, i64) {
    let before = wall_ms();
    let c = Cauldron::open(dir, cfg()).expect("the healing open");
    let after = wall_ms();
    (c, before, after)
}

/// The deadline is `boot + timeout` for a boot inside the open.
fn assert_deadline_is_boot_plus_timeout(deadline: i64, before: i64, after: i64, what: &str) {
    assert!(
        deadline >= before + TIMEOUT_MS && deadline <= after + TIMEOUT_MS,
        "{what}: deadline {deadline} is not boot + timeout for a boot in [{before}, {after}] (timeout {TIMEOUT_MS})"
    );
}

// ---------------------------------------------------------------------------
// t19_27 — killed while `queued`
// ---------------------------------------------------------------------------

/// Crash-table row 1. `T0` open (touched), `T1` queued at position 2;
/// the victim dies at `after-begin`. After the healing open: `T1` is
/// `queued`, its deadline `boot + timeout`, its position 2 — and `T0`'s
/// deadline is reset too, `last_heartbeat` on both untouched (g_d3).
/// RED: `reset_deadlines_at_open` leaving `deadline` alone → not boot +
/// timeout; the pass rewriting `last_heartbeat` → differs.
#[cfg(feature = "fault-injection")]
#[test]
fn t19_27_killed_while_queued_restarts_queued_with_deadline_boot_plus_timeout_and_position_right() {
    let dir = root("t27");
    let out = kill(&dir, "queued", TrancheStage::AfterBegin.name(), &[]);
    assert_eq!(said(&out, "t0_state"), "open");
    // The disk as the victim left it.
    let (s1, _, hb1, dl1, _, _) = tranche_cols(&dir, "T1");
    let (s0, _, hb0, _, _, _) = tranche_cols(&dir, "T0");
    assert_eq!((s0.as_str(), s1.as_str()), ("open", "queued"), "as the victim left it");

    let (c, before, after) = reopen(&dir);
    let pass = c.tranches_at_open();
    assert_eq!(pass.deadlines_reset, 2, "both live rows reset: {pass:?}");
    let t1 = pass.tranches.iter().find(|t| t.tranche == "T1").expect("T1 in the pass");
    assert_eq!((t1.state, t1.position, t1.overlay_rebuilt), (TrancheState::Queued, 2, false), "{pass:?}");
    assert_deadline_is_boot_plus_timeout(t1.deadline, before, after, "T1 in the pass");
    assert!(t1.deadline != dl1 || before + TIMEOUT_MS == dl1, "the pass RESET the deadline (was {dl1})");

    // Through the door: the holder reconnects, beats, `queued`, position 2.
    let r = heartbeat(&c, "T1", KEY_T, wall_ms());
    assert_eq!((r.state, r.position), (TrancheState::Queued, Some(2)));
    // g_d3: `last_heartbeat` as the victim left it (the beat above moved
    // it; read the disk before the beat through the pass's own row — the
    // pass reports the deadline, and the column is compared directly).
    let (_, _, hb1_after_open, dl1_after_open, _, _) = {
        // Re-read what the OPEN wrote (before our beat) from the row the
        // beat rewrote is impossible, so T0 — never beaten here — carries
        // the g_d3 assertion for this test; g_d3 proper is below.
        tranche_cols(&dir, "T1")
    };
    let _ = (hb1, hb1_after_open, dl1_after_open);
    let (_, _, hb0_after, dl0_after, _, _) = tranche_cols(&dir, "T0");
    assert_eq!(hb0_after, hb0, "T0's last_heartbeat is NOT rewritten by the pass");
    assert_deadline_is_boot_plus_timeout(dl0_after, before, after, "T0 after the open");
    let r0 = heartbeat(&c, "T0", KEY_T0, wall_ms());
    assert_eq!(r0.state, TrancheState::Open);
}

// ---------------------------------------------------------------------------
// t19_28 — killed while `staging` after k children
// ---------------------------------------------------------------------------

/// Crash-table row 2, `k = 3` (child 0 a `wrote` doc, child 1 a
/// ticket cataloguing it and publishing it, child 2 another doc); the victim
/// dies at `after-stage` of child 2. After the open: `staged = 3`, the pass
/// rebuilt the overlay, a read under key shows child 1's `SET` on child 0's
/// doc (`published`) with `children_seen = 3`, the committed pools show no
/// such doc; child 3 stages (it MATCHes child 2's doc, which exists only in
/// the overlay), the tranche lands, four rows. The touched set is identical
/// by key before the kill (the victim's `touched_before`, children 0–1) and
/// after the open + one tick as far as those rows go.
/// RED: skip the overlay rebuild → child 3's MATCH misses child 2's
/// node.
#[cfg(feature = "fault-injection")]
#[test]
fn t19_28_killed_while_staging_after_k_children_restarts_with_staged_k_the_overlay_rebuilt_and_child_k_lands_n_rows() {
    let dir = root("t28");
    let out = kill(&dir, "staging", TrancheStage::AfterStage.name(), &[("SPINE_VICTIM_CHILDREN", "3")]);
    let author = said(&out, "author");
    let doc0 = bare(&said(&out, "child0"));
    let touched_before: Vec<(String, String, String, bool)> =
        serde_json::from_str(&said(&out, "touched_before")).expect("touched_before json");
    assert!(!touched_before.is_empty(), "children 0–1 enrolled rows");
    let (state, _, _, _, _, staged) = tranche_cols(&dir, "T");
    assert_eq!((state.as_str(), staged), ("staging", 3), "as the victim left it");
    let pools_before = pools(&dir);

    let (c, _, _) = reopen(&dir);
    let pass = c.tranches_at_open();
    let t = &pass.tranches[0];
    assert_eq!((t.state, t.staged, t.overlay_rebuilt), (TrancheState::Staging, 3, true), "{pass:?}");
    assert_eq!(pools(&dir), pools_before, "the pass wrote no pool (nothing at open)");
    let r = heartbeat(&c, "T", KEY_T, wall_ms());
    assert_eq!((r.state, r.staged), (TrancheState::Staging, 3));
    // The overlay: child 1's SET on child 0's doc, visible under the key
    // only.
    let under = c
        .tranche_read(
            &actor("T", KEY_T),
            &req("MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.state", &[("id", json!(doc0))]),
            wall_ms(),
        )
        .expect("read under key");
    assert_eq!(under.children_seen, 3, "children_seen = k");
    assert_eq!(under.rows.0.first().map(|r| r["d.state"].clone()), Some(json!("published")), "the overlay shows child 1's SET: {:?}", under.rows);
    assert_eq!(doc_state(&c, &doc0), None, "nothing landed: the committed pools hold no such doc");
    // Child 2's doc exists only in the overlay (the victim died before it
    // could say its id): its id is the `create_node` delta of child 2's
    // staged payload (`tranche_children`), and a read under the key sees it
    // while the committed read does not.
    let doc2 = created_node_of(&dir, "T", 2);
    let under2 = c
        .tranche_read(&actor("T", KEY_T), &req("MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.title", &[("id", json!(doc2))]), wall_ms())
        .expect("read under key");
    assert_eq!(under2.rows.0.first().map(|r| r["d.title"].clone()), Some(json!("child 2")), "child 2's doc in the overlay: {:?}", under2.rows);
    assert_eq!(doc_state(&c, &doc2), None, "child 2's doc is not committed");
    // After one tick (the sweep included) the rows the victim named stand.
    let _ = tick(&c, wall_ms());
    let after: Vec<_> = touched(&c, "T");
    for row in &touched_before {
        assert!(after.contains(row), "lock row {row:?} survived the pass and the tick; after: {after:?}");
    }
    // Child 3 MATCHes child 2's doc — a node that exists only in the overlay.
    let o = c
        .tranche_stage(
            &actor("T", KEY_T),
            child("step-3", r#"MATCH (d: fixture-alpha/doc { id: $d }) SET d.title = "after the restart" RETURN d.id"#, &[("d", json!(doc2))]),
            wall_ms(),
        )
        .expect("child 3 stages against the rebuilt overlay");
    assert!(matches!(o, ChildOutcome::Staged { ordinal: 3, .. }), "{o:?}");
    let landed = land(&c, "T", KEY_T).expect("lands");
    assert_eq!(landed.children.len(), 4, "N rows: {landed:?}");
    assert_eq!(state_of(&c, "T"), TrancheState::Landed);
    assert_eq!(doc_state(&c, &doc0), Some("published".into()));
    assert_eq!(docs_written(&dir, &author), 2, "children 0 and 2 wrote one doc each");
    assert_eq!(tickets(&c), 1);
    assert_eq!(reserved_rows(&dir, "T").len(), 4);
}

// ---------------------------------------------------------------------------
// t19_29 — killed in `landing` before the decision
// ---------------------------------------------------------------------------

/// Crash-table row 3. The holder's `commit` flipped and the lander
/// died at `before-decision`. After the open: `landing, decided = 0`, no
/// reserved row, the pools as the victim left them (nothing at open); the
/// parent's FIRST tick spawns the lander, which re-validates and decides
/// with no holder; the holder's later `commit` is answered from the record;
/// exactly one decision (N reserved rows, no second reservation).
/// RED: treat `landing` as `staging` at boot → the first tick lands nothing.
/// RED (g_d2): land in the pass → the pools differ at open.
#[cfg(feature = "fault-injection")]
#[test]
fn t19_29_killed_in_landing_before_the_decision_the_first_tick_lands_it_with_no_holder_and_the_commit_answers_the_record() {
    let dir = root("t29");
    let out = kill(&dir, "commit", "before-decision", &[("SPINE_VICTIM_CHILDREN", "2")]);
    let author = said(&out, "author");
    let (state, decided, _, _, _, _) = tranche_cols(&dir, "T");
    assert_eq!((state.as_str(), decided), ("landing", false), "as the victim left it");
    assert!(reserved_rows(&dir, "T").is_empty());
    let pools_before = pools(&dir);

    let (c, before, after) = reopen(&dir);
    let t = &c.tranches_at_open().tranches[0];
    assert_eq!((t.state, t.decided, t.overlay_rebuilt), (TrancheState::Landing, false, false));
    assert_deadline_is_boot_plus_timeout(t.deadline, before, after, "a landing tranche's deadline is reset too");
    assert_eq!(pools(&dir), pools_before, "NOTHING AT OPEN: the pools are as the victim left them");
    assert!(reserved_rows(&dir, "T").is_empty(), "no decision at open");
    assert_eq!(state_of(&c, "T"), TrancheState::Landing);
    // The first tick spawns the lander; it lands with no holder.
    let first = tick(&c, wall_ms());
    assert_eq!(first.landers_spawned, vec![("T".to_string(), 1)], "{first:?}");
    wait_landed(&c, &dir, "T", "the lander spawned by the first tick");
    let rows = reserved_rows(&dir, "T");
    assert_eq!(rows.len(), 2, "ONE decision reserved N rows: {rows:?}");
    assert!(rows.iter().all(|(_, d, cl, at)| *d && *cl && at.is_some()), "{rows:?}");
    // The holder's later commit answers the record.
    let reply = c.tranche_commit(&actor("T", KEY_T), wall_ms()).expect("answered from the record");
    assert_eq!((reply.state, reply.children.len()), (TrancheState::Landed, 2));
    assert_eq!(reserved_rows(&dir, "T").len(), 2, "no second decision");
    assert_eq!(docs_written(&dir, &author), 1);
    assert_eq!(tickets(&c), 1);
    assert_eq!(lock_rows(&dir, anchor_seq(&dir, "T")), 0);
}

// ---------------------------------------------------------------------------
// t19_30 — killed after the decision, at five points
// ---------------------------------------------------------------------------

/// Crash-table rows 4–5, `k = 3`. Five kill points inside the landing:
/// `after-decision` (before the first child), `between-children`,
/// `between-child-parts` (inside child 1, alpha applied, beta not),
/// `before-child-clear` (child 0's part applied and flagged, its row not
/// cleared), `before-clear` (every child cleared, the final clear not run). After the
/// open NOTHING has moved (pools identical); after the first tick: every
/// child cleared, every part applied ONCE (`docs_written = 2`, one ticket,
/// child 0 published), the anchor cleared, no lock row under the tranche's
/// `seq`, three commits each stamped once.
/// RED: remove the marker read in `apply_part` → at `before-child-clear`
/// child 0's part applies twice and `docs_written` is 3.
#[cfg(feature = "fault-injection")]
#[test]
fn t19_30_killed_after_the_decision_at_five_points_every_child_cleared_every_part_once_anchor_cleared_no_lock_row() {
    for stage in ["after-decision", "between-children", "between-child-parts", "before-child-clear", "before-clear"] {
        let dir = root(&format!("t30-{stage}"));
        let out = kill(&dir, "commit", stage, &[("SPINE_VICTIM_CHILDREN", "3")]);
        let author = said(&out, "author");
        let doc0 = bare(&said(&out, "child0"));
        let (state, decided, _, _, _, _) = tranche_cols(&dir, "T");
        assert_eq!((state.as_str(), decided), ("landing", true), "{stage}: as the victim left it");
        let pre = reserved_rows(&dir, "T");
        assert_eq!(pre.len(), 3, "{stage}: the decision reserved three rows: {pre:?}");
        let anchor = anchor_seq(&dir, "T");
        let locks_pre = lock_rows(&dir, anchor);
        assert!(locks_pre > 0, "{stage}: the tranche's rows are held until the final clear");
        let pools_before = pools(&dir);
        // What the point itself left: the crash-table cell for this row.
        match stage {
            "after-decision" => assert!(pre.iter().all(|(_, _, cl, _)| !cl), "{stage}: no child cleared: {pre:?}"),
            "between-children" | "before-child-clear" => {
                assert_eq!(docs_written(&dir, &author), 1, "{stage}: child 0's part applied once");
            }
            "between-child-parts" => {
                assert_eq!(pre.iter().filter(|(_, _, cl, _)| *cl).count(), 1, "{stage}: child 0 cleared, child 1 mid-landing: {pre:?}");
                let h1 = child_hash(&dir, "T", 1);
                assert!(has_marker(&dir, ALPHA, &h1, 0), "{stage}: child 1's alpha part applied and marked");
                assert!(!has_marker(&dir, BETA, &h1, 1), "{stage}: child 1's beta part NOT applied");
            }
            "before-clear" => {
                assert!(pre.iter().all(|(_, d, cl, _)| *d && *cl), "{stage}: every child cleared: {pre:?}");
                assert_eq!(tx_flags(&dir, anchor), (false, false), "{stage}: the anchor un-decided, un-cleared");
            }
            _ => unreachable!(),
        }

        let (c, _, _) = reopen(&dir);
        assert_eq!(pools(&dir), pools_before, "{stage}: NOTHING AT OPEN");
        assert_eq!(reserved_rows(&dir, "T"), pre, "{stage}: no row flipped at open");
        assert_eq!(lock_rows(&dir, anchor), locks_pre, "{stage}: no lock released at open");
        let first = tick(&c, wall_ms());
        assert_eq!(first.landers_spawned, vec![("T".to_string(), 1)], "{stage}: {first:?}");
        wait_landed(&c, &dir, "T", stage);
        let rows = reserved_rows(&dir, "T");
        assert_eq!(rows.len(), 3, "{stage}: {rows:?}");
        assert!(rows.iter().all(|(_, d, cl, at)| *d && *cl && at.is_some()), "{stage}: every child decided, cleared, stamped: {rows:?}");
        let stamps: std::collections::BTreeSet<i64> = rows.iter().filter_map(|(_, _, _, at)| *at).collect();
        assert_eq!(stamps.len(), 3, "{stage}: N commits counted once each: {rows:?}");
        assert_eq!(tx_flags(&dir, anchor), (false, true), "{stage}: the anchor cleared, never decided");
        assert_eq!(lock_rows(&dir, anchor), 0, "{stage}: no lock row under the tranche's seq");
        assert_eq!(docs_written(&dir, &author), 2, "{stage}: EVERY PART ONCE — children 0 and 2 each wrote one doc");
        assert_eq!(tickets(&c), 1, "{stage}: one ticket");
        assert_eq!(doc_state(&c, &doc0), Some("published".into()), "{stage}");
        assert_eq!(landed_children(&dir, "T"), 3, "{stage}");
        assert!(matches!(heartbeat_result(&c, "T", KEY_T), Err(SpineError::Refused { .. })), "{stage}: a beat on a landed tranche is tranche_terminal");
        let reply = c.tranche_commit(&actor("T", KEY_T), wall_ms()).expect("the record answers");
        assert_eq!(reply.children.len(), 3, "{stage}");
    }
}

// ---------------------------------------------------------------------------
// t19_31 — killed during the rollback transaction
// ---------------------------------------------------------------------------

/// The crash table's last row. The victim dies at `in-rollback` — inside
/// the rollback's one transaction, after its statements, before its commit. The
/// transaction never committed: after the open the tranche is `staging`,
/// its two children present, its lock rows held and IDENTICAL BY KEY to the
/// victim's `touched_before` after the pass and one tick; the
/// holder's re-issued `rollback` succeeds and releases them.
/// RED: let the pass or the sweep release rows under a non-terminal
/// tranche's `seq` → the rows are gone.
#[cfg(feature = "fault-injection")]
#[test]
fn t19_31_killed_during_the_rollback_transaction_the_tranche_is_staging_with_locks_held_and_the_rollback_succeeds() {
    let dir = root("t31");
    let out = kill(&dir, "rollback", TrancheStage::InRollback.name(), &[("SPINE_VICTIM_CHILDREN", "2")]);
    let touched_before: Vec<(String, String, String, bool)> =
        serde_json::from_str(&said(&out, "touched_before")).expect("touched_before json");
    assert!(!touched_before.is_empty());
    let (state, _, _, _, _, staged) = tranche_cols(&dir, "T");
    assert_eq!((state.as_str(), staged), ("staging", 2), "the transaction did not commit");
    let anchor = anchor_seq(&dir, "T");
    assert_eq!(lock_rows(&dir, anchor) as usize, touched_before.len(), "locks held as the victim left them");

    let (c, _, _) = reopen(&dir);
    assert_eq!(c.tranches_at_open().tranches[0].state, TrancheState::Staging);
    let _ = tick(&c, wall_ms());
    assert_eq!(touched(&c, "T"), touched_before, "the lock rows, by key, identical before the kill and after the pass + one tick");
    let store = c.tranche_store("T", None).expect("store");
    assert_eq!((store.state, store.children.len()), (TrancheState::Staging, 2));
    let rb = c.tranche_rollback(&actor("T", KEY_T), wall_ms()).expect("the re-issued rollback succeeds");
    assert_eq!((rb.state, rb.stored), (TrancheState::RolledBack, 2));
    assert_eq!(lock_rows(&dir, anchor), 0);
    assert_eq!(tx_flags(&dir, anchor), (false, true));
}

// ---------------------------------------------------------------------------
// t19_32 — the gate is up from the first request after boot
// ---------------------------------------------------------------------------

/// A `whole` tranche `staging` (one child) is killed at
/// `after-stage`. After the open and BEFORE any tick or any act of the
/// holder, an agent's commit is `busy locked` — the gate is read off the
/// record, not raised by the first tick. Then the holder reconnects
/// (`staging`), a tick changes nothing for the writer, the holder commits
/// and lands, and the writer's retry lands.
/// RED: raise the gate from the scheduler's first tick → the commit lands
/// under the tranche.
#[cfg(feature = "fault-injection")]
#[test]
fn t19_32_killed_with_the_gate_up_the_gate_is_up_from_the_first_request_after_boot_before_the_holder_reconnects() {
    let dir = root("t32");
    let out = kill(&dir, "staging", TrancheStage::AfterStage.name(), &[("SPINE_VICTIM_MODE", "whole"), ("SPINE_VICTIM_CHILDREN", "1")]);
    assert!(said(&out, "begun").contains("state=open"), "{}", said(&out, "begun"));
    let (c, _, _) = reopen(&dir);
    let t = &c.tranches_at_open().tranches[0];
    assert_eq!((t.state, t.lock_mode, t.staged), (TrancheState::Staging, TrancheLockMode::Whole, 1));
    // THE FIRST REQUEST AFTER BOOT: an agent's commit, no tick, no holder.
    match keyless(&c, "w1", WRITER, &[("t", json!("under the gate"))]) {
        Err(SpineError::TrancheBusy { why, .. }) => assert_eq!(why, "locked"),
        other => panic!("the gate is up from boot: expected busy locked, got {other:?}"),
    }
    assert_eq!(spine_sql(&dir).query_row("SELECT COUNT(*) FROM transactions WHERE tranche_id IS NULL AND cleared = 0", [], |r| r.get::<_, i64>(0)).unwrap(), 0, "no row written for the refused writer");
    let r = heartbeat(&c, "T", KEY_T, wall_ms());
    assert_eq!((r.state, r.staged), (TrancheState::Staging, 1));
    let _ = tick(&c, wall_ms());
    assert!(matches!(keyless(&c, "w2", WRITER, &[("t", json!("still under the gate"))]), Err(SpineError::TrancheBusy { .. })));
    let landed = land(&c, "T", KEY_T).expect("lands");
    assert_eq!(landed.children.len(), 1);
    assert!(matches!(keyless(&c, "w3", WRITER, &[("t", json!("after the gate"))]), Ok(Outcome::Committed { .. })));
}

// ---------------------------------------------------------------------------
// t19_34 / g_d5 — a halt mid-tranche
// ---------------------------------------------------------------------------

/// See also g_d5 (both builds; no kill — a halt is a
/// state physics enters). A `touched` tranche `staging` with two children;
/// the spine HALTS (`recover::halt`, the test-support simulation of a disk
/// refusal). Every act answers `dead` (`Halted`): heartbeat, stage, commit,
/// rollback, the tick. The `tranches` row is BYTE-IDENTICAL before and after
/// — no `failed`, no `physics` reason. After repair (the halt flag lifted by
/// hand, out of band) and a restart (a fresh healing open), the tranche is
/// `staging` with its children and its lock rows, and the holder continues:
/// a third child stages and the tranche lands.
/// RED: write a `failed` reason on the halt → the bytes differ.
#[test]
fn t19_34_a_halt_mid_tranche_answers_dead_changes_no_tranche_row_and_after_repair_and_restart_the_tranche_is_staging() {
    let dir = root("t34");
    let _ = std::fs::remove_dir_all(&dir);
    let c = Cauldron::create(&dir, &[spine::schema_def::fixtures::fixture_alpha(), spine::schema_def::fixtures::fixture_beta()], cfg()).expect("create");
    let author = match keyless(&c, "seed-author", r#"CREATE (a: fixture-alpha/author { name: "halted" }) RETURN a.id"#, &[]) {
        Ok(Outcome::Committed { returned, .. }) => returned.0[0]["a.id"].as_str().unwrap().to_string(),
        other => panic!("{other:?}"),
    };
    let begun = c
        .tranche_begin(
            &spine::tranche::TrancheBegin {
                tranche: "T".into(),
                key: KEY_T.into(),
                holder_service: SERVICE.into(),
                instrument: instrument(),
                lock_mode: TrancheLockMode::Touched,
                regions: vec![ALPHA.into()],
            },
            &QUIET,
            wall_ms(),
        )
        .expect("begin");
    assert_eq!(begun.state, TrancheState::Open);
    let a = actor("T", KEY_T);
    let wrote = |i: u32| {
        child(
            &format!("step-{i}"),
            r#"MATCH (a: fixture-alpha/author { id: $a }) CREATE (d: fixture-alpha/doc { title: $t, state: "draft" }) CREATE (a) -[wrote]-> (d) RETURN d.id"#,
            &[("a", json!(author)), ("t", json!(format!("child {i}")))],
        )
    };
    for i in 0..2 {
        assert!(matches!(c.tranche_stage(&a, wrote(i), wall_ms()), Ok(ChildOutcome::Staged { .. })));
    }
    let row_before = tranche_row(&dir, "T");
    let touched_before = touched(&c, "T");
    let anchor = anchor_seq(&dir, "T");
    assert!(lock_rows(&dir, anchor) > 0);

    // THE HALT: physics refused; the spine is fail-closed, read-only.
    {
        let mut spine = spine::spine_db::SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
        spine::recover::halt(&mut spine, "physics: the disk refused the append").expect("halt recorded");
    }
    assert!(c.halted().unwrap().is_some());
    let dead = |r: Result<String, SpineError>, what: &str| match r {
        Err(SpineError::Halted { .. }) => {}
        other => panic!("{what} under a halt must answer dead (Halted), got {other:?}"),
    };
    dead(c.tranche_heartbeat(&a, &QUIET, wall_ms()).map(|r| format!("{r:?}")), "heartbeat");
    dead(c.tranche_stage(&a, wrote(2), wall_ms()).map(|r| format!("{r:?}")), "stage");
    dead(c.tranche_commit(&a, wall_ms()).map(|r| format!("{r:?}")), "commit");
    dead(c.tranche_rollback(&a, wall_ms()).map(|r| format!("{r:?}")), "rollback");
    dead(c.tranche_tick(&QUIET, wall_ms()).map(|r| format!("{r:?}")), "tick");
    dead(keyless(&c, "w", WRITER, &[("t", json!("x"))]).map(|r| format!("{r:?}")), "an ordinary writer");
    assert_eq!(tranche_row(&dir, "T"), row_before, "A HALT WRITES NO TRANCHE ROW: the row is byte-identical");
    assert_eq!(touched(&c, "T"), touched_before, "and releases no lock");

    // REPAIR, out of band, then RESTART: a fresh healing open.
    spine_sql(&dir)
        .execute("UPDATE spine_state SET halted = 0, halted_reason = NULL WHERE singleton = 1", [])
        .expect("repair");
    drop(c);
    let (c, before, after) = reopen(&dir);
    assert!(c.halted().unwrap().is_none());
    let t = &c.tranches_at_open().tranches[0];
    assert_eq!((t.state, t.staged, t.overlay_rebuilt), (TrancheState::Staging, 2, true));
    assert_deadline_is_boot_plus_timeout(t.deadline, before, after, "after repair");
    assert_eq!(touched(&c, "T"), touched_before, "its locks");
    assert_eq!(c.tranche_store("T", None).unwrap().children.len(), 2, "its children");
    // The holder continues.
    assert!(matches!(c.tranche_stage(&a, wrote(2), wall_ms()), Ok(ChildOutcome::Staged { ordinal: 2, .. })));
    let landed = land(&c, "T", KEY_T).expect("lands after the repair");
    assert_eq!(landed.children.len(), 3);
    assert_eq!(docs_written(&dir, &author), 3);
}

// ---------------------------------------------------------------------------
// g_d1 — `queued, draining = 1` survives the restart
// ---------------------------------------------------------------------------

/// g_d1 (the crash table has no such row). The victim: a
/// keyless two-part writer `W` granted and inside its landing (alpha landed,
/// blocked on a held beta); `T1` begins `whole` and is `queued, draining`
/// at the head (the gate is up from `begin`'s commit); the hold is released
/// and `W` dies at the door's `before-cleared` — decided, both parts landed,
/// un-cleared, its lock rows held. After the healing open: the ORDINARY
/// pass finished `W` (`cleared_only`) and released its locks; the tranche
/// pass left `T1` `queued, draining = 1`; an ordinary writer before any
/// tick is `busy locked` (the gate is up from boot); the holder's beat shows
/// `draining`, never `quiet`; the first tick finds `drained` empty and opens
/// `T1`. OPPOSED: the same crash with `T1` `touched` → `T1` was `open`
/// inline, and after the open the writer lands before any tick.
/// RED: clear `draining` in the tranche pass → the writer lands before the
/// tick.
#[cfg(feature = "fault-injection")]
#[test]
fn g_d1_killed_while_queued_and_draining_keeps_draining_and_opens_after_the_writers_locks_are_released() {
    for mode in ["whole", "touched"] {
        let dir = root(&format!("gd1-{mode}"));
        let out = kill(&dir, "draining", "before-cleared", &[("SPINE_VICTIM_MODE", mode)]);
        let begun = said(&out, "begun");
        let w_seq: i64 = said(&out, "w_seq").split_whitespace().next().unwrap().parse().unwrap();
        // The disk as the victim left it: W decided and un-cleared with its
        // lock rows; T1 as `begin` left it.
        assert_eq!(tx_flags(&dir, w_seq), (true, false), "{mode}: W decided, un-cleared");
        assert!(lock_rows(&dir, w_seq) > 0, "{mode}: W's granted rows stand");
        let (state, _, _, _, draining, _) = tranche_cols(&dir, "T1");
        if mode == "whole" {
            assert!(begun.contains("state=queued") && begun.contains("draining=true"), "{mode}: {begun}");
            assert_eq!((state.as_str(), draining), ("queued", true), "{mode}: as the victim left it");
        } else {
            assert!(begun.contains("state=open"), "{mode}: {begun}");
            assert_eq!((state.as_str(), draining), ("open", false), "{mode}");
        }

        let (c, _, _) = reopen(&dir);
        let ordinary = c.recovered_at_open();
        let w_hash = spine_sql(&dir).query_row("SELECT hash FROM transactions WHERE seq = ?1", [w_seq], |r| r.get::<_, String>(0)).unwrap();
        assert!(
            ordinary.cleared_only.iter().chain(ordinary.rolled_forward.iter()).any(|h| h.to_hex() == w_hash),
            "{mode}: the ordinary pass finished W: {ordinary:?}"
        );
        assert_eq!(tx_flags(&dir, w_seq), (true, true), "{mode}: W cleared");
        assert_eq!(lock_rows(&dir, w_seq), 0, "{mode}: W's locks released by the ordinary pass");
        let t1 = &c.tranches_at_open().tranches[0];
        let (state, _, _, _, draining, _) = tranche_cols(&dir, "T1");
        if mode == "whole" {
            assert_eq!((t1.state, t1.draining), (TrancheState::Queued, true), "{mode}: the pass PRESERVED draining: {t1:?}");
            assert_eq!((state.as_str(), draining), ("queued", true), "{mode}: on disk too");
            // The gate is up from boot: before any tick, before the holder.
            match keyless(&c, "w1", WRITER, &[("t", json!("under the gate at boot"))]) {
                Err(SpineError::TrancheBusy { why, .. }) => assert_eq!(why, "locked", "{mode}"),
                other => panic!("{mode}: the gate is up from boot: {other:?}"),
            }
            let beat = heartbeat(&c, "T1", KEY_T, wall_ms());
            assert_eq!((beat.state, beat.draining, beat.quiet.is_none()), (TrancheState::Queued, true, true), "{mode}: draining on the beat, never quiet");
            // The first tick: drained is empty (W's rows went with the
            // ordinary pass) → open.
            let first = tick(&c, wall_ms());
            assert_eq!(first.opened, vec![("T1".to_string(), 1)], "{mode}: {first:?}");
            assert_eq!(state_of(&c, "T1"), TrancheState::Open);
            assert!(matches!(keyless(&c, "w2", WRITER, &[("t", json!("still gated"))]), Err(SpineError::TrancheBusy { .. })), "{mode}");
            c.tranche_rollback(&actor("T1", KEY_T), wall_ms()).expect("rollback");
            assert!(matches!(keyless(&c, "w3", WRITER, &[("t", json!("after"))]), Ok(Outcome::Committed { .. })), "{mode}");
        } else {
            assert_eq!((t1.state, t1.draining), (TrancheState::Open, false), "{mode}: {t1:?}");
            // OPPOSED: no gate — the writer lands before any tick.
            assert!(matches!(keyless(&c, "w1", WRITER, &[("t", json!("no gate"))]), Ok(Outcome::Committed { .. })), "{mode}: a touched tranche gates nobody");
        }
    }
}

// ---------------------------------------------------------------------------
// g_d2 — every row of the crash table, at N = 2
// ---------------------------------------------------------------------------

/// g_d2. Every crash point of the crash table, with ONE CONCURRENT FOREIGN
/// WRITER beside the tranche — a parked, granted, un-decided row on the
/// seeded doc's body (the shape `locks.rs` parks) — so recovery has both
/// kinds of row to classify: the ordinary pass ABORTS the foreign row and
/// releases its lock (the ordinary rule), SKIPS every row with `tranche_id`
/// (the anchor, the reserved children), and the tranche pass does the table's
/// "does" cell. Per row, the "recovery finds / does" cells are asserted
/// literally: state, `decided`, lock-row counts under the anchor's `seq`,
/// the pool contents (identical across the open), `applied_parts` markers,
/// reserved rows — then the first tick does what the table says it does.
/// RED (named): the ordinary pass NOT skipping tranche rows → the anchor is
/// aborted (`skipped_tranche` empty, the anchor cleared); landing in the
/// pass instead of the tick → the pools differ across the open.
#[cfg(feature = "fault-injection")]
#[test]
fn g_d2_every_row_of_the_crash_table_at_n_2() {
    struct RowCase {
        name: &'static str,
        scenario: &'static str,
        stage: &'static str,
        children: &'static str,
    }
    let cases = [
        RowCase { name: "queued", scenario: "queued", stage: "after-begin", children: "0" },
        RowCase { name: "open/staging", scenario: "staging", stage: "after-stage", children: "2" },
        RowCase { name: "landing undecided", scenario: "commit", stage: "before-decision", children: "2" },
        RowCase { name: "landing decided, children partly landed", scenario: "commit", stage: "between-children", children: "3" },
        RowCase { name: "after the last child cleared, before the final clear", scenario: "commit", stage: "before-clear", children: "2" },
        RowCase { name: "during the rollback transaction", scenario: "rollback", stage: "in-rollback", children: "2" },
    ];
    for case in &cases {
        let dir = root(&format!("gd2-{}", case.stage));
        let out = kill(&dir, case.scenario, case.stage, &[("SPINE_VICTIM_FOREIGN", "parked"), ("SPINE_VICTIM_CHILDREN", case.children)]);
        let row = case.name;
        let t = if case.scenario == "queued" { "T1" } else { "T" };
        let foreign_seq: i64 = said(&out, "foreign_seq").parse().unwrap();
        let author = said(&out, "author");
        // THE DISK as the victim left it.
        assert_eq!(tx_flags(&dir, foreign_seq), (false, false), "{row}: the foreign writer un-decided, un-cleared");
        assert_eq!(lock_rows(&dir, foreign_seq), 1, "{row}: its granted write stands");
        let anchor = anchor_seq(&dir, t);
        let (state_pre, decided_pre, _, _, _, staged_pre) = tranche_cols(&dir, t);
        let locks_pre = lock_rows(&dir, anchor);
        let reserved_pre = reserved_rows(&dir, t);
        let pools_pre = pools(&dir);
        let foreign_hash = spine_sql(&dir).query_row("SELECT hash FROM transactions WHERE seq = ?1", [foreign_seq], |r| r.get::<_, String>(0)).unwrap();
        let anchor_hash = spine_sql(&dir).query_row("SELECT hash FROM transactions WHERE seq = ?1", [anchor], |r| r.get::<_, String>(0)).unwrap();

        let (c, before, after) = reopen(&dir);
        // The ordinary pass: the foreign row aborted, the tranche rows skipped.
        let ordinary = c.recovered_at_open();
        assert!(ordinary.aborted.iter().any(|h| h.to_hex() == foreign_hash), "{row}: the foreign writer aborted by the ordinary rule: {ordinary:?}");
        assert!(ordinary.skipped_tranche.iter().any(|h| h.to_hex() == anchor_hash), "{row}: the anchor skipped by the ordinary pass: {ordinary:?}");
        assert_eq!(tx_flags(&dir, foreign_seq), (false, true), "{row}: the foreign row aborted (cleared, un-decided)");
        assert_eq!(lock_rows(&dir, foreign_seq), 0, "{row}: its lock released");
        // The tranche pass: the table's "recovery finds / does" cells.
        let pass = c.tranches_at_open();
        let found = pass.tranches.iter().find(|x| x.tranche == t).unwrap_or_else(|| panic!("{row}: {t} in the pass: {pass:?}"));
        assert_eq!(found.state.as_str(), state_pre, "{row}: the state as found");
        assert_eq!(found.decided, decided_pre, "{row}: decided as found");
        assert_deadline_is_boot_plus_timeout(found.deadline, before, after, row);
        assert_eq!(pools(&dir), pools_pre, "{row}: the pools identical across the open — IT LANDS NOTHING");
        assert_eq!(reserved_rows(&dir, t), reserved_pre, "{row}: no reserved row flipped at open");
        assert_eq!(lock_rows(&dir, anchor), locks_pre, "{row}: the lock rows under the anchor's seq as found");
        assert_eq!(tx_flags(&dir, anchor), (false, false), "{row}: the anchor un-decided and un-cleared at open");
        match case.stage {
            "after-begin" => {
                assert_eq!((found.state, found.position, found.staged, locks_pre), (TrancheState::Queued, 2, 0, 0), "{row}: {found:?}");
                let r = heartbeat(&c, t, KEY_T, wall_ms());
                assert_eq!((r.state, r.position), (TrancheState::Queued, Some(2)), "{row}");
            }
            "after-stage" => {
                assert_eq!((found.state, found.staged, found.overlay_rebuilt), (TrancheState::Staging, 2, true), "{row}");
                assert!(locks_pre > 0, "{row}: the children's claims stand under the anchor's seq");
                assert_eq!(staged_pre, 2);
                let _ = tick(&c, wall_ms());
                assert_eq!(lock_rows(&dir, anchor), locks_pre, "{row}: a tick lands nothing for a staging tranche and sweeps none of its rows");
                assert_eq!(state_of(&c, t), TrancheState::Staging);
            }
            "before-decision" => {
                assert_eq!((found.state, found.decided), (TrancheState::Landing, false), "{row}");
                assert!(reserved_pre.is_empty(), "{row}: no reserved row before the decision");
                let first = tick(&c, wall_ms());
                assert_eq!(first.landers_spawned, vec![(t.to_string(), 1)], "{row}: the first tick runs the lander: {first:?}");
                wait_landed(&c, &dir, t, row);
                assert_eq!(reserved_rows(&dir, t).len(), 2, "{row}: one decision");
                assert_eq!(docs_written(&dir, &author), 1, "{row}");
            }
            "between-children" => {
                assert_eq!((found.state, found.decided), (TrancheState::Landing, true), "{row}");
                assert_eq!(reserved_pre.len(), 3, "{row}: three reserved rows: {reserved_pre:?}");
                assert_eq!(reserved_pre.iter().filter(|(_, _, cl, _)| *cl).count(), 1, "{row}: child 0 cleared, the rest not: {reserved_pre:?}");
                assert!(has_marker(&dir, ALPHA, &child_hash(&dir, t, 0), 0), "{row}: child 0's applied_parts marker in alpha");
                assert!(!has_marker(&dir, ALPHA, &child_hash(&dir, t, 1), 0), "{row}: child 1 not applied");
                assert_eq!(docs_written(&dir, &author), 1, "{row}: child 0's part applied once");
                let first = tick(&c, wall_ms());
                assert_eq!(first.landers_spawned, vec![(t.to_string(), 1)], "{row}: {first:?}");
                wait_landed(&c, &dir, t, row);
                let rows = reserved_rows(&dir, t);
                assert!(rows.len() == 3 && rows.iter().all(|(_, d, cl, _)| *d && *cl), "{row}: the lander continued from the first un-cleared child: {rows:?}");
                assert_eq!(docs_written(&dir, &author), 2, "{row}: a marked part skipped, the rest applied once");
                assert_eq!(tickets(&c), 1, "{row}");
                assert_eq!(lock_rows(&dir, anchor), 0, "{row}");
            }
            "before-clear" => {
                assert_eq!((found.state, found.decided), (TrancheState::Landing, true), "{row}");
                assert!(reserved_pre.len() == 2 && reserved_pre.iter().all(|(_, d, cl, _)| *d && *cl), "{row}: every child cleared: {reserved_pre:?}");
                assert!(locks_pre > 0, "{row}: lock rows present before the final clear");
                let first = tick(&c, wall_ms());
                assert_eq!(first.landers_spawned, vec![(t.to_string(), 1)], "{row}: {first:?}");
                wait_landed(&c, &dir, t, row);
                assert_eq!(tx_flags(&dir, anchor), (false, true), "{row}: the anchor cleared by the final clear");
                assert_eq!(lock_rows(&dir, anchor), 0, "{row}: every lock row under the tranche's seq released");
                assert_eq!(docs_written(&dir, &author), 1, "{row}: nothing applied twice");
            }
            "in-rollback" => {
                assert_eq!((found.state, found.staged), (TrancheState::Staging, 2), "{row}: the transaction did not commit");
                assert!(locks_pre > 0, "{row}");
                let rb = c.tranche_rollback(&actor(t, KEY_T), wall_ms()).expect("the holder re-issues rollback");
                assert_eq!(rb.stored, 2, "{row}");
                assert_eq!(lock_rows(&dir, anchor), 0, "{row}");
            }
            _ => unreachable!(),
        }
    }
}

// ---------------------------------------------------------------------------
// g_d3 — the deadline reset does not touch `last_heartbeat`
// ---------------------------------------------------------------------------

/// g_d3 (across a daemon restart): a
/// `staging` tranche killed at `after-stage`; `last_heartbeat` is
/// byte-identical on disk before and after the healing open, while
/// `deadline` became `boot + timeout` — read directly, before any act of the
/// holder could beat. Opposed in the same test: the deadline DID move
/// (unless the victim's clock made it equal by chance, excluded by the
/// bounds).
/// RED: rewrite `last_heartbeat` in `reset_deadlines_at_open` → differs.
#[cfg(feature = "fault-injection")]
#[test]
fn g_d3_deadline_reset_does_not_touch_last_heartbeat() {
    let dir = root("gd3");
    // The victim's clock is fixed far in the past, so the reset is visible
    // and `last_heartbeat` (the victim's clock) cannot collide with ours.
    let _ = kill(&dir, "staging", TrancheStage::AfterStage.name(), &[("SPINE_VICTIM_CHILDREN", "2"), ("SPINE_VICTIM_NOW_MS", "1000000")]);
    let (_, _, hb_pre, dl_pre, _, _) = tranche_cols(&dir, "T");
    // The victim's deadline is its fixed clock + timeout, plus the few ms a
    // claim wait's refresh adds from `Instant` (the poll refresh).
    assert!(
        dl_pre >= 1_000_000 + TIMEOUT_MS && dl_pre < 1_000_000 + TIMEOUT_MS + 5_000,
        "the victim's deadline, from its clock: {dl_pre}"
    );
    let (_, before, after) = reopen(&dir);
    let (_, _, hb_post, dl_post, _, _) = tranche_cols(&dir, "T");
    assert_eq!(hb_post, hb_pre, "last_heartbeat is NOT rewritten by the pass");
    assert_deadline_is_boot_plus_timeout(dl_post, before, after, "deadline = boot + timeout");
    assert_ne!(dl_post, dl_pre, "the deadline MOVED (opposed arm)");
}

// ---------------------------------------------------------------------------
// g_d4 — the orphan sweep runs on the tick
// ---------------------------------------------------------------------------

/// g_d4 (both builds). A CLEARED row with a stray lock
/// row — staged with the test-support writers: a pending row, a granted
/// write, then the row cleared by direct SQL with its lock left behind (the
/// crash between a cleared flip and its release) — is gone after ONE
/// `tranche_tick` with no door call, and the tick reports it. OPPOSED: the
/// granted row of an UN-cleared transaction is NOT swept by the same tick.
/// RED: do not call the sweep on the tick → the orphan remains.
#[test]
fn g_d4_the_orphan_sweep_runs_on_the_tick() {
    use spine::locks::{Claim, ClaimSet, LockMode, Resource};
    use spine::pool::NodeId;
    let dir = root("gd4");
    let _ = std::fs::remove_dir_all(&dir);
    let c = Cauldron::create(&dir, &[spine::schema_def::fixtures::fixture_alpha()], cfg()).expect("create");
    // OPPOSED, the TRANCHE arm: a live
    // `staging` tranche's GRANTED rows under its un-cleared anchor. A sweep
    // that took anything not held by a live ORDINARY writer would strip
    // them; the ordinary `live` arm below cannot see that.
    let tg = c
        .tranche_begin(
            &spine::tranche::TrancheBegin {
                tranche: "TG".into(),
                key: KEY_T.into(),
                holder_service: SERVICE.into(),
                instrument: instrument(),
                lock_mode: TrancheLockMode::Touched,
                regions: vec![ALPHA.into()],
            },
            &QUIET,
            wall_ms(),
        )
        .expect("begin TG");
    assert_eq!(tg.state, TrancheState::Open);
    assert!(matches!(
        c.tranche_stage(&actor("TG", KEY_T), child("g0", WRITER, &[("t", json!("tranche arm"))]), wall_ms()),
        Ok(ChildOutcome::Staged { .. })
    ));
    let tg_anchor = anchor_seq(&dir, "TG");
    let tg_rows = touched(&c, "TG");
    let tg_count = lock_rows(&dir, tg_anchor);
    assert!(!tg_rows.is_empty() && tg_count > 0, "SENTINEL: TG holds granted rows to sweep: {tg_rows:?}");
    let park = |tag: &str, node: &str| -> i64 {
        let mut spine = spine::spine_db::SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
        let env = Envelope {
            statement: format!("-- parked {tag}"),
            bindings: bindings(&[]),
            format_version: ENVELOPE_FORMAT_VERSION,
            provenance: Provenance {
                initiator: Initiator::CoreMechanical { operation: "test-outside".into() },
                instrument: instrument(),
            },
            mode: Mode::Commit,
            idempotency_nonce: Some(tag.to_string()),
        };
        let hash = env.hash_identity().unwrap();
        let seq = spine.append_pending(&env, &hash, &spine::delta::CompiledPayload { parts: vec![], minted: BTreeMap::new() }).unwrap();
        let mut set = ClaimSet::new();
        set.insert(Claim { pool: PoolName::new(ALPHA), resource: Resource::Body { node: NodeId(node.into()) }, mode: LockMode::Write });
        spine::locks::acquire(&spine, seq, &set, &cfg()).expect("granted");
        seq.0
    };
    let orphan = park("orphan", "n-orphan");
    let live = park("live", "n-live");
    // The crash between the cleared flip and the release: cleared, lock kept.
    assert_eq!(spine_sql(&dir).execute("UPDATE transactions SET cleared = 1 WHERE seq = ?1", [orphan]).unwrap(), 1);
    assert_eq!((lock_rows(&dir, orphan), lock_rows(&dir, live)), (1, 1), "the stray row and the live row both stand");
    // ONE tick, no door call.
    let r = tick(&c, wall_ms());
    assert_eq!(r.orphans_swept, 1, "the tick reports the sweep: {r:?}");
    assert_eq!(lock_rows(&dir, orphan), 0, "the orphan is gone after one tick");
    assert_eq!(lock_rows(&dir, live), 1, "OPPOSED: a live transaction's lock is never swept");
    assert_eq!(
        (touched(&c, "TG"), lock_rows(&dir, tg_anchor)),
        (tg_rows.clone(), tg_count),
        "OPPOSED (tranche arm): a live tranche's granted rows under its un-cleared anchor are never swept"
    );
    let r2 = tick(&c, wall_ms());
    assert_eq!(r2.orphans_swept, 0, "nothing to sweep on the next tick");
    assert_eq!(Seq(live).0, live);
}

// ---------------------------------------------------------------------------
// g_d5 — a halt changes no tranche row, for an `open` and a `landing` row
// ---------------------------------------------------------------------------

/// g_d5, beside t19_34's
/// `staging` shape (both builds; no kill). Two more rows of the record under
/// a halt: an `open` tranche with no child, and a `landing` tranche whose
/// `commit` flipped and whose lander has not run. The halt lands; every act
/// and the tick answer `dead`; BOTH `tranches` rows are byte-identical before
/// and after, and the `landing` row gained no `landing_error` (the tick under
/// a halt spawns no lander). After repair and a restart the `landing` one is
/// landed by the first tick and the `open` one continues (a child stages).
/// RED: write a `failed`/`physics` reason on the halt → the bytes differ.
#[test]
fn g_d5_a_halt_mid_tranche_changes_no_tranche_row() {
    let dir = root("gd5");
    let _ = std::fs::remove_dir_all(&dir);
    let c = Cauldron::create(&dir, &[spine::schema_def::fixtures::fixture_alpha()], cfg()).expect("create");
    let begin = |t: &str, k: &str| {
        c.tranche_begin(
            &spine::tranche::TrancheBegin {
                tranche: t.into(),
                key: k.into(),
                holder_service: SERVICE.into(),
                instrument: instrument(),
                lock_mode: TrancheLockMode::Touched,
                regions: vec![ALPHA.into()],
            },
            &QUIET,
            wall_ms(),
        )
        .unwrap_or_else(|e| panic!("begin {t}: {e:?}"))
    };
    // TL: lands first — opened, one child, its commit flipped (no tick: the
    // lander never ran, so it is `landing, decided = 0`).
    assert_eq!(begin("TL", KEY_T0).state, TrancheState::Open);
    let al = actor("TL", KEY_T0);
    assert!(matches!(c.tranche_stage(&al, child("l0", WRITER, &[("t", json!("landing child"))]), wall_ms()), Ok(ChildOutcome::Staged { .. })));
    // The JOINER: a commit whose join began BEFORE the halt.
    // It is joined below, bounded — never leaked.
    let joiner = {
        let (c2, a2) = (c.clone(), al.clone());
        let h = std::thread::spawn(move || c2.tranche_commit(&a2, wall_ms()));
        // The flip is durable long before the join would return; wait for
        // the state, bounded, without ticking.
        let started = Instant::now();
        while state_of(&c, "TL") != TrancheState::Landing {
            assert!(started.elapsed() < Duration::from_secs(10), "the commit never flipped");
            std::thread::sleep(Duration::from_millis(10));
        }
        h
    };
    // TO: queued behind TL (one non-queued tranche at a time).
    let to = begin("TO", KEY_T);
    assert_eq!((to.state, to.position), (TrancheState::Queued, 2));
    let row_l = tranche_row(&dir, "TL");
    let row_o = tranche_row(&dir, "TO");
    assert!(row_l.contains("\"landing\"") && row_o.contains("\"queued\""), "{row_l}\n{row_o}");

    {
        let mut spine = spine::spine_db::SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
        spine::recover::halt(&mut spine, "physics: the disk refused the append").expect("halt recorded");
    }
    let dead = |r: Result<String, SpineError>, what: &str| match r {
        Err(SpineError::Halted { .. }) => {}
        other => panic!("{what} under a halt must answer dead (Halted), got {other:?}"),
    };
    // Every act under the halt is BOUNDED (a door that hung
    // 120 s under a halt once left g_d5 running for over 60 seconds and
    // then PASSING): each runs on its own thread through
    // `answered_within_10s`, so a door that stops answering is RED at the
    // bound, never a slow green.
    let c_ = c.clone();
    dead(answered_within_10s("the tick under the halt", move || c_.tranche_tick(&QUIET, wall_ms()).map(|r| format!("{r:?}"))), "the tick");
    let c_ = c.clone();
    dead(
        answered_within_10s("TO's beat under the halt", move || c_.tranche_heartbeat(&actor("TO", KEY_T), &QUIET, wall_ms()).map(|r| format!("{r:?}"))),
        "TO's beat",
    );
    let c_ = c.clone();
    dead(answered_within_10s("TO's rollback under the halt", move || c_.tranche_rollback(&actor("TO", KEY_T), wall_ms()).map(|r| format!("{r:?}"))), "TO's rollback");
    let (c_, al_) = (c.clone(), al.clone());
    dead(answered_within_10s("TL's re-issued commit under the halt", move || c_.tranche_commit(&al_, wall_ms()).map(|r| format!("{r:?}"))), "TL's re-issued commit");
    // THE JOIN BEGUN BEFORE THE HALT answers dead, bounded (every act
    // answers; never block). Its longest poll is lock_poll_interval × 32; the bound
    // outlasts that by far, so a join that does not read the halt is RED
    // here at the bound (it would answer only after the repair).
    let join_bound = (cfg().lock_poll_interval * 32 * 30).max(Duration::from_secs(2));
    let started = Instant::now();
    while !joiner.is_finished() {
        assert!(
            started.elapsed() < join_bound,
            "a commit joined BEFORE the halt did not answer within {join_bound:?} under the halt (it must answer dead)"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    dead(joiner.join().expect("the joiner").map(|r| format!("{r:?}")), "TL's commit joined before the halt");
    assert_eq!(tranche_row(&dir, "TL"), row_l, "the landing row is byte-identical under the halt (no landing_error, no end)");
    assert_eq!(tranche_row(&dir, "TO"), row_o, "the queued row is byte-identical under the halt");
    assert!(reserved_rows(&dir, "TL").is_empty(), "no lander ran under the halt");

    spine_sql(&dir)
        .execute("UPDATE spine_state SET halted = 0, halted_reason = NULL WHERE singleton = 1", [])
        .expect("repair");
    drop(c);
    let (c, _, _) = reopen(&dir);
    let pass = c.tranches_at_open();
    assert_eq!(pass.tranches.iter().map(|t| (t.tranche.as_str(), t.state)).collect::<Vec<_>>(), vec![("TL", TrancheState::Landing), ("TO", TrancheState::Queued)], "{pass:?}");
    // OPPOSED: with NO halt, a joiner on the same `landing` record gets the
    // landed reply. Its join begins before the first tick (nothing lands at
    // open), so it is a join of a `landing` row, answered by the lander.
    let joiner = {
        let (c2, a2) = (c.clone(), al.clone());
        std::thread::spawn(move || c2.tranche_commit(&a2, wall_ms()))
    };
    std::thread::sleep(cfg().lock_poll_interval * 4);
    assert!(!joiner.is_finished(), "the no-halt joiner is joined on the landing record, not answered: nothing lands at open");
    // The landing resumes on the first tick after the restart.
    let first = tick(&c, wall_ms());
    assert_eq!(first.landers_spawned, vec![("TL".to_string(), 1)], "{first:?}");
    wait_landed(&c, &dir, "TL", "TL after the repair");
    let started = Instant::now();
    while !joiner.is_finished() {
        assert!(started.elapsed() < join_bound, "the no-halt joiner did not answer within {join_bound:?} of the landing");
        std::thread::sleep(Duration::from_millis(10));
    }
    let landed = joiner.join().expect("the no-halt joiner").expect("OPPOSED: the no-halt joiner gets the landed reply");
    assert_eq!(landed.state, TrancheState::Landed, "{landed:?}");
    assert_eq!(format!("{landed:?}"), format!("{:?}", c.tranche_commit(&al, wall_ms()).expect("re-issued")), "the joiner's reply is the record's");
    assert_eq!(reserved_rows(&dir, "TL").len(), 1);
    // The queued one opens on the next tick and continues.
    let _ = tick(&c, wall_ms());
    assert_eq!(state_of(&c, "TO"), TrancheState::Open);
    assert!(matches!(c.tranche_stage(&actor("TO", KEY_T), child("o0", WRITER, &[("t", json!("after repair"))]), wall_ms()), Ok(ChildOutcome::Staged { ordinal: 0, .. })));
}

// ---------------------------------------------------------------------------
// the seam vocabulary — five names, disjoint, refusing by name without the feature
// ---------------------------------------------------------------------------

/// The five `TrancheStage` names parse to themselves and to no other
/// vocabulary (the door's, the lander's, the gate's); `at_tranche_stage` is
/// silent with nothing armed. Then, out of process: under `fault-injection`
/// each name kills the victim's tranche job at its point (SIGABRT, the stage
/// named — `kill_at_with`'s three assertions); WITHOUT the feature the
/// production victim refuses each set name BY NAME before any effect
/// (`fault_seam_refused value=<name>`, exit 3) — one assertion per NEW
/// stage name.
#[test]
fn the_tranche_seams_speak_five_names_disjoint_from_the_other_three_vocabularies_and_the_victim_shows_which_build_carries_them() {
    let named = [
        (TrancheStage::AfterBegin, "after-begin", "queued"),
        (TrancheStage::AfterStage, "after-stage", "staging"),
        (TrancheStage::BetweenChildParts, "between-child-parts", "commit"),
        (TrancheStage::BeforeChildClear, "before-child-clear", "commit"),
        (TrancheStage::InRollback, "in-rollback", "rollback"),
    ];
    for (stage, name, _) in named {
        assert_eq!(stage.name(), name);
        assert_eq!(TrancheStage::parse(name), Some(stage));
        assert_eq!(spine::recover::Stage::parse(name), None, "{name} is not the door's");
        assert_eq!(spine::tranche::LanderStage::parse(name), None, "{name} is not the lander's");
        assert_eq!(spine::fault::GateSeam::parse(name), None, "{name} is not the gate's");
    }
    assert_eq!(TrancheStage::ALL.map(|s| s.name()), named.map(|(_, n, _)| n));
    assert!(spine::fault::at_tranche_stage(TrancheStage::AfterBegin).is_ok(), "both unset: silent");

    for (_, name, scenario) in named {
        let dir = root(&format!("seam-{name}"));
        if cfg!(feature = "fault-injection") {
            #[cfg(feature = "fault-injection")]
            {
                let out = kill(&dir, scenario, name, &[("SPINE_VICTIM_CHILDREN", "2")]);
                assert!(!kill::stdout_of(&out).contains("seam_no_such_stage"), "{name} parsed: {}", kill::stdout_of(&out));
            }
        } else {
            let out = kill::victim("tranche", &dir, Some(name))
                .env("SPINE_VICTIM_TRANCHE", scenario)
                .env("SPINE_VICTIM_CHILDREN", "2")
                .output()
                .expect("spawn the victim");
            kill::reject_a_stale_victim(&out);
            let stdout = kill::stdout_of(&out);
            assert_eq!(out.status.code(), Some(kill::EXIT_REFUSED), "{name}: the production tranche act refuses the set seam by name: {stdout} {}", kill::stderr_of(&out));
            assert!(stdout.contains("kind=fault_seam_refused") && stdout.contains(&format!("value={name}")), "{name}: {stdout}");
            // BEFORE ANY EFFECT: the refusal came from the gate at the act's
            // door, not after a row was written — the act the seam was aimed
            // at left no trace: no `T1` row (begin), one child not two
            // (stage), the state still `staging` with no reserved row
            // (commit), the state still `staging` (rollback).
            let sql = spine_sql(&dir);
            match scenario {
                "queued" => {
                    let n: i64 = sql.query_row("SELECT COUNT(*) FROM tranches WHERE id = 'T1'", [], |r| r.get(0)).unwrap();
                    assert_eq!(n, 0, "{name}: the refused begin wrote no tranche row");
                }
                "staging" => {
                    let staged: i64 = sql.query_row("SELECT staged FROM tranches WHERE id = 'T'", [], |r| r.get(0)).unwrap();
                    assert_eq!(staged, 1, "{name}: the refused stage staged nothing (the unarmed child 0 stands)");
                }
                "commit" | "rollback" => {
                    let state: String = sql.query_row("SELECT state FROM tranches WHERE id = 'T'", [], |r| r.get(0)).unwrap();
                    assert_eq!(state, "staging", "{name}: the refused {scenario} flipped nothing");
                    assert!(reserved_rows(&dir, "T").is_empty(), "{name}: no reserved row");
                }
                _ => unreachable!(),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// g_d6 — an un-granted claim's queued rows are swept, at N = 2 waiters
// ---------------------------------------------------------------------------

const GD6_DIR: &str = "SPINE_TEST_GD6_DIR";
const GD6_CTL: &str = "SPINE_TEST_GD6_CTL";
const GD6_WHO: &str = "SPINE_TEST_GD6_WHO";

/// The CHILD half of g_d6: a no-op unless g_d6 spawned this binary with the
/// environment set. Opens the store (a long ceiling, so it is still WAITING
/// when it is killed), says so, waits for `go`, then stages one child under
/// `TD` that SETs the doc named in `doc-<who>` — a claim the parent has
/// parked a granted foreign row in front of.
#[test]
fn g_d6_child_stages_behind_a_held_row() {
    let (Ok(dir), Ok(ctl), Ok(who)) = (std::env::var(GD6_DIR), std::env::var(GD6_CTL), std::env::var(GD6_WHO)) else {
        return;
    };
    let (dir, ctl) = (PathBuf::from(dir), PathBuf::from(ctl));
    let mut config = cfg();
    config.lock_wait_ceiling = Duration::from_secs(120);
    let c = Cauldron::open(&dir, config).expect("the child's open");
    std::fs::write(ctl.join(format!("opened-{who}")), b"1").unwrap();
    let started = Instant::now();
    while !ctl.join("go").exists() {
        assert!(started.elapsed() < Duration::from_secs(60), "never told to go");
        std::thread::sleep(Duration::from_millis(10));
    }
    let x = std::fs::read_to_string(ctl.join(format!("doc-{who}"))).unwrap();
    let r = c.tranche_stage(
        &actor("TD", KEY_T),
        child(&format!("d-{who}"), r#"MATCH (d: fixture-alpha/doc { id: $d }) SET d.title = "held" RETURN d.id"#, &[("d", json!(bare(&x)))]),
        wall_ms(),
    );
    println!("gd6-child-{who}: the stage answered {r:?}");
}

/// g_d6, both builds. Lost by
/// design: an un-granted claim's queued rows are swept; the child that was
/// waiting was never staged and is re-submitted. ONE HOLDER, TWO WAITERS:
/// tranche `TD` holds a GRANTED row on doc Y (its child y staged here);
/// two child PROCESSES each stage under `TD` against docs X and Z, behind
/// granted foreign rows F and G, so each enrols QUEUED rows under the
/// anchor and waits (ceiling 120 s). Both are SIGKILLed mid-wait — real
/// deaths, by the `Child` handles this test spawned. Then the healing open:
/// - every queued row under the anchor is gone, and the pass says how many;
/// - an ordinary writer on X and on Z commits (not `Busy` at the ceiling);
/// - OPPOSED: TD's granted row on Y stands, by key, and a writer on Y is
///   still `Busy` behind it; TD continues — x is re-submitted and lands.
/// RED: no withdrawal at open → the writers on X and Z are `Busy`. RED: a
/// withdrawal that also takes granted rows → TD's Y row is gone.
#[test]
fn g_d6_two_children_killed_mid_lock_wait_leave_no_queued_row_and_the_holders_granted_row_stays() {
    if std::env::var(GD6_DIR).is_ok() {
        return;
    }
    use spine::locks::{Claim, ClaimSet, LockMode, Resource};
    use spine::pool::NodeId;
    let dir = root("gd6");
    let _ = std::fs::remove_dir_all(&dir);
    let ctl = root("gd6-ctl");
    let _ = std::fs::remove_dir_all(&ctl);
    std::fs::create_dir_all(&ctl).unwrap();
    let c = Cauldron::create(&dir, &[spine::schema_def::fixtures::fixture_alpha()], cfg()).expect("create");
    let mk = |t: &str| match keyless(&c, &format!("gd6-{t}"), WRITER, &[("t", json!(t))]) {
        Ok(Outcome::Committed { returned, .. }) => returned.0[0]["d.id"].as_str().unwrap().to_string(),
        other => panic!("{other:?}"),
    };
    let (x, y, z) = (mk("X"), mk("Y"), mk("Z"));
    let set = r#"MATCH (d: fixture-alpha/doc { id: $d }) SET d.title = "W" RETURN d.id"#;
    let begun = c
        .tranche_begin(
            &spine::tranche::TrancheBegin {
                tranche: "TD".into(),
                key: KEY_T.into(),
                holder_service: SERVICE.into(),
                instrument: instrument(),
                lock_mode: TrancheLockMode::Touched,
                regions: vec![ALPHA.into()],
            },
            &QUIET,
            wall_ms(),
        )
        .expect("begin TD");
    assert_eq!(begun.state, TrancheState::Open);
    let td = actor("TD", KEY_T);
    assert!(matches!(c.tranche_stage(&td, child("y", set, &[("d", json!(bare(&y)))]), wall_ms()), Ok(ChildOutcome::Staged { .. })));
    let anchor = anchor_seq(&dir, "TD");
    let held = touched(&c, "TD");
    let granted_q = "SELECT COUNT(*) FROM locks WHERE seq = ?1 AND granted = 1";
    let queued_q = "SELECT COUNT(*) FROM locks WHERE seq = ?1 AND granted = 0";
    let count = |q: &str| -> i64 { spine_sql(&dir).query_row(q, [anchor], |r| r.get(0)).unwrap() };
    let granted_before = count(granted_q);
    assert!(granted_before > 0 && held.iter().any(|r| r.1.contains(&bare(&y))), "SENTINEL: TD holds a granted row on Y: {held:?}");
    drop(c);

    // The two waiters, as processes of this binary.
    let mut children: Vec<std::process::Child> = ["a", "b"]
        .iter()
        .map(|who| {
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tranche::crash::g_d6_child_stages_behind_a_held_row", "--nocapture", "--test-threads=1"])
                .env(GD6_DIR, &dir)
                .env(GD6_CTL, &ctl)
                .env(GD6_WHO, who)
                .env_remove("SPINE_FAULT")
                .env_remove("SPINE_PAUSE")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn a waiter");
            println!("g_d6: waiter {who} is pid {}", child.id());
            child
        })
        .collect();
    let started = Instant::now();
    while !(ctl.join("opened-a").exists() && ctl.join("opened-b").exists()) {
        for ch in children.iter_mut() {
            if let Ok(Some(st)) = ch.try_wait() {
                panic!("a waiter exited before it opened: {st:?}");
            }
        }
        assert!(started.elapsed() < Duration::from_secs(60), "the waiters never opened");
        std::thread::sleep(Duration::from_millis(10));
    }
    // The foreign holders, parked AFTER both opens (an open aborts them).
    let park = |tag: &str, node: &str| -> i64 {
        let mut spine = spine::spine_db::SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
        let env = Envelope {
            statement: format!("-- parked {tag}"),
            bindings: bindings(&[]),
            format_version: ENVELOPE_FORMAT_VERSION,
            provenance: Provenance {
                initiator: Initiator::CoreMechanical { operation: "test-outside".into() },
                instrument: instrument(),
            },
            mode: Mode::Commit,
            idempotency_nonce: Some(tag.to_string()),
        };
        let hash = env.hash_identity().unwrap();
        let seq = spine.append_pending(&env, &hash, &spine::delta::CompiledPayload { parts: vec![], minted: BTreeMap::new() }).unwrap();
        let mut set = ClaimSet::new();
        set.insert(Claim { pool: PoolName::new(ALPHA), resource: Resource::Body { node: NodeId(node.into()) }, mode: LockMode::Write });
        spine::locks::acquire(&spine, seq, &set, &cfg()).expect("granted");
        seq.0
    };
    let f = park("gd6-f", &x);
    let g = park("gd6-g", &z);
    std::fs::write(ctl.join("doc-a"), x.as_bytes()).unwrap();
    std::fs::write(ctl.join("doc-b"), z.as_bytes()).unwrap();
    std::fs::write(ctl.join("go"), b"1").unwrap();
    // Both waiters ENROLLED: a queued row under the anchor on X and on Z.
    let queued_on = |node: &str| -> i64 {
        spine_sql(&dir)
            .query_row(
                "SELECT COUNT(*) FROM locks WHERE seq = ?1 AND granted = 0 AND instr(resource, ?2) > 0",
                rusqlite::params![anchor, bare(node)],
                |r| r.get(0),
            )
            .unwrap()
    };
    let started = Instant::now();
    while queued_on(&x) == 0 || queued_on(&z) == 0 {
        assert!(started.elapsed() < Duration::from_secs(30), "SENTINEL: both waiters' claims never enrolled queued rows under the anchor");
        std::thread::sleep(Duration::from_millis(5));
    }
    let queued_before = count(queued_q);
    // The deaths: SIGKILL, by the handles this test spawned.
    for ch in children.iter_mut() {
        ch.kill().expect("SIGKILL our own waiter");
    }
    for ch in children {
        let out = ch.wait_with_output().expect("reap a waiter");
        let said: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().filter(|l| l.contains("gd6-child")).map(str::to_string).collect();
        assert!(said.is_empty() && !out.status.success(), "a waiter answered before its death — it was not waiting: {said:?} {:?}", out.status);
    }
    assert_eq!(count(queued_q), queued_before, "SENTINEL: the dead waiters' queued rows stand on disk before the open");

    let (c, _, _) = reopen(&dir);
    let pass = c.tranches_at_open().clone();
    assert_eq!((tx_flags(&dir, f), tx_flags(&dir, g)), ((false, true), (false, true)), "the parked F and G aborted by the ordinary pass");
    let write = |tag: &str, node: &str| {
        let t0 = Instant::now();
        keyless(&c, tag, set, &[("d", json!(bare(node)))])
            .map(|_| format!("committed in {:?}", t0.elapsed()))
            .map_err(|e| format!("{e:?} after {:?}", t0.elapsed()))
    };
    let (wx, wz) = (write("gd6-wx", &x), write("gd6-wz", &z));
    let (queued_after, granted_after, held_after) = (count(queued_q), count(granted_q), touched(&c, "TD"));
    println!(
        "g_d6: queued rows under TD's anchor: before the open {queued_before}, after {queued_after} (pass withdrew {}); granted {granted_before} -> {granted_after}; writer on X: {wx:?}; on Z: {wz:?}",
        pass.queued_rows_withdrawn
    );
    assert!(wx.is_ok() && wz.is_ok(), "the ordinary writers on X and Z behind the dead waiters' queued rows: X {wx:?}, Z {wz:?}");
    assert_eq!(queued_after, 0, "every un-granted row under the anchor is swept at open");
    assert_eq!(pass.queued_rows_withdrawn as i64, queued_before, "the pass reports what it withdrew: {pass:?}");
    assert_eq!((held_after, granted_after), (held.clone(), granted_before), "OPPOSED: the holder TD's granted rows stand, by key");
    let wy = write("gd6-wy", &y);
    assert!(matches!(wy, Err(ref e) if e.starts_with("Busy")), "OPPOSED: a writer on Y is still behind TD's granted row: {wy:?}");
    // TD continues: the dead waiter's child was never staged; re-submitted, it stages and lands.
    assert!(matches!(c.tranche_stage(&td, child("x-again", set, &[("d", json!(bare(&x)))]), wall_ms()), Ok(ChildOutcome::Staged { .. })));
    let landed = land(&c, "TD", KEY_T).expect("TD lands");
    assert_eq!(landed.children.len(), 2, "{landed:?}");
}

// ---------------------------------------------------------------------------
// g_s2h — two further guards on the halt and the sweep
// ---------------------------------------------------------------------------

const KEY_TQ: &str = "0303030303030303030303030303030303030303030303030303030303030303";

/// One door call on its own thread, answered within a BOUND or RED at it: a
/// door that stopped answering fails the test, it never hangs the suite. The
/// longest period on the path is the join's capped poll
/// (`lock_poll_interval` x 32 = 64 ms); 10 s outlasts it by two orders.
fn answered_within_10s<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let bound = Duration::from_secs(10);
    let h = std::thread::spawn(f);
    let started = Instant::now();
    while !h.is_finished() {
        assert!(started.elapsed() < bound, "{what} did not answer within {bound:?} (every act answers)");
        std::thread::sleep(Duration::from_millis(5));
    }
    h.join().unwrap_or_else(|_| panic!("{what} panicked"))
}

/// Guard 1: the
/// door check — `tranche_commit` reads the halt gate BEFORE `admit`. Now
/// the join's own halt gate answers dead for a `landing` or `landed`
/// commit, so g_d5 cannot see the door check any more; only a commit on a
/// QUEUED tranche reaches `legality_for_commit` without joining. Under a
/// halt it must answer dead, and the `tranches` row must
/// not move. OPPOSED twice: with no halt — before it and after the repair —
/// the same commit answers `TrancheBusy {why: "queued"}`, and a re-issued
/// commit on the landed tranche answers the landed reply byte-identically.
/// Every door call is bounded (`answered_within_10s`).
/// RED (measured): the door's `halt_gate(&tx)?` removed from
/// `tranche_commit` → the queued commit under the halt answers
/// `TrancheBusy {why: "queued"}`. Both builds; in process.
#[test]
fn g_s2h_1_a_commit_on_a_queued_tranche_under_a_halt_answers_dead_at_the_door_and_busy_queued_with_no_halt() {
    let dir = root("s2h1");
    let _ = std::fs::remove_dir_all(&dir);
    let c = Cauldron::create(&dir, &[spine::schema_def::fixtures::fixture_alpha()], cfg()).expect("create");
    let begin = |t: &str, k: &str| {
        c.tranche_begin(
            &spine::tranche::TrancheBegin {
                tranche: t.into(),
                key: k.into(),
                holder_service: SERVICE.into(),
                instrument: instrument(),
                lock_mode: TrancheLockMode::Touched,
                regions: vec![ALPHA.into()],
            },
            &QUIET,
            wall_ms(),
        )
        .unwrap_or_else(|e| panic!("begin {t}: {e:?}"))
    };
    let commit = |what: &str, a: &TrancheActor| {
        let (c2, a2) = (c.clone(), a.clone());
        answered_within_10s(what, move || c2.tranche_commit(&a2, wall_ms()).map(|r| format!("{r:?}")))
    };
    let busy_queued = |r: &Result<String, SpineError>| matches!(r, Err(SpineError::TrancheBusy { why, .. }) if why == "queued");
    // TL lands; TA is then the one non-queued tranche and TQ queues.
    assert_eq!(begin("TL", KEY_T0).state, TrancheState::Open);
    let al = actor("TL", KEY_T0);
    assert!(matches!(c.tranche_stage(&al, child("l0", WRITER, &[("t", json!("s2h1"))]), wall_ms()), Ok(ChildOutcome::Staged { .. })));
    let landed_reply = format!("{:?}", land(&c, "TL", KEY_T0).expect("TL lands"));
    assert!(landed_reply.contains("Landed"), "SENTINEL: TL landed: {landed_reply}");
    assert_eq!(begin("TA", KEY_T).state, TrancheState::Open);
    assert_eq!(begin("TQ", KEY_TQ).state, TrancheState::Queued);
    let aq = actor("TQ", KEY_TQ);
    // OPPOSED, before the halt: the queued commit is busy queued.
    let before = commit("TQ's commit before the halt", &aq);
    assert!(busy_queued(&before), "OPPOSED: with no halt a queued commit answers busy queued: {before:?}");
    let row_q = tranche_row(&dir, "TQ");

    {
        let mut spine = spine::spine_db::SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
        spine::recover::halt(&mut spine, "physics: the disk refused the append").expect("halt recorded");
    }
    let q = commit("TQ's commit under the halt", &aq);
    let l = commit("TL's re-issued commit under the halt", &al);
    assert!(matches!(q, Err(SpineError::Halted { .. })), "a commit on a QUEUED tranche under a halt must answer dead at the door, got {q:?}");
    assert!(matches!(l, Err(SpineError::Halted { .. })), "a commit on a LANDED tranche under a halt must answer dead, got {l:?}");
    assert_eq!(tranche_row(&dir, "TQ"), row_q, "the queued row is byte-identical under the halt");

    spine_sql(&dir)
        .execute("UPDATE spine_state SET halted = 0, halted_reason = NULL WHERE singleton = 1", [])
        .expect("repair");
    // OPPOSED, after the repair: the normal replies, the landed one byte-identical.
    let q2 = commit("TQ's commit after the repair", &aq);
    assert!(busy_queued(&q2), "OPPOSED: un-halted, a queued commit answers busy queued: {q2:?}");
    let l2 = commit("TL's re-issued commit after the repair", &al).expect("the landed reply");
    assert_eq!(l2, landed_reply, "after the repair the landed reply is byte-identical");
}

/// Guard 2, at N = 2 on
/// every side: the sweep at open (`withdraw_queued_at_open`) deletes ONLY
/// un-granted rows whose `seq` is a TRANCHE anchor. Two anchors (TD open with
/// a staged child, TQ queued) each carry one queued row; two ORDINARY
/// writers each carry one queued row; two ordinary holders each carry one
/// granted row. The ordinary rows carry seqs with no spine row, which the
/// ordinary pass cannot account for and leaves — the stand-in for a
/// concurrent enroller: a live ordinary door that enrolled its queued row
/// after the ordinary pass read its worklist (`Cauldron::open` is not
/// exclusive). After the open: both anchors' queued rows are
/// gone and the pass says it withdrew exactly 2; both ordinary writers'
/// queued rows and both granted rows stand, and TD's own granted rows stand.
/// RED (measured): the sweep widened to a global
/// `DELETE FROM locks WHERE granted = 0` → the ordinary writers' queued rows
/// go (and the pass reports 4). OPPOSED (measured): the sweep removed (0
/// rows) → the anchors' queued rows stay. Both builds; in process.
/// THE LIMIT: the stand-in rows carry
/// seqs with NO `transactions` row, and a real live waiter's row has a real,
/// un-cleared one. A sweep keyed on `transactions`
/// (`granted = 0 AND seq IN (SELECT seq FROM transactions)`) strips a real
/// waiter and leaves these phantoms, so THIS test stays green under it
/// (measured). That case is
/// `g_s2h_2b_a_live_ordinary_waiter_enrolled_between_the_two_passes_…`,
/// which enrols a real waiter between the two passes.
#[test]
fn g_s2h_2_the_open_withdraws_only_queued_rows_under_tranche_anchors_and_two_ordinary_writers_queued_rows_and_granted_rows_stand() {
    let dir = root("s2h2");
    let _ = std::fs::remove_dir_all(&dir);
    let c = Cauldron::create(&dir, &[spine::schema_def::fixtures::fixture_alpha()], cfg()).expect("create");
    let begin = |t: &str, k: &str| {
        c.tranche_begin(
            &spine::tranche::TrancheBegin {
                tranche: t.into(),
                key: k.into(),
                holder_service: SERVICE.into(),
                instrument: instrument(),
                lock_mode: TrancheLockMode::Touched,
                regions: vec![ALPHA.into()],
            },
            &QUIET,
            wall_ms(),
        )
        .unwrap_or_else(|e| panic!("begin {t}: {e:?}"))
    };
    assert_eq!(begin("TD", KEY_T).state, TrancheState::Open);
    let td = actor("TD", KEY_T);
    assert!(matches!(c.tranche_stage(&td, child("y", WRITER, &[("t", json!("s2h2"))]), wall_ms()), Ok(ChildOutcome::Staged { .. })));
    assert_eq!(begin("TQ", KEY_TQ).state, TrancheState::Queued);
    let (td_seq, tq_seq) = (anchor_seq(&dir, "TD"), anchor_seq(&dir, "TQ"));
    assert_ne!(td_seq, tq_seq, "SENTINEL: two distinct anchors");
    let held = touched(&c, "TD");
    assert!(!held.is_empty(), "SENTINEL: TD holds granted rows for the 'stand' check to mean something");
    drop(c);
    const WRITERS: [i64; 2] = [8_000_000, 8_000_002];
    const HOLDERS: [i64; 2] = [8_000_001, 8_000_003];
    {
        let conn = spine_sql(&dir);
        let ins = "INSERT INTO locks (pool, resource, mode, seq, granted) VALUES (?1, ?2, 'write', ?3, ?4)";
        for (seq, res, granted) in [
            (td_seq, "body:s2h2-td-q", 0),
            (tq_seq, "body:s2h2-tq-q", 0),
            (WRITERS[0], "body:s2h2-w0", 0),
            (WRITERS[1], "body:s2h2-w1", 0),
            (HOLDERS[0], "body:s2h2-h0", 1),
            (HOLDERS[1], "body:s2h2-h1", 1),
        ] {
            conn.execute(ins, rusqlite::params![ALPHA, res, seq, granted]).unwrap();
        }
    }
    let rows = |seq: i64, granted: i64| -> i64 {
        spine_sql(&dir)
            .query_row("SELECT COUNT(*) FROM locks WHERE seq = ?1 AND granted = ?2", [seq, granted], |r| r.get(0))
            .unwrap()
    };
    let census = || -> Vec<i64> {
        vec![rows(td_seq, 0), rows(tq_seq, 0), rows(WRITERS[0], 0), rows(WRITERS[1], 0), rows(HOLDERS[0], 1), rows(HOLDERS[1], 1)]
    };
    assert_eq!(census(), vec![1, 1, 1, 1, 1, 1], "SENTINEL: every row is on disk before the open");

    let (c, _, _) = reopen(&dir);
    let withdrawn = c.tranches_at_open().queued_rows_withdrawn;
    let after = census();
    println!("g_s2h_2: after the open [td_q, tq_q, w0_q, w1_q, h0_g, h1_g] = {after:?}; the pass withdrew {withdrawn}");
    assert_eq!(&after[..2], &[0, 0], "both tranche anchors' queued rows are withdrawn: {after:?}");
    assert_eq!(&after[2..4], &[1, 1], "both ordinary writers' queued rows stand (not the tranche pass's): {after:?}");
    assert_eq!(&after[4..], &[1, 1], "both ordinary granted rows stand: {after:?}");
    assert_eq!(withdrawn, 2, "the pass withdrew exactly the two anchor rows");
    assert_eq!(touched(&c, "TD"), held, "TD's granted rows stand");
}

/// g_s2h_2's limit closed with a REAL enroller.
/// The window `Cauldron::open` cannot exclude — a live ordinary door that
/// enrols its queued row after the ordinary pass read its worklist and
/// before the tranche pass sweeps — is HELD in one process by the
/// test-support hook: `open_unrecovered`, the ordinary pass by hand
/// (`recover`), the live writer, then `tranche_pass_by_hand`, the second
/// half of `open` verbatim. Tranche `TD` (`touched`) holds a granted write
/// on doc X (its child staged) and carries one queued row of its own; an
/// ordinary writer W SETs X and waits behind TD (ceiling 60 s), its queued
/// row under its OWN real, un-cleared `transactions` seq (asserted — the
/// thing g_s2h_2's phantom rows lack). After the tranche pass: TD's queued
/// row is withdrawn (the pass says 1), W's queued row STANDS and W is still
/// waiting. OPPOSED: TD rolls back and W lands (`Committed`), bounded.
/// RED (measured): the sweep keyed on `transactions`
/// (`granted = 0 AND seq IN (SELECT seq FROM transactions)`) → W's
/// queued row is gone. RED: no sweep → TD's queued row stays. Both builds.
#[test]
fn g_s2h_2b_a_live_ordinary_waiter_enrolled_between_the_two_passes_keeps_its_queued_row_through_the_tranche_pass() {
    use rusqlite::OptionalExtension;
    let dir = root("s2h2b");
    let _ = std::fs::remove_dir_all(&dir);
    let c = Cauldron::create(&dir, &[spine::schema_def::fixtures::fixture_alpha()], cfg()).expect("create");
    let x = match keyless(&c, "s2h2b-x", WRITER, &[("t", json!("x"))]) {
        Ok(Outcome::Committed { .. }) => committed(&c, "MATCH (d: fixture-alpha/doc) RETURN d.id", &[])[0]["d.id"].as_str().unwrap().to_string(),
        other => panic!("doc X: {other:?}"),
    };
    let td = actor("TD", KEY_T);
    let begun = c
        .tranche_begin(
            &spine::tranche::TrancheBegin {
                tranche: "TD".into(),
                key: KEY_T.into(),
                holder_service: SERVICE.into(),
                instrument: instrument(),
                lock_mode: TrancheLockMode::Touched,
                regions: vec![ALPHA.into()],
            },
            &QUIET,
            wall_ms(),
        )
        .expect("begin TD");
    assert_eq!(begun.state, TrancheState::Open);
    let set_x = r#"MATCH (d: fixture-alpha/doc { id: $d }) SET d.title = $t RETURN d.id"#;
    assert!(matches!(
        c.tranche_stage(&td, child("x0", set_x, &[("d", json!(bare(&x))), ("t", json!("by TD"))]), wall_ms()),
        Ok(ChildOutcome::Staged { .. })
    ));
    let td_seq = anchor_seq(&dir, "TD");
    let body_x = format!("body:{}", bare(&x));
    let granted_x = |seq: i64, granted: i64| -> i64 {
        spine_sql(&dir)
            .query_row("SELECT COUNT(*) FROM locks WHERE resource = ?1 AND seq = ?2 AND granted = ?3 AND mode = 'write'", rusqlite::params![body_x, seq, granted], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(granted_x(td_seq, 1), 1, "SENTINEL: TD holds a granted row on X for W to queue behind");
    spine_sql(&dir)
        .execute("INSERT INTO locks (pool, resource, mode, seq, granted) VALUES (?1, 'body:s2h2b-td-q', 'write', ?2, 0)", rusqlite::params![ALPHA, td_seq])
        .unwrap();
    drop(c);

    let mut config = cfg();
    config.lock_wait_ceiling = Duration::from_secs(60);
    let c = Cauldron::open_unrecovered(&dir, config).expect("open unrecovered");
    c.recover().expect("the ordinary pass, by hand");
    // THE LIVE ENROLLER, between the two passes.
    let w = {
        let (c2, x2) = (c.clone(), x.clone());
        std::thread::spawn(move || keyless(&c2, "s2h2b-w", set_x, &[("d", json!(bare(&x2))), ("t", json!("by W"))]))
    };
    let started = Instant::now();
    let w_seq: i64 = loop {
        let q: Option<i64> = spine_sql(&dir)
            .query_row("SELECT seq FROM locks WHERE resource = ?1 AND granted = 0 AND seq != ?2", rusqlite::params![body_x, td_seq], |r| r.get(0))
            .optional()
            .unwrap();
        if let Some(s) = q {
            break s;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "SENTINEL: W never enrolled a queued row on X");
        std::thread::sleep(Duration::from_millis(5));
    };
    let (w_decided, w_cleared) = tx_flags(&dir, w_seq);
    assert!(!w_decided && !w_cleared, "SENTINEL: W's queued row is under a REAL, un-cleared transactions row (seq {w_seq})");
    assert_eq!(granted_x(w_seq, 0), 1);

    let pass = c.tranche_pass_by_hand().expect("the tranche pass, by hand");
    let td_queued: i64 = spine_sql(&dir).query_row("SELECT COUNT(*) FROM locks WHERE seq = ?1 AND granted = 0", [td_seq], |r| r.get(0)).unwrap();
    println!("g_s2h_2b: withdrew {}; TD queued {td_queued}; W's queued rows on X {}", pass.queued_rows_withdrawn, granted_x(w_seq, 0));
    assert_eq!(td_queued, 0, "TD's own queued row is withdrawn");
    assert_eq!(granted_x(w_seq, 0), 1, "the LIVE waiter's queued row stands through the tranche pass");
    assert_eq!(pass.queued_rows_withdrawn, 1, "the pass withdrew exactly TD's row");
    assert_eq!(granted_x(td_seq, 1), 1, "TD's granted row stands");
    assert!(!w.is_finished(), "W is still waiting behind TD");

    // OPPOSED: the waiter is live — TD rolls back and W lands.
    c.tranche_rollback(&td, wall_ms()).expect("TD rolls back");
    let started = Instant::now();
    while !w.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(10), "W was not granted within 10 s of TD's rollback");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(matches!(w.join().expect("W"), Ok(Outcome::Committed { .. })), "W lands once TD is gone");
}

// ---------------------------------------------------------------------------
// g_j — the open beside a live stager, and the
// heartbeat clock across a halt
// ---------------------------------------------------------------------------

fn begin_touched(c: &Cauldron, t: &str, k: &str, now: i64) -> spine::tranche::TrancheBegun {
    c.tranche_begin(
        &spine::tranche::TrancheBegin {
            tranche: t.into(),
            key: k.into(),
            holder_service: SERVICE.into(),
            instrument: instrument(),
            lock_mode: TrancheLockMode::Touched,
            regions: vec![ALPHA.into()],
        },
        &QUIET,
        now,
    )
    .unwrap_or_else(|e| panic!("begin {t}: {e:?}"))
}

fn staged_ok(c: &Cauldron, t: &str, k: &str, step: &str, now: i64) {
    match c.tranche_stage(&actor(t, k), child(step, WRITER, &[("t", json!(step))]), now) {
        Ok(ChildOutcome::Staged { .. }) => {}
        other => panic!("stage {step} under {t}: {other:?}"),
    }
}

/// **g_j2 — AN OPEN BESIDE A LIVE STAGER IS NEVER REFUSED WHILE THE RECORD
/// IS CONSISTENT**. TD is
/// staging; a thread stages children back to back; this thread opens the
/// cauldron again and again beside it. Every open is OK, and the record
/// after is consistent (`staged` = the children rows). The opens are
/// COUNTED, not timed: the loop runs until `OPENS` opens have
/// answered, under a generous bound that FAILS by name rather than ending
/// the loop early — under machine load an 8 s window held only 9-10 opens,
/// so a time-boxed window measured the scheduler, not the
/// spine. Sentinels: the stager staged children DURING the opens (its count
/// moved between the first and the last open), and all `OPENS` opens ran
/// while the stager was still live (it never reached its own cap).
/// RED (measured): `rebuild_views_at_open` reading `staged` before
/// the mutex and the children after it (two snapshots, the earlier
/// shape) → opens refused "the record is torn".
/// The opposing case is `g_j2b`: a genuinely torn record is still refused.
#[test]
fn g_j2_an_open_beside_a_live_stager_is_never_refused_while_the_record_is_consistent() {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;
    let dir = root("gj2");
    let _ = std::fs::remove_dir_all(&dir);
    let c = Cauldron::create(&dir, &[spine::schema_def::fixtures::fixture_alpha()], cfg()).expect("create");
    assert_eq!(begin_touched(&c, "TD", KEY_T, wall_ms()).state, TrancheState::Open);
    staged_ok(&c, "TD", KEY_T, "s-init", wall_ms());
    // The stager's own cap is a runaway guard only, far above what the
    // opens need (40 opens moved it by 11-87 children in every logged run):
    // reaching it ends the opens, and the SENTINEL below then fails by name.
    const STAGER_CAP: u32 = 4_000;
    let stop = Arc::new(AtomicBool::new(false));
    let count = Arc::new(AtomicU32::new(0));
    let stager = {
        let (c2, stop2, count2) = (c.clone(), stop.clone(), count.clone());
        std::thread::spawn(move || {
            let mut n = 0u32;
            while !stop2.load(Ordering::Relaxed) && n < STAGER_CAP {
                staged_ok(&c2, "TD", KEY_T, &format!("s{n}"), wall_ms());
                n += 1;
                count2.store(n, Ordering::Relaxed);
            }
        })
    };
    let started = Instant::now();
    while count.load(Ordering::Relaxed) < 2 {
        assert!(started.elapsed() < Duration::from_secs(10), "the stager never started");
        std::thread::sleep(Duration::from_millis(1));
    }
    const OPENS: u32 = 40;
    const BOUND: Duration = Duration::from_secs(120);
    let (mut ok, mut refused) = (0u32, Vec::<String>::new());
    let first_seen = count.load(Ordering::Relaxed);
    let mut last_seen = first_seen;
    let started = Instant::now();
    while ok + (refused.len() as u32) < OPENS {
        match Cauldron::open(&dir, cfg()) {
            Ok(_) => ok += 1,
            Err(e) => refused.push(e.message()),
        }
        last_seen = count.load(Ordering::Relaxed);
        if last_seen >= STAGER_CAP {
            break;
        }
        assert!(
            started.elapsed() < BOUND,
            "BOUND: only {} of {OPENS} opens answered in {BOUND:?} (OK {ok}, refused {})",
            ok + refused.len() as u32,
            refused.len()
        );
    }
    stop.store(true, Ordering::Relaxed);
    stager.join().expect("the stager");
    let (state, _, _, _, _, staged) = tranche_cols(&dir, "TD");
    let rows: i64 = spine_sql(&dir)
        .query_row("SELECT COUNT(*) FROM tranche_children WHERE tranche_id = 'TD'", [], |r| r.get(0))
        .unwrap();
    println!(
        "g_j2: opens beside a live stager: OK {ok}, refused {}; the stager moved {first_seen} -> {last_seen} during them; record after: {state}, staged {staged}, rows {rows}",
        refused.len()
    );
    assert!(last_seen > first_seen, "SENTINEL: children were staged while the opens ran ({first_seen} -> {last_seen})");
    assert_eq!(ok + refused.len() as u32, OPENS, "SENTINEL: all {OPENS} opens ran beside a live stager (it stopped at {last_seen})");
    assert_eq!((state.as_str(), staged as i64), ("staging", rows), "the record is consistent");
    assert!(refused.is_empty(), "{} open(s) beside a live stager refused while the record is consistent; first: {}", refused.len(), refused[0]);
}

/// **g_j2b — A GENUINELY TORN RECORD IS STILL REFUSED BY NAME** (the
/// opposing case of g_j2). TT staging with two children; the same open
/// succeeds first (sentinel). Three tears, each on a fresh copy of the
/// shape: a child row missing (`staged` 2, rows 1), a gap at ordinal 0, and
/// `staged` raised past the rows — each open refused, naming "the record is
/// torn".
/// RED: `rebuild_in_one_snapshot` skipping the `staged` compare → the first
/// and third tears open OK.
#[test]
fn g_j2b_a_genuinely_torn_record_is_still_refused_by_name_at_open() {
    let tears: [(&str, &str); 3] = [
        ("row-missing", "DELETE FROM tranche_children WHERE tranche_id = 'TT' AND ordinal = 1"),
        ("gap-at-0", "DELETE FROM tranche_children WHERE tranche_id = 'TT' AND ordinal = 0"),
        ("staged-raised", "UPDATE tranches SET staged = 3 WHERE id = 'TT'"),
    ];
    for (name, tear) in tears {
        let dir = root(&format!("gj2b-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        let c = Cauldron::create(&dir, &[spine::schema_def::fixtures::fixture_alpha()], cfg()).expect("create");
        begin_touched(&c, "TT", KEY_T, wall_ms());
        staged_ok(&c, "TT", KEY_T, "a", wall_ms());
        staged_ok(&c, "TT", KEY_T, "b", wall_ms());
        drop(c);
        drop(Cauldron::open(&dir, cfg()).expect("SENTINEL: the untorn record opens"));
        assert_eq!(spine_sql(&dir).execute(tear, []).unwrap(), 1, "{name}: the tear wrote one row");
        let m = match Cauldron::open(&dir, cfg()) {
            Ok(_) => panic!("{name}: a torn record opened"),
            Err(e) => e.message(),
        };
        println!("g_j2b {name}: {m}");
        assert!(m.contains("the record is torn") && m.contains("`TT`"), "{name}: refused by name: {m}");
    }
}

fn halt_now(dir: &Path, why: &str) {
    let mut spine = spine::spine_db::SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    spine::recover::halt(&mut spine, why).expect("halt recorded");
}

fn unhalt(dir: &Path) {
    spine_sql(dir)
        .execute("UPDATE spine_state SET halted = 0, halted_reason = NULL WHERE singleton = 1", [])
        .expect("repair");
}

fn reason_of(dir: &Path, t: &str) -> Option<String> {
    spine_sql(dir)
        .query_row("SELECT reason FROM tranches WHERE id = ?1 AND attempt = 1", [t], |r| r.get(0))
        .unwrap()
}

/// **g_j3 — A HALT LONGER THAN THE TIMEOUT ENDS NO TRANCHE AFTER THE REPAIR,
/// WITH AND WITHOUT A RESTART UNDER IT; A HOLDER THAT THEN STOPS BEATING IS
/// ABANDONED ON SCHEDULE**. Timeout 300 ms; every clock is
/// passed explicitly and derives from the re-armed deadline, never from the
/// wall clock after the setup (the landable arms once staged and
/// landed on `wall_ms()`; the stage's beat SETS `deadline = wall + 300`, so
/// a 460-516 ms scheduler stall before the lander's first tick abandoned TS
/// by the configured timeout).
/// - arm `ticked`: the halt is seen by the scheduler's ticks (each `dead`,
///   and no tranche row written under them); the repair; the first tick at
///   `begin + 2000` (deadline lapsed by 1700 ms) abandons nothing, re-arms
///   TS's deadline to `now + timeout`, and TS is `staging` and LANDS: it
///   stages at `r + 1` and commits and ticks at `r + 2`, each inside the
///   deadline its own beat set (`r + 1 + timeout`).
/// - arm `restarted`: the daemon restarts UNDER the halt (the open's reset,
///   `boot + timeout`) and the halt is seen only by the holder's refused
///   beats (no tick under it); the first tick at
///   `boot + 2000` after the repair abandons nothing, and TS LANDS.
/// - OPPOSED, arm `stops`: after the repair and the re-arming tick at `r`,
///   the holder never beats: a tick at `r + timeout - 1` keeps TS (and
///   re-arms nothing — the re-arm is once per halt, never every tick); a
///   tick at `r + timeout + 1` abandons it, `heartbeat_lost`.
/// RED (measured): the re-arm removed → `ticked` and `restarted`
/// abandon TS at the first tick after the repair; the re-arm on EVERY tick
/// (a constant implementation) → `stops` never abandons; the halt gate not
/// noting the halt → `restarted` abandons.
/// The clause relied on: across a daemon restart, the restart is not
/// the holder's fault — applied to the repair.
#[test]
fn g_j3_a_halt_longer_than_the_timeout_ends_no_tranche_after_repair_and_a_silent_holder_is_abandoned_on_schedule() {
    let mut short = cfg();
    short.tranche.heartbeat_interval = Duration::from_millis(100);
    short.tranche.timeout = Duration::from_millis(300);
    let timeout = 300i64;
    let a = actor("TS", KEY_T0);
    let setup = |name: &str| -> (PathBuf, Cauldron, i64) {
        let dir = root(&format!("gj3-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        let c = Cauldron::create(&dir, &[spine::schema_def::fixtures::fixture_alpha()], short.clone()).expect("create");
        let b = wall_ms();
        assert_eq!(begin_touched(&c, "TS", KEY_T0, b).state, TrancheState::Open);
        staged_ok(&c, "TS", KEY_T0, "s0", b);
        (dir, c, b)
    };
    let dead = |r: spine::error::Result<TickReport>, what: &str| match r {
        Err(SpineError::Halted { .. }) => {}
        other => panic!("{what} under the halt must answer dead, got {other:?}"),
    };

    for arm in ["ticked", "restarted", "stops"] {
        let (dir, mut c, b) = setup(arm);
        halt_now(&dir, "gj3: a halt that outlasts the tranche timeout");
        let row_before = tranche_row(&dir, "TS");
        let mut clock = b;
        if arm == "restarted" {
            drop(c);
            c = Cauldron::open(&dir, short.clone()).expect("the daemon restarts under the halt");
            assert_eq!(c.tranches_at_open().deadlines_reset, 1, "SENTINEL: the open under the halt reset TS");
            clock = c.tranches_at_open().boot_ms;
            for i in 1..=3 {
                match c.tranche_heartbeat(&a, &QUIET, clock + 100 * i) {
                    Err(SpineError::Halted { .. }) => {}
                    other => panic!("a beat under the halt: {other:?}"),
                }
            }
        } else {
            for dt in [100, 500, 1_000, 2_000] {
                dead(c.tranche_tick(&QUIET, b + dt), "a tick");
            }
            assert_eq!(tranche_row(&dir, "TS"), row_before, "A HALT WRITES NO TRANCHE ROW, ticks and all");
        }
        let (_, _, _, deadline, _, _) = tranche_cols(&dir, "TS");
        unhalt(&dir);
        let r = clock + 2_000;
        assert!(r > deadline + 1_000, "SENTINEL: the halt outlasted the deadline ({deadline}) by over a second at {r}");
        let t = c.tranche_tick(&QUIET, r).expect("the first tick after the repair");
        let (state, _, _, rearmed_to, _, _) = tranche_cols(&dir, "TS");
        println!("g_j3 {arm}: deadline {deadline}, first tick after repair at {r}: abandoned {:?}, re-armed {} -> deadline {rearmed_to}; TS {state}", t.abandoned, t.deadlines_rearmed);
        assert!(t.abandoned.is_empty(), "{arm}: the first tick after the repair abandoned {:?}", t.abandoned);
        assert_eq!((state.as_str(), t.deadlines_rearmed, rearmed_to), ("staging", 1, r + timeout), "{arm}: TS staging, deadline re-armed to now + timeout");
        if arm == "stops" {
            let quiet = c.tranche_tick(&QUIET, r + timeout - 1).unwrap();
            assert!(quiet.abandoned.is_empty(), "stops: abandoned before its re-armed deadline: {quiet:?}");
            assert_eq!(quiet.deadlines_rearmed, 0, "stops: the re-arm is once per halt, not every tick");
            let late = c.tranche_tick(&QUIET, r + timeout + 1).unwrap();
            assert_eq!(late.abandoned, vec![("TS".to_string(), 1)], "stops: a silent holder is abandoned on schedule");
            assert_eq!(reason_of(&dir, "TS").as_deref(), Some("heartbeat_lost"));
        } else {
            // Landable: the holder continues, as after any restart — on the
            // logical clock the re-arm set, so a scheduler stall between the
            // stage's beat and the lander's first tick cannot age the
            // deadline (the beat SETS `deadline = now + timeout`).
            staged_ok(&c, "TS", KEY_T0, "s1", r + 1);
            // The beat is stamped `r + 1` plus the stage's own lock wait (a
            // beat during the wait is `now + elapsed`, at most the ceiling);
            // a wall-clock beat would sit ~1.7 s below `r` (r = clock + 2000).
            let beat_deadline = tranche_cols(&dir, "TS").3;
            assert!(
                (r + 1 + timeout..=r + 1 + timeout + CEILING_MS as i64).contains(&beat_deadline),
                "{arm}: the stage's beat set the deadline from its own clock: {beat_deadline}, r {r}"
            );
            let landed = land_at(&c, "TS", KEY_T0, r + 2).expect("lands after the repair");
            assert_eq!((landed.state, landed.children.len()), (TrancheState::Landed, 2), "{arm}");
        }
    }
}
