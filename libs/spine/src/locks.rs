//! Locking — claims, modes, resources, acquisition in sequence order,
//! release at the cleared flip (locks are TABLES in the spine database —
//! durable, cross-process, no side files; waiting is bounded polling with
//! backoff).
//!
//! The read/write semaphore shape is the SETTLED core; the three-kind
//! resource taxonomy, link and upgrade-intent modes, the sequence-number
//! queue order (ordered by sequence number so every wait points at an older
//! transaction and no cycle can form), restart-not-upgrade, and
//! mechanical-writes-wait are the delivery mechanism around it.
//!
//! # The grant discipline (interior mechanism)
//!
//! [`acquire`] enrols EVERY claim row first, then grants ALL of them in one
//! atomic step or NONE (and polls). All-or-nothing is what makes the
//! deadlock argument hold by construction here: a waiting transaction holds
//! no granted lock at all, so no cycle of holders-waiting-on-holders can
//! form. Within that discipline the queue order falls out of one rule:
//! a claim is blocked by a conflicting row that is GRANTED (physically held,
//! whatever its number) or QUEUED WITH AN EARLIER NUMBER — so an
//! earlier-numbered reader passes a queued later-numbered writer, and every
//! later claimant waits behind every earlier conflicting one. The one
//! deliberate residual: an upgrade-intent CONVERSION waits while holding
//! (that is the mode's definition — reads drain, then it writes); its
//! at-most-one rule keeps two converters off one resource, and the wait
//! ceiling bounds the crosswise-conversion case loudly.
//!
//! # Known residuals, named not hidden
//!
//! There are two:
//! **starvation under restart** — a transaction restarted
//! repeatedly under constant contention refreshes its sequence number each
//! time (the restart rule) and can starve behind an unbroken
//! stream of younger-but-earlier-arriving claims; the escape (the arbiter
//! pinning its original number and restarting younger holders) is a
//! measured-need option, deliberately NOT built. And **the spine file
//! itself** — the one storage-layer serialization point, first number to
//! measure; its sharding escape is deliberately undesigned (named in
//! `spine_db.rs` too). Neither escape is built until a measurement shows the
//! need.

use crate::config::SpineConfig;
use crate::error::{Result, SpineError};
use crate::pool::{NodeId, QualifiedNodeId};
use crate::schema_def::{Direction, PoolName};
use crate::spine_db::{Seq, SpineDb};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::time::Instant;

/// The lock tables' DDL, executed into `spine.db` at cauldron create.
/// One queue row per (resource, claimant); grant
/// order is spine sequence number.
///
/// `resource` is the canonical text key of [`resource_key`]. Rows are
/// DELETED at release (the cleared flip) — unlike transaction rows, lock
/// rows are queue state, not history.
pub const LOCKS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS locks (
  pool     TEXT NOT NULL,
  resource TEXT NOT NULL,
  mode     TEXT NOT NULL CHECK (mode IN ('read','write','link','upgrade-intent')),
  seq      INTEGER NOT NULL,
  granted  INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (pool, resource, seq, mode)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_locks_seq ON locks(seq);
"#;

/// The four lock modes. `Read` and `Write` are SETTLED (the semaphore
/// shape); `Link` and `UpgradeIntent` are delivery mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LockMode {
    /// Shared, unlimited count; while held the resource cannot be written.
    Read,
    /// Exclusive.
    Write,
    /// Slices only: shared among membership-changers; conflicts with read
    /// and write on the same slice. What makes the read/connect carve-out a
    /// theorem instead of an exception.
    Link,
    /// At most one per resource; compatible with readers; convertible to
    /// write when reads drain. Exists for the condition-reads-then-handler-
    /// writes pattern; cheap now, retrofit is not (triggers not in build).
    UpgradeIntent,
}

impl LockMode {
    /// Every mode, in [`COMPATIBILITY`]'s axis order. FROZEN: the matrix
    /// indexes by position here.
    pub const ALL: [LockMode; 4] = [
        LockMode::Read,
        LockMode::Write,
        LockMode::Link,
        LockMode::UpgradeIntent,
    ];

    /// This mode's row/column index in [`COMPATIBILITY`].
    pub fn index(self) -> usize {
        match self {
            LockMode::Read => 0,
            LockMode::Write => 1,
            LockMode::Link => 2,
            LockMode::UpgradeIntent => 3,
        }
    }

    /// The frozen text form used in the lock table.
    pub fn as_str(&self) -> &'static str {
        match self {
            LockMode::Read => "read",
            LockMode::Write => "write",
            LockMode::Link => "link",
            LockMode::UpgradeIntent => "upgrade-intent",
        }
    }

    /// Parse the frozen text form back; `None` for anything else — a mode
    /// that will not parse is never silently defaulted.
    pub fn from_str(s: &str) -> Option<LockMode> {
        match s {
            "read" => Some(LockMode::Read),
            "write" => Some(LockMode::Write),
            "link" => Some(LockMode::Link),
            "upgrade-intent" => Some(LockMode::UpgradeIntent),
            _ => None,
        }
    }
}

