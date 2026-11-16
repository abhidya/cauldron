//! **Lock behaviour, THROUGH THE DOOR.**
//!
//! `tests/locks_unit.rs` already proves the lock TABLE at N >= 2: the
//! compatibility matrix, the grant rule, the queue-order departure, the
//! crossed deadlock witness, release, the ceiling. It proves them by calling
//! `locks::acquire` with claim sets it writes by hand.
//!
//! **This suite proves the same theorems as properties of WHOLE STATEMENTS.**
//! Every contending party here is a real `Cauldron::submit` or
//! `Cauldron::read`, so what is under test is the composition the lock unit
//! cannot see: the claim sets `ladle::compile::claims_for` actually DERIVES
//! from real statements, acquired at the sequence numbers the spine actually
//! ISSUES, released at the cleared flip the door actually PERFORMS. A door
//! that took a write lock on an edge's endpoint, or released per part, or
//! derived a link claim on a scalar slice, would pass every test in
//! `locks_unit.rs` and fail here.
//!
//! **N >= 2 with real OS threads, everywhere it is a contention claim**
//! (arbitration cannot be exercised by its empty or
//! single case). The N = 1 arm is present for the one thing the single case
//! CAN prove — that a single actor is never refused its own grant.
//!
//! Where a contended state has to be reached deterministically, it is
//! CONSTRUCTED rather than raced for: a foreign transaction parks the
//! resource, the door calls are allowed to enrol and queue behind it, the
//! queue is asserted on disk, and only then is the parking holder released.
//! The reason: a barrier race does not discriminate arrival order
//! from sequence order, because whoever enrols first usually grants first.
//!
//! Every checker here was forced RED at least once; the poison is named in a
//! `// RED:` comment above each test.

use spine::config::SpineConfig;
use spine::delta::{CompiledPayload, Delta};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance, TxHash};
use spine::error::SpineError;
use spine::ladle::compile::{self, CompileCtx, Fuel};
use spine::ladle::parse;
use spine::locks::{self, Claim, ClaimSet, LockMode, Resource};
use spine::pool::{NodeId, Pool};
use spine::read;
use spine::schema_def::{fixtures, Direction, PoolGrammar, PoolName};
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use spine::{Cauldron, Outcome, ReadRequest};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

test_scratch::install!();

#[path = "tranche/mod.rs"] mod tranche;

const ALPHA: &str = "fixture-alpha";
const BETA: &str = "fixture-beta";

// ---------------------------------------------------------------------------
// THE CARVE-OUT, AS A THEOREM: connect and read never block each other
// ---------------------------------------------------------------------------

/// THE RULE: readers of a node, and writers that only
/// connect an edge to it, never block one another. The three-kind lockable
/// taxonomy exists ONLY to deliver this; at whole-node
/// grain this test cannot pass.
///
/// Two halves, because the theorem has two halves:
///
/// - **the claims the door derives.** Six real statements — three
///   edge-connects to one author, three write transactions that merely READ
///   that author — are compiled by the real compiler, their claim sets taken
///   from `claims_for`, and all six acquired at six real sequence numbers with
///   a tight ceiling. All six are granted SIMULTANEOUSLY, and the author's
///   body queue holds six claims of which NOT ONE is a write. A door that
///   claimed a write on the connect's endpoint would deadlock five of six
///   against the ceiling here.
/// - **the same six through the door, concurrently**, on six real threads. All
///   six commit, three docs exist, and the author's machine-maintained count
///   cell is exactly three.
// RED: (a) `claims_for`'s `AddEdge` arm changed to claim `LockMode::Write` on
// both endpoint BODIES (whole-node grain — the design rejects it) — five of
// the six acquires died at the ceiling and the "no write on the author's body"
// assertion failed; (b) `membership_mode(false)` returning `Write` instead of
// `Link` — the three connectors serialized on the author's `wrote` slice and
// the simultaneity assertion failed.
#[test]
fn the_carve_out_is_a_theorem_connectors_and_readers_share_one_node_and_none_blocks() {
    let dir = root("carve-out");
    let c = fresh_cauldron(&dir, patient());
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let author = create_author(&c, "the shared author");
    let private: Vec<NodeId> = (0..3).map(|i| create_doc(&c, &format!("private {i}"))).collect();

    let connectors: Vec<Envelope> = (0..3)
        .map(|i| {
            commit(
                CONNECT,
                &[
                    ("a", serde_json::json!(author.0)),
                    ("t", serde_json::json!(format!("connected {i}"))),
                ],
            )
        })
        .collect();
    let readers: Vec<Envelope> = (0..3)
        .map(|i| {
            commit(
                READ_THE_AUTHOR,
                &[
                    ("a", serde_json::json!(author.0)),
                    ("p", serde_json::json!(private[i].0)),
                    ("t", serde_json::json!(format!("touched {i}"))),
                ],
            )
        })
        .collect();

    // --- half one: six real claim sets, six real numbers, one node.
    let mut held: Vec<Seq> = Vec::new();
    for env in connectors.iter().chain(readers.iter()) {
        let claims = derive_claims(&c, env);
        // The carve-out, read straight off the derivation: an edge-connect
        // claims LINK on the endpoint slices and READ on the endpoint body —
        // never a write on either endpoint.
        let seq = park(&dir, &claims, &format!("carve-out {}", held.len()));
        held.push(seq);
    }
    let q = locks::queue(&spine, &PoolName::new(ALPHA), &body(&author)).expect("queue");
    assert_eq!(
        q.len(),
        6,
        "three connectors and three readers all name the author: {q:?}"
    );
    assert!(
        q.iter().all(|e| e.mode == LockMode::Read),
        "NEVER a write lock on the endpoint of a connect: {q:?}"
    );
    assert!(
        q.iter().all(|e| e.granted),
        "and all six hold it AT ONCE — that is the theorem: {q:?}"
    );
    let slice = locks::queue(
        &spine,
        &PoolName::new(ALPHA),
        &eslice(&author, "wrote", Direction::Out),
    )
    .expect("queue");
    assert_eq!(slice.len(), 3, "three membership changers on one slice");
    assert!(
        slice.iter().all(|e| e.mode == LockMode::Link && e.granted),
        "two edge-connects commute, so they share the slice in LINK: {slice:?}"
    );
    for seq in &held {
        unpark(&dir, *seq);
    }

    // --- half two: the same six through the door, at six real threads.
    let outcomes: Vec<Result<Outcome, SpineError>> = std::thread::scope(|scope| {
        let door = &c;
        let handles: Vec<_> = connectors
            .into_iter()
            .chain(readers)
            .map(|env| scope.spawn(move || door.submit(env)))
            .collect();
        handles.into_iter().map(|h| h.join().expect("join")).collect()
    });
    for (i, out) in outcomes.iter().enumerate() {
        match out {
            Ok(Outcome::Committed { .. }) => {}
            other => panic!("participant {i} was blocked or refused: {other:?}"),
        }
    }
    assert_eq!(
        docs_by(&c, "connected"),
        3,
        "three concurrent connects, three docs"
    );
    let alpha = Pool::open(&Pool::path_in(&dir, &PoolName::new(ALPHA))).expect("alpha");
    assert_eq!(
        alpha
            .snapshot()
            .expect("snapshot")
            .count_cell(&author, "docs_written")
            .expect("cell"),
        Some(3),
        "the machine-maintained cell counted every commutative increment"
    );
    assert!(spine.pending().expect("pending").is_empty());
}

