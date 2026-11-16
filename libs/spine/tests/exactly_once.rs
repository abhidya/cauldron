//! EXACTLY-ONCE, and the attempt lifecycle.
//!
//! The contract under test, in its own terms:
//! dedup keys on **did this LAND**, never on "was this attempted"; at most
//! ONE in-flight attempt per identity; a restart appends a FRESH row with a
//! FRESH sequence number; an ABORTED envelope may simply be resubmitted; and
//! a committed duplicate returns the ORIGINAL outcome **from the row alone**.
//!
//! # What this file adds over `door_smoke.rs`
//!
//! `door_smoke.rs` proves the happy path (same envelope twice, one landing),
//! the abort-then-resubmit path through a lock timeout, the restart, and the
//! concurrent-submission arbitration. It does not test the phrase that
//! carries the whole design — **"from the row alone"**. Three tests here
//! attack exactly that phrase: one moves the pools out from under a landed
//! identity and demands the ORIGINAL projection back; one asks from a BRAND
//! NEW PROCESS that shares no memory with the writer; and one asks after a
//! crash and a recovery, which is where the row's stored outcome is at its
//! weakest — and where it is answered with THE ORIGINAL
//! OUTCOME proven equal to a twin cauldron's live one. It also pins the hash identity's own composition, which nothing
//! else does, and walks one identity through all three dedup verdicts
//! deterministically instead of by racing threads.

#[cfg(all(unix, feature = "fault-injection"))]
#[path = "util/kill.rs"]
mod kill;

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance, TxHash};
use spine::error::SpineError;
use spine::schema_def::fixtures;
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use spine::{Cauldron, Outcome, ReadRequest, Returned};

test_scratch::install!();

// ---------------------------------------------------------------------------
// "from the row alone" — the actual claim
// ---------------------------------------------------------------------------

/// A committed duplicate is answered FROM THE ROW ALONE, and the way to prove
/// "alone" is to make the world disagree with the row: land the statement,
/// then move the pools underneath it with a second, unrelated transaction,
/// then submit the first envelope again.
///
/// A door that re-projected from the current state would hand back the MOVED
/// value. A door reading the row hands back what it returned the first time,
/// which is what "the original outcome" means and why the projection is
/// stored at the cleared flip at all.
// RED: (a) making `dedup_outcome` return `Returned::default()` instead of
// parsing `row.result` — the replayed projection came back empty and the
// equality with the original failed; (b) making `mark_cleared` store
// `json!(null)` — same failure one layer down, which is what proves this test
// is asserting on the stored projection and not on a coincidence.
#[test]
fn a_committed_duplicate_is_answered_from_the_row_alone_even_after_the_pools_have_moved() {
    let _env = env_shared();
    let dir = scratch("row-alone");
    let c = cauldron(&dir);
    let env = envelope(
        r#"CREATE (d: fixture-alpha/doc { title: "once", state: "draft" })
           RETURN d.id AS id, d.state AS state"#,
    );
    let hash = env.hash_identity().expect("hash");

    let (tx1, seq1, first) = expect_committed(c.submit(env.clone()).expect("first"), false);
    let id = first.0[0]["id"].as_str().expect("id").to_string();
    assert_eq!(first.0[0]["state"], serde_json::json!("draft"));
    let counter = c.committed_commit_counter().expect("counter");

    // THE WORLD MOVES. A different identity publishes the very doc the
    // original statement created and reported on.
    c.submit(envelope(&format!(
        r#"MATCH (d: fixture-alpha/doc {{ id: "{id}" }}) SET d.state = "published""#
    )))
    .expect("an unrelated commit moves the doc");
    assert_eq!(state_of(&c, &id).as_deref(), Some("published"));

    // AND THE ROW STILL ANSWERS.
    let (tx2, seq2, second) = expect_committed(c.submit(env).expect("second"), true);
    assert_eq!(tx1, tx2, "one hash identity");
    assert_eq!(seq1, seq2, "one row, one sequence number");
    assert_eq!(
        second, first,
        "the ORIGINAL projection, not a fresh one: the doc reads `published` now, and the \
         replayed answer still says `draft`"
    );
    assert_eq!(
        second.0[0]["state"],
        serde_json::json!("draft"),
        "stated separately because it is the whole point"
    );

    // ONE LANDING, and the record says so with no new machinery.
    assert_eq!(docs_titled(&c, "once"), 1);
    assert_eq!(
        c.committed_commit_counter().expect("counter"),
        counter + 1,
        "the duplicate moved the counter not at all; only the unrelated commit did"
    );
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    assert_eq!(
        spine.attempts_of(&hash).expect("history").len(),
        1,
        "a duplicate appends NOTHING — not even an aborted attempt"
    );
}

