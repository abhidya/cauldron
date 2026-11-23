//! The SEAL act: a statement form through the one door that sets
//! the machine-owned `sealed` flag on named v6 nodes, and the body freeze it
//! buys.
//!
//! The statement (`ladle/mod.rs` grammar, row `seal`):
//!
//! ```text
//! MATCH (a: <pool>/<type> { id: $a }) MATCH (b: <pool>/<type> { id: $b }) SEAL (a) SEAL (b)
//! ```
//!
//! - G1 the flag lands on EXACTLY the named nodes, every v6 type sealable;
//! - G2 a SET into a sealed node's `body` refuses `sealed_body` (door AND
//!   landing), while the same SET on an unsealed node lands;
//! - G3 correction by superseding still lands (a new node, its `supersedes`
//!   edge to the sealed one, and the sealed node's own edges and state);
//! - G4 no write statement sets or clears the flag (`MACHINE_OWNED_FIELDS`);
//! - G5 a SEAL naming an absent node refuses `unknown_node` by name and the
//!   whole statement seals nothing.
//!
//! Every test runs against the REAL v6 grammar (`grammar_v6::workspace_v6`,
//! as declared — serial ids). Every guard was forced RED (`// RED:` above
//! each test).

use spine::config::SpineConfig;
use spine::delta::{CompiledPart, Delta};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance, TxHash};
use spine::error::SpineError;
use spine::grammar_v6;
use spine::ladle::compile::{self, CompileCtx, Fuel};
use spine::ladle::parse;
use spine::locks::{Claim, LockMode, Resource};
use spine::read;
use spine::pool::{NodeId, Pool};
use spine::schema_def::{grammar_fault_tag, FieldDef, FieldKind, PoolGrammar, PoolName};
use spine::witness::Witness;
use spine::{Cauldron, Outcome, ReadRequest};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

test_scratch::install!();

const POOL: &str = "ws-v6";

fn grammar() -> PoolGrammar {
    grammar_v6::workspace_v6(POOL)
}

// ---------------------------------------------------------------------------
// G1 — the flag lands on exactly the named nodes
// ---------------------------------------------------------------------------

/// One statement seals two named nodes out of a pool holding one of every
/// v6 type plus a second `task`; afterwards exactly those two read sealed,
/// each with its whole-node rev bumped by one, every other node untouched.
// RED (measured, F1a/F1b): (a) `emit_seal` made a no-op (no
// delta pushed) — "sealed set after SEAL": left {} right {DEC-1, T-1}.
// (b) Opposed: the landing's `Seal` arm made to seal EVERY node (`WHERE
// sealed = 0`, id ignored) — this test and five others went red.
#[test]
fn g1_seal_sets_the_flag_on_exactly_the_named_nodes() {
    let (c, g) = fresh("g1-exact");
    let mut have = seed_every_type(&c, &g);
    let task2 = mint(&c, &g, "task", &have, &[], None).expect("a second task");
    have.insert("task#2".into(), task2.clone());
    assert_eq!(all_nodes(&c, &g).len(), 13, "one of each of twelve types and a second task");

    let before = all_nodes(&c, &g);
    assert!(before.values().all(|(sealed, _)| !sealed), "nothing starts sealed");
    let named: BTreeSet<NodeId> = [have["task"].clone(), have["decision"].clone()].into();
    seal(&c, &[("task", &have["task"]), ("decision", &have["decision"])])
        .unwrap_or_else(|e| panic!("SEAL of two present nodes must land: {e:?}"));

    let after = all_nodes(&c, &g);
    let sealed: BTreeSet<NodeId> = after.iter().filter(|(_, (s, _))| *s).map(|(id, _)| id.clone()).collect();
    assert_eq!(sealed, named, "sealed set after SEAL");
    for (id, (_, rev)) in &after {
        let want = before[id].1 + i64::from(named.contains(id));
        assert_eq!(*rev, want, "`{id}`: rev bumps by one exactly when sealed");
    }
    assert!(!after[&task2].0, "the unnamed node of the same type stays unsealed");
}

/// Every one of the twelve v6 types can be sealed, one statement each; after
/// each, the sealed set is exactly the nodes sealed so far.
// RED: the same two forcings — (a) "after sealing the `constraint`": left
// {} right {CON-1}; (b) left was all twelve ids at the first type.
#[test]
fn g1_every_v6_type_is_sealable_one_at_a_time() {
    let (c, g) = fresh("g1-each");
    let have = seed_every_type(&c, &g);
    let types: Vec<String> = g.node_types.keys().cloned().collect();
    assert_eq!(types.len(), 12, "the twelve v6 types");
    let mut want: BTreeSet<NodeId> = BTreeSet::new();
    for t in &types {
        seal(&c, &[(t, &have[t])]).unwrap_or_else(|e| panic!("SEAL of a `{t}` must land: {e:?}"));
        want.insert(have[t].clone());
        assert_eq!(sealed_set(&c, &g), want, "after sealing the `{t}`");
    }
}

/// Re-sealing a sealed node is a no-op that COMMITS: no refusal, flag still
/// set, rev unchanged; mixed with an unsealed node in one statement, only
/// the unsealed one changes.
// RED (Fidem): `emit_seal`'s already-sealed skip removed AND the landing's
// `AND sealed = 0` dropped — "re-seal leaves rev and flag": left (true, 3)
// right (true, 2). Either layer alone keeps it green (two nets).
#[test]
fn sealing_a_sealed_node_commits_and_changes_nothing() {
    let (c, g) = fresh("idem");
    let have = seed_every_type(&c, &g);
    let (t, d) = (&have["task"], &have["decision"]);
    seal(&c, &[("task", t)]).expect("first seal");
    let (s1, r1) = sealed_rev(&c, "task", t);
    assert!(s1);
    seal(&c, &[("task", t)]).unwrap_or_else(|e| panic!("a re-seal commits: {e:?}"));
    assert_eq!(sealed_rev(&c, "task", t), (true, r1), "re-seal leaves rev and flag");
    let (_, rd) = sealed_rev(&c, "decision", d);
    seal(&c, &[("task", t), ("decision", d)]).expect("mixed seal");
    assert_eq!(sealed_rev(&c, "task", t), (true, r1), "the sealed one is untouched");
    assert_eq!(sealed_rev(&c, "decision", d), (true, rd + 1), "the unsealed one seals");
}

