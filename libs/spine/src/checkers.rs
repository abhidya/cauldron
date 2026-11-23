//! The migration LEDGER checkers — deterministic, pure, and the only thing in
//! this build that decides whether a migration went well.
//!
//! > "An agent (the migration driver) NEVER decides pass or fail — it
//! > produces material (statements, manifests, reports) and deterministic
//! > checkers assert over what it produced and what landed."
//!
//! # THE LEDGER, NEVER THE CONTENT
//!
//! The boundary: *what the migration test asserts is the ledger, never the
//! content.* An old-equals-new assertion is BARRED by design: there is no
//! oracle for it. There is
//! **NO** old-versus-new comparison in this module, and no helper here can be
//! composed into one: nothing in this file ever sees source CONTENT. The five
//! checkers take a manifest, a census of the destination, the spine's own
//! rows, and two byte digests — and a byte digest is deliberately the only
//! thing here that touches the source at all, because it is the one question
//! about the source that is not a comparison of meaning.
//!
//! A sixth property — faithfulness of meaning — is JUDGMENT: reported,
//! flagged as judgment, never a gate, because there is no oracle for it. It is therefore ABSENT from this module by design, not by
//! omission.
//!
//! # Pure functions, impure collectors, named apart
//!
//! Each of the five checkers is a PURE function from values to a
//! [`Verdict`] — no I/O, no clock, no randomness, so the same inputs give the
//! same verdict forever and a poisoned fixture can force any of them red by
//! construction. The COLLECTORS below them read a real cauldron and build
//! those values; they are marked IMPURE in their own docs and they assert
//! nothing.
//!
//! # A checker with no red fixture does not exist
//!
//! Every checker ships with at least one poisoned fixture that makes it
//! fire — a checker with no red fixture does not count as existing. All five
//! are forced red in `tests/migration_rehearsal.rs`, and each poison is named
//! there.

use crate::envelope::{Initiator, Instrument, TxHash};
use crate::error::{Result, SpineError};
use crate::migrate::Manifest;
use crate::migrate::{Disposition, Resolution};
use crate::pool::{NodeId, Pool, QualifiedNodeId};
use crate::schema_def::PoolName;
use crate::spine_db::TxRow;
use crate::Cauldron;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

// ---------------------------------------------------------------------------
// the verdict shape
// ---------------------------------------------------------------------------

/// The five frozen checker names. Suites assert on these strings; a report
/// tabulates them.
pub const TOTAL_ACCOUNTING: &str = "total-accounting";
pub const REFERENTIAL_CLOSURE: &str = "referential-closure";
pub const ONE_DOOR_PROVENANCE: &str = "one-door-provenance";
pub const ATOMIC_OR_CLEANLY_ABORTED: &str = "atomic-or-cleanly-aborted";
pub const SOURCE_UNTOUCHED: &str = "source-untouched";

/// One checker's structured verdict. PASSING IS THE ABSENCE OF FINDINGS —
/// there is no `passed: bool` field to set independently of the evidence, so
/// a green verdict cannot be constructed beside a red finding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub checker: &'static str,
    pub findings: Vec<Finding>,
}

impl Verdict {
    fn new(checker: &'static str, findings: Vec<Finding>) -> Verdict {
        Verdict { checker, findings }
    }

    pub fn passed(&self) -> bool {
        self.findings.is_empty()
    }

    /// Panic-free assertion helper for suites that want the findings in the
    /// failure message rather than a bare `false`.
    pub fn expect_pass(&self) -> Result<()> {
        if self.passed() {
            return Ok(());
        }
        Err(SpineError::hard(format!(
            "checker `{}` fired: {}",
            self.checker,
            serde_json::to_string(&self.findings).unwrap_or_else(|e| format!("<{e}>"))
        )))
    }
}

/// One structured finding. Structured for the same reason witnesses are
/// structured: a later suite asserts on WHY a checker fired, and prose cannot
/// be asserted on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "finding", rename_all = "kebab-case")]
pub enum Finding {
    // ---- total accounting ----
    /// The manifest has a different number of entries than the source had
    /// items — a silent drop.
    EntryCountMismatch { source_count: usize, entries: usize },
    /// Imported plus rejected does not equal the source count.
    PartitionMismatch {
        source_count: usize,
        imported: usize,
        rejected: usize,
    },
    /// One source identity has two entries; the accounting is ambiguous.
    DuplicateSourceId { source_id: String },
    /// A rejection with no reason code is a silent drop wearing a label.
    RejectionWithoutReason { source_id: String },
    /// An unresolved import with no marker — the marker must be
    /// EXPLICIT.
    UnresolvedWithoutMarker { source_id: String },