/// ONE IDENTITY, THE THREE DEDUP VERDICTS, in order and deterministically —
/// no threads, no timing. This is the attempt lifecycle:
///
/// | what the record holds | verdict |
/// |---|---|
/// | an un-cleared attempt | in flight — `Busy`, never a second landing |
/// | only aborted attempts | no verdict at all — a FRESH attempt proceeds |
/// | a decided, cleared row | the ORIGINAL outcome, replayed |
///
/// The middle row is the rule itself: **dedup keys on "did this LAND"**.
// RED: (a) making `dedup_outcome` answer `Some(Err(..))` for an aborted row —
// the earlier behaviour — the resubmission came back a `Usage` refusal
// instead of committing; (b) making it answer `None` for an un-cleared row —
// the in-flight submission fell through to `append_or_existing`, hit
// `idx_tx_live`, and came back as the named `Usage` refusal instead of
// `Busy`, so the verdict changed and the test caught it.
#[test]
fn one_identity_walks_the_three_dedup_verdicts_and_lands_exactly_once() {
    let _env = env_shared();
    let dir = scratch("three-verdicts");
    let c = cauldron(&dir);
    let env = envelope(
        r#"CREATE (d: fixture-alpha/doc { title: "verdicts", state: "draft" }) RETURN d.id"#,
    );
    let hash = env.hash_identity().expect("hash");
    let before = c.committed_commit_counter().expect("counter");

    // ---- VERDICT ONE: an attempt is IN FLIGHT ----
    // Opened directly on the record, which is exactly what a crashed or a
    // still-running door would have left; the door has no way to tell the two
    // apart and must not need one.
    let in_flight = {
        let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
        spine
            .append_pending(&env, &hash, &empty_payload())
            .expect("open an attempt")
    };
    match c.submit(env.clone()) {
        Err(SpineError::Busy { message }) => {
            assert!(
                message.contains("in flight"),
                "the refusal says WHY it cannot answer yet: {message}"
            );
        }
        other => panic!("an in-flight identity is Busy, never a second landing: {other:?}"),
    }
    assert_eq!(docs_titled(&c, "verdicts"), 0, "nothing landed");
    assert_eq!(
        SpineDb::open(&dir.join(SPINE_FILE))
            .expect("spine")
            .attempts_of(&hash)
            .expect("history")
            .len(),
        1,
        "and no second row was appended beside the live one"
    );

    // ---- VERDICT TWO: every attempt ABORTED, so there is no verdict ----
    {
        let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
        let row = spine.row_at(in_flight).expect("row");
        spine::recover::abort_row(&mut spine, &row).expect("abort the attempt");
    }
    let (_, landed_seq, first) = expect_committed(
        c.submit(env.clone())
            .expect("an abort landed nothing, so it is not an answer"),
        false,
    );
    assert!(
        landed_seq > in_flight,
        "the fresh attempt takes a FRESH sequence number ({landed_seq} > {in_flight})"
    );
    assert_eq!(docs_titled(&c, "verdicts"), 1);

    // ---- VERDICT THREE: decided and cleared — the original outcome ----
    let (_, replay_seq, second) = expect_committed(c.submit(env).expect("third"), true);
    assert_eq!(replay_seq, landed_seq);
    assert_eq!(second, first);

    // EXACTLY ONCE, over the whole walk.
    assert_eq!(docs_titled(&c, "verdicts"), 1, "one landing across three asks");
    assert_eq!(c.committed_commit_counter().expect("counter"), before + 1);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let attempts = spine.attempts_of(&hash).expect("history");
    assert_eq!(attempts.len(), 2, "the aborted attempt is kept forever");
    assert!(!attempts[0].decided && attempts[0].cleared, "attempt 1 aborted");
    assert!(attempts[1].decided && attempts[1].cleared, "attempt 2 landed");
    assert_eq!(attempts[1].attempt, 2);
    assert_eq!(
        spine.find_by_hash(&hash).expect("read").expect("row").seq,
        landed_seq,
        "the DECIDED row is the authoritative one for the identity"
    );
}

