//! THE LANDING (the five columns, the
//! `landing` rows): `commit` flips and joins, the
//! tick spawns the lander, the lander re-validates, decides once, lands each
//! child as its own commit in ordinal order, clears, and on a stop records
//! `landing_error` for the scheduler's backed-off retry.
//!
//! Every guard names its forcing in a `// RED:` comment. Tests are
//! named after the numbered case they own (`t19_<n>_…`) or the guard
//! (`g<n>_…`). Every
//! act takes an explicit `now_ms` (the `Clock`), so the retry backoff is
//! crossed by the clock the test passes, never by sleeping it out; the one
//! real wait a test holds — a pool file held past the ceiling — is bounded by
//! the `lock_wait_ceiling` the test itself configures.
//!
//! THE LANDING IS THE TICK'S: no test here lands anything without a
//! `tranche_tick`. `land()` runs the holder's `commit` on one thread and
//! ticks on the calling thread until the join returns.

use serde_json::{json, Value};
use spine::config::SpineConfig;
use spine::envelope::{
    Envelope, Initiator, Instrument, Mode, Provenance, RippleKind, ENVELOPE_FORMAT_VERSION,
};
use spine::error::SpineError;
use spine::pool::Pool;
use spine::schema_def::{fixtures, PoolGrammar, PoolName};
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use spine::tranche::{
    ChildOutcome, LanderStage, QuietLevels, TickReport, TrancheActor, TrancheBegin,
    TrancheCommitted, TrancheLockMode, TrancheState, LANDING_BACKOFF_FLOOR_MS,
};
use spine::witness::Witness;
use spine::{Cauldron, Outcome, ReadRequest};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

const QUIET: QuietLevels = QuietLevels::BusNotConfigured;
const SERVICE: &str = "runner";
const ALPHA: &str = "fixture-alpha";

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-landing-{name}-{}", std::process::id()))
}

/// Fixture ALPHA with `doc` taking a SERIAL id (`id_prefix`): a CREATE mints
/// a PROVISIONAL `doc-~<16 hex>` the DECISION renumbers —
/// through a prefixed type, so g9's map is non-empty.
fn alpha() -> PoolGrammar {
    let mut g = fixtures::fixture_alpha();
    g.node_types.get_mut("doc").expect("doc").id_prefix = Some("doc".into());
    g
}

/// The landing tests' config: a short lock ceiling and a short per-statement
/// busy timeout, so a pool file held by the test is `file_held` in under a
/// second; everything else default. (The busy timeout also governs every
/// spine-file statement — at 40 ms the armed build's slower ticks hit `Busy`
/// against the lander's transactions; 300 ms leaves the hold detectable at
/// ~600 ms and the spine uncontended.)
fn quick() -> SpineConfig {
    let mut cfg = SpineConfig::default();
    cfg.lock_wait_ceiling = Duration::from_millis(400);
    cfg.busy_timeout = Duration::from_millis(300);
    cfg
}

/// ONE TICK, as the daemon would take it: a tick whose spine transaction
/// found the file busy past the timeout is a tick that did nothing and is
/// taken again next interval — never a failure of what it schedules. Any
/// other error is.
fn tick(c: &Cauldron, now: i64) -> TickReport {
    match c.tranche_tick(&QUIET, now) {
        Ok(r) => r,
        Err(SpineError::Busy { .. }) => TickReport::default(),
        Err(e) => panic!("tick: {e:?}"),
    }
}

/// Runs `f` on a detached thread and panics if it has not answered within
/// `secs` — for a call whose forced-red shape is "never answers" (a `commit`
/// joining a landing that will never come): a scoped thread cannot bound it,
/// since the scope joins the thread before it unwinds, and the test would
/// hang instead of going red. The thread is leaked on the bound firing; the
/// test binary exits without it.
fn bounded<T: Send + 'static>(secs: u64, what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let h = std::thread::spawn(f);
    let started = Instant::now();
    while !h.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(secs), "{what} has not answered within {secs} s");
        std::thread::sleep(Duration::from_millis(10));
    }
    h.join().unwrap()
}

/// Sets the flag when dropped — so a watcher thread looping on `!flag`
/// inside a `thread::scope` is released when the scope's body PANICS, not
/// only when it reaches its `store(true)`. Without it a forced red that fires
/// before the store leaves the watcher spinning, the scope never joins, and
/// the panic is never printed (the harness prints it when the test ends).
struct SetOnDrop<'a>(&'a AtomicBool);
impl Drop for SetOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn fresh_with(dir: &Path, config: SpineConfig) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(dir, &[alpha()], config).expect("create the cauldron")
}