    // ---- referential closure ----
    /// An edge in the destination points at a node that is not there.
    DanglingDestinationEdge {
        pool: PoolName,
        src: NodeId,
        kind: String,
        dst: QualifiedNodeId,
    },
    /// An edge points into a pool the census does not cover, so closure
    /// cannot be established — never silently assumed.
    EndpointPoolNotCensused {
        pool: PoolName,
        src: NodeId,
        kind: String,
        dst: QualifiedNodeId,
    },

    // ---- one-door provenance ----
    /// A landed node names a transaction the spine has no row for — the
    /// signature of a SECOND WRITE DOOR (a second import path is
    /// a second write door — a defect on day one).
    NodeWithoutSpineRow {
        pool: PoolName,
        node: NodeId,
        created_tx: String,
    },
    /// The row exists but never reached the commit point (the
    /// decided flip IS the commit point), so this node is in the destination
    /// ahead of its own record.
    NodeFromUndecidedRow {
        pool: PoolName,
        node: NodeId,
        created_tx: String,
        decided: bool,
        cleared: bool,
    },
    /// The row exists and is decided, but its compiled payload never names
    /// this node — the node did not come through THAT row either.
    NodeNotInRowPayload {
        pool: PoolName,
        node: NodeId,
        created_tx: String,
    },
    /// The same three, for an edge.
    EdgeWithoutSpineRow {
        pool: PoolName,
        src: NodeId,
        kind: String,
        dst: QualifiedNodeId,
        created_tx: String,
    },
    EdgeFromUndecidedRow {
        pool: PoolName,
        src: NodeId,
        kind: String,
        created_tx: String,
    },
    EdgeNotInRowPayload {
        pool: PoolName,
        src: NodeId,
        kind: String,
        dst: QualifiedNodeId,
        created_tx: String,
    },

    // ---- atomic or cleanly aborted ----
    /// A decided attempt whose effects are not all present: the destination
    /// is neither all-of-attempt nor none-of-attempt.
    DecidedAttemptMissingEffect { tx: String, effect: String },
    /// No attempt at this identity is decided, and yet an effect of one is in
    /// the destination — an abort left residue.
    AbortedAttemptLeftResidue { tx: String, effect: String },
    /// A decided row with an unlanded part: the row itself records the
    /// half-landed state (recovery's own input — and a run that reports
    /// without running recovery first is reporting a window, not a state).
    DecidedRowHasUnlandedPart { tx: String, pool: PoolName },
    /// Two decided rows at one identity — exactly-once broken.
    MultipleDecidedAttempts { tx: String, decided: usize },
    /// The attempt history is empty: nothing to be atomic about, and the
    /// caller asked about an identity the spine never saw.
    NoSuchAttempt { tx: String },

    // ---- source untouched ----
    /// A source entry's bytes changed. The migration is ONE-WAY and the old
    /// files are NEVER written.
    SourceEntryChanged {
        entry: String,
        before: String,
        after: String,
    },
    SourceEntryAdded { entry: String },
    SourceEntryRemoved { entry: String },
}

// ---------------------------------------------------------------------------
// checker 1 — total accounting (PURE)
// ---------------------------------------------------------------------------

