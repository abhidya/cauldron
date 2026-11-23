//! The storage layer, proven: the pinned-grammar validator, the pool file,
//! the landing transaction, and the projected namespace.
//!
//! Material is `spine::schema_def::fixtures` — the alpha/beta fixtures, shipped
//! as constructed data so every worker exercises ONE alpha and ONE beta.
//!
//! The proving rules govern this file: every checker below has
//! been forced RED at least once (each one names, in a comment, the poison
//! that made it fire). Contention is proven at N >= 2 — the count-cell test runs
//! two real concurrent landings on the same file, because the empty and single
//! cases cannot exercise what commutativity exists to manage.

use spine::delta::{CompiledPart, Delta};
use spine::envelope::TxHash;
use spine::error::SpineError;
use spine::pool::{NodeId, Pool, QualifiedNodeId};
use spine::project;
use spine::schema_def::fixtures::{fixture_alpha, fixture_beta};
use spine::schema_def::{
    grammar_fault_tag, CountCellDef, Direction, EdgeKindDef, FarSide, FieldDef, NodeTypeDef,
    PoolGrammar, PoolKind, PoolName, TypeRef,
};
use spine::witness::Witness;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

test_scratch::install!();

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A throwaway cauldron directory. Nothing is written beside the database
/// files, so removing the directory is the whole cleanup.
struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str) -> Temp {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = test_scratch::tmp_path(format!(
            "spine-storage-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        Temp(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tx(byte: u8) -> TxHash {
    TxHash([byte; 32])
}

fn nid(s: &str) -> NodeId {
    NodeId(s.to_string())
}

fn qid(pool: &str, id: &str) -> QualifiedNodeId {
    QualifiedNodeId {
        pool: PoolName::new(pool),
        id: nid(id),
    }
}

fn part(pool: &str, version: u32, deltas: Vec<Delta>) -> CompiledPart {
    CompiledPart {
        pool: PoolName::new(pool),
        schema_version: version,
        deltas,
        premises: Vec::new(),
        far_versions: BTreeMap::new(),
    }
}

/// A part carrying the DOOR's record of the far pins its cross-pool edges
/// validated against (`CompiledPart::far_versions`) — what a
/// landing asks for before it lands an edge into another pool.
fn part_reaching(pool: &str, version: u32, deltas: Vec<Delta>, far: &[(&str, u32)]) -> CompiledPart {
    let mut p = part(pool, version, deltas);
    for (name, v) in far {
        p.far_versions.insert(PoolName::new(*name), *v);
    }
    p
}

fn only(t: TypeRef) -> BTreeSet<TypeRef> {
    BTreeSet::from([t])
}

fn create(id: &str, node_type: &str, state: Option<&str>, fields: &[(&str, &str)]) -> Delta {
    Delta::CreateNode {
        id: nid(id),
        node_type: node_type.to_string(),
        state: state.map(str::to_string),
        fields: fields
            .iter()
            .map(|(k, v)| ((*k).to_string(), serde_json::json!(v)))
            .collect(),
    }
}

/// No node exists yet — the resolver a compile-time validation of a
/// create-only part sees.
fn empty_pool(_: &NodeId) -> Option<String> {
    None
}

fn witnesses(e: &SpineError) -> Vec<Witness> {
    match e {
        SpineError::Refused { witnesses } => witnesses.clone(),
        other => panic!("expected a structured refusal, got: {other:?}"),
    }
}

/// Assert on the STRUCTURE of a grammar refusal — the pool it names and the
/// frozen fault tag — never on the prose.
#[track_caller]
fn expect_fault(e: &SpineError, pool: &str, tag: &str) {
    let ws = witnesses(e);
    assert_eq!(ws.len(), 1, "refusal is whole and names one fault: {ws:?}");
    match &ws[0] {
        Witness::GrammarViolation { pool: p, .. } => {
            assert_eq!(p.as_str(), pool, "the witness must name the FAILING pool");
        }
        other => panic!("expected GrammarViolation, got: {other:?}"),
    }
    assert_eq!(grammar_fault_tag(&ws[0]), Some(tag), "fault tag");
}

/// An alpha pool ready to land parts into, plus its temp home.
fn alpha_pool(tag: &str) -> (Temp, Pool) {
    let temp = Temp::new(tag);
    let pool = Pool::create(temp.path(), &fixture_alpha()).expect("create fixture-alpha");
    (temp, pool)
}

// ---------------------------------------------------------------------------
// 1. the alpha/beta fixtures, verified against the design clause by clause
// ---------------------------------------------------------------------------

/// RED once by: changing `current_of`'s constraint to `at_most_one_per_from`
/// (the design says at most one per AUTHOR, which is the `to` endpoint).
#[test]
fn fixture_alpha_is_exactly_order_5a() {
    let g = fixture_alpha();
    assert_eq!(g.pool.as_str(), "fixture-alpha");
    assert_eq!(g.version, 1);

    // Node types: `doc` (fields: title; state in draft, published, superseded)
    // and `author` (fields: name; one machine-maintained count cell of docs
    // written).
    assert_eq!(
        g.node_types.keys().collect::<Vec<_>>(),
        vec!["author", "doc"]
    );
    let doc = g.node_type_def("doc").expect("doc");
    assert_eq!(doc.fields.keys().collect::<Vec<_>>(), vec!["title"]);
    assert_eq!(doc.states, vec!["draft", "published", "superseded"]);
    assert!(doc.count_cells.is_empty());

    let author = g.node_type_def("author").expect("author");
    assert_eq!(author.fields.keys().collect::<Vec<_>>(), vec!["name"]);
    assert!(author.states.is_empty());
    assert_eq!(author.count_cells.len(), 1);
    let cell = author.count_cells.get("docs_written").expect("docs_written");
    assert_eq!(cell.edge_kind, "wrote");
    assert_eq!(cell.direction, Direction::Out);

    // Edge kinds: wrote (author to doc), supersedes (doc to doc), current_of
    // (doc to author) constrained to AT MOST ONE PER AUTHOR.
    assert_eq!(
        g.edge_kinds.keys().collect::<Vec<_>>(),
        vec!["current_of", "supersedes", "wrote"]
    );
    let wrote = g.edge_kind_def("wrote").expect("wrote");
    assert_eq!(wrote.from, only(TypeRef::local("author")));
    assert_eq!(wrote.to, only(TypeRef::local("doc")));
    let supersedes = g.edge_kind_def("supersedes").expect("supersedes");
    assert_eq!(supersedes.from, only(TypeRef::local("doc")));
    assert_eq!(supersedes.to, only(TypeRef::local("doc")));
    let current = g.edge_kind_def("current_of").expect("current_of");
    assert_eq!(current.from, only(TypeRef::local("doc")));
    assert_eq!(current.to, only(TypeRef::local("author")));
    // Task 09: every grammar declares its KIND.
    assert_eq!(g.kind, PoolKind::new("alpha"));
    assert!(
        !current.at_most_one_per_from && current.at_most_one_per_to,
        "at most one per AUTHOR is the `to` endpoint — the unique slice locking bites on"
    );
    assert!(g.is_scalar_slice("current_of", Direction::In));
    assert!(!g.is_scalar_slice("current_of", Direction::Out));
}

/// RED once by: pinning beta to version 1 (the design requires a DIFFERENT
/// number than alpha, so version-blindness is exercised by every cross-pool
/// test).
#[test]
fn fixture_beta_is_exactly_order_5a() {
    let g = fixture_beta();
    assert_eq!(g.pool.as_str(), "fixture-beta");
    assert_ne!(
        g.version,
        fixture_alpha().version,
        "beta is pinned to a DIFFERENT version than alpha"
    );

    assert_eq!(
        g.node_types.keys().collect::<Vec<_>>(),
        vec!["shelf", "ticket"]
    );
    let shelf = g.node_type_def("shelf").expect("shelf");
    assert_eq!(shelf.fields.keys().collect::<Vec<_>>(), vec!["capacity", "label"]);
    assert!(shelf.states.is_empty());
    let ticket = g.node_type_def("ticket").expect("ticket");
    assert_eq!(ticket.states, vec!["open", "closed"]);

    assert_eq!(
        g.edge_kinds.keys().collect::<Vec<_>>(),
        vec!["catalogues", "holds"]
    );
    let holds = g.edge_kind_def("holds").expect("holds");
    assert_eq!(holds.from, only(TypeRef::local("shelf")));
    assert_eq!(holds.to, only(TypeRef::local("ticket")));
    // THE cross-pool edge kind: its far endpoint is a `doc` in any pool of
    // kind `alpha` — a REACH by kind, never by pool name.
    let cat = g.edge_kind_def("catalogues").expect("catalogues");
    assert_eq!(cat.from, only(TypeRef::local("ticket")));
    assert_eq!(cat.to, only(TypeRef::reach("alpha", "doc")));
    assert!(cat.reaches_across());
    assert_eq!(g.kind, PoolKind::new("beta"));
}

/// The fixtures are "deliberately sharing no node type and no edge kind"
/// — which is what makes a merged vocabulary detectable.
///
/// RED once by: renaming beta's `holds` to `wrote`.
#[test]
fn the_two_fixtures_share_no_vocabulary() {
    let (a, b) = (fixture_alpha(), fixture_beta());
    for t in a.node_types.keys() {
        assert!(!b.node_types.contains_key(t), "shared node type `{t}`");
    }
    for k in a.edge_kinds.keys() {
        assert!(!b.edge_kinds.contains_key(k), "shared edge kind `{k}`");
    }
}

/// RED once by: adding a count cell to `doc` counting the non-existent edge
/// kind `cites` (the well-formedness pass then refuses `malformed_grammar`).
#[test]
fn both_fixtures_are_well_formed() {
    fixture_alpha().check_wellformed().expect("alpha well-formed");
    fixture_beta().check_wellformed().expect("beta well-formed");
}

// ---------------------------------------------------------------------------
// 2. grammar well-formedness — poisoned grammars, refused before any pin
// ---------------------------------------------------------------------------

fn poisoned_grammar(mutate: impl FnOnce(&mut PoolGrammar)) -> PoolGrammar {
    let mut g = fixture_alpha();
    mutate(&mut g);
    g
}

/// Each arm is a poisoned grammar that must refuse; together they are this
/// checker's red fixtures (a checker with no red fixture does not
/// count as existing).
#[test]
fn malformed_grammars_refuse_rather_than_pin() {
    // an edge kind leaving a node type the grammar never declares
    let g = poisoned_grammar(|g| {
        g.edge_kinds.insert("cites".into(), EdgeKindDef::local("citation", "doc"));
    });
    expect_fault(
        &g.check_wellformed().unwrap_err(),
        "fixture-alpha",
        "malformed_grammar",
    );

    // ---- the kind, and the reach set ----
    // a reach as the SOURCE of an edge (the source is always local)
    let g = poisoned_grammar(|g| {
        g.edge_kinds.get_mut("wrote").unwrap().from = only(TypeRef::reach("alpha", "author"));
    });
    expect_fault(&g.check_wellformed().unwrap_err(), "fixture-alpha", "malformed_grammar");
    // an edge kind permitting NO far endpoint at all
    let g = poisoned_grammar(|g| g.edge_kinds.get_mut("wrote").unwrap().to.clear());
    expect_fault(&g.check_wellformed().unwrap_err(), "fixture-alpha", "malformed_grammar");
    // a reach toward the projected kind naming anything but its one type
    let g = poisoned_grammar(|g| {
        g.edge_kinds.get_mut("supersedes").unwrap().to = only(TypeRef::reach("spine", "doc"));
    });
    expect_fault(&g.check_wellformed().unwrap_err(), "fixture-alpha", "malformed_grammar");
    // the projected namespace's KIND, claimed by a real pool
    let g = poisoned_grammar(|g| g.kind = PoolKind::new("spine"));
    expect_fault(&g.check_wellformed().unwrap_err(), "fixture-alpha", "malformed_grammar");
    // a malformed kind name
    let g = poisoned_grammar(|g| g.kind = PoolKind::new("Alpha"));
    expect_fault(&g.check_wellformed().unwrap_err(), "fixture-alpha", "malformed_grammar");
    // ...while a reach toward the projected kind's one type is well-formed.
    let g = poisoned_grammar(|g| {
        g.edge_kinds.get_mut("supersedes").unwrap().to = only(TypeRef::reach("spine", "txn"));
    });
    g.check_wellformed().expect("a provenance reach is legal");

    // a count cell over an edge kind that does not exist
    let g = poisoned_grammar(|g| {
        g.node_types.get_mut("doc").unwrap().count_cells.insert(
            "cited_by".into(),
            CountCellDef {
                edge_kind: "cites".into(),
                direction: Direction::In,
            },
        );
    });
    expect_fault(
        &g.check_wellformed().unwrap_err(),
        "fixture-alpha",
        "malformed_grammar",
    );

    // a count cell whose direction points at the WRONG endpoint type
    let g = poisoned_grammar(|g| {
        g.node_types.get_mut("doc").unwrap().count_cells.insert(
            "wrote_out".into(),
            CountCellDef {
                edge_kind: "wrote".into(),
                direction: Direction::Out,
            },
        );
    });
    expect_fault(
        &g.check_wellformed().unwrap_err(),
        "fixture-alpha",
        "malformed_grammar",
    );

    // an at-most-one constraint on a FOREIGN `to` — that slice lives in the
    // other pool and this pool cannot hold it still
    let mut g = fixture_beta();
    g.edge_kinds.get_mut("catalogues").unwrap().at_most_one_per_to = true;
    expect_fault(
        &g.check_wellformed().unwrap_err(),
        "fixture-beta",
        "malformed_grammar",
    );

    // a grammar declaring the machine-owned stamp as a field
    let g = poisoned_grammar(|g| {
        g.node_types
            .get_mut("doc")
            .unwrap()
            .fields
            .insert("schema_version".into(), FieldDef::optional());
    });
    expect_fault(
        &g.check_wellformed().unwrap_err(),
        "fixture-alpha",
        "malformed_grammar",
    );

    // the reserved projected namespace, as a pool name
    let g = poisoned_grammar(|g| g.pool = PoolName::new("spine"));
    expect_fault(
        &g.check_wellformed().unwrap_err(),
        "spine",
        "malformed_grammar",
    );
}

// ---------------------------------------------------------------------------
// 3. per-pool, version-pinned delta validation — the poisoned variants
// ---------------------------------------------------------------------------

/// The second poisoned variant: one that
/// addresses an alpha node type into the beta pool and must refuse under
/// beta's grammar — which is what proves validation is genuinely per-pool
/// rather than one merged vocabulary.
///
/// RED once by: making `validate_part` skip its `node_types` lookup, at which
/// point beta happily accepts an alpha `doc` and the merged-vocabulary bug is
/// undetectable.
#[test]
fn an_alpha_node_type_addressed_into_beta_refuses_under_betas_grammar() {
    let alpha = fixture_alpha();
    let beta = fixture_beta();

    let doc_in_beta = part(
        "fixture-beta",
        beta.version,
        vec![create("nD", "doc", Some("draft"), &[("title", "t")])],
    );
    let err = beta
        .validate_part(&doc_in_beta, Some(1), &empty_pool, &FarSide::Landing)
        .unwrap_err();
    expect_fault(&err, "fixture-beta", "unknown_node_type");

    // The SAME delta under alpha's own grammar is legal — the two grammars
    // are genuinely separate vocabularies, not one merged namespace.
    let doc_in_alpha = part(
        "fixture-alpha",
        alpha.version,
        vec![create("nD", "doc", Some("draft"), &[("title", "t")])],
    );
    alpha
        .validate_part(&doc_in_alpha, Some(0), &empty_pool, &FarSide::Landing)
        .expect("alpha declares `doc`");
}

/// The first poisoned variant at the validation level: a beta part that
/// violates beta's own grammar. The refusal names the BETA part.
///
/// RED once by: dropping the `undeclared_field` arm from `check_field_name`.
#[test]
fn a_beta_part_violating_betas_grammar_names_the_beta_part() {
    let beta = fixture_beta();
    let bad = part(
        "fixture-beta",
        beta.version,
        vec![create("nT", "ticket", Some("open"), &[("title", "nope")])],
    );
    let err = beta
        .validate_part(&bad, Some(1), &empty_pool, &FarSide::Landing)
        .unwrap_err();
    expect_fault(&err, "fixture-beta", "undeclared_field");
    match &witnesses(&err)[0] {
        Witness::GrammarViolation { part, .. } => {
            assert_eq!(*part, Some(1), "the witness names WHICH part failed")
        }
        other => panic!("{other:?}"),
    }
}

/// The version pin is part of the validation, not decoration: a part stamped
/// with a neighbour pool's version cannot claim to have validated here
/// (the per-part validated-under stamp; version-blindness).
///
/// RED once by: deleting the `part.schema_version != self.version` arm — at
/// which point a beta-versioned part validates under alpha and the row's
/// forever-record of what it validated against becomes a lie.
#[test]
fn a_part_stamped_with_the_wrong_version_refuses() {
    let alpha = fixture_alpha();
    let mispinned = part(
        "fixture-alpha",
        fixture_beta().version,
        vec![create("nD", "doc", Some("draft"), &[("title", "t")])],
    );
    expect_fault(
        &alpha
            .validate_part(&mispinned, Some(0), &empty_pool, &FarSide::Landing)
            .unwrap_err(),
        "fixture-alpha",
        "version_mismatch",
    );

    let misaddressed = part(
        "fixture-beta",
        alpha.version,
        vec![create("nD", "doc", Some("draft"), &[("title", "t")])],
    );
    expect_fault(
        &alpha
            .validate_part(&misaddressed, Some(0), &empty_pool, &FarSide::Landing)
            .unwrap_err(),
        "fixture-alpha",
        "wrong_pool",
    );
}

/// The remaining refusal surface, each arm its own red fixture.
#[test]
fn delta_validation_refuses_every_named_grammar_fault() {
    let alpha = fixture_alpha();
    let stored = |id: &NodeId| -> Option<String> {
        match id.0.as_str() {
            "nAuthor" => Some("author".to_string()),
            "nDoc" => Some("doc".to_string()),
            _ => None,
        }
    };
    let v = |deltas: Vec<Delta>| -> SpineError {
        alpha
            .validate_part(
                &part("fixture-alpha", 1, deltas),
                Some(0),
                &stored,
                &FarSide::Landing,
            )
            .unwrap_err()
    };

    // a state outside the closed vocabulary
    expect_fault(
        &v(vec![create("nX", "doc", Some("archived"), &[("title", "t")])]),
        "fixture-alpha",
        "unknown_state",
    );
    // a state on a type that declares none
    expect_fault(
        &v(vec![create("nX", "author", Some("open"), &[("name", "a")])]),
        "fixture-alpha",
        "state_not_declared",
    );
    // a required field left out
    expect_fault(
        &v(vec![create("nX", "doc", Some("draft"), &[])]),
        "fixture-alpha",
        "missing_required_field",
    );
    // minting an identifier the pool already holds
    expect_fault(
        &v(vec![create("nDoc", "doc", Some("draft"), &[("title", "t")])]),
        "fixture-alpha",
        "duplicate_node_id",
    );
    // the machine-owned schema-version stamp, addressed by a pen
    expect_fault(
        &v(vec![Delta::SetField {
            node: nid("nDoc"),
            path: vec!["schema_version".into()],
            value: serde_json::json!(99),
        }]),
        "fixture-alpha",
        "machine_owned_field",
    );
    // a count cell, spoken as if it were a field (a count cell is never spoken)
    expect_fault(
        &v(vec![Delta::SetField {
            node: nid("nAuthor"),
            path: vec!["docs_written".into()],
            value: serde_json::json!(7),
        }]),
        "fixture-alpha",
        "machine_owned_cell",
    );
    // an edge kind this pool does not declare
    expect_fault(
        &v(vec![Delta::AddEdge {
            src: nid("nAuthor"),
            kind: "holds".into(),
            dst: qid("fixture-alpha", "nDoc"),
            note: None,
            fields: Default::default(),
        }]),
        "fixture-alpha",
        "unknown_edge_kind",
    );
    // endpoints of the wrong type
    expect_fault(
        &v(vec![Delta::AddEdge {
            src: nid("nDoc"),
            kind: "wrote".into(),
            dst: qid("fixture-alpha", "nDoc"),
            note: None,
            fields: Default::default(),
        }]),
        "fixture-alpha",
        "wrong_endpoint_type",
    );
    // a LOCAL edge kind pointed at another pool: LOCAL means this pool only,
    // whatever the far pool is (`reach_undeclared`)
    expect_fault(
        &v(vec![Delta::AddEdge {
            src: nid("nAuthor"),
            kind: "wrote".into(),
            dst: qid("fixture-beta", "nDoc"),
            note: None,
            fields: Default::default(),
        }]),
        "fixture-alpha",
        "reach_undeclared",
    );
    // a node this pool does not hold
    expect_fault(
        &v(vec![Delta::SetField {
            node: nid("nGhost"),
            path: vec!["title".into()],
            value: serde_json::json!("x"),
        }]),
        "fixture-alpha",
        "unknown_node",
    );
}

/// The two EDGE cases above, through THE DOOR (`Cauldron::submit`) rather
/// than the validator. Through the door a CREATE-edge statement is
/// refused by the compiler (`ladle::compile`'s claim and edge sites) before
/// the validator sees it, so the validator test above cannot speak for the
/// door: these once came back as untagged prose. Each refusal is
/// opposed by the legal edge of the same kind landing, so a
/// door that refused every edge with the right tag would fail here.
// RED: the `unknown_edge_kind` tag reverted to the untagged
// `grammar_violation` in `edge_deltas` — this test red,
// `delta_validation_refuses_every_named_grammar_fault` above green; the
// `wrong_endpoint_type` tag reverted at `edge_deltas`' source check — same
// split. Opposed: `edge_deltas`' source check made to refuse every source —
// the legal `wrote` refused.
#[test]
fn the_door_refuses_an_unknown_edge_kind_and_a_wrong_source_by_tag() {
    use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
    let temp = Temp::new("door-edge-tags");
    let c = spine::Cauldron::create(
        temp.path(),
        &[fixture_alpha()],
        spine::config::SpineConfig::default(),
    )
    .expect("cauldron");
    let env = |statement: &str, params: &[(&str, &str)]| {
        let mut bindings = Bindings::new();
        for (k, v) in params {
            bindings.insert((*k).to_string(), serde_json::json!(v));
        }
        Envelope {
            statement: statement.to_string(),
            bindings,
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
            provenance: Provenance {
                initiator: Initiator::Agent {
                    thread: "storage".into(),
                    dispatch: "storage".into(),
                },
                instrument: Instrument::door(),
            },
            mode: Mode::Commit,
            idempotency_nonce: None,
        }
    };
    let minted = |out: spine::Outcome, col: &str| -> String {
        match out {
            spine::Outcome::Committed { returned, .. } => {
                returned.0[0][col].as_str().expect("id").to_string()
            }
            other => panic!("expected Committed, got {other:?}"),
        }
    };
    let author = minted(
        c.submit(env(r#"CREATE (a: fixture-alpha/author { name: "ann" }) RETURN a.id"#, &[]))
            .expect("an author lands"),
        "a.id",
    );
    let doc = minted(
        c.submit(env(
            r#"CREATE (d: fixture-alpha/doc { title: "t", state: "draft" }) RETURN d.id"#,
            &[],
        ))
        .expect("a doc lands"),
        "d.id",
    );
    let edge = |src_ty: &str, kind: &str, dst_ty: &str, src: &str, dst: &str| {
        c.submit(env(
            &format!(
                "MATCH (s: fixture-alpha/{src_ty} {{ id: $s }}) MATCH (d: fixture-alpha/{dst_ty} {{ id: $d }}) CREATE (s) -[{kind}]-> (d)"
            ),
            &[("s", src), ("d", dst)],
        ))
    };

    // an edge kind this pool does not declare — the validator case's `holds`
    let e = edge("author", "holds", "doc", &author, &doc).expect_err("`holds` is undeclared");
    expect_fault(&e, "fixture-alpha", "unknown_edge_kind");
    // a source of the wrong type — the validator case's doc -[wrote]-> doc
    let e = edge("doc", "wrote", "doc", &doc, &doc).expect_err("`wrote` leaves an author");
    expect_fault(&e, "fixture-alpha", "wrong_endpoint_type");
    // opposed: the same kind between the right endpoints lands
    edge("author", "wrote", "doc", &author, &doc).expect("author -[wrote]-> doc is legal");
}

/// The cross-pool edge kind validates in BETA, naming an alpha node — by
/// LOOKUP (go check the schema
/// version on it). At the door the validator consults alpha's own pin
/// through `FarSide::Door` — its kind, its declared types, the far node's
/// actual type — and never merges the vocabularies (version-blindness still holds:
/// beta's grammar judges nothing by alpha's names; it asks alpha's pin).
///
/// RED once by: (a) making `validate_reach` skip the far-type lookup — the
/// `shelf`-typed far node then validated as a `doc`; (b) making it skip the
/// far-grammar lookup — the `fixture-gamma` edge validated; (c) making the
/// Landing arm return `Ok(())` unconditionally — the record-less part landed.
#[test]
fn the_cross_pool_edge_validates_under_beta_by_looking_alpha_up() {
    let alpha = fixture_alpha();
    let beta = fixture_beta();
    let stored = |id: &NodeId| -> Option<String> {
        (id.0 == "nTicket").then(|| "ticket".to_string())
    };
    let far_grammar = |p: &PoolName| -> Option<PoolGrammar> {
        (p.as_str() == "fixture-alpha").then(|| alpha.clone())
    };
    // What alpha holds, as alpha resolves it: a doc and a stray shelf-typed
    // node (impossible under alpha's grammar, but the validator must judge
    // the ACTUAL type it is told, not the type the reach hopes for).
    let far_type = |q: &QualifiedNodeId| -> Option<String> {
        match (q.pool.as_str(), q.id.0.as_str()) {
            ("fixture-alpha", "nDoc") => Some("doc".to_string()),
            ("fixture-alpha", "nShelf") => Some("shelf".to_string()),
            _ => None,
        }
    };
    // The kind holds alpha alone here, so "declared by the kind" is "declared
    // by alpha" — the same window the far-grammar lookup already opens.
    let declared_by_kind = |k: &spine::schema_def::PoolKind, t: &str| -> bool {
        *k == alpha.kind && alpha.node_types.contains_key(t)
    };
    let door = FarSide::Door {
        grammar: &far_grammar,
        node_type: &far_type,
        declared_by_kind: &declared_by_kind,
    };
    let edge_to = |pool: &str, id: &str| -> Vec<Delta> {
        vec![Delta::AddEdge {
            src: nid("nTicket"),
            kind: "catalogues".into(),
            dst: qid(pool, id),
            note: None,
            fields: Default::default(),
        }]
    };

    beta.validate_part(
        &part("fixture-beta", beta.version, edge_to("fixture-alpha", "nDoc")),
        Some(1),
        &stored,
        &door,
    )
    .expect("a `doc` in a pool of kind `alpha` is exactly what the reach names");

    // The far pool is not in the cauldron: refused by name.
    expect_fault(
        &beta
            .validate_part(
                &part("fixture-beta", beta.version, edge_to("fixture-gamma", "nDoc")),
                Some(1),
                &stored,
                &door,
            )
            .unwrap_err(),
        "fixture-beta",
        "foreign_pool_unknown",
    );
    // The far node's ACTUAL type is not what the reach names.
    expect_fault(
        &beta
            .validate_part(
                &part("fixture-beta", beta.version, edge_to("fixture-alpha", "nShelf")),
                Some(1),
                &stored,
                &door,
            )
            .unwrap_err(),
        "fixture-beta",
        "reach_undeclared",
    );
    // The far node does not exist there: `unknown_node` STAMPED WITH THE FAR
    // POOL — the pool whose grammar refused.
    expect_fault(
        &beta
            .validate_part(
                &part("fixture-beta", beta.version, edge_to("fixture-alpha", "nNobody")),
                Some(1),
                &stored,
                &door,
            )
            .unwrap_err(),
        "fixture-alpha",
        "unknown_node",
    );

    // At a LANDING there is no window onto alpha's file: the part must carry
    // the door's record of the far pin it validated under, or refuse.
    expect_fault(
        &beta
            .validate_part(
                &part("fixture-beta", beta.version, edge_to("fixture-alpha", "nDoc")),
                Some(1),
                &stored,
                &FarSide::Landing,
            )
            .unwrap_err(),
        "fixture-beta",
        "far_unvalidated",
    );
    beta.validate_part(
        &part_reaching(
            "fixture-beta",
            beta.version,
            edge_to("fixture-alpha", "nDoc"),
            &[("fixture-alpha", alpha.version)],
        ),
        Some(1),
        &stored,
        &FarSide::Landing,
    )
    .expect("the door's record on the part is what the landing asks for");
}

// ---------------------------------------------------------------------------
// 4. the pool file
// ---------------------------------------------------------------------------

/// RED once by: removing the `is_reserved` arm from `check_wellformed`, at
/// which point a pool file named `spine.db` would sit beside the spine's own.
#[test]
fn create_refuses_the_reserved_name_and_never_overwrites() {
    let temp = Temp::new("create");
    let mut reserved = fixture_alpha();
    reserved.pool = PoolName::new("spine");
    let err = Pool::create(temp.path(), &reserved).map(|_| ()).unwrap_err();
    expect_fault(&err, "spine", "malformed_grammar");
    assert!(
        !temp.path().join("spine.db").exists(),
        "a refusal leaves no file behind"
    );

    Pool::create(temp.path(), &fixture_alpha()).expect("first create");
    match Pool::create(temp.path(), &fixture_alpha()).map(|_| ()) {
        Err(SpineError::Usage { .. }) => {}
        other => panic!("a pool is never overwritten; got {other:?}"),
    }
}

/// A file that is not a pool is a HARD failure, never an empty pool (soup
/// idiom). RED once by: returning `Ok` with a default grammar when
/// `pool_meta` is missing.
#[test]
fn open_refuses_a_file_that_is_not_a_pool() {
    let temp = Temp::new("notapool");
    let path = temp.path().join("fixture-alpha.db");
    std::fs::write(&path, b"this is not a database").expect("write");
    match Pool::open(&path).map(|_| ()) {
        Err(SpineError::Hard { .. }) => {}
        other => panic!("a store that will not answer is never an empty store; got {other:?}"),
    }
    match Pool::open(&temp.path().join("nothing.db")).map(|_| ()) {
        Err(SpineError::NotFound { .. }) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

/// The pin round-trips: what `create` wrote is what `open` reads.
///
/// RED once by: writing `grammar_version` as `grammar.version + 1` in
/// `create`, which `open`'s disagreement check then refuses.
#[test]
fn the_pinned_grammar_round_trips_through_the_file() {
    let temp = Temp::new("pin");
    Pool::create(temp.path(), &fixture_beta()).expect("create");
    let reopened = Pool::open(&Pool::path_in(temp.path(), &PoolName::new("fixture-beta")))
        .expect("open");
    assert_eq!(reopened.name().as_str(), "fixture-beta");
    assert_eq!(*reopened.grammar(), fixture_beta());
}

// ---------------------------------------------------------------------------
// 5. the landing transaction
// ---------------------------------------------------------------------------

/// The load-bearing storage consequence of slice locking: edges must be stored
/// so that adding an edge does NOT rewrite the node's body record, or the
/// carve-out is physically unimplementable.
///
/// RED once by: adding `UPDATE nodes SET rev = rev + 1, updated_tx = ?` to the
/// `AddEdge` arm — the body row then moves and the assertion fires.
#[test]
fn an_edge_insert_leaves_the_node_body_row_byte_stable() {
    let (_t, mut pool) = alpha_pool("edgestable");
    pool.apply_part(
        &tx(1),
        0,
        &part(
            "fixture-alpha",
            1,
            vec![
                create("nA", "author", None, &[("name", "ada")]),
                create("nD", "doc", Some("draft"), &[("title", "on machines")]),
            ],
        ),
    )
    .expect("seed");

    let before = {
        let s = pool.snapshot().expect("snapshot");
        (
            s.node(&nid("nA")).expect("read").expect("author"),
            s.node(&nid("nD")).expect("read").expect("doc"),
        )
    };

    pool.apply_part(
        &tx(2),
        0,
        &part(
            "fixture-alpha",
            1,
            vec![Delta::AddEdge {
                src: nid("nA"),
                kind: "wrote".into(),
                dst: qid("fixture-alpha", "nD"),
                note: Some("connect".into()),
                fields: Default::default(),
            }],
        ),
    )
    .expect("edge lands");

    let s = pool.snapshot().expect("snapshot");
    let after = (
        s.node(&nid("nA")).expect("read").expect("author"),
        s.node(&nid("nD")).expect("read").expect("doc"),
    );
    assert_eq!(
        before.0, after.0,
        "connecting an edge must not rewrite the SOURCE body row"
    );
    assert_eq!(
        before.1, after.1,
        "connecting an edge must not rewrite the TARGET body row"
    );
    // ...and the edge really is there, as its own record.
    let edges = s.edges_from(&nid("nA"), Some("wrote")).expect("edges");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].dst, qid("fixture-alpha", "nD"));
    assert_eq!(edges[0].note.as_deref(), Some("connect"));
}

/// Contention proven at N >= 2 (the proving rule): two concurrent
/// landings on the SAME pool file each add one `wrote` edge from the same
/// author. The machine-maintained cell must end at 2 — which it can only do
/// if the increment commutes inside SQLite rather than being read into a
/// process and written back.
///
/// RED once by: changing the upsert to `DO UPDATE SET value = excluded.value`
/// (last-write-wins, the read-modify-write failure in its cheapest form) —
/// the cell then lands on 1 and the assertion fires.
#[test]
fn the_count_cell_commutes_under_two_concurrent_landings() {
    const WRITERS: usize = 2;
    const ROUNDS: usize = 12;

    let temp = Temp::new("commute");
    let mut pool = Pool::create(temp.path(), &fixture_alpha()).expect("create");
    let mut seed = vec![create("nA", "author", None, &[("name", "ada")])];
    for w in 0..WRITERS {
        for r in 0..ROUNDS {
            seed.push(create(
                &format!("nD{w}-{r}"),
                "doc",
                Some("draft"),
                &[("title", "d")],
            ));
        }
    }
    pool.apply_part(&tx(1), 0, &part("fixture-alpha", 1, seed))
        .expect("seed");
    drop(pool);

    let path = Pool::path_in(temp.path(), &PoolName::new("fixture-alpha"));
    // A barrier PER ROUND, so the two writers are released together twelve
    // times over rather than racing once and hoping.
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(WRITERS));
    let mut handles = Vec::new();
    for w in 0..WRITERS {
        let path = path.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            let mut pool = Pool::open(&path).expect("open");
            for r in 0..ROUNDS {
                let p = part(
                    "fixture-alpha",
                    1,
                    vec![Delta::AddEdge {
                        src: nid("nA"),
                        kind: "wrote".into(),
                        dst: qid("fixture-alpha", &format!("nD{w}-{r}")),
                        note: None,
                        fields: Default::default(),
                    }],
                );
                barrier.wait();
                pool.apply_part(&tx((10 + w * ROUNDS + r) as u8), 0, &p)
                    .expect("land");
            }
        }));
    }
    for h in handles {
        h.join().expect("writer thread");
    }

    let total = (WRITERS * ROUNDS) as i64;
    let mut pool = Pool::open(&path).expect("reopen");
    {
        let s = pool.snapshot().expect("snapshot");
        assert_eq!(
            s.count_cell(&nid("nA"), "docs_written").expect("cell"),
            Some(total),
            "every concurrent edge-connect must be counted exactly once"
        );
        assert_eq!(
            s.edges_from(&nid("nA"), Some("wrote")).expect("edges").len() as i64,
            total
        );
    }

    // The cell is DERIVED, not spoken: removing an edge moves it back, by the
    // same commutative UPDATE.
    pool.apply_part(
        &tx(200),
        0,
        &part(
            "fixture-alpha",
            1,
            vec![Delta::RemoveEdge {
                src: nid("nA"),
                kind: "wrote".into(),
                dst: qid("fixture-alpha", "nD0-0"),
            }],
        ),
    )
    .expect("remove");
    assert_eq!(
        pool.snapshot()
            .expect("snapshot")
            .count_cell(&nid("nA"), "docs_written")
            .expect("cell"),
        Some(total - 1)
    );
}

