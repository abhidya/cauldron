//! Regions as a lock hierarchy (`spine::regions`): the rows every write
//! names, the region revision, and the guard a writer installs.
//!
//! The rows tests are carried from the region-lock line's own suite (the
//! table, a pool without regions, a region pool never derived blind); the
//! guard and revision tests are this line's: the lock that must hold across
//! a model call is a row in the store, checked by the guard on the one
//! writer's handle.

use serde_json::{json, Map, Value};
use spine::config::SpineConfig;
use spine::delta::{CompiledPart, CompiledPayload, Delta};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance, RippleKind, ENVELOPE_FORMAT_VERSION};
use spine::error::SpineError;
use spine::locks::LockMode;
use spine::pool::{NodeId, Pool};
use spine::regions::{self, RegionGuard, RegionRow};
use spine::schema_def::{EdgeKindDef, FieldDef, NodeTypeDef, PoolGrammar, PoolKind, PoolName};
use spine::spine_db::SPINE_FILE;
use spine::{Cauldron, Outcome};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const RP: &str = "fixture-regions";

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-regions-{name}-{}", std::process::id()))
}

/// The smallest grammar that carries the region vocabulary: a `region`
/// type, a `note` content type, the three membership kinds and one ordinary
/// kind (`links`).
fn region_grammar() -> PoolGrammar {
    let mut node_types = BTreeMap::new();
    node_types.insert(
        "region".to_string(),
        NodeTypeDef { fields: BTreeMap::from([("handle".to_string(), FieldDef::optional())]), states: vec![], count_cells: BTreeMap::new(), ..Default::default() },
    );
    node_types.insert(
        "note".to_string(),
        NodeTypeDef {
            fields: BTreeMap::from([("title".to_string(), FieldDef::required())]),
            states: vec!["open".to_string(), "closed".to_string()],
            count_cells: BTreeMap::new(),
            ..Default::default()
        },
    );
    let mut edge_kinds = BTreeMap::new();
    edge_kinds.insert("in_region".to_string(), EdgeKindDef::local("note", "region"));
    edge_kinds.insert("narrows".to_string(), EdgeKindDef::local("region", "region"));
    edge_kinds.insert("rejected_by".to_string(), EdgeKindDef::local("note", "region"));
    edge_kinds.insert("links".to_string(), EdgeKindDef::local("note", "note"));
    edge_kinds.insert("supersedes".to_string(), EdgeKindDef::local("note", "note"));
    PoolGrammar { pool: PoolName::new(RP), kind: PoolKind::new("regions"), version: 1, node_types, edge_kinds }
}

fn fresh(dir: &Path) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(dir, &[region_grammar()], SpineConfig::default()).expect("create the cauldron")
}

fn bindings(params: &[(&str, Value)]) -> Bindings {
    params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn envelope(nonce: &str, statement: &str, params: &[(&str, Value)], initiator: Initiator, mode: Mode) -> Envelope {
    Envelope {
        statement: statement.to_string(),
        bindings: bindings(params),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance { initiator, instrument: Instrument { name: "runner".into(), version: "test-build".into() } },
        mode,
        idempotency_nonce: Some(nonce.to_string()),
    }
}

fn outsider() -> Initiator {
    Initiator::CoreMechanical { operation: "test-outside".into() }
}

fn ripple(run: &str) -> Initiator {
    Initiator::Ripple { kind: RippleKind::TypeTwo, run_id: run.into(), step_id: "put".into(), scope: vec![] }
}

fn submit(c: &Cauldron, nonce: &str, statement: &str, params: &[(&str, Value)]) -> spine::error::Result<Outcome> {
    c.submit(envelope(nonce, statement, params, outsider(), Mode::Commit))
}

fn seed(c: &Cauldron, statement: &str, params: &[(&str, Value)], col: &str) -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    match submit(c, &format!("seed-{n}"), statement, params) {
        Ok(Outcome::Committed { returned, .. }) => returned.0[0][col].as_str().unwrap().to_string(),
        other => panic!("seed `{statement}`: {other:?}"),
    }
}

fn region(c: &Cauldron, h: &str) -> String {
    seed(c, &format!("CREATE (r: {RP}/region {{ handle: $h }}) RETURN r.id"), &[("h", json!(h))], "r.id")
}

