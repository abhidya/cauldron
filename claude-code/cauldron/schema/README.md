# The soup schema: `workspace` grammar v6

A cauldron's **soup** is its knowledge graph: typed nodes (directives,
decisions, issues, tasks and so on) joined by a closed set of typed edges, and
grouped into overlapping **regions** that say what belongs together. Every
write to the soup is judged against one pinned grammar, and this directory
publishes that grammar, kind `workspace`, version `6`, in machine-readable
form, so an agent with no access to the cauldron source can write records the
soup will accept.

Version: `workspace@6`.

## Files

| file | what it is |
|---|---|
| `grammar.json` | the grammar itself, exactly as the soup pins it (the serialized `PoolGrammar` for the pool `ws`) |
| `node.<type>.schema.json` | one JSON Schema (draft 2020-12) per node type, twelve in all: id pattern, state vocabulary, fields with their character limits, the edges that may leave the type and the ids they may target, and the edges required at mint |
| `edges.json` | the twelve edge kinds: legal sources and targets, declared edge fields, and every type's required-at-mint rule |
| `README.md` | this file: what each type and edge means, and the rules a JSON Schema cannot express |

Every `.json` file here is **generated** from the Rust grammar
(`libs/spine/src/grammar_v6.rs`, rendered by `libs/spine/src/schema_publish.rs`)
and never edited by hand. A test in the spine crate fails when they drift. To
regenerate after a grammar change:

```sh
SPINE_SCHEMA_BLESS=1 cargo test -p spine --lib schema_publish
```

## The record

The schemas describe one node **at mint**, as a JSON (or YAML) object:

```json
{
  "id": "IS-4",
  "type": "issue",
  "state": "open",
  "handle": "The importer drops edge text when two edges share a target",
  "body": "Markdown prose. Links to other nodes are written [[DEC-2]].",
  "meta": {"kind": "case", "cites": [{"kind": "commit", "repo": "cauldron", "sha": "0123abc"}]},
  "edges": [
    {"kind": "in_region", "to": "R-1", "note": "Inside: anything about importing records"},
    {"kind": "ref", "to": "DEC-2"}
  ]
}
```

- `id` is `<PREFIX>-<n>`, `n` a positive integer with no leading zero. The
  prefix names the type (table below). Ids are immutable and never reused. How
  the final number is assigned depends on the write path: the soup's own door
  mints the serial; an import carries the id you wrote and refuses a collision,
  or, with `--remint`, treats it as a label and assigns the next free serial
  (see "Writing records into a cauldron").
- `type` is one of the twelve types. `state` is one word from the type's closed
  vocabulary; `region` and `viewer` carry no `state` at all.
- The other top-level keys are the type's declared fields. Nothing else is
  allowed (an undeclared field is refused).
- `edges` lists the edges that **leave** this node. Each is
  `{kind, to, note?}` plus the kind's declared fields (only `shows` declares
  any). `note` is optional free text on every edge that carries no declared
  fields; a `shows` edge carries its `cite` and `why` and no `note` (the soup
  stores one or the other on an edge). Inverses are never written:
  `blocked_by`, `superseded_by` and the like do not exist; the soup computes the
  inbound side.
- `to` is an id in the same pool, or `<ID>@<pool>` for a node in another pool,
  which only `derived_from`, `ref` and `resolved_by` may reach.
- Machine-owned values are never written: `rev`, the schema stamp, creation
  time and author, the seal flag.

All limits count **characters** (Unicode code points), never bytes, and every
cap is hard by one character: a 236-character handle is refused.

### Fields

| field | on | rule |
|---|---|---|
| `handle` | content types, `region` | **required**, 1 to 235 characters. The one-line claim, present tense, readable on its own. No `[[links]]` in a handle. |
| `body` | content types | optional, at most 10,000 characters. Markdown prose, stored verbatim. Over the cap, split the node by judgment into several nodes; never trim. |
| `meta` | content types, `region` | optional open object. Unknown keys are carried verbatim. See the reserved keys and the conventions below. |
| `operator` | content types, `region` | an object: the citation of the human message that caused the node. **Required on a `directive`.** Its presence means "the human said this"; its absence means an agent wrote it. The upstream shape is `{"thread": "<full thread uuid>", "msg": <message number>}`. Never invent one. |
| `searched` | content types | at least 1 character. Only for a regionless node (see mint rules): the search terms used to look for a region. |
| `boundary` | `region` | **required**, 40 to 1,000 characters. The region's inclusion rule. |
| `retired`, `provisional` | `region` | booleans (flags) |
| `deferred_by`, `thresholds` | `region` | objects (flags) |
| `wake_when` | `region` | at most 500 characters |
| `role` | `viewer` | **required**, at least 1 character |

