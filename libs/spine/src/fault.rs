//! The fault-injection seam (a cargo feature `fault-injection`), in its TWO
//! forms:
//!
//! - **the KILL form** — `SPINE_FAULT=<stage>`: the process ABORTS at the
//!   named stage;
//! - **the PAUSE form** — `SPINE_PAUSE=<stage>` plus
//!   `SPINE_PAUSE_LATCH=<path>`: the process HOLDS at the named stage until
//!   the latch file appears.
//!
//! Two hard conditions, both delivered structurally, and both binding on
//! BOTH forms:
//! - The seam uses RECOVERY'S OWN stage vocabulary ([`Stage`]) and never
//!   mints its own — two descriptions of the commit path would drift
//!   exactly where atomicity is being proven. The pause form parses its
//!   stage with the same `Stage::parse` the kill form does, and the usage
//!   message that names the stages is DERIVED from [`Stage::ALL`]: there is no
//!   second list of names anywhere in this file, in code or in prose.
//!   THE LANDER is a second pipeline with its own
//!   stages — before the decision, after it, between two children, before
//!   the clearing — and those are borrowed the same way, verbatim, from
//!   [`LanderStage`] (owned by `crate::tranche`, the record the tranche
//!   recovery pass reads). One environment variable arms either
//!   pipeline; [`at_stage`] fires on the door's four names and
//!   [`at_lander_stage`] on the lander's four, and the gate accepts both
//!   vocabularies and nothing else.
//! - Every production build must PROVE the seam absent or refusing: without
//!   the feature the abort AND pause bodies are COMPILED OUT, and a set
//!   `SPINE_FAULT` or a set `SPINE_PAUSE` makes the door refuse loudly by
//!   name — testable in both builds, in process and from outside (the
//!   victim's exit status 3).
//!
//! # Why the pause form exists
//!
//! A kill can only ever leave a window on DISK, after the process is gone.
//! Some properties need the window held open by a LIVE transaction — one
//! that still holds its locks, still has its snapshot, and will still land
//! — while a second party acts. The one that forced this form: the
//! count cell must be a commutative increment and never a plain-write
//! counter computed from the statement's snapshot, and the only
//! interleaving that tells those apart is *A decides, A is held, B lands,
//! A lands*. Before this form existed the tests hand-drove the commit
//! phases in a thread to hold that window, which is exactly the drift
//! hazard the one-vocabulary rule exists to prevent — arriving through the
//! tests.
//! With it, the window IS the real pipeline's window.
//!
//! # The pause contract, exactly
//!
//! With the feature, when `at_stage(stage)` is reached and `SPINE_PAUSE`
//! names `stage`:
//!
//! 1. the process writes `<SPINE_PAUSE_LATCH>.reached` (contents: the stage
//!    name) — so the parent KNOWS the stage was reached, never a sleep race;
//! 2. it then blocks, polling, until `<SPINE_PAUSE_LATCH>` exists;
//! 3. if the latch does not appear within the ceiling
//!    (`SPINE_PAUSE_CEILING_MS`, default [`PAUSE_CEILING_DEFAULT_MS`]) it
//!    ABORTS loudly naming the stage — a pause nobody releases must kill the
//!    test visibly, never hang it;
//! 4. once released, `at_stage` returns `Ok(())` and the pipeline continues
//!    exactly as if nothing had happened.
//!
//! A set `SPINE_PAUSE` naming no stage is a loud usage error, as for
//! `SPINE_FAULT`. A set `SPINE_PAUSE` with no `SPINE_PAUSE_LATCH` is a loud
//! usage error at the door's gate — a pause with nowhere to be released
//! from would only ever end at its ceiling. Both variables set at once:
//! refuse — one seam, one aim.
//!
//! # The ceiling is configuration, not a constant
//!
//! `SPINE_PAUSE_CEILING_MS` overrides [`PAUSE_CEILING_DEFAULT_MS`]. It lives
//! in the environment rather than in `SpineConfig` (an interior
//! choice): `at_stage`'s signature is frozen and carries no config,
//! the seam is armed by environment already, and a test-only bound in the
//! production configuration struct would be the seam leaking into a
//! consumer's surface. The default is conservative — long enough that a
//! parent on a loaded machine has time to act inside the window, short
//! enough that a forgotten release fails a suite in seconds rather than
//! hanging CI.
//!
//! # The marker file and "nothing written beside the database files"
//!
//! The soup idiom, inherited by this crate: the library writes nothing
//! beside the database files. The `.reached` marker does not breach it.
//! It is written ONLY under the `fault-injection` feature — the production
//! library contains no code that writes it (the body is `cfg`-gated out,
//! and the absence tests prove it by execution) — and ONLY to the path the
//! test parent supplied in `SPINE_PAUSE_LATCH`, which the parent chooses and
//! which no test in this crate places inside a cauldron directory. It is
//! the test harness's file, at the test harness's path, under a test-only
//! feature: the same standing as the cauldron directory a kill test
//! creates in `temp_dir()`. Nothing about the cauldron's own files, or the
//! production build's behaviour, is touched by it.

