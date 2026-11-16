//! Smoke — the crate's standing conformance checks.
//!
//! # The stub era ended here
//!
//! This file carried two stub assertions (`Cauldron::open` and
//! `Cauldron::create` are deferred bodies that fail loudly by name). Both were
//! filled, so those two tests began failing FOR THE RIGHT REASON, and
//! the stub rule is explicit that a filled body must never leave a
//! stale stub assertion green. They are REMOVED rather than re-pointed:
//! there is no `SpineError::NotBuilt` left anywhere in the crate to point
//! them at, and a conformance check that has to hunt for a stub is a check
//! that will go stale again. The rule is now asserted the other way
//! round, by `no_deferred_body_survives_this_build` below — a check that
//! cannot rot, because it names the failure mode rather than a victim.
//!
//! What remains and grows: the "absent or refusing" proof of the fault
//! seam, which is a STANDING obligation on every release and
//! never retired.

use spine::config::SpineConfig;
use spine::error::SpineError;
use spine::recover::Stage;
use spine::schema_def::fixtures;
use spine::{Cauldron, Outcome};

test_scratch::install!();

/// The stub rule, asserted as the crate-wide property it actually is:
/// no live path in this build returns a deferred body. Every entry point on
/// the public surface is exercised against a real cauldron, and any
/// `NotBuilt` anywhere in the chain fails this test by name.
///
/// RED once by: putting `not_built!("lib::Cauldron::open")` back at the top
/// of `Cauldron::open` — the assertion below names it exactly.
// RED (the strengthening): `assert_no_stub` tolerates EVERY error except
// `NotBuilt`, so the last line's claim — "the write door, all the way through
// a real commit" — was asserted by nothing at all: a door that refused every
// commit outright passed it. The submit now demands `Outcome::Committed`, and
// reds under any mutation that makes the happy path refuse rather than commit.
#[test]
fn no_deferred_body_survives_this_build() {
    // `SPINE_FAULT` is process-global and the harness runs tests in threads.
    // Without this guard the sibling test below arms the seam WHILE this one
    // is mid-`submit`, and in the fault-injection build the seam's answer to
    // an armed stage is `std::process::abort()` — which does not fail this
    // test, it kills the whole test binary. See `tests/util/kill.rs`, whose
    // ENV lock is this same idiom; the lock has to be per-BINARY, and each
    // integration test file is its own binary, so this file needs its own.
    let _env = env_shared();
    let dir = scratch("smoke-no-stubs");
    let grammars = [fixtures::fixture_alpha(), fixtures::fixture_beta()];

    let cauldron = Cauldron::create(&dir, &grammars, SpineConfig::default()).expect("create");
    assert_no_stub(cauldron.committed_commit_counter().map(|_| ()));
    assert_no_stub(cauldron.in_landing(None).map(|_| ()));
    assert_no_stub(cauldron.halted().map(|_| ()));
    assert_no_stub(cauldron.recover().map(|_| ()));
    match cauldron.quiesce() {
        Ok(token) => assert_no_stub(cauldron.resume(&token).map(|_| ())),
        Err(e) => assert_no_stub(Err(e.error)),
    }
    drop(cauldron);

    let reopened = Cauldron::open(&dir, SpineConfig::default()).expect("open");
    assert_no_stub(reopened.read(&spine::ReadRequest {
        statement: r#"MATCH (d: fixture-alpha/doc { state: "draft" }) RETURN d.id"#.into(),
        bindings: Default::default(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    }).map(|_| ()));
    // The write door, ALL THE WAY THROUGH A REAL COMMIT — and that is the
    // claim, so it is asserted as success and not merely as not-a-stub.
    match reopened.submit(envelope()) {
        Ok(Outcome::Committed { deduplicated, .. }) => {
            assert!(!deduplicated, "a first submission is not a duplicate");
        }
        Ok(other) => panic!("a Commit envelope comes back Committed, got {other:?}"),
        Err(SpineError::NotBuilt(name)) => {
            panic!("a deferred body survives on a live path: `{name}`")
        }
        Err(other) => panic!(
            "the happy path must COMMIT: a smoke test that tolerates every error but NotBuilt \
             proves nothing about the write door. Got {other:?}"
        ),
    }
    assert_eq!(
        reopened.committed_commit_counter().expect("counter"),
        1,
        "and the committed-commit counter moved for it"
    );
}

fn assert_no_stub(r: Result<(), SpineError>) {
    if let Err(SpineError::NotBuilt(name)) = r {
        panic!("a deferred body survives on a live path: `{name}`");
    }
}

/// The fault seam is "absent or refusing" in BOTH builds — a STANDING obligation on every
/// release, which is why this test outlives the stub assertions above. The
/// seam has TWO forms and TWO variables, and the obligation
/// binds both: `SPINE_FAULT` (the kill) and `SPINE_PAUSE` (the pause).
///
/// RED once by: deleting the `#[cfg(not(feature = "fault-injection"))]` arm
/// of `fault::at_stage_with` and returning `Ok(())` — the production build
/// then ignores a set `SPINE_FAULT` silently, which is the exact failure
/// this accommodation exists to make impossible.
// RED: the production `pause_at_stage_with` body replaced with
// `Ok(())` — the matching-stage call returned Ok and "a production build must
// refuse a set SPINE_PAUSE" failed; the production `pause_gate_with` body
// replaced with `Ok(())` — the gate half failed. In the armed build: the
// gate's `pause_latch()?` demand removed — "no latch path: the gate refuses"
// failed; `both_set` returning `Ok(())` — the both-set assertions failed.
// RED (the strengthening): this test MUTATES the process-global
// `SPINE_FAULT` while `no_deferred_body_survives_this_build` runs a real
// commit on another thread. In the fault-injection build that is not a flaky
// assertion, it is `std::process::abort()` inside the sibling's commit path
// and a whole test binary dying on a signal. It takes the WRITE guard; every
// other test in this file takes the read guard. Same idiom as
// `tests/util/kill.rs`'s ENV lock and `atomicity_kill.rs`'s use of it.
#[test]
fn fault_seam_absent_or_refusing() {
    let _env = env_exclusive();
    // Declared AFTER the lock so it drops BEFORE it: a failing assertion
    // below must not leave a seam variable set for the sibling test that
    // runs a real commit next. (Found by a red sweep: a mutation
    // that redded this test also redded `no_deferred_body_survives_this_build`
    // with a refusal it did not deserve.)
    let _clean = SeamEnvClean;
    // Untouched environment: the seam is silent in both builds.
    std::env::remove_var("SPINE_FAULT");
    assert!(spine::fault::at_stage(Stage::RowAppended).is_ok());
    assert!(spine::fault::gate().is_ok());

    std::env::set_var("SPINE_FAULT", "decided");

    #[cfg(not(feature = "fault-injection"))]
    {
        // Without the feature: ANY set SPINE_FAULT refuses loudly by name.
        let err = spine::fault::at_stage(Stage::RowAppended)
            .expect_err("a production build must refuse a set SPINE_FAULT");
        match err {
            SpineError::FaultSeamRefused { variable, value } => {
                assert_eq!(variable, spine::fault::FAULT_ENV, "it names WHICH seam variable");
                assert_eq!(value, "decided");
            }
            other => panic!("expected FaultSeamRefused, got: {other:?}"),
        }
        // And the DOOR refuses too, before anything is parsed or appended —
        // the seam's absence is a property of the door, not only of a stage
        // a statement might never reach.
        let err = spine::fault::gate().expect_err("the door gate must refuse too");
        match err {
            SpineError::FaultSeamRefused { variable, value } => {
                assert_eq!(variable, spine::fault::FAULT_ENV);
                assert_eq!(value, "decided");
            }
            other => panic!("expected FaultSeamRefused, got: {other:?}"),
        }
    }

    #[cfg(feature = "fault-injection")]
    {
        // With the feature: a NON-matching stage passes through armed;
        // (the matching stage would abort the process, which is the point,
        // and is exercised by the kill tests, not here).
        assert!(spine::fault::at_stage(Stage::RowAppended).is_ok());
        assert!(spine::fault::gate().is_ok());
        // A value naming NO stage is loud in the armed build too: a
        // misspelled stage that silently armed nothing would make every kill
        // test a fake green. Asserted BY TAG: a bare `is_err()`
        // would pass on any error at all.
        std::env::set_var("SPINE_FAULT", "not-a-stage");
        assert_seam_usage(
            spine::fault::gate(),
            spine::fault::USAGE_NO_SUCH_STAGE,
            "a misspelled SPINE_FAULT is loud at the gate",
        );
        let at_stage = spine::fault::at_stage(Stage::RowAppended);
        assert_seam_usage(
            at_stage,
            spine::fault::USAGE_NO_SUCH_STAGE,
            "a misspelled SPINE_FAULT is loud at every stage call",
        );
        // And the refusal NAMES every stage recovery knows, read off
        // `Stage::ALL` — the one list (the message used to carry a second,
        // hand-typed list).
        let message = spine::fault::at_stage(Stage::RowAppended)
            .expect_err("still misspelled")
            .message();
        for stage in Stage::ALL {
            assert!(
                message.contains(stage.name()),
                "the no-such-stage refusal names `{}`: {message}",
                stage.name()
            );
        }
    }

    std::env::remove_var("SPINE_FAULT");

    // ---- THE PAUSE FORM: the same obligation, the second
    //      variable. Same two conditions, same proof shape. ----
    std::env::set_var("SPINE_PAUSE", "decided");
    // A tiny ceiling, so that if a pause body ever SURVIVED into the
    // production build the matching call below would abort inside a tenth
    // of a second (visibly, as a signal) instead of hanging this binary for
    // the default thirty seconds.
    std::env::set_var("SPINE_PAUSE_CEILING_MS", "100");
    std::env::remove_var("SPINE_PAUSE_LATCH");

    #[cfg(not(feature = "fault-injection"))]
    {
        // Without the feature: ANY set SPINE_PAUSE refuses loudly by name,
        // at the MATCHING stage too — the pause body is compiled out, so the
        // call returns instead of holding. ONE variant for both forms; the `variable` field is what says it was the PAUSE form.
        let err = spine::fault::at_stage(Stage::Decided)
            .expect_err("a production build must refuse a set SPINE_PAUSE");
        match err {
            SpineError::FaultSeamRefused { variable, value } => {
                assert_eq!(variable, spine::fault::PAUSE_ENV, "it names the PAUSE variable");
                assert_eq!(value, "decided");
            }
            other => panic!("expected FaultSeamRefused naming SPINE_PAUSE, got: {other:?}"),
        }
        let err = spine::fault::gate().expect_err("the door gate must refuse a set SPINE_PAUSE");
        match err {
            SpineError::FaultSeamRefused { variable, value } => {
                assert_eq!(variable, spine::fault::PAUSE_ENV);
                assert_eq!(value, "decided");
            }
            other => panic!("expected FaultSeamRefused naming SPINE_PAUSE, got: {other:?}"),
        }
    }

    #[cfg(feature = "fault-injection")]
    {
        // With the feature: a NON-matching stage passes through armed (the
        // matching stage would HOLD, which is exercised in-process with a
        // latch by `atomicity_kill.rs`, not here).
        assert!(spine::fault::at_stage(Stage::RowAppended).is_ok());
        // The gate demands the latch path: a pause with nowhere to be
        // released from is refused at the door, not thirty seconds later.
        assert_seam_usage(
            spine::fault::gate(),
            spine::fault::USAGE_NO_LATCH,
            "no latch path: the gate refuses",
        );
        std::env::set_var("SPINE_PAUSE_LATCH", "/nonexistent/spine-smoke-latch");
        assert!(spine::fault::gate().is_ok());
        // A value naming NO stage is loud in the armed build too.
        std::env::set_var("SPINE_PAUSE", "not-a-stage");
        assert_seam_usage(
            spine::fault::gate(),
            spine::fault::USAGE_NO_SUCH_STAGE,
            "a misspelled SPINE_PAUSE is loud at the gate",
        );
        assert_seam_usage(
            spine::fault::at_stage(Stage::Decided),
            spine::fault::USAGE_NO_SUCH_STAGE,
            "a misspelled SPINE_PAUSE is loud at every stage call",
        );
        // Both variables set: ONE seam, ONE aim — refused, never armed twice.
        std::env::set_var("SPINE_PAUSE", "decided");
        std::env::set_var("SPINE_FAULT", "before-cleared");
        assert_seam_usage(
            spine::fault::gate(),
            spine::fault::USAGE_BOTH_SET,
            "both set: the gate refuses",
        );
        assert_seam_usage(
            spine::fault::at_stage(Stage::RowAppended),
            spine::fault::USAGE_BOTH_SET,
            "both set: every stage call refuses, matching or not",
        );
        std::env::remove_var("SPINE_FAULT");
        std::env::remove_var("SPINE_PAUSE_LATCH");
    }

    std::env::remove_var("SPINE_PAUSE");
    std::env::remove_var("SPINE_PAUSE_CEILING_MS");
    // Both unset again: silent in both builds.
    assert!(spine::fault::at_stage(Stage::Decided).is_ok());
    assert!(spine::fault::gate().is_ok());
}

/// The stage vocabulary the seam borrows is RECOVERY'S OWN and has exactly
/// the four stages recovery names — two descriptions of the commit path
/// would drift exactly where atomicity is being proven.
///
/// RED once by: adding a fifth stage to `recover::Stage` without a seam call
/// for it, or renaming one of the four frozen text names.
#[test]
fn the_seam_speaks_recoverys_four_stages_and_no_others() {
    let named = [
        (Stage::RowAppended, "row-appended"),
        (Stage::Decided, "decided"),
        (Stage::BetweenLandings, "between-landings"),
        (Stage::BeforeCleared, "before-cleared"),
    ];
    for (stage, name) in named {
        assert_eq!(stage.name(), name);
        assert_eq!(Stage::parse(name), Some(stage));
    }
    // `Stage::ALL` IS the list, in pipeline order — the seam's usage message
    // and nothing else derives from it, so it is pinned here to the four
    // names above exactly once.
    assert_eq!(Stage::ALL.map(|s| s.name()), named.map(|(_, n)| n));
    // A misspelled stage is never silently "no stage".
    assert_eq!(Stage::parse("row_appended"), None);
    assert_eq!(Stage::parse(""), None);
}

// ---- helpers ----

/// `SPINE_FAULT` is process-global and the harness runs tests in threads, so
/// the one test that touches the environment takes [`env_exclusive`] and every
/// test that runs a door path takes [`env_shared`]. The lock is per-BINARY by
/// necessity — each integration test file is its own process, so this file
/// carries its own static exactly as `recovery.rs` and `door_smoke.rs` do,
/// rather than borrowing `util/kill.rs`'s (which, included here, would be a
/// second static in a second process and would interlock nothing).
static ENV: std::sync::RwLock<()> = std::sync::RwLock::new(());

fn env_shared() -> std::sync::RwLockReadGuard<'static, ()> {
    ENV.read().unwrap_or_else(|e| e.into_inner())
}

