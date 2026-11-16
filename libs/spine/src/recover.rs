//! Recovery — reads ONLY the spine row, never the pool files, to decide
//! whether a transaction happened (a complete design shaped as a
//! generalization of the settled event model, so there is one commit-path
//! mechanism).
//!
//! The procedure: row not decided — ABORT (nothing landed, by
//! the per-pool atomic step; the row remains as the record of the attempt).
//! Row decided — roll the un-landed parts FORWARD from the row's own
//! payload, NEVER from the statement text (re-matching after a crash could
//! match different rows and silently do different work). Row cleared —
//! done. There is NO roll-back of landed parts, ever — that would be a
//! second write path into pools.
//!
//! # The RETURN projection of a recovered commit
//!
//! After every part has landed and BEFORE the cleared flip, recovery
//! projects RETURN exactly as the live door does — through the one
//! projection, `exec::project_landed`, from the row's own record: the
//! statement text (PARSED, which is deterministic and is not matching),
//! its parameters, the compiled payload, and the bound identities the door
//! recorded beside the row (which nodes each MATCH binding bound). It runs
//! under the row's own locks, which recovery still holds at that point
//! because release is at the cleared flip only, so the
//! projection is taken from exactly the state the live path would have
//! seen. The row's `result` column therefore holds a projection, never a
//! placeholder; the fact that it was recovered is the row's `recovered`
//! audit mark, set in the same step as the cleared flip
//! (`SpineDb::mark_cleared_recovered`). A duplicate of the commit is then
//! answered with THE ORIGINAL OUTCOME from the record alone, so the
//! refusal that used to stand here for that case is gone.
//!
//! What cannot be projected is refused LOUDLY, by name, and the row stays
//! decided-and-un-cleared with its locks held — the fail-closed posture
//! for a record that has been altered: a decided row's text
//! that does not parse (the door parsed it before the append, so it has
//! been changed since), a matched binding the record does not carry, or a
//! bound node that no longer reads. Nothing is fabricated and nothing is
//! re-matched; repair is out-of-band.
//!
//! # Several attempts per identity (the attempt-aware record)
//!
//! One hash identity may name SEVERAL rows — one per attempt
//! (see `spine_db::SPINE_DDL`). Nothing here keys on the hash, and that is
//! deliberate: **recovery is per ROW**, always. The worklist is
//! `pending()` — un-cleared rows in sequence order — and every decision
//! below is taken from the row in hand: its own `decided` flag, its own
//! payload, its own per-part landed flags, its own sequence number.
//!
//! Two structural facts make that sound, and both are enforced in
//! `spine_db.rs` rather than assumed here. At most ONE attempt per identity
//! may be un-cleared at a time (`idx_tx_live`), so the worklist can never
//! hold two rows for one identity. And at most ONE attempt per identity may
//! EVER be decided (`idx_tx_committed` plus `mark_decided`'s named guard),
//! so only one attempt can ever have landed anything — which is what makes
//! the pool-side `applied_parts` marker, keyed `(hash, part)`, still exact.
//! An ABORTED attempt landed nothing by construction (`mark_part_landed`
//! refuses on an undecided row), so it is never rolled forward and its
//! `hash` never collides with the landing attempt's marker.

use crate::envelope::TxHash;
use crate::error::{Result, SpineError};
use crate::exec;
use crate::ladle::compile::Fuel;
use crate::locks;
use crate::spine_db::{Quiescer, SpineDb, TxRow};
use crate::{Cauldron, Returned};
use serde::{Deserialize, Serialize};

/// THE STAGE VOCABULARY of the commit path — owned by recovery, borrowed
/// VERBATIM by the fault seam (a hard condition:
/// two descriptions of the commit path would drift exactly where atomicity
/// is being proven). The four named stages:
/// after the row append; after the decided flip; between one pool's landing
/// and the next; before the cleared flip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    RowAppended,
    Decided,
    BetweenLandings,
    BeforeCleared,
}

impl Stage {
    /// THE ONE LIST of the stages, in pipeline order. Everything that needs
    /// to enumerate them derives from this — [`Stage::parse`] below, and the
    /// fault seam's "names no commit stage" refusal (`fault::no_such_stage`)
    /// — so no second list of the four names exists anywhere, in code or in
    /// a message (the seam's message used to carry a hand-typed copy).
    pub const ALL: [Stage; 4] = [
        Stage::RowAppended,
        Stage::Decided,
        Stage::BetweenLandings,
        Stage::BeforeCleared,
    ];

