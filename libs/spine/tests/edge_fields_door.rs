//! An edge record carries the fields its edge kind DECLARES, judged
//! at the one door, and one edge exists per (src, kind, dst).
//!
//! The model: declared per-kind fields on `EdgeRecord` (e.g.
//! `fields: {admits: {required, min_chars: 20}}` on `in_region`); edge
//! identity stays the triple, keeping the first; edges are a set.
//!
//! The grammar here is a TEST FIXTURE, not the v6 grammar: its caps are
//! chosen small so at-cap and cap+1 are cheap to write. Every guard was
//! forced RED once; the forcing is named in a `// RED:` comment above each
//! test.

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::pool::{EdgeRecord, NodeId, Pool};
use spine::schema_def::{
    grammar_fault_tag, CountCellDef, Direction, EdgeKindDef, FieldDef, FieldKind, NodeTypeDef,
    PoolGrammar, PoolKind, PoolName, TypeRef,
};
use spine::witness::Witness;
use spine::{Cauldron, Outcome};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

test_scratch::install!();

const POOL: &str = "fixture-edged";
const ADMITS_FLOOR: u64 = 20;
const ADMITS_CAP: u64 = 60;

/// `item` and `region`; `in_region` (item -> region) declares `admits`
/// (required text, 20..=60) and `rev` (optional integer); `ref` (item ->
/// item) declares `sentence` (optional text). Count cells: `region.members`
/// counts inbound `in_region`, `item.homes` counts outbound.
fn edged_grammar() -> PoolGrammar {
    let mut node_types = BTreeMap::new();
    node_types.insert(
        "item".to_string(),
        NodeTypeDef {
            fields: BTreeMap::from([("handle".to_string(), FieldDef::text(true, 235))]),
            count_cells: BTreeMap::from([(
                "homes".to_string(),
                CountCellDef {
                    edge_kind: "in_region".to_string(),
                    direction: Direction::Out,
                },
            )]),
            ..Default::default()
        },
    );
    node_types.insert(
        "region".to_string(),
        NodeTypeDef {
            fields: BTreeMap::from([("name".to_string(), FieldDef::text(true, 235))]),
            count_cells: BTreeMap::from([(
                "members".to_string(),
                CountCellDef {
                    edge_kind: "in_region".to_string(),
                    direction: Direction::In,
                },
            )]),
            ..Default::default()
        },
    );
    let mut edge_kinds = BTreeMap::new();
    edge_kinds.insert(
        "in_region".to_string(),
        EdgeKindDef {
            fields: BTreeMap::from([
                (
                    "admits".to_string(),
                    FieldDef {
                        required: true,
                        kind: FieldKind::Text,
                        max_chars: Some(ADMITS_CAP),
                        min_chars: Some(ADMITS_FLOOR),
                    },
                ),
                (
                    "rev".to_string(),
                    FieldDef {
                        kind: FieldKind::Integer,
                        ..FieldDef::optional()
                    },
                ),
            ]),
            ..EdgeKindDef::between(["item"], [TypeRef::local("region")])
        },
    );
    edge_kinds.insert(
        "ref".to_string(),
        EdgeKindDef {
            fields: BTreeMap::from([(
                "sentence".to_string(),
                FieldDef {
                    kind: FieldKind::Text,
                    ..FieldDef::optional()
                },
            )]),
            ..EdgeKindDef::between(["item"], [TypeRef::local("item")])
        },
    );
    PoolGrammar {
        pool: PoolName::new(POOL),
        kind: PoolKind::new("edged"),
        version: 1,
        node_types,
        edge_kinds,
    }
}

// ---------------------------------------------------------------------------
// guard 1: a declared edge field round-trips through the door into storage
// ---------------------------------------------------------------------------