fn fresh(dir: &Path) -> Cauldron {
    fresh_with(dir, quick())
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

fn begin(c: &Cauldron, t: &str, k: &str, now: i64) -> spine::tranche::TrancheBegun {
    let req = TrancheBegin {
        tranche: t.into(),
        key: k.into(),
        holder_service: SERVICE.into(),
        instrument: instrument(),
        lock_mode: TrancheLockMode::Touched,
        regions: vec![ALPHA.into()],
    };
    c.tranche_begin(&req, &QUIET, now)
        .unwrap_or_else(|e| panic!("begin {t}: {e:?}"))
}

fn actor(t: &str, k: &str) -> TrancheActor {
    TrancheActor {
        tranche: t.into(),
        key: k.into(),
        holder_service: SERVICE.into(),
    }
}

/// A type-one child, its nonce the step id, run id `run`.
fn child_of(run: &str, step: &str, statement: &str, params: &[(&str, Value)]) -> Envelope {
    Envelope {
        statement: statement.to_string(),
        bindings: params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Ripple {
                kind: RippleKind::TypeOne,
                run_id: run.into(),
                step_id: step.to_string(),
                scope: vec![],
            },
            instrument: instrument(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(step.to_string()),
    }
}

fn one(step: &str, statement: &str, params: &[(&str, Value)]) -> Envelope {
    child_of("T", step, statement, params)
}

/// An ordinary (keyless) commit by the soup's own machinery.
fn seed(c: &Cauldron, statement: &str, params: &[(&str, Value)]) -> Outcome {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    keyless(
        c,
        &format!("seed-{}", N.fetch_add(1, Ordering::Relaxed)),
        statement,
        params,
    )
    .expect("seed commit")
}

/// A keyless commit whose outcome the test judges.
fn keyless(c: &Cauldron, nonce: &str, statement: &str, params: &[(&str, Value)]) -> spine::error::Result<Outcome> {
    c.submit(Envelope {
        statement: statement.to_string(),
        bindings: params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::CoreMechanical {
                operation: "test-outside".into(),
            },
            instrument: instrument(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(nonce.to_string()),
    })
}

fn doc(c: &Cauldron, title: &str, state: &str) -> String {
    match seed(
        c,
        &format!(r#"CREATE (d: fixture-alpha/doc {{ title: "{title}", state: "{state}" }}) RETURN d.id"#),
        &[],
    ) {
        Outcome::Committed { returned, .. } => returned.0[0]["d.id"].as_str().unwrap().to_string(),
        other => panic!("{other:?}"),
    }
}

fn staged(o: spine::error::Result<ChildOutcome>) -> (u32, BTreeMap<String, String>) {
    match o {
        Ok(ChildOutcome::Staged { ordinal, minted, .. }) => (ordinal, minted),
        other => panic!("expected `staged`, got {other:?}"),
    }
}

fn only_witness(e: SpineError) -> Witness {
    match e {
        SpineError::Refused { mut witnesses } if witnesses.len() == 1 => witnesses.remove(0),
        other => panic!("expected one refusal witness, got {other:?}"),
    }
}

fn req(statement: &str, params: &[(&str, Value)]) -> ReadRequest {
    ReadRequest {
        statement: statement.to_string(),
        bindings: params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        format_version: ENVELOPE_FORMAT_VERSION,
    }
}

/// The committed read — WITHOUT the key.
fn committed(c: &Cauldron, statement: &str, params: &[(&str, Value)]) -> Vec<BTreeMap<String, Value>> {
    c.read(&req(statement, params)).expect("committed read").rows.0
}

fn title_of(c: &Cauldron, id: &str) -> Value {
    let rows = committed(c, "MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.title", &[("id", json!(id))]);
    rows.first().map(|r| r["d.title"].clone()).unwrap_or(Value::Null)
}

fn state_of(c: &Cauldron, t: &str) -> TrancheState {
    c.tranche_store(t, None).expect("store").state
}

fn spine_sql(dir: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("open spine.db");
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn
}

fn pool_sql(dir: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(Pool::path_in(dir, &PoolName::new(ALPHA))).expect("open the pool");
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn
}

fn anchor_seq(c: &Cauldron, t: &str) -> Seq {
    c.tranche_anchor(t, 1).unwrap().unwrap().seq
}

/// The `tranches` row's landing columns, read directly.
fn landing_columns(dir: &Path, t: &str) -> (bool, u32, Option<Value>) {
    spine_sql(dir)
        .query_row(
            "SELECT decided, landed_children, landing_error FROM tranches WHERE id = ?1 AND attempt = 1",
            rusqlite::params![t],
            |r| {
                Ok((
                    r.get::<_, i64>(0)? != 0,
                    r.get::<_, i64>(1)? as u32,
                    r.get::<_, Option<String>>(2)?.map(|s| serde_json::from_str(&s).unwrap()),
                ))
            },
        )
        .unwrap()
}

/// The reserved rows' `seq`s, in ordinal order, as `tranche_children` names them.
fn child_seqs(dir: &Path, t: &str) -> Vec<Option<i64>> {
    let sql = spine_sql(dir);
    let mut stmt = sql
        .prepare("SELECT seq FROM tranche_children WHERE tranche_id = ?1 AND tranche_attempt = 1 ORDER BY ordinal")
        .unwrap();
    stmt.query_map(rusqlite::params![t], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
}

// ---------------------------------------------------------------------------
// THE CLOCK and THE TICKER — the landing is the scheduler's
// ---------------------------------------------------------------------------

/// The daemon clock the tests pass: advanced by hand to cross the backoff.
struct Clock(AtomicI64);

impl Clock {
    fn at(ms: i64) -> Clock {
        Clock(AtomicI64::new(ms))
    }
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn advance(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

/// Run `commit` on one thread and TICK on this one until the join returns.
/// The reports of every tick are handed back beside the reply.
///
/// The commit runs DETACHED, not scoped: a scope joins its threads before a
/// panic on this thread can unwind, so a commit that never answers (the
/// forced-red shape of `join_landing` ignoring a `landing_error`) would hang
/// the test with its bound's panic captured and never printed. Detached, the
/// bound fires, the test goes red, and the thread is leaked.
fn land(c: &Cauldron, a: &TrancheActor, clock: &Clock) -> (spine::error::Result<TrancheCommitted>, Vec<TickReport>) {
    let (c2, a2, now) = (c.clone(), a.clone(), clock.now());
    let h = std::thread::spawn(move || c2.tranche_commit(&a2, now));
    let mut reports = Vec::new();
    let started = Instant::now();
    while !h.is_finished() {
        reports.push(tick(c, clock.now()));
        assert!(started.elapsed() < Duration::from_secs(20), "the landing never finished: {reports:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    (h.join().expect("the commit thread"), reports)
}

/// Tick until `pred` holds on the store, bounded; the reports are returned.
fn tick_until(c: &Cauldron, clock: &Clock, within: Duration, mut pred: impl FnMut() -> bool) -> Vec<TickReport> {
    let mut reports = Vec::new();
    let started = Instant::now();
    loop {
        reports.push(tick(&c, clock.now()));
        if pred() {
            return reports;
        }
        assert!(started.elapsed() < within, "the condition never held within {within:?}: {reports:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Poll the store until `pred` holds, without ticking (nothing lands).
fn wait_until(c: &Cauldron, t: &str, within: Duration, mut pred: impl FnMut(TrancheState) -> bool) {
    let started = Instant::now();
    while !pred(state_of(c, t)) {
        assert!(started.elapsed() < within, "the state never moved within {within:?}: {:?}", state_of(c, t));
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// HOLD THE POOL FILE: an open IMMEDIATE transaction on a second connection
/// — the real "a pool file held by another process" of test
/// 33, in-process (SQLite's file locks do not care which thread holds them).
/// Readers (the re-validation's snapshots, WAL) pass; the lander's landing
/// transaction waits its turn to the ceiling and stops `file_held`.
struct PoolHold(Option<rusqlite::Connection>);

impl PoolHold {
    fn take(dir: &Path) -> PoolHold {
        let conn = rusqlite::Connection::open(Pool::path_in(dir, &PoolName::new(ALPHA))).expect("open the pool");
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

/// A FOREIGN LANDING of `envelope` with `payload`, by hand — the window
/// named ("a foreign submit past its dedup phase when the child staged, which
/// then appends and lands") and test 24's "with that check seamed off":
/// the ordinary door would answer this hash `busy` as a staged child, so the
/// row is appended, decided, its parts landed and cleared through the
/// test-support flips, exactly as the door's phases 8–13 would. Returns the
/// foreign row's `seq`.
fn land_foreign(dir: &Path, envelope: &Envelope, payload: &spine::delta::CompiledPayload) -> Seq {
    let hash = envelope.hash_identity().unwrap();
    let mut db = SpineDb::open(&dir.join(SPINE_FILE)).unwrap();
    let seq = db.append_pending(envelope, &hash, payload).expect("append the foreign row");
    db.mark_decided(seq).expect("decide the foreign row");
    let numbered = db.row_at(seq).unwrap().payload;
    for (i, part) in numbered.parts.iter().enumerate() {
        let mut pool = Pool::open(&Pool::path_in(dir, &part.pool)).unwrap();
        pool.apply_part(&hash, i as u32, part).expect("land the foreign part");
        db.mark_part_landed(seq, i as u32).unwrap();
    }
    assert!(db.mark_cleared(seq, &json!([])).unwrap());
    seq
}

/// The staged payload of child `ordinal`, off `tranche_children`.
fn staged_payload(dir: &Path, t: &str, ordinal: u32) -> spine::delta::CompiledPayload {
    let text: String = spine_sql(dir)
        .query_row(
            "SELECT payload FROM tranche_children WHERE tranche_id = ?1 AND tranche_attempt = 1 AND ordinal = ?2",
            rusqlite::params![t, ordinal],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

fn busy_naming_tranche(r: spine::error::Result<Outcome>, what: &str) {
    match r {
        Err(SpineError::Busy { message }) => assert!(message.contains("staged child of tranche"), "{what}: {message}"),
        other => panic!("{what}: expected busy naming the tranche, got {other:?}"),
    }
}

/// A holder's `commit` joined over a landing that was HELD past the ceiling
/// answers `busy landing_in_progress` carrying the error — or
/// `landed`, when the hold lifted inside the ceiling. Either is the record
/// speaking; a re-issued `commit` after `landed` is what the test asserts.
fn busy_or_landed(r: spine::error::Result<TrancheCommitted>, what: &str) {
    match r {
        Ok(reply) => assert_eq!(reply.state, TrancheState::Landed, "{what}"),
        Err(SpineError::TrancheBusy { why, .. }) => assert_eq!(why, "landing_in_progress", "{what}"),
        other => panic!("{what}: expected landed or busy landing_in_progress, got {other:?}"),
    }
}

fn spawned(reports: &[TickReport]) -> usize {
    reports.iter().map(|r| r.landers_spawned.len()).sum()
}

const SET_TITLE: &str = "MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t";

// ---------------------------------------------------------------------------
// Test 9 — commit on rolled-back; rollback in landing
// ---------------------------------------------------------------------------

/// `commit` on `rolled-back` → `tranche_terminal`; `rollback` in `landing` →
/// `tranche_past_rollback`. OPPOSED: `commit` on `staging` flips to
/// `landing`; `rollback` on `staging` rolls back.
// RED (measured): `legality_for_commit` admitting `rolled-back`
// (→ the commit joins a tranche that will never land: the join loops; the
// test's 20 s bound fires); `tranche_rollback`'s flip admitting `landing`
// (→ a landing tranche rolled back under its lander).
#[test]
fn t19_9_commit_on_rolled_back_is_terminal_and_rollback_in_landing_is_past_rollback() {
    let dir = root("t9");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let clock = Clock::at(1_000);

    // OPPOSED half 1: rollback on staging rolls back; then commit is terminal.
    let k1 = key(0x91);
    begin(&c, "T1", &k1, clock.now());
    let a1 = actor("T1", &k1);
    staged(c.tranche_stage(&a1, child_of("T1", "s0", SET_TITLE, &[("id", json!(x)), ("t", json!("one"))]), clock.now()));
    c.tranche_rollback(&a1, clock.now()).expect("rollback on staging");
    let (c1, a1b, now) = (c.clone(), actor("T1", &k1), clock.now());
    let commit = bounded(20, "commit on rolled-back", move || c1.tranche_commit(&a1b, now));
    match only_witness(commit.expect_err("commit on rolled-back is refused")) {
        Witness::TrancheTerminal { state, reason, .. } => {
            assert_eq!(state, "rolled-back");
            assert_eq!(reason.as_deref(), Some("holder"));
        }
        other => panic!("expected tranche_terminal, got {other:?}"),
    }
    assert_eq!(title_of(&c, &x), json!("x"), "nothing landed");

    // OPPOSED half 2: commit on staging flips to landing (no tick yet, so the
    // lander has not run); rollback in landing is past rollback.
    let k2 = key(0x92);
    begin(&c, "T2", &k2, clock.now());
    let a2 = actor("T2", &k2);
    staged(c.tranche_stage(&a2, child_of("T2", "s0", SET_TITLE, &[("id", json!(x)), ("t", json!("two"))]), clock.now()));
    let (reply, _) = std::thread::scope(|s| {
        let h = s.spawn(|| c.tranche_commit(&a2, clock.now()));
        wait_until(&c, "T2", Duration::from_secs(5), |st| st == TrancheState::Landing);
        match only_witness(c.tranche_rollback(&a2, clock.now()).expect_err("rollback in landing is refused")) {
            Witness::TranchePastRollback { tranche, attempt } => assert_eq!((tranche.as_str(), attempt), ("T2", 1)),
            other => panic!("expected tranche_past_rollback, got {other:?}"),
        }
        // A child in landing is past rollback too.
        match only_witness(
            c.tranche_stage(&a2, child_of("T2", "s1", SET_TITLE, &[("id", json!(x)), ("t", json!("late"))]), clock.now())
                .expect_err("a child in landing is refused"),
        ) {
            Witness::TranchePastRollback { .. } => {}
            other => panic!("expected tranche_past_rollback, got {other:?}"),
        }
        // Now the scheduler lands it.
        let mut reports = Vec::new();
        let started = Instant::now();
        while !h.is_finished() {
            reports.push(tick(&c, clock.now()));
            assert!(started.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(10));
        }
        (h.join().unwrap(), reports)
    });
    let reply = reply.expect("the commit lands");
    assert_eq!(reply.state, TrancheState::Landed);
    assert_eq!(title_of(&c, &x), json!("two"));
    // And rollback AFTER landed is terminal {landed}.
    match only_witness(c.tranche_rollback(&a2, clock.now()).expect_err("rollback on landed")) {
        Witness::TrancheTerminal { state, reason, .. } => {
            assert_eq!(state, "landed");
            assert_eq!(reason.as_deref(), Some("landed"));
        }
        other => panic!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test 10 — begin after attempt 1 landed / abandoned; the store answers
// attempt 1
// ---------------------------------------------------------------------------

/// `begin` after attempt 1 LANDED → `tranche_terminal {state: landed}` (a
/// type-three run lands once); after attempt 1 ABANDONED → attempt 2;
/// the store still answers attempt 1.
// RED (measured): `tranche_begin`'s step 3 (the latest attempt
// `landed` → refused) removed → a landed `T` begins attempt 2 — two landings
// of one run.
#[test]
fn t19_10_begin_after_a_landed_attempt_is_refused_and_after_an_abandoned_one_opens_attempt_2() {
    let dir = root("t10");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let clock = Clock::at(1_000);
    let timeout = c.config().tranche.timeout.as_millis() as i64;

    // Attempt 1 of T lands.
    let k = key(0xa1);
    begin(&c, "T", &k, clock.now());
    let a = actor("T", &k);
    staged(c.tranche_stage(&a, one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!("landed"))]), clock.now()));
    let (reply, _) = land(&c, &a, &clock);
    assert_eq!(reply.expect("lands").state, TrancheState::Landed);
    // A fresh key under T: refused by name, state landed.
    let e = c
        .tranche_begin(
            &TrancheBegin {
                tranche: "T".into(),
                key: key(0xa2),
                holder_service: SERVICE.into(),
                instrument: instrument(),
                lock_mode: TrancheLockMode::Touched,
                regions: vec![ALPHA.into()],
            },
            &QUIET,
            clock.now(),
        )
        .expect_err("begin after landed is refused");
    match only_witness(e) {
        Witness::TrancheTerminal { tranche, attempt, state, reason, .. } => {
            assert_eq!((tranche.as_str(), attempt), ("T", 1));
            assert_eq!(state, "landed");
            assert_eq!(reason.as_deref(), Some("landed"));
        }
        other => panic!("{other:?}"),
    }
    let store = c.tranche_store("T", Some(1)).unwrap();
    assert_eq!(store.state, TrancheState::Landed);
    assert_eq!(store.children.len(), 1);
    assert_eq!(store.children[0].state, "landed");

    // OPPOSED: T2 attempt 1 is abandoned (the deadline lapses) → a fresh key
    // opens attempt 2; the store still answers attempt 1.
    let k1 = key(0xa3);
    begin(&c, "T2", &k1, clock.now());
    clock.advance(timeout + 1);
    let tick = tick(&c, clock.now());
    assert_eq!(tick.abandoned, vec![("T2".to_string(), 1)]);
    let k2 = key(0xa4);
    let b2 = begin(&c, "T2", &k2, clock.now());
    assert_eq!((b2.attempt, b2.state), (2, TrancheState::Open));
    let first = c.tranche_store("T2", Some(1)).unwrap();
    assert_eq!((first.attempt, first.state), (1, TrancheState::Abandoned));
    assert_eq!(first.reason.as_deref(), Some("heartbeat_lost"));
}

// ---------------------------------------------------------------------------
// Test 11 — a quiesce standing
// ---------------------------------------------------------------------------

/// A quiesce standing: a child → the spine's quiesced `busy`; `commit` → the
/// quiesced `busy`, state still `staging` (the holder keeps its rollback).
/// OPPOSED: the quiesce lifted, the same `commit` lands.
// RED (measured): `tranche_commit`'s quiesce check (both the courtesy
// read and the one inside the flip's transaction) removed → `commit` enters
// `landing` under the quiesce; the backup would include a landing.
#[test]
fn t19_11_a_standing_quiesce_refuses_the_child_and_the_commit_and_the_state_stays_staging() {
    let dir = root("t11");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let clock = Clock::at(1_000);
    let k = key(0xb1);
    begin(&c, "T", &k, clock.now());
    let a = actor("T", &k);
    staged(c.tranche_stage(&a, one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!("q"))]), clock.now()));
    // The quiesce cannot drain (the anchor is un-cleared for the tranche's
    // life): it times out loudly at the ceiling with its quiescer
    // STANDING, and carries the token.
    let standing = match c.quiesce() {
        Err(e) => {
            assert!(matches!(e.error, SpineError::Busy { .. }), "{e}");
            e.standing.expect("the quiescer stands")
        }
        Ok(_) => panic!("a quiesce over a live tranche cannot drain"),
    };
    match c.tranche_stage(&a, one("s1", SET_TITLE, &[("id", json!(x)), ("t", json!("q2"))]), clock.now()) {
        Err(SpineError::Busy { message }) => assert!(message.contains("quiesced"), "{message}"),
        other => panic!("a child under a quiesce is the quiesced busy, got {other:?}"),
    }
    // Bounded: with the quiesce gates off the commit is admitted and joins a
    // landing no tick will produce — it would never answer.
    let (c1, a1, now) = (c.clone(), a.clone(), clock.now());
    match bounded(20, "a commit under a standing quiesce", move || c1.tranche_commit(&a1, now)) {
        Err(SpineError::Busy { message }) => assert!(message.contains("quiesced"), "{message}"),
        other => panic!("a commit under a quiesce is the quiesced busy, got {other:?}"),
    }
    assert_eq!(state_of(&c, "T"), TrancheState::Staging, "the state did not move");
    assert_eq!(landing_columns(&dir, "T").0, false, "nothing decided");
    // OPPOSED: lifted, the commit lands.
    assert!(c.resume(&standing).expect("resume"));
    let (reply, _) = land(&c, &a, &clock);
    assert_eq!(reply.expect("lands").state, TrancheState::Landed);
    assert_eq!(title_of(&c, &x), json!("q"));
}

// ---------------------------------------------------------------------------
// Test 12 — two tranches queued: B opens after A LANDS, not at A's flip
// ---------------------------------------------------------------------------

/// A then B: A `open`, B `queued` position 2; B's child → `busy queued
/// {position: 2, ahead: A}`; A `commit` → B stays queued WHILE A IS LANDING
/// (its locks held) and opens on the tick after A lands; B's beat shows
/// `open`.
// RED (measured): `LIVE_STATES` without `landing` (the queue
// advanced on A's `landing` flip) → B opens while A's lock rows stand.
#[test]
fn t19_12_the_second_tranche_opens_on_the_tick_after_the_first_lands_never_on_its_landing_flip() {
    let dir = root("t12");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let clock = Clock::at(1_000);
    let ka = key(0xc1);
    begin(&c, "A", &ka, clock.now());
    let a = actor("A", &ka);
    staged(c.tranche_stage(&a, child_of("A", "s0", SET_TITLE, &[("id", json!(x)), ("t", json!("a"))]), clock.now()));
    let kb = key(0xc2);
    let b = begin(&c, "B", &kb, clock.now());
    assert_eq!((b.state, b.position), (TrancheState::Queued, 2));
    let ab = actor("B", &kb);
    match c.tranche_stage(&ab, child_of("B", "s0", SET_TITLE, &[("id", json!(x)), ("t", json!("b"))]), clock.now()) {
        Err(SpineError::TrancheBusy { why, fields }) => {
            assert_eq!(why, "queued");
            assert_eq!(fields["position"], json!(2));
            assert_eq!(fields["ahead"]["tranche"], json!("A"));
        }
        other => panic!("expected busy queued, got {other:?}"),
    }
    // A's landing is HELD on the pool file: A is `landing` across several
    // ticks, and B must stay queued through every one of them.
    let hold = PoolHold::take(&dir);
    let anchor = anchor_seq(&c, "A");
    let reports = std::thread::scope(|s| {
        let h = s.spawn(|| c.tranche_commit(&a, clock.now()));
        wait_until(&c, "A", Duration::from_secs(5), |st| st == TrancheState::Landing);
        let mut reports = Vec::new();
        // Ticks while the lander is blocked (it stops `file_held` at the
        // ceiling and the backoff holds it): B must not open.
        for _ in 0..6 {
            reports.push(tick(&c, clock.now()));
            std::thread::sleep(Duration::from_millis(100));
        }
        let held: i64 = spine_sql(&dir)
            .query_row("SELECT COUNT(*) FROM locks WHERE seq = ?1", rusqlite::params![anchor.0], |r| r.get(0))
            .unwrap();
        assert!(held > 0, "A's lock rows stand while it is landing");
        assert!(reports.iter().all(|r| r.opened.is_empty()), "B opened while A was landing: {reports:?}");
        let beat = c.tranche_heartbeat(&ab, &QUIET, clock.now()).unwrap();
        assert_eq!((beat.state, beat.position), (TrancheState::Queued, Some(2)));
        hold.release();
        // Past the backoff, the retry lands A; B opens on a LATER tick, and
        // on the tick that opens it A is already `landed` (the queue advances
        // only past a terminal head).
        clock.advance(LANDING_BACKOFF_FLOOR_MS * 2);
        let started = Instant::now();
        loop {
            let r = tick(&c, clock.now());
            if !r.opened.is_empty() {
                assert_eq!(r.opened, vec![("B".to_string(), 1)]);
                assert_eq!(state_of(&c, "A"), TrancheState::Landed, "B opened while A was not landed");
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "B never opened");
            std::thread::sleep(Duration::from_millis(10));
        }
        busy_or_landed(h.join().unwrap(), "A's first commit");
        reports
    });
    assert!(reports.iter().all(|r| r.opened.is_empty()), "B opened while A was landing: {reports:?}");
    assert_eq!(c.tranche_commit(&a, clock.now()).expect("A's re-issued commit").state, TrancheState::Landed);
    let beat = c.tranche_heartbeat(&ab, &QUIET, clock.now()).unwrap();
    assert_eq!((beat.state, beat.position), (TrancheState::Open, None));
    assert_eq!(title_of(&c, &x), json!("a"));
}

// ---------------------------------------------------------------------------
// Test 17 / g1 — double landing: one decision at N ≥ 2, identical bytes
// ---------------------------------------------------------------------------

/// Two `commit`s at once on two threads → ONE decision; both replies
/// byte-identical; one row per child (`transaction_by_hash`); one
/// `applied_parts` row per (child hash, part). OPPOSED: one `commit` alone
/// → `landed` with the same shape.
// RED (measured): the decision's `AND decided = 0` dropped and the
// "already cleared → skip" removed → a retry decides again (the second
// reservation hits `idx_tx_live`, or a part applies twice). The N ≥ 2 half:
// `flip_state`'s `state IN (open, staging)` made unconditional → both
// commits "flip"; the state is still one landing (the mutex), so the
// observable red is the retry's.
#[test]
fn g1_two_commits_on_two_threads_decide_once_and_answer_identical_bytes() {
    let dir = root("g1");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let y = doc(&c, "y", "draft");
    let clock = Clock::at(1_000);
    let k = key(0xd1);
    begin(&c, "T", &k, clock.now());
    let a = actor("T", &k);
    let c0 = one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!("x1"))]);
    let c1 = one("s1", SET_TITLE, &[("id", json!(y)), ("t", json!("y1"))]);
    staged(c.tranche_stage(&a, c0.clone(), clock.now()));
    staged(c.tranche_stage(&a, c1.clone(), clock.now()));

    let done = AtomicBool::new(false);
    let (r1, r2) = std::thread::scope(|s| {
        let h1 = s.spawn(|| c.tranche_commit(&a, clock.now()));
        let h2 = s.spawn(|| c.tranche_commit(&a, clock.now()));
        let ticker = s.spawn(|| {
            let started = Instant::now();
            while !done.load(Ordering::SeqCst) {
                tick(&c, clock.now());
                assert!(started.elapsed() < Duration::from_secs(20));
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();
        done.store(true, Ordering::SeqCst);
        ticker.join().unwrap();
        (r1, r2)
    });
    let r1 = r1.expect("commit 1 lands");
    let r2 = r2.expect("commit 2 lands");
    let b1 = serde_json::to_vec(&r1).unwrap();
    let b2 = serde_json::to_vec(&r2).unwrap();
    assert_eq!(b1, b2, "the two replies are byte-identical");
    assert_eq!(r1.state, TrancheState::Landed);
    assert_eq!(r1.children.len(), 2);
    // ONE decision: one row per child, decided and cleared, and one
    // `applied_parts` row per (hash, part).
    let pool = pool_sql(&dir);
    for (env, ordinal) in [(&c0, 0u32), (&c1, 1u32)] {
        let hash = env.hash_identity().unwrap();
        let rows: i64 = spine_sql(&dir)
            .query_row("SELECT COUNT(*) FROM transactions WHERE hash = ?1", rusqlite::params![hash.to_hex()], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "one row for child {ordinal}");
        let row = c.transaction_by_hash(&hash).unwrap().expect("the child's row");
        assert!(row.decided && row.cleared);
        let mark = row.tranche.expect("the tranche mark");
        assert_eq!((mark.id.as_str(), mark.attempt, mark.ordinal, mark.index, mark.of), ("T", 1, Some(ordinal), Some(ordinal), Some(2)));
        let applied: i64 = pool
            .query_row("SELECT COUNT(*) FROM applied_parts WHERE tx_hash = ?1", rusqlite::params![hash.to_hex()], |r| r.get(0))
            .unwrap();
        assert_eq!(applied, 1, "one applied_parts row for child {ordinal}");
        assert_eq!(r1.children[ordinal as usize].seq, row.seq.0);
    }
    assert_eq!(title_of(&c, &x), json!("x1"));
    assert_eq!(title_of(&c, &y), json!("y1"));
    let (decided, landed_children, err) = landing_columns(&dir, "T");
    assert_eq!((decided, landed_children, err), (true, 2, None));
    // A third commit, after landed, on this thread: the same bytes again.
    let r3 = c.tranche_commit(&a, clock.now()).expect("idempotent commit");
    assert_eq!(serde_json::to_vec(&r3).unwrap(), b1);
}

// ---------------------------------------------------------------------------
// Test 17 / g2 — commit × rollback crossed on two threads
// ---------------------------------------------------------------------------

/// A `commit` and a `rollback` issued together on two threads → exactly one
/// of `landed` / `rolled-back` on the record; the loser answered
/// from the state it found; the pools agree with the record. Run six times
/// over fresh tranches so both orders have a chance; each order is also
/// forced once (test 9). THE HANDSHAKE: the
/// ticker — and so the lander, which is the tick's alone — starts only once
/// the rollback has answered, so a commit-won crossing is always answered
/// from `landing` (`tranche_past_rollback`) and never from a landing that
/// finished first (`tranche_terminal {landed}`, a legitimate answer
/// this guard does not accept: the fault build's full suite hit it once in
/// nine). The ORDER of the two flips stays the race under test; the third
/// outcome is gone by construction, not by a longer sleep.
// RED (measured): both flips unconditional (`flip_state` ignoring
// `from`) → both acts "succeed" and a rolled-back tranche's children land
// under no locks (carried by test 9, deterministically).
// RED (measured, the instrument): the rollback delayed
// 200 ms with the ticker running from the start → the lander lands first
// and the rollback is answered `tranche_terminal {landed}` at the
// `TranchePastRollback` assertion, 3/3 — the flake, forced; the same delay
// under the handshake is green 3/3.
#[test]
fn g2_a_commit_and_a_rollback_crossed_on_two_threads_resolve_to_one_outcome_the_pools_agree_with() {
    let dir = root("g2");
    let c = fresh(&dir);
    let clock = Clock::at(1_000);
    let mut landed = 0;
    let mut rolled = 0;
    for i in 0..6u8 {
        let t = format!("T{i}");
        let x = doc(&c, &format!("x{i}"), "draft");
        let k = key(0xe0 + i);
        begin(&c, &t, &k, clock.now());
        let a = actor(&t, &k);
        staged(c.tranche_stage(&a, child_of(&t, "s0", SET_TITLE, &[("id", json!(x)), ("t", json!("landed"))]), clock.now()));
        let done = AtomicBool::new(false);
        let (commit, rollback) = std::thread::scope(|s| {
            let hc = s.spawn(|| c.tranche_commit(&a, clock.now()));
            let hr = s.spawn(|| c.tranche_rollback(&a, clock.now()));
            // No tick until the rollback has answered: with the ticker idle
            // the record can only be `staging`, `landing` or `rolled-back`
            // when the rollback's flip runs. The rollback waits on nothing
            // but the mutex, which the commit holds for its flip alone.
            let rr = hr.join().unwrap();
            let ticker = s.spawn(|| {
                let started = Instant::now();
                while !done.load(Ordering::SeqCst) {
                    tick(&c, clock.now());
                    assert!(started.elapsed() < Duration::from_secs(20));
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
            let rc = hc.join().unwrap();
            done.store(true, Ordering::SeqCst);
            ticker.join().unwrap();
            (rc, rr)
        });
        let state = state_of(&c, &t);
        match (commit, rollback) {
            (Ok(reply), Err(e)) => {
                landed += 1;
                assert_eq!(state, TrancheState::Landed);
                assert_eq!(reply.state, TrancheState::Landed);
                let w = only_witness(e);
                assert!(matches!(w, Witness::TranchePastRollback { .. }), "{t}: the losing rollback was answered {w:?}");
                assert_eq!(title_of(&c, &x), json!("landed"), "{t}: the pool agrees with `landed`");
            }
            (Err(e), Ok(rb)) => {
                rolled += 1;
                assert_eq!(state, TrancheState::RolledBack);
                assert_eq!(rb.state, TrancheState::RolledBack);
                match only_witness(e) {
                    Witness::TrancheTerminal { state, .. } => assert_eq!(state, "rolled-back"),
                    other => panic!("{other:?}"),
                }
                assert_eq!(title_of(&c, &x), json!(format!("x{i}")), "{t}: the pool agrees with `rolled-back`");
                // No child row, no lock row.
                let seq = anchor_seq(&c, &t);
                let held: i64 = spine_sql(&dir)
                    .query_row("SELECT COUNT(*) FROM locks WHERE seq = ?1", rusqlite::params![seq.0], |r| r.get(0))
                    .unwrap();
                assert_eq!(held, 0);
                assert!(child_seqs(&dir, &t).iter().all(|s| s.is_none()), "no child row reserved");
            }
            (c_, r_) => panic!("{t}: both acts answered the same way: {c_:?} / {r_:?}"),
        }
    }
    eprintln!("g2: {landed} landed, {rolled} rolled back over 6 crossings");
}

// ---------------------------------------------------------------------------
// Test 17 — `recover()` on the live cauldron mid-landing touches no
// tranche row
// ---------------------------------------------------------------------------

/// `Cauldron::recover()` invoked on the live cauldron mid-landing — the
/// lander held on the pool file after the decision, its reserved child row
/// undecided — touches no tranche row: the anchor and the reserved row are
/// `skipped_tranche`, nothing aborted; the landing then finishes.
// RED (measured): the reservation written WITHOUT `tranche_id` →
// `recover()` aborts the reserved child row under the lander.
#[test]
fn t19_17_recover_on_the_live_cauldron_mid_landing_touches_no_tranche_row() {
    let dir = root("t17r");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let clock = Clock::at(1_000);
    let k = key(0xf1);
    begin(&c, "T", &k, clock.now());
    let a = actor("T", &k);
    let c0 = one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!("r"))]);
    staged(c.tranche_stage(&a, c0.clone(), clock.now()));
    let anchor = c.tranche_anchor("T", 1).unwrap().unwrap();
    let hold = PoolHold::take(&dir);
    let reply = std::thread::scope(|s| {
        let h = s.spawn(|| c.tranche_commit(&a, clock.now()));
        wait_until(&c, "T", Duration::from_secs(5), |st| st == TrancheState::Landing);
        // One tick: the lander decides (spine), reserves, and blocks on the
        // pool file. Wait for the reservation to appear.
        let first = tick(&c, clock.now());
        assert_eq!(first.landers_spawned, vec![("T".to_string(), 1)]);
        let started = Instant::now();
        while landing_columns(&dir, "T").0 == false {
            assert!(started.elapsed() < Duration::from_secs(5), "the decision never came");
            std::thread::sleep(Duration::from_millis(5));
        }
        let reserved = child_seqs(&dir, "T")[0].expect("the reserved row");
        // recover() beside the landing.
        let report = c.recover().expect("recover");
        assert!(report.aborted.is_empty(), "nothing aborted: {report:?}");
        assert!(report.rolled_forward.is_empty() && report.cleared_only.is_empty(), "{report:?}");
        assert!(report.skipped_tranche.contains(&anchor.hash), "the anchor is skipped: {report:?}");
        assert!(report.skipped_tranche.contains(&c0.hash_identity().unwrap()), "the reserved child row is skipped: {report:?}");
        let row = c.transaction(Seq(reserved)).unwrap();
        assert!(!row.cleared, "the reserved row was not aborted");
        assert!(row.tranche.is_some());
        hold.release();
        clock.advance(LANDING_BACKOFF_FLOOR_MS * 2);
        tick_until(&c, &clock, Duration::from_secs(10), || state_of(&c, "T") == TrancheState::Landed);
        h.join().unwrap()
    });
    busy_or_landed(reply, "the first commit");
    assert_eq!(c.tranche_commit(&a, clock.now()).expect("landed").state, TrancheState::Landed);
    assert_eq!(title_of(&c, &x), json!("r"));
}

// ---------------------------------------------------------------------------
// Test 24 — a child landed elsewhere, three shapes, plus the door check
// ---------------------------------------------------------------------------

/// The same step submitted OUTSIDE the tranche after child `k` staged →
/// `busy` at the door (the staged-child check). With that check
/// seamed off (the foreign landing done by hand, `land_foreign`): (a) child
/// `k` minting nothing, foreign payload identical → `commit` → `landed`,
/// child `k` `deduplicated: true` with the foreign `seq`, no row of its own;
/// (b) child `k` minting a node → `failed`, `tranche_landing_refused {why:
/// child_landed_elsewhere}`; (c) identical envelope, different payload →
/// `failed`, the same witness. OPPOSED: (a) lands.
// RED (measured): (1) `decide` comparing envelopes (the hash) instead
// of payloads → (c) deduplicates and child k+1 lands on a false premise;
// (2) the door's staged-child check removed → the keyless submit of (b)
// lands through the door.
#[test]
fn t19_24_a_child_landed_elsewhere_deduplicates_on_an_identical_payload_and_fails_the_tranche_otherwise() {
    let clock = Clock::at(1_000);

    // ---- the door check, and (a) ----
    let dir = root("t24a");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let y = doc(&c, "y", "draft");
    let k = key(0x24);
    begin(&c, "T", &k, clock.now());
    let a = actor("T", &k);
    let c0 = one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!("same"))]);
    let c1 = one("s1", SET_TITLE, &[("id", json!(y)), ("t", json!("after"))]);
    staged(c.tranche_stage(&a, c0.clone(), clock.now()));
    staged(c.tranche_stage(&a, c1.clone(), clock.now()));
    // The door: the same step outside the tranche is busy naming the tranche.
    busy_naming_tranche(c.submit(c0.clone()), "the staged child's step at the door");
    // Seamed off: the foreign run lands the identical payload.
    let foreign = land_foreign(&dir, &c0, &staged_payload(&dir, "T", 0));
    assert_eq!(title_of(&c, &x), json!("same"), "the foreign landing is in the pool");
    let (reply, _) = land(&c, &a, &clock);
    let reply = reply.expect("(a) lands");
    assert_eq!(reply.state, TrancheState::Landed);
    assert_eq!(reply.children.len(), 2);
    assert!(reply.children[0].deduplicated, "child 0 deduplicated: {:?}", reply.children[0]);
    assert_eq!(reply.children[0].seq, foreign.0, "against the foreign seq");
    assert!(!reply.children[1].deduplicated);
    // Child 0 has no row of its own; child 1 is index 0 of 1.
    let rows: i64 = spine_sql(&dir)
        .query_row("SELECT COUNT(*) FROM transactions WHERE hash = ?1", rusqlite::params![c0.hash_identity().unwrap().to_hex()], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1, "the foreign row is the only row for child 0's hash");
    let mark = c.transaction(Seq(reply.children[1].seq)).unwrap().tranche.unwrap();
    assert_eq!((mark.ordinal, mark.index, mark.of), (Some(1), Some(0), Some(1)));
    assert_eq!(title_of(&c, &y), json!("after"));
    let store = c.tranche_store("T", None).unwrap();
    assert_eq!(store.children[0].state, "landed_elsewhere");
    assert_eq!(store.children[1].state, "landed");

    // ---- (b): child k minted a node → failed ----
    let dir = root("t24b");
    let c = fresh(&dir);
    let k = key(0x25);
    begin(&c, "T", &k, clock.now());
    let a = actor("T", &k);
    let cb = one("s0", r#"CREATE (d: fixture-alpha/doc { title: "minted", state: "draft" })"#, &[]);
    let (_, minted) = staged(c.tranche_stage(&a, cb.clone(), clock.now()));
    assert!(minted["d"].contains("doc-~"), "a provisional id was minted: {minted:?}");
    busy_naming_tranche(c.submit(cb.clone()), "(b) the minting step at the door");
    land_foreign(&dir, &cb, &staged_payload(&dir, "T", 0));
    let (reply, _) = land(&c, &a, &clock);
    let refused = |e: SpineError, why: &str| match e {
        SpineError::Refused { witnesses } => {
            assert_eq!(witnesses.len(), 2, "{witnesses:?}");
            match &witnesses[0] {
                Witness::TrancheTerminal { state, reason, .. } => {
                    assert_eq!(state, "failed");
                    assert_eq!(reason.as_deref(), Some("landing_refused"));
                }
                other => panic!("{other:?}"),
            }
            match &witnesses[1] {
                Witness::TrancheLandingRefused { ordinal, why: w, colliding_seq, .. } => {
                    assert_eq!(*ordinal, 0);
                    assert_eq!(w, why);
                    assert!(colliding_seq.is_some());
                }
                other => panic!("{other:?}"),
            }
        }
        other => panic!("expected the two-witness refusal, got {other:?}"),
    };
    refused(reply.expect_err("(b) fails"), "child_landed_elsewhere");
    assert_eq!(state_of(&c, "T"), TrancheState::Failed);
    let store = c.tranche_store("T", None).unwrap();
    assert_eq!(store.reason.as_deref(), Some("landing_refused"));
    assert_eq!(store.refusal.as_ref().unwrap()["why"], json!("child_landed_elsewhere"));
    assert_eq!(store.children[0].state, "stored");
    // Every later commit answers the same bytes.
    let again = c.tranche_commit(&a, clock.now()).expect_err("still failed");
    refused(again, "child_landed_elsewhere");

    // ---- (c): identical envelope, DIFFERENT payload → failed ----
    let dir = root("t24c");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let k = key(0x26);
    begin(&c, "T", &k, clock.now());
    let a = actor("T", &k);
    let cc = one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!("mine"))]);
    staged(c.tranche_stage(&a, cc.clone(), clock.now()));
    // The foreign run of the SAME envelope bound differently: its payload
    // sets another value (hand-built; the hash is over the statement).
    let mut other = staged_payload(&dir, "T", 0);
    for part in &mut other.parts {
        for d in &mut part.deltas {
            if let spine::delta::Delta::SetField { value, .. } = d {
                *value = json!("theirs");
            }
        }
    }
    assert_ne!(other, staged_payload(&dir, "T", 0), "the two payloads differ");
    land_foreign(&dir, &cc, &other);
    assert_eq!(title_of(&c, &x), json!("theirs"));
    let (reply, _) = land(&c, &a, &clock);
    refused(reply.expect_err("(c) fails"), "child_landed_elsewhere");
    assert_eq!(state_of(&c, "T"), TrancheState::Failed);
    assert_eq!(title_of(&c, &x), json!("theirs"), "the tranche landed nothing");
}

// ---------------------------------------------------------------------------
// Test 33 — a stalled landing: file_held, the beat and the joined commit
// carry it, the retry lands within two ticks of the release
// ---------------------------------------------------------------------------

/// After the decision, a pool file held by another process past the ceiling
/// → `landing_error {why: file_held, at_child: 0}` on the beat and `busy
/// tranche_busy / landing_in_progress` carrying it on the joined `commit`
/// (the join RETURNS: the holder learns); the record stays `landing`,
/// decided. Release the file; past the backoff the scheduler finishes within
/// two ticks and the re-issued `commit` answers `landed`.
// RED (measured): (1) `tranche_tick`'s lander spawn skipping a row
// with `landing_error` set (no retry) → the tranche stays `landing` for
// ever; (2) `join_landing` not returning on `landing_error` → the holder's
// commit never learns (the test's bound fires).
#[test]
fn t19_33_a_pool_file_held_past_the_ceiling_is_file_held_on_the_beat_and_the_joined_commit_and_the_retry_lands_within_two_ticks() {
    let dir = root("t33");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let clock = Clock::at(1_000);
    let k = key(0x33);
    begin(&c, "T", &k, clock.now());
    let a = actor("T", &k);
    staged(c.tranche_stage(&a, one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!("held"))]), clock.now()));
    let hold = PoolHold::take(&dir);
    let (first_commit, reports) = land(&c, &a, &clock);
    // The joined commit returned BUSY carrying the error — not landed.
    let err = match first_commit {
        Err(SpineError::TrancheBusy { why, fields }) => {
            assert_eq!(why, "landing_in_progress");
            fields["landing_error"].clone()
        }
        other => panic!("expected busy landing_in_progress, got {other:?}"),
    };
    assert_eq!(err["why"], json!("file_held"));
    assert_eq!(err["at_child"], json!(0));
    assert_eq!(err["retries"], json!(1));
    assert!(spawned(&reports) >= 1);
    // The record: landing, decided, one child reserved and not landed.
    assert_eq!(state_of(&c, "T"), TrancheState::Landing);
    let (decided, landed_children, on_row) = landing_columns(&dir, "T");
    assert!(decided && landed_children == 0);
    assert_eq!(on_row.as_ref().unwrap()["why"], json!("file_held"));
    // The beat carries it.
    let beat = c.tranche_heartbeat(&a, &QUIET, clock.now()).unwrap();
    let on_beat = beat.landing_error.expect("the beat carries the error");
    assert_eq!((on_beat.why.as_str(), on_beat.at_child), ("file_held", Some(0)));
    assert!(beat.decided);
    // While the backoff runs, ticks spawn no lander (backing off).
    let t0 = tick(&c, clock.now());
    assert_eq!(t0.landers_backing_off, vec![("T".to_string(), 1)]);
    assert!(t0.landers_spawned.is_empty());
    // A re-issued commit, still within the backoff: busy again, at once
    // (bounded: with the join's landing_error exit off it never answers).
    let (c2, a2, now) = (c.clone(), a.clone(), clock.now());
    let reissued = bounded(20, "the re-issued commit during the backoff", move || c2.tranche_commit(&a2, now));
    assert!(matches!(reissued, Err(SpineError::TrancheBusy { .. })));
    // Release; cross the backoff; two ticks land it.
    hold.release();
    clock.advance(LANDING_BACKOFF_FLOOR_MS + 1);
    let t1 = tick(&c, clock.now());
    assert_eq!(t1.landers_spawned, vec![("T".to_string(), 1)]);
    let started = Instant::now();
    while state_of(&c, "T") != TrancheState::Landed {
        assert!(started.elapsed() < Duration::from_secs(5), "the retry never landed");
        std::thread::sleep(Duration::from_millis(5));
    }
    let t2 = tick(&c, clock.now());
    assert!(t2.landers_spawned.is_empty() && t2.landers_backing_off.is_empty(), "{t2:?}");
    let reply = c.tranche_commit(&a, clock.now()).expect("the re-issued commit answers landed");
    assert_eq!(reply.state, TrancheState::Landed);
    assert_eq!(title_of(&c, &x), json!("held"));
    let beat = c.tranche_heartbeat(&a, &QUIET, clock.now()).expect_err("a beat after landed is terminal");
    assert!(matches!(only_witness(beat), Witness::TrancheTerminal { .. }));
    assert_eq!(landing_columns(&dir, "T").2, None, "the error is cleared");
}

// ---------------------------------------------------------------------------
// g3 — the lander outlives the holder (test 25, in-process form)
// ---------------------------------------------------------------------------

/// The holder's `commit` is a DETACHED thread (never joined — the holder is
/// gone). After its flip and BEFORE any tick, the tranche stays `landing`
/// and undecided for 300 ms (the period a request-side lander would need is
/// milliseconds); the first tick spawns the lander; the tranche lands within
/// two ticks with no holder; a re-issued `commit` answers the record.
// RED (measured): `tranche_commit` running `exec::land` inline
// after its flip → the tranche is decided (and landed) before the first
// tick; `landers_spawned` is empty.
#[test]
fn g3_the_lander_is_the_ticks_a_holder_gone_after_its_flip_still_lands_within_two_ticks_and_not_before() {
    let dir = root("g3");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let clock = Clock::at(1_000);
    let k = key(0x53);
    begin(&c, "T", &k, clock.now());
    let a = actor("T", &k);
    staged(c.tranche_stage(&a, one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!("orphan"))]), clock.now()));
    // THE HOLDER, DETACHED: its join polls the record and is never waited on.
    {
        let c2 = c.clone();
        let a2 = a.clone();
        let now = clock.now();
        std::thread::spawn(move || {
            let _ = c2.tranche_commit(&a2, now);
        });
    }
    wait_until(&c, "T", Duration::from_secs(5), |st| st == TrancheState::Landing);
    // NO TICK for 300 ms: nothing lands (the period named: a request-side
    // lander runs in single-digit milliseconds on this fixture).
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(state_of(&c, "T"), TrancheState::Landing);
    assert_eq!(landing_columns(&dir, "T"), (false, 0, None), "undecided before the first tick");
    assert_eq!(title_of(&c, &x), json!("x"));
    // The first tick spawns the lander; within two ticks it is landed.
    let t1 = tick(&c, clock.now());
    assert_eq!(t1.landers_spawned, vec![("T".to_string(), 1)]);
    let started = Instant::now();
    while state_of(&c, "T") != TrancheState::Landed {
        assert!(started.elapsed() < Duration::from_secs(5), "never landed");
        std::thread::sleep(Duration::from_millis(5));
    }
    let t2 = tick(&c, clock.now());
    assert!(t2.landers_spawned.is_empty(), "{t2:?}");
    let reply = c.tranche_commit(&a, clock.now()).expect("the re-issued commit answers the record");
    assert_eq!(reply.state, TrancheState::Landed);
    assert_eq!(reply.children.len(), 1);
    assert_eq!(title_of(&c, &x), json!("orphan"));
}

// ---------------------------------------------------------------------------
// g4 — `landing_error` never holds the mutex across the backoff (test 25's
// second half)
// ---------------------------------------------------------------------------

/// A foreign UNDECIDED row for child 0's hash at the decision → the lander
/// stops `child_in_flight {colliding_seq}` and releases the mutex; a
/// `rollback` on a second thread during the backoff is answered
/// `tranche_past_rollback` within one poll interval — asserted at less than
/// half the backoff — not after it. The foreign row aborts; past the backoff
/// the tranche lands.
// RED (measured): `run_lander` sleeping out `next_retry_ms` while
// holding the slot guard → the rollback waits the whole backoff (elapsed ≥
// backoff/2 fires).
#[test]
fn g4_a_landing_error_releases_the_mutex_so_a_rollback_during_the_backoff_is_answered_at_once() {
    let dir = root("g4");
    let mut cfg = quick();
    // The real wall clock must be able to see "the whole backoff": make the
    // backoff floor the thing being outlasted, and the poll quick.
    cfg.lock_poll_interval = Duration::from_millis(2);
    let c = fresh_with(&dir, cfg);
    let x = doc(&c, "x", "draft");
    let y = doc(&c, "y", "draft");
    let clock = Clock::at(1_000);
    let k = key(0x54);
    begin(&c, "T", &k, clock.now());
    let a = actor("T", &k);
    let c0 = one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!("x1"))]);
    let c1 = one("s1", SET_TITLE, &[("id", json!(y)), ("t", json!("y1"))]);
    staged(c.tranche_stage(&a, c0.clone(), clock.now()));
    staged(c.tranche_stage(&a, c1.clone(), clock.now()));
    // The foreign undecided row for child 0's hash (the door would have
    // answered it busy; appended by hand — the window named above).
    let foreign = {
        let mut db = SpineDb::open(&dir.join(SPINE_FILE)).unwrap();
        db.append_pending(&c0, &c0.hash_identity().unwrap(), &staged_payload(&dir, "T", 0)).unwrap()
    };
    let (first, _) = land(&c, &a, &clock);
    let err = match first {
        Err(SpineError::TrancheBusy { why, fields }) => {
            assert_eq!(why, "landing_in_progress");
            fields["landing_error"].clone()
        }
        other => panic!("expected busy landing_in_progress, got {other:?}"),
    };
    assert_eq!(err["why"], json!("child_in_flight"));
    assert_eq!(err["colliding_seq"], json!(foreign.0));
    assert_eq!(err["at_child"], json!(0));
    assert_eq!(landing_columns(&dir, "T").0, false, "undecided: the decision rolled back");
    assert!(child_seqs(&dir, "T").iter().all(|s| s.is_none()), "nothing reserved");
    // DURING THE BACKOFF (the clock has not moved; a tick now backs off),
    // a rollback on another thread is answered at once.
    let tick = tick(&c, clock.now());
    assert_eq!(tick.landers_backing_off, vec![("T".to_string(), 1)]);
    let started = Instant::now();
    let r = std::thread::scope(|s| s.spawn(|| c.tranche_rollback(&a, clock.now())).join().unwrap());
    let elapsed = started.elapsed();
    assert!(matches!(only_witness(r.expect_err("past rollback")), Witness::TranchePastRollback { .. }));
    assert!(
        elapsed < Duration::from_millis((LANDING_BACKOFF_FLOOR_MS / 2) as u64),
        "the rollback waited {elapsed:?}: the mutex was held across the backoff"
    );
    // The foreign row aborts; past the backoff the retry decides and lands.
    {
        let mut db = SpineDb::open(&dir.join(SPINE_FILE)).unwrap();
        assert!(db.mark_aborted_if_undecided(foreign, &json!({"aborted": true})).unwrap());
    }
    clock.advance(LANDING_BACKOFF_FLOOR_MS + 1);
    tick_until(&c, &clock, Duration::from_secs(10), || state_of(&c, "T") == TrancheState::Landed);
    let reply = c.tranche_commit(&a, clock.now()).expect("landed");
    assert_eq!(reply.children.len(), 2);
    assert!(!reply.children[0].deduplicated, "an aborted foreign row deduplicates nothing");
    assert_eq!(title_of(&c, &x), json!("x1"));
    assert_eq!(title_of(&c, &y), json!("y1"));
    assert_eq!(landing_columns(&dir, "T").2, None);
}

