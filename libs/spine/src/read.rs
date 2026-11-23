//! The validated snapshot read — how lock-free reads are safe across pools
//! (deliberately not read-locked: locked display reads would inherit the
//! write queue's latency, and reads must be instantaneous; the exposure a
//! lock-free read has is closed by VALIDATION instead).
//!
//! One read semantics, deliberately — no cheaper consistency-not-guaranteed
//! tier is offered. A single-pool multi-query read needs neither locks nor
//! validation (per-pool atomic landing).
//!
//! # Two former deviations, CLOSED
//!
//! **Step 4 is read STRICTLY now.** It used to be read conservatively — any
//! counter movement invalidated — because the spine's two named exposures (a
//! scalar counter; each commit's touched-pool set) cannot say WHICH commits
//! landed inside a window. The spine now offers a THIRD exposure
//! ([`SpineDb::commits_touching`], [`crate::Cauldron::commits_touching`]):
//! every committed row carries the counter value it committed AT, stamped
//! in the same spine transaction as the last part's landed flip, so "the
//! commits in `(c_start, c_end]` that touch my pools" is one indexed
//! query. [`window_is_consistent`] discards a cut only when such a commit
//! exists, or when a still-landing transaction touches one of the read's
//! pools. A storm on OTHER pools no longer invalidates anybody.
//! "Touches" means "has a delta-bearing part there": a premise-only part
//! changed no state a reader could have straddled (`in_landing` and the
//! third exposure share the one definition).
//!
//! **The escalated read COMPLETES.** Its real read locks used to be
//! type-slice grain only — populations held still, matched BODIES not — so
//! a storm of SETs on the matched nodes kept invalidating it and it gave up
//! as `Busy`, an honest deviation from "the escalated read completes". The
//! repair is the write path's own match-then-claim-bodies RESTART SHAPE on
//! the read side (no in-place lock upgrade): the escalation appends a
//! row for a fresh sequence number, enrols a COMPLETE claim set — the
//! statement's static slice claims plus read on every matched body — takes
//! it at that number, and re-matches under it; if the match set changed,
//! the attempt is abandoned (row aborted, locks released) and the next
//! attempt enrols the larger set at a fresh number, bounded by
//! `restart_bound`. Under a SET storm on its own pool the read now
//! completes in sequence order: an older SET goes first (the reader waits
//! behind it), a younger SET waits behind the reader's body claims. Under
//! complete claims the cut is consistent by the locks and by the re-match
//! equality — the same rule the write path's phase 10 trusts — so no
//! counter check runs there. What REMAINS is exactly the lock design's named
//! starvation residual — a transaction restarted repeatedly under constant
//! contention refreshes its number each time and can starve — so a give-up
//! past `restart_bound` stays loud, as `Busy`, and is labelled as THAT
//! residual, not as a deviation.
//!
//! The restart shape needs to know which bodies a value bound, and the
//! frozen [`run_validated`] is generic over its value and cannot derive
//! that; [`run_validated_claiming`] takes the derivation and is what the
//! read door uses. [`run_validated`] itself keeps its previous escalation
//! (slices only, validated, bounded) for the write path's match phase,
//! whose completion is the write path's own restart loop's job
//! (`exec::execute`) — a known seam.
//!
//! The write statement's match phase runs AS this read, and bulk read is
//! the statement's degenerate read form — one grammar, one matcher, both
//! directions.
//!
//! # SIGNATURE CHANGES MADE HERE
//!
//! Two, both forced by the decision in [`crate::Cauldron`]'s docs that the
//! cauldron holds no open connection (a `rusqlite::Connection` is not `Sync`,
//! so a cached one would make `&Cauldron` unshareable across the writer
//! threads the contention tests need):
//!
//! 1. [`open_snapshots`] can no longer borrow its snapshots from the
//!    cauldron, because there is nothing there to borrow. Pool HANDLES are
//!    opened into an owned [`OpenPools`] first, and the snapshots borrow
//!    from that. The 2.5 step is unchanged; only where the lifetime comes
//!    from moved.
//! 2. [`run_validated`] takes the claim set its bounded ESCALATION acquires.
//!    The escalation must take real read locks and this
//!    driver is generic over its queries, so it cannot derive a claim from a
//!    statement it never sees; the caller (which has the statement) supplies
//!    `ladle::compile::claim_footprint`.
//!
//! Nothing outside this module consumed either signature.

use crate::config::SpineConfig;
use crate::delta::CompiledPayload;
use crate::envelope::{Envelope, Initiator, Instrument, Mode, Provenance, TxHash};
use crate::error::{Result, SpineError};
use crate::ladle::ast::{
    ArrowDir, Binding, HopPattern, MatchClause, NodePattern, PatternEnd, Statement, ValueExpr,
    FIELD_ID, FIELD_STATE,
};
use crate::ladle::compile::{Fuel, FUEL_PER_EVAL, FUEL_PER_ROW, PROJECTED_PATTERN_FORM};
use crate::locks::{self, ClaimSet};
use crate::pool::{EdgeRecord, NodeId, NodeRecord, Overlay, Pool, QualifiedNodeId, Snapshot};
use crate::project;
use crate::schema_def::PoolName;
use crate::locks::{Claim, LockMode, Resource};
use crate::spine_db::{AppendOutcome, CommittedCommit, InLanding, Seq, SpineDb, SPINE_FILE};
use crate::witness::Witness;
use crate::Cauldron;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

// ---- the validated-read procedure: the 4 steps as named signatures,
//      plus the bounded escalation type ----

/// STEP 1: read the spine's committed-commit counter — C-start.
pub fn counter_start(spine: &SpineDb) -> Result<u64> {
    spine.committed_commit_counter()
}

/// STEP 2, first half: open one POOL HANDLE per touched pool. Owned, so the
/// snapshots of the second half have something to borrow from — see this
/// module's signature note.
///
/// The projected namespace is NOT a pool and is never opened here:
/// a name of `spine` in `pools` is skipped rather than refused, so a caller
/// that hands the footprint through unfiltered still gets its real pools.
/// The matcher resolves ghosts against the spine file on demand instead —
/// see [`Snapshots::resolve_ghost`].
pub fn open_pools(cauldron: &Cauldron, pools: &[PoolName]) -> Result<OpenPools> {
    let mut open = BTreeMap::new();
    for p in pools {
        if project::is_projected(p) {
            continue;
        }
        if !open.contains_key(p) {
            open.insert(p.clone(), cauldron.open_pool(p)?);
        }
    }
    Ok(OpenPools {
        pools: open,
        spine_file: cauldron.root.join(SPINE_FILE),
    })
}

/// STEP 2, second half: open one snapshot per touched pool. The
/// queries then run against these. Every snapshot is opened back to back so
/// the window the validation certifies is as narrow as the machine allows.
pub fn open_snapshots<'a>(open: &'a OpenPools) -> Result<Snapshots<'a>> {
    let mut by_pool = BTreeMap::new();
    for (name, pool) in &open.pools {
        by_pool.insert(name.clone(), pool.snapshot()?);
    }
    Ok(Snapshots {
        by_pool,
        spine_file: &open.spine_file,
    })
}

/// STEP 2 under a tranche: one snapshot per touched
/// pool WITH the tranche's overlay layered on top wherever the view holds
/// one — the holder's view. Same window as [`open_snapshots`]; a pool no
/// staged child touched gets a plain committed snapshot, which is the same
/// thing with nothing on top.
pub(crate) fn open_snapshots_over<'a>(open: &'a OpenPools, view: &'a View) -> Result<Snapshots<'a>> {
    let mut by_pool = BTreeMap::new();
    for (name, pool) in &open.pools {
        let snap = match view.overlay(name) {
            Some(o) => pool.snapshot_over(o)?,
            None => pool.snapshot()?,
        };
        by_pool.insert(name.clone(), snap);
    }
    Ok(Snapshots {
        by_pool,
        spine_file: &open.spine_file,
    })
}