// ---------------------------------------------------------------------------
// the identity itself
// ---------------------------------------------------------------------------

/// WHAT THE IDENTITY IS MADE OF. Exactly-once is only as good as the hash: an
/// element left out of it silently merges two different asks into one, and an
/// element wrongly put in silently splits one ask into two. The frozen
/// algorithm names five elements — statement text, bindings, initiator,
/// instrument, nonce — and deliberately EXCLUDES mode and format version,
/// because "a validate dry run and its commit are the same ask, and a
/// re-encoded envelope is not a new transaction".
///
/// Every one of those seven claims is asserted here, in both directions.
// RED: (a) dropping the 8-byte length prefix from `hash_identity`'s
// concatenation — the statement/bindings boundary stopped being injective and
// two of the pairs below collided; (b) hashing `mode` as a sixth element —
// the commit-and-validate equality failed.
#[test]
fn the_hash_identity_is_the_five_named_elements_and_not_the_other_two() {
    let base = envelope(r#"CREATE (d: fixture-alpha/doc { title: "id", state: "draft" })"#);
    let h = |e: &Envelope| e.hash_identity().expect("hash");
    let base_hash = h(&base);

    // The five that MUST move it.
    let mut statement = base.clone();
    statement.statement = r#"CREATE (d: fixture-alpha/doc { title: "other", state: "draft" })"#.into();
    assert_ne!(h(&statement), base_hash, "the statement text is in the identity");

    let mut bindings = base.clone();
    bindings.bindings.insert("x".into(), serde_json::json!(1));
    assert_ne!(h(&bindings), base_hash, "the bindings are in the identity");

    let mut initiator = base.clone();
    initiator.provenance.initiator = Initiator::CoreMechanical {
        operation: "migration".into(),
    };
    assert_ne!(h(&initiator), base_hash, "the initiator is in the identity");

    let mut instrument = base.clone();
    instrument.provenance.instrument = Instrument {
        name: "some-other-door".into(),
        version: "9.9.9".into(),
    };
    assert_ne!(h(&instrument), base_hash, "the instrument is in the identity");

    let mut nonce = base.clone();
    nonce.idempotency_nonce = Some("n-1".into());
    assert_ne!(h(&nonce), base_hash, "the nonce is in the identity");
    let mut other_nonce = base.clone();
    other_nonce.idempotency_nonce = Some("n-2".into());
    assert_ne!(h(&other_nonce), h(&nonce), "and two nonces are two identities");

    // The two that must NOT move it.
    let mut validate = base.clone();
    validate.mode = Mode::Validate;
    assert_eq!(
        h(&validate),
        base_hash,
        "a dry run and its commit are the SAME ask"
    );
    let mut reencoded = base.clone();
    reencoded.format_version = 99;
    assert_eq!(
        h(&reencoded),
        base_hash,
        "a re-encoded envelope is not a new transaction"
    );

    // Two envelopes that differ only in where a boundary falls are two
    // identities. NEGATIVE RESULT, recorded honestly: this pair
    // does NOT prove the 8-byte length prefix necessary. Removing the prefix
    // entirely leaves this assertion green, because the three elements
    // between `statement` and `idempotency_nonce` are typed JSON with a fixed
    // shape and no caller can make them absorb a moved boundary. The prefix
    // is real defence in depth against a future element that is free text;
    // nothing reachable through today's `Envelope` can exercise it, and
    // saying so is better than a test that pretends otherwise.
    let mut a = base.clone();
    a.statement = "ab".into();
    a.idempotency_nonce = Some("c".into());
    let mut b = base.clone();
    b.statement = "a".into();
    b.idempotency_nonce = Some("bc".into());
    assert_ne!(h(&a), h(&b), "two different asks, two identities");
}

/// The nonce, END TO END: two envelopes identical in every other respect are
/// TWO transactions and land TWICE — which is exactly what the nonce is for
/// ("so a caller can distinguish two deliberate identical submissions").
/// Without this the dedup machinery would make a genuine repeat impossible to
/// express.
// RED: dropping the nonce from `hash_identity`'s element list — the second
// submission deduped against the first and only one doc landed.
#[test]
fn two_envelopes_differing_only_in_the_nonce_are_two_identities_and_land_twice() {
    let _env = env_shared();
    let dir = scratch("nonce");
    let c = cauldron(&dir);
    let before = c.committed_commit_counter().expect("counter");
    let base = envelope(
        r#"CREATE (d: fixture-alpha/doc { title: "deliberate", state: "draft" }) RETURN d.id"#,
    );

    let mut one = base.clone();
    one.idempotency_nonce = Some("nonce-one".into());
    let mut two = base.clone();
    two.idempotency_nonce = Some("nonce-two".into());

    let (tx1, seq1, _) = expect_committed(c.submit(one.clone()).expect("first"), false);
    let (tx2, seq2, _) = expect_committed(c.submit(two).expect("second"), false);
    assert_ne!(tx1, tx2, "two identities");
    assert_ne!(seq1, seq2, "two rows, two sequence numbers");
    assert_eq!(docs_titled(&c, "deliberate"), 2, "two deliberate landings");
    assert_eq!(c.committed_commit_counter().expect("counter"), before + 2);

    // And each one still dedups against ITSELF.
    let (_, seq1_again, _) = expect_committed(c.submit(one).expect("replay"), true);
    assert_eq!(seq1_again, seq1);
    assert_eq!(docs_titled(&c, "deliberate"), 2, "and not a third");
}

// ---------------------------------------------------------------------------
// "from the row alone" — across a PROCESS boundary, and across a CRASH
// ---------------------------------------------------------------------------

/// The strongest available reading of "from the row alone": the duplicate is
/// submitted by a **brand new process** that shares no memory, no connection
/// and no cache with the one that landed it — and the pools have moved so far
/// that RE-EXECUTING the statement would refuse outright (its `MATCH` looks
/// for a draft doc, and the first commit published it, so the
/// `EXPECT COUNT(d) = 1` could not hold a second time).
///
/// The dedup therefore cannot be a coincidence of re-execution. It is the row.
// RED: making `exec`'s phase-4 dedup pre-check unconditional `None` — the
// child re-executed the statement, the EXPECT failed on zero draft docs, and
// the child came back exit 3 with `kind=refused` instead of a dedup.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn a_landed_commit_is_answered_from_the_row_alone_by_a_brand_new_process() {
    let _env = kill::env_shared();
    let dir = kill::scratch("cross-process");

    // Process one lands it.
    let first = kill::run("cross-schema", &dir, None);
    assert_eq!(
        first.status.code(),
        Some(0),
        "the victim commits cleanly with no stage armed.\nstderr: {}",
        kill::stderr_of(&first)
    );
    let c = kill::open(&dir);
    assert_eq!(c.committed_commit_counter().expect("counter"), 2);
    assert_eq!(kill::ticket_count(&c), 1);
    let landed_seq = seq_of_the_cross_schema_row(&dir);

    // Process two asks again.
    let second = kill::run("resubmit", &dir, None);
    assert_eq!(
        second.status.code(),
        Some(0),
        "the duplicate is answered, not refused.\nstdout: {}\nstderr: {}",
        kill::stdout_of(&second),
        kill::stderr_of(&second)
    );
    assert_eq!(
        kill::field(&second, "deduplicated").as_deref(),
        Some("true"),
        "and answered AS a duplicate"
    );
    assert_eq!(
        kill::field(&second, "seq").as_deref(),
        Some(landed_seq.0.to_string().as_str()),
        "from the very row the first process wrote"
    );

    // Nothing landed twice.
    assert_eq!(c.committed_commit_counter().expect("counter"), 2);
    assert_eq!(kill::ticket_count(&c), 1);
    assert_eq!(kill::catalogues_hops(&c), 1);
    assert_eq!(
        kill::spine_of(&dir)
            .pending()
            .expect("pending")
            .len(),
        0
    );
}

