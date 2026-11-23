//! **Refusal with witnesses.**
//!
//! For every specified witness shape, ONE POISONED FIXTURE that forces exactly
//! it, through the real door, asserted on STRUCTURE and never on prose — and
//! after every single refusal, the assertion that binds them all: **nothing
//! landed in ANY pool.**
//!
//! The shapes, and where each one is forced:
//!
//! | witness shape | poisoned fixture |
//! |---|---|
//! | failed expectation — expected versus found | a moved whole-node revision, a moved per-field revision, a state that is not what the author expected |
//! | failed condition — the condition and its binding | a WHERE that is false of one row of a bulk match |
//! | unimplemented staged form — the named form | each of the three staged-out forms, at the write door AND the read door |
//! | version mismatch — both versions | an envelope and a read request from another format version |
//! | exhausted restart — the contested claims | a match set moved under a queued statement with `restart_bound = 0` |
//! | per-pool grammar, poison one | a beta part that violates BETA's vocabulary; alpha keeps nothing |
//! | per-pool grammar, poison two | an ALPHA node type addressed into the beta pool, refused under BETA's grammar |
//!
//! **"Nothing landed" is asserted as a whole-cauldron FINGERPRINT**, not as a
//! spot check: every node of every declared type in both pools with its state,
//! revision and fields, every edge with its whole triple, every declared count
//! cell, the committed-commit counter, the in-landing set and the pending
//! worklist. A refusal that landed one field of one node in one pool would
//! move that fingerprint. See [`Fingerprint`].
//!
//! Assertions on the `GrammarViolation` witness go through
//! [`spine::schema_def::grammar_fault_tag`], which reads the FROZEN machine
//! tag back off the message — that is the structural handle on a witness whose
//! frozen shape has a prose field and no code field.
//!
//! Every checker here was forced RED at least once; the poison is named in a
//! `// RED:` comment above each test.

use spine::config::SpineConfig;
use spine::delta::{CompiledPart, CompiledPayload, Delta};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance, TxHash};
use spine::error::SpineError;
use spine::ladle::compile::{PROJECTED_PATTERN_FORM, STAGED_OUT_FORMS};
use spine::locks::{self, Claim, ClaimSet, LockMode, Resource};
use spine::pool::{NodeId, Pool};
use spine::schema_def::{fixtures, grammar_fault_tag, PoolGrammar, PoolName};
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use spine::witness::{ExpectationKind, Witness};
use spine::{Cauldron, Outcome, ReadRequest};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

test_scratch::install!();

const ALPHA: &str = "fixture-alpha";
const BETA: &str = "fixture-beta";

// ---------------------------------------------------------------------------
// the failed EXPECTATION: the expectation, the expected, the found
// ---------------------------------------------------------------------------

/// A failed expectation returns the expectation, the expected value, and
/// the found value.
///
/// Three poisoned fixtures, one per EXPECT form that can fail this way: the
/// whole-node revision, the PER-FIELD revision (the same bookkeeping the store
/// keeps per field, and a distinct `ExpectationKind`), and the value form. Each
/// names its binding and, for the per-field form, its field — so a witness that
/// collapsed the two revision forms into one would be caught.
// RED: (a) `emit_expect_revision`'s `found != Some(want)` arm disabled — all
// three stale statements committed over moved nodes; (b)
// `ExpectationKind::Revision`'s `field` hard-coded to `None` — the per-field
// fixture's witness stopped naming `title`.
#[test]
fn every_failed_expectation_returns_the_expectation_the_expected_and_the_found() {
    let dir = root("expectation");
    let c = fresh_cauldron(&dir, SpineConfig::default());
    let doc = create_doc(&c, "the original");
    // Somebody else moves it: whole-node revision 1 becomes 2, and `title`'s
    // own per-field revision moves with it.
    c.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t"#,
        &[
            ("id", serde_json::json!(doc.0)),
            ("t", serde_json::json!("edited by somebody else")),
        ],
    ))
    .expect("the interfering edit commits");

    let before = Fingerprint::of(&c, &dir);

    // (1) the WHOLE-NODE revision form.
    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { id: $id })
               EXPECT REV(d) = 1
               SET d.state = "published""#,
            &[("id", serde_json::json!(doc.0))],
        ))
        .expect_err("a moved revision refuses");
    match only_witness(&err) {
        Witness::FailedExpectation {
            expectation,
            expected,
            found,
        } => {
            assert_eq!(
                expectation,
                ExpectationKind::Revision {
                    binding: "d".into(),
                    field: None
                }
            );
            assert_eq!(expected, serde_json::json!(1));
            assert_eq!(found, serde_json::json!(2));
        }
        other => panic!("expected a revision FailedExpectation, got {other:?}"),
    }
    before.assert_nothing_landed(&c, &dir, "a moved whole-node revision");

    // (2) the PER-FIELD revision form — a different `ExpectationKind`.
    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { id: $id })
               EXPECT REV(d.title) = 1
               SET d.state = "published""#,
            &[("id", serde_json::json!(doc.0))],
        ))
        .expect_err("a moved per-field revision refuses");
    match only_witness(&err) {
        Witness::FailedExpectation {
            expectation,
            expected,
            found,
        } => {
            assert_eq!(
                expectation,
                ExpectationKind::Revision {
                    binding: "d".into(),
                    field: Some("title".into())
                },
                "the per-field form names its FIELD, and is not the whole-node form"
            );
            assert_eq!(expected, serde_json::json!(1));
            assert_eq!(found, serde_json::json!(2));
        }
        other => panic!("expected a per-field FailedExpectation, got {other:?}"),
    }
    before.assert_nothing_landed(&c, &dir, "a moved per-field revision");

    // (3) the VALUE form — expected versus found on a field path.
    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { id: $id })
               EXPECT d.state = "published"
               SET d.title = $t"#,
            &[
                ("id", serde_json::json!(doc.0)),
                ("t", serde_json::json!("never lands")),
            ],
        ))
        .expect_err("a value expectation that does not hold refuses");
    match only_witness(&err) {
        Witness::FailedExpectation {
            expectation,
            expected,
            found,
        } => {
            assert_eq!(
                expectation,
                ExpectationKind::Value {
                    binding: "d".into(),
                    path: vec!["state".into()]
                }
            );
            assert_eq!(expected, serde_json::json!("published"));
            assert_eq!(found, serde_json::json!("draft"), "the value FOUND, not a flag");
        }
        other => panic!("expected a value FailedExpectation, got {other:?}"),
    }
    before.assert_nothing_landed(&c, &dir, "a failed value expectation");
}

