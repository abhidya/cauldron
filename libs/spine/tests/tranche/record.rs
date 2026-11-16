//! The tranche's RECORD: `begin` (anchor row + `tranches` row),
//! the queue by anchor `seq`, the quiet start's bound, the heartbeat and its
//! enforcement, rollback into the store, the ordinary recovery's skip, and
//! the spine schema fence.
//!
//! Every act takes an explicit `now_ms`, so the two bounds (the heartbeat
//! timeout, the quiet wait) are crossed by the clock the test passes, never
//! by sleeping. Each guard names its forcing in a `// RED:` comment.

use spine::config::SpineConfig;
use spine::delta::CompiledPayload;
use spine::envelope::{
    Bindings, Envelope, Initiator, Instrument, Mode, Provenance, RippleKind,
    ENVELOPE_FORMAT_VERSION,
};
use spine::error::SpineError;
use spine::schema_def::fixtures;
use spine::spine_db::{SpineDb, SPINE_FILE, SPINE_SCHEMA_VERSION};
use spine::tranche::{
    QuietLevels, TrancheActor, TrancheBegin, TrancheLockMode, TrancheRef, TrancheState,
};
use spine::witness::Witness;
use spine::Cauldron;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const QUIET: QuietLevels = QuietLevels::BusNotConfigured;
const SERVICE: &str = "runner";

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-tranche-{name}-{}", std::process::id()))
}

fn fresh(dir: &Path) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(dir, &[fixtures::fixture_alpha()], SpineConfig::default())
        .expect("create the cauldron")
}

/// A key: 32 bytes as 64 lowercase hex, distinct per `n`.
fn key(n: u8) -> String {
    format!("{:02x}", n).repeat(32)
}

fn instrument() -> Instrument {
    Instrument {
        name: "runner".into(),
        version: "test-build".into(),
    }
}

fn begin_req(t: &str, k: &str, mode: TrancheLockMode) -> TrancheBegin {
    TrancheBegin {
        tranche: t.into(),
        key: k.into(),
        holder_service: SERVICE.into(),
        instrument: instrument(),
        lock_mode: mode,
        regions: vec!["fixture-alpha".into()],
    }
}

fn begin(c: &Cauldron, t: &str, k: &str, now: i64) -> spine::tranche::TrancheBegun {
    c.tranche_begin(&begin_req(t, k, TrancheLockMode::Touched), &QUIET, now)
        .unwrap_or_else(|e| panic!("begin {t}: {e:?}"))
}

fn actor(t: &str, k: &str) -> TrancheActor {
    TrancheActor {
        tranche: t.into(),
        key: k.into(),
        holder_service: SERVICE.into(),
    }
}

