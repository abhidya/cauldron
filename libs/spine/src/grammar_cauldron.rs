//! **`cauldron@1`**: the grammar of a cauldron's own pool, the second pool of
//! its soup, beside the workspace pool (`ws`, `workspace@6`).
//!
//! It holds CLUSTERS. A cluster is an area of the surface: a name (its
//! `handle`) and a boundary description saying what it covers. A cluster
//! CLAIMS regions of the workspace pool: a `claims` edge from the cluster to
//! a region. Any cluster may claim any region and a region may be claimed by
//! many clusters (many-to-many). A claim is a statement about the surface,
//! not a lease on work: nothing is held, locked or assigned by it.
//!
//! # Why a second grammar and not a v6 type
//!
//! `workspace@6` is published and consumers depend on it, so it does not
//! change. A cluster sits ABOVE the workspace: it groups regions, and a
//! region is the workspace's own unit. So the cluster lives in a pool of its
//! own kind (`cauldron`) in the same store, and its claim is a cross-pool
//! edge (`CL-1@cauldron -[claims]-> R-3@ws`), which the spine already
//! supports: the edge is stored once, on its source, in this pool, and a
//! region's claimants are read from the edge table's reverse index.
//!
//! # The shape
//!
//! - one node type, `cluster`, prefix `CL`: `handle` (required, 1..=235, as
//!   every workspace handle), `boundary` (required, 40..=1,000 raw
//!   characters, the same rule as a region's boundary), an open `meta`
//!   object, and no `state` (an empty vocabulary refuses any);
//! - one edge kind, `claims`, from a `cluster` to a `region` in any pool of
//!   kind `workspace`; no declared fields, so the only attribute an edge may
//!   carry is the optional free-text `note` every edge has.
//!
//! Nothing here is required at mint: a cluster with no claim yet is a
//! cluster (it is drawn before it claims anything).
//!
//! # What is not grammar here
//!
//! Like the workspace grammar, the rules that read a second node are not
//! grammar: a claimed region being retired, or two clusters with the same
//! handle, are not refused. See [`LEFT_OUT`].

use crate::grammar_v6;
use crate::schema_def::{EdgeKindDef, FieldDef, FieldKind, NodeTypeDef, PoolGrammar, PoolKind, PoolName, TypeRef};
use std::collections::{BTreeMap, BTreeSet};

/// The pool every cauldron's clusters live in (the soup's second pool).
pub const POOL: &str = "cauldron";
/// The pool's kind.
pub const KIND: &str = "cauldron";
/// The pinned version: `cauldron@1`.
pub const VERSION: u32 = 1;
/// The node type.
pub const CLUSTER: &str = "cluster";
/// The id prefix: `CL-<n>`.
pub const CLUSTER_PREFIX: &str = "CL";
/// The one edge kind.
pub const CLAIMS: &str = "claims";

/// The rules this grammar deliberately does not express, and why.
pub const LEFT_OUT: &[(&str, &str)] = &[
    ("claimed region live", "a claim of a retired region is not refused: it reads the region's flag"),
    ("handle unique", "two clusters may share a handle: it reads every cluster"),
    ("Inside/Outside clauses", "a boundary's wording is not judged, as for a region's"),
];

/// The `cauldron@1` grammar for a pool named `pool`.
pub fn cauldron_v1(pool: &str) -> PoolGrammar {
    let text = |floor: u64, cap: u64| FieldDef {
        required: true,
        kind: FieldKind::Text,
        max_chars: Some(cap),
        min_chars: Some(floor),
    };
    let cluster = NodeTypeDef {
        fields: BTreeMap::from([
            ("handle".to_string(), text(grammar_v6::HANDLE_FLOOR, grammar_v6::HANDLE_CAP)),
            ("boundary".to_string(), text(grammar_v6::BOUNDARY_FLOOR, grammar_v6::BOUNDARY_CAP)),
            ("meta".to_string(), FieldDef { kind: FieldKind::Object, ..FieldDef::optional() }),
        ]),
        // No state: an empty vocabulary refuses any `state`.
        states: vec![],
        count_cells: BTreeMap::new(),
        id_prefix: Some(CLUSTER_PREFIX.to_string()),
        required_edges_at_mint: BTreeMap::new(),
        required_at_mint_any_of: vec![],
    };
    let claims = EdgeKindDef {
        from: BTreeSet::from([TypeRef::local(CLUSTER)]),
        to: BTreeSet::from([TypeRef::reach(grammar_v6::KIND, grammar_v6::REGION)]),
        at_most_one_per_from: false,
        at_most_one_per_to: false,
        fields: BTreeMap::new(),
    };
    PoolGrammar {
        pool: PoolName::new(pool),
        kind: PoolKind::new(KIND),
        version: VERSION,
        node_types: BTreeMap::from([(CLUSTER.to_string(), cluster)]),
        edge_kinds: BTreeMap::from([(CLAIMS.to_string(), claims)]),
    }
}

/// Is `id` a cluster id by its form (`CL-<n>`)? A form, not a lookup.
pub fn is_cluster_id(id: &str) -> bool {
    id.strip_prefix("CL-").is_some_and(|n| !n.is_empty() && !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cauldron_v1_is_well_formed_and_holds_exactly_the_cluster_and_claims() {
        let g = cauldron_v1(POOL);
        g.check_wellformed().expect("cauldron@1 is well formed");
        assert_eq!((g.kind.as_str(), g.version), (KIND, VERSION));
        assert_eq!(g.node_types.keys().collect::<Vec<_>>(), [CLUSTER]);
        assert_eq!(g.edge_kinds.keys().collect::<Vec<_>>(), [CLAIMS]);
        let c = &g.node_types[CLUSTER];
        assert!(c.states.is_empty());
        assert_eq!(c.id_prefix.as_deref(), Some(CLUSTER_PREFIX));
        // The boundary rule is the region's, number for number.
        let region = &grammar_v6::workspace_v6("ws").node_types[grammar_v6::REGION];
        assert_eq!(c.fields["boundary"], region.fields["boundary"]);
        assert_eq!(c.fields["handle"], region.fields["handle"]);
        assert!(c.fields["handle"].required && c.fields["boundary"].required && !c.fields["meta"].required);
        // The cluster prefix is no workspace type's.
        for (_, p, _) in grammar_v6::CONTENT_TYPES {
            assert_ne!(p, CLUSTER_PREFIX);
        }
    }

    #[test]
    fn cluster_ids_are_told_by_their_form() {
        for ok in ["CL-1", "CL-42"] {
            assert!(is_cluster_id(ok), "{ok}");
        }
        for no in ["C-1", "CL-", "CL-01", "CL-1a", "R-1", "cl-1"] {
            assert!(!is_cluster_id(no), "{no}");
        }
    }
}
