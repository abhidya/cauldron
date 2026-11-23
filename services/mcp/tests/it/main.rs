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
//! - `dumps`: the dump tools, the `dump` scope, the store beside the soup,
//!   a session transcript stored verbatim, a revoke or a kill while a
//!   transcript streams leaving no byte behind (and, under
//!   `--features fault-injection`, a dump killed before its commit, its
//!   blob swept at the next open).
//! - `review`: records a ripple wrote, marked `unreviewed` in every read,
//!   filtered by `review`, approved only through the owner's `APPROVE` verb.
//! - `ripple`: the seam verbs only the ripples service may call: the key,
//!   provenance the batch cannot change, the stage's write limits, a run
//!   that lands once (and, under `--features fault-injection`, a kill
//!   between the landing and the dump's mark).
//! - `regions`: region locks: a region a ripple holds refuses every other
//!   writer at once, by name, and every other region takes writes; two
//!   holders of disjoint regions both land; the revision check; expiry and
//!   renewal; a held child region and its parent; (under `--features
//!   fault-injection`) a kill between the landing and the release.
//! - `edges`: `soup_unlink` and `soup_cite`, correcting a record after the
//!   fact (issue #24).
//! - `interface`: the consumer's interface table (issue #10), one test per
//!   row, each tool called exactly as the table writes it.
//! - `issues`: the consumer's filed issues, each reproduction as the door
//!   answers it (the CLI's twins are the `i<n>_` tests of `cli/tests/it/soup.rs`).
//! - `writes`: the writer: concurrency, `partial`, refusals named by id,
//!   cites, `soup_link`, the duplicate guard, the audit log, revoke, and a
//!   kill mid-write recovered at the next open (and, under
//!   `--features fault-injection`, a write stopped after its decision,
//!   landed whole at the next open).

// Every child a test spawns is isolated.
test_isolation::install!();

mod support;

mod build_stamp;
mod changes;
mod clusters;
mod door;
mod dumps;
mod edges;
mod interface;
mod issues;
mod limits;
mod reads;
mod regions;
mod review;
mod ripple;
mod writes;
