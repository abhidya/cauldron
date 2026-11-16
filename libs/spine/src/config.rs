//! `SpineConfig` — every bound the design says must be configuration, never a
//! constant.
//!
//! The design fixes the SHAPE of each bound and sets its NUMBER nowhere; the
//! defaults below are conservative choices, each with its rationale.

use std::time::Duration;

/// The spine's configuration. One struct, passed at [`crate::Cauldron`]
/// create/open; conservative defaults via [`Default`].
///
/// Retry bound, restart
/// bound, escalation threshold, fuel ceiling, lock poll interval, lock wait
/// ceiling all live here.
#[derive(Debug, Clone)]
pub struct SpineConfig {
    /// How many times a lock-free validated READ
    /// retries after invalidation before it escalates to real read locks
    /// with a spine sequence number (bounded escalation).
    pub retry_bound: u32,

    /// How many times a WRITE statement restarts (fresh sequence number)
    /// after its premises moved between snapshot and grant,
    /// before it refuses whole with the contested claims as witness
    /// (the stale-match witness).
    pub restart_bound: u32,

    /// After this many restarts (and before `restart_bound` is reached), the
    /// write path's re-match runs under REAL read locks on the match set
    /// rather than as another optimistic snapshot (escalate to
    /// real read locks on the match set after a bounded number of
    /// restarts). The
    /// design names both an escalation threshold and a restart bound; this
    /// build reads them as escalate-at, refuse-at, with
    /// `escalation_threshold <= restart_bound`.
    pub escalation_threshold: u32,

    /// The match phase's fuel ceiling. Exhaustion
    /// fails loudly as [`crate::error::SpineError::FuelExhausted`], never a
    /// silent partial result. Units: one fuel per row visited or predicate
    /// evaluated by the matcher (frozen in [`crate::ladle::compile::Fuel`]).
    pub fuel_ceiling: u64,

    /// How long a queued claim sleeps between polls of the lock tables
    /// (waiting is bounded polling with backoff; this
    /// is the initial interval, doubled per poll up to 32x).
    pub lock_poll_interval: Duration,

    /// The ceiling on total wait for any one claim grant. Exceeding it is a
    /// refusal with the contested claims (never an untracked hang). It is
    /// ALSO the ceiling on how long the door's own mechanical spine and pool
    /// writes wait their turn behind a foreign holder of the file after the
    /// decided flip (`exec::wait_turn`).
    pub lock_wait_ceiling: Duration,

    /// How long ONE SQLite statement on a spine or pool connection waits for
    /// the file's write lock before answering `Busy` (SQLite's own busy
    /// handler). It used to be a 5-second constant no configuration could
    /// see — SHORTER than the default lock ceiling, so the system's shortest
    /// timeout was the one nobody configured, and it fired on the one file
    /// the design names as the serialization point. It is a per-statement wait; the door's
    /// mechanical writes after the decided flip retry past it up to
    /// `lock_wait_ceiling` in total, so a transient hold shorter than the
    /// ceiling never strands a decided commit.
    pub busy_timeout: Duration,

    /// The tranche's own numbers. The soup reads its
    /// `SOUP_TRANCHE_*` settings into this at boot; the spine never
    /// reads the environment.
    pub tranche: TrancheConfig,
}

/// The tranche settings the spine applies, each a
/// configuration value and never a constant.
#[derive(Debug, Clone)]
pub struct TrancheConfig {
    /// `SOUP_TRANCHE_HEARTBEAT_MS` — the beat cadence advertised in
    /// `begin`'s reply. The spine enforces nothing on it beyond
    /// `timeout >= 3 x interval`.
    pub heartbeat_interval: Duration,
    /// `SOUP_TRANCHE_TIMEOUT_MS` — the lapse after the last beat at which
    /// the tick abandons a `queued`, `open` or `staging` tranche.
    pub timeout: Duration,
    /// `SOUP_TRANCHE_QUIET_WAIT_MS` — the quiet start's BOUND, measured from
    /// `head_since`: past it the head opens quiet or not.
    pub quiet_wait: Duration,
    /// `SOUP_TRANCHE_MAX_CHILDREN` — the child count this build is sized for;
    /// the `max`-th child is refused `tranche_full`.
    pub max_children: u32,
    /// How often the tick's tripwire logs one `queued` tranche waiting behind
    /// another of the SAME `holder_service` (a nested
    /// type-three run that began its own tranche would wait forever behind
    /// its parent; the soup only makes that visible). Once per interval per
    /// pair; one minute by default. Not one of the four tranche settings.
    pub same_service_warn_interval: Duration,
}

impl Default for TrancheConfig {
    /// The default numbers, with their reasons: a lost holder is noticed
    /// within a human's patience (5 s beat), six beats of slack (30 s), three
    /// beats for a trigger chain of ordinary length to drain (15 s), and the
    /// largest decision transaction over child rows this build is sized for
    /// (4096).
    fn default() -> Self {
        TrancheConfig {
            heartbeat_interval: Duration::from_millis(5000),
            timeout: Duration::from_millis(30000),
            quiet_wait: Duration::from_millis(15000),
            max_children: 4096,
            same_service_warn_interval: Duration::from_secs(60),
        }
    }
}