fn region_under(c: &Cauldron, h: &str, parent: &str) -> String {
    seed(
        c,
        &format!("MATCH (p: {RP}/region {{ id: $p }}) CREATE (r: {RP}/region {{ handle: $h }}) CREATE (r) -[narrows]-> (p) RETURN r.id"),
        &[("h", json!(h)), ("p", json!(parent))],
        "r.id",
    )
}

fn note_in(c: &Cauldron, t: &str, r: &str) -> String {
    seed(
        c,
        &format!("MATCH (r: {RP}/region {{ id: $r }}) CREATE (n: {RP}/note {{ title: $t, state: \"open\" }}) CREATE (n) -[in_region]-> (r) RETURN n.id"),
        &[("t", json!(t)), ("r", json!(r))],
        "n.id",
    )
}

fn note_unplaced(c: &Cauldron, t: &str) -> String {
    seed(c, &format!("CREATE (n: {RP}/note {{ title: $t, state: \"open\" }}) RETURN n.id"), &[("t", json!(t))], "n.id")
}

const SET_NOTE: &str = "MATCH (n: fixture-regions/note { id: $id }) WHERE n.title = $was SET n.title = $t";
const SET_STATE: &str = "MATCH (n: fixture-regions/note { id: $id }) SET n.state = \"closed\"";
const SET_HANDLE: &str = "MATCH (r: fixture-regions/region { id: $id }) SET r.handle = $h";
const LINK: &str = "MATCH (a: fixture-regions/note { id: $a }) MATCH (b: fixture-regions/note { id: $b }) CREATE (a) -[links]-> (b)";
/// A carve: a new child CHILD of TOP, and member n moved from TOP to CHILD.
const CARVE: &str = "MATCH (p: fixture-regions/region { id: $p }) MATCH (n: fixture-regions/note { id: $n }) \
     CREATE (c: fixture-regions/region { handle: $h }) CREATE (c) -[narrows]-> (p) \
     REMOVE (n) -[in_region]-> (p) CREATE (n) -[in_region]-> (c) RETURN c.id";

/// TOP with children LEFT and RIGHT; OTHER a second top region; n1 in LEFT, n2 in RIGHT,
/// m in OTHER, n in TOP directly.
struct Tree {
    p: String,
    r1: String,
    r2: String,
    q: String,
    n1: String,
    n2: String,
    m: String,
    n: String,
}

fn tree(c: &Cauldron) -> Tree {
    let p = region(c, "TOP");
    let r1 = region_under(c, "LEFT", &p);
    let r2 = region_under(c, "RIGHT", &p);
    let q = region(c, "OTHER");
    let n1 = note_in(c, "n1", &r1);
    let n2 = note_in(c, "n2", &r2);
    let m = note_in(c, "m", &q);
    let n = note_in(c, "n", &p);
    Tree { p, r1, r2, q, n1, n2, m, n }
}

/// The rows one statement names, `region -> mode`, through the door's own
/// pipeline by hand over a real snapshot.
fn region_rows(c: &Cauldron, statement: &str, params: &[(&str, Value)]) -> BTreeMap<String, String> {
    let env = envelope("rows", statement, params, outsider(), Mode::Commit);
    let stmt = env.validate().expect("parse");
    let hash = env.hash_identity().expect("hash");
    let lookup = |p: &PoolName| -> Option<PoolGrammar> { c.grammar(p).cloned() };
    let kinds = |k: &PoolKind| -> Vec<PoolGrammar> { c.pools_of_kind(k).into_iter().filter_map(|n| c.grammar(n).cloned()).collect() };
    let pools: Vec<PoolName> = spine::ladle::compile::footprint(&stmt).pools.iter().cloned().collect();
    let open = spine::read::open_pools(c, &pools).expect("open");
    let snaps = spine::read::open_snapshots(&open).expect("snapshots");
    let mut fuel = spine::ladle::compile::Fuel::new(c.config().fuel_ceiling);
    let matched = spine::read::run_match(&snaps, &stmt, &env.bindings, &mut fuel).expect("match");
    let payload = {
        let mut ctx = spine::ladle::compile::CompileCtx { grammars: &lookup, pools_of_kind: &kinds, fuel: &mut fuel };
        spine::ladle::compile::compile(&stmt, &env.bindings, &matched, &hash, &mut ctx).expect("compile")
    };
    regions::rows_of(&payload, &lookup, &snaps).expect("rows").into_iter().map(|r| (r.region.0.clone(), r.mode_word().to_string())).collect()
}