/// **Total accounting**: *imported plus rejected-with-reason
/// equals source count, no silent drop.*
///
/// Also asserts the two things that would let the headline equality hold
/// while the accounting was still a lie: a rejection must carry a reason
/// code, and an unresolved import must carry its explicit marker.
pub fn total_accounting(manifest: &Manifest) -> Verdict {
    let mut findings = Vec::new();
    if manifest.entries.len() != manifest.source_count {
        findings.push(Finding::EntryCountMismatch {
            source_count: manifest.source_count,
            entries: manifest.entries.len(),
        });
    }
    let imported = manifest.imported();
    let rejected = manifest.rejected();
    if imported + rejected != manifest.source_count {
        findings.push(Finding::PartitionMismatch {
            source_count: manifest.source_count,
            imported,
            rejected,
        });
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for entry in &manifest.entries {
        if !seen.insert(entry.source_id.as_str()) {
            findings.push(Finding::DuplicateSourceId {
                source_id: entry.source_id.clone(),
            });
        }
        match &entry.disposition {
            Disposition::Rejected { reason } => {
                if reason.code.trim().is_empty() {
                    findings.push(Finding::RejectionWithoutReason {
                        source_id: entry.source_id.clone(),
                    });
                }
            }
            Disposition::Imported { resolution, .. } => {
                if let Resolution::UnresolvedEndpoint { marker, .. } = resolution {
                    if marker.trim().is_empty() {
                        findings.push(Finding::UnresolvedWithoutMarker {
                            source_id: entry.source_id.clone(),
                        });
                    }
                }
            }
        }
    }
    Verdict::new(TOTAL_ACCOUNTING, findings)
}

// ---------------------------------------------------------------------------
// checker 2 — referential closure (PURE)
// ---------------------------------------------------------------------------

/// **Referential closure of the destination**: every edge in the
/// destination has both endpoints in the destination.
///
/// A cross-pool edge (e.g. `catalogues`) is checked across the census,
/// which is why the census is whole-cauldron and not per-pool: closure of one
/// pool alone is not closure. The door checks a far node's existence at
/// commit time, but a node deleted beside the door afterwards is
/// exactly what THIS check exists to catch.
///
/// An edge into the projected `spine` namespace (a ghost edge) is
/// closed when the ghost RESOLVES — its address names a spine row — and
/// dangling when it does not. The census carries that fact in
/// [`DestinationCensus::resolved_ghosts`], collected by [`census`] from the
/// spine's own tables; this function stays pure.
pub fn referential_closure(census: &DestinationCensus) -> Verdict {
    let mut findings = Vec::new();
    for (pool, contents) in &census.pools {
        for edge in &contents.edges {
            if !contents.nodes.contains_key(&edge.src) {
                findings.push(Finding::DanglingDestinationEdge {
                    pool: pool.clone(),
                    src: edge.src.clone(),
                    kind: edge.kind.clone(),
                    dst: QualifiedNodeId {
                        pool: pool.clone(),
                        id: edge.src.clone(),
                    },
                });
            }
            if crate::project::is_projected(&edge.dst.pool) {
                if !census.resolved_ghosts.contains(&edge.dst.id) {
                    findings.push(Finding::DanglingDestinationEdge {
                        pool: pool.clone(),
                        src: edge.src.clone(),
                        kind: edge.kind.clone(),
                        dst: edge.dst.clone(),
                    });
                }
                continue;
            }
            match census.pools.get(&edge.dst.pool) {
                None => findings.push(Finding::EndpointPoolNotCensused {
                    pool: pool.clone(),
                    src: edge.src.clone(),
                    kind: edge.kind.clone(),
                    dst: edge.dst.clone(),
                }),
                Some(far) => {
                    if !far.nodes.contains_key(&edge.dst.id) {
                        findings.push(Finding::DanglingDestinationEdge {
                            pool: pool.clone(),
                            src: edge.src.clone(),
                            kind: edge.kind.clone(),
                            dst: edge.dst.clone(),
                        });
                    }
                }
            }
        }
    }
    Verdict::new(REFERENTIAL_CLOSURE, findings)
}

// ---------------------------------------------------------------------------
// checker 3 — one-door provenance (PURE)
// ---------------------------------------------------------------------------

/// **One-door provenance**: *every landed node traces to a spine
/// row.*
///
/// Three questions per landed node and per landed edge, because any one alone
/// can be satisfied by an impostor: the transaction it names must EXIST in the
/// spine; that row must have reached the commit point (decided);
/// and the row's own compiled payload must NAME this node. The third is the
/// one that catches a node inserted beside the door that borrowed a real
/// transaction's hash.
pub fn one_door_provenance(census: &DestinationCensus, ledger: &SpineLedger) -> Verdict {
    let mut findings = Vec::new();
    for (pool, contents) in &census.pools {
        for (id, node) in &contents.nodes {
            match ledger.rows.get(&node.created_tx) {
                None => findings.push(Finding::NodeWithoutSpineRow {
                    pool: pool.clone(),
                    node: id.clone(),
                    created_tx: node.created_tx.clone(),
                }),
                Some(row) if !row.decided => findings.push(Finding::NodeFromUndecidedRow {
                    pool: pool.clone(),
                    node: id.clone(),
                    created_tx: node.created_tx.clone(),
                    decided: row.decided,
                    cleared: row.cleared,
                }),
                Some(row) => {
                    if !row.creates_node(pool, id) {
                        findings.push(Finding::NodeNotInRowPayload {
                            pool: pool.clone(),
                            node: id.clone(),
                            created_tx: node.created_tx.clone(),
                        });
                    }
                }
            }
        }
        for edge in &contents.edges {
            match ledger.rows.get(&edge.created_tx) {
                None => findings.push(Finding::EdgeWithoutSpineRow {
                    pool: pool.clone(),
                    src: edge.src.clone(),
                    kind: edge.kind.clone(),
                    dst: edge.dst.clone(),
                    created_tx: edge.created_tx.clone(),
                }),
                Some(row) if !row.decided => findings.push(Finding::EdgeFromUndecidedRow {
                    pool: pool.clone(),
                    src: edge.src.clone(),
                    kind: edge.kind.clone(),
                    created_tx: edge.created_tx.clone(),
                }),
                Some(row) => {
                    if !row.adds_edge(pool, &edge.src, &edge.kind, &edge.dst) {
                        findings.push(Finding::EdgeNotInRowPayload {
                            pool: pool.clone(),
                            src: edge.src.clone(),
                            kind: edge.kind.clone(),
                            dst: edge.dst.clone(),
                            created_tx: edge.created_tx.clone(),
                        });
                    }
                }
            }
        }
    }
    Verdict::new(ONE_DOOR_PROVENANCE, findings)
}

// ---------------------------------------------------------------------------
// checker 4 — atomic or cleanly aborted (PURE)
// ---------------------------------------------------------------------------

/// **Atomic-or-cleanly-aborted**: *the destination is
/// all-of-attempt or none-of-attempt, ever.*
///
/// Takes the WHOLE attempt history of one identity, not one row, because
/// one hash identity can name several rows: a restart
/// records the abandoned attempt aborted and appends a fresh row with a fresh
/// sequence number. The rule composed over that history:
///
/// - **at most one row may be decided** — the commit point is the decided
///   flip, so two decided rows would be two landings of one ask;
/// - **if a row IS decided**, every effect its payload names must be present
///   in the destination, and every part of it must be recorded landed;
/// - **if NO row is decided**, no effect of ANY attempt may be present.
///
/// TWO WAYS TO SEE RESIDUE in the none-of-attempt arm, because a row's
/// payload does not always name the identifiers the destination would hold:
///
/// - **by identifier**: a type with no `id_prefix` is minted from the hash
///   (`ladle::compile::mint_node_id`), so every attempt at one identity names
///   the SAME identifiers and a residue node is found by its id — which is
///   why that half is evaluated over the whole history rather than per row.
/// - **by provenance**: a type with an `id_prefix` is minted a
///   PROVISIONAL `<prefix>-~<hex>` in the payload and numbered `<prefix>-<n>`
///   only at the decided flip, so an UNDECIDED row's payload never names the
///   serial a residue node would carry, and the identifier half cannot see
///   it (without this half, for prefixed types this arm could not
///   fail). The census carries every node's `created_tx` and `updated_tx`
///   and every edge's `created_tx`, so any node or edge stamped with this
///   identity while no attempt at it is decided is residue, whatever its id
///   looks like. The two halves are deduplicated by effect name.
///
/// The decided arm needs no such second half: the flip rewrites the decided
/// row's payload to the serials in the same transaction
/// (`spine_db::assign_serials`), so its effects name what landed.
///
/// KNOWN LIMIT, stated rather than hidden: the DECIDED arm checks only
/// CREATE-shaped effects (nodes and edges). A `SetField` delta's presence is
/// not observable in the census, which carries no field values. A
/// migration's freight is creates; a driver that also set fields on
/// pre-existing nodes would need the census widened. A known weak
/// seam. (The none-of-attempt arm does see a SET's residue, by the
/// node's `updated_tx`.)
pub fn atomic_or_cleanly_aborted(history: &AttemptHistory, census: &DestinationCensus) -> Verdict {
    let mut findings = Vec::new();
    if history.rows.is_empty() {
        return Verdict::new(
            ATOMIC_OR_CLEANLY_ABORTED,
            vec![Finding::NoSuchAttempt {
                tx: history.tx.clone(),
            }],
        );
    }
    let decided: Vec<&LedgerRow> = history.rows.iter().filter(|r| r.decided).collect();
    if decided.len() > 1 {
        findings.push(Finding::MultipleDecidedAttempts {
            tx: history.tx.clone(),
            decided: decided.len(),
        });
    }

    if let Some(row) = decided.first() {
        for part in &row.parts {
            if !part.landed {
                findings.push(Finding::DecidedRowHasUnlandedPart {
                    tx: history.tx.clone(),
                    pool: part.pool.clone(),
                });
            }
        }
        for effect in row.effects() {
            if !census.holds(&effect) {
                findings.push(Finding::DecidedAttemptMissingEffect {
                    tx: history.tx.clone(),
                    effect: effect.name(),
                });
            }
        }
    } else {
        let mut residue: BTreeSet<String> = BTreeSet::new();
        // By identifier: what any attempt's payload names and the census
        // holds.
        for row in &history.rows {
            for effect in row.effects() {
                if census.holds(&effect) {
                    residue.insert(effect.name());
                }
            }
        }
        // By provenance: anything the census stamps with this identity. The
        // only half that can see a prefixed type's residue (see above).
        for (pool, contents) in &census.pools {
            for (id, node) in &contents.nodes {
                if node.created_tx == history.tx || node.updated_tx == history.tx {
                    residue.insert(
                        Effect::Node {
                            pool: pool.clone(),
                            id: id.clone(),
                        }
                        .name(),
                    );
                }
            }
            for edge in &contents.edges {
                if edge.created_tx == history.tx {
                    residue.insert(
                        Effect::Edge {
                            pool: pool.clone(),
                            src: edge.src.clone(),
                            kind: edge.kind.clone(),
                            dst: edge.dst.clone(),
                        }
                        .name(),
                    );
                }
            }
        }
        for effect in residue {
            findings.push(Finding::AbortedAttemptLeftResidue {
                tx: history.tx.clone(),
                effect,
            });
        }
    }
    Verdict::new(ATOMIC_OR_CLEANLY_ABORTED, findings)
}

// ---------------------------------------------------------------------------
// checker 5 — source untouched (PURE)
// ---------------------------------------------------------------------------

/// **Source untouched**: *byte-hash before equals
/// after* — the old files are never ground truth and are NEVER written
/// (byte-identical before and after; read-only enforced).
///
/// This is the ONE checker whose subject is the source, and it is a byte
/// question, never a meaning question: it compares two digests taken by the
/// caller and never opens a file itself. That boundary is what keeps the
/// barred old-equals-new assertion out of reach — there
/// is nothing here to compare content WITH.
pub fn source_untouched(before: &SourceDigest, after: &SourceDigest) -> Verdict {
    let mut findings = Vec::new();
    for (entry, hash) in &before.entries {
        match after.entries.get(entry) {
            None => findings.push(Finding::SourceEntryRemoved {
                entry: entry.clone(),
            }),
            Some(now) if now != hash => findings.push(Finding::SourceEntryChanged {
                entry: entry.clone(),
                before: hash.clone(),
                after: now.clone(),
            }),
            Some(_) => {}
        }
    }
    for entry in after.entries.keys() {
        if !before.entries.contains_key(entry) {
            findings.push(Finding::SourceEntryAdded {
                entry: entry.clone(),
            });
        }
    }
    Verdict::new(SOURCE_UNTOUCHED, findings)
}

// ---------------------------------------------------------------------------
// the values the checkers take
// ---------------------------------------------------------------------------

/// Everything the destination holds, by pool. Whole-cauldron, because
/// referential closure across a cross-pool edge is not a per-pool question.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DestinationCensus {
    pub pools: BTreeMap<PoolName, PoolCensus>,
    /// Every ghost identifier some destination edge points at that RESOLVES
    /// to a spine row — what [`referential_closure`] counts an edge
    /// into the projected namespace as closed by. Collected by [`census`];
    /// a census built by hand that omits it reads every ghost edge as
    /// dangling, which is the honest default.
    #[serde(default)]
    pub resolved_ghosts: BTreeSet<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PoolCensus {
    pub nodes: BTreeMap<NodeId, CensusNode>,
    pub edges: Vec<CensusEdge>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CensusNode {
    pub node_type: String,
    pub state: Option<String>,
    /// The transaction that created this node, 64 lowercase hex — the row it
    /// must trace back to.
    pub created_tx: String,
    pub updated_tx: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CensusEdge {
    pub src: NodeId,
    pub kind: String,
    pub dst: QualifiedNodeId,
    pub created_tx: String,
}

impl DestinationCensus {
    pub fn node_count(&self) -> usize {
        self.pools.values().map(|p| p.nodes.len()).sum()
    }

    pub fn edge_count(&self) -> usize {
        self.pools.values().map(|p| p.edges.len()).sum()
    }

    fn holds(&self, effect: &Effect) -> bool {
        match effect {
            Effect::Node { pool, id } => self
                .pools
                .get(pool)
                .is_some_and(|p| p.nodes.contains_key(id)),
            Effect::Edge {
                pool,
                src,
                kind,
                dst,
            } => self.pools.get(pool).is_some_and(|p| {
                p.edges
                    .iter()
                    .any(|e| &e.src == src && &e.kind == kind && &e.dst == dst)
            }),
        }
    }
}

/// One create-shaped effect a spine row's payload names.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Effect {
    Node {
        pool: PoolName,
        id: NodeId,
    },
    Edge {
        pool: PoolName,
        src: NodeId,
        kind: String,
        dst: QualifiedNodeId,
    },
}

impl Effect {
    fn name(&self) -> String {
        match self {
            Effect::Node { pool, id } => format!("node {pool}/{id}"),
            Effect::Edge {
                pool,
                src,
                kind,
                dst,
            } => format!("edge {pool}/{src} -[{kind}]-> {dst}"),
        }
    }
}

/// The spine's own rows, as the checkers read them — keyed by hash hex.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SpineLedger {
    pub rows: BTreeMap<String, LedgerRow>,
}

/// One row's ledger facts. A projection of [`TxRow`], not a copy: it carries
/// what the ledger properties are about (the commit point, the per-part
/// landed flags, the created identifiers, the provenance stamp) and nothing
/// the checkers must not reason over.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerRow {
    pub tx: String,
    pub seq: i64,
    pub attempt: u32,
    pub decided: bool,
    pub cleared: bool,
    pub initiator: Initiator,
    pub instrument: Instrument,
    pub parts: Vec<LedgerPart>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerPart {
    pub pool: PoolName,
    pub landed: bool,
    pub created_nodes: BTreeSet<NodeId>,
    pub added_edges: Vec<CensusEdgeRef>,
    /// How many `SetField` deltas this part carries. Counted, never checked —
    /// see [`atomic_or_cleanly_aborted`]'s known limit.
    pub set_fields: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CensusEdgeRef {
    pub src: NodeId,
    pub kind: String,
    pub dst: QualifiedNodeId,
}

impl LedgerRow {
    fn creates_node(&self, pool: &PoolName, id: &NodeId) -> bool {
        self.parts
            .iter()
            .any(|p| &p.pool == pool && p.created_nodes.contains(id))
    }

    fn adds_edge(&self, pool: &PoolName, src: &NodeId, kind: &str, dst: &QualifiedNodeId) -> bool {
        self.parts.iter().any(|p| {
            &p.pool == pool
                && p.added_edges
                    .iter()
                    .any(|e| &e.src == src && e.kind == kind && &e.dst == dst)
        })
    }

    fn effects(&self) -> Vec<Effect> {
        let mut out = Vec::new();
        for part in &self.parts {
            for id in &part.created_nodes {
                out.push(Effect::Node {
                    pool: part.pool.clone(),
                    id: id.clone(),
                });
            }
            for e in &part.added_edges {
                out.push(Effect::Edge {
                    pool: part.pool.clone(),
                    src: e.src.clone(),
                    kind: e.kind.clone(),
                    dst: e.dst.clone(),
                });
            }
        }
        out
    }
}

/// Every attempt at ONE hash identity, in attempt order (one
/// identity, several rows, at most one decided).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttemptHistory {
    pub tx: String,
    pub rows: Vec<LedgerRow>,
}

/// A byte digest of the source corpus: entry name -> SHA-256 hex. Entry names
/// are the caller's — this type carries no notion of a file system layout and
/// no notion of a workspace directory.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SourceDigest {
    pub entries: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// the collectors — IMPURE. They read; they never assert.
// ---------------------------------------------------------------------------

/// IMPURE. Read every pool in the cauldron into a [`DestinationCensus`].
///
/// # Why this opens the pool files itself
///
/// The census needs each node's and edge's `created_tx` — the column that
/// makes one-door provenance answerable at all — and `pool::Snapshot` exposes
/// no accessor for it (`NodeRecord` deliberately carries identity, type,
/// state, revision, the schema stamp and fields, and no provenance). Rather
/// than widen another module's frozen type, this reads the two columns straight
/// out of the schema `pool::POOL_DDL` defines, in a read transaction that is
/// opened and dropped here. It duplicates knowledge of two table names, which
/// is a known weak seam.
pub fn census(cauldron: &Cauldron) -> Result<DestinationCensus> {
    let names: Vec<PoolName> = cauldron.pools().cloned().collect();
    let mut pools = BTreeMap::new();
    for name in names {
        pools.insert(name.clone(), pool_census(&Pool::path_in(&cauldron.root, &name))?);
    }
    // Ghost edges: every projected address some edge points at is
    // resolved against the spine's tables now, and only the ones that name a
    // row are recorded — computed on read, never copied.
    let mut resolved_ghosts = BTreeSet::new();
    let mut wanted: BTreeSet<NodeId> = BTreeSet::new();
    for contents in pools.values() {
        for edge in &contents.edges {
            if crate::project::is_projected(&edge.dst.pool) {
                wanted.insert(edge.dst.id.clone());
            }
        }
    }
    if !wanted.is_empty() {
        let spine = cauldron.spine()?;
        for id in wanted {
            if crate::project::resolve_ghost(&spine, &id)?.is_some() {
                resolved_ghosts.insert(id);
            }
        }
    }
    Ok(DestinationCensus {
        pools,
        resolved_ghosts,
    })
}

fn pool_census(path: &Path) -> Result<PoolCensus> {
    let conn = rusqlite::Connection::open(path)?;
    let mut nodes = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, node_type, state, created_tx, updated_tx FROM nodes ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                NodeId(r.get::<_, String>(0)?),
                CensusNode {
                    node_type: r.get(1)?,
                    state: r.get(2)?,
                    created_tx: r.get(3)?,
                    updated_tx: r.get(4)?,
                },
            ))
        })?;
        for row in rows {
            let (id, node) = row?;
            nodes.insert(id, node);
        }
    }
    let mut edges = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT src, kind, dst_pool, dst, created_tx FROM edges ORDER BY src, kind, dst_pool, dst",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(CensusEdge {
                src: NodeId(r.get::<_, String>(0)?),
                kind: r.get(1)?,
                dst: QualifiedNodeId {
                    pool: PoolName::new(r.get::<_, String>(2)?),
                    id: NodeId(r.get::<_, String>(3)?),
                },
                created_tx: r.get(4)?,
            })
        })?;
        for row in rows {
            edges.push(row?);
        }
    }
    Ok(PoolCensus { nodes, edges })
}

