//! **One completion**: the request (`COMPLETE`) and its answer.
//!
//! The request is `{model?, system?, user, schema?, max_output_tokens?,
//! timeout_s?, temperature?, request_id?}` and nothing else: a key this
//! module does not know (`tools`, `tool_choice`, `functions`, …) is refused,
//! so no tool ever reaches the model. `model` is a registered name (absent:
//! the only one registered).
//!
//! It goes to the child as ONE `POST /v1/chat/completions`: a system and a
//! user message, `max_tokens`, `stream: false`, and, with a schema,
//! `response_format: {type: "json_schema", json_schema: {name, strict: true,
//! schema}}`. llama-server compiles that schema into a grammar and samples
//! only tokens the grammar allows, so the answer is held to the schema's
//! shape as it is generated (keywords its converter does not support are
//! left loose; the caller validates the answer itself in any case).
//!
//! The answer is `{ok, request_id, model, sha256, output (the parsed JSON),
//! finish_reason, usage {prompt_tokens, completion_tokens}, timings
//! {prompt_ms, predicted_ms, prompt_per_second, predicted_per_second},
//! tokens_per_second, wall_ms, load_ms?}`. Text that is not JSON is refused
//! `not_json`, with its tokens.

use serde_json::{json, Map, Value};

use crate::supervisor::{Endpoint, Refusal};

/// The keys a request may carry.
pub const KEYS: &[&str] = &["model", "system", "user", "schema", "max_output_tokens", "timeout_s", "temperature", "request_id"];

pub const DEFAULT_MAX_OUTPUT: u32 = 4096;
pub const MAX_OUTPUT: u32 = 65_536;
pub const DEFAULT_TIMEOUT_S: u64 = 900;
pub const MAX_TIMEOUT_S: u64 = 3600;

#[derive(Debug, Clone)]
pub struct Ask {
    pub model: String,
    pub system: String,
    pub user: String,
    pub schema: Option<Value>,
    pub max_output_tokens: u32,
    pub timeout_s: u64,
    pub temperature: Option<f64>,
    pub request_id: String,
}

fn usage(m: impl Into<String>) -> Refusal {
    Refusal::new("usage", m)
}

/// A plain id: letters, digits, `.`, `_`, `:`, `-`, at most 128.
pub fn plain(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// Read a request; `default_model` names the model when it names none.
pub fn parse(a: &Map<String, Value>, default_model: impl FnOnce() -> Result<String, Refusal>) -> Result<Ask, Refusal> {
    if let Some(k) = a.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(usage(format!("`{k}` is not a key of a completion request ({}). This service never offers a model tools.", KEYS.join(", "))));
    }
    let s = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_string);
    let Some(user) = s("user").filter(|u| !u.is_empty()) else {
        return Err(usage("`user` (the text to complete) is required"));
    };
    let model = match s("model") {
        Some(m) if inference_registry::name_ok(&m) => m,
        Some(m) => return Err(usage(format!("`{m}` is not a model name"))),
        None => default_model()?,
    };
    let schema = match a.get("schema") {
        None | Some(Value::Null) => None,
        Some(v @ Value::Object(_)) => Some(v.clone()),
        Some(_) => return Err(usage("`schema` is a JSON schema object")),
    };
    let max_output_tokens = match a.get("max_output_tokens") {
        None | Some(Value::Null) => DEFAULT_MAX_OUTPUT,
        Some(v) => match v.as_u64().filter(|n| (1..=MAX_OUTPUT as u64).contains(n)) {
            Some(n) => n as u32,
            None => return Err(usage(format!("`max_output_tokens` is 1 to {MAX_OUTPUT}"))),
        },
    };
    let timeout_s = match a.get("timeout_s") {
        None | Some(Value::Null) => DEFAULT_TIMEOUT_S,
        Some(v) => match v.as_u64().filter(|n| (1..=MAX_TIMEOUT_S).contains(n)) {
            Some(n) => n,
            None => return Err(usage(format!("`timeout_s` is 1 to {MAX_TIMEOUT_S}"))),
        },
    };
    let temperature = match a.get("temperature") {
        None | Some(Value::Null) => None,
        Some(v) => match v.as_f64().filter(|t| (0.0..=2.0).contains(t)) {
            Some(t) => Some(t),
            None => return Err(usage("`temperature` is 0 to 2")),
        },
    };
    let request_id = match s("request_id") {
        Some(r) if plain(&r) => r,
        Some(r) => return Err(usage(format!("`{r}` is not a plain request id"))),
        None => format!("req-{}", crate::key::random_hex().chars().take(16).collect::<String>()),
    };
    Ok(Ask { model, system: s("system").unwrap_or_default(), user, schema, max_output_tokens, timeout_s, temperature, request_id })
}