/// Per-field revisions are the bookkeeping EXPECT rides on.
///
/// RED once by: dropping `rev = rev + 1` from the `node_fields` UPDATE — the
/// touched field's revision then stays at 1 and compare-and-swap is blind.
#[test]
fn per_field_revisions_move_only_for_the_field_that_moved() {
    let temp = Temp::new("revs");
    // A two-field type, so "only the field that moved" is observable at all.
    let mut g = fixture_alpha();
    g.node_types.get_mut("doc").unwrap().fields.insert(
        "subtitle".into(),
        FieldDef::optional(),
    );
    let mut pool = Pool::create(temp.path(), &g).expect("create");
    pool.apply_part(
        &tx(1),
        0,
        &part(
            "fixture-alpha",
            1,
            vec![create(
                "nD",
                "doc",
                Some("draft"),
                &[("title", "first"), ("subtitle", "sub")],
            )],
        ),
    )
    .expect("seed");

    let n = pool
        .snapshot()
        .unwrap()
        .node(&nid("nD"))
        .unwrap()
        .expect("doc");
    assert_eq!(n.rev, 1);
    assert_eq!(n.fields["title"].rev, 1);
    assert_eq!(n.fields["subtitle"].rev, 1);

    pool.apply_part(
        &tx(2),
        0,
        &part(
            "fixture-alpha",
            1,
            vec![Delta::SetField {
                node: nid("nD"),
                path: vec!["title".into()],
                value: serde_json::json!("second"),
            }],
        ),
    )
    .expect("set");

    let n = pool
        .snapshot()
        .unwrap()
        .node(&nid("nD"))
        .unwrap()
        .expect("doc");
    assert_eq!(n.fields["title"].value, serde_json::json!("second"));
    assert_eq!(n.fields["title"].rev, 2, "the field that moved");
    assert_eq!(n.fields["subtitle"].rev, 1, "the field that did not");
    assert_eq!(n.rev, 2, "the whole-node revision moves with any body change");

    // A dotted SET addresses INTO the field's own JSON value — and it never
    // destroys a scalar to get there. `subtitle` holds the string
    // `"sub"`, so `subtitle.lang` refuses `not_an_object`, whole: the value
    // and its revision are untouched. (This test used to assert the
    // opposite — that the string was silently replaced by `{"lang": "en"}`
    // — which encoded the defect as expected behaviour.)
    let err = pool
        .apply_part(
            &tx(3),
            0,
            &part(
                "fixture-alpha",
                1,
                vec![Delta::SetField {
                    node: nid("nD"),
                    path: vec!["subtitle".into(), "lang".into()],
                    value: serde_json::json!("en"),
                }],
            ),
        )
        .expect_err("a dotted SET through a scalar refuses");
    expect_fault(&err, "fixture-alpha", "not_an_object");
    let n = pool
        .snapshot()
        .unwrap()
        .node(&nid("nD"))
        .unwrap()
        .expect("doc");
    assert_eq!(n.fields["subtitle"].value, serde_json::json!("sub"), "nothing landed");
    assert_eq!(n.fields["subtitle"].rev, 1, "and nothing moved");
    assert_eq!(n.rev, 2);

    // Over `null` (a legal SET value) the way is clear: the landing mints
    // the object, and the field's revision moves with it.
    pool.apply_part(
        &tx(3),
        0,
        &part(
            "fixture-alpha",
            1,
            vec![
                Delta::SetField {
                    node: nid("nD"),
                    path: vec!["subtitle".into()],
                    value: serde_json::Value::Null,
                },
                Delta::SetField {
                    node: nid("nD"),
                    path: vec!["subtitle".into(), "lang".into()],
                    value: serde_json::json!("en"),
                },
            ],
        ),
    )
    .expect("nested set over null");
    let n = pool
        .snapshot()
        .unwrap()
        .node(&nid("nD"))
        .unwrap()
        .expect("doc");
    assert_eq!(n.fields["subtitle"].value, serde_json::json!({"lang": "en"}));
    assert_eq!(n.fields["subtitle"].rev, 3, "two SETs on the field: two moves");

    // A state change lands in the state column and moves the node revision.
    pool.apply_part(
        &tx(4),
        0,
        &part(
            "fixture-alpha",
            1,
            vec![Delta::SetField {
                node: nid("nD"),
                path: vec!["state".into()],
                value: serde_json::json!("published"),
            }],
        ),
    )
    .expect("state set");
    let n = pool
        .snapshot()
        .unwrap()
        .node(&nid("nD"))
        .unwrap()
        .expect("doc");
    assert_eq!(n.state.as_deref(), Some("published"));
    // 5: create (1), title (2), the null-then-nested pair on subtitle (3, 4),
    // the state change (5). The refused dotted SET moved nothing.
    assert_eq!(n.rev, 5);
    assert_eq!(
        pool.snapshot()
            .unwrap()
            .nodes_of("doc", Some("published"))
            .unwrap()
            .len(),
        1
    );
    assert!(pool
        .snapshot()
        .unwrap()
        .nodes_of("doc", Some("draft"))
        .unwrap()
        .is_empty());
}

