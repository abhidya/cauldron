//! Own-layer tests for the spine record:
//! row lifecycle, exactly-once dedup via the row, the committed-commit
//! counter's last-part coupling, the two exposures with N >= 2 in-flight
//! transactions, and strictly-increasing sequence numbers under REAL
//! concurrent appends — plus the THIRD exposure
//! (`committed_at` and `commits_touching`) and the delta-bearing meaning of
//! "touched".
//!
//! Every checker here was forced red at least once.

use spine::delta::{CompiledPart, CompiledPayload, Delta};
use spine::pool::NodeId;
use spine::envelope::{Envelope, Initiator, Instrument, Mode, Provenance, TxHash};
use spine::error::SpineError;
use spine::schema_def::PoolName;
use spine::spine_db::{AppendOutcome, Seq, SpineDb};
use std::collections::BTreeMap;
use std::path::PathBuf;

test_scratch::install!();

// ---- fixtures ----

/// A fresh directory per test (keyed by test name + pid so parallel test
/// threads and reruns never collide), holding one spine.db path.
fn fresh_spine_path(test: &str) -> PathBuf {
    let dir = test_scratch::tmp_path(format!("spine-record-{}-{}", test, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join("spine.db")
}

fn envelope(statement: &str) -> Envelope {
    Envelope {
        statement: statement.to_string(),
        bindings: BTreeMap::new(),
        format_version: 1,
        provenance: Provenance {
            initiator: Initiator::CoreMechanical {
                operation: "spine-record-test".to_string(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

/// A payload with one part per named pool, each carrying ONE delta — the
/// spine record does not interpret deltas, but it reads
/// whether a part HAS any (a delta-bearing part touches its pool; a
/// premise-only part does not), so every part here touches. See
/// [`premise_only`] for the other kind.
fn payload(pools: &[&str]) -> CompiledPayload {
    CompiledPayload {
        parts: pools.iter().map(|p| touching_part(p)).collect(),
        minted: BTreeMap::new(),
    }
}

fn touching_part(pool: &str) -> CompiledPart {
    CompiledPart {
        pool: PoolName::new(pool),
        schema_version: 1,
        deltas: vec![Delta::CreateNode {
            id: NodeId(format!("n-{pool}")),
            node_type: "thing".to_string(),
            state: None,
            fields: BTreeMap::new(),
        }],
        premises: vec![],
        far_versions: Default::default(),
    }
}

/// A part with NO deltas — a premise-only part (a satisfied count
/// expectation, a WHERE over a pool nothing in the statement changes). It is
/// recorded with its validated-under stamp and it does NOT touch its pool
/// for invalidation purposes.
fn premise_only(pool: &str) -> CompiledPart {
    CompiledPart {
        pool: PoolName::new(pool),
        schema_version: 7,
        deltas: vec![],
        premises: vec![],
        far_versions: Default::default(),
    }
}

fn hash(n: u8) -> TxHash {
    TxHash([n; 32])
}

// ---- create / open plumbing ----

#[test]
fn create_refuses_existing_and_open_refuses_missing() {
    let path = fresh_spine_path("create-open");
    let db = SpineDb::create(&path).expect("create");
    drop(db);

    // `SpineDb` is deliberately not `Debug` (its interior is a connection
    // handle that never escapes the crate), so these matches read the
    // failure side: `.err()` is `Option<SpineError>`, which IS `Debug`.
    match SpineDb::create(&path).err() {
        Some(SpineError::Usage { message }) => {
            assert!(message.contains("existing"), "names the refusal: {message}")
        }
        other => panic!("create over an existing file must refuse Usage, got {other:?}"),
    }

    let missing = path.with_file_name("never-created.db");
    match SpineDb::open(&missing).err() {
        Some(SpineError::NotFound { .. }) => {}
        other => panic!("open of a missing file must be NotFound, got {other:?}"),
    }

    SpineDb::open(&path).expect("reopen an existing spine");
}

// ---- the row lifecycle ----

#[test]
fn row_lifecycle_append_decide_land_clear() {
    let path = fresh_spine_path("lifecycle");
    let mut db = SpineDb::create(&path).expect("create");
    let h = hash(1);
    let seq = db
        .append_pending(&envelope("CREATE (d: fixture-alpha/doc {})"), &h, &payload(&["fixture-alpha", "fixture-beta"]))
        .expect("append");

    // Open: everything recorded, nothing flipped.
    let row = db.find_by_hash(&h).expect("read").expect("row exists");
    assert_eq!(row.seq, seq);
    assert_eq!(row.hash, h);
    assert!(!row.decided && !row.cleared);
    assert_eq!(row.parts.len(), 2);
    assert!(row.parts.iter().all(|p| !p.landed));
    assert_eq!(row.parts[0].pool, PoolName::new("fixture-alpha"));
    assert_eq!(row.parts[1].pool, PoolName::new("fixture-beta"));
    assert_eq!(row.parts[0].schema_version, 1);
    assert_eq!(row.statement, "CREATE (d: fixture-alpha/doc {})");
    assert_eq!(row.envelope_version, 1);
    assert!(row.result.is_none());
    // The timestamp is ISO-8601 UTC TEXT, recorded and never an ordering
    // key: shaped like 2026-09-07T...Z.
    assert!(row.timestamp.len() >= 20, "iso-8601: {}", row.timestamp);
    assert_eq!(&row.timestamp[4..5], "-");
    assert!(row.timestamp.contains('T'));
    assert!(row.timestamp.ends_with('Z'));

    // Pending worklist carries it until the cleared flip.
    let pending = db.pending().expect("pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].seq, seq);

    // Decided flip.
    db.mark_decided(seq).expect("decide");
    let row = db.find_by_hash(&h).unwrap().unwrap();
    assert!(row.decided && !row.cleared);

    // Per-part landed flips.
    db.mark_part_landed(seq, 0).expect("land part 0");
    let row = db.find_by_hash(&h).unwrap().unwrap();
    assert!(row.parts[0].landed && !row.parts[1].landed);
    db.mark_part_landed(seq, 1).expect("land part 1");

    // Cleared flip stores the RETURN projection.
    let outcome = serde_json::json!([{"id": "n0011223344556677"}]);
    db.mark_cleared(seq, &outcome).expect("clear");
    let row = db.find_by_hash(&h).unwrap().unwrap();
    assert!(row.cleared);
    assert_eq!(row.result, Some(outcome));
    assert!(db.pending().unwrap().is_empty(), "cleared rows leave the worklist");

    // Flips on a row that does not exist are loud, never silent.
    assert!(matches!(db.mark_decided(Seq(999)), Err(SpineError::NotFound { .. })));
    assert!(matches!(db.mark_part_landed(seq, 7), Err(SpineError::NotFound { .. })));
    assert!(matches!(
        db.mark_cleared(Seq(999), &serde_json::Value::Null),
        Err(SpineError::NotFound { .. })
    ));
}

/// The lifecycle is a STATE MACHINE and out-of-order flips are loud (the two
/// guards in `spine_db.rs`).
#[test]
fn out_of_order_lifecycle_flips_are_refused_by_name() {
    let path = fresh_spine_path("lifecycle-order");
    let mut db = SpineDb::create(&path).expect("create");
    let h = hash(8);
    let seq = db
        .append_pending(&envelope("guards"), &h, &payload(&["alpha", "beta"]))
        .expect("append");

    // A part may not land on an UNDECIDED row: the decided marker is what
    // recovery reads to choose abort versus roll-forward,
    // and an effect under an undecided row would make recovery abort a
    // transaction whose effects exist.
    match db.mark_part_landed(seq, 0) {
        Err(SpineError::Usage { message }) => assert!(
            message.contains("not decided"),
            "names the missing flip: {message}"
        ),
        other => panic!("landing under an undecided row must refuse, got {other:?}"),
    }

    // An UNDECIDED row may always be cleared — that IS the abort path.
    db.mark_cleared(seq, &serde_json::Value::Null)
        .expect("an undecided row clears: the abort path");
    // …and the row remains, forever, as the record of the attempt.
    assert!(db.find_by_hash(&h).unwrap().is_some());
    assert_eq!(
        db.committed_commit_counter().unwrap(),
        0,
        "an abort is not a commit"
    );

    // A DECIDED row may be cleared only when every part has landed (locks
    // release when every part in every pool has landed AND the row flips to
    // cleared).
    let h2 = hash(9);
    let seq2 = db
        .append_pending(&envelope("guards-2"), &h2, &payload(&["alpha", "beta"]))
        .expect("append");
    db.mark_decided(seq2).unwrap();
    db.mark_part_landed(seq2, 0).unwrap();
    match db.mark_cleared(seq2, &serde_json::Value::Null) {
        Err(SpineError::Usage { message }) => assert!(
            message.contains("not yet landed"),
            "names the unlanded part: {message}"
        ),
        other => panic!("clearing a decided row with an unlanded part must refuse, got {other:?}"),
    }
    assert_eq!(
        db.in_landing(None).unwrap().len(),
        1,
        "and it is still in landing state, where the exposure says it is"
    );
    db.mark_part_landed(seq2, 1).unwrap();
    db.mark_cleared(seq2, &serde_json::Value::Null)
        .expect("every part landed: the cleared flip is legal now");
}

// ---- exactly-once ----

#[test]
fn duplicate_hash_append_refuses_and_the_row_returns_the_original_outcome() {
    let path = fresh_spine_path("dedup");
    let mut db = SpineDb::create(&path).expect("create");
    let h = hash(2);
    let seq = db
        .append_pending(&envelope("SET d.title = $t"), &h, &payload(&["fixture-alpha"]))
        .expect("append");
    db.mark_decided(seq).unwrap();
    db.mark_part_landed(seq, 0).unwrap();
    let original = serde_json::json!([{"title": "first"}]);
    db.mark_cleared(seq, &original).unwrap();

    // The exactly-once door: the SECOND submission of the same hash identity
    // appends nothing and hands back the ORIGINAL row, carrying the ORIGINAL
    // outcome — in one call, with no second lookup that could observe a
    // different state.
    match db
        .append_or_existing(&envelope("SET d.title = $t"), &h, &payload(&["fixture-alpha"]))
        .expect("dedup is not a failure")
    {
        AppendOutcome::Duplicate(row) => {
            assert_eq!(row.seq, seq, "the ORIGINAL row, not a new one");
            assert!(row.cleared);
            assert_eq!(row.result, Some(original.clone()), "the ORIGINAL outcome");
        }
        AppendOutcome::Appended(s) => {
            panic!("a duplicate hash must never append twice; appended seq {s}")
        }
    }

    // The frozen `append_pending` signature cannot carry a row back, so it
    // refuses loudly and names the dedup door instead of silently returning
    // the old sequence number as if it had appended.
    let err = db
        .append_pending(&envelope("SET d.title = $t"), &h, &payload(&["fixture-alpha"]))
        .expect_err("a duplicate hash must never append twice");
    match &err {
        SpineError::Usage { message } => {
            assert!(message.contains("duplicate hash identity"), "{message}");
            assert!(message.contains(&h.to_hex()), "names the hash: {message}");
        }
        other => panic!("expected Usage on duplicate append, got {other:?}"),
    }

    // "Did this already land" is answerable from the record alone, and the
    // ORIGINAL outcome comes back from the row's result column.
    let row = db.find_by_hash(&h).unwrap().expect("the one row");
    assert_eq!(row.seq, seq);
    assert!(row.cleared);
    assert_eq!(row.result, Some(original));

    // Still exactly one row: the counter saw exactly one commit, and the
    // pending worklist is empty — nothing was appended by the duplicates.
    assert_eq!(db.committed_commit_counter().unwrap(), 1);
    assert!(db.pending().unwrap().is_empty());
}

/// The dedup is atomic with the insert: N >= 2 real threads racing the SAME
/// hash identity produce exactly ONE append, and every loser gets the
/// winner's row back (exactly-once — no global semaphore, just
/// the spine's own record).
#[test]
fn concurrent_submissions_of_one_hash_append_exactly_once() {
    let path = fresh_spine_path("dedup-race");
    let db = SpineDb::create(&path).expect("create");
    drop(db);

    let h = hash(0x2b);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let path = path.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            let mut db = SpineDb::open(&path).expect("thread open");
            barrier.wait();
            match db
                .append_or_existing(&envelope("race"), &h, &payload(&["alpha"]))
                .expect("append or dedup")
            {
                AppendOutcome::Appended(s) => (true, s),
                AppendOutcome::Duplicate(row) => (false, row.seq),
            }
        }));
    }
    let results: Vec<(bool, Seq)> = handles.into_iter().map(|h| h.join().expect("join")).collect();
    let appended: Vec<&(bool, Seq)> = results.iter().filter(|(fresh, _)| *fresh).collect();
    assert_eq!(appended.len(), 1, "exactly one append: {results:?}");
    let winner = appended[0].1;
    for (_, seq) in &results {
        assert_eq!(*seq, winner, "every loser is handed the winner's row");
    }
    let db = SpineDb::open(&path).unwrap();
    assert_eq!(db.pending().unwrap().len(), 1, "exactly one row exists");
    // `hash` is not a UNIQUE column — an identity may name several
    // ATTEMPTS — so "exactly one row" is asserted over the identity's whole attempt history rather than
    // inherited from a column constraint. `idx_tx_live` is what arbitrates
    // here, and this is its N >= 2 proof.
    assert_eq!(
        db.attempts_of(&h).unwrap().len(),
        1,
        "four concurrent submitters, one attempt"
    );
}

// ---- attempts: dedup keys on "did this LAND" ----

/// An ABORTED attempt lands nothing, so it is not an answer to "did this
/// land" and it must not block the same ask from being attempted again.
/// The restart rule needs exactly this: the abandoned attempt's row
/// is recorded aborted and the next attempt gets a FRESH sequence number.
///
/// RED once by: restoring `UNIQUE` on the `hash` column — the second append
/// then failed with a constraint violation instead of opening attempt 2.
#[test]
fn an_aborted_attempt_never_blocks_a_fresh_attempt() {
    let path = fresh_spine_path("attempts");
    let mut db = SpineDb::create(&path).expect("create");
    let h = hash(0x5a);

    // Attempt 1, abandoned: appended, never decided, then cleared — which is
    // the ABORT shape (decided = 0, cleared = 1) and is what the door writes
    // when a statement refuses or restarts.
    let first = db
        .append_pending(&envelope("attempt one"), &h, &payload(&["fixture-alpha"]))
        .expect("attempt 1");
    db.mark_cleared(first, &serde_json::json!({"aborted": true}))
        .expect("an undecided row may be cleared — that IS the abort");

    // Attempt 2: a FRESH row, a FRESH sequence number, strictly later.
    let second = match db
        .append_or_existing(&envelope("attempt one"), &h, &payload(&["fixture-alpha"]))
        .expect("an abort never blocks a retry")
    {
        AppendOutcome::Appended(seq) => seq,
        AppendOutcome::Duplicate(row) => {
            panic!("an aborted attempt must not dedup; got seq {}", row.seq)
        }
    };
    assert!(second > first, "the restarted attempt's number is fresher");

    // The identity's history reads as what it is: two attempts, in order,
    // both kept forever (the abort is a recorded fact).
    let attempts = db.attempts_of(&h).expect("history");
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].attempt, 1);
    assert_eq!(attempts[0].seq, first);
    assert!(!attempts[0].decided && attempts[0].cleared, "attempt 1 aborted");
    assert_eq!(attempts[1].attempt, 2);
    assert_eq!(attempts[1].seq, second);

    // And `find_by_hash` answers with the AUTHORITATIVE row — the live
    // attempt, not the abandoned one.
    assert_eq!(db.find_by_hash(&h).unwrap().expect("row").seq, second);
}

/// Once an identity has LANDED it is final, however many attempts preceded
/// it: every later submission dedups to the committed row and its original
/// outcome (exactly-once), and no further attempt is ever opened.
///
/// RED once by: dropping the `decided = 1` arm from `append_or_existing`'s
/// dedup test — the landed identity then opened attempt 3 and would have
/// landed twice.
#[test]
fn a_landed_identity_is_final_however_many_attempts_preceded_it() {
    let path = fresh_spine_path("attempts-final");
    let mut db = SpineDb::create(&path).expect("create");
    let h = hash(0x5b);

    // One abandoned attempt, then one that lands.
    let first = db
        .append_pending(&envelope("twice"), &h, &payload(&["fixture-alpha"]))
        .expect("attempt 1");
    db.mark_cleared(first, &serde_json::json!({"aborted": true}))
        .unwrap();
    let second = db
        .append_pending(&envelope("twice"), &h, &payload(&["fixture-alpha"]))
        .expect("attempt 2");
    db.mark_decided(second).unwrap();
    db.mark_part_landed(second, 0).unwrap();
    let original = serde_json::json!([{"id": "n-1"}]);
    db.mark_cleared(second, &original).unwrap();

    // Every later submission is a dedup onto the LANDED row, forever.
    for _ in 0..3 {
        match db
            .append_or_existing(&envelope("twice"), &h, &payload(&["fixture-alpha"]))
            .expect("dedup is not a failure")
        {
            AppendOutcome::Duplicate(row) => {
                assert_eq!(row.seq, second, "the LANDED attempt, not the aborted one");
                assert_eq!(row.attempt, 2);
                assert_eq!(row.result, Some(original.clone()), "the ORIGINAL outcome");
            }
            AppendOutcome::Appended(s) => panic!("a landed identity must never re-open; seq {s}"),
        }
    }
    assert_eq!(db.attempts_of(&h).unwrap().len(), 2, "no attempt 3 exists");
    assert_eq!(db.committed_commit_counter().unwrap(), 1, "one landing");
}

/// AN ABORTED ATTEMPT IS NEVER DECIDED AFTERWARDS (the lifecycle guard
/// on `mark_decided`). Deciding a cleared-and-undecided row would resurrect an
/// abandoned attempt into the commit index and claim a landing for a payload
/// nothing ever applied — and with several attempts per identity now legal,
/// a row that is BOTH abandoned and re-decidable is a state the record must
/// refuse rather than merely never produce.
///
/// The guard's OTHER arm — at most one attempt per identity may ever be
/// DECIDED — is a defensive net rather than a reachable path, and is recorded
/// as such: `append_or_existing` dedups a decided identity, and `idx_tx_live`
/// admits one un-cleared row, so no public sequence reaches it. It is kept so
/// that a caller bypassing the dedup door meets a NAMED refusal rather than a
/// bare SQLite constraint from `idx_tx_committed`.
///
/// RED once by: deleting `mark_decided`'s cleared arm — the aborted attempt
/// was then decided, `find_by_hash` promoted it to the authoritative row, and
/// `pending()` no longer held it, so recovery would never have touched it.
#[test]
fn an_aborted_attempt_is_never_decided_afterwards() {
    let path = fresh_spine_path("no-resurrection");
    let mut db = SpineDb::create(&path).expect("create");
    let h = hash(0x5c);

    let first = db
        .append_pending(&envelope("abandoned"), &h, &payload(&["fixture-alpha"]))
        .expect("attempt 1");
    db.mark_cleared(first, &serde_json::json!({"aborted": true}))
        .expect("an undecided row clears — that IS the abort");

    match db.mark_decided(first) {
        Err(SpineError::Usage { message }) => assert!(
            message.contains("already cleared"),
            "names the rule: {message}"
        ),
        other => panic!("deciding an aborted attempt must refuse by name, got {other:?}"),
    }
    // The abort is unchanged and still an abort.
    let row = db.row_at(first).expect("row");
    assert!(!row.decided && row.cleared);
    assert_eq!(db.committed_commit_counter().unwrap(), 0);

    // …and the retry path is still open: attempt 2 opens normally, which is
    // the whole point of the abort not being a terminal verdict.
    let second = db
        .append_pending(&envelope("abandoned"), &h, &payload(&["fixture-alpha"]))
        .expect("attempt 2");
    db.mark_decided(second).expect("the LIVE attempt decides");
    assert_eq!(db.attempts_of(&h).unwrap().len(), 2);
}

// ---- the counter's last-part coupling ----

#[test]
fn counter_increments_only_when_the_last_part_lands() {
    let path = fresh_spine_path("counter");
    let mut db = SpineDb::create(&path).expect("create");
    assert_eq!(db.committed_commit_counter().unwrap(), 0);

    let h = hash(3);
    let seq = db
        .append_pending(&envelope("s"), &h, &payload(&["a", "b", "c"]))
        .expect("append");
    db.mark_decided(seq).unwrap();

    db.mark_part_landed(seq, 0).unwrap();
    assert_eq!(db.committed_commit_counter().unwrap(), 0, "first part is not a commit");
    db.mark_part_landed(seq, 1).unwrap();
    assert_eq!(db.committed_commit_counter().unwrap(), 0, "middle part is not a commit");
    db.mark_part_landed(seq, 2).unwrap();
    assert_eq!(db.committed_commit_counter().unwrap(), 1, "the LAST landing is the commit");

    // Recovery idempotence: re-landing a landed part is
    // detected, not repeated — the counter can never double-count.
    db.mark_part_landed(seq, 2).unwrap();
    db.mark_part_landed(seq, 0).unwrap();
    assert_eq!(db.committed_commit_counter().unwrap(), 1);
}

// ---- the two exposures with N >= 2 in flight ----

#[test]
fn exposures_are_correct_with_two_in_flight_transactions() {
    let path = fresh_spine_path("exposures");
    let mut db = SpineDb::create(&path).expect("create");

    let h1 = hash(4);
    let h2 = hash(5);
    let s1 = db
        .append_pending(&envelope("t1"), &h1, &payload(&["alpha", "beta"]))
        .unwrap();
    let s2 = db
        .append_pending(&envelope("t2"), &h2, &payload(&["beta", "gamma"]))
        .unwrap();

    // Touched-pool sets, per commit.
    assert_eq!(
        db.touched_pools(&h1).unwrap(),
        vec![PoolName::new("alpha"), PoolName::new("beta")]
    );
    assert_eq!(
        db.touched_pools(&h2).unwrap(),
        vec![PoolName::new("beta"), PoolName::new("gamma")]
    );
    assert!(matches!(
        db.touched_pools(&hash(99)),
        Err(SpineError::NotFound { .. })
    ));

    // Undecided rows are NOT in landing state.
    assert!(db.in_landing(None).unwrap().is_empty());

    db.mark_decided(s1).unwrap();
    db.mark_decided(s2).unwrap();

    // Both in landing, sequence order; queryable by pool.
    let all = db.in_landing(None).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].seq, s1);
    assert_eq!(all[1].seq, s2);
    assert_eq!(all[0].pools, vec![PoolName::new("alpha"), PoolName::new("beta")]);

    let alpha = db.in_landing(Some(&PoolName::new("alpha"))).unwrap();
    assert_eq!(alpha.len(), 1);
    assert_eq!(alpha[0].hash, h1);
    let beta = db.in_landing(Some(&PoolName::new("beta"))).unwrap();
    assert_eq!(beta.len(), 2, "both touch beta");
    assert!(db.in_landing(Some(&PoolName::new("delta"))).unwrap().is_empty());

    // Land + clear T1: it leaves landing state; T2 stays; counter counts 1.
    db.mark_part_landed(s1, 0).unwrap();
    db.mark_part_landed(s1, 1).unwrap();
    db.mark_cleared(s1, &serde_json::Value::Null).unwrap();
    assert_eq!(db.committed_commit_counter().unwrap(), 1);
    let alpha = db.in_landing(Some(&PoolName::new("alpha"))).unwrap();
    assert!(alpha.is_empty());
    let remaining = db.in_landing(None).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].seq, s2);
}

