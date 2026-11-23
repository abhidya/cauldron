//! The v6 grammar corrections, each proven at the one door,
//! `Cauldron::submit`:
//!
//! - G1 is `grammar_v6_data_door.rs`'s round trip (twelve types by name);
//!   here `the_viewer_lands_as_v_1_and_reads_back_its_role` pins the new one.
//! - G2 a `viewer` takes ONE field (the role) and NO state.
//! - G3 required-at-mint is ONE OF: >= 1 `in_region`, or >= 2 `rejected_by`
//!   plus `searched`, counted as edges SURVIVING the statement.
//! - G4 a `shows` edge carries a required `cite` and a `why` <= 235.
//! - G5 the stored `depends-on` (task) and `superseded` (handoff) are gone.
//! - `ladle::compile::claims_for`'s `unknown_edge_kind` arm, which
//!   no statement through the door reaches (the compiler refuses first), IS
//!   reached by a direct caller of that public function, and is guarded
//!   here.
//!
//! Every case is paired: the refusal beside the landing it is the opposite
//! of, so neither "refuse everything" nor "accept everything" passes.

use spine::config::SpineConfig;
use spine::delta::{CompiledPart, CompiledPayload, Delta};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::grammar_v6;
use spine::ladle::compile::claims_for;
use spine::ladle::parse::parse;
use spine::pool::{NodeId, QualifiedNodeId};
use spine::read::MatchSet;
use spine::schema_def::{grammar_fault_tag, PoolGrammar, PoolName};
use spine::witness::Witness;
use spine::{Cauldron, Outcome, ReadRequest};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

test_scratch::install!();

const POOL: &str = "ws-v6";
const CITE: &str = r#"{ "thread": "00000000-0000-4000-8000-000000000001", "msg": 1 }"#;

fn g() -> PoolGrammar {
    grammar_v6::workspace_v6(POOL)
}