// ---------------------------------------------------------------------------
// the failed CONDITION: the condition, and the binding it failed on
// ---------------------------------------------------------------------------

/// A failed condition returns the failing condition and the binding it
/// failed on; a premise that fails at commit time refuses the statement with
/// the failing condition as witness.
///
/// This shape is the sharpest seam in the build: WHERE is a PREMISE, not a filter, so if the matcher
/// pre-filtered rows this witness would be unreachable dead code and a bulk
/// SET would silently narrow itself. The fixture is therefore deliberately a
/// BULK match where exactly ONE of three rows falsifies the condition: under
/// the premise reading the whole statement refuses; under a filtering reading
/// it would commit and quietly touch two of three.
// RED: (a) `emit_where_equal` reverted to recording the condition's DECLARED
// value as a premise without checking it — the false WHERE compiled clean and
// committed; (b) `run_match`'s `match_head` made to filter rows on WHERE — the
// statement committed and set two of the three docs, which is the silent
// narrowing this witness exists to prevent.
#[test]
fn a_failed_condition_names_the_condition_and_the_binding_and_never_narrows_a_bulk_set() {
    let dir = root("condition");
    let c = fresh_cauldron(&dir, SpineConfig::default());
    for i in 0..3 {
        create_doc(&c, &format!("uniform title {i}"));
    }
    // One row, and only one, is made to falsify the condition below.
    let odd = create_doc(&c, "the odd one out");
    let before = Fingerprint::of(&c, &dir);

    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { state: "draft" })
               WHERE d.state = "draft" AND d.title != "the odd one out"
               SET d.state = "published""#,
            &[],
        ))
        .expect_err("a premise false of ONE bound row refuses the WHOLE statement");
    match only_witness(&err) {
        Witness::FailedCondition { condition, binding } => {
            assert_eq!(
                binding, "d",
                "the witness names the binding whose row falsified it"
            );
            assert!(
                condition.contains("d.title") && condition.contains("the odd one out"),
                "and the condition it descended to, in canonical source form: {condition}"
            );
            assert!(
                !condition.contains("AND"),
                "a compound condition descends to the narrowest FALSE conjunct, so the witness \
                 names what actually failed rather than the whole predicate: {condition}"
            );
        }
        other => panic!("expected a FailedCondition, got {other:?}"),
    }
    before.assert_nothing_landed(&c, &dir, "a failed WHERE premise");
    assert_eq!(
        state_of(&c, &odd.0),
        Some("draft".into()),
        "and no row was quietly narrowed away and left behind"
    );
}

// ---------------------------------------------------------------------------
// the UNIMPLEMENTED staged form: the named form
// ---------------------------------------------------------------------------

