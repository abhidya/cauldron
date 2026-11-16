//! Every refusal THE DOOR gives carries its frozen tag.
//!
//! Through `Cauldron::submit` a statement is refused by the COMPILER
//! (`ladle::compile`) before the one validator (`PoolGrammar::validate_part`)
//! sees most of it. Previously those compile-stage refusals were untagged
//! prose in a `Witness::GrammarViolation`, so `grammar_fault_tag` answered
//! `None` for them while the validator's refusal of the same fact answered
//! the tag. These tests ask the DOOR, and a sibling asks the validator the
//! same question over the same part, so a forcing that reverts a tag at a
//! compile site turns the door test red while the validator test stays
//! green — which is what proves the door, not the validator, is under test.
//!
//! No tag or witness variant is new: every tag asserted here is already in
//! `schema_def::GRAMMAR_FAULTS`, and the witness is the existing
//! `GrammarViolation` read back by `grammar_fault_tag`.
//!
//! Every refusal is opposed by the legal statement of the same shape
//! landing, so a door that refused everything with the right
//! tag fails here. The forcings are named in `// RED:` comments.

use spine::config::SpineConfig;
use spine::delta::{CompiledPart, Delta};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::pool::{NodeId, QualifiedNodeId};
use spine::schema_def::{
    grammar_fault_tag, EdgeKindDef, FarSide, FieldDef, NodeTypeDef, PoolGrammar, PoolKind,
    PoolName,
};
use spine::witness::Witness;
use spine::{Cauldron, Outcome};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

test_scratch::install!();

const POOL: &str = "fixture-tags";

/// `task` mints serial `T-<n>` ids (a prefixed type) and has a state
/// vocabulary; `owner` keeps the hash rule and has none. `owns` leaves a
/// task and arrives at an owner; `follows` joins two tasks.
fn grammar() -> PoolGrammar {
    let mut node_types = BTreeMap::new();
    node_types.insert(
        "task".to_string(),
        NodeTypeDef {
            fields: BTreeMap::from([("name".to_string(), FieldDef::text(true, 40))]),
            states: vec!["open".to_string(), "done".to_string()],
            id_prefix: Some("T".to_string()),
            ..Default::default()
        },
    );
    node_types.insert(
        "owner".to_string(),
        NodeTypeDef {
            fields: BTreeMap::from([("name".to_string(), FieldDef::required())]),
            ..Default::default()
        },
    );
    let mut edge_kinds = BTreeMap::new();
    edge_kinds.insert("owns".to_string(), EdgeKindDef::local("task", "owner"));
    edge_kinds.insert("follows".to_string(), EdgeKindDef::local("task", "task"));
    PoolGrammar {
        pool: PoolName::new(POOL),
        kind: PoolKind::new("tags"),
        version: 1,
        node_types,
        edge_kinds,
    }
}

// ---------------------------------------------------------------------------
// G1 — unknown edge kind
// ---------------------------------------------------------------------------

/// A CREATE-edge statement naming an edge kind the pool does not declare is
/// refused AT THE DOOR with tag `unknown_edge_kind`, stamped with the pool —
/// between two matched nodes and from a node minted in the same statement.
/// Opposed: a declared kind between the right endpoints lands, both ways.
// RED: (a) the tag reverted to the untagged `grammar_violation` in
// `edge_deltas` — this test red (`None` for the tag),
// `the_validator_agrees_with_the_door_on_every_edge_tag` green. Reverting it
// in `claims_for`'s edge claim ALONE left everything green: through the door
// `compile` (and so `edge_deltas`) refuses before `claims_for` ever sees the
// kind, so that site is tagged for consistency but no door test can reach
// it. (b) the opposing forcing, `edge_deltas` made to treat every kind as
// undeclared — the legal `owns` refused here.
#[test]
fn g1_an_unknown_edge_kind_is_refused_at_the_door_by_tag() {
    let dir = root("g1");
    let c = fresh(&dir);
    let task = mint_task(&c, "t");
    let owner = mint_owner(&c, "o");

    let w = refused(edge(&c, "task", "holds", "owner", &task, &owner));
    assert_tag(&w, "unknown_edge_kind");
    assert_eq!(validator_tag(&c, &task, "holds", &owner), Some("unknown_edge_kind"));

    let w = refused(c.submit(commit(
        r#"MATCH (o: fixture-tags/owner { id: $o })
           CREATE (t: fixture-tags/task { name: "fresh" })
           CREATE (t) -[holds]-> (o)"#,
        &[("o", &owner)],
    )));
    assert_tag(&w, "unknown_edge_kind");

    // opposed: the declared kind, right endpoints, both shapes
    edge(&c, "task", "owns", "owner", &task, &owner).expect("task -[owns]-> owner is legal");
    c.submit(commit(
        r#"MATCH (o: fixture-tags/owner { id: $o })
           CREATE (t: fixture-tags/task { name: "fresh" })
           CREATE (t) -[owns]-> (o)"#,
        &[("o", &owner)],
    ))
    .expect("a minted task -[owns]-> owner is legal");
}