/// Two edges with DIFFERENT fields are each read back with their OWN fields
/// off the pool FILE (a fresh `Pool::open`, not the statement or the
/// cauldron's handle), and a field-less `ref` reads back empty.
// RED: `pool.rs`'s INSERT bound `'{}'` in place of the fields JSON — both
// read-backs came back `{}`. Opposed: a constant `fields` would have to
// equal two different maps at once.
#[test]
fn a_declared_edge_field_round_trips_through_submit_into_the_stored_edge() {
    let dir = root("round-trip");
    let c = fresh(&dir);
    let item = mint_item(&c);
    let r1 = mint_region(&c);
    let r2 = mint_region(&c);
    let a1 = admits(25, 'a');
    let a2 = admits(40, 'b');
    assert_ne!(a1, a2, "the sentinel: two different field values");

    connect(&c, &item, &r1, json!({"admits": a1, "rev": 7})).expect("first attestation lands");
    connect(&c, &item, &r2, json!({"admits": a2})).expect("second attestation lands");
    let other = mint_item(&c);
    committed(c.submit(commit(
        r#"MATCH (a: fixture-edged/item { id: $a }) MATCH (b: fixture-edged/item { id: $b })
           CREATE (a) -[ref]-> (b)"#,
        &[("a", json!(item.0)), ("b", json!(other.0))],
    )));

    let stored = edges_from(&dir, &item);
    let e1 = find(&stored, "in_region", &r1);
    let e2 = find(&stored, "in_region", &r2);
    assert_eq!(
        e1.fields,
        BTreeMap::from([("admits".to_string(), json!(a1)), ("rev".to_string(), json!(7))])
    );
    assert_eq!(e2.fields, BTreeMap::from([("admits".to_string(), json!(a2))]));
    assert_eq!(find(&stored, "ref", &other).fields, BTreeMap::new());
    assert_eq!(e1.note, None, "an object NOTE fills fields, never the free note");
}

// ---------------------------------------------------------------------------
// guard 2: the cap law on an edge field, by value
// ---------------------------------------------------------------------------

/// At the cap lands; cap+1 refuses `EdgeFieldOverCap` with measured=cap+1,
/// cap, overage=1; at the floor lands; floor-1 refuses `EdgeFieldUnderFloor`.
/// Nothing lands on a refusal.
// RED: (a) `check_edge_fields` skipped `check_field_value` — the 61-character
// admits committed; (b) `check_field_value`'s `measured > cap` flipped to
// `>=` — the at-cap create refused (opposes a constant "refuse").
#[test]
fn an_edge_field_one_over_its_cap_refuses_with_measured_cap_overage_and_at_cap_lands() {
    let dir = root("cap");
    let c = fresh(&dir);
    let item = mint_item(&c);
    let regions: Vec<NodeId> = (0..4).map(|_| mint_region(&c)).collect();

    // One over the cap: refused, by value, nothing landed.
    let w = refused(connect(&c, &item, &regions[0], json!({"admits": admits(ADMITS_CAP as usize + 1, 'o')})));
    match &w {
        Witness::EdgeFieldOverCap {
            pool,
            edge_kind,
            field,
            measured,
            cap,
            overage,
            ..
        } => {
            assert_eq!(pool, &PoolName::new(POOL));
            assert_eq!(edge_kind, "in_region");
            assert_eq!(field, "admits");
            assert_eq!((*measured, *cap, *overage), (ADMITS_CAP + 1, ADMITS_CAP, 1));
        }
        other => panic!("expected EdgeFieldOverCap, got {other:?}"),
    }
    assert_eq!(grammar_fault_tag(&w), Some("over_cap"));
    assert!(edges_from(&dir, &item).is_empty(), "the refusal landed nothing");

    // Exactly the cap: accepted.
    connect(&c, &item, &regions[1], json!({"admits": admits(ADMITS_CAP as usize, 'c')}))
        .expect("an admits AT the cap lands");

    // One under the floor: refused, by value.
    let w = refused(connect(&c, &item, &regions[2], json!({"admits": admits(ADMITS_FLOOR as usize - 1, 'u')})));
    match &w {
        Witness::EdgeFieldUnderFloor {
            edge_kind,
            field,
            measured,
            floor,
            shortfall,
            ..
        } => {
            assert_eq!((edge_kind.as_str(), field.as_str()), ("in_region", "admits"));
            assert_eq!((*measured, *floor, *shortfall), (ADMITS_FLOOR - 1, ADMITS_FLOOR, 1));
        }
        other => panic!("expected EdgeFieldUnderFloor, got {other:?}"),
    }

    // Exactly the floor: accepted.
    connect(&c, &item, &regions[3], json!({"admits": admits(ADMITS_FLOOR as usize, 'f')}))
        .expect("an admits AT the floor lands");
    assert_eq!(edges_from(&dir, &item).len(), 2, "exactly the two in-bounds edges landed");
}

