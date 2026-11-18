//! **The `local` backend**: a model call to this environment's `inference`
//! service (`cauldron-inference`), which runs a registered model file in a
//! `llama-server` child on this machine. Nothing leaves the machine: the
//! prompt goes over loopback to the service and from it to its child, and
//! the answer comes back the same way.
//!
//! A model id `local:<name>` names a registered model
//! (`cauldron inference models add`). The call is one `COMPLETE` on the
//! service's seam, carrying its door key (`data/<env>/inference/door.secret`,
//! read for each call): a short system line, the ripple's prompt as the user
//! text, the package's schema (loosened where llama-server's grammar
//! converter is weakest, see [`grammar_schema`]; the ripple validates the
//! answer against the full schema itself, whatever the backend did), the
//! output bound and the call's time limit. No tools.
//!
//! The service answers with the model file's sha256, the token counts and
//! the timings; the backend reports them to the run's journal as a `local`
//! line (model, sha256, prompt and completion tokens, tokens per second,
//! load and generation time). Usage is recorded with `cost_usd: 0`. A call
//! given up is cancelled with `CANCEL`, which disconnects llama-server and
//! so stops its generation.
//!
//! The seam is the `inference` row of the environment's seam table (it
//! must be live), or, in tests only, `$CAULDRON_RIPPLES_INFERENCE_SEAM`
//! (refused outside an isolated process). Either way it must be a loopback
//! address ([`inference_registry::loopback_addr`]), checked at the
//! service's start and again at every call, before anything is dialled;
//! the dial goes through `env::egress::connect_tcp`.

use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

use crate::inference::{Failure, Inference, Reply, Request, Usage};

/// The model-id prefix this backend answers.
pub const PREFIX: &str = "local:";

/// The system line sent with every call.
pub const SYSTEM: &str = "You answer with one JSON object that meets the schema you were given, and nothing else.";

pub struct Local {
    env: env::Environment,
    /// `host:port` of the seam when not the seam table's.
    pub seam: Option<String>,
    /// The time limit of one call.
    pub timeout_s: u64,
}

/// The variable that names the inference seam for tests.
pub const SEAM_ENV: &str = "CAULDRON_RIPPLES_INFERENCE_SEAM";

impl Local {
    /// A backend that finds the seam in the environment's seam table.
    pub fn new(env: &env::Environment) -> Local {
        Local {
            env: env.clone(),
            seam: None,
            timeout_s: std::env::var("CAULDRON_RIPPLES_CALL_TIMEOUT_S").ok().and_then(|v| v.parse().ok()).filter(|n| *n > 0).unwrap_or(900).min(3600),
        }
    }

    /// **The backend this process uses**: [`Local::new`], with
    /// `$CAULDRON_RIPPLES_INFERENCE_SEAM` when it is set, which is for tests:
    /// refused outside an isolated process, and refused unless it is a
    /// loopback address ([`check_override`]). A refusal stops the service
    /// at start.
    pub fn from_env(env: &env::Environment) -> anyhow::Result<Local> {
        let mut l = Local::new(env);
        l.seam = check_override(std::env::var(SEAM_ENV).ok(), env::egress::process_isolation().is_some()).map_err(|m| anyhow::anyhow!(m))?;
        Ok(l)
    }

    fn key_path(&self) -> PathBuf {
        inference_registry::dir(&self.env.data_root).join("door.secret")
    }

    fn start_hint(&self) -> String {
        let e = &self.env.name;
        format!("start it with `cauldron --env {e} start inference` (after `build inference` and `pin inference <version>`)")
    }

    /// The seam's address: loopback, or refused before anything is dialled
    /// (the probe included).
    fn seam(&self) -> Result<SocketAddr, String> {
        let addr = match self.seam.clone() {
            Some(a) => a,
            None => {
                let t = env::seams::Table::load(&self.env.name, &self.env.data_root, &self.env.seams()).map_err(|e| format!("cannot read the seam table: {e:#}"))?;
                let Some(row) = t.seams.get(inference_registry::SERVICE) else {
                    return Err(format!("the `inference` service is not running in environment `{}`; {}", self.env.name, self.start_hint()));
                };
                inference_registry::loopback_addr("the inference seam (seam table)", &row.addr)?;
                let v = env::seams::probe(row);
                if v.state != env::seams::SeamState::Live {
                    return Err(format!("the `inference` service is not reachable ({}: {}); {}", v.state.as_str(), v.detail, self.start_hint()));
                }
                row.addr.clone()
            }
        };
        inference_registry::loopback_addr("the inference seam", &addr)
    }

