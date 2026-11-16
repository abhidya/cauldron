//! The v5-pool → soup import adapter BY REGENERATION
//! (`migrate::import_v5_pool`, `migrate::v5`), and the driver's
//! required-at-mint dry run.
//!
//! Every fixture pool here is written by the test into a scratch directory
//! as LITERAL v5 YAML (the shape a v5 pool's writer produces: one
//! `nodes/<id>.yaml` per node plus `manifest.yaml`), so what the adapter
//! parses is text this file spells out, not a serializer's round trip. The
//! destination is a scratch cauldron pinned to the REAL v6 grammar
//! (`grammar_v6::workspace_v6("ws")`) — the pool directory's basename is
//! `ws`, which is how the adapter names the destination pool.
//!
//! The REAL pool is read only by `real_pool`, which is `#[ignore]`d and
//! opt-in through `V5_POOL_DIR` (the default suite is hermetic: nothing here
//! knows a real pool path), and which never writes the directory it is
//! pointed at.
//!
//! Each guard names its forcings in a `// RED:` comment.

use serde_json::{json, Value};
use spine::checkers::{self, Verdict};
use spine::config::SpineConfig;
use spine::envelope::{Envelope, Initiator, Instrument, Mode, Provenance, RippleKind};
use spine::error::SpineError;
use spine::grammar_v6::workspace_v6;
use spine::migrate::{
    self, Disposition, ImportShape, Landing, MigrationPlan, MigrationRun, Resolution, RunOutcome,
    SourceEdge, SourceItem, SourceNode, MARKER_ABSENT_FROM_SOURCE, MARKER_ENDPOINT_REJECTED,
    REJECT_GRAMMAR,
};
use spine::pool::{NodeId, QualifiedNodeId};
use spine::schema_def::{grammar_fault_tag, PoolName};
use spine::spine_db::SPINE_FILE;
use spine::witness::Witness;
use spine::{Cauldron, Outcome};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

test_scratch::install!();

const WS: &str = "ws";
const BOARD: &str = "fixture-v5";
const COORD_THREAD: &str = "00000000-0000-4000-8000-000000000001";

/// The fixture's counts, as LITERALS (total accounting compares the manifest
/// with these; a number derived from the manifest would prove nothing).
/// 11 node files + 2 viewer roles.
const FIXTURE_NODES: usize = 13;
/// 18 v5 edge entries, of which Q-16's two `ref -> DEC-9` merge into one
/// item (edge-duplicate-triple-merged), plus 1 `shows`.
const FIXTURE_EDGES: usize = 17;
/// The two Q-3 entries toward `00000000-0000-4000-8000-000000000001#12` and `Q-3@blueprint`.
const FIXTURE_EXTERNAL: usize = 2;

// ---------------------------------------------------------------------------
// the fixture pool — literal v5 YAML
// ---------------------------------------------------------------------------

const R_1: &str = r#"schema_version: 5
id: R-1
type: region
handle: The import fixture's home region
boundary: 'Inside: every fixture node of the import tests. Outside: nothing else on this board.'
provenance:
  created: '2026-10-10T18:52:54Z'
  created_by: migration:batch-0
rev: 1
meta: {}
edges: []
"#;

const R_7: &str = r#"schema_version: 5
id: R-7
type: region
handle: A deferred fixture region
boundary: 'Inside: the deferred half of the fixture, woken by a decision. Outside: everything R-1 holds.'
deferred_by:
  thread: 00000000-0000-4000-8000-000000000002
  msg: 11
wake_when: the user decides on the fixture
provenance:
  created: '2026-10-10T18:53:00Z'
  created_by: migration:batch-0
rev: 4
field_revs:
  boundary: 3
  deferred_by: 4
meta: {}
edges:
- kind: narrows
  to: R-1
"#;

