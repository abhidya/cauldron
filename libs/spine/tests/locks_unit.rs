//! The lock-table tests (including the pool-qualification rule). The lock
//! tables live in `spine.db`, so every
//! claimant here is a REAL spine sequence number issued by a REAL pending
//! row: grant order is the spine's own insertion order, and a test that
//! synthesized its numbers would be testing a different mechanism.
//!
//! **N >= 2, deliberately.** Arbitration cannot be exercised by its empty or
//! single case, so every contention contract below
//! is asserted with two or more claimants and every BLOCKING contract with
//! two or more real OS threads, each on its own connection to the same file —
//! which is also what makes these locks cross-process rather than in-process.
//! The `N = 1` arm is present for exactly one reason: to pin that a single
//! holder is never refused its own grant.
//!
//! Every checker here was forced red at least once.

use spine::config::SpineConfig;
use spine::delta::{CompiledPart, CompiledPayload};
use spine::envelope::{Envelope, Initiator, Instrument, Mode, Provenance, TxHash};
use spine::error::SpineError;
use spine::locks::{
    acquire, check_link_claims, compatible, membership_mode, queue, release_at_clear,
    release_on_abort, resource_key, Claim, ClaimSet, LockMode, Resource, COMPATIBILITY,
};
use spine::pool::{NodeId, QualifiedNodeId};
use spine::recover::abort_row;
use spine::schema_def::{Direction, PoolName};
use spine::spine_db::{Seq, SpineDb};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

test_scratch::install!();

// ---- fixtures -------------------------------------------------------------

const POOL: &str = "fixture-alpha";
const OTHER_POOL: &str = "fixture-beta";