/// The lock compatibility matrix — AS DATA, deliberately (one table, one
/// red-able test, never scattered if-arms). `COMPATIBILITY[held.index()][requested.index()]`
/// is `true` when the two coexist on one resource. The matrix is SYMMETRIC
/// (every clause is a mutual relation) and the unit tests assert both
/// the cells and the symmetry.
///
/// Each cell's clause:
/// - read×read `true` — "a read lock is a semaphore with unlimited count"
///   (SETTLED).
/// - write×anything `false` — "a write lock is exclusive" (SETTLED).
/// - link×link `true` — "shared among membership-changers": two
///   edge-connects commute.
/// - link×read, link×write `false` — "conflicts with read and write on the
///   same slice".
/// - upgrade-intent×read `true` — "compatible with readers".
/// - upgrade-intent×upgrade-intent `false` — "at most one per resource".
/// - upgrade-intent×link `false` — INTERIOR CHOICE, the one pair the rules
///   never name: upgrade-intent spans read-becoming-write and link conflicts
///   with both, so the conservative cell is conflict.
pub const COMPATIBILITY: [[bool; 4]; 4] = [
    //                     read   write  link   upgrade-intent
    /* read           */ [true, false, false, true],
    /* write          */ [false, false, false, false],
    /* link           */ [false, false, true, false],
    /* upgrade-intent */ [true, false, false, false],
];

/// Whether two modes coexist on one resource — a data lookup in
/// [`COMPATIBILITY`], never an if-arm.
pub fn compatible(held: LockMode, requested: LockMode) -> bool {
    COMPATIBILITY[held.index()][requested.index()]
}

/// THE membership-change rule, in one place: a membership
/// change on an ordinary slice takes `link` — two edge-connects commute, so
/// they need no mutual exclusion, and that is what makes the
/// read-and-connect carve-out a theorem instead of an exception. **A slice
/// constrained to at most one edge is a SCALAR**: link is FORBIDDEN on it
/// and every membership change on it is a slice WRITE, so reassignment
/// serializes against other reassigns and waits politely behind readers of
/// the dying edge.
///
/// `scalar` comes from the pool's grammar
/// ([`crate::schema_def::PoolGrammar::is_scalar_slice`]) — the grammar marks
/// them; this module never guesses.
pub fn membership_mode(scalar: bool) -> LockMode {
    if scalar {
        LockMode::Write
    } else {
        LockMode::Link
    }
}

/// Is this resource a SLICE (the only thing `link` may be taken on)? The
/// state slice is one: a state change is a
/// membership change on it, and two foreign state changes of different
/// nodes commute, so they `link` and share it exactly as creates share the
/// type slice.
fn is_slice(r: &Resource) -> bool {
    matches!(
        r,
        Resource::EdgeSlice { .. } | Resource::TypeSlice { .. } | Resource::StateSlice { .. }
    )
}

/// The grammar-free half of link legality, enforced by [`acquire`] itself:
/// **link is a SLICE mode** ("link — slices only"). A link claim
/// on a node body or on a single edge record is a caller defect, refused
/// before any row is enrolled.
fn check_link_is_slice_only(claims: &ClaimSet) -> Result<()> {
    for c in claims.iter() {
        if c.mode == LockMode::Link && !is_slice(&c.resource) {
            return Err(SpineError::usage(format!(
                "link mode is slices only; refused on {}/{}",
                c.pool,
                resource_key(&c.resource)
            )));
        }
    }
    Ok(())
}

/// The full link-legality check, INCLUDING the grammar-dependent half: link
/// is forbidden on an at-most-one (scalar) slice — every membership change
/// on such a slice is a slice WRITE.
///
/// It is a separate call rather than part of [`acquire`] because the frozen
/// `acquire` signature carries no grammar, and this module refuses to guess
/// which slices are scalar — the grammar marks them. The door calls this
/// with its own grammar lookup before acquiring; the claim derivation
/// (`ladle::compile::claims_for`) already emits WRITE for scalar slices, so
/// this is the net UNDER that derivation, not a substitute for it. Both are
/// exercised in `tests/locks_unit.rs`.
///
/// `scalar` is asked only about edge slices: `(pool, edge kind, direction)`.
pub fn check_link_claims(
    claims: &ClaimSet,
    scalar: &dyn Fn(&PoolName, &str, Direction) -> bool,
) -> Result<()> {
    check_link_is_slice_only(claims)?;
    for c in claims.iter() {
        if c.mode != LockMode::Link {
            continue;
        }
        if let Resource::EdgeSlice {
            kind, direction, ..
        } = &c.resource
        {
            if scalar(&c.pool, kind, *direction) {
                return Err(SpineError::usage(format!(
                    "link mode is forbidden on the at-most-one slice {}/{}: a scalar slice's \
                     membership change is a slice WRITE",
                    c.pool,
                    resource_key(&c.resource)
                )));
            }
        }
    }
    Ok(())
}

/// The lockable things — three kinds, the slice kind split into its
/// shapes. Whole-node grain for bodies, never a single field. The STATE
/// SLICE is the fourth shape: the same-type
/// nodes of one pool in one lifecycle state — the population a filtered scan
/// (`MATCH (b: pool/type { state: s })`) reads, and the one resource that
/// holds still "which of the population are in the scanned state" between a
/// tranche's staging and its landing. A filtered scan
/// under a tranche takes `read` on it; every writer, under a tranche or not,
/// takes `link` on the slice a node leaves and the slice it enters
/// (`ladle::compile::claims_for`). Nothing in the lock table changes for it:
/// a resource is text, and this one renders `sslice:<type>:<state>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "resource", rename_all = "snake_case")]
pub enum Resource {
    /// A node's fields plus its existence.
    Body { node: NodeId },
    /// One edge, as its own record — identified by its whole triple.
    Edge {
        src: NodeId,
        kind: String,
        dst: QualifiedNodeId,
    },
    /// The set of same-kind edges at one node in one direction.
    EdgeSlice {
        node: NodeId,
        kind: String,
        direction: Direction,
    },
    /// The set of same-type nodes in a pool (what a create link-locks and
    /// an absence watch reads).
    TypeSlice { node_type: String },
    /// The set of same-type nodes in ONE lifecycle state in a pool — what a
    /// filtered scan under a tranche reads and a state change (or a create
    /// with an initial state) link-locks. Its
    /// physical shape is the pool's `idx_nodes_type_state` index.
    StateSlice { node_type: String, state: String },
}

