//! ADVERSARY — lens: GRAMMAR, REFUSAL AND THE ONE DOOR, the
//! CROSS-POOL half: a source pool declaring reach its target's grammar does
//! not permit, a target whose schema version moved, and a declared reach
//! toward a node type that does not exist.
//!
//! Every test asserts something that should obviously be true of the
//! cross-pool grammar. RED tests are committed
//! under `#[ignore = "RED (adversary_grammar): ..."]`; run with `--ignored`.
//! Nothing in `src/` was touched by this file's author.
//!
//! The pin-moved tests move a pool's pin by DIRECT SQL on `pool_meta`. No
//! supported operation in this build moves a pin (migration machinery is
//! not built yet), so today this is reachable only out-of-band —
//! but migration machinery is exactly what moves pins, and
//! the cauldron's grammar cache has no invalidation. The tests are written
//! against the behaviour the door owes, not against the mechanism used to
//! provoke it.

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance, TxHash};
use spine::error::SpineError;
use spine::pool::{NodeId, Pool};
use spine::schema_def::fixtures::{
    cross_pool_grammars, fixture_blueprint, fixture_workspace_at, BP, WS_A, WORKSPACE_PIN,
};
use spine::schema_def::{
    grammar_fault_tag, EdgeKindDef, FieldDef, NodeTypeDef, PoolGrammar, PoolName, TypeRef,
};
use spine::spine_db::{SpineDb, SPINE_FILE};
use spine::witness::Witness;
use spine::{Cauldron, Outcome};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

test_scratch::install!();

const BP_OLD: &str = "fixture-bp";
const BP_NEW: &str = "fixture-bp-new";
const WS: &str = "fixture-ws-adv";

// ===========================================================================
// A declared reach toward a node type that does not exist — judged where?
// ===========================================================================

/// SHOULD HOLD: a reach is BY KIND and "covers every pool of that kind"
/// (schema_def module docs). Two pools of one kind may be pinned at
/// different versions with different type sets — the fixtures model this
/// deliberately (ws-a at 3, ws-b at 5). A source declaring
/// `depends_on -> reach {blueprint, migration} | reach {blueprint, epic}`
/// is a forward-compatible declaration: it names what it may point at in a
/// blueprint that HAS `epic` and in one that does not. An edge toward a
/// `migration` in the OLDER blueprint — a type that pool declares, aimed by
/// a reach that names it — must land. Checking the schema version is a
/// check of THE TARGET AIMED AT.
///
/// ACTUAL: `validate_reach` refuses `foreign_type_unknown` because the
/// declaration ALSO names `epic`, which the older blueprint lacks, even
/// though the edge is aimed at `migration`. This was an interior choice
/// (refuse loudly rather than only when the dead type is
/// aimed at) — a choice that was harmless with one pool per kind and is
/// wrong with two: the declaration is unusable against the older pool for
/// ANY type, so a workspace cannot be declared ahead of the blueprints it
/// points at. The positive control (the same edge kind toward the NEWER
/// blueprint's `epic`) lands, proving the declaration itself is legal.
#[test]
fn a_reach_is_judged_against_the_type_aimed_at_not_every_type_it_names() {
    let dir = root("reach-per-pool");
    let c = fresh(&dir, vec![workspace_reaching_epic(), fixture_blueprint(), blueprint_new()]);
    let ms = create_milestone(&c, WS, "m");
    let old_migration = create_bp(&c, BP_OLD, "migration", "old-m", "planned");
    let new_epic = create_bp(&c, BP_NEW, "epic", "new-e", "open");

    // Positive control: the declaration is legal and usable toward the pool
    // that HAS `epic`.
    c.submit(commit(
        &format!(
            r#"MATCH (m: {WS}/milestone {{ id: $m }}) MATCH (e: {BP_NEW}/epic {{ id: $e }})
               CREATE (m) -[depends_on]-> (e)"#
        ),
        &[("m", serde_json::json!(ms.0)), ("e", serde_json::json!(new_epic.0))],
    ))
    .expect("depends_on toward the newer blueprint's epic lands");

    // The finding: toward the OLDER blueprint's `migration` — a type it
    // declares, named by the reach — the edge is refused for a type nobody
    // aimed at.
    let out = c.submit(commit(
        &format!(
            r#"MATCH (m: {WS}/milestone {{ id: $m }}) MATCH (g: {BP_OLD}/migration {{ id: $g }})
               CREATE (m) -[depends_on]-> (g)"#
        ),
        &[("m", serde_json::json!(ms.0)), ("g", serde_json::json!(old_migration.0))],
    ));
    match out {
        Ok(Outcome::Committed { .. }) => {}
        Err(SpineError::Refused { witnesses }) => panic!(
            "depends_on toward `{BP_OLD}/migration` — a declared type, named by the reach — was \
             refused {:?} because the SAME reach also names `epic`, which {BP_OLD} (pinned {}) \
             lacks; the declaration is judged against every type it names rather than the type \
             aimed at",
            witnesses.iter().map(grammar_fault_tag).collect::<Vec<_>>(),
            fixture_blueprint().version
        ),
        other => panic!("unexpected: {other:?}"),
    }
}

