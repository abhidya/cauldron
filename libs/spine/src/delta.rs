//! The interior delta vocabulary — EXACTLY four deltas plus premise records.
//! These are the compiler's
//! target and the spine row's durable payload; **no longer anything a caller
//! speaks** (the old per-item operations survive only here,
//! demoted below the caller surface).
//!
//! The commit interior is AGNOSTIC to this vocabulary — the statement
//! compiler's clause-to-delta table is data, so a later grammar
//! edit rewrites a table, never the commit machinery.
//!
//! Everything here is `Serialize + Deserialize`: the compiled payload is
//! written durably into the spine row BEFORE any pool landing begins
//! (roll-forward replays THESE, never the statement text).

use crate::pool::{NodeId, QualifiedNodeId};
use crate::schema_def::PoolName;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The mark a PROVISIONAL serial identifier carries after its type's `-`.
/// A node type whose grammar sets `id_prefix` gets its identifiers
/// minted by the SPINE as `<prefix>-<n>` — serially per (pool, prefix),
/// `n` counting 1, 2, 3 ... with no reuse and no gap — and `n` is only known
/// at the DECIDED FLIP, the one spine transaction that is the commit point:
/// a number taken any earlier would be consumed by an attempt
/// that later aborts (a refusal under the granted locks, a restart), and
/// that is a gap.
///
/// So the compiler mints a PROVISIONAL `<prefix>-~<16 hex>` (the hex is the
/// frozen hash rule's, [`crate::ladle::compile::mint_node_id`], so a restart
/// re-mints the same text and the re-validation compare holds), every delta
/// and premise of the payload uses it, and [`crate::spine_db::SpineDb`]'s
/// decided flip rewrites each one to its serial inside the same transaction
/// that advances the counter — row payload, lock rows and counter together.
/// A provisional id therefore never lands in a pool: an aborted attempt's
/// row keeps it as the record of what was attempted, and a decided row
/// never carries one. `~` cannot occur in a serial (digits) or a hash id
/// (`n` + hex), and a prefix carries no `-` (`check_wellformed`), so the
/// shape is unambiguous.
pub const PROVISIONAL_MARK: char = '~';

/// The prefix of a PROVISIONAL serial identifier (`<prefix>-~<rest>`), or
/// `None` for any other identifier.
pub fn provisional_prefix(id: &NodeId) -> Option<&str> {
    let (prefix, rest) = id.0.split_once('-')?;
    if prefix.is_empty() || !rest.starts_with(PROVISIONAL_MARK) || rest.len() == 1 {
        return None;
    }
    Some(prefix)
}

/// The serial identifier the spine mints: `<prefix>-<n>`, `n` in decimal
/// with no padding (`T-70`, `DIR-11` — the v5 shape).
pub fn serial_node_id(prefix: &str, n: u64) -> NodeId {
    NodeId(format!("{prefix}-{n}"))
}

