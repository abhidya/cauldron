//! The service's life: take the environment's ripples directory for this
//! process alone ([`hold`]), mint the run key, build the engine, register
//! the seam, and run until the process is stopped. A run the stop cuts short is
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

/// The lock file in the environment's ripples directory.
pub const LOCK_FILE: &str = "service.lock";

/// **One ripples service per environment.** An exclusive lock on
/// `<data root>/ripples/service.lock`, held for the life of the process,
/// with its pid written in it. A second service started on the same data
/// root (two `start`s, a copy run by hand) refuses to start and names the
/// first's pid: two would run every automatic dump twice (both within the
/// caps, but twice the calls). The lock goes with the process, however it
/// ends.
pub fn hold(dir: &std::path::Path) -> Result<std::fs::File> {
    use std::io::{Read, Seek, Write};
    use std::os::unix::fs::OpenOptionsExt;
    env::private::make_dir(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(LOCK_FILE);
    let mut f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(env::private::FILE_MODE).open(&path).with_context(|| format!("opening {}", path.display()))?;
    if f.try_lock().is_err() {
        let mut held = String::new();
        let _ = f.read_to_string(&mut held);
        let pid = held.trim();
        anyhow::bail!(
            "REFUSED: another ripples service (pid {}) already serves this environment's data ({}); two would run every automatic dump twice. Nothing was started. Stop that one first (`cauldron --env <env> stop ripples`).",
            if pid.is_empty() { "unknown" } else { pid },
            dir.display()
        );
    }
    f.set_len(0)?;
    f.seek(std::io::SeekFrom::Start(0))?;
    f.write_all(std::process::id().to_string().as_bytes())?;
    f.sync_all()?;
    Ok(f)
}

/// Start, and serve until the process is stopped.
pub fn run(se: ServiceEnv) -> Result<()> {
    let env = environment(&se)?;
    let dir = crate::dir(&env);
    // Before anything is written: a second service changes nothing.
    let _held = hold(&dir)?;
    shapewire::warnings::COUNTER.attach_sink(&se.data_dir.join("warnings"));
    let key = crate::key::mint(&dir)?;
    let backoff = Duration::from_millis(std::env::var("CAULDRON_RIPPLES_BACKOFF_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(5000));
    let ctx = Ctx {
        inference: crate::inference::from_env(&env)?,
        mcp: Mcp::from_env(&env)?,
        journal: Journal::new(&dir),
        caps: Caps::default(),
        scratch_root: std::env::temp_dir(),
        backoff,
        lock_ttl_secs: crate::regions::ttl_secs(),
        hold_budget_secs: crate::regions::hold_budget_secs(),
        env,
    };
    // A call a kill left running is cancelled before anything else runs.
    for s in crate::engine::settle(&ctx) {
        tracing::info!(run = %s["run"], confirmed = %s["cancel"]["confirmed"], "a call left by an earlier run was cancelled");
    }
    let state = State::new(ctx, key);
    // Automatic processing: a scan now, then on every word and every period.
    crate::auto::start(state.clone())?;
    let started_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs().to_string();
    let attested = env::warnings::attest_warnings!(RipplesSeam);
    let reg = shapewire::Registration::from_env();
    let se2 = se.clone();
    let (_seam, addr) = shapewire::seam::start(&se, &attested, &reg, move |a| RipplesSeam { se: se2, addr: a, started_at, state })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // This seam takes the run key and starts paid calls: loopback only.
    if !addr.ip().is_loopback() {
        anyhow::bail!("the ripples service listens on loopback only; CAULDRON_BIND `{}` gave {addr}", se.bind);
    }
    tracing::info!(seam = %addr, env = %se.env, "cauldron-ripples listening");
    loop {
        std::thread::park();
    }
}
