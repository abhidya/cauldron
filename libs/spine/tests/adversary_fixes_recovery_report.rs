//! ADVERSARIAL TEST — lens: WHAT THE FIXES INTRODUCED. THE RECOVERY REPORT
//! AT N = 2 ON THE ABORT ARM.
//!
//! The DECIDED arm's report is refined: a lost cleared flip whose
//! winner was another recovery (the row's `recovered` mark says so) still
//! reports the row finished; a lost flip whose winner was the live door
//! reports `skipped_live`. The UN-DECIDED arm got no such refinement: a lost
//! abort flip is reported `skipped_live` unconditionally — "a live door
//! decided this row between the worklist read and the abort" (the code's
//! own comment, still in place). But a lost abort flip has TWO possible winners,
//! and only one of them is a live door; the other is a second `recover()`
//! over the same crashed row, which is the ordinary two-processes-restarting
//! case the held test `two_concurrent_recoveries_over_one_crashed_row` was
//! written for — on the decided arm only.
//!
//! What should be true: `RecoveryReport::skipped_live` names rows a LIVE DOOR
//! finished under the pass (its own doc comment). With no live door anywhere,
//! it is empty, in every report, however many recoveries overlap.
//!
//! Red tests are marked `#[ignore = "ADVERSARY RED (fixes): ..."]`; run with
//! `--ignored`. Nothing under `src/` was touched.

use spine::config::SpineConfig;
use spine::delta::{CompiledPart, CompiledPayload};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance, TxHash};
use spine::schema_def::{fixtures, PoolName};
use spine::spine_db::{AppendOutcome, SpineDb, SPINE_FILE};
use spine::Cauldron;
use std::path::{Path, PathBuf};
use std::time::Duration;

test_scratch::install!();

/// Enough rows that one pass's worklist READ (a read_row per row, no write
/// lock) outlasts the other pass's busy-handler backoff on its first write,
/// so the second pass gets its sweep in while the first is still reading and
/// both walk the same worklist. (A held write lock released onto both does
/// NOT do it: the pass that wins the lock streams writes and the other's
/// busy handler starves behind them until the walk is over.)
const ROWS: u16 = 3000;

/// Many crashed UN-DECIDED rows (a process that died after its appends and
/// before any decided flip — nothing landed, nothing locked), and two
/// `recover()` passes released together onto them. Every row is aborted by
/// exactly one pass. Neither report may say a LIVE DOOR finished anything:
/// there is no live door.
#[test]
fn two_overlapping_recoveries_over_crashed_undecided_rows_report_no_live_door() {
    let dir = scratch("abort-arm-n2");
    let _c = Cauldron::create(
        &dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        SpineConfig::default(),
    )
    .expect("create");
    // ONE template row through the real append (so every JSON column is the
    // shape the record parser expects), then the rest copied from it in one
    // transaction under fresh hashes: crashed, un-decided, zero parts.
    {
        let mut spine = spine_of(&dir);
        match spine
            .append_or_existing(&envelope("-- crashed attempt 0"), &hash_of(0), &payload())
            .expect("append")
        {
            AppendOutcome::Appended(_) => {}
            other => panic!("fresh append, got {other:?}"),
        }
    }
    {
        let mut conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("spine");
        let tx = conn.transaction().expect("tx");
        for i in 1..ROWS {
            tx.execute(
                "INSERT INTO transactions
                   (hash, attempt, ts, envelope_version, initiator, instrument, statement,
                    bindings, payload, touched)
                 SELECT ?1, 1, ts, envelope_version, initiator, instrument, statement,
                        bindings, payload, touched
                 FROM transactions WHERE seq = 1",
                rusqlite::params![hash_of(i).to_hex()],
            )
            .expect("stage a crashed row");
        }
        tx.commit().expect("commit the staging");
    }
    assert_eq!(spine_of(&dir).pending().expect("pending").len(), ROWS as usize);

    // TWO PASSES AT ONCE. The worklist read of the first to sweep (`pending()`
    // over every row, a read that holds no write lock) outlasts the second's
    // busy-handler backoff on its own sweep, so the second sweeps and reads
    // while the first is still reading; both then walk one worklist.
    let cfg = SpineConfig {
        busy_timeout: Duration::from_secs(30),
        ..SpineConfig::default()
    };
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| Cauldron::open_unrecovered(&dir, cfg.clone()).expect("open").recover());
        let b = scope.spawn(|| Cauldron::open_unrecovered(&dir, cfg.clone()).expect("open").recover());
        (a.join().expect("a"), b.join().expect("b"))
    });
    let (a, b) = (a.expect("recovery a"), b.expect("recovery b"));

    // The overlap this test claims: BOTH passes read a non-empty worklist
    // (each aborted something, or lost an abort to the other). If one of
    // them read an empty worklist the interleaving did not happen and the
    // test proves nothing — say so, distinctly from the finding.
    // (Fixing pass two: a lost abort flip now lands in `aborted_elsewhere`,
    // a bucket that did not exist when this guard was written; a pass that
    // lost every flip still walked the worklist, so the guard counts it.)
    let walked = |r: &spine::recover::RecoveryReport| {
        r.aborted.len() + r.aborted_elsewhere.len() + r.skipped_live.len()
    };
    assert!(
        walked(&a) > 0 && walked(&b) > 0,
        "no overlap observed (a walked {} = {} aborted + {} aborted elsewhere + {} skipped_live; \
         b walked {} = {} + {} + {}); rerun — this is the staging, not the finding",
        walked(&a),
        a.aborted.len(),
        a.aborted_elsewhere.len(),
        a.skipped_live.len(),
        walked(&b),
        b.aborted.len(),
        b.aborted_elsewhere.len(),
        b.skipped_live.len()
    );
    // Every row aborted exactly once, between them.
    let mut all: Vec<&TxHash> = a.aborted.iter().chain(b.aborted.iter()).collect();
    all.sort_by_key(|h| h.to_hex());
    all.dedup_by_key(|h| h.to_hex());
    assert_eq!(all.len(), ROWS as usize, "every crashed row was aborted by exactly one pass");
    assert!(spine_of(&dir).pending().expect("pending").is_empty());

    // THE ASSERTION: no live door exists, so no report names one.
    for (label, report) in [("a", &a), ("b", &b)] {
        assert!(
            report.skipped_live.is_empty(),
            "recovery {label} reports {} row(s) as finished by a LIVE DOOR (skipped_live) — \
             there is no live door; the other party was recovery {}, whose abort won. First \
             rows: {:?}",
            report.skipped_live.len(),
            if label == "a" { "b" } else { "a" },
            report
                .skipped_live
                .iter()
                .take(3)
                .map(|h| h.to_hex())
                .collect::<Vec<_>>()
        );
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

fn hash_of(i: u16) -> TxHash {
    let mut bytes = [0u8; 32];
    bytes[0] = (i & 0xff) as u8;
    bytes[1] = (i >> 8) as u8;
    bytes[2] = 0xab;
    TxHash(bytes)
}

fn payload() -> CompiledPayload {
    CompiledPayload {
        parts: vec![CompiledPart {
            pool: PoolName::new("fixture-alpha"),
            schema_version: fixtures::fixture_alpha().version,
            deltas: vec![],
            premises: vec![],
            far_versions: Default::default(),
        }],
        minted: Default::default(),
    }
}

fn envelope(statement: &str) -> Envelope {
    Envelope {
        statement: statement.to_string(),
        bindings: Bindings::new(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent {
                thread: "adversary-fixes".into(),
                dispatch: "recovery-report".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}
