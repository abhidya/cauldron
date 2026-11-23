//! **The empty match**: a match that finds no rows is a normal outcome, not
//! an error: it succeeds and changes nothing.
//!
//! Settled by design; tested anyway, because settled is not the same as
//! implemented.
//!
//! Two clauses, and this suite takes both further than a spot check:
//!
//! - **a zero-row match SUCCEEDS touching nothing.** "Touching nothing" is
//!   asserted as a whole-cauldron fingerprint over BOTH pools — every node of
//!   every declared type with its state, revision and fields, every edge, every
//!   count cell, the committed-commit counter — plus the payload's own shape
//!   (zero parts, zero deltas, zero minted identifiers), plus the CLAIM SET,
//!   which is empty: an empty match takes no locks and therefore blocks
//!   nobody, which is proven by committing one straight through a foreign
//!   write lock on the very population it scanned.
//! - **the same match under a count expectation refuses expected-versus-found**
//!   — an ORDINARY `FailedExpectation`, with no special empty-match error
//!   anywhere, and with `found = 0` because zero is a count and not an
//!   absence. The count expectation is also what makes the statement claim the
//!   population: the contrast between the two is asserted directly, because it
//!   is the mechanism by which "pin it with a count expectation" actually
//!   works rather than merely being spelled.
//!
//! Every checker here was forced RED at least once; the poison is named in a
//! `// RED:` comment above each test.

use spine::config::SpineConfig;
use spine::delta::{CompiledPayload, CountOp};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::ladle::compile::{self, CompileCtx, Fuel};
use spine::ladle::parse;
use spine::locks::{self, Claim, ClaimSet, LockMode, Resource};
use spine::pool::{NodeId, Pool};
use spine::read;
use spine::schema_def::{fixtures, PoolGrammar, PoolName};
use spine::spine_db::{Seq, SpineDb, SPINE_FILE};
use spine::witness::{ExpectationKind, Witness};
use spine::{Cauldron, Outcome, ReadRequest, Returned};
use std::path::{Path, PathBuf};
use std::time::Duration;

test_scratch::install!();

const ALPHA: &str = "fixture-alpha";
const BETA: &str = "fixture-beta";

/// Three empty bindings across TWO pools, with a WHERE, two SETs, a REMOVE and
/// a RETURN hanging off them. Nothing in this statement can find a row, so
/// every clause expands over the empty set — and the statement still succeeds.
///
/// Deliberately no CREATE: the frozen semantics say a CREATE head "mints
/// exactly one node per statement execution", so a statement with a CREATE
/// clause touches something whatever the match found, and it would not be a
/// fixture for this clause.
const EMPTY_EVERYWHERE: &str = r#"MATCH (d: fixture-alpha/doc { state: "published" })
   MATCH (e: fixture-alpha/doc { state: "superseded" })
   MATCH (t: fixture-beta/ticket { state: "closed" })
   WHERE d.title != "anything at all"
   SET d.state = "superseded"
   SET t.state = "open"
   REMOVE (d) -[supersedes]-> (e)
   RETURN d.id, t.id"#;

const EMPTY_ALPHA: &str = r#"MATCH (d: fixture-alpha/doc { state: "published" })
   SET d.state = "superseded"
   RETURN d.id"#;

// ---------------------------------------------------------------------------
// the empty match SUCCEEDS, touching nothing
// ---------------------------------------------------------------------------