/// The table, row by row: a SET on content names its regions and their
/// ancestors; a regionless SET names `~unplaced`; a header SET takes
/// `write` on the region and `link` on its ancestors only; an edge names
/// BOTH ends' regions; a mint into R names R and its ancestors; a
/// regionless mint names `~unplaced`; a region mint names its parent chain
/// and `write` on itself only through its own `narrows` edge; a carve takes
/// `write` on the new child, `link` on the parent; a re-parent takes
/// `write` on the child and `link` on both parent chains.
#[test]
fn regions_every_delta_names_the_rows_of_the_table() {
    let dir = root("table");
    let c = fresh(&dir);
    let t = tree(&c);
    let u = note_unplaced(&c, "u");
    let rows = |stmt: &str, params: &[(&str, Value)]| region_rows(&c, stmt, params);
    let x = |r: &str| (r.to_string(), "write".to_string());
    let ix = |r: &str| (r.to_string(), "link".to_string());
    let set = |v: Vec<(String, String)>| v.into_iter().collect::<BTreeMap<_, _>>();

    assert_eq!(rows(SET_NOTE, &[("id", json!(t.n1)), ("was", json!("n1")), ("t", json!("x"))]), set(vec![ix(&t.r1), ix(&t.p)]), "SET on n1 in LEFT under TOP");
    assert_eq!(rows(SET_NOTE, &[("id", json!(u)), ("was", json!("u")), ("t", json!("x"))]), set(vec![ix("~unplaced")]), "a regionless SET");
    assert_eq!(rows(SET_HANDLE, &[("id", json!(t.r1)), ("h", json!("x"))]), set(vec![x(&t.r1), ix(&t.p)]), "a header write");
    assert_eq!(rows(SET_HANDLE, &[("id", json!(t.p)), ("h", json!("x"))]), set(vec![x(&t.p)]), "a parentless header write");
    assert_eq!(rows(LINK, &[("a", json!(t.n1)), ("b", json!(t.m))]), set(vec![ix(&t.r1), ix(&t.p), ix(&t.q)]), "an ordinary edge: both ends");
    assert_eq!(
        rows(&format!("MATCH (r: {RP}/region {{ id: $r }}) CREATE (n: {RP}/note {{ title: \"t\", state: \"open\" }}) CREATE (n) -[in_region]-> (r)"), &[("r", json!(t.r2))]),
        set(vec![ix(&t.r2), ix(&t.p)]),
        "a content mint into RIGHT"
    );
    assert_eq!(rows(&format!("CREATE (n: {RP}/note {{ title: \"t\", state: \"open\" }})"), &[]), set(vec![ix("~unplaced")]), "a regionless mint");
    let mint_child = rows(&format!("MATCH (p: {RP}/region {{ id: $p }}) CREATE (r: {RP}/region {{ handle: \"h\" }}) CREATE (r) -[narrows]-> (p)"), &[("p", json!(t.r1))]);
    assert_eq!(mint_child.values().filter(|m| *m == "write").count(), 1, "write on the new child only: {mint_child:?}");
    assert_eq!(mint_child.get(&t.r1), Some(&"link".to_string()));
    assert_eq!(mint_child.get(&t.p), Some(&"link".to_string()));
    assert_eq!(mint_child.len(), 3, "{mint_child:?}");
    let carve_rows = rows(CARVE, &[("p", json!(t.p)), ("n", json!(t.n)), ("h", json!("CHILD"))]);
    assert_eq!(carve_rows.get(&t.p), Some(&"link".to_string()), "{carve_rows:?}");
    assert_eq!(carve_rows.values().filter(|m| *m == "write").count(), 1, "write on the new child: {carve_rows:?}");
    assert_eq!(carve_rows.len(), 2, "TOP and the new child, nothing else: {carve_rows:?}");
    let reparent = rows(&format!("MATCH (c: {RP}/region {{ id: $c }}) MATCH (q: {RP}/region {{ id: $q }}) CREATE (c) -[narrows]-> (q)"), &[("c", json!(t.r1)), ("q", json!(t.q))]);
    assert_eq!(reparent, set(vec![x(&t.r1), ix(&t.p), ix(&t.q)]), "a narrows change at LEFT");
    let _ = (&t.n2, &t.r2);
}

