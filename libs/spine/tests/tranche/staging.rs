//! STAGING a child against the view, the GROWING claim set, the read
//! under key, the flag rule per child, the size cap — and THE STATE SLICE a
//! filtered scan read and the projected namespace under a tranche (tests
//! 37–40).
//!
//! Every guard names its forcing in a `// RED:` comment; for the LANDING
//! halves of 37(a), 40(b) and 40(c) the tranche is ended by
//! `commit` through the scheduler's tick (`land`), never by `rollback`
//! standing in. Tests are named after the case
//! number they own (`t19_<n>_…`) or the guard (`g<n>_…`). Every act
//! takes an explicit `now_ms`; the one wait a test crosses (g5) is bounded by
//! the `lock_wait_ceiling` the test itself configures, and the test asserts
//! it was NOT waited out.

use serde_json::{json, Value};
use spine::config::SpineConfig;
use spine::envelope::{
    Envelope, Initiator, Instrument, Mode, Provenance, RippleKind, TxHash,
    ENVELOPE_FORMAT_VERSION,
};
use spine::error::SpineError;
use spine::locks::{self, Claim, ClaimSet, LockMode, Resource};
use spine::pool::{NodeId, Pool};
use spine::project;
use spine::schema_def::{
    fixtures, EdgeKindDef, FieldDef, NodeTypeDef, PoolGrammar, PoolKind, PoolName, TypeRef,
};
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use spine::tranche::{
    ChildOutcome, QuietLevels, TrancheActor, TrancheBegin, TrancheLockMode, TrancheState,
};
use spine::witness::Witness;
use spine::{Cauldron, Outcome, ReadRequest};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const QUIET: QuietLevels = QuietLevels::BusNotConfigured;
const SERVICE: &str = "runner";
const ALPHA: &str = "fixture-alpha";
const BOARD: &str = "board";

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-staging-{name}-{}", std::process::id()))
}

fn fresh_with(dir: &Path, grammars: &[PoolGrammar], config: SpineConfig) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(dir, grammars, config).expect("create the cauldron")
}

/// Fixture ALPHA with `doc` taking a SERIAL id (`id_prefix`): a
/// CREATE then mints a PROVISIONAL `doc-~<16 hex>` the decision renumbers —
/// the id shape g1 asserts. `author` stays unprefixed.
fn alpha() -> PoolGrammar {
    let mut g = fixtures::fixture_alpha();
    g.node_types.get_mut("doc").expect("doc").id_prefix = Some("doc".into());
    g
}

fn fresh(dir: &Path) -> Cauldron {
    fresh_with(dir, &[alpha()], SpineConfig::default())
}

/// A pool carrying `meta.blocking` on `question` (the flag table in
/// `spine::flags`), the shape `tests/ripple_initiator.rs` uses.
fn board() -> PoolGrammar {
    let mut node_types = BTreeMap::new();
    node_types.insert(
        "question".into(),
        NodeTypeDef {
            fields: BTreeMap::from([
                ("title".to_string(), FieldDef::required()),
                ("meta".to_string(), FieldDef::optional()),
            ]),
            ..Default::default()
        },
    );
    PoolGrammar {
        pool: PoolName::new(BOARD),
        kind: PoolKind::new("board"),
        version: 1,
        node_types,
        edge_kinds: BTreeMap::new(),
    }
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

/// A child's envelope: a type-one or type-two step writing as itself,
/// its nonce the step id, as the ripple tool mints them.
fn child(kind: RippleKind, step: &str, scope: &[&str], statement: &str, params: &[(&str, Value)], mode: Mode) -> Envelope {
    Envelope {
        statement: statement.to_string(),
        bindings: params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Ripple {
                kind,
                run_id: "T".into(),
                step_id: step.to_string(),
                scope: scope.iter().map(|s| s.to_string()).collect(),
            },
            instrument: instrument(),
        },
        mode,
        idempotency_nonce: Some(step.to_string()),
    }
}

fn one(step: &str, statement: &str, params: &[(&str, Value)]) -> Envelope {
    child(RippleKind::TypeOne, step, &[], statement, params, Mode::Commit)
}

fn two(step: &str, scope: &[&str], statement: &str, params: &[(&str, Value)]) -> Envelope {
    child(RippleKind::TypeTwo, step, scope, statement, params, Mode::Commit)
}

