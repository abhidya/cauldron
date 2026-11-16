//! The `IMPORT` statement form: carried serial ids through the ONE
//! door, the `(pool, prefix)` counter advanced
//! to the highest carried number in the same decided flip, the `Agent`-only
//! initiator gate, and the explicit arms of the flag rule
//! (`flags::check_flag_grants`, the transitional `Agent` arm — a tolerated
//! deviation, to be removed later).
//!
//! Every cauldron here pins the REAL v6 workspace grammar
//! (`grammar_v6::workspace_v6("ws")`), so a content node mints only with an
//! `in_region` edge (the one-of rule) and the carried ids are judged by
//! the prefixes v6 declares (`Q`, `DEC`, `R`, `V`, ...). One fixture pool
//! `plain` adds a type with NO prefix, the one way to reach the
//! `machine_owned_field` arm of the IMPORT judgment.
//!
//! Each guard names its forcings in a `// RED:` comment.

use spine::config::SpineConfig;
use spine::delta::{CompiledPart, CompiledPayload, Delta};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance, RippleKind};
use spine::error::SpineError;
use spine::exec::check_import_initiator;
use spine::flags::check_flag_grants;
use spine::grammar_v6::workspace_v6;
use spine::ladle::ast::Mutation;
use spine::ladle::parse::parse;
use spine::pool::{NodeId, QualifiedNodeId};
use spine::read::MatchSet;
use spine::schema_def::{grammar_fault_tag, FieldDef, NodeTypeDef, PoolGrammar, PoolKind, PoolName};
use spine::spine_db::SPINE_FILE;
use spine::witness::Witness;
use spine::{Cauldron, Outcome};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

test_scratch::install!();

const WS: &str = "ws";
const PLAIN: &str = "plain";

const BOUNDARY: &str = "every node about the import form, its counter and its initiator gate";

// ---------------------------------------------------------------------------
// guard 1: a carried id lands as itself, N >= 2 prefixes, in-process
// ---------------------------------------------------------------------------

/// One statement IMPORTs a region `R-7` and questions `Q-3`, `Q-65` with
/// string-NOTE `in_region` edges: it commits, RETURN projects the carried ids,
/// `minted` maps the bindings to them, and `node_view` shows each node and
/// the edge's note verbatim.
// RED A: the IMPORT loop's `minted.insert` replaced by the provisional
// fallback (`provisional_serial_id`) — the landed id came back `Q-1`, not
// `Q-3`. RED B (opposing): the follow-up plain CREATE must land `Q-66` — not
// `Q-65` (no counter advance: it collided, `duplicate_node_id` at landing)
// and not `Q-1` (counter untouched).
#[test]
fn a_carried_id_lands_as_itself_across_two_prefixes() {
    let dir = root("lands");
    let c = fresh(&dir);
    let out = c
        .submit(agent(
            r#"IMPORT (r: ws/region { id: $r_id, handle: $r_h, boundary: $r_b })
               IMPORT (q3: ws/question { id: $q3_id, handle: $q3_h, state: "open" })
               IMPORT (q65: ws/question { id: $q65_id, handle: $q65_h, meta: $q65_m })
               CREATE (q3) -[in_region]-> (r) NOTE $s3
               CREATE (q65) -[in_region]-> (r) NOTE $s65
               RETURN r.id, q3.id, q65.id"#,
            &[
                ("r_id", json!("R-7")),
                ("r_h", json!("the import region")),
                ("r_b", json!(BOUNDARY)),
                ("q3_id", json!("Q-3")),
                ("q3_h", json!("question three")),
                ("q65_id", json!("Q-65")),
                ("q65_h", json!("question sixty-five")),
                ("q65_m", json!({"blocking": true})),
                ("s3", json!("admits: the import form")),
                ("s65", json!("admits: the counter")),
            ],
        ))
        .expect("the IMPORT statement commits");
    let row = first_row(&out);
    assert_eq!(row["r.id"], "R-7");
    assert_eq!(row["q3.id"], "Q-3");
    assert_eq!(row["q65.id"], "Q-65");

    let q3 = view(&c, "Q-3");
    assert_eq!(q3.record.node_type, "question");
    assert_eq!(q3.record.state.as_deref(), Some("open"));
    assert_eq!(q3.record.fields["handle"].value, json!("question three"));
    assert!(!q3.record.fields.contains_key("id"), "`id` is the record's id, never a field");
    assert_eq!(q3.out.len(), 1);
    assert_eq!(q3.out[0].kind, "in_region");
    assert_eq!(q3.out[0].dst.id, NodeId("R-7".into()));
    assert_eq!(q3.out[0].note.as_deref(), Some("admits: the import form"));
    assert!(q3.out[0].fields.is_empty(), "a string NOTE is the free note, never fields");
    assert_eq!(view(&c, "Q-65").out[0].note.as_deref(), Some("admits: the counter"));
    assert_eq!(view(&c, "R-7").record.node_type, "region");

    // The opposing forcing: the next ordinary mint numbers ABOVE the carried.
    let q = create_question(&c, "R-7", "minted after the import");
    assert_eq!(q, "Q-66", "the ordinary mint after a carried Q-65");
    assert_eq!(serials(&dir)["Q"], 66);
    assert_eq!(serials(&dir)["R"], 7);
}

