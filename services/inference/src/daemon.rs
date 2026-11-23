//! The service's life: stop a child a previous start left, mint the door
//! key, check the runtime (a missing or old `llama-server` is reported in
//! `STATUS` and refused at each load, not at start), register the seam,
//! unload an idle model every tick, and on TERM, INT or HUP unload (the
//! child is stopped, its guard's grace then a kill) before exiting.

use std::time::Duration;

use anyhow::{Context, Result};
use env::seams::ServiceEnv;

use crate::seam::{InferenceSeam, State};
use crate::supervisor::Supervisor;

/// Start, and serve until the process is stopped.
pub fn run(se: ServiceEnv) -> Result<()> {
    // Loopback only: the seam, like the child.
    let host = se.bind.rsplit_once(':').map(|(h, _)| h.trim_matches(['[', ']'])).unwrap_or("");
    if !matches!(host, "127.0.0.1" | "::1" | "localhost") {
        anyhow::bail!("the inference service listens on loopback only; CAULDRON_BIND is `{}`", se.bind);
    }
    let dir = inference_registry::dir(&se.data_root);
    shapewire::warnings::COUNTER.attach_sink(&se.data_dir.join("warnings"));
    let exe = std::env::current_exe().context("finding this binary (the child's guard)")?;
    let sup = Supervisor::new(&se.data_root, &exe);
    if let Some(v) = sup.reap_leftover() {
        tracing::warn!(left = %v, "a llama-server child of an earlier start was found and stopped");
    }
    let key = crate::key::mint(&dir)?;
    match sup.check_runtime() {
        Ok(r) => tracing::info!(path = %r.path.display(), build = r.build, "llama-server checked"),
        Err(m) => tracing::warn!(problem = %m, "llama-server is not usable; every load is refused until it is"),
    }
    // Stop the child before this process ends, on any of these.
    {
        let sup = sup.clone();
        let mut signals = signal_hook::iterator::Signals::new([signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT, signal_hook::consts::SIGHUP])?;
        std::thread::Builder::new().name("inference-signals".into()).spawn(move || {
            if let Some(sig) = signals.forever().next() {
                tracing::info!(sig, "stopping: unloading first");
                sup.unload("the service is stopping");
                std::process::exit(0);
            }
        })?;
    }
    // The idle timer.
    {
        let sup = sup.clone();
        std::thread::Builder::new().name("inference-idle".into()).spawn(move || loop {
            let tick = inference_registry::Config::load(&sup.data_root).map(|c| Duration::from_millis((c.idle_seconds * 1000 / 4).clamp(100, 1000))).unwrap_or(Duration::from_secs(1));
            std::thread::sleep(tick);
            sup.idle_tick();
        })?;
    }
    let state = std::sync::Arc::new(State { sup, key });
    let started_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs().to_string();
    let attested = env::warnings::attest_warnings!(InferenceSeam);
    let reg = shapewire::Registration::from_env();
    let se2 = se.clone();
    let (_seam, addr) = shapewire::seam::start(&se, &attested, &reg, move |a| InferenceSeam { se: se2, addr: a, started_at, state })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // `localhost` is accepted above; what it resolved to must be loopback.
    if !addr.ip().is_loopback() {
        anyhow::bail!("the inference service listens on loopback only; it was bound to {addr}");
    }
    tracing::info!(seam = %addr, env = %se.env, "cauldron-inference listening");
    loop {
        std::thread::park();
    }
}
