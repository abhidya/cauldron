//! ADVERSARY — lens: CRASH, CONCURRENCY AND TIME.
//!
//! **Recovery racing live traffic.** The design gives recovery one procedure —
//! row not decided: abort; row decided: roll forward; row cleared: done — and
//! says nothing about WHEN recovery may run. `recover.rs`'s own docs half-
//! admit a live race ("`skipped_live` ... non-empty only when `recover()`
//! runs against a cauldron with live doors") and the code fixes
//! exactly one arm of it (the abort arbiter, `mark_aborted_if_undecided`).
//! This suite holds a REAL door open at a named stage with the seam's pause
//! form — in-process, so the statement under test is this file's own and its
//! RETURN can carry a field a third party can change — and runs `recover()`
//! against it. Every test asserts something that must be true in ANY sane
//! reading of the design: a commit's outcome is projected under its own locks;
//! the row's audit mark agrees with what recovery reports it did; a correct
//! statement is never told it misused the API; a door that returns leaves no
//! lock behind; a read is never refused by the quiesce gate.
//!
//! **Interleaving is PROVEN, never assumed**: the door is held by the
//! library's own `.reached` marker and observed `!is_finished()` before and
//! after every foreign act; the two concurrent recoveries are both observed
//! blocked on a pool write lock this test holds, then released together.
//!
//! **Red tests are marked `#[ignore = "ADVERSARY RED: ..."]`** so the armed
//! suite's default run stays green; run them with `--ignored`. Nothing under
//! `src/` was touched. Each test's diagnosis is in its doc comment.

#![cfg(all(unix, feature = "fault-injection"))]

#[path = "util/kill.rs"]
mod kill;

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::pool::Pool;
use spine::schema_def::fixtures;
use spine::spine_db::Seq;
use spine::{Cauldron, Outcome, ReadRequest};
use std::path::{Path, PathBuf};
use std::thread::ScopedJoinHandle;
use std::time::{Duration, Instant};

test_scratch::install!();

const ALPHA: &str = "fixture-alpha";

/// Two pools, one commit, and a RETURN that carries a MUTABLE field (`title`)
/// so that a projection taken after the locks were dropped is DISTINGUISHABLE
/// from one taken under them. The victim's own statements return only ids
/// and stamps, which is why this file drives the door in-process.
const CROSS_BY_ID: &str = r#"MATCH (d: fixture-alpha/doc { id: $id })
   EXPECT d.state = "draft"
   CREATE (t: fixture-beta/ticket { state: "open" })
   CREATE (t) -[catalogues]-> (d)
   SET d.state = "published"
   RETURN d.title AS title, d.state AS state, t.id AS ticket"#;

/// The foreign write: a third party changing the very field the held door
/// will project. Under the locking rules it cannot land until the door's own cleared
/// flip releases the doc's body lock.
const SET_TITLE: &str = r#"MATCH (d: fixture-alpha/doc { id: $id })
   SET d.title = $t
   RETURN d.title AS title"#;

/// The victim's bulk cross-schema statement, byte-for-byte (two parts: alpha
/// then beta, so `between-landings` is a real place).
const BULK_CROSS: &str = r#"MATCH (d: fixture-alpha/doc { state: "draft" })
   CREATE (t: fixture-beta/ticket { state: "open" })
   CREATE (t) -[catalogues]-> (d)
   SET d.state = "published"
   RETURN t.id AS ticket"#;

/// A cross-pool READ — holds nothing, blocks nobody, retries,
/// escalates.
const HOP_READ: &str =
    r#"MATCH (t: fixture-beta/ticket) -[catalogues]-> (d: fixture-alpha/doc) RETURN d.id"#;

const SEED_TITLE: &str = "seed title, projected under the row's own locks";
const FOREIGN_TITLE: &str = "changed by a third party while the door was held";

// ===========================================================================
// CASE 1 — recovery racing a door held AFTER its decided flip
// ===========================================================================

