//! **The validated read under contention** — with the STRICT step 4 and
//! the escalation that COMPLETES.
//!
//! The bounded escalation is the ONE path by which a reader ever enters a
//! queue, and the design says plainly what it is worth: expected cold; and
//! a cold path that has never run warm is not proven.
//!
//! So the escalation is not merely reachable here — it is **run, through the
//! real read door**, against a commit storm that genuinely invalidates every
//! cut, and it is made to WAIT in the queue behind an older transaction before
//! it completes. That last part is what "completes by the same ordering that
//! prevents deadlock" means when a reader is the one holding a sequence
//! number.
//!
//! The suite, and why each test needs the others:
//!
//! 1. the storm forces the read past its bound, the escalation runs, it queues
//!    behind an OLDER transaction on a type slice, and it completes when that
//!    transaction releases;
//! 2. (N >= 2) an OLDER SET holding a matched BODY goes first, the
//!    escalated read queues behind it on the body, a YOUNGER SET queues
//!    behind the read, and the read completes with the older value — the
//!    pre-fix build gave up as `Busy` here, because it never claimed bodies;
//! 3. (N = 1) an escalated read with no contention completes in one
//!    attempt with no restart — the control is proven not to fire;
//! 4. a match set that MOVES under the escalated read restarts with
//!    the larger claim set, once; and past `restart_bound` it gives up loudly
//!    as the named starvation residual;
//! 5. STRICT step 4: a commit on an UNRELATED pool inside the
//!    window costs zero retries; a commit on the read's own pool still costs
//!    one; a premise-only part on the read's pool costs none;
//! 6. the WARM path takes no locks, opens no row and never escalates;
//! 7. a real N >= 2 commit storm from real writer threads, where the invariant
//!    asserted is the one the whole mechanism exists for: every cut the reader
//!    RETURNS is internally consistent — and a SET storm on
//!    the read's own pool never makes it give up.
//!
//! `door_smoke.rs` proves the slice-only escalation by driving
//! `read::run_validated` directly with an instrumented closure. This suite
//! comes at the same clause from the other side — through `Cauldron::read`
//! wherever it can, and through `read::run_validated_claiming` where the
//! test must act INSIDE an attempt.
//!
//! Every checker here was forced RED at least once; the poison is named in a
//! `// RED:` comment above each test.

use spine::config::SpineConfig;
use spine::delta::{CompiledPart, CompiledPayload, Delta};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::locks::{self, Claim, ClaimSet, LockMode, Resource};
use spine::pool::{NodeId, Pool};
use spine::read;
use spine::schema_def::{fixtures, PoolName};
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use spine::{Cauldron, Outcome, ReadOutcome, ReadRequest};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

test_scratch::install!();

const ALPHA: &str = "fixture-alpha";
const BETA: &str = "fixture-beta";
/// A third pool, UNRELATED to the cross-pool read: what a storm on "other
/// pools" lands on (strict step 4).
const GAMMA: &str = "fixture-gamma";

/// A cross-pool read: its footprint names both pools, so it is a VALIDATED
/// snapshot read and not the single-pool exemption.
const CROSS_POOL_READ: &str = r#"MATCH (t: fixture-beta/ticket) -[catalogues]-> (d: fixture-alpha/doc)
   RETURN d.id AS doc, d.state AS state, d.title AS title"#;