// ===========================================================================
// A target whose schema version moved
// ===========================================================================

/// SHOULD HOLD: the far pin a cross-pool edge records on its part is "the
/// far pool's OWN pin" it validated under, so "the row carries forever what
/// it validated against" (the per-part stamp). If the
/// target's pin is not what the cauldron believes it is, the door must
/// either record the pin the pool actually holds or refuse; it must never
/// record a pin the pool no longer has.
///
/// ACTUAL: `Cauldron` loads every grammar once at create/open ("cannot
/// change while a cauldron is open" — an assumption, not a check). The pin
/// is moved underneath it; the door validates against the stale cache,
/// records the stale far pin on the part, and the landing accepts any
/// recorded far pin (`FarSide::Landing` checks presence only). The row now
/// says the edge validated under bp v4 while bp is pinned 9.
#[test]
fn the_recorded_far_pin_is_the_pin_the_far_pool_holds_at_landing() {
    let dir = root("far-pin-moved");
    let c = fresh(&dir, cross_pool_grammars());
    let ms = create_milestone(&c, WS_A, "m");
    let mig = create_bp(&c, BP, "migration", "m1", "planned");

    // The target's pin moves under the open handle (out-of-band today;
    // migration machinery once it exists).
    move_pin(&dir, BP, 9);
    assert_eq!(
        Pool::open(&Pool::path_in(&dir, &PoolName::new(BP))).expect("bp").grammar().version,
        9,
        "the pool file now carries pin 9"
    );

    let env = commit(
        &format!(
            r#"MATCH (m: {WS_A}/milestone {{ id: $m }}) MATCH (g: {BP}/migration {{ id: $g }})
               CREATE (m) -[depends_on]-> (g)"#
        ),
        &[("m", serde_json::json!(ms.0)), ("g", serde_json::json!(mig.0))],
    );
    let hash = env.hash_identity().expect("hash");
    match c.submit(env) {
        Ok(Outcome::Committed { .. }) => {
            let recorded = far_versions_of(&dir, &hash, WS_A);
            assert_eq!(
                recorded.get(&PoolName::new(BP)).copied(),
                Some(9),
                "the row records far pin {:?} for `{BP}` while the pool holds 9: the cauldron's \
                 grammar cache was consulted, never the pool",
                recorded.get(&PoolName::new(BP))
            );
        }
        Err(SpineError::Refused { .. }) => {} // refusing is the other honest answer
        other => panic!("unexpected: {other:?}"),
    }
}