/// A pool WITHOUT regions: every write names `~unplaced` (`link`); a region
/// pool asked about with no snapshot is a loud `Hard`, never an empty set.
#[test]
fn regions_a_pool_without_regions_names_unplaced_and_a_region_pool_is_never_derived_blind() {
    let alpha = spine::schema_def::fixtures::fixture_alpha();
    let pool = PoolName::new("fixture-alpha");
    let part = CompiledPart {
        pool: pool.clone(),
        schema_version: 1,
        deltas: vec![Delta::SetField { node: NodeId("nX".into()), path: vec!["title".into()], value: json!("t") }],
        premises: vec![],
        far_versions: BTreeMap::new(),
    };
    let payload = CompiledPayload { parts: vec![part.clone()], minted: BTreeMap::new() };
    let grammars = |p: &PoolName| if *p == pool { Some(alpha.clone()) } else { None };
    let rows = regions::rows_of(&payload, &grammars, &regions::NoSnapshots).expect("rows");
    assert_eq!(rows, vec![RegionRow { pool: pool.clone(), region: regions::unplaced(), mode: LockMode::Link }]);
    let rg = region_grammar();
    let rpool = PoolName::new(RP);
    let rpayload = CompiledPayload { parts: vec![CompiledPart { pool: rpool.clone(), schema_version: 1, ..part }], minted: BTreeMap::new() };
    let rgrammars = |p: &PoolName| if *p == rpool { Some(rg.clone()) } else { None };
    match regions::rows_of(&rpayload, &rgrammars, &regions::NoSnapshots) {
        Err(SpineError::Hard { message }) => assert!(message.contains("never derived blind"), "{message}"),
        other => panic!("a region pool without a snapshot must be Hard, got {other:?}"),
    }
}

// ---------------------------------------------------------------- the guard

/// A guard that holds `held` for run `owner` (`write`) and records what it
/// was asked.
struct Holds {
    held: String,
    owner: String,
    asked: Mutex<Vec<(String, Vec<(String, String)>)>>,
}

impl RegionGuard for Holds {
    fn check(&self, initiator: &Initiator, rows: &[RegionRow]) -> Result<(), (String, Map<String, Value>)> {
        let who = match initiator {
            Initiator::Ripple { run_id, .. } => run_id.clone(),
            _ => "other".to_string(),
        };
        self.asked.lock().unwrap().push((who.clone(), rows.iter().map(|r| (r.region.0.clone(), r.mode_word().to_string())).collect()));
        if who != self.owner && rows.iter().any(|r| r.region.0 == self.held) {
            let mut m = Map::new();
            m.insert("region".into(), json!(self.held));
            m.insert("holder".into(), json!(self.owner));
            return Err((format!("region {} is held", self.held), m));
        }
        Ok(())
    }
}

fn transactions(dir: &Path) -> i64 {
    let conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).unwrap();
    conn.query_row("SELECT COUNT(*) FROM transactions", [], |r| r.get(0)).unwrap()
}