// ---------------------------------------------------------------------------
// G2 — a sealed body refuses an edit; an unsealed body takes it
// ---------------------------------------------------------------------------

/// For every v6 type that declares `body`: the edit lands while unsealed;
/// after SEAL the same edit refuses `sealed_body` naming the node, and the
/// stored body and rev are unchanged.
// RED: (F2a) the door refusal in `check_settable` AND the landing check in
// `apply_part` both removed — "a sealed `constraint` body must refuse":
// Committed. (F2b) door refusal removed, landing kept — the landing refused
// AFTER the decided flip: "expected a structured refusal, got Hard { SPINE
// INTERNAL INVARIANT VIOLATED ... }". (F2c, opposed) the door refusal made
// unconditional — "an UNSEALED `constraint` body must take the edit"
// refused `sealed_body`. (F2g, opposed) the landing check made
// unconditional — same arm, `Hard`.
#[test]
fn g2_a_sealed_body_refuses_the_edit_and_an_unsealed_body_takes_it() {
    let (c, g) = fresh("g2");
    let have = seed_every_type(&c, &g);
    let mut seen = 0;
    for (t, tdef) in &g.node_types {
        if !tdef.fields.contains_key("body") {
            continue;
        }
        seen += 1;
        let id = &have[t];
        set_body(&c, t, id, &unique("draft body"))
            .unwrap_or_else(|e| panic!("an UNSEALED `{t}` body must take the edit: {e:?}"));
        let landed = body_of(&c, t, id);
        seal(&c, &[(t, id)]).expect("seal");
        let (_, rev) = sealed_rev(&c, t, id);
        let w = refused(set_body(&c, t, id, &unique("late edit")), &format!("a sealed `{t}` body must refuse"));
        assert_sealed_body(&w, id);
        assert_eq!(body_of(&c, t, id), landed, "`{t}`: the stored body is unchanged");
        assert_eq!(sealed_rev(&c, t, id), (true, rev), "`{t}`: nothing landed");
    }
    assert_eq!(seen, 10, "the ten content types declare `body`; region and viewer do not");
}

/// SEAL then SET body in ONE statement refuses (the landing would see the
/// node sealed by the time it reached the SET) — and, the refusal being
/// whole, the node is NOT sealed afterwards. SET body then SEAL lands both:
/// the draft's last edit and its seal, atomically.
// RED (F2d): `sealed_at_this_point`'s scan of this part's earlier `Seal`
// deltas removed — the door passed it and the landing refused after the
// decided flip: "expected a structured refusal, got Hard". (F2c, opposed)
// the door refusal unconditional — "SET body then SEAL lands" refused.
#[test]
fn g2_seal_then_edit_in_one_statement_refuses_whole_and_edit_then_seal_lands() {
    let (c, g) = fresh("g2-one");
    let have = seed_every_type(&c, &g);
    let t = &have["task"];
    let rev = sealed_rev(&c, "task", t).1;
    let w = refused(
        c.submit(commit(
            &format!("MATCH (a: {POOL}/task {{ id: $a }}) SEAL (a) SET a.body = $b"),
            &[("a", json(&t.0)), ("b", json(unique("edit")))],
        )),
        "SEAL then SET body",
    );
    assert_sealed_body(&w, t);
    assert_eq!(sealed_rev(&c, "task", t), (false, rev), "the whole statement refused: not sealed");

    let last = unique("last edit");
    c.submit(commit(
        &format!("MATCH (a: {POOL}/task {{ id: $a }}) SET a.body = $b SEAL (a)"),
        &[("a", json(&t.0)), ("b", json(&last))],
    ))
    .unwrap_or_else(|e| panic!("SET body then SEAL lands: {e:?}"));
    assert_eq!(body_of(&c, "task", t), json(&last));
    assert_eq!(sealed_rev(&c, "task", t), (true, rev + 2), "one bump for the SET, one for the SEAL");
}

/// What a seal does NOT freeze: the sealed node's handle, `meta` and
/// state stay writable (only `body` is frozen).
// RED (F2e, opposed to G2): the door's body test widened to every field
// (`true || name == SEALED_BODY_FIELD`) — "a sealed node's `handle` stays
// writable" refused `sealed_body`.
#[test]
fn g2_a_sealed_node_keeps_its_other_fields_and_state_writable() {
    let (c, g) = fresh("g2-other");
    let have = seed_every_type(&c, &g);
    let t = &have["task"];
    seal(&c, &[("task", t)]).expect("seal");
    let q = format!("MATCH (a: {POOL}/task {{ id: $a }}) ");
    for (what, stmt, v) in [
        ("handle", format!("{q}SET a.handle = $v"), json(unique("a new handle"))),
        ("meta", format!("{q}SET a.meta = $v"), serde_json::json!({ "k": unique("v") })),
        ("state", format!("{q}SET a.state = $v"), json(g.node_types["task"].states.last().unwrap())),
    ] {
        c.submit(commit(&stmt, &[("a", json(&t.0)), ("v", v)]))
            .unwrap_or_else(|e| panic!("a sealed node's `{what}` stays writable: {e:?}"));
    }
    assert!(sealed_rev(&c, "task", t).0, "still sealed");
}

