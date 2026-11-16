//! The grammar additions a v6 workspace needs, refused BY NAME at
//! the one door: per-field character caps and kinds, required-at-mint edges
//! with minimum counts, same-type-as-source edge targets, an id prefix per
//! node type, and the cap law's structured witness (measured, cap, overage —
//! every cap HARD by one character).
//!
//! The grammar here is a TEST FIXTURE shaped like the rules, NOT the v6
//! grammar. Its numbers are the v5 rule table's — handle
//! 235, body 10,000, boundary 40..1,000 — not invented.
//!
//! Every guard was forced RED at least once against a deliberately broken
//! implementation and once against a degenerate one; the
//! forcing is named in a `// RED:` comment above each test.

use spine::config::SpineConfig;
use spine::delta::{CompiledPart, Delta};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::pool::NodeId;
use spine::schema_def::{
    grammar_fault_tag, EdgeKindDef, FarSide, FieldDef, FieldKind, NodeTypeDef, PoolGrammar,
    PoolKind, PoolName, TypeRef,
};
use spine::witness::Witness;
use spine::{Cauldron, Outcome, ReadRequest};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

test_scratch::install!();

const POOL: &str = "fixture-capped";

/// The v5 numbers (handle <=235; body <=10,000 the WRITE cap;
/// boundary 40 to 1,000 raw characters).
const HANDLE_CAP: u64 = 235;
const BODY_CAP: u64 = 10_000;
const BOUNDARY_FLOOR: u64 = 40;
const BOUNDARY_CAP: u64 = 1_000;

/// The fixture: `note` (capped handle and body, an integer rank, an object
/// meta, requires ONE `serves` at mint), `pair` (requires TWO `serves` at
/// mint, so the witness's `minimum` is a value and not a constant), `bucket`
/// (a floored and capped boundary), `stamped` (an id prefix `ST`);
/// `supersedes` leaves any of them and arrives only at the source's own
/// type.
fn capped_grammar() -> PoolGrammar {
    let text = |required: bool, max: u64| FieldDef::text(required, max);
    let mut node_types = BTreeMap::new();
    node_types.insert(
        "note".to_string(),
        NodeTypeDef {
            fields: BTreeMap::from([
                ("handle".to_string(), text(true, HANDLE_CAP)),
                ("body".to_string(), text(false, BODY_CAP)),
                (
                    "rank".to_string(),
                    FieldDef {
                        kind: FieldKind::Integer,
                        ..FieldDef::optional()
                    },
                ),
                (
                    "meta".to_string(),
                    FieldDef {
                        kind: FieldKind::Object,
                        ..FieldDef::optional()
                    },
                ),
            ]),
            states: vec!["open".to_string(), "closed".to_string()],
            required_edges_at_mint: BTreeMap::from([("serves".to_string(), 1)]),
            ..Default::default()
        },
    );
    node_types.insert(
        "pair".to_string(),
        NodeTypeDef {
            fields: BTreeMap::from([("handle".to_string(), text(true, HANDLE_CAP))]),
            required_edges_at_mint: BTreeMap::from([("serves".to_string(), 2)]),
            ..Default::default()
        },
    );
    node_types.insert(
        "bucket".to_string(),
        NodeTypeDef {
            fields: BTreeMap::from([(
                "boundary".to_string(),
                FieldDef {
                    required: true,
                    kind: FieldKind::Text,
                    max_chars: Some(BOUNDARY_CAP),
                    min_chars: Some(BOUNDARY_FLOOR),
                },
            )]),
            ..Default::default()
        },
    );
    node_types.insert(
        "stamped".to_string(),
        NodeTypeDef {
            fields: BTreeMap::from([("name".to_string(), FieldDef::required())]),
            id_prefix: Some("ST".to_string()),
            ..Default::default()
        },
    );
    let mut edge_kinds = BTreeMap::new();
    edge_kinds.insert(
        "serves".to_string(),
        EdgeKindDef::between(["note", "pair"], [TypeRef::local("bucket")]),
    );
    edge_kinds.insert(
        "supersedes".to_string(),
        EdgeKindDef::between(["note", "pair", "bucket", "stamped"], [TypeRef::SameAsSource]),
    );
    PoolGrammar {
        pool: PoolName::new(POOL),
        kind: PoolKind::new("capped"),
        version: 1,
        node_types,
        edge_kinds,
    }
}

// ---------------------------------------------------------------------------
// character caps: hard by one, witnessed by value
// ---------------------------------------------------------------------------