    /// **How long a completion is waited for**: the service's longest load,
    /// plus the call's own time limit, plus a minute. The service may load
    /// the model first (up to [`inference_registry::LOAD_TIMEOUT_S`]) and
    /// then generate for up to the call's limit; waiting less would give up
    /// on a call the service is still running.
    pub fn completion_wait(&self) -> Duration {
        Duration::from_secs(inference_registry::LOAD_TIMEOUT_S + self.timeout_s + 60)
    }

    /// One verb; the answer's JSON (an `ok: false` answer included), or why
    /// the service could not be asked: before the line was sent
    /// ([`Asked::NotSent`]), or after ([`Asked::Lost`]).
    fn ask(&self, verb: &str, args: &Value, timeout: Duration) -> Result<Value, Asked> {
        let sa = self.seam().map_err(Asked::NotSent)?;
        let key = std::fs::read_to_string(self.key_path())
            .map_err(|e| Asked::NotSent(format!("cannot read the inference door key {} (the `inference` service mints it when it starts; {}): {e}", self.key_path().display(), self.start_hint())))?;
        let s = env::egress::connect_tcp("inference seam (ripples)", &sa, Duration::from_secs(5)).map_err(|e| Asked::NotSent(format!("cannot reach the inference seam at {sa}: {e}")))?;
        let _ = s.set_read_timeout(Some(timeout));
        let mut w = s.try_clone().map_err(|e| Asked::NotSent(e.to_string()))?;
        w.write_all(format!("{verb} {} {args}\n", key.trim()).as_bytes()).and_then(|_| w.flush()).map_err(|e| Asked::NotSent(format!("writing to the inference seam: {e}")))?;
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).map_err(|e| match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => Asked::Lost(format!("the inference service did not answer `{verb}` within {} s", timeout.as_secs())),
            _ => Asked::Lost(format!("reading the inference seam: {e}")),
        })?;
        if line.trim().is_empty() {
            return Err(Asked::Lost(format!("the inference service closed the connection without answering `{verb}` (did it stop?)")));
        }
        serde_json::from_str(line.trim()).map_err(|e| Asked::Lost(format!("the inference seam answered something that is not JSON: {e}")))
    }
}

/// Why a verb got no answer.
#[derive(Debug)]
enum Asked {
    /// Nothing reached the service.
    NotSent(String),
    /// The line was sent; its answer was lost (a time-out, a closed
    /// connection): the service may still be running it.
    Lost(String),
}

/// **The test override of the seam**, checked: unset (or blank) is none;
/// set, it must come from an isolated process and be a loopback address.
pub fn check_override(v: Option<String>, isolated: bool) -> Result<Option<String>, String> {
    let Some(v) = v.filter(|v| !v.trim().is_empty()) else { return Ok(None) };
    if !isolated {
        return Err(format!("{SEAM_ENV} is for tests: it is refused outside an isolated process (the local backend finds the inference service in the seam table)"));
    }
    inference_registry::loopback_addr(SEAM_ENV, &v)?;
    Ok(Some(v.trim().to_string()))
}

