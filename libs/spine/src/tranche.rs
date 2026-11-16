//! THE TRANCHE RECORD — a type-three ripple's
//! macro-transaction, as far as its record goes: `begin` appends the anchor
//! row and the `tranches` row, the queue opens the head by the anchor's
//! `seq`, the heartbeat keeps the attempt alive or the tick abandons it, and
//! `rollback` ends it into the store, which `tranche_store` reads back.
//!
//! Crate-interior; every act is reached through [`crate::Cauldron`]
//! (`tranche_begin`, `tranche_tick`, `tranche_heartbeat`, `tranche_rollback`,
//! `tranche_store`). What else is built on this module:
//!
//! - lock mode `whole` (the gate, the two drains, `lock`'s switch) is HERE:
//!   [`gate_up`] is the one predicate the three
//!   checkpoints read, [`drained`] the one drain predicate, `advance_head`
//!   raises the gate tick-driven, `tranche_lock` switches modes;
//! - staging children and reads under key are HERE
//!   (`tranche_stage`, `tranche_read`; the pipeline is `exec::stage` and
//!   `read::read_under`, the view `read::View`); THE LANDING is here
//!   (`tranche_commit` flips and joins; `tranche_tick` spawns THE
//!   LANDER, `exec::land`, for every `landing` tranche with none running
//!   in-process); the tranche recovery pass and the crash tests over
//!   the lander's seams ([`LanderStage`]);
//! - the door, the scheduler on the soup's tick, the `tranches` shape and the
//!   bus levels behind the quiet start live outside this module.
//!
//! # Time
//!
//! Every act takes `now_ms`, milliseconds since the Unix epoch on the
//! daemon's clock: the soup passes its clock; tests pass a chosen one.
//! The clock is RECORDED (`began_at`, `deadline`, …) and measures the two
//! bounds (the heartbeat timeout, the quiet wait) — it NEVER orders the
//! queue, which is the anchor row's `seq`.
//!
//! # The per-tranche mutex
//!
//! Every act that touches the touched set or the record runs under an
//! in-process mutex per tranche attempt — `rollback`, children and reads
//! under key, `commit`'s state flip, `lock` and the lander.
//! `heartbeat` and `commit`'s JOIN (which only waits on the
//! record) run outside it. The mutex GUARDS THE VIEW: the slot behind it holds the tranche's overlays, so the view is
//! read and grown only by the holder of the mutex — the rule of one claim
//! call at a time per tranche and the overlay's consistency are one lock. The tick takes it only with a non-blocking
//! try and skips the tranche on contention (a skip is not an error). The
//! mutex is process-wide, keyed by the cauldron's directory and `(T, n)`, so
//! two `Cauldron` handles on one directory in one process share it. Across
//! processes nothing is shared, and nothing needs to be: every state flip is
//! ALSO a conditional update inside an IMMEDIATE spine transaction
//! ([`flip_state`]), so two acts that cross resolve to exactly one outcome
//! on the record whoever runs them.

use crate::config::{SpineConfig, TrancheConfig};
use crate::delta::CompiledPayload;
use crate::envelope::{
    Bindings, Envelope, Initiator, Instrument, Mode, Provenance, RippleKind, TxHash,
    ENVELOPE_FORMAT_VERSION,
};
use crate::error::{Result, SpineError};
use crate::locks::{ClaimSet, TrancheClaimant};
use crate::read::{TrancheRead, View};
use crate::spine_db::Seq;
use crate::witness::Witness;
use crate::Cauldron;
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

// ---------------------------------------------------------------------------
// vocabulary
// ---------------------------------------------------------------------------

/// The tranche states. `Landing` and `Landed` are reached only from the
/// landing; `Failed` from its re-validation. The record's legality checks
/// name them all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrancheState {
    #[serde(rename = "queued")]
    Queued,
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "staging")]
    Staging,
    #[serde(rename = "landing")]
    Landing,
    #[serde(rename = "landed")]
    Landed,
    #[serde(rename = "rolled-back")]
    RolledBack,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "abandoned")]
    Abandoned,
}

impl TrancheState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TrancheState::Queued => "queued",
            TrancheState::Open => "open",
            TrancheState::Staging => "staging",
            TrancheState::Landing => "landing",
            TrancheState::Landed => "landed",
            TrancheState::RolledBack => "rolled-back",
            TrancheState::Failed => "failed",
            TrancheState::Abandoned => "abandoned",
        }
    }

    fn parse(s: &str) -> Result<TrancheState> {
        Ok(match s {
            "queued" => TrancheState::Queued,
            "open" => TrancheState::Open,
            "staging" => TrancheState::Staging,
            "landing" => TrancheState::Landing,
            "landed" => TrancheState::Landed,
            "rolled-back" => TrancheState::RolledBack,
            "failed" => TrancheState::Failed,
            "abandoned" => TrancheState::Abandoned,
            other => {
                return Err(SpineError::hard(format!(
                    "a `tranches` row carries the unknown state `{other}`"
                )))
            }
        })
    }

    /// The `terminal` column: a terminal state is forever.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TrancheState::Landed
                | TrancheState::RolledBack
                | TrancheState::Failed
                | TrancheState::Abandoned
        )
    }
}

/// The SQL list of the non-terminal states — the one spelling every
/// conditional update and every queue read uses.
const LIVE_STATES: &str = "('queued','open','staging','landing')";

/// The two lock modes. `Whole` (the default) raises the gate;
/// `Touched` is the spine's own locks, retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrancheLockMode {
    Whole,
    Touched,
}

impl TrancheLockMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            TrancheLockMode::Whole => "whole",
            TrancheLockMode::Touched => "touched",
        }
    }

    fn parse(s: &str) -> Result<TrancheLockMode> {
        match s {
            "whole" => Ok(TrancheLockMode::Whole),
            "touched" => Ok(TrancheLockMode::Touched),
            other => Err(SpineError::hard(format!(
                "a `tranches` row carries the unknown lock mode `{other}`"
            ))),
        }
    }
}

/// THE LANDER'S STAGE VOCABULARY —
/// the four points of the landing pipeline a crash can fall at, named here,
/// on the record's side, and borrowed VERBATIM by the fault seam
/// (`crate::fault::at_lander_stage`), exactly as the door's four are
/// `recover::Stage`'s. The tranche recovery pass is what resumes from
/// each; the lander places the calls, and the seam is present under
/// `fault-injection` and refusing by name without it.
///
/// - `before-decision` — the lander holds the mutex, re-validation done or
///   not, the decision transaction not yet begun ("landing, undecided" —
///   the next tick re-runs the re-validation and decides);
/// - `after-decision` — the decision committed, no child landed
///   ("decided, children partly landed" with none landed);
/// - `between-children` — child `k` cleared, child `k+1` not begun;
/// - `before-clear` — every child cleared, the clearing not begun ("after
///   the last child cleared, before the clearing transaction").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LanderStage {
    BeforeDecision,
    AfterDecision,
    BetweenChildren,
    BeforeClear,
}

impl LanderStage {
    /// THE ONE LIST, in pipeline order; `parse` and the seam's refusal
    /// message derive from it.
    pub const ALL: [LanderStage; 4] = [
        LanderStage::BeforeDecision,
        LanderStage::AfterDecision,
        LanderStage::BetweenChildren,
        LanderStage::BeforeClear,
    ];

    /// The frozen text names: `before-decision`, `after-decision`,
    /// `between-children`, `before-clear`. Disjoint from `recover::Stage`'s
    /// four, so one `SPINE_FAULT` value aims at exactly one pipeline.
    pub fn name(&self) -> &'static str {
        match self {
            LanderStage::BeforeDecision => "before-decision",
            LanderStage::AfterDecision => "after-decision",
            LanderStage::BetweenChildren => "between-children",
            LanderStage::BeforeClear => "before-clear",
        }
    }

    /// A frozen name back to its stage; `None` for anything else.
    pub fn parse(s: &str) -> Option<LanderStage> {
        LanderStage::ALL.iter().copied().find(|st| st.name() == s)
    }
}

/// THE RECORD'S OWN KILL POINTS — a FOURTH seam vocabulary beside the
/// door's `recover::Stage`, the lander's [`LanderStage`] and the gate's
/// `fault::GateSeam`, disjoint from all three, spoken by the same
/// `SPINE_FAULT` / `SPINE_PAUSE` variables (`fault::at_tranche_stage`). Each
/// names the instant a daemon dies so that one known crash state is what
/// recovery finds:
///
/// - `after-begin` — `begin`'s transaction committed, the reply not yet
///   answered: the tranche is `queued` (or `open` inline) with its anchor
///   (with a `whole` head behind a granted writer,
///   `queued, draining = 1` — g_d1);
/// - `after-stage` — a child's staging transaction committed,
///   the reply not yet answered: `staging` with `staged = k`;
/// - `between-child-parts` — inside the landing of ONE child, its
///   first part applied and flagged, its next part not begun: `decided`,
///   `applied_parts` markers in one pool and not the other;
/// - `before-child-clear` — every part of a child applied and flagged, the
///   child's row not yet cleared (the point at which a part would be
///   applied TWICE if the pool's marker were not read);
/// - `in-rollback` — inside the rollback's one transaction, after its
///   statements and before its commit: the transaction never commits, the
///   tranche is as before.
///
/// The points before the decision, before the first child, between two
/// children and before the clearing are the lander's own four
/// ([`LanderStage`]); a halt is not a kill (`recover::halt`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrancheStage {
    AfterBegin,
    AfterStage,
    BetweenChildParts,
    BeforeChildClear,
    InRollback,
}

impl TrancheStage {
    /// THE ONE LIST, in a tranche's lifecycle order; `parse` and the seam's
    /// refusal message derive from it.
    pub const ALL: [TrancheStage; 5] = [
        TrancheStage::AfterBegin,
        TrancheStage::AfterStage,
        TrancheStage::BetweenChildParts,
        TrancheStage::BeforeChildClear,
        TrancheStage::InRollback,
    ];

    /// The frozen text names: `after-begin`, `after-stage`,
    /// `between-child-parts`, `before-child-clear`, `in-rollback`.
    pub fn name(&self) -> &'static str {
        match self {
            TrancheStage::AfterBegin => "after-begin",
            TrancheStage::AfterStage => "after-stage",
            TrancheStage::BetweenChildParts => "between-child-parts",
            TrancheStage::BeforeChildClear => "before-child-clear",
            TrancheStage::InRollback => "in-rollback",
        }
    }

    /// A frozen name back to its stage; `None` for anything else.
    pub fn parse(s: &str) -> Option<TrancheStage> {
        TrancheStage::ALL.iter().copied().find(|st| st.name() == s)
    }
}

/// The scheduler's retry backoff for a landing that stopped short: 1 second
/// doubling to 60. Not one of the four configurable settings — the
/// design fixes the numbers — but named once here rather than inlined.
pub const LANDING_BACKOFF_FLOOR_MS: i64 = 1_000;
/// The ceiling of the same backoff.
pub const LANDING_BACKOFF_CEILING_MS: i64 = 60_000;

/// `landing_error`: why the lander stopped short,
/// where, against whom, and when — carried on the `tranches` row until a
/// retry clears it, shown on the beat and on a joined `commit`. `why` is
/// `child_in_flight` | `file_held` | `lander_cancelled` | `internal`;
/// `at_child` is the ordinal the lander was at (absent before the decision
/// for `file_held`); `colliding_seq` names the foreign row for
/// `child_in_flight`. `at_ms` and `retries` are the scheduler's: the daemon
/// clock the error was recorded at and how many errors in a row, from which
/// the tick computes the backoff (`next_retry_ms`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingError {
    pub why: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_child: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colliding_seq: Option<i64>,
    pub message: String,
    pub at_ms: i64,
    pub retries: u32,
}

impl LandingError {
    /// The daemon clock at which the scheduler may retry: `at_ms` plus
    /// `floor × 2^(retries−1)`, capped at the ceiling.
    pub fn next_retry_ms(&self) -> i64 {
        let shift = self.retries.saturating_sub(1).min(16);
        let backoff = LANDING_BACKOFF_FLOOR_MS
            .saturating_mul(1i64 << shift)
            .min(LANDING_BACKOFF_CEILING_MS);
        self.at_ms.saturating_add(backoff)
    }
}

/// The quiet start's VERDICT INPUT, supplied by the caller. The soup
/// computes the levels from its bus client; the spine never learns
/// about the bus. It only applies the rule — holds when the bus is not
/// configured, or when both consumers show nothing pending — and the BOUND
/// from `head_since`, which is the spine's because `head_since` is.
///
/// "A consumer absent from the shape counts as `pending = 0`" is the
/// caller's mapping: it passes `0` for an absent consumer.
/// `trigger_host_pending` is the trigger host's consumer level PLUS its
/// attempts without an outcome — one number, because the spine only asks
/// "is anything pending".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuietLevels {
    /// `EVENTS_SOCKET` unset: the quiet start holds vacuously.
    BusNotConfigured,
    /// The two levels the soup read off the bus's `consumers` shape.
    Levels {
        trigger_host_pending: u64,
        runner_pending: u64,
    },
}

impl QuietLevels {
    /// Does the quiet start hold on these levels alone (before the bound)?
    pub fn holds(&self) -> bool {
        match self {
            QuietLevels::BusNotConfigured => true,
            QuietLevels::Levels {
                trigger_host_pending,
                runner_pending,
            } => *trigger_host_pending == 0 && *runner_pending == 0,
        }
    }
}

/// What a `begin` asks. `tranche` is `T`, the type-three run id;
/// `holder_service` is the verified service name of the connection and
/// `instrument` the connection's instrument — both minted by the door from
/// the connection, never from the request.
#[derive(Debug, Clone)]
pub struct TrancheBegin {
    pub tranche: String,
    pub key: String,
    pub holder_service: String,
    pub instrument: Instrument,
    pub lock_mode: TrancheLockMode,
    pub regions: Vec<String>,
}

/// Who acts under a key: the key, and the connection's verified service.
#[derive(Debug, Clone)]
pub struct TrancheActor {
    pub tranche: String,
    pub key: String,
    pub holder_service: String,
}

/// `{tranche, attempt, state}` — `ahead` in `begin`'s reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrancheRef {
    pub tranche: String,
    pub attempt: u32,
    pub state: TrancheState,
}