/// IMPURE. Look up every transaction the census names and build the ledger
/// the provenance checker takes.
///
/// A hash the spine does not know simply has no row — the checker's
/// `NodeWithoutSpineRow` finding IS that case, so this collector never
/// invents an empty row to keep a lookup total.
pub fn ledger_for(cauldron: &Cauldron, census: &DestinationCensus) -> Result<SpineLedger> {
    let mut wanted: BTreeSet<String> = BTreeSet::new();
    for contents in census.pools.values() {
        for node in contents.nodes.values() {
            wanted.insert(node.created_tx.clone());
            wanted.insert(node.updated_tx.clone());
        }
        for edge in &contents.edges {
            wanted.insert(edge.created_tx.clone());
        }
    }
    let spine = cauldron.spine()?;
    let mut rows = BTreeMap::new();
    for hex in wanted {
        let Some(hash) = TxHash::from_hex(&hex) else {
            // Not a hash at all. Left OUT of the ledger deliberately, so the
            // provenance checker reports it as a node with no row rather than
            // this collector swallowing it.
            continue;
        };
        if let Some(row) = spine.find_by_hash(&hash)? {
            rows.insert(hex, ledger_row(&row));
        }
    }
    Ok(SpineLedger { rows })
}

/// IMPURE. The whole attempt history of one identity.
pub fn attempt_history(cauldron: &Cauldron, tx: &TxHash) -> Result<AttemptHistory> {
    let spine = cauldron.spine()?;
    Ok(AttemptHistory {
        tx: tx.to_hex(),
        rows: spine.attempts_of(tx)?.iter().map(ledger_row).collect(),
    })
}