/// An unimplemented clause form returns the named form — a coverage gap
/// REFUSES; a dialect misreads.
///
/// One fixture per staged-out form, at BOTH doors: the write door and the bulk
/// read door speak one grammar, so a form staged out of one is staged out of
/// the other, and a read path that quietly attempted a staged form would be
/// the dialect the grammar forbids. The form names are asserted against the coverage
/// table's own frozen constants, so a test and the table cannot drift apart.
// RED: (a) `COVERAGE_TABLE` emptied — the staged forms were attempted and
// misread instead of named; (b) `read::read_request`'s `check_coverage` call
// removed — the write door still refused and the read door did not.
#[test]
fn every_staged_out_form_refuses_by_name_at_both_doors() {
    let dir = root("staged-out");
    let c = fresh_cauldron(&dir, SpineConfig::default());
    create_doc(&c, "present");
    let before = Fingerprint::of(&c, &dir);

    let poisons = [
        // A pattern match into the projected namespace: the row
        // is FIRST in the table because the namespace is the more specific
        // fact when a statement breaks two rules.
        (r#"MATCH (g: spine/txn) RETURN g.id"#, PROJECTED_PATTERN_FORM),
        (
            r#"MATCH (d: fixture-alpha/doc { title: "present" }) RETURN d.id"#,
            "match:field-value",
        ),
        (
            r#"MATCH (a: fixture-alpha/author) -[wrote]-> (d: fixture-alpha/doc)
               MATCH (d) -[supersedes]-> (e: fixture-alpha/doc) RETURN e.id"#,
            "match:multi-hop",
        ),
        (
            r#"MATCH (d: fixture-alpha/doc) WHERE EXISTS (d) -[supersedes]-> () RETURN d.id"#,
            "where:edge-existence",
        ),
    ];
    // The names asserted here are the table's own, so this suite cannot drift
    // from the coverage cut it is asserting about.
    let named: Vec<&str> = poisons.iter().map(|(_, f)| *f).collect();
    assert_eq!(
        STAGED_OUT_FORMS.to_vec(),
        named,
        "one fixture per staged-out form, and no form without a fixture"
    );

    for (statement, form) in poisons {
        // the WRITE door
        let err = c
            .submit(commit(statement, &[]))
            .expect_err("a staged-out form refuses at the write door");
        match only_witness(&err) {
            Witness::UnimplementedForm { form: got } => assert_eq!(got, form),
            other => panic!("expected UnimplementedForm({form}), got {other:?}"),
        }
        before.assert_nothing_landed(&c, &dir, form);

        // the READ door, same form, same name
        let err = c
            .read(&ReadRequest {
                statement: statement.to_string(),
                bindings: Bindings::new(),
                format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
            })
            .expect_err("and at the read door");
        match only_witness(&err) {
            Witness::UnimplementedForm { form: got } => assert_eq!(got, form),
            other => panic!("expected UnimplementedForm({form}) from the read door, got {other:?}"),
        }
        before.assert_nothing_landed(&c, &dir, form);
    }
}

// ---------------------------------------------------------------------------
// the VERSION MISMATCH: both versions
// ---------------------------------------------------------------------------

/// A version mismatch returns both versions. Both doors, and — the part
/// that matters most — the write door refuses on the VERSION even when the statement could never
/// parse, so an old recording replayed against moved machinery is never
/// misread silently.
// RED: (a) `check_format_version` comparing with `>=` — a future version was
// accepted; (b) `Envelope::validate` re-ordered so the parse runs first — the
// unparseable fixture came back a syntax witness and the version was never
// spoken.
#[test]
fn a_version_mismatch_names_both_versions_at_both_doors_and_before_the_parse() {
    let dir = root("version");
    let c = fresh_cauldron(&dir, SpineConfig::default());
    create_doc(&c, "present");
    let before = Fingerprint::of(&c, &dir);
    let supported = spine::envelope::ENVELOPE_FORMAT_VERSION;

    for (label, statement) in [
        ("a good statement", r#"MATCH (d: fixture-alpha/doc) RETURN d.id"#),
        ("one that could never parse", "!!! not ladle at all !!!"),
    ] {
        let mut env = commit(statement, &[]);
        env.format_version = 99;
        match only_witness(&c.submit(env).expect_err("refuses")) {
            Witness::VersionMismatch {
                envelope_version,
                supported_version,
            } => {
                assert_eq!(envelope_version, 99, "{label}");
                assert_eq!(supported_version, supported, "{label}");
            }
            other => panic!("{label}: expected VersionMismatch, got {other:?}"),
        }
        before.assert_nothing_landed(&c, &dir, label);
    }

    match only_witness(
        &c.read(&ReadRequest {
            statement: r#"MATCH (d: fixture-alpha/doc) RETURN d.id"#.into(),
            bindings: Bindings::new(),
            format_version: 7,
        })
        .expect_err("the read door speaks the same version"),
    ) {
        Witness::VersionMismatch {
            envelope_version,
            supported_version,
        } => {
            assert_eq!(envelope_version, 7);
            assert_eq!(supported_version, supported);
        }
        other => panic!("expected VersionMismatch from the read door, got {other:?}"),
    }
    before.assert_nothing_landed(&c, &dir, "a read at another version");
}

// ---------------------------------------------------------------------------
// the EXHAUSTED RESTART: the contested claims
// ---------------------------------------------------------------------------

/// A stale match (restart bound exhausted) returns the contested claims.
/// The bounded restart loop is what reaches it, and it has exactly
/// one home.
///
/// The fixture: a bulk statement is held at its grant by a foreign lock while
/// its match set is moved underneath it, with `restart_bound = 0` so the first
/// restart is also the last. The witness's `contested` is a list of real
/// `Claim` values — pool-qualified, resource-typed, moded — and it names the
/// body of the row that left the match, which is what "contested" means here.
// RED: (a) `restart_bound` ignored, so the loop restarted forever — the test
// hit its own bound and reported a hang rather than a witness; (b) the phase-10
// payload-equality test removed — the moved match was never noticed and the
// statement committed a payload its row no longer described; (c)
// `contested_claims` returning an empty vector — a witness that names nothing
// is not a witness, and the non-empty assertion fails.
#[test]
fn an_exhausted_restart_refuses_with_the_contested_claims() {
    let dir = root("stale-match");
    let c = fresh_cauldron(
        &dir,
        SpineConfig {
            // Escalate on the first attempt and refuse on the first restart:
            // the bound is what this fixture is about, and zero makes the
            // refusal deterministic rather than a race with the mover.
            restart_bound: 0,
            escalation_threshold: 0,
            lock_wait_ceiling: Duration::from_secs(30),
            lock_poll_interval: Duration::from_millis(3),
            ..SpineConfig::default()
        },
    );
    let moved = create_doc(&c, "the row that leaves the match");
    let parked = create_doc(&c, "the row the holder pins");
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");

    // A foreign transaction pins one matched body, so the bulk statement
    // enrols, queues, and cannot proceed to its re-match.
    let holder = park(&dir, &one(body(&parked), LockMode::Write), "stale holder");

    let env = commit(
        r#"MATCH (d: fixture-alpha/doc { state: "draft" })
           SET d.state = "published"
           RETURN d.id"#,
        &[],
    );
    let hash = env.hash_identity().expect("hash");

    let (err, before) = std::thread::scope(|scope| {
        let door = &c;
        let statement = scope.spawn(move || door.submit(env));
        await_new_claimant(&spine, &body(&parked), &[holder]);

        // MOVE THE MATCH SET, beneath the lock table rather than through it:
        // the door's queued claims would block any door-driven mover, so the
        // poison lands the state change directly on the pool file. What the
        // statement compiled is now not what the world says.
        poison_state(&dir, &moved, "published");
        let before = Fingerprint::of(door, &dir);

        unpark(&dir, holder);
        let err = statement
            .join()
            .expect("join")
            .expect_err("the restart bound is exhausted on the first restart");
        (err, before)
    });

    match only_witness(&err) {
        Witness::StaleMatch { contested } => {
            assert!(
                !contested.is_empty(),
                "a witness that names nothing is not a witness"
            );
            assert!(
                contested.iter().all(|cl| cl.pool == PoolName::new(ALPHA)),
                "every contested claim is pool-qualified: {contested:?}"
            );
            assert!(
                contested.iter().any(|cl| cl.resource == body(&moved)),
                "and the contest names the body that left the match: {contested:?}"
            );
        }
        other => panic!("expected a StaleMatch, got {other:?}"),
    }
    before.assert_nothing_landed(&c, &dir, "an exhausted restart");
    // The abandoned attempt is kept, aborted, and holds nothing.
    let attempts = spine.attempts_of(&hash).expect("history");
    assert!(!attempts.is_empty());
    for a in &attempts {
        assert!(
            !a.decided && a.cleared,
            "every attempt at a refused identity is recorded aborted and kept: {a:?}"
        );
        assert!(a.parts.iter().all(|p| !p.landed));
    }
}

// ---------------------------------------------------------------------------
// the two per-pool grammar poisons
// ---------------------------------------------------------------------------

/// Poison one: a poisoned variant whose beta part violates beta's grammar,
/// which must refuse the WHOLE statement with a witness naming the beta part
/// — nothing lands in alpha.
///
/// The value `draft` is ALPHA's `doc` vocabulary; beta's `ticket` has only
/// open and closed. One merged vocabulary would let this through, and the
/// alpha half of the same statement would land. The witness names the POOL and
/// the PART, and its frozen machine tag says which rule broke.
// RED: making `compile` validate every part against the FIRST pool's grammar
// (the merged-vocabulary bug, staged exactly) — beta's `draft` passed under
// alpha's `doc` vocabulary and the whole statement committed.
#[test]
fn a_beta_part_violating_betas_grammar_refuses_the_whole_statement_and_alpha_keeps_nothing() {
    let dir = root("poison-beta");
    let c = fresh_cauldron(&dir, SpineConfig::default());
    create_doc(&c, "alpha keeps nothing");
    let before = Fingerprint::of(&c, &dir);

    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { state: "draft" })
               CREATE (t: fixture-beta/ticket { state: "draft" })
               CREATE (t) -[catalogues]-> (d)
               SET d.state = "published""#,
            &[],
        ))
        .expect_err("a beta-part grammar violation refuses the whole statement");
    let w = only_witness(&err);
    match &w {
        Witness::GrammarViolation { pool, part, .. } => {
            assert_eq!(
                *pool,
                PoolName::new(BETA),
                "the witness names the pool whose OWN grammar was violated"
            );
            assert!(part.is_some(), "and the part inside the payload: {part:?}");
        }
        other => panic!("expected a GrammarViolation, got {other:?}"),
    }
    assert_eq!(
        grammar_fault_tag(&w),
        Some("unknown_state"),
        "the frozen machine tag, not the prose: {w:?}"
    );
    before.assert_nothing_landed(&c, &dir, "a beta grammar violation");
}

