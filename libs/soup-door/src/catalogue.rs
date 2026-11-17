//! **The MCP door's tool catalogue**: every tool the `mcp` service lists, its
//! arguments, and the grant scope a call to it needs. It lives here, beside
//! the logic, so the stdio bridge (`cauldron mcp stdio`) can answer
//! `tools/list` with the same list when the service is not running. Static:
//! `tools/list` answers the same
//! list whether or not any cauldron is granted, so a client's configuration
//! never finds a tool missing. `soup_claim` always answers `unsupported`.
//!
//! Every tool takes an optional `cauldron`, which names the target outright
//! (the service's `ctx` module has the rest of the resolution). Every read takes an
//! optional `max_chars` that bounds the WHOLE reply payload's compact JSON
//! (the tool result's structured content; default 12,000, floor 256, ceiling
//! 50,000, a value under the floor raised to it); what was left out is listed
//! in `truncated`. `soup_dump_get` is the exception: a page of a dump's
//! content has a floor of 1,000 (`dumps::PAGE_MIN_CHARS`), and a smaller
//! value is refused as `usage`, so that no page is ever longer than asked. The JSON-RPC message carries that payload twice (as text
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

/// The door's second route: `soup_dump_session` as the stdio bridge sends
/// it. The body is the `tools/call` message on its first line (at most
/// [`SESSION_HEAD_LIMIT`] bytes) and then the session's transcript,
/// verbatim, streamed to the dumps store's blob store and never held whole
/// in memory. The service reads no path a caller names: the transcript
/// arrives as bytes from the bridge, which found it on this machine.
pub const SESSION_PATH: &str = "/mcp/dump-session";

/// The most the first line of a [`SESSION_PATH`] request may be.
pub const SESSION_HEAD_LIMIT: usize = 64 * 1024;

/// What a tool needs before it may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Needs {
    /// A grant with this scope on the resolved cauldron.
    Scope(Scope),
    /// A grant with both scopes (reading a dump's content: `read` and
    /// `dump`).
    Both(Scope, Scope),
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

/// `soup_dump_get`'s `max_chars`: a page of a dump's content has a floor of
/// its own (`dumps::PAGE_MIN_CHARS`), and a smaller value is refused.
fn page_max_chars() -> Value {
    use crate::budget::{DEFAULT_MAX_CHARS, MAX_CHARS_CEILING};
    use crate::dumps::PAGE_MIN_CHARS;
    json!({"type": "integer", "minimum": PAGE_MIN_CHARS, "maximum": MAX_CHARS_CEILING, "description": format!(
        "Bound on the page's characters (the compact JSON of the structured result; the JSON-RPC message carries it twice, as text and as structured content); no page is longer. Default {DEFAULT_MAX_CHARS}, floor {PAGE_MIN_CHARS} (a page's fixed part alone is several hundred characters, so a smaller value is refused as `usage`), ceiling {MAX_CHARS_CEILING} (a larger one is lowered to it)."
    )})
}

fn limit(default: usize) -> Value {
    json!({"type": "integer", "minimum": 0, "description": format!("How many items, newest or best first. Default {default}.")})
}