    /// The frozen text names (`SPINE_FAULT` values, serialized forms):
    /// `row-appended`, `decided`, `between-landings`, `before-cleared`. The
    /// only place a stage is spelled.
    pub fn name(&self) -> &'static str {
        match self {
            Stage::RowAppended => "row-appended",
            Stage::Decided => "decided",
            Stage::BetweenLandings => "between-landings",
            Stage::BeforeCleared => "before-cleared",
        }
    }

    /// Parse a frozen text name; `None` for anything else (a misspelled
    /// stage is never silently no stage — callers refuse loudly). Derived
    /// from [`Stage::ALL`] and [`Stage::name`] rather than a second match
    /// over the strings.
    pub fn parse(s: &str) -> Option<Stage> {
        Stage::ALL.iter().copied().find(|stage| stage.name() == s)
    }
}

/// Run recovery over every un-cleared row, in sequence order.
/// Idempotent: run twice, same result (pool-side
/// `applied_parts` markers make roll-forward re-application detectable).
/// Called by [`Cauldron::recover`]; also the crash-restart entry.
///
/// # Beside LIVE doors
///
/// Nothing on a row says "a process is inside this commit right now", so
/// this pass cannot tell a live row from a dead one and does not try to. It
/// is SAFE beside live doors by ARBITRATION instead: every flip it makes is
/// conditional and every lock release follows only a flip it won. The
/// decided arm lands parts through the pool-side idempotence marker (once,
/// whoever lands them), then the cleared-flip arbiter decides who stores the
/// outcome — the live door, losing, returns the stored outcome as its own.
/// The un-decided arm ABORTS every un-decided row it meets, a live one
/// included: that door then loses its decided flip, releases what it
/// enrolled, and RESTARTS with a fresh row (`exec`'s aborted-underfoot arm)
/// — one spurious attempt, recorded aborted like any restart, nothing landed
/// twice, nothing blamed on the caller. The price of running this beside
/// traffic is that spurious restart; whether a liveness mark should let the
/// un-decided arm skip a live row instead is an open design question.
pub fn recover(cauldron: &Cauldron) -> Result<RecoveryReport> {
    recover_pass(cauldron, TrancheRows::Skip)
}

/// Whether the ordinary pass knows a tranche row when it sees one. `Skip`
/// on every production path; `Unknown` exists only so the test-support
/// "older binary" simulation (`Cauldron::open_as_older_binary`) can run the pass an older binary WOULD run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrancheRows {
    Skip,
    #[cfg(feature = "test-support")]
    Unknown,
}

