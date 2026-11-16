//! A PROVISIONAL id never lands.
//!
//! The door compiles a prefixed type's CREATE to the provisional
//! `<P>-~<hex>` (`ladle::compile::provisional_serial_id`); the decided flip
//! rewrites it to the serial `<P>-<n>` (`CompiledPayload::assign_ids`) before
//! any pool lands. Before this guard the landing's validator judged the id PREFIX
//! only, so a provisional that skipped the flip landed. Now a pool landing
//! (`FarSide::Landing`) refuses it with the frozen tag
//! `provisional_id_at_landing` (new in `schema_def::GRAMMAR_FAULTS`).
//!
//! The refusal is asked of the REAL landing, `Pool::apply_part` (pub under
//! `test-support`), not of the validator alone, and every refusal is
//! opposed: a serial id of the same prefix lands through the
//! same call, and a serial minted through `Cauldron::submit` lands — so a
//! landing that refused every prefixed CREATE fails here. The forcings are
//! named in `// RED:` comments.

use spine::config::SpineConfig;
use spine::delta::{CompiledPart, Delta};
use spine::envelope::{
    Bindings, Envelope, Initiator, Instrument, Mode, Provenance, TxHash,
};
use spine::error::SpineError;
use spine::pool::{NodeId, Pool};
use spine::schema_def::{
    grammar_fault_tag, FarSide, FieldDef, NodeTypeDef, PoolGrammar, PoolKind, PoolName,
};
use spine::{Cauldron, Outcome};
use std::collections::BTreeMap;
use std::path::PathBuf;

test_scratch::install!();

const POOL: &str = "fixture-provisional";

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
    PoolGrammar {
        pool: PoolName::new(POOL),
        kind: PoolKind::new("provisional"),
        version: 1,
        node_types,
        edge_kinds: BTreeMap::new(),
    }
}

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn temp(name: &str) -> Temp {
    let p = test_scratch::tmp_path(format!(
        "spine-provisional-landing-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("temp dir");
    Temp(p)
}

fn create_task(id: &str) -> CompiledPart {
    CompiledPart {
        pool: PoolName::new(POOL),
        schema_version: 1,
        deltas: vec![Delta::CreateNode {
            id: NodeId(id.to_string()),
            node_type: "task".to_string(),
            state: None,
            fields: BTreeMap::from([("name".to_string(), serde_json::json!("a"))]),
        }],
        premises: vec![],
        far_versions: BTreeMap::new(),
    }
}

fn tag_of(err: &SpineError) -> Option<&str> {
    match err {
        SpineError::Refused { witnesses } => {
            assert_eq!(witnesses.len(), 1, "one refusal, one witness: {witnesses:?}");
            grammar_fault_tag(&witnesses[0])
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// G3 at the real landing: `T-~<hex>` refused `provisional_id_at_landing`
/// and nothing lands; opposed by `T-7` — the same prefix, the same call —
/// landing whole.
// RED: (a) the landing check removed from `validate_delta` (the code
// before the check) — the provisional LANDED: `apply_part` returned `Ok` and the test
// panicked at "a provisional must not land". (b) Opposed: the check made to
// fire on every prefixed id (`provisional_prefix(id).is_some() || true`) —
// `T-7` was refused `provisional_id_at_landing` and the serial arm panicked
// (:119), and `a_serial_minted_through_the_door_lands` went red with the
// door's post-flip invariant `Hard` (:176).
#[test]
fn g3_a_provisional_id_is_refused_at_landing_and_a_serial_of_its_prefix_lands() {
    let t = temp("g3");
    let mut pool = Pool::create(&t.0, &grammar()).expect("create the pool");

    let provisional = "T-~0123456789abcdef";
    let err = match pool.apply_part(&TxHash([1; 32]), 0, &create_task(provisional)) {
        Ok(landed) => panic!("a provisional must not land: {landed:?}"),
        Err(e) => e,
    };
    assert_eq!(tag_of(&err), Some("provisional_id_at_landing"), "{err:?}");
    assert!(
        pool.snapshot().expect("snapshot").node(&NodeId(provisional.into())).expect("read").is_none(),
        "the refused part left nothing behind"
    );

    pool.apply_part(&TxHash([2; 32]), 0, &create_task("T-7"))
        .unwrap_or_else(|e| panic!("a serial id of the type's prefix must land: {e:?}"));
    assert!(pool.snapshot().expect("snapshot").node(&NodeId("T-7".into())).expect("read").is_some());
}

/// The new rule is the LANDING's: the door validates the PRE-flip payload,
/// so the same part under `FarSide::Door` is not refused for its
/// provisional id — and a wrong prefix still answers `id_prefix_mismatch`
/// at the landing (the prefix rule runs first; `caller_id_against_prefix`
/// reads that tag).
// RED: the landing check moved above the prefix rule — `X-~ab` at the
// landing answered `provisional_id_at_landing`, not `id_prefix_mismatch`.
#[test]
fn the_rule_is_the_landings_and_the_prefix_rule_still_answers_first() {
    let g = grammar();
    let nobody = |_: &NodeId| -> Option<String> { None };
    let no_grammar = |_: &PoolName| -> Option<PoolGrammar> { None };
    let no_type = |_: &spine::pool::QualifiedNodeId| -> Option<String> { None };
    let no_kind = |_: &PoolKind, _: &str| -> bool { false };
    let door = FarSide::Door {
        grammar: &no_grammar,
        node_type: &no_type,
        declared_by_kind: &no_kind,
    };
    g.validate_part(&create_task("T-~0123456789abcdef"), Some(0), &nobody, &door)
        .unwrap_or_else(|e| panic!("the door judges the pre-flip payload: {e:?}"));

    let err = g
        .validate_part(&create_task("X-~ab"), Some(0), &nobody, &FarSide::Landing)
        .unwrap_err();
    assert_eq!(tag_of(&err), Some("id_prefix_mismatch"), "{err:?}");
}

/// End to end, opposed: a task minted through `Cauldron::submit` lands under
/// its serial — the flip ran before the landing, so the new refusal never
/// meets a statement that came through the door.
#[test]
fn a_serial_minted_through_the_door_lands() {
    let t = temp("door");
    let dir = t.0.join("cauldron");
    let c = Cauldron::create(&dir, &[grammar()], SpineConfig::default()).expect("cauldron");
    let mut bindings = Bindings::new();
    bindings.insert("n".to_string(), serde_json::json!("a"));
    let out = c
        .submit(Envelope {
            statement: format!("CREATE (t: {POOL}/task {{ name: $n }}) RETURN t.id"),
            bindings,
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
            provenance: Provenance {
                initiator: Initiator::Agent {
                    thread: "provisional-landing".into(),
                    dispatch: "provisional-landing".into(),
                },
                instrument: Instrument::door(),
            },
            mode: Mode::Commit,
            idempotency_nonce: None,
        })
        .unwrap_or_else(|e| panic!("the mint must commit: {e:?}"));
    match out {
        Outcome::Committed { returned, .. } => {
            assert_eq!(returned.0[0]["t.id"], serde_json::json!("T-1"));
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}