/// SHOULD HOLD: a statement that fails anywhere lands nothing anywhere and
/// returns one refusal — and a refusal means the row
/// is NOT decided (the decided flip is the commit). A part that
/// cannot validate under its pool's pin must refuse BEFORE the decided flip.
///
/// ACTUAL: the door compiles and validates every part against the CACHED
/// grammar (phase 7), flips decided (phase 11), and only then does the
/// landing (`Pool::apply_part`, which re-opens the pool file and reads its
/// real pin) refuse `version_mismatch`. `exec` passes a non-occupancy
/// landing refusal through UNCHANGED, so the caller receives a structured
/// `Refused` — "nothing landed" — while the row is DECIDED with an un-landed
/// part and its locks held to a cleared flip that will never come; recovery
/// rolls it forward into the same refusal every pass. This is the identical
/// shape to the at-most-one wedge already treated as a crate defect
/// (validation after the decided flip), on a different rule.
#[test]
fn a_part_that_cannot_validate_under_the_pools_pin_refuses_whole_and_never_decides() {
    let dir = root("source-pin-moved");
    let c = fresh(&dir, cross_pool_grammars());
    let _warm = create_milestone(&c, WS_A, "warm");

    move_pin(&dir, WS_A, WORKSPACE_PIN + 6);

    let env = commit(
        &format!(r#"CREATE (i: {WS_A}/issue {{ title: "after the pin moved", state: "open" }}) RETURN i.id"#),
        &[],
    );
    let hash = env.hash_identity().expect("hash");
    let out = c.submit(env);

    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let row = spine.find_by_hash(&hash).expect("lookup");
    let recovery = c.recover();
    let row_after = spine.find_by_hash(&hash).expect("lookup");
    match (&out, &row) {
        (Ok(_), _) => {
            // If it landed, the landing validated it under SOME pin; the
            // part's stamp must be the pin the pool holds.
            let stamp = row.as_ref().and_then(|r| r.parts.first()).map(|p| p.schema_version);
            assert_eq!(stamp, Some(WORKSPACE_PIN + 6), "the stamp must be the pool's real pin");
        }
        (Err(_), Some(r)) => {
            assert!(
                !r.decided,
                "the door answered {out:?} — a refusal — yet the row is DECIDED (cleared={}, \
                 parts landed={:?}); recovery then reported {recovery:?} and left the row \
                 decided={:?} cleared={:?}: a wedged transaction behind a `Refused` reply",
                r.cleared,
                r.parts.iter().map(|p| p.landed).collect::<Vec<_>>(),
                row_after.as_ref().map(|r| r.decided),
                row_after.as_ref().map(|r| r.cleared)
            );
        }
        (Err(_), None) => {} // refused before any row: whole, as owed
    }
}

// ===========================================================================
// WHAT HELD — attacked, did not break
// ===========================================================================

/// HELD: REMOVE of a cross-pool edge goes through the same far-side check as
/// CREATE (the compiler records `far_versions` for `RemoveEdge` parts too),
/// so the landing does not refuse it `far_unvalidated`.
#[test]
fn removing_a_cross_pool_edge_lands() {
    let dir = root("remove-cross");
    let c = fresh(&dir, cross_pool_grammars());
    let ms = create_milestone(&c, WS_A, "m");
    let mig = create_bp(&c, BP, "migration", "m1", "planned");
    let params = [("m", serde_json::json!(ms.0)), ("g", serde_json::json!(mig.0))];
    let heads = format!(
        r#"MATCH (m: {WS_A}/milestone {{ id: $m }}) MATCH (g: {BP}/migration {{ id: $g }})"#
    );
    c.submit(commit(&format!("{heads} CREATE (m) -[depends_on]-> (g)"), &params))
        .expect("the cross-pool edge lands");
    c.submit(commit(&format!("{heads} REMOVE (m) -[depends_on]-> (g)"), &params))
        .expect("removing it lands too");
    let pool = Pool::open(&Pool::path_in(&dir, &PoolName::new(WS_A))).expect("ws-a");
    let snap = pool.snapshot().expect("snapshot");
    assert!(snap.edges_from(&ms, Some("depends_on")).expect("edges").is_empty());
}

/// HELD: a reach toward a type the (only) blueprint lacks refuses
/// `foreign_type_unknown` on use, stamped with the SOURCE pool; a reach
/// toward a kind no pool has refuses `reach_undeclared` on use. (Both are in
/// the existing cross_pool suite; re-asserted here as the baseline the
/// per-pool finding above is measured against.)
#[test]
fn a_reach_toward_a_type_no_pool_of_that_kind_declares_refuses_on_use() {
    let dir = root("reach-unknown-type");
    let c = fresh(&dir, vec![workspace_reaching_epic(), fixture_blueprint()]);
    let ms = create_milestone(&c, WS, "m");
    let mig = create_bp(&c, BP_OLD, "migration", "m1", "planned");
    let err = c
        .submit(commit(
            &format!(
                r#"MATCH (m: {WS}/milestone {{ id: $m }}) MATCH (g: {BP_OLD}/migration {{ id: $g }})
                   CREATE (m) -[depends_on]-> (g)"#
            ),
            &[("m", serde_json::json!(ms.0)), ("g", serde_json::json!(mig.0))],
        ))
        .expect_err("with ONE blueprint lacking `epic`, the reach refuses on use");
    match &err {
        SpineError::Refused { witnesses } => {
            assert!(
                witnesses
                    .iter()
                    .any(|w| grammar_fault_tag(w) == Some("foreign_type_unknown")),
                "{witnesses:?}"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

// ===========================================================================
// helpers
// ===========================================================================

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-adv-cross-{name}-{}", std::process::id()))
}

fn fresh(dir: &Path, grammars: Vec<PoolGrammar>) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(dir, &grammars, SpineConfig::default()).expect("create the cauldron")
}

/// The fixture workspace shape with `depends_on` reaching BOTH
/// `{blueprint, migration}` and `{blueprint, epic}` — a declaration written
/// ahead of the blueprints it points at.
fn workspace_reaching_epic() -> PoolGrammar {
    let mut g = fixture_workspace_at(WS, WORKSPACE_PIN);
    g.edge_kinds.insert(
        "depends_on".to_string(),
        EdgeKindDef::reaching(
            "milestone",
            [
                TypeRef::reach("blueprint", "migration"),
                TypeRef::reach("blueprint", "epic"),
            ],
        ),
    );
    g
}

/// A second blueprint of the SAME kind, pinned one ahead, declaring `epic`.
fn blueprint_new() -> PoolGrammar {
    let mut g = fixture_blueprint();
    g.pool = PoolName::new(BP_NEW);
    g.version += 1;
    g.node_types.insert(
        "epic".to_string(),
        NodeTypeDef {
            fields: BTreeMap::from([("name".to_string(), FieldDef::required())]),
            states: vec!["open".to_string(), "done".to_string()],
            count_cells: BTreeMap::new(),
            ..Default::default()
        },
    );
    g
}

/// Move a pool's pinned version by direct SQL on `pool_meta` — the
/// out-of-band stand-in for migration machinery this build does not have.
fn move_pin(dir: &Path, pool: &str, version: u32) {
    let conn = rusqlite::Connection::open(Pool::path_in(dir, &PoolName::new(pool)))
        .expect("open the pool file directly");
    conn.execute(
        "UPDATE pool_meta SET grammar_version = ?1, grammar = json_set(grammar, '$.version', ?1) \
         WHERE singleton = 1",
        rusqlite::params![version as i64],
    )
    .expect("move the pin");
}

fn far_versions_of(dir: &Path, hash: &TxHash, pool: &str) -> BTreeMap<PoolName, u32> {
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let row = spine.find_by_hash(hash).expect("lookup").expect("the row exists");
    row.payload
        .parts
        .iter()
        .find(|p| p.pool == PoolName::new(pool))
        .map(|p| p.far_versions.clone())
        .unwrap_or_default()
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
                thread: "adversary-grammar".into(),
                dispatch: "adversary-grammar".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
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

fn create_milestone(c: &Cauldron, pool: &str, title: &str) -> NodeId {
    committed_id(
        c,
        commit(
            &format!(r#"CREATE (m: {pool}/milestone {{ title: $t, state: "active" }}) RETURN m.id"#),
            &[("t", serde_json::json!(title))],
        ),
        "m.id",
    )
}

fn create_bp(c: &Cauldron, pool: &str, node_type: &str, name: &str, state: &str) -> NodeId {
    committed_id(
        c,
        commit(
            &format!(r#"CREATE (b: {pool}/{node_type} {{ name: $n, state: $s }}) RETURN b.id"#),
            &[("n", serde_json::json!(name)), ("s", serde_json::json!(state))],
        ),
        "b.id",
    )
}

#[allow(dead_code)]
fn only_witness(err: &SpineError) -> Witness {
    match err {
        SpineError::Refused { witnesses } => {
            assert_eq!(witnesses.len(), 1, "one refusal, one witness: {witnesses:?}");
            witnesses[0].clone()
        }
        other => panic!("expected a structured refusal, got {other:?}"),
    }
}

// Conceptual tests — stated, not executable in this build:
//
// - GIVEN a `landed_by` edge aimed at the ghost of a transaction that is
//   still IN FLIGHT (appended, undecided), WHEN committed, THEN it lands
//   (`guard_edge_target` accepts any row); if that transaction later aborts,
//   the pool holds a provenance edge toward a transaction that never landed.
//   Whether an aborted attempt is a legal provenance target is undecided.
// - GIVEN a target pool whose grammar has NO inbound declaration of any
//   kind, WHEN a source declares a reach toward it, THEN the target has no
//   way to refuse being pointed at; "a target's grammar does not permit" is
//   not a concept this grammar can express — reach is outbound-only by
//   design, and this names the consequence rather than asserting it.
// - GIVEN two `Cauldron` handles on one directory and a future migration
//   that moves a pin through one of them, WHEN the other submits, THEN it
//   validates against its stale cache — the two pin-moved tests above are
//   this scenario, provoked by SQL because no in-build operation moves a pin.