/// A part is ONE atomic step in this pool. The at-most-one check
/// fires AFTER the deltas have been written, so this exercises a genuine
/// mid-transaction abort — not merely a pre-flight refusal.
///
/// RED once by: `sql.commit()` before the scalar-slice pass — the created doc
/// then survives a refused part, which is exactly the tear the design forbids.
#[test]
fn a_refused_part_lands_nothing_at_all() {
    let (_t, mut pool) = alpha_pool("atomic");
    pool.apply_part(
        &tx(1),
        0,
        &part(
            "fixture-alpha",
            1,
            vec![
                create("nA", "author", None, &[("name", "ada")]),
                create("nD1", "doc", Some("draft"), &[("title", "one")]),
            ],
        ),
    )
    .expect("seed");

    // One part: create a second doc, then point BOTH docs at the author
    // through the at-most-one `current_of` slice.
    let err = pool
        .apply_part(
            &tx(2),
            0,
            &part(
                "fixture-alpha",
                1,
                vec![
                    create("nD2", "doc", Some("draft"), &[("title", "two")]),
                    Delta::AddEdge {
                        src: nid("nD1"),
                        kind: "current_of".into(),
                        dst: qid("fixture-alpha", "nA"),
                        note: None,
                        fields: Default::default(),
                    },
                    Delta::AddEdge {
                        src: nid("nD2"),
                        kind: "current_of".into(),
                        dst: qid("fixture-alpha", "nA"),
                        note: None,
                        fields: Default::default(),
                    },
                ],
            ),
        )
        .unwrap_err();
    expect_fault(&err, "fixture-alpha", "at_most_one_violated");

    {
        let s = pool.snapshot().expect("snapshot");
        assert!(
            s.node(&nid("nD2")).expect("read").is_none(),
            "the node created earlier in the refused part must be rolled back"
        );
        assert!(
            s.edges_to(&qid("fixture-alpha", "nA"), Some("current_of"))
                .expect("edges")
                .is_empty(),
            "no edge from a refused part survives"
        );
        assert!(
            !s.part_applied(&tx(2), 0).expect("marker"),
            "a refused part leaves no idempotence marker"
        );
    }

    // The slice takes ONE occupant happily.
    pool.apply_part(
        &tx(3),
        0,
        &part(
            "fixture-alpha",
            1,
            vec![Delta::AddEdge {
                src: nid("nD1"),
                kind: "current_of".into(),
                dst: qid("fixture-alpha", "nA"),
                note: None,
                fields: Default::default(),
            }],
        ),
    )
    .expect("one occupant is legal");
}