/// AND ACROSS A CRASH — THE ORIGINAL OUTCOME. A commit killed
/// before its cleared flip, then rolled forward by recovery, is a LANDED
/// commit by every part of the record: the row is decided and cleared,
/// every part flag is true, the counter moved. The exactly-once
/// bar is that "did this already land" is answerable from the record — and
/// SETTLED that a duplicate returns THE ORIGINAL OUTCOME from the record
/// alone. So the same envelope, resubmitted, must be answered with the
/// projection the live commit would have returned.
///
/// This test used to be `#[ignore]`d (the projection "died with the process
/// that computed it"; the reply shape was an open question).
/// The question dissolved rather than got answered: the row now records
/// which nodes each MATCH binding bound, recovery projects RETURN at its
/// own cleared flip through the SAME projection the live door runs, and
/// the duplicate reads that projection. "Exactly as live" is proven by a
/// TWIN: the identical statement run in a second cauldron with no kill, and
/// the recovered duplicate's `returned` must equal the twin's. Identifiers
/// are minted deterministically from the hash (the frozen minting rule), so
/// every column is comparable by construction; nothing is excluded.
///
/// `before-cleared` is the case where EVERY part landed and only the
/// projection was lost — recovery's `cleared_only` path.
///
/// It uses the victim's REPEATABLE statement — two parts, no `MATCH`, no
/// `EXPECT` — so that nothing but exactly-once stands between one
/// submission and two (see the never-lands-twice test below for why).
// RED: (a) making `recover` clear the row with the OLD marker
// (`{"recovered": true}`) — the in-process duplicate came back `hard` and
// the cross-process one exit 3; (b) making `recover` store an EMPTY
// projection — `returned == twin` failed on zero rows; (c) making
// `project_returns` return `Returned::default()` when called from recovery
// only — the same failure, which is what proves the shared projection is
// the thing being asserted on.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn a_commit_recovered_after_a_crash_is_still_answered_from_the_row_alone() {
    let _env = kill::env_shared();
    let dir = kill::scratch("dedup-after-recovery");
    let twin = kill::scratch("dedup-after-recovery-twin");
    let (recovered, live, after) = recovered_versus_twin("repeatable", &dir, "before-cleared", &twin);

    assert!(!recovered.0.is_empty(), "the statement RETURNs, so the outcome has rows");
    assert_eq!(
        recovered, live,
        "THE ORIGINAL OUTCOME: the recovered duplicate's projection equals the twin's live one"
    );
    assert!(after.recovered, "and the row says it was recovered, as an audit fact");
    assert_eq!(
        after.result,
        Some(serde_json::to_value(&recovered).expect("json")),
        "the row's result column holds exactly that projection, never a placeholder"
    );
    assert_eq!(kill::ticket_count(&kill::open(&dir)), 1, "and nothing landed twice");
}

