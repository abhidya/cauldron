//! THE TRANCHE LOCK —
//! tests 6, 7, 13, 14, 15, 16, 19, 23 and the guards g1–g16, each at
//! N ≥ 2 (a lock is proven at N ≥ 2 or not proven).
//! Every guard names, in its doc, THE OBSERVATION THAT PROVES THE OVERLAP —
//! a queued lock row seen while the other actor holds, a `.reached` marker, a
//! thread blocked on the mutex behind a transaction the test holds — never
//! ordering alone.
//!
//! # Two actors, two processes
//!
//! The per-tranche mutex is process-wide (`tranche.rs`), so some crossings
//! the tests ask for cannot be reached by two threads of one process: a second
//! claim call under one key waits on the mutex (test 15's rule-1 red), the
//! tick's abandon takes the mutex with a non-blocking try and skips a
//! tranche whose child is mid-wait (test 16), two landers never run at once
//! (the double-decision guard). Those actors run in a CHILD PROCESS: this
//! test binary re-executes itself filtered to [`locks_role_child`] (the
//! `door_smoke.rs` kill-child shape), which reads its role off the
//! environment and prints `role: …` lines the parent asserts on. The child
//! opens the cauldron UNRECOVERED (`Cauldron::open_unrecovered`): the
//! ordinary recovery pass would abort the parent's parked writers.
//!
//! # Making a transaction wait where the test needs it
//!
//! [`SpineHold`] is a `BEGIN IMMEDIATE` on `spine.db` from a side connection:
//! every spine WRITE then blocks in SQLite's busy handler (reads pass under
//! WAL) until the hold is released, and the thread that holds the mutex
//! while blocked holds it for as long as the test wants. That is how the
//! crossed flips (commit × rollback, commit × abandon, a child behind
//! `commit`'s flip, two landers at the decision) are ordered
//! DETERMINISTICALLY rather than raced for.

use serde_json::{json, Value};
use spine::config::SpineConfig;
use spine::delta::CompiledPayload;
use spine::envelope::{
    Bindings, Envelope, Initiator, Instrument, Mode, Provenance, RippleKind, TxHash,
    ENVELOPE_FORMAT_VERSION,
};
use spine::error::SpineError;
use spine::locks::{self, Claim, ClaimSet, LockMode, Resource};
use spine::pool::NodeId;
use spine::schema_def::{fixtures, PoolName};
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use spine::tranche::{
    ChildOutcome, QuietLevels, TickReport, TrancheActor, TrancheBegin, TrancheBegun,
    TrancheCommitted, TrancheLockMode, TrancheState,
};
use spine::witness::Witness;
use spine::{Cauldron, Outcome, ReadRequest};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const QUIET: QuietLevels = QuietLevels::BusNotConfigured;
const NOT_QUIET: QuietLevels = QuietLevels::Levels {
    trigger_host_pending: 1,
    runner_pending: 0,
};
const SERVICE: &str = "runner";
const ALPHA: &str = "fixture-alpha";
const BETA: &str = "fixture-beta";

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-locks-{name}-{}", std::process::id()))
}

/// The lock tests' config: a 400 ms lock ceiling (so a `busy` at the ceiling
/// and a `switch` refusal arrive in under a second), a 2 ms poll, a 5 s
/// per-statement busy timeout (so a [`SpineHold`] of a few hundred ms is
/// waited out, never answered `Busy`).
fn cfg() -> SpineConfig {
    let mut c = SpineConfig::default();
    c.lock_wait_ceiling = Duration::from_millis(400);
    c.lock_poll_interval = Duration::from_millis(2);
    c.busy_timeout = Duration::from_secs(5);
    c
}

fn fresh_with(dir: &Path, config: SpineConfig) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(dir, &[fixtures::fixture_alpha(), fixtures::fixture_beta()], config)
        .expect("create the cauldron")
}

fn fresh(dir: &Path) -> Cauldron {
    fresh_with(dir, cfg())
}

/// Fixture ALPHA with `doc` minting SERIAL ids (`id_prefix`) — the
/// one shape an `IMPORT` may carry an id into (`caller_id_against_prefix`:
/// a type with no prefix is `machine_owned_field`). Test 14(c)'s cauldron.
fn fresh_prefixed(dir: &Path) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    let mut alpha = fixtures::fixture_alpha();
    alpha.node_types.get_mut("doc").expect("doc").id_prefix = Some("doc".into());
    Cauldron::create(dir, &[alpha, fixtures::fixture_beta()], cfg()).expect("create the cauldron")
}

/// A key: 32 bytes as 64 lowercase hex, distinct per `n`.
fn key(n: u8) -> String {
    format!("{:02x}", n).repeat(32)
}

fn instrument() -> Instrument {
    Instrument {
        name: "runner".into(),
        version: "test-build".into(),
    }
}

fn begin_req(t: &str, k: &str, mode: TrancheLockMode) -> TrancheBegin {
    TrancheBegin {
        tranche: t.into(),
        key: k.into(),
        holder_service: SERVICE.into(),
        instrument: instrument(),
        lock_mode: mode,
        regions: vec![ALPHA.into()],
    }
}

fn begin_as(c: &Cauldron, t: &str, k: &str, mode: TrancheLockMode, quiet: &QuietLevels, now: i64) -> TrancheBegun {
    c.tranche_begin(&begin_req(t, k, mode), quiet, now)
        .unwrap_or_else(|e| panic!("begin {t} {mode:?}: {e:?}"))
}

fn begin_whole(c: &Cauldron, t: &str, k: &str, now: i64) -> TrancheBegun {
    begin_as(c, t, k, TrancheLockMode::Whole, &QUIET, now)
}

fn begin_touched(c: &Cauldron, t: &str, k: &str, now: i64) -> TrancheBegun {
    begin_as(c, t, k, TrancheLockMode::Touched, &QUIET, now)
}

fn actor(t: &str, k: &str) -> TrancheActor {
    TrancheActor {
        tranche: t.into(),
        key: k.into(),
        holder_service: SERVICE.into(),
    }
}