/// A commit into a held region is refused `region_locked` with the
/// guard's facts, and NO row is appended for it; one into the sibling
/// lands; the holder's own write into its region lands; a dry run is never
/// asked; a handle without a guard asks nobody.
#[test]
fn regions_the_guard_refuses_a_commit_into_a_held_region_before_any_row_and_never_a_dry_run() {
    let dir = root("guard");
    let mut c = fresh(&dir);
    let t = tree(&c);
    let guard = Arc::new(Holds { held: t.r1.clone(), owner: "run-a".into(), asked: Mutex::new(vec![]) });
    c.set_region_guard(guard.clone());
    let mint = format!("MATCH (r: {RP}/region {{ id: $r }}) CREATE (n: {RP}/note {{ title: $t, state: \"open\" }}) CREATE (n) -[in_region]-> (r) RETURN n.id");

    let before = transactions(&dir);
    match c.submit(envelope("w1", &mint, &[("r", json!(t.r1)), ("t", json!("x"))], outsider(), Mode::Commit)) {
        Err(e @ SpineError::RegionLocked { .. }) => {
            assert_eq!(e.kind(), "region_locked");
            assert_eq!(e.payload()["holder"], json!("run-a"), "{}", e.payload());
            assert_eq!(e.payload()["region"], json!(t.r1));
        }
        other => panic!("a write into the held region must be refused, got {other:?}"),
    }
    assert_eq!(transactions(&dir), before, "the refusal appended no row");
    // This guard holds LEFT alone (no row above it), so a header change of
    // the top region lands.
    assert!(matches!(submit(&c, "w2", SET_HANDLE, &[("id", json!(t.p)), ("h", json!("TOP2"))]), Ok(Outcome::Committed { .. })));
    // The sibling lands.
    assert!(matches!(c.submit(envelope("w3", &mint, &[("r", json!(t.r2)), ("t", json!("y"))], outsider(), Mode::Commit)), Ok(Outcome::Committed { .. })));
    // The holder lands into its own region.
    assert!(matches!(c.submit(envelope("w4", &mint, &[("r", json!(t.r1)), ("t", json!("z"))], ripple("run-a"), Mode::Commit)), Ok(Outcome::Committed { .. })));
    // A dry run is judged and never asked.
    let asked = guard.asked.lock().unwrap().len();
    assert!(matches!(c.submit(envelope("w5", &mint, &[("r", json!(t.r1)), ("t", json!("v"))], outsider(), Mode::Validate)), Ok(Outcome::Validated { .. })));
    assert_eq!(guard.asked.lock().unwrap().len(), asked, "a dry run never reaches the guard");
    // What the guard was told for the refused write: LEFT and TOP, link.
    let first = guard.asked.lock().unwrap()[0].clone();
    assert_eq!(first.0, "other");
    let told: std::collections::BTreeSet<(String, String)> = first.1.into_iter().collect();
    assert_eq!(told, [(t.p.clone(), "link".to_string()), (t.r1.clone(), "link".to_string())].into_iter().collect());
    // A handle opened without a guard asks nobody and lands.
    let plain = Cauldron::open(&dir, SpineConfig::default()).unwrap();
    assert!(plain.region_guard().is_none());
    assert!(matches!(plain.submit(envelope("w6", &mint, &[("r", json!(t.r1)), ("t", json!("w"))], outsider(), Mode::Commit)), Ok(Outcome::Committed { .. })));
    let _ = (&t.q, &t.n1, &t.n2, &t.m, &t.n);
}

// ------------------------------------------------------------- the revision

fn rev(dir: &Path, region: &str) -> Option<String> {
    let pool = Pool::open_read_only(&Pool::path_in(dir, &PoolName::new(RP))).unwrap();
    let snap = pool.snapshot().unwrap();
    regions::region_rev(&snap, &PoolName::new(RP), &region_grammar(), &NodeId(region.to_string())).unwrap()
}

