//! The v6 workspace grammar (`spine::grammar_v6`) at the one door.
//!
//! Every test here ENUMERATES its cases from the grammar data — types,
//! capped fields, required edges, edge kinds — so a type, a cap or an edge
//! dropped from the data changes what runs, and the counts are pinned to the
//! source's own (`SOURCE_TYPE_COUNT`; a maintainer-only probe outside the
//! published tree reads the v5 files themselves).
//!
//! # The id prefix and serial ids
//!
//! The v6 grammar declares an id prefix on EVERY type (`DIR-`, `R-`, ...).
//! When these guards were written the door still minted ids by hash (`n<16 hex>`), so
//! every CREATE of a prefixed type refused `id_prefix_mismatch` through
//! `Cauldron::submit`. Each door guard therefore runs TWICE:
//!
//! - `..._prefixes_stripped` — the grammar exactly as declared, with ONLY
//!   `id_prefix` set to `None` on every type ([`unprefixed`]); proves every
//!   other rule at the door;
//! - `..._as_declared` — the grammar untouched. Ignored until serial
//!   ids (`<prefix>-<n>`) were under it; now un-ignored, and the
//!   round trip now also asserts each minted id is `<its prefix>-<n>`.
//!
//! The prefix itself is proven now at the validator (the function the door
//! calls) by `every_type_accepts_its_own_prefix_and_refuses_anothers`.
//!
//! # Viewer, rejected_by, shows
//!
//! Twelve types (`viewer` added), twelve edge kinds (`rejected_by`,
//! `shows`), and `in_region` moved from the AND form into the first arm of
//! the one-of required-at-mint; the guards here follow the data. The new
//! rules themselves are guarded in `grammar_v6_corrections.rs`.

use spine::config::SpineConfig;
use spine::delta::{CompiledPart, Delta};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::grammar_v6::{self, EDGE_KINDS};
use spine::pool::{NodeId, QualifiedNodeId};
use spine::schema_def::{grammar_fault_tag, FarSide, FieldKind, PoolGrammar, PoolName};
use spine::witness::Witness;
use spine::{Cauldron, Outcome, ReadRequest};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

test_scratch::install!();

const POOL: &str = "ws-v6";

/// The number of node types the SOURCE declares, counted there and not
/// here: the source's ten content types plus `region`, eleven
/// `node.<type>.schema.json` files, eleven `TYPE_PREFIX` entries. A literal
/// on purpose: a type dropped from the data must not move it.
const SOURCE_TYPE_COUNT: usize = 11;

/// The v6 type count: the source's eleven plus `viewer`, the twelfth.
/// Also a literal, and the names are pinned too ([`V6_TYPES`]).
const V6_TYPE_COUNT: usize = SOURCE_TYPE_COUNT + 1;
const V6_TYPES: [&str; 12] = [
    "constraint", "danger", "decision", "directive", "handoff", "idea", "issue", "milestone",
    "question", "region", "task", "viewer",
];

fn declared() -> PoolGrammar {
    grammar_v6::workspace_v6(POOL)
}

/// The declared grammar with ONLY the id prefixes removed (see the module
/// docs). Everything else is the data, untouched.
fn unprefixed() -> PoolGrammar {
    let mut g = declared();
    for t in g.node_types.values_mut() {
        t.id_prefix = None;
    }
    g
}

// ---------------------------------------------------------------------------
// guard 1: every type round-trips one valid node through Cauldron::submit
// ---------------------------------------------------------------------------