/// An ordinary (keyless) commit by the soup's own machinery — the committed
/// state a tranche is layered on.
fn seed(c: &Cauldron, statement: &str, params: &[(&str, Value)]) -> Outcome {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let env = Envelope {
        statement: statement.to_string(),
        bindings: params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::CoreMechanical {
                operation: "test-seed".into(),
            },
            instrument: instrument(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(format!("seed-{}", N.fetch_add(1, std::sync::atomic::Ordering::Relaxed))),
    };
    c.submit(env).expect("seed commit")
}

fn staged(o: spine::error::Result<ChildOutcome>) -> (u32, BTreeMap<String, String>, bool) {
    match o {
        Ok(ChildOutcome::Staged {
            ordinal,
            minted,
            changed,
            ..
        }) => (ordinal, minted, changed),
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

fn rows_of(r: spine::Returned) -> Vec<BTreeMap<String, Value>> {
    r.0
}

/// The committed read — WITHOUT the key.
fn committed(c: &Cauldron, statement: &str, params: &[(&str, Value)]) -> Vec<BTreeMap<String, Value>> {
    rows_of(c.read(&req(statement, params)).expect("committed read").rows)
}

/// The read under key.
fn under(c: &Cauldron, a: &TrancheActor, statement: &str, params: &[(&str, Value)], now: i64) -> spine::read::TrancheRead {
    c.tranche_read(a, &req(statement, params), now)
        .unwrap_or_else(|e| panic!("read under key: {e:?}"))
}

/// `<pool>/<id>` → `<id>`.
fn id_of(minted: &BTreeMap<String, String>, binding: &str) -> String {
    let q = minted.get(binding).unwrap_or_else(|| panic!("minted has no `{binding}`: {minted:?}"));
    let (pool, id) = q.split_once('/').expect("pool/id");
    assert_eq!(pool, ALPHA);
    id.to_string()
}

fn staged_count(c: &Cauldron, t: &str) -> (TrancheState, usize) {
    let s = c.tranche_store(t, None).expect("store");
    (s.state, s.children.len())
}

/// The touched set as `(resource key, mode, granted)` for one pool.
fn touched(c: &Cauldron, t: &str, pool: &str) -> Vec<(String, String, bool)> {
    c.tranche_touched(t, 1)
        .expect("touched")
        .into_iter()
        .filter(|(p, _, _, _)| p == pool)
        .map(|(_, r, m, g)| (r, m, g))
        .collect()
}

fn has(touched: &[(String, String, bool)], resource: &str, mode: &str) -> bool {
    touched.iter().any(|(r, m, g)| r == resource && m == mode && *g)
}

fn spine_sql(dir: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("open spine.db")
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

fn body(id: &str) -> Resource {
    Resource::Body {
        node: NodeId(id.to_string()),
    }
}

// ---------------------------------------------------------------------------
// g1 — THE VIEW: child k's MATCH sees child k−1's SET and CREATE (with a
// provisional id); a read WITHOUT the key does not. Plus what rides on the
// view: the read under key locks what it read and reports
// `children_seen`; `validate` stops after compile;
// dedup in three places and the door's other direction.
// ---------------------------------------------------------------------------

/// RED (measured): `exec::stage`'s step 10 — the overlay apply —
/// skipped: child 1's `MATCH {id: <provisional>}` binds nothing (`changed:
/// false`), the read under key answers no rows, and the scan sees nothing.
/// OPPOSED: the committed read (no key) stays empty throughout.
#[test]
fn g1_child_k_matches_child_k_minus_1s_create_and_set_through_the_view_and_a_keyless_read_does_not() {
    let dir = root("g1");
    let c = fresh(&dir);
    // A step landed OUTSIDE the tranche, for the landed-elsewhere dedup.
    let landed = one("landed-before", r#"CREATE (a: fixture-alpha/author { name: "ann" })"#, &[]);
    let landed_seq = match c.submit(landed.clone()).expect("an ordinary commit") {
        Outcome::Committed { seq, .. } => seq,
        other => panic!("{other:?}"),
    };

    let k = key(0x11);
    let b = begin(&c, "T", &k, 1_000);
    assert_eq!(b.state, TrancheState::Open);
    let a = actor("T", &k);

    // child 0: CREATE — a provisional identifier is minted.
    let c0 = one("s0", r#"CREATE (d: fixture-alpha/doc { title: "first", state: "draft" })"#, &[]);
    let (ord0, minted0, changed0) = staged(c.tranche_stage(&a, c0.clone(), 1_001));
    assert_eq!((ord0, changed0), (0, true));
    let d = id_of(&minted0, "d");
    assert!(d.starts_with("doc-~") && d.len() == "doc-~".len() + 16, "provisional id: {d}");
    assert_eq!(staged_count(&c, "T"), (TrancheState::Staging, 1), "open → staging on the first child");

    // child 1: MATCH by the PROVISIONAL id, SET — sees child 0 through the view.
    let c1 = one(
        "s1",
        "MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t",
        &[("id", json!(d)), ("t", json!("second"))],
    );
    let (ord1, minted1, changed1) = staged(c.tranche_stage(&a, c1.clone(), 1_002));
    assert_eq!((ord1, changed1), (1, true), "child 1 bound child 0's node and wrote it");
    assert!(minted1.is_empty());

    // The read under key: the scan unites the overlay and the committed
    // pool, the SET is visible, `children_seen` counts both children.
    let r = under(&c, &a, "MATCH (d: fixture-alpha/doc) RETURN d.id, d.title", &[], 1_003);
    assert_eq!(r.children_seen, 2);
    assert_eq!(r.restarts, 0);
    let rows = rows_of(r.rows);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["d.id"], json!(d));
    assert_eq!(rows[0]["d.title"], json!("second"));
    // …and it LOCKED what it read: the body it bound and the type
    // slice it scanned are in the touched set, granted, never released.
    let t = touched(&c, "T", ALPHA);
    assert!(has(&t, &format!("body:{d}"), "write"), "child 0's write claim: {t:?}");
    assert!(has(&t, "tslice:doc", "link"), "child 0's create link: {t:?}");
    assert!(has(&t, "tslice:doc", "read"), "the read's scan claim: {t:?}");
    assert!(t.iter().all(|(_, _, g)| *g), "nothing queued: {t:?}");

    // OPPOSED: without the key, the committed pool alone — nothing.
    assert!(committed(&c, "MATCH (d: fixture-alpha/doc) RETURN d.id", &[]).is_empty());
    assert!(committed(&c, "MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.id", &[("id", json!(d))]).is_empty());

    // `validate` under key: the same validator against the view, staging nothing.
    let v = child(
        RippleKind::TypeOne,
        "s2-dry",
        &[],
        "MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t",
        &[("id", json!(d)), ("t", json!("third"))],
        Mode::Validate,
    );
    match c.tranche_stage(&a, v, 1_004) {
        Ok(ChildOutcome::Validated { .. }) => {}
        other => panic!("expected validated, got {other:?}"),
    }
    assert_eq!(staged_count(&c, "T"), (TrancheState::Staging, 2), "validate staged nothing");
    let r = under(&c, &a, "MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.title", &[("id", json!(d))], 1_005);
    assert_eq!(rows_of(r.rows)[0]["d.title"], json!("second"), "validate wrote nothing to the view");

    // DEDUP, three places. (1) This tranche's own child: the original
    // ordinal, `deduplicated`, nothing staged.
    match c.tranche_stage(&a, c1.clone(), 1_006) {
        Ok(ChildOutcome::Deduplicated { ordinal, .. }) => assert_eq!(ordinal, 1),
        other => panic!("expected deduplicated against child 1, got {other:?}"),
    }
    // (2) A step already landed on the soup: its original outcome, not a child.
    match c.tranche_stage(&a, landed, 1_007) {
        Ok(ChildOutcome::LandedElsewhere { seq, .. }) => assert_eq!(seq, landed_seq),
        other => panic!("expected landed-elsewhere, got {other:?}"),
    }
    assert_eq!(staged_count(&c, "T"), (TrancheState::Staging, 2));
    // (3) THE OTHER DIRECTION: an ordinary submit of a staged child's hash is
    // `busy` at the door, naming the tranche as the holder.
    match c.submit(c0) {
        Err(SpineError::Busy { message }) => {
            assert!(message.contains("tranche T"), "{message}");
            assert!(message.contains("staged child"), "{message}");
        }
        other => panic!("expected busy naming tranche T, got {other:?}"),
    }
    // Nothing landed through any of it.
    assert!(committed(&c, "MATCH (d: fixture-alpha/doc) RETURN d.id", &[]).is_empty());
    // The live store lists both children as `staged`, with child 0's minted.
    let s = c.tranche_store("T", None).unwrap();
    assert_eq!(s.children.iter().map(|ch| ch.state.as_str()).collect::<Vec<_>>(), ["staged", "staged"]);
    assert_eq!(s.children[0].minted, minted0);
}

// ---------------------------------------------------------------------------
// g2 — NOTHING TOUCHES A POOL FILE: bytes and mtime unchanged after N staged
// children and a read under key.
// ---------------------------------------------------------------------------

fn file_identity(path: &Path) -> (Vec<u8>, std::time::SystemTime) {
    let bytes = std::fs::read(path).expect("read the pool file");
    let mtime = std::fs::metadata(path).unwrap().modified().unwrap();
    (bytes, mtime)
}

fn wal_len(path: &Path) -> u64 {
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0)
}

/// RED (measured): `View::apply_child` ALSO applies the part to the
/// real pool — the file's bytes change and the committed read sees the
/// staged write. OPPOSED (the instrument can move): one keyless commit
/// afterwards changes the bytes.
#[test]
fn g2_three_staged_children_and_a_read_under_key_leave_every_pool_file_byte_and_mtime_unchanged() {
    let dir = root("g2");
    let c = fresh(&dir);
    let x = match seed(&c, r#"CREATE (d: fixture-alpha/doc { title: "committed", state: "draft" }) RETURN d.id"#, &[]) {
        Outcome::Committed { returned, .. } => returned.0[0]["d.id"].as_str().unwrap().to_string(),
        other => panic!("{other:?}"),
    };
    let ann = match seed(&c, r#"CREATE (a: fixture-alpha/author { name: "ann" }) RETURN a.id"#, &[]) {
        Outcome::Committed { returned, .. } => returned.0[0]["a.id"].as_str().unwrap().to_string(),
        other => panic!("{other:?}"),
    };
    let file = Pool::path_in(&dir, &PoolName::new(ALPHA));
    let before = file_identity(&file);
    assert!(!before.0.is_empty());
    let wal_before = wal_len(&file);

    let k = key(0x22);
    begin(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    // N = 3 children: a CREATE, a SET on a committed node, an edge onto it.
    let (_, m0, _) = staged(c.tranche_stage(&a, one("s0", r#"CREATE (d: fixture-alpha/doc { title: "new", state: "draft" })"#, &[]), 1_001));
    let d = id_of(&m0, "d");
    staged(c.tranche_stage(
        &a,
        one("s1", "MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t", &[("id", json!(x)), ("t", json!("moved in the view"))]),
        1_002,
    ));
    let (_, _, changed2) = staged(c.tranche_stage(
        &a,
        one(
            "s2",
            "MATCH (a: fixture-alpha/author { id: $a }) MATCH (d: fixture-alpha/doc { id: $d }) CREATE (a) -[wrote]-> (d)",
            &[("a", json!(ann)), ("d", json!(d))],
        ),
        1_003,
    ));
    assert!(!changed2, "an edge-only child reports `changed: false`");
    // The view shows all three; the count cell on `ann` moved in the view.
    let r = under(&c, &a, "MATCH (a: fixture-alpha/author { id: $a }) -[wrote]-> (d: fixture-alpha/doc) RETURN d.id, d.title", &[("a", json!(ann))], 1_004);
    let rows = rows_of(r.rows);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["d.id"], json!(d));
    let r = under(&c, &a, "MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.title", &[("id", json!(x))], 1_005);
    assert_eq!(rows_of(r.rows)[0]["d.title"], json!("moved in the view"));

    // THE MEASUREMENT: every byte and the mtime of the pool file, unchanged.
    let after = file_identity(&file);
    assert_eq!(before.0.len(), after.0.len(), "the pool file's length moved");
    assert!(before.0 == after.0, "the pool file's bytes moved");
    assert_eq!(before.1, after.1, "the pool file's mtime moved");
    assert_eq!(wal_len(&file), wal_before, "the write-ahead log grew");
    // And the committed read agrees: `x` is as seeded, no `d`, no edge.
    let rows = committed(&c, "MATCH (d: fixture-alpha/doc) RETURN d.id, d.title", &[]);
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0]["d.id"].clone(), rows[0]["d.title"].clone()), (json!(x), json!("committed")));
    assert!(committed(&c, "MATCH (a: fixture-alpha/author) -[wrote]-> (d: fixture-alpha/doc) RETURN d.id", &[]).is_empty());

    // OPPOSED: the instrument CAN move — a keyless commit on an UNTOUCHED
    // resource (another author) changes the bytes.
    seed(&c, r#"CREATE (a: fixture-alpha/author { name: "bob" })"#, &[]);
    let moved = file_identity(&file);
    assert!(moved.0 != after.0 || wal_len(&file) != wal_before, "a real commit must move the file");
}

// ---------------------------------------------------------------------------
// g3 — GROWTH RULE 1: a claim whose earlier row under the tranche's `seq` is
// only QUEUED is not treated as held. Plus the wait's ceiling: a child
// blocked by a foreign holder is `busy` with its queued rows withdrawn, and
// stages once the holder is gone.
// ---------------------------------------------------------------------------

fn quick() -> SpineConfig {
    let mut cfg = SpineConfig::default();
    cfg.lock_wait_ceiling = Duration::from_millis(400);
    cfg
}

/// RED (measured): `acquire_for_tranche` classifies against ALL rows
/// under the `seq` (granted or queued) — the queued row is read as held, the
/// child is staged holding a `write` on `x` that is only queued.
#[test]
fn g3_a_queued_row_under_the_tranches_seq_is_not_held_and_the_second_call_grants_it() {
    let dir = root("g3");
    let c = fresh_with(&dir, &[alpha()], quick());
    let x = match seed(&c, r#"CREATE (d: fixture-alpha/doc { title: "x", state: "draft" }) RETURN d.id"#, &[]) {
        Outcome::Committed { returned, .. } => returned.0[0]["d.id"].as_str().unwrap().to_string(),
        other => panic!("{other:?}"),
    };
    let k = key(0x33);
    begin(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    staged(c.tranche_stage(&a, one("s0", r#"CREATE (d: fixture-alpha/doc { title: "n", state: "draft" })"#, &[]), 1_001));
    let seq = c.tranche_anchor("T", 1).unwrap().unwrap().seq;

    // A QUEUED row under the tranche's seq — what an act killed mid-wait
    // leaves behind (`withdraw_queued` never ran). Inserted directly: the
    // spine's own table, a state the live paths can reach but no single
    // process can hold still.
    let sql = spine_sql(&dir);
    sql.execute(
        "INSERT INTO locks (pool, resource, mode, seq, granted) VALUES (?1, ?2, 'write', ?3, 0)",
        rusqlite::params![ALPHA, format!("body:{x}"), seq.0],
    )
    .unwrap();
    let t = touched(&c, "T", ALPHA);
    assert!(t.iter().any(|(r, m, g)| r == &format!("body:{x}") && m == "write" && !*g), "the queued row stands: {t:?}");

    // The second call claims `write` on x. Rule 1: the queued row is NOT
    // held — the claim is enrolled and GRANTED (nothing else holds x).
    staged(c.tranche_stage(
        &a,
        one("s1", "MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t", &[("id", json!(x)), ("t", json!("y"))]),
        1_002,
    ));
    let t = touched(&c, "T", ALPHA);
    assert!(has(&t, &format!("body:{x}"), "write"), "x is HELD after the call: {t:?}");
    assert!(t.iter().all(|(_, _, g)| *g), "no row under the tranche's seq is merely queued: {t:?}");

    // THE CEILING: a foreign holder (an ordinary seq) holds `write` on a
    // second committed node; the child claiming it waits, hits the ceiling,
    // is `busy` naming the holder, and its queued rows are withdrawn.
    let z = match seed(&c, r#"CREATE (d: fixture-alpha/doc { title: "z", state: "draft" }) RETURN d.id"#, &[]) {
        Outcome::Committed { returned, .. } => returned.0[0]["d.id"].as_str().unwrap().to_string(),
        other => panic!("{other:?}"),
    };
    let db = SpineDb::open(&dir.join(SPINE_FILE)).unwrap();
    let foreign = Seq(900_000);
    locks::acquire(&db, foreign, &claim(ALPHA, body(&z), LockMode::Write), &quick()).expect("the foreign holder");
    let blocked = one("s2", "MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t", &[("id", json!(z)), ("t", json!("w"))]);
    let started = Instant::now();
    match c.tranche_stage(&a, blocked.clone(), 1_003) {
        Err(SpineError::Busy { message }) => assert!(message.contains("900000"), "names the holder: {message}"),
        other => panic!("expected busy at the ceiling, got {other:?}"),
    }
    assert!(started.elapsed() >= Duration::from_millis(400), "it waited out the ceiling");
    let t = touched(&c, "T", ALPHA);
    assert!(!t.iter().any(|(r, _, _)| r == &format!("body:{z}")), "the refused child's rows are withdrawn: {t:?}");
    assert_eq!(staged_count(&c, "T"), (TrancheState::Staging, 2), "nothing staged");
    // The holder clears; the same child stages.
    locks::release_at_clear(&db, foreign).unwrap();
    let (ord, _, _) = staged(c.tranche_stage(&a, blocked, 1_004));
    assert_eq!(ord, 2);
    assert!(has(&touched(&c, "T", ALPHA), &format!("body:{z}"), "write"));
}

// ---------------------------------------------------------------------------
// g4 — GROWTH CONFINED: an ordinary (non-tranche) `seq` still refuses growth
// with `locks::acquire`'s message; a tranche's `seq` grows child by child.
// ---------------------------------------------------------------------------

/// RED (measured): `acquire_interior` admits growth for any `seq`
/// (the growth refusal removed) — the ordinary second call is granted.
#[test]
fn g4_an_ordinary_seq_refuses_claim_growth_by_name_while_the_tranches_seq_grows() {
    let dir = root("g4");
    let c = fresh(&dir);
    let db = SpineDb::open(&dir.join(SPINE_FILE)).unwrap();
    let cfg = SpineConfig::default();
    let ordinary = Seq(900_001);
    locks::acquire(&db, ordinary, &claim(ALPHA, body("nA"), LockMode::Read), &cfg).expect("complete at open");
    let mut grown = claim(ALPHA, body("nA"), LockMode::Read);
    grown.insert(Claim {
        pool: PoolName::new(ALPHA),
        resource: body("nB"),
        mode: LockMode::Read,
    });
    match locks::acquire(&db, ordinary, &grown, &cfg) {
        Err(SpineError::Usage { message }) => {
            assert!(message.contains("claims are complete at open"), "{message}");
            assert!(message.contains("fresh sequence number"), "{message}");
        }
        other => panic!("an ordinary seq must refuse growth, got {other:?}"),
    }

    // OPPOSED: the tranche's seq GROWS — two children, two claim sets, one
    // touched set holding both.
    let k = key(0x44);
    begin(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    let (_, m0, _) = staged(c.tranche_stage(&a, one("s0", r#"CREATE (d: fixture-alpha/doc { title: "a", state: "draft" })"#, &[]), 1_001));
    let n0 = touched(&c, "T", ALPHA).len();
    let (_, m1, _) = staged(c.tranche_stage(&a, one("s1", r#"CREATE (a: fixture-alpha/author { name: "ann" })"#, &[]), 1_002));
    let t = touched(&c, "T", ALPHA);
    assert!(t.len() > n0, "the touched set grew: {n0} → {}", t.len());
    assert!(has(&t, &format!("body:{}", id_of(&m0, "d")), "write"));
    assert!(has(&t, &format!("body:{}", id_of(&m1, "a")), "write"));
    assert!(has(&t, "tslice:doc", "link") && has(&t, "tslice:author", "link"));
}

// ---------------------------------------------------------------------------
// g5 — RULE 2 INSIDE THE GRANT: a child whose grant poll finds `landing`
// answers `tranche_past_rollback` — while the foreign holder still holds, so
// before any grant could have come — inserting no lock row, `staged`
// unchanged; a terminal state → `tranche_terminal`. Plus the `queued` row.
// ---------------------------------------------------------------------------

/// RED (measured): `claim_state_check` removed from `poll_once` (the
/// state checked only before the mutex, and at step 9): the child never
/// learns of the flip while it waits, waits out the ceiling behind the holder
/// and answers `busy` — the refusal no longer arrives inside the grant.
#[test]
fn g5_a_state_flip_during_the_claim_wait_is_found_inside_the_grant_poll_and_inserts_no_row() {
    let dir = root("g5");
    let mut cfg = SpineConfig::default();
    cfg.lock_wait_ceiling = Duration::from_secs(3);
    let c = fresh_with(&dir, &[alpha()], cfg.clone());
    let x = match seed(&c, r#"CREATE (d: fixture-alpha/doc { title: "x", state: "draft" }) RETURN d.id"#, &[]) {
        Outcome::Committed { returned, .. } => returned.0[0]["d.id"].as_str().unwrap().to_string(),
        other => panic!("{other:?}"),
    };
    let k = key(0x55);
    begin(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    let db = SpineDb::open(&dir.join(SPINE_FILE)).unwrap();
    let foreign = Seq(900_002);
    locks::acquire(&db, foreign, &claim(ALPHA, body(&x), LockMode::Write), &cfg).expect("the foreign holder");
    let set_x = || one("s0", "MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t", &[("id", json!(x)), ("t", json!("y"))]);

    for (flip, expect) in [(TrancheState::Landing, "past_rollback"), (TrancheState::Abandoned, "terminal")] {
        c.tranche_force_state("T", 1, TrancheState::Open).unwrap();
        let started = Instant::now();
        let outcome = std::thread::scope(|s| {
            let stager = s.spawn(|| c.tranche_stage(&a, set_x(), 1_001));
            // The child passed the door's checks (state `open`) and is now
            // polling behind the holder; flip the state under it.
            std::thread::sleep(Duration::from_millis(250));
            c.tranche_force_state("T", 1, flip).unwrap();
            stager.join().expect("the stager thread")
        });
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_secs(3), "answered INSIDE the wait, not at the ceiling: {elapsed:?}");
        match (expect, only_witness(outcome.expect_err("refused"))) {
            ("past_rollback", Witness::TranchePastRollback { tranche, attempt }) => assert_eq!((tranche.as_str(), attempt), ("T", 1)),
            ("terminal", Witness::TrancheTerminal { state, .. }) => assert_eq!(state, "abandoned"),
            (e, w) => panic!("expected {e}, got {w:?}"),
        }
        let t = touched(&c, "T", ALPHA);
        assert!(t.is_empty(), "no lock row under the tranche's seq: {t:?}");
        assert_eq!(c.tranche_store("T", None).unwrap().children.len(), 0, "staged unchanged");
    }
    locks::release_at_clear(&db, foreign).unwrap();

    // A child in `queued` is `busy tranche_busy / queued`: a second
    // tranche behind a live one.
    c.tranche_force_state("T", 1, TrancheState::Open).unwrap();
    let k2 = key(0x56);
    let b2 = begin(&c, "T2", &k2, 2_000);
    assert_eq!((b2.state, b2.position), (TrancheState::Queued, 2));
    match c.tranche_stage(&actor("T2", &k2), one("q0", r#"CREATE (d: fixture-alpha/doc { title: "q", state: "draft" })"#, &[]), 2_001) {
        Err(SpineError::TrancheBusy { why, fields }) => {
            assert_eq!(why, "queued");
            assert_eq!(fields["position"], json!(2));
            assert_eq!(fields["ahead"]["tranche"], json!("T"));
        }
        other => panic!("expected busy queued, got {other:?}"),
    }
    assert_eq!(c.tranche_store("T2", None).unwrap().children.len(), 0);
    // OPPOSED: the same child under the open tranche stages.
    staged(c.tranche_stage(&a, set_x(), 2_002));
}

// ---------------------------------------------------------------------------
// test 5 — the flag rule per child, at staging
// ---------------------------------------------------------------------------

/// RED (measured), two forcings: (1) `check_flag_grants` skipped in
/// `exec::stage` → the type-one child is staged; (2) the check moved AFTER
/// the durable stage (step 9) with its verdict dropped — "run it only at
/// landing" — → the child answers `staged` as it is submitted, and `staged`
/// moves.
#[test]
fn t19_5_a_type_one_child_setting_meta_blocking_is_refused_at_staging_and_a_scoped_type_two_is_staged() {
    let dir = root("t19-5");
    let c = fresh_with(&dir, &[board()], SpineConfig::default());
    let k = key(0x05);
    begin(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    let (_, m0, _) = staged(c.tranche_stage(&a, two("s0", &[], r#"CREATE (q: board/question { title: "q" })"#, &[]), 1_001));
    let q = m0["q"].split_once('/').unwrap().1.to_string();
    assert_eq!(staged_count(&c, "T"), (TrancheState::Staging, 1));
    let before = touched(&c, "T", BOARD);
    assert!(has(&before, &format!("body:{q}"), "write"), "child 0 holds its own node: {before:?}");
    let set = "MATCH (q: board/question { id: $id }) SET q.meta.blocking = $v";
    let params = [("id", json!(q)), ("v", json!(true))];

    // Type one → refused `flag_not_writable` AT STAGING; tranche still staging.
    match only_witness(c.tranche_stage(&a, one("s1", set, &params), 1_002).expect_err("refused")) {
        Witness::FlagNotWritable { field, initiator_kind, node, .. } => {
            assert_eq!((field.as_str(), initiator_kind.as_str(), node.as_str()), ("meta.blocking", "type_one", q.as_str()));
        }
        other => panic!("expected flag_not_writable, got {other:?}"),
    }
    assert_eq!(staged_count(&c, "T"), (TrancheState::Staging, 1), "the refused child was not staged");
    assert_eq!(touched(&c, "T", BOARD), before, "a refused child leaves the touched set as it found it");

    // Type two with an EMPTY scope → refused.
    match only_witness(c.tranche_stage(&a, two("s2", &[], set, &params), 1_003).expect_err("refused")) {
        Witness::FlagNotWritable { field, initiator_kind, scope, .. } => {
            assert_eq!((field.as_str(), initiator_kind.as_str()), ("meta.blocking", "type_two"));
            assert_eq!(scope, Some(vec![]));
        }
        other => panic!("expected flag_not_writable, got {other:?}"),
    }
    assert_eq!(staged_count(&c, "T"), (TrancheState::Staging, 1));

    // Type two with `scope: ["meta.blocking"]` → STAGED, ordinal 1.
    let (ord, _, changed) = staged(c.tranche_stage(&a, two("s3", &["meta.blocking"], set, &params), 1_004));
    assert_eq!((ord, changed), (1, true));
    assert_eq!(staged_count(&c, "T"), (TrancheState::Staging, 2));
    let r = under(&c, &a, "MATCH (q: board/question { id: $id }) RETURN q.meta", &[("id", json!(q))], 1_005);
    assert_eq!(rows_of(r.rows)[0]["q.meta"], json!({"blocking": true}));
    // The same validator on `validate`: refused the same way, nothing staged.
    let dry = child(RippleKind::TypeOne, "s4", &[], set, &params, Mode::Validate);
    assert!(matches!(only_witness(c.tranche_stage(&a, dry, 1_006).unwrap_err()), Witness::FlagNotWritable { .. }));
    assert_eq!(staged_count(&c, "T"), (TrancheState::Staging, 2));
}

// ---------------------------------------------------------------------------
// test 8 — `tranche_full` at `max + 1`
// ---------------------------------------------------------------------------

/// RED (measured): off by one in EITHER direction in the size check
/// (`staged >= max` → `staged > max`: a fourth child is staged; → `staged + 1
/// >= max`: the third is refused).
#[test]
fn t19_8_the_child_after_max_children_is_refused_tranche_full_with_a_configured_max_of_3() {
    let dir = root("t19-8");
    let mut cfg = SpineConfig::default();
    cfg.tranche.max_children = 3;
    let c = fresh_with(&dir, &[alpha()], cfg);
    let k = key(0x08);
    begin(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    let mk = |n: u32| one(&format!("s{n}"), "CREATE (d: fixture-alpha/doc { title: $t, state: \"draft\" })", &[("t", json!(format!("d{n}")))]);
    for n in 0..3 {
        let (ord, _, _) = staged(c.tranche_stage(&a, mk(n), 1_001 + i64::from(n)));
        assert_eq!(ord, n, "child {n} is staged (max = 3)");
    }
    assert_eq!(staged_count(&c, "T"), (TrancheState::Staging, 3));
    // The fourth — max + 1 — is refused by name.
    for n in 3..5 {
        match only_witness(c.tranche_stage(&a, mk(n), 1_010 + i64::from(n)).expect_err("tranche_full")) {
            Witness::TrancheFull { tranche, attempt, max } => assert_eq!((tranche.as_str(), attempt, max), ("T", 1, 3)),
            other => panic!("expected tranche_full, got {other:?}"),
        }
    }
    assert_eq!(staged_count(&c, "T"), (TrancheState::Staging, 3), "still staging, still 3");
    // A read under key still answers (the cap is on children, not reads).
    let r = under(&c, &a, "MATCH (d: fixture-alpha/doc) RETURN d.title", &[], 1_020);
    assert_eq!((r.children_seen, rows_of(r.rows).len()), (3, 3));
}

// ---------------------------------------------------------------------------
// g6 — STEP 8 COMPARES CLAIM SETS: a re-match under
// the grant that binds MORE with the SAME payload leaves every newly bound
// body held under the tranche's seq, so an outside write to it
// is `busy` naming the tranche. g7 — the step's RESTART path: `restart_bound`
// reached → `busy`, nothing staged, this call's rows released.
// ---------------------------------------------------------------------------

/// The child both guards stage: a state-filtered scan, one hop into `x`, a
/// SET on `x`. `e` is READ and never written, so a binding the re-match adds
/// leaves no mark on the payload — the shape the defect was measured with.
const HOP: &str = r#"MATCH (e: fixture-alpha/doc { state: "draft" }) -[supersedes]-> (d: fixture-alpha/doc { id: $id }) SET d.title = $t"#;

fn seed_id(c: &Cauldron, statement: &str, params: &[(&str, Value)], col: &str) -> String {
    match seed(c, statement, params) {
        Outcome::Committed { returned, .. } => returned.0[0][col].as_str().unwrap().to_string(),
        other => panic!("{other:?}"),
    }
}

fn doc(c: &Cauldron, title: &str, state: &str) -> String {
    seed_id(c, &format!(r#"CREATE (d: fixture-alpha/doc {{ title: "{title}", state: "{state}" }}) RETURN d.id"#), &[], "d.id")
}

fn supersedes(c: &Cauldron, from: &str, to: &str) {
    seed(
        c,
        r#"MATCH (a: fixture-alpha/doc { id: $a }) MATCH (d: fixture-alpha/doc { id: $d }) CREATE (a) -[supersedes]-> (d)"#,
        &[("a", json!(from)), ("d", json!(to))],
    );
}

/// A keyless commit whose outcome the test judges (`seed` panics on a refusal).
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

fn in_slice(node: &str) -> Resource {
    Resource::EdgeSlice {
        node: NodeId(node.to_string()),
        kind: "supersedes".into(),
        direction: spine::schema_def::Direction::In,
    }
}

/// Block until the child has QUEUED `resource` under `seq` — proof it has
/// taken its snapshot (step 5) and is in step 7's wait — never a sleep.
fn wait_queued(dir: &Path, seq: Seq, resource: &str) {
    let sql = spine_sql(dir);
    sql.busy_timeout(Duration::from_secs(2)).unwrap();
    let started = Instant::now();
    loop {
        let n: i64 = sql
            .query_row(
                "SELECT COUNT(*) FROM locks WHERE seq = ?1 AND resource = ?2 AND granted = 0",
                rusqlite::params![seq.0, resource],
                |r| r.get(0),
            )
            .unwrap();
        if n > 0 {
            return;
        }
        assert!(started.elapsed() < Duration::from_secs(5), "the child never queued {resource} under seq {}", seq.0);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A foreign ordinary transaction's LANDING of `from -[supersedes]-> to` —
/// the part applied to the real pool under the `link` its holder was granted
/// before the child's snapshot, exactly as the ordinary door lands it. Done
/// directly because no test can hold two real commits' order still.
fn land_edge_outside(dir: &Path, from: &str, to: &str) {
    let pname = PoolName::new(ALPHA);
    let mut pool = Pool::open(&Pool::path_in(dir, &pname)).expect("open the real pool");
    let part = spine::delta::CompiledPart {
        pool: pname.clone(),
        schema_version: alpha().version,
        deltas: vec![spine::delta::Delta::AddEdge {
            src: NodeId(from.to_string()),
            kind: "supersedes".into(),
            dst: spine::pool::QualifiedNodeId {
                pool: pname,
                id: NodeId(to.to_string()),
            },
            note: None,
            fields: BTreeMap::new(),
        }],
        premises: vec![],
        far_versions: BTreeMap::new(),
    };
    pool.apply_part(&spine::envelope::TxHash([7; 32]), 0, &part).expect("the foreign landing");
}

/// A foreign `x -[cites]-> <ghost>` landed by hand on the real pool (the
/// edge toward the projected namespace, grammar `alpha_citing`), for 40(c)'s
/// red tail.
fn land_edge_outside_cites(dir: &Path, from: &str, ghost: &str) {
    let pname = PoolName::new(ALPHA);
    let mut pool = Pool::open(&Pool::path_in(dir, &pname)).expect("open the real pool");
    let part = spine::delta::CompiledPart {
        pool: pname.clone(),
        schema_version: alpha().version,
        deltas: vec![spine::delta::Delta::AddEdge {
            src: NodeId(from.to_string()),
            kind: "cites".into(),
            dst: spine::pool::QualifiedNodeId {
                pool: PoolName::new(project::PROJECTED_POOL),
                id: NodeId(ghost.to_string()),
            },
            note: None,
            fields: BTreeMap::new(),
        }],
        premises: vec![],
        far_versions: BTreeMap::new(),
    };
    pool.apply_part(&spine::envelope::TxHash([8; 32]), 0, &part).expect("the foreign cites landing");
}

/// The scenario both guards run: `e1 -> x` committed; a foreign holder was
/// granted `link` on x's in-slice BEFORE the child; the child snapshots
/// (binding e = {e1}), queues behind the holder; the holder lands `e2 -> x`
/// and clears; the child's grant arrives and its re-match binds e = {e1, e2}
/// with the SAME payload (one SET on x). Returns the child's outcome and the
/// ids (x, e1, e2).
fn moved_under_the_grant(dir: &Path, c: &Cauldron, cfg: &SpineConfig, k: &str) -> (spine::error::Result<ChildOutcome>, String, String, String) {
    let x = doc(c, "x", "published");
    let e1 = doc(c, "e1", "draft");
    let e2 = doc(c, "e2", "draft");
    supersedes(c, &e1, &x);
    begin(c, "T", k, 1_000);
    let a = actor("T", k);
    let seq = c.tranche_anchor("T", 1).unwrap().unwrap().seq;
    let db = SpineDb::open(&dir.join(SPINE_FILE)).unwrap();
    let foreign = Seq(900_006);
    locks::acquire(&db, foreign, &claim(ALPHA, in_slice(&x), LockMode::Link), cfg).expect("the foreign holder");
    let outcome = std::thread::scope(|s| {
        let stager = s.spawn(|| c.tranche_stage(&a, one("s0", HOP, &[("id", json!(x)), ("t", json!("t"))]), 1_001));
        wait_queued(dir, seq, &format!("eslice:{x}:supersedes:in"));
        land_edge_outside(dir, &e2, &x);
        locks::release_at_clear(&db, foreign).unwrap();
        stager.join().expect("the stager thread")
    });
    (outcome, x, e1, e2)
}

fn bound_e(dir: &Path) -> Vec<String> {
    let bound: String = spine_sql(dir)
        .query_row("SELECT bound FROM tranche_children WHERE ordinal = 0", [], |r| r.get(0))
        .unwrap();
    let bound: Value = serde_json::from_str(&bound).unwrap();
    let mut es: Vec<String> = bound["e"].as_array().unwrap().iter().map(|v| v["id"].as_str().unwrap().to_string()).collect();
    es.sort();
    es
}

/// RED (measured): step 8's comparison reverted to the old
/// `re_payload == payload` — the child stages with `bound.e = [e1, e2]` and NO
/// `body:e2 read` under the tranche's seq; the outside SET on e2 commits.
/// OPPOSED: an outside SET on an UNTOUCHED doc commits; the child's own SET
/// is in the view.
#[test]
fn g6_a_re_match_under_the_grant_that_binds_more_holds_every_bound_body_and_an_outside_write_to_one_is_busy() {
    let dir = root("g6");
    let mut cfg = SpineConfig::default();
    cfg.lock_wait_ceiling = Duration::from_millis(1_500);
    let c = fresh_with(&dir, &[alpha()], cfg.clone());
    let far = doc(&c, "far", "published");
    let (outcome, x, e1, e2) = moved_under_the_grant(&dir, &c, &cfg, &key(0x66));
    let (ord, _, changed) = staged(outcome);
    assert_eq!((ord, changed), (0, true));
    let seq = c.tranche_anchor("T", 1).unwrap().unwrap().seq;
    // The recorded binding names BOTH — the match under the grant.
    assert_eq!(bound_e(&dir), {
        let mut v = vec![e1.clone(), e2.clone()];
        v.sort();
        v
    });
    // THE GUARD: every body the final match bound is held
    // `read`, and the slice it traversed out of e2 too; nothing merely queued.
    let t = touched(&c, "T", ALPHA);
    assert!(has(&t, &format!("body:{e1}"), "read"), "{t:?}");
    assert!(has(&t, &format!("body:{e2}"), "read"), "the re-matched binding's body {e2} is NOT held: {t:?}");
    assert!(has(&t, &format!("eslice:{e2}:supersedes:out"), "read"), "{t:?}");
    assert!(has(&t, &format!("body:{x}"), "write"), "{t:?}");
    assert!(t.iter().all(|(_, _, g)| *g), "nothing queued: {t:?}");
    // The consequence measured the other way: an outside write
    // to e2 (moving it OUT of the child's match) is `busy` naming the
    // tranche's seq as the holder — the tranche relies on e2's body.
    match keyless(&c, "g6-out", r#"MATCH (d: fixture-alpha/doc { id: $a }) SET d.state = "published""#, &[("a", json!(e2))]) {
        Err(SpineError::Busy { message }) => assert!(message.contains(&seq.0.to_string()), "names the tranche's seq {}: {message}", seq.0),
        other => panic!("an outside write to a body the tranche bound must be busy, got {other:?}"),
    }
    // OPPOSED: the instrument can commit — an outside write to a doc the
    // child never bound lands.
    assert!(matches!(
        keyless(&c, "g6-far", r#"MATCH (d: fixture-alpha/doc { id: $a }) SET d.title = "moved""#, &[("a", json!(far))]),
        Ok(Outcome::Committed { .. })
    ));
    // The child's SET is in the view, under key; not in the committed pool.
    let r = under(&c, &a_of("T", 0x66), "MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.title", &[("id", json!(x))], 1_002);
    assert_eq!(rows_of(r.rows)[0]["d.title"], json!("t"));
    assert_eq!(committed(&c, "MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.title", &[("id", json!(x))])[0]["d.title"], json!("x"));
}

fn a_of(t: &str, n: u8) -> TrancheActor {
    actor(t, &key(n))
}

/// RED (measured): the same reverted comparison — under
/// `restart_bound = 0` the child STAGES (no restart is ever counted) instead
/// of answering `busy`. OPPOSED: under `restart_bound = 1` the same scenario
/// stages, holding e2.
#[test]
fn g7_the_step_8_restart_path_is_busy_past_restart_bound_0_with_this_calls_rows_released_and_stages_under_bound_1() {
    for bound in [0u32, 1] {
        let dir = root(&format!("g7-{bound}"));
        let mut cfg = SpineConfig::default();
        cfg.lock_wait_ceiling = Duration::from_millis(1_500);
        cfg.restart_bound = bound;
        cfg.escalation_threshold = bound;
        let c = fresh_with(&dir, &[alpha()], cfg.clone());
        let (outcome, x, _e1, e2) = moved_under_the_grant(&dir, &c, &cfg, &key(0x67));
        let t = touched(&c, "T", ALPHA);
        if bound == 0 {
            match outcome {
                Err(SpineError::Busy { message }) => {
                    assert!(message.contains("gave up after 1 restarts"), "{message}");
                    assert!(message.contains("restart_bound 0"), "{message}");
                    assert!(message.contains("nothing staged"), "{message}");
                }
                other => panic!("expected busy at the restart bound, got {other:?}"),
            }
            assert_eq!(staged_count(&c, "T"), (TrancheState::Open, 0), "nothing staged, the tranche still open");
            assert!(t.is_empty(), "the rows this call granted are released: {t:?}");
            // The pool is as the foreign landing left it; the child wrote nothing.
            assert_eq!(committed(&c, "MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.title", &[("id", json!(x))])[0]["d.title"], json!("x"));
        } else {
            let (ord, _, _) = staged(outcome);
            assert_eq!(ord, 0);
            assert!(has(&t, &format!("body:{e2}"), "read"), "under bound 1 the restart enrols e2: {t:?}");
            assert_eq!(staged_count(&c, "T"), (TrancheState::Staging, 1));
        }
    }
}

// ---------------------------------------------------------------------------
// THE STATE SLICE A FILTERED SCAN READ: a
// state-filtered head holds `read` on `sslice:<type>:<state>` beside the
// type slice and the bound bodies, and every writer takes `link` on the slice
// a node leaves and the slice it enters, so a foreign SET that would move an
// UNBOUND candidate into (or a candidate out of) the scanned set conflicts.
// Tests 37–39, every one in `touched` mode unless it says otherwise.
// ---------------------------------------------------------------------------

/// A keyless `SET <id>.state = <state>` — the foreign state change.
fn set_state(c: &Cauldron, nonce: &str, id: &str, state: &str) -> spine::error::Result<Outcome> {
    keyless(
        c,
        nonce,
        r#"MATCH (d: fixture-alpha/doc { id: $a }) SET d.state = $s"#,
        &[("a", json!(id)), ("s", json!(state))],
    )
}

/// The spine's `busy` naming the tranche's anchor `seq` as the holder.
fn busy_naming(r: spine::error::Result<Outcome>, seq: Seq, what: &str) {
    match r {
        Err(SpineError::Busy { message }) => assert!(
            message.contains(&seq.0.to_string()),
            "{what}: busy, but not naming the tranche's seq {}: {message}",
            seq.0
        ),
        other => panic!("{what}: expected busy naming tranche seq {}, got {other:?}", seq.0),
    }
}

fn anchor_seq(c: &Cauldron, t: &str) -> Seq {
    c.tranche_anchor(t, 1).unwrap().unwrap().seq
}

/// THE LANDING: the holder's `commit` on one thread, the scheduler's
/// tick on this one until the join returns — the landing is the tick's, so
/// no test here lands a tranche without one.
fn land(c: &Cauldron, a: &TrancheActor, now: i64) -> spine::error::Result<spine::tranche::TrancheCommitted> {
    std::thread::scope(|s| {
        let h = s.spawn(|| c.tranche_commit(a, now));
        let started = Instant::now();
        while !h.is_finished() {
            c.tranche_tick(&QUIET, now).expect("tick");
            assert!(started.elapsed() < Duration::from_secs(20), "the landing never finished");
            std::thread::sleep(Duration::from_millis(10));
        }
        h.join().expect("the commit thread")
    })
}

/// The P2 fixture: `x` published; `e1` draft and `e3` published,
/// both `supersedes` `x`. Returns `(x, e1, e3)`.
fn p2_fixture(c: &Cauldron) -> (String, String, String) {
    let x = doc(c, "x", "published");
    let e1 = doc(c, "e1", "draft");
    let e3 = doc(c, "e3", "published");
    supersedes(c, &e1, &x);
    supersedes(c, &e3, &x);
    (x, e1, e3)
}

const DRAFT_IDS: &str = r#"MATCH (e: fixture-alpha/doc { state: "draft" }) -[supersedes]-> (d: fixture-alpha/doc { id: $id }) RETURN e.id"#;

/// Test 37(a), the P2 fixture as a guard: the child of `HOP` binds
/// `{e1}`; the touched set holds `sslice:doc:draft read` beside `tslice:doc
/// read`; a keyless `SET e3.state = "draft"` — moving an UNBOUND candidate
/// INTO the state filter — is `busy` naming the tranche; the child's match on
/// committed state still binds `{e1}`; THE TRANCHE LANDS and the
/// writer's retry lands after it.
// RED (measured): (1) the slice `read` removed from
// `tranche_read_claims` → the foreign SET returns `Committed` and the
// committed match binds `{e1, e3}` — the measured outcome;
// (2) the `read` kept and the `link` dropped from `claims_for`'s state-SET
// arm → the same, and the landing after it is refused `tranche_landing_refused
// {why: revalidation}` (the child's match now binds {e1, e3}). OPPOSED: after
// the landing the same SET commits.
#[test]
fn t19_37a_a_foreign_set_moving_an_unbound_candidate_into_the_state_filter_is_busy_and_lands_after_the_tranche() {
    let dir = root("t37a");
    let c = fresh_with(&dir, &[alpha()], quick());
    let (x, e1, e3) = p2_fixture(&c);
    let k = key(0x72);
    begin(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    staged(c.tranche_stage(&a, one("s0", HOP, &[("id", json!(x)), ("t", json!("t"))]), 1_001));
    assert_eq!(bound_e(&dir), vec![e1.clone()], "the child bound e1 alone");
    let seq = anchor_seq(&c, "T");
    let t = touched(&c, "T", ALPHA);
    assert!(has(&t, "tslice:doc", "read") && has(&t, &format!("body:{e1}"), "read"), "{t:?}");
    // THE GUARD: the state slice the filtered scan read.
    assert!(has(&t, "sslice:doc:draft", "read"), "the scanned state slice is not held: {t:?}");
    assert!(!t.iter().any(|(r, _, _)| r == &format!("body:{e3}")), "e3's body is held by nothing (not a candidate): {t:?}");
    // A foreign SET moves e3 INTO the filter: `link` on sslice:doc:draft
    // (entering) conflicts with the tranche's `read` → busy, nothing committed.
    busy_naming(set_state(&c, "t37a-set", &e3, "draft"), seq, "the foreign SET into the scanned state");
    let now = committed(&c, DRAFT_IDS, &[("id", json!(x))]);
    assert_eq!(now.len(), 1, "the child's MATCH on committed state still binds e1 alone: {now:?}");
    assert_eq!(now[0]["e.id"], json!(e1));
    // THE TRANCHE LANDS: the child's SET is in the committed pool,
    // its claims released; the writer's retry lands, and only then does the
    // committed match bind {e1, e3}.
    let landed = land(&c, &a, 1_002).expect("the tranche lands");
    assert_eq!((landed.state, landed.children.len()), (TrancheState::Landed, 1));
    assert_eq!(committed(&c, "MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.title", &[("id", json!(x))])[0]["d.title"], json!("t"));
    assert!(touched(&c, "T", ALPHA).is_empty(), "every lock row released at the clear");
    assert!(matches!(set_state(&c, "t37a-retry", &e3, "draft"), Ok(Outcome::Committed { .. })));
    assert_eq!(committed(&c, DRAFT_IDS, &[("id", json!(x))]).len(), 2);
}

/// Test 37(b): the same through a READ UNDER KEY — `MATCH (e: doc
/// {state: "draft"}) RETURN e.id` under the key holds `sslice:doc:draft
/// read`, and the foreign SET is busy.
// RED (measured): as 37(a)'s (1). OPPOSED: a read under key WITHOUT a
// state filter holds no state slice, and the foreign SET commits.
#[test]
fn t19_37b_a_read_under_key_of_a_filtered_scan_holds_the_state_slice_and_the_foreign_set_is_busy() {
    let dir = root("t37b");
    let c = fresh_with(&dir, &[alpha()], quick());
    let (x, e1, e3) = p2_fixture(&c);
    let k = key(0x73);
    begin(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    let r = under(&c, &a, r#"MATCH (e: fixture-alpha/doc { state: "draft" }) RETURN e.id"#, &[], 1_001);
    assert_eq!(rows_of(r.rows), vec![BTreeMap::from([("e.id".to_string(), json!(e1))])]);
    let seq = anchor_seq(&c, "T");
    let t = touched(&c, "T", ALPHA);
    assert!(has(&t, "sslice:doc:draft", "read"), "the read under key holds the scanned slice: {t:?}");
    assert!(has(&t, "tslice:doc", "read") && has(&t, &format!("body:{e1}"), "read"), "{t:?}");
    busy_naming(set_state(&c, "t37b-set", &e3, "draft"), seq, "the foreign SET into the read's filter");
    // OPPOSED: an UNFILTERED scan under another key holds no state slice,
    // and a state change outside its bodies is free.
    c.tranche_rollback(&a, 1_002).expect("rollback");
    let k2 = key(0x74);
    begin(&c, "U", &k2, 1_003);
    let far = doc(&c, "far", "published");
    under(&c, &actor("U", &k2), r#"MATCH (e: fixture-alpha/doc { id: $e }) RETURN e.id"#, &[("e", json!(e1))], 1_004);
    let t = touched(&c, "U", ALPHA);
    assert!(!t.iter().any(|(r, _, _)| r.starts_with("sslice:")), "no filter, no state slice: {t:?}");
    assert!(matches!(set_state(&c, "t37b-far", &far, "draft"), Ok(Outcome::Committed { .. })));
    let _ = x;
}

/// Flip a live tranche's recorded lock mode. `stage` has NO lock-mode branch,
/// which is exactly the mechanism "in both modes" rests on. This
/// writes the column directly so that mechanism is observed, not assumed.
/// Test 37(c), BOTH MODES: under `whole` the touched set holds
/// `sslice:doc:draft read` exactly as under `touched`, and both the entering
/// `link` (e3 → draft) and the leaving `link` alone (e2, a hop-dropped draft
/// candidate, → published) conflict with it. This is the whole-mode half of
/// the rule "one guard per lock mode for the read on a filtered head and
/// the link on entering or leaving the slice"; 37(a)/38(b) are the touched
/// half.
// RED (measured): made by INSERTING a line — there is no
// mode-conditional line in `stage`, and that absence is the mechanism:
// `if lock_mode == whole { drop the StateSlice claims }` → under whole the
// slice is unheld and both foreign SETs commit. OPPOSED: the same scenario
// under `touched` (37(a)) and the store reads `lock_mode: whole` back.
#[test]
fn t19_37c_under_whole_the_filtered_scan_holds_the_state_slice_and_entering_and_leaving_links_are_busy() {
    let dir = root("t37c");
    let c = fresh_with(&dir, &[alpha()], quick());
    let (x, e1, e3) = p2_fixture(&c);
    let e2 = doc(&c, "e2", "draft"); // draft, no edge to x: a candidate the hop drops
    let k = key(0x75);
    // THE REAL `whole`: begun through `begin`, not flipped by SQL —
    // the queue is empty and nothing is granted, so it opens inline with
    // the gate up.
    let b = c
        .tranche_begin(
            &TrancheBegin {
                tranche: "T".into(),
                key: k.clone(),
                holder_service: SERVICE.into(),
                instrument: instrument(),
                lock_mode: TrancheLockMode::Whole,
                regions: vec![ALPHA.into()],
            },
            &QUIET,
            1_000,
        )
        .expect("begin whole");
    assert_eq!((b.state, b.lock_mode), (TrancheState::Open, TrancheLockMode::Whole));
    assert_eq!(c.tranche_store("T", None).unwrap().lock_mode, TrancheLockMode::Whole);
    let a = actor("T", &k);
    staged(c.tranche_stage(&a, one("s0", HOP, &[("id", json!(x)), ("t", json!("t"))]), 1_001));
    assert_eq!(bound_e(&dir), vec![e1.clone()]);
    let seq = anchor_seq(&c, "T");
    let t = touched(&c, "T", ALPHA);
    assert!(has(&t, "sslice:doc:draft", "read"), "under whole the scanned slice is held too: {t:?}");
    assert!(!t.iter().any(|(r, _, _)| r == &format!("body:{e2}")), "the hop dropped e2; its body is unheld: {t:?}");
    // Under the GATE a foreign SET is `locked` before any claim is spoken
    // (checkpoint 1) — the slice row is redundant for exclusion here and is
    // recorded anyway, for the switch below.
    match set_state(&c, "t37c-gated", &e3, "draft") {
        Err(SpineError::TrancheBusy { why, .. }) => assert_eq!(why, "locked"),
        other => panic!("under the gate the foreign SET is refused locked, got {other:?}"),
    }
    // THE REAL SWITCH: `lock` to `touched` — instant, the lock
    // table untouched: the slice row is STILL held, with its key and mode.
    let before = touched(&c, "T", ALPHA);
    let switched = c.tranche_lock(&a, TrancheLockMode::Touched, 1_002).expect("lock to touched");
    assert_eq!((switched.lock_mode, switched.switched), (TrancheLockMode::Touched, true));
    let after = touched(&c, "T", ALPHA);
    assert_eq!(before, after, "the switch releases, re-derives and re-enrols nothing");
    assert!(has(&after, "sslice:doc:draft", "read"), "{after:?}");
    // Now the gate is down and the SLICE ROW is what refuses: the entering
    // link (e3 → draft) and the leaving link alone (e2 → published) are
    // each busy NAMING THE TRANCHE.
    busy_naming(set_state(&c, "t37c-in", &e3, "draft"), seq, "after the switch: the entering link");
    busy_naming(set_state(&c, "t37c-out", &e2, "published"), seq, "after the switch: the leaving link alone");
    assert_eq!(committed(&c, DRAFT_IDS, &[("id", json!(x))]).len(), 1);
    // OPPOSED: a body write on e2 that crosses no slice commits in `touched`.
    assert!(matches!(
        keyless(&c, "t37c-title", r#"MATCH (d: fixture-alpha/doc { id: $a }) SET d.title = "renamed""#, &[("a", json!(e2))]),
        Ok(Outcome::Committed { .. })
    ));
}

/// Test 38(a): OUT of the match set — a BOUND member leaving. A foreign
/// `SET e1.state = "published"` is busy; e1 is held twice, by `body:e1 read`
/// (the bound-body row) and by the `link` on `sslice:doc:draft`, the slice
/// it leaves.
// RED (measured): the bound-body read dropped from `claims_for` AND
// the leaving-state link dropped → the SET commits and the committed match
// binds nothing. Each alone (measured in turn) → still busy.
#[test]
fn t19_38a_a_foreign_set_moving_a_bound_member_out_of_the_filter_is_busy() {
    let dir = root("t38a");
    let c = fresh_with(&dir, &[alpha()], quick());
    let (x, e1, _e3) = p2_fixture(&c);
    let k = key(0x76);
    begin(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    staged(c.tranche_stage(&a, one("s0", HOP, &[("id", json!(x)), ("t", json!("t"))]), 1_001));
    let seq = anchor_seq(&c, "T");
    let t = touched(&c, "T", ALPHA);
    // (`body:e1 read` itself is 37(a)'s and g6's assertion; not repeated here
    // so the "bound-body read seamed off alone" forcing reads as the busy
    // it is.)
    assert!(has(&t, "sslice:doc:draft", "read"), "{t:?}");
    busy_naming(set_state(&c, "t38a-out", &e1, "published"), seq, "the bound member leaving");
    let now = committed(&c, DRAFT_IDS, &[("id", json!(x))]);
    assert_eq!(now.len(), 1, "e1 is still in the committed match: {now:?}");
}

/// Test 38(b): the LEAVING-STATE `link` alone — the over-restriction,
/// observed. `e2` is draft with NO edge to `x`: the child binds `{e1}`; the
/// touched set holds `sslice:doc:draft read`, `tslice:doc read`, `body:e1
/// read` and `eslice:e2:supersedes:out read` (the trace, before the hop
/// narrowed) and NOT `body:e2`. A keyless `SET e2.state = "published"` claims
/// `body:e2 write` (unheld), `link sslice:doc:draft` (CONFLICTS) and `link
/// sslice:doc:published` (unheld) → busy, by the leaving link and nothing
/// else — the one observation that proves that line exists.
// RED (measured): the leaving-state link dropped alone → the SET
// commits (the match set is unchanged, so nothing else notices). OPPOSED: a
// SET of e2's TITLE — a body write that crosses no slice — commits.
#[test]
fn t19_38b_the_leaving_state_link_alone_makes_a_hop_dropped_candidates_state_change_busy() {
    let dir = root("t38b");
    let c = fresh_with(&dir, &[alpha()], quick());
    let x = doc(&c, "x", "published");
    let e1 = doc(&c, "e1", "draft");
    let e2 = doc(&c, "e2", "draft");
    supersedes(&c, &e1, &x);
    let k = key(0x77);
    begin(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    staged(c.tranche_stage(&a, one("s0", HOP, &[("id", json!(x)), ("t", json!("t"))]), 1_001));
    assert_eq!(bound_e(&dir), vec![e1.clone()]);
    let seq = anchor_seq(&c, "T");
    let t = touched(&c, "T", ALPHA);
    assert!(has(&t, "sslice:doc:draft", "read"), "{t:?}");
    assert!(has(&t, "tslice:doc", "read") && has(&t, &format!("body:{e1}"), "read"), "{t:?}");
    assert!(has(&t, &format!("eslice:{e2}:supersedes:out"), "read"), "the trace holds e2's out-slice: {t:?}");
    assert!(!t.iter().any(|(r, _, _)| r == &format!("body:{e2}")), "e2's body is NOT held: {t:?}");
    busy_naming(set_state(&c, "t38b-out", &e2, "published"), seq, "the hop-dropped candidate leaving the scanned state");
    // OPPOSED: a body write on e2 that crosses no slice is free.
    assert!(matches!(
        keyless(&c, "t38b-title", r#"MATCH (d: fixture-alpha/doc { id: $a }) SET d.title = "renamed""#, &[("a", json!(e2))]),
        Ok(Outcome::Committed { .. })
    ));
}

/// Test 39: a CREATE into the slice. A head-only filtered scan `MATCH (e:
/// doc {state: "draft"}) SET e.title = "reviewed"` binds `{e1}`; a foreign
/// `CREATE (n: doc {state: "draft"})` is busy — held twice, by `tslice:doc
/// read` against the create's `tslice` link and by
/// `sslice:doc:draft read` against the create's state-slice link. The SHAPE
/// of the create's claim is observed through the lock table of a second,
/// unopposed tranche: a create into `published` carries `sslice:doc:published
/// link` and NOT `sslice:doc:draft link`; a create with no state carries no
/// `sslice` claim at all.
// RED (measured): (1) the type-slice read seamed off `claim_footprint`
// AND the create's state-slice link dropped → the create commits and the
// committed match binds `{e1, n}`; each alone → still busy. (2) the create's
// slice derived from the type's FIRST declared state instead of the delta's
// `state` → the published create names `draft`, and the no-state create names
// a slice.
#[test]
fn t19_39_a_foreign_create_into_the_scanned_state_is_busy_and_the_creates_slice_link_has_the_deltas_shape() {
    let dir = root("t39");
    let c = fresh_with(&dir, &[alpha()], quick());
    let e1 = doc(&c, "e1", "draft");
    let _p = doc(&c, "p", "published");
    let k = key(0x78);
    begin(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    staged(c.tranche_stage(
        &a,
        one("s0", r#"MATCH (e: fixture-alpha/doc { state: "draft" }) SET e.title = "reviewed""#, &[]),
        1_001,
    ));
    assert_eq!(bound_e(&dir), vec![e1.clone()]);
    let seq = anchor_seq(&c, "T");
    let t = touched(&c, "T", ALPHA);
    // (`tslice:doc read` is 37(a)'s assertion; not repeated here so the
    // "type-slice read seamed off alone" forcing reads as the busy it is.)
    assert!(has(&t, "sslice:doc:draft", "read"), "{t:?}");
    busy_naming(
        keyless(&c, "t39-create", r#"CREATE (n: fixture-alpha/doc { title: "n", state: "draft" })"#, &[]),
        seq,
        "a foreign create into the scanned state",
    );
    let drafts = committed(&c, r#"MATCH (e: fixture-alpha/doc { state: "draft" }) RETURN e.id"#, &[]);
    assert_eq!(drafts.len(), 1, "the committed match still binds e1 alone: {drafts:?}");
    c.tranche_rollback(&a, 1_002).expect("rollback");

    // THE SHAPE of a create's slice link, read off a second tranche's rows.
    let k2 = key(0x79);
    begin(&c, "U", &k2, 1_003);
    staged(c.tranche_stage(
        &actor("U", &k2),
        one("u0", r#"CREATE (n: fixture-alpha/doc { title: "n", state: "published" })"#, &[]),
        1_004,
    ));
    let u = touched(&c, "U", ALPHA);
    assert!(has(&u, "sslice:doc:published", "link"), "the create links the slice its state enters: {u:?}");
    assert!(!u.iter().any(|(r, _, _)| r == "sslice:doc:draft"), "and not another: {u:?}");
    assert!(has(&u, "tslice:doc", "link"), "{u:?}");
    c.tranche_rollback(&actor("U", &k2), 1_005).expect("rollback");
    let k3 = key(0x7a);
    begin(&c, "V", &k3, 1_006);
    staged(c.tranche_stage(
        &actor("V", &k3),
        one("v0", r#"CREATE (n: fixture-alpha/doc { title: "n" })"#, &[]),
        1_007,
    ));
    let v = touched(&c, "V", ALPHA);
    assert!(!v.iter().any(|(r, _, _)| r.starts_with("sslice:")), "a create with no state enters no slice: {v:?}");
    assert!(has(&v, "tslice:doc", "link"), "{v:?}");
}

// ---------------------------------------------------------------------------
// THE PROJECTED NAMESPACE UNDER A TRANCHE: a ghost's `state` and `parts` are
// the spine's own row flips, which no lock holds, so a tranche may not rely on
// them; a projected head under a tranche must name a row that exists. Test
// 40. Refusal by name — if this is ever carried as a stated residual instead,
// this test comes out with the refusal.
// ---------------------------------------------------------------------------

/// Alpha plus `cites`: doc → the projected `spine/txn` (a reach toward kind
/// `spine`, the workspace fixture's `landed_by` shape).
fn alpha_citing() -> PoolGrammar {
    let mut g = alpha();
    g.edge_kinds.insert(
        "cites".into(),
        EdgeKindDef::reaching("doc", [TypeRef::reach(project::PROJECTED_KIND, project::TXN_NODE_TYPE)]),
    );
    g
}

/// A foreign ordinary row `H`, appended and left UNDECIDED (its ghost reads
/// `pending`). Returns the ghost's identifier and the row's `seq`.
fn pending_row(dir: &Path, tag: &str) -> (String, Seq) {
    let env = Envelope {
        statement: format!("-- held undecided: {tag}"),
        bindings: BTreeMap::new(),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::CoreMechanical {
                operation: "test-pending".into(),
            },
            instrument: instrument(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(tag.to_string()),
    };
    let hash = env.hash_identity().unwrap();
    let mut db = SpineDb::open(&dir.join(SPINE_FILE)).unwrap();
    let seq = db
        .append_pending(&env, &hash, &spine::delta::CompiledPayload { parts: vec![], minted: BTreeMap::new() })
        .unwrap();
    (project::ghost_id(&hash).0, seq)
}

fn unimplemented(e: SpineError, form: &str, what: &str) {
    match only_witness(e) {
        Witness::UnimplementedForm { form: f } => assert_eq!(f, form, "{what}"),
        other => panic!("{what}: expected unimplemented_form {form}, got {other:?}"),
    }
}

/// Test 40(a): the forms a tranche may not rely on, refused by the frozen
/// name `match:projected-pattern` as a child and as a read under key, the
/// tranche still `open`; `RETURN g.id` on an existing row answers.
// RED (measured): `check_projected_under_tranche` removed from
// `stage` → the WHERE child STAGES (the unjust landing refusal is the
// lander's to measure); removed from `read_under` → the `RETURN g.state` read
// answers a row. OPPOSED: `RETURN g.id` answers under key; the ordinary door's
// read of `g.state` is unchanged.
#[test]
fn t19_40a_a_tranche_may_not_read_a_ghosts_moving_fields_refused_by_name_as_child_and_as_read_under_key() {
    let dir = root("t40a");
    let c = fresh_with(&dir, &[alpha_citing()], quick());
    let x = doc(&c, "x", "published");
    let (h, _) = pending_row(&dir, "H");
    let k = key(0x7b);
    begin(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    let forms: [(&str, &str); 3] = [
        ("WHERE over a projected binding",
         r#"MATCH (g: spine/txn { id: $h }) MATCH (d: fixture-alpha/doc { id: $x }) WHERE g.state = "pending" SET d.title = "seen""#),
        ("a state entry on a projected head",
         r#"MATCH (g: spine/txn { id: $h, state: "pending" }) MATCH (d: fixture-alpha/doc { id: $x }) SET d.title = "seen""#),
        ("EXPECT over a projected binding",
         r#"MATCH (g: spine/txn { id: $h }) MATCH (d: fixture-alpha/doc { id: $x }) EXPECT COUNT(g) = 1 SET d.title = "seen""#),
    ];
    for (i, (what, text)) in forms.iter().enumerate() {
        let r = c.tranche_stage(&a, one(&format!("s{i}"), text, &[("h", json!(h)), ("x", json!(x))]), 1_001);
        match r {
            Err(e) => unimplemented(e, "match:projected-pattern", what),
            other => panic!("{what}: expected a refusal, got {other:?}"),
        }
        assert_eq!(staged_count(&c, "T"), (TrancheState::Open, 0), "{what}: the tranche is still open and empty");
    }
    // A read under key of a ghost's field other than `id` → the same.
    match c.tranche_read(&a, &req(r#"MATCH (g: spine/txn { id: $h }) RETURN g.state"#, &[("h", json!(h))]), 1_002) {
        Err(e) => unimplemented(e, "match:projected-pattern", "RETURN g.state under key"),
        other => panic!("RETURN g.state under key: expected a refusal, got {other:?}"),
    }
    match c.tranche_read(&a, &req(r#"MATCH (g: spine/txn { id: $h }) RETURN REV(g)"#, &[("h", json!(h))]), 1_002) {
        Err(e) => unimplemented(e, "match:projected-pattern", "RETURN REV(g) under key"),
        other => panic!("RETURN REV(g) under key: expected a refusal, got {other:?}"),
    }
    // ...while `RETURN g.id` on an existing row answers (what cannot move).
    let r = under(&c, &a, r#"MATCH (g: spine/txn { id: $h }) RETURN g.id"#, &[("h", json!(h))], 1_003);
    assert_eq!(rows_of(r.rows), vec![BTreeMap::from([("g.id".to_string(), json!(h))])]);
    assert_eq!(staged_count(&c, "T"), (TrancheState::Open, 0));
    // OPPOSED: the ORDINARY door is unchanged — `g.state` reads `pending`.
    let rows = committed(&c, r#"MATCH (g: spine/txn { id: $h }) RETURN g.state"#, &[("h", json!(h))]);
    assert_eq!(rows, vec![BTreeMap::from([("g.state".to_string(), json!("pending"))])]);
}

/// Test 40(b): what stays in — an edge toward an EXISTING ghost by `id`
/// stages; `H` then decides and clears, and nothing the child relied on
/// moved: its head by `id` binds the same row (the landing itself is the
/// lander's to run).
// RED: `guard_projected_heads` made to refuse every projected head → this
// child is refused. OPPOSED: 40(c).
#[test]
fn t19_40b_an_edge_toward_an_existing_ghost_stages_and_its_row_may_decide_and_clear_underneath() {
    let dir = root("t40b");
    let c = fresh_with(&dir, &[alpha_citing()], quick());
    let x = doc(&c, "x", "published");
    let (h, seq_h) = pending_row(&dir, "H");
    let k = key(0x7c);
    begin(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    let (ord, _, changed) = staged(c.tranche_stage(
        &a,
        one("s0", r#"MATCH (g: spine/txn { id: $h }) MATCH (d: fixture-alpha/doc { id: $x }) CREATE (d) -[cites]-> (g)"#, &[("h", json!(h)), ("x", json!(x))]),
        1_001,
    ));
    assert_eq!((ord, changed), (0, false));
    // H decides and clears underneath.
    let mut db = SpineDb::open(&dir.join(SPINE_FILE)).unwrap();
    db.mark_decided(seq_h).unwrap();
    assert!(db.mark_cleared(seq_h, &json!({ "test": "cleared" })).unwrap());
    assert_eq!(committed(&c, r#"MATCH (g: spine/txn { id: $h }) RETURN g.state"#, &[("h", json!(h))])[0]["g.state"], json!("cleared"));
    // The child relied on the row's EXISTENCE only, which cannot move: under
    // key its head still binds, and the staged edge is in the view.
    let r = under(&c, &a, r#"MATCH (g: spine/txn { id: $h }) RETURN g.id"#, &[("h", json!(h))], 1_002);
    assert_eq!(rows_of(r.rows).len(), 1);
    assert_eq!(staged_count(&c, "T"), (TrancheState::Staging, 1));
    // THE TRANCHE LANDS CLEAN: the re-validation re-runs the match
    // against the row as it stands now (cleared) and the edge still compiles
    // identically; the `cites` edge is in the committed pool.
    let landed = land(&c, &a, 1_003).expect("the tranche lands clean");
    assert_eq!((landed.state, landed.children.len()), (TrancheState::Landed, 1));
    assert!(!landed.children[0].deduplicated);
    // (A hop into the projected namespace is staged out at the ordinary
    // door too, so the landed edge is read off the pool file itself.)
    let pool = rusqlite::Connection::open(Pool::path_in(&dir, &PoolName::new(ALPHA))).unwrap();
    let edges: i64 = pool
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE src = ?1 AND kind = 'cites' AND dst_pool = ?2 AND dst = ?3",
            rusqlite::params![x, project::PROJECTED_POOL, h],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(edges, 1, "the edge toward the ghost landed");
    assert_eq!(c.tranche_store("T", None).unwrap().state, TrancheState::Landed);
}

/// Test 40(c): the ABSENT row. `H2` names no row; a child `REMOVE (d)
/// -[cites]-> (g)` toward it is refused `ghost_target_unknown` (the ordinary
/// door would stage zero deltas), the tranche still `open`; a read under key
/// of it is refused the same way.
// RED (measured): the ghost guard under a tranche restricted back to
// `CREATE`-edge targets (`guard_projected_heads` not called in `stage`) → the
// REMOVE child stages with zero deltas (`changed: false`). OPPOSED: 40(b)'s
// existing row stages.
#[test]
fn t19_40c_a_projected_head_naming_no_row_is_refused_ghost_target_unknown_under_a_tranche() {
    let dir = root("t40c");
    let c = fresh_with(&dir, &[alpha_citing()], quick());
    let x = doc(&c, "x", "published");
    // `H2`: a transaction whose row does NOT exist yet — its ghost id is a
    // function of its hash, so the child can name it before it is appended.
    let h2_env = Envelope {
        statement: "-- H2: appended only in the red's tail".into(),
        bindings: BTreeMap::new(),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::CoreMechanical { operation: "test-h2".into() },
            instrument: instrument(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some("H2".into()),
    };
    let h2_hash = h2_env.hash_identity().unwrap();
    let h2 = project::ghost_id(&h2_hash).0;
    let k = key(0x7d);
    begin(&c, "T", &k, 1_000);
    let a = actor("T", &k);
    let r = c.tranche_stage(
        &a,
        one("s0", r#"MATCH (g: spine/txn { id: $h }) MATCH (d: fixture-alpha/doc { id: $x }) REMOVE (d) -[cites]-> (g)"#, &[("h", json!(h2)), ("x", json!(x))]),
        1_001,
    );
    match r {
        Err(e) => match only_witness(e) {
            Witness::GhostTargetUnknown { target } => assert!(target.contains(&h2), "{target}"),
            other => panic!("expected ghost_target_unknown, got {other:?}"),
        },
        Ok(staged_anyway) => {
            // THE RED'S LANDING HALF, measured rather than asserted
            // from prose — reachable only with the guard gone: the REMOVE
            // child staged with zero deltas against an absent row; `H2` is
            // now appended (a row appears at the address the child named), a
            // foreign `CREATE (d) -[cites]-> (g)` lands, and the landing's
            // re-validation compiles a REMOVE delta the staged payload does
            // not carry → `commit` → `failed`, `tranche_landing_refused {why:
            // revalidation}`.
            let mut db = SpineDb::open(&dir.join(SPINE_FILE)).unwrap();
            db.append_pending(&h2_env, &h2_hash, &spine::delta::CompiledPayload { parts: vec![], minted: BTreeMap::new() })
                .unwrap();
            land_edge_outside_cites(&dir, &x, &h2);
            let outcome = land(&c, &a, 1_003);
            panic!(
                "the ghost guard is gone: the REMOVE child staged ({staged_anyway:?}) and the landing \
                 answered {outcome:?} (store: {:?}) — the measured red is `failed`, \
                 `tranche_landing_refused {{why: revalidation}}`",
                c.tranche_store("T", None).map(|s| (s.state, s.reason, s.refusal))
            );
        }
    }
    assert_eq!(staged_count(&c, "T"), (TrancheState::Open, 0));
    match c.tranche_read(&a, &req(r#"MATCH (g: spine/txn { id: $h }) RETURN g.id"#, &[("h", json!(h2))]), 1_002) {
        Err(e) => assert!(matches!(only_witness(e), Witness::GhostTargetUnknown { .. })),
        other => panic!("a read under key of an absent ghost must be refused, got {other:?}"),
    }
    // OPPOSED: the ORDINARY door reads the absent ghost as the empty match.
    assert!(committed(&c, r#"MATCH (g: spine/txn { id: $h }) RETURN g.id"#, &[("h", json!(h2))]).is_empty());
}

// ---------------------------------------------------------------------------
// g8 — `exec::contested_claims` derives its HELD set from the PRE-GRANT match
// set and the row's payload, so the stale-match
// witness names the true slices and `claims_for`'s fallback (link on EVERY
// state slice of the pool) is reached by no door path.
// ---------------------------------------------------------------------------

/// A foreign holder parked on `held` under its own undecided row.
fn park(dir: &Path, held: &ClaimSet) -> Seq {
    let env = Envelope {
        statement: "-- parking holder".into(),
        bindings: BTreeMap::new(),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::CoreMechanical {
                operation: "test-park".into(),
            },
            instrument: instrument(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some("park".into()),
    };
    let hash = env.hash_identity().unwrap();
    let mut db = SpineDb::open(&dir.join(SPINE_FILE)).unwrap();
    let seq = db
        .append_pending(&env, &hash, &spine::delta::CompiledPayload { parts: vec![], minted: BTreeMap::new() })
        .unwrap();
    locks::acquire(&db, seq, held, &SpineConfig::default()).unwrap();
    seq
}

/// Move a node's state UNDER the lock table, as a holder that already holds
/// its locks would land it.
fn poison_state(dir: &Path, node: &str, state: &str) {
    let pname = PoolName::new(ALPHA);
    let mut pool = Pool::open(&Pool::path_in(dir, &pname)).unwrap();
    let part = spine::delta::CompiledPart {
        pool: pname,
        schema_version: alpha().version,
        deltas: vec![spine::delta::Delta::SetField {
            node: NodeId(node.to_string()),
            path: vec!["state".into()],
            value: json!(state),
        }],
        premises: vec![],
        far_versions: BTreeMap::new(),
    };
    pool.apply_part(&TxHash([9; 32]), 0, &part).unwrap();
}

/// Block until some seq other than `known` has QUEUED on `resource`.
fn wait_other_queued(dir: &Path, resource: &str, known: Seq) {
    let sql = spine_sql(dir);
    sql.busy_timeout(Duration::from_secs(2)).unwrap();
    let started = Instant::now();
    loop {
        let n: i64 = sql
            .query_row(
                "SELECT COUNT(*) FROM locks WHERE resource = ?1 AND granted = 0 AND seq != ?2",
                rusqlite::params![resource, known.0],
                |r| r.get(0),
            )
            .unwrap();
        if n > 0 {
            return;
        }
        assert!(started.elapsed() < Duration::from_secs(5), "nothing queued on {resource}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The ORDINARY door's stale-match witness after revision 6: `moved` and
/// `parked` are drafts; a keyless `MATCH (d: doc {state: "draft"}) SET
/// d.state = "published"` queues behind a holder on `parked`'s body; `moved`
/// is poisoned to `published` underneath; under `restart_bound = 0` the
/// re-match drops `moved` and the statement is refused `stale_match`. The
/// contested set names `body:moved write` and NO `sslice:doc:superseded` —
/// a slice the statement never names.
// RED (measured): `contested_claims` deriving `held` from
// `re_matched` with the old payload (the pre-revision-6 code) → `moved` is in
// neither the re-match nor the payload's creates, `claims_for` takes the
// fallback, and the witness names `sslice:doc:superseded link`. OPPOSED: the
// body that left IS named.
#[test]
fn g8_the_stale_match_witness_is_derived_from_the_pre_grant_match_and_never_names_the_fallbacks_slices() {
    let dir = root("g8");
    let mut cfg = SpineConfig::default();
    cfg.restart_bound = 0;
    cfg.escalation_threshold = 0;
    cfg.lock_wait_ceiling = Duration::from_secs(10);
    cfg.lock_poll_interval = Duration::from_millis(3);
    let c = fresh_with(&dir, &[alpha()], cfg);
    let moved = doc(&c, "moved", "draft");
    let parked = doc(&c, "parked", "draft");
    let holder = park(&dir, &claim(ALPHA, body(&parked), LockMode::Write));
    let err = std::thread::scope(|s| {
        let door = &c;
        let stmt = s.spawn(move || {
            keyless(door, "g8", r#"MATCH (d: fixture-alpha/doc { state: "draft" }) SET d.state = "published" RETURN d.id"#, &[])
        });
        wait_other_queued(&dir, &format!("body:{parked}"), holder);
        poison_state(&dir, &moved, "published");
        let db = SpineDb::open(&dir.join(SPINE_FILE)).unwrap();
        locks::release_at_clear(&db, holder).unwrap();
        stmt.join().unwrap().expect_err("the restart bound is exhausted on the first restart")
    });
    match only_witness(err) {
        Witness::StaleMatch { contested } => {
            assert!(contested.iter().any(|cl| cl.resource == body(&moved)), "names the body that left: {contested:?}");
            assert!(
                !contested.iter().any(|cl| locks::resource_key(&cl.resource) == "sslice:doc:superseded"),
                "a slice the statement never names is in the witness — the fallback was reached: {contested:?}"
            );
        }
        other => panic!("expected stale_match, got {other:?}"),
    }
}