/// `heartbeat: {interval_ms, timeout_ms}` in `begin`'s reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatAdvert {
    pub interval_ms: u64,
    pub timeout_ms: u64,
}

/// `begin`'s reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrancheBegun {
    pub tranche: String,
    pub attempt: u32,
    pub state: TrancheState,
    /// 1 = the head of the queue; 2 = next.
    pub position: u32,
    pub lock_mode: TrancheLockMode,
    pub heartbeat: HeartbeatAdvert,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<TrancheRef>,
    /// `queued` at the head with the gate ALREADY UP from this `begin`'s own
    /// transaction (inline evaluation): a `whole`
    /// tranche that found the queue empty and the quiet start holding but a
    /// granted writer still landing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub draining: bool,
}

/// `lock`'s reply: `switched: false` when the mode named is the one
/// already held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrancheLocked {
    pub state: TrancheState,
    pub lock_mode: TrancheLockMode,
    pub switched: bool,
}

/// The `quiet` object on a heartbeat reply: shown while the tranche is
/// queued at the head, the bus is configured, and the quiet start is being
/// evaluated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuietShown {
    pub trigger_host_pending: u64,
    pub runner_pending: u64,
    pub waited_ms: i64,
}

/// The tranche's record as the heartbeat answers it. `decided`,
/// `landed_children` and `landing_error` are the landing's (the beat
/// carries the error until a retry clears it); `draining` is the gate's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrancheRecord {
    pub tranche: String,
    pub attempt: u32,
    pub state: TrancheState,
    pub lock_mode: TrancheLockMode,
    /// Only while `queued`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
    pub holder_service: String,
    pub staged: u32,
    pub began_at: i64,
    pub last_heartbeat: i64,
    pub deadline: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quiet: Option<QuietShown>,
    /// The decision happened: the tranche cannot fail any more.
    pub decided: bool,
    /// Children cleared so far.
    pub landed_children: u32,
    /// The lander stopped short and the scheduler is retrying.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landing_error: Option<LandingError>,
    /// `queued` at the head, the quiet start done, THE GATE UP, waiting for
    /// the granted writers to clear. Never shown beside
    /// `quiet`: the quiet start is evaluated first and the drain begins
    /// only once it has held or its bound ran out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub draining: bool,
}

/// One child in `commit`'s reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedChild {
    pub ordinal: u32,
    pub tx: String,
    /// The reserved row's `seq` — or, deduplicated, the foreign row's.
    pub seq: i64,
    /// Binding → `<pool>/<serial id>` (numbered).
    pub minted: BTreeMap<String, String>,
    /// Marked `landed_elsewhere` at the decision: no row of
    /// its own, no `index`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deduplicated: bool,
}

/// `commit`'s reply on success: computed from the record alone, so
/// every `commit` on a `landed` tranche answers the same bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrancheCommitted {
    pub state: TrancheState,
    pub tranche: String,
    pub attempt: u32,
    pub children: Vec<CommittedChild>,
    /// `<pool>/<provisional id>` → `<pool>/<serial id>`.
    pub renumbered: BTreeMap<String, String>,
}

/// `rollback`'s reply. The same bytes on every repeat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrancheRolledBack {
    pub state: TrancheState,
    pub tranche: String,
    pub attempt: u32,
    pub stored: u32,
}

/// One child in the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredChild {
    pub ordinal: u32,
    pub tx: String,
    pub statement: String,
    pub bindings: Bindings,
    pub format_version: u32,
    pub ripple: Value,
    pub instrument: Instrument,
    pub idempotency_nonce: Option<String>,
    pub minted: BTreeMap<String, String>,
    /// `stored` on a terminal attempt; `staged` on a live one.
    pub state: String,
}

/// The store's record of one attempt ("read the store").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrancheStore {
    pub tranche: String,
    pub attempt: u32,
    pub state: TrancheState,
    pub reason: Option<String>,
    pub lock_mode: TrancheLockMode,
    pub regions: Vec<String>,
    pub began_at: i64,
    pub ended_at: Option<i64>,
    pub last_heartbeat: i64,
    pub holder_service: String,
    /// Absent on a live attempt and on every end but `failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Value>,
    pub children: Vec<StoredChild>,
}

/// What one tick did: which attempts it abandoned, which
/// it opened, which it skipped because their mutex was held — and, for the
/// `landing` tranches, which it spawned a lander for, which already had one
/// running in-process, and which it left in their retry backoff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickReport {
    pub abandoned: Vec<(String, u32)>,
    pub opened: Vec<(String, u32)>,
    pub skipped_busy: Vec<(String, u32)>,
    pub landers_spawned: Vec<(String, u32)>,
    pub landers_running: Vec<(String, u32)>,
    pub landers_backing_off: Vec<(String, u32)>,
    /// The `whole` head left `queued, draining = 1` by this tick (the
    /// drain at `queued → open`, tick-driven), with
    /// the `seq`s of the granted non-`read` holders it is waiting for. Empty
    /// `seq`s = the gate rose on this tick; the drain is evaluated next tick.
    pub draining: Vec<(String, u32, Vec<i64>)>,
    /// The `warn` lines this tick emitted (also on stderr): a stranded
    /// writer the drain waits for, and the same-service tripwire — a
    /// tranche queued behind another of the SAME `holder_service`.
    pub warnings: Vec<String>,
    /// Lock rows the orphan sweep released on this tick:
    /// rows whose holding `transactions` row was already cleared. Zero on
    /// every ordinary tick.
    pub orphans_swept: usize,
    /// Non-terminal rows whose deadline this tick re-armed to `now +
    /// timeout` because it is the first un-halted tick after this process
    /// saw the spine halted (the restart rule applied to the
    /// repair — `halts_seen`). Zero on every ordinary tick.
    pub deadlines_rearmed: usize,
}

// ---------------------------------------------------------------------------
// the key
// ---------------------------------------------------------------------------

/// 64 lowercase hex characters → the 32 bytes, or a by-name usage refusal
/// (the door's `malformed`).
fn key_bytes(key: &str) -> Result<[u8; 32]> {
    let bad = || {
        SpineError::usage(
            "a tranche key is 32 bytes as 64 lowercase hex characters",
        )
    };
    if key.len() != 64 || !key.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(bad());
    }
    let mut out = [0u8; 32];
    for (i, pair) in key.as_bytes().chunks(2).enumerate() {
        let hi = (pair[0] as char).to_digit(16).ok_or_else(bad)?;
        let lo = (pair[1] as char).to_digit(16).ok_or_else(bad)?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Ok(out)
}

/// The stored form: hex SHA-256 of the key's 32 bytes. The key itself is
/// never stored.
fn key_hash(key: &str) -> Result<String> {
    let bytes = key_bytes(key)?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// Constant-time equality over two hash strings ("compares in constant
/// time"). Length is not secret (always 64).
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// the per-tranche mutex
// ---------------------------------------------------------------------------

type MutexKey = (PathBuf, String, u32);

/// What the per-tranche mutex guards: the VIEW, built on the
/// first act under the key in this process and dropped at the terminal
/// transition. `None` = not built yet, or dropped; the next act rebuilds it
/// from the `tranche_children` rows.
#[derive(Default)]
pub(crate) struct Slot {
    pub(crate) view: Option<View>,
}

fn mutexes() -> &'static Mutex<HashMap<MutexKey, Arc<Mutex<Slot>>>> {
    static M: OnceLock<Mutex<HashMap<MutexKey, Arc<Mutex<Slot>>>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The one mutex of attempt `(T, n)` in this cauldron, made on first use.
fn tranche_mutex(cauldron: &Cauldron, tranche: &str, attempt: u32) -> Arc<Mutex<Slot>> {
    let root = std::fs::canonicalize(&cauldron.root).unwrap_or_else(|_| cauldron.root.clone());
    let mut map = mutexes().lock().unwrap_or_else(|p| p.into_inner());
    map.entry((root, tranche.to_string(), attempt))
        .or_insert_with(|| Arc::new(Mutex::new(Slot::default())))
        .clone()
}

/// THE IN-PROCESS LANDER REGISTRY (any `landing` tranche that has no
/// lander running in-process gets one): the attempts a lander
/// thread is running for, in this process. The tick INSERTS before it
/// spawns and the lander REMOVES on exit (every exit path, by guard), so
/// two ticks firing together spawn one lander, never two — the mutex's
/// non-blocking try is the second line, for a child in flight.
fn landers() -> &'static Mutex<HashSet<MutexKey>> {
    static L: OnceLock<Mutex<HashSet<MutexKey>>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(HashSet::new()))
}

fn lander_key(cauldron: &Cauldron, tranche: &str, attempt: u32) -> MutexKey {
    let root = std::fs::canonicalize(&cauldron.root).unwrap_or_else(|_| cauldron.root.clone());
    (root, tranche.to_string(), attempt)
}

/// Removes the registry entry when the lander thread ends, however it ends.
struct LanderTicket(MutexKey);

impl Drop for LanderTicket {
    fn drop(&mut self) {
        landers().lock().unwrap_or_else(|p| p.into_inner()).remove(&self.0);
    }
}

// ---------------------------------------------------------------------------
// the record, read inside a transaction
// ---------------------------------------------------------------------------

/// One `tranches` row, as every act reads it.
#[derive(Debug, Clone)]
struct Row {
    id: String,
    attempt: u32,
    seq: i64,
    state: TrancheState,
    lock_mode: TrancheLockMode,
    holder_service: String,
    key_hash: String,
    regions: Vec<String>,
    began_at: i64,
    last_heartbeat: i64,
    deadline: i64,
    head_since: Option<i64>,
    ended_at: Option<i64>,
    reason: Option<String>,
    refusal: Option<String>,
    staged: u32,
    decided: bool,
    renumbered: Option<String>,
    landed_children: u32,
    landing_error: Option<String>,
    draining: bool,
}

const ROW_COLUMNS: &str = "id, attempt, seq, state, lock_mode, holder_service, key_hash, regions, \
     began_at, last_heartbeat, deadline, head_since, ended_at, reason, refusal, staged, \
     decided, renumbered, landed_children, landing_error, draining";

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
    Ok(RawRow {
        id: r.get(0)?,
        attempt: r.get(1)?,
        seq: r.get(2)?,
        state: r.get(3)?,
        lock_mode: r.get(4)?,
        holder_service: r.get(5)?,
        key_hash: r.get(6)?,
        regions: r.get(7)?,
        began_at: r.get(8)?,
        last_heartbeat: r.get(9)?,
        deadline: r.get(10)?,
        head_since: r.get(11)?,
        ended_at: r.get(12)?,
        reason: r.get(13)?,
        refusal: r.get(14)?,
        staged: r.get(15)?,
        decided: r.get(16)?,
        renumbered: r.get(17)?,
        landed_children: r.get(18)?,
        landing_error: r.get(19)?,
        draining: r.get(20)?,
    })
}

struct RawRow {
    id: String,
    attempt: i64,
    seq: i64,
    state: String,
    lock_mode: String,
    holder_service: String,
    key_hash: String,
    regions: String,
    began_at: i64,
    last_heartbeat: i64,
    deadline: i64,
    head_since: Option<i64>,
    ended_at: Option<i64>,
    reason: Option<String>,
    refusal: Option<String>,
    staged: i64,
    decided: i64,
    renumbered: Option<String>,
    landed_children: i64,
    landing_error: Option<String>,
    draining: i64,
}

impl RawRow {
    fn cook(self) -> Result<Row> {
        let regions: Vec<String> = serde_json::from_str(&self.regions).map_err(|e| {
            SpineError::hard(format!(
                "tranche `{}` attempt {} carries unparseable regions: {e}",
                self.id, self.attempt
            ))
        })?;
        Ok(Row {
            state: TrancheState::parse(&self.state)?,
            lock_mode: TrancheLockMode::parse(&self.lock_mode)?,
            id: self.id,
            attempt: self.attempt as u32,
            seq: self.seq,
            holder_service: self.holder_service,
            key_hash: self.key_hash,
            regions,
            began_at: self.began_at,
            last_heartbeat: self.last_heartbeat,
            deadline: self.deadline,
            head_since: self.head_since,
            ended_at: self.ended_at,
            reason: self.reason,
            refusal: self.refusal,
            staged: self.staged as u32,
            decided: self.decided != 0,
            renumbered: self.renumbered,
            landed_children: self.landed_children as u32,
            landing_error: self.landing_error,
            draining: self.draining != 0,
        })
    }
}

impl Row {
    /// The parsed `landing_error`, or a loud `Hard` on a column that will
    /// not parse (a store that will not answer is never an empty store).
    fn landing_error(&self) -> Result<Option<LandingError>> {
        match &self.landing_error {
            None => Ok(None),
            Some(text) => serde_json::from_str(text).map(Some).map_err(|e| {
                SpineError::hard(format!(
                    "tranche `{}` attempt {}: unparseable landing_error: {e}",
                    self.id, self.attempt
                ))
            }),
        }
    }
}

fn rows_where(tx: &Transaction<'_>, clause: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Row>> {
    let mut stmt = tx.prepare(&format!("SELECT {ROW_COLUMNS} FROM tranches WHERE {clause}"))?;
    let raw = stmt.query_map(args, read_row)?;
    let mut out = Vec::new();
    for r in raw {
        out.push(r?.cook()?);
    }
    Ok(out)
}

fn one_row(tx: &Transaction<'_>, tranche: &str, attempt: u32) -> Result<Row> {
    rows_where(tx, "id = ?1 AND attempt = ?2", &[&tranche, &attempt])?
        .pop()
        .ok_or_else(|| {
            SpineError::hard(format!(
                "tranche `{tranche}` attempt {attempt} vanished from the record mid-act"
            ))
        })
}

/// The queue position: one plus the number of NON-TERMINAL tranches with a smaller anchor
/// `seq` — the spine's insertion order, never the clock.
fn position_of(tx: &Transaction<'_>, seq: i64) -> Result<u32> {
    let ahead: i64 = tx.query_row(
        &format!("SELECT COUNT(*) FROM tranches WHERE seq < ?1 AND state IN {LIVE_STATES}"),
        params![seq],
        |r| r.get(0),
    )?;
    Ok(ahead as u32 + 1)
}

/// The head of the queue: the non-terminal tranche with the smallest anchor
/// `seq`.
fn head_of_queue(tx: &Transaction<'_>) -> Result<Option<Row>> {
    Ok(rows_where(
        tx,
        &format!("state IN {LIVE_STATES} ORDER BY seq LIMIT 1"),
        &[],
    )?
    .pop())
}