use crate::error::{Result, SpineError};
use crate::recover::Stage;
use crate::tranche::{LanderStage, TrancheStage};

/// The environment variable naming the stage to die at. Values are
/// [`Stage::name`]'s frozen strings.
pub const FAULT_ENV: &str = "SPINE_FAULT";

/// The environment variable naming the stage to PAUSE at. Values
/// are [`Stage::name`]'s frozen strings — the same vocabulary, verbatim.
pub const PAUSE_ENV: &str = "SPINE_PAUSE";

/// The latch file the paused process waits for. The process writes
/// `<path>.reached` when it arrives at the stage, then blocks until `<path>`
/// exists.
pub const PAUSE_LATCH_ENV: &str = "SPINE_PAUSE_LATCH";

/// The ceiling, in milliseconds, after which an unreleased pause ABORTS
/// loudly. Overrides [`PAUSE_CEILING_DEFAULT_MS`].
pub const PAUSE_CEILING_ENV: &str = "SPINE_PAUSE_CEILING_MS";

/// The default pause ceiling (30 seconds): the conservative bound a parent
/// that never releases runs into. Configuration by [`PAUSE_CEILING_ENV`],
/// never a constant the seam consults alone — see the module docs.
pub const PAUSE_CEILING_DEFAULT_MS: u64 = 30_000;

/// The suffix appended to the latch path for the "stage reached" marker.
pub const PAUSE_REACHED_SUFFIX: &str = ".reached";

/// The seam call the door pipeline makes at each named stage (see
/// `crate::exec::execute`'s phase list for exactly where). Both forms live
/// behind this ONE call: the pause is reachable through exactly the stage
/// calls the kill is, and nowhere else.
///
/// - **With `fault-injection`**: if `SPINE_FAULT` names `stage`, the
///   process ABORTS here — the aimed kill an outside process-kill cannot
///   deliver. If `SPINE_PAUSE` names `stage`, the process
///   PAUSES here (module docs). A set value naming no stage is a loud
///   usage error, never a silent no-op. Both variables set: refuse.
/// - **Without the feature**: any set `SPINE_FAULT` refuses loudly
///   ([`SpineError::FaultSeamRefused`] naming `SPINE_FAULT`) and any set
///   `SPINE_PAUSE` refuses loudly (the SAME variant, naming `SPINE_PAUSE`)
///   — the "absent or refusing" proof, for both
///   forms.
/// - Both unset: `Ok(())` in both builds.
pub fn at_stage(stage: Stage) -> Result<()> {
    at_named(Named::Door(stage))
}