/// THE VIEW: one tranche's committed-plus-staged state
/// — per touched pool, an [`Overlay`] holding the staged children's deltas.
/// Lives in the tranche's in-process mutex slot (`crate::tranche`), so it
/// is read and grown only under the per-tranche mutex and dropped at
/// the terminal transition. Derived state, never on disk: [`View::rebuild`]
/// replays the `tranche_children` rows in ordinal order, which is also what
/// a boot does for every non-terminal tranche.
pub struct View {
    overlays: BTreeMap<PoolName, Overlay>,
    /// How many staged children — ordinals `0..applied` — the overlays
    /// include; `children_seen` on a read under key.
    applied: u32,
}

impl View {
    pub(crate) fn empty() -> View {
        View {
            overlays: BTreeMap::new(),
            applied: 0,
        }
    }

    /// The number of staged children whose deltas this view includes.
    pub fn children_seen(&self) -> u32 {
        self.applied
    }

    pub(crate) fn overlay(&self, pool: &PoolName) -> Option<&Overlay> {
        self.overlays.get(pool)
    }

    /// Apply one staged child's parts to the overlays, in
    /// part order, creating a pool's overlay on its first touch. Reads the
    /// committed pool for copy-on-first-touch; writes NO pool file.
    pub(crate) fn apply_child(
        &mut self,
        cauldron: &Cauldron,
        hash: &TxHash,
        payload: &CompiledPayload,
    ) -> Result<()> {
        for (ordinal, part) in payload.parts.iter().enumerate() {
            let ordinal = u32::try_from(ordinal)
                .map_err(|_| SpineError::usage("a statement cannot carry more parts than u32::MAX"))?;
            if !self.overlays.contains_key(&part.pool) {
                let grammar = cauldron.grammar(&part.pool).ok_or_else(|| {
                    SpineError::hard(format!(
                        "a staged child's part names pool `{}`, which this cauldron does not hold",
                        part.pool
                    ))
                })?;
                self.overlays.insert(part.pool.clone(), Overlay::new(grammar)?);
            }
            let committed_pool = cauldron.open_pool(&part.pool)?;
            let committed = committed_pool.snapshot()?;
            let overlay = self
                .overlays
                .get_mut(&part.pool)
                .expect("inserted above");
            overlay.apply_child_part(&committed, hash, ordinal, part)?;
        }
        self.applied += 1;
        Ok(())
    }

    /// Rebuild the view of attempt `(tranche, attempt)` from its
    /// `tranche_children` rows, in ordinal order. The
    /// function a boot calls for every non-terminal tranche and the
    /// one a fresh process calls on the first act under a key it has never
    /// seen. Expects the rows to be exactly ordinals `0..n` — a gap is a torn
    /// record and is refused loudly rather than replayed around.
    pub(crate) fn rebuild(cauldron: &Cauldron, tranche: &str, attempt: u32) -> Result<View> {
        let spine = cauldron.spine()?;
        let rows = Self::children_rows(spine.conn(), tranche, attempt)?;
        drop(spine);
        Self::replay(cauldron, tranche, attempt, rows)
    }