// RED: (a) `idea` dropped from `CONTENT_TYPES` — the count assertion
// failed (10 types); (b) the opposing forcing, a door that refuses valid
// nodes: `in_region`'s minimum raised 1 -> 2 in the data — "a valid `task`
// must land" failed on `RequiredEdgeMissing { minimum: 2, found: 1 }`.
// (c) the `viewer` insert removed from `workspace_v6` — the
// count assertion failed (11 types).
fn every_type_round_trips(g: PoolGrammar, tag: &str) {
    let types: Vec<String> = g.node_types.keys().cloned().collect();
    assert_eq!(types.len(), V6_TYPE_COUNT, "the grammar's types: {types:?}");
    assert_eq!(types, V6_TYPES.map(String::from).to_vec(), "the twelve v6 types by name");
    let c = fresh(tag, &g);
    let mut have: BTreeMap<String, NodeId> = BTreeMap::new();
    // Mint in dependency order: a type mints once every required edge has a
    // target already minted. A pass that mints nothing is a grammar no pool
    // could ever fill, and fails.
    while have.len() < types.len() {
        let before = have.len();
        for t in &types {
            if have.contains_key(t) || !targets_ready(&g, t, &have) {
                continue;
            }
            // The handle — or, on a viewer (no handle), its one field.
            let key = if g.node_types[t].fields.contains_key("handle") { "handle" } else { "role" };
            let handle = unique("round trip");
            let id = mint(&c, &g, t, &have, &[(key, json(&handle))], None)
                .unwrap_or_else(|e| panic!("a valid `{t}` must land: {e:?}"));
            // The first of its prefix in a fresh pool is serial 1.
            if let Some(p) = &g.node_types[t].id_prefix {
                assert_eq!(id.0, format!("{p}-1"), "`{t}` is the first `{p}-` minted");
            }
            // Read it back through the read door: the type, the handle.
            let rows = read(
                &c,
                &format!("MATCH (n: {POOL}/{t} {{ id: $id }}) RETURN n.{key}"),
                &[("id", json(&id.0))],
            );
            assert_eq!(rows.len(), 1, "`{t}` {id} reads back");
            assert_eq!(rows[0][&format!("n.{key}")], json(&handle));
            have.insert(t.clone(), id);
        }
        assert!(have.len() > before, "no type could mint; stuck with {have:?}");
    }
    for t in &types {
        assert_eq!(count(&c, t), 1, "exactly one `{t}`");
    }
}

#[test]
fn every_type_round_trips_one_valid_node_prefixes_stripped() {
    every_type_round_trips(unprefixed(), "rt-stripped");
}

#[test]
fn every_type_round_trips_one_valid_node_as_declared() {
    every_type_round_trips(declared(), "rt-declared");
}

// ---------------------------------------------------------------------------
// guard 2: over cap by exactly one refuses with measured/cap/overage; at cap
// lands; under a floor by one refuses; at the floor lands
// ---------------------------------------------------------------------------

// RED: (a) `check_field_value`'s `measured > cap` flipped to `>=` — the
// at-cap body refused (`overage: 0`); (b) the cap arm disabled — the
// one-over mint landed; (c) the witness's overage hard-coded to 2 — the
// overage assertion failed; (d) the floor's `<` flipped to `<=` — the
// at-floor boundary refused. This loop reads its caps FROM the data, so a
// wrong NUMBER in the data (235 -> 236) leaves it green: that is caught by
// `the_source_numbers_refuse_at_the_door` (and by a maintainer-only probe
// outside the published tree).
fn every_cap_is_hard_by_one(g: PoolGrammar, tag: &str) {
    let c = fresh(tag, &g);
    let have = seed(&c, &g);
    let mut caps_seen = 0;
    let mut floors_seen = 0;
    for (t, tdef) in &g.node_types {
        for (f, fdef) in &tdef.fields {
            if fdef.kind != FieldKind::Text {
                continue;
            }
            let floor = fdef.min_chars.unwrap_or(0);
            if let Some(cap) = fdef.max_chars {
                caps_seen += 1;
                let at = filler(cap);
                mint(&c, &g, t, &have, &[(f, json(&at))], None)
                    .unwrap_or_else(|e| panic!("`{t}.{f}` AT its cap {cap} must land: {e:?}"));
                let w = refused(mint(&c, &g, t, &have, &[(f, json(&filler(cap + 1)))], None));
                match &w {
                    Witness::OverCap { pool, node_type, field, measured, cap: wcap, overage, .. } => {
                        assert_eq!(pool, &PoolName::new(POOL));
                        assert_eq!((node_type.as_str(), field.as_str()), (t.as_str(), f.as_str()));
                        assert_eq!(*measured, cap + 1, "`{t}.{f}` measured");
                        assert_eq!(*wcap, cap, "`{t}.{f}` cap");
                        assert_eq!(*overage, 1, "`{t}.{f}` overage");
                    }
                    other => panic!("`{t}.{f}` one over: expected OverCap, got {other:?}"),
                }
            }
            if floor > 0 {
                floors_seen += 1;
                mint(&c, &g, t, &have, &[(f, json(&filler(floor)))], None)
                    .unwrap_or_else(|e| panic!("`{t}.{f}` AT its floor {floor} must land: {e:?}"));
                let w = refused(mint(&c, &g, t, &have, &[(f, json(&filler(floor - 1)))], None));
                match &w {
                    Witness::UnderFloor { node_type, field, measured, floor: wf, shortfall, .. } => {
                        assert_eq!((node_type.as_str(), field.as_str()), (t.as_str(), f.as_str()));
                        assert_eq!((*measured, *wf, *shortfall), (floor - 1, floor, 1), "`{t}.{f}`");
                    }
                    other => panic!("`{t}.{f}` one under: expected UnderFloor, got {other:?}"),
                }
            }
        }
    }
    // handle + body on ten content types, handle + boundary + wake_when on
    // region (the viewer's `role` is uncapped); floors: eleven handles, the
    // boundary, ten `searched` and the viewer's `role`.
    assert_eq!((caps_seen, floors_seen), (23, 23), "the capped fields the data declares");
}

