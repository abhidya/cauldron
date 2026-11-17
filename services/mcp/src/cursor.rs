//! **The change cursor**, opaque to callers:
//! `v1.<soup instance>.<counter>.<dumps counter>`.
//!
//! - `<soup instance>` is the first 12 hex characters of SHA-256 over the
//!   cauldron's name, a NUL, and its `cauldron.toml` `created_at`, so a
//!   cauldron removed and created again under the same name has a different
//!   instance and an old cursor is recognisably someone else's.
//! - `<counter>` is the soup spine's committed-commit counter: it moves
//!   exactly when a write lands.
//! - `<dumps counter>` is the same counter of the cauldron's dumps store
//!   (`soup_door::dumps`), 0 while it has none. A cursor written before
//!   dumps existed has no fourth part, and is read as 0 there.
//!
//! `soup_status`, `soup_put`, `soup_link` and the dump tools answer it;
//! `soup_changes` reads what landed since one ([`crate::changes`]).

use std::path::Path;

use sha2::{Digest, Sha256};

/// The instance part for a cauldron.
pub fn instance(c: &env::Cauldron) -> String {
    let created = c.meta().ok().and_then(|m| m.created_at).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(c.name.as_bytes());
    h.update([0u8]);
    h.update(created.as_bytes());
    h.finalize().iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// The cursor for the soup's `counter` and the dumps store's `dumps`.
pub fn cursor(c: &env::Cauldron, counter: u64, dumps: u64) -> String {
    format!("v1.{}.{counter}.{dumps}", instance(c))
}

/// The cursor as `c` stands now, both counters read through connections
/// that cannot write.
pub fn now(c: &env::Cauldron) -> Result<String, String> {
    let soup = counter_offline(&c.soup())?;
    let dumps = soup_door::dumps::counter(c).map_err(|e| e.to_string())?;
    Ok(cursor(c, soup, dumps))
}

/// The committed-commit counter of the soup at `soup`, read through a
/// connection that cannot write.
pub fn counter_offline(soup: &Path) -> Result<u64, String> {
    let conn = spine::offline::read_only_connection(&soup.join(spine::spine_db::SPINE_FILE)).map_err(|e| e.to_string())?;
    conn.query_row("SELECT committed_commits FROM counters WHERE singleton = 1", [], |r| r.get::<_, i64>(0))
        .map(|n| n.max(0) as u64)
        .map_err(|e| e.to_string())
}
