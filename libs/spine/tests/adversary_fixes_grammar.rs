//! ADVERSARY, second pass — lens: WHAT THE FIXES INTRODUCED. The GRAMMAR
//! half: the new compile-side
//! dotted-SET walk (`check_settable`), the new witnesses, and the loud
//! invariant net under phase 10b (`REFUSED_AFTER_DECIDED`).
//!
//! THE SEAM UNDER ATTACK. The door judges a dotted SET twice: at
//! compile, against the MATCHED RECORD's current value (before any row), and
//! at the landing, against the value the pool holds AFTER EVERY EARLIER DELTA
//! OF THE SAME PART has been applied. Those are two different states whenever
//! one statement carries two SETs on one field — which the ladle admits (the
//! parser pushes every `SET` clause). Where the two judges disagree in the
//! landing's favour the door refuses a statement that would have landed;
//! where they disagree in the compiler's favour the landing refuses AFTER the
//! decided flip, which is the loud crate-invariant error —
//! and the row is left decided, un-landed, with its locks held. The rule:
//! a statement that fails anywhere lands nothing anywhere and returns one
//! refusal.
//!
//! Red tests are marked `#[ignore = "ADVERSARY RED (fixes): ..."]`; run with
//! `--ignored`. Nothing under `src/` was touched.

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::schema_def::fixtures;
use spine::spine_db::{SpineDb, SPINE_FILE};
use spine::witness::Witness;
use spine::{Cauldron, Outcome, ReadRequest};
use std::path::{Path, PathBuf};

test_scratch::install!();

/// Two SETs on one field, the second addressing THROUGH the value the first
/// just wrote. The compiler walks the matched record (`title` is `null` there,
/// so both paths pass); the landing applies the deltas in order (`title.a`
/// becomes `1`, then `title.a.b` must pass through `1`) and refuses
/// `not_an_object` — after the decided flip. Whatever the door answers, it
/// must be one of the two whole answers: Committed, or Refused with nothing
/// decided. A `Hard` naming a crate invariant, with the row decided and
/// un-landed, is neither.
#[test]
fn two_sets_on_one_field_never_refuse_after_the_decided_flip() {
    let dir = root("dotted-order-wedge");
    let c = fresh_cauldron(&dir);
    let id = create_doc(&c, "hello");
    // Make `title` null first (a legal SET value, and the value the compile
    // walk treats as "the way is clear").
    c.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = null"#,
        &[("id", serde_json::json!(id))],
    ))
    .expect("SET title = null lands");

    let env = commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title.a = 1 SET d.title.a.b = 2"#,
        &[("id", serde_json::json!(id))],
    );
    let hash = env.hash_identity().expect("hash");
    let out = c.submit(env);
    let row = spine_of(&dir).find_by_hash(&hash).expect("lookup");
    assert_whole(&out, row.as_ref(), &dir);
}

/// The same two-SET shape on a node this statement MINTS: a created binding
/// has no matched record, so the compile walk has nothing to walk and passes;
/// the landing creates `title = "x"` and then refuses `title.sub` through a
/// string — after the decided flip.
#[test]
fn a_dotted_set_on_a_minted_node_never_refuses_after_the_decided_flip() {
    let dir = root("dotted-minted-wedge");
    let c = fresh_cauldron(&dir);
    let env = commit(
        r#"CREATE (d: fixture-alpha/doc { title: "x", state: "draft" }) SET d.title.sub = 1 RETURN d.id"#,
        &[],
    );
    let hash = env.hash_identity().expect("hash");
    let out = c.submit(env);
    let row = spine_of(&dir).find_by_hash(&hash).expect("lookup");
    assert_whole(&out, row.as_ref(), &dir);
}