fn bindings(params: &[(&str, Value)]) -> Bindings {
    params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

/// A child's envelope: a type-one step writing as itself, nonce = step.
fn one(step: &str, statement: &str, params: &[(&str, Value)]) -> Envelope {
    Envelope {
        statement: statement.to_string(),
        bindings: bindings(params),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Ripple {
                kind: RippleKind::TypeOne,
                run_id: "run".into(),
                step_id: step.into(),
                scope: vec![],
            },
            instrument: instrument(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(step.to_string()),
    }
}

/// An OUTSIDE writer's envelope — the harness's own identity, an ordinary
/// commit through `Cauldron::submit`.
fn outside(nonce: &str, statement: &str, params: &[(&str, Value)], mode: Mode) -> Envelope {
    Envelope {
        statement: statement.to_string(),
        bindings: bindings(params),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::CoreMechanical {
                operation: "test-outside".into(),
            },
            instrument: instrument(),
        },
        mode,
        idempotency_nonce: Some(nonce.to_string()),
    }
}

/// An AGENT's envelope (the only initiator the import gate admits).
fn agent(nonce: &str, statement: &str, params: &[(&str, Value)]) -> Envelope {
    let mut e = outside(nonce, statement, params, Mode::Commit);
    e.provenance.initiator = Initiator::Agent {
        thread: "locks".into(),
        dispatch: nonce.into(),
    };
    e
}

fn keyless(c: &Cauldron, nonce: &str, statement: &str, params: &[(&str, Value)]) -> spine::error::Result<Outcome> {
    c.submit(outside(nonce, statement, params, Mode::Commit))
}

fn seed(c: &Cauldron, statement: &str, params: &[(&str, Value)]) -> Outcome {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    keyless(c, &format!("seed-{n}-{}", std::process::id()), statement, params)
        .unwrap_or_else(|e| panic!("seed `{statement}`: {e:?}"))
}

fn returned_id(o: Outcome, col: &str) -> String {
    match o {
        Outcome::Committed { returned, .. } => returned.0[0][col].as_str().unwrap().to_string(),
        other => panic!("{other:?}"),
    }
}

fn doc(c: &Cauldron, title: &str, state: &str) -> String {
    returned_id(
        seed(c, &format!(r#"CREATE (d: fixture-alpha/doc {{ title: "{title}", state: "{state}" }}) RETURN d.id"#), &[]),
        "d.id",
    )
}

fn ticket(c: &Cauldron) -> String {
    returned_id(seed(c, r#"CREATE (t: fixture-beta/ticket { state: "open" }) RETURN t.id"#, &[]), "t.id")
}

const SET_TITLE: &str = r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t"#;
const READ_TITLE: &str = r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.title"#;

fn set_title(c: &Cauldron, nonce: &str, id: &str, t: &str) -> spine::error::Result<Outcome> {
    keyless(c, nonce, SET_TITLE, &[("id", json!(id)), ("t", json!(t))])
}

fn req(statement: &str, params: &[(&str, Value)]) -> ReadRequest {
    ReadRequest {
        statement: statement.to_string(),
        bindings: bindings(params),
        format_version: ENVELOPE_FORMAT_VERSION,
    }
}

fn title_of(c: &Cauldron, id: &str) -> Value {
    let out = c.read(&req(READ_TITLE, &[("id", json!(id))])).expect("read");
    out.rows.0.first().map(|r| r["d.title"].clone()).unwrap_or(Value::Null)
}

fn spine_sql(dir: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("open spine.db");
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn
}

fn count(dir: &Path, sql: &str, params: &[&dyn rusqlite::ToSql]) -> i64 {
    spine_sql(dir).query_row(sql, params, |r| r.get(0)).unwrap()
}

fn rows_by_hash(dir: &Path, hash: &TxHash) -> i64 {
    count(dir, "SELECT COUNT(*) FROM transactions WHERE hash = ?1", &[&hash.to_hex()])
}

fn lock_rows_under(dir: &Path, seq: Seq) -> i64 {
    count(dir, "SELECT COUNT(*) FROM locks WHERE seq = ?1", &[&seq.0])
}

fn live_non_queued(dir: &Path) -> i64 {
    count(dir, "SELECT COUNT(*) FROM tranches WHERE state IN ('open','staging','landing')", &[])
}

fn anchor_seq(c: &Cauldron, t: &str) -> Seq {
    c.tranche_anchor(t, 1).unwrap().unwrap().seq
}

fn state_of(c: &Cauldron, t: &str) -> TrancheState {
    c.tranche_store(t, None).expect("store").state
}

fn mode_of(c: &Cauldron, t: &str) -> TrancheLockMode {
    c.tranche_store(t, None).expect("store").lock_mode
}

fn body(id: &str) -> Resource {
    Resource::Body {
        node: NodeId(id.to_string()),
    }
}

fn claim(pool: &str, resource: Resource, mode: LockMode) -> ClaimSet {
    let mut set = ClaimSet::new();
    set.insert(Claim {
        pool: PoolName::new(pool),
        resource,
        mode,
    });
    set
}

fn empty_payload() -> CompiledPayload {
    CompiledPayload {
        parts: Vec::new(),
        minted: BTreeMap::new(),
    }
}

/// PARK a foreign holder: a real pending row under a real `seq`, with a real
/// grant — a contended state CONSTRUCTED rather than raced for
/// (`lock_behaviour.rs`'s shape). A parked `write` is exactly "a
/// writer that holds a granted write claim when the gate rises is landing".
fn park(dir: &Path, held: &ClaimSet, tag: &str) -> Seq {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let env = outside(&format!("park-{tag}"), &format!("-- parking holder: {tag}"), &[], Mode::Commit);
    let hash = env.hash_identity().expect("hash");
    let seq = spine.append_pending(&env, &hash, &empty_payload()).expect("parking row");
    locks::acquire(&spine, seq, held, &cfg()).expect("park");
    seq
}

/// PARK AN ESCALATED READ: a real READ row (`append_read_row`, the one
/// append that passes the gate's checkpoint 2) under a real `read` grant —
/// what a hydrate storm leaves standing (test 23).
fn park_read(dir: &Path, resource: Resource, tag: &str) -> Seq {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let env = outside(&format!("park-read-{tag}"), &format!("-- escalated read: {tag}"), &[], Mode::Commit);
    let hash = env.hash_identity().expect("hash");
    let seq = match spine.append_read_row(&env, &hash, &empty_payload()).expect("the read's row") {
        spine::spine_db::AppendOutcome::Appended(seq) => seq,
        other => panic!("{other:?}"),
    };
    locks::acquire(&spine, seq, &claim(ALPHA, resource, LockMode::Read), &cfg()).expect("the read's grant");
    seq
}

/// A DECIDED, UN-CLEARED row touching `pool` with a delta on `node` — "a
/// landing in progress" as `in_landing` sees it, so every cross-pool
/// validated read touching that pool discards and escalates. Holds
/// no lock row (nothing is acquired), so it is not what a drain waits for.
fn park_decided(dir: &Path, pool: &str, version: u32, node: &str, tag: &str) -> Seq {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let env = outside(&format!("park-decided-{tag}"), &format!("-- decided holder: {tag}"), &[], Mode::Commit);
    let hash = env.hash_identity().expect("hash");
    let payload = CompiledPayload {
        parts: vec![spine::delta::CompiledPart {
            pool: PoolName::new(pool),
            schema_version: version,
            deltas: vec![spine::delta::Delta::SetField {
                node: NodeId(node.to_string()),
                path: vec!["state".into()],
                value: json!("open"),
            }],
            premises: vec![],
            far_versions: BTreeMap::new(),
        }],
        minted: BTreeMap::new(),
    };
    let seq = spine.append_pending(&env, &hash, &payload).expect("the decided row");
    spine.mark_decided(seq).expect("decide it");
    seq
}

/// Release a parked holder and close its row (the writer "landed").
fn unpark(dir: &Path, seq: Seq) {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    locks::release_at_clear(&spine, seq).expect("release");
    spine
        .mark_cleared(seq, &json!({ "parking_holder": "released" }))
        .expect("close the parking row");
}

/// Wait (bounded) for a lock row on `resource` in `pool` under a `seq` not in
/// `known`, with `granted` as asked — THE OVERLAP OBSERVATION: an enrolled
/// row says "a door call asked and is in flight right now". Returns its seq.
fn await_lock_row(dir: &Path, pool: &str, resource: &str, mode: &str, granted: bool, known: &[Seq]) -> Seq {
    let sql = spine_sql(dir);
    let started = Instant::now();
    loop {
        let mut stmt = sql
            .prepare("SELECT seq FROM locks WHERE pool = ?1 AND resource = ?2 AND mode = ?3 AND granted = ?4 ORDER BY seq")
            .unwrap();
        let seqs: Vec<i64> = stmt
            .query_map(rusqlite::params![pool, resource, mode, granted as i64], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        if let Some(s) = seqs.iter().find(|s| !known.iter().any(|k| k.0 == **s)) {
            return Seq(*s);
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "no {mode} row (granted={granted}) ever appeared on {pool}/{resource} beyond {known:?}: {seqs:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// [`await_lock_row`] while a CHILD PROCESS is the claimant: if the child
/// exits before the row appears, its whole output is the panic message.
fn await_lock_row_from(child: &mut std::process::Child, dir: &Path, pool: &str, resource: &str, mode: &str, granted: bool, known: &[Seq]) -> Seq {
    let sql = spine_sql(dir);
    let started = Instant::now();
    loop {
        let mut stmt = sql
            .prepare("SELECT seq FROM locks WHERE pool = ?1 AND resource = ?2 AND mode = ?3 AND granted = ?4 ORDER BY seq")
            .unwrap();
        let seqs: Vec<i64> = stmt
            .query_map(rusqlite::params![pool, resource, mode, granted as i64], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        if let Some(s) = seqs.iter().find(|s| !known.iter().any(|k| k.0 == **s)) {
            return Seq(*s);
        }
        if let Ok(Some(status)) = child.try_wait() {
            let mut out = String::new();
            let mut err = String::new();
            use std::io::Read;
            if let Some(mut o) = child.stdout.take() { let _ = o.read_to_string(&mut out); }
            if let Some(mut e) = child.stderr.take() { let _ = e.read_to_string(&mut err); }
            panic!("the child exited ({status}) before its {mode} row appeared on {pool}/{resource}.\nstdout:\n{out}\nstderr:\n{err}");
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "no {mode} row (granted={granted}) ever appeared on {pool}/{resource} beyond {known:?}: {seqs:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// ONE TICK as the daemon takes it: a tick that found the spine busy past the
/// timeout did nothing and is taken again next interval.
fn tick(c: &Cauldron, now: i64) -> TickReport {
    tick_with(c, &QUIET, now)
}

fn tick_with(c: &Cauldron, quiet: &QuietLevels, now: i64) -> TickReport {
    match c.tranche_tick(quiet, now) {
        Ok(r) => r,
        Err(SpineError::Busy { .. }) => TickReport::default(),
        Err(e) => panic!("tick: {e:?}"),
    }
}

/// Run `f` detached and panic if it has not answered within `secs` (a call
/// whose forced-red shape is "never answers" must go red, not hang; the
/// thread is leaked when the bound fires).
fn bounded<T: Send + 'static>(secs: u64, what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let h = std::thread::spawn(f);
    let started = Instant::now();
    while !h.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(secs), "{what} has not answered within {secs} s");
        std::thread::sleep(Duration::from_millis(5));
    }
    h.join().unwrap()
}

/// THE LANDING: the holder's `commit` on a detached thread, this thread
/// ticking until it answers (bounded 20 s). The landing is the tick's.
fn land(c: &Cauldron, a: &TrancheActor, now: i64) -> spine::error::Result<TrancheCommitted> {
    let (c2, a2) = (c.clone(), a.clone());
    let h = std::thread::spawn(move || c2.tranche_commit(&a2, now));
    let started = Instant::now();
    let mut reports = Vec::new();
    while !h.is_finished() {
        reports.push(tick(c, now));
        assert!(started.elapsed() < Duration::from_secs(20), "the landing never finished: {reports:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    h.join().expect("the commit thread")
}

/// Sets the flag when dropped — releases a watcher loop during a panic's
/// unwind.
struct SetOnDrop<'a>(&'a AtomicBool);
impl Drop for SetOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// `busy tranche_busy / locked` naming the holder, or panic.
fn expect_locked<T: std::fmt::Debug>(r: spine::error::Result<T>, what: &str) -> (String, u32) {
    match r {
        Err(SpineError::TrancheBusy { why, fields }) if why == "locked" => (
            fields["tranche"].as_str().unwrap().to_string(),
            fields["attempt"].as_u64().unwrap() as u32,
        ),
        other => panic!("{what}: expected busy tranche_busy/locked, got {other:?}"),
    }
}

/// The spine's `busy` naming the tranche's anchor `seq` as the holder.
fn busy_naming<T: std::fmt::Debug>(r: spine::error::Result<T>, seq: Seq, what: &str) {
    match r {
        Err(SpineError::Busy { message }) => assert!(
            message.contains(&format!("seq {}", seq.0)),
            "{what}: busy, but not naming the tranche's seq {}: {message}",
            seq.0
        ),
        other => panic!("{what}: expected busy naming tranche seq {}, got {other:?}", seq.0),
    }
}

fn committed(r: spine::error::Result<Outcome>, what: &str) -> Seq {
    match r {
        Ok(Outcome::Committed { seq, deduplicated: false, .. }) => seq,
        other => panic!("{what}: expected Committed, got {other:?}"),
    }
}

fn only_witness(e: SpineError) -> Witness {
    match e {
        SpineError::Refused { mut witnesses } if witnesses.len() == 1 => witnesses.remove(0),
        other => panic!("expected one witness, got {other:?}"),
    }
}

fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap()
}

/// `BEGIN IMMEDIATE` on `spine.db` from a side connection: every spine WRITE
/// waits in SQLite's busy handler until `release` (reads pass under WAL).
/// The test's way of holding a thread INSIDE a transaction it has entered
/// — with the mutex it took on the way in — for exactly as long as it wants.
struct SpineHold(Option<rusqlite::Connection>);

impl SpineHold {
    fn take(dir: &Path) -> SpineHold {
        let conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("open spine.db");
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").expect("hold the spine");
        SpineHold(Some(conn))
    }
    fn release(mut self) {
        if let Some(conn) = self.0.take() {
            conn.execute_batch("COMMIT").expect("release the spine");
        }
    }
}

impl Drop for SpineHold {
    fn drop(&mut self) {
        if let Some(conn) = self.0.take() {
            let _ = conn.execute_batch("COMMIT");
        }
    }
}

// ---------------------------------------------------------------------------
// THE CHILD PROCESS — a second actor with its own mutex map
// ---------------------------------------------------------------------------

const ROLE_ENV: &str = "SPINE_LOCKS_ROLE";

/// The roles the child can play; everything else comes in `SPINE_LOCKS_*`.
/// Prints `role: …` lines; the parent reads them back.
///
/// Not a test of anything when run by the harness: with no role in the
/// environment it returns at once (the dispatcher shape `door_smoke.rs`'s
/// `the_kill_child_body` uses). Every parent that spawns it asserts on the
/// lines it printed, never on its exit alone.
#[test]
fn locks_role_child() {
    let Ok(role) = std::env::var(ROLE_ENV) else {
        return;
    };
    let var = |k: &str| std::env::var(format!("SPINE_LOCKS_{k}")).unwrap_or_else(|_| panic!("SPINE_LOCKS_{k} unset"));
    let dir = PathBuf::from(var("DIR"));
    let mut config = cfg();
    if let Ok(ms) = std::env::var("SPINE_LOCKS_CEILING_MS") {
        config.lock_wait_ceiling = Duration::from_millis(ms.parse().unwrap());
    }
    if let Ok(ms) = std::env::var("SPINE_LOCKS_TIMEOUT_MS") {
        config.tranche.timeout = Duration::from_millis(ms.parse().unwrap());
        // `Cauldron::open` refuses `timeout < 3 × interval`.
        config.tranche.heartbeat_interval = config.tranche.timeout / 3;
    }
    if let Ok(ms) = std::env::var("SPINE_LOCKS_BUSY_MS") {
        config.busy_timeout = Duration::from_millis(ms.parse().unwrap());
    }
    // UNRECOVERED: the ordinary pass would abort the parent's parked rows.
    let c = Cauldron::open_unrecovered(&dir, config).expect("open the cauldron");
    // READY / GO (g13): the open WRITES the spine, so a parent that will
    // hold the spine's write lock takes it only after this marker, and this
    // process acts only once the parent says go.
    if let Ok(ready) = std::env::var("SPINE_LOCKS_READY") {
        std::fs::write(&ready, "opened").expect("write the ready marker");
    }
    if let Ok(go) = std::env::var("SPINE_LOCKS_GO") {
        let go = PathBuf::from(go);
        let started = Instant::now();
        while !go.exists() {
            assert!(started.elapsed() < Duration::from_secs(30), "the parent never said go");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let now = match std::env::var("SPINE_LOCKS_NOW").ok().as_deref() {
        Some("wall") | None => wall_ms(),
        Some(ms) => ms.parse().unwrap(),
    };
    let params: Vec<(String, Value)> = std::env::var("SPINE_LOCKS_PARAMS")
        .ok()
        .map(|p| {
            serde_json::from_str::<serde_json::Map<String, Value>>(&p)
                .unwrap()
                .into_iter()
                .collect()
        })
        .unwrap_or_default();
    let params: Vec<(&str, Value)> = params.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    let say_err = |e: SpineError| {
        let witness = match &e {
            SpineError::Refused { witnesses } => match witnesses.first() {
                Some(Witness::TrancheTerminal { state, .. }) => format!("tranche_terminal state={state}"),
                Some(Witness::TranchePastRollback { .. }) => "tranche_past_rollback".to_string(),
                Some(w) => format!("{w:?}"),
                None => "none".to_string(),
            },
            _ => "none".to_string(),
        };
        let why = match &e {
            SpineError::TrancheBusy { why, .. } => why.clone(),
            _ => "-".to_string(),
        };
        println!("role: err kind={} why={why} witness={witness} message={}", e.kind(), e.message().replace('\n', " "));
    };
    match role.as_str() {
        "stage" => {
            let a = actor(&var("T"), &var("KEY"));
            match c.tranche_stage(&a, one(&var("STEP"), &var("STMT"), &params), now) {
                Ok(ChildOutcome::Staged { ordinal, .. }) => println!("role: staged ordinal={ordinal}"),
                Ok(other) => println!("role: other {other:?}"),
                Err(e) => say_err(e),
            }
        }
        "read" => {
            let a = actor(&var("T"), &var("KEY"));
            match c.tranche_read(&a, &req(&var("STMT"), &params), now) {
                Ok(r) => println!("role: read rows={}", r.rows.0.len()),
                Err(e) => say_err(e),
            }
        }
        "submit" => match c.submit(outside(&var("NONCE"), &var("STMT"), &params, Mode::Commit)) {
            Ok(Outcome::Committed { seq, deduplicated, .. }) => println!("role: committed seq={} deduplicated={deduplicated}", seq.0),
            Ok(other) => println!("role: other {other:?}"),
            Err(e) => say_err(e),
        },
        "tick" => {
            // One tick, then wait (bounded) until the tranche named is
            // terminal or carries a landing error, so a lander this tick
            // spawned runs to its end before the process exits.
            let t = var("T");
            let r = tick(&c, now);
            println!("role: tick abandoned={:?} opened={:?} spawned={:?} running={:?} backing_off={:?} skipped={:?}", r.abandoned, r.opened, r.landers_spawned, r.landers_running, r.landers_backing_off, r.skipped_busy);
            // THE HANDSHAKE: the parent waits for this marker, never for a
            // guess at how long this process took to start.
            if let Ok(mark) = std::env::var("SPINE_LOCKS_MARK") {
                std::fs::write(&mark, "ticked").expect("write the tick marker");
            }
            // A PAUSED LANDER (g13's deterministic shape): this process must
            // outlive its own lander's pause — the lander is a thread, and a
            // returning test ends the process. So a process armed with a
            // pause latch waits for THAT latch before it starts waiting for
            // the landing's end, and, released, it waits for a TERMINAL state
            // only: the row's `landing_error` may be a sibling lander's (the
            // forced red writes one) and its own lander, still landing, is
            // the one that clears it (without this, the forced red's
            // process left at the sibling's error and took the parked lander
            // with it — nothing landed, and the red was a timeout).
            let paused = std::env::var(spine::fault::PAUSE_LATCH_ENV).ok().map(PathBuf::from);
            if let Some(latch) = &paused {
                let waited = Instant::now();
                while !latch.exists() {
                    assert!(waited.elapsed() < Duration::from_secs(150), "the parent never released this process's lander");
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            let started = Instant::now();
            loop {
                let s = c.tranche_store(&t, None).expect("store");
                let err = c
                    .tranche_heartbeat(&actor(&t, &var("KEY")), &QUIET, now)
                    .ok()
                    .and_then(|r| r.landing_error.map(|e| e.why));
                if s.state.is_terminal() || (paused.is_none() && err.is_some()) || started.elapsed() > Duration::from_secs(15) {
                    println!("role: final state={} landing_error={}", s.state.as_str(), err.unwrap_or_else(|| "none".into()));
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        other => panic!("unknown role {other}"),
    }
}

/// Spawn this binary as `role` with the environment given (plus the role and
/// the cauldron dir). `extra_env` may carry seam variables.
fn spawn_role(role: &str, dir: &Path, env: &[(&str, String)]) -> std::process::Child {
    let mut cmd = std::process::Command::new(std::env::current_exe().expect("this test binary"));
    cmd.args(["tranche::locks::locks_role_child", "--exact", "--nocapture", "--test-threads=1"])
        .env(ROLE_ENV, role)
        .env("SPINE_LOCKS_DIR", dir.as_os_str())
        .env_remove(spine::fault::FAULT_ENV)
        .env_remove(spine::fault::PAUSE_ENV)
        .env_remove(spine::fault::PAUSE_LATCH_ENV)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.spawn().expect("spawn the role child")
}

/// Wait for the child (bounded) and hand back its `role: …` lines.
fn role_lines(child: std::process::Child, secs: u64, what: &str) -> Vec<String> {
    let out = bounded(secs, what, move || child.wait_with_output().expect("the child's output"));
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let lines: Vec<String> = stdout.lines().map(|l| l.to_string()).collect();
    // Under `--nocapture` libtest prints a test's first stdout line on the
    // same line as its name, so a role line is matched by `contains`.
    assert!(lines.iter().any(|l| l.contains("role: ")), "{what}: the child printed no role line.\nstdout:\n{stdout}\nstderr:\n{stderr}");
    lines
}

fn poll_until(within: Duration, what: &str, mut pred: impl FnMut() -> bool) {
    let started = Instant::now();
    while !pred() {
        assert!(started.elapsed() < within, "{what} never held within {within:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---------------------------------------------------------------------------
// Test 6 — the gate at three checkpoints
// ---------------------------------------------------------------------------

/// Test 6, checkpoints 1 and 3 in process (checkpoint 2 is
/// [`t19_6b_…`], it needs the `after-phase-1` seam). N = 2 real writers
/// beside a parked foreign READER: writer W (a thread, its own door call)
/// passes phase 1 with the gate DOWN, appends its row and queues its `write`
/// on `x` behind the reader's granted `read` — THE OVERLAP: W's queued row
/// is observed in `locks` while the reader holds and before any tranche
/// exists. Then a `whole` tranche begins and opens inline (reads do not
/// count against the drain, a queued row is not granted), and W's next poll
/// is refused `locked` at checkpoint 3: its ONE row aborted (`decided = 0,
/// cleared = 1`), its queued rows withdrawn, W's reply the busy. A fresh
/// writer is refused at checkpoint 1 with no row at all; a `validate`
/// submit answers `validated`; a read answers rows. After `rollback` the
/// gate is down in the same transaction and W's retry lands at once.
// RED (measured): the gate at phase 1 ONLY (`gate_check` removed
// from `poll_once` and both enrolments) → W lands under the open tranche.
// OPPOSED: the same scene under a `touched` tranche — W waits out the reader
// and lands (the gate is down).
#[test]
fn t19_6_the_gate_refuses_an_outside_writer_at_checkpoints_1_and_3_and_passes_validate_and_reads() {
    let dir = root("t6");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let y = doc(&c, "y", "draft");
    for (mode, k, tag) in [(TrancheLockMode::Whole, 0x61u8, "whole"), (TrancheLockMode::Touched, 0x62, "touched")] {
        let reader = park(&dir, &claim(ALPHA, body(&x), LockMode::Read), &format!("t6-reader-{tag}"));
        let env = outside(&format!("t6-w-{tag}"), SET_TITLE, &[("id", json!(x)), ("t", json!(format!("by W {tag}")))], Mode::Commit);
        let hash = env.hash_identity().unwrap();
        let (c2, env2) = (c.clone(), env.clone());
        let w = std::thread::spawn(move || c2.submit(env2));
        // THE OVERLAP: W appended and is QUEUED behind the reader's read.
        let w_seq = await_lock_row(&dir, ALPHA, &format!("body:{x}"), "write", false, &[reader]);
        assert_eq!(rows_by_hash(&dir, &hash), 1, "W's row is on disk");
        // The tranche begins and OPENS INLINE: the reader's `read` row and
        // W's un-granted row are not what the drain waits for.
        let t = format!("T{tag}");
        let b = begin_as(&c, &t, &key(k), mode, &QUIET, 1_000);
        assert_eq!((b.state, b.draining), (TrancheState::Open, false), "{tag}: {b:?}");
        let a = actor(&t, &key(k));
        if mode == TrancheLockMode::Whole {
            // CHECKPOINT 3: W's next poll.
            let (tr, n) = expect_locked(bounded(10, "W under the gate", move || w.join().unwrap()), "W at checkpoint 3");
            assert_eq!((tr.as_str(), n), (t.as_str(), 1));
            let (decided, cleared): (i64, i64) = spine_sql(&dir)
                .query_row("SELECT decided, cleared FROM transactions WHERE hash = ?1", [hash.to_hex()], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap();
            assert_eq!((rows_by_hash(&dir, &hash), decided, cleared), (1, 0, 1), "ONE row, aborted — never restarted");
            assert_eq!(lock_rows_under(&dir, w_seq), 0, "W's queued rows withdrawn");
            // CHECKPOINT 1: a fresh writer, no row.
            let env_y = outside("t6-y", SET_TITLE, &[("id", json!(y)), ("t", json!("by Y"))], Mode::Commit);
            let hash_y = env_y.hash_identity().unwrap();
            expect_locked(c.submit(env_y), "a fresh writer at checkpoint 1");
            assert_eq!(rows_by_hash(&dir, &hash_y), 0, "no row written");
            // `validate` passes; a read passes.
            match c.submit(outside("t6-v", SET_TITLE, &[("id", json!(y)), ("t", json!("dry"))], Mode::Validate)) {
                Ok(Outcome::Validated { .. }) => {}
                other => panic!("validate under the gate: {other:?}"),
            }
            assert_eq!(title_of(&c, &x), json!("x"), "a read answers rows under the gate");
            // The tranche's own child stages under the gate.
            assert!(matches!(c.tranche_stage(&a, one("s0", SET_TITLE, &[("id", json!(y)), ("t", json!("by T"))]), 1_001), Ok(ChildOutcome::Staged { .. })));
            unpark(&dir, reader);
            // Rollback drops the gate in its own transaction: the retry lands.
            c.tranche_rollback(&a, 1_002).expect("rollback");
            committed(c.submit(env), "W's retry after the rollback");
            assert_eq!(title_of(&c, &x), json!("by W whole"));
        } else {
            // OPPOSED: no gate — W waits out the reader and lands.
            unpark(&dir, reader);
            committed(bounded(10, "W under touched", move || w.join().unwrap()), "W under a touched tranche");
            assert_eq!(title_of(&c, &x), json!("by W touched"));
            c.tranche_rollback(&a, 1_003).expect("rollback");
        }
    }
}

/// Test 6, CHECKPOINT 2 — the guarantee for new rows. The writer runs in
/// a CHILD PROCESS with `SPINE_PAUSE=after-phase-1`: it passes phase 1 with
/// the gate down and pauses between phase 1 and the append, publishing the
/// `.reached` marker — THE OVERLAP OBSERVATION (a writer provably past
/// checkpoint 1 and not yet at the append). The parent then begins a
/// `whole` tranche (inline open, gate up) and releases the latch: the
/// writer's append meets checkpoint 2 and is refused `locked` with NO row.
// RED (measured): checkpoint 2 removed from `append_or_existing` →
// the paused writer appends and lands under the open tranche. OPPOSED: the
// same paused writer with NO tranche begun commits once released.
#[cfg(feature = "fault-injection")]
#[test]
fn t19_6b_a_writer_paused_after_phase_1_meets_checkpoint_2_at_the_append_and_leaves_no_row() {
    let dir = root("t6b");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    for (gate, tag) in [(true, "gated"), (false, "free")] {
        let latch = dir.join(format!("latch-{tag}"));
        let reached = spine::fault::reached_path(&latch);
        let _ = std::fs::remove_file(&latch);
        let _ = std::fs::remove_file(&reached);
        let env = outside(&format!("t6b-{tag}"), SET_TITLE, &[("id", json!(x)), ("t", json!(tag))], Mode::Commit);
        let hash = env.hash_identity().unwrap();
        let child = spawn_role(
            "submit",
            &dir,
            &[
                ("SPINE_LOCKS_NONCE", format!("t6b-{tag}")),
                ("SPINE_LOCKS_STMT", SET_TITLE.to_string()),
                ("SPINE_LOCKS_PARAMS", json!({ "id": x, "t": tag }).to_string()),
                (spine::fault::PAUSE_ENV, "after-phase-1".to_string()),
                (spine::fault::PAUSE_LATCH_ENV, latch.to_string_lossy().to_string()),
            ],
        );
        poll_until(Duration::from_secs(20), "the writer reaching after-phase-1", || reached.exists());
        assert_eq!(std::fs::read_to_string(&reached).unwrap(), "after-phase-1");
        assert_eq!(rows_by_hash(&dir, &hash), 0, "paused BEFORE the append");
        let k = key(0x63);
        if gate {
            let b = begin_whole(&c, "T", &k, 1_000);
            assert_eq!(b.state, TrancheState::Open);
        }
        std::fs::write(&latch, "go").unwrap();
        let lines = role_lines(child, 30, "the paused writer");
        if gate {
            assert!(
                lines.iter().any(|l| l.contains("kind=tranche_busy why=locked")),
                "refused at checkpoint 2: {lines:?}"
            );
            assert_eq!(rows_by_hash(&dir, &hash), 0, "checkpoint 2 refuses BEFORE the insert");
            assert_eq!(title_of(&c, &x), json!("x"));
            c.tranche_rollback(&actor("T", &k), 1_001).unwrap();
        } else {
            assert!(lines.iter().any(|l| l.contains("role: committed")), "OPPOSED: no gate, it lands: {lines:?}");
            assert_eq!(rows_by_hash(&dir, &hash), 1);
            assert_eq!(title_of(&c, &x), json!("free"));
        }
    }
}

// ---------------------------------------------------------------------------
// Test 7 — `touched → whole` past the ceiling
// ---------------------------------------------------------------------------

/// Test 7 (N = 2: the holder's `lock` beside a foreign writer holding a
/// GRANTED `write` — a parked row, the real shape of "an ordinary commit
/// holds a granted write past the ceiling"). `lock → whole` runs the drain to
/// `lock_wait_ceiling` (400 ms, asserted elapsed), answers `busy
/// tranche_busy / switch` with `holding: [the writer's seq]`, and the mode
/// READS `touched` — so an outside writer on another node lands (no gate).
/// OVERLAP: the writer's granted row stands in `locks` for the whole switch
/// and is what `holding` names. OPPOSED: the writer released, the same
/// `lock` answers `switched: true`, the mode reads `whole`, the outside
/// writer is `locked`; `lock → whole` again is `switched: false`.
// RED (measured): the mode left at `whole` on the ceiling → the
// store reads `whole` and the outside writer is refused by a tranche that
// reported failure.
#[test]
fn t19_7_touched_to_whole_past_the_ceiling_is_busy_switch_and_the_mode_reads_touched() {
    let dir = root("t7");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let y = doc(&c, "y", "draft");
    let writer = park(&dir, &claim(ALPHA, body(&x), LockMode::Write), "t7-writer");
    let k = key(0x70);
    assert_eq!(begin_touched(&c, "T", &k, 1_000).state, TrancheState::Open);
    let a = actor("T", &k);
    let started = Instant::now();
    match c.tranche_lock(&a, TrancheLockMode::Whole, 1_001) {
        Err(SpineError::TrancheBusy { why, fields }) => {
            assert_eq!(why, "switch");
            assert_eq!(fields["holding"], json!([writer.0]), "names the granted writer");
        }
        other => panic!("expected busy switch, got {other:?}"),
    }
    assert!(started.elapsed() >= Duration::from_millis(400), "the drain ran to the ceiling: {:?}", started.elapsed());
    assert_eq!(mode_of(&c, "T"), TrancheLockMode::Touched, "the tranche as it was");
    committed(set_title(&c, "t7-y1", &y, "no gate"), "an outside writer with the mode back at touched");
    assert_eq!(lock_rows_under(&dir, writer), 1, "the switch released nothing of the writer's");
    // OPPOSED.
    unpark(&dir, writer);
    let switched = c.tranche_lock(&a, TrancheLockMode::Whole, 1_002).expect("lock to whole, drained");
    assert_eq!((switched.lock_mode, switched.switched, switched.state), (TrancheLockMode::Whole, true, TrancheState::Open));
    assert_eq!(mode_of(&c, "T"), TrancheLockMode::Whole);
    expect_locked(set_title(&c, "t7-y2", &y, "gated"), "an outside writer under whole");
    let again = c.tranche_lock(&a, TrancheLockMode::Whole, 1_003).expect("already whole");
    assert!(!again.switched);
    c.tranche_rollback(&a, 1_004).unwrap();
}

// ---------------------------------------------------------------------------
// Test 13 — two writers contending, `touched`
// ---------------------------------------------------------------------------

/// Test 13 (N = 3: the tranche and two outside writers IN FLIGHT AT
/// ONCE). The tranche stages a `SET` on `n1`; W1 (`SET n2`) and W2 (`SET n1`)
/// are spawned together. THE OVERLAP: W2's `write` row on `body:n1` is
/// observed QUEUED in `locks` while the tranche's granted `write` on it
/// stands and W1's door call is in flight — then W1 lands, W2 is `busy`
/// naming the tranche's `seq` at the ceiling, the tranche lands, and W2's
/// retry lands over it.
// RED (measured): the `write` claims dropped from `child_claims`
// (the child enrols reads only) → W2 lands first and the landing is
// REFUSED `tranche_landing_refused` (caught, not silent). OPPOSED: with the
// claim restored the landing is clean and W2's retry overwrites.
#[test]
fn t19_13_two_writers_contending_in_touched_mode_the_one_on_a_touched_body_is_busy_naming_the_tranche_and_lands_after() {
    let dir = root("t13");
    let c = fresh(&dir);
    let n1 = doc(&c, "n1", "draft");
    let n2 = doc(&c, "n2", "draft");
    let k = key(0x13);
    begin_touched(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    assert!(matches!(c.tranche_stage(&a, one("s0", SET_TITLE, &[("id", json!(n1)), ("t", json!("by T"))]), 1_001), Ok(ChildOutcome::Staged { .. })));
    let seq = anchor_seq(&c, "T");
    let (c1, c2) = (c.clone(), c.clone());
    let (m1, m2) = (n2.clone(), n1.clone());
    let w1 = std::thread::spawn(move || set_title(&c1, "t13-w1", &m1, "by W1"));
    let w2 = std::thread::spawn(move || set_title(&c2, "t13-w2", &m2, "by W2"));
    // THE OVERLAP: W2 queued behind the tranche's granted write on n1.
    let w2_seq = await_lock_row(&dir, ALPHA, &format!("body:{n1}"), "write", false, &[seq]);
    assert_eq!(count(&dir, "SELECT COUNT(*) FROM locks WHERE seq = ?1 AND resource = ?2 AND mode = 'write' AND granted = 1", &[&seq.0, &format!("body:{n1}")]), 1, "the tranche holds n1 while W2 queues");
    committed(bounded(10, "W1", move || w1.join().unwrap()), "W1 on an untouched node lands at once");
    busy_naming(bounded(10, "W2", move || w2.join().unwrap()), seq, "W2 on the touched node");
    assert_eq!(lock_rows_under(&dir, w2_seq), 0, "W2's queued rows withdrawn at the ceiling");
    assert_eq!(title_of(&c, &n1), json!("n1"), "nothing landed on n1 yet");
    let reply = land(&c, &a, 1_002).expect("the tranche lands clean");
    assert_eq!(reply.state, TrancheState::Landed);
    assert_eq!(title_of(&c, &n1), json!("by T"));
    committed(set_title(&c, "t13-w2", &n1, "by W2"), "W2's retry");
    assert_eq!(title_of(&c, &n1), json!("by W2"));
    assert_eq!(title_of(&c, &n2), json!("by W1"));
}

/// A busy behind a tranche (a writer on a touched
/// resource "is answered the spine's `busy` naming `tranche <T>` as the
/// holder"). Two holders, two busies, one test, so the text is read off a
/// real refusal at the ceiling in BOTH directions:
/// - behind a `touched` tranche `TH`'s granted write on n1 → the message
///   names `holder seq <anchor> is tranche TH (attempt 1, …)`, keeps the
///   `holder seq <anchor>` tag the soup's `busy_holder_seqs` reads, and does
///   NOT carry the ordinary "NOT DECIDED … recover() aborts it" reading of
///   the anchor's never-decided spine row;
/// - OPPOSED: behind a granted row parked under a seq the spine never issued
///   (no `tranches` row) → the ordinary "NO spine row" text, and the word
///   `tranche` nowhere in the holder's description.
/// RED (measured): `describe_holder`'s tranche arm removed →
/// the first busy says "appended and NOT DECIDED". RED: the arm made
/// unconditional (every holder `is tranche`) → the opposed busy names one.
#[test]
fn a_busy_behind_a_touched_tranche_names_tranche_t_and_a_busy_behind_an_ordinary_holder_does_not() {
    let dir = root("busy-holder");
    let c = fresh(&dir);
    let n1 = doc(&c, "n1", "draft");
    let n2 = doc(&c, "n2", "draft");
    let k = key(0x2c);
    begin_touched(&c, "TH", &k, 1_000);
    let a = actor("TH", &k);
    assert!(matches!(c.tranche_stage(&a, one("h0", SET_TITLE, &[("id", json!(n1)), ("t", json!("by TH"))]), 1_001), Ok(ChildOutcome::Staged { .. })));
    let seq = anchor_seq(&c, "TH");
    let busy_text = |r: spine::error::Result<Outcome>, what: &str| match r {
        Err(SpineError::Busy { message }) => message,
        other => panic!("{what}: expected the spine's busy at the ceiling, got {other:?}"),
    };
    let c1 = c.clone();
    let m1 = n1.clone();
    let behind_tranche = busy_text(bounded(10, "W on the touched body", move || set_title(&c1, "busy-w1", &m1, "by W1")), "W on the touched body");
    println!("busy: behind the tranche: {behind_tranche}");
    let described = behind_tranche.split_once(". ").map(|(_, d)| d.to_string()).unwrap_or_default();
    assert!(behind_tranche.contains(&format!("blocked by holder seq {}", seq.0)), "the blocker list is unchanged: {behind_tranche}");
    assert!(
        described.starts_with(&format!("holder seq {} is tranche TH (attempt 1, ", seq.0)),
        "the holder is named `tranche <T>`, after the `holder seq <n>` tag the soup reads: {described}"
    );
    assert!(!described.contains("NOT DECIDED"), "a tranche anchor is not a live door recover() aborts: {described}");

    // OPPOSED: an ordinary holder (a granted row under a seq with no spine
    // row and no tranches row) is described the ordinary way.
    const FOREIGN: i64 = 9_100_000;
    spine_sql(&dir)
        .execute(
            "INSERT INTO locks (pool, resource, mode, seq, granted) VALUES (?1, ?2, 'write', ?3, 1)",
            rusqlite::params![ALPHA, format!("body:{n2}"), FOREIGN],
        )
        .unwrap();
    let c2 = c.clone();
    let m2 = n2.clone();
    let behind_ordinary = busy_text(bounded(10, "W on the foreign-held body", move || set_title(&c2, "busy-w2", &m2, "by W2")), "W on the foreign-held body");
    println!("busy: behind the ordinary holder: {behind_ordinary}");
    let described = behind_ordinary.split_once(". ").map(|(_, d)| d.to_string()).unwrap_or_default();
    assert!(described.starts_with(&format!("holder seq {FOREIGN}: NO spine row")), "{described}");
    assert!(!described.contains("tranche"), "an ordinary holder is never named a tranche: {described}");
    c.tranche_rollback(&a, 1_002).unwrap();
}

// ---------------------------------------------------------------------------
// Test 14 — lost update through a read, three shapes
// ---------------------------------------------------------------------------

/// The child used by 14(a): premised on `n3`'s title, writing `o`.
const PREMISED_ON_N3: &str = r#"MATCH (d: fixture-alpha/doc { id: $n3 }) MATCH (o: fixture-alpha/doc { id: $o }) WHERE d.title = "n3" SET o.title = "premised on n3""#;

/// Test 14(a) (N = 2: the holder and an outside writer in flight). The
/// tranche reads `n3` under key and stages a child premised on it (`WHERE
/// d.title = "n3"`); W `SET n3` queues behind the tranche's `read` on
/// `body:n3` — THE OVERLAP: W's `write` row observed queued while the
/// tranche's granted `read` stands — and is `busy` naming the tranche; the
/// tranche lands clean; W's retry lands.
// RED (measured): the matched-body `read` dropped (the by-id head's
// row in `tranche_read_claims` AND the bound-body row in `claims_for`) → W
// lands under the tranche and the landing is refused `revalidation` (the
// premise no longer holds). OPPOSED: restored, the landing is clean.
#[test]
fn t19_14a_a_write_to_a_body_the_tranche_read_is_busy_and_the_tranche_lands_clean() {
    let dir = root("t14a");
    let c = fresh(&dir);
    let n3 = doc(&c, "n3", "draft");
    let o = doc(&c, "o", "draft");
    let k = key(0x14);
    begin_touched(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    let read = c.tranche_read(&a, &req(READ_TITLE, &[("id", json!(n3))]), 1_001).expect("read under key");
    assert_eq!(read.rows.0[0]["d.title"], json!("n3"));
    assert!(matches!(c.tranche_stage(&a, one("s0", PREMISED_ON_N3, &[("n3", json!(n3)), ("o", json!(o))]), 1_002), Ok(ChildOutcome::Staged { .. })));
    let seq = anchor_seq(&c, "T");
    let (c2, m) = (c.clone(), n3.clone());
    let w = std::thread::spawn(move || set_title(&c2, "t14a-w", &m, "moved"));
    let w_seq = await_lock_row(&dir, ALPHA, &format!("body:{n3}"), "write", false, &[seq]);
    assert_eq!(count(&dir, "SELECT COUNT(*) FROM locks WHERE seq = ?1 AND resource = ?2 AND mode = 'read' AND granted = 1", &[&seq.0, &format!("body:{n3}")]), 1, "the tranche holds its read on n3");
    busy_naming(bounded(10, "W", move || w.join().unwrap()), seq, "W on the body the tranche read");
    assert_eq!(lock_rows_under(&dir, w_seq), 0);
    let reply = land(&c, &a, 1_003).expect("the tranche lands clean");
    assert_eq!(reply.children.len(), 1);
    assert_eq!(title_of(&c, &o), json!("premised on n3"));
    committed(set_title(&c, "t14a-w", &n3, "moved"), "W's retry after the landing");
}

/// The child used by 14(b): a hop from `D` that binds no successor, premised
/// on there being none.
const NO_SUCCESSOR: &str = r#"MATCH (d: fixture-alpha/doc { id: $d }) -[supersedes]-> (q: fixture-alpha/doc) MATCH (o: fixture-alpha/doc { id: $o }) EXPECT COUNT(q) = 0 SET o.title = "D has no successor""#;
const CONNECT: &str = r#"MATCH (a: fixture-alpha/doc { id: $a }) MATCH (b: fixture-alpha/doc { id: $b }) CREATE (a) -[supersedes]-> (b)"#;

/// Test 14(b) (N = 2). The child matches `(d {id: D}) -[supersedes]->
/// (q)` and binds no `q`; the touched set holds `read` on
/// `eslice:D:supersedes:out` (the hop's trace). W creates `D -[supersedes]->
/// Q`, whose `link` on that slice conflicts — THE OVERLAP: W's `link` row
/// observed queued while the tranche's `read` on the slice stands — `busy`
/// naming the tranche; the tranche lands clean.
// RED (measured): the hop's slice claims dropped from
// `tranche_read_claims` → W lands, the child's `EXPECT COUNT(q) = 0` fails
// at re-validation, the landing is refused. OPPOSED: restored, clean.
#[test]
fn t19_14b_an_edge_into_a_traversed_slice_that_bound_nothing_is_busy_and_the_tranche_lands_clean() {
    let dir = root("t14b");
    let c = fresh(&dir);
    let d = doc(&c, "D", "draft");
    let q = doc(&c, "Q", "draft");
    let o = doc(&c, "o", "draft");
    let k = key(0x15);
    begin_touched(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    c.tranche_stage(&a, one("s0", NO_SUCCESSOR, &[("d", json!(d)), ("o", json!(o))]), 1_001).unwrap_or_else(|e| panic!("the premised child stages: {e:?}"));
    let seq = anchor_seq(&c, "T");
    let slice = format!("eslice:{d}:supersedes:out");
    assert_eq!(count(&dir, "SELECT COUNT(*) FROM locks WHERE seq = ?1 AND resource = ?2 AND mode = 'read' AND granted = 1", &[&seq.0, &slice]), 1, "the hop's out-slice is held");
    let (c2, dd, qq) = (c.clone(), d.clone(), q.clone());
    let w = std::thread::spawn(move || keyless(&c2, "t14b-w", CONNECT, &[("a", json!(dd)), ("b", json!(qq))]));
    let w_seq = await_lock_row(&dir, ALPHA, &slice, "link", false, &[seq]);
    busy_naming(bounded(10, "W", move || w.join().unwrap()), seq, "W connecting into the traversed slice");
    assert_eq!(lock_rows_under(&dir, w_seq), 0);
    land(&c, &a, 1_002).expect("the tranche lands clean");
    assert_eq!(title_of(&c, &o), json!("D has no successor"));
    committed(keyless(&c, "t14b-w", CONNECT, &[("a", json!(d)), ("b", json!(q))]), "W's retry");
}

/// The child used by 14(c): a by-identifier head that binds nothing,
/// premised on the absence.
const ABSENT_X: &str = r#"MATCH (x: fixture-alpha/doc { id: $x }) MATCH (o: fixture-alpha/doc { id: $o }) EXPECT COUNT(x) = 0 SET o.title = "x is absent""#;
const IMPORT_X: &str = r#"IMPORT (n: fixture-alpha/doc { id: $x, title: "x", state: "draft" })"#;

/// Test 14(c) (N = 2). The child matches `{id: X}` and binds nothing;
/// the touched set holds `read` on `body:X` (the by-identifier row, "the
/// body that bound nothing"). W IMPORTs `X` (the one create that carries its
/// id), whose `write` on `body:X` conflicts — THE OVERLAP: W's `write` row
/// observed queued while the tranche's `read` stands — `busy` naming the
/// tranche; the tranche lands clean.
// RED (measured): the by-identifier body `read` dropped from
// `tranche_read_claims` (a head that binds nothing has no bound-body row to
// fall back on) → W lands, `EXPECT COUNT(x) = 0` fails at re-validation,
// the landing is refused. OPPOSED: restored, clean.
#[test]
fn t19_14c_a_create_at_an_identifier_the_tranche_looked_up_and_missed_is_busy_and_the_tranche_lands_clean() {
    let dir = root("t14c");
    // `doc` mints serials here: an `IMPORT` may carry an id only into a type
    // with an `id_prefix`, and the carried id must be `<prefix>-<n>`.
    let c = fresh_prefixed(&dir);
    let o = doc(&c, "o", "draft");
    // A serial-shaped id no row carries: the by-identifier head misses it.
    let x = "doc-777";
    let k = key(0x16);
    begin_touched(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    c.tranche_stage(&a, one("s0", ABSENT_X, &[("x", json!(x)), ("o", json!(o))]), 1_001).unwrap_or_else(|e| panic!("the premised child stages: {e:?}"));
    let seq = anchor_seq(&c, "T");
    assert_eq!(count(&dir, "SELECT COUNT(*) FROM locks WHERE seq = ?1 AND resource = ?2 AND mode = 'read' AND granted = 1", &[&seq.0, &format!("body:{x}")]), 1, "the missed identifier's body is held");
    let c2 = c.clone();
    let w = std::thread::spawn(move || c2.submit(agent("t14c-w", IMPORT_X, &[("x", json!(x))])));
    let w_seq = {
        let started = Instant::now();
        loop {
            if count(&dir, "SELECT COUNT(*) FROM locks WHERE resource = ?1 AND mode = 'write' AND granted = 0 AND seq != ?2", &[&format!("body:{x}"), &seq.0]) > 0 {
                break Seq(count(&dir, "SELECT seq FROM locks WHERE resource = ?1 AND mode = 'write' AND granted = 0 AND seq != ?2", &[&format!("body:{x}"), &seq.0]));
            }
            if w.is_finished() {
                panic!("W answered before queuing on body:{x}: {:?}", w.join().unwrap());
            }
            assert!(started.elapsed() < Duration::from_secs(10), "W never queued on body:{x}");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    busy_naming(bounded(10, "W", move || w.join().unwrap()), seq, "W importing the missed identifier");
    assert_eq!(lock_rows_under(&dir, w_seq), 0);
    land(&c, &a, 1_002).expect("the tranche lands clean");
    assert_eq!(title_of(&c, &o), json!("x is absent"));
    committed(c.submit(agent("t14c-w", IMPORT_X, &[("x", json!(x))])), "W's retry");
    assert_eq!(title_of(&c, x), json!("x"));
}

// ---------------------------------------------------------------------------
// Test 15 — two acts, one key, two connections
// ---------------------------------------------------------------------------

const SET_TWO: &str = r#"MATCH (a: fixture-alpha/doc { id: $x }) MATCH (b: fixture-alpha/doc { id: $y }) SET a.title = "by B" SET b.title = "by B""#;

/// Test 15, first half (N = 2 claim calls under ONE key in TWO
/// PROCESSES, so the per-process mutex does not serialise them — the only
/// way a second call can classify while the first's row is still QUEUED).
/// Child A (the child process) stages `SET X` and queues behind a parked
/// foreign writer on `X`; child B (this process) claims `X` and `Y`. THE
/// OVERLAP: A's queued `write` on `body:X` is observed under the anchor's
/// `seq`, then B's `write` on `body:Y` is observed queued too — two claim
/// calls in flight under one number. Rule 1: B reads A's queued row
/// as NOT held, so B waits for `X` like A does (asserted: B has not
/// answered while the writer is parked). The writer releases; both grant;
/// the touched set holds `X` and `Y` GRANTED, nothing merely queued.
// RED (measured): `acquire_for_tranche` classifying against ALL rows
// under the `seq` (granted or queued) → B reads A's queued `X` as held,
// enrols `Y` alone, is granted at once and STAGES while `X` is only queued —
// B answers before the writer is released (the assertion), and the tranche
// then holds a child premised on a body it does not hold.
#[test]
fn t19_15a_two_claim_calls_under_one_key_in_two_processes_the_seconds_body_is_held_not_merely_queued() {
    let dir = root("t15a");
    let mut config = cfg();
    config.lock_wait_ceiling = Duration::from_secs(6);
    let c = fresh_with(&dir, config);
    let x = doc(&c, "x", "draft");
    let y = doc(&c, "y", "draft");
    let writer = park(&dir, &claim(ALPHA, body(&x), LockMode::Write), "t15a-writer");
    let k = key(0x17);
    begin_touched(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    let seq = anchor_seq(&c, "T");
    // Child A, in the other process, queues behind the writer.
    let child_a = spawn_role(
        "stage",
        &dir,
        &[
            ("SPINE_LOCKS_T", "T".into()),
            ("SPINE_LOCKS_KEY", k.clone()),
            ("SPINE_LOCKS_STEP", "sA".into()),
            ("SPINE_LOCKS_STMT", SET_TITLE.into()),
            ("SPINE_LOCKS_PARAMS", json!({ "id": x, "t": "by A" }).to_string()),
            ("SPINE_LOCKS_NOW", "1001".into()),
            ("SPINE_LOCKS_CEILING_MS", "6000".into()),
        ],
    );
    let mut child_a = child_a;
    let a_row = await_lock_row_from(&mut child_a, &dir, ALPHA, &format!("body:{x}"), "write", false, &[writer]);
    assert_eq!(a_row, seq, "A's queued row is under the tranche's seq");
    // Child B, here, claims X and Y.
    let (c2, a2, xx, yy) = (c.clone(), a.clone(), x.clone(), y.clone());
    let b = std::thread::spawn(move || c2.tranche_stage(&a2, one("sB", SET_TWO, &[("x", json!(xx)), ("y", json!(yy))]), 1_002));
    {
        let started = Instant::now();
        loop {
            if count(&dir, "SELECT COUNT(*) FROM locks WHERE seq = ?1 AND resource = ?2 AND mode = 'write'", &[&seq.0, &format!("body:{y}")]) > 0 {
                break;
            }
            if b.is_finished() {
                panic!("B answered before enrolling Y: {:?}", b.join().unwrap());
            }
            assert!(started.elapsed() < Duration::from_secs(10), "B never enrolled Y");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    // Rule 1: B does NOT read A's queued X as held — it waits for X too.
    // (The period outlasted: B's enrolment and first polls, milliseconds;
    // under the red B answers inside it.)
    std::thread::sleep(Duration::from_millis(400));
    assert!(!b.is_finished(), "B waits for X with A — it did not stage holding a queued X");
    assert_eq!(count(&dir, "SELECT COUNT(*) FROM locks WHERE seq = ?1 AND granted = 1 AND resource = ?2", &[&seq.0, &format!("body:{x}")]), 0, "X is not granted to the tranche yet");
    unpark(&dir, writer);
    assert!(matches!(bounded(15, "B", move || b.join().unwrap()), Ok(ChildOutcome::Staged { .. })));
    let lines = role_lines(child_a, 15, "child A");
    assert!(lines.iter().any(|l| l.contains("role: staged")), "A staged too: {lines:?}");
    let t = c.tranche_touched("T", 1).unwrap();
    for id in [&x, &y] {
        assert!(t.iter().any(|(_, r, m, g)| r == &format!("body:{id}") && m == "write" && *g), "{id} HELD: {t:?}");
    }
    assert!(t.iter().all(|(_, _, _, g)| *g), "nothing under the tranche's seq is merely queued: {t:?}");
    assert_eq!(c.tranche_store("T", None).unwrap().children.len(), 2);
    c.tranche_rollback(&a, 1_003).unwrap();
}

/// Test 15, second half (N = 2: a child and `commit` crossing on the
/// mutex). `commit` takes the mutex and is HELD inside its flip transaction
/// by a [`SpineHold`]; child B passes the door's legality check (`staging`)
/// and blocks on the mutex behind it — THE OVERLAP: B is in flight (not
/// answered) while `commit` holds the mutex, both past their door checks.
/// The hold releases: the flip lands `landing`; B takes the mutex and its
/// SECOND legality check finds `landing` → `refused tranche_past_rollback`,
/// `staged` unchanged, no lock row inserted for B's body. Then the landing.
// RED (measured): the inner check made "terminal only" (`landing`
// admitted inside the mutex) → B is staged INTO a landing tranche (`staged`
// = N + 1). OPPOSED: the same B before any `commit` stages.
#[test]
fn t19_15b_a_child_blocked_on_the_mutex_behind_commits_flip_is_refused_past_rollback_with_nothing_inserted() {
    let dir = root("t15b");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let y = doc(&c, "y", "draft");
    let k = key(0x18);
    begin_touched(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    assert!(matches!(c.tranche_stage(&a, one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!("by T"))]), 1_001), Ok(ChildOutcome::Staged { .. })));
    let seq = anchor_seq(&c, "T");
    let hold = SpineHold::take(&dir);
    let (c1, a1) = (c.clone(), a.clone());
    let commit = std::thread::spawn(move || c1.tranche_commit(&a1, 1_002));
    // `commit` is past its door check and holds the mutex, blocked in its
    // flip transaction (the period outlasted: its door check, milliseconds).
    std::thread::sleep(Duration::from_millis(300));
    let (c2, a2, yy) = (c.clone(), a.clone(), y.clone());
    let b = std::thread::spawn(move || c2.tranche_stage(&a2, one("sB", SET_TITLE, &[("id", json!(yy)), ("t", json!("by B"))]), 1_003));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!b.is_finished() && !commit.is_finished(), "both in flight, crossed on the mutex");
    assert_eq!(state_of(&c, "T"), TrancheState::Staging, "nothing flipped while held");
    hold.release();
    match bounded(10, "B", move || b.join().unwrap()) {
        Err(e) => match only_witness(e) {
            Witness::TranchePastRollback { tranche, attempt } => assert_eq!((tranche.as_str(), attempt), ("T", 1)),
            w => panic!("expected tranche_past_rollback, got {w:?}"),
        },
        Ok(o) => panic!("B was staged behind the flip: {o:?}"),
    }
    assert_eq!(c.tranche_store("T", None).unwrap().children.len(), 1, "staged unchanged");
    assert_eq!(count(&dir, "SELECT COUNT(*) FROM locks WHERE seq = ?1 AND resource = ?2", &[&seq.0, &format!("body:{y}")]), 0, "no lock row inserted for B");
    // The landing finishes on the tick; the commit answers from the record.
    let started = Instant::now();
    while !commit.is_finished() {
        tick(&c, 1_004);
        assert!(started.elapsed() < Duration::from_secs(20), "the landing never finished");
        std::thread::sleep(Duration::from_millis(10));
    }
    let reply = commit.join().unwrap().expect("landed");
    assert_eq!((reply.state, reply.children.len()), (TrancheState::Landed, 1));
}

// ---------------------------------------------------------------------------
// Test 16 — abandon during a claim wait (the `no-poll-refresh` seam)
// ---------------------------------------------------------------------------

/// Test 16 (N = 2 processes; period named: the heartbeat TIMEOUT, 300 ms
/// here). A read under key in a CHILD PROCESS waits on `body:X` behind a
/// parked foreign writer; its polls would refresh the deadline, so the
/// child runs with `SPINE_FAULT=no-poll-refresh` and the deadline lapses
/// under its wait. THE OVERLAP: the child's `read` row is observed QUEUED
/// under the anchor's `seq` while the writer's granted row stands. The
/// scheduler (this process — its mutex is free, the child's is not shared)
/// abandons the tranche; the child's NEXT POLL answers `refused
/// tranche_terminal {state: abandoned}` INSIDE its wait (bounded 5 s, the
/// writer still parked), no lock row stands under the tranche's `seq`, and a
/// new writer on `X` lands once the parked one clears.
// RED (measured): `claim_state_check` removed from `poll_once` → the
// child never learns of the abandon inside its wait (the 5 s bound fires
// while the writer is parked) and, once the writer clears, is GRANTED a
// `read` row under the terminal `seq` that nothing releases — the new writer
// `busy` until a restart. OPPOSED: without the seam the polls refresh the
// deadline, the same tick abandons nothing, and the read answers rows once
// the writer clears.
#[cfg(feature = "fault-injection")]
#[test]
fn t19_16_abandon_during_a_claim_wait_is_answered_inside_the_wait_leaves_no_lock_row_and_frees_the_resource_outlasting_the_300ms_timeout() {
    let dir = root("t16");
    let mut config = cfg();
    config.tranche.timeout = Duration::from_millis(300);
    config.tranche.heartbeat_interval = Duration::from_millis(100);
    let c = fresh_with(&dir, config);
    let x = doc(&c, "x", "draft");
    for (seam, tag) in [(true, "seamed"), (false, "refreshing")] {
        let writer = park(&dir, &claim(ALPHA, body(&x), LockMode::Write), &format!("t16-{tag}"));
        let t = format!("T{tag}");
        let k = key(if seam { 0x19 } else { 0x1a });
        begin_touched(&c, &t, &k, wall_ms());
        let seq = anchor_seq(&c, &t);
        let mut env: Vec<(&str, String)> = vec![
            ("SPINE_LOCKS_T", t.clone()),
            ("SPINE_LOCKS_KEY", k.clone()),
            ("SPINE_LOCKS_STMT", READ_TITLE.into()),
            ("SPINE_LOCKS_PARAMS", json!({ "id": x }).to_string()),
            ("SPINE_LOCKS_NOW", "wall".into()),
            ("SPINE_LOCKS_TIMEOUT_MS", "300".into()),
            ("SPINE_LOCKS_CEILING_MS", "8000".into()),
        ];
        if seam {
            env.push((spine::fault::FAULT_ENV, "no-poll-refresh".into()));
        }
        let mut child = spawn_role("read", &dir, &env);
        let row = await_lock_row_from(&mut child, &dir, ALPHA, &format!("body:{x}"), "read", false, &[writer]);
        assert_eq!(row, seq, "the child's read is queued under the anchor's seq");
        // Outlast the timeout: with the seam the deadline stays where
        // `begin` put it; without it every poll moved it forward.
        std::thread::sleep(Duration::from_millis(500));
        let now = wall_ms();
        let (began, beat, deadline): (i64, i64, i64) = spine_sql(&dir)
            .query_row("SELECT began_at, last_heartbeat, deadline FROM tranches WHERE id = ?1", [&t], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        let report = tick(&c, now);
        let clocks = format!("began_at={began} last_heartbeat={beat} (+{} ms) deadline={deadline} tick_now={now} (+{} ms)", beat - began, now - began);
        if seam {
            assert_eq!(report.abandoned, vec![(t.clone(), 1)], "{report:?}; {clocks}");
            let lines = role_lines(child, 5, "the waiting read, inside its wait");
            assert!(lines.iter().any(|l| l.contains("tranche_terminal state=abandoned")), "{lines:?}");
            assert_eq!(lock_rows_under(&dir, seq), 0, "no lock row under the terminal seq");
            unpark(&dir, writer);
            committed(set_title(&c, "t16-after", &x, "after the abandon"), "a new writer on X lands");
            assert_eq!(state_of(&c, &t), TrancheState::Abandoned);
        } else {
            assert!(report.abandoned.is_empty(), "the polls kept the holder alive: {report:?}; {clocks}");
            assert!(report.skipped_busy.is_empty(), "{report:?}");
            unpark(&dir, writer);
            let lines = role_lines(child, 10, "the read once the writer clears");
            assert!(lines.iter().any(|l| l.contains("role: read rows=1")), "{lines:?}");
            assert_eq!(count(&dir, "SELECT COUNT(*) FROM locks WHERE seq = ?1 AND granted = 1", &[&seq.0]), 1, "the read's row is held under the live seq");
            c.tranche_rollback(&actor(&t, &k), wall_ms()).unwrap();
        }
    }
}

// ---------------------------------------------------------------------------
// Test 19 — readers during a long landing, two shapes
// ---------------------------------------------------------------------------

const CLOSE_TICKET: &str = r#"MATCH (t: fixture-beta/ticket { id: $t }) SET t.state = "closed""#;
const READ_TICKET: &str = r#"MATCH (t: fixture-beta/ticket { id: $t }) RETURN t.state"#;
/// A CROSS-POOL read: the validated read's window check (and so its
/// invalidation and escalation) runs only for a read of two or more
/// pools — a single-pool read is one snapshot and is consistent by itself
/// (`read::drive`).
const CROSS_READ: &str = r#"MATCH (d: fixture-alpha/doc { id: $id }) MATCH (t: fixture-beta/ticket { id: $t }) RETURN d.title, t.state"#;

/// HOLD A POOL FILE: an open IMMEDIATE transaction on a second connection —
/// "a pool file held by another process" — so the lander's landing
/// of that pool's child stops `file_held` after its ceiling, decided, its
/// locks held, with the children behind it reserved and undecided.
struct PoolHold(Option<rusqlite::Connection>);
impl PoolHold {
    fn take(dir: &Path, pool: &str) -> PoolHold {
        let conn = rusqlite::Connection::open(spine::pool::Pool::path_in(dir, &PoolName::new(pool))).expect("open the pool");
        conn.execute_batch("BEGIN IMMEDIATE").expect("hold the pool file");
        PoolHold(Some(conn))
    }
    fn release(mut self) {
        if let Some(conn) = self.0.take() {
            conn.execute_batch("COMMIT").expect("release the pool file");
        }
    }
}
impl Drop for PoolHold {
    fn drop(&mut self) {
        if let Some(conn) = self.0.take() {
            let _ = conn.execute_batch("COMMIT");
        }
    }
}

/// Test 19, both shapes (N = 2: the lander on its thread, stalled
/// mid-landing; readers and a foreign writer on this one). Three children —
/// `x0` (alpha), ticket `t` (beta), `x2` (alpha) — and the BETA pool file
/// held: the lander lands child 0, decides child 1 and stalls on beta
/// (`file_held`), child 2 stays RESERVED AND UNDECIDED. THE OVERLAP: the
/// record reads `landing, decided, landed_children = 1` while the hold
/// stands — a landing in progress with locks held. (a) `in_landing(alpha)` is
/// EMPTY (child 2's reservation is undecided — the mechanism that keeps
/// readers of a touched pool lock-free); a cross-pool read of `z` (alpha,
/// not written) and `t2` (beta, not written) is invalidated by the stuck
/// child's landing on beta, ESCALATES to real `read` locks on bodies the
/// tranche did not write, is granted and answers rows — never `busy`; a
/// read of `x0` (landed) answers its new value; (b) the same cross-pool read
/// naming `t` (beta, decided and stuck) escalates to a `read` on `body:t`,
/// waits behind the tranche's `write` to the ceiling and is `busy` naming
/// the tranche's `seq`; a foreign `SET x0` BETWEEN children is `busy`
/// naming the tranche (the body stays held until the clear); released, the
/// tranche lands and the read of `t` answers `closed`.
// RED (measured): (a) the child rows reserved DECIDED instead of
// undecided → `in_landing(alpha)` names child 2 and the read of `z` discards
// eight times and escalates; (b) a child's body locks released when ITS
// child clears → the foreign `SET x0` lands between children in `touched`
// mode. OPPOSED: after `landed` the same `SET x0` lands and the read of `t`
// answers rows.
#[test]
fn t19_19_readers_during_a_long_landing_untouched_bodies_answer_lock_free_and_a_touched_body_is_busy_naming_the_tranche() {
    let dir = root("t19");
    let mut config = cfg();
    config.busy_timeout = Duration::from_millis(300);
    let c = fresh_with(&dir, config);
    let x0 = doc(&c, "x0", "draft");
    let x2 = doc(&c, "x2", "draft");
    let z = doc(&c, "z", "draft");
    let t = ticket(&c);
    let t2 = ticket(&c);
    let k = key(0x19);
    begin_touched(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    for (step, stmt, params) in [
        ("s0", SET_TITLE, json!({ "id": x0, "t": "x0 by T" })),
        ("s1", CLOSE_TICKET, json!({ "t": t })),
        ("s2", SET_TITLE, json!({ "id": x2, "t": "x2 by T" })),
    ] {
        let params: Vec<(&str, Value)> = params.as_object().unwrap().iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        assert!(matches!(c.tranche_stage(&a, one(step, stmt, &params), 1_001), Ok(ChildOutcome::Staged { .. })));
    }
    let seq = anchor_seq(&c, "T");
    let hold = PoolHold::take(&dir, BETA);
    let (c1, a1) = (c.clone(), a.clone());
    let commit = std::thread::spawn(move || c1.tranche_commit(&a1, 1_002));
    poll_until(Duration::from_secs(10), "the flip", || state_of(&c, "T") == TrancheState::Landing);
    tick(&c, 1_003);
    // THE STALL: child 0 landed, child 1 decided and held, child 2 reserved.
    poll_until(Duration::from_secs(10), "child 0 landed and the lander stalled on beta", || {
        let r = c.tranche_heartbeat(&a, &QUIET, 1_003).unwrap();
        r.decided && r.landed_children == 1 && r.landing_error.as_ref().map(|e| e.why == "file_held").unwrap_or(false)
    });
    assert_eq!(state_of(&c, "T"), TrancheState::Landing);
    // (a) alpha: nothing decided-and-uncleared touches it — child 2 is
    // reserved UNDECIDED — so a reader of it is lock-free.
    let in_alpha = c.in_landing(Some(&PoolName::new(ALPHA))).unwrap();
    assert!(in_alpha.is_empty(), "child 2 is reserved undecided: {in_alpha:?}");
    let in_beta = c.in_landing(Some(&PoolName::new(BETA))).unwrap();
    assert_eq!(in_beta.len(), 1, "child 1 is decided and held: {in_beta:?}");
    for _ in 0..3 {
        let out = c.read(&req(CROSS_READ, &[("id", json!(z)), ("t", json!(t2))])).expect("a cross-pool read of untouched bodies during the landing");
        assert_eq!((out.rows.0[0]["d.title"].clone(), out.rows.0[0]["t.state"].clone()), (json!("z"), json!("open")));
        assert!(out.escalated, "beta's stuck landing invalidates the window; the read escalates and is GRANTED: {out:?}");
    }
    let out = c.read(&req(READ_TITLE, &[("id", json!(x0))])).expect("a read of the landed child's body");
    assert_eq!(out.rows.0[0]["d.title"], json!("x0 by T"), "per-child visibility");
    // (b) the stuck child's own body: the escalated read waits behind the
    // tranche's write on `t` to the ceiling.
    let started = Instant::now();
    busy_naming(c.read(&req(CROSS_READ, &[("id", json!(z)), ("t", json!(t))])), seq, "a read of the body the stuck child writes");
    assert!(started.elapsed() >= Duration::from_millis(400), "it escalated and waited out the ceiling: {:?}", started.elapsed());
    // Between children, in `touched` mode, a foreign writer on the LANDED
    // child's body is still busy: locks release at the clear, never per child.
    busy_naming(set_title(&c, "t19-x0", &x0, "foreign between children"), seq, "a foreign write to a landed child's body mid-landing");
    assert_eq!(title_of(&c, &x0), json!("x0 by T"));
    // Release; the scheduler retries after the backoff and lands.
    hold.release();
    let started = Instant::now();
    while !commit.is_finished() {
        tick(&c, 1_003 + 1_500 + started.elapsed().as_millis() as i64);
        assert!(started.elapsed() < Duration::from_secs(20), "the landing never finished");
        std::thread::sleep(Duration::from_millis(10));
    }
    let reply = commit.join().unwrap();
    assert!(matches!(reply, Ok(TrancheCommitted { state: TrancheState::Landed, .. }) | Err(SpineError::TrancheBusy { .. })), "{reply:?}");
    let started = Instant::now();
    poll_until(Duration::from_secs(20), "landed", || {
        tick(&c, 1_003 + 1_500 + started.elapsed().as_millis() as i64);
        state_of(&c, "T") == TrancheState::Landed
    });
    assert_eq!(c.read(&req(READ_TICKET, &[("t", json!(t))])).unwrap().rows.0[0]["t.state"], json!("closed"));
    // OPPOSED: after the landing the foreign write lands.
    committed(set_title(&c, "t19-x0-after", &x0, "foreign after"), "after landed");
}

// ---------------------------------------------------------------------------
// Test 23 — drain under a hydrate storm
// ---------------------------------------------------------------------------

/// Test 23 (N = 2: a STORM thread parking escalated-read rows — real
/// read rows with granted `read` locks, one every few ms, kept un-cleared —
/// beside the scheduler). A `whole` tranche at the head behind a parked
/// writer opens on the tick after the writer clears, while ≥ 1 granted
/// `read` row stands un-cleared (asserted at the moment of the open): the
/// drain waits for granted NON-`read` rows only, never for every un-cleared
/// row.
// RED (measured): `drained` counting every granted row (`read` too)
// → the head never opens while the storm runs. OPPOSED: the writer still
// parked, the same tick leaves it `draining` naming the writer's seq.
#[test]
fn t19_23_a_whole_head_opens_under_a_hydrate_storm_of_escalated_read_rows() {
    let dir = root("t23");
    let c = fresh(&dir);
    let z = doc(&c, "z", "draft");
    let zz = doc(&c, "zz", "draft");
    let writer = park(&dir, &claim(ALPHA, body(&z), LockMode::Write), "t23-writer");
    let k = key(0x23);
    let b = begin_whole(&c, "T", &k, 1_000);
    assert_eq!((b.state, b.position, b.draining), (TrancheState::Queued, 1, true));
    let stop = AtomicBool::new(false);
    let parked = std::sync::Mutex::new(Vec::<Seq>::new());
    std::thread::scope(|s| {
        let _release = SetOnDrop(&stop);
        let storm = s.spawn(|| {
            let mut n = 0u32;
            while !stop.load(Ordering::SeqCst) {
                let seq = park_read(&dir, body(&zz), &format!("t23-storm-{n}"));
                parked.lock().unwrap().push(seq);
                n += 1;
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        poll_until(Duration::from_secs(10), "the storm", || parked.lock().unwrap().len() >= 3);
        // OPPOSED: the writer parked → still draining, naming it.
        let r = tick(&c, 1_001);
        assert_eq!(r.draining, vec![("T".to_string(), 1, vec![writer.0])], "{r:?}");
        assert!(r.opened.is_empty());
        unpark(&dir, writer);
        let r = tick(&c, 1_002);
        let read_rows = count(&dir, "SELECT COUNT(*) FROM locks WHERE granted = 1 AND mode = 'read'", &[]);
        assert!(read_rows >= 3, "the storm's rows stand at the open: {read_rows}");
        assert_eq!(r.opened, vec![("T".to_string(), 1)], "opened under the storm: {r:?}");
        assert_eq!(state_of(&c, "T"), TrancheState::Open);
        stop.store(true, Ordering::SeqCst);
        storm.join().unwrap();
    });
    for seq in parked.into_inner().unwrap() {
        unpark(&dir, seq);
    }
    c.tranche_rollback(&actor("T", &k), 1_003).unwrap();
}

// ---------------------------------------------------------------------------
// g1 — the predicate's edges, on a second handle
// ---------------------------------------------------------------------------

/// g1, on two handles (N = 2 handles, the second a fresh
/// `Cauldron::open` on the same directory — no process state shared but the
/// table). No gate from: a `touched` open tranche; a `whole` tranche queued
/// NOT at the head; a `whole` head `queued` with `draining = 0` (before the
/// tick). The gate from: the head once the tick raised `draining` — read by
/// the second handle too; and from `open`. Both invariants the predicate
/// leans on asserted on the table: at most one non-`queued` live tranche,
/// and a `queued, draining = 1` row only ever at the head.
// RED (measured): the predicate widened to every non-terminal
// `whole` tranche (`state IN (live)`) → the writer is refused while T2 is
// queued at position 2. OPPOSED: the same writer lands once T2 rolls back.
#[test]
fn g1_the_gate_is_table_state_up_only_for_the_head_draining_or_open_and_read_the_same_by_a_second_handle() {
    let dir = root("g1");
    let c = fresh(&dir);
    let second = Cauldron::open(&dir, cfg()).expect("a second handle");
    let x = doc(&c, "x", "draft");
    let (k1, k2) = (key(0x01), key(0x02));
    // A touched tranche open: no gate.
    assert_eq!(begin_touched(&c, "T1", &k1, 1_000).state, TrancheState::Open);
    committed(set_title(&second, "g1-a", &x, "a"), "under a touched tranche");
    // A whole tranche queued at position 2: no gate.
    let b2 = begin_whole(&c, "T2", &k2, 1_001);
    assert_eq!((b2.state, b2.position, b2.draining), (TrancheState::Queued, 2, false));
    committed(set_title(&second, "g1-b", &x, "b"), "behind a queued whole tranche");
    // T1 ends; T2 is the head, `draining = 0` until the tick: still no gate.
    c.tranche_rollback(&actor("T1", &k1), 1_002).unwrap();
    let hb = c.tranche_heartbeat(&actor("T2", &k2), &QUIET, 1_003).unwrap();
    assert_eq!((hb.state, hb.position, hb.draining), (TrancheState::Queued, Some(1), false));
    committed(set_title(&second, "g1-c", &x, "c"), "the head not yet draining");
    // The tick under a NOT-quiet bus: the quiet start is evaluated BEFORE
    // the gate rises — still no gate, `quiet` shown, never `draining`.
    let r = tick_with(&c, &NOT_QUIET, 1_004);
    assert!(r.opened.is_empty() && r.draining.is_empty(), "{r:?}");
    let hb = c.tranche_heartbeat(&actor("T2", &k2), &NOT_QUIET, 1_005).unwrap();
    assert!(hb.quiet.is_some() && !hb.draining, "{hb:?}");
    committed(set_title(&second, "g1-d", &x, "d"), "during the quiet wait");
    // A parked writer, then the quiet tick: the gate rises (draining = 1)
    // and the SECOND handle reads it off the table.
    let writer = park(&dir, &claim(ALPHA, body(&x), LockMode::Write), "g1-writer");
    let r = tick(&c, 1_006);
    assert_eq!(r.draining, vec![("T2".to_string(), 1, vec![writer.0])], "{r:?}");
    let hb = second.tranche_heartbeat(&actor("T2", &k2), &QUIET, 1_007).unwrap();
    assert!(hb.draining && hb.quiet.is_none(), "draining, never beside quiet: {hb:?}");
    expect_locked(set_title(&second, "g1-e", &x, "e"), "the second handle under the draining head");
    expect_locked(set_title(&c, "g1-f", &x, "f"), "the first handle too");
    // The two table invariants.
    assert!(live_non_queued(&dir) <= 1);
    let (draining_rows, head_seq, draining_seq): (i64, i64, Option<i64>) = {
        let sql = spine_sql(&dir);
        (
            sql.query_row("SELECT COUNT(*) FROM tranches WHERE state = 'queued' AND draining = 1", [], |r| r.get(0)).unwrap(),
            sql.query_row("SELECT MIN(seq) FROM tranches WHERE state IN ('queued','open','staging','landing')", [], |r| r.get(0)).unwrap(),
            sql.query_row("SELECT seq FROM tranches WHERE state = 'queued' AND draining = 1", [], |r| r.get(0)).unwrap(),
        )
    };
    assert_eq!((draining_rows, draining_seq), (1, Some(head_seq)), "a draining row is only ever the head");
    unpark(&dir, writer);
    assert_eq!(tick(&c, 1_008).opened, vec![("T2".to_string(), 1)]);
    assert_eq!(live_non_queued(&dir), 1);
    expect_locked(set_title(&second, "g1-g", &x, "g"), "under the open whole tranche");
    // OPPOSED: the tranche ends, the writer lands.
    c.tranche_rollback(&actor("T2", &k2), 1_009).unwrap();
    committed(set_title(&second, "g1-g", &x, "g"), "after the rollback");
}

// ---------------------------------------------------------------------------
// g3 — the switches keep every slice claim
// ---------------------------------------------------------------------------

const FILTERED_SCAN: &str = r#"MATCH (e: fixture-alpha/doc { state: "draft" }) SET e.title = "reviewed""#;

/// A filtered-scan child stages in `whole`; `lock → touched → whole`; the
/// `sslice:doc:draft read` row is present after BOTH switches with its key
/// and mode unchanged — the whole lock table is byte-equal across each
/// switch. N = 2: an outside writer whose `link` on that slice is `locked`
/// under `whole` and `busy` naming the tranche under `touched` — the same
/// slice doing the work in both modes. OPPOSED: a tranche whose child
/// names no state holds no `sslice` row in either mode.
// RED (measured): `tranche_lock` releasing the tranche's rows on a
// switch → the slice row is missing after `touched` and the writer lands.
#[test]
fn g3_lock_switches_keep_every_slice_claim_and_the_lock_table_is_unchanged_across_both_directions() {
    let dir = root("g3");
    let c = fresh(&dir);
    let e1 = doc(&c, "e1", "draft");
    let p = doc(&c, "p", "published");
    let k = key(0x03);
    assert_eq!(begin_whole(&c, "T", &k, 1_000).state, TrancheState::Open);
    let a = actor("T", &k);
    assert!(matches!(c.tranche_stage(&a, one("s0", FILTERED_SCAN, &[]), 1_001), Ok(ChildOutcome::Staged { .. })));
    let seq = anchor_seq(&c, "T");
    let slice = |t: &[(String, String, String, bool)]| t.iter().any(|(_, r, m, g)| r == "sslice:doc:draft" && m == "read" && *g);
    let t0 = c.tranche_touched("T", 1).unwrap();
    assert!(slice(&t0), "under whole the scanned slice is held: {t0:?}");
    let into_draft = |c: &Cauldron, nonce: &str| keyless(c, nonce, r#"MATCH (d: fixture-alpha/doc { id: $a }) SET d.state = "draft""#, &[("a", json!(p))]);
    expect_locked(into_draft(&c, "g3-w1"), "under whole: the gate");
    let s1 = c.tranche_lock(&a, TrancheLockMode::Touched, 1_002).unwrap();
    assert!(s1.switched && s1.lock_mode == TrancheLockMode::Touched);
    let t1 = c.tranche_touched("T", 1).unwrap();
    assert_eq!(t0, t1, "whole → touched changes no lock row");
    busy_naming(into_draft(&c, "g3-w2"), seq, "under touched: the slice row");
    let s2 = c.tranche_lock(&a, TrancheLockMode::Whole, 1_003).unwrap();
    assert!(s2.switched && s2.lock_mode == TrancheLockMode::Whole);
    let t2 = c.tranche_touched("T", 1).unwrap();
    assert_eq!(t1, t2, "touched → whole changes no lock row");
    assert!(slice(&t2));
    expect_locked(into_draft(&c, "g3-w3"), "under whole again");
    c.tranche_rollback(&a, 1_004).unwrap();
    assert_eq!(title_of(&c, &e1), json!("e1"));
    // OPPOSED: a child that names no state holds no slice, in either mode.
    let k2 = key(0x04);
    begin_whole(&c, "T2", &k2, 1_005);
    let a2 = actor("T2", &k2);
    assert!(matches!(c.tranche_stage(&a2, one("s0", SET_TITLE, &[("id", json!(e1)), ("t", json!("plain"))]), 1_006), Ok(ChildOutcome::Staged { .. })));
    c.tranche_lock(&a2, TrancheLockMode::Touched, 1_007).unwrap();
    let t = c.tranche_touched("T2", 1).unwrap();
    assert!(!t.iter().any(|(_, r, _, _)| r.starts_with("sslice:")), "{t:?}");
    c.tranche_rollback(&a2, 1_008).unwrap();
}

// ---------------------------------------------------------------------------
// g4 — the drain is tick-driven; enforcement runs on the same tick
// ---------------------------------------------------------------------------

/// The drain is tick-driven (N = 3: a parked foreign writer, a `whole` head T1 past its quiet
/// start, a queued `touched` T2 whose deadline has lapsed). ONE tick, on a
/// detached thread bounded at 10 s: it returns within 4 × `lock_poll_interval`
/// (100 ms here → 400 ms; a tick that WAITED for the writer would not return
/// until the unpark), T1 reads `queued, draining: true` naming the writer,
/// and T2 is `abandoned` ON THAT SAME TICK. The writer clears; the next tick
/// opens T1. OPPOSED: the writer never parked, the FIRST tick opens T1.
// RED (measured): the drain made a LOOP inside the tick (wait for
// `drained` to empty) → the tick does not return within the bound and T2 is
// not abandoned within it.
#[test]
fn g4_the_drain_is_tick_driven_and_enforcement_runs_on_the_same_tick() {
    for parked in [true, false] {
        let dir = root(if parked { "g4" } else { "g4-free" });
        let mut config = cfg();
        config.lock_poll_interval = Duration::from_millis(100);
        config.tranche.timeout = Duration::from_millis(1_000);
        config.tranche.heartbeat_interval = Duration::from_millis(300);
        let c = fresh_with(&dir, config);
        let x = doc(&c, "x", "draft");
        let writer = parked.then(|| park(&dir, &claim(ALPHA, body(&x), LockMode::Write), "g4-writer"));
        let (k1, k2) = (key(0x41), key(0x42));
        // T1 at the head, queued under a NOT-quiet bus (so the quiet start,
        // not the drain, holds it); T2 behind it.
        let b1 = begin_as(&c, "T1", &k1, TrancheLockMode::Whole, &NOT_QUIET, 1_000);
        assert_eq!((b1.state, b1.draining), (TrancheState::Queued, false));
        let b2 = begin_touched(&c, "T2", &k2, 1_000);
        assert_eq!((b2.state, b2.position), (TrancheState::Queued, 2));
        // T1 beats; T2 never does: at 3_000 T2's deadline (2_000) has lapsed.
        c.tranche_heartbeat(&actor("T1", &k1), &NOT_QUIET, 2_900).unwrap();
        let c2 = c.clone();
        let started = Instant::now();
        let r = bounded(10, "the tick", move || tick(&c2, 3_000));
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_millis(400), "the tick never waits (4 × the 100 ms poll): {elapsed:?} ({r:?})");
        assert_eq!(r.abandoned, vec![("T2".to_string(), 1)], "T2 abandoned on the same tick: {r:?}");
        if let Some(w) = writer {
            assert_eq!(r.draining, vec![("T1".to_string(), 1, vec![w.0])], "{r:?}");
            assert!(r.opened.is_empty());
            let hb = c.tranche_heartbeat(&actor("T1", &k1), &QUIET, 3_001).unwrap();
            assert!(hb.draining && hb.state == TrancheState::Queued && hb.position == Some(1), "{hb:?}");
            expect_locked(set_title(&c, "g4-w", &x, "w"), "the gate is up while draining");
            unpark(&dir, w);
            assert_eq!(tick(&c, 3_002).opened, vec![("T1".to_string(), 1)]);
        } else {
            assert_eq!(r.opened, vec![("T1".to_string(), 1)], "OPPOSED: nothing to drain, the first tick opens: {r:?}");
        }
        assert_eq!(state_of(&c, "T1"), TrancheState::Open);
        assert_eq!(state_of(&c, "T2"), TrancheState::Abandoned);
        c.tranche_rollback(&actor("T1", &k1), 3_003).unwrap();
    }
}

// ---------------------------------------------------------------------------
// g5 — `begin` opens inline when drained, raises `draining` in its own
// transaction otherwise
// ---------------------------------------------------------------------------

/// Opening inline when drained (N = 2). (a) Nothing granted: `begin whole` answers `open`. (b) A
/// parked granted writer: `begin whole` answers `queued, position 1,
/// draining: true`, and an ordinary writer submitted immediately after — NO
/// TICK between — is refused `locked` at checkpoint 1 with no row (its
/// hash's row count stays 0). OPPOSED: `begin touched` with the same parked
/// writer answers `open` and the ordinary writer on another body lands.
// RED (measured): `draining` set only by the tick (`begin` leaves
// it 0) → the ordinary writer lands between `begin` and the tick.
#[test]
fn g5_begin_opens_inline_when_drained_and_raises_draining_in_its_own_transaction() {
    let dir = root("g5");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let y = doc(&c, "y", "draft");
    // (a)
    let b = begin_whole(&c, "Ta", &key(0x51), 1_000);
    assert_eq!((b.state, b.draining), (TrancheState::Open, false));
    c.tranche_rollback(&actor("Ta", &key(0x51)), 1_001).unwrap();
    // (b)
    let writer = park(&dir, &claim(ALPHA, body(&x), LockMode::Write), "g5-writer");
    let b = begin_whole(&c, "Tb", &key(0x52), 1_002);
    assert_eq!((b.state, b.position, b.draining), (TrancheState::Queued, 1, true), "{b:?}");
    let env = outside("g5-w", SET_TITLE, &[("id", json!(y)), ("t", json!("w"))], Mode::Commit);
    let hash = env.hash_identity().unwrap();
    expect_locked(c.submit(env), "an ordinary writer right after begin, no tick between");
    assert_eq!(rows_by_hash(&dir, &hash), 0, "refused at checkpoint 1: no row");
    let hb = c.tranche_heartbeat(&actor("Tb", &key(0x52)), &QUIET, 1_003).unwrap();
    assert!(hb.draining && hb.quiet.is_none(), "{hb:?}");
    c.tranche_rollback(&actor("Tb", &key(0x52)), 1_004).unwrap();
    // OPPOSED
    let b = begin_touched(&c, "Tc", &key(0x53), 1_005);
    assert_eq!((b.state, b.draining), (TrancheState::Open, false));
    committed(set_title(&c, "g5-w2", &y, "w2"), "under touched with the writer parked");
    c.tranche_rollback(&actor("Tc", &key(0x53)), 1_006).unwrap();
    unpark(&dir, writer);
}

// ---------------------------------------------------------------------------
// g6 — FCFS at N = 2; an outside writer lands only between tranches
// ---------------------------------------------------------------------------

/// Whole tranches are first come, first served (N = 3: two `whole` tranches and outside writers).
/// T1 `whole` open; T2 `whole` begins → `queued, position 2` (no gate of its
/// own); W → `locked`. T1 `rollback` → the gate is down in the same
/// transaction: W lands AT ONCE, before any tick. A tick under a NOT-quiet
/// bus: T2 stays `queued`, not `draining`, `quiet` shown; W2 lands (the
/// quiet window admits ordinary writers "before the gate rises").
/// The quiet tick → T2 `open` (nothing to drain); W3 → `locked`. Never two
/// non-`queued` live tranches.
// RED (measured): (1) the queue advancing on the first `queued` row
// rather than the smallest live seq → T2 opens while T1 is live, two open
// tranches; (2) `draining` set at `head_since` → W2 is refused during the
// quiet wait (test 22's last clause).
#[test]
fn g6_whole_is_fcfs_at_n_2_and_an_outside_writer_lands_only_between_tranches() {
    let dir = root("g6");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let (k1, k2) = (key(0x61), key(0x62));
    assert_eq!(begin_whole(&c, "T1", &k1, 1_000).state, TrancheState::Open);
    let b2 = begin_whole(&c, "T2", &k2, 1_001);
    assert_eq!((b2.state, b2.position, b2.draining), (TrancheState::Queued, 2, false));
    assert_eq!(b2.ahead.as_ref().map(|a| a.tranche.as_str()), Some("T1"));
    let (t, n) = expect_locked(set_title(&c, "g6-w", &x, "w"), "W under T1");
    assert_eq!((t.as_str(), n), ("T1", 1));
    // A tick while T1 is live: T2 does not move (red 1 is here).
    let r = tick(&c, 1_002);
    assert!(r.opened.is_empty() && r.draining.is_empty(), "{r:?}");
    assert_eq!(live_non_queued(&dir), 1, "one non-queued live tranche at a time");
    c.tranche_rollback(&actor("T1", &k1), 1_003).unwrap();
    committed(set_title(&c, "g6-w", &x, "w"), "W lands at once after T1's rollback, before any tick");
    let r = tick_with(&c, &NOT_QUIET, 1_004);
    assert!(r.opened.is_empty() && r.draining.is_empty(), "{r:?}");
    let hb = c.tranche_heartbeat(&actor("T2", &k2), &NOT_QUIET, 1_005).unwrap();
    assert!(hb.state == TrancheState::Queued && !hb.draining && hb.quiet.is_some(), "{hb:?}");
    committed(set_title(&c, "g6-w2", &x, "w2"), "W2 lands during the quiet wait");
    let r = tick(&c, 1_006);
    assert!(r.opened == vec![("T2".to_string(), 1)] || !r.draining.is_empty(), "{r:?}");
    poll_until(Duration::from_secs(5), "T2 open", || {
        if state_of(&c, "T2") == TrancheState::Open {
            true
        } else {
            tick(&c, 1_007);
            false
        }
    });
    expect_locked(set_title(&c, "g6-w3", &x, "w3"), "W3 under T2");
    assert_eq!(live_non_queued(&dir), 1);
    c.tranche_rollback(&actor("T2", &k2), 1_008).unwrap();
    committed(set_title(&c, "g6-w3", &x, "w3"), "W3 after T2");
}

// ---------------------------------------------------------------------------
// g7 — a switch that outlasts the heartbeat timeout is not abandoned
// ---------------------------------------------------------------------------

/// A long switch to whole is not abandoned (N = 2; period named: the heartbeat TIMEOUT, 300 ms). A parked writer
/// is released on a thread at 600 ms; `lock → whole` (ceiling 2 s) returns
/// `switched: true` after ≥ 600 ms — every drain poll a beat — so an
/// immediate `tranche_tick` at a clock past the ORIGINAL deadline leaves
/// the tranche `open, whole`. OPPOSED: no parked writer — the switch
/// returns at once and the deadline moved anyway (`last_heartbeat` ≥ the
/// act's `now_ms`).
// RED (measured): the drain's deadline refresh dropped → the tick
// after the switch ABANDONS the tranche (`heartbeat_lost`).
#[test]
fn g7_a_switch_to_whole_that_outlasts_the_300ms_heartbeat_timeout_is_not_abandoned_by_the_next_tick() {
    let dir = root("g7");
    let mut config = cfg();
    config.tranche.timeout = Duration::from_millis(300);
    config.tranche.heartbeat_interval = Duration::from_millis(100);
    config.lock_wait_ceiling = Duration::from_secs(2);
    let c = fresh_with(&dir, config);
    let x = doc(&c, "x", "draft");
    let writer = park(&dir, &claim(ALPHA, body(&x), LockMode::Write), "g7-writer");
    let k = key(0x71);
    begin_touched(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    let dir2 = dir.clone();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(600));
        unpark(&dir2, writer);
    });
    let started = Instant::now();
    let switched = c.tranche_lock(&a, TrancheLockMode::Whole, 1_000).expect("the switch drains");
    let elapsed = started.elapsed();
    assert!(switched.switched && elapsed >= Duration::from_millis(600), "{switched:?} after {elapsed:?}");
    releaser.join().unwrap();
    // The ORIGINAL deadline was 1_300; the switch ran past it on the clock
    // it refreshes from (1_000 + elapsed ≥ 1_600): a tick at 1_500 finds the
    // deadline moved.
    let r = tick(&c, 1_500);
    assert!(r.abandoned.is_empty(), "a beating switch is not abandoned: {r:?}");
    let hb = c.tranche_heartbeat(&a, &QUIET, 1_501).unwrap();
    assert_eq!((hb.state, hb.lock_mode), (TrancheState::Open, TrancheLockMode::Whole));
    assert!(hb.deadline > 1_500, "{hb:?}");
    c.tranche_rollback(&a, 1_502).unwrap();
    // OPPOSED: nothing parked.
    let k2 = key(0x72);
    begin_touched(&c, "T2", &k2, 2_000);
    let a2 = actor("T2", &k2);
    let started = Instant::now();
    assert!(c.tranche_lock(&a2, TrancheLockMode::Whole, 2_100).unwrap().switched);
    assert!(started.elapsed() < Duration::from_millis(1_000), "at once (the ceiling is 2 s): {:?}", started.elapsed());
    let hb = c.tranche_heartbeat(&a2, &QUIET, 2_101).unwrap();
    assert!(hb.last_heartbeat >= 2_100, "the deadline moved anyway: {hb:?}");
    c.tranche_rollback(&a2, 2_102).unwrap();
}

// ---------------------------------------------------------------------------
// g8 — the tranche's own children land under the gate
// ---------------------------------------------------------------------------

/// The exemption (N = 2: the holder and an outside writer, interleaved —
/// the degraded-regime population in ONE test: at the same
/// instant the tranche's child stages, the outside writer IS refused). A
/// `whole` tranche stages two children while the writer is `locked`;
/// `commit` → `landed`, both children's rows cleared, the writer's retry
/// lands after. OPPOSED: in `touched` mode the same tranche lands too (and
/// the writer on an untouched body lands beside it).
// RED (measured): `seq != holder` dropped from checkpoint 3 (the
// holder's own claims meet the gate) → the tranche's child is refused
// `locked` at staging. OPPOSED (also forced): checkpoint 2 applied to the
// reservation path → the landing refuses itself.
#[test]
fn g8_the_tranches_own_children_stage_and_land_under_the_gate_while_an_outside_writer_is_refused() {
    let dir = root("g8");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let y = doc(&c, "y", "draft");
    let z = doc(&c, "z", "draft");
    for (mode, k, tag) in [(TrancheLockMode::Whole, 0x81u8, "whole"), (TrancheLockMode::Touched, 0x82, "touched")] {
        let t = format!("T{tag}");
        assert_eq!(begin_as(&c, &t, &key(k), mode, &QUIET, 1_000).state, TrancheState::Open);
        let a = actor(&t, &key(k));
        assert!(matches!(c.tranche_stage(&a, one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!(format!("x {tag}")))]), 1_001), Ok(ChildOutcome::Staged { .. })));
        let outside_z = set_title(&c, &format!("g8-z-{tag}"), &z, &format!("z {tag}"));
        if mode == TrancheLockMode::Whole {
            expect_locked(outside_z, "the outside writer at the same instant");
        } else {
            committed(outside_z, "OPPOSED: touched admits the untouched body");
        }
        assert!(matches!(c.tranche_stage(&a, one("s1", SET_TITLE, &[("id", json!(y)), ("t", json!(format!("y {tag}")))]), 1_002), Ok(ChildOutcome::Staged { .. })));
        let reply = land(&c, &a, 1_003).expect("the tranche lands under its own gate");
        assert_eq!((reply.state, reply.children.len()), (TrancheState::Landed, 2));
        for ch in &reply.children {
            let row = c.transaction(Seq(ch.seq)).unwrap();
            assert!(row.decided && row.cleared, "child {} cleared", ch.ordinal);
        }
        assert_eq!(title_of(&c, &x), json!(format!("x {tag}")));
        committed(set_title(&c, &format!("g8-z-{tag}-retry"), &z, &format!("z {tag} retry")), "a writer after the landing");
    }
}

// ---------------------------------------------------------------------------
// g9 — a checkpoint-3 refusal aborts the writer's row and never restarts
// ---------------------------------------------------------------------------

/// The tranche's own children land under the gate (N = 2: the writer in flight beside the holder). W appended BEFORE the
/// gate rose and queues behind a parked escalated read's `read` row on `x`
/// — THE OVERLAP: W's queued `write` row observed while the read holds. The
/// `whole` tranche opens inline (reads do not count against the drain); W's
/// next poll is `locked`: exactly ONE row for W's hash, `decided = 0,
/// cleared = 1`; no lock row under W's seq; W's reply the busy, and the row
/// count by hash is STILL 1 after W returns (no second append). After the
/// tranche ends, the next writer on `x` lands — nothing of W's dams it.
// RED (measured): (1) `TrancheBusy` treated as a restart signal in
// `execute` → a second row by hash; (2) `withdraw_queued` skipped on the
// poll-error path → W's queued row dams the next writer on `x` after the
// tranche ends (its `busy` names W's seq).
#[test]
fn g9_a_checkpoint_3_refusal_aborts_the_writers_row_withdraws_its_queue_and_never_restarts() {
    let dir = root("g9");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let reader = park(&dir, &claim(ALPHA, body(&x), LockMode::Read), "g9-reader");
    let env = outside("g9-w", SET_TITLE, &[("id", json!(x)), ("t", json!("by W"))], Mode::Commit);
    let hash = env.hash_identity().unwrap();
    let (c2, env2) = (c.clone(), env.clone());
    let w = std::thread::spawn(move || c2.submit(env2));
    let w_seq = await_lock_row(&dir, ALPHA, &format!("body:{x}"), "write", false, &[reader]);
    let k = key(0x91);
    assert_eq!(begin_whole(&c, "T", &k, 1_000).state, TrancheState::Open, "reads do not count against the drain");
    expect_locked(bounded(10, "W", move || w.join().unwrap()), "W at its next poll");
    let (decided, cleared): (i64, i64) = spine_sql(&dir)
        .query_row("SELECT decided, cleared FROM transactions WHERE hash = ?1", [hash.to_hex()], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!((rows_by_hash(&dir, &hash), decided, cleared), (1, 0, 1), "ONE row, aborted");
    assert_eq!(lock_rows_under(&dir, w_seq), 0, "W's queued rows withdrawn");
    assert_eq!(rows_by_hash(&dir, &hash), 1, "no second append after W returned");
    unpark(&dir, reader);
    c.tranche_rollback(&actor("T", &k), 1_001).unwrap();
    // Nothing of W's dams the resource: the next writer on x lands at once.
    committed(set_title(&c, "g9-next", &x, "next"), "the next writer on x (a queued row of W's would make it busy at the ceiling)");
    committed(c.submit(env), "W's own retry");
}

// ---------------------------------------------------------------------------
// g10 — two tranches of one service queued behind each other are logged
// ---------------------------------------------------------------------------

/// The same-service tripwire (N = 2; period named: the warn INTERVAL, 200 ms here). T1
/// `whole` open and T2 of the SAME `holder_service` queued behind it: the
/// tick emits exactly ONE `warn` line naming T2 behind T1 per interval —
/// one on the first tick, none on a tick inside the interval, one on a tick
/// past it. OPPOSED: T3 of a DIFFERENT service queued behind T1 → no line
/// for it.
// RED (measured): the tripwire removed → zero lines after two
// intervals.
#[test]
fn g10_two_tranches_of_one_service_queued_behind_each_other_are_logged_once_per_200ms_interval() {
    let dir = root("g10");
    let mut config = cfg();
    config.tranche.same_service_warn_interval = Duration::from_millis(200);
    let c = fresh_with(&dir, config);
    let (k1, k2, k3) = (key(0xa1), key(0xa2), key(0xa3));
    assert_eq!(begin_whole(&c, "T1", &k1, 1_000).state, TrancheState::Open);
    assert_eq!(begin_whole(&c, "T2", &k2, 1_001).state, TrancheState::Queued);
    let mut other = begin_req("T3", &k3, TrancheLockMode::Touched);
    other.holder_service = "other-service".into();
    assert_eq!(c.tranche_begin(&other, &QUIET, 1_002).unwrap().state, TrancheState::Queued);
    let names = |r: &TickReport| -> Vec<String> {
        r.warnings.iter().filter(|w| w.contains("of the same service")).cloned().collect()
    };
    let r1 = names(&tick(&c, 1_003));
    assert_eq!(r1.len(), 1, "one line on the first tick: {r1:?}");
    assert!(r1[0].contains("tranche T2 queued behind T1"), "{r1:?}");
    assert!(!r1[0].contains("T3"), "OPPOSED: another service is not a nested run: {r1:?}");
    let r2 = names(&tick(&c, 1_050));
    assert!(r2.is_empty(), "inside the interval: {r2:?}");
    let r3 = names(&tick(&c, 1_003 + 400));
    assert_eq!(r3.len(), 1, "one line past the interval: {r3:?}");
    assert!(r3[0].contains("tranche T2 queued behind T1"));
    for (t, k) in [("T1", &k1), ("T2", &k2)] {
        c.tranche_rollback(&actor(t, k), 1_500).unwrap();
    }
    c.tranche_rollback(&TrancheActor { tranche: "T3".into(), key: k3, holder_service: "other-service".into() }, 1_501).unwrap();
}

// ---------------------------------------------------------------------------
// g11 — quiesce mid-tranche times out naming the anchor
// ---------------------------------------------------------------------------

fn tranches_dump(dir: &Path) -> Vec<String> {
    let sql = spine_sql(dir);
    let mut stmt = sql.prepare("SELECT id, attempt, seq, state, lock_mode, holder_service, key_hash, regions, began_at, last_heartbeat, deadline, head_since, draining, decided, renumbered, landed_children, landing_error, ended_at, reason, refusal, staged FROM tranches ORDER BY seq").unwrap();
    let n = stmt.column_count();
    stmt.query_map([], |r| {
        let mut cols = Vec::new();
        for i in 0..n {
            cols.push(format!("{:?}", r.get::<_, rusqlite::types::Value>(i)?));
        }
        Ok(cols.join("|"))
    })
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

/// Quiesce mid-tranche (N = 2: the holder BEATING on one thread while `quiesce()` runs on
/// another). The anchor is un-cleared for the tranche's life, so the quiesce
/// times out at the lock ceiling and its error NAMES the anchor's `seq` and
/// the tranche `(T, n)`; the quiescer still stands (its token comes back)
/// and is resumed. After `rollback` the same quiesce drains at once.
/// `resume_all` changes no `tranches` row (the table's rows byte-equal
/// before and after).
// RED (measured): `quiesce` skipping rows with `tranche_id` set →
// it reports drained while the tranche is open.
#[test]
fn g11_quiesce_mid_tranche_times_out_naming_the_anchor_and_drains_after_rollback_while_resume_all_touches_no_tranche_row() {
    let dir = root("g11");
    let c = fresh(&dir);
    let k = key(0xb1);
    begin_touched(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    let seq = anchor_seq(&c, "T");
    let stop = AtomicBool::new(false);
    let beats = std::sync::atomic::AtomicU32::new(0);
    std::thread::scope(|s| {
        let _release = SetOnDrop(&stop);
        let beater = s.spawn(|| {
            let mut now = 1_001;
            while !stop.load(Ordering::SeqCst) {
                c.tranche_heartbeat(&a, &QUIET, now).expect("beat");
                beats.fetch_add(1, Ordering::SeqCst);
                now += 50;
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let started = Instant::now();
        let err = match c.quiesce() {
            Ok(_) => panic!("a quiesce drained while a tranche is open"),
            Err(e) => e,
        };
        assert!(started.elapsed() >= Duration::from_millis(400), "it waited out the ceiling");
        let msg = err.to_string();
        assert!(msg.contains(&format!("anchor row seq {}", seq.0)), "names the anchor's seq: {msg}");
        assert!(msg.contains("tranche T attempt 1 is open"), "names (T, n): {msg}");
        let token = err.standing.expect("the quiescer still stands; the token comes back");
        assert!(beats.load(Ordering::SeqCst) >= 3, "the holder beat throughout");
        assert!(c.resume(&token).unwrap());
        stop.store(true, Ordering::SeqCst);
        beater.join().unwrap();
    });
    assert_eq!(state_of(&c, "T"), TrancheState::Open, "a timed-out quiesce ended nothing");
    c.tranche_rollback(&a, 2_000).unwrap();
    let token = c.quiesce().expect("after the rollback the anchor is cleared and the quiesce drains");
    let before = tranches_dump(&dir);
    let lifted = c.resume_all("g11: the by-hand lift touches no tranche").unwrap();
    assert_eq!(lifted.lifted.len(), 1);
    assert_eq!(tranches_dump(&dir), before, "resume_all changed no tranches row");
    assert!(!c.resume(&token).unwrap(), "already lifted");
}

// ---------------------------------------------------------------------------
// g12 — read-only claim sets and `validate` pass the gate
// ---------------------------------------------------------------------------

/// Readers beside a landing in progress (N = 2: the holder and the outside reader/writer).
/// A decided, un-cleared row touching beta stands (a landing in progress as
/// `in_landing` sees it — parked before the gate rises; it holds no lock).
/// Under an open `whole` tranche: a cross-pool read of `x` and a ticket is
/// invalidated by it and ESCALATES to real `read` locks — a read-only claim
/// set with a spine row — which checkpoints 2 and 3 pass: granted, rows; a
/// `validate` submit answers `validated`; a `mechanical` write
/// (`CoreMechanical`) and an agent's write of the same shape are refused
/// `locked` like any writer.
// RED (measured): checkpoint 3 refusing on ANY claim set (the
// all-`read` exemption dropped) → the escalated read is `locked`. OPPOSED:
// the writers with the same statement shape are refused.
#[test]
fn g12_read_only_claim_sets_and_validate_pass_the_gate_while_mechanical_and_agent_writes_are_refused() {
    let dir = root("g12");
    let mut config = cfg();
    config.retry_bound = 0;
    let c = fresh_with(&dir, config);
    let x = doc(&c, "x", "draft");
    let t = ticket(&c);
    let t2 = ticket(&c);
    let decided = park_decided(&dir, BETA, fixtures::fixture_beta().version, &t, "g12");
    assert_eq!(c.in_landing(Some(&PoolName::new(BETA))).unwrap().len(), 1);
    let k = key(0xc1);
    assert_eq!(begin_whole(&c, "T", &k, 1_000).state, TrancheState::Open, "a decided row holding no lock is not drained for");
    let out = c.read(&req(CROSS_READ, &[("id", json!(x)), ("t", json!(t2))])).expect("an escalated read under the gate");
    assert!(out.escalated, "beta's in-landing row invalidates the window; retry_bound 0 escalates: {out:?}");
    assert_eq!((out.rows.0[0]["d.title"].clone(), out.rows.0[0]["t.state"].clone()), (json!("x"), json!("open")));
    match c.submit(outside("g12-v", SET_TITLE, &[("id", json!(x)), ("t", json!("dry"))], Mode::Validate)) {
        Ok(Outcome::Validated { .. }) => {}
        other => panic!("validate under the gate: {other:?}"),
    }
    expect_locked(set_title(&c, "g12-m", &x, "mechanical"), "a mechanical write");
    expect_locked(c.submit(agent("g12-a", SET_TITLE, &[("id", json!(x)), ("t", json!("agent"))])), "an agent's write");
    assert_eq!(title_of(&c, &x), json!("x"));
    c.tranche_rollback(&actor("T", &k), 1_001).unwrap();
    let _ = decided;
}

// ---------------------------------------------------------------------------
// g13 — two landers in two processes decide once
// ---------------------------------------------------------------------------

/// g13's common ground, both shapes: a `touched` tranche `T` with two staged
/// children (titles on `x` and `y`), the holder's `commit` on a detached
/// thread, the state `landing`. The record every lander below starts from.
struct G13Landing {
    dir: PathBuf,
    c: Cauldron,
    x: String,
    y: String,
    k: String,
    a: TrancheActor,
    commit: std::thread::JoinHandle<spine::error::Result<TrancheCommitted>>,
}

fn g13_landing(name: &str) -> G13Landing {
    let dir = root(name);
    let mut config = cfg();
    config.busy_timeout = Duration::from_secs(10);
    let c = fresh_with(&dir, config);
    let x = doc(&c, "x", "draft");
    let y = doc(&c, "y", "draft");
    let k = key(0xd1);
    begin_touched(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    assert!(matches!(c.tranche_stage(&a, one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!("x by T"))]), 1_001), Ok(ChildOutcome::Staged { .. })));
    assert!(matches!(c.tranche_stage(&a, one("s1", SET_TITLE, &[("id", json!(y)), ("t", json!("y by T"))]), 1_002), Ok(ChildOutcome::Staged { .. })));
    let (c1, a1) = (c.clone(), a.clone());
    let commit = std::thread::spawn(move || c1.tranche_commit(&a1, 1_003));
    poll_until(Duration::from_secs(10), "the flip", || state_of(&c, "T") == TrancheState::Landing);
    G13Landing { dir, c, x, y, k, a, commit }
}

/// g13's record, read off `spine.db` from the side: (`decided`, reserved
/// child rows, cleared child rows).
fn g13_record(dir: &Path) -> (i64, i64, i64) {
    let sql = spine_sql(dir);
    (
        sql.query_row("SELECT decided FROM tranches WHERE id = 'T'", [], |r| r.get(0)).unwrap(),
        sql.query_row("SELECT COUNT(*) FROM transactions WHERE tranche_id = 'T' AND tranche_ordinal IS NOT NULL", [], |r| r.get(0)).unwrap(),
        sql.query_row("SELECT COUNT(*) FROM transactions WHERE tranche_id = 'T' AND tranche_ordinal IS NOT NULL AND cleared = 1", [], |r| r.get(0)).unwrap(),
    )
}

/// g13's tail, both shapes: landed ONCE — exactly one reserved row per
/// child, each child's statement exactly one row, both titles T's.
fn g13_landed_each_child_once(g: &G13Landing) {
    let (decided, reserved, cleared) = g13_record(&g.dir);
    assert_eq!((decided, reserved, cleared), (1, 2, 2), "one reserved row per child, each cleared");
    for (id, step, title) in [(&g.x, "s0", "x by T"), (&g.y, "s1", "y by T")] {
        assert_eq!(rows_by_hash(&g.dir, &one(step, SET_TITLE, &[("id", json!(id)), ("t", json!(title))]).hash_identity().unwrap()), 1);
        assert_eq!(title_of(&g.c, id), json!(title));
    }
}

/// THE DETERMINISTIC SHAPE, fault build only —
/// it needs the pause seam. The double landing decision is reachable only
/// ACROSS PROCESSES (in one, the registry and the mutex make a second lander
/// unreachable), and the race the decision's `AND decided = 0`
/// exists for has two sides: lander B read `decided = 0` at the decision, lander A
/// committed its decision, B's flip runs while A is still `landing`. The
/// pause seam holds BOTH sides open. Two child processes each tick once and
/// spawn a lander for the same `landing` tranche: B (`SPINE_PAUSE =
/// before-decision`) is parked past its read of `decided = 0`; then A
/// (`SPINE_PAUSE = after-decision`) decides alone and is parked past its
/// commit, before any child lands — THE OVERLAP, observed: both `.reached`
/// markers written, `decided = 1`, two reserved rows, none cleared, `x`/`y`
/// untouched, the state still `landing`, neither latch written. B is released
/// INTO that window: its conditional flip changes 0 rows, it takes the decided path from
/// the record and lands both children; A, released after B's verdict, finds
/// every row cleared and the clear nothing to do. Landed; exactly one reserved row
/// per child; the invariant tripwire ("already has a reserved row") never
/// fires. No [`SpineHold`], no sleep: every wait is for a marker or a state.
// RED (measured): the decision made UNCONDITIONALLY
// (`decided = 0` dropped from the flip)
// → B's flip matches the still-`landing` row, B decides again, meets A's
// reserved rows and fires the invariant tripwire — a `landing_error {why:
// internal}` naming the reserved row, read off the heartbeat BEFORE A is
// released (A's clear would clear it; A then lands from its own record, so
// the tranche still ends `landed` and the test reaches its tripwire
// assertion). 10/10 red; restored 10/10 green — where the hold shape below
// was red 1 in 9 (which lander wins the released write lock,
// and whether the loser's flip runs before the winner clears, was SQLite's
// busy handler's to decide).
#[cfg(feature = "fault-injection")]
#[test]
fn g13_two_landers_in_two_processes_decide_once_and_land_each_child_once() {
    let g = g13_landing("g13");
    // The latches live beside the cauldron, never inside it (fault.rs: "no
    // test in this crate places [the latch] inside a cauldron directory").
    let seams = root("g13-seams");
    let _ = std::fs::remove_dir_all(&seams);
    std::fs::create_dir_all(&seams).unwrap();
    let (latch_b, latch_a) = (seams.join("lander-b"), seams.join("lander-a"));
    // A panic before a release would park two children until their pause
    // ceiling: on the way out, released or not, both latches are written.
    struct ReleaseOnDrop(Vec<PathBuf>);
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            for l in &self.0 {
                let _ = std::fs::write(l, "released on exit");
            }
        }
    }
    let _release = ReleaseOnDrop(vec![latch_b.clone(), latch_a.clone()]);
    let reached = |latch: &Path| spine::fault::reached_path(latch).exists();
    let lander = |stage: &str, latch: &Path| {
        spawn_role(
            "tick",
            &g.dir,
            &[
                ("SPINE_LOCKS_T", "T".into()),
                ("SPINE_LOCKS_KEY", g.k.clone()),
                ("SPINE_LOCKS_NOW", "1004".into()),
                ("SPINE_LOCKS_BUSY_MS", "10000".into()),
                (spine::fault::PAUSE_ENV, stage.into()),
                (spine::fault::PAUSE_LATCH_ENV, latch.to_string_lossy().to_string()),
                // Long enough for every bounded wait below to fire first:
                // a pause nobody releases dies at ITS ceiling, loudly.
                (spine::fault::PAUSE_CEILING_ENV, "120000".into()),
            ],
        )
    };
    // B FIRST: parked past its read of `decided = 0`, before the
    // re-validation. The record is untouched.
    let b = lander("before-decision", &latch_b);
    poll_until(Duration::from_secs(30), "lander B at before-decision", || reached(&latch_b));
    assert_eq!(g13_record(&g.dir), (0, 0, 0), "B is parked before any decision");
    // THEN A: decides alone, parked past its commit and before any landing.
    let a = lander("after-decision", &latch_a);
    poll_until(Duration::from_secs(30), "lander A at after-decision", || reached(&latch_a));
    // THE OVERLAP: A decided, nothing landed; B past its read of `decided = 0`.
    assert_eq!(g13_record(&g.dir), (1, 2, 0), "A decided and landed nothing while B is parked past its read of decided = 0");
    assert_eq!(state_of(&g.c, "T"), TrancheState::Landing);
    assert_eq!((title_of(&g.c, &g.x), title_of(&g.c, &g.y)), (json!("x"), json!("y")), "no child landed");
    assert!(!latch_b.exists() && !latch_a.exists(), "neither lander has been released");
    // B RELEASED INTO THE WINDOW: its flip must change 0 rows.
    std::fs::write(&latch_b, "go").unwrap();
    // B's verdict — landed, or a landing error — read before A is released,
    // since A's clear clears `landing_error` (a beat, as `commit` is).
    let mut tripwire = Vec::new();
    poll_until(Duration::from_secs(30), "lander B's verdict", || {
        if state_of(&g.c, "T") == TrancheState::Landed {
            return true;
        }
        match g.c.tranche_heartbeat(&g.a, &QUIET, 1_005) {
            Ok(hb) => match hb.landing_error {
                Some(e) => {
                    tripwire.push(format!("{}: {}", e.why, e.message));
                    true
                }
                None => false,
            },
            Err(_) => false,
        }
    });
    // A RELEASED: from its own record — every row cleared — nothing twice.
    std::fs::write(&latch_a, "go").unwrap();
    poll_until(Duration::from_secs(30), "landed", || state_of(&g.c, "T") == TrancheState::Landed);
    let started = Instant::now();
    while !g.commit.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(30), "the holder's commit never answered");
        std::thread::sleep(Duration::from_millis(10));
    }
    let lines_b = role_lines(b, 20, "lander B's process");
    let lines_a = role_lines(a, 20, "lander A's process");
    assert!(tripwire.is_empty(), "a second decision was made: {tripwire:?}\nB: {lines_b:?}\nA: {lines_a:?}");
    for (who, lines) in [("B", &lines_b), ("A", &lines_a)] {
        assert!(lines.iter().any(|l| l.contains("role: tick") && l.contains("spawned=[(\"T\", 1)]")), "lander {who}'s process spawned its own lander: {lines:?}");
        assert!(lines.iter().any(|l| l.contains("final state=landed landing_error=none")), "lander {who}'s process saw the landing end clean: {lines:?}");
    }
    g13_landed_each_child_once(&g);
    let reply = g.commit.join().unwrap();
    assert!(matches!(reply, Ok(TrancheCommitted { state: TrancheState::Landed, .. })), "{reply:?}");
    let _ = std::fs::remove_dir_all(&seams);
}

/// THE HOLD SHAPE, default build (no pause seam there). The same double
/// decision across processes: two ticks, one in this process and one in a
/// child process, each spawn a lander for the same `landing` tranche while a
/// [`SpineHold`] holds the spine's write lock: both re-validate (reads) and
/// both block at the decision's `BEGIN IMMEDIATE` — THE OVERLAP: the child's
/// tick MARKER is written (its lander spawned) and `decided` still reads 0
/// while the hold stands (period named: 500 ms for both landers to reach the
/// decision). The hold releases: ONE decision; the second lander finds
/// `decided = 1`, takes the decided path from the record and lands nothing twice. Landed;
/// exactly one reserved row per child; the invariant tripwire ("already has
/// a reserved row") never fires.
// RED (measured; 1 run in 9): the decision made
// UNCONDITIONALLY (`decided = 0` dropped from the flip) → the second lander
// decides again, meets the first lander's reserved rows and fires the
// invariant tripwire — a `landing_error {why: internal}` whose message names
// the reserved row, seen by the parent's poller and in the child's final
// line. PROBABILISTIC here: once the hold releases, which lander SQLite's
// busy handler wakes first and whether the loser's flip runs before the
// winner clears is timing; the fault build's shape above holds both sides
// open and is the regression guard (10/10).
#[cfg(not(feature = "fault-injection"))]
#[test]
fn g13_two_landers_in_two_processes_decide_once_and_land_each_child_once() {
    let g = g13_landing("g13");
    let (dir, c, k, a, commit) = (&g.dir, &g.c, &g.k, &g.a, &g.commit);
    let (ready, go, mark) = (dir.join("child-ready"), dir.join("child-go"), dir.join("child-ticked"));
    let child = spawn_role(
        "tick",
        dir,
        &[
            ("SPINE_LOCKS_T", "T".into()),
            ("SPINE_LOCKS_KEY", k.clone()),
            ("SPINE_LOCKS_NOW", "1004".into()),
            ("SPINE_LOCKS_BUSY_MS", "10000".into()),
            ("SPINE_LOCKS_READY", ready.to_string_lossy().to_string()),
            ("SPINE_LOCKS_GO", go.to_string_lossy().to_string()),
            ("SPINE_LOCKS_MARK", mark.to_string_lossy().to_string()),
        ],
    );
    // The child is OPEN (its open writes the spine) before the hold is taken.
    poll_until(Duration::from_secs(30), "the child's open", || ready.exists());
    let hold = SpineHold::take(dir);
    std::fs::write(&go, "go").unwrap();
    let r = tick(c, 1_004);
    assert_eq!(r.landers_spawned, vec![("T".to_string(), 1)], "{r:?}");
    // THE HANDSHAKE: the child's tick has run (its marker), so its lander
    // exists; then both landers reach the decision and block on the hold
    // (the period outlasted: re-validation, milliseconds).
    poll_until(Duration::from_secs(30), "the child's tick", || mark.exists());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(g13_record(dir), (0, 0, 0), "undecided while the hold stands");
    hold.release();
    let mut tripwire = Vec::new();
    let started = Instant::now();
    while !commit.is_finished() {
        if let Ok(hb) = c.tranche_heartbeat(a, &QUIET, 1_005) {
            if let Some(e) = hb.landing_error {
                if e.message.contains("already has a reserved row") {
                    tripwire.push(e.message.clone());
                }
            }
        }
        tick(c, 1_005 + 2_000 + started.elapsed().as_millis() as i64);
        assert!(started.elapsed() < Duration::from_secs(30), "the landing never finished");
        std::thread::sleep(Duration::from_millis(10));
    }
    let started = Instant::now();
    poll_until(Duration::from_secs(20), "landed", || {
        tick(c, 1_005 + 2_000 + started.elapsed().as_millis() as i64);
        state_of(c, "T") == TrancheState::Landed
    });
    let lines = role_lines(child, 20, "the child's tick");
    assert!(lines.iter().any(|l| l.contains("role: tick") && l.contains("spawned=[(\"T\", 1)]")), "the child spawned its own lander: {lines:?}");
    assert!(lines.iter().any(|l| l.contains("final state=landed")), "{lines:?}");
    assert!(tripwire.is_empty() && !lines.iter().any(|l| l.contains("already has a reserved row")), "a second decision was made: {tripwire:?} {lines:?}");
    g13_landed_each_child_once(&g);
    let reply = g.commit.join().unwrap();
    assert!(matches!(reply, Ok(TrancheCommitted { state: TrancheState::Landed, .. }) | Err(SpineError::TrancheBusy { .. })), "{reply:?}");
}

// ---------------------------------------------------------------------------
// g14 — commit × rollback crossed on the mutex, both orders
// ---------------------------------------------------------------------------

/// Test 17's crossed acts, DETERMINISTIC in both orders (N = 2 threads).
/// The first act takes the mutex and is held inside its flip transaction by
/// a [`SpineHold`]; the second passes its door check and blocks on the mutex
/// — THE OVERLAP: both in flight, the state still `staging`, while the hold
/// stands. Released: the first flips; the second's conditional flip changes
/// 0 rows and it answers from the state it found. Rollback first → the
/// commit is `refused tranche_terminal {rolled-back}` and no child lands;
/// commit first → the rollback is `refused tranche_past_rollback` and the
/// tranche lands. One outcome on the record each time; the pools agree.
// RED (measured): the flips made UNCONDITIONAL (`state IN (…)`
// dropped from `flip_state`) → in the rollback-first order the commit flips
// a rolled-back tranche to `landing` — this guard's own assertion, red.
#[test]
fn g14_a_commit_and_a_rollback_crossed_on_the_mutex_resolve_to_one_outcome_in_either_order() {
    for commit_first in [false, true] {
        let dir = root(if commit_first { "g14-cr" } else { "g14-rc" });
        let c = fresh(&dir);
        let x = doc(&c, "x", "draft");
        let k = key(0xe1);
        begin_touched(&c, "T", &k, 1_000);
        let a = actor("T", &k);
        assert!(matches!(c.tranche_stage(&a, one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!("by T"))]), 1_001), Ok(ChildOutcome::Staged { .. })));
        let hold = SpineHold::take(&dir);
        let (c1, a1) = (c.clone(), a.clone());
        let (c2, a2) = (c.clone(), a.clone());
        let commit_body = move || c1.tranche_commit(&a1, 1_002);
        let rollback_body = move || c2.tranche_rollback(&a2, 1_003);
        // The first act holds the mutex inside its blocked flip (period
        // outlasted: its door check, milliseconds); the second then blocks
        // on the mutex.
        let (commit, rollback) = if commit_first {
            let h1 = std::thread::spawn(commit_body);
            std::thread::sleep(Duration::from_millis(300));
            let h2 = std::thread::spawn(rollback_body);
            (h1, h2)
        } else {
            let h2 = std::thread::spawn(rollback_body);
            std::thread::sleep(Duration::from_millis(300));
            let h1 = std::thread::spawn(commit_body);
            (h1, h2)
        };
        std::thread::sleep(Duration::from_millis(300));
        assert!(!commit.is_finished() && !rollback.is_finished(), "both in flight");
        assert_eq!(state_of(&c, "T"), TrancheState::Staging);
        hold.release();
        let rolled = bounded(10, "rollback", move || rollback.join().unwrap());
        let started = Instant::now();
        while !commit.is_finished() {
            tick(&c, 1_004);
            assert!(started.elapsed() < Duration::from_secs(20), "the commit never answered");
            std::thread::sleep(Duration::from_millis(10));
        }
        let committed_reply = commit.join().unwrap();
        if commit_first {
            assert!(matches!(committed_reply, Ok(TrancheCommitted { state: TrancheState::Landed, .. })), "{committed_reply:?}");
            match only_witness(rolled.expect_err("rollback after the flip is refused")) {
                Witness::TranchePastRollback { .. } => {}
                w => panic!("{w:?}"),
            }
            assert_eq!(state_of(&c, "T"), TrancheState::Landed);
            assert_eq!(title_of(&c, &x), json!("by T"), "the pools agree with the record");
        } else {
            assert_eq!(rolled.expect("the rollback wins").state, TrancheState::RolledBack);
            match only_witness(committed_reply.expect_err("the commit finds rolled-back")) {
                Witness::TrancheTerminal { state, .. } => assert_eq!(state, "rolled-back"),
                w => panic!("{w:?}"),
            }
            assert_eq!(state_of(&c, "T"), TrancheState::RolledBack, "one outcome on the record");
            assert_eq!(title_of(&c, &x), json!("x"), "a rolled-back tranche's child never lands");
            assert_eq!(lock_rows_under(&dir, anchor_seq(&c, "T")), 0);
        }
    }
}

// ---------------------------------------------------------------------------
// g15 — commit × deadline-abandon crossed
// ---------------------------------------------------------------------------

/// The two conditional statements of the abandon and the decision, crossed on the mutex in
/// both orders (N = 2 threads; period: the heartbeat TIMEOUT, lapsed before
/// either act). Tick first: enforcement takes the mutex (`try_lock`) and is
/// held inside its abandon transaction; `commit` passes its door check and
/// blocks — THE OVERLAP: both in flight with the row still `staging`.
/// Released: abandoned; the commit's flip changes 0 rows → `refused
/// tranche_terminal {abandoned, heartbeat_lost}`. Commit first: the commit
/// holds the mutex; the tick's `try_lock` fails → `skipped_busy`, nothing
/// abandoned; released, the flip lands `landing` (a beat: deadline 5_300),
/// which ticks at 6_000 — past that deadline — never abandon, and the
/// tranche lands.
// RED (measured): the flips unconditional → tick-first flips an
// ABANDONED tranche to `landing`.
#[test]
fn g15_a_commit_and_a_deadline_abandon_crossed_on_the_mutex_resolve_to_one_outcome_in_either_order() {
    for commit_first in [false, true] {
        let dir = root(if commit_first { "g15-ca" } else { "g15-ac" });
        let mut config = cfg();
        config.tranche.timeout = Duration::from_millis(300);
        config.tranche.heartbeat_interval = Duration::from_millis(100);
        let c = fresh_with(&dir, config);
        let x = doc(&c, "x", "draft");
        let k = key(0xf1);
        begin_touched(&c, "T", &k, 1_000);
        let a = actor("T", &k);
        assert!(matches!(c.tranche_stage(&a, one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!("by T"))]), 1_001), Ok(ChildOutcome::Staged { .. })));
        // The deadline (1_301) has lapsed at 5_000.
        let hold = SpineHold::take(&dir);
        let (c1, a1) = (c.clone(), a.clone());
        let c2 = c.clone();
        let commit_body = move || c1.tranche_commit(&a1, 5_000);
        let tick_body = move || c2.tranche_tick(&QUIET, 5_000);
        let (commit, ticker) = if commit_first {
            let h1 = std::thread::spawn(commit_body);
            std::thread::sleep(Duration::from_millis(300));
            let h2 = std::thread::spawn(tick_body);
            (h1, h2)
        } else {
            let h2 = std::thread::spawn(tick_body);
            std::thread::sleep(Duration::from_millis(300));
            let h1 = std::thread::spawn(commit_body);
            (h1, h2)
        };
        std::thread::sleep(Duration::from_millis(300));
        assert!(!commit.is_finished(), "the commit is in flight");
        assert_eq!(state_of(&c, "T"), TrancheState::Staging);
        hold.release();
        let report = bounded(10, "the tick", move || ticker.join().unwrap()).expect("tick");
        if commit_first {
            assert_eq!(report.skipped_busy, vec![("T".to_string(), 1)], "the mutex was the commit's: {report:?}");
            assert!(report.abandoned.is_empty());
            // The flip was a beat (deadline = 5_000 + 300): tick PAST it, so
            // enforcement finds a lapsed `landing` row and must leave it.
            let started = Instant::now();
            while !commit.is_finished() {
                let r = tick(&c, 6_000);
                assert!(r.abandoned.is_empty(), "a landing tranche is never abandoned (deadline 5_300 < 6_000): {r:?}");
                assert!(started.elapsed() < Duration::from_secs(20));
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(matches!(commit.join().unwrap(), Ok(TrancheCommitted { state: TrancheState::Landed, .. })));
            assert_eq!(title_of(&c, &x), json!("by T"));
        } else {
            assert_eq!(report.abandoned, vec![("T".to_string(), 1)], "{report:?}");
            match only_witness(bounded(10, "the commit", move || commit.join().unwrap()).expect_err("the commit finds abandoned")) {
                Witness::TrancheTerminal { state, reason, .. } => assert_eq!((state.as_str(), reason.as_deref()), ("abandoned", Some("heartbeat_lost"))),
                w => panic!("{w:?}"),
            }
            assert_eq!(state_of(&c, "T"), TrancheState::Abandoned);
            assert_eq!(title_of(&c, &x), json!("x"));
            assert_eq!(lock_rows_under(&dir, anchor_seq(&c, "T")), 0);
        }
    }
}

// ---------------------------------------------------------------------------
// g16 — a quiescer standing at the decision postpones it
// ---------------------------------------------------------------------------

/// The window between the flip and the decision (N = 2: the quiescer on one thread, the
/// holder's `commit` joining on another). After `commit`'s flip and before
/// any tick, `quiesce()` is taken: it times out naming the anchor (the token
/// stands). The tick's lander reaches the decision and POSTPONES it —
/// `landing_error {why: quiesced}`, `decided = 0`, no reserved row, no pool
/// write — and the joined `commit` answers `busy landing_in_progress`
/// carrying it. `resume`; the clock passes the backoff; the next tick
/// decides and lands; the re-issued `commit` answers `landed`.
// RED (measured): the quiesce read removed from the decision
// transaction → `decided = 1` and reserved rows under a standing quiesce.
// OPPOSED: with no quiescer the first tick decides.
#[test]
fn g16_a_quiescer_standing_between_commits_flip_and_the_decision_postpones_the_decision_until_resumed() {
    let dir = root("g16");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let k = key(0xa6);
    begin_touched(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    assert!(matches!(c.tranche_stage(&a, one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!("by T"))]), 1_001), Ok(ChildOutcome::Staged { .. })));
    let (c1, a1) = (c.clone(), a.clone());
    let commit = std::thread::spawn(move || c1.tranche_commit(&a1, 1_002));
    poll_until(Duration::from_secs(10), "the flip", || state_of(&c, "T") == TrancheState::Landing);
    // The quiescer, on its own thread, between the flip and the decision.
    let c2 = c.clone();
    let quiescer = std::thread::spawn(move || c2.quiesce());
    let err = bounded(10, "quiesce", move || quiescer.join().unwrap()).expect_err("the anchor is un-cleared: it times out");
    assert!(err.to_string().contains("tranche T attempt 1 is open"), "{err}");
    let token = err.standing.expect("standing");
    let r = tick(&c, 1_003);
    assert_eq!(r.landers_spawned, vec![("T".to_string(), 1)]);
    poll_until(Duration::from_secs(10), "the postponed decision", || {
        c.tranche_heartbeat(&a, &QUIET, 1_003).unwrap().landing_error.map(|e| e.why == "quiesced").unwrap_or(false)
    });
    let hb = c.tranche_heartbeat(&a, &QUIET, 1_004).unwrap();
    assert!(!hb.decided && hb.landed_children == 0, "{hb:?}");
    assert_eq!(count(&dir, "SELECT COUNT(*) FROM transactions WHERE tranche_id = 'T' AND tranche_ordinal IS NOT NULL", &[]), 0, "no reserved row");
    assert_eq!(title_of(&c, &x), json!("x"), "no pool write");
    match bounded(10, "the joined commit", move || commit.join().unwrap()) {
        Err(SpineError::TrancheBusy { why, fields }) => {
            assert_eq!(why, "landing_in_progress");
            assert_eq!(fields["landing_error"]["why"], json!("quiesced"));
        }
        other => panic!("{other:?}"),
    }
    let r = tick(&c, 1_005);
    assert_eq!(r.landers_backing_off, vec![("T".to_string(), 1)], "inside the backoff: {r:?}");
    assert!(c.resume(&token).unwrap());
    // Past the backoff (1 s from 1_003), the next tick decides and lands;
    // the holder's re-issued `commit` answers from the record.
    poll_until(Duration::from_secs(10), "the landing after the resume", || {
        tick(&c, 1_003 + 1_500);
        state_of(&c, "T") == TrancheState::Landed
    });
    let reply = c.tranche_commit(&a, 1_003 + 1_600).expect("the re-issued commit");
    assert_eq!((reply.state, reply.children.len()), (TrancheState::Landed, 1));
    assert_eq!(title_of(&c, &x), json!("by T"));
}

// ---------------------------------------------------------------------------
// the gate's two seams — present under `fault-injection`, refusing by name
// without it (the `atomicity_kill.rs` shape, out of process)
// ---------------------------------------------------------------------------

/// The two gate seams round-trip their frozen names, are disjoint from the
/// door's and the lander's vocabularies, and the seam accepts them. Out of
/// process: the `spine_victim` with `SPINE_FAULT=after-phase-1` on an
/// ordinary door job dies at the seam under the feature (SIGABRT — present)
/// and is refused by name without it (exit 3); with
/// `SPINE_FAULT=no-poll-refresh` — a behaviour seam, nothing aborts at it —
/// the armed victim's door job commits and exits `EXIT_UNREACHED` (4), the
/// production one is refused by name (exit 3).
// RED (measured): `Named::parse` not consulting `GateSeam` → the
// armed victim dies `seam_no_such_stage` (exit 3, not SIGABRT / 4).
#[test]
fn the_gate_seams_speak_two_names_disjoint_from_the_door_and_the_lander_and_the_victim_shows_which_build_carries_them() {
    use spine::fault::GateSeam;
    let named = [(GateSeam::AfterPhase1, "after-phase-1"), (GateSeam::NoPollRefresh, "no-poll-refresh")];
    for (seam, name) in named {
        assert_eq!(seam.name(), name);
        assert_eq!(GateSeam::parse(name), Some(seam));
        assert_eq!(spine::recover::Stage::parse(name), None);
        assert_eq!(spine::tranche::LanderStage::parse(name), None);
    }
    assert_eq!(GateSeam::ALL.map(|s| s.name()), named.map(|(_, n)| n));
    assert!(spine::fault::at_gate_seam(GateSeam::AfterPhase1).is_ok(), "both unset: silent");
    assert_eq!(spine::fault::skip_poll_refresh().unwrap(), false, "unset: the poll refreshes");

    let Some(victim) = option_env!("CARGO_BIN_EXE_spine_victim") else {
        panic!("cargo did not set CARGO_BIN_EXE_spine_victim for this test target");
    };
    for (value, aborts) in [("after-phase-1", true), ("no-poll-refresh", false)] {
        let dir = root(&format!("seam-{value}"));
        let _ = std::fs::remove_dir_all(&dir);
        let out = std::process::Command::new(victim)
            .env("SPINE_VICTIM_DIR", dir.as_os_str())
            .env("SPINE_VICTIM_JOB", "repeatable")
            .env(spine::fault::FAULT_ENV, value)
            .env_remove(spine::fault::PAUSE_ENV)
            .env_remove(spine::fault::PAUSE_LATCH_ENV)
            .env_remove(spine::fault::PAUSE_CEILING_ENV)
            .env("SPINE_VICTIM_EXPECT_FEATURE", if cfg!(feature = "fault-injection") { "1" } else { "0" })
            .output()
            .expect("spawn the victim");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_ne!(out.status.code(), Some(7), "a stale victim of the other build: {stderr}");
        if cfg!(feature = "fault-injection") {
            if aborts {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(out.status.signal(), Some(6), "the door dies AT after-phase-1 (SIGABRT): {stdout} {stderr}");
                assert!(stderr.contains("aborting at stage `after-phase-1`"), "{stderr}");
            } else {
                assert_eq!(out.status.code(), Some(4), "parsed, nothing aborts at a behaviour seam (EXIT_UNREACHED): {stdout} {stderr}");
                assert!(stdout.contains("outcome=committed"), "{stdout}");
            }
            assert!(!stdout.contains("seam_no_such_stage"), "the gate's name parsed: {stdout}");
        } else {
            assert_eq!(out.status.code(), Some(3), "the production door refuses the set seam by name: {stdout} {stderr}");
            assert!(stdout.contains("kind=fault_seam_refused") && stdout.contains(&format!("value={value}")), "{stdout}");
        }
    }
}
