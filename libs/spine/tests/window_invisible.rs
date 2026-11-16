//! **The half-landed window is invisible,
//! three ways.**
//!
//! Between one pool's landing and the next the pools genuinely disagree: the
//! commit point is the SPINE ROW, not the storage engine, so there is a real
//! interval in which alpha holds this commit's half and beta does not. There
//! are three mechanisms that each close one leak path, and
//! *each of the three leaks without the other two*. So there is ONE TEST
//! PER LEAK PATH here, and every one of them runs against a window that is
//! genuinely open at the moment it asserts:
//!
//! 1. a lock-taker WAITS — locks release at the cleared flip only, never as
//!    parts land;
//! 2. a lock-free validated read straddling the window RETRIES and never
//!    returns the mixed state;
//! 3. a single-pool snapshot never sees a TORN per-pool landing.
//!
//! # How the window is held open — the real pipeline's window
//!
//! This file was first built against a HAND-DRIVEN window: a worker thread
//! replayed `exec::execute`'s phases through the crate's public primitives
//! and parked on a condvar between alpha's landing and beta's, because the
//! fault seam then had only a KILL form. That harness was a drift hazard
//! arriving through the tests: two descriptions of the
//! commit path, one in `exec.rs` and one in a test, drifting exactly where
//! atomicity is being proven.
//!
//! The seam now has a PAUSE form (`src/fault.rs`), and this file
//! uses it and nothing else. The window is produced by `Cauldron::submit`
//! ITSELF, in a child process (`examples/spine_victim.rs`, job `windowed`),
//! held at `recover::Stage::BetweenLandings` by `SPINE_PAUSE=between-landings`
//! until the test releases the latch. Nothing about the state on disk is
//! simulated or replayed: the spine row is decided and un-cleared, alpha's
//! part is landed and flagged, beta's is not, the lock rows are granted, and
//! `in_landing` reports the transaction over both pools — all of it written
//! by the one door. Every other party in these tests — the lock-taker, the
//! validated read, the snapshot — arrives through the same door in the
//! parent. **No hand-built window machinery remains in this file**; nothing
//! the old harness expressed is inexpressible by the pause form.
//!
//! The cost, stated: the three leak-path tests are now `fault-injection`
//! only, like every other seam-dependent suite in this crate — the pause
//! form is compiled out of the production build by the same rule that
//! compiles the kill form out. The racing half of leak path 3 needs no window and runs
//! in both builds.
//!
//! The kill-form counterpart at the end of the file runs the same three
//! assertions against the window a DEAD door leaves (killed at the same
//! stage), so the held window and the crashed window are shown to be the
//! same window.
//!
//! Assertions are over STRUCTURE — a row's flags, a lock queue, a node count,
//! an `Escalation` variant. No test in this file asserts on prose.
//!
//! Every checker here was forced RED at least once; the poison is named in a
//! `// RED:` comment above each test.

#[cfg(all(unix, feature = "fault-injection"))]
#[path = "util/kill.rs"]
mod kill;

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::pool::{NodeId, Pool};
use spine::schema_def::{fixtures, PoolName};
use spine::{Cauldron, Outcome};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

test_scratch::install!();

#[cfg(all(unix, feature = "fault-injection"))]
use spine::error::SpineError;
#[cfg(all(unix, feature = "fault-injection"))]
use spine::ReadRequest;
#[cfg(all(unix, feature = "fault-injection"))]
use spine::ladle::compile;
#[cfg(all(unix, feature = "fault-injection"))]
use spine::ladle::parse;
#[cfg(all(unix, feature = "fault-injection"))]
use spine::locks::{self, Resource};
#[cfg(all(unix, feature = "fault-injection"))]
use spine::read::{self, Escalation};
#[cfg(all(unix, feature = "fault-injection"))]
use spine::schema_def::PoolGrammar;
#[cfg(all(unix, feature = "fault-injection"))]
use spine::spine_db::{Seq, SpineDb, TxRow, SPINE_FILE};
#[cfg(all(unix, feature = "fault-injection"))]
use std::time::Instant;

const ALPHA: &str = "fixture-alpha";
#[cfg(all(unix, feature = "fault-injection"))]
const BETA: &str = "fixture-beta";

