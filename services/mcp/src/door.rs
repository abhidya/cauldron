//! **The door**: every `tools/call`, from the context to the answer.
//!
//! The checks, in order, first failure answers (the HTTP layer has already
//! checked the secret, `Host` and `Origin`):
//!
//! 1. the tool exists (else the protocol's `-32602`), and is built in this
//!    release (`not_available_yet`, `unsupported`);
//! 2. `_ctx` is well formed (`bad_ctx`, see [`crate::ctx`]);
//! 3. the grants are re-read if they changed, and any cauldron this door
//!    holds open whose grant no longer allows `write` is closed;
//! 4. the cauldron is resolved: the `cauldron` argument, else
//!    `_ctx.cauldron_var`, else the grant whose project holds `_ctx.cwd`
//!    (`no_cauldron_for_project`, `ambiguous_project`);
//! 5. a grant exists for it and holds the tool's scope (`not_granted`), the
//!    cauldron still exists (`cauldron_gone`) and has a soup (`no_soup`);
//! 6. the tool's own arguments (`usage`).
//!
//! Nothing is read before step 5 passes. Reads run on a blocking thread
//! through connections that cannot write; writes are queued to the
//! cauldron's writer ([`crate::writer`]).

use std::sync::Arc;

use serde_json::{json, Map, Value};
use shapewire::envelope::{codes, Refusal};
use shapewire::mcp::{Caller, McpTools, ToolDef};
use soup_door::budget;
use soup_door::grants::{Grant, Scope};
use soup_door::reads::{self, ReadOpts};
use soup_door::Target;

use crate::access::Access;
use crate::audit::{Audit, Line};
use crate::catalogue::{self, Needs, Tool};
use crate::ctx::{self, Ctx};
use crate::secret::Secret;
use crate::writer::{self, Who, Writers};
use crate::refusal;

/// The schema every soup speaks.
pub const SCHEMA: &str = "workspace@6";

/// One environment's door.
pub struct Door {
    pub env: env::Environment,
    secret: Secret,
    access: Arc<Access>,
    pub writers: Writers,
    audit: Arc<Audit>,
    /// The installed version label (`$CAULDRON_VERSION`).
    label: String,
}

impl Door {
    /// `dir` is `<data root>/mcp`: the grants, the secret and the audit log.
    pub fn new(env: env::Environment, secret: Secret, label: String, dir: &std::path::Path) -> Arc<Door> {
        let audit = Arc::new(Audit::new(dir));
        let access = Arc::new(Access::new(&env));
        Arc::new(Door { writers: Writers::new(audit.clone(), access.clone()), access, env, secret, audit, label })
    }

    /// Does this `Authorization` header value carry the door secret?
    pub fn admits(&self, authorization: Option<&str>) -> bool {
        self.secret.admits(authorization)
    }

    /// Re-read the grants if they changed, and close every open cauldron
    /// whose grant no longer allows `write`. Called on every call and by the
    /// service's poller, so a revoke frees the writer lock even when no
    /// call follows it. The open cauldrons are checked every time, not only
    /// when the store changed: a call authorized just before a revoke may
    /// open its cauldron just after it, and the next look closes it again.
    pub fn refresh(&self) {
        for name in self.writers.names() {
            let why = match self.env.find_cauldron(&name) {
                Ok(Some(c)) => self.access.why_not_writable(&c),
                _ => Some("the cauldron no longer exists"),
            };
            if let Some(r) = why {
                self.writers.close(&name, r);
            }
        }
    }

    /// Stop: close every open cauldron (each lock is free when this returns).
    pub fn shutdown(&self) {
        self.writers.close_all("the service is stopping");
    }