// ---------------------------------------------------------------------------
// guard 3: undeclared and missing-required edge fields, refused by name
// ---------------------------------------------------------------------------

/// A field the kind does not declare refuses `undeclared_edge_field` naming
/// it; an edge missing its required `admits` refuses
/// `missing_required_edge_field` naming it; a wrong-kind value refuses
/// `wrong_field_kind`. The same statement with the offending part fixed
/// lands (opposes a constant "refuse").
// RED: (a) the `undeclared_edge_field` arm returned `Ok` — the `bogus`
// edge committed; (b) the required loop removed — the admits-less edge
// committed.
#[test]
fn an_undeclared_edge_field_and_a_missing_required_one_each_refuse_by_name() {
    let dir = root("names");
    let c = fresh(&dir);
    let item = mint_item(&c);
    let region = mint_region(&c);
    let good = admits(30, 'g');

    let w = refused(connect(&c, &item, &region, json!({"admits": good, "bogus": "x"})));
    assert_eq!(grammar_fault_tag(&w), Some("undeclared_edge_field"), "{w:?}");
    assert!(message(&w).contains("`bogus`"), "names the field: {w:?}");

    let w = refused(connect(&c, &item, &region, json!({"rev": 1})));
    assert_eq!(grammar_fault_tag(&w), Some("missing_required_edge_field"), "{w:?}");
    assert!(message(&w).contains("`admits`"), "names the field: {w:?}");

    let w = refused(connect(&c, &item, &region, json!({"admits": good, "rev": "one"})));
    assert_eq!(grammar_fault_tag(&w), Some("wrong_field_kind"), "{w:?}");

    assert!(edges_from(&dir, &item).is_empty(), "no refusal landed an edge");
    connect(&c, &item, &region, json!({"admits": good, "rev": 1})).expect("the fixed edge lands");
    assert_eq!(edges_from(&dir, &item).len(), 1);
}

// ---------------------------------------------------------------------------
// guard 4: one edge per triple; the differing-fields rule
// ---------------------------------------------------------------------------

/// The same (src, kind, dst) created twice with IDENTICAL fields, as two
/// distinct transactions: both commit, ONE edge is stored, and each count
/// cell moved ONCE. The same triple with DIFFERENT fields refuses
/// `edge_fields_differ` naming the field, and the FIRST edge's fields stand.
/// Inside one statement the same holds; REMOVE then CREATE changes them.
// RED: (a) `check_edge_fields_agree` call removed from phase 10b — the
// differing create COMMITTED (a silent drop: the stored admits stayed the
// first); (b) the identical-fields arm `Some(held) if held == *fields`
// removed — the identical second create refused (opposes a constant
// "refuse"); (c) `pool.rs`'s count-cell bump made unconditional
// (`inserted <= 1`) — the identical re-create moved both cells to 2.
#[test]
fn one_edge_per_triple_identical_recreate_is_a_no_op_and_differing_fields_refuse() {
    let dir = root("triple");
    let c = fresh(&dir);
    let item = mint_item(&c);
    let region = mint_region(&c);
    let first = admits(30, 'p');
    let second = admits(30, 'q');

    connect(&c, &item, &region, json!({"admits": first})).expect("first create");
    assert_eq!(cells(&dir, &item, &region), (1, 1));
    connect(&c, &item, &region, json!({"admits": first})).expect("identical re-create is a no-op commit");
    let stored = edges_from(&dir, &item);
    assert_eq!(stored.len(), 1, "one edge per triple: {stored:?}");
    assert_eq!(cells(&dir, &item, &region), (1, 1), "the count cells moved once");

    let w = refused(connect(&c, &item, &region, json!({"admits": second})));
    assert_eq!(grammar_fault_tag(&w), Some("edge_fields_differ"), "{w:?}");
    assert!(message(&w).contains("`admits`"), "names the differing field: {w:?}");
    let stored = edges_from(&dir, &item);
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].fields["admits"], json!(first), "the first edge's fields stand");
    assert_eq!(cells(&dir, &item, &region), (1, 1));

    // Inside ONE statement: two creates of one triple with different fields.
    let other = mint_region(&c);
    let w = refused(c.submit(commit(
        r#"MATCH (i: fixture-edged/item { id: $i }) MATCH (r: fixture-edged/region { id: $r })
           CREATE (i) -[in_region]-> (r) NOTE $a
           CREATE (i) -[in_region]-> (r) NOTE $b"#,
        &[
            ("i", json!(item.0)),
            ("r", json!(other.0)),
            ("a", json!({"admits": first})),
            ("b", json!({"admits": second})),
        ],
    )));
    assert_eq!(grammar_fault_tag(&w), Some("edge_fields_differ"), "{w:?}");
    assert_eq!(edges_from(&dir, &item).len(), 1, "nothing landed");

    // REMOVE then CREATE in one statement is how fields change.
    committed(c.submit(commit(
        r#"MATCH (i: fixture-edged/item { id: $i }) MATCH (r: fixture-edged/region { id: $r })
           REMOVE (i) -[in_region]-> (r)
           CREATE (i) -[in_region]-> (r) NOTE $b"#,
        &[("i", json!(item.0)), ("r", json!(region.0)), ("b", json!({"admits": second}))],
    )));
    let stored = edges_from(&dir, &item);
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].fields["admits"], json!(second), "the replacement's fields are stored");
    assert_eq!(cells(&dir, &item, &region), (1, 1));
}

