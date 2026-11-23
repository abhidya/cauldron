//! THE RECOVER-AT-OPEN SEAM: a
//! transaction STRANDED by a dead process — decided, never cleared, its locks
//! held forever — is recovered by the next `Cauldron::open`, and a door that
//! waits out its lock ceiling behind such a row names it STRANDED and names
//! `recover()` as the repair.
//!
//! Two ways to strand a row, so the seam is measured in both builds: the
//! fault seam kills a real child process at the `decided` stage
//! (`fault-injection` only), and the test-support staging writes a decided
//! row by hand and takes its locks through `locks::acquire` (every build).
//! `Cauldron::open_unrecovered` is the handle that does NOT heal, used only
//! to observe the pre-state and as the pre-opened "live" handle that stands
//! beside the crash.

#[path = "util/kill.rs"]
mod kill;

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::locks::{self, Claim, ClaimSet, LockMode, Resource};
use spine::pool::NodeId;
use spine::schema_def::{fixtures, PoolName};
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use spine::{Cauldron, Outcome, RecoveryReport};
use std::path::{Path, PathBuf};
use std::time::Duration;

test_scratch::install!();

const ALPHA: &str = "fixture-alpha";

// ===========================================================================
// open heals a row stranded by a real process death (the fault seam)
// ===========================================================================

/// A child dies at the `decided` stage: its row is decided, no part landed,
/// its locks held. `Cauldron::open` finishes it — every part rolled forward
/// from the row's own payload, the row cleared and marked recovered, the
/// locks released — and reports so; a second open reports nothing.
// RED: (a) the recovery call removed from `Cauldron::open` (`open` built on
// `open_interior` alone) — `recovered_at_open().rolled_forward` was `[]`
// against `[hash]`, and the row still stood decided-and-un-cleared with its
// locks held; (b) `open` pushing a fabricated hash into `rolled_forward`
// after a pass that did nothing — the SECOND open's report was not empty.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn a_row_stranded_decided_by_a_dead_process_is_recovered_by_the_next_open() {
    let _env = kill::env_shared();
    let dir = kill::scratch("seams-open-decided");
    kill::kill_at("cross-schema", &dir, "decided");

    // THE SENTINEL: the pre-state really is a stranded row, read off the
    // disk through the non-healing handle — otherwise the open below would
    // be measured against nothing.
    let stuck = kill::the_stuck_row(&dir);
    assert!(stuck.decided && !stuck.cleared, "decided and un-cleared: {stuck:?}");
    assert!(stuck.parts.iter().all(|p| !p.landed), "no part landed at `decided`");
    assert!(kill::lock_rows(&dir, stuck.seq) > 0, "the dead child's locks are held");
    assert_eq!(kill::ticket_count(&kill::open(&dir)), 0, "beta holds nothing yet");

    // THE SEAM: open heals.
    let c = Cauldron::open(&dir, SpineConfig::default()).expect("open runs recovery and succeeds");
    assert_eq!(
        c.recovered_at_open().rolled_forward,
        vec![stuck.hash.clone()],
        "open reports the row it rolled forward: {:?}",
        c.recovered_at_open()
    );
    let after = kill::spine_of(&dir).row_at(stuck.seq).expect("row");
    assert!(after.decided && after.cleared && after.recovered, "{after:?}");
    assert!(after.parts.iter().all(|p| p.landed), "every part landed: {:?}", after.parts);
    kill::assert_all_or_nothing(&c, &after, "decided, healed at open");
    assert_eq!(kill::ticket_count(&c), 1, "beta's part was rolled forward");
    assert_eq!(kill::lock_rows(&dir, stuck.seq), 0, "the locks are released");
    assert!(kill::spine_of(&dir).pending().expect("pending").is_empty());

    // IDEMPOTENT: the next open finds nothing to do and says so.
    let again = Cauldron::open(&dir, SpineConfig::default()).expect("open again");
    assert_eq!(*again.recovered_at_open(), RecoveryReport::default());
    assert_eq!(kill::ticket_count(&again), 1, "nothing landed twice");
}

// ===========================================================================
// open heals a row stranded by hand (every build)
// ===========================================================================

/// The same seam without a child process: a decided-and-un-cleared row
/// staged by hand with an exclusive lock on a doc's body. Open clears it
/// and releases the lock; the door call that was refused behind it then
/// lands.
// RED: the recovery call removed from `Cauldron::open` — `cleared_only` was
// `[]`, the lock row stood, and the submit after the open hit its ceiling.
#[test]
fn a_row_stranded_decided_by_hand_is_recovered_by_the_next_open() {
    let dir = root("seams-open-staged");
    let live = fresh_cauldron(&dir, short_ceiling());
    let x = create_doc(&live, "held by a stranded row");
    let (seq, hash) = stage_row_holding(&dir, "d2", true, &x, &short_ceiling());
    assert_eq!(lock_rows(&dir, seq), 1, "the staged row holds its lock");

    let c = Cauldron::open(&dir, short_ceiling()).expect("open runs recovery and succeeds");
    assert_eq!(c.recovered_at_open().cleared_only, vec![hash]);
    let after = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine").row_at(seq).expect("row");
    assert!(after.decided && after.cleared && after.recovered, "{after:?}");
    assert_eq!(lock_rows(&dir, seq), 0, "the lock is released at the cleared flip");
    set_title(&c, &x, "lands now").expect("the body is free: the door call lands");
    assert_eq!(title_of(&dir, &x).as_deref(), Some("lands now"));
}