/// What one run of the decided-hold interleaving observed, read from disk at
/// each step. Three tests below each assert ONE thing about it.
struct DecidedRace {
    seq: Seq,
    hash_hex: String,
    /// What `recover()` said it did to this row.
    report: spine::recover::RecoveryReport,
    /// The row's `result` column right after recovery cleared it.
    stored_by_recovery: Option<serde_json::Value>,
    /// Lock rows for `seq` right after recovery returned, while the door was
    /// still held (observed, reported, not asserted — see the tests).
    lock_rows_after_recovery: usize,
    /// Whether the foreign SET COMMITTED while the door was still held.
    foreign_landed_while_held: bool,
    /// What the held door returned once released.
    door: Result<Outcome, SpineError>,
    /// The row as it stands after everyone has returned.
    row_after: spine::spine_db::TxRow,
}

/// Hold a two-pool door at `decided` (row decided, nothing landed, every lock
/// held), run `recover()` against it, land a foreign SET on the doc it will
/// project, release the door, and read everything back.
fn race_recovery_against_a_door_held_at_decided(name: &str) -> DecidedRace {
    let dir = scratch(name);
    let c = cauldron(&dir, 4_000);
    let id = mint_doc(&c, SEED_TITLE);
    let env = envelope_with(CROSS_BY_ID, &[("id", serde_json::json!(id))], "held-door");
    let hash_hex = env.hash_identity().expect("hash").to_hex();
    let pause = arm_pause("decided", name);

    std::thread::scope(|scope| {
        let door = scope.spawn(|| c.submit(env));
        pause.wait_reached(&door);
        // From here THIS thread's own submits must not pause; the held door
        // has already passed its env read for this stage and reads the env
        // again only at later stages, where an unset variable is `Ok(())`.
        pause.disarm();

        let row = kill::the_stuck_row(&dir);
        assert!(row.decided && !row.cleared, "held at decided: {row:?}");
        assert!(
            row.parts.iter().all(|p| !p.landed),
            "held BEFORE any landing: {:?}",
            row.parts
        );
        assert!(
            kill::lock_rows(&dir, row.seq) > 0,
            "the held door holds its locks (release is at the cleared flip only)"
        );
        assert!(!door.is_finished(), "the door is held while recovery runs");

        // RECOVERY, AGAINST THE LIVE DOOR.
        let report = c.recover().expect("recovery returns");
        let after_recovery = kill::spine_of(&dir).row_at(row.seq).expect("row");
        let stored_by_recovery = after_recovery.result.clone();
        let lock_rows_after_recovery = kill::lock_rows(&dir, row.seq);
        assert!(!door.is_finished(), "the door is STILL held after recovery returned");

        // THE FOREIGN WRITE on the doc the door will project. If the door's
        // locks are still held it waits behind them; if they are gone it
        // lands at once. Either way it is joined below, after the release.
        let foreign = scope.spawn(|| {
            c.submit(envelope_with(
                SET_TITLE,
                &[
                    ("id", serde_json::json!(id)),
                    ("t", serde_json::json!(FOREIGN_TITLE)),
                ],
                "foreign-writer",
            ))
        });
        let foreign_landed_while_held =
            wait_until(Duration::from_millis(1_200), || foreign.is_finished());
        assert!(!door.is_finished(), "the door is still held before the release");

        pause.release();
        let door_outcome = door.join().expect("the door thread did not panic");
        let foreign_outcome = foreign.join().expect("the foreign thread did not panic");
        assert!(
            matches!(foreign_outcome, Ok(Outcome::Committed { .. })),
            "the foreign SET commits once everything is released: {foreign_outcome:?}"
        );
        let row_after = kill::spine_of(&dir).row_at(row.seq).expect("row");
        DecidedRace {
            seq: row.seq,
            hash_hex,
            report,
            stored_by_recovery,
            lock_rows_after_recovery,
            foreign_landed_while_held,
            door: door_outcome,
            row_after,
        }
    })
}

