//! The one integration-test binary for `cauldron-ripples`. No test calls a
//! model: the scripted backend answers, or a fake threads chassis does.
//!
//! - `engine`: one run in process over the real door's ripple seam: what is
//!   sent, what lands, the retry, the caps, failures, a dump processed once.
//! - `service`: the binary, its run key, `RUN` and `STATUS`, and runs killed
//!   during the model call and after the put, then run again.
//! - `threads`: the threads backend against a fake chassis.
//! - `local`: the local backend against a fake inference seam.
//! - `eval`: the evaluation with scripted backends, nothing written.

// Every child a test spawns is isolated.
test_isolation::install!();

mod support;

mod engine;
mod service;
mod threads;
mod local;
mod eval;
