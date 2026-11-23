//! **The count cell is a commutative increment, not a plain-write
//! counter**: any machine-maintained count cell updates by commutative
//! increment, never read-modify-write — a plain-write counter would
//! re-serialize every create one level down.
//!
//! # Why the old test could not tell
//!
//! Four concurrent connectors driving four increments of one cell stayed
//! GREEN when `bump_cell` was rewritten as read-modify-write with a 15ms
//! gap, because `Pool::apply_part`'s `BEGIN IMMEDIATE` already serializes two
//! landings on one pool file. INSIDE one landing transaction the distinction
//! is genuinely unobservable — and that is not a defect; the file lock is
//! doing its job.
//!
//! # What the rule actually protects, and the interleaving that exposes it
//!
//! The property is one level UP. The cell must never be a plain-write
//! counter whose value was computed from the SNAPSHOT the statement matched
//! against, because that is the design that would force every create to
//! hold a WRITE lock on the counted node and re-serialize creates. The
//! discriminating interleaving is therefore:
//!
//! 1. transaction A matches its snapshot (cell = 0) and passes its decided
//!    flip;
//! 2. A is HELD between the flip and its landing — a live transaction, its
//!    locks granted, its snapshot taken;
//! 3. transaction B connects to the same node and lands fully (cell = 1);
//! 4. A is released and lands.
//!
//! Commutative increment: 2. Plain-write from A's snapshot: 1. That window
//! can only be held open deterministically by a PAUSE at a named stage, so
//! this suite runs on the seam's pause form (`SPINE_PAUSE=decided`)
//! and is `fault-injection` only, like every other seam-dependent suite.
//!
//! The same window proves the connect carve-out as a theorem from outside:
//! B — an ordinary door `submit` — COMPLETES while A is held, under a short
//! lock ceiling. A holds a read on the author's body and a link on its
//! `wrote` slice; if it held a write on the author, B would sit behind it
//! until the ceiling and come back `Busy`.
//!
//! Every checker here was forced RED at least once; the poison is named in a
//! `// RED:` comment above each test.

#![cfg(all(unix, feature = "fault-injection"))]

#[path = "util/kill.rs"]
mod kill;

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::pool::{NodeId, Pool};
use spine::schema_def::{fixtures, PoolName};
use spine::spine_db::{SpineDb, SPINE_FILE};
use spine::{Cauldron, Outcome, ReadRequest};
use std::path::Path;
use std::time::{Duration, Instant};

test_scratch::install!();

const ALPHA: &str = "fixture-alpha";
const CELL: &str = "docs_written";

/// The parent's own connect — the same shape the victim's `connect` job
/// submits, under a different title and a different initiator so the two
/// are two hash identities.
const CONNECT: &str = r#"MATCH (a: fixture-alpha/author { id: $a })
   CREATE (d: fixture-alpha/doc { title: $t, state: "draft" })
   CREATE (a) -[wrote]-> (d)
   RETURN d.id"#;

// ---------------------------------------------------------------------------
// THE discriminating check
// ---------------------------------------------------------------------------