// ---------------------------------------------------------------------------
// G2 — wrong endpoint type
// ---------------------------------------------------------------------------

/// A declared kind with a SOURCE of the wrong type is refused at the door
/// (by the compiler, `edge_deltas`) with tag `wrong_endpoint_type`; with a
/// TARGET of the wrong type it is refused at the door too (by the validator
/// the door runs over every part), same tag. Opposed: the right endpoints
/// land.
// RED: (a) the source check's tag reverted to the untagged
// `grammar_violation` in `edge_deltas` — the source half of this test red,
// the validator test green; (b) the opposing forcing, `edge_deltas`' source
// check made to refuse every source — the legal `owns` refused here.
#[test]
fn g2_a_wrong_endpoint_type_is_refused_at_the_door_by_tag() {
    let dir = root("g2");
    let c = fresh(&dir);
    let task = mint_task(&c, "t");
    let task2 = mint_task(&c, "t2");
    let owner = mint_owner(&c, "o");
    let owner2 = mint_owner(&c, "o2");

    // the SOURCE is wrong: `owns` leaves a task, this is an owner
    let w = refused(edge(&c, "owner", "owns", "owner", &owner, &owner2));
    assert_tag(&w, "wrong_endpoint_type");
    assert_eq!(validator_tag(&c, &owner, "owns", &owner2), Some("wrong_endpoint_type"));
    // the TARGET is wrong: `owns` arrives at an owner, this is a task
    let w = refused(edge(&c, "task", "owns", "task", &task, &task2));
    assert_tag(&w, "wrong_endpoint_type");

    // opposed: the right endpoints land, and `follows` takes the task the
    // `owns` target refused
    edge(&c, "task", "owns", "owner", &task, &owner).expect("task -[owns]-> owner is legal");
    edge(&c, "task", "follows", "task", &task, &task2).expect("task -[follows]-> task is legal");
}

/// The validator, asked directly over the same parts, has always tagged
/// these. It is HERE so a forcing at a compile site shows the split: this
/// stays green while the door tests go red.
#[test]
fn the_validator_agrees_with_the_door_on_every_edge_tag() {
    let dir = root("validator");
    let c = fresh(&dir);
    let task = mint_task(&c, "t");
    let owner = mint_owner(&c, "o");
    let owner2 = mint_owner(&c, "o2");
    assert_eq!(validator_tag(&c, &task, "holds", &owner), Some("unknown_edge_kind"));
    assert_eq!(validator_tag(&c, &owner, "owns", &owner2), Some("wrong_endpoint_type"));
    assert_eq!(validator_tag(&c, &task, "owns", &owner), None, "the legal edge validates");
}

// ---------------------------------------------------------------------------
// the door's other compile-stage refusals
// ---------------------------------------------------------------------------

