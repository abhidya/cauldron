//! The service's cauldron seam: the line protocol on `$CAULDRON_BIND`,
//! registered through shapewire's seam with the warnings attestation, as
//! every cauldron service does. Its own verbs:
//!
//! - `MCP` answers where the door is (`http://127.0.0.1:<port>/mcp`): how a
//!   client in an environment other than `prod` finds a door whose port the
//!   kernel chose;
//! - `OPEN` answers, as one JSON line, the cauldrons this service holds open
//!   for writing (`[{cauldron, opened_at, recovered_at_open}]`), for
//!   `cauldron mcp status`. Names and times only, never soup content;
//! - `DUMPS` answers the same for the dumps stores it holds.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{bail, Result};
use env::seams::ServiceEnv;
use env::warnings::{Conn, Export, Service, ServesWarnings, Verb};

/// One running instance, as cauldron's line protocol sees it.
pub struct McpSeam {
    pub se: ServiceEnv,
    pub addr: SocketAddr,
    /// Where `POST /mcp` listens.
    pub door: SocketAddr,
    /// This process's start, the warnings' counting window.
    pub started_at: String,
    /// The door, for `OPEN`.
    pub state: Arc<crate::door::Door>,
}

impl ServesWarnings for McpSeam {
    const SHAPE: &'static str = env::warnings::SHAPE;

    fn warnings(&self, asked: &str) -> Result<Export> {
        if !asked.is_empty() && asked != Self::SHAPE {
            bail!("this service serves the `{}` shape, not `{asked}`", Self::SHAPE);
        }
        Ok(shapewire::warnings::COUNTER.export(&self.se.service, Self::SHAPE, &self.started_at))
    }
}

impl Service for McpSeam {
    const VERBS: &'static [Verb<McpSeam>] = &[
        Verb::new("MCP", McpSeam::v_mcp),
        Verb::new("OPEN", McpSeam::v_open),
        Verb::new("DUMPS", McpSeam::v_dumps),
        Verb::new("PING", McpSeam::v_ping),
    ];

    fn service_env(&self) -> &ServiceEnv {
        &self.se
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl McpSeam {
    fn v_mcp(&self, c: &mut Conn<'_>, _rest: &str) -> Result<()> {
        c.line(&format!("http://{}{}", self.door, shapewire::mcp::http::PATH))?;
        Ok(())
    }

    fn v_open(&self, c: &mut Conn<'_>, _rest: &str) -> Result<()> {
        c.line(&serde_json::to_string(&self.state.writers.snapshot())?)?;
        Ok(())
    }

    fn v_dumps(&self, c: &mut Conn<'_>, _rest: &str) -> Result<()> {
        c.line(&serde_json::to_string(&self.state.dumps.snapshot())?)?;
        Ok(())
    }

    fn v_ping(&self, c: &mut Conn<'_>, _rest: &str) -> Result<()> {
        c.line("PONG")?;
        Ok(())
    }
}

/// The `tracing` layer that counts every WARN and ERROR by call site.
pub struct WarningsLayer;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for WarningsLayer {
    fn on_event(&self, e: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        shapewire::warnings::COUNTER.record_event(e);
    }
}