pub(crate) fn recover_pass(cauldron: &Cauldron, tranche_rows: TrancheRows) -> Result<RecoveryReport> {
    let mut spine = cauldron.spine()?;
    let mut report = RecoveryReport::default();
    // ORPHAN-LOCK SWEEP first. Locks may only be held by
    // un-cleared rows — that is the release rule's own invariant — but a crash in the instant between a row's cleared flip and
    // its lock release (either the commit path's or the abort path's) leaves
    // lock rows behind that no pending-row walk can ever reach, because the
    // row they belong to is already cleared and off the worklist. Waiters
    // would then block on a ghost until the wait ceiling. One sweep restores
    // the invariant; it touches only locks whose row is cleared, so it can
    // never strip a live transaction.
    locks::release_cleared_orphans(&spine)?;
    // The worklist is EVERY un-cleared row, in SEQUENCE order — the spine's
    // own insertion order, never the wall clock. Read once: a
    // row that arrives after this read belongs to a live door, not to
    // recovery.
    for row in spine.pending()? {
        // THE TRANCHE SKIP: a row whose
        // `tranche_id` is set is never the ordinary pass's — not to abort
        // (the anchor is un-decided by design), not to roll forward.
        if row.tranche.is_some() && tranche_rows == TrancheRows::Skip {
            report.skipped_tranche.push(row.hash);
            continue;
        }
        if !row.decided {
            // NOT DECIDED -> ABORT, through the atomic arbiter
            // (`mark_aborted_if_undecided`): the flip happens only if the
            // row is STILL un-decided at the flip, and locks release only
            // behind a won flip. Nothing landed, guaranteed by the per-pool
            // atomic step: no part is half-applied, and no part can have
            // landed at all, because `mark_part_landed` refuses on an
            // undecided row (the lifecycle guard). The row REMAINS
            // forever as the record of the aborted attempt.
            if abort_row(&mut spine, &row)? {
                report.aborted.push(row.hash);
                continue;
            }
            // LOST THE RACE — and a lost abort flip has TWO possible winners,
            // told apart by the row as it stands now:
            //
            // - DECIDED: a live door decided this row between the worklist
            //   read and the abort. It is that door's to land and clear;
            //   stripping its locks here is exactly the leak the arbiter
            //   exists to prevent. Skip it — if that door dies, a later
            //   recovery pass finds the row decided and rolls it forward.
            // - CLEARED AND UN-DECIDED: another ABORTER got there first — a
            //   second `recover()` over the same crashed row (two processes
            //   restarting at once), or the row's own door restarting it.
            //   The row is aborted, nothing landed; this pass
            //   wrote nothing and says so, rather than reporting a live door
            //   that does not exist — the same refinement the `recovered` mark
            //   gives the decided arm.
            let now = spine.row_at(row.seq)?;
            if now.decided {
                report.skipped_live.push(row.hash);
            } else if now.cleared {
                report.aborted_elsewhere.push(row.hash);
            } else {
                return Err(SpineError::hard(format!(
                    "SPINE INTERNAL INVARIANT VIOLATED: the abort arbiter lost on seq {} and the \
                     row is still un-decided and un-cleared; a conditional flip that changes \
                     nothing cannot lose",
                    row.seq
                )));
            }
            continue;
        }
        // DECIDED -> ROLL THE UN-LANDED PARTS FORWARD, from the ROW'S OWN
        // COMPILED PAYLOAD and never from the statement text (re-matching
        // after a crash could match different rows and silently
        // do different work). Nothing here parses `row.statement`; nothing
        // here opens a matcher. The text is provenance, not instruction.
        let mut rolled = false;
        for (ordinal, part) in row.payload.parts.iter().enumerate() {
            let ordinal = u32::try_from(ordinal).map_err(|_| {
                SpineError::hard(format!(
                    "transaction {} carries more parts than a part ordinal can name",
                    row.hash
                ))
            })?;
            let already = row
                .parts
                .iter()
                .find(|p| p.part == ordinal)
                .map(|p| p.landed)
                .unwrap_or(false);
            if !already {
                let mut pool = cauldron.open_pool(&part.pool)?;
                // Idempotent by the pool-side `applied_parts` marker written
                // inside the same landing transaction: a part that reached
                // the pool but whose spine flag never landed is DETECTED,
                // not re-applied.
                pool.apply_part(&row.hash, ordinal, part)?;
                drop(pool);
                rolled = true;
            }
            spine.mark_part_landed(row.seq, ordinal)?;
        }
        // EVERY PART HAS LANDED. Now the RETURN projection, from the row's
        // own record, under the row's own locks (still held — release is at
        // the cleared flip only), through the ONE projection the live door
        // runs. Then the cleared flip, which stores the projection and sets
        // the `recovered` audit mark in one atomic step.
        let returned = project_recovered(cauldron, &spine, &row)?;
        let result = serde_json::to_value(&returned).map_err(|e| {
            SpineError::hard(format!(
                "the RETURN projection recovery computed for seq {} will not serialize: {e}",
                row.seq
            ))
        })?;
        // THE CLEARED-FLIP ARBITER: the flip is
        // conditional, and the locks release only behind a flip recovery
        // WON. `false` means the LIVE DOOR that owns this row cleared it
        // between the worklist read and this line — its stored outcome and
        // its audit mark stand, its locks are its own to release, and this
        // pass touched the record of it not at all (the parts it may have
        // landed were the door's parts, landed once by the pool-side marker).
        if spine.mark_cleared_recovered(row.seq, &result)? {
            locks::release_at_clear(&spine, row.seq)?;
            if rolled {
                report.rolled_forward.push(row.hash);
            } else {
                report.cleared_only.push(row.hash);
            }
        } else if spine.row_at(row.seq)?.recovered {
            // The OTHER finisher was a recovery pass (two at once over one
            // crashed row): the row is finished, by recovery, and this pass
            // reports it so — the audit mark is set, the outcome stored, and
            // whichever of the two landed a part landed it once.
            if rolled {
                report.rolled_forward.push(row.hash);
            } else {
                report.cleared_only.push(row.hash);
            }
        } else {
            // The other finisher was the LIVE DOOR: not recovery's to report
            // as its own; the row's mark says the door did it.
            report.skipped_live.push(row.hash);
        }
    }
    Ok(report)
}

