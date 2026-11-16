//! **EVERY TEST'S SCRATCH LIVES UNDER ONE PER-RUN ROOT AND IS GONE AT EXIT.**
//!
//! Tests used to make their scratch directly in the machine's shared `$TMPDIR`
//! and leave a lot of it behind (enough, over time, to make every `getcwd`
//! walk on the machine take seconds). This crate is
//! linked ONLY into test binaries (a `[dev-dependencies]` entry under
//! `testkit/`, structurally never deployed) and fixes that at the process level.
//!
//! # The per-run scratch root
//!
//! The same initializer creates ONE scratch root for the run and points
//! `TMPDIR` at it, so `tempfile::*`, `std::env::temp_dir()` and every child the
//! tests spawn (which inherit `TMPDIR`) write inside it and nowhere else:
//!
//! * the root is `$CAULDRON_TEST_SCRATCH/cauldron-test-<pid>`, falling back to
//!   `$TMPDIR/cauldron-test-<pid>` when that variable is unset;
//! * before creating it, any `cauldron-test-<pid>` in the same base whose pid
//!   is no longer alive is removed (a run killed by SIGKILL never got to exit);
//! * a child process that is itself a test binary (a re-exec) finds
//!   `CAULDRON_TEST_ROOT` in its environment and JOINS its parent's root rather
//!   than nesting a new one, so a path a parent and child both compute agrees
//!   (it leaves `TMPDIR` as the parent set it). In a joined child, [`tmp_path`]
//!   registers a path only if it does not exist yet: one the parent made stays
//!   the parent's;
//! * at exit (libtest calls `process::exit`, which runs `atexit` handlers) the
//!   owning process removes what the helpers below registered, then LOOKS at
//!   the root: if anything else is left in it, something leaked a directory
//!   (`into_path()`, `keep()`, a `TempDir` in a `static`, a hand-rolled dir with
//!   no removal, a child that wrote into `TMPDIR` and never cleaned up), so it
//!   prints what it found, removes the root anyway and FAILS THE RUN with exit
//!   code 101. Nothing is left behind either way; the red is the evidence.
//!
//! The helpers, in the order to prefer them:
//!
//! * [`scratch`] — a fresh, unique directory under the root, returned as a
//!   [`Scratch`] guard that removes it on drop (including when the test panics,
//!   since the guard is dropped during unwinding). It derefs to `Path`.
//! * [`tmp_path`] — the drop-in for `std::env::temp_dir().join(rel)` at a site
//!   that hands a `PathBuf` around and cannot hold a guard: same path, nothing
//!   created, and `<root>/<first component of rel>` is removed when THE TEST
//!   THAT ASKED FOR IT ENDS (a thread-local guard on libtest's per-test thread,
//!   so a panicking test cleans up too). Called from a thread that is not a
//!   libtest test thread, the path is owned by the run instead (removed at exit,
//!   never reported).
//! * [`run_path`] / [`run_scope`] — for scratch that genuinely lives as long as
//!   the binary (a fixture behind a `OnceLock`, a fixed name several tests
//!   share): it is under `<root>/.run/`, removed at exit, never reported.
//! * [`root`] — the root itself, e.g. for a child spawned with `env_clear()`:
//!   `cmd.env("TMPDIR", test_isolation::root())`.
//!
//! It is its own crate (not `test-isolation`) so that linking it changes
//! nothing but where scratch goes: it does NOT export `CAULDRON_ISOLATED`.
//! `test-isolation` re-exports it, so a binary that links that gets both.
//!
//! Each test binary invokes [`install!`] once at its root; that adds one test
//! asserting the root is in place, and that reference is also what makes the
//! linker keep this object (an unreferenced rlib object is dropped). Calling
//! any helper below references it too.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Where the per-run roots are made, when set (else `$TMPDIR`).
pub const SCRATCH_ENV: &str = "CAULDRON_TEST_SCRATCH";
/// Exported to every child: the root a child test binary joins.
pub const ROOT_ENV: &str = "CAULDRON_TEST_ROOT";
/// Every per-run root is named `<PREFIX><pid>`.
pub const PREFIX: &str = "cauldron-test-";
/// The run-lifetime sub-directory, exempt from the exit check.
pub const RUN_SCOPE: &str = ".run";
/// PRODUCT scratch the code under test makes in `TMPDIR` and never removes,
/// which a test cannot reach to clean up (product code is out of a test's
/// hands). Removed at exit with a NOTE on stderr, not a failure. Each entry is
/// a known product leak with an owner, not a licence.
///
/// * `tui-isolated-home-` — `tui::cauldron::live::scratch_home`: an isolated
///   face process makes `$TMPDIR/tui-isolated-home-<pid>` once and never
///   removes it.
pub const KNOWN_PRODUCT_LEAKS: &[&str] = &["tui-isolated-home-"];