fn cross_pool_read() -> ReadRequest {
    ReadRequest {
        statement: CROSS_POOL_READ.to_string(),
        bindings: Bindings::new(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    }
}

// ---------------------------------------------------------------------------
// the storm, the bound, and the escalation RUN WARM
// ---------------------------------------------------------------------------

/// The rule: a read transaction invalidated past a bounded
/// number of retries (a commit storm on its pools) escalates to taking real
/// read locks with a spine sequence number and completes by the same ordering
/// that prevents deadlock.
///
/// The storm is a transaction left DECIDED and un-landed across both of the
/// read's pools: while it stands, step 3's landing-state check invalidates
/// every cut, so the retries are forced rather than hoped for. The read
/// therefore reaches `retry_bound`, escalates, appends its own spine row, and
/// asks for real read locks — **and it is made to queue there**, because an
/// OLDER foreign transaction is holding a write on one of the type slices the
/// read's claim footprint names. It completes only when that older transaction
/// releases, which is the ordering claim, exhibited: a reader in the queue
/// waits on an older number, exactly like a writer.
// RED: (a) `run_validated`'s escalation replaced by a return of the last
// lock-free value — `escalated` came back false and the reader never appeared
// in the lock queue; (b) `window_is_consistent`'s `in_landing` half deleted —
// the first attempt certified a cut over a half-landed commit and the read
// never retried at all; (c) `escalated` made to skip `locks::acquire` — the
// reader never queued behind the older holder and the ungranted assertion
// failed. (d) the static slice claims dropped from the complete
// claim set — the reader never queued on the doc slice at all.
#[test]
fn a_commit_storm_forces_the_read_past_its_bound_and_the_escalation_runs_and_queues() {
    let dir = root("escalation");
    let config = SpineConfig {
        retry_bound: 2,
        lock_poll_interval: Duration::from_millis(2),
        lock_wait_ceiling: Duration::from_secs(30),
        ..SpineConfig::default()
    };
    let c = fresh_cauldron(&dir, config.clone());
    let doc = seed_catalogued(&c);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");

    // An OLDER transaction holds a write on the alpha `doc` type slice — one
    // of the two resources the read's claim footprint names.
    let doc_slice = Resource::TypeSlice {
        node_type: "doc".to_string(),
    };
    let older = park(&dir, &one(ALPHA, doc_slice.clone(), LockMode::Write), "older");

    // THE STORM: decided, un-landed, over BOTH of the read's pools.
    let stuck = stall_a_cross_pool_commit(&dir);
    assert_eq!(c.in_landing(None).expect("in landing").len(), 1);

    let out: Result<ReadOutcome, SpineError> = std::thread::scope(|scope| {
        let door = &c;
        let reader = scope.spawn(move || door.read(&cross_pool_read()));

        // The read has run out of retries, escalated, taken a sequence number
        // and asked for real read locks — and is queued behind the older
        // transaction. This IS the cold path, running.
        let reader_seq = await_new_claimant(&spine, ALPHA, &doc_slice, &[older]);
        assert!(
            reader_seq > older,
            "the escalated reader's number is younger than the holder it waits on ({reader_seq} \
             > {older}) — every wait points at an older transaction"
        );
        std::thread::sleep(Duration::from_millis(60));
        let q = locks::queue(&spine, &PoolName::new(ALPHA), &doc_slice).expect("queue");
        assert!(
            q.iter().any(|e| e.seq == reader_seq && e.mode == LockMode::Read && !e.granted),
            "a reader that escalated takes REAL READ LOCKS and waits its turn: {q:?}"
        );
        // Its row is a real spine row, at that number, and it is in flight.
        let row = spine.row_at(reader_seq).expect("the escalated reader's row");
        assert!(
            matches!(row.initiator, Initiator::CoreMechanical { .. }),
            "the machinery escalated, not an agent: {:?}",
            row.initiator
        );
        assert!(row.parts.is_empty(), "a reader has no parts and lands nothing");
        assert!(row.touched.is_empty(), "and touches nothing");

        // Call the storm off, then let the older transaction go.
        finish(&dir, stuck);
        unpark(&dir, older);

        (reader.join().expect("join"), reader_seq)
    })
    .0;

    let out = out.expect("the escalated read completes");
    assert!(
        out.escalated,
        "THE COLD PATH RAN: the read completed under real read locks"
    );
    assert!(
        out.retries >= config.retry_bound,
        "and it only escalated after the bound: {} retries against a bound of {}",
        out.retries,
        config.retry_bound
    );
    // The answer is the consistent cut, not a mixed one.
    assert_eq!(out.rows.0.len(), 1);
    assert_eq!(out.rows.0[0]["doc"], serde_json::json!(doc.0));
    assert_eq!(out.rows.0[0]["state"], serde_json::json!("published"));

    // The reader left NOTHING behind: no lock (slice OR body), no in-flight
    // row, and it never moved the committed-commit counter (an escalated
    // read must not invalidate anybody else's read).
    assert!(spine.pending().expect("pending").is_empty());
    assert!(c.in_landing(None).expect("in landing").is_empty());
    assert_every_lock_released(&spine, &[&doc]);
}

// ---------------------------------------------------------------------------
// the escalation COMPLETES: N >= 2 SETs around the reader
// ---------------------------------------------------------------------------

/// The bounded escalation completes by the same ordering that
/// prevents deadlock. The pre-fix build could not deliver that under a SET
/// storm on the read's own pool, because its real read locks were type-slice
/// grain and a SET on a matched BODY never conflicted with them; it validated
/// under the locks instead, and gave up as `Busy`. The repair is the write
/// path's match-then-claim-bodies restart shape on the read side, and this is
/// its N >= 2 witness, through the real read door, deterministic:
///
/// - an OLDER SET on the matched doc's body is DECIDED, LANDED and HELD (its
///   locks held, its row un-cleared — the state a commit is in between its
///   last landing and its cleared flip);
/// - the reader is invalidated by it (landing-state check), escalates, and
///   QUEUES on the body behind the older SET — the older SET goes first;
/// - a YOUNGER SET on the same body arrives through the write door and queues
///   BEHIND the reader's body claim;
/// - the older SET clears; the reader is granted ahead of the younger writer
///   (an earlier number passes a queued later writer), completes with the
///   OLDER value, and only then does the younger SET land.
///
/// The pre-fix build never queued on the body: it returned `Busy` while the
/// older SET was held. This test panics the moment the reader finishes before
/// it has queued, which is exactly that failure, made fast.
// RED: (a) the body claims dropped from the escalation's complete claim set
// (the pre-fix shape) — the reader gave up as `Busy` instead of queueing on
// the body, and "finished before it queued" fired; (b) the younger SET's
// queue entry checked with the grant rule inverted — n/a here; instead (b')
// `finish_held` called BEFORE the younger SET was observed queued — the
// ordering assertion (younger seq > reader seq, ungranted) could not be made
// and the test refused itself; (c) the reader's value asserted as "younger" —
// red by construction, confirming the read returns the OLDER value.
#[test]
fn an_older_set_goes_first_and_a_younger_set_waits_behind_the_escalated_reads_body_claims() {
    let dir = root("ordering");
    let config = SpineConfig {
        retry_bound: 1,
        lock_poll_interval: Duration::from_millis(1),
        lock_wait_ceiling: Duration::from_secs(30),
        ..SpineConfig::default()
    };
    let c = fresh_cauldron(&dir, config.clone());
    let doc = seed_catalogued(&c);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let body = Resource::Body { node: doc.clone() };

    // THE OLDER SET: landed, held (decided, un-cleared, write on the body).
    let older = hold_a_landed_set(&dir, &doc, "older", &config);
    assert_eq!(
        c.in_landing(Some(&PoolName::new(ALPHA))).expect("in landing").len(),
        1,
        "the held SET is in landing state and touches alpha"
    );

    let (out, younger) = std::thread::scope(|scope| {
        let door = &c;
        let reader = scope.spawn(move || door.read(&cross_pool_read()));

        // The reader escalates and QUEUES ON THE BODY behind the older SET.
        let reader_seq = await_body_claimant_or_finished(&spine, &body, older, &reader);
        assert!(reader_seq > older, "the reader waits on an OLDER number");

        // THE YOUNGER SET, through the real write door — it queues behind
        // the reader's body claim.
        let id = doc.0.clone();
        let younger = scope.spawn(move || {
            door.submit(commit(
                r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = "younger""#,
                &[("id", serde_json::json!(id))],
            ))
        });
        let younger_seq = {
            let started = Instant::now();
            loop {
                let q = locks::queue(&spine, &PoolName::new(ALPHA), &body).expect("queue");
                if let Some(e) = q.iter().find(|e| {
                    e.seq > reader_seq && e.mode == LockMode::Write && !e.granted
                }) {
                    break e.seq;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(20),
                    "the younger SET never queued on the body: {q:?}"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        // The queue on the body, in sequence order: older (granted write),
        // reader (queued read), younger (queued write).
        let q = locks::queue(&spine, &PoolName::new(ALPHA), &body).expect("queue");
        let shape: Vec<(Seq, LockMode, bool)> =
            q.iter().map(|e| (e.seq, e.mode, e.granted)).collect();
        assert_eq!(
            shape,
            vec![
                (older, LockMode::Write, true),
                (reader_seq, LockMode::Read, false),
                (younger_seq, LockMode::Write, false),
            ],
            "the body's queue is ordered by sequence number, oldest first"
        );
        assert!(
            !reader.is_finished(),
            "the reader is still waiting behind the older SET"
        );

        // The older SET clears and releases. The reader goes next — ahead of
        // the younger writer — and the younger SET lands after it.
        finish_held(&dir, older);
        let out = reader.join().expect("join the reader");
        let younger = younger.join().expect("join the younger SET");
        (out, younger)
    });

    let out = out.expect("THE ESCALATED READ COMPLETES under a SET on its own matched body");
    assert!(out.escalated, "it completed under real read locks");
    assert_eq!(
        out.restarts, 0,
        "the match set never moved (SETs change fields, not membership): no restart"
    );
    assert_eq!(out.rows.0.len(), 1);
    assert_eq!(
        out.rows.0[0]["title"],
        serde_json::json!("older"),
        "the reader saw the OLDER SET's value: the older SET went first and the younger \
         waited behind the reader's body claim"
    );
    assert!(
        matches!(younger, Ok(Outcome::Committed { .. })),
        "and the younger SET landed after the reader released: {younger:?}"
    );
    let after = c.read(&cross_pool_read()).expect("a quiet read afterwards");
    assert_eq!(after.rows.0[0]["title"], serde_json::json!("younger"));

    assert!(spine.pending().expect("pending").is_empty());
    assert!(c.in_landing(None).expect("in landing").is_empty());
    assert_every_lock_released(&spine, &[&doc]);
}

/// THE N = 1 ARM of the same control (a control proven
/// at N >= 2 must also be proven not to fire at N = 1). An escalated read
/// with NO contention on its bodies completes in ONE attempt with NO
/// restart, opening exactly ONE escalation row. The escalation is forced by
/// a decided, un-landed storm that holds no locks (the fixture of test 1) —
/// which also exhibits, on purpose, that under complete claims the read
/// trusts its locks and the re-match rather than the counter: a landing that
/// conflicts with none of its claims cannot change what it returns. In the
/// real pipeline a decided transaction always holds its locks until the
/// cleared flip, so a landing that COULD change the read's answer is one the
/// read waits behind (test 2).
// RED: (a) the body-set comparison made to answer "moved" unconditionally —
// the read restarted to its bound and gave up as `Busy`; (b) the retries
// counter carried into `restarts` — `restarts == 0` failed; (c) a second
// escalation row appended per attempt — the "exactly one row" count failed.
#[test]
fn an_escalated_read_with_no_contention_completes_in_one_attempt_with_no_restart() {
    let dir = root("n-equals-one");
    let c = fresh_cauldron(
        &dir,
        SpineConfig {
            retry_bound: 0,
            lock_poll_interval: Duration::from_millis(1),
            lock_wait_ceiling: Duration::from_secs(20),
            ..SpineConfig::default()
        },
    );
    let doc = seed_catalogued(&c);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let escalations_before = escalation_rows(&dir);

    let stuck = stall_a_cross_pool_commit(&dir);
    let out = c
        .read(&cross_pool_read())
        .expect("an escalated read with nothing in its way completes");
    assert!(out.escalated, "the lock-free attempt was invalidated and it escalated");
    assert_eq!(out.retries, 0, "retry_bound 0: it escalated on the first invalidation");
    assert_eq!(out.restarts, 0, "ONE attempt under complete claims, and it stood");
    assert_eq!(out.rows.0.len(), 1);
    assert_eq!(out.rows.0[0]["doc"], serde_json::json!(doc.0));
    assert_eq!(
        escalation_rows(&dir),
        escalations_before + 1,
        "exactly one escalation row was appended"
    );

    finish(&dir, stuck);
    assert!(spine.pending().expect("pending").is_empty());
    assert_every_lock_released(&spine, &[&doc]);
}

// ---------------------------------------------------------------------------
// the restart shape: the match set moves, the claim set grows
// ---------------------------------------------------------------------------

/// The read's population, as the driver test below sees it: every published
/// doc in alpha (by pool + type + state, the staged match form) plus the
/// ticket population in beta — a cross-pool footprint, so it validates.
fn published_docs_and_tickets(snaps: &read::Snapshots<'_>) -> Result<Vec<NodeId>, SpineError> {
    let docs = snaps
        .of(&PoolName::new(ALPHA))?
        .nodes_of("doc", Some("published"))?;
    let _tickets = snaps.of(&PoolName::new(BETA))?.nodes_of("ticket", None)?;
    Ok(docs.into_iter().map(|n| n.id).collect())
}

fn body_claims_of(ids: &Vec<NodeId>) -> ClaimSet {
    let mut set = ClaimSet::new();
    for id in ids {
        set.insert(Claim {
            pool: PoolName::new(ALPHA),
            resource: Resource::Body { node: id.clone() },
            mode: LockMode::Read,
        });
    }
    set
}

fn slice_claims() -> ClaimSet {
    let mut set = ClaimSet::new();
    for (pool, ty) in [(ALPHA, "doc"), (BETA, "ticket")] {
        set.insert(Claim {
            pool: PoolName::new(pool),
            resource: Resource::TypeSlice {
                node_type: ty.to_string(),
            },
            mode: LockMode::Read,
        });
    }
    set
}

/// Claim completeness, on the read side: claims are complete at
/// open, and a read that discovers under its locks that its match set has
/// GROWN restarts with the larger claim set and a fresh sequence number —
/// never an in-place upgrade. Driven through `run_validated_claiming` because
/// the membership change must land INSIDE the last lock-free attempt, so the
/// first complete claim set (seeded from that attempt's match) is stale by
/// exactly one node: a draft doc moved to `published` by a SET that conflicts
/// with none of the reader's claims (its body was never matched, and a SET
/// takes no slice lock). The escalation's first attempt sees the larger set,
/// abandons itself, and the second attempt — claiming the new body too —
/// stands.
// RED: (a) the restart replaced by returning the first complete-claims value
// — `restarts` came back 0 and the returned population lacked the moved doc;
// (b) the seed never updated between restarts (`seed = found` dropped) — the
// read restarted to its bound and gave up as `Busy`.
// RED: `stands = true` (the
// re-match equality dropped) — THIS test red (2 calls, 3 required; `restarts`
// 0) and the residual test below red (`Ok` where `Busy` is owed); the set-storm
// stress stayed GREEN under the same mutation, so it cannot see this clause.
#[test]
fn a_match_set_that_moves_under_the_escalated_read_restarts_once_with_the_larger_claim_set() {
    let dir = root("restart");
    let config = SpineConfig {
        retry_bound: 0,
        restart_bound: 3,
        escalation_threshold: 3,
        lock_poll_interval: Duration::from_millis(1),
        lock_wait_ceiling: Duration::from_secs(20),
        ..SpineConfig::default()
    };
    let c = fresh_cauldron(&dir, config.clone());
    let published = seed_catalogued(&c);
    let draft = create_doc(&c, "still a draft");
    let stuck = stall_a_cross_pool_commit(&dir);

    let pools = vec![PoolName::new(ALPHA), PoolName::new(BETA)];
    let mut calls = 0usize;
    let result = read::run_validated_claiming(
        &c,
        &pools,
        &config,
        &slice_claims(),
        &body_claims_of,
        |snaps| {
            calls += 1;
            let seen = published_docs_and_tickets(snaps)?;
            if calls == 1 {
                // Inside the ONLY lock-free attempt (retry_bound 0), after
                // its snapshots are pinned: the population moves.
                c.submit(commit(
                    r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.state = "published""#,
                    &[("id", serde_json::json!(draft.0))],
                ))
                .expect("the state move lands: it conflicts with nothing the reader holds");
            }
            Ok(seen)
        },
    )
    .expect("the read completes after one restart");

    assert_eq!(
        calls, 3,
        "one lock-free attempt (invalidated by the storm), one complete-claims attempt that saw \
         the moved doc and restarted, one that stood"
    );
    assert_eq!(result.retries, 0);
    assert_eq!(result.restarts, 1, "exactly one restart, with the larger claim set");
    assert!(matches!(result.escalation, read::Escalation::RealReadLocks { .. }));
    let mut ids = result.value.clone();
    ids.sort();
    let mut want = vec![published.clone(), draft.clone()];
    want.sort();
    assert_eq!(ids, want, "the cut that stands includes the moved doc");

    finish(&dir, stuck);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    assert!(spine.pending().expect("pending").is_empty());
    assert_every_lock_released(&spine, &[&published, &draft]);
}

/// The restart is BOUNDED, and the give-up is the named starvation
/// residual — loud, as `Busy`, naming the bound — not a stale answer and not
/// an unbounded loop. `restart_bound: 0` with one membership move is the
/// smallest case that exhausts it.
// RED: the `restarts > config.restart_bound` check removed — the read
// restarted once more and came back `Ok`, where the bound says `Busy`.
#[test]
fn past_restart_bound_the_escalated_read_gives_up_loudly_as_the_starvation_residual() {
    let dir = root("restart-bound");
    let config = SpineConfig {
        retry_bound: 0,
        restart_bound: 0,
        escalation_threshold: 0,
        lock_poll_interval: Duration::from_millis(1),
        lock_wait_ceiling: Duration::from_secs(20),
        ..SpineConfig::default()
    };
    let c = fresh_cauldron(&dir, config.clone());
    seed_catalogued(&c);
    let draft = create_doc(&c, "still a draft");
    let stuck = stall_a_cross_pool_commit(&dir);

    let pools = vec![PoolName::new(ALPHA), PoolName::new(BETA)];
    let mut calls = 0usize;
    let started = Instant::now();
    let result = read::run_validated_claiming(
        &c,
        &pools,
        &config,
        &slice_claims(),
        &body_claims_of,
        |snaps| {
            calls += 1;
            let seen = published_docs_and_tickets(snaps)?;
            if calls == 1 {
                c.submit(commit(
                    r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.state = "published""#,
                    &[("id", serde_json::json!(draft.0))],
                ))
                .expect("the state move lands");
            }
            Ok(seen)
        },
    );
    match result {
        Err(SpineError::Busy { message }) => {
            assert!(
                message.contains("starvation residual") && message.contains("restart_bound 0"),
                "the give-up names the residual and the bound: {message}"
            );
        }
        other => panic!("past restart_bound the read must give up loudly as Busy: {other:?}"),
    }
    assert_eq!(calls, 2, "one lock-free attempt, one complete-claims attempt, then the bound");
    assert!(started.elapsed() < Duration::from_secs(15), "and it is BOUNDED");

    finish(&dir, stuck);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    assert!(
        spine.pending().expect("pending").is_empty(),
        "an abandoned reader dams nothing and leaves no row in flight"
    );
    assert_every_lock_released(&spine, &[&draft]);
}

// ---------------------------------------------------------------------------
// STRICT step 4: only a commit that TOUCHES the read's pools counts
// ---------------------------------------------------------------------------

/// The pre-fix arm, inverted: a commit on an UNRELATED pool landing inside
/// the read's window used to force a retry (any counter movement
/// invalidated). With the third exposure, step 4 is read as written — "no
/// commit in the window touched YOUR pools" — and the retry count is ZERO.
/// The commit lands inside the window deterministically: from inside the
/// query closure, after the snapshots are pinned and before C-end is read.
/// The counter is asserted to have MOVED inside the window, so the zero is
/// discriminating: the old conservative reading would have retried here.
// RED: (a) the pool filter dropped from `commits_touching` (every commit in
// the window returned) — one retry came back; (b) `window_is_consistent`
// comparing `counter != c_start` again (the pre-fix reading) — one retry.
#[test]
fn a_commit_on_an_unrelated_pool_inside_the_window_costs_zero_retries() {
    let dir = root("unrelated");
    let config = SpineConfig::default();
    let c = fresh_cauldron(&dir, config.clone());
    seed_catalogued(&c);
    let pools = vec![PoolName::new(ALPHA), PoolName::new(BETA)];

    let mut moved: Option<(u64, u64)> = None;
    let result = read::run_validated(&c, &pools, &config, &slice_claims(), |snaps| {
        let seen = published_docs_and_tickets(snaps)?;
        if moved.is_none() {
            let before = c.committed_commit_counter()?;
            c.submit(commit(
                r#"CREATE (i: fixture-gamma/issue { title: "noise", state: "open" })"#,
                &[],
            ))?;
            moved = Some((before, c.committed_commit_counter()?));
        }
        Ok(seen)
    })
    .expect("the read completes");
    let (before, after) = moved.expect("the closure ran");
    assert_eq!(after, before + 1, "the counter MOVED inside the read's window");
    assert_eq!(
        result.retries, 0,
        "and a commit on an unrelated pool costs the read NOTHING (step 4, strict)"
    );
    assert_eq!(result.escalation, read::Escalation::None);
    assert!(
        c.commits_touching(&pools, before, after)
            .expect("third exposure")
            .is_empty(),
        "the third exposure names no commit touching the read's pools in that window"
    );
    assert_eq!(
        c.commits_touching(&[PoolName::new(GAMMA)], before, after)
            .expect("third exposure")
            .len(),
        1,
        "and exactly the one commit touching gamma"
    );
}

/// The other arm, so the zero above is not "validation switched off": a
/// commit that DOES touch one of the read's pools, landing in the same
/// place, still costs one retry.
// RED: `window_is_consistent`'s commit half (`touching.is_empty()`) removed —
// zero retries, and door_smoke's escalation test counted four calls, not five.
#[test]
fn a_commit_on_the_reads_own_pool_inside_the_window_still_invalidates() {
    let dir = root("own-pool");
    let config = SpineConfig::default();
    let c = fresh_cauldron(&dir, config.clone());
    seed_catalogued(&c);
    let pools = vec![PoolName::new(ALPHA), PoolName::new(BETA)];

    let mut moved: Option<(u64, u64)> = None;
    let result = read::run_validated(&c, &pools, &config, &slice_claims(), |snaps| {
        let seen = published_docs_and_tickets(snaps)?;
        if moved.is_none() {
            let before = c.committed_commit_counter()?;
            c.submit(commit(
                r#"CREATE (d: fixture-alpha/doc { title: "noise", state: "draft" })"#,
                &[],
            ))?;
            moved = Some((before, c.committed_commit_counter()?));
        }
        Ok(seen)
    })
    .expect("the read completes");
    let (before, after) = moved.expect("the closure ran");
    assert_eq!(result.retries, 1, "one discarded attempt: the commit touched alpha");
    assert_eq!(result.escalation, read::Escalation::None);
    let touching = c.commits_touching(&pools, before, after).expect("third exposure");
    assert_eq!(touching.len(), 1);
    assert_eq!(touching[0].committed_at, after);
}

/// "Touched" is delta-bearing (by design): a statement
/// whose beta part is PREMISE-ONLY (a satisfied count expectation over
/// beta's tickets; every delta in alpha) lands in the window of a read over
/// beta and gamma and costs it nothing — while the row still records beta
/// as a part with its validated-under stamp.
// RED: `touched_pools_of` made to include zero-delta parts — one retry.
#[test]
fn a_premise_only_part_on_the_reads_pool_does_not_invalidate_it() {
    let dir = root("premise-only");
    let config = SpineConfig::default();
    let c = fresh_cauldron(&dir, config.clone());
    seed_catalogued(&c);
    let pools = vec![PoolName::new(BETA), PoolName::new(GAMMA)];
    let claims = {
        let mut set = ClaimSet::new();
        for (pool, ty) in [(BETA, "ticket"), (GAMMA, "issue")] {
            set.insert(Claim {
                pool: PoolName::new(pool),
                resource: Resource::TypeSlice {
                    node_type: ty.to_string(),
                },
                mode: LockMode::Read,
            });
        }
        set
    };

    let mut landed: Option<spine::envelope::TxHash> = None;
    let result = read::run_validated(&c, &pools, &config, &claims, |snaps| {
        let tickets = snaps.of(&PoolName::new(BETA))?.nodes_of("ticket", None)?.len();
        let issues = snaps.of(&PoolName::new(GAMMA))?.nodes_of("issue", None)?.len();
        if landed.is_none() {
            match c.submit(commit(
                r#"MATCH (t: fixture-beta/ticket)
                   EXPECT COUNT(t) = 1
                   CREATE (d: fixture-alpha/doc { title: "counted", state: "draft" })"#,
                &[],
            ))? {
                Outcome::Committed { tx, .. } => landed = Some(tx),
                other => panic!("expected Committed, got {other:?}"),
            }
        }
        Ok((tickets, issues))
    })
    .expect("the read completes");
    assert_eq!(result.retries, 0, "a premise-only part on beta touched nothing a reader could straddle");
    assert_eq!(result.value, (1, 0));

    let tx = landed.expect("the statement landed");
    let recorded = c.touched_pools(&tx).expect("recorded parts");
    assert_eq!(
        recorded,
        vec![PoolName::new(ALPHA), PoolName::new(BETA)],
        "the row is still self-describing: beta IS a recorded part, with its stamp"
    );
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let row = spine.find_by_hash(&tx).expect("row").expect("exists");
    assert_eq!(row.touched, vec![PoolName::new(ALPHA)], "but only alpha is TOUCHED");
    assert!(row.parts.iter().any(|p| p.pool.as_str() == BETA && p.landed));
}

// ---------------------------------------------------------------------------
// the warm path, and the stress
// ---------------------------------------------------------------------------

/// The escalation is "expected cold", and this is what makes the
/// tests above mean something: with no storm, a cross-pool read holds nothing,
/// blocks nobody, opens no spine row, retries zero times and never escalates.
/// A build in which every read escalated would satisfy the escalation tests
/// and violate the clause the escalation exists inside.
// RED: `window_is_consistent` made to answer `false` unconditionally — the
// warm read escalated, opened a row, and the retries assertion failed.
#[test]
fn the_warm_path_takes_no_locks_opens_no_row_and_never_escalates() {
    let dir = root("warm");
    let c = fresh_cauldron(&dir, SpineConfig::default());
    let doc = seed_catalogued(&c);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let counter = c.committed_commit_counter().expect("counter");

    for _ in 0..5 {
        let out = c
            .read(&cross_pool_read())
            .expect("a lock-free read on a quiet cauldron");
        assert_eq!(out.retries, 0, "nothing invalidated it");
        assert_eq!(out.restarts, 0);
        assert!(!out.escalated, "so it never entered a queue");
        assert_eq!(out.rows.0.len(), 1);
    }
    assert!(
        spine.pending().expect("pending").is_empty(),
        "a warm read lands no row"
    );
    assert_eq!(
        c.committed_commit_counter().expect("counter"),
        counter,
        "and moves no counter"
    );
    assert_every_lock_released(&spine, &[&doc]);
}

/// A REAL commit storm, at N >= 2 real writer threads, with a reader looping
/// against it. This is a STRESS, and it is labelled one: what it asserts is
/// not a retry count (which no honest test can pin without controlling time)
/// but **the invariant the whole mechanism exists for** — every cut the reader
/// RETURNS is internally consistent. Under a storm of cross-pool commits, each
/// of which publishes a doc in alpha and mints its ticket in beta, a torn cut
/// is a returned row whose ticket exists while its doc is still a draft. The
/// reader may retry, escalate or (past `restart_bound`, the starvation
/// residual) give up loudly; what it may never do is answer with a state that
/// never existed.
// RED: `window_is_consistent` made to answer `true` unconditionally — torn
// cuts came back within a handful of reads and the invariant failed.
#[test]
fn a_real_commit_storm_at_several_writers_never_yields_a_torn_cut() {
    let dir = root("storm");
    let c = fresh_cauldron(
        &dir,
        SpineConfig {
            retry_bound: 4,
            lock_poll_interval: Duration::from_millis(1),
            lock_wait_ceiling: Duration::from_secs(20),
            ..SpineConfig::default()
        },
    );
    // Enough drafts that three writers each have their own to publish.
    let rounds = 8usize;
    let writers = 3usize;
    let mut drafts = Vec::new();
    for w in 0..writers {
        for r in 0..rounds {
            drafts.push(create_doc(&c, &format!("draft {w}-{r}")));
        }
    }

    let reads = AtomicUsize::new(0);
    let retried = AtomicUsize::new(0);
    let escalated = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);

    std::thread::scope(|scope| {
        let door = &c;
        for w in 0..writers {
            let mine: Vec<NodeId> = drafts[w * rounds..(w + 1) * rounds].to_vec();
            let done = &done;
            scope.spawn(move || {
                for id in mine {
                    // One statement, two pools: publish the doc in alpha and
                    // mint its catalogue ticket in beta.
                    let _ = door.submit(commit(
                        r#"MATCH (d: fixture-alpha/doc { id: $id })
                           CREATE (t: fixture-beta/ticket { state: "open" })
                           CREATE (t) -[catalogues]-> (d)
                           SET d.state = "published""#,
                        &[("id", serde_json::json!(id.0))],
                    ));
                }
                done.fetch_add(1, Ordering::SeqCst);
            });
        }
        let reads = &reads;
        let retried = &retried;
        let escalated = &escalated;
        let done_ref = &done;
        scope.spawn(move || {
            while done_ref.load(Ordering::SeqCst) < writers {
                match door.read(&cross_pool_read()) {
                    Ok(out) => {
                        reads.fetch_add(1, Ordering::SeqCst);
                        if out.retries > 0 {
                            retried.fetch_add(1, Ordering::SeqCst);
                        }
                        if out.escalated {
                            escalated.fetch_add(1, Ordering::SeqCst);
                        }
                        // THE INVARIANT: a ticket exists in beta only for a
                        // doc that is published in alpha. A cut taken between
                        // the two landings would show the ticket while the doc
                        // was still a draft.
                        for row in &out.rows.0 {
                            assert_eq!(
                                row["state"],
                                serde_json::json!("published"),
                                "a TORN CUT: a ticket in beta whose doc in alpha is not \
                                 published — the combined state never existed"
                            );
                        }
                    }
                    // Loud, bounded failure is a legal outcome under a storm;
                    // a stale answer is not.
                    Err(SpineError::Busy { .. }) => {}
                    Err(other) => panic!("a read under a storm failed unexpectedly: {other:?}"),
                }
            }
        });
    });

    assert!(
        reads.load(Ordering::SeqCst) > 0,
        "the reader must have completed at least one read during the storm"
    );
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    assert!(
        spine.pending().expect("pending").is_empty(),
        "every writer and every reader left the cauldron clean"
    );
    // Reported, never asserted (a stress cannot pin these without controlling
    // time); run with --nocapture to see them.
    println!(
        "storm: {} consistent reads, {} of them retried, {} escalated",
        reads.load(Ordering::SeqCst),
        retried.load(Ordering::SeqCst),
        escalated.load(Ordering::SeqCst)
    );
}

/// The stress: a SET storm on the read's OWN pool — N >= 2 real
/// writer threads SETting the matched docs' titles over and over, through
/// the door, until the reader has completed an ESCALATED read. What it
/// asserts: the reader never gives up (`Busy` never comes back — the storm
/// changes fields, never membership, so no restart is ever needed and the
/// only give-up left would be the pre-fix one), every returned cut is
/// consistent, and at least one read completed escalated (the storm is on
/// the read's pool, so the lock-free path IS invalidated). Counts reported,
/// not asserted. Labelled a stress: the deterministic witness is test 2.
// RED (re-forced on a scratch copy): the read door
// routed through the PRE-FIX escalation (`drive` given no body derivation,
// so the slice-only shape validated under locks) — `Busy` after "2
// invalidated attempts, the last 1 of them under real read locks". NOT red
// under "the body claims dropped from the complete claim set": under
// complete-but-body-less claims no counter check runs, so nothing gives up
// and this stress cannot tell — the deterministic queue test
// (`an_older_set_goes_first_...`) is the one that reds under that mutation,
// on the lock rows. That is the division of labour between the two.
#[test]
fn a_set_storm_on_the_reads_own_pool_never_makes_the_escalated_read_give_up() {
    let dir = root("set-storm");
    let c = fresh_cauldron(
        &dir,
        SpineConfig {
            retry_bound: 1,
            lock_poll_interval: Duration::from_millis(1),
            lock_wait_ceiling: Duration::from_secs(20),
            ..SpineConfig::default()
        },
    );
    // Three DISTINCT docs (three titles, three hash identities): identical
    // seeds would dedup to one doc, and the read below would owe one row.
    let docs: Vec<NodeId> = (0..3)
        .map(|i| seed_catalogued_titled(&c, &format!("catalogued {i}")))
        .collect();
    let stop = AtomicBool::new(false);
    let sets = AtomicUsize::new(0);
    let reads = AtomicUsize::new(0);
    let escalated = AtomicUsize::new(0);

    // The reader's verdict is RECORDED inside the scope and ASSERTED after
    // it, never panicked inside it: a panic inside `thread::scope` unwinds
    // to the scope's join, which waits on writers that stop only when told
    // to — so an assertion failure here used to HANG the suite instead of
    // failing it. Every exit
    // from the loop sets `stop` first.
    let mut verdict: Result<(), String> = Ok(());
    std::thread::scope(|scope| {
        let door = &c;
        for w in 0..3usize {
            let stop = &stop;
            let sets = &sets;
            let doc = docs[w].clone();
            scope.spawn(move || {
                let mut n = 0u64;
                while !stop.load(Ordering::SeqCst) {
                    n += 1;
                    let out = door.submit(commit(
                        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t"#,
                        &[
                            ("id", serde_json::json!(doc.0)),
                            ("t", serde_json::json!(format!("set {w}-{n}"))),
                        ],
                    ));
                    if matches!(out, Ok(Outcome::Committed { .. })) {
                        sets.fetch_add(1, Ordering::SeqCst);
                    }
                }
            });
        }
        let started = Instant::now();
        loop {
            match door.read(&cross_pool_read()) {
                Ok(out) => {
                    reads.fetch_add(1, Ordering::SeqCst);
                    let torn = out.rows.0.len() != 3
                        || out
                            .rows
                            .0
                            .iter()
                            .any(|row| row["state"] != serde_json::json!("published"));
                    if torn {
                        verdict = Err(format!(
                            "an INCONSISTENT CUT under a SET storm: {} rows where every \
                             catalogued doc (3) is owed, every time (retries {}, restarts {}, \
                             escalated {}): {:?}",
                            out.rows.0.len(),
                            out.retries,
                            out.restarts,
                            out.escalated,
                            out.rows.0
                        ));
                        break;
                    }
                    if out.escalated {
                        escalated.fetch_add(1, Ordering::SeqCst);
                        break;
                    }
                }
                Err(e) => {
                    verdict = Err(format!(
                        "a SET storm on its own pool must never make the escalated read give \
                         up: {e:?}"
                    ));
                    break;
                }
            }
            if started.elapsed() >= Duration::from_secs(60) {
                verdict = Err(
                    "no read escalated in 60s under a three-writer SET storm; the storm is not \
                     a storm"
                        .to_string(),
                );
                break;
            }
        }
        stop.store(true, Ordering::SeqCst);
    });
    if let Err(why) = verdict {
        panic!("{why}");
    }

    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    assert!(spine.pending().expect("pending").is_empty());
    println!(
        "set storm: {} SETs landed, {} consistent reads, {} escalated",
        sets.load(Ordering::SeqCst),
        reads.load(Ordering::SeqCst),
        escalated.load(Ordering::SeqCst)
    );
}

// ---------------------------------------------------------------------------
// the MEASUREMENT (data, no thresholds)
// ---------------------------------------------------------------------------

/// Commits per second and read latency on one fixture, three consecutive
/// runs of 200 each: single-pool commits, cross-pool commits, quiet
/// cross-pool reads, and cross-pool reads under an UNRELATED-pool commit
/// storm (the case the strict step 4 changes). Ignored: it is a measurement,
/// not a check, and it asserts nothing. Run with
/// `cargo test --release -p spine --test validated_read -- --ignored --nocapture measure`.
#[test]
#[ignore = "a measurement, not a check: run it by name with --ignored --nocapture"]
fn measure_commits_and_reads() {
    let dir = root("measure");
    let c = fresh_cauldron(&dir, SpineConfig::default());
    let doc = seed_catalogued(&c);
    const N: usize = 200;
    for run in 1..=3 {
        // single-pool commits
        let t = Instant::now();
        for i in 0..N {
            c.submit(commit(
                r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" })"#,
                &[("t", serde_json::json!(format!("m{run}-{i}")))],
            ))
            .expect("commit");
        }
        let single = t.elapsed();
        // cross-pool commits: publish + ticket, over fresh drafts
        let drafts: Vec<NodeId> = (0..N).map(|i| create_doc(&c, &format!("x{run}-{i}"))).collect();
        let t = Instant::now();
        for id in &drafts {
            c.submit(commit(
                r#"MATCH (d: fixture-alpha/doc { id: $id })
                   CREATE (t: fixture-beta/ticket { state: "open" })
                   CREATE (t) -[catalogues]-> (d)
                   SET d.state = "published""#,
                &[("id", serde_json::json!(id.0))],
            ))
            .expect("commit");
        }
        let cross = t.elapsed();
        // quiet cross-pool reads (by identifier on the doc side, one row)
        let req = ReadRequest {
            statement: r#"MATCH (t: fixture-beta/ticket) -[catalogues]-> (d: fixture-alpha/doc { id: $id })
                          RETURN d.id AS doc"#
                .to_string(),
            bindings: Bindings::from([("id".to_string(), serde_json::json!(doc.0))]),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        };
        let t = Instant::now();
        let mut retries = 0u32;
        for _ in 0..N {
            retries += c.read(&req).expect("read").retries;
        }
        let quiet = t.elapsed();
        // cross-pool reads under an unrelated-pool storm
        let stop = AtomicBool::new(false);
        let landed = AtomicUsize::new(0);
        let (stormy, stormy_retries, stormy_escalated) = std::thread::scope(|scope| {
            let door = &c;
            let stop = &stop;
            let landed = &landed;
            scope.spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    if door
                        .submit(commit(
                            r#"CREATE (i: fixture-gamma/issue { title: "noise", state: "open" })"#,
                            &[],
                        ))
                        .is_ok()
                    {
                        landed.fetch_add(1, Ordering::SeqCst);
                    }
                }
            });
            let t = Instant::now();
            let mut retries = 0u32;
            let mut escalated = 0u32;
            for _ in 0..N {
                let out = c.read(&req).expect("read");
                retries += out.retries;
                escalated += out.escalated as u32;
            }
            let d = t.elapsed();
            stop.store(true, Ordering::SeqCst);
            (d, retries, escalated)
        });
        println!(
            "MEASUREMENT run {run}: single-pool commit {:.2} ms ({:.0}/s); cross-pool commit \
             {:.2} ms ({:.0}/s); quiet cross-pool read {:.2} ms ({retries} retries over {N}); \
             cross-pool read under unrelated-pool storm {:.2} ms ({stormy_retries} retries, \
             {stormy_escalated} escalated, {} unrelated commits landed meanwhile)",
            single.as_secs_f64() * 1000.0 / N as f64,
            N as f64 / single.as_secs_f64(),
            cross.as_secs_f64() * 1000.0 / N as f64,
            N as f64 / cross.as_secs_f64(),
            quiet.as_secs_f64() * 1000.0 / N as f64,
            stormy.as_secs_f64() * 1000.0 / N as f64,
            landed.load(Ordering::SeqCst),
        );
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-validated-{name}-{}", std::process::id()))
}

/// Three pools: the two the cross-pool read names, plus GAMMA, which nothing
/// the read touches lives in (the unrelated pool).
fn fresh_cauldron(dir: &Path, config: SpineConfig) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(
        dir,
        &[
            fixtures::fixture_alpha(),
            fixtures::fixture_beta(),
            fixtures::fixture_workspace(GAMMA),
        ],
        config,
    )
    .expect("create the cauldron")
}