/// Poison two: a second poisoned variant that addresses an alpha node type
/// into the beta pool and must refuse under BETA's grammar — which is what
/// proves validation is genuinely per-pool rather than one merged
/// vocabulary.
///
/// `doc` is a perfectly good node type — in alpha. Beta has never heard of it.
/// The refusal must therefore name BETA and the `unknown_node_type` rule; a
/// refusal naming alpha would mean the door resolved the type in the wrong
/// grammar and merely noticed afterwards.
///
/// The per-pool-ness is proven by CONTRAST, which is stronger than any single
/// witness field: the SAME statement text, with only the pool qualifier
/// changed, commits. `doc` is not an unknown type — it is unknown TO BETA, and
/// the pool qualifier is what decides.
///
/// **A known gap:** this refusal path carries no frozen
/// `GRAMMAR_FAULTS` tag and no part ordinal, unlike the poison above, so
/// `grammar_fault_tag` answers `None` for it and the only structural handles
/// it offers are the variant and the pool. Nothing is asserted about the
/// untagged message here — a suite that pinned the prose would be asserting
/// exactly what witnesses-on-structure forbids.
// RED: (a) the undeclared-type refusal hard-coding alpha as the pool it names
// — the witness came back naming the pool that DID declare `doc`; (b)
// `resolve_grammars` made to accept a type declared by ANY pool (one merged
// vocabulary) — the beta spelling committed and the contrast collapsed.
#[test]
fn an_alpha_node_type_addressed_into_beta_refuses_under_betas_own_grammar() {
    let dir = root("poison-cross");
    let c = fresh_cauldron(&dir, SpineConfig::default());
    create_doc(&c, "alpha keeps nothing");
    let before = Fingerprint::of(&c, &dir);

    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { state: "draft" })
               CREATE (x: fixture-beta/doc { title: $t, state: "draft" })
               SET d.state = "published""#,
            &[("t", serde_json::json!("a doc in the wrong pool"))],
        ))
        .expect_err("an alpha type addressed into beta refuses under beta's grammar");
    match only_witness(&err) {
        Witness::GrammarViolation { pool, .. } => assert_eq!(
            pool,
            PoolName::new(BETA),
            "judged by the grammar of the pool it was ADDRESSED TO"
        ),
        other => panic!("expected a GrammarViolation, got {other:?}"),
    }
    before.assert_nothing_landed(&c, &dir, "an alpha type addressed into beta");

    // THE CONTRAST: one pool qualifier apart, and it commits. Validation is
    // genuinely per-pool; there is no merged vocabulary anywhere.
    c.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { state: "draft" })
           CREATE (x: fixture-alpha/doc { title: $t, state: "draft" })
           SET d.state = "published""#,
        &[("t", serde_json::json!("a doc in the wrong pool"))],
    ))
    .expect("the same text, addressed to the pool that declares `doc`, commits");
}

