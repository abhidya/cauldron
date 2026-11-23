//! The ladle layer, proven: the ladle parser, the staged-coverage table,
//! the claim/footprint derivation, the clause-to-delta compiler, the fuel
//! bound, the refusal witnesses, and the envelope's gates.
//!
//! COMPILE IS PURE. Nothing here opens a cauldron, locks anything or lands
//! anything: match sets are constructed by hand, grammars are the cross-schema
//! fixtures, and every assertion is over a value the compiler returned.
//! That is deliberate — `exec.rs` and the pipeline are tested elsewhere.
//!
//! One proving rule governs this file: **every checker below has
//! been forced RED at least once**, and each one names, in a comment, the
//! poison that made it fire. Assertions are
//! STRUCTURAL — on witness variants, delta lists and claim sets — never on
//! prose.

use spine::config::SpineConfig;
use spine::delta::{CompiledPayload, CountOp, Delta, Premise};
use spine::envelope::{
    Bindings, Envelope, Initiator, Instrument, Mode, Provenance, TxHash, ENVELOPE_FORMAT_VERSION,
};
use spine::error::SpineError;
use spine::ladle::ast::*;
use spine::ladle::compile::{
    self, claim_footprint, claims_for, compile, footprint, mint_node_id, project_returns,
    CompileCtx, Footprint, Fuel, EDGE_EXISTENCE_FORM, FIELD_VALUE_FORM, MULTI_HOP_FORM,
    SCALAR_FUNCTION_FORM, STAGED_OUT_FORMS,
};
use spine::ladle::parse::parse;
use spine::locks::{Claim, ClaimSet, LockMode, Resource};
use spine::pool::{FieldValue, NodeId, NodeRecord, QualifiedNodeId};
use spine::read::{BoundNode, MatchSet};
use spine::schema_def::fixtures::{fixture_alpha, fixture_beta};
use spine::schema_def::{grammar_fault_tag, Direction, PoolGrammar, PoolName};
use spine::witness::{ExpectationKind, Witness};
use std::collections::{BTreeMap, BTreeSet};

test_scratch::install!();

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

const ALPHA: &str = "fixture-alpha";
const BETA: &str = "fixture-beta";

fn pool(name: &str) -> PoolName {
    PoolName::new(name)
}

/// The cross-schema fixture grammars, as the compiler's lookup.
fn grammars(p: &PoolName) -> Option<PoolGrammar> {
    match p.as_str() {
        ALPHA => Some(fixture_alpha()),
        BETA => Some(fixture_beta()),
        _ => None,
    }
}

fn parse_ok(text: &str) -> Statement {
    parse(text).unwrap_or_else(|e| panic!("expected `{text}` to parse; got: {}", e.message()))
}

/// The witnesses of a refusal — the machine contract every assertion below
/// reads.
fn witnesses(e: &SpineError) -> Vec<Witness> {
    match e {
        SpineError::Refused { witnesses } => witnesses.clone(),
        other => panic!("expected a structured refusal, got: {other:?}"),
    }
}

fn only_witness(e: &SpineError) -> Witness {
    let w = witnesses(e);
    assert_eq!(w.len(), 1, "refusal carries exactly one witness: {w:?}");
    w.into_iter().next().expect("one witness")
}

/// Assert the error is a refusal naming exactly this staged-out form.
fn assert_unimplemented(e: &SpineError, want: &str) {
    match only_witness(e) {
        Witness::UnimplementedForm { form } => assert_eq!(form, want),
        other => panic!("expected UnimplementedForm({want}), got: {other:?}"),
    }
}

fn field(value: serde_json::Value, rev: i64) -> FieldValue {
    FieldValue { value, rev }
}

fn record(
    id: &str,
    node_type: &str,
    state: Option<&str>,
    rev: i64,
    fields: &[(&str, serde_json::Value, i64)],
) -> NodeRecord {
    NodeRecord {
        id: NodeId(id.to_string()),
        node_type: node_type.to_string(),
        state: state.map(str::to_string),
        rev,
        schema_version: 1,
        sealed: false,
        fields: fields
            .iter()
            .map(|(n, v, r)| ((*n).to_string(), field(v.clone(), *r)))
            .collect(),
    }
}

fn bound(pool_name: &str, node: NodeRecord) -> BoundNode {
    BoundNode {
        pool: pool(pool_name),
        node,
    }
}

fn matchset(entries: &[(&str, Vec<BoundNode>)]) -> MatchSet {
    MatchSet {
        bindings: entries
            .iter()
            .map(|(b, v)| ((*b).to_string(), v.clone()))
            .collect(),
    }
}