/// THE SAME CONTRACT, killed BETWEEN LANDINGS — the case where a part still
/// had to be rolled forward AND the RETURN names a MATCHED binding (the
/// cross-schema statement matches the seeded draft doc). The matched
/// identity is exactly the half of the record added for this: without it,
/// recovery could not project `d.id` without re-matching, which it never
/// does. The twin proves the identities agree by construction: the seed
/// doc's identifier is minted from the seed statement's hash, the ticket's
/// from the cross-schema statement's hash, and both cauldrons ran the same
/// two statements.
// RED: (a) skipping the door's bound-identity record (`record_bound` made a
// no-op) — recovery refused `hard` naming binding `d` ("records no bound
// identities"), `c.recover()` blew up; (b) making `recover` store an EMPTY
// projection — the twin equality failed. Recorded separately from the
// `before-cleared` test's reds because THIS statement matches: mutation (a)
// cannot red a statement that only CREATEs.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn a_commit_recovered_from_between_landings_is_answered_with_the_original_outcome() {
    let _env = kill::env_shared();
    let dir = kill::scratch("dedup-after-recovery-between");
    let twin = kill::scratch("dedup-after-recovery-between-twin");
    let (recovered, live, after) =
        recovered_versus_twin("cross-schema", &dir, "between-landings", &twin);

    assert_eq!(recovered.0.len(), 1, "one matched doc, one created ticket: one row");
    assert_eq!(recovered, live, "THE ORIGINAL OUTCOME, matched binding and all");
    // The matched identity really is the seeded doc's, read back from the
    // pool it lives in — not a re-match, the record.
    let c = kill::open(&dir);
    let seed_id = kill::rows(&c, r#"MATCH (d: fixture-alpha/doc) RETURN d.id, d.title"#)
        .into_iter()
        .find(|r| r["d.title"] == serde_json::json!(kill::SEED_TITLE))
        .map(|r| r["d.id"].clone())
        .expect("the seeded doc");
    assert_eq!(recovered.0[0]["doc"], seed_id);
    assert!(after.recovered);
    assert_eq!(kill::seed_doc_state(&c).as_deref(), Some("published"));
    assert_eq!(kill::ticket_count(&c), 1);
    assert_eq!(kill::catalogues_hops(&c), 1);
}

