//! ADVERSARY — lens: GRAMMAR, REFUSAL AND THE ONE DOOR (the "grammar"
//! half). This file attacks the write statement's clauses, the forms
//! that must refuse BY NAME, the refusal witnesses that could lie about WHY,
//! the two stamps, and the distinction between a query that
//! matches nothing (fine) and code that runs and does nothing (must not
//! report success).
//!
//! THE INVERTED INCENTIVE: every test here asserts something that should
//! OBVIOUSLY be true of the door. None is ignored: each runs, and passes,
//! in the default suite.
//!
//! Every test names, in its doc comment, what SHOULD hold, what the design
//! says, and — for the red ones — what actually happens and the best reading
//! of why.

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::pool::{NodeId, Pool, QualifiedNodeId};
use spine::schema_def::{fixtures, grammar_fault_tag, PoolName};
use spine::spine_db::{SpineDb, SPINE_FILE};
use spine::witness::Witness;
use spine::{Cauldron, Outcome, ReadRequest};
use std::path::{Path, PathBuf};

test_scratch::install!();

const ALPHA: &str = "fixture-alpha";
const BETA: &str = "fixture-beta";

// ===========================================================================
// REFUSAL WITNESSES THAT LIE ABOUT WHY
// ===========================================================================

/// SHOULD HOLD: a binding the caller SUPPLIED but the text never references
/// is refused — both directions refuse — and the witness must
/// say WHY. A parameter that is present and unused is not "missing".
///
/// ACTUAL: `compile::check_params` refuses the surplus binding with
/// `Witness::MissingParameter { name: "extra" }`. The one witness shape that
/// exists for parameter faults is named for the opposite fault, so a suite
/// (or an agent) asserting on it cannot tell "you forgot to bind `extra`"
/// from "you bound `extra` and nothing uses it" — the repair is opposite in
/// each case. A "backwards-payload risk" was recorded on this
/// witness and left; this test is that risk exhibited at both doors.
#[test]
fn a_surplus_binding_is_not_refused_as_missing_at_either_door() {
    let dir = root("surplus");
    let c = fresh_cauldron(&dir);
    let doc = create_doc(&c, "surplus");

    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.state = "published""#,
            &[
                ("id", serde_json::json!(doc.0)),
                ("extra", serde_json::json!(1)),
            ],
        ))
        .expect_err("a surplus binding refuses");
    let w = only_witness(&err);
    assert!(
        !matches!(&w, Witness::MissingParameter { name } if name == "extra"),
        "write door: `extra` was SUPPLIED and UNUSED, yet the witness says it is MISSING: {w:?}"
    );

    let err = c
        .read(&ReadRequest {
            statement: r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.state"#.into(),
            bindings: bindings(&[
                ("id", serde_json::json!(doc.0)),
                ("extra", serde_json::json!(1)),
            ]),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect_err("a surplus binding refuses at the read door too");
    let w = only_witness(&err);
    assert!(
        !matches!(&w, Witness::MissingParameter { name } if name == "extra"),
        "read door: `extra` was SUPPLIED and UNUSED, yet the witness says it is MISSING: {w:?}"
    );
}

/// SHOULD HOLD: a failed value expectation carries the value FOUND.
/// JSON `null` is a legal field value in this build (a SET may write
/// it), so "the field holds null" and "the field does not exist" are two
/// different found-states and the witness must not collapse them.
///
/// ACTUAL: `compile::read_path` answers `Value::Null` for an absent field,
/// and `emit_expect_value` reports it as `found: null` — byte-identical to
/// the witness for a field that genuinely holds `null`. An earlier fix closed
/// exactly this class for `EXPECT REV(b.state)` ("a witness that lies about
/// WHY"); the value form still carries it.
#[test]
fn a_failed_value_expectation_distinguishes_an_absent_field_from_a_null_one() {
    let dir = root("absent-null");
    let c = fresh_cauldron(&dir);
    // `capacity` is declared but optional: created without it, it is ABSENT.
    let shelf = committed_id(
        &c,
        commit(
            r#"CREATE (s: fixture-beta/shelf { label: "a" }) RETURN s.id"#,
            &[],
        ),
        "s.id",
    );
    let absent = c
        .submit(commit(
            r#"MATCH (s: fixture-beta/shelf { id: $id }) EXPECT s.capacity = 5 SET s.label = "b""#,
            &[("id", serde_json::json!(shelf.0))],
        ))
        .expect_err("an expectation over an absent field cannot hold");
    let absent_w = only_witness(&absent);

    // Now the field EXISTS and holds JSON null.
    c.submit(commit(
        r#"MATCH (s: fixture-beta/shelf { id: $id }) SET s.capacity = null"#,
        &[("id", serde_json::json!(shelf.0))],
    ))
    .expect("SET a field to null");
    let held_null = c
        .submit(commit(
            r#"MATCH (s: fixture-beta/shelf { id: $id }) EXPECT s.capacity = 5 SET s.label = "c""#,
            &[("id", serde_json::json!(shelf.0))],
        ))
        .expect_err("an expectation over a null field cannot hold either");
    let null_w = only_witness(&held_null);

    assert!(
        matches!(&null_w, Witness::FailedExpectation { found, .. } if found.is_null()),
        "a field holding null is found as null: {null_w:?}"
    );
    assert_ne!(
        absent_w, null_w,
        "the witness for an ABSENT field must not be byte-identical to the witness for a field \
         that HOLDS null; both read `found: null`, so a caller cannot tell which repair applies"
    );
}

/// SHOULD HOLD: `rev` is a machine-owned name (`MACHINE_OWNED_FIELDS`) and
/// every node HAS a whole-node revision. Asking for it by the field path —
/// `RETURN d.rev` — must either project the revision or refuse by name; it
/// must never project `null`, which reads as "no such field". An earlier fix
/// closed this exact hole for `REV(b.state)` and for `b.schema_version`
/// (explicit ask projects the stamp); `rev` is the third machine-owned name
/// with a value, and it still answers null.
///
/// ACTUAL: `read_path` special-cases `id`, `state`, `schema_version` and
/// falls through to the fields map for everything else, so `d.rev` is
/// `Value::Null` in RETURN, and `EXPECT d.rev = 1` refuses with
/// `found: null` — a lying witness of the class already treated as a defect.
#[test]
fn the_machine_owned_rev_asked_by_field_path_is_never_a_silent_null() {
    let dir = root("rev-path");
    let c = fresh_cauldron(&dir);
    let doc = create_doc(&c, "rev");

    let out = c.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.rev, REV(d) AS whole"#,
        &[("id", serde_json::json!(doc.0))],
    ));
    match out {
        Ok(Outcome::Committed { returned, .. }) => {
            let row = &returned.0[0];
            assert_eq!(row["whole"], serde_json::json!(1), "REV(d) is the revision");
            assert!(
                !row["d.rev"].is_null(),
                "RETURN d.rev projected null for a node whose revision is 1 — indistinguishable \
                 from `no such field`: {row:?}"
            );
        }
        Err(SpineError::Refused { witnesses }) => {
            // Refusing BY NAME is the other acceptable answer.
            assert!(
                witnesses
                    .iter()
                    .any(|w| matches!(w, Witness::MalformedStatement { .. })),
                "if it refuses, it refuses by name: {witnesses:?}"
            );
        }
        other => panic!("unexpected: {other:?}"),
    }

    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { id: $id }) EXPECT d.rev = 1 SET d.state = "published""#,
            &[("id", serde_json::json!(doc.0))],
        ));
    match err {
        Ok(_) => {} // the expectation held against the real revision: fine
        Err(SpineError::Refused { witnesses }) => {
            for w in &witnesses {
                if let Witness::FailedExpectation { found, .. } = w {
                    assert!(
                        !found.is_null(),
                        "EXPECT d.rev = 1 refused with found: null — the node's revision IS 1; \
                         the witness lies about what was found: {w:?}"
                    );
                }
            }
        }
        Err(other) => panic!("unexpected: {other:?}"),
    }
}

