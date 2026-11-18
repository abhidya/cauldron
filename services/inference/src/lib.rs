//! **cauldron-inference**: local inference, on demand.
//!
//! A supervised service (`services/inference.toml`), one per environment,
//! off unless started (the installer does not pin it). It runs at most one
//! `llama-server` child (llama.cpp, found on `PATH` or named in the
//! settings, never bundled), on loopback, with one registered model file:
//! loaded when a request first names it, unloaded after the idle time,
//! refused when its memory estimate is over the budget. Requests come
//! through the service's seam with its door key; there is no other way in.
//!
//! - [`supervisor`]: the child's life: load, idle unload, crashes and their
//!   backoff, stopping it, never two at once;
//! - [`guard`]: the process between the service and the child, so the child
//!   never outlives the service;
//! - [`runtime`]: finding `llama-server` and checking it is new enough;
//! - [`complete`]: one structured-output completion, with no tools;
//! - [`http`]: the loopback HTTP client the service talks to its child with;
//! - [`seam`], [`daemon`], [`key`]: the service itself.
//!
//! Nothing here downloads anything: the child runs with `--offline`, and a
//! model is a file the owner placed and registered
//! (`inference_registry`).

pub mod complete;
pub mod daemon;
pub mod guard;
pub mod http;
pub mod key;
pub mod runtime;
pub mod seam;
pub mod supervisor;

/// The service's name: its seam row, its declaration, its data dir.
pub const SERVICE: &str = inference_registry::SERVICE;