## Node types

### The ten content types

| type | prefix | states | what it is for |
|---|---|---|---|
| `directive` | `DIR` | `active` `done` `dropped` | **Human-set direction**, the one type whose content is the human's and not an agent's. Requires `operator`. Binds every region beneath its own while active. |
| `constraint` | `CON` | `standing` `retired` | A **rule of the work** that binds reasoning wherever it is inherited. Attach it to the coarsest region it governs. |
| `danger` | `D` | `lurking` `being-slain` `slain` | A named **hazard of the terrain**: something that can go wrong if you walk into it. Not a defect record (a defect is an `issue`). Binds regions beneath while not yet slain. |
| `issue` | `IS` | `open` `resolved` | **The work ledger**: a defect or an open item that something is wrong. Closed by a `resolved_by` edge to the node that settled it. May `blocks` planning work. |
| `decision` | `DEC` | `decided` `revisited` | **A made call** about the work, ideally cited. A finding is a decision too. A chronicle of events is not. |
| `question` | `Q` | `open` `closed` | **An open fork** that needs an answer. `meta.blocking: true` halts work until answered. Closed by `resolved_by`. An unanswered question stays open. |
| `task` | `T` | `ready` `active` `done` | **One unit of dispatched work.** `depends_on` records what must finish first; a task is not `ready` while a dependency is unfinished. |
| `milestone` | `M` | `pending` `active` `done` `dropped` | An agent-set **increment toward a directive**. Its handle is the done-condition, stated as a fact that will be true. Requires `serves` to a directive. |
| `idea` | `C` | `idea` `exploring` `stub` `active` `done` `dropped` | **The creative record.** Ideas accumulate and never block (an idea that needs a decision spawns a question). `stub` marks deliberately deferred detail. Never dropped silently. |
| `handoff` | `HO` | `current` | The **start-here node** for an agent resuming long-running work: the standing position and its traps, as pointers into regions. Exactly one is current; a new one `supersedes` the last. |

Three states that look missing are computed, never stored: a task that is
waiting on a dependency (read from its `depends_on` edges), a superseded node of
any type (read from an inbound `supersedes`), and a blocked milestone (read from
inbound `blocks`). Do not write them.

**Directive, constraint and danger inherit.** An active directive, a standing
constraint and an un-slain danger bind every region that narrows (directly or
transitively) the region they sit in. Work types (issue, task, milestone,
question, idea, decision) do not inherit.

### `region` (prefix `R`)

A region is a **topic of the pool and the unit of navigation**, not a
container. It holds no member list: membership lives on each member as an
`in_region` edge, so a node may sit in several regions. Regions form a DAG by
`narrows` (child to parent, acyclic, several parents allowed). A region with no
parent is normal; there is no root.

A region's prose is its `handle` (what the region is) and its `boundary`, and
nothing else; it has no `body` and no `state`. The **boundary** is the
inclusion rule in two clauses, always:

- `Inside:` a criterion that decides, for a node you have never seen, whether
  it belongs here.
- `Outside:` the nearest neighbouring regions and where their material goes
  instead (name them by id).

A boundary that can only be written by listing today's members means there is
no region there. Instead of a state a region carries flags: `retired: true`
(no new members; kept so old references still resolve), `deferred_by` (the
human's citation, parking the region), `wake_when` (when a deferred region
should be reviewed), `provisional: true` (coined by a worker that could not
place a node, awaiting a real home), and `thresholds` (per-region size
overrides). Region ids migrated from an older format may read `SUB-<n>`; new
regions are always `R-<n>`, and an edge may target either.

### `viewer` (prefix `V`)

One per role (for example `coordinator`). Its single field is `role`; its
`shows` edges are the content nodes that role always sees first, each with a
required `cite` (an object: the human citation that admitted it) and a required
`why` of 1 to 235 characters. Keep the list short; a listed node should point
("if this applies, go to region R-7") rather than carry content.

## Edges