fn fresh_spine_path(test: &str) -> PathBuf {
    let dir = test_scratch::tmp_path(format!("spine-locks-{}-{}", test, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join("spine.db")
}

fn envelope(initiator: Initiator) -> Envelope {
    Envelope {
        statement: "locks unit".to_string(),
        bindings: BTreeMap::new(),
        format_version: 1,
        provenance: Provenance {
            initiator,
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn payload() -> CompiledPayload {
    CompiledPayload {
        parts: vec![CompiledPart {
            pool: PoolName::new(POOL),
            schema_version: 1,
            deltas: vec![],
            premises: vec![],
            far_versions: Default::default(),
        }],
        minted: BTreeMap::new(),
    }
}

/// Open `n` real transactions and hand back their sequence numbers, in issue
/// order — the numbers the lock queues are ordered by.
fn open_transactions(db: &mut SpineDb, n: u8) -> Vec<Seq> {
    (0..n)
        .map(|i| open_one(db, i, Initiator::Agent {
            thread: "locks-unit".into(),
            dispatch: format!("claimant-{i}"),
        }))
        .collect()
}

fn open_one(db: &mut SpineDb, tag: u8, initiator: Initiator) -> Seq {
    let mut bytes = [0u8; 32];
    bytes[0] = tag;
    // A distinct hash per row; the identity itself is not this suite's
    // subject, the sequence number it issues is.
    bytes[1] = (db.pending().expect("pending").len() % 251) as u8;
    bytes[2] = 0x1c;
    db.append_pending(&envelope(initiator), &TxHash(bytes), &payload())
        .expect("open a transaction")
}

fn pool() -> PoolName {
    PoolName::new(POOL)
}

fn body(id: &str) -> Resource {
    Resource::Body {
        node: NodeId(id.to_string()),
    }
}

fn eslice(node: &str, kind: &str, direction: Direction) -> Resource {
    Resource::EdgeSlice {
        node: NodeId(node.to_string()),
        kind: kind.to_string(),
        direction,
    }
}

fn tslice(node_type: &str) -> Resource {
    Resource::TypeSlice {
        node_type: node_type.to_string(),
    }
}

fn edge(src: &str, kind: &str, dst_pool: &str, dst: &str) -> Resource {
    Resource::Edge {
        src: NodeId(src.to_string()),
        kind: kind.to_string(),
        dst: QualifiedNodeId {
            pool: PoolName::new(dst_pool),
            id: NodeId(dst.to_string()),
        },
    }
}

/// One claim set, pool-qualified (pool identity plus resource
/// identity, always).
fn claims(items: &[(&str, Resource, LockMode)]) -> ClaimSet {
    let mut set = ClaimSet::new();
    for (p, r, m) in items {
        set.insert(Claim {
            pool: PoolName::new(*p),
            resource: r.clone(),
            mode: *m,
        });
    }
    set
}

fn one(resource: Resource, mode: LockMode) -> ClaimSet {
    claims(&[(POOL, resource, mode)])
}

/// Bounded polling with backoff is the waiting mechanism; these tests drive both bounds from configuration, never constants.
fn cfg(ceiling_ms: u64) -> SpineConfig {
    SpineConfig {
        lock_poll_interval: Duration::from_millis(1),
        lock_wait_ceiling: Duration::from_millis(ceiling_ms),
        ..SpineConfig::default()
    }
}

/// A generous ceiling for claims that MUST eventually be granted: long
/// enough that a slow machine never reds it, short enough that a genuine
/// deadlock fails the test instead of hanging the suite.
fn patient() -> SpineConfig {
    cfg(20_000)
}

/// Is `seq`'s claim on `resource` IN THIS MODE enrolled, and granted?
/// Mode-qualified deliberately: one claimant can hold several modes on one
/// resource (an upgrade-intent and the write it converts to, for instance),
/// and a mode-blind lookup would read the wrong row.
fn grant_state(db: &SpineDb, resource: &Resource, seq: Seq, mode: LockMode) -> Option<bool> {
    queue(db, &pool(), resource)
        .expect("queue")
        .into_iter()
        .find(|e| e.seq == seq && e.mode == mode)
        .map(|e| e.granted)
}

/// Wait (bounded) until `seq` is enrolled on `resource` in `mode` but NOT
/// granted — i.e. it is genuinely queued behind someone.
fn await_queued(db: &SpineDb, resource: &Resource, seq: Seq, mode: LockMode) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if grant_state(db, resource, seq, mode) == Some(false) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("seq {seq} never appeared as a queued {mode:?} claimant on {resource:?}");
}

/// A claimant that RECORDS the moment it is granted, by pushing its label
/// onto a shared log. Grant ORDER between two waiters is a claim no pair of
/// `Instant`s can settle without a tie-break, and this is the mechanism the
/// sequence-order-versus-arrival-order test needs.
fn recording_claimant(
    path: &Path,
    seq: Seq,
    set: ClaimSet,
    config: SpineConfig,
    label: &'static str,
    log: Arc<Mutex<Vec<&'static str>>>,
) -> std::thread::JoinHandle<spine::error::Result<()>> {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let db = SpineDb::open(&path).expect("thread open");
        let granted = acquire(&db, seq, &set, &config).map(|_| ());
        if granted.is_ok() {
            log.lock().unwrap_or_else(|e| e.into_inner()).push(label);
        }
        granted
    })
}

/// A snapshot of the grant log, taken and released — never held across an
/// assertion, whose failure message would otherwise re-lock it.
fn granted_so_far(log: &Arc<Mutex<Vec<&'static str>>>) -> Vec<&'static str> {
    log.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Spawn a claimant on its own connection to the same spine file.
fn claimant(
    path: &Path,
    seq: Seq,
    set: ClaimSet,
    config: SpineConfig,
) -> std::thread::JoinHandle<spine::error::Result<Instant>> {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let db = SpineDb::open(&path).expect("thread open");
        acquire(&db, seq, &set, &config).map(|_| Instant::now())
    })
}

// ---- the compatibility matrix, as data ------------------------------------

#[test]
fn the_compatibility_matrix_is_the_specified_table_and_is_symmetric() {
    use LockMode::*;
    // The semaphore shape, SETTLED: reads share, writes are exclusive.
    assert!(compatible(Read, Read));
    for m in LockMode::ALL {
        assert!(!compatible(Write, m), "write is exclusive against {m:?}");
        assert!(!compatible(m, Write), "write is exclusive against {m:?}");
    }
    // Link: shared among membership changers, conflicting with read and
    // write on the same slice.
    assert!(compatible(Link, Link));
    assert!(!compatible(Link, Read));
    assert!(!compatible(Read, Link));
    // Upgrade-intent: at most one, compatible with readers.
    assert!(compatible(UpgradeIntent, Read));
    assert!(compatible(Read, UpgradeIntent));
    assert!(!compatible(UpgradeIntent, UpgradeIntent));
    // The one pair the rule never names (interior choice, conservative):
    assert!(!compatible(UpgradeIntent, Link));

    for a in LockMode::ALL {
        for b in LockMode::ALL {
            assert_eq!(
                COMPATIBILITY[a.index()][b.index()],
                COMPATIBILITY[b.index()][a.index()],
                "every compatibility clause is a mutual relation: {a:?} vs {b:?}"
            );
        }
    }
}

// ---- shared reads coexist; a writer excludes (SETTLED) --------------------

#[test]
fn shared_reads_coexist_across_real_threads() {
    let path = fresh_spine_path("shared-reads");
    let mut db = SpineDb::create(&path).expect("create");
    let s = open_transactions(&mut db, 3);
    let r = body("nShared");

    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = s
        .iter()
        .map(|seq| {
            let path = path.clone();
            let seq = *seq;
            let barrier = barrier.clone();
            let set = one(r.clone(), LockMode::Read);
            std::thread::spawn(move || {
                let db = SpineDb::open(&path).expect("thread open");
                barrier.wait();
                acquire(&db, seq, &set, &cfg(5_000)).map(|_| ())
            })
        })
        .collect();
    for h in handles {
        h.join().expect("join").expect("a read never waits on a read");
    }
    let q = queue(&db, &pool(), &r).expect("queue");
    assert_eq!(q.len(), 3, "three readers, one resource");
    assert!(q.iter().all(|e| e.granted), "all granted: {q:?}");
}

#[test]
fn a_writer_excludes_readers_and_is_granted_when_they_release() {
    let path = fresh_spine_path("writer-excludes");
    let mut db = SpineDb::create(&path).expect("create");
    let s = open_transactions(&mut db, 2);
    let (reader, writer) = (s[0], s[1]);
    let r = body("nExcl");

    acquire(&db, reader, &one(r.clone(), LockMode::Read), &patient()).expect("reader");
    let h = claimant(&path, writer, one(r.clone(), LockMode::Write), patient());

    // The writer is enrolled and WAITING — never granted alongside the read.
    await_queued(&db, &r, writer, LockMode::Write);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        grant_state(&db, &r, writer, LockMode::Write),
        Some(false),
        "still waiting"
    );

    release_at_clear(&db, reader).expect("release at the cleared flip");
    h.join().expect("join").expect("the writer is granted, not refused");
    assert_eq!(grant_state(&db, &r, writer, LockMode::Write), Some(true));
}

// ---- the queue-order DEPARTURE --------------------------------------------

#[test]
fn an_earlier_reader_passes_a_queued_writer_while_a_later_reader_waits_behind_it() {
    let path = fresh_spine_path("queue-order");
    let mut db = SpineDb::create(&path).expect("create");
    let s = open_transactions(&mut db, 4);
    let (r1, r2, w3, r4) = (s[0], s[1], s[2], s[3]);
    let r = body("nDeparture");

    // An older reader holds the resource; a writer queues behind it.
    acquire(&db, r1, &one(r.clone(), LockMode::Read), &patient()).expect("r1");
    let writer = claimant(&path, w3, one(r.clone(), LockMode::Write), patient());
    await_queued(&db, &r, w3, LockMode::Write);

    // THE DEPARTURE: a reader whose number is EARLIER than the queued
    // writer's proceeds ahead of it. Under the literal arrival-order rule
    // this call would wait; ordered by sequence number it is granted, and
    // that is what keeps every wait pointing at an older transaction.
    acquire(&db, r2, &one(r.clone(), LockMode::Read), &cfg(1_000)).expect("the earlier reader passes");
    assert_eq!(grant_state(&db, &r, r2, LockMode::Read), Some(true));
    assert_eq!(
        grant_state(&db, &r, w3, LockMode::Write),
        Some(false),
        "the writer still waits"
    );

    // The literal rule, PRESERVED for every reader arriving after the writer
    // queued: a LATER-numbered reader waits behind it.
    let err = acquire(&db, r4, &one(r.clone(), LockMode::Read), &cfg(150))
        .expect_err("a later reader must wait behind the queued writer");
    match &err {
        SpineError::Busy { message } => assert!(
            message.contains(&format!("seq {w3}")),
            "the wait names the writer it is behind: {message}"
        ),
        other => panic!("expected Busy at the ceiling, got {other:?}"),
    }

    // Drain the readers: the writer's turn comes.
    release_at_clear(&db, r1).expect("release");
    release_at_clear(&db, r2).expect("release");
    writer.join().expect("join").expect("the writer is granted");
    assert_eq!(grant_state(&db, &r, w3, LockMode::Write), Some(true));

    // And the later reader, retried, is granted once the writer clears.
    release_at_clear(&db, w3).expect("release");
    acquire(&db, r4, &one(r.clone(), LockMode::Read), &cfg(1_000)).expect("r4 after the writer");
}

/// SEQUENCE ORDER, DISCRIMINATED FROM ARRIVAL ORDER — which nothing above
/// does, and that is the point of this test.
///
/// Every other queue-order checker in this file enrols its claimants in the
/// same order as their sequence numbers. Arrival order and sequence order then
/// agree on every grant, and no assertion in any of them can tell which of the
/// two the grant rule actually consults: they would all stay green on a lock
/// module that granted first-come-first-served and had never heard of a
/// sequence number.
///
/// This one takes the two apart. A granted write at the OLDEST number holds
/// the resource; a conflicting claim for a much LATER seq enrols FIRST and
/// waits; only then does a conflicting claim for an INTERMEDIATE seq enrol.
/// When the holder releases, the queue is the spine's own insertion
/// order — so the intermediate number is granted and the later one goes on
/// waiting, however long it has been in the queue.
///
/// There is no race to lose here, and that is a property of the rule rather
/// than of the timing: while `middle` is enrolled and ungranted it BLOCKS
/// `latest` (a queued conflicting row with an earlier number), and `latest`
/// never blocks `middle` (a queued conflicting row with a later number). Both
/// waiters may poll in any order, in either thread, at any speed.
// RED: changing `acquire`'s one grant rule from `other_seq < seq.0` to
// arrival order — "any ungranted conflicting row blocks", which is the
// literal first-come rule — grants `latest` first, because it enrolled first.
// The recorded grant order then reads ["latest", "middle"] and the
// still-waiting assertion fires as well.
#[test]
fn an_earlier_seq_that_enrolled_last_is_granted_before_a_later_seq_that_enrolled_first() {
    let path = fresh_spine_path("seq-not-arrival");
    let mut db = SpineDb::create(&path).expect("create");
    let s = open_transactions(&mut db, 5);
    // Deliberately NOT adjacent: holder, then a gap, then the intermediate
    // number, then another gap, then the latest.
    let (holder, middle, latest) = (s[0], s[2], s[4]);
    assert!(holder < middle && middle < latest, "the fixture's own premise");
    let r = body("nSeqOrder");

    acquire(&db, holder, &one(r.clone(), LockMode::Write), &patient()).expect("the holder");

    // ARRIVAL ORDER, DELIBERATELY BACKWARDS.
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let late = recording_claimant(
        &path,
        latest,
        one(r.clone(), LockMode::Write),
        patient(),
        "latest",
        order.clone(),
    );
    await_queued(&db, &r, latest, LockMode::Write);
    let mid = recording_claimant(
        &path,
        middle,
        one(r.clone(), LockMode::Write),
        patient(),
        "middle",
        order.clone(),
    );
    await_queued(&db, &r, middle, LockMode::Write);
    assert!(
        granted_so_far(&order).is_empty(),
        "nothing is granted while the holder holds"
    );

    release_at_clear(&db, holder).expect("the holder clears");

    // THE INTERMEDIATE NUMBER WINS, though it arrived second.
    mid.join()
        .expect("join")
        .expect("the earlier sequence number is granted");
    assert_eq!(grant_state(&db, &r, middle, LockMode::Write), Some(true));

    // …and the LATER number, which has been queued longer than anyone, is
    // still waiting — behind a claimant that enrolled after it.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        grant_state(&db, &r, latest, LockMode::Write),
        Some(false),
        "seq {latest} enrolled first and still waits behind seq {middle}"
    );
    assert_eq!(
        granted_so_far(&order),
        vec!["middle"],
        "grant order is SEQUENCE order, not arrival order"
    );

    // Drain: the later number's turn comes last, and it is granted rather
    // than starved.
    release_at_clear(&db, middle).expect("release");
    late.join()
        .expect("join")
        .expect("the later sequence number is granted last, never refused");
    assert_eq!(granted_so_far(&order), vec!["middle", "latest"]);
}