/// A decided-and-held connect (cell seen at 0), a second connect that lands
/// in that window (cell 1), the first released (cell must be 2).
///
/// Asserted in the order the interleaving happens, each fact read from disk:
/// the child's row is decided with no part landed and the cell untouched;
/// the parent's connect COMMITS while the child is still holding (the
/// carve-out); the cell reads 1; the child is released and exits 0; the cell
/// reads 2 and the author has two `wrote` edges through the door.
// RED: (the whole point of the task) `bump_cell` made a PLAIN-WRITE counter —
// `exec` phase 10 captures each connect endpoint's cell value from the
// statement's own snapshot, and the landing writes `captured + 1` as an
// ABSOLUTE value instead of `value + 1`. The final assertion read 1 where 2
// is required: the child's landing overwrote the parent's increment with the
// value its snapshot had seen. Deterministic across runs; under the same
// mutation the OLD four-connector test in `lock_behaviour.rs` flipped 5 of 6
// runs (cell 3, 3, 3, 3, 2, and once 4) — racily, because whether two
// snapshot captures overlap a landing there is a scheduling accident. Also
// red by: the pause's wait loop deleted in `fault.rs` — the child never
// held, and "exactly one held transaction" found none.
#[test]
fn a_held_connect_cannot_erase_a_concurrent_connect_the_count_cell_is_commutative() {
    let _env = kill::env_shared();
    let dir = kill::scratch("commutativity");
    let c = fresh_cauldron(&dir, short_ceiling());
    let author = create_author(&c, "counted under contention");
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let t0 = Instant::now();

    // 1 + 2: the child matches its snapshot (cell = 0), passes its decided
    // flip, and is HELD there — a live transaction in the real pipeline.
    let mut held = kill::pause_at(
        "decided",
        "connect",
        &dir,
        "commutativity",
        &[
            ("SPINE_VICTIM_AUTHOR", author.0.as_str()),
            ("SPINE_VICTIM_DOC_TITLE", "connected by the victim"),
        ],
    );
    let t_reached = t0.elapsed();
    let row = the_one_pending_row(&spine);
    assert!(row.decided, "the child is past its decided flip");
    assert!(!row.cleared);
    assert!(
        row.parts.iter().all(|p| !p.landed),
        "and it has landed nothing: its snapshot value of the cell is still what it holds"
    );
    assert!(
        kill::lock_rows(&dir, row.seq) > 0,
        "it holds its granted locks while it is held"
    );
    assert_eq!(cell(&dir, &author), None, "the cell is untouched: nobody has landed");

    // 3: the parent's connect to the SAME author, through the ordinary door,
    // must land while the child is held. This is the connect carve-out as a
    // theorem: the held transaction holds no WRITE on the author, so a
    // second connector is not behind it. The cauldron's short lock ceiling
    // turns "it waited" into a loud `Busy` rather than a slow green.
    let t_parent_start = t0.elapsed();
    let parent_doc = connect(&c, &author, "connected by the parent");
    let t_parent_done = t0.elapsed();
    assert!(
        held.is_holding(),
        "the child must STILL be held after the parent's commit — the parent did not wait \
         for it, and it did not finish behind the parent's back"
    );
    assert_eq!(
        cell(&dir, &author),
        Some(1),
        "the parent's landing derived one increment inside its own landing transaction"
    );
    assert_eq!(wrote_docs(&c, &author), vec![parent_doc.clone()]);

    // 4: release the child. It lands with a snapshot that saw the cell at 0.
    let out = held.release_and_expect_exit_zero();
    let t_released = t0.elapsed();
    let child_doc = kill::field(&out, "doc").expect("the victim names the doc it minted");
    assert_ne!(child_doc, parent_doc.0, "two connects, two docs");

    // THE ASSERTION THIS SUITE EXISTS FOR. A commutative increment inside the
    // child's landing transaction reads the parent's 1 and writes 2. A
    // plain-write counter computed from the child's snapshot writes 0 + 1 =
    // 1 and erases the parent's increment.
    assert_eq!(
        cell(&dir, &author),
        Some(2),
        "the count cell is a COMMUTATIVE INCREMENT: the held transaction's \
         landing must not overwrite an increment that landed inside its window"
    );
    let mut docs = wrote_docs(&c, &author);
    docs.sort();
    let mut want = vec![parent_doc, NodeId(child_doc)];
    want.sort();
    assert_eq!(docs, want, "and both `wrote` edges are there, through the door");
    assert!(spine.pending().expect("pending").is_empty());
    assert!(c.in_landing(None).expect("in landing").is_empty());

    // The measured interleaving, for the report (visible with --nocapture).
    println!(
        "interleaving: child reached `decided` at {t_reached:?}; parent connect {t_parent_start:?} \
         -> {t_parent_done:?} (committed while the child was held); child released at \
         {t_released:?}; cell 0 -> 1 (parent) -> 2 (child)"
    );
}

