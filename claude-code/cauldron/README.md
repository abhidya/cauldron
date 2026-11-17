# Cauldron for Claude Code

Use a Cauldron **soup** (a cauldron's typed knowledge graph) from a Claude
Code session: the plugin's MCP server gives the session tools to read,
search and write the soups their owner has opened to it, a `curator`
subagent turns notes, a conversation excerpt or findings into schema-valid
soup records and lands them, and the published schema ships with the plugin
so the session can read it.

Everything runs on the same machine as the cauldron. The MCP server is
`cauldron mcp stdio`, a bridge to the environment's `mcp` service; the
`cauldron` CLI works beside it and is the curator's fallback.

## Contents

| path | what |
|---|---|
| `.mcp.json` | the MCP server `cauldron`: `cauldron mcp stdio` |
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

To make a project directory name its cauldron, so neither you nor the
curator has to pass `--env` and `--cauldron` inside it:

```sh
cd <project>
cauldron link kb --env dev         # writes <project>/.cauldron
cauldron link --show               # what this directory resolves to, and from where
```

`cauldron soup` run in that directory or any directory below it then uses
`kb` in `dev`. The marker is a text file holding two `key = value` lines,
`env = dev` and `cauldron = kb`, and `#` comments. An explicit `--env`,
`--cauldron`, `$CAULDRON_ENV`, `$ENV` or `$CAULDRON_CAULDRON` always wins over
it; an explicit environment that
differs from the marker's, with no cauldron named, is refused rather than
guessed. Without a marker, nothing changes: every verb still needs the
environment named.

The marker is per-machine: it names an environment and a cauldron on this
machine, so keep it out of version control by adding `.cauldron` to the
project's `.gitignore` (`cauldron link` prints a hint when it is not ignored).
It can name only an environment and a cauldron, never a path, a pool or a
file. A marker found in the directory or any parent is obeyed, including one
that arrived with a cloned repository, so check `cauldron link --show` in a
project you did not link yourself. Every soup verb that took its target from a
marker says so: a line on stderr, and `"resolved_from": "marker:<path>"` in its
JSON. An import (without `--dry-run`) or `recover` refuses to take its target
from a marker that is tracked by git or owned by another user; naming `--env`
and `--cauldron` explicitly writes without reading any marker. A `.cauldron`
that is a symbolic link is never followed or written through. The curator
states the target it took from a marker before its first write, and asks you
to confirm before writing to `prod` from a marker.

## The MCP door

The plugin's `.mcp.json` starts `cauldron mcp stdio` with each session. That
bridge talks to the environment's `mcp` service, a supervised service that
listens on `127.0.0.1` only and is the one writer of every soup it holds.

**Every cauldron is closed to it until you grant it.** Granting is your act,
at a terminal on this machine:

```sh
cauldron --env dev start mcp                         # once; then the supervisor keeps it
cauldron mcp grant kb --env dev --project ~/code/proj # run it yourself, in a terminal
cauldron mcp grants --env dev                        # what is open
cauldron mcp revoke kb --env dev                     # close it again
cauldron mcp status --env dev                        # the service and what it holds
```

- `grant` runs only with its input and output on an interactive terminal, and
  no option skips that: an agent, whose shell has no terminal, cannot open a
  cauldron for itself, and no MCP tool can grant, revoke or list grants. The
  default scopes are `read,write,dump` (`--scopes read` opens it read-only;
  `dump` is not used by any tool in this release).
- A grant opens the cauldron to the Claude Code sessions you run on this
  machine against that environment, whichever directory they work in: any
  session can reach it by naming it (a call's `cauldron` argument, or
  `$CAULDRON_CAULDRON`, or `cauldron mcp stdio --cauldron`, for every call).
- `--project <dir>` (repeatable) only tells a session which cauldron a
  directory means when the session does not name one: a session working in
  that directory, or below it, uses this cauldron by default. It does not
  limit which sessions can reach the cauldron. Nothing else chooses one: the
  `.cauldron` marker never opens anything to the MCP door.
- The environment is the one Claude Code was started in: `$CAULDRON_ENV`,
  else `prod`. `$ENV` is not read by the bridge (a shell's `ENV=development`
  names no cauldron environment). The door serves that environment only.
  When the bridge cannot use its environment (no such environment, or a bad
  name), the session still lists the tools and every call is refused as
  `no_environment`, saying why.
- `revoke` takes effect on the next call: the service closes the cauldron and
  lets go of its soup. Writes still waiting in its queue are refused
  (`not_granted`) and never land; a write already being applied when the
  revoke comes finishes, all of it or none of it. `revoke --project <dir>`
  removes only that binding.
- A grant is for the cauldron it was made for, not only its name: if that
  cauldron is removed and another is made under the same name, calls are
  refused as `cauldron_gone` until you grant the new one.
- The grants are kept in `<root>/data/<env>/mcp/grants.json`, owner-only.

The tools: `soup_status` (which cauldron this session reaches and why, its
counts and change cursor), `soup_regions`, `soup_region`, `soup_nodes`,
`soup_search` and `soup_get` (the same answers as the `cauldron soup` reads,
each reply's payload bounded by `max_chars`: default 12,000, floor 256,
ceiling 50,000), `soup_put` (records in the published shape,
judged and landed as one all-or-nothing write, with `dry_run`, `remint`,
`partial`, `cite` and `allow_duplicate`; the session is cited on every record
when Claude Code passes its id), `soup_link` (edges between nodes the soup
holds) and `soup_changes` (what landed since a cursor). `soup_claim`,
`soup_dump` and `soup_dump_session` are listed and refuse
(`unsupported`, `not_available_yet`). A refusal is a tool result with a
typed `code` (`not_granted`, `no_cauldron_for_project`, `records_refused`,
`batch_already_landed`, …) and a message that says what to do.

While the service is not running, the bridge still lists the tools, and every
call is refused as `service_down` with the command that starts it.

**The CLI beside it.** While the service holds a cauldron, `cauldron soup
import` is sent to the service and lands through it (its JSON says
`forwarded_to`), with the same batch-already-landed check, and `cauldron soup
recover` is refused, because the service recovers the soup itself when it
opens it (`cauldron mcp status` shows what that recovered). The service reads
at most 16 MiB in one request, so a larger import is refused before it is sent
(never split, so it stays all or nothing): import it in smaller files, or
revoke the grant (or stop the service) and import directly. An ungranted
cauldron is never held, and the CLI works on it as before.

Every write through the door adds a line to `<root>/data/<env>/mcp/calls.jsonl`
(when, which client and session, which tool, the outcome and counts; never
record text).

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

The curator falls back to these when the session has no MCP tools or the
`mcp` service is down. All need the environment and the cauldron named, by `--env` and `--cauldron`
or by the project's `.cauldron` marker (`cauldron link`); none defaults to
production. All
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
- `cauldron soup import <file-or-dir> --env E --cauldron C [--dry-run] [--remint] [--allow-duplicate]`:
  judges every record with the soup's own write door and lands the batch as
  one write, all or nothing. `--remint` treats ids as batch-local labels and
  prints the old-to-new map; a label may not equal an id the soup already
  holds (refused as `label-shadows-existing`), so a bare id that is not a
  label always means the existing node. A real import is refused while
  another import or recovery is writing to the same soup, or while an earlier
  import was interrupted and left the soup unsettled; `--dry-run` still works
  then, judged on a read-only snapshot. A batch identical to one that already
  landed in the soup (the same records, before re-minting) is refused as
  `batch-already-landed`, saying when it landed and how many records it held;
  `--allow-duplicate` lands a second copy on purpose. Batches landed by
  0.1.4 or earlier carry no record of their content and are not recognised.
  While the `mcp` service holds the cauldron, the import is sent to it (see
  above).
- `cauldron soup recover --env E --cauldron C`: settles a soup an interrupted
  import left unsettled, by the soup's own record, and says which way it went
  in `interrupted_import`. `landed` means the interrupted import was finished
  and all of it is now in the soup (the output gives the count and ids): do
  NOT import that batch again: under `--remint` it would land every record a
  second time, and nodes cannot be deleted. `rolled-back` means none of it landed:
  importing it again is safe. `none` means there was nothing to settle; check
  `cauldron soup nodes` before importing a batch whose fate you do not know.
  Refused while the `mcp` service holds the cauldron: it recovers the soup
  when it opens it.

## A worked session

In Claude Code:

```text
> /cauldron:curate kb --env dev The import failed because a record guessed a
  region id; we decided to always read region ids from `soup regions` first.
```

This session takes the CLI route (no MCP tools in the session, or the `mcp`
service down). Through the MCP tools the curator does the same with
`soup_status`, `soup_regions`, `soup_search` and `soup_put` (`dry_run: true`
first), passing the records in the call instead of a file; the answers have
the same shape.

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