#[test]
fn every_cap_is_hard_by_one_character_prefixes_stripped() {
    every_cap_is_hard_by_one(unprefixed(), "cap-stripped");
}

#[test]
fn every_cap_is_hard_by_one_character_as_declared() {
    every_cap_is_hard_by_one(declared(), "cap-declared");
}

/// The numbers themselves, at the door, independent of the loop above: a
/// 236-character handle and a 10,001-character body each refuse with
/// exactly (236, 235, 1) and (10001, 10000, 1).
// RED: `HANDLE_CAP` in the data set to 236 — the 236-character handle
// landed and `refused` panicked.
#[test]
fn the_source_numbers_refuse_at_the_door() {
    let g = unprefixed();
    let c = fresh("numbers", &g);
    let have = seed(&c, &g);
    let w = refused(mint(&c, &g, "decision", &have, &[("handle", json(&filler(236)))], None));
    assert!(matches!(w, Witness::OverCap { measured: 236, cap: 235, overage: 1, .. }), "{w:?}");
    let w = refused(mint(&c, &g, "decision", &have, &[("body", json(&filler(10_001)))], None));
    assert!(matches!(w, Witness::OverCap { measured: 10_001, cap: 10_000, overage: 1, .. }), "{w:?}");
    let w = refused(mint(&c, &g, "region", &have, &[("boundary", json(&filler(1_001)))], None));
    assert!(matches!(w, Witness::OverCap { measured: 1_001, cap: 1_000, overage: 1, .. }), "{w:?}");
}

// ---------------------------------------------------------------------------
// guard 3: a missing required-at-mint edge is refused by name, per type
// ---------------------------------------------------------------------------

// RED: (a) `in_region` dropped from `required_edges`, and (b) the
// `REQUIRED_EXTRA` milestone row removed — each failed the case count (the
// enumeration shrank); (c) the
// validator's `found < *minimum` replaced by `false` — the mint without the
// edge LANDED; (d) replaced by `true` (refuse everything) — the mint WITH
// the edge refused.
// The content types' `in_region` is now the FIRST arm of the one-of
// (`required_at_mint_any_of`); `required_for_mint` folds it back in, so a
// mint omitting it — and carrying no other arm — still refuses it by name.
fn every_required_edge_is_refused_by_name(g: PoolGrammar, tag: &str) {
    let c = fresh(tag, &g);
    let have = seed(&c, &g);
    let mut cases = 0;
    for t in g.node_types.keys() {
        for (k, minimum) in &required_for_mint(&g, t) {
            cases += 1;
            let before = count(&c, t);
            let w = refused(mint(&c, &g, t, &have, &[], Some(k)));
            match &w {
                Witness::RequiredEdgeMissing { pool, node_type, edge_kind, minimum: m, found, .. } => {
                    assert_eq!(pool, &PoolName::new(POOL));
                    assert_eq!((node_type.as_str(), edge_kind.as_str()), (t.as_str(), k.as_str()));
                    assert_eq!((*m, *found), (*minimum, 0));
                }
                other => panic!("`{t}` without `{k}`: expected RequiredEdgeMissing, got {other:?}"),
            }
            assert_eq!(grammar_fault_tag(&w), Some("required_edge_missing"));
            assert_eq!(count(&c, t), before, "the refusal landed nothing");
            // Not-X: the same mint WITH the edge lands.
            mint(&c, &g, t, &have, &[], None).unwrap_or_else(|e| panic!("`{t}` with `{k}` lands: {e:?}"));
        }
    }
    // in_region on the ten content types, serves on the milestone.
    assert_eq!(cases, 11, "the required edges the data declares");
    // A region and a viewer require nothing: each mints bare.
    for t in ["region", "viewer"] {
        assert!(required_for_mint(&g, t).is_empty(), "{t}");
        mint(&c, &g, t, &have, &[], None).unwrap_or_else(|e| panic!("a {t} mints with no edge: {e:?}"));
    }
}