// ---- the THIRD exposure ----

/// `committed_at` is the counter's value once THIS commit was counted,
/// stamped in the same spine transaction as the last part's landed flip;
/// `commits_touching(pools, c_start, c_end)` answers "which commits landed
/// in `(c_start, c_end]` touching any of these pools" — the question a
/// scalar counter cannot. N >= 2 commits, interleaved landings, so the
/// stamps are proven against the counter and not against append order.
// RED: (a) the stamp read from the counter BEFORE the increment —
// every `committed_at` came back one low and T2's window lookup missed it;
// (b) the window's lower bound made inclusive (`>=`) — `(1, 2]` returned
// both commits; (c) the pool filter dropped from the query — `[gamma]` over
// the full window returned T1, which never touched gamma; (d) the stamp
// written in a SEPARATE transaction after the counter's — not
// distinguishable by this test (recorded honestly; the coupling is by
// construction in `mark_part_landed`, one `tx.commit()`).
#[test]
fn committed_at_stamps_with_the_last_landing_and_the_third_exposure_answers_by_window_and_pool() {
    let path = fresh_spine_path("third-exposure");
    let mut db = SpineDb::create(&path).expect("create");
    let h1 = hash(11);
    let h2 = hash(12);
    let s1 = db
        .append_pending(&envelope("t1"), &h1, &payload(&["alpha", "beta"]))
        .unwrap();
    let s2 = db
        .append_pending(&envelope("t2"), &h2, &payload(&["beta", "gamma"]))
        .unwrap();
    db.mark_decided(s1).unwrap();
    db.mark_decided(s2).unwrap();
    assert_eq!(db.row_at(s1).unwrap().committed_at, None, "not counted yet");

    // Interleaved: T2 finishes FIRST although T1 was appended first.
    db.mark_part_landed(s1, 0).unwrap();
    db.mark_part_landed(s2, 0).unwrap();
    db.mark_part_landed(s2, 1).unwrap();
    assert_eq!(db.committed_commit_counter().unwrap(), 1);
    assert_eq!(db.row_at(s2).unwrap().committed_at, Some(1), "T2 was counted at 1");
    assert_eq!(db.row_at(s1).unwrap().committed_at, None, "T1's last part is not in");
    db.mark_part_landed(s1, 1).unwrap();
    assert_eq!(db.committed_commit_counter().unwrap(), 2);
    assert_eq!(db.row_at(s1).unwrap().committed_at, Some(2), "T1 was counted at 2");
    // Re-landing (recovery idempotence) never re-stamps.
    db.mark_part_landed(s1, 1).unwrap();
    assert_eq!(db.row_at(s1).unwrap().committed_at, Some(2));

    let alpha = [PoolName::new("alpha")];
    let beta = [PoolName::new("beta")];
    let gamma = [PoolName::new("gamma")];
    let both = [PoolName::new("alpha"), PoolName::new("gamma")];
    let seqs = |v: Vec<spine::spine_db::CommittedCommit>| -> Vec<Seq> {
        v.iter().map(|c| c.seq).collect()
    };
    // The window is (c_start, c_end]: exclusive start, inclusive end.
    assert_eq!(seqs(db.commits_touching(&beta, 0, 2).unwrap()), vec![s2, s1]);
    assert_eq!(seqs(db.commits_touching(&beta, 1, 2).unwrap()), vec![s1]);
    assert_eq!(seqs(db.commits_touching(&beta, 0, 1).unwrap()), vec![s2]);
    assert!(db.commits_touching(&beta, 2, 2).unwrap().is_empty(), "the empty window");
    assert!(db.commits_touching(&beta, 2, 1).unwrap().is_empty(), "an inverted window is empty");
    // By pool.
    assert_eq!(seqs(db.commits_touching(&alpha, 0, 2).unwrap()), vec![s1]);
    assert_eq!(seqs(db.commits_touching(&gamma, 0, 2).unwrap()), vec![s2]);
    assert_eq!(seqs(db.commits_touching(&both, 0, 2).unwrap()), vec![s2, s1], "a union");
    assert!(db.commits_touching(&[PoolName::new("delta")], 0, 2).unwrap().is_empty());
    assert!(db.commits_touching(&[], 0, 2).unwrap().is_empty(), "no pools, no commits");
    let found = db.commits_touching(&alpha, 0, 2).unwrap();
    assert_eq!(found[0].hash, h1);
    assert_eq!(found[0].committed_at, 2);

    // An ABORTED attempt is never stamped and never appears.
    let h3 = hash(13);
    let s3 = db
        .append_pending(&envelope("t3"), &h3, &payload(&["alpha"]))
        .unwrap();
    assert!(db.mark_aborted_if_undecided(s3, &serde_json::Value::Null).unwrap());
    assert_eq!(db.row_at(s3).unwrap().committed_at, None);
    assert_eq!(seqs(db.commits_touching(&alpha, 0, 99).unwrap()), vec![s1]);
}