    /// The `tranche_children` rows of attempt `(tranche, attempt)`, in
    /// ordinal order, read on `conn` — inside the caller's transaction when
    /// it has one, so a caller that also reads `staged` reads both in ONE
    /// snapshot (the open's rebuild relies on this).
    pub(crate) fn children_rows(
        conn: &rusqlite::Connection,
        tranche: &str,
        attempt: u32,
    ) -> Result<Vec<(i64, String, String)>> {
        let mut stmt = conn.prepare(
            "SELECT ordinal, hash, payload FROM tranche_children
             WHERE tranche_id = ?1 AND tranche_attempt = ?2 ORDER BY ordinal",
        )?;
        let rows = stmt.query_map(rusqlite::params![tranche, attempt], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Replay `rows` (from [`View::children_rows`]) into a fresh view,
    /// refusing a gap in the ordinals as a torn record.
    pub(crate) fn replay(
        cauldron: &Cauldron,
        tranche: &str,
        attempt: u32,
        rows: Vec<(i64, String, String)>,
    ) -> Result<View> {
        let mut view = View::empty();
        for (ordinal, hash, payload) in rows {
            if ordinal != i64::from(view.applied) {
                return Err(SpineError::hard(format!(
                    "tranche `{tranche}` attempt {attempt}: child ordinals are not contiguous (found \
                     {ordinal} where {} was expected); the record is torn",
                    view.applied
                )));
            }
            let hash = TxHash::from_hex(&hash).ok_or_else(|| {
                SpineError::hard(format!(
                    "tranche `{tranche}` attempt {attempt} child {ordinal}: hash `{hash}` is not a \
                     transaction hash"
                ))
            })?;
            let payload: CompiledPayload = serde_json::from_str(&payload).map_err(|e| {
                SpineError::hard(format!(
                    "tranche `{tranche}` attempt {attempt} child {ordinal}: unparseable payload: {e}"
                ))
            })?;
            view.apply_child(cauldron, &hash, &payload)?;
        }
        Ok(view)
    }
}

/// STEP 3: read the set of transactions currently in landing state, THEN
/// the counter again (C-end), THEN — through the third exposure —
/// the commits that landed in `(c_start, c_end]` touching the read's pools
/// (the landing-state check is load-bearing: a commit landing
/// its second half after the last counter read would slip a bare counter
/// check). The commits query runs after the counter read: a commit counted
/// at or below C-end has its stamp already, because stamp and increment are
/// one spine transaction.
///
/// # The order is load-bearing: `in_landing` BEFORE the counter
///
/// The three reads are separate autocommit queries, so a commit can move
/// between them. Read the counter FIRST and a commit Y whose part is already
/// in the caller's snapshot can escape: the counter is read just before Y's
/// last landed flip, Y is counted and cleared before the `in_landing` read,
/// so Y is in neither the landing set nor `commits_touching(c_start,
/// counter)` (its `committed_at` is `counter + 1`) — and the verdict is
/// "consistent" over a snapshot that holds Y at a cut below Y. Reading
/// `in_landing` first closes it. Take any commit Z with `committed_at(Z) >
/// counter`. At the `in_landing` read Z was either (a) in landing — the read
/// discards; (b) not yet decided — so its parts land after that read, which
/// is after the snapshot pinned, and the snapshot does not hold Z; or (c)
/// already cleared — but a cleared commit was counted before it cleared, so
/// `committed_at(Z) <= counter`, a contradiction. Commits in `(c_start,
/// counter]` are caught by `commits_touching` as before. So the cut the
/// caller certifies is exact: everything at or below it, nothing above it.
/// The window is microseconds wide and no storm forces it reliably; its
/// guard is `tests/node_view.rs`'s source-order scan plus this argument.
///
/// SIGNATURE CHANGE: takes `c_start` and `pools` — the
/// window query needs both. No caller outside this module consumed the old
/// shape.
pub fn counter_end(spine: &SpineDb, c_start: u64, pools: &[PoolName]) -> Result<WindowCheck> {
    let in_landing = spine.in_landing(None)?;
    let counter = spine.committed_commit_counter()?;
    let touching = spine.commits_touching(pools, c_start, counter)?;
    Ok(WindowCheck {
        c_start,
        counter,
        in_landing,
        touching,
    })
}

/// STEP 4, STRICT: the consistency verdict — no commit in the window
/// TOUCHED the read's pools, and no still-landing transaction touches them:
/// the combined read is a consistent cut. Otherwise discard and retry.
/// Both halves are pool-precise now: the commit
/// half through the third exposure, the landing half through each landing
/// transaction's touched set. A storm on other pools does not invalidate.
///
/// SIGNATURE CHANGE: `c_start` moved into
/// [`WindowCheck`], where the window query already needed it.
pub fn window_is_consistent(end: &WindowCheck, pools: &[PoolName]) -> Result<bool> {
    if !end.touching.is_empty() {
        return Ok(false);
    }
    for landing in &end.in_landing {
        if landing.touched.iter().any(|p| pools.contains(p)) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// What step 3 reads: the window `(c_start, counter]`, the in-landing set
/// with touched pools, and the commits that landed in the window touching
/// the read's pools (the third exposure; empty means the counter half is
/// clean for THESE pools, whatever the counter did for others).
#[derive(Debug, Clone)]
pub struct WindowCheck {
    pub c_start: u64,
    pub counter: u64,
    pub in_landing: Vec<InLanding>,
    pub touching: Vec<CommittedCommit>,
}

/// One node, hydrated whole, with the committed-commit cut it is consistent
/// at (what the soup daemon's hydrate consumes).
///
/// `cut` is the counter value the read was certified at: every commit with
/// `committed_at <= cut` is in `record`/`out`, and none with `committed_at >
/// cut` is — including a commit that was LANDING while the read ran (a
/// view that shows a commit below its
/// `committed_at` makes a fold apply it twice). That is what lets a consumer
/// apply exactly the committed events with `committed_at > cut` on top.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeView {
    pub record: NodeRecord,
    /// The spine row that created the node (`nodes.created_tx`).
    pub created_tx: TxHash,
    /// Every edge leaving the node, as [`Snapshot::edges_from`] orders them.
    pub out: Vec<EdgeRecord>,
    pub cut: u64,
}

/// Hydrate one node under the validated-window discipline,
/// EVEN THOUGH it reads one pool. [`drive`]'s single-pool exemption is
/// sound for a value with no cut — one snapshot is internally consistent —
/// but it reads no counter, so it cannot say WHERE that snapshot sits on
/// the commit order, and a cut read beside it unvalidated can sit below a
/// commit the snapshot already shows. So each attempt runs the four steps
/// explicitly: C-start, one snapshot (node, `created_tx`, out-edges), step
/// 3 ([`counter_end`], whose order is what makes the cut exact), step 4.
///
/// A consistent window returns `cut = C-end`. An inconsistent one drops the
/// snapshot, sleeps `lock_poll_interval`, and retries up to `retry_bound`;
/// past it the answer is a loud [`SpineError::Busy`] naming the node, the
/// attempts and every landing that invalidated them — never a cut below a
/// commit the view shows (no completing escalation, by design: a body
/// read lock would not hold the node's edges
/// still, and no contract designs a claim set over its edge slices).
///
/// `Ok(None)` — the node is absent — is certified by the same window. An
/// unknown pool, or the projected `spine` namespace, is `NotFound` through
/// [`Cauldron`]'s own pool refusal. A node row whose `created_tx` will not
/// parse is `Hard`.
pub fn node_view(cauldron: &Cauldron, id: &QualifiedNodeId) -> Result<Option<NodeView>> {
    let config = cauldron.config();
    let pools = [id.pool.clone()];
    // Refuse an unknown or projected pool before touching the spine.
    let pool = cauldron.open_pool(&id.pool)?;
    let spine = cauldron.spine()?;
    let mut attempts: u32 = 0;
    let mut invalidated_by: BTreeSet<String> = BTreeSet::new();
    loop {
        attempts += 1;
        // 1. C-start, BEFORE the snapshot pins its view.
        let c_start = counter_start(&spine)?;
        // 2. one snapshot; every query runs against it.
        let read = {
            let snap = pool.snapshot()?;
            match snap.node(&id.id)? {
                None => None,
                Some(record) => {
                    let created_tx = snap.created_tx(&id.id)?.ok_or_else(|| {
                        SpineError::hard(format!(
                            "node `{id}` has a body row but no `created_tx`; the read cannot \
                             name the spine row that created it"
                        ))
                    })?;
                    let out = snap.edges_from(&id.id, None)?;
                    Some((record, created_tx, out))
                }
            }
            // The snapshot drops here, BEFORE step 3: its view pinned when it
            // opened, and nothing it read can change after that.
        };
        // 3. in-landing, C-end, the commits in the window. 4. the verdict.
        let end = counter_end(&spine, c_start, &pools)?;
        if window_is_consistent(&end, &pools)? {
            return Ok(read.map(|(record, created_tx, out)| NodeView {
                record,
                created_tx,
                out,
                cut: end.counter,
            }));
        }
        for c in &end.touching {
            invalidated_by.insert(format!("committed {} (seq {}, at {})", c.hash.to_hex(), c.seq.0, c.committed_at));
        }
        for l in &end.in_landing {
            if l.touched.iter().any(|p| pools.contains(p)) {
                invalidated_by.insert(format!("landing {} (seq {})", l.hash.to_hex(), l.seq.0));
            }
        }
        if attempts > config.retry_bound {
            return Err(SpineError::Busy {
                message: format!(
                    "node_view `{id}`: {attempts} attempt(s) were each invalidated (retry_bound \
                     {}); no consistent cut was certified, so none is returned. Invalidated by: {}",
                    config.retry_bound,
                    invalidated_by.into_iter().collect::<Vec<_>>().join("; ")
                ),
            });
        }
        std::thread::sleep(config.lock_poll_interval);
    }
}

/// The owned pool handles one validated read runs against (see the
/// module's signature note). Dropping it closes every connection.
pub struct OpenPools {
    pools: BTreeMap<PoolName, Pool>,
    /// Where the spine lives, for on-demand ghost resolution.
    spine_file: PathBuf,
}

impl OpenPools {
    /// The snapshots of these handles — [`open_snapshots`] by another name,
    /// for callers that read better as a method.
    pub fn snapshots(&self) -> Result<Snapshots<'_>> {
        open_snapshots(self)
    }

    /// The pin one of these handles read off its FILE when it was opened —
    /// the pool's own record, not the cauldron's cache (the door's
    /// pre-decided pin check, `exec::check_pins_and_occupancy`).
    /// `None` for a pool this set never opened.
    pub fn pin_of(&self, pool: &PoolName) -> Option<u32> {
        self.pools.get(pool).map(|p| p.grammar().version)
    }
}

/// One snapshot per touched pool, opened together for one validated read.
pub struct Snapshots<'a> {
    pub by_pool: BTreeMap<PoolName, Snapshot<'a>>,
    /// The spine file, opened only when a statement names a ghost.
    spine_file: &'a Path,
}

impl Snapshots<'_> {
    /// Resolve a by-identifier match into the projected namespace:
    /// the ghost's record, COMPUTED ON READ from the spine's own tables
    /// (the projection resolves transparently on read and refuses on
    /// write).
    ///
    /// The spine is opened on demand rather than with the pool snapshots:
    /// the price (one file open) is paid only by statements that name a
    /// ghost, and the projection is deliberately OUTSIDE the validated
    /// read's snapshot set — nothing can write it, and the one fact an edge
    /// toward it depends on (that the row EXISTS) can only become true, never
    /// false (rows are kept forever). Its lifecycle state may move between
    /// this read and the commit; that is the projection reading the tables
    /// as they are, which is what "computed on read" means.
    pub fn resolve_ghost(&self, id: &NodeId) -> Result<Option<NodeRecord>> {
        let spine = SpineDb::open(self.spine_file)?;
        project::resolve_ghost(&spine, id)
    }
    /// One pool's snapshot, or a loud failure naming the pool. A matcher that
    /// reaches for a pool the footprint never opened is a bug in the door,
    /// not a caller's fault, so it is `Hard` rather than a refusal.
    pub fn of(&self, pool: &PoolName) -> Result<&Snapshot<'_>> {
        self.by_pool.get(pool).ok_or_else(|| {
            SpineError::hard(format!(
                "the validated read opened no snapshot for pool `{pool}`; the statement's \
                 footprint and the snapshot set disagree"
            ))
        })
    }
}