// ---- link mode -----------------------------------------------------------

#[test]
fn link_shares_with_link_and_conflicts_with_read_and_write_on_the_slice() {
    let path = fresh_spine_path("link");
    let mut db = SpineDb::create(&path).expect("create");
    let s = open_transactions(&mut db, 5);
    let slice = eslice("nAuthor", "wrote", Direction::Out);

    // Two edge-connects commute: link shares with link. This is the
    // connectors' carve-out delivered as a theorem — any number of connectors
    // work on one node at once.
    acquire(&db, s[0], &one(slice.clone(), LockMode::Link), &patient()).expect("link 1");
    acquire(&db, s[1], &one(slice.clone(), LockMode::Link), &patient()).expect("link 2");
    let q = queue(&db, &pool(), &slice).expect("queue");
    assert_eq!(q.len(), 2);
    assert!(q.iter().all(|e| e.granted));

    // …and conflicts with read on the same slice (an absence watch needs the
    // membership held still).
    let err = acquire(&db, s[2], &one(slice.clone(), LockMode::Read), &cfg(150))
        .expect_err("read must not share a slice with link");
    assert!(matches!(err, SpineError::Busy { .. }), "{err:?}");

    release_at_clear(&db, s[0]).expect("release");
    release_at_clear(&db, s[1]).expect("release");

    // A slice WRITE excludes link, both ways.
    acquire(&db, s[3], &one(slice.clone(), LockMode::Write), &patient()).expect("slice write");
    let err = acquire(&db, s[4], &one(slice.clone(), LockMode::Link), &cfg(150))
        .expect_err("link must not share a slice with a writer");
    assert!(matches!(err, SpineError::Busy { .. }), "{err:?}");
}