// ---------------------------------------------------------------------------
// writer exclusivity
// ---------------------------------------------------------------------------

/// A write lock is exclusive.
///
/// While a foreign transaction holds a write on a doc's body, a door writer
/// and a door READER-inside-a-write-transaction are both refused their grant
/// at the ceiling, loudly, naming the holder. When it releases, both go
/// through. Then the N >= 2 arm with no foreign holder at all: two real
/// threads both writing the same body SERIALIZE and both commit — exclusivity
/// is scheduling, never refusal, and the second revision proves neither write
/// was lost.
// RED: (a) `COMPATIBILITY`'s write row set to all-`true` — both door calls
// were granted alongside the holder; (b) `release_at_clear` made a no-op —
// the calls after the release never got through and the test hit its ceiling.
#[test]
fn writer_exclusivity_through_the_door_and_two_writers_serialize_without_refusal() {
    let dir = root("exclusivity");
    let c = fresh_cauldron(&dir, patient());
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let x = create_doc(&c, "the exclusive doc");
    let p = create_doc(&c, "somewhere else");

    let holder = park(&dir, &one(body(&x), LockMode::Write), "exclusive holder");

    let impatient = Cauldron::open_unrecovered(&dir, cfg(150)).expect("second handle");
    // A WRITER on the held body.
    let err = impatient
        .submit(commit(
            SET_TITLE,
            &[
                ("x", serde_json::json!(x.0)),
                ("t", serde_json::json!("blocked")),
            ],
        ))
        .expect_err("a writer cannot share an exclusive lock");
    assert_busy_naming(&err, holder);
    // A READER INSIDE A WRITE TRANSACTION on the same body — the first
    // kind of read, which takes a real read lock. Write excludes it too.
    let err = impatient
        .submit(commit(
            READ_ONE_WRITE_ANOTHER,
            &[
                ("x", serde_json::json!(x.0)),
                ("p", serde_json::json!(p.0)),
                ("t", serde_json::json!("blocked")),
            ],
        ))
        .expect_err("a write lock excludes readers too");
    assert_busy_naming(&err, holder);
    // A LOCK-FREE read transaction is not in that queue at all.
    assert_eq!(title_of(&c, &x.0), Some("the exclusive doc".into()));

    unpark(&dir, holder);
    c.submit(commit(
        SET_TITLE,
        &[
            ("x", serde_json::json!(x.0)),
            ("t", serde_json::json!("granted after release")),
        ],
    ))
    .expect("granted once the holder releases");

    // N >= 2, no foreign holder: two door writers on ONE body, real threads.
    // Exclusivity is scheduling, not refusal — both land, neither is lost.
    let env_a = commit(
        SET_TITLE,
        &[
            ("x", serde_json::json!(x.0)),
            ("t", serde_json::json!("writer a")),
        ],
    );
    let env_b = commit(
        SET_TITLE,
        &[
            ("x", serde_json::json!(x.0)),
            ("t", serde_json::json!("writer b")),
        ],
    );
    let outcomes: Vec<Result<Outcome, SpineError>> = std::thread::scope(|scope| {
        let door = &c;
        let a = scope.spawn(move || door.submit(env_a));
        let b = scope.spawn(move || door.submit(env_b));
        vec![a.join().expect("join"), b.join().expect("join")]
    });
    for out in &outcomes {
        assert!(
            matches!(out, Ok(Outcome::Committed { .. })),
            "a writer waits its turn; it is never refused: {out:?}"
        );
    }
    let alpha = Pool::open(&Pool::path_in(&dir, &PoolName::new(ALPHA))).expect("alpha");
    let rev = alpha
        .snapshot()
        .expect("snapshot")
        .node(&x)
        .expect("node")
        .expect("the doc")
        .rev;
    assert_eq!(
        rev, 4,
        "one create plus three landed writes: both concurrent writes are in the revision, so \
         neither was lost under exclusion"
    );
    assert!(spine.pending().expect("pending").is_empty());
}

// ---------------------------------------------------------------------------
// the queue-order DEPARTURE, both halves, through the door
// ---------------------------------------------------------------------------