/// Recovery replays parts; replaying a landed one must be a NO-OP (recovery
/// is idempotent: run it twice, same result). Keyed by the
/// transaction hash plus the part ordinal.
///
/// RED once by: deleting the `applied_parts` pre-read — the second apply then
/// double-counts the cell (and, for a create, fails on the primary key, which
/// is a different and much less honest kind of red).
#[test]
fn re_applying_a_landed_part_is_a_no_op() {
    let (_t, mut pool) = alpha_pool("idem");
    pool.apply_part(
        &tx(1),
        0,
        &part(
            "fixture-alpha",
            1,
            vec![
                create("nA", "author", None, &[("name", "ada")]),
                create("nD", "doc", Some("draft"), &[("title", "one")]),
            ],
        ),
    )
    .expect("seed");

    let edge = part(
        "fixture-alpha",
        1,
        vec![Delta::AddEdge {
            src: nid("nA"),
            kind: "wrote".into(),
            dst: qid("fixture-alpha", "nD"),
            note: None,
            fields: Default::default(),
        }],
    );
    let first = pool.apply_part(&tx(7), 3, &edge).expect("first landing");
    assert!(!first.already_landed);
    assert_eq!(first.tx, tx(7));

    let second = pool.apply_part(&tx(7), 3, &edge).expect("replay");
    assert!(
        second.already_landed,
        "a landed part re-applied is detected, not repeated"
    );

    let s = pool.snapshot().expect("snapshot");
    assert_eq!(s.count_cell(&nid("nA"), "docs_written").unwrap(), Some(1));
    assert!(s.part_applied(&tx(7), 3).unwrap());
    // The key is (hash, part): a DIFFERENT part ordinal of the same
    // transaction is a different landing.
    assert!(!s.part_applied(&tx(7), 4).unwrap());
    assert!(!s.part_applied(&tx(8), 3).unwrap());
}

