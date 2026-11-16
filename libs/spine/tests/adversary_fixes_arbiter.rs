//! ADVERSARY, second pass — lens: WHAT THE FIXES INTRODUCED. THE CLEARED-FLIP
//! ARBITER, attacked on both arms and with three contenders.
//!
//! The fix made the cleared flip conditional on both sides and moved lock
//! release BEHIND A WON FLIP. That is the right arbiter for the flip. It is
//! the wrong rule for the RELEASE: before the fix the live door released its
//! own row's locks unconditionally on its way out; now a door that LOSES the
//! flip returns to its caller without touching the lock table, trusting the
//! winner to release. The winner is a `recover()` in another process, whose
//! flip-then-release is exactly the two-step the orphan sweep exists for:
//! it can die, or be descheduled, between the two. The door — alive, holding
//! the row's sequence number, and able to see the row is CLEARED — walks
//! away from lock rows it enrolled, and the next writer on the doc waits to
//! the ceiling behind a ghost. The fix applied the correct reasoning
//! ("a cleared row's locks are orphans by the sweep's own reasoning, and this
//! door, which enrolled them, releases them on its way out") to the ABORT arm
//! in `abandon_attempt`, and not to the decided arm at phase 13.
//!
//! The interleaving is proven with the seam's in-process pause form, as the
//! first concurrency adversary did. Red tests are marked
//! `#[ignore = "ADVERSARY RED (fixes): ..."]`; run with
//! `cargo test -p spine --features fault-injection --test adversary_fixes_arbiter -- --include-ignored --test-threads=1`.
//! Nothing under `src/` was touched.

#![cfg(all(unix, feature = "fault-injection"))]

#[path = "util/kill.rs"]
mod kill;

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::schema_def::fixtures;
use spine::spine_db::Seq;
use spine::{Cauldron, Outcome};
use std::path::{Path, PathBuf};
use std::thread::ScopedJoinHandle;
use std::time::{Duration, Instant};

test_scratch::install!();

const CROSS_BY_ID: &str = r#"MATCH (d: fixture-alpha/doc { id: $id })
   EXPECT d.state = "draft"
   CREATE (t: fixture-beta/ticket { state: "open" })
   CREATE (t) -[catalogues]-> (d)
   SET d.state = "published"
   RETURN d.title AS title, d.state AS state, t.id AS ticket"#;

const SET_TITLE: &str = r#"MATCH (d: fixture-alpha/doc { id: $id })
   SET d.title = $t
   RETURN d.title AS title"#;

const SEED_TITLE: &str = "seed title, projected under the row's own locks";

// ===========================================================================
// FINDING — the door that LOSES the cleared flip walks away from its locks
// ===========================================================================

struct LostFlip {
    seq: Seq,
    door: Result<Outcome, SpineError>,
    lock_rows_after_door_returned: usize,
    row_after: spine::spine_db::TxRow,
    later: Result<Outcome, SpineError>,
    later_took: Duration,
}

/// Hold a two-pool door at `before-cleared` (decided, every part landed,
/// every lock held). Make the OTHER finisher's cleared flip by hand — the
/// shape of a `recover()` that won the flip and died, or is still
/// descheduled, before its `release_at_clear` — and leave the lock rows in
/// place, exactly as that death leaves them. Release the door. It loses the
/// flip and returns the stored outcome. Then look at the lock table, and at
/// what the next statement on the doc is told.
fn race_a_lost_cleared_flip(name: &str, ceiling_ms: u64) -> LostFlip {
    let dir = scratch(name);
    let c = cauldron(&dir, ceiling_ms);
    let id = mint_doc(&c, SEED_TITLE);
    let env = envelope_with(CROSS_BY_ID, &[("id", serde_json::json!(id))], "held-door");
    let pause = arm_pause("before-cleared", name);

    std::thread::scope(|scope| {
        let door = scope.spawn(|| c.submit(env));
        pause.wait_reached(&door);
        pause.disarm();

        let row = kill::the_stuck_row(&dir);
        assert!(
            row.decided && !row.cleared && row.parts.iter().all(|p| p.landed),
            "held at before-cleared: {row:?}"
        );
        let held = kill::lock_rows(&dir, row.seq);
        assert!(held > 0, "the held door holds its locks");

        // THE OTHER FINISHER'S FLIP, without its release: what recovery's
        // `mark_cleared_recovered` writes, taken the instant before its
        // `release_at_clear` — and then the process is gone.
        let stored = serde_json::json!([{
            "title": SEED_TITLE,
            "state": "published",
            "ticket": "projected by the other finisher"
        }]);
        assert!(
            kill::spine_of(&dir)
                .mark_cleared(row.seq, &stored)
                .expect("the other finisher's cleared flip"),
            "the by-hand flip won (the row was un-cleared)"
        );
        assert_eq!(
            kill::lock_rows(&dir, row.seq),
            held,
            "the flip alone releases nothing (release is a second step)"
        );
        assert!(!door.is_finished(), "the door is still held");

        pause.release();
        let door_outcome = door.join().expect("the door thread did not panic");
        let lock_rows_after_door_returned = kill::lock_rows(&dir, row.seq);
        let row_after = kill::spine_of(&dir).row_at(row.seq).expect("row");

        let started = Instant::now();
        let later = c.submit(envelope_with(
            SET_TITLE,
            &[
                ("id", serde_json::json!(id)),
                ("t", serde_json::json!("a later statement on the same doc")),
            ],
            "later-writer",
        ));
        LostFlip {
            seq: row.seq,
            door: door_outcome,
            lock_rows_after_door_returned,
            row_after,
            later,
            later_took: started.elapsed(),
        }
    })
}

