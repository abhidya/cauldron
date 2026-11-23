//! **`_ctx`: what the caller's side knows and the service cannot.**
//!
//! The service is a loopback HTTP process. It cannot know the working
//! directory, the session, or the environment variables of the program that
//! calls it. The stdio bridge Claude Code launches (`cauldron mcp stdio`) does
//! know them, so it adds one object to every `tools/call`'s `arguments`:
//!
//! ```json
//! "_ctx": {
//!   "client": "claude-code",
//!   "cwd": "/abs/path/of/the/session",
//!   "session_id": "the-session-id",
//!   "cauldron_var": "kb"
//! }
//! ```
//!
//! | key | required | what it is |
//! |---|---|---|
//! | `client` | yes | who is calling: `[A-Za-z0-9._-]`, 1 to 64 characters. The bridge sends `claude-code`; a forwarded `cauldron soup import` sends `cli`. |
//! | `cwd` | no | the caller's working directory, an absolute path. Used only to pick a cauldron by the grants' `projects` (longest prefix, canonical paths). |
//! | `session_id` | no | the caller's session: `[A-Za-z0-9._-]`, 1 to 128 characters. Written into the audit line and cited on every `soup_put` record as `{kind: "session", id}`. |
//! | `cauldron_var` | no | the value of `$CAULDRON_CAULDRON` in the bridge's own environment, when it is set and not empty. |
//!
//! No other key is accepted: an unknown key refuses the call (`bad_ctx`),
//! so a misspelt key is never silently ignored. `tools/list` never shows
//! `_ctx`; it is the bridge's, not the model's.
//!
//! **The cauldron a call is about** is, in this order: the call's own
//! `cauldron` argument; else `cauldron_var`; else the grant whose project
//! contains `cwd`. The project marker file (`.cauldron`) is never read
//! here: a marker cannot open a cauldron, because opening needs a grant.

use std::path::PathBuf;

use serde_json::{Map, Value};
use shapewire::envelope::Refusal;

/// The key the bridge puts in `arguments`.
pub const KEY: &str = "_ctx";

/// The refusal code for a missing or malformed `_ctx`.
pub const BAD_CTX: &str = "bad_ctx";

/// One call's context, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ctx {
    pub client: String,
    pub cwd: Option<PathBuf>,
    pub session_id: Option<String>,
    pub cauldron_var: Option<String>,
}

impl Ctx {
    /// `mcp:<client>[:<session>]`, the `thread` of the spine's `agent`
    /// initiator on every write this call makes.
    pub fn initiator_thread(&self) -> String {
        match &self.session_id {
            Some(s) => format!("mcp:{}:{s}", self.client),
            None => format!("mcp:{}", self.client),
        }
    }
}

fn token_ok(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn bad(msg: impl Into<String>) -> Refusal {
    Refusal::new(
        BAD_CTX,
        format!(
            "{} The `_ctx` object is added to every call by the `cauldron mcp stdio` bridge: \
             {{client, cwd?, session_id?, cauldron_var?}}. Nothing was read or written.",
            msg.into()
        ),
    )
}

/// Take `_ctx` out of `arguments` and check it. The rest of the arguments
/// are returned as the tool's own.
pub fn split(arguments: Value) -> Result<(Ctx, Map<String, Value>), Refusal> {
    let mut args = match arguments {
        Value::Object(m) => m,
        Value::Null => Map::new(),
        _ => return Err(Refusal::new("usage", "`arguments` must be an object. Nothing was read or written.")),
    };
    let Some(raw) = args.remove(KEY) else {
        return Err(bad("This call carries no `_ctx`."));
    };
    let Value::Object(mut m) = raw else {
        return Err(bad("`_ctx` is not an object."));
    };
    let mut text = |k: &str| -> Result<Option<String>, Refusal> {
        match m.remove(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(bad(format!("`_ctx.{k}` is not a string."))),
        }
    };
    let client = text("client")?.ok_or_else(|| bad("`_ctx.client` is missing."))?;
    if !token_ok(&client, 64) {
        return Err(bad("`_ctx.client` must be 1 to 64 characters of [A-Za-z0-9._-]."));
    }
    let cwd = match text("cwd")? {
        None => None,
        Some(c) => {
            let p = PathBuf::from(&c);
            if !p.is_absolute() {
                return Err(bad("`_ctx.cwd` must be an absolute path."));
            }
            Some(p)
        }
    };
    let session_id = text("session_id")?.filter(|s| !s.is_empty());
    if let Some(s) = &session_id {
        if !token_ok(s, 128) {
            return Err(bad("`_ctx.session_id` must be 1 to 128 characters of [A-Za-z0-9._-]."));
        }
    }
    let cauldron_var = text("cauldron_var")?.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(k) = m.keys().next() {
        return Err(bad(format!("`_ctx.{k}` is not a key this door knows.")));
    }
    Ok((Ctx { client, cwd, session_id, cauldron_var }, args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_well_formed_ctx_is_taken_out_of_the_arguments() {
        let (c, rest) = split(json!({"ids": ["T-1"], "_ctx": {"client": "claude-code", "cwd": "/tmp/p", "session_id": "s-1", "cauldron_var": " kb "}})).unwrap();
        assert_eq!(c.client, "claude-code");
        assert_eq!(c.cwd.as_deref(), Some(std::path::Path::new("/tmp/p")));
        assert_eq!(c.cauldron_var.as_deref(), Some("kb"));
        assert_eq!(c.initiator_thread(), "mcp:claude-code:s-1");
        assert_eq!(Value::Object(rest), json!({"ids": ["T-1"]}));
    }

    #[test]
    fn every_malformed_ctx_refuses_as_bad_ctx() {
        for bad in [
            json!({}),
            json!({"_ctx": "x"}),
            json!({"_ctx": {}}),
            json!({"_ctx": {"client": ""}}),
            json!({"_ctx": {"client": "a b"}}),
            json!({"_ctx": {"client": "c", "cwd": "relative"}}),
            json!({"_ctx": {"client": "c", "session_id": "../x"}}),
            json!({"_ctx": {"client": "c", "cwd": 3}}),
            json!({"_ctx": {"client": "c", "session": "s"}}),
        ] {
            let e = split(bad.clone()).unwrap_err();
            assert_eq!(e.code, BAD_CTX, "{bad}");
        }
        assert_eq!(split(json!([1])).unwrap_err().code, "usage");
    }
}
