//! SERIAL node identifiers. A node type whose grammar sets
//! `id_prefix` gets its ids minted by the spine as `<prefix>-<n>` (e.g.
//! `T-70`, `Q-81`, `DIR-11`), serially per
//! (pool, prefix): 1, 2, 3 ... with no reuse after a removal, no gap and no
//! duplicate under concurrent submits, the counter a row in the spine file so
//! it survives a reopen and a recovery.
//!
//! The grammar here is a TEST FIXTURE that sets `id_prefix` (no real node
//! type needs to set one here), shaped only to drive these guards:
//! `task` mints `T-<n>`, `question` mints `Q-<n>`, `owner` sets no prefix
//! and keeps the hash rule; `owns` is at-most-one per owner so a statement
//! can be refused AFTER its row is appended (phase 10b), and `follows`
//! joins two tasks so one statement's edge between two of its own mints is
//! checked to land on the serials.
//!
//! Every guard was forced RED and opposed by a forcing a
//! constant implementation fails; each forcing is named in a
//! `// RED:` comment above its test.

use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::ladle::compile::{claims_for, compile, CompileCtx, Fuel};
use spine::pool::{NodeId, Pool};
use spine::read::MatchSet;
use spine::schema_def::{
    grammar_fault_tag, EdgeKindDef, FieldDef, NodeTypeDef, PoolGrammar, PoolKind, PoolName,
};
use spine::spine_db::{SpineDb, SPINE_FILE};
use spine::witness::Witness;
use spine::{Cauldron, Outcome, ReadRequest};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

test_scratch::install!();

const POOL: &str = "fixture-serial";
const POOL_B: &str = "fixture-serial-b";