fn title_of(r: &Result<Outcome, SpineError>) -> Option<String> {
    match r {
        Ok(Outcome::Committed { returned, .. }) => returned
            .0
            .first()
            .and_then(|row| row.get("title"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        _ => None,
    }
}

/// A commit's RETURN projection is taken from the landed state UNDER THE
/// ROW'S OWN LOCKS, before release (locks release only at
/// the cleared flip). Nothing can change the doc's title between the decided
/// flip and the projection, so the door must return the seed title —
/// whether or not a recovery pass ran beside it.
///
/// What happens instead: `recover()` rolls the held row forward, clears it
/// and RELEASES ITS LOCKS while the door is still inside its commit path
/// (`recover.rs:207-208` — `mark_cleared_recovered` then `release_at_clear`,
/// neither conditional on the row still being the recovery's to finish). The
/// foreign SET then lands against a doc nobody holds. The door resumes,
/// finds every part already landed, projects RETURN from the CURRENT pool
/// state — the foreign title — and `mark_cleared` (an unconditional UPDATE,
/// `spine_db.rs:748`) stores that as the row's result. The commit's own
/// reply, and the record every later duplicate answers from, both carry a
/// value that was never true under the commit's locks.
#[test]
fn a_door_held_after_its_decided_flip_returns_the_outcome_projected_under_its_own_locks() {
    let _env = kill::env_exclusive();
    let r = race_recovery_against_a_door_held_at_decided("rvl-decided-return");
    assert!(
        matches!(r.door, Ok(Outcome::Committed { .. })),
        "the held door completes its commit: {:?}",
        r.door
    );
    assert_eq!(
        title_of(&r.door).as_deref(),
        Some(SEED_TITLE),
        "the door's RETURN must be the projection under its own locks (seq {}); lock rows for \
         the row right after recovery returned: {} (0 means recovery released a live door's \
         locks); foreign SET landed while the door was still held: {}; recovery report: {:?}",
        r.seq,
        r.lock_rows_after_recovery,
        r.foreign_landed_while_held,
        r.report
    );
}

/// The row is "the one structure whose purpose is to be trusted about the
/// past". Its `recovered` audit mark exists so that "was this
/// recovered" is read from the row and never inferred. So the mark must
/// AGREE with what recovery reported: if `recover()` says it rolled this row
/// forward, the row says `recovered`; if it skipped a live row, the row says
/// not. Any other combination is a record that lies about its own history.
///
/// What happens instead: recovery reports `rolled_forward = [hash]` and sets
/// `recovered = 1`; the released door then runs its own `mark_cleared`,
/// which writes `recovered = 0` back (`mark_cleared_with(.., false)`), and
/// the audit fact is gone.
#[test]
fn the_rows_recovered_mark_agrees_with_what_recovery_reported() {
    let _env = kill::env_exclusive();
    let r = race_recovery_against_a_door_held_at_decided("rvl-decided-mark");
    let recovery_says_it_rolled_forward = r
        .report
        .rolled_forward
        .iter()
        .chain(r.report.cleared_only.iter())
        .any(|h| h.to_hex() == r.hash_hex);
    assert_eq!(
        r.row_after.recovered, recovery_says_it_rolled_forward,
        "row.recovered = {} but recovery reported {:?} for seq {} (hash {})",
        r.row_after.recovered, r.report, r.seq, r.hash_hex
    );
}

/// A CLEARED row is final: cleared never un-clears and the stored outcome is
/// what every duplicate submission answers with (exactly-once). Once one party has cleared the row and stored its projection,
/// no second party may rewrite that result — the flip is the commit point,
/// and there is one of it.
///
/// What happens instead: recovery clears the row with its projection; the
/// released door clears it AGAIN with a different projection. Two cleared
/// flips, two results, the second silently replacing the first.
#[test]
fn a_cleared_rows_stored_result_is_never_rewritten_by_a_second_cleared_flip() {
    let _env = kill::env_exclusive();
    let r = race_recovery_against_a_door_held_at_decided("rvl-decided-result");
    assert!(
        r.stored_by_recovery.is_some(),
        "recovery stored a projection at its cleared flip"
    );
    assert_eq!(
        r.row_after.result, r.stored_by_recovery,
        "the row's result after the door returned differs from what the first cleared flip \
         stored (seq {}); door returned title {:?}",
        r.seq,
        title_of(&r.door)
    );
}

// ===========================================================================
// CASE 2 — recovery racing a door held BEFORE its decided flip
// ===========================================================================

struct AppendedRace {
    seq: Seq,
    report: spine::recover::RecoveryReport,
    door: Result<Outcome, SpineError>,
    lock_rows_after_door_returned: usize,
    landed: bool,
    /// A later statement on the same doc, under a short ceiling.
    later: Result<Outcome, SpineError>,
}

/// Hold a door at `row-appended` (row on disk, un-decided, locks NOT yet
/// enrolled — acquisition is phase 9, after this stage), run `recover()`
/// against it, release, and see what the door and the lock table say.
fn race_recovery_against_a_door_held_at_row_appended(name: &str, ceiling_ms: u64) -> AppendedRace {
    let dir = scratch(name);
    let c = cauldron(&dir, ceiling_ms);
    let id = mint_doc(&c, SEED_TITLE);
    let env = envelope_with(CROSS_BY_ID, &[("id", serde_json::json!(id))], "held-door");
    let pause = arm_pause("row-appended", name);

    std::thread::scope(|scope| {
        let door = scope.spawn(|| c.submit(env));
        pause.wait_reached(&door);
        pause.disarm();

        let row = kill::the_stuck_row(&dir);
        assert!(!row.decided && !row.cleared, "held at row-appended: {row:?}");
        assert_eq!(
            kill::lock_rows(&dir, row.seq),
            0,
            "at row-appended nothing is enrolled yet (acquire is phase 9)"
        );
        assert!(!door.is_finished());

        // RECOVERY, AGAINST THE LIVE DOOR: the row is un-decided, so recovery's
        // procedure aborts it — and nothing tells recovery the door is alive.
        let report = c.recover().expect("recovery returns");
        let after = kill::spine_of(&dir).row_at(row.seq).expect("row");
        assert!(
            after.cleared && !after.decided,
            "recovery aborted the live door's row: {after:?}"
        );
        assert!(!door.is_finished(), "the door is still held after recovery");

        pause.release();
        let door_outcome = door.join().expect("the door thread did not panic");
        let lock_rows_after_door_returned = kill::lock_rows(&dir, row.seq);
        let landed = kill::ticket_count(&c) == 1;

        let later = c.submit(envelope_with(
            SET_TITLE,
            &[
                ("id", serde_json::json!(id)),
                ("t", serde_json::json!("a later statement on the same doc")),
            ],
            "later-writer",
        ));
        AppendedRace {
            seq: row.seq,
            report,
            door: door_outcome,
            lock_rows_after_door_returned,
            landed,
            later,
        }
    })
}

/// A caller who submits a well-formed statement against a healthy cauldron
/// is never told it MISUSED THE API. `SpineError::Usage` is "fix the call;
/// nothing happened" (`error.rs`), and the door's contract admits exactly
/// four answers to such a caller: it landed; it was refused with witnesses;
/// it is `Busy` (wait and re-ask); or the spine is halted. A recovery pass
/// running beside the door is the spine's own business, not the caller's.
///
/// What happens instead: recovery aborts the un-decided row (nothing marks
/// it live); the door resumes, acquires its locks, re-validates, records its
/// bound identities, and then `mark_decided` refuses — "seq N is already
/// cleared; a cleared-and-undecided row is an ABORTED attempt" — as a
/// `Usage` error, which `execute` hands straight back to the caller.
#[test]
fn a_live_door_whose_row_recovery_aborted_never_blames_the_caller() {
    let _env = kill::env_exclusive();
    let r = race_recovery_against_a_door_held_at_row_appended("rvl-appended-kind", 4_000);
    match &r.door {
        Ok(Outcome::Committed { .. }) => {}
        Ok(other) => panic!("a commit envelope came back {other:?}"),
        Err(e) => {
            let kind = e.kind();
            assert!(
                kind == "busy" || kind == "refused" || kind == "halted",
                "the door answered a correct statement with `{kind}`: {}\n(recovery reported \
                 {:?} for seq {}; the door's row was aborted under it)",
                e.message(),
                r.report,
                r.seq
            );
        }
    }
    // Whatever it answered, it must not have half-landed.
    if r.door.is_err() {
        assert!(!r.landed, "an error answer means nothing landed");
    }
}

/// A door that has RETURNED holds no lock. There are exactly two release
/// points — the cleared flip and the abort — and every exit of `execute`
/// passes through one of them (`locks.rs` release docs).
/// Whatever a door answers, the next statement on the same resources must
/// not wait on a ghost.
///
/// What happens instead: the door enrols and is GRANTED its claims at a
/// sequence number whose row recovery already cleared (`acquire` never
/// consults the transactions table); `mark_decided` then refuses; `execute`
/// takes its un-decided arm and calls `abort_row`, whose arbiter finds the
/// row already cleared and returns `false` — so the locks are NOT released
/// (`recover.rs:296-298`: release follows only a won flip). The lock rows
/// stay granted under a cleared row until some later `recover()` sweeps
/// them; until then every conflicting claimant waits to the ceiling and
/// comes back `Busy`.
#[test]
fn a_door_that_has_returned_holds_no_lock() {
    let _env = kill::env_exclusive();
    let r = race_recovery_against_a_door_held_at_row_appended("rvl-appended-locks", 1_500);
    assert_eq!(
        r.lock_rows_after_door_returned, 0,
        "lock rows still held for seq {} after the door returned {:?}; the next statement on \
         the doc answered {:?}",
        r.seq,
        r.door.as_ref().map(|_| "committed").map_err(|e| e.kind()),
        r.later.as_ref().map(|_| "committed").map_err(|e| e.message())
    );
    assert!(
        matches!(r.later, Ok(Outcome::Committed { .. })),
        "a later statement on the same doc lands promptly: {:?}",
        r.later.as_ref().map_err(|e| e.message())
    );
}

// ===========================================================================
// CASE 3 — a quiesce arriving while a cross-pool read is straddling a
// held commit: the READ is refused by the write-side gate
// ===========================================================================

/// A read transaction "takes no locks at all ... holding nothing, blocking
/// nobody, retrying on the rare mid-read commit, escalating after a bound".
/// Quiesce is "no new COMMITS begin; in-flight commits
/// finish". A read is not a commit. So a cross-pool read that is
/// straddling an in-flight commit while a quiesce is waiting for that commit
/// to drain must either complete (after the commit finishes) or keep waiting
/// — it must never FAIL because the quiesce flag is up.
///
/// What happens instead: the read is invalidated by the in-landing row eight
/// times, escalates, and the escalation APPENDS A SPINE ROW for its sequence
/// number (`read.rs::escalated_claiming`) — through `append_or_existing`,
/// whose in-transaction quiesce gate refuses it. The reader gets
/// `Busy: the cauldron is quiesced: no new commit begins`, within
/// milliseconds, for a read.
#[test]
fn a_quiesce_never_refuses_a_read() {
    let _env = kill::env_exclusive();
    let name = "rvl-quiesce-read";
    let dir = scratch(name);
    let c = cauldron(&dir, 30_000);
    mint_doc(&c, "a draft the bulk statement will publish");
    let env = envelope_with(BULK_CROSS, &[], "held-door");
    let pause = arm_pause("between-landings", name);

    let (finished_early, reader_outcome, quiesce_outcome, door_outcome) =
        std::thread::scope(|scope| {
            let door = scope.spawn(|| c.submit(env));
            pause.wait_reached(&door);
            pause.disarm();
            let row = kill::the_stuck_row(&dir);
            assert!(row.decided && !row.cleared);
            assert!(row.parts[0].landed && !row.parts[1].landed, "alpha landed, beta not");

            // THE QUIESCE, waiting for the held commit to drain.
            let quiescer = scope.spawn(|| c.quiesce());
            assert!(
                wait_until(Duration::from_secs(5), || {
                    spine::recover::is_quiesced(&kill::spine_of(&dir)).expect("flag")
                }),
                "the quiesce flag goes up first"
            );
            assert!(!quiescer.is_finished(), "quiesce waits for the in-flight commit");

            // THE READ, straddling the held commit.
            let reader = scope.spawn(|| {
                c.read(&ReadRequest {
                    statement: HOP_READ.to_string(),
                    bindings: Bindings::new(),
                    format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
                })
            });
            let finished_early = wait_until(Duration::from_millis(1_500), || reader.is_finished());
            assert!(!door.is_finished(), "the door is still held");

            pause.release();
            let door_outcome = door.join().expect("door");
            let quiesce_outcome = quiescer.join().expect("quiescer");
            let reader_outcome = reader.join().expect("reader");
            (finished_early, reader_outcome, quiesce_outcome, door_outcome)
        });

    assert!(
        matches!(door_outcome, Ok(Outcome::Committed { .. })),
        "the held commit finishes: {door_outcome:?}"
    );
    let token = match quiesce_outcome {
        Ok(token) => token,
        Err(e) => panic!("the quiesce drains: {e:?}"),
    };
    match &reader_outcome {
        Ok(out) => assert_eq!(out.rows.0.len(), 1, "the read sees the whole commit"),
        Err(e) => panic!(
            "a READ failed under quiesce (finished before the commit it straddled was \
             released: {finished_early}): kind `{}`: {}",
            e.kind(),
            e.message()
        ),
    }
    c.resume(&token).expect("resume");
}

// ===========================================================================
// CASE 4 — TIME: a transient spine-busy past the (hard-coded) timeout,
// arriving after the decided flip, wedges a fully landed commit's resources
// for the life of the process
// ===========================================================================

/// "Mechanical writes wait their turn; they are never refused"
/// and "no retry-forever, no silent degradation" together describe a
/// door that finishes what it decided. A commit whose every part has landed
/// and which is one row flip from cleared must not be ABANDONED because the
/// spine file was busy for a few seconds — and whatever the door answers,
/// once the spine is free again the next statement on those resources must
/// land within the configured lock ceiling, not sit behind a ghost.
///
/// What happens instead: the spine connection's busy timeout is a hard-coded
/// 5 s (`spine_db.rs::apply_pragmas`), invisible to `SpineConfig` and SHORTER
/// than the 10 s lock ceiling the design's tests are tuned around. A foreign
/// writer holding `spine.db`'s write lock for 6 s across the door's
/// `mark_cleared` turns that flip into `SpineError::Busy`; `execute` takes
/// its decided arm and RETURNS the error, leaving the row decided, every
/// part landed, un-cleared, its locks granted. Nothing in a live process
/// ever finishes it (`recover()` is the crash-restart entry — and, per
/// cases 1 and 2, unsafe to run beside live doors). Every later statement
/// touching the doc waits to the ceiling and comes back `Busy`, forever.
#[test]
#[ignore = "WRONG TEST in part, SPECIFICATION QUESTION in part. The ROOT it found is fixed: the busy timeout is configuration (SpineConfig::busy_timeout), the door's mechanical writes after the decided flip wait their turn up to lock_wait_ceiling (exec::wait_turn — proven in tests/fix_adversary_reds.rs with a hold shorter than the ceiling), and acquire withdraws its enrolment on any failed poll. As WRITTEN this test cannot pass under any implementation: the holder releases the spine only AFTER door.join() returns, so the door can never clear before returning (assertion 2 is unsatisfiable by construction), and assertion 1 then requires the NEXT door to finish another transaction's decided row — which tests/window_invisible.rs forbids by design (a killed commit's locks must hold until recover()). Whether a live door may run recovery's decided arm on a row that blocks it is an open design question; a hold longer than the ceiling still leaves the row for recover()."]
fn a_transient_spine_busy_after_the_decided_flip_never_wedges_a_landed_commits_resources() {
    let _env = kill::env_exclusive();
    let name = "rvl-transient-busy";
    let dir = scratch(name);
    let c = cauldron(&dir, 2_500);
    let id = mint_doc(&c, SEED_TITLE);
    let env = envelope_with(CROSS_BY_ID, &[("id", serde_json::json!(id))], "held-door");
    let pause = arm_pause("before-cleared", name);

    let (door_outcome, row_seq) = std::thread::scope(|scope| {
        let door = scope.spawn(|| c.submit(env));
        pause.wait_reached(&door);
        pause.disarm();
        let row = kill::the_stuck_row(&dir);
        assert!(row.decided && !row.cleared && row.parts.iter().all(|p| p.landed));

        // A FOREIGN WRITER holds the spine's write lock across the flip —
        // for longer than the hard-coded busy timeout, shorter than anything
        // the design calls pathological.
        let holder = kill::spine_sql(&dir);
        holder
            .execute_batch("BEGIN IMMEDIATE")
            .expect("hold the spine's write lock");
        pause.release();
        let door_outcome = door.join().expect("door");
        holder.execute_batch("COMMIT").expect("let go of the spine");
        (door_outcome, row.seq)
    });

    // Whatever the door answered, the spine is free now. The commit is whole
    // on disk; the next statement on the same doc must land.
    let later = c.submit(envelope_with(
        SET_TITLE,
        &[
            ("id", serde_json::json!(id)),
            ("t", serde_json::json!("after the spine came back")),
        ],
        "later-writer",
    ));
    let after = kill::spine_of(&dir).row_at(row_seq).expect("row");
    assert!(
        matches!(later, Ok(Outcome::Committed { .. })),
        "the doc is wedged after a transient spine-busy: later statement answered {:?}; the \
         door had answered {:?}; row seq {} is decided={} cleared={} parts landed={} with {} lock \
         rows still granted",
        later.as_ref().map(|_| "committed").map_err(|e| e.message()),
        door_outcome.as_ref().map(|_| "committed").map_err(|e| e.message()),
        row_seq,
        after.decided,
        after.cleared,
        after.parts.iter().all(|p| p.landed),
        kill::lock_rows(&dir, row_seq)
    );
    assert!(
        matches!(door_outcome, Ok(Outcome::Committed { .. })),
        "a door one flip from cleared finishes its commit: {:?}",
        door_outcome.as_ref().map_err(|e| e.message())
    );
}

// ===========================================================================
// ATTACKED AND HELD — two recoveries at once; a recovery that died mid-part
// ===========================================================================

/// Two `recover()` passes over ONE crashed decided row, genuinely overlapped:
/// this test holds the alpha pool's write lock so BOTH block inside their
/// first `apply_part`, is seen to be holding both, then lets go. Everything
/// lands once; the row is cleared and marked recovered; no lock remains.
// Held. The idempotence marker inside the landing transaction and the
// "already landed" arm of `mark_part_landed` carry this at N = 2.
#[test]
fn two_concurrent_recoveries_over_one_crashed_row_land_it_exactly_once() {
    let _env = kill::env_shared();
    let dir = scratch("rvl-two-recoveries");
    kill::kill_at("cross-schema", &dir, "decided");
    let row = kill::the_stuck_row(&dir);
    assert!(row.decided && row.parts.iter().all(|p| !p.landed));

    // HOLD alpha's write lock so every landing on it queues behind this test.
    let blocker =
        rusqlite::Connection::open(Pool::path_in(&dir, &spine::schema_def::PoolName::new(ALPHA)))
            .expect("open alpha directly");
    blocker.execute_batch("BEGIN IMMEDIATE").expect("hold alpha's write lock");

    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| Cauldron::open_unrecovered(&dir, SpineConfig::default()).expect("open").recover());
        let b = scope.spawn(|| Cauldron::open_unrecovered(&dir, SpineConfig::default()).expect("open").recover());
        std::thread::sleep(Duration::from_millis(500));
        // BOTH are mid-recovery at once — that is the overlap this test claims.
        assert!(!a.is_finished() && !b.is_finished(), "both recoveries are blocked on alpha");
        blocker.execute_batch("COMMIT").expect("release alpha");
        (a.join().expect("a"), b.join().expect("b"))
    });
    let (a, b) = (a.expect("recovery a"), b.expect("recovery b"));
    for (label, report) in [("a", &a), ("b", &b)] {
        assert!(
            !report.aborted.contains(&row.hash) && !report.skipped_live.contains(&row.hash),
            "recovery {label} never aborts a decided row: {report:?}"
        );
        assert!(
            report.rolled_forward.contains(&row.hash) || report.cleared_only.contains(&row.hash),
            "recovery {label} finished the row: {report:?}"
        );
    }
    let c = kill::open(&dir);
    let after = kill::spine_of(&dir).row_at(row.seq).expect("row");
    kill::assert_all_or_nothing(&c, &after, "decided, two recoveries");
    assert!(after.recovered);
    assert_eq!(kill::ticket_count(&c), 1, "exactly one ticket");
    assert_eq!(kill::lock_rows(&dir, row.seq), 0);
}

