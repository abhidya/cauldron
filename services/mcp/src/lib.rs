//! **cauldron-mcp**: the MCP door to an environment's soups.
//!
//! A supervised service (`services/mcp.toml`), one per environment. It
//! listens on `127.0.0.1` only and answers JSON-RPC on `POST /mcp` through
//! shapewire's MCP carrier. A cauldron is closed to it until the owner
//! grants it at a terminal (`cauldron mcp grant`); the grants are checked
//! on every call. Reads go through the soup's read-only connections; this
//! service is the one writer of every granted soup it holds open.
//!
//! - [`http`]: the loopback listener, the secret, `Host` and `Origin`;
//! - [`door`]: every call, from `_ctx` to the answer;
//! - [`ctx`]: the per-call context the stdio bridge injects;
//! - [`catalogue`]: the tools;
//! - [`changes`]: `soup_changes`, what landed since a cursor;
//! - [`writer`]: one writer thread per open cauldron;
//! - [`access`], [`secret`], [`audit`], [`cursor`], [`seam`], [`daemon`].

pub mod access;
pub mod audit;
pub mod catalogue;
pub mod changes;
pub mod ctx;
pub mod cursor;
pub mod daemon;
pub mod door;
pub mod http;
pub mod seam;
pub mod secret;
pub mod writer;

use shapewire::envelope::Refusal;

/// A soup-door error as a refusal: its stable code and its message.
pub fn refusal(e: soup_door::Error) -> Refusal {
    Refusal::new(e.code.as_str(), e.message)
}

/// The service's directory: `<data root>/mcp`, where the grants, the door
/// secret and the audit log live.
pub fn dir(e: &env::Environment) -> std::path::PathBuf {
    soup_door::grants::store_dir(e)
}