    /// Answer one call (blocking: run it on a blocking thread).
    pub fn call(&self, tool: &'static Tool, arguments: Value) -> Result<Value, Refusal> {
        let scope = match tool.needs {
            Needs::Scope(s) => s,
            Needs::NotYet(why) => {
                return Err(Refusal::new(codes::NOT_AVAILABLE_YET, format!("{why}. Nothing was read or written.")))
            }
            Needs::Unsupported => {
                return Err(Refusal::new(
                    codes::UNSUPPORTED,
                    "workspace-v6 has no clusters; use `in_region` (an edge from the node to its region, written with \
                     soup_put or soup_link). Nothing was read or written.",
                ))
            }
        };
        let (ctx, args) = ctx::split(arguments)?;
        let mut target_name = None;
        let out = self.call_scoped(tool, scope, &ctx, args, &mut target_name);
        if scope != Scope::Read {
            self.audit.write(audit_line(tool.name, &ctx, target_name, &out));
        }
        out
    }

    fn call_scoped(&self, tool: &Tool, scope: Scope, ctx: &Ctx, args: Map<String, Value>, named: &mut Option<String>) -> Result<Value, Refusal> {
        let mut a = Args { tool: tool.name, m: args };
        let explicit = a.str("cauldron")?;
        self.refresh();
        let grants = self.access.grants().map_err(refusal)?;
        let res = grants
            .resolve(&self.env.name, ctx.cwd.as_deref(), explicit.as_deref(), ctx.cauldron_var.as_deref())
            .map_err(refusal)?;
        *named = Some(res.cauldron.clone());
        let grant = grants.authorize(&self.env, &res.cauldron, scope).map_err(refusal)?.clone();
        let cauldron = self
            .env
            .find_cauldron(&res.cauldron)
            .ok()
            .flatten()
            .ok_or_else(|| Refusal::new("cauldron_gone", format!("cauldron `{}` no longer exists. Nothing was read or written.", res.cauldron)))?;
        let t = Target { env: self.env.clone(), cauldron, resolved_from: Some(res.from.as_text()) };
        let mut doc = match tool.name {
            "soup_status" => {
                a.done()?;
                self.status(&t, &grant)?
            }
            "soup_regions" => {
                let mc = a.count("max_chars")?;
                a.done()?;
                bounded(reads::regions(&t, &ReadOpts::default()).map_err(refusal)?, mc, &t)
            }
            "soup_region" => {
                let id = a.required_str("id")?;
                let o = ReadOpts { depth: a.count("depth")?, limit: a.count("limit")?, ..ReadOpts::default() };
                let mc = a.count("max_chars")?;
                a.done()?;
                let mut doc = reads::region(&t, &id, &o).map_err(refusal)?;
                doc["clusters"] = json!([]);
                bounded(doc, mc, &t)
            }
            "soup_nodes" => {
                let o = ReadOpts {
                    region: a.str("region")?,
                    node_type: a.str("type")?,
                    state: a.str("state")?,
                    limit: a.count("limit")?,
                    ..ReadOpts::default()
                };
                let mc = a.count("max_chars")?;
                a.done()?;
                bounded(reads::nodes(&t, &o).map_err(refusal)?, mc, &t)
            }
            "soup_search" => {
                let query = a.required_str("query")?;
                let o = ReadOpts { region: a.str("region")?, node_type: a.str("type")?, limit: a.count("limit")?, ..ReadOpts::default() };
                let mc = a.count("max_chars")?;
                a.done()?;
                if reads::search_terms(&query).is_empty() {
                    return Err(usage("soup_search needs a query with at least one word"));
                }
                bounded(reads::search(&t, &query, &o).map_err(refusal)?, mc, &t)
            }
            "soup_get" => {
                let ids = a.strings("ids")?;
                let mc = a.count("max_chars")?;
                a.done()?;
                bounded(reads::get(&t, &ids, &ReadOpts::default()).map_err(refusal)?, mc, &t)
            }
            "soup_changes" => {
                let since = a.str("since")?;
                let limit = a.count("limit")?;
                let mc = a.count("max_chars")?;
                a.done()?;
                let mut doc = bounded(crate::changes::changes(&t, since.as_deref(), limit)?, mc, &t);
                crate::changes::after_budget(&t, &mut doc);
                doc
            }
            "soup_put" => {
                let records = a.objects("records")?;
                let cite = a.take("cite");
                if let Some(c) = &cite {
                    if !c.get("kind").and_then(Value::as_str).is_some_and(|k| !k.is_empty()) {
                        return Err(usage("`cite` must be an object with a `kind` (meta.cites form), e.g. {\"kind\":\"url\",\"url\":\"…\"}"));
                    }
                }
                let put = writer::Put {
                    records,
                    cite,
                    remint: a.flag("remint")?,
                    dry_run: a.flag("dry_run")?,
                    partial: a.flag("partial")?,
                    allow_duplicate: a.flag("allow_duplicate")?,
                };
                a.done()?;
                self.writers.put(&t, who(ctx), put)?
            }
            "soup_link" => {
                let links = a.links()?;
                a.done()?;
                self.writers.link(&t, who(ctx), links)?
            }
            other => return Err(Refusal::new(codes::UNKNOWN_METHOD, other.to_string())),
        };
        if let Some(o) = doc.as_object_mut() {
            o.entry("resolved_from").or_insert_with(|| json!(res.from.as_text()));
        }
        Ok(doc)
    }

