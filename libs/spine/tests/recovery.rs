//! RECOVERY, and the FAIL-CLOSED HALT inside it.
//!
//! # What this file adds over `door_smoke.rs`
//!
//! `door_smoke.rs` proves deltas-not-text by APPENDING a row whose statement
//! text was already garbage. That shows recovery survives an unparseable
//! text; it cannot show the real hazard, which is the opposite
//! failure — *"re-matching after a crash could match different rows and
//! silently do different work."* The tests here poison the text AFTER the
//! decided flip, by direct SQL on the spine file, on a row a REAL crash left
//! behind; and one of the two poisons is a perfectly VALID statement that
//! would do different work silently. A recovery that re-matched would pass
//! `door_smoke`'s version of this test and fail this one.
//!
//! It also proves fail-closed against a spine append that PHYSICS refuses,
//! rather than against a halt flag set by hand.
//!
//! Recovery also PROJECTS RETURN at its cleared flip, from the
//! row's record (parsed text — parsing is not matching — plus the payload
//! and the recorded bound identities). Two tests here police that record:
//! a decided row with no record for a matched binding refuses loudly, and a
//! recorded node that no longer reads refuses loudly. Neither ever projects
//! an empty row in place of the original outcome.
//!
//! # Direct SQL on the spine
//!
//! Legal for a test, and necessary: nothing public can rewrite a row, and
//! rightly so. Every such write is in `util/kill.rs`, named `poison_*`, and
//! touches only the columns it says it touches.

#[cfg(all(unix, feature = "fault-injection"))]
#[path = "util/kill.rs"]
mod kill;

use spine::config::SpineConfig;
use spine::delta::{CompiledPart, CompiledPayload};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance, TxHash};
use spine::error::SpineError;
use spine::locks::{acquire, queue, Claim, ClaimSet, LockMode, Resource};
use spine::pool::NodeId;
use spine::schema_def::{fixtures, PoolName};
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use spine::{Cauldron, Outcome, ReadRequest};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

test_scratch::install!();

// ---------------------------------------------------------------------------
// roll-forward replays the COMPILED DELTAS, never the text
// ---------------------------------------------------------------------------

/// POISON ONE: after a real crash at the decided flip, the row's statement
/// text is replaced — by direct SQL, on the row already on disk — with
/// something that could never parse. Recovery must never MATCH it; it
/// PARSES it, for the RETURN projection it computes at its own
/// cleared flip (parsing is deterministic and is not matching), and a text
/// that does not parse on a DECIDED row is an altered record: the door
/// parsed it before the append.
///
/// The rule this asserts (a derived design choice):
/// the parts still LAND — the landing is the commit and never depended on
/// the text — and then recovery refuses LOUDLY, by name, before the cleared
/// flip. Nothing is fabricated, nothing is re-matched, the row stays
/// decided and un-cleared with its locks held, every later pass refuses the
/// same way, and repairing the record out-of-band lets the next pass finish
/// with a real projection. Fail-closed.
// RED: (a) making `recover` re-match the text instead of replaying the
// payload — recovery failed BEFORE landing (nothing landed, the
// "everything landed" assertions failed); (b) making `project_recovered`
// swallow a parse failure into an empty projection — recovery returned Ok,
// the row cleared, and the `expect_err` below panicked; (c) skipping the
// landing loop when the projection cannot be computed — the pools stayed
// empty. Re-run after RETURN projection was added: still red under (a).
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn roll_forward_never_re_matches_a_statement_text_poisoned_after_the_decided_flip() {
    let _env = kill::env_shared();
    let dir = kill::scratch("poison-unparseable");
    kill::kill_at("cross-schema", &dir, "decided");

    let row = kill::the_stuck_row(&dir);
    assert!(row.decided && !row.cleared);
    assert!(row.parts.iter().all(|p| !p.landed));
    let original_statement = row.statement.clone();
    let original_bindings = serde_json::to_string(&row.bindings).expect("json");
    let locks_held = kill::lock_rows(&dir, row.seq);
    assert!(locks_held > 0, "the killed commit still holds its locks");

    // THE POISON, applied to a row whose payload is already durable.
    kill::poison_statement(&dir, row.seq, "!!! this text never parsed and never will !!!");
    kill::poison_bindings(&dir, row.seq, r#"{"nonsense": true}"#);
    assert_eq!(
        kill::spine_of(&dir).row_at(row.seq).expect("row").statement,
        "!!! this text never parsed and never will !!!",
        "the poison is really on the row recovery is about to read"
    );

    let c = kill::open(&dir);
    let err = c
        .recover()
        .expect_err("a decided row whose text no longer parses refuses LOUDLY");
    assert_eq!(err.kind(), "hard");
    let message = err.message();
    assert!(
        message.contains("does not parse") && message.contains(&row.seq.to_string()),
        "it names the row and the reason: {message}"
    );

    // THE PARTS LANDED ANYWAY — the payload won, and the text was never
    // matched: the doc is published and the ticket exists, observed POOL BY
    // POOL. The cross-pool hop is deliberately NOT read here: the row is
    // decided and un-cleared with its locks held, so a cross-pool validated
    // read of the hop is invalidated by design (validated-read step 3), escalates,
    // queues behind the held locks and comes back `Busy` at the ceiling —
    // `catalogues_hops`'s own doc says never to call it inside the window
    // (this test was once found doing exactly that). The hop is
    // asserted once the record is repaired and the row cleared, below, by
    // `assert_all_or_nothing`.
    let o = kill::observe(&c);
    assert!(kill::everything_landed(&o), "the deltas landed from the payload: {o:?}");
    let after = kill::spine_of(&dir).row_at(row.seq).expect("row");
    assert!(after.decided, "still decided");
    assert!(after.parts.iter().all(|p| p.landed), "every part flag landed");
    assert!(!after.cleared, "and NOT cleared: the projection could not be computed");
    assert!(!after.recovered);
    assert_eq!(after.result, None, "nothing fabricated into the result column");
    assert_eq!(
        kill::lock_rows(&dir, row.seq),
        locks_held,
        "the locks are still held — release is at the cleared flip only"
    );
    // The poisoned text is STILL THERE: recovery neither consults it for
    // landing nor repairs it (the row records what was asked).
    assert_eq!(after.statement, "!!! this text never parsed and never will !!!");

    // A SECOND PASS refuses the same way and lands nothing twice.
    let again = c.recover().expect_err("still refused");
    assert_eq!(again.kind(), "hard");
    assert_eq!(kill::ticket_count(&c), 1, "idempotent even while refusing");

    // REPAIR OUT-OF-BAND, and the next pass finishes: a real projection,
    // the cleared flip, the recovered mark, the locks released.
    kill::poison_statement(&dir, row.seq, &original_statement);
    kill::poison_bindings(&dir, row.seq, &original_bindings);
    let report = c.recover().expect("repaired record: recovery completes");
    assert_eq!(report.cleared_only, vec![row.hash], "every part had landed; only the clear was owed");
    let repaired = kill::spine_of(&dir).row_at(row.seq).expect("row");
    kill::assert_all_or_nothing(&c, &repaired, "decided (text poisoned, then repaired)");
    assert!(repaired.recovered);
    let projection = repaired.result.expect("a projection");
    assert_eq!(projection[0]["doc_stamp"], serde_json::json!(1));
    assert_eq!(projection[0]["ticket_stamp"], serde_json::json!(2));
    assert!(projection[0]["ticket"].is_string() && projection[0]["doc"].is_string());
    assert_eq!(kill::lock_rows(&dir, row.seq), 0, "released at the cleared flip");
}

/// POISON TWO, and the one that matters: the statement text is replaced with
/// a **perfectly valid statement that would do DIFFERENT WORK**. This is the
/// exact hazard recovery must avoid — *"re-matching after a crash could match
/// different rows and silently do different work"* — and it is the only form
/// of the poison that a re-matching recovery would fail SILENTLY rather than
/// loudly.
///
/// The swapped statement would set the matched doc to `superseded` and create
/// nothing at all. The row's compiled payload sets it to `published` and
/// creates a ticket and a cross-pool edge. After recovery the two are
/// distinguishable in every direction.
// RED: making `recover::recover` compile and run `row.statement` instead of
// replaying `row.payload` — the doc came back `superseded`, beta stayed empty,
// and recovery reported success the whole time. That silent pass is precisely
// what this test exists to make impossible.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn roll_forward_ignores_a_statement_swapped_for_one_that_would_do_different_work() {
    let _env = kill::env_shared();
    let dir = kill::scratch("poison-plausible");
    kill::kill_at("cross-schema", &dir, "decided");

    let row = kill::the_stuck_row(&dir);
    let original_payload = row.payload.clone();
    kill::poison_statement(
        &dir,
        row.seq,
        r#"MATCH (d: fixture-alpha/doc { state: "draft" })
           SET d.state = "superseded""#,
    );

    let c = kill::open(&dir);
    let report = c.recover().expect("recovery runs");
    assert_eq!(report.rolled_forward, vec![row.hash]);

    // THE DELTAS WON, and every observable says so.
    let o = kill::observe(&c);
    assert_eq!(
        o.doc_state.as_deref(),
        Some("published"),
        "the PAYLOAD's SET landed; the swapped statement's `superseded` did not"
    );
    assert_eq!(
        o.tickets, 1,
        "the payload's beta half landed — the swapped statement creates nothing, \
         so a re-matching recovery would have left beta empty"
    );
    assert_eq!(kill::catalogues_hops(&c), 1, "and the cross-pool edge with it");

    let after = kill::spine_of(&dir).row_at(row.seq).expect("row");
    kill::assert_all_or_nothing(&c, &after, "decided (text swapped)");
    assert_eq!(
        after.payload, original_payload,
        "the payload — the thing recovery actually reads — was never touched"
    );
    // The projection was computed from the SWAPPED text's RETURN — it has
    // none, so the projection is empty — over the PAYLOAD's landed state;
    // the text was parsed and never matched. The row is recovered, and its
    // result is a projection, not a marker.
    assert!(after.recovered);
    assert_eq!(after.result, Some(serde_json::json!([])));
}