/// THE OPPOSITE: an open with nothing stranded recovers nothing and changes
/// nothing — the report is empty, the counter and the pools are untouched.
/// A recovery-at-open that fired on a clean cauldron would be measured here.
// RED: `open` pushing a fabricated hash into `rolled_forward` after a pass
// that did nothing — the report was not `default()`.
#[test]
fn an_open_with_nothing_stranded_recovers_nothing_and_changes_nothing() {
    let dir = root("seams-open-clean");
    let c = fresh_cauldron(&dir, SpineConfig::default());
    let x = create_doc(&c, "clean");
    let counter = c.committed_commit_counter().expect("counter");
    assert!(SpineDb::open(&dir.join(SPINE_FILE)).expect("spine").pending().expect("pending").is_empty());

    let reopened = Cauldron::open(&dir, SpineConfig::default()).expect("open");
    assert_eq!(*reopened.recovered_at_open(), RecoveryReport::default());
    assert_eq!(reopened.committed_commit_counter().expect("counter"), counter);
    assert_eq!(title_of(&dir, &x).as_deref(), Some("clean"));
}

// ===========================================================================
// the Busy-at-ceiling refusal names a stranded holder, and names recover()
// ===========================================================================

/// A live door (a handle opened BEFORE the crash, which is how a live
/// process meets a stranded row) waits out its ceiling behind a
/// decided-and-un-cleared holder. The refusal names the holder's seq, says
/// it is decided and not cleared — STRANDED — and names `recover()` as the
/// repair. Then an open heals it and the same call lands.
// RED: (a) the holder description dropped from the ceiling message (the old
// text alone) — none of "DECIDED and NOT CLEARED", "STRANDED", "recover()"
// was in it; (b) `describe_holder` made a constant "STRANDED" — the
// un-decided case below was named stranded too.
#[test]
fn a_door_that_waits_out_its_ceiling_behind_a_stranded_decided_row_names_it_and_names_recover() {
    let dir = root("seams-busy-stranded");
    let live = fresh_cauldron(&dir, short_ceiling());
    let x = create_doc(&live, "behind a stranded row");
    let (seq, hash) = stage_row_holding(&dir, "d3", true, &x, &short_ceiling());

    let err = set_title(&live, &x, "never lands").expect_err("the body is held");
    let message = match &err {
        SpineError::Busy { message } => message.clone(),
        other => panic!("a ceiling is a loud Busy, got {other:?}"),
    };
    assert!(message.contains("lock wait ceiling exceeded"), "{message}");
    assert!(
        message.contains(&format!("blocked by holder seq {seq}")),
        "names the holder it waited behind: {message}"
    );
    assert!(
        message.contains(&format!("holder seq {seq} (appended ")),
        "describes that holder by its spine row: {message}"
    );
    assert!(message.contains("DECIDED and NOT CLEARED"), "names its state: {message}");
    assert!(message.contains("STRANDED"), "names it stranded: {message}");
    assert!(message.contains("recover()"), "names the repair: {message}");
    assert!(message.contains("Cauldron::open runs it"), "and where it runs: {message}");
    assert!(!message.contains("NOT DECIDED"), "it is decided; the other arm must not fire: {message}");

    // The repair the message named: an open heals it, and the call lands.
    let healed = Cauldron::open(&dir, short_ceiling()).expect("open");
    assert_eq!(healed.recovered_at_open().cleared_only, vec![hash]);
    set_title(&live, &x, "lands now").expect("the live handle's call lands after the heal");
    assert_eq!(title_of(&dir, &x).as_deref(), Some("lands now"));
}

/// THE OPPOSITE HOLDER: appended and NOT decided (a door between its append
/// and its decided flip, or an attempt that died there). It is named for
/// what it is and NOT called stranded; the repair for it is the abort arm.
// RED: `describe_holder` made a constant "STRANDED" — the `!contains` below.
#[test]
fn a_door_that_waits_out_its_ceiling_behind_an_undecided_holder_does_not_call_it_stranded() {
    let dir = root("seams-busy-undecided");
    let live = fresh_cauldron(&dir, short_ceiling());
    let x = create_doc(&live, "behind an undecided row");
    let (seq, hash) = stage_row_holding(&dir, "d4", false, &x, &short_ceiling());

    let err = set_title(&live, &x, "never lands").expect_err("the body is held");
    let message = match &err {
        SpineError::Busy { message } => message.clone(),
        other => panic!("a ceiling is a loud Busy, got {other:?}"),
    };
    assert!(message.contains(&format!("blocked by holder seq {seq}")), "{message}");
    assert!(message.contains("NOT DECIDED"), "names its state: {message}");
    assert!(!message.contains("STRANDED"), "an un-decided holder is not stranded: {message}");
    assert!(message.contains("recover()"), "the abort arm is still recover()'s: {message}");

    // And open's abort arm is the repair for THIS one.
    let healed = Cauldron::open(&dir, short_ceiling()).expect("open");
    assert_eq!(healed.recovered_at_open().aborted, vec![hash]);
    assert_eq!(lock_rows(&dir, seq), 0);
    set_title(&live, &x, "lands now").expect("the live handle's call lands after the abort");
}