/// Every other refusal the compiler gives a caller (`check_settable`, the
/// CREATE head's fields, head resolution) carries the SAME tag the validator
/// gives the same fact. Opposed by the legal SET / CREATE landing.
// RED: each tag reverted to untagged prose in `grammar_fault` itself, one
// tag at a time (machine_owned_field, unknown_state, undeclared_field,
// state_not_declared, unknown_node_type), and then the CREATE head's `id`,
// the CREATE head's non-string `state`, `check_settable`'s non-string
// `state` and its undeclared-field site one at a time — the matching
// assertion red with `None` every time. `check_settable`'s own
// `unknown_node_type` site was NOT forced: a MATCH on an undeclared type is
// refused at head resolution first, so no statement reaches it. Opposed:
// `check_settable` made to refuse every path with one tag — red on the
// first assertion naming another tag.
#[test]
fn every_compile_stage_refusal_carries_the_validators_tag() {
    let dir = root("others");
    let c = fresh(&dir);
    let task = mint_task(&c, "t");
    let owner = mint_owner(&c, "o");
    let set_task = |lhs: &str, rhs: &str| {
        c.submit(commit(
            &format!("MATCH (t: fixture-tags/task {{ id: $t }}) SET t.{lhs} = {rhs}"),
            &[("t", &task)],
        ))
    };

    assert_tag(&refused(set_task("id", r#""T-99""#)), "machine_owned_field");
    assert_tag(&refused(set_task("state", r#""bogus""#)), "unknown_state");
    assert_tag(&refused(set_task("state", "3")), "unknown_state");
    assert_tag(&refused(set_task("state.deep", r#""x""#)), "undeclared_field");
    assert_tag(&refused(set_task("nope", r#""x""#)), "undeclared_field");
    let w = refused(c.submit(commit(
        r#"MATCH (o: fixture-tags/owner { id: $o }) SET o.state = "open""#,
        &[("o", &owner)],
    )));
    assert_tag(&w, "state_not_declared");
    let w = refused(c.submit(commit(
        r#"CREATE (o: fixture-tags/owner { id: "n0", name: "x" })"#,
        &[],
    )));
    assert_tag(&w, "machine_owned_field");
    let w = refused(c.submit(commit(
        r#"CREATE (o: fixture-tags/owner { name: "x", state: 3 })"#,
        &[],
    )));
    assert_tag(&w, "unknown_state");
    let w = refused(c.submit(commit(r#"CREATE (g: fixture-tags/ghost { name: "x" })"#, &[])));
    assert_tag(&w, "unknown_node_type");

    // opposed: the legal forms land
    set_task("state", r#""done""#).expect("a state in the vocabulary is settable");
    set_task("name", r#""renamed""#).expect("a declared field is settable");
    c.submit(commit(r#"CREATE (o: fixture-tags/owner { name: "x" })"#, &[]))
        .expect("a declared type, no id, lands");
}

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

#[track_caller]
fn assert_tag(w: &Witness, tag: &str) {
    match w {
        Witness::GrammarViolation { pool, .. } => {
            assert_eq!(pool.as_str(), POOL, "the witness names the pool: {w:?}")
        }
        other => panic!("expected a GrammarViolation tagged `{tag}`, got {other:?}"),
    }
    assert_eq!(grammar_fault_tag(w), Some(tag), "{w:?}");
}

fn edge(
    c: &Cauldron,
    src_ty: &str,
    kind: &str,
    dst_ty: &str,
    src: &str,
    dst: &str,
) -> spine::error::Result<Outcome> {
    c.submit(commit(
        &format!(
            "MATCH (s: fixture-tags/{src_ty} {{ id: $s }}) MATCH (d: fixture-tags/{dst_ty} {{ id: $d }}) CREATE (s) -[{kind}]-> (d)"
        ),
        &[("s", src), ("d", dst)],
    ))
}

/// The tag the one validator gives a single `src -[kind]-> dst` part,
/// resolving node types from the cauldron's own pool (`None` = valid).
fn validator_tag(c: &Cauldron, src: &str, kind: &str, dst: &str) -> Option<&'static str> {
    let g = grammar();
    let types: BTreeMap<String, String> = ["task", "owner"]
        .iter()
        .flat_map(|ty| ids(c, ty).into_iter().map(move |id| (id, ty.to_string())))
        .collect();
    let known = |id: &NodeId| types.get(&id.0).cloned();
    let part = CompiledPart {
        pool: PoolName::new(POOL),
        schema_version: g.version,
        deltas: vec![Delta::AddEdge {
            src: NodeId(src.to_string()),
            kind: kind.into(),
            dst: QualifiedNodeId {
                pool: PoolName::new(POOL),
                id: NodeId(dst.to_string()),
            },
            note: None,
            fields: Default::default(),
        }],
        premises: vec![],
        far_versions: BTreeMap::new(),
    };
    match g.validate_part(&part, Some(0), &known, &FarSide::Landing) {
        Ok(()) => None,
        Err(SpineError::Refused { witnesses }) => {
            assert_eq!(witnesses.len(), 1, "{witnesses:?}");
            grammar_fault_tag(&witnesses[0])
        }
        Err(other) => panic!("expected a refusal, got {other:?}"),
    }
}

fn ids(c: &Cauldron, node_type: &str) -> Vec<String> {
    c.read(&spine::ReadRequest {
        statement: format!("MATCH (n: {POOL}/{node_type}) RETURN n.id"),
        bindings: Bindings::new(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    })
    .expect("read")
    .rows
    .0
    .iter()
    .map(|r| r["n.id"].as_str().expect("id").to_string())
    .collect()
}

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-door-tags-{name}-{}", std::process::id()))
}

fn fresh(dir: &Path) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(dir, &[grammar()], SpineConfig::default()).expect("create the cauldron")
}

fn commit(statement: &str, params: &[(&str, &str)]) -> Envelope {
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
                thread: "door-refusal-tags".into(),
                dispatch: "door-refusal-tags".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn returned_id(out: Outcome, col: &str) -> String {
    match out {
        Outcome::Committed { returned, .. } => returned.0[0][col].as_str().expect("id").to_string(),
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn mint_task(c: &Cauldron, name: &str) -> String {
    let id = returned_id(
        c.submit(commit(
            r#"CREATE (t: fixture-tags/task { name: $n, state: "open" }) RETURN t.id"#,
            &[("n", name)],
        ))
        .expect("a task lands"),
        "t.id",
    );
    assert!(id.starts_with("T-"), "a serial id: {id}");
    id
}

fn mint_owner(c: &Cauldron, name: &str) -> String {
    returned_id(
        c.submit(commit(
            r#"CREATE (o: fixture-tags/owner { name: $n }) RETURN o.id"#,
            &[("n", name)],
        ))
        .expect("an owner lands"),
        "o.id",
    )
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