const Q_3: &str = r#"schema_version: 5
id: Q-3
type: question
state: open
handle: Does the import keep its external references?
body: |
  It cites [[00000000-0000-4000-8000-000000000001#12]] and descends from the v3 board's Q-3.
provenance:
  created: '2026-09-01T10:00:00Z'
  created_by: migration:batch-1
rev: 2
meta: {}
edges:
- kind: in_region
  to: R-1
  admits: 'every fixture node of the import tests'
- kind: ref
  to: 00000000-0000-4000-8000-000000000001#12
  sentence: The cited message asked for this.
- kind: derived_from
  to: Q-3@blueprint
"#;

const Q_65: &str = r#"schema_version: 5
id: Q-65
type: question
state: open
handle: Is the soup's write surface one transaction?
body: |
  The write surface needs a decision before it is built.
provenance:
  created: '2026-09-02T22:26:11Z'
  created_by: migration:batch-6
  operator:
    thread: 00000000-0000-4000-8000-000000000001
    msg: 12
rev: 1
meta:
  blocking: true
edges:
- kind: in_region
  to: R-7
  admits: 'the deferred half of the fixture'
"#;

const Q_16: &str = r#"schema_version: 5
id: Q-16
type: question
state: closed
handle: Was the counter advanced past the carried ids?
provenance:
  created: '2026-09-03T09:00:00Z'
  created_by: curator:round-12
rev: 3
field_revs:
  state: 3
  meta: 2
meta:
  blocking: false
edges:
- kind: in_region
  to: R-1
  admits: 'every fixture node of the import tests'
- kind: ref
  to: DEC-9
  sentence: DEC-9 decided where the counter advances.
- kind: ref
  to: DEC-9
  sentence: DEC-9 also names the flip.
"#;

const DEC_9: &str = r#"schema_version: 5
id: DEC-9
type: decision
state: revisited
handle: The counter advances in the decided flip
provenance:
  created: '2026-09-04T09:00:00Z'
  created_by: migration:batch-2
rev: 1
meta: {}
edges:
- kind: in_region
  to: R-1
  admits: 'every fixture node of the import tests'
"#;

const DEC_331: &str = r#"schema_version: 5
id: DEC-331
type: decision
state: decided
handle: The counter advances to the max carried id
provenance:
  created: '2026-10-11T09:00:00Z'
  created_by: curator:round-40
rev: 2
meta: {}
edges:
- kind: in_region
  to: R-1
  admits: 'every fixture node of the import tests'
- kind: supersedes
  to: DEC-9
  note: DEC-331 restates DEC-9 after the later decision
"#;

const DIR_2: &str = r#"schema_version: 5
id: DIR-2
type: directive
state: active
handle: Import the board by regeneration
provenance:
  created: '2026-10-01T09:00:00Z'
  created_by: migration:batch-0
  operator:
    thread: 00000000-0000-4000-8000-000000000001
    msg: 13
rev: 1
meta: {}
edges:
- kind: in_region
  to: R-1
  admits: 'every fixture node of the import tests'
"#;

const IS_4: &str = r#"schema_version: 5
id: IS-4
type: issue
state: open
handle: An issue that was migrated once before
provenance:
  created: '2026-09-05T09:00:00Z'
  created_by: migration:batch-3
rev: 1
meta:
  imported_from: IS-4@blueprint
  blocking: false
edges:
- kind: in_region
  to: R-1
  admits: 'every fixture node of the import tests'
- kind: ref
  to: Q-3
  note: the issue's question
  cite:
    thread: 00000000-0000-4000-8000-000000000001
    msg: 14
"#;

const T_5: &str = r#"schema_version: 5
id: T-5
type: task
state: depends-on
handle: A task whose v5 state v6 computes from its edge
provenance:
  created: '2026-09-06T09:00:00Z'
  created_by: migration:batch-4
rev: 1
meta: {}
edges:
- kind: in_region
  to: R-1
  admits: 'every fixture node of the import tests'
- kind: depends_on
  to: T-6
"#;

const T_6: &str = r#"schema_version: 5
id: T-6
type: task
state: ready
handle: The task T-5 waits on
provenance:
  created: '2026-09-06T09:01:00Z'
  created_by: migration:batch-4
rev: 1
meta: {}
edges:
- kind: in_region
  to: R-1
  admits: 'every fixture node of the import tests'
"#;

const MANIFEST: &str = r#"schema_version: 5
workspace:
  id: ws-fixture
  slug: fixture-v5
  status: active
viewers:
  curator:
    nodes: []
  coordinator:
    nodes:
    - id: Q-65
      cite:
        thread: 00000000-0000-4000-8000-000000000002
        msg: 15
      why: the fixture's start-here question
"#;

fn fixture_files() -> Vec<(&'static str, String)> {
    [
        ("R-1", R_1),
        ("R-7", R_7),
        ("Q-3", Q_3),
        ("Q-65", Q_65),
        ("Q-16", Q_16),
        ("DEC-9", DEC_9),
        ("DEC-331", DEC_331),
        ("DIR-2", DIR_2),
        ("IS-4", IS_4),
        ("T-5", T_5),
        ("T-6", T_6),
    ]
    .into_iter()
    .map(|(id, y)| (id, y.to_string()))
    .collect()
}

/// Write a v5 pool `<root>/ws` from `files` (id, YAML) and `manifest`.
fn write_pool(root: &Path, files: &[(&str, String)], manifest: &str) -> PathBuf {
    let pool = root.join(WS);
    let _ = std::fs::remove_dir_all(&pool);
    std::fs::create_dir_all(pool.join("nodes")).expect("mkdir nodes");
    for (id, yaml) in files {
        std::fs::write(pool.join("nodes").join(format!("{id}.yaml")), yaml).expect("write node");
    }
    std::fs::write(pool.join("manifest.yaml"), manifest).expect("write manifest");
    pool
}

fn scratch(name: &str) -> PathBuf {
    let d = test_scratch::tmp_path(format!("spine-import-v5-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("mkdir scratch");
    d
}

fn destination(root: &Path) -> Cauldron {
    Cauldron::create(&root.join("cauldron"), &[workspace_v6(WS)], SpineConfig::default())
        .expect("create the destination cauldron")
}

fn agent_plan() -> MigrationPlan {
    MigrationPlan::new(Initiator::Agent {
        thread: "import-v5".into(),
        dispatch: "import-v5".into(),
    })
}

fn ran(o: spine::error::Result<RunOutcome>) -> MigrationRun {
    o.expect("the import produces material")
        .expect_ran()
        .expect("the destination was empty")
}

fn import_fixture(name: &str) -> (PathBuf, Cauldron, MigrationRun) {
    let root = scratch(name);
    let pool = write_pool(&root, &fixture_files(), MANIFEST);
    let c = destination(&root);
    let run = ran(migrate::import_v5_pool(&c, &agent_plan(), &pool));
    (root, c, run)
}

fn view(c: &Cauldron, id: &str) -> Option<spine::read::NodeView> {
    c.node_view(&QualifiedNodeId {
        pool: PoolName::new(WS),
        id: NodeId(id.into()),
    })
    .expect("node_view")
}

fn node(c: &Cauldron, id: &str) -> spine::read::NodeView {
    view(c, id).unwrap_or_else(|| panic!("node {id} landed"))
}

fn field(v: &spine::read::NodeView, f: &str) -> Value {
    v.record
        .fields
        .get(f)
        .map(|x| x.value.clone())
        .unwrap_or(Value::Null)
}

fn green(v: &Verdict) {
    assert!(v.passed(), "checker `{}` fired: {:?}", v.checker, v.findings);
}

/// The witness's tag: a grammar fault's frozen tag, else its serde `kind`.
fn tag(w: &Witness) -> String {
    if let Witness::GrammarViolation { .. } = w {
        return grammar_fault_tag(w).unwrap_or("?").to_string();
    }
    serde_json::to_value(w).expect("witness serializes")["kind"]
        .as_str()
        .expect("kind")
        .to_string()
}

fn rejected_tags(run: &MigrationRun, source_id: &str) -> Vec<String> {
    match &run
        .manifest
        .entry(source_id)
        .unwrap_or_else(|| panic!("no entry for {source_id}"))
        .disposition
    {
        Disposition::Rejected { reason } => {
            assert_eq!(reason.code, REJECT_GRAMMAR, "{source_id}: {reason:?}");
            reason.witnesses.iter().map(tag).collect()
        }
        other => panic!("{source_id} should be rejected; got {other:?}"),
    }
}

/// Every manifest entry for an edge item leaving `src`.
fn edge_entries<'a>(run: &'a MigrationRun, src: &str) -> Vec<&'a migrate::ManifestEntry> {
    run.manifest
        .entries
        .iter()
        .filter(|e| match &e.disposition {
            Disposition::Imported {
                shape: ImportShape::Edge { src: s, .. },
                ..
            } => s == src,
            Disposition::Rejected { .. } => e.source_id.starts_with(&format!("{src}/")),
            _ => false,
        })
        .collect()
}

fn resolution(e: &migrate::ManifestEntry) -> &Resolution {
    match &e.disposition {
        Disposition::Imported { resolution, .. } => resolution,
        other => panic!("{} not imported: {other:?}", e.source_id),
    }
}

fn marker(e: &migrate::ManifestEntry) -> Option<&str> {
    match resolution(e) {
        Resolution::UnresolvedEndpoint { marker, .. } => Some(marker.as_str()),
        Resolution::Complete => None,
    }
}

fn landed_count(run: &MigrationRun) -> usize {
    match &run.landing {
        Landing::Committed { landed, .. } => landed.len(),
        other => panic!("expected a landing, got {other:?}"),
    }
}

fn serials(c_root: &Path) -> BTreeMap<String, i64> {
    let conn = rusqlite::Connection::open(c_root.join("cauldron").join(SPINE_FILE)).expect("spine");
    let has: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'id_serials'",
            [],
            |r| r.get(0),
        )
        .expect("schema");
    if has == 0 {
        return BTreeMap::new();
    }
    let mut st = conn
        .prepare("SELECT prefix, last FROM id_serials WHERE pool = ?1")
        .expect("prepare");
    let rows = st
        .query_map([WS], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    rows
}

fn the_one_attempt(c: &Cauldron, run: &MigrationRun) {
    let (tx, _) = run.landing.attempt().expect("it landed");
    let history = checkers::attempt_history(c, tx).expect("history");
    assert_eq!(history.rows.len(), 1, "one attempt, one row");
    green(&checkers::total_accounting(&run.manifest));
    let census = checkers::census(c).expect("census");
    green(&checkers::atomic_or_cleanly_aborted(&history, &census));
    green(&checkers::referential_closure(&census));
}

// ---------------------------------------------------------------------------
// the fixture lands — every node under its own id, in ONE attempt
// ---------------------------------------------------------------------------

/// The whole fixture through `import_v5_pool`: every node lands under its own
/// v5 id, the viewers are minted, every local edge lands, the two external
/// edges are counted, and it is ONE commit on ONE row.
// RED (the driver alone): pass one dry-running each node WITHOUT its
// required-at-mint edges — every content node Rejected
// `required_edge_missing`, only the two regions landed.
#[test]
fn the_fixture_lands_every_node_under_its_own_id_in_one_attempt() {
    let (root, c, run) = import_fixture("lands");
    assert_eq!(run.manifest.source_count, FIXTURE_NODES + FIXTURE_EDGES);
    assert_eq!(run.manifest.rejected(), 0, "{:?}", run.manifest);
    assert_eq!(landed_count(&run), FIXTURE_NODES);
    let unresolved = run
        .manifest
        .entries
        .iter()
        .filter(|e| matches!(&e.disposition, Disposition::Imported { resolution: Resolution::UnresolvedEndpoint { .. }, .. }))
        .count();
    assert_eq!(unresolved, FIXTURE_EXTERNAL);
    for (id, _) in fixture_files() {
        assert_eq!(node(&c, id).record.id.0, id, "{id} landed as itself");
    }
    let census = checkers::census(&c).expect("census");
    assert_eq!(census.node_count(), FIXTURE_NODES);
    assert_eq!(census.edge_count(), FIXTURE_EDGES - FIXTURE_EXTERNAL);
    let s = serials(&root);
    for (p, n) in [("R", 7), ("Q", 65), ("DEC", 331), ("DIR", 2), ("IS", 4), ("T", 6), ("V", 2)] {
        assert_eq!(s.get(p).copied(), Some(n), "id_serials {p}: {s:?}");
    }
    the_one_attempt(&c, &run);
}

// ---------------------------------------------------------------------------
// the dry run and the landing agree
// ---------------------------------------------------------------------------

fn snode(id: &str, ty: &str, state: Option<&str>, fields: Value) -> SourceItem {
    SourceItem::Node(SourceNode {
        source_id: id.into(),
        pool: PoolName::new(WS),
        node_type: ty.into(),
        state: state.map(str::to_string),
        carried_id: Some(id.into()),
        fields: fields
            .as_object()
            .expect("fields object")
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        transforms: Vec::new(),
    })
}

fn sedge(id: &str, kind: &str, src: &str, dst: &str, fields: Value) -> SourceItem {
    SourceItem::Edge(SourceEdge {
        source_id: id.into(),
        pool: PoolName::new(WS),
        kind: kind.into(),
        src: src.into(),
        dst: dst.into(),
        note: None,
        fields: fields
            .as_object()
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default(),
        transforms: Vec::new(),
    })
}

/// Pass one carries each node's REQUIRED-AT-MINT edges (in_region here)
/// toward endpoints ACCEPTED earlier in the pass, and nothing else: a
/// question whose only `in_region` points at a REJECTED region is rejected
/// `required_edge_missing` (the grammar's own verdict, which the landing
/// would give), a question homed in a good region lands even though its
/// non-required `ref` edge is faulty (the fault stays on the edge's item),
/// and the regions are judged before the content that needs them even
/// though they come LAST in source order.
// RED A (dry-run alone, the driver without its dependency order): Q-2 and Q-3 are both
// rejected `required_edge_missing`. RED B (opposing, dry-run with ALL
// outgoing edges): Q-3 is rejected for its `ref`'s `undeclared_edge_field`.
// RED C (no dependency order: source order): Q-2 is judged before R-1 and
// rejected `required_edge_missing`.
#[test]
fn the_dry_run_and_the_landing_agree_on_required_edges() {
    let root = scratch("b1");
    let c = destination(&root);
    let good = "Inside: every node of the dry-run fixture. Outside: nothing else.";
    let items = vec![
        snode("Q-1", "question", Some("open"), json!({"handle": "homed in a rejected region"})),
        snode("Q-2", "question", Some("open"), json!({"handle": "homed in a good region"})),
        snode("Q-3", "question", Some("open"), json!({"handle": "homed, with a faulty ref"})),
        sedge("e1", "in_region", "Q-1", "R-2", json!(null)),
        sedge("e2", "in_region", "Q-2", "R-1", json!(null)),
        sedge("e3", "in_region", "Q-3", "R-1", json!(null)),
        // `ref` declares no fields: an object NOTE is `undeclared_edge_field`.
        sedge("e4", "ref", "Q-3", "Q-2", json!({"label": "not a declared field"})),
        // Regions LAST in source order.
        snode("R-1", "region", None, json!({"handle": "good", "boundary": good})),
        snode("R-2", "region", None, json!({"handle": "bad", "boundary": "too short"})),
    ];
    let run = ran(migrate::run(&c, &agent_plan(), items));
    assert_eq!(rejected_tags(&run, "R-2"), vec!["under_floor"]);
    assert_eq!(rejected_tags(&run, "Q-1"), vec!["required_edge_missing"]);
    assert!(run.manifest.entry("Q-2").unwrap().lands(), "{:?}", run.manifest.entry("Q-2"));
    assert!(run.manifest.entry("Q-3").unwrap().lands(), "{:?}", run.manifest.entry("Q-3"));
    assert_eq!(rejected_tags(&run, "e4"), vec!["undeclared_edge_field"]);
    assert_eq!(
        marker(run.manifest.entry("e1").unwrap()),
        Some(MARKER_ENDPOINT_REJECTED)
    );
    // The landing agrees with the dry run: it commits, and holds exactly
    // what the manifest says lands.
    match &run.landing {
        Landing::Committed { landed, .. } => {
            let mut ids: Vec<&str> = landed.values().map(|q| q.id.0.as_str()).collect();
            ids.sort();
            assert_eq!(ids, vec!["Q-2", "Q-3", "R-1"]);
        }
        other => panic!("the landing must commit what the dry run accepted: {other:?}"),
    }
    assert!(view(&c, "Q-1").is_none());
    assert_eq!(node(&c, "Q-3").out.len(), 1, "only the in_region; the faulty ref did not land");
}

// ---------------------------------------------------------------------------
// guard 4: a second import is a named outcome
// ---------------------------------------------------------------------------

/// Same fixture again: the default plan refuses before any dry run, naming
/// the pool and its node count; told otherwise, the viewer roles collide
/// and the import refuses naming the role; with the viewers emptied, every
/// node is a `duplicate_node_id` rejection, every edge `endpoint-rejected`,
/// and nothing is submitted; an overlapping corpus lands only what is new
/// and advances the counter to it.
// RED: drop the carried-id collision check (exec::check_carried_ids_absent)
// — the re-run's regions pass the dry run and the landing refuses AFTER the
// decided flip: `Hard { SPINE INTERNAL INVARIANT VIOLATED: a pool landing
// REFUSED a part AFTER the decided flip }`.
#[test]
fn a_second_import_is_a_named_outcome() {
    let (root, c, _first) = import_fixture("second");
    let pool = root.join(WS);
    let counter = c.committed_commit_counter().expect("counter");
    let before = serials(&root);

    match migrate::import_v5_pool(&c, &agent_plan(), &pool).expect("material") {
        RunOutcome::RefusedPopulatedDestination { populated } => {
            assert_eq!(populated.len(), 1);
            assert_eq!(populated[0].pool, PoolName::new(WS));
            assert_eq!(populated[0].nodes, FIXTURE_NODES);
        }
        other => panic!("a re-run refuses by default: {other:?}"),
    }
    assert_eq!(serials(&root), before);

    let mut allow = agent_plan();
    allow.allow_populated_destination = true;
    let err = migrate::import_v5_pool(&c, &allow, &pool).expect_err("viewer roles collide");
    assert!(matches!(err, SpineError::Usage { .. }), "{err:?}");
    assert!(err.message().contains("`coordinator`"), "{}", err.message());

    // Viewers emptied: every carried node is a duplicate.
    write_pool(&root, &fixture_files(), "schema_version: 5\nworkspace:\n  slug: fixture-v5\nviewers: {}\n");
    let run = ran(migrate::import_v5_pool(&c, &allow, &pool));
    // Every carried node is rejected. A REGION (no required-at-mint edge)
    // answers `duplicate_node_id`; a CONTENT node's every required edge
    // points at a region that was itself rejected, so pass one dry-runs it
    // with no `in_region` and the grammar's required-at-mint judgment
    // (compile, phase 7) answers before the carried-id collision check
    // (phase 10b) is reached: `required_edge_missing`. Both are the door's
    // own refusals, not "every node duplicate_node_id".
    for (id, _) in fixture_files() {
        let expected = if id.starts_with("R-") { "duplicate_node_id" } else { "required_edge_missing" };
        assert_eq!(rejected_tags(&run, id), vec![expected], "{id}");
    }
    for e in &run.manifest.entries {
        // Every edge's NEAR end is a rejected node, and the driver reports
        // the near end first — external targets included.
        if let Disposition::Imported { shape: ImportShape::Edge { .. }, .. } = &e.disposition {
            assert_eq!(marker(e), Some(MARKER_ENDPOINT_REJECTED), "{}", e.source_id);
        }
    }
    assert_eq!(run.landing, Landing::NothingAccepted);
    assert_eq!(c.committed_commit_counter().expect("counter"), counter, "no row");
    assert_eq!(serials(&root), before);

    // Overlap: R-7 and Q-65 again, plus a NEW region R-8 and a new Q-200
    // homed in it. (A new node homed in an ALREADY-imported region cannot
    // land through this driver: the corpus's copy of that region is a
    // duplicate, and the driver never MATCHes destination nodes — that is a
    // separate delta pass's job.)
    let r8 = R_7.replace("id: R-7", "id: R-8").replace("- kind: narrows\n  to: R-1\n", "");
    let q200 = Q_65
        .replace("id: Q-65", "id: Q-200")
        .replace("blocking: true", "blocking: false")
        .replace("to: R-7", "to: R-8");
    write_pool(
        &root,
        &[("R-7", R_7.to_string()), ("Q-65", Q_65.to_string()), ("R-8", r8), ("Q-200", q200)],
        "schema_version: 5\nworkspace:\n  slug: fixture-v5\n",
    );
    let run = ran(migrate::import_v5_pool(&c, &allow, &pool));
    assert_eq!(rejected_tags(&run, "R-7"), vec!["duplicate_node_id"]);
    assert_eq!(rejected_tags(&run, "Q-65"), vec!["required_edge_missing"]);
    assert!(run.manifest.entry("R-8").unwrap().lands());
    assert!(run.manifest.entry("Q-200").unwrap().lands());
    assert_eq!(node(&c, "Q-200").record.id.0, "Q-200");
    assert_eq!(serials(&root)["Q"], 200);
}

// ---------------------------------------------------------------------------
// guard 5 (the e2e half): flags land under the Agent arm; the ripple
// negatives hold on the SAME fields; a non-Agent plan is refused
// ---------------------------------------------------------------------------

fn ripple_env(kind: RippleKind, step: &str, scope: &[&str], statement: &str, params: Value) -> Envelope {
    Envelope {
        statement: statement.into(),
        bindings: params
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Ripple {
                kind,
                run_id: format!("run-of-{step}"),
                step_id: step.into(),
                scope: scope.iter().map(|s| s.to_string()).collect(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(step.into()),
    }
}

fn flag_witness(r: spine::error::Result<Outcome>) -> Witness {
    match r {
        Err(SpineError::Refused { witnesses }) => {
            assert_eq!(witnesses.len(), 1, "{witnesses:?}");
            witnesses[0].clone()
        }
        other => panic!("expected flag_not_writable, got {other:?}"),
    }
}

/// After the fixture import under an `Agent` plan, the flag fields it wrote
/// read back (`Q-65.meta.blocking == true`, `Q-16`'s `false` present,
/// `R-7.deferred_by` equal to the file). On the same cauldron a type-one
/// ripple may not SET `Q-65.meta.blocking`, a type-two ripple scoped to
/// `meta.stop` may not SET `R-7.deferred_by`, and the same SET scoped to
/// `deferred_by` lands (the positive sentinel).
// RED A: delete flags.rs's Agent arm (Agent falls to the ripple judgment) —
// the import's `Q-65` item is Rejected `flag_not_writable` and the
// `node(&c, "Q-65")` read panics. RED B (opposing): `check_flag_grants`
// returns Ok(()) always — the type-one SET lands.
#[test]
fn flags_land_under_the_agent_arm_and_ripples_are_refused_on_the_same_fields() {
    let (_root, c, run) = import_fixture("flags");
    assert_eq!(run.manifest.rejected(), 0);
    assert_eq!(field(&node(&c, "Q-65"), "meta")["blocking"], json!(true));
    let q16 = field(&node(&c, "Q-16"), "meta");
    assert_eq!(q16.get("blocking"), Some(&json!(false)), "the key is present");
    let deferred = json!({"thread": "00000000-0000-4000-8000-000000000002", "msg": 11});
    assert_eq!(field(&node(&c, "R-7"), "deferred_by"), deferred);

    let w = flag_witness(c.submit(ripple_env(
        RippleKind::TypeOne,
        "t1",
        &[],
        "MATCH (q: ws/question { id: $id }) SET q.meta.blocking = $v",
        json!({"id": "Q-65", "v": false}),
    )));
    match &w {
        Witness::FlagNotWritable { field, initiator_kind, .. } => {
            assert_eq!(field, "meta.blocking");
            assert_eq!(initiator_kind, "type_one");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(field(&node(&c, "Q-65"), "meta")["blocking"], json!(true), "unchanged");

    let set_deferred = "MATCH (r: ws/region { id: $id }) SET r.deferred_by = $v";
    let new = json!({"thread": "00000000-0000-4000-8000-000000000001", "msg": 999});
    let w = flag_witness(c.submit(ripple_env(
        RippleKind::TypeTwo,
        "t2-out",
        &["meta.stop"],
        set_deferred,
        json!({"id": "R-7", "v": new}),
    )));
    match &w {
        Witness::FlagNotWritable { field, initiator_kind, scope, .. } => {
            assert_eq!(field, "deferred_by");
            assert_eq!(initiator_kind, "type_two");
            assert_eq!(scope.as_deref(), Some(&["meta.stop".to_string()][..]));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(field(&node(&c, "R-7"), "deferred_by"), deferred, "unchanged");

    c.submit(ripple_env(
        RippleKind::TypeTwo,
        "t2-in",
        &["deferred_by"],
        set_deferred,
        json!({"id": "R-7", "v": new}),
    ))
    .expect("in scope, the same SET lands");
    assert_eq!(field(&node(&c, "R-7"), "deferred_by"), new);
}

/// `import_v5_pool` takes an `Agent` plan only: a
/// `CoreMechanical` plan, and a ripple plan, are refused as usage naming the
/// rule, before anything is dry-run or submitted.
// RED C: let the adapter accept `CoreMechanical` — the import lands and the
// row's initiator reads `core_mechanical`; this test's `expect_err` fails.
#[test]
fn a_non_agent_plan_is_refused_before_any_dry_run() {
    let root = scratch("non-agent");
    let pool = write_pool(&root, &fixture_files(), MANIFEST);
    let c = destination(&root);
    let counter = c.committed_commit_counter().expect("counter");
    for initiator in [
        Initiator::CoreMechanical { operation: "import".into() },
        Initiator::Ripple {
            kind: RippleKind::TypeTwo,
            run_id: "r".into(),
            step_id: "s".into(),
            scope: vec![],
        },
    ] {
        let err = migrate::import_v5_pool(&c, &MigrationPlan::new(initiator.clone()), &pool)
            .expect_err("only an Agent plan imports");
        assert!(matches!(err, SpineError::Usage { .. }), "{err:?}");
        assert!(err.message().contains("the import's initiator is `Agent`"), "{}", err.message());
    }
    assert_eq!(c.committed_commit_counter().expect("counter"), counter);
    assert!(view(&c, "R-1").is_none(), "nothing landed");
}

// ---------------------------------------------------------------------------
// guard 6: a malformed v5 node is refused by name and the rest lands
// ---------------------------------------------------------------------------

/// One poison per run, each in a fresh scratch: exactly that item is
/// Rejected with the grammar's own tag, its edges are `endpoint-rejected`,
/// every other node lands in ONE attempt, total accounting green.
// RED: skip pass one's dry run (every node judged Imported) — the poisoned
// item is `Imported`, not Rejected, and the assertion on its tag fires.
#[test]
fn each_poison_is_rejected_by_name_and_the_rest_lands() {
    let long_body = format!("body: |\n  {}\n", "x".repeat(10_001));
    // (name, the poisoned file's id, the replacement files, the expected tag)
    let poisons: Vec<(&str, &str, Vec<(&str, String)>, &str)> = vec![
        ("a-over-cap", "Q-16", vec![("Q-16", Q_16.replace("rev: 3\n", &format!("rev: 3\n{long_body}")))], "over_cap"),
        ("b-state", "Q-16", vec![("Q-16", Q_16.replace("state: closed", "state: archived"))], "unknown_state"),
        ("c-prefix", "DEC-9", vec![("DEC-9", Q_16.replace("id: Q-16", "id: DEC-9"))], "id_prefix_mismatch"),
        ("d1-provisional", "Q-~abc", vec![("Q-~abc", Q_16.replace("id: Q-16", "id: Q-~abc"))], "carried_id_not_serial"),
        ("d2-letters", "Q-abc", vec![("Q-abc", Q_16.replace("id: Q-16", "id: Q-abc"))], "carried_id_not_serial"),
        ("d3-zero", "Q-0", vec![("Q-0", Q_16.replace("id: Q-16", "id: Q-0"))], "carried_id_not_serial"),
        ("e-floor", "R-7", vec![("R-7", R_7.replace(
            "boundary: 'Inside: the deferred half of the fixture, woken by a decision. Outside: everything R-1 holds.'",
            &format!("boundary: '{}'", "b".repeat(39)),
        ))], "under_floor"),
        ("f-operator", "DIR-2", vec![("DIR-2", DIR_2.replace(
            "  operator:\n    thread: 00000000-0000-4000-8000-000000000001\n    msg: 13\n", "",
        ))], "missing_required_field"),
    ];
    for (name, poisoned, replace, expected) in poisons {
        let root = scratch(&format!("poison-{name}"));
        let mut files = fixture_files();
        for (id, yaml) in replace {
            if let Some(slot) = files.iter_mut().find(|(i, _)| *i == id) {
                slot.1 = yaml;
            } else {
                // c: DEC-9's file now says `type: question` — replace it, and
                // d*: an ADDED file beside the clean fixture.
                files.push((id, yaml));
            }
        }
        let pool = write_pool(&root, &files, MANIFEST);
        let c = destination(&root);
        let run = ran(migrate::import_v5_pool(&c, &agent_plan(), &pool));
        let tags = rejected_tags(&run, poisoned);
        assert_eq!(tags, vec![expected.to_string()], "{name}");
        if let Disposition::Rejected { reason } = &run.manifest.entry(poisoned).unwrap().disposition {
            if expected == "carried_id_not_serial" {
                let Witness::GrammarViolation { message, .. } = &reason.witnesses[0] else {
                    panic!("{name}: {reason:?}")
                };
                assert!(message.contains(poisoned), "{name}: the detail names the id: {message}");
            }
        }
        assert_eq!(run.manifest.rejected(), if expected == "under_floor" { 2 } else { 1 },
            "{name}: exactly that item (R-7's member Q-65 falls with it)");
        for e in edge_entries(&run, poisoned) {
            if let Disposition::Imported { shape: ImportShape::Edge { dst, .. }, .. } = &e.disposition {
                if !dst.contains('#') && !dst.contains('@') {
                    assert_eq!(marker(e), Some(MARKER_ENDPOINT_REJECTED), "{name}: {}", e.source_id);
                }
            }
        }
        // Every OTHER node landed.
        for (id, _) in &files {
            if *id == poisoned || (expected == "under_floor" && *id == "Q-65") {
                continue;
            }
            assert!(view(&c, id).is_some(), "{name}: {id} landed");
        }
        the_one_attempt(&c, &run);
    }
}

/// A `shows` entry whose `why` is 236 characters is judged on the `shows`
/// edge item (`edge_field_over_cap`) — the viewer and every node land.
#[test]
fn a_shows_why_over_cap_is_rejected_on_the_edge_item() {
    let root = scratch("poison-g-why");
    let manifest = MANIFEST.replace(
        "why: the fixture's start-here question",
        &format!("why: {}", "w".repeat(236)),
    );
    let pool = write_pool(&root, &fixture_files(), &manifest);
    let c = destination(&root);
    let run = ran(migrate::import_v5_pool(&c, &agent_plan(), &pool));
    assert_eq!(rejected_tags(&run, "viewer:coordinator/shows/0"), vec!["edge_field_over_cap"]);
    assert_eq!(run.manifest.rejected(), 1);
    assert_eq!(landed_count(&run), FIXTURE_NODES);
    the_one_attempt(&c, &run);
}

// ---------------------------------------------------------------------------
// guard 7: external endpoints are counted, never resolved; the record keeps
// them
// ---------------------------------------------------------------------------

/// The `<thread>#<msg>` ref and the `@blueprint` derived_from are
/// `endpoint-absent-from-source`, store nothing, and are kept verbatim in
/// `Q-3.meta.imported_from.unresolved_edges`; no edge anywhere is a
/// self-loop.
// RED: strip `@board` and resolve locally — `Q-3 derived_from Q-3` is
// stored and the self-loop assertion fires.
#[test]
fn external_endpoints_are_counted_never_resolved() {
    let (_root, c, run) = import_fixture("external");
    let q3 = edge_entries(&run, "Q-3");
    let absent: Vec<&str> = q3
        .iter()
        .filter(|e| marker(e) == Some(MARKER_ABSENT_FROM_SOURCE))
        .map(|e| match &e.disposition {
            Disposition::Imported { shape: ImportShape::Edge { dst, .. }, .. } => dst.as_str(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(absent, vec!["00000000-0000-4000-8000-000000000001#12", "Q-3@blueprint"]);
    let v = node(&c, "Q-3");
    assert_eq!(v.out.len(), 1, "only the in_region is stored: {:?}", v.out);
    assert_eq!(
        field(&v, "meta")["imported_from"]["unresolved_edges"],
        json!([
            {"kind": "ref", "to": "00000000-0000-4000-8000-000000000001#12", "sentence": "The cited message asked for this."},
            {"kind": "derived_from", "to": "Q-3@blueprint"},
        ])
    );
    let census = checkers::census(&c).expect("census");
    for (id, _) in fixture_files() {
        for e in node(&c, id).out {
            assert_ne!(e.dst.id.0, id, "a self-loop {id} -[{}]-> {id}", e.kind);
        }
    }
    assert_eq!(census.edge_count(), FIXTURE_EDGES - FIXTURE_EXTERNAL);
}

// ---------------------------------------------------------------------------
// guard 8: the import record and the edge note are verbatim; ordinary fields
// are where v6 homes them
// ---------------------------------------------------------------------------

// RED A: put `rev` under `meta.v5` instead — the record equality fails.
// RED B (opposing): send `provenance.operator` under
// `meta.imported_from.operator` — DIR-2 is Rejected `missing_required_field`
// (the grammar judges it) and the `node(&c, "DIR-2")` read panics.
// RED C: drop the NOTE from the edge clause — `note == None`.
#[test]
fn the_import_record_and_the_edge_notes_are_verbatim() {
    let (_root, c, _run) = import_fixture("record");
    let q16 = node(&c, "Q-16");
    assert_eq!(
        field(&q16, "meta"),
        json!({
            "blocking": false,
            "imported_from": {
                "board": BOARD,
                "v5_id": "Q-16",
                "origin": "Q-16@fixture-v5",
                "rev": 3,
                "created": "2026-09-03T09:00:00Z",
                "created_by": "curator:round-12",
                "field_revs": {"state": 3, "meta": 2},
                "schema_version": 5,
                "admits_by_edge": {"R-1": "every fixture node of the import tests"},
                "unresolved_edges": [],
            }
        })
    );
    assert_eq!(q16.record.state.as_deref(), Some("closed"));
    assert_eq!(field(&q16, "handle"), json!("Was the counter advanced past the carried ids?"));

    let q65 = node(&c, "Q-65");
    assert_eq!(field(&q65, "operator"), json!({"thread": COORD_THREAD, "msg": 12}));
    let meta = field(&q65, "meta");
    for k in ["v5", "rev", "created", "created_by", "operator"] {
        assert!(meta.get(k).is_none(), "Q-65.meta.{k} must not exist: {meta}");
    }
    assert_eq!(meta["imported_from"]["origin"], json!("Q-65@fixture-v5"));
    assert!(meta["imported_from"].get("field_revs").is_none(), "only when present");
    let ir = &q65.out[0];
    assert_eq!(ir.kind, "in_region");
    assert_eq!(ir.dst.id.0, "R-7");
    assert_eq!(ir.note.as_deref(), Some("the deferred half of the fixture"));
    assert!(ir.fields.is_empty());

    assert_eq!(field(&node(&c, "DIR-2"), "operator"), json!({"thread": COORD_THREAD, "msg": 13}));

    let dec331 = node(&c, "DEC-331");
    let sup = dec331.out.iter().find(|e| e.kind == "supersedes").expect("supersedes");
    assert_eq!(sup.dst.id.0, "DEC-9");
    assert_eq!(sup.note.as_deref(), Some("DEC-331 restates DEC-9 after the later decision"));

    // A pre-existing meta.imported_from is nested as `prior`, never lost.
    let is4 = field(&node(&c, "IS-4"), "meta");
    assert_eq!(is4["imported_from"]["prior"], json!("IS-4@blueprint"));
    assert_eq!(is4["imported_from"]["v5_id"], json!("IS-4"));
    assert!(is4["imported_from"].get("id").is_none(), "the source id is `v5_id`");
    assert_eq!(is4["blocking"], json!(false), "ordinary meta on an issue");
    // An edge with two text attributes: joined, labelled, fixed order.
    let r = node(&c, "IS-4").out.into_iter().find(|e| e.kind == "ref").expect("ref");
    assert_eq!(
        r.note.as_deref(),
        Some("note: the issue's question\ncite: {\"msg\":14,\"thread\":\"00000000-0000-4000-8000-000000000001\"}")
    );
    // Two entries of one triple: one edge, both sentences kept.
    let refs: Vec<_> = q16.out.iter().filter(|e| e.kind == "ref").collect();
    assert_eq!(refs.len(), 1);
    assert_eq!(
        refs[0].note.as_deref(),
        Some("sentence: DEC-9 decided where the counter advances.\nsentence: DEC-9 also names the flip.")
    );
    // A v5 state v6 computes from edges lands stateless, its edge imported.
    let t5 = node(&c, "T-5");
    assert_eq!(t5.record.state, None);
    assert!(t5.out.iter().any(|e| e.kind == "depends_on" && e.dst.id.0 == "T-6"));
    assert_eq!(node(&c, "T-6").record.state.as_deref(), Some("ready"));
    // The regions' own fields.
    let r7 = node(&c, "R-7");
    assert_eq!(field(&r7, "wake_when"), json!("the user decides on the fixture"));
    assert_eq!(r7.out[0].kind, "narrows");
    assert_eq!(r7.out[0].note, None, "a v5 edge with no text has no note");
}

/// Every node's `meta.imported_from.admits_by_edge`
/// holds each of its `in_region` edges' v5 `admits:` span VERBATIM keyed by
/// destination id. The EXPECTED side is parsed from the fixture YAML (the
/// file the adapter read), never from the store, and one literal (Q-65)
/// pins the shape independently of that parse.
// RED D: drop one entry (the adapter skips the FIRST in_region entry of a
// node) — the equality fails. RED E (opposing): build `admits_by_edge` from
// the edge item's `note` after a NOTE-dropping forcing — the record reads
// `{}` and must still equal the SOURCE file, so it fails.
#[test]
fn the_record_carries_every_admits_span_by_destination() {
    let (_root, c, _run) = import_fixture("admits-by-edge");
    let mut total = 0usize;
    for (id, yaml) in fixture_files() {
        let doc: Value = serde_yaml::from_str(&yaml).expect("fixture parses");
        let want: serde_json::Map<String, Value> = doc["edges"]
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter(|e| e["kind"] == json!("in_region"))
            .map(|e| (e["to"].as_str().unwrap().to_string(), e["admits"].clone()))
            .collect();
        total += want.len();
        let rec = &field(&node(&c, id), "meta")["imported_from"];
        assert_eq!(rec["v5_id"], json!(id));
        assert_eq!(rec["admits_by_edge"], Value::Object(want), "{id}");
    }
    assert_eq!(total, 9, "the fixture's nine in_region entries");
    assert_eq!(
        field(&node(&c, "Q-65"), "meta")["imported_from"]["admits_by_edge"],
        json!({"R-7": "the deferred half of the fixture"})
    );
    // The edge's note keeps its copy (the edge's free text).
    assert_eq!(node(&c, "Q-65").out[0].note.as_deref(), Some("the deferred half of the fixture"));
}

/// The adapter's items, read directly: the names of its transforms are on
/// the items they describe.
#[test]
fn the_adapter_names_every_transform_on_its_item() {
    let root = scratch("transforms");
    let pool = write_pool(&root, &fixture_files(), MANIFEST);
    let items = migrate::v5::source_items(&pool).expect("read the fixture");
    assert_eq!(items.len(), FIXTURE_NODES + FIXTURE_EDGES);
    let names = |id: &str| -> Vec<String> {
        items
            .iter()
            .find(|i| i.source_id() == id)
            .unwrap_or_else(|| panic!("{id}"))
            .transforms()
            .iter()
            .map(|t| t.name.clone())
            .collect()
    };
    assert_eq!(names("T-5"), vec!["drop-computed-state", "v5-record-fields-into-imported-from"]);
    assert_eq!(names("IS-4"), vec!["prior-imported-from-nested", "v5-record-fields-into-imported-from"]);
    assert_eq!(names("Q-16"), vec!["v5-record-fields-into-imported-from"]);
    assert_eq!(names("IS-4/edges/1"), vec!["edge-text-joined"]);
    assert_eq!(names("Q-16/edges/1"), vec!["edge-text-joined", "edge-duplicate-triple-merged"]);
    assert!(items.iter().all(|i| i.source_id() != "Q-16/edges/2"), "merged into /1");
    assert_eq!(names("viewer:coordinator"), vec!["viewers-from-manifest"]);
    // A carried id never rides `fields`.
    for i in &items {
        if let SourceItem::Node(n) = i {
            assert!(!n.fields.contains_key("id"), "{}", n.source_id);
        }
    }
    // The viewer note has no v6 home: named, lossy.
    let m = MANIFEST.replace("  curator:\n    nodes: []\n", "  curator:\n    nodes: []\n    note: the curator's viewer note\n");
    let pool = write_pool(&root, &fixture_files(), &m);
    let items = migrate::v5::source_items(&pool).expect("read");
    let v = items.iter().find(|i| i.source_id() == "viewer:curator").unwrap();
    let dropped: Vec<_> = v.transforms().iter().filter(|t| t.name == "viewer-note-dropped").collect();
    assert_eq!(dropped.len(), 1);
    assert!(dropped[0].lossy);
}

// ---------------------------------------------------------------------------
// guard 9: viewers land as V- nodes with shows{cite, why}
// ---------------------------------------------------------------------------

/// Two viewers, minted in ASCENDING ROLE ORDER (`V-1` coordinator, `V-2`
/// curator — the manifest lists curator FIRST), `V-1 -[shows]-> Q-65` with
/// `{cite, why}` as declared fields and no note, `V-2` with no edges,
/// `id_serials.V == 2`; the viewer items come after every node file's items.
// RED A: yield the viewers BEFORE the node files — the landing still
// commits (the driver binds every node before any edge, so there is no
// `hard` error); the ordering assertion fires ("29 vs 0"). RED B (opposing): omit `cite` — the
// `shows` item is Rejected `missing_required_edge_field`.
#[test]
fn viewers_land_as_v_nodes_with_shows() {
    let (root, c, run) = import_fixture("viewers");
    let first_viewer = run
        .manifest
        .entries
        .iter()
        .position(|e| e.source_id.starts_with("viewer:"))
        .expect("viewer items");
    // Every node file's entry (node or edge) precedes every viewer entry —
    // measured against the LAST non-viewer entry, so a viewers-first order
    // cannot pass it vacuously (it did, once: the first form of this
    // assertion only looked before the first viewer, which is empty when
    // viewers come first).
    let last_file = run
        .manifest
        .entries
        .iter()
        .rposition(|e| !e.source_id.starts_with("viewer:"))
        .expect("node-file items");
    assert!(last_file < first_viewer, "viewers come after every node file: {last_file} vs {first_viewer}");
    assert_eq!(run.manifest.entries[first_viewer].source_id, "viewer:coordinator");
    let v1 = node(&c, "V-1");
    assert_eq!(v1.record.node_type, "viewer");
    assert_eq!(field(&v1, "role"), json!("coordinator"));
    assert_eq!(v1.out.len(), 1);
    assert_eq!(v1.out[0].kind, "shows");
    assert_eq!(v1.out[0].dst.id.0, "Q-65");
    assert_eq!(v1.out[0].note, None);
    assert_eq!(
        Value::Object(v1.out[0].fields.clone().into_iter().collect()),
        json!({
            "cite": {"thread": "00000000-0000-4000-8000-000000000002", "msg": 15},
            "why": "the fixture's start-here question",
        })
    );
    let v2 = node(&c, "V-2");
    assert_eq!(field(&v2, "role"), json!("curator"));
    assert!(v2.out.is_empty());
    assert_eq!(serials(&root)["V"], 2);
}

/// The coordinator viewer names a node the run rejected (Q-65 poisoned with
/// an out-of-vocabulary state): its `shows` edge is `endpoint-rejected`, and
/// the viewer still lands.
#[test]
fn a_viewer_whose_node_was_rejected_still_lands() {
    let root = scratch("viewer-rejected");
    let mut files = fixture_files();
    files.iter_mut().find(|(i, _)| *i == "Q-65").unwrap().1 = Q_65.replace("state: open", "state: archived");
    let pool = write_pool(&root, &files, MANIFEST);
    let c = destination(&root);
    let run = ran(migrate::import_v5_pool(&c, &agent_plan(), &pool));
    assert_eq!(rejected_tags(&run, "Q-65"), vec!["unknown_state"]);
    assert_eq!(
        marker(run.manifest.entry("viewer:coordinator/shows/0").unwrap()),
        Some(MARKER_ENDPOINT_REJECTED)
    );
    assert_eq!(field(&node(&c, "V-1"), "role"), json!("coordinator"));
    assert!(node(&c, "V-1").out.is_empty());
}

/// Omitting `cite` from a `shows` entry is judged by the grammar on that
/// edge item.
#[test]
fn a_shows_entry_without_cite_is_rejected_by_the_grammar() {
    let root = scratch("viewer-no-cite");
    let m = MANIFEST.replace(
        "      cite:\n        thread: 00000000-0000-4000-8000-000000000002\n        msg: 15\n",
        "",
    );
    let pool = write_pool(&root, &fixture_files(), &m);
    let c = destination(&root);
    let run = ran(migrate::import_v5_pool(&c, &agent_plan(), &pool));
    assert_eq!(
        rejected_tags(&run, "viewer:coordinator/shows/0"),
        vec!["missing_required_edge_field"]
    );
}

// ---------------------------------------------------------------------------
// the census and the copy — the real pool is never the import's input
// ---------------------------------------------------------------------------

/// One regular file of a directory tree: its root-relative path
/// (`/`-separated), its sha256 (lower hex) and its size in bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CensusEntry {
    path: String,
    sha256: String,
    size: u64,
}

/// A directory's CENSUS: the CANONICAL root it was taken over, and every
/// regular file under it, recursive, sorted by path, NOTHING skipped (`.index/`,
/// `artifacts/`, `PROGRESS-*.md` included — it is a census of the directory,
/// not of what the adapter reads). Anything that is neither a regular file nor
/// a directory (a symlink, a socket) is REFUSED by name rather than skipped,
/// because a census that omits what it cannot read and still prints a total is
/// a census whose totals are claims. Reads only (`read_dir`,
/// `std::fs::read`, `canonicalize`); it never opens anything for writing.
#[derive(Debug, Clone)]
struct DirCensus {
    root: PathBuf,
    entries: Vec<CensusEntry>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

impl DirCensus {
    fn of(dir: &Path) -> DirCensus {
        let root = std::fs::canonicalize(dir).unwrap_or_else(|e| panic!("census: canonicalize `{}`: {e}", dir.display()));
        let mut entries = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap_or_else(|e| panic!("census: list `{}`: {e}", d.display())) {
                let e = e.unwrap_or_else(|err| panic!("census: entry of `{}`: {err}", d.display()));
                let p = e.path();
                // `DirEntry::file_type` does NOT follow a symlink, so a link is
                // seen as a link and refused below, never silently traversed.
                let ft = e.file_type().unwrap_or_else(|err| panic!("census: file type of `{}`: {err}", p.display()));
                if ft.is_dir() {
                    stack.push(p);
                } else if ft.is_file() {
                    let bytes = std::fs::read(&p).unwrap_or_else(|err| panic!("census: read `{}`: {err}", p.display()));
                    let rel = p
                        .strip_prefix(&root)
                        .expect("under the root")
                        .components()
                        .map(|c| c.as_os_str().to_str().expect("a UTF-8 path").to_string())
                        .collect::<Vec<_>>()
                        .join("/");
                    entries.push(CensusEntry { path: rel, sha256: sha256_hex(&bytes), size: bytes.len() as u64 });
                } else {
                    panic!("census: `{}` is neither a regular file nor a directory; the census refuses to skip it", p.display());
                }
            }
        }
        entries.sort();
        DirCensus { root, entries }
    }

    fn bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.size).sum()
    }

    /// sha256 over `<path>\t<sha256>\t<size>\n` per entry, in path order.
    fn digest(&self) -> String {
        let mut text = String::new();
        for e in &self.entries {
            text.push_str(&format!("{}\t{}\t{}\n", e.path, e.sha256, e.size));
        }
        sha256_hex(text.as_bytes())
    }

    fn line(&self, label: &str) -> String {
        format!(
            "REHEARSAL census {label} root={} files={} bytes={} digest={}",
            self.root.display(),
            self.entries.len(),
            self.bytes(),
            self.digest()
        )
    }

    /// Every path whose presence, size or content differs, NAMED.
    fn diff(&self, other: &DirCensus) -> Vec<String> {
        let a: BTreeMap<&str, &CensusEntry> = self.entries.iter().map(|e| (e.path.as_str(), e)).collect();
        let b: BTreeMap<&str, &CensusEntry> = other.entries.iter().map(|e| (e.path.as_str(), e)).collect();
        let mut out = Vec::new();
        for (p, ea) in &a {
            match b.get(p) {
                None => out.push(format!("gone: {p}")),
                Some(eb) if eb != ea => out.push(format!(
                    "changed: {p} (size {} -> {}, sha256 {} -> {})",
                    ea.size, eb.size, ea.sha256, eb.sha256
                )),
                Some(_) => {}
            }
        }
        for p in b.keys() {
            if !a.contains_key(p) {
                out.push(format!("new: {p}"));
            }
        }
        out
    }
}

/// Copy the tree `src` to `dst` (created; must not exist): directories and
/// regular files only, anything else refused by name. `src` is only READ.
fn copy_tree(src: &Path, dst: &Path) {
    assert!(!dst.exists(), "copy_tree: `{}` already exists", dst.display());
    std::fs::create_dir_all(dst).unwrap_or_else(|e| panic!("copy_tree: mkdir `{}`: {e}", dst.display()));
    for e in std::fs::read_dir(src).unwrap_or_else(|e| panic!("copy_tree: list `{}`: {e}", src.display())) {
        let e = e.expect("dir entry");
        let ft = e.file_type().expect("file type");
        let to = dst.join(e.file_name());
        if ft.is_dir() {
            copy_tree(&e.path(), &to);
        } else if ft.is_file() {
            let bytes = std::fs::read(e.path()).unwrap_or_else(|err| panic!("copy_tree: read `{}`: {err}", e.path().display()));
            std::fs::write(&to, bytes).unwrap_or_else(|err| panic!("copy_tree: write `{}`: {err}", to.display()));
        } else {
            panic!("copy_tree: `{}` is neither a regular file nor a directory", e.path().display());
        }
    }
}

/// Guard 2 (name the two things and prove they are different):
/// the directory handed to the import is the COPY — not the source, not
/// under it, not above it — and the two censuses were taken over two
/// different canonical directories with EQUAL contents (the copy is
/// byte-identical, file count equal). Runs BEFORE the import.
// RED: hand the source path to the import (`let import_dir = source_dir`) —
// this panics before `import_v5_pool` runs.
fn the_import_reads_the_copy(source: &DirCensus, copy: &DirCensus, import_dir: &Path) {
    let import = std::fs::canonicalize(import_dir).expect("canonicalize the import dir");
    assert_ne!(import, source.root, "the import must read the COPY, never the source");
    assert!(!import.starts_with(&source.root), "the import dir `{}` is under the source", import.display());
    assert!(!source.root.starts_with(&import), "the source is under the import dir `{}`", import.display());
    assert_eq!(import, copy.root, "the import reads the directory the copy census was taken over");
    assert_ne!(source.root, copy.root, "the two censuses were taken over two different directories");
    assert_eq!(copy.entries.len(), source.entries.len(), "the copy's file count equals the source's");
    let diff = source.diff(copy);
    assert!(diff.is_empty(), "the copy is not byte-identical to the source: {diff:#?}");
}

/// The census and the copy, hermetic (no real pool path): a nested tree with a
/// dot-directory, an empty file and a non-YAML file copies byte-identical; the
/// census then NAMES a one-byte append, a new file and a removed file — so
/// the end-of-rehearsal comparison can come out differently.
#[test]
fn the_census_names_every_difference_and_the_copy_is_byte_identical() {
    let root = scratch("census");
    let src = root.join("pool");
    std::fs::create_dir_all(src.join("nodes")).unwrap();
    std::fs::create_dir_all(src.join(".index/deep")).unwrap();
    std::fs::write(src.join("nodes/R-1.yaml"), R_1).unwrap();
    std::fs::write(src.join(".index/deep/x.bin"), [0u8, 1, 2, 255]).unwrap();
    std::fs::write(src.join("PROGRESS-1.md"), "").unwrap();
    std::fs::write(src.join("manifest.yaml"), MANIFEST).unwrap();
    let before = DirCensus::of(&src);
    assert_eq!(before.entries.len(), 4, "{:?}", before.entries);
    assert_eq!(before.entries[0].path, ".index/deep/x.bin");
    let copy = root.join("copy").join("pool");
    copy_tree(&src, &copy);
    let c = DirCensus::of(&copy);
    the_import_reads_the_copy(&before, &c, &copy);
    assert_eq!(c.digest(), before.digest());
    assert_ne!(c.root, before.root);
    // A one-byte append, a new file, a removed file: each named.
    use std::io::Write;
    std::fs::OpenOptions::new().append(true).open(copy.join("nodes/R-1.yaml")).unwrap().write_all(b"x").unwrap();
    std::fs::write(copy.join("new.txt"), "n").unwrap();
    std::fs::remove_file(copy.join("PROGRESS-1.md")).unwrap();
    let after = DirCensus::of(&copy);
    assert_ne!(after.digest(), before.digest());
    let diff = before.diff(&after);
    assert_eq!(diff.len(), 3, "{diff:#?}");
    assert!(diff[0] == "gone: PROGRESS-1.md", "{diff:#?}");
    assert!(diff[1].starts_with("changed: nodes/R-1.yaml (size "), "{diff:#?}");
    assert_eq!(diff[2], "new: new.txt");
    // The copy census refuses to hand a differing copy to the import.
    let refused = std::panic::catch_unwind(|| the_import_reads_the_copy(&before, &after, &copy));
    assert!(refused.is_err(), "a differing copy is refused");
    // And the source itself is refused as the import dir.
    let refused = std::panic::catch_unwind(|| the_import_reads_the_copy(&before, &before, &src));
    assert!(refused.is_err(), "the source is refused as the import dir");
}

// ---------------------------------------------------------------------------
// guard 10 + the real pool (opt-in) — rehearsed on a COPY
// ---------------------------------------------------------------------------

/// THE REAL POOL, opt-in: `V5_POOL_DIR=<dir> cargo test -p spine --test
/// import_v5 -- --ignored real_pool --nocapture`. The test CENSUSES
/// the source (every regular file, sha256 + size), COPIES the whole directory
/// into a fresh scratch dir under the same basename, proves the copy
/// byte-identical and a different directory (guard 2), imports the COPY into
/// a scratch cauldron it created, prints the manifest summary, and re-censuses
/// the SOURCE on every exit path — a panic anywhere in the rehearsal still
/// checks it (guard 1). The source path is passed to nothing but the census
/// and the copy.
// RED (guard 1, only ever against a throwaway pool under /tmp/fakepool):
// append one byte to a SOURCE file after the copy — the end-of-test census
// names it. RED (guard 10): one byte appended to a node file of the COPY
// after the import — `source_untouched` fires.
#[test]
#[ignore = "opt-in: set V5_POOL_DIR to a v5 regions pool (read-only; the test imports a copy)"]
fn real_pool() {
    let source_dir = PathBuf::from(std::env::var("V5_POOL_DIR").expect("V5_POOL_DIR names the pool"));
    let before = DirCensus::of(&source_dir);
    println!("{}", before.line("source BEFORE"));
    let body = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rehearse(&source_dir, &before)));
    let after = DirCensus::of(&source_dir);
    println!("{}", after.line("source AFTER"));
    let diff = before.diff(&after);
    assert!(
        diff.is_empty(),
        "THE SOURCE WAS WRITTEN during the rehearsal ({} difference(s)){}: {diff:#?}",
        diff.len(),
        if body.is_err() { " — and the rehearsal itself panicked (above)" } else { "" }
    );
    println!("REHEARSAL source unchanged: files={} digest={}", after.entries.len(), after.digest());
    if let Err(p) = body {
        std::panic::resume_unwind(p);
    }
}

fn rehearse(source_dir: &Path, source_census: &DirCensus) {
    let pool_name = source_dir
        .file_name()
        .and_then(|n| n.to_str())
        .expect("the pool directory has a basename")
        .to_string();
    let root = scratch(&format!("rehearsal-{pool_name}"));
    let copy = root.join(&pool_name);
    copy_tree(source_dir, &copy);
    let copy_census = DirCensus::of(&copy);
    println!("{}", copy_census.line("copy"));
    let import_dir = copy.clone();
    the_import_reads_the_copy(source_census, &copy_census, &import_dir);

    let digest_before = checkers::SourceDigest::of_dir(&import_dir).expect("digest of the copy before");
    let source = migrate::v5::read_pool(&import_dir).expect("read the copy");
    let c = Cauldron::create(&root.join("cauldron"), &[workspace_v6(&pool_name)], SpineConfig::default())
        .expect("scratch cauldron");
    let mut plan = MigrationPlan::new(Initiator::Agent {
        thread: COORD_THREAD.into(),
        dispatch: "import-v5-rehearsal".into(),
    });
    plan.idempotency_nonce = Some(format!("import-v5-rehearsal-{pool_name}"));
    let started = std::time::Instant::now();
    let run = ran(migrate::import_v5_pool(&c, &plan, &import_dir));
    let elapsed = started.elapsed();

    // ---- the manifest summary ----
    let m = &run.manifest;
    let node_items = source.items.iter().filter(|i| matches!(i, SourceItem::Node(_))).count();
    let viewer_items = source.items.iter().filter(|i| i.source_id().starts_with("viewer:") && matches!(i, SourceItem::Node(_))).count();
    let shows_items = source.items.iter().filter(|i| matches!(i, SourceItem::Edge(e) if e.kind == "shows")).count();
    let edge_items = source.items.len() - node_items;
    let unresolved = m.entries.iter().filter(|e| matches!(&e.disposition, Disposition::Imported { resolution: Resolution::UnresolvedEndpoint { .. }, .. })).count();
    let mut by_marker: BTreeMap<String, usize> = BTreeMap::new();
    for e in &m.entries {
        if let Disposition::Imported { resolution: Resolution::UnresolvedEndpoint { marker, .. }, .. } = &e.disposition {
            *by_marker.entry(marker.clone()).or_default() += 1;
        }
    }
    let mut transforms: BTreeMap<String, usize> = BTreeMap::new();
    for i in &source.items {
        for t in i.transforms() {
            *transforms.entry(t.name.clone()).or_default() += 1;
        }
    }
    let census = checkers::census(&c).expect("census");
    let s = {
        let conn = rusqlite::Connection::open(root.join("cauldron").join(SPINE_FILE)).unwrap();
        let mut st = conn.prepare("SELECT prefix, last FROM id_serials WHERE pool = ?1 ORDER BY prefix").unwrap();
        let rows: Vec<(String, i64)> = st.query_map([&pool_name], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect();
        rows
    };
    println!("REAL-POOL pool={pool_name} board={}", source.board);
    println!("REAL-POOL source_count={} node_items={node_items} (viewers {viewer_items}) edge_items={edge_items} (shows {shows_items})", m.source_count);
    println!("REAL-POOL imported={} rejected={} unresolved={unresolved} by_marker={by_marker:?}", m.imported(), m.rejected());
    println!("REAL-POOL landed_nodes={} census nodes={} edges={}", landed_count(&run), census.node_count(), census.edge_count());
    println!("REAL-POOL transforms={transforms:?}");
    println!("REAL-POOL id_serials={s:?}");
    println!("REAL-POOL elapsed={elapsed:?}");
    let items_by_id: BTreeMap<&str, &SourceItem> = source.items.iter().map(|i| (i.source_id(), i)).collect();
    let mut rejected: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let (mut rejected_node_tags, mut rejected_edge_tags) = (BTreeMap::<String, usize>::new(), BTreeMap::<String, usize>::new());
    for e in &m.entries {
        if let Disposition::Rejected { reason } = &e.disposition {
            let tags: Vec<String> = reason.witnesses.iter().map(tag).collect();
            println!("REAL-POOL REJECTED {} {:?}", e.source_id, tags);
            let into = match items_by_id.get(e.source_id.as_str()) {
                Some(SourceItem::Edge(_)) => &mut rejected_edge_tags,
                _ => &mut rejected_node_tags,
            };
            for t in &tags {
                *into.entry(t.clone()).or_default() += 1;
            }
            rejected.insert(e.source_id.clone(), tags);
        }
    }
    println!("REHEARSAL rejected by tag: nodes={rejected_node_tags:?} edges={rejected_edge_tags:?}");
    let landed: &BTreeMap<String, QualifiedNodeId> = match &run.landing {
        Landing::Committed { landed, .. } => landed,
        other => panic!("expected a landing, got {other:?}"),
    };
    let mut landed_by_type: BTreeMap<String, usize> = BTreeMap::new();
    for sid in landed.keys() {
        if let Some(SourceItem::Node(n)) = items_by_id.get(sid.as_str()) {
            *landed_by_type.entry(n.node_type.clone()).or_default() += 1;
        }
    }
    println!("REHEARSAL landed nodes by type={landed_by_type:?}");

    // ---- no silent renumbering (guard 5) — FIRST ----
    // Every node item landed under its OWN source id (viewers mint `V-n` and
    // are checked below). Run before the per-file fidelity loop so a
    // renumbering is reported as what it is, not as the missing local edges
    // it also causes. RED (guard 5): map `SUB-<n>` -> `R-<n+1000>` in the
    // adapter — this fires naming the first aliased region.
    for (sid, q) in landed {
        if !sid.starts_with("viewer:") {
            assert_eq!(&q.id.0, sid, "{sid} landed under an id other than its file's: {}", q.id.0);
        }
    }

    // Why a LOCAL source edge may be absent from the store: the manifest's own
    // disposition of its item — an unresolved marker, or a rejection by tag.
    // Keyed (src, kind, dst). A local edge the manifest does not explain is
    // MISSING, and that fails.
    let mut unstored_why: BTreeMap<(String, String, String), String> = BTreeMap::new();
    let (mut near_rejected, mut far_rejected) = (0usize, 0usize);
    for e in &m.entries {
        match &e.disposition {
            Disposition::Imported {
                shape: ImportShape::Edge { kind, src, dst },
                resolution: Resolution::UnresolvedEndpoint { marker, .. },
                ..
            } => {
                if marker == MARKER_ENDPOINT_REJECTED {
                    if rejected.contains_key(src) {
                        near_rejected += 1;
                    } else {
                        far_rejected += 1;
                    }
                }
                unstored_why.insert((src.clone(), kind.clone(), dst.clone()), marker.clone());
            }
            Disposition::Rejected { .. } => {
                if let Some(SourceItem::Edge(se)) = items_by_id.get(e.source_id.as_str()) {
                    unstored_why.insert(
                        (se.src.clone(), se.kind.clone(), se.dst.clone()),
                        format!("rejected {:?}", rejected[&e.source_id]),
                    );
                }
            }
            _ => {}
        }
    }

    // ---- guard 10 ----
    let after = checkers::SourceDigest::of_dir(&import_dir).expect("digest of the copy after");
    green(&checkers::source_untouched(&digest_before, &after));
    let ledger = checkers::ledger_for(&c, &census).expect("ledger");
    green(&checkers::one_door_provenance(&census, &ledger));
    the_one_attempt(&c, &run);
    let (tx, _) = run.landing.attempt().unwrap();
    let row = &checkers::attempt_history(&c, tx).unwrap().rows[0];
    assert_eq!(row.instrument.name, migrate::IMPORT_TOOL);
    assert_eq!(row.instrument, migrate::import_tool());
    assert_eq!(
        row.initiator,
        Initiator::Agent { thread: COORD_THREAD.into(), dispatch: "import-v5-rehearsal".into() }
    );

    // ---- fidelity, file by file (the COPY's files; never the source) ----
    let pool = PoolName::new(&pool_name);
    let get = |id: &str| {
        c.node_view(&QualifiedNodeId { pool: pool.clone(), id: NodeId(id.into()) })
            .unwrap()
            .unwrap_or_else(|| panic!("{id} landed"))
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(import_dir.join("nodes"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("yaml"))
        .collect();
    files.sort();
    let docs: Vec<(String, Value)> = files
        .iter()
        .map(|f| {
            let doc: Value = serde_yaml::from_slice(&std::fs::read(f).unwrap()).unwrap();
            (doc["id"].as_str().expect("a string id").to_string(), doc)
        })
        .collect();
    let local: std::collections::BTreeSet<String> = docs.iter().map(|(id, _)| id.clone()).collect();
    step0_diagnosis(&docs, m, &rejected, &items_by_id, &import_dir);
    let (mut blocking_q,mut blocking_q_true, mut blocking_is, mut deferred, mut admits, mut sentences, mut operators) =
        (0, 0, 0, 0, 0, 0, 0);
    let (mut admits_by_edge, mut admits_unstored, mut landed_external, mut local_stored) = (0usize, 0usize, 0usize, 0usize);
    let mut unstored_local: BTreeMap<String, usize> = BTreeMap::new();
    let mut rejected_nodes: Vec<(String, String, Vec<String>)> = Vec::new();
    for (id, doc) in &docs {
        let id = id.as_str();
        if let Some(tags) = rejected.get(id) {
            rejected_nodes.push((id.to_string(), doc["type"].as_str().unwrap().to_string(), tags.clone()));
            continue;
        }
        let q = landed.get(id).unwrap_or_else(|| panic!("{id}: neither landed nor rejected"));
        assert_eq!(q.id.0, id, "{id} landed under its own id");
        let v = get(id);
        assert_eq!(v.record.node_type, doc["type"].as_str().unwrap(), "{id}");
        let meta = field(&v, "meta");
        let rec = &meta["imported_from"];
        assert_eq!(rec["board"], json!(source.board), "{id}");
        assert_eq!(rec["v5_id"], json!(id));
        let entries: Vec<Value> = doc["edges"].as_array().cloned().unwrap_or_default();
        // admits_by_edge, expected side read from the FILE (never the store).
        let want: serde_json::Map<String, Value> = entries
            .iter()
            .filter(|e| e["kind"] == json!("in_region"))
            .filter_map(|e| e.get("admits").map(|a| (e["to"].as_str().unwrap().to_string(), a.clone())))
            .collect();
        assert_eq!(rec["admits_by_edge"], Value::Object(want.clone()), "{id} admits_by_edge");
        admits_by_edge += want.len();
        assert_eq!(rec["rev"], doc["rev"], "{id}");
        assert_eq!(rec["created"], doc["provenance"]["created"], "{id}");
        assert_eq!(rec["created_by"], doc["provenance"]["created_by"], "{id}");
        assert_eq!(rec["schema_version"], json!(5), "{id}");
        if let Some(b) = doc["meta"].get("blocking") {
            assert_eq!(meta.get("blocking"), Some(b), "{id}");
            match doc["type"].as_str().unwrap() {
                "question" => {
                    blocking_q += 1;
                    if b == &json!(true) {
                        blocking_q_true += 1;
                    }
                }
                "issue" => blocking_is += 1,
                _ => {}
            }
        }
        if let Some(d) = doc.get("deferred_by") {
            assert_eq!(&field(&v, "deferred_by"), d, "{id}");
            deferred += 1;
        }
        if let Some(op) = doc["provenance"].get("operator") {
            assert_eq!(&field(&v, "operator"), op, "{id}");
            operators += 1;
        }
        // (ii) the record's `unresolved_edges` IS the
        // file's external entries — every entry whose `to` is not a node file
        // of this pool — in ORDER, COUNT and CONTENT, compared as parsed YAML
        // values (every key: kind, to, sentence, note, label, admits, cite).
        // RED A: drop the last external entry from the record — COUNT fails.
        // RED B (same count): rewrite one entry's `to` — CONTENT fails.
        // RED C: append a spurious entry — COUNT fails.
        let want_ext: Vec<Value> = entries
            .iter()
            .filter(|e| !local.contains(e["to"].as_str().unwrap()))
            .cloned()
            .collect();
        let got_ext = rec["unresolved_edges"]
            .as_array()
            .unwrap_or_else(|| panic!("{id}: meta.imported_from.unresolved_edges is not a list: {}", rec["unresolved_edges"]));
        assert_eq!(got_ext.len(), want_ext.len(), "{id}: unresolved_edges COUNT (record {got_ext:?} vs file {want_ext:?})");
        assert_eq!(got_ext, &want_ext, "{id}: unresolved_edges CONTENT");
        landed_external += want_ext.len();
        // (i) a LOCAL edge of the source is stored,
        // or the manifest names why not (an unresolved marker — its far end
        // was rejected — or the edge item's own rejection by tag). Anything
        // else is a MISSING local edge and fails, naming it. (The old loop
        // skipped it: `else { continue; }`.)
        for e in &entries {
            let kind = e["kind"].as_str().unwrap();
            let to = e["to"].as_str().unwrap();
            if !local.contains(to) {
                continue; // external: (ii) holds it
            }
            let Some(stored) = v.out.iter().find(|s| s.kind == kind && s.dst.id.0 == to) else {
                let why = unstored_why
                    .get(&(id.to_string(), kind.to_string(), to.to_string()))
                    .unwrap_or_else(|| {
                        panic!(
                            "MISSING LOCAL EDGE {{node: {id}, kind: {kind}, to: {to}}}: the source file carries it, \
                             the store does not, and the manifest does not say why"
                        )
                    });
                *unstored_local.entry(why.clone()).or_default() += 1;
                if kind == "in_region" && e.get("admits").is_some() {
                    admits_unstored += 1;
                }
                continue;
            };
            local_stored += 1;
            if let Some(a) = e.get("admits") {
                assert_eq!(stored.note.as_deref(), a.as_str(), "{id} in_region {to}");
                admits += 1;
            }
            if let Some(snt) = e.get("sentence") {
                let note = stored.note.as_deref().unwrap_or("");
                assert!(note == snt.as_str().unwrap() || note.contains(&format!("sentence: {}", snt.as_str().unwrap())), "{id} ref {to}: {note}");
                sentences += 1;
            }
        }
    }
    println!("REAL-POOL blocking questions={blocking_q} (true {blocking_q_true}) issues={blocking_is} deferred_by={deferred} admits_verbatim={admits} sentences_kept={sentences} operator={operators}");
    println!("REAL-POOL admits_by_edge entries equal to source={admits_by_edge}");
    assert_eq!(admits_by_edge, admits + admits_unstored, "every in_region admits span is in the record");

    // ---- the viewers: one per manifest role key, ascending ----
    let manifest: Value = serde_yaml::from_slice(&std::fs::read(import_dir.join("manifest.yaml")).unwrap()).unwrap();
    let mut roles: Vec<String> = manifest["viewers"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
    roles.sort();
    assert_eq!(roles.len(), viewer_items, "one viewer item per manifest role key");
    let mut viewer_external = 0usize;
    for (n, role) in roles.iter().enumerate() {
        let sid = format!("viewer:{role}");
        let vid = landed.get(&sid).unwrap_or_else(|| panic!("{sid} landed")).id.0.clone();
        assert_eq!(vid, format!("V-{}", n + 1), "{sid}: viewers mint in ascending role order");
        let v = get(&vid);
        assert_eq!(field(&v, "role"), json!(role));
        let listed: Vec<Value> = manifest["viewers"][role.as_str()]["nodes"].as_array().cloned().unwrap_or_default();
        for entry in &listed {
            let to = entry["id"].as_str().expect("a viewer listing names an id");
            if !local.contains(to) {
                viewer_external += 1;
            }
            if v.out.iter().any(|e| e.kind == "shows" && e.dst.id.0 == to) {
                continue;
            }
            let why = unstored_why.get(&(sid.clone(), "shows".to_string(), to.to_string())).unwrap_or_else(|| {
                panic!("MISSING LOCAL EDGE {{node: {vid} ({sid}), kind: shows, to: {to}}}: listed by the manifest, not stored, not explained")
            });
            *unstored_local.entry(format!("shows: {why}")).or_default() += 1;
        }
        println!(
            "REHEARSAL viewer {vid} role={role} manifest_nodes_key={} listed={} shows_stored={}",
            manifest["viewers"][role.as_str()].get("nodes").is_some(),
            listed.len(),
            v.out.iter().filter(|e| e.kind == "shows").count()
        );
    }
    // (ii), summed: every external entry of a LANDED node and every viewer
    // listing toward a non-node is the manifest's `endpoint-absent-from-source`
    // count, exactly (a rejected node's external entries are marked
    // `endpoint-rejected` — its near end — and are not in this sum).
    let absent = by_marker.get(MARKER_ABSENT_FROM_SOURCE).copied().unwrap_or(0);
    assert_eq!(
        landed_external + viewer_external,
        absent,
        "the landed nodes' unresolved_edges ({landed_external}) + viewer externals ({viewer_external}) = the manifest's `{MARKER_ABSENT_FROM_SOURCE}` count"
    );
    println!("REHEARSAL unresolved_edges verbatim: landed-node externals={landed_external} viewer externals={viewer_external} manifest {MARKER_ABSENT_FROM_SOURCE}={absent}");
    println!("REHEARSAL local edges: stored-or-merged={local_stored} unstored-explained={unstored_local:?}");

    // ---- every node through the door; no edge only in the manifest;
    // citations survive. Computed from the FILES
    // and the LANDED STORE — never from a manifest marker (this block does
    // not lean on `unstored_why`).
    // Every failure is collected and reported together, each by name.
    let mut failures: Vec<String> = Vec::new();
    // (guard 6) rejected = 0, landed = node files + viewers, HO-1/HO-2/M-1.
    if m.rejected() != 0 {
        failures.push(format!("rejected={} (expected 0): {:?}", m.rejected(), rejected.keys().collect::<Vec<_>>()));
    }
    if landed_count(&run) != docs.len() + viewer_items {
        failures.push(format!("landed nodes={} != node files {} + viewers {viewer_items}", landed_count(&run), docs.len()));
    }
    for name in ["HO-1", "HO-2", "M-1"] {
        if local.contains(name) && !landed.contains_key(name) {
            failures.push(format!("{name} did not land: {:?}", rejected.get(name)));
        }
    }
    // (guard 6) the coordinator viewer shows the manifest's coordinator ids.
    {
        let mf: Value = serde_yaml::from_slice(&std::fs::read(import_dir.join("manifest.yaml")).unwrap()).unwrap();
        let want: std::collections::BTreeSet<String> = mf["viewers"]["coordinator"]["nodes"]
            .as_array()
            .map(|a| a.iter().map(|n| n["id"].as_str().unwrap().to_string()).collect())
            .unwrap_or_default();
        let coord_sid = "viewer:coordinator".to_string();
        let got: std::collections::BTreeSet<String> = landed
            .get(&coord_sid)
            .and_then(|q| c.node_view(q).unwrap())
            .map(|v| v.out.iter().filter(|e| e.kind == "shows").map(|e| e.dst.id.0.clone()).collect())
            .unwrap_or_default();
        println!("REHEARSAL coordinator viewer shows stored={got:?} manifest={want:?}");
        if want.is_empty() || got != want {
            failures.push(format!("the coordinator viewer shows {got:?}; the manifest lists {want:?} (non-empty required)"));
        }
    }
    // (guard 7) the issue count = the independent never-had count.
    let never = never_had_required(&docs);
    let mut issues: Vec<(String, String)> = Vec::new();
    for (id, _) in &docs {
        if landed.contains_key(id) {
            if let Some(list) = field(&get(id), "meta")["imported_from"]["issues"].as_array() {
                issues.extend(list.iter().map(|i| (id.clone(), i["edge_kind"].as_str().unwrap_or("?").to_string())));
            }
        }
    }
    println!("REHEARSAL issues (meta.imported_from.issues, from the store)={} never-had (from the files)={} {issues:?}", issues.len(), never.len());
    let never_pairs: Vec<(String, String)> = never.iter().map(|(n, k, _)| (n.clone(), k.clone())).collect();
    if issues != never_pairs {
        failures.push(format!("issues {issues:?} != never-had {never_pairs:?}"));
    }
    // (guard 8) every LOCAL source edge of a landed node whose far
    // end LANDED is stored (merged triples are one stored edge), and no local
    // edge of a landed node points at a node that did not land.
    let (mut edge_residual, mut endpoint_rejected_on_landed) = (Vec::new(), Vec::new());
    let mut local_checked = 0usize;
    for (id, doc) in &docs {
        let Some(q) = landed.get(id) else { continue };
        let v = c.node_view(q).unwrap().expect("landed");
        for e in doc["edges"].as_array().cloned().unwrap_or_default() {
            let (kind, to) = (e["kind"].as_str().unwrap(), e["to"].as_str().unwrap());
            if !local.contains(to) {
                continue;
            }
            local_checked += 1;
            let entry = format!("{{node: {id}, kind: {kind}, to: {to}}}");
            if !landed.contains_key(to) {
                endpoint_rejected_on_landed.push(entry);
            } else if !v.out.iter().any(|s| s.kind == kind && s.dst.id.0 == to) {
                edge_residual.push(entry);
            }
        }
    }
    println!(
        "REHEARSAL (from the store): local edges of landed nodes checked={local_checked} unstored-with-both-ends-landed={} toward-a-node-that-did-not-land={}",
        edge_residual.len(),
        endpoint_rejected_on_landed.len()
    );
    if !edge_residual.is_empty() {
        failures.push(format!("EDGE ONLY IN THE MANIFEST (both ends landed, not stored): {edge_residual:?}"));
    }
    if !endpoint_rejected_on_landed.is_empty() {
        failures.push(format!("LOCAL EDGE OF A LANDED NODE NOT STORED (its far end did not land): {endpoint_rejected_on_landed:?}"));
    }
    // (guard 9) citations to SUB-n survive: every `to: SUB-<n>` edge of a
    // landed node is stored toward a landed SUB-<n>, and every `[[SUB-<n>]]`
    // in a landed node's stored fields names a landed node.
    let sub_cite = regex_lite_sub_cites;
    let (mut sub_edges, mut sub_text, mut cite_failures) = (0usize, 0usize, Vec::new());
    for (id, doc) in &docs {
        let Some(q) = landed.get(id) else { continue };
        let v = c.node_view(q).unwrap().expect("landed");
        for e in doc["edges"].as_array().cloned().unwrap_or_default() {
            let (kind, to) = (e["kind"].as_str().unwrap(), e["to"].as_str().unwrap());
            // A LOCAL `SUB-<n>` id only: `SUB-1@another-board`
            // names another board (external, held verbatim in
            // `unresolved_edges` and counted by guard (ii) above).
            let local_sub = to.strip_prefix("SUB-").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
            if !local_sub {
                continue;
            }
            sub_edges += 1;
            if !landed.contains_key(to) || !v.out.iter().any(|s| s.kind == kind && s.dst.id.0 == to) {
                cite_failures.push(format!("{id} -[{kind}]-> {to}"));
            }
        }
        let text: BTreeMap<&String, &Value> = v.record.fields.iter().map(|(k, f)| (k, &f.value)).collect();
        for cited in sub_cite(&serde_json::to_string(&text).unwrap()) {
            sub_text += 1;
            if !landed.contains_key(&cited) {
                cite_failures.push(format!("{id} cites [[{cited}]]"));
            }
        }
    }
    println!("REHEARSAL SUB- citations: edges to SUB-<n> on landed nodes={sub_edges} [[SUB-<n>]] text cites={sub_text} unresolved={}", cite_failures.len());
    if !cite_failures.is_empty() {
        failures.push(format!("CITATIONS TO SUB- DO NOT RESOLVE: {cite_failures:?}"));
    }
    if !failures.is_empty() {
        panic!("NOT every node through the door ({} finding(s)):\n- {}", failures.len(), failures.join("\n- "));
    }

    // ---- the cascade ----
    let cascade: Vec<&(String, String, Vec<String>)> = rejected_nodes
        .iter()
        .filter(|(_, ty, tags)| ty != "region" && tags.iter().all(|t| t == "required_edge_missing"))
        .collect();
    println!(
        "REHEARSAL cascade: rejected nodes={} of which required_edge_missing (non-region)={} {:?}",
        rejected_nodes.len(),
        cascade.len(),
        cascade.iter().map(|(id, ty, _)| format!("{id}:{ty}")).collect::<Vec<_>>()
    );
    println!("REHEARSAL cascade: endpoint-rejected edge items={} (near end rejected {near_rejected}, far end only {far_rejected})", by_marker.get(MARKER_ENDPOINT_REJECTED).copied().unwrap_or(0));

    // ---- the region block ----
    // Every v5 REGION keeps its number. The expected side
    // is the FILES (id and type read from each file); the landed side is the
    // store, looked up by the source id and its type re-read. The v6 region
    // prefix is read from the grammar, not assumed, so a v5 prefix that
    // differs from it is a finding here, never a silent renumbering: each
    // region file EITHER landed under its own id, prefix v6's, its own
    // number, OR was REJECTED by name with its witness tags printed; and NO
    // node landed under an id other than its file's.
    // RED (guard 5): map `SUB-<n>` -> `R-<n+1000>` in the adapter — the
    // landed-under-another-id assertion (run first, after the landing) fires.
    let v6_region_prefix = workspace_v6(&pool_name).node_types["region"]
        .id_prefix
        .clone()
        .expect("v6 regions mint a serial");
    // (the landed-under-another-id check runs FIRST, right after the landing
    // — see "no silent renumbering" above the per-file loop.)
    let region_ids: std::collections::BTreeSet<&str> =
        docs.iter().filter(|(_, d)| d["type"] == json!("region")).map(|(id, _)| id.as_str()).collect();
    for r in census_regions(&census, &pool) {
        assert!(region_ids.contains(r.as_str()), "the store holds region {r}, which no region file is");
    }
    let mut region_numbers: BTreeMap<u64, String> = BTreeMap::new();
    let mut legacy_numbers: BTreeMap<u64, String> = BTreeMap::new();
    let mut by_prefix: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut rejected_regions: Vec<(String, Vec<String>)> = Vec::new();
    for (id, doc) in &docs {
        if doc["type"] != json!("region") {
            continue;
        }
        let (prefix, n) = id.split_once('-').expect("a region id is <prefix>-<n>");
        let n: u64 = n.parse().expect("a region number");
        if let Some(tags) = rejected.get(id.as_str()) {
            by_prefix.entry(prefix.to_string()).or_default().1 += 1;
            rejected_regions.push((id.clone(), tags.clone()));
            continue;
        }
        by_prefix.entry(prefix.to_string()).or_default().0 += 1;
        // A region lands under v6's prefix or the legacy import
        // prefix, under its OWN id and number.
        assert!(
            prefix == v6_region_prefix || prefix == spine::grammar_v6::REGION_LEGACY_PREFIX,
            "{id}: a region LANDED whose prefix is neither v6's nor the legacy import prefix"
        );
        let landed_region = get(id);
        assert_eq!(landed_region.record.node_type, "region", "{id}");
        assert_eq!(landed_region.record.id.0, format!("{prefix}-{n}"), "{id} keeps its id and number");
        if prefix == v6_region_prefix {
            assert!(region_numbers.insert(n, id.to_string()).is_none(), "two v5 regions map to {v6_region_prefix}-{n}");
        } else {
            assert!(legacy_numbers.insert(n, id.to_string()).is_none(), "two v5 regions map to {prefix}-{n}");
        }
    }
    println!(
        "REHEARSAL regions by prefix {{prefix: (landed, rejected)}} = {:?}",
        by_prefix
    );
    for (id, tags) in &rejected_regions {
        println!("REHEARSAL region REJECTED {id} {tags:?}");
    }
    let non_v6_prefix_files: usize = by_prefix.iter().filter(|(p, _)| *p != &v6_region_prefix).map(|(_, (l, r))| l + r).sum();
    // Every region file's prefix lands in full, none rejected — the
    // expected side counted from the FILES.
    let mut want_prefix: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for id in &region_ids {
        want_prefix.entry(id.split_once('-').unwrap().0.to_string()).or_default().0 += 1;
    }
    assert_eq!(by_prefix, want_prefix, "regions by prefix (landed, rejected) = (files, 0)");
    // Every rejection is EXPLAINED by the prefix finding: a rejected node is
    // either a region whose prefix is not v6's, or a non-region rejected
    // `required_edge_missing` whose EVERY source `in_region` points at a
    // rejected node (the cascade). Any other rejection is a new finding and
    // fails here, naming it.
    for (id, ty, tags) in &rejected_nodes {
        if ty == "region" {
            let prefix = id.split_once('-').map(|(p, _)| p).unwrap_or("");
            assert_ne!(prefix, v6_region_prefix, "region {id} carries v6's prefix and was rejected: {tags:?}");
            continue;
        }
        assert_eq!(tags, &vec!["required_edge_missing".to_string()], "{id} ({ty}) rejected for a reason the prefix finding does not explain");
        let doc = &docs.iter().find(|(d, _)| d == id).expect("its file").1;
        let homes: Vec<&str> = doc["edges"]
            .as_array()
            .map(|a| a.iter().filter(|e| e["kind"] == json!("in_region")).map(|e| e["to"].as_str().unwrap()).collect())
            .unwrap_or_default();
        assert!(
            !homes.is_empty() && homes.iter().all(|h| rejected.contains_key(*h)),
            "{id} ({ty}) rejected required_edge_missing, but not every home is rejected: {homes:?}"
        );
    }
    println!("REHEARSAL every rejection explained: {} rejected nodes = {} non-`{v6_region_prefix}` regions + {} homed only in them", rejected_nodes.len(), rejected_regions.len(), cascade.len());
    assert_eq!(rejected_nodes.len(), rejected_regions.len() + cascade.len());
    if non_v6_prefix_files == 0 {
        // A pool whose regions all carry v6's prefix (blueprint): exactly as
        // strong as the fixture — nothing rejected, every region kept its number.
        assert_eq!(m.rejected(), 0, "an all-`{v6_region_prefix}` pool imports with no rejection");
        assert!(rejected_regions.is_empty());
    }
    let max_region = *region_numbers.keys().last().expect("the pool has regions");
    let r_serial = s.iter().find(|(p, _)| *p == v6_region_prefix).map(|(_, l)| *l);
    assert_eq!(r_serial, Some(max_region as i64), "id_serials.{v6_region_prefix} = the max carried region number");
    let legacy = spine::grammar_v6::REGION_LEGACY_PREFIX;
    let legacy_serial = s.iter().find(|(p, _)| p == legacy).map(|(_, l)| *l);
    assert_eq!(
        legacy_serial,
        legacy_numbers.keys().last().map(|n| *n as i64),
        "id_serials.{legacy} = the max carried {legacy} number (absent when none was carried)"
    );
    println!(
        "REAL-POOL regions kept number={}/{} prefix {v6_region_prefix}->{v6_region_prefix} max={v6_region_prefix}-{max_region}; {legacy}->{legacy} {} max={:?} id_serials.{legacy}={legacy_serial:?} collisions=0",
        region_numbers.len() + legacy_numbers.len(),
        region_ids.len(),
        legacy_numbers.len(),
        legacy_numbers.keys().last()
    );
    // The coordinator viewer and its shows edges.
    let viewers: Vec<_> = (1..=viewer_items).map(|n| get(&format!("V-{n}"))).collect();
    let coord = viewers.iter().find(|v| field(v, "role") == json!("coordinator")).expect("coordinator viewer");
    println!("REAL-POOL coordinator viewer = {} shows={:?}", coord.record.id.0, coord.out.iter().map(|e| (&e.dst.id.0, &e.fields)).collect::<Vec<_>>());

    // ---- the judgment sample: printed here, judged by a reader
    // against the source files. Chosen by RULE, not by id: the first
    // landed node of every type (file order), every handoff and milestone
    // (landed or not), and the first landed node carrying an edge of each of
    // in_region / narrows / derived_from / ref toward a REJECTED region.
    let mut sample: Vec<String> = Vec::new();
    let mut seen_types = std::collections::BTreeSet::new();
    for (id, doc) in &docs {
        let ty = doc["type"].as_str().unwrap();
        if ty == "handoff" || ty == "milestone" {
            sample.push(id.clone());
        } else if !rejected.contains_key(id) && seen_types.insert(ty.to_string()) {
            sample.push(id.clone());
        }
    }
    for want_kind in ["in_region", "narrows", "derived_from", "ref"] {
        if let Some((id, _)) = docs.iter().find(|(id, doc)| {
            !rejected.contains_key(id)
                && !sample.contains(id)
                && doc["edges"].as_array().is_some_and(|a| {
                    a.iter().any(|e| e["kind"] == json!(want_kind) && region_ids.contains(e["to"].as_str().unwrap()) && rejected.contains_key(e["to"].as_str().unwrap()))
                })
        }) {
            sample.push(id.clone());
        }
    }
    // A fixed MEANING sample (nodes with edges toward SUB- regions, and two
    // SUB- regions), when the pool has them.
    for id in ["DEC-175", "DEC-604", "CON-23", "R-10", "SUB-7", "SUB-14"] {
        if local.contains(id) && !sample.iter().any(|s| s == id) {
            sample.push(id.to_string());
        }
    }
    for id in sample.iter().chain(std::iter::once(&coord.record.id.0)) {
        match rejected.get(id) {
            Some(tags) => println!("REHEARSAL SAMPLE {id} REJECTED {tags:?}"),
            None => {
                let v = get(id);
                let fields: BTreeMap<String, Value> = v.record.fields.iter().map(|(k, f)| (k.clone(), f.value.clone())).collect();
                let out: Vec<Value> = v
                    .out
                    .iter()
                    .map(|e| json!({"kind": e.kind, "to": e.dst.id.0, "note": e.note, "fields": e.fields}))
                    .collect();
                println!(
                    "REHEARSAL SAMPLE {id} type={} state={:?} fields={} out={}",
                    v.record.node_type,
                    v.record.state,
                    serde_json::to_string(&fields).unwrap(),
                    serde_json::to_string(&out).unwrap()
                );
            }
        }
    }
    // A CREATE region AFTER the import mints
    // `R-<max carried R + 1>` — never the legacy prefix (runs last: it
    // writes the scratch destination).
    let created = c
        .submit(Envelope {
            statement: format!("CREATE (r: {pool_name}/region {{ handle: $h, boundary: $b }}) RETURN r.id"),
            bindings: [
                ("h".to_string(), json!("minted after the rehearsal import")),
                ("b".to_string(), json!("Inside: nothing; the rehearsal's post-import mint. Outside: everything.")),
            ]
            .into_iter()
            .collect(),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
            provenance: Provenance { initiator: plan.initiator.clone(), instrument: Instrument::door() },
            mode: Mode::Commit,
            idempotency_nonce: None,
        })
        .expect("a CREATE region after the import");
    let minted = match &created {
        Outcome::Committed { returned, .. } => returned.0[0]["r.id"].as_str().unwrap().to_string(),
        other => panic!("{other:?}"),
    };
    println!("REAL-POOL CREATE region after the import minted {minted}");
    assert_eq!(minted, format!("{v6_region_prefix}-{}", max_region + 1), "the first mint after the import");
}

// ---------------------------------------------------------------------------
// guard 7: a v6-only required edge the v5 node NEVER had is an ISSUE,
// not a rejection
// ---------------------------------------------------------------------------

const ISSUE_R_1: &str = r#"schema_version: 5
id: R-1
type: region
handle: The issue fixture's home region
boundary: 'Inside: every node of the issue fixture. Outside: nothing else.'
rev: 1
meta: {}
edges: []
"#;
const ISSUE_R_2: &str = r#"schema_version: 5
id: R-2
type: region
handle: A region the grammar rejects
boundary: too short
rev: 1
meta: {}
edges: []
"#;
/// NEVER had a home: no in_region, no rejected_by.
const ISSUE_IS_10: &str = r#"schema_version: 5
id: IS-10
type: issue
state: open
handle: An issue the v5 pool never homed
rev: 1
meta: {}
edges:
- kind: ref
  to: R-1
"#;
/// Homed, but NEVER had a `serves` (v6's AND-form for a milestone).
const ISSUE_M_3: &str = r#"schema_version: 5
id: M-3
type: milestone
state: pending
handle: A milestone that never served a directive
rev: 1
meta: {}
edges:
- kind: in_region
  to: R-1
  admits: 'every node of the issue fixture'
"#;
/// HAD an in_region — toward the rejected R-2: not "never had", so judged
/// exactly as before.
const ISSUE_Q_11: &str = r#"schema_version: 5
id: Q-11
type: question
state: open
handle: A question homed only in a rejected region
rev: 1
meta: {}
edges:
- kind: in_region
  to: R-2
"#;

fn issue_files() -> Vec<(&'static str, String)> {
    [("R-1", ISSUE_R_1), ("R-2", ISSUE_R_2), ("IS-10", ISSUE_IS_10), ("M-3", ISSUE_M_3), ("Q-11", ISSUE_Q_11)]
        .into_iter()
        .map(|(id, y)| (id, y.to_string()))
        .collect()
}

/// The issues the STORE holds: every landed node's
/// `meta.imported_from.issues` entry, as (node, edge_kind).
fn stored_issues(c: &Cauldron, ids: &[&str]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for id in ids {
        if let Some(v) = view(c, id) {
            if let Some(list) = field(&v, "meta")["imported_from"]["issues"].as_array() {
                for i in list {
                    out.push((id.to_string(), i["edge_kind"].as_str().unwrap().to_string()));
                }
            }
        }
    }
    out
}

/// IS-10 (no in_region, no rejected_by) and M-3 (no serves) LAND on IMPORT,
/// each with its gap in `meta.imported_from.issues` AND named on its
/// manifest entry by `v6-required-edge-never-had`; Q-11, which HAD an
/// in_region toward a rejected region, is still rejected
/// `required_edge_missing`; the store's issue count equals the never-had
/// count read from the fixture YAML. Opposed (a): the same nodes through a
/// plain CREATE — their issue declaration included — still refuse
/// `required_edge_missing`.
// RED: revert the door's waiver (schema_def's `waived` always false) —
// IS-10 and M-3 rejected `required_edge_missing`. RED (b, opposing): the
// adapter declares the issue for EVERY content node, had or not — Q-11
// landed with an issue and the store's count (3) exceeded the never-had
// count (2). RED (a'): the door waiving without the carried-id condition
// — the plain CREATE of IS-10 committed.
#[test]
fn a_never_had_required_edge_is_an_issue_not_a_rejection() {
    let root = scratch("issues");
    let files = issue_files();
    let pool = write_pool(&root, &files, "schema_version: 5\nworkspace:\n  slug: issue-fixture\nviewers: {}\n");
    let c = destination(&root);
    let run = ran(migrate::import_v5_pool(&c, &agent_plan(), &pool));

    let docs: Vec<(String, Value)> = files.iter().map(|(id, y)| (id.to_string(), serde_yaml::from_str(y).unwrap())).collect();
    let never = never_had_required(&docs);
    assert_eq!(never, vec![("IS-10".to_string(), "in_region".to_string(), 1), ("M-3".to_string(), "serves".to_string(), 1)]);

    for (id, kind) in [("IS-10", "in_region"), ("M-3", "serves")] {
        let e = run.manifest.entry(id).unwrap();
        assert!(e.lands(), "{id} lands on IMPORT: {e:?}");
        let Disposition::Imported { transforms, .. } = &e.disposition else { unreachable!() };
        assert!(
            transforms.iter().any(|t| t.name == migrate::v5::TRANSFORM_REQUIRED_EDGE_ISSUE && !t.lossy),
            "{id}: the manifest names the issue: {transforms:?}"
        );
        let v = node(&c, id);
        assert_eq!(v.record.id.0, id);
        assert_eq!(
            field(&v, "meta")["imported_from"]["issues"],
            json!([{"kind": "required_edge_missing", "edge_kind": kind, "min": 1, "had": 0}]),
            "{id}"
        );
    }
    assert_eq!(rejected_tags(&run, "Q-11"), vec!["required_edge_missing"], "HAD an in_region: not waived");
    assert_eq!(rejected_tags(&run, "R-2"), vec!["under_floor"]);
    let ids: Vec<&str> = files.iter().map(|(id, _)| *id).collect();
    let stored = stored_issues(&c, &ids);
    assert_eq!(stored.len(), never.len(), "issue count = never-had count: {stored:?} vs {never:?}");
    assert!(node(&c, "R-1").record.fields["meta"].value["imported_from"].get("issues").is_none(), "no issue key without an issue");

    // Opposed (a): a plain CREATE of the same nodes, declaration included.
    let meta_is = field(&node(&c, "IS-10"), "meta");
    let meta_m = field(&node(&c, "M-3"), "meta");
    let door = |statement: &str, params: Value| {
        c.submit(Envelope {
            statement: statement.into(),
            bindings: params.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
            provenance: Provenance { initiator: agent_plan().initiator, instrument: Instrument::door() },
            mode: Mode::Commit,
            idempotency_nonce: None,
        })
    };
    for (statement, params) in [
        (r#"CREATE (n: ws/issue { handle: "created, never homed", state: "open", meta: $m })"#, json!({"m": meta_is})),
        (
            r#"MATCH (r: ws/region { id: "R-1" }) CREATE (n: ws/milestone { handle: "created, serves nothing", state: "pending", meta: $m }) CREATE (n) -[in_region]-> (r)"#,
            json!({"m": meta_m}),
        ),
    ] {
        match door(statement, params) {
            Err(SpineError::Refused { witnesses }) => {
                assert_eq!(witnesses.iter().map(tag).collect::<Vec<_>>(), vec!["required_edge_missing"], "{statement}")
            }
            other => panic!("a plain CREATE is not waived: {other:?}"),
        }
    }
}

/// Every `[[SUB-<n>]]` citation in `text`, in order (`n` serial-shaped).
fn regex_lite_sub_cites(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("[[SUB-") {
        let tail = &rest[at + 2..];
        let digits: String = tail["SUB-".len()..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() && tail["SUB-".len() + digits.len()..].starts_with("]]") {
            out.push(format!("SUB-{digits}"));
        }
        rest = &tail[4..];
    }
    out
}

/// The v6 required-at-mint NEVER-HAD population of one pool, counted from
/// the COPY's YAML alone (independent of the adapter and of the manifest):
/// per node file, each required-at-mint the v6 grammar names that the v5
/// node carries NO entry for at all — the one-of (`in_region` >= 1, or
/// `rejected_by` >= 2 plus `searched`) on a content type, never had when
/// the node carries neither an `in_region` nor a `rejected_by` entry; and
/// every `grammar_v6::REQUIRED_EXTRA` (type, kind, min). Returns
/// `(node id, edge kind, min)` per gap, the one-of reported as `in_region`.
fn never_had_required(docs: &[(String, Value)]) -> Vec<(String, String, u32)> {
    let mut out = Vec::new();
    for (id, doc) in docs {
        let ty = doc["type"].as_str().unwrap();
        let entries: Vec<Value> = doc["edges"].as_array().cloned().unwrap_or_default();
        let count = |k: &str| entries.iter().filter(|e| e["kind"] == json!(k)).count() as u32;
        if !spine::grammar_v6::mint_alternatives(ty).is_empty() {
            if count("in_region") == 0 && count("rejected_by") == 0 {
                out.push((id.clone(), "in_region".to_string(), 1));
            }
        }
        for (t, k, n) in spine::grammar_v6::REQUIRED_EXTRA {
            if t == ty && count(k) == 0 {
                out.push((id.clone(), k.to_string(), n));
            }
        }
    }
    out
}

/// The evidence for each rehearsal number, printed
/// (`DIAG` lines). Prints only; asserts nothing.
fn step0_diagnosis(
    docs: &[(String, Value)],
    m: &migrate::Manifest,
    rejected: &BTreeMap<String, Vec<String>>,
    items_by_id: &BTreeMap<&str, &SourceItem>,
    import_dir: &Path,
) {
    let doc_of = |id: &str| docs.iter().find(|(d, _)| d == id).map(|(_, v)| v);
    let reason_of = |sid: &str| match m.entry(sid).map(|e| &e.disposition) {
        Some(Disposition::Rejected { reason }) => Some(reason.clone()),
        _ => None,
    };
    // (a) the SUB- regions: each refusal's witness, verbatim.
    let subs: Vec<&String> = docs.iter().filter(|(_, d)| d["type"] == json!("region")).map(|(id, _)| id).filter(|id| id.starts_with("SUB-")).collect();
    for id in &subs {
        match reason_of(id) {
            Some(r) => println!("DIAG SUB-region {id} code={} witnesses={}", r.code, serde_json::to_string(&r.witnesses).unwrap()),
            None => println!("DIAG SUB-region {id} NOT rejected"),
        }
    }
    // (b) the non-region rejections: each one's in_region homes and their fate.
    let mut non_region_rejected = 0usize;
    let mut homes_all_sub = 0usize;
    for (id, doc) in docs {
        if doc["type"] == json!("region") || !rejected.contains_key(id) {
            continue;
        }
        non_region_rejected += 1;
        let homes: Vec<&str> = doc["edges"].as_array().map(|a| a.iter().filter(|e| e["kind"] == json!("in_region")).map(|e| e["to"].as_str().unwrap()).collect()).unwrap_or_default();
        let all_sub = !homes.is_empty() && homes.iter().all(|h| h.starts_with("SUB-") && rejected.contains_key(*h));
        if all_sub {
            homes_all_sub += 1;
        }
        let r = reason_of(id).unwrap();
        println!("DIAG non-region REJECTED {id} type={} homes={homes:?} all_homes_rejected_SUB={all_sub} witnesses={}", doc["type"].as_str().unwrap(), serde_json::to_string(&r.witnesses).unwrap());
    }
    println!("DIAG non-region rejected={non_region_rejected} of which every in_region home is a rejected SUB- region={homes_all_sub}");
    // (c) HO-1, HO-2, M-1 by name: source edges beside the manifest entry.
    for id in ["HO-1", "HO-2", "M-1"] {
        let Some(doc) = doc_of(id) else {
            println!("DIAG {id} absent from this pool");
            continue;
        };
        let edges: Vec<(String, String)> = doc["edges"].as_array().map(|a| a.iter().map(|e| (e["kind"].as_str().unwrap().to_string(), e["to"].as_str().unwrap().to_string())).collect()).unwrap_or_default();
        let disp = m.entry(id).map(|e| serde_json::to_string(&e.disposition).unwrap());
        println!("DIAG {id} type={} source_edges={edges:?}", doc["type"].as_str().unwrap());
        println!("DIAG {id} manifest={}", disp.unwrap_or_default());
    }
    // (d) the coordinator viewer's shows items.
    for e in &m.entries {
        if e.source_id.starts_with("viewer:coordinator/shows/") {
            println!("DIAG {} disposition={}", e.source_id, serde_json::to_string(&e.disposition).unwrap());
        }
    }
    // (e) the endpoint-rejected population: near end (src rejected) vs far
    // end only, by kind, with what explains the rejected endpoint.
    let cause = |id: &str| -> &'static str {
        if id.starts_with("SUB-") && rejected.contains_key(id) {
            "SUB-region"
        } else if rejected.contains_key(id) {
            "homed-only-in-SUB"
        } else {
            "NOT-REJECTED?"
        }
    };
    let (mut near, mut far): (BTreeMap<(String, &str), usize>, BTreeMap<(String, &str), usize>) = (BTreeMap::new(), BTreeMap::new());
    let mut far_local = 0usize;
    for e in &m.entries {
        if let Disposition::Imported { shape: ImportShape::Edge { kind, src, dst }, resolution: Resolution::UnresolvedEndpoint { marker, .. }, .. } = &e.disposition {
            if marker != MARKER_ENDPOINT_REJECTED {
                continue;
            }
            if rejected.contains_key(src) {
                *near.entry((kind.clone(), cause(src))).or_default() += 1;
            } else {
                *far.entry((kind.clone(), cause(dst))).or_default() += 1;
                if items_by_id.get(src.as_str()).is_some() {
                    far_local += 1;
                }
            }
        }
    }
    println!("DIAG endpoint-rejected NEAR (src rejected) by (kind, src cause)={near:?} total={}", near.values().sum::<usize>());
    println!("DIAG endpoint-rejected FAR-ONLY (src landed, dst rejected) by (kind, dst cause)={far:?} total={} (src a source item: {far_local})", far.values().sum::<usize>());
    let mut far_by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    for ((k, _), n) in &far {
        *far_by_kind.entry(k.as_str()).or_default() += n;
    }
    println!("DIAG endpoint-rejected FAR-ONLY by kind={far_by_kind:?}");
    let unexplained: Vec<&String> = rejected.keys().filter(|id| cause(id) == "NOT-REJECTED?").collect();
    let not_downstream: Vec<&String> = rejected
        .keys()
        .filter(|id| {
            let Some(doc) = doc_of(id) else { return false }; // an edge item / viewer
            if doc["type"] == json!("region") {
                return !id.starts_with("SUB-");
            }
            let homes: Vec<&str> = doc["edges"].as_array().map(|a| a.iter().filter(|e| e["kind"] == json!("in_region")).map(|e| e["to"].as_str().unwrap()).collect()).unwrap_or_default();
            !(!homes.is_empty() && homes.iter().all(|h| h.starts_with("SUB-")))
        })
        .collect();
    println!("DIAG rejections NOT downstream of the SUB- refusals={not_downstream:?} (unexplained={unexplained:?})");
    // (f) the v6-only required-at-mint population, from YAML alone.
    let never = never_had_required(docs);
    let mut by: BTreeMap<String, usize> = BTreeMap::new();
    for (_, k, _) in &never {
        *by.entry(k.clone()).or_default() += 1;
    }
    println!("DIAG v6 required-at-mint: one-of {{in_region>=1 | rejected_by>={}+{}}} on content types; AND-form {:?}; required edge fields: shows {{cite, why}} only (admits/note/rejects are LEFT_OUT)", spine::grammar_v6::REJECTED_BY_MIN, spine::grammar_v6::SEARCHED, spine::grammar_v6::REQUIRED_EXTRA);
    println!("DIAG never-had required-at-mint (from YAML) count={} by_kind={by:?} nodes={never:?}", never.len());
    let manifest: Value = serde_yaml::from_slice(&std::fs::read(import_dir.join("manifest.yaml")).unwrap()).unwrap();
    let mut shows_missing = 0usize;
    if let Some(vs) = manifest["viewers"].as_object() {
        for (_, spec) in vs {
            for n in spec["nodes"].as_array().cloned().unwrap_or_default() {
                if n.get("cite").is_none() || n.get("why").is_none() {
                    shows_missing += 1;
                }
            }
        }
    }
    println!("DIAG never-had required edge FIELDS (shows without cite or why, manifest)={shows_missing}");
}

/// Every `region` node the store holds in `pool`, by id (the destination
/// census, read after the landing).
fn census_regions(census: &checkers::DestinationCensus, pool: &PoolName) -> Vec<String> {
    census.pools[pool]
        .nodes
        .iter()
        .filter(|(_, n)| n.node_type == "region")
        .map(|(id, _)| id.0.clone())
        .collect()
}