/// THE STAMP ON EXPLICIT ASK SURVIVES RECOVERY (the schema-version stamp
/// appears in no default render and IS projected on explicit ask). The cross-schema RETURN asks for it on BOTH sides —
/// the matched doc's (`d.schema_version`, read from its landed record) and
/// the created ticket's (`t.schema_version`, from the payload part's pin) —
/// and after a kill between landings the recovered duplicate projects both
/// exactly as the twin's live commit did: alpha's pin 1 and beta's pin 2.
// RED: making `project_created` answer `Null` for `schema_version` — the
// `ticket_stamp` equality with 2 failed on both the recovered and the twin
// side, which shows the assertion reaches the stamp itself and not only
// the twin equality; making `read_path` fall through for it — `doc_stamp`.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn the_schema_version_stamp_on_explicit_ask_survives_recovery_on_both_sides() {
    let _env = kill::env_shared();
    let dir = kill::scratch("stamp-after-recovery");
    let twin = kill::scratch("stamp-after-recovery-twin");
    let (recovered, live, _) = recovered_versus_twin("cross-schema", &dir, "between-landings", &twin);

    let row = &recovered.0[0];
    assert_eq!(row["doc_stamp"], serde_json::json!(1), "the matched doc: alpha's pin");
    assert_eq!(row["ticket_stamp"], serde_json::json!(2), "the created ticket: beta's pin");
    assert_eq!(row["doc_stamp"], live.0[0]["doc_stamp"], "exactly as live");
    assert_eq!(row["ticket_stamp"], live.0[0]["ticket_stamp"], "exactly as live");
}

/// The half of the recovered-duplicate contract that never depended on the
/// reply shape, and that keeps its own running test: whatever the door
/// replies, a resubmission after a crash-and-recovery must never land a
/// second time. It looks at the pools and at the record, which is where a
/// genuine exactly-once regression would show: a second doc, a second
/// ticket, a second attempt row, or a moved committed-commit counter.
///
/// The reply IS defined — the original outcome — so the ONE
/// legal exit is 0 with `deduplicated=true`; the old admission of the named
/// refusal (exit 3) is gone, because that refusal is gone.
///
/// # Why it uses the REPEATABLE statement and not the cross-schema one
///
/// The red sweep's most useful finding. Written against the cross-schema
/// statement this test could not be forced red AT ALL — not by removing the
/// door's phase-4 dedup pre-check, not by removing `append_or_existing`'s
/// decided-row query, not by dropping `idx_tx_committed`, not by all three
/// together. The cross-schema statement carries `EXPECT COUNT(d) = 1` over DRAFT docs,
/// and the first commit publishes the doc it matched, so a second execution
/// refuses on its own premises long before any dedup machinery is consulted.
/// The test was green for a reason that had nothing to do with exactly-once,
/// which is the definition of a checker that does not exist. The victim
/// therefore grew a two-part statement with no `MATCH` and no `EXPECT`, and
/// with it this test reds under the three-belt mutation below.
// RED: removing the door's phase-4 dedup pre-check AND
// `append_or_existing`'s decided-row query AND `idx_tx_committed` — the
// resubmitted envelope re-executed, appended attempt 2, landed a second doc
// and a second ticket, and the counter moved to 2.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn a_recovered_commit_never_lands_twice_however_the_duplicate_is_answered() {
    let _env = kill::env_shared();
    let dir = kill::scratch("recovered-no-second-landing");
    kill::kill_at("repeatable", &dir, "before-cleared");

    let c = kill::open(&dir);
    let row = kill::the_stuck_row(&dir);
    c.recover().expect("recovery clears it");
    assert_eq!(c.committed_commit_counter().expect("counter"), 1);
    assert_eq!(kill::ticket_count(&c), 1);
    assert_eq!(kill::docs_titled(&c, "repeatable"), 1);

    // Ask again, from a fresh process. THAT the child got as far as a reply
    // is asserted, and it has to be: a child that died on startup — a stale
    // victim, a cauldron it could not open, an unknown job — would make
    // every "nothing landed twice" assertion below vacuously true. A
    // duplicate that never reached the door proves nothing about
    // exactly-once. And since the reply is defined, exactly one is legal.
    let out = kill::run("resubmit-repeatable", &dir, None);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the child reached the door and was ANSWERED as a duplicate; any other \
         status is a refusal, a death, or a child that never submitted.\nstdout: {}\nstderr: {}",
        kill::stdout_of(&out),
        kill::stderr_of(&out)
    );
    assert_eq!(kill::field(&out, "deduplicated").as_deref(), Some("true"));
    assert_eq!(
        kill::field(&out, "seq").as_deref(),
        Some(row.seq.0.to_string().as_str())
    );

    assert_eq!(
        c.committed_commit_counter().expect("counter"),
        1,
        "the duplicate did not move the committed-commit counter"
    );
    assert_eq!(kill::ticket_count(&c), 1, "and landed no second ticket");
    assert_eq!(kill::docs_titled(&c, "repeatable"), 1, "nor a second doc");
    assert_eq!(kill::catalogues_hops(&c), 1);
    let spine = kill::spine_of(&dir);
    let after = spine.row_at(row.seq).expect("row");
    assert_eq!(
        spine.attempts_of(&after.hash).expect("history").len(),
        1,
        "and appended no second attempt at the identity"
    );
    assert!(after.decided && after.cleared);
    assert!(spine.pending().expect("pending").is_empty());
}