/// THE LANDER'S seam call, at each of the four
/// [`LanderStage`]s — the same two forms behind the same two variables as
/// [`at_stage`], fired by the lander's names and never by the door's:
/// `SPINE_FAULT=before-decision` kills a lander at its re-validation and
/// leaves every door commit in the same process untouched (and the reverse
/// for `decided`). Without the feature a set variable refuses loudly by name,
/// exactly as [`at_stage`] does. The crash windows these open are proven by
/// the tranche recovery pass's tests.
pub fn at_lander_stage(stage: LanderStage) -> Result<()> {
    at_named(Named::Lander(stage))
}

/// THE GATE'S two seams — a third
/// vocabulary beside the door's [`Stage`] and the lander's [`LanderStage`],
/// disjoint from both, spoken by the same two variables:
///
/// - `after-phase-1` — a POINT in `exec::execute`, between phase 1 (the halt,
///   quiesce and gate reads) and the append: `SPINE_PAUSE=after-phase-1`
///   holds a writer that passed checkpoint 1 with the gate down until the
///   latch, so it reaches checkpoint 2 after the gate rose; a kill
///   there leaves no row.
/// - `no-poll-refresh` — a BEHAVIOUR, not a point: while `SPINE_FAULT` names
///   it, a tranche claimant's grant poll SKIPS the heartbeat refresh it
///   otherwise gets ("an act in progress is proof of life"), so a
///   deadline can lapse under a child waiting on a lock and the scheduler's
///   abandon can be crossed with the wait. Nothing aborts at it;
///   [`skip_poll_refresh`] is how it is read.
///
/// Without `fault-injection` a set variable naming either refuses loudly by
/// name at the door's gate, as every other stage does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateSeam {
    AfterPhase1,
    NoPollRefresh,
}

impl GateSeam {
    /// THE ONE LIST; `parse` and the no-such-stage message derive from it.
    pub const ALL: [GateSeam; 2] = [GateSeam::AfterPhase1, GateSeam::NoPollRefresh];

    /// The frozen text names: `after-phase-1`, `no-poll-refresh`.
    pub fn name(&self) -> &'static str {
        match self {
            GateSeam::AfterPhase1 => "after-phase-1",
            GateSeam::NoPollRefresh => "no-poll-refresh",
        }
    }

    /// A frozen name back to its seam; `None` for anything else.
    pub fn parse(s: &str) -> Option<GateSeam> {
        GateSeam::ALL.iter().copied().find(|g| g.name() == s)
    }
}

/// THE GATE'S seam call at a POINT (`after-phase-1`): the same two forms
/// behind the same two variables as [`at_stage`], fired by the gate's names
/// and never by the door's or the lander's.
pub fn at_gate_seam(seam: GateSeam) -> Result<()> {
    at_named(Named::Gate(seam))
}

/// THE TRANCHE RECORD'S seam call, at each of the five [`TrancheStage`]s — the FOURTH
/// vocabulary, owned by `crate::tranche` beside [`LanderStage`], disjoint
/// from the door's, the lander's and the gate's, spoken by the same two
/// variables and fired only by its own names: `SPINE_FAULT=after-stage`
/// kills the process the instant a child's staging transaction has
/// committed and leaves every door commit and every lander in the same
/// process untouched. Without the feature a set variable refuses loudly by
/// name, exactly as [`at_stage`] does — and the tranche acts call [`gate`]
/// first, so the production build refuses BEFORE any row is written.
pub fn at_tranche_stage(stage: TrancheStage) -> Result<()> {
    at_named(Named::Tranche(stage))
}

/// THE BEHAVIOUR SEAM `no-poll-refresh`: `Ok(true)` when
/// `SPINE_FAULT` names it under `fault-injection` — the tranche claimant's
/// poll then skips its deadline refresh; `Ok(false)` when the variable is
/// unset or names another stage (which the door's gate has already
/// validated). Without the feature a set `SPINE_FAULT` refuses by name,
/// exactly as [`at_stage`] does — "absent or refusing", never a silent
/// no-op.
pub fn skip_poll_refresh() -> Result<bool> {
    match std::env::var(FAULT_ENV).ok() {
        None => Ok(false),
        Some(value) => skip_with(&value),
    }
}