/// A recovery that DIED between landing a part in its pool and flipping the
/// spine flag for it — staged by landing part 0 by hand and leaving the flag
/// down — is finished by the next pass without landing anything twice.
// Held. Recovery's per-part loop trusts the pool-side marker, not the flag.
#[test]
fn a_recovery_that_died_between_a_pool_landing_and_its_spine_flag_is_finished_by_the_next_pass() {
    let _env = kill::env_shared();
    let dir = scratch("rvl-died-mid-part");
    kill::kill_at("cross-schema", &dir, "decided");
    let row = kill::the_stuck_row(&dir);
    {
        let mut alpha = Pool::open(&Pool::path_in(&dir, &spine::schema_def::PoolName::new(ALPHA)))
            .expect("open alpha");
        let landed = alpha
            .apply_part(&row.hash, 0, &row.payload.parts[0])
            .expect("land part 0 by hand, as a dying recovery would have");
        assert!(!landed.already_landed);
    }
    assert!(!kill::spine_of(&dir).row_at(row.seq).expect("row").parts[0].landed);
    let c = kill::open(&dir);
    let report = c.recover().expect("the next pass");
    assert!(report.rolled_forward.contains(&row.hash));
    let after = kill::spine_of(&dir).row_at(row.seq).expect("row");
    kill::assert_all_or_nothing(&c, &after, "decided, died mid-part");
    assert_eq!(kill::docs_titled(&c, kill::SEED_TITLE), 1, "the doc was not landed twice");
    assert_eq!(kill::ticket_count(&c), 1);
    assert_eq!(kill::lock_rows(&dir, row.seq), 0);
}

