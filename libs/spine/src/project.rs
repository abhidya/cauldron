//! The projection — how spine rows appear as nodes WITHOUT being nodes.
//! The origin of these nodes is code: they are hard-coded because they are
//! structural.
//!
//! - Computed on READ, never copied — the code's tables are the only
//!   storage; there is no second copy to go stale.
//! - The pen REFUSES any write addressed to the projected namespace.
//! - A GHOST NODE is an identifier that can be the target of edges but has
//!   no stored node content anywhere; an edge minted toward a ghost
//!   identifier that names no row is refused.

use crate::envelope::{TxHash, ENVELOPE_FORMAT_VERSION};
use crate::error::{Result, SpineError};
use crate::pool::{FieldValue, NodeId, NodeRecord, QualifiedNodeId};
use crate::schema_def::{NodeTypeDef, PoolGrammar, PoolKind, PoolName};
use crate::spine_db::SpineDb;
use crate::witness::Witness;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// The reserved namespace's pool name (frozen for all workers): `spine`.
/// No real pool may take this name ([`crate::schema_def::PoolName`]).
/// Edges cross-linking provenance address `spine/<hash-hex>`.
pub const PROJECTED_POOL: &str = "spine";

/// The projected namespace's pool KIND: what a source pool's reach
/// names to declare a provenance edge toward a transaction —
/// `TypeRef::reach("spine", "txn")`. No real pool may declare this kind.
pub const PROJECTED_KIND: &str = "spine";

/// The one projected node type in this build: a transaction row, projected.
pub const TXN_NODE_TYPE: &str = "txn";

/// The projected node's state vocabulary — the row's lifecycle, computed on
/// read (see [`resolve_ghost`]).
pub const TXN_STATES: &[&str] = &["pending", "decided", "cleared"];

/// Is this pool name the reserved projected namespace?
pub fn is_projected(pool: &PoolName) -> bool {
    pool.as_str() == PROJECTED_POOL
}

/// The projected namespace's grammar — CODE-DEFINED, never pinned into a file
/// (the origin of these nodes in the core schema is code, hard-coded because
/// it is structural). This is what
/// makes the projected namespace a pool KIND a source grammar can reach
/// toward, and what the cross-pool lookup finds when it "checks the
/// schema version on it": version = [`ENVELOPE_FORMAT_VERSION`] (the shape
/// moves when that grammar moves, and never otherwise); one node type,
/// `txn`, with NO settable fields (its fields are derived on read — `ts`,
/// `initiator`, `instrument`, `statement`, `parts` — and the pen refuses every
/// write anyway); NO edge kinds (a ghost is a target, never a source: it has
/// no stored record to hang an edge off).
///
/// Deliberately NOT run through `check_wellformed`: that check refuses the
/// reserved name and kind precisely so no REAL pool can shadow this one.
pub fn projected_grammar() -> PoolGrammar {
    PoolGrammar {
        pool: PoolName::new(PROJECTED_POOL),
        kind: PoolKind::new(PROJECTED_KIND),
        version: ENVELOPE_FORMAT_VERSION,
        node_types: BTreeMap::from([(
            TXN_NODE_TYPE.to_string(),
            NodeTypeDef {
                fields: BTreeMap::new(),
                states: TXN_STATES.iter().map(|s| s.to_string()).collect(),
                count_cells: BTreeMap::new(),
                ..Default::default()
            },
        )]),
        edge_kinds: BTreeMap::new(),
    }
}

/// The ghost identifier of a transaction: its 64-char hex hash — the
/// address IS the row's identity; the target is the address, not a copy.
pub fn ghost_id(tx: &TxHash) -> NodeId {
    NodeId(tx.to_hex())
}

