//! The refusal witnesses — every refusal shape, structured and machine-readable
//! (a Rust enum serializable to JSON, one variant per specified witness shape,
//! asserted on structurally — prose cannot be asserted on).
//!
//! Refusal is WHOLE: a statement that fails anywhere
//! lands nothing anywhere and returns ONE
//! [`crate::error::SpineError::Refused`] carrying these.
//!
//! An OPEN QUESTION, left as a loud boundary, not solved here: what a
//! structured witness IS when the failed thing is an ARBITRARY pattern is
//! not designed. It sits outside the staged coverage; needing it inside the
//! staged forms is a design change, not a new variant — no variant below is
//! that shape, deliberately.

use crate::delta::CountOp;
use crate::locks::Claim;
use crate::schema_def::PoolName;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One structured refusal witness. Serializes with a `kind`
/// tag so suites match on the variant by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Witness {
    /// A failed EXPECT: the expectation, the expected value, and the found
    /// value (the empty-match-under-count case
    /// arrives here too). An empty match WITHOUT a
    /// count is a success, not a no-op error: the code-only no-op principle
    /// binds code that is invoked and does nothing while reporting success,
    /// and leaves "a pass over data that legitimately has nothing to act on"
    /// outside its scope.
    FailedExpectation {
        expectation: ExpectationKind,
        expected: Value,
        found: Value,
    },
    /// A failed WHERE premise: the failing condition (its source text) and
    /// the binding it failed on.
    FailedCondition { condition: String, binding: String },
    /// A grammatically valid but staged-out clause form, refused BY NAME —
    /// never a silent misread, never a partial attempt. `form` is the
    /// frozen form name from [`crate::ladle::compile`]'s coverage table.
    UnimplementedForm { form: String },
    /// An envelope format version this build does not speak — both versions
    /// named.
    VersionMismatch {
        envelope_version: u32,
        supported_version: u32,
    },
    /// The restart bound exhausted under contention: the contested claims
    /// (the bounded restart loop).
    StaleMatch { contested: Vec<Claim> },
    /// A part violated the touched pool's OWN pinned grammar — names the
    /// pool and the part, so the poisoned-grammar variants can assert that
    /// validation is genuinely per-pool, never one merged vocabulary.
    GrammarViolation {
        pool: PoolName,
        /// The part ordinal within the compiled payload, when known
        /// (validation before compilation may have no part yet).
        part: Option<u32>,
        message: String,
    },
    /// A write addressed to the reserved projected namespace — the pen
    /// refuses: the projection's origin is code.
    ProjectedNamespaceWrite { pool: PoolName, target: String },
    /// An edge minted toward a ghost identifier that names no transaction
    /// row (refused — the target is the address, not a copy).
    GhostTargetUnknown { target: String },
    /// The statement text did not parse. Not one of the enumerated shapes;
    /// an addition under the general rule (refusals are structured
    /// and name why).
    MalformedStatement { message: String, offset: usize },
    /// The text references `$name` and the bindings do not supply it.
    /// An addition, as above. (It used to carry the OPPOSITE
    /// direction too — a supplied-but-unused binding — under this one name,
    /// so a witness said "missing" about a name the caller had supplied, at
    /// both doors.)
    MissingParameter { name: String },
    /// The bindings supply `$name` and the text never references it — the
    /// other direction of "neither may be silently ignored",
    /// with its own name because its repair is the opposite of
    /// [`Witness::MissingParameter`]'s (under the general
    /// rule: a refusal names WHY).
    UnusedParameter { name: String },
    /// A value expectation over a field the node does NOT carry. Distinct
    /// from [`Witness::FailedExpectation`] with `found: null`, which is a
    /// field that EXISTS and holds JSON `null` (a legal SET value in this
    /// build) — the two repairs differ, so the two witnesses do.
    AbsentField {
        expectation: ExpectationKind,
        expected: Value,
    },
    /// A field written OVER its declared character cap (the cap
    /// law: every cap is HARD by one character and every
    /// cap refusal names measured, cap and overage so the writer knows how
    /// much to cut). Characters, never bytes. `overage` is always
    /// `measured - cap` and always >= 1 — a structured number a suite asserts
    /// by value, never prose it has to parse.
    OverCap {
        pool: PoolName,
        part: Option<u32>,
        node_type: String,
        field: String,
        measured: u64,
        cap: u64,
        overage: u64,
    },
    /// A field written UNDER its declared character floor — the other half
    /// of the same law (a boundary of fewer than 40 characters, an `admits`
    /// span shorter than 20): measured, floor and shortfall, by value.
    UnderFloor {
        pool: PoolName,
        part: Option<u32>,
        node_type: String,
        field: String,
        measured: u64,
        floor: u64,
        shortfall: u64,
    },
    /// A node minted without an edge its type requires AT MINT (e.g. a
    /// milestone without `serves`, a node without `in_region`): the edge
    /// kind, the minimum the grammar demands and how many the same part
    /// actually carries leaving the minted node. Judged over the part being
    /// written and nothing else (a rule that reads only the
    /// part is grammar; one that reads a second node is a constraint).
    RequiredEdgeMissing {
        pool: PoolName,
        part: Option<u32>,
        node_type: String,
        edge_kind: String,
        minimum: u32,
        found: u32,
    },
    /// An EDGE field written over its declared character cap (the
    /// same cap law as [`Witness::OverCap`], owned by an edge kind instead
    /// of a node type — a separate variant so a suite cannot mistake an
    /// edge kind for a node type by reading one string field).
    EdgeFieldOverCap {
        pool: PoolName,
        part: Option<u32>,
        edge_kind: String,
        field: String,
        measured: u64,
        cap: u64,
        overage: u64,
    },
    /// An EDGE field written under its declared character floor.
    EdgeFieldUnderFloor {
        pool: PoolName,
        part: Option<u32>,
        edge_kind: String,
        field: String,
        measured: u64,
        floor: u64,
        shortfall: u64,
    },
    /// A write that sets a FLAG field its initiator may not set: a SET whose path is a flag, a CREATE whose fields
    /// carry one, or a SEAL — from a type-one ripple (always), or a type-two
    /// ripple whose asserted `scope` does not list the field. Refused whole,
    /// like every witness; nothing lands. `field` names the flag
    /// (`meta.blocking`, `retired`, `sealed`, …); `initiator_kind` is
    /// `type_one` | `type_two`; `scope` is the write's asserted list.
    FlagNotWritable {
        pool: PoolName,
        part: Option<u32>,
        node: String,
        field: String,
        initiator_kind: String,
        scope: Option<Vec<String>>,
    },
    /// An `IMPORT` statement (the carried-id form) from an initiator
    /// that is not `Agent` (the import's initiator is `Agent` — the
    /// only writer that holds a v5 pool). Refused whole, before any pool is
    /// opened, on both modes. `initiator_kind` is `core_mechanical` |
    /// `type_one` | `type_two` | `external`.
    ImportNotPermitted { initiator_kind: String },
    /// An `IMPORT` statement that also edits a row it did not import — a
    /// `SET`, `SEAL` or `REMOVE` clause beside an `IMPORT` clause. The soup
    /// tags a commit carrying any IMPORT `import` WHOLE and fires no trigger
    /// on it, so such an edit would escape every trigger. Refused
    /// whole, before any pool is opened, on both modes. `CREATE` beside
    /// `IMPORT` is admitted: the v5 regeneration (`migrate::run`) lands a
    /// pool as ONE statement whose store-minted viewer roles and edges are
    /// `CREATE` clauses. `form` is the first offending clause's frozen form
    /// name: `set:field` | `seal:node` | `remove:edge`.
    ImportMixedWithEdit { form: String },
    /// An act under a key whose tranche attempt has ENDED: which end and when. `state` is `landed` |
    /// `rolled-back` | `failed` | `abandoned`; `reason` is `holder` |
    /// `heartbeat_lost` | `landing_refused` | `landed`. A heartbeat after the
    /// scheduler's auto-rollback is refused with this, `state: abandoned,
    /// reason: heartbeat_lost`. Times are milliseconds since the
    /// Unix epoch on the daemon's clock.
    TrancheTerminal {
        tranche: String,
        attempt: u32,
        state: String,
        reason: Option<String>,
        began_at: i64,
        ended_at: Option<i64>,
        last_heartbeat: i64,
        deadline: i64,
    },
    /// The act needs the holder's right to roll back, and the tranche is in
    /// `landing`: past the point of no return.
    TranchePastRollback { tranche: String, attempt: u32 },
    /// The `max`-th child: the tranche holds as many children as this build
    /// is sized for (`SOUP_TRANCHE_MAX_CHILDREN`).
    TrancheFull {
        tranche: String,
        attempt: u32,
        max: u32,
    },
    /// The landing was refused before the decision: which
    /// child, why (`revalidation` | `child_landed_elsewhere`), the child's
    /// own witnesses, and the colliding row's `seq` when another row holds
    /// the child's hash. The tranche is `failed`.
    TrancheLandingRefused {
        tranche: String,
        attempt: u32,
        ordinal: u32,
        child_tx: String,
        why: String,
        witnesses: Vec<Witness>,
        colliding_seq: Option<i64>,
    },
}

/// Which expectation failed (the EXPECT parameters: revision,
/// state/value, count — the count form is our addition).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "of", rename_all = "snake_case")]
pub enum ExpectationKind {
    /// A revision expectation: whole-node when `field` is `None`, per-field
    /// otherwise (same per-field revision bookkeeping the store keeps).
    Revision {
        binding: String,
        field: Option<String>,
    },
    /// A state-or-value expectation on a field path (dotted keys address
    /// into the metadata object; `["state"]` is the state form).
    Value { binding: String, path: Vec<String> },
    /// A count expectation over what a binding matched.
    Count { binding: String, op: CountOp },
}