/// The stamp is copied from the pool's pin at creation, no API
/// path lets a caller set it, and it appears in NO default render.
///
/// RED once by: adding `schema_version` to `render_default` — the stamp then
/// shows up in an ordinary read, which version-blindness forbids twice over.
#[test]
fn the_schema_version_stamp_is_machine_owned_and_invisible_by_default() {
    let temp = Temp::new("stamp");
    let mut g = fixture_alpha();
    g.version = 9; // an arbitrary pin, to prove the stamp is COPIED from it
    let mut pool = Pool::create(temp.path(), &g).expect("create");
    pool.apply_part(
        &tx(1),
        0,
        &part(
            "fixture-alpha",
            9,
            vec![create("nD", "doc", Some("draft"), &[("title", "one")])],
        ),
    )
    .expect("seed");

    let n = pool
        .snapshot()
        .unwrap()
        .node(&nid("nD"))
        .unwrap()
        .expect("doc");
    assert_eq!(n.schema_version, 9, "copied from the pool's pin at create");
    assert!(!n.stamp_skewed(pool.grammar()), "no skew against its own pin");

    let default = n.render_default();
    assert!(
        default.get("schema_version").is_none(),
        "the stamp appears in NO default render"
    );
    assert_eq!(default["id"], "nD");
    assert_eq!(default["state"], "draft");
    assert_eq!(default["fields"]["title"], "one");

    assert_eq!(
        n.render_with_stamp()["schema_version"],
        9,
        "visible on explicit ask, and only there"
    );

    // No pen can set it: the delta vocabulary has no variant for it, and
    // addressing it as a field refuses.
    let err = pool
        .apply_part(
            &tx(2),
            0,
            &part(
                "fixture-alpha",
                9,
                vec![Delta::SetField {
                    node: nid("nD"),
                    path: vec!["schema_version".into()],
                    value: serde_json::json!(1),
                }],
            ),
        )
        .unwrap_err();
    expect_fault(&err, "fixture-alpha", "machine_owned_field");
    assert_eq!(
        pool.snapshot()
            .unwrap()
            .node(&nid("nD"))
            .unwrap()
            .unwrap()
            .schema_version,
        9
    );
}