// ===========================================================================
// FORMS THAT MUST REFUSE BY NAME RATHER THAN BE SILENTLY MISREAD
// ===========================================================================

/// SHOULD HOLD: a dotted SET addresses INTO a declared field's JSON value
/// (the build's recorded reading of the SET clause). Addressing into a field that
/// holds a SCALAR is not a sub-key write; it can only be satisfied by
/// destroying the scalar. That must refuse — a silent replacement of
/// `"hello"` by `{"sub": 1}` is exactly the forbidden "silent misread",
/// and it is data loss with a `Committed` reply.
///
/// ACTUAL: `pool::set_at_path` replaces any non-object with a fresh object
/// and the statement commits; the title is gone.
#[test]
fn a_dotted_set_into_a_scalar_field_refuses_rather_than_destroying_it() {
    let dir = root("dotted-scalar");
    let c = fresh_cauldron(&dir);
    let doc = create_doc(&c, "hello");

    let out = c.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title.sub = 1"#,
        &[("id", serde_json::json!(doc.0))],
    ));
    let title = field_of(&dir, ALPHA, &doc, "title");
    assert!(
        out.is_err(),
        "a dotted SET into a scalar must refuse; it committed and the title is now {title:?}"
    );
    assert_eq!(
        title,
        Some(serde_json::json!("hello")),
        "nothing lands after a refusal"
    );
}

