//! **The change cursor**, opaque to callers: `v1.<soup instance>.<counter>`.
//!
//! - `<soup instance>` is the first 12 hex characters of SHA-256 over the
//!   cauldron's name, a NUL, and its `cauldron.toml` `created_at`, so a
//!   cauldron removed and created again under the same name has a different
//!   instance and an old cursor is recognisably someone else's.
//! - `<counter>` is the spine's committed-commit counter: it moves exactly
//!   when a write lands.
//!
//! `soup_status`, `soup_put` and `soup_link` answer it; `soup_changes`
//! reads what landed since one ([`crate::changes`]).

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

/// The cursor for `counter`.
pub fn cursor(c: &env::Cauldron, counter: u64) -> String {
    format!("v1.{}.{counter}", instance(c))
}

/// The committed-commit counter of the soup at `soup`, read through a
/// connection that cannot write.
pub fn counter_offline(soup: &Path) -> Result<u64, String> {
    let conn = spine::offline::read_only_connection(&soup.join(spine::spine_db::SPINE_FILE)).map_err(|e| e.to_string())?;
    conn.query_row("SELECT committed_commits FROM counters WHERE singleton = 1", [], |r| r.get::<_, i64>(0))
        .map(|n| n.max(0) as u64)
        .map_err(|e| e.to_string())
}