| kind | from | to | meaning |
|---|---|---|---|
| `in_region` | any content type | `region` | This node belongs to that region. Every content node needs at least one (see mint rules). |
| `rejected_by` | any content type | `region` | A candidate region whose boundary turns this node away; the evidence for a regionless node. |
| `depends_on` | `task`, `milestone` | `task`, `milestone`, `question` | Planned order: the target must finish first. |
| `blocks` | `issue`, `question`, `danger` | `task`, `milestone`, `directive`, `question` | This problem or open fork stops the target. Written on the blocker, never as `blocked_by`. |
| `supersedes` | any type but `viewer` | a node of the **same type** | This node replaces the target. The target keeps its text and reads as superseded. Put the replaced clauses in `note`. |
| `derived_from` | any type but `viewer` | any type but `viewer`, in any `workspace` pool | This node was made from the target (a split, a migration rewrite, a lesson drawn from a case). |
| `ref` | any type but `viewer` | any type but `viewer`, in any `workspace` pool | A reference: the body mentions the target as `[[ID]]`. |
| `resolved_by` | `issue`, `question` | `decision`, `task`; or a `migration` or `release` in a `blueprint` pool | What closed this issue or question. Written by the act that closes it. |
| `serves` | `milestone` | `directive` | The directive this milestone is an increment toward. |
| `narrows` | `region` | `region` | Child region to parent region. |
| `relates` | `region` | `region` | Two regions are adjacent, without hierarchy. |
| `shows` | `viewer` | any content type | The viewer always renders the target. Fields `cite` (object, required) and `why` (1 to 235 characters, required). |

Only `shows` declares edge fields. Any other key on an edge except `kind`, `to`
and `note` is refused. The kind set is closed: an unknown kind is refused.

## Mint rules (required at mint)

These are in the grammar and in each node schema's `allOf`.

1. **Every content node is placed.** It mints with at least one `in_region`
   edge, **or**, when no region's boundary admits it, with at least two
   `rejected_by` edges (two candidate regions that turn it away) **plus** the
   `searched` field. The regionless form is a short-lived exception: such a
   node is expected to be placed in a region, by a re-cut boundary or a new
   region, before the writer's round of work ends. Never place a node in a
   region "near enough".
2. **A milestone serves a directive.** It mints with at least one `serves`
   edge, in addition to rule 1.
3. **A directive carries `operator`.**
4. A `region` and a `viewer` have no required edges.

## Rules the grammar leaves out

The soup's grammar judges one node at a time. These rules read a second node,
or the text, and are kept by the tools that write the soup. A writer must keep
them too.

- **Quote the boundary.** An `in_region` edge should carry, in its `note`, a
  verbatim span of at least 20 characters of the target region's current
  boundary: the clause that admits the node. A `rejected_by` edge carries the
  verbatim span (20 to 1,000 characters) that turns the node away.
- **Live regions only.** `in_region` must target a region that is not
  `retired`. Every region named in an `Outside:` clause must exist and be live.
- **A boundary edit keeps its quotes.** Editing a boundary so that it no longer
  contains a span a member quotes requires re-quoting or re-homing that member
  in the same act.
- **Boundaries have both clauses**, `Inside:` and `Outside:`.
- **`narrows` is acyclic.**
- **No dual home along one chain.** A node may not sit in both a region and one
  of that region's ancestors; put it in the most specific region that admits it.
  Membership in unrelated regions is fine.
- **Ready means unblocked.** A task may be `ready` only when every `depends_on`
  target is finished (`done`, `dropped`, `closed`, or `resolved`).
- **Close by edge, while open.** `resolved_by` is written on an `open` issue or
  question by the act that closes it (and sets `resolved` or `closed`).
- **Supersede with a note.** A `supersedes` edge names, in `note`, which
  clauses of the old node the new one replaces. A superseding node is the way
  to correct any node whose body is final.
- **One current handoff.** Exactly one `handoff` is current; a new one carries
  `supersedes` to the previous one.
- **One viewer per role.**
- **Links agree with edges.** Every `[[ID]]` link in a body has a matching
  `ref` edge, and no handle contains a `[[link]]`.
- **A body never describes the present.** If a sentence could become false
  without anyone editing the node, it belongs in an issue, a task state, or a
  link whose live state is shown when read. "What happened on this date" is a
  report elsewhere, linked, not a node.