fn ledger_row(row: &TxRow) -> LedgerRow {
    use crate::delta::Delta;
    let mut parts = Vec::new();
    for part in &row.payload.parts {
        let landed = row
            .parts
            .iter()
            .find(|p| p.pool == part.pool)
            .is_some_and(|p| p.landed);
        let mut created_nodes = BTreeSet::new();
        let mut added_edges = Vec::new();
        let mut set_fields = 0usize;
        for delta in &part.deltas {
            match delta {
                Delta::CreateNode { id, .. } => {
                    created_nodes.insert(id.clone());
                }
                Delta::AddEdge {
                    src, kind, dst, ..
                } => added_edges.push(CensusEdgeRef {
                    src: src.clone(),
                    kind: kind.clone(),
                    dst: dst.clone(),
                }),
                Delta::SetField { .. } => set_fields += 1,
                // A seal creates nothing and adds no edge; the
                // ledger census counts neither.
                Delta::RemoveEdge { .. } | Delta::Seal { .. } => {}
            }
        }
        parts.push(LedgerPart {
            pool: part.pool.clone(),
            landed,
            created_nodes,
            added_edges,
            set_fields,
        });
    }
    LedgerRow {
        tx: row.hash.to_hex(),
        seq: row.seq.0,
        attempt: row.attempt,
        decided: row.decided,
        cleared: row.cleared,
        initiator: row.initiator.clone(),
        instrument: row.instrument.clone(),
        parts,
    }
}