/// The other direction of the same disagreement. `SET d.title = null` then
/// `SET d.title.lang = "en"` is EXACTLY the pair the
/// rewrite of `tests/storage.rs` asserts LANDS at the pool ("over `null` the
/// way is clear: the landing mints the object"). Through the door the same
/// pair is refused `not_an_object`, because the compile walk reads `title`
/// as the string it holds BEFORE the statement, not the null the first SET
/// leaves. One validator at two doors must reach one verdict.
#[test]
fn a_set_to_null_followed_by_a_nested_set_lands_as_the_landing_says_it_does() {
    let dir = root("dotted-null-then-nested");
    let c = fresh_cauldron(&dir);
    let id = create_doc(&c, "hello");
    let out = c.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = null SET d.title.lang = "en""#,
        &[("id", serde_json::json!(id))],
    ));
    match &out {
        Ok(Outcome::Committed { .. }) => {}
        other => panic!(
            "the pair the landing accepts must land through the door too; got {}",
            describe(other)
        ),
    }
    let title = read_title(&c, &id);
    assert_eq!(title, serde_json::json!({ "lang": "en" }));
}

/// Validate mode is "the same validator, two callers", and the dry run
/// judges pins and occupancy so it "accepts
/// exactly what the commit accepts". For the two-SET statement above the dry
/// run answers `Validated` and the commit wedges. A dry run must not accept
/// what the landing refuses.
#[test]
fn validate_mode_does_not_accept_a_statement_the_landing_refuses() {
    let dir = root("dotted-order-validate");
    let c = fresh_cauldron(&dir);
    let id = create_doc(&c, "hello");
    c.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = null"#,
        &[("id", serde_json::json!(id))],
    ))
    .expect("SET title = null lands");
    let mut env = commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title.a = 1 SET d.title.a.b = 2"#,
        &[("id", serde_json::json!(id))],
    );
    env.mode = Mode::Validate;
    let dry = c.submit(env);
    // The landing's verdict, taken directly against the same pool state.
    let landing = {
        use spine::delta::{CompiledPart, CompiledPayload, Delta};
        use spine::envelope::TxHash;
        use spine::pool::{NodeId, Pool};
        let mut pool = Pool::open(&Pool::path_in(&dir, &spine::schema_def::PoolName::new("fixture-alpha")))
            .expect("open alpha");
        let part = CompiledPart {
            pool: spine::schema_def::PoolName::new("fixture-alpha"),
            schema_version: fixtures::fixture_alpha().version,
            deltas: vec![
                Delta::SetField {
                    node: NodeId(id.clone()),
                    path: vec!["title".into(), "a".into()],
                    value: serde_json::json!(1),
                },
                Delta::SetField {
                    node: NodeId(id.clone()),
                    path: vec!["title".into(), "a".into(), "b".into()],
                    value: serde_json::json!(2),
                },
            ],
            premises: vec![],
            far_versions: Default::default(),
        };
        let _ = CompiledPayload {
            parts: vec![],
            minted: Default::default(),
        };
        pool.apply_part(&TxHash([0x77; 32]), 0, &part)
    };
    let landing_refuses = matches!(&landing, Err(SpineError::Refused { .. }));
    assert!(
        landing_refuses,
        "the landing refuses this part (the premise of the test): {landing:?}"
    );
    assert!(
        !matches!(dry, Ok(Outcome::Validated { .. })),
        "the dry run accepted a statement the landing refuses: dry run answered {}",
        describe(&dry)
    );
}