/// "Touched" is delta-bearing: a premise-only part is RECORDED — it is in
/// the row's parts with its validated-under stamp, and `touched_pools`
/// (the per-part exposure) lists it — but it does NOT touch its
/// pool for invalidation: `in_landing(Some(pool))` and `commits_touching`
/// ignore it, and `InLanding::touched` / `TxRow::touched` omit it. One
/// definition, every consumer.
// RED: `touched_pools_of` made to include zero-delta parts —
// `in_landing(Some(beta))` returned the row, `row.touched` listed beta, and
// `commits_touching([beta], ..)` returned it after landing.
#[test]
fn a_premise_only_part_is_recorded_with_its_stamp_but_does_not_touch_its_pool() {
    let path = fresh_spine_path("premise-only");
    let mut db = SpineDb::create(&path).expect("create");
    let h = hash(21);
    let seq = db
        .append_pending(
            &envelope("premise-only on beta"),
            &h,
            &CompiledPayload {
                parts: vec![touching_part("alpha"), premise_only("beta")],
                minted: BTreeMap::new(),
            },
        )
        .unwrap();
    db.mark_decided(seq).unwrap();

    // Recorded: both parts, beta with its own stamp (7).
    let row = db.row_at(seq).unwrap();
    assert_eq!(row.parts.len(), 2);
    assert_eq!(row.parts[1].pool, PoolName::new("beta"));
    assert_eq!(row.parts[1].schema_version, 7, "the validated-under stamp is kept");
    assert_eq!(
        db.touched_pools(&h).unwrap(),
        vec![PoolName::new("alpha"), PoolName::new("beta")],
        "the per-part exposure still names every recorded part"
    );
    // Touched: alpha only.
    assert_eq!(row.touched, vec![PoolName::new("alpha")]);
    let landing = db.in_landing(None).unwrap();
    assert_eq!(landing.len(), 1);
    assert_eq!(landing[0].pools, vec![PoolName::new("alpha"), PoolName::new("beta")]);
    assert_eq!(landing[0].touched, vec![PoolName::new("alpha")]);
    assert_eq!(db.in_landing(Some(&PoolName::new("alpha"))).unwrap().len(), 1);
    assert!(
        db.in_landing(Some(&PoolName::new("beta"))).unwrap().is_empty(),
        "a reader of beta has nothing to straddle here"
    );

    // Landed: the counter moves (it is a commit), the stamp lands, and the
    // third exposure names it for alpha and not for beta.
    db.mark_part_landed(seq, 0).unwrap();
    db.mark_part_landed(seq, 1).unwrap();
    db.mark_cleared(seq, &serde_json::Value::Null).unwrap();
    assert_eq!(db.committed_commit_counter().unwrap(), 1);
    assert_eq!(db.commits_touching(&[PoolName::new("alpha")], 0, 1).unwrap().len(), 1);
    assert!(db.commits_touching(&[PoolName::new("beta")], 0, 1).unwrap().is_empty());
}

