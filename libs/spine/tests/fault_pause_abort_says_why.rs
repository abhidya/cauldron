//! Both abort paths of `fault::pause_here` say WHY on stderr, even
//! under a captured `cargo test` run.
//!
//! The defect: the two aborts wrote
//! their reason with `eprintln!`, which libtest's output capture redirects
//! into a per-test buffer that is only replayed when the test RETURNS. An
//! abort never returns, so a captured run showed a bare SIGABRT and nothing
//! else. The fix writes the reason through the raw `std::io::stderr()`
//! handle, which capture does not intercept.
//!
//! # How this is measured
//!
//! An abort kills the process, so each case runs in a CHILD: this test
//! binary re-executes itself (`std::env::current_exe()`) filtered to the
//! single test [`child_entry`], WITHOUT `--nocapture` — capture ON, the
//! exact condition of the defect — with `PAUSE_ABORT_CHILD=<mode>` naming what the
//! child does. The parent reads the child's real stderr pipe.
//!
//! Instrument checks, so a pass cannot be the probe witnessing itself:
//! - every child must print libtest's `running 1 test` — the filter matched
//!   and the child body ran (a filter that matched nothing would exit 0
//!   with empty stderr, a fake green for the negative case);
//! - every child writes a sentinel line through raw stderr BEFORE the
//!   seam call — proves the pipe the parent reads does carry raw stderr
//!   writes, so "the reason is absent" could have come out differently;
//! - the abort cases assert death by SIGABRT, and that the pause was
//!   actually entered (the ceiling case finds the `.reached` marker); the
//!   negative case asserts a clean exit, `Ok(())` from the seam, and the
//!   marker too.
//!
//! The child's environment is built explicitly (`SPINE_FAULT` removed, the
//! pause variables set on the `Command`), never by mutating this process's
//! environment, so these tests cannot race the parent's other tests.

#![cfg(feature = "fault-injection")]

use spine::fault::{PAUSE_CEILING_ENV, PAUSE_ENV, PAUSE_LATCH_ENV, PAUSE_REACHED_SUFFIX};
use spine::recover::Stage;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

test_scratch::install!();

const CHILD_ENV: &str = "PAUSE_ABORT_CHILD";
const SENTINEL: &str = "PAUSE-ABORT-CHILD-SENTINEL: raw stderr reaches the parent";

/// The reason the ceiling abort prints (the stable head of its message).
const CEILING_REASON: &str = "was never released within";
/// The reason the marker-write abort prints (the stable head of its message).
const MARKER_REASON: &str = "cannot write the reached-marker";

/// The child half — re-executed by the parent tests below, never a test of
/// its own. `#[ignore]`: as a plain `#[test]` with an early return
/// it was counted as a PASS in every top-level run while checking nothing.
/// Now a top-level run lists it as `ignored`, the parents run it with
/// `--ignored --exact child_entry`, and a run that reaches it WITHOUT
/// `PAUSE_ABORT_CHILD` (e.g. `-- --ignored` by hand) FAILS by name rather than pass.
#[test]
#[ignore = "child entry: run only by this file's parent tests (--ignored --exact child_entry)"]
fn child_entry() {
    let mode = std::env::var(CHILD_ENV).unwrap_or_else(|_| {
        panic!("child_entry is run only by this file's parent tests, with PAUSE_ABORT_CHILD set")
    });
    {
        use std::io::Write;
        let mut e = std::io::stderr().lock();
        writeln!(e, "{SENTINEL} (mode {mode})").unwrap();
        e.flush().unwrap();
    }
    let r = spine::fault::at_stage(Stage::Decided);
    // Reached only when the pause did NOT abort.
    assert!(r.is_ok(), "the seam returned {r:?}");
}

/// The ceiling the two ABORT cases set: short, so the ceiling case dies fast.
const CEILING_MS: u64 = 100;
/// The ceiling the RELEASED case sets: long enough that the parent's
/// see-the-marker-then-write-the-latch round trip lands inside it even under
/// load, and short enough that a broken release reds as the ceiling abort in
/// seconds. The test's name carries it.
const RELEASED_CEILING_MS: u64 = 2_000;

