# Cauldron for Claude Code

Use a Cauldron **soup** (a cauldron's typed knowledge graph) from a Claude
Code session: a `curator` subagent turns notes, a conversation excerpt or
findings into schema-valid soup records and lands them, and the published
schema ships with the plugin so the session can read it.

Reads work at any time, and so do imports: this release runs no soup daemon,
so nothing else holds a cauldron's soup. All
access goes through the `cauldron` CLI on the same machine as the cauldron;
the plugin has no MCP server.

## Contents

| path | what |
|---|---|
| `agents/curator.md` | the curator subagent (`cauldron:curator`) |
| `commands/curate.md` | `/cauldron:curate <cauldron> [--env <env>] [what to record]` |
| `skills/soup-schema/SKILL.md` | `/cauldron:soup-schema`: the schema in brief, with a pointer to the full README |
| `schema/` | a copy of the published schema (`schema/soup/workspace-v6/` in the Cauldron repository): `README.md`, `grammar.json`, `edges.json`, one JSON Schema per node type |

`schema/` is generated: `scripts/sync-plugin-schema` copies it from the
published directory, and a test in the spine crate fails when the copy drifts.
Do not edit it here.

## Install

You need a `cauldron` binary on `PATH` that has the `soup` verbs
(`cauldron soup help` prints them), and a cauldron with a soup. The binary
finds its data under `$CAULDRON_HOME` (default `~/.cauldron`); set it in the
environment Claude Code is started from if your cauldrons live elsewhere.

To make a cauldron to write into, from a shell:

```sh
cauldron env create dev            # an environment
cauldron create kb --env dev       # a cauldron with an empty soup (pool `ws`)
```

A new soup has **no regions**, and the curator never invents one. On the first
run it files nothing it cannot place and proposes regions instead; approve
them (or import a few region records yourself with `cauldron soup import`) and
run it again.

For one session, from a checkout of the Cauldron repository:

```sh
claude --plugin-dir <checkout>/claude-code/cauldron
```

To install it persistently, add the marketplace in `claude-code/` and install
from it (inside Claude Code):

```text
/plugin marketplace add <checkout>/claude-code
/plugin install cauldron@cauldron
```

## The verbs

All need the environment named (`--env`); none defaults to production. All
print JSON and exit non-zero on a refusal. The reads cannot write, and each
takes `--max-chars N` (longer text is cut and listed under `truncated`); the
lists take `--limit N` and say how many `more` there were.

- `cauldron soup regions --env E --cauldron C`: every region with its id,
  handle, boundary, flags, the regions it narrows and relates to, and its
  member count.
- `cauldron soup region <R> --env E --cauldron C [--depth N]`: one region, the
  regions it narrows and that narrow it (to depth N, default 1), the regions
  it relates to either way, and its members, newest first.
- `cauldron soup nodes --env E --cauldron C [--region R] [--type T] [--state S]`:
  `{id, type, state, handle, regions}` per node, newest first (default 50).
- `cauldron soup search <words> --env E --cauldron C [--region R] [--type T]`:
  each word of the query found case-insensitively, as a substring, in handles
  and bodies; ranked by terms matched, then handle over body, then newest; `{id, type, state, handle,
  regions, score, snippet}` (default 20).
- `cauldron soup get <id>... --env E --cauldron C`: nodes as records in the
  published shape, with their outgoing edges.
- `cauldron soup import <file-or-dir> --env E --cauldron C [--dry-run] [--remint]`:
  judges every record with the soup's own write door and lands the batch as
  one write, all or nothing. `--remint` treats ids as batch-local labels and
  prints the old-to-new map; a label may not equal an id the soup already
  holds (refused as `label-shadows-existing`), so a bare id that is not a
  label always means the existing node. A real import is refused while
  another import or recovery is writing to the same soup, or while an earlier
  import was interrupted and left the soup unsettled; `--dry-run` still works
  then, judged on a read-only snapshot.
- `cauldron soup recover --env E --cauldron C`: settles a soup an interrupted
  import left unsettled, by the soup's own record, and says which way it went
  in `interrupted_import`. `landed` means the interrupted import was finished
  and all of it is now in the soup (the output gives the count and ids): do
  NOT import that batch again, because it would land every record a second
  time and nodes cannot be deleted. `rolled-back` means none of it landed:
  importing it again is safe. `none` means there was nothing to settle; check
  `cauldron soup nodes` before importing a batch whose fate you do not know.

## A worked session

In Claude Code:

```text
> /cauldron:curate kb --env dev The import failed because a record guessed a
  region id; we decided to always read region ids from `soup regions` first.
```

The curator reads `schema/README.md`, lists the regions, searches the soup
for what it already says (`soup search region id`), and writes a batch with
labels `n1`, `n2` to a file:

```json
[
  {"id": "n1", "type": "issue", "state": "open",
   "handle": "A batch import failed because one edge targeted an id the soup did not hold",
   "body": "The dry run reported unknown-target on the edge; the record named a region by a guessed id. See [[n2]].",
   "meta": {"kind": "case", "cites": [{"kind": "file", "path": "notes/session.md", "line": 3}]},
   "edges": [{"kind": "in_region", "to": "R-2", "note": "landing externally written records in a soup"},
             {"kind": "ref", "to": "n2"}]},
  {"id": "n2", "type": "decision", "state": "decided",
   "handle": "Read region ids from `cauldron soup regions` before writing any record",
   "meta": {"kind": "pattern"},
   "edges": [{"kind": "in_region", "to": "R-1", "note": "how outside agents use it"},
             {"kind": "derived_from", "to": "n1"}]}
]
```

It dry-runs with `cauldron soup import batch.json --env dev --cauldron kb
--dry-run --remint`. A first draft that wrote `"state": "done"` on the decision
and pointed the issue at a region `R-9` came back refused:

```json
[{"id": "n1", "disposition": "refused", "reasons": [{"code": "unknown-target",
   "detail": "edge 0 (in_region) points at `R-9`, which is neither a record of this batch nor a node of pool `ws`"}]},
 {"id": "n2", "disposition": "refused", "reasons": [{"code": "refused-by-destination-grammar",
   "witnesses": [{"kind": "grammar_violation",
     "message": "unknown_state: `done` is outside `decision`'s closed state vocabulary"}]}]}]
```

Fixed, the dry run accepts both, and the real import lands them in one write:

```json
{"outcome": "landed", "accepted": 2, "refused": 0,
 "id_map": {"n1": "IS-2", "n2": "DEC-2"}, "landed": {"seq": 2}}
```

`cauldron soup get IS-2` shows the body's link rewritten to `[[DEC-2]]` and its
`ref` edge pointing at `DEC-2`. The curator reports the two new ids, their
handles and regions. Had no region admitted a record, it would have left that
record out and proposed a region for you to approve instead.