// ---- sequence numbers under real concurrency ----

#[test]
fn sequence_numbers_strictly_increase_under_concurrent_appends() {
    let path = fresh_spine_path("concurrent-seq");
    let db = SpineDb::create(&path).expect("create");
    drop(db);

    const THREADS: u8 = 4;
    const PER_THREAD: u8 = 10;
    // The spine file is the one storage-layer serialization point (a known
    // residual) and it is the first number to measure. This test is the
    // measurement: timed here, printed as DATA, with no invented threshold
    // and no assertion on it — a performance assertion would be a threshold
    // nobody chose.
    let started = std::time::Instant::now();
    let mut handles = Vec::new();
    for t in 0..THREADS {
        let path = path.clone();
        handles.push(std::thread::spawn(move || {
            let mut db = SpineDb::open(&path).expect("thread open");
            let mut seqs = Vec::new();
            for i in 0..PER_THREAD {
                let mut bytes = [0u8; 32];
                bytes[0] = t;
                bytes[1] = i;
                bytes[2] = 0xAA; // distinct from every other test's hashes
                let seq = db
                    .append_pending(&envelope("concurrent"), &TxHash(bytes), &payload(&["alpha"]))
                    .expect("concurrent append");
                seqs.push(seq.0);
            }
            seqs
        }));
    }
    let per_thread: Vec<Vec<i64>> = handles.into_iter().map(|h| h.join().expect("join")).collect();
    let elapsed = started.elapsed();
    let appends = (THREADS as u32) * (PER_THREAD as u32);
    eprintln!(
        "MEASUREMENT spine-file append throughput: {appends} appends, {THREADS} threads, \
         synchronous=FULL, WAL: {elapsed:?} total, {:.0} appends/s",
        appends as f64 / elapsed.as_secs_f64()
    );

    // Each thread's own issue order is strictly increasing…
    for seqs in &per_thread {
        for w in seqs.windows(2) {
            assert!(w[0] < w[1], "issue order must strictly increase: {w:?}");
        }
    }
    // …and no number is ever issued twice across threads.
    let mut all: Vec<i64> = per_thread.into_iter().flatten().collect();
    let issued = all.len();
    assert_eq!(issued, (THREADS as usize) * (PER_THREAD as usize));
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), issued, "sequence numbers must be unique");

    // The worklist reads back in sequence order, never timestamp order.
    let db = SpineDb::open(&path).unwrap();
    let pending = db.pending().unwrap();
    assert_eq!(pending.len(), issued);
    for w in pending.windows(2) {
        assert!(w[0].seq < w[1].seq);
    }
}