// ---------------------------------------------------------------------------
// guard 2 (in-process half): CREATE still refuses a caller id; only Agent
// may IMPORT
// ---------------------------------------------------------------------------

/// A plain CREATE naming `id` still refuses `machine_owned_field` (a
/// prefixed id) and `id_prefix_mismatch` (a wrong prefix), byte-for-byte as
/// before the form existed — the IMPORT path is LEXICAL, so the CREATE arm is
/// untouched.
// RED: route CREATE's `FIELD_ID` arm through `carried_id` (the import
// judgment) — the CREATE with `id: "Q-900"` landed as `Q-900`.
#[test]
fn an_ordinary_create_naming_an_id_still_refuses() {
    let dir = root("create-id");
    let c = fresh(&dir);
    seed_region(&c, "R-1");
    for (id, tag) in [("Q-900", "machine_owned_field"), ("DEC-900", "id_prefix_mismatch")] {
        let w = refused(c.submit(agent(
            r#"MATCH (r: ws/region { id: $r })
               CREATE (q: ws/question { id: $id, handle: "h" })
               CREATE (q) -[in_region]-> (r)"#,
            &[("r", json!("R-1")), ("id", json!(id))],
        )));
        assert_eq!(grammar_fault_tag(&w), Some(tag), "CREATE with id {id}: {w:?}");
    }
    assert!(!exists(&c, "Q-900"));
}

/// `IMPORT` is an `Agent`'s alone: from `CoreMechanical` and from a type-two
/// ripple (empty scope) it is refused whole, `import_not_permitted` naming
/// the initiator kind, in BOTH modes, and nothing lands. The same statement
/// from an `Agent` lands (the opposing sentinel: a constant-refuse gate
/// fails it).
// RED: `check_import_initiator` admitting every initiator — the
// `CoreMechanical` IMPORT committed and `Q-5` existed. Opposing: the Agent
// submission must commit (a constant refusal fails it).
#[test]
fn only_an_agent_may_submit_an_import() {
    let dir = root("gate");
    let c = fresh(&dir);
    let stmt = r#"IMPORT (r: ws/region { id: $r, handle: "h", boundary: $b })
                  IMPORT (q: ws/question { id: $q, handle: "h" })
                  CREATE (q) -[in_region]-> (r) RETURN q.id"#;
    let params = [("r", json!("R-2")), ("b", json!(BOUNDARY)), ("q", json!("Q-5"))];
    let others = [
        (Initiator::CoreMechanical { operation: "import-probe".into() }, "core_mechanical"),
        (
            Initiator::Ripple {
                kind: RippleKind::TypeTwo,
                run_id: "run".into(),
                step_id: "step".into(),
                scope: vec![],
            },
            "type_two",
        ),
        (
            Initiator::Ripple {
                kind: RippleKind::TypeOne,
                run_id: "run".into(),
                step_id: "step".into(),
                scope: vec![],
            },
            "type_one",
        ),
    ];
    for (initiator, kind) in others {
        for mode in [Mode::Commit, Mode::Validate] {
            let mut env = agent(stmt, &params);
            env.provenance.initiator = initiator.clone();
            env.mode = mode;
            let w = refused(c.submit(env));
            assert_eq!(
                w,
                Witness::ImportNotPermitted { initiator_kind: kind.into() },
                "{kind} in {mode:?}"
            );
        }
    }
    assert!(!exists(&c, "Q-5"), "nothing landed from a refused initiator");
    assert_eq!(c.committed_commit_counter().unwrap(), 0);
    let out = c.submit(agent(stmt, &params)).expect("the Agent's IMPORT commits");
    assert_eq!(first_row(&out)["q.id"], "Q-5");
}

