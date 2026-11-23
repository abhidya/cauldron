//! The MIGRATION REHEARSAL — fixture-based and METHOD-INDIFFERENT.
//!
//! Fixture-based and method-indifferent: validate-then-commit over a
//! fixture source, asserting the five ledger properties — total accounting,
//! referential closure, one-door provenance, atomic-or-cleanly-aborted,
//! source byte-identical — and never old-equals-new, which is barred by
//! design.
//!
//! # The source here is a FIXTURE, and the adapter is a demonstration
//!
//! The corpus below is a handful of JSON files this test writes into a
//! scratch directory and an adapter that reads them back into
//! `migrate::SourceItem`s. It stands in for the REGENERATION method. A REPLAY
//! adapter would produce the same item type from a message stream and the
//! driver could not tell the difference — which is the whole point. The
//! method is now SETTLED (regenerate): the refusing
//! `migrate::import_live_pools` and its sentinel test are retired, and the
//! real adapter is `migrate::import_v5_pool` (tests/import_v5.rs). Nothing
//! here reads a live pool and nothing here knows a live pool's path.
//!
//! # The barred assertion
//!
//! There is NO old-equals-new assertion in this file, not even as a helper
//! (SETTLED: the design removed the oracle). The source's CONTENT
//! is never compared with the destination's. The only question this file asks
//! about the source is a byte digest, which is a question about whether the
//! source was WRITTEN, not about what it means.
//!
//! # Every checker forced red
//!
//! A checker with no red fixture does not count as existing.
//! Each of the five checkers has a `red_*` test below that poisons a fixture
//! until it fires, and each poison is named in the test's own doc comment.
//! Three of the five poisons are REAL damage to a
//! real database or a real file, not a mutated struct.

use spine::checkers::{self, Finding, Verdict};
use spine::config::SpineConfig;
use spine::envelope::{Initiator, TxHash};
use spine::migrate::{
    self, Disposition, ImportShape, Landing, Manifest, MigrationPlan, MigrationRun, Resolution,
    RunOutcome, SourceEdge, SourceItem, SourceNode,
};
use spine::pool::NodeId;
use spine::schema_def::{fixtures, PoolName};
use spine::witness::Witness;
use spine::Cauldron;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

test_scratch::install!();

const ALPHA: &str = "fixture-alpha";
const BETA: &str = "fixture-beta";

/// How many items the fixture corpus yields. Written as a LITERAL and
/// asserted against the corpus builder, because total accounting compares the
/// manifest with this number and a count derived from the manifest would
/// prove nothing.
const SOURCE_ITEMS: usize = 14;
/// Of those: two node items violate the destination grammar.
const REJECTED_ITEMS: usize = 2;
/// Of those: two edge items dangle and import with a marker, landing nothing.
const UNRESOLVED_ITEMS: usize = 2;
/// What actually lands: six nodes and four edges.
const LANDED_NODES: usize = 6;
const LANDED_EDGES: usize = 4;

// ---------------------------------------------------------------------------
// the rehearsal proper
// ---------------------------------------------------------------------------

/// The whole migration shape, end to end: validate-then-commit over a fixture
/// source into a fresh two-pool destination, then all FIVE ledger properties
/// asserted by the deterministic checkers — never by this test's own eye, and
/// never over content.
///
/// The driver decides nothing here. It produced a manifest, a statement and a
/// landing record; every judgement below is a `checkers::` verdict.
// RED: each of the five checkers is forced red by its own `red_*` test below.
// The composite here was additionally forced red once by pointing the landing
// statement's RETURN at the wrong binding — `Landing::Committed.landed` then
// carried the wrong minted identifier and the provenance checker stayed green
// (it reads the store, not the driver's claim), which is exactly why the
// `landed` map is asserted against the census separately.
#[test]
fn the_rehearsal_lands_and_all_five_ledger_properties_hold() {
    let (source_dir, source) = corpus("rehearsal");
    let before = checkers::SourceDigest::of_dir(&source_dir).expect("digest the source");
    let c = cauldron("rehearsal");

    let run = migrate::run(&c, &plan(), source)
        .expect("the driver produces material")
        .expect_ran()
        .expect("the destination was empty");

    // ---- what the driver produced ----
    let (tx, landed) = match &run.landing {
        Landing::Committed {
            tx,
            landed,
            deduplicated,
            ..
        } => {
            assert!(!*deduplicated, "a first import is not a replay");
            (*tx, landed.clone())
        }
        other => panic!("the accepted set should have landed; got {other:?}"),
    };
    assert_eq!(
        landed.len(),
        LANDED_NODES,
        "one minted identifier per landed node"
    );
    assert_eq!(
        run.manifest.attempt.as_ref().map(|a| a.tx.clone()),
        Some(tx.to_hex()),
        "the manifest records the ONE attempt identifier the set landed under"
    );

    // ---- the material the checkers take ----
    let after = checkers::SourceDigest::of_dir(&source_dir).expect("digest the source again");
    let census = checkers::census(&c).expect("census the destination");
    let ledger = checkers::ledger_for(&c, &census).expect("read the spine's rows");
    let history = checkers::attempt_history(&c, &tx).expect("the attempt history");

    assert_eq!(census.node_count(), LANDED_NODES);
    assert_eq!(census.edge_count(), LANDED_EDGES);
    // The driver's claim about what landed agrees with the store's own record
    // — asserted, never assumed, because the driver is not a witness.
    for (source_id, qualified) in &landed {
        let pool = census
            .pools
            .get(&qualified.pool)
            .unwrap_or_else(|| panic!("`{source_id}` landed in an uncensused pool"));
        assert!(
            pool.nodes.contains_key(&qualified.id),
            "`{source_id}` claims `{qualified}` and the store does not hold it"
        );
    }

    // ---- THE FIVE LEDGER PROPERTIES ----
    green(&checkers::total_accounting(&run.manifest));
    green(&checkers::referential_closure(&census));
    green(&checkers::one_door_provenance(&census, &ledger));
    green(&checkers::atomic_or_cleanly_aborted(&history, &census));
    green(&checkers::source_untouched(&before, &after));

    // The manifest, verbatim, for the record (visible with --nocapture).
    println!(
        "MANIFEST {}",
        serde_json::to_string_pretty(&run.manifest).expect("the manifest serializes")
    );
}