/// SHOULD HOLD: a MATCH field map may name `state` (match by
/// pool + type + state). A state OUTSIDE the type's closed vocabulary, or a
/// state on a type that carries NO vocabulary, is not a query that happens
/// to match nothing — it is a query the grammar can prove can never match,
/// and the same fact refuses BY NAME on the write side (`unknown_state`,
/// `state_not_declared`). The door already refuses a MATCH on an undeclared
/// node TYPE by name (`resolve_grammars`); the state half is the same
/// grammar fact and must be judged the same way, or the closed vocabulary is
/// closed for writers and open for readers.
///
/// ACTUAL: both heads bind nothing and the statement commits under the
/// empty-match rule — a `Committed` reply for a statement the grammar could
/// have refused before it ran, with a spine row appended for it.
#[test]
fn a_match_on_a_state_the_grammar_cannot_admit_refuses_by_name() {
    let dir = root("state-vocab");
    let c = fresh_cauldron(&dir);
    let _doc = create_doc(&c, "vocab");

    // (1) outside a CLOSED vocabulary — `doc` admits draft/published/superseded.
    let out = c.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { state: "nonsense" }) SET d.title = "x""#,
        &[],
    ));
    match out {
        Err(SpineError::Refused { witnesses }) => {
            assert!(
                witnesses.iter().any(|w| grammar_fault_tag(w) == Some("unknown_state")),
                "refused, but not by the grammar's own name: {witnesses:?}"
            );
        }
        other => panic!(
            "a MATCH on `nonsense` (outside doc's closed vocabulary) should refuse `unknown_state` \
             as SET does; got {other:?}"
        ),
    }

    // (2) a state on a type with NO vocabulary — `author` carries none.
    let out = c.submit(commit(
        r#"MATCH (a: fixture-alpha/author { state: "draft" }) SET a.name = "x""#,
        &[],
    ));
    match out {
        Err(SpineError::Refused { witnesses }) => {
            assert!(
                witnesses
                    .iter()
                    .any(|w| grammar_fault_tag(w) == Some("state_not_declared")),
                "refused, but not by the grammar's own name: {witnesses:?}"
            );
        }
        other => panic!(
            "a MATCH addressing `state` on a type with no vocabulary should refuse \
             `state_not_declared` as SET does; got {other:?}"
        ),
    }
}

// ===========================================================================
// VALIDATE MODE — "one validator, two callers"
// ===========================================================================