#[test]
fn every_required_edge_is_refused_by_name_prefixes_stripped() {
    every_required_edge_is_refused_by_name(unprefixed(), "req-stripped");
}

#[test]
fn every_required_edge_is_refused_by_name_as_declared() {
    every_required_edge_is_refused_by_name(declared(), "req-declared");
}

// ---------------------------------------------------------------------------
// guard 4: the edge set is CLOSED, and each kind's ends are its own
// ---------------------------------------------------------------------------

// RED: (a) `governs` added to `EDGE_KINDS` (All -> All) — the set-size
// assertion failed; with that and the outside-the-set precondition removed,
// the `governs` edge COMMITTED and `refused` panicked; (b) `serves`'s
// sources widened with `task` — task->directive serves committed; (c)
// `supersedes`'s targets set to All — decision->constraint committed; (d)
// the opposing forcing, the validator's source check made to refuse every
// edge — the legal edges (first, the seed's `in_region`) refused.
// (e) the compiler's `unknown_edge_kind`
// tag at the CREATE-edge site renamed to `wrong_endpoint_type` — the door
// tag assertion failed; (f) its `wrong_endpoint_type` (illegal source)
// renamed to `unknown_edge_kind` — the source-end door tag failed.
fn the_edge_set_is_closed(g: PoolGrammar, tag: &str) {
    let c = fresh(tag, &g);
    let have = seed_every_type(&c, &g);

    // Every kind in the set lands between one legal pair.
    for spec in EDGE_KINDS {
        let (src, dst) = legal_pair(&g, spec.kind);
        add_edge(&c, &have, src, spec.kind, dst)
            .unwrap_or_else(|e| panic!("`{}` {src} -> {dst} is legal and must land: {e:?}", spec.kind));
    }
    // edge-entry.schema.json's ten, plus `rejected_by` and `shows`.
    assert_eq!(g.edge_kinds.len(), 12, "the closed set's size");

    // A kind not in the set refuses, naming it: a retired kind, an inverse
    // spelling, one that never entered, and nonsense. AT THE DOOR the
    // compiler refuses first (`ladle/compile.rs`, the CREATE-edge path),
    // and that refusal is TAGGED `unknown_edge_kind` — so the
    // door half asserts the frozen tag, the refusal's pool and the kind its
    // prose names, and the validator half (`validate_part`, the
    // function the door calls for every part) asserts the same tag.
    for kind in ["governs", "traces", "blocked_by", "superseded_by", "contains", "continues", "blesses"] {
        assert!(!g.edge_kinds.contains_key(kind), "`{kind}` is outside the set");
        let w = refused(add_edge(&c, &have, "decision", kind, "region"));
        assert_door_refusal(&w, "unknown_edge_kind", &format!("edge kind `{kind}` is not declared"));
        assert_eq!(
            validator_tag(&g, &have, "decision", kind, "region"),
            Some("unknown_edge_kind"),
            "{kind} at the validator"
        );
    }

    // A kind IN the set with an illegal source, and with an illegal target —
    // the source's own cases (HEAD_GRAMMAR), then every kind's
    // first illegal source and target, enumerated from the data. An illegal
    // SOURCE is refused at the door by the compiler (tagged
    // `wrong_endpoint_type`, "edge kind `K` leaves ..."; the same
    // tag at the validator); an illegal TARGET by the validator at the door,
    // tagged.
    let bad: &[(&str, &str, &str, End)] = &[
        ("task", "serves", "directive", End::Source),       // serves leaves a milestone only
        ("milestone", "serves", "decision", End::Target),   // and arrives at a directive only
        ("decision", "supersedes", "constraint", End::Target), // same type only
        ("region", "in_region", "region", End::Source),     // a region never attests (R2)
        ("decision", "narrows", "region", End::Source),     // region-only kind (R2)
        ("decision", "relates", "decision", End::Source),   // content<->content relates refused
        ("directive", "depends_on", "task", End::Source),   // depends_on leaves task|milestone
        ("task", "blocks", "task", End::Source),            // blocks leaves issue|question|danger
        ("issue", "resolved_by", "issue", End::Target),     // resolved_by arrives at decision|task
        ("issue", "in_region", "decision", End::Target),    // in_region arrives at a region
        ("decision", "shows", "decision", End::Source),     // shows leaves a viewer only
        ("viewer", "shows", "region", End::Target),         // and arrives at a content type only
        ("viewer", "ref", "decision", End::Source),         // a viewer carries `shows` and nothing else
        ("region", "rejected_by", "region", End::Source),   // rejected_by leaves a content node
        ("task", "rejected_by", "decision", End::Target),   // and arrives at a region
    ];
    for (src, kind, dst, end) in bad {
        illegal_end_refused(&c, &g, &have, src, kind, dst, *end);
    }
    let mut enumerated = 0;
    for spec in EDGE_KINDS {
        let kdef = &g.edge_kinds[spec.kind];
        if let Some(src) = have.keys().find(|t| !kdef.leaves(t.as_str())) {
            let (_, dst) = legal_pair(&g, spec.kind);
            illegal_end_refused(&c, &g, &have, src, spec.kind, dst, End::Source);
            enumerated += 1;
        }
        let (src, _) = legal_pair(&g, spec.kind);
        if let Some(dst) = have
            .keys()
            .find(|d| !kdef.permits(&g.kind, d.as_str(), true, src))
        {
            illegal_end_refused(&c, &g, &have, src, spec.kind, dst, End::Target);
            enumerated += 1;
        }
    }
    // Twelve kinds, two ends each: 24. The viewer is an illegal
    // source and target for supersedes/derived_from/ref (a viewer carries
    // `shows` only), so every end of every kind has an illegal type.
    assert_eq!(enumerated, 24, "illegal ends the data leaves");
}

