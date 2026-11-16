//! ADVERSARY, second pass — lens: WHAT THE FIXES INTRODUCED. QUIESCE
//! OWNERSHIP: the `quiescers` table, the per-handle nonce stack, `resume()` lifting only what
//! its handle took.
//!
//! One residual is already known — a quiescer whose PROCESS dies keeps
//! the cauldron quiesced until an out-of-band `DELETE`. These tests look for
//! the ones it did not name: a nonce the handle drops on a failed write, and
//! a handle shared across threads (the type's own documented use) whose LIFO
//! stack lifts a sibling's quiesce.
//!
//! Red tests are marked `#[ignore = "ADVERSARY RED (fixes): ..."]`; run with
//! `--ignored`. Nothing under `src/` was touched.

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::schema_def::fixtures;
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use spine::{Cauldron, Outcome};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

test_scratch::install!();

/// `resume()` pops the nonce off the handle BEFORE it writes the spine
/// (`recover::resume`: `take_quiesce()` then `spine()` then
/// `remove_quiescer`). If that write answers `Busy` — the spine file held by
/// another writer past `busy_timeout`, the ordinary transient the busy
/// handling exists for — the error goes back to the caller and the nonce is GONE
/// from the handle while the quiescer row STANDS. The next `resume()` on the
/// same handle answers `Ok(false)` ("this handle holds none") and the cauldron
/// is quiesced forever, by a live process that believes it resumed — the
/// exact shape of the known out-of-band residual, produced
/// without any process dying.
///
/// What should be true: a `resume()` that failed lifted nothing, so the
/// handle still holds the quiesce and the NEXT `resume()` lifts it.
// (Ported to the token API — `quiesce()` returns the token that owns the
// quiesce and `resume(&token)` lifts exactly it; every assertion is
// unchanged.)
#[test]
fn a_resume_whose_spine_write_failed_still_holds_the_quiesce_for_the_next_resume() {
    let dir = scratch("resume-busy-orphan");
    let c = Cauldron::create(
        &dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        SpineConfig {
            busy_timeout: Duration::from_millis(50),
            lock_wait_ceiling: Duration::from_millis(2_000),
            ..SpineConfig::default()
        },
    )
    .expect("create");
    let token = c.quiesce().expect("an idle cauldron quiesces at once");
    assert!(is_quiesced(&dir));

    // A FOREIGN WRITER holds the spine's write lock across the resume.
    let holder = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("open spine directly");
    holder.busy_timeout(Duration::from_secs(5)).expect("busy");
    holder.execute_batch("BEGIN IMMEDIATE").expect("hold the spine's write lock");
    let first = c.resume(&token);
    holder.execute_batch("COMMIT").expect("let go");
    // The premise: the write under the hold did not succeed. (If it did, the
    // hold was not a hold and this test proves nothing — say so.)
    assert!(
        first.is_err(),
        "the resume under a held spine was expected to fail transiently; it answered {first:?}"
    );
    assert!(
        is_quiesced(&dir),
        "a resume that failed lifted nothing (the premise): the cauldron is still quiesced"
    );

    // THE ASSERTION: the spine is free, the handle took the quiesce, so the
    // next resume lifts it.
    let second = c.resume(&token).expect("the spine is free now");
    assert!(
        second,
        "resume() after a transiently failed resume answered Ok(false) — 'this handle holds \
         none' — while the quiescer this very handle recorded still stands; nothing in the \
         public API can lift it now (is_quiesced = {})",
        is_quiesced(&dir)
    );
    assert!(!is_quiesced(&dir), "and the cauldron is open again");
    let landed = c.submit(envelope(
        r#"CREATE (d: fixture-alpha/doc { title: "after resume", state: "draft" })"#,
    ));
    assert!(
        matches!(landed, Ok(Outcome::Committed { .. })),
        "a commit begins after the resume: {}",
        landed.as_ref().map(|_| "committed".to_string()).unwrap_or_else(|e| e.message().to_string())
    );
}

