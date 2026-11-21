# The soup schema: `workspace` grammar v6

A cauldron's **soup** is its knowledge graph: typed nodes (directives,
decisions, issues, tasks and so on) joined by a closed set of typed edges, and
grouped into overlapping **regions** that say what belongs together. Every
write to the soup is judged against one pinned grammar, and this directory
publishes that grammar, kind `workspace`, version `6`, in machine-readable
form, so an agent with no access to the cauldron source can write records the
soup will accept.

Version: `workspace@6`.

**Since 0.1.10, see also.** The grammar and the schema files here have not
changed since 0.1.10, on purpose; the soup's door has grown rules around
them, described in the repository's root `README.md` and `CHANGELOG.md`:

- the owner's rejection, `meta.rejected` (`cauldron ripples reject`), and
  the reads that leave rejected records out (`rejected_hidden`, and
  `include_rejected` to list them);
- an edge's `note` is at most 1,000 characters at the door (0.1.16), though
  the schemas put no bound on it (below, under the record shape);
- region locks: a write into a region a ripple holds is refused
  `region_locked` (0.1.14);
- clusters, which claim regions, on their own grammar (kind `cauldron`,
  version 1), published in the sibling directory with its own README
  (0.1.16);
- writing back a record you read: `../write-back.json` lists, per key, what
  a write drops (keys a read adds) and what it refuses (0.1.17);
- `meta.ripple` is refused on every write but a ripple's own
  (`ripple_provenance`, 0.1.17);
- the review flags `meta.approved` and `meta.rejected` are declared in the
  schemas because stored records carry them, and refused on every record
  write (`usage`): only the owner's approval and rejection set them. A
  validator built from the schemas alone passes them; leave them out;
- an edge whose `note` is `null` is taken as an edge with no note, though
  the schemas want a string where the key is present: leave the key out
  rather than send `null` (0.1.17 documents this; nothing changed);
- a `supersedes` edge is written only with the new node that supersedes,
  and a `supersedes` or `narrows` edge from a node to itself, or one closing
  a cycle, is refused on every write (`edge-loop`, 0.1.17).

## Files

| file | what it is |
|---|---|
| `grammar.json` | the grammar itself, exactly as the soup pins it (the serialized `PoolGrammar` for the pool `ws`) |
| `node.<type>.schema.json` | one JSON Schema (draft 2020-12) per node type, twelve in all: id pattern, state vocabulary, fields with their character limits, the edges that may leave the type and the ids they may target, and the edges required at mint |
| `remint.<type>.schema.json` | the same schema for a record of a `--remint` batch: `id` is a batch label (any non-blank string without `@` or `#`), and an edge's `to` may name a label of the batch; nothing else differs |
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
  stores one or the other on an edge). The schemas put no bound on `note`,
  but the soup's door takes a NEW note of at most 1,000 characters, on every
  write path (a record's edge, `soup_link`, `soup_claim`): a longer one
  refuses the write. A validator built from these schemas alone will pass a
  longer note; check the bound yourself. Inverses are never written:
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
`in_region` edge, so a node may sit in several regions. Regions are an
overlapping index: a node about two topics belongs in both, and several homes
are normal. The one limit is along a chain (see "Not in a region and its
ancestor" below). Regions form a DAG by
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
- **Not in a region and its ancestor.** A node may not sit in both a region
  and one of that region's ancestors (a region it `narrows`, directly or
  through others); put it in the most specific region that admits it. This is
  the only limit on a node's regions: membership in several regions that are
  not on one chain is normal.
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
  `dispatched_as`. `handoff`: `covers`, `repair_report`. Every content
  type: `approved` (the review flag, below) and `ripple` (set by the MCP
  door on a record a ripple wrote, below).
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
| `dump` | `id`: a dump (`DM-<n>`) in the same cauldron's dumps store, the raw text the record came from |

```json
"meta": {"cites": [{"kind": "url", "url": "https://example.com/spec"},
                   {"kind": "file", "path": "src/lib.rs", "line": 40}]}
```

The import checks every citation: a `kind` outside this table, a citation
that is not an object, or a required key missing, null or empty refuses the
record as `bad-cite` (dry run and real, `cauldron soup import` and `soup_put`
alike), naming the record and the citation. Other keys are allowed. The
per-type schemas say the same under `meta.cites`.

`meta.cites` is evidence. It is not `operator`: only the human's own message
makes a node the human's.

**Records a ripple wrote, and the review flag.** A ripple (`cauldron
ripples run`, from 0.1.10) writes new records only, each citing its dump
(`{"kind": "dump", "id": "DM-<n>"}`) and carrying `meta.ripple: {package,
version, run, model}`, which the MCP door sets and a batch cannot change.
From 0.1.17 no other write sets it: a record that carries `meta.ripple`, in
any casing or nesting, is refused at the door as `ripple_provenance` on
every write path but a ripple's (`soup_put`, `cauldron soup import`,
`--remint` or not, dry run or not). A record written before 0.1.17 may
carry one a session made up, so trust the field only on a record a ripple
wrote: who wrote a record is read from the transaction that wrote it
(`soup_review`), not from any field. Such a record is unreviewed until its
`meta.approved` is set (any value but `false` or `null`; the owner's
decision writes `{at, by, session?}`: `cauldron soup approve` or `cauldron
ripples approve` at a terminal, `by: "tty"`, or, only where the owner
opened approving to agents, those commands without one or the MCP tool
`soup_approve`, `by: "agent"`); the reads mark it `unreviewed: true`,
which is computed and is not a field (leave it out when you write the record
again). `meta.approved` is a flag: the soup refuses it to every ripple, so a
ripple can never approve its own records. Do not write `meta.approved` or
`meta.ripple` yourself on a record you create.
The same holds for `meta.rejected`, which a rejection writes (`{at, by,
session?, reason?}`, `by` as above). The descriptions of `meta.approved` and
`meta.rejected` in the generated schema files say the same. Their words may
change from one release to the next; what they accept may not: a test in the
spine crate holds every validation keyword of the published files (everything
but `description`, `title` and `$comment`) to the files published with
0.1.16.