static RAN: AtomicBool = AtomicBool::new(false);

struct Run {
    root: PathBuf,
    /// True in the process that created the root; false in a child that joined.
    owner: bool,
}

static RUN: OnceLock<Option<Run>> = OnceLock::new();
/// Paths owned by the run (removed at exit, never reported).
static RUN_OWNED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
/// Paths owned by a test thread that has not ended yet.
static LIVE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static SEQ: AtomicU64 = AtomicU64::new(0);

extern "C" {
    fn atexit(cb: extern "C" fn()) -> i32;
    fn _exit(code: i32) -> !;
    fn kill(pid: i32, sig: i32) -> i32;
}

extern "C" fn at_load() {
    let _ = RUN.set(init_root());
    RAN.store(true, Ordering::SeqCst);
}

fn init_root() -> Option<Run> {
    // A re-exec'd child of a test binary joins its parent's root.
    if let Some(r) = std::env::var_os(ROOT_ENV).map(PathBuf::from) {
        if r.is_dir() {
            // TMPDIR is left as the parent gave it: normally the root itself,
            // but a test may hand its child a TMPDIR on purpose.
            return Some(Run { root: r, owner: false });
        }
    }
    let base = match std::env::var_os(SCRATCH_ENV).filter(|v| !v.is_empty()) {
        Some(b) => {
            let b = PathBuf::from(b);
            if std::fs::create_dir_all(&b).is_err() {
                return None;
            }
            b
        }
        None => std::env::temp_dir(),
    };
    sweep_dead(&base);
    let root = base.join(format!("{PREFIX}{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root); // a remnant of a reused pid
    if std::fs::create_dir_all(root.join(RUN_SCOPE)).is_err() {
        return None;
    }
    // Who owns this root, so a root a killed run left behind can be traced.
    let _ = std::fs::write(
        root.join(RUN_SCOPE).join("owner"),
        std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default(),
    );
    std::env::set_var("TMPDIR", &root);
    std::env::set_var(ROOT_ENV, &root);
    unsafe {
        atexit(at_exit);
    }
    Some(Run { root, owner: true })
}

fn pid_alive(pid: i32) -> bool {
    // kill(pid, 0): 0 = exists; -1 with EPERM = exists but not ours.
    if pid <= 0 {
        return false;
    }
    let r = unsafe { kill(pid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(1)
}

/// Remove every `cauldron-test-<pid>` directly under `base` whose pid is dead.
/// Returns how many it removed.
pub fn sweep_dead(base: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(base) else { return 0 };
    let mut n = 0;
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(pid) = name.strip_prefix(PREFIX).and_then(|p| p.parse::<i32>().ok()) else { continue };
        if pid as u32 != std::process::id() && !pid_alive(pid) && std::fs::remove_dir_all(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

fn remove(p: &Path) {
    if std::fs::symlink_metadata(p).map(|m| m.is_dir()).unwrap_or(false) {
        let _ = std::fs::remove_dir_all(p);
    } else {
        let _ = std::fs::remove_file(p);
    }
}

extern "C" fn at_exit() {
    let code = std::panic::catch_unwind(exit_check).unwrap_or(0);
    if code != 0 {
        unsafe { _exit(code) }
    }
}

/// What the owning process does at exit; returns the exit code to force (0 = none).
fn exit_check() -> i32 {
    let Some(Some(run)) = RUN.get() else { return 0 };
    if !run.owner {
        return 0;
    }
    let owned: Vec<PathBuf> = {
        let mut v = std::mem::take(&mut *RUN_OWNED.lock().unwrap_or_else(|e| e.into_inner()));
        v.append(&mut LIVE.lock().unwrap_or_else(|e| e.into_inner()));
        v
    };
    for p in &owned {
        remove(p);
    }
    let _ = std::fs::remove_dir_all(run.root.join(RUN_SCOPE));
    sweep_dead(&run.root);
    let (product, left): (Vec<OsString>, Vec<OsString>) = leftovers(&run.root)
        .into_iter()
        .partition(|n| n.to_str().is_some_and(|n| KNOWN_PRODUCT_LEAKS.iter().any(|p| n.starts_with(p))));
    for n in &product {
        eprintln!("test-isolation NOTE: removed known product scratch {} (product code never removes it)", n.to_string_lossy());
    }
    if left.is_empty() {
        remove_root(&run.root);
        return 0;
    }
    eprintln!(
        "\ntest-isolation: {} entr{} LEFT BEHIND in this run's scratch root {} — a test or a child it spawned \
         leaked scratch (into_path/keep/a static TempDir/a hand-rolled dir with no removal). Use test_isolation::scratch(), \
         tmp_path() or run_path(). The root has been removed; failing the run.",
        left.len(),
        if left.len() == 1 { "y" } else { "ies" },
        run.root.display()
    );
    for n in left.iter().take(25) {
        eprintln!("  left behind: {}{}", n.to_string_lossy(), peek(&run.root.join(n)));
    }
    if left.len() > 25 {
        eprintln!("  ... and {} more", left.len() - 25);
    }
    remove_root(&run.root);
    101
}

/// What is inside a leftover dir (two levels, a few names), so the report says
/// which code made it.
fn peek(p: &Path) -> String {
    let mut names = Vec::new();
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten().take(6) {
            let mut n = e.file_name().to_string_lossy().into_owned();
            if let Ok(rd2) = std::fs::read_dir(e.path()) {
                let inner: Vec<String> = rd2.flatten().take(4).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
                n = format!("{n}/{{{}}}", inner.join(","));
            }
            names.push(n);
        }
    }
    if names.is_empty() { String::new() } else { format!("  [{}]", names.join(" ")) }
}

/// Remove the root, and again if a thread that is still running (a daemon a
/// test never stopped) re-creates something in it before the process is gone.
fn remove_root(root: &Path) {
    for _ in 0..5 {
        let _ = std::fs::remove_dir_all(root);
        std::thread::sleep(std::time::Duration::from_millis(20));
        if !root.exists() {
            return;
        }
    }
}

fn leftovers(root: &Path) -> Vec<OsString> {
    let mut v: Vec<OsString> = std::fs::read_dir(root)
        .map(|rd| rd.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// Whether the load-time initializer ran in this process. Reads [`AT_LOAD`]
/// so that referencing this keeps the initializer linked.
pub fn ran() -> bool {
    std::hint::black_box(AT_LOAD as usize);
    RAN.load(Ordering::SeqCst)
}

/// This run's scratch root (what `TMPDIR` points at). Falls back to
/// `std::env::temp_dir()` if the root could not be made.
pub fn root() -> PathBuf {
    ran();
    match RUN.get() {
        Some(Some(r)) => r.root.clone(),
        _ => std::env::temp_dir(),
    }
}

/// Whether this process owns the root (false in a child that joined one).
pub fn owns_root() -> bool {
    ran();
    matches!(RUN.get(), Some(Some(Run { owner: true, .. })))
}

/// `<root>/.run`: scratch that lives as long as the binary; removed at exit and
/// exempt from the leak check. For fixtures behind a `OnceLock`.
pub fn run_scope() -> PathBuf {
    let p = root().join(RUN_SCOPE);
    let _ = std::fs::create_dir_all(&p);
    p
}

/// The drop-in for a FIXED-name `temp_dir().join(rel)` that several tests in a
/// binary share: `<root>/.run/<rel>`, removed at exit. Nothing is created.
pub fn run_path(rel: impl AsRef<Path>) -> PathBuf {
    run_scope().join(rel)
}

fn top(root: &Path, rel: &Path) -> PathBuf {
    match rel.components().next() {
        Some(c) => root.join(c),
        None => root.to_path_buf(),
    }
}

/// True on a thread libtest spawned for one test (it names it after the test).
fn on_test_thread() -> bool {
    let t = std::thread::current();
    match t.name() {
        None | Some("main") => false,
        Some(n) => !n.starts_with("tokio-") && !n.starts_with("rayon"),
    }
}

struct ThreadScope(Vec<PathBuf>);

impl Drop for ThreadScope {
    fn drop(&mut self) {
        let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
        for p in self.0.drain(..) {
            remove(&p);
            live.retain(|q| q != &p);
        }
    }
}

thread_local! {
    static SCOPE: RefCell<ThreadScope> = const { RefCell::new(ThreadScope(Vec::new())) };
}

/// The drop-in for `std::env::temp_dir().join(rel)`: returns `<root>/<rel>`
/// (nothing is created) and arranges for `<root>/<first component>` to be
/// removed when the calling test ends — panic or not. See the crate docs.
pub fn tmp_path(rel: impl AsRef<Path>) -> PathBuf {
    let root = root();
    let rel = rel.as_ref();
    let p = root.join(rel);
    let t = top(&root, rel);
    if t == root {
        return p;
    }
    if !owns_root() && t.exists() {
        // A joined child recomputing a path its parent already made: the
        // parent owns it (and may still need it after this child exits).
        return p;
    }
    if on_test_thread() {
        let fresh = SCOPE
            .try_with(|s| {
                let mut s = s.borrow_mut();
                if s.0.contains(&t) {
                    false
                } else {
                    s.0.push(t.clone());
                    true
                }
            })
            .unwrap_or(false);
        if fresh {
            LIVE.lock().unwrap_or_else(|e| e.into_inner()).push(t);
        }
    } else {
        let mut v = RUN_OWNED.lock().unwrap_or_else(|e| e.into_inner());
        if !v.contains(&t) {
            v.push(t);
        }
    }
    p
}

/// A unique directory under the run's root, removed when dropped (which
/// includes unwinding out of a panicking test).
pub struct Scratch {
    path: PathBuf,
}

/// Make a fresh, unique scratch directory named after `tag`.
pub fn scratch(tag: &str) -> Scratch {
    Scratch::new(tag)
}

impl Scratch {
    pub fn new(tag: &str) -> Scratch {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let tag: String = tag.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        let path = root().join(format!("{tag}-{}-{n}-{nanos:x}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap_or_else(|e| panic!("test-isolation: create {}: {e}", path.display()));
        Scratch { path }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn to_path_buf(&self) -> PathBuf {
        self.path.clone()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl std::fmt::Debug for Scratch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.path.fmt(f)
    }
}

#[used]
#[cfg_attr(target_os = "macos", link_section = "__DATA,__mod_init_func")]
#[cfg_attr(target_os = "linux", link_section = ".init_array")]
static AT_LOAD: extern "C" fn() = at_load;

/// Invoke once at a test binary's root: adds a test that fails unless this
/// binary's scratch root is in place and `TMPDIR` points at it.
#[macro_export]
macro_rules! install {
    () => {
        #[test]
        fn this_test_binary_scratch_lives_under_one_run_root() {
            assert!($crate::ran(), "the test-scratch load-time initializer did not run in this test binary");
            let r = $crate::root();
            assert!(r.is_dir(), "no scratch root: {}", r.display());
            assert_eq!(std::env::temp_dir().as_path(), r.as_path(), "TMPDIR does not point at this run's scratch root");
            assert_eq!(
                std::env::var_os($crate::ROOT_ENV).map(std::path::PathBuf::from).as_deref(),
                Some(r.as_path()),
                "children are not told the root"
            );
        }
    };
}