/// THE TRANCHE PASS, run by
/// [`Cauldron::open`] AFTER the ordinary pass (which skipped every row with
/// `tranche_id` set — anchor and reserved child rows alike) and
/// BEFORE the door answers anything. It reads only the spine and does
/// exactly four things, over `tranches`:
///
/// 0. **withdraws every tranche's UN-GRANTED (queued) lock rows** (lost by
///    design: an un-granted claim's queued rows are swept; the child
///    that was waiting was never staged and is re-submitted). A child killed mid-wait leaves
///    `granted = 0` rows under the anchor's `seq`; the ordinary pass never touches a tranche
///    row and the orphan sweep only `cleared = 1` ones, so without this an
///    earlier-numbered queued row blocks every ordinary writer on that
///    resource for the tranche's whole life. GRANTED rows stay: they are
///    what the tranche holds. A waiter still live in ANOTHER process
///    (an open beside live doors, `Cauldron::open`'s docs) loses only its
///    queue position: its next poll upserts the grant (`locks::poll_once`),
///    so no exclusivity is lost — the same arbitration posture as the
///    ordinary pass's spurious restart. Ordinary queued rows are NOT this
///    pass's: their crashed owners are un-decided and the ordinary pass
///    aborts them and releases every row they hold;
/// 1. **resets every non-terminal deadline** to `boot + timeout` (across a
///    daemon restart; `landing` included),
///    `last_heartbeat` untouched;
/// 2. **rebuilds every `open`/`staging` tranche's overlay** by replaying its
///    `tranche_children` rows in ordinal order into the
///    per-tranche mutex slot, through the one rebuild — a torn record
///    refuses the open by name;
/// 3. **recomputes positions** — derived from the anchor `seq`s,
///    never stored, so "recompute" is a read, reported here.
///
/// **It lands nothing.** No lander is spawned, no pool file is written, no
/// `transactions` row flips: every landing, decided or not, is the
/// scheduler's lander from its first `tranche_tick`,
/// so there is ONE actor that lands, live or after a restart. `draining` is
/// preserved: a `whole` head left `queued, draining = 1` keeps
/// the gate up from the first door request after boot; the ordinary pass
/// has already released the crashed writers' locks, so the first tick
/// finds `drained` empty and opens it. A death inside the tranche-commit
/// transaction needs nothing from this pass: the transaction never
/// committed and the tranche is as before.
pub(crate) fn tranche_pass(cauldron: &Cauldron, boot_ms: i64) -> Result<TranchePassReport> {
    // The `boot + timeout` reset below covers any halt a
    // previous handle on this spine saw; this process re-learns a standing
    // halt from its own ticks and refused acts (`tranche::halts_seen`).
    crate::tranche::forget_halt_seen(cauldron)?;
    let queued_rows_withdrawn = crate::tranche::withdraw_queued_at_open(cauldron)?;
    let deadlines_reset = crate::tranche::reset_deadlines_at_open(cauldron, boot_ms)?;
    let tranches = crate::tranche::rebuild_views_at_open(cauldron)?;
    Ok(TranchePassReport {
        boot_ms,
        queued_rows_withdrawn,
        deadlines_reset,
        tranches,
    })
}

/// What the tranche pass found and did at open — kept on the
/// handle as [`Cauldron::tranches_at_open`], so a test across a real
/// process death reads the pass's own account beside the record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranchePassReport {
    /// The `boot` the deadlines were reset from.
    pub boot_ms: i64,
    /// Un-granted (queued) lock rows under a tranche anchor's `seq` the pass
    /// withdrew (an un-granted claim's queued rows are swept).
    pub queued_rows_withdrawn: usize,
    /// Non-terminal rows whose deadline became `boot + timeout`.
    pub deadlines_reset: usize,
    /// Every non-terminal tranche, in queue order.
    pub tranches: Vec<crate::tranche::RecoveredTranche>,
}

/// The RETURN projection of a recovered row (module docs), from the
/// row alone, refusing loudly on anything it cannot project.
///
/// PARSING IS NOT MATCHING. The statement text is parsed here — a pure,
/// deterministic function of the text — because the projection needs the
/// RETURN clause's shape and the CREATE heads' values; nothing here opens a
/// matcher, and the identities the projection reads come from the row's
/// recorded bound identities and minted identities, never from the text.
/// Re-matching after a crash could match different rows and
/// silently do different work. That is why the text is read and never
/// matched.
///
/// Every failure is a named `Hard` carrying the sequence number and the
/// hash, and it is raised AFTER the parts have landed and BEFORE the
/// cleared flip: the landing was the commit and depended on the text not
/// at all; the projection is the reply and cannot be fabricated. The row
/// stays decided and un-cleared, holding its locks, until the record is
/// repaired out-of-band — every later pass refuses the same way.
fn project_recovered(cauldron: &Cauldron, spine: &SpineDb, row: &TxRow) -> Result<Returned> {
    let stmt = crate::ladle::parse::parse(&row.statement).map_err(|e| {
        SpineError::hard(format!(
            "recovery cannot project RETURN for seq {} (transaction {}): the statement text on \
             this DECIDED row does not parse ({}). The door parsed it before the row was \
             appended, so the record has been altered since; recovery will not re-match and \
             will not fabricate a projection. Every part has landed; the row stays un-cleared \
             with its locks held until the record is repaired out-of-band \
             (fail-closed)",
            row.seq,
            row.hash,
            e.message()
        ))
    })?;
    let bound = exec::read_bound(spine, row.seq)?.unwrap_or_default();
    let mut fuel = Fuel::new(cauldron.config().fuel_ceiling);
    exec::project_landed(cauldron, &stmt, &row.bindings, &row.payload, &bound, &mut fuel).map_err(
        |e| {
            SpineError::hard(format!(
                "recovery cannot project RETURN for seq {} (transaction {}): {}. Every part has \
                 landed; the row stays un-cleared with its locks held until the record is \
                 repaired out-of-band (fail-closed)",
                row.seq,
                row.hash,
                e.message()
            ))
        },
    )
}