/// **The schema as sent for the grammar**: the package's schema with the
/// parts llama-server's JSON-schema-to-grammar converter does not express
/// (or expands into a very large grammar) rewritten or loosened:
///
/// - `allOf` with `if`/`then` (the state each type takes), which the
///   converter ignores, becomes an `anyOf` of one object per state, each
///   with that state and the types that take it, so the grammar itself
///   pairs a type with its state. (The keys reach llama-server in
///   alphabetical order, so a model writes `state` before `type`; with one
///   `enum` of every state, a local model paired them wrongly in about a
///   third of its answers.) An `allOf` whose pairs cannot be read becomes
///   one `enum` of every state, as before;
/// - a `maxLength` of 1000 or more is dropped.
///
/// The answer is still validated against the full schema by the ripple.
pub fn grammar_schema(s: &Value) -> Value {
    fn walk(v: &mut Value) {
        match v {
            Value::Object(m) => {
                let mut variants: Option<Vec<Value>> = None;
                if let Some(Value::Array(all)) = m.get("allOf") {
                    let mut states: Vec<Value> = vec![];
                    // (state, the types that take it), in the schema's order.
                    let mut pairs: Vec<(Value, Vec<Value>)> = vec![];
                    let mut readable = true;
                    let conditional = all.iter().all(|x| x.get("if").is_some());
                    if conditional {
                        for x in all {
                            let state = x.pointer("/then/properties/state/const");
                            let ty = x.pointer("/if/properties/type/const");
                            if let Some(c) = state {
                                if !states.contains(c) {
                                    states.push(c.clone());
                                }
                            }
                            match (state, ty) {
                                (Some(st), Some(t)) => match pairs.iter_mut().find(|(s, _)| s == st) {
                                    Some((_, ts)) => ts.push(t.clone()),
                                    None => pairs.push((st.clone(), vec![t.clone()])),
                                },
                                _ => readable = false,
                            }
                        }
                        m.remove("allOf");
                        let has_type = m.get("properties").and_then(|p| p.get("type")).is_some();
                        if readable && has_type && !pairs.is_empty() {
                            let base = Value::Object(m.clone());
                            variants = Some(
                                pairs
                                    .into_iter()
                                    .map(|(st, ts)| {
                                        let mut o = base.clone();
                                        o["properties"]["state"] = json!({"const": st});
                                        o["properties"]["type"] = if ts.len() == 1 { json!({"const": ts[0]}) } else { json!({"enum": ts}) };
                                        o
                                    })
                                    .collect(),
                            );
                        } else if !states.is_empty() {
                            if let Some(p) = m.get_mut("properties").and_then(|p| p.as_object_mut()) {
                                p.insert("state".into(), json!({"enum": states}));
                            }
                        }
                    }
                }
                if let Some(mut vs) = variants {
                    vs.iter_mut().for_each(walk);
                    *v = json!({"anyOf": vs});
                    return;
                }
                if m.get("maxLength").and_then(Value::as_u64).is_some_and(|n| n >= 1000) {
                    m.remove("maxLength");
                }
                m.remove("$schema");
                for (_, x) in m.iter_mut() {
                    walk(x);
                }
            }
            Value::Array(a) => a.iter_mut().for_each(walk),
            _ => {}
        }
    }
    let mut out = s.clone();
    walk(&mut out);
    out
}