/// The landing's own net: a hand-built SetField into a sealed body, applied
/// straight to the pool file (the test-support side door), refuses
/// `sealed_body`; the same part on an unsealed node lands.
// RED: (F2f) the landing check in `apply_part` removed — "a hand-built
// body SET on a sealed node must refuse at the landing": `Landed {..}`.
// (F2g, opposed) the landing check made unconditional (`sealed == Some(1)
// || true`) — "the unsealed node's body lands" refused `sealed_body`.
#[test]
fn g2_the_landing_refuses_a_hand_built_body_set_on_a_sealed_node() {
    let (c, g) = fresh("g2-landing");
    let have = seed_every_type(&c, &g);
    let (sealed, open) = (have["task"].clone(), have["decision"].clone());
    seal(&c, &[("task", &sealed)]).expect("seal");
    let dir = scratch_path("g2-landing");
    drop(c);
    let mut pool = Pool::open(&Pool::path_in(&dir, &PoolName::new(POOL))).expect("open the pool");
    let part = |node: &NodeId| CompiledPart {
        pool: PoolName::new(POOL),
        schema_version: g.version,
        deltas: vec![Delta::SetField { node: node.clone(), path: vec!["body".into()], value: json("by hand") }],
        premises: vec![],
        far_versions: BTreeMap::new(),
    };
    let err = match pool.apply_part(&TxHash([0x51; 32]), 0, &part(&sealed)) {
        Ok(l) => panic!("a hand-built body SET on a sealed node must refuse at the landing: {l:?}"),
        Err(e) => e,
    };
    assert_sealed_body(&only_witness(&err), &sealed);
    pool.apply_part(&TxHash([0x52; 32]), 0, &part(&open))
        .unwrap_or_else(|e| panic!("the unsealed node's body lands: {e:?}"));
}

// ---------------------------------------------------------------------------
// G3 — correction by superseding still lands
// ---------------------------------------------------------------------------

/// A sealed `task` is corrected by minting a successor carrying a
/// `supersedes` edge to it; the edge reads back; and the sealed node itself
/// still takes new edges (a second `in_region`) and a state change.
// RED (F3): every write touching a sealed node refused — the door's sealed
// test applied to every SET field, and the landing refusing any `AddEdge`
// whose src OR dst is sealed — "a successor superseding the sealed task
// must land": Hard (refused at the landing).
#[test]
fn g3_superseding_a_sealed_node_lands() {
    let (c, g) = fresh("g3");
    let mut have = seed_every_type(&c, &g);
    let old = have["task"].clone();
    seal(&c, &[("task", &old)]).expect("seal");

    let region = have["region"].clone();
    let new = c
        .submit(commit(
            &format!(
                "MATCH (r: {POOL}/region {{ id: $r }}) MATCH (o: {POOL}/task {{ id: $o }}) \
                 CREATE (n: {POOL}/task {{ handle: $h, state: $s }}) \
                 CREATE (n) -[in_region]-> (r) CREATE (n) -[supersedes]-> (o) RETURN n.id"
            ),
            &[
                ("r", json(&region.0)),
                ("o", json(&old.0)),
                ("h", json(unique("the corrected task"))),
                ("s", json(&g.node_types["task"].states[0])),
            ],
        ))
        .map(|o| returned_id(o, "n.id"))
        .unwrap_or_else(|e| panic!("a successor superseding the sealed task must land: {e:?}"));
    let rows = read(
        &c,
        &format!("MATCH (n: {POOL}/task {{ id: $n }}) -[supersedes]-> (o: {POOL}/task) RETURN o.id"),
        &[("n", json(&new.0))],
    );
    assert_eq!(rows.len(), 1, "one supersedes edge");
    assert_eq!(rows[0]["o.id"], json(&old.0), "it points at the sealed node");

    // The sealed node's own edges and state stay writable.
    let r2 = mint(&c, &g, "region", &have, &[], None).expect("a second region");
    have.insert("region#2".into(), r2.clone());
    c.submit(commit(
        &format!("MATCH (o: {POOL}/task {{ id: $o }}) MATCH (r: {POOL}/region {{ id: $r }}) CREATE (o) -[in_region]-> (r)"),
        &[("o", json(&old.0)), ("r", json(&r2.0))],
    ))
    .unwrap_or_else(|e| panic!("an edge out of the sealed node lands: {e:?}"));
    assert!(sealed_rev(&c, "task", &old).0, "still sealed");
    assert!(!sealed_rev(&c, "task", &new).0, "the successor is a fresh draft");
}

// ---------------------------------------------------------------------------
// G4 — no write statement sets or clears the flag
// ---------------------------------------------------------------------------

/// SET can neither clear the flag on a sealed node nor set it on an unsealed
/// one; CREATE cannot mint a node sealed; and no grammar may declare a field
/// named `sealed`. Each refusal is `machine_owned_field` (the grammar one is
/// the pin's own validation), and the flag reads unchanged.
// RED (F4): `"sealed"` dropped from `MACHINE_OWNED_FIELDS` — "SET a.sealed
// = false": left Some("undeclared_field") right Some("machine_owned_field").
// The SET still refused (the flag is a column, not a field): what the list
// buys is the NAME — see the grammar test below.
#[test]
fn g4_no_write_statement_sets_or_clears_the_flag() {
    let (c, g) = fresh("g4");
    let have = seed_every_type(&c, &g);
    let (s, u) = (&have["task"], &have["decision"]);
    seal(&c, &[("task", s)]).expect("seal");
    for (t, id, v, was) in [("task", s, false, true), ("decision", u, true, false)] {
        let w = refused(
            c.submit(commit(
                &format!("MATCH (a: {POOL}/{t} {{ id: $a }}) SET a.sealed = $v"),
                &[("a", json(&id.0)), ("v", json(v))],
            )),
            &format!("SET a.sealed = {v}"),
        );
        assert_eq!(grammar_fault_tag(&w), Some("machine_owned_field"), "SET a.sealed = {v}: {w:?}");
        assert_eq!(sealed_rev(&c, t, id).0, was, "SET a.sealed = {v} changed nothing");
    }
    let region = &have["region"];
    let w = refused(
        c.submit(commit(
            &format!(
                "MATCH (r: {POOL}/region {{ id: $r }}) CREATE (n: {POOL}/task {{ handle: $h, state: $s, sealed: true }}) \
                 CREATE (n) -[in_region]-> (r)"
            ),
            &[("r", json(&region.0)), ("h", json(unique("born sealed"))), ("s", json(&g.node_types["task"].states[0]))],
        )),
        "CREATE with sealed: true",
    );
    assert_eq!(grammar_fault_tag(&w), Some("machine_owned_field"), "CREATE sealed: {w:?}");
}