/// `tranche_terminal` for a row (the witness table).
fn terminal_witness(row: &Row) -> Witness {
    Witness::TrancheTerminal {
        tranche: row.id.clone(),
        attempt: row.attempt,
        state: row.state.as_str().to_string(),
        reason: row.reason.clone(),
        began_at: row.began_at,
        ended_at: row.ended_at,
        last_heartbeat: row.last_heartbeat,
        deadline: row.deadline,
    }
}

/// The legality rule for a CHILD or a READ UNDER KEY (and `lock`), judged
/// from the state found: `queued` → `busy tranche_busy / queued` with the
/// position and the head; `landing` → `refused tranche_past_rollback`; a
/// terminal state → `refused tranche_terminal`; `open` or `staging` → the
/// act proceeds. Run TWICE (the legality check runs twice): at the
/// door before the mutex, so an act issued in `landing` is not parked
/// behind a long landing before it is refused; and inside the mutex, where
/// it is the guarantee.
fn legality_for_act(tx: &Transaction<'_>, row: &Row) -> Result<()> {
    match row.state {
        TrancheState::Open | TrancheState::Staging => Ok(()),
        TrancheState::Queued => {
            let position = position_of(tx, row.seq)?;
            let ahead = head_of_queue(tx)?.map(|h| TrancheRef {
                tranche: h.id,
                attempt: h.attempt,
                state: h.state,
            });
            let mut fields = serde_json::Map::new();
            fields.insert("position".into(), json!(position));
            fields.insert(
                "ahead".into(),
                serde_json::to_value(ahead)
                    .map_err(|e| SpineError::hard(format!("ahead will not serialize: {e}")))?,
            );
            Err(SpineError::TrancheBusy {
                why: "queued".to_string(),
                fields,
            })
        }
        TrancheState::Landing => Err(SpineError::refuse(Witness::TranchePastRollback {
            tranche: row.id.clone(),
            attempt: row.attempt,
        })),
        _ => Err(SpineError::refuse(terminal_witness(row))),
    }
}

/// The legality rule for `commit`, judged from the state found: `queued`
/// → `busy queued`; `open` / `staging` / `landing` / `landed` → proceed
/// (flip, or join); `failed` → `refused` carrying BOTH `tranche_terminal
/// {reason: landing_refused}` and `tranche_landing_refused` (so every
/// `commit` on a failed tranche answers the same bytes); `rolled-back` /
/// `abandoned` → `refused tranche_terminal`.
fn legality_for_commit(tx: &Transaction<'_>, row: &Row) -> Result<()> {
    match row.state {
        TrancheState::Open | TrancheState::Staging | TrancheState::Landing | TrancheState::Landed => {
            Ok(())
        }
        TrancheState::Queued => legality_for_act(tx, row),
        TrancheState::Failed => Err(failed_refusal(row)?),
        _ => Err(SpineError::refuse(terminal_witness(row))),
    }
}

/// `failed`'s refusal: the two witnesses, the second read back off
/// the row's `refusal` column exactly as the store recorded it.
fn failed_refusal(row: &Row) -> Result<SpineError> {
    let mut witnesses = vec![terminal_witness(row)];
    if let Some(text) = &row.refusal {
        let w: Witness = serde_json::from_str(text).map_err(|e| {
            SpineError::hard(format!(
                "tranche `{}` attempt {}: unparseable refusal: {e}",
                row.id, row.attempt
            ))
        })?;
        witnesses.push(w);
    }
    Ok(SpineError::Refused { witnesses })
}

/// `commit`'s reply on `landed`, FROM THE RECORD ALONE: the children
/// in ordinal order off `tranche_children` (`landed` rows carry their
/// reserved row's `seq` and numbered payload; `landed_elsewhere` rows the
/// foreign `seq`), `renumbered` off the `tranches` row. Two `commit`s on two
/// threads after `landed` compute the same bytes.
fn committed_reply(tx: &Transaction<'_>, row: &Row) -> Result<TrancheCommitted> {
    let mut stmt = tx.prepare(
        "SELECT ordinal, hash, payload, state, seq FROM tranche_children
         WHERE tranche_id = ?1 AND tranche_attempt = ?2 ORDER BY ordinal",
    )?;
    let rows = stmt.query_map(params![row.id, row.attempt], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<i64>>(4)?,
        ))
    })?;
    let mut children = Vec::new();
    for r in rows {
        let (ordinal, hash, payload, state, seq) = r?;
        let payload: CompiledPayload = serde_json::from_str(&payload).map_err(|e| {
            SpineError::hard(format!(
                "tranche `{}` attempt {} child {ordinal}: unparseable payload: {e}",
                row.id, row.attempt
            ))
        })?;
        let seq = seq.ok_or_else(|| {
            SpineError::hard(format!(
                "tranche `{}` attempt {} is landed but child {ordinal} ({state}) names no row",
                row.id, row.attempt
            ))
        })?;
        children.push(CommittedChild {
            ordinal: ordinal as u32,
            tx: hash,
            seq,
            minted: payload
                .minted
                .iter()
                .map(|(k, v)| (k.clone(), v.to_string()))
                .collect(),
            deduplicated: state == CHILD_LANDED_ELSEWHERE,
        });
    }
    let renumbered: BTreeMap<String, String> = match &row.renumbered {
        None => BTreeMap::new(),
        Some(text) => serde_json::from_str(text).map_err(|e| {
            SpineError::hard(format!(
                "tranche `{}` attempt {}: unparseable renumbered map: {e}",
                row.id, row.attempt
            ))
        })?,
    };
    Ok(TrancheCommitted {
        state: TrancheState::Landed,
        tranche: row.id.clone(),
        attempt: row.attempt,
        children,
        renumbered,
    })
}

/// `tranche_children.state` values the landing writes: a child whose
/// row was reserved at the decision, one that landed, one deduplicated at
/// the decision against a foreign row. (`staged` is the staging's, `stored`
/// is the release's.)
pub(crate) const CHILD_RESERVED: &str = "reserved";
pub(crate) const CHILD_LANDED: &str = "landed";
pub(crate) const CHILD_LANDED_ELSEWHERE: &str = "landed_elsewhere";

/// CLAIM RULE 2, the one spelling: inside the same immediate
/// transaction as every enrolment and every grant under a tranche's `seq`,
/// the `tranches` row that owns `seq` is read, and only `open` or `staging`
/// lets the call proceed — `landing` → `refused tranche_past_rollback`, a
/// terminal state → `refused tranche_terminal`, `queued` → the queued busy.
/// A `seq` no `tranches` row names is `Hard`: claim growth is admitted for
/// a tranche's number only, and `locks::acquire` still
/// refuses it for every other `seq`. Called by `locks::acquire_for_tranche`
/// and by `exec::stage`'s durable stage.
pub(crate) fn claim_state_check(conn: &rusqlite::Connection, seq: Seq) -> Result<()> {
    let raw: Option<RawRow> = conn
        .query_row(
            &format!("SELECT {ROW_COLUMNS} FROM tranches WHERE seq = ?1"),
            params![seq.0],
            read_row,
        )
        .optional()?;
    let Some(raw) = raw else {
        return Err(SpineError::hard(format!(
            "seq {seq} is not a tranche's anchor: claim growth under one sequence number is \
             admitted for a non-terminal tranche's `seq` only"
        )));
    };
    let row = raw.cook()?;
    match row.state {
        TrancheState::Open | TrancheState::Staging => Ok(()),
        TrancheState::Landing => Err(SpineError::refuse(Witness::TranchePastRollback {
            tranche: row.id,
            attempt: row.attempt,
        })),
        TrancheState::Queued => {
            let mut fields = serde_json::Map::new();
            fields.insert("position".into(), json!(null));
            Err(SpineError::TrancheBusy {
                why: "queued".to_string(),
                fields,
            })
        }
        _ => Err(SpineError::refuse(terminal_witness(&row))),
    }
}

// ---------------------------------------------------------------------------
// THE GATE — one predicate, three checkpoints
// ---------------------------------------------------------------------------

/// The tranche holding the whole-soup write lock, as [`gate_up`]
/// answers it: `seq` is the anchor's, the one number every checkpoint
/// exempts (`WHERE seq != ?holder`) — the tranche's own acts and its landing
/// never meet the gate by THIS and by nothing else (test g8: no second
/// code path, no "am I a tranche" flag on a request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateHolder {
    pub tranche: String,
    pub attempt: u32,
    pub seq: i64,
}

/// THE PREDICATE, STATED ONCE: the gate is up exactly when a tranche
/// with `lock_mode = whole` is `open`, `staging` or `landing`, or is `queued`
/// with `draining = 1`. ONE SQL read over `tranches`, no in-memory flag
/// anywhere — the gate is table state, so a second `Cauldron` handle, a
/// restart (`draining` survives a crash) and the
/// scheduler all read the same truth. `idx_tranches_live` admits one live
/// row per `id` and the queue admits one non-`queued` live tranche overall,
/// and only the head is ever `queued, draining = 1`, so at most one
/// row matches (asserted by test g1). Called by `exec::execute` phase 1
/// (checkpoint 1), `SpineDb::append_or_existing` (checkpoint 2) and
/// `locks::gate_check` at enrolment and every grant poll (checkpoint 3).
pub(crate) fn gate_up(conn: &rusqlite::Connection) -> Result<Option<GateHolder>> {
    let found = conn
        .query_row(
            "SELECT id, attempt, seq FROM tranches
             WHERE lock_mode = 'whole'
               AND (state IN ('open','staging','landing')
                    OR (state = 'queued' AND draining = 1))
             LIMIT 1",
            [],
            |r| {
                Ok(GateHolder {
                    tranche: r.get::<_, String>(0)?,
                    attempt: r.get::<_, i64>(1)? as u32,
                    seq: r.get(2)?,
                })
            },
        )
        .optional()?;
    Ok(found)
}

/// `busy tranche_busy / locked` ("a commit from any other writer while
/// the gate is up" — outside every retry bound), naming the holder.
pub(crate) fn locked_busy(holder: &GateHolder) -> SpineError {
    let mut fields = serde_json::Map::new();
    fields.insert("tranche".into(), json!(holder.tranche));
    fields.insert("attempt".into(), json!(holder.attempt));
    SpineError::TrancheBusy {
        why: "locked".to_string(),
        fields,
    }
}

/// THE DRAIN PREDICATE: every `seq` but the holder's own
/// that holds a GRANTED lock row in any mode other than `read` — `write`,
/// `link` and `upgrade-intent` alike. Empty = drained. Once the gate is up
/// the set can only shrink: checkpoint 3 refuses every new non-`read` grant,
/// and a granted writer is landing and finishes. Read by `advance_head` at
/// `queued → open` (once per tick, never a loop inside the tick) and by `tranche_lock`'s bounded `touched → whole` drain.
pub(crate) fn drained(conn: &rusqlite::Connection, holder_seq: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT seq FROM locks
         WHERE granted = 1 AND mode != 'read' AND seq != ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map(params![holder_seq], |r| r.get::<_, i64>(0))?;
    rows.collect::<std::result::Result<_, _>>().map_err(Into::into)
}