// ---------------------------------------------------------------------------
// the door's own refusal paths — nothing lands after those either
// ---------------------------------------------------------------------------

/// The binding clause is about EVERY refusal, not only the enumerated
/// shapes: a statement that fails anywhere lands nothing anywhere and returns
/// one refusal. The three door-level refusals that are the flagged
/// additions to the witness enum go through the same fingerprint here, so the
/// clause is proven over the whole refusal surface the door can reach and not
/// only over the six shapes above.
// RED: (a) `exec`'s phase-5 CREATE-head check removed TOGETHER WITH
// `compile`'s CREATE-head site (the two are duplicates with ONE
// witness — `CREATE (g)` — so removing either alone stays green) — the write
// came back `GrammarViolation` in pool `spine` (`unknown_state`: `draft` is
// not a `txn` state) instead of the pen's refusal; (b) `check_params`
// disabled in both directions — the unsupplied and the unreferenced parameter
// both stopped refusing.
#[test]
fn the_doors_other_refusals_are_structured_too_and_land_nothing() {
    let dir = root("other-refusals");
    let c = fresh_cauldron(&dir, SpineConfig::default());
    let doc = create_doc(&c, "present");
    let before = Fingerprint::of(&c, &dir);

    // The reserved projected namespace: origin is code.
    let err = c
        .submit(commit(
            r#"CREATE (g: spine/txn { title: $t, state: "draft" })"#,
            &[("t", serde_json::json!("forged provenance"))],
        ))
        .expect_err("the pen refuses the projected namespace");
    match only_witness(&err) {
        Witness::ProjectedNamespaceWrite { pool, .. } => {
            assert_eq!(pool, PoolName::new(spine::project::PROJECTED_POOL))
        }
        other => panic!("expected ProjectedNamespaceWrite, got {other:?}"),
    }
    before.assert_nothing_landed(&c, &dir, "a write into the projected namespace");

    // A referenced-but-unsupplied parameter, and a supplied-but-unreferenced
    // one: both directions refuse (frozen lexical rule), and each names THE
    // OFFENDING PARAMETER — the one the caller has to go and fix.
    //
    // The statement below references `$id` and `$state`; the "unsupplied"
    // fixture supplies NEITHER, and the witness must name the first
    // referenced-but-missing name (`id`, the sorted-first of the two) rather
    // than anything the caller did supply. The "unreferenced" fixture
    // supplies both referenced names PLUS `spare`, so the only name it can
    // honestly report is `spare`.
    // RED: the assertion used to be variant-only (`MissingParameter { .. }`),
    // so `check_params` reporting the WRONG name — the supplied one for the
    // unsupplied direction, or a referenced name for the unreferenced one,
    // which reads backwards — passed silently.
    // Swapping either arm's `name:` field in `check_params` now fires here.
    // Each direction has its OWN witness: a referenced-but-unsupplied name is `MissingParameter`
    // and a supplied-but-unreferenced one is `UnusedParameter`, because the
    // repairs are opposite and a witness that said "missing" about a name
    // the caller had supplied lied about WHY.
    for (label, params, want) in [
        (
            "unsupplied",
            vec![],
            Witness::MissingParameter { name: "id".into() },
        ),
        (
            "unreferenced",
            vec![
                ("id", serde_json::json!(doc.0)),
                ("state", serde_json::json!("published")),
                ("spare", serde_json::json!("never used")),
            ],
            Witness::UnusedParameter {
                name: "spare".into(),
            },
        ),
    ] {
        let err = c
            .submit(commit(
                r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.state = $state"#,
                &params,
            ))
            .expect_err("a parameter mismatch refuses");
        assert_eq!(
            only_witness(&err),
            want,
            "{label}: the witness names the OFFENDING parameter and the direction of the fault"
        );
        before.assert_nothing_landed(&c, &dir, label);
    }

    // And text that is not ladle at all.
    let err = c
        .submit(commit("MATCH RETURN SET", &[]))
        .expect_err("malformed text refuses");
    match only_witness(&err) {
        Witness::MalformedStatement { .. } => {}
        other => panic!("expected MalformedStatement, got {other:?}"),
    }
    before.assert_nothing_landed(&c, &dir, "a malformed statement");
}