/// THE AFFIRMATIVE BRANCH of the same clause, which nothing had ever
/// observed: `stamp_skewed` had only been asserted `false` (against a node's
/// own pool pin, where it is false by construction), so `true` was a branch no
/// test had ever entered — and a `stamp_skewed` that returned `false`
/// unconditionally would have passed the whole suite.
///
/// The stamp becomes visible exactly when it matters — on
/// explicit ask; WHEN A NODE'S STAMP DIFFERS FROM ITS POOL'S PIN; and to
/// migration tooling. Skew is exactly when the stamp carries information, so
/// this is the case the stamp exists for: a node written under pin 9, read
/// after the pool's grammar has moved on.
///
/// The render policy is re-asserted HERE, on the skewed node, and not only on
/// the un-skewed one: a stamp that leaked into `render_default` would leak
/// hardest precisely where it differs.
// RED: (a) making `stamp_skewed` return `false` unconditionally, or comparing
// `self.schema_version != self.schema_version` — every `assert!(skewed)`
// below fires while the existing negative test stays green, which is the
// asymmetry that let the affirmative branch go unobserved; (b) making
// `render_with_stamp` return `self.render_default()` unchanged — the
// `schema_version` lookup on it fires; (c) adding `schema_version` to
// `render_default` — the absence assertion fires.
#[test]
fn a_node_whose_stamp_differs_from_the_pools_pin_reports_skew_and_renders_it_on_ask() {
    let temp = Temp::new("stamp-skew");
    let mut pinned_at_9 = fixture_alpha();
    pinned_at_9.version = 9;
    let mut pool = Pool::create(temp.path(), &pinned_at_9).expect("create");
    pool.apply_part(
        &tx(1),
        0,
        &part(
            "fixture-alpha",
            9,
            vec![create("nSkew", "doc", Some("draft"), &[("title", "written at 9")])],
        ),
    )
    .expect("seed");

    let n = pool
        .snapshot()
        .unwrap()
        .node(&nid("nSkew"))
        .unwrap()
        .expect("doc");
    assert_eq!(n.schema_version, 9);
    // Against its OWN pin: no skew. This is the only case the suite had.
    assert!(!n.stamp_skewed(pool.grammar()), "no skew against its own pin");

    // AGAINST A PIN THAT MOVED: skew, in BOTH directions — a grammar ahead of
    // the node (the migration case) and one behind it (a node written by a
    // newer build than the one now reading the pool).
    for moved_to in [10, 1] {
        let mut moved = fixture_alpha();
        moved.version = moved_to;
        assert!(
            n.stamp_skewed(&moved),
            "a node stamped 9 is SKEWED against a pool pinned at {moved_to}"
        );
    }
    // And it is the version and nothing else that decides: a second grammar
    // pinned at 9 that differs everywhere else is still not skew.
    let mut same_version_other_shape = fixture_beta();
    same_version_other_shape.version = 9;
    assert!(
        !n.stamp_skewed(&same_version_other_shape),
        "skew is the VERSION comparison alone, not a grammar comparison"
    );

    // THE RENDER POLICY, on the skewed node. The stamp is out of the default
    // render and in the explicit one — the same two rules, asserted where they
    // are load-bearing.
    let default = n.render_default();
    assert!(
        default.get("schema_version").is_none(),
        "a SKEWED stamp appears in no default render either"
    );
    assert_eq!(default["id"], "nSkew");
    assert_eq!(default["fields"]["title"], "written at 9");

    let stamped = n.render_with_stamp();
    assert_eq!(
        stamped["schema_version"], 9,
        "the explicit ask is the ONLY way the stamp reaches a render"
    );
    // …and `render_with_stamp` is the default render PLUS the stamp: it adds
    // one key and changes nothing else.
    let mut stripped = stamped.clone();
    stripped
        .as_object_mut()
        .expect("an object")
        .remove("schema_version");
    assert_eq!(stripped, default, "it adds the stamp and nothing else");
}

