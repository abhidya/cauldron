//! **cauldron-ripples**: one named dump into knowledge-graph records, when
//! its owner asks.
//!
//! A supervised service (`services/ripples.toml`), one per environment, off
//! unless started. In this release it acts only on `cauldron ripples run
//! <DM-n>`: it reads the dump and the cauldron's regions through the `mcp`
//! service's ripple seam, makes one structured model call through an
//! [`inference::Inference`] backend (the `threads` service in production),
//! and hands the record batch back to the `mcp` service, which lands it under
//! a ripple's provenance, unreviewed, and marks the dump `processed`.
//!
//! - [`engine`]: one run, step by step, with its caps and retries;
//! - [`inference`]: the model-call trait and its `scripted` backend (tests);
//! - [`threads`]: the `threads` backend;
//! - [`local`]: the `local` backend (the `inference` service on this
//!   machine), and the router that sends `local:<name>` model ids to it;
//! - [`eval`]: backends side by side on fixture dumps, nothing written;
//! - [`mcp`]: the client of the `mcp` service's ripple seam;
//! - [`package`]: the dump package (`package/dump/v1`: one prompt file and the
//!   batch's JSON schema);
//! - [`journal`]: the run journal (`runs.jsonl`);
//! - [`scrub`]: the secret scrubber, on what is sent and on the answer;
//! - [`auto`]: automatic processing, for a cauldron whose owner turned it
//!   on (`cauldron ripples on`);
//! - [`seam`], [`daemon`], [`key`]: the service itself.

pub mod auto;
pub mod daemon;
pub mod engine;
pub mod eval;
pub mod inference;
pub mod journal;
pub mod key;
pub mod local;
pub mod mcp;
pub mod package;
pub mod regions;
pub mod scrub;
pub mod seam;
pub mod threads;

/// The service's name: its seam row, its declaration, its data dir.
pub const SERVICE: &str = "ripples";

/// The service's directory under an environment's data root.
pub fn dir(e: &env::Environment) -> std::path::PathBuf {
    e.data_root.join(SERVICE)
}