/// The `External` arm is reachable only by calling the function directly:
/// both doors refuse an `External` envelope as `Usage` inside
/// `Envelope::validate`, before this gate. Pinned so a change is visible; a
/// statement with no IMPORT passes every initiator.
#[test]
fn the_external_arm_answers_when_called_directly() {
    let with = parse(r#"IMPORT (q: ws/question { id: $q, handle: "h" })"#).expect("parses");
    let without = parse(r#"CREATE (q: ws/question { handle: "h" })"#).expect("parses");
    match check_import_initiator(&Initiator::External, &with) {
        Err(SpineError::Refused { witnesses }) => assert_eq!(
            witnesses,
            vec![Witness::ImportNotPermitted { initiator_kind: "external".into() }]
        ),
        other => panic!("External IMPORT must refuse: {other:?}"),
    }
    for i in [
        Initiator::External,
        Initiator::CoreMechanical { operation: "x".into() },
        agent_initiator(),
    ] {
        check_import_initiator(&i, &without).expect("no IMPORT clause, nothing to gate");
    }
    check_import_initiator(&agent_initiator(), &with).expect("an Agent may IMPORT");
}

// ---------------------------------------------------------------------------
// guard 3: the counter advances TO the max carried id, per (pool, prefix)
// ---------------------------------------------------------------------------

/// ONE statement imports `Q-3, Q-65, DEC-9, DEC-331` and ALSO creates one
/// question and one decision: the counters read `Q=66`, `DEC=332` and the
/// two mints are `Q-66`, `DEC-332` — numbered above the carried in the same
/// flip. The next statement mints `Q-67`, `DEC-333`. A later import of a
/// LOWER free id (`Q-1`) lands and leaves the counter at 67 (`max`, never
/// down); a re-import of `Q-65` is `duplicate_node_id` and writes no row.
// RED A: skip the carried advance — the mixed statement's question minted
// `Q-1`... no: with the counter at 0 the mint is `Q-1`, which LANDS beside
// Q-3 (no collision) — so the assertion is equality on `Q-66`, red either
// way. RED B (opposing): advance to `max+1` — the mint came back `Q-67`.
// RED C: advance only the first carried prefix — `DEC` stayed 0 and the
// decision minted `DEC-1`. RED D: number provisionals BEFORE advancing —
// the mint was `Q-1` and the counter then `max`ed to 65: a landed `Q-1`
// beside `Q-65` (asserted absent below).
#[test]
fn the_counter_advances_to_the_max_carried_id_per_prefix_in_one_flip() {
    let dir = root("counter");
    let c = fresh(&dir);
    let out = c
        .submit(agent(
            r#"IMPORT (r: ws/region { id: $r, handle: "h", boundary: $b })
               IMPORT (q3: ws/question { id: $q3, handle: "h" })
               IMPORT (q65: ws/question { id: $q65, handle: "h" })
               IMPORT (d9: ws/decision { id: $d9, handle: "h" })
               IMPORT (d331: ws/decision { id: $d331, handle: "h" })
               CREATE (nq: ws/question { handle: "minted q" })
               CREATE (nd: ws/decision { handle: "minted d" })
               CREATE (q3) -[in_region]-> (r)
               CREATE (q65) -[in_region]-> (r)
               CREATE (d9) -[in_region]-> (r)
               CREATE (d331) -[in_region]-> (r)
               CREATE (nq) -[in_region]-> (r)
               CREATE (nd) -[in_region]-> (r)
               RETURN nq.id, nd.id"#,
            &[
                ("r", json!("R-7")),
                ("b", json!(BOUNDARY)),
                ("q3", json!("Q-3")),
                ("q65", json!("Q-65")),
                ("d9", json!("DEC-9")),
                ("d331", json!("DEC-331")),
            ],
        ))
        .expect("the mixed statement commits");
    let row = first_row(&out);
    assert_eq!(row["nq.id"], "Q-66", "the question minted above the carried Q-65");
    assert_eq!(row["nd.id"], "DEC-332", "the decision minted above the carried DEC-331");
    let s = serials(&dir);
    assert_eq!((s["Q"], s["DEC"], s["R"]), (66, 332, 7), "{s:?}");
    assert!(!exists(&c, "Q-1"), "no mint numbered from the un-advanced counter (RED D)");
    assert!(!exists(&c, "DEC-1"));

    // The next statement continues from the advanced counters.
    let out = c
        .submit(agent(
            r#"MATCH (r: ws/region { id: $r })
               CREATE (q: ws/question { handle: "next q" })
               CREATE (d: ws/decision { handle: "next d" })
               CREATE (q) -[in_region]-> (r)
               CREATE (d) -[in_region]-> (r)
               RETURN q.id, d.id"#,
            &[("r", json!("R-7"))],
        ))
        .expect("the next mints commit");
    let row = first_row(&out);
    assert_eq!((row["q.id"].clone(), row["d.id"].clone()), (json!("Q-67"), json!("DEC-333")));

    // A LOWER free id imports, and the counter does not move down.
    import_question(&c, "Q-1", "R-7").expect("a free lower id imports");
    assert_eq!(serials(&dir)["Q"], 67, "`max(last, n)`: the counter never decrements");

    // A carried id the pool already holds refuses whole, before the flip.
    let before = c.committed_commit_counter().unwrap();
    let w = refused(import_question(&c, "Q-65", "R-7"));
    assert_eq!(grammar_fault_tag(&w), Some("duplicate_node_id"), "{w:?}");
    assert_eq!(c.committed_commit_counter().unwrap(), before, "no row landed");
    assert_eq!(serials(&dir)["Q"], 67);
}

/// A refused import advances nothing: the counter is the flip's (a
/// number is consumed exactly when a commit is made).
// RED: advance the counter at compile instead of at the flip — `Q` read
// 80 after the refused statement.
#[test]
fn a_refused_import_advances_no_counter() {
    let dir = root("refused-no-advance");
    let c = fresh(&dir);
    seed_region(&c, "R-1");
    // `body` over the 10,000 cap: the grammar refuses the whole statement.
    let w = refused(c.submit(agent(
        r#"MATCH (r: ws/region { id: $r })
           IMPORT (q: ws/question { id: $q, handle: "h", body: $body })
           CREATE (q) -[in_region]-> (r)"#,
        &[("r", json!("R-1")), ("q", json!("Q-80")), ("body", json!("x".repeat(10_001)))],
    )));
    assert!(matches!(w, Witness::OverCap { .. }), "{w:?}");
    assert_eq!(serials(&dir).get("Q"), None, "no Q counter exists after a refusal");
    assert_eq!(create_question(&c, "R-1", "first real"), "Q-1");
}

// ---------------------------------------------------------------------------
// the provisional-id landing check never fires on a
// carried id
// ---------------------------------------------------------------------------

/// A carried serial id is not a provisional: the landing's
/// `provisional_id_at_landing` judgment (`schema_def::validate_delta`)
/// reads only the `<P>-~hex` shape, so an IMPORT of `Q-65` (and `R-7`, a
/// second prefix) commits on BOTH modes and lands under its own id; the
/// counter is advanced and nothing is rewritten.
// RED: route carried ids through the provisional landing check (the landing
// arm fires on any `<P>-<digits>` id too) — the IMPORT is refused
// `provisional_id_at_landing`. OPPOSING: make the check never fire —
// `provisional_id_landing`'s G3 (`T-~<hex>` at a real landing) goes red, so
// a constant-pass check fails there while this test stays green.
#[test]
fn a_carried_id_never_trips_the_provisional_landing_check() {
    let dir = root("provisional-landing");
    let c = fresh(&dir);
    let statement = r#"IMPORT (r: ws/region { id: $r, handle: "h", boundary: $b })
           IMPORT (q: ws/question { id: $q, handle: "carried, never provisional" })
           CREATE (q) -[in_region]-> (r)
           RETURN r.id, q.id"#;
    let params = [("r", json!("R-7")), ("b", json!(BOUNDARY)), ("q", json!("Q-65"))];
    let mut validate = agent(statement, &params);
    validate.mode = Mode::Validate;
    match c.submit(validate) {
        Ok(Outcome::Validated { .. }) => {}
        other => panic!("VALIDATE of an IMPORT must pass: {other:?}"),
    }
    let out = match c.submit(agent(statement, &params)) {
        Ok(out) => out,
        Err(SpineError::Refused { witnesses }) => panic!(
            "the carried id was refused at the landing: {:?}",
            witnesses.iter().map(|w| grammar_fault_tag(w)).collect::<Vec<_>>()
        ),
        Err(other) => panic!("the carried id did not land: {other:?}"),
    };
    let row = first_row(&out);
    assert_eq!((row["r.id"].clone(), row["q.id"].clone()), (json!("R-7"), json!("Q-65")));
    assert_eq!(view(&c, "Q-65").record.id, NodeId("Q-65".into()));
    assert_eq!(serials(&dir)["Q"], 65);
}

// ---------------------------------------------------------------------------
// the form's own judgments, in their fixed order
// ---------------------------------------------------------------------------

/// The carried id's three judgments: a type with no prefix is
/// `machine_owned_field`; a wrong prefix is `id_prefix_mismatch`; a rest that
/// is not an unpadded serial ≥ 1 — the provisional `-~` form among them — is
/// `carried_id_not_serial` with the id named in the detail.
// RED: drop the serial-shape filter (accept any non-empty rest) — `Q-~abc`
// reached the landing and refused `provisional_id_at_landing` (after the
// flip: `Hard`), `Q-abc` and `Q-007` LANDED. Opposing: `Q-10` must land.
#[test]
fn the_carried_id_is_judged_prefix_then_serial() {
    let dir = root("judged");
    let c = fresh(&dir);
    seed_region(&c, "R-1");
    for bad in ["Q-~abc", "Q-~0123456789abcdef", "Q-abc", "Q-0", "Q-007", "Q-1x"] {
        let w = refused(import_question(&c, bad, "R-1"));
        assert_eq!(grammar_fault_tag(&w), Some("carried_id_not_serial"), "{bad}: {w:?}");
        let Witness::GrammarViolation { message, .. } = &w else { unreachable!() };
        assert!(message.contains(&format!("`{bad}`")), "the detail names the id: {message}");
    }
    // `Q-` (empty rest) answers the PREFIX rule first, in the fixed order:
    // the one validator's id-prefix check already requires a non-empty rest,
    // so it never reaches the serial judgment.
    for (bad, tag) in [("DEC-9", "id_prefix_mismatch"), ("q-9", "id_prefix_mismatch"), ("Q-", "id_prefix_mismatch")] {
        let w = refused(import_question(&c, bad, "R-1"));
        assert_eq!(grammar_fault_tag(&w), Some(tag), "{bad}: {w:?}");
    }
    // A non-string id carries no prefix.
    let w = refused(c.submit(agent(
        r#"MATCH (r: ws/region { id: $r })
           IMPORT (q: ws/question { id: $q, handle: "h" })
           CREATE (q) -[in_region]-> (r)"#,
        &[("r", json!("R-1")), ("q", json!(65))],
    )));
    assert_eq!(grammar_fault_tag(&w), Some("id_prefix_mismatch"), "{w:?}");
    // A type whose grammar mints no serial.
    let w = refused(c.submit(agent(
        r#"IMPORT (n: plain/note { id: $n, name: "x" })"#,
        &[("n", json!("N-1"))],
    )));
    assert_eq!(grammar_fault_tag(&w), Some("machine_owned_field"), "{w:?}");
    // The opposing sentinel.
    import_question(&c, "Q-10", "R-1").expect("a serial id imports");
    assert!(exists(&c, "Q-10"));
    assert!(!exists(&c, "Q-abc") && !exists(&c, "Q-007"));
}

/// Two IMPORT heads naming one id in one statement refuse
/// `duplicate_node_id` (the validator's part-level check judges an id
/// against the KNOWN set only, so the statement-level duplicate is refused
/// by the import loop, same tag); a carried id the pool holds is
/// `duplicate_node_id` in a DRY RUN too (phase 10b runs on both modes).
// RED: delete the import loop's in-statement duplicate check — the
// statement reached the landing and the second INSERT failed after the
// decided flip (`Hard`, not a refusal).
#[test]
fn a_duplicate_carried_id_refuses_by_name() {
    let dir = root("dup");
    let c = fresh(&dir);
    seed_region(&c, "R-1");
    let w = refused(c.submit(agent(
        r#"MATCH (r: ws/region { id: $r })
           IMPORT (a: ws/question { id: $q, handle: "a" })
           IMPORT (b: ws/question { id: $q, handle: "b" })
           CREATE (a) -[in_region]-> (r)
           CREATE (b) -[in_region]-> (r)"#,
        &[("r", json!("R-1")), ("q", json!("Q-4"))],
    )));
    assert_eq!(grammar_fault_tag(&w), Some("duplicate_node_id"), "{w:?}");
    assert!(!exists(&c, "Q-4"));
    // The stored collision, in validate mode.
    let w = refused(c.submit({
        let mut e = agent(
            r#"IMPORT (r: ws/region { id: $r, handle: "h", boundary: $b })"#,
            &[("r", json!("R-1")), ("b", json!(BOUNDARY))],
        );
        e.mode = Mode::Validate;
        e
    }));
    assert_eq!(grammar_fault_tag(&w), Some("duplicate_node_id"), "{w:?}");
}

/// The parser: IMPORT takes a fresh node head naming `id`; a bound end, an
/// edge shape, or a head without `id` is malformed; IMPORT sits in the
/// mutation phase (after MATCH, before RETURN).
#[test]
fn the_parser_admits_exactly_a_node_head_naming_id() {
    let s = parse(r#"MATCH (r: ws/region { id: $r }) IMPORT (q: ws/question { id: $q, handle: "h" }) CREATE (q) -[in_region]-> (r) RETURN q.id"#)
        .expect("parses");
    assert_eq!(s.imported_bindings().len(), 1);
    assert!(s.created_bindings().is_empty(), "an import head is not a CREATE ordinal");
    assert!(s.matched_bindings().contains("r") && !s.matched_bindings().contains("q"));
    assert!(s.referenced_params().contains("q"));
    assert!(matches!(s.mutations[0], Mutation::ImportNode(_)), "{:?}", s.mutations[0]);
    for (text, needle) in [
        (r#"IMPORT (q: ws/question { handle: "h" })"#, "name `id`"),
        (r#"MATCH (q: ws/question { id: $q }) IMPORT (q)"#, "IMPORT takes a node head"),
        (r#"IMPORT (a: ws/question { id: $a }) -[ref]-> (b: ws/question { id: $b })"#, "an edge is CREATEd"),
        (r#"MATCH (r: ws/region { id: $r }) RETURN r.id IMPORT (q: ws/question { id: $q })"#, "out of order"),
    ] {
        match parse(text) {
            Err(SpineError::Refused { witnesses }) => {
                let Witness::MalformedStatement { message, .. } = &witnesses[0] else {
                    panic!("{text}: {witnesses:?}")
                };
                assert!(message.contains(needle), "{text}: {message}");
            }
            other => panic!("{text} must be malformed: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// guard 5 (direct half): the flag rule's explicit arms, N = 2 fields
// ---------------------------------------------------------------------------

/// One hand-built payload carrying BOTH `meta.blocking` on a question AND
/// `deferred_by` on a region. `Agent`, `CoreMechanical` and `External` are
/// admitted (the last because the rule is silent on it); a type-one ripple is refused
/// BOTH fields; a type-two ripple scoped to `deferred_by` is refused exactly
/// the other one.
// RED A: delete the `Agent` arm (let `Agent` fall to the ripple judgment,
// as type one) — the Agent call returned two `flag_not_writable`. RED B
// (opposing): `check_flag_grants` returning `Ok(())` unconditionally — the
// type-one call came back `Ok`.
#[test]
fn the_flag_rule_names_every_initiator_explicitly() {
    let payload = CompiledPayload {
        parts: vec![CompiledPart {
            pool: PoolName::new(WS),
            schema_version: workspace_v6(WS).version,
            deltas: vec![
                Delta::CreateNode {
                    id: NodeId("Q-65".into()),
                    node_type: "question".into(),
                    state: None,
                    fields: BTreeMap::from([("meta".to_string(), json!({"blocking": true}))]),
                },
                Delta::CreateNode {
                    id: NodeId("R-7".into()),
                    node_type: "region".into(),
                    state: None,
                    fields: BTreeMap::from([(
                        "deferred_by".to_string(),
                        json!({"thread": "t", "msg": 1}),
                    )]),
                },
            ],
            premises: vec![],
            far_versions: BTreeMap::new(),
        }],
        minted: BTreeMap::new(),
    };
    let m = MatchSet::default();
    for i in [
        agent_initiator(),
        Initiator::CoreMechanical { operation: "x".into() },
        Initiator::External,
    ] {
        check_flag_grants(&i, &payload, &m).unwrap_or_else(|e| panic!("{i:?} is admitted: {e:?}"));
    }
    let ripple = |kind, scope: &[&str]| Initiator::Ripple {
        kind,
        run_id: "r".into(),
        step_id: "s".into(),
        scope: scope.iter().map(|s| s.to_string()).collect(),
    };
    let fields = |r: spine::error::Result<()>| -> Vec<(String, String)> {
        match r {
            Err(SpineError::Refused { witnesses }) => witnesses
                .into_iter()
                .map(|w| match w {
                    Witness::FlagNotWritable { field, initiator_kind, .. } => (field, initiator_kind),
                    other => panic!("{other:?}"),
                })
                .collect(),
            other => panic!("expected flag_not_writable, got {other:?}"),
        }
    };
    assert_eq!(
        fields(check_flag_grants(&ripple(RippleKind::TypeOne, &[]), &payload, &m)),
        vec![
            ("meta.blocking".to_string(), "type_one".to_string()),
            ("deferred_by".to_string(), "type_one".to_string()),
        ]
    );
    assert_eq!(
        fields(check_flag_grants(&ripple(RippleKind::TypeTwo, &["deferred_by"]), &payload, &m)),
        vec![("meta.blocking".to_string(), "type_two".to_string())]
    );
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// an IMPORT statement edits nothing it did not import
// ---------------------------------------------------------------------------

/// A statement that carries an `IMPORT` clause and a `SET`, a `SEAL` or a
/// `REMOVE` on a matched, pre-existing row is refused whole,
/// `import_mixed_with_edit` naming the clause's form, in BOTH modes, and
/// neither the import nor the edit lands (the row's `rev` and seal stand;
/// the committed counter stays at the seed's 1). The soup tags a commit
/// carrying any IMPORT `import` whole and fires no trigger on it,
/// so an edit riding in one would escape every trigger.
/// Opposing: `IMPORT` beside `CREATE` — a store-minted node
/// and its edges, the v5 regeneration's shape (`migrate::run`) — commits,
/// so a gate refusing every second clause form fails this test.
// RED: the gate removed from `check_write_form` — the IMPORT+SET committed:
// `Q-30` existed and `R-1`'s rev moved. Opposing: a gate on ANY clause
// beside IMPORT refuses the IMPORT+CREATE statement.
#[test]
fn an_import_statement_edits_nothing_it_did_not_import() {
    let dir = root("mixed-edit");
    let c = fresh(&dir);
    seed_region(&c, "R-1");
    let before = view(&c, "R-1").record;
    assert!(!before.sealed);
    let edits: [(&str, &str); 3] = [
        (
            r#"MATCH (r: ws/region { id: $r })
               IMPORT (q: ws/question { id: $q, handle: "h" })
               CREATE (q) -[in_region]-> (r)
               SET r.handle = "edited""#,
            "set:field",
        ),
        (
            r#"MATCH (r: ws/region { id: $r })
               IMPORT (q: ws/question { id: $q, handle: "h" })
               CREATE (q) -[in_region]-> (r)
               SEAL (r)"#,
            "seal:node",
        ),
        (
            r#"MATCH (r: ws/region { id: $r })
               IMPORT (q: ws/question { id: $q, handle: "h" })
               CREATE (q) -[in_region]-> (r)
               REMOVE (q) -[in_region]-> (r)"#,
            "remove:edge",
        ),
    ];
    for (stmt, form) in edits {
        for mode in [Mode::Commit, Mode::Validate] {
            let mut env = agent(stmt, &[("r", json!("R-1")), ("q", json!("Q-30"))]);
            env.mode = mode;
            let w = refused(c.submit(env));
            assert_eq!(w, Witness::ImportMixedWithEdit { form: form.into() }, "{form} in {mode:?}");
        }
    }
    assert!(!exists(&c, "Q-30"), "nothing landed from a refused mix");
    let after = view(&c, "R-1").record;
    assert_eq!((after.rev, after.sealed), (before.rev, false), "the matched row was not edited");
    assert_eq!(c.committed_commit_counter().unwrap(), 1, "the seed only");

    // Opposing: IMPORT beside CREATE (a store-minted node and its edges) commits.
    let out = c
        .submit(agent(
            r#"MATCH (r: ws/region { id: $r })
               IMPORT (q: ws/question { id: $q, handle: "imported" })
               CREATE (q) -[in_region]-> (r)
               CREATE (n: ws/question { handle: "minted" })
               CREATE (n) -[in_region]-> (r)
               RETURN q.id, n.id"#,
            &[("r", json!("R-1")), ("q", json!("Q-30"))],
        ))
        .expect("IMPORT beside CREATE is the importer's own shape and commits");
    let row = first_row(&out);
    assert_eq!(row["q.id"], "Q-30");
    let n = row["n.id"].as_str().expect("a minted id");
    assert!(n.starts_with("Q-") && n != "Q-30", "the create minted a fresh Q: {n}");
    assert!(exists(&c, "Q-30") && exists(&c, n));
}

// ---------------------------------------------------------------------------
// the legacy `SUB-` region prefix on
// the IMPORT form ONLY
// ---------------------------------------------------------------------------

fn import_region(c: &Cauldron, id: &str) -> spine::error::Result<Outcome> {
    c.submit(agent(
        r#"IMPORT (r: ws/region { id: $r, handle: "legacy", boundary: $b }) RETURN r.id"#,
        &[("r", json!(id)), ("b", json!(BOUNDARY))],
    ))
}

fn create_region(c: &Cauldron) -> String {
    let out = c
        .submit(agent(
            r#"CREATE (r: ws/region { handle: "minted", boundary: $b }) RETURN r.id"#,
            &[("b", json!(BOUNDARY))],
        ))
        .unwrap_or_else(|e| panic!("create region: {e:?}"));
    first_row(&out)["r.id"].as_str().expect("id").to_string()
}

/// Guard 1: a carried `SUB-3` region IMPORTs under exactly that id,
/// and a content node homed in it lands (its `in_region` resolves to the
/// carried id); `SUB-0`, `SUB-007`, `SUB-~ab` refuse `carried_id_not_serial`
/// by the same serial rule an `R-` id meets.
// RED: revert the legacy acceptance (`IdPrefixRule::OrLegacyImport` admits
// only the mint prefix) — `SUB-3` refused `id_prefix_mismatch: node type
// `region` mints ids `R-<n>`; `SUB-3` does not carry that prefix` (the
// old tag), and the three malformed ids answered `id_prefix_mismatch`
// too, never reaching the serial rule.
#[test]
fn a_carried_sub_region_lands_on_import() {
    let dir = root("sub-lands");
    let c = fresh(&dir);
    let out = import_region(&c, "SUB-3").expect("a carried SUB-3 region imports");
    assert_eq!(first_row(&out)["r.id"], "SUB-3");
    let r = view(&c, "SUB-3");
    assert_eq!(r.record.node_type, "region");
    assert_eq!(r.record.id.0, "SUB-3", "kept its id and number");
    import_question(&c, "Q-4", "SUB-3").expect("a question homed in SUB-3 imports");
    let q = view(&c, "Q-4");
    assert_eq!(q.out[0].kind, "in_region");
    assert_eq!(q.out[0].dst.id, NodeId("SUB-3".into()), "the citation resolves to the carried id");
    for bad in ["SUB-0", "SUB-007", "SUB-~ab"] {
        let w = refused(import_region(&c, bad));
        assert_eq!(grammar_fault_tag(&w), Some("carried_id_not_serial"), "{bad}: {w:?}");
        let Witness::GrammarViolation { message, .. } = &w else { unreachable!() };
        assert!(message.contains(&format!("`{bad}`")), "the detail names the id: {message}");
        assert!(!exists(&c, bad));
    }
}

/// Guard 2: ...and ONLY on IMPORT. A plain CREATE naming `SUB-3`
/// refuses with exactly the tag AND witness shape a wrong-prefix CREATE
/// answers (`X-3`, a prefix no type declares), and a CREATE region with no
/// id still mints `R-<n>`.
// RED: admit the legacy prefix in the CREATE caller-id rule too
// (`caller_id_against_prefix` judging under `OrLegacyImport`) — the
// `SUB-3` CREATE answered `machine_owned_field`, not `id_prefix_mismatch`.
// Opposes guard 1: "admit nowhere" fails guard 1, "admit everywhere" this.
#[test]
fn a_create_naming_a_sub_region_refuses_like_any_wrong_prefix() {
    let dir = root("sub-create");
    let c = fresh(&dir);
    let create = |id: &str| {
        refused(c.submit(agent(
            r#"CREATE (r: ws/region { id: $r, handle: "h", boundary: $b })"#,
            &[("r", json!(id)), ("b", json!(BOUNDARY))],
        )))
    };
    let sub = create("SUB-3");
    let wrong = create("X-3");
    assert_eq!(grammar_fault_tag(&sub), Some("id_prefix_mismatch"), "{sub:?}");
    assert_eq!(grammar_fault_tag(&wrong), Some("id_prefix_mismatch"), "{wrong:?}");
    let (Witness::GrammarViolation { message: ms, pool: ps, part: pts }, Witness::GrammarViolation { message: mw, pool: pw, part: ptw }) = (&sub, &wrong) else {
        panic!("grammar witnesses: {sub:?} / {wrong:?}")
    };
    assert_eq!((ps, pts), (pw, ptw), "same witness shape");
    assert_eq!(ms.replace("SUB-3", "X-3"), *mw, "same message, the id aside");
    assert!(ms.contains("mints ids `R-<n>`"), "{ms}");
    assert!(!exists(&c, "SUB-3"));
    assert_eq!(create_region(&c), "R-1", "a CREATE region with no id mints R-");
}

/// Guard 3: ...and ONLY for a region. Under IMPORT, `SUB-4` on a
/// decision and `SUB-5` on a question refuse `id_prefix_mismatch`.
// RED: admit `SUB` for every type under IMPORT (the legacy lookup ignoring
// the node type) — both landed.
#[test]
fn a_sub_id_on_any_other_type_refuses_under_import() {
    let dir = root("sub-other");
    let c = fresh(&dir);
    seed_region(&c, "R-1");
    for (ty, id) in [("decision", "SUB-4"), ("question", "SUB-5")] {
        let w = refused(c.submit(agent(
            &format!(
                r#"MATCH (r: ws/region {{ id: $r }})
                   IMPORT (n: ws/{ty} {{ id: $n, handle: "h" }})
                   CREATE (n) -[in_region]-> (r)"#
            ),
            &[("r", json!("R-1")), ("n", json!(id))],
        )));
        assert_eq!(grammar_fault_tag(&w), Some("id_prefix_mismatch"), "{ty} {id}: {w:?}");
        assert!(!exists(&c, id));
    }
}

/// Guard 4: new regions still mint `R-`; the counters stay apart.
/// After importing `R-5` and `SUB-9` in ONE statement, the counters read
/// `R = 5`, `SUB = 9` (a carried id advances the counter of ITS OWN prefix),
/// and a CREATE region mints `R-6` — never `SUB-*`, never `R-10`.
// RED (a): mint from the last-carried prefix — the CREATE minted `SUB-10`.
// RED (b): advance the `R` counter by the `SUB` serial — the CREATE minted
// `R-10`. Each fails its own assertion below.
#[test]
fn new_regions_mint_r_and_the_counters_stay_apart() {
    let dir = root("sub-counters");
    let c = fresh(&dir);
    c.submit(agent(
        r#"IMPORT (a: ws/region { id: $a, handle: "carried r", boundary: $b })
           IMPORT (s: ws/region { id: $s, handle: "carried sub", boundary: $b })"#,
        &[("a", json!("R-5")), ("s", json!("SUB-9")), ("b", json!(BOUNDARY))],
    ))
    .expect("R-5 and SUB-9 import");
    let s = serials(&dir);
    assert_eq!(s.get("R"), Some(&5), "{s:?}");
    assert_eq!(s.get("SUB"), Some(&9), "{s:?}");
    let minted = create_region(&c);
    assert!(!minted.starts_with("SUB-"), "a region never MINTS the legacy prefix: {minted}");
    assert_ne!(minted, "R-10", "the R counter was advanced by the SUB serial");
    assert_eq!(minted, "R-6");
    assert_eq!(serials(&dir).get("SUB"), Some(&9), "a mint leaves the SUB counter alone");
}

fn plain_grammar() -> PoolGrammar {
    PoolGrammar {
        pool: PoolName::new(PLAIN),
        kind: PoolKind::new("plain"),
        version: 1,
        node_types: BTreeMap::from([(
            "note".to_string(),
            NodeTypeDef {
                fields: BTreeMap::from([("name".to_string(), FieldDef::text(true, 40))]),
                ..Default::default()
            },
        )]),
        edge_kinds: BTreeMap::new(),
    }
}

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-import-form-{name}-{}", std::process::id()))
}

fn fresh(dir: &Path) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(dir, &[workspace_v6(WS), plain_grammar()], SpineConfig::default())
        .expect("create the cauldron")
}

fn agent_initiator() -> Initiator {
    Initiator::Agent {
        thread: "import-form".into(),
        dispatch: "import-form".into(),
    }
}

fn agent(statement: &str, params: &[(&str, Value)]) -> Envelope {
    let mut bindings = Bindings::new();
    for (k, v) in params {
        bindings.insert((*k).to_string(), v.clone());
    }
    Envelope {
        statement: statement.to_string(),
        bindings,
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: agent_initiator(),
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn seed_region(c: &Cauldron, id: &str) {
    c.submit(agent(
        r#"IMPORT (r: ws/region { id: $r, handle: "seed", boundary: $b })"#,
        &[("r", json!(id)), ("b", json!(BOUNDARY))],
    ))
    .unwrap_or_else(|e| panic!("seed region {id}: {e:?}"));
}

fn import_question(c: &Cauldron, id: &str, region: &str) -> spine::error::Result<Outcome> {
    c.submit(agent(
        r#"MATCH (r: ws/region { id: $r })
           IMPORT (q: ws/question { id: $q, handle: "imported" })
           CREATE (q) -[in_region]-> (r)
           RETURN q.id"#,
        &[("r", json!(region)), ("q", json!(id))],
    ))
}

fn create_question(c: &Cauldron, region: &str, handle: &str) -> String {
    let out = c
        .submit(agent(
            r#"MATCH (r: ws/region { id: $r })
               CREATE (q: ws/question { handle: $h })
               CREATE (q) -[in_region]-> (r)
               RETURN q.id"#,
            &[("r", json!(region)), ("h", json!(handle))],
        ))
        .unwrap_or_else(|e| panic!("create question: {e:?}"));
    first_row(&out)["q.id"].as_str().expect("id").to_string()
}

fn first_row(out: &Outcome) -> BTreeMap<String, Value> {
    match out {
        Outcome::Committed { returned, .. } => returned.0[0].clone(),
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn refused<T: std::fmt::Debug>(r: spine::error::Result<T>) -> Witness {
    match r {
        Err(SpineError::Refused { witnesses }) => {
            assert_eq!(witnesses.len(), 1, "one refusal, one witness: {witnesses:?}");
            witnesses[0].clone()
        }
        Err(other) => panic!("expected a structured refusal, got {other:?}"),
        Ok(v) => panic!("expected a refusal, got {v:?}"),
    }
}

fn view(c: &Cauldron, id: &str) -> spine::read::NodeView {
    c.node_view(&QualifiedNodeId {
        pool: PoolName::new(WS),
        id: NodeId(id.into()),
    })
    .expect("node_view")
    .unwrap_or_else(|| panic!("node {id} is in the pool"))
}

fn exists(c: &Cauldron, id: &str) -> bool {
    c.node_view(&QualifiedNodeId {
        pool: PoolName::new(WS),
        id: NodeId(id.into()),
    })
    .expect("node_view")
    .is_some()
}

/// The `id_serials` rows for pool `ws`, straight from the spine file (the
/// counter is a spine table; no door reads it).
fn serials(dir: &Path) -> BTreeMap<String, i64> {
    let conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("spine");
    let has: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'id_serials'",
            [],
            |r| r.get(0),
        )
        .expect("schema");
    if has == 0 {
        return BTreeMap::new();
    }
    let mut st = conn
        .prepare("SELECT prefix, last FROM id_serials WHERE pool = ?1")
        .expect("prepare");
    let rows = st
        .query_map([WS], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    rows
}