/// THE RECOVERED-VERSUS-TWIN HARNESS. Kill `job` at `stage` in `dir`, run
/// recovery, and answer the duplicate THREE ways — from a brand-new process
/// (the victim's resubmit job), in-process through the door, and from the
/// row's stored `result` — all of which must agree. Then run the identical
/// `job` in `twin` with NO kill, and answer ITS duplicate in-process. Returns
/// (the recovered duplicate's projection, the twin's live projection, the
/// recovered row as it stands after recovery).
///
/// The in-process envelopes are rebuilt FROM THE ROW (statement, bindings,
/// initiator, instrument, format version — the row is self-describing)
/// and their hash identity is asserted equal to the row's, so a
/// drift between this file and the victim's envelope shape is loud rather
/// than a silently different identity. The victim sets no idempotency
/// nonce, which the row does not record; the hash assertion is what makes
/// that assumption checked rather than trusted.
#[cfg(all(unix, feature = "fault-injection"))]
fn recovered_versus_twin(
    job: &str,
    dir: &std::path::Path,
    stage: &str,
    twin: &std::path::Path,
) -> (Returned, Returned, spine::spine_db::TxRow) {
    kill::kill_at(job, dir, stage);
    let c = kill::open(dir);
    let stuck = kill::the_stuck_row(dir);
    let report = c.recover().expect("recovery runs");
    match stage {
        "before-cleared" => assert_eq!(report.cleared_only, vec![stuck.hash]),
        _ => assert_eq!(report.rolled_forward, vec![stuck.hash]),
    }
    let after = kill::spine_of(dir).row_at(stuck.seq).expect("row");
    assert!(
        after.decided && after.cleared && after.parts.iter().all(|p| p.landed),
        "by the record this identity LANDED"
    );

    // ONE: from a brand-new process.
    let resubmit = match job {
        "repeatable" => "resubmit-repeatable",
        "cross-schema" => "resubmit",
        other => panic!("no resubmit job for `{other}`"),
    };
    let out = kill::run(resubmit, dir, None);
    assert_eq!(
        out.status.code(),
        Some(0),
        "a landed identity is answered, never failed.\nstdout: {}\nstderr: {}",
        kill::stdout_of(&out),
        kill::stderr_of(&out)
    );
    assert_eq!(kill::field(&out, "deduplicated").as_deref(), Some("true"));
    assert_eq!(
        kill::field(&out, "seq").as_deref(),
        Some(stuck.seq.0.to_string().as_str())
    );

    // TWO: in-process, through the door, with the envelope rebuilt from the row.
    let env = envelope_of_row(&after);
    let (tx, seq, recovered) = expect_committed(c.submit(env).expect("answered"), true);
    assert_eq!(tx, after.hash);
    assert_eq!(seq, after.seq, "from the very row recovery cleared");

    // THREE: the stored result column is that same projection.
    assert_eq!(after.result, Some(serde_json::to_value(&recovered).expect("json")));

    // THE TWIN: no kill, same job, same statements, same hash identities.
    let live_out = kill::run(job, twin, None);
    assert_eq!(
        live_out.status.code(),
        Some(0),
        "the twin commits cleanly with no stage armed.\nstderr: {}",
        kill::stderr_of(&live_out)
    );
    let twin_spine = kill::spine_of(twin);
    let twin_row = twin_spine
        .find_by_hash(&after.hash)
        .expect("read")
        .expect("the twin landed the same hash identity");
    assert!(!twin_row.recovered, "the twin was never recovered");
    let twin_c = kill::open(twin);
    let (_, _, live) = expect_committed(
        twin_c.submit(envelope_of_row(&twin_row)).expect("answered"),
        true,
    );
    assert_eq!(
        twin_row.result,
        Some(serde_json::to_value(&live).expect("json")),
        "the twin's stored projection is what its duplicate replays"
    );
    (recovered, live, after)
}