/// `Cauldron` is one handle "shareable across writer threads" — its own docs
/// say so, and the nonce stack is a `Mutex` for exactly that reason. Two
/// threads on one handle each `quiesce()` (a backup-taker and a migration
/// rehearsal, say). The first to finish calls `resume()`. `Ok` from
/// `quiesce()` is "quiesced, BY YOU, and drained" (the quiesce API's own
/// contract) and `resume()` is "lift what THIS HANDLE took" — but the handle
/// took two, and it lifts the NEWEST: the other thread's. That thread's drain
/// loop then finds its own nonce gone and answers `Busy: this quiesce was
/// lifted out from under its caller` — to a caller nobody resumed — while
/// the cauldron stays quiesced under the FIRST thread's nonce, which that
/// thread believes it just lifted. Both callers are told the opposite of what
/// happened to them.
///
/// What should be true: a `quiesce()` nobody resumed returns `Ok` when the
/// spine drains, and after ONE resume of TWO quiesces the cauldron is still
/// quiesced. (The second half holds by count; the first is the red.)
// (Ported to the token API; the assertions are unchanged.)
#[test]
fn a_quiesce_nobody_resumed_is_not_reported_lifted_because_a_sibling_thread_resumed_its_own() {
    let dir = scratch("shared-handle-lifo");
    let c = Cauldron::create(
        &dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        SpineConfig {
            lock_wait_ceiling: Duration::from_secs(30),
            ..SpineConfig::default()
        },
    )
    .expect("create");
    // One landed row to copy a well-formed record from.
    c.submit(envelope(
        r#"CREATE (d: fixture-alpha/doc { title: "seed", state: "draft" })"#,
    ))
    .expect("seed");

    // THREAD ONE (this thread) quiesces first, on an idle cauldron: Ok.
    let mine = c.quiesce().expect("first quiesce on an idle cauldron");
    assert_eq!(quiescer_count(&dir), 1);

    // Now a row IN FLIGHT appears — staged by hand (the commit gate refuses
    // an append while quiesced, and this is precisely the shape of a row a
    // door appended just before the gate went up): decided, un-cleared,
    // zero parts, so the drain has to wait and a by-hand cleared flip can
    // finish it.
    let seq = stage_decided_row_by_copy(&dir, "ee");
    assert_eq!(spine_of(&dir).pending().expect("pending").len(), 1);

    let (sibling_outcome, resumed_first) = std::thread::scope(|scope| {
        // THREAD TWO quiesces on the SAME handle and drains.
        let sibling = scope.spawn(|| c.quiesce());
        assert!(
            wait_until(Duration::from_secs(5), || quiescer_count(&dir) == 2),
            "the sibling's quiescer is recorded"
        );
        std::thread::sleep(Duration::from_millis(150));
        assert!(!sibling.is_finished(), "the sibling is draining behind the staged row");

        // THREAD ONE is done with its snapshot and resumes ITS quiesce.
        let resumed_first = c.resume(&mine).expect("resume");
        // Finish the staged row so the sibling's drain can see pending()
        // empty.
        spine_of(&dir)
            .mark_cleared(seq, &serde_json::json!([]))
            .expect("clear the staged row");
        let sibling_outcome = sibling.join().expect("the sibling thread did not panic");
        (sibling_outcome, resumed_first)
    });

    assert!(resumed_first, "thread one's resume lifted one quiesce");
    assert!(
        is_quiesced(&dir),
        "one resume of two quiesces: the cauldron is still quiesced (holds by count)"
    );
    assert!(
        sibling_outcome.is_ok(),
        "a quiesce NOBODY resumed must drain to Ok; the sibling was answered {}. The resumer's \
         handle popped the newest nonce — the sibling's — and left its own standing: {} \
         quiescer(s) outstanding",
        sibling_outcome
            .as_ref()
            .map(|_| "Ok".to_string())
            .unwrap_or_else(|e| format!("{}: {}", e.error.kind(), e.error.message())),
        quiescer_count(&dir)
    );
    // Clean up whatever is left so the scratch directory is not quiesced
    // forever (irrelevant to the assertion above).
    if let Ok(theirs) = &sibling_outcome {
        let _ = c.resume(theirs);
    }
}

// ---------------------------------------------------------------------------
// plumbing
// ---------------------------------------------------------------------------

fn scratch(name: &str) -> PathBuf {
    let dir = test_scratch::tmp_path(format!("spine-adversary-fixes-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn spine_of(dir: &Path) -> SpineDb {
    SpineDb::open(&dir.join(SPINE_FILE)).expect("open the spine")
}

fn is_quiesced(dir: &Path) -> bool {
    spine::recover::is_quiesced(&spine_of(dir)).expect("read the quiesce state")
}

fn quiescer_count(dir: &Path) -> i64 {
    rusqlite::Connection::open(dir.join(SPINE_FILE))
        .expect("spine")
        .query_row("SELECT COUNT(*) FROM quiescers", [], |r| r.get(0))
        .expect("count")
}

/// A decided, un-cleared, zero-part row, copied from the first landed row's
/// well-formed columns under a fresh hash — staged by direct SQL because the
/// append gate (correctly) refuses a commit's append while quiesced.
fn stage_decided_row_by_copy(dir: &Path, hex_byte: &str) -> Seq {
    let conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("spine");
    let hash = hex_byte.repeat(32);
    conn.execute(
        "INSERT INTO transactions
           (hash, attempt, ts, envelope_version, initiator, instrument, statement,
            bindings, payload, touched, decided, cleared)
         SELECT ?1, 1, ts, envelope_version, initiator, instrument, statement,
                bindings, '{\"parts\":[],\"minted\":{}}', '[]', 1, 0
         FROM transactions ORDER BY seq LIMIT 1",
        rusqlite::params![hash],
    )
    .expect("stage the row");
    Seq(conn.last_insert_rowid())
}

fn envelope(statement: &str) -> Envelope {
    Envelope {
        statement: statement.to_string(),
        bindings: Bindings::new(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent {
                thread: "adversary-fixes".into(),
                dispatch: "quiesce".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
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

#[allow(dead_code)]
fn kind_of(r: &Result<(), SpineError>) -> &'static str {
    match r {
        Ok(()) => "ok",
        Err(e) => e.kind(),
    }
}
