//! THE RECOVER-AT-OPEN SEAM'S FAIL-CLOSED HALF:
//! `Cauldron::open` runs recovery before it hands back a handle, and a
//! recovery that cannot complete REFUSES THE OPEN — a named `Hard`, no
//! handle, so no `submit`, `quiesce` or `resume_all` is reachable — and the
//! open does not FINISH the stranded row on its way out: it stays decided,
//! un-cleared and locked. (Its parts do land first, by design — the landing
//! is the commit; only the projection, the reply, refuses.)
//!
//! The failure is staged without a fault seam: a decided, un-cleared row
//! whose statement text does not parse. Its payload is REAL: one
//! alpha part setting `x`'s title. Recovery rolls that part forward — the
//! landing is the commit and does not depend on the text (recover.rs,
//! `project_recovered`) — and then cannot project its RETURN, which
//! `recover.rs` refuses by name rather than fabricate. Because the part
//! lands, the refusal is told apart from an open that found nothing to
//! recover: as first written the payload was empty, so "the pool is
//! unchanged" held whether recovery ran or not. THE TWIN is
//! the same row with a statement that parses: the same open then succeeds
//! and clears it — so an `open` that refused everything would fail here, not
//! pass.

use spine::config::SpineConfig;
use spine::delta::{CompiledPart, CompiledPayload, Delta};
use spine::error::SpineError;
use spine::locks::{self, Claim, ClaimSet, LockMode, Resource};
use spine::pool::NodeId;
use spine::schema_def::{fixtures, PoolName};
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use spine::{Cauldron, Outcome};
use std::path::{Path, PathBuf};

test_scratch::install!();

const ALPHA: &str = "fixture-alpha";
const UNPARSEABLE: &str = "this record was altered after its row was decided";
const PARSEABLE: &str = r#"CREATE (d: fixture-alpha/doc { title: "staged", state: "draft" })"#;
/// The title the staged row's one part sets — what roll-forward lands.
const ROLLED: &str = "rolled forward by recovery";

/// A decided row whose record recovery cannot project: `open` refuses with a
/// named Hard that carries the row's seq, returns no handle, and leaves the
/// row decided, un-cleared, un-recovered, its lock held. Its one part HAS
/// landed (exactly what recover.rs documents: parts land, then the
/// projection refuses) — the proof that this refusal came from a recovery
/// that ran over real work.
// RED (each seen): (a) `Cauldron::open` proceeding past a failed
// recovery (`recover::recover(&cauldron).unwrap_or_default()`) — the open
// came back Ok with a handle ("handed back a handle"); (b) the error mapped
// to an anonymous `hard("open failed")` — the seq-naming assertion; (c) the
// refused open releasing every pending row's locks on its way out — "the
// row keeps its lock". (Also seen:) recovery skipping the roll-forward
// (`pool.apply_part` cut from `recover::recover`) — "the part landed before
// the refusal"; the empty-payload version stayed ok under that cut.
#[test]
fn open_refuses_with_a_named_hard_and_no_handle_when_recovery_fails() {
    let dir = root("refuses");
    let live = fresh_cauldron(&dir);
    let x = create_doc(&live, "before the altered row");
    drop(live);
    let (seq, hash) = stage_decided_row(&dir, UNPARSEABLE, &x);
    // Sentinel: the staged part carries work — the title it sets is not the
    // title the pool holds now. BOTH operands are produced by the code:
    // the title the part sets is read back from the spine's
    // own stored payload, the title the pool holds from the pool. (The line
    // here was `assert_ne!(ROLLED, "before the altered row")`, two literals.)
    // RED (seen): `stage_decided_row`'s delta setting the title the
    // doc already has — the literal-vs-literal form stayed ok under it.
    assert_eq!(title_of(&dir, &x).as_deref(), Some("before the altered row"));
    let staged_title = staged_title_of(&dir, seq);
    assert_ne!(
        staged_title.as_deref(),
        title_of(&dir, &x).as_deref(),
        "the staged part must set a title the pool does not already hold, or its landing is invisible"
    );

    let err = match Cauldron::open(&dir, SpineConfig::default()) {
        Ok(_) => panic!("an open whose recovery failed handed back a handle"),
        Err(e) => e,
    };
    let message = match &err {
        SpineError::Hard { message } => message.clone(),
        other => panic!("a failed recovery refuses the open as Hard, got {other:?}"),
    };
    assert!(
        message.contains(&format!("recovery cannot project RETURN for seq {}", seq.0)),
        "names the row recovery could not finish: {message}"
    );
    assert!(message.contains(&hash.to_string()), "names its transaction: {message}");
    assert!(message.contains("fail-closed"), "names the posture: {message}");

    // THE ROW is not finished on the refused open: decided, un-cleared,
    // un-recovered, its lock held. (An earlier version also asserted the
    // spine's row count unchanged; no open path appends a row, so that could
    // not fail, and it was removed.)
    let after = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine").row_at(seq).expect("row");
    assert!(after.decided && !after.cleared && !after.recovered, "{after:?}");
    assert_eq!(lock_rows(&dir, seq), 1, "the row keeps its lock");
    // THE PART LANDED, before the refusal: recovery ran over real work.
    assert!(after.parts.iter().all(|p| p.landed) && after.parts.len() == 1, "{after:?}");
    assert_eq!(
        title_of(&dir, &x).as_deref(),
        Some(ROLLED),
        "the part landed before the refusal (recovery ran, it did not find nothing to do)"
    );

    // And it refuses again — every later pass refuses the same way.
    assert!(matches!(
        Cauldron::open(&dir, SpineConfig::default()),
        Err(SpineError::Hard { .. })
    ));
}