- **Reserved `meta` keys** have fixed meanings when present:
  `directive`/`milestone`: `stop` (boolean, a human checkpoint).
  `decision`: `revisit_trigger`. `idea`: `addressed` (boolean),
  `revisit_trigger`. `question`: `blocking` (boolean), `recommendation`,
  `human_action`. `issue`: `answer`, `status`, `severity`, `area`, `tb_id`,
  `next_action`, `human_action`. `task`: `file`, `report`, `model`,
  `dispatched_as`. `handoff`: `covers`, `repair_report`.
- **Retired `meta` keys** must not be written: `subjects`, `regions` (use
  `in_region`), `resolution` (use `resolved_by`), `depends_on` (use the edge),
  `verbatim_evidence`, `operator_cite`, `traces_raw`, `expand_on_init`,
  `token_target`.
- **Rendered sizes are the tools' business.** Region size thresholds, the
  per-viewer render budget and similar caps are measured when the soup is
  rendered, not when a record is written.

## Conventions (decided for now)

These ride in the open `meta` object until the grammar declares fields for
them. A later grammar version may promote them; records written this way will
be migrated, not refused.

**External citations go in `meta.cites`:** a list of objects, each with a
`kind` and the keys that locate it.

| `kind` | keys |
|---|---|
| `session` | `id` (the agent session), optional `at` |
| `file` | `path`, optional `line`, optional `repo`, optional `commit` |
| `commit` | `repo`, `sha` |
| `url` | `url` |
| `thread` | `thread`, `msg` |

```json
"meta": {"cites": [{"kind": "url", "url": "https://example.com/spec"},
                   {"kind": "file", "path": "src/lib.rs", "line": 40}]}
```

`meta.cites` is evidence. It is not `operator`: only the human's own message
makes a node the human's.

**Knowledge capture: patterns and cases.** There is no `pattern` or `case`
type. Record a **pattern** (a lesson, a rule learned) as a `decision` (a call
or finding) or a `constraint` (a rule that should bind future work), with
`meta.kind: "pattern"`. Record a **case** (a concrete instance, the evidence)
as an `idea` or an `issue` (when something went wrong), with
`meta.kind: "case"`. A pattern carries `derived_from` to each case it was drawn
from; a later case that illustrates an existing pattern carries `ref` to it
(and links it as `[[ID]]` in its body).

## A worked example

A small, complete pool. Every record validates against its schema, and the
pool keeps the rules above.

```json
[
  {
    "id": "R-1",
    "type": "region",
    "handle": "Publishing the soup schema",
    "boundary": "Inside: anything about the published, machine-readable schema of the soup and how outside agents use it. Outside: the importer's code goes to R-2."
  },
  {
    "id": "R-2",
    "type": "region",
    "handle": "Importing records into a soup",
    "boundary": "Inside: anything about landing externally written records in a soup: ids, collisions, validation. Outside: the schema itself goes to R-1.",
    "edges": [{"kind": "relates", "to": "R-1"}]
  },
  {
    "id": "DIR-1",
    "type": "directive",
    "state": "active",
    "handle": "Agents on other machines can write valid soup records without the cauldron source",
    "operator": {"thread": "00000000-0000-4000-8000-000000000000", "msg": 1},
    "edges": [{"kind": "in_region", "to": "R-1", "note": "anything about the published, machine-readable schema"}]
  },
  {
    "id": "M-1",
    "type": "milestone",
    "state": "active",
    "handle": "The v6 schema is published with a drift test and a README",
    "edges": [
      {"kind": "serves", "to": "DIR-1"},
      {"kind": "in_region", "to": "R-1", "note": "anything about the published, machine-readable schema"}
    ]
  },
  {
    "id": "Q-1",
    "type": "question",
    "state": "open",
    "handle": "Does a repeat import re-mint carried ids or refuse the collision?",
    "meta": {"blocking": true},
    "edges": [
      {"kind": "in_region", "to": "R-2", "note": "landing externally written records in a soup"},
      {"kind": "blocks", "to": "T-1"}
    ]
  },
  {
    "id": "T-1",
    "type": "task",
    "state": "active",
    "handle": "Add an import verb that lands a validated record set in one tranche",
    "edges": [{"kind": "in_region", "to": "R-2", "note": "landing externally written records in a soup"}]
  },
  {
    "id": "IS-1",
    "type": "issue",
    "state": "open",
    "handle": "A record with an unknown edge kind was refused only at import, after an hour of writing",
    "body": "Validating against node.task.schema.json before writing would have caught it. See [[DEC-1]].",
    "meta": {"kind": "case", "cites": [{"kind": "file", "path": "notes/import-run.log", "line": 12}]},
    "edges": [
      {"kind": "in_region", "to": "R-2", "note": "landing externally written records in a soup"},
      {"kind": "ref", "to": "DEC-1"}
    ]
  },
  {
    "id": "DEC-1",
    "type": "decision",
    "state": "decided",
    "handle": "Validate every record against the published schema before writing it",
    "meta": {"kind": "pattern"},
    "edges": [
      {"kind": "in_region", "to": "R-1", "note": "how outside agents use it"},
      {"kind": "derived_from", "to": "IS-1"}
    ]
  },
  {
    "id": "CON-1",
    "type": "constraint",
    "state": "standing",
    "handle": "Schema files are generated from the grammar and never edited by hand",
    "edges": [{"kind": "in_region", "to": "R-1", "note": "anything about the published, machine-readable schema"}]
  },
  {
    "id": "C-1",
    "type": "idea",
    "state": "idea",
    "handle": "A colour theme for the terminal face",
    "searched": "theme colour face",
    "edges": [
      {"kind": "rejected_by", "to": "R-1", "note": "Inside: anything about the published, machine-readable schema"},
      {"kind": "rejected_by", "to": "R-2", "note": "Inside: anything about landing externally written records in a soup"}
    ]
  },
  {
    "id": "V-1",
    "type": "viewer",
    "role": "coordinator",
    "edges": [
      {"kind": "shows", "to": "DIR-1", "cite": {"thread": "00000000-0000-4000-8000-000000000000", "msg": 2}, "why": "The goal every round serves"}
    ]
  }
]
```