// ---------------------------------------------------------------------------
// g5 — two ticks, one lander
// ---------------------------------------------------------------------------

/// With the lander blocked on a held pool file (mutex held, registered), two
/// `tranche_tick`s fired on two threads spawn NO second lander: both report
/// it running; the total spawned over the tranche's life is 1 until the
/// stop. OPPOSED: after the stop a tick past the backoff spawns again.
// RED (measured): the in-process registry removed (every tick
// spawns) → the two concurrent ticks each spawn → total 3.
#[test]
fn g5_two_ticks_fired_together_while_the_lander_holds_the_mutex_spawn_no_second_lander() {
    let dir = root("g5");
    let mut cfg = quick();
    cfg.lock_wait_ceiling = Duration::from_secs(3);
    let c = fresh_with(&dir, cfg);
    let x = doc(&c, "x", "draft");
    let clock = Clock::at(1_000);
    let k = key(0x55);
    begin(&c, "T", &k, clock.now());
    let a = actor("T", &k);
    staged(c.tranche_stage(&a, one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!("once"))]), clock.now()));
    let hold = PoolHold::take(&dir);
    let reply = std::thread::scope(|s| {
        let h = s.spawn(|| c.tranche_commit(&a, clock.now()));
        wait_until(&c, "T", Duration::from_secs(5), |st| st == TrancheState::Landing);
        let t1 = tick(&c, clock.now());
        assert_eq!(t1.landers_spawned, vec![("T".to_string(), 1)]);
        // The lander is now blocked in its landing on the held file, holding
        // the mutex. Two ticks at once:
        let (ta, tb) = std::thread::scope(|s2| {
            let ha = s2.spawn(|| tick(&c, clock.now()));
            let hb = s2.spawn(|| tick(&c, clock.now()));
            (ha.join().unwrap(), hb.join().unwrap())
        });
        assert_eq!(spawned(&[ta.clone(), tb.clone()]), 0, "no second lander: {ta:?} {tb:?}");
        assert_eq!(ta.landers_running, vec![("T".to_string(), 1)]);
        assert_eq!(tb.landers_running, vec![("T".to_string(), 1)]);
        // A child in landing is refused at the door, not parked behind the
        // held mutex ("the legality check runs twice").
        let started = Instant::now();
        let r = c.tranche_stage(&a, one("s9", SET_TITLE, &[("id", json!(x)), ("t", json!("late"))]), clock.now());
        assert!(matches!(only_witness(r.expect_err("refused")), Witness::TranchePastRollback { .. }));
        assert!(started.elapsed() < Duration::from_secs(1), "refused at the door, not after the landing");
        hold.release();
        tick_until(&c, &clock, Duration::from_secs(10), || state_of(&c, "T") == TrancheState::Landed);
        h.join().unwrap()
    });
    busy_or_landed(reply, "the first commit");
    assert_eq!(c.tranche_commit(&a, clock.now()).expect("landed").state, TrancheState::Landed);
    assert_eq!(title_of(&c, &x), json!("once"));
}