/// The halt gate, inside the act's own transaction: a halted spine refuses
/// every write, a tranche act included.
fn halt_gate(tx: &Transaction<'_>) -> Result<()> {
    let (halted, reason): (i64, Option<String>) = tx.query_row(
        "SELECT halted, halted_reason FROM spine_state WHERE singleton = 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if halted != 0 {
        note_halt_seen(tx);
        return Err(SpineError::Halted {
            reason: reason.unwrap_or_else(|| "halted with no recorded reason".to_string()),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// the heartbeat across a halt
// ---------------------------------------------------------------------------

/// **A HALT IS NOT THE HOLDER'S FAULT.** Under a halt every beat is `dead`
/// and the holder is told to keep the key and STOP until
/// the daemon answers again, so the heartbeat clock cannot be the holder's
/// to keep while the spine is halted. Without this, a halt longer than
/// `timeout` ended every queued/open/staging tranche at the first tick after
/// the repair (`abandoned, heartbeat_lost`) — a halt ending a tranche one
/// tick late, against the rule that a halt ends no tranche (after repair
/// the tranche is where it was).
///
/// The rule relied on is the restart rule: across a daemon restart a
/// recovered tranche's deadline is reset to `boot + timeout`, because the
/// restart is not the holder's fault. It is applied to the repair for the same reason: the
/// FIRST TICK THAT FINDS THE SPINE UN-HALTED after this process saw it
/// halted re-arms every non-terminal deadline to `now + timeout` (never
/// shortening one) BEFORE enforcement, and nothing else of the row. A halt
/// still writes no tranche row (t19_34, g_d5): the re-arm is a write after
/// the repair, never under the halt.
///
/// "Saw it halted" is per process and per spine file: set by every halt
/// gate that refuses (every beat, act and tick under the halt) and by the
/// tick's own read. A restart under the halt clears it at open, whose
/// `boot + timeout` reset covers the restart; the new process's own ticks
/// and refused beats see the halt again. A halt that begins and is repaired
/// with no tick and no act in between is shorter than one sweep interval,
/// which `timeout ≥ 3 × interval` already absorbs.
fn halts_seen() -> &'static Mutex<HashSet<PathBuf>> {
    static H: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(HashSet::new()))
}

/// The spine file a connection is open on, as the halt registry keys it.
fn spine_file_of(conn: &rusqlite::Connection) -> Option<PathBuf> {
    let p = PathBuf::from(conn.path()?);
    Some(std::fs::canonicalize(&p).unwrap_or(p))
}

fn note_halt_seen(conn: &rusqlite::Connection) {
    if let Some(p) = spine_file_of(conn) {
        halts_seen().lock().unwrap_or_else(|p| p.into_inner()).insert(p);
    }
}

/// `true` (and forgotten) when this process saw the spine halted since the
/// last re-arm.
fn take_halt_seen(conn: &rusqlite::Connection) -> bool {
    match spine_file_of(conn) {
        Some(p) => halts_seen().lock().unwrap_or_else(|p| p.into_inner()).remove(&p),
        None => false,
    }
}

/// Called by the tranche pass at open: the open's own `boot + timeout`
/// reset covers whatever halt the previous handle saw.
pub(crate) fn forget_halt_seen(cauldron: &Cauldron) -> Result<()> {
    let spine = cauldron.spine()?;
    take_halt_seen(spine.conn());
    Ok(())
}

/// THE CONDITIONAL STATE FLIP (each flip conditional on the state): move
/// attempt `(T, n)` to `to` only while it is in one of `from`, setting the
/// extra columns in the same statement. `true` = this caller won the flip;
/// `false` = the state was already elsewhere and NOTHING changed — the
/// caller re-reads the row and answers from what it found.
fn flip_state(
    tx: &Transaction<'_>,
    tranche: &str,
    attempt: u32,
    from: &[TrancheState],
    to: TrancheState,
    set_extra: &str,
    extra: &[&dyn rusqlite::ToSql],
) -> Result<bool> {
    let from_sql: Vec<String> = from.iter().map(|s| format!("'{}'", s.as_str())).collect();
    let set = if set_extra.is_empty() {
        String::new()
    } else {
        format!(", {set_extra}")
    };
    // ?1 = to, ?2 = id, ?3 = attempt, then the extras from ?4.
    let sql = format!(
        "UPDATE tranches SET state = ?1{set} WHERE id = ?2 AND attempt = ?3 AND state IN ({})",
        from_sql.join(",")
    );
    let to_s = to.as_str();
    let mut args: Vec<&dyn rusqlite::ToSql> = vec![&to_s, &tranche, &attempt];
    args.extend_from_slice(extra);
    let changed = tx.execute(&sql, args.as_slice())?;
    Ok(changed == 1)
}

/// The release, behind a WON flip to a terminal state, in the same
/// transaction: the anchor aborted (`decided = 0, cleared = 1`, kept forever,
/// its `result` naming the end and the children), every lock row under the
/// anchor's `seq` deleted (`release_at_clear` semantics), the children
/// marked `stored`. Returns how many children the store holds.
fn release_into_store(tx: &Transaction<'_>, row: &Row, end: TrancheState, reason: &str) -> Result<u32> {
    let children: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT hash FROM tranche_children WHERE tranche_id = ?1 AND tranche_attempt = ?2
             ORDER BY ordinal",
        )?;
        let rows = stmt.query_map(params![row.id, row.attempt], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let result = json!({
        "aborted": true,
        "landed": false,
        "tranche": {
            "id": row.id,
            "attempt": row.attempt,
            "state": end.as_str(),
            "reason": reason,
            "children": children,
        },
    });
    let anchored = tx.execute(
        "UPDATE transactions SET cleared = 1, result = ?2
         WHERE seq = ?1 AND decided = 0 AND cleared = 0",
        params![row.seq, result.to_string()],
    )?;
    if anchored != 1 {
        return Err(SpineError::hard(format!(
            "SPINE INTERNAL INVARIANT VIOLATED: tranche `{}` attempt {} won its flip to `{}` but its \
             anchor row seq {} was not un-decided and un-cleared",
            row.id,
            row.attempt,
            end.as_str(),
            row.seq
        )));
    }
    tx.execute("DELETE FROM locks WHERE seq = ?1", params![row.seq])?;
    tx.execute(
        "UPDATE tranche_children SET state = 'stored' WHERE tranche_id = ?1 AND tranche_attempt = ?2",
        params![row.id, row.attempt],
    )?;
    Ok(children.len() as u32)
}

/// Resolve the attempt a key names under `T`: the key's hash against
/// every attempt's, in constant time; then the service. Any miss is the one
/// `tranche_key` answer, with the distinguishing fact for the spine's own
/// callers.
fn admit(tx: &Transaction<'_>, actor: &TrancheActor) -> Result<Row> {
    let hash = key_hash(&actor.key)?;
    let rows = rows_where(tx, "id = ?1 ORDER BY attempt", &[&actor.tranche])?;
    let mut found = None;
    for r in rows {
        // Every attempt is compared, so the time taken does not say which.
        if ct_eq(&r.key_hash, &hash) && found.is_none() {
            found = Some(r);
        }
    }
    let row = found.ok_or_else(|| SpineError::TrancheKey {
        why: "no_tranche".to_string(),
    })?;
    if row.holder_service != actor.holder_service {
        return Err(SpineError::TrancheKey {
            why: "other_service".to_string(),
        });
    }
    Ok(row)
}

impl Cauldron {
    /// **WHICH `T` A KEY NAMES, READ FROM THE RECORD**. A child (whose
    /// `ripple.run_id` is the CHILD's run) and a read under key (no
    /// `ripple`) carry no `T`, so the
    /// door must find it; its per-process hint map is empty after a restart,
    /// and the key proves continuity "across reconnects and a daemon
    /// restart". This answers from the `tranches` rows: every `T` whose
    /// attempt's `key_hash` is this key's hash, held by `holder_service`,
    /// newest anchor first — TERMINAL attempts included, so a key of a
    /// settled tranche still reaches the `refused tranche_terminal` rather
    /// than the wrong-key answer.
    ///
    /// The key is hashed once and compared against EVERY row in constant
    /// time (`admit`'s shape over the whole set), so the time taken does not
    /// say which row, or whether any, matched. An empty answer is the
    /// caller's `tranche_key`. It is a lookup, never an admission: every act
    /// under the answer is still admitted by `admit` (hash and service).
    pub fn tranche_by_key(&self, key: &str, holder_service: &str) -> Result<Vec<String>> {
        let hash = key_hash(key)?;
        let mut spine = self.spine()?;
        let tx = spine.conn_mut().transaction()?;
        let rows: Vec<(String, String, String)> = {
            let mut stmt =
                tx.prepare("SELECT id, key_hash, holder_service FROM tranches ORDER BY seq DESC")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        drop(tx);
        let mut found: Vec<String> = Vec::new();
        for (id, row_hash, service) in rows {
            // Both compares run on every row (`&`, not `&&`).
            let hit = ct_eq(&row_hash, &hash) & (service == holder_service);
            if hit && !found.contains(&id) {
                found.push(id);
            }
        }
        Ok(found)
    }
}

fn ms(d: std::time::Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

fn begun_reply(
    tx: &Transaction<'_>,
    row: &Row,
    cfg: &TrancheConfig,
) -> Result<TrancheBegun> {
    let position = position_of(tx, row.seq)?;
    let ahead = if position > 1 {
        head_of_queue(tx)?.map(|h| TrancheRef {
            tranche: h.id,
            attempt: h.attempt,
            state: h.state,
        })
    } else {
        None
    };
    Ok(TrancheBegun {
        tranche: row.id.clone(),
        attempt: row.attempt,
        state: row.state,
        position,
        lock_mode: row.lock_mode,
        heartbeat: HeartbeatAdvert {
            interval_ms: ms(cfg.heartbeat_interval) as u64,
            timeout_ms: ms(cfg.timeout) as u64,
        },
        ahead,
        draining: false,
    })
}

/// What `advance_head` did with the head (the `queued → open` transition).
#[derive(Debug, Clone, PartialEq, Eq)]
enum HeadAdvance {
    /// The quiet start neither holds nor has run its bound: still `queued`,
    /// the gate down, `quiet` shown on the beat.
    Waiting,
    /// Flipped `queued → open`.
    Opened,
    /// A `whole` head past its quiet start: `draining = 1` stands (the gate
    /// is up from this transaction's commit) and these granted non-`read`
    /// holders are still landing — empty when the gate rose on this very
    /// call and the drain is first evaluated next tick.
    Draining(Vec<i64>),
}

/// The quiet start, its bound, and for mode `whole` THE DRAIN (the
/// `queued → open` transition), for a head still `queued`. Records
/// `head_since` the first time the tranche is seen at the head. The quiet
/// start is evaluated BEFORE the gate rises, never after (a gate
/// raised while it is evaluated would refuse exactly the pending writes it
/// waits for). Then:
///
/// - `touched` → `open` at once.
/// - `whole`, `inline` (from `begin`): no granted non-`read` row under
///   any other `seq` → `open` at once; else `draining = 1` in this same
///   transaction (the gate is up from `begin`'s commit
///   and the reply says `draining`, never `quiet`).
/// - `whole`, from the tick (TICK-DRIVEN, the tick
///   never waits): `draining = 0` → set it (the gate is up from this
///   commit); then, raised on this tick or an earlier one, evaluate
///   [`drained`] ONCE: empty → `open`; else return the stranded `seq`s for
///   the caller to log at `warn`. "No ceiling"
///   means the tranche stays `queued, draining = 1` across ticks — a
///   stranded granted writer is an existing failure with an existing repair
///   (recovery) — never a loop inside the tick.
fn advance_head(
    tx: &Transaction<'_>,
    head: &Row,
    quiet: &QuietLevels,
    cfg: &TrancheConfig,
    now_ms: i64,
    inline: bool,
) -> Result<HeadAdvance> {
    let head_since = match head.head_since {
        Some(t) => t,
        None => {
            tx.execute(
                "UPDATE tranches SET head_since = ?3 WHERE id = ?1 AND attempt = ?2 AND state = 'queued'",
                params![head.id, head.attempt, now_ms],
            )?;
            now_ms
        }
    };
    let bound_ran_out = !inline && now_ms.saturating_sub(head_since) >= ms(cfg.quiet_wait);
    if !(quiet.holds() || bound_ran_out) {
        return Ok(HeadAdvance::Waiting);
    }
    let open = |tx: &Transaction<'_>| -> Result<HeadAdvance> {
        let won = flip_state(
            tx,
            &head.id,
            head.attempt,
            &[TrancheState::Queued],
            TrancheState::Open,
            "head_since = NULL, draining = 0",
            &[],
        )?;
        Ok(if won { HeadAdvance::Opened } else { HeadAdvance::Waiting })
    };
    match head.lock_mode {
        TrancheLockMode::Touched => open(tx),
        TrancheLockMode::Whole => {
            let raise = |tx: &Transaction<'_>| -> Result<()> {
                tx.execute(
                    "UPDATE tranches SET draining = 1 WHERE id = ?1 AND attempt = ?2 AND state = 'queued'",
                    params![head.id, head.attempt],
                )?;
                Ok(())
            };
            if inline {
                let holding = drained(tx, head.seq)?;
                if holding.is_empty() {
                    return open(tx);
                }
                raise(tx)?;
                return Ok(HeadAdvance::Draining(holding));
            }
            if !head.draining {
                // Raise the gate (durably, this transaction) — then ONE
                // evaluation of the drain in the same transaction, so a
                // head with nothing to wait for opens on this tick (test
                // g4's opposed arm); never a loop, never a wait.
                raise(tx)?;
            }
            let holding = drained(tx, head.seq)?;
            if holding.is_empty() {
                return open(tx);
            }
            Ok(HeadAdvance::Draining(holding))
        }
    }
}

/// THE SAME-SERVICE TRIPWIRE: the last daemon clock at which the
/// same-service warning was logged for a queued attempt, per cauldron — so
/// the line is emitted once per `same_service_warn_interval` per pair, not
/// once per tick. Process-global, like the mutexes; a restart simply logs
/// again.
fn same_service_warned() -> &'static Mutex<HashMap<MutexKey, i64>> {
    static W: OnceLock<Mutex<HashMap<MutexKey, i64>>> = OnceLock::new();
    W.get_or_init(|| Mutex::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// the acts, reached through `Cauldron`
// ---------------------------------------------------------------------------

impl Cauldron {
    /// `begin`: one IMMEDIATE spine transaction. Idempotent
    /// by key: a live attempt under `T` with this key and service answers
    /// its state. A live attempt with another key or service, or no
    /// attempt this key names → [`SpineError::TrancheKey`]. A key whose hash
    /// matches ANY earlier attempt → `refused tranche_terminal` for that
    /// attempt (a key names one attempt, forever). The latest attempt
    /// `landed` → `refused tranche_terminal` (a type-three run lands once).
    /// Otherwise: the anchor row appended (undecided, `tranche_id` set), the
    /// `queued` `tranches` row written with the key's HASH, and the head
    /// evaluated once — `begin` never waits: if the queue was empty and the
    /// quiet start holds, `touched` opens inline, and `whole` opens inline
    /// when no granted non-`read` row stands under any other `seq`; else a
    /// `whole` head sets `draining = 1` in this same transaction (the gate is
    /// up from `begin`'s commit) and answers `queued, draining`.
    pub fn tranche_begin(
        &self,
        req: &TrancheBegin,
        quiet: &QuietLevels,
        now_ms: i64,
    ) -> Result<TrancheBegun> {
        // THE SEAM'S GATE: a production build refuses a set
        // `SPINE_FAULT`/`SPINE_PAUSE` by name BEFORE any effect, exactly as
        // the door does — never after a row is written.
        crate::fault::gate()?;
        let cfg = self.config().tranche.clone();
        if req.tranche.trim().is_empty() || req.holder_service.trim().is_empty() {
            return Err(SpineError::usage(
                "a tranche begins under a non-empty run id and a verified holder service",
            ));
        }
        let hash = key_hash(&req.key)?;
        // The anchor's envelope: empty statement and bindings,
        // initiator the type-three run, the connection's instrument, nonce T.
        let anchor = Envelope {
            statement: String::new(),
            bindings: Bindings::new(),
            format_version: ENVELOPE_FORMAT_VERSION,
            provenance: Provenance {
                initiator: Initiator::Ripple {
                    kind: RippleKind::TypeThree,
                    run_id: req.tranche.clone(),
                    step_id: req.tranche.clone(),
                    scope: vec![],
                },
                instrument: req.instrument.clone(),
            },
            mode: Mode::Commit,
            idempotency_nonce: Some(req.tranche.clone()),
        };
        anchor.check_provenance()?;
        let anchor_hash = anchor.hash_identity()?;
        let payload = CompiledPayload {
            parts: vec![],
            minted: BTreeMap::new(),
        };

        let mut spine = self.spine()?;
        let conn = spine.conn_mut();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        halt_gate(&tx)?;
        // A standing quiesce: no new row begins (the spine's own quiesce
        // contract) — the quiesced busy, as a child gets.
        {
            let standing: i64 = tx.query_row("SELECT COUNT(*) FROM quiescers", [], |r| r.get(0))?;
            if standing != 0 {
                let q = crate::spine_db::quiescers_in(&tx)?;
                return Err(crate::recover::quiesced_busy(&q));
            }
        }
        let attempts = rows_where(&tx, "id = ?1 ORDER BY attempt", &[&req.tranche])?;
        // 1. A live attempt under T.
        if let Some(live) = attempts.iter().find(|r| !r.state.is_terminal()) {
            if ct_eq(&live.key_hash, &hash) && live.holder_service == req.holder_service {
                return begun_reply(&tx, live, &cfg);
            }
            return Err(SpineError::TrancheKey {
                why: "live_other_key".to_string(),
            });
        }
        // 2. The key names an earlier attempt: never reused.
        let mut reused = None;
        for r in &attempts {
            if ct_eq(&r.key_hash, &hash) && reused.is_none() {
                reused = Some(r);
            }
        }
        if let Some(r) = reused {
            return Err(SpineError::refuse(terminal_witness(r)));
        }
        // 3. The latest attempt landed: a type-three run lands once.
        if let Some(latest) = attempts.last() {
            if latest.state == TrancheState::Landed {
                return Err(SpineError::refuse(terminal_witness(latest)));
            }
        }
        let attempt = attempts.last().map(|r| r.attempt + 1).unwrap_or(1);
        // 4. The anchor row: undecided, kept forever, carrying the mark.
        let ts: String =
            tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| r.get(0))?;
        tx.execute(
            "INSERT INTO transactions
               (hash, attempt, ts, envelope_version, initiator, instrument, statement, bindings,
                payload, touched, tranche_id, tranche_attempt)
             VALUES (?1,
                     (SELECT COALESCE(MAX(attempt), 0) + 1 FROM transactions WHERE hash = ?1),
                     ?2, ?3, ?4, ?5, '', '{}', ?6, '[]', ?7, ?8)",
            params![
                anchor_hash.to_hex(),
                ts,
                anchor.format_version,
                to_json(&anchor.provenance.initiator)?,
                to_json(&anchor.provenance.instrument)?,
                to_json(&payload)?,
                req.tranche,
                attempt,
            ],
        )?;
        let seq = tx.last_insert_rowid();
        // 5. The `tranches` row.
        let regions = serde_json::to_string(&req.regions)
            .map_err(|e| SpineError::hard(format!("regions will not serialize: {e}")))?;
        tx.execute(
            "INSERT INTO tranches
               (id, attempt, seq, state, lock_mode, holder_service, key_hash, regions, began_at,
                last_heartbeat, deadline)
             VALUES (?1, ?2, ?3, 'queued', ?4, ?5, ?6, ?7, ?8, ?8, ?9)",
            params![
                req.tranche,
                attempt,
                seq,
                req.lock_mode.as_str(),
                req.holder_service,
                hash,
                regions,
                now_ms,
                now_ms.saturating_add(ms(cfg.timeout)),
            ],
        )?;
        // 6. Evaluate the head ONCE: only if the queue was empty.
        let row = one_row(&tx, &req.tranche, attempt)?;
        if position_of(&tx, seq)? == 1 {
            advance_head(&tx, &row, quiet, &cfg, now_ms, true)?;
        }
        let row = one_row(&tx, &req.tranche, attempt)?;
        let mut reply = begun_reply(&tx, &row, &cfg)?;
        reply.draining = row.state == TrancheState::Queued && row.draining;
        tx.commit()?;
        // [tranche stage: after begin] — the record is durable, the reply
        // not yet answered.
        crate::fault::at_tranche_stage(TrancheStage::AfterBegin)?;
        Ok(reply)
    }

    /// The scheduler's tick, spine half: heartbeat
    /// ENFORCEMENT, then the QUEUE. Each in its own IMMEDIATE transaction.
    ///
    /// - Enforcement: per `queued`/`open`/`staging` tranche whose deadline
    ///   has passed, ONE conditional statement (`deadline < now AND state IN
    ///   (…)`) flips it `abandoned, reason: heartbeat_lost`, and only if a
    ///   row changed, the release in the same transaction. A `landing`
    ///   tranche is never abandoned. The tranche's mutex is taken with a
    ///   non-blocking try; contention skips it (`skipped_busy`).
    /// - Queue: the head (smallest anchor `seq` among non-terminal
    ///   tranches), if still `queued`, records `head_since` and opens once
    ///   `quiet` holds or `SOUP_TRANCHE_QUIET_WAIT_MS` has run from
    ///   `head_since` — for mode `whole` through the TICK-DRIVEN drain
    ///   (`advance_head`): one tick raises `draining`, each
    ///   later tick evaluates `drained` once and logs the stranded `seq`s at
    ///   `warn`; the tick never waits, so enforcement runs every tick behind
    ///   one stranded writer as it does behind none.
    /// - The same-service tripwire: a `queued` tranche whose head is a
    ///   non-terminal tranche of the SAME `holder_service` (a nested type-3
    ///   that began its own tranche — the queue would never advance) is
    ///   logged at `warn` once per `same_service_warn_interval` per pair.
    ///
    /// - The lander: for every `landing` tranche with no
    ///   lander running in this process (the registry) and no retry backoff
    ///   still running (`landing_error.next_retry_ms() > now`), ONE lander
    ///   thread is spawned — the tick returns without waiting for it. The
    ///   lander takes the tranche's mutex with a non-blocking try and runs
    ///   the landing (`exec::land`); a skip on contention records nothing and
    ///   advances no backoff. The landing is the scheduler's task, never the
    ///   `commit` request's: a holder gone after its flip still lands.
    ///
    /// - The orphan-lock sweep (the scheduler also runs the spine's
    ///   orphan-lock sweep on every tick): LAST, after the
    ///   landers are spawned, and as a WRITE only when a read finds an orphan
    ///   — a lock row whose holding `transactions` row is cleared (a crash
    ///   between a cleared flip and its release). A tick behind a held spine
    ///   must still spawn the lander (test g13), so the sweep never takes
    ///   the write lock for nothing. By `seq` only: the anchor row is
    ///   un-cleared for the tranche's life, so no non-terminal tranche's rows
    ///   — its slice rows included — are ever swept.
    pub fn tranche_tick(&self, quiet: &QuietLevels, now_ms: i64) -> Result<TickReport> {
        crate::fault::gate()?;
        let cfg = self.config().tranche.clone();
        let mut report = TickReport::default();
        let mut spine = self.spine()?;
        // The heartbeat across a halt (`halts_seen`): a halted
        // spine is noted (the gates below still refuse the tick `dead`); the
        // first un-halted tick after one re-arms the deadlines BEFORE
        // enforcement reads them.
        if spine.halted()?.is_some() {
            note_halt_seen(spine.conn());
        } else if take_halt_seen(spine.conn()) {
            let tx = spine
                .conn_mut()
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            halt_gate(&tx)?;
            report.deadlines_rearmed = tx.execute(
                &format!("UPDATE tranches SET deadline = MAX(deadline, ?1) WHERE state IN {LIVE_STATES}"),
                params![now_ms.saturating_add(ms(cfg.timeout))],
            )?;
            tx.commit()?;
        }
        // Enforcement.
        let lapsed: Vec<Row> = {
            let tx = spine.conn_mut().transaction()?;
            let rows = rows_where(
                &tx,
                "deadline < ?1 AND state IN ('queued','open','staging') ORDER BY seq",
                &[&now_ms],
            )?;
            drop(tx);
            rows
        };
        for row in lapsed {
            let m = tranche_mutex(self, &row.id, row.attempt);
            let _guard = match m.try_lock() {
                Ok(g) => g,
                Err(std::sync::TryLockError::WouldBlock) => {
                    report.skipped_busy.push((row.id.clone(), row.attempt));
                    continue;
                }
                Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            };
            let tx = spine
                .conn_mut()
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            halt_gate(&tx)?;
            let won = tx.execute(
                "UPDATE tranches SET state = 'abandoned', reason = 'heartbeat_lost', ended_at = ?3
                 WHERE id = ?1 AND attempt = ?2 AND deadline < ?3
                   AND state IN ('queued','open','staging')",
                params![row.id, row.attempt, now_ms],
            )? == 1;
            if won {
                let fresh = one_row(&tx, &row.id, row.attempt)?;
                release_into_store(&tx, &fresh, TrancheState::Abandoned, "heartbeat_lost")?;
                report.abandoned.push((row.id.clone(), row.attempt));
            }
            tx.commit()?;
        }
        // Queue. A WRITE transaction only when the head is `queued` and may
        // move; otherwise a read — the tick never takes the spine's write
        // lock for nothing (a tick behind a held spine must still spawn the
        // lander and read the tripwire; test g13).
        let head_queued = {
            let tx = spine.conn_mut().transaction()?;
            matches!(head_of_queue(&tx)?, Some(h) if h.state == TrancheState::Queued)
        };
        let tx = if head_queued {
            spine
                .conn_mut()
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?
        } else {
            spine.conn_mut().transaction()?
        };
        halt_gate(&tx)?;
        if let Some(head) = head_of_queue(&tx)? {
            if head.state == TrancheState::Queued && head_queued {
                match advance_head(&tx, &head, quiet, &cfg, now_ms, false)? {
                    HeadAdvance::Opened => report.opened.push((head.id.clone(), head.attempt)),
                    HeadAdvance::Waiting => {}
                    HeadAdvance::Draining(holding) => {
                        if !holding.is_empty() {
                            let line = format!(
                                "spine: tranche {} attempt {} is queued at the head with the gate up, \
                                 draining: granted write holders still landing under seq {} (a \
                                 holder that never clears is a stranded commit — \
                                 Cauldron::recover() is its repair)",
                                head.id,
                                head.attempt,
                                holding.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(", ")
                            );
                            eprintln!("{line}");
                            report.warnings.push(line);
                        }
                        report.draining.push((head.id.clone(), head.attempt, holding));
                    }
                }
            }
            // The same-service tripwire, over every queued
            // tranche behind this head.
            if !head.state.is_terminal() {
                let behind = rows_where(
                    &tx,
                    "state = 'queued' AND seq > ?1 AND holder_service = ?2 ORDER BY seq",
                    &[&head.seq, &head.holder_service],
                )?;
                let interval = ms(cfg.same_service_warn_interval);
                for q in behind {
                    let key = lander_key(self, &q.id, q.attempt);
                    let due = {
                        let mut map = same_service_warned().lock().unwrap_or_else(|p| p.into_inner());
                        match map.get(&key) {
                            Some(last) if now_ms.saturating_sub(*last) < interval => false,
                            _ => {
                                map.insert(key, now_ms);
                                true
                            }
                        }
                    };
                    if due {
                        let line = format!(
                            "spine: tranche {} queued behind {} of the same service for {}s \
                             (holder_service {}; a nested type-three run that began its own tranche \
                             never opens — the runner carries one key per top-level run)",
                            q.id,
                            head.id,
                            now_ms.saturating_sub(q.began_at) / 1000,
                            q.holder_service
                        );
                        eprintln!("{line}");
                        report.warnings.push(line);
                    }
                }
            }
        }
        tx.commit()?;
        // The lander, for every `landing` tranche.
        let landing: Vec<Row> = {
            let tx = spine.conn_mut().transaction()?;
            let rows = rows_where(&tx, "state = 'landing' ORDER BY seq", &[])?;
            drop(tx);
            rows
        };
        for row in landing {
            if let Some(err) = row.landing_error()? {
                if err.next_retry_ms() > now_ms {
                    report.landers_backing_off.push((row.id.clone(), row.attempt));
                    continue;
                }
            }
            let key = lander_key(self, &row.id, row.attempt);
            let fresh = landers()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(key.clone());
            if !fresh {
                report.landers_running.push((row.id.clone(), row.attempt));
                continue;
            }
            let ticket = LanderTicket(key);
            let cauldron = self.clone();
            let (id, attempt) = (row.id.clone(), row.attempt);
            let spawned = std::thread::Builder::new()
                .name(format!("spine-lander-{id}-{attempt}"))
                .spawn(move || {
                    let _ticket = ticket;
                    run_lander(&cauldron, &id, attempt, now_ms);
                });
            match spawned {
                Ok(_) => report.landers_spawned.push((row.id.clone(), row.attempt)),
                // The ticket was moved into the closure that never ran;
                // Rust drops it on this error path, freeing the registry.
                Err(e) => {
                    return Err(SpineError::hard(format!(
                        "the lander for tranche `{}` attempt {} could not be spawned: {e}",
                        row.id, row.attempt
                    )))
                }
            }
        }
        // The orphan-lock sweep — a read first, the
        // one-statement release only when there is something to release.
        report.orphans_swept = sweep_orphans(&spine)?;
        Ok(report)
    }

    /// `heartbeat`: ONE conditional update — `last_heartbeat
    /// = now, deadline = now + timeout` while the tranche is non-terminal —
    /// in an IMMEDIATE transaction, so it serialises with the tick's abandon:
    /// a beat accepted is a beat that counted. Zero rows → `refused
    /// tranche_terminal {state, reason, last_heartbeat, deadline, ended_at}`.
    /// Runs outside the tranche's mutex. `quiet` is the caller's
    /// current levels, shown back while the tranche waits at the head.
    pub fn tranche_heartbeat(
        &self,
        actor: &TrancheActor,
        quiet: &QuietLevels,
        now_ms: i64,
    ) -> Result<TrancheRecord> {
        crate::fault::gate()?;
        let cfg = self.config().tranche.clone();
        let mut spine = self.spine()?;
        let tx = spine
            .conn_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        halt_gate(&tx)?;
        let row = admit(&tx, actor)?;
        let beat = tx.execute(
            &format!(
                "UPDATE tranches SET last_heartbeat = ?3, deadline = ?4
                 WHERE id = ?1 AND attempt = ?2 AND state IN {LIVE_STATES}"
            ),
            params![
                row.id,
                row.attempt,
                now_ms,
                now_ms.saturating_add(ms(cfg.timeout))
            ],
        )?;
        let row = one_row(&tx, &row.id, row.attempt)?;
        if beat == 0 {
            drop(tx);
            return Err(SpineError::refuse(terminal_witness(&row)));
        }
        let position = if row.state == TrancheState::Queued {
            Some(position_of(&tx, row.seq)?)
        } else {
            None
        };
        // `quiet` and `draining` never appear together — once the
        // gate is up the quiet start is over.
        let quiet_shown = match (position, quiet, row.head_since) {
            _ if row.draining => None,
            (
                Some(1),
                QuietLevels::Levels {
                    trigger_host_pending,
                    runner_pending,
                },
                Some(since),
            ) => Some(QuietShown {
                trigger_host_pending: *trigger_host_pending,
                runner_pending: *runner_pending,
                waited_ms: now_ms.saturating_sub(since),
            }),
            _ => None,
        };
        let record = TrancheRecord {
            tranche: row.id.clone(),
            attempt: row.attempt,
            state: row.state,
            lock_mode: row.lock_mode,
            position,
            holder_service: row.holder_service.clone(),
            staged: row.staged,
            began_at: row.began_at,
            last_heartbeat: row.last_heartbeat,
            deadline: row.deadline,
            quiet: quiet_shown,
            decided: row.decided,
            landed_children: row.landed_children,
            landing_error: row.landing_error()?,
            draining: row.state == TrancheState::Queued && row.draining,
        };
        tx.commit()?;
        Ok(record)
    }

    /// `rollback`, under the tranche's mutex: one
    /// IMMEDIATE transaction, the conditional flip FIRST (`state IN
    /// ('queued','open','staging')` → `rolled-back, reason: holder`), and
    /// behind a won flip, in the same transaction, the release — anchor
    /// aborted, every lock row under its `seq` deleted, children kept as the
    /// store. Zero rows → the state found answers: `rolled-back` the same
    /// reply (idempotent), `landing` `refused tranche_past_rollback`, any
    /// other end `refused tranche_terminal`.
    pub fn tranche_rollback(&self, actor: &TrancheActor, now_ms: i64) -> Result<TrancheRolledBack> {
        crate::fault::gate()?;
        let mut spine = self.spine()?;
        let (id, attempt) = {
            let tx = spine.conn_mut().transaction()?;
            let row = admit(&tx, actor)?;
            (row.id, row.attempt)
        };
        let m = tranche_mutex(self, &id, attempt);
        let mut slot = m.lock().unwrap_or_else(|p| p.into_inner());
        let tx = spine
            .conn_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        halt_gate(&tx)?;
        let won = flip_state(
            &tx,
            &id,
            attempt,
            &[TrancheState::Queued, TrancheState::Open, TrancheState::Staging],
            TrancheState::RolledBack,
            "reason = 'holder', ended_at = ?4",
            &[&now_ms],
        )?;
        let row = one_row(&tx, &id, attempt)?;
        let stored = if won {
            // Durability: the view is dropped at the terminal
            // transition.
            slot.view = None;
            let stored = release_into_store(&tx, &row, TrancheState::RolledBack, "holder")?;
            // [tranche stage: in rollback] — the release's statements issued,
            // its ONE transaction not committed: a death here commits nothing
            // and the tranche is as before.
            crate::fault::at_tranche_stage(TrancheStage::InRollback)?;
            stored
        } else {
            match row.state {
                TrancheState::RolledBack => stored_count(&tx, &id, attempt)?,
                TrancheState::Landing => {
                    return Err(SpineError::refuse(Witness::TranchePastRollback {
                        tranche: id,
                        attempt,
                    }))
                }
                _ => return Err(SpineError::refuse(terminal_witness(&row))),
            }
        };
        tx.commit()?;
        Ok(TrancheRolledBack {
            state: TrancheState::RolledBack,
            tranche: id,
            attempt,
            stored,
        })
    }

    /// `lock`: switch the lock mode mid-tranche,
    /// from the type-three process. The key admits the attempt; legality is
    /// judged at the door and again inside the mutex (twice); the
    /// mode already held answers `switched: false`.
    ///
    /// **`whole → touched`**: one conditional `UPDATE … lock_mode =
    /// 'touched' WHERE state IN ('open','staging')`. The gate is down from
    /// that commit; what is released is everything NOT touched. Instant,
    /// never refused, and the lock table is untouched — the touched set was
    /// recorded under the gate all along (claims are enrolled under
    /// `whole` too), so nothing is re-derived.
    ///
    /// **`touched → whole`**: the same conditional update to `whole` (the
    /// three checkpoints are live from that commit), then THE BOUNDED DRAIN
    /// under the mutex: [`drained`] polled with the spine's backoff
    /// (`lock_poll_interval` doubling to 32×) up to `lock_wait_ceiling`,
    /// every poll refreshing `last_heartbeat`/`deadline` exactly as a
    /// claimant's poll does (an act in progress is proof of life, so a
    /// switch that outlasts the heartbeat timeout
    /// is not abandoned by the next tick). Empty → `switched: true`. At the
    /// ceiling → the mode is set back to `touched` durably and the answer
    /// is `busy tranche_busy / switch` with `holding: [seq…]`, the tranche
    /// as it was. Neither direction releases, re-derives or re-enrols a
    /// claim.
    pub fn tranche_lock(
        &self,
        actor: &TrancheActor,
        mode: TrancheLockMode,
        now_ms: i64,
    ) -> Result<TrancheLocked> {
        crate::fault::gate()?;
        let config = self.config().clone();
        let timeout = ms(config.tranche.timeout);
        let mut spine = self.spine()?;
        // THE FIRST legality check, at the door (it runs twice).
        let (id, attempt) = {
            let tx = spine.conn_mut().transaction()?;
            let row = admit(&tx, actor)?;
            legality_for_act(&tx, &row)?;
            (row.id, row.attempt)
        };
        let m = tranche_mutex(self, &id, attempt);
        let _slot = m.lock().unwrap_or_else(|p| p.into_inner());
        // THE SECOND, inside the mutex, in the write's own transaction.
        let row = {
            let tx = spine
                .conn_mut()
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            halt_gate(&tx)?;
            let row = one_row(&tx, &id, attempt)?;
            legality_for_act(&tx, &row)?;
            if row.lock_mode == mode {
                drop(tx);
                return Ok(TrancheLocked {
                    state: row.state,
                    lock_mode: row.lock_mode,
                    switched: false,
                });
            }
            // Both directions write `lock_mode` conditional on the
            // state, like every flip; zero rows → the legality check answers.
            let changed = tx.execute(
                "UPDATE tranches SET lock_mode = ?3
                 WHERE id = ?1 AND attempt = ?2 AND state IN ('open','staging')",
                params![id, attempt, mode.as_str()],
            )?;
            if changed != 1 {
                let fresh = one_row(&tx, &id, attempt)?;
                legality_for_act(&tx, &fresh)?;
                return Err(SpineError::hard(format!(
                    "tranche `{id}` attempt {attempt}: the mode write changed {changed} rows while \
                     the state was `{}`",
                    fresh.state.as_str()
                )));
            }
            let row = one_row(&tx, &id, attempt)?;
            tx.commit()?;
            row
        };
        if mode == TrancheLockMode::Touched {
            // The gate is down from the commit above; nothing else moves.
            return Ok(TrancheLocked {
                state: row.state,
                lock_mode: row.lock_mode,
                switched: true,
            });
        }
        // `touched → whole`: THE BOUNDED DRAIN, the gate already up.
        let started = std::time::Instant::now();
        let mut interval = config.lock_poll_interval;
        let max_interval = config.lock_poll_interval * 32;
        loop {
            let holding = {
                let tx = spine
                    .conn_mut()
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let holding = drained(&tx, row.seq)?;
                // Every poll of the drain is a beat.
                let now = now_ms.saturating_add(ms(started.elapsed()));
                tx.execute(
                    &format!(
                        "UPDATE tranches SET last_heartbeat = ?3, deadline = ?4
                         WHERE id = ?1 AND attempt = ?2 AND state IN {LIVE_STATES}"
                    ),
                    params![id, attempt, now, now.saturating_add(timeout)],
                )?;
                tx.commit()?;
                holding
            };
            if holding.is_empty() {
                let tx = spine.conn_mut().transaction()?;
                let fresh = one_row(&tx, &id, attempt)?;
                return Ok(TrancheLocked {
                    state: fresh.state,
                    lock_mode: fresh.lock_mode,
                    switched: true,
                });
            }
            if started.elapsed() >= config.lock_wait_ceiling {
                // The ceiling: back to `touched`, durably; the tranche as it
                // was; `busy switch` naming the writers still holding.
                let tx = spine
                    .conn_mut()
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                tx.execute(
                    "UPDATE tranches SET lock_mode = 'touched'
                     WHERE id = ?1 AND attempt = ?2 AND state IN ('open','staging')",
                    params![id, attempt],
                )?;
                tx.commit()?;
                let mut fields = serde_json::Map::new();
                fields.insert("holding".into(), json!(holding));
                return Err(SpineError::TrancheBusy {
                    why: "switch".to_string(),
                    fields,
                });
            }
            std::thread::sleep(interval);
            interval = (interval * 2).min(max_interval);
        }
    }

    /// `commit`: THE FLIP, then THE JOIN.
    ///
    /// The key admits the attempt; legality is judged at the door — `queued`
    /// → `busy queued`; `rolled-back` / `abandoned` → `refused
    /// tranche_terminal`; `failed` → `refused` with `tranche_terminal
    /// {reason: landing_refused}` AND `tranche_landing_refused`; `landing` /
    /// `landed` → straight to the join. In `open` or `staging`: a standing
    /// quiesce refuses with the spine's quiesced `busy` before anything
    /// changes (the holder keeps its rollback); then, under the
    /// tranche's mutex, ONE immediate transaction makes the conditional flip
    /// `open | staging → landing` — zero rows means the state moved while the
    /// request waited for the mutex, and the state found answers. The
    /// mutex is released and the request JOINS: it waits on the record, with
    /// no ceiling of its own, until the tranche is `landed` (the reply,
    /// computed from the record alone — byte-identical on every repeat),
    /// `failed` (the two witnesses) or carries a `landing_error` (`busy
    /// tranche_busy / landing_in_progress` carrying it; the holder re-issues
    /// after one interval). The landing itself is the scheduler's
    /// (`tranche_tick`), never this request's: a caller that drops the join
    /// changes nothing. From the flip the holder cannot roll back and the
    /// heartbeat is not enforced.
    pub fn tranche_commit(&self, actor: &TrancheActor, now_ms: i64) -> Result<TrancheCommitted> {
        crate::fault::gate()?;
        let config = self.config().clone();
        let mut spine = self.spine()?;
        // THE FIRST legality check, at the door (it runs twice).
        let (id, attempt, state) = {
            let tx = spine.conn_mut().transaction()?;
            // A halted spine refuses every act (`dead`) —
            // including a commit on a `landing` tranche, which would
            // otherwise join a record no lander can move while halted.
            halt_gate(&tx)?;
            let row = admit(&tx, actor)?;
            legality_for_commit(&tx, &row)?;
            (row.id, row.attempt, row.state)
        };
        if matches!(state, TrancheState::Open | TrancheState::Staging) {
            // A standing quiesce refuses the entry before anything
            // changes — the courtesy read; the same gate stands inside the
            // flip's transaction below.
            let standing = spine.quiescers()?;
            if !standing.is_empty() {
                return Err(crate::recover::quiesced_busy(&standing));
            }
            let m = tranche_mutex(self, &id, attempt);
            let _slot = m.lock().unwrap_or_else(|p| p.into_inner());
            let tx = spine
                .conn_mut()
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            halt_gate(&tx)?;
            {
                let standing: i64 = tx.query_row("SELECT COUNT(*) FROM quiescers", [], |r| r.get(0))?;
                if standing != 0 {
                    let q = crate::spine_db::quiescers_in(&tx)?;
                    return Err(crate::recover::quiesced_busy(&q));
                }
            }
            // The flip is an act under the key, so it is a beat (any
            // other act under the key is) — the holder's last proof of life
            // before the landing becomes the scheduler's. The LANDER never
            // touches these two columns.
            let deadline = now_ms.saturating_add(ms(self.config().tranche.timeout));
            let won = flip_state(
                &tx,
                &id,
                attempt,
                &[TrancheState::Open, TrancheState::Staging],
                TrancheState::Landing,
                "last_heartbeat = ?4, deadline = ?5",
                &[&now_ms, &deadline],
            )?;
            if !won {
                // The state moved while this request waited for the mutex:
                // the state found answers.
                let row = one_row(&tx, &id, attempt)?;
                legality_for_commit(&tx, &row)?;
            }
            tx.commit()?;
        }
        drop(spine);
        self.join_landing(&id, attempt, &config)
    }

    /// THE JOIN: wait on the record — no mutex, no ceiling — and
    /// answer from it. Polls at the lock poll interval, backing off to 32×;
    /// every poll reads the halt first, so a halt wakes every joiner `dead`
    /// within one (capped) interval.
    fn join_landing(&self, id: &str, attempt: u32, config: &SpineConfig) -> Result<TrancheCommitted> {
        let mut interval = config.lock_poll_interval;
        let max_interval = config.lock_poll_interval * 32;
        loop {
            let mut spine = self.spine()?;
            let tx = spine.conn_mut().transaction()?;
            // "the spine is halted → dead", on EVERY poll (never
            // block): a commit whose join began BEFORE a halt
            // answers `dead` within one poll interval of it, never only after
            // the repair. The halt may be written by another process, so the
            // record is the only signal — every joiner reads it here. The
            // same order as the door: the halt first, every answer after it.
            halt_gate(&tx)?;
            let row = one_row(&tx, id, attempt)?;
            match row.state {
                TrancheState::Landed => return committed_reply(&tx, &row),
                TrancheState::Landing => {
                    if let Some(err) = row.landing_error()? {
                        let mut fields = serde_json::Map::new();
                        fields.insert(
                            "landing_error".into(),
                            serde_json::to_value(&err).map_err(|e| {
                                SpineError::hard(format!("landing_error will not serialize: {e}"))
                            })?,
                        );
                        return Err(SpineError::TrancheBusy {
                            why: "landing_in_progress".to_string(),
                            fields,
                        });
                    }
                }
                _ => legality_for_commit(&tx, &row)?,
            }
            drop(tx);
            drop(spine);
            std::thread::sleep(interval);
            interval = (interval * 2).min(max_interval);
        }
    }

    /// Read the store: attempt `n` of `T`, or the latest.
    /// No key. A terminal attempt answers its end, its reason and its
    /// children `stored`; a live one the same record without `refusal` and
    /// with each child `staged`. Never resurrects anything: it reads.
    pub fn tranche_store(&self, tranche: &str, attempt: Option<u32>) -> Result<TrancheStore> {
        let mut spine = self.spine()?;
        let tx = spine.conn_mut().transaction()?;
        let row = match attempt {
            Some(n) => rows_where(&tx, "id = ?1 AND attempt = ?2", &[&tranche, &n])?.pop(),
            None => rows_where(&tx, "id = ?1 ORDER BY attempt DESC LIMIT 1", &[&tranche])?.pop(),
        }
        .ok_or_else(|| {
            SpineError::not_found(match attempt {
                Some(n) => format!("the spine holds no tranche `{tranche}` attempt {n}"),
                None => format!("the spine holds no tranche `{tranche}`"),
            })
        })?;
        let live = !row.state.is_terminal();
        let mut children = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT ordinal, hash, envelope, payload, state FROM tranche_children
                 WHERE tranche_id = ?1 AND tranche_attempt = ?2 ORDER BY ordinal",
            )?;
            let rows = stmt.query_map(params![row.id, row.attempt], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?;
            for r in rows {
                let (ordinal, hash, envelope, payload, child_state) = r?;
                let env: Envelope = serde_json::from_str(&envelope).map_err(|e| {
                    SpineError::hard(format!(
                        "tranche `{}` attempt {} child {ordinal}: unparseable envelope: {e}",
                        row.id, row.attempt
                    ))
                })?;
                let payload: CompiledPayload = serde_json::from_str(&payload).map_err(|e| {
                    SpineError::hard(format!(
                        "tranche `{}` attempt {} child {ordinal}: unparseable payload: {e}",
                        row.id, row.attempt
                    ))
                })?;
                children.push(StoredChild {
                    ordinal: ordinal as u32,
                    tx: hash,
                    statement: env.statement,
                    bindings: env.bindings,
                    format_version: env.format_version,
                    ripple: serde_json::to_value(&env.provenance.initiator).map_err(|e| {
                        SpineError::hard(format!("a child's initiator will not serialize: {e}"))
                    })?,
                    instrument: env.provenance.instrument,
                    idempotency_nonce: env.idempotency_nonce,
                    minted: payload
                        .minted
                        .iter()
                        .map(|(k, v)| (k.clone(), v.to_string()))
                        .collect(),
                    // A live attempt's children are `staged`; a terminal
                    // one's are `stored` — except a LANDED attempt's,
                    // which are the commits (`landed` / `landed_elsewhere`)
                    // and are never "a failed tranche location".
                    state: if live {
                        "staged".to_string()
                    } else if row.state == TrancheState::Landed {
                        child_state
                    } else {
                        "stored".to_string()
                    },
                });
            }
        }
        let refusal = if live {
            None
        } else {
            match &row.refusal {
                None => None,
                Some(text) => Some(serde_json::from_str(text).map_err(|e| {
                    SpineError::hard(format!(
                        "tranche `{}` attempt {}: unparseable refusal: {e}",
                        row.id, row.attempt
                    ))
                })?),
            }
        };
        Ok(TrancheStore {
            tranche: row.id,
            attempt: row.attempt,
            state: row.state,
            reason: row.reason,
            lock_mode: row.lock_mode,
            regions: row.regions,
            began_at: row.began_at,
            ended_at: row.ended_at,
            last_heartbeat: row.last_heartbeat,
            holder_service: row.holder_service,
            refusal,
            children,
        })
    }
}

// ---------------------------------------------------------------------------
// staging and reads under key
// ---------------------------------------------------------------------------

/// A child's reply, as spine values — the door's JSON is built
/// elsewhere. `minted` maps each `CREATE` binding to `<pool>/<provisional id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildOutcome {
    /// `{ tx, staged: true, ordinal, minted, changed }` — `changed` is true
    /// when the payload carries a node creation, a field set or a seal (a
    /// body write); false for a statement that only adds or removes edges.
    Staged {
        tx: TxHash,
        ordinal: u32,
        minted: BTreeMap<String, String>,
        changed: bool,
    },
    /// Deduplicated against THIS tranche's own children: `{ tx,
    /// staged: true, ordinal: <the original's>, deduplicated: true, minted }`.
    Deduplicated {
        tx: TxHash,
        ordinal: u32,
        minted: BTreeMap<String, String>,
    },
    /// Deduplicated against a step already LANDED on the soup: `{ tx,
    /// committed: true, deduplicated: true, seq, minted }`. Not a child; no
    /// ordinal.
    LandedElsewhere {
        tx: TxHash,
        seq: Seq,
        minted: BTreeMap<String, String>,
    },
    /// `mode: validate` — `{ tx, validated: true }`: the same validator,
    /// against the view, staging nothing.
    Validated { tx: TxHash },
}