**Knowledge capture: patterns and cases.** There is no `pattern` or `case`
type. Record a **pattern** (a lesson, a rule learned) as a `decision` (a call
or finding) or a `constraint` (a rule that should bind future work), with
`meta.kind: "pattern"`. Record a **case** (a concrete instance, the evidence)
as an `idea` or an `issue` (when something went wrong), with
`meta.kind: "case"`. A pattern carries `derived_from` to each case it was drawn
from; a later case that illustrates an existing pattern carries `ref` to it
(and links it as `[[ID]]` in its body). A plain decision (not a pattern) that
was drawn from a case carries `derived_from` to it too; one that only mentions
a case carries `ref` (and the `[[ID]]` link). A case recorded as an `idea` is
a finished record of something that happened: its state is `done`. (A case
recorded as an `issue` is `open` until something resolves it.)

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
in this shape into a cauldron's soup. Nothing has to be stopped first: the
face (the terminal UI) being open does not matter, and no release so far ships
a soup daemon, so the import writes the soup itself. The one other writer is
the environment's `mcp` service: while it holds a granted cauldron, the import
is sent to it and lands through it (its output says `forwarded_to`), with the
same judgement. Records can also be written from a Claude Code session with
the MCP tool `soup_put`, which takes the same records. Input is
one file holding a list of records (or one record), or a directory of
one-record files; JSON, or YAML for `.yaml` / `.yml`. Every record is judged by
the soup's own write door, and the printed JSON gives each record's
disposition: `accepted`, or `refused` with the reason (the grammar's own
witnesses when the grammar refused it). The batch lands as **one write, all or
nothing**: one refused record and nothing lands. A record whose edge points
at a refused record of the same batch is refused too, as `target-refused`, so
the refused count can be larger than the number of problems: fix the records
refused for any other reason first, and the `target-refused` ones usually
follow. `--dry-run` judges and lands nothing, on a read-only snapshot of the
soup. A dry run also reports `batch-already-landed` when the same batch
already landed, exactly as the real import would. A refused batch always prints the same keys:
`outcome: "refused"`, a one-word `refusal` (`records-refused`,
`landing-refused`, or `batch-already-landed` for a batch identical to one that
already landed), `accepted`, `refused` and `records` (empty when the batch was
refused before any record was judged).

**What the import checks, and what it does not.** It judges each record by
the grammar (types, states, fields and their limits, edge kinds and their
ends, the mint rules) and checks `meta.cites`. It does not check the rules
under "Rules the grammar leaves out" (a boundary quoted in `in_region`'s
note, `Outside:` clauses, `[[links]]` matching `ref` edges, retired `meta`
keys, a node in a region and its ancestor, and the rest): a batch that breaks
them can validate and land. Keeping them is the writer's job.

**Changing a node.** Imports and `soup_put` only create nodes: a node's
fields are never rewritten. A few writes add to, or take from, a node that
exists: `soup_link` and `cauldron soup link` add an edge; `soup_unlink` and
`cauldron soup unlink` remove one; `soup_cite` and `cauldron soup cite` add
or remove one entry of `meta.cites`; and the owner's approve and reject set
a review flag. To change what a node SAYS, write a new node of the **same
type** with a new id (a label, under `--remint`) that carries a `supersedes`
edge to the old one, with a `note` naming the clauses it replaces, and
carries forward the old node's `in_region` and other edges that still hold:

```json
{"id": "n1", "type": "decision", "state": "decided",
 "handle": "Read region ids from `soup regions` before writing, and cache them per batch",
 "edges": [{"kind": "supersedes", "to": "DEC-2", "note": "adds: cache region ids per batch"},
           {"kind": "in_region", "to": "R-1", "note": "how outside agents use it"}]}
```