/// The reserved `spine` namespace refuses a WRITE and a PATTERN MATCH by two
/// distinct names, and the difference is the point (the projection rule,
/// the ghost read, refuse-by-name):
///
/// 1. a CREATE head addressed INTO it is a WRITE — the pen refuses with
///    [`Witness::ProjectedNamespaceWrite`], citing the rule that the origin
///    of these nodes is code;
/// 2. a MATCH head in it by anything but IDENTIFIER — a population scan —
///    is `UnimplementedForm { form: "match:projected-pattern" }` at the
///    WRITE door, a coverage-table row;
/// 3. the same head at the READ door refuses by the SAME name: one grammar,
///    one matcher, one staged-out form.
///
/// What is NOT refused any more, and is proven in `tests/cross_pool.rs`: a
/// MATCH BY IDENTIFIER into the namespace, which resolves the ghost through
/// the one matcher at both doors (a ghost resolves transparently on read
/// and refuses on write). The two door gates
/// that used to stand here — `match:projected-namespace` and
/// `read:projected-namespace`, which refused EVERY touch — are retired.
// RED: (a) removing the `PROJECTED_PATTERN_FORM` row from `COVERAGE_TABLE`
// AND the matcher's own arm — arms 2 and 3 fell through to a ghost
// resolution of an absent `id` and failed as Hard, not by name; (b) making
// the detector ignore a head without `id` — arm 2's assertion fired on
// `MalformedStatement`; (c) `exec`'s created-head arm removed AND `compile`'s
// created-head site (`project::guard_write_target`, site 1) silenced — arm 1
// came back `GrammarViolation` in pool `spine` (`unknown_state`) instead of
// the pen's refusal. Either net ALONE keeps arm 1 green (forced both
// ways): they are duplicates refusing one fact with ONE witness text,
// `CREATE (g)` — never two nets with two witness texts.
// The SET-on-a-ghost and edge-from-a-ghost writes, which only `compile`'s
// net can catch, are proven pre-append in `tests/cross_pool.rs`.
#[test]
fn the_projected_namespace_refuses_a_write_and_a_pattern_match_by_two_distinct_names() {
    let dir = root("projected-namespace");
    let c = fresh_cauldron(&dir, SpineConfig::default());
    create_doc(&c, "present");
    let before = Fingerprint::of(&c, &dir);

    // ARM 1 — a CREATE head INTO the namespace: a write, and the pen refuses.
    let err = c
        .submit(commit(
            r#"CREATE (g: spine/txn { title: $t, state: "draft" })"#,
            &[("t", serde_json::json!("forged provenance"))],
        ))
        .expect_err("the pen refuses a write into the projected namespace");
    match only_witness(&err) {
        Witness::ProjectedNamespaceWrite { pool, target } => {
            assert_eq!(pool, PoolName::new(spine::project::PROJECTED_POOL));
            assert!(
                target.contains("CREATE"),
                "the witness names the offending head; got `{target}`"
            );
        }
        other => panic!("arm 1: expected ProjectedNamespaceWrite, got {other:?}"),
    }
    before.assert_nothing_landed(&c, &dir, "a CREATE into the projected namespace");

    // ARM 2 — a MATCH head in the namespace WITHOUT an identifier at the
    // WRITE door: a population scan of the projection, staged OUT by name.
    let err = c
        .submit(commit(
            r#"MATCH (g: spine/txn)
               MATCH (d: fixture-alpha/doc { state: "draft" })
               SET d.state = "published""#,
            &[],
        ))
        .expect_err("a pattern match into the projected namespace refuses at the write door");
    match only_witness(&err) {
        Witness::UnimplementedForm { form } => assert_eq!(
            form, PROJECTED_PATTERN_FORM,
            "a match-only touch is not a ProjectedNamespaceWrite"
        ),
        other => panic!("arm 2: expected UnimplementedForm, got {other:?}"),
    }
    before.assert_nothing_landed(&c, &dir, "a pattern MATCH into the projected namespace");

    // ARM 3 — the same head at the READ door: the same name, because it is
    // the same matcher and the same coverage table.
    let err = c
        .read(&ReadRequest {
            statement: r#"MATCH (g: spine/txn) RETURN g.id"#.to_string(),
            bindings: Bindings::new(),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect_err("a pattern read into the projected namespace refuses by name");
    match only_witness(&err) {
        Witness::UnimplementedForm { form } => assert_eq!(form, PROJECTED_PATTERN_FORM),
        other => panic!("arm 3: expected UnimplementedForm, got {other:?}"),
    }
    before.assert_nothing_landed(&c, &dir, "a pattern READ of the projected namespace");

    // The by-name refusal IS a coverage-TABLE row now: detected
    // from the statement's shape, like every other staged-out form. The two
    // retired door-gate names are gone from the crate.
    assert!(
        STAGED_OUT_FORMS.contains(&PROJECTED_PATTERN_FORM),
        "`{PROJECTED_PATTERN_FORM}` is a staged-coverage row"
    );
    for name in ["match:projected-namespace", "read:projected-namespace"] {
        assert!(!STAGED_OUT_FORMS.contains(&name), "`{name}` is retired");
    }
}

// ---------------------------------------------------------------------------
// the whole-cauldron fingerprint
// ---------------------------------------------------------------------------

/// Everything a landing could possibly move, in both pools, as one comparable
/// value: every node of every declared type with its state, revision and
/// fields; every edge with its whole pool-qualified triple; every declared
/// count cell; the committed-commit counter; the in-landing set; the pending
/// worklist.
///
/// "Nothing landed in ANY pool" is asserted as equality of this value, so a
/// refusal that landed one field of one node in one pool — or left a
/// transaction in landing state, or moved the counter — cannot pass.
#[derive(Debug, PartialEq, Eq)]
struct Fingerprint {
    counter: u64,
    pools: Vec<String>,
}

impl Fingerprint {
    fn of(c: &Cauldron, dir: &Path) -> Fingerprint {
        let mut pools = Vec::new();
        for grammar in [fixtures::fixture_alpha(), fixtures::fixture_beta()] {
            pools.push(pool_fingerprint(dir, &grammar));
        }
        Fingerprint {
            counter: c.committed_commit_counter().expect("counter"),
            pools,
        }
    }

    /// The landed state is unchanged, AND the refusal left nothing in flight:
    /// no transaction in landing state (which would keep locks held and
    /// invalidate every cross-pool read) and no un-cleared row in the pending
    /// worklist (which would mean an attempt neither committed nor aborted).
    /// The in-flight half is asserted absolutely rather than by comparison,
    /// because a fixture may be holding a transaction open when the `before`
    /// reading is taken, and a refusal must still leave nothing behind.
    fn assert_nothing_landed(&self, c: &Cauldron, dir: &Path, what: &str) {
        let now = Fingerprint::of(c, dir);
        assert_eq!(
            *self, now,
            "REFUSAL IS WHOLE: `{what}` must land NOTHING in ANY pool"
        );
        assert!(
            c.in_landing(None).expect("in landing").is_empty(),
            "`{what}` left a transaction in landing state"
        );
        let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
        assert!(
            spine.pending().expect("pending").is_empty(),
            "`{what}` left a transaction neither committed nor aborted: {:?}",
            spine.pending().expect("pending")
        );
    }
}

fn pool_fingerprint(dir: &Path, grammar: &PoolGrammar) -> String {
    let pool = Pool::open(&Pool::path_in(dir, &grammar.pool)).expect("pool");
    let snap = pool.snapshot().expect("snapshot");
    let mut out = String::new();
    for (ty, def) in &grammar.node_types {
        for node in snap.nodes_of(ty, None).expect("nodes") {
            // `schema_version` is in here DELIBERATELY (the
            // machine-owned stamp). It is part of the node's landed state,
            // so a refusal that moved it — a re-stamp on a premise that
            // failed, a create that stamped a node before the whole
            // statement refused — must move the fingerprint too.
            // RED: the stamp used to be omitted from this line, so a landing
            // that touched ONLY the stamp compared equal and "nothing
            // landed" passed for the wrong reason.
            out.push_str(&format!(
                "node {}/{} type={} state={:?} rev={} schema_version={} fields={:?}\n",
                grammar.pool,
                node.id.0,
                node.node_type,
                node.state,
                node.rev,
                node.schema_version,
                node.fields
            ));
            for cell in def.count_cells.keys() {
                out.push_str(&format!(
                    "cell {}/{}.{} = {:?}\n",
                    grammar.pool,
                    node.id.0,
                    cell,
                    snap.count_cell(&node.id, cell).expect("cell")
                ));
            }
            for edge in snap.edges_from(&node.id, None).expect("edges") {
                out.push_str(&format!(
                    "edge {}/{} -[{}]-> {}/{}\n",
                    grammar.pool, edge.src.0, edge.kind, edge.dst.pool, edge.dst.id.0
                ));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-refusal-{name}-{}", std::process::id()))
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
                thread: "refusal-witnesses".into(),
                dispatch: "refusal-witnesses".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
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

fn body(node: &NodeId) -> Resource {
    Resource::Body { node: node.clone() }
}

fn one(resource: Resource, mode: LockMode) -> ClaimSet {
    let mut set = ClaimSet::new();
    set.insert(Claim {
        pool: PoolName::new(ALPHA),
        resource,
        mode,
    });
    set
}

fn park(dir: &Path, held: &ClaimSet, tag: &str) -> Seq {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let env = commit(&format!("-- parking holder: {tag}"), &[]);
    let hash = env.hash_identity().expect("hash");
    let seq = spine
        .append_pending(
            &env,
            &hash,
            &CompiledPayload {
                parts: Vec::new(),
                minted: std::collections::BTreeMap::new(),
            },
        )
        .expect("parking row");
    locks::acquire(&spine, seq, held, &SpineConfig::default()).expect("park");
    seq
}

fn unpark(dir: &Path, seq: Seq) {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    locks::release_at_clear(&spine, seq).expect("release");
    spine
        .mark_cleared(seq, &serde_json::json!({ "parking_holder": "released" }))
        .expect("close the parking row");
}

/// Move a node's state UNDER the lock table, the way a foreign process that
/// already holds its locks would have. Used to make a match set stale while
/// the statement under test is queued — a door-driven mover cannot do it,
/// because the queued statement's own claims would block it.
fn poison_state(dir: &Path, node: &NodeId, state: &str) {
    let mut pool = Pool::open(&Pool::path_in(dir, &PoolName::new(ALPHA))).expect("alpha");
    let part = CompiledPart {
        pool: PoolName::new(ALPHA),
        schema_version: 1,
        deltas: vec![Delta::SetField {
            node: node.clone(),
            path: vec!["state".into()],
            value: serde_json::json!(state),
        }],
        premises: Vec::new(),
        far_versions: Default::default(),
    };
    let mut bytes = [0u8; 32];
    bytes[0] = 0xf0;
    bytes[1] = node.0.len() as u8;
    pool.apply_part(&TxHash(bytes), 0, &part).expect("poison");
}

fn await_new_claimant(spine: &SpineDb, resource: &Resource, known: &[Seq]) -> Seq {
    let started = Instant::now();
    loop {
        let q = locks::queue(spine, &PoolName::new(ALPHA), resource).expect("queue");
        if let Some(e) = q.iter().find(|e| !known.contains(&e.seq) && !e.granted) {
            return e.seq;
        }
        assert!(
            started.elapsed() < Duration::from_secs(25),
            "no door call ever queued on {resource:?}: {q:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn create_doc(c: &Cauldron, title: &str) -> NodeId {
    match c
        .submit(commit(
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

fn state_of(c: &Cauldron, id: &str) -> Option<String> {
    let mut bindings = Bindings::new();
    bindings.insert("id".into(), serde_json::json!(id));
    c.read(&ReadRequest {
        statement: r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.state"#.into(),
        bindings,
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    })
    .expect("read")
    .rows
    .0
    .first()
    .and_then(|r| r["d.state"].as_str().map(str::to_string))
}