/// SHOULD HOLD: the dry run is THE SAME statement through THE SAME validator
/// with nothing at stake (validate-then-commit rests on
/// it: "the commit never meets a refusable item"). A statement the commit
/// path will refuse must be refused by the dry run.
///
/// ACTUAL: `exec` returns `Outcome::Validated` at phase 7, BEFORE phase 10b's
/// occupancy check. An at-most-one violation (the scalar slice, the
/// fixture's `current_of`) passes validation and refuses at commit. The
/// migration driver's manifest would mark such an edge `Imported` and the
/// landing would then refuse the WHOLE attempt — the split validate-then-commit
/// exists to prevent. Occupancy can be read from a snapshot in validate mode exactly
/// as phase 10b reads it under locks; the dry run simply does not.
#[test]
fn validate_mode_judges_at_most_one_occupancy_like_the_commit_does() {
    let dir = root("validate-occupancy");
    let c = fresh_cauldron(&dir);
    let author = create_author(&c, "ann");
    let d1 = create_doc(&c, "one");
    let d2 = create_doc(&c, "two");
    c.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { id: $d }) MATCH (a: fixture-alpha/author { id: $a })
           CREATE (d) -[current_of]-> (a)"#,
        &[("d", serde_json::json!(d1.0)), ("a", serde_json::json!(author.0))],
    ))
    .expect("the first current_of lands");

    let poison = r#"MATCH (d: fixture-alpha/doc { id: $d }) MATCH (a: fixture-alpha/author { id: $a })
                    CREATE (d) -[current_of]-> (a)"#;
    let params = [("d", serde_json::json!(d2.0)), ("a", serde_json::json!(author.0))];

    // The commit refuses it (occupancy: at most one current_of per author).
    let err = c
        .submit(commit(poison, &params))
        .expect_err("a second current_of toward the same author refuses at commit");
    assert_eq!(
        grammar_fault_tag(&only_witness(&err)),
        Some("at_most_one_violated"),
        "the commit refuses by the occupancy name"
    );

    // The dry run must say the same thing.
    let dry = c.submit(validate(poison, &params));
    match dry {
        Err(SpineError::Refused { witnesses }) => assert!(
            witnesses
                .iter()
                .any(|w| grammar_fault_tag(w) == Some("at_most_one_violated")),
            "the dry run refused, but not for occupancy: {witnesses:?}"
        ),
        other => panic!(
            "Validate mode accepted a statement the commit refuses (`at_most_one_violated`): \
             one validator, two callers is false for occupancy; got {other:?}"
        ),
    }
}

/// SHOULD HOLD: `Outcome::Validated` is what a `Mode::Validate` envelope
/// answers with; `Outcome::Committed` means "one commit, one spine row"
/// (lib.rs's own doc). A validate-mode envelope must never be answered
/// `Committed`.
///
/// ACTUAL: `mode` is outside the hash identity (by design), and phase 4's
/// dedup runs BEFORE the mode is consulted, so a validate-mode re-submission
/// of an envelope that already landed answers `Committed { deduplicated:
/// true }`. The migration driver defends itself against exactly this reply
/// (`migrate::validate` turns it into a Hard error), which is evidence the
/// door's contract is being worked around rather than kept.
#[test]
fn a_validate_mode_envelope_is_never_answered_committed() {
    let dir = root("validate-dedup");
    let c = fresh_cauldron(&dir);
    let text = r#"CREATE (d: fixture-alpha/doc { title: "once", state: "draft" }) RETURN d.id"#;
    c.submit(commit(text, &[])).expect("the commit lands");

    let out = c.submit(validate(text, &[])).expect("a dry run of a landed ask is not an error");
    assert!(
        matches!(out, Outcome::Validated { .. }),
        "Mode::Validate answered {out:?}; a dry run must answer Validated (or refuse), never \
         Committed — the caller asked for nothing to land and is being told something did"
    );
}

// ===========================================================================
// A QUERY THAT MATCHES NOTHING versus CODE THAT DOES NOTHING
// ===========================================================================

/// SHOULD HOLD: a query that has no matches is fine; code that does
/// nothing is not. A statement with NO
/// mutation clause and NO premise is a READ (the same grammar with no
/// mutation clauses is executed as a READ TRANSACTION). Submitted
/// to the WRITE door in commit mode it does not match nothing — it matches
/// fine — and then the door runs and DOES NOTHING: no delta, no part, no
/// pool touched. Answering `Committed` is a success report for code that
/// did nothing, and it appends a permanent spine row that is decided and
/// cleared with `committed_at` NULL and zero parts — a "commit" the
/// committed-commit counter never counted.
///
/// ACTUAL: `Committed { deduplicated: false }`, row decided + cleared,
/// `committed_at: None`, `parts: []`.
#[test]
fn the_write_door_does_not_report_committed_for_a_statement_with_nothing_to_write() {
    let dir = root("read-through-write");
    let c = fresh_cauldron(&dir);
    let doc = create_doc(&c, "read me");
    let counter_before = c.committed_commit_counter().expect("counter");

    let env = commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.title"#,
        &[("id", serde_json::json!(doc.0))],
    );
    let hash = env.hash_identity().expect("hash");
    let out = c.submit(env);

    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let row = spine.find_by_hash(&hash).expect("lookup");
    assert!(
        !matches!(&out, Ok(Outcome::Committed { .. })),
        "the write door answered Committed for a statement that wrote nothing; the row it \
         appended is {:?} (decided={:?}, cleared={:?}, committed_at={:?}, parts={:?}) — a \
         decided commit the counter never counted (counter before {counter_before}, after {})",
        out,
        row.as_ref().map(|r| r.decided),
        row.as_ref().map(|r| r.cleared),
        row.as_ref().map(|r| r.committed_at),
        row.as_ref().map(|r| r.parts.len()),
        c.committed_commit_counter().expect("counter")
    );
}