/// A handle of exactly the cap lands; one character more refuses with
/// `OverCap` naming measured, cap and overage BY VALUE; and the cap is
/// metered in CHARACTERS (235 two-byte characters land).
// RED: (a) `check_field_value`'s `measured > cap` flipped to `>=` — the
// at-cap create refused (the acceptance half); (b) the cap arm removed — the
// 236-character create committed (the refusal half); (c) `overage`
// hard-coded to 1 — the 300-character case's `overage == 65` failed; (d)
// `measure_chars` changed to `s.len()` — the 235 two-byte-character handle
// refused as 470.
#[test]
fn a_handle_at_its_cap_lands_and_one_character_over_refuses_with_measured_cap_and_overage() {
    let dir = root("handle-cap");
    let c = fresh(&dir);
    let b = mint_bucket(&c);
    let before = count(&c, "note");

    // Exactly the cap: accepted.
    let at_cap = "h".repeat(HANDLE_CAP as usize);
    create_note(&c, &b, &at_cap).expect("a handle AT the cap lands");
    assert_eq!(count(&c, "note"), before + 1);

    // One over: refused, by value.
    let over_by_one = "h".repeat(HANDLE_CAP as usize + 1);
    let w = refused(create_note(&c, &b, &over_by_one));
    match &w {
        Witness::OverCap {
            pool,
            node_type,
            field,
            measured,
            cap,
            overage,
            ..
        } => {
            assert_eq!(pool, &PoolName::new(POOL));
            assert_eq!(node_type, "note");
            assert_eq!(field, "handle");
            assert_eq!(*measured, HANDLE_CAP + 1);
            assert_eq!(*cap, HANDLE_CAP);
            assert_eq!(*overage, 1);
        }
        other => panic!("expected OverCap, got {other:?}"),
    }
    assert_eq!(grammar_fault_tag(&w), Some("over_cap"));
    assert_eq!(count(&c, "note"), before + 1, "the refusal landed nothing");

    // Well over: the numbers are measured, not constants.
    let w = refused(create_note(&c, &b, &"h".repeat(300)));
    match w {
        Witness::OverCap {
            measured,
            cap,
            overage,
            ..
        } => {
            assert_eq!((measured, cap, overage), (300, HANDLE_CAP, 300 - HANDLE_CAP));
        }
        other => panic!("expected OverCap, got {other:?}"),
    }

    // Characters, never bytes: 235 x 'é' is 470 bytes and AT the cap.
    let accented = "é".repeat(HANDLE_CAP as usize);
    assert_eq!(accented.len(), 470, "the sentinel: two bytes per character");
    create_note(&c, &b, &accented).expect("235 characters of two bytes each land");
    assert_eq!(count(&c, "note"), before + 2);
}

/// The same cap binds a SET as it binds a CREATE — one function, two
/// writes — and the body's own number is the body's (10,000), not the
/// handle's.
// RED: the SET arm of `validate_delta` returned `Ok(())` for a bare field
// path before reaching `check_field_value` — the 10,001-character SET
// committed.
#[test]
fn the_body_cap_binds_set_as_it_binds_create_at_its_own_number() {
    let dir = root("body-cap");
    let c = fresh(&dir);
    let b = mint_bucket(&c);
    let n = create_note(&c, &b, "a note").expect("mint");

    let at_cap = "b".repeat(BODY_CAP as usize);
    c.submit(commit(
        r#"MATCH (n: fixture-capped/note { id: $id }) SET n.body = $body"#,
        &[("id", json(&n.0)), ("body", json(&at_cap))],
    ))
    .expect("a body AT the cap lands by SET");

    let over = "b".repeat(BODY_CAP as usize + 1);
    let w = refused(c.submit(commit(
        r#"MATCH (n: fixture-capped/note { id: $id }) SET n.body = $body"#,
        &[("id", json(&n.0)), ("body", json(&over))],
    )));
    match w {
        Witness::OverCap {
            node_type,
            field,
            measured,
            cap,
            overage,
            ..
        } => {
            assert_eq!((node_type.as_str(), field.as_str()), ("note", "body"));
            assert_eq!((measured, cap, overage), (BODY_CAP + 1, BODY_CAP, 1));
        }
        other => panic!("expected OverCap, got {other:?}"),
    }
    // And the CREATE path, at the body's number.
    let w = refused(c.submit(commit(
        r#"MATCH (b: fixture-capped/bucket { id: $b })
           CREATE (n: fixture-capped/note { handle: "h", body: $body, state: "open" })
           CREATE (n) -[serves]-> (b)"#,
        &[("b", json(&b.0)), ("body", json(&over))],
    )));
    assert!(
        matches!(&w, Witness::OverCap { field, cap, .. } if field == "body" && *cap == BODY_CAP),
        "{w:?}"
    );
}