/// THE RECORD, OR NOTHING (both configurations). A decided row
/// whose RETURN names a MATCHED binding that the row's bound-identity
/// record does not carry cannot be projected without re-matching — which
/// recovery never does — so it refuses LOUDLY naming the binding, and the
/// row stays un-cleared. The two contrasts that keep that refusal honest: a
/// record saying the binding bound NOTHING projects zero rows and clears;
/// a record naming the real node projects it. Staged by hand: a live doc,
/// then a decided row with a `MATCH ... RETURN` text, zero parts, and no
/// record — the shape a row appended before bound identities were recorded
/// would have.
// RED: (a) making `project_landed` treat a missing binding as an empty
// match (dropping the "records no bound identities" refusal) — the first
// `expect_err` panicked on an Ok with `[]`; (b) making `read_bound` return
// `Some(empty)` for a missing row — same; (c) making `project_landed` skip
// the pool re-read and project from nothing — the third contrast's title
// came back null.
#[test]
fn recovery_refuses_a_matched_return_whose_bound_identities_were_never_recorded() {
    let _env = env_shared();
    let dir = scratch("bound-record-missing");
    let c = cauldron(&dir);
    let doc = match submit(
        &c,
        r#"CREATE (d: fixture-alpha/doc { title: "recorded", state: "draft" }) RETURN d.id"#,
    )
    .expect("a live doc")
    {
        Outcome::Committed { returned, .. } => returned.0[0]["d.id"]
            .as_str()
            .expect("id")
            .to_string(),
        other => panic!("{other:?}"),
    };

    let env = envelope(r#"MATCH (d: fixture-alpha/doc) RETURN d.title"#);
    let hash = env.hash_identity().expect("hash");
    let seq = {
        let mut spine = spine_of(&dir);
        let seq = spine.append_pending(&env, &hash, &payload(&[])).expect("append");
        spine.mark_decided(seq).expect("decided");
        seq
    };

    // NO RECORD: loud, named, un-cleared.
    let err = c.recover().expect_err("no bound identities for `d`: refuse, never project nothing");
    assert_eq!(err.kind(), "hard");
    let message = err.message();
    assert!(
        message.contains("`d`") && message.contains("no bound identities"),
        "it names the binding: {message}"
    );
    let row = spine_of(&dir).row_at(seq).expect("row");
    assert!(row.decided && !row.cleared && !row.recovered && row.result.is_none());

    // CONTRAST ONE: a record saying `d` bound NOTHING — zero rows, cleared.
    let sql = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("direct SQL");
    sql.execute_batch(spine::exec::BOUND_DDL).expect("the table, by its own DDL");
    let empty: spine::delta::BoundIdentities = [("d".to_string(), Vec::new())].into();
    sql.execute(
        &format!("INSERT OR REPLACE INTO {} (seq, bound) VALUES (?1, ?2)", spine::exec::BOUND_TABLE),
        rusqlite::params![seq.0, serde_json::to_string(&empty).expect("json")],
    )
    .expect("stage the record");
    let report = c.recover().expect("an empty match projects zero rows");
    assert_eq!(report.cleared_only, vec![hash]);
    let row = spine_of(&dir).row_at(seq).expect("row");
    assert!(row.cleared && row.recovered);
    assert_eq!(row.result, Some(serde_json::json!([])));

    // CONTRAST TWO: a fresh decided row whose record names the real node —
    // it projects the node's CURRENT title, read from the pool.
    let env2 = envelope(r#"MATCH (d: fixture-alpha/doc) RETURN d.title AS title"#);
    let hash2 = env2.hash_identity().expect("hash");
    let seq2 = {
        let mut spine = spine_of(&dir);
        let seq = spine.append_pending(&env2, &hash2, &payload(&[])).expect("append");
        spine.mark_decided(seq).expect("decided");
        seq
    };
    let named: spine::delta::BoundIdentities = [(
        "d".to_string(),
        vec![spine::pool::QualifiedNodeId {
            pool: PoolName::new(ALPHA),
            id: NodeId(doc.clone()),
        }],
    )]
    .into();
    sql.execute(
        &format!("INSERT OR REPLACE INTO {} (seq, bound) VALUES (?1, ?2)", spine::exec::BOUND_TABLE),
        rusqlite::params![seq2.0, serde_json::to_string(&named).expect("json")],
    )
    .expect("stage the record");
    let report = c.recover().expect("a recorded binding projects");
    assert_eq!(report.cleared_only, vec![hash2]);
    let row = spine_of(&dir).row_at(seq2).expect("row");
    assert!(row.cleared && row.recovered);
    assert_eq!(row.result, Some(serde_json::json!([{ "title": "recorded" }])));
    // And the duplicate replays exactly that, through the door.
    match c.submit(env2).expect("answered") {
        Outcome::Committed {
            returned,
            deduplicated,
            seq,
            ..
        } => {
            assert!(deduplicated);
            assert_eq!(seq, seq2);
            assert_eq!(returned.0[0]["title"], serde_json::json!("recorded"));
        }
        other => panic!("{other:?}"),
    }
}

/// A BOUND NODE THAT NO LONGER READS IS A LOUD `Hard`, never a silent empty
/// projection (both configurations). Nodes are never removed
/// (REMOVE retires, it does not delete), so a recorded identity that reads back as nothing
/// is a broken invariant of the store; recovery names it and leaves the row
/// un-cleared rather than projecting a row short.
// RED: restoring the live door's old fallback (`record.unwrap_or_else(||
// pre-landing record)`) as an empty-projection fallback in `project_landed`
// — recovery returned Ok with `[]` and the `expect_err` panicked.
#[test]
fn a_bound_node_that_no_longer_reads_refuses_recovery_loudly() {
    let _env = env_shared();
    let dir = scratch("bound-node-missing");
    let c = cauldron(&dir);
    let env = envelope(r#"MATCH (d: fixture-alpha/doc) RETURN d.title"#);
    let hash = env.hash_identity().expect("hash");
    let seq = {
        let mut spine = spine_of(&dir);
        let seq = spine.append_pending(&env, &hash, &payload(&[])).expect("append");
        spine.mark_decided(seq).expect("decided");
        seq
    };
    let sql = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("direct SQL");
    sql.execute_batch(spine::exec::BOUND_DDL).expect("the table");
    let ghostly: spine::delta::BoundIdentities = [(
        "d".to_string(),
        vec![spine::pool::QualifiedNodeId {
            pool: PoolName::new(ALPHA),
            id: NodeId("n-never-existed".into()),
        }],
    )]
    .into();
    sql.execute(
        &format!("INSERT INTO {} (seq, bound) VALUES (?1, ?2)", spine::exec::BOUND_TABLE),
        rusqlite::params![seq.0, serde_json::to_string(&ghostly).expect("json")],
    )
    .expect("stage the record");

    let err = c.recover().expect_err("a node that does not read is a broken invariant");
    assert_eq!(err.kind(), "hard");
    let message = err.message();
    assert!(
        message.contains("n-never-existed") && message.contains("no longer reads"),
        "it names the node: {message}"
    );
    let row = spine_of(&dir).row_at(seq).expect("row");
    assert!(row.decided && !row.cleared && row.result.is_none());
}

/// RECOVERY IS IDEMPOTENT (run it twice, same result). Run three
/// times, each through a FRESH `Cauldron` handle, from the hardest starting
/// point there is — a half-landed cross-file commit. Everything observable
/// must be stable after the first pass: the pools, the row, the per-part
/// flags, the committed-commit counter, the lock table, and the report itself.
// RED twice.
// (a) Making roll-forward skip its own cleared flip when it rolled anything
//     (`if !rolled { mark_cleared(..) }`) — pass two then re-walked the
//     still-un-cleared row and reported `cleared_only`, so the report
//     equality failed on "pass 2 found nothing to do". This is the mutation
//     that reaches the idempotence assertions themselves.
// (b) Widening `SpineDb::pending`'s worklist to the whole table
//     (`SELECT seq FROM transactions ORDER BY seq`) plus re-applying already
//     landed parts — red at the precondition rather than at the idempotence
//     assertions (the worklist then holds two rows), so it is recorded as the
//     weaker of the two.
//
// NEGATIVE RESULT, recorded: the re-apply mutation ALONE flips nothing, and
// neither does widening the worklist to `cleared = 0 OR decided = 0`. A
// settled row is decided AND cleared, so it satisfies neither clause. The
// real guard is `pending()`'s predicate, with the pool-side `applied_parts`
// marker behind it as a second one — defence in depth, and either alone
// holds the line.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn recovery_is_idempotent_across_repeated_runs_and_fresh_handles() {
    let _env = kill::env_shared();
    let dir = kill::scratch("idempotent");
    kill::kill_at("cross-schema", &dir, "between-landings");
    let row = kill::the_stuck_row(&dir);

    let first = kill::open(&dir).recover().expect("first pass");
    assert_eq!(first.rolled_forward, vec![row.hash]);

    let settled = kill::observe(&kill::open(&dir));
    let settled_row = kill::spine_of(&dir).row_at(row.seq).expect("row");
    let settled_counter = kill::open(&dir)
        .committed_commit_counter()
        .expect("counter");
    assert_eq!(settled_counter, 2);

    for pass in 2..=3 {
        let c = kill::open(&dir);
        assert_eq!(
            c.recover().expect("a later pass"),
            spine::recover::RecoveryReport::default(),
            "pass {pass} found nothing to do"
        );
        assert_eq!(kill::observe(&c), settled, "pass {pass} changed no pool");
        assert_eq!(
            c.committed_commit_counter().expect("counter"),
            settled_counter,
            "pass {pass} moved the committed-commit counter"
        );
        assert_eq!(kill::catalogues_hops(&c), 1, "pass {pass} duplicated an edge");
        assert_eq!(kill::lock_rows(&dir, row.seq), 0);
        let now = kill::spine_of(&dir).row_at(row.seq).expect("row");
        assert_eq!(now.decided, settled_row.decided);
        assert_eq!(now.cleared, settled_row.cleared);
        assert_eq!(now.parts, settled_row.parts, "pass {pass} moved a landed flag");
    }
}

/// ABORT-BEFORE-DECIDED KEEPS THE ROW — and keeps the WHOLE row. "The row
/// remains as the record of the aborted attempt" is a
/// claim about a self-describing record, not about a surviving flag: the
/// statement that was asked, its bindings, its provenance stamps, its
/// compiled payload and its per-part pool and validated-under stamps are all
/// still there after recovery has written the attempt off.
// RED: (a) making `recover::abort_row` DELETE the row — every assertion below
// failed at `row_at`; (b) making `abort_row` clear the payload column first
// ("it will never be replayed") — the payload assertions failed, which is the
// point: an audit reading this row in ten years must still be able to see
// what was going to happen.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn an_abort_before_the_decided_flip_lands_nothing_and_keeps_the_whole_record() {
    let _env = kill::env_shared();
    let dir = kill::scratch("abort-record");
    kill::kill_at("cross-schema", &dir, "row-appended");

    let before = kill::the_stuck_row(&dir);
    let c = kill::open(&dir);
    let report = c.recover().expect("recovery runs");
    assert_eq!(report.aborted, vec![before.hash]);

    let after = kill::spine_of(&dir).row_at(before.seq).expect("the row is kept");
    assert!(!after.decided, "an aborted attempt never decided");
    assert!(after.cleared, "and it has left the pending worklist");
    assert!(
        after.parts.iter().all(|p| !p.landed),
        "its landed flags stay false — the abort is itself a recorded fact"
    );
    assert!(kill::nothing_landed(&kill::observe(&c)));

    // THE WHOLE RECORD, unchanged.
    assert_eq!(after.statement, before.statement);
    assert!(
        after.statement.contains("catalogues"),
        "the statement that was ASKED is still legible"
    );
    assert_eq!(after.bindings, before.bindings);
    assert_eq!(after.payload, before.payload, "the compiled deltas survive");
    assert_eq!(after.initiator, before.initiator);
    assert_eq!(after.instrument, before.instrument);
    assert_eq!(after.envelope_version, before.envelope_version);
    assert_eq!(after.timestamp, before.timestamp);
    assert_eq!(after.attempt, 1);
    assert_eq!(
        after
            .parts
            .iter()
            .map(|p| (p.pool.as_str().to_string(), p.schema_version))
            .collect::<Vec<_>>(),
        vec![
            (kill::ALPHA.to_string(), 1),
            (kill::BETA.to_string(), 2)
        ],
        "the per-part pool and validated-under stamps survive the abort too"
    );

    // Idempotent over aborts: the second pass has nothing to say.
    assert_eq!(
        c.recover().expect("twice"),
        spine::recover::RecoveryReport::default()
    );
}

// ---------------------------------------------------------------------------
// FAIL-CLOSED-AND-HALT
// ---------------------------------------------------------------------------

/// FAIL-CLOSED, with the spine append refused by PHYSICS rather than by a
/// halt flag set by hand: the spine's bookkeeping table is removed by direct
/// SQL, so the append fails part-way through its own atomic step. The
/// transaction never happened, the cauldron halts loudly into an effectively
/// read-only state, the halt is recorded DURABLY, and every subsequent write
/// refuses NAMING it — including after the underlying fault is repaired,
/// because fail-closed means repaired out-of-band, not repaired by luck.
// RED: (a) removing the `Err(SpineError::Hard { .. })` arm from `exec`'s
// phase 8 so the failure propagated as a bare `Hard` — `c.halted()` came back
// `None` and the cauldron happily accepted the next write; (b) dropping the
// halt gate from `exec::execute`'s phase 1 — writes continued after the halt
// was recorded.
#[test]
fn a_spine_append_that_physics_refuses_halts_the_cauldron_and_later_writes_refuse_by_name() {
    let _env = env_shared();
    let name = "fail-closed";
    let dir = scratch(name);
    let c = cauldron(&dir);
    let landed_before = c.committed_commit_counter().expect("counter");
    submit(&c, r#"CREATE (d: fixture-alpha/doc { title: "before", state: "draft" })"#)
        .expect("an ordinary commit, before the fault");

    // PHYSICS REFUSES. `tx_parts` is where the append writes its per-part
    // rows, inside the same atomic step as the transaction row; without it
    // the append cannot complete and cannot half-complete either.
    let sql = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("direct SQL");
    sql.execute_batch("DROP TABLE tx_parts;").expect("poison");
    drop(sql);

    let err = submit(
        &c,
        r#"CREATE (d: fixture-alpha/doc { title: "during", state: "draft" })"#,
    )
    .expect_err("the append cannot succeed");
    let reason = match &err {
        SpineError::Halted { reason } => reason.clone(),
        other => panic!("a refused spine append is fail-closed-and-HALT, got {other:?}"),
    };
    assert!(
        reason.contains("the spine append failed"),
        "the halt names its cause: {reason}"
    );

    // NO EFFECT EXISTS WITHOUT ITS RECORD: the row rolled back with the part
    // rows it could not write, and nothing reached the pool.
    let sql = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("direct SQL");
    let rows: i64 = sql
        .query_row("SELECT COUNT(*) FROM transactions WHERE cleared = 0", [], |r| r.get(0))
        .expect("count");
    assert_eq!(rows, 0, "the failed append left no row behind");
    // Restore the table so the cauldron is READABLE again — and watch the
    // halt survive the repair.
    sql.execute_batch(
        "CREATE TABLE tx_parts (
           seq INTEGER NOT NULL, part INTEGER NOT NULL, pool TEXT NOT NULL,
           schema_version INTEGER NOT NULL, landed INTEGER NOT NULL DEFAULT 0,
           PRIMARY KEY (seq, part)) WITHOUT ROWID;
         CREATE INDEX idx_tx_parts_pool ON tx_parts(pool, landed);",
    )
    .expect("restore");
    drop(sql);

    assert_eq!(
        c.committed_commit_counter().expect("counter"),
        landed_before + 1,
        "only the commit from before the fault"
    );
    assert_eq!(docs_titled(&c, "during"), 0, "nothing landed");

    // HALTED, DURABLY.
    let halted = c.halted().expect("halted").expect("the halt was recorded");
    assert!(halted.contains("the spine append failed"));

    // EVERY SUBSEQUENT WRITE REFUSES, NAMING THE HALT — including one that
    // could never even parse, which is how this test tells the door's phase-1
    // gate from a failure four phases later.
    for statement in [
        r#"CREATE (d: fixture-alpha/doc { title: "after", state: "draft" })"#,
        r#"CREATE (t: fixture-beta/ticket { state: "open" })"#,
        "!!! this is not ladle !!!",
    ] {
        match submit(&c, statement) {
            Err(SpineError::Halted { reason }) => {
                assert!(reason.contains("the spine append failed"), "{reason}")
            }
            other => panic!("a halted cauldron refuses `{statement}`, got {other:?}"),
        }
    }
    assert_eq!(docs_titled(&c, "after"), 0);

    // EFFECTIVELY READ-ONLY, not dead: the read door still answers, and the
    // halt survives a fresh handle on the directory — it is repaired
    // out-of-band or not at all.
    assert_eq!(docs_titled(&c, "before"), 1);
    let reopened = Cauldron::open(&dir, SpineConfig::default()).expect("reopen");
    assert!(
        reopened.halted().expect("halted").is_some(),
        "the halt is durable: reopening the cauldron does not lift it"
    );
    assert!(matches!(
        submit(&reopened, r#"CREATE (d: fixture-alpha/doc { title: "reopened", state: "draft" })"#),
        Err(SpineError::Halted { .. })
    ));
}

/// The NO-EFFECT half of the same fail-closed rule, against the fault the
/// rule actually names — a spine file the process cannot write. This test
/// claims exactly what it asserts and no more: the submit refuses, and no
/// effect exists anywhere afterwards. WHICH refusal it is, is the sibling
/// test's claim, and the sibling is ignored because the source does not yet
/// deliver it (see below).
// RED by POISONED FIXTURE rather than by mutation, and the reason is worth
// stating: nothing short of reordering the door's phases can make a
// commit land with an unwritable spine, so there is no small mutation to
// make. Removing the `set_read_only` calls — leaving the spine WRITABLE —
// makes the submit succeed and `.expect_err` panic, which proves the
// checker fires on the fixture it is measuring and not on something
// incidental. Recorded as a fixture red.
#[test]
fn a_read_only_spine_file_lands_no_effect_at_all() {
    let _env = env_shared();
    let dir = scratch("read-only-spine-no-effect");
    let c = cauldron(&dir);
    submit(&c, r#"CREATE (d: fixture-alpha/doc { title: "before", state: "draft" })"#)
        .expect("an ordinary commit, before the fault");
    let before = c.committed_commit_counter().expect("counter");

    make_spine_read_only(&dir, true);
    let err = submit(
        &c,
        r#"CREATE (d: fixture-alpha/doc { title: "during", state: "draft" })"#,
    )
    .expect_err("a spine that cannot be written never commits");
    // The refusal is LOUD and structural — never a refusal witness, which is
    // the shape reserved for a statement the pools legitimately declined.
    assert!(
        !matches!(err, SpineError::Refused { .. }),
        "a physics failure is never a refusal witness: {err:?}"
    );
    make_spine_read_only(&dir, false);

    // NO EFFECT EXISTS ANYWHERE. That is the whole of fail-closed.
    assert_eq!(
        c.committed_commit_counter().expect("counter"),
        before,
        "the counter never moved"
    );
    assert_eq!(docs_titled(&c, "during"), 0, "and nothing reached the pool");
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    assert!(
        spine.pending().expect("pending").is_empty(),
        "no half-written row survives the refusal"
    );
}

/// THE HALT half (fail-closed AND HALT). This test was born RED
/// against a genuine source defect and is green now because the defect was
/// FIXED AT SOURCE when the test was written — the honest
/// history, kept because a forced red is what makes this green evidence:
///
/// Its sibling above used to carry this assertion as
/// `Halted { .. } | Hard { .. }`, which is not an assertion at all: `Hard` is
/// what an unwritable spine already returned, so the disjunction was green
/// for the wrong reason. Tightened to `Halted` only, it fired, and the cause
/// was structural: `exec::execute` opens its spine handle at the top of the
/// call, and `SpineDb::open`'s `journal_mode = WAL` pragma is itself a
/// WRITE — so a read-only spine file failed four phases before the
/// fail-closed arm, with a bare storage error, no announcement, and no
/// refusal by name. The fix routes a failed spine OPEN through the same
/// fail-closed rule as a failed append (exec.rs phase 0): announced
/// best-effort, refused as `Halted` naming the state — with the reason
/// carrying the one honest caveat, that a durable halt mark cannot be
/// written to storage that refuses writes.
// RED: against the pre-fix source (exec.rs phase 0 returning the bare open
// error), this test fails on the `Halted` match arm. It was observed red
// there before the fix landed.
#[test]
fn a_read_only_spine_file_fails_the_append_closed_and_halts() {
    let _env = env_shared();
    let dir = scratch("read-only-spine-halt");
    let c = cauldron(&dir);
    submit(&c, r#"CREATE (d: fixture-alpha/doc { title: "before", state: "draft" })"#)
        .expect("an ordinary commit, before the fault");

    make_spine_read_only(&dir, true);
    let err = submit(
        &c,
        r#"CREATE (d: fixture-alpha/doc { title: "during", state: "draft" })"#,
    )
    .expect_err("a spine that cannot be written never commits");
    make_spine_read_only(&dir, false);

    match &err {
        SpineError::Halted { reason } => {
            assert!(
                reason.contains("fail-closed"),
                "the refusal names the fail-closed rule; got: {reason}"
            );
            assert!(
                reason.contains("could not be durably recorded"),
                "the reason carries the honest caveat that no durable mark exists on \
                 read-only storage; got: {reason}"
            );
        }
        other => panic!(
            "a spine append physics refuses is fail-closed-and-HALT, got {other:?}"
        ),
    }
}

/// Checkpoint the WAL out and take (or give back) write permission on every
/// file the spine writes, so a refusal is the file system's and not a lock's.
fn make_spine_read_only(dir: &Path, read_only: bool) {
    if read_only {
        let sql = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("direct SQL");
        sql.pragma_update(None, "journal_mode", "delete")
            .expect("checkpoint out of WAL");
    }
    for suffix in ["", "-wal", "-shm"] {
        let p = dir.join(format!("{SPINE_FILE}{suffix}"));
        if p.exists() {
            set_read_only(&p, read_only);
        }
    }
}

// ---------------------------------------------------------------------------
// THE QUIESCE GATE, now inside the append's own transaction
// ---------------------------------------------------------------------------

/// A quiesced spine refuses THE APPEND ITSELF, not only at the
/// door's early phase-1 gate. The flag is set straight on the spine, behind
/// the door's back, and then asked twice.
///
/// The DIRECT `append_or_existing` call is the discriminating half, and it is
/// here for a precise reason: `exec`'s phase-1 gate answers with the SAME
/// `Busy` message as the append's own gate, so a whole-`submit` test cannot
/// tell the two apart and cannot see the inner one go missing. The inner gate
/// is the one that carries the guarantee — a door call that passed phase 1
/// before the flag flipped still has to get past it, and without that the
/// append could land AFTER `quiesce()` observed the spine drained and
/// returned.
// RED: deleting the `if quiesced != 0` block from
// `spine_db::append_or_existing_interior` — the direct append below then
// succeeds and leaves a row in `pending()`, which is exactly the row a
// whole-cauldron backup taken on quiesce()'s assurance would contain.
// NEGATIVE RESULT, recorded: the `submit` half alone does NOT red under that
// mutation, because phase 1 catches it first with an identical message. That
// is why both halves are here.
#[test]
fn a_quiesced_spine_refuses_the_append_itself_and_nothing_is_appended() {
    let _env = env_shared();
    let dir = scratch("quiesce-append-gate");
    let c = cauldron(&dir);
    let before = c.committed_commit_counter().expect("counter");

    {
        let mut spine = spine_of(&dir);
        spine.set_quiesced(true).expect("quiesce the spine directly");
    }

    // (1) THE APPEND ITSELF, called directly. No door, no phase 1.
    {
        let mut spine = spine_of(&dir);
        let err = spine
            .append_or_existing(&envelope("CREATE (d: fixture-alpha/doc)"), &hash(0xa1), &payload(&[ALPHA]))
            .expect_err("a quiesced spine refuses its own append");
        assert_quiesce_busy(&err, &dir);
    }

    // (2) AND THE WHOLE DOOR.
    let err = submit(
        &c,
        r#"CREATE (d: fixture-alpha/doc { title: "while quiesced", state: "draft" })"#,
    )
    .expect_err("a quiesced cauldron begins no new commit");
    assert_quiesce_busy(&err, &dir);

    // NOTHING WAS APPENDED, by either route.
    let spine = spine_of(&dir);
    assert!(
        spine.pending().expect("pending").is_empty(),
        "a refused append leaves no row at all"
    );
    assert_eq!(c.committed_commit_counter().expect("counter"), before);
    assert_eq!(docs_titled(&c, "while quiesced"), 0);

    // LIFTING IT (by hand, as it was set by hand — a quiesce
    // has an owner and `resume()` lifts only a quiesce its own handle took;
    // `set_quiesced(false)` is the test-support repair that lifts every
    // quiescer), the very same envelope then lands.
    spine_of(&dir)
        .set_quiesced(false)
        .expect("lift the by-hand quiesce");
    let outcome = submit(
        &c,
        r#"CREATE (d: fixture-alpha/doc { title: "while quiesced", state: "draft" })"#,
    )
    .expect("the same envelope lands once the quiesce is lifted");
    assert!(
        matches!(outcome, Outcome::Committed { .. }),
        "and it COMMITS: {outcome:?}"
    );
    assert_eq!(docs_titled(&c, "while quiesced"), 1);
    assert_eq!(c.committed_commit_counter().expect("counter"), before + 1);
}

/// THE DRAIN LOOP, RUN WITH OUTSTANDING WORK — which nothing had ever done,
/// and which is exactly what hid the missing append gate: every earlier
/// quiesce test ran against an already-drained spine, where the loop returns
/// on its first read and its waiting half never executes at all.
///
/// A decided-but-un-cleared row is staged by hand, a second thread calls
/// `Cauldron::quiesce`, and the claim is asserted in both directions: it does
/// NOT return while that row is on the worklist, and it DOES return, `Ok`,
/// once the row finishes (in-flight commits finish; it returns when the
/// spine has drained).
// RED: making `recover::quiesce`'s drain loop test `in_landing` empty instead
// of `pending()` empty, or simply returning `Ok(())` after setting the flag —
// the quiesce thread then finishes before its drain loop has recorded
// `DRAIN_WAITS` waits, and the `!is_finished()` assertion fires. Also
// forced returning `Ok` after 1 wait, and after 3 and after
// `DRAIN_WAITS - 1` = 9 waits: red at the same assertion every time. That is
// the whole point of the hook: a backup taken on a quiesce that returned early
// contains a half-landed commit.
//
// NO TIMING ASSUMPTION: this used to sleep a fixed
// 300ms and then assert the flag, which went red under load whenever the
// quiesce thread's open + `add_quiescer` took longer than that. Now it waits
// on two handshakes instead — the quiescer's ROW (`await_quiescers`) and the
// drain loop's own record that it saw the row in flight and waited
// `DRAIN_WAITS` times (`recover::drain_probe`, test-support only) — each
// bounded only by a hang guard that fails loudly. The period this test
// outlasts is therefore none; what it proves is counted in DRAIN CHECKS.
//
// THE WINDOW. The old 300ms sleep proved the property over
// every drain check that fits in 300ms, and the count must not shrink that.
// Derived from `recover::quiesce`'s backoff under `lock_cfg` (poll 2ms,
// doubling, capped at 32x = 64ms): wait n is recorded just before the n-th
// sleep, so it falls after the first n-1 sleeps: 0, 2, 6, 14, 30, 62, 126,
// 190, 254, 318ms for n = 1..=10. Nine waits fit in 300ms, so an unloaded
// run of the old test caught a quiesce returning after up to 9 waits.
// Requiring 10 catches every return after k <= 9 waits deterministically:
// such a mutant never records a 10th wait, so it has finished when the loop
// below exits. That is the old window exactly, and under load it is wider,
// not narrower. It is about 318ms of drain at most, far below the 20s hang
// guard and the 30s ceiling.
#[test]
fn quiesce_does_not_return_while_a_row_is_still_in_flight_and_returns_once_it_clears() {
    let _env = env_shared();
    let dir = scratch("quiesce-drains");
    let c = cauldron(&dir);
    // ONE un-cleared row, decided, with its parts NOT landed: in flight by
    // every reading of the word.
    let seq = stage_decided_row(&dir, 0xd1, &[ALPHA, BETA]);
    assert_eq!(spine_of(&dir).pending().expect("pending").len(), 1);

    // The quiesce's TOKEN comes back with the outcome: a quiesce is lifted
    // by the call that took it and by no other — through the token that
    // call holds, on whichever handle.
    let quiescing = {
        let dir = dir.clone();
        std::thread::spawn(move || {
            let c = Cauldron::open_unrecovered(&dir, lock_cfg(30_000)).expect("open");
            c.quiesce()
        })
    };

    // IT DOES NOT RETURN. The flag goes down first (the call's first act) —
    // awaited on its ROW, not on a clock — so what follows measures the DRAIN.
    await_quiescers(&dir, 1);
    assert!(
        spine::recover::is_quiesced(&spine_of(&dir)).expect("flag"),
        "the flag goes down first, before the drain begins"
    );
    let nonce = quiescer_nonces(&dir).pop().expect("the one quiescer's nonce");
    // Wait until the drain loop has seen the row in flight and chosen to wait
    // DRAIN_WAITS times, or until the call has returned — whichever comes
    // first. A quiesce that returns after fewer than DRAIN_WAITS waits never
    // records the last one, so it is finished when this loop exits and the
    // assertion below fires. That holds for every k < DRAIN_WAITS and is
    // claimed for no larger k. (See THE WINDOW above for why it is 10.)
    const DRAIN_WAITS: u64 = 10;
    let started = Instant::now();
    while spine::recover::drain_probe::waits(&nonce) < DRAIN_WAITS && !quiescing.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "hang guard: the quiesce neither waited {DRAIN_WAITS} times nor returned"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        !quiescing.is_finished(),
        "quiesce returned with a decided, un-cleared row still on the worklist \
         (drain-loop waits recorded: {})",
        spine::recover::drain_probe::waits(&nonce)
    );
    assert_eq!(
        spine_of(&dir).pending().expect("pending").len(),
        1,
        "and the row really is still in flight"
    );

    // FINISH THE ROW — both parts land, then the cleared flip.
    {
        let mut spine = spine_of(&dir);
        spine.mark_part_landed(seq, 0).expect("alpha lands");
        spine.mark_part_landed(seq, 1).expect("beta lands");
        spine
            .mark_cleared(seq, &serde_json::json!({ "staged": true }))
            .expect("cleared");
    }

    // AND NOW IT RETURNS, Ok.
    let started = Instant::now();
    let token = quiescing
        .join()
        .expect("the quiescing thread did not panic")
        .expect("a drained spine quiesces successfully");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "it noticed the drain promptly: {:?}",
        started.elapsed()
    );
    assert!(spine::recover::is_quiesced(&spine_of(&dir)).expect("flag"));
    // Only the token lifts it (a handle without one cannot call `resume` at
    // all — that arm is the type's now), and it lifts it from any handle.
    assert!(c.resume(&token).expect("resume"));
    assert!(!spine::recover::is_quiesced(&spine_of(&dir)).expect("flag"));
}

/// THE TIMEOUT ARM of the same loop: a row nobody ever finishes. The refusal
/// is loud, NAMES the still-in-flight sequence number, and — the part that
/// matters — the quiesce flag STAYS SET. Silently resuming would hand back a
/// cauldron that is neither drained nor quiesced, so `resume` is the caller's
/// to call and nobody else's.
// RED: making the ceiling arm of `recover::quiesce` call `set_quiesced(false)`
// before returning its `Busy` — the "still set" assertion below fires, and so
// does the submit that must still refuse. Also red by returning `Ok(())` at
// the ceiling instead of `Err`.
#[test]
fn a_quiesce_that_never_drains_refuses_by_name_and_leaves_the_flag_set_until_resume() {
    let _env = env_shared();
    let dir = scratch("quiesce-timeout");
    // The wait ceiling is CONFIGURATION, never a constant, which
    // is what lets this test take 300ms instead of the default ten seconds.
    let c = Cauldron::create(
        &dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        lock_cfg(300),
    )
    .expect("create the cauldron");
    let seq = stage_decided_row(&dir, 0xd2, &[ALPHA]);

    let started = Instant::now();
    let err = c
        .quiesce()
        .expect_err("nobody ever finishes the staged row");
    let waited = started.elapsed();
    match &err.error {
        SpineError::Busy { message } => {
            assert!(message.contains("did not drain"), "{message}");
            assert!(
                message.contains(&format!("seq {seq}")),
                "the refusal NAMES the still-in-flight seq: {message}"
            );
            assert!(
                message.contains("resume"),
                "and names the remedy: {message}"
            );
        }
        other => panic!("a quiesce that does not drain is a loud Busy, got {other:?}"),
    }
    assert!(waited >= Duration::from_millis(300), "it waited: {waited:?}");
    assert!(waited < Duration::from_secs(10), "and it stopped: {waited:?}");

    // THE FLAG STAYS SET — asserted twice, on the flag itself and on the
    // behaviour that flag exists to produce.
    assert!(
        spine::recover::is_quiesced(&spine_of(&dir)).expect("flag"),
        "a failed quiesce never lifts itself"
    );
    let still = submit(
        &c,
        r#"CREATE (d: fixture-alpha/doc { title: "after the timeout", state: "draft" })"#,
    )
    .expect_err("and the cauldron is still refusing new commits");
    assert_quiesce_busy(&still, &dir);

    // Only `resume` lifts it — with the token the error carries, since the
    // quiescer this call recorded still stands.
    let standing = err
        .standing
        .expect("the quiescer stands, so its token comes back with the error");
    assert!(c.resume(&standing).expect("resume"));
    assert!(!spine::recover::is_quiesced(&spine_of(&dir)).expect("flag"));
}

/// N = 2: `assert_quiesce_busy` had only ever been
/// run against ONE quiescer, so its per-quiescer loop had never iterated
/// twice and nothing showed the refusal names every quiescer rather than the
/// first. Here two `quiesce()` calls, on two handles in two threads, are BOTH
/// draining behind one staged row at the same time; a new commit is refused
/// at both gates (the append itself and the whole door) by a `Busy` that
/// counts two and names each by holder and since. Then the first lifts its
/// own quiesce and the refusal names exactly the one left (N = 2 -> 1), and
/// the second's lift lets the very same statement land.
// RED (seen): `recover::quiesced_busy` naming only the first
// standing quiescer (`.iter().take(1)`) — reds HERE ONLY, at the per-quiescer
// loop on the SECOND quiescer; every N = 1 call site in this file stays green
// under it, which is the gap this test closes. And naming only the later ones
// (`.iter().skip(1)`) — reds here on the FIRST quiescer (and at the N = 1
// sites too, which name nobody under it).
#[test]
fn two_concurrent_quiescers_are_each_named_by_the_busy_refusal_at_both_gates() {
    let _env = env_shared();
    let dir = scratch("quiesce-two-draining");
    let c = cauldron(&dir);
    let seq = stage_decided_row(&dir, 0xd3, &[ALPHA]);
    let quiescer = || {
        let dir = dir.clone();
        std::thread::spawn(move || {
            Cauldron::open_unrecovered(&dir, lock_cfg(30_000))
                .expect("open")
                .quiesce()
        })
    };
    let first = quiescer();
    await_quiescers(&dir, 1);
    // `since` is recorded to the millisecond; step past one so the two
    // records cannot coincide even though both holders are this pid.
    std::thread::sleep(Duration::from_millis(5));
    let second = quiescer();
    await_quiescers(&dir, 2);

    // Sentinel: two DIFFERENT records, so naming one cannot pass for both.
    let both = quiescer_records(&dir);
    assert_eq!(both.len(), 2, "{both:?}");
    assert_ne!(both[0], both[1], "the two quiescers must be told apart: {both:?}");
    assert!(
        !first.is_finished() && !second.is_finished(),
        "both quiesce calls are still draining behind the staged row"
    );

    // (1) THE APPEND ITSELF, and (2) THE WHOLE DOOR, under two quiescers.
    {
        let mut spine = spine_of(&dir);
        let err = spine
            .append_or_existing(&envelope("CREATE (d: fixture-alpha/doc)"), &hash(0xa2), &payload(&[ALPHA]))
            .expect_err("a doubly quiesced spine refuses its own append");
        assert_quiesce_busy(&err, &dir);
    }
    const STATEMENT: &str =
        r#"CREATE (d: fixture-alpha/doc { title: "under two quiescers", state: "draft" })"#;
    let err = submit(&c, STATEMENT).expect_err("a doubly quiesced cauldron begins no new commit");
    assert_quiesce_busy(&err, &dir);

    // Finish the staged row: both drains complete, each with its own token.
    {
        let mut spine = spine_of(&dir);
        spine.mark_part_landed(seq, 0).expect("alpha lands");
        spine.mark_cleared(seq, &serde_json::json!({ "staged": true })).expect("cleared");
    }
    let first = first.join().expect("first thread").expect("the first quiesce drains Ok");
    let second = second.join().expect("second thread").expect("the second quiesce drains Ok");

    // N = 2 -> 1: the first lifts ITS OWN; the refusal now names the second only.
    assert!(c.resume(&first).expect("resume the first"));
    assert_eq!(quiescer_records(&dir), vec![both[1].clone()], "only the second stands");
    let err = submit(&c, STATEMENT).expect_err("still quiesced by the second");
    assert_quiesce_busy(&err, &dir);
    match &err {
        SpineError::Busy { message } => assert!(
            !message.contains(&format!("{} since {}", both[0].0, both[0].1)),
            "the lifted quiescer is no longer named: {message}"
        ),
        other => panic!("Busy, got {other:?}"),
    }

    assert!(c.resume(&second).expect("resume the second"));
    assert!(matches!(submit(&c, STATEMENT), Ok(Outcome::Committed { .. })));
    assert_eq!(docs_titled(&c, "under two quiescers"), 1);
}

/// Every quiescer on record, `(holder, since)`, oldest first — by direct SQL.
fn quiescer_records(dir: &Path) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("direct SQL");
    let mut stmt = conn
        .prepare("SELECT holder, since FROM quiescers ORDER BY since, nonce")
        .expect("prepare");
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .expect("query");
    rows.map(|r| r.expect("row")).collect()
}

/// The nonce of every quiescer on record, by direct SQL (the key the
/// drain loop's test-support probe records its waits under).
fn quiescer_nonces(dir: &Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("direct SQL");
    let mut stmt = conn
        .prepare("SELECT nonce FROM quiescers ORDER BY since, nonce")
        .expect("prepare");
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .expect("query");
    rows.map(|r| r.expect("row")).collect()
}

/// Wait (bounded) until exactly `n` quiescers are on record.
fn await_quiescers(dir: &Path, n: usize) {
    let started = Instant::now();
    while quiescer_records(dir).len() != n {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "never saw {n} quiescer(s): {:?}",
            quiescer_records(dir)
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

// ---------------------------------------------------------------------------
// THE ABORT ARBITER (`mark_aborted_if_undecided`)
// ---------------------------------------------------------------------------

/// The arbiter WINS on an un-decided row: one atomic conditional flip, `true`,
/// and the row afterwards reads cleared-and-undecided — which is
/// unambiguously "this attempt aborted" to any auditor.
// RED: making `mark_aborted_if_undecided_interior` return `Ok(changed >= 0)`
// (always true) leaves this green — it is the DECIDED case below that pins the
// predicate. What reds here is dropping the `cleared = 1` from the UPDATE, or
// widening the WHERE to drop `decided = 0`.
#[test]
fn the_abort_arbiter_wins_on_an_undecided_row_and_leaves_it_cleared_and_undecided() {
    let (path, mut spine) = bare_spine("arbiter-undecided");
    let seq = append_bare(&mut spine, 0xb1, &[ALPHA]);

    let won = spine
        .mark_aborted_if_undecided(seq, &serde_json::json!({ "aborted": true, "landed": false }))
        .expect("the arbiter answers");
    assert!(won, "an un-decided row is the arbiter's to abort");

    let row = spine.row_at(seq).expect("the row is KEPT");
    assert!(!row.decided, "an aborted attempt never decided");
    assert!(row.cleared, "and it has left the pending worklist");
    assert!(spine.pending().expect("pending").is_empty());

    // Idempotent by construction: the second call finds it cleared and loses.
    assert!(
        !spine
            .mark_aborted_if_undecided(seq, &serde_json::Value::Null)
            .expect("second call"),
        "an already-cleared row is nobody's to abort a second time"
    );
    let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
}

/// The arbiter LOSES on a decided row: `false`, and the row is UNTOUCHED.
/// This is the whole reason the arbiter exists — a `recover()` racing a live
/// door reads a row un-decided, the door decides it, and the abort must then
/// change nothing at all rather than clear a transaction that is landing.
// RED: widening `mark_aborted_if_undecided_interior`'s WHERE clause to
// `WHERE seq = ?1` — it returns true and the decided row comes back cleared,
// so `assert!(!won)` and the `!row.cleared` assertion both fire.
#[test]
fn the_abort_arbiter_loses_on_a_decided_row_and_touches_nothing() {
    let (path, mut spine) = bare_spine("arbiter-decided");
    let seq = append_bare(&mut spine, 0xb2, &[ALPHA]);
    spine.mark_decided(seq).expect("a live door decides it");
    let before = spine.row_at(seq).expect("row");

    let won = spine
        .mark_aborted_if_undecided(seq, &serde_json::json!({ "aborted": true, "landed": false }))
        .expect("the arbiter answers");
    assert!(!won, "a decided row belongs to the door that decided it");

    let after = spine.row_at(seq).expect("row");
    assert!(after.decided, "still decided");
    assert!(!after.cleared, "and NOT cleared — the abort touched nothing");
    assert_eq!(after.result, before.result, "not even the result column");
    assert_eq!(after.parts, before.parts);
    assert_eq!(
        spine.pending().expect("pending").len(),
        1,
        "it is still on the worklist, for the door or for a later recovery pass"
    );
    let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
}

// ---------------------------------------------------------------------------
// THE ORPHAN-LOCK SWEEP at the top of `recover()`
// ---------------------------------------------------------------------------

/// A crash in the instant BETWEEN a row's cleared flip and its lock release
/// leaves lock rows that no worklist reaches: the row is cleared, so
/// `pending()` never sees it again, and the locks are held against every
/// later claimant until the wait ceiling — forever, one refusal at a time.
/// `recover()` opens with a sweep that restores the invariant (locks are held
/// only by un-cleared rows), and it is not a worklist action:
/// the report below is EMPTY and the locks are still gone.
// RED: deleting the `locks::release_cleared_orphans(&spine)?` call from the
// top of `recover::recover` — the second acquire below then fails with the
// same `Busy` as the first, naming a holder whose transaction finished.
#[test]
fn recovery_sweeps_the_locks_of_a_row_that_cleared_without_releasing_them() {
    let _env = env_shared();
    let dir = scratch("orphan-locks");
    let c = cauldron(&dir);
    let spine = spine_of(&dir);
    let resource = Resource::Body {
        node: NodeId("nOrphan".to_string()),
    };

    // THE CRASHED ROW: appended, decided, landed, CLEARED — and its locks
    // never released, which is the one instant the release rule cannot
    // cover by itself.
    let crashed = {
        let mut spine = spine_of(&dir);
        let seq = append_bare(&mut spine, 0xc1, &[ALPHA]);
        acquire(&spine, seq, &write_claim(&resource), &lock_cfg(20_000))
            .expect("the crashed transaction held a write lock");
        spine.mark_decided(seq).expect("decide");
        spine.mark_part_landed(seq, 0).expect("land");
        spine
            .mark_cleared(seq, &serde_json::json!({ "landed": true }))
            .expect("cleared — and then the process died, before the release");
        seq
    };
    assert_eq!(
        queue(&spine, &PoolName::new(ALPHA), &resource)
            .expect("queue")
            .len(),
        1,
        "the orphan is really on the table"
    );

    // A LATER CLAIMANT, itself settled so recovery has nothing to say about
    // it, blocks on a ghost.
    let waiter = {
        let mut spine = spine_of(&dir);
        let seq = append_bare(&mut spine, 0xc2, &[ALPHA]);
        spine.mark_decided(seq).expect("decide");
        spine.mark_part_landed(seq, 0).expect("land");
        spine
            .mark_cleared(seq, &serde_json::json!({ "landed": true }))
            .expect("cleared");
        seq
    };
    assert!(
        spine.pending().expect("pending").is_empty(),
        "no row is on the worklist: the sweep is the ONLY thing recovery can do here"
    );

    let err = acquire(&spine, waiter, &write_claim(&resource), &lock_cfg(200))
        .expect_err("the orphan blocks every conflicting claim");
    match &err {
        SpineError::Busy { message } => assert!(
            message.contains(&format!("seq {crashed}")),
            "blocked by the crashed row's ghost: {message}"
        ),
        other => panic!("expected a loud Busy, got {other:?}"),
    }

    // THE SWEEP.
    assert_eq!(
        c.recover().expect("recovery runs"),
        spine::recover::RecoveryReport::default(),
        "the sweep is not a worklist action: it reports nothing, because there is \
         no un-cleared row for it to report on"
    );
    assert!(
        queue(&spine, &PoolName::new(ALPHA), &resource)
            .expect("queue")
            .is_empty(),
        "the orphan lock rows are gone"
    );

    // AND THE CLAIM THAT COULD NEVER BE GRANTED IS GRANTED.
    acquire(&spine, waiter, &write_claim(&resource), &lock_cfg(1_000))
        .expect("no ghost blocks the queue after the sweep");
}

// ---------------------------------------------------------------------------
// helpers — this file runs in BOTH builds, so it cannot borrow `kill.rs`
// ---------------------------------------------------------------------------

pub const ALPHA: &str = "fixture-alpha";
pub const BETA: &str = "fixture-beta";

static ENV: std::sync::RwLock<()> = std::sync::RwLock::new(());

fn env_shared() -> std::sync::RwLockReadGuard<'static, ()> {
    ENV.read().unwrap_or_else(|e| e.into_inner())
}

fn scratch(name: &str) -> PathBuf {
    let dir = test_scratch::tmp_path(format!("spine-recovery-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

// ---- staging rows and locks by hand (the `test-support` lifecycle surface) --
//
// Every flip below is a `pub`-under-`test-support` method on `SpineDb`, and
// the crash states these tests need cannot be staged any other way: nothing on
// the door path will leave a decided-and-un-cleared row behind on purpose, and
// nothing at all will leave a cleared row holding locks.

/// A fresh handle on the cauldron's own spine file. One per call, like the
/// crate's own `Cauldron::spine`, so two of them behave like two processes.
fn spine_of(dir: &Path) -> SpineDb {
    SpineDb::open(&dir.join(SPINE_FILE)).expect("open the spine file")
}

/// A standalone spine file with no cauldron around it — enough for the row
/// lifecycle AND the lock table, which live in this one file.
fn bare_spine(name: &str) -> (PathBuf, SpineDb) {
    let dir = scratch(name);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(SPINE_FILE);
    let db = SpineDb::create(&path).expect("create the spine");
    (path, db)
}

/// A distinct hash identity per staged row. The identity is not these tests'
/// subject — the sequence number it issues, and the flags on the row, are —
/// but it must be distinct: `idx_tx_live` admits one live attempt per
/// identity, and two staged rows sharing a hash would be refused.
fn hash(tag: u8) -> TxHash {
    TxHash([tag; 32])
}

/// A compiled payload naming one part per pool, each carrying NO deltas. The
/// tests below flip spine FLAGS; they never land anything in a pool, so an
/// empty delta list is the honest payload and not a shortcut.
fn payload(pools: &[&str]) -> CompiledPayload {
    CompiledPayload {
        parts: pools
            .iter()
            .map(|p| CompiledPart {
                pool: PoolName::new(*p),
                // The stamp each part validated under; the
                // fixtures pin alpha at 1 and beta at 2.
                schema_version: if *p == BETA { 2 } else { 1 },
                deltas: vec![],
                premises: vec![],
                far_versions: Default::default(),
            })
            .collect(),
        minted: Default::default(),
    }
}

/// Append one real pending row and hand back its sequence number.
fn append_bare(spine: &mut SpineDb, tag: u8, pools: &[&str]) -> Seq {
    match spine
        .append_or_existing(&envelope("staged by the recovery suite"), &hash(tag), &payload(pools))
        .expect("append")
    {
        spine::spine_db::AppendOutcome::Appended(seq) => seq,
        other => panic!("the staged row must be a fresh append, got {other:?}"),
    }
}

/// Stage a row that is DECIDED and NOT cleared — in flight by every reading
/// of the word, and the state the drain loop had never been run against.
fn stage_decided_row(dir: &Path, tag: u8, pools: &[&str]) -> Seq {
    let mut spine = spine_of(dir);
    let seq = append_bare(&mut spine, tag, pools);
    spine.mark_decided(seq).expect("decide");
    seq
}

/// One exclusive claim on one resource in alpha.
fn write_claim(resource: &Resource) -> ClaimSet {
    let mut set = ClaimSet::new();
    set.insert(Claim {
        pool: PoolName::new(ALPHA),
        resource: resource.clone(),
        mode: LockMode::Write,
    });
    set
}

/// Both bounds are configuration, never constants: a short
/// ceiling is what makes the timeout arms of these tests fast instead of
/// ten seconds each.
fn lock_cfg(ceiling_ms: u64) -> SpineConfig {
    SpineConfig {
        lock_poll_interval: Duration::from_millis(2),
        lock_wait_ceiling: Duration::from_millis(ceiling_ms),
        ..SpineConfig::default()
    }
}

/// The refusal a quiesced spine gives, whichever gate produced it: `Busy`,
/// naming the quiesce and what it is for — and naming every standing
/// quiescer by holder and since when, plus the
/// by-hand lift. Asserted at BOTH gates through this one helper, so the
/// append's own gate (the guarantee) is measured and not only the door's
/// courtesy one. (An earlier version of this helper asked only for the word
/// "quiesced", which the old constant text satisfied at either gate.)
///
/// THE TWO OPERANDS ARE DIFFERENT THINGS: the
/// message comes from the refusal; holder and since come from the spine's
/// own `quiescers` table, read here by direct SQL — not from the message and
/// not through the crate. An earlier version checked for `quiescer(s): ` (the
/// format string's own template text, always emitted) and ` since `, so a
/// refusal naming the wrong holder or a made-up time passed it.
// RED (seen): `quiesced_busy` formatting a constant in place of
// `q.since` — every call site reds here; the earlier helper stayed ok under the
// same cut. And `q.holder` replaced by the constant "by-hand" — reds at the
// timeout test, whose quiescer is the sanctioned hook's `pid <n>`, while the
// two by-hand call sites stay green (a constant holder is right for them).
fn assert_quiesce_busy(err: &SpineError, dir: &Path) {
    let recorded: Vec<(String, String)> = {
        let conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("direct SQL");
        let mut stmt = conn
            .prepare("SELECT holder, since FROM quiescers ORDER BY since, nonce")
            .expect("prepare");
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .expect("query");
        rows.map(|r| r.expect("row")).collect()
    };
    // Sentinel: the record is non-empty, so the per-quiescer loop below
    // cannot pass by running zero times.
    assert!(!recorded.is_empty(), "a quiesced spine has a quiescer on record");
    match err {
        SpineError::Busy { message } => {
            assert!(message.contains("quiesced"), "the refusal names the quiesce: {message}");
            assert!(
                message.contains(&format!("Quiesced by {} quiescer(s)", recorded.len())),
                "the refusal counts the quiescers on record ({}): {message}",
                recorded.len()
            );
            for (holder, since) in &recorded {
                assert!(
                    !holder.is_empty() && !since.is_empty(),
                    "sentinel: a recorded quiescer has a holder and a since: {recorded:?}"
                );
                assert!(
                    message.contains(&format!("{holder} since {since}")),
                    "the refusal names quiescer `{holder}` since `{since}` as the spine \
                     recorded it: {message}"
                );
            }
            assert!(message.contains("resume_all"), "and names the by-hand lift: {message}");
        }
        other => panic!("a quiesced spine refuses Busy, got {other:?}"),
    }
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
                thread: "recovery-suite".into(),
                dispatch: "recovery".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn submit(c: &Cauldron, statement: &str) -> Result<Outcome, SpineError> {
    c.submit(envelope(statement))
}

fn docs_titled(c: &Cauldron, title: &str) -> usize {
    c.read(&ReadRequest {
        statement: r#"MATCH (d: fixture-alpha/doc) RETURN d.title"#.to_string(),
        bindings: Bindings::new(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    })
    .expect("bulk read")
    .rows
    .0
    .iter()
    .filter(|r| r["d.title"] == serde_json::json!(title))
    .count()
}

fn set_read_only(path: &std::path::Path, read_only: bool) {
    let mut perms = std::fs::metadata(path).expect("metadata").permissions();
    perms.set_readonly(read_only);
    std::fs::set_permissions(path, perms).expect("set permissions");
}