/// Every item gets a disposition, and the three interesting ones say the
/// right thing: the LOSSY item names its transform, the GRAMMAR-VIOLATING
/// item carries the door's own structured witness, and each DANGLING edge
/// imports with an explicit marker.
// RED: dropping the `transforms` field from the manifest's `Imported`
// disposition — the lossy item then imported silently, which is the exact
// thing a lossy transform must never do.
#[test]
fn the_manifest_disposes_every_item_with_transform_marker_and_witness() {
    let (_dir, source) = corpus("dispositions");
    let c = cauldron("dispositions");
    let run = ran(&c, source);
    let m = &run.manifest;

    assert_eq!(m.source_count, SOURCE_ITEMS);
    assert_eq!(m.entries.len(), SOURCE_ITEMS);
    assert_eq!(m.rejected(), REJECTED_ITEMS);
    assert_eq!(m.imported(), SOURCE_ITEMS - REJECTED_ITEMS);
    assert_eq!(m.landable().len(), LANDED_NODES + LANDED_EDGES);

    // The lossy item: imported, with the transform NAMED.
    let lossy = entry(m, "src-doc-2");
    match &lossy.disposition {
        Disposition::Imported {
            shape, transforms, ..
        } => {
            assert_eq!(
                shape,
                &ImportShape::Node {
                    node_type: "doc".into()
                }
            );
            assert_eq!(transforms.len(), 1, "the transform is named, not silent");
            assert_eq!(transforms[0].name, "drop-undeclared-field");
            assert_eq!(transforms[0].detail, "summary");
            assert!(transforms[0].lossy);
        }
        other => panic!("expected an import, got {other:?}"),
    }

    // The grammar-violating item: rejected, with the door's OWN witness —
    // structured, so the reason is asserted on structurally.
    for (source_id, pool) in [("src-doc-bad", ALPHA), ("src-ticket-bad", BETA)] {
        match &entry(m, source_id).disposition {
            Disposition::Rejected { reason } => {
                assert_eq!(reason.code, migrate::REJECT_GRAMMAR);
                assert_eq!(reason.witnesses.len(), 1, "one refusal, one witness");
                match &reason.witnesses[0] {
                    Witness::GrammarViolation { pool: p, .. } => {
                        assert_eq!(p, &PoolName::new(pool), "the witness names the pool");
                    }
                    other => panic!("expected a grammar violation, got {other:?}"),
                }
            }
            other => panic!("`{source_id}` should be rejected; got {other:?}"),
        }
    }

    // The two dangling edges: IMPORTED (never dropped),
    // each with an explicit marker naming the endpoint that did not resolve.
    let dangling = entry(m, "src-edge-dangling");
    match &dangling.disposition {
        Disposition::Imported { resolution, .. } => assert_eq!(
            resolution,
            &Resolution::UnresolvedEndpoint {
                endpoint: migrate::EndpointSide::Dst,
                source_ref: "src-doc-missing".into(),
                marker: migrate::MARKER_ABSENT_FROM_SOURCE.into(),
            }
        ),
        other => panic!("a dangling edge imports with a marker; got {other:?}"),
    }
    match &entry(m, "src-edge-to-rejected").disposition {
        Disposition::Imported { resolution, .. } => assert_eq!(
            resolution,
            &Resolution::UnresolvedEndpoint {
                endpoint: migrate::EndpointSide::Dst,
                source_ref: "src-doc-bad".into(),
                marker: migrate::MARKER_ENDPOINT_REJECTED.into(),
            }
        ),
        other => panic!("an edge to a rejected node imports with a marker; got {other:?}"),
    }

    // And nothing landed for either of them: the destination holds exactly the
    // four resolved edges. (The collision this exhibits — the dangling edge
    // "imports", and the one door cannot land an edge with an absent
    // endpoint — is recorded, not absorbed.)
    let census = checkers::census(&c).expect("census");
    assert_eq!(census.edge_count(), LANDED_EDGES);
    assert_eq!(m.entries.iter().filter(|e| !e.lands()).count(), REJECTED_ITEMS + UNRESOLVED_ITEMS);
}