// The REMOVE-of-an-absent-edge test that stood here under `#[ignore]` as a
// specification question was decided: edges are a set in both directions. It is now the HELD test
// `removing_an_absent_edge_is_a_set_no_op_that_commits`, beside the
// CREATE-of-an-existing-edge test it mirrors.

// ===========================================================================
// WHAT HELD — attacked, did not break (kept as evidence of aim)
// ===========================================================================

/// HELD: a non-equality WHERE is a PREMISE, not a filter. The compiler's
/// docs say only the equality form is RECORDED as a premise and that other
/// shapes are "enforced by the matcher" — which would be the silent
/// narrowing that is forbidden — but `check_condition` evaluates every operator
/// universally before recording, so a bulk SET with one violating row
/// refuses whole. The doc comment is wrong; the behaviour is right.
#[test]
fn a_non_equality_where_refuses_the_whole_bulk_statement() {
    let dir = root("where-ne");
    let c = fresh_cauldron(&dir);
    let _a = create_doc(&c, "keep");
    let _b = create_doc(&c, "keep");
    let x = create_doc(&c, "x");
    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc) WHERE d.title != "x" SET d.state = "published""#,
            &[],
        ))
        .expect_err("one of three rows falsifies the premise; the statement refuses whole");
    match only_witness(&err) {
        Witness::FailedCondition { binding, .. } => assert_eq!(binding, "d"),
        other => panic!("expected FailedCondition, got {other:?}"),
    }
    assert_eq!(state_of(&dir, ALPHA, &x).as_deref(), Some("draft"), "nothing landed");
}

/// HELD: the schema-version stamp is machine-owned end to end — neither a
/// CREATE field map nor a SET may name it, and the refusal carries the
/// frozen `machine_owned_field` tag (no pen sets it).
#[test]
fn no_pen_sets_the_schema_version_stamp_by_any_clause() {
    let dir = root("stamp-pen");
    let c = fresh_cauldron(&dir);
    let doc = create_doc(&c, "stamped");
    let err = c
        .submit(commit(
            r#"CREATE (d: fixture-alpha/doc { title: "t", state: "draft", schema_version: 7 })"#,
            &[],
        ))
        .expect_err("CREATE naming the stamp refuses");
    assert_eq!(grammar_fault_tag(&only_witness(&err)), Some("machine_owned_field"));
    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.schema_version = 7"#,
            &[("id", serde_json::json!(doc.0))],
        ))
        .expect_err("SET naming the stamp refuses");
    assert!(
        matches!(only_witness(&err), Witness::GrammarViolation { .. }),
        "a grammar refusal, structured"
    );
    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.created_tx = "x""#,
            &[("id", serde_json::json!(doc.0))],
        ))
        .expect_err("SET naming the provenance column refuses");
    assert!(matches!(only_witness(&err), Witness::GrammarViolation { .. }));
}