#[cfg(feature = "fault-injection")]
fn skip_with(value: &str) -> Result<bool> {
    match Named::parse(value) {
        Some(Named::Gate(GateSeam::NoPollRefresh)) => Ok(true),
        Some(_) => Ok(false),
        None => Err(no_such_stage(FAULT_ENV, value)),
    }
}

#[cfg(not(feature = "fault-injection"))]
fn skip_with(value: &str) -> Result<bool> {
    Err(SpineError::FaultSeamRefused {
        variable: FAULT_ENV.to_string(),
        value: value.to_string(),
    })
}

/// A stage of ANY of the three pipelines — what one seam variable can name.
/// In the production build nothing reads it: every body refuses before
/// looking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
enum Named {
    Door(Stage),
    Lander(LanderStage),
    Gate(GateSeam),
    Tranche(TrancheStage),
}

impl Named {
    /// Parse a frozen text name from any of the four vocabularies; `None`
    /// for anything else. The names are disjoint by construction (asserted
    /// by `tests/tranche/landing.rs`, `locks.rs` and `crash.rs`).
    #[cfg(feature = "fault-injection")]
    fn parse(s: &str) -> Option<Named> {
        Stage::parse(s)
            .map(Named::Door)
            .or_else(|| LanderStage::parse(s).map(Named::Lander))
            .or_else(|| GateSeam::parse(s).map(Named::Gate))
            .or_else(|| TrancheStage::parse(s).map(Named::Tranche))
    }

    #[cfg(feature = "fault-injection")]
    fn name(&self) -> &'static str {
        match self {
            Named::Door(s) => s.name(),
            Named::Lander(s) => s.name(),
            Named::Gate(s) => s.name(),
            Named::Tranche(s) => s.name(),
        }
    }

    /// Every name any pipeline knows, for the no-such-stage refusal —
    /// derived from the four `ALL` lists, never retyped.
    #[cfg(feature = "fault-injection")]
    fn all_names() -> Vec<&'static str> {
        Stage::ALL
            .iter()
            .map(|s| s.name())
            .chain(LanderStage::ALL.iter().map(|s| s.name()))
            .chain(GateSeam::ALL.iter().map(|s| s.name()))
            .chain(TrancheStage::ALL.iter().map(|s| s.name()))
            .collect()
    }
}

fn at_named(stage: Named) -> Result<()> {
    match (std::env::var(FAULT_ENV).ok(), std::env::var(PAUSE_ENV).ok()) {
        (None, None) => Ok(()),
        (Some(fault), Some(pause)) => both_set(&fault, &pause),
        (Some(fault), None) => at_stage_with(stage, &fault),
        (None, Some(pause)) => pause_at_stage_with(stage, &pause),
    }
}

/// THE DOOR'S FAULT-SEAM GATE (`exec::execute` phase 2).
///
/// [`at_stage`] can only speak when the pipeline actually reaches a stage.
/// This gate speaks at the door, before anything is parsed or appended, so
/// that "every production build must PROVE the seam absent — compiled out or
/// refusing" is a property of the DOOR and not
/// merely of a stage the statement might never reach.
///
/// - **Without `fault-injection`**: any set `SPINE_FAULT` refuses the
///   statement loudly ([`SpineError::FaultSeamRefused`], `variable` =
///   `SPINE_FAULT`) before any effect; any set `SPINE_PAUSE` refuses it
///   loudly (the same variant, `variable` = `SPINE_PAUSE`).
/// - **With the feature**: a set value naming a real stage is armed and this
///   gate passes; a value naming NO stage is a loud usage error here rather
///   than a silent no-op that would make the whole kill (or pause) test a
///   fake green. For the pause form the gate also demands the latch path
///   and a parseable ceiling, so a mis-set pause fails at the door and not
///   thirty seconds later at its ceiling. Both variables set: refuse.
/// - Both unset: `Ok(())` in both builds.
pub fn gate() -> Result<()> {
    match (std::env::var(FAULT_ENV).ok(), std::env::var(PAUSE_ENV).ok()) {
        (None, None) => Ok(()),
        (Some(fault), Some(pause)) => both_set(&fault, &pause),
        (Some(fault), None) => gate_with(&fault),
        (None, Some(pause)) => pause_gate_with(&pause),
    }
}