/// The ABORT of one un-decided row: the row is marked cleared
/// IF it is still un-decided — one atomic conditional flip
/// ([`SpineDb::mark_aborted_if_undecided`]) — and its locks release only
/// behind a won flip. Returns whether the abort won; `false` means someone
/// decided (or already cleared) the row first and NOTHING was touched.
///
/// REORDERED: this used to release the locks FIRST
/// and flip the row second, with no decided-check joining the two — so a
/// `recover()` racing a live door could strip the locks a decided
/// transaction was landing under and leak the half-landed window to
/// lock-takers (the release rule, broken where it is load-bearing).
/// The flip-then-release order opens a different, smaller hole — a crash
/// between the two leaves orphaned lock rows — and that hole is closed
/// structurally by [`recover`]'s orphan-lock sweep, which is why the sweep
/// exists.
///
/// INTERIOR CHOICE (both readings were open). An aborted row is CLEARED.
/// `decided = 0 AND cleared = 1` is unambiguously "this attempt aborted" to
/// any auditor, the row and its false landed flags are kept forever exactly
/// as the design requires, and — the deciding reason — it leaves the
/// un-cleared worklist. Leaving aborted rows un-cleared would make
/// `pending()` grow without bound, make every later recovery re-walk and
/// re-report every abort that ever happened, and make "run recovery twice,
/// same result" false in the only place it is observable.
///
/// VISIBILITY (the one-door law): `pub` only under `test-support`.
/// From outside the crate this aborts SOMEBODY ELSE'S live attempt — the
/// owning door would then find its row cleared under it.
/// Crate-private for every consumer; identical behavior.
#[cfg(feature = "test-support")]
pub fn abort_row(spine: &mut SpineDb, row: &TxRow) -> Result<bool> {
    abort_row_interior(spine, row)
}
#[cfg(not(feature = "test-support"))]
pub(crate) fn abort_row(spine: &mut SpineDb, row: &TxRow) -> Result<bool> {
    abort_row_interior(spine, row)
}

fn abort_row_interior(spine: &mut SpineDb, row: &TxRow) -> Result<bool> {
    let won = spine.mark_aborted_if_undecided(
        row.seq,
        &serde_json::json!({ "aborted": true, "landed": false }),
    )?;
    if won {
        locks::release_on_abort(spine, row.seq)?;
    }
    Ok(won)
}

/// Is this spine quiesced (the quiesce hook)? While ANY
/// quiescer is outstanding (`spine_db::SPINE_DDL`'s `quiescers` table).
pub fn is_quiesced(spine: &SpineDb) -> Result<bool> {
    Ok(spine.quiescer_count()? > 0)
}

/// What recovery did, per transaction — a report, so the kill tests assert
/// the invariant: everything landed or nothing, and the row tells which.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Rows found un-decided: aborted, BY THIS PASS; nothing had landed;
    /// row kept.
    pub aborted: Vec<TxHash>,
    /// Rows found un-decided whose abort flip this pass LOST to another
    /// aborter — a second `recover()` over the same crashed row, or the
    /// row's own door restarting it. The row is aborted and
    /// nothing landed, exactly as for `aborted`; this pass wrote none of it
    /// (these used to be reported as `skipped_live`, a live
    /// door that did not exist).
    pub aborted_elsewhere: Vec<TxHash>,
    /// Rows found decided with un-landed parts: rolled forward from the
    /// row's payload and cleared.
    pub rolled_forward: Vec<TxHash>,
    /// Rows found with every part landed but not yet cleared: cleared.
    pub cleared_only: Vec<TxHash>,
    /// Rows a LIVE DOOR finished under this pass: read un-decided and then
    /// decided by their door before the abort arbiter flipped them, or
    /// read decided and then CLEARED by their door
    /// before the cleared-flip arbiter flipped them. In
    /// neither case did this pass change the record of the row: it belongs
    /// to that door, and a later recovery pass rolls it forward if the door
    /// died. Non-empty only when `recover()` runs beside live doors.
    pub skipped_live: Vec<TxHash>,
    /// Un-cleared rows carrying a `tranche_id` — a tranche's anchor and
    /// its reserved child rows — which the ORDINARY pass never touches:
    /// the anchor is un-decided for the
    /// tranche's whole life, and aborting it would end a live tranche behind
    /// its holder's back. The tranche record is the scheduler's (and
    /// the tranche pass's).
    pub skipped_tranche: Vec<TxHash>,
}