// ===========================================================================
// helpers
// ===========================================================================

fn root(name: &str) -> PathBuf {
    let dir = test_scratch::tmp_path(format!("spine-seams-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn fresh_cauldron(dir: &Path, config: SpineConfig) -> Cauldron {
    Cauldron::create(dir, &[fixtures::fixture_alpha(), fixtures::fixture_beta()], config)
        .expect("create the cauldron")
}

/// A short ceiling so the Busy arms take a fraction of a second, not ten.
fn short_ceiling() -> SpineConfig {
    SpineConfig {
        lock_poll_interval: Duration::from_millis(2),
        lock_wait_ceiling: Duration::from_millis(150),
        ..SpineConfig::default()
    }
}

fn envelope(statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
    let mut b = Bindings::new();
    for (k, v) in params {
        b.insert((*k).to_string(), v.clone());
    }
    Envelope {
        statement: statement.to_string(),
        bindings: b,
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent {
                thread: "seams".into(),
                dispatch: "seams-recover-at-open".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn create_doc(c: &Cauldron, title: &str) -> NodeId {
    match c
        .submit(envelope(
            r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" }) RETURN d.id"#,
            &[("t", serde_json::json!(title))],
        ))
        .expect("create a doc")
    {
        Outcome::Committed { returned, .. } => {
            NodeId(returned.0[0]["d.id"].as_str().expect("id").to_string())
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn set_title(c: &Cauldron, x: &NodeId, title: &str) -> Result<Outcome, SpineError> {
    c.submit(envelope(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t"#,
        &[("id", serde_json::json!(x.0)), ("t", serde_json::json!(title))],
    ))
}

fn title_of(dir: &Path, x: &NodeId) -> Option<String> {
    let pool = spine::pool::Pool::open(&spine::pool::Pool::path_in(dir, &PoolName::new(ALPHA)))
        .expect("alpha");
    let snap = pool.snapshot().expect("snapshot");
    snap.node(x)
        .expect("read")
        .and_then(|n| n.fields.get("title").and_then(|v| v.value.as_str().map(|s| s.to_string())))
}

fn lock_rows(dir: &Path, seq: Seq) -> usize {
    rusqlite::Connection::open(dir.join(SPINE_FILE))
        .expect("spine")
        .query_row("SELECT COUNT(*) FROM locks WHERE seq = ?1", [seq.0], |r| r.get::<_, i64>(0))
        .expect("count") as usize
}

/// Stage a row BY HAND — decided or not — holding an exclusive lock on `x`'s
/// body, so a door call on `x` waits behind it exactly as behind a dead
/// process's row. The record is copied from the first landed row (a CREATE
/// with no RETURN, so recovery's projection is empty) with a fresh hash and
/// an empty payload, so recovery clears it (decided) or aborts it
/// (un-decided) and releases the lock. Returns the seq and the hash.
fn stage_row_holding(
    dir: &Path,
    hex_byte: &str,
    decided: bool,
    x: &NodeId,
    config: &SpineConfig,
) -> (Seq, spine::envelope::TxHash) {
    let conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("spine");
    let hash = hex_byte.repeat(32);
    conn.execute(
        "INSERT INTO transactions
           (hash, attempt, ts, envelope_version, initiator, instrument, statement,
            bindings, payload, touched, decided, cleared)
         SELECT ?1, 1, ts, envelope_version, initiator, instrument,
                'CREATE (d: fixture-alpha/doc { title: \"staged\", state: \"draft\" })',
                '{}', '{\"parts\":[],\"minted\":{}}', '[]', ?2, 0
         FROM transactions ORDER BY seq LIMIT 1",
        rusqlite::params![hash, if decided { 1 } else { 0 }],
    )
    .expect("stage the row");
    let seq = Seq(conn.last_insert_rowid());
    drop(conn);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let mut claims = ClaimSet::new();
    claims.insert(Claim {
        pool: PoolName::new(ALPHA),
        resource: Resource::Body { node: x.clone() },
        mode: LockMode::Write,
    });
    locks::acquire(&spine, seq, &claims, config).expect("an uncontended grant");
    let hash = spine.row_at(seq).expect("row").hash;
    (seq, hash)
}
