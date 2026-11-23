//! Proof: the exit check FAILS a run that leaves anything in its
//! scratch root, still removes the root, and passes a run that cleans up.
//!
//! Each case re-executes this binary as an OWNING run (its own
//! `CAULDRON_TEST_SCRATCH`, no inherited `CAULDRON_TEST_ROOT`), filtered to one
//! probe test. A probe does nothing unless `TMP_ROOT_PROBE` names it, so a
//! normal run of this binary leaks nothing.

use std::path::{Path, PathBuf};
use std::process::Command;

test_scratch::install!();

const PROBE: &str = "TMP_ROOT_PROBE";

fn probe_is(name: &str) -> bool {
    std::env::var(PROBE).ok().as_deref() == Some(name)
}

#[test]
fn probe_leak_hand_rolled() {
    if probe_is("hand_rolled") {
        std::fs::create_dir_all(std::env::temp_dir().join("hand-rolled-no-removal/inner")).unwrap();
    }
}

#[test]
fn probe_leak_forgotten_guard() {
    if probe_is("forgotten_guard") {
        let s = test_scratch::scratch("forgotten");
        std::fs::write(s.join("f"), b"x").unwrap();
        std::mem::forget(s);
    }
}

#[test]
fn probe_clean() {
    if probe_is("clean") {
        let s = test_scratch::scratch("guarded");
        std::fs::write(s.join("f"), b"x").unwrap();
        let p = test_scratch::tmp_path(format!("drop-in-{}", std::process::id()));
        std::fs::create_dir_all(p.join("deep/er")).unwrap();
        let r = test_scratch::run_path("shared-fixture");
        std::fs::create_dir_all(&r).unwrap();
        let _child = Command::new("/bin/sh").arg("-c").arg("mkdir \"$TMPDIR/child-wrote-here\" && test -d \"$TMPDIR/child-wrote-here\" && rmdir \"$TMPDIR/child-wrote-here\"").status().unwrap();
        assert!(_child.success(), "a child did not get TMPDIR inside the root");
    }
}

#[test]
fn probe_panics_after_making_scratch() {
    if probe_is("panics") {
        let s = test_scratch::scratch("panicky");
        std::fs::write(s.join("f"), b"x").unwrap();
        let p = test_scratch::tmp_path("panicky-drop-in");
        std::fs::create_dir_all(&p).unwrap();
        panic!("deliberate panic with scratch held");
    }
}

struct Case {
    status: std::process::ExitStatus,
    stderr: String,
    base_entries: Vec<String>,
}

fn run(probe: &str, test: &str) -> Case {
    let base = test_scratch::tmp_path(format!("leak-check-base-{probe}"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--test-threads", "1"])
        .env_remove(test_scratch::ROOT_ENV)
        .env(test_scratch::SCRATCH_ENV, &base)
        .env(PROBE, probe)
        .output()
        .unwrap();
    // libtest prints a failing test's captured panic on STDOUT; the exit check writes to stderr.
    let both = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Case { status: out.status, stderr: both, base_entries: entries(&base) }
}

fn entries(p: &Path) -> Vec<String> {
    std::fs::read_dir(p).map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default()
}

#[test]
fn a_hand_rolled_dir_with_no_removal_fails_the_run_and_is_still_removed() {
    let c = run("hand_rolled", "probe_leak_hand_rolled");
    assert_eq!(c.status.code(), Some(101), "the leak did not fail the run: {}", c.stderr);
    assert!(c.stderr.contains("LEFT BEHIND") && c.stderr.contains("hand-rolled-no-removal"), "{}", c.stderr);
    assert!(c.base_entries.is_empty(), "the root was not removed: {:?}", c.base_entries);
}

#[test]
fn a_forgotten_guard_fails_the_run_and_is_still_removed() {
    let c = run("forgotten_guard", "probe_leak_forgotten_guard");
    assert_eq!(c.status.code(), Some(101), "the leak did not fail the run: {}", c.stderr);
    assert!(c.stderr.contains("left behind: forgotten-"), "{}", c.stderr);
    assert!(c.base_entries.is_empty(), "the root was not removed: {:?}", c.base_entries);
}

#[test]
fn guards_drop_ins_run_paths_and_children_leave_nothing_and_pass() {
    let c = run("clean", "probe_clean");
    assert!(c.status.success(), "a clean run failed: {}", c.stderr);
    assert!(!c.stderr.contains("LEFT BEHIND"), "{}", c.stderr);
    assert!(c.base_entries.is_empty(), "left in the scratch base: {:?}", c.base_entries);
}

#[test]
fn a_panicking_test_still_removes_its_scratch() {
    let c = run("panics", "probe_panics_after_making_scratch");
    assert_eq!(c.status.code(), Some(101), "the probe's own panic should fail it: {}", c.stderr);
    assert!(c.stderr.contains("deliberate panic"), "{}", c.stderr);
    assert!(!c.stderr.contains("LEFT BEHIND"), "a panic leaked scratch: {}", c.stderr);
    assert!(c.base_entries.is_empty(), "left in the scratch base: {:?}", c.base_entries);
}

#[test]
fn a_dead_runs_root_is_swept_at_start() {
    let base = test_scratch::tmp_path("leak-check-stale-base");
    let _ = std::fs::remove_dir_all(&base);
    // pid 2^22+7 is above macOS's pid range, so it is never alive.
    let dead: PathBuf = base.join(format!("{}{}", test_scratch::PREFIX, 4_194_311));
    std::fs::create_dir_all(dead.join("junk")).unwrap();
    let live: PathBuf = base.join(format!("{}{}", test_scratch::PREFIX, std::process::id()));
    std::fs::create_dir_all(&live).unwrap();
    assert_eq!(test_scratch::sweep_dead(&base), 1);
    assert!(!dead.exists(), "a dead run's root survived the sweep");
    assert!(live.exists(), "a live run's root was swept");
    std::fs::remove_dir_all(&live).unwrap();
}

#[test]
fn a_child_test_binary_joins_the_parents_root() {
    if std::env::var("TMP_ROOT_JOIN_PROBE").is_ok() {
        println!("JOINED={}", test_scratch::root().display());
        assert!(!test_scratch::owns_root());
        return;
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "a_child_test_binary_joins_the_parents_root", "--nocapture"])
        .env("TMP_ROOT_JOIN_PROBE", "1")
        .output()
        .unwrap();
    let so = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{so} {}", String::from_utf8_lossy(&out.stderr));
    assert!(so.contains(&format!("JOINED={}", test_scratch::root().display())), "{so}");
}