impl Inference for Local {
    fn name(&self) -> &'static str {
        "local"
    }

    fn call(&self, r: &Request) -> Result<Reply, Failure> {
        let Some(name) = r.model.strip_prefix(PREFIX).filter(|n| inference_registry::name_ok(n)) else {
            return Err(Failure::Failed { message: format!("`{}` is not a local model id (`local:<registered name>`)", r.model), usage: Usage::default() });
        };
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let request_id = format!("ripple-{}-{nanos}", &r.name[..r.name.len().min(12)]);
        let args = json!({
            "model": name, "system": SYSTEM, "user": r.prompt, "schema": grammar_schema(&r.schema),
            "max_output_tokens": r.max_output_tokens, "timeout_s": self.timeout_s, "request_id": request_id,
        });
        r.note("sent", json!({"backend": "local", "model": r.model, "request_id": request_id}));
        let v = match self.ask("COMPLETE", &args, self.completion_wait()) {
            Ok(v) => v,
            Err(Asked::NotSent(m)) => return Err(Failure::Unavailable(m)),
            // Sent, and no answer: the service may still be generating, so
            // the call is cancelled (and charged, as any call given up is).
            Err(Asked::Lost(m)) => {
                let c = self.cancel(&json!({"request_id": request_id}));
                r.note("cancel", json!({"backend": "local", "request_id": request_id, "confirmed": c["confirmed"], "detail": c["detail"], "how_to_stop": c.get("how_to_stop")}));
                return Err(Failure::Failed { message: format!("the local model (`{}`) was given up: {m}; the call was cancelled", r.model), usage: Usage::default() });
            }
        };
        let usage_of = |u: &Value| Usage {
            input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
            output_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
            cost_usd: Some(0.0),
            ..Usage::default()
        };
        if v["ok"] == json!(true) {
            let usage = usage_of(&v["usage"]);
            r.note(
                "local",
                json!({
                    "backend": "local", "model": r.model, "sha256": v["sha256"], "request_id": request_id,
                    "prompt_tokens": usage.input_tokens, "completion_tokens": usage.output_tokens,
                    "tokens_per_second": v["tokens_per_second"], "prompt_ms": v["timings"]["prompt_ms"], "predicted_ms": v["timings"]["predicted_ms"],
                    "load_ms": v["load_ms"], "wall_ms": v["wall_ms"], "finish_reason": v["finish_reason"], "cost_usd": 0.0,
                }),
            );
            return Ok(Reply { output: v["output"].clone(), usage, model: r.model.clone() });
        }
        let code = v["code"].as_str().unwrap_or("failed");
        let message = format!("the local model (`{}`) was not run to an answer: {} ({code})", r.model, v["message"].as_str().unwrap_or(""));
        match code {
            "busy" => Err(Failure::Busy(message)),
            "not_json" => {
                let usage = usage_of(&v["detail"]["usage"]);
                r.note("local", json!({"backend": "local", "model": r.model, "sha256": v["detail"]["sha256"], "request_id": request_id, "prompt_tokens": usage.input_tokens, "completion_tokens": usage.output_tokens, "tokens_per_second": v["detail"]["tokens_per_second"], "failure": code, "cost_usd": 0.0}));
                Err(Failure::SchemaUnmet { message, usage, model: r.model.clone() })
            }
            "timeout" | "cancelled" => {
                r.note("cancel", json!({"backend": "local", "request_id": request_id, "confirmed": true, "detail": "the inference service disconnected llama-server, which stops its generation"}));
                Err(Failure::Failed { message, usage: Usage::default() })
            }
            "unauthorized" => Err(Failure::Unavailable(format!("{message}; the inference service restarted while this call read its key: run the same command again"))),
            _ => Err(Failure::Failed { message, usage: Usage::default() }),
        }
    }

    fn cancel(&self, sent: &Value) -> Value {
        let Some(id) = sent["request_id"].as_str() else {
            return json!({"confirmed": true, "detail": "nothing of this call is left to stop"});
        };
        match self.ask("CANCEL", &json!({"request_id": id}), Duration::from_secs(15)) {
            Ok(v) if v["ok"] == json!(true) => json!({"confirmed": true, "request_id": id, "detail": v["detail"]}),
            Ok(v) => json!({"confirmed": false, "request_id": id, "detail": v["message"], "how_to_stop": "run `cauldron inference unload`, or stop the inference service"}),
            // A service that is not running has no child, so nothing of the call runs.
            Err(Asked::NotSent(m)) => json!({"confirmed": true, "request_id": id, "detail": format!("the inference service could not be asked ({m}); without it no llama-server runs")}),
            Err(Asked::Lost(m)) => json!({"confirmed": false, "request_id": id, "detail": m, "how_to_stop": "run `cauldron inference unload`, or stop the inference service"}),
        }
    }
}

/// **The backend a process uses**: the default one (threads, or scripted in
/// tests) for every model id, and [`Local`] for `local:<name>`.
pub struct Routed {
    pub default: Box<dyn Inference>,
    pub local: Box<dyn Inference>,
}