/// The third leak path: a single-pool snapshot must never see a
/// torn per-pool landing. The snapshot is opened before a landing and must
/// see NONE of it, however many deltas that landing carries.
///
/// RED once by: making `snapshot()` hand back a bare connection with no read
/// transaction — the snapshot then sees the landing appear underneath it.
#[test]
fn a_single_pool_snapshot_never_sees_a_torn_landing() {
    let temp = Temp::new("snap");
    let mut writer = Pool::create(temp.path(), &fixture_alpha()).expect("create");
    writer
        .apply_part(
            &tx(1),
            0,
            &part(
                "fixture-alpha",
                1,
                vec![create("nA", "author", None, &[("name", "ada")])],
            ),
        )
        .expect("seed");

    let path = Pool::path_in(temp.path(), &PoolName::new("fixture-alpha"));
    let reader = Pool::open(&path).expect("second handle");
    let snap = reader.snapshot().expect("snapshot");
    assert_eq!(snap.nodes_of("doc", None).unwrap().len(), 0);

    writer
        .apply_part(
            &tx(2),
            0,
            &part(
                "fixture-alpha",
                1,
                vec![
                    create("nD1", "doc", Some("draft"), &[("title", "one")]),
                    create("nD2", "doc", Some("draft"), &[("title", "two")]),
                    Delta::AddEdge {
                        src: nid("nA"),
                        kind: "wrote".into(),
                        dst: qid("fixture-alpha", "nD1"),
                        note: None,
                        fields: Default::default(),
                    },
                ],
            ),
        )
        .expect("land while the snapshot is open");

    assert_eq!(
        snap.nodes_of("doc", None).unwrap().len(),
        0,
        "the open snapshot sees none of a landing that began after it"
    );
    assert_eq!(
        snap.count_cell(&nid("nA"), "docs_written").unwrap(),
        None,
        "including the derived cells"
    );
    drop(snap);

    let after = reader.snapshot().expect("fresh snapshot");
    assert_eq!(
        after.nodes_of("doc", None).unwrap().len(),
        2,
        "a fresh snapshot sees ALL of it — the landing is one atomic step"
    );
    assert_eq!(
        after.count_cell(&nid("nA"), "docs_written").unwrap(),
        Some(1)
    );
}

/// The cross-pool edge is stored in beta with its destination pool ALWAYS
/// explicit, and alpha's file is untouched by it —
/// no ATTACH, one file per landing.
///
/// RED once by: writing `dst_pool` as the local pool name in the `AddEdge`
/// arm, which loses the cross-pool address entirely.
#[test]
fn the_cross_pool_edge_lands_in_beta_and_names_alpha_explicitly() {
    let temp = Temp::new("crosspool");
    let mut alpha = Pool::create(temp.path(), &fixture_alpha()).expect("alpha");
    let mut beta = Pool::create(temp.path(), &fixture_beta()).expect("beta");

    alpha
        .apply_part(
            &tx(1),
            0,
            &part(
                "fixture-alpha",
                1,
                vec![create("nD", "doc", Some("draft"), &[("title", "one")])],
            ),
        )
        .expect("alpha part");
    // The part carries the door's record of alpha's pin: a landing
    // has no window onto alpha's file and asks for the record instead.
    beta.apply_part(
        &tx(1),
        1,
        &part_reaching(
            "fixture-beta",
            2,
            vec![
                create("nT", "ticket", Some("open"), &[]),
                Delta::AddEdge {
                    src: nid("nT"),
                    kind: "catalogues".into(),
                    dst: qid("fixture-alpha", "nD"),
                    note: None,
                    fields: Default::default(),
                },
            ],
            &[("fixture-alpha", 1)],
        ),
    )
    .expect("beta part");

    {
        let s = beta.snapshot().expect("snapshot");
        let edges = s.edges_from(&nid("nT"), Some("catalogues")).expect("edges");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].dst.pool.as_str(), "fixture-alpha");
        assert_eq!(edges[0].dst.id.0, "nD");
        // Both parts of one transaction, each stamped with its OWN pool's pin.
        assert!(s.part_applied(&tx(1), 1).unwrap());
        assert_eq!(
            s.node(&nid("nT")).unwrap().unwrap().schema_version,
            2,
            "beta's node carries beta's pin"
        );
    }
    {
        let s = alpha.snapshot().expect("snapshot");
        assert!(s.part_applied(&tx(1), 0).unwrap());
        assert_eq!(
            s.node(&nid("nD")).unwrap().unwrap().schema_version,
            1,
            "alpha's node carries alpha's pin — two pools, two versions, one transaction"
        );
        // alpha's own file holds no beta edge — one landing, one file.
        assert!(s
            .edges_to(&qid("fixture-alpha", "nD"), None)
            .unwrap()
            .is_empty());
    }
}