// ---------------------------------------------------------------------------
// g6 — the internal landing path takes no fresh lock
// ---------------------------------------------------------------------------

/// Over the tranche's life, sampled by a watcher thread every 2 ms from the
/// flip to `landed`: every lock row's `seq` is the anchor's; no reserved
/// child row's `seq` ever appears in `locks`; after `landed` no lock row
/// remains. The children land UNDER THE ANCHOR'S rows — what the tranche gate's
/// exemption by `seq` relies on. Deterministic mid-landing sample too: the
/// lander held on the pool file after reserving.
// RED (measured): `exec::land` enrolling a lock row under the
// reserved child's own `seq` before its landing (the shape `execute` would
// take) → the watcher sees a foreign seq / the child's seq in `locks`.
#[test]
fn g6_the_landing_enrols_no_lock_row_under_any_seq_but_the_anchors_and_releases_them_all_at_the_clear() {
    let dir = root("g6");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let y = doc(&c, "y", "draft");
    let clock = Clock::at(1_000);
    let k = key(0x56);
    begin(&c, "T", &k, clock.now());
    let a = actor("T", &k);
    staged(c.tranche_stage(&a, one("s0", SET_TITLE, &[("id", json!(x)), ("t", json!("x1"))]), clock.now()));
    staged(c.tranche_stage(&a, one("s1", SET_TITLE, &[("id", json!(y)), ("t", json!("y1"))]), clock.now()));
    let anchor = anchor_seq(&c, "T");
    let held_before: i64 = spine_sql(&dir)
        .query_row("SELECT COUNT(*) FROM locks WHERE seq = ?1", rusqlite::params![anchor.0], |r| r.get(0))
        .unwrap();
    assert!(held_before >= 2, "the tranche holds its children's claims: {held_before}");
    let hold = PoolHold::take(&dir);
    let stop = AtomicBool::new(false);
    let (reply, foreign_seen, child_seen) = std::thread::scope(|s| {
        let _release_watcher = SetOnDrop(&stop);
        let watcher = s.spawn(|| {
            let sql = spine_sql(&dir);
            let mut foreign: Vec<i64> = Vec::new();
            let mut sampled = 0u64;
            while !stop.load(Ordering::SeqCst) {
                let mut stmt = sql.prepare("SELECT DISTINCT seq FROM locks").unwrap();
                for seq in stmt.query_map([], |r| r.get::<_, i64>(0)).unwrap() {
                    let seq = seq.unwrap();
                    if seq != anchor.0 && !foreign.contains(&seq) {
                        foreign.push(seq);
                    }
                }
                sampled += 1;
                std::thread::sleep(Duration::from_millis(2));
            }
            (foreign, sampled)
        });
        let h = s.spawn(|| c.tranche_commit(&a, clock.now()));
        wait_until(&c, "T", Duration::from_secs(5), |st| st == TrancheState::Landing);
        tick(&c, clock.now());
        let started = Instant::now();
        while !landing_columns(&dir, "T").0 {
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(5));
        }
        // Mid-landing, deterministic: reserved, blocked on the pool file.
        let seqs: Vec<i64> = child_seqs(&dir, "T").into_iter().map(|s| s.expect("reserved")).collect();
        let sql = spine_sql(&dir);
        let others: i64 = sql
            .query_row("SELECT COUNT(*) FROM locks WHERE seq != ?1", rusqlite::params![anchor.0], |r| r.get(0))
            .unwrap();
        assert_eq!(others, 0, "mid-landing: every lock row is the anchor's");
        hold.release();
        clock.advance(LANDING_BACKOFF_FLOOR_MS * 2);
        tick_until(&c, &clock, Duration::from_secs(10), || state_of(&c, "T") == TrancheState::Landed);
        let reply = h.join().unwrap();
        stop.store(true, Ordering::SeqCst);
        let (foreign, sampled) = watcher.join().unwrap();
        assert!(sampled > 10, "the watcher sampled {sampled} times");
        let child_seen: Vec<i64> = foreign.iter().copied().filter(|s| seqs.contains(s)).collect();
        (reply, foreign, child_seen)
    });
    busy_or_landed(reply, "the first commit");
    assert_eq!(c.tranche_commit(&a, clock.now()).expect("landed").state, TrancheState::Landed);
    assert!(foreign_seen.is_empty(), "lock rows under a seq that is not the anchor's: {foreign_seen:?}");
    assert!(child_seen.is_empty(), "a child row's seq in `locks`: {child_seen:?}");
    let remaining: i64 = spine_sql(&dir).query_row("SELECT COUNT(*) FROM locks", [], |r| r.get(0)).unwrap();
    assert_eq!(remaining, 0, "every lock row released at the clear");
    // OPPOSED: a foreign writer DOES enrol its own seq (the instrument can see one).
    let stop2 = AtomicBool::new(false);
    let (saw, _) = std::thread::scope(|s| {
        let stop = &stop2;
        let _release_watcher = SetOnDrop(stop);
        let w = s.spawn(|| {
            let sql = spine_sql(&dir);
            let mut saw = false;
            while !stop.load(Ordering::SeqCst) {
                let n: i64 = sql.query_row("SELECT COUNT(*) FROM locks", [], |r| r.get(0)).unwrap();
                saw |= n > 0;
                std::thread::yield_now();
            }
            saw
        });
        for i in 0..40 {
            seed(&c, SET_TITLE, &[("id", json!(x)), ("t", json!(format!("w{i}")))]);
        }
        stop.store(true, Ordering::SeqCst);
        (w.join().unwrap(), ())
    });
    assert!(saw, "the watcher's predicate can see a lock row when a foreign writer takes one");
}

