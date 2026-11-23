//! Kill-injection plumbing for the crash suites — the parent half of
//! `examples/spine_victim.rs`.
//!
//! Included with `#[path = "util/kill.rs"] mod kill;` rather than through a
//! `tests/util/mod.rs`: a file under `tests/` that is not a top-level `.rs`
//! is not a cargo test target either way.
//!
//! # What "a kill" means here, precisely
//!
//! A REAL process death. [`kill_at`] spawns the victim as a CHILD PROCESS
//! with `SPINE_FAULT=<stage>` in its environment, and asserts the child died
//! by SIGABRT with no exit code at all — `std::process::abort()`, not an
//! error return, not a panic, not an unwind. The half-landed window on disk
//! is the thing under test, and only a process that never got to run its
//! cleanup can leave one (the atomicity family).
//!
//! Every assertion helper below is deliberately shaped to answer one
//! question: everything landed or nothing, and does the ROW tell which.
//!
//! # And what "a pause" means here
//!
//! A REAL, LIVE transaction, held at a named stage by the seam's PAUSE form
//! in a child process. [`pause_at`] spawns the victim with
//! `SPINE_PAUSE=<stage>` and a latch path, and returns only once the
//! library's own `<latch>.reached` marker exists — the stage was REACHED,
//! never assumed from a sleep. [`Paused::release`] creates the latch and
//! waits for the child to finish. Between the two, the child holds every
//! lock it acquired, its row is in whatever state the stage leaves it, and
//! it will still land: the window is the real pipeline's window.

#![allow(dead_code)]

use spine::config::SpineConfig;
use spine::envelope::TxHash;
use spine::schema_def::PoolName;
use spine::spine_db::{Seq, SpineDb, TxRow, SPINE_FILE};
use spine::{Cauldron, ReadRequest};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

pub const ALPHA: &str = "fixture-alpha";
pub const BETA: &str = "fixture-beta";

/// The title of the doc `spine_victim` seeds and the victim's statement matches.
pub const SEED_TITLE: &str = "catalogued";

// The four named commit stages are `spine::recover::Stage::ALL` and nothing
// else. A hand-typed `STAGES: [&str; 4]` used to live here — a second list of
// the four names, read by nobody; it is gone
// so that the seam's vocabulary has exactly one spelling, recovery's.

/// A seam usage refusal, asserted BY ITS FROZEN TAG: the
/// result must be an `Err` whose `fault::seam_usage_tag` is exactly `tag`.
/// An `Ok`, any other error, and any OTHER seam usage refusal all fail here
/// — which is what a bare `is_err()` could not do.
pub fn assert_seam_usage<T: std::fmt::Debug>(r: spine::error::Result<T>, tag: &str, why: &str) {
    assert_eq!(
        spine::fault::seam_usage_tag(&r),
        Some(tag),
        "{why}: expected the seam usage refusal `{tag}`, got {r:?}"
    );
}

/// SIGABRT. What `std::process::abort()` raises, and the only death this
/// module accepts as a kill.
pub const SIGABRT: i32 = 6;

/// The victim's own exit statuses (see `examples/spine_victim.rs`).
pub const EXIT_REFUSED: i32 = 3;
pub const EXIT_UNREACHED: i32 = 4;
pub const EXIT_WRONG_BUILD: i32 = 7;

// ---------------------------------------------------------------------------
// process-global environment, serialized
// ---------------------------------------------------------------------------

/// `SPINE_FAULT` is process-global and the harness runs tests in threads, so
/// any test that touches the PARENT's environment takes [`env_exclusive`] and
/// every other test takes [`env_shared`]. Child environments are always set
/// per-`Command` and never through the parent, so kill tests only need the
/// shared guard.
static ENV: RwLock<()> = RwLock::new(());

pub fn env_shared() -> RwLockReadGuard<'static, ()> {
    ENV.read().unwrap_or_else(|e| e.into_inner())
}

pub fn env_exclusive() -> RwLockWriteGuard<'static, ()> {
    ENV.write().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// spawning the victim
// ---------------------------------------------------------------------------