/// Both arms: a re-run against a POPULATED
/// destination REFUSES — naming the pools and their counts, so a caller can
/// act on it — and lifts only when the plan says so explicitly.
///
/// The refusal happens BEFORE any dry run: nothing is submitted, not even in
/// validate mode, which is why the counters below cannot have moved.
// RED: deleting the `if !plan.allow_populated_destination` gate from
// `migrate::run` — the second run then imported the whole corpus a second
// time into the populated destination, silently doubling it.
#[test]
fn a_re_run_against_a_populated_destination_refuses_unless_told_otherwise() {
    let (_dir, source) = corpus("rerun");
    let c = cauldron("rerun");
    let first = ran(&c, corpus_items("rerun-first"));
    assert!(matches!(first.landing, Landing::Committed { .. }));
    let after_first = c.committed_commit_counter().expect("counter");

    // The re-run refuses, and names what it found.
    match migrate::run(&c, &plan(), source).expect("a refusal is material, not an error") {
        RunOutcome::RefusedPopulatedDestination { populated } => {
            assert_eq!(populated.len(), 2, "both destination pools are populated");
            assert_eq!(populated[0].pool, PoolName::new(ALPHA));
            assert_eq!(populated[1].pool, PoolName::new(BETA));
            assert_eq!(
                populated.iter().map(|p| p.nodes).sum::<usize>(),
                LANDED_NODES
            );
        }
        other => panic!("a populated destination refuses; got {other:?}"),
    }
    assert_eq!(
        c.committed_commit_counter().expect("counter"),
        after_first,
        "the refusal happened before anything was submitted"
    );

    // "unless explicitly told otherwise" — the one lever that lifts it.
    let mut told = plan();
    told.allow_populated_destination = true;
    told.idempotency_nonce = Some("second-deliberate-run".into());
    let second = migrate::run(&c, &told, corpus_items("rerun-second"))
        .expect("driver")
        .expect_ran()
        .expect("told otherwise");
    assert!(matches!(second.landing, Landing::Committed { .. }));
    assert_eq!(
        c.committed_commit_counter().expect("counter"),
        after_first + 1
    );
}

/// The migration run's provenance is initiator AGENT and instrument the
/// IMPORT TOOL at its version — harness-supplied, never self-reported, and
/// on the ROW, which is the only place an audit can find it later.
// RED: defaulting `MigrationPlan::instrument` to `Instrument::door()` — the
// row then claimed the ordinary write door and migration freight became
// unselectable in the ledger, which is the one thing the second
// instrument name exists for.
#[test]
fn the_landed_row_carries_the_import_tool_and_the_agent_initiator() {
    let (_dir, source) = corpus("provenance");
    let c = cauldron("provenance");
    let run = ran(&c, source);
    let tx = run.landing.attempt().expect("it landed").0.to_owned();

    let history = checkers::attempt_history(&c, &tx).expect("history");
    assert_eq!(history.rows.len(), 1, "one ask, one attempt, no restart");
    let row = &history.rows[0];
    assert_eq!(row.instrument.name, migrate::IMPORT_TOOL);
    assert!(!row.instrument.version.trim().is_empty(), "at its version");
    match &row.initiator {
        Initiator::Agent { thread, dispatch } => {
            assert_eq!(thread, "migration-rehearsal");
            assert_eq!(dispatch, "rehearsal");
        }
        other => panic!("the migration's initiator is an agent; got {other:?}"),
    }
    assert!(row.decided && row.cleared, "the attempt reached the commit point");
}