impl SourceDigest {
    /// IMPURE. Digest a set of source entries the CALLER names: entry label
    /// paired with the path whose bytes are hashed.
    ///
    /// The label is the caller's, not derived from the path, so a corpus that
    /// is not a directory tree (a message stream written to scratch files, for
    /// instance) digests exactly as well as one that is — which is what keeps
    /// this method-indifferent alongside the driver.
    pub fn of_entries<'a, I>(entries: I) -> Result<SourceDigest>
    where
        I: IntoIterator<Item = (&'a str, &'a Path)>,
    {
        use sha2::{Digest, Sha256};
        let mut out = BTreeMap::new();
        for (label, path) in entries {
            let bytes = std::fs::read(path).map_err(|e| {
                SpineError::hard(format!(
                    "cannot digest source entry `{label}` at `{}`: {e}",
                    path.display()
                ))
            })?;
            let mut h = Sha256::new();
            h.update(&bytes);
            let mut hex = String::with_capacity(64);
            for b in h.finalize() {
                use std::fmt::Write;
                let _ = write!(hex, "{b:02x}");
            }
            out.insert(label.to_string(), hex);
        }
        Ok(SourceDigest { entries: out })
    }

    /// IMPURE. Digest every file directly inside `dir`, labelled by file
    /// name. Sorted, and a directory that will not read is a loud failure
    /// rather than an empty digest — an empty digest would make
    /// [`source_untouched`] pass over a source that vanished.
    pub fn of_dir(dir: &Path) -> Result<SourceDigest> {
        // RECURSIVE, deliberately (this walk
        // used to read only the top level, so a nested real corpus — and the
        // two live pools ARE nested, one YAML per node under subdirectories —
        // would have produced a green source-untouched verdict over files the
        // digest never hashed). Labels are root-relative paths, sorted, so
        // the digest is deterministic and a moved file changes it.
        let mut paths: Vec<(String, std::path::PathBuf)> = Vec::new();
        let mut stack: Vec<std::path::PathBuf> = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let entries = std::fs::read_dir(&d).map_err(|e| {
                SpineError::hard(format!("cannot read source dir `{}`: {e}", d.display()))
            })?;
            for entry in entries {
                let entry = entry.map_err(|e| {
                    SpineError::hard(format!("cannot read source dir `{}`: {e}", d.display()))
                })?;
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.is_file() {
                    let label = path
                        .strip_prefix(dir)
                        .map(|r| r.to_string_lossy().into_owned())
                        .map_err(|e| {
                            SpineError::hard(format!(
                                "source path `{}` is not under `{}`: {e}",
                                path.display(),
                                dir.display()
                            ))
                        })?;
                    paths.push((label, path));
                }
            }
        }
        paths.sort();
        SourceDigest::of_entries(paths.iter().map(|(l, p)| (l.as_str(), p.as_path())))
    }
}