`C-1` is the regionless form: no boundary admits it, so it carries two
`rejected_by` edges and `searched`, and it is owed a region (a new one, most
likely) before the round of work that wrote it ends.

## Writing records into a cauldron

`cauldron soup import <file-or-dir> --cauldron <name> --env <env>` lands records
in this shape into a cauldron's soup when no soup daemon holds it. Input is
one file holding a list of records (or one record), or a directory of
one-record files; JSON, or YAML for `.yaml` / `.yml`. Every record is judged by
the soup's own write door, and the printed JSON gives each record's
disposition: `accepted`, or `refused` with the reason (the grammar's own
witnesses when the grammar refused it). The batch lands as **one write, all or
nothing**: one refused record and nothing lands. `--dry-run` judges and lands
nothing; it also works while a soup daemon holds the soup, judged on a
read-only snapshot of it.

- An edge's `to` names a record of the same batch when one carries that id;
  otherwise it must name a node the soup already holds, or the record is
  refused (`unknown-target`). `<ID>@<pool>` always names a node already held.
- Without `--remint`, ids are carried as written; an id the soup already holds
  refuses the whole import. With `--remint`, every id in the batch is a label:
  each record gets the next free id for its type, references and `[[links]]`
  between batch records are rewritten, and the output's `id_map` gives old to
  new. Under `--remint` a label may be any string without `@` or `#` that is
  not an id the soup already holds: such a label is refused
  (`label-shadows-existing`), so a bare id that is not a label always names
  the existing node. Labels like `n1` never collide.
- Reads, all read-only and JSON, all taking `--max-chars N` (longer text is
  cut and listed under `truncated`): `cauldron soup regions` (every region:
  id, handle, boundary, flags, what it narrows and relates to, member count);
  `cauldron soup region <id> [--depth N]` (one region, its parents, children,
  related regions and members); `cauldron soup nodes [--region R] [--type T]
  [--state S] [--limit N]` (id, type, state, handle and regions, newest
  first); `cauldron soup search <words> [--region R] [--type T] [--limit N]`
  (each word of the query found case-insensitively, as a substring, in
  handles and bodies; ranked, with a snippet); and `cauldron soup get <id>...` (nodes in this shape, with their
  outgoing edges).

## Validating a record

Pick the schema by the record's `type`:

```python
import json, jsonschema  # pip install jsonschema
schema = json.load(open(f"node.{record['type']}.schema.json"))
jsonschema.Draft202012Validator(schema).validate(record)
```

The schemas check one record. The rules under "Rules the grammar leaves out"
need the whole pool and are not checked by them.