/// A door that has RETURNED holds no lock (the rule the fix already
/// applies on the abort arm). The row is CLEARED — its locks are orphans by the sweep's own
/// definition — and the door is the one live party that knows it. Whatever
/// the door answers, the lock rows for its sequence number are gone when it
/// returns.
#[test]
fn a_door_that_lost_the_cleared_flip_leaves_no_lock_behind() {
    let _env = kill::env_exclusive();
    let r = race_a_lost_cleared_flip("fx-lost-flip-locks", 4_000);
    assert!(
        matches!(r.door, Ok(Outcome::Committed { .. })),
        "the door completes its commit from the stored outcome: {:?}",
        r.door.as_ref().map_err(|e| e.message())
    );
    assert!(r.row_after.cleared, "the row is cleared: {:?}", r.row_after);
    assert_eq!(
        r.lock_rows_after_door_returned, 0,
        "lock rows still granted for seq {} after the door returned Committed on a CLEARED row",
        r.seq
    );
}

/// The consequence, as the next writer sees it: the doc is not wedged behind
/// a cleared row. The next statement lands within the ceiling rather than
/// being answered `Busy` naming a holder whose transaction is over.
#[test]
fn the_next_statement_after_a_lost_cleared_flip_is_not_busy_behind_a_cleared_row() {
    let _env = kill::env_exclusive();
    let r = race_a_lost_cleared_flip("fx-lost-flip-next", 1_500);
    assert!(
        matches!(r.later, Ok(Outcome::Committed { .. })),
        "the next statement on the doc answered {:?} after {:?}; lock rows for the cleared \
         row seq {}: {}",
        r.later.as_ref().map(|_| "committed").map_err(|e| e.message()),
        r.later_took,
        r.seq,
        r.lock_rows_after_door_returned
    );
}

// ===========================================================================
// THREE CONTENDERS — a held door and TWO recoveries at once, both arms
// ===========================================================================

/// The decided arm at N = 3: a door held at `decided` (nothing landed, every
/// lock held) with two `recover()` passes released against it together, then
/// the door released. Exactly one landing, the counter moves exactly once,
/// the door's reply is the projection under the row's locks, the row is
/// cleared with a stored result, and no lock survives.
#[test]
fn a_held_door_and_two_recoveries_finish_one_commit_exactly_once() {
    let _env = kill::env_exclusive();
    let name = "fx-three-decided";
    let dir = scratch(name);
    let c = cauldron(&dir, 4_000);
    let id = mint_doc(&c, SEED_TITLE);
    let counter_before = c.committed_commit_counter().expect("counter");
    let env = envelope_with(CROSS_BY_ID, &[("id", serde_json::json!(id))], "held-door");
    let pause = arm_pause("decided", name);

    let (door_outcome, ra, rb, seq) = std::thread::scope(|scope| {
        let door = scope.spawn(|| c.submit(env));
        pause.wait_reached(&door);
        pause.disarm();
        let row = kill::the_stuck_row(&dir);
        assert!(row.decided && !row.cleared && row.parts.iter().all(|p| !p.landed));

        let ra = scope.spawn(|| c.recover());
        let rb = scope.spawn(|| c.recover());
        let ra = ra.join().expect("a");
        let rb = rb.join().expect("b");
        assert!(!door.is_finished(), "the door is still held after both recoveries");
        pause.release();
        let door_outcome = door.join().expect("door");
        (door_outcome, ra, rb, row.seq)
    });
    let (ra, rb) = (ra.expect("recovery a"), rb.expect("recovery b"));
    let after = kill::spine_of(&dir).row_at(seq).expect("row");

    assert!(
        matches!(door_outcome, Ok(Outcome::Committed { .. })),
        "the door completes: {:?}",
        door_outcome.as_ref().map_err(|e| e.message())
    );
    assert_eq!(title_of(&door_outcome).as_deref(), Some(SEED_TITLE));
    assert!(after.cleared && after.decided && after.result.is_some());
    assert_eq!(kill::ticket_count(&c), 1, "exactly one ticket");
    assert_eq!(
        c.committed_commit_counter().expect("counter"),
        counter_before + 1,
        "the counter moved exactly once for one commit"
    );
    assert_eq!(kill::lock_rows(&dir, seq), 0, "no lock survives");
    for (label, report) in [("a", &ra), ("b", &rb)] {
        assert!(
            !report.aborted.contains(&after.hash),
            "recovery {label} never aborts a decided row: {report:?}"
        );
    }
    // Between them, the two passes and the door finished the row; the reports
    // agree with the row's mark.
    let recovery_finished = ra.rolled_forward.contains(&after.hash)
        || ra.cleared_only.contains(&after.hash)
        || rb.rolled_forward.contains(&after.hash)
        || rb.cleared_only.contains(&after.hash);
    assert_eq!(
        after.recovered, recovery_finished,
        "the row's recovered mark agrees with the reports: {ra:?} / {rb:?}"
    );
}

