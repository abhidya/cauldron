//! **The MCP door's tool catalogue**: every tool the `mcp` service lists, its
//! arguments, and the grant scope a call to it needs. It lives here, beside
//! the logic, so the stdio bridge (`cauldron mcp stdio`) can answer
//! `tools/list` with the same list when the service is not running. Static:
//! `tools/list` answers the same
//! list whether or not any cauldron is granted, so a client's configuration
//! never finds a tool missing. A tool that is listed but not yet built in
//! this release answers `not_available_yet`; `soup_claim` always answers
//! `unsupported`.
//!
//! Every tool takes an optional `cauldron`, which names the target outright
//! (the service's `ctx` module has the rest of the resolution). Every read takes an
//! optional `max_chars` that bounds the WHOLE reply payload's compact JSON
//! (the tool result's structured content; default 12,000, floor 256, ceiling
//! 50,000, a value under the floor raised to it); what was left out is listed
//! in `truncated`. The JSON-RPC message carries that payload twice (as text
//! and as structured content), so the message itself is about twice as long.

use serde_json::{json, Value};
use shapewire::ToolDef;
use crate::grants::Scope;

/// The key under which the stdio bridge adds the caller's context to every
/// call's `arguments`. No tool lists it.
pub const CTX_KEY: &str = "_ctx";

/// The largest request body the MCP door reads from a caller that carries
/// its secret (16 MiB). A larger request is refused as `too_large` from its
/// `Content-Length`, before any of its body is read. It lives here so the
/// command line can refuse a forwarded `cauldron soup import` that would not
/// fit before sending it.
pub const REQUEST_BODY_LIMIT: usize = 16 * 1024 * 1024;

/// What a tool needs before it may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Needs {
    /// A grant with this scope on the resolved cauldron.
    Scope(Scope),
    /// Listed, not built in this release: always `not_available_yet`.
    NotYet(&'static str),
    /// Listed so a client's configuration finds it; always `unsupported`.
    Unsupported,
}

/// One tool.
pub struct Tool {
    pub name: &'static str,
    pub needs: Needs,
    pub description: &'static str,
    schema: fn() -> Value,
}

impl Tool {
    pub fn def(&self) -> ToolDef {
        ToolDef { name: self.name.to_string(), description: self.description.to_string(), input_schema: (self.schema)() }
    }
}

fn cauldron() -> Value {
    json!({"type": "string", "description": "The cauldron to use. When absent: $CAULDRON_CAULDRON of the session, else the cauldron whose grant names the session's project directory (a project only chooses the default; it does not limit access)."})
}

fn max_chars() -> Value {
    use crate::budget::{DEFAULT_MAX_CHARS, MAX_CHARS_CEILING, MAX_CHARS_FLOOR};
    json!({"type": "integer", "minimum": MAX_CHARS_FLOOR, "maximum": MAX_CHARS_CEILING, "description": format!(
        "Bound on the reply payload's characters (the compact JSON of the structured result; the JSON-RPC message carries it twice, as text and as structured content). Default {DEFAULT_MAX_CHARS}, floor {MAX_CHARS_FLOOR} (a smaller value is raised to it), ceiling {MAX_CHARS_CEILING} (a larger one is lowered to it). What was left out is listed in `truncated`."
    )})
}

fn limit(default: usize) -> Value {
    json!({"type": "integer", "minimum": 0, "description": format!("How many items, newest or best first. Default {default}.")})
}

fn object(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required})
}