// ---------------------------------------------------------------------------
// g7 — the degraded regime's population: a `landing` tranche
// is never abandoned; a `queued` one IS, at the same deadline, on the same
// tick
// ---------------------------------------------------------------------------

/// A `landing` (held on the pool file) past its deadline and B `queued`
/// behind it past the same deadline: one tick abandons B and only B; A is
/// still `landing` and lands once released. (Only one non-queued
/// tranche is admitted at a time, so the second tranche of this pair is
/// `queued`, which enforcement treats as it does `staging`; a `staging` second tranche is
/// unreachable while A is live.)
// RED (measured): the enforcement's `state IN (…)` widened to
// `landing` → A abandoned under its lander, its reserved child row stranded.
// OPPOSED: B, in the enforced regime, is abandoned by the same tick.
#[test]
fn g7_a_landing_tranche_past_its_deadline_is_never_abandoned_while_a_queued_one_is_on_the_same_tick() {
    let dir = root("g7");
    let c = fresh(&dir);
    let x = doc(&c, "x", "draft");
    let clock = Clock::at(1_000);
    let timeout = c.config().tranche.timeout.as_millis() as i64;
    let ka = key(0x57);
    begin(&c, "A", &ka, clock.now());
    let a = actor("A", &ka);
    staged(c.tranche_stage(&a, child_of("A", "s0", SET_TITLE, &[("id", json!(x)), ("t", json!("a"))]), clock.now()));
    let kb = key(0x58);
    let b = begin(&c, "B", &kb, clock.now());
    assert_eq!(b.state, TrancheState::Queued);
    let hold = PoolHold::take(&dir);
    let reply = std::thread::scope(|s| {
        let h = s.spawn(|| c.tranche_commit(&a, clock.now()));
        wait_until(&c, "A", Duration::from_secs(5), |st| st == TrancheState::Landing);
        let t1 = tick(&c, clock.now());
        assert_eq!(t1.landers_spawned, vec![("A".to_string(), 1)]);
        // Wait for A's lander to STOP (`file_held` on the row) so A's mutex is
        // free at the enforcing tick: enforcement `try_lock`s each lapsed row
        // and skips a held one as busy, so with the lander still blocked on
        // the pool file "A not abandoned" would be the skip, not the gate.
        let started = Instant::now();
        while landing_columns(&dir, "A").2.is_none() {
            assert!(started.elapsed() < Duration::from_secs(5), "A's lander never stopped file_held");
            std::thread::sleep(Duration::from_millis(5));
        }
        // Both deadlines lapse. One tick.
        clock.advance(timeout + 1);
        let tick = tick(&c, clock.now());
        assert!(tick.skipped_busy.is_empty(), "A was skipped as busy, not spared by the gate: {tick:?}");
        assert_eq!(tick.abandoned, vec![("B".to_string(), 1)], "B and only B: {tick:?}");
        assert_eq!(state_of(&c, "A"), TrancheState::Landing);
        assert_eq!(state_of(&c, "B"), TrancheState::Abandoned);
        hold.release();
        clock.advance(LANDING_BACKOFF_FLOOR_MS * 2);
        tick_until(&c, &clock, Duration::from_secs(10), || state_of(&c, "A") == TrancheState::Landed);
        h.join().unwrap()
    });
    busy_or_landed(reply, "A's first commit");
    assert_eq!(c.tranche_commit(&a, clock.now()).expect("A landed").state, TrancheState::Landed);
    assert_eq!(title_of(&c, &x), json!("a"));
}