#[test]
fn the_edge_set_is_closed_and_each_kinds_ends_are_its_own_prefixes_stripped() {
    the_edge_set_is_closed(unprefixed(), "edge-stripped");
}

#[test]
fn the_edge_set_is_closed_and_each_kinds_ends_are_its_own_as_declared() {
    the_edge_set_is_closed(declared(), "edge-declared");
}

// ---------------------------------------------------------------------------
// the id prefix, at the validator the door calls (runs now)
// ---------------------------------------------------------------------------

/// Every type accepts `<its prefix>-1` and refuses another type's prefix
/// and the hash id the compiler mints today — judged by
/// `PoolGrammar::validate_part`, the function `Cauldron::submit` calls.
// RED: the validator's prefix arm disabled — "`constraint` refuses a hash
// id" failed. A wrong prefix IN THE DATA ("D" -> "DG") leaves this test
// green (it reads prefixes from the data); only a check against the source
// files catches it.
#[test]
fn every_type_accepts_its_own_prefix_and_refuses_anothers() {
    let g = declared();
    let mut prefixes = BTreeMap::new();
    for (t, tdef) in &g.node_types {
        prefixes.insert(t.clone(), tdef.id_prefix.clone().expect("every v6 type declares a prefix"));
    }
    assert_eq!(prefixes.len(), V6_TYPE_COUNT);
    let known = |id: &NodeId| -> Option<String> {
        match id.0.as_str() {
            "R-900" => Some("region".into()),
            "DIR-900" => Some("directive".into()),
            _ => None,
        }
    };
    let judge = |t: &str, id: &str| -> Option<&'static str> {
        let mut deltas = vec![Delta::CreateNode {
            id: NodeId(id.into()),
            node_type: t.into(),
            state: g.node_types[t].states.first().cloned(),
            fields: valid_fields(&g, t, &[]),
        }];
        for (k, _) in &required_for_mint(&g, t) {
            let dst = if k == "serves" { "DIR-900" } else { "R-900" };
            deltas.push(Delta::AddEdge {
                src: NodeId(id.into()),
                kind: k.clone(),
                dst: QualifiedNodeId { pool: PoolName::new(POOL), id: NodeId(dst.into()) },
                note: None,
                fields: Default::default(),
            });
        }
        let part = CompiledPart {
            pool: PoolName::new(POOL),
            schema_version: g.version,
            deltas,
            premises: vec![],
            far_versions: BTreeMap::new(),
        };
        match g.validate_part(&part, Some(0), &known, &FarSide::Landing) {
            Ok(()) => None,
            Err(e) => grammar_fault_tag(&only_witness(&e)),
        }
    };
    for (t, p) in &prefixes {
        assert_eq!(judge(t, &format!("{p}-1")), None, "`{t}` accepts `{p}-1`");
        assert_eq!(judge(t, "n0123456789abcdef"), Some("id_prefix_mismatch"), "`{t}` refuses a hash id");
        let other = prefixes.values().find(|q| *q != p).unwrap();
        assert_eq!(judge(t, &format!("{other}-1")), Some("id_prefix_mismatch"), "`{t}` refuses `{other}-1`");
    }
}