/// A corpus whose every item is refused lands NOTHING and submits nothing —
/// no empty envelope, no empty spine row. A commit of nothing is not a
/// commit.
#[test]
fn a_wholly_rejected_corpus_lands_nothing_and_writes_no_row() {
    let c = cauldron("all-rejected");
    let before = c.committed_commit_counter().expect("counter");
    let run = ran(&c, vec![bad_doc(), bad_ticket()]);
    assert_eq!(run.landing, Landing::NothingAccepted);
    assert_eq!(run.manifest.rejected(), 2);
    assert_eq!(run.manifest.imported(), 0);
    assert_eq!(c.committed_commit_counter().expect("counter"), before);
    assert!(run.manifest.attempt.is_none());
    green(&checkers::total_accounting(&run.manifest));
    green(&checkers::census(&c).map(|x| checkers::referential_closure(&x)).expect("census"));
}

/// PER-ITEM VALIDATION DOES NOT COMPOSE TO SET-LEVEL VALIDITY, and the driver
/// reports that rather than hiding it. Two `current_of` edges are each legal
/// alone and illegal together (a unique slice: at most one
/// `current_of` per author), so both pass the dry run and the LANDING is
/// refused whole. Nothing lands anywhere, the manifest still
/// accounts for every item, and the checkers still hold — which is the
/// honest shape of this seam and the reason `Landing::Refused` exists.
#[test]
fn a_set_legal_item_by_item_can_still_refuse_whole_at_the_landing() {
    let c = cauldron("set-level");
    let before = c.committed_commit_counter().expect("counter");
    let items = vec![
        node(
            "a",
            ALPHA,
            "author",
            None,
            &[("name", serde_json::json!("ada"))],
        ),
        node(
            "d1",
            ALPHA,
            "doc",
            Some("draft"),
            &[("title", serde_json::json!("one"))],
        ),
        node(
            "d2",
            ALPHA,
            "doc",
            Some("draft"),
            &[("title", serde_json::json!("two"))],
        ),
        edge("e1", ALPHA, "current_of", "d1", "a"),
        edge("e2", ALPHA, "current_of", "d2", "a"),
    ];
    let run = ran(&c, items);

    // Every item passed its own dry run — the manifest says so.
    assert_eq!(run.manifest.imported(), 5);
    assert_eq!(run.manifest.rejected(), 0);
    green(&checkers::total_accounting(&run.manifest));

    // The scalar slice bites at the LANDING, and refusal is WHOLE: the
    // witness names alpha's part and the slice it violated, and the
    // destination keeps NOTHING — not even the three perfectly legal nodes.
    let census = checkers::census(&c).expect("census");
    match &run.landing {
        Landing::Refused { witnesses } => {
            assert_eq!(witnesses.len(), 1, "one refusal, one witness");
            match &witnesses[0] {
                Witness::GrammarViolation { pool, message, .. } => {
                    assert_eq!(pool, &PoolName::new(ALPHA));
                    assert!(message.contains("current_of"), "{message}");
                }
                other => panic!("expected a grammar violation, got {other:?}"),
            }
        }
        other => panic!("the set-level violation must refuse whole; got {other:?}"),
    }
    assert_eq!(census.node_count(), 0, "nothing landed anywhere");
    assert_eq!(census.edge_count(), 0);
    assert_eq!(c.committed_commit_counter().expect("counter"), before);
    assert!(
        run.manifest.attempt.is_none(),
        "no attempt identifier: nothing landed under one"
    );
    green(&checkers::referential_closure(&census));
}

// ---------------------------------------------------------------------------
// THE RED SWEEP — one poisoned fixture per checker
// ---------------------------------------------------------------------------

/// RED 1 — **total accounting**. POISON: drop one entry from the manifest,
/// exactly as a driver that silently swallowed an item would leave it. The
/// checker fires on both halves: the entry count and the partition.
#[test]
fn red_total_accounting_when_an_entry_is_dropped_from_the_manifest() {
    let (_dir, source) = corpus("red-accounting");
    let c = cauldron("red-accounting");
    let run = ran(&c, source);
    green(&checkers::total_accounting(&run.manifest));

    let mut poisoned: Manifest = run.manifest.clone();
    let dropped = poisoned.entries.remove(3);

    let v = checkers::total_accounting(&poisoned);
    assert!(!v.passed(), "a swallowed item is a silent drop");
    assert!(v.findings.contains(&Finding::EntryCountMismatch {
        source_count: SOURCE_ITEMS,
        entries: SOURCE_ITEMS - 1,
    }));
    assert!(v
        .findings
        .iter()
        .any(|f| matches!(f, Finding::PartitionMismatch { .. })));

    // And the second poison the same checker owns: one identity, two entries.
    let mut doubled: Manifest = run.manifest.clone();
    doubled.entries.push(dropped);
    doubled.source_count += 1;
    let v = checkers::total_accounting(&doubled);
    assert!(v.findings.iter().any(|f| matches!(
        f,
        Finding::DuplicateSourceId { source_id } if source_id == &doubled.entries[3].source_id
    )));
}