/// One interior delta (four variants, plus `Seal`; count
/// cells are NOT here — increments are derived mechanically inside the
/// per-pool landing transaction).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Delta {
    /// Mint a node. The identifier is minted AT COMPILE TIME and carried
    /// here, so roll-forward replay is deterministic by construction
    /// (recovery replays the compiled deltas, never re-matches).
    /// Minting rule frozen in [`crate::ladle::compile`]. For a type whose
    /// grammar sets `id_prefix` the compiled id is a PROVISIONAL the spine's
    /// decided flip rewrites to the serial `<prefix>-<n>` on the row
    /// ([`PROVISIONAL_MARK`]), so a DECIDED row's payload, the one
    /// roll-forward replays, always carries the serial.
    CreateNode {
        id: NodeId,
        node_type: String,
        /// Initial state, when the type has a state vocabulary.
        state: Option<String>,
        /// Initial top-level fields (JSON values).
        fields: BTreeMap<String, Value>,
    },
    /// Set one field, with per-field revision bookkeeping bumped by the
    /// landing (SET). `path` is the top-level field followed by
    /// dotted keys into the metadata object; never empty.
    SetField {
        node: NodeId,
        path: Vec<String>,
        value: Value,
    },
    /// Add one edge — its own record; adding it must not rewrite the node
    /// body record (load-bearing).
    AddEdge {
        src: NodeId,
        kind: String,
        dst: QualifiedNodeId,
        note: Option<String>,
        /// The edge's declared fields (the kind's
        /// [`crate::schema_def::EdgeKindDef::fields`]). Serde default and
        /// omitted when empty, so a payload compiled before this field —
        /// and every field-less edge since — serializes byte-identically
        /// and its hash identity does not move.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        fields: BTreeMap<String, Value>,
    },
    /// Remove one edge. Edges only — nodes are never removed (REMOVE:
    /// retirement is a state change through SET, unbreakable by
    /// construction because no node-removal delta exists).
    RemoveEdge {
        src: NodeId,
        kind: String,
        dst: QualifiedNodeId,
    },
    /// Set the node's machine-owned `sealed` flag — the `SEAL`
    /// statement's one delta, and the only delta that touches the flag;
    /// there is no unseal. Landing rule (`pool::Pool::apply_part`): an
    /// unsealed node becomes sealed and its whole-node `rev` bumps by 1; a
    /// node already sealed is left exactly as it is (no rev bump) — the door
    /// never emits that case (`ladle::compile`'s `seal:node` row skips a
    /// sealed node), so it reaches a landing only from a hand-built part.
    /// A FIFTH variant, added after the four-delta vocabulary above was
    /// frozen: serde-additive (`{"op":"seal",...}`), so
    /// every payload written before it still parses.
    Seal { node: NodeId },
}

/// One premise record — what the statement required to be true, re-validated
/// under the granted locks before the decided flip, and carried in the
/// payload as the durable record of what was checked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "premise", rename_all = "snake_case")]
pub enum Premise {
    /// EXPECT REV(...): whole-node when `field` is `None`, else per-field.
    RevisionIs {
        node: NodeId,
        field: Option<String>,
        rev: i64,
    },
    /// EXPECT field = value / WHERE comparison that compiled to a premise.
    FieldIs {
        node: NodeId,
        path: Vec<String>,
        value: Value,
    },
    /// State expectation (sugar over the value form, kept distinct for the
    /// richer witness).
    StateIs { node: NodeId, state: String },
    /// An edge must exist. **FORWARD ROOM, currently unproducible**: the one producer this variant anticipates
    /// — the WHERE edge-existence condition — is staged OUT
    /// (`ladle::compile::EDGE_EXISTENCE_FORM` refuses by name), so no path
    /// in this build mints it, and nothing interprets recorded premises
    /// post-hoc (re-validation re-compiles and compares payloads), so a
    /// hand-crafted one cannot silently pass either. Flipping the staged
    /// form in requires the compiler to EMIT these (and claim the slice
    /// read) — a coverage-table row deletion plus that emitter, not the
    /// one-line flip an earlier comment implied.
    EdgeExists {
        src: NodeId,
        kind: String,
        dst: QualifiedNodeId,
    },
    /// An edge must be absent — the absence watch; takes a read on the
    /// relevant slice (WHERE: create-if-absent with no merge
    /// clause). `dst: None` means "no edge of this kind at all".
    /// **FORWARD ROOM, currently unproducible** — same status and same
    /// flip cost as [`Premise::EdgeExists`] above.
    EdgeAbsent {
        src: NodeId,
        kind: String,
        dst: Option<QualifiedNodeId>,
    },
    /// EXPECT COUNT(binding): recorded with what the match actually found
    /// (count expectations).
    MatchCount {
        binding: String,
        op: CountOp,
        expected: i64,
        found: i64,
    },
}

/// The comparison a count expectation makes: exactly N, or at most N.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CountOp {
    Exactly,
    AtMost,
}

