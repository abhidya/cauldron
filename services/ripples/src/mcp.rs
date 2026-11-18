//! **The client of the `mcp` service's ripple seam.** Every read of a dump
//! or the soup, and every write, goes through it: this service never opens a
//! cauldron.
//!
//! The seam is the `mcp` row of the environment's seam table (it must be
//! live), or `$CAULDRON_RIPPLES_MCP_SEAM` (`ip:port`); either must be a
//! loopback address (`inference_registry::loopback_addr`), checked at the
//! service's start and at every call before anything is dialled. Every line carries
//! the ripples key, read for each call from `<data root>/mcp/ripples.secret`
//! (the `mcp` service mints it at every start). One line out, one JSON line
//! back. The dial goes through `env::egress::connect_tcp`.

use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

/// Why a seam call did not answer `ok`.
#[derive(Debug, Clone)]
pub struct Refused {
    pub code: String,
    pub message: String,
    pub detail: Value,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

pub struct Mcp {
    env: env::Environment,
    /// `host:port` of the seam when not the seam table's (from
    /// `$CAULDRON_RIPPLES_MCP_SEAM`, or set by a caller).
    pub seam: Option<String>,
    /// Long enough for a put to be judged and landed.
    pub timeout: Duration,
}

impl Mcp {
    pub fn new(env: &env::Environment) -> Mcp {
        Mcp {
            env: env.clone(),
            seam: std::env::var("CAULDRON_RIPPLES_MCP_SEAM").ok().filter(|v| !v.trim().is_empty()),
            timeout: Duration::from_secs(600),
        }
    }

    /// [`Mcp::new`], refused at the service's start when
    /// `$CAULDRON_RIPPLES_MCP_SEAM` names an address that is not loopback.
    pub fn from_env(env: &env::Environment) -> anyhow::Result<Mcp> {
        let m = Mcp::new(env);
        if let Some(a) = &m.seam {
            inference_registry::loopback_addr("CAULDRON_RIPPLES_MCP_SEAM", a).map_err(|e| anyhow::anyhow!(e))?;
        }
        Ok(m)
    }

    fn key_path(&self) -> PathBuf {
        self.env.data_root.join("mcp").join("ripples.secret")
    }

    fn seam(&self) -> Result<SocketAddr, Refused> {
        let down = |m: String| Refused { code: "mcp_down".into(), message: m, detail: Value::Null };
        let addr = match self.seam.clone() {
            Some(a) => a,
            None => {
                let t = env::seams::Table::load(&self.env.name, &self.env.data_root, &self.env.seams())
                    .map_err(|e| down(format!("cannot read the seam table: {e:#}")))?;
                let Some(row) = t.seams.get("mcp") else {
                    return Err(down(format!("the `mcp` service is not running in environment `{}`; start it with `cauldron --env {} start mcp`", self.env.name, self.env.name)));
                };
                inference_registry::loopback_addr("the mcp seam (seam table)", &row.addr).map_err(down)?;
                let v = env::seams::probe(row);
                if v.state != env::seams::SeamState::Live {
                    return Err(down(format!("the `mcp` service is not reachable ({}: {})", v.state.as_str(), v.detail)));
                }
                row.addr.clone()
            }
        };
        inference_registry::loopback_addr("the mcp seam", &addr).map_err(down)
    }

    /// One verb; the answer's JSON when it is `ok`.
    pub fn ask(&self, verb: &str, args: &Value) -> Result<Value, Refused> {
        let down = |m: String| Refused { code: "mcp_down".into(), message: m, detail: Value::Null };
        let sa = self.seam()?;
        let key = std::fs::read_to_string(self.key_path())
            .map_err(|e| down(format!("cannot read the ripples key {} (the `mcp` service mints it when it starts): {e}", self.key_path().display())))?;
        let s = env::egress::connect_tcp("mcp seam (ripples)", &sa, Duration::from_secs(5)).map_err(|e| down(format!("cannot reach the mcp seam at {sa}: {e}")))?;
        let _ = s.set_read_timeout(Some(self.timeout));
        let mut w = s.try_clone().map_err(|e| down(e.to_string()))?;
        w.write_all(format!("{verb} {} {args}\n", key.trim()).as_bytes()).and_then(|_| w.flush()).map_err(|e| down(format!("writing to the mcp seam: {e}")))?;
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).map_err(|e| down(format!("reading the mcp seam: {e}")))?;
        if line.trim().is_empty() {
            return Err(down(format!("the mcp service closed the connection without answering `{verb}` (did it stop?)")));
        }
        let v: Value = serde_json::from_str(line.trim()).map_err(|e| down(format!("the mcp seam answered something that is not JSON: {e}")))?;
        if v["ok"] == Value::Bool(true) {
            Ok(v)
        } else {
            Err(Refused {
                code: v["code"].as_str().unwrap_or("refused").to_string(),
                message: v["message"].as_str().unwrap_or("").to_string(),
                detail: v.get("detail").cloned().unwrap_or(Value::Null),
            })
        }
    }
}