    /// `soup_status`.
    fn status(&self, t: &Target, grant: &Grant) -> Result<Value, Refusal> {
        let soup = t.soup();
        let pool = spine::schema_def::PoolName::new(reads::DEFAULT_POOL);
        let store = |e: &dyn std::fmt::Display| Refusal::new("store", format!("reading cauldron `{}`'s soup: {e}", t.cauldron.name));
        let handle = spine::pool::Pool::open_read_only(&spine::pool::Pool::path_in(&soup, &pool)).map_err(|e| store(&e))?;
        let snap = handle.snapshot().map_err(|e| store(&e))?;
        let mut counts = Map::new();
        let mut total = 0usize;
        for ty in handle.grammar().node_types.keys() {
            let n = snap.nodes_of(ty, None).map_err(|e| store(&e))?.len();
            total += n;
            if n > 0 {
                counts.insert(ty.clone(), json!(n));
            }
        }
        let regions = counts.get("region").and_then(Value::as_u64).unwrap_or(0);
        let cursor = crate::cursor::counter_offline(&soup).map(|n| crate::cursor::cursor(&t.cauldron, n)).map_err(|e| store(&e))?;
        let writer = match self.writers.get(&t.cauldron.name) {
            Some(w) => json!({
                "open": true,
                "pid": std::process::id(),
                "opened_at": w.opened_at,
                "recovered_at_open": w.recovered,
            }),
            None => {
                let rest = spine::offline::rest_check(&soup).map_err(|e| store(&e))?;
                json!({
                    "open": false,
                    "at_rest": rest.is_empty(),
                    "not_at_rest": rest,
                    "note": "this door opens a cauldron for writing on its first write-scoped call, and recovers it then",
                })
            }
        };
        Ok(json!({
            "env": t.env.name,
            "cauldron": t.cauldron.name,
            "scopes": grant.scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            "schema": SCHEMA,
            "service": {"name": "cauldron-mcp", "version": env!("CARGO_PKG_VERSION"), "label": self.label},
            "counts": counts,
            "regions": regions,
            "total": total,
            "cursor": cursor,
            "writer": writer,
        }))
    }
}

fn who(ctx: &Ctx) -> Who {
    Who { client: ctx.client.clone(), thread: ctx.initiator_thread(), session: ctx.session_id.clone() }
}