fn params(entries: &[(&str, serde_json::Value)]) -> Bindings {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

/// A tank big enough that no test hits the ceiling by accident; the fuel
/// tests use their own small tanks deliberately.
fn tank() -> Fuel {
    Fuel::new(SpineConfig::default().fuel_ceiling)
}

fn hash(byte: u8) -> TxHash {
    TxHash([byte; 32])
}

fn compile_with(
    stmt: &Statement,
    p: &Bindings,
    matched: &MatchSet,
    h: &TxHash,
    fuel: &mut Fuel,
) -> Result<CompiledPayload, SpineError> {
    let g: &dyn Fn(&PoolName) -> Option<PoolGrammar> = &grammars;
    let kinds = |k: &spine::schema_def::PoolKind| -> Vec<PoolGrammar> {
        [fixture_alpha(), fixture_beta()]
            .into_iter()
            .filter(|g| g.kind == *k)
            .collect()
    };
    let mut ctx = CompileCtx {
        grammars: g,
        pools_of_kind: &kinds,
        fuel,
    };
    compile(stmt, p, matched, h, &mut ctx)
}

fn claim(pool_name: &str, resource: Resource, mode: LockMode) -> Claim {
    Claim {
        pool: pool(pool_name),
        resource,
        mode,
    }
}

fn claim_set(claims: &[Claim]) -> ClaimSet {
    let mut s = ClaimSet::new();
    for c in claims {
        s.insert(c.clone());
    }
    s
}

fn body(id: &str) -> Resource {
    Resource::Body {
        node: NodeId(id.to_string()),
    }
}

fn tslice(node_type: &str) -> Resource {
    Resource::TypeSlice {
        node_type: node_type.to_string(),
    }
}

fn sslice(node_type: &str, state: &str) -> Resource {
    Resource::StateSlice {
        node_type: node_type.into(),
        state: state.into(),
    }
}

fn eslice(node: &str, kind: &str, direction: Direction) -> Resource {
    Resource::EdgeSlice {
        node: NodeId(node.to_string()),
        kind: kind.to_string(),
        direction,
    }
}

fn envelope(statement: &str, bindings: Bindings) -> Envelope {
    Envelope {
        statement: statement.to_string(),
        bindings,
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent {
                thread: "t".to_string(),
                dispatch: "d".to_string(),
            },
            instrument: Instrument {
                name: "i".to_string(),
                version: "1".to_string(),
            },
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

// ---------------------------------------------------------------------------
// 1. Parser round-trips — every STAGED clause form
// ---------------------------------------------------------------------------

// RED: dropping the `id` field-map entry from the expected pattern (the
// assertion fired on a `fields: []` mismatch), and separately by feeding the
// parser `MATCH (old: doc { id: $x })`, which refuses instead of parsing.
#[test]
fn parses_match_by_identifier() {
    let s = parse_ok("MATCH (old: fixture-alpha/doc { id: $old_id })");
    assert_eq!(s.matches.len(), 1);
    let MatchClause::Node(h) = &s.matches[0] else {
        panic!("expected a node head, got {:?}", s.matches[0]);
    };
    assert_eq!(h.binding, Binding("old".into()));
    assert_eq!(h.pool, pool(ALPHA));
    assert_eq!(h.node_type, "doc");
    assert_eq!(
        h.fields,
        vec![("id".to_string(), ValueExpr::Param("old_id".into()))]
    );
    assert!(s.wheres.is_empty() && s.expects.is_empty() && s.mutations.is_empty());
    assert!(s.returns.is_none());
    assert!(s.is_read_only(), "a bare MATCH mutates nothing");
}

// RED: asserting `state` parsed as a Param rather than a literal.
#[test]
fn parses_match_by_pool_type_state() {
    let s = parse_ok(r#"MATCH (d: fixture-alpha/doc { state: "draft" })"#);
    let MatchClause::Node(h) = &s.matches[0] else {
        panic!("expected a node head");
    };
    assert_eq!(h.node_type, "doc");
    assert_eq!(
        h.field("state"),
        Some(&ValueExpr::Literal(serde_json::json!("draft")))
    );
    // Pool + type alone, with no field map at all, is the same form.
    let bare = parse_ok("MATCH (d: fixture-alpha/doc)");
    let MatchClause::Node(h) = &bare.matches[0] else {
        panic!("expected a node head");
    };
    assert!(h.fields.is_empty());
}

// RED: asserting `ArrowDir::LeftToRight` for the `<-[..]-` spelling — the
// direction field is the thing that stops the compiler inverting an edge.
#[test]
fn parses_single_edge_hop_both_directions() {
    let fwd = parse_ok("MATCH (a: fixture-alpha/author) -[wrote]-> (d: fixture-alpha/doc)");
    let MatchClause::Hop(h) = &fwd.matches[0] else {
        panic!("expected a hop");
    };
    assert_eq!(h.kind, "wrote");
    assert_eq!(h.arrow, ArrowDir::LeftToRight);
    assert!(matches!(h.left, PatternEnd::Head(_)));
    assert!(matches!(h.right, PatternEnd::Head(_)));

    // The backward spelling, with a BOUND end — `(d)` inherits the pool its
    // head qualified it with (frozen semantics).
    let back = parse_ok(
        "MATCH (d: fixture-alpha/doc) MATCH (d) <-[wrote]- (a: fixture-alpha/author)",
    );
    let MatchClause::Hop(h) = &back.matches[1] else {
        panic!("expected a hop");
    };
    assert_eq!(h.arrow, ArrowDir::RightToLeft);
    assert_eq!(h.left, PatternEnd::Bound(Binding("d".into())));
}

// RED: asserting `field: None` on the per-field form.
#[test]
fn parses_expect_whole_revision_value_and_count() {
    let s = parse_ok(
        "MATCH (d: fixture-alpha/doc) \
         EXPECT REV(d) = 12 \
         EXPECT REV(d.title) = 3 \
         EXPECT d.state = \"draft\" \
         EXPECT COUNT(d) = 1",
    );
    assert_eq!(s.expects.len(), 4);
    assert_eq!(
        s.expects[0],
        Expectation::Revision {
            binding: Binding("d".into()),
            field: None,
            expected: ValueExpr::Literal(serde_json::json!(12)),
        }
    );
    assert_eq!(
        s.expects[1],
        Expectation::Revision {
            binding: Binding("d".into()),
            field: Some("title".into()),
            expected: ValueExpr::Literal(serde_json::json!(3)),
        }
    );
    assert_eq!(
        s.expects[2],
        Expectation::Value {
            field: FieldRef {
                binding: Binding("d".into()),
                path: vec!["state".into()]
            },
            expected: ValueExpr::Literal(serde_json::json!("draft")),
        }
    );
    assert_eq!(
        s.expects[3],
        Expectation::Count {
            binding: Binding("d".into()),
            op: CountOp::Exactly,
            expected: ValueExpr::Literal(serde_json::json!(1)),
        }
    );
    let at_most = parse_ok("MATCH (d: fixture-alpha/doc) EXPECT COUNT(d) <= $cap");
    assert_eq!(
        at_most.expects[0],
        Expectation::Count {
            binding: Binding("d".into()),
            op: CountOp::AtMost,
            expected: ValueExpr::Param("cap".into()),
        }
    );
}

// RED: asserting the NOTE survives as `None`.
#[test]
fn parses_create_set_and_remove_whole() {
    let s = parse_ok(
        r#"MATCH (old: fixture-alpha/doc { id: $old_id })
           CREATE (new: fixture-alpha/doc { title: $title, state: "draft" })
           CREATE (new) -[supersedes]-> (old) NOTE "why"
           SET old.state = "superseded"
           SET old.meta.tag = 7
           REMOVE (new) -[current_of]-> (old)"#,
    );
    assert_eq!(s.mutations.len(), 5);
    let Mutation::CreateNode(h) = &s.mutations[0] else {
        panic!("expected CREATE node");
    };
    assert_eq!(h.binding, Binding("new".into()));
    assert_eq!(h.fields.len(), 2);
    assert_eq!(
        s.mutations[1],
        Mutation::CreateEdge {
            from: Binding("new".into()),
            kind: "supersedes".into(),
            to: Binding("old".into()),
            arrow: ArrowDir::LeftToRight,
            note: Some(ValueExpr::Literal(serde_json::json!("why"))),
        }
    );
    assert_eq!(
        s.mutations[2],
        Mutation::Set {
            target: FieldRef {
                binding: Binding("old".into()),
                path: vec!["state".into()]
            },
            value: ValueExpr::Literal(serde_json::json!("superseded")),
        }
    );
    // Dotted keys address into the metadata object (SET).
    let Mutation::Set { target, .. } = &s.mutations[3] else {
        panic!("expected SET");
    };
    assert_eq!(target.path, vec!["meta".to_string(), "tag".to_string()]);
    assert_eq!(
        s.mutations[4],
        Mutation::RemoveEdge {
            from: Binding("new".into()),
            kind: "current_of".into(),
            to: Binding("old".into()),
            arrow: ArrowDir::LeftToRight,
        }
    );
    assert!(!s.is_read_only());
}

/// REMOVE: **edges only — nodes are never removed.** The law holds
/// BY CONSTRUCTION (there is no node-removal shape in [`Mutation`], and
/// `remove_clause` demands an arrow), but a law that goes red nowhere is a
/// law nobody notices breaking. This is where it goes red.
///
/// Every plausible node-removal spelling refuses, and the AST is asserted to
/// carry no node-removal variant at all — so growing one, or relaxing
/// `remove_clause` into an optional arrow, fails here rather than silently
/// shipping a delete path into a store whose whole point is that the past is
/// never unwritten.
// RED: making `remove_clause`'s arrow optional (`if self.at_arrow() { .. }`,
// falling back to a whole-node removal) — every spelling below then parsed,
// and the exhaustiveness arm below stopped compiling once a `RemoveNode`
// variant existed to make it meaningful.
#[test]
fn a_node_can_never_be_removed_in_any_spelling() {
    for text in [
        // Parenthesised, after a match — the spelling a Cypher habit reaches
        // for first.
        r#"MATCH (d: fixture-alpha/doc { id: $id }) REMOVE (d)"#,
        // Bare binding.
        r#"MATCH (d: fixture-alpha/doc { id: $id }) REMOVE d"#,
        // Re-qualified head, as if REMOVE took a pattern.
        r#"MATCH (d: fixture-alpha/doc { id: $id }) REMOVE (d: fixture-alpha/doc)"#,
        // A freshly minted node, removed in the same breath.
        r#"CREATE (n: fixture-alpha/doc { title: $t, state: "draft" }) REMOVE (n)"#,
    ] {
        let err = parse(text)
            .err()
            .unwrap_or_else(|| panic!("`{text}` must refuse: REMOVE is edges only"));
        // WHATEVER the refusal is, it is a refusal and nothing landed in the
        // AST — this test asserts the law, not one message.
        match only_witness(&err) {
            Witness::MalformedStatement { .. } | Witness::UnimplementedForm { .. } => {}
            other => panic!("`{text}`: expected a structured refusal, got {other:?}"),
        }
    }

    // The STAGED-IN spelling is the edge form, and it is the ONLY one.
    let ok = parse_ok(
        r#"MATCH (a: fixture-alpha/author) MATCH (d: fixture-alpha/doc)
           REMOVE (d) -[current_of]-> (a)"#,
    );
    assert_eq!(ok.mutations.len(), 1);
    // EXHAUSTIVE over the mutation vocabulary: every arm the AST admits, and
    // the only removal among them takes two endpoints and an edge kind.
    match &ok.mutations[0] {
        Mutation::RemoveEdge { from, kind, to, .. } => {
            assert_eq!(from, &Binding("d".into()));
            assert_eq!(kind, "current_of");
            assert_eq!(to, &Binding("a".into()));
        }
        Mutation::CreateNode(_)
        | Mutation::CreateEdge { .. }
        | Mutation::Set { .. }
        | Mutation::Seal { .. }
        | Mutation::ImportNode(_) => {
            panic!("expected the edge-removal form")
        }
    }
}

// RED: asserting `CmpOp::Eq` for the `!=` leaf.
#[test]
fn parses_where_over_bound_fields() {
    let s = parse_ok(
        r#"MATCH (d: fixture-alpha/doc)
           WHERE d.title != "x" AND (d.rank >= 3 OR NOT d.rank < 0)"#,
    );
    assert_eq!(s.wheres.len(), 1);
    let Condition::And(parts) = &s.wheres[0] else {
        panic!("expected a conjunction, got {:?}", s.wheres[0]);
    };
    assert_eq!(parts.len(), 2);
    assert!(matches!(
        parts[0],
        Condition::Compare { op: CmpOp::Ne, .. }
    ));
    let Condition::Or(alts) = &parts[1] else {
        panic!("expected a disjunction");
    };
    assert!(matches!(alts[0], Condition::Compare { op: CmpOp::Ge, .. }));
    assert!(matches!(alts[1], Condition::Not(_)));
    // The canonical rendering is what the FailedCondition witness carries.
    assert_eq!(
        s.wheres[0].to_string(),
        r#"(d.title != "x" AND (d.rank >= 3 OR NOT (d.rank < 0)))"#
    );
}

// RED: asserting the un-aliased column name is `"id"` rather than the item's
// source text `"new.id"`.
#[test]
fn parses_return_projections_and_column_names() {
    let s = parse_ok(
        "MATCH (old: fixture-alpha/doc) \
         CREATE (new: fixture-alpha/doc { title: $t }) \
         RETURN new.id AS new_id, REV(old), old.title, REV(old.title)",
    );
    let r = s.returns.as_ref().expect("a RETURN clause");
    assert_eq!(r.items.len(), 4);
    assert_eq!(r.items[0].column_name(), "new_id");
    assert_eq!(r.items[1].column_name(), "REV(old)");
    assert_eq!(r.items[2].column_name(), "old.title");
    assert_eq!(r.items[3].column_name(), "REV(old.title)");
    assert_eq!(
        r.items[1].expr,
        ReturnExpr::Rev {
            binding: Binding("old".into()),
            field: None
        }
    );
}

// RED: asserting an unknown escape parses (it refuses), and asserting a
// bare word is a value (it refuses).
#[test]
fn parses_every_frozen_value_form() {
    let s = parse_ok(
        r#"MATCH (d: fixture-alpha/doc { a: "s\"q\\\n\t", b: 12, c: -3, d: 1.5, e: true, f: false, g: null, h: $p })"#,
    );
    let MatchClause::Node(h) = &s.matches[0] else {
        panic!("head");
    };
    let v: Vec<&ValueExpr> = h.fields.iter().map(|(_, v)| v).collect();
    assert_eq!(*v[0], ValueExpr::Literal(serde_json::json!("s\"q\\\n\t")));
    assert_eq!(*v[1], ValueExpr::Literal(serde_json::json!(12)));
    assert_eq!(*v[2], ValueExpr::Literal(serde_json::json!(-3)));
    assert_eq!(*v[3], ValueExpr::Literal(serde_json::json!(1.5)));
    assert_eq!(*v[4], ValueExpr::Literal(serde_json::json!(true)));
    assert_eq!(*v[5], ValueExpr::Literal(serde_json::json!(false)));
    assert_eq!(*v[6], ValueExpr::Literal(serde_json::Value::Null));
    assert_eq!(*v[7], ValueExpr::Param("p".into()));
}

/// The whole worked example, verbatim from the frozen module docs.
// RED: renaming `supersedes` to `supersede` in the expected AST.
#[test]
fn parses_the_frozen_worked_example() {
    let s = parse_ok(
        r#"MATCH (old: fixture-alpha/doc { id: $old_id })
           EXPECT REV(old) = 12
           CREATE (new: fixture-alpha/doc { title: $title, state: "draft" })
           CREATE (new) -[supersedes]-> (old)
           SET old.state = "superseded"
           RETURN new.id AS new_id, REV(old)"#,
    );
    assert_eq!(s.matches.len(), 1);
    assert_eq!(s.expects.len(), 1);
    assert_eq!(s.mutations.len(), 3);
    assert_eq!(s.returns.as_ref().unwrap().items.len(), 2);
    assert_eq!(
        s.referenced_params(),
        BTreeSet::from(["old_id".to_string(), "title".to_string()])
    );
    assert_eq!(
        s.binding_types(),
        BTreeMap::from([
            ("old".to_string(), (pool(ALPHA), "doc".to_string())),
            ("new".to_string(), (pool(ALPHA), "doc".to_string())),
        ])
    );
    assert_eq!(
        s.matched_bindings(),
        BTreeSet::from(["old".to_string()]),
        "the created binding is not a matched one"
    );
}

// ---------------------------------------------------------------------------
// 2. Refusal BY NAME — grammatical-but-staged, and the pool-qualification rule
//    (a dialect misreads, a coverage gap refuses)
// ---------------------------------------------------------------------------

/// UNSTAGED FORM 1 — a two-hop path, in BOTH of its spellings.
// RED twice: deleting the parser's chained-arrow refusal (spelling A fell
// back to "expected a clause keyword", a syntax error, which is exactly the
// misread the staging rule forbids), and deleting the coverage row (spelling B compiled).
#[test]
fn two_hop_path_refuses_by_name_in_both_spellings() {
    // Spelling A: chained in one clause. Longer than the frozen EBNF's `hop`,
    // so only the parser can see it — and it refuses BY NAME, never as a
    // syntax error, never keeping a partial hop.
    let err = parse(
        "MATCH (a: fixture-alpha/author) -[wrote]-> (d: fixture-alpha/doc) -[supersedes]-> (e: fixture-alpha/doc)",
    )
    .expect_err("a two-hop path is grammatical-but-staged");
    assert_unimplemented(&err, MULTI_HOP_FORM);

    // Spelling B: two hop clauses sharing a binding. Fully grammatical, so it
    // PARSES whole and the coverage table refuses it by the same name.
    let stmt = parse_ok(
        "MATCH (a: fixture-alpha/author) -[wrote]-> (d: fixture-alpha/doc) \
         MATCH (d) -[supersedes]-> (e: fixture-alpha/doc)",
    );
    let err = compile::check_coverage(&stmt).expect_err("multi-hop is staged out");
    assert_unimplemented(&err, MULTI_HOP_FORM);
}

/// UNSTAGED FORM 2 — a match on a field that is neither `id` nor `state`.
// RED: changing the detector to `f != FIELD_ID` only, which let a `state`
// match refuse (it must not) while still catching this one.
#[test]
fn field_value_match_refuses_by_name() {
    let stmt = parse_ok(r#"MATCH (d: fixture-alpha/doc { title: "x" })"#);
    let err = compile::check_coverage(&stmt).expect_err("field-value match is staged out");
    assert_unimplemented(&err, FIELD_VALUE_FORM);

    // ... and the two STAGED-IN field-map keys are not caught by it.
    compile::check_coverage(&parse_ok(r#"MATCH (d: fixture-alpha/doc { id: $i })"#))
        .expect("match by identifier is staged IN");
    compile::check_coverage(&parse_ok(r#"MATCH (d: fixture-alpha/doc { state: "draft" })"#))
        .expect("match by pool+type+state is staged IN");
}

/// UNSTAGED FORM 3 — the edge-existence condition (`EXISTS` / `NOT EXISTS`),
/// which the grammar names but the staged list ("the condition clause over bound
/// FIELDS") excludes. Flipping it is one row.
// RED: deleting the `where:edge-existence` coverage row — the gate went
// silent and the `expect_err` fired. (The compiler is the second gate on the
// same form: `eval_condition` refuses an `EXISTS` by the same name rather
// than treating it as a quietly-true premise, so a direct caller who skips
// `check_coverage` still gets a named refusal, never a misread.)
#[test]
fn edge_existence_condition_refuses_by_name() {
    let stmt = parse_ok(
        "MATCH (a: fixture-alpha/author) MATCH (d: fixture-alpha/doc) \
         WHERE NOT EXISTS (a) -[wrote]-> (d)",
    );
    let err = compile::check_coverage(&stmt).expect_err("edge existence is staged out");
    assert_unimplemented(&err, EDGE_EXISTENCE_FORM);

    // The `()` any-endpoint spelling is the same form.
    let any = parse_ok("MATCH (a: fixture-alpha/author) WHERE EXISTS (a) -[wrote]-> ()");
    assert_unimplemented(
        &compile::check_coverage(&any).expect_err("staged out"),
        EDGE_EXISTENCE_FORM,
    );
}

/// UNSTAGED FORM 4 — a SCALAR FUNCTION call, which the grammar names among
/// WHERE's parameters and the staged list ("the condition clause over
/// bound FIELDS") excludes. It has no AST shape at all, so the PARSER is the
/// only gate that can see it, and it refuses BY NAME
/// ([`SCALAR_FUNCTION_FORM`]) in BOTH positions the grammar admits a call:
/// the condition head and the value position.
// RED: this is the whole point of the guard — before it, `WHERE len(d.title)
// > 3` fell out of the parser as `"`len` is a bare word, not a value"` /
// `"expected a comparison operator"`, i.e. a bare SYNTAX ERROR, which is
// exactly the misread the staging rule forbids for a form the grammar names. Deleting
// either `matches!(..., Tok::LParen)` arm in `parse.rs` (`unary` for the
// condition head, `value` for the value position) turns this red again.
#[test]
fn a_scalar_function_call_refuses_by_name_in_condition_and_value_position() {
    // POSITION ONE — the condition head.
    let err = parse("MATCH (d: fixture-alpha/doc) WHERE len(d.title) > 3")
        .expect_err("a scalar function in a condition head is grammatical-but-staged");
    assert_unimplemented(&err, SCALAR_FUNCTION_FORM);

    // POSITION TWO — the value side of a comparison.
    let err = parse("MATCH (d: fixture-alpha/doc) WHERE d.title = len($x)")
        .expect_err("a scalar function in a value position is grammatical-but-staged");
    assert_unimplemented(&err, SCALAR_FUNCTION_FORM);

    // The value position is the same wherever the grammar takes a value —
    // a field map and a SET both reach `value()`, so both name the form.
    for text in [
        r#"CREATE (d: fixture-alpha/doc { title: upper($t), state: "draft" })"#,
        r#"MATCH (d: fixture-alpha/doc) SET d.title = concat($a, $b)"#,
    ] {
        let err = parse(text)
            .err()
            .unwrap_or_else(|| panic!("`{text}` must refuse"));
        assert_unimplemented(&err, SCALAR_FUNCTION_FORM);
    }

    // The frozen name is NOT a coverage-table row: no detector could fire on
    // a parsed statement, because parsing never produces one. A row that can
    // never fire would be a lie.
    assert!(
        !STAGED_OUT_FORMS.contains(&SCALAR_FUNCTION_FORM),
        "the scalar-function form is parser-refused, not table-detected"
    );

    // And a BARE word in the same positions is still an ordinary syntax
    // error — the guard must key on the `(`, not on the identifier.
    let bare = parse(r#"MATCH (d: fixture-alpha/doc) WHERE d.title = unquoted"#)
        .expect_err("a bare word is not a value");
    match only_witness(&bare) {
        Witness::MalformedStatement { message, .. } => {
            assert!(message.contains("bare word"), "got: {message}")
        }
        other => panic!("expected MalformedStatement, got {other:?}"),
    }
}

/// The frozen names are a closed, asserted-on list.
// RED: adding a fourth entry to `STAGED_OUT_FORMS` without a table row.
#[test]
fn every_staged_out_name_has_a_coverage_row() {
    let rows: Vec<&str> = compile::COVERAGE_TABLE.iter().map(|r| r.form).collect();
    assert_eq!(rows, STAGED_OUT_FORMS.to_vec());
    assert_eq!(
        STAGED_OUT_FORMS,
        &[
            compile::PROJECTED_PATTERN_FORM,
            FIELD_VALUE_FORM,
            MULTI_HOP_FORM,
            EDGE_EXISTENCE_FORM
        ]
    );
}

/// Pool qualification is required on EVERY pattern head, and its
/// absence refuses NAMING THE RULE — not as a coverage gap, because a head
/// with no pool names no lock domain at all, in this build or any later one.
// RED: making `pattern_end` default the pool to the previous head's — the
// statement then parsed and the assertion fired on the missing refusal.
#[test]
fn pattern_head_without_a_pool_refuses_naming_the_rule() {
    for text in [
        "MATCH (d: doc)",
        r#"MATCH (d: doc { id: $x })"#,
        "MATCH (a: fixture-alpha/author) -[wrote]-> (d: doc)",
        "CREATE (n: doc { title: $t })",
    ] {
        let err = parse(text).expect_err("a head with no pool refuses");
        match only_witness(&err) {
            Witness::MalformedStatement { message, .. } => assert!(
                message.contains("pool qualification is required on every pattern head"),
                "the refusal must NAME the rule; got: {message}"
            ),
            other => panic!("expected MalformedStatement, got: {other:?}"),
        }
    }
}

/// Shapes the frozen grammar does not admit AT ALL. These are genuine syntax
/// errors, and each one is a rule the grammar states.
// RED: allowing a second RETURN (removing the `returns.is_some()` guard).
#[test]
fn shapes_outside_the_frozen_grammar_are_malformed() {
    let cases = [
        ("", "empty statement"),
        ("MATCH (d: fixture-alpha/doc) SET d.state = \"draft\" MATCH (e: fixture-alpha/doc)", "out of order"),
        ("MATCH (d: fixture-alpha/doc) RETURN d.id RETURN d.state", "at most one RETURN"),
        ("MATCH (d: fixture-alpha/doc) MATCH (d: fixture-alpha/doc)", "introduced twice"),
        ("MATCH (a: fixture-alpha/author) -[wrote]-> (nope)", "used before it is bound"),
        ("MATCH (d: fixture-alpha/doc { title: unquoted })", "bare word"),
        ("MATCH (d: fixture-alpha/doc) EXPECT REV(d) = \"twelve\"", "integer"),
        ("MATCH (d: fixture-alpha/doc { a: \"unterminated )", "unterminated"),
        ("MATCH (d: fixture-alpha/doc { a: \"\\q\" })", "unknown string escape"),
        ("Match (d: fixture-alpha/doc)", "mixed"),
        ("MATCH (D: fixture-alpha/doc)", "expected a binding name"),
        ("MATCH (d: _pool/doc)", "not a legal pool name"),
        ("MATCH (d: 1pool/doc)", "expected a pool name"),
        ("MATCH (d: fixture-alpha/doc) EXPECT COUNT(d) >= 1", "EXPECT COUNT takes"),
        ("MATCH (d: fixture-alpha/doc) WHERE d.a ~ 1", "unexpected character"),
    ];
    for (text, needle) in cases {
        let err = parse(text).err().unwrap_or_else(|| panic!("`{text}` must refuse"));
        match only_witness(&err) {
            Witness::MalformedStatement { message, .. } => assert!(
                message.contains(needle),
                "`{text}`: expected a message naming `{needle}`, got `{message}`"
            ),
            other => panic!("`{text}`: expected MalformedStatement, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// 3. The compile table — the worked supersede example, delta list asserted EXACTLY
// ---------------------------------------------------------------------------

const SUPERSEDE: &str = r#"MATCH (old: fixture-alpha/doc { id: $old_id })
EXPECT REV(old) = 12
CREATE (new: fixture-alpha/doc { title: $title, state: "draft" })
CREATE (new) -[supersedes]-> (old)
SET old.state = "superseded"
RETURN new.id AS new_id, REV(old)"#;

fn supersede_match() -> MatchSet {
    matchset(&[(
        "old",
        vec![bound(
            ALPHA,
            record(
                "d-40",
                "doc",
                Some("published"),
                12,
                &[("title", serde_json::json!("first"), 4)],
            ),
        )],
    )])
}

fn supersede_params() -> Bindings {
    params(&[
        ("old_id", serde_json::json!("d-40")),
        ("title", serde_json::json!("second")),
    ])
}

// RED: swapping the two CREATE clauses in the statement text — the assertion
// fired on delta order, which is what proves mutations compile in SOURCE order.
#[test]
fn supersede_compiles_to_the_exact_delta_list() {
    let stmt = parse_ok(SUPERSEDE);
    compile::check_coverage(&stmt).expect("every clause is staged in");
    let h = hash(0xA1);
    let minted = mint_node_id(&h, 0);
    let payload = compile_with(
        &stmt,
        &supersede_params(),
        &supersede_match(),
        &h,
        &mut tank(),
    )
    .expect("the worked example compiles");

    assert_eq!(payload.parts.len(), 1, "one touched pool, one part");
    let part = &payload.parts[0];
    assert_eq!(part.pool, pool(ALPHA));
    assert_eq!(part.schema_version, 1, "alpha is pinned at 1");

    assert_eq!(
        part.deltas,
        vec![
            Delta::CreateNode {
                id: minted.clone(),
                node_type: "doc".into(),
                state: Some("draft".into()),
                fields: BTreeMap::from([("title".to_string(), serde_json::json!("second"))]),
            },
            Delta::AddEdge {
                src: minted.clone(),
                kind: "supersedes".into(),
                dst: QualifiedNodeId {
                    pool: pool(ALPHA),
                    id: NodeId("d-40".into())
                },
                note: None,
                fields: Default::default(),
            },
            Delta::SetField {
                node: NodeId("d-40".into()),
                path: vec!["state".into()],
                value: serde_json::json!("superseded"),
            },
        ],
        "the four-delta vocabulary, in source order"
    );

    assert_eq!(
        part.premises,
        vec![Premise::RevisionIs {
            node: NodeId("d-40".into()),
            field: None,
            rev: 12,
        }]
    );

    assert_eq!(
        payload.minted,
        BTreeMap::from([(
            "new".to_string(),
            QualifiedNodeId {
                pool: pool(ALPHA),
                id: minted.clone()
            }
        )])
    );

    // The RETURN hands the minted identifier back with no follow-up read.
    let returned = project_returns(
        &stmt,
        &supersede_params(),
        &supersede_match(),
        &payload,
        &mut tank(),
    )
    .expect("projection");
    assert_eq!(returned.0.len(), 1);
    assert_eq!(returned.0[0]["new_id"], serde_json::json!(minted.0));
    assert_eq!(returned.0[0]["REV(old)"], serde_json::json!(12));
}

/// Minting is deterministic in the transaction hash and the create ordinal —
/// what makes recovery's roll-forward replay reproduce identical rows.
// RED: making `mint_node_id` ignore the ordinal (both creates then collided).
#[test]
fn minted_identifiers_are_deterministic_and_distinct() {
    assert_eq!(mint_node_id(&hash(1), 0), mint_node_id(&hash(1), 0));
    assert_ne!(mint_node_id(&hash(1), 0), mint_node_id(&hash(1), 1));
    assert_ne!(mint_node_id(&hash(1), 0), mint_node_id(&hash(2), 0));
    let id = mint_node_id(&hash(1), 0);
    assert!(id.0.starts_with('n') && id.0.len() == 17, "id = {}", id.0);
}

/// A statement whose parts span TWO pools at DIFFERENT pinned versions —
/// the cross-schema fixtures' whole point, and the per-part validated-under stamp with it.
// RED: stamping every part with alpha's version 1 (beta's part must read 2).
#[test]
fn cross_pool_statement_compiles_one_part_per_pool_in_frozen_order() {
    let stmt = parse_ok(
        r#"MATCH (d: fixture-alpha/doc { id: $d })
           MATCH (t: fixture-beta/ticket { id: $t })
           SET d.state = "published"
           CREATE (t) -[catalogues]-> (d)"#,
    );
    compile::check_coverage(&stmt).expect("staged in");
    let matched = matchset(&[
        (
            "d",
            vec![bound(
                ALPHA,
                record("d-1", "doc", Some("draft"), 1, &[("title", serde_json::json!("a"), 1)]),
            )],
        ),
        (
            "t",
            vec![bound(BETA, record("t-1", "ticket", Some("open"), 1, &[]))],
        ),
    ]);
    let p = params(&[("d", serde_json::json!("d-1")), ("t", serde_json::json!("t-1"))]);
    let payload = compile_with(&stmt, &p, &matched, &hash(7), &mut tank()).expect("compiles");

    assert_eq!(payload.parts.len(), 2);
    // Frozen landing order: ascending pool name.
    assert_eq!(payload.parts[0].pool, pool(ALPHA));
    assert_eq!(payload.parts[0].schema_version, 1);
    assert_eq!(payload.parts[1].pool, pool(BETA));
    assert_eq!(payload.parts[1].schema_version, 2);
    // The cross-pool edge lives in the SOURCE end's part, with the far
    // endpoint pool-qualified.
    assert_eq!(
        payload.parts[1].deltas,
        vec![Delta::AddEdge {
            src: NodeId("t-1".into()),
            kind: "catalogues".into(),
            dst: QualifiedNodeId {
                pool: pool(ALPHA),
                id: NodeId("d-1".into())
            },
            note: None,
            fields: Default::default(),
        }]
    );
}

/// The arrow's DIRECTION, not the source-text slot order, decides which end
/// is the edge's source — normalized exactly once, where the deltas are built.
/// The cross-pool kind is the discriminating case: written the other way, the
/// edge would be minted in the wrong pool under the wrong grammar.
// RED: making `src_dst` return `(from, to)` for the backward arrow — the
// `catalogues` edge was then attributed to `d` in fixture-alpha, whose grammar
// declares no such kind, and the compile refused with a GrammarViolation.
#[test]
fn edge_direction_is_normalized_once_from_the_arrow() {
    let stmt = parse_ok(
        "MATCH (d: fixture-alpha/doc) MATCH (t: fixture-beta/ticket) \
         CREATE (d) <-[catalogues]- (t)",
    );
    let matched = matchset(&[
        ("d", vec![bound(ALPHA, record("d-1", "doc", Some("draft"), 1, &[]))]),
        ("t", vec![bound(BETA, record("t-1", "ticket", Some("open"), 1, &[]))]),
    ]);
    let payload = compile_with(&stmt, &params(&[]), &matched, &hash(14), &mut tank())
        .expect("the backward spelling of the same edge compiles");
    assert_eq!(payload.parts.len(), 1);
    assert_eq!(payload.parts[0].pool, pool(BETA), "the SOURCE end's pool owns the part");
    assert_eq!(
        payload.parts[0].deltas,
        vec![Delta::AddEdge {
            src: NodeId("t-1".into()),
            kind: "catalogues".into(),
            dst: QualifiedNodeId {
                pool: pool(ALPHA),
                id: NodeId("d-1".into())
            },
            note: None,
            fields: Default::default(),
        }]
    );
    // The same for REMOVE, and for the WHERE-side direction the parser
    // records on an existence test.
    let rm = parse_ok(
        "MATCH (d: fixture-alpha/doc) MATCH (t: fixture-beta/ticket) \
         REMOVE (d) <-[catalogues]- (t)",
    );
    let payload = compile_with(&rm, &params(&[]), &matched, &hash(14), &mut tank()).expect("compiles");
    assert_eq!(
        payload.parts[0].deltas,
        vec![Delta::RemoveEdge {
            src: NodeId("t-1".into()),
            kind: "catalogues".into(),
            dst: QualifiedNodeId {
                pool: pool(ALPHA),
                id: NodeId("d-1".into())
            },
        }]
    );
    let ex = parse_ok("MATCH (a: fixture-alpha/author) WHERE EXISTS (a) <-[wrote]- ()");
    let Condition::Exists { direction, .. } = &ex.wheres[0] else {
        panic!("expected an existence test");
    };
    assert_eq!(*direction, Direction::In);
}

/// THE EMPTY MATCH SUCCEEDS TOUCHING NOTHING.
// RED: making `Emit::endpoints` refuse on an unbound binding — the empty case
// then failed instead of landing nothing.
#[test]
fn empty_match_succeeds_touching_nothing() {
    let stmt = parse_ok(
        r#"MATCH (d: fixture-alpha/doc { state: "draft" })
           WHERE d.title != "x"
           SET d.state = "published"
           RETURN d.id"#,
    );
    let payload = compile_with(&stmt, &params(&[]), &matchset(&[("d", vec![])]), &hash(3), &mut tank())
        .expect("an empty match is not an error");
    assert!(payload.parts.is_empty(), "nothing landed anywhere");
    assert!(payload.minted.is_empty());
    let returned = project_returns(&stmt, &params(&[]), &matchset(&[("d", vec![])]), &payload, &mut tank())
        .expect("projection");
    assert!(returned.0.is_empty(), "an empty match returns no rows");
}

/// ... and a count expectation is the ONLY way emptiness becomes an error.
// RED twice: reading `CountOp::AtMost` as `<` (`COUNT(d) <= 0` then refused
// on the empty match, exactly the case that must succeed), and
// inverting `CountOp::Exactly` (`= 1` on an empty match stopped refusing).
#[test]
fn count_expectation_turns_emptiness_into_expected_versus_found() {
    let stmt = parse_ok("MATCH (d: fixture-alpha/doc) EXPECT COUNT(d) = 1 SET d.state = \"draft\"");
    let err = compile_with(&stmt, &params(&[]), &matchset(&[("d", vec![])]), &hash(4), &mut tank())
        .expect_err("the author pinned emptiness");
    match only_witness(&err) {
        Witness::FailedExpectation {
            expectation,
            expected,
            found,
        } => {
            assert_eq!(
                expectation,
                ExpectationKind::Count {
                    binding: "d".into(),
                    op: CountOp::Exactly
                }
            );
            assert_eq!(expected, serde_json::json!(1));
            assert_eq!(found, serde_json::json!(0));
        }
        other => panic!("expected FailedExpectation, got {other:?}"),
    }
    // `<= n` over an empty match holds, and emptiness stays a success — at
    // the BOUNDARY too, which is the cell that separates `<=` from `<`.
    for text in [
        "MATCH (d: fixture-alpha/doc) EXPECT COUNT(d) <= 3 SET d.state = \"draft\"",
        "MATCH (d: fixture-alpha/doc) EXPECT COUNT(d) <= 0 SET d.state = \"draft\"",
    ] {
        let ok = parse_ok(text);
        compile_with(&ok, &params(&[]), &matchset(&[("d", vec![])]), &hash(4), &mut tank())
            .unwrap_or_else(|e| panic!("`{text}`: at-most is satisfied by zero; got {}", e.message()));
    }
    // ... and `= n` at the boundary holds when the match is exactly N.
    let exact = parse_ok("MATCH (d: fixture-alpha/doc) EXPECT COUNT(d) = 1 SET d.state = \"draft\"");
    let one = matchset(&[(
        "d",
        vec![bound(ALPHA, record("d-1", "doc", Some("published"), 1, &[]))],
    )]);
    compile_with(&exact, &params(&[]), &one, &hash(4), &mut tank()).expect("exactly one");
}

/// A stale premise refuses with expected-versus-found (the worked
/// example: the revision moved to 13 while the author was thinking).
// RED: comparing `found` against the found value instead of the expectation.
#[test]
fn stale_revision_refuses_with_expected_versus_found() {
    let stmt = parse_ok(SUPERSEDE);
    let moved = matchset(&[(
        "old",
        vec![bound(
            ALPHA,
            record("d-40", "doc", Some("published"), 13, &[("title", serde_json::json!("first"), 4)]),
        )],
    )]);
    let err = compile_with(&stmt, &supersede_params(), &moved, &hash(0xA1), &mut tank())
        .expect_err("rev 13 does not satisfy EXPECT REV = 12");
    match only_witness(&err) {
        Witness::FailedExpectation {
            expectation,
            expected,
            found,
        } => {
            assert_eq!(
                expectation,
                ExpectationKind::Revision {
                    binding: "old".into(),
                    field: None
                }
            );
            assert_eq!(expected, serde_json::json!(12));
            assert_eq!(found, serde_json::json!(13));
        }
        other => panic!("expected FailedExpectation, got {other:?}"),
    }
}

/// A per-field revision expectation reads the store's per-field bookkeeping.
// RED: pointing the lookup at the whole-node `rev` (4 != 9 fired immediately).
#[test]
fn per_field_revision_expectation_reads_the_field_revision() {
    let stmt = parse_ok("MATCH (d: fixture-alpha/doc) EXPECT REV(d.title) = 4 SET d.state = \"draft\"");
    let m = matchset(&[(
        "d",
        vec![bound(
            ALPHA,
            record("d-1", "doc", Some("draft"), 9, &[("title", serde_json::json!("t"), 4)]),
        )],
    )]);
    compile_with(&stmt, &params(&[]), &m, &hash(5), &mut tank()).expect("field rev 4 holds");

    let stale = parse_ok("MATCH (d: fixture-alpha/doc) EXPECT REV(d.title) = 5 SET d.state = \"draft\"");
    let err = compile_with(&stale, &params(&[]), &m, &hash(5), &mut tank()).expect_err("4 != 5");
    match only_witness(&err) {
        Witness::FailedExpectation { expectation, .. } => assert_eq!(
            expectation,
            ExpectationKind::Revision {
                binding: "d".into(),
                field: Some("title".into())
            }
        ),
        other => panic!("expected FailedExpectation, got {other:?}"),
    }
}

/// A per-field revision exists for DECLARED fields ONLY. `state`, `id` and
/// every machine-owned name ([`spine::schema_def::MACHINE_OWNED_FIELDS`]) are
/// columns the store keeps no per-field bookkeeping for, so
/// `EXPECT REV(b.<that>)` is a statement that can never be true of anything —
/// MALFORMED, naming the repair.
// RED: before the guard in `emit_expect_revision`, every name below refused
// as `FailedExpectation` with `found: null` — a witness that says "the node
// has no such revision right now" when the truth is "this name can NEVER
// carry one", so an author retried forever against a premise that cannot
// hold. Deleting the guard restores exactly that.
#[test]
fn a_per_field_revision_expectation_on_a_machine_owned_name_is_malformed() {
    let m = matchset(&[(
        "d",
        vec![bound(
            ALPHA,
            record("d-1", "doc", Some("draft"), 9, &[("title", serde_json::json!("t"), 4)]),
        )],
    )]);

    // `state` and `id` first, then every machine-owned name by NAME — so a
    // name added to `MACHINE_OWNED_FIELDS` without a guard is caught.
    let mut names: Vec<&str> = vec!["state", "id"];
    names.extend_from_slice(spine::schema_def::MACHINE_OWNED_FIELDS);
    for f in names {
        let text =
            format!("MATCH (d: fixture-alpha/doc) EXPECT REV(d.{f}) = 1 SET d.state = \"draft\"");
        let stmt = parse_ok(&text);
        let err = compile_with(&stmt, &params(&[]), &m, &hash(7), &mut tank())
            .err()
            .unwrap_or_else(|| panic!("`{text}` must refuse"));
        match only_witness(&err) {
            Witness::MalformedStatement { message, .. } => assert!(
                message.contains("no per-field revision"),
                "`{text}`: the refusal must name the rule; got: {message}"
            ),
            other => panic!("`{text}`: expected MalformedStatement, got {other:?}"),
        }
    }

    // The WHOLE-NODE revision on the same binding is the repair the message
    // names, and it holds — so the guard is about the FIELD, not about REV.
    let whole = parse_ok("MATCH (d: fixture-alpha/doc) EXPECT REV(d) = 9 SET d.state = \"draft\"");
    compile_with(&whole, &params(&[]), &m, &hash(7), &mut tank()).expect("the whole-node form is the repair");
    // And a DECLARED field still reads its own bookkeeping.
    let declared =
        parse_ok("MATCH (d: fixture-alpha/doc) EXPECT REV(d.title) = 4 SET d.state = \"draft\"");
    compile_with(&declared, &params(&[]), &m, &hash(7), &mut tank())
        .expect("a declared field carries a per-field revision");
}

/// The RETURN side of the same rule: `RETURN REV(b.<machine-owned>)` refuses
/// as malformed rather than PROJECTING A SILENT NULL.
// RED: before the guard in `project_returns`, these projected `null` into the
// reply — a column that reads "revision zero-ish / absent" for a name that
// can never have a revision, which is the same lie as the EXPECT form told
// but delivered as data instead of as a refusal. Deleting the guard makes
// every case below come back `Ok` with a null column.
#[test]
fn a_returned_per_field_revision_on_a_machine_owned_name_is_malformed() {
    let m = matchset(&[(
        "d",
        vec![bound(
            ALPHA,
            record("d-1", "doc", Some("draft"), 9, &[("title", serde_json::json!("t"), 4)]),
        )],
    )]);
    let payload = CompiledPayload {
        parts: vec![],
        minted: BTreeMap::new(),
    };

    let mut names: Vec<&str> = vec!["state", "id"];
    names.extend_from_slice(spine::schema_def::MACHINE_OWNED_FIELDS);
    for f in names {
        let text = format!("MATCH (d: fixture-alpha/doc) RETURN REV(d.{f})");
        let stmt = parse_ok(&text);
        let err = project_returns(&stmt, &params(&[]), &m, &payload, &mut tank())
            .err()
            .unwrap_or_else(|| panic!("`{text}` must refuse"));
        match only_witness(&err) {
            Witness::MalformedStatement { message, .. } => assert!(
                message.contains("no per-field revision"),
                "`{text}`: the refusal must name the rule; got: {message}"
            ),
            other => panic!("`{text}`: expected MalformedStatement, got {other:?}"),
        }
    }

    // The repair the message names projects the whole-node revision, and a
    // declared field still projects its own.
    let whole = parse_ok("MATCH (d: fixture-alpha/doc) RETURN REV(d) AS r, REV(d.title) AS t");
    let out = project_returns(&whole, &params(&[]), &m, &payload, &mut tank()).expect("projects");
    assert_eq!(out.0.len(), 1);
    assert_eq!(out.0[0]["r"], serde_json::json!(9));
    assert_eq!(out.0[0]["t"], serde_json::json!(4));
}

/// A failed WHERE premise refuses with the failing condition and the binding
/// (the specified refusal shape). WHERE is a PREMISE, not a filter.
// RED: dropping the `check_condition` call from `compile` — the false premise
// was then recorded as a durable `FieldIs` and the statement compiled clean.
#[test]
fn failed_where_premise_refuses_with_condition_and_binding() {
    let stmt = parse_ok(
        r#"MATCH (d: fixture-alpha/doc) WHERE d.title = "keep" SET d.state = "published""#,
    );
    let m = matchset(&[(
        "d",
        vec![bound(
            ALPHA,
            record("d-1", "doc", Some("draft"), 1, &[("title", serde_json::json!("moved"), 1)]),
        )],
    )]);
    let err = compile_with(&stmt, &params(&[]), &m, &hash(6), &mut tank()).expect_err("premise failed");
    match only_witness(&err) {
        Witness::FailedCondition { condition, binding } => {
            assert_eq!(condition, r#"d.title = "keep""#);
            assert_eq!(binding, "d");
        }
        other => panic!("expected FailedCondition, got {other:?}"),
    }

    // The UNIVERSAL reading: a conjunction over a MULTI-row binding refuses
    // when any one row fails, and names the failing conjunct alone.
    let two = parse_ok(
        "MATCH (d: fixture-alpha/doc) WHERE d.rank > 1 AND d.rank < 100 SET d.state = \"published\"",
    );
    let rows = matchset(&[(
        "d",
        vec![
            bound(ALPHA, record("d-1", "doc", None, 1, &[("rank", serde_json::json!(5), 1)])),
            bound(ALPHA, record("d-2", "doc", None, 1, &[("rank", serde_json::json!(500), 1)])),
        ],
    )]);
    let err = compile_with(&two, &params(&[]), &rows, &hash(6), &mut tank()).expect_err("row two fails");
    match only_witness(&err) {
        Witness::FailedCondition { condition, binding } => {
            assert_eq!(condition, "d.rank < 100");
            assert_eq!(binding, "d");
        }
        other => panic!("expected FailedCondition, got {other:?}"),
    }
    // Both rows inside the band: the premise holds and both rows compile.
    let ok = matchset(&[(
        "d",
        vec![
            bound(ALPHA, record("d-1", "doc", None, 1, &[("rank", serde_json::json!(5), 1)])),
            bound(ALPHA, record("d-2", "doc", None, 1, &[("rank", serde_json::json!(50), 1)])),
        ],
    )]);
    let payload = compile_with(&two, &params(&[]), &ok, &hash(6), &mut tank()).expect("holds");
    assert_eq!(payload.parts[0].deltas.len(), 2, "one SET per matched row");
}

/// Every part is validated against the TOUCHED pool's OWN pinned grammar
/// — never one merged vocabulary.
// RED: pointing the grammar lookup at `fixture_alpha()` for both pools — the
// beta statement then compiled a `holds` edge under alpha's vocabulary.
#[test]
fn per_pool_grammar_violations_name_the_pool() {
    let cases: [(&str, &str, &str); 4] = [
        (
            r#"MATCH (d: fixture-alpha/doc) SET d.nope = 1"#,
            ALPHA,
            "is not declared on node type",
        ),
        (
            r#"MATCH (d: fixture-alpha/doc) SET d.state = "archived""#,
            ALPHA,
            "closed state vocabulary",
        ),
        (
            r#"MATCH (t: fixture-beta/ticket) SET t.state = "draft""#,
            BETA,
            "closed state vocabulary",
        ),
        (
            r#"MATCH (a: fixture-alpha/author) SET a.state = "open""#,
            ALPHA,
            "carries no state vocabulary",
        ),
    ];
    for (text, want_pool, needle) in cases {
        let stmt = parse_ok(text);
        let binding = stmt.binding_types().keys().next().cloned().expect("one binding");
        let (p, ty) = stmt.binding_types()[&binding].clone();
        let state = if ty == "doc" {
            Some("draft")
        } else if ty == "ticket" {
            Some("open")
        } else {
            None
        };
        let m = matchset(&[(
            Box::leak(binding.into_boxed_str()),
            vec![bound(p.as_str(), record("x-1", &ty, state, 1, &[]))],
        )]);
        let err = compile_with(&stmt, &params(&[]), &m, &hash(9), &mut tank())
            .err()
            .unwrap_or_else(|| panic!("`{text}` must refuse"));
        match only_witness(&err) {
            Witness::GrammarViolation { pool: got, message, .. } => {
                assert_eq!(got, pool(want_pool), "`{text}`");
                assert!(message.contains(needle), "`{text}`: got `{message}`");
            }
            other => panic!("`{text}`: expected GrammarViolation, got {other:?}"),
        }
    }
}

/// An unsupplied `$param` refuses, and so does a supplied-but-unreferenced
/// binding (frozen lexical rule — the two travel separately, so
/// neither may be silently ignored).
// RED: dropping the second loop of `check_params` — the unused binding then
// rode along silently.
#[test]
fn parameters_refuse_in_both_directions() {
    let stmt = parse_ok(r#"MATCH (d: fixture-alpha/doc { id: $wanted })"#);
    let err = compile::check_params(&stmt, &params(&[])).expect_err("unsupplied");
    match only_witness(&err) {
        Witness::MissingParameter { name } => assert_eq!(name, "wanted"),
        other => panic!("expected MissingParameter, got {other:?}"),
    }
    let err = compile::check_params(
        &stmt,
        &params(&[("wanted", serde_json::json!("x")), ("spare", serde_json::json!(1))]),
    )
    .expect_err("unreferenced");
    // Its OWN witness: `spare` was SUPPLIED and unused, so the
    // witness says "unused", never "missing" — the two repairs are opposite.
    match only_witness(&err) {
        Witness::UnusedParameter { name } => assert_eq!(name, "spare"),
        other => panic!("expected UnusedParameter, got {other:?}"),
    }
    compile::check_params(&stmt, &params(&[("wanted", serde_json::json!("x"))])).expect("exact cover");
}

// ---------------------------------------------------------------------------
// 4. Footprint and claim derivation — from TEXT, and from MATCHED bindings
// ---------------------------------------------------------------------------

/// The static footprint reads pools, types, edge kinds and fields out of the
/// statement's TEXT alone (the second, load-bearing job of MATCH).
// RED: deleting the RETURN pass from `footprint` — `REV(old)`'s `rev` and
// `new.id`'s `id` vanished from the field set.
#[test]
fn static_footprint_is_read_from_the_text_alone() {
    let fp = footprint(&parse_ok(SUPERSEDE));
    assert_eq!(
        fp,
        Footprint {
            pools: BTreeSet::from([pool(ALPHA)]),
            types: BTreeSet::from([(pool(ALPHA), "doc".to_string())]),
            edge_kinds: BTreeSet::from([(pool(ALPHA), "supersedes".to_string())]),
            fields: BTreeSet::from([
                (pool(ALPHA), "doc".to_string(), "id".to_string()),
                (pool(ALPHA), "doc".to_string(), "rev".to_string()),
                (pool(ALPHA), "doc".to_string(), "state".to_string()),
                (pool(ALPHA), "doc".to_string(), "title".to_string()),
            ]),
        }
    );
}

/// One case per STAGED form, so the derivation is proven form by form and not
/// only on the worked example.
// RED three times: recording `FIELD_REV` as `id`; making the hop attribute
// its kind to the arrow's LEFT end rather than its source; and the same for
// the CREATE/REMOVE edge forms (`from` instead of the arrow's source). The
// last two are only visible on the CROSS-POOL kind, which is why it is here.
#[test]
fn footprint_per_staged_form() {
    // match by identifier
    let fp = footprint(&parse_ok(r#"MATCH (d: fixture-alpha/doc { id: $x })"#));
    assert_eq!(fp.pools, BTreeSet::from([pool(ALPHA)]));
    assert_eq!(
        fp.fields,
        BTreeSet::from([(pool(ALPHA), "doc".into(), "id".to_string())])
    );
    assert!(fp.edge_kinds.is_empty());

    // match by pool+type+state
    let fp = footprint(&parse_ok(r#"MATCH (d: fixture-alpha/doc { state: "draft" })"#));
    assert_eq!(
        fp.fields,
        BTreeSet::from([(pool(ALPHA), "doc".into(), "state".to_string())])
    );

    // single hop — the edge kind is attributed to the SOURCE end's pool, in
    // both arrow spellings. The CROSS-POOL kind is the discriminating case:
    // when both ends live in one pool, source-versus-left is unobservable.
    for text in [
        "MATCH (a: fixture-alpha/author) -[wrote]-> (d: fixture-alpha/doc)",
        "MATCH (d: fixture-alpha/doc) <-[wrote]- (a: fixture-alpha/author)",
    ] {
        let fp = footprint(&parse_ok(text));
        assert_eq!(
            fp.edge_kinds,
            BTreeSet::from([(pool(ALPHA), "wrote".to_string())]),
            "{text}"
        );
        assert_eq!(fp.types.len(), 2, "{text}");
    }
    for text in [
        "MATCH (t: fixture-beta/ticket) -[catalogues]-> (d: fixture-alpha/doc)",
        "MATCH (d: fixture-alpha/doc) <-[catalogues]- (t: fixture-beta/ticket)",
        "MATCH (d: fixture-alpha/doc) MATCH (t: fixture-beta/ticket) CREATE (d) <-[catalogues]- (t)",
        "MATCH (d: fixture-alpha/doc) MATCH (t: fixture-beta/ticket) REMOVE (t) -[catalogues]-> (d)",
    ] {
        let fp = footprint(&parse_ok(text));
        assert_eq!(
            fp.edge_kinds,
            BTreeSet::from([(pool(BETA), "catalogues".to_string())]),
            "`{text}`: the kind belongs to the SOURCE end's pool, whichever way \
             the arrow was written"
        );
    }

    // EXPECT: whole-node revision is `rev`, per-field is the field, value is
    // the dotted path, count adds no field.
    let fp = footprint(&parse_ok(
        "MATCH (d: fixture-alpha/doc) EXPECT REV(d) = 1 EXPECT REV(d.title) = 1 EXPECT d.state = \"draft\" EXPECT COUNT(d) = 1",
    ));
    assert_eq!(
        fp.fields,
        BTreeSet::from([
            (pool(ALPHA), "doc".into(), "rev".to_string()),
            (pool(ALPHA), "doc".into(), "title".to_string()),
            (pool(ALPHA), "doc".into(), "state".to_string()),
        ])
    );

    // WHERE over bound fields
    let fp = footprint(&parse_ok(
        "MATCH (d: fixture-alpha/doc) WHERE d.title != \"x\" AND d.meta.tag = 1",
    ));
    assert_eq!(
        fp.fields,
        BTreeSet::from([
            (pool(ALPHA), "doc".into(), "title".to_string()),
            (pool(ALPHA), "doc".into(), "meta.tag".to_string()),
        ])
    );

    // CREATE node / CREATE edge / SET / REMOVE
    let fp = footprint(&parse_ok(
        r#"MATCH (a: fixture-alpha/author)
           CREATE (d: fixture-alpha/doc { title: $t })
           CREATE (a) -[wrote]-> (d)
           REMOVE (d) -[current_of]-> (a)
           SET a.name = "x""#,
    ));
    assert_eq!(
        fp.types,
        BTreeSet::from([
            (pool(ALPHA), "author".to_string()),
            (pool(ALPHA), "doc".to_string())
        ])
    );
    assert_eq!(
        fp.edge_kinds,
        BTreeSet::from([
            (pool(ALPHA), "wrote".to_string()),
            (pool(ALPHA), "current_of".to_string())
        ])
    );
    assert!(fp.fields.contains(&(pool(ALPHA), "doc".into(), "title".into())));
    assert!(fp.fields.contains(&(pool(ALPHA), "author".into(), "name".into())));

    // cross-pool: two pools in one footprint
    let fp = footprint(&parse_ok(
        "MATCH (d: fixture-alpha/doc) MATCH (t: fixture-beta/ticket) CREATE (t) -[catalogues]-> (d)",
    ));
    assert_eq!(fp.pools, BTreeSet::from([pool(ALPHA), pool(BETA)]));
    assert_eq!(
        fp.edge_kinds,
        BTreeSet::from([(pool(BETA), "catalogues".to_string())]),
        "the edge is claimed in the SOURCE end's pool"
    );
}

/// The PRE-MATCH claim footprint: text names populations, never identities.
// RED twice: dropping the `h.field(FIELD_ID).is_none()` guard (a match-by-id
// then claimed a type-slice read it never performs), and weakening
// `combine_modes`'s read+link case to `Read` (the scan-and-mint statement
// then held the weaker of two conflicting modes on one resource).
#[test]
fn claim_footprint_from_text_claims_populations_only() {
    // Match by identifier: no scan, so no type-slice read; the create's link
    // claim is the only one the text can prove.
    let set = claim_footprint(&parse_ok(SUPERSEDE), &grammars).expect("grammars resolve");
    assert_eq!(
        set,
        claim_set(&[claim(ALPHA, tslice("doc"), LockMode::Link)]),
        "a create takes LINK on the type slice, never a write"
    );

    // A scanning match reads the population.
    let set = claim_footprint(
        &parse_ok(r#"MATCH (d: fixture-alpha/doc { state: "draft" }) SET d.state = "published""#),
        &grammars,
    )
    .expect("grammars resolve");
    assert_eq!(set, claim_set(&[claim(ALPHA, tslice("doc"), LockMode::Read)]));

    // A count expectation holds the population still.
    let set = claim_footprint(
        &parse_ok(r#"MATCH (d: fixture-alpha/doc { id: $i }) EXPECT COUNT(d) = 1"#),
        &grammars,
    )
    .expect("grammars resolve");
    assert_eq!(set, claim_set(&[claim(ALPHA, tslice("doc"), LockMode::Read)]));

    // A statement that BOTH scans a type and mints into it holds read AND
    // link on one resource; the lock compatibility rule makes those two
    // conflict, so one transaction holding both must hold the STRONGER.
    let set = claim_footprint(
        &parse_ok(
            r#"MATCH (d: fixture-alpha/doc { state: "draft" })
               CREATE (n: fixture-alpha/doc { title: $t })"#,
        ),
        &grammars,
    )
    .expect("grammars resolve");
    assert_eq!(
        set,
        claim_set(&[claim(ALPHA, tslice("doc"), LockMode::Write)]),
        "read + link on one resource escalates to write, never to the weaker"
    );
    assert!(
        !spine::locks::compatible(LockMode::Read, LockMode::Link),
        "the escalation exists because the compatibility matrix says the two conflict"
    );

    // An unknown pool, and an undeclared type, refuse BEFORE any claim is
    // spoken — naming the pool.
    // The unknown pool carries the FROZEN tag `foreign_pool_unknown` (one
    // fact, one tag — the door's producer of it), stamped with the
    // pool the statement named.
    // RED: `resolve_grammars`' refusal reverted to untagged prose — the
    // tag assertion fired (`None`).
    let err = claim_footprint(&parse_ok("MATCH (d: nowhere/doc)"), &grammars).expect_err("unknown pool");
    match only_witness(&err) {
        w @ Witness::GrammarViolation { .. } => {
            let Witness::GrammarViolation { pool: p, .. } = &w else { unreachable!() };
            assert_eq!(p, &pool("nowhere"));
            assert_eq!(grammar_fault_tag(&w), Some("foreign_pool_unknown"), "{w:?}");
        }
        other => panic!("expected GrammarViolation, got {other:?}"),
    }
    let err = claim_footprint(&parse_ok("MATCH (d: fixture-alpha/shelf)"), &grammars)
        .expect_err("shelf is beta's type, not alpha's");
    match only_witness(&err) {
        Witness::GrammarViolation { pool: p, message, .. } => {
            assert_eq!(p, pool(ALPHA));
            assert!(message.contains("not declared by this pool's grammar"));
        }
        other => panic!("expected GrammarViolation, got {other:?}"),
    }
}

/// THE PEN'S NET in `compile`: each of
/// `compile`'s three write-target sites — the CREATE head, the SET target,
/// the edge SOURCE — refuses a target in the projected namespace with
/// `ProjectedNamespaceWrite` through `project::guard_write_target`, for a
/// DIRECT caller of `compile`. Proven here, not only through the door,
/// because at the door `exec`'s phase-5 CREATE-head duplicate refuses a
/// CREATE into `spine` first and SHADOWS site 1 — a poison of site 1 alone is
/// invisible through `Cauldron::submit`. Sites 2 and 3 are also proven
/// through the door, pre-append, in `tests/cross_pool.rs`.
///
/// The witness text: site 1 names the head (`CREATE (g)`, the SAME text
/// exec's duplicate writes — one fact, one witness); sites 2 and 3 name the
/// ghost's address.
// RED: (a) `guard_write_target` returning `Ok(())` — the first arm fired
// (the loop stops there): site 1 came back `GrammarViolation` in pool
// `spine`, `undeclared_field` (the projected `txn` declares no `title`);
// (b) each site silenced ALONE — only its own arm fired: site 1 as in (a);
// site 2 COMPILED, handing back a `spine` part carrying a `SetField` (the
// projected `txn` states accept "pending" — the delta the door would append
// and then fail HARD on at phase 10b); site 3 came back `GrammarViolation` in
// pool `spine` (no edge kind `wrote` in a grammar with no edge kinds). Site 1
// is ONLY provable here: through the door, exec's phase-5 duplicate shadows it
// and `cross_pool`/`refusal_witnesses` stay green with site 1 silenced.
#[test]
fn the_pens_net_in_compile_refuses_every_projected_write_target_for_a_direct_caller() {
    let ghost_hex = "ab".repeat(32);
    let ghost = bound(
        "spine",
        record(&ghost_hex, "txn", Some("cleared"), 1, &[]),
    );
    let doc = bound(ALPHA, record("d-1", "doc", Some("draft"), 1, &[("title", serde_json::json!("t"), 1)]));
    let arms: [(&str, &str, Bindings, MatchSet, String); 3] = [
        (
            "site 1: a CREATE head",
            r#"CREATE (g: spine/txn { title: $t })"#,
            params(&[("t", serde_json::json!("forged provenance"))]),
            matchset(&[]),
            "CREATE (g)".to_string(),
        ),
        (
            "site 2: a SET target",
            r#"MATCH (g: spine/txn { id: $h }) SET g.state = "pending""#,
            params(&[("h", serde_json::json!(ghost_hex))]),
            matchset(&[("g", vec![ghost.clone()])]),
            ghost_hex.clone(),
        ),
        (
            "site 3: an edge SOURCE",
            r#"MATCH (g: spine/txn { id: $h }) MATCH (d: fixture-alpha/doc { id: $d }) CREATE (g) -[wrote]-> (d)"#,
            params(&[("h", serde_json::json!(ghost_hex)), ("d", serde_json::json!("d-1"))]),
            matchset(&[("g", vec![ghost.clone()]), ("d", vec![doc.clone()])]),
            ghost_hex.clone(),
        ),
    ];
    for (label, text, p, matched, want_target) in arms {
        let err = compile_with(&parse_ok(text), &p, &matched, &hash(9), &mut tank())
            .expect_err(label);
        match only_witness(&err) {
            Witness::ProjectedNamespaceWrite { pool: p, target } => {
                assert_eq!(p, pool("spine"), "{label}");
                assert_eq!(target, want_target, "{label}: the witness names the target");
            }
            other => panic!("{label}: expected ProjectedNamespaceWrite, got {other:?}"),
        }
    }
}

/// The CONCRETE claim set — the claim table applied to what MATCHED and
/// what the compilation actually emits.
// RED: dropping the `Resource::Edge` write claim from `claims_for` — the new
// edge was then minted under no claim of its own.
#[test]
fn concrete_claims_from_the_matched_supersede() {
    let stmt = parse_ok(SUPERSEDE);
    let matched = supersede_match();
    let h = hash(0xA1);
    let minted = mint_node_id(&h, 0);
    let payload = compile_with(&stmt, &supersede_params(), &matched, &h, &mut tank()).expect("compiles");
    let set = claims_for(&stmt, &matched, &payload, &grammars).expect("claims");

    assert_eq!(
        set,
        claim_set(&[
            // the matched body is read, then WRITTEN by the SET: one claim,
            // the stronger mode
            claim(ALPHA, body("d-40"), LockMode::Write),
            // the minted body: written by the create, read as an edge endpoint
            claim(ALPHA, body(&minted.0), LockMode::Write),
            // the create's link on the pool's type population
            claim(ALPHA, tslice("doc"), LockMode::Link),
            // the new edge itself, by its whole triple
            claim(
                ALPHA,
                Resource::Edge {
                    src: minted.clone(),
                    kind: "supersedes".into(),
                    dst: QualifiedNodeId {
                        pool: pool(ALPHA),
                        id: NodeId("d-40".into())
                    },
                },
                LockMode::Write
            ),
            // both endpoint slices, LINK because `supersedes` is not an
            // at-most-one slice
            claim(ALPHA, eslice(&minted.0, "supersedes", Direction::Out), LockMode::Link),
            claim(ALPHA, eslice("d-40", "supersedes", Direction::In), LockMode::Link),
            // the state slices: the create LINKS the
            // slice its state enters; the SET links the slice `old` enters and
            // the one its bound state leaves
            claim(ALPHA, sslice("doc", "draft"), LockMode::Link),
            claim(ALPHA, sslice("doc", "superseded"), LockMode::Link),
            claim(ALPHA, sslice("doc", "published"), LockMode::Link),
        ])
    );
}

/// Everything the match bound is READ (read on a read node's
/// body) — including the conditional-write case's premise node, which the
/// write never touches (WHERE: "set this task ready only if its
/// milestone is still active" writes one field but its premise lives on a
/// different node).
// RED: dropping the matched-bindings loop from `claims_for` — the premise
// node then carried NO claim at all, so nothing would have stopped another
// writer moving it under this transaction's feet.
#[test]
fn a_matched_body_that_is_only_read_claims_read() {
    let stmt = parse_ok(
        r#"MATCH (d: fixture-alpha/doc { id: $i })
           MATCH (a: fixture-alpha/author { id: $j })
           WHERE a.name = "keep"
           SET d.state = "published""#,
    );
    let matched = matchset(&[
        ("d", vec![bound(ALPHA, record("d-1", "doc", Some("draft"), 1, &[]))]),
        (
            "a",
            vec![bound(
                ALPHA,
                record("a-1", "author", None, 1, &[("name", serde_json::json!("keep"), 1)]),
            )],
        ),
    ]);
    let p = params(&[("i", serde_json::json!("d-1")), ("j", serde_json::json!("a-1"))]);
    let payload = compile_with(&stmt, &p, &matched, &hash(15), &mut tank()).expect("compiles");
    let set = claims_for(&stmt, &matched, &payload, &grammars).expect("claims");
    assert_eq!(
        set,
        claim_set(&[
            claim(ALPHA, body("a-1"), LockMode::Read),
            claim(ALPHA, body("d-1"), LockMode::Write),
            // the state SET links the slice d enters and the one its bound
            // state leaves
            claim(ALPHA, sslice("doc", "published"), LockMode::Link),
            claim(ALPHA, sslice("doc", "draft"), LockMode::Link),
        ]),
        "the premise node is read-claimed; only the written node is write-claimed"
    );
}

/// An AT-MOST-ONE slice is a SCALAR: link is forbidden on it and every
/// membership change is a slice WRITE. `current_of` is
/// at-most-one PER TO (at most one per author).
// RED: reading `at_most_one_per_to` off the FROM side — the incoming slice
// dropped back to Link and the assertion fired.
#[test]
fn at_most_one_slice_takes_a_write_not_a_link() {
    let stmt = parse_ok(
        "MATCH (d: fixture-alpha/doc) MATCH (a: fixture-alpha/author) CREATE (d) -[current_of]-> (a)",
    );
    let matched = matchset(&[
        ("d", vec![bound(ALPHA, record("d-1", "doc", Some("draft"), 1, &[]))]),
        ("a", vec![bound(ALPHA, record("a-1", "author", None, 1, &[]))]),
    ]);
    let payload = compile_with(&stmt, &params(&[]), &matched, &hash(11), &mut tank()).expect("compiles");
    let set = claims_for(&stmt, &matched, &payload, &grammars).expect("claims");
    let modes: BTreeMap<String, LockMode> = set
        .iter()
        .map(|c| (spine::locks::resource_key(&c.resource), c.mode))
        .collect();
    assert_eq!(
        modes["eslice:a-1:current_of:in"],
        LockMode::Write,
        "the at-most-one side is a scalar: membership changes are slice WRITES"
    );
    assert_eq!(
        modes["eslice:d-1:current_of:out"],
        LockMode::Link,
        "the unconstrained side stays shared among membership-changers"
    );
}

/// A REMOVE over a bound SET of endpoints dissolves a cluster in one line
/// — the cartesian product, one delta per pair.
// RED: iterating only the first source endpoint (2 deltas instead of 4).
#[test]
fn remove_over_bound_sets_is_the_cartesian_product() {
    let stmt = parse_ok(
        "MATCH (a: fixture-alpha/author) MATCH (d: fixture-alpha/doc) REMOVE (a) -[wrote]-> (d)",
    );
    let matched = matchset(&[
        (
            "a",
            vec![
                bound(ALPHA, record("a-1", "author", None, 1, &[])),
                bound(ALPHA, record("a-2", "author", None, 1, &[])),
            ],
        ),
        (
            "d",
            vec![
                bound(ALPHA, record("d-1", "doc", Some("draft"), 1, &[])),
                bound(ALPHA, record("d-2", "doc", Some("draft"), 1, &[])),
            ],
        ),
    ]);
    let payload = compile_with(&stmt, &params(&[]), &matched, &hash(12), &mut tank()).expect("compiles");
    assert_eq!(payload.parts[0].deltas.len(), 4);
    assert!(payload.parts[0]
        .deltas
        .iter()
        .all(|d| matches!(d, Delta::RemoveEdge { .. })));
}

// ---------------------------------------------------------------------------
// 5. The evaluation ceiling — fuel
// ---------------------------------------------------------------------------

/// A pathological match exhausts the tank and fails LOUDLY as an engine
/// error — never a partial payload.
// RED: raising the tank to the default ceiling — the same statement then
// compiled 4,000 deltas clean, which is the exact silent-partial-result the
// bound exists to prevent.
#[test]
fn fuel_exhaustion_fires_loudly_on_a_pathological_match() {
    let stmt = parse_ok("MATCH (d: fixture-alpha/doc) SET d.state = \"published\"");
    let rows: Vec<BoundNode> = (0..2000)
        .map(|i| bound(ALPHA, record(&format!("d-{i}"), "doc", Some("draft"), 1, &[])))
        .collect();
    let matched = matchset(&[("d", rows)]);

    let mut small = Fuel::new(16);
    let err = compile_with(&stmt, &params(&[]), &matched, &hash(13), &mut small)
        .expect_err("2000 rows do not fit in 16 fuel");
    match err {
        SpineError::FuelExhausted { spent, ceiling } => {
            assert_eq!(ceiling, 16);
            assert!(spent > ceiling, "spent {spent} must report the OVERSHOOT");
        }
        other => panic!("expected FuelExhausted, got {other:?}"),
    }
    // The engine error is NOT a refusal: it carries no witnesses, because the
    // statement was never judged — the engine gave up.
    let mut small = Fuel::new(16);
    let err = compile_with(&stmt, &params(&[]), &matched, &hash(13), &mut small).unwrap_err();
    assert_eq!(err.kind(), "fuel_exhausted");
    assert_eq!(err.payload()["ceiling"], 16);

    // The same statement inside a generous tank compiles whole — proving the
    // red above came from the BOUND and not from a defect in the expansion.
    let mut big = tank();
    let payload = compile_with(&stmt, &params(&[]), &matched, &hash(13), &mut big).expect("fits");
    assert_eq!(payload.parts[0].deltas.len(), 2000);
    assert!(big.remaining() < SpineConfig::default().fuel_ceiling, "fuel was debited");
}

/// A zero tank refuses the very first step: the bound is real at N = 0, not
/// just at scale.
// RED: making `spend` compare `units >= self.remaining` (a 0-unit spend on a
// full tank then failed).
#[test]
fn fuel_is_bounded_at_the_first_step() {
    let mut dry = Fuel::new(0);
    assert!(dry.spend(1).is_err());
    let mut one = Fuel::new(1);
    one.spend(1).expect("exactly the tank");
    assert_eq!(one.remaining(), 0);
    assert!(one.spend(1).is_err());
}

/// RETURN projection is fuelled too — a wide cartesian product cannot cost
/// unbounded work on the reply side either.
// RED: removing the per-row `spend` from `project_returns`.
#[test]
fn return_projection_is_fuelled() {
    let stmt = parse_ok("MATCH (a: fixture-alpha/author) MATCH (d: fixture-alpha/doc) RETURN a.id, d.id");
    let matched = matchset(&[
        (
            "a",
            (0..60)
                .map(|i| bound(ALPHA, record(&format!("a-{i}"), "author", None, 1, &[])))
                .collect(),
        ),
        (
            "d",
            (0..60)
                .map(|i| bound(ALPHA, record(&format!("d-{i}"), "doc", Some("draft"), 1, &[])))
                .collect(),
        ),
    ]);
    let payload = CompiledPayload {
        parts: vec![],
        minted: BTreeMap::new(),
    };
    let mut small = Fuel::new(50);
    let err = project_returns(&stmt, &params(&[]), &matched, &payload, &mut small)
        .expect_err("3600 rows do not fit in 50 fuel");
    assert_eq!(err.kind(), "fuel_exhausted");

    let mut big = tank();
    let out = project_returns(&stmt, &params(&[]), &matched, &payload, &mut big).expect("fits");
    assert_eq!(out.0.len(), 3600, "the cartesian product of the named bindings");
}

// ---------------------------------------------------------------------------
// 6. The refusal witnesses — every refusal shape, structured and serializable
// ---------------------------------------------------------------------------

/// Every refusal shape round-trips through JSON with a `kind` tag a suite can
/// match on by name.
// RED: removing `#[serde(tag = "kind")]` — the round-trip survived but the
// tag assertion fired, which is the half that suites actually depend on.
#[test]
fn every_witness_shape_serializes_and_round_trips() {
    let all = vec![
        (
            "failed_expectation",
            Witness::FailedExpectation {
                expectation: ExpectationKind::Revision {
                    binding: "old".into(),
                    field: None,
                },
                expected: serde_json::json!(12),
                found: serde_json::json!(13),
            },
        ),
        (
            "failed_condition",
            Witness::FailedCondition {
                condition: "d.title = \"keep\"".into(),
                binding: "d".into(),
            },
        ),
        (
            "unimplemented_form",
            Witness::UnimplementedForm {
                form: MULTI_HOP_FORM.into(),
            },
        ),
        (
            "version_mismatch",
            Witness::VersionMismatch {
                envelope_version: 99,
                supported_version: ENVELOPE_FORMAT_VERSION,
            },
        ),
        (
            "stale_match",
            Witness::StaleMatch {
                contested: vec![claim(ALPHA, body("d-40"), LockMode::Write)],
            },
        ),
        (
            "grammar_violation",
            Witness::GrammarViolation {
                pool: pool(ALPHA),
                part: Some(0),
                message: "unknown_state: nope".into(),
            },
        ),
        (
            "projected_namespace_write",
            Witness::ProjectedNamespaceWrite {
                pool: pool("spine"),
                target: "abc".into(),
            },
        ),
        (
            "ghost_target_unknown",
            Witness::GhostTargetUnknown {
                target: "abc".into(),
            },
        ),
        (
            "malformed_statement",
            Witness::MalformedStatement {
                message: "m".into(),
                offset: 3,
            },
        ),
        (
            "missing_parameter",
            Witness::MissingParameter { name: "p".into() },
        ),
    ];
    for (tag, w) in all {
        let json = serde_json::to_value(&w).expect("a witness serializes");
        assert_eq!(json["kind"], tag, "the frozen machine tag");
        let back: Witness = serde_json::from_value(json).expect("and round-trips");
        assert_eq!(back, w);
    }

    // The three expectation kinds carry their own `of` tag.
    for (tag, k) in [
        (
            "revision",
            ExpectationKind::Revision {
                binding: "b".into(),
                field: Some("f".into()),
            },
        ),
        (
            "value",
            ExpectationKind::Value {
                binding: "b".into(),
                path: vec!["state".into()],
            },
        ),
        (
            "count",
            ExpectationKind::Count {
                binding: "b".into(),
                op: CountOp::AtMost,
            },
        ),
    ] {
        assert_eq!(serde_json::to_value(&k).expect("serializes")["of"], tag);
    }
}

/// A refusal travels as ONE structured payload — witnesses under `witnesses`,
/// nothing asserted on prose.
// RED: asserting `kind == "refused"` on a `FuelExhausted` error.
#[test]
fn a_refusal_carries_its_witnesses_in_the_machine_payload() {
    let err = parse("MATCH (d: doc)").expect_err("no pool");
    assert_eq!(err.kind(), "refused");
    let payload = err.payload();
    assert_eq!(payload["witnesses"][0]["kind"], "malformed_statement");
    assert!(payload["witnesses"][0]["message"]
        .as_str()
        .expect("a message")
        .contains("pool qualification"));
}

// ---------------------------------------------------------------------------
// 7. The envelope — hash identity, and the three gates
// ---------------------------------------------------------------------------

/// The FROZEN hash algorithm, checked against a digest computed OUTSIDE this
/// crate so the test proves the algorithm and not merely self-consistency.
///
/// Derivation (python3, the frozen spec read literally — SHA-256 over, for
/// each element in order, an 8-byte big-endian length prefix followed by the
/// element's UTF-8 bytes):
///
/// ```text
/// els = [b'RETURN', b'{}',
///        b'{"class":"agent","thread":"t","dispatch":"d"}',
///        b'{"name":"i","version":"1"}', b'']
/// h = hashlib.sha256()
/// for e in els: h.update(struct.pack('>Q', len(e))); h.update(e)
/// ```
// RED: prefixing with 4 bytes instead of 8, and separately by hashing the
// nonce before the instrument — both produced a different digest here.
#[test]
fn hash_identity_matches_the_frozen_algorithm_byte_for_byte() {
    let env = envelope("RETURN", params(&[]));
    // The canonical JSON of each element, asserted so the digest below is
    // pinned to a known encoding and not to serde's current mood.
    assert_eq!(serde_json::to_string(&env.bindings).expect("json"), "{}");
    assert_eq!(
        serde_json::to_string(&env.provenance.initiator).expect("json"),
        r#"{"class":"agent","thread":"t","dispatch":"d"}"#
    );
    assert_eq!(
        serde_json::to_string(&env.provenance.instrument).expect("json"),
        r#"{"name":"i","version":"1"}"#
    );
    assert_eq!(
        env.hash_identity().expect("hashes").to_hex(),
        "b7a2610b96d2b84dd6be67a263f973ba9e7127f04eb8825e54c75b64f039acc2"
    );
}

/// `mode` and `format_version` are deliberately OUTSIDE the identity: a
/// validate dry run and its commit are the SAME ask, and a re-encoded
/// envelope is not a new transaction (frozen doc; one validator,
/// two callers).
// RED: folding `mode` into the hash — the dry-run/commit assertion fired.
#[test]
fn mode_and_format_version_are_outside_the_hash_identity() {
    let base = envelope("RETURN", params(&[]));
    let mut dry = base.clone();
    dry.mode = Mode::Validate;
    let mut reencoded = base.clone();
    reencoded.format_version = 7;
    assert_eq!(
        base.hash_identity().expect("h"),
        dry.hash_identity().expect("h")
    );
    assert_eq!(
        base.hash_identity().expect("h"),
        reencoded.hash_identity().expect("h")
    );
}

/// Everything INSIDE the identity moves it — the exactly-once dedup depends
/// on each of the five elements being load-bearing.
// RED: dropping the instrument from the element list — the two instruments
// then hashed alike.
#[test]
fn every_element_of_the_hash_identity_is_load_bearing() {
    let base = envelope("RETURN", params(&[]));
    let h0 = base.hash_identity().expect("h");

    let mut text = base.clone();
    text.statement = "RETURN x.id".into();
    assert_ne!(h0, text.hash_identity().expect("h"));

    let mut binds = base.clone();
    binds.bindings = params(&[("p", serde_json::json!(1))]);
    assert_ne!(h0, binds.hash_identity().expect("h"));

    let mut who = base.clone();
    who.provenance.initiator = Initiator::CoreMechanical {
        operation: "migration".into(),
    };
    assert_ne!(h0, who.hash_identity().expect("h"));

    let mut how = base.clone();
    how.provenance.instrument = Instrument {
        name: "import-tool".into(),
        version: "2".into(),
    };
    assert_ne!(h0, how.hash_identity().expect("h"));

    let mut nonce = base.clone();
    nonce.idempotency_nonce = Some("second-time".into());
    assert_ne!(h0, nonce.hash_identity().expect("h"));

    // Length prefixes make the concatenation injective: two bindings maps
    // whose JSON differs only in where the key boundary falls hash apart.
    let mut a = base.clone();
    a.bindings = params(&[("ab", serde_json::json!("c"))]);
    let mut b = base.clone();
    b.bindings = params(&[("a", serde_json::json!("bc"))]);
    assert_ne!(a.hash_identity().expect("h"), b.hash_identity().expect("h"));

    // The hex form round-trips.
    let hex = h0.to_hex();
    assert_eq!(hex.len(), 64);
    assert_eq!(TxHash::from_hex(&hex), Some(h0));
    assert_eq!(TxHash::from_hex("nope"), None);
}

/// GATE ONE: a format-version mismatch refuses NAMING BOTH VERSIONS.
// RED: naming only the envelope's version — the `supported_version`
// assertion fired.
#[test]
fn format_version_mismatch_refuses_naming_both_versions() {
    let mut env = envelope("MATCH (d: fixture-alpha/doc)", params(&[]));
    env.format_version = ENVELOPE_FORMAT_VERSION + 41;
    let err = env.validate().expect_err("a moved grammar is never best-effort parsed");
    match only_witness(&err) {
        Witness::VersionMismatch {
            envelope_version,
            supported_version,
        } => {
            assert_eq!(envelope_version, ENVELOPE_FORMAT_VERSION + 41);
            assert_eq!(supported_version, ENVELOPE_FORMAT_VERSION);
        }
        other => panic!("expected VersionMismatch, got {other:?}"),
    }
    // The version gate runs FIRST: a mismatched envelope is never parsed.
    let mut broken = envelope("this is not ladle", params(&[]));
    broken.format_version = 99;
    match only_witness(&broken.validate().expect_err("refuses")) {
        Witness::VersionMismatch { .. } => {}
        other => panic!("the version gate must precede the parse; got {other:?}"),
    }
}

/// GATE TWO: provenance is harness-supplied and must be COMPLETE. A blank
/// stamp is caller misuse, not a statement refusal.
// RED: checking only the instrument — the blank-thread case passed.
#[test]
fn incomplete_provenance_refuses_as_caller_misuse() {
    let complete = envelope("MATCH (d: fixture-alpha/doc)", params(&[]));
    complete.check_provenance().expect("a complete stamp passes");

    let blanks = [
        Provenance {
            initiator: Initiator::Agent {
                thread: "  ".into(),
                dispatch: "d".into(),
            },
            instrument: Instrument {
                name: "i".into(),
                version: "1".into(),
            },
        },
        Provenance {
            initiator: Initiator::Agent {
                thread: "t".into(),
                dispatch: String::new(),
            },
            instrument: Instrument {
                name: "i".into(),
                version: "1".into(),
            },
        },
        Provenance {
            initiator: Initiator::CoreMechanical {
                operation: String::new(),
            },
            instrument: Instrument {
                name: "i".into(),
                version: "1".into(),
            },
        },
        Provenance {
            initiator: Initiator::Agent {
                thread: "t".into(),
                dispatch: "d".into(),
            },
            instrument: Instrument {
                name: String::new(),
                version: "1".into(),
            },
        },
        Provenance {
            initiator: Initiator::Agent {
                thread: "t".into(),
                dispatch: "d".into(),
            },
            instrument: Instrument {
                name: "i".into(),
                version: String::new(),
            },
        },
    ];
    for p in blanks {
        let mut env = complete.clone();
        env.provenance = p;
        match env.check_provenance().expect_err("a blank stamp refuses") {
            SpineError::Usage { message } => {
                assert!(message.contains("harness-supplied"), "got: {message}")
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }
    // `external` is OUT OF SCOPE this build (external systems
    // never write directly; only the enum value is reserved as room). The
    // door refuses it loudly rather than letting an unbuilt boundary's
    // traffic through. (This test used to assert acceptance.)
    let mut ext = complete.clone();
    ext.provenance.initiator = Initiator::External;
    match ext
        .check_provenance()
        .expect_err("an external initiator refuses in this build")
    {
        SpineError::Usage { message } => {
            assert!(message.contains("out of scope"), "got: {message}");
            assert!(message.contains("external"), "got: {message}");
        }
        other => panic!("expected Usage, got {other:?}"),
    }
}

/// GATE FIVE: the bindings must cover every `$param`, both directions, and
/// `validate` hands the parsed statement back so the door parses once.
// RED: supplying `old_id` but not `title` — the missing name fired.
#[test]
fn envelope_validate_composes_every_gate() {
    let good = envelope(SUPERSEDE, supersede_params());
    let stmt = good.validate().expect("the worked example validates");
    assert_eq!(stmt.mutations.len(), 3);

    let short = envelope(SUPERSEDE, params(&[("old_id", serde_json::json!("d-40"))]));
    match only_witness(&short.validate().expect_err("an unbound parameter refuses")) {
        Witness::MissingParameter { name } => assert_eq!(name, "title"),
        other => panic!("expected MissingParameter, got {other:?}"),
    }

    // A staged-out form refuses BY NAME through the same entry point.
    let staged = envelope(r#"MATCH (d: fixture-alpha/doc { title: "x" })"#, params(&[]));
    assert_unimplemented(&staged.validate().expect_err("staged out"), FIELD_VALUE_FORM);

    // A malformed statement refuses structurally through the same entry point.
    let bad = envelope("MATCH (d: doc)", params(&[]));
    match only_witness(&bad.validate().expect_err("no pool")) {
        Witness::MalformedStatement { .. } => {}
        other => panic!("expected MalformedStatement, got {other:?}"),
    }
}