/// How many alpha docs the windowed statement moves. Deliberately > 1: the
/// per-pool landing this suite calls un-tearable has to have something to
/// tear, and a one-delta part cannot exhibit it. The victim's `windowed` job
/// publishes EVERY draft in alpha, so the parent seeds this many.
const DOCS: usize = 6;

/// A cross-pool read: its footprint names BOTH pools, so it is a validated
/// snapshot read rather than the single-pool exemption.
#[cfg(all(unix, feature = "fault-injection"))]
const CROSS_POOL_READ: &str =
    r#"MATCH (t: fixture-beta/ticket) -[catalogues]-> (d: fixture-alpha/doc) RETURN d.id"#;

// ---------------------------------------------------------------------------
// leak path 1 — a lock-taker waits; locks release at the cleared flip only
// ---------------------------------------------------------------------------

/// Release: all locks, at the cleared flip only.
///
/// The discriminating fact is asserted at the moment it discriminates: ALPHA'S
/// PART HAS LANDED (the row says so, and the pool shows it), the row is not
/// cleared, and a door submission that wants one of the landed rows is
/// enrolled in the lock table and NOT GRANTED. A build that released a
/// transaction's alpha locks when alpha's part landed — the obvious and wrong
/// optimization — would grant this waiter inside the window and hand it a
/// world where alpha moved and beta did not.
///
/// The holder is the REAL DOOR in a child process, held at
/// `between-landings`; the waiter is an ordinary `Cauldron::submit` in the
/// parent; the holder's state on disk is asserted rather than assumed.
// RED (against the real pipeline): `locks::release_at_clear(spine,
// seq)` inserted into `exec` phase 12 right after each `mark_part_landed` —
// mutation Y, the per-part release this clause forbids, which
// the hand-driven window could NOT detect. It reds at `assert_window_open`
// ("the window holds a granted lock on ...: []"): with the locks gone the
// window is not the window this test is about, and it says so before a
// single waiter is spawned. Also red by the seam call removed from `exec`
// (the victim exited 4; the pause never opened). Also:
// `SpineError::Busy`'s holder naming removed from `acquire`'s ceiling path —
// the short-ceiling arm stopped naming the holder.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn a_lock_taker_waits_through_the_half_landed_window_and_is_freed_only_at_the_cleared_flip() {
    let _env = kill::env_shared();
    let name = "lock-taker";
    let dir = root(name);
    let c = fresh_cauldron(&dir, patient());
    let docs = seed_drafts(&c, DOCS);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");

    let window = open_window(&dir, name);
    let held = assert_window_open(&spine);

    // Alpha's half is on disk and beta's is not — the window, exhibited.
    assert_eq!(published(&c), DOCS, "alpha's part landed");
    assert_eq!(tickets(&c), 0, "beta's part did not");

    // A SHORT-CEILINGED door call refuses LOUDLY rather than hanging, and it
    // names the holder it is behind.
    let impatient = Cauldron::open_unrecovered(&dir, cfg(120)).expect("second handle");
    let err = impatient
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t"#,
            &[
                ("id", serde_json::json!(docs[0].0)),
                ("t", serde_json::json!("never lands")),
            ],
        ))
        .expect_err("a writer cannot take a body the window still holds");
    match &err {
        SpineError::Busy { message } => assert!(
            message.contains(&format!("seq {}", held.seq)),
            "the wait names the holder it is behind: {message}"
        ),
        other => panic!("expected a loud Busy at the ceiling, got {other:?}"),
    }

    // Now a PATIENT door call, which must wait for the cleared flip and then
    // succeed — never be refused.
    let waiter_body = docs[1].clone();
    let out = std::thread::scope(|scope| {
        let door = &c;
        let target = waiter_body.clone();
        let waiting = scope.spawn(move || {
            door.submit(commit(
                r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t"#,
                &[
                    ("id", serde_json::json!(target.0)),
                    ("t", serde_json::json!("landed after the flip")),
                ],
            ))
        });

        // Wait until the waiter's claim is physically QUEUED — the state that
        // says "it asked and it did not get it", with no sleep race.
        let waiter_seq = await_foreign_queued(&spine, &waiter_body, held.seq);

        // THE ASSERTION THIS TEST EXISTS FOR. Re-read the row from disk: the
        // transaction's alpha part is landed, its beta part is not, it is not
        // cleared — and the waiter is still ungranted.
        let row = spine.row_at(held.seq).expect("row");
        assert!(row.decided && !row.cleared);
        assert!(row.parts[0].landed && !row.parts[1].landed);
        assert_eq!(
            grant_state(&spine, &waiter_body, waiter_seq),
            Some(false),
            "a landed part is NOT a release"
        );
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(
            grant_state(&spine, &waiter_body, waiter_seq),
            Some(false),
            "and it stays unreleased for as long as the window is open"
        );

        // Release the held door; it lands beta, clears, and releases at
        // exactly that flip. `release` returns only once the child has
        // exited, so nothing below races the flip.
        window.release_and_expect_exit_zero();
        waiting.join().expect("join")
    })
    .expect("the waiter is granted once the window clears, never refused");

    match out {
        Outcome::Committed { .. } => {}
        other => panic!("expected Committed, got {other:?}"),
    }

    // After the cleared flip: every lock of the window's transaction is gone,
    // and BOTH halves of the commit are visible.
    let row = spine.row_at(held.seq).expect("row");
    assert!(row.cleared && row.parts.iter().all(|p| p.landed));
    for d in &docs {
        assert!(
            locks::queue(&spine, &PoolName::new(ALPHA), &body(d))
                .expect("queue")
                .iter()
                .all(|e| e.seq != held.seq),
            "every lock released at the cleared flip"
        );
    }
    assert_eq!(tickets(&c), 1, "beta's half is visible after the flip");
    assert_eq!(
        title_of(&c, &waiter_body.0),
        Some("landed after the flip".into())
    );
}