/// QUIESCE (a hook): no new commits
/// begin; in-flight commits finish; returns when the spine has drained — so
/// a whole-cauldron backup snapshot never contains a half-landed commit.
/// Backup itself is not this crate's to build. A restore remains an
/// out-of-band event the spine cannot detect (stated, not solved).
///
/// # A quiesce has an OWNER
///
/// Everything that arbitrates is proven at N >= 2, and the second party here
/// is a SECOND quiescer or a resumer. The hook used to be one boolean with
/// no owner and no count: two backup-takers quiesced, the first resumed, and
/// a commit landed in the middle of the second's snapshot; a `resume()` from
/// anyone mid-drain was invisible to the drain loop, which still answered
/// `Ok`. Now each call records its OWN quiescer under a nonce; the cauldron
/// is quiesced while ANY quiescer is outstanding; and the drain loop
/// re-checks that its own quiescer still stands before it answers `Ok`, so
/// `Ok` means "quiesced, by you, and drained" — a postcondition a
/// backup-taker may act on.
///
/// # The owner is THE CALL, and the call holds a token
///
/// The first cut kept the nonces on the `Cauldron` handle, newest last, and
/// `resume()` popped the newest. Two things that broke:
/// the pop happened BEFORE the spine write, so a transient `Busy` on that
/// write dropped the nonce while the quiescer row stood — the cauldron
/// quiesced by a live process with no API left to lift it; and one handle
/// shared across threads (its documented use) lifted a SIBLING thread's
/// quiesce, telling the sibling it had been resumed out from under while
/// the resumer's own quiesce still stood. The handle is the wrong owner. So
/// `quiesce()` returns a [`Quiesce`] token and [`resume`] takes it by
/// reference: the token names exactly the quiescer this call recorded,
/// wherever the token travels; a failed `resume` leaves it in the caller's
/// hand; and a handle never holds anything. When the drain does NOT finish,
/// the quiescer still stands (the gate stays up so the in-flight set can
/// only shrink; silently lifting would hand back a cauldron neither drained
/// nor quiesced) and the token comes back INSIDE the error, so nothing this
/// process recorded is ever without a token — [`QuiesceError::standing`].
/// A quiescer whose PROCESS died holding its token is still lifted
/// out-of-band, exactly as a halt is repaired.
pub fn quiesce(cauldron: &Cauldron) -> std::result::Result<Quiesce, QuiesceError> {
    let mut spine = cauldron.spine().map_err(QuiesceError::before_recording)?;
    // The quiescer first: no new commit begins from the instant it is
    // recorded (the append gate counts quiescers inside its own IMMEDIATE
    // transaction), so the in-flight set can only shrink.
    let token = Quiesce {
        nonce: quiesce_nonce(),
    };
    spine
        .add_quiescer(&token.nonce, &format!("pid {}", std::process::id()))
        .map_err(QuiesceError::before_recording)?;
    let config = cauldron.config();
    let started = std::time::Instant::now();
    let mut interval = config.lock_poll_interval;
    let max_interval = config.lock_poll_interval * 32;
    loop {
        // THIS quiesce must still stand, or `Ok` would be a lie about the
        // cauldron the caller is about to snapshot. Lifted by hand mid-drain
        // means nothing of this call stands: no token comes back.
        match spine.holds_quiescer(&token.nonce) {
            Ok(true) => {}
            Ok(false) => {
                return Err(QuiesceError {
                    error: SpineError::Busy {
                        message: "this quiesce was lifted out from under its caller before the \
                                  spine drained; the cauldron is not quiesced by this call and \
                                  nothing it drained can be relied on — quiesce again"
                            .to_string(),
                    },
                    standing: None,
                });
            }
            Err(e) => return Err(QuiesceError::standing(e, token)),
        }
        // DRAINED = no row is between its append and its cleared flip. That
        // is `pending()` empty, which is stronger than `in_landing` empty:
        // an appended-but-undecided transaction is in flight too, and a
        // backup taken while one is mid-commit is exactly what this hook
        // exists to prevent.
        let outstanding = match spine.pending() {
            Ok(rows) => rows,
            Err(e) => return Err(QuiesceError::standing(e, token)),
        };
        if outstanding.is_empty() {
            return Ok(token);
        }
        if started.elapsed() >= config.lock_wait_ceiling {
            // Loud, and the quiesce STAYS SET: the caller asked for a
            // drained cauldron and did not get one, and silently resuming
            // would hand back a cauldron that is neither drained nor
            // quiesced. `resume` is the caller's to call — with the token
            // this error carries.
            // A tranche's ANCHOR row is un-cleared
            // from `begin` to the tranche's end, so a quiesce taken
            // mid-tranche times out — correct, the cauldron is not
            // quiescent — and the message NAMES the anchor's `seq` and the
            // tranche `(T, n)` it belongs to, so the user reads "a
            // tranche is open", not "a commit is stuck".
            let anchors: Vec<String> = outstanding
                .iter()
                .filter_map(|r| {
                    r.tranche.as_ref().filter(|m| m.is_anchor()).map(|m| {
                        format!(
                            "tranche {} attempt {} is open (its anchor row seq {} is un-cleared for \
                             the tranche's life — the cauldron is not quiescent while a tranche is \
                             open)",
                            m.id, m.attempt, r.seq
                        )
                    })
                })
                .collect();
            let anchors = if anchors.is_empty() {
                String::new()
            } else {
                format!("; {}", anchors.join("; "))
            };
            return Err(QuiesceError::standing(
                SpineError::Busy {
                    message: format!(
                        "quiesce did not drain within {:?}: {} transaction(s) still in flight \
                         (seq {}){anchors}; the quiescer still stands — resume() the token this \
                         error carries to lift it",
                        config.lock_wait_ceiling,
                        outstanding.len(),
                        outstanding
                            .iter()
                            .map(|r| r.seq.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                },
                token,
            ));
        }
        #[cfg(feature = "test-support")]
        drain_probe::waited(&token.nonce);
        std::thread::sleep(interval);
        interval = (interval * 2).min(max_interval);
    }
}

/// TEST-SUPPORT ONLY: how many times the drain loop of the quiesce
/// holding `nonce` has seen rows still in flight and decided to WAIT. The one
/// observable that says "this quiesce reached its drain check and did not
/// return" without a timing assumption; compiled out of every other build.
#[cfg(feature = "test-support")]
pub mod drain_probe {
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    static WAITS: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());
    pub(crate) fn waited(nonce: &str) {
        *WAITS.lock().unwrap_or_else(|e| e.into_inner()).entry(nonce.to_string()).or_default() += 1;
    }
    /// Drain-loop waits recorded for the quiesce holding `nonce` (0 if none).
    pub fn waits(nonce: &str) -> u64 {
        WAITS.lock().unwrap_or_else(|e| e.into_inner()).get(nonce).copied().unwrap_or(0)
    }
}

/// The receipt for ONE quiesce: it names exactly the quiescer the
/// [`quiesce`] call that issued it recorded, and [`resume`] lifts exactly
/// that one. Opaque, not `Clone` (one owner), and `Send` so a quiesce taken
/// on one thread can be lifted on another. Dropping it lifts nothing — the
/// quiescer stands until it is resumed (or repaired out-of-band).
#[must_use = "the quiescer stands until this token is resumed"]
#[derive(Debug)]
pub struct Quiesce {
    nonce: String,
}

/// Why a [`quiesce`] did not answer `Ok`, with whatever of it still stands.
/// `standing` is `Some` exactly when this call's quiescer is still recorded
/// (the drain did not finish within the ceiling, or a read under the drain
/// failed): the cauldron is quiesced by this call, the caller did not get a
/// drained one, and the token is how it lifts it. `None` when nothing of
/// this call stands: the quiescer could not be recorded, or it was lifted
/// by hand mid-drain.
#[derive(Debug)]
pub struct QuiesceError {
    pub error: SpineError,
    pub standing: Option<Quiesce>,
}

impl QuiesceError {
    fn before_recording(error: SpineError) -> QuiesceError {
        QuiesceError {
            error,
            standing: None,
        }
    }

    fn standing(error: SpineError, token: Quiesce) -> QuiesceError {
        QuiesceError {
            error,
            standing: Some(token),
        }
    }
}

impl From<QuiesceError> for SpineError {
    fn from(e: QuiesceError) -> SpineError {
        e.error
    }
}

impl std::fmt::Display for QuiesceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.error.kind(), self.error.message())?;
        if self.standing.is_some() {
            write!(f, " (the quiescer still stands; resume the token this error carries)")?;
        }
        Ok(())
    }
}