// ---------------------------------------------------------------------------
// guard 5: removing a fielded edge removes the record and moves the cell
// ---------------------------------------------------------------------------

/// REMOVE of a fielded edge deletes its record and moves both count cells
/// back; a second fielded edge on the same source is untouched (opposes a
/// constant "delete everything").
// RED: `pool.rs`'s RemoveEdge DELETE made a no-op (`AND 0`) — the edge was
// still stored and `members` stayed 1.
#[test]
fn removing_a_fielded_edge_removes_its_record_and_its_count_cells() {
    let dir = root("remove");
    let c = fresh(&dir);
    let item = mint_item(&c);
    let gone = mint_region(&c);
    let kept = mint_region(&c);
    connect(&c, &item, &gone, json!({"admits": admits(30, 'r'), "rev": 2})).expect("lands");
    connect(&c, &item, &kept, json!({"admits": admits(30, 'k')})).expect("lands");
    assert_eq!(cells(&dir, &item, &gone), (2, 1));

    committed(c.submit(commit(
        r#"MATCH (i: fixture-edged/item { id: $i }) MATCH (r: fixture-edged/region { id: $r })
           REMOVE (i) -[in_region]-> (r)"#,
        &[("i", json!(item.0)), ("r", json!(gone.0))],
    )));
    let stored = edges_from(&dir, &item);
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].dst.id, kept, "the other edge is untouched");
    assert_eq!(cells(&dir, &item, &gone), (1, 0), "both cells moved back once");
    assert_eq!(cells(&dir, &item, &kept), (1, 1));
}

/// The grammar's own coherence for edge fields: a cap on a non-text edge
/// field and a floor above its cap are refused at create; the fixture passes.
// RED: the edge-field loop in `check_wellformed` removed — both malformed
// grammars were accepted.
#[test]
fn check_wellformed_refuses_malformed_edge_fields_and_accepts_the_fixture() {
    edged_grammar().check_wellformed().expect("the fixture is well-formed");
    let mut g = edged_grammar();
    g.edge_kinds.get_mut("in_region").unwrap().fields.get_mut("rev").unwrap().max_chars = Some(3);
    assert!(g.check_wellformed().is_err(), "a cap on an integer edge field refuses");
    let mut g = edged_grammar();
    g.edge_kinds.get_mut("in_region").unwrap().fields.get_mut("admits").unwrap().min_chars = Some(61);
    assert!(g.check_wellformed().is_err(), "a floor above its cap refuses");
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-edge-fields-{name}-{}", std::process::id()))
}

fn fresh(dir: &Path) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(dir, &[edged_grammar()], SpineConfig::default()).expect("create the cauldron")
}