/// A floor is the cap law's other half: a boundary of 39 characters refuses
/// `UnderFloor` with measured, floor and shortfall by value; 40 lands.
// RED: (a) `measured < floor` flipped to `<=` — the 40-character boundary
// refused; (b) the floor arm removed — the 39-character boundary committed.
#[test]
fn a_boundary_under_its_floor_refuses_with_measured_floor_and_shortfall() {
    let dir = root("floor");
    let c = fresh(&dir);
    let before = count(&c, "bucket");

    let short = "x".repeat(BOUNDARY_FLOOR as usize - 1);
    let w = refused(c.submit(commit(
        r#"CREATE (b: fixture-capped/bucket { boundary: $t }) RETURN b.id"#,
        &[("t", json(&short))],
    )));
    match &w {
        Witness::UnderFloor {
            node_type,
            field,
            measured,
            floor,
            shortfall,
            ..
        } => {
            assert_eq!((node_type.as_str(), field.as_str()), ("bucket", "boundary"));
            assert_eq!((*measured, *floor, *shortfall), (BOUNDARY_FLOOR - 1, BOUNDARY_FLOOR, 1));
        }
        other => panic!("expected UnderFloor, got {other:?}"),
    }
    assert_eq!(grammar_fault_tag(&w), Some("under_floor"));
    assert_eq!(count(&c, "bucket"), before, "the refusal landed nothing");

    // A shortfall that is not 1.
    let w = refused(c.submit(commit(
        r#"CREATE (b: fixture-capped/bucket { boundary: "ten chars." }) RETURN b.id"#,
        &[],
    )));
    assert!(
        matches!(&w, Witness::UnderFloor { measured: 10, floor, shortfall, .. }
            if *floor == BOUNDARY_FLOOR && *shortfall == BOUNDARY_FLOOR - 10),
        "{w:?}"
    );

    mint_bucket(&c); // exactly 40: lands
    assert_eq!(count(&c, "bucket"), before + 1);
}

// ---------------------------------------------------------------------------
// field kinds
// ---------------------------------------------------------------------------