/// The named DEPARTURE from literal arrival order:
/// every per-resource queue is ordered by the transaction's spine sequence
/// number, and a reader whose number is EARLIER than a queued writer's
/// proceeds ahead of it; the literal rule is preserved for every reader
/// arriving after the writer queued.
///
/// Both halves, at three real door calls on one body. A foreign write parks
/// the doc; then reader R1 queues, then writer W queues, then reader R2
/// queues — so `seq(R1) < seq(W) < seq(R2)` by the spine's own issue order.
/// The parking holder releases, and the completion order is R1, then W, then
/// R2: the EARLIER reader passed the queued writer, and the LATER reader
/// waited behind it. Under literal arrival order R1 would have waited behind W
/// as well, and the order would be W, R1, R2 — or nothing at all.
// RED: the grant rule's `other_seq < seq.0` weakened to `other_seq != seq.0`
// (literal arrival order) — R1 no longer passed the queued writer, the
// completion order came back W-first, and the ordering assertion failed.
#[test]
fn queue_order_through_the_door_an_earlier_reader_passes_and_a_later_reader_waits() {
    let dir = root("queue-order");
    let c = fresh_cauldron(&dir, patient());
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let x = create_doc(&c, "the contended doc");
    let p1 = create_doc(&c, "r1's own doc");
    let p2 = create_doc(&c, "r2's own doc");

    let holder = park(&dir, &one(body(&x), LockMode::Write), "queue-order holder");
    let mut known = vec![holder];

    let env_r1 = commit(
        READ_ONE_WRITE_ANOTHER,
        &[
            ("x", serde_json::json!(x.0)),
            ("p", serde_json::json!(p1.0)),
            ("t", serde_json::json!("r1 landed")),
        ],
    );
    let env_w = commit(
        SET_TITLE,
        &[
            ("x", serde_json::json!(x.0)),
            ("t", serde_json::json!("w landed")),
        ],
    );
    let env_r2 = commit(
        READ_ONE_WRITE_ANOTHER,
        &[
            ("x", serde_json::json!(x.0)),
            ("p", serde_json::json!(p2.0)),
            ("t", serde_json::json!("r2 landed")),
        ],
    );
    let (t_r1, t_w, t_r2) = std::thread::scope(|scope| {
        let door = &c;
        let r1 = scope.spawn(move || door.submit(env_r1).map(|_| Instant::now()));
        let seq_r1 = await_new_claimant(&spine, &body(&x), &mut known);

        let w = scope.spawn(move || door.submit(env_w).map(|_| Instant::now()));
        let seq_w = await_new_claimant(&spine, &body(&x), &mut known);

        let r2 = scope.spawn(move || door.submit(env_r2).map(|_| Instant::now()));
        let seq_r2 = await_new_claimant(&spine, &body(&x), &mut known);

        // The spine's own issue order is what the queue is ordered by.
        assert!(
            seq_r1 < seq_w && seq_w < seq_r2,
            "numbers are issued at open, in submission order: {seq_r1} {seq_w} {seq_r2}"
        );
        let q = locks::queue(&spine, &PoolName::new(ALPHA), &body(&x)).expect("queue");
        assert_eq!(q.len(), 4, "the holder plus three queued door calls: {q:?}");
        assert!(
            q.iter().filter(|e| e.seq != holder).all(|e| !e.granted),
            "nobody passes a GRANTED conflicting holder, whatever their number: {q:?}"
        );

        unpark(&dir, holder);
        (
            r1.join().expect("join"),
            w.join().expect("join"),
            r2.join().expect("join"),
        )
    });

    let t_r1 = t_r1.expect("the earlier reader completes");
    let t_w = t_w.expect("the writer completes");
    let t_r2 = t_r2.expect("the later reader completes");
    assert!(
        t_r1 < t_w,
        "THE DEPARTURE: the reader whose number is EARLIER than the queued writer's went first"
    );
    assert!(
        t_w < t_r2,
        "THE STRICT QUEUE ORDER, PRESERVED: the reader that arrived after the writer queued waited \
         behind it"
    );
    assert_eq!(title_of(&c, &x.0), Some("w landed".into()));
    assert_eq!(title_of(&c, &p1.0), Some("r1 landed".into()));
    assert_eq!(title_of(&c, &p2.0), Some("r2 landed".into()));
    assert!(spine.pending().expect("pending").is_empty());
}

// ---------------------------------------------------------------------------
// THE TWO-NODE CROSSED DEADLOCK WITNESS, through the door
// ---------------------------------------------------------------------------

/// The two-node crossed read-then-write case that deadlocks under
/// literal arrival order must complete under sequence order — that case is WHY
/// the departure from literal arrival order exists, and exhibiting it is the
/// honest way to show the choice.
///
/// The witness is CONSTRUCTED, not raced for (a
/// barrier race does not discriminate the two rules, because a waiter holds
/// nothing and whoever enrols first usually grants first). An older foreign
/// transaction parks BOTH bodies; statement A (reads x, writes y) and
/// statement B (reads y, writes x) both enrol behind it; the crossed state is
/// asserted ON DISK — four ungranted claims, each transaction wanting what the
/// other will hold — and only then is the parking holder released.
///
/// Under ARRIVAL order, from that state, A's read on x waits behind B's queued
/// write on x while B's read on y waits behind A's queued write on y, forever,
/// and there is no global lock to fall back on. Under SEQUENCE order every
/// wait points at an OLDER transaction, so the older takes both and the
/// younger follows.
// RED: the grant rule weakened to literal arrival order (`other_seq !=
// seq.0`) — both door calls hung to their ceiling and came back `Busy`, each
// naming the other as its blocker. That failure IS the deadlock, exhibited.
#[test]
fn the_two_node_crossed_case_completes_through_the_door_under_sequence_order() {
    let dir = root("crossed");
    let c = fresh_cauldron(&dir, patient());
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let x = create_doc(&c, "node x");
    let y = create_doc(&c, "node y");

    let holder = park(
        &dir,
        &claims(&[(body(&x), LockMode::Write), (body(&y), LockMode::Write)]),
        "crossed parking holder",
    );
    let mut on_x = vec![holder];
    let mut on_y = vec![holder];

    // A: reads x, writes y.  B: reads y, writes x.
    let env_a = commit(
        READ_ONE_WRITE_ANOTHER,
        &[
            ("x", serde_json::json!(x.0)),
            ("p", serde_json::json!(y.0)),
            ("t", serde_json::json!("a wrote y")),
        ],
    );
    let env_b = commit(
        READ_ONE_WRITE_ANOTHER,
        &[
            ("x", serde_json::json!(y.0)),
            ("p", serde_json::json!(x.0)),
            ("t", serde_json::json!("b wrote x")),
        ],
    );
    let (t_a, t_b) = std::thread::scope(|scope| {
        let door = &c;
        let a = scope.spawn(move || door.submit(env_a).map(|_| Instant::now()));
        let seq_a = await_new_claimant(&spine, &body(&x), &mut on_x);
        on_y.push(seq_a);
        let b = scope.spawn(move || door.submit(env_b).map(|_| Instant::now()));
        let seq_b = await_new_claimant(&spine, &body(&y), &mut on_y);
        on_x.push(seq_b);
        assert!(seq_a < seq_b, "A is the older transaction");

        // THE CROSSED STATE, ON DISK: both transactions enrolled on both
        // nodes, each reading one and writing the other, neither granted.
        await_claim(&spine, &body(&x), seq_b, LockMode::Write);
        await_claim(&spine, &body(&y), seq_a, LockMode::Write);
        for (node, reader, writer) in [(&x, seq_a, seq_b), (&y, seq_b, seq_a)] {
            let q = locks::queue(&spine, &PoolName::new(ALPHA), &body(node)).expect("queue");
            assert!(
                q.iter().any(|e| e.seq == reader && e.mode == LockMode::Read && !e.granted),
                "the crossed READ is enrolled and ungranted on {node:?}: {q:?}"
            );
            assert!(
                q.iter().any(|e| e.seq == writer && e.mode == LockMode::Write && !e.granted),
                "the crossed WRITE is enrolled and ungranted on {node:?}: {q:?}"
            );
        }

        unpark(&dir, holder);
        (a.join().expect("join"), b.join().expect("join"))
    });

    let t_a = t_a.expect("the OLDER transaction completes — under arrival order it would not");
    let t_b = t_b.expect("and so does the younger — no cycle can form");
    assert!(
        t_a < t_b,
        "sequence order decides it: every wait points at an older transaction"
    );
    assert_eq!(title_of(&c, &y.0), Some("a wrote y".into()));
    assert_eq!(title_of(&c, &x.0), Some("b wrote x".into()));
    assert!(spine.pending().expect("pending").is_empty());
    assert!(c.in_landing(None).expect("in landing").is_empty());
}