impl Inference for Routed {
    fn name(&self) -> &'static str {
        self.default.name()
    }

    fn name_for(&self, model: &str) -> &'static str {
        if model.starts_with(PREFIX) {
            self.local.name()
        } else {
            self.default.name_for(model)
        }
    }

    fn call(&self, r: &Request) -> Result<Reply, Failure> {
        if r.model.starts_with(PREFIX) {
            self.local.call(r)
        } else {
            self.default.call(r)
        }
    }

    fn cancel(&self, sent: &Value) -> Value {
        if sent["backend"] == "local" {
            self.local.cancel(sent)
        } else {
            self.default.cancel(sent)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_completion_is_waited_for_through_a_whole_load_and_its_own_limit() {
        let e = env::Environment {
            root: "/nonexistent".into(),
            root_source: env::paths::Source::CauldronHome,
            name: "dev".into(),
            name_source: env::paths::Source::CauldronEnv,
            data_root: "/nonexistent/data/dev".into(),
        };
        let mut l = Local::new(&e);
        l.timeout_s = 900;
        assert_eq!(l.completion_wait().as_secs(), inference_registry::LOAD_TIMEOUT_S + 900 + 60);
        assert!(l.completion_wait().as_secs() > inference_registry::LOAD_TIMEOUT_S + l.timeout_s, "never shorter than what the service may take");
    }

    #[test]
    fn the_seam_override_is_for_isolated_tests_and_loopback_only() {
        assert_eq!(check_override(None, false), Ok(None));
        assert_eq!(check_override(Some(" ".into()), false), Ok(None));
        assert!(check_override(Some("127.0.0.1:9".into()), false).unwrap_err().contains("is for tests"));
        assert_eq!(check_override(Some("127.0.0.1:9".into()), true), Ok(Some("127.0.0.1:9".into())));
        for bad in ["192.0.2.7:443", "0.0.0.0:9", "example.invalid:443"] {
            let e = check_override(Some(bad.into()), true).unwrap_err();
            assert!(e.contains(SEAM_ENV) && e.contains("nothing was sent"), "{bad}: {e}");
        }
    }

    #[test]
    fn the_grammar_schema_is_the_package_schema_loosened_only_where_said() {
        let full = crate::package::schema();
        let g = grammar_schema(&full);
        let variants = g["properties"]["records"]["items"]["anyOf"].as_array().expect("one object per state");
        let pairs: Vec<(Value, Value)> = variants.iter().map(|o| (o["properties"]["state"].clone(), o["properties"]["type"].clone())).collect();
        assert_eq!(
            pairs,
            vec![
                (json!({"const": "idea"}), json!({"const": "idea"})),
                (json!({"const": "open"}), json!({"enum": ["issue", "question"]})),
                (json!({"const": "decided"}), json!({"const": "decision"})),
                (json!({"const": "standing"}), json!({"const": "constraint"})),
                (json!({"const": "ready"}), json!({"const": "task"})),
                (json!({"const": "lurking"}), json!({"const": "danger"})),
            ]
        );
        for o in variants {
            assert!(o.get("allOf").is_none());
            assert!(o["properties"]["body"].get("maxLength").is_none());
            assert_eq!(o["required"], full["properties"]["records"]["items"]["required"]);
            assert_eq!(o["additionalProperties"], false);
        }
        let item = &variants[0];
        // Every pairing the grammar admits is one the full schema accepts,
        // and a mismatched pair it refuses is refused by the full schema too.
        for o in variants {
            let st = o["properties"]["state"]["const"].clone();
            let types: Vec<Value> = o["properties"]["type"].get("enum").and_then(Value::as_array).cloned().unwrap_or_else(|| vec![o["properties"]["type"]["const"].clone()]);
            for t in types {
                let a = json!({"records": [{"label": "n1", "type": t, "state": st, "handle": "h", "regions": ["R-1"]}]});
                assert!(crate::package::validate(&a).is_ok(), "{a}");
            }
        }
        assert!(crate::package::validate(&json!({"records": [{"label": "n1", "type": "danger", "state": "ready", "handle": "h", "regions": ["R-1"]}]})).is_err());
        // An `allOf` whose pairs cannot be read is loosened as before.
        let odd = grammar_schema(&json!({"type": "object", "properties": {"type": {"enum": ["a"]}, "state": {"type": "string"}}, "allOf": [{"if": {"properties": {"kind": {"const": "a"}}}, "then": {"properties": {"state": {"const": "s"}}}}]}));
        assert!(odd.get("allOf").is_none() && odd.get("anyOf").is_none());
        assert_eq!(odd["properties"]["state"], json!({"enum": ["s"]}));
        assert_eq!(item["properties"]["handle"]["maxLength"], 235);
        assert_eq!(g["properties"]["records"]["maxItems"], 25);
        assert_eq!(item["properties"]["label"], full["properties"]["records"]["items"]["properties"]["label"]);
        assert!(g.get("$schema").is_none());
        assert_eq!(g["additionalProperties"], false);
    }
}