/// HELD: a CREATE-edge toward a node already linked is a set no-op that
/// commits — documented as set semantics in `pool.rs` and defensible (the
/// declared post-state is true). The REMOVE mirror follows it: edges
/// are a set in BOTH directions, so
/// the two tests read as one rule rather than as an asymmetry.
#[test]
fn re_creating_an_existing_edge_is_a_set_no_op_that_commits() {
    let dir = root("edge-set");
    let c = fresh_cauldron(&dir);
    let author = create_author(&c, "cy");
    let doc = create_doc(&c, "linked");
    let text = r#"MATCH (a: fixture-alpha/author { id: $a }) MATCH (d: fixture-alpha/doc { id: $d })
                  CREATE (a) -[wrote]-> (d)"#;
    let params = [("a", serde_json::json!(author.0)), ("d", serde_json::json!(doc.0))];
    c.submit(commit(text, &params)).expect("first link");
    // A different identity (nonce) so dedup does not answer for it.
    let mut again = commit(text, &params);
    again.idempotency_nonce = Some("again".into());
    c.submit(again).expect("re-linking commits as a set no-op");
    let pool = Pool::open(&Pool::path_in(&dir, &PoolName::new(ALPHA))).expect("alpha");
    let snap = pool.snapshot().expect("snapshot");
    assert_eq!(snap.edges_from(&author, Some("wrote")).expect("edges").len(), 1);
    assert_eq!(
        snap.count_cell(&author, "docs_written").expect("cell"),
        Some(1),
        "the count cell did not move for the no-op"
    );
}

/// HELD: `REMOVE (a) -[kind]-> (b)`
/// by bound triple where the edge does not exist is a SET NO-OP that commits,
/// exactly as the CREATE of an edge already present above — one rule,
/// idempotent in both directions. The declared post-state ("a does not
/// wrote d") is true; the row records what was ASKED, never a
/// claim that something was removed; and the count cell does not move,
/// because the set did not change. A caller who needs strictness matches
/// the edge and pins `EXPECT COUNT = 1`, which refuses expected-versus-found.
///
/// The adversary's original asserted the opposite and stood
/// under `#[ignore]` as a specification question until it was decided.
// RED: (a) restoring the refusal — `removed == 0` in `pool.rs`'s RemoveEdge
// arm made a grammar refusal — the submit was `Err(Refused)`; (b) bumping
// the count cell unconditionally (`self_bump_cells(.., -1)` whether or not a
// row was deleted) — `docs_written` read `Some(-1)` against `None` before.
#[test]
fn removing_an_absent_edge_is_a_set_no_op_that_commits() {
    let dir = root("remove-absent");
    let c = fresh_cauldron(&dir);
    let author = create_author(&c, "bob");
    let doc = create_doc(&c, "never linked");
    let pool_path = Pool::path_in(&dir, &PoolName::new(ALPHA));
    let cell_before = Pool::open(&pool_path)
        .expect("alpha")
        .snapshot()
        .expect("snapshot")
        .count_cell(&author, "docs_written")
        .expect("cell");
    let counter_before = c.committed_commit_counter().expect("counter");

    let env = commit(
        r#"MATCH (a: fixture-alpha/author { id: $a }) MATCH (d: fixture-alpha/doc { id: $d })
           REMOVE (a) -[wrote]-> (d)"#,
        &[("a", serde_json::json!(author.0)), ("d", serde_json::json!(doc.0))],
    );
    let hash = env.hash_identity().expect("hash");
    let out = c.submit(env).expect("REMOVE of an absent edge commits as a set no-op");
    assert!(
        matches!(&out, Outcome::Committed { deduplicated: false, .. }),
        "a fresh commit, not a replay: {out:?}"
    );

    // The record: one row, decided and cleared, carrying the ASK — a
    // `remove_edge` delta — beside what was done.
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let row = spine.find_by_hash(&hash).expect("lookup").expect("the row landed");
    assert!(row.decided && row.cleared, "decided and cleared: {row:?}");
    let deltas: usize = row.payload.parts.iter().map(|p| p.deltas.len()).sum();
    assert_eq!(deltas, 1, "the ask is recorded, once");
    assert_eq!(
        c.committed_commit_counter().expect("counter"),
        counter_before + 1,
        "a committed commit counts"
    );

    // The pool: the set is unchanged and the count cell did not move.
    let alpha = Pool::open(&pool_path).expect("alpha");
    let snap = alpha.snapshot().expect("snapshot");
    assert_eq!(snap.edges_from(&author, Some("wrote")).expect("edges").len(), 0);
    assert_eq!(
        snap.count_cell(&author, "docs_written").expect("cell"),
        cell_before,
        "the count cell did not move for the no-op (was {cell_before:?})"
    );
}