/// The flag is a COLUMN (`nodes.sealed`), so a SET could only ever reach a
/// grammar-declared FIELD of the same name — a shadow that `RETURN n.sealed`
/// would never read and every render would show. No grammar may declare one:
/// the pin refuses it, while the same grammar without it pins.
// RED (F4): `"sealed"` dropped from `MACHINE_OWNED_FIELDS` — "a grammar
// declaring `sealed` must not pin": it pinned.
#[test]
fn g4_no_grammar_may_declare_a_field_named_sealed() {
    assert!(
        Cauldron::create(&scratch("g4-grammar-ok"), &[grammar()], SpineConfig::default()).is_ok(),
        "the v6 grammar as declared pins"
    );
    let mut bad = grammar();
    bad.node_types.get_mut("task").unwrap().fields.insert(
        "sealed".into(),
        FieldDef { kind: FieldKind::Boolean, ..FieldDef::optional() },
    );
    let dir = scratch("g4-grammar");
    assert!(
        Cauldron::create(&dir, &[bad], SpineConfig::default()).is_err(),
        "a grammar declaring `sealed` must not pin"
    );
}

// ---------------------------------------------------------------------------
// G5 — an absent node refuses by name and nothing is sealed
// ---------------------------------------------------------------------------

/// SEAL naming one present and one absent node refuses `unknown_node`,
/// naming the absent identifier, in either clause order; the present node
/// is NOT sealed and its rev has not moved. An absent node alone refuses the
/// same way.
// RED (F5a): `check_seal_targets` removed (the absent binding then expands
// to no delta, the empty-match rule) — "SEAL(present) SEAL(absent) must
// refuse": Committed. (F5b, opposed) every SEAL refused (`bound == 0 ||
// true`) — ten of eleven tests red, e.g. "SEAL of a `constraint` must
// land": unknown_node.
#[test]
fn g5_sealing_an_absent_node_refuses_by_name_and_seals_nothing() {
    let (c, g) = fresh("g5");
    let have = seed_every_type(&c, &g);
    let t = &have["task"];
    let absent = NodeId("T-999".into());
    let rev = sealed_rev(&c, "task", t).1;
    for (label, pairs) in [
        ("SEAL(present) SEAL(absent)", vec![("task", t), ("task", &absent)]),
        ("SEAL(absent) SEAL(present)", vec![("task", &absent), ("task", t)]),
        ("SEAL(absent)", vec![("task", &absent)]),
    ] {
        let w = refused(seal(&c, &pairs), &format!("{label} must refuse"));
        assert_eq!(grammar_fault_tag(&w), Some("unknown_node"), "{label}: {w:?}");
        match &w {
            Witness::GrammarViolation { pool, message, .. } => {
                assert_eq!(pool, &PoolName::new(POOL));
                assert!(message.contains("T-999"), "{label}: the absent id by name: {message}");
            }
            other => panic!("{label}: expected GrammarViolation, got {other:?}"),
        }
        assert_eq!(sealed_rev(&c, "task", t), (false, rev), "{label}: nothing sealed");
    }
    assert!(sealed_set(&c, &g).is_empty(), "no node anywhere was sealed");
}

// ---------------------------------------------------------------------------
// a pool file from before the flag
// ---------------------------------------------------------------------------

/// A pool file written before the seal act has no `nodes.sealed` column. Opening it
/// adds the column (`DEFAULT 0`): its nodes read unsealed, and SEAL works.
/// The fixture is rebuilt from the pool's OWN `nodes` DDL with the `sealed`
/// line cut, and asserted column-less before the open (the sentinel that the
/// fixture really is the old shape).
// RED (F7): the `upgrade_sealed_column` call in `Pool::open` removed — the
// read after the re-open failed: `Hard { "storage failure: no such column:
// sealed in SELECT ... FROM nodes ..." }`.
#[test]
fn a_pool_file_from_before_the_flag_opens_unsealed_and_seals() {
    let (c, g) = fresh("upgrade");
    let have = seed_every_type(&c, &g);
    let t = have["task"].clone();
    drop(c);
    let dir = scratch_path("upgrade");
    let has_column = |conn: &rusqlite::Connection| -> bool {
        conn.query_row("SELECT count(*) FROM pragma_table_info('nodes') WHERE name = 'sealed'", [], |r| {
            r.get::<_, i64>(0)
        })
        .expect("table_info")
            == 1
    };
    {
        let conn = rusqlite::Connection::open(Pool::path_in(&dir, &PoolName::new(POOL))).expect("pool");
        assert!(has_column(&conn), "a fresh pool has the column");
        let ddl: String = conn
            .query_row("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'nodes'", [], |r| r.get(0))
            .expect("the nodes DDL");
        let cut = ddl.find(",\n  sealed").unwrap_or_else(|| panic!("the DDL names `sealed`: {ddl}"));
        let old = format!("{}\n)", &ddl[..cut]).replacen("CREATE TABLE nodes", "CREATE TABLE nodes_v0", 1);
        conn.execute_batch(&format!(
            "PRAGMA foreign_keys = OFF; BEGIN; {old};
             INSERT INTO nodes_v0 SELECT id, node_type, state, rev, schema_version, created_tx, updated_tx, created, updated FROM nodes;
             DROP TABLE nodes; ALTER TABLE nodes_v0 RENAME TO nodes;
             CREATE INDEX idx_nodes_type_state ON nodes(node_type, state); COMMIT;"
        ))
        .expect("rewrite the pool as it was before the sealed column");
        assert!(!has_column(&conn), "the fixture is the old shape");
    }
    let c = Cauldron::open(&dir, SpineConfig::default()).expect("re-open the old pool");
    assert_eq!(sealed_rev(&c, "task", &t).0, false, "an old node reads unsealed");
    seal(&c, &[("task", &t)]).unwrap_or_else(|e| panic!("SEAL on an upgraded pool: {e:?}"));
    assert!(sealed_rev(&c, "task", &t).0, "sealed");
    drop(c);
    let conn = rusqlite::Connection::open(Pool::path_in(&dir, &PoolName::new(POOL))).expect("pool");
    assert!(has_column(&conn), "the open added the column");
}