/// THE TWIN: the same staged row with a statement that parses. The same
/// open succeeds, clears it, releases its lock, and the handle writes.
// RED (seen): `Cauldron::open` refusing unconditionally after
// recovery (`Err(SpineError::hard(..))` in place of `Ok(cauldron)`) — the
// `expect` below panicked; the refusal test above stayed green under it.
#[test]
fn open_succeeds_and_clears_the_same_row_when_its_record_projects() {
    let dir = root("twin");
    let live = fresh_cauldron(&dir);
    let x = create_doc(&live, "before the intact row");
    drop(live);
    let (seq, hash) = stage_decided_row(&dir, PARSEABLE, &x);

    let c = Cauldron::open(&dir, SpineConfig::default()).expect("an intact record recovers");
    assert_eq!(c.recovered_at_open().rolled_forward, vec![hash], "{:?}", c.recovered_at_open());
    assert_eq!(title_of(&dir, &x).as_deref(), Some(ROLLED));
    let after = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine").row_at(seq).expect("row");
    assert!(after.decided && after.cleared && after.recovered, "{after:?}");
    assert_eq!(lock_rows(&dir, seq), 0);
    assert!(matches!(set_title(&c, &x, "lands"), Ok(Outcome::Committed { .. })));
}

/// The refusal is about the RECORD, not the directory: repair the altered
/// statement out-of-band and the very cauldron that refused opens. The part
/// landed on the refused pass, so the repaired open only clears (it lands
/// nothing twice).
// RED (seen): forcing (a) above — "refused while altered"; and the
// unconditional refusal — "opens after the repair".
#[test]
fn the_same_cauldron_opens_once_its_record_is_repaired() {
    let dir = root("repaired");
    let live = fresh_cauldron(&dir);
    let x = create_doc(&live, "repair me");
    drop(live);
    let (seq, hash) = stage_decided_row(&dir, UNPARSEABLE, &x);
    assert!(Cauldron::open(&dir, SpineConfig::default()).is_err(), "refused while altered");

    rusqlite::Connection::open(dir.join(SPINE_FILE))
        .expect("spine")
        .execute(
            "UPDATE transactions SET statement = ?1 WHERE seq = ?2",
            rusqlite::params![PARSEABLE, seq.0],
        )
        .expect("repair the record");
    let c = Cauldron::open(&dir, SpineConfig::default()).expect("opens after the repair");
    assert_eq!(c.recovered_at_open().cleared_only, vec![hash], "{:?}", c.recovered_at_open());
    assert!(c.recovered_at_open().rolled_forward.is_empty());
    assert_eq!(title_of(&dir, &x).as_deref(), Some(ROLLED));
}

// ===========================================================================
// helpers
// ===========================================================================

fn root(name: &str) -> PathBuf {
    let dir = test_scratch::tmp_path(format!("spine-seams-open-refuses-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn fresh_cauldron(dir: &Path) -> Cauldron {
    Cauldron::create(dir, &[fixtures::fixture_alpha(), fixtures::fixture_beta()], SpineConfig::default())
        .expect("create the cauldron")
}

fn envelope(statement: &str, params: &[(&str, serde_json::Value)]) -> spine::envelope::Envelope {
    use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
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
                dispatch: "seams-open-refuses".into(),
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

/// The title the staged row's payload sets, read back from the spine's own
/// record of that row (not from [`ROLLED`]): exactly one `title` SetField.
fn staged_title_of(dir: &Path, seq: Seq) -> Option<String> {
    let row = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine").row_at(seq).expect("row");
    let sets: Vec<&serde_json::Value> = row
        .payload
        .parts
        .iter()
        .flat_map(|p| p.deltas.iter())
        .filter_map(|d| match d {
            Delta::SetField { path, value, .. } if path.len() == 1 && path[0] == "title" => {
                Some(value)
            }
            _ => None,
        })
        .collect();
    assert_eq!(sets.len(), 1, "the staged payload sets the title exactly once: {:?}", row.payload);
    sets[0].as_str().map(str::to_string)
}

fn lock_rows(dir: &Path, seq: Seq) -> usize {
    rusqlite::Connection::open(dir.join(SPINE_FILE))
        .expect("spine")
        .query_row("SELECT COUNT(*) FROM locks WHERE seq = ?1", [seq.0], |r| r.get::<_, i64>(0))
        .expect("count") as usize
}

/// Stage a DECIDED, un-cleared row by hand with the given statement text,
/// a REAL payload — one alpha part setting `x`'s title to [`ROLLED`] — and
/// an exclusive lock on `x`'s body. Appended through the spine's own append
/// (part rows, touched set), then decided; the statement is the variable.
fn stage_decided_row(dir: &Path, statement: &str, x: &NodeId) -> (Seq, spine::envelope::TxHash) {
    let env = envelope(statement, &[]);
    let hash = env.hash_identity().expect("hash");
    let payload = CompiledPayload {
        parts: vec![CompiledPart {
            pool: PoolName::new(ALPHA),
            schema_version: fixtures::fixture_alpha().version,
            deltas: vec![Delta::SetField {
                node: x.clone(),
                path: vec!["title".into()],
                value: serde_json::json!(ROLLED),
            }],
            premises: vec![],
            far_versions: Default::default(),
        }],
        minted: Default::default(),
    };
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let seq = spine.append_pending(&env, &hash, &payload).expect("append the staged row");
    spine.mark_decided(seq).expect("decide");
    let mut claims = ClaimSet::new();
    claims.insert(Claim {
        pool: PoolName::new(ALPHA),
        resource: Resource::Body { node: x.clone() },
        mode: LockMode::Write,
    });
    locks::acquire(&spine, seq, &claims, &SpineConfig::default()).expect("an uncontended grant");
    (seq, hash)
}
