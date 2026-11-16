//! The `SPINE_FAULT` kill form says WHY on stderr, even under a
//! captured `cargo test` run.
//!
//! The defect: `fault::at_stage_with` wrote its reason with `eprintln!` and
//! then aborted. libtest's output capture redirects `eprintln!` into a
//! per-test buffer that is only replayed when the test RETURNS; an abort
//! never returns, so a captured run showed a bare SIGABRT and nothing else
//! (`tests/door_smoke.rs` only kept the message because it runs its child
//! with `--nocapture`). The fix routes the kill through
//! `say_then_abort`, which writes through the raw `std::io::stderr()`
//! handle that capture does not intercept.
//!
//! # How this is measured
//!
//! The same shape as `tests/fault_pause_abort_says_why.rs`: this
//! test binary re-executes itself filtered to [`child_entry`], WITHOUT
//! `--nocapture` — capture ON, the exact condition of the defect — with
//! `SAYS_WHY_CHILD=<mode>` set and `SPINE_FAULT` set on the `Command` (never on
//! this process, so these tests cannot race the parent's other tests). The
//! parent reads the child's real stderr pipe.
//!
//! Instrument checks, so a pass cannot be the probe witnessing itself:
//! - every child must print libtest's `running 1 test` — the filter matched
//!   and the child body ran;
//! - every child writes a SENTINEL through raw stderr BEFORE the seam call,
//!   proving the pipe carries raw stderr writes, so "the reason is absent"
//!   could have come out differently;
//! - every child writes an AFTER line through raw stderr once the seam
//!   RETURNS: the kill case asserts it is absent (the seam, not something
//!   later, killed the child) and the negative case asserts it is present;
//! - the kill case asserts death by SIGABRT, so a seam that merely prints
//!   the reason and returns fails; the negative case (the variable naming a
//!   DIFFERENT stage) asserts a clean exit and no reason, so a seam that
//!   always prints fails.

#![cfg(feature = "fault-injection")]

use spine::fault::{FAULT_ENV, PAUSE_ENV, PAUSE_LATCH_ENV};
use spine::recover::Stage;
use std::process::{Command, Output};

test_scratch::install!();

const CHILD_ENV: &str = "SAYS_WHY_CHILD";
const SENTINEL: &str = "SAYS-WHY-CHILD-SENTINEL: raw stderr reaches the parent";
const AFTER: &str = "SAYS-WHY-CHILD-AFTER: the seam returned";

/// The child half — re-executed by the parent tests below, never a test of
/// its own. `#[ignore]`: as a plain `#[test]` with an early return
/// it was counted as a PASS in every top-level run while checking nothing.
/// Now a top-level run lists it as `ignored`, the parents run it with
/// `--ignored --exact child_entry`, and a run that reaches it WITHOUT
/// `SAYS_WHY_CHILD` (e.g. `-- --ignored` by hand) FAILS by name rather than pass.
#[test]
#[ignore = "child entry: run only by this file's parent tests (--ignored --exact child_entry)"]
fn child_entry() {
    let mode = std::env::var(CHILD_ENV).unwrap_or_else(|_| {
        panic!("child_entry is run only by this file's parent tests, with SAYS_WHY_CHILD set")
    });
    use std::io::Write;
    {
        let mut e = std::io::stderr().lock();
        writeln!(e, "{SENTINEL} (mode {mode})").unwrap();
        e.flush().unwrap();
    }
    let r = spine::fault::at_stage(Stage::Decided);
    {
        let mut e = std::io::stderr().lock();
        writeln!(e, "{AFTER}: {r:?}").unwrap();
        e.flush().unwrap();
    }
    assert!(r.is_ok(), "the seam returned {r:?}");
}

fn run_child(mode: &str, fault_value: &str) -> Output {
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(exe)
        // capture ON: no --nocapture, deliberately.
        .args(["--ignored", "--exact", "child_entry", "--test-threads=1"])
        .env(CHILD_ENV, mode)
        .env(FAULT_ENV, fault_value)
        .env_remove(PAUSE_ENV)
        .env_remove(PAUSE_LATCH_ENV)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("running 1 test"),
        "the child filter did not run exactly child_entry\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains(SENTINEL),
        "the child's raw stderr never reached the parent's pipe — the instrument is blind\n\
         stderr:\n{stderr}"
    );
    out
}

#[cfg(unix)]
fn died_by_sigabrt(out: &Output) -> bool {
    use std::os::unix::process::ExitStatusExt;
    out.status.code().is_none() && out.status.signal() == Some(6)
}

fn reason_lines(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|l| l.starts_with("SPINE_FAULT:") && l.contains("aborting"))
        .collect()
}

#[cfg(unix)]
#[test]
fn spine_fault_abort_says_why_on_stderr_under_capture() {
    let out = run_child("kill", Stage::Decided.name());
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        died_by_sigabrt(&out),
        "expected SIGABRT, got {:?}\nstderr:\n{stderr}",
        out.status
    );
    assert!(
        !stderr.contains(AFTER),
        "the seam returned, so the seam did not kill the child\nstderr:\n{stderr}"
    );
    let lines = reason_lines(&stderr);
    assert_eq!(lines.len(), 1, "want exactly one abort reason line\nstderr:\n{stderr}");
    assert!(
        lines[0].contains(&format!("`{}`", Stage::Decided.name())),
        "reason does not name the stage: {}",
        lines[0]
    );
}

/// The paired negative: `SPINE_FAULT` naming a stage OTHER than
/// the one reached does not abort and prints no reason.
#[cfg(unix)]
#[test]
fn spine_fault_at_other_stage_neither_aborts_nor_says_why() {
    let out = run_child("other", Stage::BetweenLandings.name());
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "the child did not exit cleanly: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(stdout.contains("test result: ok. 1 passed"), "child body did not pass\n{stdout}");
    assert!(stderr.contains(AFTER), "the seam never returned\nstderr:\n{stderr}");
    assert!(reason_lines(&stderr).is_empty(), "an abort reason with no abort\n{stderr}");
}
