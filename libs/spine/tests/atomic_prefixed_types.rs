//! `checkers::atomic_or_cleanly_aborted` can FAIL for a type with
//! an `id_prefix` (serial ids).
//!
//! The problem: an UNDECIDED row's payload carries the
//! PROVISIONAL `<prefix>-~<hex>` id (only the decided flip numbers it), so
//! the none-of-attempt arm, which looked for the payload's identifiers in the
//! destination, could never see residue of a prefixed type — residue would
//! carry a serial `<prefix>-<n>`. The checker now also finds residue by
//! PROVENANCE: any node or edge the census stamps with the identity.
//!
//! The fault is INJECTED as real damage to a real pool file: an attempt the
//! door genuinely refused after its row was appended (recorded aborted) is
//! given a landed node under a serial, stamped with that attempt's hash —
//! the shape a landing that escaped its abort would leave.

use spine::checkers::{self, Finding};
use spine::config::SpineConfig;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::pool::Pool;
use spine::schema_def::{
    grammar_fault_tag, EdgeKindDef, FieldDef, NodeTypeDef, PoolGrammar, PoolKind, PoolName,
};
use spine::{Cauldron, Outcome};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

test_scratch::install!();

const POOL: &str = "fixture-atomic";

/// `task` mints `T-<n>`; `owner` keeps the hash rule; `owns` is at most one
/// per owner, so a second `owns` is refused AFTER its row is appended
/// (phase 10b) and the attempt is recorded aborted.
fn grammar() -> PoolGrammar {
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
    PoolGrammar {
        pool: PoolName::new(POOL),
        kind: PoolKind::new("atomic"),
        version: 1,
        node_types,
        edge_kinds,
    }
}