/// A cauldron holding the v6 grammar AS DECLARED (prefixes and all), in a
/// temp dir removed when the guard drops.
struct Fresh {
    c: Option<Cauldron>,
    dir: PathBuf,
}
impl Drop for Fresh {
    fn drop(&mut self) {
        drop(self.c.take());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
impl std::ops::Deref for Fresh {
    type Target = Cauldron;
    fn deref(&self) -> &Cauldron {
        self.c.as_ref().unwrap()
    }
}
fn fresh(tag: &str) -> Fresh {
    let dir = test_scratch::tmp_path(format!("spine-v6-corrections-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let c = Cauldron::create(&dir, &[g()], SpineConfig::default()).expect("create the cauldron");
    Fresh { c: Some(c), dir }
}

fn jv(v: impl serde::Serialize) -> Value {
    serde_json::to_value(v).expect("json")
}

fn unique(what: &str) -> String {
    static SERIAL: AtomicU32 = AtomicU32::new(0);
    format!("{what} {:06}", SERIAL.fetch_add(1, Ordering::SeqCst))
}

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
            initiator: Initiator::Agent { thread: "grammar-v6-corrections".into(), dispatch: "grammar-v6-corrections".into() },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(unique("nonce")),
    }
}

#[track_caller]
fn landed(r: spine::error::Result<Outcome>) -> Outcome {
    match r {
        Ok(o @ Outcome::Committed { .. }) => o,
        other => panic!("expected Committed, got {other:?}"),
    }
}

#[track_caller]
fn refused(r: spine::error::Result<Outcome>) -> Witness {
    match r {
        Err(SpineError::Refused { witnesses }) => {
            assert_eq!(witnesses.len(), 1, "one refusal, one witness: {witnesses:?}");
            witnesses[0].clone()
        }
        other => panic!("expected a structured refusal, got {other:?}"),
    }
}

fn id_of(o: Outcome, column: &str) -> String {
    match o {
        Outcome::Committed { returned, .. } => returned.0[0][column].as_str().expect("id").to_string(),
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn count(c: &Cauldron, node_type: &str) -> usize {
    c.read(&ReadRequest {
        statement: format!("MATCH (n: {POOL}/{node_type}) RETURN n.id"),
        bindings: Bindings::new(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    })
    .expect("read")
    .rows
    .0
    .len()
}

fn region(c: &Cauldron) -> String {
    id_of(
        landed(c.submit(commit(
            &format!("CREATE (r: {POOL}/region {{ handle: $h, boundary: $b }}) RETURN r.id"),
            &[("h", jv(unique("region"))), ("b", jv(format!("{} Inside: everything. Outside: nothing.", unique("b"))))],
        ))),
        "r.id",
    )
}

fn decision(c: &Cauldron, r: &str) -> String {
    id_of(
        landed(c.submit(commit(
            &format!(
                "MATCH (r: {POOL}/region {{ id: $r }}) CREATE (n: {POOL}/decision {{ handle: $h, state: \"decided\" }}) CREATE (n) -[in_region]-> (r) RETURN n.id"
            ),
            &[("r", jv(r)), ("h", jv(unique("decision")))],
        ))),
        "n.id",
    )
}

fn viewer(c: &Cauldron) -> String {
    id_of(
        landed(c.submit(commit(
            &format!("CREATE (v: {POOL}/viewer {{ role: $role }}) RETURN v.id"),
            &[("role", jv(unique("coordinator")))],
        ))),
        "v.id",
    )
}

/// A string of exactly `n` characters, unique in its head.
fn chars(n: usize) -> String {
    let mut s = unique("w");
    s.truncate(n);
    while s.chars().count() < n {
        s.push('y');
    }
    assert_eq!(s.chars().count(), n);
    s
}

// ---------------------------------------------------------------------------
// G1 — the twelfth type lands
// ---------------------------------------------------------------------------

// RED (G1): the `viewer` insert removed from `workspace_v6` — this refused
// `unknown_node_type` (and the door suite's round trip failed its count).
// Opposed by G2's refusals: a door accepting every create fails there.
#[test]
fn the_viewer_lands_as_v_1_and_reads_back_its_role() {
    let c = fresh("g1");
    let role = unique("coordinator");
    let id = id_of(
        landed(c.submit(commit(
            &format!("CREATE (v: {POOL}/viewer {{ role: $role }}) RETURN v.id"),
            &[("role", jv(&role))],
        ))),
        "v.id",
    );
    assert_eq!(id, "V-1", "the first viewer is serial 1 of prefix V");
    let rows = c
        .read(&ReadRequest {
            statement: format!("MATCH (v: {POOL}/viewer {{ id: $id }}) RETURN v.role"),
            bindings: Bindings::from([("id".to_string(), jv(&id))]),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect("read")
        .rows
        .0;
    assert_eq!(rows.len(), 1);
    assert_eq!(serde_json::to_value(&rows[0]).unwrap()["v.role"], jv(&role));
}

// ---------------------------------------------------------------------------
// G2 — a viewer: one field, no state
// ---------------------------------------------------------------------------

// RED (G2): (a) `handle` added to the viewer's fields in `workspace_v6` —
// "a viewer with a second field" LANDED; (b) the viewer given the states
// ["live"] — "a viewer with a state" refused `unknown_state`, not
// `state_not_declared`, and the SET of `state` landed. Opposed: the plain
// viewer (role only, no state) lands in the same test.
#[test]
fn a_viewer_with_a_second_field_or_a_state_is_refused() {
    let c = fresh("g2");
    // Not-X: the one-field, stateless viewer lands.
    let v = viewer(&c);
    let before = count(&c, "viewer");

    // A second field: `handle`, which every other type carries.
    let w = refused(c.submit(commit(
        &format!("CREATE (v: {POOL}/viewer {{ role: $role, handle: \"h\" }}) RETURN v.id"),
        &[("role", jv(unique("r")))],
    )));
    assert_eq!(grammar_fault_tag(&w), Some("undeclared_field"), "{w:?}");
    // A state at mint.
    let w = refused(c.submit(commit(
        &format!("CREATE (v: {POOL}/viewer {{ role: $role, state: \"active\" }}) RETURN v.id"),
        &[("role", jv(unique("r")))],
    )));
    assert_eq!(grammar_fault_tag(&w), Some("state_not_declared"), "{w:?}");
    // A state SET later, and a second field SET later.
    let w = refused(c.submit(commit(
        &format!("MATCH (v: {POOL}/viewer {{ id: $v }}) SET v.state = \"active\""),
        &[("v", jv(&v))],
    )));
    assert_eq!(grammar_fault_tag(&w), Some("state_not_declared"), "{w:?}");
    let w = refused(c.submit(commit(
        &format!("MATCH (v: {POOL}/viewer {{ id: $v }}) SET v.why = \"because\""),
        &[("v", jv(&v))],
    )));
    assert_eq!(grammar_fault_tag(&w), Some("undeclared_field"), "{w:?}");
    // The one field is required.
    let w = refused(c.submit(commit(&format!("CREATE (v: {POOL}/viewer {{ }}) RETURN v.id"), &[])));
    assert_eq!(grammar_fault_tag(&w), Some("missing_required_field"), "{w:?}");
    assert_eq!(count(&c, "viewer"), before, "no refusal landed a viewer");
}

// ---------------------------------------------------------------------------
// G3 — required-at-mint is ONE OF, over the edges that SURVIVE
// ---------------------------------------------------------------------------

/// Mint a `task` through the door with the given edge clauses (each a
/// `CREATE`/`REMOVE` line naming `(n)` and the regions `r0`, `r1`) and the
/// `searched` field when `searched` is true.
fn task_mint(c: &Cauldron, r: &[String; 2], edges: &[&str], searched: bool) -> spine::error::Result<Outcome> {
    let s = if searched { ", searched: \"region find task\"" } else { "" };
    let stmt = format!(
        "MATCH (r0: {POOL}/region {{ id: $r0 }}) MATCH (r1: {POOL}/region {{ id: $r1 }}) CREATE (n: {POOL}/task {{ handle: $h, state: \"ready\"{s} }}) {} RETURN n.id",
        edges.join(" ")
    );
    c.submit(commit(&stmt, &[("r0", jv(&r[0])), ("r1", jv(&r[1])), ("h", jv(unique("task")))]))
}

#[track_caller]
fn assert_required_edge(w: &Witness, kind: &str, minimum: u32, found: u32) {
    match w {
        Witness::RequiredEdgeMissing { pool, node_type, edge_kind, minimum: m, found: f, .. } => {
            assert_eq!(pool, &PoolName::new(POOL));
            assert_eq!((node_type.as_str(), edge_kind.as_str(), *m, *f), ("task", kind, minimum, found), "{w:?}");
        }
        other => panic!("expected RequiredEdgeMissing({kind}, {minimum}, {found}), got {other:?}"),
    }
}

// RED (G3), each run then restored:
// (a) AND instead of OR — the two alternatives merged into ONE
//     (`in_region: 1, rejected_by: 2, searched`) in `mint_alternatives`:
//     "in_region alone lands" refused `RequiredEdgeMissing { rejected_by,
//     2, 0 }`.
// (b) the opposing forcing, accept-all — `one_of_shortfall` made to return
//     `None`: "1 x rejected_by + searched" LANDED.
// (c) the add-then-remove hole — `surviving_edges` made to ignore
//     `RemoveEdge`: "add-then-remove of the only in_region" LANDED.
// (d) the field arm dropped (`alt.fields` ignored): "2 x rejected_by
//     without searched" LANDED.
#[test]
fn required_at_mint_is_in_region_or_two_rejected_by_plus_searched() {
    let c = fresh("g3");
    let r = [region(&c), region(&c)];
    assert_ne!(r[0], r[1], "two regions, not one deduplicated mint");

    // --- lands ---
    // in_region alone.
    landed(task_mint(&c, &r, &["CREATE (n) -[in_region]-> (r0)"], false));
    // two rejected_by (distinct regions) plus searched, NO in_region.
    landed(task_mint(&c, &r, &["CREATE (n) -[rejected_by]-> (r0)", "CREATE (n) -[rejected_by]-> (r1)"], true));
    // both arms at once.
    landed(task_mint(
        &c,
        &r,
        &["CREATE (n) -[in_region]-> (r0)", "CREATE (n) -[rejected_by]-> (r0)", "CREATE (n) -[rejected_by]-> (r1)"],
        true,
    ));
    let landed_tasks = count(&c, "task");
    assert_eq!(landed_tasks, 3);

    // --- refused, each naming the shortfall of the arm it attempted ---
    // nothing at all: the first arm, in_region 1/0.
    assert_required_edge(&refused(task_mint(&c, &r, &[], false)), "in_region", 1, 0);
    // one rejected_by plus searched: rejected_by 2/1.
    assert_required_edge(
        &refused(task_mint(&c, &r, &["CREATE (n) -[rejected_by]-> (r0)"], true)),
        "rejected_by",
        2,
        1,
    );
    // two rejected_by to the SAME region is ONE edge (the triple is the
    // key): rejected_by 2/1.
    assert_required_edge(
        &refused(task_mint(&c, &r, &["CREATE (n) -[rejected_by]-> (r0)", "CREATE (n) -[rejected_by]-> (r0)"], true)),
        "rejected_by",
        2,
        1,
    );
    // two rejected_by, distinct, but NO searched: the field is named.
    let w = refused(task_mint(&c, &r, &["CREATE (n) -[rejected_by]-> (r0)", "CREATE (n) -[rejected_by]-> (r1)"], false));
    assert_eq!(grammar_fault_tag(&w), Some("missing_required_field"), "{w:?}");
    match &w {
        Witness::GrammarViolation { message, .. } => assert!(message.contains("`searched`"), "{message}"),
        other => panic!("{other:?}"),
    }
    // add-then-remove of the only in_region: in_region 1/0.
    assert_required_edge(
        &refused(task_mint(&c, &r, &["CREATE (n) -[in_region]-> (r0)", "REMOVE (n) -[in_region]-> (r0)"], false)),
        "in_region",
        1,
        0,
    );
    // add two rejected_by, remove one, with searched: rejected_by 2/1.
    assert_required_edge(
        &refused(task_mint(
            &c,
            &r,
            &["CREATE (n) -[rejected_by]-> (r0)", "CREATE (n) -[rejected_by]-> (r1)", "REMOVE (n) -[rejected_by]-> (r1)"],
            true,
        )),
        "rejected_by",
        2,
        1,
    );
    assert_eq!(count(&c, "task"), landed_tasks, "no refusal landed a task");
}

/// The rule is the data's, on EVERY content type, and on no other: each
/// content type mints regionless (2 x rejected_by + searched) and refuses
/// bare; a region and a viewer carry no alternative.
// RED: `mint_alternatives` made to give every content type but
// `constraint` the `in_region` arm only — failed "`danger`" (1 arm, not 2).
// Opposed: the bare mint of every content type is refused in the same loop.
#[test]
fn every_content_type_takes_the_one_of_and_no_other_type_does() {
    let c = fresh("g3-all");
    let r = [region(&c), region(&c)];
    let g = g();
    let cite: Value = serde_json::from_str(CITE).unwrap();
    let directive = id_of(
        landed(c.submit(commit(
            &format!(
                "MATCH (r: {POOL}/region {{ id: $r }}) CREATE (d: {POOL}/directive {{ handle: $h, state: \"active\", operator: $op }}) CREATE (d) -[in_region]-> (r) RETURN d.id"
            ),
            &[("r", jv(&r[0])), ("h", jv(unique("dir"))), ("op", cite.clone())],
        ))),
        "d.id",
    );
    let mut content = 0;
    for (t, tdef) in &g.node_types {
        if t == "region" || t == "viewer" {
            assert!(tdef.required_at_mint_any_of.is_empty(), "`{t}` carries no one-of");
            continue;
        }
        content += 1;
        assert_eq!(tdef.required_at_mint_any_of.len(), 2, "`{t}`");
        let state = tdef.states.first().map(|s| format!(", state: \"{s}\"")).unwrap_or_default();
        let op = if t == "directive" { ", operator: $op" } else { "" };
        let serves = if t == "milestone" { "CREATE (n) -[serves]-> (d)" } else { "" };
        let mint = |edges: &str, searched: bool| {
            let s = if searched { ", searched: \"terms\"" } else { "" };
            let mut params =
                vec![("r0", jv(&r[0])), ("r1", jv(&r[1])), ("d", jv(&directive)), ("h", jv(unique(t)))];
            if !op.is_empty() {
                params.push(("op", cite.clone()));
            }
            c.submit(commit(
                &format!(
                    "MATCH (r0: {POOL}/region {{ id: $r0 }}) MATCH (r1: {POOL}/region {{ id: $r1 }}) MATCH (d: {POOL}/directive {{ id: $d }}) CREATE (n: {POOL}/{t} {{ handle: $h{state}{op}{s} }}) {serves} {edges} RETURN n.id"
                ),
                &params,
            ))
        };
        landed(mint("CREATE (n) -[rejected_by]-> (r0) CREATE (n) -[rejected_by]-> (r1)", true));
        let w = refused(mint("", false));
        assert!(
            matches!(&w, Witness::RequiredEdgeMissing { edge_kind, minimum: 1, found: 0, .. } if edge_kind == "in_region"),
            "`{t}` bare: {w:?}"
        );
    }
    assert_eq!(content, 10, "the ten content types");
}

// ---------------------------------------------------------------------------
// G4 — shows: required cite, why <= 235
// ---------------------------------------------------------------------------

fn shows(c: &Cauldron, v: &str, n: &str, fields: Value) -> spine::error::Result<Outcome> {
    c.submit(commit(
        &format!(
            "MATCH (v: {POOL}/viewer {{ id: $v }}) MATCH (n: {POOL}/decision {{ id: $n }}) CREATE (v) -[shows]-> (n) NOTE $f"
        ),
        &[("v", jv(v)), ("n", jv(n)), ("f", fields)],
    ))
}

// RED (G4): (a) `cite`'s `required` flipped to false in `edge_fields` —
// "no cite" LANDED; (b) `WHY_CAP` 235 -> 236 — "why of 236" LANDED; (c) the
// opposing forcing, `WHY_CAP` 235 -> 234 — "why of 235 lands" refused
// `EdgeFieldOverCap { measured: 235, cap: 234, overage: 1 }`.
#[test]
fn a_shows_edge_needs_a_cite_and_a_why_of_at_most_235() {
    let c = fresh("g4");
    let r = region(&c);
    let v = viewer(&c);
    let cite: Value = serde_json::from_str(CITE).unwrap();
    // why at exactly 235 lands (each on its own decision: one edge per
    // (src, kind, dst), and a differing re-create refuses).
    let n = decision(&c, &r);
    landed(shows(&c, &v, &n, json!({ "cite": cite, "why": chars(235) })));
    // why of 236 refuses, measured / cap / overage by value.
    let n = decision(&c, &r);
    let w = refused(shows(&c, &v, &n, json!({ "cite": cite, "why": chars(236) })));
    match &w {
        Witness::EdgeFieldOverCap { pool, edge_kind, field, measured, cap, overage, .. } => {
            assert_eq!(pool, &PoolName::new(POOL));
            assert_eq!((edge_kind.as_str(), field.as_str()), ("shows", "why"));
            assert_eq!((*measured, *cap, *overage), (236, 235, 1));
        }
        other => panic!("expected EdgeFieldOverCap, got {other:?}"),
    }
    assert_eq!(grammar_fault_tag(&w), Some("over_cap"));
    // no cite refuses by name.
    let w = refused(shows(&c, &v, &n, json!({ "why": chars(40) })));
    assert_eq!(grammar_fault_tag(&w), Some("missing_required_edge_field"), "{w:?}");
    match &w {
        Witness::GrammarViolation { message, .. } => assert!(message.contains("`cite`"), "{message}"),
        other => panic!("{other:?}"),
    }
    // no why refuses too (the grammar requires it); an empty why
    // is under its floor of 1.
    let w = refused(shows(&c, &v, &n, json!({ "cite": cite })));
    assert_eq!(grammar_fault_tag(&w), Some("missing_required_edge_field"), "{w:?}");
    let w = refused(shows(&c, &v, &n, json!({ "cite": cite, "why": "" })));
    assert!(matches!(w, Witness::EdgeFieldUnderFloor { measured: 0, floor: 1, shortfall: 1, .. }), "{w:?}");
    // a cite that is not an object refuses its kind.
    let w = refused(shows(&c, &v, &n, json!({ "cite": "not-an-object", "why": "w" })));
    assert_eq!(grammar_fault_tag(&w), Some("wrong_field_kind"), "{w:?}");
    // Not-X again on the same target: the valid edge lands after all the
    // refusals left it untouched.
    landed(shows(&c, &v, &n, json!({ "cite": cite, "why": chars(1) })));
}

// ---------------------------------------------------------------------------
// G5 — the computed states are not stored
// ---------------------------------------------------------------------------

// RED (G5): (a) `depends-on` restored to task's states in `CONTENT_TYPES` —
// "a task minted depends-on" LANDED; (b) `superseded` restored to handoff's
// — "a handoff minted superseded" LANDED. Opposed: `ready`, `active` and
// `current` land in the same test.
#[test]
fn storing_depends_on_or_superseded_is_refused() {
    let c = fresh("g5");
    let r = region(&c);
    let mint = |t: &str, state: &str| {
        c.submit(commit(
            &format!(
                "MATCH (r: {POOL}/region {{ id: $r }}) CREATE (n: {POOL}/{t} {{ handle: $h, state: \"{state}\" }}) CREATE (n) -[in_region]-> (r) RETURN n.id"
            ),
            &[("r", jv(&r)), ("h", jv(unique(t)))],
        ))
    };
    let task = id_of(landed(mint("task", "ready")), "n.id");
    let handoff = id_of(landed(mint("handoff", "current")), "n.id");
    for (t, s) in grammar_v6::DROPPED_STATES.iter().map(|(t, s, _)| (*t, *s)) {
        let w = refused(mint(t, s));
        assert_eq!(grammar_fault_tag(&w), Some("unknown_state"), "{t} minted {s}: {w:?}");
        let id = if t == "task" { &task } else { &handoff };
        let w = refused(c.submit(commit(
            &format!("MATCH (n: {POOL}/{t} {{ id: $id }}) SET n.state = \"{s}\""),
            &[("id", jv(id))],
        )));
        assert_eq!(grammar_fault_tag(&w), Some("unknown_state"), "{t} SET {s}: {w:?}");
    }
    // The two by name, so emptying DROPPED_STATES cannot pass this.
    assert_eq!(
        grammar_v6::DROPPED_STATES.iter().map(|(t, s, _)| (*t, *s)).collect::<Vec<_>>(),
        vec![("task", "depends-on"), ("handoff", "superseded")]
    );
    // Not-X: a stored state the vocabulary keeps still moves.
    landed(c.submit(commit(
        &format!("MATCH (n: {POOL}/task {{ id: $id }}) SET n.state = \"active\""),
        &[("id", jv(&task))],
    )));
    assert_eq!(count(&c, "task"), 1);
    assert_eq!(count(&c, "handoff"), 1);
}

// ---------------------------------------------------------------------------
// `claims_for`'s unknown_edge_kind arm
// ---------------------------------------------------------------------------

/// `claims_for` is public (`ladle_unit.rs` calls it directly). Through
/// `Cauldron::submit` its `unknown_edge_kind` arm cannot fire, because
/// `compile` refuses an undeclared kind first (the CREATE/REMOVE-edge path,
/// tagged) and `claims_for` only ever sees a payload `compile`
/// produced under the same pinned grammar. A DIRECT caller handing it a
/// payload it did not compile reaches the arm — so the arm is not dead, it
/// is the function's own guard, and this test forces it.
// RED: the arm's `.ok_or_else(...)?` replaced by skipping the
// edge (`let Some(def) = ... else { continue };`) — the call returned Ok
// and this test failed "expected a refusal". Opposed: the same payload
// with a DECLARED kind returns Ok with the edge's write claim.
#[test]
fn claims_for_refuses_an_undeclared_edge_kind_in_a_direct_payload() {
    let grammars = |p: &PoolName| if p.as_str() == POOL { Some(g()) } else { None };
    let stmt = parse(&format!("CREATE (n: {POOL}/region {{ handle: \"h\" }})")).expect("parses");
    let matched = MatchSet { bindings: BTreeMap::new() };
    let payload = |kind: &str| CompiledPayload {
        parts: vec![CompiledPart {
            pool: PoolName::new(POOL),
            schema_version: grammar_v6::PIN,
            deltas: vec![Delta::AddEdge {
                src: NodeId("T-1".into()),
                kind: kind.into(),
                dst: QualifiedNodeId { pool: PoolName::new(POOL), id: NodeId("R-1".into()) },
                note: None,
                fields: Default::default(),
            }],
            premises: vec![],
            far_versions: BTreeMap::new(),
        }],
        minted: BTreeMap::new(),
    };
    match claims_for(&stmt, &matched, &payload("governs"), &grammars) {
        Err(SpineError::Refused { witnesses }) => {
            assert_eq!(witnesses.len(), 1);
            assert_eq!(grammar_fault_tag(&witnesses[0]), Some("unknown_edge_kind"), "{witnesses:?}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    let ok = claims_for(&stmt, &matched, &payload("in_region"), &grammars).expect("a declared kind claims");
    assert!(format!("{ok:?}").contains("in_region"), "the edge's claim is in the set: {ok:?}");
}