/// RED 2 — **referential closure**. POISON: real damage. Delete the landed
/// `doc` that the cross-pool `catalogues` edge points at, straight out of
/// alpha's database file, and re-census. The edge lives in BETA and its far
/// endpoint is in ALPHA, so this also proves the checker looks ACROSS the
/// census rather than closing each pool against itself.
#[test]
fn red_referential_closure_when_a_cross_pool_endpoint_is_deleted() {
    let (_dir, source) = corpus("red-closure");
    let c = cauldron("red-closure");
    let run = ran(&c, source);
    green(&checkers::referential_closure(
        &checkers::census(&c).expect("census"),
    ));

    let target = landed_id(&run, "src-doc-1");
    execute_beside_the_door(
        &pool_file("red-closure", ALPHA),
        "DELETE FROM nodes WHERE id = ?1",
        rusqlite::params![target.0],
    );

    let census = checkers::census(&c).expect("census");
    let v = checkers::referential_closure(&census);
    assert!(!v.passed(), "the destination is no longer closed");
    assert!(
        v.findings.iter().any(|f| matches!(
            f,
            Finding::DanglingDestinationEdge { pool, kind, dst, .. }
                if pool == &PoolName::new(BETA)
                    && kind == "catalogues"
                    && dst.pool == PoolName::new(ALPHA)
                    && dst.id == target
        )),
        "{:?}",
        v.findings
    );
}