/// The canonical text key a resource occupies in the lock table. FROZEN so
/// every caller computes the same key:
/// `body:<id>` | `edge:<src>:<kind>:<dst_pool>/<dst>` |
/// `eslice:<id>:<kind>:<out|in>` | `tslice:<type>` | `sslice:<type>:<state>`
/// (the type and the state are each a declared grammar name, so neither
/// carries `:`).
pub fn resource_key(r: &Resource) -> String {
    match r {
        Resource::Body { node } => format!("body:{}", node.0),
        Resource::Edge { src, kind, dst } => {
            format!("edge:{}:{}:{}/{}", src.0, kind, dst.pool, dst.id.0)
        }
        Resource::EdgeSlice {
            node,
            kind,
            direction,
        } => {
            let d = match direction {
                Direction::Out => "out",
                Direction::In => "in",
            };
            format!("eslice:{}:{}:{}", node.0, kind, d)
        }
        Resource::TypeSlice { node_type } => format!("tslice:{node_type}"),
        Resource::StateSlice { node_type, state } => format!("sslice:{node_type}:{state}"),
    }
}

/// One pool-qualified claim (every lock claim is pool identity plus
/// resource identity — the one naming rule that carries
/// queues, numbers, restart and the deadlock argument to the cross-pool
/// case verbatim).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Claim {
    pub pool: PoolName,
    pub resource: Resource,
    pub mode: LockMode,
}

/// A transaction's complete claim set — complete AT OPEN, by law: a
/// transaction that must grow it RESTARTS with a fresh sequence number
/// (no in-place upgrade). A set, so the claim
/// derivation dedupes by construction.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimSet(pub BTreeSet<Claim>);

impl ClaimSet {
    pub fn new() -> ClaimSet {
        ClaimSet(BTreeSet::new())
    }
    pub fn insert(&mut self, claim: Claim) {
        self.0.insert(claim);
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = &Claim> {
        self.0.iter()
    }
}

/// Proof-of-grant marker handed back by [`acquire`]; consumed by the door.
/// Deliberately NOT a guard that releases on drop: locks release only at
/// the cleared flip, by an explicit
/// [`release_at_clear`] the commit path calls — never per-pool as parts
/// land, and never implicitly.
#[derive(Debug)]
pub struct Granted {
    pub seq: Seq,
}

/// Why a transaction must restart with a fresh sequence number rather than
/// upgrade in place (the restart signal types).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "restart", rename_all = "snake_case")]
pub enum RestartSignal {
    /// Mid-flight discovery that the claim set must grow (a write to
    /// something only read): restart with the larger set.
    ClaimGrowth { needed: ClaimSet },
    /// The data moved between snapshot and grant; premises no longer hold
    /// under the granted locks: restart and re-match.
    MovedUnderfoot { contested: Vec<Claim> },
}

/// Begin an IMMEDIATE transaction on a borrowed connection.
///
/// Unchecked because [`acquire`] holds `&SpineDb` (no transaction handle
/// escapes the crate either way, shared-contract idiom), and IMMEDIATE
/// because every transaction in this module reads the lock table and then
/// writes it — see the note in [`acquire`].
fn immediate(conn: &rusqlite::Connection) -> Result<rusqlite::Transaction<'_>> {
    Ok(rusqlite::Transaction::new_unchecked(
        conn,
        rusqlite::TransactionBehavior::Immediate,
    )?)
}

/// One claim as [`acquire`] tracks it through enrolment and grant.
struct Wanted {
    pool: String,
    key: String,
    mode: LockMode,
    /// A write claim standing where the same seq holds upgrade-intent: on
    /// grant, the upgrade-intent row is consumed — "convertible to write
    /// when reads drain".
    conversion: bool,
}

/// One row of a resource's queue, oldest first — read-only observability
/// over the lock table. A SURFACE ADDITION beyond the frozen contract: the
/// contention tests need to SEE a
/// queued claimant without racing it, and the lock table is otherwise
/// interior to this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueEntry {
    pub seq: Seq,
    pub mode: LockMode,
    pub granted: bool,
}

/// Read one resource's queue, in sequence order. See [`QueueEntry`].
pub fn queue(spine: &SpineDb, pool: &PoolName, resource: &Resource) -> Result<Vec<QueueEntry>> {
    let conn = spine.conn();
    let mut stmt = conn.prepare(
        "SELECT seq, mode, granted FROM locks
         WHERE pool = ?1 AND resource = ?2 ORDER BY seq, mode",
    )?;
    let rows = stmt.query_map(params![pool.as_str(), resource_key(resource)], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (seq, mode, granted) = row?;
        let mode = LockMode::from_str(&mode).ok_or_else(|| {
            SpineError::hard(format!("lock table carries an unparseable mode: {mode}"))
        })?;
        out.push(QueueEntry {
            seq: Seq(seq),
            mode,
            granted: granted != 0,
        });
    }
    Ok(out)
}

