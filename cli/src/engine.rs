//! **THE SOUP STORE THAT `cauldron create` LAYS.** This release has no
//! engine: no events, soup or ripples service is declared, pinned or
//! launched. A new cauldron still gets its soup's store on disk (two pools:
//! `ws`, on grammar v6, and `cauldron`, on `cauldron@1`, which holds the
//! clusters), because that is the store `cauldron soup import | regions | get
//! | nodes | search | region | claim` read and write offline.

use std::path::Path;

use anyhow::{Context, Result};

/// The soup's one pool.
pub(crate) const SOUP_POOL: &str = "ws";

/// **THE SOUP'S CAULDRON**: the pool `ws` on grammar v6 and the pool
/// `cauldron` on `cauldron@1`. A cauldron made before 0.1.16 has only `ws`,
/// and gets `cauldron` on its first cluster write
/// (`soup_door::clusters::ensure_pool`). `true` when it
/// created it. An existing directory is never touched (the soup refuses it
/// loudly at boot if it is not a cauldron).
pub(crate) fn ensure_soup_cauldron(dir: &Path) -> Result<bool> {
    if dir.exists() {
        return Ok(false);
    }
    if let Some(p) = dir.parent() {
        if !p.exists() {
            if let Some(pp) = p.parent() {
                std::fs::create_dir_all(pp).with_context(|| format!("creating {}", pp.display()))?;
            }
            // The soup's directory is its owner's only (`env::private`).
            env::private::make_dir(p).with_context(|| format!("creating {}", p.display()))?;
        }
    }
    env::private::make_dir(dir).with_context(|| format!("creating {}", dir.display()))?;
    let c = spine::Cauldron::create(
        dir,
        &[spine::grammar_v6::workspace_v6(SOUP_POOL), spine::grammar_cauldron::cauldron_v1(spine::grammar_cauldron::POOL)],
        spine::config::SpineConfig::default(),
    )
        .map_err(|e| anyhow::anyhow!("creating the soup's cauldron at {}: {e}", dir.display()))?;
    drop(c);
    env::private::tighten_tree(dir).with_context(|| format!("making {} private to its owner", dir.display()))?;
    Ok(true)
}