// ---------------------------------------------------------------------------
// 6. the projected namespace
// ---------------------------------------------------------------------------

/// The pen refuses any write addressed to the projected namespace, with a
/// named structured refusal (an editable projection of code state
/// is a lie waiting to be written).
///
/// RED once by: making `guard_write_target` return `Ok(())` unconditionally.
#[test]
fn the_pen_refuses_a_write_addressed_to_the_projected_namespace() {
    let target = qid(project::PROJECTED_POOL, &"ab".repeat(32));
    let err = project::guard_write_target(&target).unwrap_err();
    match &witnesses(&err)[0] {
        Witness::ProjectedNamespaceWrite { pool, target: t } => {
            assert_eq!(pool.as_str(), "spine");
            assert_eq!(*t, "ab".repeat(32));
        }
        other => panic!("expected ProjectedNamespaceWrite, got {other:?}"),
    }
    // An ordinary pool target is not this guard's business.
    project::guard_write_target(&qid("fixture-alpha", "nD")).expect("ordinary target passes");
    assert!(project::is_projected(&PoolName::new("spine")));
    assert!(!project::is_projected(&PoolName::new("fixture-alpha")));
}

/// The ghost identifier IS the row's address (the target is the
/// address, not a copy).
///
/// RED once by: minting the ghost id from the seq instead of the hash.
#[test]
fn a_ghost_identifier_is_the_transactions_hash_hex() {
    let h = tx(0xab);
    assert_eq!(project::ghost_id(&h).0, h.to_hex());
    assert_eq!(project::ghost_id(&h).0.len(), 64);
}

/// A pool may never be named `spine`, which is what keeps the projected
/// namespace un-shadowable.
///
/// RED once by: dropping the `is_reserved` check (covered above from the pool
/// side; asserted here at the name level too, because the two callers are
/// different).
#[test]
fn the_reserved_namespace_cannot_be_taken_by_a_pool() {
    assert!(PoolName::new("spine").is_reserved());
    assert!(!PoolName::new("fixture-alpha").is_reserved());
    assert!(!PoolName::new("Fixture").is_well_formed());
    assert!(!PoolName::new("9pool").is_well_formed());
    assert!(PoolName::new("fixture-alpha").is_well_formed());
}

// --- the two projection paths that need a LIVE spine row -------------------
//
// `project::resolve_ghost` and `project::guard_edge_target` call
// `spine_db::SpineDb`'s frozen read signatures.
// Those bodies are now REAL, so both tests below run (they were `#[ignore]`d
// while `spine_db.rs` was still `NotBuilt`).
//
// `envelope::Envelope::hash_identity` is real. It is STILL deliberately not
// used below: `append_pending` takes the hash as a PARAMETER, and the
// contract these tests exercise is the projection's — "the target is the
// address, not a copy" — which is indifferent to how the
// address was computed. A synthetic hash proves exactly that promise.

#[test]
fn a_ghost_identifier_naming_no_row_resolves_to_none() {
    let temp = Temp::new("ghostnone");
    let spine = spine::spine_db::SpineDb::create(&temp.path().join("spine.db")).expect("spine");
    // Well-formed address, no row behind it.
    assert!(project::resolve_ghost(&spine, &nid(&"cd".repeat(32)))
        .expect("resolve")
        .is_none());
    // Not an address at all.
    assert!(project::resolve_ghost(&spine, &nid("nD"))
        .expect("resolve")
        .is_none());
    // ...and an edge minted toward it is refused by name.
    let err = project::guard_edge_target(&spine, &qid(project::PROJECTED_POOL, &"cd".repeat(32)))
        .unwrap_err();
    match &witnesses(&err)[0] {
        Witness::GhostTargetUnknown { target } => {
            assert!(target.contains(&"cd".repeat(32)));
        }
        other => panic!("expected GhostTargetUnknown, got {other:?}"),
    }
    // A non-projected destination is not this guard's business.
    project::guard_edge_target(&spine, &qid("fixture-alpha", "nD")).expect("ordinary dst");
}

#[test]
fn a_spine_row_projects_as_a_txn_node_computed_on_read() {
    use spine::delta::CompiledPayload;
    use spine::envelope::{Envelope, Initiator, Instrument, Mode, Provenance};

    let temp = Temp::new("ghost");
    let mut spine_db =
        spine::spine_db::SpineDb::create(&temp.path().join("spine.db")).expect("spine");
    let env = Envelope {
        statement: "CREATE (d: fixture-alpha/doc {title: $t})".to_string(),
        bindings: BTreeMap::from([("t".to_string(), serde_json::json!("one"))]),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::CoreMechanical {
                operation: "task-02-test".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    };
    // NOT `env.hash_identity()`: the address is a
    // PARAMETER to `append_pending`, and what the projection promises is that
    // this address resolves to this row — not how the address was derived.
    // A synthetic hash proves exactly that promise.
    let hash = tx(0x5a);
    let payload = CompiledPayload {
        parts: vec![part(
            "fixture-alpha",
            1,
            vec![create("nD", "doc", Some("draft"), &[("title", "one")])],
        )],
        minted: BTreeMap::new(),
    };
    spine_db
        .append_pending(&env, &hash, &payload)
        .expect("append");

    let ghost = project::ghost_id(&hash);
    let node = project::resolve_ghost(&spine_db, &ghost)
        .expect("resolve")
        .expect("the row is there");
    assert_eq!(node.node_type, project::TXN_NODE_TYPE);
    assert_eq!(node.state.as_deref(), Some("pending"));
    assert_eq!(node.rev, 1);
    assert_eq!(node.fields["statement"].value, serde_json::json!(env.statement));
    let parts = node.fields["parts"].value.as_array().expect("parts");
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["pool"], "fixture-alpha");
    assert_eq!(parts[0]["landed"], false);
    assert_eq!(parts[0]["validated_under"], 1);

    // Computed on READ, never copied: every lifecycle flip is visible on the
    // next call with no projection write anywhere. All THREE states are
    // walked — a projection that froze at any one of them would still satisfy
    // a two-state check.
    let seq = spine_db.pending().expect("pending")[0].seq;
    spine_db.mark_decided(seq).expect("decide");
    let again = project::resolve_ghost(&spine_db, &ghost)
        .expect("resolve")
        .expect("still there");
    assert_eq!(again.state.as_deref(), Some("decided"));
    assert_eq!(
        again.fields["parts"].value.as_array().expect("parts")[0]["landed"],
        false,
        "decided is not landed"
    );

    spine_db.mark_part_landed(seq, 0).expect("land part 0");
    spine_db
        .mark_cleared(seq, &serde_json::json!({}))
        .expect("clear");
    let cleared = project::resolve_ghost(&spine_db, &ghost)
        .expect("resolve")
        .expect("still there");
    assert_eq!(cleared.state.as_deref(), Some("cleared"));
    assert_eq!(
        cleared.fields["parts"].value.as_array().expect("parts")[0]["landed"],
        true,
        "the per-part landed flag is derived from the row on every read"
    );

    // An edge toward a ghost that DOES name a row is allowed.
    project::guard_edge_target(&spine_db, &qid(project::PROJECTED_POOL, &ghost.0))
        .expect("a real row is a legal edge target");
}

// A silence-breaker: `NodeTypeDef` is used above only through the fixtures, so
// name it once to keep the import honest if the fixtures ever stop needing it.
#[allow(dead_code)]
fn _type_check(_: &NodeTypeDef) {}
