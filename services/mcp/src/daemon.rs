//! The service's life: bind, mint, serve, register, and stop.

use std::time::Duration;

use anyhow::{Context, Result};
use env::seams::ServiceEnv;
use shapewire::envelope::Identity;

use crate::door::{Door, Tools};
use crate::{http, secret};

/// How often the grants are looked at between calls, so a revoke frees a
/// writer lock even when no call follows it.
pub const GRANT_POLL: Duration = Duration::from_millis(200);

/// The environment the service serves, built from what `cauldron start`
/// handed it (`<root>/data/<env>` is its data root).
pub fn environment(se: &ServiceEnv) -> Result<env::Environment> {
    let root = se
        .data_root
        .parent()
        .and_then(|d| d.parent())
        .context("the data root has no cauldron root above it")?
        .to_path_buf();
    Ok(env::Environment {
        root,
        root_source: env::paths::Source::CauldronHome,
        name: se.env.clone(),
        name_source: env::paths::Source::CauldronEnv,
        data_root: se.data_root.clone(),
    })
}

/// Run until SIGINT or SIGTERM.
///
/// THE ORDER: the door's port is bound first, so a second instance fails
/// before it touches anything; the secret is minted before the door
/// answers, so no request is ever served without one; the seam is
/// registered last, so its row never names a door nobody can reach.
pub async fn run(se: ServiceEnv) -> Result<()> {
    let started_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs().to_string();
    let env = environment(&se)?;
    shapewire::warnings::COUNTER.attach_sink(&se.data_dir.join("warnings"));
    let std_listener = http::bind(&se.env)?;
    let addr = std_listener.local_addr()?;
    let dir = crate::dir(&env);
    let secret = secret::mint(&dir)?;
    let ripples = secret::mint_named(&dir, secret::RIPPLES_FILE)?;
    let approve = secret::mint_named(&dir, secret::APPROVE_FILE)?;
    let door = Door::new(env, secret, se.version.clone(), &dir);

    {
        let door = door.clone();
        std::thread::Builder::new().name("grants-poll".into()).spawn(move || loop {
            std::thread::sleep(GRANT_POLL);
            door.refresh();
        })?;
    }

    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(std_listener)?;
    let identity = Identity {
        daemon: "cauldron-mcp".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        proto: 1,
        boot_id: format!("{}-{started_at}", std::process::id()),
    };
    let app = http::router(Tools(door.clone()), identity, addr);
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(http::serve(listener, app, async move {
        let _ = stop_rx.await;
    }));

    let attested = env::warnings::attest_warnings!(crate::seam::McpSeam);
    let reg = shapewire::Registration::from_env();
    let se2 = se.clone();
    let st = started_at.clone();
    let state = door.clone();
    let (_seam, seam_addr) = shapewire::seam::start(&se, &attested, &reg, move |a| crate::seam::McpSeam {
        se: se2,
        addr: a,
        door: addr,
        started_at: st,
        state,
        ripples,
        approve,
    })
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    tracing::info!(door = %addr, seam = %seam_addr, env = %se.env, "cauldron-mcp listening on http://{addr}/mcp");

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let signal = tokio::select! {
        _ = tokio::signal::ctrl_c() => "SIGINT",
        _ = term.recv() => "SIGTERM",
    };
    tracing::info!(signal, "cauldron-mcp stopping");
    let _ = stop_tx.send(());
    let closing = door.clone();
    tokio::task::spawn_blocking(move || closing.shutdown()).await?;
    let _ = tokio::time::timeout(Duration::from_secs(10), server).await;
    Ok(())
}