/// The driver: run `queries` under the 4-step validation loop, retrying up
/// to `config.retry_bound` times, then escalating (the bounded
/// escalation — the only path by which a reader ever enters a queue,
/// expected cold).
///
/// `escalation_claims` is what the escalation acquires as REAL READ LOCKS;
/// see the module's signature note for why it is a parameter.
///
/// A SINGLE-POOL read skips validation entirely (single-pool
/// multi-query reads need neither locks nor validation — one pool snapshot
/// is consistent by the per-pool atomic landing step).
pub fn run_validated<T>(
    cauldron: &Cauldron,
    pools: &[PoolName],
    config: &SpineConfig,
    escalation_claims: &ClaimSet,
    mut queries: impl FnMut(&Snapshots<'_>) -> Result<T>,
) -> Result<ValidatedResult<T>> {
    drive(
        cauldron,
        pools,
        config,
        escalation_claims,
        None,
        &mut queries,
    )
}

/// [`run_validated`] with the escalation that COMPLETES (see the module
/// docs): `bodies_of` derives, from a value the queries produced, the read
/// claims on every body it bound — [`body_claims`] over a [`MatchSet`] is
/// the derivation the read door uses. Past `retry_bound`, the escalation
/// enrols `escalation_claims` plus those body claims at a fresh sequence
/// number, re-runs the queries under them, and restarts with the larger set
/// whenever the bodies changed, bounded by `config.restart_bound`.
///
/// A SURFACE ADDITION beside the frozen signature, not a change to it: the
/// write path's match phase (`exec::match_phase`) keeps calling
/// [`run_validated`] unchanged.
pub fn run_validated_claiming<T>(
    cauldron: &Cauldron,
    pools: &[PoolName],
    config: &SpineConfig,
    escalation_claims: &ClaimSet,
    bodies_of: &dyn Fn(&T) -> ClaimSet,
    mut queries: impl FnMut(&Snapshots<'_>) -> Result<T>,
) -> Result<ValidatedResult<T>> {
    drive(
        cauldron,
        pools,
        config,
        escalation_claims,
        Some(bodies_of),
        &mut queries,
    )
}

/// The read claims on every body a match bound (reading a
/// node's fields takes read on its body) — except a GHOST, which takes no
/// claim (nothing can write the projection). The read side's
/// derivation, byte-for-byte the rule `ladle::compile::claims_for` applies
/// to the write side's matched set.
pub fn body_claims(matched: &MatchSet) -> ClaimSet {
    let mut set = ClaimSet::new();
    for nodes in matched.bindings.values() {
        for n in nodes {
            if project::is_projected(&n.pool) {
                continue;
            }
            set.insert(Claim {
                pool: n.pool.clone(),
                resource: Resource::Body {
                    node: n.node.id.clone(),
                },
                mode: LockMode::Read,
            });
        }
    }
    set
}

/// The 4-step loop both entries share, then the escalation the caller
/// chose: with `bodies_of`, the restart shape that completes; without it,
/// the slice-only validated escalation.
fn drive<T>(
    cauldron: &Cauldron,
    pools: &[PoolName],
    config: &SpineConfig,
    escalation_claims: &ClaimSet,
    bodies_of: Option<&dyn Fn(&T) -> ClaimSet>,
    queries: &mut impl FnMut(&Snapshots<'_>) -> Result<T>,
) -> Result<ValidatedResult<T>> {
    if pools.len() <= 1 {
        let open = open_pools(cauldron, pools)?;
        let snaps = open_snapshots(&open)?;
        let value = queries(&snaps)?;
        return Ok(ValidatedResult {
            value,
            retries: 0,
            restarts: 0,
            escalation: Escalation::None,
        });
    }

    let spine = cauldron.spine()?;
    let mut retries: u32 = 0;
    // The last discarded value's body claims — the SEED of the escalation's
    // first complete claim set. It may be stale (its window was invalidated);
    // the re-match under locks is what makes that harmless.
    let mut seed = ClaimSet::new();
    loop {
        // 1. C-start, BEFORE any snapshot pins its view.
        let c_start = counter_start(&spine)?;
        // 2. one snapshot per touched pool; run the queries.
        let open = open_pools(cauldron, pools)?;
        let snaps = open_snapshots(&open)?;
        let value = queries(&snaps)?;
        // 3. C-end, the in-landing set, the commits in the window touching
        //    these pools. 4. the verdict.
        let end = counter_end(&spine, c_start, pools)?;
        if window_is_consistent(&end, pools)? {
            return Ok(ValidatedResult {
                value,
                retries,
                restarts: 0,
                escalation: Escalation::None,
            });
        }
        if let Some(derive) = bodies_of {
            seed = derive(&value);
        }
        drop(snaps);
        drop(open);
        if retries >= config.retry_bound {
            break;
        }
        retries += 1;
        std::thread::sleep(config.lock_poll_interval);
    }
    match bodies_of {
        Some(derive) => escalated_claiming(
            cauldron,
            pools,
            config,
            escalation_claims,
            seed,
            retries,
            derive,
            queries,
        ),
        None => escalated(cauldron, pools, config, escalation_claims, retries, queries),
    }
}

/// The BOUNDED ESCALATION: a read invalidated
/// past `retry_bound` takes REAL READ LOCKS with a spine sequence number and
/// completes by the same ordering that prevents deadlock — the only path by
/// which a reader ever enters a queue, expected cold.
///
/// THIS is the slice-only shape (`run_validated`'s, without a body
/// derivation): the static claims at one number, then the validated loop
/// under them, bounded, giving up loudly. The read door does not use it
/// any more — [`escalated_claiming`] is the one that completes;
/// this one stays for the write path's match phase, whose own restart loop
/// claims bodies after the match (`exec::execute`, phase 9).
///
/// # Where a reader's sequence number comes from
///
/// The sequence number IS the pending row's AUTOINCREMENT rowid issued
/// at open, by definition. A number from anywhere else would be
/// a second, incomparable ordering, and the deadlock argument is exactly the
/// claim that all waits point at an older number in ONE order. So an
/// escalating reader APPENDS A PENDING ROW — a `core-mechanical` initiator,
/// the read's own text, an EMPTY payload — takes its locks at that number,
/// reads, releases, and marks the row cleared. The row never flips decided,
/// so it never enters `in_landing` and never moves the committed-commit
/// counter: an escalated read cannot invalidate anybody else's read.
///
/// This is observable (a spine row exists for an escalated read) and it
/// contradicts the general "reads land no row" rule, which is about
/// the WARM path. It is the most debatable interior choice here;
/// the alternative — inventing a
/// second number source — is worse, and refusing the cold path outright
/// would leave `retry_bound` with nothing behind it.
fn escalated<T>(
    cauldron: &Cauldron,
    pools: &[PoolName],
    config: &SpineConfig,
    claims: &ClaimSet,
    retries: u32,
    queries: &mut impl FnMut(&Snapshots<'_>) -> Result<T>,
) -> Result<ValidatedResult<T>> {
    let mut spine = cauldron.spine()?;
    let envelope = escalation_envelope(pools);
    let hash = envelope.hash_identity()?;
    let empty = CompiledPayload {
        parts: Vec::new(),
        minted: BTreeMap::new(),
    };
    // A READ's row: under the halt gate, past the quiesce gate — a reader
    // is not a commit.
    let seq = match spine.append_read_row(&envelope, &hash, &empty)? {
        AppendOutcome::Appended(seq) => seq,
        AppendOutcome::Duplicate(row) => {
            return Err(SpineError::hard(format!(
                "read escalation minted a hash identity that already exists at seq {}; the \
                 escalation nonce is not unique",
                row.seq
            )))
        }
    };

    let outcome = (|| -> Result<ValidatedResult<T>> {
        locks::acquire(&spine, seq, claims, config)?;
        let mut attempts = retries;
        loop {
            let c_start = counter_start(&spine)?;
            let open = open_pools(cauldron, pools)?;
            let snaps = open_snapshots(&open)?;
            let value = queries(&snaps)?;
            let end = counter_end(&spine, c_start, pools)?;
            if window_is_consistent(&end, pools)? {
                return Ok(ValidatedResult {
                    value,
                    retries: attempts,
                    restarts: 0,
                    escalation: Escalation::RealReadLocks { seq },
                });
            }
            drop(snaps);
            drop(open);
            attempts += 1;
            if attempts >= retries.saturating_add(config.retry_bound) {
                // Loud, never a silent stale answer, and never an unbounded
                // wait: the storm outlasted both the lock-free bound and the
                // escalated one.
                return Err(SpineError::Busy {
                    message: format!(
                        "validated read gave up after {attempts} invalidated attempts, the last \
                         {} of them under real read locks at seq {seq}; pools: {}",
                        config.retry_bound,
                        pools
                            .iter()
                            .map(|p| p.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            }
            std::thread::sleep(config.lock_poll_interval);
        }
    })();

    // The reader leaves nothing behind, whatever happened: locks released,
    // row cleared (undecided + cleared IS the abort shape), row
    // kept forever like every other row.
    locks::release_at_clear(&spine, seq)?;
    spine.mark_cleared(
        seq,
        &serde_json::json!({ "read_escalation": true, "outcome": outcome.is_ok() }),
    )?;
    outcome
}

/// The BOUNDED ESCALATION THAT COMPLETES (by the same ordering that
/// prevents deadlock; no in-place upgrade; the write path's restart
/// shape, on the read side).
///
/// Each attempt is one whole restart-shape turn, exactly as the write
/// path's: append a FRESH row (a fresh sequence number — the
/// number IS the row), enrol the COMPLETE claim set at it — the statement's
/// static slice claims plus read on every body the previous match bound —
/// take it in sequence order, re-run the read under it, and compare the
/// bodies it bound with the bodies it claimed. Equal: the cut is consistent
/// by the locks (nothing that could change what this read sees is
/// compatible with a claim it holds — the lock act table) and by the
/// re-match equality (the write path's phase-10 rule), and it is returned.
/// Different: the population moved between the seed and the grant; the
/// attempt is abandoned — locks released, row recorded aborted, kept
/// forever — and the next turn enrols the larger set at a fresh number.
///
/// No counter check runs under complete claims, deliberately: a commit on
/// this read's pools that conflicts with none of its claims cannot change
/// what it returns, and validating against it anyway is what made the
/// slice-only escalation give up under storms it should have ridden out.
///
/// Bounded by `config.restart_bound`. Past it the read gives up LOUDLY as
/// `Busy` — and that give-up is the lock design's named starvation residual
/// (constant restarts under constant contention; the escape — pinning the
/// original number — is a measured-need option deliberately not built
/// yet), not a deviation.
///
/// Every row this shape appends has the escalation's own identity (unique
/// nonce), `core-mechanical` initiator, no parts; it never flips decided,
/// never enters `in_landing`, never moves the counter.
#[allow(clippy::too_many_arguments)]
fn escalated_claiming<T>(
    cauldron: &Cauldron,
    pools: &[PoolName],
    config: &SpineConfig,
    static_claims: &ClaimSet,
    mut seed: ClaimSet,
    retries: u32,
    bodies_of: &dyn Fn(&T) -> ClaimSet,
    queries: &mut impl FnMut(&Snapshots<'_>) -> Result<T>,
) -> Result<ValidatedResult<T>> {
    let mut spine = cauldron.spine()?;
    let empty = CompiledPayload {
        parts: Vec::new(),
        minted: BTreeMap::new(),
    };
    let mut restarts: u32 = 0;
    loop {
        // A FRESH number for this attempt (claims complete at
        // open; growth restarts, never upgrades in place).
        let envelope = escalation_envelope(pools);
        let hash = envelope.hash_identity()?;
        // A READ's row: under the halt gate, past the quiesce gate.
        let seq = match spine.append_read_row(&envelope, &hash, &empty)? {
            AppendOutcome::Appended(seq) => seq,
            AppendOutcome::Duplicate(row) => {
                return Err(SpineError::hard(format!(
                    "read escalation minted a hash identity that already exists at seq {}; the \
                     escalation nonce is not unique",
                    row.seq
                )))
            }
        };
        let mut claims = static_claims.clone();
        for c in seed.iter() {
            claims.insert(c.clone());
        }

        let attempt = (|| -> Result<(T, ClaimSet)> {
            locks::acquire(&spine, seq, &claims, config)?;
            let open = open_pools(cauldron, pools)?;
            let snaps = open_snapshots(&open)?;
            let value = queries(&snaps)?;
            let found = bodies_of(&value);
            Ok((value, found))
        })();

        // Whatever happened, this attempt leaves nothing behind: locks
        // released, row cleared (undecided + cleared IS the abort shape),
        // row kept forever like every other row.
        locks::release_at_clear(&spine, seq)?;
        let (value, found) = match attempt {
            Ok(pair) => pair,
            Err(e) => {
                spine.mark_cleared(
                    seq,
                    &serde_json::json!({ "read_escalation": true, "outcome": false }),
                )?;
                return Err(e);
            }
        };
        let stands = found == seed;
        spine.mark_cleared(
            seq,
            &serde_json::json!({
                "read_escalation": true,
                "outcome": stands,
                "restart": !stands,
            }),
        )?;
        if stands {
            return Ok(ValidatedResult {
                value,
                retries,
                restarts,
                escalation: Escalation::RealReadLocks { seq },
            });
        }
        restarts += 1;
        if restarts > config.restart_bound {
            // THE LOCK DESIGN'S NAMED STARVATION RESIDUAL, loud: the match set
            // moved under every one of `restart_bound` complete claim sets.
            return Err(SpineError::Busy {
                message: format!(
                    "validated read gave up after {retries} invalidated lock-free attempts and \
                     {restarts} restarts under real read locks (last at seq {seq}): the match \
                     set moved under every complete claim set — the lock design's starvation \
                     residual (constant restarts under constant contention), bounded by restart_bound \
                     {}; pools: {}",
                    config.restart_bound,
                    pools
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        seed = found;
    }
}

/// The envelope an escalating reader appends. `core-mechanical` because the
/// machinery, not an agent, is what escalated (a closed initiator class),
/// and a unique nonce because two identical escalations are two different
/// asks that must not dedupe onto one another.
fn escalation_envelope(pools: &[PoolName]) -> Envelope {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TICKET: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let nonce = format!(
        "read-escalation:{}:{nanos}:{}",
        std::process::id(),
        TICKET.fetch_add(1, Ordering::Relaxed)
    );
    Envelope {
        statement: format!(
            "-- validated read escalation over pools: {}",
            pools
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        bindings: BTreeMap::new(),
        format_version: crate::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::CoreMechanical {
                operation: "read::escalation".to_string(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(nonce),
    }
}

/// A validated read's result, honest about how it completed.
#[derive(Debug)]
pub struct ValidatedResult<T> {
    pub value: T,
    /// How many invalidated attempts were discarded before this one.
    pub retries: u32,
    /// How many complete-claim-set attempts the escalation abandoned because
    /// the match set moved under them (the restart shape). Zero on
    /// every lock-free completion and on an escalation whose first complete
    /// claim set stood.
    pub restarts: u32,
    pub escalation: Escalation,
}

/// The bounded escalation type: whether the read
/// completed lock-free or had to take real read locks with a spine sequence
/// number, completing by the same ordering that prevents deadlock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Escalation {
    /// Completed lock-free — the expected, warm path.
    None,
    /// A commit storm on the read's pools forced real read locks.
    RealReadLocks { seq: Seq },
}

// ---- the matcher (the match phase runs as this read; bulk read is
//      the same matcher on the read path) ----

/// Bind a statement's MATCH patterns against one consistent set of
/// snapshots, spending `fuel` per row visited and pattern evaluated
/// (the evaluation ceiling).
///
/// # WHERE IS NOT EVALUATED HERE, and that is load-bearing
///
/// An earlier draft said "MATCH and WHERE", but **WHERE is a PREMISE, not a
/// filter.** A premise that fails at commit time refuses the
/// statement with the failing condition as witness, and `ladle::compile`
/// enforces it universally over every row a binding bound, producing
/// [`Witness::FailedCondition`]. If this matcher pre-filtered rows on WHERE,
/// every condition would be true of the surviving set by construction, the
/// compiler's refusal would be unreachable dead code, and a bulk SET would
/// silently narrow itself — the exact surprise the count expectation exists
/// to prevent. So this function binds on the MATCH pattern ALONE and leaves
/// WHERE entirely to the premise check.
///
/// Count expectations are likewise the compiler's, checked against the sizes
/// of the sets returned here.
pub fn run_match(
    snapshots: &Snapshots<'_>,
    stmt: &Statement,
    params: &crate::envelope::Bindings,
    fuel: &mut Fuel,
) -> Result<MatchSet> {
    run_match_traced(snapshots, stmt, params, fuel).map(|(set, _)| set)
}

/// The candidates one hop TRAVERSED, before the hop narrowed either end to
/// the connected pairs (a hop from a bound node):
/// the tail end's rows and the head end's rows as they stood when the hop
/// ran. A tranche claims `read` on each tail node's out-slice and each head
/// node's in-slice of `kind`, because a candidate the hop DROPPED — a node
/// with no connecting edge — could gain one between staging and landing,
/// and the landing would then refuse a tranche that did nothing wrong.
#[derive(Debug, Clone)]
pub struct HopTrace {
    pub kind: String,
    /// The edge's SOURCE candidates (the arrow's tail).
    pub src: Vec<(PoolName, NodeId)>,
    /// The edge's DESTINATION candidates (the arrow's head).
    pub dst: Vec<(PoolName, NodeId)>,
}

/// [`run_match`], also handing back what every hop traversed. The matcher
/// is unchanged; the trace is a by-product the tranche's claim derivation
/// consumes (`ladle::compile::tranche_read_claims`).
pub fn run_match_traced(
    snapshots: &Snapshots<'_>,
    stmt: &Statement,
    params: &crate::envelope::Bindings,
    fuel: &mut Fuel,
) -> Result<(MatchSet, Vec<HopTrace>)> {
    let mut set = MatchSet::default();
    let mut trace = Vec::new();
    let types = stmt.binding_types();
    for clause in &stmt.matches {
        fuel.spend(FUEL_PER_EVAL)?;
        match clause {
            MatchClause::Node(head) => {
                let rows = match_head(snapshots, head, params, fuel)?;
                set.bindings.insert(head.binding.0.clone(), rows);
            }
            MatchClause::Hop(hop) => {
                // A hop with a projected end is `match:projected-pattern`,
                // staged OUT: a ghost has no outgoing edges to walk
                // and its incoming slice lives in no one pool's snapshot. The
                // coverage table refuses it by name before the door reaches
                // here; this arm refuses it again so a direct caller cannot
                // slip past.
                for b in clause.bindings() {
                    if types.get(&b.0).is_some_and(|(p, _)| project::is_projected(p)) {
                        return Err(SpineError::refuse(Witness::UnimplementedForm {
                            form: PROJECTED_PATTERN_FORM.to_string(),
                        }));
                    }
                }
                match_hop(snapshots, hop, params, fuel, &mut set, &mut trace)?
            }
        }
    }
    Ok((set, trace))
}

/// One `(b: pool/type { ... })` head. The staged match forms are exactly
/// two: by identifier, and by pool+type+state. Any other
/// field-map entry is `match:field-value`, staged OUT — the coverage table
/// refuses it by name before the door ever reaches here, and this arm
/// refuses it again by the same name so a direct caller cannot slip past.
///
/// Into the projected `spine` namespace exactly ONE form stages
/// in: by identifier, resolved through [`Snapshots::resolve_ghost`]. A
/// population scan of the projection is `match:projected-pattern`, staged
/// OUT and refused by name here as well as in the coverage table.
fn match_head(
    snapshots: &Snapshots<'_>,
    head: &NodePattern,
    params: &crate::envelope::Bindings,
    fuel: &mut Fuel,
) -> Result<Vec<BoundNode>> {
    if project::is_projected(&head.pool) {
        return match_ghost(snapshots, head, params, fuel);
    }
    let snap = snapshots.of(&head.pool)?;
    let mut want_id: Option<String> = None;
    let mut want_state: Option<String> = None;
    for (field, value) in &head.fields {
        let resolved = resolve(value, params)?;
        match field.as_str() {
            FIELD_ID => {
                want_id = Some(resolved.as_str().map(str::to_string).ok_or_else(|| {
                    SpineError::refuse(Witness::MalformedStatement {
                        message: format!(
                            "`id` matches a store-minted identifier, which is text; `{resolved}` \
                             is not"
                        ),
                        offset: 0,
                    })
                })?);
            }
            FIELD_STATE => {
                want_state = Some(resolved.as_str().map(str::to_string).ok_or_else(|| {
                    SpineError::refuse(Witness::MalformedStatement {
                        message: format!(
                            "`state` matches a value from the type's closed vocabulary, which is \
                             text; `{resolved}` is not"
                        ),
                        offset: 0,
                    })
                })?);
            }
            _ => {
                return Err(SpineError::refuse(Witness::UnimplementedForm {
                    form: crate::ladle::compile::FIELD_VALUE_FORM.to_string(),
                }))
            }
        }
    }

    let mut out = Vec::new();
    match want_id {
        // By identifier: one node, addressed. A head that names `id` never
        // scans the population (which is why `claim_footprint` gives it no
        // type-slice read).
        Some(id) => {
            fuel.spend(FUEL_PER_ROW)?;
            if let Some(node) = snap.node(&NodeId(id))? {
                if node.node_type == head.node_type
                    && want_state.as_deref().is_none_or(|s| node.state.as_deref() == Some(s))
                {
                    out.push(BoundNode {
                        pool: head.pool.clone(),
                        node,
                    });
                }
            }
        }
        // By pool + type + state: the population, in identifier order, so a
        // matcher's row order is deterministic and a RETURN over it is too.
        None => {
            for node in snap.nodes_of(&head.node_type, want_state.as_deref())? {
                fuel.spend(FUEL_PER_ROW)?;
                out.push(BoundNode {
                    pool: head.pool.clone(),
                    node,
                });
            }
        }
    }
    Ok(out)
}

/// The by-identifier match into the projected namespace (a projected node
/// is a stable identifier in a reserved namespace, resolved against the
/// tables at read time). The head must carry `id`
/// (anything else is the staged-out pattern form); `state` may narrow it,
/// exactly as for a real pool — ON THE ORDINARY DOOR: under a tranche a
/// `state` entry on a projected head is refused by
/// `ladle::compile::check_projected_under_tranche` before this matcher runs,
/// because the spine's own flips move a ghost's state and no lock holds it
/// still; the type must be the projection's one type
/// or the head binds nothing, exactly as a mistyped `id` match in a real
/// pool binds nothing. An identifier naming no row binds NOTHING — the
/// empty match — and it is the WRITE DOOR's edge-target guard
/// (`project::guard_edge_target`), not this matcher, that turns "nothing
/// there" into `GhostTargetUnknown` when an edge is aimed at it.
fn match_ghost(
    snapshots: &Snapshots<'_>,
    head: &NodePattern,
    params: &crate::envelope::Bindings,
    fuel: &mut Fuel,
) -> Result<Vec<BoundNode>> {
    let mut want_id: Option<String> = None;
    let mut want_state: Option<String> = None;
    for (field, value) in &head.fields {
        let resolved = resolve(value, params)?;
        let text = resolved.as_str().map(str::to_string).ok_or_else(|| {
            SpineError::refuse(Witness::MalformedStatement {
                message: format!(
                    "`{field}` on a projected head matches text (a transaction hash for `id`, \
                     a lifecycle state for `state`); `{resolved}` is not text"
                ),
                offset: 0,
            })
        })?;
        match field.as_str() {
            FIELD_ID => want_id = Some(text),
            FIELD_STATE => want_state = Some(text),
            _ => {
                return Err(SpineError::refuse(Witness::UnimplementedForm {
                    form: PROJECTED_PATTERN_FORM.to_string(),
                }))
            }
        }
    }
    let Some(id) = want_id else {
        return Err(SpineError::refuse(Witness::UnimplementedForm {
            form: PROJECTED_PATTERN_FORM.to_string(),
        }));
    };
    fuel.spend(FUEL_PER_ROW)?;
    let mut out = Vec::new();
    if let Some(node) = snapshots.resolve_ghost(&NodeId(id))? {
        if node.node_type == head.node_type
            && want_state.as_deref().is_none_or(|s| node.state.as_deref() == Some(s))
        {
            out.push(BoundNode {
                pool: head.pool.clone(),
                node,
            });
        }
    }
    Ok(out)
}

/// One single-hop pattern. A hop NARROWS BOTH ENDS to the pairs an edge of
/// this kind actually connects (frozen semantics, `crate::ladle` mod docs) —
/// so a bound end that came in with ten rows leaves with the rows that are
/// genuinely connected, and the other end likewise.
///
/// Paths longer than one hop are `match:multi-hop`, staged OUT and refused
/// by name — in the parser for the chained spelling, in the
/// coverage table for the two-clause spelling.
fn match_hop(
    snapshots: &Snapshots<'_>,
    hop: &HopPattern,
    params: &crate::envelope::Bindings,
    fuel: &mut Fuel,
    set: &mut MatchSet,
    trace: &mut Vec<HopTrace>,
) -> Result<()> {
    let left = end_rows(snapshots, &hop.left, params, fuel, set)?;
    let right = end_rows(snapshots, &hop.right, params, fuel, set)?;
    // The edge runs from the tail of the arrow to its head.
    let (src, dst) = match hop.arrow {
        ArrowDir::LeftToRight => (left, right),
        ArrowDir::RightToLeft => (right, left),
    };
    let (src_binding, dst_binding) = match hop.arrow {
        ArrowDir::LeftToRight => (end_binding(&hop.left), end_binding(&hop.right)),
        ArrowDir::RightToLeft => (end_binding(&hop.right), end_binding(&hop.left)),
    };
    // What this hop traverses, BEFORE narrowing (see `HopTrace`).
    trace.push(HopTrace {
        kind: hop.kind.clone(),
        src: src.iter().map(|n| (n.pool.clone(), n.node.id.clone())).collect(),
        dst: dst.iter().map(|n| (n.pool.clone(), n.node.id.clone())).collect(),
    });

    // The far ends this hop may reach, keyed by their whole pool-qualified
    // identity — an edge stored in one pool may point into another (e.g.
    // `catalogues`), so the pool is part of the key, never assumed.
    let reachable: BTreeSet<(PoolName, String)> = dst
        .iter()
        .map(|n| (n.pool.clone(), n.node.id.0.clone()))
        .collect();

    let mut kept_src: Vec<BoundNode> = Vec::new();
    let mut kept_dst_keys: BTreeSet<(PoolName, String)> = BTreeSet::new();
    for s in &src {
        fuel.spend(FUEL_PER_ROW)?;
        let snap = snapshots.of(&s.pool)?;
        let mut connected = false;
        for edge in snap.edges_from(&s.node.id, Some(&hop.kind))? {
            fuel.spend(FUEL_PER_EVAL)?;
            let key = (edge.dst.pool.clone(), edge.dst.id.0.clone());
            if reachable.contains(&key) {
                connected = true;
                kept_dst_keys.insert(key);
            }
        }
        if connected {
            kept_src.push(s.clone());
        }
    }
    let kept_dst: Vec<BoundNode> = dst
        .into_iter()
        .filter(|n| kept_dst_keys.contains(&(n.pool.clone(), n.node.id.0.clone())))
        .collect();

    set.bindings.insert(src_binding.0.clone(), kept_src);
    set.bindings.insert(dst_binding.0.clone(), kept_dst);
    Ok(())
}

/// The rows one end of a hop offers: a fresh head matches now; a bound end
/// contributes what an earlier clause already bound.
fn end_rows(
    snapshots: &Snapshots<'_>,
    end: &PatternEnd,
    params: &crate::envelope::Bindings,
    fuel: &mut Fuel,
    set: &MatchSet,
) -> Result<Vec<BoundNode>> {
    match end {
        PatternEnd::Head(head) => match_head(snapshots, head, params, fuel),
        PatternEnd::Bound(b) => set.bindings.get(&b.0).cloned().ok_or_else(|| {
            SpineError::refuse(Witness::MalformedStatement {
                message: format!(
                    "`{b}` is used as a bound hop end before any clause bound it (clause order is \
                     fixed — frozen grammar)"
                ),
                offset: 0,
            })
        }),
    }
}

fn end_binding(end: &PatternEnd) -> &Binding {
    match end {
        PatternEnd::Head(h) => &h.binding,
        PatternEnd::Bound(b) => b,
    }
}

fn resolve(v: &ValueExpr, params: &crate::envelope::Bindings) -> Result<serde_json::Value> {
    match v {
        ValueExpr::Literal(x) => Ok(x.clone()),
        ValueExpr::Param(p) => params
            .get(p)
            .cloned()
            .ok_or_else(|| SpineError::refuse(Witness::MissingParameter { name: p.clone() })),
    }
}

/// What matching produced: for each binding, the set of nodes it bound
/// (each matched thing gets a name later clauses use). The
/// empty set is a SUCCESSFUL match.
#[derive(Debug, Clone, Default)]
pub struct MatchSet {
    pub bindings: BTreeMap<String, Vec<BoundNode>>,
}

impl MatchSet {
    /// Which binding, if any, bound this pool-qualified node — how a premise
    /// re-validated under locks (which knows only identifiers) names the
    /// binding its witness owes.
    pub fn binding_of(&self, pool: &PoolName, id: &NodeId) -> Option<&str> {
        for (name, rows) in &self.bindings {
            if rows.iter().any(|n| n.pool == *pool && n.node.id == *id) {
                return Some(name.as_str());
            }
        }
        None
    }

    /// A node type resolver over everything the match bound, for the ONE
    /// grammar validator (`PoolGrammar::validate_part`) at compile time —
    /// the same closure shape the landing uses against the `nodes` table.
    pub fn type_resolver(&self, pool: &PoolName) -> BTreeMap<NodeId, String> {
        let mut out = BTreeMap::new();
        for rows in self.bindings.values() {
            for n in rows {
                if n.pool == *pool {
                    out.insert(n.node.id.clone(), n.node.node_type.clone());
                }
            }
        }
        out
    }
}

/// One node a binding bound, with the pool it lives in (bindings are
/// pool-qualified by their pattern heads).
#[derive(Debug, Clone)]
pub struct BoundNode {
    pub pool: PoolName,
    pub node: NodeRecord,
}

/// The read-side entry the door's bulk-read path calls (bulk read as the
/// statement's degenerate read form): MATCH
/// plus WHERE plus RETURN, executed as a read transaction — lock-free,
/// holding nothing, blocking nobody, retrying on the rare mid-read commit,
/// escalating after the bound.
///
/// Reading INTO the projected `spine` namespace (it resolves transparently
/// on read and refuses on write):
/// MATCH BY IDENTIFIER resolves the ghost through the one matcher
/// (`run_match` — `Snapshots::resolve_ghost`), and is EXCLUDED from the
/// validated read's pool set — it takes no snapshot, no claim, and no part in
/// the window check, because nothing can ever write the projection. Every
/// other match form into it is `match:projected-pattern`, staged OUT and
/// refused by name (the coverage table, before this function runs). The
/// gate that used to stand here, `read:projected-namespace`, is retired.
pub fn bulk_read(
    cauldron: &Cauldron,
    stmt: &Statement,
    params: &crate::envelope::Bindings,
    config: &SpineConfig,
) -> Result<ValidatedResult<crate::Returned>> {
    if !stmt.is_read_only() {
        return Err(SpineError::usage(
            "the bulk-read form is MATCH plus WHERE plus RETURN; a statement with \
             mutation clauses goes through the write door, which stamps provenance and lands a row",
        ));
    }
    let lookup = cauldron.grammar_lookup();
    let footprint = crate::ladle::compile::footprint(stmt);
    let pools: Vec<PoolName> = footprint
        .pools
        .iter()
        .filter(|p| !project::is_projected(p))
        .cloned()
        .collect();
    // WHERE is a premise even on the read path, so the read's own claim
    // footprint is the statement's (populations the pattern scans).
    let claims = crate::ladle::compile::claim_footprint(stmt, &lookup)?;

    let ceiling = config.fuel_ceiling;
    // The escalation that COMPLETES: the value carries the body
    // claims of the match it was projected from, so the escalation can
    // enrol them at its fresh number and re-match under them.
    let bodies_of = |v: &(crate::Returned, ClaimSet)| v.1.clone();
    let result = run_validated_claiming(cauldron, &pools, config, &claims, &bodies_of, |snaps| {
        // A fresh tank per ATTEMPT: the ceiling
        // bounds the cost of ONE evaluation, not of a retry storm, so a
        // contended read fails as `Busy` — which it is — instead of as a
        // fuel error, which it is not.
        let mut fuel = Fuel::new(ceiling);
        let matched = run_match(snaps, stmt, params, &mut fuel)?;
        let bodies = body_claims(&matched);
        // WHERE and EXPECT are PREMISES on the read path too, and they run
        // through the SAME evaluator the write path uses — `compile` over a
        // statement with no mutation clauses emits no deltas and no minted
        // identifiers, and does nothing but check the premises. One
        // statement means one thing in both directions (one
        // grammar, one matcher), and there is exactly one condition
        // evaluator in this crate rather than a read-side copy to drift.
        let null_tx = TxHash([0u8; 32]);
        let kinds = cauldron.kind_lookup();
        let mut ctx = crate::ladle::compile::CompileCtx {
            grammars: &lookup,
            pools_of_kind: &kinds,
            fuel: &mut fuel,
        };
        let payload =
            crate::ladle::compile::compile(stmt, params, &matched, &null_tx, &mut ctx)?;
        let rows =
            crate::ladle::compile::project_returns(stmt, params, &matched, &payload, &mut fuel)?;
        Ok((rows, bodies))
    })?;
    Ok(ValidatedResult {
        value: result.value.0,
        retries: result.retries,
        restarts: result.restarts,
        escalation: result.escalation,
    })
}

/// [`crate::Cauldron::read`]'s body: the read door's own gates, then
/// [`bulk_read`]. Reads carry NO provenance and land no row on the warm path
/// (by design), so the envelope's provenance gate does not apply and
/// the version gate is applied here directly.
pub fn read_request(cauldron: &Cauldron, request: &crate::ReadRequest) -> Result<crate::ReadOutcome> {
    if request.format_version != crate::envelope::ENVELOPE_FORMAT_VERSION {
        return Err(SpineError::refuse(Witness::VersionMismatch {
            envelope_version: request.format_version,
            supported_version: crate::envelope::ENVELOPE_FORMAT_VERSION,
        }));
    }
    let stmt = crate::ladle::parse::parse(&request.statement)?;
    crate::ladle::compile::check_coverage(&stmt)?;
    crate::ladle::compile::check_params(&stmt, &request.bindings)?;
    // The closed state vocabulary is closed for readers exactly as for
    // writers.
    crate::ladle::compile::check_match_states(&stmt, &request.bindings, &cauldron.grammar_lookup())?;
    let result = bulk_read(cauldron, &stmt, &request.bindings, cauldron.config())?;
    Ok(crate::ReadOutcome {
        rows: result.value,
        retries: result.retries,
        restarts: result.restarts,
        escalated: result.escalation != Escalation::None,
    })
}

/// A read under key's answer: the rows, how many complete
/// claim sets the re-match abandoned, and how many staged children the view
/// included. `retries` is always 0 and `escalated` always true on the wire
/// (a read under key is always escalated); the door writes those constants.
#[derive(Debug, Clone)]
pub struct TrancheRead {
    pub rows: crate::Returned,
    pub restarts: u32,
    pub children_seen: u32,
}

/// READ UNDER KEY — the body `Cauldron::tranche_read`
/// runs under the tranche's mutex. The ordinary read's gates, then: the
/// match against the VIEW; the read's claims (the read side — the static
/// population reads, read on every bound body, the by-identifier heads that
/// bound nothing, the traversed edge slices) enrolled under the TRANCHE's
/// `seq` through `acquire` (the caller's `locks::acquire_for_tranche`,
/// which also refreshes the heartbeat); the match re-run under the grant;
/// if the claim set it derives differs, the larger set is enrolled and the
/// read runs again, bounded by `restart_bound`, refusing `Busy` past it.
/// The rows answered are the ones read under the final grant. No row is
/// appended and nothing is released: the anchor is the row these claims
/// belong to, and they hold until the tranche's end.
///
/// Why compare DERIVED CLAIM SETS and not the rows: the claims are a
/// function of exactly what the match read (its bodies, its populations,
/// its traversals); equal sets under the grant mean nothing the read
/// depends on is unheld — the same rule `escalated_claiming` trusts.
pub(crate) fn read_under(
    cauldron: &Cauldron,
    view: &View,
    request: &crate::ReadRequest,
    acquire: &mut dyn FnMut(&ClaimSet) -> Result<()>,
) -> Result<TrancheRead> {
    if request.format_version != crate::envelope::ENVELOPE_FORMAT_VERSION {
        return Err(SpineError::refuse(Witness::VersionMismatch {
            envelope_version: request.format_version,
            supported_version: crate::envelope::ENVELOPE_FORMAT_VERSION,
        }));
    }
    let stmt = crate::ladle::parse::parse(&request.statement)?;
    crate::ladle::compile::check_coverage(&stmt)?;
    // The TRANCHE-ONLY projected check (the projected namespace under a
    // tranche): the same check a child runs at staging — a `state` entry on a projected head, a `WHERE` or
    // `EXPECT` over a projected binding, a `RETURN` of a projected field
    // other than `id` are refused by the frozen name before any match.
    crate::ladle::compile::check_projected_under_tranche(&stmt)?;
    crate::ladle::compile::check_params(&stmt, &request.bindings)?;
    let lookup = cauldron.grammar_lookup();
    crate::ladle::compile::check_match_states(&stmt, &request.bindings, &lookup)?;
    // ...and every projected head must name a row that EXISTS (the
    // absent-row half of the same closure; `ghost_target_unknown`).
    crate::exec::guard_projected_heads(&cauldron.spine()?, &stmt, &request.bindings)?;
    if !stmt.is_read_only() {
        return Err(SpineError::usage(
            "a read under key is MATCH plus WHERE plus RETURN; a statement with \
             mutation clauses is a child, submitted through `tranche_stage`",
        ));
    }
    let config = cauldron.config();
    let params = &request.bindings;
    let footprint = crate::ladle::compile::footprint(&stmt);
    let pools: Vec<PoolName> = footprint
        .pools
        .iter()
        .filter(|p| !project::is_projected(p))
        .cloned()
        .collect();
    let static_claims = crate::ladle::compile::claim_footprint(&stmt, &lookup)?;

    // One match against the view, with the claims it implies.
    let match_once = |fuel: &mut Fuel| -> Result<(MatchSet, ClaimSet)> {
        let open = open_pools(cauldron, &pools)?;
        let snaps = open_snapshots_over(&open, view)?;
        let (matched, trace) = run_match_traced(&snaps, &stmt, params, fuel)?;
        let mut claims = static_claims.clone();
        for c in body_claims(&matched).iter() {
            claims.insert(c.clone());
        }
        for c in crate::ladle::compile::tranche_read_claims(&stmt, params, &trace)?.iter() {
            claims.insert(c.clone());
        }
        Ok((matched, claims))
    };

    let mut fuel = Fuel::new(config.fuel_ceiling);
    let (_, mut claims) = match_once(&mut fuel)?;
    let mut restarts: u32 = 0;
    loop {
        // Enrol under the tranche's number, granted all-or-nothing.
        acquire(&claims)?;
        // The match re-run UNDER THE GRANT. A fresh tank per attempt, as the
        // ordinary read does: the ceiling bounds one evaluation.
        let mut fuel = Fuel::new(config.fuel_ceiling);
        let (matched, found) = match_once(&mut fuel)?;
        if found == claims {
            // Premises (WHERE, EXPECT) and the projection, through the one
            // evaluator the ordinary read uses (`bulk_read`).
            let null_tx = TxHash([0u8; 32]);
            let kinds = cauldron.kind_lookup();
            let mut ctx = crate::ladle::compile::CompileCtx {
                grammars: &lookup,
                pools_of_kind: &kinds,
                fuel: &mut fuel,
            };
            let payload =
                crate::ladle::compile::compile(&stmt, params, &matched, &null_tx, &mut ctx)?;
            let rows =
                crate::ladle::compile::project_returns(&stmt, params, &matched, &payload, &mut fuel)?;
            return Ok(TrancheRead {
                rows,
                restarts,
                children_seen: view.children_seen(),
            });
        }
        restarts += 1;
        if restarts > config.restart_bound {
            return Err(SpineError::Busy {
                message: format!(
                    "read under key gave up after {restarts} restarts: the match set moved under \
                     every complete claim set — the lock design's starvation residual, bounded by \
                     restart_bound {}; pools: {}",
                    config.restart_bound,
                    pools.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(", ")
                ),
            });
        }
        // The LARGER set (what was granted stays granted under the tranche's
        // seq; growth) — enrol the difference and re-run.
        claims = found;
    }
}

/// One transaction hash surfaced to read validation callers.
pub type CommitId = TxHash;