// ===========================================================================
// helpers
// ===========================================================================

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-adv-grammar-{name}-{}", std::process::id()))
}

fn fresh_cauldron(dir: &Path) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(
        dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        SpineConfig::default(),
    )
    .expect("create the cauldron")
}

fn bindings(params: &[(&str, serde_json::Value)]) -> Bindings {
    let mut b = Bindings::new();
    for (k, v) in params {
        b.insert((*k).to_string(), v.clone());
    }
    b
}

fn envelope(statement: &str, params: &[(&str, serde_json::Value)], mode: Mode) -> Envelope {
    Envelope {
        statement: statement.to_string(),
        bindings: bindings(params),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent {
                thread: "adversary-grammar".into(),
                dispatch: "adversary-grammar".into(),
            },
            instrument: Instrument::door(),
        },
        mode,
        idempotency_nonce: None,
    }
}

fn commit(statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
    envelope(statement, params, Mode::Commit)
}

fn validate(statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
    envelope(statement, params, Mode::Validate)
}

fn only_witness(err: &SpineError) -> Witness {
    match err {
        SpineError::Refused { witnesses } => {
            assert_eq!(witnesses.len(), 1, "one refusal, one witness: {witnesses:?}");
            witnesses[0].clone()
        }
        other => panic!("expected a structured refusal, got {other:?}"),
    }
}

fn committed_id(c: &Cauldron, env: Envelope, column: &str) -> NodeId {
    match c.submit(env).expect("commit") {
        Outcome::Committed { returned, .. } => {
            NodeId(returned.0[0][column].as_str().expect("id").to_string())
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn create_doc(c: &Cauldron, title: &str) -> NodeId {
    committed_id(
        c,
        commit(
            r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" }) RETURN d.id"#,
            &[("t", serde_json::json!(title))],
        ),
        "d.id",
    )
}

fn create_author(c: &Cauldron, name: &str) -> NodeId {
    committed_id(
        c,
        commit(
            r#"CREATE (a: fixture-alpha/author { name: $n }) RETURN a.id"#,
            &[("n", serde_json::json!(name))],
        ),
        "a.id",
    )
}

fn field_of(dir: &Path, pool: &str, id: &NodeId, field: &str) -> Option<serde_json::Value> {
    let pool = Pool::open(&Pool::path_in(dir, &PoolName::new(pool))).expect("pool");
    let snap = pool.snapshot().expect("snapshot");
    snap.node(id)
        .expect("read")
        .and_then(|n| n.fields.get(field).map(|f| f.value.clone()))
}

fn state_of(dir: &Path, pool: &str, id: &NodeId) -> Option<String> {
    let pool = Pool::open(&Pool::path_in(dir, &PoolName::new(pool))).expect("pool");
    let snap = pool.snapshot().expect("snapshot");
    snap.node(id).expect("read").and_then(|n| n.state)
}

#[allow(dead_code)]
fn qualified(pool: &str, id: &NodeId) -> QualifiedNodeId {
    QualifiedNodeId {
        pool: PoolName::new(pool),
        id: id.clone(),
    }
}

#[allow(dead_code)]
const _BETA: &str = BETA;

// Conceptual tests — stated, not executable in this build (one line each,
// given/when/then):
//
// - GIVEN a MATCH by identifier whose `id` names a node of a DIFFERENT type
//   than the head declares, WHEN submitted, THEN it binds nothing and commits
//   under the empty-match rule — a probable authoring error swallowed; the
//   rule as written covers it, so it is flagged, not asserted.
// - GIVEN two SET clauses on the same field in one statement, WHEN committed,
//   THEN the per-field revision advances by two for one observable change;
//   the SET "rewrite case" says nothing about revision arithmetic.
// - GIVEN an EXPECT over a ghost's lifecycle state (`EXPECT g.state =
//   "cleared"`), WHEN committed, THEN it is checked at compile time and
//   recorded nowhere (`push_premise` drops projected premises); this
//   is a known weak place and no decision exists.
// - GIVEN `Initiator::Agent` with a thread and dispatch of the caller's
//   choosing, WHEN submitted, THEN the row records whatever was claimed —
//   "harness-supplied, never self-reported" is unenforceable inside
//   the crate and is an obligation on the caller the crate cannot witness.