/// What `exec::stage` needs from the record, under the mutex.
pub(crate) struct StageCtx<'a> {
    pub tranche: String,
    pub attempt: u32,
    pub seq: Seq,
    pub view: &'a mut View,
    pub now_ms: i64,
    pub timeout_ms: i64,
    pub max_children: u32,
    /// Set by `exec::stage` when the overlay apply failed AFTER the durable
    /// stage: the view no longer matches the rows and must be rebuilt.
    pub view_dirty: bool,
}

/// The view of attempt `(T, n)`, built or re-synced from the rows
/// (durability): absent in this process, or holding a different number of
/// children than the record says — a rebuild from the `tranche_children`
/// rows, in ordinal order.
fn ensure_view<'s>(
    cauldron: &Cauldron,
    slot: &'s mut Slot,
    tranche: &str,
    attempt: u32,
    staged: u32,
) -> Result<&'s mut View> {
    let stale = match &slot.view {
        None => true,
        Some(v) => v.children_seen() != staged,
    };
    if stale {
        slot.view = Some(View::rebuild(cauldron, tranche, attempt)?);
    }
    let view = slot.view.as_mut().expect("set above");
    if view.children_seen() != staged {
        return Err(SpineError::hard(format!(
            "tranche `{tranche}` attempt {attempt}: the record says {staged} children are staged but \
             the rebuilt view holds {}; the record is torn",
            view.children_seen()
        )));
    }
    Ok(view)
}