#[test]
fn link_is_refused_on_a_scalar_slice_and_on_anything_that_is_not_a_slice() {
    let path = fresh_spine_path("link-legality");
    let mut db = SpineDb::create(&path).expect("create");
    let s = open_transactions(&mut db, 1);

    // The rule, in one place: an at-most-one slice's membership change is a
    // slice WRITE, never link.
    assert_eq!(membership_mode(true), LockMode::Write);
    assert_eq!(membership_mode(false), LockMode::Link);

    // The grammar MARKS them; this module never guesses. The fixture's `current_of`
    // is at most one per author.
    let alpha = spine::schema_def::fixtures::fixture_alpha();
    assert!(alpha.is_scalar_slice("current_of", Direction::In));
    assert!(!alpha.is_scalar_slice("wrote", Direction::Out));
    let scalar = |p: &PoolName, kind: &str, d: Direction| {
        p.as_str() == POOL && alpha.is_scalar_slice(kind, d)
    };

    let scalar_link = one(eslice("nAuthor", "current_of", Direction::In), LockMode::Link);
    match check_link_claims(&scalar_link, &scalar) {
        Err(SpineError::Usage { message }) => {
            assert!(message.contains("at-most-one"), "{message}");
            assert!(message.contains("current_of"), "names the slice: {message}");
        }
        other => panic!("link on a scalar slice must be refused, got {other:?}"),
    }
    // The same claim in WRITE mode is exactly what the rule demands, and passes.
    let scalar_write = one(eslice("nAuthor", "current_of", Direction::In), LockMode::Write);
    check_link_claims(&scalar_write, &scalar).expect("a scalar slice takes a slice WRITE");
    // An ordinary slice still links.
    let ordinary = one(eslice("nAuthor", "wrote", Direction::Out), LockMode::Link);
    check_link_claims(&ordinary, &scalar).expect("an ordinary slice links");

    // The grammar-free half is enforced by `acquire` itself: link is a SLICE
    // mode, so a link claim on a body or on one edge record never reaches
    // the table at all.
    for r in [body("nAuthor"), edge("nAuthor", "wrote", POOL, "nDoc")] {
        match acquire(&db, s[0], &one(r.clone(), LockMode::Link), &cfg(150)) {
            Err(SpineError::Usage { message }) => {
                assert!(message.contains("slices only"), "{message}")
            }
            other => panic!("link on {r:?} must be refused as a usage error, got {other:?}"),
        }
        assert!(
            queue(&db, &pool(), &r).expect("queue").is_empty(),
            "a refused claim enrols nothing"
        );
    }
}