/// Acquire every claim in `claims` for the transaction holding `seq`,
/// granted strictly in spine-sequence order per resource queue (an
/// EARLIER-numbered reader proceeds
/// ahead of a queued writer; every later reader waits behind it). Waiting
/// is bounded polling with backoff under `config.lock_poll_interval` /
/// `config.lock_wait_ceiling`. Mechanical writes wait their turn like
/// everyone else; they are never refused, only late.
///
/// Contracts delivered here, each tested in `tests/locks_unit.rs`:
/// - **All-or-nothing grant** (module docs): a waiter holds nothing, so the
///   deadlock argument holds by construction.
/// - **A single holder is never refused its own grant**: a seq's own rows
///   never conflict with each other, and re-acquiring an already-granted
///   set returns `Granted` immediately.
/// - **No growth**: claims are complete at open. A call for a seq that
///   already holds rows, asking for anything beyond that set, refuses with
///   [`SpineError::Usage`] carrying the serialized
///   [`RestartSignal::ClaimGrowth`] — the fresh sequence number is the
///   caller's job. (A first-class error variant would need `error.rs`;
///   not added.)
/// - **Upgrade-intent conversion is NOT growth**: a write claim on a
///   resource where the same seq holds granted upgrade-intent converts —
///   it grants when reads drain, consuming the upgrade-intent row.
/// - **The wait ceiling is loud**: exceeding it withdraws this call's
///   queued rows (an abandoned claim must not dam the queue) and returns
///   [`SpineError::Busy`] NAMING every blocking holder — never a silent
///   hang. (`config.rs` phrases the ceiling as "a refusal with the
///   contested claims"; `Busy` names the holders, which the refusal
///   witness cannot — the divergence is deliberate.)
///
/// VISIBILITY (the one-door law). The lock table is a second
/// write path if a consumer can reach it: a production crate that could
/// enrol rows for a sequence number the spine never issued would dam every
/// door on that resource until the ceiling, and no cleared row would ever
/// own the orphan. So this
/// is `pub` ONLY under `test-support` — the crate's own suites stage lock
/// states with it — and `pub(crate)` for every consumer, exactly as
/// `Pool::apply_part` and the row-lifecycle flips are gated. Behavior is
/// identical in both builds; the contract docs live on the interior.
#[cfg(feature = "test-support")]
pub fn acquire(
    spine: &SpineDb,
    seq: Seq,
    claims: &ClaimSet,
    config: &SpineConfig,
) -> Result<Granted> {
    acquire_interior(spine, seq, claims, config)
}
#[cfg(not(feature = "test-support"))]
pub(crate) fn acquire(
    spine: &SpineDb,
    seq: Seq,
    claims: &ClaimSet,
    config: &SpineConfig,
) -> Result<Granted> {
    acquire_interior(spine, seq, claims, config)
}

/// CHECKPOINT 3 of the tranche gate, at every
/// enrolment and inside every grant poll, for a claim set holding any mode
/// other than `read`: while a `whole` tranche holds the gate, no claimant
/// but the holder itself is enrolled or granted — `busy tranche_busy /
/// locked`, and the caller withdraws this call's queued rows. Read inside
/// the caller's own immediate transaction, so it serialises with the gate's
/// rise. The exemption is by the HOLDER'S `seq` and by nothing else — the
/// tranche's own children and reads under key (`acquire_for_tranche`) run
/// the same check and pass because their `seq` is the anchor's; there is
/// no second code path and no "am I a tranche" flag. A
/// read-only set (an escalated read) passes: readers are never
/// blocked. `upgrade-intent` counts as non-`read`.
fn gate_check(tx: &rusqlite::Connection, seq: Seq, wanted: &[Wanted]) -> Result<()> {
    if wanted.iter().all(|w| w.mode == LockMode::Read) {
        return Ok(());
    }
    if let Some(holder) = crate::tranche::gate_up(tx)? {
        if holder.seq != seq.0 {
            return Err(crate::tranche::locked_busy(&holder));
        }
    }
    Ok(())
}