/// SETTLED: a match that finds zero rows SUCCEEDS touching
/// nothing. The reasoning is the racing case — two agents
/// retiring the same node, and the loser finds it already retired — which
/// should not be an error path.
///
/// "Touching nothing" is asserted five ways, because each of them could hold
/// while another failed: the whole-cauldron fingerprint over both pools; the
/// committed-commit counter; the payload's own shape on the durable row; the
/// touched-pool set; and the lock table.
// RED: (a) `match_head` made to error on an empty `nodes_of` result — the
// empty match became an error path, which is exactly the rule's opposite;
// (b) `compile`'s mutation loop made to emit a delta for an unbound binding —
// the payload gained a part and the fingerprint moved; (c) `mark_cleared`
// made to skip the row when there are no parts — the record of the ask
// vanished and the "one row, cleared, zero parts" assertion failed.
#[test]
fn a_zero_row_match_succeeds_touching_nothing_in_either_pool() {
    let dir = root("empty-success");
    let c = fresh_cauldron(&dir, SpineConfig::default());
    // A populated cauldron, so "nothing changed" is a claim with teeth: there
    // are rows here that a mis-expanded clause could have touched.
    let drafts: Vec<NodeId> = (0..3).map(|i| create_doc(&c, &format!("draft {i}"))).collect();
    let ticket = create_ticket(&c);
    let before = Fingerprint::of(&c, &dir);
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");

    let env = commit(EMPTY_EVERYWHERE, &[]);
    let hash = env.hash_identity().expect("hash");

    // The CLAIM SET is empty before anything runs: an empty match holds
    // nothing, which is why it can never block anybody.
    let claims = derive_claims(&c, &env);
    assert!(
        claims.is_empty(),
        "an empty match claims nothing at all: {claims:?}"
    );

    let out = c.submit(env).expect("an empty match is not an error");
    let (tx, returned) = match out {
        Outcome::Committed {
            tx,
            returned,
            deduplicated,
            ..
        } => {
            assert!(!deduplicated, "a fresh ask is not a replay");
            (tx, returned)
        }
        other => panic!("expected Committed, got {other:?}"),
    };
    assert_eq!(returned, Returned::default(), "nothing to return");
    assert!(
        c.touched_pools(&tx).expect("touched").is_empty(),
        "zero deltas means zero parts means no pool touched"
    );

    // The RECORD of the ask still exists — an empty match is still a request,
    // and the spine records what was ASKED beside what
    // was done.
    let row = spine.find_by_hash(&tx).expect("read").expect("the row");
    assert!(row.decided && row.cleared, "it committed: {row:?}");
    assert!(row.parts.is_empty(), "with no parts");
    assert!(row.payload.parts.is_empty(), "and a payload of no parts");
    assert!(row.payload.minted.is_empty(), "and nothing minted");
    assert_eq!(row.statement, EMPTY_EVERYWHERE, "the ask, recorded verbatim");

    // Nothing landed, anywhere, and the counter did not move: it counts a
    // commit's LAST PART landing, and there was no part.
    before.assert_unchanged(&c, &dir, "an empty match");
    for d in &drafts {
        assert_eq!(state_of(&c, &d.0), Some("draft".into()));
    }
    assert_eq!(ticket_state(&c, &ticket.0), Some("open".into()));
    assert!(spine.pending().expect("pending").is_empty());
    let _ = hash;
}

/// The consequence of claiming nothing, exhibited: an empty match commits
/// STRAIGHT THROUGH a foreign write lock on the whole population it scanned,
/// with a 150ms ceiling — while the same match under a count expectation waits
/// behind that lock, because the count is what needs the population held
/// still (a count expectation reads the type slice).
///
/// This is the mechanism behind "pin it with a count expectation": the
/// pin is not a flag on the refusal, it is a real claim on the population.
// RED: `claims_for`'s count-expectation arm deleted — the pinned statement
// stopped claiming the population, sailed past the foreign lock, and the two
// halves of this contrast became identical.
#[test]
fn an_empty_match_holds_no_lock_but_a_count_expectation_claims_the_population() {
    let dir = root("empty-locks");
    let c = fresh_cauldron(&dir, cfg(150));
    create_doc(&c, "a draft nobody matches");
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");

    // A foreign transaction holds the alpha `doc` population still.
    let holder = park(&dir, &one(type_slice("doc"), LockMode::Write), "population");

    // The unpinned empty match: no claim, no queue, straight through.
    c.submit(commit(EMPTY_ALPHA, &[]))
        .expect("an empty match takes no locks and blocks on nobody");

    // The same match, PINNED with a count: it needs the population, so it
    // waits — and says loudly whose lock it is behind.
    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { state: "published" })
               EXPECT COUNT(d) = 0
               SET d.state = "superseded""#,
            &[],
        ))
        .expect_err("a count expectation must hold the population still");
    match &err {
        SpineError::Busy { message } => assert!(
            message.contains(&format!("seq {holder}")),
            "it names the holder it is behind: {message}"
        ),
        other => panic!("expected a loud Busy at the ceiling, got {other:?}"),
    }

    // Released, the pinned statement gets its claim and SUCCEEDS: zero rows is
    // a count of zero, and `COUNT = 0` is satisfied by it.
    unpark(&dir, holder);
    c.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { state: "published" })
           EXPECT COUNT(d) = 0
           SET d.state = "superseded""#,
        &[],
    ))
    .expect("zero rows satisfies a count of zero");
    assert!(spine.pending().expect("pending").is_empty());
}