/// A read's answer bounded to `max_chars` as a whole. When items were
/// left out, `next` says how to get them, and the bound still holds with it
/// (it is left off only when the bound is too small to carry it;
/// `truncated.dropped_from` says the same thing in numbers).
fn bounded(mut v: Value, max_chars: Option<usize>, t: &Target) -> Value {
    if let (Some(o), Some(from)) = (v.as_object_mut(), &t.resolved_from) {
        o.insert("resolved_from".into(), json!(from));
    }
    let max = budget::clamp(max_chars);
    let first = budget::budget(v.clone(), max);
    if first["truncated"]["dropped_from"].as_object().is_none_or(|d| d.is_empty()) {
        return first;
    }
    let next = json!(format!(
        "items were left out to fit max_chars: ask again with a larger max_chars (ceiling {}), a smaller limit, or a narrower filter",
        budget::MAX_CHARS_CEILING
    ));
    let reserve = budget::size(&json!({ "next": next }));
    if max < budget::MAX_CHARS_FLOOR + reserve {
        return first;
    }
    let mut b = budget::budget(v, max - reserve);
    b["next"] = next;
    b
}

fn usage(msg: impl Into<String>) -> Refusal {
    Refusal::new("usage", format!("{}. Nothing was read or written.", msg.into()))
}

/// What a write call writes to `calls.jsonl`: never text.
fn audit_line(tool: &str, ctx: &Ctx, cauldron: Option<String>, out: &Result<Value, Refusal>) -> Line {
    let doc = match out {
        Ok(d) => Some(d),
        Err(r) => r.detail.as_ref(),
    };
    let mut counts = Map::new();
    let mut tx = None;
    if let Some(d) = doc {
        for k in ["accepted", "refused"] {
            if let Some(n) = d.get(k).and_then(Value::as_u64) {
                counts.insert(k.into(), json!(n));
            }
        }
        if let Some(rs) = d.get("records").and_then(Value::as_array) {
            counts.insert("records".into(), json!(rs.len()));
        }
        if let Some(ds) = d.get("dropped").and_then(Value::as_array) {
            counts.insert("dropped".into(), json!(ds.len()));
        }
        if let Some(ls) = d.get("links").and_then(Value::as_array) {
            counts.insert("links".into(), json!(ls.len()));
            counts.insert("linked".into(), json!(ls.iter().filter(|l| l["status"] == "linked").count()));
        }
        tx = d.get("landed").and_then(|l| l.get("tx")).and_then(Value::as_str).map(str::to_string);
    }
    let outcome = match out {
        Ok(d) => d.get("outcome").and_then(Value::as_str).unwrap_or("ok").to_string(),
        Err(r) => r.code.to_string(),
    };
    Line {
        client: Some(ctx.client.clone()),
        session: ctx.session_id.clone(),
        cauldron,
        tool: tool.to_string(),
        outcome,
        tx,
        counts: Value::Object(counts),
        ..Line::default()
    }
}

// --------------------------------------------------------------- arguments

/// A tool's arguments, taken one by one; [`Args::done`] refuses any left.
struct Args {
    tool: &'static str,
    m: Map<String, Value>,
}

impl Args {
    fn take(&mut self, k: &str) -> Option<Value> {
        self.m.remove(k).filter(|v| !v.is_null())
    }

    fn wrong(&self, k: &str, what: &str) -> Refusal {
        usage(format!("`{k}` of {} must be {what}", self.tool))
    }