`supersedes` across types is refused (`wrong_endpoint_type`). The old node
keeps its text and reads as superseded (see the reads below).

**Adding or removing an edge of a node that exists.** An import writes a
node's outgoing edges when it mints the node, so it cannot add one to a node
the soup already holds; an edge INTO an existing node needs nothing special,
since any new record can carry it. `soup_link` (or `cauldron soup link
<from> <kind> <to>`) adds an edge between two nodes the soup holds, and
`soup_unlink` (or `cauldron soup unlink`) removes one, so a wrong edge does
not stay readable on the old node; the change feed keeps both as `link` and
`unlink`. A removal is refused for a node's last `in_region` (add the new one
first), a milestone's last `serves`, and a `supersedes` edge, which is
history (except one from a node to itself, or one on a cycle of `supersedes`
edges: neither records a replacement, so either may be removed). `soup_link`
never adds a `supersedes` edge (`supersedes-at-creation`): it is written
only with the new node that supersedes. A `supersedes` or `narrows` edge
from a node to itself, or one that would close a cycle of its kind, is
refused on every write (`edge-loop`). A wrong or missing citation is fixed the same way, one entry at a
time, with `soup_cite` (or `cauldron soup cite <id> --add | --remove`).

**Edge order.** `get` answers a node's edges sorted by kind, then by target,
not in the order they were written; compare records as sets of edges.

**Validating a `--remint` batch.** The `node.<type>.schema.json` schemas
check `id` and every edge's `to` against the `<PREFIX>-<n>` id pattern, so a
batch label such as `n1` fails them, though the import accepts it. Validate a
`--remint` batch's records against `remint.<type>.schema.json` instead: the
same schema, with `id` a batch label and an edge's `to` a label or an id.
What no schema can see is still the import's to judge: a label equal to an id
the soup already holds (`label-shadows-existing`), and a `to` that names
neither a record of the batch nor a node the soup holds (`unknown-target`).

- An edge's `to` names a record of the same batch when one carries that id;
  otherwise it must name a node the soup already holds, or the record is
  refused (`unknown-target`). `<ID>@<pool>` always names a node already held.
- Without `--remint`, ids are carried as written; an id the soup already holds
  refuses the whole import. With `--remint`, every id in the batch is a label:
  each record gets the next free id for its type, references and `[[links]]`
  between batch records are rewritten (an edge's `to`, and a `[[label]]`
  link in a record's `body`; nothing else: a label in a `handle`, a
  region's `boundary` or an edge's `note`, linked or in plain words, is kept
  as written, so name batch records from those fields only once they have
  ids), and the output's `id_map` gives old to
  new when the batch landed (or, on a dry run, would land as it stands); a
  refused batch prints no `id_map`, since its ids were never taken. Under `--remint` a label may be any string without `@` or `#` that is
  not an id the soup already holds: such a label is refused
  (`label-shadows-existing`), so a bare id that is not a label always names
  the existing node. Labels like `n1` never collide.
- Reads, all read-only and JSON, all taking `--max-chars N` (longer text is
  cut and listed under `truncated`): `cauldron soup regions` (every region:
  id, handle, boundary, flags, what it narrows and relates to, member count);
  `cauldron soup region <id> [--depth N]` (one region, its parents, children,
  related regions and members); `cauldron soup nodes [--region R] [--type T]
  [--state S] [--limit N]` (id, type, state, handle and regions, newest
  first; a region or viewer is listed without `state`, as it carries none); `cauldron soup search <words> [--region R] [--type T] [--limit N]`
  (each word of the query found case-insensitively, as a substring, in
  handles and bodies; ranked, with a snippet); and `cauldron soup get <id>...` (nodes in this shape, with their
  outgoing edges).
- A superseded node (one another node's `supersedes` edge points at) is left
  out of `nodes`, `search` and a region's members, each saying how many it
  left out (`superseded_hidden`); `--include-superseded` lists it too. `get`
  always answers it, adding `superseded_by: [ids]`. That key is read from the
  edges, not stored: a write drops it (and names it in its answer's
  `ignored`), as it drops every key a read adds. `../write-back.json` lists
  them, and the keys a write refuses that a read can show (the review flags,
  `meta.ripple`, and the id itself), so a record read back needs no
  hand-made strip list.

## Validating a record

Pick the schema by the record's `type`:

```python
import json, jsonschema  # pip install jsonschema
schema = json.load(open(f"node.{record['type']}.schema.json"))
# a record of a --remint batch: f"remint.{record['type']}.schema.json"
jsonschema.Draft202012Validator(schema).validate(record)
```

The schemas check one record. The rules under "Rules the grammar leaves out"
need the whole pool and are not checked by them. A `--remint` batch's
records are checked against `remint.<type>.schema.json`: see "Validating a
`--remint` batch" above.