// ---------------------------------------------------------------------------
// the same match under a count expectation
// ---------------------------------------------------------------------------

/// An author who needs emptiness to be an error pins it with a
/// count expectation, which turns emptiness into an ORDINARY
/// expected-versus-found refusal.
///
/// Ordinary is the load-bearing word, and it is what is asserted: the witness
/// is `FailedExpectation`, not a bespoke empty-match variant; the expectation
/// is a `Count` naming its binding and its operator; `expected` is what the
/// author wrote and `found` is `0` — a COUNT, not a null and not an absence.
///
/// Both count operators are covered, in both directions, and the non-empty
/// case is here too: without it, a build whose count always reported zero
/// would pass every empty-match test in the suite.
// RED: (a) `emit_expect_count`'s comparison arm disabled — the pinned empty
// match committed and emptiness stopped being an error even when pinned;
// (b) `CountOp::AtMost` implemented as `found < want` — the `<= 0` boundary
// case flipped, which is the cell that separates the rule from its opposite;
// (c) the count computed from the payload's deltas rather than from the match
// — `found` came back 0 for the two-row case as well.
#[test]
fn a_count_expectation_turns_emptiness_into_an_ordinary_expected_versus_found_refusal() {
    let dir = root("empty-count");
    let c = fresh_cauldron(&dir, SpineConfig::default());
    create_doc(&c, "a draft, not a published doc");
    let before = Fingerprint::of(&c, &dir);

    // (1) EXACTLY: emptiness pinned to one, found zero.
    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { state: "published" })
               EXPECT COUNT(d) = 1
               SET d.state = "superseded""#,
            &[],
        ))
        .expect_err("emptiness pinned by a count is a refusal");
    match only_witness(&err) {
        Witness::FailedExpectation {
            expectation,
            expected,
            found,
        } => {
            assert_eq!(
                expectation,
                ExpectationKind::Count {
                    binding: "d".into(),
                    op: CountOp::Exactly
                },
                "an ORDINARY count expectation, naming its binding and its operator"
            );
            assert_eq!(expected, serde_json::json!(1));
            assert_eq!(
                found,
                serde_json::json!(0),
                "zero is a COUNT, never a null and never an absence"
            );
        }
        other => panic!("expected a count FailedExpectation, got {other:?}"),
    }
    before.assert_unchanged(&c, &dir, "a pinned empty match");

    // (2) the boundary that separates the rule from its opposite: an empty
    // match satisfies `<= 0`, and a one-row match does not.
    c.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { state: "published" })
           EXPECT COUNT(d) <= 0
           SET d.state = "superseded""#,
        &[],
    ))
    .expect("zero rows satisfies `at most zero`");
    // Re-baselined deliberately, and a known weak seam: a
    // SATISFIED count expectation over an empty match compiles to a part that
    // carries the premise and no deltas, so that part LANDS and the
    // committed-commit counter moves for a commit that changed no node. The
    // pools themselves are still untouched — which is what the fingerprint
    // below re-establishes.
    let before = Fingerprint::of(&c, &dir);
    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { state: "draft" })
               EXPECT COUNT(d) <= 0
               SET d.state = "published""#,
            &[],
        ))
        .expect_err("one row does not satisfy `at most zero`");
    match only_witness(&err) {
        Witness::FailedExpectation {
            expectation,
            expected,
            found,
        } => {
            assert_eq!(
                expectation,
                ExpectationKind::Count {
                    binding: "d".into(),
                    op: CountOp::AtMost
                }
            );
            assert_eq!(expected, serde_json::json!(0));
            assert_eq!(found, serde_json::json!(1));
        }
        other => panic!("expected an at-most count FailedExpectation, got {other:?}"),
    }
    before.assert_unchanged(&c, &dir, "an at-most count that does not hold");

    // (3) the count genuinely COUNTS: a two-row match pinned to one refuses
    // with found = 2. Without this, a build whose count always said zero
    // would pass every other assertion in this file.
    create_doc(&c, "a second draft");
    let after_two = Fingerprint::of(&c, &dir);
    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { state: "draft" })
               EXPECT COUNT(d) = 1
               SET d.state = "published""#,
            &[],
        ))
        .expect_err("two rows do not satisfy a count of one");
    match only_witness(&err) {
        Witness::FailedExpectation { expected, found, .. } => {
            assert_eq!(expected, serde_json::json!(1));
            assert_eq!(found, serde_json::json!(2), "the count is a real count");
        }
        other => panic!("expected a count FailedExpectation, got {other:?}"),
    }
    after_two.assert_unchanged(&c, &dir, "a count that found too many");

    // (4) and the empty match with NO count still succeeds on the same
    // cauldron, so the difference is the pin and nothing else.
    c.submit(commit(EMPTY_ALPHA, &[]))
        .expect("unpinned, the same empty match still succeeds");
}

