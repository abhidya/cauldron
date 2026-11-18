//! **The service's cauldron seam** (the `CAULDRON_BIND` line protocol, on
//! loopback), registered with the warnings attestation as every cauldron
//! service does. Its verbs, each answering one JSON line:
//!
//! - `STATUS <key> {}`: the state (`idle`, `loading`, `ready`, or `unloading`
//!   while an unload waits for its child to exit), the loaded
//!   model (name, sha256, context, memory estimate, child pid, port, when it
//!   was loaded, how long it has been idle and when it unloads), the
//!   runtime (path, version, build), the settings, the registered names,
//!   the request in flight, crashes and the backoff, the last unload;
//! - `COMPLETE <key> {model?, system?, user, schema?, max_output_tokens?,
//!   timeout_s?, temperature?, request_id?}`: one completion
//!   ([`crate::complete`]), loading the model first if needed; the line
//!   is answered when the completion is. One at a time: another meanwhile
//!   is answered `busy`;
//! - `CANCEL <key> {request_id}`: stop that request if it is in flight;
//! - `UNLOAD <key> {}`: unload the model now;
//! - `PING`.
//!
//! Every verb but `PING` answers only a line carrying the door key
//! ([`crate::key`]). A refusal is `{"ok": false, code, message, detail?}`.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{bail, Result};
use env::seams::ServiceEnv;
use env::warnings::{Conn, Export, Service, ServesWarnings, Verb};
use serde_json::{json, Map, Value};

use crate::key::Key;
use crate::supervisor::{Refusal, Supervisor};

/// What the verbs share.
pub struct State {
    pub sup: Arc<Supervisor>,
    pub key: Key,
}

impl State {
    fn args(&self, rest: &str) -> Result<Map<String, Value>, Value> {
        let (given, body) = rest.split_once(' ').unwrap_or((rest, ""));
        if given.is_empty() || !self.key.is(given) {
            return Err(Refusal::new("unauthorized", "this verb answers only a line carrying the inference door key (door.secret)").json());
        }
        let body = body.trim();
        if body.is_empty() {
            return Ok(Map::new());
        }
        match serde_json::from_str::<Value>(body) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Err(Refusal::new("usage", "one JSON object follows the key").json()),
        }
    }

    pub fn status(&self, rest: &str) -> Value {
        match self.args(rest) {
            Ok(_) => self.sup.status(),
            Err(v) => v,
        }
    }

    pub fn complete(&self, rest: &str) -> Value {
        let a = match self.args(rest) {
            Ok(a) => a,
            Err(v) => return v,
        };
        let data_root = self.sup.data_root.clone();
        let ask = match crate::complete::parse(&a, || {
            let r = inference_registry::Registry::load(&data_root).map_err(|e| Refusal::new("settings", format!("{e:#}")))?;
            r.default_model().map(|m| m.name.clone()).map_err(|e| Refusal::new("unknown_model", format!("{e:#}")))
        }) {
            Ok(x) => x,
            Err(r) => return r.json(),
        };
        match self.sup.complete(&ask) {
            Ok(v) => v,
            Err(r) => {
                let mut v = r.json();
                v["request_id"] = json!(ask.request_id);
                v
            }
        }
    }

    pub fn cancel(&self, rest: &str) -> Value {
        let a = match self.args(rest) {
            Ok(a) => a,
            Err(v) => return v,
        };
        match a.get("request_id").and_then(Value::as_str) {
            Some(id) => self.sup.cancel(id),
            None => Refusal::new("usage", "CANCEL needs `request_id`").json(),
        }
    }

    pub fn unload(&self, rest: &str) -> Value {
        if let Err(v) = self.args(rest) {
            return v;
        }
        match self.sup.unload("asked") {
            Some(v) => json!({"ok": true, "unloaded": v}),
            None => json!({"ok": true, "unloaded": null, "detail": "no model was loaded"}),
        }
    }
}

/// One running instance, as cauldron's line protocol sees it.
pub struct InferenceSeam {
    pub se: ServiceEnv,
    pub addr: SocketAddr,
    pub started_at: String,
    pub state: Arc<State>,
}

impl ServesWarnings for InferenceSeam {
    const SHAPE: &'static str = env::warnings::SHAPE;

    fn warnings(&self, asked: &str) -> Result<Export> {
        if !asked.is_empty() && asked != Self::SHAPE {
            bail!("this service serves the `{}` shape, not `{asked}`", Self::SHAPE);
        }
        Ok(shapewire::warnings::COUNTER.export(&self.se.service, Self::SHAPE, &self.started_at))
    }
}

impl Service for InferenceSeam {
    const VERBS: &'static [Verb<InferenceSeam>] = &[
        Verb::new("STATUS", InferenceSeam::v_status),
        Verb::new("COMPLETE", InferenceSeam::v_complete),
        Verb::new("CANCEL", InferenceSeam::v_cancel),
        Verb::new("UNLOAD", InferenceSeam::v_unload),
        Verb::new("PING", InferenceSeam::v_ping),
    ];

    fn service_env(&self) -> &ServiceEnv {
        &self.se
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl InferenceSeam {
    fn v_status(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        c.line(&self.state.status(rest).to_string())?;
        Ok(())
    }

    fn v_complete(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        c.line(&self.state.complete(rest).to_string())?;
        Ok(())
    }

    fn v_cancel(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        c.line(&self.state.cancel(rest).to_string())?;
        Ok(())
    }

    fn v_unload(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        c.line(&self.state.unload(rest).to_string())?;
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