/// THE STATE SLICE: a fourth slice
/// shape, `sslice:<type>:<state>`. `link` is legal on it (a state change is a
/// membership change, and two of them on different nodes commute), its key
/// renders type then state, and in the table it is ONE resource — a `link`
/// on it conflicts with a `read` on it by the same compatibility row every
/// other slice obeys.
// RED (measured): `is_slice` not admitting `StateSlice` → `acquire`
// refuses the link as "slices only"; `resource_key` rendering state before
// type → the key assertion fails. OPPOSED: link on a BODY is still refused.
#[test]
fn link_is_legal_on_a_state_slice_whose_key_is_sslice_type_state() {
    let path = fresh_spine_path("state-slice");
    let mut db = SpineDb::create(&path).expect("create");
    let s = open_transactions(&mut db, 3);
    let slice = Resource::StateSlice {
        node_type: "doc".into(),
        state: "draft".into(),
    };
    assert_eq!(resource_key(&slice), "sslice:doc:draft");
    let scalar = |_: &PoolName, _: &str, _: Direction| false;
    check_link_claims(&one(slice.clone(), LockMode::Link), &scalar).expect("link is legal on a state slice");

    // Enrolled and granted as a link; a second link shares it.
    acquire(&db, s[0], &one(slice.clone(), LockMode::Link), &patient()).expect("link 1");
    acquire(&db, s[1], &one(slice.clone(), LockMode::Link), &patient()).expect("link 2");
    let q = queue(&db, &pool(), &slice).expect("queue");
    assert_eq!(q.len(), 2, "{q:?}");
    assert!(q.iter().all(|e| e.granted));
    // ...and a READ of the slice — a filtered scan under a tranche — waits
    // behind the state changes, so the key is one resource in the table.
    let err = acquire(&db, s[2], &one(slice.clone(), LockMode::Read), &cfg(150))
        .expect_err("read must not share a state slice with link");
    assert!(matches!(err, SpineError::Busy { .. }), "{err:?}");
    // OPPOSED: a body is still no slice.
    match acquire(&db, s[2], &one(body("nDoc"), LockMode::Link), &cfg(150)) {
        Err(SpineError::Usage { message }) => assert!(message.contains("slices only"), "{message}"),
        other => panic!("link on a body must be refused, got {other:?}"),
    }
}

// ---- upgrade-intent ------------------------------------------------------

#[test]
fn upgrade_intent_is_a_singleton_that_readers_may_share() {
    let path = fresh_spine_path("upgrade-singleton");
    let mut db = SpineDb::create(&path).expect("create");
    let s = open_transactions(&mut db, 3);
    let r = body("nIntent");

    acquire(&db, s[0], &one(r.clone(), LockMode::UpgradeIntent), &patient()).expect("the one intent");
    // At most one per resource.
    let err = acquire(&db, s[1], &one(r.clone(), LockMode::UpgradeIntent), &cfg(150))
        .expect_err("a second upgrade-intent must not be granted");
    assert!(matches!(err, SpineError::Busy { .. }), "{err:?}");
    // Compatible with readers — that is the whole point of the mode.
    acquire(&db, s[2], &one(r.clone(), LockMode::Read), &cfg(1_000)).expect("readers share it");
    assert_eq!(grant_state(&db, &r, s[2], LockMode::Read), Some(true));
}

#[test]
fn upgrade_intent_converts_to_write_when_the_reads_drain() {
    let path = fresh_spine_path("upgrade-convert");
    let mut db = SpineDb::create(&path).expect("create");
    let s = open_transactions(&mut db, 2);
    let (holder, reader) = (s[0], s[1]);
    let r = body("nConvert");

    acquire(&db, holder, &one(r.clone(), LockMode::UpgradeIntent), &patient()).expect("intent");
    acquire(&db, reader, &one(r.clone(), LockMode::Read), &patient()).expect("a reader alongside");

    // The conversion is NOT claim growth — it is the mode's definition — but
    // it cannot be granted while a read is held.
    let converting = claimant(&path, holder, one(r.clone(), LockMode::Write), patient());
    await_queued(&db, &r, holder, LockMode::Write);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        grant_state(&db, &r, holder, LockMode::Write),
        Some(false),
        "the intent does not become a write while a reader holds"
    );

    release_at_clear(&db, reader).expect("reads drain");
    converting.join().expect("join").expect("the intent converts");

    let q = queue(&db, &pool(), &r).expect("queue");
    assert_eq!(q.len(), 1, "the upgrade-intent row is consumed by its write: {q:?}");
    assert_eq!(q[0].seq, holder);
    assert_eq!(q[0].mode, LockMode::Write);
    assert!(q[0].granted);
}

// ---- claims complete at open; growth restarts ----------------------------