/// The grammar is well-formed as data (pinnable), and a cauldron holding it
/// opens beside the fixture workspace kind it shares.
#[test]
fn the_v6_grammar_is_well_formed_and_pins() {
    declared().check_wellformed().expect("the v6 grammar is well-formed");
    let c = fresh("pin", &declared());
    drop(c);
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn fresh(tag: &str, g: &PoolGrammar) -> Cauldron {
    let dir: PathBuf =
        test_scratch::tmp_path(format!("spine-grammar-v6-data-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Cauldron::create(&dir, &[g.clone()], SpineConfig::default()).expect("create the cauldron")
}

fn json(v: impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(v).expect("json")
}

/// A string unique across this process — the door is exactly-once over the
/// envelope's hash identity, so two identical mints would be ONE node.
fn unique(what: &str) -> String {
    static SERIAL: AtomicU32 = AtomicU32::new(0);
    format!("{what} {:06}", SERIAL.fetch_add(1, Ordering::SeqCst))
}

/// Exactly `n` characters, unique per call in its head (see [`unique`]).
fn filler(n: u64) -> String {
    let n = n as usize;
    let mut s = unique("x");
    s.truncate(n);
    while s.chars().count() < n {
        s.push('y');
    }
    // `unique` may be cut short by a small `n`; keep the length exact.
    assert_eq!(s.chars().count(), n);
    s
}

/// A value of each required field that the data admits; `overrides` win.
fn valid_fields(
    g: &PoolGrammar,
    t: &str,
    overrides: &[(&str, serde_json::Value)],
) -> BTreeMap<String, serde_json::Value> {
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
    for (f, v) in overrides {
        out.insert((*f).to_string(), v.clone());
    }
    out
}

/// The local types an edge kind may arrive at from `src`, from the data.
fn targets_of(g: &PoolGrammar, kind: &str, src: &str) -> Vec<String> {
    let kdef = &g.edge_kinds[kind];
    g.node_types
        .keys()
        .filter(|t| kdef.permits(&g.kind, t, true, src))
        .cloned()
        .collect()
}

fn targets_ready(g: &PoolGrammar, t: &str, have: &BTreeMap<String, NodeId>) -> bool {
    required_for_mint(g, t)
        .keys()
        .all(|k| targets_of(g, k, t).iter().any(|d| have.contains_key(d)))
}

/// The edges a plain mint of `t` carries: the AND form
/// (`required_edges_at_mint`) plus the FIRST one-of alternative's edges
/// (`in_region` on a content type). The first alternative demands
/// no field, so a plain mint needs nothing more; the second (`rejected_by`
/// + `searched`) is exercised by `grammar_v6_corrections.rs`.
fn required_for_mint(g: &PoolGrammar, t: &str) -> BTreeMap<String, u32> {
    let tdef = &g.node_types[t];
    let mut m = tdef.required_edges_at_mint.clone();
    if let Some(first) = tdef.required_at_mint_any_of.first() {
        assert!(first.fields.is_empty(), "`{t}`'s first alternative demands no field");
        m.extend(first.edges.clone());
    }
    m
}

/// Mint one `t` through the door: its required fields (or `overrides`), its
/// first state, and every required-at-mint edge toward an already-minted
/// target — except `omit`, which is left out on purpose.
fn mint(
    c: &Cauldron,
    g: &PoolGrammar,
    t: &str,
    have: &BTreeMap<String, NodeId>,
    overrides: &[(&str, serde_json::Value)],
    omit: Option<&String>,
) -> spine::error::Result<NodeId> {
    let mut fields = valid_fields(g, t, &[]);
    // Unique per call, so no two mints share an envelope identity.
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
    let stmt = format!(
        "{matches}CREATE (n: {POOL}/{t} {{ {} }}) {edges}RETURN n.id",
        props.join(", ")
    );
    let params: Vec<(&str, serde_json::Value)> =
        params.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    let id = c.submit(commit(&stmt, &params)).map(|o| returned_id(o, "n.id"))?;
    assert_minted_id_shape(g, t, &id);
    Ok(id)
}

/// Every id the door returns has the shape the grammar implies: a
/// prefixed type `<prefix>-<n>`, n a decimal serial from 1 (never the
/// provisional `<prefix>-~<hex>`); an unprefixed type the hash
/// form `n<16 hex>`. So the `_as_declared` guards prove serial ids at the
/// door on every mint, and the `_prefixes_stripped` ones prove the hash.
#[track_caller]
fn assert_minted_id_shape(g: &PoolGrammar, t: &str, id: &NodeId) {
    match &g.node_types[t].id_prefix {
        Some(p) => {
            let serial = id.0.strip_prefix(&format!("{p}-")).unwrap_or_else(|| panic!("`{t}` id {id:?}: not `{p}-<n>`"));
            assert!(
                !serial.is_empty() && serial.bytes().all(|b| b.is_ascii_digit()) && !serial.starts_with('0'),
                "`{t}` id {id:?}: `{p}-` then not a serial"
            );
        }
        None => assert!(
            id.0.len() == 17 && id.0.starts_with('n') && id.0[1..].bytes().all(|b| b.is_ascii_hexdigit()),
            "`{t}` id {id:?}: not the hash form `n<16 hex>`"
        ),
    }
}

/// The targets every required edge needs: a region and a directive.
fn seed(c: &Cauldron, g: &PoolGrammar) -> BTreeMap<String, NodeId> {
    let mut have = BTreeMap::new();
    let r = mint(c, g, "region", &have, &[], None).expect("seed a region");
    have.insert("region".to_string(), r);
    let d = mint(c, g, "directive", &have, &[], None).expect("seed a directive");
    have.insert("directive".to_string(), d);
    have
}

/// One node of every type.
fn seed_every_type(c: &Cauldron, g: &PoolGrammar) -> BTreeMap<String, NodeId> {
    let mut have = seed(c, g);
    for t in g.node_types.keys() {
        if !have.contains_key(t) {
            let id = mint(c, g, t, &have, &[], None).unwrap_or_else(|e| panic!("seed `{t}`: {e:?}"));
            have.insert(t.clone(), id);
        }
    }
    have
}

/// The first (source type, target type) the data permits for `kind`.
fn legal_pair<'g>(g: &'g PoolGrammar, kind: &str) -> (&'g str, &'g str) {
    for src in g.node_types.keys() {
        if !g.edge_kinds[kind].leaves(src) {
            continue;
        }
        if let Some(dst) = g.node_types.keys().find(|d| g.edge_kinds[kind].permits(&g.kind, d, true, src)) {
            return (src.as_str(), dst.as_str());
        }
    }
    panic!("`{kind}` has no legal local pair");
}

/// Add one edge `src_type -[kind]-> dst_type` between the seeded nodes.
fn add_edge(
    c: &Cauldron,
    have: &BTreeMap<String, NodeId>,
    src: &str,
    kind: &str,
    dst: &str,
) -> spine::error::Result<Outcome> {
    // A fresh supersedes target of the same type would need a second node;
    // a self-loop is a legal same-type edge and keeps the seed at one each.
    let (s, d) = (&have[src].0, &have[dst].0);
    // A kind that declares fields (`shows`) carries a valid value of
    // each REQUIRED one, read from the data, so only the end under test can
    // be wrong.
    let fields = edge_fields_for(kind);
    let note = if fields.is_empty() { String::new() } else { " NOTE $f".to_string() };
    let stmt = if s == d {
        format!("MATCH (a: {POOL}/{src} {{ id: $a }}) CREATE (a) -[{kind}]-> (a){note}")
    } else {
        format!(
            "MATCH (a: {POOL}/{src} {{ id: $a }}) MATCH (b: {POOL}/{dst} {{ id: $b }}) CREATE (a) -[{kind}]-> (b){note}"
        )
    };
    let mut params = vec![("a", json(s))];
    if s != d {
        params.push(("b", json(d)));
    }
    if !fields.is_empty() {
        params.push(("f", serde_json::Value::Object(fields.into_iter().collect())));
    }
    let out = c.submit(commit(&stmt, &params))?;
    match &out {
        Outcome::Committed { .. } => Ok(out),
        other => panic!("expected Committed or a refusal, got {other:?}"),
    }
}

/// A valid value of each REQUIRED field `kind` declares, from the data
/// (`grammar_v6::edge_fields`); empty for a kind outside the set.
fn edge_fields_for(kind: &str) -> BTreeMap<String, serde_json::Value> {
    let mut out = BTreeMap::new();
    for (f, fdef) in grammar_v6::edge_fields(kind) {
        if !fdef.required {
            continue;
        }
        let v = match fdef.kind {
            FieldKind::Text => json(filler(fdef.min_chars.unwrap_or(1).max(20).min(fdef.max_chars.unwrap_or(u64::MAX)))),
            FieldKind::Object => {
                serde_json::json!({ "thread": "00000000-0000-4000-8000-000000000001", "msg": 1 })
            }
            FieldKind::Integer => json(1),
            FieldKind::Boolean => json(true),
            FieldKind::Any => json("any"),
        };
        out.insert(f, v);
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Source,
    Target,
}

/// An edge with an illegal `end` is refused at the door AND at the
/// validator, each in its own form (see `the_edge_set_is_closed`).
fn illegal_end_refused(
    c: &Cauldron,
    g: &PoolGrammar,
    have: &BTreeMap<String, NodeId>,
    src: &str,
    kind: &str,
    dst: &str,
    end: End,
) {
    let w = refused(add_edge(c, have, src, kind, dst));
    match end {
        End::Source => assert_door_refusal(&w, "wrong_endpoint_type", &format!("edge kind `{kind}` leaves")),
        End::Target => {
            assert_eq!(grammar_fault_tag(&w), Some("wrong_endpoint_type"), "{src} -{kind}-> {dst}: {w:?}")
        }
    }
    assert_eq!(
        validator_tag(g, have, src, kind, dst),
        Some("wrong_endpoint_type"),
        "{src} -{kind}-> {dst} at the validator"
    );
}

/// The door's compile-stage refusal: a `GrammarViolation` stamped with this
/// pool, carrying the frozen `tag`, whose
/// message carries `needle` (which names the kind).
#[track_caller]
fn assert_door_refusal(w: &Witness, tag: &str, needle: &str) {
    assert_eq!(grammar_fault_tag(w), Some(tag), "the door's frozen tag: {w:?}");
    match w {
        Witness::GrammarViolation { pool, message, .. } => {
            assert_eq!(pool, &PoolName::new(POOL), "{w:?}");
            assert!(message.contains(needle), "expected `{needle}` in: {message}");
        }
        other => panic!("expected GrammarViolation naming `{needle}`, got {other:?}"),
    }
}

/// The tag `PoolGrammar::validate_part` gives one `src -[kind]-> dst` edge
/// between the seeded nodes (`None` = accepted).
fn validator_tag(
    g: &PoolGrammar,
    have: &BTreeMap<String, NodeId>,
    src: &str,
    kind: &str,
    dst: &str,
) -> Option<&'static str> {
    let by_id: BTreeMap<String, String> = have.iter().map(|(t, id)| (id.0.clone(), t.clone())).collect();
    let known = |id: &NodeId| by_id.get(&id.0).cloned();
    let part = CompiledPart {
        pool: PoolName::new(POOL),
        schema_version: g.version,
        deltas: vec![Delta::AddEdge {
            src: have[src].clone(),
            kind: kind.into(),
            dst: QualifiedNodeId { pool: PoolName::new(POOL), id: have[dst].clone() },
            note: None,
            // The kind's required fields, valid, as `add_edge` sends them.
            fields: edge_fields_for(kind),
        }],
        premises: vec![],
        far_versions: BTreeMap::new(),
    };
    match g.validate_part(&part, Some(0), &known, &FarSide::Landing) {
        Ok(()) => None,
        Err(e) => grammar_fault_tag(&only_witness(&e)),
    }
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
            initiator: Initiator::Agent {
                thread: "grammar-v6-data".into(),
                dispatch: "grammar-v6-door".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(unique("nonce")),
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

fn read(
    c: &Cauldron,
    statement: &str,
    params: &[(&str, serde_json::Value)],
) -> Vec<BTreeMap<String, serde_json::Value>> {
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
    rows.into_iter()
        .map(|r| serde_json::from_value(serde_json::to_value(r).unwrap()).unwrap())
        .collect()
}

fn count(c: &Cauldron, node_type: &str) -> usize {
    read(c, &format!("MATCH (n: {POOL}/{node_type}) RETURN n.id"), &[]).len()
}