// ---------------------------------------------------------------------------
// g8 — order and the five columns
// ---------------------------------------------------------------------------

/// `N` children land as `N` rows with `tranche_ordinal` 0…N−1,
/// `tranche_index` = ordinal, `tranche_of` = N, `committed_at` ascending in
/// ordinal order; with child 1 of 3 deduplicated at the decision the two
/// rows carry `index` 0 and 1 and `of = 2`.
// RED (measured): `of` written as the staged count (3) → the dedup
// case reads `of = 3`; `index` written as the ordinal → the dedup case's
// second row reads `index = 2`.
#[test]
fn g8_n_children_land_as_n_rows_in_order_with_index_and_of_counting_only_the_children_that_land() {
    let clock = Clock::at(1_000);
    // ---- three children, none deduplicated ----
    let dir = root("g8a");
    let c = fresh(&dir);
    let ids: Vec<String> = (0..3).map(|i| doc(&c, &format!("d{i}"), "draft")).collect();
    let k = key(0x61);
    begin(&c, "T", &k, clock.now());
    let a = actor("T", &k);
    for (i, id) in ids.iter().enumerate() {
        staged(c.tranche_stage(&a, one(&format!("s{i}"), SET_TITLE, &[("id", json!(id)), ("t", json!(format!("t{i}")))]), clock.now()));
    }
    let (reply, _) = land(&c, &a, &clock);
    let reply = reply.expect("lands");
    let mut last_at = 0u64;
    for (i, child) in reply.children.iter().enumerate() {
        let row = c.transaction(Seq(child.seq)).unwrap();
        let mark = row.tranche.expect("mark");
        assert_eq!((mark.ordinal, mark.index, mark.of), (Some(i as u32), Some(i as u32), Some(3)));
        let at = row.committed_at.expect("stamped");
        assert!(at > last_at, "committed_at ascending: {at} after {last_at}");
        last_at = at;
        assert_eq!(title_of(&c, &ids[i]), json!(format!("t{i}")));
    }
    // ---- child 1 of 3 deduplicated ----
    let dir = root("g8b");
    let c = fresh(&dir);
    let ids: Vec<String> = (0..3).map(|i| doc(&c, &format!("d{i}"), "draft")).collect();
    let k = key(0x62);
    begin(&c, "T", &k, clock.now());
    let a = actor("T", &k);
    let envs: Vec<Envelope> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| one(&format!("s{i}"), SET_TITLE, &[("id", json!(id)), ("t", json!(format!("t{i}")))]))
        .collect();
    for env in &envs {
        staged(c.tranche_stage(&a, env.clone(), clock.now()));
    }
    let foreign = land_foreign(&dir, &envs[1], &staged_payload(&dir, "T", 1));
    let (reply, _) = land(&c, &a, &clock);
    let reply = reply.expect("lands");
    assert_eq!(reply.children.len(), 3);
    assert!(reply.children[1].deduplicated && reply.children[1].seq == foreign.0);
    let expect = [(0u32, Some(0u32)), (2, Some(1))];
    let mut last_at = 0u64;
    for (ordinal, index) in expect {
        let child = &reply.children[ordinal as usize];
        assert!(!child.deduplicated);
        let row = c.transaction(Seq(child.seq)).unwrap();
        let mark = row.tranche.expect("mark");
        assert_eq!((mark.ordinal, mark.index, mark.of), (Some(ordinal), index, Some(2)), "ordinal {ordinal}");
        let at = row.committed_at.expect("stamped");
        assert!(at > last_at);
        last_at = at;
    }
    assert!(c.transaction(Seq(foreign.0)).unwrap().tranche.is_none(), "the foreign row carries no mark");
}