#[test]
fn claim_growth_is_refused_with_the_restart_signal() {
    let path = fresh_spine_path("growth");
    let mut db = SpineDb::create(&path).expect("create");
    let s = open_transactions(&mut db, 1);
    let seq = s[0];

    acquire(&db, seq, &one(body("nA"), LockMode::Read), &patient()).expect("the claim set at open");

    // Mid-flight discovery of a NEW resource: refused, with the restart
    // signal — a fresh sequence number is the caller's job.
    let grown = claims(&[
        (POOL, body("nA"), LockMode::Read),
        (OTHER_POOL, body("nB"), LockMode::Write),
    ]);
    match acquire(&db, seq, &grown, &cfg(150)) {
        Err(SpineError::Usage { message }) => {
            assert!(message.contains("claim growth"), "{message}");
            assert!(message.contains("fresh sequence number"), "{message}");
            assert!(
                message.contains(r#""restart":"claim_growth""#),
                "carries the structured restart signal: {message}"
            );
            assert!(message.contains("nB"), "names what it would have grown by: {message}");
        }
        other => panic!("claim growth must refuse with the restart signal, got {other:?}"),
    }

    // NO in-place read-to-write upgrade, ever: the same resource in a
    // stronger mode is growth too.
    match acquire(&db, seq, &one(body("nA"), LockMode::Write), &cfg(150)) {
        Err(SpineError::Usage { message }) => assert!(message.contains("claim growth"), "{message}"),
        other => panic!("an in-place upgrade must be refused, got {other:?}"),
    }

    // Nothing of the refused growth was enrolled: a refusal leaves the
    // queues exactly as it found them.
    assert!(queue(&db, &PoolName::new(OTHER_POOL), &body("nB"))
        .expect("queue")
        .is_empty());
    let q = queue(&db, &pool(), &body("nA")).expect("queue");
    assert_eq!(q.len(), 1);
    assert!(q[0].granted && q[0].mode == LockMode::Read);
}

// ---- THE DEADLOCK WITNESS ------------------------------------------------

/// The two-node crossed case: two transactions each holding a read
/// on one node and needing a write on the other's. Under the literal
/// arrival-order rule this deadlocks — the case that is WHY the departure
/// exists — and under sequence order every wait points at an older
/// transaction, so no cycle can form and both complete.
///
/// This MUST be, and is, a real two-thread test: the empty and single cases
/// cannot exhibit a cycle at all.
#[test]
fn the_two_node_crossed_case_completes_under_sequence_order() {
    let path = fresh_spine_path("crossed");
    let mut db = SpineDb::create(&path).expect("create");

    // --- the deterministic half: the older transaction holds both, the
    //     younger waits for it, and completes when it releases.
    let s = open_transactions(&mut db, 2);
    let (older, younger) = (s[0], s[1]);
    let x = body("nX");
    let y = body("nY");
    let crossed_a = claims(&[
        (POOL, x.clone(), LockMode::Read),
        (POOL, y.clone(), LockMode::Write),
    ]);
    let crossed_b = claims(&[
        (POOL, y.clone(), LockMode::Read),
        (POOL, x.clone(), LockMode::Write),
    ]);
    acquire(&db, older, &crossed_a, &patient()).expect("the older transaction");
    let waiter = claimant(&path, younger, crossed_b.clone(), patient());
    await_queued(&db, &x, younger, LockMode::Write);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        grant_state(&db, &x, younger, LockMode::Write),
        Some(false),
        "the younger transaction waits — on an OLDER one, which is the whole argument"
    );
    release_at_clear(&db, older).expect("release");
    waiter.join().expect("join").expect("the younger completes; no cycle formed");
    release_at_clear(&db, younger).expect("release");

    // --- THE WITNESS: the crossed state, constructed deterministically.
    //
    // The deterministic half above does not yet discriminate the two rules:
    // this module grants ALL of a claim set or NONE, so a waiter holds
    // nothing, and one transaction that simply gets there first never
    // deadlocks under either rule. The state that DOES discriminate is the
    // one the departure exists for — both transactions enrolled on both nodes,
    // crosswise, neither granted — and it is reached by parking both behind
    // an OLDER holder and then releasing it, so the interleaving is a
    // construction rather than a hoped-for race.
    //
    // Under ARRIVAL order from that state: A's read on X waits behind B's
    // queued write on X, B's read on Y waits behind A's queued write on Y,
    // forever, and there is no global lock to fall back on. Under SEQUENCE
    // order every wait points at an OLDER transaction only, so A (older)
    // takes both and B follows. Both complete, in that order.
    let s = open_transactions(&mut db, 3);
    let (blocker, a_seq, b_seq) = (s[0], s[1], s[2]);
    let x = body("nCrossX");
    let y = body("nCrossY");
    let a_set = claims(&[
        (POOL, x.clone(), LockMode::Read),
        (POOL, y.clone(), LockMode::Write),
    ]);
    let b_set = claims(&[
        (POOL, y.clone(), LockMode::Read),
        (POOL, x.clone(), LockMode::Write),
    ]);
    // An older holder parks both of them, so both enrol before either grants.
    acquire(
        &db,
        blocker,
        &claims(&[
            (POOL, x.clone(), LockMode::Write),
            (POOL, y.clone(), LockMode::Write),
        ]),
        &patient(),
    )
    .expect("the parking holder");

    let spawn_crossed = |seq: Seq, set: ClaimSet| {
        let path = path.clone();
        std::thread::spawn(move || {
            let db = SpineDb::open(&path).expect("thread open");
            let granted = acquire(&db, seq, &set, &cfg(8_000)).map(|_| Instant::now());
            if granted.is_ok() {
                std::thread::sleep(Duration::from_millis(5));
                release_at_clear(&db, seq).expect("release");
            }
            granted
        })
    };
    let a = spawn_crossed(a_seq, a_set);
    let b = spawn_crossed(b_seq, b_set);

    // The crossed state, on disk: each transaction enrolled on both nodes,
    // reading one and writing the other, neither granted.
    await_queued(&db, &x, a_seq, LockMode::Read);
    await_queued(&db, &y, a_seq, LockMode::Write);
    await_queued(&db, &y, b_seq, LockMode::Read);
    await_queued(&db, &x, b_seq, LockMode::Write);

    release_at_clear(&db, blocker).expect("unpark them into the crossed state");

    let at_a = a
        .join()
        .expect("join")
        .expect("the older transaction completes — under arrival order it would not");
    let at_b = b
        .join()
        .expect("join")
        .expect("and so does the younger — no cycle can form");
    assert!(
        at_a < at_b,
        "sequence order decides it: the OLDER transaction is granted first"
    );

    // --- a stress, not the witness: the same shape started simultaneously,
    //     many times, to catch anything the constructed case cannot see.
    for round in 0..12u8 {
        let s = open_transactions(&mut db, 2);
        let (a_seq, b_seq) = (s[0], s[1]);
        let x = body(&format!("nX{round}"));
        let y = body(&format!("nY{round}"));
        let a_set = claims(&[
            (POOL, x.clone(), LockMode::Read),
            (POOL, y.clone(), LockMode::Write),
        ]);
        let b_set = claims(&[
            (POOL, y.clone(), LockMode::Read),
            (POOL, x.clone(), LockMode::Write),
        ]);
        let barrier = Arc::new(Barrier::new(2));
        let mut hs = Vec::new();
        for (seq, set) in [(a_seq, a_set), (b_seq, b_set)] {
            let path = path.clone();
            let barrier = barrier.clone();
            hs.push(std::thread::spawn(move || {
                let db = SpineDb::open(&path).expect("thread open");
                barrier.wait();
                let granted = acquire(&db, seq, &set, &cfg(10_000));
                // Hold briefly, so the crossing is real rather than lucky.
                std::thread::sleep(Duration::from_millis(2));
                release_at_clear(&db, seq).expect("release");
                granted.map(|_| ())
            }));
        }
        for h in hs {
            h.join()
                .expect("join")
                .unwrap_or_else(|e| panic!("round {round} did not complete: {e:?}"));
        }
    }
}