/// The same property at N = 2 HELD transactions plus the parent: two children
/// both decided with a snapshot of 0, the parent lands (1), the children are
/// released one after the other (2, then 3). A plain-write counter computed
/// from a snapshot would leave 1 whichever child landed last; an increment
/// that read the cell inside its own landing but only on the second child
/// would leave 2. Only the commutative increment reaches 3.
// RED: the same plain-write mutation as above — the cell read 1 where 2 was
// required after the first release; the pause's wait loop deleted — "two
// held transactions" found none.
#[test]
fn two_held_connects_and_a_live_one_all_count() {
    let _env = kill::env_shared();
    let dir = kill::scratch("commutativity-n2");
    let c = fresh_cauldron(&dir, short_ceiling());
    let author = create_author(&c, "counted thrice");
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");

    let first = kill::pause_at(
        "decided",
        "connect",
        &dir,
        "commutativity-n2-first",
        &[
            ("SPINE_VICTIM_AUTHOR", author.0.as_str()),
            ("SPINE_VICTIM_DOC_TITLE", "held first"),
        ],
    );
    let second = kill::pause_at(
        "decided",
        "connect",
        &dir,
        "commutativity-n2-second",
        &[
            ("SPINE_VICTIM_AUTHOR", author.0.as_str()),
            ("SPINE_VICTIM_DOC_TITLE", "held second"),
        ],
    );
    // Two decided, un-landed rows; the cell untouched; both sets of locks
    // granted simultaneously (the carve-out, at N = 2 real processes).
    let pending = spine.pending().expect("pending");
    assert_eq!(pending.len(), 2, "two held transactions: {pending:?}");
    assert!(pending.iter().all(|r| r.decided && !r.cleared));
    assert!(pending
        .iter()
        .all(|r| r.parts.iter().all(|p| !p.landed)));
    for r in &pending {
        assert!(kill::lock_rows(&dir, r.seq) > 0);
    }
    assert_eq!(cell(&dir, &author), None);

    connect(&c, &author, "landed live");
    assert_eq!(cell(&dir, &author), Some(1));

    first.release_and_expect_exit_zero();
    assert_eq!(cell(&dir, &author), Some(2), "held first, landed second: 1 + 1");
    second.release_and_expect_exit_zero();
    assert_eq!(
        cell(&dir, &author),
        Some(3),
        "held with a snapshot of 0, landed last: the cell must still read every increment"
    );
    assert_eq!(wrote_docs(&c, &author).len(), 3);
    assert!(spine.pending().expect("pending").is_empty());
}