/// Resolve a ghost identifier against the spine's tables at read time
/// (computed on read, never copied). `None` when the
/// identifier names no row — which refuses an edge mint toward it
/// ([`Witness::GhostTargetUnknown`]).
///
/// The projected record's shape (frozen): `node_type` = `txn`, `state` =
/// `pending` | `decided` | `cleared`, fields `ts`, `initiator`,
/// `instrument`, `statement`, and per-part `parts` (pool, landed,
/// validated-under); `rev` is always 1 and the schema-version stamp is the
/// spine's own [`crate::envelope::ENVELOPE_FORMAT_VERSION`].
pub fn resolve_ghost(spine: &SpineDb, id: &NodeId) -> Result<Option<NodeRecord>> {
    // The identifier IS the address. Anything that is not a 64-hex hash names
    // no row — and says so as `None` rather than as an error, because "is
    // there a row for this address" is exactly the question the ghost-target
    // check asks.
    let Some(hash) = TxHash::from_hex(&id.0) else {
        return Ok(None);
    };
    let Some(row) = spine.find_by_hash(&hash)? else {
        return Ok(None);
    };

    // COMPUTED ON READ, NEVER COPIED: every value below is
    // derived from the row on this call. There is no second copy to go stale,
    // which is the whole reason the projection is a projection.
    let state = if row.cleared {
        "cleared"
    } else if row.decided {
        "decided"
    } else {
        "pending"
    };

    let parts: Vec<Value> = row
        .parts
        .iter()
        .map(|p| {
            json!({
                "part": p.part,
                "pool": p.pool.as_str(),
                "landed": p.landed,
                // The per-part validated-under stamp — the whole cross-version
                // record (the spine is version-blind).
                "validated_under": p.schema_version,
            })
        })
        .collect();

    let mut fields: BTreeMap<String, FieldValue> = BTreeMap::new();
    let mut put = |name: &str, value: Value| {
        // `rev` is always 1: a projected node has no revision history of its
        // own, because it has no stored body to revise.
        fields.insert(name.to_string(), FieldValue { value, rev: 1 });
    };
    put("ts", json!(row.timestamp));
    put(
        "initiator",
        serde_json::to_value(&row.initiator)
            .map_err(|e| SpineError::hard(format!("initiator will not serialize: {e}")))?,
    );
    put(
        "instrument",
        serde_json::to_value(&row.instrument)
            .map_err(|e| SpineError::hard(format!("instrument will not serialize: {e}")))?,
    );
    put("statement", json!(row.statement));
    put("parts", Value::Array(parts));

    Ok(Some(NodeRecord {
        id: id.clone(),
        node_type: TXN_NODE_TYPE.to_string(),
        state: Some(state.to_string()),
        rev: 1,
        // The projection's own stamp is the spine's envelope format version:
        // the shape above moves when that grammar moves, and never otherwise.
        schema_version: ENVELOPE_FORMAT_VERSION,
        // A ghost has no body to freeze and nothing can write it (the pen's
        // net refuses every write into the projection), so the seal flag
        // carries no information here.
        sealed: false,
        fields,
    }))
}

/// The pen's guard on every write target (the origin of these nodes is
/// CODE — hard-coded because it is structural). Any write addressed to the
/// reserved namespace refuses with a structured refusal; an editable
/// projection of code state is a lie waiting to be written.
///
/// # Its callers, exactly
///
/// THE PEN'S NET is `ladle::compile`, which calls this at its three
/// write-target sites and nowhere else:
///
/// 1. every CREATE head, in `compile` before any identifier is minted — the
///    target text is `CREATE (<binding>)` (a created node has no id yet);
/// 2. every SET target, in `emit_set_field` — including a MATCH-by-identifier
///    ghost, bindable by identifier;
/// 3. every edge SOURCE, CREATE and REMOVE alike, in `edge_deltas`.
///
/// All three run in phase 7 (compile), BEFORE the row append at phase 8, so
/// a write addressed into the projection refuses with nothing written.
///
/// `exec`'s phase-5 CREATE-head check is an EARLIER DUPLICATE, for CREATE
/// heads only, and it does not call this function: it builds the same
/// [`Witness::ProjectedNamespaceWrite`] inline, with the same
/// `CREATE (<binding>)` target text, so a CREATE into `spine` refuses before
/// the match phase opens any pool. SET targets and edge sources cannot be
/// checked there — they reach the projection only through a binding the match
/// resolves — so for those two the net in `compile` is the only one.
///
/// It is deliberately NOT called on an edge's destination: pointing AT a
/// transaction is the entire point of the projection (see
/// [`guard_edge_target`]).
pub fn guard_write_target(target: &QualifiedNodeId) -> Result<()> {
    if is_projected(&target.pool) {
        return Err(projected_write_refusal(target));
    }
    Ok(())
}

/// The guard on an edge's DESTINATION. A projected target must name a real
/// spine row: "an edge minted toward a ghost identifier that names no row is
/// refused" — provenance edges need a target, and the target is
/// the address, not a copy. A non-projected destination is not this module's
/// business and passes through.
///
/// Its caller is the write door (`exec`, phase 5): for every CREATE-edge
/// clause whose far end is a by-identifier MATCH head into `spine`, the
/// identifier is resolved against the spine before the match runs. Without
/// this guard an unresolvable ghost would simply bind nothing, the edge
/// would expand to zero deltas, and the statement would "succeed" landing
/// nothing — the empty-match rule swallowing exactly the
/// refusal the projection demands.
pub fn guard_edge_target(spine: &SpineDb, target: &QualifiedNodeId) -> Result<()> {
    if !is_projected(&target.pool) {
        return Ok(());
    }
    match resolve_ghost(spine, &target.id)? {
        Some(_) => Ok(()),
        None => Err(SpineError::refuse(Witness::GhostTargetUnknown {
            target: target.to_string(),
        })),
    }
}

/// The pen-refusal for a write addressed to the projected namespace (an
/// editable projection of code state is a lie waiting to be
/// written). Constructs the structured refusal. Its one
/// caller in the crate is [`guard_write_target`] (`compile`'s
/// three write-target sites go through the guard); `exec`'s phase-5 duplicate builds the
/// same witness inline.
pub fn projected_write_refusal(target: &QualifiedNodeId) -> SpineError {
    SpineError::refuse(Witness::ProjectedNamespaceWrite {
        pool: target.pool.clone(),
        target: target.id.0.clone(),
    })
}