/// The un-decided arm at N = 3: a door held at `row-appended` with two
/// `recover()` passes released against it together. Both abort arbiters
/// contend for one row; the door then restarts underfoot. The caller is never
/// blamed, nothing is half-landed, no lock survives, and exactly one report
/// aborts the row.
#[test]
fn a_held_door_and_two_recoveries_on_its_undecided_row_restart_it_once() {
    let _env = kill::env_exclusive();
    let name = "fx-three-undecided";
    let dir = scratch(name);
    let c = cauldron(&dir, 4_000);
    let id = mint_doc(&c, SEED_TITLE);
    let env = envelope_with(CROSS_BY_ID, &[("id", serde_json::json!(id))], "held-door");
    let pause = arm_pause("row-appended", name);

    let (door_outcome, ra, rb, seq) = std::thread::scope(|scope| {
        let door = scope.spawn(|| c.submit(env));
        pause.wait_reached(&door);
        pause.disarm();
        let row = kill::the_stuck_row(&dir);
        assert!(!row.decided && !row.cleared);
        let ra = scope.spawn(|| c.recover());
        let rb = scope.spawn(|| c.recover());
        let ra = ra.join().expect("a");
        let rb = rb.join().expect("b");
        assert!(!door.is_finished(), "the door is still held after both recoveries");
        pause.release();
        let door_outcome = door.join().expect("door");
        (door_outcome, ra, rb, row.seq)
    });
    let (ra, rb) = (ra.expect("recovery a"), rb.expect("recovery b"));
    match &door_outcome {
        Ok(Outcome::Committed { .. }) => {}
        Ok(other) => panic!("a commit envelope came back {other:?}"),
        Err(e) => {
            let kind = e.kind();
            assert!(
                kind == "busy" || kind == "refused" || kind == "halted",
                "the door answered a correct statement with `{kind}`: {}",
                e.message()
            );
        }
    }
    let first = kill::spine_of(&dir).row_at(seq).expect("row");
    assert!(first.cleared && !first.decided, "the first attempt is recorded aborted");
    assert_eq!(kill::lock_rows(&dir, seq), 0, "no lock survives on the aborted attempt");
    let aborts = [&ra, &rb]
        .iter()
        .filter(|r| r.aborted.contains(&first.hash))
        .count();
    assert_eq!(aborts, 1, "exactly one pass aborted the row: {ra:?} / {rb:?}");
    if matches!(door_outcome, Ok(Outcome::Committed { .. })) {
        assert_eq!(kill::ticket_count(&c), 1, "the restarted attempt landed once");
    } else {
        assert_eq!(kill::ticket_count(&c), 0, "an error answer means nothing landed");
    }
    // Nothing on the restarted attempt's locks either.
    let total: i64 = kill::spine_sql(&dir)
        .query_row("SELECT COUNT(*) FROM locks", [], |r| r.get(0))
        .expect("count");
    assert_eq!(total, 0, "no lock survives anywhere once everyone has returned");
}

// ===========================================================================
// plumbing — the in-process pause, and the fixtures (as in the first adversary file)
// ===========================================================================

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

struct InProcessPause {
    stage: &'static str,
    latch_dir: PathBuf,
    latch: PathBuf,
    reached: PathBuf,
}

fn arm_pause(stage: &'static str, name: &str) -> InProcessPause {
    let latch_dir = test_scratch::tmp_path(format!(
        "spine-adversary-fixes-pause-{name}-{}",
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

fn scratch(name: &str) -> PathBuf {
    let dir = test_scratch::tmp_path(format!("spine-adversary-fixes-{name}-{}", std::process::id()));
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
                thread: "adversary-fixes".into(),
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