/// The carve-out, stated negatively, so the positive half above is known to
/// discriminate: against the SAME held connect, a statement that needs a
/// WRITE on the author's body (a rename) does wait, and under the short
/// ceiling that wait is a loud `Busy` naming the holder. Without this test,
/// "the parent's connect completed while the child was held" could be true
/// of a build whose lock table granted everything to everyone.
// RED: the pause's wait loop deleted in `fault.rs` — the child never held,
// "exactly one held transaction" found none, and the rename would have
// committed straight through. (The mutation `COMPATIBILITY`'s
// write row all-true is the lock-table poison for this shape and reds it
// the other way; it is not re-run here.)
#[test]
fn a_held_write_on_the_author_does_make_a_connector_wait_so_the_carve_out_above_is_real() {
    let _env = kill::env_shared();
    let dir = kill::scratch("commutativity-negative");
    let c = fresh_cauldron(&dir, short_ceiling());
    let author = create_author(&c, "renamed while held");
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");

    // Built the other way round from the doc comment's phrasing, on purpose:
    // the child is held at `decided` on a plain connect (it holds a READ on
    // the author's body), and the PARENT submits a statement that needs a
    // WRITE on that body — a rename. A write behind a held read must wait.
    // Same lock table, same theorem, opposite direction, and it needs no
    // extra victim job.
    let held = kill::pause_at(
        "decided",
        "connect",
        &dir,
        "commutativity-negative",
        &[("SPINE_VICTIM_AUTHOR", author.0.as_str())],
    );
    let row = the_one_pending_row(&spine);
    let err = c
        .submit(commit(
            r#"MATCH (a: fixture-alpha/author { id: $a }) SET a.name = $n"#,
            &[
                ("a", serde_json::json!(author.0)),
                ("n", serde_json::json!("never lands while held")),
            ],
        ))
        .expect_err("a WRITE on the author must wait behind the held transaction's read");
    match err {
        spine::error::SpineError::Busy { message } => assert!(
            message.contains(&format!("seq {}", row.seq)),
            "the wait names the holder it is behind: {message}"
        ),
        other => panic!("expected a loud Busy at the ceiling, got {other:?}"),
    }
    held.release_and_expect_exit_zero();
    // And once released, the same rename commits.
    c.submit(commit(
        r#"MATCH (a: fixture-alpha/author { id: $a }) SET a.name = $n"#,
        &[
            ("a", serde_json::json!(author.0)),
            ("n", serde_json::json!("renamed after release")),
        ],
    ))
    .expect("the rename lands once the read is released");
    assert_eq!(cell(&dir, &author), Some(1));
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// A ceiling short enough that "the parent waited behind the child" is a
/// loud failure inside a second, long enough that an uncontended grant on a
/// loaded machine is never mistaken for a wait.
fn short_ceiling() -> SpineConfig {
    SpineConfig {
        lock_wait_ceiling: Duration::from_millis(1500),
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
                thread: "commutativity".into(),
                dispatch: "commutativity".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn minted(c: &Cauldron, statement: &str, params: &[(&str, serde_json::Value)], key: &str) -> NodeId {
    match c.submit(commit(statement, params)).expect("mint") {
        Outcome::Committed { returned, .. } => {
            NodeId(returned.0[0][key].as_str().expect("id").to_string())
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn create_author(c: &Cauldron, name: &str) -> NodeId {
    minted(
        c,
        r#"CREATE (a: fixture-alpha/author { name: $n }) RETURN a.id"#,
        &[("n", serde_json::json!(name))],
        "a.id",
    )
}

fn connect(c: &Cauldron, author: &NodeId, title: &str) -> NodeId {
    minted(
        c,
        CONNECT,
        &[
            ("a", serde_json::json!(author.0)),
            ("t", serde_json::json!(title)),
        ],
        "d.id",
    )
}

/// The cell, read from the pool file directly: count cells are machine-owned
/// and not projectable through RETURN, so this is the one way
/// to read one, and it is the same way `lock_behaviour.rs` reads it.
fn cell(dir: &Path, author: &NodeId) -> Option<i64> {
    Pool::open(&Pool::path_in(dir, &PoolName::new(ALPHA)))
        .expect("alpha")
        .snapshot()
        .expect("snapshot")
        .count_cell(author, CELL)
        .expect("cell")
}

/// The docs the author `wrote`, through the door.
fn wrote_docs(c: &Cauldron, author: &NodeId) -> Vec<NodeId> {
    let mut b = Bindings::new();
    b.insert("a".into(), serde_json::json!(author.0));
    c.read(&ReadRequest {
        statement: r#"MATCH (a: fixture-alpha/author { id: $a }) -[wrote]-> (d: fixture-alpha/doc) RETURN d.id"#
            .to_string(),
        bindings: b,
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    })
    .expect("read")
    .rows
    .0
    .iter()
    .map(|r| NodeId(r["d.id"].as_str().expect("id").to_string()))
    .collect()
}

fn the_one_pending_row(spine: &SpineDb) -> spine::spine_db::TxRow {
    let mut pending = spine.pending().expect("pending");
    assert_eq!(pending.len(), 1, "exactly one held transaction: {pending:?}");
    pending.remove(0)
}