/// A value of the wrong JSON shape refuses `wrong_field_kind`; the right
/// shape lands. A dotted SET into a text field refuses the same way even
/// when the field is absent (where the landing would otherwise mint an
/// object inside a field declared to hold text); into an object field it
/// lands.
// RED: (a) `FieldKind::admits` returned `true` for every kind — the string
// rank committed; (b) the dotted-SET arm's `addressable()` check removed —
// `SET n.body.lang` on an absent body committed and the body became an
// object.
#[test]
fn a_value_of_the_wrong_kind_refuses_and_a_dotted_set_into_text_refuses() {
    let dir = root("kinds");
    let c = fresh(&dir);
    let b = mint_bucket(&c);
    let n = create_note(&c, &b, "a note").expect("mint");

    let set_rank = |v: serde_json::Value| {
        c.submit(commit(
            r#"MATCH (n: fixture-capped/note { id: $id }) SET n.rank = $v"#,
            &[("id", json(&n.0)), ("v", v)],
        ))
    };
    let w = refused(set_rank(serde_json::json!("7")));
    assert_eq!(grammar_fault_tag(&w), Some("wrong_field_kind"), "{w:?}");
    let w = refused(set_rank(serde_json::json!(7.5)));
    assert_eq!(grammar_fault_tag(&w), Some("wrong_field_kind"), "{w:?}");
    set_rank(serde_json::json!(7)).expect("an integer rank lands");

    // The CREATE path judges the same kind.
    let w = refused(c.submit(commit(
        r#"MATCH (b: fixture-capped/bucket { id: $b })
           CREATE (n: fixture-capped/note { handle: "h", rank: "seven", state: "open" })
           CREATE (n) -[serves]-> (b)"#,
        &[("b", json(&b.0))],
    )));
    assert_eq!(grammar_fault_tag(&w), Some("wrong_field_kind"), "{w:?}");

    // A dotted SET into an ABSENT text field: refused by name, never an
    // object minted where text belongs.
    let w = refused(c.submit(commit(
        r#"MATCH (n: fixture-capped/note { id: $id }) SET n.body.lang = "en""#,
        &[("id", json(&n.0))],
    )));
    assert_eq!(grammar_fault_tag(&w), Some("wrong_field_kind"), "{w:?}");
    // ...and into an object field it lands.
    c.submit(commit(
        r#"MATCH (n: fixture-capped/note { id: $id }) SET n.meta.lang = "en""#,
        &[("id", json(&n.0))],
    ))
    .expect("a dotted SET into an object field lands");
    let rows = c
        .read(&ReadRequest {
            statement: r#"MATCH (n: fixture-capped/note { id: $id }) RETURN n.body, n.meta"#.into(),
            bindings: bindings(&[("id", json(&n.0))]),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect("read")
        .rows;
    assert_eq!(rows.0.len(), 1);
    assert_eq!(rows.0[0]["n.body"], serde_json::Value::Null, "the body stayed absent");
    assert_eq!(rows.0[0]["n.meta"], serde_json::json!({ "lang": "en" }));
}

// ---------------------------------------------------------------------------
// required-at-mint edges
// ---------------------------------------------------------------------------

/// A note minted without its `serves` refuses `RequiredEdgeMissing` naming
/// the kind, the minimum and the count found; minted with the edge in the
/// same statement it lands. A `pair` needs TWO, so one is a shortfall of one
/// — `minimum` and `found` are values, not constants.
// RED: (a) the required-edge loop in `validate_part` removed — the bare
// note committed; (b) `found` hard-coded to 0 — the one-edged pair's
// witness said `found: 0`; (c) `minimum` compared with `<=` — the
// one-edged note (exactly its minimum) refused.
#[test]
fn a_node_minted_without_its_required_edge_refuses_naming_the_kind_and_the_minimum() {
    let dir = root("required-edge");
    let c = fresh(&dir);
    let b = mint_bucket(&c);
    let before = count(&c, "note");

    let w = refused(c.submit(commit(
        r#"CREATE (n: fixture-capped/note { handle: "alone", state: "open" }) RETURN n.id"#,
        &[],
    )));
    match &w {
        Witness::RequiredEdgeMissing {
            pool,
            node_type,
            edge_kind,
            minimum,
            found,
            ..
        } => {
            assert_eq!(pool, &PoolName::new(POOL));
            assert_eq!(node_type, "note");
            assert_eq!(edge_kind, "serves");
            assert_eq!((*minimum, *found), (1, 0));
        }
        other => panic!("expected RequiredEdgeMissing, got {other:?}"),
    }
    assert_eq!(grammar_fault_tag(&w), Some("required_edge_missing"));
    assert_eq!(count(&c, "note"), before, "the refusal landed nothing");

    // With the edge, in the same statement: lands, and the edge is there.
    let n = create_note(&c, &b, "served").expect("a note minted WITH its serves lands");
    assert_eq!(count(&c, "note"), before + 1);
    let rows = c
        .read(&ReadRequest {
            statement: r#"MATCH (n: fixture-capped/note { id: $id }) -[serves]-> (b: fixture-capped/bucket) RETURN b.id"#
                .into(),
            bindings: bindings(&[("id", json(&n.0))]),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect("read")
        .rows;
    assert_eq!(rows.0.len(), 1, "the serves edge landed with the note");
    assert_eq!(rows.0[0]["b.id"], json(&b.0));

    // A pair with ONE of its two: short by one, by value.
    let w = refused(c.submit(commit(
        r#"MATCH (b: fixture-capped/bucket { id: $b })
           CREATE (p: fixture-capped/pair { handle: "one of two" })
           CREATE (p) -[serves]-> (b)"#,
        &[("b", json(&b.0))],
    )));
    assert!(
        matches!(&w, Witness::RequiredEdgeMissing { node_type, edge_kind, minimum: 2, found: 1, .. }
            if node_type == "pair" && edge_kind == "serves"),
        "{w:?}"
    );
    // Two `serves` CLAUSES toward the SAME bucket are ONE edge — an edge is
    // keyed (src, kind, dst) and the landing stores one row per key — so a
    // pair minted that way is still short by one (an earlier version
    // counted statements and let this land with one edge stored).
    let w = refused(c.submit(commit(
        r#"MATCH (b: fixture-capped/bucket { id: $b })
           CREATE (p: fixture-capped/pair { handle: "twice to one" })
           CREATE (p) -[serves]-> (b)
           CREATE (p) -[serves]-> (b)"#,
        &[("b", json(&b.0))],
    )));
    assert!(
        matches!(&w, Witness::RequiredEdgeMissing { node_type, edge_kind, minimum: 2, found: 1, .. }
            if node_type == "pair" && edge_kind == "serves"),
        "{w:?}"
    );
    assert_eq!(count(&c, "pair"), 0, "the refusal landed nothing");
    // With two (toward two DISTINCT buckets): lands, and both edges are
    // there. The sentinel first: the two targets ARE two nodes.
    let b2 = mint_bucket(&c);
    assert_ne!(b, b2, "two mints, two buckets — not one deduplicated commit");
    c.submit(commit(
        r#"MATCH (b: fixture-capped/bucket { id: $b })
           MATCH (c: fixture-capped/bucket { id: $c })
           CREATE (p: fixture-capped/pair { handle: "two of two" })
           CREATE (p) -[serves]-> (b)
           CREATE (p) -[serves]-> (c)"#,
        &[("b", json(&b.0)), ("c", json(&b2.0))],
    ))
    .expect("a pair minted with both its serves lands");
    assert_eq!(count(&c, "pair"), 1);
    let rows = c
        .read(&ReadRequest {
            statement: r#"MATCH (p: fixture-capped/pair) -[serves]-> (b: fixture-capped/bucket) RETURN b.id"#
                .into(),
            bindings: Bindings::new(),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect("read")
        .rows;
    let mut served: Vec<String> = rows
        .0
        .iter()
        .map(|r| r["b.id"].as_str().expect("id").to_string())
        .collect();
    served.sort();
    let mut want = vec![b.0.clone(), b2.0.clone()];
    want.sort();
    assert_eq!(served, want, "two distinct serves edges landed with the pair");
}

/// A required edge the minting statement CREATEs and then REMOVEs does not
/// count: what is judged is what LANDS, the part's adds and removes applied
/// in order (an add-only count once committed a note with zero
/// `serves` and a pair with one). Three tests so each forcing's red is its
/// own line: the note (found 0), the pair (found 1), and the opposing
/// CREATE, REMOVE, re-CREATE whose edge survives and lands — so the rule is
/// "count the survivors", not "refuse any REMOVE".
// RED (note, pair): the survivor count in `validate_part` restored to the
// add-only set of AddEdge targets (the earlier shape) — both committed
// (`expected a refusal, got Committed`). RED (re-create): the degenerate
// "any RemoveEdge of the kind in the part zeroes the count" — the
// re-CREATE case refused `found: 0`.
fn two_buckets(tag: &str) -> (PathBuf, Cauldron, NodeId, NodeId) {
    let dir = root(tag);
    let c = fresh(&dir);
    let b = mint_bucket(&c);
    let b2 = mint_bucket(&c);
    assert_ne!(b, b2, "two mints, two buckets — not one deduplicated commit");
    (dir, c, b, b2)
}

#[test]
fn a_note_whose_required_edge_is_removed_in_the_minting_statement_refuses_found_zero() {
    let (_dir, c, b, _b2) = two_buckets("required-edge-removed-note");
    let w = refused(c.submit(commit(
        r#"MATCH (b: fixture-capped/bucket { id: $b })
           CREATE (n: fixture-capped/note { handle: "served then not", state: "open" })
           CREATE (n) -[serves]-> (b)
           REMOVE (n) -[serves]-> (b)"#,
        &[("b", json(&b.0))],
    )));
    assert!(
        matches!(&w, Witness::RequiredEdgeMissing { node_type, edge_kind, minimum: 1, found: 0, .. }
            if node_type == "note" && edge_kind == "serves"),
        "{w:?}"
    );
    assert_eq!(count(&c, "note"), 0, "the refusal landed nothing");
}

#[test]
fn a_pair_with_one_of_two_distinct_required_edges_removed_refuses_found_one() {
    let (_dir, c, b, b2) = two_buckets("required-edge-removed-pair");
    let w = refused(c.submit(commit(
        r#"MATCH (b: fixture-capped/bucket { id: $b })
           MATCH (c: fixture-capped/bucket { id: $c })
           CREATE (p: fixture-capped/pair { handle: "two then one" })
           CREATE (p) -[serves]-> (b)
           CREATE (p) -[serves]-> (c)
           REMOVE (p) -[serves]-> (c)"#,
        &[("b", json(&b.0)), ("c", json(&b2.0))],
    )));
    assert!(
        matches!(&w, Witness::RequiredEdgeMissing { node_type, edge_kind, minimum: 2, found: 1, .. }
            if node_type == "pair" && edge_kind == "serves"),
        "{w:?}"
    );
    assert_eq!(count(&c, "pair"), 0, "the refusal landed nothing");
}

#[test]
fn a_required_edge_created_removed_and_created_again_survives_and_lands() {
    let (_dir, c, b, _b2) = two_buckets("required-edge-recreated");
    let out = c
        .submit(commit(
            r#"MATCH (b: fixture-capped/bucket { id: $b })
               CREATE (n: fixture-capped/note { handle: "served, unserved, served", state: "open" })
               CREATE (n) -[serves]-> (b)
               REMOVE (n) -[serves]-> (b)
               CREATE (n) -[serves]-> (b)
               RETURN n.id"#,
            &[("b", json(&b.0))],
        ))
        .expect("a note whose serves survives the statement lands");
    let n = returned_id(out, "n.id");
    assert_eq!(count(&c, "note"), 1);
    let rows = c
        .read(&ReadRequest {
            statement: r#"MATCH (n: fixture-capped/note { id: $id }) -[serves]-> (b: fixture-capped/bucket) RETURN b.id"#
                .into(),
            bindings: bindings(&[("id", json(&n.0))]),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect("read")
        .rows;
    assert_eq!(rows.0.len(), 1, "the surviving serves edge is stored");
    assert_eq!(rows.0[0]["b.id"], json(&b.0));
}

// ---------------------------------------------------------------------------
// same-type-as-source
// ---------------------------------------------------------------------------

/// `supersedes` arrives only at a node of the SOURCE's own type: note ->
/// note lands, bucket -> bucket lands (so the rule is "same as source", not
/// "is a note"), note -> bucket refuses `wrong_endpoint_type`.
// RED: (a) `permits`' SameAsSource arm returned `local` (any local type) —
// note -> bucket committed; (b) the arm compared against the literal
// `"note"` — bucket -> bucket refused.
#[test]
fn supersedes_arrives_only_at_the_sources_own_type() {
    let dir = root("same-type");
    let c = fresh(&dir);
    let b1 = mint_bucket(&c);
    let b2 = mint_bucket(&c);
    assert_ne!(b1, b2, "two mints, two buckets");
    let n1 = create_note(&c, &b1, "first").expect("mint");
    let n2 = create_note(&c, &b1, "second").expect("mint");

    let supersede = |src: &NodeId, src_ty: &str, dst: &NodeId, dst_ty: &str| {
        c.submit(commit(
            &format!(
                "MATCH (s: fixture-capped/{src_ty} {{ id: $s }}) MATCH (d: fixture-capped/{dst_ty} {{ id: $d }}) CREATE (s) -[supersedes]-> (d)"
            ),
            &[("s", json(&src.0)), ("d", json(&dst.0))],
        ))
    };
    supersede(&n2, "note", &n1, "note").expect("note -> note is the source's own type");
    supersede(&b2, "bucket", &b1, "bucket").expect("bucket -> bucket is the source's own type");
    let w = refused(supersede(&n1, "note", &b1, "bucket"));
    assert_eq!(grammar_fault_tag(&w), Some("wrong_endpoint_type"), "{w:?}");
    let w = refused(supersede(&b1, "bucket", &n1, "note"));
    assert_eq!(grammar_fault_tag(&w), Some("wrong_endpoint_type"), "{w:?}");
}

// ---------------------------------------------------------------------------
// the id prefix
// ---------------------------------------------------------------------------

/// A CREATE whose id lacks its type's prefix refuses `id_prefix_mismatch`.
/// Through the door, a `stamped` mint takes the spine's SERIAL
/// id `ST-1` (`tests/serial_ids_mint.rs` owns the serial guards), so the
/// door proves the check RUNS on the one id a caller can still put in front
/// of it — a caller-supplied `id` without the prefix — and the validator
/// (the same function the door calls) proves what it accepts: `ST-1`
/// lands, `n1`, `XX-1` and `ST-` do not, and a type with NO prefix judges
/// nothing.
// RED (validator half): (a) the prefix arm in `validate_delta`
// removed — the validator accepted `n1`; (b) the check reduced to
// `starts_with(prefix)` — `ST-` (empty rest) and `STX-1` passed.
// RED (door half): `compile::caller_id_against_prefix`'s call
// removed — the door refused the caller's `n1` store-minted, not
// `id_prefix_mismatch`. (An earlier door half asserted every
// `stamped` mint refused, when ids were minted by hash.)
#[test]
fn a_create_whose_id_lacks_its_types_prefix_refuses() {
    let dir = root("prefix");
    let c = fresh(&dir);
    let before = count(&c, "stamped");
    let w = refused(c.submit(commit(
        r#"CREATE (s: fixture-capped/stamped { id: "n1", name: "x" }) RETURN s.id"#,
        &[],
    )));
    assert_eq!(grammar_fault_tag(&w), Some("id_prefix_mismatch"), "{w:?}");
    assert_eq!(count(&c, "stamped"), before, "the refusal landed nothing");
    let minted = c
        .submit(commit(
            r#"CREATE (s: fixture-capped/stamped { name: "x" }) RETURN s.id"#,
            &[],
        ))
        .map(|o| returned_id(o, "s.id"))
        .expect("a stamped mint without a caller id lands");
    assert_eq!(minted, NodeId("ST-1".into()), "the spine minted the serial");

    let g = capped_grammar();
    let part_with = |id: &str, node_type: &str| CompiledPart {
        pool: PoolName::new(POOL),
        schema_version: g.version,
        deltas: vec![Delta::CreateNode {
            id: NodeId(id.to_string()),
            node_type: node_type.to_string(),
            state: None,
            fields: BTreeMap::from([("name".to_string(), json("x"))]),
        }],
        premises: vec![],
        far_versions: BTreeMap::new(),
    };
    let nobody = |_: &NodeId| -> Option<String> { None };
    let judge = |id: &str, node_type: &str| -> Option<&'static str> {
        match g.validate_part(&part_with(id, node_type), Some(0), &nobody, &FarSide::Landing) {
            Ok(()) => None,
            Err(e) => Some(grammar_fault_tag(&only_witness(&e)).expect("a tagged fault")),
        }
    };
    assert_eq!(judge("ST-1", "stamped"), None, "the prefix and a non-empty rest: accepted");
    assert_eq!(judge("ST-0007", "stamped"), None);
    assert_eq!(judge("n1", "stamped"), Some("id_prefix_mismatch"));
    assert_eq!(judge("XX-1", "stamped"), Some("id_prefix_mismatch"));
    assert_eq!(judge("STX-1", "stamped"), Some("id_prefix_mismatch"), "the prefix is whole, not a head");
    assert_eq!(judge("ST-", "stamped"), Some("id_prefix_mismatch"), "the rest is non-empty");
    assert_eq!(judge("ST1", "stamped"), Some("id_prefix_mismatch"), "the separator is `-`");
    // A type with no prefix judges nothing about the id (the fixtures' hash
    // ids keep landing). `bucket` carries no `name`, so use its own field.
    let bucket_part = CompiledPart {
        pool: PoolName::new(POOL),
        schema_version: g.version,
        deltas: vec![Delta::CreateNode {
            id: NodeId("n0123456789abcdef".to_string()),
            node_type: "bucket".to_string(),
            state: None,
            fields: BTreeMap::from([("boundary".to_string(), json(&"y".repeat(40)))]),
        }],
        premises: vec![],
        far_versions: BTreeMap::new(),
    };
    g.validate_part(&bucket_part, Some(0), &nobody, &FarSide::Landing)
        .expect("an unprefixed type accepts a hash id");
}

// ---------------------------------------------------------------------------
// the grammar's own coherence
// ---------------------------------------------------------------------------

/// `check_wellformed` refuses each malformed use of the additions — and
/// accepts the fixture, so the refusals are not a validator that refuses
/// everything.
// RED: each arm removed in turn made its poisoned grammar pin; the fixture
// is the positive sentinel over the same function.
#[test]
fn check_wellformed_refuses_each_malformed_addition_and_accepts_the_fixture() {
    capped_grammar().check_wellformed().expect("the fixture is well-formed");

    let poisoned = |f: &dyn Fn(&mut PoolGrammar)| {
        let mut g = capped_grammar();
        f(&mut g);
        let err = g.check_wellformed().expect_err("a poisoned grammar refuses");
        let w = only_witness(&err);
        assert_eq!(grammar_fault_tag(&w), Some("malformed_grammar"), "{w:?}");
        w
    };
    // a cap on a non-text kind
    poisoned(&|g| {
        g.node_types.get_mut("note").unwrap().fields.get_mut("rank").unwrap().max_chars = Some(3);
    });
    // a floor above its cap
    poisoned(&|g| {
        let f = g.node_types.get_mut("bucket").unwrap().fields.get_mut("boundary").unwrap();
        f.min_chars = Some(2_000);
    });
    // two types with one prefix
    poisoned(&|g| g.node_types.get_mut("note").unwrap().id_prefix = Some("ST".into()));
    // a prefix carrying the separator, and an empty one
    poisoned(&|g| g.node_types.get_mut("stamped").unwrap().id_prefix = Some("S-T".into()));
    poisoned(&|g| g.node_types.get_mut("stamped").unwrap().id_prefix = Some(String::new()));
    // a required edge of an unknown kind
    poisoned(&|g| {
        g.node_types.get_mut("note").unwrap().required_edges_at_mint.insert("blesses".into(), 1);
    });
    // a required edge of a kind that does not leave this type
    poisoned(&|g| {
        g.node_types.get_mut("bucket").unwrap().required_edges_at_mint.insert("serves".into(), 1);
    });
    // a minimum of zero
    poisoned(&|g| {
        g.node_types.get_mut("note").unwrap().required_edges_at_mint.insert("serves".into(), 0);
    });
    // same-as-source as a SOURCE, and no source at all — on `supersedes`,
    // which no type requires at mint, so the refusal can only be the
    // `from`-set arm's (poisoning `serves` was refused by the required-edge
    // arm first and proved nothing about this one: forcings F24/F25).
    poisoned(&|g| {
        g.edge_kinds.get_mut("supersedes").unwrap().from = BTreeSet::from([TypeRef::SameAsSource]);
    });
    poisoned(&|g| g.edge_kinds.get_mut("supersedes").unwrap().from = BTreeSet::new());
    // a source type the grammar does not declare
    poisoned(&|g| {
        g.edge_kinds.get_mut("serves").unwrap().from.insert(TypeRef::local("ghost"));
    });
    // A multi-source kind IS well-formed (the positive case for the set).
    let mut g = capped_grammar();
    g.edge_kinds.get_mut("serves").unwrap().from.insert(TypeRef::local("bucket"));
    g.check_wellformed().expect("a kind leaving several declared types is well-formed");
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-grammar-v6-{name}-{}", std::process::id()))
}

fn fresh(dir: &Path) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(dir, &[capped_grammar()], SpineConfig::default()).expect("create the cauldron")
}

fn json(v: impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(v).expect("json")
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
                thread: "grammar-v6".into(),
                dispatch: "grammar-v6".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

/// A bucket with a boundary of exactly the floor (40 characters) — a
/// DIFFERENT boundary every call, because the door is exactly-once over the
/// envelope's hash identity (statement + bindings, no nonce here) and two
/// identical mints hand back the ORIGINAL node, not a second one (with a
/// fixed boundary this suite's "two distinct buckets" were one bucket).
fn mint_bucket(c: &Cauldron) -> NodeId {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SERIAL: AtomicU32 = AtomicU32::new(0);
    let n = SERIAL.fetch_add(1, Ordering::SeqCst);
    let boundary = format!("Inside: bucket number {n:05} of this run.");
    assert_eq!(boundary.chars().count(), BOUNDARY_FLOOR as usize, "the sentinel: exactly the floor");
    returned_id(
        c.submit(commit(
            r#"CREATE (b: fixture-capped/bucket { boundary: $t }) RETURN b.id"#,
            &[("t", json(&boundary))],
        ))
        .expect("mint a bucket"),
        "b.id",
    )
}

/// A note with `handle`, minted WITH the one `serves` its type requires.
fn create_note(c: &Cauldron, bucket: &NodeId, handle: &str) -> spine::error::Result<NodeId> {
    c.submit(commit(
        r#"MATCH (b: fixture-capped/bucket { id: $b })
           CREATE (n: fixture-capped/note { handle: $h, state: "open" })
           CREATE (n) -[serves]-> (b)
           RETURN n.id"#,
        &[("b", json(&bucket.0)), ("h", json(handle))],
    ))
    .map(|o| returned_id(o, "n.id"))
}

fn returned_id(outcome: Outcome, column: &str) -> NodeId {
    match outcome {
        Outcome::Committed { returned, .. } => {
            NodeId(returned.0[0][column].as_str().expect("id").to_string())
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn refused<T: std::fmt::Debug>(r: spine::error::Result<T>) -> Witness {
    match r {
        Err(e) => only_witness(&e),
        Ok(v) => panic!("expected a refusal, got {v:?}"),
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

/// How many nodes of `node_type` the pool holds — a population read through
/// the read door, so a refusal that landed a node moves it.
fn count(c: &Cauldron, node_type: &str) -> usize {
    c.read(&ReadRequest {
        statement: format!("MATCH (n: fixture-capped/{node_type}) RETURN n.id"),
        bindings: Bindings::new(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    })
    .expect("read")
    .rows
    .0
    .len()
}