// ---- the wait ceiling is loud --------------------------------------------

#[test]
fn the_wait_ceiling_is_loud_names_the_holder_and_leaves_no_dam_behind() {
    let path = fresh_spine_path("ceiling");
    let mut db = SpineDb::create(&path).expect("create");
    let s = open_transactions(&mut db, 3);
    let (holder, loser, later) = (s[0], s[1], s[2]);
    let r = body("nCeiling");

    acquire(&db, holder, &one(r.clone(), LockMode::Write), &patient()).expect("holder");

    let started = Instant::now();
    let err = acquire(&db, loser, &one(r.clone(), LockMode::Read), &cfg(200))
        .expect_err("a wait past the ceiling is never silent");
    let waited = started.elapsed();
    match &err {
        SpineError::Busy { message } => {
            assert!(message.contains("wait ceiling"), "{message}");
            assert!(
                message.contains(&format!("seq {holder}")),
                "NAMES the holder: {message}"
            );
            assert!(message.contains("write"), "names the holder's mode: {message}");
            assert!(message.contains("granted"), "and that it is held: {message}");
        }
        other => panic!("expected a loud Busy, got {other:?}"),
    }
    assert!(waited >= Duration::from_millis(200), "it waited: {waited:?}");
    assert!(waited < Duration::from_secs(5), "and it stopped: {waited:?}");

    // The abandoned claim withdrew itself: it must not dam the queue for
    // claimants behind it.
    let q = queue(&db, &pool(), &r).expect("queue");
    assert_eq!(q.len(), 1, "only the holder remains: {q:?}");
    assert_eq!(q[0].seq, holder);

    // Proof that it is not damming: once the holder releases, a LATER
    // claimant is granted at once. Had the abandoned row stayed queued with
    // its earlier number, this call would block behind a ghost.
    release_at_clear(&db, holder).expect("release");
    acquire(&db, later, &one(r.clone(), LockMode::Write), &cfg(500))
        .expect("no ghost claimant blocks the queue");
}

// ---- release: at the cleared flip, and on abort --------------------------

#[test]
fn locks_release_at_the_cleared_flip_and_on_abort_and_never_as_parts_land() {
    let path = fresh_spine_path("release");
    let mut db = SpineDb::create(&path).expect("create");
    let s = open_transactions(&mut db, 2);
    let (committing, aborting) = (s[0], s[1]);

    let held = claims(&[
        (POOL, body("nR1"), LockMode::Read),
        (POOL, tslice("doc"), LockMode::Link),
        (OTHER_POOL, body("nR2"), LockMode::Write),
    ]);
    acquire(&db, committing, &held, &patient()).expect("acquire");

    // A part landing is NOT a release: between one pool's landing and the
    // next the pools disagree, and this rule is one of the three ways that
    // window stays invisible.
    db.mark_decided(committing).expect("decide");
    db.mark_part_landed(committing, 0).expect("land the only part");
    assert_eq!(
        grant_state(&db, &body("nR1"), committing, LockMode::Read),
        Some(true)
    );

    // The cleared flip releases everything, in every pool.
    db.mark_cleared(committing, &serde_json::Value::Null).expect("clear");
    release_at_clear(&db, committing).expect("release at the cleared flip");
    assert!(queue(&db, &pool(), &body("nR1")).unwrap().is_empty());
    assert!(queue(&db, &pool(), &tslice("doc")).unwrap().is_empty());
    assert!(queue(&db, &PoolName::new(OTHER_POOL), &body("nR2"))
        .unwrap()
        .is_empty());
    // Idempotent: releasing a seq that holds nothing is a quiet no-op.
    release_at_clear(&db, committing).expect("idempotent");

    // The abort path releases too — the same act, named for its call site.
    acquire(&db, aborting, &one(body("nR3"), LockMode::Write), &patient()).expect("acquire");
    release_on_abort(&db, aborting).expect("release on abort");
    assert!(queue(&db, &pool(), &body("nR3")).unwrap().is_empty());
    // …and the aborted transaction's ROW remains forever.
    assert_eq!(db.pending().unwrap().len(), 1);
}