/// **THE OPEN'S REBUILD, IN ONE SNAPSHOT**. The
/// row's `staged` and the `tranche_children` rows are read inside ONE read
/// transaction (WAL: one snapshot from its first read), then replayed and
/// compared: a child staged by a live holder beside this open lands in both
/// reads or in neither, so a consistent record is never called torn. Before
/// this, `staged` was read before the mutex and the children after it, in
/// two snapshots, and an open beside a live stager was refused "the record
/// is torn" while the record was sound. A GENUINELY torn record (`staged` ≠
/// the rows, or a gap in the ordinals) is still refused by name. Returns the
/// `staged` it compared against. The caller holds the tranche's mutex.
fn rebuild_in_one_snapshot(
    cauldron: &Cauldron,
    slot: &mut Slot,
    tranche: &str,
    attempt: u32,
) -> Result<u32> {
    let (staged, rows) = {
        let mut spine = cauldron.spine()?;
        let tx = spine.conn_mut().transaction()?;
        let staged = one_row(&tx, tranche, attempt)?.staged;
        let rows = View::children_rows(&tx, tranche, attempt)?;
        drop(tx);
        (staged, rows)
    };
    let view = View::replay(cauldron, tranche, attempt, rows)?;
    if view.children_seen() != staged {
        return Err(SpineError::hard(format!(
            "tranche `{tranche}` attempt {attempt}: the record says {staged} children are staged but \
             the rebuilt view holds {}; the record is torn",
            view.children_seen()
        )));
    }
    slot.view = Some(view);
    Ok(staged)
}