// ---------------------------------------------------------------------------
// the seam's own USAGE refusals — frozen tags
// ---------------------------------------------------------------------------
//
// An armed-build mis-arming is a `SpineError::Usage`, and there are FOUR
// distinct causes. A test that asserted `is_err()` passed on any of them —
// "no latch path: the gate refuses" was satisfied by a misspelled stage,
// and "both set" by a latch refusal. So each cause carries a FROZEN TAG at
// the head of its message (`<tag>: <detail>`), the same shape
// `schema_def::GRAMMAR_FAULTS` gives grammar refusals, and a test reads the
// tag back with [`seam_usage_tag`] and asserts THAT — structure, never a
// substring of prose.

/// A set seam variable naming no commit stage.
pub const USAGE_NO_SUCH_STAGE: &str = "seam_no_such_stage";
/// `SPINE_PAUSE` set with no `SPINE_PAUSE_LATCH` (refused at the gate).
pub const USAGE_NO_LATCH: &str = "seam_no_latch";
/// `SPINE_PAUSE_CEILING_MS` set to something that is not a number of
/// milliseconds (refused at the gate).
pub const USAGE_BAD_CEILING: &str = "seam_bad_ceiling";
/// Both seam variables set at once: one seam, one aim.
pub const USAGE_BOTH_SET: &str = "seam_both_set";
/// The frozen list. A tag outside it cannot be minted (`seam_usage`
/// asserts membership), and [`seam_usage_tag`] answers only from it.
pub const SEAM_USAGE_TAGS: &[&str] = &[
    USAGE_NO_SUCH_STAGE,
    USAGE_NO_LATCH,
    USAGE_BAD_CEILING,
    USAGE_BOTH_SET,
];

/// Read the frozen tag back off a seam usage refusal: `Some(tag)` for a
/// [`SpineError::Usage`] whose message head is one of [`SEAM_USAGE_TAGS`];
/// `None` for any other error, any other `Usage`, or an `Ok`. Available in
/// both builds so a test asserting a tag compiles in both; in the
/// production build no seam usage refusal is ever minted, so it answers
/// `None` for everything.
pub fn seam_usage_tag<T>(r: &Result<T>) -> Option<&'static str> {
    let Err(SpineError::Usage { message }) = r else {
        return None;
    };
    let head = message.split(':').next()?;
    SEAM_USAGE_TAGS.iter().copied().find(|t| *t == head)
}

/// Mint a tagged seam usage refusal. Armed build only: the production build
/// has no seam to mis-arm and refuses by [`SpineError::FaultSeamRefused`]
/// before any usage question arises.
#[cfg(feature = "fault-injection")]
fn seam_usage(tag: &'static str, detail: String) -> SpineError {
    debug_assert!(
        SEAM_USAGE_TAGS.contains(&tag),
        "seam usage tags are frozen in SEAM_USAGE_TAGS"
    );
    SpineError::usage(format!("{tag}: {detail}"))
}

// ---------------------------------------------------------------------------
// the kill form
// ---------------------------------------------------------------------------