/// THE ABORT ARBITER, SEEN FROM THE LOCK TABLE (the release rule, and
/// recovery's abort): `recover::abort_row` on a DECIDED row answers `Ok(false)`
/// and leaves that row's locks exactly where they are.
///
/// This is the half of the arbiter that the lock table can see, and it is the
/// half the arbiter exists for. `abort_row` used to release the locks FIRST
/// and flip the row second, with no decided-check joining the two — so a
/// `recover()` racing a live door could read a row un-decided, watch the door
/// decide it, and then strip the locks that door was landing under. The
/// half-landed window the release rule requires to be invisible would go straight to the
/// next lock-taker, which is the release rule broken exactly where it is
/// load-bearing.
// RED: restoring the old body of `recover::abort_row` — an unconditional
// `locks::release_on_abort(spine, row.seq)` before the flip. The conflicting
// acquire below is then GRANTED instead of refused, which IS the leak; the
// `grant_state` assertion above it fires first and names it.
#[test]
fn a_failed_abort_never_strips_the_locks_of_the_decided_row_it_lost_to() {
    let path = fresh_spine_path("abort-arbiter-locks");
    let mut db = SpineDb::create(&path).expect("create");
    let s = open_transactions(&mut db, 2);
    let (landing, taker) = (s[0], s[1]);
    let r = body("nArbiter");

    // A LIVE DOOR: claims granted, row decided, parts NOT yet landed — the
    // half-landed window, exactly.
    acquire(&db, landing, &one(r.clone(), LockMode::Write), &patient()).expect("the door's grant");
    db.mark_decided(landing).expect("the door decides");

    // RECOVERY, racing it: it read this row un-decided a moment ago and now
    // tries to abort. The arbiter refuses, in one atomic conditional flip.
    let row = db.row_at(landing).expect("row");
    assert!(
        !abort_row(&mut db, &row).expect("the abort runs"),
        "a decided row is not recovery's to abort"
    );

    // THE ROW IS UNTOUCHED…
    let after = db.row_at(landing).expect("row");
    assert!(after.decided, "still decided");
    assert!(!after.cleared, "and NOT cleared");

    // …AND SO IS THE LOCK TABLE. Asserted on the table, and then on the
    // behaviour the table exists to produce.
    assert_eq!(
        grant_state(&db, &r, landing, LockMode::Write),
        Some(true),
        "the landing transaction's grant still stands"
    );
    let err = acquire(&db, taker, &one(r.clone(), LockMode::Write), &cfg(200))
        .expect_err("the half-landed window stays invisible to lock-takers");
    match &err {
        SpineError::Busy { message } => assert!(
            message.contains(&format!("seq {landing}")),
            "still blocked by the transaction that is landing: {message}"
        ),
        other => panic!("expected a loud Busy, got {other:?}"),
    }

    // The release comes at the CLEARED flip and at no other moment — which is
    // the rule the failed abort was protecting.
    db.mark_part_landed(landing, 0).expect("land the only part");
    db.mark_cleared(landing, &serde_json::Value::Null).expect("clear");
    release_at_clear(&db, landing).expect("release at the cleared flip");
    acquire(&db, taker, &one(r.clone(), LockMode::Write), &cfg(1_000))
        .expect("and only now is the next claimant granted");
}

// ---- mechanical writes queue like everyone else --------------------------

#[test]
fn a_mechanical_write_waits_its_turn_and_is_never_refused() {
    let path = fresh_spine_path("mechanical");
    let mut db = SpineDb::create(&path).expect("create");
    let reader = open_one(&mut db, 1, Initiator::Agent {
        thread: "locks-unit".into(),
        dispatch: "reader".into(),
    });
    let mechanical = open_one(
        &mut db,
        2,
        Initiator::CoreMechanical {
            operation: "count-cell-increment".into(),
        },
    );
    let r = body("nMech");

    acquire(&db, reader, &one(r.clone(), LockMode::Read), &patient()).expect("a reader holds");
    let h = claimant(&path, mechanical, one(r.clone(), LockMode::Write), patient());

    // "Cannot be refused" governs REFUSAL, not scheduling: it queues, and
    // lands late rather than torn. If it bypassed locks, the reader could
    // watch its node change underneath it.
    await_queued(&db, &r, mechanical, LockMode::Write);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        grant_state(&db, &r, mechanical, LockMode::Write),
        Some(false),
        "it waits"
    );

    release_at_clear(&db, reader).expect("release");
    h.join()
        .expect("join")
        .expect("and it is granted, never refused");
}

// ---- the N = 1 arm (stated for what it CAN show) -------------------------

/// A single holder is never refused its own grant. This is the one contract
/// the single case can prove, and it is here for exactly that: a seq's own
/// rows never conflict with each other, however many resources and however
/// strong the modes, and re-presenting an already-held set grants again
/// rather than refusing as growth.
#[test]
fn n_equals_one_a_single_holder_is_never_refused_its_own_grant() {
    let path = fresh_spine_path("single-holder");
    let mut db = SpineDb::create(&path).expect("create");
    let seq = open_transactions(&mut db, 1)[0];

    // Deliberately overlapping and deliberately strong: a read AND a write
    // on one body, a link and a write on slices, an edge — all one claimant.
    let set = claims(&[
        (POOL, body("nSolo"), LockMode::Read),
        (POOL, body("nSolo"), LockMode::Write),
        (POOL, body("nSolo"), LockMode::UpgradeIntent),
        (POOL, tslice("doc"), LockMode::Link),
        (POOL, eslice("nSolo", "wrote", Direction::Out), LockMode::Write),
        (POOL, edge("nSolo", "wrote", POOL, "nDoc"), LockMode::Write),
        (OTHER_POOL, body("nSoloBeta"), LockMode::Write),
    ]);
    acquire(&db, seq, &set, &cfg(500)).expect("a single holder is never blocked by itself");
    let q = queue(&db, &pool(), &body("nSolo")).expect("queue");
    assert_eq!(q.len(), 3, "three modes, one claimant, no self-conflict: {q:?}");
    assert!(q.iter().all(|e| e.granted));

    // Re-presenting the same set is not growth and is granted again.
    acquire(&db, seq, &set, &cfg(500)).expect("re-acquiring what it holds");
    // A SUBSET is not growth either.
    acquire(&db, seq, &one(body("nSolo"), LockMode::Write), &cfg(500)).expect("a subset");

    // An empty claim set is trivially granted and enrols nothing.
    acquire(&db, seq, &ClaimSet::new(), &cfg(500)).expect("the empty claim set");
}