// ---- aborted rows remain; halt is durable and fail-closed ----

#[test]
fn aborted_rows_remain_forever_and_a_halted_spine_refuses_appends() {
    let path = fresh_spine_path("abort-halt");
    let mut db = SpineDb::create(&path).expect("create");

    // An "aborted" transaction: appended, never decided, never landed. Its
    // row remains — the abort is a recorded fact, and no pruning path
    // exists anywhere on this surface.
    let h = hash(6);
    db.append_pending(&envelope("aborted"), &h, &payload(&["alpha"]))
        .unwrap();
    let row = db.find_by_hash(&h).unwrap().expect("the abort's record");
    assert!(!row.decided && !row.cleared);
    assert!(row.parts.iter().all(|p| !p.landed));

    // Halt: fail-closed, loud, durable across handles.
    assert!(db.halted().unwrap().is_none());
    db.set_halted("spine-record test: simulated physics refusal").unwrap();
    let err = db
        .append_pending(&envelope("after halt"), &hash(7), &payload(&["alpha"]))
        .expect_err("a halted spine must refuse new appends");
    match err {
        SpineError::Halted { reason } => assert!(reason.contains("simulated physics refusal")),
        other => panic!("expected Halted, got {other:?}"),
    }
    drop(db);
    let reopened = SpineDb::open(&path).unwrap();
    assert!(
        reopened.halted().unwrap().is_some(),
        "a halted spine stays halted across processes until repaired"
    );

    // The aborted row is still there, after the halt, forever.
    assert!(reopened.find_by_hash(&h).unwrap().is_some());
}
