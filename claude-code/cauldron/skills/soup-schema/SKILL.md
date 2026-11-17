---
name: soup-schema
description: A short orientation to the Cauldron soup schema (workspace grammar v6), the node types, edges and record shape that the cauldron MCP tools (`soup_put`) and `cauldron soup import` accept. Use when you need to read, write or reason about records in a Cauldron soup, or before calling the curator.
---

A Cauldron **soup** is a knowledge graph: typed nodes joined by a closed set of
typed edges, grouped into overlapping **regions**. Every write is judged by one
pinned grammar, `workspace@6`. The full reference, with every rule and a
worked example, is `${CLAUDE_PLUGIN_ROOT}/schema/README.md`; read it before you
write records yourself.

**The record.** One JSON (or YAML) object per node:
`{id, type, state?, <fields>, edges: [{kind, to, note?}]}`. `handle` is the
one-line claim (at most 235 characters), `body` optional prose (at most
10,000), `meta` an open object. Edges list only what leaves the node.

**Types.** Ten content types, each with an id prefix and a closed state set:
`directive` (DIR, human-set direction; needs `operator`), `constraint` (CON, a
rule of the work), `danger` (D, a hazard), `issue` (IS, something wrong),
`decision` (DEC, a call or finding), `question` (Q, an open fork), `task` (T,
dispatched work), `milestone` (M, serves a directive), `idea` (C), `handoff`
(HO). Plus `region` (R, a topic with an `Inside:`/`Outside:` boundary, no state)
and `viewer` (V).

**Placement.** Every content node mints with an `in_region` edge (or, rarely,
two `rejected_by` edges and `searched`). Regions are found with the
`soup_regions` MCP tool, or `cauldron soup regions --env E --cauldron C`.

**Knowledge capture.** A pattern is a `decision` or `constraint` with
`meta.kind: "pattern"`; a case is an `idea` or `issue` with
`meta.kind: "case"`. Evidence goes in `meta.cites` as `{kind, ...}` objects.

**Writing.** Use the `cauldron:curator` subagent (`/cauldron:curate`), or the
`soup_put` MCP tool (`remint: true`, `dry_run: true` first), or
`cauldron soup import <file> --env E --cauldron C --remint`, `--dry-run` first.
Either lands all records or none, and says why any record was refused. Under
`--remint` use labels like `n1`: a label equal to a held id is refused, and
such a record is checked against `remint.<type>.schema.json`, not
`node.<type>.schema.json`. Find
what is there with `soup_search`, `soup_nodes` and `soup_region` (or
`cauldron soup search <words>`, `soup nodes [--region R]` and `soup region
<R>`), then read nodes with `soup_get` (or `cauldron soup get <id>...`). The
MCP tools reach a cauldron only once its owner granted it (`cauldron mcp
grant`, at a terminal).