/// The whole catalogue, in the order `tools/list` answers it.
pub const TOOLS: &[Tool] = &[
    Tool {
        name: "soup_status",
        needs: Needs::Scope(Scope::Read),
        description: "Which cauldron this session reaches and how it was chosen, its grant's scopes, node counts by type, the region count, the change cursor, and whether this door holds it open for writing (with what the open recovered).",
        schema: || object(json!({"cauldron": cauldron()}), &[]),
    },
    Tool {
        name: "soup_regions",
        needs: Needs::Scope(Scope::Read),
        description: "Every region: id, handle, boundary, flags, the regions it narrows or relates to, and how many nodes sit in it. The same items as `cauldron soup regions`.",
        schema: || object(json!({"cauldron": cauldron(), "max_chars": max_chars()}), &[]),
    },
    Tool {
        name: "soup_region",
        needs: Needs::Scope(Scope::Read),
        description: "One region: its record, the regions it narrows and that narrow it (to `depth` levels), the regions it relates to, and its direct members {id, type, state, handle}. There are no clusters in this schema (`clusters` is always empty).",
        schema: || {
            object(
                json!({"cauldron": cauldron(), "id": {"type": "string", "description": "The region's id, e.g. R-1."},
                       "depth": {"type": "integer", "minimum": 0, "description": "Levels of parents and children. Default 1."},
                       "limit": limit(crate::reads::MEMBERS_LIMIT), "max_chars": max_chars()}),
                &["id"],
            )
        },
    },
    Tool {
        name: "soup_nodes",
        needs: Needs::Scope(Scope::Read),
        description: "Nodes, newest first, as {id, type, state, handle, regions}, filtered by region (direct members), type and state. The same items as `cauldron soup nodes`.",
        schema: || {
            object(
                json!({"cauldron": cauldron(), "region": {"type": "string"}, "type": {"type": "string"}, "state": {"type": "string"},
                       "limit": limit(crate::reads::NODES_LIMIT), "max_chars": max_chars()}),
                &[],
            )
        },
    },
    Tool {
        name: "soup_search",
        needs: Needs::Scope(Scope::Read),
        description: "Search handles and bodies for the query's words, best first, as {id, type, state, handle, regions, score, snippet}. The same items as `cauldron soup search`.",
        schema: || {
            object(
                json!({"cauldron": cauldron(), "query": {"type": "string"}, "region": {"type": "string"}, "type": {"type": "string"},
                       "limit": limit(crate::reads::SEARCH_LIMIT), "max_chars": max_chars()}),
                &["query"],
            )
        },
    },
    Tool {
        name: "soup_get",
        needs: Needs::Scope(Scope::Read),
        description: "Whole records in the published shape (workspace-v6), with their outgoing edges and `meta.cites`. Ids the soup does not hold are listed in `missing`.",
        schema: || object(json!({"cauldron": cauldron(), "ids": {"type": "array", "items": {"type": "string"}, "minItems": 1}, "max_chars": max_chars()}), &["ids"]),
    },
    Tool {
        name: "soup_put",
        needs: Needs::Scope(Scope::Write),
        description: "Land records written in the published shape (workspace-v6) as ONE write, all or nothing: every record is judged by the soup's grammar first. `dry_run` judges and lands nothing. `remint` treats ids as batch-local labels and answers `id_map`. `partial` lands the records that pass and lists the ones dropped (dependents of a dropped record are dropped too). `cite` (an object in `meta.cites` form) is added to every record, and so is this session. A batch identical to one that already landed is refused unless `allow_duplicate`.",
        schema: || {
            object(
                json!({"cauldron": cauldron(),
                       "records": {"type": "array", "items": {"type": "object"}, "minItems": 1},
                       "cite": {"type": "object", "description": "One citation in `meta.cites` form, e.g. {\"kind\":\"url\",\"url\":\"…\"}."},
                       "remint": {"type": "boolean"}, "dry_run": {"type": "boolean"}, "partial": {"type": "boolean"},
                       "allow_duplicate": {"type": "boolean", "description": "Land a batch even when the same batch already landed."}}),
                &["records"],
            )
        },
    },
    Tool {
        name: "soup_link",
        needs: Needs::Scope(Scope::Write),
        description: "Connect edges between nodes the soup already holds, all in ONE write: each link is judged by the grammar first, and if any is refused nothing lands. Answers each link as `linked` or `refused` with the reason.",
        schema: || {
            object(
                json!({"cauldron": cauldron(),
                       "links": {"type": "array", "minItems": 1, "items": {"type": "object",
                           "properties": {"from": {"type": "string"}, "kind": {"type": "string"}, "to": {"type": "string"}, "note": {"type": "string"}},
                           "required": ["from", "kind", "to"]}}}),
                &["links"],
            )
        },
    },
    Tool {
        name: "soup_changes",
        needs: Needs::Scope(Scope::Read),
        description: "What landed since a cursor, oldest first: {changes: [{seq, at, op, tx, ids: [{id, type, handle}], count}], cursor, head, more}. `op` is `create` (new nodes), `link` (new edges only) or `update`. With no `since`, only the current cursor, never a dump of the soup. Take a cursor from soup_status, soup_put, soup_link or an earlier soup_changes, and pass the answer's `cursor` as the next `since`. A cursor of another cauldron, or of a cauldron removed and made again under the same name, is refused as `cursor_unknown`; one ahead of the soup as `cursor_stale`.",
        schema: || {
            object(
                json!({"cauldron": cauldron(), "since": {"type": "string", "description": "A cursor (v1.…) this cauldron answered earlier."},
                       "limit": limit(100), "max_chars": max_chars()}),
                &[],
            )
        },
    },
    Tool {
        name: "soup_claim",
        needs: Needs::Unsupported,
        description: "Not supported: this schema (workspace-v6) has no clusters. Put a node in a region with an `in_region` edge instead.",
        schema: || object(json!({"cluster": {"type": "string"}, "region": {"type": "string"}}), &[]),
    },
    Tool {
        name: "soup_dump",
        needs: Needs::NotYet("`soup_dump` is listed and not built in this release"),
        description: "Keep a piece of text in the cauldron as a dump, for later processing. Answers a receipt with the dump's id, never the text.",
        schema: || object(json!({"cauldron": cauldron(), "text": {"type": "string"}, "title": {"type": "string"}, "cite": {"type": "object"}, "refs": {"type": "array", "items": {"type": "string"}}}), &["text"]),
    },
    Tool {
        name: "soup_dump_session",
        needs: Needs::NotYet("`soup_dump_session` is listed and not built in this release"),
        description: "Keep a session's transcript in the cauldron as a dump, for later processing. Answers a receipt with the dump's id.",
        schema: || object(json!({"cauldron": cauldron(), "session_id": {"type": "string"}, "title": {"type": "string"}}), &[]),
    },
];