fn env_exclusive() -> std::sync::RwLockWriteGuard<'static, ()> {
    ENV.write().unwrap_or_else(|e| e.into_inner())
}

/// A seam usage refusal, asserted BY ITS FROZEN TAG: the
/// result must be an `Err` whose `fault::seam_usage_tag` is exactly `tag`.
/// An `Ok`, any other error, and any OTHER seam usage refusal all fail here
/// — which is what a bare `is_err()` could not do. Armed build only: the
/// production build mints no seam usage refusal (it refuses by variant
/// before any usage question arises), so nothing there calls this.
#[cfg(feature = "fault-injection")]
fn assert_seam_usage<T: std::fmt::Debug>(r: spine::error::Result<T>, tag: &str, why: &str) {
    assert_eq!(
        spine::fault::seam_usage_tag(&r),
        Some(tag),
        "{why}: expected the seam usage refusal `{tag}`, got {r:?}"
    );
}

/// Removes every seam variable on drop, so an env-mutating test that panics
/// mid-way cannot poison the tests that run after it.
struct SeamEnvClean;

impl Drop for SeamEnvClean {
    fn drop(&mut self) {
        for v in [
            spine::fault::FAULT_ENV,
            spine::fault::PAUSE_ENV,
            spine::fault::PAUSE_LATCH_ENV,
            spine::fault::PAUSE_CEILING_ENV,
        ] {
            std::env::remove_var(v);
        }
    }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = test_scratch::tmp_path(format!("spine-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn envelope() -> spine::envelope::Envelope {
    spine::envelope::Envelope {
        statement: r#"CREATE (d: fixture-alpha/doc { title: "smoke", state: "draft" }) RETURN d.id"#
            .into(),
        bindings: Default::default(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: spine::envelope::Provenance {
            initiator: spine::envelope::Initiator::Agent {
                thread: "smoke".into(),
                dispatch: "smoke".into(),
            },
            instrument: spine::envelope::Instrument::door(),
        },
        mode: spine::envelope::Mode::Commit,
        idempotency_nonce: None,
    }
}
