//! **`dump@1`**: the grammar of a cauldron's dumps store, the second spine
//! that sits beside its soup (`<cauldron>/dumps/`).
//!
//! A dump is raw text put into a cauldron for later processing: a note, a
//! paste, or a Claude Code session's transcript. It is not knowledge yet, so
//! it is not a node of the soup's workspace grammar; it waits here, in state
//! `received`, until a later release processes it.
//!
//! The one node type, `dump`, is grammar v7's `dump` COPIED: prefix `DM`,
//! states `received | processed | failed`, and the same fields (an uncapped
//! `body`, `env`, `from_cluster`, `seq`, `notes`, `artifacts`), plus one
//! optional `meta` object for what this release records about where a dump
//! came from. v7 itself is not pinned here: the copy is checked against v7's
//! definition, kept as a fixture beside this file
//! (`grammar_dump_v7_dump.json`, the JSON v7's own code renders for its
//! `dump`), so the two cannot drift apart unseen. Keeping the prefix and the
//! states the same means a later release can carry these dumps into a v7
//! workspace with their ids.
//!
//! The grammar declares no edge kinds: a dump stands alone, and a soup record
//! that came from one cites it (`meta.cites: [{kind: "dump", id: "DM-3"}]`),
//! since an edge cannot cross from one spine to another.

use crate::schema_def::{FieldDef, FieldKind, NodeTypeDef, PoolGrammar, PoolKind, PoolName};
use std::collections::BTreeMap;

/// The pool of the dumps store, and the only one.
pub const POOL: &str = "dumps";
/// The pool's kind.
pub const KIND: &str = "dumps";
/// The pinned version: `dump@1`.
pub const VERSION: u32 = 1;
/// The node type (v7's name).
pub const DUMP: &str = "dump";
/// The id prefix (v7's).
pub const DUMP_PREFIX: &str = "DM";
/// v7's states: `received`, then `processed` or `failed` once processed.
pub const DUMP_STATES: [&str; 3] = ["received", "processed", "failed"];
/// The state every dump lands in, which later processing picks up.
pub const RECEIVED: &str = "received";

/// v7's `dump` node type, as v7 defines it (no `meta`).
pub fn dump_v7() -> NodeTypeDef {
    let opt = |kind: FieldKind| FieldDef { kind, ..FieldDef::optional() };
    let text = |required: bool, floor: Option<u64>, cap: Option<u64>| FieldDef {
        required,
        kind: FieldKind::Text,
        max_chars: cap,
        min_chars: floor,
    };
    NodeTypeDef {
        fields: BTreeMap::from([
            ("body".to_string(), text(true, Some(1), None)),
            ("env".to_string(), text(true, Some(1), None)),
            ("from_cluster".to_string(), text(false, None, None)),
            ("seq".to_string(), opt(FieldKind::Integer)),
            ("notes".to_string(), opt(FieldKind::Any)),
            ("artifacts".to_string(), opt(FieldKind::Object)),
        ]),
        states: DUMP_STATES.iter().map(|s| s.to_string()).collect(),
        count_cells: BTreeMap::new(),
        id_prefix: Some(DUMP_PREFIX.to_string()),
        required_edges_at_mint: BTreeMap::new(),
        required_at_mint_any_of: vec![],
    }
}

/// The `dump@1` grammar for a pool named `pool`: v7's `dump` plus an
/// optional `meta` object.
pub fn dumps_v1(pool: &str) -> PoolGrammar {
    let mut dump = dump_v7();
    dump.fields.insert("meta".to_string(), FieldDef { kind: FieldKind::Object, ..FieldDef::optional() });
    PoolGrammar {
        pool: PoolName::new(pool),
        kind: PoolKind::new(KIND),
        version: VERSION,
        node_types: BTreeMap::from([(DUMP.to_string(), dump)]),
        edge_kinds: BTreeMap::new(),
    }
}

/// v7's `dump`, as v7's own code renders it to JSON.
pub const V7_DUMP_JSON: &str = include_str!("grammar_dump_v7_dump.json");

#[cfg(test)]
mod tests {
    use super::*;

    /// The copy is v7's `dump`, field for field, and `dump@1` adds only
    /// `meta`. If either side changes, this fails.
    #[test]
    fn the_copy_is_v7_s_dump_and_dump_v1_adds_only_meta() {
        let v7: NodeTypeDef = serde_json::from_str(V7_DUMP_JSON).expect("the fixture parses");
        assert_eq!(dump_v7(), v7, "the copied dump drifted from grammar v7's");
        let g = dumps_v1(POOL);
        g.check_wellformed().expect("dump@1 is well formed");
        let mine = &g.node_types[DUMP];
        let mut without_meta = mine.clone();
        assert!(without_meta.fields.remove("meta").is_some());
        assert_eq!(without_meta, v7);
        assert_eq!(mine.fields["meta"], FieldDef { kind: FieldKind::Object, ..FieldDef::optional() });
        assert_eq!(g.node_types.len(), 1);
        assert!(g.edge_kinds.is_empty());
        assert_eq!((g.version, g.kind.as_str()), (1, KIND));
    }
}