// ===========================================================================
// plumbing — the in-process pause, and the fixtures
// ===========================================================================

/// The seam's PAUSE form, armed in THIS process: a door submitted on a
/// scoped thread holds at `stage`, this thread acts, then releases it. The
/// process-global environment is taken exclusively by every test that uses
/// this (`kill::env_exclusive`).
struct InProcessPause {
    stage: &'static str,
    latch_dir: PathBuf,
    latch: PathBuf,
    reached: PathBuf,
}

fn arm_pause(stage: &'static str, name: &str) -> InProcessPause {
    let latch_dir = test_scratch::tmp_path(format!(
        "spine-adversary-pause-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&latch_dir);
    std::fs::create_dir_all(&latch_dir).expect("latch dir");
    let latch = latch_dir.join("latch");
    let reached = spine::fault::reached_path(&latch);
    std::env::remove_var(spine::fault::FAULT_ENV);
    std::env::remove_var(spine::fault::PAUSE_CEILING_ENV);
    std::env::set_var(spine::fault::PAUSE_LATCH_ENV, latch.as_os_str());
    std::env::set_var(spine::fault::PAUSE_ENV, stage);
    InProcessPause {
        stage,
        latch_dir,
        latch,
        reached,
    }
}

impl InProcessPause {
    /// Block until the library's own marker says the stage was reached —
    /// never a timer — and insist the held thread is still running.
    fn wait_reached<T>(&self, held: &ScopedJoinHandle<'_, T>) {
        let started = Instant::now();
        while !self.reached.is_file() {
            assert!(
                !held.is_finished(),
                "the door returned before reaching `{}`; the pause never opened",
                self.stage
            );
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "the door never reached `{}`",
                self.stage
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            std::fs::read_to_string(&self.reached).expect("marker").trim(),
            self.stage
        );
        assert!(!held.is_finished(), "held after the marker");
    }

    /// Un-arm the pause for THIS thread's own later submits. The held thread
    /// has already passed the stage's env read.
    fn disarm(&self) {
        std::env::remove_var(spine::fault::PAUSE_ENV);
    }

    fn release(&self) {
        std::fs::write(&self.latch, b"released").expect("write the latch");
    }
}

impl Drop for InProcessPause {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.latch, b"released by drop");
        std::env::remove_var(spine::fault::PAUSE_ENV);
        std::env::remove_var(spine::fault::PAUSE_LATCH_ENV);
        std::env::remove_var(spine::fault::PAUSE_CEILING_ENV);
        let _ = std::fs::remove_dir_all(&self.latch_dir);
    }
}

fn wait_until(within: Duration, mut done: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    loop {
        if done() {
            return true;
        }
        if started.elapsed() >= within {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = test_scratch::tmp_path(format!("spine-adversary-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn cauldron(dir: &Path, ceiling_ms: u64) -> Cauldron {
    Cauldron::create(
        dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        SpineConfig {
            lock_wait_ceiling: Duration::from_millis(ceiling_ms),
            ..SpineConfig::default()
        },
    )
    .expect("create the cauldron")
}

fn envelope_with(statement: &str, params: &[(&str, serde_json::Value)], who: &str) -> Envelope {
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
                thread: "adversary-concurrency".into(),
                dispatch: who.into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn mint_doc(c: &Cauldron, title: &str) -> String {
    match c.submit(envelope_with(
        r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" }) RETURN d.id"#,
        &[("t", serde_json::json!(title))],
        "seed",
    )) {
        Ok(Outcome::Committed { returned, .. }) => returned.0[0]["d.id"]
            .as_str()
            .expect("id")
            .to_string(),
        other => panic!("seed: {other:?}"),
    }
}