fn serial_grammar(pool: &str) -> PoolGrammar {
    let mut node_types = BTreeMap::new();
    node_types.insert(
        "task".to_string(),
        NodeTypeDef {
            fields: BTreeMap::from([("name".to_string(), FieldDef::text(true, 40))]),
            id_prefix: Some("T".to_string()),
            ..Default::default()
        },
    );
    node_types.insert(
        "question".to_string(),
        NodeTypeDef {
            fields: BTreeMap::from([("name".to_string(), FieldDef::text(true, 40))]),
            id_prefix: Some("Q".to_string()),
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
    edge_kinds.insert(
        "owns".to_string(),
        EdgeKindDef {
            at_most_one_per_to: true,
            ..EdgeKindDef::local("task", "owner")
        },
    );
    edge_kinds.insert("follows".to_string(), EdgeKindDef::local("task", "task"));
    PoolGrammar {
        pool: PoolName::new(pool),
        kind: PoolKind::new("serial"),
        version: 1,
        node_types,
        edge_kinds,
    }
}

// ---------------------------------------------------------------------------
// guard 1: serial order, per prefix
// ---------------------------------------------------------------------------

/// Three `task` mints get `T-1`, `T-2`, `T-3`, and `question` mints
/// interleaved among them count `Q-1`, `Q-2` on their OWN counter; a second
/// pool pinning the same grammar counts its own `T-1`. An unprefixed type
/// keeps the hash rule.
// RED: (a) the counter keyed on a constant instead of (pool, prefix) — one
// global counter: the first `question` came back `Q-3`; (b) the counter
// keyed on prefix alone (not pool) — pool B's first task came back `T-4`;
// (c) the constant degenerate, `next = 1` always — the second task's
// landing refused after the decided flip (the door's invariant arm, `Hard`),
// so the test's `expect` panicked.
#[test]
fn three_mints_count_one_two_three_and_each_prefix_keeps_its_own_counter() {
    let dir = root("order");
    let c = fresh(&dir);
    assert_eq!(mint(&c, POOL, "task", "a"), "T-1");
    assert_eq!(mint(&c, POOL, "task", "b"), "T-2");
    assert_eq!(mint(&c, POOL, "question", "c"), "Q-1", "Q counts from 1, not from T's count");
    assert_eq!(mint(&c, POOL, "task", "d"), "T-3");
    assert_eq!(mint(&c, POOL, "question", "e"), "Q-2");
    assert_eq!(mint(&c, POOL_B, "task", "a"), "T-1", "another pool counts its own T");
    assert_eq!(mint(&c, POOL, "task", "f"), "T-4", "and pool B's mint moved nothing here");
    let owner = mint(&c, POOL, "owner", "o");
    assert!(
        owner.starts_with('n') && owner.len() == 17,
        "a type with no prefix keeps the hash rule: {owner}"
    );
    assert_eq!(
        ids(&c, POOL, "task"),
        set(&["T-1", "T-2", "T-3", "T-4"]),
        "what LANDED is the serials RETURN named"
    );
    assert_eq!(ids(&c, POOL, "question"), set(&["Q-1", "Q-2"]));
}

/// One statement minting TWO tasks and an edge between them: both serials
/// are numbered in CREATE order, and the edge lands on the serials — the
/// flip's rewrite reached the edge's endpoints, not only the CREATEs.
// RED: `CompiledPayload::assign_ids` rewriting CreateNode ids only (the
// edge arm removed) — the landing refused the edge's provisional source,
// and the door's invariant arm answered `Hard` (the `expect` panicked).
#[test]
fn one_statement_minting_two_serials_and_an_edge_between_them_lands_on_the_serials() {
    let dir = root("pair");
    let c = fresh(&dir);
    mint(&c, POOL, "task", "first");
    let out = c
        .submit(commit(
            r#"CREATE (a: fixture-serial/task { name: "a" })
               CREATE (b: fixture-serial/task { name: "b" })
               CREATE (a) -[follows]-> (b)
               RETURN a.id, b.id"#,
            &[],
        ))
        .expect("the pair lands");
    let row = first_row(&out);
    assert_eq!(row["a.id"], "T-2");
    assert_eq!(row["b.id"], "T-3");
    let followed = read_ids(
        &c,
        r#"MATCH (a: fixture-serial/task { id: "T-2" }) -[follows]-> (b: fixture-serial/task) RETURN b.id"#,
        "b.id",
    );
    assert_eq!(followed, set(&["T-3"]), "the edge landed between the two serials");
}

// ---------------------------------------------------------------------------
// guard 2: no reuse after a removal
// ---------------------------------------------------------------------------

/// Mint `T-1..T-3`, remove `T-2`, mint: `T-4`. Then remove `T-4` — now the
/// HIGHEST — and mint: `T-5`. Nodes have no removal delta (retirement
/// is a state change), so the removal is out-of-band, straight
/// on the pool file: the counter must not be derived from what the pool
/// holds.
// RED: (a) next = (live `task` nodes in the pool) + 1 — after removing
// `T-2` the next number is `T-3`, a live node's, and the landing refused
// after the decided flip (`Hard`; `expect` panicked); (b) next = (highest live serial) + 1
// — the first half held (`T-4`), and after removing `T-4` the mint came
// back `T-4` again.
#[test]
fn a_removed_serial_is_never_minted_again() {
    let dir = root("reuse");
    let c = fresh(&dir);
    for n in ["a", "b", "c"] {
        mint(&c, POOL, "task", n);
    }
    remove_node(&dir, POOL, "T-2");
    assert_eq!(ids(&c, POOL, "task"), set(&["T-1", "T-3"]), "the removal is real");
    assert_eq!(mint(&c, POOL, "task", "d"), "T-4", "T-2 is not reused, T-3 is not duplicated");
    remove_node(&dir, POOL, "T-4");
    assert_eq!(ids(&c, POOL, "task"), set(&["T-1", "T-3"]));
    assert_eq!(mint(&c, POOL, "task", "e"), "T-5", "the highest, removed, is not reused either");
}

// ---------------------------------------------------------------------------
// guard 3: the counter survives a reopen, and a recovery
// ---------------------------------------------------------------------------

/// Mint, close the cauldron, reopen it, mint: the number continues.
// RED: the counter kept in memory only (a process map keyed by pool and
// prefix, emptied when a cauldron is opened — what a restart does to
// memory): after the reopen the counter restarted at 1, `T-1` is a live
// node's, and the landing refused after the decided flip (`Hard`; `expect`
// panicked). Opposed by the first two mints, which the same forcing passes
// (`T-1`, `T-2` before the reopen).
#[test]
fn the_counter_continues_across_a_close_and_reopen() {
    let dir = root("reopen");
    let c = fresh(&dir);
    assert_eq!(mint(&c, POOL, "task", "a"), "T-1");
    assert_eq!(mint(&c, POOL, "task", "b"), "T-2");
    drop(c);
    let c = Cauldron::open(&dir, SpineConfig::default()).expect("reopen");
    assert_eq!(mint(&c, POOL, "task", "c"), "T-3");
    assert_eq!(ids(&c, POOL, "task"), set(&["T-1", "T-2", "T-3"]));
}

/// A row DECIDED and then abandoned before any landing — the crash window
/// recovery exists for — carries its serial on the ROW: the flip rewrote
/// the row's payload and its lock rows from the provisional id in the same
/// transaction that advanced the counter. Recovery lands exactly that
/// serial, and the next door mint continues after it.
// RED: (a) the flip numbering the payload but NOT writing it back to the
// row (`UPDATE transactions SET payload` removed) — the door, which lands
// the ROW's payload, landed the provisional: the very first mint came back
// `T-~a8dd3ee4373670e3`, not `T-1`; (b) the lock-row
// rewrite removed — the lock keys still named the provisional id.
#[test]
fn a_decided_row_carries_its_serial_and_recovery_lands_it() {
    let dir = root("recover");
    let c = fresh(&dir);
    assert_eq!(mint(&c, POOL, "task", "a"), "T-1");

    // Stage the crash state by hand (test-support): compile, append, take
    // the claims, flip decided — and stop, the way a door killed after its
    // decided flip stops.
    let env = commit(r#"CREATE (t: fixture-serial/task { name: "crashed" }) RETURN t.id"#, &[]);
    let stmt = env.validate().expect("parse");
    let hash = env.hash_identity().expect("hash");
    let g = serial_grammar(POOL);
    let gb = serial_grammar(POOL_B);
    let grammars = |p: &PoolName| -> Option<PoolGrammar> {
        [&g, &gb].into_iter().find(|x| x.pool == *p).cloned()
    };
    let kinds = |k: &PoolKind| -> Vec<PoolGrammar> {
        [&g, &gb].into_iter().filter(|x| x.kind == *k).cloned().collect()
    };
    let mut fuel = Fuel::new(SpineConfig::default().fuel_ceiling);
    let payload = {
        let mut ctx = CompileCtx {
            grammars: &grammars,
            pools_of_kind: &kinds,
            fuel: &mut fuel,
        };
        compile(&stmt, &env.bindings, &MatchSet::default(), &hash, &mut ctx).expect("compile")
    };
    let provisional = payload.minted["t"].id.0.clone();
    assert!(
        provisional.starts_with("T-~"),
        "the compiler mints the PROVISIONAL, never a number: {provisional}"
    );
    let claims = claims_for(&stmt, &MatchSet::default(), &payload, &grammars).expect("claims");
    let mut spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let seq = spine.append_pending(&env, &hash, &payload).expect("append");
    spine::locks::acquire(&mut spine, seq, &claims, &SpineConfig::default()).expect("locks");
    assert!(
        lock_keys(&dir, seq.0).iter().any(|k| k.contains(&provisional)),
        "sentinel: before the flip the locks name the provisional id"
    );
    spine.mark_decided(seq).expect("decided");

    let row = spine.row_at(seq).expect("row");
    assert_eq!(row.payload.minted["t"].id, NodeId("T-2".into()), "the ROW carries the serial");
    assert!(
        !serde_json::to_string(&row.payload).unwrap().contains("T-~"),
        "no provisional id survives on a decided row"
    );
    let keys = lock_keys(&dir, seq.0);
    assert!(keys.iter().any(|k| k == "body:T-2"), "the lock rows name the serial: {keys:?}");
    assert!(!keys.iter().any(|k| k.contains("T-~")), "and not the provisional: {keys:?}");
    drop(spine);

    let report = c.recover().expect("recover");
    assert_eq!(report.rolled_forward, vec![hash], "recovery rolled the staged row forward");
    assert_eq!(ids(&c, POOL, "task"), set(&["T-1", "T-2"]), "recovery landed the serial");
    assert_eq!(mint(&c, POOL, "task", "after"), "T-3", "and the counter continues past it");
}

// ---------------------------------------------------------------------------
// guard 4: a refused or rolled-back attempt consumes no number
// ---------------------------------------------------------------------------

/// Three refusals, each with a `task` mint in it, consume nothing: one
/// refused at compile (an over-cap name), one dry run (Validate mode), and
/// one refused AFTER ITS ROW IS APPENDED — a second `owns` toward an owner
/// already owned, which the door judges under its granted locks (phase
/// 10b) and records ABORTED. The next real mint is `T-2`.
// RED: the serial taken at the APPEND (the counter advanced and the row's
// payload rewritten inside `append_or_existing`) instead of at the decided
// flip — the aborted attempt consumed `T-2` and the next mint came back
// `T-3`. Opposed by the first mint, `T-1`, which that forcing passes.
#[test]
fn a_refused_or_aborted_mint_consumes_no_number() {
    let dir = root("abort");
    let c = fresh(&dir);
    let owner = mint(&c, POOL, "owner", "o");
    let owned = c
        .submit(commit(
            r#"MATCH (o: fixture-serial/owner { id: $o })
               CREATE (t: fixture-serial/task { name: "holder" })
               CREATE (t) -[owns]-> (o)
               RETURN t.id"#,
            &[("o", json(&owner))],
        ))
        .expect("the first owner lands");
    assert_eq!(first_row(&owned)["t.id"], "T-1");

    // (1) refused at compile, before any row.
    let w = refused(c.submit(commit(
        r#"CREATE (t: fixture-serial/task { name: $n }) RETURN t.id"#,
        &[("n", json("x".repeat(41)))],
    )));
    assert!(matches!(w, Witness::OverCap { .. }), "{w:?}");

    // (2) a dry run.
    let mut dry = commit(r#"CREATE (t: fixture-serial/task { name: "dry" }) RETURN t.id"#, &[]);
    dry.mode = Mode::Validate;
    assert!(matches!(c.submit(dry), Ok(Outcome::Validated { .. })));

    // (3) refused after the append: the row exists and is aborted.
    let env = commit(
        r#"MATCH (o: fixture-serial/owner { id: $o })
           CREATE (t: fixture-serial/task { name: "usurper" })
           CREATE (t) -[owns]-> (o)
           RETURN t.id"#,
        &[("o", json(&owner))],
    );
    let hash = env.hash_identity().expect("hash");
    let w = refused(c.submit(env));
    assert_eq!(grammar_fault_tag(&w), Some("at_most_one_violated"), "{w:?}");
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let attempts = spine.attempts_of(&hash).expect("attempts");
    assert_eq!(attempts.len(), 1, "sentinel: the refused attempt DID reach the append");
    assert!(
        !attempts[0].decided && attempts[0].cleared,
        "and its row is recorded aborted: {:?}",
        attempts[0]
    );
    drop(spine);

    assert_eq!(mint(&c, POOL, "task", "next"), "T-2", "no refusal consumed a number");
    assert_eq!(ids(&c, POOL, "task"), set(&["T-1", "T-2"]));
}

// ---------------------------------------------------------------------------
// guard 5: concurrent writers, N >= 2 — no duplicate, no gap
// ---------------------------------------------------------------------------

/// `WRITERS` threads, each with its OWN cauldron handle on one directory
/// (so its own spine connections), each minting `PER` tasks at once. Every
/// submit commits, and the serials handed back are exactly `T-1` through
/// `T-(WRITERS*PER)`: none twice, none skipped — and the pool holds exactly
/// those.
// RED: the counter READ OUTSIDE the flip's transaction (read on the
// connection in autocommit, a 1 ms yield, then the IMMEDIATE flip writes
// read + 1) — interleaved flips took the same number and 20 of the 48
// submits failed with the door's invariant arm (a landing refused after
// the decided flip; the all-Ok assertion red).
// Opposed by the single-writer test above, which that forcing passes.
#[test]
fn concurrent_writers_mint_no_duplicate_and_no_gap() {
    const WRITERS: usize = 6;
    const PER: usize = 8;
    let dir = root("concurrent");
    drop(fresh(&dir));
    let results: Vec<Result<String, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..WRITERS)
            .map(|w| {
                let dir = dir.clone();
                scope.spawn(move || {
                    let c = Cauldron::open(&dir, patient()).expect("open");
                    (0..PER)
                        .map(|i| {
                            c.submit(commit(
                                r#"CREATE (t: fixture-serial/task { name: $n }) RETURN t.id"#,
                                &[("n", json(format!("w{w}-{i}")))],
                            ))
                            .map(|o| first_row(&o)["t.id"].as_str().unwrap().to_string())
                            .map_err(|e| format!("{e:?}"))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("join")).collect()
    });
    let failed: Vec<&String> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert!(failed.is_empty(), "every concurrent mint commits: {failed:?}");
    let got: Vec<String> = results.into_iter().map(Result::unwrap).collect();
    let distinct: BTreeSet<String> = got.iter().cloned().collect();
    assert_eq!(distinct.len(), got.len(), "no serial handed out twice: {got:?}");
    let want: BTreeSet<String> = (1..=WRITERS * PER).map(|n| format!("T-{n}")).collect();
    assert_eq!(distinct, want, "no gap: exactly T-1..T-{}", WRITERS * PER);
    let c = Cauldron::open(&dir, SpineConfig::default()).expect("open");
    assert_eq!(ids(&c, POOL, "task"), want, "and the pool holds exactly those");
}

// ---------------------------------------------------------------------------
// guard 6: a caller-supplied id with the wrong prefix
// ---------------------------------------------------------------------------

/// A CREATE of a prefixed type naming an id WITHOUT the type's prefix is
/// refused by the prefix rule — `id_prefix_mismatch`, the witness naming the
/// prefix the type mints (`T-<n>`). Opposed both ways: an id that DOES
/// carry the prefix is refused too, but as store-minted (the spine numbers
/// a prefixed type; a caller does not), not as a prefix mismatch; and an
/// unprefixed type's caller id is refused store-minted as it always was.
/// Nothing lands and no number is consumed.
// RED: (a) the `caller_id_against_prefix` call removed from
// `emit_create_node` — `Q-1` on a task refused store-minted, the tag
// assertion failed; (b) the constant degenerate, `caller_id_against_prefix`
// refusing `id_prefix_mismatch` for EVERY caller id on a prefixed type — the
// `T-9` opposition came back `id_prefix_mismatch`.
#[test]
fn a_caller_id_without_the_types_prefix_is_refused_naming_the_prefix() {
    let dir = root("caller-id");
    let c = fresh(&dir);
    let create = |node_type: &str, id: &str| {
        refused(c.submit(commit(
            &format!(r#"CREATE (t: fixture-serial/{node_type} {{ id: $id, name: "x" }}) RETURN t.id"#),
            &[("id", json(id))],
        )))
    };

    for wrong in ["Q-1", "n0123456789abcdef", "TT-1", "T1"] {
        let w = create("task", wrong);
        assert_eq!(grammar_fault_tag(&w), Some("id_prefix_mismatch"), "{wrong}: {w:?}");
        let Witness::GrammarViolation { pool, message, .. } = &w else {
            panic!("a grammar violation: {w:?}")
        };
        assert_eq!(pool, &PoolName::new(POOL));
        assert!(message.contains("`T-<n>`"), "the witness names the expected prefix: {message}");
        assert!(message.contains(wrong), "and the id it refused: {message}");
    }

    let w = create("task", "T-9");
    assert_ne!(grammar_fault_tag(&w), Some("id_prefix_mismatch"), "{w:?}");
    assert!(
        matches!(&w, Witness::GrammarViolation { message, .. } if message.contains("store-minted")),
        "a carried prefix is still the spine's to mint: {w:?}"
    );
    let w = create("owner", "X-1");
    assert_ne!(grammar_fault_tag(&w), Some("id_prefix_mismatch"), "{w:?}");
    assert!(
        matches!(&w, Witness::GrammarViolation { message, .. } if message.contains("store-minted")),
        "an unprefixed type judges no prefix: {w:?}"
    );

    assert!(ids(&c, POOL, "task").is_empty(), "nothing landed");
    assert_eq!(mint(&c, POOL, "task", "first"), "T-1", "and nothing consumed a number");
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-serial-ids-{name}-{}", std::process::id()))
}

fn fresh(dir: &Path) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(
        dir,
        &[serial_grammar(POOL), serial_grammar(POOL_B)],
        SpineConfig::default(),
    )
    .expect("create the cauldron")
}

/// A long lock ceiling for the concurrent writers: contention is the point,
/// and a writer timing out in the queue is not what the test measures.
fn patient() -> SpineConfig {
    SpineConfig {
        lock_wait_ceiling: std::time::Duration::from_secs(60),
        ..SpineConfig::default()
    }
}

fn json(v: impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(v).expect("json")
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
                thread: "serial-ids".into(),
                dispatch: "serial-ids".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

/// Mint one node of `node_type` named `name` through the door; its id.
fn mint(c: &Cauldron, pool: &str, node_type: &str, name: &str) -> String {
    let out = c
        .submit(commit(
            &format!(r#"CREATE (t: {pool}/{node_type} {{ name: $n }}) RETURN t.id"#),
            &[("n", json(name))],
        ))
        .unwrap_or_else(|e| panic!("minting {node_type} {name:?} in {pool} must commit: {e:?}"));
    first_row(&out)["t.id"].as_str().expect("id").to_string()
}

fn first_row(out: &Outcome) -> BTreeMap<String, serde_json::Value> {
    match out {
        Outcome::Committed { returned, .. } => returned.0[0].clone(),
        other => panic!("expected Committed, got {other:?}"),
    }
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

fn read_ids(c: &Cauldron, statement: &str, column: &str) -> BTreeSet<String> {
    c.read(&ReadRequest {
        statement: statement.to_string(),
        bindings: Bindings::new(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    })
    .expect("read")
    .rows
    .0
    .iter()
    .map(|r| r[column].as_str().expect("id").to_string())
    .collect()
}

/// Every id of `node_type` the pool holds, through the read door.
fn ids(c: &Cauldron, pool: &str, node_type: &str) -> BTreeSet<String> {
    read_ids(c, &format!("MATCH (n: {pool}/{node_type}) RETURN n.id"), "n.id")
}

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

/// Remove a node OUT OF BAND, straight on the pool file — there is no
/// node-removal delta, and the point is a removal the spine's
/// counter never hears about.
fn remove_node(dir: &Path, pool: &str, id: &str) {
    let conn = rusqlite::Connection::open(Pool::path_in(dir, &PoolName::new(pool))).expect("pool");
    let fields = conn
        .execute("DELETE FROM node_fields WHERE node_id = ?1", [id])
        .expect("fields");
    let nodes = conn.execute("DELETE FROM nodes WHERE id = ?1", [id]).expect("node");
    assert_eq!(nodes, 1, "removed exactly one node `{id}` ({fields} field rows)");
}

/// The resource keys of one row's lock entries, straight from the spine file.
fn lock_keys(dir: &Path, seq: i64) -> Vec<String> {
    let conn = rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("spine");
    let mut st = conn
        .prepare("SELECT resource FROM locks WHERE seq = ?1 ORDER BY resource")
        .expect("prepare");
    let keys = st
        .query_map([seq], |r| r.get::<_, String>(0))
        .expect("query")
        .map(|k| k.expect("key"))
        .collect();
    keys
}