// ---------------------------------------------------------------------------
// the statement's shape
// ---------------------------------------------------------------------------

/// SEAL names nodes MATCHed by identifier: a minted binding, a population
/// MATCH (no `id`) and an inline head all refuse before anything lands.
// RED (F6): the `head.field(FIELD_ID)` requirement skipped in
// `check_seal_targets` — "a population MATCH: SEAL must refuse": Committed.
// (F5a) the whole check removed — "a minted binding": Committed.
#[test]
fn seal_takes_only_a_node_matched_by_identifier() {
    let (c, g) = fresh("shape");
    let have = seed_every_type(&c, &g);
    let region = &have["region"];
    for (label, stmt, params) in [
        (
            "a minted binding",
            format!(
                "MATCH (r: {POOL}/region {{ id: $r }}) CREATE (n: {POOL}/task {{ handle: $h, state: $s }}) \
                 CREATE (n) -[in_region]-> (r) SEAL (n)"
            ),
            vec![("r", json(&region.0)), ("h", json(unique("minted"))), ("s", json(&g.node_types["task"].states[0]))],
        ),
        ("a population MATCH", format!("MATCH (a: {POOL}/task) SEAL (a)"), vec![]),
        ("an inline head", format!("SEAL (a: {POOL}/task {{ id: $a }})"), vec![("a", json(&have["task"].0))]),
    ] {
        let params: Vec<(&str, serde_json::Value)> = params.into_iter().collect();
        match c.submit(commit(&stmt, &params)) {
            Ok(o) => panic!("{label}: SEAL must refuse, got {o:?}"),
            Err(e) => assert!(format!("{e:?}").contains("SEAL"), "{label}: names SEAL: {e:?}"),
        }
    }
    assert!(sealed_set(&c, &g).is_empty(), "nothing sealed");
}

// ---------------------------------------------------------------------------
// the seal act's side paths
// ---------------------------------------------------------------------------

/// G2a: a compiled SEAL claims Body WRITE on EACH node it seals — the lock a
/// body SET takes, so the two serialize on the node (`ladle::compile::
/// claims_for`, the `Delta::Seal` arm). The claim set is derived through the
/// crate's own matcher, compiler and claim derivation, as the door derives
/// it; one mode per resource (the builder merges READ into WRITE). Opposed:
/// a re-SEAL of an already sealed node compiles to no delta and its body
/// claim is the match's READ only — so the WRITE tracks the seal delta, not
/// the statement's shape.
// RED (G2a): `| Delta::Seal { node }` dropped from the
// SetField arm of `claims_for` (Seal then claims nothing; the match's READ
// stands) — "SEAL claims Body WRITE on `T-1`": left Some(Read) right
// Some(Write).
#[test]
fn g2a_a_compiled_seal_claims_body_write_on_each_sealed_node() {
    let (c, g) = fresh("g2a");
    let have = seed_every_type(&c, &g);
    let (t, d) = (have["task"].clone(), have["decision"].clone());
    let claims = seal_claims(&c, &[("task", &t), ("decision", &d)]);
    for id in [&t, &d] {
        assert_eq!(body_mode(&claims, id), Some(LockMode::Write), "SEAL claims Body WRITE on `{id}`: {claims:?}");
    }
    // Opposed: once `t` is sealed, sealing it again (with `d` still open)
    // claims WRITE on `d` only; `t` keeps the match's READ.
    seal(&c, &[("task", &t)]).expect("seal the task");
    let claims = seal_claims(&c, &[("task", &t), ("decision", &d)]);
    assert_eq!(body_mode(&claims, &t), Some(LockMode::Read), "a re-seal claims only READ on `{t}`: {claims:?}");
    assert_eq!(body_mode(&claims, &d), Some(LockMode::Write), "the unsealed `{d}` still takes the WRITE");
}