// ---------------------------------------------------------------------------
// restart, never an in-place upgrade, at N >= 2
// ---------------------------------------------------------------------------

/// No in-place read-to-write upgrade, ever: a transaction that
/// discovers mid-flight it must write something it only read RESTARTS with the
/// larger claim set and a fresh sequence number, and two such transactions
/// must not deadlock.
///
/// Two statements, on two pools, both blocked at their grant while the world
/// moves under them, both restarting, both completing — on real threads, at
/// the same time. For each one the restart lifecycle is asserted
/// on the record: attempt 1 recorded ABORTED and kept forever with nothing
/// landed, attempt 2 committed, and **attempt 2's sequence number strictly
/// greater than attempt 1's** — which is the whole content of "a fresh
/// sequence number", and the reason no claim set ever grows in place.
///
/// The final state also proves the restart re-matched rather than resumed:
/// each statement published the row that appeared while it was queued.
// RED: (a) the restart made to reuse the row's seq instead of appending a
// fresh row — `attempts_of` returned one row and the greater-number assertion
// failed; (b) the payload-equality test in phase 10 removed — the attempts
// proceeded on stale payloads and the late-arriving rows were never touched.
#[test]
fn two_transactions_restart_mid_flight_and_both_complete_with_fresh_sequence_numbers() {
    let dir = root("restart");
    let c = fresh_cauldron(&dir, patient());
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let doc = create_doc(&c, "first draft");
    let ticket = create_ticket(&c);

    // One foreign holder per pool, so BOTH statements park at their grant.
    let hold_doc = park(&dir, &one(body(&doc), LockMode::Write), "doc holder");
    let hold_ticket = park(
        &dir,
        &claims_in(BETA, &[(body(&ticket), LockMode::Write)]),
        "ticket holder",
    );

    let alpha_env = commit(PUBLISH_DRAFTS, &[]);
    let beta_env = commit(CLOSE_TICKETS, &[]);
    let alpha_hash = alpha_env.hash_identity().expect("hash");
    let beta_hash = beta_env.hash_identity().expect("hash");

    std::thread::scope(|scope| {
        let door = &c;
        let a = scope.spawn(move || door.submit(alpha_env));
        let b = scope.spawn(move || door.submit(beta_env));

        // Both are physically queued: their optimistic matches have happened
        // and their grants have not.
        let mut on_doc = vec![hold_doc];
        let mut on_ticket = vec![hold_ticket];
        await_new_claimant(&spine, &body(&doc), &mut on_doc);
        await_new_claimant_in(&spine, BETA, &body(&ticket), &mut on_ticket);

        // MOVE THE WORLD under both of them, through the door, in both pools.
        create_doc(door, "second draft");
        create_ticket(door);

        unpark(&dir, hold_doc);
        unpark(&dir, hold_ticket);
        a.join().expect("join").expect("the alpha statement restarts and commits");
        b.join().expect("join").expect("the beta statement restarts and commits");
    });

    for (hash, label) in [(alpha_hash, "alpha"), (beta_hash, "beta")] {
        let attempts = spine.attempts_of(&hash).expect("history");
        assert_eq!(attempts.len(), 2, "{label}: one restart means two attempts");
        assert_eq!(attempts[0].attempt, 1);
        assert!(
            !attempts[0].decided && attempts[0].cleared,
            "{label}: the abandoned attempt's row is recorded ABORTED"
        );
        assert!(
            attempts[0].parts.iter().all(|p| !p.landed),
            "{label}: an aborted attempt landed nothing"
        );
        assert_eq!(attempts[1].attempt, 2);
        assert!(attempts[1].decided && attempts[1].cleared);
        assert!(
            attempts[1].seq > attempts[0].seq,
            "{label}: the second attempt carries a FRESH sequence number ({} > {})",
            attempts[1].seq,
            attempts[0].seq
        );
        // Nothing is upgraded in place, so the abandoned number holds nothing.
        for pool in [ALPHA, BETA] {
            for resource in [body(&doc), body(&ticket)] {
                assert!(
                    locks::queue(&spine, &PoolName::new(pool), &resource)
                        .expect("queue")
                        .iter()
                        .all(|e| e.seq != attempts[0].seq),
                    "{label}: the abandoned attempt released every lock it enrolled"
                );
            }
        }
    }

    // The restarts RE-MATCHED: the rows that arrived mid-flight were touched.
    assert_eq!(
        read_rows(&c, r#"MATCH (d: fixture-alpha/doc { state: "draft" }) RETURN d.id"#).len(),
        0,
        "no draft survived the restarted bulk statement"
    );
    assert_eq!(
        read_rows(&c, r#"MATCH (t: fixture-beta/ticket { state: "open" }) RETURN t.id"#).len(),
        0,
        "no open ticket survived the restarted bulk statement"
    );
    assert!(spine.pending().expect("pending").is_empty());
}

// ---------------------------------------------------------------------------
// the scalar slice: reassignment serializes, the loser RE-EVALUATES
// ---------------------------------------------------------------------------

/// A slice constrained to at most one edge is a scalar; reassignment
/// serializes against other reassigns. **A second reassigner, once granted,
/// re-evaluates under its locks and proceeds against the current occupant or
/// refuses — it never blindly destroys an edge that is already gone.**
///
/// `current_of` is the fixture's unique slice: at most one may arrive at any author.
/// Two reassignments race for it. The serialization is structural — the claim
/// derivation escalates the scalar slice from LINK to WRITE, so the two
/// reassigners exclude each other — and the re-evaluation is asserted on the
/// LOSER'S OWN RECORD: its abandoned attempt's payload names the original
/// occupant, and its committed attempt's payload names the WINNER'S doc, which
/// is the current occupant it re-evaluated against. It never emitted a removal
/// of a gone edge in the attempt that landed.
// RED: (a) `membership_mode(true)` returning `Link` instead of `Write` — the
// two reassignments no longer serialized, both landed, and the pool's
// at-most-one occupancy check refused the second at LANDING; (b) the phase-10
// payload-equality test removed — the loser proceeded on its stale payload,
// removed an edge that was already gone and left two edges on a scalar slice.
#[test]
fn two_scalar_slice_reassignments_serialize_and_the_loser_re_evaluates() {
    let dir = root("scalar-slice");
    let c = fresh_cauldron(&dir, patient());
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let author = create_author(&c, "the author with one current doc");
    let d0 = create_doc(&c, "the original current doc");
    let d1 = create_doc(&c, "candidate one");
    let d2 = create_doc(&c, "candidate two");
    c.submit(commit(
        SET_CURRENT,
        &[
            ("a", serde_json::json!(author.0)),
            ("d", serde_json::json!(d0.0)),
        ],
    ))
    .expect("the first occupant");

    // The scalar slice, claimed by the derivation as a WRITE — that is the
    // serialization, read off the real claim set before anything races.
    let reassign_one = commit(
        REASSIGN_CURRENT,
        &[
            ("a", serde_json::json!(author.0)),
            ("new", serde_json::json!(d1.0)),
        ],
    );
    let reassign_two = commit(
        REASSIGN_CURRENT,
        &[
            ("a", serde_json::json!(author.0)),
            ("new", serde_json::json!(d2.0)),
        ],
    );
    let scalar = eslice(&author, "current_of", Direction::In);
    let derived = derive_claims(&c, &reassign_one);
    assert!(
        derived.iter().any(|cl| cl.resource == scalar && cl.mode == LockMode::Write),
        "an at-most-one slice's membership change is a slice WRITE, never link: {derived:?}"
    );
    assert!(
        !derived.iter().any(|cl| cl.mode == LockMode::Link && cl.resource == scalar),
        "link is FORBIDDEN on a scalar slice"
    );

    let holder = park(&dir, &one(scalar.clone(), LockMode::Write), "scalar holder");
    let mut known = vec![holder];
    let hash_one = reassign_one.hash_identity().expect("hash");
    let hash_two = reassign_two.hash_identity().expect("hash");

    let (seq_one, seq_two) = std::thread::scope(|scope| {
        let door = &c;
        let first = scope.spawn(move || door.submit(reassign_one));
        let seq_one = await_new_claimant(&spine, &scalar, &mut known);
        let second = scope.spawn(move || door.submit(reassign_two));
        let seq_two = await_new_claimant(&spine, &scalar, &mut known);
        assert!(seq_one < seq_two, "the first submitter is the older number");

        let q = locks::queue(&spine, &PoolName::new(ALPHA), &scalar).expect("queue");
        assert_eq!(q.len(), 3, "the holder plus two reassigners: {q:?}");
        assert!(
            q.iter().all(|e| e.mode == LockMode::Write),
            "every membership change on a scalar slice is a slice WRITE: {q:?}"
        );

        unpark(&dir, holder);
        first.join().expect("join").expect("the winner commits");
        // "proceeds against the current occupant OR REFUSES" — both are legal
        // outcomes for the loser; what is illegal is destroying a gone edge.
        let loser = second.join().expect("join");
        assert!(
            !matches!(loser, Err(SpineError::Hard { .. })),
            "a loser is a refusal or a commit, never a storage failure: {loser:?}"
        );
        (seq_one, seq_two)
    });
    let _ = (seq_one, seq_two);

    // EXACTLY ONE occupant, always — the scalar slice was never doubled and
    // never emptied.
    let occupants = read_rows(
        &c,
        r#"MATCH (d: fixture-alpha/doc) -[current_of]-> (a: fixture-alpha/author) RETURN d.id"#,
    );
    assert_eq!(occupants.len(), 1, "at most one, and never zero: {occupants:?}");
    let occupant = occupants[0]["d.id"].as_str().expect("id").to_string();
    assert_ne!(occupant, d0.0, "the reassignment happened");

    // THE RE-EVALUATION, ON THE LOSER'S OWN RECORD.
    let loser_hash = if occupant == d2.0 { hash_two } else { hash_one };
    let winner_doc = if occupant == d2.0 { &d1 } else { &d2 };
    let attempts = spine.attempts_of(&loser_hash).expect("history");
    assert_eq!(
        attempts.len(),
        2,
        "the loser's first attempt was abandoned and a second was opened"
    );
    assert!(
        attempts[1].seq > attempts[0].seq,
        "with a fresh sequence number, as a restart requires"
    );
    assert_eq!(
        removed_edge_source(&attempts[0].payload),
        Some(d0.clone()),
        "attempt one was compiled against the ORIGINAL occupant"
    );
    assert_eq!(
        removed_edge_source(&attempts[1].payload),
        Some(winner_doc.clone()),
        "and the attempt that LANDED was re-compiled against the CURRENT occupant — the loser \
         never blindly destroyed an edge that was already gone"
    );

    // No transaction is wedged: nothing decided-and-unlanded is left behind.
    assert!(c.in_landing(None).expect("in landing").is_empty());
    assert!(spine.pending().expect("pending").is_empty());
}

// ---------------------------------------------------------------------------
// mechanical writes queue and land late rather than torn
// ---------------------------------------------------------------------------

/// Mechanical writes wait their turn; they are
/// never refused. 'Cannot be refused' governs refusal (no constraint may
/// reject them), not scheduling (they queue like everyone else and land late
/// rather than torn). If they bypassed locks, a reader holding a read lock
/// could watch its node change underneath it.
///
/// Two halves. A `core-mechanical` statement at the door queues behind a
/// foreign READ on the node it writes, is not refused, and lands once the read
/// drains — and its provenance is asserted from the spine row, so the door is
/// genuinely treating a mechanical initiator like everyone else. Then the
/// MACHINE-DERIVED write: four concurrent connectors drive four commutative
/// increments of one count cell, and the cell is exactly four — late, never
/// torn, never lost.
// RED: (a) a `granted = 1` short-circuit for `core-mechanical` claimants in
// `acquire` — the mechanical write was granted alongside the reader; (b)
// `bump_cell`'s commutative UPSERT rewritten as read-modify-write in the
// process — the four concurrent increments raced and the cell came back < 4.
#[test]
fn mechanical_writes_queue_and_land_late_rather_than_torn() {
    let dir = root("mechanical");
    let c = fresh_cauldron(&dir, patient());
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let x = create_doc(&c, "watched by a reader");

    let holder = park(&dir, &one(body(&x), LockMode::Read), "the reader");
    let mut known = vec![holder];

    let env = mechanical(
        SET_TITLE,
        &[
            ("x", serde_json::json!(x.0)),
            ("t", serde_json::json!("mechanically retitled")),
        ],
    );
    let hash = env.hash_identity().expect("hash");
    std::thread::scope(|scope| {
        let door = &c;
        let m = scope.spawn(move || door.submit(env));
        let seq = await_new_claimant(&spine, &body(&x), &mut known);
        // It WAITS — a mechanical write that bypassed the lock table would
        // change this node under the reader holding it.
        std::thread::sleep(Duration::from_millis(60));
        let q = locks::queue(&spine, &PoolName::new(ALPHA), &body(&x)).expect("queue");
        assert!(
            q.iter().any(|e| e.seq == seq && !e.granted),
            "the mechanical write is queued like everyone else: {q:?}"
        );
        assert_eq!(title_of(door, &x.0), Some("watched by a reader".into()));
        unpark(&dir, holder);
        m.join()
            .expect("join")
            .expect("and it is granted, never refused");
    });
    assert_eq!(title_of(&c, &x.0), Some("mechanically retitled".into()));
    let row = spine.find_by_hash(&hash).expect("read").expect("the row");
    assert!(
        matches!(row.initiator, Initiator::CoreMechanical { .. }),
        "the row records it as mechanical: {:?}",
        row.initiator
    );

    // The machine-derived write, under contention: four commutative
    // increments of one cell from four real threads.
    let author = create_author(&c, "counted");
    let connects: Vec<Envelope> = (0..4)
        .map(|i| {
            commit(
                CONNECT,
                &[
                    ("a", serde_json::json!(author.0)),
                    ("t", serde_json::json!(format!("counted {i}"))),
                ],
            )
        })
        .collect();
    std::thread::scope(|scope| {
        let door = &c;
        let handles: Vec<_> = connects
            .into_iter()
            .map(|env| scope.spawn(move || door.submit(env)))
            .collect();
        for h in handles {
            h.join().expect("join").expect("every connect commits");
        }
    });
    let alpha = Pool::open(&Pool::path_in(&dir, &PoolName::new(ALPHA))).expect("alpha");
    assert_eq!(
        alpha
            .snapshot()
            .expect("snapshot")
            .count_cell(&author, "docs_written")
            .expect("cell"),
        Some(4),
        "four increments, derived inside four landing transactions, none lost and none torn"
    );
    assert!(spine.pending().expect("pending").is_empty());
}

// ---------------------------------------------------------------------------
// the N = 1 arm (stated for what it CAN show)
// ---------------------------------------------------------------------------

/// A SINGLE actor is never refused its own grant. That is the one contract the
/// single case can prove, and it is here for exactly that.
///
/// The statement is deliberately self-overlapping: it READS the author (a
/// matched body), WRITES the author (a SET on the same body), LINKS the
/// author's `wrote` slice (an edge-connect), writes the new doc's body and
/// links the pool's `doc` type slice. Several claims name one node, in modes
/// that would conflict between two transactions, and the door grants them all
/// at once with a tight ceiling.
// RED: (a) `acquire`'s own-rows filter (`seq != ?3`) removed AND the
// already-granted short-circuit removed — the statement blocked on itself and
// died at its own ceiling; (b) `combine_modes`' read+link escalation weakened
// to `Read` — the derivation stopped collapsing the overlap and the write
// claim assertion failed.
#[test]
fn n_equals_one_a_single_actor_is_never_refused_its_own_grant_through_the_door() {
    let dir = root("single-actor");
    let c = fresh_cauldron(&dir, cfg(400));
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let author = create_author(&c, "solo");

    let env = commit(
        CONNECT_AND_RENAME,
        &[
            ("a", serde_json::json!(author.0)),
            ("t", serde_json::json!("solo doc")),
            ("n", serde_json::json!("solo renamed")),
        ],
    );
    let derived = derive_claims(&c, &env);
    let naming_author: Vec<&Claim> = derived
        .iter()
        .filter(|cl| match &cl.resource {
            Resource::Body { node } => node == &author,
            Resource::EdgeSlice { node, .. } => node == &author,
            _ => false,
        })
        .collect();
    assert!(
        naming_author.len() >= 2,
        "the statement claims the author several ways: {naming_author:?}"
    );
    assert!(
        naming_author
            .iter()
            .any(|cl| cl.resource == body(&author) && cl.mode == LockMode::Write),
        "read and write on one body collapse to the mode satisfying both: {naming_author:?}"
    );

    // With a 400ms ceiling: if a claimant could block itself, this would die.
    c.submit(env).expect("a single actor is never blocked by itself");
    assert_eq!(docs_by(&c, "solo doc"), 1);
    assert!(
        locks::queue(&spine, &PoolName::new(ALPHA), &body(&author))
            .expect("queue")
            .is_empty(),
        "and everything released at the cleared flip"
    );
    assert!(spine.pending().expect("pending").is_empty());
}

// ---------------------------------------------------------------------------
// the statements under test
// ---------------------------------------------------------------------------

/// An edge-connect to a shared author: reads the author's body, links its
/// `wrote` slice, and never writes the endpoint (the carve-out).
const CONNECT: &str = r#"MATCH (a: fixture-alpha/author { id: $a })
   CREATE (d: fixture-alpha/doc { title: $t, state: "draft" })
   CREATE (a) -[wrote]-> (d)
   RETURN d.id"#;

/// A read of the shared author INSIDE a write transaction (the first
/// kind of read: a read lock held to the end of the commit).
const READ_THE_AUTHOR: &str = r#"MATCH (a: fixture-alpha/author { id: $a })
   MATCH (p: fixture-alpha/doc { id: $p })
   SET p.title = $t"#;

const SET_TITLE: &str = r#"MATCH (x: fixture-alpha/doc { id: $x }) SET x.title = $t"#;

/// Reads one node and writes another — the shape the crossed case is made
/// of, and the shape of a reader that holds a real read lock.
const READ_ONE_WRITE_ANOTHER: &str = r#"MATCH (x: fixture-alpha/doc { id: $x })
   MATCH (p: fixture-alpha/doc { id: $p })
   SET p.title = $t"#;

const PUBLISH_DRAFTS: &str =
    r#"MATCH (d: fixture-alpha/doc { state: "draft" }) SET d.state = "published" RETURN d.id"#;

const CLOSE_TICKETS: &str =
    r#"MATCH (t: fixture-beta/ticket { state: "open" }) SET t.state = "closed" RETURN t.id"#;

const SET_CURRENT: &str = r#"MATCH (a: fixture-alpha/author { id: $a })
   MATCH (d: fixture-alpha/doc { id: $d })
   CREATE (d) -[current_of]-> (a)"#;

/// The scalar-slice REASSIGNMENT, spelled the way the rule describes it: the
/// outgoing occupant is found BY THE SLICE, not named by the caller, so a
/// re-match under the granted locks genuinely re-evaluates who the current
/// occupant is.
const REASSIGN_CURRENT: &str = r#"MATCH (a: fixture-alpha/author { id: $a })
   MATCH (old: fixture-alpha/doc) -[current_of]-> (a)
   MATCH (new: fixture-alpha/doc { id: $new })
   REMOVE (old) -[current_of]-> (a)
   CREATE (new) -[current_of]-> (a)
   RETURN old.id AS was"#;

/// Deliberately self-overlapping, for the N = 1 arm.
const CONNECT_AND_RENAME: &str = r#"MATCH (a: fixture-alpha/author { id: $a })
   CREATE (d: fixture-alpha/doc { title: $t, state: "draft" })
   CREATE (a) -[wrote]-> (d)
   SET a.name = $n
   RETURN d.id"#;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-locks-door-{name}-{}", std::process::id()))
}