fn commit(statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
    let mut bindings = Bindings::new();
    for (k, v) in params {
        bindings.insert((*k).to_string(), v.clone());
    }
    Envelope {
        statement: statement.to_string(),
        bindings,
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent {
                thread: "validated-read".into(),
                dispatch: "validated-read".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn create_doc(c: &Cauldron, title: &str) -> NodeId {
    match c
        .submit(commit(
            r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" }) RETURN d.id"#,
            &[("t", serde_json::json!(title))],
        ))
        .expect("create a doc")
    {
        Outcome::Committed { returned, .. } => {
            NodeId(returned.0[0]["d.id"].as_str().expect("id").to_string())
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}

/// One published doc in alpha with its catalogue ticket in beta, so the
/// cross-pool read has exactly one row to return.
///
/// NOT "every call adds one more": two calls submit byte-identical envelopes
/// (same statement, same bindings, same provenance, no nonce), and the door
/// answers the second as a DUPLICATE of the first — exactly-once
/// — so a second call seeds nothing. A test that wants N catalogued
/// docs seeds N DISTINCT titles through [`seed_catalogued_titled`] (the
/// SET-storm test once seeded one doc three times this way).
fn seed_catalogued(c: &Cauldron) -> NodeId {
    seed_catalogued_titled(c, "catalogued")
}

/// [`seed_catalogued`] under a caller-chosen title, so N calls with N titles
/// are N distinct commits and N rows for the cross-pool read.
fn seed_catalogued_titled(c: &Cauldron, title: &str) -> NodeId {
    let doc = create_doc(c, title);
    c.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id })
           CREATE (t: fixture-beta/ticket { state: "open" })
           CREATE (t) -[catalogues]-> (d)
           SET d.state = "published""#,
        &[("id", serde_json::json!(doc.0))],
    ))
    .expect("the cross-pool seed commits");
    doc
}

/// THE STORM, as a state rather than as traffic: a transaction left DECIDED
/// and un-landed across both pools, holding NO locks. While it stands,
/// step 3's landing-state check invalidates every lock-free cross-pool cut,
/// so the read's retries are forced deterministically instead of raced for.
fn stall_a_cross_pool_commit(dir: &Path) -> Seq {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let env = commit("-- the storm", &[]);
    let hash = env.hash_identity().expect("hash");
    let payload = CompiledPayload {
        parts: vec![
            CompiledPart {
                pool: PoolName::new(ALPHA),
                schema_version: 1,
                deltas: vec![Delta::CreateNode {
                    id: NodeId("n-storm-alpha".into()),
                    node_type: "doc".into(),
                    state: Some("draft".into()),
                    fields: std::collections::BTreeMap::from([(
                        "title".to_string(),
                        serde_json::json!("never landed"),
                    )]),
                }],
                premises: Vec::new(),
                far_versions: Default::default(),
            },
            CompiledPart {
                pool: PoolName::new(BETA),
                schema_version: 2,
                deltas: vec![Delta::CreateNode {
                    id: NodeId("n-storm-beta".into()),
                    node_type: "ticket".into(),
                    state: Some("open".into()),
                    fields: std::collections::BTreeMap::new(),
                }],
                premises: Vec::new(),
                far_versions: Default::default(),
            },
        ],
        minted: std::collections::BTreeMap::new(),
    };
    let seq = spine.append_pending(&env, &hash, &payload).expect("append");
    spine.mark_decided(seq).expect("decided");
    seq
}

/// Call the storm off: land both parts and clear, so the next cut is
/// consistent.
fn finish(dir: &Path, seq: Seq) {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    for part in 0..2u32 {
        spine.mark_part_landed(seq, part).expect("land");
    }
    spine
        .mark_cleared(seq, &serde_json::json!({ "storm": "over" }))
        .expect("clear");
}

/// A SET on one doc's title, taken as far as a real commit gets before its
/// cleared flip and HELD there: row appended, write on the body acquired,
/// decided, the one part LANDED in the pool (counter moved, committed-at
/// stamped) — and neither cleared nor released. This is the state a commit
/// is in between its last landing and its cleared flip; the pause seam's
/// `before-cleared` stage holds a real door there, but no victim job SETs
/// an existing doc, so the window is hand-held here
/// with the library's own test-support flips.
fn hold_a_landed_set(dir: &Path, doc: &NodeId, title: &str, config: &SpineConfig) -> Seq {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let env = commit(&format!("-- held SET: {title}"), &[]);
    let hash = env.hash_identity().expect("hash");
    let part = CompiledPart {
        pool: PoolName::new(ALPHA),
        schema_version: 1,
        deltas: vec![Delta::SetField {
            node: doc.clone(),
            path: vec!["title".to_string()],
            value: serde_json::json!(title),
        }],
        premises: Vec::new(),
        far_versions: Default::default(),
    };
    let payload = CompiledPayload {
        parts: vec![part.clone()],
        minted: std::collections::BTreeMap::new(),
    };
    let seq = spine.append_pending(&env, &hash, &payload).expect("append");
    locks::acquire(
        &spine,
        seq,
        &one(ALPHA, Resource::Body { node: doc.clone() }, LockMode::Write),
        config,
    )
    .expect("the older SET holds its body write");
    spine.mark_decided(seq).expect("decided");
    let mut pool = Pool::open(&Pool::path_in(dir, &PoolName::new(ALPHA))).expect("alpha");
    pool.apply_part(&hash, 0, &part).expect("the SET lands in alpha");
    spine.mark_part_landed(seq, 0).expect("landed");
    seq
}

/// The held SET's cleared flip and, at exactly that flip, its release.
fn finish_held(dir: &Path, seq: Seq) {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    spine
        .mark_cleared(seq, &serde_json::json!({ "held_set": "cleared" }))
        .expect("clear");
    locks::release_at_clear(&spine, seq).expect("release");
}

fn one(pool: &str, resource: Resource, mode: LockMode) -> ClaimSet {
    let mut set = ClaimSet::new();
    set.insert(Claim {
        pool: PoolName::new(pool),
        resource,
        mode,
    });
    set
}

fn park(dir: &Path, held: &ClaimSet, tag: &str) -> Seq {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let env = commit(&format!("-- parking holder: {tag}"), &[]);
    let hash = env.hash_identity().expect("hash");
    let seq = spine
        .append_pending(
            &env,
            &hash,
            &CompiledPayload {
                parts: Vec::new(),
                minted: std::collections::BTreeMap::new(),
            },
        )
        .expect("parking row");
    locks::acquire(&spine, seq, held, &SpineConfig::default()).expect("park");
    seq
}

fn unpark(dir: &Path, seq: Seq) {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    locks::release_at_clear(&spine, seq).expect("release");
    spine
        .mark_cleared(seq, &serde_json::json!({ "parking_holder": "released" }))
        .expect("close the parking row");
}

fn await_new_claimant(spine: &SpineDb, pool: &str, resource: &Resource, known: &[Seq]) -> Seq {
    let started = Instant::now();
    loop {
        let q = locks::queue(spine, &PoolName::new(pool), resource).expect("queue");
        if let Some(e) = q.iter().find(|e| !known.contains(&e.seq) && !e.granted) {
            return e.seq;
        }
        assert!(
            started.elapsed() < Duration::from_secs(25),
            "the escalation never queued on {pool}/{resource:?}: {q:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Wait for a NEW read claimant on `body` (not `holder`) — or for the reader
/// to finish first, which is the pre-fix failure (it gave up as `Busy`
/// without ever claiming the body) and is reported as exactly that.
fn await_body_claimant_or_finished<T>(
    spine: &SpineDb,
    body: &Resource,
    holder: Seq,
    reader: &std::thread::ScopedJoinHandle<'_, T>,
) -> Seq {
    let started = Instant::now();
    loop {
        let q = locks::queue(spine, &PoolName::new(ALPHA), body).expect("queue");
        if let Some(e) = q
            .iter()
            .find(|e| e.seq != holder && e.mode == LockMode::Read && !e.granted)
        {
            return e.seq;
        }
        assert!(
            !reader.is_finished(),
            "the escalated read FINISHED without ever claiming the matched body — the pre-fix \
             shape (slice-only claims, validated under locks, gave up as Busy); queue: {q:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(25),
            "the escalated read never queued on the body: {q:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// How many escalation rows the spine holds (the reader's `core-mechanical`
/// identity, `read::escalation`).
fn escalation_rows(dir: &Path) -> i64 {
    let sql = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("direct SQL");
    sql.query_row(
        "SELECT COUNT(*) FROM transactions WHERE initiator LIKE '%read::escalation%'",
        [],
        |r| r.get(0),
    )
    .expect("count")
}

/// Every type slice AND every named body is unlocked.
fn assert_every_lock_released(spine: &SpineDb, bodies: &[&NodeId]) {
    for pool in [ALPHA, BETA, GAMMA] {
        for ty in ["doc", "ticket", "author", "shelf", "issue"] {
            assert!(
                locks::queue(
                    spine,
                    &PoolName::new(pool),
                    &Resource::TypeSlice {
                        node_type: ty.into()
                    }
                )
                .expect("queue")
                .is_empty(),
                "every slice lock was released ({pool}/{ty})"
            );
        }
    }
    for b in bodies {
        assert!(
            locks::queue(
                spine,
                &PoolName::new(ALPHA),
                &Resource::Body { node: (*b).clone() }
            )
            .expect("queue")
            .is_empty(),
            "every body lock was released ({})",
            b.0
        );
    }
}