fn acquire_interior(
    spine: &SpineDb,
    seq: Seq,
    claims: &ClaimSet,
    config: &SpineConfig,
) -> Result<Granted> {
    if claims.is_empty() {
        return Ok(Granted { seq });
    }
    // Link is a slice mode, always. The at-most-one half of
    // link legality needs the grammar and lives in [`check_link_claims`],
    // which the door calls; this half needs nothing and is enforced here so
    // no path reaches the table with a body/edge link claim.
    check_link_is_slice_only(claims)?;
    let conn = spine.conn();

    // ---- enrolment: classify against rows this seq already holds, refuse
    //      growth BEFORE touching the table, then insert every new row ----
    //
    // IMMEDIATE, not deferred: every transaction in this function reads the
    // lock table and then writes it. A deferred transaction takes its read
    // snapshot first, and SQLite refuses the later upgrade with an immediate
    // SQLITE_BUSY (the busy handler is NOT invoked for a snapshot conflict),
    // so under exactly the concurrency this module exists to arbitrate, a
    // deferred grant loop would fail hard instead of waiting. IMMEDIATE
    // takes the write lock up front, where `busy_timeout` applies.
    let tx = immediate(conn)?;
    let existing: BTreeSet<(String, String, String)> = {
        let mut stmt =
            tx.prepare("SELECT pool, resource, mode FROM locks WHERE seq = ?1")?;
        let rows = stmt.query_map(params![seq.0], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let mut wanted: Vec<Wanted> = Vec::new();
    let mut growth = ClaimSet::new();
    for claim in claims.iter() {
        let pool = claim.pool.as_str().to_string();
        let key = resource_key(&claim.resource);
        let mode = claim.mode;
        let held_already =
            existing.contains(&(pool.clone(), key.clone(), mode.as_str().to_string()));
        let conversion = !held_already
            && mode == LockMode::Write
            && existing.contains(&(
                pool.clone(),
                key.clone(),
                LockMode::UpgradeIntent.as_str().to_string(),
            ));
        if !held_already && !conversion && !existing.is_empty() {
            growth.insert(claim.clone());
            continue;
        }
        wanted.push(Wanted {
            pool,
            key,
            mode,
            conversion,
        });
    }
    if !growth.is_empty() {
        drop(tx);
        let signal = RestartSignal::ClaimGrowth { needed: growth };
        let signal_json = serde_json::to_string(&signal)
            .map_err(|e| SpineError::hard(format!("serialization failure: {e}")))?;
        return Err(SpineError::usage(format!(
            "acquire refuses claim growth for seq {seq}: claims are complete at open; \
             restart with a fresh sequence number (no in-place upgrade). \
             restart signal: {signal_json}"
        )));
    }
    // Checkpoint 3 at enrolment: before any row is inserted.
    gate_check(&tx, seq, &wanted)?;
    for w in &wanted {
        tx.execute(
            "INSERT OR IGNORE INTO locks (pool, resource, mode, seq, granted)
             VALUES (?1, ?2, ?3, ?4, 0)",
            params![w.pool, w.key, w.mode.as_str(), seq.0],
        )?;
    }
    tx.commit()?;

    grant_loop(conn, seq, &wanted, config, None)?;
    Ok(Granted { seq })
}

/// THE TRANCHE'S CLAIMANT — what `acquire_for_tranche`
/// needs beside the claims: the act's clock, so every poll of the wait can
/// refresh the heartbeat deadline (an act in progress is proof of
/// life). `now_ms` is the daemon clock the act was called with;
/// the poll's clock is that plus the wall time elapsed since.
pub(crate) struct TrancheClaimant {
    pub now_ms: i64,
    pub timeout_ms: i64,
    pub started: Instant,
}

impl TrancheClaimant {
    fn now(&self) -> i64 {
        let elapsed = i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX);
        self.now_ms.saturating_add(elapsed)
    }
}

/// ACQUIRE FOR A TRANCHE — claim GROWTH under
/// one sequence number, admitted ONLY for a `seq` the `tranches` table names
/// as non-terminal, under the three rules that keep it sound:
///
/// 1. **Enrolment classifies against the tranche's GRANTED rows only.** A
///    claim already granted under `seq` is held and skipped; anything else
///    is enrolled — a QUEUED row of an earlier call never counts as held,
///    or a second call would believe it holds what is only queued.
/// 2. **The state check runs inside every enrolment and every grant**, in
///    the same IMMEDIATE transaction (`crate::tranche::claim_state_check`):
///    anything but `open` or `staging` inserts nothing, withdraws this
///    call's queued rows, and answers `refused tranche_past_rollback`
///    (`landing`) or `tranche_terminal`. A `seq` no `tranches` row names is
///    refused `Hard` — growth is confined to a tranche's number, and
///    [`acquire`] still refuses it for every other `seq`.
/// 3. **One claim call at a time per tranche** — the per-tranche mutex is
///    the CALLER's; this function does not take it.
///
/// Everything else is [`acquire`]'s: enrolment at the tranche's number,
/// blocked by a granted conflicting row or an earlier-numbered queued one,
/// bounded wait, loud `Busy` naming the holders with the queued rows
/// withdrawn. Release is at the tranche's end (`release_into_store`,
/// the tranche's clearing), never here. Returns the claims THIS CALL newly
/// enrolled, so a caller refused after the grant can release
/// exactly those.
pub(crate) fn acquire_for_tranche(
    spine: &SpineDb,
    seq: Seq,
    claims: &ClaimSet,
    config: &SpineConfig,
    claimant: &TrancheClaimant,
) -> Result<Vec<Claim>> {
    if claims.is_empty() {
        return Ok(Vec::new());
    }
    check_link_is_slice_only(claims)?;
    let conn = spine.conn();
    let tx = immediate(conn)?;
    // Rule 2, at enrolment: nothing is inserted unless the tranche is open.
    crate::tranche::claim_state_check(&tx, seq)?;
    // Rule 1: the GRANTED rows are the held set.
    let granted: BTreeSet<(String, String, String)> = {
        let mut stmt =
            tx.prepare("SELECT pool, resource, mode FROM locks WHERE seq = ?1 AND granted = 1")?;
        let rows = stmt.query_map(params![seq.0], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let mut wanted: Vec<Wanted> = Vec::new();
    let mut fresh: Vec<Claim> = Vec::new();
    for claim in claims.iter() {
        let pool = claim.pool.as_str().to_string();
        let key = resource_key(&claim.resource);
        let mode = claim.mode;
        if granted.contains(&(pool.clone(), key.clone(), mode.as_str().to_string())) {
            continue;
        }
        let conversion = mode == LockMode::Write
            && granted.contains(&(
                pool.clone(),
                key.clone(),
                LockMode::UpgradeIntent.as_str().to_string(),
            ));
        wanted.push(Wanted {
            pool,
            key,
            mode,
            conversion,
        });
        fresh.push(claim.clone());
    }
    if wanted.is_empty() {
        drop(tx);
        return Ok(fresh);
    }
    // Checkpoint 3 at enrolment, the SAME check an ordinary claimant runs:
    // the tranche's own `seq` is the holder's and passes by that alone.
    gate_check(&tx, seq, &wanted)?;
    for w in &wanted {
        tx.execute(
            "INSERT OR IGNORE INTO locks (pool, resource, mode, seq, granted)
             VALUES (?1, ?2, ?3, ?4, 0)",
            params![w.pool, w.key, w.mode.as_str(), seq.0],
        )?;
    }
    tx.commit()?;

    grant_loop(conn, seq, &wanted, config, Some(claimant))?;
    Ok(fresh)
}

/// Release exactly these claims under `seq` — the rows a tranche's call
/// enrolled and was then refused after the grant (release the rows this
/// call granted). Never the tranche's earlier rows.
pub(crate) fn release_claims(spine: &SpineDb, seq: Seq, claims: &[Claim]) -> Result<()> {
    let conn = spine.conn();
    let tx = immediate(conn)?;
    for c in claims {
        tx.execute(
            "DELETE FROM locks WHERE pool = ?1 AND resource = ?2 AND seq = ?3 AND mode = ?4",
            params![c.pool.as_str(), resource_key(&c.resource), seq.0, c.mode.as_str()],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// The grant loop both acquire paths share: all-or-nothing, bounded polling
/// with backoff. `tranche` is `Some` for a tranche's claimant, whose every
/// poll re-checks the state (rule 2) and refreshes the deadline.
fn grant_loop(
    conn: &rusqlite::Connection,
    seq: Seq,
    wanted: &[Wanted],
    config: &SpineConfig,
    tranche: Option<&TrancheClaimant>,
) -> Result<()> {
    let started = Instant::now();
    let mut interval = config.lock_poll_interval;
    let max_interval = config.lock_poll_interval * 32;
    loop {
        // ONE POLL. A storage failure inside it (the spine file busy past
        // its timeout, say) used to return straight out of this function
        // with this call's QUEUED rows left in the table — withdrawal ran
        // only on the ceiling path — so an abandoned claim dammed every
        // later conflicting claimant. Now ANY failed poll withdraws the
        // enrolment, best effort, before the error goes back — a tranche's
        // rule-2 refusal included (this call's queued rows withdrawn).
        let polled = poll_once(conn, seq, wanted, tranche);
        let blockers = match polled {
            Ok(None) => return Ok(()),
            Ok(Some(blockers)) => blockers,
            Err(e) => {
                withdraw_queued(conn, seq, wanted);
                return Err(e);
            }
        };
        if started.elapsed() >= config.lock_wait_ceiling {
            // Withdraw this call's queued (ungranted) rows: an abandoned
            // claim must never dam the queue for claimants behind it.
            let tx = immediate(conn)?;
            for w in wanted {
                tx.execute(
                    "DELETE FROM locks
                     WHERE pool = ?1 AND resource = ?2 AND seq = ?3 AND mode = ?4
                       AND granted = 0",
                    params![w.pool, w.key, seq.0, w.mode.as_str()],
                )?;
            }
            tx.commit()?;
            let named = blockers
                .iter()
                .map(|(what, s, m, g)| {
                    format!(
                        "{what} blocked by holder seq {s} ({}, {})",
                        m.as_str(),
                        if *g { "granted" } else { "queued" }
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            // WHAT each holder IS, read off its spine row: a holder that is decided and not cleared after the
            // whole ceiling is named STRANDED and the repair is named with
            // it. Locks release only at the cleared flip, so a
            // commit that died between its decided flip and its cleared
            // flip holds them forever, and nothing a waiter can do lifts
            // them — `Cauldron::recover()` does, and `Cauldron::open` runs
            // it.
            let holders: BTreeSet<i64> = blockers.iter().map(|(_, s, _, _)| *s).collect();
            let described = holders
                .iter()
                .map(|s| describe_holder(conn, *s, config))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(SpineError::Busy {
                message: format!(
                    "lock wait ceiling exceeded ({:?}) for seq {seq}: {named}. {described}",
                    config.lock_wait_ceiling
                ),
            });
        }
        std::thread::sleep(interval);
        interval = (interval * 2).min(max_interval);
    }
}

/// One blocking holder, classified by its spine row for the ceiling
/// refusal. The three states a holder of locks can be in, and what each means
/// to the waiter:
/// - DECIDED AND NOT CLEARED, after the full ceiling: either a commit still
///   landing (a landing that outlasts the ceiling is itself pathological)
///   or a commit whose process died between its decided flip and its
///   cleared flip — STRANDED, holding its locks forever, finished only by
///   `Cauldron::recover()` (which rolls it forward from its own payload and
///   releases its locks; `Cauldron::open` runs it).
/// - APPENDED AND NOT DECIDED: a live door between its append and its
///   decided flip, or an attempt that died there; `recover()` aborts it.
/// - CLEARED: a lock row its own cleared flip should have released — the
///   orphan `recover()` sweeps first.
/// A row that cannot be read is said so, never guessed at.
///
/// A TRANCHE ANCHOR is named first and by name (under `touched` a writer on
/// a touched resource is answered the spine's `busy` naming `tranche <T>` as
/// the holder): its seq is a `tranches`
/// row's, and its spine row is appended and never decided for the whole
/// life of the tranche, so the ordinary classification would call it a
/// "live door … recover() aborts it", which is false — the tranche pass
/// skips it. The text keeps the `holder seq <n>` head every other arm has:
/// the soup's `busy_holder_seqs` (services/soup/src/trigger_host.rs) reads
/// exactly that tag and nothing else of this message.
fn describe_holder(conn: &rusqlite::Connection, holder: i64, config: &SpineConfig) -> String {
    let tranche: Option<(String, i64, String)> = match conn
        .query_row(
            "SELECT id, attempt, state FROM tranches WHERE seq = ?1",
            params![holder],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
    {
        Ok(t) => t,
        Err(e) => {
            return format!("holder seq {holder}: its tranche row could not be read ({e})");
        }
    };
    if let Some((id, attempt, state)) = tranche {
        return format!(
            "holder seq {holder} is tranche {id} (attempt {attempt}, {state}): the anchor of a \
             tranche holding what its children touched; its claims release when it lands, \
             rolls back or is abandoned"
        );
    }
    let row: Option<(i64, i64, String)> = match conn
        .query_row(
            "SELECT decided, cleared, ts FROM transactions WHERE seq = ?1",
            params![holder],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
    {
        Ok(row) => row,
        Err(e) => {
            return format!("holder seq {holder}: its spine row could not be read ({e})");
        }
    };
    match row {
        None => format!(
            "holder seq {holder}: NO spine row — a lock row for a sequence number the spine \
             never issued; recover() releases nothing it cannot account for, repair by hand"
        ),
        Some((_, cleared, ts)) if cleared != 0 => format!(
            "holder seq {holder} (appended {ts}): CLEARED, yet its lock rows stand — an orphan \
             of a crash between the cleared flip and the release; recover() sweeps it"
        ),
        Some((decided, _, ts)) if decided != 0 => format!(
            "holder seq {holder} (appended {ts}): DECIDED and NOT CLEARED after the whole wait \
             ceiling ({:?}) — a commit that died between its decided flip and its cleared flip \
             leaves exactly this, STRANDED, holding its locks forever; Cauldron::recover() \
             rolls it forward from its own record and releases them (Cauldron::open runs it)",
            config.lock_wait_ceiling
        ),
        Some((_, _, ts)) => format!(
            "holder seq {holder} (appended {ts}): appended and NOT DECIDED — a live door between \
             its append and its decided flip, or an attempt that died there; \
             Cauldron::recover() aborts it"
        ),
    }
}

/// Withdraw this call's queued (ungranted) rows after a poll FAILED, best
/// effort: the spine may still be the thing that is busy, in which case the
/// rows stay and the next `recover()`'s orphan sweep or a later poll by the
/// same seq is the net. The original error is what the caller reports.
fn withdraw_queued(conn: &rusqlite::Connection, seq: Seq, wanted: &[Wanted]) {
    let Ok(tx) = immediate(conn) else {
        return;
    };
    for w in wanted {
        let _ = tx.execute(
            "DELETE FROM locks
             WHERE pool = ?1 AND resource = ?2 AND seq = ?3 AND mode = ?4 AND granted = 0",
            params![w.pool, w.key, seq.0, w.mode.as_str()],
        );
    }
    let _ = tx.commit();
}

/// One poll of the grant loop: `Ok(None)` — every claim granted, in one
/// atomic step; `Ok(Some(blockers))` — nothing granted, and these are the
/// rows in the way; `Err` — the poll itself failed.
fn poll_once(
    conn: &rusqlite::Connection,
    seq: Seq,
    wanted: &[Wanted],
    tranche: Option<&TrancheClaimant>,
) -> Result<Option<Vec<(String, i64, LockMode, bool)>>> {
    let tx = immediate(conn)?;
    // Checkpoint 3 inside EVERY grant poll: a writer that appended
    // and enrolled before the gate rose is refused at its next poll, before
    // it holds anything; the holder's own polls pass by `seq`.
    gate_check(&tx, seq, wanted)?;
    if let Some(t) = tranche {
        // Rule 2, INSIDE THE GRANT: the state is read in the
        // same immediate transaction as the grant; anything but open or
        // staging refuses, and the caller withdraws this call's queued rows.
        // Without this half, a tranche abandoned while its child waits on a
        // foreign holder would be granted rows after its release and dam the
        // resource until the next restart.
        crate::tranche::claim_state_check(&tx, seq)?;
        // Every poll of a claim wait is a heartbeat — unless the
        // `no-poll-refresh` seam is armed, which
        // is the only way a deadline can lapse under a live claim wait.
        if !crate::fault::skip_poll_refresh()? {
            let now = t.now();
            tx.execute(
                "UPDATE tranches SET last_heartbeat = ?2, deadline = ?3
                 WHERE seq = ?1 AND state IN ('queued','open','staging','landing')",
                params![seq.0, now, now.saturating_add(t.timeout_ms)],
            )?;
        }
    }
    // (claim description, blocker seq, blocker mode, blocker granted)
    let mut blockers: Vec<(String, i64, LockMode, bool)> = Vec::new();
    {
        for w in wanted {
            // `.optional()?`, never `.unwrap_or(0)`: an absent row means not
            // yet granted, but a storage failure is a storage failure and is
            // never quietly read as "not granted".
            let already: i64 = tx
                .query_row(
                    "SELECT granted FROM locks
                     WHERE pool = ?1 AND resource = ?2 AND seq = ?3 AND mode = ?4",
                    params![w.pool, w.key, seq.0, w.mode.as_str()],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0);
            if already != 0 {
                // Its own grant already stands: a holder is never refused
                // it, and it never re-enters the blocker evaluation.
                continue;
            }
            let mut stmt = tx.prepare(
                "SELECT seq, mode, granted FROM locks
                 WHERE pool = ?1 AND resource = ?2 AND seq != ?3",
            )?;
            let rows = stmt.query_map(params![w.pool, w.key, seq.0], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (other_seq, other_mode, other_granted) = row?;
                let other_mode = LockMode::from_str(&other_mode).ok_or_else(|| {
                    SpineError::hard(format!(
                        "lock table carries an unparseable mode: {other_mode}"
                    ))
                })?;
                let conflicts = !compatible(other_mode, w.mode);
                // The one grant rule (module docs): a conflicting row
                // blocks when it is GRANTED (held, whatever its number) or
                // queued with an EARLIER number. A later-numbered queued
                // claimant never blocks — the earlier reader passes the
                // queued later writer, and no wait ever points younger
                // among the ungranted.
                if conflicts && (other_granted != 0 || other_seq < seq.0) {
                    blockers.push((
                        format!("{} on {}/{}", w.mode.as_str(), w.pool, w.key),
                        other_seq,
                        other_mode,
                        other_granted != 0,
                    ));
                }
            }
        }
    }
    if blockers.is_empty() {
        for w in wanted {
            tx.execute(
                "INSERT INTO locks (pool, resource, mode, seq, granted)
                 VALUES (?1, ?2, ?3, ?4, 1)
                 ON CONFLICT (pool, resource, seq, mode) DO UPDATE SET granted = 1",
                params![w.pool, w.key, w.mode.as_str(), seq.0],
            )?;
            if w.conversion {
                // Reads drained; the intent converts. The upgrade-intent
                // row is consumed by its own write grant.
                tx.execute(
                    "DELETE FROM locks
                     WHERE pool = ?1 AND resource = ?2 AND seq = ?3
                       AND mode = 'upgrade-intent'",
                    params![w.pool, w.key, seq.0],
                )?;
            }
        }
        tx.commit()?;
        return Ok(None);
    }
    // BLOCKED: COMMIT, never drop. The only write this transaction made is
    // the tranche claimant's heartbeat refresh above, and a dropped
    // transaction ROLLS IT BACK — so a child or a read under key waiting on
    // a lock never actually moved its deadline (found by the opposed arm of
    // the test that reads the deadline while a claimant waits across
    // processes). The heartbeat is promised; this is where it lands.
    tx.commit()?;
    Ok(Some(blockers))
}

/// Release EVERY lock held by `seq`, at the cleared flip only (between one
/// pool's landing and the next the pools disagree; this release
/// rule is one of the three ways that window stays invisible). The SAME
/// call serves the abort path — an aborted transaction's locks release too,
/// just never per-pool as parts land. Idempotent: releasing a seq that
/// holds nothing is a quiet no-op.
///
/// VISIBILITY (the one-door law): `pub` only under `test-support`
/// — from outside the crate this would strip a decided, un-cleared
/// transaction's locks and open the half-landed window to every lock-taker
/// Crate-private for every consumer; identical behavior.
#[cfg(feature = "test-support")]
pub fn release_at_clear(spine: &SpineDb, seq: Seq) -> Result<()> {
    release_at_clear_interior(spine, seq)
}
#[cfg(not(feature = "test-support"))]
pub(crate) fn release_at_clear(spine: &SpineDb, seq: Seq) -> Result<()> {
    release_at_clear_interior(spine, seq)
}

fn release_at_clear_interior(spine: &SpineDb, seq: Seq) -> Result<()> {
    spine
        .conn()
        .execute("DELETE FROM locks WHERE seq = ?1", params![seq.0])?;
    Ok(())
}

/// Release every lock held by `seq` because the transaction ABORTED — it was
/// refused, or recovery found it un-decided. Deliberately the SAME act as [`release_at_clear`] and not a
/// second mechanism: the two release points are the cleared flip and the
/// abort, and there is no third, no per-pool release as parts land, and no
/// implicit drop-guard anywhere. The separate name exists so the abort path
/// reads as an abort at its call site instead of borrowing the word
/// "cleared" for a transaction that never cleared. Idempotent.
///
/// VISIBILITY: gated exactly as [`release_at_clear`].
#[cfg(feature = "test-support")]
pub fn release_on_abort(spine: &SpineDb, seq: Seq) -> Result<()> {
    release_at_clear_interior(spine, seq)
}
#[cfg(not(feature = "test-support"))]
pub(crate) fn release_on_abort(spine: &SpineDb, seq: Seq) -> Result<()> {
    release_at_clear_interior(spine, seq)
}

/// Release every lock whose holding row is already CLEARED — the orphan
/// sweep [`crate::recover::recover`] opens with. Locks may
/// only be held by un-cleared rows; a crash in the instant between a row's
/// cleared flip and its lock release leaves lock rows no pending-row walk
/// can reach. This restores the invariant in one statement and can never
/// touch a live transaction's locks, because a cleared row never un-clears.
///
/// VISIBILITY: gated exactly as [`release_at_clear`] — benign
/// in effect, still a writer of the lock table.
#[cfg(feature = "test-support")]
pub fn release_cleared_orphans(spine: &SpineDb) -> Result<()> {
    release_cleared_orphans_interior(spine)
}
#[cfg(not(feature = "test-support"))]
pub(crate) fn release_cleared_orphans(spine: &SpineDb) -> Result<()> {
    release_cleared_orphans_interior(spine)
}

fn release_cleared_orphans_interior(spine: &SpineDb) -> Result<()> {
    spine.conn().execute(
        "DELETE FROM locks WHERE seq IN (SELECT seq FROM transactions WHERE cleared = 1)",
        [],
    )?;
    Ok(())
}