static SERIAL: AtomicU32 = AtomicU32::new(0);

fn serial() -> u32 {
    SERIAL.fetch_add(1, Ordering::SeqCst)
}

/// Exactly `n` characters, starting with `c`.
fn admits(n: usize, c: char) -> String {
    std::iter::repeat(c).take(n).collect()
}

/// A COMMIT envelope with a fresh nonce: the door is exactly-once over the
/// envelope hash, so a repeated statement must be a DISTINCT transaction to
/// test what a second create of the same triple does.
fn commit(statement: &str, params: &[(&str, Value)]) -> Envelope {
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
                thread: "edge-fields".into(),
                dispatch: "edge-fields-door".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(format!("nonce-{}", serial())),
    }
}

fn mint_item(c: &Cauldron) -> NodeId {
    returned_id(
        committed(c.submit(commit(
            r#"CREATE (n: fixture-edged/item { handle: $h }) RETURN n.id"#,
            &[("h", json!(format!("item {}", serial())))],
        ))),
        "n.id",
    )
}

fn mint_region(c: &Cauldron) -> NodeId {
    returned_id(
        committed(c.submit(commit(
            r#"CREATE (r: fixture-edged/region { name: $n }) RETURN r.id"#,
            &[("n", json!(format!("region {}", serial())))],
        ))),
        "r.id",
    )
}

/// `CREATE (i) -[in_region]-> (r) NOTE $f` with `$f` the edge's fields.
fn connect(c: &Cauldron, item: &NodeId, region: &NodeId, fields: Value) -> spine::error::Result<Outcome> {
    c.submit(commit(
        r#"MATCH (i: fixture-edged/item { id: $i }) MATCH (r: fixture-edged/region { id: $r })
           CREATE (i) -[in_region]-> (r) NOTE $f"#,
        &[("i", json!(item.0)), ("r", json!(region.0)), ("f", fields)],
    ))
}

#[track_caller]
fn committed(r: spine::error::Result<Outcome>) -> Outcome {
    match r {
        Ok(o @ Outcome::Committed { .. }) => o,
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn returned_id(outcome: Outcome, column: &str) -> NodeId {
    match outcome {
        Outcome::Committed { returned, .. } => {
            NodeId(returned.0[0][column].as_str().expect("id").to_string())
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}

#[track_caller]
fn refused<T: std::fmt::Debug>(r: spine::error::Result<T>) -> Witness {
    match r {
        Err(SpineError::Refused { witnesses }) => {
            assert_eq!(witnesses.len(), 1, "one refusal, one witness: {witnesses:?}");
            witnesses[0].clone()
        }
        other => panic!("expected a structured refusal, got {other:?}"),
    }
}

#[track_caller]
fn message(w: &Witness) -> &str {
    match w {
        Witness::GrammarViolation { message, .. } => message,
        other => panic!("expected GrammarViolation, got {other:?}"),
    }
}

/// The edges leaving `src`, read off the POOL FILE through a fresh handle.
fn edges_from(dir: &Path, src: &NodeId) -> Vec<EdgeRecord> {
    let p = Pool::open(&Pool::path_in(dir, &PoolName::new(POOL))).expect("pool");
    let s = p.snapshot().expect("snapshot");
    s.edges_from(src, None).expect("edges")
}

fn find<'a>(edges: &'a [EdgeRecord], kind: &str, dst: &NodeId) -> &'a EdgeRecord {
    edges
        .iter()
        .find(|e| e.kind == kind && e.dst.id == *dst)
        .unwrap_or_else(|| panic!("no `{kind}` edge to `{dst}` in {edges:?}"))
}

/// (`item.homes`, `region.members`), off the pool file; absent reads 0.
fn cells(dir: &Path, item: &NodeId, region: &NodeId) -> (i64, i64) {
    let p = Pool::open(&Pool::path_in(dir, &PoolName::new(POOL))).expect("pool");
    let s = p.snapshot().expect("snapshot");
    (
        s.count_cell(item, "homes").expect("cell").unwrap_or(0),
        s.count_cell(region, "members").expect("cell").unwrap_or(0),
    )
}