/// G3: the LANDING's own no-op for a SEAL of an already sealed node
/// (`pool.rs`, the `Delta::Seal` arm's `AND sealed = 0`), alone — a
/// hand-built Seal part applied straight to the pool file (the test-support
/// side door), so `emit_seal`'s skip never runs. Observable: the node's
/// `rev` and `updated_tx`, read off the pool file, do not move. Opposed:
/// the same hand-built part on an UNSEALED node seals it, bumps `rev` by
/// one, and stamps `updated_tx` with the part's own transaction.
// RED (G3): `AND sealed = 0` removed from the landing's
// UPDATE (nothing else touched) — "a hand-built re-seal is a no-op at the
// landing": left (1, 3, "6161…") right (1, 2, <the door SEAL's tx>).
// (Opposed forcing tried: the landing made to never seal, `AND sealed = 0
// AND 0` — this test went red at its fixture sentinel "the fixture node is
// sealed" (left 0 right 1), with ten others; the opposed half is green at
// HEAD, which is what shows the observable CAN move.)
#[test]
fn g3_a_hand_built_reseal_is_a_noop_at_the_landing() {
    let (c, g) = fresh("g3-reseal");
    let have = seed_every_type(&c, &g);
    let (sealed, open) = (have["task"].clone(), have["decision"].clone());
    seal(&c, &[("task", &sealed)]).expect("seal");
    drop(c);
    let file = Pool::path_in(&scratch_path("g3-reseal"), &PoolName::new(POOL));
    let row = |id: &NodeId| -> (i64, i64, String) {
        rusqlite::Connection::open(&file)
            .expect("pool")
            .query_row("SELECT sealed, rev, updated_tx FROM nodes WHERE id = ?1", [&id.0], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .expect("the node row")
    };
    let part = |node: &NodeId| CompiledPart {
        pool: PoolName::new(POOL),
        schema_version: g.version,
        deltas: vec![Delta::Seal { node: node.clone() }],
        premises: vec![],
        far_versions: BTreeMap::new(),
    };
    let before = row(&sealed);
    assert_eq!(before.0, 1, "the fixture node is sealed");
    let mut pool = Pool::open(&file).expect("open the pool");
    pool.apply_part(&TxHash([0x61; 32]), 0, &part(&sealed))
        .unwrap_or_else(|e| panic!("a hand-built re-seal lands (as a no-op): {e:?}"));
    assert_eq!(row(&sealed), before, "a hand-built re-seal is a no-op at the landing");

    // Opposed: the same part on an unsealed node changes every observable.
    let (s0, r0, _) = row(&open);
    assert_eq!(s0, 0, "the opposed node starts unsealed");
    pool.apply_part(&TxHash([0x62; 32]), 0, &part(&open))
        .unwrap_or_else(|e| panic!("a hand-built seal of an unsealed node lands: {e:?}"));
    let (s1, r1, u1) = row(&open);
    assert_eq!((s1, r1), (1, r0 + 1), "the unsealed node seals, rev + 1");
    assert_eq!(u1, "62".repeat(32), "its updated_tx is the hand-built part's transaction");
}

/// G4 per field: a SET on each machine-owned header field — `rev`,
/// `schema_version`, `created_tx`, `updated_tx`, and `sealed` — refuses
/// `machine_owned_field` (the `MACHINE_OWNED_FIELDS` arm in
/// `check_settable`; before it, the four header fields refused
/// `undeclared_field`), and the node's rev does not move. Opposed: a SET on
/// a name that is neither declared nor machine-owned refuses
/// `undeclared_field`, and a declared field lands — so the tag is not a
/// constant.
// RED (G4): the `MACHINE_OWNED_FIELDS` arm at the head of
// `check_settable`'s match removed — every one of the five names came back
// Some("undeclared_field").
#[test]
fn g4_set_on_each_machine_owned_header_field_refuses_machine_owned_field() {
    let (c, g) = fresh("g4-each");
    let have = seed_every_type(&c, &g);
    let t = &have["task"];
    let rev = sealed_rev(&c, "task", t).1;
    // Every field's tag is collected first and compared as one map, so a
    // red run names the tag of EACH field, not only the first.
    let mut got: BTreeMap<&str, Option<&str>> = BTreeMap::new();
    for (field, v) in [
        ("rev", json(99)),
        ("schema_version", json(99)),
        ("created_tx", json("00".repeat(32))),
        ("updated_tx", json("00".repeat(32))),
        ("sealed", json(true)),
    ] {
        let w = refused(
            c.submit(commit(&format!("MATCH (a: {POOL}/task {{ id: $a }}) SET a.{field} = $v"), &[("a", json(&t.0)), ("v", v)])),
            &format!("SET a.{field}"),
        );
        got.insert(field, grammar_fault_tag(&w));
        assert_eq!(sealed_rev(&c, "task", t), (false, rev), "SET a.{field} changed nothing");
    }
    let want: BTreeMap<&str, Option<&str>> = ["rev", "schema_version", "created_tx", "updated_tx", "sealed"]
        .into_iter()
        .map(|f| (f, Some("machine_owned_field")))
        .collect();
    assert_eq!(got, want, "the refusal tag of SET on each machine-owned header field");
    let w = refused(
        c.submit(commit(&format!("MATCH (a: {POOL}/task {{ id: $a }}) SET a.no_such_field = 1"), &[("a", json(&t.0))])),
        "SET a.no_such_field",
    );
    assert_eq!(grammar_fault_tag(&w), Some("undeclared_field"), "an undeclared, non-machine name: {w:?}");
    c.submit(commit(&format!("MATCH (a: {POOL}/task {{ id: $a }}) SET a.handle = $v"), &[("a", json(&t.0)), ("v", json(unique("h")))]))
        .unwrap_or_else(|e| panic!("a declared field lands: {e:?}"));
}

/// The one mode `claims` holds on `id`'s body (None: no claim at all).
fn body_mode(claims: &BTreeSet<Claim>, id: &NodeId) -> Option<LockMode> {
    let want = Resource::Body { node: id.clone() };
    let modes: Vec<LockMode> =
        claims.iter().filter(|c| c.pool == PoolName::new(POOL) && c.resource == want).map(|c| c.mode).collect();
    assert!(modes.len() <= 1, "one mode per resource: {modes:?}");
    modes.first().copied()
}

/// The claim set the DOOR would acquire for a SEAL of `nodes`, derived
/// through the crate's own matcher, compiler and claim derivation (the
/// `lock_behaviour.rs` / `empty_match.rs` idiom).
fn seal_claims(c: &Cauldron, nodes: &[(&str, &NodeId)]) -> BTreeSet<Claim> {
    let mut stmt = String::new();
    let mut b = Bindings::new();
    for (i, (t, id)) in nodes.iter().enumerate() {
        stmt.push_str(&format!("MATCH (s{i}: {POOL}/{t} {{ id: $s{i} }}) "));
        b.insert(format!("s{i}"), json(&id.0));
    }
    for i in 0..nodes.len() {
        stmt.push_str(&format!("SEAL (s{i}) "));
    }
    let lookup = |p: &PoolName| -> Option<PoolGrammar> { c.grammar(p).cloned() };
    let parsed = parse::parse(stmt.trim_end()).expect("parse");
    let pools: Vec<PoolName> = compile::footprint(&parsed).pools.iter().cloned().collect();
    let mut fuel = Fuel::new(c.config().fuel_ceiling);
    let matched = {
        let open = read::open_pools(c, &pools).expect("open pools");
        let snaps = read::open_snapshots(&open).expect("snapshots");
        read::run_match(&snaps, &parsed, &b, &mut fuel).expect("match")
    };
    let hash = TxHash([0x77; 32]);
    let payload = {
        let kinds = |k: &spine::schema_def::PoolKind| -> Vec<PoolGrammar> {
            c.pools_of_kind(k).into_iter().filter_map(|n| c.grammar(n).cloned()).collect()
        };
        let mut ctx = CompileCtx { grammars: &lookup, pools_of_kind: &kinds, fuel: &mut fuel };
        compile::compile(&parsed, &b, &matched, &hash, &mut ctx).expect("compile")
    };
    compile::claims_for(&parsed, &matched, &payload, &lookup).expect("claims").0
}

// ---------------------------------------------------------------------------
// helpers (the v6 mint helpers follow `grammar_v6_data_door.rs`)
// ---------------------------------------------------------------------------

#[track_caller]
fn assert_sealed_body(w: &Witness, node: &NodeId) {
    assert_eq!(grammar_fault_tag(w), Some("sealed_body"), "the frozen tag: {w:?}");
    match w {
        Witness::GrammarViolation { pool, message, .. } => {
            assert_eq!(pool, &PoolName::new(POOL), "{w:?}");
            assert!(message.contains(&format!("{POOL}/{node}")), "names the node: {message}");
            assert!(message.contains("supersed"), "names the repair: {message}");
        }
        other => panic!("expected GrammarViolation, got {other:?}"),
    }
}

/// One statement: a MATCH by identifier and a SEAL per named node.
fn seal(c: &Cauldron, nodes: &[(&str, &NodeId)]) -> spine::error::Result<Outcome> {
    let mut stmt = String::new();
    let mut params = Vec::new();
    for (i, (t, id)) in nodes.iter().enumerate() {
        stmt.push_str(&format!("MATCH (s{i}: {POOL}/{t} {{ id: $s{i} }}) "));
        params.push((format!("s{i}"), json(&id.0)));
    }
    for i in 0..nodes.len() {
        stmt.push_str(&format!("SEAL (s{i}) "));
    }
    let params: Vec<(&str, serde_json::Value)> = params.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    c.submit(commit(stmt.trim_end(), &params))
}

fn set_body(c: &Cauldron, t: &str, id: &NodeId, body: &str) -> spine::error::Result<Outcome> {
    c.submit(commit(
        &format!("MATCH (a: {POOL}/{t} {{ id: $a }}) SET a.body = $b"),
        &[("a", json(&id.0)), ("b", json(body))],
    ))
}

fn body_of(c: &Cauldron, t: &str, id: &NodeId) -> serde_json::Value {
    let rows = read(c, &format!("MATCH (n: {POOL}/{t} {{ id: $id }}) RETURN n.body AS b"), &[("id", json(&id.0))]);
    assert_eq!(rows.len(), 1, "`{t}` {id} reads back");
    rows[0]["b"].clone()
}

/// The flag and the whole-node rev, read through the read door.
fn sealed_rev(c: &Cauldron, t: &str, id: &NodeId) -> (bool, i64) {
    let rows = read(
        c,
        &format!("MATCH (n: {POOL}/{t} {{ id: $id }}) RETURN n.sealed AS s, REV(n) AS r"),
        &[("id", json(&id.0))],
    );
    assert_eq!(rows.len(), 1, "`{t}` {id} reads back");
    (
        rows[0]["s"].as_bool().unwrap_or_else(|| panic!("`sealed` is a boolean: {:?}", rows[0])),
        rows[0]["r"].as_i64().unwrap_or_else(|| panic!("rev is an integer: {:?}", rows[0])),
    )
}

/// Every node in the pool, by type, with its flag and rev.
fn all_nodes(c: &Cauldron, g: &PoolGrammar) -> BTreeMap<NodeId, (bool, i64)> {
    let mut out = BTreeMap::new();
    for t in g.node_types.keys() {
        for r in read(c, &format!("MATCH (n: {POOL}/{t}) RETURN n.id AS id, n.sealed AS s, REV(n) AS r"), &[]) {
            out.insert(
                NodeId(r["id"].as_str().unwrap().to_string()),
                (r["s"].as_bool().expect("sealed is a boolean"), r["r"].as_i64().expect("rev")),
            );
        }
    }
    out
}

fn sealed_set(c: &Cauldron, g: &PoolGrammar) -> BTreeSet<NodeId> {
    all_nodes(c, g).into_iter().filter(|(_, (s, _))| *s).map(|(id, _)| id).collect()
}

fn scratch_path(tag: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-seal-{tag}-{}", std::process::id()))
}

fn scratch(tag: &str) -> PathBuf {
    let dir = scratch_path(tag);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn fresh(tag: &str) -> (Cauldron, PoolGrammar) {
    let g = grammar();
    let c = Cauldron::create(&scratch(tag), &[g.clone()], SpineConfig::default()).expect("create the cauldron");
    (c, g)
}

fn json(v: impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(v).expect("json")
}

fn unique(what: &str) -> String {
    static SERIAL: AtomicU32 = AtomicU32::new(0);
    format!("{what} {:06}", SERIAL.fetch_add(1, Ordering::SeqCst))
}

fn filler(n: u64) -> String {
    let n = n as usize;
    let mut s = unique("x");
    s.truncate(n);
    while s.chars().count() < n {
        s.push('y');
    }
    s
}

fn valid_fields(g: &PoolGrammar, t: &str) -> BTreeMap<String, serde_json::Value> {
    let mut out = BTreeMap::new();
    for (f, fdef) in &g.node_types[t].fields {
        if !fdef.required {
            continue;
        }
        let v = match fdef.kind {
            FieldKind::Text => {
                let floor = fdef.min_chars.unwrap_or(1).max(1);
                let cap = fdef.max_chars.unwrap_or(u64::MAX);
                json(filler(floor.max(20).min(cap)))
            }
            FieldKind::Object => {
                serde_json::json!({ "thread": "00000000-0000-4000-8000-000000000001", "msg": 1 })
            }
            FieldKind::Integer => json(1),
            FieldKind::Boolean => json(true),
            FieldKind::Any => json("any"),
        };
        out.insert(f.clone(), v);
    }
    out
}

fn targets_of(g: &PoolGrammar, kind: &str, src: &str) -> Vec<String> {
    let kdef = &g.edge_kinds[kind];
    g.node_types.keys().filter(|t| kdef.permits(&g.kind, t, true, src)).cloned().collect()
}

fn required_for_mint(g: &PoolGrammar, t: &str) -> BTreeMap<String, u32> {
    let tdef = &g.node_types[t];
    let mut m = tdef.required_edges_at_mint.clone();
    if let Some(first) = tdef.required_at_mint_any_of.first() {
        m.extend(first.edges.clone());
    }
    m
}

fn targets_ready(g: &PoolGrammar, t: &str, have: &BTreeMap<String, NodeId>) -> bool {
    required_for_mint(g, t).keys().all(|k| targets_of(g, k, t).iter().any(|d| have.contains_key(d)))
}

fn mint(
    c: &Cauldron,
    g: &PoolGrammar,
    t: &str,
    have: &BTreeMap<String, NodeId>,
    overrides: &[(&str, serde_json::Value)],
    omit: Option<&String>,
) -> spine::error::Result<NodeId> {
    let mut fields = valid_fields(g, t);
    if g.node_types[t].fields.contains_key("handle") {
        fields.insert("handle".into(), json(unique(t)));
    }
    for (f, v) in overrides {
        fields.insert((*f).to_string(), v.clone());
    }
    let mut params: Vec<(String, serde_json::Value)> = Vec::new();
    let mut props: Vec<String> = Vec::new();
    for (i, (f, v)) in fields.iter().enumerate() {
        props.push(format!("{f}: $f{i}"));
        params.push((format!("f{i}"), v.clone()));
    }
    if let Some(s) = g.node_types[t].states.first() {
        props.push(format!("state: \"{s}\""));
    }
    let mut matches = String::new();
    let mut edges = String::new();
    for (j, (k, _)) in required_for_mint(g, t).iter().enumerate() {
        if Some(k) == omit {
            continue;
        }
        let dt = targets_of(g, k, t)
            .into_iter()
            .find(|d| have.contains_key(d))
            .unwrap_or_else(|| panic!("no minted target for `{t}` -{k}->"));
        matches.push_str(&format!("MATCH (t{j}: {POOL}/{dt} {{ id: $t{j} }}) "));
        params.push((format!("t{j}"), json(&have[&dt].0)));
        edges.push_str(&format!("CREATE (n) -[{k}]-> (t{j}) "));
    }
    let stmt = format!("{matches}CREATE (n: {POOL}/{t} {{ {} }}) {edges}RETURN n.id", props.join(", "));
    let params: Vec<(&str, serde_json::Value)> = params.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    c.submit(commit(&stmt, &params)).map(|o| returned_id(o, "n.id"))
}

/// One node of every type (dependency order), plus nothing else.
fn seed_every_type(c: &Cauldron, g: &PoolGrammar) -> BTreeMap<String, NodeId> {
    let types: Vec<String> = g.node_types.keys().cloned().collect();
    let mut have: BTreeMap<String, NodeId> = BTreeMap::new();
    while have.len() < types.len() {
        let before = have.len();
        for t in &types {
            if have.contains_key(t) || !targets_ready(g, t, &have) {
                continue;
            }
            let id = mint(c, g, t, &have, &[], None).unwrap_or_else(|e| panic!("seed `{t}`: {e:?}"));
            have.insert(t.clone(), id);
        }
        assert!(have.len() > before, "no type could mint; stuck with {have:?}");
    }
    have
}

fn commit(statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
    let mut b = Bindings::new();
    for (k, v) in params {
        b.insert((*k).to_string(), v.clone());
    }
    Envelope {
        statement: statement.to_string(),
        bindings: b,
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent { thread: "seal".into(), dispatch: "seal".into() },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(unique("nonce")),
    }
}

fn returned_id(outcome: Outcome, column: &str) -> NodeId {
    match outcome {
        Outcome::Committed { returned, .. } => NodeId(returned.0[0][column].as_str().expect("id").to_string()),
        other => panic!("expected Committed, got {other:?}"),
    }
}

#[track_caller]
fn refused<T: std::fmt::Debug>(r: spine::error::Result<T>, what: &str) -> Witness {
    match r {
        Err(e) => only_witness(&e),
        Ok(v) => panic!("{what}: expected a refusal, got {v:?}"),
    }
}

#[track_caller]
fn only_witness(err: &SpineError) -> Witness {
    match err {
        SpineError::Refused { witnesses } => {
            assert_eq!(witnesses.len(), 1, "one refusal, one witness: {witnesses:?}");
            witnesses[0].clone()
        }
        other => panic!("expected a structured refusal, got {other:?}"),
    }
}

fn read(c: &Cauldron, statement: &str, params: &[(&str, serde_json::Value)]) -> Vec<BTreeMap<String, serde_json::Value>> {
    let mut b = Bindings::new();
    for (k, v) in params {
        b.insert((*k).to_string(), v.clone());
    }
    let rows = c
        .read(&ReadRequest {
            statement: statement.to_string(),
            bindings: b,
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect("read")
        .rows
        .0;
    rows.into_iter().map(|r| serde_json::from_value(serde_json::to_value(r).unwrap()).unwrap()).collect()
}