fn include_superseded() -> Value {
    json!({"type": "boolean", "description": "List superseded nodes too (a node another node's `supersedes` edge points at), each with its `superseded_by`. Default false: they are left out and counted in `superseded_hidden`."})
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
        description: "One region: its record, the regions it narrows and that narrow it (to `depth` levels), the regions it relates to, and its direct members {id, type, state, handle} (no `state` for a node of a type that has none), without superseded members unless `include_superseded` (`superseded_hidden` says how many were left out). There are no clusters in this schema (`clusters` is always empty).",
        schema: || {
            object(
                json!({"cauldron": cauldron(), "id": {"type": "string", "description": "The region's id, e.g. R-1."},
                       "depth": {"type": "integer", "minimum": 0, "description": "Levels of parents and children. Default 1."},
                       "limit": limit(crate::reads::MEMBERS_LIMIT), "max_chars": max_chars(), "include_superseded": include_superseded()}),
                &["id"],
            )
        },
    },
    Tool {
        name: "soup_nodes",
        needs: Needs::Scope(Scope::Read),
        description: "Nodes, newest first, as {id, type, state, handle, regions} (no `state` for a region or viewer: they carry none), filtered by region (direct members), type and state. A superseded node is left out unless `include_superseded`, and `superseded_hidden` says how many were. The same items as `cauldron soup nodes`.",
        schema: || {
            object(
                json!({"cauldron": cauldron(), "region": {"type": "string"}, "type": {"type": "string"}, "state": {"type": "string"},
                       "limit": limit(crate::reads::NODES_LIMIT), "max_chars": max_chars(), "include_superseded": include_superseded()}),
                &[],
            )
        },
    },
    Tool {
        name: "soup_search",
        needs: Needs::Scope(Scope::Read),
        description: "Search handles and bodies for the query's words, best first, as {id, type, state, handle, regions, score, snippet} (no `state` for a region or viewer: they carry none). A superseded node is left out unless `include_superseded`, and `superseded_hidden` says how many were. The same items as `cauldron soup search`.",
        schema: || {
            object(
                json!({"cauldron": cauldron(), "query": {"type": "string"}, "region": {"type": "string"}, "type": {"type": "string"},
                       "limit": limit(crate::reads::SEARCH_LIMIT), "max_chars": max_chars(), "include_superseded": include_superseded()}),
                &["query"],
            )
        },
    },
    Tool {
        name: "soup_get",
        needs: Needs::Scope(Scope::Read),
        description: "Whole records in the published shape (workspace-v6), with their outgoing edges and `meta.cites`. A superseded node is answered all the same, with `superseded_by: [ids]` (read from the inbound `supersedes` edges; not a field, so leave it out when writing the record again). Ids the soup does not hold are listed in `missing`.",
        schema: || object(json!({"cauldron": cauldron(), "ids": {"type": "array", "items": {"type": "string"}, "minItems": 1}, "max_chars": max_chars()}), &["ids"]),
    },
    Tool {
        name: "soup_put",
        needs: Needs::Scope(Scope::Write),
        description: "Land records written in the published shape (workspace-v6) as ONE write, all or nothing: every record is judged by the soup's grammar first, and every `meta.cites` entry by the citation table (a kind outside it, or a citation without its kind's keys, is refused as `bad-cite`). `dry_run` judges and lands nothing. `remint` treats ids as batch-local labels and answers `id_map` when the batch lands or a dry run validates (a refused batch has none: its ids were never taken). `partial` lands the records that pass and lists the ones dropped (dependents of a dropped record are dropped too). `cite` (an object in `meta.cites` form) is added to every record, and so is this session. A batch identical to one that already landed is refused unless `allow_duplicate`. A refused batch's `detail` (and a refused dry run's answer) has one shape: `outcome: \"refused\"`, `refusal` (`records-refused`, `landing-refused` or `batch-already-landed`), `accepted`, `refused` and `records`.",
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
        description: "What landed since a cursor, oldest first: {changes: [{seq, at, op, tx, ids: [{id, type, handle}], count}], dumps: [{seq, at, op, id, kind, bytes, state}], cursor, head, more}. In `changes` (the soup), `op` is `create` (new nodes), `link` (new edges only) or `update`; `dumps` are the dumps received (ids and sizes, never text). With no `since`, only the current cursor, never a dump of the soup. Take a cursor from soup_status, soup_put, soup_link or an earlier soup_changes, and pass the answer's `cursor` as the next `since`. A cursor of another cauldron, or of a cauldron removed and made again under the same name, is refused as `cursor_unknown`; one ahead of the soup as `cursor_stale`.",
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
        needs: Needs::Scope(Scope::Dump),
        description: "Keep a piece of raw text in the cauldron as a dump, for later processing: it lands as `DM-<n>` in state `received`, in the cauldron's dumps store beside its soup (not as a soup node). One dump is one write. At most 1 MiB of text (larger is refused as `too_large`; split it). `title` names it; `source` says where the text came from; `cite` (meta.cites form) and `refs` (soup ids) are kept with it. Answers a receipt (id, bytes, sha256, cursor), never the text. Processing of dumps comes in a later release.",
        schema: || {
            object(
                json!({"cauldron": cauldron(),
                       "text": {"type": "string", "description": "The text to keep, verbatim. At most 1 MiB (UTF-8 bytes)."},
                       "title": {"type": "string", "description": "A short name for the dump (at most 200 characters)."},
                       "source": {"type": "string", "description": "Where the text came from, in a few words (at most 500 characters)."},
                       "cite": {"type": "object", "description": "One citation in `meta.cites` form, e.g. {\"kind\":\"url\",\"url\":\"…\"}."},
                       "refs": {"type": "array", "items": {"type": "string"}, "description": "Ids of soup nodes this text is about."}}),
                &["text"],
            )
        },
    },
    Tool {
        name: "soup_dump_session",
        needs: Needs::Scope(Scope::Dump),
        description: "Keep a Claude Code session's transcript in the cauldron as a dump, verbatim, for later processing (at most 64 MiB). The session is `session_id`, else this session. Its transcript is found on this machine under the Claude Code config directory's `projects` store, by the bridge, never by a path. A transcript holds tool output and anything pasted into the session; it is stored as it is. Answers a receipt (id, bytes, sha256 equal to the transcript file's, lines, cwd). `session_unknown` when no transcript has that id.",
        schema: || {
            object(
                json!({"cauldron": cauldron(),
                       "session_id": {"type": "string", "description": "The session whose transcript to keep (its id, never a path). Default: this session."},
                       "title": {"type": "string", "description": "A short name for the dump (at most 200 characters)."}}),
                &[],
            )
        },
    },
    Tool {
        name: "soup_dumps",
        needs: Needs::Scope(Scope::Read),
        description: "The cauldron's dumps, newest first, as summaries {id, state, kind, title, bytes, sha256, received_at, source}: never their text. Filter by `state` (received, processed, failed, removing, removed) and `kind` (text, session). A `removing` dump's removal is unfinished: its content no longer reads, it carries `erasure: \"unfinished\"` and `finish_with`, the shell command that finishes it.",
        schema: || {
            object(
                json!({"cauldron": cauldron(), "state": {"type": "string"}, "kind": {"type": "string"},
                       "limit": limit(crate::dumps::LIST_LIMIT), "max_chars": max_chars()}),
                &[],
            )
        },
    },
    Tool {
        name: "soup_dump_get",
        needs: Needs::Both(Scope::Read, Scope::Dump),
        description: "One dump's content, from byte `offset`, as much as fits `max_chars`: a text dump's text, or a session transcript verbatim (`view: \"raw\"`, the default). For a session transcript, `view: \"text\"` answers only what the person typed and what the assistant wrote as text, as turns each with `at` (the byte where its line starts), `role` and `text`; everything else is left out and counted in `elided` by kind: tool calls and results, thinking, `!` shell commands and their output (`shell`), local commands' output (`local_command`), slash-command wrappers (`commands`), system reminders, other text Claude Code writes itself (`generated`), isMeta lines, attachments, images and other lines. The raw view still holds the whole transcript. Pages end between turns and read the same every time; a turn longer than a page is cut, with `chars` its whole length. The answer says `next_offset` (and, in the text view, `next_turn_offset`) when there is more; call again with them. No page is longer than `max_chars`, which is at least 1000 here (smaller is refused as `usage`). A removed dump answers its tombstone (`removed: true`, its summary with `removed_at`, no text); one whose removal is unfinished (state `removing`) also says `erasure: \"unfinished\"` and `finish_with`, the shell command that finishes it. Needs a grant of both `read` and `dump`.",
        schema: || {
            object(
                json!({"cauldron": cauldron(), "id": {"type": "string", "description": "The dump's id, e.g. DM-1."},
                       "offset": {"type": "integer", "minimum": 0, "description": "The byte to start at. Default 0. In the text view, 0 or a `next_offset` it answered (where a line starts)."},
                       "view": {"type": "string", "enum": ["raw", "text"], "description": "`raw` (default): the content verbatim. `text`: a session transcript's user and assistant text turns only."},
                       "turn_offset": {"type": "integer", "minimum": 0, "description": "Text view only: a page's `next_turn_offset`, the characters of a cut turn already read. Default 0."},
                       "max_chars": page_max_chars()}),
                &["id"],
            )
        },
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
            if t.name == "soup_dump_get" {
                // A page of a dump's content has a floor of its own, refused below.
                assert_eq!(m["minimum"], json!(crate::dumps::PAGE_MIN_CHARS));
                assert_eq!(m["maximum"], json!(crate::budget::MAX_CHARS_CEILING));
                let d = m["description"].as_str().unwrap();
                assert!(d.contains("floor 1000") && d.contains("refused") && d.contains("twice"), "{d}");
                continue;
            }
            assert_eq!(m["minimum"], json!(crate::budget::MAX_CHARS_FLOOR), "{}", t.name);
            assert_eq!(m["maximum"], json!(crate::budget::MAX_CHARS_CEILING), "{}", t.name);
            let d = m["description"].as_str().unwrap();
            assert!(d.contains("floor 256") && d.contains("ceiling 50000") && d.contains("twice"), "{d}");
        }
        assert_eq!(seen, 8, "every read takes max_chars");
        assert_eq!(crate::budget::clamp(Some(1)), crate::budget::MAX_CHARS_FLOOR);
    }
}