// ---------------------------------------------------------------------------
// leak path 2 — a lock-free validated read retries; never the mixed state
// ---------------------------------------------------------------------------

/// A reader straddling a cross-pool commit must never return a
/// combined state that never existed. The leak is closed by VALIDATION, not by
/// locking, so this test watches the validation loop from the inside.
///
/// Two halves, deliberately:
///
/// - the REAL read door (`Cauldron::read`) meets the open window and comes
///   back a bounded, loud failure — never an `Ok` carrying the mixed state;
/// - the validated-read driver is then run directly with an instrumented query closure,
///   which records what EVERY attempt saw. Four attempts observe the mixed
///   state (alpha moved, beta empty); the driver discards all four, escalates,
///   and the value it finally RETURNS is the consistent post-commit cut. The
///   mixed state is seen four times and returned zero times, which is the
///   whole claim.
// RED (against the real pipeline): `read::window_is_consistent`'s
// `in_landing` half deleted — half one's real read door returned a cut
// (`Returned([])`, retries 0, escalated false) across the held window; and
// mutation Y (per-part lock release in `exec` phase 12) —
// `assert_window_open` found no granted lock for the held row. Also: the
// counter half deleted as well — same, one attempt earlier; the
// window released BEFORE the read starts — the "every discarded attempt was
// mixed" assertion fails, which is what keeps this test honest about
// straddling.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn a_validated_read_straddling_the_window_retries_and_never_returns_the_mixed_state() {
    let _env = kill::env_shared();
    let name = "straddle";
    let dir = root(name);
    let c = fresh_cauldron(&dir, patient());
    seed_drafts(&c, DOCS);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");

    let window = open_window(&dir, name);
    assert_window_open(&spine);

    // --- half one: the real read door, against the open window.
    let bounded = Cauldron::open_unrecovered(
        &dir,
        SpineConfig {
            retry_bound: 2,
            lock_wait_ceiling: Duration::from_millis(250),
            lock_poll_interval: Duration::from_millis(2),
            ..SpineConfig::default()
        },
    )
    .expect("second handle");
    match bounded.read(&ReadRequest {
        statement: CROSS_POOL_READ.to_string(),
        bindings: Bindings::new(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    }) {
        Err(SpineError::Busy { .. }) => {}
        Ok(out) => panic!(
            "a cross-pool read straddling the window returned a cut: {:?} (retries {}, escalated \
             {})",
            out.rows, out.retries, out.escalated
        ),
        other => panic!("expected a bounded, loud failure, got {other:?}"),
    }

    // --- half two: the validated-read driver, instrumented.
    let config = SpineConfig {
        // Three retries, so the FOURTH lock-free attempt is the last one
        // before escalation — that is the attempt inside which the window is
        // released, so the escalation never has to queue behind the window's
        // own link claim on beta's ticket slice.
        retry_bound: 3,
        lock_poll_interval: Duration::from_millis(1),
        lock_wait_ceiling: Duration::from_secs(20),
        ..SpineConfig::default()
    };
    let pools = vec![PoolName::new(ALPHA), PoolName::new(BETA)];
    let stmt = parse::parse(CROSS_POOL_READ).expect("parse");
    let grammars = grammar_lookup();
    let claims = compile::claim_footprint(&stmt, &grammars).expect("read claims");

    let mut window = Some(window);
    let mut seen: Vec<(usize, usize)> = Vec::new();
    let result = read::run_validated(&c, &pools, &config, &claims, |snaps| {
        let alpha = snaps.of(&PoolName::new(ALPHA))?.nodes_of("doc", Some("published"))?.len();
        let beta = snaps.of(&PoolName::new(BETA))?.nodes_of("ticket", None)?.len();
        seen.push((alpha, beta));
        if seen.len() == 4 {
            // The window closes DURING the last lock-free attempt — this
            // attempt's snapshots were pinned before it, so it still sees the
            // mixed state and is still discarded. `release` waits for the
            // child to exit, so the flip is complete before attempt five.
            window
                .take()
                .expect("released once")
                .release_and_expect_exit_zero();
        }
        Ok((alpha, beta))
    })
    .expect("the read completes once the window closes");

    assert_eq!(seen.len(), 5, "four discarded attempts, then one that stands");
    assert_eq!(result.retries, 3);
    assert!(
        matches!(result.escalation, Escalation::RealReadLocks { .. }),
        "past the bound the reader escalates to real read locks: {:?}",
        result.escalation
    );
    for (i, cut) in seen.iter().take(4).enumerate() {
        assert_eq!(
            *cut,
            (DOCS, 0),
            "attempt {} straddled the window and saw the MIXED state",
            i + 1
        );
    }
    assert_eq!(
        seen[4],
        (DOCS, 1),
        "the attempt that stands is the consistent cut"
    );
    assert_eq!(
        result.value,
        (DOCS, 1),
        "and the mixed state, seen four times, is returned zero times"
    );

    // The escalated reader left nothing behind, and the window's transaction
    // is cleared with both parts landed.
    assert!(spine.pending().expect("pending").is_empty());
    assert!(c.in_landing(None).expect("in landing").is_empty());
}