impl std::error::Error for QuiesceError {}

/// A nonce for one quiesce: process, clock, ticket — unique within the
/// process by the ticket and across processes by the pid.
fn quiesce_nonce() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TICKET: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "quiesce:{}:{nanos}:{}",
        std::process::id(),
        TICKET.fetch_add(1, Ordering::Relaxed)
    )
}

/// Lift the quiesce this token was issued for: `true` when it was lifted;
/// `false` when it no longer stood — lifted already, by an earlier `resume`
/// of the same token or by the out-of-band repair — so nothing was lifted
/// now, and any other party's quiesce stands. The token is taken by
/// reference: a `resume` that fails (a transient `Busy` on the spine write)
/// lifted nothing and leaves the token in the caller's hand for the next
/// try. New commits begin again only once every
/// outstanding quiescer has resumed.
pub fn resume(cauldron: &Cauldron, token: &Quiesce) -> Result<bool> {
    let mut spine = cauldron.spine()?;
    spine.remove_quiescer(&token.nonce)
}

/// THE BY-HAND LIFT: lift EVERY outstanding quiescer, whoever took it, naming each one —
/// holder and since when — in what it returns and on stderr.
///
/// It exists for exactly one case: a quiescer whose PROCESS died holding its
/// token. A [`Quiesce`] token is the only sanctioned way to lift the quiesce
/// it names, and a dead process takes its token with it; the cauldron then
/// refuses every commit, loudly, until someone lifts the pause by hand. This
/// is that hand, as a named act instead of raw SQL on the `quiescers` table.
///
/// NEVER AUTOMATIC. Nothing in the crate calls it. The two automatic shapes
/// were weighed and refused: a process-id liveness check (pid
/// reuse now, no pid at all once sharded) and a lease (a clock across
/// processes — the objection that keeps the liveness mark open). A dead
/// backup-taker's half-copied snapshot is garbage anyway, so the only harm
/// of a standing quiesce is that writes refuse loudly — the intended
/// failure direction — and the lift is a decision, not a timer.
///
/// `reason` is REQUIRED: an empty or blank one is a usage refusal, nothing
/// lifted. It is announced beside each lifted quiescer. Lifts LIVE quiescers
/// too — it cannot tell a dead taker from a live one, and does not try; a
/// live `quiesce()` mid-drain then finds its own quiescer gone and answers
/// `Busy: lifted out from under its caller`, which is the loud outcome a
/// by-hand lift under a live backup deserves. With nothing standing it
/// lifts nothing and returns an empty list: the state asked for is reached.
pub fn resume_all(cauldron: &Cauldron, reason: &str) -> Result<LiftedQuiesce> {
    if reason.trim().is_empty() {
        return Err(SpineError::usage(
            "resume_all lifts every standing quiesce by hand and needs a reason to announce \
             beside what it lifted; an empty reason lifts nothing (a named, loud act)",
        ));
    }
    let mut spine = cauldron.spine()?;
    let lifted = spine.take_all_quiescers()?;
    if lifted.is_empty() {
        eprintln!("spine: resume_all ({reason}): no quiesce stood; nothing lifted");
    }
    for q in &lifted {
        eprintln!(
            "spine: resume_all ({reason}): LIFTED BY HAND the quiesce taken by {} since {} \
             (nonce {})",
            q.holder, q.since, q.nonce
        );
    }
    Ok(LiftedQuiesce {
        reason: reason.to_string(),
        lifted,
    })
}