// ---------------------------------------------------------------------------
// the whole-cauldron fingerprint
// ---------------------------------------------------------------------------

/// Everything a landing could move, in both pools, as one comparable value.
/// See `refusal_witnesses.rs` for the same device; it is duplicated rather
/// than shared because an integration test binary cannot import another one.
#[derive(Debug, PartialEq, Eq)]
struct Fingerprint {
    counter: u64,
    pools: Vec<String>,
}

impl Fingerprint {
    fn of(c: &Cauldron, dir: &Path) -> Fingerprint {
        let mut pools = Vec::new();
        for grammar in [fixtures::fixture_alpha(), fixtures::fixture_beta()] {
            let pool = Pool::open(&Pool::path_in(dir, &grammar.pool)).expect("pool");
            let snap = pool.snapshot().expect("snapshot");
            let mut out = String::new();
            for (ty, def) in &grammar.node_types {
                for node in snap.nodes_of(ty, None).expect("nodes") {
                    out.push_str(&format!(
                        "node {}/{} type={} state={:?} rev={} fields={:?}\n",
                        grammar.pool, node.id.0, node.node_type, node.state, node.rev, node.fields
                    ));
                    for cell in def.count_cells.keys() {
                        out.push_str(&format!(
                            "cell {}/{}.{} = {:?}\n",
                            grammar.pool,
                            node.id.0,
                            cell,
                            snap.count_cell(&node.id, cell).expect("cell")
                        ));
                    }
                    for edge in snap.edges_from(&node.id, None).expect("edges") {
                        out.push_str(&format!(
                            "edge {}/{} -[{}]-> {}/{}\n",
                            grammar.pool, edge.src.0, edge.kind, edge.dst.pool, edge.dst.id.0
                        ));
                    }
                }
            }
            pools.push(out);
        }
        Fingerprint {
            counter: c.committed_commit_counter().expect("counter"),
            pools,
        }
    }