/// The body sent to `/v1/chat/completions`. No `tools`, `tool_choice` or
/// `functions` key, ever.
pub fn body(a: &Ask, alias: &str) -> Value {
    let mut messages = vec![];
    if !a.system.is_empty() {
        messages.push(json!({"role": "system", "content": a.system}));
    }
    messages.push(json!({"role": "user", "content": a.user}));
    let mut b = json!({"model": alias, "messages": messages, "max_tokens": a.max_output_tokens, "stream": false, "cache_prompt": true});
    if let Some(s) = &a.schema {
        b["response_format"] = json!({"type": "json_schema", "json_schema": {"name": "answer", "strict": true, "schema": s}});
    }
    if let Some(t) = a.temperature {
        b["temperature"] = json!(t);
    }
    b
}

/// The child's answer to `a`, as this service answers it.
pub fn answer(a: &Ask, ep: &Endpoint, r: crate::http::Response, wall_ms: u64) -> Result<Value, Refusal> {
    let v: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    if r.status != 200 {
        let msg = v["error"]["message"].as_str().map(str::to_string).unwrap_or_else(|| r.text().chars().take(500).collect());
        let low = msg.to_ascii_lowercase();
        if r.status == 503 {
            return Err(Refusal::new("busy", format!("llama-server is not ready ({msg})")));
        }
        if low.contains("context") && (low.contains("exceed") || low.contains("too long") || low.contains("larger")) {
            return Err(Refusal::with("too_long", format!("the prompt does not fit the model's context: {msg}"), json!({"status": r.status})));
        }
        return Err(Refusal::with("llama_error", format!("llama-server answered {}: {msg}", r.status), json!({"status": r.status})));
    }
    let choice = &v["choices"][0];
    let content = choice["message"]["content"].as_str().unwrap_or("").to_string();
    let finish = choice["finish_reason"].clone();
    let usage = json!({"prompt_tokens": v["usage"]["prompt_tokens"].as_u64().unwrap_or(0), "completion_tokens": v["usage"]["completion_tokens"].as_u64().unwrap_or(0)});
    let t = &v["timings"];
    let timings = json!({
        "prompt_ms": t["prompt_ms"], "predicted_ms": t["predicted_ms"],
        "prompt_per_second": t["prompt_per_second"], "predicted_per_second": t["predicted_per_second"],
    });
    let tps = t["predicted_per_second"].as_f64().or_else(|| {
        let n = usage["completion_tokens"].as_u64()? as f64;
        (wall_ms > 0).then(|| n * 1000.0 / wall_ms as f64)
    });
    let mut out = json!({
        "ok": true, "request_id": a.request_id, "model": ep.model.name, "sha256": ep.model.sha256,
        "finish_reason": finish, "usage": usage, "timings": timings, "tokens_per_second": tps, "wall_ms": wall_ms,
    });
    if let Some(ms) = ep.load_ms {
        out["load_ms"] = json!(ms);
    }
    match serde_json::from_str::<Value>(content.trim()) {
        Ok(o) if a.schema.is_none() || o.is_object() || o.is_array() => {
            out["output"] = o;
            Ok(out)
        }
        _ if a.schema.is_none() => {
            out["output"] = json!(content);
            Ok(out)
        }
        _ => {
            let why = if finish == json!("length") { "it stopped at the output limit (finish_reason `length`)" } else { "it is not JSON" };
            out["text"] = json!(content.chars().take(4000).collect::<String>());
            Err(Refusal::with("not_json", format!("the answer does not meet the schema: {why}"), out))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_never_carries_tools() {
        let m = |v: Value| v.as_object().unwrap().clone();
        let d = || Ok("qwen".to_string());
        let e = parse(&m(json!({"user": "hi", "tools": [{"type": "function"}]})), d).unwrap_err();
        assert_eq!(e.code, "usage");
        assert!(e.message.contains("never offers a model tools"));
        for k in ["tool_choice", "functions", "messages", "response_format", "grammar"] {
            assert!(parse(&m(json!({"user": "hi", k: 1})), || Ok("q".to_string())).is_err(), "{k}");
        }
        let a = parse(&m(json!({"user": "hi", "system": "be brief", "schema": {"type": "object"}, "max_output_tokens": 100})), || Ok("qwen".to_string())).unwrap();
        assert_eq!((a.model.as_str(), a.max_output_tokens, a.timeout_s), ("qwen", 100, DEFAULT_TIMEOUT_S));
        let b = body(&a, "qwen");
        let keys: Vec<&String> = b.as_object().unwrap().keys().collect();
        assert!(!keys.iter().any(|k| k.contains("tool") || k.contains("function")), "{keys:?}");
        assert_eq!(b["response_format"]["type"], "json_schema");
        assert_eq!(b["response_format"]["json_schema"]["schema"], json!({"type": "object"}));
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["content"], "hi");
        assert_eq!(b["stream"], false);
        assert!(parse(&m(json!({"user": "hi", "max_output_tokens": 0})), || Ok("q".to_string())).is_err());
        assert!(parse(&m(json!({"user": ""})), || Ok("q".to_string())).is_err());
        assert!(parse(&m(json!({"user": "x", "model": "Bad Name"})), || Ok("q".to_string())).is_err());
    }
}