/// `AbsentField` exists so a failed expectation never says "found `null`"
/// about a field the node does not carry. `state` on a type
/// that carries NO state vocabulary (`fixture-alpha/author`) is the same
/// fact — the node has no state — and `EXPECT a.state = ...` on it must be
/// refused with the absent witness, not with `found: null`.
#[test]
fn an_expectation_on_a_stateless_types_state_is_refused_as_absent_not_as_null() {
    let dir = root("stateless-expect");
    let c = fresh_cauldron(&dir);
    let id = match c.submit(commit(
        r#"CREATE (a: fixture-alpha/author { name: "ann" }) RETURN a.id"#,
        &[],
    )) {
        Ok(Outcome::Committed { returned, .. }) => returned.0[0]["a.id"]
            .as_str()
            .expect("id")
            .to_string(),
        other => panic!("seed: {other:?}"),
    };
    let err = c
        .submit(commit(
            r#"MATCH (a: fixture-alpha/author { id: $id }) EXPECT a.state = "x" SET a.name = "bob""#,
            &[("id", serde_json::json!(id))],
        ))
        .expect_err("an expectation on a state the node cannot have refuses");
    let witnesses = match &err {
        SpineError::Refused { witnesses } => witnesses.clone(),
        other => panic!("expected a structured refusal, got {other:?}"),
    };
    assert!(
        witnesses.iter().any(|w| matches!(w, Witness::AbsentField { .. })),
        "a stateless node's state is ABSENT; the witness said {witnesses:?}"
    );
}

// ---------------------------------------------------------------------------
// plumbing
// ---------------------------------------------------------------------------

/// Whole means: Committed, or Refused with no row decided. Anything else —
/// and in particular the crate-invariant `Hard` with a decided, un-cleared
/// row behind it — fails here with everything a fix needs.
fn assert_whole(out: &Result<Outcome, SpineError>, row: Option<&spine::spine_db::TxRow>, dir: &Path) {
    let wedged = row.map(|r| r.decided && !r.cleared).unwrap_or(false);
    let locks: i64 = rusqlite::Connection::open(dir.join(SPINE_FILE))
        .expect("spine")
        .query_row("SELECT COUNT(*) FROM locks", [], |r| r.get(0))
        .expect("count locks");
    match out {
        Ok(Outcome::Committed { .. }) => {}
        Err(SpineError::Refused { witnesses }) => {
            assert!(
                !wedged,
                "refused ({witnesses:?}) but the row is decided and un-cleared: {row:?}"
            );
        }
        other => panic!(
            "neither Committed nor a whole refusal: {}; row decided-and-un-cleared: {wedged}; \
             lock rows in the spine: {locks}; row: {row:?}",
            describe(other)
        ),
    }
    assert!(!wedged, "the row is decided and un-cleared: {row:?}");
}

fn describe(r: &Result<Outcome, SpineError>) -> String {
    match r {
        Ok(Outcome::Committed { .. }) => "Ok(Committed)".into(),
        Ok(Outcome::Validated { .. }) => "Ok(Validated)".into(),
        Err(e) => format!("Err({}: {})", e.kind(), e.message()),
    }
}

fn root(name: &str) -> PathBuf {
    let dir = test_scratch::tmp_path(format!("spine-adversary-fixes-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn fresh_cauldron(dir: &Path) -> Cauldron {
    Cauldron::create(
        dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        SpineConfig::default(),
    )
    .expect("create the cauldron")
}

fn spine_of(dir: &Path) -> SpineDb {
    SpineDb::open(&dir.join(SPINE_FILE)).expect("open the spine")
}

fn bindings(params: &[(&str, serde_json::Value)]) -> Bindings {
    let mut b = Bindings::new();
    for (k, v) in params {
        b.insert((*k).to_string(), v.clone());
    }
    b
}

fn commit(statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
    Envelope {
        statement: statement.to_string(),
        bindings: bindings(params),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent {
                thread: "adversary-fixes".into(),
                dispatch: "grammar".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn create_doc(c: &Cauldron, title: &str) -> String {
    match c.submit(commit(
        r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" }) RETURN d.id"#,
        &[("t", serde_json::json!(title))],
    )) {
        Ok(Outcome::Committed { returned, .. }) => returned.0[0]["d.id"]
            .as_str()
            .expect("id")
            .to_string(),
        other => panic!("seed: {other:?}"),
    }
}

fn read_title(c: &Cauldron, id: &str) -> serde_json::Value {
    let out = c
        .read(&ReadRequest {
            statement: r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.title"#.into(),
            bindings: bindings(&[("id", serde_json::json!(id))]),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect("read");
    out.rows.0[0]["d.title"].clone()
}
