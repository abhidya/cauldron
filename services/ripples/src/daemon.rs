//! The service's life: mint the run key, build the engine, register the
//! seam, and run until the process is stopped. A run the stop cuts short is
//! left in the journal without its `done` line, and reads as `interrupted`;
//! a model call it left running is cancelled at the next start
//! (`engine::settle`).

use std::time::Duration;

use anyhow::{Context, Result};
use env::seams::ServiceEnv;

use crate::engine::{Caps, Ctx};
use crate::journal::Journal;
use crate::mcp::Mcp;
use crate::seam::{RipplesSeam, State};

/// The environment the service serves (`<root>/data/<env>` is its data
/// root).
pub fn environment(se: &ServiceEnv) -> Result<env::Environment> {
    let root = se.data_root.parent().and_then(|d| d.parent()).context("the data root has no cauldron root above it")?.to_path_buf();
    Ok(env::Environment {
        root,
        root_source: env::paths::Source::CauldronHome,
        name: se.env.clone(),
        name_source: env::paths::Source::CauldronEnv,
        data_root: se.data_root.clone(),
    })
}

/// Start, and serve until the process is stopped.
pub fn run(se: ServiceEnv) -> Result<()> {
    let env = environment(&se)?;
    let dir = crate::dir(&env);
    shapewire::warnings::COUNTER.attach_sink(&se.data_dir.join("warnings"));
    let key = crate::key::mint(&dir)?;
    let backoff = Duration::from_millis(std::env::var("CAULDRON_RIPPLES_BACKOFF_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(5000));
    let ctx = Ctx {
        inference: crate::inference::from_env(&env)?,
        mcp: Mcp::new(&env),
        journal: Journal::new(&dir),
        caps: Caps::default(),
        scratch_root: std::env::temp_dir(),
        backoff,
        env,
    };
    // A call a kill left running is cancelled before anything else runs.
    for s in crate::engine::settle(&ctx) {
        tracing::info!(run = %s["run"], confirmed = %s["cancel"]["confirmed"], "a call left by an earlier run was cancelled");
    }
    let state = State::new(ctx, key);
    let started_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs().to_string();
    let attested = env::warnings::attest_warnings!(RipplesSeam);
    let reg = shapewire::Registration::from_env();
    let se2 = se.clone();
    let (_seam, addr) = shapewire::seam::start(&se, &attested, &reg, move |a| RipplesSeam { se: se2, addr: a, started_at, state })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    tracing::info!(seam = %addr, env = %se.env, "cauldron-ripples listening");
    loop {
        std::thread::park();
    }
}
