//! Where the `spine_victim` binary is — the ONE spelling of that lookup,
//! shared by `util/kill.rs` (the armed suites) and `atomicity_kill.rs`'s
//! production-build proof (where `kill.rs` is not compiled at all).
//!
//! Included with `#[path = "util/victim.rs"] mod victim;` (or, from
//! `kill.rs`, `#[path = "victim.rs"]`), for the same reason `kill.rs` is.
//!
//! # Why `CARGO_BIN_EXE_spine_victim` and nothing else
//!
//! Under a build wrapper that shares cargo's intermediate build area, a test
//! binary runs out of a SHARED SLOT, while the victim, a final artifact, is
//! uplifted into the caller's own target dir. So "beside `current_exe()`" is
//! a path guess into the slot and finds nothing, and every suite that spawns
//! the victim goes red. The robust route is the variable cargo itself sets
//! for an integration test: inside a build script or test, cargo's `OUT_DIR`
//! and `CARGO_BIN_EXE_<name>` are set correctly. Cargo sets it only for a
//! `[[bin]]` target, which is why the victim is declared as one in
//! `Cargo.toml` (with `required-features = ["test-support"]`, so no
//! production build ever builds it).
//!
//! Read at COMPILE time (`option_env!`), because that is when cargo sets it
//! for an integration test.

#![allow(dead_code)]

use std::path::PathBuf;

/// The victim's path, as cargo reported it to this test target at compile
/// time. `None` when cargo did not set it (the bin target was not declared,
/// or its required feature was off).
const CARGO_BIN_EXE_SPINE_VICTIM: Option<&str> = option_env!("CARGO_BIN_EXE_spine_victim");

/// Where cargo put the `spine_victim` binary. Fails LOUDLY, naming what it
/// looked for, rather than silently skipping: a kill suite that quietly stops
/// spawning children is a fake green, and this is the one path that could
/// produce one.
pub fn victim_binary() -> PathBuf {
    let Some(path) = CARGO_BIN_EXE_SPINE_VICTIM else {
        panic!(
            "cargo did not set CARGO_BIN_EXE_spine_victim for this test target, so there is no \
             `spine_victim` binary to kill. It is a `[[bin]]` of the spine package with \
             `required-features = [\"test-support\"]` (see libs/spine/Cargo.toml); never treat \
             this as a reason to skip the kill"
        );
    };
    let path = PathBuf::from(path);
    assert!(
        path.is_file(),
        "CARGO_BIN_EXE_spine_victim names {}, and no file is there. Cargo uplifts the victim \
         into the target dir as part of `cargo test`, so either it did not build or a concurrent \
         cargo in this worktree replaced it mid-run — never treat it as a reason to skip the kill",
        path.display()
    );
    path
}