// ---------------------------------------------------------------------------
// leak path 3 — a single-pool snapshot never sees a torn per-pool landing
// ---------------------------------------------------------------------------

/// One pool's share of a commit lands as ONE atomic SQLite
/// transaction, so a single-pool snapshot needs neither locks nor validation —
/// it can never see half of a part.
///
/// The deterministic witness, inside the held window: a single-pool read of
/// alpha returns ALL of alpha's `DOCS` deltas (never a subset) and a
/// single-pool read of beta returns NONE of beta's — answered immediately,
/// with no retry and no escalation, while the cross-pool read of the test
/// above cannot answer at all. The racing witness is the next test, which
/// needs no window and runs in both builds.
// RED (against the real pipeline): the `BetweenLandings` seam call
// removed from `exec` — the victim ran to completion and exited 4, and
// `pause_at` reported "the pause never opened"; mutation Y
// (per-part lock release) — `assert_window_open` found no granted lock.
// NEGATIVE RESULT, recorded: `Pool::apply_part` committing after each delta
// (mutation W) leaves THIS test green — a held door has finished alpha's
// part, whole or torn, before the parent looks — while the racing test
// below saw a torn count ("sample 122 saw 1 of 6") within milliseconds.
// That is exactly why the racing half exists.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn a_single_pool_snapshot_never_sees_a_torn_per_pool_landing() {
    let _env = kill::env_shared();
    let name = "torn-window";
    let dir = root(name);
    let c = fresh_cauldron(&dir, patient());
    seed_drafts(&c, DOCS);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let window = open_window(&dir, name);
    assert_window_open(&spine);

    let alpha = Pool::open(&Pool::path_in(&dir, &PoolName::new(ALPHA))).expect("alpha");
    let beta = Pool::open(&Pool::path_in(&dir, &PoolName::new(BETA))).expect("beta");
    {
        let snap = alpha.snapshot().expect("alpha snapshot");
        assert_eq!(
            snap.nodes_of("doc", Some("published")).expect("published").len(),
            DOCS,
            "alpha's part is whole in one snapshot — every delta or none"
        );
        assert_eq!(
            snap.nodes_of("doc", Some("draft")).expect("draft").len(),
            0,
            "and no doc is left on the near side of the same part"
        );
        let snap = beta.snapshot().expect("beta snapshot");
        assert_eq!(snap.nodes_of("ticket", None).expect("tickets").len(), 0);
    }
    // The same fact through the read door: a single-pool read answers at once
    // in a window where the cross-pool read cannot answer at all.
    let out = c
        .read(&ReadRequest {
            statement: r#"MATCH (d: fixture-alpha/doc { state: "published" }) RETURN d.id"#
                .to_string(),
            bindings: Bindings::new(),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect("a single-pool read needs neither locks nor validation");
    assert_eq!(out.rows.0.len(), DOCS);
    assert_eq!(out.retries, 0);
    assert!(!out.escalated, "it never entered a queue");
    window.release_and_expect_exit_zero();
    assert_eq!(tickets(&c), 1, "beta's half lands once released");
}

/// The racing witness for leak path 3: a sampler thread takes real snapshots
/// in a tight loop while a real door commit lands a `DOCS`-delta part over
/// and over. Every sample is 0 or `DOCS`; a torn landing is any other number.
/// The sampler must observe BOTH endpoints, or it never straddled a landing
/// and the green means nothing. Needs no held window, so it runs in both
/// build configurations.
///
/// THE SENTINELS ARE DETERMINISTIC. As
/// first written the writer ran its 24 landings flat out and the sampler
/// thread sampled whenever the scheduler let it, so "saw both endpoints" and
/// "sampled more than once per landing" were bets on the sampler getting the
/// CPU before the writer finished — the longest period on this path is the
/// scheduler's latency for the sampler thread, which load stretches without
/// bound. Under a 20-way CPU burn the old form failed 14/20 ("saw only
/// [0]", once "1 samples over 24 landings"). Now the writer WAITS PAST that
/// period on a real signal: before the first landing and after every
/// landing it waits until the sampler has completed a sample that BEGAN
/// after that point (`await_fresh_sample`). So every landed state, 0 and
/// `DOCS` alike, is sampled at least once by construction, and the sampler
/// still samples flat out WHILE each landing runs, which is the race the
/// torn check needs.
// RED: `Pool::apply_part` committing after each delta instead of
// once at the end (the part split into one transaction per delta) — the
// sampler saw intermediate counts within milliseconds.
// RED (seen): the landings' SET aimed at `title` instead of `state`
// (no landing moves the published count) — "the sampler never straddled a
// landing (it saw only [0])", under no load at all: the sentinel can fail.
#[test]
fn a_racing_sampler_never_sees_a_torn_per_pool_landing() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = root("torn-race");
    let c = fresh_cauldron(&dir, patient());
    seed_drafts(&c, DOCS);
    let alpha_path = Pool::path_in(&dir, &PoolName::new(ALPHA));

    let rounds = 24usize;
    let stop = Arc::new(Mutex::new(false));
    // Samples BEGUN and samples DONE, counted by the sampler.
    let begun = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    // Wait until the sampler has finished a sample that began after this
    // call: with `b` samples begun now, sample number `b` begins later, and
    // `done > b` says it has finished. Bounded, so a dead sampler is a named
    // failure and not a hang.
    let await_fresh_sample = |when: &str| {
        let b = begun.load(Ordering::SeqCst);
        let started = std::time::Instant::now();
        while done.load(Ordering::SeqCst) <= b {
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the sampler took no fresh sample {when} within 30 s"
            );
            std::thread::sleep(Duration::from_micros(200));
        }
    };
    let (samples, both_ends) = std::thread::scope(|scope| {
        let sampler_stop = stop.clone();
        let (begun, done) = (&begun, &done);
        let sampler = scope.spawn(move || {
            let pool = Pool::open(&alpha_path).expect("sampler pool");
            let mut counts: Vec<usize> = Vec::new();
            loop {
                begun.fetch_add(1, Ordering::SeqCst);
                let snap = pool.snapshot().expect("snapshot");
                counts.push(
                    snap.nodes_of("doc", Some("published"))
                        .expect("published")
                        .len(),
                );
                drop(snap);
                done.fetch_add(1, Ordering::SeqCst);
                if *sampler_stop.lock().unwrap() {
                    break;
                }
            }
            counts
        });

        await_fresh_sample("before the first landing");
        for round in 0..rounds {
            let (from, to) = if round % 2 == 0 {
                ("draft", "published")
            } else {
                ("published", "draft")
            };
            c.submit(commit(
                &format!(
                    r#"MATCH (d: fixture-alpha/doc {{ state: "{from}" }}) SET d.state = "{to}""#
                ),
                &[],
            ))
            .unwrap_or_else(|e| panic!("round {round} must commit: {e:?}"));
            await_fresh_sample(&format!("after landing {round}"));
        }
        *stop.lock().unwrap() = true;
        let counts = sampler.join().expect("join");
        let both = counts.contains(&0) && counts.contains(&DOCS);
        (counts, both)
    });

    assert!(
        samples.len() > rounds,
        "the sampler must actually sample: {} samples over {rounds} landings",
        samples.len()
    );
    assert!(
        both_ends,
        "the sampler never straddled a landing (it saw only {:?}), so a green here would prove \
         nothing",
        distinct(&samples)
    );
    for (i, n) in samples.iter().enumerate() {
        assert!(
            *n == 0 || *n == DOCS,
            "sample {i} saw {n} of {DOCS} published: a per-pool landing was TORN"
        );
    }
}