/// Binding name -> the ORDERED qualified identities the MATCH bound — the
/// `MatchSet`'s identities, never its records.
///
/// This is the OTHER half of what a RETURN projection needs after a crash.
/// [`CompiledPayload::minted`] already carries the CREATEd identities; this
/// carries the MATCHed ones. With both recorded durably before any landing,
/// recovery can project RETURN exactly as the live door does — from the
/// landed state, under the row's own locks, at the cleared flip — with no
/// re-match anywhere ("re-matching after a crash could match
/// different rows and silently do different work"). A duplicate of a
/// recovered commit is then answered with THE ORIGINAL OUTCOME from the
/// record alone, and there is no "recovered reply shape" to
/// choose. Every matched binding has an entry, even one that bound zero
/// nodes: an ABSENT binding means "not recorded" and recovery refuses
/// loudly on it; an EMPTY entry means "matched nothing" and projects zero
/// rows.
///
/// # Where it lives on disk, and why not (yet) on this struct
///
/// It is not a field of [`CompiledPayload`]: `CompiledPayload` is built by
/// struct literal in many places, and a new field breaks every one of
/// those literals. So the record is written by
/// the door into its own table in the spine file, `tx_bound`
/// ([`crate::exec::BOUND_TABLE`]), keyed by the row's sequence number, from
/// the match RE-VALIDATED UNDER THE GRANTED LOCKS — the set the live RETURN
/// projection has always been taken over — before the decided flip and
/// before any landing, so a DECIDED row always has it. **Folding it into this struct
/// is a one-pass edit**: add `#[serde(default)] pub bound:
/// BoundIdentities` here, fix the literals, and replace `exec::record_bound`
/// / `exec::read_bound` with the field. The JSON shape stored in `tx_bound`
/// is byte-for-byte what that field would serialize to, so no migration.
pub type BoundIdentities = BTreeMap<String, Vec<QualifiedNodeId>>;

/// The full compiled change payload of one statement — what is written
/// durably into the spine row BEFORE any pool landing begins (the recovery
/// design is the argument).
///
/// The bound identities of the MATCH — the third thing a RETURN needs after
/// a crash — are [`BoundIdentities`], recorded beside this payload (see its
/// docs for why beside rather than on it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledPayload {
    /// One part per touched pool. Parts land in THIS vector's order, which
    /// is frozen as ascending pool name — deterministic, so the
    /// between-landings kill window is reproducible (the atomicity
    /// tests).
    pub parts: Vec<CompiledPart>,
    /// Binding name -> the identifier minted for each CREATEd node, so
    /// RETURN hands identifiers back without a second read.
    pub minted: BTreeMap<String, QualifiedNodeId>,
}

/// One pool's share of a commit — lands as ONE atomic SQLite transaction on
/// that pool's file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledPart {
    pub pool: PoolName,
    /// The pool's pinned grammar version this part validated under — the
    /// row's per-part validated-under stamp (the spine is
    /// version-blind; this is the whole cross-version record).
    pub schema_version: u32,
    pub deltas: Vec<Delta>,
    /// The premises that touch THIS pool's rows.
    pub premises: Vec<Premise>,
    /// For every OTHER pool a cross-pool edge in this part points into: the
    /// pinned version of THAT pool's grammar this part's cross-pool edges
    /// validated against (the far side's schema version, made
    /// durable on the row forever). The projected namespace records
    /// [`crate::envelope::ENVELOPE_FORMAT_VERSION`]. Filled by the compiler;
    /// read by a pool landing as the record that the door's far check ran
    /// (`schema_def::FarSide::Landing`). Serde default so payloads written
    /// before this field parse (as parts with no cross-pool edges).
    #[serde(default)]
    pub far_versions: BTreeMap<PoolName, u32>,
}