#[cfg(feature = "fault-injection")]
fn at_stage_with(stage: Named, value: &str) -> Result<()> {
    match Named::parse(value) {
        // Through `say_then_abort`, never `eprintln!` + abort:
        // under a captured test run the `eprintln!` line dies in libtest's
        // buffer with the process. Proved by
        // `tests/fault_spine_fault_abort_says_why.rs`.
        Some(named) if named == stage => {
            say_then_abort(&format!("SPINE_FAULT: aborting at stage `{}`", stage.name()))
        }
        Some(_) => Ok(()),
        None => Err(no_such_stage(FAULT_ENV, value)),
    }
}

#[cfg(not(feature = "fault-injection"))]
fn at_stage_with(_stage: Named, value: &str) -> Result<()> {
    Err(SpineError::FaultSeamRefused {
        variable: FAULT_ENV.to_string(),
        value: value.to_string(),
    })
}

#[cfg(feature = "fault-injection")]
fn gate_with(value: &str) -> Result<()> {
    match Named::parse(value) {
        Some(_) => Ok(()),
        None => Err(no_such_stage(FAULT_ENV, value)),
    }
}

#[cfg(not(feature = "fault-injection"))]
fn gate_with(value: &str) -> Result<()> {
    Err(SpineError::FaultSeamRefused {
        variable: FAULT_ENV.to_string(),
        value: value.to_string(),
    })
}

// ---------------------------------------------------------------------------
// the pause form
// ---------------------------------------------------------------------------

#[cfg(feature = "fault-injection")]
fn pause_at_stage_with(stage: Named, value: &str) -> Result<()> {
    match Named::parse(value) {
        Some(named) if named == stage => pause_here(stage),
        Some(_) => Ok(()),
        None => Err(no_such_stage(PAUSE_ENV, value)),
    }
}

#[cfg(not(feature = "fault-injection"))]
fn pause_at_stage_with(_stage: Named, value: &str) -> Result<()> {
    Err(SpineError::FaultSeamRefused {
        variable: PAUSE_ENV.to_string(),
        value: value.to_string(),
    })
}

#[cfg(feature = "fault-injection")]
fn pause_gate_with(value: &str) -> Result<()> {
    if Named::parse(value).is_none() {
        return Err(no_such_stage(PAUSE_ENV, value));
    }
    // Everything the pause will need, demanded at the door: a latch path,
    // and a ceiling that parses. A pause that fails HERE fails before any
    // row is appended; one that failed at the stage would leave a live
    // transaction behind it.
    pause_latch()?;
    pause_ceiling()?;
    Ok(())
}

#[cfg(not(feature = "fault-injection"))]
fn pause_gate_with(value: &str) -> Result<()> {
    Err(SpineError::FaultSeamRefused {
        variable: PAUSE_ENV.to_string(),
        value: value.to_string(),
    })
}

/// THE PAUSE. Write the marker, then hold until released or until the
/// ceiling — and at the ceiling, die loudly rather than hang.
#[cfg(feature = "fault-injection")]
fn pause_here(stage: Named) -> Result<()> {
    use std::time::{Duration, Instant};

    let latch = pause_latch()?;
    let ceiling = pause_ceiling()?;
    let reached = reached_path(&latch);
    // THE MARKER FIRST: the parent must never have to guess whether the
    // stage was reached. If it cannot be written the parent can never know,
    // so this is a visible death and not an error return that the door would
    // fold into an ordinary failure and the victim into an exit status the
    // parent could mistake for a refusal.
    //
    // PUBLISHED ATOMICALLY: written to a sibling temp path and RENAMED into
    // place, so the marker can never exist without its content. A plain
    // `fs::write` creates the file and then fills it, and a parent polling
    // for existence can read it in between — observed once as
    // `left: "", right: "decided"` in a test's marker
    // check. Rename is the one filesystem step that is atomic here.
    let staging = {
        let mut s = reached.as_os_str().to_os_string();
        s.push(".writing");
        std::path::PathBuf::from(s)
    };
    let published = std::fs::write(&staging, stage.name())
        .and_then(|()| std::fs::rename(&staging, &reached));
    if let Err(e) = published {
        say_then_abort(&format!(
            "SPINE_PAUSE: cannot write the reached-marker `{}` at stage `{}`: {e}; aborting",
            reached.display(),
            stage.name()
        ));
    }
    eprintln!(
        "SPINE_PAUSE: paused at stage `{}`; waiting for `{}` (ceiling {ceiling:?})",
        stage.name(),
        latch.display()
    );
    let started = Instant::now();
    let mut interval = Duration::from_millis(1);
    let max_interval = Duration::from_millis(16);
    while !latch.exists() {
        if started.elapsed() >= ceiling {
            say_then_abort(&format!(
                "SPINE_PAUSE: pause at stage `{}` was never released within {ceiling:?} \
                 (no `{}`); aborting rather than hanging the test",
                stage.name(),
                latch.display()
            ));
        }
        std::thread::sleep(interval);
        interval = (interval * 2).min(max_interval);
    }
    eprintln!("SPINE_PAUSE: released at stage `{}`", stage.name());
    Ok(())
}

