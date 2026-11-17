//! The one integration-test binary for `cauldron-mcp`. Every test starts the
//! real binary under its own scratch root (see `support`).
//!
//! - `door`: the listener and its guards (secret, `Host`, `Origin`,
//!   loopback only), the catalogue, the seam, default-closed.
//! - `limits`: what is refused from a request's head before its body is
//!   read, the connection cap, the in-flight body budget, the timeouts.
//! - `reads`: the read tools, the grant checked on every call, the
//!   whole-reply budget, resolution by project.
//! - `changes`: `soup_changes`, its cursor and its refusals.
//! - `writes`: the writer: concurrency, `partial`, refusals named by id,
//!   cites, `soup_link`, the duplicate guard, the audit log, revoke, and a
//!   kill mid-write recovered at the next open (and, under
//!   `--features fault-injection`, a write stopped after its decision,
//!   landed whole at the next open).

// Every child a test spawns is isolated.
test_isolation::install!();

mod support;

mod changes;
mod door;
mod limits;
mod reads;
mod writes;