impl Default for SpineConfig {
    /// The FINALIZED defaults of this build, each with one line of
    /// rationale. Every number is a
    /// conservative choice on an unmeasured system; none is a constant
    /// anywhere, and every one of them is overridable per cauldron.
    fn default() -> Self {
        SpineConfig {
            // 8: a lock-free read is invalidated only by a commit landing on
            // its own pools inside its own window, so eight consecutive
            // invalidations is already a storm rather than bad luck — high
            // enough that the cold escalation path stays cold, low enough
            // that a genuinely hot pool escalates in milliseconds, not
            // seconds.
            retry_bound: 8,

            // 8, deliberately the SAME number as `retry_bound`: both bound
            // "how many times may an optimistic attempt be discarded before
            // this build stops being optimistic", and two different numbers
            // for one idea would be two things to tune and two things to get
            // wrong.
            restart_bound: 8,

            // 4 = half the restart bound: escalate at the midpoint, so a
            // statement gets a fair run of cheap optimistic restarts and
            // still has four attempts left under real read locks before it
            // refuses. `escalation_threshold <= restart_bound` is checked
            // ([`SpineConfig::check`]) rather than assumed.
            escalation_threshold: 4,

            // 1,000,000 steps, one unit per row visited, predicate evaluated
            // and delta emitted: a legitimate bulk statement over a few tens
            // of thousands of rows fits with room to spare, while a runaway
            // cartesian product stops in well under a second of work. No
            // measurement justifies a tighter number yet, and the design sets
            // it nowhere.
            fuel_ceiling: 1_000_000,

            // 2ms, doubling to 32x (64ms): short enough that an uncontended
            // hand-off is imperceptible, long enough that a long wait is a
            // sleep rather than a spin on the one file the design already
            // names as the first bottleneck.
            lock_poll_interval: Duration::from_millis(2),

            // 10s: longer than any honest contention on a loaded machine
            // (a commit path is scripted and fast), short
            // enough that a genuine cycle or a crashed lock holder is
            // REPORTED with its blockers rather than hung on forever.
            lock_wait_ceiling: Duration::from_secs(10),

            // 5s: the number soup's connections have always run under and
            // this crate inherited as a constant (now
            // configuration). Per statement; the door's post-decided writes
            // wait their turn past it up to the ceiling above.
            busy_timeout: Duration::from_secs(5),

            // The four tranche settings, each with its reason there.
            tranche: TrancheConfig::default(),
        }
    }
}

impl SpineConfig {
    /// The one invariant between two of these numbers, checked loudly at
    /// [`crate::Cauldron`] create and open rather than assumed: the write
    /// path escalates to real read locks at `escalation_threshold` restarts
    /// and refuses at `restart_bound`, so a threshold ABOVE the bound would
    /// silently mean "never escalate" — a cold path that can never run is
    /// worse than one that runs rarely (a cold path that has
    /// never run warm is not proven).
    pub fn check(&self) -> crate::error::Result<()> {
        if self.escalation_threshold > self.restart_bound {
            return Err(crate::error::SpineError::usage(format!(
                "SpineConfig: escalation_threshold ({}) is above restart_bound ({}); the write \
                 path would refuse before it ever escalated to real read locks",
                self.escalation_threshold, self.restart_bound
            )));
        }
        if self.fuel_ceiling == 0 {
            return Err(crate::error::SpineError::usage(
                "SpineConfig: fuel_ceiling 0 refuses every statement before it evaluates one row",
            ));
        }
        if self.lock_poll_interval.is_zero() {
            return Err(crate::error::SpineError::usage(
                "SpineConfig: lock_poll_interval 0 turns bounded polling into a spin",
            ));
        }
        if self.busy_timeout.is_zero() {
            return Err(crate::error::SpineError::usage(
                "SpineConfig: busy_timeout 0 makes every contended statement on a spine or pool \
                 file answer Busy at once; a mechanical write waits its turn",
            ));
        }
        // One dropped beat must never abandon a tranche.
        let t = &self.tranche;
        if t.heartbeat_interval.is_zero() || t.timeout < t.heartbeat_interval * 3 {
            return Err(crate::error::SpineError::usage(format!(
                "SpineConfig: tranche timeout ({} ms) must be at least 3 x a non-zero heartbeat                  interval ({} ms), so one dropped beat never abandons a tranche",
                t.timeout.as_millis(),
                t.heartbeat_interval.as_millis()
            )));
        }
        if t.max_children == 0 {
            return Err(crate::error::SpineError::usage(
                "SpineConfig: tranche max_children 0 refuses every child",
            ));
        }
        Ok(())
    }
}