/// The envelope a row records, rebuilt so its hash identity is the row's
/// (asserted). See `recovered_versus_twin` for the nonce caveat.
#[cfg(all(unix, feature = "fault-injection"))]
fn envelope_of_row(row: &spine::spine_db::TxRow) -> Envelope {
    let env = Envelope {
        statement: row.statement.clone(),
        bindings: row.bindings.clone(),
        format_version: row.envelope_version,
        provenance: Provenance {
            initiator: row.initiator.clone(),
            instrument: row.instrument.clone(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    };
    assert_eq!(
        env.hash_identity().expect("hash"),
        row.hash,
        "the envelope rebuilt from the row must BE the row's identity (a nonce would break this)"
    );
    env
}

/// The cross-schema row's sequence number, for the cross-process tests. It is
/// the only two-part row in a victim's cauldron.
#[cfg(all(unix, feature = "fault-injection"))]
fn seq_of_the_cross_schema_row(dir: &std::path::Path) -> Seq {
    let spine = kill::spine_of(dir);
    let conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("direct SQL");
    let seq: i64 = conn
        .query_row(
            "SELECT seq FROM tx_parts GROUP BY seq HAVING COUNT(*) = 2",
            [],
            |r| r.get(0),
        )
        .expect("exactly one two-part row");
    let _ = &spine;
    Seq(seq)
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

static ENV: std::sync::RwLock<()> = std::sync::RwLock::new(());

fn env_shared() -> std::sync::RwLockReadGuard<'static, ()> {
    ENV.read().unwrap_or_else(|e| e.into_inner())
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = test_scratch::tmp_path(format!("spine-once-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn cauldron(dir: &std::path::Path) -> Cauldron {
    Cauldron::create(
        dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        SpineConfig::default(),
    )
    .expect("create the cauldron")
}

fn envelope(statement: &str) -> Envelope {
    Envelope {
        statement: statement.to_string(),
        bindings: Bindings::new(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent {
                thread: "exactly-once-suite".into(),
                dispatch: "exactly-once".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn expect_committed(out: Outcome, want_dedup: bool) -> (TxHash, Seq, Returned) {
    match out {
        Outcome::Committed {
            tx,
            seq,
            returned,
            deduplicated,
        } => {
            assert_eq!(deduplicated, want_dedup, "the dedup flag is part of the answer");
            (tx, seq, returned)
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn empty_payload() -> spine::delta::CompiledPayload {
    spine::delta::CompiledPayload {
        parts: Vec::new(),
        minted: std::collections::BTreeMap::new(),
    }
}

fn rows(
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

fn docs_titled(c: &Cauldron, title: &str) -> usize {
    rows(c, r#"MATCH (d: fixture-alpha/doc) RETURN d.title"#)
        .iter()
        .filter(|r| r["d.title"] == serde_json::json!(title))
        .count()
}

fn state_of(c: &Cauldron, id: &str) -> Option<String> {
    rows(c, r#"MATCH (d: fixture-alpha/doc) RETURN d.id, d.state"#)
        .into_iter()
        .find(|r| r["d.id"] == serde_json::json!(id))
        .and_then(|r| r["d.state"].as_str().map(str::to_string))
}