// ---------------------------------------------------------------------------
// g9 — renumbering through ONE map
// ---------------------------------------------------------------------------

/// A provisional id minted in child 0 and used (by `MATCH {id}`) in child 2
/// resolves to the SAME serial in both landed rows, in `commit`'s
/// `renumbered`, and in the pool; a second mint in child 1 takes the next
/// serial; the touched set's lock rows named the serial at the clear (none
/// remain). OPPOSED: a child minting nothing leaves `renumbered` empty.
// RED (measured): the map built per child (`mint_serials` called
// once per payload) → child 2's SET still names `doc-~…`; its landing
// refuses (`provisional_id_at_landing`) or lands on a node that does not
// exist.
#[test]
fn g9_a_provisional_id_minted_in_child_0_and_used_in_child_2_resolves_to_one_serial_through_one_map() {
    let dir = root("g9");
    let c = fresh(&dir);
    let clock = Clock::at(1_000);
    let k = key(0x63);
    begin(&c, "T", &k, clock.now());
    let a = actor("T", &k);
    let (_, m0) = staged(c.tranche_stage(&a, one("s0", r#"CREATE (d: fixture-alpha/doc { title: "first", state: "draft" })"#, &[]), clock.now()));
    let (_, m1) = staged(c.tranche_stage(&a, one("s1", r#"CREATE (e: fixture-alpha/doc { title: "second", state: "draft" })"#, &[]), clock.now()));
    let prov0 = m0["d"].strip_prefix("fixture-alpha/").unwrap().to_string();
    let prov1 = m1["e"].strip_prefix("fixture-alpha/").unwrap().to_string();
    assert!(prov0.starts_with("doc-~") && prov1.starts_with("doc-~"));
    staged(c.tranche_stage(&a, one("s2", SET_TITLE, &[("id", json!(prov0)), ("t", json!("renamed"))]), clock.now()));
    let (reply, _) = land(&c, &a, &clock);
    let reply = reply.expect("lands");
    assert_eq!(reply.children.len(), 3);
    // ONE map: both provisionals, each to a serial; the two serials differ.
    let s0 = reply.renumbered.get(&format!("fixture-alpha/{prov0}")).expect("prov0 renumbered").clone();
    let s1 = reply.renumbered.get(&format!("fixture-alpha/{prov1}")).expect("prov1 renumbered").clone();
    assert_eq!(reply.renumbered.len(), 2);
    assert_ne!(s0, s1);
    let serial0 = s0.strip_prefix("fixture-alpha/").unwrap();
    let serial1 = s1.strip_prefix("fixture-alpha/").unwrap();
    assert!(serial0.starts_with("doc-") && !serial0.contains('~'), "{serial0}");
    assert!(serial1.starts_with("doc-") && !serial1.contains('~'), "{serial1}");
    // The replies' minted maps carry the serials.
    assert_eq!(reply.children[0].minted["d"], s0);
    assert_eq!(reply.children[1].minted["e"], s1);
    // The landed rows name the serial — child 0's CREATE and child 2's SET.
    let row0 = c.transaction(Seq(reply.children[0].seq)).unwrap();
    let row2 = c.transaction(Seq(reply.children[2].seq)).unwrap();
    let names = |p: &spine::delta::CompiledPayload| -> Vec<String> {
        p.parts
            .iter()
            .flat_map(|part| part.deltas.iter())
            .map(|d| match d {
                spine::delta::Delta::CreateNode { id, .. } => id.0.clone(),
                spine::delta::Delta::SetField { node, .. } => node.0.clone(),
                other => panic!("{other:?}"),
            })
            .collect()
    };
    assert_eq!(names(&row0.payload), vec![serial0.to_string()]);
    assert_eq!(names(&row2.payload), vec![serial0.to_string()], "child 2 names child 0's serial");
    assert!(!serde_json::to_string(&row2.payload).unwrap().contains('~'), "no provisional survives");
    // The pool holds doc-<n> with the renamed title; the provisional does not exist.
    assert_eq!(title_of(&c, serial0), json!("renamed"));
    assert_eq!(title_of(&c, serial1), json!("second"));
    assert_eq!(title_of(&c, &prov0), Value::Null);
    // And child 2's bound identities were rewritten too (the RETURN record).
    let bound: String = spine_sql(&dir)
        .query_row("SELECT bound FROM tx_bound WHERE seq = ?1", rusqlite::params![row2.seq.0], |r| r.get(0))
        .unwrap();
    assert!(bound.contains(serial0) && !bound.contains('~'), "{bound}");
    // OPPOSED: a tranche minting nothing has an empty map.
    let x = doc(&c, "x", "draft");
    let k2 = key(0x64);
    begin(&c, "T2", &k2, clock.now());
    let a2 = actor("T2", &k2);
    staged(c.tranche_stage(&a2, child_of("T2", "s0", SET_TITLE, &[("id", json!(x)), ("t", json!("plain"))]), clock.now()));
    let (reply, _) = land(&c, &a2, &clock);
    assert!(reply.expect("lands").renumbered.is_empty());
}

// ---------------------------------------------------------------------------
// the lander's seam — present under `fault-injection`, refusing by name
// without it (the `atomicity_kill.rs` shape, out of process); `crash.rs` proves
// the crash windows
// ---------------------------------------------------------------------------

/// The four lander stages round-trip their frozen names, are disjoint from
/// the door's four, and the seam accepts them: a child `spine_victim` with
/// `SPINE_FAULT=before-decision` running an ORDINARY door job — under the
/// feature — commits and exits `EXIT_UNREACHED` (4), its contract for "an
/// armed stage this job never reached": the gate parsed the lander's name
/// (a misspelling is `seam_no_such_stage`, exit 3) and the door's pipeline
/// never reaches a lander stage; without the feature it is refused by name
/// at the door (exit 3, `kind=fault_seam_refused value=before-decision`).
// RED (measured): `Named::parse` not consulting `LanderStage` → the
// armed victim dies `seam_no_such_stage` (exit 3, not 0); a lander name
// colliding with a door name → the disjointness assertion.
#[test]
fn the_lander_seam_speaks_four_stages_disjoint_from_the_doors_and_the_victim_shows_which_build_carries_it() {
    let named = [
        (LanderStage::BeforeDecision, "before-decision"),
        (LanderStage::AfterDecision, "after-decision"),
        (LanderStage::BetweenChildren, "between-children"),
        (LanderStage::BeforeClear, "before-clear"),
    ];
    for (stage, name) in named {
        assert_eq!(stage.name(), name);
        assert_eq!(LanderStage::parse(name), Some(stage));
        assert_eq!(spine::recover::Stage::parse(name), None, "disjoint from the door's vocabulary");
    }
    assert_eq!(LanderStage::ALL.map(|s| s.name()), named.map(|(_, n)| n));
    assert_eq!(LanderStage::parse("before_decision"), None);
    // Both unset: silent in both builds.
    assert!(spine::fault::at_lander_stage(LanderStage::BeforeDecision).is_ok());

    // Out of process: the victim, armed at a LANDER stage, running a door job.
    let Some(victim) = option_env!("CARGO_BIN_EXE_spine_victim") else {
        panic!("cargo did not set CARGO_BIN_EXE_spine_victim for this test target");
    };
    let dir = root("seam");
    let _ = std::fs::remove_dir_all(&dir);
    let out = std::process::Command::new(victim)
        .env("SPINE_VICTIM_DIR", dir.as_os_str())
        .env("SPINE_VICTIM_JOB", "repeatable")
        .env("SPINE_FAULT", "before-decision")
        .env_remove(spine::fault::PAUSE_ENV)
        .env_remove(spine::fault::PAUSE_LATCH_ENV)
        .env_remove(spine::fault::PAUSE_CEILING_ENV)
        .env("SPINE_VICTIM_EXPECT_FEATURE", if cfg!(feature = "fault-injection") { "1" } else { "0" })
        .output()
        .expect("spawn the victim");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(out.status.code(), Some(7), "a stale victim of the other build (EXIT_WRONG_BUILD): {stderr}");
    if cfg!(feature = "fault-injection") {
        assert_eq!(
            out.status.code(),
            Some(4),
            "the armed gate accepts a lander stage, the door job commits, and no stage aborted (EXIT_UNREACHED): {stdout} {stderr}"
        );
        assert!(stdout.contains("outcome=committed"), "{stdout}");
        assert!(stderr.contains("no stage aborted this process"), "{stderr}");
        assert!(!stdout.contains("seam_no_such_stage"), "the lander's name parsed: {stdout}");
    } else {
        assert_eq!(out.status.code(), Some(3), "the production door refuses the set seam by name: {stdout} {stderr}");
        assert!(stdout.contains("kind=fault_seam_refused") && stdout.contains("value=before-decision"), "{stdout}");
    }
}