/// RED 3 — **one-door provenance**. POISON: real damage, and it is the exact
/// defect the design names — a second import path is a second write door,
/// a defect on day one. Two nodes are inserted straight into alpha's database
/// beside the door: one naming a transaction the spine never saw, and one
/// IMPOSTOR borrowing the real migration's hash. The checker must catch both,
/// because a hash that exists is not the same as a row that created you.
#[test]
fn red_one_door_provenance_when_nodes_arrive_beside_the_door() {
    let (_dir, source) = corpus("red-provenance");
    let c = cauldron("red-provenance");
    let run = ran(&c, source);
    let real = run.landing.attempt().expect("landed").0.to_hex();
    let census = checkers::census(&c).expect("census");
    green(&checkers::one_door_provenance(
        &census,
        &checkers::ledger_for(&c, &census).expect("ledger"),
    ));

    let alpha = pool_file("red-provenance", ALPHA);
    let phantom = "0".repeat(64);
    for (id, tx) in [("n-second-door", phantom.as_str()), ("n-impostor", real.as_str())] {
        execute_beside_the_door(
            &alpha,
            "INSERT INTO nodes (id, node_type, state, rev, schema_version, created_tx, updated_tx, created, updated)
             VALUES (?1, 'doc', 'draft', 1, 1, ?2, ?2, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            rusqlite::params![id, tx],
        );
    }

    let census = checkers::census(&c).expect("census");
    let ledger = checkers::ledger_for(&c, &census).expect("ledger");
    let v = checkers::one_door_provenance(&census, &ledger);
    assert!(!v.passed(), "a node beside the door has no provenance");
    assert!(
        v.findings.contains(&Finding::NodeWithoutSpineRow {
            pool: PoolName::new(ALPHA),
            node: NodeId("n-second-door".into()),
            created_tx: phantom.clone(),
        }),
        "{:?}",
        v.findings
    );
    assert!(
        v.findings.contains(&Finding::NodeNotInRowPayload {
            pool: PoolName::new(ALPHA),
            node: NodeId("n-impostor".into()),
            created_tx: real.clone(),
        }),
        "a real hash is not provenance; the row must NAME the node: {:?}",
        v.findings
    );
}

/// RED 4 — **atomic or cleanly aborted**. POISON: real damage. Delete ONE of
/// the attempt's landed nodes — the orphan `doc` with no edges, so nothing
/// else can fire — leaving the destination neither all-of-attempt nor
/// none-of-attempt. This is the half-landed shape the whole commit protocol
/// exists to make impossible; the checker's job is to say so when it happens
/// anyway.
#[test]
fn red_atomic_or_cleanly_aborted_when_half_the_attempt_is_missing() {
    let (_dir, source) = corpus("red-atomic");
    let c = cauldron("red-atomic");
    let run = ran(&c, source);
    let tx = run.landing.attempt().expect("landed").0.to_owned();
    let history = checkers::attempt_history(&c, &tx).expect("history");
    green(&checkers::atomic_or_cleanly_aborted(
        &history,
        &checkers::census(&c).expect("census"),
    ));

    let orphan = landed_id(&run, "src-doc-3");
    execute_beside_the_door(
        &pool_file("red-atomic", ALPHA),
        "DELETE FROM nodes WHERE id = ?1",
        rusqlite::params![orphan.0],
    );

    let census = checkers::census(&c).expect("census");
    let v = checkers::atomic_or_cleanly_aborted(&history, &census);
    assert!(!v.passed(), "half of a decided attempt is not atomic");
    assert!(
        v.findings.iter().any(|f| matches!(
            f,
            Finding::DecidedAttemptMissingEffect { tx: t, effect }
                if t == &tx.to_hex() && effect.contains(&orphan.0)
        )),
        "{:?}",
        v.findings
    );
    // The other pool is untouched and the closure checker stays green, which
    // is what makes this poison a single-checker poison.
    green(&checkers::referential_closure(&census));

    // The OTHER arm of the same checker: an identity the spine never saw.
    let unknown = TxHash([7u8; 32]);
    let empty = checkers::attempt_history(&c, &unknown).expect("history");
    let v = checkers::atomic_or_cleanly_aborted(&empty, &census);
    assert_eq!(
        v.findings,
        vec![Finding::NoSuchAttempt {
            tx: unknown.to_hex()
        }]
    );

    // The checker's other three branches, each poisoned in the HISTORY rather
    // than the store, because each is a state the door's own machinery makes
    // unreachable from outside and a branch nobody can fire is a branch
    // nobody has checked.
    let clean = checkers::census(&c).expect("census");
    let clean = {
        // Put the orphan back so this half starts from a whole destination.
        let mut c2 = clean;
        let alpha = c2.pools.get_mut(&PoolName::new(ALPHA)).expect("alpha");
        alpha.nodes.insert(
            orphan.clone(),
            checkers::CensusNode {
                node_type: "doc".into(),
                state: Some("draft".into()),
                created_tx: tx.to_hex(),
                updated_tx: tx.to_hex(),
            },
        );
        c2
    };
    green(&checkers::atomic_or_cleanly_aborted(&history, &clean));

    // (a) the attempt never reached the commit point, and yet its effects are
    //     in the store: an abort that left residue.
    let mut undecided = history.clone();
    undecided.rows[0].decided = false;
    let v = checkers::atomic_or_cleanly_aborted(&undecided, &clean);
    assert!(
        v.findings
            .iter()
            .any(|f| matches!(f, Finding::AbortedAttemptLeftResidue { .. })),
        "{:?}",
        v.findings
    );
    assert_eq!(
        v.findings.len(),
        LANDED_NODES + LANDED_EDGES,
        "every effect of the aborted attempt is named, not just the first"
    );

    // (b) a decided row with a part that never landed — the half-landed shape
    //     the row itself records.
    let mut torn = history.clone();
    torn.rows[0].parts[1].landed = false;
    let v = checkers::atomic_or_cleanly_aborted(&torn, &clean);
    assert!(
        v.findings
            .iter()
            .any(|f| matches!(f, Finding::DecidedRowHasUnlandedPart { .. })),
        "{:?}",
        v.findings
    );

    // (c) two decided rows at one identity — exactly-once broken.
    let mut twice = history.clone();
    let dup = twice.rows[0].clone();
    twice.rows.push(dup);
    let v = checkers::atomic_or_cleanly_aborted(&twice, &clean);
    assert!(
        v.findings
            .contains(&Finding::MultipleDecidedAttempts {
                tx: tx.to_hex(),
                decided: 2
            }),
        "{:?}",
        v.findings
    );
}

/// RED 5 — **source untouched**. POISON: real damage, one byte. The
/// migration is ONE-WAY and the old files are NEVER written (SETTLED:
/// byte-identical before and after). Nothing in the driver can write
/// a source file — it never sees one — so the byte is appended by this test,
/// which is the only way this checker can be made to fire at all and is
/// itself the finding worth recording.
#[test]
fn red_source_untouched_when_one_source_byte_changes() {
    let (source_dir, source) = corpus("red-source");
    let before = checkers::SourceDigest::of_dir(&source_dir).expect("digest");
    let c = cauldron("red-source");
    let _ = ran(&c, source);
    let after = checkers::SourceDigest::of_dir(&source_dir).expect("digest");
    green(&checkers::source_untouched(&before, &after));

    // One byte, appended to one file.
    let victim = source_dir.join("edges.json");
    let mut bytes = std::fs::read(&victim).expect("read the source file");
    bytes.push(b'\n');
    std::fs::write(&victim, &bytes).expect("write the source file");

    let poisoned = checkers::SourceDigest::of_dir(&source_dir).expect("digest");
    let v = checkers::source_untouched(&before, &poisoned);
    assert!(!v.passed(), "the source was written");
    assert!(
        v.findings
            .iter()
            .any(|f| matches!(f, Finding::SourceEntryChanged { entry, .. } if entry == "edges.json")),
        "{:?}",
        v.findings
    );

    // And the two neighbouring shapes, so the checker is not merely a
    // hash-equality with one arm: a removed entry and an added one.
    std::fs::remove_file(&victim).expect("remove");
    let removed = checkers::SourceDigest::of_dir(&source_dir).expect("digest");
    assert!(checkers::source_untouched(&before, &removed)
        .findings
        .contains(&Finding::SourceEntryRemoved {
            entry: "edges.json".into()
        }));
    std::fs::write(source_dir.join("intruder.json"), b"[]").expect("write");
    let added = checkers::SourceDigest::of_dir(&source_dir).expect("digest");
    assert!(checkers::source_untouched(&before, &added)
        .findings
        .contains(&Finding::SourceEntryAdded {
            entry: "intruder.json".into()
        }));
}

// ---------------------------------------------------------------------------
// helpers — the destination, the plan, and the fixture corpus
// ---------------------------------------------------------------------------

fn green(v: &Verdict) {
    assert!(
        v.passed(),
        "checker `{}` fired: {:?}",
        v.checker,
        v.findings
    );
}

fn scratch(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-migrate-{name}-{}", std::process::id()))
}

fn cauldron(name: &str) -> Cauldron {
    let dir = scratch(name);
    let _ = std::fs::remove_dir_all(&dir);
    Cauldron::create(
        &dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        SpineConfig::default(),
    )
    .expect("create the destination cauldron")
}

fn pool_file(name: &str, pool: &str) -> PathBuf {
    scratch(name).join(format!("{pool}.db"))
}

/// Provenance is HARNESS-SUPPLIED: the test plays the harness
/// and hands the driver an initiator; the driver never invents one.
fn plan() -> MigrationPlan {
    MigrationPlan::new(Initiator::Agent {
        thread: "migration-rehearsal".into(),
        dispatch: "rehearsal".into(),
    })
}

fn ran(c: &Cauldron, source: Vec<SourceItem>) -> MigrationRun {
    migrate::run(c, &plan(), source)
        .expect("the driver produces material")
        .expect_ran()
        .expect("the destination was empty")
}

fn entry<'a>(m: &'a Manifest, source_id: &str) -> &'a spine::migrate::ManifestEntry {
    m.entry(source_id)
        .unwrap_or_else(|| panic!("no manifest entry for `{source_id}`"))
}