impl CompiledPayload {
    /// Every PROVISIONAL serial this payload mints, in the order the spine
    /// numbers them: parts in landing order, CREATE deltas in delta order
    /// within a part (which is the statement's CREATE order). Each entry is
    /// (pool, prefix, provisional id). Empty for a payload that mints no
    /// prefixed type — the decided flip then touches no counter.
    pub fn provisional_mints(&self) -> Vec<(PoolName, String, NodeId)> {
        let mut out = Vec::new();
        for part in &self.parts {
            for d in &part.deltas {
                if let Delta::CreateNode { id, .. } = d {
                    if let Some(prefix) = provisional_prefix(id) {
                        out.push((part.pool.clone(), prefix.to_string(), id.clone()));
                    }
                }
            }
        }
        out
    }

    /// Every CARRIED serial this payload lands:
    /// each `CreateNode` whose id splits at its first `-` into a non-empty
    /// prefix and a rest `[1-9][0-9]*` that parses as `u64`, as (pool,
    /// prefix, n), in part then delta order. DERIVED, never recorded: a
    /// compiled `CreateNode` id has exactly three shapes — a hash id
    /// `n<16 hex>` (no `-`), a provisional `<P>-~<hex>` (rest starts `~`),
    /// or a carried serial (rest is digits) — because CREATE refuses every
    /// caller id, so the IMPORT form is the only producer of the third. The
    /// decided flip advances each (pool, prefix) counter to `max(last, n)`
    /// before it numbers the row's provisionals.
    pub fn carried_serials(&self) -> Vec<(PoolName, String, u64)> {
        let mut out = Vec::new();
        for part in &self.parts {
            for d in &part.deltas {
                let Delta::CreateNode { id, .. } = d else {
                    continue;
                };
                let Some((prefix, rest)) = id.0.split_once('-') else {
                    continue;
                };
                let serial = !prefix.is_empty()
                    && rest.bytes().next().is_some_and(|b| (b'1'..=b'9').contains(&b))
                    && rest.bytes().all(|b| b.is_ascii_digit());
                if !serial {
                    continue;
                }
                if let Ok(n) = rest.parse::<u64>() {
                    out.push((part.pool.clone(), prefix.to_string(), n));
                }
            }
        }
        out
    }

    /// Rewrite every occurrence of a mapped identifier — CREATE ids, SET
    /// targets, edge endpoints on both sides (a cross-pool edge's far end
    /// included), premises, and the `minted` map RETURN reads — so the
    /// payload names one node by one identifier everywhere. `map` is keyed
    /// by the QUALIFIED identity: a part's local ids are qualified by the
    /// part's pool.
    pub fn assign_ids(&mut self, map: &BTreeMap<QualifiedNodeId, NodeId>) {
        if map.is_empty() {
            return;
        }
        let local = |pool: &PoolName, id: &mut NodeId| {
            let key = QualifiedNodeId {
                pool: pool.clone(),
                id: id.clone(),
            };
            if let Some(to) = map.get(&key) {
                *id = to.clone();
            }
        };
        let qualified = |q: &mut QualifiedNodeId| {
            if let Some(to) = map.get(q) {
                q.id = to.clone();
            }
        };
        for part in &mut self.parts {
            let pool = part.pool.clone();
            for d in &mut part.deltas {
                match d {
                    Delta::CreateNode { id, .. } => local(&pool, id),
                    Delta::SetField { node, .. } | Delta::Seal { node } => local(&pool, node),
                    Delta::AddEdge { src, dst, .. } | Delta::RemoveEdge { src, dst, .. } => {
                        local(&pool, src);
                        qualified(dst);
                    }
                }
            }
            for p in &mut part.premises {
                match p {
                    Premise::RevisionIs { node, .. }
                    | Premise::FieldIs { node, .. }
                    | Premise::StateIs { node, .. } => local(&pool, node),
                    Premise::EdgeExists { src, dst, .. } => {
                        local(&pool, src);
                        qualified(dst);
                    }
                    Premise::EdgeAbsent { src, dst, .. } => {
                        local(&pool, src);
                        if let Some(dst) = dst {
                            qualified(dst);
                        }
                    }
                    Premise::MatchCount { .. } => {}
                }
            }
        }
        for q in self.minted.values_mut() {
            qualified(q);
        }
    }
}