// ---------------------------------------------------------------------------
// the same three properties, against a window a DEAD door left behind
// ---------------------------------------------------------------------------

/// The KILL form of the seam, used for what this suite
/// asks of it here: not "can recovery roll it forward" (tested elsewhere)
/// but **is the window the real door leaves actually invisible three
/// ways** when the door is gone.
///
/// The victim's `windowed` job runs the same statement under
/// `SPINE_FAULT=between-landings` and is killed by the seam after alpha's
/// part has landed and before beta's. The parent then re-runs all three
/// leak-path assertions against that window — including the lock one,
/// because the child died holding its locks and they are still in the table,
/// which is exactly what "released at the cleared flip only" means when the
/// flip never came.
///
/// With the tests above on the pause form, this is the test that shows the
/// HELD window and the CRASHED window are the same window: same statement,
/// same stage, same door, same three properties.
// RED: the `BetweenLandings` seam call removed from `exec` — the
// child exited 4 (the armed stage was never reached) and `kill_at` failed on
// "a kill is a DEATH, not an exit status"; mutation Y (per-part
// release) — the dead child's locks were gone and the leak-path-1 writer
// committed ("the killed commit's locks must still hold, got Ok(Committed
// ...)"); `window_is_consistent`'s `in_landing` half deleted — the
// cross-pool read returned a cut across the dead door's window.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn the_door_s_own_half_landed_window_is_invisible_the_same_three_ways() {
    let _env = kill::env_shared();
    let dir = root("kill-window");
    let c = fresh_cauldron(&dir, patient());
    seed_drafts(&c, DOCS);
    drop(c);
    kill::kill_at("windowed", &dir, "between-landings");

    let c = Cauldron::open_unrecovered(&dir, patient()).expect("reopen after the kill");
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let stuck = spine
        .pending()
        .expect("pending")
        .into_iter()
        .find(|r| r.decided)
        .expect("a decided, un-cleared row survives the kill");
    assert_eq!(stuck.parts.len(), 2);
    assert!(stuck.parts[0].landed && !stuck.parts[1].landed);
    assert!(!stuck.cleared);

    // LEAK PATH 3 first (it needs no waiting): alpha's part is whole, beta's
    // is absent, and the single-pool read answers at once.
    assert_eq!(published(&c), DOCS, "alpha's part is whole");
    assert_eq!(tickets(&c), 0, "beta's is absent");

    // LEAK PATH 1: the dead child's locks are STILL HELD, because the cleared
    // flip never happened. A door writer on one of those bodies waits, and
    // says loudly whose lock it is behind.
    let doc = spine
        .row_at(stuck.seq)
        .expect("row")
        .payload
        .parts
        .iter()
        .find(|p| p.pool == PoolName::new(ALPHA))
        .and_then(|p| p.deltas.first().cloned())
        .map(|d| match d {
            spine::delta::Delta::SetField { node, .. } => node,
            other => panic!("alpha's first delta should be a SET, got {other:?}"),
        })
        .expect("alpha delta");
    let impatient = Cauldron::open_unrecovered(&dir, cfg(120)).expect("second handle");
    match impatient.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t"#,
        &[
            ("id", serde_json::json!(doc.0)),
            ("t", serde_json::json!("never lands")),
        ],
    )) {
        Err(SpineError::Busy { message }) => assert!(
            message.contains(&format!("seq {}", stuck.seq)),
            "names the dead holder: {message}"
        ),
        other => panic!("the killed commit's locks must still hold, got {other:?}"),
    }

    // LEAK PATH 2: the cross-pool validated read cannot certify a cut.
    match impatient.read(&ReadRequest {
        statement: CROSS_POOL_READ.to_string(),
        bindings: Bindings::new(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    }) {
        Err(SpineError::Busy { .. }) => {}
        Ok(out) => panic!("the read returned a cut across the door's own window: {out:?}"),
        other => panic!("expected a bounded, loud failure, got {other:?}"),
    }

    // And the window closes the way recovery closes it: roll forward.
    let report = c.recover().expect("recovery");
    assert_eq!(report.rolled_forward, vec![stuck.hash]);
    assert_eq!(tickets(&c), 1, "everything landed, and the row told which");
    let out = c
        .read(&ReadRequest {
            statement: CROSS_POOL_READ.to_string(),
            bindings: Bindings::new(),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect("and the cross-pool read answers once the window is closed");
    assert_eq!(out.rows.0.len(), DOCS);
}

// ---------------------------------------------------------------------------
// the window — the real door, held at `between-landings`
// ---------------------------------------------------------------------------

/// Open the window: spawn the victim's `windowed` job on the parent's
/// cauldron and hold it at `between-landings`. Returns once the child has
/// REACHED the stage (the library's own marker), never on a timer.
#[cfg(all(unix, feature = "fault-injection"))]
fn open_window(dir: &Path, name: &str) -> kill::Paused {
    kill::pause_at("between-landings", "windowed", dir, name, &[])
}

/// What the held door reports, read from disk.
#[cfg(all(unix, feature = "fault-injection"))]
struct WindowFacts {
    seq: Seq,
}

/// Assert the window is genuinely open ON DISK, and hand back its facts.
/// Everything here is read from the spine row and the lock table, never from
/// the harness's own memory — and the harness has none: the window is the
/// child's.
#[cfg(all(unix, feature = "fault-injection"))]
fn assert_window_open(spine: &SpineDb) -> WindowFacts {
    let mut pending = spine.pending().expect("pending");
    assert_eq!(
        pending.len(),
        1,
        "exactly one held transaction: {:?}",
        pending
            .iter()
            .map(|r| (r.seq, r.decided, r.cleared))
            .collect::<Vec<_>>()
    );
    let row: TxRow = pending.remove(0);
    assert!(row.decided, "the commit point has passed: the row is decided");
    assert!(!row.cleared, "and it has not cleared");
    assert_eq!(row.parts.len(), 2, "two pools, two parts");
    assert_eq!(row.parts[0].pool, PoolName::new(ALPHA));
    assert_eq!(row.parts[1].pool, PoolName::new(BETA));
    assert!(row.parts[0].landed, "alpha's part landed");
    assert!(!row.parts[1].landed, "beta's part has not");
    assert_eq!(
        row.payload.parts[0].deltas.len(),
        DOCS,
        "alpha's part is multi-delta, so a torn landing is exhibitable"
    );
    assert!(
        row.payload.parts[1].deltas.len() >= 2,
        "beta's part mints a ticket and at least one cross-pool edge"
    );
    // The transaction is in landing state over BOTH pools — this is what
    // the validated read checks, and it is why no cross-pool cut can be certified.
    let landing = spine.in_landing(None).expect("in landing");
    let ours = landing
        .iter()
        .find(|l| l.seq == row.seq)
        .expect("the window is in landing state");
    assert_eq!(
        ours.pools,
        vec![PoolName::new(ALPHA), PoolName::new(BETA)],
        "and it is in landing over both pools"
    );
    // Its locks are granted and held.
    let queues: Vec<_> = row.payload.parts[0]
        .deltas
        .iter()
        .filter_map(|d| match d {
            spine::delta::Delta::SetField { node, .. } => Some(node.clone()),
            _ => None,
        })
        .collect();
    assert!(!queues.is_empty());
    for node in &queues {
        let q = locks::queue(spine, &PoolName::new(ALPHA), &body(node)).expect("queue");
        assert!(
            q.iter().any(|e| e.seq == row.seq && e.granted),
            "the window holds a granted lock on {node:?}: {q:?}"
        );
    }
    WindowFacts { seq: row.seq }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-window-{name}-{}", std::process::id()))
}

fn patient() -> SpineConfig {
    SpineConfig {
        lock_wait_ceiling: Duration::from_secs(30),
        lock_poll_interval: Duration::from_millis(3),
        ..SpineConfig::default()
    }
}

#[cfg(all(unix, feature = "fault-injection"))]
fn cfg(ceiling_ms: u64) -> SpineConfig {
    SpineConfig {
        retry_bound: 2,
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

#[cfg(all(unix, feature = "fault-injection"))]
fn grammar_lookup() -> impl Fn(&PoolName) -> Option<PoolGrammar> {
    move |p: &PoolName| match p.as_str() {
        ALPHA => Some(fixtures::fixture_alpha()),
        BETA => Some(fixtures::fixture_beta()),
        _ => None,
    }
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
                thread: "window-invisible".into(),
                dispatch: "window-invisible".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

#[cfg(all(unix, feature = "fault-injection"))]
fn body(node: &NodeId) -> Resource {
    Resource::Body { node: node.clone() }
}

fn seed_drafts(c: &Cauldron, n: usize) -> Vec<NodeId> {
    (0..n)
        .map(|i| {
            let out = c
                .submit(commit(
                    r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" }) RETURN d.id"#,
                    &[("t", serde_json::json!(format!("draft {i}")))],
                ))
                .expect("seed a draft");
            match out {
                Outcome::Committed { returned, .. } => {
                    NodeId(returned.0[0]["d.id"].as_str().expect("id").to_string())
                }
                other => panic!("expected Committed, got {other:?}"),
            }
        })
        .collect()
}

#[cfg(all(unix, feature = "fault-injection"))]
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

#[cfg(all(unix, feature = "fault-injection"))]
fn published(c: &Cauldron) -> usize {
    read_rows(
        c,
        r#"MATCH (d: fixture-alpha/doc { state: "published" }) RETURN d.id"#,
    )
    .len()
}

#[cfg(all(unix, feature = "fault-injection"))]
fn tickets(c: &Cauldron) -> usize {
    read_rows(c, r#"MATCH (t: fixture-beta/ticket) RETURN t.id"#).len()
}

#[cfg(all(unix, feature = "fault-injection"))]
fn title_of(c: &Cauldron, id: &str) -> Option<String> {
    c.read(&ReadRequest {
        statement: r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.title"#.to_string(),
        bindings: {
            let mut b = Bindings::new();
            b.insert("id".into(), serde_json::json!(id));
            b
        },
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    })
    .expect("read")
    .rows
    .0
    .first()
    .and_then(|r| r["d.title"].as_str().map(str::to_string))
}

#[cfg(all(unix, feature = "fault-injection"))]
fn grant_state(spine: &SpineDb, node: &NodeId, seq: Seq) -> Option<bool> {
    locks::queue(spine, &PoolName::new(ALPHA), &body(node))
        .expect("queue")
        .into_iter()
        .find(|e| e.seq == seq)
        .map(|e| e.granted)
}

/// Wait (bounded) until some claimant OTHER than `holder` is enrolled and
/// ungranted on `node`, and hand back its sequence number. No sleep races:
/// this is the state that says "a door call asked for this lock and did not
/// get it".
#[cfg(all(unix, feature = "fault-injection"))]
fn await_foreign_queued(spine: &SpineDb, node: &NodeId, holder: Seq) -> Seq {
    let started = Instant::now();
    loop {
        let q = locks::queue(spine, &PoolName::new(ALPHA), &body(node)).expect("queue");
        if let Some(e) = q.iter().find(|e| e.seq != holder && !e.granted) {
            return e.seq;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "no door call ever queued on {node:?}: {q:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn distinct(samples: &[usize]) -> Vec<usize> {
    let mut v: Vec<usize> = samples.to_vec();
    v.sort_unstable();
    v.dedup();
    v
}