fn scratch(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = test_scratch::tmp_path(format!(
        "spine-r3s6-{name}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn child_command(mode: &str, latch: &Path, ceiling_ms: u64) -> Command {
    let exe = std::env::current_exe().unwrap();
    let mut cmd = Command::new(exe);
    // capture ON: no --nocapture, deliberately.
    cmd.args(["--ignored", "--exact", "child_entry", "--test-threads=1"])
        .env(CHILD_ENV, mode)
        .env_remove(spine::fault::FAULT_ENV)
        .env(PAUSE_ENV, Stage::Decided.name())
        .env(PAUSE_LATCH_ENV, latch.as_os_str())
        .env(PAUSE_CEILING_ENV, ceiling_ms.to_string());
    cmd
}

fn run_child(mode: &str, latch: &Path) -> Output {
    let out = child_command(mode, latch, CEILING_MS).output().unwrap();
    check_instrument(out)
}

/// The instrument checks every child's output must pass (see the module doc).
fn check_instrument(out: Output) -> Output {
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
        .filter(|l| l.starts_with("SPINE_PAUSE:") && l.contains("aborting"))
        .collect()
}

#[cfg(unix)]
#[test]
fn ceiling_abort_says_why_on_stderr_under_capture() {
    let dir = scratch("ceiling");
    let latch = dir.join("latch"); // never created: the pause hits its ceiling
    let out = run_child("ceiling", &latch);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(died_by_sigabrt(&out), "expected SIGABRT, got {:?}\nstderr:\n{stderr}", out.status);
    let mut reached = latch.as_os_str().to_os_string();
    reached.push(PAUSE_REACHED_SUFFIX);
    assert_eq!(
        std::fs::read_to_string(PathBuf::from(reached)).ok().as_deref(),
        Some(Stage::Decided.name()),
        "the pause was never entered, so its abort was not what killed the child"
    );
    let lines = reason_lines(&stderr);
    assert_eq!(lines.len(), 1, "want exactly one abort reason line\nstderr:\n{stderr}");
    assert!(lines[0].contains(CEILING_REASON), "wrong reason: {}", lines[0]);
    assert!(lines[0].contains("`decided`"), "reason does not name the stage: {}", lines[0]);
    assert!(!stderr.contains(MARKER_REASON), "the marker reason fired on the ceiling path\n{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn marker_write_abort_says_why_on_stderr_under_capture() {
    let dir = scratch("marker");
    // The latch's directory does not exist, so the reached-marker cannot be
    // written (its staging file has no parent).
    let latch = dir.join("no-such-dir").join("latch");
    let out = run_child("marker", &latch);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(died_by_sigabrt(&out), "expected SIGABRT, got {:?}\nstderr:\n{stderr}", out.status);
    let lines = reason_lines(&stderr);
    assert_eq!(lines.len(), 1, "want exactly one abort reason line\nstderr:\n{stderr}");
    assert!(lines[0].contains(MARKER_REASON), "wrong reason: {}", lines[0]);
    assert!(lines[0].contains("`decided`"), "reason does not name the stage: {}", lines[0]);
    assert!(!stderr.contains(CEILING_REASON), "the ceiling reason fired on the marker path\n{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The paired negative: a pause that is RELEASED does not
/// abort and prints no abort reason.
///
/// This used to pre-create the latch and then sleep
/// 300 ms after the seam returned. With the latch already present the wait
/// loop never iterates, and the ceiling can only fire INSIDE that loop, so
/// the sleep outlasted nothing that could abort. Now the release is a real
/// event the child is waiting on: the latch is absent, the parent waits for
/// the REAL SIGNAL that the child is held (the `.reached` marker, with the
/// child process still alive), and only then writes the latch. The child
/// must return `Ok` and exit cleanly. The period named is the 2 s ceiling
/// (`RELEASED_CEILING_MS`): the only abort on this path is the ceiling, it
/// can fire only while the child waits, and a broken release turns this red
/// as exactly that abort, inside those 2 s.
// RED (seen): the parent's latch write removed (the release broken) —
// the child dies by SIGABRT at the 2 s ceiling, "was never released within
// 2s"; and, opposed, `pause_here` aborting unconditionally after its wait
// loop (a seam that always aborts) — SIGABRT after the release.
#[cfg(unix)]
#[test]
fn a_pause_released_mid_wait_returns_without_abort_reason_inside_its_2s_ceiling() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let dir = scratch("released");
    let latch = dir.join("latch"); // ABSENT: the child must wait on it
    let mut reached = latch.as_os_str().to_os_string();
    reached.push(PAUSE_REACHED_SUFFIX);
    let reached = PathBuf::from(reached);
    let mut child = child_command("released", &latch, RELEASED_CEILING_MS)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // THE REAL SIGNAL: the marker the seam publishes before its wait loop.
    let started = Instant::now();
    while !reached.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "the child exited before the pause was ever reached"
        );
        assert!(
            started.elapsed() < Duration::from_millis(RELEASED_CEILING_MS),
            "the pause was never reached"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    // Held: the marker is up, the latch is absent, the child is alive.
    assert!(child.try_wait().unwrap().is_none(), "the child is held at the pause");
    assert!(!latch.exists(), "nothing has released it yet");
    // THE RELEASE.
    std::fs::write(&latch, b"released").unwrap();
    let out = check_instrument(child.wait_with_output().unwrap());
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "the released child did not exit cleanly: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(stdout.contains("test result: ok. 1 passed"), "child body did not pass\n{stdout}");
    assert_eq!(
        std::fs::read_to_string(&reached).ok().as_deref(),
        Some(Stage::Decided.name()),
        "the marker names the stage the child was held at"
    );
    assert!(reason_lines(&stderr).is_empty(), "an abort reason on a released pause\n{stderr}");
    assert!(!stderr.contains(CEILING_REASON) && !stderr.contains(MARKER_REASON), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}
