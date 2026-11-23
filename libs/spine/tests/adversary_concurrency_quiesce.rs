//! ADVERSARY — lens: CRASH, CONCURRENCY AND TIME.
//!
//! **Quiesce at N = 2.** The quiesce hook ("no new commits begin;
//! in-flight commits finish", so that "a whole-cauldron backup snapshot never
//! contains a half-landed commit") arbitrates between whoever is taking a
//! snapshot and every writer. Everything that arbitrates is proven at two or
//! more (the N >= 2 rule), and the second party here
//! is not only a writer — it is a SECOND quiescer, or a resumer. The hook is
//! one boolean column (`spine_state.quiesced`) with no owner and no count:
//! `quiesce()` sets it, `resume()` clears it, and neither knows who else is
//! relying on it. Both tests below run in the default build (no seam), and
//! both are marked `#[ignore = "ADVERSARY RED: ..."]` so the default run
//! stays green; run with `--ignored`. Nothing under `src/` was touched.

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::schema_def::fixtures;
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use spine::{Cauldron, Outcome};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

test_scratch::install!();

/// Two backup-takers quiesce the same cauldron; the first finishes and
/// resumes. The second's snapshot is still in progress, so the cauldron must
/// STILL be quiesced: no new commit may begin until the second resumes too.
///
/// What happens instead: the second `quiesce()` re-sets the same flag, the
/// first `resume()` clears it, and a commit lands in the middle of the
/// second caller's snapshot — the half-landed-commit-in-a-backup the hook
/// exists to prevent, delivered by the hook's own second user.
#[test]
fn a_quiesce_held_by_two_callers_survives_the_first_resume() {
    let dir = scratch("quiesce-two-callers");
    let c = cauldron(&dir, 2_000);

    // (Fixing pass two: a quiesce is owned by its call through the token
    // `quiesce()` returns, and `resume(&token)` lifts exactly that one.)
    let first = c.quiesce().expect("the first backup-taker quiesces");
    let second = c.quiesce().expect("the second backup-taker quiesces");
    assert!(is_quiesced(&dir));

    // THE FIRST FINISHES ITS SNAPSHOT AND RESUMES.
    c.resume(&first).expect("the first backup-taker resumes");

    // THE SECOND IS STILL SNAPSHOTTING.
    assert!(
        is_quiesced(&dir),
        "a second quiesce is still outstanding, so the cauldron must still be quiesced"
    );
    let r = submit(
        &c,
        r#"CREATE (d: fixture-alpha/doc { title: "landed inside a backup", state: "draft" })"#,
    );
    assert!(
        matches!(r, Err(SpineError::Busy { .. })),
        "no new commit begins while any quiesce is outstanding; got {r:?}"
    );
    c.resume(&second).expect("the second resumes");
    assert!(!is_quiesced(&dir));
}

/// `quiesce()` returning `Ok` is a POSTCONDITION a backup-taker acts on: the
/// cauldron is quiesced AND drained, and it stays quiesced until this caller
/// resumes (`recover::quiesce`'s docs: "returns when the spine has drained";
/// the timeout arm insists "the quiesce flag is still set — call resume()").
/// A second actor calling `resume()` while the first is still draining must
/// not turn that `Ok` into a lie.
///
/// What happens instead: the drain loop never re-reads the flag it set. A
/// `resume()` from anyone lifts it mid-drain; a commit then BEGINS AND LANDS
/// during "the drain"; the staged row finishes; the loop sees `pending()`
/// empty and returns `Ok` — to a caller whose cauldron is neither quiesced
/// nor quiet, and never was for the whole of its "drained" wait.
#[test]
fn a_quiesce_that_returns_ok_has_not_been_resumed_out_from_under_its_caller() {
    let dir = scratch("quiesce-resumed-mid-drain");
    let c = cauldron(&dir, 30_000);
    // (Fixing pass two: `resume` takes the token `quiesce()` issued, so "a
    // second actor resumes" can only mean a second actor resuming a quiesce
    // it took ITSELF — the nearest executable form of this attack. That
    // actor's token is taken first, on the idle cauldron; the in-flight row
    // is then staged by direct copy, because the append gate — correctly —
    // refuses a commit's append while quiesced. The assertions below are
    // the original ones, untouched.)
    submit(
        &c,
        r#"CREATE (d: fixture-alpha/doc { title: "seed", state: "draft" })"#,
    )
    .expect("a landed row to copy a well-formed record from");
    let second_actor = c.quiesce().expect("the second actor's own quiesce, on an idle cauldron");
    // ONE decided, un-cleared row: in flight, so the drain loop has to wait.
    let seq = stage_decided_row_by_copy(&dir, "a1");

    let quiescer = {
        let dir = dir.clone();
        std::thread::spawn(move || {
            Cauldron::open_unrecovered(&dir, config(30_000))
                .expect("open")
                .quiesce()
        })
    };
    assert!(
        wait_until(Duration::from_secs(5), || quiescer_count(&dir) == 2),
        "the flag goes up first"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(!quiescer.is_finished(), "the quiesce is draining behind the staged row");

    // A SECOND ACTOR RESUMES, mid-drain.
    c.resume(&second_actor).expect("resume by a second actor");

    // Observation, not the assertion: a commit now begins and lands during
    // what the first caller believes is its drain.
    let landed_mid_drain = submit(
        &c,
        r#"CREATE (d: fixture-alpha/doc { title: "landed mid-drain", state: "draft" })"#,
    );

    // Finish the staged row so the drain loop can see `pending()` empty.
    spine_of(&dir)
        .mark_cleared(seq, &serde_json::json!({ "staged": true }))
        .expect("cleared");
    let outcome = quiescer.join().expect("the quiescing thread did not panic");
    assert!(outcome.is_ok(), "the drain completed: {outcome:?}");

    assert!(
        is_quiesced(&dir),
        "quiesce() returned Ok, so its caller's cauldron must be quiesced — but a second \
         actor's resume lifted it mid-drain and a commit landed meanwhile: {:?}",
        landed_mid_drain.as_ref().map(|_| "committed").map_err(|e| e.kind())
    );
}

// ---------------------------------------------------------------------------
// plumbing
// ---------------------------------------------------------------------------

fn scratch(name: &str) -> PathBuf {
    let dir = test_scratch::tmp_path(format!("spine-adversary-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn config(ceiling_ms: u64) -> SpineConfig {
    SpineConfig {
        lock_wait_ceiling: Duration::from_millis(ceiling_ms),
        ..SpineConfig::default()
    }
}

fn cauldron(dir: &Path, ceiling_ms: u64) -> Cauldron {
    Cauldron::create(
        dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        config(ceiling_ms),
    )
    .expect("create the cauldron")
}

fn spine_of(dir: &Path) -> SpineDb {
    SpineDb::open(&dir.join(SPINE_FILE)).expect("open the spine")
}

fn is_quiesced(dir: &Path) -> bool {
    spine::recover::is_quiesced(&spine_of(dir)).expect("read the quiesce flag")
}

fn quiescer_count(dir: &Path) -> i64 {
    rusqlite::Connection::open(dir.join(SPINE_FILE))
        .expect("spine")
        .query_row("SELECT COUNT(*) FROM quiescers", [], |r| r.get(0))
        .expect("count")
}

/// A decided, un-cleared, zero-part row copied from the first landed row's
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
                thread: "adversary-concurrency".into(),
                dispatch: "quiesce".into(),
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