/// G4. A genuinely aborted attempt that minted a prefixed `task`: green on
/// the clean abort; RED once residue stamped with its identity is injected
/// under a serial id — a node, then (that undone) an edge,
/// then a SET (an existing node's `updated_tx`). Sentinels first: the
/// aborted row's payload names only the PROVISIONAL id, never the serial
/// the residue carries, so the identifier half genuinely cannot see it.
// RED: the provenance half of the none-of-attempt arm removed (the checker
// as it stood before that half was added) — the injected node residue came back GREEN, the
// checker could not fail. Opposed by the clean abort, which a constant
// "always residue" checker fails.
#[test]
fn g4_atomic_or_cleanly_aborted_fails_on_residue_of_a_prefixed_type() {
    let dir = root("g4");
    let c = fresh(&dir);
    let owner = mint(&c, r#"CREATE (o: fixture-atomic/owner { name: "o" }) RETURN o.id"#, &[], "o.id");
    let holder = mint(
        &c,
        r#"MATCH (o: fixture-atomic/owner { id: $o })
           CREATE (t: fixture-atomic/task { name: "holder" })
           CREATE (t) -[owns]-> (o)
           RETURN t.id"#,
        &[("o", &owner)],
        "t.id",
    );
    assert_eq!(holder, "T-1");

    // An attempt refused after its row is appended: aborted, never decided.
    let env = commit(
        r#"MATCH (o: fixture-atomic/owner { id: $o })
           CREATE (t: fixture-atomic/task { name: "usurper" })
           CREATE (t) -[owns]-> (o)"#,
        &[("o", &owner)],
    );
    let hash = env.hash_identity().expect("hash");
    match c.submit(env) {
        Err(SpineError::Refused { witnesses }) => {
            assert_eq!(grammar_fault_tag(&witnesses[0]), Some("at_most_one_violated"))
        }
        other => panic!("the usurper must be refused: {other:?}"),
    }
    let history = checkers::attempt_history(&c, &hash).expect("history");
    assert_eq!(history.rows.len(), 1, "sentinel: the refused attempt reached the append");
    assert!(!history.rows[0].decided && history.rows[0].cleared, "{:?}", history.rows[0]);
    let named: Vec<String> = history.rows[0]
        .parts
        .iter()
        .flat_map(|p| p.created_nodes.iter().map(|n| n.0.clone()))
        .collect();
    assert_eq!(named.len(), 1, "{named:?}");
    assert!(named[0].starts_with("T-~"), "sentinel: the payload names the PROVISIONAL id: {named:?}");

    // Opposed: the clean abort is green.
    let clean = checkers::census(&c).expect("census");
    let v = checkers::atomic_or_cleanly_aborted(&history, &clean);
    assert!(v.passed(), "a clean abort is atomic: {:?}", v.findings);

    // Injected: the aborted attempt's node landed under the serial it would
    // have been numbered.
    let residue = "T-2";
    assert!(!named.contains(&residue.to_string()), "sentinel: the payload never names `{residue}`");
    beside_the_door(
        &dir,
        "INSERT INTO nodes (id, node_type, state, rev, schema_version, created_tx, updated_tx, created, updated)
         VALUES (?1, 'task', NULL, 1, 1, ?2, ?2, '0', '0')",
        rusqlite::params![residue, hash.to_hex()],
    );
    let v = checkers::atomic_or_cleanly_aborted(&history, &checkers::census(&c).expect("census"));
    assert_eq!(
        v.findings,
        vec![Finding::AbortedAttemptLeftResidue {
            tx: hash.to_hex(),
            effect: format!("node {POOL}/{residue}"),
        }],
        "the residue node is named, once"
    );
    beside_the_door(&dir, "DELETE FROM nodes WHERE id = ?1", rusqlite::params![residue]);
    assert!(
        checkers::atomic_or_cleanly_aborted(&history, &checkers::census(&c).expect("census")).passed(),
        "sentinel: the damage was undone"
    );

    // Injected: an edge stamped with the aborted identity.
    beside_the_door(
        &dir,
        "INSERT INTO edges (src, kind, dst_pool, dst, note, fields, created_tx, created)
         VALUES (?1, 'owns', ?2, ?3, NULL, '{}', ?4, '0')",
        rusqlite::params![holder, POOL, "nResidueOwner", hash.to_hex()],
    );
    let v = checkers::atomic_or_cleanly_aborted(&history, &checkers::census(&c).expect("census"));
    assert!(
        v.findings.iter().any(|f| matches!(
            f,
            Finding::AbortedAttemptLeftResidue { effect, .. } if effect.starts_with("edge ")
        )),
        "{:?}",
        v.findings
    );
    beside_the_door(&dir, "DELETE FROM edges WHERE created_tx = ?1", rusqlite::params![hash.to_hex()]);

    // Injected: a SET's residue — an existing node last updated by it.
    beside_the_door(
        &dir,
        "UPDATE nodes SET updated_tx = ?1 WHERE id = ?2",
        rusqlite::params![hash.to_hex(), holder],
    );
    let v = checkers::atomic_or_cleanly_aborted(&history, &checkers::census(&c).expect("census"));
    assert_eq!(
        v.findings,
        vec![Finding::AbortedAttemptLeftResidue {
            tx: hash.to_hex(),
            effect: format!("node {POOL}/{holder}"),
        }]
    );
}

/// The DECIDED arm for a prefixed type, opposed: a committed mint is green
/// (its row's payload was rewritten to the serial at the flip), and red when
/// the serial it landed is removed.
// RED: opposed by construction — the same checker green on the commit and
// red on the removal; a constant checker fails one of the two.
#[test]
fn a_decided_prefixed_mint_is_green_whole_and_red_when_its_node_is_missing() {
    let dir = root("decided");
    let c = fresh(&dir);
    let env = commit(r#"CREATE (t: fixture-atomic/task { name: "a" }) RETURN t.id"#, &[]);
    let hash = env.hash_identity().expect("hash");
    let id = match c.submit(env).expect("lands") {
        Outcome::Committed { returned, .. } => returned.0[0]["t.id"].as_str().expect("id").to_string(),
        other => panic!("{other:?}"),
    };
    assert_eq!(id, "T-1");
    let history = checkers::attempt_history(&c, &hash).expect("history");
    let v = checkers::atomic_or_cleanly_aborted(&history, &checkers::census(&c).expect("census"));
    assert!(v.passed(), "{:?}", v.findings);
    beside_the_door(&dir, "DELETE FROM nodes WHERE id = ?1", rusqlite::params![id]);
    let v = checkers::atomic_or_cleanly_aborted(&history, &checkers::census(&c).expect("census"));
    assert_eq!(
        v.findings,
        vec![Finding::DecidedAttemptMissingEffect {
            tx: hash.to_hex(),
            effect: format!("node {POOL}/T-1"),
        }]
    );
}

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

fn beside_the_door(dir: &Path, sql: &str, params: impl rusqlite::Params) {
    let conn = rusqlite::Connection::open(Pool::path_in(dir, &PoolName::new(POOL))).expect("pool");
    conn.execute(sql, params).expect("damage");
}

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-atomic-prefixed-{name}-{}", std::process::id()))
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
                thread: "atomic-prefixed".into(),
                dispatch: "atomic-prefixed".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn mint(c: &Cauldron, statement: &str, params: &[(&str, &str)], col: &str) -> String {
    match c.submit(commit(statement, params)).expect("lands") {
        Outcome::Committed { returned, .. } => returned.0[0][col].as_str().expect("id").to_string(),
        other => panic!("expected Committed, got {other:?}"),
    }
}