fn landed_id(run: &MigrationRun, source_id: &str) -> NodeId {
    match &run.landing {
        Landing::Committed { landed, .. } => landed
            .get(source_id)
            .unwrap_or_else(|| panic!("`{source_id}` did not land"))
            .id
            .clone(),
        other => panic!("nothing landed: {other:?}"),
    }
}

/// A SECOND WRITE DOOR, opened deliberately and only to poison a fixture
/// (the day-one defect; the checkers exist to catch
/// it, and a checker that has never caught one does not count as existing).
fn execute_beside_the_door(db: &Path, sql: &str, params: &[&dyn rusqlite::ToSql]) {
    let conn = rusqlite::Connection::open(db).expect("open the pool file");
    conn.execute(sql, params).expect("poison the fixture");
}

// ---------------------------------------------------------------------------
// the fixture corpus — written to files, read back by an adapter
// ---------------------------------------------------------------------------

/// Write the corpus to a scratch directory and read it back through the
/// adapter, returning both the directory (for the byte digests) and the
/// items. The round trip through the file system is deliberate: `source
/// untouched` is a claim about FILES, and a corpus built in memory could not
/// be used to make it.
fn corpus(name: &str) -> (PathBuf, Vec<SourceItem>) {
    let dir = scratch(&format!("source-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the source dir");
    write_json(&dir.join("nodes-alpha.json"), &alpha_nodes());
    write_json(&dir.join("nodes-beta.json"), &beta_nodes());
    write_json(&dir.join("edges.json"), &edges());
    let items = read_corpus(&dir);
    assert_eq!(items.len(), SOURCE_ITEMS, "the corpus is what it claims");
    (dir, items)
}

fn corpus_items(name: &str) -> Vec<SourceItem> {
    corpus(name).1
}

fn write_json(path: &Path, items: &[SourceItem]) {
    std::fs::write(
        path,
        serde_json::to_vec_pretty(items).expect("the corpus serializes"),
    )
    .expect("write the corpus file");
}

/// THE ADAPTER — the half that knows the source format. This one regenerates
/// items from files as they stand; a replay adapter would build the same
/// items from a message stream, and `migrate::run` cannot tell which it got.
/// It is READ-ONLY over the source by construction: `std::fs::read`, nothing
/// else.
fn read_corpus(dir: &Path) -> Vec<SourceItem> {
    let mut out = Vec::new();
    for file in ["nodes-alpha.json", "nodes-beta.json", "edges.json"] {
        let bytes = std::fs::read(dir.join(file)).expect("read the corpus file");
        let items: Vec<SourceItem> = serde_json::from_slice(&bytes).expect("parse the corpus file");
        out.extend(items);
    }
    out
}

fn node(
    source_id: &str,
    pool: &str,
    node_type: &str,
    state: Option<&str>,
    fields: &[(&str, serde_json::Value)],
) -> SourceItem {
    SourceItem::Node(SourceNode {
        source_id: source_id.into(),
        pool: PoolName::new(pool),
        node_type: node_type.into(),
        state: state.map(str::to_string),
        carried_id: None,
        fields: fields
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect::<BTreeMap<_, _>>(),
        transforms: Vec::new(),
    })
}

fn edge(source_id: &str, pool: &str, kind: &str, src: &str, dst: &str) -> SourceItem {
    SourceItem::Edge(SourceEdge {
        source_id: source_id.into(),
        pool: PoolName::new(pool),
        kind: kind.into(),
        src: src.into(),
        dst: dst.into(),
        note: None,
        fields: BTreeMap::new(),
        transforms: Vec::new(),
    })
}

/// `doc` with a state outside alpha's closed vocabulary — the
/// grammar-violating item.
fn bad_doc() -> SourceItem {
    node(
        "src-doc-bad",
        ALPHA,
        "doc",
        Some("archived"),
        &[("title", serde_json::json!("outside the vocabulary"))],
    )
}

/// `ticket` carrying a field beta declares nowhere — a LOSS nobody named,
/// which must be a rejection rather than a silent drop.
fn bad_ticket() -> SourceItem {
    node(
        "src-ticket-bad",
        BETA,
        "ticket",
        Some("open"),
        &[("label", serde_json::json!("beta declares no such field"))],
    )
}

fn alpha_nodes() -> Vec<SourceItem> {
    let mut lossy = node(
        "src-doc-2",
        ALPHA,
        "doc",
        Some("published"),
        &[("title", serde_json::json!("the second"))],
    );
    // The LOSSY item: the adapter dropped a field the destination cannot hold
    // and NAMED what it did (never silent).
    if let SourceItem::Node(n) = &mut lossy {
        n.transforms.push(migrate::NamedTransform {
            name: "drop-undeclared-field".into(),
            detail: "summary".into(),
            lossy: true,
        });
    }
    vec![
        node(
            "src-author",
            ALPHA,
            "author",
            None,
            &[("name", serde_json::json!("ada"))],
        ),
        node(
            "src-doc-1",
            ALPHA,
            "doc",
            Some("draft"),
            &[("title", serde_json::json!("the first"))],
        ),
        lossy,
        // An orphan with no edges — the single-checker poison target for the
        // atomicity red.
        node(
            "src-doc-3",
            ALPHA,
            "doc",
            Some("draft"),
            &[("title", serde_json::json!("the orphan"))],
        ),
        bad_doc(),
    ]
}

fn beta_nodes() -> Vec<SourceItem> {
    vec![
        node(
            "src-shelf",
            BETA,
            "shelf",
            None,
            &[
                ("label", serde_json::json!("S1")),
                ("capacity", serde_json::json!(10)),
            ],
        ),
        node("src-ticket", BETA, "ticket", Some("open"), &[]),
        bad_ticket(),
    ]
}

fn edges() -> Vec<SourceItem> {
    vec![
        edge("src-edge-wrote-1", ALPHA, "wrote", "src-author", "src-doc-1"),
        edge("src-edge-wrote-2", ALPHA, "wrote", "src-author", "src-doc-2"),
        edge("src-edge-holds", BETA, "holds", "src-shelf", "src-ticket"),
        // THE CROSS-POOL EDGE (`catalogues`): stored in beta, far
        // endpoint in alpha — one import, two pools, two grammars, two pinned
        // versions, one commit.
        edge(
            "src-edge-catalogues",
            BETA,
            "catalogues",
            "src-ticket",
            "src-doc-1",
        ),
        // A DANGLING source edge: the far endpoint is in no source item at all.
        edge(
            "src-edge-dangling",
            ALPHA,
            "wrote",
            "src-author",
            "src-doc-missing",
        ),
        // A dangling-by-consequence edge: the far endpoint IS in the source
        // and was rejected, so it will never exist in the destination.
        edge(
            "src-edge-to-rejected",
            ALPHA,
            "supersedes",
            "src-doc-1",
            "src-doc-bad",
        ),
    ]
}