/// The revision moves when the region's header, membership, members'
/// states, a member's superseding or the region's children change, and
/// NOT when a member's other field changes or another region changes; `~unplaced` has one; a node that is not a
/// region has none.
#[test]
fn regions_a_revision_moves_with_what_a_reader_of_the_region_sees_and_only_that() {
    let dir = root("rev");
    let c = fresh(&dir);
    let t = tree(&c);
    let r1 = rev(&dir, &t.r1).expect("LEFT has a revision");
    assert_eq!(rev(&dir, &t.r1).as_deref(), Some(r1.as_str()), "read twice, the same");
    assert_eq!(r1.len(), 16);
    // Another region's change leaves the left region's alone.
    note_in(&c, "elsewhere", &t.r2);
    assert_eq!(rev(&dir, &t.r1).as_deref(), Some(r1.as_str()), "a member added to RIGHT");
    // A member's title: not what the revision covers.
    assert!(matches!(submit(&c, "t1", SET_NOTE, &[("id", json!(t.n1)), ("was", json!("n1")), ("t", json!("n1b"))]), Ok(Outcome::Committed { .. })));
    assert_eq!(rev(&dir, &t.r1).as_deref(), Some(r1.as_str()), "a member's title changed");
    // A member's state: moves.
    assert!(matches!(submit(&c, "t2", SET_STATE, &[("id", json!(t.n1))]), Ok(Outcome::Committed { .. })));
    let r1b = rev(&dir, &t.r1).unwrap();
    assert_ne!(r1b, r1, "a member closed");
    // A new member: moves.
    note_in(&c, "new", &t.r1);
    let r1c = rev(&dir, &t.r1).unwrap();
    assert_ne!(r1c, r1b, "a member added");
    // Superseded by a note that lives in another region: moves.
    let newer = note_in(&c, "newer", &t.q);
    let before = rev(&dir, &t.r1).unwrap();
    assert!(matches!(
        submit(&c, "t2b", "MATCH (a: fixture-regions/note { id: $a }) MATCH (b: fixture-regions/note { id: $b }) CREATE (a) -[supersedes]-> (b)", &[("a", json!(newer)), ("b", json!(t.n1))]),
        Ok(Outcome::Committed { .. })
    ));
    assert_ne!(rev(&dir, &t.r1).unwrap(), before, "a member superseded from another region");
    // The header: moves.
    assert!(matches!(submit(&c, "t3", SET_HANDLE, &[("id", json!(t.r1)), ("h", json!("LEFT again"))]), Ok(Outcome::Committed { .. })));
    let r1d = rev(&dir, &t.r1).unwrap();
    assert_ne!(r1d, r1c, "the boundary rewritten");
    // A split of TOP: the top region's revision moves (a child, a member moved out).
    let p = rev(&dir, &t.p).unwrap();
    match submit(&c, "t4", CARVE, &[("p", json!(t.p)), ("n", json!(t.n)), ("h", json!("CHILD"))]) {
        Ok(Outcome::Committed { .. }) => {}
        other => panic!("the carve: {other:?}"),
    }
    assert_ne!(rev(&dir, &t.p).unwrap(), p, "TOP was split");
    // `~unplaced` has a revision that moves with a regionless node.
    let u = rev(&dir, regions::UNPLACED).unwrap();
    note_unplaced(&c, "u");
    assert_ne!(rev(&dir, regions::UNPLACED).unwrap(), u);
    // Not a region: none.
    assert_eq!(rev(&dir, &t.m), None);
    assert_eq!(rev(&dir, "R-404"), None);
    let _ = (&t.q, &t.n2);
}

fn head(dir: &Path, region: &str) -> Option<String> {
    let pool = Pool::open_read_only(&Pool::path_in(dir, &PoolName::new(RP))).unwrap();
    let snap = pool.snapshot().unwrap();
    regions::region_head_rev(&snap, &PoolName::new(RP), &NodeId(region.to_string())).unwrap()
}

/// The head revision covers a region's boundary and identity only: it
/// does NOT move when a member is added, closed or superseded, and it moves
/// when the header is rewritten or the region is split (a child); `~unplaced`
/// and a node that is not a region have none.
#[test]
fn regions_a_head_revision_moves_with_the_boundary_and_never_with_a_member() {
    let dir = root("head");
    let c = fresh(&dir);
    let t = tree(&c);
    let h1 = head(&dir, &t.r1).expect("LEFT has a head revision");
    assert_eq!(h1.len(), 16);
    note_in(&c, "a new member", &t.r1);
    assert!(matches!(submit(&c, "h2", SET_STATE, &[("id", json!(t.n1))]), Ok(Outcome::Committed { .. })));
    let newer = note_in(&c, "newer", &t.q);
    assert!(matches!(
        submit(&c, "h3", "MATCH (a: fixture-regions/note { id: $a }) MATCH (b: fixture-regions/note { id: $b }) CREATE (a) -[supersedes]-> (b)", &[("a", json!(newer)), ("b", json!(t.n1))]),
        Ok(Outcome::Committed { .. })
    ));
    assert_eq!(head(&dir, &t.r1).as_deref(), Some(h1.as_str()), "members added, closed or superseded leave the head alone");
    assert!(matches!(submit(&c, "h4", SET_HANDLE, &[("id", json!(t.r1)), ("h", json!("LEFT, rewritten"))]), Ok(Outcome::Committed { .. })));
    assert_ne!(head(&dir, &t.r1).unwrap(), h1, "the boundary rewritten");
    let p = head(&dir, &t.p).unwrap();
    match submit(&c, "h5", CARVE, &[("p", json!(t.p)), ("n", json!(t.n)), ("h", json!("CHILD"))]) {
        Ok(Outcome::Committed { .. }) => {}
        other => panic!("the carve: {other:?}"),
    }
    assert_ne!(head(&dir, &t.p).unwrap(), p, "a child region added");
    assert_eq!(head(&dir, regions::UNPLACED), None);
    assert_eq!(head(&dir, &t.m), None);
    assert_eq!(head(&dir, "R-404"), None);
}