/// THE SEAM'S LAST WORDS. Every
/// abort in this file goes through here. Write `reason` as one line straight to
/// the process's stderr, then abort.
///
/// NOT `eprintln!`: under a captured `cargo test` run libtest redirects
/// `eprint!`/`eprintln!` (and `print!`) into a per-test buffer that it only
/// replays when the test RETURNS — and an abort never returns, so the
/// buffer dies with the process and the run shows a bare SIGABRT with no
/// reason. A write through the `std::io::stderr()` handle bypasses that
/// capture and reaches fd 2. Proved by
/// `tests/fault_pause_abort_says_why.rs`, which runs each abort path in a
/// child test process under capture and reads the child's stderr.
#[cfg(feature = "fault-injection")]
fn say_then_abort(reason: &str) -> ! {
    use std::io::Write;
    let mut err = std::io::stderr().lock();
    // Best effort: if fd 2 itself is gone there is nowhere left to say it,
    // and the abort must happen regardless.
    let _ = writeln!(err, "{reason}");
    let _ = err.flush();
    std::process::abort();
}

/// The path the parent named, or a loud usage error.
#[cfg(feature = "fault-injection")]
fn pause_latch() -> Result<std::path::PathBuf> {
    match std::env::var(PAUSE_LATCH_ENV) {
        Ok(p) if !p.is_empty() => Ok(std::path::PathBuf::from(p)),
        _ => Err(seam_usage(
            USAGE_NO_LATCH,
            format!(
                "{PAUSE_ENV} is set but {PAUSE_LATCH_ENV} names no latch file; a pause with \
                 nowhere to be released from would only ever end at its ceiling"
            ),
        )),
    }
}

/// The ceiling: `SPINE_PAUSE_CEILING_MS` if set (and it must parse), else
/// [`PAUSE_CEILING_DEFAULT_MS`].
#[cfg(feature = "fault-injection")]
fn pause_ceiling() -> Result<std::time::Duration> {
    match std::env::var(PAUSE_CEILING_ENV) {
        Err(_) => Ok(std::time::Duration::from_millis(PAUSE_CEILING_DEFAULT_MS)),
        Ok(raw) => raw
            .trim()
            .parse::<u64>()
            .map(std::time::Duration::from_millis)
            .map_err(|_| {
                seam_usage(
                    USAGE_BAD_CEILING,
                    format!("{PAUSE_CEILING_ENV}={raw} is not a number of milliseconds"),
                )
            }),
    }
}

/// `<latch>.reached` — the marker path for a latch path. Public so the
/// parent side (`tests/util/kill.rs`) derives it from the same rule instead
/// of restating it.
pub fn reached_path(latch: &std::path::Path) -> std::path::PathBuf {
    let mut s = latch.as_os_str().to_os_string();
    s.push(PAUSE_REACHED_SUFFIX);
    std::path::PathBuf::from(s)
}

// ---------------------------------------------------------------------------
// shared
// ---------------------------------------------------------------------------

