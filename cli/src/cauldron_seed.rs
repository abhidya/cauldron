//! **`cauldron create --seed` IS REFUSED IN THIS RELEASE.** The `--seed`
//! hook `cauldrons::create` calls. Seeding a cauldron with the curated demo
//! needs a rework of the demo crate that is not in 0.1.x (here the demo is
//! an environment, `env create cauldron-build`), so:
//!
//! 1. `parse_seed` refuses every value, before anything is written.
//! 2. `seed_created` is a no-op: no cauldron made here asks for a seed.

use anyhow::{bail, Result};

/// The flag `create` accepts for the demo.
pub(crate) const SEED_FLAG: &str = "--seed";

/// Every `--seed` value is refused in this release; nothing is created.
pub(crate) fn parse_seed(v: &str) -> Result<String> {
    bail!("`{SEED_FLAG} {v}`: seeding a cauldron is not in this release; nothing was created")
}

/// A no-op: nothing created in this release asks for a seed.
pub(crate) fn seed_created(_c: &::env::Cauldron) -> Result<Option<::soup::demo::Installed>> {
    Ok(None)
}