impl Cauldron {
    /// STAGE A CHILD. The key admits the attempt, legality is judged
    /// before the mutex, then again inside it; under
    /// the mutex the child runs the staging pipeline (`exec::stage`) against the
    /// view and stages durably, touching no pool file. `now_ms` is the
    /// daemon clock; a claim wait refreshes the heartbeat from it.
    pub fn tranche_stage(
        &self,
        actor: &TrancheActor,
        envelope: Envelope,
        now_ms: i64,
    ) -> Result<ChildOutcome> {
        crate::fault::gate()?;
        let cfg = self.config().tranche.clone();
        let mut spine = self.spine()?;
        // The FIRST legality check, at the door (it runs twice).
        let (id, attempt) = {
            let tx = spine.conn_mut().transaction()?;
            let row = admit(&tx, actor)?;
            legality_for_act(&tx, &row)?;
            size_check(&row, cfg.max_children)?;
            (row.id, row.attempt)
        };
        let m = tranche_mutex(self, &id, attempt);
        let mut slot = m.lock().unwrap_or_else(|p| p.into_inner());
        // The SECOND, inside the mutex: the state and the size as they are
        // now that nothing else under this key can move them.
        let row = {
            let tx = spine.conn_mut().transaction()?;
            let row = one_row(&tx, &id, attempt)?;
            legality_for_act(&tx, &row)?;
            size_check(&row, cfg.max_children)?;
            row
        };
        drop(spine);
        let view = ensure_view(self, &mut slot, &id, attempt, row.staged)?;
        let mut ctx = StageCtx {
            tranche: id.clone(),
            attempt,
            seq: Seq(row.seq),
            view,
            now_ms,
            timeout_ms: ms(cfg.timeout),
            max_children: cfg.max_children,
            view_dirty: false,
        };
        let outcome = crate::exec::stage(self, &mut ctx, envelope);
        if ctx.view_dirty {
            slot.view = None;
        }
        if matches!(outcome, Ok(ChildOutcome::Staged { .. })) {
            // [tranche stage: after stage] — the durable stage's transaction
            // committed, the reply not yet answered.
            crate::fault::at_tranche_stage(TrancheStage::AfterStage)?;
        }
        outcome
    }

    /// READ UNDER KEY: the ordinary read against
    /// the view, which LOCKS WHAT IT READ under the tranche's `seq` with no
    /// release, under the mutex. A read WITHOUT the key is
    /// [`Cauldron::read`], the committed read.
    pub fn tranche_read(
        &self,
        actor: &TrancheActor,
        request: &crate::ReadRequest,
        now_ms: i64,
    ) -> Result<TrancheRead> {
        let cfg = self.config().tranche.clone();
        let config = self.config().clone();
        let mut spine = self.spine()?;
        let (id, attempt) = {
            let tx = spine.conn_mut().transaction()?;
            let row = admit(&tx, actor)?;
            legality_for_act(&tx, &row)?;
            (row.id, row.attempt)
        };
        let m = tranche_mutex(self, &id, attempt);
        let mut slot = m.lock().unwrap_or_else(|p| p.into_inner());
        let row = {
            let tx = spine.conn_mut().transaction()?;
            let row = one_row(&tx, &id, attempt)?;
            legality_for_act(&tx, &row)?;
            row
        };
        let view = ensure_view(self, &mut slot, &id, attempt, row.staged)?;
        let seq = Seq(row.seq);
        let claimant = TrancheClaimant {
            now_ms,
            timeout_ms: ms(cfg.timeout),
            started: std::time::Instant::now(),
        };
        let mut acquire = |claims: &ClaimSet| -> Result<()> {
            crate::locks::acquire_for_tranche(&spine, seq, claims, &config, &claimant).map(|_| ())
        };
        crate::read::read_under(self, view, request, &mut acquire)
    }

    /// THE TEST SEAM for claim rule 2 and the legality checks (a test that
    /// needs a `landing` state for rule 2 sets it through a test-only seam,
    /// shown unreachable in a release build): write `state` on attempt
    /// `(T, n)` directly, nothing else. `pub` ONLY under `test-support` and
    /// compiled out of every other build — no production consumer has it.
    /// `commit` makes the real `landing` flip.
    #[cfg(feature = "test-support")]
    pub fn tranche_force_state(&self, tranche: &str, attempt: u32, state: TrancheState) -> Result<()> {
        let mut spine = self.spine()?;
        let tx = spine
            .conn_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "UPDATE tranches SET state = ?3 WHERE id = ?1 AND attempt = ?2",
            params![tranche, attempt, state.as_str()],
        )?;
        tx.commit()?;
        if n != 1 {
            return Err(SpineError::not_found(format!(
                "the spine holds no tranche `{tranche}` attempt {attempt}"
            )));
        }
        Ok(())
    }

    /// The lock rows under attempt `(T, n)`'s `seq` — the TOUCHED SET,
    /// as `(pool, resource key, mode, granted)`. A read of a fact the spine
    /// owns, for audits and for the tests that check what a child or a read
    /// enrolled.
    pub fn tranche_touched(&self, tranche: &str, attempt: u32) -> Result<Vec<(String, String, String, bool)>> {
        let spine = self.spine()?;
        let seq: Option<i64> = spine
            .conn()
            .query_row(
                "SELECT seq FROM tranches WHERE id = ?1 AND attempt = ?2",
                params![tranche, attempt],
                |r| r.get(0),
            )
            .optional()?;
        let Some(seq) = seq else {
            return Err(SpineError::not_found(format!(
                "the spine holds no tranche `{tranche}` attempt {attempt}"
            )));
        };
        let mut stmt = spine.conn().prepare(
            "SELECT pool, resource, mode, granted FROM locks WHERE seq = ?1 ORDER BY pool, resource, mode",
        )?;
        let rows = stmt.query_map(params![seq], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? != 0,
            ))
        })?;
        rows.collect::<std::result::Result<_, _>>().map_err(Into::into)
    }
}

// ---------------------------------------------------------------------------
// the lander's record side
// ---------------------------------------------------------------------------

/// What one lander run concluded (`exec::land`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LandOutcome {
    /// The clearing ran: the tranche is `landed`.
    Landed,
    /// Re-validation or the decision refused: the tranche is `failed`.
    Failed,
    /// The lander stopped short; the caller records the error and
    /// the scheduler retries with backoff.
    Stopped(LandingStop),
    /// The tranche was not `landing` when the lander looked (rolled back,
    /// landed by another, …): nothing recorded.
    NotLanding,
}

/// Why a lander stopped short: `child_in_flight`, `file_held`, or
/// `internal` (an error the design does not name, carried verbatim so a human
/// can repair what it names).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LandingStop {
    pub why: &'static str,
    pub at_child: Option<u32>,
    pub colliding_seq: Option<i64>,
    pub message: String,
}