fn only_witness(e: SpineError) -> Witness {
    match e {
        SpineError::Refused { mut witnesses } if witnesses.len() == 1 => witnesses.remove(0),
        other => panic!("expected one refusal witness, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// a beat after the scheduler abandoned the tranche
// ---------------------------------------------------------------------------

/// RED (measured): the heartbeat's conditional update with its
/// `state IN (…)` clause removed — the deadline still enforced, the key left
/// live — accepts the beat after the abandon.
#[test]
fn t19_4_a_beat_after_the_abandon_is_refused_by_name_and_one_before_moves_the_deadline() {
    let dir = root("t19-4");
    let c = fresh(&dir);
    let timeout = c.config().tranche.timeout.as_millis() as i64;
    let k = key(1);
    let b = begin(&c, "T4", &k, 1_000);
    assert_eq!(b.state, TrancheState::Open);

    // OPPOSED: a beat BEFORE the deadline is accepted and moves it.
    let r = c.tranche_heartbeat(&actor("T4", &k), &QUIET, 20_000).expect("a live beat");
    assert_eq!(r.state, TrancheState::Open);
    assert_eq!(r.last_heartbeat, 20_000);
    assert_eq!(r.deadline, 20_000 + timeout, "the beat moves the deadline");
    // The old deadline (1_000 + timeout) has passed; the moved one has not.
    let tick = c.tranche_tick(&QUIET, 1_000 + timeout + 1).expect("tick");
    assert!(tick.abandoned.is_empty(), "a beaten tranche is not abandoned: {tick:?}");

    // The deadline lapses; the tick abandons.
    let lapse = 20_000 + timeout + 1;
    let tick = c.tranche_tick(&QUIET, lapse).expect("tick");
    assert_eq!(tick.abandoned, vec![("T4".to_string(), 1)]);

    // The beat after it: refused BY NAME, with the tranche's fields named.
    let e = c
        .tranche_heartbeat(&actor("T4", &k), &QUIET, lapse + 5)
        .expect_err("a beat after the abandon is refused");
    match only_witness(e) {
        Witness::TrancheTerminal {
            tranche,
            attempt,
            state,
            reason,
            last_heartbeat,
            deadline,
            ended_at,
            ..
        } => {
            assert_eq!((tranche.as_str(), attempt), ("T4", 1));
            assert_eq!(state, "abandoned");
            assert_eq!(reason.as_deref(), Some("heartbeat_lost"));
            assert_eq!(last_heartbeat, 20_000);
            assert_eq!(deadline, 20_000 + timeout);
            assert_eq!(ended_at, Some(lapse));
        }
        other => panic!("expected tranche_terminal, got {other:?}"),
    }
    // The abandon ran the release: the anchor is aborted, not left live.
    let anchor = c.tranche_anchor("T4", 1).unwrap().expect("anchor");
    assert!(!anchor.decided && anchor.cleared, "the anchor is aborted at the abandon");
}

// ---------------------------------------------------------------------------
// a reused key
// ---------------------------------------------------------------------------

/// RED (measured): `tranche_begin`'s key-reuse check removed — the
/// reused key opens attempt 2.
#[test]
fn t19_26_a_reused_key_after_abandonment_is_refused_and_a_new_key_opens_attempt_2() {
    let dir = root("t19-26");
    let c = fresh(&dir);
    let timeout = c.config().tranche.timeout.as_millis() as i64;
    let k1 = key(0x11);
    begin(&c, "T26", &k1, 1_000);
    let tick = c.tranche_tick(&QUIET, 1_000 + timeout + 1).unwrap();
    assert_eq!(tick.abandoned, vec![("T26".to_string(), 1)]);

    let e = c
        .tranche_begin(&begin_req("T26", &k1, TrancheLockMode::Touched), &QUIET, 50_000)
        .expect_err("attempt 1's key is never reused");
    match only_witness(e) {
        Witness::TrancheTerminal { attempt, state, reason, .. } => {
            assert_eq!(attempt, 1);
            assert_eq!(state, "abandoned");
            assert_eq!(reason.as_deref(), Some("heartbeat_lost"));
        }
        other => panic!("expected tranche_terminal {{attempt: 1}}, got {other:?}"),
    }
    // No attempt 2 exists.
    assert_eq!(c.tranche_store("T26", None).unwrap().attempt, 1);
    assert!(c.tranche_store("T26", Some(2)).is_err());

    // OPPOSED: a NEW key after the abandonment opens attempt 2.
    let k2 = key(0x22);
    let b = begin(&c, "T26", &k2, 50_001);
    assert_eq!((b.attempt, b.state), (2, TrancheState::Open));
    assert_eq!(c.tranche_store("T26", None).unwrap().attempt, 2);
}

// ---------------------------------------------------------------------------
// the schema fence
// ---------------------------------------------------------------------------

/// RED (measured): `SpineDb::fence_and_upgrade`'s `found >
/// binary_version` refusal removed — the older-binary open succeeds and its
/// ordinary pass (the skip also absent in that simulation) aborts the anchor.
#[test]
fn t19_35_an_older_binary_is_refused_by_name_while_a_tranche_is_live() {
    for (label, open_it) in [("queued", false), ("open", true)] {
        let dir = root(&format!("t19-35-{label}"));
        let c = fresh(&dir);
        if open_it {
            let b = begin(&c, "T35", &key(0x35), 1_000);
            assert_eq!(b.state, TrancheState::Open);
        } else {
            // A tranche ahead keeps T35 queued.
            begin(&c, "AHEAD", &key(0x01), 1_000);
            let b = begin(&c, "T35", &key(0x35), 1_001);
            assert_eq!(b.state, TrancheState::Queued);
        }
        let anchor = c.tranche_anchor("T35", 1).unwrap().expect("anchor");
        drop(c);

        let older = SPINE_SCHEMA_VERSION - 1;
        match Cauldron::open_as_older_binary(&dir, SpineConfig::default(), older) {
            Err(SpineError::SpineSchemaNewer {
                spine_version,
                binary_version,
            }) => {
                assert_eq!((spine_version, binary_version), (SPINE_SCHEMA_VERSION, older));
            }
            Err(other) => panic!("[{label}] expected spine_schema_newer, got {other:?}"),
            Ok((_, report)) => panic!(
                "[{label}] an older binary OPENED a spine carrying a live tranche; its pass \
                 aborted {:?} (the anchor is {})",
                report.aborted, anchor.hash
            ),
        }
        // Nothing was written by the refused open: the anchor is as it was.
        let c = Cauldron::open(&dir, SpineConfig::default()).expect("this binary opens it");
        let after = c.tranche_anchor("T35", 1).unwrap().expect("anchor");
        assert!(!after.decided && !after.cleared, "[{label}] the anchor is untouched");
        assert!(
            c.recovered_at_open().skipped_tranche.contains(&anchor.hash),
            "[{label}] this binary's pass SKIPPED the anchor"
        );
        assert_eq!(
            c.tranche_store("T35", None).unwrap().state,
            if open_it { TrancheState::Open } else { TrancheState::Queued }
        );
    }
}

// ---------------------------------------------------------------------------
// g1 — the ordinary recovery pass leaves an anchor undecided and un-aborted
// ---------------------------------------------------------------------------

/// RED (measured): the skip in `recover::recover_pass` removed — the
/// anchor is aborted at open.
#[test]
fn g1_ordinary_recovery_skips_the_anchor_and_still_aborts_an_ordinary_undecided_row() {
    let dir = root("g1");
    let c = fresh(&dir);
    begin(&c, "TG1", &key(0x61), 1_000);
    let anchor = c.tranche_anchor("TG1", 1).unwrap().expect("anchor");
    assert!(anchor.tranche.as_ref().is_some_and(|m| m.is_anchor()));
    assert!(!anchor.decided && !anchor.cleared);
    drop(c);

    // OPPOSED: an ordinary un-decided row with no tranche_id, stranded the
    // way a dead process strands one.
    let ordinary = Envelope {
        statement: "CREATE (d: fixture-alpha/doc { title: \"stranded\", state: \"draft\" })".into(),
        bindings: Bindings::new(),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::CoreMechanical {
                operation: "g1".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some("g1".into()),
    };
    let ordinary_hash = ordinary.hash_identity().unwrap();
    let ordinary_seq = {
        let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).unwrap();
        spine
            .append_pending(
                &ordinary,
                &ordinary_hash,
                &CompiledPayload {
                    parts: vec![],
                    minted: BTreeMap::new(),
                },
            )
            .unwrap()
    };

    let c = Cauldron::open(&dir, SpineConfig::default()).expect("open recovers");
    let report = c.recovered_at_open();
    assert_eq!(report.aborted, vec![ordinary_hash], "the ordinary row IS aborted");
    assert_eq!(report.skipped_tranche, vec![anchor.hash], "the anchor is skipped");
    let ordinary_row = c.transaction(ordinary_seq).unwrap();
    assert!(!ordinary_row.decided && ordinary_row.cleared);
    let after = c.tranche_anchor("TG1", 1).unwrap().unwrap();
    assert!(!after.decided && !after.cleared, "the anchor is still undecided and un-aborted");
    // `Cauldron::recover()` on the live cauldron touches no tranche row.
    let again = c.recover().unwrap();
    assert_eq!(again.skipped_tranche, vec![anchor.hash]);
    assert!(again.aborted.is_empty());
    assert_eq!(c.tranche_store("TG1", None).unwrap().state, TrancheState::Open);
}

// ---------------------------------------------------------------------------
// g2 — queue order is by anchor seq, never the clock
// ---------------------------------------------------------------------------

/// RED (measured): `position_of` and `head_of_queue` ordered by
/// `began_at` instead of `seq` — B, begun second with an EARLIER clock, opens.
#[test]
fn g2_the_queue_is_ordered_by_anchor_seq_when_the_clocks_are_out_of_order() {
    let dir = root("g2");
    let c = fresh(&dir);
    let (ka, kb) = (key(0xa1), key(0xb2));
    let a = begin(&c, "TA", &ka, 5_000);
    // B begins SECOND, with a clock forced EARLIER than A's.
    let b = begin(&c, "TB", &kb, 1_000);
    assert_eq!((a.state, a.position), (TrancheState::Open, 1));
    assert_eq!((b.state, b.position), (TrancheState::Queued, 2));
    assert_eq!(
        b.ahead,
        Some(TrancheRef {
            tranche: "TA".into(),
            attempt: 1,
            state: TrancheState::Open
        })
    );
    let sa = c.tranche_anchor("TA", 1).unwrap().unwrap().seq;
    let sb = c.tranche_anchor("TB", 1).unwrap().unwrap().seq;
    assert!(sa < sb, "A's anchor precedes B's in the spine");

    // B stays queued, position 2, while A is live — whatever the clock.
    let tick = c.tranche_tick(&QUIET, 6_000).unwrap();
    assert!(tick.opened.is_empty(), "{tick:?}");
    let rb = c.tranche_heartbeat(&actor("TB", &kb), &QUIET, 6_001).unwrap();
    assert_eq!((rb.state, rb.position), (TrancheState::Queued, Some(2)));

    // A ends; the next tick opens B.
    c.tranche_rollback(&actor("TA", &ka), 7_000).unwrap();
    let tick = c.tranche_tick(&QUIET, 7_001).unwrap();
    assert_eq!(tick.opened, vec![("TB".to_string(), 1)]);
    let rb = c.tranche_heartbeat(&actor("TB", &kb), &QUIET, 7_002).unwrap();
    assert_eq!((rb.state, rb.position), (TrancheState::Open, None));
}

// ---------------------------------------------------------------------------
// g3 — lock mode `whole` is ACCEPTED (it was once refused by name): an empty queue opens it inline with the gate up; a
// tranche ahead queues it at position 2 with no gate of its own
// ---------------------------------------------------------------------------

/// This guard once asserted `whole` refused `not_yet` (a stub before the
/// lock existed); it now asserts the
/// opposite — the refusal is gone from `begin` AND from `advance_head`. The gate's
/// own behaviour is `tests/tranche/locks.rs`'s.
// RED (measured): a `NotYet` arm put back
// in `tranche_begin` → `whole` is refused again and this guard fails.
#[test]
fn g3_whole_is_accepted_at_begin_and_opens_inline_on_an_empty_queue() {
    let dir = root("g3");
    let c = fresh(&dir);
    // Empty queue, nothing granted anywhere: a `whole` tranche opens inline.
    let b = c
        .tranche_begin(&begin_req("TW1", &key(0x71), TrancheLockMode::Whole), &QUIET, 1_000)
        .expect("whole is accepted");
    assert_eq!((b.state, b.lock_mode, b.position), (TrancheState::Open, TrancheLockMode::Whole, 1));
    assert!(!b.draining, "open, not draining: {b:?}");
    // OPPOSED: `touched` behind it queues at position 2.
    let t = c
        .tranche_begin(&begin_req("TT", &key(0x72), TrancheLockMode::Touched), &QUIET, 1_001)
        .expect("touched queues");
    assert_eq!((t.state, t.position), (TrancheState::Queued, 2));
    // A second `whole` behind both: queued at 3, and NOT draining (only the
    // head ever drains).
    let w2 = c
        .tranche_begin(&begin_req("TW2", &key(0x73), TrancheLockMode::Whole), &QUIET, 1_002)
        .expect("whole behind a queue");
    assert_eq!((w2.state, w2.position, w2.draining), (TrancheState::Queued, 3, false));
    assert_eq!(c.tranche_store("TW1", None).unwrap().lock_mode, TrancheLockMode::Whole);
}

// ---------------------------------------------------------------------------
// g4 — `type_three` provenance
// ---------------------------------------------------------------------------

fn type_three(scope: &[&str]) -> Envelope {
    Envelope {
        statement: String::new(),
        bindings: Bindings::new(),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Ripple {
                kind: RippleKind::TypeThree,
                run_id: "T".into(),
                step_id: "T".into(),
                scope: scope.iter().map(|s| s.to_string()).collect(),
            },
            instrument: instrument(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some("T".into()),
    }
}

/// RED (measured): `check_provenance`'s `TypeThree` scope arm removed
/// — a scoped type-three provenance is accepted.
#[test]
fn g4_type_three_with_a_scope_is_refused_empty_scope_accepted_and_the_wire_name_round_trips() {
    match type_three(&["meta.blocking"]).check_provenance() {
        Err(SpineError::Usage { message }) => assert!(message.contains("type_three"), "{message}"),
        other => panic!("a scoped type_three must be refused, got {other:?}"),
    }
    // OPPOSED: the empty scope is accepted.
    type_three(&[]).check_provenance().expect("empty scope accepted");
    // The wire: `type_three`, both ways.
    assert_eq!(serde_json::to_value(RippleKind::TypeThree).unwrap(), "type_three");
    assert_eq!(RippleKind::TypeThree.as_str(), "type_three");
    let back: RippleKind = serde_json::from_value(serde_json::json!("type_three")).unwrap();
    assert_eq!(back, RippleKind::TypeThree);
    let init = type_three(&[]).provenance.initiator;
    let text = serde_json::to_string(&init).unwrap();
    assert!(text.contains(r#""kind":"type_three""#), "{text}");
    assert_eq!(serde_json::from_str::<Initiator>(&text).unwrap(), init);
}

// ---------------------------------------------------------------------------
// g5 — the quiet start's bound, from head_since
// ---------------------------------------------------------------------------

/// RED (measured): the bound dropped from `advance_head` (opens only
/// when the levels hold) — the head never opens while the host stays busy.
/// The period outlasted is the bound itself, SOUP_TRANCHE_QUIET_WAIT_MS =
/// 15000 ms by default; the test crosses it by the clock it passes.
#[test]
fn g5_not_quiet_holds_the_head_queued_until_head_since_plus_the_15000ms_bound() {
    let dir = root("g5");
    let c = fresh(&dir);
    let bound = c.config().tranche.quiet_wait.as_millis() as i64;
    assert_eq!(bound, 15_000);
    let busy = QuietLevels::Levels {
        trigger_host_pending: 1,
        runner_pending: 0,
    };
    let k = key(0x55);
    let b = c
        .tranche_begin(&begin_req("TQ", &k, TrancheLockMode::Touched), &busy, 1_000)
        .unwrap();
    assert_eq!((b.state, b.position), (TrancheState::Queued, 1));
    // Just inside the bound: still queued, and the beat shows what it waits on.
    let tick = c.tranche_tick(&busy, 1_000 + bound - 1).unwrap();
    assert!(tick.opened.is_empty());
    let r = c.tranche_heartbeat(&actor("TQ", &k), &busy, 1_000 + bound - 1).unwrap();
    assert_eq!(r.state, TrancheState::Queued);
    let q = r.quiet.expect("queued at the head with the bus configured shows `quiet`");
    assert_eq!((q.trigger_host_pending, q.runner_pending, q.waited_ms), (1, 0, bound - 1));
    // At the bound: it opens, quiet or not.
    let tick = c.tranche_tick(&busy, 1_000 + bound).unwrap();
    assert_eq!(tick.opened, vec![("TQ".to_string(), 1)]);

    // OPPOSED: a verdict that turns quiet opens the head BEFORE the bound.
    c.tranche_rollback(&actor("TQ", &k), 20_000).unwrap();
    let k2 = key(0x56);
    let b = c
        .tranche_begin(&begin_req("TQ2", &k2, TrancheLockMode::Touched), &busy, 30_000)
        .unwrap();
    assert_eq!(b.state, TrancheState::Queued);
    let quiet = QuietLevels::Levels {
        trigger_host_pending: 0,
        runner_pending: 0,
    };
    let tick = c.tranche_tick(&quiet, 30_001).unwrap();
    assert_eq!(tick.opened, vec![("TQ2".to_string(), 1)], "quiet opens at once");
}

// ---------------------------------------------------------------------------
// the rest of the record: idempotence (begin and rollback),
// the key, rollback into the store, the deadline reset at open
// ---------------------------------------------------------------------------

#[test]
fn begin_and_rollback_are_idempotent_and_the_store_reads_the_attempt_back() {
    let dir = root("idem");
    let c = fresh(&dir);
    let k = key(0x36);
    let first = begin(&c, "T36", &k, 1_000);
    let again = begin(&c, "T36", &k, 1_500);
    assert_eq!(first, again, "begin twice with the same key: the same state, no error");

    // A live attempt in the store: no refusal, no children yet.
    let live = c.tranche_store("T36", None).unwrap();
    assert_eq!((live.state, live.refusal.clone()), (TrancheState::Open, None));
    assert_eq!(live.regions, vec!["fixture-alpha".to_string()]);

    let r1 = c.tranche_rollback(&actor("T36", &k), 2_000).unwrap();
    let r2 = c.tranche_rollback(&actor("T36", &k), 3_000).unwrap();
    assert_eq!(r1, r2, "rollback twice: the same reply");
    assert_eq!((r1.state, r1.attempt, r1.stored), (TrancheState::RolledBack, 1, 0));

    let store = c.tranche_store("T36", Some(1)).unwrap();
    assert_eq!(store.state, TrancheState::RolledBack);
    assert_eq!(store.reason.as_deref(), Some("holder"));
    assert_eq!(store.ended_at, Some(2_000));
    let anchor = c.tranche_anchor("T36", 1).unwrap().unwrap();
    assert!(!anchor.decided && anchor.cleared, "rollback aborts the anchor");
    assert_eq!(anchor.result.as_ref().unwrap()["tranche"]["state"], "rolled-back");

    // After the end, a beat is refused tranche_terminal {state: rolled-back}.
    match only_witness(c.tranche_heartbeat(&actor("T36", &k), &QUIET, 4_000).unwrap_err()) {
        Witness::TrancheTerminal { state, reason, .. } => {
            assert_eq!((state.as_str(), reason.as_deref()), ("rolled-back", Some("holder")))
        }
        other => panic!("{other:?}"),
    }
    // An abandoned attempt refuses rollback with tranche_terminal.
    let k2 = key(0x37);
    begin(&c, "T37", &k2, 10_000);
    let timeout = c.config().tranche.timeout.as_millis() as i64;
    c.tranche_tick(&QUIET, 10_000 + timeout + 1).unwrap();
    match only_witness(c.tranche_rollback(&actor("T37", &k2), 99_000).unwrap_err()) {
        Witness::TrancheTerminal { state, .. } => assert_eq!(state, "abandoned"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_key_admits_its_own_attempt_and_service_only() {
    let dir = root("key");
    let c = fresh(&dir);
    let k = key(0x41);
    begin(&c, "TK", &k, 1_000);
    let why = |e: SpineError| match e {
        SpineError::TrancheKey { why } => why,
        other => panic!("expected tranche_key, got {other:?}"),
    };
    // A random wrong key.
    assert_eq!(
        why(c.tranche_heartbeat(&actor("TK", &key(0x42)), &QUIET, 1_001).unwrap_err()),
        "no_tranche"
    );
    // The right key from another service.
    let mut other = actor("TK", &k);
    other.holder_service = "intruder".into();
    assert_eq!(why(c.tranche_heartbeat(&other, &QUIET, 1_002).unwrap_err()), "other_service");
    // `begin` under the live T with another key.
    assert_eq!(
        why(c
            .tranche_begin(&begin_req("TK", &key(0x43), TrancheLockMode::Touched), &QUIET, 1_003)
            .unwrap_err()),
        "live_other_key"
    );
    // A malformed key is a usage refusal (the door's `malformed`).
    let bad = "AB".repeat(32);
    assert!(matches!(
        c.tranche_begin(&begin_req("TK2", &bad, TrancheLockMode::Touched), &QUIET, 1_004),
        Err(SpineError::Usage { .. })
    ));
    // The right key, the right service: admitted.
    c.tranche_heartbeat(&actor("TK", &k), &QUIET, 1_005).expect("admitted");
}

#[test]
fn open_resets_a_live_deadline_to_boot_plus_timeout() {
    let dir = root("reset");
    let c = fresh(&dir);
    let timeout = c.config().tranche.timeout.as_millis() as i64;
    let k = key(0x27);
    begin(&c, "TR", &k, 1_000);
    drop(c);
    let wall = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    };
    let before = wall();
    let c = Cauldron::open(&dir, SpineConfig::default()).unwrap();
    let after = wall();
    // Without the reset the deadline is 1_000 + timeout, long past: a tick
    // just short of `before + timeout` would abandon it.
    let tick = c.tranche_tick(&QUIET, before + timeout - 1).unwrap();
    assert!(tick.abandoned.is_empty(), "the restart is not the holder's fault: {tick:?}");
    // And the reset is boot + timeout, not forever: past `after + timeout`
    // the tick abandons it.
    let tick = c.tranche_tick(&QUIET, after + timeout + 1).unwrap();
    assert_eq!(tick.abandoned, vec![("TR".to_string(), 1)]);
}

// ---------------------------------------------------------------------------
// g6 — a spine from before the tranche record is upgraded in place at open
// ---------------------------------------------------------------------------

/// Turn a fresh spine back into the shape every spine had before the tranche
/// record (version 0): no `spine_schema_version`, no `tranche_*` columns, no
/// `tranches` / `tranche_children` tables.
fn make_version_zero(dir: &Path) {
    // SQLite's `DROP COLUMN` refuses on this DDL (its comments end up
    // unterminated in the rewritten text), so the two tables are REBUILT with
    // the column set `spine_db.rs` had before the tranche record, and its four
    // indexes.
    let conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).unwrap();
    conn.execute_batch(
        r#"
        DROP INDEX idx_tx_tranche;
        DROP TABLE tranche_children;
        DROP TABLE tranches;
        CREATE TABLE tx_v0 (
          seq INTEGER PRIMARY KEY AUTOINCREMENT, hash TEXT NOT NULL, attempt INTEGER NOT NULL DEFAULT 1,
          ts TEXT NOT NULL, envelope_version INTEGER NOT NULL,
          initiator TEXT NOT NULL CHECK (json_valid(initiator)), instrument TEXT NOT NULL CHECK (json_valid(instrument)),
          statement TEXT NOT NULL, bindings TEXT NOT NULL CHECK (json_valid(bindings)),
          payload TEXT NOT NULL CHECK (json_valid(payload)), result TEXT CHECK (result IS NULL OR json_valid(result)),
          decided INTEGER NOT NULL DEFAULT 0, cleared INTEGER NOT NULL DEFAULT 0, recovered INTEGER NOT NULL DEFAULT 0,
          committed_at INTEGER, touched TEXT NOT NULL CHECK (json_valid(touched)));
        INSERT INTO tx_v0 SELECT seq, hash, attempt, ts, envelope_version, initiator, instrument, statement, bindings,
          payload, result, decided, cleared, recovered, committed_at, touched FROM transactions;
        DROP TABLE transactions;
        ALTER TABLE tx_v0 RENAME TO transactions;
        CREATE INDEX idx_tx_committed_at ON transactions(committed_at) WHERE committed_at IS NOT NULL;
        CREATE UNIQUE INDEX idx_tx_live ON transactions(hash) WHERE cleared = 0;
        CREATE UNIQUE INDEX idx_tx_committed ON transactions(hash) WHERE decided = 1;
        CREATE INDEX idx_tx_hash ON transactions(hash, attempt);
        CREATE TABLE state_v0 (singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
          halted INTEGER NOT NULL DEFAULT 0, halted_reason TEXT);
        INSERT INTO state_v0 SELECT singleton, halted, halted_reason FROM spine_state;
        DROP TABLE spine_state;
        ALTER TABLE state_v0 RENAME TO spine_state;
"#,
    )
    .unwrap();
}

/// RED (measured): `fence_and_upgrade` returning before the upgrade
/// when the spine is older — the version-0 spine opens and its first tranche
/// act fails on the missing table.
#[test]
fn g6_a_version_zero_spine_is_upgraded_in_place_at_open_and_its_rows_survive() {
    let dir = root("g6");
    let c = fresh(&dir);
    let seq = match c
        .submit(Envelope {
            statement: "CREATE (d: fixture-alpha/doc { title: \"before\", state: \"draft\" })".into(),
            bindings: Bindings::new(),
            format_version: ENVELOPE_FORMAT_VERSION,
            provenance: Provenance {
                initiator: Initiator::CoreMechanical {
                    operation: "g6".into(),
                },
                instrument: Instrument::door(),
            },
            mode: Mode::Commit,
            idempotency_nonce: Some("g6".into()),
        })
        .expect("an ordinary commit")
    {
        spine::Outcome::Committed { seq, .. } => seq,
        other => panic!("{other:?}"),
    };
    drop(c);
    make_version_zero(&dir);
    // Sentinel: the shape really is version 0 (no tranches table).
    {
        let conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('tranches','tranche_children')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "the staged spine is version 0");
    }
    let c = Cauldron::open(&dir, SpineConfig::default()).expect("a version-0 spine opens");
    // The row from before reads back, with no tranche mark.
    let row = c.transaction(seq).unwrap();
    assert!(row.decided && row.cleared && row.tranche.is_none());
    // The tranche record now exists.
    let b = begin(&c, "TU", &key(0x66), 1_000);
    assert_eq!((b.attempt, b.state), (1, TrancheState::Open));
    let version: i64 = rusqlite::Connection::open(dir.join(SPINE_FILE))
        .unwrap()
        .query_row("SELECT spine_schema_version FROM spine_state", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SPINE_SCHEMA_VERSION as i64);
}

// ---------------------------------------------------------------------------
// g_j1 — which T a key names, read from the record
// ---------------------------------------------------------------------------

/// **g_j1 — `tranche_by_key` AT N = 2, ACROSS A REOPEN, TERMINAL INCLUDED.**
/// TA open and TB queued under two keys; TR begun and rolled back under a
/// third. After a reopen (a restart: a fresh handle, the record only), each
/// live key names its OWN tranche and not the other's (keys not crossed),
/// and the act the answer admits is the right one (TA's beat `open`, TB's
/// `queued`); the rolled-back key still names TR, so a child under it reaches
/// `tranche_terminal` rather than the wrong-key answer. OPPOSED: a key
/// no tranche holds names nothing; the right key on another service names
/// nothing; a malformed key is a usage refusal, never an answer.
/// RED: `tranche_by_key` answering every row's `T` (no compare) → TA's key
/// names TB too; answering only the first row → TB's key names TA.
#[test]
fn g_j1_tranche_by_key_names_each_keys_own_tranche_at_n_2_across_a_reopen_terminal_included() {
    let dir = root("bykey");
    let c = fresh(&dir);
    let (ka, kb, kr) = (key(0x51), key(0x52), key(0x53));
    assert_eq!(begin(&c, "TA", &ka, 1_000).state, TrancheState::Open);
    assert_eq!(begin(&c, "TB", &kb, 1_001).state, TrancheState::Queued);
    begin(&c, "TR", &kr, 1_002);
    c.tranche_rollback(&actor("TR", &kr), 1_003).unwrap();
    drop(c);
    let c = Cauldron::open(&dir, SpineConfig::default()).expect("reopen");
    let names = |k: &str| c.tranche_by_key(k, SERVICE).expect("lookup");
    assert_eq!(names(&ka), vec!["TA".to_string()], "TA's key names TA alone");
    assert_eq!(names(&kb), vec!["TB".to_string()], "TB's key names TB alone");
    assert_eq!(names(&kr), vec!["TR".to_string()], "a terminal attempt's key still names its T");
    // The answers admit the right acts.
    let ta = c.tranche_heartbeat(&actor(&names(&ka)[0], &ka), &QUIET, 2_000).expect("TA's beat");
    assert_eq!(ta.state, TrancheState::Open);
    let tb = c.tranche_heartbeat(&actor(&names(&kb)[0], &kb), &QUIET, 2_001).expect("TB's beat");
    assert_eq!(tb.state, TrancheState::Queued);
    match only_witness(c.tranche_heartbeat(&actor(&names(&kr)[0], &kr), &QUIET, 2_002).unwrap_err()) {
        Witness::TrancheTerminal { state, .. } => assert_eq!(state, "rolled-back"),
        other => panic!("{other:?}"),
    }
    // OPPOSED.
    assert!(names(&key(0x54)).is_empty(), "a key no tranche holds names nothing");
    assert!(c.tranche_by_key(&ka, "intruder").unwrap().is_empty(), "another service's lookup names nothing");
    assert!(matches!(c.tranche_by_key(&"AB".repeat(32), SERVICE), Err(SpineError::Usage { .. })));
}