/// Both variables set at once: ONE seam, ONE aim. Refused in both builds —
/// in the armed build as a usage error naming both, in the production build
/// as the kill form's refusal (the first variable checked; the pause would
/// be refused too if it were alone).
#[cfg(feature = "fault-injection")]
fn both_set(fault: &str, pause: &str) -> Result<()> {
    Err(seam_usage(
        USAGE_BOTH_SET,
        format!(
            "{FAULT_ENV}={fault} and {PAUSE_ENV}={pause} are both set; one seam, one aim — arm \
             the kill form or the pause form, never both"
        ),
    ))
}

#[cfg(not(feature = "fault-injection"))]
fn both_set(fault: &str, _pause: &str) -> Result<()> {
    Err(SpineError::FaultSeamRefused {
        variable: FAULT_ENV.to_string(),
        value: fault.to_string(),
    })
}

/// A set variable naming no stage: loud, never a silent no-op. The stages it
/// names are read off [`Stage::ALL`] — recovery's one list — and
/// [`LanderStage::ALL`] — the lander's — so this message can never drift
/// from the vocabularies it describes.
#[cfg(feature = "fault-injection")]
fn no_such_stage(variable: &str, value: &str) -> SpineError {
    let stages = Named::all_names().join(", ");
    seam_usage(
        USAGE_NO_SUCH_STAGE,
        format!("{variable}={value} names no commit stage; the stages are {stages}"),
    )
}

// ---------------------------------------------------------------------------
// the old-pool upgrade window — an in-process hook, not a stage
// ---------------------------------------------------------------------------

/// THE UPGRADE WINDOW (test seam for `pool::upgrade_sealed_column`).
///
/// Two opens of one pool file written before the seal act existed can both
/// find the
/// `nodes.sealed` column absent and both try to add it; the loser's `ALTER`
/// fails ("duplicate column") and the recheck after it absorbs that. Without
/// a seam that race fires only by luck, so the recheck was unproven at any N
/// (`pool.rs` upgrade). This hook lets a test HOLD
/// each opener between its column check and its `ALTER`, so both are in the
/// window together, deterministically, and lets it COUNT the loser.
///
/// It is NOT a commit stage: the upgrade runs inside `Pool::open`, outside
/// the door's pipeline, so recovery's [`Stage`] vocabulary has no name for
/// it and this seam does not mint one. It is an in-process hook rather than
/// an environment variable because the race is between THREADS of one test
/// process, each of which must reach the window — an environment pause
/// would hold whichever arrived first and could not rendezvous two.
///
/// Armed build only. Without `fault-injection` this module does not exist
/// and the call sites in `pool.rs` are compiled out: the production build
/// has no hook to install.
#[cfg(feature = "fault-injection")]
pub mod upgrade_window {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    /// Where in `upgrade_sealed_column` the hook is called.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Event {
        /// The column check found the column ABSENT; the `ALTER` is next.
        BeforeAlter,
        /// The `ALTER` failed and the recheck found the column present:
        /// another opener added it first, and this open absorbs that.
        AlterLostToARacer,
    }

    type Hook = Arc<dyn Fn(&Path, Event) + Send + Sync>;
    static HOOK: Mutex<Option<Hook>> = Mutex::new(None);

    /// Install the hook (replacing any). Every `Pool::open` in the process
    /// calls it with the pool file's path, so a hook filters by path.
    pub fn install(hook: impl Fn(&Path, Event) + Send + Sync + 'static) {
        *HOOK.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(hook));
    }

    /// Remove the hook.
    pub fn clear() {
        *HOOK.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Called by `pool::upgrade_sealed_column`. The lock is NOT held while
    /// the hook runs, so a hook that blocks (a rendezvous) blocks only its
    /// own opener.
    pub(crate) fn reached(path: &Path, event: Event) {
        let hook = HOOK.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(h) = hook {
            h(path, event);
        }
    }
}