/// Generous where a grant MUST eventually happen: long enough that a loaded
/// machine never reds it, short enough that a genuine cycle fails the test
/// instead of hanging the suite.
fn patient() -> SpineConfig {
    SpineConfig {
        lock_wait_ceiling: Duration::from_secs(30),
        lock_poll_interval: Duration::from_millis(3),
        ..SpineConfig::default()
    }
}

fn cfg(ceiling_ms: u64) -> SpineConfig {
    SpineConfig {
        lock_wait_ceiling: Duration::from_millis(ceiling_ms),
        lock_poll_interval: Duration::from_millis(2),
        ..SpineConfig::default()
    }
}

fn fresh_cauldron(dir: &Path, config: SpineConfig) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(
        dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        config,
    )
    .expect("create the cauldron")
}

fn envelope_with(initiator: Initiator, statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
    let mut bindings = Bindings::new();
    for (k, v) in params {
        bindings.insert((*k).to_string(), v.clone());
    }
    Envelope {
        statement: statement.to_string(),
        bindings,
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator,
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn commit(statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
    envelope_with(
        Initiator::Agent {
            thread: "lock-behaviour".into(),
            dispatch: "lock-behaviour".into(),
        },
        statement,
        params,
    )
}

fn mechanical(statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
    envelope_with(
        Initiator::CoreMechanical {
            operation: "count-cell-maintenance".into(),
        },
        statement,
        params,
    )
}

fn body(node: &NodeId) -> Resource {
    Resource::Body { node: node.clone() }
}

fn eslice(node: &NodeId, kind: &str, direction: Direction) -> Resource {
    Resource::EdgeSlice {
        node: node.clone(),
        kind: kind.to_string(),
        direction,
    }
}

fn claims(items: &[(Resource, LockMode)]) -> ClaimSet {
    claims_in(ALPHA, items)
}

fn claims_in(pool: &str, items: &[(Resource, LockMode)]) -> ClaimSet {
    let mut set = ClaimSet::new();
    for (r, m) in items {
        set.insert(Claim {
            pool: PoolName::new(pool),
            resource: r.clone(),
            mode: *m,
        });
    }
    set
}

fn one(resource: Resource, mode: LockMode) -> ClaimSet {
    claims(&[(resource, mode)])
}

fn grammar_of(c: &Cauldron) -> impl Fn(&PoolName) -> Option<PoolGrammar> + '_ {
    move |p: &PoolName| c.grammar(p).cloned()
}

/// The claim set the DOOR would acquire for this envelope — derived through
/// the crate's own matcher, compiler and claim derivation, so these tests are
/// asserting over real claims and not over claims a test invented.
fn derive_claims(c: &Cauldron, env: &Envelope) -> ClaimSet {
    let (_, _, claims) = derive(c, env);
    claims
}

fn derive(c: &Cauldron, env: &Envelope) -> (TxHash, CompiledPayload, ClaimSet) {
    let lookup = grammar_of(c);
    let stmt = parse::parse(&env.statement).expect("parse");
    let pools: Vec<PoolName> = compile::footprint(&stmt).pools.iter().cloned().collect();
    let mut fuel = Fuel::new(c.config().fuel_ceiling);
    let matched = {
        let open = read::open_pools(c, &pools).expect("open pools");
        let snaps = read::open_snapshots(&open).expect("snapshots");
        read::run_match(&snaps, &stmt, &env.bindings, &mut fuel).expect("match")
    };
    let hash = env.hash_identity().expect("hash");
    let payload = {
        let kinds = |k: &spine::schema_def::PoolKind| -> Vec<PoolGrammar> {
            c.pools_of_kind(k)
                .into_iter()
                .filter_map(|n| c.grammar(n).cloned())
                .collect()
        };
        let mut ctx = CompileCtx {
            grammars: &lookup,
            pools_of_kind: &kinds,
            fuel: &mut fuel,
        };
        compile::compile(&stmt, &env.bindings, &matched, &hash, &mut ctx).expect("compile")
    };
    let claims = compile::claims_for(&stmt, &matched, &payload, &lookup).expect("claims");
    (hash, payload, claims)
}

/// Park a resource under a foreign transaction: a real pending row for a real
/// sequence number, and a real grant. This is how a contended state is
/// CONSTRUCTED rather than raced for.
fn park(dir: &Path, held: &ClaimSet, tag: &str) -> Seq {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let env = commit(&format!("-- parking holder: {tag}"), &[]);
    let hash = env.hash_identity().expect("hash");
    let seq = spine
        .append_pending(&env, &hash, &empty_payload())
        .expect("parking row");
    locks::acquire(&spine, seq, held, &patient()).expect("park");
    seq
}

/// Release a parked holder and close its row, so the cauldron is left with no
/// in-flight transaction the next assertion would trip over.
fn unpark(dir: &Path, seq: Seq) {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    locks::release_at_clear(&spine, seq).expect("release");
    spine
        .mark_cleared(seq, &serde_json::json!({ "parking_holder": "released" }))
        .expect("close the parking row");
}

fn empty_payload() -> CompiledPayload {
    CompiledPayload {
        parts: Vec::new(),
        minted: std::collections::BTreeMap::new(),
    }
}

/// Wait (bounded) for a claimant on `resource` whose sequence number is not
/// already known, record it, and hand it back. No sleep races: an enrolled,
/// ungranted row is the state that says "a door call asked and did not get it".
fn await_new_claimant(spine: &SpineDb, resource: &Resource, known: &mut Vec<Seq>) -> Seq {
    await_new_claimant_in(spine, ALPHA, resource, known)
}

fn await_new_claimant_in(
    spine: &SpineDb,
    pool: &str,
    resource: &Resource,
    known: &mut Vec<Seq>,
) -> Seq {
    let started = Instant::now();
    loop {
        let q = locks::queue(spine, &PoolName::new(pool), resource).expect("queue");
        if let Some(e) = q.iter().find(|e| !known.contains(&e.seq) && !e.granted) {
            known.push(e.seq);
            return e.seq;
        }
        assert!(
            started.elapsed() < Duration::from_secs(25),
            "no new door call ever queued on {pool}/{resource:?}: {q:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Wait (bounded) until a specific claim row exists, ungranted.
fn await_claim(spine: &SpineDb, resource: &Resource, seq: Seq, mode: LockMode) {
    let started = Instant::now();
    loop {
        let q = locks::queue(spine, &PoolName::new(ALPHA), resource).expect("queue");
        if q.iter().any(|e| e.seq == seq && e.mode == mode) {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(25),
            "seq {seq} never enrolled {mode:?} on {resource:?}: {q:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn assert_busy_naming(err: &SpineError, holder: Seq) {
    match err {
        SpineError::Busy { message } => assert!(
            message.contains(&format!("seq {holder}")),
            "the wait names the holder it is behind: {message}"
        ),
        other => panic!("expected a loud Busy at the ceiling, got {other:?}"),
    }
}

/// The source node of the one `RemoveEdge` delta in a payload — what a
/// reassignment says it is unhooking.
fn removed_edge_source(payload: &CompiledPayload) -> Option<NodeId> {
    payload
        .parts
        .iter()
        .flat_map(|p| p.deltas.iter())
        .find_map(|d| match d {
            Delta::RemoveEdge { src, .. } => Some(src.clone()),
            _ => None,
        })
}

fn create_author(c: &Cauldron, name: &str) -> NodeId {
    minted(
        c,
        r#"CREATE (a: fixture-alpha/author { name: $n }) RETURN a.id"#,
        &[("n", serde_json::json!(name))],
        "a.id",
    )
}

fn create_doc(c: &Cauldron, title: &str) -> NodeId {
    minted(
        c,
        r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" }) RETURN d.id"#,
        &[("t", serde_json::json!(title))],
        "d.id",
    )
}

fn create_ticket(c: &Cauldron) -> NodeId {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Tickets carry no fields, so a distinct NOTE-free statement per ticket is
    // impossible; the idempotency nonce is what keeps two creates two asks.
    let mut env = commit(
        r#"CREATE (t: fixture-beta/ticket { state: "open" }) RETURN t.id"#,
        &[],
    );
    env.idempotency_nonce = Some(format!("ticket-{n}"));
    match c.submit(env).expect("create a ticket") {
        Outcome::Committed { returned, .. } => {
            NodeId(returned.0[0]["t.id"].as_str().expect("id").to_string())
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn minted(
    c: &Cauldron,
    statement: &str,
    params: &[(&str, serde_json::Value)],
    column: &str,
) -> NodeId {
    match c.submit(commit(statement, params)).expect("create") {
        Outcome::Committed { returned, .. } => {
            NodeId(returned.0[0][column].as_str().expect("id").to_string())
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn read_rows(
    c: &Cauldron,
    statement: &str,
) -> Vec<std::collections::BTreeMap<String, serde_json::Value>> {
    c.read(&ReadRequest {
        statement: statement.to_string(),
        bindings: Bindings::new(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    })
    .expect("bulk read")
    .rows
    .0
}

fn title_of(c: &Cauldron, id: &str) -> Option<String> {
    let mut bindings = Bindings::new();
    bindings.insert("id".into(), serde_json::json!(id));
    c.read(&ReadRequest {
        statement: r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.title"#.into(),
        bindings,
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    })
    .expect("read")
    .rows
    .0
    .first()
    .and_then(|r| r["d.title"].as_str().map(str::to_string))
}

fn docs_by(c: &Cauldron, title_prefix: &str) -> usize {
    read_rows(c, r#"MATCH (d: fixture-alpha/doc) RETURN d.title"#)
        .iter()
        .filter(|r| {
            r["d.title"]
                .as_str()
                .is_some_and(|t| t.starts_with(title_prefix))
        })
        .count()
}
