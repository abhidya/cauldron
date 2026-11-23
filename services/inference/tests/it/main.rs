//! The one integration-test binary for `cauldron-inference`. No model is
//! loaded and nothing is downloaded: the service runs a fake `llama-server`
//! (this binary's `fake` module, behind a shell script).
//!
//! - `fake`: the fake `llama-server` (a test that does nothing unless the
//!   script runs it);
//! - `service`: the binary: load on demand, argv, completion, key, idle
//!   unload, budget, runtime check, crashes, cancel, no orphan child.

// Every child a test spawns is isolated.
test_isolation::install!();

mod fake;
mod support;

mod service;