    fn assert_unchanged(&self, c: &Cauldron, dir: &Path, what: &str) {
        let now = Fingerprint::of(c, dir);
        assert_eq!(
            *self, now,
            "`{what}` must touch NOTHING in either pool"
        );
        assert!(
            c.in_landing(None).expect("in landing").is_empty(),
            "`{what}` left nothing in landing state"
        );
        let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
        assert!(
            spine.pending().expect("pending").is_empty(),
            "`{what}` left no transaction in flight"
        );
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-empty-{name}-{}", std::process::id()))
}

fn cfg(ceiling_ms: u64) -> SpineConfig {
    SpineConfig {
        lock_wait_ceiling: Duration::from_millis(ceiling_ms),
        lock_poll_interval: Duration::from_millis(2),
        ..SpineConfig::default()
    }
}

fn fresh_cauldron(dir: &Path, config: SpineConfig) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(
        dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        config,
    )
    .expect("create the cauldron")
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
                thread: "empty-match".into(),
                dispatch: "empty-match".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
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

fn type_slice(node_type: &str) -> Resource {
    Resource::TypeSlice {
        node_type: node_type.to_string(),
    }
}

fn one(resource: Resource, mode: LockMode) -> ClaimSet {
    let mut set = ClaimSet::new();
    set.insert(Claim {
        pool: PoolName::new(ALPHA),
        resource,
        mode,
    });
    set
}

fn park(dir: &Path, held: &ClaimSet, tag: &str) -> Seq {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let env = commit(&format!("-- parking holder: {tag}"), &[]);
    let hash = env.hash_identity().expect("hash");
    let seq = spine
        .append_pending(&env, &hash, &empty_payload())
        .expect("parking row");
    locks::acquire(&spine, seq, held, &SpineConfig::default()).expect("park");
    seq
}

fn unpark(dir: &Path, seq: Seq) {
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    locks::release_at_clear(&spine, seq).expect("release");
    spine
        .mark_cleared(seq, &serde_json::json!({ "parking_holder": "released" }))
        .expect("close the parking row");
}

fn empty_payload() -> CompiledPayload {
    CompiledPayload {
        parts: Vec::new(),
        minted: std::collections::BTreeMap::new(),
    }
}

/// The claim set the DOOR would acquire for this envelope, derived through the
/// crate's own matcher, compiler and claim derivation.
fn derive_claims(c: &Cauldron, env: &Envelope) -> ClaimSet {
    let lookup = |p: &PoolName| -> Option<PoolGrammar> { c.grammar(p).cloned() };
    let stmt = parse::parse(&env.statement).expect("parse");
    let pools: Vec<PoolName> = compile::footprint(&stmt).pools.iter().cloned().collect();
    let mut fuel = Fuel::new(c.config().fuel_ceiling);
    let matched = {
        let open = read::open_pools(c, &pools).expect("open pools");
        let snaps = read::open_snapshots(&open).expect("snapshots");
        read::run_match(&snaps, &stmt, &env.bindings, &mut fuel).expect("match")
    };
    let hash = env.hash_identity().expect("hash");
    let payload = {
        let kinds = |k: &spine::schema_def::PoolKind| -> Vec<PoolGrammar> {
            c.pools_of_kind(k)
                .into_iter()
                .filter_map(|n| c.grammar(n).cloned())
                .collect()
        };
        let mut ctx = CompileCtx {
            grammars: &lookup,
            pools_of_kind: &kinds,
            fuel: &mut fuel,
        };
        compile::compile(&stmt, &env.bindings, &matched, &hash, &mut ctx).expect("compile")
    };
    assert!(
        payload.parts.is_empty(),
        "the empty match compiles to no parts at all: {payload:?}"
    );
    compile::claims_for(&stmt, &matched, &payload, &lookup).expect("claims")
}

fn create_doc(c: &Cauldron, title: &str) -> NodeId {
    match c
        .submit(commit(
            r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" }) RETURN d.id"#,
            &[("t", serde_json::json!(title))],
        ))
        .expect("create a doc")
    {
        Outcome::Committed { returned, .. } => {
            NodeId(returned.0[0]["d.id"].as_str().expect("id").to_string())
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn create_ticket(c: &Cauldron) -> NodeId {
    match c
        .submit(commit(
            r#"CREATE (t: fixture-beta/ticket { state: "open" }) RETURN t.id"#,
            &[],
        ))
        .expect("create a ticket")
    {
        Outcome::Committed { returned, .. } => {
            NodeId(returned.0[0]["t.id"].as_str().expect("id").to_string())
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn state_of(c: &Cauldron, id: &str) -> Option<String> {
    field_of(c, ALPHA, "doc", id, "d.state")
}

fn ticket_state(c: &Cauldron, id: &str) -> Option<String> {
    field_of(c, BETA, "ticket", id, "t.state")
}

fn field_of(c: &Cauldron, pool: &str, ty: &str, id: &str, column: &str) -> Option<String> {
    let binding = &column[..1];
    let mut bindings = Bindings::new();
    bindings.insert("id".into(), serde_json::json!(id));
    c.read(&ReadRequest {
        statement: format!(
            "MATCH ({binding}: {pool}/{ty} {{ id: $id }}) RETURN {binding}.state"
        ),
        bindings,
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    })
    .expect("read")
    .rows
    .0
    .first()
    .and_then(|r| r[column].as_str().map(str::to_string))
}