/// A scratch cauldron directory, removed if a previous run left one. Keyed by
/// pid so two concurrent cargo invocations never share a path.
pub fn scratch(name: &str) -> PathBuf {
    let dir = test_scratch::tmp_path(format!("spine-kill-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

// Where cargo put the victim: `util/victim.rs`, the one spelling of that
// lookup (cargo's own `CARGO_BIN_EXE_spine_victim`, never a path guess).
#[path = "victim.rs"]
mod victim_path;
pub use victim_path::victim_binary;

/// The victim, configured but not yet run. `SPINE_FAULT` is always set or
/// explicitly removed, never inherited, so a test that arms the PARENT's
/// environment can never accidentally arm a child. The pause form's three
/// variables are removed here for the same reason; [`pause_at`] sets them.
pub fn victim(job: &str, dir: &Path, stage: Option<&str>) -> Command {
    let mut cmd = Command::new(victim_binary());
    cmd.env("SPINE_VICTIM_DIR", dir.as_os_str())
        .env("SPINE_VICTIM_JOB", job)
        .env_remove(spine::fault::PAUSE_ENV)
        .env_remove(spine::fault::PAUSE_LATCH_ENV)
        .env_remove(spine::fault::PAUSE_CEILING_ENV)
        // Cargo uplifts every feature configuration of an example to ONE
        // path, so the victim on disk may have been built by another run.
        // The victim checks this against its own `cfg!` and refuses a
        // mismatch by name (see `examples/spine_victim.rs::check_build`).
        .env(
            "SPINE_VICTIM_EXPECT_FEATURE",
            if cfg!(feature = "fault-injection") {
                "1"
            } else {
                "0"
            },
        );
    match stage {
        Some(s) => cmd.env("SPINE_FAULT", s),
        None => cmd.env_remove("SPINE_FAULT"),
    };
    cmd
}

/// Turn a stale-victim exit into the diagnostic it deserves, before any other
/// assertion gets a chance to misread it.
///
/// TWO staleness shapes, both real and both met in this task's own run.
/// `EXIT_WRONG_BUILD` is the victim built under the other feature set.
/// `EXIT_USAGE` reached here means the victim does not know a job this suite
/// asks for — which can only mean the binary predates the test. Both have the
/// same remedy and neither may be allowed to look like a result.
pub fn reject_a_stale_victim(out: &Output) {
    match out.status.code() {
        Some(EXIT_WRONG_BUILD) => panic!("{}", stderr_of(out)),
        Some(2) => panic!(
            "the `spine_victim` example on disk is OLDER than this suite: {}\nRe-run without a \
             `--test` filter (`cargo test -p spine [--features fault-injection]`) so cargo \
             rebuilds the example.",
            stderr_of(out).trim()
        ),
        _ => {}
    }
}

/// RUN THE VICTIM AND ASSERT IT DIED — a real process death at `stage`.
///
/// Returns nothing on purpose: everything the parent may conclude about the
/// crash it must read off the DISK afterwards, because that is the whole
/// claim of the design (recovery consults the row, never the process).
#[cfg(unix)]
pub fn kill_at(job: &str, dir: &Path, stage: &str) {
    kill_at_with(job, dir, stage, &[]);
}

/// [`kill_at`] with further environment for the job (the tranche job's
/// `SPINE_VICTIM_TRANCHE`, `SPINE_VICTIM_MODE`, …), handing back
/// everything the victim SAID before it died — its `victim: key=value`
/// lines, flushed line by line, which the parent reads with [`said`]. The
/// same three assertions: no exit status, SIGABRT, the stage named.
#[cfg(unix)]
pub fn kill_at_with(job: &str, dir: &Path, stage: &str, extra: &[(&str, &str)]) -> Output {
    use std::os::unix::process::ExitStatusExt;
    let mut cmd = victim(job, dir, Some(stage));
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn the victim");
    reject_a_stale_victim(&out);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(
        out.status.code(),
        None,
        "a kill is a DEATH, not an exit status: the victim returned normally.\nstderr: {stderr}"
    );
    assert_eq!(
        out.status.signal(),
        Some(SIGABRT),
        "the seam must abort the process AT `{stage}` (SIGABRT), not unwind.\nstderr: {stderr}"
    );
    assert!(
        stderr.contains(&format!("aborting at stage `{stage}`")),
        "the seam names the stage it died at; stderr was:\n{stderr}"
    );
    out
}

/// What the victim SAID before it died: the value of `victim: <key>=…` on
/// its stdout (the tranche job's lines). `None` when it never said
/// it — a parent asserting on a value asserts it was said.
pub fn said(out: &Output, key: &str) -> Option<String> {
    stdout_of(out)
        .lines()
        .filter_map(|l| l.strip_prefix("victim: "))
        .find_map(|l| {
            l.strip_prefix(&format!("{key}="))
                .or_else(|| l.strip_prefix(&format!("{key} ")))
                .map(str::to_string)
        })
}

/// Run the victim to completion and hand back everything it said. Used for
/// the jobs that must NOT die: `resubmit`, the aimed-seam check, and the
/// production build's out-of-process seam-absence proof.
pub fn run(job: &str, dir: &Path, stage: Option<&str>) -> Output {
    let out = victim(job, dir, stage).output().expect("spawn the victim");
    reject_a_stale_victim(&out);
    out
}

// ---------------------------------------------------------------------------
// the pause form — a live transaction held at a stage in a child process
// ---------------------------------------------------------------------------

/// How long the parent waits for the child to REACH the stage before giving
/// up loudly. Generous: the child has to build a cauldron handle and run its
/// setup first, on a machine that may be running the whole suite.
pub const REACH_CEILING: Duration = Duration::from_secs(30);

/// A victim HELD at a named stage. Obtained from [`pause_at`]; ends with
/// [`Paused::release`]. Dropping it un-released still writes the latch and
/// reaps the child, so a test that panics mid-window never leaves a process
/// behind waiting on its ceiling.
pub struct Paused {
    child: Option<Child>,
    pub stage: String,
    pub latch: PathBuf,
    pub reached: PathBuf,
    /// The scratch directory holding the latch and its marker — never the
    /// cauldron directory (see `src/fault.rs`'s module docs).
    pub latch_dir: PathBuf,
}

/// SPAWN THE VICTIM AND HOLD IT AT `stage`. Returns once the library's own
/// `.reached` marker exists — the stage was reached, in the real pipeline,
/// by `Cauldron::submit` — and never on a timer.
///
/// `extra` is any further environment the job wants (`SPINE_VICTIM_AUTHOR`,
/// `SPINE_VICTIM_DOC_TITLE`, `SPINE_PAUSE_CEILING_MS`, …). `SPINE_FAULT` is
/// removed: one seam, one aim.
///
/// The latch lives in its own scratch directory under `temp_dir()`, keyed by
/// `name` and pid, so two concurrent tests never share one and nothing is
/// ever written beside the cauldron's files.
pub fn pause_at(stage: &str, job: &str, dir: &Path, name: &str, extra: &[(&str, &str)]) -> Paused {
    let latch_dir = test_scratch::tmp_path(format!(
        "spine-pause-{name}-{}-{}",
        std::process::id(),
        stage
    ));
    let _ = std::fs::remove_dir_all(&latch_dir);
    std::fs::create_dir_all(&latch_dir).expect("create the latch directory");
    let latch = latch_dir.join("latch");
    let reached = spine::fault::reached_path(&latch);

    let mut cmd = victim(job, dir, None);
    cmd.env(spine::fault::PAUSE_ENV, stage)
        .env(spine::fault::PAUSE_LATCH_ENV, latch.as_os_str())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn the victim");

    // WAIT FOR THE MARKER, or for the child to die first — a child that
    // exits before reaching the stage is a result to report, never a hang.
    let started = Instant::now();
    loop {
        if reached.is_file() {
            break;
        }
        if let Some(status) = child.try_wait().expect("poll the child") {
            let out = child.wait_with_output().expect("collect the child's output");
            reject_a_stale_victim(&out);
            panic!(
                "the victim exited ({status}) before reaching `{stage}` on job `{job}`; \
                 the pause never opened.\nstdout: {}\nstderr: {}",
                stdout_of(&out),
                stderr_of(&out)
            );
        }
        assert!(
            started.elapsed() < REACH_CEILING,
            "the victim never reached `{stage}` on job `{job}` within {REACH_CEILING:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    // The marker names the stage it was written at: one more check that the
    // seam speaks recovery's vocabulary and that THIS stage is the one held.
    let named = std::fs::read_to_string(&reached).expect("read the marker");
    assert_eq!(
        named.trim(),
        stage,
        "the reached-marker names the stage the child is held at"
    );
    Paused {
        child: Some(child),
        stage: stage.to_string(),
        latch,
        reached,
        latch_dir,
    }
}

impl Paused {
    /// Is the held child still alive (and therefore still holding)?
    pub fn is_holding(&mut self) -> bool {
        match self.child.as_mut() {
            Some(c) => c.try_wait().expect("poll the child").is_none(),
            None => false,
        }
    }

    /// RELEASE the pause and wait for the child to finish; hand back
    /// everything it said. Blocks until the child has EXITED, so nothing
    /// after this call races the rest of its commit path (the same guarantee
    /// the old hand-driven harness gave with a `join`).
    pub fn release(mut self) -> Output {
        std::fs::write(&self.latch, b"released").expect("write the latch");
        let child = self.child.take().expect("released once");
        let out = child.wait_with_output().expect("collect the child's output");
        reject_a_stale_victim(&out);
        out
    }

    /// Release, and insist the child completed normally: exit 0, not a
    /// signal, not a status.
    pub fn release_and_expect_exit_zero(self) -> Output {
        let stage = self.stage.clone();
        let out = self.release();
        assert_eq!(
            out.status.code(),
            Some(0),
            "a released pause at `{stage}` must let the commit finish and the process exit 0.\n\
             stdout: {}\nstderr: {}",
            stdout_of(&out),
            stderr_of(&out)
        );
        out
    }

    /// Do NOT release: wait for the child to end on its own (the ceiling
    /// test), and hand back what happened. BOUNDED — a child that outlives
    /// `within` is killed and reported, so a build whose pause has no
    /// ceiling fails this test instead of hanging the suite on it.
    pub fn wait_unreleased_within(mut self, within: Duration) -> Output {
        let mut child = self.child.take().expect("waited once");
        let started = Instant::now();
        loop {
            if child.try_wait().expect("poll the child").is_some() {
                return child.wait_with_output().expect("collect the child's output");
            }
            if started.elapsed() >= within {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "the unreleased pause at `{}` did not end on its own within {within:?}: a \
                     pause nobody releases must abort at its ceiling, never hang",
                    self.stage
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Paused {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // A test that panicked mid-window: let the child go rather than
            // leave it waiting on its ceiling, then reap it.
            let _ = std::fs::write(&self.latch, b"released by drop");
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.latch_dir);
    }
}

pub fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

pub fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Pull `key=value` out of the victim's one machine-readable stdout line.
pub fn field(out: &Output, key: &str) -> Option<String> {
    stdout_of(out)
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix(&format!("{key}=")).map(str::to_string))
}

// ---------------------------------------------------------------------------
// reading the cauldron the victim left behind
// ---------------------------------------------------------------------------

/// Reopen the cauldron the victim left behind WITHOUT healing it. Since
/// `Cauldron::open` runs recovery, which would finish the very
/// crash window these suites exist to inspect before `the_stuck_row` could
/// look at it; the test-support `open_unrecovered` is the handle the old
/// `open` was. The healing open itself is proven in `tests/seams_*.rs`.
pub fn open(dir: &Path) -> Cauldron {
    Cauldron::open_unrecovered(dir, SpineConfig::default())
        .expect("reopen the cauldron after the kill")
}

pub fn spine_of(dir: &Path) -> SpineDb {
    SpineDb::open(&dir.join(SPINE_FILE)).expect("open the spine file")
}

/// A direct connection to the spine file. Direct SQL on the spine is legal
/// for a test and it is the ONLY way
/// to poison a statement text after the decided flip — nothing public can
/// rewrite a row, and rightly so.
pub fn spine_sql(dir: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(dir.join(SPINE_FILE)).expect("direct SQL on the spine file")
}

/// The one un-cleared row a killed commit leaves. Fails loudly when there is
/// not exactly one: "the row tells which" is meaningless if the worklist is
/// ambiguous.
pub fn the_stuck_row(dir: &Path) -> TxRow {
    let spine = spine_of(dir);
    let mut pending = spine.pending().expect("pending");
    assert_eq!(
        pending.len(),
        1,
        "a killed commit leaves exactly ONE un-cleared row; found {:?}",
        pending
            .iter()
            .map(|r| (r.seq, r.decided, r.cleared))
            .collect::<Vec<_>>()
    );
    pending.remove(0)
}

/// How many rows the lock table still holds for one transaction. Locks
/// release only at the CLEARED flip, so a process killed after
/// its grant leaves them held — durably, in the spine, because the locks are
/// tables and not process state.
pub fn lock_rows(dir: &Path, seq: Seq) -> usize {
    spine_sql(dir)
        .query_row(
            "SELECT COUNT(*) FROM locks WHERE seq = ?1",
            [seq.0],
            |r| r.get::<_, i64>(0),
        )
        .expect("count the lock rows") as usize
}

/// Rewrite one row's statement text, in place, by direct SQL. THE poison of
/// the recovery family: roll-forward replays the row's COMPILED DELTAS and
/// never the text, and the only way to prove that is to make the text lie
/// after the payload is already durable.
pub fn poison_statement(dir: &Path, seq: Seq, statement: &str) {
    let n = spine_sql(dir)
        .execute(
            "UPDATE transactions SET statement = ?1 WHERE seq = ?2",
            rusqlite::params![statement, seq.0],
        )
        .expect("poison the statement text");
    assert_eq!(n, 1, "the poison must land on exactly one row");
}

/// Rewrite one row's bindings, in place. The statement text's other half: a
/// recovery that re-matched would resolve `$name` from here.
pub fn poison_bindings(dir: &Path, seq: Seq, bindings_json: &str) {
    let n = spine_sql(dir)
        .execute(
            "UPDATE transactions SET bindings = ?1 WHERE seq = ?2",
            rusqlite::params![bindings_json, seq.0],
        )
        .expect("poison the bindings");
    assert_eq!(n, 1, "the poison must land on exactly one row");
}

// ---------------------------------------------------------------------------
// observing what landed
// ---------------------------------------------------------------------------

/// What the two pools hold, read POOL BY POOL.
///
/// Single-pool reads on purpose: a cross-pool validated read is inconsistent
/// by construction while a decided transaction is mid-landing,
/// so asking one in the half-landed window would measure the reader instead
/// of the disk. The per-pool atomic step is exactly what makes each half of
/// this observation trustworthy on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// The seeded doc's state in alpha — `draft` before the commit,
    /// `published` after it.
    pub doc_state: Option<String>,
    /// How many tickets exist in beta.
    pub tickets: usize,
}

pub fn observe(c: &Cauldron) -> Observation {
    Observation {
        doc_state: seed_doc_state(c),
        tickets: ticket_count(c),
    }
}

pub fn seed_doc_state(c: &Cauldron) -> Option<String> {
    rows(c, r#"MATCH (d: fixture-alpha/doc) RETURN d.title, d.state"#)
        .into_iter()
        .find(|r| r["d.title"] == serde_json::json!(SEED_TITLE))
        .and_then(|r| r["d.state"].as_str().map(str::to_string))
}

pub fn docs_titled(c: &Cauldron, title: &str) -> usize {
    rows(c, r#"MATCH (d: fixture-alpha/doc) RETURN d.title"#)
        .iter()
        .filter(|r| r["d.title"] == serde_json::json!(title))
        .count()
}

pub fn ticket_count(c: &Cauldron) -> usize {
    rows(c, r#"MATCH (t: fixture-beta/ticket) RETURN t.id"#).len()
}

/// The CROSS-POOL hop, which only answers once the commit is whole. Never
/// call it inside the half-landed window.
pub fn catalogues_hops(c: &Cauldron) -> usize {
    rows(
        c,
        r#"MATCH (t: fixture-beta/ticket) -[catalogues]-> (d: fixture-alpha/doc) RETURN d.id"#,
    )
    .len()
}

pub fn rows(
    c: &Cauldron,
    statement: &str,
) -> Vec<std::collections::BTreeMap<String, serde_json::Value>> {
    c.read(&ReadRequest {
        statement: statement.to_string(),
        bindings: spine::envelope::Bindings::new(),
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    })
    .expect("bulk read")
    .rows
    .0
}

/// NOTHING LANDED: the seeded doc is untouched and beta holds no ticket.
pub fn nothing_landed(o: &Observation) -> bool {
    o.doc_state.as_deref() == Some("draft") && o.tickets == 0
}

/// EVERYTHING LANDED: alpha's half and beta's half, both.
pub fn everything_landed(o: &Observation) -> bool {
    o.doc_state.as_deref() == Some("published") && o.tickets == 1
}

/// THE ATOMICITY INVARIANT, in one call: **everything landed or nothing, and the row
/// tells which.** The row's `decided` flag is the commit point of the whole
/// design, so it — and nothing else — must agree with the disk.
pub fn assert_all_or_nothing(c: &Cauldron, row: &TxRow, stage: &str) {
    let o = observe(c);
    let all = everything_landed(&o);
    let none = nothing_landed(&o);
    assert!(
        all ^ none,
        "after recovery from a kill at `{stage}` the pools are in neither state: {o:?}"
    );
    assert_eq!(
        all, row.decided,
        "the ROW must tell which: decided = {}, but the pools say landed = {all} ({o:?})",
        row.decided
    );
    assert!(
        row.cleared,
        "recovery leaves no un-cleared row behind (kill at `{stage}`)"
    );
    assert_eq!(
        row.parts.iter().all(|p| p.landed),
        all,
        "the per-part landed flags must agree with the pools (kill at `{stage}`)"
    );
    if all {
        assert_eq!(
            catalogues_hops(c),
            1,
            "the cross-pool edge is part of the same commit (kill at `{stage}`)"
        );
    }
}

/// The pools a row's parts name, in the payload's frozen landing order.
pub fn part_pools(row: &TxRow) -> Vec<PoolName> {
    row.parts.iter().map(|p| p.pool.clone()).collect()
}

pub fn hex(h: &TxHash) -> String {
    h.to_hex()
}