/// THE LANDER THREAD'S BODY: take the mutex with a non-blocking try
/// (contention — a child or a read in flight — is a skip, not an error:
/// nothing recorded, no backoff advanced; the next tick spawns again), run
/// `exec::land`, and record what it concluded. A thread cannot return an
/// error to anyone, so a failure to RECORD is said on stderr and the next
/// tick retries from the record as it stands.
fn run_lander(cauldron: &Cauldron, tranche: &str, attempt: u32, now_ms: i64) {
    let m = tranche_mutex(cauldron, tranche, attempt);
    let mut slot = match m.try_lock() {
        Ok(g) => g,
        Err(std::sync::TryLockError::WouldBlock) => return,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
    };
    let outcome = crate::exec::land(cauldron, &mut slot, tranche, attempt, now_ms);
    drop(slot);
    let stop = match outcome {
        Ok(LandOutcome::Landed) | Ok(LandOutcome::Failed) | Ok(LandOutcome::NotLanding) => return,
        Ok(LandOutcome::Stopped(stop)) => stop,
        Err(e) => LandingStop {
            why: "internal",
            at_child: None,
            colliding_seq: None,
            message: e.message(),
        },
    };
    if let Err(e) = record_landing_error(cauldron, tranche, attempt, &stop, now_ms) {
        eprintln!(
            "spine: the lander for tranche `{tranche}` attempt {attempt} stopped ({}: {}) and the \
             error could not be recorded: {e}",
            stop.why, stop.message
        );
    }
}

/// Record `landing_error` on the `tranches` row — the previous error's
/// `retries` plus one, so the backoff doubles — only while the tranche is
/// still `landing`. The mutex is NOT held here: the
/// backoff runs between ticks, never under the mutex, so a `rollback`
/// arriving during it is answered `tranche_past_rollback` at once.
pub(crate) fn record_landing_error(
    cauldron: &Cauldron,
    tranche: &str,
    attempt: u32,
    stop: &LandingStop,
    now_ms: i64,
) -> Result<()> {
    let mut spine = cauldron.spine()?;
    let tx = spine
        .conn_mut()
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let row = one_row(&tx, tranche, attempt)?;
    let retries = row.landing_error()?.map(|e| e.retries).unwrap_or(0).saturating_add(1);
    let err = LandingError {
        why: stop.why.to_string(),
        at_child: stop.at_child,
        colliding_seq: stop.colliding_seq,
        message: stop.message.clone(),
        at_ms: now_ms,
        retries,
    };
    let json = serde_json::to_string(&err)
        .map_err(|e| SpineError::hard(format!("landing_error will not serialize: {e}")))?;
    tx.execute(
        "UPDATE tranches SET landing_error = ?3 WHERE id = ?1 AND attempt = ?2 AND state = 'landing'",
        params![tranche, attempt, json],
    )?;
    tx.commit()?;
    Ok(())
}

/// FAIL THE TRANCHE WHOLE before the decision —
/// one immediate transaction, conditional on `state = 'landing' AND decided
/// = 0`: `state = failed, reason = landing_refused, ended_at = now`, the
/// refusal recorded, then the release (anchor aborted, lock rows under
/// the anchor's `seq` deleted, children kept `stored`). `false` when the
/// state had moved (nothing written). The view is dropped: a terminal
/// transition.
pub(crate) fn fail_landing(
    cauldron: &Cauldron,
    slot: &mut Slot,
    tranche: &str,
    attempt: u32,
    refusal: &Witness,
    now_ms: i64,
) -> Result<bool> {
    let json = serde_json::to_string(refusal)
        .map_err(|e| SpineError::hard(format!("the landing refusal will not serialize: {e}")))?;
    let mut spine = cauldron.spine()?;
    let tx = spine
        .conn_mut()
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    halt_gate(&tx)?;
    let won = tx.execute(
        "UPDATE tranches SET state = 'failed', reason = 'landing_refused', ended_at = ?3,
                             refusal = ?4, landing_error = NULL
         WHERE id = ?1 AND attempt = ?2 AND state = 'landing' AND decided = 0",
        params![tranche, attempt, now_ms, json],
    )? == 1;
    if won {
        let row = one_row(&tx, tranche, attempt)?;
        release_into_store(&tx, &row, TrancheState::Failed, "landing_refused")?;
        slot.view = None;
    }
    tx.commit()?;
    Ok(won)
}

/// THE CLEARING: one immediate transaction, conditional on `state =
/// 'landing' AND decided = 1` (zero rows → nothing runs, `false`): the anchor
/// row cleared `decided = 0, cleared = 1` with `result = {children:
/// [{ordinal, seq, tx, minted}], renumbered}` (never a commit,
/// never counted, never published); every lock row under the anchor's `seq`
/// released; `state = landed, reason = landed, ended_at = now`. The gate is
/// down from that instant; the scheduler advances the queue on its next
/// tick. The view is dropped.
pub(crate) fn clear_landed(
    cauldron: &Cauldron,
    slot: &mut Slot,
    tranche: &str,
    attempt: u32,
    now_ms: i64,
) -> Result<bool> {
    let mut spine = cauldron.spine()?;
    let tx = spine
        .conn_mut()
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    halt_gate(&tx)?;
    let row = one_row(&tx, tranche, attempt)?;
    if row.state != TrancheState::Landing || !row.decided {
        return Ok(false);
    }
    let reply = committed_reply(&tx, &row)?;
    let result = json!({
        "aborted": true,
        "landed": false,
        "tranche": {
            "id": row.id,
            "attempt": row.attempt,
            "state": TrancheState::Landed.as_str(),
            "reason": "landed",
        },
        "children": reply.children.iter().map(|c| json!({
            "ordinal": c.ordinal,
            "seq": c.seq,
            "tx": c.tx,
            "minted": c.minted,
            "deduplicated": c.deduplicated,
        })).collect::<Vec<_>>(),
        "renumbered": reply.renumbered,
    });
    let anchored = tx.execute(
        "UPDATE transactions SET cleared = 1, result = ?2
         WHERE seq = ?1 AND decided = 0 AND cleared = 0",
        params![row.seq, result.to_string()],
    )?;
    if anchored != 1 {
        return Err(SpineError::hard(format!(
            "SPINE INTERNAL INVARIANT VIOLATED: tranche `{}` attempt {} reached the clearing but its anchor \
             row seq {} was not un-decided and un-cleared",
            row.id, row.attempt, row.seq
        )));
    }
    tx.execute("DELETE FROM locks WHERE seq = ?1", params![row.seq])?;
    let flipped = flip_state(
        &tx,
        tranche,
        attempt,
        &[TrancheState::Landing],
        TrancheState::Landed,
        "reason = 'landed', ended_at = ?4, landing_error = NULL",
        &[&now_ms],
    )?;
    if !flipped {
        return Err(SpineError::hard(format!(
            "SPINE INTERNAL INVARIANT VIOLATED: tranche `{}` attempt {} was `landing` a statement ago \
             and is not now, inside one immediate transaction",
            row.id, row.attempt
        )));
    }
    tx.commit()?;
    slot.view = None;
    Ok(true)
}

/// The size cap: the `max`-th child is refused `tranche_full` — `staged` children
/// already hold the tranche at its configured bound.
fn size_check(row: &Row, max: u32) -> Result<()> {
    if row.staged >= max {
        return Err(SpineError::refuse(Witness::TrancheFull {
            tranche: row.id.clone(),
            attempt: row.attempt,
            max,
        }));
    }
    Ok(())
}

fn stored_count(tx: &Transaction<'_>, tranche: &str, attempt: u32) -> Result<u32> {
    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM tranche_children WHERE tranche_id = ?1 AND tranche_attempt = ?2",
        params![tranche, attempt],
        |r| r.get(0),
    )?;
    Ok(n as u32)
}

/// THE ORPHAN-LOCK SWEEP ON THE TICK: a lock row whose
/// holding `transactions` row is already cleared is an orphan (a crash
/// between a cleared flip and its release — the door's or the ordinary
/// recovery pass's). Counted by a READ first; the one-statement release
/// (`locks::release_cleared_orphans`, the same sweep `recover` opens with)
/// runs only when the count is non-zero, so an ordinary tick takes no write
/// lock for it. Returns how many rows it released.
fn sweep_orphans(spine: &crate::spine_db::SpineDb) -> Result<usize> {
    let orphans: i64 = spine.conn().query_row(
        "SELECT COUNT(*) FROM locks WHERE seq IN (SELECT seq FROM transactions WHERE cleared = 1)",
        [],
        |r| r.get(0),
    )?;
    if orphans == 0 {
        return Ok(0);
    }
    crate::locks::release_cleared_orphans(spine)?;
    Ok(orphans as usize)
}

/// One non-terminal tranche as THE TRANCHE PASS at open found it — what `Cauldron::tranches_at_open` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredTranche {
    pub tranche: String,
    pub attempt: u32,
    pub state: TrancheState,
    pub lock_mode: TrancheLockMode,
    /// The queue position, RECOMPUTED from the anchor `seq`s (it is never
    /// stored, so there is nothing to rewrite — `position_of` derives it).
    pub position: u32,
    /// `boot + timeout`, as the pass reset it.
    pub deadline: i64,
    /// `staged` on the row — and, for an `open`/`staging` tranche, the number
    /// of children the rebuilt overlay replayed (equal, or the open refuses).
    pub staged: u32,
    pub overlay_rebuilt: bool,
    /// `queued` at the head with the gate up: PRESERVED by the
    /// pass, never cleared — the gate is up from the first door request.
    pub draining: bool,
    pub decided: bool,
}

/// THE OVERLAY HALF OF THE TRANCHE PASS (it rebuilds every non-terminal
/// tranche's overlay by replaying the staging rows): for every
/// `open`/`staging` tranche, the view is rebuilt into
/// its mutex slot through the ONE rebuild (`View::rebuild` — the
/// same function the first act under a key would call lazily), so the
/// holder's first child after the restart matches against the staged
/// deltas and a torn record (ordinals not contiguous, `staged` ≠ children
/// replayed) is refused AT OPEN rather than at the holder's next act. A
/// `queued` tranche has no children and a `landing` one is the lander's
/// (nothing at open — the lander builds its own view at re-validation), so
/// neither is touched. Writes NO pool file and NO spine row; lands nothing.
///
/// Returns every non-terminal row, with what the pass did to it. Run by
/// `recover::tranche_pass` AFTER `reset_deadlines_at_open`.
pub(crate) fn rebuild_views_at_open(cauldron: &Cauldron) -> Result<Vec<RecoveredTranche>> {
    // Every non-terminal row with its position, derived inside one read
    // (by anchor `seq`, never the clock).
    let rows: Vec<(Row, u32)> = {
        let mut spine = cauldron.spine()?;
        let tx = spine.conn_mut().transaction()?;
        let rows = rows_where(&tx, &format!("state IN {LIVE_STATES} ORDER BY seq"), &[])?;
        let mut positioned = Vec::with_capacity(rows.len());
        for r in rows {
            let p = position_of(&tx, r.seq)?;
            positioned.push((r, p));
        }
        positioned
    };
    let mut report = Vec::with_capacity(rows.len());
    for (row, position) in rows {
        let rebuild = matches!(row.state, TrancheState::Open | TrancheState::Staging);
        let mut staged = row.staged;
        if rebuild {
            let m = tranche_mutex(cauldron, &row.id, row.attempt);
            let mut slot = m.lock().unwrap_or_else(|p| p.into_inner());
            slot.view = None;
            staged = rebuild_in_one_snapshot(cauldron, &mut slot, &row.id, row.attempt)?;
        }
        report.push(RecoveredTranche {
            tranche: row.id.clone(),
            attempt: row.attempt,
            state: row.state,
            lock_mode: row.lock_mode,
            position,
            deadline: row.deadline,
            staged,
            overlay_rebuilt: rebuild,
            draining: row.draining,
            decided: row.decided,
        });
    }
    Ok(report)
}

/// THE QUEUED-ROW HALF OF THE TRANCHE PASS (lost by design: an
/// un-granted claim's queued rows are swept; the child that was waiting was
/// never staged and is re-submitted): every `granted = 0` lock
/// row under a tranche anchor's `seq` is withdrawn, in one IMMEDIATE
/// transaction. Granted rows — what the tranche holds — are never touched.
/// Run first by `recover::tranche_pass`. Returns how many rows it withdrew.
pub(crate) fn withdraw_queued_at_open(cauldron: &Cauldron) -> Result<usize> {
    let mut spine = cauldron.spine()?;
    let tx = spine
        .conn_mut()
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let n = tx.execute(
        "DELETE FROM locks WHERE granted = 0 AND seq IN (SELECT seq FROM tranches)",
        [],
    )?;
    tx.commit()?;
    Ok(n)
}

/// Across a daemon restart, every non-terminal tranche's deadline is
/// reset to `boot + timeout` — the restart is not the holder's fault.
/// `landing` included (it resets every non-terminal deadline),
/// `last_heartbeat` NOT rewritten (the store reads it), and
/// nothing else of the row — `draining` least of all. Run by
/// `recover::tranche_pass` from `Cauldron::open`, after the ordinary pass.
/// Returns how many rows it reset.
pub(crate) fn reset_deadlines_at_open(cauldron: &Cauldron, boot_ms: i64) -> Result<usize> {
    let timeout = ms(cauldron.config().tranche.timeout);
    let mut spine = cauldron.spine()?;
    let tx = spine
        .conn_mut()
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let n = tx.execute(
        &format!("UPDATE tranches SET deadline = ?1 WHERE state IN {LIVE_STATES}"),
        params![boot_ms.saturating_add(timeout)],
    )?;
    tx.commit()?;
    Ok(n)
}

impl Cauldron {
    /// The anchor row of attempt `(T, n)`, read back whole (an
    /// audit asking what did run `T` do reads the anchor). `None` when the
    /// spine holds no such attempt. A read of a fact the spine owns.
    pub fn tranche_anchor(&self, tranche: &str, attempt: u32) -> Result<Option<crate::spine_db::TxRow>> {
        let spine = self.spine()?;
        let seq = spine
            .conn()
            .query_row(
                "SELECT seq FROM tranches WHERE id = ?1 AND attempt = ?2",
                params![tranche, attempt],
                |r| r.get::<_, i64>(0),
            )
            .optional()?;
        match seq {
            None => Ok(None),
            Some(s) => Ok(Some(spine.row_at(Seq(s))?)),
        }
    }
}

/// Milliseconds since the Unix epoch on this process's clock — what
/// `Cauldron::open` uses as `boot`.
pub(crate) fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| ms(d))
        .unwrap_or(0)
}

fn to_json<T: Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v)
        .map_err(|e| SpineError::hard(format!("a tranche anchor element will not serialize: {e}")))
}