/// What [`resume_all`] did: the reason it was given back, and every quiescer
/// it lifted — holder and since when — in the order they were taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiftedQuiesce {
    pub reason: String,
    pub lifted: Vec<Quiescer>,
}

/// The refusal a quiesced cauldron gives a commit (both gates — the door's
/// phase 1 and the append's own IMMEDIATE transaction): `Busy`, NAMING every
/// standing quiescer by holder and since when, so a pause whose owner
/// is dead is diagnosable from the refusal alone, and naming the by-hand
/// lift. `standing` is what the caller read; an empty list is still a
/// quiesced refusal (the count gate fired), just one that could not name its
/// holder, and says so rather than pretending.
pub fn quiesced_busy(standing: &[Quiescer]) -> SpineError {
    let named = if standing.is_empty() {
        "(no quiescer row could be read back)".to_string()
    } else {
        standing
            .iter()
            .map(|q| format!("{} since {}", q.holder, q.since))
            .collect::<Vec<_>>()
            .join("; ")
    };
    SpineError::Busy {
        message: format!(
            "the cauldron is quiesced: no new commit begins while a whole-cauldron snapshot \
             is being taken; in-flight commits finish. Quiesced by {} quiescer(s): \
             {named}. Each is lifted by its own token (`resume`); a quiesce whose taker is dead \
             is lifted by hand with `resume_all(reason)`",
            standing.len()
        ),
    }
}

/// Enter the fail-closed halt (halt rather than degrade): when physics refuses
/// the spine append, no effect exists without its record — the system halts
/// loudly into an effectively read-only state, announced best-effort. No
/// retry-forever, no silent degradation, no unrecorded work.
///
/// VISIBILITY (the one-door law): the halt is a state PHYSICS
/// enters, never an operation a consumer performs — the `Cauldron` handle
/// exposes `quiesce`/`resume` and deliberately no `halt`. `pub` only under
/// `test-support` (the suites simulate a disk refusal with it); crate-private
/// for every consumer.
#[cfg(feature = "test-support")]
pub fn halt(spine: &mut SpineDb, reason: &str) -> Result<()> {
    halt_interior(spine, reason)
}
#[cfg(not(feature = "test-support"))]
pub(crate) fn halt(spine: &mut SpineDb, reason: &str) -> Result<()> {
    halt_interior(spine, reason)
}

fn halt_interior(spine: &mut SpineDb, reason: &str) -> Result<()> {
    // ANNOUNCED BEST-EFFORT, and to the one channel that cannot itself be
    // the thing that failed: a halt caused by a full disk cannot be
    // announced by writing to the disk.
    eprintln!("spine: HALTED (fail-closed, read-only): {reason}");
    // Then durably, so the halt survives the process that entered it and
    // every later door refuses by name. If THIS write fails too, the
    // failure is reported rather than swallowed — the caller still refuses
    // the transaction, but nobody gets to believe the halt was recorded.
    spine.set_halted(reason).map_err(|e| {
        SpineError::hard(format!(
            "the spine could not record its own halt ({reason}); the underlying failure was: {e}. \
             The cauldron is halted in this process and NOT durably marked — repair it \
             out-of-band before reopening."
        ))
    })
}