/// The tool named `name`.
pub fn find(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.name == name)
}

/// Every tool, as `tools/list` answers it.
pub fn defs() -> Vec<ToolDef> {
    TOOLS.iter().map(Tool::def).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No tool here can grant, revoke or list grants, and none shows `_ctx`.
    #[test]
    fn no_tool_touches_grants_and_none_lists_ctx() {
        for t in TOOLS {
            assert!(!t.name.contains("grant") && !t.name.contains("revoke"), "{}", t.name);
            let s = (t.schema)();
            assert!(s["properties"].get(CTX_KEY).is_none(), "{} shows _ctx", t.name);
            assert_eq!(s["type"], "object");
        }
        let mut names: Vec<_> = TOOLS.iter().map(|t| t.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), TOOLS.len(), "a tool is listed twice");
    }

    #[test]
    fn max_chars_is_declared_with_the_floor_and_ceiling_the_budget_applies() {
        let mut seen = 0;
        for t in TOOLS {
            let s = (t.schema)();
            let Some(m) = s["properties"].get("max_chars") else { continue };
            seen += 1;
            assert_eq!(m["minimum"], json!(crate::budget::MAX_CHARS_FLOOR), "{}", t.name);
            assert_eq!(m["maximum"], json!(crate::budget::MAX_CHARS_CEILING), "{}", t.name);
            let d = m["description"].as_str().unwrap();
            assert!(d.contains("floor 256") && d.contains("ceiling 50000") && d.contains("twice"), "{d}");
        }
        assert_eq!(seen, 6, "every read takes max_chars");
        assert_eq!(crate::budget::clamp(Some(1)), crate::budget::MAX_CHARS_FLOOR);
    }
}