    fn str(&mut self, k: &str) -> Result<Option<String>, Refusal> {
        match self.take(k) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(self.wrong(k, "a string")),
        }
    }

    fn required_str(&mut self, k: &str) -> Result<String, Refusal> {
        match self.str(k)? {
            Some(s) if !s.trim().is_empty() => Ok(s),
            _ => Err(self.wrong(k, "given, a non-empty string")),
        }
    }

    fn count(&mut self, k: &str) -> Result<Option<usize>, Refusal> {
        match self.take(k) {
            None => Ok(None),
            Some(v) => v.as_u64().map(|n| Some(n as usize)).ok_or_else(|| self.wrong(k, "a whole number")),
        }
    }

    fn flag(&mut self, k: &str) -> Result<bool, Refusal> {
        match self.take(k) {
            None => Ok(false),
            Some(Value::Bool(b)) => Ok(b),
            Some(_) => Err(self.wrong(k, "true or false")),
        }
    }

    fn strings(&mut self, k: &str) -> Result<Vec<String>, Refusal> {
        let err = self.wrong(k, "a non-empty list of strings");
        match self.take(k) {
            Some(Value::Array(a)) if !a.is_empty() => a.into_iter().map(|v| v.as_str().map(str::to_string).ok_or_else(|| err.clone())).collect(),
            _ => Err(err),
        }
    }

    fn objects(&mut self, k: &str) -> Result<Vec<Value>, Refusal> {
        let err = self.wrong(k, "a non-empty list of objects");
        match self.take(k) {
            Some(Value::Array(a)) if !a.is_empty() && a.iter().all(Value::is_object) => Ok(a),
            _ => Err(err),
        }
    }

    fn links(&mut self) -> Result<Vec<writer::Link>, Refusal> {
        let err = self.wrong("links", "a non-empty list of {from, kind, to, note?} with non-empty strings");
        let Some(Value::Array(list)) = self.take("links") else { return Err(err) };
        if list.is_empty() {
            return Err(err);
        }
        let mut out = Vec::new();
        for l in list {
            let Value::Object(mut m) = l else { return Err(err) };
            let mut s = |k: &str| match m.remove(k) {
                Some(Value::String(v)) if !v.trim().is_empty() => Ok(Some(v)),
                None | Some(Value::Null) => Ok(None),
                _ => Err(err.clone()),
            };
            let (from, kind, to, note) = (s("from")?, s("kind")?, s("to")?, s("note")?);
            let (Some(from), Some(kind), Some(to)) = (from, kind, to) else { return Err(err) };
            if let Some(k) = m.keys().next() {
                return Err(usage(format!("a link has an unknown key `{k}`; a link is {{from, kind, to, note?}}")));
            }
            out.push(writer::Link { from, kind, to, note });
        }
        Ok(out)
    }

    fn done(self) -> Result<(), Refusal> {
        match self.m.keys().next() {
            None => Ok(()),
            Some(k) => Err(usage(format!("{} takes no argument `{k}`", self.tool))),
        }
    }
}

// ----------------------------------------------------------- the carrier

/// What the shapewire carrier is handed.
pub struct Tools(pub Arc<Door>);

impl McpTools for Tools {
    fn verify(&self, authorization: Option<&str>) -> Result<Caller, Refusal> {
        if self.0.admits(authorization) {
            Ok(Caller { thread_id: "mcp-door".into(), run_id: String::new(), root: String::new(), depth: 0, turn: String::new() })
        } else {
            Err(Refusal::new(
                codes::UNAUTHORIZED,
                "this door needs `Authorization: Bearer <secret>`, the secret being the contents of the environment's \
                 mcp/door.secret, which the service mints at every start",
            ))
        }
    }

    fn tools(&self) -> Vec<ToolDef> {
        catalogue::defs()
    }

    fn instructions(&self, _caller: &Caller) -> Option<String> {
        Some(format!(
            "This is cauldron's MCP door to the soups of environment `{}`. Every cauldron is closed to it until its owner \
             grants it from a terminal (`cauldron mcp grant`); no tool here can grant, revoke or list grants. The cauldron a \
             call is about is its `cauldron` argument, else $CAULDRON_CAULDRON, else the cauldron whose grant names the \
             session's project directory. A refusal is a tool result with isError and a typed code — read it; it is never an absence.",
            self.0.env.name
        ))
    }

    fn call<'a>(
        &'a self,
        _caller: &'a Caller,
        name: &'a str,
        arguments: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, Refusal>> + Send + 'a>> {
        Box::pin(async move {
            let Some(tool) = catalogue::find(name) else {
                return Err(Refusal::new(codes::UNKNOWN_METHOD, name.to_string()));
            };
            let door = self.0.clone();
            tokio::task::spawn_blocking(move || door.call(tool, arguments))
                .await
                .unwrap_or_else(|e| Err(Refusal::new("store", format!("the call failed inside the service: {e}"))))
        })
    }
}
